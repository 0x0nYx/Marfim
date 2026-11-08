//! Monitor: long-lived change watchers (W3.2 migrated onto the terminal
//! substrate). Two sources:
//! - [`MonitorSource::File`] — mtime baseline polling (the original
//!   semantics, unchanged).
//! - [`MonitorSource::Terminal`] — a terminal session's OUTPUT: the
//!   monitor fires when the session produced bytes since its baseline
//!   (poll = "did my terminal say anything"). The baseline advances on
//!   read so each burst reports exactly once.
//!
//! Monitors are pure STATE (no background threads): the session loop
//! polls between turns, which is why subagents (poll-based children) are
//! denied Monitor/MonitorKill/Terminal wholesale.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub enum MonitorSource {
    File(String),
    Terminal(u64),
}

#[derive(Debug, Clone)]
pub struct MonitorRec {
    pub id: u64,
    pub source: MonitorSource,
    /// File: baseline mtime. Terminal: baseline output-seq.
    pub baseline: std::time::SystemTime,
    pub baseline_seq: u64,
    pub active: bool,
}

#[derive(Default)]
pub struct Monitors {
    next_id: AtomicU64,
    pub list: Mutex<Vec<MonitorRec>>,
}

impl Monitors {
    pub fn start(&self, path: &str) -> u64 {
        self.start_source(MonitorSource::File(path.to_string()))
    }

    /// Start watching any source (the migrated entry point).
    pub fn start_source(&self, source: MonitorSource) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        let (baseline, baseline_seq) = match &source {
            MonitorSource::File(path) => (
                std::fs::metadata(path)
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
                0,
            ),
            // Terminal baselines are captured at first poll (the session
            // may not exist yet at watch time); seq 0 = "anything counts".
            MonitorSource::Terminal(_) => (std::time::SystemTime::UNIX_EPOCH, 0),
        };
        self.list
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(MonitorRec {
                id,
                source,
                baseline,
                baseline_seq,
                active: true,
            });
        id
    }

    pub fn kill(&self, id: u64) -> bool {
        let mut list = self.list.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(m) = list.iter_mut().find(|m| m.id == id && m.active) {
            m.active = false;
            true
        } else {
            false
        }
    }

    /// Returns monitors whose source changed since their baseline, and
    /// advances the baseline (each burst reports exactly once).
    pub fn poll(&self) -> Vec<u64> {
        self.poll_with_terminals(None)
    }

    /// The migrated poll: file mtime as before; terminal sources compare
    /// the session's output sequence (monotonic ingest counter) against
    /// the baseline. `terminals` resolves ids → sessions; `None` simply
    /// skips terminal sources (a session-less poll never lies).
    pub fn poll_with_terminals(
        &self,
        terminals: Option<&std::sync::Arc<marfim_term::TermServer>>,
    ) -> Vec<u64> {
        let mut changed = Vec::new();
        for m in self
            .list
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter_mut()
        {
            if !m.active {
                continue;
            }
            match &m.source {
                MonitorSource::File(path) => {
                    if let Ok(meta) = std::fs::metadata(path) {
                        if let Ok(mt) = meta.modified() {
                            if mt > m.baseline {
                                changed.push(m.id);
                                m.baseline = mt;
                            }
                        }
                    }
                }
                MonitorSource::Terminal(term_id) => {
                    let Some(server) = terminals else { continue };
                    let Some(session) = server.get(*term_id) else {
                        continue;
                    };
                    let seq = session.output_seq();
                    if seq > m.baseline_seq {
                        changed.push(m.id);
                        m.baseline_seq = seq;
                    }
                }
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watch_detects_change() {
        let dir = marfim_testkit::unique_dir("marfim-mon");
        let _ = std::fs::create_dir_all(&dir);
        let f = dir.join("watched.txt");
        std::fs::write(&f, "v1").unwrap();
        let m = Monitors::default();
        let id = m.start(f.to_str().unwrap());
        assert!(m.poll().is_empty(), "no change yet");
        std::thread::sleep(std::time::Duration::from_millis(30));
        std::fs::write(&f, "v2").unwrap();
        assert_eq!(m.poll(), vec![id]);
        assert!(m.poll().is_empty(), "baseline advanced");
        assert!(m.kill(id));
        assert!(!m.kill(id));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn terminal_monitor_fires_on_output_and_advances() {
        let server = std::sync::Arc::new(marfim_term::TermServer::default());
        let mut spec = marfim_term::TermSpec::new("sh");
        spec.args = vec!["-c".into(), "echo term-burst; sleep 5".into()];
        let tid = server.spawn(&spec).unwrap();
        let m = Monitors::default();
        let id = m.start_source(MonitorSource::Terminal(tid));
        // wait until the burst landed
        let s = server.get(tid).unwrap();
        for _ in 0..200 {
            if s.output_seq() > 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let fired = m.poll_with_terminals(Some(&server));
        assert_eq!(fired, vec![id], "output burst fires the monitor");
        assert!(
            m.poll_with_terminals(Some(&server)).is_empty(),
            "baseline advanced — no double fire"
        );
        // a session-less poll skips terminal sources instead of lying
        assert!(m.poll().is_empty());
        server.close(tid);
        assert!(m.kill(id));
    }

    #[test]
    fn dead_terminal_never_fires() {
        let server = std::sync::Arc::new(marfim_term::TermServer::default());
        let m = Monitors::default();
        let id = m.start_source(MonitorSource::Terminal(999));
        assert!(m.poll_with_terminals(Some(&server)).is_empty());
        assert!(m.kill(id));
    }
}
