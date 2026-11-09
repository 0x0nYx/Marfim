//! W10.2 (§G, Hermes `async_delegation.py` doctrine, clean-room): the
//! async delegation ledger — dispatch rows for work handed to a worker
//! agent that settles LATER. Rules (all enforced here):
//!
//! - **Replay window**: a dispatched row may be re-dispatched (replayed)
//!   for at most [`REPLAY_WINDOW_MS`] (48h); past the window the row
//!   EXPIRES (results would contradict work the caller already assumed
//!   lost).
//! - **Attempt cap**: at most [`MAX_ATTEMPTS`] dispatches per row.
//! - **Orphan sweep**: a dispatched row whose lease is stale
//!   ([`LEASE_TTL_MS`]) returns to the queue on the next sweep (the
//!   sweep period is the caller's cadence; the ledger decides state).
//! - **Settle**: the result enters as a NEW turn upstream; here the row
//!   closes exactly once with its result recorded.

use crate::schema::migrate;
use crate::wal;
use rusqlite::Connection;

/// 48h replay window.
pub const REPLAY_WINDOW_MS: u64 = 48 * 3600 * 1000;
/// At most this many dispatch attempts.
pub const MAX_ATTEMPTS: u32 = 8;
/// Dispatch lease TTL (the orphan threshold).
pub const LEASE_TTL_MS: u64 = 300_000;
/// The orphan sweep cadence the caller should use (30s).
pub const SWEEP_PERIOD_MS: u64 = 30_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum DelegationState {
    Queued,
    Dispatched,
    Replayed,
    Done,
    Expired,
}

impl DelegationState {
    pub fn name(&self) -> &'static str {
        match self {
            DelegationState::Queued => "queued",
            DelegationState::Dispatched => "dispatched",
            DelegationState::Replayed => "replayed",
            DelegationState::Done => "done",
            DelegationState::Expired => "expired",
        }
    }
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "queued" => DelegationState::Queued,
            "dispatched" => DelegationState::Dispatched,
            "replayed" => DelegationState::Replayed,
            "done" => DelegationState::Done,
            "expired" => DelegationState::Expired,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DelegationRow {
    pub id: i64,
    pub prompt: String,
    pub target: String,
    pub state: DelegationState,
    pub attempts: u32,
    pub result: Option<String>,
}

pub struct DelegationLedger {
    conn: Connection,
    policy_marker: wal::WalPolicy,
}

impl DelegationLedger {
    pub fn open(path: &std::path::Path) -> Result<Self, String> {
        let conn = Connection::open(path).map_err(|e| format!("open {path:?}: {e}"))?;
        let policy = wal::apply(&conn, false)?;
        migrate(&conn)?;
        Ok(Self {
            conn,
            policy_marker: policy,
        })
    }

    pub fn open_in_memory() -> Result<Self, String> {
        let conn = Connection::open_in_memory().map_err(|e| e.to_string())?;
        wal::apply(&conn, false)?;
        migrate(&conn)?;
        Ok(Self {
            conn,
            policy_marker: wal::WalPolicy::Wal,
        })
    }

    pub fn journal_mode(&self) -> &'static str {
        self.policy_marker.name()
    }

    /// Queue a delegation.
    pub fn queue(&mut self, prompt: &str, target: &str, now_ms: u64) -> Result<i64, String> {
        if prompt.trim().is_empty() || target.trim().is_empty() {
            return Err("prompt and target are required".into());
        }
        self.conn
            .execute(
                "INSERT INTO delegation(prompt, target, created_ms) VALUES (?1, ?2, ?3)",
                rusqlite::params![prompt, target, now_ms as i64],
            )
            .map_err(|e| format!("queue: {e}"))?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Dispatch the OLDEST queueable row (queued or replayable) to a
    /// holder. Returns (id, prompt, target) or None. Enforces the
    /// attempt cap and the replay window (an over-cap or over-window row
    /// is moved to its terminal state instead of dispatching — never
    /// silently dropped).
    pub fn dispatch_next(
        &mut self,
        holder: &str,
        now_ms: u64,
    ) -> Result<Option<(i64, String, String)>, String> {
        // terminalize over-cap / over-window rows first (honest sweep)
        self.expire_unrunnable(now_ms)?;
        let row: Option<(i64, String, String, i64)> = {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT id, prompt, target, attempts FROM delegation \
                     WHERE state IN ('queued','replayed') ORDER BY id ASC LIMIT 1",
                )
                .map_err(|e| e.to_string())?;
            stmt.query_row([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .ok()
        };
        let Some((id, prompt, target, attempts)) = row else {
            return Ok(None);
        };
        let attempts = attempts as u32 + 1;
        if attempts > MAX_ATTEMPTS {
            self.conn
                .execute(
                    "UPDATE delegation SET state='expired', settled_ms=?2 WHERE id=?1",
                    rusqlite::params![id, now_ms as i64],
                )
                .map_err(|e| e.to_string())?;
            return Err(format!(
                "delegation {id} exceeded {MAX_ATTEMPTS} attempts — expired honestly"
            ));
        }
        let state = if attempts == 1 {
            "dispatched"
        } else {
            "replayed"
        };
        self.conn
            .execute(
                "UPDATE delegation SET state=?2, attempts=?3, holder=?4, lease_expires_ms=?5 WHERE id=?1",
                rusqlite::params![
                    id,
                    state,
                    attempts as i64,
                    holder,
                    (now_ms + LEASE_TTL_MS) as i64
                ],
            )
            .map_err(|e| e.to_string())?;
        Ok(Some((id, prompt, target)))
    }

    /// Orphan sweep: dispatched rows with an expired lease return to the
    /// queue. Returns the ids reclaimed.
    pub fn sweep_orphans(&mut self, now_ms: u64) -> Result<Vec<i64>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id FROM delegation WHERE state IN ('dispatched','replayed') \
                 AND lease_expires_ms IS NOT NULL AND lease_expires_ms < ?1",
            )
            .map_err(|e| e.to_string())?;
        let ids: Vec<i64> = stmt
            .query_map(rusqlite::params![now_ms as i64], |r| r.get(0))
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect();
        drop(stmt);
        for id in &ids {
            self.conn
                .execute(
                    "UPDATE delegation SET state='queued', holder=NULL, lease_expires_ms=NULL WHERE id=?1",
                    rusqlite::params![id],
                )
                .map_err(|e| e.to_string())?;
        }
        Ok(ids)
    }

    /// Settle a row with its result (exactly once; a second settle is
    /// refused — the result already entered as a turn upstream).
    pub fn settle(&mut self, id: i64, result: &str, now_ms: u64) -> Result<(), String> {
        let n = self
            .conn
            .execute(
                "UPDATE delegation SET state='done', result=?2, settled_ms=?3 \
                 WHERE id=?1 AND state IN ('dispatched','replayed')",
                rusqlite::params![id, result, now_ms as i64],
            )
            .map_err(|e| e.to_string())?;
        if n == 0 {
            let st: String = self
                .conn
                .query_row(
                    "SELECT state FROM delegation WHERE id=?1",
                    rusqlite::params![id],
                    |r| r.get(0),
                )
                .unwrap_or_else(|_| "missing".into());
            return Err(format!(
                "delegation {id} is {st:?} — settle refused (exactly-once)"
            ));
        }
        Ok(())
    }

    /// Expire rows that cannot run anymore: over the replay window, or
    /// over the attempt cap (swept BEFORE dispatch; visible, not silent).
    fn expire_unrunnable(&mut self, now_ms: u64) -> Result<usize, String> {
        let n = self
            .conn
            .execute(
                "UPDATE delegation SET state='expired', settled_ms=?2 \
                 WHERE state IN ('queued','replayed') \
                 AND (created_ms < ?1 - ?3 OR attempts >= ?4)",
                rusqlite::params![
                    now_ms as i64,
                    now_ms as i64,
                    REPLAY_WINDOW_MS as i64,
                    MAX_ATTEMPTS as i64
                ],
            )
            .map_err(|e| e.to_string())?;
        Ok(n)
    }

    pub fn list(&self) -> Result<Vec<DelegationRow>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, prompt, target, state, attempts, result FROM delegation ORDER BY id ASC")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, Option<String>>(5)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for row in rows {
            let (id, prompt, target, state, attempts, result) = row.map_err(|e| e.to_string())?;
            out.push(DelegationRow {
                id,
                prompt,
                target,
                state: DelegationState::parse(&state)
                    .ok_or_else(|| format!("corrupt delegation state {state:?}"))?,
                attempts: attempts as u32,
                result,
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use marfim_testkit::Temp;

    #[test]
    fn queue_dispatch_settle_exactly_once() {
        let mut l = DelegationLedger::open_in_memory().unwrap();
        let id = l
            .queue("fix the flaky test", "build-resolver", 100)
            .unwrap();
        let got = l.dispatch_next("session-1", 110).unwrap().unwrap();
        assert_eq!(got.0, id);
        assert_eq!(got.1, "fix the flaky test");
        assert_eq!(got.2, "build-resolver");
        l.settle(id, "test fixed, suite green", 200).unwrap();
        // second settle refused
        assert!(l.settle(id, "again", 201).is_err());
        let rows = l.list().unwrap();
        assert_eq!(rows[0].state, DelegationState::Done);
        assert_eq!(rows[0].result.as_deref(), Some("test fixed, suite green"));
        // nothing else to dispatch
        assert!(l.dispatch_next("s", 300).unwrap().is_none());
    }

    #[test]
    fn replay_window_48h_then_expire() {
        let mut l = DelegationLedger::open_in_memory().unwrap();
        let t0 = 1_000_000u64; // created here
        let id = l.queue("p", "worker", t0).unwrap();
        l.dispatch_next("h", t0 + 1000).unwrap();
        // orphan sweep returns it to the queue after the lease TTL
        assert_eq!(
            l.sweep_orphans(t0 + 1000 + LEASE_TTL_MS + 1).unwrap(),
            vec![id]
        );
        // within the 48h window (from CREATION) it re-dispatches (REPLAYED)
        let within = t0 + REPLAY_WINDOW_MS - 1000;
        let got = l.dispatch_next("h2", within).unwrap().unwrap();
        assert_eq!(got.0, id);
        let rows = l.list().unwrap();
        assert_eq!(rows[0].state, DelegationState::Replayed);
        assert_eq!(rows[0].attempts, 2);
        // past the window the row expires instead of dispatching
        l.sweep_orphans(within + LEASE_TTL_MS + 1).unwrap();
        let past = t0 + REPLAY_WINDOW_MS + 1_000_000;
        l.sweep_orphans(past).unwrap();
        assert!(l.dispatch_next("h3", past).unwrap().is_none());
        let rows = l.list().unwrap();
        assert_eq!(rows[0].state, DelegationState::Expired);
    }

    #[test]
    fn attempt_cap_eight() {
        let mut l = DelegationLedger::open_in_memory().unwrap();
        l.queue("p", "worker", 0).unwrap();
        let mut t = 0u64;
        for n in 0..MAX_ATTEMPTS {
            t += LEASE_TTL_MS + 1000;
            l.sweep_orphans(t).unwrap();
            let got = l.dispatch_next("h", t).unwrap();
            assert!(got.is_some(), "attempt {} should dispatch", n + 1);
        }
        assert_eq!(l.list().unwrap()[0].attempts, MAX_ATTEMPTS);
        // the 9th attempt never dispatches: the row expires honestly
        t += LEASE_TTL_MS + 1000;
        l.sweep_orphans(t).unwrap();
        assert!(l.dispatch_next("h", t).unwrap().is_none());
        let rows = l.list().unwrap();
        assert_eq!(rows[0].state, DelegationState::Expired);
        assert_eq!(rows[0].attempts, MAX_ATTEMPTS);
    }

    #[test]
    fn empty_inputs_refused() {
        let mut l = DelegationLedger::open_in_memory().unwrap();
        assert!(l.queue("", "w", 0).is_err());
        assert!(l.queue("p", " ", 0).is_err());
    }

    #[test]
    fn file_open_round_trip() {
        let dir = Temp::new("marfim-delegation").unwrap();
        let path = dir.path().join("d.db");
        {
            let mut l = DelegationLedger::open(&path).unwrap();
            l.queue("survives reopen", "worker", 1).unwrap();
        }
        let l = DelegationLedger::open(&path).unwrap();
        let rows = l.list().unwrap();
        assert_eq!(rows[0].prompt, "survives reopen");
        assert_eq!(l.journal_mode(), "wal");
    }
}
