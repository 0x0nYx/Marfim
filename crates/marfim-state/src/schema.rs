//! Schema + migrations (forward-only, user_version-stamped).

use rusqlite::Connection;

/// Current schema version.
pub const SCHEMA_VERSION: i64 = 2;

/// Apply pending migrations. Forward-only: version stamps move UP; a
/// database NEWER than this build refuses (never a silent downgrade of
/// someone's durable state).
pub fn migrate(conn: &Connection) -> Result<i64, String> {
    let v: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| format!("user_version probe: {e}"))?;
    if v > SCHEMA_VERSION {
        return Err(format!(
            "database schema v{v} is NEWER than this build (v{SCHEMA_VERSION}) — refusing to open durable state with an older binary"
        ));
    }
    if v < 1 {
        conn.execute_batch(SCHEMA_V1)
            .map_err(|e| format!("migration v1 failed: {e}"))?;
        conn.pragma_update(None, "user_version", 1)
            .map_err(|e| format!("user_version stamp: {e}"))?;
    }
    if v < 2 {
        conn.execute_batch(SCHEMA_V2)
            .map_err(|e| format!("migration v2 failed: {e}"))?;
        conn.pragma_update(None, "user_version", 2)
            .map_err(|e| format!("user_version stamp: {e}"))?;
    }
    Ok(SCHEMA_VERSION)
}

const SCHEMA_V1: &str = r#"
-- the durable inbox: settle-once tool calls (opencode v2 spec)
CREATE TABLE IF NOT EXISTS inbox_items (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    kind        TEXT NOT NULL,               -- e.g. tool_call
    payload     TEXT NOT NULL,               -- canonical JSON
    payload_digest TEXT NOT NULL,
    state       TEXT NOT NULL DEFAULT 'pending',  -- pending|leased|settled|failed
    lease_holder TEXT,
    lease_expires_ms INTEGER,
    settle_receipt TEXT,                     -- execution receipt JSON
    settled_ms  INTEGER,
    created_ms  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_inbox_state ON inbox_items(state);

-- receipts: exactly-once execution proofs (execution_id unique)
CREATE TABLE IF NOT EXISTS receipts (
    execution_id   TEXT PRIMARY KEY,
    inbox_id       INTEGER NOT NULL REFERENCES inbox_items(id),
    sequence       INTEGER NOT NULL,
    payload_digest TEXT NOT NULL,
    outcome        TEXT NOT NULL,
    created_ms     INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_receipts_exec ON receipts(execution_id);

-- session tree projection (W6.3): parentId + titles, UUIDv7 ids as TEXT
CREATE TABLE IF NOT EXISTS session_nodes (
    id          TEXT PRIMARY KEY,            -- UUIDv7
    parent_id   TEXT REFERENCES session_nodes(id),
    title       TEXT NOT NULL,
    created_ms  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_nodes_parent ON session_nodes(parent_id);
"#;

/// W10.2: the ordered durable effect queue + outbox, and the async
/// delegation ledger (both store-first; crash-recovery semantics live
/// with their modules).
const SCHEMA_V2: &str = r#"
-- the ordered outbox: effects run STRICTLY in insertion order; an
-- effect behind a pending head cannot run (ordered execution is the
-- contract, receipts the proof)
CREATE TABLE IF NOT EXISTS effects (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    kind        TEXT NOT NULL,
    payload     TEXT NOT NULL,
    payload_digest TEXT NOT NULL,
    state       TEXT NOT NULL DEFAULT 'queued',  -- queued|claimed|done|dead
    attempts    INTEGER NOT NULL DEFAULT 0,
    claimed_by  TEXT,
    receipt     TEXT,
    done_ms     INTEGER,
    created_ms  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_effects_state ON effects(state);

-- async delegation dispatch rows (Hermes async_delegation doctrine):
-- replay window 48h, attempts <= 8, orphan sweep returns expired
-- dispatched rows to the queue
CREATE TABLE IF NOT EXISTS delegation (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    prompt      TEXT NOT NULL,
    target      TEXT NOT NULL,               -- agent persona name
    state       TEXT NOT NULL DEFAULT 'queued',  -- queued|dispatched|replayed|done|expired
    attempts    INTEGER NOT NULL DEFAULT 0,
    holder      TEXT,
    lease_expires_ms INTEGER,
    result      TEXT,
    created_ms  INTEGER NOT NULL,
    settled_ms  INTEGER
);
CREATE INDEX IF NOT EXISTS idx_delegation_state ON delegation(state);
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_is_idempotent_and_forward_only() {
        let dir = marfim_testkit::Temp::new("marfim-schema").unwrap();
        let conn = Connection::open(dir.path().join("s.db")).unwrap();
        assert_eq!(migrate(&conn).unwrap(), 2);
        assert_eq!(migrate(&conn).unwrap(), 2, "second apply is a no-op");
        // tables exist
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN ('inbox_items','receipts','session_nodes','effects','delegation')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 5);
        // a NEWER database refuses
        let conn2 = Connection::open_in_memory().unwrap();
        conn2.pragma_update(None, "user_version", 99).unwrap();
        let err = migrate(&conn2).unwrap_err();
        assert!(err.contains("NEWER"), "{err}");
    }
}
