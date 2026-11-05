//! Monitor: file watchers (mtime baseline polling).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub struct MonitorRec {
    pub id: u64,
    pub path: String,
    pub baseline_mtime: std::time::SystemTime,
    pub active: bool,
}

#[derive(Default)]
pub struct Monitors {
    next_id: AtomicU64,
    pub list: Mutex<Vec<MonitorRec>>,
}

impl Monitors {
    pub fn start(&self, path: &str) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        let baseline = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        self.list
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(MonitorRec {
                id,
                path: path.into(),
                baseline_mtime: baseline,
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

    /// Returns monitors whose target changed since their baseline.
    pub fn poll(&self) -> Vec<u64> {
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
            if let Ok(meta) = std::fs::metadata(&m.path) {
                if let Ok(mt) = meta.modified() {
                    if mt > m.baseline_mtime {
                        changed.push(m.id);
                        m.baseline_mtime = mt;
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
        let dir = std::env::temp_dir().join(format!("marfim-mon-{}", std::process::id()));
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
}
