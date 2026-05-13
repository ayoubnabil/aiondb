//! Follower lag alarm.
//!
//! Fires when a follower has been below the leader's high water
//! mark by more than `threshold_lsn` for at least `duration`. The
//! alarm clears as soon as the lag drops back inside the threshold.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct FollowerLagAlarm {
    inner: Arc<std::sync::Mutex<AlarmState>>,
    threshold_lsn: u64,
    duration: Duration,
}

#[derive(Default, Debug)]
struct AlarmState {
    last_below: BTreeMap<u64, Instant>,
    firing: std::collections::BTreeSet<u64>,
    leader_lsn: u64,
}

impl FollowerLagAlarm {
    pub fn new(threshold_lsn: u64, duration: Duration) -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(AlarmState::default())),
            threshold_lsn,
            duration,
        }
    }

    pub fn update_leader_lsn(&self, lsn: u64) {
        self.inner.lock().unwrap().leader_lsn = lsn;
    }

    pub fn observe_follower(&self, node: u64, lsn: u64) {
        let mut g = self.inner.lock().unwrap();
        let lag = g.leader_lsn.saturating_sub(lsn);
        let now = Instant::now();
        if lag > self.threshold_lsn {
            let last = g.last_below.entry(node).or_insert(now);
            if now.saturating_duration_since(*last) >= self.duration {
                g.firing.insert(node);
            }
        } else {
            g.last_below.remove(&node);
            g.firing.remove(&node);
        }
    }

    pub fn firing(&self) -> Vec<u64> {
        self.inner.lock().unwrap().firing.iter().copied().collect()
    }

    pub fn is_firing(&self, node: u64) -> bool {
        self.inner.lock().unwrap().firing.contains(&node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alarm_fires_after_sustained_lag() {
        let a = FollowerLagAlarm::new(100, Duration::from_millis(5));
        a.update_leader_lsn(1000);
        a.observe_follower(1, 500); // lag = 500 > 100
        std::thread::sleep(Duration::from_millis(15));
        a.observe_follower(1, 500);
        assert!(a.is_firing(1));
    }

    #[test]
    fn alarm_clears_when_caught_up() {
        let a = FollowerLagAlarm::new(100, Duration::from_millis(5));
        a.update_leader_lsn(1000);
        a.observe_follower(1, 500);
        std::thread::sleep(Duration::from_millis(15));
        a.observe_follower(1, 500);
        assert!(a.is_firing(1));
        a.observe_follower(1, 990);
        assert!(!a.is_firing(1));
    }

    #[test]
    fn brief_lag_does_not_fire() {
        let a = FollowerLagAlarm::new(100, Duration::from_secs(60));
        a.update_leader_lsn(1000);
        a.observe_follower(1, 500);
        assert!(!a.is_firing(1));
    }

    #[test]
    fn within_threshold_no_alarm() {
        let a = FollowerLagAlarm::new(100, Duration::from_millis(1));
        a.update_leader_lsn(1000);
        a.observe_follower(1, 950);
        std::thread::sleep(Duration::from_millis(10));
        a.observe_follower(1, 950);
        assert!(!a.is_firing(1));
    }

    #[test]
    fn firing_lists_all_active_alarms() {
        let a = FollowerLagAlarm::new(100, Duration::from_millis(5));
        a.update_leader_lsn(1000);
        a.observe_follower(1, 500);
        a.observe_follower(2, 500);
        std::thread::sleep(Duration::from_millis(15));
        a.observe_follower(1, 500);
        a.observe_follower(2, 500);
        assert_eq!(a.firing().len(), 2);
    }
}
