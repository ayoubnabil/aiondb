//! Per-tenant quota ledger.
//!
//! Tracks consumption (bytes written, request count, CPU ms) per
//! tenant in time buckets so the cluster can :
//!
//! - Apply rate-based admission throttles.
//! - Emit billing rollups on a periodic schedule.
//! - Detect tenants that exceed their configured monthly quota.
//!
//! Buckets are aligned on `bucket_duration`. Old buckets older than
//! `retention` are rolled into a single tail bucket so the ledger
//! stays O(retention / bucket).

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct UsageDelta {
    pub bytes_written: u64,
    pub requests: u64,
    pub cpu_micros: u64,
}

impl UsageDelta {
    pub fn merge(&mut self, other: UsageDelta) {
        self.bytes_written = self.bytes_written.saturating_add(other.bytes_written);
        self.requests = self.requests.saturating_add(other.requests);
        self.cpu_micros = self.cpu_micros.saturating_add(other.cpu_micros);
    }
}

#[derive(Clone, Debug)]
pub struct UsageBucket {
    pub start: SystemTime,
    pub delta: UsageDelta,
}

#[derive(Clone, Debug)]
pub struct LedgerTenantQuota {
    pub max_bytes_per_month: u64,
    pub max_requests_per_second: u64,
}

#[derive(Clone, Debug, Default)]
pub struct QuotaLedger {
    inner: Arc<std::sync::Mutex<LedgerState>>,
}

#[derive(Default, Debug)]
struct LedgerState {
    by_tenant: BTreeMap<String, VecDeque<UsageBucket>>,
    quotas: BTreeMap<String, LedgerTenantQuota>,
    bucket_duration: Duration,
    retention: Duration,
}

impl QuotaLedger {
    pub fn new(bucket_duration: Duration, retention: Duration) -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(LedgerState {
                by_tenant: BTreeMap::new(),
                quotas: BTreeMap::new(),
                bucket_duration: bucket_duration.max(Duration::from_secs(1)),
                retention,
            })),
        }
    }

    pub fn set_quota(&self, tenant: impl Into<String>, quota: LedgerTenantQuota) {
        self.inner
            .lock()
            .unwrap()
            .quotas
            .insert(tenant.into(), quota);
    }

    pub fn record(&self, tenant: &str, now: SystemTime, delta: UsageDelta) {
        let mut g = self.inner.lock().unwrap();
        let bd = g.bucket_duration;
        let bucket_start = align(now, bd);
        let series = g.by_tenant.entry(tenant.to_string()).or_default();
        match series.back_mut() {
            Some(b) if b.start == bucket_start => b.delta.merge(delta),
            _ => series.push_back(UsageBucket {
                start: bucket_start,
                delta,
            }),
        }
        let retention = g.retention;
        let series = g.by_tenant.get_mut(tenant).unwrap();
        let cutoff = now.checked_sub(retention).unwrap_or(SystemTime::UNIX_EPOCH);
        while let Some(front) = series.front() {
            if front.start < cutoff {
                series.pop_front();
            } else {
                break;
            }
        }
    }

    pub fn usage(&self, tenant: &str) -> UsageDelta {
        let g = self.inner.lock().unwrap();
        let Some(series) = g.by_tenant.get(tenant) else {
            return UsageDelta::default();
        };
        let mut total = UsageDelta::default();
        for b in series {
            total.merge(b.delta);
        }
        total
    }

    pub fn exceeded_quota(&self, tenant: &str) -> bool {
        let g = self.inner.lock().unwrap();
        let Some(q) = g.quotas.get(tenant) else {
            return false;
        };
        let series = match g.by_tenant.get(tenant) {
            Some(s) => s,
            None => return false,
        };
        let mut bytes = 0u64;
        for b in series {
            bytes = bytes.saturating_add(b.delta.bytes_written);
        }
        bytes > q.max_bytes_per_month
    }

    pub fn rollup(&self) -> BTreeMap<String, UsageDelta> {
        let g = self.inner.lock().unwrap();
        g.by_tenant
            .iter()
            .map(|(k, v)| {
                let mut total = UsageDelta::default();
                for b in v {
                    total.merge(b.delta);
                }
                (k.clone(), total)
            })
            .collect()
    }
}

fn align(t: SystemTime, bucket: Duration) -> SystemTime {
    let since = t
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let bucket_ns = bucket.as_nanos().max(1);
    let aligned = since - (since % bucket_ns);
    SystemTime::UNIX_EPOCH + Duration::from_nanos(aligned as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_aggregate() {
        let l = QuotaLedger::new(Duration::from_secs(60), Duration::from_secs(3600));
        l.record(
            "alice",
            SystemTime::UNIX_EPOCH + Duration::from_secs(10),
            UsageDelta {
                bytes_written: 100,
                requests: 5,
                cpu_micros: 50,
            },
        );
        l.record(
            "alice",
            SystemTime::UNIX_EPOCH + Duration::from_secs(70),
            UsageDelta {
                bytes_written: 50,
                requests: 2,
                cpu_micros: 30,
            },
        );
        let u = l.usage("alice");
        assert_eq!(u.bytes_written, 150);
        assert_eq!(u.requests, 7);
    }

    #[test]
    fn quota_breach_detected() {
        let l = QuotaLedger::new(Duration::from_secs(60), Duration::from_secs(3600));
        l.set_quota(
            "alice",
            LedgerTenantQuota {
                max_bytes_per_month: 100,
                max_requests_per_second: 100,
            },
        );
        l.record(
            "alice",
            SystemTime::UNIX_EPOCH + Duration::from_secs(10),
            UsageDelta {
                bytes_written: 200,
                ..Default::default()
            },
        );
        assert!(l.exceeded_quota("alice"));
    }

    #[test]
    fn unknown_tenant_within_quota() {
        let l = QuotaLedger::new(Duration::from_secs(60), Duration::from_secs(3600));
        assert!(!l.exceeded_quota("ghost"));
    }

    #[test]
    fn same_bucket_merges() {
        let l = QuotaLedger::new(Duration::from_secs(60), Duration::from_secs(3600));
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        l.record(
            "x",
            t,
            UsageDelta {
                bytes_written: 10,
                ..Default::default()
            },
        );
        l.record(
            "x",
            t + Duration::from_secs(10),
            UsageDelta {
                bytes_written: 20,
                ..Default::default()
            },
        );
        assert_eq!(l.usage("x").bytes_written, 30);
    }

    #[test]
    fn rollup_includes_all_tenants() {
        let l = QuotaLedger::new(Duration::from_secs(60), Duration::from_secs(3600));
        l.record(
            "a",
            SystemTime::UNIX_EPOCH + Duration::from_secs(1),
            UsageDelta {
                bytes_written: 1,
                ..Default::default()
            },
        );
        l.record(
            "b",
            SystemTime::UNIX_EPOCH + Duration::from_secs(1),
            UsageDelta {
                bytes_written: 2,
                ..Default::default()
            },
        );
        let r = l.rollup();
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn retention_drops_old_buckets() {
        let l = QuotaLedger::new(Duration::from_secs(60), Duration::from_secs(120));
        l.record(
            "a",
            SystemTime::UNIX_EPOCH + Duration::from_secs(10),
            UsageDelta {
                bytes_written: 5,
                ..Default::default()
            },
        );
        l.record(
            "a",
            SystemTime::UNIX_EPOCH + Duration::from_secs(3600),
            UsageDelta {
                bytes_written: 3,
                ..Default::default()
            },
        );
        assert_eq!(l.usage("a").bytes_written, 3);
    }
}
