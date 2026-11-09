//! Marfim tools: registry + all built-in tools (Node v1.2.0 names kept
//! verbatim — see PARITY §4).

#![forbid(unsafe_code)]

pub mod bash;
pub mod bounding;
pub mod code_mode;
pub mod compute;
pub mod edit_ladder;
pub mod fs;
pub mod globber;
pub mod misc_tools;
pub mod monitor;
pub mod patch;
pub mod rea_tool;
pub mod registry;
pub mod repomap;
// W5.3: tree-sitter extractor, compiled only behind the `treesitter`
// feature (default OFF — the regex extractor is the default path).
#[cfg(feature = "treesitter")]
pub mod repomap_treesitter;
pub mod subagent;
pub mod terminal;
pub mod todo;
pub mod tools_host;
pub mod walkcache;
pub mod web;

pub use registry::{ToolCtx, ToolDef, ToolOutput, ToolRegistry};
