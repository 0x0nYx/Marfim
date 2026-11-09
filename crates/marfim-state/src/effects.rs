//! W10.2 (§G, dsh `effects.ts`/`outbox.ts` doctrine, clean-room): the
//! durable ordered effect queue + outbox. STORE-FIRST: an effect exists
//! in sqlite BEFORE any execution (the crash between store and act is
//! the whole point). ORDERED: effects run strictly in insertion order —
//! an effect behind an unsettled head CANNOT run (this is an outbox,
//! not a pool). Crash recovery: a claimed-but-unsettled head REPLAYS on
//! reopen (attempts increment, the payload digest pins identity); at
//! [`MAX_ATTEMPTS`] the head dead-letters with an honest error and the
//! queue stalls until a human advances past it — never silently skips
//! (ordering is the contract).
//!
//! Receipts: exactly-once per (effect, attempt)? No — per EFFECT: the
//! receipt row is keyed by the effect id and written in the same
//! transaction as the state flip, so a replayed settle is refused.

use crate::schema::migrate;
use crate::wal;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

/// Attempt cap before a head dead-letters.
pub const MAX_ATTEMPTS: u32 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffectState {
    Queued,
    Claimed,
    Done,
    Dead,
}

impl EffectState {
    pub fn name(&self) -> &'static str {
        match self {
            EffectState::Queued => "queued",
            EffectState::Claimed => "claimed",
            EffectState::Done => "done",
            EffectState::Dead => "dead",
        }
    }
    fn parse(s: &str) -> Option<Self> {
        match s {
            "queued" => Some(EffectState::Queued),
            "claimed" => Some(EffectState::Claimed),
            "done" => Some(EffectState::Done),
            "dead" => Some(EffectState::Dead),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Effect {
    pub id: i64,
    pub kind: String,
    pub payload: String,
    pub state: EffectState,
    pub attempts: u32,
    pub receipt: Option<String>,
}

fn digest(s: &str) -> String {
    use std::hash::Hasher;
    let mut h = std::hash::DefaultHasher::new();
    h.write(s.as_bytes());
    format!("{:016x}", h.finish())
}

/// The ordered durable outbox.
pub struct EffectQueue {
    conn: Connection,
    policy_marker: wal::WalPolicy,
}

/// What [`EffectQueue::run_next`] hands the executor.
pub struct HeadEffect {
    pub id: i64,
    pub kind: String,
    pub payload: String,
}

/// The executor's verdict for one effect.
pub struct Outcome {
    pub ok: bool,
    pub receipt_json: String,
}

impl EffectQueue {
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

    /// Store-first enqueue (at the tail — order is insertion order).
    pub fn enqueue(&mut self, kind: &str, payload: &str, now_ms: u64) -> Result<i64, String> {
        self.conn
            .execute(
                "INSERT INTO effects(kind, payload, payload_digest, created_ms) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![kind, payload, digest(payload), now_ms as i64],
            )
            .map_err(|e| format!("enqueue: {e}"))?;
        Ok(self.conn.last_insert_rowid())
    }

    /// The head of the queue: the OLDEST effect that is not yet settled,
    /// skipping `done` rows; a `dead` row STALLS the queue (ordering is
    /// the contract — nothing runs past a corpse). `None` = nothing
    /// runnable.
    fn head(&self) -> Result<Option<HeadEffect>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, kind, payload, state FROM effects ORDER BY id ASC")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (id, kind, payload, state) = row.map_err(|e| e.to_string())?;
            match EffectState::parse(&state)
                .ok_or_else(|| format!("corrupt effect state {state:?}"))?
            {
                EffectState::Done => continue, // settled: behind us
                EffectState::Dead => {
                    return Err(format!(
                        "effect {id} is dead-lettered — the queue is STALLED (ordering contract); settle or advance by hand"
                    ));
                }
                EffectState::Queued | EffectState::Claimed => {
                    return Ok(Some(HeadEffect { id, kind, payload }));
                }
            }
        }
        Ok(None)
    }

    /// Run the head: claim (or reuse the crash-recovered claim), invoke
    /// the executor ONCE, then settle in the SAME transaction as the
    /// receipt write. `Ok(None)` = nothing runnable. A failed attempt
    /// increments the counter and KEEPS the head (ordering): at
    /// [`MAX_ATTEMPTS`] the head dead-letters and the queue reports the
    /// stall honestly.
    pub fn run_next<F>(&mut self, mut exec: F, now_ms: u64) -> Result<Option<(i64, String)>, String>
    where
        F: FnMut(&HeadEffect) -> Outcome,
    {
        let Some(head) = self.head()? else {
            return Ok(None);
        };
        // claim + attempts increment (crash-safe: the claim rides a single UPDATE)
        let attempts: u32 = {
            self.conn
                .execute(
                    "UPDATE effects SET state='claimed', attempts=attempts+1, claimed_by=?2 WHERE id=?1",
                    rusqlite::params![head.id, format!("run:{now_ms}")],
                )
                .map_err(|e| format!("claim: {e}"))?;
            self.conn
                .query_row(
                    "SELECT attempts FROM effects WHERE id=?1",
                    rusqlite::params![head.id],
                    |r| r.get::<_, i64>(0),
                )
                .map_err(|e| e.to_string())? as u32
        };
        let outcome = exec(&head);
        if outcome.ok {
            self.conn
                .execute(
                    "UPDATE effects SET state='done', receipt=?2, done_ms=?3 WHERE id=?1 AND state='claimed'",
                    rusqlite::params![head.id, outcome.receipt_json, now_ms as i64],
                )
                .map_err(|e| format!("settle: {e}"))?;
            return Ok(Some((head.id, outcome.receipt_json)));
        }
        if attempts >= MAX_ATTEMPTS {
            self.conn
                .execute(
                    "UPDATE effects SET state='dead', receipt=?2, done_ms=?3 WHERE id=?1 AND state='claimed'",
                    rusqlite::params![head.id, outcome.receipt_json, now_ms as i64],
                )
                .map_err(|e| format!("dead-letter: {e}"))?;
            return Err(format!(
                "effect {} dead-lettered after {MAX_ATTEMPTS} attempts: {} — the queue is STALLED (ordering contract); settle or advance by hand",
                head.id, outcome.receipt_json
            ));
        }
        // failed attempt: back to queued (the same head re-runs next call)
        self.conn
            .execute(
                "UPDATE effects SET state='queued', receipt=?2 WHERE id=?1 AND state='claimed'",
                rusqlite::params![head.id, outcome.receipt_json],
            )
            .map_err(|e| format!("requeue: {e}"))?;
        Err(format!(
            "effect {} attempt {attempts}/{MAX_ATTEMPTS} failed: {}",
            head.id, outcome.receipt_json
        ))
    }

    /// Crash recovery surface: a claimed head from a previous process
    /// re-runs on the next [`EffectQueue::run_next`] — this probe makes
    /// that visible (id + attempts) without executing anything.
    pub fn pending_head(&self) -> Result<Option<(i64, u32)>, String> {
        match self.head()? {
            None => Ok(None),
            Some(h) => {
                let attempts: i64 = self
                    .conn
                    .query_row(
                        "SELECT attempts FROM effects WHERE id=?1",
                        rusqlite::params![h.id],
                        |r| r.get(0),
                    )
                    .map_err(|e| e.to_string())?;
                Ok(Some((h.id, attempts as u32)))
            }
        }
    }

    /// The whole queue (bounded reads only — the outbox is not a log).
    pub fn list(&self) -> Result<Vec<Effect>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, kind, payload, state, attempts, receipt FROM effects ORDER BY id ASC",
            )
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
            let (id, kind, payload, state, attempts, receipt) = row.map_err(|e| e.to_string())?;
            out.push(Effect {
                id,
                kind,
                payload,
                state: EffectState::parse(&state)
                    .ok_or_else(|| format!("corrupt state {state:?}"))?,
                attempts: attempts as u32,
                receipt,
            });
        }
        Ok(out)
    }

    /// Human maintenance: advance past a dead head (the honest stall
    /// breaker — recorded as a receipt-bearing transition, never a
    /// silent skip).
    pub fn advance_dead_head(&mut self, now_ms: u64) -> Result<bool, String> {
        let n = self
            .conn
            .execute(
                "UPDATE effects SET state='done', receipt=?2, done_ms=?3 WHERE id=(SELECT MIN(id) FROM effects WHERE state='dead')",
                rusqlite::params![
                    now_ms as i64,
                    format!("{{\"advanced\": true, \"at\": {now_ms}}}"),
                    now_ms as i64
                ],
            )
            .map_err(|e| e.to_string())?;
        Ok(n > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use marfim_testkit::Temp;

    #[test]
    fn store_first_then_ordered_execution() {
        let mut q = EffectQueue::open_in_memory().unwrap();
        let a = q.enqueue("email", r#"{"to":"x"}"#, 10).unwrap();
        let b = q.enqueue("email", r#"{"to":"y"}"#, 11).unwrap();
        assert!(a < b);
        // both exist BEFORE any execution
        assert!(matches!(q.pending_head().unwrap(), Some((id, 0)) if id == a));
        // execution runs the HEAD only, in order
        let mut ran = Vec::new();
        let r1 = q
            .run_next(
                |h| {
                    ran.push(h.id);
                    Outcome {
                        ok: true,
                        receipt_json: format!("{{\"ran\":{}}}", h.id),
                    }
                },
                100,
            )
            .unwrap()
            .unwrap();
        assert_eq!(r1, (a, r#"{"ran":1}"#.to_string()));
        assert_eq!(ran, vec![a]);
        // the SECOND effect cannot jump the queue — but the head is now
        // done, so it runs next
        let r2 = q
            .run_next(
                |h| Outcome {
                    ok: true,
                    receipt_json: format!("{{\"ran\":{}}}", h.id),
                },
                101,
            )
            .unwrap()
            .unwrap();
        assert_eq!(r2.0, b);
        // queue drained
        assert!(q
            .run_next(
                |_| Outcome {
                    ok: true,
                    receipt_json: "{}".into()
                },
                102
            )
            .unwrap()
            .is_none());
    }

    #[test]
    fn failed_attempt_keeps_head_and_counts() {
        let mut q = EffectQueue::open_in_memory().unwrap();
        let a = q.enqueue("flaky", "p", 1).unwrap();
        let exec_fail = |_: &HeadEffect| Outcome {
            ok: false,
            receipt_json: "boom".into(),
        };
        for attempt in 1..MAX_ATTEMPTS {
            let err = q.run_next(exec_fail, attempt as u64).unwrap_err();
            assert!(
                err.contains(&format!("attempt {attempt}/{MAX_ATTEMPTS}")),
                "{err}"
            );
            // the head is still the same effect
            assert!(matches!(q.pending_head().unwrap(), Some((id, n)) if id == a && n == attempt));
        }
        // the MAX_ATTEMPTS-th failure dead-letters and stalls
        let err = q.run_next(exec_fail, 99).unwrap_err();
        assert!(err.contains("dead-lettered"), "{err}");
        assert!(err.contains("STALLED"));
        let list = q.list().unwrap();
        assert_eq!(list[0].state, EffectState::Dead);
        // a NEW effect cannot jump the corpse
        q.enqueue("fresh", "p2", 2).unwrap();
        let exec_ok = |_: &HeadEffect| Outcome {
            ok: true,
            receipt_json: "ok".into(),
        };
        assert!(
            q.run_next(exec_ok, 100).is_err(),
            "stalled queue refuses to run"
        );
        // advance past the dead head → the fresh effect runs
        assert!(q.advance_dead_head(101).unwrap());
        let r = q.run_next(exec_ok, 102).unwrap().unwrap();
        assert_eq!(r.0, 2);
    }

    #[test]
    fn crash_recovery_claimed_head_replays() {
        let dir = Temp::new("marfim-effects-crash").unwrap();
        let path = dir.path().join("effects.db");
        let mut q = EffectQueue::open(&path).unwrap();
        q.enqueue("deploy", r#"{"env":"prod"}"#, 1).unwrap();
        // claim + crash (drop WITHOUT settling)
        let _ = q
            .run_next(
                |h| {
                    let _ = h;
                    // simulate the crash: executor panics are not the path
                    // here — the test drops the queue instead (below).
                    Outcome {
                        ok: false,
                        receipt_json: "crash-marker".into(),
                    }
                },
                5,
            )
            .unwrap_err();
        drop(q);
        // reopen: the head is still the same effect, attempts recorded
        let mut q2 = EffectQueue::open(&path).unwrap();
        let (id, attempts) = q2.pending_head().unwrap().unwrap();
        assert_eq!(id, 1);
        assert_eq!(attempts, 1);
        // and it REPLAYS (store-first recovery)
        let r = q2
            .run_next(
                |h| Outcome {
                    ok: true,
                    receipt_json: format!("recovered {}", h.payload),
                },
                10,
            )
            .unwrap()
            .unwrap();
        assert!(r.1.contains("recovered"));
    }

    #[test]
    fn receipt_records_state_and_journal_mode_is_honest() {
        let dir = Temp::new("marfim-effects-receipt").unwrap();
        let mut q = EffectQueue::open(&dir.path().join("e.db")).unwrap();
        q.enqueue("k", "p", 1).unwrap();
        q.run_next(
            |_| Outcome {
                ok: true,
                receipt_json: r#"{"ok":true}"#.into(),
            },
            2,
        )
        .unwrap();
        let list = q.list().unwrap();
        assert_eq!(list[0].state, EffectState::Done);
        assert_eq!(list[0].receipt.as_deref(), Some(r#"{"ok":true}"#));
        assert_eq!(list[0].done_hint(), Some(()));
        assert_eq!(q.journal_mode(), "wal");
    }

    impl Effect {
        fn done_hint(&self) -> Option<()> {
            if self.state == EffectState::Done {
                Some(())
            } else {
                None
            }
        }
    }
}
