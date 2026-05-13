//! Multi-tenant resource quota enforcer.
//!
//! Each tenant has independent budgets for :
//!
//! - CPU shares.
//! - Concurrent in-flight requests.
//! - Bytes scanned per minute.
//!
//! Violations are reported but not automatically enforced; the
//! orchestrator decides whether to throttle, abort, or page.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::TenantId;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TenantQuota {
    pub max_concurrent: u64,
    pub max_bytes_scanned_per_minute: u64,
    pub cpu_share: u32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct TenantUsage {
    pub in_flight: u64,
    pub bytes_scanned_window: u64,
    pub last_reset_ts: u64,
}

#[derive(Clone, Debug)]
pub enum QuotaVerdict {
    Within,
    Over { detail: String },
}

#[derive(Clone, Debug, Default)]
pub struct TenantIsolation {
    quotas: Arc<std::sync::Mutex<BTreeMap<TenantId, TenantQuota>>>,
    usage: Arc<std::sync::Mutex<BTreeMap<TenantId, TenantUsage>>>,
}

impl TenantIsolation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_quota(&self, tenant: TenantId, quota: TenantQuota) {
        self.quotas.lock().unwrap().insert(tenant, quota);
    }

    pub fn admit_request(&self, tenant: TenantId, bytes: u64) -> QuotaVerdict {
        let quota = self
            .quotas
            .lock()
            .unwrap()
            .get(&tenant)
            .copied()
            .unwrap_or_default();
        let mut usage_guard = self.usage.lock().unwrap();
        let usage = usage_guard.entry(tenant).or_default();
        if quota.max_concurrent > 0 && usage.in_flight + 1 > quota.max_concurrent {
            return QuotaVerdict::Over {
                detail: format!(
                    "tenant {:?} exceeds in-flight cap {}",
                    tenant, quota.max_concurrent
                ),
            };
        }
        if quota.max_bytes_scanned_per_minute > 0
            && usage.bytes_scanned_window + bytes > quota.max_bytes_scanned_per_minute
        {
            return QuotaVerdict::Over {
                detail: format!(
                    "tenant {:?} exceeds scan-bytes cap {}",
                    tenant, quota.max_bytes_scanned_per_minute
                ),
            };
        }
        usage.in_flight = usage.in_flight.saturating_add(1);
        usage.bytes_scanned_window = usage.bytes_scanned_window.saturating_add(bytes);
        QuotaVerdict::Within
    }

    pub fn release_request(&self, tenant: TenantId) {
        let mut guard = self.usage.lock().unwrap();
        if let Some(u) = guard.get_mut(&tenant) {
            u.in_flight = u.in_flight.saturating_sub(1);
        }
    }

    pub fn reset_window(&self, tenant: TenantId) {
        if let Some(u) = self.usage.lock().unwrap().get_mut(&tenant) {
            u.bytes_scanned_window = 0;
        }
    }

    pub fn usage_of(&self, tenant: TenantId) -> TenantUsage {
        self.usage
            .lock()
            .unwrap()
            .get(&tenant)
            .copied()
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(n: u64) -> TenantId {
        TenantId(n)
    }

    #[test]
    fn unrestricted_tenant_always_within() {
        let iso = TenantIsolation::new();
        match iso.admit_request(t(1), 1_000_000) {
            QuotaVerdict::Within => {}
            other => panic!("expected Within, got {other:?}"),
        }
    }

    #[test]
    fn concurrent_cap_enforced() {
        let iso = TenantIsolation::new();
        iso.set_quota(
            t(1),
            TenantQuota {
                max_concurrent: 2,
                ..Default::default()
            },
        );
        assert!(matches!(iso.admit_request(t(1), 0), QuotaVerdict::Within));
        assert!(matches!(iso.admit_request(t(1), 0), QuotaVerdict::Within));
        assert!(matches!(
            iso.admit_request(t(1), 0),
            QuotaVerdict::Over { .. }
        ));
    }

    #[test]
    fn release_request_lowers_inflight() {
        let iso = TenantIsolation::new();
        iso.set_quota(
            t(1),
            TenantQuota {
                max_concurrent: 1,
                ..Default::default()
            },
        );
        iso.admit_request(t(1), 0);
        iso.release_request(t(1));
        assert_eq!(iso.usage_of(t(1)).in_flight, 0);
        assert!(matches!(iso.admit_request(t(1), 0), QuotaVerdict::Within));
    }

    #[test]
    fn bytes_cap_enforced() {
        let iso = TenantIsolation::new();
        iso.set_quota(
            t(1),
            TenantQuota {
                max_bytes_scanned_per_minute: 1_000,
                ..Default::default()
            },
        );
        assert!(matches!(iso.admit_request(t(1), 600), QuotaVerdict::Within));
        assert!(matches!(
            iso.admit_request(t(1), 600),
            QuotaVerdict::Over { .. }
        ));
    }

    #[test]
    fn reset_window_re_admits_bytes() {
        let iso = TenantIsolation::new();
        iso.set_quota(
            t(1),
            TenantQuota {
                max_bytes_scanned_per_minute: 1_000,
                ..Default::default()
            },
        );
        iso.admit_request(t(1), 900);
        iso.reset_window(t(1));
        assert!(matches!(iso.admit_request(t(1), 500), QuotaVerdict::Within));
    }
}
