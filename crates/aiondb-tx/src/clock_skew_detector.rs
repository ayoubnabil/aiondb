//! Cluster clock skew detector.
//!
//! Each gossip / heartbeat tick reports the remote node's wall time.
//! The detector keeps a sliding window of recent samples and computes
//! the apparent offset (peer_wall_ns - local_wall_ns - round_trip / 2).
//! Nodes whose offset exceeds `max_skew` are flagged so the lease
//! manager can refuse to grant them leadership.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SkewSample {
    pub local_ns: i128,
    pub peer_ns: i128,
    pub round_trip_ns: i128,
}

impl SkewSample {
    pub fn offset_ns(&self) -> i128 {
        self.peer_ns - self.local_ns - (self.round_trip_ns / 2)
    }
}

#[derive(Clone, Debug)]
pub struct NodeSkew {
    pub node_id: u64,
    pub median_offset_ns: i128,
    pub max_offset_ns: i128,
    pub samples: usize,
}

#[derive(Clone, Debug)]
pub struct ClockSkewDetector {
    inner: Arc<std::sync::Mutex<BTreeMap<u64, VecDeque<SkewSample>>>>,
    window: usize,
    max_skew: Duration,
}

impl ClockSkewDetector {
    pub fn new(window: usize, max_skew: Duration) -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(BTreeMap::new())),
            window: window.max(1),
            max_skew,
        }
    }

    pub fn record(&self, node_id: u64, sample: SkewSample) {
        let mut g = self.inner.lock().unwrap();
        let deq = g.entry(node_id).or_default();
        deq.push_back(sample);
        while deq.len() > self.window {
            deq.pop_front();
        }
    }

    pub fn skew(&self, node_id: u64) -> Option<NodeSkew> {
        let g = self.inner.lock().unwrap();
        let deq = g.get(&node_id)?;
        if deq.is_empty() {
            return None;
        }
        let mut offsets: Vec<i128> = deq.iter().map(|s| s.offset_ns()).collect();
        offsets.sort_unstable();
        let median = offsets[offsets.len() / 2];
        let max = offsets.iter().map(|o| o.abs()).max().unwrap_or(0);
        Some(NodeSkew {
            node_id,
            median_offset_ns: median,
            max_offset_ns: max,
            samples: deq.len(),
        })
    }

    pub fn flagged_nodes(&self) -> Vec<NodeSkew> {
        let g = self.inner.lock().unwrap();
        let mut out = Vec::new();
        let max_ns = self.max_skew.as_nanos() as i128;
        for (id, deq) in g.iter() {
            if deq.is_empty() {
                continue;
            }
            let mut offsets: Vec<i128> = deq.iter().map(|s| s.offset_ns()).collect();
            offsets.sort_unstable();
            let median = offsets[offsets.len() / 2];
            if median.abs() > max_ns {
                let max = offsets.iter().map(|o| o.abs()).max().unwrap_or(0);
                out.push(NodeSkew {
                    node_id: *id,
                    median_offset_ns: median,
                    max_offset_ns: max,
                    samples: deq.len(),
                });
            }
        }
        out
    }

    pub fn forget(&self, node_id: u64) {
        self.inner.lock().unwrap().remove(&node_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_samples_returns_none() {
        let d = ClockSkewDetector::new(8, Duration::from_millis(500));
        assert!(d.skew(1).is_none());
    }

    #[test]
    fn records_and_computes_median() {
        let d = ClockSkewDetector::new(8, Duration::from_millis(500));
        for i in 0..5 {
            d.record(
                1,
                SkewSample {
                    local_ns: 1_000_000,
                    peer_ns: 1_000_000 + i * 100_000,
                    round_trip_ns: 0,
                },
            );
        }
        let s = d.skew(1).unwrap();
        assert_eq!(s.samples, 5);
        assert!(s.median_offset_ns >= 0);
    }

    #[test]
    fn window_is_bounded() {
        let d = ClockSkewDetector::new(3, Duration::from_millis(500));
        for i in 0..10 {
            d.record(
                1,
                SkewSample {
                    local_ns: 0,
                    peer_ns: i,
                    round_trip_ns: 0,
                },
            );
        }
        assert_eq!(d.skew(1).unwrap().samples, 3);
    }

    #[test]
    fn flagged_nodes_exceed_threshold() {
        let d = ClockSkewDetector::new(4, Duration::from_millis(100));
        d.record(
            1,
            SkewSample {
                local_ns: 0,
                peer_ns: 200_000_000, // 200ms ahead
                round_trip_ns: 0,
            },
        );
        d.record(
            2,
            SkewSample {
                local_ns: 0,
                peer_ns: 1_000_000, // 1ms ahead
                round_trip_ns: 0,
            },
        );
        let flagged = d.flagged_nodes();
        assert_eq!(flagged.len(), 1);
        assert_eq!(flagged[0].node_id, 1);
    }

    #[test]
    fn rtt_subtracts_half() {
        let s = SkewSample {
            local_ns: 0,
            peer_ns: 100,
            round_trip_ns: 60,
        };
        assert_eq!(s.offset_ns(), 70);
    }

    #[test]
    fn forget_removes_history() {
        let d = ClockSkewDetector::new(4, Duration::from_millis(100));
        d.record(
            1,
            SkewSample {
                local_ns: 0,
                peer_ns: 0,
                round_trip_ns: 0,
            },
        );
        d.forget(1);
        assert!(d.skew(1).is_none());
    }
}
