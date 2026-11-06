//! Integration: full client↔server MCP roundtrip against a real spawned
//! child process (node-free /bin/sh server) — handshake, tools/list,
//! tools/call, all with strict request-id matching. Also proves that a
//! broken server that replays stale ids fails fast and honestly (bounded
//! deadline) instead of burning a 30 s fixed wait.

use marfim_mcp::client::McpClient;
use std::time::Duration;

#[test]
fn request_response_lines_roundtrip() {
    let req = marfim_mcp::jsonrpc::Request::new(7, "tools/list", serde_json::json!({}));
    let line = req.to_line();
    let parsed: marfim_mcp::jsonrpc::Request = serde_json::from_str(&line).unwrap();
    assert_eq!(parsed.id, Some(serde_json::json!(7)));
    assert_eq!(parsed.method, "tools/list");

    let resp = marfim_mcp::jsonrpc::Response::ok(
        serde_json::json!(7),
        serde_json::json!({"tools": [{"name": "Read"}]}),
    );
    let line = resp.to_line();
    let parsed: marfim_mcp::jsonrpc::Response = serde_json::from_str(&line).unwrap();
    assert_eq!(parsed.id, serde_json::json!(7));
    assert!(parsed.result.unwrap()["tools"][0]["name"] == "Read");
}

/// Write an MCP test server script and return its path. The server
/// answers EVERY request with the request's own id (strict JSON-RPC id
/// matching) and serves initialize, tools/list and tools/call.
fn write_id_echo_server(tag: &str) -> std::path::PathBuf {
    let script = r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"tools/list"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo_tool","description":"echo test tool","inputSchema":{"type":"object","properties":{}}}]}}\n' "$id" ;;
    *'"tools/call"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"pong"}]}}\n' "$id" ;;
    *)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","serverInfo":{"name":"echo"}}}\n' "$id" ;;
  esac
done
"#;
    let tmp = std::env::temp_dir().join(format!("mcp-echo-{tag}-{}.sh", std::process::id()));
    std::fs::write(&tmp, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    tmp
}

#[tokio::test]
async fn client_full_roundtrip_with_strict_id_matching() {
    let script = write_id_echo_server("full");
    let c = McpClient::connect(
        "echo",
        &marfim_mcp::client::McpServerConfig::Stdio {
            command: "/bin/sh".into(),
            args: vec![script.to_string_lossy().to_string()],
            env: std::collections::BTreeMap::new(),
        },
    )
    .await
    .expect("handshake (initialize + tools/list) must succeed");

    // tools/list answered with the right id and the listing was namespaced
    let names = c.tool_names().await;
    assert!(
        names.iter().any(|n| n == "echo_tool"),
        "echo_tool missing from listing: {names:?}"
    );

    // tools/call answered with the right id and payload round-tripped
    let res = c
        .call_tool("echo_tool", serde_json::json!({}))
        .await
        .expect("tools/call must succeed");
    assert_eq!(res["content"][0]["text"], "pong", "{res}");

    c.shutdown().await;
    let _ = std::fs::remove_file(&script);
}

#[tokio::test]
async fn stale_id_server_fails_fast_and_honestly() {
    // A broken server that always replays id:1: initialize (id 1) matches,
    // then tools/list (id 2) never gets a matching response. The injected
    // deadline bounds the wait — this suite used to burn a fixed 30 s here.
    let script = r#"while IFS= read -r line; do
  echo "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2024-11-05\",\"serverInfo\":{\"name\":\"echo\"}}}"
done"#;
    let tmp = std::env::temp_dir().join(format!("mcp-stale-{}.sh", std::process::id()));
    std::fs::write(&tmp, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let t0 = std::time::Instant::now();
    let res = McpClient::connect_with_timeout(
        "stale",
        &marfim_mcp::client::McpServerConfig::Stdio {
            command: "/bin/sh".into(),
            args: vec![tmp.to_string_lossy().to_string()],
            env: std::collections::BTreeMap::new(),
        },
        Duration::from_millis(300),
    )
    .await;
    let elapsed = t0.elapsed();

    match res {
        Err(e) => assert!(e.contains("timed out"), "honest error expected: {e}"),
        Ok(_) => panic!("stale-id server must not produce a successful handshake"),
    }
    assert!(
        elapsed < Duration::from_secs(5),
        "stale responses must fail fast, took {elapsed:?}"
    );

    let _ = std::fs::remove_file(&tmp);
}
