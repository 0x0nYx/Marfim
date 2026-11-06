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

/// Hermetic child environment: journals land in a temp MARFIM_HOME.
fn isolated_child(args: &[&str]) -> std::process::Command {
    let home = std::env::temp_dir().join(format!(
        "marfim-e2e-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::create_dir_all(&home);
    let mut c = Command::new(bin_path());
    c.args(args).env("MARFIM_HOME", &home).current_dir("/tmp");
    c
}

/// Incrementally drain a child pipe into a shared buffer (line-by-line,
/// so the test thread can poll progress before EOF).
fn drain_into<R: std::io::Read + Send + 'static>(
    stream: R,
    buf: std::sync::Arc<std::sync::Mutex<String>>,
) {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => *buf.lock().unwrap() += &line,
            }
        }
    });
}

#[test]
fn version_and_doctor() {
    let out = Command::new(bin_path()).arg("--version").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("marfim 1.0.0"), "{text}");

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
    // D11: the provider is EXPLICIT (mock here); no silent default exists.
    let out = isolated_child(&[
        "-p",
        "hello",
        "--provider",
        "mock",
        "--output-format",
        "json",
    ])
    .output()
    .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value =
        serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("not json: {e}: {text}"));
    assert_eq!(v["type"], "result");
    assert_eq!(v["stop"], "end_turn");
    // D12: success exits 0
    assert_eq!(out.status.code(), Some(0), "stdout: {text}");
}

#[test]
fn headless_json_contract_via_config_file() {
    // D13: the provider can come from the layered config file
    let home = std::env::temp_dir().join(format!(
        "marfim-e2e-cfg-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(home.join(".marfim")).unwrap();
    std::fs::write(
        home.join(".marfim").join("config.toml"),
        "provider = \"mock\"\nmodel = \"mock-1\"\n",
    )
    .unwrap();
    let mut c = Command::new(bin_path());
    c.args(["-p", "hello", "--output-format", "json"])
        .env("MARFIM_HOME", &home)
        .env_remove("MARFIM_PROVIDER")
        .current_dir("/tmp");
    let out = c.output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value =
        serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("not json: {e}: {text}"));
    assert_eq!(v["type"], "result");
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn headless_without_provider_is_an_explicit_error_not_mock() {
    // D11: no silent mock — missing provider config is a hard error
    let home = std::env::temp_dir().join(format!(
        "marfim-e2e-noprov-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::create_dir_all(&home);
    let mut c = Command::new(bin_path());
    c.args(["-p", "hello"])
        .env("MARFIM_HOME", &home)
        .env_remove("MARFIM_PROVIDER")
        .current_dir("/tmp");
    let out = c.output().unwrap();
    assert_eq!(out.status.code(), Some(1), "must exit 1");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("no provider configured"), "{err}");
}

#[test]
fn headless_unknown_provider_errors_d12_exit_1() {
    let out = isolated_child(&["-p", "hi", "--provider", "does-not-exist"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("unknown provider"), "{err}");
}

#[test]
fn headless_stream_json_events_are_jsonl() {
    let out = isolated_child(&[
        "-p",
        "hi",
        "--provider",
        "mock",
        "--output-format",
        "stream-json",
    ])
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
    // D9: stream deltas ride the same bus
    assert!(
        lines.iter().any(|l| l.contains("stream_delta")),
        "stream deltas in stream-json output"
    );
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
    // The gateway is fail-closed: token mandatory, Host validated. This
    // e2e passes an explicit token, asserts the same request WITHOUT the
    // token is refused (401), then proves the authorized healthz works.
    let mut child = Command::new(bin_path())
        .args(["--port", "17999", "--token", "e2e-token", "serve"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    use std::net::TcpStream;
    let mut unauth = String::new();
    for _ in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if let Ok(mut s) = TcpStream::connect("127.0.0.1:17999") {
            s.write_all(
                b"GET /healthz HTTP/1.1\r\nHost: 127.0.0.1:17999\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
            let mut buf = vec![0u8; 2048];
            let n = std::io::Read::read(&mut s, &mut buf).unwrap_or(0);
            unauth = String::from_utf8_lossy(&buf[..n]).to_string();
            if !unauth.is_empty() {
                break;
            }
        }
    }
    assert!(
        unauth.starts_with("HTTP/1.1 401"),
        "unauthenticated must 401: {unauth}"
    );
    let mut text = String::new();
    for _ in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if let Ok(mut s) = TcpStream::connect("127.0.0.1:17999") {
            s.write_all(b"GET /healthz HTTP/1.1\r\nHost: 127.0.0.1:17999\r\nAuthorization: Bearer e2e-token\r\nConnection: close\r\n\r\n")
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

/// Hermetic MARFIM_HOME with a mock-provider config; returns (home, config dir).
fn mock_config_home(tag: &str) -> std::path::PathBuf {
    let home = std::env::temp_dir().join(format!(
        "marfim-e2e-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(home.join(".marfim")).unwrap();
    std::fs::write(
        home.join(".marfim").join("config.toml"),
        "provider = \"mock\"\nmodel = \"mock-1\"\n",
    )
    .unwrap();
    home
}

// D12: exit code 2 — a run where the ONLY tool attempt is denied by the
// permission engine exits 2 (permission-denied), not 0 and not 1.
#[test]
fn d12_headless_exit_code_2_when_every_tool_is_denied() {
    let home = mock_config_home("d12");
    std::fs::write(
        home.join(".marfim").join("config.toml"),
        "provider = \"mock\"\nmodel = \"mock-1\"\n\n[permissions]\ndeny = [\"Read\"]\n",
    )
    .unwrap();
    let mut c = Command::new(bin_path());
    c.args(["-p", "read the hostname file", "--output-format", "json"])
        .env("MARFIM_HOME", &home)
        .env_remove("MARFIM_PROVIDER")
        .env(
            "MARFIM_MOCK_SCRIPT",
            r#"[
                {"tool_call":{"id":"t1","name":"Read","arguments":{"path":"/etc/hostname"}}},
                {"text":"done despite denial"}
            ]"#,
        )
        .current_dir("/tmp");
    let out = c.output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value =
        serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("not json: {e}: {text}"));
    assert_eq!(v["type"], "result");
    // D12 mapping: denied>0 && denied==executed => exit 2
    assert_eq!(out.status.code(), Some(2), "stdout: {text}");
}

// D14: the REPL prints errors, owns stdin exactly once, and survives
// across turns — including a provider error mid-session. Lines are sent
// strictly AFTER the previous turn finishes (sending mid-turn would be
// steering, which is a different feature).
#[test]
fn d14_repl_prints_errors_and_survives_across_turns() {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    let home = mock_config_home("d14");
    let mut child = Command::new(bin_path())
        .env("MARFIM_HOME", &home)
        .env_remove("MARFIM_PROVIDER")
        .env("MARFIM_MOCK_SCRIPT", r#"[{"error":"boom"}]"#)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir("/tmp")
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    let out_buf = Arc::new(Mutex::new(String::new()));
    let err_buf = Arc::new(Mutex::new(String::new()));
    drain_into(stdout, out_buf.clone());
    drain_into(stderr, err_buf.clone());

    // send one line, then wait for the expected reply (bounded)
    let mut send = |line: &str| {
        writeln!(stdin, "{line}").unwrap();
        let _ = stdin.flush();
    };
    let wait_for = |buf: &Arc<Mutex<String>>, needle: &str| {
        let t0 = std::time::Instant::now();
        loop {
            assert!(
                t0.elapsed() < Duration::from_secs(20),
                "timed out waiting for {needle:?}; got: {}",
                buf.lock().unwrap()
            );
            if buf.lock().unwrap().contains(needle) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    };

    // banner proves the REPL is up
    wait_for(&err_buf, "type /help for commands");

    // slash commands work before any provider traffic
    send("/help");
    wait_for(&out_buf, "commands (65)");

    // turn: provider error → printed on stderr EXACTLY ONCE, REPL stays alive
    send("one");
    wait_for(&err_buf, "boom");
    std::thread::sleep(Duration::from_millis(500)); // allow any duplicate print to land
    let boom_count = err_buf
        .lock()
        .unwrap()
        .matches("provider error: boom")
        .count();
    assert_eq!(
        boom_count, 1,
        "provider error printed {boom_count}x, expected 1"
    );

    // the SAME stdin owner still serves commands after the failed turn
    // (the router keeps the failed mock unavailable — fail-closed failover —
    // so the follow-up input is a slash command, not a second model turn)
    send("/frobnicate");
    wait_for(&out_buf, "unknown command /frobnicate");

    send("/quit");
    let t0 = std::time::Instant::now();
    let status = loop {
        if let Some(st) = child.try_wait().unwrap() {
            break st;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "REPL did not exit after /quit — stdin ownership or loop bug"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(status.code(), Some(0));
}

#[test]
#[cfg(unix)]
fn d8_ctrl_c_signal_wires_to_graceful_cancel() {
    use std::time::{Duration, Instant};

    let home = mock_config_home("d8sig");
    let mut child = Command::new(bin_path())
        .args(["-p", "hi", "--output-format", "stream-json"])
        .env("MARFIM_HOME", &home)
        .env_remove("MARFIM_PROVIDER")
        .env("MARFIM_MOCK_SCRIPT", r#"[{"sleep_ms": 8000}]"#)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir("/tmp")
        .spawn()
        .unwrap();

    // give the runtime time to arm the tokio ctrl-c handler before signalling
    std::thread::sleep(Duration::from_millis(1200));
    let t0 = Instant::now();
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(child.id() as i32),
        nix::sys::signal::Signal::SIGINT,
    )
    .unwrap();

    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(6),
            "process ignored SIGINT (wiring broken) — waited {}ms",
            t0.elapsed().as_millis()
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    let waited = t0.elapsed();

    // graceful wiring: the turn unwinds through the cancel token and emits
    // the turn_end event with the aborted reason — a default-action SIGKILL-
    // style death could never produce this output.
    let mut out = String::new();
    use std::io::Read;
    let mut stdout = child.stdout.take().unwrap();
    let _ = stdout.read_to_string(&mut out);
    assert!(
        out.contains("turn_end") && out.contains("aborted"),
        "expected graceful turn_end/aborted event after SIGINT, got: {out}"
    );
    assert_eq!(status.code(), Some(1), "aborted turn exits 1 (D12)");
    assert!(
        waited < Duration::from_secs(6),
        "SIGINT → graceful exit took {waited:?}, expected well under the scripted 8s sleep"
    );
}

#[test]
fn p2_resume_rebuilds_history_from_journal() {
    let home = mock_config_home("p2resume");
    // turn 1: creates the session
    let out = Command::new(bin_path())
        .args(["-p", "first question", "--output-format", "json"])
        .env("MARFIM_HOME", &home)
        .env_remove("MARFIM_PROVIDER")
        .env("MARFIM_MOCK_SCRIPT", r#"[{"text":"first answer"}]"#)
        .current_dir("/tmp")
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // find the session journal that was just created
    let sessions = marfim_home_sessions(&home);
    assert!(
        !sessions.is_empty(),
        "session journal exists after first run"
    );

    // turn 2: --resume must rebuild history from the journal (P2)
    let sid = sessions
        .iter()
        .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().to_string()))
        .next()
        .unwrap();
    let out2 = Command::new(bin_path())
        .args([
            "-p",
            "second question",
            "--output-format",
            "json",
            "--resume",
            &sid,
        ])
        .env("MARFIM_HOME", &home)
        .env_remove("MARFIM_PROVIDER")
        .env("MARFIM_MOCK_SCRIPT", r#"[{"text":"second answer"}]"#)
        .current_dir("/tmp")
        .output()
        .unwrap();
    let err2 = String::from_utf8_lossy(&out2.stderr);
    assert!(
        err2.contains("rebuilt") && err2.contains("message(s) from journal"),
        "resume must rebuild history from journal, stderr: {err2}"
    );
    assert_eq!(out2.status.code(), Some(0), "{err2}");
}

fn marfim_home_sessions(home: &std::path::Path) -> Vec<std::path::PathBuf> {
    let dir = home.join("sessions");
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("jsonl") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

#[test]
fn acp_real_turn_over_stdio() {
    // The ACP surface drives SessionHost: session/prompt runs a REAL
    // journaled turn (mock provider), streaming session/update
    // notifications and answering with a real stopReason. No echo.
    let home = mock_config_home("acp");
    let mut child = Command::new(bin_path())
        .args(["--provider", "mock", "acp"])
        .env("MARFIM_HOME", &home)
        .env_remove("MARFIM_PROVIDER")
        .env(
            "MARFIM_MOCK_SCRIPT",
            r#"[{"text":"acp says hello from the real loop"}]"#,
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let requests = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"session/new\",\"params\":{\"cwd\":\"/tmp\"}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"session/prompt\",\"params\":{\"sessionId\":\"s1\",\"content\":[{\"type\":\"text\",\"text\":\"say hi\"}]}}\n",
    );
    stdin.write_all(requests.as_bytes()).unwrap();
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(stdout.contains("\"protocolVersion\":1"), "{stdout}");
    assert!(stdout.contains("\"sessionId\":\"s1\""), "{stdout}");
    assert!(
        stdout.contains("agent_message_chunk")
            && stdout.contains("acp says hello from the real loop"),
        "ACP turn must stream the real assistant text: {stdout}"
    );
    assert!(
        stdout.contains("\"stopReason\":\"end_turn\""),
        "real turn must end with end_turn: {stdout}"
    );
}

#[test]
fn mcp_server_runs_all_tools_through_permission_path() {
    // `marfim mcp` serves the builtin catalog over stdio and routes EVERY
    // tools/call through the PermissionEngine (the old read-only refusal
    // is gone). stdin is the protocol, so Ask verdicts fail closed.
    let home = mock_config_home("mcp-server");
    let mut child = Command::new(bin_path())
        .arg("mcp")
        .env("MARFIM_HOME", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let reqs = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"Bash\",\"arguments\":{\"command\":\"echo perm-path\"}}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"tools/call\",\"params\":{\"name\":\"Read\",\"arguments\":{\"path\":\"/etc/hostname\"}}}\n",
    );
    stdin.write_all(reqs.as_bytes()).unwrap();
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(stdout.contains("\"tools\""), "catalog listed: {stdout}");

    // Bash is Execute-class → Ask → no approver on stdio → DENIED (the
    // call must reach the permission verdict, not a hardcoded refusal)
    let denied = stdout.contains("\"id\":3")
        && (stdout.contains("permission denied") || stdout.contains("denied"));
    assert!(denied, "Bash must be permission-denied over MCP: {stdout}");

    // Read is ReadOnly → Allow in default mode → REAL execution
    assert!(
        stdout.contains("\"id\":4") && stdout.contains("isError\":false"),
        "Read must execute through the permission path: {stdout}"
    );
}

#[test]
fn session_connects_configured_mcp_servers() {
    // build_session constructs McpClient per config `mcpServers` behind
    // the trust gate; tools register as mcp__<server>__<tool> and route
    // through the same permission engine. Scenario (a): unapproved mcp
    // tool → Ask → headless denies → exit 2. Scenario (b): allow-rule →
    // real roundtrip to the child server → exit 0.
    let home = mock_config_home("mcp-client");
    let bin = bin_path();
    std::fs::write(
        home.join(".marfim").join("config.toml"),
        format!(
            "provider = \"mock\"\nmodel = \"mock-1\"\n\n[mcpServers.self]\ncommand = \"{}\"\nargs = [\"mcp\"]\n",
            bin.display()
        ),
    )
    .unwrap();

    // (a) no allow rule: mcp__self__Read is Execute-class → Ask → denied
    let out = Command::new(&bin)
        .args(["-p", "use the mcp tool", "--output-format", "text"])
        .env("MARFIM_HOME", &home)
        .env_remove("MARFIM_PROVIDER")
        .env(
            "MARFIM_MOCK_SCRIPT",
            r#"[{"tool_call": {"id": "m1", "name": "mcp__self__Read", "arguments": {"path": "/etc/hostname"}}}, {"text": "mcp attempt finished"}]"#,
        )
        .current_dir("/tmp")
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("mcp server self: connected"),
        "session must connect the configured mcp server: {err}"
    );
    assert!(
        err.contains("[mcp__self__Read] error: permission denied"),
        "mcp tool call must route through the permission engine: {err}"
    );
    assert_eq!(out.status.code(), Some(2), "{err}");

    // (b) allow rule: the call round-trips to the child server for real
    std::fs::write(
        home.join(".marfim").join("config.toml"),
        format!(
            "provider = \"mock\"\nmodel = \"mock-1\"\n\n[permissions]\nallow = [\"mcp__self__Read\"]\n\n[mcpServers.self]\ncommand = \"{}\"\nargs = [\"mcp\"]\n",
            bin.display()
        ),
    )
    .unwrap();
    let out = Command::new(&bin)
        .args(["-p", "use the mcp tool", "--output-format", "text"])
        .env("MARFIM_HOME", &home)
        .env_remove("MARFIM_PROVIDER")
        .env(
            "MARFIM_MOCK_SCRIPT",
            r#"[{"tool_call": {"id": "m1", "name": "mcp__self__Read", "arguments": {"path": "/etc/hostname"}}}, {"text": "mcp attempt finished"}]"#,
        )
        .current_dir("/tmp")
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("permission denied") && !err.contains("permission denied"),
        "allowed mcp tool must execute: {stdout} | {err}"
    );
    assert!(
        stdout.contains("mcp attempt finished"),
        "turn must complete after the mcp roundtrip: {stdout} | {err}"
    );
    assert_eq!(out.status.code(), Some(0), "{stdout} | {err}");
}
