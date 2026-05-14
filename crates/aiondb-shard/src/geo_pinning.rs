//! Geo write-pinning policy.
//!
//! Per-table policy attaching writes to a specific home region for
//! compliance (data residency) or latency optimisation. Reads remain
//! free to follow the standard zone-router rules.

use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WritePinningOutcome {
    Allowed,
    Rejected { reason_code: u32 },
}

#[derive(Clone, Debug, Default)]
pub struct GeoPinningPolicy {
    inner: Arc<std::sync::Mutex<BTreeMap<u64, String>>>, // table_id -> required_region
}

impl GeoPinningPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn pin(&self, table_id: u64, region: impl Into<String>) {
        self.inner.lock().unwrap().insert(table_id, region.into());
    }

    pub fn unpin(&self, table_id: u64) {
        self.inner.lock().unwrap().remove(&table_id);
    }

    pub fn required_region(&self, table_id: u64) -> Option<String> {
        self.inner.lock().unwrap().get(&table_id).cloned()
    }

    pub fn check_write(&self, table_id: u64, write_region: &str) -> WritePinningOutcome {
        let required = match self.required_region(table_id) {
            Some(r) => r,
            None => return WritePinningOutcome::Allowed,
        };
        if required == write_region {
            WritePinningOutcome::Allowed
        } else {
            WritePinningOutcome::Rejected { reason_code: 4001 }
        }
    }

    pub fn snapshot(&self) -> Vec<(u64, String)> {
        let guard = self.inner.lock().unwrap();
        let mut out: Vec<_> = guard.iter().map(|(k, v)| (*k, v.clone())).collect();
        out.sort_by_key(|(k, _)| *k);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpinned_table_allows_any_region() {
        let p = GeoPinningPolicy::new();
        assert_eq!(p.check_write(1, "us"), WritePinningOutcome::Allowed);
    }

    #[test]
    fn pinned_table_rejects_other_regions() {
        let p = GeoPinningPolicy::new();
        p.pin(1, "eu");
        assert_eq!(
            p.check_write(1, "us"),
            WritePinningOutcome::Rejected { reason_code: 4001 }
        );
        assert_eq!(p.check_write(1, "eu"), WritePinningOutcome::Allowed);
    }

    #[test]
    fn unpin_clears_constraint() {
        let p = GeoPinningPolicy::new();
        p.pin(1, "eu");
        p.unpin(1);
        assert_eq!(p.check_write(1, "us"), WritePinningOutcome::Allowed);
    }

    #[test]
    fn snapshot_sorted_by_table_id() {
        let p = GeoPinningPolicy::new();
        p.pin(3, "ap");
        p.pin(1, "eu");
        p.pin(2, "us");
        let snap = p.snapshot();
        assert_eq!(snap[0].0, 1);
        assert_eq!(snap[2].0, 3);
    }

    #[test]
    fn required_region_returns_some_when_pinned() {
        let p = GeoPinningPolicy::new();
        p.pin(7, "ap");
        assert_eq!(p.required_region(7), Some("ap".into()));
        assert_eq!(p.required_region(99), None);
    }
}
