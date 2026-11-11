//! LSP server over stdio (Content-Length framing): initialize/shutdown,
//! textDocument/didOpen|didChange, textDocument/completion (slash
//! commands + files), publishDiagnostics (TODO/FIXME + long lines).

use serde_json::Value;
use std::io::{Read, Write};

pub const SLASH_COMMANDS: &[&str] = &[
    "accounts",
    "agents",
    "architect",
    "audit",
    "btw",
    "budget",
    "cachehealth",
    "capabilities",
    "checks",
    "clear",
    "commit",
    "compact",
    "context",
    "copy-context",
    "cost",
    "cron",
    "diff",
    "diffs",
    "dig",
    "doctor",
    "editor",
    "explain",
    "export",
    "forecast",
    "fork",
    "goal",
    "handoff",
    "help",
    "hooks",
    "hot",
    "init",
    "map",
    "mcp",
    "memory",
    "model",
    "models",
    "new",
    "permissions",
    "plan",
    "plugins",
    "policy",
    "profile",
    "provider",
    "quit",
    "recall",
    "recipes",
    "replay",
    "resume",
    "review",
    "rewind",
    "sandbox",
    "sdiff",
    "skills",
    "smoke",
    "stats",
    "status",
    "swarm",
    "theme",
    "tree",
    "undo",
    "usage",
    "verify",
    "wal",
    "workflow",
    "yolo",
];

#[derive(Default)]
pub struct LspState {
    pub documents: std::collections::BTreeMap<String, String>,
}

/// Handle one request; returns the response Value (or None for notifications).
pub fn handle(state: &mut LspState, req: &Value) -> Option<Value> {
    let method = req["method"].as_str()?;
    let id = req["id"].clone();
    let params = req.get("params").cloned().unwrap_or(Value::Null);
    let resp = match method {
        "initialize" => serde_json::json!({
            "jsonrpc": "2.0", "id": id,
            "result": {
                "capabilities": {
                    "textDocumentSync": 1,
                    "completionProvider": {"triggerCharacters": ["/"]},
                    "diagnosticProvider": {"interFileDependencies": false, "workspaceDiagnostics": false}
                },
                "serverInfo": {"name": "marfim-lsp", "version": marfim_core::VERSION}
            }
        }),
        "shutdown" => serde_json::json!({"jsonrpc": "2.0", "id": id, "result": null}),
        "textDocument/completion" => {
            let text = params["context"]["triggerCharacter"].as_str().unwrap_or("");
            let items: Vec<Value> = if text == "/" {
                SLASH_COMMANDS
                    .iter()
                    .map(|c| {
                        serde_json::json!({
                            "label": format!("/{c}"),
                            "kind": 14, // Keyword
                            "detail": "marfim slash command"
                        })
                    })
                    .collect()
            } else {
                vec![]
            };
            serde_json::json!({"jsonrpc": "2.0", "id": id, "result": {"isIncomplete": false, "items": items}})
        }
        "textDocument/didOpen" | "textDocument/didChange" => {
            let uri = params["textDocument"]["uri"]
                .as_str()
                .unwrap_or("")
                .to_string();
            let text = params["textDocument"]["text"]
                .as_str()
                .or_else(|| params["contentChanges"][0]["text"].as_str())
                .unwrap_or("")
                .to_string();
            state.documents.insert(uri.clone(), text);
            serde_json::Value::Null // notification; diagnostics follow separately
        }
        "textDocument/didClose" => {
            if let Some(uri) = params["textDocument"]["uri"].as_str() {
                state.documents.remove(uri);
            }
            serde_json::Value::Null
        }
        "exit" => std::process::exit(0),
        other => serde_json::json!({
            "jsonrpc": "2.0", "id": id,
            "error": {"code": -32601, "message": format!("method not found: {other}")}
        }),
    };
    if resp.is_null() {
        None
    } else {
        Some(resp)
    }
}

/// Diagnostics: TODO/FIXME markers + lines > 120 chars.
pub fn diagnostics(text: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        let mut note = None;
        if trimmed.contains("TODO") || trimmed.contains("FIXME") {
            note = Some(("TODO/FIXME marker", 2u8)); // warning
        } else if line.chars().count() > 120 {
            note = Some(("line longer than 120 chars", 3u8)); // info
        }
        if let Some((msg, sev)) = note {
            out.push(serde_json::json!({
                "range": {
                    "start": {"line": i, "character": 0},
                    "end": {"line": i, "character": line.chars().count()}
                },
                "severity": sev,
                "source": "marfim",
                "message": msg
            }));
        }
    }
    out
}

/// Content-Length framed send.
pub fn framed_send(out: &mut impl Write, v: &Value) -> std::io::Result<()> {
    let body = serde_json::to_string(v)?;
    write!(out, "Content-Length: {}\r\n\r\n{body}", body.len())?;
    out.flush()
}

/// Framed read (one message).
pub fn framed_read(input: &mut impl Read) -> std::io::Result<Option<String>> {
    let mut header = Vec::new();
    let mut byte = [0u8; 1];
    // read until \r\n\r\n
    loop {
        if input.read(&mut byte)? == 0 {
            return Ok(None);
        }
        header.push(byte[0]);
        if header.ends_with(b"\r\n\r\n") {
            break;
        }
        // safety: absurd headers
        if header.len() > 64 * 1024 {
            return Ok(None);
        }
    }
    let header_s = String::from_utf8_lossy(&header);
    let cl: usize = header_s
        .lines()
        .find_map(|l| {
            let l = l.to_lowercase();
            l.strip_prefix("content-length:")
                .and_then(|v| v.trim().parse().ok())
        })
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "no content-length"))?;
    let mut body = vec![0u8; cl];
    input.read_exact(&mut body)?;
    Ok(Some(String::from_utf8_lossy(&body).to_string()))
}

/// Serve one stdio session until exit.
pub fn serve_stdio() {
    let mut state = LspState::default();
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    loop {
        let Some(line) = framed_read(&mut stdin.lock()).ok().flatten() else {
            return;
        };
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        // didOpen/didChange → publish diagnostics
        if let Some(m) = v["method"].as_str() {
            if m.starts_with("textDocument/did") {
                let uri = v["params"]["textDocument"]["uri"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                if let Some(text) = state.documents.get(&uri) {
                    let diags = diagnostics(text);
                    let note = serde_json::json!({
                        "jsonrpc": "2.0", "method": "textDocument/publishDiagnostics",
                        "params": {"uri": uri, "diagnostics": diags}
                    });
                    let _ = framed_send(&mut out, &note);
                }
                continue;
            }
        }
        if let Some(resp) = handle(&mut state, &v) {
            let _ = framed_send(&mut out, &resp);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_lists_capabilities() {
        let mut st = LspState::default();
        let req =
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}});
        let resp = handle(&mut st, &req).unwrap();
        assert_eq!(resp["result"]["capabilities"]["textDocumentSync"], 1);
        assert_eq!(resp["result"]["serverInfo"]["name"], "marfim-lsp");
    }

    #[test]
    fn completion_lists_all_slash_commands() {
        let mut st = LspState::default();
        let req = serde_json::json!({
            "jsonrpc": "2.0", "id": 2, "method": "textDocument/completion",
            "params": {"context": {"triggerCharacter": "/"}}
        });
        let resp = handle(&mut st, &req).unwrap();
        let items = resp["result"]["items"].as_array().unwrap();
        assert_eq!(items.len(), SLASH_COMMANDS.len());
        assert!(items.iter().any(|i| i["label"] == "/rewind"));
    }

    #[test]
    fn didopen_tracks_documents() {
        let mut st = LspState::default();
        let req = serde_json::json!({
            "jsonrpc": "2.0", "method": "textDocument/didOpen",
            "params": {"textDocument": {"uri": "file:///a.rs", "text": "// TODO fix"}}
        });
        assert!(handle(&mut st, &req).is_none());
        assert_eq!(st.documents["file:///a.rs"], "// TODO fix");
    }

    #[test]
    fn diagnostics_find_todos_and_long_lines() {
        let d = diagnostics("// TODO: fix this\nshort\n");
        assert_eq!(d.len(), 1);
        assert_eq!(d[0]["severity"], 2);
        let long = "x".repeat(121);
        let d2 = diagnostics(&long);
        assert_eq!(d2.len(), 1);
        assert_eq!(d2[0]["severity"], 3);
    }

    #[test]
    fn framing_roundtrip() {
        let v = serde_json::json!({"jsonrpc": "2.0", "id": 9, "result": {"x": [1, 2, 3]}});
        let mut buf = Vec::new();
        framed_send(&mut buf, &v).unwrap();
        let text = String::from_utf8(buf.clone()).unwrap();
        assert!(text.starts_with("Content-Length: "));
        let back = framed_read(&mut &buf[..]).unwrap().unwrap();
        let parsed: Value = serde_json::from_str(&back).unwrap();
        assert_eq!(parsed["id"], 9);
    }

    #[test]
    fn unknown_method_is_jsonrpc_error() {
        let mut st = LspState::default();
        let req = serde_json::json!({"jsonrpc": "2.0", "id": 3, "method": "no/such", "params": {}});
        let resp = handle(&mut st, &req).unwrap();
        assert_eq!(resp["error"]["code"], -32601);
    }
}
