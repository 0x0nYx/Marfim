//! The terminal registry: bounded id space, honest caps, lazy reaping.

use crate::session::{TermSession, TermSpec, REAP_AFTER};
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Sessions a single surface may hold at once. Exited sessions keep their
/// final screen readable until they are reaped, but the cap counts them
/// too — forgetting to close a runaway loop must surface as an error, not
/// as unbounded process growth.
pub const MAX_SESSIONS: usize = 16;

/// Serialized descriptor for lists and route responses.
#[derive(Debug, Clone, Serialize)]
pub struct TermInfo {
    pub id: u64,
    pub command: String,
    pub alive: bool,
    pub exit_code: Option<u32>,
    pub total_bytes: u64,
    pub pending_bytes: usize,
    pub age_secs: u64,
}

#[derive(Default)]
struct Inner {
    next: AtomicU64,
    map: BTreeMap<u64, Arc<TermSession>>,
}

/// Owns every terminal session of one surface (the engine's ToolCtx or
/// the gateway embedding). Spawn is capped, ids are monotonic, reaping is
/// lazy on access (no background reaper thread — nothing ticks when the
/// surface is idle).
pub struct TermServer {
    max_sessions: usize,
    inner: Mutex<Inner>,
}

impl Default for TermServer {
    fn default() -> Self {
        Self::new(MAX_SESSIONS)
    }
}

impl TermServer {
    pub fn new(max_sessions: usize) -> Self {
        Self {
            max_sessions: max_sessions.max(1),
            inner: Mutex::new(Inner::default()),
        }
    }

    /// Spawn a session; errors honestly when the cap is reached (the
    /// error names the cap and the remedy).
    pub fn spawn(&self, spec: &TermSpec) -> Result<u64, String> {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        self.reap_locked(&mut inner, Instant::now());
        if inner.map.len() >= self.max_sessions {
            return Err(format!(
                "terminal session cap reached ({}/{}); close one with action=close first",
                inner.map.len(),
                self.max_sessions
            ));
        }
        let id = inner.next.fetch_add(1, Ordering::SeqCst) + 1;
        let session = TermSession::spawn(id, spec)?;
        inner.map.insert(id, session);
        Ok(id)
    }

    pub fn get(&self, id: u64) -> Option<Arc<TermSession>> {
        let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.map.get(&id).cloned()
    }

    /// List (after reaping expired exits). Newest last — id order.
    pub fn list(&self) -> Vec<TermInfo> {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        self.reap_locked(&mut inner, Instant::now());
        let now = Instant::now();
        inner
            .map
            .values()
            .map(|s| TermInfo {
                id: s.id(),
                command: s.command().to_string(),
                alive: s.alive(),
                exit_code: s.exit_code(),
                total_bytes: s.total_bytes(),
                pending_bytes: s.pending_len(),
                age_secs: now.duration_since(s.started_at()).as_secs(),
            })
            .collect()
    }

    /// Kill + remove one session. Reports whether it existed.
    pub fn close(&self, id: u64) -> bool {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        match inner.map.remove(&id) {
            Some(s) => {
                s.kill();
                true
            }
            None => false,
        }
    }

    pub fn active(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .map
            .len()
    }

    pub fn max_sessions(&self) -> usize {
        self.max_sessions
    }

    /// Kill everything (surface shutdown path).
    pub fn close_all(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        for (_, s) in std::mem::take(&mut inner.map) {
            s.kill();
        }
    }

    fn reap_locked(&self, inner: &mut Inner, now: Instant) {
        let expired: Vec<u64> = inner
            .map
            .iter()
            .filter(|(_, s)| s.reapable(now))
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            if let Some(s) = inner.map.remove(&id) {
                s.kill();
            }
        }
    }
}

/// Reap grace re-exported for tests/docs (keeps the constant single-source).
pub const REAP_GRACE: std::time::Duration = REAP_AFTER;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::TermSpec;

    fn sh(script: &str) -> TermSpec {
        let mut s = TermSpec::new("sh");
        s.args = vec!["-c".into(), script.into()];
        s
    }

    #[test]
    fn ids_are_monotonic_and_get_works() {
        let srv = TermServer::new(4);
        let a = srv.spawn(&sh("sleep 5")).unwrap();
        let b = srv.spawn(&sh("sleep 5")).unwrap();
        assert!(b > a);
        assert_eq!(srv.get(b).unwrap().id(), b);
        assert!(srv.get(999).is_none());
        assert_eq!(srv.active(), 2);
        srv.close_all();
        assert_eq!(srv.active(), 0);
    }

    #[test]
    fn cap_is_enforced_with_an_honest_error() {
        let srv = TermServer::new(2);
        srv.spawn(&sh("sleep 5")).unwrap();
        srv.spawn(&sh("sleep 5")).unwrap();
        let err = srv.spawn(&sh("sleep 5")).unwrap_err();
        assert!(err.contains("cap reached (2/2)"), "err: {err}");
        assert!(srv.close(1), "close frees a slot");
        assert!(srv.spawn(&sh("sleep 5")).is_ok());
        srv.close_all();
    }

    #[test]
    fn close_kills_the_child() {
        let srv = TermServer::new(4);
        let id = srv.spawn(&sh("sleep 30")).unwrap();
        let s = srv.get(id).unwrap();
        assert!(s.alive());
        assert!(srv.close(id));
        assert!(!srv.close(id), "second close is false");
        for _ in 0..100 {
            if !s.alive() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(!s.alive(), "closed session's child must die");
        assert_eq!(srv.list().len(), 0, "removed from the registry");
    }

    #[test]
    fn list_reports_shape() {
        let srv = TermServer::new(4);
        let id = srv.spawn(&sh("echo hi")).unwrap();
        for _ in 0..100 {
            if srv.get(id).map(|s| !s.alive()).unwrap_or(false) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let infos = srv.list();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].id, id);
        assert!(!infos[0].alive);
        assert_eq!(infos[0].exit_code, Some(0));
        assert!(infos[0].total_bytes > 0);
        srv.close_all();
    }
}
