//! P3 ACP handshake fixtures: the recorded request transcripts in
//! `fixtures/acp/handshake/` replay through `serve_agent_stream` with
//! in-memory buffers, and the responses are asserted field-by-field.
//! This is the protocol-compliance evidence for OUR agent side; external
//! client profiles (opencode/hermes) stay `verified=false` until the
//! owner runs them against the real binaries.

use marfim_gateway::acp::{serve_agent_stream, AgentHandlers};
use serde_json::Value;
use std::io::{BufReader, Cursor, Read};
use std::sync::Arc;

fn fixture(name: &str) -> String {
    let path = format!(
        "{}/../../fixtures/acp/handshake/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("fixture {name} missing: {e}"))
}

/// Drive a fixture through the agent and return (responses, updates)
/// where updates are the session/update notifications in order.
fn run_fixture(name: &str, prompt_text: &'static str) -> (Vec<Value>, Vec<Value>) {
    let transcript = fixture(name);
    let mut input = Vec::new();
    for line in transcript.lines() {
        if line.trim().is_empty() {
            continue;
        }
        input.extend_from_slice(line.as_bytes());
        input.push(b'\n');
    }
    let handlers = AgentHandlers {
        prompt: Arc::new(move |_sid, text, emit| {
            assert_eq!(text, prompt_text, "prompt content blocks joined in order");
            emit(
                serde_json::json!({"sessionUpdate": "agent_message", "content": {"type": "text", "text": format!("echo: {text}")}}),
            );
            "done".to_string()
        }),
        modes: vec!["default".into(), "yolo".into()],
        cwd: std::env::temp_dir(),
    };
    let mut out: Vec<u8> = Vec::new();
    let mut reader = BufReader::new(Cursor::new(input));
    let mut pending = Vec::new();
    serve_agent_stream(&mut reader, &mut out, handlers, &mut pending);
    // read everything the agent wrote
    let mut s = String::new();
    let mut cursor = Cursor::new(out);
    cursor.read_to_string(&mut s).unwrap();
    let mut responses = Vec::new();
    let mut updates = Vec::new();
    for line in s.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let v: Value =
            serde_json::from_str(line).unwrap_or_else(|e| panic!("bad line {line}: {e}"));
        if v.get("method").and_then(|m| m.as_str()) == Some("session/update") {
            updates.push(v["params"].clone());
        } else if v.get("id").is_some() {
            responses.push(v);
        }
    }
    (responses, updates)
}

#[test]
fn fixture_initialize_hands_protocol_version() {
    let (responses, updates) = run_fixture("initialize.jsonl", "");
    assert!(updates.is_empty());
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0]["id"], 1);
    assert_eq!(responses[0]["result"]["protocolVersion"], 1);
    assert_eq!(
        responses[0]["result"]["agentCapabilities"]["loadSession"],
        false
    );
}

#[test]
fn fixture_session_roundtrip_updates_before_response() {
    let (responses, updates) = run_fixture("session-roundtrip.jsonl", "hello marfim");
    assert_eq!(
        responses.len(),
        3,
        "initialize + session/new + session/prompt"
    );
    // initialize
    assert_eq!(responses[0]["result"]["protocolVersion"], 1);
    // session/new
    assert_eq!(responses[1]["result"]["sessionId"], "s1");
    assert_eq!(responses[1]["result"]["modes"]["current"], "default");
    assert_eq!(
        responses[1]["result"]["modes"]["available"],
        serde_json::json!(["default", "yolo"])
    );
    // prompt: the update notification MUST land before the response line
    assert_eq!(updates.len(), 1);
    assert_eq!(
        updates[0]["content"]["text"],
        serde_json::json!("echo: hello marfim")
    );
    assert_eq!(responses[2]["result"]["stopReason"], "done");
}

#[test]
fn fixture_missing_file_is_jsonrpc_error_not_a_panic() {
    let (responses, _) = run_fixture("fs-read-missing.jsonl", "");
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0]["error"]["code"], -32000);
    assert!(
        responses[0]["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("definitely/not/here.txt"),
        "error names the missing path"
    );
}

#[test]
fn fixture_unknown_method_is_method_not_found() {
    let (responses, _) = run_fixture("unknown-method.jsonl", "");
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0]["error"]["code"], -32601);
    assert!(responses[0]["error"]["message"]
        .as_str()
        .unwrap_or("")
        .contains("definitely/not/a/method"));
}

#[test]
fn external_profiles_stay_unverified_without_binaries() {
    for (name, _cmd, _args, _enabled, verified) in marfim_gateway::acp::external_agent_catalog() {
        // the no-invention rule: nothing is marked verified from docs alone
        assert!(!verified, "profile {name} must not claim verification");
    }
}
