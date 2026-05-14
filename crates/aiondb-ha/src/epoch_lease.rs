//! Epoch-based lease manager.
//!
//! A resource (e.g. a Raft group ID, a job name) has at most one
//! current lease holder. The lease carries a monotonic epoch. When
//! a new holder acquires, the epoch increments — any operation
//! tagged with the previous epoch is fenced (rejected) at apply
//! time. This is the same primitive Spanner uses for leadership.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EpochLease {
    pub resource: String,
    pub holder: String,
    pub epoch: u64,
    pub expires_at: Instant,
}

#[derive(Clone, Debug, Default)]
pub struct EpochLeaseManager {
    inner: Arc<std::sync::Mutex<BTreeMap<String, EpochLease>>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcquireOutcome {
    Granted(u64),
    Denied,
    Renewed(u64),
}

impl EpochLeaseManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn try_acquire(&self, resource: &str, holder: &str, ttl: Duration) -> AcquireOutcome {
        let mut g = self.inner.lock().unwrap();
        let now = Instant::now();
        match g.get_mut(resource) {
            Some(existing) if existing.expires_at > now && existing.holder != holder => {
                AcquireOutcome::Denied
            }
            Some(existing) if existing.holder == holder => {
                existing.expires_at = now + ttl;
                AcquireOutcome::Renewed(existing.epoch)
            }
            _ => {
                let epoch = g
                    .get(resource)
                    .map(|l| l.epoch.saturating_add(1))
                    .unwrap_or(1);
                g.insert(
                    resource.to_string(),
                    EpochLease {
                        resource: resource.to_string(),
                        holder: holder.to_string(),
                        epoch,
                        expires_at: now + ttl,
                    },
                );
                AcquireOutcome::Granted(epoch)
            }
        }
    }

    pub fn current(&self, resource: &str) -> Option<EpochLease> {
        self.inner.lock().unwrap().get(resource).cloned()
    }

    /// True iff `(holder, epoch)` is the live lease for `resource`.
    pub fn is_current(&self, resource: &str, holder: &str, epoch: u64) -> bool {
        let g = self.inner.lock().unwrap();
        let now = Instant::now();
        g.get(resource)
            .map(|l| l.holder == holder && l.epoch == epoch && l.expires_at > now)
            .unwrap_or(false)
    }

    pub fn release(&self, resource: &str, holder: &str) -> bool {
        let mut g = self.inner.lock().unwrap();
        if let Some(l) = g.get(resource) {
            if l.holder == holder {
                g.remove(resource);
                return true;
            }
        }
        false
    }

    pub fn expired(&self) -> Vec<EpochLease> {
        let now = Instant::now();
        self.inner
            .lock()
            .unwrap()
            .values()
            .filter(|l| l.expires_at <= now)
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_acquire_granted() {
        let m = EpochLeaseManager::new();
        let r = m.try_acquire("range-1", "node-a", Duration::from_secs(5));
        assert_eq!(r, AcquireOutcome::Granted(1));
    }

    #[test]
    fn second_holder_denied_while_active() {
        let m = EpochLeaseManager::new();
        m.try_acquire("r", "a", Duration::from_secs(5));
        let r = m.try_acquire("r", "b", Duration::from_secs(5));
        assert_eq!(r, AcquireOutcome::Denied);
    }

    #[test]
    fn same_holder_renews() {
        let m = EpochLeaseManager::new();
        let g1 = m.try_acquire("r", "a", Duration::from_secs(5));
        let g2 = m.try_acquire("r", "a", Duration::from_secs(5));
        assert!(matches!(g1, AcquireOutcome::Granted(_)));
        assert!(matches!(g2, AcquireOutcome::Renewed(_)));
    }

    #[test]
    fn expired_lease_reassigned_with_new_epoch() {
        let m = EpochLeaseManager::new();
        m.try_acquire("r", "a", Duration::from_millis(5));
        std::thread::sleep(Duration::from_millis(20));
        let g = m.try_acquire("r", "b", Duration::from_secs(5));
        assert_eq!(g, AcquireOutcome::Granted(2));
    }

    #[test]
    fn is_current_rejects_old_epoch() {
        let m = EpochLeaseManager::new();
        m.try_acquire("r", "a", Duration::from_millis(5));
        std::thread::sleep(Duration::from_millis(20));
        m.try_acquire("r", "b", Duration::from_secs(5));
        assert!(!m.is_current("r", "a", 1));
        assert!(m.is_current("r", "b", 2));
    }

    #[test]
    fn release_drops_lease() {
        let m = EpochLeaseManager::new();
        m.try_acquire("r", "a", Duration::from_secs(5));
        assert!(m.release("r", "a"));
        assert!(m.current("r").is_none());
    }

    #[test]
    fn release_by_other_holder_rejected() {
        let m = EpochLeaseManager::new();
        m.try_acquire("r", "a", Duration::from_secs(5));
        assert!(!m.release("r", "b"));
        assert!(m.current("r").is_some());
    }
}
