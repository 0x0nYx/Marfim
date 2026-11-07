//! W1.4: SandboxLevel battery — every level's spawn semantics, the
//! honest attestations, and the app-uid force-ask escalation (deny
//! floors stay absolute).

#![forbid(unsafe_code)]

use marfim_core::permissions::{Decision, Mode, PermissionEngine, Rule, ToolClass};
use marfim_core::sandbox::{Sandbox, SandboxLevel};

fn rule(tool: &str, d: Decision) -> Rule {
    Rule {
        tool: tool.to_string(),
        decision: d,
        argv_prefix: None,
    }
}

#[test]
fn level_parse_is_strict() {
    assert_eq!(
        SandboxLevel::parse("enforced"),
        Some(SandboxLevel::Enforced)
    );
    assert_eq!(SandboxLevel::parse("appUid"), Some(SandboxLevel::AppUid));
    assert_eq!(SandboxLevel::parse("app-uid"), Some(SandboxLevel::AppUid));
    assert_eq!(SandboxLevel::parse("none"), Some(SandboxLevel::None));
    // unknown / empty / trick casing never parse (fail-closed)
    assert_eq!(SandboxLevel::parse("Enforced"), None);
    assert_eq!(SandboxLevel::parse(""), None);
    assert_eq!(SandboxLevel::parse("off"), None);
}

#[test]
fn default_level_is_enforced() {
    assert_eq!(Sandbox::default().level, SandboxLevel::Enforced);
    assert_eq!(Sandbox::probe().level, SandboxLevel::Enforced);
}

#[test]
fn every_level_prints_an_honest_attestation() {
    for l in [
        SandboxLevel::Enforced,
        SandboxLevel::AppUid,
        SandboxLevel::None,
    ] {
        let a = l.attestation();
        assert!(!a.is_empty());
        assert!(a.contains("sandbox:"), "{a}");
        // the honesty doctrine: courtesy language, never "secure"
        assert!(!a.to_lowercase().contains("secure"), "{a}");
    }
}

#[tokio::test]
async fn app_uid_level_spawns_without_containment_machinery() {
    // AppUid never routes through the exec-helper/unshare paths: a plain
    // spawn must work even with writable_roots configured (the level IS
    // the attestation that no kernel sandbox exists).
    let mut s = Sandbox::default().with_level(SandboxLevel::AppUid);
    s.writable_roots = vec!["/tmp".into()];
    let mut cmd = s
        .command("/bin/echo")
        .unwrap_or_else(|e| panic!("appUid must spawn plainly: {e}"));
    let out = cmd.output().await.expect("echo runs");
    assert!(out.status.success());
}

#[tokio::test]
async fn none_level_spawns_plainly_with_roots_configured() {
    let mut s = Sandbox::default().with_level(SandboxLevel::None);
    s.writable_roots = vec!["/tmp".into()];
    let mut cmd = s
        .command("/bin/echo")
        .unwrap_or_else(|e| panic!("none must spawn plainly: {e}"));
    let out = cmd.output().await.expect("echo runs");
    assert!(out.status.success());
}

#[tokio::test]
async fn enforced_level_still_fails_closed_without_helper() {
    // Enforced + roots + NO exec helper = refusal (the pre-W1.4 D4
    // contract must survive the level work).
    // force the unprobed/fail-closed state the packaging path can hit:
    let mut s = Sandbox::default().without_exec_helper();
    s.writable_roots = vec!["/tmp".into()];
    if s.fs_sandbox_available() {
        // platform can enforce; the refusal is exercised on the helper
        // error path only when it is genuinely unavailable.
        return;
    }
    let err = s
        .command("/bin/echo")
        .map(|_| ())
        .expect_err("must refuse on an unenforceable platform");
    assert!(err.contains("refusing to run"), "{err}");
}

#[test]
fn app_uid_force_ask_keeps_allow_for_readonly() {
    let (e, _rx) = PermissionEngine::new(Mode::Yolo, vec![]);
    let e = std::sync::Arc::new(e.with_force_ask_mutations(true));
    let rt = tokio::runtime::Runtime::new().unwrap_or_else(|e| panic!("rt: {e}"));
    rt.block_on(async move {
        // headless (no listener) — ReadOnly default-allow never asks
        let v = e.check("Read", ToolClass::ReadOnly, None, None, "r").await;
        assert_eq!(v.decision, Decision::Allow);
    });
}

#[tokio::test]
async fn app_uid_force_ask_converts_mutation_allows_to_ask() {
    // explicit ALLOW rule on Write + Yolo: without the flag = Allow,
    // with the flag = Ask (and with no listener = fail-closed Deny).
    let (e_always, _rx) = PermissionEngine::new(Mode::Yolo, vec![rule("Write", Decision::Allow)]);
    let v = e_always
        .check("Write", ToolClass::Edit, None, None, "w")
        .await;
    assert_eq!(v.decision, Decision::Allow);

    let (e_appuid, rx) = PermissionEngine::new(Mode::Yolo, vec![rule("Write", Decision::Allow)]);
    let e_appuid = e_appuid.with_force_ask_mutations(true);
    drop(rx); // no approval listener attached → the ask FAILS CLOSED to deny
    let v = e_appuid
        .check("Write", ToolClass::Edit, None, None, "w")
        .await;
    assert_eq!(v.decision, Decision::Deny);
    assert!(v.reason.contains("fail closed"), "{v:?}");
}

#[tokio::test]
async fn app_uid_force_ask_never_touches_denies() {
    // explicit DENY rule stays Deny under the app-uid escalation; the
    // hardline floor (tested in permissions.rs) precedes everything.
    let (e, mut rx) = PermissionEngine::new(Mode::Yolo, vec![rule("Bash", Decision::Deny)]);
    let e = e.with_force_ask_mutations(true);
    let v = e.check("Bash", ToolClass::Execute, None, None, "b").await;
    assert_eq!(v.decision, Decision::Deny);
    assert!(rx.try_recv().is_err());
}
