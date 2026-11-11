//! W6.2 durable inbox: store-first, settle-once, lease-with-reclaim —
//! the crash-recovery settlement story, proven against a REAL sqlite
//! file (reopen = a crash recovery).

use marfim_state::inbox::{digest_of, DurableInbox, ItemState, Receipt};

#[test]
fn store_first_survives_a_reopen() {
    let dir = marfim_testkit::Temp::new("marfim-inbox").unwrap();
    let db = dir.path().join("state.db");
    {
        let mut inbox = DurableInbox::open(&db).unwrap();
        assert_eq!(inbox.journal_mode(), "wal");
        let id = inbox
            .push("tool_call", r#"{"tool":"Bash","command":"echo hi"}"#, 1_000)
            .unwrap();
        assert_eq!(inbox.get(id).unwrap().state, ItemState::Pending);
    } // drop = the "crash"
    let inbox = DurableInbox::open(&db).unwrap();
    let ready = inbox.ready(2_000);
    assert_eq!(ready.len(), 1, "the item survived the crash");
    assert_eq!(ready[0].kind, "tool_call");
}

#[test]
fn lease_then_settle_is_exactly_once() {
    let mut inbox = DurableInbox::open_in_memory().unwrap();
    let id = inbox.push("tool_call", r#"{"x":1}"#, 1_000).unwrap();
    let lease = inbox.claim(id, "worker-a", 2_000).unwrap();
    assert_eq!(lease.holder, "worker-a");
    assert!(lease.expires_ms > 2_000);
    let err = inbox.claim(id, "worker-b", 3_000).unwrap_err();
    assert!(err.contains("leased by another holder"), "{err}");
    let r1 = inbox
        .settle(
            id,
            Receipt {
                execution_id: "exec-1".into(),
                sequence: id,
                payload_digest: digest_of(r#"{"x":1}"#),
                outcome: "ok".into(),
            },
            4_000,
        )
        .unwrap();
    assert_eq!(r1.outcome, "ok");
    let r2 = inbox
        .settle(
            id,
            Receipt {
                execution_id: "exec-1".into(),
                sequence: id,
                payload_digest: digest_of(r#"{"x":1}"#),
                outcome: "ok-again".into(),
            },
            5_000,
        )
        .unwrap();
    assert_eq!(r2.outcome, "ok", "the first receipt wins");
    assert_eq!(inbox.get(id).unwrap().state, ItemState::Settled);
}

#[test]
fn expired_leases_are_reclaimable() {
    let mut inbox = DurableInbox::open_in_memory().unwrap();
    let id = inbox.push("tool_call", r#"{"x":2}"#, 1_000).unwrap();
    let _ = inbox.claim(id, "worker-a", 2_000).unwrap();
    let later = 2_000 + 301_000;
    let ready = inbox.ready(later);
    assert_eq!(ready.len(), 1, "expired lease shows as ready");
    let lease = inbox.claim(id, "worker-b", later).unwrap();
    assert_eq!(lease.holder, "worker-b", "dead holder reclaimed");
}

#[test]
fn digest_mismatch_refuses_settlement() {
    let mut inbox = DurableInbox::open_in_memory().unwrap();
    let id = inbox.push("tool_call", r#"{"x":3}"#, 1_000).unwrap();
    let err = inbox
        .settle(
            id,
            Receipt {
                execution_id: "exec-9".into(),
                sequence: id,
                payload_digest: "deadbeef".into(),
                outcome: "ok".into(),
            },
            2_000,
        )
        .unwrap_err();
    assert!(err.contains("disagrees"), "{err}");
}

#[test]
fn settled_items_leave_the_ready_queue() {
    let mut inbox = DurableInbox::open_in_memory().unwrap();
    let a = inbox.push("tool_call", "a", 1_000).unwrap();
    let b = inbox.push("tool_call", "b", 1_100).unwrap();
    let _ = inbox.claim(a, "w", 2_000).unwrap();
    let _ = inbox
        .settle(
            a,
            Receipt {
                execution_id: "e-a".into(),
                sequence: a,
                payload_digest: digest_of("a"),
                outcome: "ok".into(),
            },
            3_000,
        )
        .unwrap();
    // a is settled (gone); b is still PENDING (present); a live-leased
    // item would NOT be ready either.
    let ready = inbox.ready(4_000);
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].id, b);
    assert_eq!(inbox.get(a).unwrap().state, ItemState::Settled);
}
