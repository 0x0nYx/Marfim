//! Offline model catalog (P3): a versioned snapshot of the models.dev
//! directory, committed verbatim as data. Lookup gives display name,
//! context window and per-Mtok USD pricing for a provider/model pair.
//! Nothing here touches the network at runtime (D11) — refresh the
//! snapshot by re-running the fetch script and committing the diff.
//!
//! Snapshot date: 2026-11-05. Source: https://models.dev/api.json
//! (fetched live, curated to language models of the keepers below).

use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::OnceLock;

pub const SNAPSHOT_DATE: &str = "2026-11-05";
const SNAPSHOT: &str = include_str!("catalog/models-dev.snapshot.json");

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct CatalogModel {
    pub name: String,
    #[serde(rename = "context")]
    pub context_tokens: Option<u64>,
    #[serde(rename = "input_usd_per_mtok")]
    pub input_usd_per_mtok: Option<f64>,
    #[serde(rename = "output_usd_per_mtok")]
    pub output_usd_per_mtok: Option<f64>,
}

fn catalog() -> &'static BTreeMap<String, BTreeMap<String, CatalogModel>> {
    static CATALOG: OnceLock<BTreeMap<String, BTreeMap<String, CatalogModel>>> = OnceLock::new();
    // G1: no unwrap/expect — a corrupt snapshot degrades to an empty
    // catalog (lookups honestly return None), and the test suite would
    // catch a corrupt commit immediately.
    CATALOG.get_or_init(|| serde_json::from_str(SNAPSHOT).unwrap_or_default())
}

/// Map a marfim preset name to its models.dev provider id. Presets that
/// are locally hosted (ollama/vllm/lmstudio) have no catalog entry.
pub fn catalog_provider_for(preset: &str) -> Option<&'static str> {
    Some(match preset {
        "anthropic" => "anthropic",
        "openai" => "openai",
        "xai" => "xai",
        "gemini" => "google",
        "groq" => "groq",
        "openrouter" => "openrouter",
        "deepseek" => "deepseek",
        "mistral" => "mistral",
        _ => return None,
    })
}

/// Exact lookup: (provider id from `catalog_provider_for`, model id).
pub fn lookup(provider: &str, model: &str) -> Option<CatalogModel> {
    catalog().get(provider)?.get(model).cloned()
}

/// Best-effort lookup: exact match first, then unique prefix match —
/// dated variants (`claude-sonnet-4-5-20251001`) resolve for the base
/// id (`claude-sonnet-4-5`) and vice versa.
pub fn lookup_fuzzy(provider: &str, model: &str) -> Option<CatalogModel> {
    if let Some(m) = lookup(provider, model) {
        return Some(m);
    }
    let models = catalog().get(provider)?;
    let prefix = models
        .iter()
        .filter(|(k, _)| k.starts_with(model) || model.starts_with(k.as_str()))
        .collect::<Vec<_>>();
    if prefix.len() == 1 {
        Some(prefix[0].1.clone())
    } else {
        None
    }
}

pub fn providers() -> Vec<String> {
    catalog().keys().cloned().collect()
}

/// All models of one provider id (sorted by model id for stable output).
pub fn models_of(provider: &str) -> Vec<(String, CatalogModel)> {
    catalog()
        .get(provider)
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default()
}

pub fn model_count(provider: &str) -> usize {
    catalog().get(provider).map(|m| m.len()).unwrap_or(0)
}

/// W8 context promotion: the CATALOG context window for one model id,
/// scanned across every committed provider (deterministic: the snapshot
/// is a committed BTreeMap; an id present under several providers —
/// e.g. google vs google-vertex — resolves to the FIRST provider in
/// key order, which is stable within one committed snapshot).
pub fn context_of(model_id: &str) -> Option<u64> {
    for p in providers() {
        if let Some(ms) = catalog().get(&p) {
            if let Some(m) = ms.get(model_id) {
                return m.context_tokens;
            }
        }
    }
    None
}

pub fn total_model_count() -> usize {
    catalog().values().map(|m| m.len()).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_parses_and_has_expected_providers() {
        for p in ["anthropic", "openai", "xai", "google", "groq", "openrouter"] {
            assert!(model_count(p) > 0, "catalog has {p}");
        }
        assert!(total_model_count() > 100);
    }

    #[test]
    fn real_models_have_real_pricing() {
        let m = lookup("anthropic", "claude-sonnet-4-5").expect("sonnet 4.5 in catalog");
        assert_eq!(m.context_tokens, Some(200_000));
        assert_eq!(m.input_usd_per_mtok, Some(3.0));
        assert_eq!(m.output_usd_per_mtok, Some(15.0));
    }

    #[test]
    fn preset_mapping_covers_real_presets() {
        assert_eq!(catalog_provider_for("gemini"), Some("google"));
        assert_eq!(catalog_provider_for("xai"), Some("xai"));
        assert_eq!(
            catalog_provider_for("ollama"),
            None,
            "local hosts have no catalog"
        );
    }

    #[test]
    fn fuzzy_resolves_dated_variants() {
        // a dated variant id should resolve through the prefix path
        let hit = lookup_fuzzy("anthropic", "claude-haiku-4-5");
        assert!(hit.is_some(), "base id resolves");
    }

    #[test]
    fn unknown_model_is_none_not_invented() {
        assert!(lookup_fuzzy("anthropic", "definitely-not-a-model-xyz").is_none());
        assert!(lookup("nope", "also-nope").is_none());
    }
}
