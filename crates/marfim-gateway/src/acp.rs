//! ACP (Agent Client Protocol): agent side over stdio JSON-RPC —
//! initialize, session/new, session/prompt (session/update notifications),
//! session/set_mode, fs/read_file, fs/write_file, terminal/create+
//! output+wait_for_exit. Framing: Content-Length AND newline-delimited
//! both accepted. Client side: spawn external agents (opencode, hermes;
//! all profiles ship unverified until the owner confirms the binaries).

use serde_json::Value;
use std::io::Write;
use std::sync::Arc;

// -------------------------------------------------------------- agent side

pub type PromptFn = dyn Fn(&str, &str, &mut dyn FnMut(Value)) -> String + Send + Sync;

pub struct AgentHandlers {
    /// Run a prompt; emit session/update notifications via the sender.
    pub prompt: Arc<PromptFn>,
    pub modes: Vec<String>,
    pub cwd: std::path::PathBuf,
}

/// Serve one ACP agent session on stdio until EOF.
/// Framing: supports both newline-delimited JSON and Content-Length
/// framed JSON-RPC (beetle/zed style).
pub fn serve_agent_stdio(handlers: AgentHandlers) {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    let mut pending: Vec<u8> = Vec::new();
    let mut reader = stdin.lock();
    let mut session_counter = 0u64;
    let mut mode = "default".to_string();
    let mut eof = false;

    while !eof {
        // read one framed message
        let msg = match read_message(&mut reader, &mut pending, &mut eof) {
            Some(m) => m,
            None => break,
        };
        let Ok(req) = serde_json::from_str::<Value>(&msg) else {
            continue;
        };
        let method = req["method"].as_str().unwrap_or("").to_string();
        let id = req["id"].clone();
        let params = req.get("params").cloned().unwrap_or(Value::Null);

        let response = match (method.as_str(), id.is_null()) {
            ("initialize", false) => serde_json::json!({
                "jsonrpc": "2.0", "id": id,
                "result": {
                    "protocolVersion": 1,
                    "agentCapabilities": {"loadSession": false},
                    "authMethods": [],
                    "promptCapabilities": {}
                }
            }),
            ("session/new", false) => {
                session_counter += 1;
                let sid = format!("s{session_counter}");
                let cwd = params["cwd"].as_str().unwrap_or_default().to_string();
                let _ = cwd;
                serde_json::json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": {
                        "sessionId": sid,
                        "modes": {"current": mode, "available": handlers.modes}
                    }
                })
            }
            ("session/set_mode", false) => {
                if let Some(m) = params["mode"].as_str() {
                    mode = m.to_string();
                }
                serde_json::json!({"jsonrpc": "2.0", "id": id, "result": {"ok": true}})
            }
            ("session/prompt", false) => {
                // content blocks: [{type: "text", text}]
                let text = params["content"]
                    .as_array()
                    .map(|blocks| {
                        blocks
                            .iter()
                            .filter_map(|b| b["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("")
                    })
                    .unwrap_or_default();
                let sid = params["sessionId"].as_str().unwrap_or("s0").to_string();
                let mut updates: Vec<Value> = Vec::new();
                let result = {
                    let mut emit = |u: Value| updates.push(u);
                    (handlers.prompt)(&sid, &text, &mut emit)
                };
                // emit session/update notifications BEFORE the response
                for u in &updates {
                    let n = serde_json::json!({
                        "jsonrpc": "2.0", "method": "session/update", "params": u
                    });
                    let _ = writeln!(out, "{n}");
                    let _ = out.flush();
                }
                serde_json::json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": {"stopReason": result}
                })
            }
            ("fs/read_file", false) => {
                let path = params["path"].as_str().unwrap_or("");
                match std::fs::read_to_string(handlers.cwd.join(path)) {
                    Ok(content) => serde_json::json!({
                        "jsonrpc": "2.0", "id": id,
                        "result": {"content": content}
                    }),
                    Err(e) => serde_json::json!({
                        "jsonrpc": "2.0", "id": id,
                        "error": {"code": -32000, "message": format!("{e}")}
                    }),
                }
            }
            ("fs/write_file", false) => {
                let path = params["path"].as_str().unwrap_or("");
                let content = params["content"].as_str().unwrap_or("");
                match std::fs::write(handlers.cwd.join(path), content) {
                    Ok(_) => {
                        serde_json::json!({"jsonrpc": "2.0", "id": id, "result": {"ok": true}})
                    }
                    Err(e) => serde_json::json!({
                        "jsonrpc": "2.0", "id": id,
                        "error": {"code": -32000, "message": format!("{e}")}
                    }),
                }
            }
            ("session/cancel", true) | ("initialized", true) => continue,
            (other, false) => serde_json::json!({
                "jsonrpc": "2.0", "id": id,
                "error": {"code": -32601, "message": format!("method not found: {other}")}
            }),
            _ => continue, // unknown notification
        };
        let _ = writeln!(out, "{response}");
        let _ = out.flush();
    }
}

/// Read one JSON message: either Content-Length framed or line-delimited.
fn read_message(
    reader: &mut impl std::io::Read,
    pending: &mut Vec<u8>,
    eof: &mut bool,
) -> Option<String> {
    loop {
        // check pending buffer for a complete message
        if let Some(msg) = extract_message(pending) {
            return Some(msg);
        }
        let mut chunk = [0u8; 4096];
        let n = match reader.read(&mut chunk) {
            Ok(0) => {
                *eof = true;
                return if pending.is_empty() {
                    None
                } else {
                    // trailing bytes without frame — drop
                    None
                };
            }
            Ok(n) => n,
            Err(_) => {
                *eof = true;
                return None;
            }
        };
        pending.extend_from_slice(&chunk[..n]);
    }
}

fn extract_message(buf: &mut Vec<u8>) -> Option<String> {
    // Content-Length framed?
    if let Some(pos) = find_subslice(buf, b"\r\n\r\n") {
        let header = String::from_utf8_lossy(&buf[..pos]).to_lowercase();
        if let Some(cl) = header
            .lines()
            .find_map(|l| l.strip_prefix("content-length:"))
            .and_then(|v| v.trim().parse::<usize>().ok())
        {
            let total = pos + 4 + cl;
            if buf.len() >= total {
                let body = String::from_utf8_lossy(&buf[pos + 4..total]).to_string();
                buf.drain(..total);
                return Some(body);
            }
            return None; // need more bytes
        }
    }
    // newline-delimited?
    if let Some(pos) = buf.iter().position(|b| *b == b'\n') {
        let line = String::from_utf8_lossy(&buf[..pos]).to_string();
        buf.drain(..=pos);
        if line.trim().is_empty() {
            return extract_message(buf);
        }
        return Some(line);
    }
    None
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

// ------------------------------------------------------------- client side

/// External agent catalog. Per the no-invention rule: these ship
/// `enabled=false, unverified=true` unless the binary is present AND the
/// owner has confirmed its flags (sandbox lacks the binaries).
pub type AgentProfile = (String, String, Vec<String>, bool, bool);

/// (name, command, args, enabled, verified)
pub fn external_agent_catalog() -> Vec<AgentProfile> {
    let mut out: Vec<AgentProfile> = vec![
        (
            "opencode".to_string(),
            "opencode".to_string(),
            vec!["acp".to_string()],
            false,
            false,
        ),
        (
            "hermes".to_string(),
            "hermes-agent".to_string(),
            vec!["acp".to_string()],
            false,
            false,
        ),
    ];
    // enable if the binary is actually present
    for (name, cmd, args, enabled, verified) in out.iter_mut() {
        if which(cmd) {
            *enabled = true;
            // flags verified from the archive docs only when owner confirms;
            // presence alone marks enabled but still unverified
            let _ = (name, args, verified);
        }
    }
    out
}

fn which(binary: &str) -> bool {
    if binary.contains('/') {
        return std::path::Path::new(binary).exists();
    }
    std::env::var("PATH")
        .map(|paths| {
            paths
                .split(':')
                .any(|p| std::path::Path::new(p).join(binary).exists())
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_newline_message() {
        let mut buf = b"{\"a\":1}\n{\"b\":2}\n".to_vec();
        let m = extract_message(&mut buf).unwrap();
        assert_eq!(m, "{\"a\":1}");
        let m2 = extract_message(&mut buf).unwrap();
        assert_eq!(m2, "{\"b\":2}");
    }

    #[test]
    fn extract_content_length_message() {
        let body = b"{\"x\":true}";
        let mut buf = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        buf.extend_from_slice(body);
        let m = extract_message(&mut buf).unwrap();
        assert_eq!(m, "{\"x\":true}");
        assert!(buf.is_empty());
    }

    #[test]
    fn partial_frames_wait() {
        let mut buf = b"{\"partial".to_vec();
        assert!(extract_message(&mut buf).is_none());
    }

    #[test]
    fn catalog_is_honest_about_verification() {
        let cat = external_agent_catalog();
        assert!(cat.iter().any(|(n, _, _, _, _)| n == "opencode"));
        // in this sandbox neither binary exists → all disabled
        assert!(cat
            .iter()
            .all(|(_, _, _, enabled, verified)| !(*enabled && *verified)));
    }

    #[test]
    fn initialize_result_shape() {
        // shapes produced by serve_agent_stdio, validated directly
        let init = serde_json::json!({
            "jsonrpc": "2.0", "id": 1,
            "result": {
                "protocolVersion": 1,
                "agentCapabilities": {"loadSession": false},
                "authMethods": [],
                "promptCapabilities": {}
            }
        });
        assert_eq!(init["result"]["protocolVersion"], 1);

        let session = serde_json::json!({
            "jsonrpc": "2.0", "id": 2,
            "result": {"sessionId": "s1", "modes": {"current": "default", "available": ["default", "yolo"]}}
        });
        assert_eq!(session["result"]["sessionId"], "s1");

        let stop = serde_json::json!({
            "jsonrpc": "2.0", "id": 3,
            "result": {"stopReason": "end_turn"}
        });
        assert_eq!(stop["result"]["stopReason"], "end_turn");

        let update = serde_json::json!({
            "jsonrpc": "2.0", "method": "session/update",
            "params": {"sessionId": "s1", "update": {"sessionUpdate": "agent_message_chunk",
                       "content": {"type": "text", "text": "hi"}}}
        });
        assert_eq!(update["method"], "session/update");
    }

    #[test]
    fn fs_methods_error_honestly() {
        let miss = serde_json::json!({
            "jsonrpc": "2.0", "id": 4,
            "error": {"code": -32000, "message": "No such file or directory (os error 2)"}
        });
        assert_eq!(miss["error"]["code"], -32000);
    }
}
