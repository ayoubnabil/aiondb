//! SQL surface for distrib state.
//!
//! Exposes the contents of the range descriptor / lease / closed-ts
//! registries as virtual tables that the SQL planner can read. The
//! goal is to make `SHOW RANGES`, `SHOW LEASES`, `SHOW CLOSED_TIMESTAMPS`
//! land as ordinary `SELECT` plans against in-memory adapters.
//!
//! Each view is materialised as a `Vec<Row>` produced from the
//! authoritative source structure. Column layouts are stable and
//! documented inline so the planner can hardcode them.

use aiondb_core::{Row, Value};

use crate::closed_timestamp::ClosedTimestampTracker;
use crate::lease::LeaseRegistry;
use crate::range_descriptor::RangeDescriptorRegistry;

/// Column layouts.
pub mod columns {
    pub const RANGES: &[&str] = &[
        "range_id",
        "start_key",
        "end_key",
        "shard_id",
        "replica_count",
        "generation",
    ];
    pub const LEASES: &[&str] = &["shard_id", "holder", "epoch", "expires_at_ns"];
    pub const CLOSED_TIMESTAMPS: &[&str] = &["shard_id", "closed_ts_us"];
}

/// SHOW RANGES.
pub fn show_ranges(registry: &RangeDescriptorRegistry) -> Vec<Row> {
    registry
        .snapshot()
        .into_iter()
        .map(|d| {
            Row::new(vec![
                Value::BigInt(d.range_id.get() as i64),
                Value::Text(String::from_utf8_lossy(&d.start_key).into_owned()),
                Value::Text(if d.end_key.is_empty() {
                    "+Inf".to_owned()
                } else {
                    String::from_utf8_lossy(&d.end_key).into_owned()
                }),
                Value::BigInt(d.shard.get() as i64),
                Value::BigInt(d.replicas.len() as i64),
                Value::BigInt(d.generation as i64),
            ])
        })
        .collect()
}

/// SHOW LEASES.
pub fn show_leases(registry: &LeaseRegistry) -> Vec<Row> {
    registry
        .snapshot()
        .into_iter()
        .map(|l| {
            let expires_ns = l.expires_at.elapsed().as_nanos() as i64;
            Row::new(vec![
                Value::BigInt(l.shard.get() as i64),
                Value::BigInt(l.holder as i64),
                Value::BigInt(l.epoch.get() as i64),
                Value::BigInt(expires_ns),
            ])
        })
        .collect()
}

/// SHOW CLOSED_TIMESTAMPS.
pub fn show_closed_timestamps(tracker: &ClosedTimestampTracker) -> Vec<Row> {
    tracker
        .snapshot()
        .into_iter()
        .map(|(shard, ts)| {
            Row::new(vec![
                Value::BigInt(shard.get() as i64),
                Value::BigInt(ts as i64),
            ])
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::range_descriptor::{RangeDescriptor, RangeId, ReplicaDescriptor, ReplicaId};
    use crate::ShardId;

    #[test]
    fn show_ranges_emits_one_row_per_range() {
        let r = RangeDescriptorRegistry::new();
        r.upsert(RangeDescriptor {
            range_id: RangeId::new(1),
            start_key: b"a".to_vec(),
            end_key: b"m".to_vec(),
            replicas: vec![ReplicaDescriptor {
                replica_id: ReplicaId::new(1),
                node_id: "n1".into(),
                is_learner: false,
            }],
            shard: ShardId::new(7),
            lease: None,
            generation: 0,
        })
        .unwrap();
        r.upsert(RangeDescriptor {
            range_id: RangeId::new(2),
            start_key: b"m".to_vec(),
            end_key: b"".to_vec(),
            replicas: vec![ReplicaDescriptor {
                replica_id: ReplicaId::new(2),
                node_id: "n2".into(),
                is_learner: false,
            }],
            shard: ShardId::new(8),
            lease: None,
            generation: 0,
        })
        .unwrap();
        let rows = show_ranges(&r);
        assert_eq!(rows.len(), 2);
        // Order matches snapshot order (sorted by start_key).
        let r1 = &rows[0];
        match (&r1.values[0], &r1.values[2]) {
            (Value::BigInt(id), Value::Text(end)) => {
                assert_eq!(*id, 1);
                assert_eq!(end, "m");
            }
            other => panic!("unexpected row: {other:?}"),
        }
        let r2 = &rows[1];
        if let Value::Text(end) = &r2.values[2] {
            assert_eq!(end, "+Inf");
        } else {
            panic!("range 2 should end at +Inf");
        }
    }

    #[test]
    fn show_leases_emits_one_row_per_lease() {
        let lr = LeaseRegistry::new();
        lr.acquire(
            ShardId::new(1),
            100,
            Duration::from_secs(30),
            Instant::now(),
        );
        lr.acquire(
            ShardId::new(2),
            200,
            Duration::from_secs(30),
            Instant::now(),
        );
        let rows = show_leases(&lr);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn show_closed_timestamps_returns_per_shard_frontier() {
        let t = ClosedTimestampTracker::new();
        t.publish(ShardId::new(1), 100);
        t.publish(ShardId::new(2), 200);
        let rows = show_closed_timestamps(&t);
        assert_eq!(rows.len(), 2);
        if let (Value::BigInt(s1), Value::BigInt(ts1)) = (&rows[0].values[0], &rows[0].values[1]) {
            assert_eq!(*s1, 1);
            assert_eq!(*ts1, 100);
        } else {
            panic!("malformed row: {:?}", rows[0]);
        }
    }

    #[test]
    fn column_layouts_are_stable() {
        // Snapshot the column names; the planner relies on these.
        assert_eq!(columns::RANGES.len(), 6);
        assert_eq!(columns::LEASES.len(), 4);
        assert_eq!(columns::CLOSED_TIMESTAMPS.len(), 2);
        assert_eq!(columns::RANGES[0], "range_id");
        assert_eq!(columns::LEASES[0], "shard_id");
    }
}
