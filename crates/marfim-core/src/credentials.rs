//! Credential sourcing (W1.3): ONE trait, ONE precedence chain.
//!
//! Chain: **env → config → memory pool** (first non-empty wins; an empty
//! final answer is honest — no fabrication, providers fail their own auth
//! visibly). The pool exists for embeddings that cannot use environment
//! variables (W4.1 `marfim_set_secret` from the mobile Keystore bridge).
//!
//! Bounds: secrets live only inside the resolved provider struct; they
//! are never logged, never journaled, and `sanitize_env` (child process
//! environments) is deliberately untouched by this module.

use crate::config::Config;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

pub trait CredentialSource: Send + Sync {
    /// Resolve the credential named `name` (conventionally the preset's
    /// `api_key_env`). `None` = this source has no such credential.
    fn resolve(&self, name: &str) -> Option<String>;
    fn name(&self) -> &'static str;
}

/// Source 1: the process environment (pre-W1.3 behavior, first in the
/// chain so existing setups are byte-identical).
pub struct EnvSource;

impl CredentialSource for EnvSource {
    fn resolve(&self, name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|v| !v.is_empty())
    }

    fn name(&self) -> &'static str {
        "env"
    }
}

/// Source 2: the trust-gated config layer (`[credentials]` table, keyed
/// by the same names as the env vars). D13 already refuses untrusted cwd
/// layers, so a workspace can never inject a credential this way.
#[derive(Debug, Clone, Default)]
pub struct ConfigSource {
    credentials: BTreeMap<String, String>,
}

impl ConfigSource {
    /// Snapshot the `credentials` table out of the config. Unknown top
    /// level keys are refused by the strict config validator; unknown
    /// entries INSIDE the table are simply credentials we don't consume
    /// (resolve() never matches them — fail-closed by lookup).
    pub fn from_config(config: &Config) -> Self {
        let mut credentials = BTreeMap::new();
        if let Some(serde_json::Value::Object(map)) = config.get("credentials") {
            for (k, v) in map {
                if let Some(s) = v.as_str() {
                    credentials.insert(k.clone(), s.to_string());
                }
            }
        }
        Self { credentials }
    }
}

impl CredentialSource for ConfigSource {
    fn resolve(&self, name: &str) -> Option<String> {
        self.credentials
            .get(name)
            .filter(|v| !v.is_empty())
            .cloned()
    }

    fn name(&self) -> &'static str {
        "config"
    }
}

/// Source 3: the in-memory pool. Process-wide, never persisted, never
/// logged; populated by embedding surfaces (FFI `marfim_set_secret`).
#[derive(Default)]
pub struct MemoryPool {
    entries: Mutex<BTreeMap<String, String>>,
}

impl MemoryPool {
    pub fn new() -> Self {
        Self::default()
    }

    /// Store/overwrite one credential. Values are plain strings owned by
    /// the pool; callers must never log them.
    pub fn set(&self, name: &str, value: String) {
        self.entries
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(name.to_string(), value);
    }

    /// The process-wide pool ( lazily created; one per process).
    pub fn global() -> &'static MemoryPool {
        static POOL: OnceLock<MemoryPool> = OnceLock::new();
        POOL.get_or_init(MemoryPool::new)
    }
}

impl CredentialSource for MemoryPool {
    fn resolve(&self, name: &str) -> Option<String> {
        self.entries
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(name)
            .filter(|v| !v.is_empty())
            .cloned()
    }

    fn name(&self) -> &'static str {
        "memory"
    }
}

/// Adapter: the process-wide pool as a chain source (the pool itself is
/// borrowed `&'static`; Arc needs an owned value).
struct GlobalPool;

impl CredentialSource for GlobalPool {
    fn resolve(&self, name: &str) -> Option<String> {
        MemoryPool::global().resolve(name)
    }

    fn name(&self) -> &'static str {
        "memory"
    }
}

/// The precedence chain. Resolution order = construction order; the
/// standard chain is env → config → memory pool. Clone is cheap (every
/// source is an Arc) — the W7.4 pool expansion clones the base chain per
/// snapshot and prepends one FixedSource.
#[derive(Clone)]
pub struct CredentialChain {
    sources: Vec<Arc<dyn CredentialSource>>,
}

/// W7.4: one immutable pool snapshot as a chain source — the expansion
/// unit of a credential pool (rotation = the failover walk over one
/// snapshot per chain entry; no published context is ever mutated).
#[derive(Clone)]
pub struct FixedSource {
    name: String,
    value: std::sync::Arc<str>,
}

impl FixedSource {
    pub fn new(name: &str, value: std::sync::Arc<str>) -> Self {
        Self {
            name: name.to_string(),
            value,
        }
    }
}

impl CredentialSource for FixedSource {
    fn resolve(&self, name: &str) -> Option<String> {
        if name == self.name {
            Some(self.value.to_string())
        } else {
            None
        }
    }

    fn name(&self) -> &'static str {
        "pool-snapshot"
    }
}

impl std::fmt::Debug for CredentialChain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // NEVER the values — only the source names.
        let names: Vec<&str> = self.sources.iter().map(|s| s.name()).collect();
        write!(f, "CredentialChain({names:?})")
    }
}

impl Default for CredentialChain {
    fn default() -> Self {
        Self::env_only()
    }
}

impl CredentialChain {
    /// Empty chain (resolve always returns "") — for tests that inject
    /// their own sources.
    pub fn empty() -> Self {
        Self { sources: vec![] }
    }

    /// Environment only: byte-identical to the pre-W1.3 resolution.
    pub fn env_only() -> Self {
        Self {
            sources: vec![Arc::new(EnvSource)],
        }
    }

    /// env → config → memory pool (the W1.3 standard chain).
    pub fn standard(config: &Config) -> Self {
        Self {
            sources: vec![
                Arc::new(EnvSource),
                Arc::new(ConfigSource::from_config(config)),
                Arc::new(GlobalPool),
            ],
        }
    }

    /// Append a source at the END of the chain (lowest precedence).
    pub fn with_source(mut self, source: Arc<dyn CredentialSource>) -> Self {
        self.sources.push(source);
        self
    }

    /// Prepend a source at the FRONT of the chain (highest precedence) —
    /// W7.4 pool snapshots pin ONE value for a pooled name ahead of env
    /// and config (an explicit pool is deliberate).
    pub fn with_source_front(mut self, source: Arc<dyn CredentialSource>) -> Self {
        self.sources.insert(0, source);
        self
    }

    /// Source names in precedence order (Debug/testing only — never
    /// values).
    pub fn source_names(&self) -> Vec<&'static str> {
        self.sources.iter().map(|s| s.name()).collect()
    }

    /// First non-empty credential in chain order; "" when none (the
    /// provider request then fails its own auth — visible, honest).
    /// A source that resolves to an EMPTY string is skipped, never
    /// sticky — an empty answer must not shadow a lower source.
    pub fn resolve(&self, name: &str) -> String {
        for s in &self.sources {
            if let Some(v) = s.resolve(name) {
                if !v.is_empty() {
                    return v;
                }
            }
        }
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(&'static str, &'static str);
    impl CredentialSource for Fixed {
        fn resolve(&self, name: &str) -> Option<String> {
            if name == self.0 {
                Some(self.1.to_string())
            } else {
                None
            }
        }
        fn name(&self) -> &'static str {
            "fixed"
        }
    }

    #[test]
    fn chain_precedence_first_non_empty_wins() {
        let chain = CredentialChain::empty()
            .with_source(Arc::new(Fixed("K", "first")))
            .with_source(Arc::new(Fixed("K", "second")));
        assert_eq!(chain.resolve("K"), "first");
        assert_eq!(chain.resolve("MISSING"), "");
    }

    #[test]
    fn chain_empty_values_are_skipped_not_sticky() {
        // A source that RESOLVES but to an empty string must not shadow
        // a lower source (that would make the pool unreachable).
        struct EmptyK;
        impl CredentialSource for EmptyK {
            fn resolve(&self, _name: &str) -> Option<String> {
                Some(String::new())
            }
            fn name(&self) -> &'static str {
                "empty"
            }
        }
        let chain = CredentialChain::empty()
            .with_source(Arc::new(EmptyK))
            .with_source(Arc::new(Fixed("K", "real")));
        assert_eq!(chain.resolve("K"), "real");
    }

    #[test]
    fn config_source_extracts_credentials_table() {
        let config = Config::from_value(serde_json::json!({
            "credentials": {
                "ANTHROPIC_API_KEY": "sk-config",
                "EMPTY": "",
                "NOT_A_STRING": 42
            }
        }))
        .unwrap_or_else(|e| panic!("config: {e}"));
        let src = ConfigSource::from_config(&config);
        assert_eq!(
            src.resolve("ANTHROPIC_API_KEY").as_deref(),
            Some("sk-config")
        );
        // empty and non-string entries are not credentials
        assert_eq!(src.resolve("EMPTY"), None);
        assert_eq!(src.resolve("NOT_A_STRING"), None);
        assert_eq!(src.resolve("UNRELATED"), None);
    }

    #[test]
    fn memory_pool_set_and_resolve() {
        let pool = MemoryPool::new();
        assert_eq!(pool.resolve("POOL_KEY"), None);
        pool.set("POOL_KEY", "pool-secret".to_string());
        assert_eq!(pool.resolve("POOL_KEY").as_deref(), Some("pool-secret"));
        pool.set("POOL_KEY", "rotated".to_string());
        assert_eq!(pool.resolve("POOL_KEY").as_deref(), Some("rotated"));
    }

    #[test]
    fn env_source_ignores_empty_and_missing() {
        // A nonsense name never exists; no env mutation (races).
        assert_eq!(EnvSource.resolve("MARFIM_NO_SUCH_CREDENTIAL_XYZ"), None);
    }

    #[test]
    fn debug_never_contains_values() {
        let chain = CredentialChain::empty().with_source(Arc::new(Fixed("K", "super-secret")));
        let rendered = format!("{chain:?}");
        assert!(!rendered.contains("super-secret"), "{rendered}");
        assert!(rendered.contains("fixed"));
    }
}
