pub(crate) mod overflow;

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use aiondb_core::{
    convert::usize_to_u64_saturating, DbError, DbResult, Row, TupleId, TxnId, Value,
};
use aiondb_storage_api::TableStorageDescriptor;
use aiondb_tx::Snapshot;

use self::overflow::{OverflowStore, StoredRow, StoredValue};

#[derive(Clone, Debug)]
pub(crate) struct TableData {
    pub(crate) descriptor: TableStorageDescriptor,
    heap: BTreeMap<TupleId, HeapTuple>,
    paged_latest_tuple_ids: BTreeMap<TupleId, u64>,
    dead_row_estimate: u64,
    live_row_estimate: u64,
    latest_value_counts: BTreeMap<(usize, CountableValue), u64>,
    pub(crate) next_tuple_id: u64,
    next_heap_position: u64,
    /// `true` while the heap has only insert-mutations: every live
    /// version's `heap_position` is monotonically increasing in
    /// `tuple_id` order, so a `BTreeMap`-keyed iteration is already in
    /// heap-physical order and the per-scan `records_are_heap_ordered`
    /// O(N) check can be skipped. Cleared on the first UPDATE / DELETE
    /// that introduces non-monotonic positions.
    heap_positions_monotonic: bool,
    /// Timestamp of the last read or write access to this table. Used by the
    /// LRU eviction logic to identify cold tables whose committed rows can be
    /// offloaded to the paged store.
    pub(crate) last_accessed: Instant,
    /// Timestamp of the last autovacuum pass on this table.
    last_autovacuum: Option<Instant>,
}

#[derive(Clone, Debug)]
struct HeapTuple {
    versions: Vec<HeapTupleVersion>,
    latest_visible_index: Option<usize>,
}

#[derive(Clone, Debug)]
struct HeapTupleVersion {
    row: StoredRow,
    xmin: TxnId,
    xmax: Option<TxnId>,
    next_version: Option<usize>,
    heap_position: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum CountableValue {
    Int(i32),
    BigInt(i64),
    Text(String),
    Boolean(bool),
}

impl CountableValue {
    fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::Int(value) => Some(Self::Int(*value)),
            Value::BigInt(value) => Some(Self::BigInt(*value)),
            Value::Text(value) => Some(Self::Text(value.clone())),
            Value::Boolean(value) => Some(Self::Boolean(*value)),
            _ => None,
        }
    }

    fn from_stored(value: &StoredValue) -> Option<Self> {
        let StoredValue::Inline(value) = value else {
            return None;
        };
        Self::from_value(value)
    }
}

impl TableData {
    pub(crate) fn new(descriptor: TableStorageDescriptor) -> Self {
        Self {
            descriptor,
            heap: BTreeMap::new(),
            paged_latest_tuple_ids: BTreeMap::new(),
            dead_row_estimate: 0,
            live_row_estimate: 0,
            latest_value_counts: BTreeMap::new(),
            next_tuple_id: 1,
            next_heap_position: 1,
            heap_positions_monotonic: true,
            last_accessed: Instant::now(),
            last_autovacuum: None,
        }
    }

    /// Whether every live version's heap position monotonically tracks
    /// `tuple_id` order. Used by scan paths to skip the O(N)
    /// `records_are_heap_ordered` recheck.
    pub(crate) fn heap_positions_monotonic(&self) -> bool {
        self.heap_positions_monotonic
    }

    #[allow(dead_code)] // exposed for callers that want to pre-emptively flag mutations.
    pub(crate) fn note_heap_position_disorder(&mut self) {
        self.heap_positions_monotonic = false;
    }

    /// Record that this table was just accessed for LRU tracking.
    pub(crate) fn touch(&mut self) {
        self.last_accessed = Instant::now();
    }

    pub(crate) fn autovacuum_due(&self, now: Instant, min_interval: Duration) -> bool {
        self.last_autovacuum.map_or(true, |last| {
            now.saturating_duration_since(last) >= min_interval
        })
    }

    pub(crate) fn note_autovacuum(&mut self, now: Instant) {
        self.last_autovacuum = Some(now);
    }

    /// Number of rows currently held in-memory (as opposed to paged-only).
    pub(crate) fn in_memory_row_count(&self) -> usize {
        self.heap.len()
    }

    /// Best-effort dead-row estimate maintained on write paths.
    /// Used to avoid full heap scans on every autovacuum probe.
    pub(crate) fn dead_row_estimate(&self) -> u64 {
        self.dead_row_estimate
    }

    /// Best-effort live-row estimate maintained on write paths.
    pub(crate) fn live_row_estimate(&self) -> u64 {
        self.live_row_estimate
    }

    pub(crate) fn latest_eq_row_count(&self, ordinal: usize, value: &Value) -> Option<u64> {
        if self.has_paged_tuples() {
            return None;
        }
        let key = CountableValue::from_value(value)?;
        Some(
            self.latest_value_counts
                .get(&(ordinal, key))
                .copied()
                .unwrap_or(0),
        )
    }

    /// Returns `Some(true)` when the count map can prove that NO
    /// visible row of `ordinal` has a value within
    /// `[lower_inclusive, upper_inclusive]` (where either bound may
    /// be `Unbounded` to mean a half-open range). Returns
    /// `Some(false)` when at least one bucket in the range carries
    /// a positive count, or `None` when the count map cannot answer
    /// authoritatively (paged tuples, untrackable column type, …).
    /// Used as a constant-time short-circuit for `WHERE col CMP lit`
    /// and `WHERE col BETWEEN lo AND hi` shapes.
    pub(crate) fn latest_range_is_empty(
        &self,
        ordinal: usize,
        lower: std::ops::Bound<&Value>,
        upper: std::ops::Bound<&Value>,
    ) -> Option<bool> {
        if self.has_paged_tuples() {
            return None;
        }
        // The count map keys on `CountableValue`, which is Ord.
        // Translate the SQL `Bound<&Value>` to a `Bound<CountableValue>`
        // we can hand to `BTreeMap::range`. If either bound contains
        // a `Value` variant the count map can't represent, bail.
        let to_key = |b: std::ops::Bound<&Value>| -> Option<std::ops::Bound<CountableValue>> {
            match b {
                std::ops::Bound::Unbounded => Some(std::ops::Bound::Unbounded),
                std::ops::Bound::Included(v) => {
                    CountableValue::from_value(v).map(std::ops::Bound::Included)
                }
                std::ops::Bound::Excluded(v) => {
                    CountableValue::from_value(v).map(std::ops::Bound::Excluded)
                }
            }
        };
        let low_key = to_key(lower)?;
        let high_key = to_key(upper)?;
        // BTreeMap orders by `(ordinal, CountableValue)`. The value
        // range for `ordinal` lives at keys `(ordinal, low)..=(ordinal,
        // high)`. We must wrap the user's bounds with the ordinal so
        // the iterator stays inside the column's bucket.
        // To stay both fast AND tolerant of Int↔BigInt coercion the
        // eq-filter scan downstream performs, query the count map
        // for each variant separately with a tight BTreeMap range.
        // Each `try_variant` call costs O(log n + matching_entries);
        // the no-match case walks zero entries and returns Some(true)
        // in <1 µs.
        //
        // `Text` / `Boolean` literals only need their own variant.
        // `Int` and `BigInt` literals query both numeric variants
        // because the eq-filter `values_match_eq_filter` accepts
        // both forms.
        let try_variant = |min: CountableValue, max: CountableValue| -> bool {
            // Returns `true` when at least one positive-count entry
            // exists in `[ord:min, ord:max]`. The function
            // intersects the user's `low_key`/`high_key` bounds with
            // the variant's natural [min, max] so a `Bound<Text>`
            // probe never accidentally walks `Int` keys.
            let lo = match &low_key {
                std::ops::Bound::Unbounded => std::ops::Bound::Included((ordinal, min)),
                std::ops::Bound::Included(b)
                    if std::mem::discriminant(b) == std::mem::discriminant(&max) =>
                {
                    std::ops::Bound::Included((ordinal, b.clone()))
                }
                std::ops::Bound::Excluded(b)
                    if std::mem::discriminant(b) == std::mem::discriminant(&max) =>
                {
                    std::ops::Bound::Excluded((ordinal, b.clone()))
                }
                // Cross-type bound: degenerate to the variant's
                // natural minimum so the variant-specific walk
                // still stays correct under coercion.
                _ => std::ops::Bound::Included((ordinal, min)),
            };
            let hi = match &high_key {
                std::ops::Bound::Unbounded => std::ops::Bound::Included((ordinal, max)),
                std::ops::Bound::Included(b)
                    if std::mem::discriminant(b) == std::mem::discriminant(&max) =>
                {
                    std::ops::Bound::Included((ordinal, b.clone()))
                }
                std::ops::Bound::Excluded(b)
                    if std::mem::discriminant(b) == std::mem::discriminant(&max) =>
                {
                    std::ops::Bound::Excluded((ordinal, b.clone()))
                }
                _ => std::ops::Bound::Included((ordinal, max)),
            };
            self.latest_value_counts
                .range((lo, hi))
                .any(|((_, _), count)| *count > 0)
        };

        let any_match = match (&low_key, &high_key) {
            // Numeric literal on either side — query both Int and
            // BigInt buckets. For each absent variant, use that
            // variant's natural min/max as the walk window.
            (
                std::ops::Bound::Included(CountableValue::Int(_) | CountableValue::BigInt(_))
                | std::ops::Bound::Excluded(CountableValue::Int(_) | CountableValue::BigInt(_)),
                _,
            )
            | (
                _,
                std::ops::Bound::Included(CountableValue::Int(_) | CountableValue::BigInt(_))
                | std::ops::Bound::Excluded(CountableValue::Int(_) | CountableValue::BigInt(_)),
            ) => {
                try_variant(CountableValue::Int(i32::MIN), CountableValue::Int(i32::MAX))
                    || try_variant(
                        CountableValue::BigInt(i64::MIN),
                        CountableValue::BigInt(i64::MAX),
                    )
            }
            (
                std::ops::Bound::Included(CountableValue::Boolean(_))
                | std::ops::Bound::Excluded(CountableValue::Boolean(_)),
                _,
            )
            | (
                _,
                std::ops::Bound::Included(CountableValue::Boolean(_))
                | std::ops::Bound::Excluded(CountableValue::Boolean(_)),
            ) => try_variant(
                CountableValue::Boolean(false),
                CountableValue::Boolean(true),
            ),
            (
                std::ops::Bound::Included(CountableValue::Text(_))
                | std::ops::Bound::Excluded(CountableValue::Text(_)),
                _,
            )
            | (
                _,
                std::ops::Bound::Included(CountableValue::Text(_))
                | std::ops::Bound::Excluded(CountableValue::Text(_)),
            ) => try_variant(
                CountableValue::Text(String::new()),
                // `Text` is open-ended, so use a large sentinel —
                // any text orders before this `\u{10FFFF}` repeat.
                CountableValue::Text("\u{10FFFF}".repeat(8)),
            ),
            (std::ops::Bound::Unbounded, std::ops::Bound::Unbounded) => {
                // Pure unbounded range: walk every entry on this
                // ordinal across all variants. Same cost as the
                // generic per-column walk we'd do otherwise.
                try_variant(CountableValue::Int(i32::MIN), CountableValue::Int(i32::MAX))
                    || try_variant(
                        CountableValue::BigInt(i64::MIN),
                        CountableValue::BigInt(i64::MAX),
                    )
                    || try_variant(
                        CountableValue::Text(String::new()),
                        CountableValue::Text("\u{10FFFF}".repeat(8)),
                    )
                    || try_variant(
                        CountableValue::Boolean(false),
                        CountableValue::Boolean(true),
                    )
            }
        };

        if any_match {
            Some(false)
        } else {
            Some(true)
        }
    }

    fn add_latest_value_counts(&mut self, row: &StoredRow) {
        for (ordinal, value) in row.values().iter().enumerate() {
            let Some(key) = CountableValue::from_stored(value) else {
                continue;
            };
            *self.latest_value_counts.entry((ordinal, key)).or_insert(0) += 1;
        }
    }

    fn remove_latest_value_counts(&mut self, row: &StoredRow) {
        for (ordinal, value) in row.values().iter().enumerate() {
            let Some(key) = CountableValue::from_stored(value) else {
                continue;
            };
            let map_key = (ordinal, key);
            let Some(count) = self.latest_value_counts.get_mut(&map_key) else {
                continue;
            };
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.latest_value_counts.remove(&map_key);
            }
        }
    }

    pub(crate) fn allocate_tuple_id(&mut self) -> TupleId {
        let tuple_id = TupleId::new(self.next_tuple_id);
        self.next_tuple_id += 1;
        tuple_id
    }

    pub(crate) fn tuple_ids(&self) -> impl Iterator<Item = TupleId> + '_ {
        self.heap.keys().copied().chain(
            self.paged_latest_tuple_ids
                .keys()
                .copied()
                .filter(|tuple_id| !self.heap.contains_key(tuple_id)),
        )
    }

    pub(crate) fn has_paged_tuples(&self) -> bool {
        !self.paged_latest_tuple_ids.is_empty()
    }

    pub(crate) fn iter_latest_stored_rows(
        &self,
    ) -> impl Iterator<Item = (TupleId, &StoredRow, u64)> + '_ {
        self.heap.iter().filter_map(|(tuple_id, tuple)| {
            let index = tuple.latest_visible_index?;
            let version = tuple.versions.get(index)?;
            Some((*tuple_id, &version.row, version.heap_position))
        })
    }

    pub(crate) fn iter_paged_only_tuple_ids(&self) -> impl Iterator<Item = (TupleId, u64)> + '_ {
        self.paged_latest_tuple_ids
            .iter()
            .filter_map(|(tuple_id, heap_position)| {
                (!self.heap.contains_key(tuple_id)).then_some((*tuple_id, *heap_position))
            })
    }

    pub(crate) fn contains_tuple(&self, tuple_id: TupleId) -> bool {
        self.heap.contains_key(&tuple_id) || self.paged_latest_tuple_ids.contains_key(&tuple_id)
    }

    pub(crate) fn has_live_tuple(&self, tuple_id: TupleId) -> bool {
        self.latest_stored_row(tuple_id).is_some()
            || self.paged_latest_tuple_ids.contains_key(&tuple_id)
    }

    pub(crate) fn is_paged_tuple(&self, tuple_id: TupleId) -> bool {
        self.paged_latest_tuple_ids.contains_key(&tuple_id)
    }

    pub(crate) fn clear_paged_tuple_marker(&mut self, tuple_id: TupleId) -> bool {
        let had_live_before = self.has_live_tuple(tuple_id);
        let removed = self.paged_latest_tuple_ids.remove(&tuple_id).is_some();
        if had_live_before && !self.has_live_tuple(tuple_id) {
            self.live_row_estimate = self.live_row_estimate.saturating_sub(1);
        }
        removed
    }

    pub(crate) fn mark_paged_tuple(&mut self, tuple_id: TupleId) {
        let had_live_before = self.has_live_tuple(tuple_id);
        let heap_position = self.allocate_heap_position();
        self.paged_latest_tuple_ids.insert(tuple_id, heap_position);
        if !had_live_before {
            self.live_row_estimate = self.live_row_estimate.saturating_add(1);
        }
        if tuple_id.get() >= self.next_tuple_id {
            self.next_tuple_id = tuple_id.get() + 1;
        }
    }

    pub(crate) fn next_heap_position(&self) -> u64 {
        self.next_heap_position
    }

    pub(crate) fn latest_heap_position(&self, tuple_id: TupleId) -> Option<u64> {
        self.latest_live_version(tuple_id)
            .map(|version| version.heap_position)
            .or_else(|| self.paged_latest_tuple_ids.get(&tuple_id).copied())
    }

    pub(crate) fn visible_heap_position(
        &self,
        tuple_id: TupleId,
        snapshot: &Snapshot,
    ) -> Option<u64> {
        self.visible_version(tuple_id, snapshot)
            .map(|version| version.heap_position)
            .or_else(|| self.paged_latest_tuple_ids.get(&tuple_id).copied())
    }

    pub(crate) fn load_latest_row(
        &self,
        overflow: &OverflowStore,
        tuple_id: TupleId,
    ) -> DbResult<Option<Row>> {
        self.latest_stored_row(tuple_id)
            .map(|row| overflow.load_row(row))
            .transpose()
    }

    pub(crate) fn load_latest_row_projected(
        &self,
        overflow: &OverflowStore,
        tuple_id: TupleId,
        projection_ordinals: &[usize],
    ) -> DbResult<Option<Row>> {
        self.latest_stored_row(tuple_id)
            .map(|row| overflow.load_row_projected(row, projection_ordinals))
            .transpose()
    }

    pub(crate) fn latest_value_matches_any_filter(
        &self,
        overflow: &OverflowStore,
        tuple_id: TupleId,
        ordinal: usize,
        filter_values: &[Value],
    ) -> DbResult<Option<bool>> {
        self.latest_stored_row(tuple_id)
            .map(|row| {
                row.values()
                    .get(ordinal)
                    .ok_or_else(|| DbError::internal("row is missing projected value"))
                    .and_then(|value| {
                        overflow.stored_value_matches_any_filter(value, filter_values)
                    })
            })
            .transpose()
    }

    pub(crate) fn load_latest_row_for_paging(
        &self,
        overflow: &OverflowStore,
        tuple_id: TupleId,
    ) -> DbResult<Option<(TxnId, Row)>> {
        self.latest_live_version(tuple_id)
            .map(|version| {
                overflow
                    .load_row(&version.row)
                    .map(|row| (version.xmin, row))
            })
            .transpose()
    }

    pub(crate) fn load_visible_row(
        &self,
        overflow: &OverflowStore,
        tuple_id: TupleId,
        snapshot: &Snapshot,
    ) -> DbResult<Option<Row>> {
        self.visible_stored_row(tuple_id, snapshot)
            .map(|row| overflow.load_row(row))
            .transpose()
    }

    pub(crate) fn load_visible_row_projected(
        &self,
        overflow: &OverflowStore,
        tuple_id: TupleId,
        snapshot: &Snapshot,
        projection_ordinals: &[usize],
    ) -> DbResult<Option<Row>> {
        self.visible_stored_row(tuple_id, snapshot)
            .map(|row| overflow.load_row_projected(row, projection_ordinals))
            .transpose()
    }

    pub(crate) fn visible_value_matches_any_filter(
        &self,
        overflow: &OverflowStore,
        tuple_id: TupleId,
        snapshot: &Snapshot,
        ordinal: usize,
        filter_values: &[Value],
    ) -> DbResult<Option<bool>> {
        self.visible_stored_row(tuple_id, snapshot)
            .map(|row| {
                row.values()
                    .get(ordinal)
                    .ok_or_else(|| DbError::internal("row is missing projected value"))
                    .and_then(|value| {
                        overflow.stored_value_matches_any_filter(value, filter_values)
                    })
            })
            .transpose()
    }

    pub(crate) fn commit_insert(&mut self, tuple_id: TupleId, txn: TxnId, row: StoredRow) {
        let had_live_before = self.has_live_tuple(tuple_id);
        self.paged_latest_tuple_ids.remove(&tuple_id);
        let heap_position = self.allocate_heap_position();
        self.add_latest_value_counts(&row);
        self.heap.insert(
            tuple_id,
            HeapTuple {
                versions: vec![HeapTupleVersion {
                    row,
                    xmin: txn,
                    xmax: None,
                    next_version: None,
                    heap_position,
                }],
                latest_visible_index: Some(0),
            },
        );
        if !had_live_before {
            self.live_row_estimate = self.live_row_estimate.saturating_add(1);
        }
    }

    pub(crate) fn commit_update(
        &mut self,
        tuple_id: TupleId,
        txn: TxnId,
        row: StoredRow,
    ) -> DbResult<()> {
        let heap_position = self.allocate_heap_position();
        let old_row = {
            let tuple = self
                .heap
                .get(&tuple_id)
                .ok_or_else(|| DbError::internal("tuple does not exist"))?;
            let current_index = tuple
                .latest_visible_index
                .ok_or_else(|| DbError::internal("tuple does not exist"))?;
            tuple.versions[current_index].row.clone()
        };
        self.remove_latest_value_counts(&old_row);
        self.add_latest_value_counts(&row);

        let tuple = self
            .heap
            .get_mut(&tuple_id)
            .ok_or_else(|| DbError::internal("tuple does not exist"))?;
        let current_index = tuple
            .latest_visible_index
            .ok_or_else(|| DbError::internal("tuple does not exist"))?;
        tuple.versions[current_index].xmax = Some(txn);
        let next_index = tuple.versions.len();
        tuple.versions[current_index].next_version = Some(next_index);
        tuple.versions.push(HeapTupleVersion {
            row,
            xmin: txn,
            xmax: None,
            next_version: None,
            heap_position,
        });
        tuple.latest_visible_index = Some(next_index);
        self.dead_row_estimate = self.dead_row_estimate.saturating_add(1);
        // The new version's heap_position is monotonically larger than
        // any previously allocated position, but its `tuple_id` is
        // older — so iterating by tuple_id no longer matches heap-physical
        // order. Mark the table so future scans don't trust the iter
        // order and skip the O(N) recheck.
        self.heap_positions_monotonic = false;
        Ok(())
    }

    pub(crate) fn commit_delete(&mut self, tuple_id: TupleId, txn: TxnId) -> DbResult<()> {
        let tuple = self
            .heap
            .get_mut(&tuple_id)
            .ok_or_else(|| DbError::internal("tuple does not exist"))?;
        let current_index = tuple
            .latest_visible_index
            .ok_or_else(|| DbError::internal("tuple does not exist"))?;
        let old_row = tuple.versions[current_index].row.clone();
        tuple.versions[current_index].xmax = Some(txn);
        tuple.latest_visible_index = None;
        self.remove_latest_value_counts(&old_row);
        self.dead_row_estimate = self.dead_row_estimate.saturating_add(1);
        self.live_row_estimate = self.live_row_estimate.saturating_sub(1);
        Ok(())
    }

    /// Remove dead tuple versions and fully-dead tuples whose deleting
    /// transaction is below `xmin_horizon` (i.e. visible to all active
    /// snapshots).
    ///
    /// When `xmin_horizon` is `TxnId::default()` (zero), **all** dead
    /// versions are eligible for removal - this is the behaviour used
    /// when no transactions are active.
    ///
    /// Returns the number of dead versions removed.
    pub(crate) fn vacuum(&mut self, overflow: &mut OverflowStore, xmin_horizon: TxnId) -> u64 {
        let (dead_count, released_rows) = self.vacuum_collect_released_rows(xmin_horizon);
        for row in &released_rows {
            overflow.release_row(row);
        }
        dead_count
    }

    /// Vacuum dead versions while retaining rows scheduled for overflow
    /// release. The caller can decide when to release those rows, which allows
    /// deferring physical overflow reclamation until post-vacuum index rebuild
    /// succeeds.
    pub(crate) fn vacuum_collect_released_rows(
        &mut self,
        xmin_horizon: TxnId,
    ) -> (u64, Vec<StoredRow>) {
        let mut dead_count = 0u64;
        let mut released_rows = Vec::new();
        let mut tuples_to_remove = Vec::new();

        for (&tuple_id, tuple) in &mut self.heap {
            let has_live = tuple.versions.iter().any(|v| v.xmax.is_none());

            if has_live {
                // Keep live versions and dead versions still needed by active
                // snapshots; release the rest.
                let mut kept = Vec::new();
                for version in tuple.versions.drain(..) {
                    match version.xmax {
                        None => {
                            // Live version - always keep.
                            kept.push(version);
                        }
                        Some(xmax) if xmax.get() == 0 => {
                            // xmax == 0 means autocommit - the update/delete
                            // is always committed.  However, an active
                            // transaction that started *before* this
                            // autocommit may still need the old version for
                            // snapshot reads.  Only collect when no active
                            // transaction can need it (xmin_horizon == 0
                            // means "no active txns, collect everything").
                            if xmin_horizon.get() == 0 {
                                released_rows.push(version.row.clone());
                                dead_count += 1;
                            } else {
                                kept.push(version);
                            }
                        }
                        Some(xmax)
                            if xmin_horizon.get() > 0 && xmax.get() >= xmin_horizon.get() =>
                        {
                            // The deleting txn is still potentially visible to an
                            // active snapshot - keep this version.
                            kept.push(version);
                        }
                        Some(_) => {
                            released_rows.push(version.row.clone());
                            dead_count += 1;
                        }
                    }
                }
                // Clear next_version pointers since dead versions are gone
                for v in &mut kept {
                    v.next_version = None;
                }
                tuple.versions = kept;
                tuple.latest_visible_index = tuple
                    .versions
                    .iter()
                    .enumerate()
                    .rev()
                    .find_map(|(index, version)| version.xmax.is_none().then_some(index));
            } else {
                // All versions are dead - check if they are all below the
                // horizon before removing.
                let all_below_horizon = tuple.versions.iter().all(|v| {
                    if let Some(xmax) = v.xmax {
                        // xmax == 0 (autocommit) is always below any positive
                        // horizon, so it is naturally included here.
                        xmin_horizon.get() == 0 || xmax.get() < xmin_horizon.get()
                    } else {
                        true
                    }
                });
                if all_below_horizon {
                    for version in &tuple.versions {
                        released_rows.push(version.row.clone());
                        dead_count += 1;
                    }
                    tuples_to_remove.push(tuple_id);
                }
            }
        }

        for tuple_id in tuples_to_remove {
            self.heap.remove(&tuple_id);
        }

        self.dead_row_estimate = self.dead_row_estimate.saturating_sub(dead_count);

        (dead_count, released_rows)
    }

    /// Count the number of live rows (tuples with at least one version
    /// whose `xmax` is `None`).
    pub(crate) fn live_row_count(&self) -> u64 {
        if self.paged_latest_tuple_ids.is_empty() {
            return self.live_row_estimate;
        }
        let in_memory = self
            .heap
            .values()
            .filter(|tuple| tuple.versions.iter().any(|v| v.xmax.is_none()))
            .count()
            .try_into()
            .unwrap_or(u64::MAX);
        let paged_only = self
            .paged_latest_tuple_ids
            .iter()
            .filter(|(tuple_id, _)| !self.heap.contains_key(tuple_id))
            .count()
            .try_into()
            .unwrap_or(u64::MAX);
        in_memory + paged_only
    }

    /// Count rows visible to `snapshot` without materializing row values.
    pub(crate) fn visible_row_count(&self, snapshot: &Snapshot) -> u64 {
        if snapshot_is_latest(snapshot) {
            return self.live_row_estimate;
        }
        let in_memory = self
            .heap
            .values()
            .filter(|tuple| {
                tuple
                    .versions
                    .iter()
                    .rev()
                    .any(|version| version_visible(version, snapshot))
            })
            .count()
            .try_into()
            .unwrap_or(u64::MAX);
        let paged_only = self
            .paged_latest_tuple_ids
            .iter()
            .filter(|(tuple_id, _)| !self.heap.contains_key(tuple_id))
            .count()
            .try_into()
            .unwrap_or(u64::MAX);
        in_memory + paged_only
    }

    /// Count the number of dead row versions (versions whose `xmax` is `Some`).
    pub(crate) fn dead_row_count(&self) -> u64 {
        self.heap
            .values()
            .flat_map(|tuple| &tuple.versions)
            .filter(|v| v.xmax.is_some())
            .count()
            .try_into()
            .unwrap_or(u64::MAX)
    }

    /// Estimate the in-memory byte footprint of this table.
    ///
    /// This is a rough approximation: we count the overhead of each
    /// `HeapTuple` and `HeapTupleVersion`, plus the estimated size of
    /// each inline `StoredValue`. Overflow values are tracked separately
    /// by `OverflowStore::estimated_bytes`.
    pub(crate) fn estimated_bytes(&self) -> u64 {
        // Base overhead: BTreeMap node overhead (~64 bytes per entry) +
        // HeapTuple struct (Vec pointer = 24 bytes) + TupleId key (8 bytes).
        const PER_TUPLE_OVERHEAD: u64 = 96;
        // Per-version overhead: xmin (8) + xmax (16 option) + next_version (16 option) +
        // heap position (8) + StoredRow vec pointer (24).
        const PER_VERSION_OVERHEAD: u64 = 72;
        // Per stored value: enum discriminant + value.
        const PER_INLINE_VALUE_BASE: u64 = 40;

        let mut bytes = 0u64;
        for tuple in self.heap.values() {
            bytes += PER_TUPLE_OVERHEAD;
            for version in &tuple.versions {
                bytes += PER_VERSION_OVERHEAD;
                for sv in version.row.values() {
                    bytes += PER_INLINE_VALUE_BASE;
                    bytes += stored_value_extra_bytes(sv);
                }
            }
        }
        let paged_orphan_count: u64 = self
            .paged_latest_tuple_ids
            .iter()
            .filter(|(tuple_id, _)| !self.heap.contains_key(tuple_id))
            .count()
            .try_into()
            .unwrap_or(u64::MAX);
        bytes = bytes.saturating_add(paged_orphan_count.saturating_mul(16));
        bytes
    }

    pub(crate) fn release_overflow(&self, overflow: &mut OverflowStore) {
        for tuple in self.heap.values() {
            for version in &tuple.versions {
                overflow.release_row(&version.row);
            }
        }
    }

    pub(crate) fn offload_latest_rows_to_paged_store(
        &mut self,
        overflow: &mut OverflowStore,
    ) -> u64 {
        let tuple_ids = self.heap.keys().copied().collect::<Vec<_>>();
        let mut tuples_to_remove = Vec::new();
        let mut offloaded = 0u64;

        for tuple_id in tuple_ids {
            let Some(tuple) = self.heap.get_mut(&tuple_id) else {
                continue;
            };
            let Some(current_index) = tuple.latest_visible_index else {
                // No live version in the heap.  If the tuple was already
                // offloaded to the paged store its latest version lives
                // clean up truly dead tuples that are not paged.
                if !self.paged_latest_tuple_ids.contains_key(&tuple_id) {
                    let removed_dead = u64::try_from(tuple.versions.len()).unwrap_or(u64::MAX);
                    self.dead_row_estimate = self.dead_row_estimate.saturating_sub(removed_dead);
                    for version in &tuple.versions {
                        overflow.release_row(&version.row);
                    }
                    tuples_to_remove.push(tuple_id);
                }
                continue;
            };

            let removed = tuple.versions.remove(current_index);
            overflow.release_row(&removed.row);
            for version in &mut tuple.versions {
                version.next_version = None;
            }

            tuple.latest_visible_index = tuple
                .versions
                .iter()
                .enumerate()
                .rev()
                .find_map(|(index, version)| version.xmax.is_none().then_some(index));

            if tuple.versions.is_empty() {
                tuples_to_remove.push(tuple_id);
            }
            if removed.xmax.is_none() {
                self.paged_latest_tuple_ids
                    .insert(tuple_id, removed.heap_position);
                offloaded += 1;
            }
        }

        for tuple_id in tuples_to_remove {
            self.heap.remove(&tuple_id);
        }

        offloaded
    }

    pub(crate) fn hydrate_paged_latest_row(
        &mut self,
        tuple_id: TupleId,
        xmin: TxnId,
        row: StoredRow,
    ) {
        let had_live_before = self.has_live_tuple(tuple_id);
        let heap_position = self
            .paged_latest_tuple_ids
            .remove(&tuple_id)
            .unwrap_or_else(|| self.allocate_heap_position());
        if tuple_id.get() >= self.next_tuple_id {
            self.next_tuple_id = tuple_id.get() + 1;
        }
        match self.heap.get_mut(&tuple_id) {
            Some(tuple) => {
                for version in &mut tuple.versions {
                    version.next_version = None;
                }
                let new_index = tuple.versions.len();
                tuple.versions.push(HeapTupleVersion {
                    row,
                    xmin,
                    xmax: None,
                    next_version: None,
                    heap_position,
                });
                tuple.latest_visible_index = Some(new_index);
            }
            None => {
                self.heap.insert(
                    tuple_id,
                    HeapTuple {
                        versions: vec![HeapTupleVersion {
                            row,
                            xmin,
                            xmax: None,
                            next_version: None,
                            heap_position,
                        }],
                        latest_visible_index: Some(0),
                    },
                );
            }
        }
        if !had_live_before {
            self.live_row_estimate = self.live_row_estimate.saturating_add(1);
        }
    }

    fn allocate_heap_position(&mut self) -> u64 {
        let heap_position = self.next_heap_position;
        self.next_heap_position = self.next_heap_position.saturating_add(1);
        heap_position
    }

    fn latest_stored_row(&self, tuple_id: TupleId) -> Option<&StoredRow> {
        let tuple = self.heap.get(&tuple_id)?;
        let index = tuple.latest_visible_index?;
        tuple.versions.get(index).map(|version| &version.row)
    }

    fn latest_live_version(&self, tuple_id: TupleId) -> Option<&HeapTupleVersion> {
        let tuple = self.heap.get(&tuple_id)?;
        let index = tuple.latest_visible_index?;
        tuple.versions.get(index)
    }

    fn visible_version(&self, tuple_id: TupleId, snapshot: &Snapshot) -> Option<&HeapTupleVersion> {
        if snapshot_is_latest(snapshot) {
            return self.latest_live_version(tuple_id);
        }
        self.heap
            .get(&tuple_id)?
            .versions
            .iter()
            .rev()
            .find(|version| version_visible(version, snapshot))
    }

    fn visible_stored_row(&self, tuple_id: TupleId, snapshot: &Snapshot) -> Option<&StoredRow> {
        self.visible_version(tuple_id, snapshot)
            .map(|version| &version.row)
    }
}

/// Estimate extra heap bytes for a single `StoredValue` beyond the base
/// enum overhead. For inline values we count the heap-allocated portion
/// (e.g. string content, blob bytes, vector floats). For overflow values
/// we only count the pointer metadata - the pages themselves are tracked
/// by `OverflowStore`.
fn stored_value_extra_bytes(sv: &StoredValue) -> u64 {
    match sv {
        StoredValue::Inline(value) => match value {
            Value::Text(s) => usize_to_u64_saturating(s.len()),
            Value::Blob(b) => usize_to_u64_saturating(b.len()),
            Value::Vector(v) => {
                usize_to_u64_saturating(v.values.len().saturating_mul(std::mem::size_of::<f32>()))
            }
            Value::Array(a) => usize_to_u64_saturating(a.len().saturating_mul(48)), // rough per-element estimate
            Value::Jsonb(_) => 128, // rough estimate for JSON tree
            _ => 0,
        },
        StoredValue::Overflow(_) => 24, // pointer + len + kind
    }
}

pub(crate) fn snapshot_is_latest(snapshot: &Snapshot) -> bool {
    snapshot.xmin == TxnId::default()
        && snapshot.xmax == TxnId::default()
        && snapshot.active.is_empty()
}

fn version_visible(version: &HeapTupleVersion, snapshot: &Snapshot) -> bool {
    txn_visible(version.xmin, snapshot)
        && !version
            .xmax
            .is_some_and(|delete_txn| txn_visible(delete_txn, snapshot))
}

fn txn_visible(txn: TxnId, snapshot: &Snapshot) -> bool {
    if txn == TxnId::default() || snapshot_is_latest(snapshot) {
        return true;
    }
    txn.get() < snapshot.xmax.get() && !snapshot.active.contains(&txn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_core::{ColumnId, DataType, RelationId, Row, Value};
    use aiondb_storage_api::StorageColumn;

    fn descriptor() -> TableStorageDescriptor {
        TableStorageDescriptor {
            table_id: RelationId::new(1),
            columns: vec![
                StorageColumn {
                    column_id: ColumnId::new(1),
                    data_type: DataType::Int,
                    nullable: false,
                },
                StorageColumn {
                    column_id: ColumnId::new(2),
                    data_type: DataType::Text,
                    nullable: false,
                },
            ],
            primary_key: None,
            shard_config: None,
        }
    }

    fn stored(row: Row) -> StoredRow {
        let mut overflow = OverflowStore::default();
        overflow.store_row(&row)
    }

    #[test]
    fn latest_counts_track_insert_update_delete() {
        let mut table = TableData::new(descriptor());
        table.commit_insert(
            TupleId::new(1),
            TxnId::new(1),
            stored(Row::new(vec![
                Value::Int(1),
                Value::Text("open".to_owned()),
            ])),
        );
        table.commit_insert(
            TupleId::new(2),
            TxnId::new(1),
            stored(Row::new(vec![
                Value::Int(2),
                Value::Text("open".to_owned()),
            ])),
        );
        assert_eq!(table.live_row_count(), 2);
        assert_eq!(
            table.latest_eq_row_count(1, &Value::Text("open".to_owned())),
            Some(2)
        );

        table
            .commit_update(
                TupleId::new(2),
                TxnId::new(2),
                stored(Row::new(vec![
                    Value::Int(2),
                    Value::Text("done".to_owned()),
                ])),
            )
            .unwrap();
        assert_eq!(table.live_row_count(), 2);
        assert_eq!(
            table.latest_eq_row_count(1, &Value::Text("open".to_owned())),
            Some(1)
        );
        assert_eq!(
            table.latest_eq_row_count(1, &Value::Text("done".to_owned())),
            Some(1)
        );

        table.commit_delete(TupleId::new(1), TxnId::new(3)).unwrap();
        assert_eq!(table.live_row_count(), 1);
        assert_eq!(
            table.latest_eq_row_count(1, &Value::Text("open".to_owned())),
            Some(0)
        );
        assert_eq!(
            table.latest_eq_row_count(1, &Value::Text("done".to_owned())),
            Some(1)
        );
    }
}
