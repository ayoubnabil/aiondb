//! Persistent Raft state: current term, voted-for, and cluster commands.

#![allow(clippy::missing_errors_doc, clippy::must_use_candidate)]

use std::fs;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use aiondb_core::{DbError, DbResult};
use serde::{Deserialize, Serialize};

use crate::protocol::{Epoch, NodeId};

const MAX_RAFT_STATE_BYTES: u64 = 64 * 1024;

/// A command that can be replicated via the Raft log.
///
/// These represent cluster metadata mutations - not row-level data.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum RaftCommand {
    /// entries from previous terms.
    Noop,
    /// Register a node in the cluster.
    AddNode { node_id: u64, address: String },
    /// Remove a node from the cluster.
    RemoveNode { node_id: u64 },
    /// Assign a shard to a node.
    AssignShard {
        table_id: u64,
        shard_id: u32,
        node_id: u64,
    },
    /// Transfer a shard between nodes.
    TransferShard {
        table_id: u64,
        shard_id: u32,
        from_node: u64,
        to_node: u64,
    },
    /// Update cluster-wide configuration.
    UpdateConfig { key: String, value: String },
    /// Generic KV write replicated through the Raft log. `value =
    /// None` denotes a tombstone (delete). Bytes keep the on-the-wire
    /// representation opaque so callers can store anything that fits a
    /// byte string.
    KvWrite {
        key: Vec<u8>,
        value: Option<Vec<u8>>,
    },
}

/// Persistent Raft metadata (must survive restarts).
///
/// Raft safety requires that `current_term` and `voted_for` are durable
/// before responding to any RPC.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct PersistentState {
    /// Current term (monotonically increasing).
    pub current_term: u64,
    /// Node we voted for in the current term (None if not voted yet).
    pub voted_for: Option<u64>,
    /// Index of the highest log entry known to be committed.
    pub commit_index: u64,
}

impl PersistentState {
    /// Load state from disk, or return default if no file exists.
    pub fn load(path: &Path) -> DbResult<Self> {
        match fs::File::open(path) {
            Ok(file) => {
                let file_len = file
                    .metadata()
                    .map_err(|e| DbError::internal(format!("failed to stat Raft state: {e}")))?
                    .len();
                if file_len > MAX_RAFT_STATE_BYTES {
                    return Err(DbError::internal(format!(
                        "Raft state exceeds maximum size of {MAX_RAFT_STATE_BYTES} bytes"
                    )));
                }
                let capacity = usize::try_from(file_len)
                    .map_err(|_| DbError::internal("Raft state length does not fit in usize"))?;
                let mut contents = String::with_capacity(capacity);
                let mut limited = file.take(MAX_RAFT_STATE_BYTES.saturating_add(1));
                limited
                    .read_to_string(&mut contents)
                    .map_err(|e| DbError::internal(format!("failed to read Raft state: {e}")))?;
                if u64::try_from(contents.len()).unwrap_or(u64::MAX) > MAX_RAFT_STATE_BYTES {
                    return Err(DbError::internal(format!(
                        "Raft state grew beyond maximum size of {MAX_RAFT_STATE_BYTES} bytes while reading"
                    )));
                }
                serde_json::from_str(&contents)
                    .map_err(|e| DbError::internal(format!("failed to parse Raft state: {e}")))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(DbError::internal(format!("failed to read Raft state: {e}"))),
        }
    }

    /// Persist state to disk atomically (write tmp + fsync + rename).
    pub fn save(&self, path: &Path) -> DbResult<()> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| DbError::internal(format!("failed to serialize Raft state: {e}")))?;
        let tmp = path.with_extension("tmp");

        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|e| {
                DbError::internal(format!("failed to create Raft state directory: {e}"))
            })?;
        }

        let mut file = create_tmp_file(&tmp, "Raft state")?;
        file.write_all(json.as_bytes())
            .map_err(|e| DbError::internal(format!("failed to write Raft state tmp: {e}")))?;
        file.sync_all()
            .map_err(|e| DbError::internal(format!("failed to fsync Raft state tmp: {e}")))?;
        drop(file);
        fs::rename(&tmp, path)
            .map_err(|e| DbError::internal(format!("failed to rename Raft state: {e}")))?;
        sync_parent_dir(path, "Raft state")
    }

    /// Update term and clear `voted_for` if the new term is higher.
    /// Persists to disk. Returns true if the term was advanced.
    pub fn advance_term(&mut self, new_term: u64, path: &Path) -> DbResult<bool> {
        if new_term <= self.current_term {
            return Ok(false);
        }
        self.current_term = new_term;
        self.voted_for = None;
        self.save(path)?;
        Ok(true)
    }

    /// Record a vote for a candidate in the current term.
    /// Persists to disk before returning.
    pub fn vote_for(&mut self, candidate: NodeId, path: &Path) -> DbResult<()> {
        self.voted_for = Some(candidate.get());
        self.save(path)
    }

    /// Update the commit index and persist.
    pub fn set_commit_index(&mut self, index: u64, path: &Path) -> DbResult<()> {
        if index > self.commit_index {
            self.commit_index = index;
            self.save(path)?;
        }
        Ok(())
    }

    /// Return the current term as an Epoch.
    pub fn epoch(&self) -> Epoch {
        Epoch::new(self.current_term)
    }
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

/// Handle for managing persistent Raft state at a fixed path.
#[derive(Debug)]
pub struct PersistentStateHandle {
    state: PersistentState,
    path: PathBuf,
}

impl PersistentStateHandle {
    /// Open or create the persistent state file.
    pub fn open(path: PathBuf) -> DbResult<Self> {
        let state = PersistentState::load(&path)?;
        Ok(Self { state, path })
    }

    pub fn state(&self) -> &PersistentState {
        &self.state
    }

    pub fn current_term(&self) -> u64 {
        self.state.current_term
    }

    pub fn voted_for(&self) -> Option<u64> {
        self.state.voted_for
    }

    pub fn commit_index(&self) -> u64 {
        self.state.commit_index
    }

    pub fn epoch(&self) -> Epoch {
        self.state.epoch()
    }

    /// Advance term (clears `voted_for`). Persists before returning.
    pub fn advance_term(&mut self, new_term: u64) -> DbResult<bool> {
        self.state.advance_term(new_term, &self.path)
    }

    /// Record a vote. Persists before returning.
    pub fn vote_for(&mut self, candidate: NodeId) -> DbResult<()> {
        self.state.vote_for(candidate, &self.path)
    }

    /// Update commit index. Persists before returning.
    pub fn set_commit_index(&mut self, index: u64) -> DbResult<()> {
        self.state.set_commit_index(index, &self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_state() {
        let state = PersistentState::default();
        assert_eq!(state.current_term, 0);
        assert_eq!(state.voted_for, None);
        assert_eq!(state.commit_index, 0);
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("raft_state.json");

        let mut state = PersistentState::default();
        state.current_term = 5;
        state.voted_for = Some(42);
        state.commit_index = 10;
        state.save(&path).unwrap();

        let loaded = PersistentState::load(&path).unwrap();
        assert_eq!(loaded, state);
    }

    #[test]
    fn load_missing_file_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.json");
        let loaded = PersistentState::load(&path).unwrap();
        assert_eq!(loaded, PersistentState::default());
    }

    #[test]
    fn load_rejects_oversized_state_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("raft_state.json");
        std::fs::write(&path, vec![b' '; MAX_RAFT_STATE_BYTES as usize + 1]).unwrap();

        let error = PersistentState::load(&path).expect_err("oversized state must be rejected");
        assert!(error.to_string().contains("exceeds maximum size"));
    }

    #[test]
    fn advance_term_clears_voted_for() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("raft_state.json");

        let mut state = PersistentState::default();
        state.vote_for(NodeId::new(1), &path).unwrap();
        assert_eq!(state.voted_for, Some(1));

        state.advance_term(2, &path).unwrap();
        assert_eq!(state.current_term, 2);
        assert_eq!(state.voted_for, None);

        // Verify persistence
        let loaded = PersistentState::load(&path).unwrap();
        assert_eq!(loaded.current_term, 2);
        assert_eq!(loaded.voted_for, None);
    }

    #[test]
    fn advance_term_no_op_on_equal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("raft_state.json");

        let mut state = PersistentState {
            current_term: 5,
            voted_for: Some(3),
            commit_index: 0,
        };
        let advanced = state.advance_term(5, &path).unwrap();
        assert!(!advanced);
        assert_eq!(state.voted_for, Some(3)); // not cleared
    }

    #[test]
    fn handle_open_and_operations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("raft_state.json");

        let mut handle = PersistentStateHandle::open(path.clone()).unwrap();
        assert_eq!(handle.current_term(), 0);

        handle.advance_term(1).unwrap();
        assert_eq!(handle.current_term(), 1);

        handle.vote_for(NodeId::new(7)).unwrap();
        assert_eq!(handle.voted_for(), Some(7));

        handle.set_commit_index(42).unwrap();
        assert_eq!(handle.commit_index(), 42);

        // Reopen and verify persistence
        let handle2 = PersistentStateHandle::open(path).unwrap();
        assert_eq!(handle2.current_term(), 1);
        assert_eq!(handle2.voted_for(), Some(7));
        assert_eq!(handle2.commit_index(), 42);
    }
}
