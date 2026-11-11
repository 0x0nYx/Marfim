//! Gate G11 (W2.2): the protocol fixtures are the CONTRACT. The bytes in
//! `fixtures/protocol/*.json` must regenerate byte-identically from
//! `marfim_gateway::protocol::fixture_bytes()` — a wording drift, a new
//! field, a reordered key, anything, fails this test and the gate.
//!
//! Regenerate with:
//!   MARFIM_REGEN_FIXTURES=1 cargo test -p marfim-gateway \
//!     --test protocol_fixtures regenerate -- --ignored

use std::path::PathBuf;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/protocol")
}

#[test]
fn fixtures_are_byte_identical_to_the_contract() {
    let fx = marfim_gateway::protocol::fixture_bytes();
    assert!(!fx.is_empty(), "fixture set must not be empty");
    for (name, bytes) in &fx {
        let path = fixture_dir().join(name);
        let on_disk = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!(
                "fixture {} is missing ({}); regenerate with MARFIM_REGEN_FIXTURES=1 \
                 cargo test -p marfim-gateway --test protocol_fixtures regenerate -- --ignored",
                path.display(),
                e
            )
        });
        assert_eq!(
            &on_disk, bytes,
            "fixture {name} drifted from the protocol contract; regenerate with \
             MARFIM_REGEN_FIXTURES=1 cargo test -p marfim-gateway --test protocol_fixtures \
             regenerate -- --ignored"
        );
    }
}

/// The exact error literals the server answers with — searched in the
/// serve source so the fixtures and the implementation cannot drift.
#[test]
fn error_fixture_bodies_appear_verbatim_in_serve_source() {
    let src =
        std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/serve.rs"))
            .expect("serve.rs readable");
    let fx = marfim_gateway::protocol::fixture_bytes();
    let errors = fx.get("errors.json").expect("errors fixture");
    let v: serde_json::Value = serde_json::from_str(errors).expect("errors fixture parses");
    for (kind, entry) in v.as_object().expect("errors object") {
        let body = entry["body"]["error"].as_str().unwrap_or_default();
        assert!(!body.is_empty(), "error fixture {kind} has no body string");
        // Some bodies embed a runtime number (e.g. the configured body
        // cap) — the stable prefix before the first digit must still be
        // verbatim in the source.
        let prefix = body
            .find(|c: char| c.is_ascii_digit())
            .map(|i| body[..i].to_string())
            .unwrap_or_else(|| body.to_string());
        let logical = prefix.replace("\\\"", "\"");
        assert!(
            src.contains(&logical),
            "serve.rs no longer produces the {kind} error body verbatim: {body}"
        );
    }
}

#[test]
#[ignore = "regeneration helper: MARFIM_REGEN_FIXTURES=1 cargo test -p marfim-gateway --test protocol_fixtures regenerate -- --ignored"]
fn regenerate() {
    if std::env::var("MARFIM_REGEN_FIXTURES").as_deref() != Ok("1") {
        panic!("set MARFIM_REGEN_FIXTURES=1 to regenerate the fixtures");
    }
    let dir = fixture_dir();
    std::fs::create_dir_all(&dir).expect("create fixtures/protocol");
    for (name, bytes) in marfim_gateway::protocol::fixture_bytes() {
        let path = dir.join(&name);
        std::fs::write(&path, bytes).expect("write fixture");
        println!("wrote {}", path.display());
    }
}
