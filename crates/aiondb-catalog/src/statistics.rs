use aiondb_core::{ColumnId, RelationId, TxnId, Value};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TableStatistics {
    pub table_id: RelationId,
    pub row_count: u64,
    pub total_bytes: u64,
    pub dead_row_count: u64,
    pub last_updated_by: Option<TxnId>,
    pub column_stats: Vec<ColumnStatistics>,
}

/// Per-column statistics used for selectivity estimation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColumnStatistics {
    pub column_id: ColumnId,
    /// Estimated number of distinct values (0 = unknown).
    pub ndistinct: f64,
    /// Fraction of NULL values (0.0–1.0).
    pub null_fraction: f64,
    /// Average width in bytes (0 = unknown).
    pub avg_width: u32,
    /// Equi-depth histogram for selectivity estimation (None = not collected).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub histogram: Option<Histogram>,
    /// Most Common Values and their frequencies (None = not collected).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcv: Option<McvList>,
    /// Pearson correlation between physical row order and column value
    /// order, in `[-1.0, 1.0]`. Mirrors `pg_stats.correlation`.
    /// Used by the cost model to blend sequential vs random heap-fetch I/O
    /// on index range scans (PG `genericcostestimate`). Defaults to 0
    /// when ANALYZE has not been run, matching PG's conservative behaviour.
    #[serde(default)]
    pub correlation: f64,
}

impl Eq for ColumnStatistics {}

/// Equi-depth histogram for selectivity estimation.
/// Each bucket covers approximately the same number of rows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Histogram {
    /// Bucket boundaries in sorted order (serialized as display strings).
    /// For N buckets there are N+1 boundaries.
    pub boundaries: Vec<String>,
    /// Number of rows per bucket (approximate).
    pub rows_per_bucket: f64,
    /// Number of distinct values in each bucket.
    pub ndistinct_per_bucket: Vec<f64>,
}

impl Eq for Histogram {}

/// Narrow a `usize` to `f64`. Histogram bucket counts are small enough in
/// practice (≪ 2^53) that the precision loss called out by
/// `clippy::cast_precision_loss` (already silenced crate-wide) does not
/// matter; the cast is the standard idiom for this conversion.
fn usize_to_f64(value: usize) -> f64 {
    value as f64
}

impl Histogram {
    /// Returns true if the histogram has no data.
    pub fn is_empty(&self) -> bool {
        self.boundaries.len() < 2
    }

    /// Number of buckets in the histogram.
    pub fn bucket_count(&self) -> usize {
        if self.boundaries.len() <= 1 {
            0
        } else {
            self.boundaries.len() - 1
        }
    }

    /// Estimate selectivity for an equality predicate: column = value.
    pub fn estimate_equality_selectivity(&self, value: &Value) -> Option<f64> {
        if self.is_empty() {
            return None;
        }
        let bucket_idx = self.find_bucket(value)?;
        let ndistinct = self
            .ndistinct_per_bucket
            .get(bucket_idx)
            .copied()
            .unwrap_or(1.0);
        if ndistinct <= 0.0 {
            return None;
        }
        let total_rows = self.rows_per_bucket * usize_to_f64(self.bucket_count());
        if total_rows <= 0.0 {
            return None;
        }
        Some((self.rows_per_bucket / ndistinct) / total_rows)
    }

    /// Estimate selectivity for a range predicate.
    pub fn estimate_range_selectivity(
        &self,
        lower: Option<&Value>,
        upper: Option<&Value>,
    ) -> Option<f64> {
        if self.is_empty() {
            return None;
        }
        let n_buckets = self.bucket_count();
        if n_buckets == 0 {
            return None;
        }
        let low_bucket = match lower {
            Some(v) => self.find_bucket(v).unwrap_or(0),
            None => 0,
        };
        let high_bucket = match upper {
            Some(v) => self.find_bucket(v).unwrap_or(n_buckets - 1),
            None => n_buckets - 1,
        };
        let matching_buckets = usize_to_f64(high_bucket.saturating_sub(low_bucket) + 1);
        Some((matching_buckets / usize_to_f64(n_buckets)).clamp(0.001, 1.0))
    }

    fn find_bucket(&self, value: &Value) -> Option<usize> {
        let n = self.bucket_count();
        if n == 0 {
            return None;
        }
        let value_str = value_to_sortable_string(value);
        let mut lo = 0usize;
        let mut hi = n;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.boundaries[mid + 1].as_str() <= value_str.as_str() {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        Some(lo.min(n - 1))
    }
}

/// Most Common Values list: the top-N most frequent values and their
/// relative frequencies (fraction of total rows).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct McvList {
    /// Most common values, sorted by frequency descending.
    pub values: Vec<Value>,
    /// Relative frequency of each value (fraction of total rows, 0.0–1.0).
    pub frequencies: Vec<f64>,
}

impl Eq for McvList {}

impl McvList {
    /// Sum of all MCV frequencies (the fraction of rows covered by MCVs).
    pub fn sum_frequencies(&self) -> f64 {
        self.frequencies.iter().sum()
    }

    /// Look up the frequency of a specific value.  Returns `None` if the
    /// value is not in the MCV list.
    pub fn frequency_of(&self, value: &Value) -> Option<f64> {
        self.values
            .iter()
            .position(|v| v == value)
            .map(|i| self.frequencies[i])
    }

    /// Number of values in the MCV list.
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Returns `true` if the MCV list is empty.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

/// Convert a Value to a lexically-monotonic string for histogram bucket
/// binary search. Naive `format!("{v:020}")` produces `-...001 < -...002`
/// (ASCII compares `1` < `2`) which reverses numeric order for negatives,
/// so we shift signed integers into an unsigned space before zero-padding,
/// and route floats through their bit pattern with sign-aware adjustment.
fn value_to_sortable_string(value: &Value) -> String {
    match value {
        Value::Int(v) => {
            let shifted = i64::from(*v).cast_unsigned() ^ (1u64 << 63);
            format!("{shifted:020}")
        }
        Value::BigInt(v) => {
            let shifted = v.cast_unsigned() ^ (1u64 << 63);
            format!("{shifted:020}")
        }
        Value::Real(v) => {
            let bits = v.to_bits();
            let key: u32 = if bits >> 31 == 0 {
                bits ^ 0x8000_0000
            } else {
                !bits
            };
            format!("{key:010}")
        }
        Value::Double(v) => {
            let bits = v.to_bits();
            let key: u64 = if bits >> 63 == 0 {
                bits ^ 0x8000_0000_0000_0000
            } else {
                !bits
            };
            format!("{key:020}")
        }
        Value::Text(v) => v.clone(),
        _ => format!("{value:?}"),
    }
}

impl ColumnStatistics {
    /// Estimate selectivity of an equality predicate on this column.
    /// Returns 1/ndistinct when known, otherwise a default.
    pub fn equality_selectivity(&self) -> f64 {
        if self.ndistinct > 0.0 {
            1.0 / self.ndistinct
        } else {
            DEFAULT_EQUALITY_SELECTIVITY
        }
    }
}

/// Default selectivity for equality predicates when no stats are available.
const DEFAULT_EQUALITY_SELECTIVITY: f64 = 0.01;

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_stats() -> TableStatistics {
        TableStatistics {
            table_id: RelationId::new(42),
            row_count: 1_000_000,
            total_bytes: 128 * 1024 * 1024,
            dead_row_count: 500,
            last_updated_by: Some(TxnId::new(99)),
            column_stats: Vec::new(),
        }
    }

    #[test]
    fn construction_with_all_fields() {
        let stats = sample_stats();
        assert_eq!(stats.table_id, RelationId::new(42));
        assert_eq!(stats.row_count, 1_000_000);
        assert_eq!(stats.total_bytes, 128 * 1024 * 1024);
        assert_eq!(stats.dead_row_count, 500);
        assert_eq!(stats.last_updated_by, Some(TxnId::new(99)));
    }

    #[test]
    fn clone_produces_equal() {
        let stats = sample_stats();
        let cloned = stats.clone();
        assert_eq!(stats, cloned);
    }

    #[test]
    fn ne_when_row_count_differs() {
        let a = sample_stats();
        let mut b = sample_stats();
        b.row_count = 0;
        assert_ne!(a, b);
    }

    #[test]
    fn last_updated_by_can_be_none() {
        let stats = TableStatistics {
            table_id: RelationId::new(1),
            row_count: 0,
            total_bytes: 0,
            dead_row_count: 0,
            last_updated_by: None,
            column_stats: Vec::new(),
        };
        assert_eq!(stats.last_updated_by, None);
    }

    #[test]
    fn ne_when_last_updated_by_differs() {
        let a = sample_stats();
        let mut b = sample_stats();
        b.last_updated_by = None;
        assert_ne!(a, b);
    }

    #[test]
    fn zero_stats() {
        let stats = TableStatistics {
            table_id: RelationId::new(0),
            row_count: 0,
            total_bytes: 0,
            dead_row_count: 0,
            last_updated_by: None,
            column_stats: Vec::new(),
        };
        assert_eq!(stats.row_count, 0);
        assert_eq!(stats.total_bytes, 0);
        assert_eq!(stats.dead_row_count, 0);
    }

    #[test]
    fn debug_format_contains_fields() {
        let stats = sample_stats();
        let dbg = format!("{stats:?}");
        assert!(dbg.contains("row_count"));
        assert!(dbg.contains("total_bytes"));
        assert!(dbg.contains("dead_row_count"));
        assert!(dbg.contains("last_updated_by"));
    }

    #[test]
    fn max_values() {
        let stats = TableStatistics {
            table_id: RelationId::new(u64::MAX),
            row_count: u64::MAX,
            total_bytes: u64::MAX,
            dead_row_count: u64::MAX,
            last_updated_by: Some(TxnId::new(u64::MAX)),
            column_stats: Vec::new(),
        };
        assert_eq!(stats.row_count, u64::MAX);
        assert_eq!(stats.total_bytes, u64::MAX);
    }

    // --- ne when total_bytes differs ---
    #[test]
    fn ne_when_total_bytes_differs() {
        let a = sample_stats();
        let mut b = sample_stats();
        b.total_bytes = 0;
        assert_ne!(a, b);
    }

    // --- ne when dead_row_count differs ---
    #[test]
    fn ne_when_dead_row_count_differs() {
        let a = sample_stats();
        let mut b = sample_stats();
        b.dead_row_count = 999_999;
        assert_ne!(a, b);
    }

    // --- ne when table_id differs ---
    #[test]
    fn ne_when_table_id_differs() {
        let a = sample_stats();
        let mut b = sample_stats();
        b.table_id = RelationId::new(1);
        assert_ne!(a, b);
    }

    // --- dead_row_count greater than row_count is structurally allowed ---
    #[test]
    fn dead_rows_greater_than_row_count() {
        let stats = TableStatistics {
            table_id: RelationId::new(1),
            row_count: 10,
            total_bytes: 100,
            dead_row_count: 1000,
            last_updated_by: None,
            column_stats: Vec::new(),
        };
        assert!(stats.dead_row_count > stats.row_count);
    }

    // --- Statistics with row_count=1 (single row table) ---
    #[test]
    fn single_row_statistics() {
        let stats = TableStatistics {
            table_id: RelationId::new(1),
            row_count: 1,
            total_bytes: 64,
            dead_row_count: 0,
            last_updated_by: Some(TxnId::new(1)),
            column_stats: Vec::new(),
        };
        assert_eq!(stats.row_count, 1);
        assert_eq!(stats.dead_row_count, 0);
    }

    // --- Clone independence: modifying clone doesn't affect original ---
    #[test]
    fn clone_is_independent() {
        let stats = sample_stats();
        let mut cloned = stats.clone();
        cloned.row_count = 0;
        cloned.last_updated_by = None;
        assert_ne!(stats.row_count, cloned.row_count);
        assert_ne!(stats.last_updated_by, cloned.last_updated_by);
    }

    // --- last_updated_by with TxnId(0) ---
    #[test]
    fn last_updated_by_txn_id_zero() {
        let stats = TableStatistics {
            table_id: RelationId::new(1),
            row_count: 0,
            total_bytes: 0,
            dead_row_count: 0,
            last_updated_by: Some(TxnId::new(0)),
            column_stats: Vec::new(),
        };
        assert_eq!(stats.last_updated_by, Some(TxnId::new(0)));
    }

    // --- Two stats with identical values are equal ---
    #[test]
    fn identical_stats_are_equal() {
        let a = sample_stats();
        let b = sample_stats();
        assert_eq!(a, b);
    }

    // --- total_bytes = 0 but row_count > 0 is structurally allowed ---
    #[test]
    fn zero_bytes_nonzero_rows() {
        let stats = TableStatistics {
            table_id: RelationId::new(1),
            row_count: 100,
            total_bytes: 0,
            dead_row_count: 0,
            last_updated_by: None,
            column_stats: Vec::new(),
        };
        assert_eq!(stats.total_bytes, 0);
        assert_eq!(stats.row_count, 100);
    }

    // --- Large byte count (petabyte range) ---
    #[test]
    fn large_byte_count() {
        let petabyte: u64 = 1024 * 1024 * 1024 * 1024 * 1024;
        let stats = TableStatistics {
            table_id: RelationId::new(1),
            row_count: 1_000_000_000,
            total_bytes: petabyte,
            dead_row_count: 0,
            last_updated_by: Some(TxnId::new(1)),
            column_stats: Vec::new(),
        };
        assert_eq!(stats.total_bytes, petabyte);
    }
}
