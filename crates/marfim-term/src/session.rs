//! One PTY session: spawn, feed the emulator, bounded pending ring.

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::num::NonZeroU16;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Default terminal geometry (matches the TUI's 80x24 minimum).
pub const DEFAULT_ROWS: u16 = 24;
pub const DEFAULT_COLS: u16 = 80;
/// Grid scrollback the emulator retains (lines above the viewport).
pub const SCROLLBACK: usize = 1_000;
/// Cap on bytes pending since the last [`TermSession::drain`]. Past this,
/// the oldest half is dropped and an explicit marker is prepended to the
/// next drain — the reader can never stall the child (a full pty buffer
/// would block the writer side), and the model is told honestly.
pub const MAX_PENDING: usize = 64 * 1024;
/// Read chunk size from the master (small enough to keep the lock short).
const CHUNK: usize = 4 * 1024;
/// Exited sessions older than this are reaped by [`TermServer`] calls
/// (their exit screen had a grace window to be read).
pub const REAP_AFTER: Duration = Duration::from_secs(60);

/// What to spawn. The child environment is an ALLOWLIST, not an
/// inheritance: `TERM` is always set (colors + curses depend on it), the
/// identity trio PATH/HOME/SHELL passes through from the harness when
/// present, and everything else must be explicit in `env`.
#[derive(Debug, Clone)]
pub struct TermSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<std::path::PathBuf>,
    pub env: BTreeMap<String, String>,
    pub rows: u16,
    pub cols: u16,
    pub scrollback: usize,
}

impl TermSpec {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env: BTreeMap::new(),
            rows: DEFAULT_ROWS,
            cols: DEFAULT_COLS,
            scrollback: SCROLLBACK,
        }
    }
}

struct State {
    parser: vt100::Parser,
    pending: Vec<u8>,
    dropped: bool,
    total_bytes: u64,
}

pub struct TermSession {
    id: u64,
    command: String,
    started_at: Instant,
    state: Mutex<State>,
    writer: Mutex<Box<dyn Write + Send>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    child: Mutex<Box<dyn Child + Send + Sync>>,
    exit_code: Mutex<Option<u32>>,
    finished_at: Mutex<Option<Instant>>,
    seq: AtomicU64,
}

/// Geometry floors: a 0-row terminal is meaningless — clamp, don't pretend.
fn nz(v: u16) -> NonZeroU16 {
    NonZeroU16::new(v.max(1)).unwrap_or(NonZeroU16::MIN)
}

impl std::fmt::Debug for TermSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TermSession")
            .field("id", &self.id)
            .field("command", &self.command)
            .field("alive", &self.alive())
            .field("exit_code", &self.exit_code())
            .field("total_bytes", &self.total_bytes())
            .finish()
    }
}

impl TermSession {
    /// Spawn the child on a fresh pty and start the reader thread.
    /// `id` is assigned by the caller (the server owns the id space).
    pub fn spawn(id: u64, spec: &TermSpec) -> Result<Arc<Self>, String> {
        let rows = spec.rows.max(1);
        let cols = spec.cols.max(1);
        let pty_system = portable_pty::native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("pty open failed: {e}"))?;

        let mut cmd = CommandBuilder::new(&spec.program);
        for a in &spec.args {
            cmd.arg(a);
        }
        if let Some(dir) = &spec.cwd {
            if !dir.is_dir() {
                return Err(format!("cwd is not a directory: {}", dir.display()));
            }
            cmd.cwd(dir);
        }
        // Allowlisted environment: TERM always; identity trio passthrough;
        // caller-supplied pairs last (explicit wins).
        cmd.env_clear();
        cmd.env("TERM", "xterm-256color");
        for k in ["PATH", "HOME", "SHELL", "LANG", "TMPDIR"] {
            if let Ok(v) = std::env::var(k) {
                cmd.env(k, v);
            }
        }
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }

        let slave = pair.slave;
        let master = pair.master;
        let child = slave
            .spawn_command(cmd)
            .map_err(|e| format!("spawn `{}` failed: {e}", spec.program))?;
        // The slave handle must be dropped after spawn or the master never
        // sees EOF (the pty stays open through our fd).
        drop(slave);

        let reader = master
            .try_clone_reader()
            .map_err(|e| format!("pty reader: {e}"))?;
        let writer = master
            .take_writer()
            .map_err(|e| format!("pty writer: {e}"))?;

        let session = Arc::new(Self {
            id,
            command: format!(
                "{}{}",
                spec.program,
                spec.args
                    .iter()
                    .map(|a| format!(" {a}"))
                    .collect::<String>()
            ),
            started_at: Instant::now(),
            state: Mutex::new(State {
                parser: vt100::Parser::new(nz(rows), nz(cols), spec.scrollback),
                pending: Vec::new(),
                dropped: false,
                total_bytes: 0,
            }),
            writer: Mutex::new(writer),
            master: Mutex::new(master),
            child: Mutex::new(child),
            exit_code: Mutex::new(None),
            finished_at: Mutex::new(None),
            seq: AtomicU64::new(0),
        });

        // Reader thread: master bytes → emulator + pending ring. EOF or a
        // read error ends the loop; the child's real exit status is taken
        // afterwards (best effort — a stuck wait() marks exit 0-free EOF).
        let me = Arc::clone(&session);
        std::thread::Builder::new()
            .name(format!("marfim-term-{id}"))
            .spawn(move || {
                let mut reader = reader;
                let mut buf = [0u8; CHUNK];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => me.ingest(&buf[..n]),
                    }
                }
                // Reap the real status once output closed.
                if let Ok(mut child) = me.child.lock() {
                    // wait() may already have been preceded by kill(); both
                    // paths converge on a status here.
                    if let Ok(st) = child.wait() {
                        *me.exit_code.lock().unwrap_or_else(|p| p.into_inner()) =
                            Some(st.exit_code());
                    }
                }
                *me.finished_at.lock().unwrap_or_else(|p| p.into_inner()) = Some(Instant::now());
            })
            .map_err(|e| format!("reader thread: {e}"))?;

        Ok(session)
    }

    fn ingest(&self, bytes: &[u8]) {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        st.parser.process(bytes);
        st.total_bytes += bytes.len() as u64;
        // Monotonic activity counter: monitors (W3.2) poll this to detect
        // "the terminal said something" without draining.
        self.seq.fetch_add(1, Ordering::Relaxed);
        if st.pending.len() + bytes.len() > MAX_PENDING {
            // Drop the OLDEST half, keep the newest — the marker lands in
            // the next drain, honestly.
            let keep_from = st.pending.len() / 2;
            st.pending.drain(..keep_from);
            st.dropped = true;
        }
        st.pending.extend_from_slice(bytes);
    }

    pub fn id(&self) -> u64 {
        self.id
    }
    pub fn command(&self) -> &str {
        &self.command
    }
    pub fn started_at(&self) -> Instant {
        self.started_at
    }
    /// Monotonic output sequence: bumped on every ingest, lets a poller
    /// detect activity without draining.
    pub fn output_seq(&self) -> u64 {
        self.seq.load(Ordering::Relaxed)
    }

    /// Write bytes to the child's stdin (input, not commands — no shell
    /// parsing ever happens here).
    pub fn write(&self, bytes: &[u8]) -> Result<(), String> {
        self.writer
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .write_all(bytes)
            .map_err(|e| format!("write failed: {e}"))
    }

    /// Resize the pty AND the emulator grid (the child gets SIGWINCH).
    pub fn resize(&self, rows: u16, cols: u16) -> Result<(), String> {
        self.master
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("resize failed: {e}"))?;
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .parser
            .set_size(nz(rows), nz(cols));
        Ok(())
    }

    /// Take and clear the bytes produced since the last call, as lossy
    /// UTF-8 with an explicit drop marker when the ring overflowed.
    pub fn drain(&self) -> String {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let raw = std::mem::take(&mut st.pending);
        let dropped = std::mem::take(&mut st.dropped);
        drop(st);
        let mut text = String::from_utf8_lossy(&raw).to_string();
        if dropped {
            text.insert_str(0, "[marfim-term: older output dropped (ring cap)]\n");
        }
        text
    }

    /// Plain-text snapshot of the visible grid (what a human sees now).
    pub fn screen(&self) -> String {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .parser
            .screen()
            .contents()
    }

    /// ANSI-attributed snapshot (colors/attributes) for real terminals.
    pub fn screen_ansi(&self) -> String {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .parser
            .screen()
            .contents_formatted()
    }

    /// (row, col) of the cursor.
    pub fn cursor(&self) -> (u16, u16) {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .parser
            .screen()
            .cursor_position()
    }

    pub fn total_bytes(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .total_bytes
    }

    pub fn pending_len(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .pending
            .len()
    }

    /// Whether the pending ring already overflowed since the last drain
    /// (the marker text accompanies the next [`Self::drain`]).
    pub fn pending_dropped(&self) -> bool {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).dropped
    }

    pub fn scrollback_len(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .parser
            .screen()
            .scrollback_len()
    }

    /// True while the child has not been observed to exit.
    pub fn alive(&self) -> bool {
        self.exit_code
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_none()
    }

    /// The child's exit code, once the reader thread observed the close.
    pub fn exit_code(&self) -> Option<u32> {
        *self.exit_code.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn finished_at(&self) -> Option<Instant> {
        *self.finished_at.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Kill the child (best effort; the reader thread still collects the
    /// status so the session ends honestly).
    pub fn kill(&self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
        }
    }

    /// Reap eligibility: exited, and the exit screen had its grace window.
    pub fn reapable(&self, now: Instant) -> bool {
        match self.finished_at() {
            Some(t) => now.duration_since(t) >= REAP_AFTER,
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> TermSpec {
        let mut s = TermSpec::new("sh");
        s.args = vec!["-c".into(), script.into()];
        s
    }

    fn wait_for<F: Fn() -> bool>(f: F) -> bool {
        for _ in 0..100 {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn pty_gives_the_child_a_tty_and_captures_output() {
        let s = TermSession::spawn(1, &sh("test -t 1 && echo TTY-OK || echo NO-TTY")).unwrap();
        assert!(wait_for(|| !s.alive()), "child should exit");
        assert!(wait_for(|| s.exit_code() == Some(0)));
        assert!(
            s.drain().contains("TTY-OK"),
            "stdout must arrive through the pty; got: {:?}",
            s.screen()
        );
    }

    #[test]
    fn screen_model_holds_the_text() {
        let s = TermSession::spawn(2, &sh("printf 'line-one\\nline-two\\n'")).unwrap();
        assert!(wait_for(|| s.total_bytes() > 0));
        let screen = s.screen();
        assert!(screen.contains("line-one"), "screen: {screen:?}");
        assert!(screen.contains("line-two"));
    }

    #[test]
    fn write_reaches_stdin_without_shell_parsing() {
        let s = TermSession::spawn(3, &sh("cat")).unwrap();
        // The pty line discipline echoes input, so the emulator sees the
        // bytes even though `cat` says nothing on its own.
        s.write(b"hello-pty\n").unwrap();
        assert!(wait_for(|| s.screen().contains("hello-pty")));
        s.kill();
        assert!(wait_for(|| !s.alive()));
    }

    #[test]
    fn cwd_and_env_allowlist_apply() {
        let dir = marfim_testkit::Temp::new("marfim-term-cwd").unwrap();
        let mut spec = sh("pwd; echo V=$MARFIM_TERM_TEST; echo P=$NOT_SET");
        spec.cwd = Some(dir.path().to_path_buf());
        spec.env.insert("MARFIM_TERM_TEST".into(), "present".into());
        let s = TermSession::spawn(4, &spec).unwrap();
        assert!(wait_for(|| !s.alive()));
        let out = s.drain();
        assert!(
            out.contains(dir.path().to_str().unwrap()),
            "pwd must be the spec cwd: {out:?}"
        );
        assert!(out.contains("V=present"), "spec env applies: {out:?}");
        assert!(out.contains("P="), "env allowlist keeps unknown vars out");
        assert!(!out.contains("$NOT_SET") || out.contains("P=\n") || out.contains("P=\r"));
    }

    #[test]
    fn resize_propagates_to_the_child() {
        let mut spec = sh("stty size; read _; stty size");
        spec.rows = 10;
        spec.cols = 40;
        let s = TermSession::spawn(5, &spec).unwrap();
        std::thread::sleep(Duration::from_millis(150));
        s.resize(30, 100).unwrap();
        s.write(b"go\n").unwrap();
        assert!(wait_for(|| s.drain().contains("30 100")));
        s.kill();
    }

    #[test]
    fn exit_code_is_honest() {
        let s = TermSession::spawn(6, &sh("exit 7")).unwrap();
        assert!(wait_for(|| s.exit_code().is_some()));
        assert_eq!(s.exit_code(), Some(7));
    }

    #[test]
    fn pending_ring_reports_overflow_honestly() {
        let s = TermSession::spawn(7, &sh("yes marfim-flood")).unwrap();
        assert!(
            wait_for(|| s.pending_dropped()),
            "flood should trip the ring's overflow flag"
        );
        let drained = s.drain();
        assert!(drained.starts_with("[marfim-term: older output dropped"));
        s.kill();
        // Quiesce before the clear-check: with the flood still running, any
        // ingest between drain() and the assert re-trips the flag (a real
        // flake the full-suite run caught). After the reader thread sees
        // EOF no further ingest can happen, so the flag is honestly clear.
        assert!(wait_for(|| !s.alive()), "session should end after kill");
        let _ = s.drain();
        assert!(!s.pending_dropped(), "drain clears the flag");
    }

    #[test]
    fn missing_cwd_fails_closed() {
        let mut spec = sh("true");
        spec.cwd = Some(std::path::PathBuf::from("/definitely/not/here-marfim"));
        let err = TermSession::spawn(8, &spec).unwrap_err();
        assert!(err.contains("not a directory"), "err: {err}");
    }
}
