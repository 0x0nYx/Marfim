//! Aux failover recovery ladder (W7.3, distillation §F — Hermes
//! `auxiliary_client.py`): on a 400-SHAPE error the same model is retried
//! with the offending parameter STRIPPED before the failover walk moves
//! to the next chain entry. Pure and deterministic: the error text
//! classifies to one parameter, the rung removes exactly that parameter.
//!
//! Rungs (in the spec's order — temperature → format → reasoning →
//! max_tokens; Marfim has no format parameter, so that rung is n/a):
//! - temperature: the model rejects sampling controls (thinking/
//!   reasoning models commonly do)
//! - reasoning: effort/budget/thinking rejected at this size or tier
//! - max_tokens: the completion cap exceeds the model's window
//!
//! Credential refresh and key rotation stay QUEUED (they need the W7.4
//! credential pools; a rung without a pool to refresh against would be
//! theater). Capability-scoped recovery (strict-tool 400 → disable
//! strict) is MOOT: Marfim never sends `strict: true`.

use super::ChatRequest;

/// One shape rung: which parameter the wire rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeParam {
    Temperature,
    Reasoning,
    MaxTokens,
}

/// Classify a provider error body to the parameter it names. Conservative
/// by design: only exact, vendor-documented shape errors classify —
/// anything ambiguous returns None and the failover walk proceeds
/// unchanged (never invent a retry the wire did not ask for).
pub fn shape_param(err: &str) -> Option<ShapeParam> {
    let e = err.to_ascii_lowercase();
    // Anthropic / OpenAI shape rejections name the field explicitly.
    if e.contains("temperature")
        && (e.contains("unsupported") || e.contains("does not support") || e.contains("unexpected"))
    {
        return Some(ShapeParam::Temperature);
    }
    if e.contains("thinking")
        || e.contains("reasoning_effort")
        || (e.contains("reasoning") && e.contains("unsupported"))
        || (e.contains("budget_tokens") && e.contains("invalid"))
    {
        return Some(ShapeParam::Reasoning);
    }
    if (e.contains("max_tokens")
        || e.contains("max_completion_tokens")
        || e.contains("maxoutputtokens"))
        && (e.contains("too large") || e.contains("exceed") || e.contains("greater than"))
    {
        return Some(ShapeParam::MaxTokens);
    }
    None
}

/// The next rung DOWN: the same request with the offending parameter
/// removed. Returns `None` when the request does not carry the parameter
/// (nothing to strip — the walk proceeds).
pub fn strip_param(req: &ChatRequest, p: ShapeParam) -> Option<ChatRequest> {
    let mut r = req.clone();
    match p {
        ShapeParam::Temperature => {
            let had = r.temperature.take().is_some();
            had.then_some(r)
        }
        ShapeParam::Reasoning => {
            let had = r.effort.is_some() || r.thinking_budget.is_some();
            r.effort = None;
            r.thinking_budget = None;
            had.then_some(r)
        }
        ShapeParam::MaxTokens => {
            let had = r.max_completion_tokens.take().is_some();
            had.then_some(r)
        }
    }
}

/// Convenience: classify + strip in one step (the loop's single rung).
pub fn strip_after_shape_error(req: &ChatRequest, err: &str) -> Option<ChatRequest> {
    strip_param(req, shape_param(err)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req() -> ChatRequest {
        ChatRequest {
            messages: vec![super::super::ChatMessage::user("hi")],
            temperature: Some(0.7),
            effort: Some(super::super::effort::Effort::High),
            thinking_budget: Some(4096),
            max_completion_tokens: Some(999),
            ..Default::default()
        }
    }

    #[test]
    fn classifies_the_documented_shape_errors() {
        assert_eq!(
            shape_param("400 {\"error\": \"temperature is unsupported for this model\"}"),
            Some(ShapeParam::Temperature)
        );
        assert_eq!(
            shape_param("400 thinking is not supported at this tier"),
            Some(ShapeParam::Reasoning)
        );
        assert_eq!(
            shape_param("400 max_tokens: 999999 exceeds the model window"),
            Some(ShapeParam::MaxTokens)
        );
    }

    #[test]
    fn ambiguous_errors_never_classify() {
        assert_eq!(shape_param("401 unauthorized"), None);
        assert_eq!(shape_param("429 rate limited"), None);
        assert_eq!(shape_param("connection reset"), None);
        // a bare mention without the shape verbs is NOT a shape error
        assert_eq!(shape_param("400 temperature must be a number"), None);
    }

    #[test]
    fn rungs_strip_exactly_one_parameter() {
        let r = strip_after_shape_error(&req(), "400 temperature is unsupported for this model")
            .unwrap();
        assert!(r.temperature.is_none());
        assert!(r.effort.is_some(), "other parameters survive the rung");
        assert!(r.thinking_budget.is_some());
        assert_eq!(r.max_completion_tokens, Some(999));
    }

    #[test]
    fn reasoning_rung_drops_both_effort_and_budget() {
        let r =
            strip_after_shape_error(&req(), "400 thinking is not supported at this tier").unwrap();
        assert!(r.effort.is_none());
        assert!(r.thinking_budget.is_none());
        assert_eq!(r.temperature, Some(0.7));
    }

    #[test]
    fn stripping_an_absent_parameter_says_so() {
        let mut r = req();
        r.temperature = None;
        assert!(strip_after_shape_error(&r, "400 temperature is unsupported").is_none());
    }
}
