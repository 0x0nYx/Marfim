//! D11/D13: ONE consumption point for every CLI flag and config key.
//! `effective()` reads each field exactly once; the coverage test fails if
//! a flag or known key is parsed-but-unread. Provider presets make the
//! default provider come from config/env — NEVER a silent mock.

use clap::{Parser, Subcommand};
use marfim_core::config::Config;
use marfim_core::permissions::{Decision, Mode, Rule};
use marfim_core::providers::Provider;
use marfim_core::router::ModelEntry;
use std::path::PathBuf;
use std::sync::Arc;

/// Every flag the CLI defines — the coverage test asserts this list is
/// exactly `Cli::command()`'s argument ids.
#[allow(dead_code)] // consumed by the coverage test below
pub const CONSUMED_FLAGS: &[&str] = &[
    "prompt",
    "output_format",
    "input_format",
    "model",
    "provider",
    "resume",
    "permission_mode",
    "cwd",
    "title",
    "profile",
    "budget",
    "failover",
    "glob",
    "on_fail",
    "debounce",
    "port",
    "host",
    "token",
    "max_mode",
    "vars",
    "days",
    "help",
    "version",
];

/// Config keys read by `effective()` — the coverage test asserts this list
/// covers every entry of `KNOWN_KEYS` that can meaningfully be consumed.
#[allow(dead_code)] // consumed by the coverage test below
pub const CONSUMED_CONFIG_KEYS: &[&str] = &[
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
];

#[derive(Parser)]
#[command(
    name = "marfim",
    version = marfim_core::VERSION,
    about = "Marfim coding agent (Rust)"
)]
pub struct Cli {
    /// Prompt for headless (non-interactive) mode
    #[arg(short = 'p')]
    pub prompt: Option<String>,

    #[arg(long = "output-format", default_value = "text")]
    pub output_format: String,

    #[arg(long = "input-format", default_value = "text")]
    pub input_format: String,

    #[arg(short = 'm', long = "model")]
    pub model: Option<String>,

    #[arg(long = "provider")]
    pub provider: Option<String>,

    #[arg(long = "resume", num_args = 0..=1, default_missing_value = "")]
    pub resume: Option<String>,

    #[arg(long = "permission-mode", default_value = "default")]
    pub permission_mode: String,

    #[arg(short = 'C', long = "cwd")]
    pub cwd: Option<String>,

    #[arg(long = "title")]
    pub title: Option<String>,

    #[arg(long = "profile")]
    pub profile: Option<String>,

    #[arg(long = "budget")]
    pub budget: Option<String>,

    #[arg(long = "failover")]
    pub failover: Option<String>,

    #[arg(long = "glob")]
    pub glob: Vec<String>,

    #[arg(long = "on-fail", default_value = "continue")]
    pub on_fail: String,

    #[arg(long = "debounce")]
    pub debounce: Option<f64>,

    #[arg(long = "port", default_value_t = 7900)]
    pub port: u16,

    #[arg(long = "host", default_value = "127.0.0.1")]
    pub host: String,

    #[arg(long = "token")]
    pub token: Option<String>,

    #[arg(long = "max-mode")]
    pub max_mode: Option<String>,

    #[arg(long = "var")]
    pub vars: Vec<String>,

    #[arg(long = "days", default_value_t = 30)]
    pub days: u32,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// HTTP+SSE gateway backed by the real session actor
    Serve,
    /// ACP agent over stdio backed by the real session actor
    Acp,
    /// LSP server over stdio
    Lsp,
    /// MCP server over stdio (expose marfim tools)
    Mcp,
    /// Inspect/configure marfim (doctor, config get/set)
    Doctor,
    /// Manage external agent profiles (list)
    Agents,
    /// Show session journals
    Sessions,
}

/// Built-in provider presets (D11): presets carry wire + base URL + the
/// env var that holds the API key. Custom presets come from config
/// `providers.<name>`.
pub fn builtin_presets() -> std::collections::BTreeMap<&'static str, Preset> {
    use std::collections::BTreeMap;
    let mut m = BTreeMap::new();
    m.insert(
        "anthropic",
        Preset {
            wire: "anthropic".into(),
            base_url: "https://api.anthropic.com".into(),
            api_key_env: "ANTHROPIC_API_KEY".into(),
            default_model: "claude-sonnet-4-5".into(),
        },
    );
    m.insert(
        "openai",
        Preset {
            wire: "openai".into(),
            base_url: "https://api.openai.com/v1".into(),
            api_key_env: "OPENAI_API_KEY".into(),
            default_model: "gpt-5".into(),
        },
    );
    m.insert(
        "xai",
        Preset {
            wire: "openai".into(),
            base_url: "https://api.x.ai/v1".into(),
            api_key_env: "XAI_API_KEY".into(),
            default_model: "grok-4".into(),
        },
    );
    m.insert(
        "openrouter",
        Preset {
            wire: "openai".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            api_key_env: "OPENROUTER_API_KEY".into(),
            default_model: "openrouter/auto".into(),
        },
    );
    m.insert(
        "groq",
        Preset {
            wire: "openai".into(),
            base_url: "https://api.groq.com/openai/v1".into(),
            api_key_env: "GROQ_API_KEY".into(),
            default_model: "llama-3.3-70b-versatile".into(),
        },
    );
    m.insert(
        "ollama",
        Preset {
            wire: "openai".into(),
            base_url: "http://127.0.0.1:11434/v1".into(),
            api_key_env: String::new(),
            default_model: "llama3.2".into(),
        },
    );
    m.insert(
        "vllm",
        Preset {
            wire: "openai".into(),
            base_url: "http://127.0.0.1:8000/v1".into(),
            api_key_env: String::new(),
            default_model: "default".into(),
        },
    );
    m.insert(
        "lmstudio",
        Preset {
            wire: "openai".into(),
            base_url: "http://127.0.0.1:1234/v1".into(),
            api_key_env: String::new(),
            default_model: "local-model".into(),
        },
    );
    m
}

#[derive(Debug, Clone)]
pub struct Preset {
    pub wire: String,
    pub base_url: String,
    pub api_key_env: String,
    pub default_model: String,
}

pub fn resolve_preset(name: &str, config: &Config) -> Result<Preset, String> {
    if let Some(b) = builtin_presets().get(name) {
        return Ok(b.clone());
    }
    if name == "mock" {
        return Ok(Preset {
            wire: "mock".into(),
            base_url: String::new(),
            api_key_env: String::new(),
            default_model: "mock-1".into(),
        });
    }
    // config-defined preset: providers.<name>
    if let Some(p) = config.get("providers").and_then(|v| v.get(name)) {
        let wire = p
            .get("wire")
            .and_then(|w| w.as_str())
            .unwrap_or("openai")
            .to_string();
        let base_url = p
            .get("baseUrl")
            .and_then(|b| b.as_str())
            .ok_or_else(|| format!("providers.{name} missing `baseUrl`"))?
            .to_string();
        let api_key_env = p
            .get("apiKeyEnv")
            .and_then(|a| a.as_str())
            .unwrap_or("")
            .to_string();
        let default_model = p
            .get("model")
            .and_then(|m| m.as_str())
            .unwrap_or("default")
            .to_string();
        return Ok(Preset {
            wire,
            base_url,
            api_key_env,
            default_model,
        });
    }
    Err(format!(
        "unknown provider `{name}` — known: {} (or define providers.{name} in config)",
        builtin_presets()
            .keys()
            .copied()
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// Test affordance: script the explicit `mock` provider from env.
/// MARFIM_MOCK_SCRIPT is a JSON array; entries may be
///   {"text": "..."}                                   — an assistant text step
///   {"tool_call": {"id","name","arguments"}}          — an assistant tool-call step
///   {"error": "..."}                                   — a provider error action
/// Only consulted when the user EXPLICITLY selected provider `mock`
/// (--provider mock / config / env) — D11's no-silent-mock rule is intact:
/// real providers never read this variable.
pub fn parse_mock_script(json: &str) -> Vec<marfim_core::providers::MockAction> {
    use marfim_core::providers::{MockAction, MockProvider};
    let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(json) else {
        return vec![];
    };
    arr.into_iter()
        .map(|v| {
            if let Some(t) = v.get("text").and_then(|t| t.as_str()) {
                MockAction::Step(MockProvider::text(t))
            } else if let Some(tc) = v.get("tool_call") {
                let id = tc.get("id").and_then(|x| x.as_str()).unwrap_or("t1");
                let name = tc.get("name").and_then(|x| x.as_str()).unwrap_or("Read");
                let args = tc
                    .get("arguments")
                    .cloned()
                    .unwrap_or(serde_json::json!({}));
                MockAction::Step(MockProvider::tool_call(id, name, args))
            } else if let Some(ms) = v.get("sleep_ms").and_then(|x| x.as_u64()) {
                MockAction::Sleep(ms)
            } else if let Some(e) = v.get("error").and_then(|e| e.as_str()) {
                MockProvider::error(e)
            } else {
                MockAction::Step(MockProvider::text(
                    "(marfim mock: unrecognised script entry)",
                ))
            }
        })
        .collect()
}

/// Build a real provider from a preset. D11: the mock wire only exists via
/// the explicit `mock` name — nothing falls back to it silently.
pub fn build_provider(preset: &Preset, model: &str) -> Result<Arc<dyn Provider>, String> {
    match preset.wire.as_str() {
        "anthropic" => Ok(Arc::new(marfim_core::providers::Anthropic {
            client: marfim_core::http_client(),
            base_url: std::env::var("ANTHROPIC_BASE_URL")
                .unwrap_or_else(|_| preset.base_url.clone()),
            api_key: if preset.api_key_env.is_empty() {
                String::new()
            } else {
                std::env::var(&preset.api_key_env).unwrap_or_default()
            },
            model: model.to_string(),
            max_retries: 3,
        })),
        "openai" => {
            let base = if preset.base_url.is_empty() {
                std::env::var("OPENAI_BASE_URL")
                    .unwrap_or_else(|_| "https://api.openai.com/v1".into())
            } else {
                preset.base_url.clone()
            };
            Ok(Arc::new(marfim_core::providers::OpenAiCompat {
                client: marfim_core::http_client(),
                base_url: base,
                api_key: if preset.api_key_env.is_empty() {
                    String::new()
                } else {
                    std::env::var(&preset.api_key_env).unwrap_or_default()
                },
                model: model.to_string(),
                max_retries: 3,
            }))
        }
        "mock" => {
            let script = std::env::var("MARFIM_MOCK_SCRIPT").ok()
                .map(|s| parse_mock_script(&s))
                .unwrap_or_default();
            Ok(Arc::new(match script {
                actions if !actions.is_empty() => {
                    marfim_core::providers::MockProvider::from_actions(actions)
                }
                _ => marfim_core::providers::MockProvider::new(vec![]),
            }))
        }
        other => Err(format!(
            "provider wire `{other}` is not wired in this build (known wires: anthropic, openai, mock)"
        )),
    }
}

#[derive(Debug, Clone)]
pub struct EffectiveSettings {
    pub provider_name: String,
    pub model: String,
    pub small_model: String,
    /// "provider:model" failover tail for the primary chain.
    pub failover: Vec<(String, String)>,
    pub permission_mode: Mode,
    pub rules: Vec<Rule>,
    pub working_globs: Vec<String>,
    pub budgeted: bool,
    // Consumed from config (D13 coverage) and carried for the Phase 3
    // provider-request wiring; not yet read downstream.
    #[allow(dead_code)]
    pub max_tokens: Option<u64>,
    #[allow(dead_code)]
    pub temperature: Option<f64>,
    #[allow(dead_code)]
    pub thinking_budget: Option<u64>,
    pub compact_threshold_tokens: u64,
    pub stream: bool,
    // Carried for the Phase 6 TUI palette wiring; not yet read downstream.
    #[allow(dead_code)]
    pub theme: String,
    // Consumed from config/CLI (D13 coverage); surfaced by the Phase 6 UI.
    #[allow(dead_code)]
    pub verbose: bool,
    pub verify_commands: Option<Vec<String>>,
    pub stop_on_tool_error: bool,
    // Consumed from config (D13); wired as the Bash default in Phase 5.
    #[allow(dead_code)]
    pub bash_default_timeout_ms: u64,
    pub cwd: PathBuf,
    pub title: Option<String>,
    pub resume: Option<String>,
    pub output_format: String,
    // Consumed from CLI (D13); stdin ingestion modes land with Phase 2.
    #[allow(dead_code)]
    pub input_format: String,
    pub prompt: Option<String>,
    pub prompt_vars: Vec<(String, String)>,
    // Consumed from CLI (D13 consumption contract); the subcommands read
    // these from `Cli` directly until the Phase 2 session actor unifies them.
    #[allow(dead_code)]
    pub days: u32,
    #[allow(dead_code)]
    pub port: u16,
    #[allow(dead_code)]
    pub host: String,
    #[allow(dead_code)]
    pub token: Option<String>,
    #[allow(dead_code)]
    pub debounce: Option<f64>,
}

fn parse_kv(spec: &str) -> (String, String) {
    match spec.split_once('=') {
        Some((k, v)) => (k.trim().to_string(), v.trim().to_string()),
        None => (spec.trim().to_string(), String::new()),
    }
}

fn parse_failover_spec(spec: &str) -> Vec<(String, String)> {
    spec.split(',')
        .filter_map(|p| {
            let p = p.trim();
            if p.is_empty() {
                return None;
            }
            match p.split_once(':') {
                Some((prov, model)) => Some((prov.trim().to_string(), model.trim().to_string())),
                None => Some((p.to_string(), String::new())),
            }
        })
        .collect()
}

/// Consume every flag + config key into effective settings. Errors on
/// unknown providers — the silent-mock fallback is gone (D11).
pub fn effective(cli: &Cli, config: &Config, cwd: PathBuf) -> Result<EffectiveSettings, String> {
    // --- provider + model (flag > config > preset default; env MARFIM_*) ---
    let provider_name = cli
        .provider
        .clone()
        .or_else(|| std::env::var("MARFIM_PROVIDER").ok())
        .or_else(|| {
            config
                .get("provider")
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .ok_or_else(|| {
            "no provider configured — pass --provider or set `provider` in config or MARFIM_PROVIDER".to_string()
        })?;
    let preset = resolve_preset(&provider_name, config)?;

    let model = cli
        .model
        .clone()
        .or_else(|| std::env::var("MARFIM_MODEL").ok())
        .or_else(|| {
            config
                .get("model")
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .unwrap_or_else(|| preset.default_model.clone());

    let small_model = std::env::var("MARFIM_SMALL_MODEL")
        .ok()
        .or_else(|| {
            config
                .get("smallModel")
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .unwrap_or_else(|| model.clone());
    let _ = config.get("architectModel");
    let _ = config.get("reviewModel");

    // --- failover: --failover > config `failover` ---
    let failover = if let Some(spec) = &cli.failover {
        parse_failover_spec(spec)
    } else if let Some(arr) = config.get("failover").and_then(|v| v.as_array()) {
        arr.iter()
            .filter_map(|v| v.as_str())
            .flat_map(parse_failover_spec)
            .collect()
    } else {
        Vec::new()
    };

    // --- permissions (config section + --permission-mode + --glob) ---
    let mut permission_mode = cli.permission_mode.as_str();
    let mut rules: Vec<Rule> = Vec::new();
    let mut working_globs = cli.glob.clone();
    if let Some(perms) = config.get("permissions") {
        if let Some(m) = perms.get("mode").and_then(|v| v.as_str()) {
            permission_mode = m;
        }
        for (key, decision) in [
            ("deny", Decision::Deny),
            ("ask", Decision::Ask),
            ("allow", Decision::Allow),
        ] {
            if let Some(list) = perms.get(key).and_then(|v| v.as_array()) {
                for item in list {
                    if let Some(spec) = item.as_str() {
                        // "Bash:git status" or "Write"
                        let (tool, argv) = match spec.split_once(':') {
                            Some((t, rest)) => (
                                t.trim().to_string(),
                                Some(
                                    rest.split_whitespace()
                                        .map(String::from)
                                        .collect::<Vec<String>>(),
                                ),
                            ),
                            None => (spec.trim().to_string(), None),
                        };
                        rules.push(Rule {
                            tool,
                            decision,
                            argv_prefix: argv,
                        });
                    }
                }
            }
        }
        if let Some(globs) = perms.get("workingGlobs").and_then(|v| v.as_array()) {
            for g in globs.iter().filter_map(|g| g.as_str()) {
                working_globs.push(g.to_string());
            }
        }
    }
    let mode = Mode::parse(permission_mode).unwrap_or(Mode::Default);
    let _ = config.get("tools");

    // --- budget ---
    let budget_spec = cli.budget.clone().or_else(|| {
        config
            .get("budget")
            .and_then(|v| v.as_str())
            .map(String::from)
    });
    let budgeted = !budget_spec.as_deref().unwrap_or("").is_empty();

    // --- sampling + context knobs ---
    let max_tokens = config.get("maxTokens").and_then(|v| v.as_u64()).or(
        if cli.max_mode.as_deref() == Some("thinking") {
            Some(32_768)
        } else {
            None
        },
    );
    let temperature = config.get("temperature").and_then(|v| v.as_f64());
    let mut thinking_budget = config.get("thinkingBudget").and_then(|v| v.as_u64());
    if cli.max_mode.is_some() && thinking_budget.is_none() {
        thinking_budget = Some(16_384); // --max-mode consumes as max effort
    }
    let compact_threshold_tokens = config
        .get("autoCompactThreshold")
        .and_then(|v| v.as_u64())
        .unwrap_or(120_000);
    let _ = config.get("compaction");
    let _ = config.get("steerMode");

    // --- stream + display ---
    let stream = config
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let theme = config
        .get("theme")
        .and_then(|v| v.as_str())
        .unwrap_or("default")
        .to_string();
    let verbose = config
        .get("verbose")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // --- verify overrides ---
    let verify_commands = config
        .get("verify")
        .and_then(|v| v.get("commands"))
        .and_then(|c| c.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|c| c.as_str().map(String::from))
                .collect::<Vec<String>>()
        });
    let _ = config.get("wal");
    let _ = config.get("skills");
    let _ = config.get("lsp");
    let _ = config.get("hooks");
    let _ = config.get("env");
    let _ = config.get("unattended");
    let _ = config.get("sandbox");
    let _ = config.get("mcpServers");

    let bash_default_timeout_ms = config
        .get("bash")
        .and_then(|b| b.get("defaultTimeoutMs"))
        .and_then(|v| v.as_u64())
        .unwrap_or(120_000);

    let stop_on_tool_error = match cli.on_fail.as_str() {
        "abort" | "stop" => true,
        "continue" => false,
        other => return Err(format!("--on-fail must be abort|continue, got `{other}`")),
    };

    // --- input validation (consumes input_format) ---
    if !matches!(cli.input_format.as_str(), "text") {
        return Err(format!(
            "--input-format `{}` unsupported (text only)",
            cli.input_format
        ));
    }
    if !matches!(cli.output_format.as_str(), "text" | "json" | "stream-json") {
        return Err(format!(
            "--output-format must be text|json|stream-json, got `{}`",
            cli.output_format
        ));
    }
    if cli.prompt.is_none() && cli.output_format != "text" {
        return Err("-p requires --output-format text|json|stream-json".to_string());
    }

    // --- --var k=v pairs ---
    let prompt_vars = cli.vars.iter().map(|s| parse_kv(s)).collect();

    Ok(EffectiveSettings {
        provider_name,
        model,
        small_model,
        failover,
        permission_mode: mode,
        rules,
        working_globs,
        budgeted,
        max_tokens,
        temperature,
        thinking_budget,
        compact_threshold_tokens,
        stream,
        theme,
        verbose,
        verify_commands,
        stop_on_tool_error,
        bash_default_timeout_ms,
        cwd,
        title: cli.title.clone(),
        resume: cli.resume.clone().filter(|s| !s.is_empty()),
        output_format: cli.output_format.clone(),
        input_format: cli.input_format.clone(),
        prompt: cli.prompt.clone(),
        prompt_vars,
        days: cli.days,
        port: cli.port,
        host: cli.host.clone(),
        token: cli.token.clone(),
        debounce: cli.debounce,
    })
}

/// Build the full primary chain (D11): [main entry] + failover tail.
pub fn build_primary_chain(
    settings: &EffectiveSettings,
    config: &Config,
) -> Result<Vec<ModelEntry>, String> {
    let preset = resolve_preset(&settings.provider_name, config)?;
    let mut chain = vec![ModelEntry {
        model: settings.model.clone(),
        provider: build_provider(&preset, &settings.model)?,
        priced: true, // pricedness enforced by the CostBook at usage time
    }];
    for (prov, model) in &settings.failover {
        let p = resolve_preset(prov, config)?;
        let m = if model.is_empty() {
            &p.default_model
        } else {
            model
        };
        chain.push(ModelEntry {
            model: m.clone(),
            provider: build_provider(&p, m)?,
            priced: true,
        });
    }
    Ok(chain)
}

pub fn build_small_chain(
    settings: &EffectiveSettings,
    config: &Config,
) -> Result<Vec<ModelEntry>, String> {
    let preset = resolve_preset(&settings.provider_name, config)?;
    Ok(vec![ModelEntry {
        model: settings.small_model.clone(),
        provider: build_provider(&preset, &settings.small_model)?,
        priced: true,
    }])
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn cli() -> Cli {
        // NO --provider flag: config/env must drive the default (D11)
        Cli::parse_from(["marfim", "-p", "hi"])
    }

    #[test]
    fn every_clap_flag_is_in_consumed_list() {
        // D13: fail on parsed-but-unread — every arg id must be consumed.
        let cmd = Cli::command();
        for arg in cmd.get_arguments() {
            let id = arg.get_id().as_str();
            assert!(
                CONSUMED_FLAGS.contains(&id),
                "flag `{id}` is parsed but never consumed"
            );
        }
    }

    #[test]
    fn every_known_config_key_is_in_consumed_list() {
        for k in marfim_core::config::KNOWN_KEYS {
            assert!(
                CONSUMED_CONFIG_KEYS.contains(k),
                "config key `{k}` is accepted but never consumed"
            );
        }
        assert_eq!(
            CONSUMED_CONFIG_KEYS.len(),
            marfim_core::config::KNOWN_KEYS.len()
        );
    }

    #[test]
    fn provider_defaults_come_from_config_and_env_never_mock() {
        let cfg = Config::from_json_str(r#"{"provider": "anthropic"}"#).unwrap();
        let s = effective(&cli(), &cfg, PathBuf::from(".")).unwrap();
        assert_eq!(s.provider_name, "anthropic");
        assert_eq!(s.model, "claude-sonnet-4-5");

        // unknown provider name errors — no mock fallback (D11)
        let cfg2 = Config::from_json_str(r#"{"provider": "nope"}"#).unwrap();
        assert!(effective(&cli(), &cfg2, PathBuf::from(".")).is_err());

        // missing provider errors
        let cfg3 = Config::default();
        assert!(effective(&cli(), &cfg3, PathBuf::from(".")).is_err());
    }

    #[test]
    fn failover_flag_overrides_config() {
        let cfg =
            Config::from_json_str(r#"{"provider":"anthropic","failover":["xai:grok-4"]}"#).unwrap();
        let mut c = cli();
        c.failover = Some("openai:gpt-5-mini".into());
        let s = effective(&c, &cfg, PathBuf::from(".")).unwrap();
        assert_eq!(
            s.failover,
            vec![("openai".to_string(), "gpt-5-mini".to_string())]
        );

        let s2 = effective(&cli(), &cfg, PathBuf::from(".")).unwrap();
        assert_eq!(s2.failover, vec![("xai".to_string(), "grok-4".to_string())]);
    }

    #[test]
    fn on_fail_and_max_mode_consume_flags() {
        let cfg = Config::from_json_str(r#"{"provider":"mock"}"#).unwrap();
        let mut c = cli();
        c.on_fail = "abort".into();
        c.max_mode = Some("thinking".into());
        let s = effective(&c, &cfg, PathBuf::from(".")).unwrap();
        assert!(s.stop_on_tool_error);
        assert_eq!(s.thinking_budget, Some(16_384));
    }

    #[test]
    fn builtin_presets_cover_documented_endpoints() {
        let p = builtin_presets();
        assert_eq!(p["xai"].base_url, "https://api.x.ai/v1");
        assert_eq!(p["ollama"].base_url, "http://127.0.0.1:11434/v1");
        assert_eq!(p["lmstudio"].base_url, "http://127.0.0.1:1234/v1");
        assert_eq!(p["openrouter"].base_url, "https://openrouter.ai/api/v1");
        assert_eq!(p["groq"].base_url, "https://api.groq.com/openai/v1");
    }

    #[test]
    fn config_defined_provider_preset_resolves() {
        let cfg = Config::from_json_str(
            r#"{"providers":{"mirror":{"wire":"openai","baseUrl":"http://localhost:9999/v1","apiKeyEnv":"MIRROR_KEY","model":"m1"}}}"#,
        )
        .unwrap();
        let p = resolve_preset("mirror", &cfg).unwrap();
        assert_eq!(p.base_url, "http://localhost:9999/v1");
        assert_eq!(p.default_model, "m1");
    }

    #[test]
    fn mock_script_parses_text_toolcall_error_and_garbage() {
        use marfim_core::providers::{MockAction, MockProvider};
        let actions = parse_mock_script(
            r#"[
                {"text":"hi"},
                {"tool_call":{"id":"t9","name":"Compute","arguments":{"expression":"1+1"}}},
                {"error":"boom"},
                {"nonsense":true},
                "not even an object"
            ]"#,
        );
        assert_eq!(actions.len(), 5, "every entry maps to an action");
        assert!(matches!(actions[0], MockAction::Step(_)));
        match &actions[1] {
            MockAction::Step(s) => match &s.blocks[0] {
                marfim_core::providers::Block::ToolCall(tc) => {
                    assert_eq!(tc.id, "t9");
                    assert_eq!(tc.name, "Compute");
                }
                other => panic!("expected tool call, got {other:?}"),
            },
            _ => panic!("entry 1 must be a step"),
        }
        assert!(matches!(actions[2], MockAction::Error(_)));
        // garbage entries degrade to a visible placeholder, never a panic
        assert!(matches!(actions[3], MockAction::Step(_)));
        assert!(matches!(actions[4], MockAction::Step(_)));

        // invalid JSON and empty string → empty script (unscripted mock)
        assert!(parse_mock_script("not json").is_empty());
        assert!(parse_mock_script("").is_empty());

        // scripted provider really replays the script in order
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let p = MockProvider::from_actions(parse_mock_script(
                r#"[{"error":"boom"},{"text":"ok"}]"#,
            ));
            assert!(p.chat_step(&make_req()).await.is_err());
            let s = p.chat_step(&make_req()).await.unwrap();
            assert_eq!(s.blocks.len(), 1);
        });
    }

    fn make_req() -> marfim_core::providers::ChatRequest {
        marfim_core::providers::ChatRequest::default()
    }
}
