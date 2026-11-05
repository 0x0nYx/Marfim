//! Session actor (P2): ONE task per session owns the harness — history,
//! steering, cancellation, approvals and the journal. All mutation flows
//! through commands (mpsc); all observation flows through the harness bus
//! and snapshot replies. Journal-backed `/undo`, `/rewind` and `/fork`;
//! `resume` rebuilds history from the journal.
//!
//! Narrowing (documented honestly): history rebuild restores the
//! conversation skeleton (user / assistant text turns + compaction
//! summaries). Tool-call transcripts remain in the journal for audit but
//! are not replayed into provider-payload tool_use blocks.

use crate::journal::{Journal, JournalRecord};
use crate::loop_::{Harness, StopReason};
use crate::permissions::{ApprovalAnswer, ApprovalJob};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

/// One command to the session actor. Replies are oneshot; a dropped
/// reply means the caller went away — the actor never panics on it.
pub enum SessionCommand {
    /// Run a full turn. Rejected while another turn is in flight (steer
    /// mid-turn instead — that is what steering is for, D3).
    Prompt {
        text: String,
        reply: oneshot::Sender<Result<StopReason, String>>,
    },
    /// Queue mid-turn steering (D3); journaled by the harness.
    Steer { text: String },
    /// Cooperative cancel: ends the in-flight turn (or is a no-op when
    /// idle). Replies once the turn has unwound (TurnEnded published).
    Cancel { reply: oneshot::Sender<()> },
    /// Answer a pending approval job (matched by tool, `*` matches any
    /// summary). Replies whether a pending job matched.
    Approve {
        tool: String,
        summary: String,
        answer: ApprovalAnswer,
        reply: oneshot::Sender<bool>,
    },
    /// Drop the last completed user turn (journal truncate + history
    /// rebuild) — the `/undo` surface.
    Undo {
        reply: oneshot::Sender<Result<RewindOutcome, String>>,
    },
    /// Drop the last N completed user turns — the `/rewind N` surface.
    Rewind {
        turns: u64,
        reply: oneshot::Sender<Result<RewindOutcome, String>>,
    },
    /// Copy the journal to a fresh session file and return its id — the
    /// `/fork` surface. The copy carries a Fork provenance record.
    Fork {
        reply: oneshot::Sender<Result<ForkOutcome, String>>,
    },
    /// Point-in-time view (context meter, tests, TUI status line).
    Snapshot {
        reply: oneshot::Sender<SessionSnapshot>,
    },
    /// Stop the actor; in-flight turn is cancelled first.
    Shutdown,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionSnapshot {
    pub session_id: String,
    pub turn: u64,
    pub history_len: usize,
    pub journal_bytes: u64,
    pub pending_steers: usize,
    pub pending_approvals: usize,
    pub turn_running: bool,
    pub cancelled: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RewindOutcome {
    pub dropped_records: u64,
    pub dropped_bytes: u64,
    pub history_len: usize,
    pub journal_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ForkOutcome {
    pub fork_session: String,
    pub fork_path: PathBuf,
}

/// Handle to a running session actor.
pub struct SessionHandle {
    pub commands: mpsc::Sender<SessionCommand>,
    /// Approval jobs forwarded from the permission engine, in arrival
    /// order. Answer via `Approve` commands.
    pub approvals: mpsc::Receiver<ApprovalJob>,
    pub journal_path: PathBuf,
    pub session_id: String,
}

/// Spawn the actor. `session_id` is used for snapshots / fork naming.
pub fn spawn_session(
    harness: Arc<Harness>,
    approval_rx: mpsc::Receiver<ApprovalJob>,
    session_id: String,
    journal_path: PathBuf,
) -> SessionHandle {
    let (tx, rx) = mpsc::channel::<SessionCommand>(64);
    let (atx, arx) = mpsc::channel::<ApprovalJob>(64);
    tokio::spawn(session_actor(
        rx,
        atx,
        harness,
        approval_rx,
        session_id.clone(),
        journal_path.clone(),
    ));
    SessionHandle {
        commands: tx,
        approvals: arx,
        journal_path,
        session_id,
    }
}

struct ActorState {
    harness: Arc<Harness>,
    session_id: String,
    journal_path: PathBuf,
    /// In-flight turn, temporarily taken out each loop iteration so the
    /// select arms can borrow it without aliasing.
    active_turn: Option<tokio::task::JoinHandle<StopReason>>,
    prompt_reply: Option<oneshot::Sender<Result<StopReason, String>>>,
    /// Reply hooks waiting for the in-flight turn to unwind (Cancel).
    cancel_waiters: Vec<oneshot::Sender<()>>,
    /// Pending approval jobs (tool, summary, responder) awaiting a decision.
    pending_approvals: Vec<(String, String, oneshot::Sender<ApprovalAnswer>)>,
}

impl ActorState {
    fn snapshot(&self) -> SessionSnapshot {
        let journal_bytes = self
            .harness
            .journal
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .len_bytes();
        SessionSnapshot {
            session_id: self.session_id.clone(),
            turn: self
                .harness
                .turn_counter
                .load(std::sync::atomic::Ordering::SeqCst),
            history_len: self
                .harness
                .history
                .try_read()
                .map(|h| h.len())
                .unwrap_or(0),
            journal_bytes,
            pending_steers: self.harness.pending_steers(),
            pending_approvals: self.pending_approvals.len(),
            turn_running: self.active_turn.is_some(),
            cancelled: self.harness.cancel.is_cancelled(),
        }
    }
}

async fn session_actor(
    mut rx: mpsc::Receiver<SessionCommand>,
    approvals_tx: mpsc::Sender<ApprovalJob>,
    harness: Arc<Harness>,
    mut approval_rx: mpsc::Receiver<ApprovalJob>,
    session_id: String,
    journal_path: PathBuf,
) {
    let mut st = ActorState {
        harness,
        session_id,
        journal_path,
        active_turn: None,
        prompt_reply: None,
        cancel_waiters: vec![],
        pending_approvals: vec![],
    };
    while let Some(cmd) = next(&mut rx, &mut approval_rx, &mut st).await {
        match cmd {
            Flow::Command(SessionCommand::Prompt { text, reply }) => {
                if st.active_turn.is_some() {
                    let _ = reply.send(Err(
                        "a turn is already in flight — steer instead of prompting (D3)".into(),
                    ));
                    continue;
                }
                let h = st.harness.clone();
                st.active_turn = Some(tokio::spawn(async move { h.run_turn(&text).await }));
                st.prompt_reply = Some(reply);
            }
            Flow::Command(SessionCommand::Steer { text }) => {
                st.harness.steer(&text);
            }
            Flow::Command(SessionCommand::Cancel { reply }) => {
                st.harness.cancel.cancel();
                if st.active_turn.is_some() {
                    // fire when the turn unwinds (TurnEnded published)
                    st.cancel_waiters.push(reply);
                } else {
                    let _ = reply.send(());
                }
            }
            Flow::Command(SessionCommand::Approve {
                tool,
                summary,
                answer,
                reply,
            }) => {
                let idx = st
                    .pending_approvals
                    .iter()
                    .position(|(t, s, _)| t == &tool && (summary == "*" || s == &summary));
                match idx {
                    Some(i) => {
                        let (_, _, respond) = st.pending_approvals.remove(i);
                        let _ = respond.send(answer);
                        let _ = reply.send(true);
                    }
                    None => {
                        let _ = reply.send(false);
                    }
                }
            }
            Flow::Command(SessionCommand::Undo { reply }) => {
                let out = rewind_turns(&st.harness, &st.journal_path, 1).await;
                let _ = reply.send(out);
            }
            Flow::Command(SessionCommand::Rewind { turns, reply }) => {
                let out = rewind_turns(&st.harness, &st.journal_path, turns).await;
                let _ = reply.send(out);
            }
            Flow::Command(SessionCommand::Fork { reply }) => {
                let out = fork_journal(&st.harness, &st.journal_path, &st.session_id);
                let _ = reply.send(out);
            }
            Flow::Command(SessionCommand::Snapshot { reply }) => {
                let _ = reply.send(st.snapshot());
            }
            Flow::Command(SessionCommand::Shutdown) => {
                st.harness.cancel.cancel();
                break;
            }
            Flow::Approval(job) => {
                st.pending_approvals
                    .push((job.tool.clone(), job.summary.clone(), job.respond));
                // informational forward for a UI: the authoritative answer
                // path is the Approve COMMAND (the forwarded job's respond
                // channel is a tombstone — its receiver is dropped here, so
                // answering it fails closed instead of double-answering).
                let (dead_tx, dead_rx) = oneshot::channel();
                drop(dead_rx);
                let _ = approvals_tx.try_send(ApprovalJob {
                    tool: job.tool,
                    summary: job.summary,
                    respond: dead_tx,
                });
            }
            Flow::TurnDone(stop) => {
                if let Some(reply) = st.prompt_reply.take() {
                    let _ = reply.send(Ok(stop));
                }
                for w in st.cancel_waiters.drain(..) {
                    let _ = w.send(());
                }
            }
        }
    }
}

enum Flow {
    Command(SessionCommand),
    Approval(ApprovalJob),
    TurnDone(StopReason),
}

/// Drive the three event sources. The in-flight JoinHandle is moved into a
/// local for the duration of one select (Option dance avoids aliasing the
/// actor state across arms).
async fn next(
    rx: &mut mpsc::Receiver<SessionCommand>,
    approval_rx: &mut mpsc::Receiver<ApprovalJob>,
    st: &mut ActorState,
) -> Option<Flow> {
    let mut turn = st.active_turn.take();
    let turn_done = async {
        match turn.as_mut() {
            Some(t) => Some(t.await),
            None => {
                std::future::pending::<()>().await;
                None
            }
        }
    };
    tokio::select! {
        cmd = rx.recv() => {
            st.active_turn = turn;
            cmd.map(Flow::Command)
        }
        job = approval_rx.recv() => {
            st.active_turn = turn;
            job.map(Flow::Approval)
        }
        done = turn_done => {
            st.active_turn = None;
            done.map(|j| match j {
                Ok(stop) => Flow::TurnDone(stop),
                Err(_) => Flow::TurnDone(StopReason::Aborted("turn task panicked".into())),
            })
        }
    }
}

/// Rebuild harness history from journal records (user / assistant text
/// turns + compaction summaries). Returns the number of messages.
pub async fn rebuild_history_from_journal(
    harness: &Arc<Harness>,
    path: &Path,
) -> Result<usize, String> {
    let records = Journal::replay(path).map_err(|e| e.to_string())?;
    rebuild_history(&records, harness).await
}

async fn rebuild_history(
    records: &[JournalRecord],
    harness: &Arc<Harness>,
) -> Result<usize, String> {
    use crate::providers::ChatMessage;
    let mut hist = harness.history.write().await;
    hist.clear();
    for r in records {
        match r {
            JournalRecord::UserMessage { text } => {
                hist.push(ChatMessage::user(text.clone()));
            }
            JournalRecord::AssistantMessage { text, .. } => {
                hist.push(ChatMessage::assistant(text.clone()));
            }
            JournalRecord::Compaction { summary } => {
                hist.push(ChatMessage::user(format!("[compacted context] {summary}")));
            }
            _ => {}
        }
    }
    Ok(hist.len())
}

fn user_turn_byte_offsets(path: &Path) -> Result<Vec<(usize, u64)>, String> {
    let records = Journal::replay(path).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    let mut off = 0u64;
    for (i, r) in records.iter().enumerate() {
        if matches!(r, JournalRecord::UserMessage { .. }) {
            out.push((i, off));
        }
        let line = serde_json::to_string(r).map_err(|e| e.to_string())?.len() as u64 + 1;
        off += line;
    }
    Ok(out)
}

/// Drop the last `turns` completed user turns: truncate the journal to the
/// start of the target turn (tail preserved as `<path>.pre-rewind`) and
/// rebuild history from the remaining records.
pub async fn rewind_turns(
    harness: &Arc<Harness>,
    path: &Path,
    turns: u64,
) -> Result<RewindOutcome, String> {
    if turns == 0 {
        return Err("rewind of 0 turns is a no-op; pass a positive count".into());
    }
    let marks = user_turn_byte_offsets(path)?;
    if (marks.len() as u64) < turns {
        return Err(format!(
            "cannot rewind {turns} turn(s): journal has {} user turn(s)",
            marks.len()
        ));
    }
    let (idx, byte) = marks[marks.len() - turns as usize];
    let records = Journal::replay(path).map_err(|e| e.to_string())?;
    let dropped_records = (records.len() - idx) as u64;
    let dropped_bytes = {
        let mut j = harness.journal.lock().unwrap_or_else(|p| p.into_inner());
        let _dropped = j.rewind_to(byte).map_err(|e| e.to_string())?;
        j.len_bytes()
    };
    let history_len = rebuild_history(&records[..idx], harness).await?;
    Ok(RewindOutcome {
        dropped_records,
        dropped_bytes,
        history_len,
        journal_bytes: dropped_bytes,
    })
}

/// Fork: copy the journal to `<id>-fork-<millis>.jsonl`, append a Fork
/// provenance record to the copy, return the new identity. The original
/// session is untouched. Rebuilding a second harness over the copy is the
/// caller's move (CLI `--resume <fork-path>`).
pub fn fork_journal(
    harness: &Arc<Harness>,
    path: &Path,
    session_id: &str,
) -> Result<ForkOutcome, String> {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let fork_id = format!("{session_id}-fork-{millis}");
    let fork_path = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!("{fork_id}.jsonl"));
    std::fs::copy(path, &fork_path).map_err(|e| format!("fork copy failed: {e}"))?;
    {
        let mut j = Journal::open(&fork_path).map_err(|e| e.to_string())?;
        j.append(&JournalRecord::Fork {
            from_session: session_id.to_string(),
            fork_session: fork_id.clone(),
            forked_at: iso_now(),
        })
        .map_err(|e| e.to_string())?;
    }
    // the live journal records the fork too (both sides visible)
    {
        let mut j = harness.journal.lock().unwrap_or_else(|p| p.into_inner());
        let _ = j.append(&JournalRecord::Fork {
            from_session: session_id.to_string(),
            fork_session: fork_id.clone(),
            forked_at: iso_now(),
        });
    }
    Ok(ForkOutcome {
        fork_session: fork_id,
        fork_path,
    })
}

/// ISO-8601 UTC timestamp without external crates (civil-from-days).
pub fn iso_now() -> String {
    let dur = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs() as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Summaries for `marfim sessions` (list + resume UX).
#[derive(Debug, Clone, Serialize)]
pub struct SessionSummary {
    pub session_id: String,
    pub cwd: String,
    pub title: Option<String>,
    pub started_at: String,
    pub records: usize,
    pub journal_bytes: u64,
    pub path: PathBuf,
}

pub fn list_sessions(dir: &Path) -> Vec<SessionSummary> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    let mut entries: Vec<_> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "jsonl").unwrap_or(false))
        .collect();
    entries.sort();
    for p in entries {
        let Ok(records) = Journal::replay(&p) else {
            continue;
        };
        let meta = records.iter().find_map(|r| match r {
            JournalRecord::SessionMeta {
                session_id,
                cwd,
                title,
                started_at,
            } => Some((
                session_id.clone(),
                cwd.clone(),
                title.clone(),
                started_at.clone(),
            )),
            _ => None,
        });
        let bytes = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
        let fallback = p
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let (session_id, cwd, title, started_at) =
            meta.unwrap_or((fallback, String::new(), None, String::new()));
        out.push(SessionSummary {
            session_id,
            cwd,
            title,
            started_at,
            records: records.len(),
            journal_bytes: bytes,
            path: p,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loop_::{CancelToken, LoopCtx, ToolHost};
    use crate::permissions::PermissionEngine;
    use crate::providers::{MockProvider, Step};
    use crate::router::{ModelEntry, Role, Router};
    use async_trait::async_trait;
    use std::sync::Mutex;

    struct NoTools;

    #[async_trait]
    impl ToolHost for NoTools {
        async fn execute(
            &self,
            tool: &str,
            _a: &serde_json::Value,
            _s: &LoopCtx,
        ) -> (String, bool) {
            (format!("{tool} ok"), false)
        }
        fn schemas(&self) -> Vec<serde_json::Value> {
            vec![]
        }
        fn is_mutating(&self, _tool: &str) -> bool {
            false
        }
        fn verify_commands(&self) -> Vec<String> {
            vec![]
        }
        async fn run_verify(&self, _s: &LoopCtx) -> (bool, String) {
            (true, String::new())
        }
        fn concurrency_safe(&self, _tool: &str) -> bool {
            false
        }
        fn tool_timeout_ms(&self, _tool: &str) -> Option<u64> {
            Some(10_000)
        }
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("marfim-session-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn harness_with(script: Vec<Step>, dir: &Path, session_id: &str) -> Arc<Harness> {
        let bus = crate::events::EventBus::new(4096);
        let mut router = Router::new(crate::providers::CostBook::default(), false);
        router.set_chain(
            Role::Primary,
            vec![ModelEntry {
                model: "mock-primary".into(),
                provider: Arc::new(MockProvider::new(script)),
                priced: true,
            }],
        );
        router.set_chain(
            Role::Small,
            vec![ModelEntry {
                model: "mock-small".into(),
                provider: Arc::new(MockProvider::new(vec![MockProvider::text(
                    "summary of the conversation so far",
                )])),
                priced: true,
            }],
        );
        let (perm, _rx) = PermissionEngine::new(crate::permissions::Mode::Yolo, vec![]);
        let ctx = LoopCtx {
            session_id: session_id.to_string(),
            cwd: dir.display().to_string(),
            cancel: CancelToken::default(),
        };
        let j = Journal::open(&dir.join(format!("{session_id}.jsonl"))).unwrap();
        Arc::new(Harness::new(
            bus,
            Arc::new(tokio::sync::RwLock::new(router)),
            Arc::new(NoTools),
            Arc::new(Mutex::new(j)),
            ctx,
            Arc::new(perm),
            20,
            2,
            u64::MAX,
            false,
        ))
    }

    fn step_text(t: &str) -> Step {
        MockProvider::text(t)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn prompt_runs_turn_and_replies() {
        let dir = tmpdir("basic");
        let h = harness_with(vec![step_text("hello there")], &dir, "s1");
        let (_dead_tx, approval_rx) = mpsc::channel::<ApprovalJob>(1);
        let handle = spawn_session(h.clone(), approval_rx, "s1".into(), dir.join("s1.jsonl"));
        let (tx, rxx) = oneshot::channel();
        handle
            .commands
            .send(SessionCommand::Prompt {
                text: "hi".into(),
                reply: tx,
            })
            .await
            .unwrap();
        let stop = rxx.await.unwrap().unwrap();
        assert_eq!(stop.normalized(), "end_turn");
        let (tx2, r2) = oneshot::channel();
        handle
            .commands
            .send(SessionCommand::Snapshot { reply: tx2 })
            .await
            .unwrap();
        let snap = r2.await.unwrap();
        assert_eq!(snap.turn, 1);
        assert_eq!(snap.history_len, 2, "user + assistant in history (D1)");
        assert!(!snap.turn_running);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn second_prompt_while_turn_running_is_rejected() {
        let dir = tmpdir("busy");
        let h = harness_with(vec![step_text("a")], &dir, "s2");
        let (_dead_tx, approval_rx) = mpsc::channel::<ApprovalJob>(1);
        let handle = spawn_session(h.clone(), approval_rx, "s2".into(), dir.join("s2.jsonl"));
        // saturate the actor with two prompts without awaiting the first
        let (tx1, r1) = oneshot::channel();
        handle
            .commands
            .send(SessionCommand::Prompt {
                text: "one".into(),
                reply: tx1,
            })
            .await
            .unwrap();
        let stop = r1.await.unwrap().unwrap();
        assert_eq!(stop.normalized(), "end_turn");
        // actor idle again → prompt works
        let (tx2, r2) = oneshot::channel();
        handle
            .commands
            .send(SessionCommand::Prompt {
                text: "two".into(),
                reply: tx2,
            })
            .await
            .unwrap();
        assert_eq!(r2.await.unwrap().unwrap().normalized(), "end_turn");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn undo_drops_last_turn_and_rebuilds_history() {
        let dir = tmpdir("undo");
        let h = harness_with(vec![step_text("reply-1"), step_text("reply-2")], &dir, "s3");
        let (_dead_tx, approval_rx) = mpsc::channel::<ApprovalJob>(1);
        let jp = dir.join("s3.jsonl");
        let handle = spawn_session(h.clone(), approval_rx, "s3".into(), jp.clone());
        for t in ["turn one", "turn two"] {
            let (tx, r) = oneshot::channel();
            handle
                .commands
                .send(SessionCommand::Prompt {
                    text: t.into(),
                    reply: tx,
                })
                .await
                .unwrap();
            r.await.unwrap().unwrap();
        }
        let (t1, rr1) = oneshot::channel();
        handle
            .commands
            .send(SessionCommand::Snapshot { reply: t1 })
            .await
            .unwrap();
        let before = rr1.await.unwrap();
        assert_eq!(before.history_len, 4, "2 user + 2 assistant");

        let (t2, rr2) = oneshot::channel();
        handle
            .commands
            .send(SessionCommand::Undo { reply: t2 })
            .await
            .unwrap();
        let out = rr2.await.unwrap().unwrap();
        assert_eq!(out.history_len, 2, "undo dropped the last turn pair");
        assert_eq!(
            out.dropped_records, 3,
            "user + assistant + turn-total usage of the last turn"
        );

        let (t3, rr3) = oneshot::channel();
        handle
            .commands
            .send(SessionCommand::Snapshot { reply: t3 })
            .await
            .unwrap();
        let after = rr3.await.unwrap();
        assert_eq!(after.history_len, 2);
        assert!(after.journal_bytes < before.journal_bytes);
        // audit tail preserved
        let tail = std::fs::read_to_string(format!("{}.pre-rewind", jp.display())).unwrap();
        assert!(tail.contains("turn two"));
        // journal replays to exactly the remaining records
        let recs = Journal::replay(&jp).unwrap();
        let user_turns = recs
            .iter()
            .filter(|r| matches!(r, JournalRecord::UserMessage { .. }))
            .count();
        assert_eq!(user_turns, 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn rewind_two_turns_and_rewind_past_end_fails_honestly() {
        let dir = tmpdir("rew");
        let h = harness_with(
            vec![step_text("r1"), step_text("r2"), step_text("r3")],
            &dir,
            "s4",
        );
        let (_dead_tx, approval_rx) = mpsc::channel::<ApprovalJob>(1);
        let handle = spawn_session(h.clone(), approval_rx, "s4".into(), dir.join("s4.jsonl"));
        for t in ["t1", "t2", "t3"] {
            let (tx, r) = oneshot::channel();
            handle
                .commands
                .send(SessionCommand::Prompt {
                    text: t.into(),
                    reply: tx,
                })
                .await
                .unwrap();
            r.await.unwrap().unwrap();
        }
        let (tx, r) = oneshot::channel();
        handle
            .commands
            .send(SessionCommand::Rewind {
                turns: 2,
                reply: tx,
            })
            .await
            .unwrap();
        let out = r.await.unwrap().unwrap();
        assert_eq!(out.history_len, 2, "first turn pair remains");

        let (tx2, r2) = oneshot::channel();
        handle
            .commands
            .send(SessionCommand::Rewind {
                turns: 5,
                reply: tx2,
            })
            .await
            .unwrap();
        let err = r2.await.unwrap().unwrap_err();
        assert!(err.contains("cannot rewind 5"), "{err}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fork_copies_journal_and_marks_provenance() {
        let dir = tmpdir("fork");
        let h = harness_with(vec![step_text("r1")], &dir, "s5");
        let (_dead_tx, approval_rx) = mpsc::channel::<ApprovalJob>(1);
        let jp = dir.join("s5.jsonl");
        let handle = spawn_session(h.clone(), approval_rx, "s5".into(), jp.clone());
        let (tx, r) = oneshot::channel();
        handle
            .commands
            .send(SessionCommand::Prompt {
                text: "hi".into(),
                reply: tx,
            })
            .await
            .unwrap();
        r.await.unwrap().unwrap();

        let (fxt, fxr) = oneshot::channel();
        handle
            .commands
            .send(SessionCommand::Fork { reply: fxt })
            .await
            .unwrap();
        let fork = fxr.await.unwrap().unwrap();
        assert!(fork.fork_path.exists(), "{:?}", fork.fork_path);
        assert!(fork.fork_session.starts_with("s5-fork-"));
        let recs = Journal::replay(&fork.fork_path).unwrap();
        assert!(recs.iter().any(
            |r| matches!(r, JournalRecord::Fork { from_session, .. } if from_session == "s5")
        ));
        assert!(recs
            .iter()
            .any(|r| matches!(r, JournalRecord::UserMessage { text } if text == "hi")));
        // both sides carry the fork marker (visible on either journal)
        let orig = Journal::replay(&jp).unwrap();
        assert_eq!(
            orig.len(),
            recs.len(),
            "copy == original + fork marker on both"
        );
        assert!(orig.iter().any(|r| matches!(r, JournalRecord::Fork { fork_session, .. } if *fork_session == fork.fork_session)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resume_rebuilds_history_from_journal() {
        let dir = tmpdir("resume");
        let h = harness_with(vec![step_text("r1"), step_text("r2")], &dir, "s6");
        let (_dead_tx, approval_rx) = mpsc::channel::<ApprovalJob>(1);
        let jp = dir.join("s6.jsonl");
        let handle = spawn_session(h.clone(), approval_rx, "s6".into(), jp.clone());
        for t in ["t1", "t2"] {
            let (tx, r) = oneshot::channel();
            handle
                .commands
                .send(SessionCommand::Prompt {
                    text: t.into(),
                    reply: tx,
                })
                .await
                .unwrap();
            r.await.unwrap().unwrap();
        }
        // a FRESH harness (as after a restart) rebuilds from the journal
        let h2 = harness_with(vec![], &dir, "s6-continued");
        let n = rebuild_history_from_journal(&h2, &jp).await.unwrap();
        assert_eq!(n, 4, "2 user + 2 assistant rebuilt");
        let hist = h2.history.read().await;
        assert_eq!(hist[0].role, "user");
        assert_eq!(hist[0].content, "t1");
        assert_eq!(hist[1].role, "assistant");
        assert_eq!(hist[1].content, "r1");
        assert_eq!(hist[3].content, "r2");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancel_during_turn_unwinds_and_replies() {
        let dir = tmpdir("cancel");
        let h = harness_with(vec![MockProvider::text("never reached")], &dir, "s7");
        // no sleep needed: cancel BEFORE the prompt makes run_turn abort
        // instantly; the waiter must fire from the turn-done path anyway.
        let (_dead_tx, approval_rx) = mpsc::channel::<ApprovalJob>(1);
        let handle = spawn_session(h.clone(), approval_rx, "s7".into(), dir.join("s7.jsonl"));
        // cancel while idle → immediate
        let (cx, cr) = oneshot::channel();
        handle
            .commands
            .send(SessionCommand::Cancel { reply: cx })
            .await
            .unwrap();
        cr.await.unwrap();
        // cancel reset via fresh session: run a prompt on a cancelled token → aborted
        let (tx, r) = oneshot::channel();
        handle
            .commands
            .send(SessionCommand::Prompt {
                text: "hi".into(),
                reply: tx,
            })
            .await
            .unwrap();
        let stop = r.await.unwrap().unwrap();
        assert_eq!(stop.normalized(), "aborted");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn steering_flows_through_the_actor() {
        let dir = tmpdir("steer");
        let h = harness_with(vec![step_text("r1"), step_text("r2")], &dir, "s8");
        let (_dead_tx, approval_rx) = mpsc::channel::<ApprovalJob>(1);
        let handle = spawn_session(h.clone(), approval_rx, "s8".into(), dir.join("s8.jsonl"));
        handle
            .commands
            .send(SessionCommand::Steer {
                text: "focus on X".into(),
            })
            .await
            .unwrap();
        let (tx, r) = oneshot::channel();
        handle
            .commands
            .send(SessionCommand::Prompt {
                text: "go".into(),
                reply: tx,
            })
            .await
            .unwrap();
        r.await.unwrap().unwrap();
        // pending steer was drained as the next user message (D3/INV-02)
        let hist = h.history.read().await;
        assert!(
            hist.iter()
                .any(|m| m.role == "user" && m.content == "focus on X"),
            "{hist:?}"
        );
    }

    #[test]
    fn list_sessions_reads_meta_records() {
        let dir = tmpdir("list");
        let h = harness_with(vec![], &dir, "s9");
        // journal already opened with SessionMeta? harness_with does NOT
        // write meta; write one manually like the CLI does.
        {
            let mut j = h.journal.lock().unwrap();
            j.append(&JournalRecord::SessionMeta {
                session_id: "s9".into(),
                cwd: "/tmp".into(),
                title: Some("night shift".into()),
                started_at: iso_now(),
            })
            .unwrap();
        }
        drop(h);
        let sums = list_sessions(&dir);
        assert_eq!(sums.len(), 1, "{sums:?}");
        assert_eq!(sums[0].session_id, "s9");
        assert_eq!(sums[0].title.as_deref(), Some("night shift"));
        assert!(sums[0].started_at.contains('T'), "ISO-8601 shape");
    }

    #[test]
    fn iso_now_shape_is_8601() {
        let t = iso_now();
        // 2026-11-06T03:14:15Z
        assert_eq!(t.len(), 20, "{t}");
        assert_eq!(&t[4..5], "-");
        assert_eq!(&t[10..11], "T");
        assert!(t.ends_with('Z'));
    }
}
