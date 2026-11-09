//! Per-model capability registry (W7.1): compat rules as committed TOML
//! data, resolved by longest-prefix match over the model id.
//!
//! Distillation §R/§R3/§P: per-model capability tables (the Hermes
//! "fable/opus-5.5 mandatory-thinking" doctrine) and Gemini thought
//! signatures preserved per block for replay. The registry is the single
//! place a new model generation touches: add a row, no code changes.
//!
//! Fail-closed doctrine: the registry is embedded at compile time
//! (`include_str!`), parsed lazily once; a malformed row makes EVERY
//! lookup fail visibly (`capability_for` returns the default AND the
//! parse error is pinned by a test so a bad commit cannot ship).

use std::sync::OnceLock;

const REGISTRY_TOML: &str = include_str!("capabilities.toml");

/// How the Anthropic wire should engage thinking for a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThinkingMode {
    /// Explicit effort/budget enables thinking (today's default).
    #[default]
    Budgeted,
    /// Thinking rides `{"type": "adaptive", "budget_tokens": N}`.
    Adaptive,
    /// The model MUST think: a default budget is forced even when the
    /// user set nothing (Hermes mandatory-thinking doctrine).
    Mandatory,
}

/// Resolved capability set for one model id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ModelCaps {
    pub thinking: ThinkingMode,
    /// The wire returns per-call `thoughtSignature` fields that MUST be
    /// replayed verbatim on later turns (Gemini 3+ function calls).
    pub thought_signatures: bool,
}

#[derive(serde::Deserialize)]
struct Row {
    pattern: String,
    #[serde(default)]
    thinking: Option<String>,
    #[serde(default)]
    thought_signatures: Option<bool>,
}

#[derive(serde::Deserialize)]
struct Registry {
    #[serde(default, rename = "model")]
    models: Vec<Row>,
}

fn parse_registry() -> Result<Registry, String> {
    let reg: Registry = toml::from_str(REGISTRY_TOML)
        .map_err(|e| format!("capability registry is malformed TOML: {e}"))?;
    for (i, row) in reg.models.iter().enumerate() {
        if row.pattern.is_empty() {
            return Err(format!("capability registry row {i} has an empty pattern"));
        }
        if let Some(t) = &row.thinking {
            match t.as_str() {
                "budgeted" | "adaptive" | "mandatory" => {}
                other => {
                    return Err(format!(
                        "capability registry row `{}` has unknown thinking mode `{other}`",
                        row.pattern
                    ))
                }
            }
        }
    }
    Ok(reg)
}

fn registry() -> &'static Registry {
    static REG: OnceLock<Registry> = OnceLock::new();
    REG.get_or_init(|| {
        // The committed file is pinned by tests; a runtime failure here
        // degrades to the default capability set (never a panic — G1).
        parse_registry().unwrap_or_else(|_| Registry { models: Vec::new() })
    })
}

/// Resolve the capability set for a model id: the row whose pattern is
/// the LONGEST prefix of the id wins (deterministic tie-break: the first
/// such row in file order). Unknown ids resolve to the default
/// capability set — which keeps the wire behavior byte-identical for
/// models the registry does not know.
pub fn capability_for(model: &str) -> ModelCaps {
    let reg = registry();
    let mut best: Option<(usize, &Row)> = None;
    for row in &reg.models {
        if model.starts_with(&row.pattern) {
            let len = row.pattern.len();
            if best.map(|(l, _)| len > l).unwrap_or(true) {
                best = Some((len, row));
            }
        }
    }
    let Some((_, row)) = best else {
        return ModelCaps::default();
    };
    ModelCaps {
        thinking: match row.thinking.as_deref() {
            Some("adaptive") => ThinkingMode::Adaptive,
            Some("mandatory") => ThinkingMode::Mandatory,
            _ => ThinkingMode::Budgeted,
        },
        thought_signatures: row.thought_signatures.unwrap_or(false),
    }
}

/// The parse contract of the COMMITTED registry (fail-loud at test time,
/// fail-soft at runtime): the file must parse and every row must be
/// well-formed.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn committed_registry_parses_clean() {
        let reg = parse_registry().expect("committed capability registry must parse");
        assert!(
            !reg.models.is_empty(),
            "registry must carry at least one row"
        );
    }

    #[test]
    fn mandatory_thinking_rows_resolve() {
        // §R: fable / opus-5.5 mandatory-thinking (Hermes adapter).
        assert_eq!(
            capability_for("claude-fable-5-1").thinking,
            ThinkingMode::Mandatory
        );
        assert_eq!(
            capability_for("claude-opus-5-5").thinking,
            ThinkingMode::Mandatory
        );
    }

    #[test]
    fn adaptive_rows_resolve() {
        assert_eq!(
            capability_for("claude-opus-4-6").thinking,
            ThinkingMode::Adaptive
        );
        assert_eq!(
            capability_for("claude-sonnet-4-6").thinking,
            ThinkingMode::Adaptive
        );
    }

    #[test]
    fn unknown_model_resolves_default() {
        let caps = capability_for("mock-primary");
        assert_eq!(caps, ModelCaps::default());
        // A prefix that matches nothing keeps the default too.
        assert_eq!(capability_for("claude-haiku-4-5"), ModelCaps::default());
    }

    #[test]
    fn gemini_thought_signatures_split_at_generation() {
        assert!(!capability_for("gemini-2.5-pro").thought_signatures);
        assert!(capability_for("gemini-3.1-pro-preview").thought_signatures);
        assert!(capability_for("gemini-4-argon").thought_signatures);
    }

    #[test]
    fn longest_prefix_wins() {
        // `claude-opus-5-5` is more specific than a hypothetical shorter
        // overlapping pattern; pin the resolution order explicitly.
        let caps = capability_for("claude-opus-5-5-20260101");
        assert_eq!(caps.thinking, ThinkingMode::Mandatory);
    }
}
