use std::cmp::Ordering;

use aiondb_core::{
    convert::usize_to_u64_saturating, ColumnId, DbError, DbResult, Row, SqlState, TupleId, Value,
};
use aiondb_storage_api::{
    Bound, IndexKeyColumn, IndexStorageDescriptor, KeyRange, TableStorageDescriptor, TupleRecord,
};

#[derive(Clone, Debug)]
pub(crate) struct IndexData {
    pub(crate) descriptor: IndexStorageDescriptor,
    covering_column_ids: Vec<ColumnId>,
    leaves: Vec<LeafPage>,
    leaf_page_capacity: usize,
}

// Current ordered-index implementation.
//
// This is deliberately an in-memory leaf-page structure. Its descriptor is
// durable through WAL/snapshot records and the structure is rebuilt from table
// rows during recovery; the leaf pages themselves are not a disk-resident,
// buffer-managed PostgreSQL-style B-tree yet.
#[derive(Clone, Debug)]
struct LeafPage {
    entries: Vec<LeafEntry>,
    tuple_id_count: usize,
}

impl LeafPage {
    #[inline]
    fn with_entries(entries: Vec<LeafEntry>) -> Self {
        let tuple_id_count = entries.iter().map(|entry| entry.tuple_ids.len()).sum();
        Self {
            entries,
            tuple_id_count,
        }
    }

    #[inline]
    fn first_key(&self) -> Option<&IndexKey> {
        self.entries.first().map(|entry| &entry.key)
    }

    #[inline]
    fn last_key(&self) -> Option<&IndexKey> {
        self.entries.last().map(|entry| &entry.key)
    }
}

#[derive(Clone, Debug)]
struct LeafEntry {
    key: IndexKey,
    tuple_ids: Vec<TupleId>,
    covering_rows: Vec<Row>,
}

impl IndexData {
    const DEFAULT_LEAF_PAGE_CAPACITY: usize = 64;

    #[allow(dead_code)]
    pub(crate) fn new(descriptor: IndexStorageDescriptor) -> Self {
        let covering_column_ids = default_covering_column_ids(&descriptor, None);
        Self {
            descriptor,
            covering_column_ids,
            leaves: Vec::new(),
            leaf_page_capacity: Self::DEFAULT_LEAF_PAGE_CAPACITY,
        }
    }

    pub(crate) fn new_for_table(
        descriptor: IndexStorageDescriptor,
        table_descriptor: &TableStorageDescriptor,
    ) -> Self {
        let covering_column_ids = default_covering_column_ids(&descriptor, Some(table_descriptor));
        Self {
            descriptor,
            covering_column_ids,
            leaves: Vec::new(),
            leaf_page_capacity: Self::DEFAULT_LEAF_PAGE_CAPACITY,
        }
    }

    #[cfg(test)]
    fn new_with_leaf_page_capacity(
        descriptor: IndexStorageDescriptor,
        leaf_page_capacity: usize,
    ) -> Self {
        let covering_column_ids = default_covering_column_ids(&descriptor, None);
        Self {
            descriptor,
            covering_column_ids,
            leaves: Vec::new(),
            leaf_page_capacity: leaf_page_capacity.max(1),
        }
    }

    pub(crate) fn from_rows<I>(
        descriptor: &IndexStorageDescriptor,
        table_descriptor: &TableStorageDescriptor,
        rows: I,
    ) -> DbResult<Self>
    where
        I: IntoIterator<Item = (TupleId, Row)>,
    {
        let rows = rows.into_iter();
        let (rows_lower_bound, _) = rows.size_hint();
        let key_ordinals = resolve_index_key_ordinals(table_descriptor, descriptor)?;
        let covering_column_ids = default_covering_column_ids(descriptor, Some(table_descriptor));
        let covering_ordinals = resolve_column_ordinals(table_descriptor, &covering_column_ids)?;
        let mut keyed_rows = Vec::with_capacity(rows_lower_bound);
        for (tuple_id, row) in rows {
            keyed_rows.push((
                build_index_key_with_ordinals(descriptor, &row, &key_ordinals)?,
                tuple_id,
                build_covering_row_with_ordinals(&row, &covering_ordinals)?,
            ));
        }

        keyed_rows.sort_unstable_by(
            |(left_key, left_tuple_id, _), (right_key, right_tuple_id, _)| {
                left_key
                    .cmp(right_key)
                    .then(left_tuple_id.cmp(right_tuple_id))
            },
        );

        let mut entries: Vec<LeafEntry> = Vec::with_capacity(keyed_rows.len());
        for (key, tuple_id, covering_row) in keyed_rows {
            if let Some(entry) = entries.last_mut() {
                if entry.key == key {
                    if descriptor.unique
                        && (descriptor.nulls_not_distinct || !Self::key_has_null(&key))
                        && match entry.tuple_ids.as_slice() {
                            [] => false,
                            [only_tuple_id] => *only_tuple_id != tuple_id,
                            _ => true,
                        }
                    {
                        return Err(unique_violation_error(descriptor.index_id));
                    }
                    insert_tuple_payload_sorted(
                        &mut entry.tuple_ids,
                        &mut entry.covering_rows,
                        tuple_id,
                        covering_row,
                    );
                    continue;
                }
            }
            entries.push(LeafEntry {
                key,
                tuple_ids: vec![tuple_id],
                covering_rows: vec![covering_row],
            });
        }

        let mut index = Self::new_for_table(descriptor.clone(), table_descriptor);
        index.covering_column_ids = covering_column_ids;
        index.bulk_load_entries(entries);
        Ok(index)
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn insert_tuple(
        &mut self,
        table_descriptor: &TableStorageDescriptor,
        tuple_id: TupleId,
        row: &Row,
    ) -> DbResult<()> {
        let key = self.validate_key_for_insert(table_descriptor, tuple_id, row)?;
        let covering_row = self.build_covering_row(table_descriptor, row)?;
        self.insert_entry(key, tuple_id, covering_row);
        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) fn insert_prebuilt_key_unchecked(&mut self, key: IndexKey, tuple_id: TupleId) {
        self.insert_entry(key, tuple_id, Row::new(Vec::new()));
    }

    pub(crate) fn insert_prebuilt_entry_unchecked(
        &mut self,
        key: IndexKey,
        tuple_id: TupleId,
        covering_row: Row,
    ) {
        self.insert_entry(key, tuple_id, covering_row);
    }

    pub(crate) fn build_covering_row(
        &self,
        table_descriptor: &TableStorageDescriptor,
        row: &Row,
    ) -> DbResult<Row> {
        let ordinals = resolve_column_ordinals(table_descriptor, &self.covering_column_ids)?;
        build_covering_row_with_ordinals(row, &ordinals)
    }

    pub(crate) fn covering_column_ids(&self) -> &[ColumnId] {
        &self.covering_column_ids
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn remove_tuple(
        &mut self,
        table_descriptor: &TableStorageDescriptor,
        tuple_id: TupleId,
        row: &Row,
    ) -> DbResult<()> {
        let key = self.build_key_for_row(table_descriptor, row)?;
        self.remove_prebuilt_key(tuple_id, &key);
        Ok(())
    }

    pub(crate) fn remove_prebuilt_key(&mut self, tuple_id: TupleId, key: &IndexKey) {
        if self.leaves.is_empty() {
            return;
        }

        let leaf_index = self.leaf_index_for_key(key);
        let mut remove_leaf = false;
        if let Some(leaf) = self.leaves.get_mut(leaf_index) {
            if let Ok(entry_index) = leaf.entries.binary_search_by(|entry| entry.key.cmp(key)) {
                let entry = &mut leaf.entries[entry_index];
                if remove_tuple_payload_sorted(
                    &mut entry.tuple_ids,
                    &mut entry.covering_rows,
                    tuple_id,
                ) {
                    leaf.tuple_id_count = leaf.tuple_id_count.saturating_sub(1);
                }
                if entry.tuple_ids.is_empty() {
                    leaf.entries.remove(entry_index);
                }
                remove_leaf = leaf.entries.is_empty();
            }
        }
        if remove_leaf {
            self.leaves.remove(leaf_index);
        }
    }

    /// First non-NULL key value for a single-column index. The leaf
    /// pages are stored in ASC order, so iterating forward and
    /// returning the first non-NULL component yields MIN(col) in
    /// effectively O(log N) (one btree path walk to the first leaf
    /// + a few entries for any NULL prefix).
    ///
    /// Returns `None` for multi-column indexes (callers should
    /// fall back to the full aggregate path) or empty / all-NULL
    /// indexes (caller treats as SQL NULL).
    pub(crate) fn first_non_null_single_column_value(&self) -> Option<Value> {
        if self.descriptor.key_columns.len() != 1 {
            return None;
        }
        for leaf in &self.leaves {
            for entry in &leaf.entries {
                let component = entry.key.components.first()?;
                if !matches!(component.value, Value::Null) {
                    return Some(component.value.clone());
                }
            }
        }
        None
    }

    /// Last non-NULL key value for a single-column index. The
    /// counterpart of `first_non_null_single_column_value` for
    /// MAX(col): walk leaves in reverse and take the last
    /// non-NULL component.
    pub(crate) fn last_non_null_single_column_value(&self) -> Option<Value> {
        if self.descriptor.key_columns.len() != 1 {
            return None;
        }
        for leaf in self.leaves.iter().rev() {
            for entry in leaf.entries.iter().rev() {
                let component = entry.key.components.first()?;
                if !matches!(component.value, Value::Null) {
                    return Some(component.value.clone());
                }
            }
        }
        None
    }

    pub(crate) fn key_is_strictly_after_last(&self, key: &IndexKey) -> bool {
        self.leaves
            .last()
            .and_then(LeafPage::last_key)
            .map_or(true, |last_key| key > last_key)
    }

    pub(crate) fn single_column_group_counts(
        &self,
        key_range: &KeyRange,
    ) -> DbResult<Vec<(Value, u64)>> {
        if self.descriptor.key_columns.len() != 1 {
            return Err(DbError::feature_not_supported(
                "index group counts require a single-column index",
            ));
        }
        let mut groups = Vec::new();
        for leaf in &self.leaves {
            for entry in &leaf.entries {
                if !key_matches_range(&entry.key, &self.descriptor.key_columns, key_range) {
                    continue;
                }
                let Some(component) = entry.key.components.first() else {
                    continue;
                };
                groups.push((
                    component.value.clone(),
                    usize_to_u64_saturating(entry.tuple_ids.len()),
                ));
            }
        }
        Ok(groups)
    }

    /// Walk the in-memory leaf pages to collect tuple_ids whose index keys
    /// fall inside `key_range`. This is the in-memory counterpart of the
    /// disk-ordered backends; for the unique-exact-point case it bottoms
    /// out as O(log N) via `candidate_tuple_ids_for_exact_bound`.
    ///
    /// (Was previously gated by `#[cfg(test)]`, which forced production
    /// scans to fall back to a full-tuple iteration in
    /// `latest_unique_exact_tuple_id`.)
    pub(crate) fn candidate_tuple_ids(&self, key_range: &KeyRange) -> DbResult<Vec<TupleId>> {
        if let Some(bound) = unique_non_null_exact_bound(&self.descriptor, key_range) {
            return Ok(self.candidate_tuple_ids_for_exact_bound(bound));
        }

        if matches!(key_range.lower, Bound::Unbounded)
            && matches!(key_range.upper, Bound::Unbounded)
        {
            let total_tuple_ids = self.leaves.iter().map(|leaf| leaf.tuple_id_count).sum();
            let mut candidates = Vec::with_capacity(total_tuple_ids);
            for leaf in &self.leaves {
                for entry in &leaf.entries {
                    match entry.tuple_ids.as_slice() {
                        [tuple_id] => candidates.push(*tuple_id),
                        tuple_ids => candidates.extend_from_slice(tuple_ids),
                    }
                }
            }
            return Ok(candidates);
        }

        let key_columns = &self.descriptor.key_columns;
        let start = self.leaves.partition_point(|leaf| {
            leaf_is_before_lower_bound(leaf, key_columns, &key_range.lower)
        });
        let end = self.leaves.partition_point(|leaf| {
            !leaf_is_after_upper_bound(leaf, key_columns, &key_range.upper)
        });
        if start >= end {
            return Ok(Vec::new());
        }

        let mut candidates = Vec::with_capacity(self.leaf_page_capacity);
        for leaf_index in start..end {
            let leaf = &self.leaves[leaf_index];
            let entry_start = if leaf_index == start {
                leaf_entry_start_index(leaf, key_columns, &key_range.lower)
            } else {
                0
            };
            let entry_end = if leaf_index + 1 == end {
                leaf_entry_end_index(leaf, key_columns, &key_range.upper)
            } else {
                leaf.entries.len()
            };
            if entry_start >= entry_end {
                continue;
            }
            let entries = &leaf.entries[entry_start..entry_end];
            // Every entry contributes at least one tuple id; this lowers realloc churn
            // on unique-index scans while keeping behavior unchanged.
            let additional_capacity = if self.descriptor.unique {
                entries.len()
            } else if entry_start == 0 && entry_end == leaf.entries.len() {
                leaf.tuple_id_count
            } else {
                entries.iter().map(|entry| entry.tuple_ids.len()).sum()
            };
            candidates.reserve(additional_capacity);
            for entry in entries {
                match entry.tuple_ids.as_slice() {
                    [tuple_id] => candidates.push(*tuple_id),
                    tuple_ids => candidates.extend_from_slice(tuple_ids),
                }
            }
        }
        Ok(candidates)
    }

    pub(crate) fn candidate_tuple_ids_ordered(
        &self,
        key_range: &KeyRange,
        descending: bool,
        limit: usize,
    ) -> DbResult<Vec<TupleId>> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let mut candidates = Vec::with_capacity(limit.min(1024));
        let key_columns = &self.descriptor.key_columns;
        let start = self.leaves.partition_point(|leaf| {
            leaf_is_before_lower_bound(leaf, key_columns, &key_range.lower)
        });
        let end = self.leaves.partition_point(|leaf| {
            !leaf_is_after_upper_bound(leaf, key_columns, &key_range.upper)
        });
        if start >= end {
            return Ok(candidates);
        }

        if descending {
            for leaf_index in (start..end).rev() {
                let leaf = &self.leaves[leaf_index];
                let entry_start = if leaf_index == start {
                    leaf_entry_start_index(leaf, key_columns, &key_range.lower)
                } else {
                    0
                };
                let entry_end = if leaf_index + 1 == end {
                    leaf_entry_end_index(leaf, key_columns, &key_range.upper)
                } else {
                    leaf.entries.len()
                };
                if entry_start >= entry_end {
                    continue;
                }
                for entry in leaf.entries[entry_start..entry_end].iter().rev() {
                    for tuple_id in entry.tuple_ids.iter().rev() {
                        candidates.push(*tuple_id);
                        if candidates.len() >= limit {
                            return Ok(candidates);
                        }
                    }
                }
            }
        } else {
            for leaf_index in start..end {
                let leaf = &self.leaves[leaf_index];
                let entry_start = if leaf_index == start {
                    leaf_entry_start_index(leaf, key_columns, &key_range.lower)
                } else {
                    0
                };
                let entry_end = if leaf_index + 1 == end {
                    leaf_entry_end_index(leaf, key_columns, &key_range.upper)
                } else {
                    leaf.entries.len()
                };
                if entry_start >= entry_end {
                    continue;
                }
                for entry in &leaf.entries[entry_start..entry_end] {
                    for tuple_id in &entry.tuple_ids {
                        candidates.push(*tuple_id);
                        if candidates.len() >= limit {
                            return Ok(candidates);
                        }
                    }
                }
            }
        }
        Ok(candidates)
    }

    pub(crate) fn covering_records_ordered_limited(
        &self,
        key_range: &KeyRange,
        projected_columns: Option<&[ColumnId]>,
        descending: bool,
        limit: usize,
    ) -> DbResult<Option<Vec<TupleRecord>>> {
        if self.covering_column_ids.is_empty() {
            return Ok(None);
        }
        let Some(projected_columns) = projected_columns else {
            return Ok(None);
        };
        let projection_ordinals = match resolve_covering_projection_ordinals(
            &self.covering_column_ids,
            projected_columns,
        ) {
            Some(ordinals) => ordinals,
            None => return Ok(None),
        };
        if limit == 0 {
            return Ok(Some(Vec::new()));
        }

        let mut records = Vec::with_capacity(limit.min(1024));
        let key_columns = &self.descriptor.key_columns;
        let start = self.leaves.partition_point(|leaf| {
            leaf_is_before_lower_bound(leaf, key_columns, &key_range.lower)
        });
        let end = self.leaves.partition_point(|leaf| {
            !leaf_is_after_upper_bound(leaf, key_columns, &key_range.upper)
        });
        if start >= end {
            return Ok(Some(records));
        }

        if descending {
            for leaf_index in (start..end).rev() {
                let leaf = &self.leaves[leaf_index];
                let entry_start = if leaf_index == start {
                    leaf_entry_start_index(leaf, key_columns, &key_range.lower)
                } else {
                    0
                };
                let entry_end = if leaf_index + 1 == end {
                    leaf_entry_end_index(leaf, key_columns, &key_range.upper)
                } else {
                    leaf.entries.len()
                };
                if entry_start >= entry_end {
                    continue;
                }
                for entry in leaf.entries[entry_start..entry_end].iter().rev() {
                    if entry.covering_rows.len() != entry.tuple_ids.len() {
                        return Ok(None);
                    }
                    for offset in (0..entry.tuple_ids.len()).rev() {
                        records.push(project_covering_record(
                            entry.tuple_ids[offset],
                            &entry.covering_rows[offset],
                            &projection_ordinals,
                        )?);
                        if records.len() >= limit {
                            return Ok(Some(records));
                        }
                    }
                }
            }
        } else {
            for leaf_index in start..end {
                let leaf = &self.leaves[leaf_index];
                let entry_start = if leaf_index == start {
                    leaf_entry_start_index(leaf, key_columns, &key_range.lower)
                } else {
                    0
                };
                let entry_end = if leaf_index + 1 == end {
                    leaf_entry_end_index(leaf, key_columns, &key_range.upper)
                } else {
                    leaf.entries.len()
                };
                if entry_start >= entry_end {
                    continue;
                }
                for entry in &leaf.entries[entry_start..entry_end] {
                    if entry.covering_rows.len() != entry.tuple_ids.len() {
                        return Ok(None);
                    }
                    for (tuple_id, covering_row) in entry.tuple_ids.iter().zip(&entry.covering_rows)
                    {
                        records.push(project_covering_record(
                            *tuple_id,
                            covering_row,
                            &projection_ordinals,
                        )?);
                        if records.len() >= limit {
                            return Ok(Some(records));
                        }
                    }
                }
            }
        }
        Ok(Some(records))
    }

    fn candidate_tuple_ids_for_exact_bound(&self, bound: &[Value]) -> Vec<TupleId> {
        if self.leaves.is_empty() {
            return Vec::new();
        }

        let key_columns = &self.descriptor.key_columns;
        let leaf_index = self.leaves.partition_point(|leaf| {
            leaf.last_key().is_some_and(|last_key| {
                compare_key_to_bound(last_key, key_columns, bound) == Ordering::Less
            })
        });
        let Some(leaf) = self.leaves.get(leaf_index) else {
            return Vec::new();
        };

        let Ok(entry_index) = leaf
            .entries
            .binary_search_by(|entry| compare_key_to_bound(&entry.key, key_columns, bound))
        else {
            return Vec::new();
        };

        leaf.entries[entry_index].tuple_ids.clone()
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn key_ordinals_for_table(
        &self,
        table_descriptor: &TableStorageDescriptor,
    ) -> DbResult<Vec<usize>> {
        resolve_index_key_ordinals_for_descriptor(table_descriptor, &self.descriptor)
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn build_key_for_row(
        &self,
        table_descriptor: &TableStorageDescriptor,
        row: &Row,
    ) -> DbResult<IndexKey> {
        build_index_key_for_descriptor(&self.descriptor, table_descriptor, row)
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn build_key_for_row_with_ordinals(
        &self,
        row: &Row,
        key_ordinals: &[usize],
    ) -> DbResult<IndexKey> {
        build_index_key_for_descriptor_with_ordinals(&self.descriptor, row, key_ordinals)
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn row_matches_with_ordinals(
        &self,
        row: &Row,
        key_range: &KeyRange,
        key_ordinals: &[usize],
    ) -> DbResult<bool> {
        row_matches_index_descriptor_with_ordinals(&self.descriptor, row, key_range, key_ordinals)
    }

    #[cfg(test)]
    #[allow(dead_code)]
    #[inline]
    pub(crate) fn key_matches(&self, key: &IndexKey, key_range: &KeyRange) -> bool {
        key_matches_index_descriptor(&self.descriptor, key, key_range)
    }

    #[allow(dead_code)]
    pub(crate) fn unique_key_for_row(
        &self,
        table_descriptor: &TableStorageDescriptor,
        row: &Row,
    ) -> DbResult<Option<IndexKey>> {
        unique_key_for_descriptor_row(&self.descriptor, table_descriptor, row)
    }

    #[cfg(test)]
    #[allow(dead_code)]
    fn validate_key_for_insert(
        &self,
        table_descriptor: &TableStorageDescriptor,
        tuple_id: TupleId,
        row: &Row,
    ) -> DbResult<IndexKey> {
        let key = self.build_key_for_row(table_descriptor, row)?;
        if self.descriptor.unique
            && (self.descriptor.nulls_not_distinct || !Self::key_has_null(&key))
            && self.key_conflicts_with_other_tuple(&key, tuple_id)
        {
            return Err(unique_violation_error(self.descriptor.index_id));
        }
        Ok(key)
    }

    /// Estimate the in-memory byte footprint of this index.
    pub(crate) fn estimated_bytes(&self) -> u64 {
        const PER_LEAF_PAGE_OVERHEAD: u64 = 128;
        const PER_ENTRY_OVERHEAD: u64 = 24;
        // Per IndexKeyComponent: Value enum (~48 bytes) + 2 bools.
        const PER_KEY_COMPONENT: u64 = 56;
        const PER_TUPLE_ID: u64 = 8;

        let mut bytes = 0u64;
        for leaf in &self.leaves {
            bytes += PER_LEAF_PAGE_OVERHEAD;
            for entry in &leaf.entries {
                bytes += PER_ENTRY_OVERHEAD;
                bytes += usize_to_u64_saturating(entry.key.components.len()) * PER_KEY_COMPONENT;
                bytes += usize_to_u64_saturating(entry.tuple_ids.len()) * PER_TUPLE_ID;
            }
        }
        bytes
    }

    fn bulk_load_entries(&mut self, entries: Vec<LeafEntry>) {
        self.leaves.clear();
        if entries.is_empty() {
            return;
        }

        let fill = self.bulk_load_leaf_fill();
        let leaf_count = entries.len().div_ceil(fill);
        self.leaves.reserve(leaf_count);
        let mut iter = entries.into_iter();
        loop {
            let mut leaf_entries = Vec::with_capacity(fill);
            for _ in 0..fill {
                let Some(entry) = iter.next() else {
                    break;
                };
                leaf_entries.push(entry);
            }
            if leaf_entries.is_empty() {
                break;
            }
            self.leaves.push(LeafPage::with_entries(leaf_entries));
        }
    }

    fn bulk_load_leaf_fill(&self) -> usize {
        self.leaf_page_capacity
            .saturating_sub(self.leaf_page_capacity / 8)
            .max(1)
    }

    fn insert_entry(&mut self, key: IndexKey, tuple_id: TupleId, covering_row: Row) {
        if self.leaves.is_empty() {
            self.leaves.push(LeafPage {
                entries: vec![LeafEntry {
                    key,
                    tuple_ids: vec![tuple_id],
                    covering_rows: vec![covering_row],
                }],
                tuple_id_count: 1,
            });
            return;
        }

        let leaf_index = self.leaf_index_for_key(&key);
        let leaf = &mut self.leaves[leaf_index];
        match leaf.entries.binary_search_by(|entry| entry.key.cmp(&key)) {
            Ok(entry_index) => {
                let entry = &mut leaf.entries[entry_index];
                if insert_tuple_payload_sorted(
                    &mut entry.tuple_ids,
                    &mut entry.covering_rows,
                    tuple_id,
                    covering_row,
                ) {
                    leaf.tuple_id_count += 1;
                }
            }
            Err(entry_index) => {
                let new_entry = LeafEntry {
                    key,
                    tuple_ids: vec![tuple_id],
                    covering_rows: vec![covering_row],
                };
                if entry_index == leaf.entries.len() {
                    leaf.entries.push(new_entry);
                } else {
                    leaf.entries.insert(entry_index, new_entry);
                }
                leaf.tuple_id_count += 1;
            }
        }

        if leaf.entries.len() > self.leaf_page_capacity {
            self.split_leaf(leaf_index);
        }
    }

    fn leaf_index_for_key(&self, key: &IndexKey) -> usize {
        let index = self
            .leaves
            .partition_point(|leaf| leaf.first_key().is_some_and(|first| first <= key));
        index.saturating_sub(1)
    }

    fn split_leaf(&mut self, leaf_index: usize) {
        let Some(leaf) = self.leaves.get_mut(leaf_index) else {
            return;
        };
        if leaf.entries.len() <= self.leaf_page_capacity {
            return;
        }

        let split_at = leaf.entries.len() / 2;
        let right_entries = leaf.entries.split_off(split_at);
        let right_tuple_id_count = right_entries
            .iter()
            .map(|entry| entry.tuple_ids.len())
            .sum();
        leaf.tuple_id_count = leaf.tuple_id_count.saturating_sub(right_tuple_id_count);
        self.leaves.insert(
            leaf_index + 1,
            LeafPage {
                entries: right_entries,
                tuple_id_count: right_tuple_id_count,
            },
        );
    }

    pub(crate) fn key_has_null(key: &IndexKey) -> bool {
        key.components
            .iter()
            .any(|component| matches!(component.value, Value::Null))
    }

    pub(crate) fn candidate_tuple_ids_for_key(&self, key: &IndexKey) -> Vec<TupleId> {
        if self.leaves.is_empty() {
            return Vec::new();
        }
        let leaf_index = self.leaf_index_for_key(key);
        let Some(leaf) = self.leaves.get(leaf_index) else {
            return Vec::new();
        };
        let Ok(entry_index) = leaf.entries.binary_search_by(|entry| entry.key.cmp(key)) else {
            return Vec::new();
        };
        leaf.entries[entry_index].tuple_ids.clone()
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn key_conflicts_with_other_tuple(&self, key: &IndexKey, tuple_id: TupleId) -> bool {
        let tuple_ids = self.candidate_tuple_ids_for_key(key);
        match tuple_ids.as_slice() {
            [] => false,
            [only_tuple_id] => *only_tuple_id != tuple_id,
            _ => true,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct IndexKey {
    components: Vec<IndexKeyComponent>,
}

#[derive(Clone, Debug)]
struct IndexKeyComponent {
    value: Value,
    descending: bool,
    nulls_first: bool,
}

impl PartialEq for IndexKey {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for IndexKey {}

impl PartialOrd for IndexKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for IndexKey {
    fn cmp(&self, other: &Self) -> Ordering {
        for (left, right) in self.components.iter().zip(&other.components) {
            let mut ordering = compare_values_total(&left.value, &right.value, left.nulls_first);
            if left.descending {
                ordering = ordering.reverse();
            }
            if ordering != Ordering::Equal {
                return ordering;
            }
        }
        self.components.len().cmp(&other.components.len())
    }
}

#[cfg(test)]
fn build_index_key(
    table_descriptor: &TableStorageDescriptor,
    index_descriptor: &IndexStorageDescriptor,
    row: &Row,
) -> DbResult<IndexKey> {
    let key_ordinals = resolve_index_key_ordinals(table_descriptor, index_descriptor)?;
    build_index_key_with_ordinals(index_descriptor, row, &key_ordinals)
}

fn resolve_index_key_ordinals(
    table_descriptor: &TableStorageDescriptor,
    index_descriptor: &IndexStorageDescriptor,
) -> DbResult<Vec<usize>> {
    let mut ordinals = Vec::with_capacity(index_descriptor.key_columns.len());
    for key_column in &index_descriptor.key_columns {
        let ordinal = table_descriptor
            .columns
            .iter()
            .position(|column| column.column_id == key_column.column_id)
            .ok_or_else(|| DbError::internal("index key references unknown column"))?;
        ordinals.push(ordinal);
    }
    Ok(ordinals)
}

fn default_covering_column_ids(
    index_descriptor: &IndexStorageDescriptor,
    table_descriptor: Option<&TableStorageDescriptor>,
) -> Vec<ColumnId> {
    let primary_key = table_descriptor.and_then(|table| table.primary_key.as_ref());
    let primary_key_index = primary_key.is_some_and(|primary_key| {
        primary_key.len() == index_descriptor.key_columns.len()
            && primary_key
                .iter()
                .zip(index_descriptor.key_columns.iter())
                .all(|(primary_column, key_column)| *primary_column == key_column.column_id)
    });
    if primary_key_index && index_descriptor.include_columns.is_empty() {
        if let Some(table) = table_descriptor {
            return table
                .columns
                .iter()
                .map(|column| column.column_id)
                .collect();
        }
    }
    let primary_key_adds_payload = primary_key.is_some_and(|primary_key| {
        primary_key.iter().any(|column_id| {
            !index_descriptor
                .key_columns
                .iter()
                .any(|key_column| key_column.column_id == *column_id)
                && !index_descriptor.include_columns.contains(column_id)
        })
    });
    if index_descriptor.include_columns.is_empty() && !primary_key_adds_payload {
        return Vec::new();
    }
    let mut column_ids = Vec::with_capacity(
        index_descriptor.key_columns.len()
            + index_descriptor.include_columns.len()
            + primary_key.map_or(0, Vec::len),
    );
    for key_column in &index_descriptor.key_columns {
        push_unique_column_id(&mut column_ids, key_column.column_id);
    }
    for column_id in &index_descriptor.include_columns {
        push_unique_column_id(&mut column_ids, *column_id);
    }
    if let Some(primary_key) = primary_key {
        for column_id in primary_key {
            push_unique_column_id(&mut column_ids, *column_id);
        }
    }
    column_ids
}

#[inline]
fn push_unique_column_id(column_ids: &mut Vec<ColumnId>, column_id: ColumnId) {
    if !column_ids.contains(&column_id) {
        column_ids.push(column_id);
    }
}

fn resolve_column_ordinals(
    table_descriptor: &TableStorageDescriptor,
    column_ids: &[ColumnId],
) -> DbResult<Vec<usize>> {
    let mut ordinals = Vec::with_capacity(column_ids.len());
    for column_id in column_ids {
        let ordinal = table_descriptor
            .columns
            .iter()
            .position(|column| column.column_id == *column_id)
            .ok_or_else(|| DbError::internal("index covering column references unknown column"))?;
        ordinals.push(ordinal);
    }
    Ok(ordinals)
}

fn build_covering_row_with_ordinals(row: &Row, covering_ordinals: &[usize]) -> DbResult<Row> {
    let mut values = Vec::with_capacity(covering_ordinals.len());
    for ordinal in covering_ordinals {
        values.push(
            row.values
                .get(*ordinal)
                .cloned()
                .ok_or_else(|| DbError::internal("row is missing index covering value"))?,
        );
    }
    Ok(Row::new(values))
}

fn resolve_covering_projection_ordinals(
    covering_column_ids: &[ColumnId],
    projected_columns: &[ColumnId],
) -> Option<Vec<usize>> {
    let mut ordinals = Vec::with_capacity(projected_columns.len());
    for projected_column in projected_columns {
        let ordinal = covering_column_ids
            .iter()
            .position(|column_id| column_id == projected_column)?;
        ordinals.push(ordinal);
    }
    Some(ordinals)
}

fn project_covering_record(
    tuple_id: TupleId,
    covering_row: &Row,
    projection_ordinals: &[usize],
) -> DbResult<TupleRecord> {
    let mut values = Vec::with_capacity(projection_ordinals.len());
    for ordinal in projection_ordinals {
        values.push(
            covering_row.values.get(*ordinal).cloned().ok_or_else(|| {
                DbError::internal("index covering row is missing projected value")
            })?,
        );
    }
    Ok(TupleRecord {
        tuple_id,
        heap_position: tuple_id.get(),
        row: Row::new(values),
    })
}

pub(crate) fn resolve_index_key_ordinals_for_descriptor(
    table_descriptor: &TableStorageDescriptor,
    index_descriptor: &IndexStorageDescriptor,
) -> DbResult<Vec<usize>> {
    resolve_index_key_ordinals(table_descriptor, index_descriptor)
}

fn build_index_key_with_ordinals(
    index_descriptor: &IndexStorageDescriptor,
    row: &Row,
    key_ordinals: &[usize],
) -> DbResult<IndexKey> {
    if key_ordinals.len() != index_descriptor.key_columns.len() {
        return Err(DbError::internal(
            "index key ordinals do not match index descriptor columns",
        ));
    }
    let mut components = Vec::with_capacity(index_descriptor.key_columns.len());
    for (key_column, ordinal) in index_descriptor.key_columns.iter().zip(key_ordinals.iter()) {
        let value = row
            .values
            .get(*ordinal)
            .cloned()
            .ok_or_else(|| DbError::internal("row is missing indexed value"))?;
        components.push(IndexKeyComponent {
            value,
            descending: key_column.descending,
            nulls_first: key_column.nulls_first,
        });
    }
    Ok(IndexKey { components })
}

pub(crate) fn build_index_key_for_descriptor_with_ordinals(
    index_descriptor: &IndexStorageDescriptor,
    row: &Row,
    key_ordinals: &[usize],
) -> DbResult<IndexKey> {
    build_index_key_with_ordinals(index_descriptor, row, key_ordinals)
}

pub(crate) fn build_index_key_for_descriptor(
    index_descriptor: &IndexStorageDescriptor,
    table_descriptor: &TableStorageDescriptor,
    row: &Row,
) -> DbResult<IndexKey> {
    let key_ordinals =
        resolve_index_key_ordinals_for_descriptor(table_descriptor, index_descriptor)?;
    build_index_key_for_descriptor_with_ordinals(index_descriptor, row, &key_ordinals)
}

pub(crate) fn index_key_has_null(key: &IndexKey) -> bool {
    IndexData::key_has_null(key)
}

pub(crate) fn unique_key_for_descriptor_row(
    index_descriptor: &IndexStorageDescriptor,
    table_descriptor: &TableStorageDescriptor,
    row: &Row,
) -> DbResult<Option<IndexKey>> {
    let key_ordinals =
        resolve_index_key_ordinals_for_descriptor(table_descriptor, index_descriptor)?;
    let key = build_index_key_for_descriptor_with_ordinals(index_descriptor, row, &key_ordinals)?;
    if IndexData::key_has_null(&key) && !index_descriptor.nulls_not_distinct {
        return Ok(None);
    }
    Ok(Some(key))
}

fn key_matches_range(key: &IndexKey, key_columns: &[IndexKeyColumn], key_range: &KeyRange) -> bool {
    let lower_ok = match &key_range.lower {
        Bound::Unbounded => true,
        Bound::Included(bound) => compare_key_to_bound(key, key_columns, bound) != Ordering::Less,
        Bound::Excluded(bound) => {
            compare_key_to_bound(key, key_columns, bound) == Ordering::Greater
        }
    };
    if !lower_ok {
        return false;
    }

    match &key_range.upper {
        Bound::Unbounded => true,
        Bound::Included(bound) => {
            compare_key_to_bound(key, key_columns, bound) != Ordering::Greater
        }
        Bound::Excluded(bound) => compare_key_to_bound(key, key_columns, bound) == Ordering::Less,
    }
}

pub(crate) fn key_matches_index_descriptor(
    index_descriptor: &IndexStorageDescriptor,
    key: &IndexKey,
    key_range: &KeyRange,
) -> bool {
    key_matches_range(key, &index_descriptor.key_columns, key_range)
}

pub(crate) fn row_matches_index_descriptor_with_ordinals(
    index_descriptor: &IndexStorageDescriptor,
    row: &Row,
    key_range: &KeyRange,
    key_ordinals: &[usize],
) -> DbResult<bool> {
    let key = build_index_key_for_descriptor_with_ordinals(index_descriptor, row, key_ordinals)?;
    Ok(key_matches_range(
        &key,
        &index_descriptor.key_columns,
        key_range,
    ))
}

#[inline]
fn unique_non_null_exact_bound<'a>(
    descriptor: &IndexStorageDescriptor,
    key_range: &'a KeyRange,
) -> Option<&'a [Value]> {
    if !descriptor.unique {
        return None;
    }
    let (Bound::Included(lower), Bound::Included(upper)) = (&key_range.lower, &key_range.upper)
    else {
        return None;
    };
    if lower.len() != descriptor.key_columns.len() || upper.len() != descriptor.key_columns.len() {
        return None;
    }
    if lower.iter().any(|value| matches!(value, Value::Null)) {
        return None;
    }
    if !bound_values_equal(&descriptor.key_columns, lower, upper) {
        return None;
    }
    Some(lower.as_slice())
}

#[inline]
fn bound_values_equal(key_columns: &[IndexKeyColumn], left: &[Value], right: &[Value]) -> bool {
    left.iter()
        .zip(right)
        .zip(key_columns)
        .all(|((left_value, right_value), key_column)| {
            compare_values_total(left_value, right_value, key_column.nulls_first) == Ordering::Equal
        })
}

#[inline]
fn leaf_is_before_lower_bound(
    leaf: &LeafPage,
    key_columns: &[IndexKeyColumn],
    lower: &Bound<Vec<Value>>,
) -> bool {
    let Some(last_key) = leaf.last_key() else {
        return false;
    };

    match lower {
        Bound::Unbounded => false,
        Bound::Included(bound) => {
            compare_key_to_bound(last_key, key_columns, bound) == Ordering::Less
        }
        Bound::Excluded(bound) => {
            compare_key_to_bound(last_key, key_columns, bound) != Ordering::Greater
        }
    }
}

#[inline]
fn leaf_is_after_upper_bound(
    leaf: &LeafPage,
    key_columns: &[IndexKeyColumn],
    upper: &Bound<Vec<Value>>,
) -> bool {
    let Some(first_key) = leaf.first_key() else {
        return false;
    };

    match upper {
        Bound::Unbounded => false,
        Bound::Included(bound) => {
            compare_key_to_bound(first_key, key_columns, bound) == Ordering::Greater
        }
        Bound::Excluded(bound) => {
            compare_key_to_bound(first_key, key_columns, bound) != Ordering::Less
        }
    }
}

#[inline]
fn leaf_entry_start_index(
    leaf: &LeafPage,
    key_columns: &[IndexKeyColumn],
    lower: &Bound<Vec<Value>>,
) -> usize {
    match lower {
        Bound::Unbounded => 0,
        Bound::Included(bound) => leaf.entries.partition_point(|entry| {
            compare_key_to_bound(&entry.key, key_columns, bound) == Ordering::Less
        }),
        Bound::Excluded(bound) => leaf.entries.partition_point(|entry| {
            compare_key_to_bound(&entry.key, key_columns, bound) != Ordering::Greater
        }),
    }
}

#[inline]
fn leaf_entry_end_index(
    leaf: &LeafPage,
    key_columns: &[IndexKeyColumn],
    upper: &Bound<Vec<Value>>,
) -> usize {
    match upper {
        Bound::Unbounded => leaf.entries.len(),
        Bound::Included(bound) => leaf.entries.partition_point(|entry| {
            compare_key_to_bound(&entry.key, key_columns, bound) != Ordering::Greater
        }),
        Bound::Excluded(bound) => leaf.entries.partition_point(|entry| {
            compare_key_to_bound(&entry.key, key_columns, bound) == Ordering::Less
        }),
    }
}

fn insert_tuple_payload_sorted(
    tuple_ids: &mut Vec<TupleId>,
    covering_rows: &mut Vec<Row>,
    tuple_id: TupleId,
    covering_row: Row,
) -> bool {
    match tuple_ids.binary_search(&tuple_id) {
        Ok(index) => {
            if let Some(slot) = covering_rows.get_mut(index) {
                *slot = covering_row;
            }
            false
        }
        Err(index) => {
            tuple_ids.insert(index, tuple_id);
            if covering_rows.len() == tuple_ids.len().saturating_sub(1) {
                covering_rows.insert(index, covering_row);
            }
            true
        }
    }
}

fn unique_violation_error(index_id: aiondb_core::IndexId) -> DbError {
    DbError::constraint_error(
        SqlState::UniqueViolation,
        format!(
            "duplicate key value violates unique index {}",
            index_id.get()
        ),
    )
}

fn remove_tuple_payload_sorted(
    tuple_ids: &mut Vec<TupleId>,
    covering_rows: &mut Vec<Row>,
    tuple_id: TupleId,
) -> bool {
    if let Ok(index) = tuple_ids.binary_search(&tuple_id) {
        tuple_ids.remove(index);
        if index < covering_rows.len() {
            covering_rows.remove(index);
        }
        true
    } else {
        false
    }
}

#[inline]
fn compare_key_to_bound(
    key: &IndexKey,
    key_columns: &[IndexKeyColumn],
    bound_values: &[Value],
) -> Ordering {
    for ((component, key_column), bound_value) in key
        .components
        .iter()
        .zip(key_columns)
        .zip(bound_values.iter())
    {
        let mut ordering =
            compare_values_total(&component.value, bound_value, key_column.nulls_first);
        if key_column.descending {
            ordering = ordering.reverse();
        }
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    // Prefix bounds (`WHERE k1 >= ...` on an index `(k1, k2, ...)`) are
    // considered equal once all bound components match.
    if bound_values.len() <= key.components.len() {
        Ordering::Equal
    } else {
        Ordering::Less
    }
}

fn jsonb_type_rank(value: &serde_json::Value) -> u8 {
    match value {
        serde_json::Value::Null => 0,
        serde_json::Value::Bool(_) => 1,
        serde_json::Value::Number(_) => 2,
        serde_json::Value::String(_) => 3,
        serde_json::Value::Array(_) => 4,
        serde_json::Value::Object(_) => 5,
    }
}

fn compare_jsonb_numbers(left: &serde_json::Number, right: &serde_json::Number) -> Ordering {
    match (left.as_i64(), right.as_i64()) {
        (Some(l), Some(r)) => return l.cmp(&r),
        (Some(l), None) => {
            if let Some(r) = right.as_u64() {
                return if l < 0 {
                    Ordering::Less
                } else {
                    u64::try_from(l).unwrap_or(0).cmp(&r)
                };
            }
        }
        (None, Some(r)) => {
            if let Some(l) = left.as_u64() {
                return if r < 0 {
                    Ordering::Greater
                } else {
                    l.cmp(&u64::try_from(r).unwrap_or(0))
                };
            }
        }
        (None, None) => {}
    }

    if let (Some(l), Some(r)) = (left.as_u64(), right.as_u64()) {
        return l.cmp(&r);
    }

    if let (Some(l), Some(r)) = (left.as_f64(), right.as_f64()) {
        if let Some(ordering) = l.partial_cmp(&r) {
            return ordering;
        }
    }

    left.to_string().cmp(&right.to_string())
}

const MAX_BTREE_VALUE_COMPARE_DEPTH: usize = 256;

fn compare_jsonb_values_total(left: &serde_json::Value, right: &serde_json::Value) -> Ordering {
    compare_jsonb_values_total_at_depth(left, right, 0)
}

fn compare_jsonb_values_total_at_depth(
    left: &serde_json::Value,
    right: &serde_json::Value,
    depth: usize,
) -> Ordering {
    let rank_cmp = jsonb_type_rank(left).cmp(&jsonb_type_rank(right));
    if rank_cmp != Ordering::Equal {
        return rank_cmp;
    }
    if depth >= MAX_BTREE_VALUE_COMPARE_DEPTH {
        return match (left, right) {
            (serde_json::Value::Array(left), serde_json::Value::Array(right)) => {
                left.len().cmp(&right.len())
            }
            (serde_json::Value::Object(left), serde_json::Value::Object(right)) => {
                left.len().cmp(&right.len())
            }
            _ => Ordering::Equal,
        };
    }

    match (left, right) {
        (serde_json::Value::Null, serde_json::Value::Null) => Ordering::Equal,
        (serde_json::Value::Bool(left), serde_json::Value::Bool(right)) => left.cmp(right),
        (serde_json::Value::Number(left), serde_json::Value::Number(right)) => {
            compare_jsonb_numbers(left, right)
        }
        (serde_json::Value::String(left), serde_json::Value::String(right)) => left.cmp(right),
        (serde_json::Value::Array(left), serde_json::Value::Array(right)) => {
            for (left_elem, right_elem) in left.iter().zip(right.iter()) {
                let ordering =
                    compare_jsonb_values_total_at_depth(left_elem, right_elem, depth + 1);
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
            left.len().cmp(&right.len())
        }
        (serde_json::Value::Object(left), serde_json::Value::Object(right)) => {
            let mut left_entries = left.iter().collect::<Vec<_>>();
            let mut right_entries = right.iter().collect::<Vec<_>>();
            left_entries.sort_unstable_by_key(|(key, _)| *key);
            right_entries.sort_unstable_by_key(|(key, _)| *key);

            for ((left_key, left_val), (right_key, right_val)) in
                left_entries.iter().zip(right_entries.iter())
            {
                let key_cmp = left_key.cmp(right_key);
                if key_cmp != Ordering::Equal {
                    return key_cmp;
                }
                let value_cmp = compare_jsonb_values_total_at_depth(left_val, right_val, depth + 1);
                if value_cmp != Ordering::Equal {
                    return value_cmp;
                }
            }
            left_entries.len().cmp(&right_entries.len())
        }
        _ => Ordering::Equal,
    }
}

fn compare_values_total(left: &Value, right: &Value, nulls_first: bool) -> Ordering {
    compare_values_total_at_depth(left, right, nulls_first, 0)
}

use super::helpers::u64_to_f64;

fn i64_to_f64(value: i64) -> f64 {
    if value.is_negative() {
        -u64_to_f64(value.unsigned_abs())
    } else {
        u64_to_f64(value.unsigned_abs())
    }
}

fn numeric_to_f64_without_numeric(value: &Value) -> Option<f64> {
    match value {
        Value::Int(v) => Some(f64::from(*v)),
        Value::BigInt(v) => Some(i64_to_f64(*v)),
        Value::Real(v) => Some(f64::from(*v)),
        Value::Double(v) => Some(*v),
        Value::Money(v) => Some(i64_to_f64(*v)),
        Value::Boolean(v) => Some(if *v { 1.0 } else { 0.0 }),
        _ => None,
    }
}

fn numeric_value_for_index_cmp(value: &Value) -> Option<aiondb_core::NumericValue> {
    match value {
        Value::Numeric(v) => Some(v.clone()),
        Value::Int(v) => Some(aiondb_core::NumericValue::from_i32(*v)),
        Value::BigInt(v) => Some(aiondb_core::NumericValue::from_i64(*v)),
        Value::Money(v) => Some(aiondb_core::NumericValue::from_i64(*v)),
        Value::Boolean(v) => Some(aiondb_core::NumericValue::from_i32(i32::from(*v))),
        Value::Real(v) => numeric_value_from_f64(f64::from(*v)),
        Value::Double(v) => numeric_value_from_f64(*v),
        _ => None,
    }
}

fn numeric_value_from_f64(value: f64) -> Option<aiondb_core::NumericValue> {
    if value.is_nan() {
        return Some(aiondb_core::NumericValue::NAN);
    }
    if value == f64::INFINITY {
        return Some(aiondb_core::NumericValue::INFINITY);
    }
    if value == f64::NEG_INFINITY {
        return Some(aiondb_core::NumericValue::NEG_INFINITY);
    }
    value.to_string().parse::<aiondb_core::NumericValue>().ok()
}

fn compare_array_values_total(left: &[Value], right: &[Value], depth: usize) -> Ordering {
    if depth >= MAX_BTREE_VALUE_COMPARE_DEPTH {
        return left.len().cmp(&right.len());
    }
    for (left_value, right_value) in left.iter().zip(right.iter()) {
        let ordering = compare_values_total_at_depth(left_value, right_value, false, depth + 1);
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    left.len().cmp(&right.len())
}

fn compare_values_total_at_depth(
    left: &Value,
    right: &Value,
    nulls_first: bool,
    depth: usize,
) -> Ordering {
    if depth >= MAX_BTREE_VALUE_COMPARE_DEPTH {
        return value_rank(left).cmp(&value_rank(right));
    }

    match (left, right) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Null, _) => {
            if nulls_first {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        }
        (_, Value::Null) => {
            if nulls_first {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        }
        (Value::Numeric(left), Value::Numeric(right)) => left.cmp(right),
        _ if matches!(left, Value::Numeric(_)) || matches!(right, Value::Numeric(_)) => {
            if let (Some(left_num), Some(right_num)) = (
                numeric_value_for_index_cmp(left),
                numeric_value_for_index_cmp(right),
            ) {
                left_num.cmp(&right_num)
            } else {
                value_rank(left).cmp(&value_rank(right))
            }
        }
        // Keep mixed numeric types in one ordered domain so index-range
        // predicates like FLOAT column vs NUMERIC literal remain comparable.
        _ if numeric_to_f64_without_numeric(left).is_some()
            && numeric_to_f64_without_numeric(right).is_some() =>
        {
            if let (Some(left_num), Some(right_num)) = (
                numeric_to_f64_without_numeric(left),
                numeric_to_f64_without_numeric(right),
            ) {
                left_num.total_cmp(&right_num)
            } else {
                value_rank(left).cmp(&value_rank(right))
            }
        }
        (Value::Int(left), Value::Int(right)) => left.cmp(right),
        (Value::Int(left), Value::BigInt(right)) => i64::from(*left).cmp(right),
        (Value::BigInt(left), Value::Int(right)) => left.cmp(&i64::from(*right)),
        (Value::BigInt(left), Value::BigInt(right)) => left.cmp(right),
        (Value::Real(left), Value::Real(right)) => left.total_cmp(right),
        (Value::Real(left), Value::Double(right)) => f64::from(*left).total_cmp(right),
        (Value::Double(left), Value::Real(right)) => left.total_cmp(&f64::from(*right)),
        (Value::Double(left), Value::Double(right)) => left.total_cmp(right),
        (Value::Text(left), Value::Text(right)) => left.cmp(right),
        (Value::Boolean(left), Value::Boolean(right)) => left.cmp(right),
        (Value::Blob(left), Value::Blob(right)) => left.cmp(right),
        (Value::Timestamp(left), Value::Timestamp(right)) => left.cmp(right),
        (Value::Date(left), Value::Date(right)) => left.cmp(right),
        (Value::Interval(left), Value::Interval(right)) => left
            .months
            .cmp(&right.months)
            .then(left.days.cmp(&right.days))
            .then(left.micros.cmp(&right.micros)),
        (Value::Tid(left), Value::Tid(right)) => left.cmp(right),
        (Value::PgLsn(left), Value::PgLsn(right)) => left.cmp(right),
        (Value::MacAddr(left), Value::MacAddr(right)) => left.as_bytes().cmp(right.as_bytes()),
        (Value::MacAddr8(left), Value::MacAddr8(right)) => left.as_bytes().cmp(right.as_bytes()),
        (Value::Uuid(left), Value::Uuid(right)) => left.cmp(right),
        (Value::TimestampTz(left), Value::TimestampTz(right)) => left.cmp(right),
        (Value::Jsonb(left), Value::Jsonb(right)) => compare_jsonb_values_total(left, right),
        (Value::Vector(left), Value::Vector(right)) => left
            .dims
            .cmp(&right.dims)
            .then_with(|| compare_vector_values(&left.values, &right.values)),
        (Value::Array(left), Value::Array(right)) => {
            compare_array_values_total(left, right, depth + 1)
        }
        _ => value_rank(left).cmp(&value_rank(right)),
    }
}

use super::{compare_vector_values, value_rank};

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_core::{ColumnId, DataType, IndexId, RelationId};

    fn table_descriptor() -> TableStorageDescriptor {
        TableStorageDescriptor {
            table_id: RelationId::new(1),
            columns: vec![aiondb_storage_api::StorageColumn {
                column_id: ColumnId::new(1),
                data_type: DataType::Int,
                nullable: false,
            }],
            primary_key: None,
            shard_config: None,
        }
    }

    fn index_descriptor() -> IndexStorageDescriptor {
        IndexStorageDescriptor {
            index_id: IndexId::new(1),
            table_id: RelationId::new(1),
            unique: false,
            nulls_not_distinct: false,
            gin: false,
            key_columns: vec![IndexKeyColumn {
                column_id: ColumnId::new(1),
                descending: false,
                nulls_first: false,
            }],
            include_columns: Vec::new(),
            hnsw_options: None,
            ivf_flat_options: None,
        }
    }

    fn table_descriptor_two_cols() -> TableStorageDescriptor {
        TableStorageDescriptor {
            table_id: RelationId::new(1),
            columns: vec![
                aiondb_storage_api::StorageColumn {
                    column_id: ColumnId::new(1),
                    data_type: DataType::Int,
                    nullable: false,
                },
                aiondb_storage_api::StorageColumn {
                    column_id: ColumnId::new(2),
                    data_type: DataType::Int,
                    nullable: false,
                },
            ],
            primary_key: None,
            shard_config: None,
        }
    }

    fn composite_index_descriptor() -> IndexStorageDescriptor {
        IndexStorageDescriptor {
            index_id: IndexId::new(1),
            table_id: RelationId::new(1),
            unique: false,
            nulls_not_distinct: false,
            gin: false,
            key_columns: vec![
                IndexKeyColumn {
                    column_id: ColumnId::new(1),
                    descending: false,
                    nulls_first: false,
                },
                IndexKeyColumn {
                    column_id: ColumnId::new(2),
                    descending: false,
                    nulls_first: false,
                },
            ],
            include_columns: Vec::new(),
            hnsw_options: None,
            ivf_flat_options: None,
        }
    }

    #[test]
    fn bulk_load_builds_multiple_leaf_pages() {
        let table = table_descriptor();
        let descriptor = index_descriptor();
        let rows = (0..20).map(|i| (TupleId::new((i + 1) as u64), Row::new(vec![Value::Int(i)])));
        let mut built = IndexData::new_with_leaf_page_capacity(descriptor.clone(), 4);
        let mut keyed_rows = Vec::new();
        for (tuple_id, row) in rows {
            keyed_rows.push((
                build_index_key(&table, &descriptor, &row).unwrap(),
                tuple_id,
            ));
        }
        keyed_rows.sort_unstable_by(|(left_key, left_tuple_id), (right_key, right_tuple_id)| {
            left_key
                .cmp(right_key)
                .then(left_tuple_id.cmp(right_tuple_id))
        });
        let entries = keyed_rows
            .into_iter()
            .map(|(key, tuple_id)| LeafEntry {
                key,
                tuple_ids: vec![tuple_id],
                covering_rows: vec![Row::new(Vec::new())],
            })
            .collect();
        built.bulk_load_entries(entries);

        assert!(!built.leaves.is_empty());
        assert!(built.leaves.len() >= 5);
        assert!(built.estimated_bytes() > 0);
    }

    #[test]
    fn insert_splits_leaf_pages_explicitly() {
        let table = table_descriptor();
        let mut index = IndexData::new_with_leaf_page_capacity(index_descriptor(), 2);

        for i in 0..5 {
            index
                .insert_tuple(
                    &table,
                    TupleId::new((i + 1) as u64),
                    &Row::new(vec![Value::Int(i)]),
                )
                .unwrap();
        }

        assert!(index.leaves.len() >= 3);
    }

    #[test]
    fn duplicate_tuple_insert_is_idempotent() {
        let table = table_descriptor();
        let mut index = IndexData::new(index_descriptor());
        let row = Row::new(vec![Value::Int(7)]);

        index.insert_tuple(&table, TupleId::new(1), &row).unwrap();
        index.insert_tuple(&table, TupleId::new(1), &row).unwrap();

        let matches = index
            .candidate_tuple_ids(&KeyRange {
                lower: Bound::Included(vec![Value::Int(7)]),
                upper: Bound::Included(vec![Value::Int(7)]),
            })
            .unwrap();
        assert_eq!(matches, vec![TupleId::new(1)]);
    }

    #[test]
    fn range_bounds_respect_inclusive_and_exclusive_edges() {
        let table = table_descriptor();
        let descriptor = index_descriptor();
        let rows = (1..=5).map(|i| (TupleId::new(i as u64), Row::new(vec![Value::Int(i)])));
        let index = IndexData::from_rows(&descriptor, &table, rows).unwrap();

        let inclusive = index
            .candidate_tuple_ids(&KeyRange {
                lower: Bound::Included(vec![Value::Int(2)]),
                upper: Bound::Included(vec![Value::Int(4)]),
            })
            .unwrap();
        assert_eq!(
            inclusive,
            vec![TupleId::new(2), TupleId::new(3), TupleId::new(4)]
        );

        let exclusive = index
            .candidate_tuple_ids(&KeyRange {
                lower: Bound::Excluded(vec![Value::Int(2)]),
                upper: Bound::Excluded(vec![Value::Int(4)]),
            })
            .unwrap();
        assert_eq!(exclusive, vec![TupleId::new(3)]);
    }

    #[test]
    fn composite_index_prefix_bound_matches_equal_prefix() {
        let table = table_descriptor_two_cols();
        let mut index = IndexData::new(composite_index_descriptor());
        index
            .insert_tuple(
                &table,
                TupleId::new(1),
                &Row::new(vec![Value::Int(9), Value::Int(123)]),
            )
            .unwrap();

        let matches = index
            .candidate_tuple_ids(&KeyRange {
                lower: Bound::Included(vec![Value::Int(9)]),
                upper: Bound::Included(vec![Value::Int(9)]),
            })
            .unwrap();
        assert_eq!(matches, vec![TupleId::new(1)]);
    }

    #[test]
    fn numeric_bound_matches_real_index_key() {
        let table = TableStorageDescriptor {
            table_id: RelationId::new(1),
            columns: vec![aiondb_storage_api::StorageColumn {
                column_id: ColumnId::new(1),
                data_type: DataType::Real,
                nullable: false,
            }],
            primary_key: None,
            shard_config: None,
        };
        let mut index = IndexData::new(index_descriptor());
        index
            .insert_tuple(&table, TupleId::new(1), &Row::new(vec![Value::Real(5.6)]))
            .unwrap();

        let matches = index
            .candidate_tuple_ids(&KeyRange {
                lower: Bound::Included(vec![Value::Numeric(aiondb_core::NumericValue::new(
                    214, 2,
                ))]),
                upper: Bound::Included(vec![Value::Numeric(aiondb_core::NumericValue::new(
                    815, 2,
                ))]),
            })
            .unwrap();
        assert_eq!(matches, vec![TupleId::new(1)]);
    }

    #[test]
    fn jsonb_object_key_order_is_canonicalized() {
        let left = Value::Jsonb(serde_json::json!({"a": 1, "b": [2, 3]}));
        let right = Value::Jsonb(serde_json::json!({"b": [2, 3], "a": 1}));

        assert_eq!(compare_values_total(&left, &right, false), Ordering::Equal);
    }

    #[test]
    fn jsonb_index_range_filters_exact_key() {
        let table = TableStorageDescriptor {
            table_id: RelationId::new(1),
            columns: vec![aiondb_storage_api::StorageColumn {
                column_id: ColumnId::new(1),
                data_type: DataType::Jsonb,
                nullable: false,
            }],
            primary_key: None,
            shard_config: None,
        };
        let descriptor = index_descriptor();
        let rows = vec![
            (
                TupleId::new(1),
                Row::new(vec![Value::Jsonb(serde_json::json!({"a": 1}))]),
            ),
            (
                TupleId::new(2),
                Row::new(vec![Value::Jsonb(serde_json::json!({"a": 2}))]),
            ),
        ];
        let index = IndexData::from_rows(&descriptor, &table, rows).unwrap();

        let matches = index
            .candidate_tuple_ids(&KeyRange {
                lower: Bound::Included(vec![Value::Jsonb(serde_json::json!({"a": 1}))]),
                upper: Bound::Included(vec![Value::Jsonb(serde_json::json!({"a": 1}))]),
            })
            .unwrap();

        assert_eq!(matches, vec![TupleId::new(1)]);
    }

    #[test]
    fn numeric_and_bigint_compare_without_f64_precision_loss() {
        let left = Value::Numeric(aiondb_core::NumericValue::new(9_007_199_254_740_993, 0));
        let right = Value::BigInt(9_007_199_254_740_992);

        assert_eq!(
            compare_values_total(&left, &right, false),
            Ordering::Greater
        );
        assert_eq!(compare_values_total(&right, &left, false), Ordering::Less);
    }

    #[test]
    fn numeric_values_compare_by_numeric_order_not_raw_scale() {
        let left = Value::Numeric(aiondb_core::NumericValue::new(1, 0));
        let right = Value::Numeric(aiondb_core::NumericValue::new(10, 1));

        assert_eq!(compare_values_total(&left, &right, false), Ordering::Equal);
    }
}
