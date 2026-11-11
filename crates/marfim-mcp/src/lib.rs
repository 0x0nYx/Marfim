//! MCP client + server over stdio JSON-RPC 2.0. Client: spawn servers,
//! initialize, tools/list, tools/call — namespaced `server__tool` into
//! the registry (deferred by default). Server: expose marfim tools.

#![forbid(unsafe_code)]

pub mod client;
pub mod jsonrpc;
pub mod server;

pub use jsonrpc::{Request, Response};
