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

#[derive(Parser, Clone)]
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

    /// Model selection strategy: "chain" (default, strict order) or
    /// "bandit" (UCB1 over observed outcomes, P3).
    #[arg(long = "routing", default_value = "chain")]
    pub routing: String,

    #[arg(long = "failover")]
    pub failover: Option<String>,

    #[arg(long = "glob")]
    pub glob: Vec<String>,

    #[arg(long = "on-fail", default_value = "continue")]
    pub on_fail: String,

    #[arg(long = "port", default_value_t = 7900)]
    pub port: u16,

    #[arg(long = "host", default_value = "127.0.0.1")]
    pub host: String,

    #[arg(long = "token")]
    pub token: Option<String>,

    /// W2.3: bind a unix socket at this path instead of TCP (serve).
    /// The bearer token stays mandatory; the socket is chmod 0600.
    #[arg(long = "unix")]
    pub unix: Option<String>,

    /// W2.3: TLS certificate chain (PEM) for serve over TCP; requires
    /// --tls-key. Omit for plain HTTP.
    #[arg(long = "tls-cert")]
    pub tls_cert: Option<String>,

    /// W2.3: TLS private key (PEM); requires --tls-cert.
    #[arg(long = "tls-key")]
    pub tls_key: Option<String>,

    #[arg(long = "max-mode")]
    pub max_mode: Option<String>,

    #[arg(long = "var")]
    pub vars: Vec<String>,

    #[arg(long = "days", default_value_t = 30)]
    pub days: u32,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Clone, Subcommand)]
pub enum AgentsCmd {
    /// List known external agents and their gate state
    List,
    /// Delegate ONE prompt turn to an external agent over ACP
    Run {
        /// Profile name from the catalog (opencode, hermes)
        agent: String,
        /// The prompt (single argument; quote it)
        prompt: String,
    },
}

#[derive(Subcommand, Clone)]
pub enum Command {
    /// HTTP+SSE gateway backed by the real session actor
    Serve,
    /// ACP agent over stdio backed by the real session actor
    Acp,
    /// LSP server over stdio
    Lsp,
    /// MCP server over stdio (expose marfim tools)
    Mcp,
    /// ratatui terminal UI over the real session actor
    Tui,
    /// Inspect/configure marfim (doctor, config get/set)
    Doctor,
    /// Manage external agent profiles: list, or run <name> <prompt> to
    /// delegate one ACP turn (worktree-isolated, fail-closed gates)
    Agents {
        #[command(subcommand)]
        cmd: Option<AgentsCmd>,
    },
    /// Show session journals
    Sessions,
    /// List models from the offline models.dev catalog (--provider filters)
    Models,
    /// Internal: run <cmd>... with filesystem writes confined to --root
    /// dirs (Landlock on Linux / Seatbelt on macOS). FAIL-CLOSED: if the
    /// platform cannot enforce, the command refuses to start.
    SandboxExec {
        /// Writable roots (reads stay broad; execution inherited)
        #[arg(long = "root")]
        roots: Vec<String>,
        /// Command to exec after the sandbox is applied
        #[arg(last = true)]
        cmd: Vec<String>,
    },
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
        "gemini",
        Preset {
            // Native generative-language wire (distillation §R3): the
            // SSE endpoint is `{base}/models/{model}:streamGenerate
            // Content?alt=sse`, auth via `x-goog-api-key`.
            wire: "gemini".into(),
            base_url: "https://generativelanguage.googleapis.com/v1beta".into(),
            api_key_env: "GEMINI_API_KEY".into(),
            default_model: "gemini-2.5-pro".into(),
        },
    );
    m.insert(
        "openai-responses",
        Preset {
            // Native Responses wire (distillation §R2).
            wire: "openai-responses".into(),
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
    m.insert(
        "mistral",
        Preset {
            // W7.5: Mistral speaks the OpenAI chat-completions dialect on
            // its own host — one preset, one cassette, no second wire.
            wire: "openai".into(),
            base_url: "https://api.mistral.ai/v1".into(),
            api_key_env: "MISTRAL_API_KEY".into(),
            default_model: "mistral-large-latest".into(),
        },
    );
    m.insert(
        "google-vertex",
        Preset {
            // W7.5 (FORMER-REJECTS adoption form, OFF until selected): the
            // Gemini wire + Bearer auth on the regional Vertex endpoint.
            // The model is the FULL Vertex publisher-model id
            // (publishers/google/models/gemini-2.5-pro).
            wire: "gemini-vertex".into(),
            base_url: "https://us-central1-aiplatform.googleapis.com/v1".into(),
            api_key_env: "VERTEX_ACCESS_TOKEN".into(),
            default_model: "publishers/google/models/gemini-2.5-pro".into(),
        },
    );
    m.insert(
        "amazon-bedrock",
        Preset {
            // W7.5: SigV4-signed Bedrock invoke (non-streaming — the
            // binary event-stream decoder is the honest limitation).
            // `model` is the Bedrock model or inference-profile id.
            wire: "bedrock".into(),
            base_url: String::new(),
            api_key_env: "BEDROCK_ACCESS_KEY".into(),
            default_model: "anthropic.claude-sonnet-4-5-v2:0".into(),
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

/// The credential seam (W1.3): every api_key read goes through the
/// chain. Empty `api_key_env` presets (e.g. ollama/mock) resolve to ""
/// regardless of the chain — no credential is invented.
fn api_key_for(preset: &Preset, chain: &marfim_core::credentials::CredentialChain) -> String {
    if preset.api_key_env.is_empty() {
        String::new()
    } else {
        chain.resolve(&preset.api_key_env)
    }
}

/// W7.4: everything the chain builders need that is resolved ONCE and
/// refreshed OUTSIDE the builder (OAuth tokens refresh in the async
/// factory, where a real HTTP round-trip is possible).
pub struct ChainMaterials {
    pub oauth: Vec<Arc<marfim_core::oauth::OAuthSource>>,
    pub pool: marfim_core::oauth::CredentialPool,
}

/// Read `[credentials.oauth.<NAME>]` entries and `[credentialPools]`.
/// Both are optional; absent sections yield empty materials.
pub fn prepare_chain_materials(config: &Config) -> Result<ChainMaterials, String> {
    let mut oauth = Vec::new();
    if let Some(v) = config.get("credentials") {
        if let Some(obj) = v.as_object() {
            if let Some(oa) = obj.get("oauth") {
                let map = oa
                    .as_object()
                    .ok_or_else(|| "credentials.oauth must be an object".to_string())?;
                for (name, entry) in map {
                    oauth.push(Arc::new(marfim_core::oauth::OAuthSource::new(
                        name,
                        marfim_core::oauth::OAuthConfig::from_value(entry)?,
                    )));
                }
            }
        }
    }
    let pool = marfim_core::oauth::CredentialPool::from_config(config)?;
    Ok(ChainMaterials { oauth, pool })
}

impl ChainMaterials {
    /// The credential chains for one preset. The OAuth sources sit at the
    /// FRONT of the chain: a machine-refreshed access token must beat a
    /// possibly-stale env value FOR ITS OWN NAME (other names fall
    /// through untouched). With a pool: one chain PER immutable snapshot,
    /// snapshot in front of everything (rotation is the failover walk;
    /// no published context mutates).
    fn chains_for(
        &self,
        config: &Config,
        api_key_env: &str,
    ) -> Vec<marfim_core::credentials::CredentialChain> {
        let mut base = marfim_core::credentials::CredentialChain::standard(config);
        for s in self.oauth.iter().rev() {
            let src: Arc<dyn marfim_core::credentials::CredentialSource> = s.clone();
            base = base.with_source_front(src);
        }
        let snaps = self.pool.snapshots(api_key_env);
        if snaps.is_empty() {
            return vec![base];
        }
        snaps
            .into_iter()
            .map(|v| {
                base.clone().with_source_front(Arc::new(
                    marfim_core::credentials::FixedSource::new(api_key_env, v),
                ))
            })
            .collect()
    }
}

/// Build a real provider from a preset through an explicit credential
/// chain (W1.3). D11: the mock wire only exists via the explicit `mock`
/// name — nothing falls back to it silently.
pub fn build_provider_with(
    preset: &Preset,
    model: &str,
    chain: &marfim_core::credentials::CredentialChain,
) -> Result<Arc<dyn Provider>, String> {
    let api_key = api_key_for(preset, chain);
    match preset.wire.as_str() {
        "anthropic" => Ok(Arc::new(marfim_core::providers::Anthropic {
            client: marfim_core::http_client(),
            base_url: std::env::var("ANTHROPIC_BASE_URL")
                .unwrap_or_else(|_| preset.base_url.clone()),
            api_key,
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
                api_key,
                model: model.to_string(),
                max_retries: 3,
            }))
        }
        "openai-responses" => {
            let base = if preset.base_url.is_empty() {
                std::env::var("OPENAI_BASE_URL")
                    .unwrap_or_else(|_| "https://api.openai.com/v1".into())
            } else {
                preset.base_url.clone()
            };
            Ok(Arc::new(marfim_core::providers::OpenAiResponses {
                client: marfim_core::http_client(),
                base_url: base,
                api_key,
                model: model.to_string(),
                max_retries: 3,
            }))
        }
        "gemini" => {
            let base = if preset.base_url.is_empty() {
                std::env::var("GEMINI_BASE_URL").unwrap_or_else(|_| {
                    "https://generativelanguage.googleapis.com/v1beta".into()
                })
            } else {
                preset.base_url.clone()
            };
            Ok(Arc::new(marfim_core::providers::Gemini {
                client: marfim_core::http_client(),
                base_url: base,
                api_key,
                model: model.to_string(),
                max_retries: 3,
                auth_bearer: false,
            }))
        }
        "gemini-vertex" => {
            // W7.5 (FORMER-REJECTS adoption form): the Vertex preset rides
            // the SAME Gemini wire with `Authorization: Bearer` — the
            // access token comes from the credential chain (W1.3/W7.4:
            // env `VERTEX_ACCESS_TOKEN`, `[credentials]`, or an
            // `[credentials.oauth]` refresh flow). The preset exists and
            // works, but it is OFF until explicitly selected.
            let base = if preset.base_url.is_empty() {
                std::env::var("VERTEX_BASE_URL").unwrap_or_else(|_| {
                    "https://us-central1-aiplatform.googleapis.com/v1".into()
                })
            } else {
                preset.base_url.clone()
            };
            Ok(Arc::new(marfim_core::providers::Gemini {
                client: marfim_core::http_client(),
                base_url: base,
                api_key,
                model: model.to_string(),
                max_retries: 3,
                auth_bearer: true,
            }))
        }
        "bedrock" => {
            // W7.5: SigV4-signed invoke (clean-room, vector-tested). The
            // three AWS credentials ride the credential chain under the
            // names BEDROCK_ACCESS_KEY / BEDROCK_SECRET_KEY /
            // BEDROCK_SESSION_TOKEN (the token optional).
            let access = chain.resolve("BEDROCK_ACCESS_KEY");
            let secret = chain.resolve("BEDROCK_SECRET_KEY");
            if access.is_empty() || secret.is_empty() {
                return Err(
                    "bedrock preset needs BEDROCK_ACCESS_KEY and BEDROCK_SECRET_KEY \
                     (env, [credentials], or a credential pool)"
                        .to_string(),
                );
            }
            let region = std::env::var("BEDROCK_REGION").unwrap_or_else(|_| "us-east-1".into());
            Ok(Arc::new(marfim_core::providers::bedrock::Bedrock {
                client: marfim_core::http_client(),
                base_url: preset.base_url.clone(),
                region,
                model_id: model.to_string(),
                access_key: access,
                secret_key: secret,
                session_token: {
                    let t = chain.resolve("BEDROCK_SESSION_TOKEN");
                    (!t.is_empty()).then_some(t)
                },
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
            "provider wire `{other}` is not wired in this build (known wires: anthropic, openai, openai-responses, gemini, gemini-vertex, bedrock, mock)"
        )),
    }
}

/// Pre-W1.3 signature: environment-only credentials.
pub fn build_provider(preset: &Preset, model: &str) -> Result<Arc<dyn Provider>, String> {
    build_provider_with(
        preset,
        model,
        &marfim_core::credentials::CredentialChain::env_only(),
    )
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
    /// "chain" | "bandit" (P3 UCB1 routing).
    pub routing: String,
    // Sampling overrides: consumed by the Harness (config truth) — every
    // primary request carries maxTokens/temperature/thinkingBudget.
    pub max_tokens: Option<u64>,
    pub temperature: Option<f64>,
    pub thinking_budget: Option<u64>,
    /// Canonical reasoning-effort ladder (distillation §R1).
    pub effort: Option<marfim_core::providers::effort::Effort>,
    pub compact_threshold_tokens: u64,
    pub stream: bool,
    // Consumed by the TUI palette (`Palette::detect`) and /theme.
    pub theme: String,
    // Consumed by the headless/REPL printers (full tool calls + results).
    pub verbose: bool,
    pub verify_commands: Option<Vec<String>>,
    pub stop_on_tool_error: bool,
    // Consumed by build_session: the Bash tool's default timeout.
    pub bash_default_timeout_ms: u64,
    /// W1.4: the attested sandbox level (consumed by build_session).
    pub sandbox_level: marfim_core::sandbox::SandboxLevel,
    pub cwd: PathBuf,
    pub title: Option<String>,
    pub resume: Option<String>,
    pub output_format: String,
    pub prompt: Option<String>,
    pub prompt_vars: Vec<(String, String)>,
    /// §privacy (claude-code `privacy` mod): the account address the guard
    /// protects; empty = guard disabled.
    pub account_email: String,
    /// §P (claude-code 2.1.319): asks become denies, the turn continues.
    pub deny_instead_of_prompting: bool,
    /// W7.2: the `[cache]` config section (ttl / cachedContentName / warm).
    pub cache: marfim_core::providers::cache::CacheSettings,
    /// W7.5: `responses.customTools` (default off).
    pub allow_custom_tools: bool,
    /// W8: the compaction stage order (default promote→micro→summarize).
    pub compaction_strategies: Vec<marfim_core::compact::Strategy>,
}

/// Permission rules from the config `permissions` section only (no CLI):
/// used by non-interactive surfaces like `marfim mcp`.
pub fn rules_from_config(config: &Config) -> Vec<Rule> {
    let mut rules: Vec<Rule> = Vec::new();
    let Some(perms) = config.get("permissions") else {
        return rules;
    };
    for (key, decision) in [
        ("deny", Decision::Deny),
        ("ask", Decision::Ask),
        ("allow", Decision::Allow),
    ] {
        if let Some(list) = perms.get(key).and_then(|v| v.as_array()) {
            for item in list {
                if let Some(spec) = item.as_str() {
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
    rules
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

    // --- responses (config section; W7.5) — the custom-grammar adoption
    // form: `customTools = true` passes `custom` tool defs through the
    // Responses wire verbatim (default OFF = function-tool fallback).
    let mut allow_custom_tools = false;
    if let Some(v) = config.get("responses") {
        let obj = v
            .as_object()
            .ok_or_else(|| "responses section must be an object".to_string())?;
        if let Some(ct) = obj.get("customTools") {
            allow_custom_tools = ct
                .as_bool()
                .ok_or_else(|| "responses.customTools must be a boolean".to_string())?;
        }
    }

    // --- cache (config section; W7.2) — unknown keys fail closed via
    // validate_consumed_sections, type errors fail closed here.
    let cache = marfim_core::providers::cache::CacheSettings::from_config(config)?;

    // --- permissions (config section + --permission-mode + --glob) ---
    let mut permission_mode = cli.permission_mode.as_str();
    let mut rules: Vec<Rule> = Vec::new();
    let mut working_globs = cli.glob.clone();
    let mut deny_instead_of_prompting = false;
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
        deny_instead_of_prompting = perms
            .get("denyInsteadOfPrompting")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
    }
    let mode = Mode::parse(permission_mode).unwrap_or(Mode::Default);
    let _ = config.get("tools");

    // --- §privacy + §P permission-engine options ---
    let account_email = config
        .get("accountEmail")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

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
    // Distillation §R1: canonical effort ladder. Unknown strings fail
    // closed with a named error instead of silently degrading.
    let effort = match config.get("effort") {
        None | Some(serde_json::Value::Null) => None,
        Some(v) => {
            let s = v.as_str().unwrap_or("");
            Some(marfim_core::providers::effort::Effort::parse(s).ok_or_else(|| {
                format!(
                    "config `effort`: unknown level `{s}` (known: minimal, low, medium, high, xhigh, max)"
                )
            })?)
        }
    };
    let compact_threshold_tokens = config
        .get("autoCompactThreshold")
        .and_then(|v| v.as_u64())
        .unwrap_or(120_000);
    // W8 (§C ordered strategies): the compaction stage ORDER is config —
    // default promote → micro → summarize (identical to the pre-W8
    // ladder). Unknown stages and the empty list fail closed.
    let mut compaction_strategies: Vec<marfim_core::compact::Strategy> =
        marfim_core::compact::DEFAULT_STRATEGIES.to_vec();
    if let Some(v) = config.get("compaction") {
        let obj = v
            .as_object()
            .ok_or_else(|| "compaction section must be an object".to_string())?;
        if let Some(st) = obj.get("strategies") {
            let list = st
                .as_array()
                .ok_or_else(|| "compaction.strategies must be an array".to_string())?;
            compaction_strategies = marfim_core::compact::parse_strategies(list)?;
        }
    }
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
    // W1.4: sandbox.level is REAL config now (enforced | appUid | none;
    // unknown values fail closed — an invalid attestation is never a
    // silent Enforced).
    let sandbox_level = match config
        .get("sandbox")
        .and_then(|s| s.get("level"))
        .and_then(|v| v.as_str())
    {
        None => marfim_core::sandbox::SandboxLevel::Enforced,
        Some(raw) => match marfim_core::sandbox::SandboxLevel::parse(raw) {
            Some(l) => l,
            None => {
                return Err(format!(
                    "config: sandbox.level `{raw}` is not one of enforced|appUid|none"
                ))
            }
        },
    };
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
        routing: cli.routing.clone(),
        max_tokens,
        temperature,
        thinking_budget,
        effort,
        compact_threshold_tokens,
        stream,
        theme,
        verbose,
        verify_commands,
        stop_on_tool_error,
        bash_default_timeout_ms,
        sandbox_level,
        cwd,
        title: cli.title.clone(),
        resume: cli.resume.clone().filter(|s| !s.is_empty()),
        output_format: cli.output_format.clone(),
        prompt: cli.prompt.clone(),
        prompt_vars,
        account_email,
        deny_instead_of_prompting,
        cache,
        allow_custom_tools,
        compaction_strategies,
    })
}

/// Build the full primary chain (D11): [main entry] + failover tail.
pub fn build_primary_chain(
    settings: &EffectiveSettings,
    config: &Config,
) -> Result<Vec<ModelEntry>, String> {
    let materials = prepare_chain_materials(config)?;
    build_primary_chain_with(settings, config, &materials)
}

/// The materials-taking variant: the factory prepares the materials once,
/// refreshes the OAuth tokens (async), then builds every chain.
pub fn build_primary_chain_with(
    settings: &EffectiveSettings,
    config: &Config,
    materials: &ChainMaterials,
) -> Result<Vec<ModelEntry>, String> {
    // W1.3: env -> config -> memory pool (identical to env-only when the
    // config has no `credentials` table and the pool is empty). W7.4:
    // a pooled credential expands to one immutable snapshot per chain
    // entry — rotation IS the failover walk.
    let preset = resolve_preset(&settings.provider_name, config)?;
    let mut chain = Vec::new();
    for creds in materials.chains_for(config, &preset.api_key_env) {
        chain.push(ModelEntry {
            model: settings.model.clone(),
            provider: build_provider_with(&preset, &settings.model, &creds)?,
            priced: true, // pricedness enforced by the CostBook at usage time
        });
    }
    for (prov, model) in &settings.failover {
        let p = resolve_preset(prov, config)?;
        let m = if model.is_empty() {
            &p.default_model
        } else {
            model
        };
        for creds in materials.chains_for(config, &p.api_key_env) {
            chain.push(ModelEntry {
                model: m.clone(),
                provider: build_provider_with(&p, m, &creds)?,
                priced: true,
            });
        }
    }
    Ok(chain)
}

pub fn build_small_chain(
    settings: &EffectiveSettings,
    config: &Config,
) -> Result<Vec<ModelEntry>, String> {
    let materials = prepare_chain_materials(config)?;
    build_small_chain_with(settings, config, &materials)
}

pub fn build_small_chain_with(
    settings: &EffectiveSettings,
    config: &Config,
    materials: &ChainMaterials,
) -> Result<Vec<ModelEntry>, String> {
    let preset = resolve_preset(&settings.provider_name, config)?;
    let mut out = Vec::new();
    for creds in materials.chains_for(config, &preset.api_key_env) {
        out.push(ModelEntry {
            model: settings.small_model.clone(),
            provider: build_provider_with(&preset, &settings.small_model, &creds)?,
            priced: true,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    /// D13 coverage lists live with the tests that enforce them: parsed-
    /// but-unread flags/keys are bugs, not dead-code suppressions.
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
        "routing",
        "failover",
        "glob",
        "on_fail",
        "port",
        "host",
        "token",
        "unix",
        "tls_cert",
        "tls_key",
        "max_mode",
        "vars",
        "days",
        "help",
        "version",
    ];

    pub const CONSUMED_CONFIG_KEYS: &[&str] = &[
        "model",
        "provider",
        "providers",
        "credentials",     // W1.3: CredentialChain::standard reads [credentials]
        "cache",           // W7.2: CacheSettings::from_config reads [cache]
        "credentialPools", // W7.4: CredentialPool::from_config reads [credentialPools]
        "responses",       // W7.5: responses.customTools (custom-grammar adoption)
        "compaction",      // W8: compaction.strategies (ordered stages)
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
    ];

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

    // W1.3: the credential seam — empty api_key_env presets stay empty,
    // named presets resolve through the chain.
    #[test]
    fn api_key_seam_honors_chain_and_empty_env() {
        struct Fixed(&'static str, &'static str);
        impl marfim_core::credentials::CredentialSource for Fixed {
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
        let mut p = builtin_presets()
            .remove("anthropic")
            .unwrap_or_else(|| panic!("anthropic preset"));
        let chain = marfim_core::credentials::CredentialChain::empty()
            .with_source(std::sync::Arc::new(Fixed("ANTHROPIC_API_KEY", "sk-test")));
        assert_eq!(api_key_for(&p, &chain), "sk-test");
        // empty api_key_env (e.g. mock/ollama) stays empty — no invention
        p.api_key_env = String::new();
        assert_eq!(api_key_for(&p, &chain), "");
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
    fn pool_expands_chains_per_snapshot_and_oauth_sits_in_front() {
        // W7.4: a pooled name yields one immutable-snapshot chain per
        // value; an unpooled name yields exactly one chain; the OAuth
        // source resolves its own name ahead of env/config and nothing
        // else.
        let config = Config::from_json_str(
            r#"{"credentialPools": {"POOL_KEY": {"values": ["one", "two"]}},
                "credentials": {"oauth": {"OAUTH_KEY": {"tokenUrl": "http://127.0.0.1:1/t",
                                                          "clientId": "cid",
                                                          "refreshToken": "rt"}}}}"#,
        )
        .unwrap();
        let materials = prepare_chain_materials(&config).unwrap();
        assert_eq!(materials.oauth.len(), 1);

        let pooled = materials.chains_for(&config, "POOL_KEY");
        assert_eq!(pooled.len(), 2, "one chain per snapshot");
        assert_eq!(pooled[0].resolve("POOL_KEY"), "one");
        assert_eq!(pooled[1].resolve("POOL_KEY"), "two");
        // Debug never prints values — only source names.
        let dbg = format!("{pooled:?}");
        assert!(!dbg.contains("\"one\"") && !dbg.contains("\"two\""));

        let plain = materials.chains_for(&config, "OTHER_KEY");
        assert_eq!(plain.len(), 1, "no pool for this name → single chain");
        assert_eq!(
            plain[0].resolve("OAUTH_KEY"),
            "",
            "no seed + no refresh = honest empty"
        );

        // The OAuth source answers ahead of env for ITS OWN NAME only.
        std::env::set_var("OAUTH_KEY", "stale-from-env");
        let seeded = ChainMaterials {
            oauth: vec![Arc::new(marfim_core::oauth::OAuthSource::new(
                "OAUTH_KEY",
                marfim_core::oauth::OAuthConfig {
                    token_url: "http://127.0.0.1:1/t".into(),
                    client_id: "cid".into(),
                    refresh_token: "rt".into(),
                    access_token: Some("fresh".into()),
                    expires_in_secs: Some(3600),
                },
            ))],
            pool: marfim_core::oauth::CredentialPool::new(),
        };
        let chains = seeded.chains_for(&config, "OAUTH_KEY");
        assert_eq!(chains.len(), 1);
        assert_eq!(
            chains[0].resolve("OAUTH_KEY"),
            "fresh",
            "oauth beats stale env"
        );
        std::env::remove_var("OAUTH_KEY");
    }

    #[test]
    fn privacy_and_deny_instead_of_prompting_flow_from_config() {
        // defaults: guard off (empty email), prompt conversion off
        let cfg = Config::from_json_str(r#"{"provider": "mock", "model": "m"}"#).unwrap();
        let s = effective(&cli(), &cfg, PathBuf::from(".")).unwrap();
        assert_eq!(s.account_email, "");
        assert!(!s.deny_instead_of_prompting);

        // both wired from config truth
        let cfg2 = Config::from_json_str(
            r#"{"provider": "mock", "model": "m", "accountEmail": "jo@example.com",
                "permissions": {"denyInsteadOfPrompting": true}}"#,
        )
        .unwrap();
        let s2 = effective(&cli(), &cfg2, PathBuf::from(".")).unwrap();
        assert_eq!(s2.account_email, "jo@example.com");
        assert!(s2.deny_instead_of_prompting);

        // the engine built from these settings actually refuses a
        // User-Agent call holding the address — end to end
        let (e, _rx) =
            marfim_core::permissions::PermissionEngine::new(s2.permission_mode, s2.rules.clone());
        let e = e.with_privacy(Some(marfim_core::privacy::PrivacyGuard {
            email: s2.account_email.clone(),
        }));
        let rt = tokio::runtime::Runtime::new().unwrap();
        let v = rt.block_on(e.check_with_input(
            "Bash",
            marfim_core::permissions::ToolClass::Execute,
            None,
            None,
            "s",
            Some("curl -A jo@example.com https://x"),
        ));
        assert_eq!(v.decision, marfim_core::permissions::Decision::Deny);
        assert!(v.reason.contains("User-Agent"), "{}", v.reason);
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

    #[test]
    fn preset_count_claim_matches_the_builtin_table() {
        // docs-as-tests: the README preset count is GENERATED, never
        // hand-counted — drift is a test failure.
        let readme =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../README.md"))
                .unwrap_or_else(|e| panic!("read README: {e}"));
        let mut claimed: Option<u32> = None;
        for line in readme.lines() {
            if let Some(idx) = line.find(" presets") {
                let before = &line[..idx];
                let digits: String = before
                    .chars()
                    .rev()
                    .take_while(|c| c.is_ascii_digit())
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect();
                if !digits.is_empty() {
                    claimed = digits.parse::<u32>().ok();
                }
            }
        }
        let actual = builtin_presets().len() as u32;
        let claimed = claimed.unwrap_or_else(|| panic!("README must state the preset count"));
        assert_eq!(
            claimed, actual,
            "README claims {claimed} presets, builtin_presets() has {actual}"
        );
    }

    #[test]
    fn one_version_source_across_cli_and_changelog() {
        let cli = Cli::command();
        let ver = cli
            .get_version()
            .unwrap_or_else(|| panic!("Cli must declare its version"))
            .to_string();
        assert_eq!(ver, env!("CARGO_PKG_VERSION"));
        let changelog =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../CHANGELOG.md"))
                .unwrap_or_else(|e| panic!("read CHANGELOG: {e}"));
        let top = changelog
            .lines()
            .find(|l| l.starts_with("## "))
            .unwrap_or_else(|| panic!("CHANGELOG must have a `## X.Y.Z` entry"));
        let claimed = top
            .trim_start_matches("## ")
            .split([' ', '-'])
            .next()
            .unwrap_or("")
            .trim()
            .trim_start_matches('v');
        assert_eq!(
            claimed,
            env!("CARGO_PKG_VERSION"),
            "CHANGELOG top `{claimed}` must equal workspace version"
        );
    }

    #[test]
    fn every_subcommand_declares_help() {
        let cmd = Cli::command();
        for sub in cmd.get_subcommands() {
            let name = sub.get_name().to_string();
            assert!(
                sub.get_about().is_some() || sub.get_long_about().is_some(),
                "subcommand `{name}` ships without help text"
            );
            for sub2 in sub.get_subcommands() {
                let n2 = format!("{name} {}", sub2.get_name());
                assert!(
                    sub2.get_about().is_some() || sub2.get_long_about().is_some(),
                    "subcommand `{n2}` ships without help text"
                );
            }
        }
    }

    fn make_req() -> marfim_core::providers::ChatRequest {
        marfim_core::providers::ChatRequest::default()
    }
}
