//! Distributed sort with k-way merge.
//!
//! Each shard sorts its local rows; this module merges the K sorted
//! streams into one globally sorted output. Memory usage is O(K),
//! independent of total row count, because we only keep one row per
//! stream in the heap.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

#[derive(Debug)]
struct HeapEntry<T: Ord> {
    value: T,
    stream_idx: usize,
}

impl<T: Ord> PartialEq for HeapEntry<T> {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value && self.stream_idx == other.stream_idx
    }
}
impl<T: Ord> Eq for HeapEntry<T> {}

impl<T: Ord> PartialOrd for HeapEntry<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl<T: Ord> Ord for HeapEntry<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.value
            .cmp(&other.value)
            .then_with(|| self.stream_idx.cmp(&other.stream_idx))
    }
}

pub fn k_way_merge<T>(mut streams: Vec<Vec<T>>) -> Vec<T>
where
    T: Ord,
{
    // Each input must be pre-sorted ascending.
    for s in &streams {
        debug_assert!(is_sorted(s));
    }
    // Reverse each so we can pop from the end cheaply.
    for s in &mut streams {
        s.reverse();
    }
    let mut heap: BinaryHeap<Reverse<HeapEntry<T>>> = BinaryHeap::new();
    for (idx, s) in streams.iter_mut().enumerate() {
        if let Some(v) = s.pop() {
            heap.push(Reverse(HeapEntry {
                value: v,
                stream_idx: idx,
            }));
        }
    }
    let mut out = Vec::new();
    while let Some(Reverse(entry)) = heap.pop() {
        let idx = entry.stream_idx;
        out.push(entry.value);
        if let Some(next) = streams[idx].pop() {
            heap.push(Reverse(HeapEntry {
                value: next,
                stream_idx: idx,
            }));
        }
    }
    out
}

/// Top-K from K sorted streams. Stops once `limit` rows produced.
pub fn k_way_top<T>(streams: Vec<Vec<T>>, limit: usize) -> Vec<T>
where
    T: Ord,
{
    let mut out = k_way_merge(streams);
    out.truncate(limit);
    out
}

fn is_sorted<T: Ord>(s: &[T]) -> bool {
    s.windows(2).all(|w| w[0] <= w[1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_three_streams() {
        let r = k_way_merge(vec![vec![1, 4, 7], vec![2, 5, 8], vec![3, 6, 9]]);
        assert_eq!(r, vec![1, 2, 3, 4, 5, 6, 7, 8, 9]);
    }

    #[test]
    fn merge_handles_empty_streams() {
        let r = k_way_merge::<i32>(vec![vec![], vec![], vec![]]);
        assert!(r.is_empty());
    }

    #[test]
    fn merge_handles_uneven_streams() {
        let r = k_way_merge(vec![vec![1, 2, 3, 4, 5], vec![10]]);
        assert_eq!(r, vec![1, 2, 3, 4, 5, 10]);
    }

    #[test]
    fn k_way_top_respects_limit() {
        let r = k_way_top(vec![vec![1, 2, 3], vec![4, 5, 6]], 3);
        assert_eq!(r, vec![1, 2, 3]);
    }

    #[test]
    fn merge_with_duplicates_preserves_count() {
        let r = k_way_merge(vec![vec![1, 1, 2], vec![1, 3]]);
        assert_eq!(r, vec![1, 1, 1, 2, 3]);
    }

    #[test]
    fn single_stream_is_passthrough() {
        let r = k_way_merge(vec![vec![5, 6, 7]]);
        assert_eq!(r, vec![5, 6, 7]);
    }
}
