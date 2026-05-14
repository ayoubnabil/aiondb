//! Leader heartbeat emitter.
//!
//! On a quiet leader, heartbeats need only fire often enough to
//! keep the followers' election timers from expiring. On a busy
//! leader, log entries already double as heartbeats; the emitter
//! suppresses redundant heartbeats. The cadence adapts to the
//! recent traffic.

use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct HeartbeatConfig {
    pub idle_interval: Duration,
    pub min_interval: Duration,
    pub suppress_after_send: Duration,
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        Self {
            idle_interval: Duration::from_millis(50),
            min_interval: Duration::from_millis(10),
            suppress_after_send: Duration::from_millis(40),
        }
    }
}

#[derive(Clone, Debug)]
pub struct LeaderHeartbeatEmitter {
    inner: Arc<std::sync::Mutex<EmitterState>>,
    config: HeartbeatConfig,
}

#[derive(Debug)]
struct EmitterState {
    last_send: Option<Instant>,
    last_message: Option<Instant>,
}

impl LeaderHeartbeatEmitter {
    pub fn new(config: HeartbeatConfig) -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(EmitterState {
                last_send: None,
                last_message: None,
            })),
            config,
        }
    }

    pub fn record_message(&self) {
        self.inner.lock().unwrap().last_message = Some(Instant::now());
    }

    pub fn should_emit(&self) -> bool {
        let g = self.inner.lock().unwrap();
        let now = Instant::now();
        if let Some(last_msg) = g.last_message {
            if now.saturating_duration_since(last_msg) < self.config.suppress_after_send {
                return false;
            }
        }
        if let Some(last) = g.last_send {
            if now.saturating_duration_since(last) < self.config.min_interval {
                return false;
            }
            if now.saturating_duration_since(last) >= self.config.idle_interval {
                return true;
            }
            return false;
        }
        true
    }

    pub fn record_emit(&self) {
        let mut g = self.inner.lock().unwrap();
        g.last_send = Some(Instant::now());
        g.last_message = Some(Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_call_emits() {
        let e = LeaderHeartbeatEmitter::new(HeartbeatConfig::default());
        assert!(e.should_emit());
    }

    #[test]
    fn busy_leader_suppresses_heartbeat() {
        let e = LeaderHeartbeatEmitter::new(HeartbeatConfig::default());
        e.record_emit();
        e.record_message();
        assert!(!e.should_emit());
    }

    #[test]
    fn idle_leader_emits_after_interval() {
        let e = LeaderHeartbeatEmitter::new(HeartbeatConfig {
            idle_interval: Duration::from_millis(5),
            min_interval: Duration::from_millis(1),
            suppress_after_send: Duration::from_millis(1),
        });
        e.record_emit();
        std::thread::sleep(Duration::from_millis(10));
        assert!(e.should_emit());
    }

    #[test]
    fn min_interval_prevents_rapid_emit() {
        let e = LeaderHeartbeatEmitter::new(HeartbeatConfig {
            idle_interval: Duration::from_millis(1),
            min_interval: Duration::from_secs(60),
            suppress_after_send: Duration::ZERO,
        });
        e.record_emit();
        assert!(!e.should_emit());
    }

    #[test]
    fn record_emit_resets_state() {
        let e = LeaderHeartbeatEmitter::new(HeartbeatConfig::default());
        e.record_emit();
        let inner = e.inner.lock().unwrap();
        assert!(inner.last_send.is_some());
    }
}
