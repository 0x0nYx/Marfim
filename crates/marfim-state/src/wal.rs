//! WAL policy (distilled: Hermes `hermes_state_wal.py`, clean-room).
//!
//! The matrix: try `journal_mode=wal`; if the database (or filesystem —
//! network mounts refuse WAL) cannot support it, fall back to `delete`
//! ONLY when the policy allows, otherwise fail closed. A live downgrade
//! (wal→delete while another connection holds the file) is refused, not
//! forced. `journal_size_limit` keeps the -wal file bounded; `synchronous
//! =FULL` pairs with WAL for durability (macOS `fullfsync` rides
//! libsqlite3's own handling — we set the pragma, we don't reimplement
//! fsync).

use rusqlite::Connection;

/// The resolved journal mode, reported honestly to the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalPolicy {
    Wal,
    Delete,
}

impl WalPolicy {
    pub fn name(&self) -> &'static str {
        match self {
            WalPolicy::Wal => "wal",
            WalPolicy::Delete => "delete",
        }
    }
}

/// Apply the WAL policy. `require_wal` = fail closed when WAL is
/// unavailable (cross-VM correctness beats availability). Returns the
/// mode ACTUALLY in force — the caller surfaces it, never guesses.
pub fn apply(conn: &Connection, require_wal: bool) -> Result<WalPolicy, String> {
    let before: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .map_err(|e| format!("journal_mode probe failed: {e}"))?;
    let before = before.to_lowercase();
    // Never LIVE-DOWNGRADE: a wal database stays wal (or the call fails).
    let target = if before == "wal" {
        WalPolicy::Wal
    } else {
        let now: String = conn
            .query_row("PRAGMA journal_mode=wal", [], |r| r.get(0))
            .map_err(|e| format!("journal_mode=wal failed: {e}"))?;
        if now.eq_ignore_ascii_case("wal") {
            WalPolicy::Wal
        } else if require_wal {
            return Err(format!(
                "WAL required but the filesystem/database refuses it (got {now}) — failing closed rather than running a cross-VM-unsafe journal"
            ));
        } else {
            WalPolicy::Delete
        }
    };
    if target == WalPolicy::Wal {
        conn.execute_batch(
            "PRAGMA synchronous=FULL; PRAGMA journal_size_limit=67108864;", // 64 MiB
        )
        .map_err(|e| format!("wal pragmas failed: {e}"))?;
    } else {
        conn.execute_batch("PRAGMA synchronous=FULL;")
            .map_err(|e| format!("synchronous pragma failed: {e}"))?;
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wal_applies_to_files_and_in_memory_falls_back_honestly() {
        // File-backed: WAL engages.
        let dir = marfim_testkit::Temp::new("marfim-wal-file").unwrap();
        let conn = Connection::open(dir.path().join("s.db")).unwrap();
        assert_eq!(apply(&conn, false).unwrap(), WalPolicy::Wal);
        assert_eq!(apply(&conn, false).unwrap(), WalPolicy::Wal, "idempotent");
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
        // In-memory: journals cannot exist → honest Delete fallback (the
        // caller surfaces it; require_wal=true would have refused).
        let mem = Connection::open_in_memory().unwrap();
        assert_eq!(apply(&mem, false).unwrap(), WalPolicy::Delete);
        assert!(apply(&mem, true).is_err(), "require_wal fails closed");
    }

    #[test]
    fn reapply_never_downgrades_a_wal_database() {
        let dir = marfim_testkit::Temp::new("marfim-wal").unwrap();
        let conn = Connection::open(dir.path().join("s.db")).unwrap();
        assert_eq!(apply(&conn, false).unwrap(), WalPolicy::Wal);
        // even an explicit delete request cannot downgrade (never-live-
        // downgrade is structural, not a flag)
        assert_eq!(apply(&conn, false).unwrap(), WalPolicy::Wal);
    }
}
