//! W10.2 (§G, dsh architecture): source plugins — external triggers
//! (GitHub webhooks, Feishu events) normalized into `SourceEvent`s and
//! routed to per-ticket worker sessions. Deterministic, deduplicated,
//! bounded. The untrusted-webhook doctrine: worker sessions spawned from
//! a source event get ONLY the platform-restricted toolset (web_search,
//! web_extract, vision, clarify) — never shell/edit.

use serde::{Deserialize, Serialize};

/// The platform-restricted toolset for untrusted webhook surfaces
/// (Hermes toolsets.py doctrine).
pub const UNTRUSTED_WORKER_TOOLSET: &[&str] = &["web_search", "web_extract", "vision", "clarify"];

/// Bounded dedupe window: seen event keys remembered per registry.
pub const SEEN_CAP: usize = 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceEvent {
    /// plugin name ("github", "feishu", or any registered source)
    pub source: String,
    /// normalized kind ("issue.opened", "pr.opened", "push", "message.received", ...)
    pub kind: String,
    /// the ticket/thread key (issue number, PR number, chat id)
    pub ticket: String,
    pub title: String,
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// delivery id from the provider (dedupe key component)
    pub delivery_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkerSpec {
    pub source: String,
    pub kind: String,
    pub ticket: String,
    /// the worker session's opening prompt
    pub prompt: String,
    /// the toolset the worker may use (untrusted webhook surface = restricted)
    pub toolset: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RouteError {
    /// the plugin is not registered
    UnknownSource(String),
    /// the kind is not allowlisted for this plugin
    KindNotAllowed { source: String, kind: String },
    /// duplicate delivery (already routed)
    DuplicateDelivery,
    /// malformed event (empty ticket or delivery id)
    Malformed,
}

#[derive(Debug, Clone, Default)]
pub struct SourceRegistry {
    /// plugin name → allowlisted kinds
    plugins: std::collections::BTreeMap<String, Vec<String>>,
    /// seen (source, delivery_id) keys, bounded drop-oldest
    seen: std::collections::VecDeque<(String, String)>,
}

impl SourceRegistry {
    /// Register a plugin with its allowlisted kinds. An empty kind list
    /// registers the plugin as inert (accepts nothing) — honest, not a
    /// wildcard.
    pub fn register(&mut self, name: &str, kinds: &[&str]) {
        self.plugins.insert(
            name.to_string(),
            kinds.iter().map(|s| s.to_string()).collect(),
        );
    }

    pub fn plugin_names(&self) -> Vec<String> {
        self.plugins.keys().cloned().collect()
    }

    pub fn allowed_kinds(&self, name: &str) -> Option<Vec<String>> {
        self.plugins.get(name).cloned()
    }

    /// Route one event to a worker spec. Dedupe key = (source,
    /// delivery_id); a replayed delivery is refused, not re-routed.
    pub fn route(&mut self, e: &SourceEvent) -> Result<WorkerSpec, RouteError> {
        if e.ticket.is_empty() || e.delivery_id.is_empty() || e.source.is_empty() {
            return Err(RouteError::Malformed);
        }
        let Some(kinds) = self.plugins.get(&e.source) else {
            return Err(RouteError::UnknownSource(e.source.clone()));
        };
        if !kinds.iter().any(|k| k == &e.kind) {
            return Err(RouteError::KindNotAllowed {
                source: e.source.clone(),
                kind: e.kind.clone(),
            });
        }
        let key = (e.source.clone(), e.delivery_id.clone());
        if self.seen.contains(&key) {
            return Err(RouteError::DuplicateDelivery);
        }
        self.seen.push_back(key);
        while self.seen.len() > SEEN_CAP {
            self.seen.pop_front();
        }
        Ok(WorkerSpec {
            source: e.source.clone(),
            kind: e.kind.clone(),
            ticket: e.ticket.clone(),
            prompt: format!(
                "You are the worker for {source} {kind} #{ticket}: {title}\n\n{body}\n\nWork the ticket. Report what you changed and what you verified.",
                source = e.source,
                kind = e.kind,
                ticket = e.ticket,
                title = e.title,
                body = e.body,
            ),
            // the untrusted webhook surface NEVER gets shell/edit
            toolset: UNTRUSTED_WORKER_TOOLSET.iter().map(|s| s.to_string()).collect(),
        })
    }
}

/// Normalize a GitHub webhook payload (the subset Marfim reacts to).
/// Unknown event names fail closed — the caller decides whether to
/// ignore the error or log it.
pub fn normalize_github(
    event_name: &str,
    payload: &serde_json::Value,
) -> Result<SourceEvent, String> {
    let delivery = payload
        .get("delivery_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    match event_name {
        "issues" => {
            let action = payload.get("action").and_then(|v| v.as_str()).unwrap_or("");
            if action != "opened" {
                return Err(format!(
                    "issues action {action:?} not routable (only \"opened\")"
                ));
            }
            let issue = payload
                .get("issue")
                .ok_or("issues payload missing `issue`")?;
            Ok(SourceEvent {
                source: "github".into(),
                kind: "issue.opened".into(),
                ticket: issue
                    .get("number")
                    .and_then(|v| v.as_u64())
                    .map(|n| n.to_string())
                    .ok_or("issue missing number")?,
                title: issue
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .into(),
                body: issue
                    .get("body")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .into(),
                url: issue
                    .get("html_url")
                    .and_then(|v| v.as_str())
                    .map(|s| s.into()),
                delivery_id: delivery,
            })
        }
        "pull_request" => {
            let action = payload.get("action").and_then(|v| v.as_str()).unwrap_or("");
            if action != "opened" {
                return Err(format!(
                    "pull_request action {action:?} not routable (only \"opened\")"
                ));
            }
            let pr = payload
                .get("pull_request")
                .ok_or("payload missing `pull_request`")?;
            Ok(SourceEvent {
                source: "github".into(),
                kind: "pr.opened".into(),
                ticket: pr
                    .get("number")
                    .and_then(|v| v.as_u64())
                    .map(|n| n.to_string())
                    .ok_or("pull_request missing number")?,
                title: pr
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .into(),
                body: pr.get("body").and_then(|v| v.as_str()).unwrap_or("").into(),
                url: pr
                    .get("html_url")
                    .and_then(|v| v.as_str())
                    .map(|s| s.into()),
                delivery_id: delivery,
            })
        }
        "push" => {
            let after = payload.get("after").and_then(|v| v.as_str()).unwrap_or("");
            if after.is_empty() {
                return Err("push payload missing `after`".into());
            }
            Ok(SourceEvent {
                source: "github".into(),
                kind: "push".into(),
                ticket: after.chars().take(12).collect(),
                title: format!(
                    "push to {}",
                    payload.get("ref").and_then(|v| v.as_str()).unwrap_or("?")
                ),
                body: payload
                    .get("head_commit")
                    .and_then(|c| c.get("message"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .into(),
                url: None,
                delivery_id: delivery,
            })
        }
        other => Err(format!("github event {other:?} is not routable")),
    }
}

/// Normalize a Feishu event payload (message received). Only the
/// `im.message.receive_v1` event is routable.
pub fn normalize_feishu(payload: &serde_json::Value) -> Result<SourceEvent, String> {
    let header = payload
        .get("header")
        .ok_or("feishu payload missing `header`")?;
    let event_type = header
        .get("event_type")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if event_type != "im.message.receive_v1" {
        return Err(format!("feishu event {event_type:?} is not routable"));
    }
    let delivery = header
        .get("event_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let event = payload
        .get("event")
        .ok_or("feishu payload missing `event`")?;
    let chat = event
        .pointer("/message/chat_id")
        .and_then(|v| v.as_str())
        .ok_or("feishu event missing message.chat_id")?;
    let content = event
        .pointer("/message/content")
        .and_then(|v| v.as_str())
        .unwrap_or("{}");
    let text = serde_json::from_str::<serde_json::Value>(content)
        .ok()
        .and_then(|c| {
            c.get("text")
                .and_then(|t| t.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_default();
    Ok(SourceEvent {
        source: "feishu".into(),
        kind: "message.received".into(),
        ticket: chat.to_string(),
        title: format!("feishu message in {chat}"),
        body: text,
        url: None,
        delivery_id: delivery,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg() -> SourceRegistry {
        let mut r = SourceRegistry::default();
        r.register("github", &["issue.opened", "pr.opened", "push"]);
        r.register("feishu", &["message.received"]);
        r
    }

    fn ev() -> SourceEvent {
        SourceEvent {
            source: "github".into(),
            kind: "issue.opened".into(),
            ticket: "42".into(),
            title: "build fails".into(),
            body: "cargo test red".into(),
            url: None,
            delivery_id: "d1".into(),
        }
    }

    #[test]
    fn route_builds_worker_spec_with_restricted_toolset() {
        let mut r = reg();
        let w = r.route(&ev()).unwrap();
        assert_eq!(w.toolset, UNTRUSTED_WORKER_TOOLSET.to_vec());
        assert!(!w.toolset.iter().any(|t| t == "Bash" || t == "Edit"));
        assert!(w.prompt.contains("#42"));
        assert!(w.prompt.contains("build fails"));
    }

    #[test]
    fn duplicate_delivery_refused() {
        let mut r = reg();
        assert!(r.route(&ev()).is_ok());
        assert_eq!(r.route(&ev()), Err(RouteError::DuplicateDelivery));
    }

    #[test]
    fn unknown_source_and_kind_refused() {
        let mut r = reg();
        let mut e = ev();
        e.source = "twitter".into();
        assert_eq!(
            r.route(&e),
            Err(RouteError::UnknownSource("twitter".into()))
        );
        let mut e2 = ev();
        e2.kind = "issue.closed".into();
        assert!(matches!(
            r.route(&e2),
            Err(RouteError::KindNotAllowed { .. })
        ));
    }

    #[test]
    fn malformed_events_refused() {
        let mut r = reg();
        let mut e = ev();
        e.ticket = String::new();
        assert_eq!(r.route(&e), Err(RouteError::Malformed));
        let mut e2 = ev();
        e2.delivery_id = String::new();
        assert_eq!(r.route(&e2), Err(RouteError::Malformed));
    }

    #[test]
    fn seen_window_is_bounded_drop_oldest() {
        let mut r = SourceRegistry::default();
        r.register("github", &["issue.opened"]);
        // SEEN_CAP+1 distinct deliveries, then replay the FIRST — the
        // oldest entry was evicted, so the replay is accepted again.
        for n in 0..(SEEN_CAP + 1) {
            let mut e = ev();
            e.delivery_id = format!("d{n}");
            assert!(r.route(&e).is_ok(), "delivery {n} refused");
        }
        let mut first = ev();
        first.delivery_id = "d0".into();
        assert!(
            r.route(&first).is_ok(),
            "evicted delivery may replay (bounded window)"
        );
    }

    #[test]
    fn github_issue_normalization() {
        let payload = serde_json::json!({
            "action": "opened",
            "delivery_id": "gh-1",
            "issue": {"number": 7, "title": "crash on start", "body": "steps...", "html_url": "https://github.com/x/y/issues/7"}
        });
        let e = normalize_github("issues", &payload).unwrap();
        assert_eq!(e.kind, "issue.opened");
        assert_eq!(e.ticket, "7");
        assert_eq!(e.delivery_id, "gh-1");
        assert_eq!(e.url.as_deref(), Some("https://github.com/x/y/issues/7"));
        // non-opened actions refused
        assert!(normalize_github(
            "issues",
            &serde_json::json!({"action": "closed", "issue": {"number": 7}})
        )
        .is_err());
        assert!(normalize_github("fork", &payload).is_err());
    }

    #[test]
    fn github_push_and_pr_normalization() {
        let push = serde_json::json!({
            "after": "abcdef1234567890", "ref": "refs/heads/main", "delivery_id": "d9",
            "head_commit": {"message": "fix: the thing"}
        });
        let e = normalize_github("push", &push).unwrap();
        assert_eq!(e.kind, "push");
        assert_eq!(e.ticket, "abcdef123456");
        let pr = serde_json::json!({
            "action": "opened", "delivery_id": "d10",
            "pull_request": {"number": 3, "title": "the fix", "body": "closes #1"}
        });
        let e2 = normalize_github("pull_request", &pr).unwrap();
        assert_eq!(e2.kind, "pr.opened");
        assert_eq!(e2.ticket, "3");
    }

    #[test]
    fn feishu_normalization_and_refusal() {
        let payload = serde_json::json!({
            "header": {"event_type": "im.message.receive_v1", "event_id": "fs-1"},
            "event": {"message": {"chat_id": "oc_123", "content": "{\"text\":\"run the nightly\"}"}}
        });
        let e = normalize_feishu(&payload).unwrap();
        assert_eq!(e.source, "feishu");
        assert_eq!(e.ticket, "oc_123");
        assert_eq!(e.body, "run the nightly");
        let bad = serde_json::json!({"header": {"event_type": "card.action.trigger"}});
        assert!(normalize_feishu(&bad).is_err());
    }
}
