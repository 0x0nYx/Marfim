//! Append-only session journal (Node `wal` semantics): JSONL of journal
//! records; crash recovery = replay; rewind = truncate to a checkpoint +
//! git snapshot restore handled by the caller.

use crate::{CoreError, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JournalRecord {
    SessionMeta {
        session_id: String,
        cwd: String,
        title: Option<String>,
        started_at: String,
    },
    UserMessage {
        text: String,
    },
    AssistantMessage {
        text: String,
        model: String,
        stop_reason: String,
    },
    ToolCall {
        id: String,
        tool: String,
        args: Value,
    },
    ToolResult {
        id: String,
        output: String,
        is_error: bool,
    },
    Usage {
        role: String,
        model: String,
        input_tokens: u64,
        output_tokens: u64,
        cost_usd: Option<f64>,
    },
    Checkpoint {
        id: u64,
        git_tree: Option<String>,
        note: String,
    },
    Steer {
        text: String,
    },
    Compaction {
        summary: String,
    },
    RewindMarker {
        to_record: u64,
    },
}

impl JournalRecord {
    pub fn kind(&self) -> &'static str {
        match self {
            JournalRecord::SessionMeta { .. } => "session_meta",
            JournalRecord::UserMessage { .. } => "user_message",
            JournalRecord::AssistantMessage { .. } => "assistant_message",
            JournalRecord::ToolCall { .. } => "tool_call",
            JournalRecord::ToolResult { .. } => "tool_result",
            JournalRecord::Usage { .. } => "usage",
            JournalRecord::Checkpoint { .. } => "checkpoint",
            JournalRecord::Steer { .. } => "steer",
            JournalRecord::Compaction { .. } => "compaction",
            JournalRecord::RewindMarker { .. } => "rewind_marker",
        }
    }
}

/// Append-only journal. Every write is flushed + fsynced (crash safety).
pub struct Journal {
    path: PathBuf,
    file: std::fs::File,
    len: u64,
    /// Fault-injection hook (tests / chaos drills): when set, every append
    /// fails so callers exercise their write-error paths (D6).
    fail_writes: bool,
}

impl Journal {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        let len = file.metadata()?.len();
        Ok(Self {
            path: path.to_path_buf(),
            file,
            len,
            fail_writes: false,
        })
    }

    /// Fault-injection hook (D6): fail every subsequent append.
    pub fn set_fail_writes(&mut self, on: bool) {
        self.fail_writes = on;
    }

    pub fn append(&mut self, rec: &JournalRecord) -> Result<u64> {
        if self.fail_writes {
            return Err(CoreError::Session("injected journal write failure".into()));
        }
        let line = serde_json::to_string(rec)?;
        self.file.write_all(line.as_bytes())?;
        self.file.write_all(b"\n")?;
        self.file.flush()?;
        self.file.sync_all()?;
        let offset = self.len;
        self.len += line.len() as u64 + 1;
        Ok(offset)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn len_bytes(&self) -> u64 {
        self.len
    }

    /// Replay all records (crash recovery reads the same way).
    pub fn replay(path: &Path) -> Result<Vec<JournalRecord>> {
        let f = std::fs::File::open(path).map_err(|e| {
            CoreError::Session(format!("cannot open journal {}: {e}", path.display()))
        })?;
        let mut out = Vec::new();
        for line in std::io::BufReader::new(f).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let rec: JournalRecord = serde_json::from_str(&line).map_err(|e| {
                CoreError::Session(format!(
                    "torn/corrupt journal record at {}: {e}",
                    path.display()
                ))
            })?;
            out.push(rec);
        }
        Ok(out)
    }

    /// Rewind: keep only records up to `byte_offset` (exclusive), i.e.
    /// truncate the file back to the checkpoint. The removed tail is
    /// preserved in `<path>.pre-rewind` for audit.
    pub fn rewind_to(&self, byte_offset: u64) -> Result<u64> {
        use std::io::Seek;
        let mut src = std::fs::File::open(&self.path)?;
        let dropped = src.metadata()?.len().saturating_sub(byte_offset);
        if dropped > 0 {
            let mut tail = String::new();
            src.seek(std::io::SeekFrom::Start(byte_offset))?;
            std::io::Read::read_to_string(&mut src, &mut tail)?;
            std::fs::write(format!("{}.pre-rewind", self.path.display()), tail)?;
        }
        let tmp = format!("{}.tmp", self.path.display());
        {
            use std::io::{Read, Seek, SeekFrom};
            let mut src = std::fs::File::open(&self.path)?;
            let mut dst = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&tmp)?;
            src.seek(SeekFrom::Start(0))?;
            let mut buf = vec![0u8; byte_offset as usize];
            src.read_exact(&mut buf)?;
            dst.write_all(&buf)?;
            dst.sync_all()?;
        }
        std::fs::rename(&tmp, &self.path)?;
        Ok(dropped)
    }
}

/// Records → linear conversation context for the next provider call.
pub fn context_from(records: &[JournalRecord]) -> Vec<(String, String)> {
    // (role, content) pairs; tool calls/results folded into assistant/user text.
    let mut out = Vec::new();
    for r in records {
        match r {
            JournalRecord::UserMessage { text } => out.push(("user".into(), text.clone())),
            JournalRecord::AssistantMessage { text, .. } => {
                out.push(("assistant".into(), text.clone()))
            }
            JournalRecord::ToolCall { tool, args, .. } => {
                out.push(("assistant_toolcall".into(), format!("{tool}({args})")))
            }
            JournalRecord::ToolResult {
                id,
                output,
                is_error: _,
            } => {
                // tool name is recovered via matching id in the caller; here
                // we only know output — the harness attaches the tool name.
                out.push(("tool".into(), format!("[{id}] {output}")))
            }
            JournalRecord::Compaction { summary } => {
                out.push(("user".into(), format!("[compacted context] {summary}")))
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("marfim-journal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn append_replay_roundtrip() {
        let d = tmpdir();
        let p = d.join("s1.jsonl");
        let mut j = Journal::open(&p).unwrap();
        j.append(&JournalRecord::SessionMeta {
            session_id: "s1".into(),
            cwd: "/tmp".into(),
            title: None,
            started_at: "2026-01-01T00:00:00Z".into(),
        })
        .unwrap();
        j.append(&JournalRecord::UserMessage {
            text: "hello".into(),
        })
        .unwrap();
        let recs = Journal::replay(&p).unwrap();
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].kind(), "session_meta");
        assert_eq!(recs[1].kind(), "user_message");
    }

    #[test]
    fn rewind_preserves_tail_and_truncates() {
        let d = tmpdir();
        let p = d.join("s2.jsonl");
        let mut j = Journal::open(&p).unwrap();
        j.append(&JournalRecord::UserMessage {
            text: "keep".into(),
        })
        .unwrap();
        let off = j
            .append(&JournalRecord::UserMessage {
                text: "gone".into(),
            })
            .unwrap();
        j.rewind_to(off).unwrap();
        let recs = Journal::replay(&p).unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].kind(), "user_message");
        let tail = std::fs::read_to_string(format!("{}.pre-rewind", p.display())).unwrap();
        assert!(tail.contains("gone"));
    }

    #[test]
    fn corrupt_record_is_an_explicit_error() {
        let d = tmpdir();
        let p = d.join("s3.jsonl");
        std::fs::write(&p, "{\"kind\":\"user_message\",\"text\":\"ok\"}\n{torn\n").unwrap();
        assert!(Journal::replay(&p).is_err());
    }
}
