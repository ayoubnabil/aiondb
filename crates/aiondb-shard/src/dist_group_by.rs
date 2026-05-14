//! Distributed GROUP BY aggregator.
//!
//! Two phases :
//!
//! 1. **Local pre-aggregation** — each shard collapses rows in its
//!    fragment into partial buckets using an associative reducer.
//! 2. **Global aggregation** — the coordinator merges partial
//!    buckets per group key.
//!
//! For sum/count/min/max this gives identical results to a single-
//! site GROUP BY. AVG is supported via (sum, count) carry pairs.

use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AggOp {
    Sum,
    Count,
    Min,
    Max,
    Avg,
}

#[derive(Clone, Copy, Debug)]
pub struct PartialAgg {
    pub op: AggOp,
    pub sum: f64,
    pub count: u64,
    pub min: f64,
    pub max: f64,
}

impl PartialAgg {
    pub fn for_value(op: AggOp, v: f64) -> Self {
        Self {
            op,
            sum: v,
            count: 1,
            min: v,
            max: v,
        }
    }

    pub fn merge(&mut self, other: PartialAgg) {
        debug_assert_eq!(self.op, other.op);
        self.sum += other.sum;
        self.count += other.count;
        self.min = self.min.min(other.min);
        self.max = self.max.max(other.max);
    }

    pub fn finalize(&self) -> f64 {
        match self.op {
            AggOp::Sum => self.sum,
            AggOp::Count => self.count as f64,
            AggOp::Min => self.min,
            AggOp::Max => self.max,
            AggOp::Avg => {
                if self.count == 0 {
                    0.0
                } else {
                    self.sum / self.count as f64
                }
            }
        }
    }
}

pub fn local_aggregate(rows: &[(Vec<u8>, f64)], op: AggOp) -> BTreeMap<Vec<u8>, PartialAgg> {
    let mut buckets: BTreeMap<Vec<u8>, PartialAgg> = BTreeMap::new();
    for (key, v) in rows {
        let entry = buckets
            .entry(key.clone())
            .or_insert_with(|| PartialAgg::for_value(op, *v));
        // first insert with for_value already accounts for the first row; only
        // merge for subsequent rows.
        if !std::ptr::eq(
            entry as *const _,
            &PartialAgg::for_value(op, *v) as *const _,
        ) {
            // Detect first-vs-subsequent by checking insertion. Use len > 0 trick.
            // Simpler: always overwrite then merge.
        }
        // Simpler logic: rebuild buckets manually.
        let _ = entry;
    }
    // Rewrite using clean logic
    let mut clean: BTreeMap<Vec<u8>, PartialAgg> = BTreeMap::new();
    for (key, v) in rows {
        let p = PartialAgg::for_value(op, *v);
        clean
            .entry(key.clone())
            .and_modify(|cur| cur.merge(p))
            .or_insert(p);
    }
    clean
}

pub fn global_aggregate(
    shards: Vec<BTreeMap<Vec<u8>, PartialAgg>>,
) -> BTreeMap<Vec<u8>, PartialAgg> {
    let mut out: BTreeMap<Vec<u8>, PartialAgg> = BTreeMap::new();
    for shard in shards {
        for (k, p) in shard {
            out.entry(k).and_modify(|cur| cur.merge(p)).or_insert(p);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str) -> Vec<u8> {
        s.as_bytes().to_vec()
    }

    #[test]
    fn local_sum_aggregates() {
        let rows = vec![(key("a"), 1.0), (key("a"), 2.0), (key("b"), 3.0)];
        let r = local_aggregate(&rows, AggOp::Sum);
        assert_eq!(r.get(&key("a")).unwrap().finalize(), 3.0);
        assert_eq!(r.get(&key("b")).unwrap().finalize(), 3.0);
    }

    #[test]
    fn global_sum_combines_shards() {
        let s1 = local_aggregate(&[(key("a"), 1.0)], AggOp::Sum);
        let s2 = local_aggregate(&[(key("a"), 2.0), (key("b"), 5.0)], AggOp::Sum);
        let g = global_aggregate(vec![s1, s2]);
        assert_eq!(g.get(&key("a")).unwrap().finalize(), 3.0);
        assert_eq!(g.get(&key("b")).unwrap().finalize(), 5.0);
    }

    #[test]
    fn count_op_returns_row_count() {
        let s1 = local_aggregate(&[(key("a"), 0.0), (key("a"), 0.0)], AggOp::Count);
        let s2 = local_aggregate(&[(key("a"), 0.0)], AggOp::Count);
        let g = global_aggregate(vec![s1, s2]);
        assert_eq!(g.get(&key("a")).unwrap().finalize(), 3.0);
    }

    #[test]
    fn min_and_max() {
        let rows = vec![(key("g"), 5.0), (key("g"), 1.0), (key("g"), 9.0)];
        let mn = local_aggregate(&rows, AggOp::Min);
        let mx = local_aggregate(&rows, AggOp::Max);
        assert_eq!(mn.get(&key("g")).unwrap().finalize(), 1.0);
        assert_eq!(mx.get(&key("g")).unwrap().finalize(), 9.0);
    }

    #[test]
    fn avg_carries_sum_count() {
        let s1 = local_aggregate(&[(key("a"), 10.0), (key("a"), 20.0)], AggOp::Avg);
        let s2 = local_aggregate(&[(key("a"), 30.0)], AggOp::Avg);
        let g = global_aggregate(vec![s1, s2]);
        assert_eq!(g.get(&key("a")).unwrap().finalize(), 20.0);
    }

    #[test]
    fn empty_input_returns_empty_output() {
        let r: BTreeMap<Vec<u8>, PartialAgg> = local_aggregate(&[], AggOp::Sum);
        assert!(r.is_empty());
    }
}
