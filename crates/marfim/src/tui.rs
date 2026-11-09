//! `marfim tui` — a ratatui terminal UI over the session actor.
//!
//! Layout: a one-line header (model, session id, turn, running state), a
//! scrollable transcript pane (stream deltas append live; tool calls render
//! as cards with args summary + result/error flag), a right sidebar
//! (session/context meter from `Snapshot`, usage/cost totals, and a WHY
//! panel with the latest faculty hooks), a prompt input line, and a
//! key-hints footer. Approvals surface as a centered modal with a risk
//! badge; while it is up only y/n/esc are handled.
//!
//! Testability contract: every state transition is a pure method on
//! [`TuiModel`] (events in → model out) and the whole screen is one pure
//! [`draw`] into a [`ratatui::buffer::Buffer`]. `tests/tui_snapshots.rs`
//! feeds scripted [`Event`]s into a `TestBackend` and asserts on the
//! rendered buffer — no processes are spawned.
//!
//! Runtime wiring (the only impure part): a dedicated OS thread reads
//! crossterm events into a bounded channel (D14: one stdin owner), the bus
//! subscription and the actor's approval forwarder feed the same loop, a
//! 500 ms ticker polls `Snapshot` while a turn is running (plus on demand
//! via `r`), and every actor command's oneshot reply is awaited inside a
//! detached task so replies are never dropped unanswered.
//!
//! Honest narrowings:
//! - text wrapping is char-count based (no unicode-width dependency here);
//!   East-Asian wide glyphs may misalign a wrapped line.
//! - tool-card results render a capped number of lines; the full output
//!   stays in the session journal.
//! - the sidebar `ctx` bar is journal size against a nominal 1 MiB — a
//!   volume gauge, not the token budget (the budget lives in usage totals).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event as CtEvent, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use marfim_core::events::Event;
use marfim_core::permissions::ApprovalAnswer;
use marfim_core::session::{SessionCommand, SessionSnapshot};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::{Frame, Terminal};
use tokio::sync::{mpsc, oneshot};

/// Sidebar width (session meter + usage + why).
const SIDEBAR_WIDTH: u16 = 26;
/// Tool-card result lines rendered before the "more lines" hint.
const TOOL_RESULT_MAX_LINES: usize = 6;
/// Faculty hooks kept in the WHY panel.
const WHY_KEEP: usize = 8;
/// Double-Esc window for quit.
const ESC_QUIT_WINDOW: Duration = Duration::from_millis(900);

// ---------------------------------------------------------------------------
// Palette: monochrome base + ONE accent; NO_COLOR collapses everything to
// the terminal default style.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub accent_color: Color,
    pub mono: bool,
}

impl Palette {
    /// NO_COLOR (non-empty) forces pure monochrome; otherwise the accent
    /// comes from `settings.theme` when it names a base color, else cyan.
    pub fn detect(theme: &str) -> Self {
        let no_color = std::env::var("NO_COLOR")
            .map(|v| !v.is_empty())
            .unwrap_or(false);
        if no_color {
            return Self::monochrome();
        }
        Self::from_theme(theme)
    }

    /// Pure monochrome: every style helper returns the default style.
    pub fn monochrome() -> Self {
        Self {
            accent_color: Color::Reset,
            mono: true,
        }
    }

    pub fn from_theme(theme: &str) -> Self {
        let accent = match theme.to_ascii_lowercase().as_str() {
            "red" => Color::Red,
            "green" => Color::Green,
            "blue" => Color::Blue,
            "magenta" => Color::Magenta,
            "yellow" => Color::Yellow,
            "white" => Color::White,
            "gray" | "grey" => Color::Gray,
            "dark-gray" | "darkgrey" => Color::DarkGray,
            "light-red" => Color::LightRed,
            "light-green" => Color::LightGreen,
            "light-blue" => Color::LightBlue,
            "light-cyan" => Color::LightCyan,
            "light-magenta" => Color::LightMagenta,
            "light-yellow" => Color::LightYellow,
            _ => Color::Cyan,
        };
        Self {
            accent_color: accent,
            mono: false,
        }
    }

    fn st(&self, fg: Color, bold: bool) -> Style {
        if self.mono {
            return Style::default();
        }
        let mut s = Style::default().fg(fg);
        if bold {
            s = s.add_modifier(Modifier::BOLD);
        }
        s
    }

    pub fn dim(&self) -> Style {
        self.st(Color::DarkGray, false)
    }

    pub fn accent(&self, bold: bool) -> Style {
        self.st(self.accent_color, bold)
    }

    pub fn danger(&self) -> Style {
        self.st(Color::Red, false)
    }

    pub fn warn(&self) -> Style {
        self.st(Color::Yellow, false)
    }
}

// ---------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------

/// Risk badge approximation: anything Bash/Execute-shaped is `exec`
/// (danger); every other ask is `ask` (warn).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Risk {
    Exec,
    Ask,
}

impl Risk {
    pub fn of(tool: &str) -> Self {
        let t = tool.to_ascii_lowercase();
        if t.contains("bash") || t.contains("exec") || t.contains("shell") {
            Risk::Exec
        } else {
            Risk::Ask
        }
    }

    pub fn badge(&self) -> &'static str {
        match self {
            Risk::Exec => "exec",
            Risk::Ask => "ask",
        }
    }

    fn style(&self, pal: Palette) -> Style {
        match self {
            Risk::Exec => pal.danger(),
            Risk::Ask => pal.warn(),
        }
    }
}

/// The centered approval modal. Answered ONLY via the actor's `Approve`
/// command; the forwarded job's own respond channel is a tombstone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalModal {
    pub tool: String,
    pub summary: String,
    pub risk: Risk,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    pub output: String,
    pub is_error: bool,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCard {
    pub id: String,
    pub tool: String,
    pub args: String,
    pub result: Option<ToolResult>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhyEntry {
    pub faculty: String,
    pub hook: String,
    pub detail: String,
}

/// One transcript entry. Flattened to styled lines at render time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Turn(u64),
    User(String),
    Assistant(String),
    Thinking(String),
    Note(String),
    Card(ToolCard),
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct UsageTotals {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
    pub cost: f64,
}

/// The full UI state: pure, owned by the event loop, exercised directly by
/// the snapshot tests.
#[derive(Debug, Clone)]
pub struct TuiModel {
    pub session_id: String,
    pub model: String,
    pub turn: u64,
    pub running: bool,
    pub cancelled: bool,
    pub entries: Vec<Entry>,
    pub stream_text: String,
    pub stream_thinking: String,
    /// Stick to the bottom of the transcript while true.
    pub follow: bool,
    /// Absolute top line when the user scrolled up (`follow == false`).
    pub scroll_top: usize,
    /// Viewport geometry, refreshed on every draw.
    pub view_rows: usize,
    pub total_lines: usize,
    pub input: String,
    pub usage: UsageTotals,
    /// Catalog context window for the primary model, when known — powers
    /// the header `ctx` gauge (distillation §H1, OMP `context_pct`).
    pub context_window: Option<u64>,
    /// §H `token_rate` live segment (distilled: OMP status-line segments):
    /// bounded ring of (wall instant, cumulative stream chars). Rate is
    /// derived chars/4-per-sec over the window — display-only, never
    /// persisted, never affects the loop.
    pub rate_window: std::collections::VecDeque<(std::time::Instant, u64)>,
    pub stream_chars: u64,
    pub snapshot: Option<SessionSnapshot>,
    pub why: VecDeque<WhyEntry>,
    pub modal: Option<ApprovalModal>,
    /// Approvals that arrived while a modal was already up.
    pub modal_queue: VecDeque<(String, String)>,
    pub palette: Palette,
    pub last_stop: Option<String>,
    /// Set by `r` / turn end: pull a snapshot on the next tick.
    pub snapshot_wanted: bool,
}

impl TuiModel {
    pub fn new(session_id: String, model: String, palette: Palette) -> Self {
        Self {
            session_id,
            model,
            turn: 0,
            running: false,
            cancelled: false,
            entries: Vec::new(),
            stream_text: String::new(),
            stream_thinking: String::new(),
            follow: true,
            scroll_top: 0,
            view_rows: 0,
            total_lines: 0,
            input: String::new(),
            usage: UsageTotals::default(),
            context_window: None,
            rate_window: VecDeque::new(),
            stream_chars: 0,
            snapshot: None,
            why: VecDeque::new(),
            modal: None,
            modal_queue: VecDeque::new(),
            palette,
            last_stop: None,
            snapshot_wanted: false,
        }
    }

    // -- event application --------------------------------------------------

    pub fn apply_event(&mut self, ev: &Event) {
        match ev {
            Event::SessionStarted { session_id, .. } => {
                self.session_id = session_id.clone();
            }
            Event::TurnStarted { turn } => {
                self.turn = *turn;
                self.running = true;
                self.cancelled = false;
                self.stream_text.clear();
                self.stream_thinking.clear();
                self.entries.push(Entry::Turn(*turn));
            }
            Event::ModelSelected { role, model, .. } if role == "primary" => {
                self.model = model.clone();
            }
            Event::ModelSelected { .. } => {}
            Event::AssistantMessage { text } => {
                // Thinking deltas stream separately from the final text; the
                // authoritative AssistantMessage replaces the accumulated
                // text stream (same content — never duplicated).
                if !self.stream_thinking.is_empty() {
                    let t = std::mem::take(&mut self.stream_thinking);
                    self.entries.push(Entry::Thinking(t));
                }
                self.stream_text.clear();
                self.entries.push(Entry::Assistant(text.clone()));
            }
            Event::Thinking { text } => {
                self.entries.push(Entry::Thinking(text.clone()));
            }
            Event::StreamDelta { kind, text } => {
                if kind == "thinking" {
                    self.stream_thinking.push_str(text);
                } else {
                    self.stream_text.push_str(text);
                }
                // §H rate window: bounded sample push (cap 64).
                self.stream_chars = self
                    .stream_chars
                    .saturating_add(text.chars().count() as u64);
                self.rate_window
                    .push_back((std::time::Instant::now(), self.stream_chars));
                while self.rate_window.len() > 64 {
                    self.rate_window.pop_front();
                }
            }
            Event::ToolCallStarted { id, tool, args } => {
                self.entries.push(Entry::Card(ToolCard {
                    id: id.clone(),
                    tool: tool.clone(),
                    args: summarize_args(args),
                    result: None,
                }));
            }
            Event::ToolCallResult {
                id,
                tool,
                output,
                is_error,
                duration_ms,
            } => {
                let found = self.entries.iter_mut().rev().any(|e| match e {
                    Entry::Card(c) if c.id == *id => {
                        c.result = Some(ToolResult {
                            output: clip(output, 4096),
                            is_error: *is_error,
                            duration_ms: *duration_ms,
                        });
                        true
                    }
                    _ => false,
                });
                if !found {
                    self.entries
                        .push(Entry::Note(format!("[{tool}] {}", clip(output, 200))));
                }
            }
            Event::ApprovalRequested {
                approval_id,
                tool,
                summary,
            } => {
                self.entries.push(Entry::Note(format!(
                    "approval {approval_id} requested: {tool} - {summary}"
                )));
            }
            Event::ApprovalResolved {
                approval_id,
                decision,
            } => {
                self.entries
                    .push(Entry::Note(format!("approval {approval_id} -> {decision}")));
            }
            Event::Usage {
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_creation_tokens,
                cost_usd,
                ..
            } => {
                self.usage.input += input_tokens;
                self.usage.output += output_tokens;
                self.usage.cache_read += cache_read_tokens;
                self.usage.cache_creation += cache_creation_tokens;
                if let Some(c) = cost_usd {
                    self.usage.cost += c;
                }
            }
            Event::Compaction {
                before_tokens,
                after_tokens,
            } => {
                self.entries.push(Entry::Note(format!(
                    "context compacted: {before_tokens} -> {after_tokens} tokens"
                )));
            }
            Event::VerifyStarted { commands } => {
                self.entries.push(Entry::Note(format!(
                    "verify started: {} command(s)",
                    commands.len()
                )));
            }
            Event::VerifyFinished { passed, output } => {
                if *passed {
                    self.entries.push(Entry::Note("verify passed".into()));
                } else {
                    self.entries
                        .push(Entry::Note(format!("verify FAILED: {}", clip(output, 200))));
                }
            }
            Event::FacultyHook {
                faculty,
                hook,
                detail,
            } => {
                self.why.push_back(WhyEntry {
                    faculty: clip(faculty, 10),
                    hook: clip(hook, 8),
                    detail: clip(&detail.to_string(), 12),
                });
                while self.why.len() > WHY_KEEP {
                    self.why.pop_front();
                }
            }
            // W7.2: the cache-warm ping outcome lands in the why panel
            // (observability only — it changes no UI state).
            Event::CacheWarm { ok, model } => {
                self.why.push_back(WhyEntry {
                    faculty: clip("cache", 10),
                    hook: clip("warm", 8),
                    detail: clip(
                        &format!("{}/{}", if *ok { "ok" } else { "failed" }, model),
                        12,
                    ),
                });
                while self.why.len() > WHY_KEEP {
                    self.why.pop_front();
                }
            }
            Event::Actuation {
                faculty,
                kind,
                after,
                ..
            } => {
                // Every actuation is visible in the transcript (never
                // invisible influence) and lands in the WHY panel.
                self.entries.push(Entry::Note(format!(
                    "actuation: {kind} by {faculty} → {after}"
                )));
                self.why.push_back(WhyEntry {
                    faculty: clip(faculty, 10),
                    hook: clip("actuation", 8),
                    detail: clip(&format!("{kind} → {after}"), 12),
                });
                while self.why.len() > WHY_KEEP {
                    self.why.pop_front();
                }
            }
            Event::TurnEnded { turn, stop_reason } => {
                // Flush anything the provider streamed without a final
                // AssistantMessage (defensive; the loop always sends one).
                if !self.stream_thinking.is_empty() {
                    let t = std::mem::take(&mut self.stream_thinking);
                    self.entries.push(Entry::Thinking(t));
                }
                if !self.stream_text.is_empty() {
                    let t = std::mem::take(&mut self.stream_text);
                    self.entries.push(Entry::Assistant(t));
                }
                self.entries
                    .push(Entry::Note(format!("turn {turn} ended: {stop_reason}")));
                self.last_stop = Some(stop_reason.clone());
                self.running = false;
                self.cancelled = false;
                self.snapshot_wanted = true;
            }
            Event::Error { message, fatal } => {
                if *fatal {
                    self.entries
                        .push(Entry::Note(format!("FATAL error: {message}")));
                } else {
                    self.entries.push(Entry::Note(format!("error: {message}")));
                }
            }
        }
    }

    pub fn apply_snapshot(&mut self, snap: &SessionSnapshot) {
        self.snapshot = Some(snap.clone());
        self.turn = snap.turn;
        self.running = snap.turn_running;
        self.cancelled = snap.cancelled;
    }

    // -- input / keys --------------------------------------------------------

    pub fn push_char(&mut self, c: char) {
        self.input.push(c);
    }

    pub fn backspace(&mut self) {
        self.input.pop();
    }

    /// Enter pressed: the trimmed prompt, or None when empty.
    pub fn take_prompt(&mut self) -> Option<String> {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return None;
        }
        self.input.clear();
        Some(text)
    }

    /// Prompt handed to the actor: record it in the transcript and show
    /// running until TurnStarted/TurnEnded arrive.
    pub fn push_user_prompt(&mut self, text: &str) {
        self.entries.push(Entry::User(text.to_string()));
        self.running = true;
        self.snapshot_wanted = true;
    }

    pub fn note(&mut self, text: String) {
        self.entries.push(Entry::Note(text));
    }

    // -- scrolling -----------------------------------------------------------

    fn max_top(&self) -> usize {
        self.total_lines.saturating_sub(self.view_rows)
    }

    /// The scroll offset actually rendered this frame.
    pub fn effective_top(&self) -> usize {
        if self.follow {
            self.max_top()
        } else {
            self.scroll_top.min(self.max_top())
        }
    }

    pub fn scroll_up(&mut self, n: usize) {
        let top = self.effective_top();
        self.scroll_top = top.saturating_sub(n);
        self.follow = false;
    }

    pub fn scroll_down(&mut self, n: usize) {
        if self.follow {
            return;
        }
        self.scroll_top = self.scroll_top.saturating_add(n);
        if self.scroll_top >= self.max_top() {
            self.follow = true;
        }
    }

    pub fn page_up(&mut self) {
        self.scroll_up(self.view_rows.max(10));
    }

    pub fn page_down(&mut self) {
        self.scroll_down(self.view_rows.max(10));
    }

    // -- approvals ------------------------------------------------------------

    pub fn modal_open(&self) -> bool {
        self.modal.is_some()
    }

    /// An approval job arrived on the actor's forwarder channel. If a modal
    /// is already up, the job queues (the actor keeps it pending).
    pub fn open_approval(&mut self, tool: &str, summary: &str) {
        self.entries.push(Entry::Note(format!(
            "approval requested: {tool} - {summary}"
        )));
        if self.modal.is_some() {
            self.modal_queue
                .push_back((tool.to_string(), summary.to_string()));
        } else {
            self.modal = Some(ApprovalModal {
                tool: tool.to_string(),
                summary: summary.to_string(),
                risk: Risk::of(tool),
            });
        }
    }

    /// Resolve the visible modal (test surface too): returns what the caller
    /// must send as the actor's `Approve` command. A queued job, if any,
    /// becomes the next modal.
    pub fn resolve_approval(
        &mut self,
        answer: ApprovalAnswer,
    ) -> Option<(String, String, ApprovalAnswer)> {
        let m = self.modal.take()?;
        let word = match answer {
            ApprovalAnswer::Allow | ApprovalAnswer::AllowAlways => "approved",
            ApprovalAnswer::Deny => "denied",
        };
        self.entries.push(Entry::Note(format!(
            "approval {word}: {} - {}",
            m.tool, m.summary
        )));
        if let Some((tool, summary)) = self.modal_queue.pop_front() {
            let risk = Risk::of(&tool);
            self.modal = Some(ApprovalModal {
                tool,
                summary,
                risk,
            });
        }
        Some((m.tool, m.summary, answer))
    }

    // -- transcript flattening --------------------------------------------------

    /// Pure: entries + live stream buffers → styled physical lines. Returns
    /// the lines and the total (pre-viewport) line count.
    pub fn transcript_lines(&self, width: usize) -> (Vec<Line<'static>>, usize) {
        let pal = self.palette;
        let mut lines: Vec<Line<'static>> = Vec::new();
        for e in &self.entries {
            match e {
                Entry::Turn(n) => {
                    let label = format!(" turn {n} ");
                    let fill = width.saturating_sub(label.chars().count()).max(3);
                    lines.push(Line::from(vec![
                        Span::styled(label, pal.accent(true)),
                        Span::styled("-".repeat(fill), pal.dim()),
                    ]));
                }
                Entry::User(text) => {
                    push_prefixed(&mut lines, pal, "you: ", text, width, Style::default());
                }
                Entry::Assistant(text) => {
                    for l in wrap(text, width) {
                        lines.push(Line::from(Span::raw(l)));
                    }
                    lines.push(Line::from(Span::raw("")));
                }
                Entry::Thinking(text) => {
                    push_prefixed(&mut lines, pal, "thinking: ", text, width, pal.dim());
                }
                Entry::Note(text) => {
                    push_prefixed(&mut lines, pal, "* ", text, width, pal.dim());
                }
                Entry::Card(c) => {
                    lines.push(Line::from(vec![
                        Span::styled("+ ", pal.accent(false)),
                        Span::styled(c.tool.clone(), pal.accent(true)),
                        Span::styled(format!(" {}", c.args), pal.dim()),
                    ]));
                    match &c.result {
                        None => {
                            lines.push(Line::from(Span::styled("  ... running", pal.dim())));
                        }
                        Some(r) => {
                            let (mark, style) = if r.is_error {
                                ("x ", pal.danger())
                            } else {
                                ("= ", pal.dim())
                            };
                            let body = wrap(&r.output, width.saturating_sub(4));
                            let shown = body.len().min(TOOL_RESULT_MAX_LINES);
                            for (i, l) in body.iter().take(shown).enumerate() {
                                let lead = if i == 0 {
                                    format!("  {mark}")
                                } else {
                                    "    ".to_string()
                                };
                                lines.push(Line::from(vec![
                                    Span::styled(lead, style),
                                    Span::styled(l.clone(), style),
                                ]));
                            }
                            if body.len() > shown {
                                lines.push(Line::from(Span::styled(
                                    format!("  ... {} more lines", body.len() - shown),
                                    pal.dim(),
                                )));
                            }
                        }
                    }
                }
            }
        }
        // Live streaming buffers (not yet finalized into entries).
        if !self.stream_thinking.is_empty() {
            let t = self.stream_thinking.clone();
            push_prefixed(&mut lines, pal, "thinking: ", &t, width, pal.dim());
        }
        if !self.stream_text.is_empty() {
            for l in wrap(&self.stream_text.clone(), width) {
                lines.push(Line::from(Span::raw(l)));
            }
        }
        let total = lines.len();
        (lines, total)
    }
}

// ---------------------------------------------------------------------------
// Pure render helpers
// ---------------------------------------------------------------------------

/// Char-count greedy word wrap (width 0 degenerates to one unwrapped line).
pub fn wrap(s: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![s.to_string()];
    }
    let mut out: Vec<String> = Vec::new();
    for para in s.split('\n') {
        if para.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut cur = String::new();
        let mut cur_len = 0usize;
        for word in para.split(' ') {
            let mut rest: &str = word;
            while !rest.is_empty() {
                let wlen = rest.chars().count();
                let sep = usize::from(!cur.is_empty());
                if cur_len + sep + wlen <= width {
                    if sep == 1 {
                        cur.push(' ');
                    }
                    cur.push_str(rest);
                    cur_len += sep + wlen;
                    rest = "";
                } else if sep == 1 {
                    out.push(std::mem::take(&mut cur));
                    cur_len = 0;
                } else {
                    // word longer than the whole line: hard-split it
                    let take = width.min(wlen);
                    let chunk: String = rest.chars().take(take).collect();
                    let skip: usize = rest.chars().take(take).map(char::len_utf8).sum();
                    out.push(chunk);
                    rest = &rest[skip..];
                }
            }
        }
        out.push(cur);
    }
    out
}

/// Clip `s` to `max` chars with a trailing `...` when truncated.
fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max.saturating_sub(3)).collect();
    format!("{head}...")
}

fn summarize_args(v: &serde_json::Value) -> String {
    let flat = match v {
        serde_json::Value::Object(map) => {
            let mut parts: Vec<String> = Vec::new();
            for (k, val) in map.iter().take(6) {
                let rendered = val
                    .as_str()
                    .map(String::from)
                    .unwrap_or_else(|| val.to_string());
                parts.push(format!("{k}={}", clip(&rendered, 40)));
            }
            if map.len() > 6 {
                parts.push("...".to_string());
            }
            parts.join(", ")
        }
        other => other.to_string(),
    };
    clip(&flat, 120)
}

fn human_bytes(b: u64) -> String {
    if b < 1024 {
        format!("{b} B")
    } else if b < 1024 * 1024 {
        format!("{:.1} KiB", b as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", b as f64 / (1024.0 * 1024.0))
    }
}

/// ASCII volume bar: `[####......]`.
fn bar(ratio: f64, cells: usize) -> String {
    let r = ratio.clamp(0.0, 1.0);
    let filled = ((r * cells as f64).round() as usize).min(cells);
    format!("[{}{}]", "#".repeat(filled), ".".repeat(cells - filled))
}

fn push_prefixed(
    lines: &mut Vec<Line<'static>>,
    pal: Palette,
    prefix: &str,
    text: &str,
    width: usize,
    style: Style,
) {
    let pad = prefix.chars().count();
    for (i, l) in wrap(text, width.saturating_sub(pad))
        .into_iter()
        .enumerate()
    {
        if i == 0 {
            lines.push(Line::from(vec![
                Span::styled(prefix.to_string(), pal.dim()),
                Span::styled(l, style),
            ]));
        } else {
            lines.push(Line::from(vec![
                Span::styled(" ".repeat(pad), pal.dim()),
                Span::styled(l, style),
            ]));
        }
    }
}

// ---------------------------------------------------------------------------
// Frame rendering
// ---------------------------------------------------------------------------

/// Pure screen paint: model → buffer. Records viewport geometry on the
/// model so key handling can page without re-measuring.
pub fn draw(f: &mut Frame<'_>, model: &mut TuiModel) {
    let root = f.area();
    let [header, body, input, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(root);
    render_header(f, model, header);
    let side_w = SIDEBAR_WIDTH.min(body.width);
    let [tx, side] =
        Layout::horizontal([Constraint::Min(20), Constraint::Length(side_w)]).areas(body);
    render_sidebar(f, model, side);
    render_transcript(f, model, tx);
    render_input(f, model, input);
    render_footer(f, model, footer);
    if model.modal_open() {
        render_modal(f, model, root);
    } else {
        let col = (input.x + 2 + model.input.chars().count() as u16)
            .min(input.x + input.width.saturating_sub(1));
        f.set_cursor_position((col, input.y));
    }
}

fn state_span(model: &TuiModel) -> (&'static str, Style) {
    if model.cancelled {
        ("CANCELLING", model.palette.danger())
    } else if model.running {
        ("RUNNING", model.palette.accent(true))
    } else {
        ("IDLE", model.palette.dim())
    }
}

fn render_header(f: &mut Frame<'_>, model: &TuiModel, area: Rect) {
    let (state, st) = state_span(model);
    let pal = model.palette;
    let mut spans = vec![
        Span::styled(
            format!(" marfim v{}", marfim_core::VERSION),
            pal.accent(true),
        ),
        Span::styled(" | ", pal.dim()),
        Span::raw(model.model.clone()),
        Span::styled(" | ", pal.dim()),
        Span::raw(format!("session {}", model.session_id)),
        Span::styled(" | ", pal.dim()),
        Span::raw(format!("turn {}", model.turn)),
        Span::styled(" | ", pal.dim()),
        Span::styled(state, st),
    ];
    // Distillation §H1: live usage segments (OMP status-line segments
    // `token_total` / `cost` / `context_pct` — shown only once observed,
    // never invented).
    let u = &model.usage;
    if u.input > 0 || u.output > 0 {
        spans.push(Span::styled(" | ", pal.dim()));
        let mut tok = format!("tok {}/{}", u.input, u.output);
        if u.cache_read > 0 {
            tok.push_str(&format!(" (cache {})", u.cache_read));
        }
        spans.push(Span::raw(tok));
        // §H token_rate: chars→tokens over the ring window, shown only
        // while actually streaming (≥2 samples spanning ≥1s).
        if let (Some(first), Some(last)) = (model.rate_window.front(), model.rate_window.back()) {
            let dt = last.0.saturating_duration_since(first.0);
            if dt.as_secs_f64() >= 1.0 {
                let chars = last.1.saturating_sub(first.1);
                let tps = (chars as f64 / 4.0) / dt.as_secs_f64();
                if tps > 0.5 {
                    spans.push(Span::styled(" | ", pal.dim()));
                    spans.push(Span::raw(format!("{tps:.0} tok/s")));
                }
            }
        }
        if u.cost > 0.0 {
            spans.push(Span::styled(" | ", pal.dim()));
            spans.push(Span::raw(format!("${:.4}", u.cost)));
        }
    }
    if let Some(w) = model.context_window {
        let used = u.input + u.cache_read + u.cache_creation;
        if let Some(pct) = used.checked_mul(100).and_then(|v| v.checked_div(w)) {
            if pct > 0 {
                spans.push(Span::styled(" | ", pal.dim()));
                spans.push(Span::raw(format!("ctx {pct}%")));
            }
        }
    }
    let line = Line::from(spans);
    f.render_widget(Paragraph::new(line), area);
}

fn kv_styled(lines: &mut Vec<Line<'static>>, pal: Palette, k: &str, v: String, style: Style) {
    lines.push(Line::from(vec![
        Span::styled(format!("{k:<6}"), pal.dim()),
        Span::styled(v, style),
    ]));
}

fn kv(lines: &mut Vec<Line<'static>>, pal: Palette, k: &str, v: String) {
    kv_styled(lines, pal, k, v, Style::default());
}

fn render_sidebar(f: &mut Frame<'_>, model: &TuiModel, area: Rect) {
    let pal = model.palette;
    let [sess, usage, why] = Layout::vertical([
        Constraint::Length(10),
        Constraint::Length(6),
        Constraint::Min(1),
    ])
    .areas(area);

    let snap_turn = model
        .snapshot
        .as_ref()
        .map(|s| s.turn)
        .unwrap_or(model.turn);
    let hist = model.snapshot.as_ref().map(|s| s.history_len).unwrap_or(0);
    let journal = model
        .snapshot
        .as_ref()
        .map(|s| s.journal_bytes)
        .unwrap_or(0);
    let steers = model
        .snapshot
        .as_ref()
        .map(|s| s.pending_steers)
        .unwrap_or(0);
    let pending = model
        .snapshot
        .as_ref()
        .map(|s| s.pending_approvals)
        .unwrap_or(0);

    let inner_w = sess.width.saturating_sub(2) as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    kv(
        &mut lines,
        pal,
        "id",
        clip(&model.session_id, inner_w.saturating_sub(6)),
    );
    kv(&mut lines, pal, "turn", snap_turn.to_string());
    kv(&mut lines, pal, "hist", format!("{hist} msgs"));
    kv(&mut lines, pal, "jrnl", human_bytes(journal));
    kv(
        &mut lines,
        pal,
        "ctx",
        bar(journal as f64 / (1024.0 * 1024.0), 10),
    );
    kv(&mut lines, pal, "steer", format!("{steers} queued"));
    kv(&mut lines, pal, "appr", format!("{pending} pending"));
    let (state, st) = state_span(model);
    kv_styled(&mut lines, pal, "state", state.to_string(), st);
    let block = Block::bordered()
        .title(" session ")
        .title_style(pal.accent(true));
    f.render_widget(Paragraph::new(lines).block(block), sess);

    let mut ul: Vec<Line<'static>> = Vec::new();
    kv(&mut ul, pal, "in", model.usage.input.to_string());
    kv(&mut ul, pal, "out", model.usage.output.to_string());
    kv(
        &mut ul,
        pal,
        "cache",
        format!(
            "{} / {}",
            model.usage.cache_read, model.usage.cache_creation
        ),
    );
    kv(&mut ul, pal, "cost", format!("${:.4}", model.usage.cost));
    let ublock = Block::bordered()
        .title(" usage ")
        .title_style(pal.accent(true));
    f.render_widget(Paragraph::new(ul).block(ublock), usage);

    let visible = why.height.saturating_sub(2) as usize;
    let detail_budget = (why.width.saturating_sub(2) as usize)
        .saturating_sub(13)
        .max(4);
    let wlines: Vec<Line<'static>> = model
        .why
        .iter()
        .rev()
        .take(visible)
        .rev()
        .map(|w| {
            Line::from(vec![
                Span::styled(clip(&format!("{}.{}", w.faculty, w.hook), 12), pal.dim()),
                Span::raw(format!(" {}", clip(&w.detail, detail_budget))),
            ])
        })
        .collect();
    let wblock = Block::bordered()
        .title(" why ")
        .title_style(pal.accent(true));
    f.render_widget(Paragraph::new(wlines).block(wblock), why);
}

fn render_transcript(f: &mut Frame<'_>, model: &mut TuiModel, area: Rect) {
    let inner_w = area.width.saturating_sub(2) as usize;
    let (lines, total) = model.transcript_lines(inner_w);
    model.total_lines = total;
    model.view_rows = area.height.saturating_sub(2) as usize;
    let top = u16::try_from(model.effective_top()).unwrap_or(u16::MAX);
    let block = Block::bordered()
        .title(" transcript ")
        .title_style(model.palette.accent(true));
    let para = Paragraph::new(lines).block(block).scroll((top, 0));
    f.render_widget(para, area);
}

fn render_input(f: &mut Frame<'_>, model: &TuiModel, area: Rect) {
    let line = Line::from(vec![
        Span::styled("> ", model.palette.accent(false)),
        Span::raw(model.input.clone()),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn render_footer(f: &mut Frame<'_>, model: &TuiModel, area: Rect) {
    let mut spans = vec![Span::styled(
        "enter=send ctrl+c=cancel pgup/dn=scroll r=snapshot esc esc=quit",
        model.palette.dim(),
    )];
    if let Some(stop) = &model.last_stop {
        spans.push(Span::styled(
            format!("  stop: {stop}"),
            model.palette.accent(false),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_modal(f: &mut Frame<'_>, model: &TuiModel, area: Rect) {
    let Some(m) = model.modal.as_ref() else {
        return;
    };
    let pal = model.palette;
    let w = 56.min(area.width);
    let h = 9.min(area.height);
    if w == 0 || h == 0 {
        return;
    }
    let r = Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    };
    f.render_widget(Clear, r);
    let inner_w = (w as usize).saturating_sub(4);
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(Span::styled(
        "! APPROVAL REQUIRED",
        pal.accent(true),
    )));
    lines.push(Line::from(Span::raw("")));
    lines.push(Line::from(vec![
        Span::styled("tool     ", pal.dim()),
        Span::raw(m.tool.clone()),
        Span::styled(format!("  [{}]", m.risk.badge()), m.risk.style(pal)),
    ]));
    for (i, l) in wrap(&m.summary, inner_w.saturating_sub(9))
        .into_iter()
        .take(2)
        .enumerate()
    {
        if i == 0 {
            lines.push(Line::from(vec![
                Span::styled("summary  ", pal.dim()),
                Span::raw(l),
            ]));
        } else {
            lines.push(Line::from(vec![
                Span::styled("         ", pal.dim()),
                Span::raw(l),
            ]));
        }
    }
    lines.push(Line::from(Span::raw("")));
    lines.push(Line::from(Span::styled(
        "y approve    n / esc deny",
        pal.dim(),
    )));
    let block = Block::bordered()
        .title(" approval ")
        .title_style(pal.accent(true));
    f.render_widget(Paragraph::new(lines).block(block), r);
}

// ---------------------------------------------------------------------------
// Key handling (pure over the model; the loop turns outcomes into commands)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum KeyOutcome {
    None,
    Prompt(String),
    Cancel,
    Approve(String, String, ApprovalAnswer),
    Refresh,
    Esc,
    Quit,
}

fn deny_or_none(model: &mut TuiModel) -> KeyOutcome {
    match model.resolve_approval(ApprovalAnswer::Deny) {
        Some((tool, summary, answer)) => KeyOutcome::Approve(tool, summary, answer),
        None => KeyOutcome::None,
    }
}

fn allow_or_none(model: &mut TuiModel) -> KeyOutcome {
    match model.resolve_approval(ApprovalAnswer::Allow) {
        Some((tool, summary, answer)) => KeyOutcome::Approve(tool, summary, answer),
        None => KeyOutcome::None,
    }
}

pub fn handle_key(model: &mut TuiModel, key: KeyEvent) -> KeyOutcome {
    if key.kind != KeyEventKind::Press {
        return KeyOutcome::None;
    }
    // While the modal is up, nothing else is handled.
    if model.modal_open() {
        return match key.code {
            KeyCode::Char('y' | 'Y') => allow_or_none(model),
            KeyCode::Char('n' | 'N') => deny_or_none(model),
            KeyCode::Esc => deny_or_none(model),
            _ => KeyOutcome::None,
        };
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    match key.code {
        KeyCode::Char('c') if ctrl => KeyOutcome::Cancel,
        // `r` refreshes the snapshot — but only while the prompt line is
        // empty, so it stays typeable.
        KeyCode::Char('r') if !ctrl && !alt && model.input.is_empty() => KeyOutcome::Refresh,
        KeyCode::Char(c) if !ctrl && !alt => {
            model.push_char(c);
            KeyOutcome::None
        }
        KeyCode::Enter => match model.take_prompt() {
            Some(text) if text == "/quit" => KeyOutcome::Quit,
            Some(text) => KeyOutcome::Prompt(text),
            None => KeyOutcome::None,
        },
        KeyCode::Backspace => {
            model.backspace();
            KeyOutcome::None
        }
        KeyCode::PageUp => {
            model.page_up();
            KeyOutcome::None
        }
        KeyCode::PageDown => {
            model.page_down();
            KeyOutcome::None
        }
        KeyCode::Home => {
            model.follow = false;
            model.scroll_top = 0;
            KeyOutcome::None
        }
        KeyCode::End => {
            model.follow = true;
            KeyOutcome::None
        }
        KeyCode::Esc => KeyOutcome::Esc,
        _ => KeyOutcome::None,
    }
}

fn handle_mouse(model: &mut TuiModel, m: MouseEvent) {
    match m.kind {
        MouseEventKind::ScrollUp => model.scroll_up(3),
        MouseEventKind::ScrollDown => model.scroll_down(3),
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Async runtime: build session → spawn actor → drive it
// ---------------------------------------------------------------------------

enum UiUpdate {
    Snapshot(SessionSnapshot),
    PromptDone(Result<String, String>),
    ApprovalMatched(bool),
}

/// Restore the terminal on every exit path, including early returns.
struct TerminalRestore;

impl Drop for TerminalRestore {
    fn drop(&mut self) {
        let _ = execute!(std::io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}

fn request_snapshot(cmds: &mpsc::Sender<SessionCommand>, upd: mpsc::Sender<UiUpdate>) {
    let (tx, rx) = oneshot::channel();
    let cmds = cmds.clone();
    tokio::spawn(async move {
        if cmds
            .send(SessionCommand::Snapshot { reply: tx })
            .await
            .is_err()
        {
            return; // actor gone; other arms surface that
        }
        if let Ok(snap) = rx.await {
            let _ = upd.send(UiUpdate::Snapshot(snap)).await;
        }
    });
}

fn send_prompt(cmds: mpsc::Sender<SessionCommand>, upd: mpsc::Sender<UiUpdate>, text: String) {
    tokio::spawn(async move {
        let (tx, rx) = oneshot::channel();
        if cmds
            .send(SessionCommand::Prompt { text, reply: tx })
            .await
            .is_err()
        {
            let _ = upd
                .send(UiUpdate::PromptDone(Err(
                    "session actor is not running".into()
                )))
                .await;
            return;
        }
        let res = match rx.await {
            Ok(Ok(stop)) => Ok(stop.normalized().to_string()),
            Ok(Err(e)) => Err(e),
            Err(_) => Err("session actor dropped the prompt reply".into()),
        };
        let _ = upd.send(UiUpdate::PromptDone(res)).await;
    });
}

fn send_cancel(cmds: mpsc::Sender<SessionCommand>) {
    tokio::spawn(async move {
        let (tx, rx) = oneshot::channel();
        if cmds
            .send(SessionCommand::Cancel { reply: tx })
            .await
            .is_ok()
        {
            let _ = rx.await; // fires when the in-flight turn unwinds
        }
    });
}

fn send_approve(
    cmds: mpsc::Sender<SessionCommand>,
    upd: mpsc::Sender<UiUpdate>,
    tool: String,
    summary: String,
    answer: ApprovalAnswer,
) {
    tokio::spawn(async move {
        let (tx, rx) = oneshot::channel();
        let cmd = SessionCommand::Approve {
            tool,
            summary,
            answer,
            reply: tx,
        };
        if cmds.send(cmd).await.is_err() {
            let _ = upd.send(UiUpdate::ApprovalMatched(false)).await;
            return;
        }
        let matched = rx.await.unwrap_or_default();
        let _ = upd.send(UiUpdate::ApprovalMatched(matched)).await;
    });
}

/// `marfim tui` entry: build the session through the SessionHost (the
/// same owner every other surface uses), then drive the actor from the
/// event loop below.
pub async fn run(
    _cli: &marfim_app::settings::Cli,
    config: marfim_core::config::Config,
    settings: marfim_app::settings::EffectiveSettings,
) -> i32 {
    // One host, one hosted session; the host owns the actor lifecycle.
    let host = marfim_app::factory::make_host(
        &config,
        Some(settings.clone()),
        _cli.clone(),
        settings.cwd.clone(),
    );
    let hosted = match host
        .create(marfim_core::session_host::SessionRequest::default())
        .await
    {
        Ok(h) => h,
        Err(e) => {
            eprintln!("marfim tui: session failed: {e}");
            return 1;
        }
    };
    let session_id = hosted.actor.session_id.clone();
    // The TUI owns approval intake: unanswered jobs surface in the modal.
    let mut approval_rx = hosted.take_approval_receiver().unwrap_or_else(|| {
        let (_tx, rx) = tokio::sync::mpsc::channel(1);
        rx
    });
    let bus = hosted.bus.clone();

    let palette = Palette::detect(&settings.theme);
    let mut model = TuiModel::new(session_id, settings.model.clone(), palette);
    // Distillation §H1: header ctx gauge needs the catalog window.
    model.context_window = marfim_core::catalog::catalog_provider_for(&settings.provider_name)
        .and_then(|pid| marfim_core::catalog::lookup_fuzzy(pid, &settings.model))
        .and_then(|m| m.context_tokens);
    model.snapshot_wanted = true;

    if let Err(e) = enable_raw_mode() {
        eprintln!("marfim tui: cannot enable raw mode: {e}");
        return 1;
    }
    if let Err(e) = execute!(std::io::stdout(), EnterAlternateScreen, EnableMouseCapture) {
        let _ = disable_raw_mode();
        eprintln!("marfim tui: cannot enter alternate screen: {e}");
        return 1;
    }
    let _restore = TerminalRestore;

    let backend = CrosstermBackend::new(std::io::stdout());
    let mut terminal = match Terminal::new(backend) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("marfim tui: terminal init failed: {e}");
            return 1;
        }
    };

    // D14: ONE stdin owner — a plain thread reads crossterm events into a
    // bounded channel; the async loop never blocks on the terminal.
    let (key_tx, mut key_rx) = mpsc::channel::<CtEvent>(64);
    std::thread::spawn(move || {
        while let Ok(ev) = crossterm::event::read() {
            if key_tx.blocking_send(ev).is_err() {
                break; // UI loop gone
            }
        }
    });

    let (upd_tx, mut upd_rx) = mpsc::channel::<UiUpdate>(64);
    let mut sub = bus.subscribe();
    let mut ticker = tokio::time::interval(Duration::from_millis(500));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut esc_at: Option<Instant> = None;
    let mut bus_closed = false;
    let mut quit = false;
    let mut rc = 0;

    loop {
        if let Err(e) = terminal.draw(|f| draw(f, &mut model)) {
            eprintln!("marfim tui: draw failed: {e}");
            rc = 1;
            break;
        }
        tokio::select! {
            ev = sub.recv(), if !bus_closed => match ev {
                Ok(event) => model.apply_event(&event),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    model.note(format!("event bus lagged; {n} event(s) dropped"));
                }
                Err(_) => {
                    bus_closed = true;
                    model.note("event bus closed".into());
                }
            },
            Some(job) = approval_rx.recv() => {
                // The forwarded job's `respond` is a tombstone (the actor
                // dropped its receiver on purpose); the authoritative answer
                // path is the Approve COMMAND. Dropping the job here is the
                // fail-closed path for the tombstone.
                model.open_approval(&job.tool, &job.summary);
            }
            Some(ev) = key_rx.recv() => match ev {
                CtEvent::Key(key) => match handle_key(&mut model, key) {
                    KeyOutcome::None => {}
                    KeyOutcome::Prompt(text) => {
                        model.push_user_prompt(&text);
                        send_prompt(hosted.actor.commands.clone(), upd_tx.clone(), text);
                    }
                    KeyOutcome::Cancel => {
                        model.cancelled = true;
                        send_cancel(hosted.actor.commands.clone());
                    }
                    KeyOutcome::Approve(tool, summary, answer) => {
                        send_approve(hosted.actor.commands.clone(), upd_tx.clone(), tool, summary, answer);
                    }
                    KeyOutcome::Refresh => {
                        request_snapshot(&hosted.actor.commands, upd_tx.clone());
                    }
                    KeyOutcome::Esc => {
                        let now = Instant::now();
                        let double = esc_at
                            .map(|t| now.duration_since(t) <= ESC_QUIT_WINDOW)
                            .unwrap_or(false);
                        if double {
                            quit = true;
                        } else {
                            esc_at = Some(now);
                        }
                    }
                    KeyOutcome::Quit => quit = true,
                },
                CtEvent::Mouse(m) => handle_mouse(&mut model, m),
                _ => {}
            },
            upd = upd_rx.recv() => match upd {
                Some(UiUpdate::Snapshot(s)) => model.apply_snapshot(&s),
                Some(UiUpdate::PromptDone(Ok(_))) => {
                    model.running = false;
                    model.snapshot_wanted = true;
                }
                Some(UiUpdate::PromptDone(Err(e))) => {
                    model.note(format!("error: {e}"));
                    model.snapshot_wanted = true;
                }
                Some(UiUpdate::ApprovalMatched(ok)) => {
                    if !ok {
                        model.note("approval no longer pending (already resolved)".into());
                    }
                    model.snapshot_wanted = true;
                }
                None => {}
            },
            _ = ticker.tick() => {
                // Snapshot poll: ~every 500 ms while a turn runs, plus on
                // demand (r key / after a turn ends).
                if model.running || model.snapshot_wanted {
                    model.snapshot_wanted = false;
                    request_snapshot(&hosted.actor.commands, upd_tx.clone());
                }
            }
        }
        if quit {
            break;
        }
    }

    let _ = hosted.actor.commands.send(SessionCommand::Shutdown).await;
    let _ = terminal.show_cursor();
    rc
}
