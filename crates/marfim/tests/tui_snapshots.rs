//! P6 snapshot tests for the ratatui TUI (TestBackend 80x24 — no real
//! processes are spawned; the session actor is never driven here).
//!
//! `marfim` is a binary-only crate, so the TUI module is compiled into this
//! test crate verbatim via `#[path]`. The two binary-only items it
//! references (`settings::{Cli, EffectiveSettings}` and `build_session`)
//! are compile-time stand-ins below; the live `run()` is never executed.
//! Every test drives the pure `TuiModel` (events in → model out) and
//! asserts on the rendered `Buffer`.

#![allow(dead_code)] // the included module's runtime side is unused in tests

#[path = "../src/tui.rs"]
mod tui;

use marfim_core::events::Event;
use marfim_core::permissions::ApprovalAnswer;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Color;
use ratatui::Terminal;
use tui::{Palette, TuiModel};

// -- compile-time stand-ins for the binary crate's items ---------------------

/// Mirrors the binary's `EffectiveSettings` for the fields the TUI reads.
pub mod settings {
    #[derive(Clone)]
    pub struct EffectiveSettings {
        pub theme: String,
        pub model: String,
        pub cwd: std::path::PathBuf,
    }
    #[derive(Clone)]
    pub struct Cli;
}

/// Mirrors the binary's `build_session` signature — never called by tests.
pub async fn build_session(
    _config: &marfim_core::config::Config,
    _settings: &settings::EffectiveSettings,
) -> (
    std::sync::Arc<marfim_core::loop_::Harness>,
    marfim_core::events::EventBus,
    marfim_tools::registry::ToolCtx,
    std::sync::Arc<marfim_core::permissions::PermissionEngine>,
    tokio::sync::mpsc::Receiver<marfim_core::permissions::ApprovalJob>,
) {
    unreachable!("snapshot tests never drive the live session runtime")
}

/// Mirrors the binary's `make_host` — the TUI's session constructor.
pub fn make_host(
    _config: &marfim_core::config::Config,
    _settings: Option<settings::EffectiveSettings>,
    _cli: settings::Cli,
    _cwd: std::path::PathBuf,
) -> std::sync::Arc<marfim_core::session_host::SessionHost> {
    std::sync::Arc::new(marfim_core::session_host::SessionHost::new(
        std::sync::Arc::new(|_req| Box::pin(async { Err("stand-in host never runs".to_string()) })),
    ))
}

// -- helpers ------------------------------------------------------------------

fn render(model: &mut TuiModel) -> Buffer {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| tui::draw(f, model)).unwrap();
    terminal.backend().buffer().clone()
}

fn text_of(buf: &Buffer) -> String {
    let mut out = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            if let Some(c) = buf.cell((x, y)) {
                out.push_str(c.symbol());
            }
        }
        out.push('\n');
    }
    out
}

fn model_with_palette(pal: Palette) -> TuiModel {
    TuiModel::new("s-test".into(), "test-model".into(), pal)
}

/// One realistic turn: streamed deltas (finalized by the authoritative
/// AssistantMessage), a tool call with result, usage, a faculty hook, a
/// second assistant step, then TurnEnded.
fn scripted() -> Vec<Event> {
    vec![
        Event::SessionStarted {
            session_id: "s-test".into(),
            cwd: "/tmp".into(),
        },
        Event::TurnStarted { turn: 1 },
        Event::StreamDelta {
            kind: "text".into(),
            text: "Reading the repo ".into(),
        },
        Event::StreamDelta {
            kind: "text".into(),
            text: "now.".into(),
        },
        Event::AssistantMessage {
            text: "Reading the repo now.".into(),
        },
        Event::ToolCallStarted {
            id: "t1".into(),
            tool: "Bash".into(),
            args: serde_json::json!({"cmd": "ls -la", "timeout_ms": 1000}),
        },
        Event::ToolCallResult {
            id: "t1".into(),
            tool: "Bash".into(),
            output: "src\nCargo.toml".into(),
            is_error: false,
            duration_ms: 42,
        },
        Event::Usage {
            role: "primary".into(),
            model: "test-model".into(),
            input_tokens: 1234,
            output_tokens: 567,
            cache_read_tokens: 89,
            cache_creation_tokens: 12,
            cost_usd: Some(0.0123),
        },
        Event::FacultyHook {
            faculty: "spellfix".into(),
            hook: "prefix".into(),
            detail: serde_json::json!({"hit": true}),
        },
        Event::StreamDelta {
            kind: "text".into(),
            text: "Done: listed files.".into(),
        },
        Event::AssistantMessage {
            text: "Done: listed files.".into(),
        },
        Event::TurnEnded {
            turn: 1,
            stop_reason: "end_turn".into(),
        },
    ]
}

fn feed(model: &mut TuiModel, evs: &[Event]) {
    for e in evs {
        model.apply_event(e);
    }
}

// -- snapshot tests -------------------------------------------------------------

#[test]
fn snapshot_renders_tool_card_usage_and_why_panel() {
    let mut m = model_with_palette(Palette::from_theme("cyan"));
    feed(&mut m, &scripted());
    let text = text_of(&render(&mut m));
    assert!(text.contains("Bash"), "tool card name missing:\n{text}");
    assert!(text.contains("cmd=ls -la"), "args summary missing:\n{text}");
    assert!(text.contains("Cargo.toml"), "tool result missing:\n{text}");
    assert!(text.contains("$0.0123"), "cost line missing:\n{text}");
    assert!(text.contains("1234"), "input tokens missing:\n{text}");
    assert!(text.contains("567"), "output tokens missing:\n{text}");
    assert!(text.contains("spellfix"), "why panel line missing:\n{text}");
    assert!(text.contains("hit"), "why panel detail missing:\n{text}");
    assert!(
        text.contains("end_turn"),
        "stop reason line missing:\n{text}"
    );
    assert!(text.contains("turn 1"), "turn divider missing:\n{text}");
    assert!(
        text.contains("IDLE"),
        "idle state missing after TurnEnded:\n{text}"
    );
    assert!(!text.contains("RUNNING"), "stale running state:\n{text}");
}

#[test]
fn no_color_renders_identical_text_without_style() {
    let mut accent = model_with_palette(Palette::from_theme("cyan"));
    feed(&mut accent, &scripted());
    let mut mono = model_with_palette(Palette::monochrome());
    feed(&mut mono, &scripted());

    let ab = render(&mut accent);
    let mb = render(&mut mono);
    assert_eq!(
        text_of(&ab),
        text_of(&mb),
        "NO_COLOR must not change rendered text"
    );

    // Monochrome: every cell must be in the default style.
    for y in 0..mb.area.height {
        for x in 0..mb.area.width {
            if let Some(c) = mb.cell((x, y)) {
                assert_eq!(c.fg, Color::Reset, "mono fg styled at ({x},{y})");
                assert_eq!(c.bg, Color::Reset, "mono bg styled at ({x},{y})");
                assert!(c.modifier.is_empty(), "mono modifier at ({x},{y})");
            }
        }
    }
    // Accent palette: at least one cell carries the accent color.
    let has_accent = (0..ab.area.height).any(|y| {
        (0..ab.area.width).any(|x| {
            ab.cell((x, y))
                .map(|c| c.fg == Color::Cyan)
                .unwrap_or(false)
        })
    });
    assert!(has_accent, "accent palette styled nothing");
}

#[test]
fn approval_modal_shows_risk_badge_and_resolves() {
    let mut m = model_with_palette(Palette::from_theme("cyan"));
    m.open_approval("Bash", "rm -rf build");
    assert!(m.modal_open());
    let text = text_of(&render(&mut m));
    assert!(text.contains("APPROVAL REQUIRED"), "modal missing:\n{text}");
    assert!(text.contains("Bash"), "modal tool missing:\n{text}");
    assert!(
        text.contains("rm -rf build"),
        "modal summary missing:\n{text}"
    );
    assert!(text.contains("[exec]"), "exec risk badge missing:\n{text}");

    let resolved = m.resolve_approval(ApprovalAnswer::Allow);
    assert_eq!(
        resolved,
        Some((
            "Bash".to_string(),
            "rm -rf build".to_string(),
            ApprovalAnswer::Allow
        ))
    );
    assert!(!m.modal_open(), "modal still open after resolve");

    // Non-exec tools get the ask badge; esc-equivalent is deny.
    m.open_approval("Read", "read src/main.rs");
    let text = text_of(&render(&mut m));
    assert!(text.contains("[ask]"), "ask risk badge missing:\n{text}");
    assert!(!text.contains("[exec]"), "wrong risk badge:\n{text}");
    let resolved = m.resolve_approval(ApprovalAnswer::Deny);
    assert_eq!(
        resolved,
        Some((
            "Read".to_string(),
            "read src/main.rs".to_string(),
            ApprovalAnswer::Deny
        ))
    );
}

#[test]
fn approval_modal_queues_next_job() {
    let mut m = model_with_palette(Palette::from_theme("cyan"));
    m.open_approval("Bash", "first command");
    m.open_approval("Bash", "second command");
    assert!(m.modal_open());
    let resolved = m.resolve_approval(ApprovalAnswer::Allow);
    assert_eq!(
        resolved.map(|(tool, _, _)| tool).as_deref(),
        Some("Bash"),
        "first job should resolve"
    );
    assert!(m.modal_open(), "queued job should auto-open next modal");
    let text = text_of(&render(&mut m));
    assert!(
        text.contains("summary  second command"),
        "queued job not shown:\n{text}"
    );
    assert!(
        text.matches("APPROVAL REQUIRED").count() == 1,
        "two modals at once:\n{text}"
    );
    assert!(
        text.contains("approval approved: Bash - first command"),
        "resolution note missing:\n{text}"
    );
}

#[test]
fn streamed_text_is_not_duplicated_by_assistant_message() {
    let mut m = model_with_palette(Palette::from_theme("cyan"));
    m.apply_event(&Event::StreamDelta {
        kind: "text".into(),
        text: "Hel".into(),
    });
    m.apply_event(&Event::StreamDelta {
        kind: "text".into(),
        text: "lo world".into(),
    });
    m.apply_event(&Event::AssistantMessage {
        text: "Hello world".into(),
    });
    let text = text_of(&render(&mut m));
    assert_eq!(
        text.matches("Hello world").count(),
        1,
        "assistant text duplicated:\n{text}"
    );
}

#[test]
fn thinking_stream_finalizes_separately() {
    let mut m = model_with_palette(Palette::from_theme("cyan"));
    m.apply_event(&Event::StreamDelta {
        kind: "thinking".into(),
        text: "pondering".into(),
    });
    m.apply_event(&Event::StreamDelta {
        kind: "text".into(),
        text: "answer text".into(),
    });
    m.apply_event(&Event::AssistantMessage {
        text: "answer text".into(),
    });
    let text = text_of(&render(&mut m));
    assert!(text.contains("pondering"), "thinking stream lost:\n{text}");
    assert!(text.contains("answer text"), "assistant text lost:\n{text}");
    assert_eq!(text.matches("answer text").count(), 1, "text duplicated");
}

#[test]
fn scroll_pages_and_restores_follow() {
    let mut m = model_with_palette(Palette::from_theme("cyan"));
    feed(&mut m, &scripted());
    for i in 0..40 {
        m.note(format!("note line {i}"));
    }
    let _ = render(&mut m); // records total_lines + view_rows
    assert!(m.follow, "should start pinned to the bottom");
    let total = m.total_lines;
    assert!(total > m.view_rows, "test needs scrollable content");

    m.scroll_up(3);
    assert!(!m.follow, "scrolling up must unpin");
    assert_eq!(
        m.scroll_top,
        total.saturating_sub(m.view_rows).saturating_sub(3)
    );

    let scrolled_text = text_of(&render(&mut m));
    m.page_down();
    assert!(m.follow, "page down at the bottom must re-pin");

    // A pinned view shows the newest note; the scrolled view must differ.
    let pinned_text = text_of(&render(&mut m));
    assert_ne!(scrolled_text, pinned_text, "scroll had no visual effect");
    assert!(
        pinned_text.contains("note line 39"),
        "latest note not visible:\n{pinned_text}"
    );
}

#[test]
fn error_events_surface_as_notes() {
    let mut m = model_with_palette(Palette::from_theme("cyan"));
    m.apply_event(&Event::Error {
        message: "provider down".into(),
        fatal: false,
    });
    let text = text_of(&render(&mut m));
    assert!(
        text.contains("provider down"),
        "error note missing:\n{text}"
    );

    m.apply_event(&Event::Error {
        message: "fatal shutdown".into(),
        fatal: true,
    });
    let text = text_of(&render(&mut m));
    assert!(
        text.contains("FATAL error: fatal shutdown"),
        "fatal note missing:\n{text}"
    );
}

#[test]
fn usage_events_accumulate() {
    let mut m = model_with_palette(Palette::from_theme("cyan"));
    for cost in [Some(0.0123), Some(0.0001)] {
        m.apply_event(&Event::Usage {
            role: "primary".into(),
            model: "test-model".into(),
            input_tokens: 1234,
            output_tokens: 567,
            cache_read_tokens: 89,
            cache_creation_tokens: 12,
            cost_usd: cost,
        });
    }
    let text = text_of(&render(&mut m));
    assert!(text.contains("2468"), "input tokens not summed:\n{text}");
    assert!(text.contains("1134"), "output tokens not summed:\n{text}");
    assert!(text.contains("$0.0124"), "cost not summed:\n{text}");
}

#[test]
fn palette_detect_defaults_and_no_color_env() {
    // The environment may already carry NO_COLOR (CI sandboxes do), so
    // normalize it first and restore the original value after.
    let saved = std::env::var("NO_COLOR").ok();
    std::env::remove_var("NO_COLOR");

    let p = Palette::detect("magenta");
    assert_eq!(p.accent_color, Color::Magenta);
    assert!(!p.mono);
    let unknown = Palette::detect("sparkle-rainbow");
    assert_eq!(unknown.accent_color, Color::Cyan);

    // NO_COLOR (non-empty) forces monochrome.
    std::env::set_var("NO_COLOR", "1");
    let nc = Palette::detect("magenta");
    match saved {
        Some(v) => std::env::set_var("NO_COLOR", v),
        None => std::env::remove_var("NO_COLOR"),
    }
    assert!(nc.mono, "NO_COLOR must force monochrome");
}

#[test]
fn wrap_breaks_words_and_long_tokens() {
    assert_eq!(tui::wrap("hello world", 5), vec!["hello", "world"]);
    assert_eq!(tui::wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
    assert_eq!(tui::wrap("a\n\nb", 4), vec!["a", "", "b"]);
    assert_eq!(tui::wrap("tiny", 10), vec!["tiny"]);
}

#[test]
fn key_input_composes_prompt_and_quit() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    let key = |c: KeyCode| KeyEvent {
        code: c,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    };

    let mut m = model_with_palette(Palette::from_theme("cyan"));
    assert_eq!(
        tui::handle_key(&mut m, key(KeyCode::Char('h'))),
        tui::KeyOutcome::None
    );
    assert_eq!(
        tui::handle_key(&mut m, key(KeyCode::Char('i'))),
        tui::KeyOutcome::None
    );
    assert_eq!(m.input, "hi");
    assert_eq!(
        tui::handle_key(&mut m, key(KeyCode::Backspace)),
        tui::KeyOutcome::None
    );
    assert_eq!(m.input, "h");
    assert_eq!(
        tui::handle_key(&mut m, key(KeyCode::Char('i'))),
        tui::KeyOutcome::None
    );
    match tui::handle_key(&mut m, key(KeyCode::Enter)) {
        tui::KeyOutcome::Prompt(text) => assert_eq!(text, "hi"),
        other => panic!("expected prompt outcome, got {other:?}"),
    }
    assert_eq!(m.input, "", "input should clear after Enter");
    m.push_user_prompt("hi");
    let text = text_of(&render(&mut m));
    assert!(
        text.contains("you: hi"),
        "user prompt line missing:\n{text}"
    );

    // /quit is a quit outcome, not a prompt.
    for c in "/quit".chars() {
        tui::handle_key(&mut m, key(KeyCode::Char(c)));
    }
    assert!(matches!(
        tui::handle_key(&mut m, key(KeyCode::Enter)),
        tui::KeyOutcome::Quit
    ));
}
