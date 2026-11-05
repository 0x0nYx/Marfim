//! Outbound-body validators (D10): every body sent to Anthropic or an
//! OpenAI-compatible endpoint is validated BEFORE the request goes out.
//! The vendored JSON Schemas live in `fixtures/` (validated with the
//! `jsonschema` crate in tests); the semantic rules below carry the parts
//! a structural schema cannot express (tool_use/tool_result pairing,
//! user-first conversation).

use std::collections::BTreeSet;

fn is_block_array(v: &serde_json::Value) -> bool {
    v.is_array()
}

/// Anthropic /v1/messages body. Returns Err with a precise reason.
pub fn validate_anthropic_body(body: &serde_json::Value) -> Result<(), String> {
    let Some(obj) = body.as_object() else {
        return Err("body must be a JSON object".into());
    };
    for key in ["model", "max_tokens", "messages"] {
        if !obj.contains_key(key) {
            return Err(format!("missing required field `{key}`"));
        }
    }
    if !obj["model"].is_string() {
        return Err("`model` must be a string".into());
    }
    if !obj["max_tokens"].is_u64() {
        return Err("`max_tokens` must be a positive integer".into());
    }
    let Some(msgs) = obj["messages"].as_array() else {
        return Err("`messages` must be an array".into());
    };
    if msgs.is_empty() {
        return Err("`messages` must not be empty".into());
    }
    // D10: the conversation starts with a user message
    if msgs[0]["role"].as_str() != Some("user") {
        return Err("conversation must start with a user message".into());
    }
    let mut declared: BTreeSet<String> = BTreeSet::new();
    let mut answered: BTreeSet<String> = BTreeSet::new();
    for (i, m) in msgs.iter().enumerate() {
        let Some(role) = m["role"].as_str() else {
            return Err(format!("messages[{i}] missing `role`"));
        };
        if role != "user" && role != "assistant" {
            return Err(format!(
                "messages[{i}] role `{role}` invalid — Anthropic allows only user/assistant"
            ));
        }
        match &m["content"] {
            serde_json::Value::String(_) => {}
            c if is_block_array(c) => {
                for (j, b) in c.as_array().unwrap().iter().enumerate() {
                    let Some(btype) = b["type"].as_str() else {
                        return Err(format!("messages[{i}].content[{j}] missing block `type`"));
                    };
                    match btype {
                        "text" => {
                            if !b["text"].is_string() {
                                return Err(format!(
                                    "messages[{i}].content[{j}] text block needs `text` string"
                                ));
                            }
                        }
                        "thinking" => {
                            if role != "assistant" {
                                return Err(format!(
                                    "messages[{i}].content[{j}] thinking block must be assistant"
                                ));
                            }
                            if !b["thinking"].is_string() {
                                return Err(format!(
                                    "messages[{i}].content[{j}] thinking block needs `thinking`"
                                ));
                            }
                        }
                        "tool_use" => {
                            if role != "assistant" {
                                return Err(format!(
                                    "messages[{i}].content[{j}] tool_use must be assistant"
                                ));
                            }
                            let Some(id) = b["id"].as_str() else {
                                return Err(format!(
                                    "messages[{i}].content[{j}] tool_use missing `id`"
                                ));
                            };
                            if !b["name"].is_string() {
                                return Err(format!(
                                    "messages[{i}].content[{j}] tool_use missing `name`"
                                ));
                            }
                            if !b["input"].is_object() {
                                return Err(format!(
                                    "messages[{i}].content[{j}] tool_use `input` must be object"
                                ));
                            }
                            declared.insert(id.to_string());
                        }
                        "tool_result" => {
                            if role != "user" {
                                return Err(format!(
                                    "messages[{i}].content[{j}] tool_result must be inside a user message (D10)"
                                ));
                            }
                            let Some(tid) = b["tool_use_id"].as_str() else {
                                return Err(format!(
                                    "messages[{i}].content[{j}] tool_result missing `tool_use_id`"
                                ));
                            };
                            if !declared.contains(tid) {
                                return Err(format!(
                                    "messages[{i}].content[{j}] tool_result answers unknown/unprior tool_use `{tid}`"
                                ));
                            }
                            answered.insert(tid.to_string());
                            if let Some(err_flag) = b.get("is_error") {
                                if !err_flag.is_boolean() {
                                    return Err(format!(
                                        "messages[{i}].content[{j}] is_error must be boolean"
                                    ));
                                }
                            }
                        }
                        other => {
                            return Err(format!(
                                "messages[{i}].content[{j}] unknown block type `{other}`"
                            ));
                        }
                    }
                }
            }
            _ => {
                return Err(format!("messages[{i}].content must be string or block array"));
            }
        }
    }
    for id in &declared {
        if !answered.contains(id) {
            return Err(format!("tool_use `{id}` has no tool_result (D10)"));
        }
    }
    if let Some(tools) = obj.get("tools") {
        let Some(tarr) = tools.as_array() else {
            return Err("`tools` must be an array".into());
        };
        for (i, t) in tarr.iter().enumerate() {
            if !t["name"].is_string() {
                return Err(format!("tools[{i}] missing `name`"));
            }
            if !t["input_schema"].is_object() {
                return Err(format!("tools[{i}] missing `input_schema` object"));
            }
        }
    }
    Ok(())
}

/// OpenAI chat/completions body.
pub fn validate_openai_body(body: &serde_json::Value) -> Result<(), String> {
    let Some(obj) = body.as_object() else {
        return Err("body must be a JSON object".into());
    };
    if !obj["model"].is_string() {
        return Err("`model` must be a string".into());
    }
    let Some(msgs) = obj["messages"].as_array() else {
        return Err("`messages` must be an array".into());
    };
    if msgs.is_empty() {
        return Err("`messages` must not be empty".into());
    }
    // D10: conversation starts with user (an optional system preamble first)
    let first = msgs[0]["role"].as_str().unwrap_or("");
    if first == "system" || first == "developer" {
        if msgs.len() < 2 || msgs[1]["role"].as_str() != Some("user") {
            return Err("conversation must start with a user message after the system preamble".into());
        }
    } else if first != "user" {
        return Err("conversation must start with a user message".into());
    }
    let mut declared: BTreeSet<String> = BTreeSet::new();
    let mut answered: BTreeSet<String> = BTreeSet::new();
    for (i, m) in msgs.iter().enumerate() {
        let Some(role) = m["role"].as_str() else {
            return Err(format!("messages[{i}] missing `role`"));
        };
        if !matches!(role, "system" | "developer" | "user" | "assistant" | "tool") {
            return Err(format!("messages[{i}] role `{role}` invalid"));
        }
        if role == "tool" {
            let Some(tid) = m["tool_call_id"].as_str() else {
                return Err(format!("messages[{i}] tool message missing `tool_call_id`"));
            };
            if !declared.contains(tid) {
                return Err(format!(
                    "messages[{i}] tool message answers unknown/unprior tool_call `{tid}`"
                ));
            }
            answered.insert(tid.to_string());
            // OpenAI tool content: string (or null)
            if !(m["content"].is_string() || m["content"].is_null()) {
                return Err(format!(
                    "messages[{i}] tool content must be a string (D10)"
                ));
            }
        }
        if role == "assistant" {
            if let Some(tcs) = m.get("tool_calls") {
                let Some(tarr) = tcs.as_array() else {
                    return Err(format!("messages[{i}] tool_calls must be an array"));
                };
                for (j, tc) in tarr.iter().enumerate() {
                    if tc["type"].as_str() != Some("function") {
                        return Err(format!(
                            "messages[{i}].tool_calls[{j}] type must be `function`"
                        ));
                    }
                    let Some(id) = tc["id"].as_str() else {
                        return Err(format!("messages[{i}].tool_calls[{j}] missing `id`"));
                    };
                    if !tc["function"]["name"].is_string() {
                        return Err(format!(
                            "messages[{i}].tool_calls[{j}] missing function name"
                        ));
                    }
                    if !tc["function"]["arguments"].is_string() {
                        return Err(format!(
                            "messages[{i}].tool_calls[{j}] function.arguments must be a string"
                        ));
                    }
                    declared.insert(id.to_string());
                }
            }
        }
    }
    for id in &declared {
        if !answered.contains(id) {
            return Err(format!("tool_call `{id}` has no tool message (D10)"));
        }
    }
    if let Some(tools) = obj.get("tools") {
        let Some(tarr) = tools.as_array() else {
            return Err("`tools` must be an array".into());
        };
        for (i, t) in tarr.iter().enumerate() {
            if t["type"].as_str() != Some("function") {
                return Err(format!("tools[{i}] type must be `function`"));
            }
            if !t["function"]["name"].is_string() {
                return Err(format!("tools[{i}] missing function name"));
            }
            if !t["function"]["parameters"].is_object() {
                return Err(format!("tools[{i}] missing function parameters object"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn anthropic_good_body() -> serde_json::Value {
        json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 8192,
            "stream": true,
            "system": [{"type": "text", "text": "sys", "cache_control": {"type": "ephemeral"}}],
            "tools": [{"name": "Read", "input_schema": {"type": "object"}}],
            "messages": [
                {"role": "user", "content": "read x"},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "ok"},
                    {"type": "tool_use", "id": "t1", "name": "Read", "input": {"path": "x"}},
                    {"type": "tool_use", "id": "t2", "name": "Read", "input": {"path": "y"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": [{"type": "text", "text": "out"}]},
                    {"type": "tool_result", "tool_use_id": "t2", "content": [{"type": "text", "text": "err"}], "is_error": true}
                ]},
                {"role": "assistant", "content": "done"}
            ]
        })
    }

    #[test]
    fn anthropic_good_body_passes() {
        assert_eq!(validate_anthropic_body(&anthropic_good_body()), Ok(()));
    }

    #[test]
    fn anthropic_rejects_wrong_shapes() {
        let mut b = anthropic_good_body();
        b["messages"][0]["role"] = json!("assistant");
        assert!(validate_anthropic_body(&b)
            .unwrap_err()
            .contains("must start with a user message"));

        let mut b = anthropic_good_body();
        b["messages"][2] = json!({"role": "user", "content": "results dropped"});
        assert!(validate_anthropic_body(&b).is_err(), "tool_use t1/t2 unanswered");

        let mut b = anthropic_good_body();
        b["messages"][2]["content"][0]["tool_use_id"] = json!("nope");
        assert!(validate_anthropic_body(&b).unwrap_err().contains("unknown/unprior"));

        let mut b = anthropic_good_body();
        b["messages"][2]["content"][1]["is_error"] = json!("yes");
        assert!(validate_anthropic_body(&b).unwrap_err().contains("is_error"));

        let mut b = anthropic_good_body();
        b["tools"][0].as_object_mut().unwrap().remove("input_schema");
        assert!(validate_anthropic_body(&b).unwrap_err().contains("input_schema"));
    }

    #[test]
    fn anthropic_tool_results_must_live_in_user_messages() {
        let mut b = anthropic_good_body();
        b["messages"][2]["role"] = json!("assistant");
        let err = validate_anthropic_body(&b).unwrap_err();
        assert!(err.contains("tool_result must be inside a user message"), "{err}");
    }

    fn openai_good_body() -> serde_json::Value {
        json!({
            "model": "gpt-5",
            "stream": true,
            "messages": [
                {"role": "system", "content": "sys"},
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "", "tool_calls": [
                    {"id": "call1", "type": "function", "function": {"name": "Read", "arguments": "{\"path\":\"x\"}"}}
                ]},
                {"role": "tool", "tool_call_id": "call1", "content": "out"},
                {"role": "assistant", "content": "done"}
            ],
            "tools": [{"type": "function", "function": {"name": "Read", "parameters": {"type": "object"}}}]
        })
    }

    #[test]
    fn openai_good_body_passes() {
        assert_eq!(validate_openai_body(&openai_good_body()), Ok(()));
    }

    #[test]
    fn openai_rejects_wrong_shapes() {
        let mut b = openai_good_body();
        b["messages"][3].as_object_mut().unwrap().remove("tool_call_id");
        assert!(validate_openai_body(&b).unwrap_err().contains("missing `tool_call_id`"));

        let mut b = openai_good_body();
        b["messages"].as_array_mut().unwrap().remove(3);
        assert!(validate_openai_body(&b).unwrap_err().contains("has no tool message"));

        let mut b = openai_good_body();
        b["messages"][3]["content"] = json!([{"type": "text", "text": "arr"}]);
        assert!(validate_openai_body(&b).unwrap_err().contains("must be a string"));
    }
}
