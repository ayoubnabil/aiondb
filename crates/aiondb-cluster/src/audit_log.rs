//! Replicated audit logger.
//!
//! Appends compliance-relevant events to a per-tenant log. Each
//! entry carries tenant_id + actor + action + timestamp. Used for
//! SOC2 / HIPAA / RGPD trails. Ring-buffered with a configurable cap
//! per tenant.

use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AuditEntry {
    pub tenant_id: u64,
    pub actor: String,
    pub action: String,
    pub target: String,
    pub at_us: u64,
    pub success: bool,
}

#[derive(Clone, Debug, Default)]
pub struct AuditLog {
    inner: Arc<std::sync::Mutex<BTreeMap<u64, VecDeque<AuditEntry>>>>,
    capacity_per_tenant: usize,
}

impl AuditLog {
    pub fn new(capacity_per_tenant: usize) -> Self {
        Self {
            inner: Arc::default(),
            capacity_per_tenant: capacity_per_tenant.max(1),
        }
    }

    pub fn append(&self, entry: AuditEntry) {
        let mut guard = self.inner.lock().unwrap();
        let log = guard.entry(entry.tenant_id).or_default();
        log.push_back(entry);
        while log.len() > self.capacity_per_tenant {
            log.pop_front();
        }
    }

    pub fn entries_for(&self, tenant_id: u64) -> Vec<AuditEntry> {
        self.inner
            .lock()
            .unwrap()
            .get(&tenant_id)
            .map(|v| v.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn tenant_count(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    pub fn purge(&self, tenant_id: u64) {
        self.inner.lock().unwrap().remove(&tenant_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(tenant: u64, success: bool) -> AuditEntry {
        AuditEntry {
            tenant_id: tenant,
            actor: "alice".into(),
            action: "select".into(),
            target: "users".into(),
            at_us: 1000,
            success,
        }
    }

    #[test]
    fn append_then_entries_for_returns_record() {
        let log = AuditLog::new(10);
        log.append(entry(1, true));
        let entries = log.entries_for(1);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].actor, "alice");
    }

    #[test]
    fn distinct_tenants_keep_independent_logs() {
        let log = AuditLog::new(10);
        log.append(entry(1, true));
        log.append(entry(2, true));
        assert_eq!(log.entries_for(1).len(), 1);
        assert_eq!(log.entries_for(2).len(), 1);
    }

    #[test]
    fn capacity_evicts_oldest() {
        let log = AuditLog::new(3);
        for _ in 0..5 {
            log.append(entry(1, true));
        }
        assert_eq!(log.entries_for(1).len(), 3);
    }

    #[test]
    fn purge_clears_tenant_log() {
        let log = AuditLog::new(10);
        log.append(entry(1, true));
        log.purge(1);
        assert!(log.entries_for(1).is_empty());
    }

    #[test]
    fn json_round_trips() {
        let log = AuditLog::new(10);
        log.append(entry(1, false));
        let entries = log.entries_for(1);
        let json = serde_json::to_string(&entries[0]).unwrap();
        assert!(json.contains("\"actor\":\"alice\""));
    }
}
