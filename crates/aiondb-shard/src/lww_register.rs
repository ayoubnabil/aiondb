//! Last-Writer-Wins Register CRDT.
//!
//! Each write carries an HLC timestamp; on merge, the newer timestamp
//! wins. Ties break deterministically on the (timestamp, writer_id)
//! tuple so concurrent writes converge.

use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct LwwTimestamp {
    pub wall_us: u64,
    pub logical: u32,
    pub writer_id: u64,
}

#[derive(Clone, Debug)]
pub struct LwwRegister<T: Clone + std::fmt::Debug> {
    inner: Arc<std::sync::Mutex<Option<(LwwTimestamp, T)>>>,
}

impl<T: Clone + std::fmt::Debug> Default for LwwRegister<T> {
    fn default() -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(None)),
        }
    }
}

impl<T: Clone + std::fmt::Debug> LwwRegister<T> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn write(&self, ts: LwwTimestamp, value: T) {
        let mut guard = self.inner.lock().unwrap();
        let should_apply = match guard.as_ref() {
            None => true,
            Some((current, _)) => ts > *current,
        };
        if should_apply {
            *guard = Some((ts, value));
        }
    }

    pub fn read(&self) -> Option<(LwwTimestamp, T)> {
        self.inner.lock().unwrap().clone()
    }

    pub fn merge(&self, other: &LwwRegister<T>) {
        let other_state = other.inner.lock().unwrap().clone();
        if let Some((ts, value)) = other_state {
            self.write(ts, value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(wall: u64, writer: u64) -> LwwTimestamp {
        LwwTimestamp {
            wall_us: wall,
            logical: 0,
            writer_id: writer,
        }
    }

    #[test]
    fn newer_timestamp_overwrites() {
        let r: LwwRegister<u32> = LwwRegister::new();
        r.write(ts(100, 1), 1);
        r.write(ts(200, 1), 2);
        let (_, v) = r.read().unwrap();
        assert_eq!(v, 2);
    }

    #[test]
    fn older_write_is_ignored() {
        let r: LwwRegister<u32> = LwwRegister::new();
        r.write(ts(200, 1), 2);
        r.write(ts(100, 1), 1);
        let (_, v) = r.read().unwrap();
        assert_eq!(v, 2);
    }

    #[test]
    fn writer_id_breaks_ts_ties() {
        let r: LwwRegister<u32> = LwwRegister::new();
        r.write(ts(100, 1), 1);
        r.write(ts(100, 2), 2);
        // ts(100,2) > ts(100,1) since writer_id is part of the tuple.
        let (_, v) = r.read().unwrap();
        assert_eq!(v, 2);
    }

    #[test]
    fn merge_takes_newer_value() {
        let a: LwwRegister<u32> = LwwRegister::new();
        a.write(ts(100, 1), 1);
        let b: LwwRegister<u32> = LwwRegister::new();
        b.write(ts(200, 2), 2);
        a.merge(&b);
        let (_, v) = a.read().unwrap();
        assert_eq!(v, 2);
    }

    #[test]
    fn empty_register_reads_none() {
        let r: LwwRegister<u32> = LwwRegister::new();
        assert!(r.read().is_none());
    }
}
