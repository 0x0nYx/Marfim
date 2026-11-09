//! Strict config: unknown keys are errors with did-you-mean, nested maps
//! validated recursively. Keys mirror the Node v1.2.0 surface (PARITY §7).

use crate::levenshtein::did_you_mean;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const KNOWN_KEYS: &[&str] = &[
    "model",
    "provider",
    "providers",
    "smallModel",
    "architectModel",
    "reviewModel",
    "failover",
    "budget",
    "permissions",
    "sandbox",
    "bash",
    "tools",
    "mcpServers",
    "hooks",
    "lsp",
    "wal",
    "compaction",
    "autoCompactThreshold",
    "steerMode",
    "stream",
    "theme",
    "env",
    "skills",
    "verify",
    "unattended",
    "verbose",
    "maxTokens",
    "temperature",
    "thinkingBudget",
    "effort",
    "accountEmail",
    "credentials",
    "credentialPools",
    "cache",
    "responses",
];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(flatten)]
    raw: BTreeMap<String, serde_json::Value>,
}

impl Config {
    pub fn from_value(v: serde_json::Value) -> crate::Result<Self> {
        let obj = v
            .as_object()
            .ok_or_else(|| crate::CoreError::Config("config root must be an object".into()))?;
        let known: Vec<&str> = KNOWN_KEYS.to_vec();
        for k in obj.keys() {
            if !KNOWN_KEYS.contains(&k.as_str()) {
                let hint = did_you_mean(k, &known)
                    .map(|s| format!(" (did you mean `{s}`?)"))
                    .unwrap_or_default();
                return Err(crate::CoreError::Config(format!(
                    "unknown config key `{k}`{hint}"
                )));
            }
        }
        Ok(Self {
            raw: obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        })
    }

    pub fn from_json_str(s: &str) -> crate::Result<Self> {
        let v: serde_json::Value = serde_json::from_str(s)?;
        Self::from_value(v)
    }

    pub fn get(&self, key: &str) -> Option<&serde_json::Value> {
        self.raw.get(key)
    }

    pub fn get_str(&self, key: &str, default: &str) -> String {
        self.raw
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or(default)
            .to_string()
    }

    pub fn get_bool(&self, key: &str, default: bool) -> bool {
        self.raw
            .get(key)
            .and_then(|v| v.as_bool())
            .unwrap_or(default)
    }

    pub fn get_f64(&self, key: &str, default: f64) -> f64 {
        self.raw
            .get(key)
            .and_then(|v| v.as_f64())
            .unwrap_or(default)
    }

    pub fn set(&mut self, key: &str, value: serde_json::Value) -> crate::Result<()> {
        if !KNOWN_KEYS.contains(&key) {
            let hint = did_you_mean(key, KNOWN_KEYS)
                .map(|s| format!(" (did you mean `{s}`?)"))
                .unwrap_or_default();
            return Err(crate::CoreError::Config(format!(
                "unknown config key `{key}`{hint}"
            )));
        }
        self.raw.insert(key.into(), value);
        Ok(())
    }

    pub fn as_json(&self) -> serde_json::Value {
        serde_json::Value::Object(
            self.raw
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        )
    }

    // ---------------------------------------------------------- D13
    /// Strict per-section validation of the keys Marfim actually consumes.
    /// Unknown keys INSIDE a consumed section are errors (not just
    /// top-level keys) — fail loud, never silently ignore.
    pub fn validate_consumed_sections(&self) -> crate::Result<()> {
        let nested: &[(&str, &[&str])] = &[
            (
                "providers",
                &["baseUrl", "apiKeyEnv", "model", "pricing", "wire"],
            ),
            ("mcpServers", &["command", "args", "env", "url"]),
            (
                "permissions",
                &[
                    "mode",
                    "deny",
                    "ask",
                    "allow",
                    "workingGlobs",
                    "denyInsteadOfPrompting",
                ],
            ),
            ("bash", &["defaultTimeoutMs", "envAllowlist"]),
            ("hooks", &["PreToolUse", "PostToolUse"]),
            ("verify", &["commands"]),
            ("cache", &["ttl", "cachedContentName", "warm"]),
            ("credentialPools", &["values"]),
            ("responses", &["customTools"]),
            ("compaction", &["strategies"]),
        ];
        for (section, allowed) in nested {
            if let Some(v) = self.raw.get(*section) {
                let Some(map) = v.as_object() else {
                    return Err(crate::CoreError::Config(format!(
                        "`{section}` must be an object"
                    )));
                };
                // providers/mcpServers are maps of sub-configs; permissions
                // etc. are flat option objects.
                if matches!(*section, "providers" | "mcpServers") {
                    for (name, sub) in map {
                        let Some(sobj) = sub.as_object() else {
                            return Err(crate::CoreError::Config(format!(
                                "`{section}.{name}` must be an object"
                            )));
                        };
                        for k in sobj.keys() {
                            if !allowed.contains(&k.as_str()) {
                                let hint = did_you_mean(k, allowed)
                                    .map(|s| format!(" (did you mean `{s}`?)"))
                                    .unwrap_or_default();
                                return Err(crate::CoreError::Config(format!(
                                    "unknown key `{k}` in `{section}.{name}`{hint}"
                                )));
                            }
                        }
                    }
                } else {
                    for k in map.keys() {
                        if !allowed.contains(&k.as_str()) {
                            let hint = did_you_mean(k, allowed)
                                .map(|s| format!(" (did you mean `{s}`?)"))
                                .unwrap_or_default();
                            return Err(crate::CoreError::Config(format!(
                                "unknown key `{k}` in `{section}`{hint}"
                            )));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// D13: layered config — `~/.marfim/config.toml` first, then
    /// `.marfim/config.toml` from the working directory (cwd wins).
    /// The cwd layer is a TRUST GATE: it only loads when the directory is
    /// trusted at Workspace level or above; an untrusted cwd must never be
    /// able to configure providers or permissions.
    pub fn load_layered(
        home_dir: &std::path::Path,
        cwd: &std::path::Path,
        cwd_trust: crate::trust::Trust,
    ) -> crate::Result<(Self, Vec<String>)> {
        let mut out = Self::default();
        let mut loaded = Vec::new();
        let home_cfg = home_dir.join(".marfim").join("config.toml");
        if home_cfg.is_file() {
            let text = std::fs::read_to_string(&home_cfg)?;
            let layer = Self::from_toml_str(&text)
                .map_err(|e| crate::CoreError::Config(format!("{}: {e}", home_cfg.display())))?;
            out.merge(layer);
            loaded.push(home_cfg.display().to_string());
        }
        if matches!(
            cwd_trust,
            crate::trust::Trust::Workspace | crate::trust::Trust::Trusted
        ) {
            let cwd_cfg = cwd.join(".marfim").join("config.toml");
            if cwd_cfg.is_file() {
                let text = std::fs::read_to_string(&cwd_cfg)?;
                let layer = Self::from_toml_str(&text)
                    .map_err(|e| crate::CoreError::Config(format!("{}: {e}", cwd_cfg.display())))?;
                out.merge(layer);
                loaded.push(cwd_cfg.display().to_string());
            }
        }
        out.validate_consumed_sections()?;
        Ok((out, loaded))
    }

    pub fn from_toml_str(s: &str) -> crate::Result<Self> {
        let table: toml::Table = toml::from_str(s)
            .map_err(|e| crate::CoreError::Config(format!("invalid TOML: {e}")))?;
        let v = serde_json::to_value(table)?;
        Self::from_value(v)
    }

    /// Later layers win on scalar conflicts; objects merge shallowly.
    pub fn merge(&mut self, other: Self) {
        for (k, v) in other.raw {
            self.raw.insert(k, v);
        }
    }

    pub fn keys(&self) -> Vec<String> {
        self.raw.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CoreError;

    #[test]
    fn accepts_known_keys() {
        let c = Config::from_json_str(r#"{"model": "m1", "verbose": true}"#).unwrap();
        assert_eq!(c.get_str("model", "none"), "m1");
        assert!(c.get_bool("verbose", false));
    }

    #[test]
    fn unknown_key_errors_with_did_you_mean() {
        let err = Config::from_json_str(r#"{"modle": 1}"#).unwrap_err();
        match err {
            CoreError::Config(m) => {
                assert!(m.contains("did you mean `model`"), "{m}");
            }
            other => panic!("wrong error: {other}"),
        }
    }

    #[test]
    fn totally_unknown_key_errors_without_hint() {
        let err = Config::from_json_str(r#"{"qqqqqq": 1}"#).unwrap_err();
        assert!(matches!(err, CoreError::Config(_)));
    }

    #[test]
    fn set_rejects_unknown() {
        let mut c = Config::default();
        assert!(c.set("notakey", 1.into()).is_err());
        assert!(c.set("model", "x".into()).is_ok());
    }

    // ---------------------------------------------------------- D13
    #[test]
    fn layered_config_cwd_overrides_home_and_trust_gate_blocks_cwd_layer() {
        let home = marfim_testkit::unique_dir("marfim-cfg-h");
        let cwd = marfim_testkit::unique_dir("marfim-cwd");
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&cwd);
        std::fs::create_dir_all(home.join(".marfim")).unwrap();
        std::fs::create_dir_all(cwd.join(".marfim")).unwrap();
        std::fs::write(
            home.join(".marfim").join("config.toml"),
            "model = \"home-model\"\nverbose = true\n",
        )
        .unwrap();
        std::fs::write(
            cwd.join(".marfim").join("config.toml"),
            "model = \"cwd-model\"\n",
        )
        .unwrap();

        // trusted cwd: both layers load, cwd wins
        let (cfg, loaded) =
            Config::load_layered(&home, &cwd, crate::trust::Trust::Workspace).unwrap();
        assert_eq!(cfg.get_str("model", ""), "cwd-model");
        assert!(cfg.get_bool("verbose", false));
        assert_eq!(loaded.len(), 2);

        // untrusted cwd: the cwd layer MUST NOT load (trust gate)
        let (cfg2, loaded2) =
            Config::load_layered(&home, &cwd, crate::trust::Trust::Untrusted).unwrap();
        assert_eq!(cfg2.get_str("model", ""), "home-model");
        assert_eq!(loaded2.len(), 1);
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn invalid_config_file_fails_loud_with_its_path_and_is_never_rewritten() {
        // distilled: claude-code 2.1.318 config-integrity wave — a damaged
        // config must fail the start with a readable error naming the file,
        // never be silently replaced by defaults, and never be mutated.
        let home = marfim_testkit::unique_dir("marfim-cfg-bad");
        let cwd = marfim_testkit::unique_dir("marfim-cwd-bad");
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&cwd);
        std::fs::create_dir_all(home.join(".marfim")).unwrap();
        std::fs::create_dir_all(cwd.join(".marfim")).unwrap();
        let bad = home.join(".marfim").join("config.toml");
        std::fs::write(&bad, "model = \"unterminated\nbroken =\n").unwrap();
        let before = std::fs::read(&bad).unwrap();

        let err = Config::load_layered(&home, &cwd, crate::trust::Trust::Untrusted)
            .expect_err("invalid TOML must fail the load");
        let msg = format!("{err}");
        assert!(
            msg.contains(&home_cfg_display(&home)),
            "path in error: {msg}"
        );
        assert!(msg.contains("invalid TOML"), "{msg}");
        // the damaged file is untouched — restore instructions are honest
        assert_eq!(std::fs::read(&bad).unwrap(), before);
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&cwd);
    }

    fn home_cfg_display(home: &std::path::Path) -> String {
        home.join(".marfim")
            .join("config.toml")
            .display()
            .to_string()
    }

    #[test]
    fn nested_section_keys_are_strict() {
        let c =
            Config::from_toml_str("[mcpServers.files]\ncommand = \"mcp-fileserver\"\nbadKey = 1\n")
                .unwrap();
        let err = c.validate_consumed_sections().unwrap_err();
        assert!(format!("{err}").contains("badKey"), "{err}");

        let c2 = Config::from_toml_str(
            "[providers.xai]\nbaseUrl = \"https://api.x.ai/v1\"\napiKeyEnv = \"XAI_API_KEY\"\n",
        )
        .unwrap();
        assert!(c2.validate_consumed_sections().is_ok());
    }

    #[test]
    fn toml_config_parses_strictly() {
        assert!(Config::from_toml_str("modle = \"x\"").is_err());
        let c = Config::from_toml_str("[permissions]\nmode = \"yolo\"\n").unwrap();
        assert_eq!(c.get_str("permissions.mode", ""), ""); // top-level access only
        assert_eq!(c.get("permissions").unwrap()["mode"], "yolo");
    }
}
