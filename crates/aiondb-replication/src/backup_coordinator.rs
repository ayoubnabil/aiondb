//! Distributed backup coordinator.
//!
//! Drives the choreography for cluster-wide backups :
//!
//! 1. **Full** : pick a closed timestamp, fan out a snapshot request
//!    to every range, collect their per-range snapshots into a single
//!    manifest.
//! 2. **Incremental** : reuse a prior full backup's timestamp as the
//!    base, ship only the WAL entries `(base_ts, now)` per range.
//! 3. **Restore** : iterate the manifest in deterministic order
//!    (range_id ascending) so range-by-range restore is reproducible.
//!
//! The coordinator owns metadata; the actual byte streams are
//! produced by `base_backup.rs` + WAL replay. Tests verify the
//! manifest contracts.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum BackupKind {
    Full,
    Incremental,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RangeManifestEntry {
    pub range_id: u64,
    pub start_key: Vec<u8>,
    pub end_key: Vec<u8>,
    pub bytes: u64,
    pub from_ts_us: u64,
    pub to_ts_us: u64,
    pub artefact_path: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BackupManifest {
    pub id: String,
    pub kind: BackupKind,
    pub created_at_us: u64,
    pub closed_ts_us: u64,
    pub base_manifest_id: Option<String>,
    pub ranges: Vec<RangeManifestEntry>,
}

impl BackupManifest {
    pub fn total_bytes(&self) -> u64 {
        self.ranges.iter().map(|r| r.bytes).sum()
    }

    pub fn range_count(&self) -> usize {
        self.ranges.len()
    }
}

/// In-memory coordinator. Cheap to clone.
#[derive(Clone, Debug, Default)]
pub struct BackupCoordinator {
    inner: Arc<std::sync::Mutex<BTreeMap<String, BackupManifest>>>,
    seq: Arc<std::sync::atomic::AtomicU64>,
}

impl BackupCoordinator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a fresh full backup. Returns the assigned manifest id.
    pub fn start_full(&self, closed_ts_us: u64) -> String {
        self.allocate(BackupKind::Full, closed_ts_us, None)
    }

    /// Start an incremental backup based on `base_id`.
    pub fn start_incremental(&self, base_id: &str, closed_ts_us: u64) -> Option<String> {
        let guard = self.inner.lock().unwrap();
        if !guard.contains_key(base_id) {
            return None;
        }
        drop(guard);
        Some(self.allocate(
            BackupKind::Incremental,
            closed_ts_us,
            Some(base_id.to_owned()),
        ))
    }

    fn allocate(&self, kind: BackupKind, closed_ts_us: u64, base_id: Option<String>) -> String {
        let id = format!(
            "bkp-{}-{:06}",
            now_us(),
            self.seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        );
        let manifest = BackupManifest {
            id: id.clone(),
            kind,
            created_at_us: now_us(),
            closed_ts_us,
            base_manifest_id: base_id,
            ranges: Vec::new(),
        };
        self.inner.lock().unwrap().insert(id.clone(), manifest);
        id
    }

    /// Add or replace a range entry in an in-flight manifest.
    pub fn record_range(&self, manifest_id: &str, entry: RangeManifestEntry) -> bool {
        let mut guard = self.inner.lock().unwrap();
        let Some(m) = guard.get_mut(manifest_id) else {
            return false;
        };
        // Replace if a previous entry for the same range_id exists.
        if let Some(slot) = m.ranges.iter_mut().find(|r| r.range_id == entry.range_id) {
            *slot = entry;
        } else {
            m.ranges.push(entry);
        }
        true
    }

    /// Finalize a manifest : sort ranges by id, mark as complete.
    pub fn finalize(&self, manifest_id: &str) -> Option<BackupManifest> {
        let mut guard = self.inner.lock().unwrap();
        let m = guard.get_mut(manifest_id)?;
        m.ranges.sort_by_key(|r| r.range_id);
        Some(m.clone())
    }

    pub fn manifest(&self, id: &str) -> Option<BackupManifest> {
        self.inner.lock().unwrap().get(id).cloned()
    }

    pub fn list(&self) -> Vec<BackupManifest> {
        let guard = self.inner.lock().unwrap();
        let mut out: Vec<_> = guard.values().cloned().collect();
        out.sort_by_key(|m| m.created_at_us);
        out
    }

    pub fn drop(&self, id: &str) -> Option<BackupManifest> {
        self.inner.lock().unwrap().remove(id)
    }
}

fn now_us() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_micros()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(range: u64, bytes: u64) -> RangeManifestEntry {
        RangeManifestEntry {
            range_id: range,
            start_key: format!("k-{range}").into_bytes(),
            end_key: format!("k-{}", range + 1).into_bytes(),
            bytes,
            from_ts_us: 0,
            to_ts_us: 100,
            artefact_path: format!("/backups/range-{range}.bin"),
        }
    }

    #[test]
    fn full_backup_carries_zero_base_id() {
        let c = BackupCoordinator::new();
        let id = c.start_full(1_000);
        let m = c.manifest(&id).unwrap();
        assert_eq!(m.kind, BackupKind::Full);
        assert!(m.base_manifest_id.is_none());
    }

    #[test]
    fn incremental_requires_existing_base() {
        let c = BackupCoordinator::new();
        assert!(c.start_incremental("missing", 1_000).is_none());
        let base = c.start_full(1_000);
        let incr = c.start_incremental(&base, 2_000).unwrap();
        let m = c.manifest(&incr).unwrap();
        assert_eq!(m.kind, BackupKind::Incremental);
        assert_eq!(m.base_manifest_id.as_deref(), Some(base.as_str()));
    }

    #[test]
    fn record_range_replaces_previous_entry_for_same_range() {
        let c = BackupCoordinator::new();
        let id = c.start_full(1_000);
        c.record_range(&id, entry(1, 100));
        c.record_range(&id, entry(1, 200)); // same range_id, fresh data
        let m = c.manifest(&id).unwrap();
        assert_eq!(m.ranges.len(), 1);
        assert_eq!(m.ranges[0].bytes, 200);
    }

    #[test]
    fn finalize_sorts_ranges_by_id() {
        let c = BackupCoordinator::new();
        let id = c.start_full(1_000);
        c.record_range(&id, entry(3, 100));
        c.record_range(&id, entry(1, 100));
        c.record_range(&id, entry(2, 100));
        let m = c.finalize(&id).unwrap();
        let ids: Vec<u64> = m.ranges.iter().map(|r| r.range_id).collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[test]
    fn total_bytes_sums_per_range_payloads() {
        let c = BackupCoordinator::new();
        let id = c.start_full(0);
        c.record_range(&id, entry(1, 1024));
        c.record_range(&id, entry(2, 2048));
        let m = c.finalize(&id).unwrap();
        assert_eq!(m.total_bytes(), 3072);
        assert_eq!(m.range_count(), 2);
    }

    #[test]
    fn list_orders_manifests_by_creation_time() {
        let c = BackupCoordinator::new();
        let _ = c.start_full(0);
        std::thread::sleep(std::time::Duration::from_millis(2));
        let _ = c.start_full(0);
        let list = c.list();
        assert_eq!(list.len(), 2);
        assert!(list[0].created_at_us <= list[1].created_at_us);
    }

    #[test]
    fn drop_removes_manifest() {
        let c = BackupCoordinator::new();
        let id = c.start_full(0);
        assert!(c.drop(&id).is_some());
        assert!(c.manifest(&id).is_none());
    }
}
