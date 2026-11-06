//! SessionHost: the single owner of SessionActors for every surface —
//! tui, headless, serve, acp. Actor lifecycle (spawn, registry, snapshot
//! listing, teardown) lives here; construction of the underlying harness
//! is injected, because only the binary crate can assemble providers,
//! tools and config. Core keeps no dependency on marfim-tools.
//!
//! Every session is created through [`SessionHost::create`]; every
//! observation (snapshot, listing) and every teardown (close, close_all)
//! flows through the host, so surfaces can never fork actor state.

use crate::events::EventBus;
use crate::loop_::Harness;
use crate::permissions::ApprovalJob;
use crate::session::{spawn_session, SessionCommand, SessionHandle, SessionSnapshot};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Everything the factory must deliver for one actor: the owned harness,
/// the session's event bus and the raw approval receiver (the host wires
/// it into the actor).
pub struct SessionDeps {
    pub harness: Arc<Harness>,
    pub bus: Arc<EventBus>,
    pub approval_rx: tokio::sync::mpsc::Receiver<ApprovalJob>,
}

/// What the surface asks for. `resume` names (or, empty, picks the
/// latest) journal; `title` names a fresh session.
#[derive(Debug, Clone, Default)]
pub struct SessionRequest {
    pub resume: Option<String>,
    pub title: Option<String>,
}

/// Injected construction: build the full harness for one session.
pub type SessionFactory = Arc<
    dyn Fn(
            SessionRequest,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<SessionDeps, String>> + Send>,
        > + Send
        + Sync,
>;

/// One live session under the host: the actor handle, the bus the actor
/// publishes to, and the approval receiver (taken out of the handle at
/// spawn; `take_approval_receiver` hands it to exactly one indexer).
pub struct HostedSession {
    pub actor: SessionHandle,
    pub bus: Arc<EventBus>,
    approvals: std::sync::Mutex<Option<tokio::sync::mpsc::Receiver<ApprovalJob>>>,
}

impl HostedSession {
    /// Take the approval receiver — once. The caller (serve, TUI) owns the
    /// approval indexing; a second taker gets None.
    pub fn take_approval_receiver(&self) -> Option<tokio::sync::mpsc::Receiver<ApprovalJob>> {
        self.approvals
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
    }

    /// Non-blocking snapshot of the actor (missing reply = defaults).
    pub async fn snapshot(&self) -> SessionSnapshot {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let sent = self
            .actor
            .commands
            .send(SessionCommand::Snapshot { reply: tx })
            .await
            .is_ok();
        if !sent {
            return SessionSnapshot {
                session_id: self.actor.session_id.clone(),
                turn: 0,
                history_len: 0,
                journal_bytes: 0,
                pending_steers: 0,
                pending_approvals: 0,
                turn_running: false,
                cancelled: true,
                denied_tools: 0,
                executed_tools: 0,
            };
        }
        // the actor replies promptly; a hang here would be an actor bug —
        // bound it so the host can never stall on one bad actor
        match tokio::time::timeout(std::time::Duration::from_secs(2), rx).await {
            Ok(Ok(s)) => s,
            _ => SessionSnapshot {
                session_id: self.actor.session_id.clone(),
                turn: 0,
                history_len: 0,
                journal_bytes: 0,
                pending_steers: 0,
                pending_approvals: 0,
                turn_running: false,
                cancelled: true,
                denied_tools: 0,
                executed_tools: 0,
            },
        }
    }
}

/// Owns every SessionActor in the process.
pub struct SessionHost {
    factory: SessionFactory,
    sessions: Mutex<BTreeMap<String, Arc<HostedSession>>>,
    counter: AtomicU64,
}

impl SessionHost {
    pub fn new(factory: SessionFactory) -> Self {
        Self {
            factory,
            sessions: Mutex::new(BTreeMap::new()),
            counter: AtomicU64::new(0),
        }
    }

    /// Build (via the factory), spawn and register one session. Returns
    /// the hosted handle; the session id comes from the harness context.
    pub async fn create(&self, req: SessionRequest) -> Result<Arc<HostedSession>, String> {
        let deps = (self.factory)(req).await?;
        let session_id = deps.harness.ctx.session_id.clone();
        let journal_path = {
            let journal = deps
                .harness
                .journal
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            journal.path().to_path_buf()
        };
        let (actor, approval_rx) = spawn_session(
            deps.harness,
            deps.approval_rx,
            session_id.clone(),
            journal_path,
        );
        let hosted = Arc::new(HostedSession {
            actor,
            bus: deps.bus,
            approvals: std::sync::Mutex::new(Some(approval_rx)),
        });
        let n = self.counter.fetch_add(1, Ordering::SeqCst);
        let _ = n;
        self.sessions
            .lock()
            .await
            .insert(session_id, hosted.clone());
        Ok(hosted)
    }

    /// Look up a registered session by id.
    pub async fn get(&self, session_id: &str) -> Option<Arc<HostedSession>> {
        self.sessions.lock().await.get(session_id).cloned()
    }

    /// First registered session (the default surface target) or None.
    pub async fn default_session(&self) -> Option<Arc<HostedSession>> {
        let guard = self.sessions.lock().await;
        guard.values().next().cloned()
    }

    /// Get an existing session or create one (single default session per
    /// surface when no id is supplied).
    pub async fn get_or_create(
        &self,
        session_id: Option<&str>,
        req: SessionRequest,
    ) -> Result<Arc<HostedSession>, String> {
        if let Some(id) = session_id {
            if let Some(s) = self.get(id).await {
                return Ok(s);
            }
        }
        if session_id.is_none() {
            if let Some(s) = self.default_session().await {
                return Ok(s);
            }
        }
        self.create(req).await
    }

    /// Snapshots of every live session, in id order.
    pub async fn list(&self) -> Vec<SessionSnapshot> {
        let guard = self.sessions.lock().await;
        let mut out = Vec::with_capacity(guard.len());
        for s in guard.values() {
            out.push(s.snapshot().await);
        }
        out.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        out
    }

    /// Stop one session (in-flight turn cancelled first) and deregister.
    pub async fn close(&self, session_id: &str) -> bool {
        let removed = self.sessions.lock().await.remove(session_id);
        match removed {
            Some(s) => {
                let _ = s.actor.commands.send(SessionCommand::Shutdown).await;
                true
            }
            None => false,
        }
    }

    /// Stop everything (used at surface shutdown).
    pub async fn close_all(&self) {
        let ids: Vec<String> = self.sessions.lock().await.keys().cloned().collect();
        for id in ids {
            self.close(&id).await;
        }
    }

    /// Number of live sessions.
    pub async fn len(&self) -> usize {
        self.sessions.lock().await.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.len().await == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stub_factory(fail: bool) -> SessionFactory {
        Arc::new(move |_req| {
            Box::pin(async move {
                if fail {
                    return Err("factory refused".into());
                }
                let harness = test_harness().await;
                let bus = Arc::new(crate::events::EventBus::new(16));
                let (_perm_tx, approval_rx) = tokio::sync::mpsc::channel(4);
                Ok(SessionDeps {
                    harness,
                    bus,
                    approval_rx,
                })
            })
        })
    }

    // -- helper defined in session_host::tests to keep core free of
    // -- marfim-tools; builds a minimal harness around a mock provider.
    pub(super) async fn test_harness() -> Arc<Harness> {
        crate::session_host::session_host_tests::minimal_harness().await
    }

    #[tokio::test]
    async fn create_register_snapshot_close() {
        let host = SessionHost::new(stub_factory(false));
        let s = host
            .create(SessionRequest::default())
            .await
            .expect("create");
        let id = s.actor.session_id.clone();
        assert!(!id.is_empty());
        let snap = s.snapshot().await;
        assert_eq!(snap.session_id, id);
        assert!(!snap.turn_running);
        assert_eq!(host.len().await, 1);
        let listed = host.list().await;
        assert_eq!(listed.len(), 1);
        assert!(host.close(&id).await);
        assert!(host.is_empty().await);
        assert!(!host.close(&id).await);
    }

    #[tokio::test]
    async fn get_or_create_reuses_default() {
        let host = SessionHost::new(stub_factory(false));
        let a = host
            .get_or_create(None, SessionRequest::default())
            .await
            .unwrap();
        let b = host
            .get_or_create(None, SessionRequest::default())
            .await
            .unwrap();
        assert_eq!(a.actor.session_id, b.actor.session_id);
        assert_eq!(host.len().await, 1);
        let missing = host
            .get_or_create(Some("nope"), SessionRequest::default())
            .await
            .unwrap();
        assert_ne!(missing.actor.session_id, a.actor.session_id);
        assert_eq!(host.len().await, 2);
        host.close_all().await;
        assert!(host.is_empty().await);
    }

    #[tokio::test]
    async fn factory_errors_propagate() {
        let host = SessionHost::new(stub_factory(true));
        let err = match host.create(SessionRequest::default()).await {
            Ok(_) => panic!("factory error must surface, not succeed"),
            Err(e) => e,
        };
        assert!(err.contains("refused"));
        assert!(host.is_empty().await);
    }
}

/// Test support (cfg(test) only): a minimal harness so SessionHost tests
/// exercise real actors without marfim-tools. NOT compiled into release.
#[cfg(test)]
pub(crate) mod session_host_tests {
    use super::*;
    use crate::journal::Journal;
    use crate::loop_::LoopCtx;
    use crate::permissions::{Mode, PermissionEngine};
    use crate::providers::{CostBook, MockProvider};
    use crate::router::{ModelEntry, Role, Router};
    use serde_json::Value;
    use tokio::sync::RwLock;

    struct StubTools;

    #[async_trait::async_trait]
    impl crate::loop_::ToolHost for StubTools {
        async fn execute(&self, tool: &str, _args: &Value, _s: &LoopCtx) -> (String, bool) {
            (format!("stub {tool} executed"), false)
        }
        fn schemas(&self) -> Vec<Value> {
            vec![]
        }
        fn is_mutating(&self, _tool: &str) -> bool {
            false
        }
        fn verify_commands(&self) -> Vec<String> {
            vec![]
        }
        async fn run_verify(&self, _s: &LoopCtx) -> (bool, String) {
            (true, "ok".into())
        }
    }

    /// Deterministic unique-ish journal path per test process+counter.
    fn scratch_journal(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("marfim-session-host-tests");
        let _ = std::fs::create_dir_all(&dir);
        let n = std::sync::atomic::AtomicU64::new(0);
        let k = n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let path = dir.join(format!(
            "{tag}-{}-{k}-{}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        Journal::open(&path).expect("journal open");
        path
    }

    pub(crate) async fn minimal_harness() -> Arc<Harness> {
        let bus = EventBus::new(64);
        let mut router = Router::new(CostBook::default(), false);
        router.set_chain(
            Role::Primary,
            vec![ModelEntry {
                model: "mock-1".into(),
                provider: Arc::new(MockProvider::new(vec![])),
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
        let (perm, _approval_rx) = PermissionEngine::new(Mode::Default, vec![]);
        // The actor owns the approval receiver; tests of SessionHost do not
        // exercise approvals, so park the engine's receiver in a zombie task.
        let (_keep_tx, keep_rx) = tokio::sync::mpsc::channel::<ApprovalJob>(4);
        tokio::spawn(async move {
            let mut rx = keep_rx;
            while rx.recv().await.is_some() {}
        });
        let id = format!(
            "sh-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        );
        let harness = Harness::new(
            bus,
            Arc::new(RwLock::new(router)),
            Arc::new(StubTools),
            Arc::new(std::sync::Mutex::new(
                Journal::open(&scratch_journal("sh")).expect("journal"),
            )),
            LoopCtx {
                session_id: id,
                cwd: std::env::temp_dir().to_string_lossy().to_string(),
                cancel: crate::loop_::CancelToken::default(),
            },
            Arc::new(perm),
            64,
            3,
            100_000,
            false,
        );
        Arc::new(harness)
    }
}
