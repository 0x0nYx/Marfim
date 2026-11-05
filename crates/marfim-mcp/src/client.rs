//! MCP client: spawn a server over stdio, initialize handshake,
//! tools/list (→ registry `server__tool` entries, deferred),
//! tools/call. Transport: newline-delimited JSON-RPC.

use crate::jsonrpc::{Request, Response};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

pub type ToolListing = Vec<(String, String, Value)>; // (name, description, schema)
type PendingRx = Option<tokio::sync::mpsc::UnboundedReceiver<Response>>;

pub struct McpClient {
    pub server_name: String,
    child: Mutex<Child>,
    stdin: Arc<Mutex<tokio::process::ChildStdin>>,
    next_id: AtomicU64,
    pending: std::sync::Mutex<PendingRx>,
    pub tools: Mutex<ToolListing>,
    /// Per-call deadline. The 30 s default is for real servers; tests
    /// inject a short deadline so stale responses fail fast and honestly.
    pub call_timeout: std::time::Duration,
}

impl McpClient {
    /// Spawn `command args...` and run the initialize handshake.
    pub async fn spawn(
        server_name: &str,
        command: &str,
        args: &[String],
    ) -> Result<Arc<Self>, String> {
        Self::spawn_with_timeout(server_name, command, args, std::time::Duration::from_secs(30))
            .await
    }

    /// Like [`McpClient::spawn`] with an explicit per-call deadline.
    pub async fn spawn_with_timeout(
        server_name: &str,
        command: &str,
        args: &[String],
        call_timeout: std::time::Duration,
    ) -> Result<Arc<Self>, String> {
        let mut child = Command::new(command)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("mcp spawn {command}: {e}"))?;

        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;

        let client = Arc::new(Self {
            server_name: server_name.into(),
            child: Mutex::new(child),
            stdin: Arc::new(Mutex::new(stdin)),
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
        {
            let mut stdin = self.stdin.lock().await;
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

    async fn notify(&self, method: &str, params: Value) {
        let req = Request::notification(method, params);
        let mut stdin = self.stdin.lock().await;
        let _ = stdin
            .write_all(format!("{}\n", req.to_line()).as_bytes())
            .await;
        let _ = stdin.flush().await;
    }

    pub async fn call_tool(&self, tool: &str, args: Value) -> Result<Value, String> {
        self.call(
            "tools/call",
            serde_json::json!({"name": tool, "arguments": args}),
        )
        .await
    }

    pub async fn shutdown(&self) {
        if let Ok(mut c) = self.child.try_lock() {
            let _ = c.kill().await;
        }
    }
}

// pending is on the struct; declare it via impl field trick — Rust needs
// the field in the struct. (Declared below to keep the spawn flow tidy.)
#[cfg(test)]
mod tests {
    use super::*;

    // The MCP client is integration-tested against the built-in server
    // (tests/mcp_roundtrip.rs) using a real spawned child process.
    #[tokio::test]
    async fn spawn_missing_binary_errors_honestly() {
        let e = McpClient::spawn("x", "/nonexistent/mcp-server-xyz", &[]).await;
        assert!(e.is_err());
    }
}
