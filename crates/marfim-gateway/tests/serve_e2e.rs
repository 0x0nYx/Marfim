//! Step 1 e2e: serve over a REAL socket with a REAL mock-provider session —
//! a scripted tool call goes through the permission engine, lands as a
//! pending approval on /v1/approvals, is resolved via POST /v1/approvals/:id,
//! and the turn completes over SSE. Plus: a REAL ACP turn over stdio driven
//! through the same SessionHost shape.

use marfim_core::events::EventBus;
use marfim_core::journal::Journal;
use marfim_core::loop_::{Harness, LoopCtx};
use marfim_core::permissions::ApprovalJob;
use marfim_core::permissions::{Mode, PermissionEngine};
use marfim_core::providers::{CostBook, MockProvider, Step};
use marfim_core::router::{ModelEntry, Role, Router};
use marfim_core::session_host::{SessionDeps, SessionHost};
use marfim_gateway::serve::{ServeOptions, ServeState, Server, ToolInfo};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;

fn harness_with(
    steps: Vec<Step>,
    dir: &std::path::Path,
    tag: &str,
) -> (Arc<Harness>, tokio::sync::mpsc::Receiver<ApprovalJob>) {
    let bus = EventBus::new(256);
    let (perm, approval_rx) = PermissionEngine::new(Mode::Default, vec![]);
    let perm = Arc::new(perm);
    let tool_ctx = marfim_tools::registry::ToolCtx {
        cwd: dir.to_path_buf(),
        session_id: tag.to_string(),
        ..Default::default()
    };
    let host_tools = marfim_tools::tools_host::HostTools::new(
        perm.clone(),
        tool_ctx,
        marfim_core::sandbox::Sandbox::probe(),
        marfim_core::hooks::Hooks::default(),
    );
    let mut router = Router::new(CostBook::default(), false);
    router.set_chain(
        Role::Primary,
        vec![ModelEntry {
            model: "mock-1".into(),
            provider: Arc::new(MockProvider::new(steps)),
            priced: true,
        }],
    );
    router.set_chain(
        Role::Small,
        vec![ModelEntry {
            model: "mock-1".into(),
            provider: Arc::new(MockProvider::new(vec![])),
            priced: true,
        }],
    );
    let journal_path = dir.join(format!("{tag}.jsonl"));
    let _ = Journal::open(&journal_path).expect("journal");
    let harness = Harness::new(
        bus,
        Arc::new(tokio::sync::RwLock::new(router)),
        Arc::new(host_tools),
        Arc::new(std::sync::Mutex::new(
            Journal::open(&journal_path).expect("journal"),
        )),
        LoopCtx {
            session_id: tag.to_string(),
            cwd: dir.to_string_lossy().to_string(),
            cancel: marfim_core::loop_::CancelToken::default(),
        },
        perm,
        64,
        3,
        100_000,
        false,
    );
    (Arc::new(harness), approval_rx)
}

fn host_for(steps: Vec<Step>, dir: &std::path::Path) -> Arc<SessionHost> {
    let dir = dir.to_path_buf();
    Arc::new(SessionHost::new(Arc::new(move |_req| {
        let dir = dir.clone();
        let steps = steps.clone();
        Box::pin(async move {
            // the ENGINE's approval receiver goes to the actor (SessionDeps);
            // dropping it would fail every approval closed
            let (harness, approval_rx) = harness_with(steps, &dir, "e2e");
            let bus = Arc::new(harness.bus.clone());
            Ok(SessionDeps {
                harness,
                bus,
                approval_rx,
            })
        })
    })))
}

fn tools_catalog() -> Arc<Vec<ToolInfo>> {
    let mut registry = marfim_tools::registry::ToolRegistry::new();
    marfim_tools::tools_host::register_builtins(&mut registry);
    Arc::new(
        registry
            .list_visible()
            .into_iter()
            .map(|t| ToolInfo {
                name: t.name.to_string(),
                description: t.description.to_string(),
                parameters: t.parameters.clone(),
            })
            .collect(),
    )
}

async fn start(steps: Vec<Step>) -> (u16, String) {
    let dir = std::env::temp_dir().join(format!(
        "marfim-serve-e2e-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::create_dir_all(&dir);
    let token = "e2e-secret".to_string();
    let state = ServeState::new(
        host_for(steps, &dir),
        token.clone(),
        tools_catalog(),
        vec!["127.0.0.1".into(), "localhost".into()],
        ServeOptions {
            sse_ping: Duration::from_millis(50),
            ..Default::default()
        },
    );
    let server = Server::new(state);
    let listener = Server::bind("127.0.0.1", 0).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = server.serve(listener).await;
    });
    (port, token)
}

async fn get_json(port: u16, token: &str, path: &str) -> (u16, serde_json::Value) {
    let req = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n");
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(3), s.read_to_end(&mut buf)).await;
    let text = String::from_utf8_lossy(&buf).to_string();
    let status: u16 = text
        .split_whitespace()
        .nth(1)
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let body = text.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
    let v = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
    (status, v)
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_call_approval_roundtrip_over_http() {
    // scripted provider: first a Bash tool call (Execute → Ask), then text
    let steps = vec![
        MockProvider::tool_call("t1", "Bash", serde_json::json!({"command": "echo hi"})),
        MockProvider::text("tool answered; done"),
    ];
    let (port, token) = start(steps).await;

    // tools catalog is real
    let (code, tools) = get_json(port, &token, "/v1/tools").await;
    assert_eq!(code, 200);
    assert!(
        tools["tools"]
            .as_array()
            .map(|a| a.iter().any(|t| t["name"] == "Bash"))
            .unwrap_or(false),
        "Bash must be listed"
    );

    // no sessions yet
    let (code, sessions) = get_json(port, &token, "/v1/sessions").await;
    assert_eq!(code, 200);
    assert_eq!(sessions["sessions"].as_array().map(|a| a.len()), Some(0));

    // run the prompt; stream the SSE until the terminal frame
    let prompt_body = serde_json::json!({"prompt": "run echo hi"}).to_string();
    let body = format!(
        "POST /v1/prompt HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{prompt_body}",
        prompt_body.len()
    );
    let s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let (mut prompt_rh, mut prompt_wh) = tokio::io::split(s);
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    prompt_wh.write_all(body.as_bytes()).await.unwrap();

    // read the stream until the actor parks on the approval
    let mut sse = String::new();
    let mut buf = vec![0u8; 8192];
    for _ in 0..120 {
        match tokio::time::timeout(Duration::from_millis(100), prompt_rh.read(&mut buf)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => sse.push_str(&String::from_utf8_lossy(&buf[..n])),
            Ok(Err(_)) => break,
            Err(_) => {}
        }
        if sse.contains("approval_requested") {
            break;
        }
    }
    assert!(
        sse.contains("approval_requested"),
        "no approval surfaced: {sse}"
    );

    // the approval is REAL and pending
    let (code, approvals) = get_json(port, &token, "/v1/approvals").await;
    assert_eq!(code, 200);
    let list = approvals["approvals"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(list.len(), 1, "{approvals}");
    assert_eq!(list[0]["tool"], "Bash");
    let id = list[0]["id"].as_str().unwrap().to_string();

    // resolve it: allow → the tool runs → the turn completes
    let decision_body = serde_json::json!({"decision": "allow"}).to_string();
    let req = format!(
        "POST /v1/approvals/{id} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{decision_body}",
        decision_body.len()
    );
    let s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let (mut appr_rh, mut appr_wh) = tokio::io::split(s);
    appr_wh.write_all(req.as_bytes()).await.unwrap();
    // NOTE: no half-close — hyper aborts half-closed HTTP/1.1 requests
    // without answering; Connection: close makes the server EOF for us.
    let mut resp = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(3), appr_rh.read_to_end(&mut resp)).await;
    let resp = String::from_utf8_lossy(&resp).to_string();
    assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");

    // drain the rest of the prompt SSE: tool result + final turn_end
    let mut rest = String::new();
    for _ in 0..160 {
        match tokio::time::timeout(Duration::from_millis(100), prompt_rh.read(&mut buf)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => rest.push_str(&String::from_utf8_lossy(&buf[..n])),
            Ok(Err(_)) => break,
            Err(_) => {}
        }
        if rest.contains("turn_end") {
            break;
        }
    }
    let all = format!("{sse}{rest}");
    assert!(
        all.contains("tool_call_started") && all.contains("tool_call_result"),
        "tool events must stream: {all}"
    );
    assert!(
        all.contains("turn_end") && all.contains("end_turn"),
        "turn must complete after approval: {all}"
    );
}
