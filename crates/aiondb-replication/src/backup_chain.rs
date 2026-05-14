//! Backup chain index.
//!
//! Tracks a chain of `BackupFile`s : one `Full` base plus zero or
//! more `Incremental`s. Restore picks the most recent full plus
//! every incremental created after it. The chain is invalidated
//! when a full is deleted while incrementals depending on it still
//! exist.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::SystemTime;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackupKind {
    Full,
    Incremental,
}

#[derive(Clone, Debug)]
pub struct BackupFile {
    pub id: u64,
    pub kind: BackupKind,
    pub start_lsn: u64,
    pub end_lsn: u64,
    pub size_bytes: u64,
    pub created_at: SystemTime,
}

#[derive(Clone, Debug, Default)]
pub struct BackupChain {
    inner: Arc<std::sync::Mutex<BTreeMap<u64, BackupFile>>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChainError {
    NoFullBackup,
    BrokenChain,
}

impl BackupChain {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, file: BackupFile) {
        self.inner.lock().unwrap().insert(file.id, file);
    }

    pub fn remove(&self, id: u64) -> bool {
        self.inner.lock().unwrap().remove(&id).is_some()
    }

    pub fn latest_full(&self) -> Option<BackupFile> {
        self.inner
            .lock()
            .unwrap()
            .values()
            .filter(|f| f.kind == BackupKind::Full)
            .max_by_key(|f| f.created_at)
            .cloned()
    }

    pub fn restore_plan(&self) -> Result<Vec<BackupFile>, ChainError> {
        let full = self.latest_full().ok_or(ChainError::NoFullBackup)?;
        let g = self.inner.lock().unwrap();
        let mut incrementals: Vec<BackupFile> = g
            .values()
            .filter(|f| {
                f.kind == BackupKind::Incremental
                    && f.start_lsn >= full.end_lsn
                    && f.created_at > full.created_at
            })
            .cloned()
            .collect();
        incrementals.sort_by_key(|f| f.start_lsn);
        // Verify contiguous chain.
        let mut cursor = full.end_lsn;
        for inc in &incrementals {
            if inc.start_lsn != cursor {
                return Err(ChainError::BrokenChain);
            }
            cursor = inc.end_lsn;
        }
        let mut out = vec![full];
        out.extend(incrementals);
        Ok(out)
    }

    pub fn total_size(&self) -> u64 {
        self.inner
            .lock()
            .unwrap()
            .values()
            .map(|f| f.size_bytes)
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn full(id: u64, end_lsn: u64, age_s: u64) -> BackupFile {
        BackupFile {
            id,
            kind: BackupKind::Full,
            start_lsn: 0,
            end_lsn,
            size_bytes: 1024,
            created_at: SystemTime::UNIX_EPOCH + Duration::from_secs(age_s),
        }
    }
    fn inc(id: u64, start: u64, end: u64, age_s: u64) -> BackupFile {
        BackupFile {
            id,
            kind: BackupKind::Incremental,
            start_lsn: start,
            end_lsn: end,
            size_bytes: 512,
            created_at: SystemTime::UNIX_EPOCH + Duration::from_secs(age_s),
        }
    }

    #[test]
    fn no_full_returns_error() {
        let c = BackupChain::new();
        c.register(inc(1, 100, 200, 1));
        assert_eq!(c.restore_plan().unwrap_err(), ChainError::NoFullBackup);
    }

    #[test]
    fn full_only_returns_single_step() {
        let c = BackupChain::new();
        c.register(full(1, 1000, 0));
        let plan = c.restore_plan().unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].kind, BackupKind::Full);
    }

    #[test]
    fn full_plus_incrementals_chained() {
        let c = BackupChain::new();
        c.register(full(1, 100, 0));
        c.register(inc(2, 100, 200, 1));
        c.register(inc(3, 200, 300, 2));
        let plan = c.restore_plan().unwrap();
        assert_eq!(plan.len(), 3);
    }

    #[test]
    fn broken_chain_detected() {
        let c = BackupChain::new();
        c.register(full(1, 100, 0));
        c.register(inc(2, 100, 200, 1));
        c.register(inc(3, 250, 300, 2));
        assert_eq!(c.restore_plan().unwrap_err(), ChainError::BrokenChain);
    }

    #[test]
    fn latest_full_picks_most_recent() {
        let c = BackupChain::new();
        c.register(full(1, 100, 0));
        c.register(full(2, 200, 5));
        let latest = c.latest_full().unwrap();
        assert_eq!(latest.id, 2);
    }

    #[test]
    fn total_size_aggregates() {
        let c = BackupChain::new();
        c.register(full(1, 100, 0));
        c.register(inc(2, 100, 200, 1));
        assert_eq!(c.total_size(), 1024 + 512);
    }
}
