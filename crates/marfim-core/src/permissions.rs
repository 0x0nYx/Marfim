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

pub struct PermissionEngine {
    pub mode: Mode,
    pub rules: Vec<Rule>,
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
                queue_tx: tx,
            },
            rx,
        )
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

    /// Check a tool invocation. `argv` is Some for Bash-like tools after
    /// argv-level analysis. Returns verdict; `Ask` results go through the
    /// single approval queue.
    pub async fn check(
        &self,
        tool: &str,
        class: ToolClass,
        argv: Option<&[String]>,
        risk: Option<ShellRisk>,
        summary: &str,
    ) -> Verdict {
        // §A hardline floor (distilled: Hermes approval_floors.py —
        // floor-before-bypass ordering): the corpus denies BEFORE any
        // explicit rule, yolo mode or unattended default. deny>ask>allow
        // starts here.
        if let Some(ShellRisk::Hardline) = risk {
            return Verdict::new(
                Decision::Deny,
                "hardline pattern — denied unconditionally (floor-before-bypass)",
            );
        }

        // 1. explicit rules first (deny > ask > allow)
        if let Some((idx, d)) = self.matching_rule(tool, argv) {
            return match d {
                Decision::Deny => Verdict {
                    decision: Decision::Deny,
                    reason: "explicit deny rule".into(),
                    provenance: None,
                },
                Decision::Ask => {
                    return self.ask(tool, summary).await;
                }
                Decision::Allow => {
                    // compound/nested shell constructs can NEVER ride an
                    // allow rule past ask (INV-08).
                    if tool == "Bash" {
                        if let Some(ShellRisk::NestedExec | ShellRisk::Heredoc) = risk {
                            return self.ask(tool, summary).await;
                        }
                        if let Some(ShellRisk::Sequenced) = risk {
                            return self.ask(tool, summary).await;
                        }
                    }
                    Verdict {
                        decision: Decision::Allow,
                        reason: "explicit allow rule".into(),
                        provenance: Some(format!("allowlist entry rules[{idx}]")),
                    }
                }
            };
        }

        // 2. mode default
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

        // plan mode denies writes; yolo still funnels risky shell via ask
        // above when rules demanded it.
        match default {
            Decision::Allow if tool == "Bash" => {
                if let Some(r) = risk {
                    if matches!(r, ShellRisk::NestedExec | ShellRisk::Heredoc) {
                        return self.ask(tool, summary).await;
                    }
                }
                Verdict {
                    decision: Decision::Allow,
                    reason: format!("mode {:?} default", self.mode),
                    provenance: None,
                }
            }
            Decision::Ask => self.ask(tool, summary).await,
            other => Verdict {
                decision: other,
                reason: format!("mode {:?} default", self.mode),
                provenance: None,
            },
        }
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
}
