//! The agent loop (INV-01..07,10):
//!  - user message lands in session history at turn start (D1)
//!  - history lives on the Harness across turns (D2); the session actor
//!    drives the same structure across every surface (tui/headless/serve)
//!  - real steering: amend vs follow-up, drained ONLY after a full
//!    tool-result batch (or as the seed of the next turn) (D3)
//!  - compaction on per-step context tokens, summary re-injected as the
//!    first message, Small-role usage ledgered (D4)
//!  - every tool_use gets a tool_result, no duplicate assistant message (D5)
//!  - no expect/unwrap on fallible paths; journal write errors stop the
//!    turn (D6)
//!  - any mutation (incl. Bash) arms verify; reset after pass; `[verify]`
//!    output marker honoured (D7)
//!  - CancellationToken from Ctrl-C into loop+tools; concurrency_safe tools
//!    run in parallel; sync tools via spawn_blocking; timeout_ms enforced (D8)
//!  - stream via chat_step_stream onto the bus (D9)
//!  - every stop path emits TurnEnded{stop_reason} (D12)

use crate::events::{Event, EventBus};
use crate::journal::{Journal, JournalRecord};
use crate::providers::{ChatMessage, ChatRequest, StreamEvent, ToolCall, Usage};
use crate::router::{ModelEntry, Role, Router};
use serde_json::Value;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{Notify, RwLock};

/// Cooperative cancellation (D8): Ctrl-C sets the flag, waiters wake.
///
/// §X audit (distilled: OMP `pi-natives/task.rs` CancelToken split —
/// `abort_reason()` excludes elapsed deadlines so completed work is never
/// spuriously rejected): Marfim's token is FLAG-ONLY and never conflated
/// with deadlines — timeouts live in `tokio::time::timeout` wrappers
/// (exec_one, the provider transports) that produce their own error and
/// never touch this flag. A completed tool result can therefore never be
/// retroactively reclassified as aborted. Conclusion: the OMP split
/// semantics are already held by construction; no deadline field was
/// added to keep the invariant trivially checkable.
#[derive(Clone, Default)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl CancelToken {
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// Resolves once cancelled (never otherwise).
    pub async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        let notified = self.notify.notified();
        if self.is_cancelled() {
            return;
        }
        notified.await;
    }
}

/// Trait the loop needs from the tool layer (implemented by marfim-tools).
#[async_trait::async_trait]
pub trait ToolHost: Send + Sync {
    /// Execute a tool call. Returns (output, is_error).
    async fn execute(&self, tool: &str, args: &Value, session: &LoopCtx) -> (String, bool);
    /// JSON schemas for the current wire (visible tools only).
    fn schemas(&self) -> Vec<Value>;
    /// Whether this tool mutates files (verify gating). Bash counts: any
    /// shell execution arms verify (D7).
    fn is_mutating(&self, tool: &str) -> bool;
    /// Commands to run for verification of the current project.
    fn verify_commands(&self) -> Vec<String>;
    /// Run verification; returns (passed, output).
    async fn run_verify(&self, session: &LoopCtx) -> (bool, String);
    /// Whether the tool may run concurrently with siblings in one batch (D8).
    fn concurrency_safe(&self, tool: &str) -> bool {
        let _ = tool;
        false
    }
    /// Enforced per-call timeout in milliseconds (D8).
    fn tool_timeout_ms(&self, tool: &str) -> Option<u64> {
        let _ = tool;
        None
    }
}

#[derive(Clone, Default)]
pub struct LoopCtx {
    pub session_id: String,
    pub cwd: String,
    /// Cooperative cancellation handed down to tools (D8).
    pub cancel: CancelToken,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SteerClass {
    /// `amend`-class: replaces the queued follow-up
    Amend,
    /// follow-up: appended after the current tool batch
    FollowUp,
}

/// Classification for a steering line typed mid-turn: an explicit
/// `amend:`/`amend ` prefix amends; everything else is a follow-up.
pub fn classify_steer(text: &str) -> SteerClass {
    let t = text.trim_start();
    if t.starts_with("amend:") || t.starts_with("amend ") {
        SteerClass::Amend
    } else {
        SteerClass::FollowUp
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    EndTurn,
    Aborted(String),
    CompactionFailed(String),
    VerifyExhausted(String),
    BudgetExceeded(String),
}

impl StopReason {
    /// Normalized stop reason string for TurnEnded + headless exit codes.
    pub fn normalized(&self) -> &'static str {
        match self {
            StopReason::EndTurn => "end_turn",
            StopReason::Aborted(_) => "aborted",
            StopReason::CompactionFailed(_) => "compaction_failed",
            StopReason::VerifyExhausted(_) => "verify_exhausted",
            StopReason::BudgetExceeded(_) => "budget_exceeded",
        }
    }
}

/// Sampling overrides applied to EVERY primary provider request (config
/// truth: `maxTokens`/`temperature`/`thinkingBudget`/`effort` from
/// settings reach the wire instead of dying in an unused struct field).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Sampling {
    pub max_completion_tokens: Option<u64>,
    pub temperature: Option<f64>,
    pub thinking_budget: Option<u64>,
    /// Canonical effort ladder (distillation §R1) — each wire emits its
    /// own dialect from this one value.
    pub effort: Option<crate::providers::effort::Effort>,
}

pub struct Harness {
    pub bus: EventBus,
    pub router: Arc<RwLock<Router>>,
    pub tools: Arc<dyn ToolHost>,
    pub journal: Arc<std::sync::Mutex<Journal>>,
    pub ctx: LoopCtx,
    pub permissions: Arc<crate::permissions::PermissionEngine>,
    pub max_steps: u64,
    pub max_verify_feedback: u32,
    /// Session-scoped conversation history (D2): survives across turns.
    pub history: Arc<RwLock<Vec<ChatMessage>>>,
    /// Cooperative cancellation into loop + tools (D8).
    pub cancel: CancelToken,
    /// Per-step context-token threshold that triggers compaction (D4).
    pub compact_threshold_tokens: u64,
    /// Sampling overrides for provider requests (config truth).
    pub sampling: Sampling,
    /// Catalog context window for the primary model, when known — feeds
    /// the distilled compaction trigger math (§C1). `None` keeps the
    /// plain configured threshold.
    pub context_window: Option<u64>,
    /// Monotonic turn counter (TurnStarted/TurnEnded carry the real turn).
    pub turn_counter: AtomicU64,
    /// Tool calls denied by the permission engine this session (D12 exit 2).
    pub denied_tools: AtomicU64,
    /// verify armed by a mutation this batch (D7).
    verify_armed: AtomicBool,
    /// --on-fail abort: a tool error ends the turn immediately (D11 flag
    /// consumption in headless pipelines).
    pub stop_on_tool_error: bool,
    /// Tool calls executed this session (D12 exit-code bookkeeping).
    pub executed_tools: AtomicU64,
    /// Two-queue steering (omp-style): amend vs follow-up (D3).
    steer: Arc<std::sync::Mutex<SteerQueues>>,
    /// P4 faculties attached to this session (observational hooks).
    pub faculties: Vec<Arc<dyn crate::faculty::Faculty>>,
    /// Turn-scoped actuation overrides (the four bounded powers). Set at
    /// turn start from validated faculty actuations, cleared at turn end:
    /// an actuation never outlives the turn it was granted for.
    turn_overrides: std::sync::Mutex<TurnOverrides>,
    /// §A consecutive permission denials this turn (circuit breaker).
    consecutive_denials: std::sync::Mutex<u32>,
    /// §W whether the previous turn ended Aborted (drives the interrupt
    /// placeholder healing at the next turn start).
    last_turn_aborted: AtomicBool,
    /// W5.5 tool_monitor (goose): the last call's identity (name+args
    /// hash) and its consecutive repeat count within THIS turn. The cap
    /// refuses the third identical call — a model stuck in a loop gets an
    /// honest, actionable refusal instead of burning budget.
    last_call: std::sync::Mutex<(Option<u64>, u32)>,
    /// W5.5 per-turn tool budget: total tool calls in the current turn
    /// (reset at TurnStarted; the cap refuses further dispatch).
    turn_tool_calls: AtomicU64,
    /// W8 defrag ledger (§C, Hermes micro_compaction): consecutive
    /// micro-compaction sweeps that failed to reclaim the 2000-token
    /// minimum. At 3 the micro stage is skipped entirely (the history is
    /// already compressed — churning it wastes steps) until a full
    /// summarization resets the counter.
    defrag_fails: std::sync::atomic::AtomicU32,
    /// W8 ordered strategies (§C): the compaction stage order from
    /// `[compaction] strategies` (default promote → micro → summarize).
    pub compaction_strategies: Vec<crate::compact::Strategy>,
    /// W8 async compaction: the ONE background summarization task
    /// (started past the soft threshold, consumed at the hard trigger).
    pending_compaction: std::sync::Mutex<Option<PendingCompaction>>,
    /// W8 async compaction: history length when the background summary's
    /// snapshot was taken — messages beyond it survive compaction as the
    /// protected tail.
    compaction_snapshot_len: std::sync::atomic::AtomicU64,
    /// W7.2 cache plumbing: the `[cache]` config section (ttl /
    /// cachedContentName / warm), resolved once at settings time.
    pub cache: crate::providers::cache::CacheSettings,
    /// W7.5 (FORMER-REJECTS adoption form): pass `custom`-grammar tools
    /// through the Responses wire verbatim (default OFF — function-tool
    /// fallback).
    pub allow_custom_tools: bool,
    /// W7.2: the warming ping fires at most once per session.
    cache_warmed: AtomicBool,
    /// W10.1: the session-start prompt surfaces (capped memory render,
    /// active instincts, active skills) — computed by the factory from
    /// the stores, injected byte-identically-empty when unset.
    pub surfaces: PromptSurfaces,
}

/// W10.1: the session-start prompt surfaces. Each section is `Some` only
/// when its store has content — an empty store keeps the base prompt
/// byte-identical (reversible, observable).
#[derive(Debug, Clone, Default)]
pub struct PromptSurfaces {
    /// capped render of the project memory store (memcaps::MEMORY_CAP)
    pub memory: Option<String>,
    /// instincts::InstinctStore::render_active()
    pub instincts: Option<String>,
    /// curator::SkillBook::render_active()
    pub skills: Option<String>,
}

/// Turn-scoped faculty overrides. Every field is optional and validated:
/// the model must exist in the chain, the compaction threshold is clamped
/// to [0.5x, 1.5x] of the configured value, tightened tools only ADD
/// denies (deny>ask>allow: the PermissionEngine stays the sole authority).
#[derive(Default)]
struct TurnOverrides {
    model: Option<String>,
    tightened_tools: Vec<String>,
    compact_threshold: Option<u64>,
}

/// §R2 prompt-cache affinity: `pc_` + the session id, truncated so the
/// whole key stays ≤64 chars (OMP wire rule). Deterministic per session.
pub fn prompt_cache_key(session_id: &str) -> String {
    let mut k = format!("pc_{session_id}");
    k.truncate(64);
    k
}

/// W5.5 (goose tool_monitor doctrine): the per-turn tool-call budget and
/// the identical-call cap. A tool call repeated VERBATIM this many times
/// in one turn is a loop — the next identical call is refused with the
/// remedy in the message. Denials do NOT count toward the streak (the
/// denial circuit breaker owns that path; the two guards stay
/// orthogonal — a denied call is the breaker's business, not a loop).
/// W8 async compaction (§C): the background summarizer starts at this
/// fraction of the hard trigger — the expensive Small-role call overlaps
/// the rest of the turn instead of blocking the next one.
pub const COMPACTION_SOFT_RATIO: f64 = 0.7;

/// W8 async compaction: the pending background summarization handle.
pub type PendingCompaction = tokio::task::JoinHandle<crate::Result<(String, Usage, String)>>;

pub const MAX_TOOL_CALLS_PER_TURN: u64 = 200;
pub const MAX_IDENTICAL_CALLS: u32 = 3;

/// FNV-1a over the call identity (tool name + canonical args JSON) — a
/// loop-detection fingerprint, not a security primitive.
fn call_fingerprint(tool: &str, args: &Value) -> u64 {
    use std::hash::Hasher;
    let mut h = std::hash::DefaultHasher::new();
    h.write(tool.as_bytes());
    h.write(&[0]);
    h.write(args.to_string().as_bytes());
    h.finish()
}

/// §W interrupt placeholder healing (distilled: Hermes structural
/// placeholders, never prose): an aborted exchange closes with this
/// EXACT structural marker, so replay never echo-poisons the model
/// into answering a message that was never answered.
pub const INTERRUPT_PLACEHOLDER: &str = "[interrupt: aborted]";

/// §A denial circuit breaker (distilled: Hermes anti-retry contract):
/// after this many CONSECUTIVE permission denials in one turn, an
/// explicit stop-addendum lands in the conversation — identical retries
/// keep failing; the model must change approach or ask.
pub const DENIAL_BREAKER_LIMIT: u32 = 3;

/// §S memory nudge cadence (distilled: Hermes memory loop): every Nth
/// turn the system prompt carries a one-line review reminder.
pub const NUDGE_EVERY_TURNS: u64 = 10;
pub const MEMORY_NUDGE: &str =
    "[memory cadence] turn checkpoint: if anything durable about the user or project surfaced (preferences, constraints, invariants), record it via the memory tool now.";

/// Steering queues (omp-style two-queue: amend vs follow-up).
#[derive(Default)]
pub struct SteerQueues {
    pub amend: Vec<String>,
    pub follow_ups: Vec<String>,
}

impl SteerQueues {
    pub fn submit_follow_up(&mut self, text: &str) {
        self.follow_ups.push(text.to_string());
    }

    /// An amend REPLACES the queued follow-up (the follow-up is superseded).
    pub fn submit_amend(&mut self, text: &str) {
        self.follow_ups.clear();
        self.amend.push(text.to_string());
    }

    /// Submit with automatic classification.
    pub fn submit(&mut self, text: &str) -> SteerClass {
        match classify_steer(text) {
            SteerClass::Amend => {
                self.submit_amend(text);
                SteerClass::Amend
            }
            SteerClass::FollowUp => {
                self.submit_follow_up(text);
                SteerClass::FollowUp
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.amend.is_empty() && self.follow_ups.is_empty()
    }

    /// Drain ONE message. Amend supersedes queued follow-ups.
    pub fn drain(&mut self) -> Option<String> {
        if let Some(a) = self.amend.pop() {
            return Some(a);
        }
        if !self.follow_ups.is_empty() {
            return Some(self.follow_ups.remove(0));
        }
        None
    }
}

impl Harness {
    /// Public constructor: internal invariants (verify arming, steering
    /// queues, counters) start in their canonical state.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        bus: EventBus,
        router: Arc<RwLock<Router>>,
        tools: Arc<dyn ToolHost>,
        journal: Arc<std::sync::Mutex<Journal>>,
        ctx: LoopCtx,
        permissions: Arc<crate::permissions::PermissionEngine>,
        max_steps: u64,
        max_verify_feedback: u32,
        compact_threshold_tokens: u64,
        stop_on_tool_error: bool,
    ) -> Self {
        Self {
            bus,
            router,
            tools,
            journal,
            ctx,
            permissions,
            max_steps,
            max_verify_feedback,
            history: Arc::new(RwLock::new(Vec::new())),
            cancel: CancelToken::default(),
            compact_threshold_tokens,
            sampling: Sampling::default(),
            context_window: None,
            turn_counter: AtomicU64::new(0),
            last_call: std::sync::Mutex::new((None, 0)),
            turn_tool_calls: AtomicU64::new(0),
            defrag_fails: std::sync::atomic::AtomicU32::new(0),
            cache: crate::providers::cache::CacheSettings::default(),
            allow_custom_tools: false,
            compaction_strategies: crate::compact::DEFAULT_STRATEGIES.to_vec(),
            pending_compaction: std::sync::Mutex::new(None),
            compaction_snapshot_len: std::sync::atomic::AtomicU64::new(0),
            cache_warmed: AtomicBool::new(false),
            denied_tools: AtomicU64::new(0),
            verify_armed: AtomicBool::new(false),
            stop_on_tool_error,
            executed_tools: AtomicU64::new(0),
            steer: Arc::new(std::sync::Mutex::new(SteerQueues::default())),
            faculties: Vec::new(),
            turn_overrides: std::sync::Mutex::new(TurnOverrides::default()),
            consecutive_denials: std::sync::Mutex::new(0),
            last_turn_aborted: AtomicBool::new(false),
            surfaces: PromptSurfaces::default(),
        }
    }

    /// P4: attach a faculty (observability + narrow influence hooks).
    pub fn add_faculty(&mut self, f: Arc<dyn crate::faculty::Faculty>) {
        self.faculties.push(f);
    }

    /// P4: faculty context for the current position.
    fn hook_ctx(&self, step: u64) -> crate::faculty::HookCtx {
        crate::faculty::HookCtx {
            session_id: self.ctx.session_id.clone(),
            cwd: self.ctx.cwd.clone(),
            turn: self.turn_counter.load(Ordering::SeqCst),
            step,
        }
    }

    /// P4 idle surface: collect proposals from all faculties (the UI
    /// approval flow consumes them; nothing executes silently).
    pub async fn idle_proposals(&self) -> Vec<crate::faculty::Proposal> {
        let ctx = self.hook_ctx(0);
        crate::faculty::run_idle(&self.bus, &self.faculties, &ctx).await
    }

    pub async fn run_turn(&self, user_text: &str) -> StopReason {
        let turn = self.turn_counter.fetch_add(1, Ordering::SeqCst) + 1;
        self.bus.publish(Event::TurnStarted { turn });
        // W5.5 tool_monitor reset: every turn starts with a clean budget
        // and a clean repeat tracker.
        self.turn_tool_calls.store(0, Ordering::SeqCst);
        *self.last_call.lock().unwrap_or_else(|p| p.into_inner()) = (None, 0);

        // P4 turn-start actuations: collect from the faculties, validate
        // and clamp each, journal + apply what survived. An actuation that
        // names a model outside the chain, a threshold outside the clamp
        // band, or an unknown kind is DROPPED here (fail-visible on the
        // bus, never applied).
        {
            let ctx = self.hook_ctx(0);
            let acts = crate::faculty::run_actuations(&self.bus, &self.faculties, &ctx).await;
            for (faculty, a) in acts {
                match a.kind.as_str() {
                    "model_select" => {
                        let want = a.after.as_str().unwrap_or("").to_string();
                        let before = a.before.as_str().unwrap_or("").to_string();
                        let in_chain = self
                            .router
                            .read()
                            .await
                            .chain(Role::Primary)
                            .iter()
                            .any(|e| e.model == want);
                        if in_chain && !want.is_empty() && want != before {
                            self.record_actuation(&faculty, "model_select", &a, "applied");
                            self.turn_overrides
                                .lock()
                                .unwrap_or_else(|p| p.into_inner())
                                .model = Some(want);
                        } else {
                            self.record_actuation(
                                &faculty,
                                "model_select",
                                &a,
                                if in_chain {
                                    "dropped: no change"
                                } else {
                                    "dropped: model not in chain"
                                },
                            );
                        }
                    }
                    "compact_threshold" => {
                        let base = self.compact_threshold_tokens;
                        let lo = (base / 2).max(1);
                        let hi = base.saturating_mul(3) / 2;
                        let want = a.after.as_u64().unwrap_or(base);
                        if want < lo || want > hi {
                            self.record_actuation(
                                &faculty,
                                "compact_threshold",
                                &a,
                                "dropped: outside [0.5x, 1.5x] clamp",
                            );
                        } else if want == base {
                            self.record_actuation(
                                &faculty,
                                "compact_threshold",
                                &a,
                                "dropped: no change",
                            );
                        } else {
                            self.record_actuation(&faculty, "compact_threshold", &a, "applied");
                            self.turn_overrides
                                .lock()
                                .unwrap_or_else(|p| p.into_inner())
                                .compact_threshold = Some(want);
                        }
                    }
                    "permission_tighten" => {
                        let tools: Vec<String> = a
                            .after
                            .as_array()
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|v| v.as_str().map(str::to_string))
                                    .collect()
                            })
                            .unwrap_or_default();
                        if tools.is_empty() {
                            self.record_actuation(
                                &faculty,
                                "permission_tighten",
                                &a,
                                "dropped: empty deny list",
                            );
                        } else {
                            self.record_actuation(&faculty, "permission_tighten", &a, "applied");
                            let mut ov = self
                                .turn_overrides
                                .lock()
                                .unwrap_or_else(|p| p.into_inner());
                            for t in tools {
                                if !ov.tightened_tools.contains(&t) {
                                    ov.tightened_tools.push(t);
                                }
                            }
                        }
                    }
                    "idle_propose" => {
                        // Proposals never execute: surface only (already
                        // published on the bus by run_actuations).
                        self.record_actuation(&faculty, "idle_propose", &a, "surfaced only");
                    }
                    other => {
                        self.record_actuation(
                            &faculty,
                            other,
                            &a,
                            "dropped: unknown actuation kind",
                        );
                    }
                }
            }
        }

        // D1: the user message lands in history at turn start.
        // §W healing first: an aborted previous turn may have left a
        // trailing unanswered user message — close that exchange with the
        // structural interrupt placeholder before appending a new one.
        {
            let aborted = self.last_turn_aborted.load(Ordering::SeqCst);
            let mut h = self.history.write().await;
            if aborted && h.last().map(|m| m.role == "user").unwrap_or(false) {
                h.push(ChatMessage::assistant(INTERRUPT_PLACEHOLDER));
            }
        }
        if let Err(e) = self.journal_rec(&JournalRecord::UserMessage {
            text: user_text.into(),
        }) {
            return self.finish_turn(turn, StopReason::Aborted(format!("{e}")));
        }
        self.history
            .write()
            .await
            .push(ChatMessage::user(user_text.to_string()));

        let stop = match self.run_turn_inner().await {
            Ok(s) => s,
            Err(e) => {
                self.bus.publish(Event::Error {
                    message: format!("{e}"),
                    fatal: true,
                });
                StopReason::Aborted(format!("{e}"))
            }
        };
        self.finish_turn(turn, stop)
    }

    /// D12: every stop path funnels through here, so TurnEnded is always
    /// emitted exactly once with the normalized stop reason.
    fn finish_turn(&self, turn: u64, stop: StopReason) -> StopReason {
        // Actuations are turn-scoped: clear them when the turn unwinds.
        *self
            .turn_overrides
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = TurnOverrides::default();
        // §A the denial breaker is turn-scoped too.
        *self
            .consecutive_denials
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = 0;
        // §W record the abort so the next turn heals the exchange.
        self.last_turn_aborted
            .store(matches!(stop, StopReason::Aborted(_)), Ordering::SeqCst);
        self.bus.publish(Event::TurnEnded {
            turn,
            stop_reason: stop.normalized().into(),
        });
        stop
    }

    fn journal_rec(&self, rec: &JournalRecord) -> crate::Result<()> {
        let mut j = self.journal.lock().unwrap_or_else(|p| p.into_inner());
        j.append(rec)
            .map(|_| ())
            .map_err(|e| crate::CoreError::Session(format!("journal write failed: {e}")))
    }

    /// Persist one actuation decision (applied or dropped, with the
    /// disposition recorded) to the session journal: the full
    /// before/after/evidence triple is on the bus Event::Actuation, the
    /// journal record keeps the session history complete for /replay and
    /// forensic replay A/B.
    fn record_actuation(
        &self,
        faculty: &str,
        kind: &str,
        a: &crate::faculty::Actuation,
        disposition: &str,
    ) {
        let _ = self.journal_rec(&JournalRecord::Actuation {
            detail: serde_json::json!({
                "faculty": faculty,
                "kind": kind,
                "before": a.before,
                "after": a.after,
                "evidence": a.evidence,
                "disposition": disposition,
            }),
        });
    }

    fn steer_lock(&self) -> std::sync::MutexGuard<'_, SteerQueues> {
        self.steer.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Steering entry points (D3): queued, consumed only after a full
    /// tool-result batch. Both journal the Steer record.
    pub fn steer(&self, text: &str) {
        let class = self.steer_lock().submit(text);
        let _ = self.journal_rec(&JournalRecord::Steer {
            text: format!("[{class:?}] {text}"),
        });
    }

    pub fn steer_amend(&self, text: &str) {
        self.steer_lock().submit_amend(text);
        let _ = self.journal_rec(&JournalRecord::Steer {
            text: format!("[Amend] {text}"),
        });
    }

    pub fn pending_steers(&self) -> usize {
        let q = self.steer_lock();
        q.amend.len() + q.follow_ups.len()
    }

    async fn run_turn_inner(&self) -> crate::Result<StopReason> {
        let mut usage_total = Usage::default();
        let mut steps: u64 = 0;
        let mut verify_used = 0u32;
        // D4: per-step context tokens (the LAST step's usage), not cumulative.
        let mut last_step_input_tokens: u64;
        // §S memory-cadence base: the current turn number (already incremented).
        let turn_counter_local = self.turn_counter.load(Ordering::SeqCst);

        loop {
            // D8: Ctrl-C between steps ends the turn.
            if self.cancel.is_cancelled() {
                return Ok(StopReason::Aborted("cancelled by user".into()));
            }
            steps += 1;
            if steps > self.max_steps {
                return Ok(StopReason::BudgetExceeded(format!(
                    "step budget {} exceeded",
                    self.max_steps
                )));
            }

            // 1. build request from session history (user msg already in it)
            let history_snapshot = self.history.read().await.clone();
            if history_snapshot.is_empty() {
                return Ok(StopReason::Aborted("empty history".into()));
            }

            let req = ChatRequest {
                messages: history_snapshot,
                tools: self.tools.schemas(),
                cache_breakpoints: true,
                // W7.2: the [cache] config section rides every primary
                // request (None/default = byte-identical bodies).
                cache_ttl: self.cache.ttl,
                cached_content: self.cache.cached_content.clone(),
                // W7.5: custom-grammar tools pass through only when the
                // config turned the adoption form on.
                allow_custom_tools: self.allow_custom_tools,
                system: Some(system_prompt_with_surfaces(
                    &self.ctx,
                    // §S memory cadence: every Nth turn carries the
                    // one-line review reminder in the system surface.
                    turn_counter_local
                        .is_multiple_of(NUDGE_EVERY_TURNS)
                        .then_some(MEMORY_NUDGE),
                    &self.surfaces,
                )),
                max_completion_tokens: self.sampling.max_completion_tokens,
                temperature: self.sampling.temperature,
                thinking_budget: self.sampling.thinking_budget,
                effort: self.sampling.effort,
                // §R2 session-derived prompt-cache affinity key.
                prompt_cache_key: Some(prompt_cache_key(&self.ctx.session_id)),
            };

            // P4 pre_model hook (observational)
            crate::faculty::run_pre_model(&self.bus, &self.faculties, &self.hook_ctx(steps), &req)
                .await;

            // W7.2 (§P): the bounded cache-warming ping — at most ONCE per
            // session, before the first provider step, when `cache.warm`
            // is on. The result is discarded; the outcome is observable on
            // the bus (CacheWarm) and never fatal: a failed warm leaves
            // the turn untouched.
            if self.cache.warm && !self.cache_warmed.swap(true, Ordering::SeqCst) {
                let sys = req.system.clone().unwrap_or_default();
                let warm_req = crate::providers::cache::warm_request(
                    &sys,
                    &prompt_cache_key(&self.ctx.session_id),
                );
                let primary = self
                    .router
                    .read()
                    .await
                    .chain(Role::Primary)
                    .first()
                    .cloned();
                let (ok, model) = match primary {
                    Some(entry) => {
                        let ok = entry.provider.chat_step(&warm_req).await.is_ok();
                        (ok, entry.model)
                    }
                    None => (false, String::new()),
                };
                self.bus.publish(Event::CacheWarm { ok, model });
            }

            // 2. call provider — D11 real failover: on a transport error the
            //    model is marked unavailable and the next chain entry is
            //    tried; streaming onto the bus (D9), cancellable (D8). The
            //    provider transport owns the ONLY retry layer.
            let chain_len = self.router.read().await.chain(Role::Primary).len();
            let mut entry = None;
            let mut step = None;
            for attempt in 0..chain_len.max(1) {
                let (e, tried) = {
                    let router = self.router.read().await;
                    // P4 model_select actuation: when the faculty picked a
                    // model for THIS turn and it is in the chain, try it
                    // first; on failure the normal failover predicate takes
                    // over for the remaining attempts.
                    let prefer = {
                        let ov = self
                            .turn_overrides
                            .lock()
                            .unwrap_or_else(|p| p.into_inner());
                        ov.model.clone()
                    };
                    let filter: Box<dyn Fn(&ModelEntry) -> bool> = match &prefer {
                        Some(m) if attempt == 0 => {
                            let want = m.clone();
                            Box::new(move |e: &ModelEntry| e.model == want)
                        }
                        _ => Box::new(|_: &ModelEntry| true),
                    };
                    router.resolve(Role::Primary, filter)?
                };
                self.bus.publish(Event::ModelSelected {
                    role: Role::Primary.as_str().into(),
                    model: e.model.clone(),
                    why: self
                        .router
                        .read()
                        .await
                        .explain(Role::Primary, &e.model, &tried),
                });
                let bus = self.bus.clone();
                let cancel = self.cancel.clone();
                let mut on_ev = |ev: StreamEvent| match ev {
                    StreamEvent::Text(t) => bus.publish(Event::StreamDelta {
                        kind: "text".into(),
                        text: t,
                    }),
                    StreamEvent::Thinking(t) => bus.publish(Event::StreamDelta {
                        kind: "thinking".into(),
                        text: t,
                    }),
                };
                let step_t0 = std::time::Instant::now();
                match tokio::select! {
                    r = e.provider.chat_step_stream(&req, &mut on_ev) => r,
                    _ = cancel.cancelled() => {
                        return Ok(StopReason::Aborted("cancelled by user".into()));
                    }
                } {
                    Ok(s) => {
                        // P3 bandit routing input: observed per-model outcome
                        self.router.write().await.report_outcome(
                            Role::Primary,
                            &e.model,
                            true,
                            step_t0.elapsed().as_secs_f64() * 1000.0,
                        );
                        self.router
                            .write()
                            .await
                            .mark_success(Role::Primary, &e.model);
                        entry = Some(e);
                        step = Some(s);
                        break;
                    }
                    Err(err) => {
                        self.router.write().await.report_outcome(
                            Role::Primary,
                            &e.model,
                            false,
                            step_t0.elapsed().as_secs_f64() * 1000.0,
                        );
                        // Distillation §F1: cooldown ladder — 60s doubling
                        // per consecutive failure, capped at 4h (provider
                        // Retry-After would win if it were longer, but the
                        // transport layer already consumed the header into
                        // its own retry loop; the ladder covers the rest).
                        self.router
                            .write()
                            .await
                            .mark_failure(Role::Primary, &e.model, None);
                        // W7.3 recovery rung (§F): a 400-SHAPE error names a
                        // parameter the model rejects — retry THIS model
                        // once with that parameter stripped before the
                        // failover walk moves on. W7.2 adds the cache
                        // ladder, which classifies FIRST (a ttl rejection
                        // is strictly more specific than a parameter shape
                        // error): ttl removed → then every cache marker
                        // removed. One rung per attempt; ambiguous errors
                        // never classify.
                        let cache_stripped =
                            crate::providers::cache::cache_after_error(&req, &err.to_string());
                        let stripped = cache_stripped.clone().or_else(|| {
                            crate::providers::recovery::strip_after_shape_error(
                                &req,
                                &err.to_string(),
                            )
                        });
                        if let Some(stripped) = stripped {
                            let rung_kind = if cache_stripped.is_some() {
                                "cache marker"
                            } else {
                                "parameter"
                            };
                            self.bus.publish(Event::Error {
                                message: format!(
                                    "provider `{}` rejected a {} ({err}); \
                                     retrying with it stripped",
                                    e.model, rung_kind
                                ),
                                fatal: false,
                            });
                            let bus2 = bus.clone();
                            let cancel2 = cancel.clone();
                            let mut on_ev2 = |ev: StreamEvent| match ev {
                                StreamEvent::Text(t) => bus2.publish(Event::StreamDelta {
                                    kind: "text".into(),
                                    text: t,
                                }),
                                StreamEvent::Thinking(t) => bus2.publish(Event::StreamDelta {
                                    kind: "thinking".into(),
                                    text: t,
                                }),
                            };
                            match tokio::select! {
                                r = e.provider.chat_step_stream(&stripped, &mut on_ev2) => r,
                                _ = cancel2.cancelled() => {
                                    return Ok(StopReason::Aborted("cancelled by user".into()));
                                }
                            } {
                                Ok(s) => {
                                    self.router.write().await.report_outcome(
                                        Role::Primary,
                                        &e.model,
                                        true,
                                        step_t0.elapsed().as_secs_f64() * 1000.0,
                                    );
                                    self.router
                                        .write()
                                        .await
                                        .mark_success(Role::Primary, &e.model);
                                    entry = Some(e);
                                    step = Some(s);
                                    break;
                                }
                                Err(_) => {
                                    // the rung did not save the attempt —
                                    // the failover walk proceeds below
                                }
                            }
                        }
                        if attempt + 1 >= chain_len.max(1) {
                            return Err(err);
                        }
                        self.bus.publish(Event::Error {
                            message: format!("provider `{}` failed ({err}); failing over", e.model),
                            fatal: false,
                        });
                    }
                }
            }
            let entry = entry.ok_or_else(|| {
                crate::CoreError::Provider("no provider entry after failover".into())
            })?;
            let step =
                step.ok_or_else(|| crate::CoreError::Provider("no step after failover".into()))?;
            usage_total.add(&step.usage);
            last_step_input_tokens = step.usage.input_tokens;
            self.bus.publish(Event::Usage {
                role: Role::Primary.as_str().into(),
                model: entry.model.clone(),
                input_tokens: step.usage.input_tokens,
                output_tokens: step.usage.output_tokens,
                cache_read_tokens: step.usage.cache_read_tokens,
                cache_creation_tokens: step.usage.cache_creation_tokens,
                cost_usd: self
                    .router
                    .read()
                    .await
                    .costs
                    .cost_usd(&entry.model, &step.usage),
            });

            let text = step.text();
            if !text.is_empty() {
                self.bus
                    .publish(Event::AssistantMessage { text: text.clone() });
            }

            // 3. tool calls?
            let calls: Vec<ToolCall> = step.tool_calls().iter().map(|t| (*t).clone()).collect();

            // P4 post_step hook: the learning stream (drift/conformal/regret)
            crate::faculty::run_post_step(
                &self.bus,
                &self.faculties,
                &self.hook_ctx(steps),
                &crate::faculty::StepOutcome {
                    model: entry.model.clone(),
                    input_tokens: step.usage.input_tokens,
                    output_tokens: step.usage.output_tokens,
                    had_tool_calls: !calls.is_empty(),
                    had_tool_error: false,
                    text_len: step.text().chars().count() as u64,
                    latency_ms: 0.0,
                },
            )
            .await;

            if calls.is_empty() {
                self.journal_rec(&JournalRecord::AssistantMessage {
                    text: text.clone(),
                    model: entry.model.clone(),
                    stop_reason: step
                        .stop
                        .map(|s| s.as_str().to_string())
                        .unwrap_or_else(|| "end_turn".into()),
                })?;
                self.history
                    .write()
                    .await
                    .push(ChatMessage::assistant(text.clone()));

                // INV: verify only when no tool call AND a mutation armed it
                if self.verify_armed.load(Ordering::SeqCst)
                    && !self.tools.verify_commands().is_empty()
                {
                    if verify_used >= self.max_verify_feedback {
                        self.journal_total_usage(&usage_total, &entry.model)?;
                        return Ok(StopReason::VerifyExhausted(format!(
                            "verify feedback budget {verify_used} exhausted with failing checks"
                        )));
                    }
                    verify_used += 1;
                    let cmds = self.tools.verify_commands();
                    self.bus.publish(Event::VerifyStarted {
                        commands: cmds.clone(),
                    });
                    let (passed, output) = self.tools.run_verify(&self.ctx).await;
                    self.bus.publish(Event::VerifyFinished {
                        passed,
                        output: output.clone(),
                    });
                    if !passed {
                        // feed verify output back into history as a user message
                        self.history.write().await.push(ChatMessage::user(format!(
                            "Verification failed. Fix the issues.\n\n--- verify output ---\n{output}\n--- end ---"
                        )));
                        self.verify_armed.store(false, Ordering::SeqCst);
                        continue;
                    }
                    // D7: verify passed → disarm (a later mutation re-arms)
                    self.verify_armed.store(false, Ordering::SeqCst);
                }

                // D4: compaction also applies to a clean step end so the
                // NEXT turn starts from the summary.
                if let Err(e) = self.maybe_compact(last_step_input_tokens).await {
                    self.journal_total_usage(&usage_total, &entry.model)?;
                    return Ok(StopReason::CompactionFailed(format!("{e}")));
                }

                // Pending steering seeds the next turn (never interrupts a
                // clean step mid-flight; the batch drain point is below).
                let next = self.steer_lock().drain();
                if let Some(s) = next {
                    self.history.write().await.push(ChatMessage::user(s));
                    continue;
                }

                self.journal_total_usage(&usage_total, &entry.model)?;
                return Ok(StopReason::EndTurn);
            }

            // 4. truncation check (INV-03 + D5): any call with unparseable
            //    JSON → re-request BEFORE the truncated call runs; the ONE
            //    assistant message carries all tool_calls, and EVERY call
            //    gets a tool_result.
            let has_bad = calls.iter().any(|tc| tc.parse_args().is_err());
            if has_bad {
                let truncated_note = format!(
                    "TOOL_CALL_TRUNCATED: arguments JSON was incomplete (stop={}). \
                     Re-emit the complete tool call with full arguments.",
                    step.stop.map(|s| s.as_str()).unwrap_or("unknown")
                );
                // ONE assistant message (D5: no duplicate assistant message)
                self.journal_rec(&JournalRecord::AssistantMessage {
                    text: text.clone(),
                    model: entry.model.clone(),
                    stop_reason: "tool_use".into(),
                })?;
                self.history.write().await.push(ChatMessage {
                    role: "assistant".into(),
                    content: text.clone(),
                    tool_calls: calls.clone(),
                    tool_call_id: None,
                    is_error: false,
                });
                self.journal_tool_calls(&calls)?;
                // error result for each truncated call
                for tc in &calls {
                    if tc.parse_args().is_err() {
                        self.journal_rec(&JournalRecord::ToolResult {
                            id: tc.id.clone(),
                            output: truncated_note.clone(),
                            is_error: true,
                        })?;
                        self.history.write().await.push(ChatMessage::tool_result(
                            tc.id.clone(),
                            truncated_note.clone(),
                            true,
                        ));
                    }
                }
                // valid siblings still execute so they have real results (D5)
                let valid: Vec<ToolCall> = calls
                    .iter()
                    .filter(|tc| tc.parse_args().is_ok())
                    .cloned()
                    .collect();
                if !valid.is_empty() {
                    self.execute_batch(&valid).await;
                }
                continue; // single re-request; the truncated call never ran
            }

            // 5. journal the assistant batch + tool calls, execute through
            //    permissions. ONE assistant message carries all tool_calls.
            self.journal_rec(&JournalRecord::AssistantMessage {
                text: text.clone(),
                model: entry.model.clone(),
                stop_reason: "tool_use".into(),
            })?;
            self.history.write().await.push(ChatMessage {
                role: "assistant".into(),
                content: text.clone(),
                tool_calls: calls.clone(),
                tool_call_id: None,
                is_error: false,
            });
            self.journal_tool_calls(&calls)?;
            let any_error = self.execute_batch(&calls).await;
            // --on-fail abort: a tool error ends the turn immediately
            if self.stop_on_tool_error && any_error {
                return Ok(StopReason::Aborted(
                    "tool error with stop-on-fail enabled".into(),
                ));
            }

            // compaction check (INV-07 + D4): PER-STEP context tokens, not
            // cumulative; explicit stop on failure.
            match self.maybe_compact(last_step_input_tokens).await {
                Ok(compacted) => {
                    if compacted {
                        usage_total = Usage::default(); // reset after compact
                    }
                }
                Err(e) => {
                    self.journal_total_usage(&usage_total, &entry.model)?;
                    return Ok(StopReason::CompactionFailed(format!("{e}")));
                }
            }

            // INV-02 (D3): steering drains ONLY after the full tool-result
            // batch has landed in history.
            let next = self.steer_lock().drain();
            if let Some(s) = next {
                self.history.write().await.push(ChatMessage::user(s));
            }
        }
    }

    fn journal_tool_calls(&self, calls: &[ToolCall]) -> crate::Result<()> {
        for tc in calls {
            let args = tc.parse_args().unwrap_or(Value::Null);
            self.journal_rec(&JournalRecord::ToolCall {
                id: tc.id.clone(),
                tool: tc.name.clone(),
                args,
            })?;
        }
        Ok(())
    }

    /// Execute one tool call with events, cooperative cancel (D8) and an
    /// enforced per-call timeout (D8).
    async fn exec_one(&self, tc: &ToolCall) -> (String, bool) {
        let args = tc.parse_args().unwrap_or(Value::Null);
        // W5.5 per-turn budget: a turn that has spent its tool calls is
        // done — the refusal names the budget and the remedy.
        let calls_this_turn = self.turn_tool_calls.fetch_add(1, Ordering::SeqCst) + 1;
        if calls_this_turn > MAX_TOOL_CALLS_PER_TURN {
            self.executed_tools.fetch_add(1, Ordering::SeqCst);
            return (
                format!(
                    "tool budget for this turn exhausted ({MAX_TOOL_CALLS_PER_TURN} calls) — finish with what you have or end the turn; the next turn gets a fresh budget",
                ),
                true,
            );
        }
        // W5.5 identical-call cap (goose tool_monitor): a call repeated
        // VERBATIM MAX_IDENTICAL_CALLS times (and actually RUN, not
        // permission-denied — the breaker owns denials) is a loop; the
        // next identical call is refused pre-dispatch.
        let fp = call_fingerprint(&tc.name, &args);
        {
            let last = self.last_call.lock().unwrap_or_else(|p| p.into_inner());
            if last.0 == Some(fp) && last.1 >= MAX_IDENTICAL_CALLS {
                self.executed_tools.fetch_add(1, Ordering::SeqCst);
                return (
                    format!(
                        "identical call refused: `{}` ran with byte-identical arguments {} times this turn — change the arguments or take a different approach (loop guard)",
                        tc.name, last.1
                    ),
                    true,
                );
            }
        }
        // P4 permission_tighten actuation: a faculty-tightened tool is
        // denied for this turn (deny>ask>allow — this only ADDS denies;
        // the PermissionEngine remains the sole authority for everything
        // else).
        {
            let ov = self
                .turn_overrides
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if ov.tightened_tools.iter().any(|t| t == &tc.name) {
                self.executed_tools.fetch_add(1, Ordering::SeqCst);
                return (
                    format!(
                        "denied by faculty actuation (permission_tighten): tool `{}` is denied for this turn — the learning faculty observed an error-rate drift on it; run /why for the evidence",
                        tc.name
                    ),
                    true,
                );
            }
        }
        // P4 pre_tool hook: a faculty may deny with a verbatim reason
        let step_now = self.turn_counter.load(Ordering::SeqCst);
        if let Some(reason) = crate::faculty::run_pre_tool(
            &self.bus,
            &self.faculties,
            &self.hook_ctx(step_now),
            &tc.name,
            &args,
        )
        .await
        {
            self.executed_tools.fetch_add(1, Ordering::SeqCst);
            return (format!("denied by faculty `{reason}`"), true);
        }
        self.bus.publish(Event::ToolCallStarted {
            id: tc.id.clone(),
            tool: tc.name.clone(),
            args: args.clone(),
        });
        let timeout_ms = self.tools.tool_timeout_ms(&tc.name).unwrap_or(120_000);
        let tools = self.tools.clone();
        let mut ctx = self.ctx.clone();
        ctx.cancel = self.cancel.clone();
        let name = tc.name.clone();
        let cancel = self.cancel.clone();
        let attempt = async move {
            tokio::select! {
                r = tools.execute(&name, &args, &ctx) => r,
                _ = cancel.cancelled() => ("cancelled by user".to_string(), true),
            }
        };
        let result =
            match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), attempt).await
            {
                Ok(pair) => pair,
                Err(_) => (
                    format!("tool `{}` timed out after {timeout_ms}ms", tc.name),
                    true,
                ),
            };
        // W5.5 post-result streak bookkeeping: denials RESET the streak
        // (the denial circuit breaker owns that path); everything else
        // extends it when the fingerprint repeats.
        {
            let denied = result.1 && result.0.starts_with("permission denied");
            let mut last = self.last_call.lock().unwrap_or_else(|p| p.into_inner());
            if denied {
                *last = (Some(fp), 0);
            } else if last.0 == Some(fp) {
                last.1 += 1;
            } else {
                *last = (Some(fp), 1);
            }
        }
        result
    }

    /// Execute a batch (D8): Execute-class/unsafe tools run sequentially in
    /// call order; concurrency_safe tools run in parallel. Results are
    /// journaled, evented and appended to history in CALL ORDER. Returns
    /// whether ANY call errored (for stop-on-fail).
    async fn execute_batch(&self, calls: &[ToolCall]) -> bool {
        let mut results: Vec<Option<(String, bool)>> = vec![None; calls.len()];
        let mut seq_idx = Vec::new();
        let mut par_idx = Vec::new();
        for (i, tc) in calls.iter().enumerate() {
            if self.tools.concurrency_safe(&tc.name) {
                par_idx.push(i);
            } else {
                seq_idx.push(i);
            }
        }
        for &i in &seq_idx {
            results[i] = Some(self.exec_one(&calls[i]).await);
        }
        if !par_idx.is_empty() {
            let futs: Vec<_> = par_idx.iter().map(|&i| self.exec_one(&calls[i])).collect();
            let done = futures_util::future::join_all(futs).await;
            for (k, &i) in par_idx.iter().enumerate() {
                results[i] = Some(done[k].clone());
            }
        }

        let mut arm = false;
        let mut any_error = false;
        let mut denial_streak_changed = false;
        for (i, tc) in calls.iter().enumerate() {
            let (output, is_error) = results[i].clone().unwrap_or_else(|| {
                (
                    format!("tool `{}` produced no result (internal error)", tc.name),
                    true,
                )
            });
            self.executed_tools.fetch_add(1, Ordering::SeqCst);
            if is_error {
                any_error = true;
            }
            if is_error && output.starts_with("permission denied") {
                self.denied_tools.fetch_add(1, Ordering::SeqCst);
                // §A breaker: consecutive denials count up; any non-denial
                // result breaks the streak.
                let mut c = self
                    .consecutive_denials
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                *c = c.saturating_add(1);
                denial_streak_changed = true;
            } else {
                *self
                    .consecutive_denials
                    .lock()
                    .unwrap_or_else(|p| p.into_inner()) = 0;
            }
            if !is_error && (self.tools.is_mutating(&tc.name) || output.contains("[verify]")) {
                arm = true;
            }
            if let Err(e) = self.journal_rec(&JournalRecord::ToolResult {
                id: tc.id.clone(),
                output: output.clone(),
                is_error,
            }) {
                self.bus.publish(Event::Error {
                    message: format!("journal write failed: {e}"),
                    fatal: true,
                });
            }
            self.bus.publish(Event::ToolCallResult {
                id: tc.id.clone(),
                tool: tc.name.clone(),
                output: output.clone(),
                is_error,
                duration_ms: 0,
            });
            // §A per-tool outcome stream into the faculties (tighten
            // actuation evidence; bounded, observational).
            crate::faculty::run_observe_tool(
                &self.bus,
                &self.faculties,
                &self.hook_ctx(self.turn_counter.load(Ordering::SeqCst)),
                &tc.name,
                is_error,
            )
            .await;
            self.history.write().await.push(ChatMessage::tool_result(
                tc.id.clone(),
                output,
                is_error,
            ));
        }
        if arm {
            self.verify_armed.store(true, Ordering::SeqCst);
        }
        // §A denial circuit breaker: LIMIT consecutive denials → an
        // explicit stop-addendum lands as the next user-role message
        // (bounded: one addendum per trip; journaled + evented).
        if denial_streak_changed {
            let streak = *self
                .consecutive_denials
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if streak == DENIAL_BREAKER_LIMIT {
                let addendum = format!(
                    "[circuit breaker] {DENIAL_BREAKER_LIMIT} consecutive permission denials — the same call keeps failing closed. STOP retrying it: change the approach or ask the user. Identical retries will keep being denied."
                );
                let _ = self.journal_rec(&JournalRecord::Steer {
                    text: addendum.clone(),
                });
                self.history.write().await.push(ChatMessage::user(addendum));
            }
        }
        any_error
    }

    /// /compact and other callers: force a compaction NOW regardless of
    /// threshold. Re-injects the summary as the first message.
    pub async fn compact_now(&self) -> crate::Result<String> {
        let (summary, _usage, _model) = self.compact_via_small().await?;
        *self.history.write().await =
            vec![ChatMessage::user(format!("[compacted context]\n{summary}"))];
        Ok(summary)
    }

    /// D4: compaction on PER-STEP context tokens. Runs the Small-role
    /// summary, re-injects it as the first message, resets history. Returns
    /// whether a compaction happened; failure is an explicit error.
    async fn maybe_compact(
        &self,
        last_step_input_tokens: u64,
    ) -> std::result::Result<bool, crate::CoreError> {
        // P4 compact_threshold actuation (clamped at turn start to
        // [0.5x, 1.5x] of the configured value). Distillation §C1: when
        // the catalog knows the primary model's context window, the
        // Hermes trigger math raises the configured threshold to an
        // honest floor — `(ctx − max_out) × pct` with a 64k floor capped
        // at 85% and a 75% floor on small (<512k) windows. The actuation
        // override still wins absolutely; the math only ever RAISES the
        // plain configured value.
        let configured = self
            .turn_overrides
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .compact_threshold
            .unwrap_or(self.compact_threshold_tokens);
        let threshold = match self.context_window {
            Some(w) => configured.max(crate::compact::trigger_tokens(
                w,
                self.sampling.max_completion_tokens.unwrap_or(8192),
                crate::compact::DEFAULT_TRIGGER_PCT,
            )),
            None => configured,
        };
        if last_step_input_tokens <= threshold {
            // W8 async compaction (§C): between the SOFT threshold (70%
            // of the trigger) and the hard one, START the Small-role
            // summarization in the BACKGROUND — the next over-trigger
            // step usually consumes a ready summary instead of paying
            // the latency inline. Bounded: at most ONE pending task;
            // failure is swallowed at consume time (the sync path owns
            // the retry) and the task never mutates anything itself.
            let soft = (threshold as f64 * COMPACTION_SOFT_RATIO) as u64;
            if last_step_input_tokens > soft {
                // Lock-free around the await: check → snapshot → re-check
                // → store (a std Mutex guard must never cross an await).
                let free = self
                    .pending_compaction
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .is_none();
                let mut started = false;
                if free {
                    let snapshot = self.history.read().await.clone();
                    self.compaction_snapshot_len
                        .store(snapshot.len() as u64, std::sync::atomic::Ordering::SeqCst);
                    let mut slot = self
                        .pending_compaction
                        .lock()
                        .unwrap_or_else(|p| p.into_inner());
                    if slot.is_none() {
                        let fut = summarize_via_small(self.router.clone(), snapshot);
                        *slot = Some(tokio::spawn(fut));
                        started = true;
                    }
                }
                if started {
                    self.bus
                        .publish(Event::CompactionStarted { background: true });
                }
            }
            return Ok(false);
        }
        // W8 (§C, ordered strategies): the stages run in the CONFIGURED
        // order (default promote → micro → summarize — byte-identical to
        // the pre-W8 ladder).
        for stage in self.compaction_strategies.iter().copied() {
            match stage {
                crate::compact::Strategy::Promote => {
                    // W8 context promotion (§C, OMP trigger order): near
                    // the honest ceiling, a chain sibling with a LARGER
                    // known context window takes the next step INSTEAD of
                    // compacting — promotion is lossless, summarization is
                    // not. The promotion band is the trigger math's cap
                    // ratio; when no larger sibling exists (or the catalog
                    // knows no windows) the next stage proceeds.
                    if let Some(w) = self.context_window {
                        let ceiling = ((w
                            .saturating_sub(self.sampling.max_completion_tokens.unwrap_or(8192))
                            as f64)
                            * crate::compact::FLOOR_CAP_RATIO)
                            as u64;
                        if last_step_input_tokens > ceiling {
                            let router = self.router.read().await;
                            let promote = router.promote_overflow(
                                Role::Primary,
                                last_step_input_tokens,
                                self.context_window,
                            );
                            drop(router);
                            if let Some(bigger) = promote {
                                self.bus.publish(Event::Error {
                                    message: format!(
                                        "context overflow: promoting to `{bigger}` instead of compacting"
                                    ),
                                    fatal: false,
                                });
                                self.turn_overrides
                                    .lock()
                                    .unwrap_or_else(|p| p.into_inner())
                                    .model = Some(bigger);
                                return Ok(false);
                            }
                        }
                    }
                }
                crate::compact::Strategy::Micro => {
                    // Stage: micro-compaction — elide stale tool results
                    // (cheap, pure, no provider call). Protected tail
                    // keeps the live exchange intact — §C2: with a known
                    // window the tail is sized in TOKENS (2.5% clamped
                    // [10k, 25k]) on top of the message-count floor; errors
                    // are never elided.
                    //
                    // W8 defrag ledger (§C, Hermes micro_compaction): a
                    // sweep that reclaims less than DEFRAG_MIN_TOKENS is a
                    // failure; at 3 consecutive failures the micro stage is
                    // skipped entirely until a full summarization resets
                    // the counter.
                    let fails = self.defrag_fails.load(std::sync::atomic::Ordering::SeqCst);
                    if fails < crate::compact::DEFRAG_MAX_FAILS {
                        let mut history = self.history.write().await;
                        let removed = crate::compact::micro_compact_budget(
                            &mut history,
                            threshold,
                            6,
                            Some(crate::compact::tail_budget_tokens(self.context_window)),
                        );
                        let removed_tokens = removed.div_ceil(4);
                        if removed_tokens >= crate::compact::DEFRAG_MIN_TOKENS {
                            self.defrag_fails
                                .store(0, std::sync::atomic::Ordering::SeqCst);
                            let after =
                                (history.iter().map(|m| m.content.len() as u64).sum::<u64>())
                                    .div_ceil(4);
                            self.bus.publish(Event::Compaction {
                                before_tokens: last_step_input_tokens,
                                after_tokens: after,
                            });
                            self.journal_rec(&JournalRecord::Compaction {
                                summary: format!(
                                    "micro-compaction: elided {removed} bytes of stale tool results"
                                ),
                            })?;
                            if after <= threshold {
                                return Ok(true); // micro stage was enough
                            }
                        } else {
                            // insufficient sweep (or nothing eligible):
                            // count it
                            let n = self
                                .defrag_fails
                                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                                + 1;
                            if n >= crate::compact::DEFRAG_MAX_FAILS {
                                self.bus.publish(Event::Error {
                                    message: format!(
                                        "micro-compaction skipped: {n} sweeps reclaimed \
                                         <{} tokens each — summarizing instead",
                                        crate::compact::DEFRAG_MIN_TOKENS
                                    ),
                                    fatal: false,
                                });
                            }
                        }
                    }
                }
                crate::compact::Strategy::Summarize => {
                    // Stage: full summarization via the Small role. W8
                    // async form: consume the background summary when one
                    // is pending (started at the soft threshold); a failed
                    // or absent background run falls back to the
                    // synchronous path — never a silent continuation.
                    self.bus
                        .publish(Event::CompactionStarted { background: false });
                    let (summary, small_usage, small_model) = {
                        let pending = self
                            .pending_compaction
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .take();
                        match pending {
                            Some(handle) => match handle.await {
                                Ok(Ok(out)) => out,
                                _ => {
                                    self.bus.publish(Event::Error {
                                        message:
                                            "background compaction failed; compacting synchronously"
                                                .into(),
                                        fatal: false,
                                    });
                                    self.compact_via_small().await?
                                }
                            },
                            None => self.compact_via_small().await?,
                        }
                    };
                    self.bus.publish(Event::Compaction {
                        before_tokens: last_step_input_tokens,
                        after_tokens: (summary.len() as u64).div_ceil(4),
                    });
                    self.journal_rec(&JournalRecord::Compaction {
                        summary: summary.clone(),
                    })?;
                    // D4: Small-role usage is ledgered (event + journal)
                    let cost = self
                        .router
                        .read()
                        .await
                        .costs
                        .cost_usd(&small_model, &small_usage);
                    self.bus.publish(Event::Usage {
                        role: Role::Small.as_str().into(),
                        model: small_model.clone(),
                        input_tokens: small_usage.input_tokens,
                        output_tokens: small_usage.output_tokens,
                        cache_read_tokens: small_usage.cache_read_tokens,
                        cache_creation_tokens: small_usage.cache_creation_tokens,
                        cost_usd: cost,
                    });
                    self.journal_rec(&JournalRecord::Usage {
                        role: Role::Small.as_str().into(),
                        model: small_model,
                        input_tokens: small_usage.input_tokens,
                        output_tokens: small_usage.output_tokens,
                        cost_usd: cost,
                    })?;
                    // D4: summary re-injected as the FIRST message. W8
                    // async form: turns that ran WHILE the background
                    // summary was in flight survive as a protected tail —
                    // snapped forward past any orphaned tool result so the
                    // wire never sees a tool_result without its call.
                    let snapshot_len = self
                        .compaction_snapshot_len
                        .swap(0, std::sync::atomic::Ordering::SeqCst);
                    let snap = snapshot_len as usize;
                    let mut tail: Vec<ChatMessage> = Vec::new();
                    {
                        let history = self.history.read().await;
                        if snap > 0 && history.len() > snap {
                            tail = history[snap..].to_vec();
                            // snap past a leading orphaned tool result
                            let cut = tail
                                .iter()
                                .position(|m| m.tool_call_id.is_none())
                                .unwrap_or(tail.len());
                            tail = tail.split_off(cut.min(tail.len()));
                        }
                    }
                    let mut next =
                        vec![ChatMessage::user(format!("[compacted context]\n{summary}"))];
                    next.extend(tail);
                    *self.history.write().await = next;
                    // W8 defrag ledger: a full summarization does NOT
                    // reset the failure count — in a two-stage loop every
                    // micro failure is immediately followed by
                    // summarization, so the only honest reset is a sweep
                    // that actually reclaims ≥ DEFRAG_MIN_TOKENS. Until
                    // then the micro stage stays skipped (the summarizer
                    // owns reduction; sweeps on an already-compressed
                    // state are waste).
                    return Ok(true);
                }
            }
        }
        // Every configured stage ran without reducing below the trigger
        // (e.g. a promote-only config): honest report, no compaction.
        Ok(false)
    }

    /// D4: compaction via the Small role, usage returned for ledgering.
    /// §C: the summary prompt carries a VERBATIM user-message section
    /// (24k chars total / 4k per message, newest-first — Hermes lean
    /// summary spec) and a recovery footer steering to the session
    /// search tool, so post-compaction work can still reach pre-compaction
    /// facts through the journal.
    async fn compact_via_small(&self) -> crate::Result<(String, Usage, String)> {
        summarize_via_small(self.router.clone(), self.history.read().await.clone()).await
    }

    fn journal_total_usage(&self, usage: &Usage, model: &str) -> crate::Result<()> {
        self.journal_rec(&JournalRecord::Usage {
            role: Role::Primary.as_str().into(),
            model: model.into(),
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cost_usd: None,
        })
    }
}

pub fn system_prompt(ctx: &LoopCtx) -> String {
    system_prompt_with_nudge(ctx, None)
}

/// W10.1: the full system surface — base prompt + optional nudge + the
/// session-start sections (memory cap / instincts / skills). Every
/// section is appended as its own block ONLY when present; an all-None
/// surfaces (and no nudge) keeps the base prompt byte-identical.
pub fn system_prompt_with_surfaces(
    ctx: &LoopCtx,
    nudge: Option<&str>,
    surfaces: &PromptSurfaces,
) -> String {
    let mut out = system_prompt_with_nudge(ctx, nudge);
    if let Some(m) = &surfaces.memory {
        out.push_str("\n\nProject memory (persistent; the Memory tool manages it):\n");
        out.push_str(m);
    }
    if let Some(i) = &surfaces.instincts {
        out.push('\n');
        out.push('\n');
        out.push_str(i);
    }
    if let Some(s) = &surfaces.skills {
        out.push('\n');
        out.push('\n');
        out.push_str(s);
    }
    out
}

/// W8 async compaction: the Small-role summarization as a FREE function
/// over owned state (the router Arc + a history snapshot), so it can run
/// in a background task started at the soft threshold. The prompt carries
/// a VERBATIM user-message section (24k chars total / 4k per message,
/// newest-first — Hermes lean summary spec) and a recovery footer
/// steering to the session search tool. Pure with respect to the harness:
/// the task never mutates harness state — the caller consumes the result.
pub(crate) async fn summarize_via_small(
    router: Arc<RwLock<Router>>,
    history: Vec<ChatMessage>,
) -> crate::Result<(String, Usage, String)> {
    let (entry, _tried) = router.read().await.resolve(Role::Small, |_| true)?;
    let body = history
        .iter()
        .map(|m| {
            let tc_info = if m.tool_calls.is_empty() {
                String::new()
            } else {
                format!(
                    " [calls: {}]",
                    m.tool_calls
                        .iter()
                        .map(|t| t.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            format!("{}: {}{tc_info}", m.role, m.content)
        })
        .collect::<Vec<_>>()
        .join("\n");
    // Verbatim user messages, newest-first, hard-capped: 4000 chars
    // per message, 24000 chars total. Deterministic selection.
    const VERBATIM_TOTAL_CAP: usize = 24_000;
    const VERBATIM_PER_MSG_CAP: usize = 4_000;
    let mut verbatim = String::new();
    for m in history.iter().rev().filter(|m| m.role == "user") {
        if verbatim.len() >= VERBATIM_TOTAL_CAP {
            break;
        }
        let mut t: String = m.content.chars().take(VERBATIM_PER_MSG_CAP).collect();
        if m.content.chars().count() > VERBATIM_PER_MSG_CAP {
            t.push_str(" …[truncated]");
        }
        let room = VERBATIM_TOTAL_CAP - verbatim.len();
        let t: String = t.chars().take(room).collect();
        verbatim.push_str("\n---\n");
        verbatim.push_str(&t);
    }
    if verbatim.is_empty() {
        verbatim.push_str("\n---\n(no user messages)");
    }
    let req = ChatRequest {
        messages: vec![ChatMessage::user(format!(
            "Summarize the following conversation for context continuation. \
             Keep file paths, decisions, and pending work.\
             \n\n=== VERBATIM RECENT USER REQUESTS (newest first; preserve these goals EXACTLY) ==={verbatim}\
             \n\n=== CONVERSATION ===\
             \n{body}\
             \n\nNOTE: older tool outputs may have been elided or summarized away. \
             For exact details from earlier in this session, run the SessionSearch tool with keywords from the original task."
        ))],
        ..Default::default()
    };
    let step = entry.provider.chat_step(&req).await?;
    let s = step.text();
    if s.trim().is_empty() {
        return Err(crate::CoreError::Provider(
            "compaction produced empty summary".into(),
        ));
    }
    Ok((s, step.usage, entry.model.clone()))
}

/// System prompt with an optional cadence note (§S memory nudge). The
/// note is appended as its own line; `None` keeps the base prompt byte-
/// identical (reversible, observable).
pub fn system_prompt_with_nudge(ctx: &LoopCtx, nudge: Option<&str>) -> String {
    let base = format!(
        "You are Marfim, a coding agent (Rust v{}). Working directory: {}. \
Use the provided tools to read, edit and run code. Be precise and verify your work.",
        crate::VERSION,
        ctx.cwd
    );
    match nudge {
        Some(n) if !n.is_empty() => format!("{base}\n{n}"),
        _ => base,
    }
}

#[cfg(test)]
mod tests {
    //! Loop invariant tests (INV-01..07, D1..D9) run against the mock provider.

    use super::*;
    use crate::permissions::{Decision, Mode, PermissionEngine, Rule, ToolClass};
    use crate::providers::{Block, MockAction, MockProvider, Step, Stop};
    use std::sync::Mutex;

    struct ScriptedTools {
        pub executed: Mutex<Vec<(String, Value)>>,
        pub verify_fails: Mutex<bool>,
        pub safe: Mutex<bool>,
        pub slow_ms: Mutex<Option<u64>>,
        pub started: Mutex<Vec<(String, std::time::Instant)>>,
        /// Tools scripted to return an error result (D12 stop-on-fail path).
        pub fail_tools: Mutex<Vec<String>>,
        /// Tools scripted to be DENIED by the permission surface (the
        /// tools_host contract: the error text starts with "permission denied").
        pub deny_tools: Mutex<Vec<String>>,
        /// Concurrency probe: how many execute() calls are inside the scripted
        /// sleep right now, and the high-water mark ever reached. Wall-clock
        /// elapsed time is scheduler-dependent (a loaded CI runner stretches a
        /// 150ms sleep past any fixed budget) — overlap itself is not.
        pub active: std::sync::atomic::AtomicUsize,
        pub max_active: std::sync::atomic::AtomicUsize,
    }

    impl ScriptedTools {
        fn new() -> Self {
            Self {
                executed: Mutex::new(vec![]),
                verify_fails: Mutex::new(false),
                safe: Mutex::new(false),
                slow_ms: Mutex::new(None),
                started: Mutex::new(vec![]),
                fail_tools: Mutex::new(vec![]),
                deny_tools: Mutex::new(vec![]),
                active: std::sync::atomic::AtomicUsize::new(0),
                max_active: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl ToolHost for ScriptedTools {
        async fn execute(&self, tool: &str, args: &Value, _s: &LoopCtx) -> (String, bool) {
            self.started
                .lock()
                .unwrap()
                .push((tool.to_string(), std::time::Instant::now()));
            let slow = *self.slow_ms.lock().unwrap();
            if let Some(ms) = slow {
                // overlap probe: enter the sleep "inside" the call
                let now = self
                    .active
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                    + 1;
                self.max_active
                    .fetch_max(now, std::sync::atomic::Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                self.active
                    .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            }
            if self.fail_tools.lock().unwrap().iter().any(|t| t == tool) {
                return (format!("{tool} failed (scripted error)"), true);
            }
            if self.deny_tools.lock().unwrap().iter().any(|t| t == tool) {
                return ("permission denied: explicit deny rule".to_string(), true);
            }
            self.executed
                .lock()
                .unwrap()
                .push((tool.to_string(), args.clone()));
            (format!("{tool} ok"), false)
        }
        fn schemas(&self) -> Vec<Value> {
            vec![]
        }
        fn is_mutating(&self, tool: &str) -> bool {
            tool == "Write" || tool == "Bash"
        }
        fn verify_commands(&self) -> Vec<String> {
            vec!["true".into()]
        }
        async fn run_verify(&self, _s: &LoopCtx) -> (bool, String) {
            let fails = *self.verify_fails.lock().unwrap();
            if fails {
                (false, "FAIL tests".into())
            } else {
                (true, String::new())
            }
        }
        fn concurrency_safe(&self, _tool: &str) -> bool {
            *self.safe.lock().unwrap()
        }
        fn tool_timeout_ms(&self, _tool: &str) -> Option<u64> {
            Some(10_000)
        }
    }

    fn harness(script: Vec<Step>, tools: Arc<dyn ToolHost>) -> (Harness, EventBus) {
        harness_actions(script.into_iter().map(MockAction::Step).collect(), tools)
    }

    fn harness_actions(actions: Vec<MockAction>, tools: Arc<dyn ToolHost>) -> (Harness, EventBus) {
        let bus = EventBus::new(4096);
        let mut router = Router::new(crate::providers::CostBook::default(), false);
        router.set_chain(
            Role::Primary,
            vec![crate::router::ModelEntry {
                model: "mock-primary".into(),
                provider: Arc::new(MockProvider::from_actions(actions)),
                priced: true,
            }],
        );
        router.set_chain(
            Role::Small,
            vec![crate::router::ModelEntry {
                model: "mock-small".into(),
                provider: Arc::new(MockProvider::new(vec![MockProvider::text(
                    "summary of the conversation so far",
                )])),
                priced: true,
            }],
        );
        let (perm, _rx) = PermissionEngine::new(Mode::Yolo, vec![]);
        let ctx = LoopCtx {
            session_id: "t".into(),
            cwd: "/tmp".into(),
            cancel: CancelToken::default(),
        };
        let tmpdir = marfim_testkit::unique_dir("marfim-loop");
        let _ = std::fs::create_dir_all(&tmpdir);
        let journal = Journal::open(&tmpdir.join(format!("loop-{}.jsonl", rand_suffix()))).unwrap();
        let h = Harness {
            bus: bus.clone(),
            router: Arc::new(RwLock::new(router)),
            tools,
            journal: Arc::new(Mutex::new(journal)),
            ctx,
            permissions: Arc::new(perm),
            max_steps: 20,
            max_verify_feedback: 2,
            history: Arc::new(RwLock::new(Vec::new())),
            cancel: CancelToken::default(),
            compact_threshold_tokens: u64::MAX,
            sampling: Sampling::default(),
            context_window: None,
            turn_counter: AtomicU64::new(0),
            last_call: std::sync::Mutex::new((None, 0)),
            turn_tool_calls: AtomicU64::new(0),
            defrag_fails: std::sync::atomic::AtomicU32::new(0),
            cache: crate::providers::cache::CacheSettings::default(),
            allow_custom_tools: false,
            compaction_strategies: crate::compact::DEFAULT_STRATEGIES.to_vec(),
            pending_compaction: std::sync::Mutex::new(None),
            compaction_snapshot_len: std::sync::atomic::AtomicU64::new(0),
            cache_warmed: AtomicBool::new(false),
            denied_tools: AtomicU64::new(0),
            faculties: Vec::new(),
            turn_overrides: std::sync::Mutex::new(TurnOverrides::default()),
            consecutive_denials: std::sync::Mutex::new(0),
            last_turn_aborted: AtomicBool::new(false),
            verify_armed: AtomicBool::new(false),
            stop_on_tool_error: false,
            executed_tools: AtomicU64::new(0),
            steer: Arc::new(Mutex::new(SteerQueues::default())),
            surfaces: PromptSurfaces::default(),
        };
        (h, bus)
    }

    fn rand_suffix() -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos() as u64
    }

    fn step_with_usage(text: &str, input_tokens: u64) -> Step {
        Step {
            blocks: vec![Block::Text(text.into())],
            stop: Some(Stop::EndTurn),
            usage: Usage {
                input_tokens,
                ..Default::default()
            },
        }
    }

    #[tokio::test]
    async fn inv01_verify_runs_only_without_toolcall_and_after_mutation() {
        let tools = Arc::new(ScriptedTools::new());
        let script = vec![
            MockProvider::tool_call("c1", "Write", serde_json::json!({"path": "a"})),
            MockProvider::text("all done"),
        ];
        let (h, _bus) = harness(script, tools.clone());
        let stop = h.run_turn("please write").await;
        assert_eq!(stop, StopReason::EndTurn);
        let exec = tools.executed.lock().unwrap();
        assert!(exec.iter().any(|(t, _)| t == "Write"));
    }

    #[tokio::test]
    async fn inv01_no_verify_when_toolcall_present() {
        let tools = Arc::new(ScriptedTools::new());
        let script = vec![
            MockProvider::tool_call("c1", "Write", serde_json::json!({"path": "a"})),
            MockProvider::tool_call("c2", "Read", serde_json::json!({"path": "a"})),
            MockProvider::text("done"),
        ];
        let (h, _bus) = harness(script, tools);
        let _ = h.run_turn("x").await;
    }

    #[tokio::test]
    async fn w73_shape_error_rung_strips_the_named_parameter_and_retries() {
        // W7.3 (§F): a 400-SHAPE error gets ONE same-model retry with the
        // offending parameter stripped — before the failover walk.
        let tools = Arc::new(ScriptedTools::new());
        let actions = vec![
            MockProvider::error("400 temperature is unsupported for this model"),
            MockAction::Step(MockProvider::text("recovered")),
        ];
        let (mut h, bus) = harness_actions(actions, tools.clone());
        h.sampling.temperature = Some(0.7);
        let mut sub = bus.subscribe();
        let stop = h.run_turn("go").await;
        assert_eq!(stop, StopReason::EndTurn);

        // the rung event was published, naming the stripped retry
        let mut saw_rung = false;
        while let Ok(ev) = sub.try_recv() {
            if let Event::Error { message, .. } = ev {
                if message.contains("retrying with it stripped") {
                    saw_rung = true;
                }
            }
        }
        assert!(saw_rung, "rung event missing on the bus");
    }

    #[tokio::test]
    async fn w73_ambiguous_errors_never_trigger_a_rung() {
        // A plain transport failure must NOT get a stripped retry — the
        // failover walk owns it (one scripted error, no second step).
        let tools = Arc::new(ScriptedTools::new());
        let actions = vec![MockProvider::error("503 upstream unavailable")];
        let (mut h, _bus) = harness_actions(actions, tools.clone());
        h.sampling.temperature = Some(0.7);
        let stop = h.run_turn("go").await;
        // single-model chain: the error is terminal (fail-visible)
        assert!(
            matches!(stop, StopReason::Aborted(_)),
            "unexpected stop {stop:?}"
        );
    }

    #[tokio::test]
    async fn w8_defrag_ledger_skips_micro_after_three_insufficient_sweeps() {
        // W8 (§C Hermes micro_compaction): a sweep reclaiming <2000
        // tokens is a failure; at 3 consecutive failures the micro stage
        // is skipped until a full summarization resets the ledger.
        let tools = Arc::new(ScriptedTools::new());
        let (mut h, bus) = harness(vec![], tools.clone());
        h.compact_threshold_tokens = 1;
        {
            let mut hist = h.history.write().await;
            // 400 bytes ≈ 100 tokens reclaimed — below the 2000 floor
            hist.push(ChatMessage::tool_result("t1", "x".repeat(400), false));
            hist.push(ChatMessage::user("hi"));
        }
        let mut sub = bus.subscribe();
        let _ = h.maybe_compact(10).await.unwrap();
        let _ = h.maybe_compact(10).await.unwrap();
        let _ = h.maybe_compact(10).await.unwrap();
        let mut saw_skip = false;
        while let Ok(ev) = sub.try_recv() {
            if let Event::Error { message, .. } = ev {
                if message.contains("micro-compaction skipped") {
                    saw_skip = true;
                }
            }
        }
        assert!(saw_skip, "3-failure skip event missing on the bus");
        assert_eq!(
            h.defrag_fails.load(std::sync::atomic::Ordering::SeqCst),
            3,
            "three consecutive insufficient sweeps"
        );

        // the next trigger skips the micro stage entirely (no sweep) and
        // goes straight to stage 2 — the summarizer owns reduction now
        let _ = h.maybe_compact(10).await.unwrap();
        assert_eq!(
            h.defrag_fails.load(std::sync::atomic::Ordering::SeqCst),
            3,
            "skip state persists until a sweep succeeds"
        );
        let hist = h.history.read().await;
        assert!(
            hist[0].content.contains("[compacted context]"),
            "stage 2 summarized the history: {:?}",
            hist[0].content
        );
    }

    #[tokio::test]
    async fn inv03_truncated_toolcall_is_never_executed() {
        let tools = Arc::new(ScriptedTools::new());
        let bad = Step {
            blocks: vec![Block::ToolCall(ToolCall {
                id: "bad1".into(),
                name: "Bash".into(),
                arguments_json: r#"{"command": "rm -rf /tmp/unc"#.into(),
                thought_signature: None,
            })],
            stop: Some(Stop::MaxTokens),
            usage: Usage::default(),
        };
        let script = vec![
            bad,
            MockProvider::tool_call("ok1", "Read", serde_json::json!({"path": "x"})),
            MockProvider::text("recovered"),
        ];
        let (h, _bus) = harness(script, tools.clone());
        let _ = h.run_turn("go").await;
        let exec = tools.executed.lock().unwrap();
        assert!(
            !exec.iter().any(|(t, _)| t == "Bash"),
            "truncated call must NEVER execute"
        );
        assert!(
            exec.iter().any(|(t, _)| t == "Read"),
            "re-request succeeded"
        );
    }

    #[tokio::test]
    async fn inv05_usage_journaled_with_real_model_id() {
        let tools = Arc::new(ScriptedTools::new());
        let (h, _bus) = harness(vec![MockProvider::text("done")], tools);
        let _ = h.run_turn("hi").await;
        // The harness owns its journal path — read it directly instead of
        // scanning a (formerly shared) temp directory for `loop-*` files.
        let journal_path = h.journal.lock().unwrap().path().to_path_buf();
        let recs = crate::journal::Journal::replay(&journal_path).unwrap();
        let found = recs
            .iter()
            .any(|r| matches!(r, JournalRecord::Usage { model, .. } if model == "mock-primary"));
        assert!(found, "usage with real model id must be journaled");
    }

    #[tokio::test]
    async fn inv07_compaction_failure_stops_turn_explicitly() {
        let tools = Arc::new(ScriptedTools::new());
        let bus = EventBus::new(256);
        let mut router = Router::new(crate::providers::CostBook::default(), false);
        router.set_chain(
            Role::Primary,
            vec![crate::router::ModelEntry {
                model: "mock-primary".into(),
                provider: Arc::new(MockProvider::new(vec![MockProvider::tool_call(
                    "c",
                    "Read",
                    serde_json::json!({}),
                )])),
                priced: true,
            }],
        );
        router.set_chain(
            Role::Small,
            vec![crate::router::ModelEntry {
                model: "mock-small".into(),
                provider: Arc::new(MockProvider::new(vec![MockProvider::text("")])),
                priced: true,
            }],
        );
        let (perm, _rx) = PermissionEngine::new(Mode::Yolo, vec![]);
        let tmpdir = std::env::temp_dir().join("marfim-compact-fail");
        let _ = std::fs::create_dir_all(&tmpdir);
        let h = Harness {
            bus: bus.clone(),
            router: Arc::new(RwLock::new(router)),
            tools,
            journal: Arc::new(Mutex::new(Journal::open(&tmpdir.join("cf.jsonl")).unwrap())),
            ctx: LoopCtx::default(),
            permissions: Arc::new(perm),
            max_steps: 5,
            max_verify_feedback: 1,
            history: Arc::new(RwLock::new(Vec::new())),
            cancel: CancelToken::default(),
            compact_threshold_tokens: u64::MAX,
            sampling: Sampling::default(),
            context_window: None,
            turn_counter: AtomicU64::new(0),
            last_call: std::sync::Mutex::new((None, 0)),
            turn_tool_calls: AtomicU64::new(0),
            defrag_fails: std::sync::atomic::AtomicU32::new(0),
            cache: crate::providers::cache::CacheSettings::default(),
            allow_custom_tools: false,
            compaction_strategies: crate::compact::DEFAULT_STRATEGIES.to_vec(),
            pending_compaction: std::sync::Mutex::new(None),
            compaction_snapshot_len: std::sync::atomic::AtomicU64::new(0),
            cache_warmed: AtomicBool::new(false),
            denied_tools: AtomicU64::new(0),
            faculties: Vec::new(),
            turn_overrides: std::sync::Mutex::new(TurnOverrides::default()),
            consecutive_denials: std::sync::Mutex::new(0),
            last_turn_aborted: AtomicBool::new(false),
            verify_armed: AtomicBool::new(false),
            stop_on_tool_error: false,
            executed_tools: AtomicU64::new(0),
            steer: Arc::new(Mutex::new(SteerQueues::default())),
            surfaces: PromptSurfaces::default(),
        };
        let res = h.compact_via_small().await;
        assert!(res.is_err(), "empty summary must fail compaction");
    }

    #[test]
    fn steering_queues_classify() {
        let mut q = SteerQueues::default();
        q.submit_follow_up("also fix the lint");
        q.submit_amend("no wait, use rust");
        assert_eq!(q.follow_ups.len(), 0, "amend supersedes the follow-up");
        assert_eq!(q.amend.len(), 1);
        assert_eq!(
            q.drain(),
            Some("no wait, use rust".to_string()),
            "amend drains first"
        );
        assert_eq!(q.drain(), None);
        assert_eq!(classify_steer("amend: use rust"), SteerClass::Amend);
        assert_eq!(classify_steer("amend the model"), SteerClass::Amend);
        assert_eq!(classify_steer("also add tests"), SteerClass::FollowUp);
    }

    #[test]
    fn permissions_shared_between_loop_and_tools() {
        let (perm, _rx) = PermissionEngine::new(
            Mode::Default,
            vec![Rule {
                tool: "Bash".into(),
                decision: Decision::Deny,
                argv_prefix: None,
            }],
        );
        let _shared: Arc<PermissionEngine> = Arc::new(perm);
        assert_eq!(ToolClass::ReadOnly, ToolClass::ReadOnly);
    }

    // ------------------------------------------------------ D1: history
    #[tokio::test]
    async fn d1_user_message_in_history_and_steer_after_transcript() {
        // D1: user msg pushed at turn start; steering appended AFTER the
        // transcript (never before it).
        let tools = Arc::new(ScriptedTools::new());
        let script = vec![
            MockProvider::tool_call("c1", "Read", serde_json::json!({"path": "a"})),
            MockProvider::text("done"),
        ];
        let (h, _bus) = harness(script, tools);
        h.steer("also fix lint");
        let _ = h.run_turn("please read a").await;
        let hist = h.history.read().await;
        let roles: Vec<&str> = hist.iter().map(|m| m.role.as_str()).collect();
        // user, assistant(toolcall), tool, steer-user, assistant(final)
        assert_eq!(roles[0], "user", "user msg is first in history (D1)");
        assert_eq!(roles[1], "assistant");
        assert_eq!(roles[2], "tool");
        assert_eq!(
            roles[3], "user",
            "steering appended AFTER the tool-result batch (D1/D3)"
        );
        assert!(hist[3].content.contains("also fix lint"));
        assert_eq!(roles[4], "assistant", "turn continues after steering");
    }

    #[tokio::test]
    async fn d2_history_persists_across_turns() {
        let tools = Arc::new(ScriptedTools::new());
        let script = vec![MockProvider::text("first"), MockProvider::text("second")];
        let (h, _bus) = harness(script, tools);
        let _ = h.run_turn("turn one").await;
        let _ = h.run_turn("turn two").await;
        // the session history must contain both turns (D2)
        let hist = h.history.read().await;
        let texts: Vec<&str> = hist.iter().map(|m| m.content.as_str()).collect();
        assert!(texts.contains(&"turn one"));
        assert!(texts.contains(&"first"));
        assert!(texts.contains(&"turn two"));
        assert!(texts.contains(&"second"));
    }

    // ------------------------------------------------------ D3: steering
    #[tokio::test]
    async fn d3_steering_drains_only_after_full_tool_batch() {
        // With a tool batch, steering must appear AFTER the tool result in
        // history; the next request sees [.., tool_result, steer].
        let tools = Arc::new(ScriptedTools::new());
        let script = vec![
            MockProvider::tool_call("c1", "Read", serde_json::json!({"path": "a"})),
            MockProvider::text("done"),
        ];
        let (h, _bus) = harness(script, tools);
        // pre-queue steering: it must NOT be consumed before the batch
        h.steer("follow-up instruction");
        let _ = h.run_turn("start").await;
        let hist = h.history.read().await;
        let tool_result_pos = hist
            .iter()
            .position(|m| m.role == "tool")
            .expect("tool result in history");
        let steer_pos = hist
            .iter()
            .rposition(|m| m.content.contains("follow-up instruction"))
            .expect("steer drained");
        assert!(
            steer_pos > tool_result_pos,
            "steering drained only after the full tool-result batch"
        );
        assert!(h.pending_steers() == 0);
    }

    // ------------------------------------------------------ D4: compaction
    #[tokio::test]
    async fn d4_compaction_per_step_reset_summary_first_small_ledgered() {
        let tools = Arc::new(ScriptedTools::new());
        // step1: big input triggers compaction; step2: final text
        let script = vec![
            step_with_usage("working", 150_000),
            MockProvider::text("after compact"),
        ];
        let (mut h, bus) = harness(script, tools);
        h.compact_threshold_tokens = 100_000;
        let mut sub = bus.subscribe();
        let _ = h.run_turn("long task").await;

        let mut saw_small_usage = false;
        let mut saw_compaction = false;
        while let Ok(ev) = sub.try_recv() {
            if let Event::Usage { role, model, .. } = &ev {
                if role == "small" && model == "mock-small" {
                    saw_small_usage = true;
                }
            }
            if matches!(&ev, Event::Compaction { .. }) {
                saw_compaction = true;
            }
        }
        assert!(saw_compaction, "compaction event published");
        assert!(saw_small_usage, "Small-role usage ledgered via event (D4)");

        // summary re-injected as FIRST message of the next request
        let hist = h.history.read().await;
        assert_eq!(hist.len(), 1, "history reset to the summary alone");
        assert!(
            hist[0]
                .content
                .contains("summary of the conversation so far"),
            "summary is the first message (D4), got: {}",
            hist[0].content
        );
        assert_eq!(hist[0].role, "user");
    }

    // ------------------------------------------------------ D5: truncation
    #[tokio::test]
    async fn d5_every_tool_use_gets_result_no_duplicate_assistant() {
        let tools = Arc::new(ScriptedTools::new());
        let bad_batch = Step {
            blocks: vec![
                Block::ToolCall(ToolCall {
                    id: "good1".into(),
                    name: "Read".into(),
                    arguments_json: r#"{"path": "x"}"#.into(),
                    thought_signature: None,
                }),
                Block::ToolCall(ToolCall {
                    id: "bad1".into(),
                    name: "Bash".into(),
                    arguments_json: r#"{"command": "rm"#.into(),
                    thought_signature: None,
                }),
            ],
            stop: Some(Stop::MaxTokens),
            usage: Usage::default(),
        };
        let script = vec![bad_batch, MockProvider::text("recovered")];
        let (h, _bus) = harness(script, tools.clone());
        let _ = h.run_turn("go").await;

        {
            let exec = tools.executed.lock().unwrap();
            assert!(
                !exec.iter().any(|(t, _)| t == "Bash"),
                "truncated call never executes"
            );
            assert!(
                exec.iter().any(|(t, _)| t == "Read"),
                "valid sibling in the truncated batch still executes (D5)"
            );
        }

        // every tool_use got exactly one tool_result in history
        let hist = h.history.read().await;
        let assistant_batches: Vec<&ChatMessage> = hist
            .iter()
            .filter(|m| m.role == "assistant" && !m.tool_calls.is_empty())
            .collect();
        assert_eq!(
            assistant_batches.len(),
            1,
            "exactly ONE assistant message carries the batch (no duplicate, D5)"
        );
        let result_ids: Vec<&str> = hist
            .iter()
            .filter_map(|m| m.tool_call_id.as_deref())
            .collect();
        assert!(
            result_ids.contains(&"good1") && result_ids.contains(&"bad1"),
            "every tool_use has a tool_result (D5): {result_ids:?}"
        );
        assert_eq!(result_ids.len(), 2, "no duplicate results");
        let bad_result = hist
            .iter()
            .find(|m| m.tool_call_id.as_deref() == Some("bad1"))
            .unwrap();
        assert!(bad_result.is_error && bad_result.content.contains("TOOL_CALL_TRUNCATED"));
    }

    // ------------------------------------------------------ D6: no unwrap
    #[tokio::test]
    async fn d6_no_primary_model_is_explicit_abort_not_panic() {
        let tools = Arc::new(ScriptedTools::new());
        let (h, bus) = harness(vec![], tools);
        {
            let mut r = h.router.write().await;
            r.set_chain(Role::Primary, vec![]); // empty chain
        }
        let mut sub = bus.subscribe();
        let stop = h.run_turn("x").await;
        assert_eq!(stop.normalized(), "aborted");
        let mut saw_error = false;
        while let Ok(ev) = sub.try_recv() {
            if matches!(ev, Event::Error { .. }) {
                saw_error = true;
            }
        }
        assert!(saw_error, "error event published instead of panic");
    }

    #[tokio::test]
    async fn d6_journal_write_failure_stops_turn() {
        let tools = Arc::new(ScriptedTools::new());
        let (h, _bus) = harness(vec![MockProvider::text("ok")], tools);
        // fault injection: every journal append fails
        h.journal
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .set_fail_writes(true);
        let stop = h.run_turn("hello").await;
        assert!(
            matches!(stop, StopReason::Aborted(ref m) if m.contains("journal write failed")),
            "journal write failure stops the turn (D6), got {stop:?}"
        );
    }

    // ------------------------------------------------------ D7: verify
    #[tokio::test]
    async fn d7_bash_mutation_arms_verify_and_resets_after_pass() {
        let tools = Arc::new(ScriptedTools::new());
        let script = vec![
            MockProvider::tool_call("c1", "Bash", serde_json::json!({"command": "touch f"})),
            MockProvider::text("ran it"),
            MockProvider::text("final answer"),
        ];
        let (mut h, bus) = harness(script, tools);
        h.max_steps = 10;
        let mut sub = bus.subscribe();
        let _ = h.run_turn("run").await;
        let mut verify_count = 0;
        while let Ok(ev) = sub.try_recv() {
            if matches!(ev, Event::VerifyStarted { .. }) {
                verify_count += 1;
            }
        }
        assert_eq!(
            verify_count, 1,
            "Bash arms verify (D7) and it runs exactly once"
        );
    }

    #[tokio::test]
    async fn d7_verify_output_marker_arms_verify() {
        // A tool whose output carries `[verify]` arms verification even
        // though it is not class-mutating (D7).
        struct Marker;
        #[async_trait::async_trait]
        impl ToolHost for Marker {
            async fn execute(&self, _t: &str, _a: &Value, _s: &LoopCtx) -> (String, bool) {
                ("analysis done [verify] please".into(), false)
            }
            fn schemas(&self) -> Vec<Value> {
                vec![]
            }
            fn is_mutating(&self, _t: &str) -> bool {
                false
            }
            fn verify_commands(&self) -> Vec<String> {
                vec!["true".into()]
            }
            async fn run_verify(&self, _s: &LoopCtx) -> (bool, String) {
                (true, String::new())
            }
        }
        let tools = Arc::new(Marker);
        let script = vec![
            MockProvider::tool_call("c1", "Think", serde_json::json!({"thought": "x"})),
            MockProvider::text("done"),
        ];
        let (h, bus) = harness(script, tools);
        let mut sub = bus.subscribe();
        let _ = h.run_turn("analyse").await;
        let mut verify_count = 0;
        while let Ok(ev) = sub.try_recv() {
            if matches!(ev, Event::VerifyStarted { .. }) {
                verify_count += 1;
            }
        }
        assert_eq!(
            verify_count, 1,
            "[verify] marker in tool output arms verification (D7)"
        );
    }

    // ------------------------------------------------------ D8: cancel
    // W5.5: the 4th byte-identical call in one turn is refused (the
    // guard allows MAX_IDENTICAL_CALLS runs, then breaks the loop).
    #[tokio::test]
    async fn w55_identical_call_cap_refuses_after_three_verbatim_runs() {
        let tools = Arc::new(ScriptedTools::new());
        let same = serde_json::json!({"path": "a"});
        let script = vec![
            MockProvider::tool_call("c1", "Read", same.clone()),
            MockProvider::tool_call("c2", "Read", same.clone()),
            MockProvider::tool_call("c3", "Read", same.clone()),
            MockProvider::tool_call("c4", "Read", same.clone()),
            MockProvider::text("done"),
        ];
        let (h, _bus) = harness(script, tools.clone());
        let stop = h.run_turn("loop test").await;
        assert!(
            matches!(stop, StopReason::EndTurn),
            "the turn still ends: {stop:?}"
        );
        let executed = tools.executed.lock().unwrap();
        let read_calls = executed.iter().filter(|(t, _)| t == "Read").count();
        assert_eq!(
            read_calls, 3,
            "three verbatim Reads ran; the 4th was refused: {executed:?}"
        );
    }

    // W5.5 orthogonality: permission DENIALS never trip the loop guard —
    // the denial circuit breaker owns that path (the breaker test's four
    // denied Reads stay four engine denials).
    #[tokio::test]
    async fn w55_denied_calls_do_not_count_as_loops() {
        let tools = Arc::new(ScriptedTools::new());
        tools.deny_tools.lock().unwrap().push("Read".to_string());
        let same = serde_json::json!({"path": "a"});
        let script = vec![
            MockProvider::tool_call("d1", "Read", same.clone()),
            MockProvider::tool_call("d2", "Read", same.clone()),
            MockProvider::tool_call("d3", "Read", same.clone()),
            MockProvider::tool_call("d4", "Read", same.clone()),
            MockProvider::text("gave up"),
        ];
        let (h, _bus) = harness(script, tools.clone());
        let _ = h.run_turn("all denied").await;
        assert_eq!(
            h.denied_tools.load(Ordering::SeqCst),
            4,
            "all four reached the engine as denials (never loop-refused)"
        );
    }

    #[tokio::test]
    async fn d8_cancel_token_aborts_turn_and_tools() {
        let tools = Arc::new(ScriptedTools::new());
        *tools.slow_ms.lock().unwrap() = Some(2_000);
        let script = vec![MockProvider::tool_call(
            "c1",
            "Read",
            serde_json::json!({"path": "a"}),
        )];
        let (h, _bus) = harness(script, tools.clone());
        let cancel = h.cancel.clone();
        let c2 = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            c2.cancel();
        });
        let t0 = std::time::Instant::now();
        let stop = h.run_turn("slow tool").await;
        assert!(
            matches!(stop, StopReason::Aborted(ref m) if m.contains("cancelled")),
            "cancel aborts the turn (D8), got {stop:?}"
        );
        assert!(
            t0.elapsed() < std::time::Duration::from_millis(1_500),
            "tool wait was cut short by cancel"
        );
    }

    #[tokio::test]
    async fn d8_concurrency_safe_tools_run_in_parallel() {
        let tools = Arc::new(ScriptedTools::new());
        *tools.safe.lock().unwrap() = true;
        *tools.slow_ms.lock().unwrap() = Some(150);
        let script = vec![MockProvider::text("done")];
        let (h, _bus) = harness(script, tools.clone());
        // two parallel-safe calls in one turn: exercise execute_batch
        let calls = vec![
            ToolCall {
                id: "p1".into(),
                name: "Read".into(),
                arguments_json: r#"{"path":"a"}"#.into(),
                thought_signature: None,
            },
            ToolCall {
                id: "p2".into(),
                name: "Read".into(),
                arguments_json: r#"{"path":"b"}"#.into(),
                thought_signature: None,
            },
        ];
        h.history.write().await.push(ChatMessage::user("go"));
        h.execute_batch(&calls).await;
        // D8 concurrency is asserted by OBSERVED OVERLAP, not elapsed time:
        // two futures run in parallel iff both were inside their sleeps at the
        // same moment. Sequential execution caps the mark at 1 regardless of
        // how fast or slow the machine is — the assertion is scheduler-proof.
        let max_active = tools.max_active.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            max_active, 2,
            "two 150ms safe tools must overlap (D8); observed max concurrency {max_active}"
        );
        let hist = h.history.read().await;
        let ids: Vec<&str> = hist
            .iter()
            .filter_map(|m| m.tool_call_id.as_deref())
            .collect();
        assert_eq!(ids, vec!["p1", "p2"], "results land in call order");
    }

    #[tokio::test]
    async fn d8_tool_timeout_enforced() {
        struct NeverTools;
        #[async_trait::async_trait]
        impl ToolHost for NeverTools {
            async fn execute(&self, _t: &str, _a: &Value, _s: &LoopCtx) -> (String, bool) {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                ("never".to_string(), false)
            }
            fn schemas(&self) -> Vec<Value> {
                vec![]
            }
            fn is_mutating(&self, _t: &str) -> bool {
                false
            }
            fn verify_commands(&self) -> Vec<String> {
                vec![]
            }
            async fn run_verify(&self, _s: &LoopCtx) -> (bool, String) {
                (true, String::new())
            }
            fn tool_timeout_ms(&self, _t: &str) -> Option<u64> {
                Some(100)
            }
        }
        let bus = EventBus::new(64);
        let mut router = Router::new(crate::providers::CostBook::default(), false);
        router.set_chain(
            Role::Primary,
            vec![crate::router::ModelEntry {
                model: "mock-primary".into(),
                provider: Arc::new(MockProvider::new(vec![MockProvider::tool_call(
                    "t1",
                    "Read",
                    serde_json::json!({}),
                )])),
                priced: true,
            }],
        );
        let (perm, _rx) = PermissionEngine::new(Mode::Yolo, vec![]);
        let tmpdir = marfim_testkit::unique_dir("marfim-tmo");
        let _ = std::fs::create_dir_all(&tmpdir);
        let h = Harness {
            bus: bus.clone(),
            router: Arc::new(RwLock::new(router)),
            tools: Arc::new(NeverTools),
            journal: Arc::new(Mutex::new(Journal::open(&tmpdir.join("t.jsonl")).unwrap())),
            ctx: LoopCtx::default(),
            permissions: Arc::new(perm),
            max_steps: 6,
            max_verify_feedback: 1,
            history: Arc::new(RwLock::new(Vec::new())),
            cancel: CancelToken::default(),
            compact_threshold_tokens: u64::MAX,
            sampling: Sampling::default(),
            context_window: None,
            turn_counter: AtomicU64::new(0),
            last_call: std::sync::Mutex::new((None, 0)),
            turn_tool_calls: AtomicU64::new(0),
            defrag_fails: std::sync::atomic::AtomicU32::new(0),
            cache: crate::providers::cache::CacheSettings::default(),
            allow_custom_tools: false,
            compaction_strategies: crate::compact::DEFAULT_STRATEGIES.to_vec(),
            pending_compaction: std::sync::Mutex::new(None),
            compaction_snapshot_len: std::sync::atomic::AtomicU64::new(0),
            cache_warmed: AtomicBool::new(false),
            denied_tools: AtomicU64::new(0),
            faculties: Vec::new(),
            turn_overrides: std::sync::Mutex::new(TurnOverrides::default()),
            consecutive_denials: std::sync::Mutex::new(0),
            last_turn_aborted: AtomicBool::new(false),
            verify_armed: AtomicBool::new(false),
            stop_on_tool_error: false,
            executed_tools: AtomicU64::new(0),
            steer: Arc::new(Mutex::new(SteerQueues::default())),
            surfaces: PromptSurfaces::default(),
        };
        let stop = h.run_turn("hang").await;
        assert_eq!(stop, StopReason::EndTurn, "timeout result feeds the model");
        let hist = h.history.read().await;
        let tr = hist
            .iter()
            .find(|m| m.tool_call_id.as_deref() == Some("t1"))
            .expect("tool result present");
        assert!(
            tr.is_error && tr.content.contains("timed out after 100ms"),
            "timeout enforced with explicit error result (D8): {}",
            tr.content
        );
    }

    // ------------------------------------------------------ D9: streaming
    #[tokio::test]
    async fn d9_stream_deltas_reach_the_bus() {
        let tools = Arc::new(ScriptedTools::new());
        let (h, bus) = harness(vec![MockProvider::text("streamed text")], tools);
        let mut sub = bus.subscribe();
        let _ = h.run_turn("hi").await;
        let mut saw_delta = false;
        while let Ok(ev) = sub.try_recv() {
            if let Event::StreamDelta { kind, text } = ev {
                if kind == "text" && text == "streamed text" {
                    saw_delta = true;
                }
            }
        }
        assert!(saw_delta, "stream deltas published on the bus (D9)");
    }

    // ------------------------------------------------------ D11: failover
    #[tokio::test]
    async fn d11_failover_chain_recovers_from_transport_error() {
        let tools = Arc::new(ScriptedTools::new());
        let bus = EventBus::new(4096);
        let mut router = Router::new(crate::providers::CostBook::default(), false);
        let failing = Arc::new(MockProvider::from_actions(vec![MockProvider::error(
            "HTTP 503 after retries",
        )]));
        let working = Arc::new(MockProvider::new(vec![MockProvider::text("backup worked")]));
        router.set_chain(
            Role::Primary,
            vec![
                crate::router::ModelEntry {
                    model: "flaky".into(),
                    provider: failing.clone(),
                    priced: true,
                },
                crate::router::ModelEntry {
                    model: "solid".into(),
                    provider: working.clone(),
                    priced: true,
                },
            ],
        );
        router.set_chain(
            Role::Small,
            vec![crate::router::ModelEntry {
                model: "small".into(),
                provider: Arc::new(MockProvider::new(vec![MockProvider::text(
                    "summary long enough",
                )])),
                priced: true,
            }],
        );
        let (perm, _rx) = PermissionEngine::new(Mode::Yolo, vec![]);
        let tmpdir = marfim_testkit::unique_dir("marfim-failover");
        let _ = std::fs::create_dir_all(&tmpdir);
        let h = Harness {
            bus: bus.clone(),
            router: Arc::new(RwLock::new(router)),
            tools,
            journal: Arc::new(Mutex::new(Journal::open(&tmpdir.join("f.jsonl")).unwrap())),
            ctx: LoopCtx::default(),
            permissions: Arc::new(perm),
            max_steps: 6,
            max_verify_feedback: 2,
            history: Arc::new(RwLock::new(Vec::new())),
            cancel: CancelToken::default(),
            compact_threshold_tokens: u64::MAX,
            sampling: Sampling::default(),
            context_window: None,
            turn_counter: AtomicU64::new(0),
            last_call: std::sync::Mutex::new((None, 0)),
            turn_tool_calls: AtomicU64::new(0),
            defrag_fails: std::sync::atomic::AtomicU32::new(0),
            cache: crate::providers::cache::CacheSettings::default(),
            allow_custom_tools: false,
            compaction_strategies: crate::compact::DEFAULT_STRATEGIES.to_vec(),
            pending_compaction: std::sync::Mutex::new(None),
            compaction_snapshot_len: std::sync::atomic::AtomicU64::new(0),
            cache_warmed: AtomicBool::new(false),
            denied_tools: AtomicU64::new(0),
            faculties: Vec::new(),
            turn_overrides: std::sync::Mutex::new(TurnOverrides::default()),
            consecutive_denials: std::sync::Mutex::new(0),
            last_turn_aborted: AtomicBool::new(false),
            verify_armed: AtomicBool::new(false),
            stop_on_tool_error: false,
            executed_tools: AtomicU64::new(0),
            steer: Arc::new(Mutex::new(SteerQueues::default())),
            surfaces: PromptSurfaces::default(),
        };
        let stop = h.run_turn("hi").await;
        assert_eq!(stop, StopReason::EndTurn, "failover completes the turn");
        assert_eq!(
            working.request_count(),
            1,
            "second chain entry served the step (D11)"
        );
        // the flaky model is marked unavailable: the next resolve skips it
        {
            let r = h.router.read().await;
            let (e, tried) = r.resolve(Role::Primary, |_| true).unwrap();
            assert_eq!(e.model, "solid");
            assert!(
                tried.iter().any(|t| t.contains("flaky:unavailable")),
                "flaky marked unavailable: {tried:?}"
            );
        }
    }

    // ------------------------------------------------------ D12: stop paths
    #[tokio::test]
    async fn d12_turn_emitted_on_every_stop_path() {
        // budget-exceeded path emits TurnEnded too
        let tools = Arc::new(ScriptedTools::new());
        let (mut h, bus) = harness(vec![], tools);
        h.max_steps = 0;
        let mut sub = bus.subscribe();
        let stop = h.run_turn("x").await;
        assert_eq!(stop.normalized(), "budget_exceeded");
        let mut ended = Vec::new();
        while let Ok(ev) = sub.try_recv() {
            if let Event::TurnEnded { stop_reason, .. } = ev {
                ended.push(stop_reason);
            }
        }
        assert_eq!(ended, vec!["budget_exceeded".to_string()]);
    }

    // ------------------------------------------------ D4 tighten: the
    // compaction trigger is PER-STEP, and the turn-total usage ledger
    // reflects the post-compaction reset.
    #[tokio::test]
    async fn d4_compaction_per_step_not_cumulative_and_usage_total_resets() {
        let tools = Arc::new(ScriptedTools::new());
        // step1: tool call carrying 150k input tokens (above threshold) →
        // mid-turn compaction fires ONCE and usage_total resets;
        // step2: 40k input (below threshold even though turn-cumulative
        // would be 190k without the reset) → NO second compaction.
        let mut big_call = MockProvider::tool_call("t1", "Read", serde_json::json!({"path": "a"}));
        big_call.usage.input_tokens = 150_000;
        let script = vec![big_call, step_with_usage("done", 40_000)];
        let (mut h, bus) = harness(script, tools);
        h.compact_threshold_tokens = 100_000;
        let mut sub = bus.subscribe();
        let stop = h.run_turn("long task").await;
        assert_eq!(stop.normalized(), "end_turn");

        let mut compactions = 0usize;
        while let Ok(ev) = sub.try_recv() {
            if matches!(ev, Event::Compaction { .. }) {
                compactions += 1;
            }
        }
        assert_eq!(
            compactions, 1,
            "per-step check compacts exactly once; a cumulative check would fire twice (D4)"
        );

        // history starts from the summary after compaction, then step2's text
        let hist = h.history.read().await;
        assert_eq!(hist.len(), 2);
        assert!(hist[0]
            .content
            .contains("summary of the conversation so far"));
        drop(hist);

        // the journal's turn-total usage records the POST-RESET total (40k),
        // not the cumulative 190k — the reset actually happened (D4).
        let path = h
            .journal
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .path()
            .to_path_buf();
        let recs = crate::journal::Journal::replay(&path).unwrap();
        let usage_totals: Vec<u64> = recs
            .iter()
            .filter_map(|r| match r {
                JournalRecord::Usage {
                    role, input_tokens, ..
                } if role == "primary" => Some(*input_tokens),
                _ => None,
            })
            .collect();
        assert_eq!(
            usage_totals,
            vec![40_000],
            "turn-total usage reflects the post-compaction reset (D4), got {usage_totals:?}"
        );
    }

    // ------------------------------------------------ D12 tighten: every
    // stop path emits TurnEnded EXACTLY ONCE with the EXACT normalized
    // reason.
    async fn assert_one_turnended(sub: &mut tokio::sync::broadcast::Receiver<Event>, reason: &str) {
        let mut count = 0usize;
        let mut reasons = Vec::new();
        while let Ok(ev) = sub.try_recv() {
            if let Event::TurnEnded { stop_reason, .. } = ev {
                count += 1;
                reasons.push(stop_reason);
            }
        }
        assert_eq!(
            count, 1,
            "exactly one TurnEnded expected for {reason}, got {count} ({reasons:?})"
        );
        assert_eq!(reasons[0], reason, "exact normalized stop reason (D12)");
    }

    #[tokio::test]
    async fn d12_exact_reason_single_turnended_on_every_stop_path() {
        // (a) clean end
        let (h, bus) = harness(
            vec![MockProvider::text("hi")],
            Arc::new(ScriptedTools::new()),
        );
        let mut sub = bus.subscribe();
        assert_eq!(h.run_turn("x").await.normalized(), "end_turn");
        assert_one_turnended(&mut sub, "end_turn").await;

        // (b) cancelled before the turn → aborted
        let (h, bus) = harness(
            vec![MockProvider::text("hi")],
            Arc::new(ScriptedTools::new()),
        );
        h.cancel.cancel();
        let mut sub = bus.subscribe();
        assert_eq!(h.run_turn("x").await.normalized(), "aborted");
        assert_one_turnended(&mut sub, "aborted").await;

        // (c) tool error with stop-on-fail → aborted
        let tools = Arc::new(ScriptedTools::new());
        tools.fail_tools.lock().unwrap().push("Bash".into());
        let (mut h, bus) = harness(
            vec![MockProvider::tool_call(
                "t1",
                "Bash",
                serde_json::json!({"command": "ls"}),
            )],
            tools,
        );
        h.stop_on_tool_error = true;
        let mut sub = bus.subscribe();
        assert_eq!(h.run_turn("x").await.normalized(), "aborted");
        assert_one_turnended(&mut sub, "aborted").await;

        // (d) verify feedback budget exhausted → verify_exhausted
        let tools = Arc::new(ScriptedTools::new());
        *tools.verify_fails.lock().unwrap() = true;
        let (mut h, bus) = harness(
            vec![
                MockProvider::tool_call("m1", "Bash", serde_json::json!({"command": "make"})),
                MockProvider::text("attempting fix"),
            ],
            tools,
        );
        h.max_verify_feedback = 0;
        let mut sub = bus.subscribe();
        assert_eq!(h.run_turn("x").await.normalized(), "verify_exhausted");
        assert_one_turnended(&mut sub, "verify_exhausted").await;

        // (e) step budget exceeded
        let (mut h, bus) = harness(vec![], Arc::new(ScriptedTools::new()));
        h.max_steps = 0;
        let mut sub = bus.subscribe();
        assert_eq!(h.run_turn("x").await.normalized(), "budget_exceeded");
        assert_one_turnended(&mut sub, "budget_exceeded").await;

        // (f) compaction failure (Small role unresolvable) → compaction_failed
        let (mut h, bus) = harness(
            vec![step_with_usage("big", 150_000)],
            Arc::new(ScriptedTools::new()),
        );
        h.compact_threshold_tokens = 100_000;
        h.router.write().await.set_chain(Role::Small, vec![]);
        let mut sub = bus.subscribe();
        assert_eq!(h.run_turn("x").await.normalized(), "compaction_failed");
        assert_one_turnended(&mut sub, "compaction_failed").await;
    }

    // ------------------------------------------------- P4: actuations
    struct ActuatingFaculty {
        acts: Vec<crate::faculty::Actuation>,
    }

    #[async_trait::async_trait]
    impl crate::faculty::Faculty for ActuatingFaculty {
        fn name(&self) -> &'static str {
            "actuating"
        }
        async fn actuations(
            &self,
            _ctx: &crate::faculty::HookCtx,
        ) -> Vec<crate::faculty::Actuation> {
            self.acts.clone()
        }
    }

    fn act(kind: &str, after: Value, before: Value) -> crate::faculty::Actuation {
        crate::faculty::Actuation {
            kind: kind.into(),
            before,
            after,
            evidence: serde_json::json!({"why": "regression test"}),
        }
    }

    #[tokio::test]
    async fn p4_actuations_validated_applied_and_turn_scoped() {
        let tools = Arc::new(ScriptedTools::new());
        let (mut h, bus) = harness(
            vec![
                MockProvider::tool_call("m1", "Bash", serde_json::json!({"command": "ls"})),
                MockProvider::text("done"),
            ],
            tools,
        );
        // keep the clamp band deterministic: base threshold 100k →
        // [50k, 150k] accepted, 200k clamped out
        h.compact_threshold_tokens = 100_000;
        // give the chain a second entry so a REAL model_select can apply
        h.router.write().await.set_chain(
            Role::Primary,
            vec![
                crate::router::ModelEntry {
                    model: "mock-primary".into(),
                    provider: Arc::new(MockProvider::new(vec![MockProvider::text("never asked")])),
                    priced: true,
                },
                crate::router::ModelEntry {
                    model: "mock-alt".into(),
                    provider: Arc::new(MockProvider::new(vec![
                        MockProvider::tool_call("m1", "Bash", serde_json::json!({"command": "ls"})),
                        MockProvider::text("done"),
                    ])),
                    priced: true,
                },
            ],
        );
        h.add_faculty(Arc::new(ActuatingFaculty {
            acts: vec![
                // (1) valid model_select: in chain, different from before → applied
                act(
                    "model_select",
                    serde_json::json!("mock-alt"),
                    serde_json::json!("mock-primary"),
                ),
                // (2) invalid model_select: not in chain → dropped
                act(
                    "model_select",
                    serde_json::json!("ghost-model"),
                    serde_json::json!("mock-primary"),
                ),
                // (3) valid compact_threshold (no clamp violation) → applied
                act(
                    "compact_threshold",
                    serde_json::json!(50_000),
                    serde_json::json!(100_000),
                ),
                // (4) clamp violation (2x) → dropped
                act(
                    "compact_threshold",
                    serde_json::json!(200_000),
                    serde_json::json!(100_000),
                ),
                // (5) permission_tighten: Bash denied for this turn → applied
                act(
                    "permission_tighten",
                    serde_json::json!(["Bash"]),
                    serde_json::json!([]),
                ),
            ],
        }));
        let mut sub = bus.subscribe();
        let stop = h.run_turn("run the tool").await;
        assert_eq!(stop, StopReason::EndTurn, "turn completes despite tighten");

        // (1)/(2): the turn ran on the alt model (applied), ghost dropped.
        let mut models: Vec<String> = Vec::new();
        while let Ok(ev) = sub.try_recv() {
            if let Event::ModelSelected { model, .. } = ev {
                models.push(model);
            }
        }
        assert!(
            models.iter().all(|m| m == "mock-alt"),
            "faculty-selected model served every step: {models:?}"
        );

        // (5): the Bash call was denied by the tighten actuation. The
        // read guard is scoped: turn 2 below re-takes the lock for write.
        {
            let hist = h.history.read().await;
            let tr = hist
                .iter()
                .find(|m| m.tool_call_id.as_deref() == Some("m1"))
                .expect("tool result present");
            assert!(
                tr.is_error && tr.content.contains("permission_tighten"),
                "tightened tool denied with the actuation reason: {}",
                tr.content
            );
        }

        // Journal carries every actuation with its disposition (applied
        // AND dropped: the audit trail is complete either way).
        let recs = crate::journal::Journal::replay(
            h.journal.lock().unwrap_or_else(|p| p.into_inner()).path(),
        )
        .unwrap_or_default();
        let acts: Vec<&JournalRecord> = recs.iter().filter(|r| r.kind() == "actuation").collect();
        assert_eq!(acts.len(), 5, "all five actuations journaled");
        let dispositions: Vec<String> = acts
            .iter()
            .filter_map(|r| match r {
                JournalRecord::Actuation { detail } => detail
                    .get("disposition")
                    .and_then(|d| d.as_str())
                    .map(str::to_string),
                _ => None,
            })
            .collect();
        assert_eq!(
            dispositions,
            vec![
                "applied",
                "dropped: model not in chain",
                "applied",
                "dropped: outside [0.5x, 1.5x] clamp",
                "applied",
            ],
            "exact dispositions in order: {dispositions:?}"
        );

        // Turn-scoped: a SECOND turn has no overrides (fresh state), so
        // the primary model serves it and Bash executes again.
        h.router.write().await.set_chain(
            Role::Primary,
            vec![crate::router::ModelEntry {
                model: "mock-primary".into(),
                provider: Arc::new(MockProvider::new(vec![MockProvider::text("second turn")])),
                priced: true,
            }],
        );
        let stop2 = h.run_turn("again").await;
        assert_eq!(stop2, StopReason::EndTurn);
        let hist2 = h.history.read().await;
        assert!(
            hist2
                .iter()
                .any(|m| m.role == "assistant" && m.content.contains("second turn")),
            "second turn ran on the default chain (overrides cleared)"
        );
    }

    // ---- Distillation batch 2 tests (§A breaker, §W healing, §R2 key,
    //      §S nudge, §C verbatim+footer, §C2 tail budget) ----

    #[test]
    fn prompt_cache_key_is_prefixed_and_capped() {
        let k = prompt_cache_key("0123456789abcdef".repeat(8).as_str());
        assert!(k.starts_with("pc_"));
        assert!(k.len() <= 64, "len {}", k.len());
        assert_eq!(prompt_cache_key("abc"), "pc_abc");
    }

    #[test]
    fn system_prompt_nudge_appends_only_when_present() {
        let ctx = LoopCtx::default();
        let base = system_prompt(&ctx);
        let with = system_prompt_with_nudge(&ctx, Some(MEMORY_NUDGE));
        assert!(with.starts_with(&base));
        assert!(with.contains(MEMORY_NUDGE));
        assert_eq!(system_prompt_with_nudge(&ctx, None), base);
        assert_eq!(system_prompt_with_nudge(&ctx, Some("")), base);
    }

    #[tokio::test]
    async fn interrupt_healing_closes_aborted_exchange_structurally() {
        let tools = Arc::new(ScriptedTools::new());
        let script = vec![MockProvider::text("answer")];
        let (h, _bus) = harness(script, tools);
        // simulate an aborted turn that left a trailing user message
        h.history
            .write()
            .await
            .push(ChatMessage::user("was cut off"));
        h.last_turn_aborted.store(true, Ordering::SeqCst);
        let stop = h.run_turn("next question").await;
        assert_eq!(stop, StopReason::EndTurn);
        let hist = h.history.read().await;
        assert!(
            hist.iter()
                .any(|m| m.role == "assistant" && m.content == INTERRUPT_PLACEHOLDER),
            "structural placeholder closes the aborted exchange"
        );
        // and it is NOT re-added on the next turn
        let placeholders = hist
            .iter()
            .filter(|m| m.content == INTERRUPT_PLACEHOLDER)
            .count();
        assert_eq!(placeholders, 1);
    }

    #[tokio::test]
    async fn no_healing_when_previous_turn_ended_cleanly() {
        let tools = Arc::new(ScriptedTools::new());
        let script = vec![MockProvider::text("ok")];
        let (h, _bus) = harness(script, tools);
        h.history.write().await.push(ChatMessage::user("old"));
        h.last_turn_aborted.store(false, Ordering::SeqCst);
        let _ = h.run_turn("new").await;
        let hist = h.history.read().await;
        assert!(
            !hist.iter().any(|m| m.content == INTERRUPT_PLACEHOLDER),
            "clean history is never touched"
        );
    }

    #[tokio::test]
    async fn denial_circuit_breaker_injects_stop_addendum_at_three() {
        // Four denied Read calls: the streak trips at exactly 3, so ONE
        // addendum lands; the 4th denial does not add a second one.
        let tools = Arc::new(ScriptedTools::new());
        tools.deny_tools.lock().unwrap().push("Read".into());
        let script = vec![
            MockProvider::tool_call("c1", "Read", serde_json::json!({"path": "a"})),
            MockProvider::tool_call("c2", "Read", serde_json::json!({"path": "a"})),
            MockProvider::tool_call("c3", "Read", serde_json::json!({"path": "a"})),
            MockProvider::tool_call("c4", "Read", serde_json::json!({"path": "a"})),
            MockProvider::text("gave up"),
        ];
        let (h, _bus) = harness(script, tools);
        let stop = h.run_turn("try reads").await;
        assert_eq!(stop, StopReason::EndTurn);
        let hist = h.history.read().await;
        let addenda = hist
            .iter()
            .filter(|m| m.role == "user" && m.content.contains("[circuit breaker]"))
            .count();
        assert_eq!(addenda, 1, "exactly one addendum at the LIMIT");
        assert_eq!(h.denied_tools.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn denial_streak_resets_on_a_successful_call_and_at_turn_end() {
        let tools = Arc::new(ScriptedTools::new());
        tools.deny_tools.lock().unwrap().push("Read".into());
        // deny, deny, SUCCESS (Write), deny, deny → never trips (streak
        // broken by the success).
        let script = vec![
            MockProvider::tool_call("c1", "Read", serde_json::json!({"path": "a"})),
            MockProvider::tool_call("c2", "Read", serde_json::json!({"path": "a"})),
            MockProvider::tool_call("c3", "Write", serde_json::json!({"path": "a"})),
            MockProvider::tool_call("c4", "Read", serde_json::json!({"path": "a"})),
            MockProvider::tool_call("c5", "Read", serde_json::json!({"path": "a"})),
            MockProvider::text("done"),
        ];
        let (h, _bus) = harness(script, tools);
        let _ = h.run_turn("mixed").await;
        let hist = h.history.read().await;
        assert!(
            !hist.iter().any(|m| m.content.contains("[circuit breaker]")),
            "streak broken by success — no addendum"
        );
    }

    #[tokio::test]
    async fn compaction_summary_prompt_carries_verbatim_and_footer() {
        let tools = Arc::new(ScriptedTools::new());
        let script = vec![MockProvider::text("step text")];
        let (h, _bus) = harness(script, tools);
        h.history.write().await.push(ChatMessage::user(
            "please fix the frobnicator in src/lib.rs",
        ));
        h.history
            .write()
            .await
            .push(ChatMessage::assistant("working on it"));
        // Keep the typed Arc so the captured calls stay reachable.
        let small_mock = Arc::new(MockProvider::new(vec![MockProvider::text("summary")]));
        h.router.write().await.set_chain(
            Role::Small,
            vec![crate::router::ModelEntry {
                model: "mock-small".into(),
                provider: small_mock.clone(),
                priced: true,
            }],
        );
        let (_s, _u, _m) = h.compact_via_small().await.unwrap();
        let calls = small_mock.calls.lock().unwrap();
        let body = calls.last().unwrap().messages[0].content.clone();
        assert!(
            body.contains("VERBATIM RECENT USER REQUESTS"),
            "verbatim section present"
        );
        assert!(
            body.contains("frobnicator in src/lib.rs"),
            "user message text carried verbatim"
        );
        assert!(
            body.contains("SessionSearch"),
            "recovery footer steers to session search"
        );
        assert!(body.contains("=== CONVERSATION ==="));
    }

    #[tokio::test]
    async fn run_turn_request_carries_session_prompt_cache_key() {
        let tools = Arc::new(ScriptedTools::new());
        let primary_mock = Arc::new(MockProvider::new(vec![MockProvider::text("hi")]));
        let (h, _bus) = harness(vec![], tools);
        h.router.write().await.set_chain(
            Role::Primary,
            vec![crate::router::ModelEntry {
                model: "mock-primary".into(),
                provider: primary_mock.clone(),
                priced: true,
            }],
        );
        let _ = h.run_turn("q").await;
        let calls = primary_mock.calls.lock().unwrap();
        let key = calls.last().unwrap().prompt_cache_key.clone();
        assert_eq!(key, Some(prompt_cache_key(&h.ctx.session_id)));
        assert!(key.unwrap().starts_with("pc_"));
    }

    #[tokio::test]
    async fn memory_nudge_fires_every_tenth_turn() {
        let tools = Arc::new(ScriptedTools::new());
        // Empty script: the mock answers with its default step forever,
        // so the test can drive 12 real turns.
        let primary_mock = Arc::new(MockProvider::new(vec![]));
        let (h, _bus) = harness(vec![], tools);
        h.router.write().await.set_chain(
            Role::Primary,
            vec![crate::router::ModelEntry {
                model: "mock-primary".into(),
                provider: primary_mock.clone(),
                priced: true,
            }],
        );
        for _ in 0..12 {
            let _ = h.run_turn("q").await;
        }
        let calls = primary_mock.calls.lock().unwrap();
        assert_eq!(calls.len(), 12);
        let nudged: Vec<usize> = calls
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                r.system
                    .as_deref()
                    .map(|s| s.contains(MEMORY_NUDGE))
                    .unwrap_or(false)
            })
            .map(|(i, _)| i)
            .collect();
        assert_eq!(nudged, vec![9], "exactly turn 10 (0-based index 9)");
    }

    // ---------------------------------------------- W10.1 prompt surfaces

    #[test]
    fn surfaces_absent_keeps_base_prompt_byte_identical() {
        let ctx = LoopCtx {
            session_id: "s".into(),
            cwd: "/tmp".into(),
            cancel: CancelToken::default(),
        };
        let empty = PromptSurfaces::default();
        assert_eq!(
            system_prompt_with_surfaces(&ctx, None, &empty),
            system_prompt(&ctx)
        );
        assert_eq!(system_prompt(&ctx), system_prompt_with_nudge(&ctx, None));
    }

    #[test]
    fn surfaces_present_append_sections_in_order() {
        let ctx = LoopCtx {
            session_id: "s".into(),
            cwd: "/tmp".into(),
            cancel: CancelToken::default(),
        };
        let s = PromptSurfaces {
            memory: Some("m1: likes rust".into()),
            instincts: Some(
                "Learned behaviors (instincts — follow unless the user overrides):\n1. run fmt"
                    .into(),
            ),
            skills: Some("Available skills (Skill tool to load one):\n- tidy: tidy up".into()),
        };
        let full = system_prompt_with_surfaces(&ctx, None, &s);
        let base = system_prompt(&ctx);
        assert!(full.starts_with(&base));
        let mi = full
            .find("Project memory (persistent; the Memory tool manages it):")
            .unwrap();
        let ii = full.find("Learned behaviors").unwrap();
        let si = full.find("Available skills").unwrap();
        assert!(mi < ii && ii < si, "memory → instincts → skills order");
        assert!(full.contains("m1: likes rust"));
    }

    #[test]
    fn memory_surface_is_capped() {
        let ctx = LoopCtx {
            session_id: "s".into(),
            cwd: "/tmp".into(),
            cancel: CancelToken::default(),
        };
        let big = "x".repeat(5000);
        let capped = crate::memcaps::cap_memory(&big);
        let s = PromptSurfaces {
            memory: Some(capped.clone()),
            ..Default::default()
        };
        let full = system_prompt_with_surfaces(&ctx, None, &s);
        assert!(capped.starts_with(crate::memcaps::CAP_MARKER));
        assert!(full.contains(&capped));
        assert!(capped.chars().count() <= crate::memcaps::MEMORY_CAP_CHARS);
    }
}
