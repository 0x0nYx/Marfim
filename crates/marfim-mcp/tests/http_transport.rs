//! Streamable-HTTP MCP transport: a REAL stub server on a loopback
//! socket answers JSON-RPC POSTs (initialize, tools/list, tools/call);
//! the client connects, lists tools and calls one through the HTTP wire.

use marfim_mcp::client::{McpClient, McpServerConfig};
use std::collections::BTreeMap;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Minimal streamable-HTTP MCP stub: reads one POST per connection,
/// answers with a JSON-RPC response.
async fn stub_server(listener: TcpListener) {
    loop {
        let Ok((mut sock, _)) = listener.accept().await else {
            return;
        };
        tokio::spawn(async move {
            let mut buf = Vec::new();
            // read until end of headers
            loop {
                let mut chunk = [0u8; 4096];
                let n = match sock.read(&mut chunk).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let head_end = buf
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .unwrap_or(buf.len());
            let headers = String::from_utf8_lossy(&buf[..head_end]).to_lowercase();
            let cl: usize = headers
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
            let body_start = head_end + 4;
            while buf.len() < body_start + cl {
                let mut chunk = [0u8; 4096];
                let n = match sock.read(&mut chunk).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                buf.extend_from_slice(&chunk[..n]);
            }
            let body: serde_json::Value =
                serde_json::from_slice(&buf[body_start..body_start + cl]).unwrap_or_default();
            let method = body["method"].as_str().unwrap_or("");
            let id = body["id"].clone();
            let result = match method {
                "initialize" => serde_json::json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "stub", "version": "0"}
                }),
                "tools/list" => serde_json::json!({
                    "tools": [{
                        "name": "echo",
                        "description": "echoes the input",
                        "inputSchema": {"type": "object", "properties": {"msg": {"type": "string"}}}
                    }]
                }),
                "tools/call" => {
                    let msg = body["params"]["arguments"]["msg"].as_str().unwrap_or("");
                    serde_json::json!({
                        "content": [{"type": "text", "text": format!("echo: {msg}")}]
                    })
                }
                _ => serde_json::json!({"unhandled": method}),
            };
            let resp = serde_json::json!({
                "jsonrpc": "2.0",
                "id": if id.is_null() { serde_json::Value::Null } else { id },
                "result": result
            });
            let resp_body = resp.to_string();
            let http = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp_body}",
                resp_body.len()
            );
            let _ = sock.write_all(http.as_bytes()).await;
            let _ = sock.shutdown().await;
        });
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn http_transport_lists_and_calls() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(stub_server(listener));

    let client = McpClient::connect(
        "stub",
        &McpServerConfig::Http {
            url: format!("http://{addr}/mcp"),
            headers: BTreeMap::new(),
        },
    )
    .await
    .expect("http connect + handshake");

    let names = client.tool_names().await;
    assert_eq!(names, vec!["echo".to_string()], "tools/list over HTTP");

    let res = client
        .call_tool("echo", serde_json::json!({"msg": "hello http"}))
        .await
        .expect("tools/call over HTTP");
    let text = res["content"][0]["text"].as_str().unwrap_or("");
    assert_eq!(text, "echo: hello http", "{res}");
}

#[tokio::test(flavor = "multi_thread")]
async fn http_transport_errors_honestly_on_bad_url() {
    let e = McpClient::connect(
        "bad",
        &McpServerConfig::Http {
            url: "not a url".into(),
            headers: BTreeMap::new(),
        },
    )
    .await;
    assert!(e.is_err());
}
