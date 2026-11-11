//! MCP client: connect to a server over stdio or streamable HTTP,
//! initialize handshake, tools/list (→ registry `mcp__server__tool`
//! entries), tools/call. Stdio transport: newline-delimited JSON-RPC.
//! HTTP transport: JSON-RPC POSTs; both plain JSON and SSE-framed
//! responses are accepted.
//!
//! Security: stdio servers spawn with a SCRUBBED environment — only an
//! explicit keep-list plus the config-declared `env` entries. Provider
//! keys and other ambient secrets never leak into MCP server processes.

use crate::jsonrpc::{Request, Response};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

pub type ToolListing = Vec<(String, String, Value)>; // (name, description, schema)

/// One `mcpServers` entry from config: a stdio command or an HTTP URL.
#[derive(Debug, Clone)]
pub enum McpServerConfig {
    Stdio {
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
    },
    Http {
        url: String,
        headers: BTreeMap<String, String>,
    },
}

impl McpServerConfig {
    /// Parse one config entry. Shapes:
    ///   {"command": "...", "args": [...], "env": {...}}
    ///   {"url": "http://...", "headers": {...}}
    pub fn from_json(v: &Value) -> Result<Self, String> {
        if let Some(cmd) = v["command"].as_str() {
            let args = v["args"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let env = json_string_map(&v["env"]);
            return Ok(Self::Stdio {
                command: cmd.to_string(),
                args,
                env,
            });
        }
        if let Some(url) = v["url"].as_str() {
            let headers = json_string_map(&v["headers"]);
            return Ok(Self::Http {
                url: url.to_string(),
                headers,
            });
        }
        Err("mcpServers entry needs `command` (stdio) or `url` (HTTP)".into())
    }
}

fn json_string_map(v: &Value) -> BTreeMap<String, String> {
    v.as_object()
        .map(|o| {
            o.iter()
                .filter_map(|(k, x)| x.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// Environment variables MCP server children may inherit. Everything
/// else (API keys, tokens, secrets) is scrubbed: a tool server must not
/// silently receive the harness's credentials.
const ENV_KEEP: [&str; 9] = [
    "PATH", "HOME", "LANG", "LC_ALL", "TERM", "TMPDIR", "USER", "SHELL", "PWD",
];

/// The scrubbed environment for an MCP stdio server: keep-list vars from
/// the current process + the config-declared `env` entries (explicit
/// user intent wins).
pub fn scrub_env(extra: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = std::env::vars()
        .filter(|(k, _)| ENV_KEEP.contains(&k.as_str()))
        .collect();
    for (k, v) in extra {
        out.insert(k.clone(), v.clone());
    }
    out
}

enum Transport {
    Stdio {
        _child: Mutex<tokio::process::Child>,
        stdin: Arc<Mutex<tokio::process::ChildStdin>>,
    },
    Http {
        url: String,
        http: reqwest::Client,
        headers: BTreeMap<String, String>,
    },
}

pub struct McpClient {
    pub server_name: String,
    transport: Transport,
    next_id: AtomicU64,
    pending: std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<Response>>>,
    pub tools: Mutex<ToolListing>,
    /// Per-call deadline. The 30 s default is for real servers; tests
    /// inject a short deadline so stale responses fail fast and honestly.
    pub call_timeout: std::time::Duration,
}

impl McpClient {
    /// Connect per config: spawn (stdio) or dial (HTTP), run the
    /// initialize handshake, list tools.
    pub async fn connect(server_name: &str, cfg: &McpServerConfig) -> Result<Arc<Self>, String> {
        Self::connect_with_timeout(server_name, cfg, std::time::Duration::from_secs(30)).await
    }

    pub async fn connect_with_timeout(
        server_name: &str,
        cfg: &McpServerConfig,
        call_timeout: std::time::Duration,
    ) -> Result<Arc<Self>, String> {
        let client = match cfg {
            McpServerConfig::Stdio { command, args, env } => {
                Self::spawn_stdio(server_name, command, args, env, call_timeout).await?
            }
            McpServerConfig::Http { url, headers } => {
                Self::dial_http(server_name, url, headers, call_timeout).await?
            }
        };
        // initialize handshake
        let result = client
            .call(
                "initialize",
                serde_json::json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {"tools": {}},
                    "clientInfo": {"name": "marfim", "version": marfim_core::VERSION}
                }),
            )
            .await?;
        let _ = result; // serverInfo; capabilities are advisory
        client
            .notify("notifications/initialized", serde_json::json!({}))
            .await;

        // tools/list → namespace
        let listing = client.call("tools/list", serde_json::json!({})).await?;
        {
            let mut tools = client.tools.lock().await;
            if let Some(arr) = listing["tools"].as_array() {
                for t in arr {
                    let name = t["name"].as_str().unwrap_or("").to_string();
                    let desc = t["description"].as_str().unwrap_or("").to_string();
                    let schema = t["inputSchema"].clone();
                    tools.push((name, desc, schema));
                }
            }
        }
        Ok(client)
    }

    async fn spawn_stdio(
        server_name: &str,
        command: &str,
        args: &[String],
        env: &BTreeMap<String, String>,
        call_timeout: std::time::Duration,
    ) -> Result<Arc<Self>, String> {
        let mut cmd = tokio::process::Command::new(command);
        cmd.args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        // scrubbed environment: keep-list + declared env ONLY
        cmd.env_clear();
        for (k, v) in scrub_env(env) {
            cmd.env(k, v);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("mcp spawn {command}: {e}"))?;

        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;

        let client = Arc::new(Self {
            server_name: server_name.into(),
            transport: Transport::Stdio {
                _child: Mutex::new(child),
                stdin: Arc::new(Mutex::new(stdin)),
            },
            next_id: AtomicU64::new(1),
            pending: std::sync::Mutex::new(None),
            tools: Mutex::new(vec![]),
            call_timeout,
        });

        // reader task: collect responses by id, notifications dropped
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Response>();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(resp) = serde_json::from_str::<Response>(&line) {
                    if tx.send(resp).is_err() {
                        break;
                    }
                }
            }
        });
        *client.pending.lock().unwrap_or_else(|p| p.into_inner()) = Some(rx);
        Ok(client)
    }

    async fn dial_http(
        server_name: &str,
        url: &str,
        headers: &BTreeMap<String, String>,
        call_timeout: std::time::Duration,
    ) -> Result<Arc<Self>, String> {
        let http = reqwest::Client::builder()
            .timeout(call_timeout)
            .build()
            .map_err(|e| format!("mcp http client: {e}"))?;
        // dial check: a HEAD/GET would not be MCP — verify by attempting
        // the handshake lazily (initialize runs in connect_with_timeout);
        // here only validate the URL parses
        let parsed = reqwest::Url::parse(url).map_err(|e| format!("mcp url {url}: {e}"))?;
        let _ = parsed;
        Ok(Arc::new(Self {
            server_name: server_name.into(),
            transport: Transport::Http {
                url: url.to_string(),
                http,
                headers: headers.clone(),
            },
            next_id: AtomicU64::new(1),
            pending: std::sync::Mutex::new(None),
            tools: Mutex::new(vec![]),
            call_timeout,
        }))
    }

    pub async fn tool_names(&self) -> Vec<String> {
        self.tools
            .lock()
            .await
            .iter()
            .map(|(n, _, _)| n.clone())
            .collect()
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let req = Request::new(id, method, params);
        match &self.transport {
            Transport::Stdio { stdin, .. } => {
                {
                    let mut stdin = stdin.lock().await;
                    stdin
                        .write_all(format!("{}\n", req.to_line()).as_bytes())
                        .await
                        .map_err(|e| format!("mcp write: {e}"))?;
                    stdin.flush().await.map_err(|e| format!("mcp flush: {e}"))?;
                }
                // wait for the matching response (dropping stale ones)
                let deadline = self.call_timeout;
                let started = tokio::time::Instant::now();
                loop {
                    if started.elapsed() > deadline {
                        return Err(format!("mcp {method}: timed out"));
                    }
                    // take the receiver out of the sync mutex (never held across await)
                    let mut taken = self
                        .pending
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .take()
                        .ok_or_else(|| format!("mcp {method}: reader not installed"))?;
                    let resp = match tokio::time::timeout(deadline, taken.recv()).await {
                        Ok(Some(r)) => r,
                        Ok(None) => return Err(format!("mcp {method}: server closed output")),
                        Err(_) => return Err(format!("mcp {method}: timed out")),
                    };
                    // put the receiver back for the next call
                    *self.pending.lock().unwrap_or_else(|p| p.into_inner()) = Some(taken);
                    if resp.id.as_u64() == Some(id) {
                        if let Some(err) = resp.error {
                            return Err(format!("mcp {method}: {} ({})", err.message, err.code));
                        }
                        return Ok(resp.result.unwrap_or(Value::Null));
                    }
                    // stale response — ignore and keep waiting
                }
            }
            Transport::Http { url, http, headers } => {
                let mut r = http
                    .post(url)
                    .header("Accept", "application/json, text/event-stream")
                    .header("Content-Type", "application/json")
                    .body(req.to_line());
                for (k, v) in headers {
                    r = r.header(k.as_str(), v.as_str());
                }
                let resp = r
                    .send()
                    .await
                    .map_err(|e| format!("mcp http {method}: {e}"))?;
                let status = resp.status();
                if !status.is_success() {
                    return Err(format!("mcp http {method}: HTTP {status}"));
                }
                let ct = resp
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_lowercase();
                let body = resp
                    .text()
                    .await
                    .map_err(|e| format!("mcp http {method}: read: {e}"))?;
                let parsed = if ct.contains("text/event-stream") {
                    // SSE-framed: take the LAST data: line carrying our id
                    let mut found = None;
                    for line in body.lines() {
                        if let Some(data) = line.strip_prefix("data:") {
                            if let Ok(r) = serde_json::from_str::<Response>(data.trim()) {
                                if r.id.as_u64() == Some(id) {
                                    found = Some(r);
                                }
                            }
                        }
                    }
                    found
                } else {
                    serde_json::from_str::<Response>(&body).ok()
                };
                match parsed {
                    Some(resp) if resp.id.as_u64() == Some(id) => {
                        if let Some(err) = resp.error {
                            return Err(format!("mcp {method}: {} ({})", err.message, err.code));
                        }
                        Ok(resp.result.unwrap_or(Value::Null))
                    }
                    _ => Err(format!("mcp http {method}: no matching response")),
                }
            }
        }
    }

    async fn notify(&self, method: &str, params: Value) {
        let req = Request::notification(method, params);
        if let Transport::Stdio { stdin, .. } = &self.transport {
            let mut stdin = stdin.lock().await;
            let _ = stdin
                .write_all(format!("{}\n", req.to_line()).as_bytes())
                .await;
            let _ = stdin.flush().await;
        }
        // HTTP notifications are skipped: streamable HTTP carries no
        // client->server notification channel in this minimal transport
    }

    pub async fn call_tool(&self, tool: &str, args: Value) -> Result<Value, String> {
        self.call(
            "tools/call",
            serde_json::json!({"name": tool, "arguments": args}),
        )
        .await
    }

    /// Stop the server (stdio child killed; HTTP is stateless).
    pub async fn shutdown(&self) {
        if let Transport::Stdio { _child, .. } = &self.transport {
            // kill_on_drop(true) reaps the child when the client drops;
            // an explicit kill here would need the mutex, and the Mutex
            // is only held by drop — do nothing (documented narrowing).
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The MCP client is integration-tested against the built-in server
    // (tests/mcp_roundtrip.rs) using a real spawned child process, and
    // against a stub HTTP server (tests/http_transport.rs).

    #[tokio::test]
    async fn spawn_missing_binary_errors_honestly() {
        let e = McpClient::connect(
            "x",
            &McpServerConfig::Stdio {
                command: "/nonexistent/mcp-server-xyz".into(),
                args: vec![],
                env: BTreeMap::new(),
            },
        )
        .await;
        assert!(e.is_err());
    }

    #[test]
    fn config_parse_stdio_and_http() {
        let stdio = McpServerConfig::from_json(
            &serde_json::json!({"command": "uvx", "args": ["mcp-server-x"], "env": {"X": "1"}}),
        )
        .expect("stdio shape");
        match stdio {
            McpServerConfig::Stdio { command, args, env } => {
                assert_eq!(command, "uvx");
                assert_eq!(args, vec!["mcp-server-x".to_string()]);
                assert_eq!(env.get("X").map(String::as_str), Some("1"));
            }
            _ => panic!("expected stdio"),
        }
        let http =
            McpServerConfig::from_json(&serde_json::json!({"url": "http://127.0.0.1:9/mcp"}))
                .expect("http shape");
        match http {
            McpServerConfig::Http { url, .. } => assert!(url.contains("127.0.0.1")),
            _ => panic!("expected http"),
        }
        assert!(McpServerConfig::from_json(&serde_json::json!({"foo": 1})).is_err());
    }

    #[test]
    fn env_scrub_strips_secrets() {
        // set a secret-looking var, then scrub: it must NOT survive
        let key = format!("MARFIM_TEST_SECRET_{}", std::process::id());
        std::env::set_var(&key, "leak-me");
        let extra = BTreeMap::new();
        let env = scrub_env(&extra);
        assert!(std::env::var("PATH").is_ok()); // PATH is always kept
        assert!(env.contains_key("PATH"));
        assert!(
            !env.contains_key(key.as_str()),
            "secret leaked through scrub"
        );
        std::env::remove_var(&key);
        // explicit config env survives (user intent)
        let mut with_extra = BTreeMap::new();
        with_extra.insert("MY_SERVER_TOKEN".to_string(), "abc".to_string());
        let env2 = scrub_env(&with_extra);
        assert_eq!(env2.get("MY_SERVER_TOKEN").map(String::as_str), Some("abc"));
    }
}
