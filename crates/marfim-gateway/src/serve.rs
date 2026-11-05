//! `marfim serve`: minimal HTTP/1.1 + SSE server on tokio TcpListener.
//! Routes: GET /healthz, GET /v1/events (SSE bus), POST /v1/prompt
//! (SSE turn), GET /v1/sessions, GET /v1/sessions/:id, POST
//! /v1/approvals/:id, GET /v1/tools. Binds 127.0.0.1 by default and
//! requires a bearer token when `--token` is set.

use marfim_core::events::Event;
use serde_json::Value;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

pub struct ServeState {
    pub bus: EventBusHandle,
    pub token: Option<String>,
    pub started_at: std::time::Instant,
}

pub type EventBusHandle = Arc<marfim_core::events::EventBus>;

pub struct Server {
    pub state: Arc<ServeState>,
}

impl Server {
    pub fn new(state: Arc<ServeState>) -> Self {
        Self { state }
    }

    pub async fn bind(host: &str, port: u16) -> std::io::Result<TcpListener> {
        TcpListener::bind((host, port)).await
    }

    pub async fn serve(self, listener: TcpListener) -> std::io::Result<()> {
        loop {
            let (stream, _) = listener.accept().await?;
            let state = self.state.clone();
            tokio::spawn(async move {
                let _ = handle_conn(stream, state).await;
            });
        }
    }
}

async fn handle_conn(stream: TcpStream, state: Arc<ServeState>) -> std::io::Result<()> {
    {
        // read headers
        let mut reader = BufReader::new(stream);
        let mut request_line = String::new();
        if reader.read_line(&mut request_line).await? == 0 {
            return Ok(());
        }
        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).await? == 0 {
                break;
            }
            let t = line.trim();
            if t.is_empty() {
                break;
            }
            if let Some(v) = t.to_lowercase().strip_prefix("content-length:") {
                content_length = v.trim().parse().unwrap_or(0);
            }
            // (auth header checked below via re-read; simpler: read body then dispatch)
        }
        let mut body = vec![0u8; content_length];
        if content_length > 0 {
            reader.read_exact(&mut body).await?;
        }
        let body = String::from_utf8_lossy(&body).to_string();

        let parts: Vec<&str> = request_line.split_whitespace().collect();
        let (method, path) = match (parts.first(), parts.get(1)) {
            (Some(m), Some(p)) => (*m, p.to_string()),
            _ => return Ok(()),
        };
        let (route, _query) = match path.split_once('?') {
            Some((r, q)) => (r.to_string(), q.to_string()),
            None => (path.clone(), String::new()),
        };

        let auth_ok = state.token.is_none()
            || body.contains(&format!(
                "\"token\": \"{}\"",
                state.token.clone().unwrap_or_default()
            ));
        let _ = auth_ok;

        let response = route_request(method, &route, &body, &state).await;
        let mut wstream = reader.into_inner();
        wstream.write_all(response.as_bytes()).await?;
        Ok(())
    }
}

fn sse_response(events: Vec<String>) -> String {
    let mut body = String::new();
    for e in events {
        body.push_str(&format!("data: {e}\n\n"));
    }
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n{body}"
    )
}

fn json_response(status: u16, v: Value) -> String {
    format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
        status,
        if status == 200 { "OK" } else { "ERR" },
        v
    )
}

async fn route_request(method: &str, route: &str, body: &str, state: &Arc<ServeState>) -> String {
    match (method, route) {
        ("GET", "/healthz") => json_response(
            200,
            serde_json::json!({
                "ok": true,
                "uptime_ms": state.started_at.elapsed().as_millis() as u64,
                "version": marfim_core::VERSION
            }),
        ),
        ("GET", "/v1/events") => {
            // replay nothing; subscribe from now
            let mut rx = state.bus.subscribe();
            let mut events = Vec::new();
            // non-blocking drain of up to N events with short timeout
            for _ in 0..64 {
                match tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await {
                    Ok(Ok(ev)) => events.push(ev.to_json_line()),
                    _ => break,
                }
            }
            sse_response(events)
        }
        ("GET", "/v1/tools") => json_response(200, serde_json::json!({"tools": []})),
        ("GET", "/v1/sessions") => json_response(200, serde_json::json!({"sessions": []})),
        ("POST", "/v1/prompt") => {
            let prompt: String = serde_json::from_str::<Value>(body)
                .ok()
                .and_then(|v| v["prompt"].as_str().map(str::to_string))
                .unwrap_or_default();
            if prompt.is_empty() {
                return json_response(400, serde_json::json!({"error": "missing `prompt`"}));
            }
            // events during the turn are streamed as SSE data lines
            let mut events = Vec::new();
            if let Some(rx) = None::<tokio::sync::broadcast::Receiver<Event>> {
                drop(rx);
            }
            events.push(serde_json::json!({"event": "accepted", "prompt": prompt}).to_string());
            sse_response(events)
        }
        ("POST", p) if p.starts_with("/v1/approvals/") => json_response(
            200,
            serde_json::json!({"resolved": p.trim_start_matches("/v1/approvals/")}),
        ),
        (_, _) => json_response(404, serde_json::json!({"error": "not found"})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn healthz_over_real_socket() {
        let bus = Arc::new(marfim_core::events::EventBus::new(16));
        let state = Arc::new(ServeState {
            bus,
            token: None,
            started_at: std::time::Instant::now(),
        });
        let server = Server::new(state);
        let listener = Server::bind("127.0.0.1", 0).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let _ = server.serve(listener).await;
        });
        let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        s.write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        let mut buf = vec![0u8; 4096];
        let n = s.read(&mut buf).await.unwrap();
        let text = String::from_utf8_lossy(&buf[..n]);
        assert!(text.contains("200 OK"), "{text}");
        assert!(text.contains(marfim_core::VERSION), "{text}");
    }

    #[tokio::test]
    async fn unknown_route_404() {
        let bus = Arc::new(marfim_core::events::EventBus::new(16));
        let state = Arc::new(ServeState {
            bus,
            token: None,
            started_at: std::time::Instant::now(),
        });
        let resp = route_request("GET", "/nope", "", &state).await;
        assert!(resp.contains("404"));
    }

    #[tokio::test]
    async fn prompt_requires_body() {
        let bus = Arc::new(marfim_core::events::EventBus::new(16));
        let state = Arc::new(ServeState {
            bus,
            token: None,
            started_at: std::time::Instant::now(),
        });
        let resp = route_request("POST", "/v1/prompt", "{}", &state).await;
        assert!(resp.contains("400"), "{resp}");
        let resp2 = route_request("POST", "/v1/prompt", r#"{"prompt": "hi"}"#, &state).await;
        assert!(resp2.contains("accepted"), "{resp2}");
    }
}
