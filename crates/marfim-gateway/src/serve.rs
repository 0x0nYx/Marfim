//! `marfim serve`: HTTP/1.1 + SSE gateway on hyper 1.x (via hyper-util's
//! auto connection builder) over tokio TcpListener.
//!
//! Security posture (fail-closed):
//! - bearer token MANDATORY (`--token <t>` or auto-generated, printed
//!   once by the CLI layer); compared in constant time;
//! - Origin/Host validation (DNS-rebinding defense): the Host header and
//!   any present Origin must name an allowed host (bound host + loopback
//!   aliases); violations get 403 before the body is read;
//! - 1 MiB header buffer (hyper `max_buf_size`) and 1 MiB body cap —
//!   oversized declared `Content-Length` is refused with 413 unread;
//! - per-connection timeouts: header phase (hyper `header_read_timeout`
//!   with its `timer`), body phase (bounded, time-boxed collect), and SSE
//!   streams carrying pings and a hard lifetime.
//!
//! Nothing a client sends aborts the process: malformed garbage, huge
//! headers and slowloris dribbles kill only their own connection while
//! the listener keeps serving.
//!
//! Routes (all behind the token):
//!   GET  /healthz            liveness + version
//!   GET  /v1/events          SSE bus subscription (ping + lifetime)
//!   GET  /v1/tools           real tool catalog (name/description/schema)
//!   GET  /v1/sessions        SessionHost snapshot listing
//!   GET  /v1/sessions/:id    one session snapshot
//!   POST /v1/prompt          run a REAL turn on a hosted session (SSE)
//!   GET  /v1/approvals       pending approval jobs (indexed)
//!   POST /v1/approvals/:id   resolve a pending approval for real

use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, Full, Limited, StreamBody};
use hyper::body::{Bytes, Frame, Incoming};
use hyper::header::{AUTHORIZATION, CONTENT_LENGTH, HOST, ORIGIN};
use hyper::service::Service;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto;
use marfim_core::events::{Event, EventBus};
use marfim_core::permissions::{ApprovalAnswer, ApprovalJob};
use marfim_core::session::SessionCommand;
use marfim_core::session_host::{HostedSession, SessionHost, SessionRequest};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};

/// Tool catalog entry served by /v1/tools (built by the CLI layer from
/// the real registry; the gateway never fabricates tool names).
#[derive(Clone, Debug)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Clone, Debug)]
pub struct ServeOptions {
    /// Header-phase timeout (slowloris defense).
    pub header_timeout: Duration,
    /// Body-collection timeout.
    pub body_timeout: Duration,
    /// Hard lifetime for SSE streams.
    pub stream_lifetime: Duration,
    /// SSE keepalive comment interval.
    pub sse_ping: Duration,
    /// Body size cap (bytes).
    pub max_body: usize,
    /// Header buffer cap (bytes).
    pub max_header_buf: usize,
}

impl Default for ServeOptions {
    fn default() -> Self {
        Self {
            header_timeout: Duration::from_secs(30),
            body_timeout: Duration::from_secs(60),
            stream_lifetime: Duration::from_secs(3600),
            sse_ping: Duration::from_secs(15),
            max_body: 1024 * 1024,
            max_header_buf: 1024 * 1024,
        }
    }
}

/// A pending approval under the index. The actor tombstones the job's
/// direct responder (its authoritative answer path is the Approve
/// COMMAND), so resolution routes through the actor's command channel,
/// matching on (tool, summary) — identical pending jobs resolve in
/// arrival order.
struct PendingApproval {
    tool: String,
    summary: String,
    commands: tokio::sync::mpsc::Sender<SessionCommand>,
    bus: Option<EventBus>,
}

#[derive(Default)]
pub struct ApprovalIndex {
    next: AtomicU64,
    map: Mutex<BTreeMap<String, PendingApproval>>,
}

impl ApprovalIndex {
    /// Register a pending job and ANNOUNCE it on the session bus (the
    /// engine's queue is transport-internal; surfaces observe approvals
    /// through events).
    fn register(
        &self,
        job: ApprovalJob,
        commands: tokio::sync::mpsc::Sender<SessionCommand>,
        bus: Option<EventBus>,
    ) -> String {
        let id = format!("a{}", self.next.fetch_add(1, Ordering::SeqCst) + 1);
        if let Some(b) = &bus {
            b.publish(Event::ApprovalRequested {
                approval_id: id.clone(),
                tool: job.tool.clone(),
                summary: job.summary.clone(),
            });
        }
        self.map.lock().unwrap_or_else(|p| p.into_inner()).insert(
            id.clone(),
            PendingApproval {
                tool: job.tool,
                summary: job.summary,
                commands,
                bus,
            },
        );
        id
    }

    /// Resolve by answering the actor's Approve command (which forwards
    /// the real decision to the permission engine). Publishes
    /// ApprovalResolved. Returns whether a pending job matched.
    async fn resolve(&self, id: &str, answer: ApprovalAnswer, decision: &str) -> bool {
        let Some(entry) = self
            .map
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(id)
        else {
            return false;
        };
        let (tx, rx) = tokio::sync::oneshot::channel();
        let sent = entry
            .commands
            .send(SessionCommand::Approve {
                tool: entry.tool.clone(),
                summary: entry.summary.clone(),
                answer,
                reply: tx,
            })
            .await
            .is_ok();
        let matched = sent && rx.await.unwrap_or(false);
        if let Some(b) = &entry.bus {
            b.publish(Event::ApprovalResolved {
                approval_id: id.to_string(),
                decision: decision.to_string(),
            });
        }
        matched
    }

    fn list(&self) -> Vec<(String, String, String)> {
        self.map
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .map(|(id, p)| (id.clone(), p.tool.clone(), p.summary.clone()))
            .collect()
    }
}

pub struct ServeState {
    pub host: Arc<SessionHost>,
    pub token: String,
    pub started_at: std::time::Instant,
    pub options: ServeOptions,
    /// Real tool catalog (from the CLI's registry).
    pub tools: Arc<Vec<ToolInfo>>,
    /// Hosts allowed by Host/Origin checks (lowercased authority or bare
    /// host; loopback aliases are added by the CLI layer).
    pub allowed_hosts: Vec<String>,
    /// Sessions whose approval indexer is already running.
    indexed: Mutex<BTreeSet<String>>,
    /// Pending approvals across all hosted sessions.
    approvals: Arc<ApprovalIndex>,
}

impl ServeState {
    pub fn new(
        host: Arc<SessionHost>,
        token: String,
        tools: Arc<Vec<ToolInfo>>,
        allowed_hosts: Vec<String>,
        options: ServeOptions,
    ) -> Arc<Self> {
        Arc::new(Self {
            host,
            token,
            started_at: std::time::Instant::now(),
            options,
            tools,
            allowed_hosts,
            indexed: Mutex::new(BTreeSet::new()),
            approvals: Arc::new(ApprovalIndex::default()),
        })
    }
}

/// Constant-time equality for short secrets (length is public anyway;
/// the comparison itself never short-circuits on content).
fn ct_eq(a: &str, b: &str) -> bool {
    let (x, y) = (a.as_bytes(), b.as_bytes());
    if x.len() != y.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..x.len() {
        diff |= x[i] ^ y[i];
    }
    diff == 0
}

fn authorized(headers: &hyper::HeaderMap, token: &str) -> bool {
    let supplied = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .unwrap_or("");
    ct_eq(supplied, token)
}

/// Authority validation: accepts "host:port", bare "host", and (for
/// Origin) "scheme://host[:port]". Lowercase compare, bare-host alias of
/// a port-ful allowlist entry counts as allowed.
fn authority_allowed(authority: &str, allowed: &[String]) -> bool {
    let a = authority.trim().to_lowercase();
    let host = a
        .strip_prefix("http://")
        .or_else(|| a.strip_prefix("https://"))
        .unwrap_or(&a);
    let bare = host.split(':').next().unwrap_or("");
    if bare.is_empty() {
        return false;
    }
    for cand in allowed {
        let c = cand.trim().to_lowercase();
        if host == c || bare == c {
            return true;
        }
        let c_bare = c.split(':').next().unwrap_or("");
        if !c_bare.is_empty() && bare == c_bare {
            return true;
        }
    }
    false
}

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
                // one task per connection; every failure is contained to
                // this task — the listener never stops
                let _ = serve_connection(stream, state).await;
            });
        }
    }
}

async fn serve_connection(stream: TcpStream, state: Arc<ServeState>) {
    let svc = Svc {
        state: state.clone(),
    };
    let _ = auto::Builder::new(TokioExecutor::default())
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(state.options.header_timeout)
        .max_buf_size(state.options.max_header_buf)
        .serve_connection_with_upgrades(TokioIo::new(stream), svc)
        .await;
}

pub type RespBody = UnsyncBoxBody<Bytes, Infallible>;

struct Svc {
    state: Arc<ServeState>,
}

impl Service<Request<Incoming>> for Svc {
    type Response = Response<RespBody>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Response<RespBody>, Infallible>> + Send>>;

    fn call(&self, req: Request<Incoming>) -> Self::Future {
        let state = self.state.clone();
        Box::pin(async move { Ok(handle(req, state).await) })
    }
}

fn json_resp(status: StatusCode, v: Value) -> Response<RespBody> {
    let body = Full::new(Bytes::from(v.to_string()))
        .map_err(|e| match e {})
        .boxed_unsync();
    let mut resp = Response::new(body);
    *resp.status_mut() = status;
    resp.headers_mut().insert(
        "content-type",
        hyper::header::HeaderValue::from_static("application/json"),
    );
    resp
}

fn sse_resp<S>(stream: S) -> Response<RespBody>
where
    S: futures_util::Stream<Item = Result<Frame<Bytes>, Infallible>> + Send + 'static,
{
    let body = StreamBody::new(stream)
        .map_err(|e| match e {})
        .boxed_unsync();
    let mut resp = Response::new(body);
    *resp.status_mut() = StatusCode::OK;
    let h = resp.headers_mut();
    h.insert(
        "content-type",
        hyper::header::HeaderValue::from_static("text/event-stream"),
    );
    h.insert(
        "cache-control",
        hyper::header::HeaderValue::from_static("no-cache"),
    );
    resp
}

fn sse_frame(ev: &Event) -> Frame<Bytes> {
    Frame::data(Bytes::from(format!("data: {}\n\n", ev.to_json_line())))
}

fn sse_text(s: &str) -> Frame<Bytes> {
    Frame::data(Bytes::from(format!("data: {s}\n\n")))
}

async fn handle(req: Request<Incoming>, state: Arc<ServeState>) -> Response<RespBody> {
    let (parts, body) = req.into_parts();

    // 1. Host/Origin gate BEFORE anything else.
    let host_hdr = parts
        .headers
        .get(HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if !authority_allowed(&host_hdr, &state.allowed_hosts) {
        return json_resp(
            StatusCode::FORBIDDEN,
            serde_json::json!({"error": "host not allowed (DNS-rebinding guard)"}),
        );
    }
    if let Some(origin) = parts.headers.get(ORIGIN).and_then(|v| v.to_str().ok()) {
        if !authority_allowed(origin, &state.allowed_hosts) {
            return json_resp(
                StatusCode::FORBIDDEN,
                serde_json::json!({"error": "origin not allowed"}),
            );
        }
    }

    // 2. Token gate (mandatory, constant-time).
    if !authorized(&parts.headers, &state.token) {
        return json_resp(
            StatusCode::UNAUTHORIZED,
            serde_json::json!({"error": "unauthorized"}),
        );
    }

    // 3. Body: refuse declared oversize unread; bound real reads in size
    //    and time.
    let body_json: Option<Value> = if parts.method == Method::POST {
        if let Some(cl) = parts
            .headers
            .get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
        {
            if cl > state.options.max_body as u64 {
                return json_resp(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    serde_json::json!({
                        "error": format!("body exceeds {} byte cap", state.options.max_body)
                    }),
                );
            }
        }
        let limited = Limited::new(body, state.options.max_body);
        match tokio::time::timeout(state.options.body_timeout, BodyExt::collect(limited)).await {
            Err(_) => {
                return json_resp(
                    StatusCode::REQUEST_TIMEOUT,
                    serde_json::json!({"error": "body read timed out"}),
                );
            }
            Ok(Err(e)) => {
                let over = e
                    .downcast_ref::<http_body_util::LengthLimitError>()
                    .is_some();
                return json_resp(
                    if over {
                        StatusCode::PAYLOAD_TOO_LARGE
                    } else {
                        StatusCode::BAD_REQUEST
                    },
                    serde_json::json!({"error": "body read failed"}),
                );
            }
            Ok(Ok(collected)) => {
                let bytes = collected.to_bytes();
                if bytes.is_empty() {
                    None
                } else {
                    serde_json::from_slice::<Value>(&bytes).ok()
                }
            }
        }
    } else {
        None
    };

    let route = parts.uri.path().to_string();
    let method = parts.method.clone();

    match (method, route.as_str()) {
        (Method::GET, "/healthz") => json_resp(
            StatusCode::OK,
            serde_json::json!({
                "ok": true,
                "uptime_ms": state.started_at.elapsed().as_millis() as u64,
                "version": marfim_core::VERSION
            }),
        ),
        (Method::GET, "/v1/tools") => json_resp(
            StatusCode::OK,
            serde_json::json!({
                "tools": state
                    .tools
                    .iter()
                    .map(|t| serde_json::json!({
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    }))
                    .collect::<Vec<_>>()
            }),
        ),
        (Method::GET, "/v1/sessions") => {
            let snaps = state.host.list().await;
            json_resp(
                StatusCode::OK,
                serde_json::json!({
                    "sessions": serde_json::to_value(&snaps).unwrap_or_default()
                }),
            )
        }
        (Method::GET, r) if r.starts_with("/v1/sessions/") => {
            let id = r.trim_start_matches("/v1/sessions/");
            match state.host.get(id).await {
                Some(s) => {
                    let snap = s.snapshot().await;
                    json_resp(
                        StatusCode::OK,
                        serde_json::to_value(&snap).unwrap_or_default(),
                    )
                }
                None => json_resp(
                    StatusCode::NOT_FOUND,
                    serde_json::json!({"error": "no such session"}),
                ),
            }
        }
        (Method::POST, "/v1/prompt") => {
            let prompt = body_json
                .as_ref()
                .and_then(|v| v["prompt"].as_str())
                .unwrap_or("")
                .to_string();
            if prompt.is_empty() {
                return json_resp(
                    StatusCode::BAD_REQUEST,
                    serde_json::json!({"error": "missing `prompt`"}),
                );
            }
            let sid = body_json
                .as_ref()
                .and_then(|v| v["session_id"].as_str())
                .map(str::to_string);
            let hosted = match state
                .host
                .get_or_create(sid.as_deref(), SessionRequest::default())
                .await
            {
                Ok(h) => h,
                Err(e) => {
                    return json_resp(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        serde_json::json!({"error": e}),
                    );
                }
            };
            ensure_indexer(&state, &hosted);
            prompt_stream(state, hosted, prompt).await
        }
        (Method::GET, "/v1/approvals") => {
            let pending: Vec<Value> = state
                .approvals
                .list()
                .into_iter()
                .map(|(id, tool, summary)| {
                    serde_json::json!({"id": id, "tool": tool, "summary": summary})
                })
                .collect();
            json_resp(StatusCode::OK, serde_json::json!({"approvals": pending}))
        }
        (Method::POST, r) if r.starts_with("/v1/approvals/") => {
            let id = r.trim_start_matches("/v1/approvals/").to_string();
            let decision = body_json
                .as_ref()
                .and_then(|v| v["decision"].as_str())
                .unwrap_or("")
                .to_lowercase();
            let answer = match decision.as_str() {
                "allow" | "allow_once" => ApprovalAnswer::Allow,
                "allow_always" => ApprovalAnswer::AllowAlways,
                "deny" => ApprovalAnswer::Deny,
                _ => {
                    return json_resp(
                        StatusCode::BAD_REQUEST,
                        serde_json::json!({
                            "error": "decision must be allow|allow_once|allow_always|deny"
                        }),
                    );
                }
            };
            let matched = state.approvals.resolve(&id, answer, &decision).await;
            if matched {
                json_resp(
                    StatusCode::OK,
                    serde_json::json!({"resolved": id, "decision": decision}),
                )
            } else {
                json_resp(
                    StatusCode::NOT_FOUND,
                    serde_json::json!({"error": "no pending approval with that id"}),
                )
            }
        }
        (Method::GET, "/v1/events") => events_stream(state).await,
        (Method::POST, _) | (Method::GET, _) => json_resp(
            StatusCode::NOT_FOUND,
            serde_json::json!({"error": "not found"}),
        ),
        _ => json_resp(
            StatusCode::METHOD_NOT_ALLOWED,
            serde_json::json!({"error": "method not allowed"}),
        ),
    }
}

/// Start (once per session) the approval indexer: drains the hosted
/// session's approval queue into the shared index so /v1/approvals is
/// backed by REAL pending jobs.
fn ensure_indexer(state: &Arc<ServeState>, hosted: &Arc<HostedSession>) {
    let sid = hosted.actor.session_id.clone();
    let fresh = {
        let mut guard = state.indexed.lock().unwrap_or_else(|p| p.into_inner());
        guard.insert(sid)
    };
    if !fresh {
        return;
    }
    let Some(mut rx) = hosted.take_approval_receiver() else {
        return;
    };
    let index = Arc::clone(&state.approvals);
    let bus = hosted.bus.clone();
    let commands = hosted.actor.commands.clone();
    tokio::spawn(async move {
        while let Some(job) = rx.recv().await {
            index.register(job, commands.clone(), Some(bus.as_ref().clone()));
        }
    });
}

/// SSE stream of one REAL turn. Subscribes BEFORE prompting (no event
/// loss), forwards every bus event as a data frame, emits one terminal
/// frame carrying the actor's real StopReason, then ends the stream.
async fn prompt_stream(
    state: Arc<ServeState>,
    hosted: Arc<HostedSession>,
    prompt: String,
) -> Response<RespBody> {
    let rx = hosted.bus.subscribe();
    let (tx, reply) =
        tokio::sync::oneshot::channel::<Result<marfim_core::loop_::StopReason, String>>();
    if hosted
        .actor
        .commands
        .send(SessionCommand::Prompt {
            text: prompt,
            reply: tx,
        })
        .await
        .is_err()
    {
        return json_resp(
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({"error": "session actor is gone"}),
        );
    }
    let ping = state.options.sse_ping;
    let deadline = tokio::time::Instant::now() + state.options.stream_lifetime;
    let stream = futures_util::stream::unfold(
        (rx, Some(reply), deadline, ping, false),
        |(mut rx, mut reply, deadline, ping, done)| async move {
            if done {
                return None;
            }
            if tokio::time::Instant::now() >= deadline {
                let f = sse_text("{\"event\":\"stream_deadline\"}");
                return Some((Ok(f), (rx, reply, deadline, ping, true)));
            }
            match tokio::time::timeout(ping, rx.recv()).await {
                Err(_) => Some((Ok(sse_text(": ping")), (rx, reply, deadline, ping, false))),
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(n))) => {
                    let f = sse_text(&format!("{{\"event\":\"lagged\",\"dropped\":{n}}}"));
                    Some((Ok(f), (rx, reply, deadline, ping, false)))
                }
                Ok(Err(_)) => {
                    // bus closed: actor gone
                    let f = sse_text("{\"event\":\"turn_end\",\"stop_reason\":\"error\"}");
                    Some((Ok(f), (rx, reply, deadline, ping, true)))
                }
                Ok(Ok(ev)) => match ev {
                    Event::TurnEnded { .. } => {
                        // the actor sends its reply as the turn unwinds;
                        // bound the wait so a wedged actor can't hold the
                        // stream open forever
                        let stop = match reply
                            .take()
                            .map(|r| tokio::time::timeout(Duration::from_secs(5), r))
                        {
                            Some(fut) => match fut.await {
                                Ok(Ok(Ok(stop))) => stop.normalized().to_string(),
                                _ => "error".to_string(),
                            },
                            None => "error".to_string(),
                        };
                        let f = sse_text(&format!(
                            "{{\"event\":\"turn_end\",\"stop_reason\":\"{stop}\"}}"
                        ));
                        Some((Ok(f), (rx, reply, deadline, ping, true)))
                    }
                    other => {
                        let f = sse_frame(&other);
                        Some((Ok(f), (rx, reply, deadline, ping, false)))
                    }
                },
            }
        },
    );
    sse_resp(stream)
}

/// SSE subscription to the default hosted session's bus (pings + hard
/// lifetime). With no session yet, the stream is pings only.
async fn events_stream(state: Arc<ServeState>) -> Response<RespBody> {
    let rx = state
        .host
        .default_session()
        .await
        .map(|s| s.bus.subscribe());
    let ping = state.options.sse_ping;
    let deadline = tokio::time::Instant::now() + state.options.stream_lifetime;
    let stream = futures_util::stream::unfold(
        (rx, deadline, ping),
        move |(mut rx, deadline, ping)| async move {
            if tokio::time::Instant::now() >= deadline {
                let f = sse_text("{\"event\":\"stream_deadline\"}");
                return Some((Ok(f), (None, deadline, ping)));
            }
            match rx.as_mut() {
                None => {
                    tokio::time::sleep(ping).await;
                    Some((Ok(sse_text(": ping")), (rx, deadline, ping)))
                }
                Some(r) => match tokio::time::timeout(ping, r.recv()).await {
                    Err(_) => Some((Ok(sse_text(": ping")), (rx, deadline, ping))),
                    Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(n))) => {
                        let f = sse_text(&format!("{{\"event\":\"lagged\",\"dropped\":{n}}}"));
                        Some((Ok(f), (rx, deadline, ping)))
                    }
                    Ok(Err(_)) => {
                        let f = sse_text("{\"event\":\"stream_deadline\"}");
                        Some((Ok(f), (None, deadline, ping)))
                    }
                    Ok(Ok(ev)) => Some((Ok(sse_frame(&ev)), (rx, deadline, ping))),
                },
            }
        },
    );
    sse_resp(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with(token: &str) -> Arc<ServeState> {
        let host = Arc::new(SessionHost::new(Arc::new(|_req| {
            Box::pin(async { Err("no factory in unit tests".to_string()) })
        })));
        ServeState::new(
            host,
            token.to_string(),
            Arc::new(vec![]),
            vec!["127.0.0.1".into(), "localhost".into()],
            ServeOptions {
                header_timeout: Duration::from_millis(400),
                body_timeout: Duration::from_secs(2),
                stream_lifetime: Duration::from_secs(2),
                sse_ping: Duration::from_millis(50),
                max_body: 1024 * 1024,
                max_header_buf: 64 * 1024,
            },
        )
    }

    #[test]
    fn ct_equal_and_unequal() {
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "abcd"));
        assert!(!ct_eq("", "x"));
    }

    #[test]
    fn authority_rules() {
        let allowed = vec!["127.0.0.1:8080".into(), "localhost".into()];
        assert!(authority_allowed("127.0.0.1:8080", &allowed));
        assert!(authority_allowed("127.0.0.1", &allowed));
        assert!(authority_allowed("localhost:9999", &allowed));
        assert!(authority_allowed("http://localhost:8080", &allowed));
        assert!(!authority_allowed("evil.example.com", &allowed));
        assert!(!authority_allowed("127.0.0.1.evil.com:8080", &allowed));
        assert!(!authority_allowed("", &allowed));
    }

    async fn start(state: Arc<ServeState>) -> u16 {
        let server = Server::new(state);
        let listener = Server::bind("127.0.0.1", 0).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let _ = server.serve(listener).await;
        });
        port
    }

    async fn get(port: u16, path: &str, auth: Option<&str>, host_hdr: &str) -> (u16, String) {
        let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let auth_line = auth
            .map(|t| format!("Authorization: Bearer {t}\r\n"))
            .unwrap_or_default();
        let req = format!(
            "GET {path} HTTP/1.1\r\nHost: {host_hdr}\r\n{auth_line}Connection: close\r\n\r\n"
        );
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        s.write_all(req.as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut buf)).await;
        let text = String::from_utf8_lossy(&buf).to_string();
        let status: u16 = text
            .split_whitespace()
            .nth(1)
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        (status, text)
    }

    #[tokio::test]
    async fn unauthenticated_gets_401_and_process_survives() {
        let port = start(state_with("s3cret")).await;
        let (code, _) = get(port, "/v1/sessions", None, "127.0.0.1").await;
        assert_eq!(code, 401);
        let (code2, _) = get(port, "/v1/sessions", Some("wrong"), "127.0.0.1").await;
        assert_eq!(code2, 401);
        // alive: authorized request succeeds afterwards
        let (code3, _) = get(port, "/v1/sessions", Some("s3cret"), "127.0.0.1").await;
        assert_eq!(code3, 200);
    }

    #[tokio::test]
    async fn hostile_host_header_gets_403() {
        let port = start(state_with("t")).await;
        let (code, body) = get(port, "/healthz", Some("t"), "attacker.example.com").await;
        assert_eq!(code, 403, "{body}");
        let (code2, _) = get(port, "/healthz", Some("t"), "127.0.0.1").await;
        assert_eq!(code2, 200);
    }

    #[tokio::test]
    async fn giant_content_length_is_413_and_process_alive() {
        let port = start(state_with("t")).await;
        let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let req = "POST /v1/prompt HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer t\r\nContent-Type: application/json\r\nContent-Length: 1000000000000\r\n\r\n";
        s.write_all(req.as_bytes()).await.unwrap();
        // send >1MiB to prove the cap trips on real bytes too
        let chunk = vec![b'a'; 64 * 1024];
        for _ in 0..20 {
            let _ = s.write_all(&chunk).await;
        }
        let mut buf = vec![0u8; 65536];
        let n = tokio::time::timeout(Duration::from_secs(5), s.read(&mut buf))
            .await
            .expect("server must answer, not hang")
            .unwrap_or(0);
        let text = String::from_utf8_lossy(&buf[..n]).to_string();
        assert!(text.starts_with("HTTP/1.1 413"), "{text}");
        drop(s);
        // process alive
        let (code, _) = get(port, "/v1/sessions", Some("t"), "127.0.0.1").await;
        assert_eq!(code, 200);
    }

    #[tokio::test]
    async fn slowloris_connection_is_dropped_listener_survives() {
        let port = start(state_with("t")).await;
        // open a socket and dribble nothing — the header timeout must kill it
        let mut slow = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        use tokio::io::AsyncWriteExt;
        slow.write_all(b"GET /v1/sessions HTTP/1.1\r\nHost: 127.0.0.1\r\nAutho")
            .await
            .unwrap();
        let started = std::time::Instant::now();
        // wait for the server to close its side: read returns 0
        let mut sink = Vec::new();
        use tokio::io::AsyncReadExt;
        let _ = tokio::time::timeout(Duration::from_secs(3), slow.read_to_end(&mut sink)).await;
        let waited = started.elapsed();
        assert!(
            waited < Duration::from_secs(3),
            "slowloris conn should be dropped by header timeout, took {waited:?}"
        );
        // listener alive and serving
        let (code, _) = get(port, "/v1/sessions", Some("t"), "127.0.0.1").await;
        assert_eq!(code, 200);
    }

    #[tokio::test]
    async fn malformed_request_does_not_abort_process() {
        let port = start(state_with("t")).await;
        // garbage line
        let mut g = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        g.write_all(b"NOT-HTTP AT ALL\r\n\r\n").await.unwrap();
        let mut buf = vec![0u8; 8192];
        let _ = tokio::time::timeout(Duration::from_secs(2), g.read(&mut buf)).await;
        drop(g);
        // binary garbage
        let mut g2 = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        g2.write_all(&[0u8, 159, 146, 150, 10, 13, 10, 13])
            .await
            .unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(2), g2.read(&mut buf)).await;
        drop(g2);
        // process alive
        let (code, _) = get(port, "/healthz", Some("t"), "127.0.0.1").await;
        assert_eq!(code, 200);
    }

    #[tokio::test]
    async fn unknown_route_404_bad_body_400() {
        let port = start(state_with("t")).await;
        let (code, _) = get(port, "/nope", Some("t"), "127.0.0.1").await;
        assert_eq!(code, 404);
        // prompt with empty body → 400
        let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        s.write_all(b"POST /v1/prompt HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer t\r\nContent-Length: 2\r\n\r\n{}")
            .await.unwrap();
        let mut buf = vec![0u8; 8192];
        let n = tokio::time::timeout(Duration::from_secs(3), s.read(&mut buf))
            .await
            .unwrap()
            .unwrap_or(0);
        let text = String::from_utf8_lossy(&buf[..n]).to_string();
        assert!(text.starts_with("HTTP/1.1 400"), "{text}");
    }
}
