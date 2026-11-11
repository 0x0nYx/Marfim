//! Gateway surfaces: HTTP+SSE serve, ACP agent + client, LSP server.
//! All hand-rolled on tokio (no axum: binary-size budget — see CHANGELOG).

#![forbid(unsafe_code)]

pub mod acp;
pub mod lsp;
pub mod protocol;
pub mod serve;
