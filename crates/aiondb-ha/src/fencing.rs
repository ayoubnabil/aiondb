#![allow(
    clippy::items_after_statements,
    clippy::missing_errors_doc,
    clippy::must_use_candidate
)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use aiondb_core::{DbError, DbResult};
use serde::{Deserialize, Serialize};

use crate::protocol::{current_timestamp_us, Epoch, NodeId};

const MAX_FENCING_TOKEN_BYTES: u64 = 4096;

/// Persistent fencing token that proves leadership for a given epoch.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FencingToken {
    pub epoch: u64,
    pub node_id: u64,
    pub timestamp_us: u64,
}

/// Guards against split-brain by ensuring only the holder of the current
/// epoch's fencing token can act as primary.
pub struct FencingGuard {
    current_epoch: AtomicU64,
    max_epoch: AtomicU64,
    node_id: NodeId,
    token_path: Option<PathBuf>,
}

impl FencingGuard {
    pub fn new(node_id: NodeId, token_path: Option<PathBuf>) -> Self {
        let guard = Self {
            current_epoch: AtomicU64::new(0),
            max_epoch: AtomicU64::new(0),
            node_id,
            token_path,
        };
        // Seed the in-memory epoch from any persisted token so a process
        // restart can never quietly regress to 0 (which would let a fenced
        // node validate stale-epoch requests).
        if let Ok(Some(token)) = guard.load_persisted() {
            guard.current_epoch.store(token.epoch, Ordering::Release);
            guard.max_epoch.store(token.epoch, Ordering::Release);
        }
        guard
    }

    /// Create a fencing token for the given epoch, persist it if a path is
    /// configured, and update the current epoch. Refuses to regress: an
    /// `acquire` for an epoch ≤ the currently-held one is rejected so a
    pub fn acquire(&self, epoch: Epoch) -> DbResult<FencingToken> {
        let new_epoch = epoch.get();
        // CAS-like monotonic guard on the atomic.
        loop {
            let observed = self.max_epoch.load(Ordering::Acquire);
            if new_epoch <= observed {
                return Err(DbError::internal(format!(
                    "fencing acquire rejected: epoch {new_epoch} <= current {observed}"
                )));
            }
            if self
                .max_epoch
                .compare_exchange(observed, new_epoch, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break;
            }
        }
        let token = FencingToken {
            epoch: new_epoch,
            node_id: self.node_id.get(),
            timestamp_us: current_timestamp_us(),
        };
        self.current_epoch.store(new_epoch, Ordering::Release);
        // Persist after the in-memory epoch has been bumped so a crash
        // between bump and persist re-loads the (lower) old token but
        // next legitimate acquire will still be rejected unless it's
        // strictly higher than the persisted floor.
        self.persist(&token)?;
        Ok(token)
    }

    /// Check whether the given epoch matches the currently held fencing token.
    pub fn validate(&self, epoch: Epoch) -> bool {
        self.current_epoch.load(Ordering::Acquire) == epoch.get()
    }

    /// Return the current fencing epoch.
    pub fn current_epoch(&self) -> Epoch {
        Epoch::new(self.current_epoch.load(Ordering::Acquire))
    }

    /// Release the fencing token (set epoch to 0).
    pub fn release(&self) {
        self.current_epoch.store(0, Ordering::Release);
    }

    /// Load a previously persisted fencing token from disk.
    pub fn load_persisted(&self) -> DbResult<Option<FencingToken>> {
        let Some(path) = &self.token_path else {
            return Ok(None);
        };
        if !path.exists() {
            return Ok(None);
        }
        let contents = read_fencing_token_file(path)?;
        let token: FencingToken = serde_json::from_str(&contents)
            .map_err(|e| DbError::internal(format!("failed to parse fencing token: {e}")))?;
        Ok(Some(token))
    }

    fn persist(&self, token: &FencingToken) -> DbResult<()> {
        use std::io::Write;

        let Some(path) = &self.token_path else {
            return Ok(());
        };
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)
                .map_err(|e| DbError::internal(format!("failed to create fencing dir: {e}")))?;
        }
        let json = serde_json::to_string(token)
            .map_err(|e| DbError::internal(format!("failed to serialize fencing token: {e}")))?;
        let tmp_path = path.with_extension("tmp");
        let mut tmp_file = create_tmp_file(&tmp_path)?;
        tmp_file
            .write_all(json.as_bytes())
            .map_err(|e| DbError::internal(format!("failed to write fencing token tmp: {e}")))?;
        tmp_file
            .sync_all()
            .map_err(|e| DbError::internal(format!("failed to sync fencing token tmp: {e}")))?;
        drop(tmp_file);
        std::fs::rename(&tmp_path, path)
            .map_err(|e| DbError::internal(format!("failed to rename fencing token: {e}")))?;
        sync_parent_dir(path)?;
        Ok(())
    }
}

fn read_fencing_token_file(path: &Path) -> DbResult<String> {
    use std::io::Read as _;

    let file = std::fs::File::open(path)
        .map_err(|e| DbError::internal(format!("failed to read fencing token: {e}")))?;
    let metadata = file
        .metadata()
        .map_err(|e| DbError::internal(format!("failed to inspect fencing token: {e}")))?;
    if metadata.len() > MAX_FENCING_TOKEN_BYTES {
        return Err(DbError::program_limit(format!(
            "fencing token exceeds maximum {MAX_FENCING_TOKEN_BYTES} bytes"
        )));
    }

    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    let mut reader = file.take(MAX_FENCING_TOKEN_BYTES.saturating_add(1));
    reader
        .read_to_end(&mut bytes)
        .map_err(|e| DbError::internal(format!("failed to read fencing token: {e}")))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_FENCING_TOKEN_BYTES {
        return Err(DbError::program_limit(format!(
            "fencing token grew beyond maximum {MAX_FENCING_TOKEN_BYTES} bytes while reading"
        )));
    }

    String::from_utf8(bytes)
        .map_err(|e| DbError::internal(format!("failed to decode fencing token as UTF-8: {e}")))
}

fn create_tmp_file(path: &Path) -> DbResult<std::fs::File> {
    for attempt in 0..2 {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }

        match options.open(path) {
            Ok(file) => return Ok(file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && attempt == 0 => {
                std::fs::remove_file(path).map_err(|remove_error| {
                    DbError::internal(format!(
                        "failed to cleanup stale fencing tmp {}: {remove_error}",
                        path.display()
                    ))
                })?;
            }
            Err(error) => {
                return Err(DbError::internal(format!(
                    "failed to create fencing token tmp {}: {error}",
                    path.display()
                )));
            }
        }
    }

    Err(DbError::internal("failed to create fencing token tmp"))
}

fn sync_parent_dir(path: &Path) -> DbResult<()> {
    aiondb_core::bounded_io::sync_parent_dir(path).map_err(|error| {
        let parent = path.parent().unwrap_or(path);
        DbError::internal(format!(
            "failed to sync fencing token parent directory {}: {error}",
            parent.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_test_dir(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "aiondb-fencing-test-{name}-{}-{nanos}",
            std::process::id()
        ))
    }

    #[test]
    fn acquire_and_validate() {
        let guard = FencingGuard::new(NodeId::new(1), None);
        let token = guard.acquire(Epoch::new(5)).unwrap();
        assert_eq!(token.epoch, 5);
        assert_eq!(token.node_id, 1);
        assert!(guard.validate(Epoch::new(5)));
        assert!(!guard.validate(Epoch::new(4)));
    }

    #[test]
    fn release_resets_epoch() {
        let guard = FencingGuard::new(NodeId::new(1), None);
        guard.acquire(Epoch::new(3)).unwrap();
        assert!(guard.validate(Epoch::new(3)));
        guard.release();
        assert_eq!(guard.current_epoch(), Epoch::new(0));
        assert!(!guard.validate(Epoch::new(3)));
    }

    #[test]
    fn release_does_not_allow_epoch_regression() {
        let guard = FencingGuard::new(NodeId::new(1), None);
        guard.acquire(Epoch::new(7)).unwrap();
        guard.release();

        let error = guard
            .acquire(Epoch::new(5))
            .expect_err("release must not lower the monotonic fencing floor");
        assert!(error.to_string().contains("epoch 5 <= current 7"));
        assert!(guard.acquire(Epoch::new(8)).is_ok());
    }

    #[test]
    fn release_does_not_regress_persisted_fencing_floor() {
        let dir = unique_test_dir("release-floor");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fencing.json");

        let guard = FencingGuard::new(NodeId::new(42), Some(path.clone()));
        guard.acquire(Epoch::new(7)).unwrap();
        guard.release();
        assert_eq!(guard.current_epoch(), Epoch::new(0));
        assert!(guard.acquire(Epoch::new(5)).is_err());

        let restarted = FencingGuard::new(NodeId::new(42), Some(path.clone()));
        assert_eq!(restarted.current_epoch(), Epoch::new(7));
        assert!(restarted.acquire(Epoch::new(5)).is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn persist_and_load() {
        let dir = unique_test_dir("persist-and-load");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fencing.json");

        let guard = FencingGuard::new(NodeId::new(42), Some(path.clone()));
        let token = guard.acquire(Epoch::new(7)).unwrap();

        let loaded = guard.load_persisted().unwrap().unwrap();
        assert_eq!(loaded.epoch, token.epoch);
        assert_eq!(loaded.node_id, token.node_id);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn persist_creates_private_token_file() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = unique_test_dir("private-file");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fencing.json");

        let guard = FencingGuard::new(NodeId::new(42), Some(path.clone()));
        guard.acquire(Epoch::new(7)).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_missing_returns_none() {
        let guard = FencingGuard::new(
            NodeId::new(1),
            Some(PathBuf::from("/tmp/aiondb-nonexistent-fencing-token.json")),
        );
        assert!(guard.load_persisted().unwrap().is_none());
    }

    #[test]
    fn load_persisted_rejects_oversized_token_file() {
        let dir = unique_test_dir("oversized-token");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fencing.json");
        let file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .unwrap();
        file.set_len(MAX_FENCING_TOKEN_BYTES + 1).unwrap();

        let guard = FencingGuard::new(NodeId::new(1), Some(path));
        let err = guard
            .load_persisted()
            .expect_err("oversized fencing token should be rejected");
        assert!(
            err.to_string().contains("exceeds maximum"),
            "unexpected error: {err}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_no_path_returns_none() {
        let guard = FencingGuard::new(NodeId::new(1), None);
        assert!(guard.load_persisted().unwrap().is_none());
    }
}
