//! Idle connection reaper.
//!
//! Walks the pool periodically and closes connections that have
//! been idle for longer than `idle_timeout`. Active connections
//! refresh their "last activity" via [`IdleConnReaper::touch`].
//! Connections within `grace` of expiry get a one-time warning so
//! the caller can renew them.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReapDecision {
    Keep,
    GracePeriod,
    Close,
}

#[derive(Clone, Debug, Default)]
pub struct IdleConnReaper {
    inner: Arc<std::sync::Mutex<BTreeMap<u64, Instant>>>,
}

impl IdleConnReaper {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn touch(&self, conn_id: u64) {
        self.inner.lock().unwrap().insert(conn_id, Instant::now());
    }

    pub fn forget(&self, conn_id: u64) {
        self.inner.lock().unwrap().remove(&conn_id);
    }

    pub fn decide(&self, conn_id: u64, idle_timeout: Duration, grace: Duration) -> ReapDecision {
        let g = self.inner.lock().unwrap();
        let Some(last) = g.get(&conn_id).copied() else {
            return ReapDecision::Close;
        };
        let idle = Instant::now().saturating_duration_since(last);
        if idle > idle_timeout {
            ReapDecision::Close
        } else if idle + grace > idle_timeout {
            ReapDecision::GracePeriod
        } else {
            ReapDecision::Keep
        }
    }

    pub fn reap(&self, idle_timeout: Duration) -> Vec<u64> {
        let now = Instant::now();
        let mut g = self.inner.lock().unwrap();
        let to_close: Vec<u64> = g
            .iter()
            .filter(|(_, t)| now.saturating_duration_since(**t) > idle_timeout)
            .map(|(k, _)| *k)
            .collect();
        for id in &to_close {
            g.remove(id);
        }
        to_close
    }

    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touched_connection_is_kept() {
        let r = IdleConnReaper::new();
        r.touch(1);
        assert_eq!(
            r.decide(1, Duration::from_secs(60), Duration::from_secs(5)),
            ReapDecision::Keep
        );
    }

    #[test]
    fn idle_connection_closed() {
        let r = IdleConnReaper::new();
        r.touch(1);
        std::thread::sleep(Duration::from_millis(20));
        let d = r.decide(1, Duration::from_millis(10), Duration::from_millis(1));
        assert_eq!(d, ReapDecision::Close);
    }

    #[test]
    fn grace_period_signals_renewal() {
        let r = IdleConnReaper::new();
        r.touch(1);
        std::thread::sleep(Duration::from_millis(50));
        let d = r.decide(1, Duration::from_secs(1), Duration::from_secs(60));
        assert_eq!(d, ReapDecision::GracePeriod);
    }

    #[test]
    fn unknown_connection_is_closed() {
        let r = IdleConnReaper::new();
        let d = r.decide(99, Duration::from_secs(60), Duration::from_secs(5));
        assert_eq!(d, ReapDecision::Close);
    }

    #[test]
    fn reap_drops_idle_entries() {
        let r = IdleConnReaper::new();
        r.touch(1);
        r.touch(2);
        std::thread::sleep(Duration::from_millis(20));
        let reaped = r.reap(Duration::from_millis(5));
        assert_eq!(reaped.len(), 2);
        assert!(r.is_empty());
    }

    #[test]
    fn forget_removes_entry() {
        let r = IdleConnReaper::new();
        r.touch(1);
        r.forget(1);
        assert_eq!(
            r.decide(1, Duration::from_secs(60), Duration::from_secs(5)),
            ReapDecision::Close
        );
    }
}
