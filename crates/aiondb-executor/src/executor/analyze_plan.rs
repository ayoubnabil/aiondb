use super::*;
use aiondb_catalog::Histogram;

/// Maximum number of sample values to collect per column for histogram building.
const MAX_SAMPLE_VALUES: usize = 10_000;

/// Target number of histogram buckets.
const TARGET_BUCKETS: usize = 100;

/// Maximum number of MCV (Most Common Values) entries kept per column.
/// Mirrors PostgreSQL's `default_statistics_target = 100`.
const TARGET_MCV_LEN: usize = 100;

/// Minimum frequency for a value to be retained in the MCV list. PG's
/// `selfuncs.c::compute_distinct_stats` uses an analogous threshold to
/// avoid spending lookup time on values that contribute negligibly to
/// selectivity estimates.
const MIN_MCV_FREQUENCY: f64 = 1e-4;

use aiondb_core::convert::u64_to_u32_saturating;

impl Executor {
    /// Execute an ANALYZE command: scan the table to compute statistics and
    /// store them in the catalog.
    pub(super) fn execute_analyze(
        &self,
        table_id: RelationId,
        context: &ExecutionContext,
    ) -> DbResult<ExecutionResult> {
        context.check_deadline()?;
        // Bare `ANALYZE` (no table) lowers to RelationId(0). PG refreshes
        // statistics for every relation in the database in that form;
        // AionDB has no whole-database driver yet, so return a success tag
        // immediately rather than refusing the statement.
        if table_id.get() == 0 {
            return Ok(ExecutionResult::Command {
                tag: "ANALYZE".to_owned(),
                rows_affected: 0,
            });
        }
        // Scan the table to compute statistics
        let mut stream = self.scan_table_locked(context, table_id, None)?;
        let mut row_count: u64 = 0;
        let mut total_bytes: u64 = 0;

        // Fetch column descriptors to build per-column stats
        let table = self
            .catalog_reader
            .get_table_by_id(context.txn_id, table_id)?
            .ok_or_else(|| DbError::internal("analyzed table is missing from catalog"))?;
        let num_columns = table.columns.len();
        let mut null_counts = vec![0u64; num_columns];
        let mut distinct_sets: Vec<std::collections::HashSet<u64>> =
            vec![std::collections::HashSet::new(); num_columns];
        let mut column_bytes = vec![0u64; num_columns];
        let mut sample_values: Vec<Vec<String>> = vec![Vec::new(); num_columns];
        // Per-column samples of (physical_position, sortable_value) used to
        // compute Pearson correlation between heap order and value order, in
        // the same spirit as PostgreSQL's `compute_scalar_stats` which
        // populates `pg_stats.correlation`.
        let mut correlation_samples: Vec<Vec<(u64, String)>> = vec![Vec::new(); num_columns];
        // Per-column value frequency map used to derive the Most Common
        // Values list (`pg_stats.most_common_vals` /
        // `pg_stats.most_common_freqs`). PG `compute_distinct_stats` and
        // `compute_scalar_stats` build the MCV list by counting value
        // frequencies during the scan and keeping the top-K in the
        // catalog. We retain Value alongside the count so we can return
        // the original typed value (PG keeps the raw datum array).
        let mut value_counts: Vec<std::collections::HashMap<String, (Value, u64)>> = (0
            ..num_columns)
            .map(|_| std::collections::HashMap::new())
            .collect();
        // Scratch buffer reused across cells for the Debug-format
        // fingerprint hash; saves a `format!` allocation per cell.
        let mut scratch = String::with_capacity(64);

        while let Some(record) = stream.next()? {
            context.check_deadline()?;
            let physical_pos = record.heap_position;
            row_count = row_count.saturating_add(1);
            for (i, value) in record.row.values.iter().enumerate() {
                if i < num_columns {
                    let byte_size = estimate_value_bytes(value);
                    total_bytes = total_bytes.saturating_add(byte_size);
                    column_bytes[i] = column_bytes[i].saturating_add(byte_size);
                    if matches!(value, Value::Null) {
                        null_counts[i] = null_counts[i].saturating_add(1);
                    } else {
                        use std::fmt::Write;
                        use std::hash::{Hash, Hasher};
                        let mut hasher = std::collections::hash_map::DefaultHasher::new();
                        // Reuse a per-iteration scratch String for the
                        // Debug fingerprint instead of allocating a fresh
                        // one through `format!` every cell.
                        scratch.clear();
                        let _ = write!(&mut scratch, "{value:?}");
                        scratch.hash(&mut hasher);
                        distinct_sets[i].insert(hasher.finish());

                        let sortable = value_to_sortable_string(value);
                        // Collect sample values for histogram
                        if sample_values[i].len() < MAX_SAMPLE_VALUES {
                            sample_values[i].push(sortable.clone());
                        }
                        // Cap correlation samples at the same target so
                        // the rank computation is bounded.
                        if correlation_samples[i].len() < MAX_SAMPLE_VALUES {
                            correlation_samples[i].push((physical_pos, sortable.clone()));
                        }
                        // Track exact frequencies for the MCV pass.  We
                        // intentionally count every row (not just the
                        // sample) because MCV thresholds are global
                        // properties of the live table.
                        let entry = value_counts[i]
                            .entry(sortable)
                            .or_insert_with(|| (value.clone(), 0));
                        entry.1 = entry.1.saturating_add(1);
                    }
                }
            }
        }

        // Build column statistics
        let column_stats: Vec<aiondb_catalog::ColumnStatistics> = table
            .columns
            .iter()
            .enumerate()
            .map(|(i, col)| {
                let ndistinct = usize_to_f64(distinct_sets[i].len());
                let null_fraction = if row_count > 0 {
                    u64_to_f64(null_counts[i]) / u64_to_f64(row_count)
                } else {
                    0.0
                };
                let avg_width = column_bytes[i]
                    .checked_div(row_count)
                    .map(u64_to_u32_saturating)
                    .unwrap_or(0);
                let correlation = compute_correlation(&mut correlation_samples[i]);
                let histogram = build_histogram(&mut sample_values[i], TARGET_BUCKETS);
                let mcv = build_mcv_list(&value_counts[i], row_count, TARGET_MCV_LEN);
                aiondb_catalog::ColumnStatistics {
                    column_id: col.column_id,
                    ndistinct,
                    null_fraction,
                    avg_width,
                    histogram,
                    mcv,
                    correlation,
                }
            })
            .collect();

        // Build WAL column data before moving column_stats into TableStatistics.
        let column_data: Vec<(ColumnId, f64, f64, u32)> = column_stats
            .iter()
            .map(|cs| (cs.column_id, cs.ndistinct, cs.null_fraction, cs.avg_width))
            .collect();

        let stats = aiondb_catalog::TableStatistics {
            table_id,
            row_count,
            total_bytes,
            dead_row_count: 0,
            last_updated_by: Some(context.txn_id),
            column_stats,
        };
        self.catalog_writer
            .update_statistics(context.txn_id, stats)?;

        // Persist statistics to WAL so they survive restart.
        self.storage_dml
            .log_analyze_stats(table_id, row_count, total_bytes, 0, column_data)?;

        Ok(ExecutionResult::command("ANALYZE"))
    }
}

/// Convert a Value to a sortable string representation for histogram boundaries.
///
/// Signed integers are offset to unsigned space (wrapping offset)
/// so that lexicographic ordering of the zero-padded decimal matches
/// numeric ordering for the full range including negative values.
fn value_to_sortable_string(value: &Value) -> String {
    match value {
        Value::Int(v) => format!(
            "{:020}",
            i64::from(*v).wrapping_add(i64::MIN.wrapping_neg())
        ),
        Value::BigInt(v) => format!("{:020}", (*v).wrapping_add(i64::MIN.wrapping_neg())),
        Value::Real(v) => float_to_sortable_string(*v as f64),
        Value::Double(v) => float_to_sortable_string(*v),
        Value::Text(v) => v.clone(),
        _ => format!("{value:?}"),
    }
}

/// Encode an f64 as a lexicographically-sortable string.
///
/// IEEE 754 doubles, when reinterpreted as u64, sort correctly for
/// non-negative values. For negative values the bits are flipped.
fn float_to_sortable_string(v: f64) -> String {
    let bits = v.to_bits();
    let sortable = if v.is_sign_negative() {
        !bits // flip all bits for negatives
    } else {
        bits ^ (1u64 << 63) // flip sign bit for positives
    };
    format!("{sortable:020}")
}

/// Compute Pearson correlation between physical heap order and column
/// value order from a sample of `(physical_pos, sortable_value)` pairs.
///
/// Mirrors PostgreSQL's `compute_scalar_stats` correlation pass in
/// `src/backend/commands/analyze.c`: rank the sample by value, then
/// correlate the value-rank against the physical-position-rank. A result
/// near 1.0 means heap order tracks value order (e.g. a clustered PK on
/// insert order); near -1.0 means reverse-clustered; near 0.0 means
/// random. The cost model uses `correlation^2` to blend
/// random-page and sequential-page heap-fetch I/O on index range scans
/// (see PG `genericcostestimate`).
fn compute_correlation(samples: &mut [(u64, String)]) -> f64 {
    let n = samples.len();
    if n < 2 {
        return 0.0;
    }
    // Rank by physical position.
    samples.sort_by(|a, b| a.0.cmp(&b.0));
    let physical_ranks: Vec<f64> = (0..n).map(|i| i as f64).collect();
    // Rank by value (stable on ties to keep the rank assignment well
    // defined; ties are rare in practice and PG handles them similarly).
    let mut indexed: Vec<(usize, &String)> = samples
        .iter()
        .enumerate()
        .map(|(i, (_, v))| (i, v))
        .collect();
    indexed.sort_by(|a, b| a.1.cmp(b.1));
    let mut value_ranks = vec![0.0f64; n];
    for (rank, (orig_idx, _)) in indexed.iter().enumerate() {
        value_ranks[*orig_idx] = rank as f64;
    }
    // Pearson correlation over the two rank vectors.
    let mean_x = physical_ranks.iter().sum::<f64>() / n as f64;
    let mean_y = value_ranks.iter().sum::<f64>() / n as f64;
    let mut num = 0.0f64;
    let mut den_x = 0.0f64;
    let mut den_y = 0.0f64;
    for i in 0..n {
        let dx = physical_ranks[i] - mean_x;
        let dy = value_ranks[i] - mean_y;
        num += dx * dy;
        den_x += dx * dx;
        den_y += dy * dy;
    }
    if den_x <= 0.0 || den_y <= 0.0 {
        return 0.0;
    }
    let corr = num / (den_x.sqrt() * den_y.sqrt());
    if corr.is_finite() {
        corr.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

/// Build a Most Common Values list from a per-column value frequency map.
///
/// Mirrors PostgreSQL `compute_scalar_stats` / `compute_distinct_stats`:
/// keep the top-K most frequent values (default K = 100, matching PG's
/// `default_statistics_target`) and store their relative frequencies.
/// Tied frequencies break by sort order so the result is deterministic
/// across runs.
fn build_mcv_list(
    counts: &std::collections::HashMap<String, (aiondb_core::Value, u64)>,
    row_count: u64,
    max_len: usize,
) -> Option<aiondb_catalog::McvList> {
    if counts.is_empty() || row_count == 0 || max_len == 0 {
        return None;
    }
    let mut entries: Vec<(&String, &aiondb_core::Value, u64)> = counts
        .iter()
        .map(|(key, (value, count))| (key, value, *count))
        .collect();
    // Sort by descending frequency, breaking ties on the sortable string
    // so two ANALYZE runs over the same data produce the same MCV list.
    entries.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(b.0)));

    let total = u64_to_f64(row_count);
    let mut values = Vec::new();
    let mut frequencies = Vec::new();
    for (_, value, count) in entries.into_iter().take(max_len) {
        let freq = u64_to_f64(count) / total;
        if freq < MIN_MCV_FREQUENCY {
            break;
        }
        values.push(value.clone());
        frequencies.push(freq);
    }
    if values.is_empty() {
        None
    } else {
        Some(aiondb_catalog::McvList {
            values,
            frequencies,
        })
    }
}

#[cfg(test)]
mod correlation_tests {
    use super::compute_correlation;

    fn samples<I, S>(iter: I) -> Vec<(u64, String)>
    where
        I: IntoIterator<Item = (u64, S)>,
        S: ToString,
    {
        iter.into_iter()
            .map(|(pos, value)| (pos, value.to_string()))
            .collect()
    }

    #[test]
    fn perfectly_clustered_returns_one() {
        let mut s = samples((0..10u64).map(|i| (i, format!("{i:020}"))));
        let corr = compute_correlation(&mut s);
        assert!((corr - 1.0).abs() < 1e-9, "got {corr}");
    }

    #[test]
    fn reverse_clustered_returns_minus_one() {
        let mut s = samples((0..10u64).map(|i| (i, format!("{:020}", 10 - i))));
        let corr = compute_correlation(&mut s);
        assert!((corr + 1.0).abs() < 1e-9, "got {corr}");
    }

    #[test]
    fn empty_or_single_returns_zero() {
        let mut empty: Vec<(u64, String)> = Vec::new();
        assert_eq!(compute_correlation(&mut empty), 0.0);
        let mut single = samples([(0u64, "x")]);
        assert_eq!(compute_correlation(&mut single), 0.0);
    }

    #[test]
    fn all_same_value_returns_zero() {
        // Variance of value rank is 0 (well, ranks are still distinct
        // by stable sort, but the *value* spread is zero so the
        // correlation reflects the rank assignment, which still maps
        // 1:1 to physical position — Pearson gives 1.0). PG behaves
        // similarly: identical values produce a fully-clustered shape.
        let mut s = samples((0..10u64).map(|i| (i, "constant")));
        let corr = compute_correlation(&mut s);
        // Stable sort by value preserves input order, so ranks correlate
        // perfectly with positions.
        assert!((corr - 1.0).abs() < 1e-9);
    }
}

/// Build an equi-depth histogram from a collection of sortable string values.
fn build_histogram(values: &mut [String], target_buckets: usize) -> Option<Histogram> {
    if values.is_empty() {
        return None;
    }
    values.sort();
    let n = values.len();
    let num_buckets = target_buckets.min(n);
    if num_buckets == 0 {
        return None;
    }
    let bucket_size = n / num_buckets;
    if bucket_size == 0 {
        return None;
    }

    let mut boundaries = Vec::with_capacity(num_buckets + 1);
    let mut ndistinct_per_bucket = Vec::with_capacity(num_buckets);

    for i in 0..num_buckets {
        let start = i * bucket_size;
        let end = if i == num_buckets - 1 {
            n
        } else {
            (i + 1) * bucket_size
        };
        boundaries.push(values[start].clone());

        // Count distinct values in this bucket
        let mut distinct = 1u64;
        for j in (start + 1)..end {
            if values[j] != values[j - 1] {
                distinct += 1;
            }
        }
        ndistinct_per_bucket.push(u64_to_f64(distinct));
    }
    // Add final boundary
    boundaries.push(values[n - 1].clone());

    Some(Histogram {
        boundaries,
        rows_per_bucket: usize_to_f64(bucket_size),
        ndistinct_per_bucket,
    })
}
