//! Compaction module (INV-07): summarize via the small role; failure is
//! an explicit stop — never a silent continuation with truncated context.

use crate::providers::{ChatMessage, ChatRequest, Provider};

pub const MIN_SUMMARY_LEN: usize = 16;

pub fn summarize_via(
    provider: &dyn Provider,
    transcript: &[(String, String)],
) -> crate::Result<String> {
    let body = transcript
        .iter()
        .map(|(role, content)| format!("{role}: {content}"))
        .collect::<Vec<_>>()
        .join("\n");
    let req = ChatRequest {
        messages: vec![ChatMessage::user(format!(
            "Summarize this conversation for context continuation. Keep file paths, decisions, and pending work.\n\n{body}"
        ))],
        ..Default::default()
    };
    let step = futures_executor(provider, &req)?;
    let s = step.text();
    if s.trim().len() < MIN_SUMMARY_LEN {
        return Err(crate::CoreError::Provider(format!(
            "compaction summary too short ({} chars) — refusing to silently lose context",
            s.trim().len()
        )));
    }
    Ok(s)
}

fn futures_executor(
    provider: &dyn Provider,
    req: &ChatRequest,
) -> crate::Result<crate::providers::Step> {
    // bridge sync fn → async provider (used from sync contexts/tests)
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| crate::CoreError::Provider(format!("runtime: {e}")))?;
    rt.block_on(provider.chat_step(req))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{MockProvider, Step};

    #[test]
    fn short_summary_is_explicit_failure() {
        let mock = MockProvider::new(vec![MockProvider::text("ok")]);
        assert!(summarize_via(&mock, &[("user".into(), "hi".into())]).is_err());
    }

    #[test]
    fn good_summary_passes() {
        let mock = MockProvider::new(vec![Step {
            blocks: vec![crate::providers::Block::Text(
                "The user asked to fix the parser; work continues in src/parse.rs.".into(),
            )],
            stop: Some(crate::providers::Stop::EndTurn),
            usage: Default::default(),
        }]);
        let s = summarize_via(&mock, &[("user".into(), "hi".into())]).unwrap();
        assert!(s.contains("parser"));
    }
}
