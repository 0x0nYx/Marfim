//! Strict config: unknown keys are errors with did-you-mean, nested maps
//! validated recursively. Keys mirror the Node v1.2.0 surface (PARITY §7).

use crate::levenshtein::did_you_mean;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const KNOWN_KEYS: &[&str] = &[
    "model",
    "provider",
    "providers",
    "profile",
    "smallModel",
    "architectModel",
    "failover",
    "routing",
    "budget",
    "permissions",
    "sandbox",
    "bash",
    "tools",
    "mcpServers",
    "hooks",
    "lsp",
    "wal",
    "cache",
    "compaction",
    "microCompact",
    "autoCompact",
    "autoCompactThreshold",
    "autoCompactFloor",
    "steerMode",
    "stream",
    "theme",
    "env",
    "git",
    "review",
    "audit",
    "evidence",
    "evidenceGate",
    "guardian",
    "riskScores",
    "swarm",
    "cron",
    "goal",
    "plugins",
    "policy",
    "skills",
    "verify",
    "ttsr",
    "externalAgents",
    "dialectTools",
    "shield",
    "gateguard",
    "supplyChainGate",
    "mcpHealthGate",
    "unattended",
    "verbose",
    "maxTokens",
    "temperature",
    "thinkingBudget",
    "externalAgents",
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
}
