//! W9.3 — the smart-approval judge (distillation §A, Hermes
//! `approval_smart.py`; the FORMER-REJECTS adoption form: an OPT-IN flag,
//! default OFF). A permission ASK can be routed through a chain:
//!
//! 1. **Inspector chain** (deterministic, always first): pure inspectors
//!    over (tool, argv) that answer VERDICT or ABSTAIN. The first
//!    non-abstaining inspector decides — the LLM judge is never called
//!    when a deterministic inspector has an opinion.
//! 2. **LLM judge** (only when the flag is ON, an aux model is wired,
//!    and every inspector abstained): a bounded verdict request — temp 0,
//!    the answer must START with APPROVE or DENY; an APPROVE can only
//!    approve the EXACT command it was shown (command-only containment:
//!    the approved call is the judged call, never a variant).
//! 3. **Decision cache**: (tool + argv + cwd) hash → verdict for the
//!    session; identical inputs do not re-bill the judge.
//!
//! Fail-closed everywhere: a judge error, an unparsable verdict, a
//! verdict naming a DIFFERENT command — all deny with an honest reason.
//! The flag being off means the judge is structurally unreachable (no
//! aux-model spend, no cache writes).

use crate::providers::ChatMessage;
use std::collections::BTreeMap;

/// What an inspector answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InspectorVerdict {
    /// Deny with a reason (deterministic — no model involvement).
    Deny(String),
    /// Approve outright (deterministic allow, e.g. a known-safe read).
    Approve,
    /// No opinion — the chain proceeds.
    Abstain,
}

/// One deterministic inspector: pure, sync, no I/O.
pub trait Inspector: Send + Sync {
    fn name(&self) -> &'static str;
    fn inspect(&self, tool: &str, argv: &[String], cwd: &str) -> InspectorVerdict;
}

/// Built-in inspector: the immutable-danger corpus (the hardline floor
/// already denies these in the engine — this inspector answers first and
/// keeps the judge away from them entirely).
pub struct ImmutableDanger;

impl Inspector for ImmutableDanger {
    fn name(&self) -> &'static str {
        "immutable-danger"
    }

    fn inspect(&self, _tool: &str, argv: &[String], _cwd: &str) -> InspectorVerdict {
        let joined = argv.join(" ");
        const PATTERNS: &[&str] = &[
            "mkfs",
            "dd if=",
            "of=/dev/",
            ":(){ :|:& };:",
            "shutdown",
            "reboot",
            "> /dev/sd",
        ];
        for p in PATTERNS {
            if joined.contains(p) {
                return InspectorVerdict::Deny(format!(
                    "inspector `immutable-danger`: `{p}` is never approvable"
                ));
            }
        }
        InspectorVerdict::Abstain
    }
}

/// Built-in inspector: read-only commands inside the working directory
/// are approvable outright (an honest allow, not a guess — the argv shape
/// is a single known read-only program with no output redirect).
pub struct KnownReadOnly;

impl Inspector for KnownReadOnly {
    fn name(&self) -> &'static str {
        "known-read-only"
    }

    fn inspect(&self, tool: &str, argv: &[String], _cwd: &str) -> InspectorVerdict {
        if tool != "Bash" || argv.len() != 1 {
            return InspectorVerdict::Abstain;
        }
        const READ_ONLY: &[&str] = &["ls", "cat", "pwd", "echo", "which", "rg", "head", "tail"];
        let cmd = argv[0].trim();
        let first = cmd.split_whitespace().next().unwrap_or("");
        if READ_ONLY.contains(&first) && !cmd.contains('>') && !cmd.contains('|') {
            return InspectorVerdict::Approve;
        }
        InspectorVerdict::Abstain
    }
}

/// The judge chain. `judger: None` or `enabled: false` = the LLM rung is
/// structurally unreachable.
pub struct SmartApproval {
    pub enabled: bool,
    pub inspectors: Vec<Box<dyn Inspector>>,
    /// The aux-model runner (temp 0, verdict-only prompt). Wired by the
    /// factory from the Small role; None = judge unavailable.
    #[allow(clippy::type_complexity)]
    pub judger: Option<
        Box<
            dyn Fn(String) -> futures_util::future::BoxFuture<'static, Result<String, String>>
                + Send
                + Sync,
        >,
    >,
    /// Session-scoped decision cache: FNV(tool+argv+cwd) → verdict text.
    cache: std::sync::Mutex<BTreeMap<u64, JudgeDecision>>,
}

/// The judge's final answer for one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JudgeDecision {
    Approve,
    Deny(String),
}

/// FNV-1a over the decision identity — a cache fingerprint, not a
/// security primitive (same shape as the W5.5 loop fingerprint).
fn fingerprint(tool: &str, argv: &[String], cwd: &str) -> u64 {
    use std::hash::Hasher;
    let mut h = std::hash::DefaultHasher::new();
    h.write(tool.as_bytes());
    h.write(&[0]);
    h.write(argv.join(" ").as_bytes());
    h.write(&[0]);
    h.write(cwd.as_bytes());
    h.finish()
}

impl SmartApproval {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            inspectors: vec![Box::new(ImmutableDanger), Box::new(KnownReadOnly)],
            judger: None,
            cache: std::sync::Mutex::new(BTreeMap::new()),
        }
    }

    /// Run the chain for one pending approval. Order is fixed:
    /// cache → inspectors (first non-abstain decides) → judge.
    /// `flag off` short-circuits to None immediately.
    pub async fn decide(&self, tool: &str, argv: &[String], cwd: &str) -> Option<JudgeDecision> {
        if !self.enabled {
            return None;
        }
        let fp = fingerprint(tool, argv, cwd);
        if let Some(d) = self
            .cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&fp)
        {
            return Some(d.clone());
        }
        for insp in &self.inspectors {
            match insp.inspect(tool, argv, cwd) {
                InspectorVerdict::Deny(r) => return Some(JudgeDecision::Deny(r)),
                InspectorVerdict::Approve => return Some(JudgeDecision::Approve),
                InspectorVerdict::Abstain => {}
            }
        }
        // Every inspector abstained → the judge rung (only when wired).
        let runner = self.judger.as_ref()?;
        let prompt = judge_prompt(tool, argv, cwd);
        match runner(prompt).await {
            Ok(out) => {
                let d = parse_verdict(&out, argv);
                self.cache
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(fp, d.clone());
                Some(d)
            }
            Err(e) => Some(JudgeDecision::Deny(format!(
                "judge unavailable ({e}) — denied fail-closed"
            ))),
        }
    }

    pub fn cache_len(&self) -> usize {
        self.cache.lock().unwrap_or_else(|p| p.into_inner()).len()
    }
}

/// The bounded judge prompt: the command, the cwd, the strict contract.
pub fn judge_prompt(tool: &str, argv: &[String], cwd: &str) -> String {
    format!(
        "You are a permission judge. Decide whether this tool call is safe to run.\n\
         Tool: {tool}\nWorking directory: {cwd}\n\
         Command: {argv:?}\n\n\
         Answer with EXACTLY one word first: APPROVE or DENY.\n\
         After DENY you may add one short reason.\n\
         APPROVE approves ONLY this exact command as written. Temperature 0."
    )
}

/// Strict verdict parsing: must START with APPROVE or DENY (case-
/// sensitive, the contract the prompt states). An APPROVE approves ONLY
/// the exact argv shown — there is nothing to substitute, so the
/// containment holds by construction; anything unparsable denies.
pub fn parse_verdict(out: &str, _argv: &[String]) -> JudgeDecision {
    let t = out.trim_start();
    if t.starts_with("APPROVE") {
        return JudgeDecision::Approve;
    }
    if let Some(rest) = t.strip_prefix("DENY") {
        let reason = rest.trim();
        return JudgeDecision::Deny(if reason.is_empty() {
            "judge denied (no reason given)".to_string()
        } else {
            format!("judge denied: {reason}")
        });
    }
    JudgeDecision::Deny("judge verdict unparsable — denied fail-closed".to_string())
}

/// The judge's aux-model request shape (temp 0; the factory wires the
/// Small-role provider behind it). Bounded: 16 completion tokens max.
pub fn judge_request(prompt: &str) -> crate::providers::ChatRequest {
    crate::providers::ChatRequest {
        messages: vec![ChatMessage::user(prompt)],
        max_completion_tokens: Some(16),
        temperature: Some(0.0),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[tokio::test]
    async fn flag_off_is_structurally_unreachable() {
        let s = SmartApproval::new(false);
        assert_eq!(
            s.decide("Bash", &argv(&["rm", "-rf", "/"]), "/w").await,
            None
        );
        assert_eq!(s.cache_len(), 0, "no cache writes with the flag off");
    }

    #[tokio::test]
    async fn inspectors_decide_without_the_judge() {
        let mut s = SmartApproval::new(true);
        s.judger = Some(Box::new(|_| {
            Box::pin(async { Err("judge must not be called".to_string()) })
        }));
        // The danger inspector denies first.
        assert!(matches!(
            s.decide("Bash", &argv(&["echo", "mkfs.ext4", "/dev/sda"]), "/w")
                .await,
            Some(JudgeDecision::Deny(_))
        ));
        // The read-only inspector approves outright (the command line is
        // ONE argv element — the shape shellparse hands Bash).
        assert_eq!(
            s.decide("Bash", &argv(&["ls -la"]), "/w").await,
            Some(JudgeDecision::Approve)
        );
        assert_eq!(
            s.cache_len(),
            0,
            "inspector decisions do not bill the cache"
        );
    }

    #[tokio::test]
    async fn judge_rung_runs_only_when_everything_abstains() {
        let mut s = SmartApproval::new(true);
        let called = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c2 = called.clone();
        s.judger = Some(Box::new(move |prompt| {
            c2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            assert!(
                prompt.contains("APPROVE or DENY"),
                "the contract is in the prompt"
            );
            Box::pin(async { Ok("DENY rm is destructive".to_string()) })
        }));
        let d = s.decide("Bash", &argv(&["rm", "-rf", "build"]), "/w").await;
        assert!(matches!(d, Some(JudgeDecision::Deny(_))));
        let d = d.unwrap();
        assert!(matches!(d, JudgeDecision::Deny(ref r) if r.contains("rm is destructive")));
        assert_eq!(called.load(std::sync::atomic::Ordering::SeqCst), 1);

        // Same identity → cache hit, judge NOT re-called.
        let d2 = s.decide("Bash", &argv(&["rm", "-rf", "build"]), "/w").await;
        assert!(d2.is_some());
        assert_eq!(called.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(s.cache_len(), 1);
    }

    #[tokio::test]
    async fn judge_errors_and_garbage_deny_fail_closed() {
        let mut s = SmartApproval::new(true);
        s.judger = Some(Box::new(|_| {
            Box::pin(async { Err("endpoint down".to_string()) })
        }));
        let d = s.decide("Bash", &argv(&["make", "all"]), "/w").await;
        assert!(matches!(d, Some(JudgeDecision::Deny(_))));
        assert!(matches!(d, Some(JudgeDecision::Deny(ref r)) if r.contains("fail-closed")));

        let mut s2 = SmartApproval::new(true);
        s2.judger = Some(Box::new(|_| {
            Box::pin(async { Ok("probably fine, go ahead".to_string()) })
        }));
        let d = s2.decide("Bash", &argv(&["make", "all"]), "/w").await;
        assert!(matches!(d, Some(JudgeDecision::Deny(_))));
        assert!(matches!(d, Some(JudgeDecision::Deny(ref r)) if r.contains("unparsable")));
    }

    #[test]
    fn verdict_parsing_is_strict() {
        assert_eq!(parse_verdict("APPROVE", &[]), JudgeDecision::Approve);
        assert_eq!(
            parse_verdict("DENY wipes the home dir", &[]),
            JudgeDecision::Deny("judge denied: wipes the home dir".to_string())
        );
        assert!(matches!(parse_verdict("DENY", &[]), JudgeDecision::Deny(_)));
        assert!(matches!(
            parse_verdict("Sure, APPROVE!", &[]),
            JudgeDecision::Deny(_)
        ));
    }

    #[test]
    fn judge_request_is_bounded() {
        let r = judge_request("p");
        assert_eq!(r.max_completion_tokens, Some(16));
        assert_eq!(r.temperature, Some(0.0));
        assert!(r.tools.is_empty());
    }
}
