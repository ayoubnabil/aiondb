//! Cross-shard snapshot multiplexer.
//!
//! Combines per-shard row streams pinned at a shared snapshot
//! timestamp into a single merged iterator that the SQL executor can
//! consume as if it came from one shard. The merge preserves the
//! per-shard order so callers can layer a stable sort on top.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use aiondb_core::Row;

#[derive(Debug)]
pub struct ShardStream {
    pub shard_id: u32,
    pub rows: Vec<Row>,
    pub cursor: usize,
}

impl ShardStream {
    pub fn new(shard_id: u32, rows: Vec<Row>) -> Self {
        Self {
            shard_id,
            rows,
            cursor: 0,
        }
    }

    fn peek(&self) -> Option<&Row> {
        self.rows.get(self.cursor)
    }

    fn advance(&mut self) -> Option<Row> {
        if self.cursor >= self.rows.len() {
            return None;
        }
        let row = self.rows[self.cursor].clone();
        self.cursor += 1;
        Some(row)
    }
}

/// Snapshot mux. Merges by ascending value of column `sort_ordinal`.
pub struct SnapshotMux {
    streams: Vec<ShardStream>,
    sort_ordinal: usize,
}

impl SnapshotMux {
    pub fn new(streams: Vec<ShardStream>, sort_ordinal: usize) -> Self {
        Self {
            streams,
            sort_ordinal,
        }
    }

    /// Merge into a single `Vec<Row>`, ordered by the sort ordinal.
    pub fn merge_sorted(mut self) -> Vec<Row> {
        let mut out = Vec::new();
        let mut heap: BinaryHeap<HeapEntry> = BinaryHeap::new();
        for (idx, stream) in self.streams.iter().enumerate() {
            if let Some(row) = stream.peek() {
                heap.push(HeapEntry::new(idx, row.clone(), self.sort_ordinal));
            }
        }
        while let Some(entry) = heap.pop() {
            let stream = &mut self.streams[entry.stream_idx];
            stream.advance();
            out.push(entry.row);
            if let Some(next) = stream.peek() {
                heap.push(HeapEntry::new(
                    entry.stream_idx,
                    next.clone(),
                    self.sort_ordinal,
                ));
            }
        }
        out
    }

    /// Merge ignoring order — concatenates streams in input order.
    pub fn merge_concat(mut self) -> Vec<Row> {
        let mut out = Vec::new();
        for stream in &mut self.streams {
            while let Some(row) = stream.advance() {
                out.push(row);
            }
        }
        out
    }
}

#[derive(Debug)]
struct HeapEntry {
    stream_idx: usize,
    row: Row,
    sort_ordinal: usize,
}

impl HeapEntry {
    fn new(stream_idx: usize, row: Row, sort_ordinal: usize) -> Self {
        Self {
            stream_idx,
            row,
            sort_ordinal,
        }
    }
}

impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        compare_rows(&self.row, &other.row, self.sort_ordinal) == Ordering::Equal
    }
}

impl Eq for HeapEntry {}

impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // BinaryHeap is a max-heap; we want min ordering so reverse.
        compare_rows(&self.row, &other.row, self.sort_ordinal).reverse()
    }
}

fn compare_rows(a: &Row, b: &Row, ordinal: usize) -> Ordering {
    use aiondb_core::Value;
    let av = a.values.get(ordinal);
    let bv = b.values.get(ordinal);
    match (av, bv) {
        (Some(Value::Int(x)), Some(Value::Int(y))) => x.cmp(y),
        (Some(Value::BigInt(x)), Some(Value::BigInt(y))) => x.cmp(y),
        (Some(Value::Text(x)), Some(Value::Text(y))) => x.cmp(y),
        _ => Ordering::Equal,
    }
}

#[cfg(test)]
mod tests {
    use aiondb_core::Value;

    use super::*;

    fn row(v: i64) -> Row {
        Row::new(vec![Value::BigInt(v)])
    }

    #[test]
    fn merge_sorted_returns_global_ordering() {
        let s1 = ShardStream::new(1, vec![row(1), row(3), row(5)]);
        let s2 = ShardStream::new(2, vec![row(2), row(4), row(6)]);
        let mux = SnapshotMux::new(vec![s1, s2], 0);
        let merged = mux.merge_sorted();
        let vals: Vec<i64> = merged
            .into_iter()
            .map(|r| match r.values[0] {
                Value::BigInt(n) => n,
                _ => 0,
            })
            .collect();
        assert_eq!(vals, vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn merge_concat_keeps_input_order() {
        let s1 = ShardStream::new(1, vec![row(10), row(20)]);
        let s2 = ShardStream::new(2, vec![row(5)]);
        let mux = SnapshotMux::new(vec![s1, s2], 0);
        let merged = mux.merge_concat();
        let vals: Vec<i64> = merged
            .into_iter()
            .map(|r| match r.values[0] {
                Value::BigInt(n) => n,
                _ => 0,
            })
            .collect();
        assert_eq!(vals, vec![10, 20, 5]);
    }

    #[test]
    fn empty_streams_yield_empty_result() {
        let mux = SnapshotMux::new(vec![], 0);
        assert!(mux.merge_sorted().is_empty());
    }

    #[test]
    fn duplicates_are_preserved() {
        let s1 = ShardStream::new(1, vec![row(2), row(2)]);
        let s2 = ShardStream::new(2, vec![row(2)]);
        let mux = SnapshotMux::new(vec![s1, s2], 0);
        let merged = mux.merge_sorted();
        assert_eq!(merged.len(), 3);
    }

    #[test]
    fn text_sort_ordinal_works() {
        let s1 = ShardStream::new(
            1,
            vec![
                Row::new(vec![Value::Text("apple".into())]),
                Row::new(vec![Value::Text("cherry".into())]),
            ],
        );
        let s2 = ShardStream::new(2, vec![Row::new(vec![Value::Text("banana".into())])]);
        let mux = SnapshotMux::new(vec![s1, s2], 0);
        let merged = mux.merge_sorted();
        let vals: Vec<String> = merged
            .into_iter()
            .map(|r| match &r.values[0] {
                Value::Text(s) => s.clone(),
                _ => String::new(),
            })
            .collect();
        assert_eq!(vals, vec!["apple", "banana", "cherry"]);
    }
}
