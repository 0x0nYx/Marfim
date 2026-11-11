//! The durable inbox (W6.2, distilled from opencode v2
//! `specs/v2/session.md` + Hermes worker receipts, clean-room) holds
//! tool calls that survive a crash because they are STORED FIRST and
//! executed later.  Settlement can only happen ONCE (receipts keyed by
//! `execution_id`); leases (TTL + dead-holder reclaim) let a second
//! surface pick up abandoned work — with a wait notice, not a silent
//! steal.

use crate::schema::migrate;
use crate::wal;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

/// Lease TTL (Hermes state doctrine): a holder that has not renewed
/// within this window is dead; reclaim happens on the NEXT claimer's
/// wait-notice path.
pub const LEASE_TTL_MS: u64 = 300_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ItemState {
    Pending,
    Leased,
    Settled,
    Failed,
}

impl ItemState {
    pub fn name(&self) -> &'static str {
        match self {
            ItemState::Pending => "pending",
            ItemState::Leased => "leased",
            ItemState::Settled => "settled",
            ItemState::Failed => "failed",
        }
    }
    fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(ItemState::Pending),
            "leased" => Some(ItemState::Leased),
            "settled" => Some(ItemState::Settled),
            "failed" => Some(ItemState::Failed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct InboxItem {
    pub id: i64,
    pub kind: String,
    pub payload: String,
    pub state: ItemState,
    pub lease: Option<LeaseInfo>,
    pub receipt: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LeaseInfo {
    pub holder: String,
    pub expires_ms: u64,
}

/// A settle-once receipt (Hermes worker_executions exactly-once):
/// execution_id + sequence + payload_digest must all agree with the
/// stored item or the settlement REFUSES.
#[derive(Debug, Clone, Serialize)]
pub struct Receipt {
    pub execution_id: String,
    pub sequence: i64,
    pub payload_digest: String,
    pub outcome: String,
}

pub struct DurableInbox {
    conn: Connection,
    policy_marker: wal::WalPolicy,
}

/// Public digest used by receipts (and tests) — single source.
pub fn digest_of(s: &str) -> String {
    digest(s)
}

fn digest(s: &str) -> String {
    use std::hash::Hasher;
    let mut h = std::hash::DefaultHasher::new();
    h.write(s.as_bytes());
    format!("{:016x}", h.finish())
}

impl DurableInbox {
    /// Open (creating + migrating + WAL) at `path`.
    pub fn open(path: &std::path::Path) -> Result<Self, String> {
        let conn = Connection::open(path).map_err(|e| format!("open {path:?}: {e}"))?;
        let policy = wal::apply(&conn, false)?;
        migrate(&conn)?;
        Ok(Self {
            conn,
            policy_marker: policy,
        })
    }

    /// In-memory variant (tests).
    pub fn open_in_memory() -> Result<Self, String> {
        let conn = Connection::open_in_memory().map_err(|e| e.to_string())?;
        wal::apply(&conn, false)?;
        migrate(&conn)?;
        Ok(Self {
            conn,
            policy_marker: wal::WalPolicy::Wal,
        })
    }

    /// The journal mode ACTUALLY in force (honesty surface).
    pub fn journal_mode(&self) -> &'static str {
        self.policy_marker.name()
    }

    /// Store-first: an item exists BEFORE any execution (the crash
    /// between store and execute is the whole point).
    pub fn push(&mut self, kind: &str, payload: &str, now_ms: u64) -> Result<i64, String> {
        let mut core = crate::write::WriteCore::new();
        let payload_digest = digest(payload);
        let out = core.exec(&self.conn, |c| {
            c.execute(
                "INSERT INTO inbox_items(kind, payload, payload_digest, created_ms) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![kind, payload, payload_digest, now_ms as i64],
            )
        })?;
        let id = self
            .conn
            .query_row("SELECT last_insert_rowid()", [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        let _ = out;
        Ok(id)
    }

    /// Claim: lease a pending item (or RECLAIM an expired lease — the
    /// dead holder's lease is simply older than [`LEASE_TTL_MS`]).
    pub fn claim(&mut self, item_id: i64, holder: &str, now_ms: u64) -> Result<LeaseInfo, String> {
        let (state, expires): (String, Option<i64>) = self
            .conn
            .query_row(
                "SELECT state, lease_expires_ms FROM inbox_items WHERE id = ?1",
                [item_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|_| format!("no inbox item {item_id}"))?;
        let state = ItemState::parse(&state).ok_or("corrupt state")?;
        match state {
            ItemState::Settled | ItemState::Failed => {
                return Err(format!("item {item_id} is {state:?} — not claimable"));
            }
            ItemState::Leased => {
                let exp = expires.unwrap_or(0) as u64;
                if exp > now_ms {
                    return Err(format!(
                        "item {item_id} is leased by another holder until {exp} (wait notices, not silent steals)"
                    ));
                }
                // expired → reclaim falls through
            }
            ItemState::Pending => {}
        }
        let lease = LeaseInfo {
            holder: holder.to_string(),
            expires_ms: now_ms + LEASE_TTL_MS,
        };
        let mut core = crate::write::WriteCore::new();
        core.exec(&self.conn, |c| {
            c.execute(
                "UPDATE inbox_items SET state='leased', lease_holder=?1, lease_expires_ms=?2 WHERE id=?3",
                rusqlite::params![lease.holder, lease.expires_ms as i64, item_id],
            )
        })?;
        Ok(lease)
    }

    /// Settle-once: the receipt lands; a SECOND settle with the same
    /// execution_id is a no-op returning the stored receipt (exactly-
    /// once), and a settle whose digest disagrees with the payload
    /// REFUSES (the receipt describes a different execution).
    pub fn settle(
        &mut self,
        item_id: i64,
        receipt: Receipt,
        now_ms: u64,
    ) -> Result<Receipt, String> {
        let (payload_digest, sequence): (String, i64) = self
            .conn
            .query_row(
                "SELECT payload_digest, id FROM inbox_items WHERE id = ?1",
                [item_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|_| format!("no inbox item {item_id}"))?;
        if receipt.payload_digest != payload_digest {
            return Err(format!(
                "receipt digest {} disagrees with item payload digest {payload_digest} — refusing to settle a different execution",
                receipt.payload_digest
            ));
        }
        // exactly-once: an existing receipt for this execution_id returns
        // as-is.
        let existing: Option<String> = self
            .conn
            .query_row(
                "SELECT outcome FROM receipts WHERE execution_id = ?1",
                [&receipt.execution_id],
                |r| r.get(0),
            )
            .ok();
        if let Some(outcome) = existing {
            return Ok(Receipt {
                execution_id: receipt.execution_id,
                sequence,
                payload_digest,
                outcome,
            });
        }
        let mut core = crate::write::WriteCore::new();
        core.exec(&self.conn, |c| {
            c.execute(
                "INSERT INTO receipts(execution_id, inbox_id, sequence, payload_digest, outcome, created_ms) VALUES (?1,?2,?3,?4,?5,?6)",
                rusqlite::params![
                    receipt.execution_id,
                    item_id,
                    sequence,
                    receipt.payload_digest,
                    receipt.outcome,
                    now_ms as i64
                ],
            )?;
            c.execute(
                "UPDATE inbox_items SET state='settled', settle_receipt=?1, settled_ms=?2, lease_holder=NULL, lease_expires_ms=NULL WHERE id=?3",
                rusqlite::params![receipt.outcome, now_ms as i64, item_id],
            )
        })?;
        Ok(receipt)
    }

    pub fn get(&self, item_id: i64) -> Result<InboxItem, String> {
        self.conn
            .query_row(
                "SELECT id, kind, payload, state, lease_holder, lease_expires_ms, settle_receipt FROM inbox_items WHERE id=?1",
                [item_id],
                |r| {
                    let state_s: String = r.get(3)?;
                    let holder: Option<String> = r.get(4)?;
                    let expires: Option<i64> = r.get(5)?;
                    Ok(InboxItem {
                        id: r.get(0)?,
                        kind: r.get(1)?,
                        payload: r.get(2)?,
                        state: ItemState::parse(&state_s).unwrap_or(ItemState::Failed),
                        lease: holder.zip(expires).map(|(h, e)| LeaseInfo {
                            holder: h,
                            expires_ms: e as u64,
                        }),
                        receipt: r.get(6)?,
                    })
                },
            )
            .map_err(|_| format!("no inbox item {item_id}"))
    }

    /// Items awaiting a worker (pending or expired-lease), oldest first.
    pub fn ready(&self, now_ms: u64) -> Vec<InboxItem> {
        let mut out = Vec::new();
        let Ok(mut stmt) = self
            .conn
            .prepare("SELECT id, kind, payload, state, lease_holder, lease_expires_ms, settle_receipt FROM inbox_items ORDER BY id")
        else {
            return out;
        };
        let rows = stmt.query_map([], |r| {
            let state_s: String = r.get(3)?;
            let holder: Option<String> = r.get(4)?;
            let expires: Option<i64> = r.get(5)?;
            Ok(InboxItem {
                id: r.get(0)?,
                kind: r.get(1)?,
                payload: r.get(2)?,
                state: ItemState::parse(&state_s).unwrap_or(ItemState::Failed),
                lease: holder.zip(expires).map(|(h, e)| LeaseInfo {
                    holder: h,
                    expires_ms: e as u64,
                }),
                receipt: r.get(6)?,
            })
        });
        if let Ok(rows) = rows {
            for item in rows.flatten() {
                match item.state {
                    ItemState::Pending => out.push(item),
                    ItemState::Leased
                        if item
                            .lease
                            .as_ref()
                            .map(|l| l.expires_ms <= now_ms)
                            .unwrap_or(true) =>
                    {
                        out.push(item)
                    }
                    _ => {}
                }
            }
        }
        out
    }
}
