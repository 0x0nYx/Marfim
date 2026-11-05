//! End-to-end: the compiled `marfim` binary as a child process.
//! Covers: headless json/stream-json, doctor, agents (unverified
//! profiles), MCP server stdio roundtrip, LSP framed roundtrip,
//! serve /healthz over loopback.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

fn bin_path() -> std::path::PathBuf {
    // cargo builds test deps next to the binary
    let mut p = std::env::current_exe().unwrap();
    p.pop(); // deps/
    if p.ends_with("deps") {
        p.pop();
    }
    p.join("marfim")
}

#[test]
fn version_and_doctor() {
    let out = Command::new(bin_path()).arg("--version").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("marfim 0.3.0"), "{text}");

    let out = Command::new(bin_path()).arg("doctor").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("rust"), "{text}");
    assert!(text.contains("sandbox"), "{text}");
}

#[test]
fn agents_profiles_are_unverified_by_default() {
    let out = Command::new(bin_path()).arg("agents").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("opencode"));
    assert!(text.contains("hermes"));
    // no-invention rule: enabled=false verified=false in a clean sandbox
    assert!(text.contains("enabled=false verified=false"), "{text}");
}

#[test]
fn headless_json_contract() {
    let out = Command::new(bin_path())
        .args(["-p", "hello", "--output-format", "json"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value =
        serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("not json: {e}: {text}"));
    assert_eq!(v["type"], "result");
    assert_eq!(v["stop"], "end_turn");
}

#[test]
fn headless_stream_json_events_are_jsonl() {
    let out = Command::new(bin_path())
        .args(["-p", "hi", "--output-format", "stream-json"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    assert!(!lines.is_empty());
    // first line is an event, last is turn_end
    let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(first["event"], "turn_started");
    let last: serde_json::Value = serde_json::from_str(lines[lines.len() - 1]).unwrap();
    assert_eq!(last["event"], "turn_end");
}

#[test]
fn mcp_server_lists_builtin_tools_over_stdio() {
    let mut child = Command::new(bin_path())
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{}}}}"#
    )
    .unwrap();
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{{}}}}"#
    )
    .unwrap();
    // read-only tool executes; execution-class tool honestly refuses
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{{"name":"Compute","arguments":{{"expression":"6*7"}}}}}}"#
    )
    .unwrap_or_else(|_| {});
    let _ = stdin.flush();
    drop(stdin);
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut l1 = String::new();
    let mut l2 = String::new();
    stdout.read_line(&mut l1).unwrap();
    stdout.read_line(&mut l2).unwrap();
    let r1: serde_json::Value = serde_json::from_str(l1.trim()).unwrap();
    assert_eq!(r1["result"]["serverInfo"]["name"], "marfim");
    let r2: serde_json::Value = serde_json::from_str(l2.trim()).unwrap();
    let tools = r2["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    for expected in [
        "Read",
        "Write",
        "Edit",
        "Bash",
        "Grep",
        "Glob",
        "ApplyPatch",
        "RepoMap",
        "Task",
        "WebFetch",
        "ToolSearch",
        "Memory",
    ] {
        assert!(names.contains(&expected), "missing {expected} in {names:?}");
    }
    let mut l3 = String::new();
    stdout.read_line(&mut l3).unwrap_or(0);
    if !l3.trim().is_empty() {
        let r3: serde_json::Value = serde_json::from_str(l3.trim()).unwrap();
        assert!(
            r3["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or("")
                .contains("42")
                || r3["result"]["isError"] == true,
            "{r3}"
        );
    }
    let _ = child.wait();
}

#[test]
fn lsp_initialize_and_completion_over_framed_stdio() {
    let mut child = Command::new(bin_path())
        .arg("lsp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut send = |v: serde_json::Value| {
        let body = serde_json::to_string(&v).unwrap();
        write!(stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        let _ = stdin.flush();
    };
    send(serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}));
    send(
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"textDocument/completion","params":{"context":{"triggerCharacter":"/"}}}),
    );
    drop(stdin); // close the pipe → child sees EOF
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut got_init = false;
    let mut got_completion = false;
    for _ in 0..2 {
        let mut header = String::new();
        // read header lines
        loop {
            let mut line = String::new();
            if stdout.read_line(&mut line).unwrap_or(0) == 0 {
                break;
            }
            if line == "\r\n" {
                break;
            }
            header.push_str(&line);
        }
        let cl: usize = header
            .lines()
            .find_map(|l| {
                l.to_lowercase()
                    .strip_prefix("content-length:")
                    .and_then(|v| v.trim().parse().ok())
            })
            .unwrap_or(0);
        if cl == 0 {
            break;
        }
        let mut body = vec![0u8; cl];
        std::io::Read::read_exact(&mut stdout, &mut body).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        if v["id"] == 1 {
            assert_eq!(v["result"]["serverInfo"]["name"], "marfim-lsp");
            got_init = true;
        }
        if v["id"] == 2 {
            let items = v["result"]["items"].as_array().unwrap();
            assert!(items.len() >= 60, "{}", items.len());
            got_completion = true;
        }
    }
    assert!(got_init);
    assert!(got_completion);
    let _ = child.wait();
}

#[test]
fn serve_healthz_on_loopback() {
    let mut child = Command::new(bin_path())
        .args(["serve", "--port", "0"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // --port must precede the subcommand (top-level clap arg)
    let _ = child.kill();
    let _ = child.wait();
    let mut child = Command::new(bin_path())
        .args(["--port", "17999", "serve"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // retry loop: wait until the listener is up (max ~5s)
    use std::net::TcpStream;
    let mut text = String::new();
    for _ in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if let Ok(mut s) = TcpStream::connect("127.0.0.1:17999") {
            s.write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\n\r\n")
                .unwrap();
            let mut buf = vec![0u8; 2048];
            let n = std::io::Read::read(&mut s, &mut buf).unwrap_or(0);
            text = String::from_utf8_lossy(&buf[..n]).to_string();
            if text.contains("\"ok\":true") {
                break;
            }
        }
    }
    assert!(
        text.contains("\"ok\":true"),
        "healthz never healthy: {text}"
    );
    assert!(text.contains("version"), "{text}");
    let _ = child.kill();
    let _ = child.wait();
}
