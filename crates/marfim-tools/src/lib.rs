//! Marfim tools: registry + all built-in tools (Node v1.2.0 names kept
//! verbatim — see PARITY §4).

#![forbid(unsafe_code)]

pub mod bash;
pub mod compute;
pub mod edit_ladder;
pub mod fs;
pub mod globber;
pub mod misc_tools;
pub mod monitor;
pub mod patch;
pub mod registry;
pub mod repomap;
pub mod todo;
pub mod tools_host;
pub mod web;

pub use registry::{ToolCtx, ToolDef, ToolOutput, ToolRegistry};
