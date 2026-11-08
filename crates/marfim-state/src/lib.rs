//! marfim-state (W6.1/W6.2, distilled: Hermes `hermes_state*.py` + opencode
//! v2 `specs/v2/session.md`, clean-room).
//!
//! Four pieces, all fail-closed:
//! - [`wal`] — the WAL policy module: journal_mode wal→delete fallback
//!   matrix, journal_size_limit, never-live-downgrade, fail-closed probe.
//! - [`write`] — the BEGIN IMMEDIATE write core (bounded retries, one
//!   mid-transaction checkpoint every N writes).
//! - [`schema`] — migrations (user_version-stamped, forward-only).
//! - [`inbox`] — the durable inbox: settle-once tool calls with receipts
//!   (execution_id + sequence + payload_digest exactly-once) and leases
//!   (TTL + dead-holder reclaim).
//!
//! `#![forbid(unsafe_code)]` holds; rusqlite owns every unsafe byte.

#![forbid(unsafe_code)]

pub mod inbox;
pub mod schema;
pub mod wal;
pub mod write;

pub use inbox::{DurableInbox, InboxItem, ItemState, LeaseInfo};
pub use write::WriteCore;
