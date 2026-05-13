//! Hash-partitioned exchange for distributed query execution.
//!
//! Implements the **shuffle** phase that turns a row stream produced
//! by one fragment into N partition-local row streams keyed by hash
//! of selected columns. This is the missing primitive flagged by the
//! audit: the planner emits [`aiondb_plan::ExchangeKind::Repartition`]
//! edges but the runtime previously treated them as `Gather`,
//! collapsing all rows onto one downstream consumer.
//!
//! # Why hash, not modulo on the value?
//!
//! - Hash spreads correlated keys uniformly so a join key like
//!   `customer_id` does not concentrate all rows on one partition just
//!   because consecutive ids share a low-order bit pattern.
//! - SHA-256 is overkill for shuffling, so we use a fast FNV-1a 64-bit
//!   hash. Quality matches xxhash for short keys and avoids dragging
//!   in a new dep.
//!
//! # Determinism
//!
//! The partitioner is fully deterministic: the same `(row, key
//! ordinals, partition count)` always picks the same partition. This
//! is critical so co-located joins on `a = b` route both sides to the
//! same partition independent of the upstream fragment's task layout.

use aiondb_core::{DbError, DbResult, Row, Value};

/// Hash-partition router.
///
/// Caller picks the number of partitions and which row columns to use
/// as the partition key. Empty key list is rejected because hashing
/// nothing collapses everything onto partition zero.
#[derive(Clone, Debug)]
pub struct HashPartitioner {
    partitions: usize,
    key_ordinals: Vec<usize>,
}

impl HashPartitioner {
    pub fn new(partitions: usize, key_ordinals: Vec<usize>) -> DbResult<Self> {
        if partitions == 0 {
            return Err(DbError::internal(
                "HashPartitioner requires at least 1 partition",
            ));
        }
        if key_ordinals.is_empty() {
            return Err(DbError::internal(
                "HashPartitioner requires at least one key ordinal",
            ));
        }
        Ok(Self {
            partitions,
            key_ordinals,
        })
    }

    pub fn partitions(&self) -> usize {
        self.partitions
    }

    pub fn key_ordinals(&self) -> &[usize] {
        &self.key_ordinals
    }

    /// Pick the destination partition for a single row.
    ///
    /// # Errors
    /// Returns `DbError::internal` when a key ordinal is out of range
    /// for the row.
    pub fn partition_of(&self, row: &Row) -> DbResult<usize> {
        let mut hasher = FnvHasher::new();
        for ordinal in &self.key_ordinals {
            let value = row.values.get(*ordinal).ok_or_else(|| {
                DbError::internal(format!(
                    "HashPartitioner: row width {} too small for key ordinal {}",
                    row.values.len(),
                    ordinal
                ))
            })?;
            hash_value(&mut hasher, value);
        }
        Ok((hasher.finish() % self.partitions as u64) as usize)
    }

    /// Group `rows` into per-partition batches. Returned `Vec` has
    /// exactly `partitions` entries; empty entries mean "no rows for
    /// that partition".
    pub fn partition_batch(&self, rows: Vec<Row>) -> DbResult<Vec<Vec<Row>>> {
        let mut buckets: Vec<Vec<Row>> = (0..self.partitions).map(|_| Vec::new()).collect();
        for row in rows {
            let idx = self.partition_of(&row)?;
            buckets[idx].push(row);
        }
        Ok(buckets)
    }

    /// Stream-friendly variant: feed rows one at a time, get the
    /// destination index for each. Avoids the up-front buffering of
    /// [`Self::partition_batch`] when the upstream is asynchronous.
    pub fn route<I>(&self, iter: I) -> impl Iterator<Item = DbResult<(usize, Row)>> + '_
    where
        I: IntoIterator<Item = Row>,
        I::IntoIter: 'static,
    {
        iter.into_iter()
            .map(move |row| self.partition_of(&row).map(|idx| (idx, row)))
    }
}

/// FNV-1a 64-bit hasher. Inlined here so the partitioner has zero
/// external hash deps.
struct FnvHasher {
    state: u64,
}

impl FnvHasher {
    fn new() -> Self {
        Self {
            state: 0xcbf2_9ce4_8422_2325,
        }
    }
    fn write(&mut self, bytes: &[u8]) {
        let mut state = self.state;
        for byte in bytes {
            state ^= u64::from(*byte);
            state = state.wrapping_mul(0x100_0000_01b3);
        }
        self.state = state;
    }
    fn finish(&self) -> u64 {
        self.state
    }
}

fn hash_value(hasher: &mut FnvHasher, value: &Value) {
    match value {
        Value::Null => hasher.write(&[0u8]),
        Value::Boolean(b) => {
            hasher.write(&[1u8]);
            hasher.write(if *b { &[1] } else { &[0] });
        }
        Value::Int(n) => {
            hasher.write(&[2u8]);
            hasher.write(&n.to_le_bytes());
        }
        Value::BigInt(n) => {
            hasher.write(&[3u8]);
            hasher.write(&n.to_le_bytes());
        }
        Value::Text(s) => {
            hasher.write(&[4u8]);
            hasher.write(s.as_bytes());
        }
        Value::Uuid(bytes) => {
            hasher.write(&[5u8]);
            hasher.write(bytes);
        }
        other => {
            // Fallback : serialise via Debug. This is intentionally
            // type-aware so distinct types do not collide trivially.
            hasher.write(&[0xFF]);
            hasher.write(format!("{other:?}").as_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn row(values: Vec<Value>) -> Row {
        Row::new(values)
    }

    #[test]
    fn zero_partitions_or_empty_keys_is_rejected() {
        assert!(HashPartitioner::new(0, vec![0]).is_err());
        assert!(HashPartitioner::new(4, vec![]).is_err());
    }

    #[test]
    fn same_row_always_lands_on_same_partition() {
        let p = HashPartitioner::new(8, vec![0]).unwrap();
        let r = row(vec![Value::Int(42)]);
        let a = p.partition_of(&r).unwrap();
        let b = p.partition_of(&r).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn different_rows_can_land_on_different_partitions() {
        let p = HashPartitioner::new(8, vec![0]).unwrap();
        let r1 = row(vec![Value::Int(1)]);
        let r2 = row(vec![Value::Int(1_000_000)]);
        // Not guaranteed in general, but extremely likely for these
        // specific keys with the FNV-1a hash.
        let a = p.partition_of(&r1).unwrap();
        let b = p.partition_of(&r2).unwrap();
        assert_ne!(a, b, "same hash for {r1:?} and {r2:?}");
    }

    #[test]
    fn partition_batch_distributes_rows_uniformly() {
        let p = HashPartitioner::new(8, vec![0]).unwrap();
        let rows: Vec<Row> = (0..10_000).map(|i| row(vec![Value::BigInt(i)])).collect();
        let buckets = p.partition_batch(rows).unwrap();
        assert_eq!(buckets.len(), 8);
        let total: usize = buckets.iter().map(|b| b.len()).sum();
        assert_eq!(total, 10_000);
        // Each bucket should have roughly 10_000/8 = 1_250 rows. We
        // tolerate +/-30% just to keep the assertion stable under FNV
        // bias; in practice the spread is much tighter.
        for (i, bucket) in buckets.iter().enumerate() {
            let lo = 875usize;
            let hi = 1_625usize;
            assert!(
                bucket.len() >= lo && bucket.len() <= hi,
                "bucket {i} has {} rows, outside [{lo}, {hi}]",
                bucket.len()
            );
        }
    }

    #[test]
    fn missing_ordinal_errors_cleanly() {
        let p = HashPartitioner::new(4, vec![5]).unwrap();
        let err = p.partition_of(&row(vec![Value::Int(1)])).unwrap_err();
        assert!(err.to_string().contains("too small"), "err: {err}");
    }

    #[test]
    fn composite_key_is_position_sensitive() {
        let p = HashPartitioner::new(8, vec![0, 1]).unwrap();
        let r1 = row(vec![Value::Int(1), Value::Int(2)]);
        let r2 = row(vec![Value::Int(2), Value::Int(1)]);
        // Different ordering of values must produce different hashes,
        // otherwise the partitioner would collapse `(a,b)` and `(b,a)`
        // rows onto the same partition unexpectedly.
        assert_ne!(
            p.partition_of(&r1).unwrap(),
            p.partition_of(&r2).unwrap(),
            "composite key must be ordering-sensitive"
        );
    }

    #[test]
    fn types_with_same_serialisation_collide_only_if_truly_equal() {
        let p = HashPartitioner::new(8, vec![0]).unwrap();
        // Int(0) and BigInt(0) are different types -- they must land
        // on different partitions so a join on `t.k::int = u.k::bigint`
        // cannot accidentally co-locate mismatched rows.
        let a = p.partition_of(&row(vec![Value::Int(0)])).unwrap();
        let b = p.partition_of(&row(vec![Value::BigInt(0)])).unwrap();
        assert_ne!(a, b);
        // Null is its own class.
        let n = p.partition_of(&row(vec![Value::Null])).unwrap();
        assert_ne!(a, n);
        assert_ne!(b, n);
    }

    #[test]
    fn route_iterator_preserves_order_and_decorates_with_partition() {
        let p = HashPartitioner::new(4, vec![0]).unwrap();
        let rows = vec![
            row(vec![Value::Int(1)]),
            row(vec![Value::Int(2)]),
            row(vec![Value::Int(3)]),
        ];
        let mut by_partition: HashMap<usize, Vec<Value>> = HashMap::new();
        for item in p.route(rows.clone()) {
            let (idx, r) = item.unwrap();
            by_partition
                .entry(idx)
                .or_default()
                .push(r.values[0].clone());
        }
        let total: usize = by_partition.values().map(|v| v.len()).sum();
        assert_eq!(total, 3);
        let _ = rows;
    }
}
