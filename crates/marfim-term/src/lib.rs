//! marfim-term (W3.1): durable PTY sessions over a real terminal screen
//! model.
//!
//! Two halves, one crate:
//! - **PTY**: [`portable_pty`] owns every unsafe byte (openpty/fork/setsid/
//!   ConPTY on Windows); this crate stays `#![forbid(unsafe_code)]` and
//!   drives it through the safe facade only. The child gets stdin/stdout/
//!   stderr on the pty slave, so `isatty()` is true and curses/REPL
//!   programs behave.
//! - **Emulator**: [`vt100`] maintains the grid (the "vt100 first, alacritty
//!   fallback" doctrine — the alacritty_terminal backend remains a drop-in
//!   alternative if exotic sequences ever demand it).
//!
//! The API is **synchronous**: tool dispatch (`ToolFn`) and the gateway's
//! injected handlers are sync-first, and each session owns one plain
//! reader thread feeding the parser. Memory is bounded twice — the parser
//! keeps a fixed grid + capped scrollback, and the pending-output ring
//! (bytes since the last [`TermSession::drain`]) drops oldest-first past
//! [`MAX_PENDING`] with an explicit marker in the drained text.
//!
//! Tests spawn real children and are `#[cfg(unix)]`; on other targets the
//! API compiles and honestly errors at spawn time instead of pretending.

#![forbid(unsafe_code)]

pub mod server;
pub mod session;

pub use server::{TermInfo, TermServer};
pub use session::{TermSession, TermSpec, DEFAULT_COLS, DEFAULT_ROWS, MAX_PENDING, SCROLLBACK};
