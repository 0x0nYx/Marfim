//! Protocol v1 contract (W2.2): the canonical wire shapes `marfim serve`
//! speaks, as regenerable fixtures.
//!
//! `fixture_bytes()` is the SINGLE SOURCE: every JSON file under
//! `fixtures/protocol/` must regenerate byte-identically from it (gate
//! G11, test `protocol_fixtures`). The serve loop reuses
//! [`sse_data_frame`] for its data frames, so the fixture bytes and the
//! live bytes cannot drift apart. docs/PROTOCOL.md documents the same
//! contract in prose.
//!
//! Determinism: BTreeMap ordering + `serde_json::to_string_pretty` +
//! one trailing newline. No wall-clock, no HashMap iteration.

use marfim_core::events::Event;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// Protocol version pinned by docs/PROTOCOL.md and the fixtures.
pub const PROTOCOL_VERSION: &str = "1";

/// The exact bytes of a seq-stamped SSE data frame. The serve loop's
/// `sse_data` delegates here — one source, byte-identical everywhere.
pub fn sse_data_frame(seq: u64, line: &str) -> String {
    format!("id: {seq}\ndata: {line}\n\n")
}

/// Keepalive comment frame (no `id:` — comments are not events).
pub fn sse_ping_frame() -> String {
    ": ping\n\n".to_string()
}

/// The exact bytes of an id-less SSE control frame (terminal markers on
/// the ring stream: `stream_deadline`, `lagged`).
pub fn sse_control_frame(line: &str) -> String {
    format!("data: {line}\n\n")
}

fn pretty(v: &Value) -> String {
    let mut s = serde_json::to_string_pretty(v).unwrap_or_else(|_| "{}".to_string());
    s.push('\n');
    s
}

/// One canonical bus event of every kind, serialized through the REAL
/// `Event::to_json_line()` — the same bytes the ring stores and SSE
/// delivers. Field order is declaration order (serde), not sorted.
pub fn event_lines() -> BTreeMap<String, String> {
    let events: Vec<Event> = vec![
        Event::SessionStarted {
            session_id: "s1762531200000".to_string(),
            cwd: "/workspace".to_string(),
        },
        Event::TurnStarted { turn: 1 },
        Event::ModelSelected {
            role: "primary".to_string(),
            model: "claude-sonnet-4-5".to_string(),
            why: "chain head".to_string(),
        },
        Event::AssistantMessage {
            text: "Done.".to_string(),
        },
        Event::Thinking {
            text: "considering".to_string(),
        },
        Event::StreamDelta {
            kind: "text".to_string(),
            text: "he".to_string(),
        },
        Event::ToolCallStarted {
            id: "t1".to_string(),
            tool: "Bash".to_string(),
            args: json!({"command": "echo hi"}),
        },
        Event::ToolCallResult {
            id: "t1".to_string(),
            tool: "Bash".to_string(),
            output: "hi".to_string(),
            is_error: false,
            duration_ms: 12,
        },
        Event::ApprovalRequested {
            approval_id: "a1".to_string(),
            tool: "Bash".to_string(),
            summary: "echo hi".to_string(),
        },
        Event::ApprovalResolved {
            approval_id: "a1".to_string(),
            decision: "allow".to_string(),
        },
        Event::Usage {
            role: "primary".to_string(),
            model: "claude-sonnet-4-5".to_string(),
            input_tokens: 120,
            output_tokens: 45,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            cost_usd: None,
        },
        Event::Compaction {
            before_tokens: 90_000,
            after_tokens: 21_000,
        },
        Event::VerifyStarted {
            commands: vec!["cargo test".to_string()],
        },
        Event::VerifyFinished {
            passed: true,
            output: "test result: ok".to_string(),
        },
        Event::TurnEnded {
            turn: 1,
            stop_reason: "end_turn".to_string(),
        },
        Event::Error {
            message: "provider unreachable".to_string(),
            fatal: false,
        },
        Event::FacultyHook {
            faculty: "learning".to_string(),
            hook: "post_step".to_string(),
            detail: json!({"drift": 0.01}),
        },
        Event::CacheWarm {
            ok: false,
            model: "claude-sonnet-4-5".to_string(),
        },
        Event::Actuation {
            faculty: "learning".to_string(),
            kind: "model_select".to_string(),
            before: json!({"model": "a"}),
            after: json!({"model": "b"}),
            evidence: json!({"regret": 3}),
        },
    ];
    let mut out = BTreeMap::new();
    for ev in events {
        out.insert(ev.kind().to_string(), ev.to_json_line());
    }
    out
}

/// The canonical route table: request shape and real response shape per
/// route. Responses are built from the same serializers the server uses
/// (SessionSnapshot/RewindOutcome/ForkOutcome derives, the handler
/// envelope, the error literals in serve.rs).
fn routes() -> BTreeMap<String, Value> {
    let snapshot = marfim_core::session::SessionSnapshot {
        session_id: "s1762531200000".to_string(),
        turn: 3,
        history_len: 9,
        journal_bytes: 4096,
        pending_steers: 0,
        pending_approvals: 1,
        turn_running: false,
        cancelled: false,
        denied_tools: 1,
        executed_tools: 7,
    };
    let mut r = BTreeMap::new();
    r.insert(
        "healthz".to_string(),
        json!({
            "method": "GET", "path": "/healthz",
            "request": null,
            "status": 200,
            "response": {"ok": true, "uptime_ms": 120000, "version": marfim_core::VERSION},
            "note": "liveness; still behind the bearer token"
        }),
    );
    r.insert(
        "list_sessions".to_string(),
        json!({
            "method": "GET", "path": "/v1/sessions",
            "request": null,
            "status": 200,
            "response": {"sessions": [snapshot]},
        }),
    );
    r.insert(
        "create_session".to_string(),
        json!({
            "method": "POST", "path": "/v1/sessions",
            "request": {"title": "refactor", "resume": null},
            "status": 201,
            "response": snapshot,
            "note": "resume names (or empty = latest) a journal to reopen"
        }),
    );
    r.insert(
        "get_session".to_string(),
        json!({
            "method": "GET", "path": "/v1/sessions/:id",
            "request": null,
            "status": 200,
            "response": snapshot,
        }),
    );
    r.insert(
        "close_session".to_string(),
        json!({
            "method": "DELETE", "path": "/v1/sessions/:id",
            "request": null,
            "status": 200,
            "response": {"closed": "s1762531200000"},
            "note": "in-flight turn is cancelled first"
        }),
    );
    r.insert(
        "prompt".to_string(),
        json!({
            "method": "POST", "path": "/v1/prompt",
            "request": {"prompt": "run the tests", "session_id": null},
            "status": 200,
            "response": "text/event-stream",
            "note": "one SSE stream per turn; frames id-stamped by a per-stream counter; terminal frame {\"event\":\"turn_end\",\"stop_reason\":\"...\"}; session_id omitted = default session"
        }),
    );
    r.insert(
        "prompt_scoped".to_string(),
        json!({
            "method": "POST", "path": "/v1/sessions/:id/prompt",
            "request": {"prompt": "run the tests"},
            "status": 200,
            "response": "text/event-stream",
        }),
    );
    r.insert(
        "steer".to_string(),
        json!({
            "method": "POST", "path": "/v1/sessions/:id/steer",
            "request": {"text": "stop, use rg instead"},
            "status": 202,
            "response": {"queued": true},
            "note": "drains only after a full tool-result batch (INV-02); no-op when idle"
        }),
    );
    r.insert(
        "cancel".to_string(),
        json!({
            "method": "POST", "path": "/v1/sessions/:id/cancel",
            "request": null,
            "status": 200,
            "response": {"cancelled": true},
            "note": "cooperative; replies once the turn has unwound"
        }),
    );
    r.insert(
        "undo".to_string(),
        json!({
            "method": "POST", "path": "/v1/sessions/:id/undo",
            "request": null,
            "status": 200,
            "response": {
                "dropped_records": 4, "dropped_bytes": 512,
                "history_len": 5, "journal_bytes": 2048
            },
            "error_status": 409,
            "note": "409 when there is nothing to undo"
        }),
    );
    r.insert(
        "rewind".to_string(),
        json!({
            "method": "POST", "path": "/v1/sessions/:id/rewind",
            "request": {"turns": 2},
            "status": 200,
            "response": {
                "dropped_records": 8, "dropped_bytes": 1024,
                "history_len": 3, "journal_bytes": 1024
            },
            "error_status": 409,
        }),
    );
    r.insert(
        "fork".to_string(),
        json!({
            "method": "POST", "path": "/v1/sessions/:id/fork",
            "request": null,
            "status": 201,
            "response": {
                "fork_session": "s1762531209999",
                "fork_path": "/home/u/.marfim/sessions/s1762531209999.jsonl"
            },
            "note": "journal copy carries a Fork provenance record"
        }),
    );
    r.insert(
        "session_events".to_string(),
        json!({
            "method": "GET", "path": "/v1/sessions/:id/events",
            "request": null,
            "status": 200,
            "response": "text/event-stream",
            "note": "ring-backed: Last-Event-ID (header or ?lastEventId=) replays exactly the missed seqs"
        }),
    );
    r.insert(
        "events".to_string(),
        json!({
            "method": "GET", "path": "/v1/events",
            "request": null,
            "status": 200,
            "response": "text/event-stream",
            "note": "the default session's ring; pings-only until a session exists"
        }),
    );
    r.insert(
        "approvals".to_string(),
        json!({
            "method": "GET", "path": "/v1/approvals",
            "request": null,
            "status": 200,
            "response": {"approvals": [{"id": "a1", "tool": "Bash", "summary": "echo hi"}]},
        }),
    );
    r.insert(
        "resolve_approval".to_string(),
        json!({
            "method": "POST", "path": "/v1/approvals/:id",
            "request": {"decision": "allow|allow_once|allow_always|deny"},
            "status": 200,
            "response": {"resolved": "a1", "decision": "allow"},
            "error_status": 404,
        }),
    );
    r.insert(
        "config".to_string(),
        json!({
            "method": "GET", "path": "/v1/config",
            "request": null,
            "status": 200,
            "response": {
                "provider": "anthropic", "model": "claude-sonnet-4-5",
                "small_model": "claude-haiku-4-5", "routing": "chain",
                "permission_mode": "Default", "sandbox_level": "Enforced",
                "budgeted": false, "compact_threshold_tokens": 160000,
                "bash_default_timeout_ms": 120000, "cwd": "/workspace",
                "mcp_servers": [], "secrets": "redacted"
            },
            "note": "credential material is structurally absent (redacted at the app layer)"
        }),
    );
    r.insert(
        "fs_read".to_string(),
        json!({
            "method": "POST", "path": "/v1/fs/read",
            "request": {"path": "src/main.rs", "offset": null, "limit": null},
            "status": 200,
            "response": {"tool": "Read", "output": "     1\tpub fn main() {\n     2\t}\n"},
            "error_status": 400,
            "note": "runs through the REAL tool host: hooks -> permission -> sandbox -> dispatch"
        }),
    );
    r.insert(
        "fs_list".to_string(),
        json!({
            "method": "POST", "path": "/v1/fs/list",
            "request": {"path": "src"},
            "status": 200,
            "response": {"tool": "LS", "output": "main.rs\nlib.rs\n"},
            "error_status": 400,
        }),
    );
    r.insert(
        "fs_write".to_string(),
        json!({
            "method": "POST", "path": "/v1/fs/write",
            "request": {"path": "notes/a.txt", "content": "hello"},
            "status": 200,
            "response": {"tool": "Write", "output": "wrote notes/a.txt (5 bytes)"},
            "error_status": 400,
            "note": "mutations flow through the PermissionEngine; unattended asks are denied (D14)"
        }),
    );
    r.insert(
        "git_status".to_string(),
        json!({
            "method": "GET", "path": "/v1/git/status",
            "request": null,
            "status": 200,
            "response": {"backend": "cli", "dirty": true, "modified": ["src/main.rs"]},
            "error_status": 400,
            "note": "read-only; 400 outside a repository"
        }),
    );
    r.insert(
        "git_diff".to_string(),
        json!({
            "method": "POST", "path": "/v1/git/diff",
            "request": {},
            "status": 200,
            "response": {"backend": "cli", "diff": "--- a/src/main.rs\n+++ b/src/main.rs\n"},
            "error_status": 400,
        }),
    );
    r.insert(
        "git_log".to_string(),
        json!({
            "method": "POST", "path": "/v1/git/log",
            "request": {"limit": 20},
            "status": 200,
            "response": {"backend": "cli", "commits": [{"id": "79392fe", "summary": "W2.1: gateway protocol routes"}]},
            "error_status": 400,
        }),
    );
    r.insert(
        "mcp".to_string(),
        json!({
            "method": "GET", "path": "/v1/mcp",
            "request": null,
            "status": 200,
            "response": {"servers": ["fs"], "tools": ["mcp__fs__read_file"]},
        }),
    );
    // -- W3.1: terminal surface. Six routes, one injected handler; the
    // embedding wires them through the REAL Terminal tool (hooks →
    // permission → sandbox → dispatch), so an unwired gateway answers 501
    // and a wired one can never bypass the permission engine.
    r.insert(
        "terminals_spawn".to_string(),
        json!({
            "method": "POST", "path": "/v1/terminals",
            "request": {"command": "sh", "args": ["-c", "watchmaker --progress"], "cwd": "/workspace", "rows": 24, "cols": 80},
            "status": 200,
            "response": {"id": 1},
            "error_status": 400,
            "note": "spawns a PTY session; id is the terminal handle for every other terminal route",
        }),
    );
    r.insert(
        "terminals_list".to_string(),
        json!({
            "method": "GET", "path": "/v1/terminals",
            "request": null,
            "status": 200,
            "response": {"terminals": [{"id": 1, "command": "sh -c watchmaker --progress", "alive": true, "exit_code": null, "total_bytes": 4096, "pending_bytes": 0, "age_secs": 7}]},
        }),
    );
    r.insert(
        "terminals_input".to_string(),
        json!({
            "method": "POST", "path": "/v1/terminals/{id}/input",
            "request": {"text": "q"},
            "status": 200,
            "response": {"written": 1},
            "error_status": 400,
            "note": "raw bytes to the child's stdin — no shell parsing",
        }),
    );
    r.insert(
        "terminals_resize".to_string(),
        json!({
            "method": "POST", "path": "/v1/terminals/{id}/resize",
            "request": {"rows": 30, "cols": 100},
            "status": 200,
            "response": {"ok": true},
            "error_status": 400,
        }),
    );
    r.insert(
        "terminals_screen".to_string(),
        json!({
            "method": "GET", "path": "/v1/terminals/{id}/screen",
            "request": null,
            "status": 200,
            "response": {"id": 1, "alive": true, "exit_code": null, "text": "build ok\n", "cursor": [23, 8]},
            "error_status": 400,
        }),
    );
    r.insert(
        "terminals_close".to_string(),
        json!({
            "method": "DELETE", "path": "/v1/terminals/{id}",
            "request": null,
            "status": 200,
            "response": {"closed": 1},
            "error_status": 400,
        }),
    );
    r
}

/// The exact error bodies serve.rs answers with (searched by the contract
/// test so a wording drift fails the gate).
fn errors() -> BTreeMap<String, Value> {
    let mut e = BTreeMap::new();
    e.insert(
        "unauthorized".to_string(),
        json!({"status": 401, "body": {"error": "unauthorized"}}),
    );
    e.insert(
        "host_not_allowed".to_string(),
        json!({"status": 403, "body": {"error": "host not allowed (DNS-rebinding guard)"}}),
    );
    e.insert(
        "origin_not_allowed".to_string(),
        json!({"status": 403, "body": {"error": "origin not allowed"}}),
    );
    e.insert(
        "body_too_large".to_string(),
        json!({"status": 413, "body": {"error": "body exceeds 1048576 byte cap"}}),
    );
    e.insert(
        "no_such_session".to_string(),
        json!({"status": 404, "body": {"error": "no such session"}}),
    );
    e.insert(
        "no_such_approval".to_string(),
        json!({"status": 404, "body": {"error": "no pending approval with that id"}}),
    );
    e.insert(
        "not_found".to_string(),
        json!({"status": 404, "body": {"error": "not found"}}),
    );
    e.insert(
        "nothing_to_undo".to_string(),
        json!({"status": 409, "body": {"error": "nothing to undo"}}),
    );
    e.insert(
        "route_not_wired".to_string(),
        json!({"status": 501, "body": {"error": "route not wired by the embedding surface"}}),
    );
    e
}

/// The SSE frame shapes, as consumed by clients. Ring frames carry the
/// event's seq in `id:`; per-stream prompt frames carry a local counter;
/// terminal control markers on the ring stream are id-less.
fn frames() -> BTreeMap<String, String> {
    let mut f = BTreeMap::new();
    let event_line = Event::AssistantMessage {
        text: "hi".to_string(),
    }
    .to_json_line();
    f.insert(
        "ring_data_frame".to_string(),
        sse_data_frame(7, &event_line),
    );
    f.insert(
        "ring_terminal_frame".to_string(),
        sse_control_frame("{\"event\":\"stream_deadline\"}"),
    );
    f.insert(
        "ring_lag_marker".to_string(),
        sse_control_frame("{\"event\":\"lagged\",\"dropped\":3}"),
    );
    f.insert(
        "ring_resume_replay".to_string(),
        format!(
            "{}{}",
            sse_data_frame(8, &event_line),
            sse_data_frame(9, "{\"event\":\"ring_lagged\",\"dropped\":1}")
        ),
    );
    f.insert("ping".to_string(), sse_ping_frame());
    f.insert(
        "prompt_data_frame".to_string(),
        sse_data_frame(1, &event_line),
    );
    f.insert(
        "prompt_terminal_frame".to_string(),
        sse_data_frame(2, "{\"event\":\"turn_end\",\"stop_reason\":\"end_turn\"}"),
    );
    f
}

/// All fixture files: name (under fixtures/protocol/) → exact bytes.
pub fn fixture_bytes() -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    out.insert("frames.json".to_string(), pretty(&json!(frames())));
    out.insert(
        "events.json".to_string(),
        pretty(&json!({
            "protocol": PROTOCOL_VERSION,
            "note": "every bus event kind, serialized by the real Event::to_json_line(); these lines are what /v1/events delivers inside SSE data frames",
            "events": event_lines(),
        })),
    );
    out.insert("routes.json".to_string(), pretty(&json!({
        "protocol": PROTOCOL_VERSION,
        "auth": "Authorization: Bearer <token> on EVERY route; token is mandatory (generated and printed once when not supplied)",
        "routes": routes(),
    })));
    out.insert("errors.json".to_string(), pretty(&json!(errors())));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_builders_are_exact() {
        assert_eq!(sse_ping_frame(), ": ping\n\n");
        assert_eq!(sse_data_frame(3, "{\"a\":1}"), "id: 3\ndata: {\"a\":1}\n\n");
        assert_eq!(sse_control_frame("{\"b\":2}"), "data: {\"b\":2}\n\n");
    }

    #[test]
    fn every_event_kind_has_a_fixture_line() {
        // 19 kinds in the enum; the fixture must cover all of them.
        assert_eq!(event_lines().len(), 19);
    }

    #[test]
    fn fixtures_are_well_formed_and_versioned() {
        let fx = fixture_bytes();
        assert_eq!(fx.len(), 4, "frames/events/routes/errors");
        for (name, bytes) in &fx {
            let v: Value = serde_json::from_str(bytes)
                .unwrap_or_else(|e| panic!("fixture {name} is not valid JSON: {e}"));
            let obj = v
                .as_object()
                .unwrap_or_else(|| panic!("{name} not an object"));
            if name != "frames.json" && name != "errors.json" {
                assert_eq!(obj["protocol"], PROTOCOL_VERSION, "{name}");
            }
        }
    }
}
