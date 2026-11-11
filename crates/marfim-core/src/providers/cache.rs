//! W7.2 — cache-TTL plumbing (distillation §R/§P: cachedContent, warming;
//! Hermes TTL-reject ladder). Everything here is default-off or
//! default-identical: without a `[cache]` config section every wire body
//! stays byte-identical, and the cache ladder never classifies.
//!
//! Pieces:
//! - [`CacheTtl`] — Anthropic `cache_control.ttl` (`"5m"` | `"1h"`).
//!   Applied to the SYSTEM breakpoint only (bounded: tools keep the
//!   plain ephemeral marker).
//! - [`CacheSettings`] — the config surface: `[cache] ttl`,
//!   `cachedContentName` (Gemini `cachedContent`, sent verbatim), `warm`.
//! - [`warm_request`] — the bounded warming ping: the expensive shared
//!   prefix (system), no history, no tools, 1-token completion, ONE try.
//! - the TTL-reject ladder (`cache_after_error`): a 400 that names the
//!   ttl classifies to `StripTtl` (retry with ttl removed) and, failing
//!   again, `StripMarkers` (retry with every cache_control removed).
//!   Ambiguous errors never classify — the failover walk proceeds.

use crate::config::Config;
use crate::providers::{ChatMessage, ChatRequest};

/// Anthropic prompt-cache TTL dialect (documented: ephemeral markers may
/// carry `"ttl": "5m"` (default) or `"ttl": "1h"`). Serde round-trips the
/// WIRE strings (manual impls below).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CacheTtl {
    #[default]
    FiveMinutes,
    OneHour,
}

impl serde::Serialize for CacheTtl {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.wire_value())
    }
}

impl<'de> serde::Deserialize<'de> for CacheTtl {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        CacheTtl::parse(&s).map_err(serde::de::Error::custom)
    }
}

impl CacheTtl {
    /// Parse the config string; unknown values fail closed (the caller
    /// surfaces the error, nothing guesses).
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "5m" => Ok(CacheTtl::FiveMinutes),
            "1h" => Ok(CacheTtl::OneHour),
            other => Err(format!(
                "cache.ttl: unknown value `{other}` (expected \"5m\" or \"1h\")"
            )),
        }
    }

    /// The wire value on the Anthropic breakpoint.
    pub fn wire_value(&self) -> &'static str {
        match self {
            CacheTtl::FiveMinutes => "5m",
            CacheTtl::OneHour => "1h",
        }
    }
}

/// The `[cache]` config section, resolved once at settings time.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CacheSettings {
    /// Anthropic system-breakpoint TTL. `None` = plain ephemeral
    /// (byte-identical to every pre-W7.2 body).
    pub ttl: Option<CacheTtl>,
    /// Gemini `cachedContent` name, sent verbatim when set.
    pub cached_content: Option<String>,
    /// Bounded preflight warming ping at the first turn (default off).
    pub warm: bool,
}

impl CacheSettings {
    /// Read `[cache]` from the layered config. Absent section = defaults.
    /// Unknown keys inside the section are refused by
    /// `Config::validate_consumed_sections` (the section is registered
    /// there) — this reader only answers for KNOWN keys, and type errors
    /// fail closed.
    pub fn from_config(config: &Config) -> Result<Self, String> {
        let mut out = CacheSettings::default();
        let Some(v) = config.get("cache") else {
            return Ok(out);
        };
        let obj = v
            .as_object()
            .ok_or_else(|| "cache section must be an object".to_string())?;
        if let Some(ttl) = obj.get("ttl") {
            let s = ttl
                .as_str()
                .ok_or_else(|| "cache.ttl must be a string".to_string())?;
            out.ttl = Some(CacheTtl::parse(s)?);
        }
        if let Some(name) = obj.get("cachedContentName") {
            let s = name
                .as_str()
                .ok_or_else(|| "cache.cachedContentName must be a string".to_string())?;
            if s.is_empty() {
                return Err("cache.cachedContentName must not be empty".to_string());
            }
            out.cached_content = Some(s.to_string());
        }
        if let Some(w) = obj.get("warm") {
            out.warm = w
                .as_bool()
                .ok_or_else(|| "cache.warm must be a boolean".to_string())?;
        }
        Ok(out)
    }
}

/// The bounded warming ping: the system prompt (the expensive shared
/// prefix) with a 1-token completion, no tools, no history. Sent ONCE at
/// the first turn when `cache.warm` is on; failures are observable
/// (CacheWarm event) and never fatal.
pub fn warm_request(system: &str, key: &str) -> ChatRequest {
    ChatRequest {
        messages: vec![ChatMessage::user("warm")],
        tools: vec![],
        max_completion_tokens: Some(1),
        cache_breakpoints: true,
        system: Some(system.to_string()),
        prompt_cache_key: Some(key.to_string()),
        ..Default::default()
    }
}

/// One cache rung: what to remove after the wire refused the cache shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheRung {
    /// Drop the explicit ttl (the "1h" marker) — keep the breakpoints.
    StripTtl,
    /// Drop EVERY cache_control marker — the model/wire accepts no
    /// caching at all.
    StripMarkers,
}

/// Classify a provider error to a cache rung. Conservative: only errors
/// that NAME the ttl / cache-control surface classify. Order is fixed:
/// `StripTtl` classifies first; `StripMarkers` only when the error names
/// cache-control wholesale (or the request carries no ttl to strip).
pub fn cache_rung(err: &str) -> Option<CacheRung> {
    let e = err.to_ascii_lowercase();
    if (e.contains("ttl") || e.contains("cache_control") || e.contains("cachcontrol"))
        && (e.contains("invalid") || e.contains("unsupported") || e.contains("unknown"))
    {
        return Some(CacheRung::StripTtl);
    }
    if e.contains("cache_control") && e.contains("not supported") {
        return Some(CacheRung::StripMarkers);
    }
    None
}

/// The next request one rung down. `None` = the request carries nothing
/// the rung could remove (the walk proceeds unchanged).
pub fn strip_cache(req: &ChatRequest, rung: CacheRung) -> Option<ChatRequest> {
    let mut r = req.clone();
    match rung {
        CacheRung::StripTtl => {
            let had = r.cache_ttl.take().is_some();
            had.then_some(r)
        }
        CacheRung::StripMarkers => {
            let had = r.cache_ttl.is_some() || r.cache_breakpoints;
            r.cache_ttl = None;
            r.cache_breakpoints = false;
            had.then_some(r)
        }
    }
}

/// Convenience: classify + strip in one step (the loop's cache rung).
/// `StripMarkers` is reachable only when the request has no ttl to drop
/// but the wire still rejects caching — the ladder never burns a retry
/// that removes nothing.
pub fn cache_after_error(req: &ChatRequest, err: &str) -> Option<ChatRequest> {
    let rung = cache_rung(err)?;
    match rung {
        CacheRung::StripTtl => strip_cache(req, CacheRung::StripTtl)
            .or_else(|| strip_cache(req, CacheRung::StripMarkers)),
        CacheRung::StripMarkers => strip_cache(req, CacheRung::StripMarkers),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ttl_parse_fails_closed() {
        assert_eq!(CacheTtl::parse("1h"), Ok(CacheTtl::OneHour));
        assert_eq!(CacheTtl::parse("5m"), Ok(CacheTtl::FiveMinutes));
        assert!(CacheTtl::parse("24h").is_err());
        assert!(CacheTtl::parse("").is_err());
    }

    #[test]
    fn settings_read_the_config_section() {
        let cfg = Config::from_json_str(
            r#"{"cache": {"ttl": "1h", "cachedContentName": "cachedContents/abc", "warm": true}}"#,
        )
        .unwrap();
        let s = CacheSettings::from_config(&cfg).unwrap();
        assert_eq!(s.ttl, Some(CacheTtl::OneHour));
        assert_eq!(s.cached_content.as_deref(), Some("cachedContents/abc"));
        assert!(s.warm);
    }

    #[test]
    fn absent_section_is_all_defaults() {
        let cfg = Config::from_json_str(r#"{"model": "m"}"#).unwrap();
        let s = CacheSettings::from_config(&cfg).unwrap();
        assert_eq!(s, CacheSettings::default());
    }

    #[test]
    fn type_errors_fail_closed() {
        let cfg = Config::from_json_str(r#"{"cache": {"ttl": 17}}"#).unwrap();
        assert!(CacheSettings::from_config(&cfg).is_err());
        let cfg = Config::from_json_str(r#"{"cache": {"warm": "yes"}}"#).unwrap();
        assert!(CacheSettings::from_config(&cfg).is_err());
        let cfg = Config::from_json_str(r#"{"cache": {"cachedContentName": ""}}"#).unwrap();
        assert!(CacheSettings::from_config(&cfg).is_err());
    }

    #[test]
    fn warm_request_is_bounded() {
        let r = warm_request("SYS", "pc_x");
        assert_eq!(r.messages.len(), 1);
        assert!(r.tools.is_empty(), "warming carries no tool schemas");
        assert_eq!(r.max_completion_tokens, Some(1));
        assert_eq!(r.system.as_deref(), Some("SYS"));
        assert!(r.cache_breakpoints, "the warm ping sets the cache prefix");
        assert_eq!(r.prompt_cache_key.as_deref(), Some("pc_x"));
    }

    #[test]
    fn ttl_reject_classifies() {
        assert_eq!(
            cache_rung(r#"400 {"error": "cache_control.ttl invalid value"}"#),
            Some(CacheRung::StripTtl)
        );
        assert_eq!(
            cache_rung("400 ttl unsupported for this model"),
            Some(CacheRung::StripTtl)
        );
        assert_eq!(
            cache_rung("400 cache_control is not supported on this endpoint"),
            Some(CacheRung::StripMarkers)
        );
        // ambiguous errors never classify
        assert_eq!(cache_rung("400 bad request"), None);
        assert_eq!(cache_rung("429 rate limited"), None);
    }

    #[test]
    fn ladder_strips_ttl_then_markers() {
        let mut r = warm_request("SYS", "k");
        r.cache_ttl = Some(CacheTtl::OneHour);
        // rung 1: ttl goes, markers stay
        let r1 = cache_after_error(&r, "400 ttl invalid").unwrap();
        assert!(r1.cache_ttl.is_none());
        assert!(r1.cache_breakpoints);
        // rung 2 (on a fresh error naming cache_control): markers go
        let r2 = cache_after_error(&r1, "400 cache_control not supported").unwrap();
        assert!(!r2.cache_breakpoints);
        // nothing left to strip → the walk proceeds
        assert!(cache_after_error(&r2, "400 cache_control not supported").is_none());
    }

    #[test]
    fn no_ttl_falls_through_to_markers() {
        let r = warm_request("SYS", "k"); // no explicit ttl
        let r1 = cache_after_error(&r, "400 ttl invalid").unwrap();
        assert!(
            !r1.cache_breakpoints,
            "no ttl to drop → markers go directly"
        );
    }
}
