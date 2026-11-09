//! W7.5 — the Bedrock wire (distillation §P wire breadth; cassette-
//! tested). SigV4 is implemented clean-room from AWS's published signing
//! process (Signature Version 4, the `aws4_request` algorithm) and is
//! verified against the official documentation test vector
//! (GET iam.amazonaws.com ListUsers → the documented signature), so the
//! signing chain is provably correct before any Bedrock URL is involved.
//!
//! Honest streaming note: `invoke-with-response-stream` speaks the binary
//! AWS event-stream framing (vnd.amazon.eventstream), NOT SSE. That
//! decoder is deliberately NOT implemented here; this provider uses the
//! NON-streaming `invoke` endpoint and `chat_step_stream` buffers the
//! whole response then emits the text as ONE event. Streaming stays an
//! honest limitation recorded in the ledger — never a fake stream.

use super::anthropic;
use super::*;

pub struct Bedrock {
    pub client: reqwest::Client,
    /// Override for tests and private endpoints (VPC endpoints); empty =
    /// the real `bedrock-runtime.{region}.amazonaws.com`.
    pub base_url: String,
    /// e.g. "us-east-1".
    pub region: String,
    /// Model id or inference-profile id (e.g.
    /// `anthropic.claude-sonnet-4-5-v2:0`; `:` percent-encodes on the URL).
    pub model_id: String,
    pub access_key: String,
    pub secret_key: String,
    pub session_token: Option<String>,
    pub max_retries: u32,
}

// ------------------------------------------------------------- SigV4 core

/// The hex-encoded SHA-256 of an (empty) payload — part of the canonical
/// request.
fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    let d = sha2::Sha256::digest(data);
    d.iter().map(|b| format!("{b:02x}")).collect()
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    use hmac::Mac;
    let mut mac = match hmac::Hmac::<sha2::Sha256>::new_from_slice(key) {
        Ok(m) => m,
        // HMAC-SHA256 accepts EVERY key length (RFC 2104); this arm is
        // unreachable by construction and asserts the invariant loudly
        // (the hygiene doctrine blesses unreachable! for proven ones).
        Err(e) => unreachable!("HMAC accepts any key length: {e}"),
    };
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// URI-encode per the SigV4 rules: RFC 3986 unreserved (`A-Za-z0-9-._~`)
/// stays, everything else becomes `%XX` uppercase hex.
pub fn aws_uri_encode(s: &str, encode_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            b'/' if !encode_slash => out.push('/'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// One signing input, fully resolved. Everything is decided by the caller
/// (date, region, service) so the function stays pure and testable.
pub struct SigV4Input<'a> {
    pub method: &'a str,
    pub host: &'a str,
    /// Raw path, unencoded (e.g. `/model/anthropic.x:0/invoke`).
    pub path: &'a str,
    /// Sorted query pairs (raw keys/values, unencoded).
    pub query: &'a [(&'a str, &'a str)],
    pub payload: &'a [u8],
    /// The Content-Type header to sign. `None` omits it entirely (the AWS
    /// documentation vector signs only host;x-amz-date); Bedrock signs
    /// application/json.
    pub content_type: Option<&'a str>,
    pub access_key: &'a str,
    pub secret_key: &'a str,
    pub session_token: Option<&'a str>,
    /// e.g. "20250830T123600Z".
    pub amz_date: &'a str,
    pub region: &'a str,
    pub service: &'a str,
}

/// The computed signature plus every header the request must carry.
pub struct SigV4Output {
    pub authorization: String,
    pub amz_date: String,
    pub payload_hash: String,
}

/// The `aws4_request` signing process (AWS docs, Signature Version 4):
/// canonical request → string-to-sign → derived key → signature.
pub fn sign(input: &SigV4Input) -> SigV4Output {
    let payload_hash = sha256_hex(input.payload);
    // The canonical URI keeps `/` unencoded (AWS: the path is URI-encoded
    // per segment; slashes are separators, never encoded) — encoding them
    // breaks every signature (the doc vector caught exactly this).
    let canonical_uri = aws_uri_encode(input.path, false);
    let mut canonical_query: Vec<String> = input
        .query
        .iter()
        .map(|(k, v)| format!("{}={}", aws_uri_encode(k, true), aws_uri_encode(v, true)))
        .collect();
    canonical_query.sort();
    let canonical_query = canonical_query.join("&");

    let mut headers: Vec<(String, String)> = vec![
        ("host".into(), input.host.to_ascii_lowercase()),
        ("x-amz-date".into(), input.amz_date.to_string()),
    ];
    if let Some(ct) = input.content_type {
        headers.push(("content-type".into(), ct.to_string()));
    }
    if let Some(tok) = input.session_token {
        headers.push(("x-amz-security-token".into(), tok.to_string()));
    }
    headers.sort();
    let canonical_headers: String = headers.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed_headers: String = headers
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(";");

    let canonical_request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        input.method,
        canonical_uri,
        canonical_query,
        canonical_headers,
        signed_headers,
        payload_hash
    );

    let scope = format!(
        "{}/{}/{}/aws4_request",
        &input.amz_date[..8],
        input.region,
        input.service
    );
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{}\n{}",
        input.amz_date,
        scope,
        sha256_hex(canonical_request.as_bytes())
    );

    // Derived signing key: kSecret → kDate → kRegion → kService → kSigning.
    let k_secret = format!("AWS4{}", input.secret_key);
    let k_date = hmac_sha256(k_secret.as_bytes(), &input.amz_date.as_bytes()[..8]);
    let k_region = hmac_sha256(&k_date, input.region.as_bytes());
    let k_service = hmac_sha256(&k_region, input.service.as_bytes());
    let k_signing = hmac_sha256(&k_service, b"aws4_request");
    let signature = hmac_sha256(&k_signing, string_to_sign.as_bytes());
    let signature: String = signature.iter().map(|b| format!("{b:02x}")).collect();

    SigV4Output {
        authorization: format!(
            "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
            input.access_key, scope, signed_headers, signature
        ),
        amz_date: input.amz_date.to_string(),
        payload_hash,
    }
}

// ------------------------------------------------------------- Bedrock wire

impl Bedrock {
    /// The RAW (unencoded) request path — the URL encodes it once and the
    /// canonical URI encodes it once from the SAME raw form (never a
    /// double-encoded `%253A`).
    fn raw_path(&self) -> String {
        format!("/model/{}/invoke", self.model_id)
    }

    /// (url, host) — the host signs the request, so an overridden base_url
    /// (tests, VPC endpoints) changes BOTH consistently. Never defaults to
    /// the real endpoint when an override is present.
    fn url(&self) -> (String, String) {
        let path = aws_uri_encode(&self.raw_path(), false);
        if !self.base_url.is_empty() {
            let base = self.base_url.trim_end_matches('/');
            let host = base
                .split_once("://")
                .map(|(_, rest)| rest.split('/').next().unwrap_or(rest).to_string())
                .unwrap_or_else(|| base.to_string());
            return (format!("{base}{path}"), host);
        }
        let host = format!("bedrock-runtime.{}.amazonaws.com", self.region);
        (format!("https://{host}{path}"), host)
    }

    /// The invoke request body: the ANTHROPIC Messages shape (Bedrock
    /// speaks the same schema for Anthropic-family models) with
    /// `stream: false` — the invoke endpoint answers one JSON document.
    pub(crate) fn invoke_body(req: &ChatRequest, model_id: &str) -> serde_json::Value {
        let mut body = anthropic::build_body(req, model_id);
        body["stream"] = serde_json::json!(false);
        body
    }

    fn signed_headers(&self, body: &[u8]) -> Vec<(String, String)> {
        let (_, host) = self.url();
        let amz_date = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // Bedrock signs with today's date; formatting is fixed-shape.
        let days = amz_date / 86_400;
        let secs = amz_date % 86_400;
        // Civil-from-days (Howard Hinnant's algorithm) — deterministic,
        // no external time crate.
        let (y, m, d) = civil_from_days(days as i64);
        let date = format!(
            "{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z",
            secs / 3600,
            (secs % 3600) / 60,
            secs % 60
        );
        let sig = sign(&SigV4Input {
            method: "POST",
            host: &host,
            path: &self.raw_path(),
            query: &[],
            payload: body,
            content_type: Some("application/json"),
            access_key: &self.access_key,
            secret_key: &self.secret_key,
            session_token: self.session_token.as_deref(),
            amz_date: &date,
            region: &self.region,
            service: "bedrock",
        });
        vec![
            ("content-type".into(), "application/json".into()),
            ("host".into(), host),
            ("x-amz-date".into(), sig.amz_date),
            ("authorization".into(), sig.authorization),
        ]
    }

    async fn send_once(&self, body: &serde_json::Value) -> Result<reqwest::Response> {
        let (url, _) = self.url();
        let payload = serde_json::to_vec(body)
            .map_err(|e| CoreError::Provider(format!("request serialization failed: {e}")))?;
        let mut rb = self.client.post(url);
        for (k, v) in self.signed_headers(&payload) {
            rb = rb.header(k, v);
        }
        if let Some(tok) = &self.session_token {
            rb = rb.header("x-amz-security-token", tok);
        }
        rb.body(payload)
            .send()
            .await
            .map_err(|e| CoreError::Provider(format!("network error: {e}")))
    }

    async fn send_with_retry(&self, body: &serde_json::Value) -> Result<reqwest::Response> {
        for attempt in 0..=self.max_retries {
            match self.send_once(body).await? {
                r if r.status().is_success() => return Ok(r),
                r => {
                    let status = r.status().as_u16();
                    let text = r.text().await.unwrap_or_default();
                    if !retry_policy(status) || attempt == self.max_retries {
                        return Err(CoreError::Provider(format!(
                            "HTTP {status}: {}",
                            super::openai::truncate(&text, 600)
                        )));
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(backoff_ms(attempt))).await;
                }
            }
        }
        Err(CoreError::Provider("unreachable retry state".into()))
    }
}

/// Days-since-epoch → (y, m, d). Hinnant's civil_from_days.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Map a Bedrock invoke response document onto a [`Step`] — the SAME
/// content/stop_reason/usage schema the Anthropic wire answers with.
pub(crate) fn parse_invoke(body: &serde_json::Value) -> Step {
    let mut acc = Step::default();
    if let Some(content) = body["content"].as_array() {
        for block in content {
            match block["type"].as_str().unwrap_or("") {
                "text" => {
                    let t = block["text"].as_str().unwrap_or("");
                    if !t.is_empty() {
                        acc.blocks.push(Block::Text(t.into()));
                    }
                }
                "thinking" => {
                    let t = block["thinking"].as_str().unwrap_or("");
                    if !t.is_empty() {
                        acc.blocks.push(Block::Thinking(t.into()));
                    }
                }
                _ => {}
            }
        }
    }
    if let Some(sr) = body["stop_reason"].as_str() {
        acc.stop = Some(anthropic::stop_from_anthropic(sr));
    }
    if let Some(u) = body.get("usage") {
        acc.usage.input_tokens = u["input_tokens"].as_u64().unwrap_or(0);
        acc.usage.output_tokens = u["output_tokens"].as_u64().unwrap_or(0);
    }
    acc
}

#[async_trait::async_trait]
impl Provider for Bedrock {
    fn name(&self) -> &str {
        "bedrock"
    }

    async fn chat_step(&self, req: &ChatRequest) -> Result<Step> {
        let body = Self::invoke_body(req, &self.model_id);
        let res = self.send_with_retry(&body).await?;
        let text = res
            .text()
            .await
            .map_err(|e| CoreError::Provider(format!("body read failed: {e}")))?;
        let v: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| CoreError::Provider(format!("invoke response not JSON: {e}")))?;
        Ok(parse_invoke(&v))
    }

    async fn chat_step_stream(
        &self,
        req: &ChatRequest,
        on_event: &mut (dyn FnMut(StreamEvent) + Send),
    ) -> Result<Step> {
        // Honest limitation (module doc): invoke is non-streaming. The
        // full answer arrives as ONE event — the loop contract holds,
        // nothing pretends to stream.
        let step = self.chat_step(req).await?;
        for b in &step.blocks {
            match b {
                Block::Text(t) => on_event(StreamEvent::Text(t.clone())),
                Block::Thinking(t) => on_event(StreamEvent::Thinking(t.clone())),
                _ => {}
            }
        }
        Ok(step)
    }
}

// The invoke path parses plain JSON documents; the shared Anthropic SSE
// types are not needed here (the binary AWS event-stream decoder is the
// honest limitation recorded at the module head).

#[cfg(test)]
mod tests {
    use super::*;

    /// The official AWS Signature Version 4 documentation example:
    /// GET https://iam.amazonaws.com/?Action=ListUsers&Version=2010-05-08,
    /// x-amz-date 20150830T123600Z, us-east-1, iam — the DOCUMENTED
    /// signature is 5d672d79c15b13162d9279b0855cfba6789a8edb4c82c400e06b59
    /// 24a6f2b5d7. The full signing chain (canonical request → string-to-
    /// sign → derived key) is proven against that literal value.
    #[test]
    fn sigv4_matches_the_official_documentation_vector() {
        let sig = sign(&SigV4Input {
            method: "GET",
            host: "iam.amazonaws.com",
            path: "/",
            query: &[("Action", "ListUsers"), ("Version", "2010-05-08")],
            payload: b"",
            content_type: Some("application/x-www-form-urlencoded; charset=utf-8"),
            access_key: "AKIDEXAMPLE",
            secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            session_token: None,
            amz_date: "20150830T123600Z",
            region: "us-east-1",
            service: "iam",
        });
        // The literal Authorization header AWS's SigV4 documentation prints
        // for this exact request.
        assert_eq!(
            sig.authorization,
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/iam/aws4_request, \
             SignedHeaders=content-type;host;x-amz-date, \
             Signature=5d672d79c15b13162d9279b0855cfba6789a8edb4c82c400e06b5924a6f2b5d7"
        );
    }

    #[test]
    fn uri_encode_follows_the_sigv4_rules() {
        assert_eq!(
            aws_uri_encode("anthropic.claude-sonnet-4-5-v2:0", true),
            "anthropic.claude-sonnet-4-5-v2%3A0"
        );
        assert_eq!(
            aws_uri_encode("/model/x/invoke", true),
            "%2Fmodel%2Fx%2Finvoke"
        );
        assert_eq!(aws_uri_encode("/model/x/invoke", false), "/model/x/invoke");
        assert_eq!(aws_uri_encode("a-b.c_d~e", true), "a-b.c_d~e");
    }

    #[test]
    fn invoke_body_is_the_anthropic_shape_without_stream() {
        let req = ChatRequest {
            messages: vec![ChatMessage::user("hi")],
            system: Some("sys".into()),
            cache_breakpoints: true,
            ..Default::default()
        };
        let body = Bedrock::invoke_body(&req, "anthropic.claude-sonnet-4-5-v2:0");
        assert_eq!(body["stream"], serde_json::json!(false));
        assert_eq!(body["model"], "anthropic.claude-sonnet-4-5-v2:0");
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn invoke_response_maps_onto_a_step() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"content":[{"type":"text","text":"answer"},{"type":"thinking","thinking":"hm"}],
                "stop_reason":"end_turn","usage":{"input_tokens":12,"output_tokens":34}}"#,
        )
        .unwrap();
        let step = parse_invoke(&v);
        assert_eq!(step.text(), "answer");
        assert!(matches!(step.stop, Some(Stop::EndTurn)));
        assert_eq!(step.usage.input_tokens, 12);
        assert_eq!(step.usage.output_tokens, 34);
    }

    #[test]
    fn civil_dates_round_trip() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_475), (2026, 1, 22)); // 2026-01-22
        assert_eq!(civil_from_days(19_731), (2024, 1, 9)); // leap-year era
    }
}
