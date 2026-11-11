//! W5.7 walker discipline (distilled: OMP `pi-walker`, clean-room):
//!
//! - **Chunk-fed grep**: files stream through a fixed-size buffer with a
//!   line-carry state machine — memory is bounded by the CHUNK + the
//!   longest carried line (itself capped), never by the file size. A
//!   10 GB log greps in constant memory.
//! - **Stamp-based directory cache**: listings keyed by (dir, stamp)
//!   where the stamp is the directory's own mtime; a 50ms racy window
//!   trusts an entry without re-statting (hot-loop friendliness), and
//!   the total cached bytes are capped (64 MiB) with oldest-shard
//!   eviction. ZERO invalidation API — staleness is a stamp compare,
//!   not a protocol.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Read chunk size (64 KiB — one allocation per file, reused).
pub const CHUNK: usize = 64 * 1024;
/// A carried line longer than this is treated as binary junk: the
/// remainder of the line is skipped, the line is NOT tested.
pub const MAX_LINE: usize = 1 << 20;
/// Display cap for a matched line (huge minified lines are useless).
pub const LINE_DISPLAY: usize = 4_000;
/// Racy window: entries younger than this are trusted without a re-stat.
pub const RACY_WINDOW: Duration = Duration::from_millis(50);
/// Total cached listing bytes before eviction starts.
pub const CACHE_BYTES: usize = 64 * 1024 * 1024;
/// Shard count (power of two; the shard is picked by path hash).
pub const SHARDS: usize = 64;

/// Grep one file with bounded memory. Returns (line_no, display_line)
/// pairs; stops after `limit` matches. Returns Err only when the file
/// cannot be opened; decode errors inside are handled lossily.
pub fn grep_file_chunked(
    path: &Path,
    is_match: &dyn Fn(&str) -> bool,
    limit: usize,
) -> Result<Vec<(u64, String)>, String> {
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut matches = Vec::new();
    let mut buf = vec![0u8; CHUNK];
    let mut carry: Vec<u8> = Vec::new();
    let mut line_no: u64 = 0;
    let mut done = false;
    while !done {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            // flush the final unterminated line
            if !carry.is_empty() {
                line_no += 1;
                if let Ok(text) = std::str::from_utf8(&carry) {
                    if is_match(text) {
                        matches.push((line_no, clip(text)));
                    }
                }
                carry.clear();
            }
            break;
        }
        let mut start = 0usize;
        for (i, b) in buf[..n].iter().enumerate() {
            if *b == b'\n' {
                carry.extend_from_slice(&buf[start..i]);
                start = i + 1;
                line_no += 1;
                if carry.len() <= MAX_LINE {
                    if let Ok(text) = std::str::from_utf8(&carry) {
                        if is_match(text) {
                            matches.push((line_no, clip(text)));
                            if matches.len() >= limit {
                                done = true;
                                break;
                            }
                        }
                    }
                }
                carry.clear();
            }
        }
        if done {
            break;
        }
        carry.extend_from_slice(&buf[start..n]);
        if carry.len() > MAX_LINE {
            // binary-ish monster line: skip to its end within the chunk stream
            carry.clear();
        }
    }
    Ok(matches)
}

fn clip(line: &str) -> String {
    if line.len() <= LINE_DISPLAY {
        line.to_string()
    } else {
        let mut end = LINE_DISPLAY;
        while end < line.len() && !line.is_char_boundary(end) {
            end += 1;
        }
        format!("{}[…]", &line[..end])
    }
}

struct ShardEntry {
    listing: Vec<PathBuf>,
    bytes: usize,
    at: Instant,
    stamp: Option<std::time::SystemTime>,
}

/// Stamp-based directory listing cache. `listing(dir)` returns the
/// cached entries when the directory's mtime still matches (or the
/// entry is inside the racy window); otherwise it re-lists.
pub struct DirCache {
    shards: Vec<Mutex<std::collections::BTreeMap<PathBuf, ShardEntry>>>,
    total: Mutex<usize>,
}

impl Default for DirCache {
    fn default() -> Self {
        Self::new()
    }
}

impl DirCache {
    pub fn new() -> Self {
        Self {
            shards: (0..SHARDS)
                .map(|_| Mutex::new(Default::default()))
                .collect(),
            total: Mutex::new(0),
        }
    }

    fn shard(&self, dir: &Path) -> &Mutex<std::collections::BTreeMap<PathBuf, ShardEntry>> {
        let h = std::collections::hash_map::DefaultHasher::new();
        use std::hash::Hasher;
        let mut h = h;
        h.write(dir.to_string_lossy().as_bytes());
        let idx = (h.finish() as usize) & (SHARDS - 1);
        &self.shards[idx]
    }

    /// The cached listing, or `None` when the stamp moved (caller re-lists
    /// and calls [`Self::store`]).
    pub fn listing(&self, dir: &Path) -> Option<Vec<PathBuf>> {
        let shard = self.shard(dir).lock().unwrap_or_else(|p| p.into_inner());
        let entry = shard.get(dir)?;
        // Racy window: brand-new entries are trusted without a re-stat.
        if entry.at.elapsed() < RACY_WINDOW {
            return Some(entry.listing.clone());
        }
        // Stamp compare: the directory's own mtime IS the invalidation
        // protocol — no API, no callbacks, no stale subscribers.
        let now = std::fs::metadata(dir).and_then(|m| m.modified()).ok();
        match (now, entry.stamp) {
            (Some(now), Some(stored)) if now == stored => Some(entry.listing.clone()),
            _ => None,
        }
    }

    pub fn store(&self, dir: &Path, listing: Vec<PathBuf>) {
        let stamp = std::fs::metadata(dir).and_then(|m| m.modified()).ok();
        let bytes = listing
            .iter()
            .map(|p| p.as_os_str().len() + 8)
            .sum::<usize>();
        let mut shard = self.shard(dir).lock().unwrap_or_else(|p| p.into_inner());
        let replaced = shard.insert(
            dir.to_path_buf(),
            ShardEntry {
                listing: listing.clone(),
                bytes,
                at: Instant::now(),
                stamp,
            },
        );
        let mut total = self.total.lock().unwrap_or_else(|p| p.into_inner());
        *total += bytes;
        if let Some(old) = replaced {
            *total = total.saturating_sub(old.bytes);
        }
        // Evict oldest when over budget (zero invalidation API: aging out
        // is the ONLY deletion path, and it's correctness-neutral).
        if *total > CACHE_BYTES {
            let mut oldest: Vec<(PathBuf, Instant, usize)> = shard
                .iter()
                .map(|(k, v)| (k.clone(), v.at, v.bytes))
                .collect();
            oldest.sort_by_key(|(_, at, _)| *at);
            let half = *total / 2;
            let mut freed = 0usize;
            for (k, _, b) in oldest {
                if freed >= half {
                    break;
                }
                shard.remove(&k);
                freed += b;
            }
            *total = total.saturating_sub(freed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunked_grep_finds_matches_across_chunk_boundaries() {
        let dir = marfim_testkit::Temp::new("marfim-grepc").unwrap();
        // a line that straddles the 64KiB chunk edge
        let filler = "z".repeat(CHUNK - 5);
        let content = format!("first needle\n{filler}\nneedle-two\nplain\nneedle-three");
        std::fs::write(dir.path().join("f.txt"), &content).unwrap();
        let hits = grep_file_chunked(
            &dir.path().join("f.txt"),
            &|l: &str| l.contains("needle"),
            100,
        )
        .unwrap();
        assert_eq!(hits.len(), 3, "all matches found: {hits:?}");
        assert_eq!(hits[0], (1, "first needle".to_string()));
        assert_eq!(hits[2], (5, "needle-three".to_string()));
    }

    #[test]
    fn chunked_grep_respects_the_limit() {
        let dir = marfim_testkit::Temp::new("marfim-grepl").unwrap();
        let content = "needle\nneedle\nneedle\n".repeat(100);
        std::fs::write(dir.path().join("f.txt"), &content).unwrap();
        let hits = grep_file_chunked(
            &dir.path().join("f.txt"),
            &|l: &str| l.contains("needle"),
            5,
        )
        .unwrap();
        assert_eq!(hits.len(), 5, "limit respected");
    }

    #[test]
    fn chunked_grep_handles_binary_and_oversized_lines() {
        let dir = marfim_testkit::Temp::new("marfim-grepb").unwrap();
        let mut content = vec![0u8; 100];
        content.extend_from_slice(&[0xFF, 0xFE, b'x']);
        content.extend_from_slice(&[b'a'; MAX_LINE + 10]);
        content.extend_from_slice(b"\nneedle-at-end\n");
        std::fs::write(dir.path().join("f.bin"), &content).unwrap();
        let hits = grep_file_chunked(
            &dir.path().join("f.bin"),
            &|l: &str| l.contains("needle"),
            10,
        )
        .unwrap();
        assert_eq!(hits.len(), 1, "monster line skipped, tail match kept");
        assert_eq!(hits[0].1, "needle-at-end");
    }

    #[test]
    fn dir_cache_stamps_and_racy_window() {
        let dir = marfim_testkit::Temp::new("marfim-dc").unwrap();
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        let cache = DirCache::new();
        let d = dir.path().to_path_buf();
        assert!(cache.listing(&d).is_none(), "cold cache");
        let listing = vec![d.join("a.txt")];
        cache.store(&d, listing.clone());
        // inside the racy window: trusted without re-stat
        assert_eq!(cache.listing(&d), Some(listing.clone()));
        // outside the window with a MOVED stamp: re-list required
        std::thread::sleep(RACY_WINDOW * 3);
        std::fs::write(d.join("b.txt"), "y").unwrap();
        assert_eq!(cache.listing(&d), None, "stamp moved → re-list");
        let listing2 = vec![d.join("a.txt"), d.join("b.txt")];
        cache.store(&d, listing2.clone());
        assert_eq!(cache.listing(&d), Some(listing2));
    }
}
