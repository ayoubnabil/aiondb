//! Per-range key envelope manager.
//!
//! Tracks the active Data Encryption Key (DEK) per range plus
//! previous DEKs needed to decrypt older data (when rotation has not
//! finished re-encrypting). Decryption falls back through the
//! version list until a key matches.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::range_descriptor::RangeId;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DekVersion {
    pub version: u32,
    pub key_id: String,
}

#[derive(Clone, Debug, Default)]
pub struct KeyEnvelopeManager {
    inner: Arc<std::sync::Mutex<BTreeMap<RangeId, Vec<DekVersion>>>>,
}

impl KeyEnvelopeManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rotate to a fresh DEK. Older DEKs remain available for
    /// decryption until manually retired.
    pub fn rotate(&self, range: RangeId, new_key_id: impl Into<String>) -> u32 {
        let mut guard = self.inner.lock().unwrap();
        let versions = guard.entry(range).or_default();
        let next_version = versions.last().map(|v| v.version + 1).unwrap_or(1);
        versions.push(DekVersion {
            version: next_version,
            key_id: new_key_id.into(),
        });
        next_version
    }

    pub fn active(&self, range: RangeId) -> Option<DekVersion> {
        self.inner
            .lock()
            .unwrap()
            .get(&range)
            .and_then(|v| v.last().cloned())
    }

    pub fn all_versions(&self, range: RangeId) -> Vec<DekVersion> {
        self.inner
            .lock()
            .unwrap()
            .get(&range)
            .cloned()
            .unwrap_or_default()
    }

    /// Retire a DEK (after re-encryption completes).
    pub fn retire(&self, range: RangeId, version: u32) {
        if let Some(versions) = self.inner.lock().unwrap().get_mut(&range) {
            versions.retain(|v| v.version != version);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(n: u64) -> RangeId {
        RangeId::new(n)
    }

    #[test]
    fn rotate_starts_at_version_1() {
        let m = KeyEnvelopeManager::new();
        assert_eq!(m.rotate(range(1), "key-a"), 1);
        assert_eq!(m.rotate(range(1), "key-b"), 2);
    }

    #[test]
    fn active_returns_latest_dek() {
        let m = KeyEnvelopeManager::new();
        m.rotate(range(1), "key-a");
        m.rotate(range(1), "key-b");
        let active = m.active(range(1)).unwrap();
        assert_eq!(active.key_id, "key-b");
        assert_eq!(active.version, 2);
    }

    #[test]
    fn all_versions_lists_history() {
        let m = KeyEnvelopeManager::new();
        m.rotate(range(1), "k1");
        m.rotate(range(1), "k2");
        m.rotate(range(1), "k3");
        let versions = m.all_versions(range(1));
        assert_eq!(versions.len(), 3);
    }

    #[test]
    fn retire_removes_specific_version() {
        let m = KeyEnvelopeManager::new();
        m.rotate(range(1), "k1");
        m.rotate(range(1), "k2");
        m.retire(range(1), 1);
        let versions = m.all_versions(range(1));
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].version, 2);
    }

    #[test]
    fn empty_range_has_no_active_dek() {
        let m = KeyEnvelopeManager::new();
        assert!(m.active(range(99)).is_none());
    }
}
