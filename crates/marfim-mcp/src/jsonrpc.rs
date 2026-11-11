//! Minimal JSON-RPC 2.0 framing (newline-delimited for stdio transports).

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl Request {
    pub fn new(id: u64, method: &str, params: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: Some(id.into()),
            method: method.into(),
            params: Some(params),
        }
    }

    pub fn notification(method: &str, params: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: None,
            method: method.into(),
            params: Some(params),
        }
    }

    pub fn to_line(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

impl Response {
    pub fn ok(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: Value, code: i64, message: String) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(RpcError { code, message }),
        }
    }

    pub fn to_line(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_request_and_response() {
        let r = Request::new(1, "tools/list", serde_json::json!({}));
        let line = r.to_line();
        let back: Request = serde_json::from_str(&line).unwrap();
        assert_eq!(back.method, "tools/list");

        let res = Response::ok(serde_json::json!(1), serde_json::json!({"tools": []}));
        let line2 = res.to_line();
        let back2: Response = serde_json::from_str(&line2).unwrap();
        assert!(back2.result.is_some());
        assert!(back2.error.is_none());
    }

    #[test]
    fn notifications_have_no_id() {
        let n = Request::notification("initialized", serde_json::json!({}));
        assert!(!n.to_line().contains("\"id\""));
    }

    #[test]
    fn error_response_shape() {
        let e = Response::err(serde_json::json!(2), -32601, "method not found".into());
        let v: Value = serde_json::from_str(&e.to_line()).unwrap();
        assert_eq!(v["error"]["code"], -32601);
    }
}
