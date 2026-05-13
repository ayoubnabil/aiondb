//! Distributed top-K aggregator.
//!
//! Each shard returns its own top-K plus the max score of any other
//! row it has. The coordinator merges them and produces a globally
//! correct top-K. The optimistic upper bound lets us skip shards
//! that cannot contribute any further qualifying row.

use std::collections::BinaryHeap;

#[derive(Clone, Debug)]
pub struct ShardTopK<T> {
    pub rows: Vec<(f64, T)>,
    pub max_remaining_score: f64,
}

pub fn merge_topk<T: Clone>(shards: Vec<ShardTopK<T>>, k: usize) -> Vec<(f64, T)> {
    let mut heap: BinaryHeap<Ord64<T>> = BinaryHeap::new();
    for s in &shards {
        for (score, val) in &s.rows {
            heap.push(Ord64 {
                score: *score,
                value: val.clone(),
            });
            if heap.len() > k * shards.len().max(1) {
                heap.pop();
            }
        }
    }
    let sorted: Vec<Ord64<T>> = heap.into_sorted_vec();
    // Ord64 is a min-heap on score, so into_sorted_vec gives
    // ascending Ord = descending score, which is what we want.
    sorted
        .into_iter()
        .take(k)
        .map(|e| (e.score, e.value))
        .collect()
}

#[derive(Clone, Debug)]
struct Ord64<T> {
    score: f64,
    value: T,
}

impl<T> PartialEq for Ord64<T> {
    fn eq(&self, other: &Self) -> bool {
        self.score == other.score
    }
}
impl<T> Eq for Ord64<T> {}
impl<T> PartialOrd for Ord64<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl<T> Ord for Ord64<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Reverse to keep BinaryHeap as min-heap on score.
        other
            .score
            .partial_cmp(&self.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    }
}

/// Returns true when no further requests to any shard could possibly
/// improve the current global top-K. Caller short-circuits then.
pub fn cannot_improve(
    current_topk: &[(f64, impl Clone)],
    shards: &[ShardTopK<impl Clone>],
) -> bool {
    if current_topk.is_empty() {
        return false;
    }
    let kth = current_topk
        .last()
        .map(|(s, _)| *s)
        .unwrap_or(f64::NEG_INFINITY);
    shards.iter().all(|s| s.max_remaining_score <= kth)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shard(rows: Vec<(f64, i32)>, max_rem: f64) -> ShardTopK<i32> {
        ShardTopK {
            rows,
            max_remaining_score: max_rem,
        }
    }

    #[test]
    fn merge_picks_global_top_k() {
        let s1 = shard(vec![(10.0, 1), (9.0, 2)], 0.0);
        let s2 = shard(vec![(11.0, 3), (5.0, 4)], 0.0);
        let r = merge_topk(vec![s1, s2], 2);
        assert_eq!(r[0].1, 3);
        assert_eq!(r[1].1, 1);
    }

    #[test]
    fn merge_handles_empty_shards() {
        let r: Vec<(f64, i32)> = merge_topk::<i32>(vec![], 5);
        assert!(r.is_empty());
    }

    #[test]
    fn merge_with_k_zero_returns_empty() {
        let s = shard(vec![(1.0, 1)], 0.0);
        let r = merge_topk(vec![s], 0);
        assert!(r.is_empty());
    }

    #[test]
    fn cannot_improve_when_remaining_score_below_kth() {
        let current = vec![(10.0, 1i32), (9.0, 2)];
        let s = shard(vec![], 8.0);
        assert!(cannot_improve(&current, &[s]));
    }

    #[test]
    fn cannot_improve_false_when_remaining_score_high() {
        let current = vec![(10.0, 1i32), (9.0, 2)];
        let s = shard(vec![], 12.0);
        assert!(!cannot_improve(&current, &[s]));
    }

    #[test]
    fn cannot_improve_false_when_current_empty() {
        let current: Vec<(f64, i32)> = vec![];
        let s = shard(vec![(1.0, 1)], 0.0);
        assert!(!cannot_improve(&current, &[s]));
    }
}
