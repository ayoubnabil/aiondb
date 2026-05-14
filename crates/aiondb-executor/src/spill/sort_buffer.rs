//! Spillable sort buffer.
//!
//! Collects rows in memory up to a configurable budget.  When the
//! budget is exceeded, the current batch is sorted and flushed to a
//! temporary file as a *run*.  On [`SortBuffer::finish`], all runs are
//! merged via a k-way merge to produce a single sorted stream.
//!
//! When no spill occurs the fast-path returns the in-memory sort
//! directly - zero disk I/O.

use std::cmp::Ordering;
use std::path::PathBuf;

use aiondb_core::{DbError, DbResult, Row, Value};
use aiondb_plan::SortExpr;
use serde::{Deserialize, Serialize};

use super::spill_file::{resolve_spill_dir, SpillReader, SpillWriter};
use crate::context::ExecutionContext;

/// Pre-computed sort key attached to a row.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct KeyedRow {
    pub(crate) row: Row,
    pub(crate) sort_keys: Vec<Value>,
}

/// Default spill threshold: 75% of `max_memory_bytes`.
///
/// We leave headroom for the merge phase and other allocations.
fn spill_threshold(context: &ExecutionContext) -> u64 {
    context.max_memory_bytes.saturating_mul(3) / 4
}

// -----------------------------------------------------------------------
// SortBuffer
// -----------------------------------------------------------------------

pub(crate) struct SortBuffer {
    /// Rows accumulated in the current in-memory batch.
    batch: Vec<KeyedRow>,
    /// Approximate bytes used by the current batch.
    batch_bytes: u64,
    /// Sorted runs that have been spilled to disk.
    runs: Vec<SpillReader<KeyedRow>>,
    /// Total bytes spilled to disk (tracked against `max_temp_bytes`).
    spilled_bytes: u64,
    /// Memory threshold that triggers a spill.
    threshold: u64,
    /// Directory for spill files.
    spill_dir: PathBuf,
    /// Sort specification (needed to sort runs before spilling).
    order_by: Vec<SortExpr>,
}

impl SortBuffer {
    /// Create a new sort buffer.
    ///
    /// * `order_by` - sort specification used to order each run.
    /// * `context`  - execution context for memory budget and temp dir.
    pub(crate) fn new(order_by: &[SortExpr], context: &ExecutionContext) -> DbResult<Self> {
        let spill_dir = resolve_spill_dir(context.server_data_dir.as_deref())?;
        Ok(Self {
            batch: Vec::new(),
            batch_bytes: 0,
            runs: Vec::new(),
            spilled_bytes: 0,
            threshold: spill_threshold(context),
            spill_dir,
            order_by: order_by.to_vec(),
        })
    }

    /// Push a row with pre-computed sort keys into the buffer.
    ///
    /// If the in-memory budget is exceeded, the current batch is sorted
    /// and flushed to a spill file.
    pub(crate) fn push(
        &mut self,
        row: Row,
        sort_keys: Vec<Value>,
        context: &ExecutionContext,
    ) -> DbResult<()> {
        let row_bytes = crate::executor::helpers::estimate_row_bytes(&row);
        self.batch_bytes += row_bytes;
        self.batch.push(KeyedRow { row, sort_keys });

        if self.batch_bytes >= self.threshold && !self.batch.is_empty() {
            self.flush_batch(context)?;
        }
        Ok(())
    }

    /// Sort the current in-memory batch and spill it to a temp file.
    fn flush_batch(&mut self, context: &ExecutionContext) -> DbResult<()> {
        context.check_deadline()?;
        sort_batch_in_memory(&mut self.batch, &self.order_by, context)?;

        // Write sorted rows to a spill file.
        let mut writer: SpillWriter<KeyedRow> = SpillWriter::new(&self.spill_dir)?;
        for keyed in self.batch.drain(..) {
            writer.write_item(&keyed)?;
        }
        let bytes = writer.byte_count();
        self.spilled_bytes += bytes;

        // Check temp budget.
        if self.spilled_bytes > context.max_temp_bytes {
            return Err(DbError::program_limit(
                "maximum temporary disk space exceeded during sort spill",
            ));
        }
        context
            .temp_used
            .fetch_add(bytes, std::sync::atomic::Ordering::Relaxed);

        let reader = writer.into_reader()?;
        self.runs.push(reader);
        self.batch_bytes = 0;
        Ok(())
    }

    /// Consume the buffer and return all rows in sorted order.
    ///
    /// **Fast path**: if no runs were spilled, sorts the in-memory
    /// batch and returns it directly.
    ///
    /// **Spill path**: flushes the remaining in-memory rows as the
    /// final run, then performs a k-way merge across all runs.
    pub(crate) fn finish(mut self, context: &ExecutionContext) -> DbResult<Vec<KeyedRow>> {
        if self.runs.is_empty() {
            // Fast path - everything fits in memory.
            sort_batch_in_memory(&mut self.batch, &self.order_by, context)?;
            return Ok(self.batch);
        }

        // Flush the last in-memory batch as a run.
        if !self.batch.is_empty() {
            self.flush_batch(context)?;
        }

        // K-way merge all runs.
        self.merge_runs(context)
    }

    /// K-way merge of sorted spill runs using a binary min-heap.
    ///
    /// Each heap entry holds the current head row of one run together
    /// with the run index.  Extracting the minimum is O(log K) instead
    /// of the O(K) linear scan used previously.
    fn merge_runs(&mut self, context: &ExecutionContext) -> DbResult<Vec<KeyedRow>> {
        // Seed the heap with the first row from each run.
        let mut heap: Vec<(KeyedRow, usize)> = Vec::with_capacity(self.runs.len());
        for (idx, run) in self.runs.iter_mut().enumerate() {
            if let Some(row) = run.next_item()? {
                heap.push((row, idx));
            }
        }

        // Build the min-heap (Floyd's algorithm).
        let order_by = &self.order_by;
        for i in (0..heap.len() / 2).rev() {
            heap_sift_down(&mut heap, i, order_by)?;
        }

        let mut result = Vec::new();
        while !heap.is_empty() {
            context.check_deadline()?;

            // The minimum element is at index 0.
            let (row, run_idx) = if let Some(next) = self.runs[heap[0].1].next_item()? {
                // Replace root with next row from the same run, then sift down.
                let run_idx_0 = heap[0].1;
                let min = std::mem::replace(&mut heap[0], (next, run_idx_0));
                heap_sift_down(&mut heap, 0, order_by)?;
                min
            } else {
                // Run exhausted - swap root with last element and shrink.
                let last = heap.len() - 1;
                heap.swap(0, last);
                let Some(min) = heap.pop() else {
                    break;
                };
                if !heap.is_empty() {
                    heap_sift_down(&mut heap, 0, order_by)?;
                }
                min
            };
            let _ = run_idx;
            result.push(row);
        }

        Ok(result)
    }
}

// -----------------------------------------------------------------------
// Min-heap helpers for K-way merge
// -----------------------------------------------------------------------

/// Sift element at `pos` down the min-heap until the heap property is
/// restored.  Comparison uses the sort specification and is fallible.
fn heap_sift_down(
    heap: &mut [(KeyedRow, usize)],
    mut pos: usize,
    order_by: &[SortExpr],
) -> DbResult<()> {
    let len = heap.len();
    loop {
        let left = 2 * pos + 1;
        if left >= len {
            break;
        }
        let right = left + 1;
        // Find the smaller child.
        let mut smallest = left;
        if right < len
            && compare_keyed_rows(&heap[right].0, &heap[left].0, order_by)? == Ordering::Less
        {
            smallest = right;
        }
        // If parent is already ≤ smallest child, stop.
        if compare_keyed_rows(&heap[pos].0, &heap[smallest].0, order_by)? != Ordering::Greater {
            break;
        }
        heap.swap(pos, smallest);
        pos = smallest;
    }
    Ok(())
}

// -----------------------------------------------------------------------
// Comparison helpers
// -----------------------------------------------------------------------

fn compare_keyed_rows(a: &KeyedRow, b: &KeyedRow, order_by: &[SortExpr]) -> DbResult<Ordering> {
    for (i, sort) in order_by.iter().enumerate() {
        let left = a.sort_keys.get(i).unwrap_or(&Value::Null);
        let right = b.sort_keys.get(i).unwrap_or(&Value::Null);
        let ord = crate::executor::helpers::compare_sort_values(
            left,
            right,
            sort.descending,
            sort.nulls_first,
        )?;
        if ord != Ordering::Equal {
            return Ok(ord);
        }
    }
    Ok(Ordering::Equal)
}

fn sort_batch_in_memory(
    batch: &mut [KeyedRow],
    order_by: &[SortExpr],
    context: &ExecutionContext,
) -> DbResult<()> {
    let failed = std::cell::Cell::new(false);
    let error: std::cell::RefCell<Option<DbError>> = std::cell::RefCell::new(None);
    batch.sort_by(|a, b| {
        if failed.get() {
            return Ordering::Equal;
        }
        if let Err(e) = context.check_deadline() {
            failed.set(true);
            *error.borrow_mut() = Some(e);
            return Ordering::Equal;
        }
        match compare_keyed_rows(a, b, order_by) {
            Ok(ordering) => ordering,
            Err(e) => {
                failed.set(true);
                *error.borrow_mut() = Some(e);
                Ordering::Equal
            }
        }
    });
    match error.into_inner() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

// -----------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_core::{DataType, Value};
    use aiondb_plan::{SortExpr, TypedExpr};

    fn asc_sort(ordinal: usize) -> SortExpr {
        SortExpr {
            expr: TypedExpr::column_ref("col", ordinal, DataType::Int, false),
            descending: false,
            nulls_first: None,
        }
    }

    fn desc_sort(ordinal: usize) -> SortExpr {
        SortExpr {
            expr: TypedExpr::column_ref("col", ordinal, DataType::Int, false),
            descending: true,
            nulls_first: None,
        }
    }

    fn make_context_with_threshold(max_mem: u64) -> ExecutionContext {
        let mut ctx = ExecutionContext::default();
        ctx.max_memory_bytes = max_mem;
        ctx.max_temp_bytes = u64::MAX;
        ctx
    }

    #[test]
    fn fast_path_no_spill() {
        let ctx = make_context_with_threshold(1_000_000);
        let order_by = vec![asc_sort(0)];
        let mut buf = SortBuffer::new(&order_by, &ctx).unwrap();

        buf.push(Row::new(vec![Value::Int(3)]), vec![Value::Int(3)], &ctx)
            .unwrap();
        buf.push(Row::new(vec![Value::Int(1)]), vec![Value::Int(1)], &ctx)
            .unwrap();
        buf.push(Row::new(vec![Value::Int(2)]), vec![Value::Int(2)], &ctx)
            .unwrap();

        assert!(buf.runs.is_empty());
        let rows = buf.finish(&ctx).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].row.values[0], Value::Int(1));
        assert_eq!(rows[1].row.values[0], Value::Int(2));
        assert_eq!(rows[2].row.values[0], Value::Int(3));
    }

    #[test]
    fn spill_path_sorts_correctly() {
        // Very small threshold to force spilling.
        let ctx = make_context_with_threshold(50);
        let order_by = vec![asc_sort(0)];
        let mut buf = SortBuffer::new(&order_by, &ctx).unwrap();

        // Insert enough rows to force multiple spills.
        for i in (0..100).rev() {
            buf.push(Row::new(vec![Value::Int(i)]), vec![Value::Int(i)], &ctx)
                .unwrap();
        }

        assert!(!buf.runs.is_empty());
        let rows = buf.finish(&ctx).unwrap();
        assert_eq!(rows.len(), 100);
        for (i, row) in rows.iter().enumerate() {
            assert_eq!(
                row.row.values[0],
                Value::Int(i as i32),
                "row {i} out of order"
            );
        }
    }

    #[test]
    fn descending_sort_with_spill() {
        let ctx = make_context_with_threshold(50);
        let order_by = vec![desc_sort(0)];
        let mut buf = SortBuffer::new(&order_by, &ctx).unwrap();

        for i in 0..50 {
            buf.push(Row::new(vec![Value::Int(i)]), vec![Value::Int(i)], &ctx)
                .unwrap();
        }

        let rows = buf.finish(&ctx).unwrap();
        assert_eq!(rows.len(), 50);
        assert_eq!(rows[0].row.values[0], Value::Int(49));
        assert_eq!(rows[49].row.values[0], Value::Int(0));
    }

    #[test]
    fn empty_buffer_returns_empty() {
        let ctx = make_context_with_threshold(1_000_000);
        let order_by = vec![asc_sort(0)];
        let buf = SortBuffer::new(&order_by, &ctx).unwrap();
        let rows = buf.finish(&ctx).unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn single_row_no_spill() {
        let ctx = make_context_with_threshold(1_000_000);
        let order_by = vec![asc_sort(0)];
        let mut buf = SortBuffer::new(&order_by, &ctx).unwrap();
        buf.push(Row::new(vec![Value::Int(7)]), vec![Value::Int(7)], &ctx)
            .unwrap();
        let rows = buf.finish(&ctx).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].row.values[0], Value::Int(7));
    }

    #[test]
    fn null_handling_in_sort() {
        let ctx = make_context_with_threshold(1_000_000);
        let order_by = vec![asc_sort(0)];
        let mut buf = SortBuffer::new(&order_by, &ctx).unwrap();

        buf.push(Row::new(vec![Value::Int(2)]), vec![Value::Int(2)], &ctx)
            .unwrap();
        buf.push(Row::new(vec![Value::Null]), vec![Value::Null], &ctx)
            .unwrap();
        buf.push(Row::new(vec![Value::Int(1)]), vec![Value::Int(1)], &ctx)
            .unwrap();

        let rows = buf.finish(&ctx).unwrap();
        assert_eq!(rows.len(), 3);
        // Default nulls_first=false for ASC → nulls last
        assert_eq!(rows[0].row.values[0], Value::Int(1));
        assert_eq!(rows[1].row.values[0], Value::Int(2));
        assert_eq!(rows[2].row.values[0], Value::Null);
    }

    #[test]
    fn spill_merge_uses_precomputed_sort_keys() {
        let ctx = make_context_with_threshold(50);
        let order_by = vec![asc_sort(0)];
        let mut buf = SortBuffer::new(&order_by, &ctx).unwrap();

        // Row values are intentionally out-of-order relative to sort keys.
        // This guards against regressions where merge compares row columns
        // instead of the precomputed expression keys.
        buf.push(Row::new(vec![Value::Int(100)]), vec![Value::Int(2)], &ctx)
            .unwrap();
        buf.push(Row::new(vec![Value::Int(300)]), vec![Value::Int(3)], &ctx)
            .unwrap();
        buf.push(Row::new(vec![Value::Int(200)]), vec![Value::Int(1)], &ctx)
            .unwrap();

        let rows = buf.finish(&ctx).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].sort_keys[0], Value::Int(1));
        assert_eq!(rows[1].sort_keys[0], Value::Int(2));
        assert_eq!(rows[2].sort_keys[0], Value::Int(3));
        assert_eq!(rows[0].row.values[0], Value::Int(200));
        assert_eq!(rows[1].row.values[0], Value::Int(100));
        assert_eq!(rows[2].row.values[0], Value::Int(300));
    }
}
