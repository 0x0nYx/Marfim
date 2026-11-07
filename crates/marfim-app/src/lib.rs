//! marfim-app — the embeddable application layer (W1.1).
//!
//! `settings` resolves every CLI flag and config key through ONE
//! consumption point; `factory` owns the provider/chain building and the
//! session factory. The `marfim` bin is thin (clap + tui + slash); every
//! embedding surface (FFI, terminal, Flutter transport) builds on this
//! crate with zero behavior drift from the CLI.

#![forbid(unsafe_code)]

pub mod factory;
pub mod settings;
