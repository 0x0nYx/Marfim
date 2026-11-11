//! Canonical reasoning-effort ladder + per-family emitters.
//!
//! Distilled from two production agents (distillation ledger, docs/
//! DISTILLATION.md §R1):
//! - the ladder itself (`minimal|low|medium|high|xhigh|max`) comes from
//!   Oh My Pi's catalog effort module;
//! - the Anthropic budget table (`xhigh 32000 / high 16000 / medium 8000
//!   / low 4000`) and the `max_tokens >= budget + 4096` guard come from
//!   Hermes' anthropic adapter;
//! - the Gemini `MINIMAL|LOW|MEDIUM|HIGH` level mapping follows Oh My
//!   Pi's `mapEffortToGoogleThinkingLevel` (xhigh/max collapse to HIGH).
//!
//! One enum, four emitters: the config surface speaks the ladder, each
//! wire speaks its own dialect. Pure and deterministic.

/// Canonical effort ladder. `max` is the strongest setting a model can
/// express; wires that cannot go that high collapse it one notch down.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum Effort {
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl Effort {
    pub fn as_str(&self) -> &'static str {
        match self {
            Effort::Minimal => "minimal",
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::Xhigh => "xhigh",
            Effort::Max => "max",
        }
    }

    /// Parse the config surface. Accepts the exact ladder strings (and
    /// nothing else — unknown efforts fail closed instead of silently
    /// degrading to "medium").
    pub fn parse(s: &str) -> Option<Effort> {
        match s.trim().to_ascii_lowercase().as_str() {
            "minimal" => Some(Effort::Minimal),
            "low" => Some(Effort::Low),
            "medium" => Some(Effort::Medium),
            "high" => Some(Effort::High),
            "xhigh" => Some(Effort::Xhigh),
            "max" => Some(Effort::Max),
            _ => None,
        }
    }
}

/// Anthropic `thinking.budget_tokens` (Hermes table). `minimal` maps to
/// the same floor as `low`: there is no smaller Anthropic budget.
pub fn anthropic_budget_tokens(e: Effort) -> u64 {
    match e {
        Effort::Minimal | Effort::Low => 4_000,
        Effort::Medium => 8_000,
        Effort::High => 16_000,
        Effort::Xhigh | Effort::Max => 32_000,
    }
}

/// Anthropic requires `max_tokens >= budget + 4096` when thinking is
/// enabled (Hermes adapter rule — a smaller cap starves the answer).
pub fn anthropic_min_max_tokens(budget: u64) -> u64 {
    budget.saturating_add(4_096)
}

/// OpenAI chat-completions `reasoning_effort`: the wire accepts
/// `minimal|low|medium|high`; xhigh/max collapse to `high`.
pub fn openai_reasoning_effort(e: Effort) -> &'static str {
    match e {
        Effort::Minimal => "minimal",
        Effort::Low => "low",
        Effort::Medium => "medium",
        Effort::High | Effort::Xhigh | Effort::Max => "high",
    }
}

/// OpenAI Responses `reasoning.effort`: the wire additionally accepts
/// `xhigh`; `max` collapses to `xhigh`.
pub fn responses_reasoning_effort(e: Effort) -> &'static str {
    match e {
        Effort::Minimal => "minimal",
        Effort::Low => "low",
        Effort::Medium => "medium",
        Effort::High => "high",
        Effort::Xhigh | Effort::Max => "xhigh",
    }
}

/// Gemini `generationConfig.thinkingConfig.thinkingLevel`
/// (`MINIMAL|LOW|MEDIUM|HIGH`); xhigh/max collapse to `HIGH`.
pub fn gemini_thinking_level(e: Effort) -> &'static str {
    match e {
        Effort::Minimal => "MINIMAL",
        Effort::Low => "LOW",
        Effort::Medium => "MEDIUM",
        Effort::High | Effort::Xhigh | Effort::Max => "HIGH",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ladder_round_trips() {
        for s in ["minimal", "low", "medium", "high", "xhigh", "max"] {
            let e = Effort::parse(s).unwrap_or_else(|| panic!("{s} must parse"));
            assert_eq!(e.as_str(), s);
        }
        assert_eq!(
            Effort::parse("MAX").unwrap(),
            Effort::Max,
            "case-insensitive"
        );
        assert_eq!(Effort::parse(" medium "), Some(Effort::Medium), "trims");
        assert!(Effort::parse("ultra").is_none(), "unknown fails closed");
        assert!(Effort::parse("").is_none());
        assert!(Effort::parse("ultrathink").is_none());
    }

    #[test]
    fn anthropic_budgets_match_hermes_table() {
        assert_eq!(anthropic_budget_tokens(Effort::Minimal), 4_000);
        assert_eq!(anthropic_budget_tokens(Effort::Low), 4_000);
        assert_eq!(anthropic_budget_tokens(Effort::Medium), 8_000);
        assert_eq!(anthropic_budget_tokens(Effort::High), 16_000);
        assert_eq!(anthropic_budget_tokens(Effort::Xhigh), 32_000);
        assert_eq!(anthropic_budget_tokens(Effort::Max), 32_000);
        assert_eq!(anthropic_min_max_tokens(32_000), 36_096);
        assert_eq!(anthropic_min_max_tokens(0), 4_096);
    }

    #[test]
    fn openai_collapses_above_high() {
        assert_eq!(openai_reasoning_effort(Effort::Minimal), "minimal");
        assert_eq!(openai_reasoning_effort(Effort::High), "high");
        assert_eq!(openai_reasoning_effort(Effort::Xhigh), "high");
        assert_eq!(openai_reasoning_effort(Effort::Max), "high");
        assert_eq!(responses_reasoning_effort(Effort::Xhigh), "xhigh");
        assert_eq!(responses_reasoning_effort(Effort::Max), "xhigh");
        assert_eq!(responses_reasoning_effort(Effort::Minimal), "minimal");
    }

    #[test]
    fn gemini_levels_match_omp_mapping() {
        assert_eq!(gemini_thinking_level(Effort::Minimal), "MINIMAL");
        assert_eq!(gemini_thinking_level(Effort::Low), "LOW");
        assert_eq!(gemini_thinking_level(Effort::Medium), "MEDIUM");
        assert_eq!(gemini_thinking_level(Effort::High), "HIGH");
        assert_eq!(gemini_thinking_level(Effort::Xhigh), "HIGH");
        assert_eq!(gemini_thinking_level(Effort::Max), "HIGH");
    }

    #[test]
    fn ladder_orders_strictly() {
        let l = [
            Effort::Minimal,
            Effort::Low,
            Effort::Medium,
            Effort::High,
            Effort::Xhigh,
            Effort::Max,
        ];
        for w in l.windows(2) {
            assert!(w[0] < w[1], "ladder must be strictly ordered");
        }
    }
}
