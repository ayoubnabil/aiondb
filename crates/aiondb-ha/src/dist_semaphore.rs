//! Cluster-wide semaphore.
//!
//! Holds at most N permits across the cluster. Permits are
//! represented by an opaque ticket; the holder must call `release`
//! to return the permit. If a holder crashes, the manager reaps
//! permits with `ttl_expired_at < now`. Acquire is non-blocking ;
//! callers retry with their own back-off.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemaphorePermit {
    pub ticket: u64,
    pub holder: String,
    pub expires_at: Instant,
}

#[derive(Clone, Debug)]
pub struct DistSemaphore {
    inner: Arc<std::sync::Mutex<SemState>>,
    capacity: u32,
}

#[derive(Debug)]
struct SemState {
    permits: BTreeMap<u64, SemaphorePermit>,
    waiters: VecDeque<String>,
    next_ticket: u64,
}

impl DistSemaphore {
    pub fn new(capacity: u32) -> Self {
        Self {
            capacity,
            inner: Arc::new(std::sync::Mutex::new(SemState {
                permits: BTreeMap::new(),
                waiters: VecDeque::new(),
                next_ticket: 1,
            })),
        }
    }

    pub fn try_acquire(&self, holder: &str, ttl: Duration) -> Option<SemaphorePermit> {
        let mut g = self.inner.lock().unwrap();
        self.reap_expired_locked(&mut g);
        if g.permits.len() >= self.capacity as usize {
            if !g.waiters.iter().any(|w| w == holder) {
                g.waiters.push_back(holder.to_string());
            }
            return None;
        }
        let ticket = g.next_ticket;
        g.next_ticket += 1;
        let p = SemaphorePermit {
            ticket,
            holder: holder.to_string(),
            expires_at: Instant::now() + ttl,
        };
        g.permits.insert(ticket, p.clone());
        g.waiters.retain(|w| w != holder);
        Some(p)
    }

    pub fn release(&self, ticket: u64) -> bool {
        let mut g = self.inner.lock().unwrap();
        g.permits.remove(&ticket).is_some()
    }

    pub fn renew(&self, ticket: u64, ttl: Duration) -> bool {
        let mut g = self.inner.lock().unwrap();
        let Some(p) = g.permits.get_mut(&ticket) else {
            return false;
        };
        p.expires_at = Instant::now() + ttl;
        true
    }

    pub fn outstanding(&self) -> Vec<SemaphorePermit> {
        self.inner
            .lock()
            .unwrap()
            .permits
            .values()
            .cloned()
            .collect()
    }

    pub fn waiters(&self) -> Vec<String> {
        self.inner.lock().unwrap().waiters.iter().cloned().collect()
    }

    pub fn reap_expired(&self) -> Vec<SemaphorePermit> {
        let mut g = self.inner.lock().unwrap();
        self.reap_expired_locked(&mut g)
    }

    fn reap_expired_locked(&self, g: &mut SemState) -> Vec<SemaphorePermit> {
        let now = Instant::now();
        let expired: Vec<SemaphorePermit> = g
            .permits
            .values()
            .filter(|p| p.expires_at <= now)
            .cloned()
            .collect();
        for p in &expired {
            g.permits.remove(&p.ticket);
        }
        expired
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquires_up_to_capacity() {
        let s = DistSemaphore::new(2);
        let p1 = s.try_acquire("a", Duration::from_secs(5));
        let p2 = s.try_acquire("b", Duration::from_secs(5));
        let p3 = s.try_acquire("c", Duration::from_secs(5));
        assert!(p1.is_some());
        assert!(p2.is_some());
        assert!(p3.is_none());
    }

    #[test]
    fn release_frees_permit() {
        let s = DistSemaphore::new(1);
        let p = s.try_acquire("a", Duration::from_secs(5)).unwrap();
        s.release(p.ticket);
        let p2 = s.try_acquire("b", Duration::from_secs(5));
        assert!(p2.is_some());
    }

    #[test]
    fn waiters_track_deferred_holders() {
        let s = DistSemaphore::new(1);
        let _p = s.try_acquire("a", Duration::from_secs(5)).unwrap();
        assert!(s.try_acquire("b", Duration::from_secs(5)).is_none());
        assert_eq!(s.waiters(), vec!["b".to_string()]);
    }

    #[test]
    fn expired_permits_are_reaped_on_acquire() {
        let s = DistSemaphore::new(1);
        let _p = s.try_acquire("a", Duration::from_millis(5)).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        let p2 = s.try_acquire("b", Duration::from_secs(5));
        assert!(p2.is_some());
    }

    #[test]
    fn renew_extends_ttl() {
        let s = DistSemaphore::new(1);
        let p = s.try_acquire("a", Duration::from_millis(5)).unwrap();
        s.renew(p.ticket, Duration::from_secs(60));
        std::thread::sleep(Duration::from_millis(10));
        // permit still alive.
        assert_eq!(s.outstanding().len(), 1);
    }

    #[test]
    fn renew_unknown_ticket_fails() {
        let s = DistSemaphore::new(1);
        assert!(!s.renew(999, Duration::from_secs(60)));
    }

    #[test]
    fn release_unknown_ticket_fails() {
        let s = DistSemaphore::new(1);
        assert!(!s.release(999));
    }
}
