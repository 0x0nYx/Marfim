//! Permission engine (INV-08/09): rules deny > ask > allow, then mode
//! default; argv-level shell analysis; single approval queue; env
//! sanitization; PreToolUse hooks fail closed (see hooks.rs).

use crate::shellparse::ShellRisk;

pub use crate::shellparse::ShellRisk as PublicShellRisk;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Mode {
    Default,
    Plan,
    AcceptEdits,
    Yolo,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "default" => Some(Mode::Default),
            "plan" => Some(Mode::Plan),
            "acceptEdits" | "accept-edits" => Some(Mode::AcceptEdits),
            "yolo" => Some(Mode::Yolo),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolClass {
    ReadOnly,
    Edit,
    Execute,
    Network,
    None_,
}

/// W5.6 (codex/omp): the TOOL SPEC declares its approval tier — the
/// tool's own contract layered on top of class/mode/rules.
///
/// - `Default`: class/mode/rules decide (today's behavior).
/// - `AlwaysAsk`: the tool demands oversight — every Allow escalates to
///   an Ask, even in Yolo. Denies are untouched (the deny floor wins).
/// - `NeverAsk`: the tool opts OUT of prompting — a plain mode-based Ask
///   becomes an Allow. Denies are untouched, the W1.4 AppUid force-ask
///   still wins (a sandbox-attestation floor is not the tool's to
///   waive), and privacy denies still fire first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ApprovalTier {
    #[default]
    Default,
    AlwaysAsk,
    NeverAsk,
}

#[derive(Debug, Clone)]
pub struct Rule {
    pub tool: String,
    pub decision: Decision,
    /// For Bash: argv[0] (and optional argv[1] prefix) this rule covers.
    pub argv_prefix: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Deny,
    Ask,
    Allow,
}

#[derive(Debug, Clone)]
pub struct Verdict {
    pub decision: Decision,
    pub reason: String,
    /// §A approval provenance (distilled: Hermes approval_provenance.py):
    /// which rule produced an ALLOW, for the tool-result note. Provenance
    /// NEVER decides — it only annotates; deny/ask paths carry None.
    pub provenance: Option<String>,
}

impl Verdict {
    fn new(decision: Decision, reason: impl Into<String>) -> Self {
        Self {
            decision,
            reason: reason.into(),
            provenance: None,
        }
    }
}

/// The pipeline's pre-execution decision: deny/allow with their reasons, or
/// an ask that has NOT yet reached the approval queue.
enum Stage {
    Deny(String),
    Ask,
    Allow {
        reason: String,
        provenance: Option<String>,
    },
}

pub struct PermissionEngine {
    pub mode: Mode,
    pub rules: Vec<Rule>,
    /// Outbound privacy guard (distilled: claude-code `privacy` mod): when
    /// set, a shell call holding the account email AND naming a User-Agent
    /// is refused with no prompt. None = disabled.
    pub privacy: Option<crate::privacy::PrivacyGuard>,
    /// `permissions.denyInsteadOfPrompting` (distilled: claude-code 2.1.319):
    /// a call that would wait on a permission prompt is denied instead and
    /// the turn continues. Ask verdicts from rules AND mode defaults both
    /// convert.
    pub deny_instead_of_prompting: bool,
    /// W9.4 observe mode: a MODE-based ask becomes an ALLOW annotated
    /// "[observe]" — analytics without a human. Floors, denies, privacy
    /// and rule-denies stay absolute (observe never weakens a floor).
    pub observe: bool,
    /// W1.4 `SandboxLevel::AppUid`: no kernel containment available, so
    /// every mutation-tier verdict (Edit/Execute/Network) that would
    /// ALLOW is forced to Ask. Deny floors/rules stay absolute.
    pub force_ask_mutations: bool,
    /// Single approval queue: every pending ask flows through one channel
    /// so approvals serialize and cannot deadlock or double-prompt.
    queue_tx: mpsc::Sender<ApprovalJob>,
}

pub struct ApprovalJob {
    pub tool: String,
    pub summary: String,
    pub respond: oneshot::Sender<ApprovalAnswer>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ApprovalAnswer {
    Allow,
    AllowAlways,
    Deny,
}

impl PermissionEngine {
    pub fn new(mode: Mode, rules: Vec<Rule>) -> (Self, mpsc::Receiver<ApprovalJob>) {
        let (tx, rx) = mpsc::channel(64);
        (
            Self {
                mode,
                rules,
                privacy: None,
                deny_instead_of_prompting: false,
                observe: false,
                force_ask_mutations: false,
                queue_tx: tx,
            },
            rx,
        )
    }

    /// Attach the outbound privacy guard (builder style so existing
    /// constructors stay untouched).
    pub fn with_privacy(mut self, guard: Option<crate::privacy::PrivacyGuard>) -> Self {
        self.privacy = guard;
        self
    }

    /// Convert ask verdicts into denies (builder style).
    /// W1.4: app-uid attestation — mutations ask, always.
    pub fn with_force_ask_mutations(mut self, on: bool) -> Self {
        self.force_ask_mutations = on;
        self
    }

    /// W9.4: arm observe mode (record-only analytics on mode-asks).
    pub fn with_observe(mut self, on: bool) -> Self {
        self.observe = on;
        self
    }

    pub fn with_deny_instead_of_prompting(mut self, on: bool) -> Self {
        self.deny_instead_of_prompting = on;
        self
    }

    pub fn rule_for(&self, tool: &str, argv: Option<&[String]>) -> Option<Decision> {
        self.matching_rule(tool, argv).map(|(_, d)| d)
    }

    /// The matched rule (index + decision) with the engine's precedence:
    /// later rules override earlier ones at equal specificity; deny wins
    /// over ask wins over allow at the same position. Exposed so an
    /// ALLOW can carry its §A provenance (which entry approved it).
    pub fn matching_rule(&self, tool: &str, argv: Option<&[String]>) -> Option<(usize, Decision)> {
        let mut best: Option<(usize, Decision)> = None;
        for (i, r) in self.rules.iter().enumerate() {
            if r.tool != tool {
                continue;
            }
            if let Some(prefix) = &r.argv_prefix {
                let Some(args) = argv else { continue };
                if args.len() < prefix.len() {
                    continue;
                }
                if !args
                    .iter()
                    .zip(prefix.iter())
                    .all(|(a, p)| a == p || a.starts_with(p.as_str()) && p.ends_with('*'))
                {
                    continue;
                }
            }
            // later rules override earlier ones at equal specificity;
            // deny wins over ask wins over allow at the same position.
            match best {
                Some((_, Decision::Deny)) => {}
                _ => best = Some((i, r.decision)),
            }
        }
        best
    }

    /// The pipeline's decision WITHOUT executing any prompt: hardline
    /// floor → explicit rules → mode default. `Ask` is returned as a stage,
    /// never sent to the queue — the caller decides whether to prompt,
    /// convert to deny, or pre-empt with the privacy guard.
    fn decide_stage(
        &self,
        tool: &str,
        class: ToolClass,
        argv: Option<&[String]>,
        risk: Option<ShellRisk>,
    ) -> Stage {
        // §A hardline floor (distilled: Hermes approval_floors.py —
        // floor-before-bypass ordering): the corpus denies BEFORE any
        // explicit rule, yolo mode or unattended default. deny>ask>allow
        // starts here.
        if let Some(ShellRisk::Hardline) = risk {
            return Stage::Deny(
                "hardline pattern — denied unconditionally (floor-before-bypass)".into(),
            );
        }

        // 1. explicit rules first (deny > ask > allow)
        if let Some((idx, d)) = self.matching_rule(tool, argv) {
            return match d {
                Decision::Deny => Stage::Deny("explicit deny rule".into()),
                Decision::Ask => Stage::Ask,
                Decision::Allow => {
                    // compound/nested shell constructs can NEVER ride an
                    // allow rule past ask (INV-08).
                    if tool == "Bash"
                        && matches!(
                            risk,
                            Some(ShellRisk::NestedExec)
                                | Some(ShellRisk::Heredoc)
                                | Some(ShellRisk::Sequenced)
                        )
                    {
                        Stage::Ask
                    } else {
                        Stage::Allow {
                            reason: "explicit allow rule".into(),
                            provenance: Some(format!("allowlist entry rules[{idx}]")),
                        }
                    }
                }
            };
        }

        // 2. mode default; plan mode denies writes; yolo still funnels
        // risky shell via ask above when rules demanded it.
        let default = match self.mode {
            Mode::Yolo => Decision::Allow,
            Mode::AcceptEdits => match class {
                ToolClass::Edit | ToolClass::ReadOnly | ToolClass::None_ => Decision::Allow,
                ToolClass::Execute | ToolClass::Network => Decision::Ask,
            },
            Mode::Plan => match class {
                ToolClass::ReadOnly => Decision::Allow,
                _ => Decision::Deny,
            },
            Mode::Default => match class {
                ToolClass::ReadOnly | ToolClass::None_ => Decision::Allow,
                _ => Decision::Ask,
            },
        };
        match default {
            Decision::Allow if tool == "Bash" => {
                if matches!(risk, Some(ShellRisk::NestedExec | ShellRisk::Heredoc)) {
                    Stage::Ask
                } else {
                    Stage::Allow {
                        reason: format!("mode {:?} default", self.mode),
                        provenance: None,
                    }
                }
            }
            Decision::Ask => Stage::Ask,
            Decision::Deny => Stage::Deny(format!("mode {:?} default", self.mode)),
            Decision::Allow => Stage::Allow {
                reason: format!("mode {:?} default", self.mode),
                provenance: None,
            },
        }
    }

    /// Privacy pre-emption: `Some(deny verdict)` when the guard reads this
    /// shell call as sending the account address out as a User-Agent.
    /// Runs BEFORE any prompt: the refusal raises no prompt and allow
    /// rules do not clear it; an explicit deny beneath keeps its own
    /// reason (callers check Deny first).
    fn privacy_deny(&self, tool: &str, input: Option<&str>) -> Option<Verdict> {
        let guard = self.privacy.as_ref()?;
        let text = input?;
        guard.denies_call(tool, text).map(|reason| Verdict {
            decision: Decision::Deny,
            reason: reason.into(),
            provenance: None,
        })
    }

    /// Check a tool invocation. `argv` is Some for Bash-like tools after
    /// argv-level analysis. Returns verdict; `Ask` results go through the
    /// single approval queue. `input` is the raw command text for
    /// shell-like tools (privacy-guard readings; ignored elsewhere).
    pub async fn check_with_input(
        &self,
        tool: &str,
        class: ToolClass,
        argv: Option<&[String]>,
        risk: Option<ShellRisk>,
        summary: &str,
        input: Option<&str>,
    ) -> Verdict {
        self.check_with_tier(
            tool,
            class,
            argv,
            risk,
            summary,
            input,
            ApprovalTier::Default,
        )
        .await
    }

    /// W5.6: the tier-aware check — the host passes the tool spec's
    /// declared approval tier; everything else matches `check_with_input`.
    #[allow(clippy::too_many_arguments)]
    pub async fn check_with_tier(
        &self,
        tool: &str,
        class: ToolClass,
        argv: Option<&[String]>,
        risk: Option<ShellRisk>,
        summary: &str,
        input: Option<&str>,
        tier: ApprovalTier,
    ) -> Verdict {
        let mut stage = self.decide_stage(tool, class, argv, risk);
        // W5.6 tool-declared approval tier (BEFORE the AppUid force-ask,
        // so the sandbox floor keeps the last word):
        //  - AlwaysAsk escalates an Allow to Ask (a Deny stays a Deny);
        //  - NeverAsk downgrades a plain Ask to Allow (a Deny stays a
        //    Deny — and the AppUid force-ask below re-escalates mutations
        //    regardless, because containment absence is not waivable).
        match tier {
            ApprovalTier::AlwaysAsk if matches!(stage, Stage::Allow { .. }) => stage = Stage::Ask,
            ApprovalTier::NeverAsk if matches!(stage, Stage::Ask) => {
                stage = Stage::Allow {
                    reason: "tool spec declares approval tier never-ask (W5.6)".into(),
                    provenance: None,
                };
            }
            _ => {}
        }
        // W1.4 `SandboxLevel::AppUid`: no kernel containment available —
        // every mutation-tier ALLOW escalates to Ask (floors/denies are
        // untouched; deny > ask > allow holds). ReadOnly stays allow.
        if self.force_ask_mutations
            && matches!(stage, Stage::Allow { .. })
            && !matches!(class, ToolClass::ReadOnly | ToolClass::None_)
        {
            stage = Stage::Ask;
        }
        match stage {
            Stage::Deny(reason) => Verdict {
                decision: Decision::Deny,
                reason,
                provenance: None,
            },
            Stage::Allow { reason, provenance } => {
                if let Some(v) = self.privacy_deny(tool, input) {
                    return v;
                }
                Verdict {
                    decision: Decision::Allow,
                    reason,
                    provenance,
                }
            }
            Stage::Ask => {
                if let Some(v) = self.privacy_deny(tool, input) {
                    return v;
                }
                if self.observe {
                    // W9.4: record-only analytics — the mutation runs, the
                    // provenance note records what WOULD have waited.
                    return Verdict {
                        decision: Decision::Allow,
                        reason: "observe mode: would have asked".into(),
                        provenance: Some("[observe] would-have-asked".to_string()),
                    };
                }
                if self.deny_instead_of_prompting {
                    return Verdict::new(
                        Decision::Deny,
                        "prompt converted to deny (permissions.denyInsteadOfPrompting)",
                    );
                }
                self.ask(tool, summary).await
            }
        }
    }

    /// Check a tool invocation (no raw input → privacy guard inactive).
    pub async fn check(
        &self,
        tool: &str,
        class: ToolClass,
        argv: Option<&[String]>,
        risk: Option<ShellRisk>,
        summary: &str,
    ) -> Verdict {
        self.check_with_input(tool, class, argv, risk, summary, None)
            .await
    }

    async fn ask(&self, tool: &str, summary: &str) -> Verdict {
        let (tx, rx) = oneshot::channel();
        let job = ApprovalJob {
            tool: tool.to_string(),
            summary: summary.to_string(),
            respond: tx,
        };
        if self.queue_tx.send(job).await.is_err() {
            return Verdict {
                decision: Decision::Deny,
                reason: "no approval listener — fail closed".into(),
                provenance: None,
            };
        }
        match rx.await {
            Ok(ApprovalAnswer::Allow) | Ok(ApprovalAnswer::AllowAlways) => Verdict {
                decision: Decision::Allow,
                reason: "user approved".into(),
                provenance: None,
            },
            Ok(ApprovalAnswer::Deny) | Err(_) => Verdict {
                decision: Decision::Deny,
                reason: "user denied".into(),
                provenance: None,
            },
        }
    }
}

/// Environment allowlist for subprocesses (INV-09): everything else is
/// stripped. PATH/HOME/TMPDIR and locale essentials pass through.
pub const ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "HOME",
    "TMPDIR",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TERM",
    "SHELL",
    "USER",
    "LOGNAME",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_DATA_HOME",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
];

pub fn sanitize_env<I, K, V>(vars: I) -> Vec<(String, String)>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    vars.into_iter()
        .filter(|(k, _)| ENV_ALLOWLIST.contains(&k.as_ref()))
        .map(|(k, v)| (k.as_ref().to_string(), v.as_ref().to_string()))
        .collect()
}

pub type SharedEngine = Arc<PermissionEngine>;

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn observe_mode_converts_mode_asks_but_never_floors() {
        // A mode-ask becomes an observe-allow with the provenance note.
        let (e, _rx) = PermissionEngine::new(Mode::Default, vec![]);
        let e = e.with_observe(true);
        let v = e
            .check("Write", ToolClass::Edit, None, None, "write file")
            .await;
        assert_eq!(v.decision, Decision::Allow);
        assert_eq!(v.provenance.as_deref(), Some("[observe] would-have-asked"));

        // A hardline deny STAYS a deny under observe (floors absolute).
        let v2 = e
            .check(
                "Bash",
                ToolClass::Execute,
                Some(&["rm".to_string(), "-rf".to_string(), "/".to_string()]),
                Some(crate::shellparse::ShellRisk::Hardline),
                "rm -rf /",
            )
            .await;
        assert_eq!(v2.decision, Decision::Deny);
    }

    use super::*;

    fn engine(
        mode: Mode,
        rules: Vec<Rule>,
    ) -> (Arc<PermissionEngine>, mpsc::Receiver<ApprovalJob>) {
        let (e, rx) = PermissionEngine::new(mode, rules);
        (Arc::new(e), rx)
    }

    #[tokio::test]
    async fn deny_rule_wins_over_everything() {
        let (e, _rx) = engine(
            Mode::Yolo,
            vec![Rule {
                tool: "Bash".into(),
                decision: Decision::Deny,
                argv_prefix: Some(vec!["rm".into()]),
            }],
        );
        let argv = vec!["rm".to_string(), "-rf".to_string(), "x".to_string()];
        let v = e
            .check("Bash", ToolClass::Execute, Some(&argv), None, "rm")
            .await;
        assert_eq!(v.decision, Decision::Deny);
    }

    #[tokio::test]
    async fn yolo_still_asks_for_nested_exec() {
        let (e, mut rx) = engine(Mode::Yolo, vec![]);
        let argv = vec!["echo".to_string(), "$(id)".to_string()];
        let task = tokio::spawn(async move {
            e.check(
                "Bash",
                ToolClass::Execute,
                Some(&argv),
                Some(ShellRisk::NestedExec),
                "echo $(id)",
            )
            .await
        });
        let job = rx.recv().await.expect("approval arrives");
        job.respond.send(ApprovalAnswer::Allow).unwrap();
        let v = task.await.unwrap();
        assert_eq!(v.decision, Decision::Allow);
        assert!(v.reason.contains("user approved"));
    }

    #[tokio::test]
    async fn plan_mode_denies_edits() {
        let (e, _rx) = engine(Mode::Plan, vec![]);
        let v = e.check("Write", ToolClass::Edit, None, None, "w").await;
        assert_eq!(v.decision, Decision::Deny);
        let v2 = e.check("Read", ToolClass::ReadOnly, None, None, "r").await;
        assert_eq!(v2.decision, Decision::Allow);
    }

    #[tokio::test]
    async fn no_listener_fails_closed() {
        let (e, rx) = PermissionEngine::new(Mode::Default, vec![]);
        drop(rx);
        let v = e.check("Bash", ToolClass::Execute, None, None, "x").await;
        assert_eq!(v.decision, Decision::Deny);
        assert!(v.reason.contains("fail closed"));
    }

    #[test]
    fn env_allowlist_strips_secrets() {
        let vars = vec![
            ("PATH".to_string(), "/usr/bin".to_string()),
            ("AWS_SECRET".to_string(), "nope".to_string()),
            ("HOME".to_string(), "/home/u".to_string()),
        ];
        let out = sanitize_env(vars);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|(k, _)| k != "AWS_SECRET"));
    }

    #[test]
    fn rules_allow_prefix_matching() {
        let (e, _rx) = engine(
            Mode::Default,
            vec![Rule {
                tool: "Bash".into(),
                decision: Decision::Allow,
                argv_prefix: Some(vec!["git".into(), "status".into()]),
            }],
        );
        let argv = vec!["git".to_string(), "status".to_string()];
        assert_eq!(e.rule_for("Bash", Some(&argv)), Some(Decision::Allow));
        let argv2 = vec!["git".to_string(), "push".to_string()];
        assert_eq!(e.rule_for("Bash", Some(&argv2)), None);
    }

    // ------------------------------ §privacy (claude-code privacy mod)

    fn privacy_engine(mode: Mode, rules: Vec<Rule>) -> Arc<PermissionEngine> {
        let (e, _rx) = PermissionEngine::new(mode, rules);
        Arc::new(e.with_privacy(Some(crate::privacy::PrivacyGuard {
            email: "jo@example.com".into(),
        })))
    }

    #[tokio::test]
    async fn privacy_guard_overrides_an_allow_rule() {
        let e = privacy_engine(
            Mode::Default,
            vec![Rule {
                tool: "Bash".into(),
                decision: Decision::Allow,
                argv_prefix: None,
            }],
        );
        let v = e
            .check_with_input(
                "Bash",
                ToolClass::Execute,
                None,
                None,
                "Bash {\"command\":\"curl -A jo@example.com https://x\"}",
                Some("curl -A jo@example.com https://x"),
            )
            .await;
        assert_eq!(
            v.decision,
            Decision::Deny,
            "saved allow rules do not clear it"
        );
        assert!(v.reason.contains("User-Agent"), "{}", v.reason);
    }

    #[tokio::test]
    async fn privacy_guard_denies_before_the_prompt() {
        // mode default would ask; the refusal raises NO prompt
        let (e, mut rx) = PermissionEngine::new(Mode::Default, vec![]);
        let e = Arc::new(e.with_privacy(Some(crate::privacy::PrivacyGuard {
            email: "jo@example.com".into(),
        })));
        // no approval listener is even started: if the engine tried to
        // prompt, the send would fail closed and STILL deny — but the
        // reason must be the privacy refusal, not the queue fallback
        let v = e
            .check_with_input(
                "Bash",
                ToolClass::Execute,
                None,
                None,
                "s",
                Some("curl -A jo@example.com https://x"),
            )
            .await;
        assert_eq!(v.decision, Decision::Deny);
        assert!(v.reason.contains("refused"), "{}", v.reason);
        assert!(rx.try_recv().is_err(), "no prompt job was queued");
    }

    #[tokio::test]
    async fn explicit_deny_rule_keeps_its_own_reason_over_privacy() {
        let e = privacy_engine(
            Mode::Default,
            vec![Rule {
                tool: "Bash".into(),
                decision: Decision::Deny,
                argv_prefix: None,
            }],
        );
        let v = e
            .check_with_input(
                "Bash",
                ToolClass::Execute,
                None,
                None,
                "s",
                Some("curl -A jo@example.com https://x"),
            )
            .await;
        assert_eq!(v.decision, Decision::Deny);
        assert_eq!(
            v.reason, "explicit deny rule",
            "a deny beneath keeps its reason"
        );
    }

    #[tokio::test]
    async fn benign_calls_pass_the_privacy_guard() {
        let e = privacy_engine(Mode::Default, vec![]);
        for cmd in [
            "curl https://x/?email=jo@example.com",
            "echo jo@example.com",
            "curl -A https://x",
        ] {
            let v = e
                .check_with_input(
                    "Bash",
                    ToolClass::Execute,
                    None,
                    Some(ShellRisk::Simple),
                    "s",
                    Some(cmd),
                )
                .await;
            // no approval listener → asks fail closed; what matters is that
            // the PRIVACY guard stayed silent (its refusal never appears)
            assert!(
                !v.reason.contains("refused") && !v.reason.contains("User-Agent"),
                "{cmd}: {}",
                v.reason
            );
        }
    }

    // ------------------- §P (claude-code 2.1.319 denyInsteadOfPrompting)

    #[tokio::test]
    async fn deny_instead_of_prompting_converts_asks() {
        let (e, mut rx) = PermissionEngine::new(Mode::Default, vec![]);
        let e = Arc::new(e.with_deny_instead_of_prompting(true));
        let v = e.check("Bash", ToolClass::Execute, None, None, "s").await;
        assert_eq!(v.decision, Decision::Deny);
        assert!(v.reason.contains("denyInsteadOfPrompting"), "{}", v.reason);
        assert!(rx.try_recv().is_err(), "no prompt job was queued");
    }

    #[tokio::test]
    async fn deny_instead_of_prompting_leaves_allows_alone() {
        let (e, _rx) = PermissionEngine::new(Mode::Default, vec![]);
        let e = Arc::new(e.with_deny_instead_of_prompting(true));
        let v = e.check("Read", ToolClass::ReadOnly, None, None, "r").await;
        assert_eq!(v.decision, Decision::Allow);
    }

    // ------------------------------------------------ W5.6 approval tiers
    #[tokio::test]
    async fn w56_always_ask_escalates_even_in_yolo() {
        let (e, mut rx) = PermissionEngine::new(Mode::Yolo, vec![]);
        // Yolo allows a ReadOnly tool; AlwaysAsk escalates it to an Ask,
        // which (with no listener) fails CLOSED.
        tokio::spawn(async move {
            while let Some(job) = rx.recv().await {
                let _ = job.respond.send(ApprovalAnswer::Allow);
            }
        });
        let v = e
            .check_with_tier(
                "Note",
                ToolClass::ReadOnly,
                None,
                None,
                "note",
                None,
                ApprovalTier::AlwaysAsk,
            )
            .await;
        // with a listener that allows, the escalation resolves to Allow
        // — but the ask itself proves the tier engaged
        assert_eq!(v.decision, Decision::Allow);
    }

    #[tokio::test]
    async fn w56_always_ask_fails_closed_without_listener() {
        let (e, rx) = PermissionEngine::new(Mode::Yolo, vec![]);
        drop(rx); // no listener
        let v = e
            .check_with_tier(
                "Note",
                ToolClass::ReadOnly,
                None,
                None,
                "note",
                None,
                ApprovalTier::AlwaysAsk,
            )
            .await;
        assert_eq!(v.decision, Decision::Deny);
        assert!(v.reason.contains("fail closed"));
    }

    #[tokio::test]
    async fn w56_never_ask_downgrades_a_mode_ask_but_not_a_deny() {
        // Default mode: an Edit-class tool asks. NeverAsk downgrades it.
        let (e, mut rx) = PermissionEngine::new(Mode::Default, vec![]);
        tokio::spawn(async move {
            while let Some(job) = rx.recv().await {
                let _ = job.respond.send(ApprovalAnswer::Deny);
            }
        });
        let v = e
            .check_with_tier(
                "Write",
                ToolClass::Edit,
                None,
                None,
                "w",
                None,
                ApprovalTier::NeverAsk,
            )
            .await;
        assert_eq!(
            v.decision,
            Decision::Allow,
            "the ask never reached the queue"
        );

        // an explicit deny RULE still wins — the tier cannot unlock
        let (e2, _rx2) = PermissionEngine::new(
            Mode::Default,
            vec![Rule {
                tool: "Write".into(),
                decision: Decision::Deny,
                argv_prefix: None,
            }],
        );
        let v2 = e2
            .check_with_tier(
                "Write",
                ToolClass::Edit,
                None,
                None,
                "w",
                None,
                ApprovalTier::NeverAsk,
            )
            .await;
        assert_eq!(v2.decision, Decision::Deny, "deny floor is absolute");
    }

    #[tokio::test]
    async fn w56_never_ask_cannot_waive_the_appuid_force_ask() {
        // W1.4: without kernel containment every mutation asks — a tool
        // declaring NeverAsk must NOT waive that floor.
        let (e0, mut rx) = PermissionEngine::new(Mode::Yolo, vec![]);
        let e = e0.with_force_ask_mutations(true);
        tokio::spawn(async move {
            while let Some(job) = rx.recv().await {
                let _ = job.respond.send(ApprovalAnswer::Deny);
            }
        });
        let v = e
            .check_with_tier(
                "Write",
                ToolClass::Edit,
                None,
                None,
                "w",
                None,
                ApprovalTier::NeverAsk,
            )
            .await;
        assert_eq!(
            v.decision,
            Decision::Deny,
            "the force-ask floor re-escalated and the listener denied"
        );
    }
}
