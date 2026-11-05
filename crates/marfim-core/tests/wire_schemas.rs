//! D10: every outbound body shape is validated against the VENDORED wire
//! schemas (fixtures/*.schema.json) plus the semantic pairing rules in
//! providers::validate. CI runs this offline — the schemas ship in-tree.

use marfim_core::providers::anthropic::build_body as anthropic_body;
use marfim_core::providers::openai::build_body as openai_body;
use marfim_core::providers::{
    validate_anthropic_body, validate_openai_body, ChatMessage, ChatRequest, Provider,
};

fn load_schema(name: &str) -> jsonschema::Validator {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let raw = std::fs::read_to_string(path.join(name))
        .unwrap_or_else(|e| panic!("vendored schema {name} missing: {e}"));
    let v: serde_json::Value = serde_json::from_str(&raw).expect("schema parses");
    jsonschema::validator_for(&v).expect("schema compiles")
}

/// A realistic transcript: user → assistant w/ 2 tool calls → 2 tool
/// results → assistant final.
fn transcript() -> Vec<ChatMessage> {
    vec![
        ChatMessage::user("read both files"),
        ChatMessage {
            role: "assistant".into(),
            content: "reading now".into(),
            tool_calls: vec![
                marfim_core::providers::ToolCall {
                    id: "t1".into(),
                    name: "Read".into(),
                    arguments_json: r#"{"path":"a.txt"}"#.into(),
                },
                marfim_core::providers::ToolCall {
                    id: "t2".into(),
                    name: "Read".into(),
                    arguments_json: r#"{"path":"b.txt"}"#.into(),
                },
            ],
            tool_call_id: None,
            is_error: false,
        },
        ChatMessage::tool_result("t1", "file a contents", false),
        ChatMessage::tool_result("t2", "file b missing", true),
        ChatMessage::assistant("done reading"),
    ]
}

fn req() -> ChatRequest {
    ChatRequest {
        messages: transcript(),
        tools: vec![serde_json::json!({
            "name": "Read",
            "description": "Read a file",
            "input_schema": {"type": "object", "properties": {"path": {"type": "string"}}}
        })],
        system: Some("You are Marfim.".into()),
        cache_breakpoints: true,
        ..Default::default()
    }
}

#[test]
fn anthropic_body_matches_vendored_schema_and_semantics() {
    let schema = load_schema("anthropic-messages.schema.json");
    let body = anthropic_body(&req(), "claude-sonnet-4-5");
    assert!(
        schema.is_valid(&body),
        "anthropic body violates vendored schema: {:?}",
        schema.iter_errors(&body).take(3).collect::<Vec<_>>()
    );
    validate_anthropic_body(&body)
        .unwrap_or_else(|e| panic!("anthropic semantic validation failed: {e}"));

    // D10: the two tool results of one batch are merged into ONE user message
    let msgs = body["messages"].as_array().unwrap();
    let result_msgs: Vec<&serde_json::Value> = msgs
        .iter()
        .filter(|m| {
            m["content"].is_array()
                && m["content"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|b| b["type"] == "tool_result")
        })
        .collect();
    assert_eq!(
        result_msgs.len(),
        1,
        "tool results of one batch must merge into ONE user message (D10)"
    );
    let blocks = result_msgs[0]["content"].as_array().unwrap();
    assert_eq!(
        blocks.len(),
        2,
        "both tool_result blocks in that one message"
    );
    assert_eq!(blocks[0]["type"], "tool_result");
    assert_eq!(blocks[0]["tool_use_id"], "t1");
    assert_eq!(blocks[1]["is_error"], true, "error flag rides the block");
    assert_eq!(
        msgs[0]["role"], "user",
        "conversation starts with user (D10)"
    );
    // no raw "tool" role ever escapes to the Anthropic wire
    assert!(
        msgs.iter().all(|m| m["role"] != "tool"),
        "role `tool` is never sent to Anthropic"
    );
}

#[test]
fn openai_body_matches_vendored_schema_and_semantics() {
    let schema = load_schema("openai-chat-completions.schema.json");
    let body = openai_body(&req(), "gpt-5");
    assert!(
        schema.is_valid(&body),
        "openai body violates vendored schema: {:?}",
        schema.iter_errors(&body).take(3).collect::<Vec<_>>()
    );
    validate_openai_body(&body)
        .unwrap_or_else(|e| panic!("openai semantic validation failed: {e}"));
    let msgs = body["messages"].as_array().unwrap();
    assert_eq!(msgs[0]["role"], "system");
    assert_eq!(msgs[1]["role"], "user", "conversation starts with user");
    assert_eq!(msgs[3]["role"], "tool");
    assert!(
        msgs[3]["content"].is_string(),
        "openai tool content is a string (D10)"
    );
    assert!(
        msgs[4]["content"].as_str().unwrap().starts_with("[error]"),
        "error tool results are marked in-band"
    );
}

#[test]
fn schema_rejects_structurally_broken_bodies() {
    let schema = load_schema("anthropic-messages.schema.json");
    let bad = serde_json::json!({
        "model": "claude-sonnet-4-5",
        "max_tokens": 0,
        "messages": [{"role": "tool", "content": "nope"}]
    });
    assert!(!schema.is_valid(&bad), "schema must reject broken bodies");

    let oai = load_schema("openai-chat-completions.schema.json");
    let bad_oai = serde_json::json!({
        "model": "gpt-5",
        "messages": [{"role": "wizard", "content": "nope"}]
    });
    assert!(!oai.is_valid(&bad_oai));
}

#[test]
fn anthropic_provider_refuses_invalid_transcript_before_sending() {
    // a transcript with an orphan tool result must be rejected by the
    // provider before any HTTP request is made
    let bad_req = ChatRequest {
        messages: vec![
            ChatMessage::user("hi"),
            ChatMessage::tool_result("orphan-1", "no such call", false),
        ],
        ..Default::default()
    };
    let a = marfim_core::providers::Anthropic {
        client: marfim_core::http_client(),
        base_url: "http://127.0.0.1:1".into(),
        api_key: "k".into(),
        model: "claude-sonnet-4-5".into(),
        max_retries: 0,
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let err = rt
        .block_on(a.chat_step(&bad_req))
        .expect_err("orphan tool_result must fail validation");
    assert!(
        format!("{err}").contains("vendored schema"),
        "rejection mentions the vendored schema: {err}"
    );
}
