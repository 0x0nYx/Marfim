//! Marfim core: event bus, agent loop, providers, router, permissions,
//! sessions, hooks, trust, sandbox, skills, memory, guardian, evidence.
//!
//! #![forbid(unsafe_code)] — enforced crate-wide; no FFI in core.

#![forbid(unsafe_code)]

pub mod board;
pub mod catalog;
pub mod compact;
pub mod config;
pub mod events;
pub mod faculty;
pub mod guardian;
pub mod hooks;
pub mod journal;
pub mod levenshtein;
pub mod loop_;
pub mod memory;
pub mod permissions;
pub mod providers;
pub mod router;
pub mod sandbox;
pub mod session;
pub mod session_host;
pub mod shellparse;
pub mod skills;
pub mod trust;
pub mod usage;

/// Single version source: every user agent (UA header, CLI `--version`,
/// generated docs) derives from this constant.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Canonical user agent for outbound HTTP.
pub fn user_agent() -> String {
    format!("marfim/{VERSION} (rust)")
}

/// Shared reqwest client with the canonical UA (single version source).
/// Builder failure (broken TLS backend) degrades to a plain client rather
/// than panicking at request time.
pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(user_agent())
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("config error: {0}")]
    Config(String),
    #[error("provider error: {0}")]
    Provider(String),
    #[error("permission denied: {0}")]
    Permission(String),
    #[error("session error: {0}")]
    Session(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, CoreError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_and_ua_agree() {
        let ua = user_agent();
        assert!(ua.starts_with("marfim/"), "ua: {ua}");
        assert!(ua.contains(VERSION));
    }
}
