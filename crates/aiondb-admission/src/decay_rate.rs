//! Exponentially-decaying rate counter.
//!
//! Smooths instantaneous QPS into a moving average. Used as a
//! signal source for admission decisions.

use std::sync::Arc;
use std::time::Instant;

#[derive(Clone, Debug)]
pub struct DecayingRate {
    inner: Arc<std::sync::Mutex<DecayState>>,
    decay_half_life_us: f64,
}

#[derive(Debug)]
struct DecayState {
    value: f64,
    last_update: Instant,
}

impl DecayingRate {
    pub fn new(decay_half_life_ms: f64) -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(DecayState {
                value: 0.0,
                last_update: Instant::now(),
            })),
            decay_half_life_us: decay_half_life_ms * 1000.0,
        }
    }

    pub fn add(&self, sample: f64) {
        let now = Instant::now();
        let mut guard = self.inner.lock().unwrap();
        let elapsed = now.saturating_duration_since(guard.last_update).as_micros() as f64;
        if guard.value > 0.0 && self.decay_half_life_us > 0.0 {
            let decay = 0.5f64.powf(elapsed / self.decay_half_life_us);
            guard.value *= decay;
        }
        guard.value += sample;
        guard.last_update = now;
    }

    pub fn value(&self) -> f64 {
        let mut guard = self.inner.lock().unwrap();
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(guard.last_update).as_micros() as f64;
        if guard.value > 0.0 && self.decay_half_life_us > 0.0 {
            let decay = 0.5f64.powf(elapsed / self.decay_half_life_us);
            guard.value *= decay;
            guard.last_update = now;
        }
        guard.value
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn add_increases_value() {
        let r = DecayingRate::new(1000.0);
        r.add(5.0);
        r.add(3.0);
        assert!(r.value() >= 7.5);
    }

    #[test]
    fn value_decays_over_time() {
        let r = DecayingRate::new(10.0);
        r.add(10.0);
        std::thread::sleep(Duration::from_millis(50));
        let after = r.value();
        assert!(after < 10.0);
    }

    #[test]
    fn zero_half_life_does_not_decay() {
        let r = DecayingRate::new(0.0);
        r.add(5.0);
        std::thread::sleep(Duration::from_millis(10));
        assert_eq!(r.value(), 5.0);
    }

    #[test]
    fn empty_counter_is_zero() {
        let r = DecayingRate::new(100.0);
        assert_eq!(r.value(), 0.0);
    }

    #[test]
    fn negative_samples_supported_for_credit_back() {
        let r = DecayingRate::new(0.0);
        r.add(10.0);
        r.add(-3.0);
        assert!((r.value() - 7.0).abs() < 0.01);
    }
}
