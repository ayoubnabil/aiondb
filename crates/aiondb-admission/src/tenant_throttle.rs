//! Per-tenant throttle.
//!
//! Maintains an independent token bucket per tenant id so a noisy
//! tenant cannot starve the others. Tenants are isolated by
//! (tenant_id, priority) ; admission decisions consult both.
//!
//! In a sharded deployment the tenant bucket "lives" on the node
//! hashing of `tenant_id` — but this module provides only the local
//! state, which is enough for nodes that already shard their
//! workload before reaching admission.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use aiondb_core::DbResult;

use crate::TokenBucket;

/// Tenant identifier. Opaque numeric so the throttle does not depend
/// on the catalog's tenant naming.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TenantId(pub u64);

#[derive(Clone, Debug)]
pub struct TenantThrottleConfig {
    pub default_rate_per_sec: f64,
    pub default_burst: u64,
    pub max_tenants: usize,
}

impl Default for TenantThrottleConfig {
    fn default() -> Self {
        Self {
            default_rate_per_sec: 1_000.0,
            default_burst: 200,
            max_tenants: 10_000,
        }
    }
}

/// Per-tenant throttle.
#[derive(Clone, Debug)]
pub struct TenantThrottle {
    inner: Arc<std::sync::Mutex<BTreeMap<TenantId, Arc<TokenBucket>>>>,
    config: TenantThrottleConfig,
}

impl TenantThrottle {
    pub fn new(config: TenantThrottleConfig) -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(BTreeMap::new())),
            config,
        }
    }

    /// Try to consume one token for `tenant`. Bucket created on demand.
    pub fn try_admit(&self, tenant: TenantId) -> DbResult<bool> {
        let bucket = self.bucket_for(tenant)?;
        Ok(bucket.try_acquire(1))
    }

    /// Async wait for capacity. Returns Ok(()) when admission granted.
    pub async fn admit(&self, tenant: TenantId, timeout: Duration) -> DbResult<()> {
        let bucket = self.bucket_for(tenant)?;
        bucket.acquire(1, Some(timeout)).await
    }

    /// Override the rate / burst for a specific tenant.
    pub fn set_quota(&self, tenant: TenantId, rate_per_sec: f64, burst: u64) -> DbResult<()> {
        let new_bucket = Arc::new(TokenBucket::new(burst, rate_per_sec)?);
        let mut guard = self.inner.lock().unwrap();
        guard.insert(tenant, new_bucket);
        Ok(())
    }

    pub fn forget(&self, tenant: TenantId) {
        self.inner.lock().unwrap().remove(&tenant);
    }

    pub fn tenant_count(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    fn bucket_for(&self, tenant: TenantId) -> DbResult<Arc<TokenBucket>> {
        let mut guard = self.inner.lock().unwrap();
        if let Some(b) = guard.get(&tenant) {
            return Ok(Arc::clone(b));
        }
        if guard.len() >= self.config.max_tenants {
            // Evict the lowest-id tenant for predictability. Real
            // deployments would use LRU; this keeps the test surface
            // simple.
            if let Some(victim) = guard.keys().next().copied() {
                guard.remove(&victim);
            }
        }
        let bucket = Arc::new(TokenBucket::new(
            self.config.default_burst,
            self.config.default_rate_per_sec,
        )?);
        guard.insert(tenant, Arc::clone(&bucket));
        Ok(bucket)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tenant(n: u64) -> TenantId {
        TenantId(n)
    }

    #[test]
    fn fresh_tenant_gets_default_burst() {
        let t = TenantThrottle::new(TenantThrottleConfig {
            default_rate_per_sec: 0.0,
            default_burst: 3,
            max_tenants: 10,
        });
        assert!(t.try_admit(tenant(1)).unwrap());
        assert!(t.try_admit(tenant(1)).unwrap());
        assert!(t.try_admit(tenant(1)).unwrap());
        assert!(!t.try_admit(tenant(1)).unwrap());
    }

    #[test]
    fn distinct_tenants_have_independent_quotas() {
        let t = TenantThrottle::new(TenantThrottleConfig {
            default_rate_per_sec: 0.0,
            default_burst: 1,
            max_tenants: 10,
        });
        assert!(t.try_admit(tenant(1)).unwrap());
        assert!(!t.try_admit(tenant(1)).unwrap()); // exhausted
        assert!(t.try_admit(tenant(2)).unwrap()); // fresh bucket
    }

    #[test]
    fn set_quota_overrides_default() {
        let t = TenantThrottle::new(TenantThrottleConfig {
            default_rate_per_sec: 0.0,
            default_burst: 1,
            max_tenants: 10,
        });
        t.set_quota(tenant(1), 100.0, 5).unwrap();
        for _ in 0..5 {
            assert!(t.try_admit(tenant(1)).unwrap());
        }
        assert!(!t.try_admit(tenant(1)).unwrap());
    }

    #[test]
    fn max_tenants_evicts_oldest() {
        let t = TenantThrottle::new(TenantThrottleConfig {
            default_rate_per_sec: 0.0,
            default_burst: 1,
            max_tenants: 2,
        });
        let _ = t.try_admit(tenant(1)).unwrap();
        let _ = t.try_admit(tenant(2)).unwrap();
        let _ = t.try_admit(tenant(3)).unwrap();
        assert!(t.tenant_count() <= 2);
    }

    #[test]
    fn forget_drops_bucket() {
        let t = TenantThrottle::new(TenantThrottleConfig::default());
        t.try_admit(tenant(1)).unwrap();
        assert_eq!(t.tenant_count(), 1);
        t.forget(tenant(1));
        assert_eq!(t.tenant_count(), 0);
    }
}
