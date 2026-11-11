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
//!   GET  /healthz                       liveness + version
//!   GET  /v1/events                     SSE bus feed (seq envelope,
//!                                       Last-Event-ID replay ring, ping)
//!   GET  /v1/sessions/:id/events        same, scoped to one session
//!   GET  /v1/tools                      real tool catalog (name/description/schema)
//!   GET  /v1/sessions                   SessionHost snapshot listing
//!   POST /v1/sessions                   create a hosted session
//!   GET  /v1/sessions/:id               one session snapshot
//!   DELETE /v1/sessions/:id             close a hosted session
//!   POST /v1/prompt                     run a REAL turn on a hosted session (SSE)
//!   POST /v1/sessions/:id/prompt        the same, session addressed in the path
//!   POST /v1/sessions/:id/steer         queue mid-turn steering (D3)
//!   POST /v1/sessions/:id/cancel        cooperative cancel of the in-flight turn
//!   POST /v1/sessions/:id/undo          drop the last completed turn
//!   POST /v1/sessions/:id/rewind        drop the last N completed turns
//!   POST /v1/sessions/:id/fork          copy the journal to a fresh session
//!   GET  /v1/approvals                  pending approval jobs (indexed)
//!   POST /v1/approvals/:id              resolve a pending approval for real
//!   GET  /v1/config                     redacted config view (never secrets)
//!   POST /v1/fs/read|list|write         workspace fs through the REAL tool host
//!   GET  /v1/git/status · POST /v1/git/diff|log
//!                                       read-only git through the real backend
//!   GET  /v1/mcp                        connected MCP servers + tool names
//!
//! fs/git/mcp behavior is INJECTED by the embedding surface (marfim-app)
//! as [`Handler`]s — the gateway never fabricates tool behavior; an
//! unwired handler answers 501 (honest, fail-closed).

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
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;

#[cfg(unix)]
use tokio::net::UnixListener;

/// A route behavior injected by the embedding surface (marfim-app):
/// request body → future of the JSON response or an error string. The
/// gateway owns transport, auth and envelope; the handler owns policy —
/// real handlers run through the permission engine (fail-closed).
pub type Handler =
    Arc<dyn Fn(Value) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send>> + Send + Sync>;

/// fs/git/mcp/terminal route behaviors. `None` = not wired by this
/// embedding → the route answers 501 instead of pretending.
#[derive(Clone, Default)]
pub struct RouteHandlers {
    pub fs_read: Option<Handler>,
    pub fs_list: Option<Handler>,
    pub fs_write: Option<Handler>,
    pub git_status: Option<Handler>,
    pub git_diff: Option<Handler>,
    pub git_log: Option<Handler>,
    pub mcp_list: Option<Handler>,
    /// W3.1 terminal routes: ONE handler owning every action — the body
    /// carries `action` (list|spawn|write|resize|screen|close) plus the
    /// per-action fields. Wired by the embedding through the REAL tool
    /// host (the Terminal tool), so a terminal can never bypass the
    /// permission engine.
    pub terminals: Option<Handler>,
}

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
    /// fs/git/mcp route behaviors (injected by the embedding surface).
    pub handlers: RouteHandlers,
    /// Redacted config view built by the embedding surface; the gateway
    /// never holds raw credentials, so it cannot leak them.
    pub config_view: Value,
    /// Sessions whose approval indexer is already running.
    indexed: Mutex<BTreeSet<String>>,
    /// Pending approvals across all hosted sessions.
    approvals: Arc<ApprovalIndex>,
    /// Per-session SSE event rings (seq + Last-Event-ID replay).
    rings: Mutex<BTreeMap<String, Arc<EventRing>>>,
}

impl ServeState {
    pub fn new(
        host: Arc<SessionHost>,
        token: String,
        tools: Arc<Vec<ToolInfo>>,
        allowed_hosts: Vec<String>,
        options: ServeOptions,
        handlers: RouteHandlers,
        config_view: Value,
    ) -> Arc<Self> {
        Arc::new(Self {
            host,
            token,
            started_at: std::time::Instant::now(),
            options,
            tools,
            allowed_hosts,
            handlers,
            config_view,
            indexed: Mutex::new(BTreeSet::new()),
            approvals: Arc::new(ApprovalIndex::default()),
            rings: Mutex::new(BTreeMap::new()),
        })
    }
}

/// W2.1 SSE envelope: one ring per hosted session. The pump task (see
/// `ensure_stream`) is the ONLY writer: it subscribes to the session bus,
/// assigns the session's monotonic sequence numbers, retains the last
/// `cap` entries for Last-Event-ID replay and broadcasts `(seq, line)` to
/// live subscribers. Replay and live delivery share one sequence space,
/// so a resuming client never sees a duplicate or a silent gap (a bus
/// lag becomes an explicit `ring_lagged` entry).
pub struct EventRing {
    next_seq: AtomicU64,
    entries: Mutex<VecDeque<(u64, String)>>,
    cap: usize,
    tx: tokio::sync::broadcast::Sender<(u64, String)>,
}

impl EventRing {
    fn new(cap: usize) -> Arc<Self> {
        let (tx, _) = tokio::sync::broadcast::channel(cap);
        Arc::new(Self {
            next_seq: AtomicU64::new(0),
            entries: Mutex::new(VecDeque::with_capacity(cap)),
            cap,
            tx,
        })
    }

    /// Assign the next seq, retain the line, notify live subscribers.
    fn record(&self, line: String) -> (u64, String) {
        let seq = self.next_seq.fetch_add(1, Ordering::SeqCst) + 1;
        {
            let mut q = self.entries.lock().unwrap_or_else(|p| p.into_inner());
            q.push_back((seq, line.clone()));
            while q.len() > self.cap {
                q.pop_front();
            }
        }
        let _ = self.tx.send((seq, line.clone()));
        (seq, line)
    }

    /// Every retained entry with seq > `last` (Last-Event-ID replay).
    fn replay_after(&self, last: u64) -> Vec<(u64, String)> {
        self.entries
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .filter(|(seq, _)| *seq > last)
            .cloned()
            .collect()
    }

    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<(u64, String)> {
        self.tx.subscribe()
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

    /// W2.3: bind a unix socket. A stale SOCKET at the path is removed;
    /// any non-socket file fails the bind (never deleted). The socket is
    /// chmod 0600 (owner-only) after bind — the bearer token stays
    /// mandatory on top, defense in depth.
    #[cfg(unix)]
    pub fn bind_unix(path: &str) -> std::io::Result<UnixListener> {
        use std::os::unix::fs::{FileTypeExt, PermissionsExt};
        match std::fs::metadata(path) {
            Ok(m) if m.file_type().is_socket() => {
                std::fs::remove_file(path)?;
            }
            Ok(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!("{path} exists and is not a socket; refusing to remove it"),
                ));
            }
            Err(_) => {}
        }
        let listener = UnixListener::bind(path)?;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        Ok(listener)
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

    /// W2.3: serve over a unix socket (same protocol, same token gate).
    #[cfg(unix)]
    pub async fn serve_unix(self, listener: UnixListener) -> std::io::Result<()> {
        loop {
            let (stream, _) = listener.accept().await?;
            let state = self.state.clone();
            tokio::spawn(async move {
                let _ = serve_connection(stream, state).await;
            });
        }
    }

    /// W2.3: serve TLS over TCP. A failed handshake kills only its own
    /// connection; the listener keeps serving (fail-visible in the
    /// connection task, never process-fatal).
    pub async fn serve_tls(
        self,
        listener: TcpListener,
        acceptor: tokio_rustls::TlsAcceptor,
    ) -> std::io::Result<()> {
        loop {
            let (stream, _) = listener.accept().await?;
            let state = self.state.clone();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                if let Ok(tls) = acceptor.accept(stream).await {
                    let _ = serve_connection(tls, state).await;
                }
                // handshake garbage / bad SNI / dead peer: contained
            });
        }
    }
}

/// W2.3: build a TLS acceptor from PEM cert chain + private key files
/// (fail-closed: any parse problem is a boot error, never a warning).
pub fn tls_acceptor_from_pem_files(
    cert_path: &str,
    key_path: &str,
) -> Result<tokio_rustls::TlsAcceptor, String> {
    use rustls::pki_types::pem::PemObject;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    // W2.3+: PEM parsing rides rustls-pki-types' PemObject directly --
    // rustls-pemfile is unmaintained (RUSTSEC-2025-0134) and this is its
    // documented migration. Fail-closed shape is unchanged: a missing or
    // unparseable file is a boot error, never a warning.
    let certs: Vec<CertificateDer> = CertificateDer::pem_file_iter(cert_path)
        .map_err(|e| format!("tls cert {cert_path}: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("tls cert parse {cert_path}: {e}"))?;
    if certs.is_empty() {
        return Err(format!("tls cert {cert_path}: no certificates found"));
    }
    let key: PrivateKeyDer = PrivateKeyDer::from_pem_file(key_path).map_err(|e| match e {
        rustls::pki_types::pem::Error::Io(io) => format!("tls key {key_path}: {io}"),
        rustls::pki_types::pem::Error::NoItemsFound => {
            format!("tls key {key_path}: no private key found")
        }
        other => format!("tls key parse {key_path}: {other}"),
    })?;
    let mut cfg = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
    .map_err(|e| format!("tls protocol versions: {e}"))?
    .with_no_client_auth()
    .with_single_cert(certs, key)
    .map_err(|e| format!("tls single cert: {e}"))?;
    cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(tokio_rustls::TlsAcceptor::from(Arc::new(cfg)))
}

async fn serve_connection<I>(io: I, state: Arc<ServeState>)
where
    I: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let svc = Svc {
        state: state.clone(),
    };
    let _ = auto::Builder::new(TokioExecutor::default())
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(state.options.header_timeout)
        .max_buf_size(state.options.max_header_buf)
        .serve_connection_with_upgrades(TokioIo::new(io), svc)
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

fn sse_data(seq: u64, line: &str) -> Frame<Bytes> {
    // single source of the frame bytes: the protocol contract module
    Frame::data(Bytes::from(crate::protocol::sse_data_frame(seq, line)))
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
    // W2.1 resume: SSE Last-Event-ID, header first, `?lastEventId=` as the
    // fallback for clients that cannot set headers on reconnect.
    let last_event_id = parts
        .headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .or_else(|| {
            parts.uri.query().and_then(|q| {
                q.split('&')
                    .find_map(|kv| kv.strip_prefix("lastEventId="))
                    .and_then(|v| v.parse::<u64>().ok())
            })
        })
        .unwrap_or(0);

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
        (Method::GET, r) if r.starts_with("/v1/sessions/") && !r.ends_with("/events") => {
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
            ensure_stream(&state, &hosted);
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
        (Method::GET, "/v1/events") => events_stream(state, None, last_event_id).await,
        (Method::POST, r) if r.starts_with("/v1/sessions/") && r.ends_with("/prompt") => {
            let id = r["/v1/sessions/".len()..r.len() - "/prompt".len()].to_string();
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
            match state.host.get(&id).await {
                Some(hosted) => {
                    ensure_stream(&state, &hosted);
                    prompt_stream(state, hosted, prompt).await
                }
                None => json_resp(
                    StatusCode::NOT_FOUND,
                    serde_json::json!({"error": "no such session"}),
                ),
            }
        }
        (Method::GET, r) if r.starts_with("/v1/sessions/") && r.ends_with("/events") => {
            let id = r["/v1/sessions/".len()..r.len() - "/events".len()].to_string();
            match state.host.get(&id).await {
                Some(hosted) => events_stream(state, Some(hosted), last_event_id).await,
                None => json_resp(
                    StatusCode::NOT_FOUND,
                    serde_json::json!({"error": "no such session"}),
                ),
            }
        }
        (Method::POST, r) if r.starts_with("/v1/sessions/") => {
            session_command_route(state, r, body_json).await
        }
        (Method::DELETE, r) if r.starts_with("/v1/sessions/") => {
            let id = r.trim_start_matches("/v1/sessions/").to_string();
            match state.host.close(&id).await {
                true => {
                    state
                        .rings
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .remove(&id);
                    json_resp(StatusCode::OK, serde_json::json!({"closed": id}))
                }
                false => json_resp(
                    StatusCode::NOT_FOUND,
                    serde_json::json!({"error": "no such session"}),
                ),
            }
        }
        (Method::POST, "/v1/sessions") => {
            let req = SessionRequest {
                resume: body_json
                    .as_ref()
                    .and_then(|v| v["resume"].as_str())
                    .map(str::to_string),
                title: body_json
                    .as_ref()
                    .and_then(|v| v["title"].as_str())
                    .map(str::to_string),
            };
            match state.host.create(req).await {
                Ok(hosted) => {
                    ensure_stream(&state, &hosted);
                    let snap = hosted.snapshot().await;
                    json_resp(
                        StatusCode::CREATED,
                        serde_json::to_value(&snap).unwrap_or_default(),
                    )
                }
                Err(e) => json_resp(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    serde_json::json!({"error": e}),
                ),
            }
        }
        (Method::GET, "/v1/config") => json_resp(StatusCode::OK, state.config_view.clone()),
        (Method::POST, "/v1/fs/read") => {
            inject_handler(
                state.handlers.fs_read.clone(),
                body_json.unwrap_or_default(),
            )
            .await
        }
        (Method::POST, "/v1/fs/list") => {
            inject_handler(
                state.handlers.fs_list.clone(),
                body_json.unwrap_or_default(),
            )
            .await
        }
        (Method::POST, "/v1/fs/write") => {
            inject_handler(
                state.handlers.fs_write.clone(),
                body_json.unwrap_or_default(),
            )
            .await
        }
        (Method::GET, "/v1/git/status") => {
            inject_handler(state.handlers.git_status.clone(), serde_json::json!({})).await
        }
        (Method::POST, "/v1/git/diff") => {
            inject_handler(
                state.handlers.git_diff.clone(),
                body_json.unwrap_or_default(),
            )
            .await
        }
        (Method::POST, "/v1/git/log") => {
            inject_handler(
                state.handlers.git_log.clone(),
                body_json.unwrap_or_default(),
            )
            .await
        }
        (Method::GET, "/v1/mcp") => {
            inject_handler(state.handlers.mcp_list.clone(), serde_json::json!({})).await
        }
        // -- W3.1: terminal surface (PTY sessions behind the Terminal tool)
        (Method::GET, "/v1/terminals") => {
            inject_handler(
                state.handlers.terminals.clone(),
                serde_json::json!({"action": "list"}),
            )
            .await
        }
        (Method::POST, "/v1/terminals") => {
            let mut body = body_json.unwrap_or_default();
            body["action"] = serde_json::json!("spawn");
            inject_handler(state.handlers.terminals.clone(), body).await
        }
        (Method::POST, r) if r.starts_with("/v1/terminals/") => {
            terminal_route(state.handlers.terminals.clone(), r, body_json).await
        }
        (Method::GET, r) if r.starts_with("/v1/terminals/") => {
            terminal_route(state.handlers.terminals.clone(), r, None).await
        }
        (Method::DELETE, r) if r.starts_with("/v1/terminals/") => {
            terminal_route(state.handlers.terminals.clone(), r, None).await
        }
        (Method::POST, _) | (Method::GET, _) | (Method::DELETE, _) => json_resp(
            StatusCode::NOT_FOUND,
            serde_json::json!({"error": "not found"}),
        ),
        _ => json_resp(
            StatusCode::METHOD_NOT_ALLOWED,
            serde_json::json!({"error": "method not allowed"}),
        ),
    }
}

/// Run one injected handler; `None` (unwired) answers 501 honestly.
async fn inject_handler(h: Option<Handler>, args: Value) -> Response<RespBody> {
    let Some(h) = h else {
        return json_resp(
            StatusCode::NOT_IMPLEMENTED,
            serde_json::json!({"error": "route not wired by the embedding surface"}),
        );
    };
    match h(args).await {
        Ok(v) => json_resp(StatusCode::OK, v),
        Err(e) => json_resp(StatusCode::BAD_REQUEST, serde_json::json!({"error": e})),
    }
}

/// `/v1/terminals/:id/{input|resize|screen}` (and bare
/// `/v1/terminals/:id` for DELETE=close). The route parses ONLY the
/// address; every policy decision stays in the injected handler (the
/// Terminal tool behind the permission engine).
async fn terminal_route(
    h: Option<Handler>,
    route: &str,
    body: Option<Value>,
) -> Response<RespBody> {
    let rest = route.trim_start_matches("/v1/terminals/");
    let mut args = body.unwrap_or_default();
    match rest.rsplit_once('/') {
        // /:id/input | /:id/resize | /:id/screen
        Some((id, action)) if matches!(action, "input" | "resize" | "screen") => {
            let canonical = match action {
                "input" => "write",
                other => other,
            };
            args["action"] = serde_json::json!(canonical);
            match id.parse::<u64>() {
                Ok(id) => args["id"] = serde_json::json!(id),
                Err(_) => {
                    return json_resp(
                        StatusCode::BAD_REQUEST,
                        serde_json::json!({"error": "terminal id must be an integer"}),
                    );
                }
            }
            inject_handler(h, args).await
        }
        // DELETE /v1/terminals/:id — the whole rest is the id.
        None => match rest.parse::<u64>() {
            Ok(id) => {
                args["action"] = serde_json::json!("close");
                args["id"] = serde_json::json!(id);
                inject_handler(h, args).await
            }
            Err(_) => json_resp(
                StatusCode::BAD_REQUEST,
                serde_json::json!({"error": "terminal id must be an integer"}),
            ),
        },
        _ => json_resp(
            StatusCode::NOT_FOUND,
            serde_json::json!({"error": "not found"}),
        ),
    }
}

/// Session-scoped command routes: /v1/sessions/:id/{steer,cancel,undo,
/// rewind,fork}. Every command flows through the actor's REAL command
/// channel — the HTTP surface never touches harness internals.
async fn session_command_route(
    state: Arc<ServeState>,
    route: &str,
    body: Option<Value>,
) -> Response<RespBody> {
    let rest = route.trim_start_matches("/v1/sessions/");
    let Some((id, action)) = rest.rsplit_once('/') else {
        return json_resp(
            StatusCode::NOT_FOUND,
            serde_json::json!({"error": "not found"}),
        );
    };
    let Some(hosted) = state.host.get(id).await else {
        return json_resp(
            StatusCode::NOT_FOUND,
            serde_json::json!({"error": "no such session"}),
        );
    };
    ensure_stream(&state, &hosted);
    match action {
        "steer" => {
            let Some(text) = body.as_ref().and_then(|v| v["text"].as_str()) else {
                return json_resp(
                    StatusCode::BAD_REQUEST,
                    serde_json::json!({"error": "missing `text`"}),
                );
            };
            if text.is_empty() {
                return json_resp(
                    StatusCode::BAD_REQUEST,
                    serde_json::json!({"error": "`text` must not be empty"}),
                );
            }
            let queued = hosted
                .actor
                .commands
                .send(SessionCommand::Steer {
                    text: text.to_string(),
                })
                .await
                .is_ok();
            if queued {
                json_resp(StatusCode::ACCEPTED, serde_json::json!({"queued": true}))
            } else {
                json_resp(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    serde_json::json!({"error": "session actor is gone"}),
                )
            }
        }
        "cancel" => {
            let (tx, rx) = tokio::sync::oneshot::channel();
            let sent = hosted
                .actor
                .commands
                .send(SessionCommand::Cancel { reply: tx })
                .await
                .is_ok();
            if !sent {
                return json_resp(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    serde_json::json!({"error": "session actor is gone"}),
                );
            }
            let _ = tokio::time::timeout(Duration::from_secs(10), rx).await;
            json_resp(StatusCode::OK, serde_json::json!({"cancelled": true}))
        }
        "undo" => {
            let (tx, rx) = tokio::sync::oneshot::channel();
            let sent = hosted
                .actor
                .commands
                .send(SessionCommand::Undo { reply: tx })
                .await
                .is_ok();
            if !sent {
                return json_resp(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    serde_json::json!({"error": "session actor is gone"}),
                );
            }
            match tokio::time::timeout(Duration::from_secs(10), rx).await {
                Ok(Ok(Ok(outcome))) => json_resp(
                    StatusCode::OK,
                    serde_json::to_value(&outcome).unwrap_or_default(),
                ),
                _ => json_resp(
                    StatusCode::CONFLICT,
                    serde_json::json!({"error": "nothing to undo"}),
                ),
            }
        }
        "rewind" => {
            let Some(turns) = body.as_ref().and_then(|v| v["turns"].as_u64()) else {
                return json_resp(
                    StatusCode::BAD_REQUEST,
                    serde_json::json!({"error": "missing `turns`"}),
                );
            };
            if turns == 0 {
                return json_resp(
                    StatusCode::BAD_REQUEST,
                    serde_json::json!({"error": "`turns` must be >= 1"}),
                );
            }
            let (tx, rx) = tokio::sync::oneshot::channel();
            let sent = hosted
                .actor
                .commands
                .send(SessionCommand::Rewind { turns, reply: tx })
                .await
                .is_ok();
            if !sent {
                return json_resp(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    serde_json::json!({"error": "session actor is gone"}),
                );
            }
            match tokio::time::timeout(Duration::from_secs(10), rx).await {
                Ok(Ok(Ok(outcome))) => json_resp(
                    StatusCode::OK,
                    serde_json::to_value(&outcome).unwrap_or_default(),
                ),
                _ => json_resp(
                    StatusCode::CONFLICT,
                    serde_json::json!({"error": "rewind refused"}),
                ),
            }
        }
        "fork" => {
            let (tx, rx) = tokio::sync::oneshot::channel();
            let sent = hosted
                .actor
                .commands
                .send(SessionCommand::Fork { reply: tx })
                .await
                .is_ok();
            if !sent {
                return json_resp(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    serde_json::json!({"error": "session actor is gone"}),
                );
            }
            match tokio::time::timeout(Duration::from_secs(10), rx).await {
                Ok(Ok(Ok(outcome))) => json_resp(
                    StatusCode::CREATED,
                    serde_json::to_value(&outcome).unwrap_or_default(),
                ),
                _ => json_resp(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    serde_json::json!({"error": "fork failed"}),
                ),
            }
        }
        _ => json_resp(
            StatusCode::NOT_FOUND,
            serde_json::json!({"error": "unknown session action"}),
        ),
    }
}

/// Start (once per session) the approval indexer AND the SSE ring pump:
/// the indexer drains the hosted session's approval queue into the shared
/// index so /v1/approvals is backed by REAL pending jobs; the pump feeds
/// the session's EventRing (seq assignment + Last-Event-ID replay). Both
/// subscribe to the bus independently — neither can starve the other.
fn ensure_stream(state: &Arc<ServeState>, hosted: &Arc<HostedSession>) {
    let sid = hosted.actor.session_id.clone();
    let fresh = {
        let mut guard = state.indexed.lock().unwrap_or_else(|p| p.into_inner());
        guard.insert(sid)
    };
    if !fresh {
        return;
    }
    // ring: registered before the pump starts so no subscriber can miss it
    let ring = {
        let mut rings = state.rings.lock().unwrap_or_else(|p| p.into_inner());
        rings
            .entry(hosted.actor.session_id.clone())
            .or_insert_with(|| EventRing::new(4096))
            .clone()
    };
    let mut events = hosted.bus.subscribe();
    tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(ev) => {
                    ring.record(ev.to_json_line());
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    // honest gap marker: seq stays contiguous, the client
                    // sees exactly what it missed
                    ring.record(format!("{{\"event\":\"ring_lagged\",\"dropped\":{n}}}"));
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
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
/// loss), forwards every bus event as an `id:`-stamped data frame (the
/// W2.1 envelope; per-stream counter — resume semantics live on the
/// /v1/events ring), emits one terminal frame carrying the actor's real
/// StopReason, then ends the stream.
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
        (rx, Some(reply), deadline, ping, false, 0u64),
        |(mut rx, mut reply, deadline, ping, done, mut seq)| async move {
            if done {
                return None;
            }
            if tokio::time::Instant::now() >= deadline {
                seq += 1;
                let f = sse_data(seq, "{\"event\":\"stream_deadline\"}");
                return Some((Ok(f), (rx, reply, deadline, ping, true, seq)));
            }
            match tokio::time::timeout(ping, rx.recv()).await {
                Err(_) => Some((
                    Ok(sse_text(": ping")),
                    (rx, reply, deadline, ping, false, seq),
                )),
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(n))) => {
                    seq += 1;
                    let f = sse_data(seq, &format!("{{\"event\":\"lagged\",\"dropped\":{n}}}"));
                    Some((Ok(f), (rx, reply, deadline, ping, false, seq)))
                }
                Ok(Err(_)) => {
                    // bus closed: actor gone
                    seq += 1;
                    let f = sse_data(seq, "{\"event\":\"turn_end\",\"stop_reason\":\"error\"}");
                    Some((Ok(f), (rx, reply, deadline, ping, true, seq)))
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
                        seq += 1;
                        let f = sse_data(
                            seq,
                            &format!("{{\"event\":\"turn_end\",\"stop_reason\":\"{stop}\"}}"),
                        );
                        Some((Ok(f), (rx, reply, deadline, ping, true, seq)))
                    }
                    other => {
                        seq += 1;
                        let f = sse_data(seq, &other.to_json_line());
                        Some((Ok(f), (rx, reply, deadline, ping, false, seq)))
                    }
                },
            }
        },
    );
    sse_resp(stream)
}

/// SSE subscription with the W2.1 envelope (pings + hard lifetime +
/// `id: <seq>` on every data frame + Last-Event-ID replay from the
/// session's ring). The default hosted session is used when none is
/// named; with no session at all the stream is pings only.
async fn events_stream(
    state: Arc<ServeState>,
    hosted: Option<Arc<HostedSession>>,
    last_event_id: u64,
) -> Response<RespBody> {
    let ping = state.options.sse_ping;
    let deadline = tokio::time::Instant::now() + state.options.stream_lifetime;
    let Some(hosted) = hosted else {
        let stream = futures_util::stream::unfold(
            (deadline, ping, false),
            move |(deadline, ping, done)| async move {
                if done {
                    return None;
                }
                if tokio::time::Instant::now() >= deadline {
                    let f = sse_text("{\"event\":\"stream_deadline\"}");
                    return Some((Ok(f), (deadline, ping, true)));
                }
                tokio::time::sleep(ping).await;
                Some((Ok(sse_text(": ping")), (deadline, ping, false)))
            },
        );
        return sse_resp(stream);
    };
    ensure_stream(&state, &hosted);
    let ring = {
        let mut rings = state.rings.lock().unwrap_or_else(|p| p.into_inner());
        rings
            .entry(hosted.actor.session_id.clone())
            .or_insert_with(|| EventRing::new(4096))
            .clone()
    };
    let replay: VecDeque<(u64, String)> = ring.replay_after(last_event_id).into();
    let last_seen = last_event_id;
    let live = ring.subscribe();
    let stream = futures_util::stream::unfold(
        (Some(replay), live, last_seen, deadline, ping),
        move |(mut replay, mut live, mut last_seen, deadline, ping)| async move {
            loop {
                if tokio::time::Instant::now() >= deadline {
                    let f = sse_text("{\"event\":\"stream_deadline\"}");
                    return Some((Ok(f), (None, live, last_seen, deadline, ping)));
                }
                // 1. replay queue (Last-Event-ID resume)
                if let Some(q) = replay.as_mut() {
                    match q.pop_front() {
                        Some((seq, line)) => {
                            last_seen = seq;
                            let f = sse_data(seq, &line);
                            return Some((Ok(f), (replay, live, last_seen, deadline, ping)));
                        }
                        None => {
                            replay = None; // drained: go live
                            continue;
                        }
                    }
                }
                // 2. live ring deliveries
                match tokio::time::timeout(ping, live.recv()).await {
                    Err(_) => {
                        return Some((
                            Ok(sse_text(": ping")),
                            (None, live, last_seen, deadline, ping),
                        ));
                    }
                    Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(n))) => {
                        let f = sse_text(&format!("{{\"event\":\"lagged\",\"dropped\":{n}}}"));
                        return Some((Ok(f), (None, live, last_seen, deadline, ping)));
                    }
                    Ok(Err(_)) => {
                        let f = sse_text("{\"event\":\"stream_deadline\"}");
                        return Some((Ok(f), (None, live, last_seen, deadline, ping)));
                    }
                    Ok(Ok((seq, line))) => {
                        if seq <= last_seen {
                            // already delivered by the replay phase
                            continue;
                        }
                        last_seen = seq;
                        let f = sse_data(seq, &line);
                        return Some((Ok(f), (None, live, last_seen, deadline, ping)));
                    }
                }
            }
        },
    );
    sse_resp(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpStream;

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
            RouteHandlers::default(),
            serde_json::json!({"provider": "test"}),
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

    // ---- W2.1: protocol routes + SSE envelope ----

    #[test]
    fn event_ring_seqs_replay_and_cap() {
        let ring = EventRing::new(3);
        for i in 0..5 {
            ring.record(format!("e{i}"));
        }
        let all = ring.replay_after(0);
        assert_eq!(all.len(), 3, "cap bounds the replay window");
        assert_eq!(all[0].0, 3, "oldest entries are evicted, seqs stay global");
        assert_eq!(all[0].1, "e2");
        assert_eq!(all[2].1, "e4");
        let after = ring.replay_after(3);
        assert_eq!(after.len(), 2);
        assert_eq!(after[0].0, 4);
        assert_eq!(after[1].0, 5);
    }

    /// A real minimal harness per session: no providers needed (no turn
    /// runs in these tests — only actor commands and bus events).
    fn harness_deps(tag: &str) -> Result<marfim_core::session_host::SessionDeps, String> {
        use marfim_core::journal::Journal;
        use marfim_core::loop_::{CancelToken, Harness, LoopCtx};
        use marfim_core::permissions::{Mode, PermissionEngine};
        use marfim_core::providers::CostBook;
        use marfim_core::router::Router;
        use marfim_core::sandbox::Sandbox;
        use marfim_tools::registry::ToolCtx;
        use marfim_tools::tools_host::HostTools;
        let dir = marfim_testkit::unique_dir(tag);
        let (perm, approval_rx) = PermissionEngine::new(Mode::Default, vec![]);
        let perm = Arc::new(perm);
        let bus = EventBus::new(4096);
        let journal = Journal::open(&dir.join("journal.jsonl")).map_err(|e| e.to_string())?;
        let sid = format!("{tag}-{}", std::process::id());
        let ctx = LoopCtx {
            session_id: sid.clone(),
            cwd: dir.display().to_string(),
            cancel: CancelToken::default(),
        };
        let host = HostTools::new(
            perm.clone(),
            ToolCtx::default(),
            Sandbox::probe(),
            marfim_core::hooks::Hooks::default(),
        );
        let harness = Harness::new(
            bus.clone(),
            Arc::new(tokio::sync::RwLock::new(Router::new(
                CostBook::default(),
                false,
            ))),
            Arc::new(host),
            Arc::new(std::sync::Mutex::new(journal)),
            ctx,
            perm,
            64,
            3,
            120_000,
            false,
        );
        Ok(marfim_core::session_host::SessionDeps {
            harness: Arc::new(harness),
            bus: Arc::new(bus),
            approval_rx,
        })
    }

    fn state_with_sessions(token: &str, tag: &str) -> Arc<ServeState> {
        let tag = tag.to_string();
        let host = Arc::new(SessionHost::new(Arc::new(move |_req| {
            let tag = tag.clone();
            Box::pin(async move { harness_deps(&tag) })
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
            RouteHandlers::default(),
            serde_json::json!({"provider": "test"}),
        )
    }

    async fn post_json(port: u16, path: &str, body: &Value, token: &str) -> (u16, String) {
        let text = body.to_string();
        let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let req = format!(
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
            text.len()
        );
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        s.write_all(req.as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(8), s.read_to_end(&mut buf)).await;
        let resp = String::from_utf8_lossy(&buf).to_string();
        let status: u16 = resp
            .split_whitespace()
            .nth(1)
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        (status, resp)
    }

    async fn delete(port: u16, path: &str, token: &str) -> u16 {
        let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let req = format!(
            "DELETE {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n"
        );
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        s.write_all(req.as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut buf)).await;
        String::from_utf8_lossy(&buf)
            .split_whitespace()
            .nth(1)
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    }

    fn ring_len(state: &Arc<ServeState>, sid: &str) -> usize {
        state
            .rings
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(sid)
            .map(|r| r.replay_after(0).len())
            .unwrap_or(0)
    }

    #[tokio::test]
    async fn session_lifecycle_and_last_event_id_replay() {
        let state = state_with_sessions("tok", "ringtest");
        let port = start(state.clone()).await;
        let (code, body) = post_json(
            port,
            "/v1/sessions",
            &serde_json::json!({"title": "t"}),
            "tok",
        )
        .await;
        assert_eq!(code, 201, "{body}");
        let sid = serde_json::from_str::<Value>(body.rsplit_once("\r\n\r\n").unwrap().1.trim())
            .unwrap()["session_id"]
            .as_str()
            .unwrap()
            .to_string();
        // publish real events on the hosted bus; the ring pump records them
        let hosted = state.host.get(&sid).await.unwrap();
        hosted.bus.publish(Event::SessionStarted {
            session_id: sid.clone(),
            cwd: ".".into(),
        });
        hosted
            .bus
            .publish(Event::AssistantMessage { text: "one".into() });
        hosted
            .bus
            .publish(Event::AssistantMessage { text: "two".into() });
        for _ in 0..100 {
            if ring_len(&state, &sid) >= 3 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(ring_len(&state, &sid), 3, "ring pump must record all three");
        // full subscription: envelope carries id: 1..3
        let (code, body) = get(
            port,
            &format!("/v1/sessions/{sid}/events"),
            Some("tok"),
            "127.0.0.1",
        )
        .await;
        assert_eq!(code, 200);
        assert!(body.contains("id: 1"), "{body}");
        assert!(body.contains("id: 3"), "{body}");
        assert!(body.contains("\"text\":\"two\""), "{body}");
        // resume after id 2: only the third replays
        let (code, body) = get(
            port,
            &format!("/v1/sessions/{sid}/events?lastEventId=2"),
            Some("tok"),
            "127.0.0.1",
        )
        .await;
        assert_eq!(code, 200);
        assert!(body.contains("id: 3"), "{body}");
        assert!(!body.contains("id: 1"), "no duplicate replay: {body}");
        assert!(!body.contains("id: 2"), "no duplicate replay: {body}");
        // Last-Event-ID header form works too
        let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let req = format!(
            "GET /v1/sessions/{sid}/events HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer tok\r\nLast-Event-ID: 3\r\nConnection: close\r\n\r\n"
        );
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        s.write_all(req.as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut buf)).await;
        let resp = String::from_utf8_lossy(&buf).to_string();
        assert!(resp.contains("HTTP/1.1 200"), "{resp}");
        assert!(
            !resp.contains("id: 1") && !resp.contains("id: 3"),
            "nothing left to replay after 3: {resp}"
        );
    }

    #[tokio::test]
    async fn session_command_routes() {
        let state = state_with_sessions("tok2", "cmdtest");
        let port = start(state.clone()).await;
        // unknown session → 404 for every command route
        let (code, _) = post_json(
            port,
            "/v1/sessions/nope/steer",
            &serde_json::json!({"text": "x"}),
            "tok2",
        )
        .await;
        assert_eq!(code, 404);
        let code = delete(port, "/v1/sessions/nope", "tok2").await;
        assert_eq!(code, 404);
        // create a REAL session
        let (code, body) = post_json(port, "/v1/sessions", &serde_json::json!({}), "tok2").await;
        assert_eq!(code, 201, "{body}");
        let sid = serde_json::from_str::<Value>(body.rsplit_once("\r\n\r\n").unwrap().1.trim())
            .unwrap()["session_id"]
            .as_str()
            .unwrap()
            .to_string();
        // steer → 202 queued; empty text → 400
        let (code, body) = post_json(
            port,
            &format!("/v1/sessions/{sid}/steer"),
            &serde_json::json!({"text": "go"}),
            "tok2",
        )
        .await;
        assert_eq!(code, 202, "{body}");
        let (code, _) = post_json(
            port,
            &format!("/v1/sessions/{sid}/steer"),
            &serde_json::json!({"text": ""}),
            "tok2",
        )
        .await;
        assert_eq!(code, 400);
        // cancel on an idle session → 200
        let (code, body) = post_json(
            port,
            &format!("/v1/sessions/{sid}/cancel"),
            &serde_json::json!({}),
            "tok2",
        )
        .await;
        assert_eq!(code, 200, "{body}");
        // fork → 201 with a new session id
        let (code, body) = post_json(
            port,
            &format!("/v1/sessions/{sid}/fork"),
            &serde_json::json!({}),
            "tok2",
        )
        .await;
        assert_eq!(code, 201, "{body}");
        // rewind with a bad body → 400
        let (code, _) = post_json(
            port,
            &format!("/v1/sessions/{sid}/rewind"),
            &serde_json::json!({}),
            "tok2",
        )
        .await;
        assert_eq!(code, 400);
        // DELETE closes the session
        let code = delete(port, &format!("/v1/sessions/{sid}"), "tok2").await;
        assert_eq!(code, 200);
        let (code, _) = get(port, "/v1/sessions", Some("tok2"), "127.0.0.1").await;
        assert_eq!(code, 200);
    }

    #[tokio::test]
    async fn fs_git_mcp_routes_use_injected_handlers() {
        // unwired handlers → honest 501
        let port = start(state_with("t")).await;
        let (code, _) =
            post_json(port, "/v1/fs/read", &serde_json::json!({"path": "a"}), "t").await;
        assert_eq!(code, 501);
        let (code, _) = get(port, "/v1/git/status", Some("t"), "127.0.0.1").await;
        assert_eq!(code, 501);
        // wired handlers: ok → 200, handler error → 400
        let host = Arc::new(SessionHost::new(Arc::new(|_req| {
            Box::pin(async { Err("no factory".to_string()) })
        })));
        let handlers = RouteHandlers {
            fs_read: Some(Arc::new(|args| {
                Box::pin(async move {
                    match args["path"].as_str() {
                        Some("ok") => Ok(serde_json::json!({"content": "hello"})),
                        _ => Err("no such file".to_string()),
                    }
                })
            })),
            mcp_list: Some(Arc::new(|_args| {
                Box::pin(async { Ok(serde_json::json!({"servers": [], "tools": []})) })
            })),
            ..Default::default()
        };
        let state = ServeState::new(
            host,
            "t2".to_string(),
            Arc::new(vec![]),
            vec!["127.0.0.1".into(), "localhost".into()],
            ServeOptions::default(),
            handlers,
            serde_json::json!({"provider": "p", "model": "m"}),
        );
        let port = start(state).await;
        let (code, body) = post_json(
            port,
            "/v1/fs/read",
            &serde_json::json!({"path": "ok"}),
            "t2",
        )
        .await;
        assert_eq!(code, 200, "{body}");
        assert!(body.contains("hello"), "{body}");
        let (code, _) = post_json(
            port,
            "/v1/fs/read",
            &serde_json::json!({"path": "bad"}),
            "t2",
        )
        .await;
        assert_eq!(code, 400);
        let (code, body) = get(port, "/v1/mcp", Some("t2"), "127.0.0.1").await;
        assert_eq!(code, 200);
        assert!(body.contains("servers"), "{body}");
        // config view is served verbatim (redaction happened at the app layer)
        let (code, body) = get(port, "/v1/config", Some("t2"), "127.0.0.1").await;
        assert_eq!(code, 200);
        assert!(
            body.contains("provider") && body.contains("model"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn terminal_routes_auth_unwired_and_shape() {
        // Unwired embedding → 501 on every terminal route (never a fake
        // success), auth still enforced first.
        let port = start(state_with("tk")).await;
        let (code, _) = get(port, "/v1/terminals", Some("tk"), "127.0.0.1").await;
        assert_eq!(code, 501);
        let (code, _) = get(port, "/v1/terminals", None, "127.0.0.1").await;
        assert_eq!(code, 401, "token is mandatory on terminal routes too");
        let (code, _) = post_json(
            port,
            "/v1/terminals",
            &serde_json::json!({"command": "sh"}),
            "tk",
        )
        .await;
        assert_eq!(code, 501);
        let code = delete(port, "/v1/terminals/1", "tk").await;
        assert_eq!(code, 501);
        let (code, _) = get(port, "/v1/terminals/1/screen", Some("tk"), "127.0.0.1").await;
        assert_eq!(code, 501);
    }

    #[tokio::test]
    async fn terminal_routes_parse_addresses_and_forward() {
        // A fake handler records the action/id the route parsed, so the
        // address grammar is pinned without spawning a real PTY here.
        type Seen = Arc<Mutex<Vec<(String, Option<u64>)>>>;
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let rec = seen.clone();
        let host = Arc::new(SessionHost::new(Arc::new(|_req| {
            Box::pin(async { Err("no factory".to_string()) })
        })));
        let handlers = RouteHandlers {
            terminals: Some(Arc::new(move |args| {
                let rec = rec.clone();
                Box::pin(async move {
                    let action = args["action"].as_str().unwrap_or("").to_string();
                    let id = args["id"].as_u64();
                    rec.lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .push((action.clone(), id));
                    Ok(serde_json::json!({"ok": true, "action": action}))
                })
            })),
            ..Default::default()
        };
        let state = ServeState::new(
            host,
            "tk2".to_string(),
            Arc::new(vec![]),
            vec!["127.0.0.1".into(), "localhost".into()],
            ServeOptions::default(),
            handlers,
            serde_json::json!({"provider": "p"}),
        );
        let port = start(state).await;
        let (code, _) = get(port, "/v1/terminals", Some("tk2"), "127.0.0.1").await;
        assert_eq!(code, 200, "list route");
        let (code, _) = post_json(
            port,
            "/v1/terminals",
            &serde_json::json!({"command": "sh", "args": ["-c", "ls"]}),
            "tk2",
        )
        .await;
        assert_eq!(code, 200, "spawn route");
        let (code, _) = post_json(
            port,
            "/v1/terminals/3/input",
            &serde_json::json!({"text": "q"}),
            "tk2",
        )
        .await;
        assert_eq!(code, 200, "input route");
        let (code, _) = post_json(
            port,
            "/v1/terminals/3/resize",
            &serde_json::json!({"rows": 30, "cols": 100}),
            "tk2",
        )
        .await;
        assert_eq!(code, 200, "resize route");
        let (code, _) = get(port, "/v1/terminals/3/screen", Some("tk2"), "127.0.0.1").await;
        assert_eq!(code, 200, "screen route");
        let code = delete(port, "/v1/terminals/3", "tk2").await;
        assert_eq!(code, 200, "close route");
        // non-integer id → 400 before any handler runs
        let (code, _) = get(port, "/v1/terminals/nope/screen", Some("tk2"), "127.0.0.1").await;
        assert_eq!(code, 400);
        let (code, _) = get(port, "/v1/terminals/1/nope", Some("tk2"), "127.0.0.1").await;
        assert_eq!(code, 404, "unknown terminal action is not found");
        let actions: Vec<(String, Option<u64>)> =
            seen.lock().unwrap_or_else(|p| p.into_inner()).clone();
        assert_eq!(
            actions,
            vec![
                ("list".to_string(), None),
                ("spawn".to_string(), None),
                ("write".to_string(), Some(3)),
                ("resize".to_string(), Some(3)),
                ("screen".to_string(), Some(3)),
                ("close".to_string(), Some(3)),
            ],
            "route grammar: action + parsed id reach the handler"
        );
    }
}
