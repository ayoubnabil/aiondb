//! Persistent Raft log: term-indexed entries for metadata consensus.

#![allow(
    clippy::cast_possible_truncation,
    clippy::missing_errors_doc,
    clippy::must_use_candidate
)]

use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use aiondb_core::{DbError, DbResult};
use serde::{Deserialize, Serialize};

use super::state::RaftCommand;

const MAX_RAFT_LOG_BYTES: u64 = 64 * 1024 * 1024;

/// A single entry in the Raft log.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RaftEntry {
    /// 1-based log index.
    pub index: u64,
    /// Term in which this entry was created.
    pub term: u64,
    /// The command to replicate.
    pub command: RaftCommand,
}

/// Persistent Raft log stored as a JSON-lines file.
///
/// Each line in the file is a JSON-serialized `RaftEntry`.
/// Truncation replaces the file atomically.
#[derive(Debug)]
pub struct RaftLog {
    entries: Vec<RaftEntry>,
    path: PathBuf,
}

impl RaftLog {
    /// Open or create the log file.
    pub fn open(path: PathBuf) -> DbResult<Self> {
        let entries = match fs::File::open(&path) {
            Ok(file) => {
                let file_len = file
                    .metadata()
                    .map_err(|e| DbError::internal(format!("failed to stat Raft log: {e}")))?
                    .len();
                if file_len > MAX_RAFT_LOG_BYTES {
                    return Err(DbError::internal(format!(
                        "Raft log exceeds maximum size of {MAX_RAFT_LOG_BYTES} bytes"
                    )));
                }
                let capacity = usize::try_from(file_len)
                    .map_err(|_| DbError::internal("Raft log length does not fit in usize"))?;
                let mut contents = String::with_capacity(capacity);
                let mut limited = file.take(MAX_RAFT_LOG_BYTES.saturating_add(1));
                limited
                    .read_to_string(&mut contents)
                    .map_err(|e| DbError::internal(format!("failed to read Raft log: {e}")))?;
                if u64::try_from(contents.len()).unwrap_or(u64::MAX) > MAX_RAFT_LOG_BYTES {
                    return Err(DbError::internal(format!(
                        "Raft log grew beyond maximum size of {MAX_RAFT_LOG_BYTES} bytes while reading"
                    )));
                }
                let mut entries = Vec::new();
                for line in contents.lines() {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let entry: RaftEntry = serde_json::from_str(line).map_err(|e| {
                        DbError::internal(format!("failed to parse Raft log entry: {e}"))
                    })?;
                    if entry.index == 0 {
                        return Err(DbError::internal("Raft log entry index must be >= 1"));
                    }
                    // Validate contiguity on load.
                    if let Some(prev) = entries.last() {
                        let prev: &RaftEntry = prev;
                        let expected_index = next_log_index(prev.index, "Raft log load")?;
                        if entry.index != expected_index {
                            return Err(DbError::internal(format!(
                                "Raft log index gap: expected {}, got {}",
                                expected_index, entry.index
                            )));
                        }
                    }
                    entries.push(entry);
                }
                entries
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                return Err(DbError::internal(format!("failed to read Raft log: {e}")));
            }
        };
        Ok(Self { entries, path })
    }

    /// Return the index of the last entry, or 0 if the log is empty.
    pub fn last_index(&self) -> u64 {
        self.entries.last().map_or(0, |e| e.index)
    }

    /// Return the term of the last entry, or 0 if the log is empty.
    pub fn last_term(&self) -> u64 {
        self.entries.last().map_or(0, |e| e.term)
    }

    /// Return the term of the entry at the given index, or 0 if not found.
    pub fn term_at(&self, index: u64) -> u64 {
        self.get(index).map_or(0, |e| e.term)
    }

    /// Get an entry by index.
    pub fn get(&self, index: u64) -> Option<&RaftEntry> {
        if index == 0 || self.entries.is_empty() {
            return None;
        }
        let first = self.entries[0].index;
        if index < first {
            return None;
        }
        let offset = usize::try_from(index - first).ok()?;
        self.entries.get(offset)
    }

    /// Get entries from `start_index` (inclusive) to the end of the log.
    pub fn entries_from(&self, start_index: u64) -> &[RaftEntry] {
        if self.entries.is_empty() || start_index == 0 {
            return &[];
        }
        let first = self.entries[0].index;
        if start_index < first {
            return &self.entries;
        }
        let Ok(offset) = usize::try_from(start_index - first) else {
            return &[];
        };
        if offset >= self.entries.len() {
            return &[];
        }
        &self.entries[offset..]
    }

    /// Number of entries in the log.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True if the log has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Append a new entry to the log and persist.
    pub fn append(&mut self, term: u64, command: RaftCommand) -> DbResult<u64> {
        let index = next_log_index(self.last_index(), "Raft log append")?;
        let entry = RaftEntry {
            index,
            term,
            command,
        };
        self.entries.push(entry);
        self.persist()?;
        Ok(index)
    }

    /// `Fix #5`: append entries received from a leader with contiguity
    /// validation. `expected_prev_index` is the `prev_log_index` from the
    /// `AppendEntries` RPC - the first entry must have index
    /// `expected_prev_index + 1`.
    ///
    /// If there's a conflict (same index, different term), truncate from
    /// the conflict point and replace with the leader's entries.
    pub fn append_entries_checked(
        &mut self,
        expected_prev_index: u64,
        entries: &[RaftEntry],
    ) -> DbResult<()> {
        if entries.is_empty() {
            return Ok(());
        }

        // Validate that entries start right after expected_prev_index.
        let expected_first = next_log_index(expected_prev_index, "Raft log append entries")?;
        if entries[0].index != expected_first {
            return Err(DbError::internal(format!(
                "Raft log: expected first entry index {}, got {}",
                expected_first, entries[0].index
            )));
        }

        // Validate internal contiguity of the incoming entries.
        for window in entries.windows(2) {
            let expected_next = next_log_index(window[0].index, "Raft log append entries")?;
            if window[1].index != expected_next {
                return Err(DbError::internal(format!(
                    "Raft log: non-contiguous entries: {} followed by {}",
                    window[0].index, window[1].index
                )));
            }
        }

        for entry in entries {
            if let Some(existing) = self.get(entry.index) {
                if existing.term == entry.term {
                    continue;
                }
                self.truncate_from(entry.index);
            }
            self.entries.push(entry.clone());
        }
        // Always persist when the leader sends entries - even if all were
        // duplicates, the fsync ensures prior writes are durable.
        self.persist()?;
        Ok(())
    }

    /// Truncate the log from the given index (inclusive) to the end.
    fn truncate_from(&mut self, from_index: u64) {
        if self.entries.is_empty() {
            return;
        }
        let first = self.entries[0].index;
        if from_index < first {
            self.entries.clear();
            return;
        }
        let Ok(offset) = usize::try_from(from_index - first) else {
            self.entries.clear();
            return;
        };
        self.entries.truncate(offset);
    }

    /// [Fix #8] Persist the entire log to disk atomically with fsync.
    fn persist(&self) -> DbResult<()> {
        use std::io::Write;

        let mut buf = String::new();
        for entry in &self.entries {
            let line = serde_json::to_string(entry).map_err(|e| {
                DbError::internal(format!("failed to serialize Raft log entry: {e}"))
            })?;
            buf.push_str(&line);
            buf.push('\n');
        }
        let tmp = self.path.with_extension("tmp");
        {
            if let Some(parent) = self
                .path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                fs::create_dir_all(parent).map_err(|e| {
                    DbError::internal(format!("failed to create Raft log directory: {e}"))
                })?;
            }
            let file = create_tmp_file(&tmp, "Raft log")?;
            let mut writer = std::io::BufWriter::new(file);
            writer
                .write_all(buf.as_bytes())
                .map_err(|e| DbError::internal(format!("failed to write Raft log tmp: {e}")))?;
            writer
                .flush()
                .map_err(|e| DbError::internal(format!("failed to flush Raft log tmp: {e}")))?;
            let file = writer
                .into_inner()
                .map_err(|e| DbError::internal(format!("failed to unwrap Raft log writer: {e}")))?;
            file.sync_all()
                .map_err(|e| DbError::internal(format!("failed to fsync Raft log tmp: {e}")))?;
        }
        fs::rename(&tmp, &self.path)
            .map_err(|e| DbError::internal(format!("failed to rename Raft log: {e}")))?;
        sync_parent_dir(&self.path, "Raft log")
    }
}

fn next_log_index(index: u64, operation: &str) -> DbResult<u64> {
    index
        .checked_add(1)
        .ok_or_else(|| DbError::internal(format!("{operation}: log index overflow")))
}

fn create_tmp_file(path: &Path, context: &str) -> DbResult<fs::File> {
    for attempt in 0..2 {
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(file) => return Ok(file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && attempt == 0 => {
                fs::remove_file(path).map_err(|remove_error| {
                    DbError::internal(format!(
                        "failed to cleanup stale {context} tmp {}: {remove_error}",
                        path.display()
                    ))
                })?;
            }
            Err(error) => {
                return Err(DbError::internal(format!(
                    "failed to create {context} tmp {}: {error}",
                    path.display()
                )));
            }
        }
    }

    Err(DbError::internal(format!("failed to create {context} tmp")))
}

fn sync_parent_dir(path: &Path, context: &str) -> DbResult<()> {
    aiondb_core::bounded_io::sync_parent_dir(path).map_err(|error| {
        let parent = path.parent().unwrap_or(path);
        DbError::internal(format!(
            "failed to fsync {context} parent directory {}: {error}",
            parent.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_log() {
        let dir = tempfile::tempdir().unwrap();
        let log = RaftLog::open(dir.path().join("raft.log")).unwrap();
        assert!(log.is_empty());
        assert_eq!(log.last_index(), 0);
        assert_eq!(log.last_term(), 0);
        assert_eq!(log.len(), 0);
    }

    #[test]
    fn append_and_read() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = RaftLog::open(dir.path().join("raft.log")).unwrap();

        let idx = log.append(1, RaftCommand::Noop).unwrap();
        assert_eq!(idx, 1);
        assert_eq!(log.last_index(), 1);
        assert_eq!(log.last_term(), 1);

        let idx2 = log
            .append(
                1,
                RaftCommand::AddNode {
                    node_id: 42,
                    address: "127.0.0.1:5433".to_owned(),
                },
            )
            .unwrap();
        assert_eq!(idx2, 2);
        assert_eq!(log.len(), 2);

        let entry = log.get(1).unwrap();
        assert_eq!(entry.term, 1);
        assert!(matches!(entry.command, RaftCommand::Noop));

        let entry2 = log.get(2).unwrap();
        assert!(matches!(entry2.command, RaftCommand::AddNode { .. }));
    }

    #[test]
    fn persistence_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("raft.log");

        {
            let mut log = RaftLog::open(path.clone()).unwrap();
            log.append(1, RaftCommand::Noop).unwrap();
            log.append(2, RaftCommand::RemoveNode { node_id: 1 })
                .unwrap();
        }

        let log = RaftLog::open(path).unwrap();
        assert_eq!(log.len(), 2);
        assert_eq!(log.last_term(), 2);
        assert_eq!(log.get(1).unwrap().term, 1);
        assert_eq!(log.get(2).unwrap().term, 2);
    }

    #[test]
    fn append_replaces_stale_tmp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("raft.log");
        std::fs::write(path.with_extension("tmp"), b"stale").unwrap();

        let mut log = RaftLog::open(path.clone()).unwrap();
        log.append(1, RaftCommand::Noop).unwrap();

        assert!(!path.with_extension("tmp").exists());
        let reopened = RaftLog::open(path).unwrap();
        assert_eq!(reopened.len(), 1);
    }

    #[test]
    fn open_rejects_zero_index_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("raft.log");
        let entry = RaftEntry {
            index: 0,
            term: 1,
            command: RaftCommand::Noop,
        };
        std::fs::write(&path, serde_json::to_string(&entry).unwrap()).unwrap();

        let err = RaftLog::open(path).unwrap_err();
        assert!(err.to_string().contains("index must be >= 1"));
    }

    #[test]
    fn open_rejects_oversized_log_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("raft.log");
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        file.set_len(MAX_RAFT_LOG_BYTES + 1).unwrap();

        let err = RaftLog::open(path).unwrap_err();
        assert!(err.to_string().contains("exceeds maximum size"));
    }

    #[test]
    fn append_entries_checked_with_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = RaftLog::open(dir.path().join("raft.log")).unwrap();

        log.append(1, RaftCommand::Noop).unwrap();
        log.append(1, RaftCommand::Noop).unwrap();
        log.append(1, RaftCommand::Noop).unwrap();
        assert_eq!(log.len(), 3);

        // Leader sends entries starting at index 2 with term 2.
        let leader_entries = vec![
            RaftEntry {
                index: 2,
                term: 2,
                command: RaftCommand::Noop,
            },
            RaftEntry {
                index: 3,
                term: 2,
                command: RaftCommand::Noop,
            },
        ];
        log.append_entries_checked(1, &leader_entries).unwrap();

        assert_eq!(log.len(), 3);
        assert_eq!(log.get(1).unwrap().term, 1);
        assert_eq!(log.get(2).unwrap().term, 2);
        assert_eq!(log.get(3).unwrap().term, 2);
    }

    // [Fix #5] Non-contiguous entries are rejected.
    #[test]
    fn append_entries_checked_rejects_gap() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = RaftLog::open(dir.path().join("raft.log")).unwrap();

        let entries = vec![
            RaftEntry {
                index: 1,
                term: 1,
                command: RaftCommand::Noop,
            },
            RaftEntry {
                index: 3,
                term: 1,
                command: RaftCommand::Noop,
            }, // gap!
        ];
        assert!(log.append_entries_checked(0, &entries).is_err());
    }

    // [Fix #5] Wrong starting index is rejected.
    #[test]
    fn append_entries_checked_rejects_wrong_start() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = RaftLog::open(dir.path().join("raft.log")).unwrap();

        let entries = vec![RaftEntry {
            index: 5,
            term: 1,
            command: RaftCommand::Noop,
        }];
        // expected_prev_index=0, so first entry should be 1, not 5.
        assert!(log.append_entries_checked(0, &entries).is_err());
    }

    #[test]
    fn append_rejects_index_overflow() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = RaftLog {
            entries: vec![RaftEntry {
                index: u64::MAX,
                term: 1,
                command: RaftCommand::Noop,
            }],
            path: dir.path().join("raft.log"),
        };

        let err = log.append(1, RaftCommand::Noop).unwrap_err();
        assert!(err.to_string().contains("overflow"));
    }

    #[test]
    fn append_entries_checked_rejects_prev_index_overflow() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = RaftLog::open(dir.path().join("raft.log")).unwrap();
        let entries = vec![RaftEntry {
            index: u64::MAX,
            term: 1,
            command: RaftCommand::Noop,
        }];

        let err = log.append_entries_checked(u64::MAX, &entries).unwrap_err();
        assert!(err.to_string().contains("overflow"));
    }

    #[test]
    fn entries_from_range() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = RaftLog::open(dir.path().join("raft.log")).unwrap();
        log.append(1, RaftCommand::Noop).unwrap();
        log.append(1, RaftCommand::Noop).unwrap();
        log.append(2, RaftCommand::Noop).unwrap();

        let from_2 = log.entries_from(2);
        assert_eq!(from_2.len(), 2);
        assert_eq!(from_2[0].index, 2);
        assert_eq!(from_2[1].index, 3);

        let from_4 = log.entries_from(4);
        assert!(from_4.is_empty());
    }

    #[test]
    fn term_at() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = RaftLog::open(dir.path().join("raft.log")).unwrap();
        log.append(1, RaftCommand::Noop).unwrap();
        log.append(3, RaftCommand::Noop).unwrap();

        assert_eq!(log.term_at(1), 1);
        assert_eq!(log.term_at(2), 3);
        assert_eq!(log.term_at(0), 0);
        assert_eq!(log.term_at(3), 0);
    }
}
