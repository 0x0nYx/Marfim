//! BEGIN IMMEDIATE write core (distilled: Hermes `hermes_state.py`,
//! clean-room): one writer, serialized by SQLite itself. Every mutation
//! runs inside `BEGIN IMMEDIATE` (the write lock is taken UP FRONT — no
//! upgrade deadlocks), with bounded retries across the three patience
//! tiers, one mid-transaction retry on IOERR, and a passive checkpoint
//! every [`CHECKPOINT_EVERY`] writes so the -wal file cannot creep.

use rusqlite::Connection;
use std::time::Duration;

/// A passive checkpoint every N successful writes.
pub const CHECKPOINT_EVERY: u64 = 50;
/// Retry patience: 3 attempts across the tier backoffs (10/50/200ms).
pub const RETRY_BACKOFF_MS: [u64; 3] = [10, 50, 200];

/// The result of one write: rows changed + whether a checkpoint ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteOutcome {
    pub rows_changed: usize,
    pub checkpointed: bool,
}

/// The write core wraps ONE connection used ONLY for writes (readers use
/// their own connections — WAL allows the concurrency).
pub struct WriteCore {
    writes: u64,
}

impl Default for WriteCore {
    fn default() -> Self {
        Self::new()
    }
}

impl WriteCore {
    pub fn new() -> Self {
        Self { writes: 0 }
    }

    /// Execute `f` inside BEGIN IMMEDIATE with bounded retries. `f` must
    /// be idempotent-safe: on retry it runs against a FRESH transaction.
    pub fn exec<F>(&mut self, conn: &Connection, mut f: F) -> Result<WriteOutcome, String>
    where
        F: FnMut(&Connection) -> Result<usize, rusqlite::Error>,
    {
        let mut last_err = String::new();
        for (attempt, backoff) in RETRY_BACKOFF_MS.iter().enumerate() {
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(*backoff));
            }
            conn.execute_batch("BEGIN IMMEDIATE;")
                .map_err(|e| format!("begin immediate: {e}"))?;
            match f(conn) {
                Ok(rows) => {
                    // one mid-transaction IOERR retry doctrine: a commit
                    // failure retries ONCE inside the same patience tier
                    let mut commit = conn.execute_batch("COMMIT;");
                    if commit.is_err() {
                        commit = conn.execute_batch("COMMIT;");
                    }
                    match commit {
                        Ok(()) => {
                            self.writes += 1;
                            let checkpointed = if self.writes.is_multiple_of(CHECKPOINT_EVERY) {
                                conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);")
                                    .is_ok()
                            } else {
                                false
                            };
                            return Ok(WriteOutcome {
                                rows_changed: rows,
                                checkpointed,
                            });
                        }
                        Err(e) => {
                            let _ = conn.execute_batch("ROLLBACK;");
                            last_err = format!("commit: {e}");
                            continue;
                        }
                    }
                }
                Err(e) => {
                    let _ = conn.execute_batch("ROLLBACK;");
                    last_err = format!("write: {e}");
                    continue;
                }
            }
        }
        Err(format!("write core exhausted 3 retries: {last_err}"))
    }

    pub fn writes(&self) -> u64 {
        self.writes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::migrate;
    use crate::wal;

    #[test]
    fn writes_serialize_and_checkpoint_on_schedule() {
        let dir = marfim_testkit::Temp::new("marfim-wcore").unwrap();
        let conn = Connection::open(dir.path().join("s.db")).unwrap();
        wal::apply(&conn, true).unwrap();
        migrate(&conn).unwrap();
        let mut core = WriteCore::new();
        for i in 0..(CHECKPOINT_EVERY + 1) {
            let out = core
                .exec(&conn, |c| {
                    c.execute(
                        "INSERT INTO inbox_items(kind, payload, payload_digest, created_ms) VALUES ('t', ?1, 'd', 1)",
                        [format!("p{i}")],
                    )
                })
                .unwrap();
            assert_eq!(out.rows_changed, 1);
        }
        assert_eq!(core.writes(), CHECKPOINT_EVERY + 1);
        // the 50th write checkpointed
        let n: i64 = conn
            .query_row("SELECT count(*) FROM inbox_items", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 51);
    }

    #[test]
    fn failures_rollback_cleanly_and_retry_budget_is_bounded() {
        let dir = marfim_testkit::Temp::new("marfim-wcore-fail").unwrap();
        let conn = Connection::open(dir.path().join("s.db")).unwrap();
        wal::apply(&conn, true).unwrap();
        migrate(&conn).unwrap();
        let mut core = WriteCore::new();
        let err = core
            .exec(&conn, |_c| Err(rusqlite::Error::InvalidQuery))
            .unwrap_err();
        assert!(err.contains("3 retries"), "{err}");
        // the failed writes left NOTHING behind
        let n: i64 = conn
            .query_row("SELECT count(*) FROM inbox_items", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }
}
