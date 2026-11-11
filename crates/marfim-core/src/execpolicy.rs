//! W9.1 — the exec policy (distillation §P, codex `execpolicy` doctrine):
//! allow / prompt / forbidden PREFIX rules over child argv, with
//! amendment rules and SELF-TESTS — a policy that cannot prove its own
//! semantics refuses to load (fail-closed).
//!
//! Layers:
//! 1. The rule TABLE (always compiled): longest matching prefix wins;
//!    at equal length the fail-closed order is forbidden > prompt > allow.
//!    No match = `None` (the caller's floor — the PermissionEngine —
//!    decides, as always).
//! 2. Amendments: a policy document may add/remove rules at load time,
//!    applied in declaration order (deterministic).
//! 3. Self-tests: the document pins argv → decision expectations that
//!    MUST hold after amendments; a single mismatch refuses the whole
//!    policy.
//!
//! The feature-`starlark` predicate layer (the FORMER-REJECTS adoption
//! form for arbitrary-logic policies) is deliberately NOT landed in this
//! wave: the repo doctrine requires every third-party API to be verified
//! from its vendored source before use, and the Starlark interpreter's
//! API has not been. The table above is the load-bearing surface; the
//! evaluator is queued with that reason recorded honestly.

use crate::config::Config;

/// One decision a policy may answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Prompt,
    Forbidden,
}

impl Verdict {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "allow" => Ok(Verdict::Allow),
            "prompt" => Ok(Verdict::Prompt),
            "forbidden" => Ok(Verdict::Forbidden),
            other => Err(format!(
                "execpolicy: unknown decision `{other}` (known: allow, prompt, forbidden)"
            )),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Verdict::Allow => "allow",
            Verdict::Prompt => "prompt",
            Verdict::Forbidden => "forbidden",
        }
    }
}

/// One prefix rule: argv must START with exactly this sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecRule {
    pub prefix: Vec<String>,
    pub verdict: Verdict,
}

/// An amendment applied at load time, in declaration order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Amendment {
    Add(ExecRule),
    /// Remove every rule with exactly this prefix.
    Remove(Vec<String>),
}

/// The loaded policy: rules + the (already-applied) amendments and the
/// (already-passed) self-tests. `evaluate` is pure and deterministic.
#[derive(Debug, Clone, Default)]
pub struct ExecPolicy {
    pub rules: Vec<ExecRule>,
}

impl ExecPolicy {
    /// Longest matching prefix wins; at equal length the fail-closed
    /// order is Forbidden > Prompt > Allow; no match = None (the caller's
    /// floor decides).
    pub fn evaluate(&self, argv: &[String]) -> Option<Verdict> {
        let mut best: Option<(usize, Verdict)> = None;
        for rule in &self.rules {
            if rule.prefix.is_empty() || argv.len() < rule.prefix.len() {
                continue;
            }
            if argv[..rule.prefix.len()] != rule.prefix[..] {
                continue;
            }
            let len = rule.prefix.len();
            match best {
                Some((bl, _bv)) if bl > len => {}
                Some((bl, bv)) if bl == len => {
                    // fail-closed tie order
                    let rank = |v: Verdict| match v {
                        Verdict::Forbidden => 2,
                        Verdict::Prompt => 1,
                        Verdict::Allow => 0,
                    };
                    if rank(rule.verdict) > rank(bv) {
                        best = Some((len, rule.verdict));
                    }
                }
                _ => best = Some((len, rule.verdict)),
            }
        }
        best.map(|(_, v)| v)
    }

    /// Apply amendments in order (deterministic; Remove clears every rule
    /// with exactly the given prefix).
    pub fn amend(&mut self, amendments: &[Amendment]) {
        for a in amendments {
            match a {
                Amendment::Add(r) => self.rules.push(r.clone()),
                Amendment::Remove(p) => self.rules.retain(|r| r.prefix != *p),
            }
        }
    }

    /// Run the self-tests against this policy (post-amendment). Every
    /// expectation must hold — one mismatch refuses the policy.
    pub fn self_test(&self, tests: &[(Vec<String>, Verdict)]) -> Result<(), String> {
        for (i, (argv, expect)) in tests.iter().enumerate() {
            match (self.evaluate(argv), expect) {
                (Some(v), e) if v == *e => {}
                (got, e) => {
                    return Err(format!(
                        "execpolicy self-test #{i}: argv {argv:?} → {} (policy answers {})",
                        e.as_str(),
                        got.map(|v| v.as_str()).unwrap_or("none"),
                    ))
                }
            }
        }
        Ok(())
    }

    /// Load from the config `execpolicy` section (registered in the
    /// strict config gates). Shape:
    /// ```toml
    /// [[execpolicy.rule]]
    /// prefix = ["git", "push"]
    /// decision = "prompt"
    ///
    /// [[execpolicy.amend]]
    /// op = "add"          # or "remove"
    /// prefix = ["cargo"]
    /// decision = "allow"  # ignored for remove
    ///
    /// [[execpolicy.self_test]]
    /// argv = ["git", "push", "--force"]
    /// expect = "prompt"
    /// ```
    /// Self-tests run AFTER amendments and MUST pass.
    pub fn from_config(config: &Config) -> Result<Option<ExecPolicy>, String> {
        let Some(v) = config.get("execpolicy") else {
            return Ok(None);
        };
        let obj = v
            .as_object()
            .ok_or_else(|| "execpolicy section must be an object".to_string())?;
        let mut policy = ExecPolicy::default();
        if let Some(rules) = obj.get("rule") {
            for r in rules
                .as_array()
                .ok_or_else(|| "execpolicy.rule must be an array".to_string())?
            {
                policy.rules.push(parse_rule(r)?);
            }
        }
        let mut amendments: Vec<Amendment> = Vec::new();
        if let Some(am) = obj.get("amend") {
            for a in am
                .as_array()
                .ok_or_else(|| "execpolicy.amend must be an array".to_string())?
            {
                let op = a["op"]
                    .as_str()
                    .ok_or("execpolicy.amend entry needs `op`")?;
                let prefix = parse_prefix(a)?;
                amendments.push(match op {
                    "add" => Amendment::Add(ExecRule {
                        prefix,
                        verdict: Verdict::parse(
                            a["decision"]
                                .as_str()
                                .ok_or("execpolicy.amend add needs `decision`")?,
                        )?,
                    }),
                    "remove" => Amendment::Remove(prefix),
                    other => {
                        return Err(format!(
                            "execpolicy.amend: unknown op `{other}` (known: add, remove)"
                        ))
                    }
                });
            }
        }
        policy.amend(&amendments);
        let mut tests: Vec<(Vec<String>, Verdict)> = Vec::new();
        if let Some(st) = obj.get("self_test") {
            for t in st
                .as_array()
                .ok_or_else(|| "execpolicy.self_test must be an array".to_string())?
            {
                tests.push((
                    parse_argv(t)?,
                    Verdict::parse(
                        t["expect"]
                            .as_str()
                            .ok_or("execpolicy.self_test entry needs `expect`")?,
                    )?,
                ));
            }
        }
        policy.self_test(&tests)?;
        Ok(Some(policy))
    }
}

fn parse_rule(r: &serde_json::Value) -> Result<ExecRule, String> {
    Ok(ExecRule {
        prefix: parse_prefix(r)?,
        verdict: Verdict::parse(
            r["decision"]
                .as_str()
                .ok_or("execpolicy.rule needs `decision`")?,
        )?,
    })
}

fn parse_prefix(v: &serde_json::Value) -> Result<Vec<String>, String> {
    parse_str_list(v, "prefix")
}

fn parse_argv(v: &serde_json::Value) -> Result<Vec<String>, String> {
    parse_str_list(v, "argv")
}

fn parse_str_list(v: &serde_json::Value, key: &str) -> Result<Vec<String>, String> {
    v[key]
        .as_array()
        .ok_or_else(|| format!("execpolicy entry needs `{key}` (array of strings)"))?
        .iter()
        .map(|x| {
            x.as_str()
                .map(String::from)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| format!("execpolicy.{key} must hold non-empty strings"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn policy() -> ExecPolicy {
        ExecPolicy {
            rules: vec![
                ExecRule {
                    prefix: argv(&["git"]),
                    verdict: Verdict::Allow,
                },
                ExecRule {
                    prefix: argv(&["git", "push"]),
                    verdict: Verdict::Prompt,
                },
                ExecRule {
                    prefix: argv(&["git", "push", "--force"]),
                    verdict: Verdict::Forbidden,
                },
                ExecRule {
                    prefix: argv(&["cargo", "test"]),
                    verdict: Verdict::Allow,
                },
            ],
        }
    }

    #[test]
    fn longest_prefix_wins() {
        let p = policy();
        assert_eq!(p.evaluate(&argv(&["git", "status"])), Some(Verdict::Allow));
        assert_eq!(p.evaluate(&argv(&["git", "push"])), Some(Verdict::Prompt));
        assert_eq!(
            p.evaluate(&argv(&["git", "push", "--force", "origin"])),
            Some(Verdict::Forbidden)
        );
        assert_eq!(
            p.evaluate(&argv(&["cargo", "test", "-q"])),
            Some(Verdict::Allow)
        );
        assert_eq!(
            p.evaluate(&argv(&["rm", "-rf", "/"])),
            None,
            "no match = no opinion"
        );
    }

    #[test]
    fn tie_goes_fail_closed() {
        let p = ExecPolicy {
            rules: vec![
                ExecRule {
                    prefix: argv(&["a"]),
                    verdict: Verdict::Allow,
                },
                ExecRule {
                    prefix: argv(&["a"]),
                    verdict: Verdict::Forbidden,
                },
                ExecRule {
                    prefix: argv(&["a"]),
                    verdict: Verdict::Prompt,
                },
            ],
        };
        assert_eq!(
            p.evaluate(&argv(&["a", "b"])),
            Some(Verdict::Forbidden),
            "equal-length tie → forbidden"
        );
    }

    #[test]
    fn amendments_add_and_remove_in_order() {
        let mut p = ExecPolicy {
            rules: vec![ExecRule {
                prefix: argv(&["cargo"]),
                verdict: Verdict::Forbidden,
            }],
        };
        p.amend(&[
            Amendment::Add(ExecRule {
                prefix: argv(&["cargo", "test"]),
                verdict: Verdict::Allow,
            }),
            Amendment::Remove(argv(&["cargo"])),
        ]);
        // remove cleared the broad rule; the later-added specific one stays
        assert_eq!(p.evaluate(&argv(&["cargo", "test"])), Some(Verdict::Allow));
        assert_eq!(p.evaluate(&argv(&["cargo", "build"])), None);
    }

    #[test]
    fn self_tests_refuse_a_policy_that_lies() {
        let p = policy();
        p.self_test(&[(argv(&["git", "push"]), Verdict::Prompt)])
            .unwrap();
        let err = p
            .self_test(&[(argv(&["git", "push"]), Verdict::Allow)])
            .unwrap_err();
        assert!(err.contains("self-test #0"), "{err}");
    }

    #[test]
    fn config_load_runs_amendments_then_self_tests() {
        let cfg = Config::from_json_str(
            r#"{"execpolicy": {
                "rule": [{"prefix": ["git"], "decision": "allow"}],
                "amend": [{"op": "add", "prefix": ["git", "push"], "decision": "prompt"}],
                "self_test": [{"argv": ["git", "push"], "expect": "prompt"}]
            }}"#,
        )
        .unwrap();
        let p = ExecPolicy::from_config(&cfg).unwrap().unwrap();
        assert_eq!(p.evaluate(&argv(&["git", "push"])), Some(Verdict::Prompt));

        // A self-test that fails after amendments refuses the WHOLE policy.
        let bad = Config::from_json_str(
            r#"{"execpolicy": {
                "rule": [{"prefix": ["git"], "decision": "allow"}],
                "self_test": [{"argv": ["git", "push"], "expect": "forbidden"}]
            }}"#,
        )
        .unwrap();
        assert!(
            ExecPolicy::from_config(&bad).is_err(),
            "lying policy refused"
        );
    }
}
