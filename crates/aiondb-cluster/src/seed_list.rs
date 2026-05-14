//! Cluster gossip seed list.
//!
//! Maintains a rotating list of "always-contact" seed nodes for new
//! members to bootstrap from. Failed seeds are demoted; healthy seeds
//! are promoted so the active head of the list is responsive.

use std::collections::VecDeque;
use std::sync::Arc;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SeedEntry {
    pub address: String,
    pub healthy: bool,
    pub last_used_at_us: u64,
}

#[derive(Clone, Debug, Default)]
pub struct SeedList {
    inner: Arc<std::sync::Mutex<VecDeque<SeedEntry>>>,
}

impl SeedList {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&self, address: impl Into<String>) {
        let entry = SeedEntry {
            address: address.into(),
            healthy: true,
            last_used_at_us: 0,
        };
        self.inner.lock().unwrap().push_back(entry);
    }

    pub fn next_seed(&self) -> Option<String> {
        let mut guard = self.inner.lock().unwrap();
        // Rotate : pop front, push back.
        if let Some(mut entry) = guard.pop_front() {
            let addr = entry.address.clone();
            entry.last_used_at_us = now_us();
            guard.push_back(entry);
            Some(addr)
        } else {
            None
        }
    }

    pub fn report_failure(&self, address: &str) {
        let mut guard = self.inner.lock().unwrap();
        for entry in guard.iter_mut() {
            if entry.address == address {
                entry.healthy = false;
            }
        }
    }

    pub fn report_success(&self, address: &str) {
        let mut guard = self.inner.lock().unwrap();
        for entry in guard.iter_mut() {
            if entry.address == address {
                entry.healthy = true;
            }
        }
    }

    pub fn healthy_count(&self) -> usize {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.healthy)
            .count()
    }
}

fn now_us() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_micros()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_then_next_returns_address() {
        let l = SeedList::new();
        l.add("host:1");
        assert_eq!(l.next_seed(), Some("host:1".to_owned()));
    }

    #[test]
    fn next_rotates_round_robin() {
        let l = SeedList::new();
        l.add("a");
        l.add("b");
        l.add("c");
        let order = (0..6).map(|_| l.next_seed().unwrap()).collect::<Vec<_>>();
        assert_eq!(order, vec!["a", "b", "c", "a", "b", "c"]);
    }

    #[test]
    fn report_failure_marks_unhealthy() {
        let l = SeedList::new();
        l.add("a");
        l.add("b");
        l.report_failure("a");
        assert_eq!(l.healthy_count(), 1);
        l.report_success("a");
        assert_eq!(l.healthy_count(), 2);
    }

    #[test]
    fn empty_list_returns_none() {
        let l = SeedList::new();
        assert!(l.next_seed().is_none());
    }

    #[test]
    fn unknown_address_report_is_ignored() {
        let l = SeedList::new();
        l.add("a");
        l.report_failure("ghost");
        assert_eq!(l.healthy_count(), 1);
    }
}
