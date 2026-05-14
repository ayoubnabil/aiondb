//! Read-only lease.
//!
//! Granted to the leaseholder for `lease_duration`. While valid, the
//! holder can serve linearizable reads without sending ReadIndex
//! over the network. Before answering a read, the holder verifies
//! `now <= expires_at - clock_skew` so a clock jump can't make a
//! stale read look valid.

use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct ReadLease {
    pub holder: String,
    pub expires_at: Instant,
    pub max_clock_skew: Duration,
}

#[derive(Clone, Debug, Default)]
pub struct ReadLeaseManager {
    inner: Arc<std::sync::Mutex<Option<ReadLease>>>,
}

impl ReadLeaseManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn install(&self, holder: String, duration: Duration, max_clock_skew: Duration) {
        let l = ReadLease {
            holder,
            expires_at: Instant::now() + duration,
            max_clock_skew,
        };
        *self.inner.lock().unwrap() = Some(l);
    }

    pub fn current(&self) -> Option<ReadLease> {
        self.inner.lock().unwrap().clone()
    }

    pub fn can_serve_locally(&self, holder: &str) -> bool {
        let g = self.inner.lock().unwrap();
        let Some(l) = g.as_ref() else {
            return false;
        };
        if l.holder != holder {
            return false;
        }
        let now = Instant::now();
        l.expires_at > now + l.max_clock_skew
    }

    pub fn extend(&self, additional: Duration) -> bool {
        let mut g = self.inner.lock().unwrap();
        let Some(l) = g.as_mut() else {
            return false;
        };
        l.expires_at += additional;
        true
    }

    pub fn invalidate(&self) {
        *self.inner.lock().unwrap() = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_and_serve() {
        let m = ReadLeaseManager::new();
        m.install(
            "n1".into(),
            Duration::from_secs(60),
            Duration::from_millis(50),
        );
        assert!(m.can_serve_locally("n1"));
    }

    #[test]
    fn other_holder_cannot_serve() {
        let m = ReadLeaseManager::new();
        m.install(
            "n1".into(),
            Duration::from_secs(60),
            Duration::from_millis(50),
        );
        assert!(!m.can_serve_locally("n2"));
    }

    #[test]
    fn expired_lease_cannot_serve() {
        let m = ReadLeaseManager::new();
        m.install(
            "n1".into(),
            Duration::from_millis(1),
            Duration::from_millis(50),
        );
        std::thread::sleep(Duration::from_millis(20));
        assert!(!m.can_serve_locally("n1"));
    }

    #[test]
    fn near_expiry_inside_skew_window_cannot_serve() {
        let m = ReadLeaseManager::new();
        m.install(
            "n1".into(),
            Duration::from_millis(20),
            Duration::from_millis(50),
        );
        // We're inside the skew protection window from the start.
        assert!(!m.can_serve_locally("n1"));
    }

    #[test]
    fn extend_pushes_expiry_out() {
        let m = ReadLeaseManager::new();
        m.install(
            "n1".into(),
            Duration::from_millis(10),
            Duration::from_millis(1),
        );
        m.extend(Duration::from_secs(60));
        assert!(m.can_serve_locally("n1"));
    }

    #[test]
    fn invalidate_clears() {
        let m = ReadLeaseManager::new();
        m.install(
            "n1".into(),
            Duration::from_secs(60),
            Duration::from_millis(0),
        );
        m.invalidate();
        assert!(m.current().is_none());
    }

    #[test]
    fn no_lease_means_no_local_read() {
        let m = ReadLeaseManager::new();
        assert!(!m.can_serve_locally("n1"));
    }
}
