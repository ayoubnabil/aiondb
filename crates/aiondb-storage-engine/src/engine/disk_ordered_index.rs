//! Experimental SQL-facing wrapper over the page-backed `DiskBTree`.
//!
//! This is the first integration layer between storage-engine index semantics
//! and the new persistent B+tree primitive in `aiondb-buffer-pool`.
//!
//! Scope of this slice:
//! - ordered single-column `BOOL`, `INT` and bounded `BIGINT` ranges;
//! - ordered single-column `TEXT` and `UUID` ranges on the variable-key path,
//!   with SQL row recheck and explicit `DESC` / `NULLS FIRST|LAST` handling;
//! - prefix-ordered single-column `TEXT` and `UUID` ranges on the fixed-key
//!   path with SQL row recheck;
//! - exact-match scalar keys for `BOOL`, `INT`, `BIGINT`, `TEXT`, `UUID` and
//!   composites of those types;
//! - duplicate values by packing `(logical key, tuple_id)` into the physical
//!   B+tree key;
//! - persistent pages through the shared buffer pool/page store.
//!
//! Variable-length composite keys are hashed into the physical key space and
//! therefore require a row recheck in the SQL scan path. Single-column `TEXT`
//! and `UUID` additionally retain a conservative fixed-key ordered-prefix path:
//! it never skips possible rows, but may return false positives that the SQL
//! layer rechecks. Text ordering is still bytewise here; full PostgreSQL
//! collation semantics require richer collation metadata plus a collation-aware
//! variable-length page key format.
#![allow(dead_code)]

use std::sync::Arc;

use aiondb_buffer_pool::{
    BufferPool, DiskBTree, DiskBTreeConfig, DiskVarBTree, DiskVarBTreeConfig, VarEntry,
};
use aiondb_core::{ColumnId, DataType, DbError, DbResult, Row, SqlState, TupleId, Value};
use aiondb_storage_api::{Bound, IndexStorageDescriptor, KeyRange, TableStorageDescriptor};

#[derive(Debug)]
pub(crate) struct DiskOrderedIntIndex {
    tree: DiskBTree,
}

pub(crate) struct DiskVarExactIndex {
    tree: DiskVarBTree,
}

impl std::fmt::Debug for DiskVarExactIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiskVarExactIndex").finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DiskOrderedScanMode {
    Ordered,
    OrderedWithRecheck,
    HashedExact,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DiskIndexRegistryPlan {
    pub build_fixed: bool,
    pub build_var: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DiskIndexLookupBackend {
    Fixed,
    Var,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DiskIndexLookupPlan {
    pub backend: DiskIndexLookupBackend,
    pub mode: DiskOrderedScanMode,
}

impl DiskOrderedIntIndex {
    pub(crate) fn open_or_create(pool: Arc<BufferPool>, relation_id: u64) -> DbResult<Self> {
        let tree = DiskBTree::open_or_create(pool, DiskBTreeConfig::new(relation_id))
            .map_err(map_btree_error)?;
        Ok(Self { tree })
    }

    pub(crate) fn insert(&self, value: &Value, tuple_id: TupleId) -> DbResult<()> {
        let key = pack_ordered_fixed_tuple_key(value, tuple_id)?;
        self.tree
            .insert(key, tuple_id.get())
            .map_err(map_btree_error)
    }

    pub(crate) fn insert_row(
        &self,
        descriptor: &IndexStorageDescriptor,
        table: &TableStorageDescriptor,
        row: &Row,
        tuple_id: TupleId,
    ) -> DbResult<()> {
        let values = key_values_for_row(descriptor, table, row)?;
        if values.iter().any(|value| matches!(value, Value::Null)) {
            return Ok(());
        }
        for key in pack_row_keys(descriptor, table, &values, tuple_id)? {
            self.tree
                .insert(key, tuple_id.get())
                .map_err(map_btree_error)?;
        }
        Ok(())
    }

    pub(crate) fn bulk_load_rows(
        &self,
        descriptor: &IndexStorageDescriptor,
        table: &TableStorageDescriptor,
        rows: impl IntoIterator<Item = (TupleId, Row)>,
    ) -> DbResult<()> {
        let owned: Vec<(TupleId, Row)> = rows.into_iter().collect();
        self.bulk_load_row_refs(descriptor, table, owned.iter().map(|(t, r)| (*t, r)))
    }

    pub(crate) fn bulk_load_row_refs<'a>(
        &self,
        descriptor: &IndexStorageDescriptor,
        table: &TableStorageDescriptor,
        rows: impl IntoIterator<Item = (TupleId, &'a Row)>,
    ) -> DbResult<()> {
        let mut entries = Vec::new();
        for (tuple_id, row) in rows {
            let values = key_values_for_row(descriptor, table, row)?;
            if values.iter().any(|value| matches!(value, Value::Null)) {
                continue;
            }
            for key in pack_row_keys(descriptor, table, &values, tuple_id)? {
                entries.push((key, tuple_id.get()));
            }
        }
        entries.sort_unstable_by_key(|(key, value)| (*key, *value));
        self.tree
            .bulk_load_sorted(&entries)
            .map_err(map_btree_error)
    }

    pub(crate) fn remove(&self, value: &Value, tuple_id: TupleId) -> DbResult<bool> {
        let key = pack_ordered_fixed_tuple_key(value, tuple_id)?;
        self.tree.delete(key).map_err(map_btree_error)
    }

    pub(crate) fn remove_row(
        &self,
        descriptor: &IndexStorageDescriptor,
        table: &TableStorageDescriptor,
        row: &Row,
        tuple_id: TupleId,
    ) -> DbResult<bool> {
        let values = key_values_for_row(descriptor, table, row)?;
        if values.iter().any(|value| matches!(value, Value::Null)) {
            return Ok(false);
        }
        let mut removed = false;
        for key in pack_row_keys(descriptor, table, &values, tuple_id)? {
            removed |= self.tree.delete(key).map_err(map_btree_error)?;
        }
        Ok(removed)
    }

    pub(crate) fn exact(&self, value: &Value) -> DbResult<Vec<TupleId>> {
        let (lower, upper) = packed_ordered_fixed_bounds(value)?;
        let rows = self
            .tree
            .range(Some(lower), Some(upper), None)
            .map_err(map_btree_error)?;
        Ok(rows
            .into_iter()
            .map(|(_, tuple_raw)| TupleId::new(tuple_raw))
            .collect())
    }

    pub(crate) fn range(
        &self,
        lower: Option<&Value>,
        upper: Option<&Value>,
        limit: Option<usize>,
    ) -> DbResult<Vec<TupleId>> {
        let lower = match lower {
            Some(value) => Some(packed_ordered_fixed_bounds(value)?.0),
            None => None,
        };
        let upper = match upper {
            Some(value) => Some(packed_ordered_fixed_bounds(value)?.1),
            None => None,
        };
        let rows = self
            .tree
            .range(lower, upper, limit)
            .map_err(map_btree_error)?;
        Ok(rows
            .into_iter()
            .map(|(_, tuple_raw)| TupleId::new(tuple_raw))
            .collect())
    }

    pub(crate) fn scan_key_range(
        &self,
        key_range: &KeyRange,
        limit: Option<usize>,
    ) -> DbResult<Vec<TupleId>> {
        self.scan_key_range_ordered(key_range, limit, false)
    }

    pub(crate) fn scan_key_range_ordered(
        &self,
        key_range: &KeyRange,
        limit: Option<usize>,
        descending: bool,
    ) -> DbResult<Vec<TupleId>> {
        let bounds = match packed_key_range_bounds(key_range)? {
            Some(bounds) => bounds,
            None => return Ok(Vec::new()),
        };
        let rows = if descending {
            self.tree
                .range_desc(bounds.lower, bounds.upper, limit)
                .map_err(map_btree_error)?
        } else {
            self.tree
                .range(bounds.lower, bounds.upper, limit)
                .map_err(map_btree_error)?
        };
        Ok(rows
            .into_iter()
            .map(|(_, tuple_raw)| TupleId::new(tuple_raw))
            .collect())
    }

    pub(crate) fn group_counts(
        &self,
        descriptor: &IndexStorageDescriptor,
        table: &TableStorageDescriptor,
        key_range: &KeyRange,
    ) -> DbResult<Option<Vec<(Value, u64)>>> {
        let Some(kind) = descriptor_ordered_fixed_kind(descriptor, table) else {
            return Ok(None);
        };
        let key_column_id = descriptor.key_columns[0].column_id;
        if table
            .columns
            .iter()
            .find(|column| column.column_id == key_column_id)
            .is_some_and(|column| column.nullable)
        {
            return Ok(None);
        }
        let bounds = match packed_key_range_bounds(key_range)? {
            Some(bounds) => bounds,
            None => return Ok(Some(Vec::new())),
        };
        let rows = self
            .tree
            .range(bounds.lower, bounds.upper, None)
            .map_err(map_btree_error)?;
        let mut groups = Vec::new();
        let mut current_prefix = None;
        let mut current_count = 0_u64;
        for (packed_key, _) in rows {
            let prefix = ordered_fixed_group_prefix(kind, packed_key);
            match current_prefix {
                Some(current) if current == prefix => {
                    current_count = current_count.saturating_add(1);
                }
                Some(current) => {
                    groups.push((
                        decode_ordered_fixed_group_prefix(kind, current)?,
                        current_count,
                    ));
                    current_prefix = Some(prefix);
                    current_count = 1;
                }
                None => {
                    current_prefix = Some(prefix);
                    current_count = 1;
                }
            }
        }
        if let Some(prefix) = current_prefix {
            groups.push((
                decode_ordered_fixed_group_prefix(kind, prefix)?,
                current_count,
            ));
        }
        Ok(Some(groups))
    }

    #[cfg(test)]
    pub(crate) fn stats(&self) -> DbResult<aiondb_buffer_pool::DiskBTreeStats> {
        self.tree.stats().map_err(map_btree_error)
    }

    pub(crate) fn flush(&self) -> DbResult<()> {
        self.tree.flush().map_err(map_btree_error)
    }
}

impl DiskVarExactIndex {
    pub(crate) fn open_or_create(pool: Arc<BufferPool>, relation_id: u64) -> DbResult<Self> {
        let tree = DiskVarBTree::open_or_create(pool, DiskVarBTreeConfig::new(relation_id))
            .map_err(map_btree_error)?;
        Ok(Self { tree })
    }

    pub(crate) fn bulk_load_rows(
        &self,
        descriptor: &IndexStorageDescriptor,
        table: &TableStorageDescriptor,
        rows: impl IntoIterator<Item = (TupleId, Row)>,
    ) -> DbResult<()> {
        let owned: Vec<(TupleId, Row)> = rows.into_iter().collect();
        self.bulk_load_row_refs(descriptor, table, owned.iter().map(|(t, r)| (*t, r)))
    }

    pub(crate) fn bulk_load_row_refs<'a>(
        &self,
        descriptor: &IndexStorageDescriptor,
        table: &TableStorageDescriptor,
        rows: impl IntoIterator<Item = (TupleId, &'a Row)>,
    ) -> DbResult<()> {
        let mut entries = Vec::new();
        for (tuple_id, row) in rows {
            let values = key_values_for_row(descriptor, table, row)?;
            entries.push(VarEntry {
                key: encode_var_key(values.iter().copied())?,
                value: tuple_id.get(),
            });
            if supports_var_ordered_range_descriptor(descriptor, table) {
                entries.push(VarEntry {
                    key: encode_ordered_var_key(descriptor, values.iter().copied())?,
                    value: tuple_id.get(),
                });
            }
        }
        entries.sort_unstable_by(|left, right| {
            (left.key.as_slice(), left.value).cmp(&(right.key.as_slice(), right.value))
        });
        self.tree
            .bulk_load_sorted(&entries)
            .map_err(map_btree_error)
    }

    pub(crate) fn insert_row(
        &self,
        descriptor: &IndexStorageDescriptor,
        table: &TableStorageDescriptor,
        row: &Row,
        tuple_id: TupleId,
    ) -> DbResult<()> {
        let values = key_values_for_row(descriptor, table, row)?;
        self.tree
            .insert(encode_var_key(values.iter().copied())?, tuple_id.get())
            .map_err(map_btree_error)?;
        if supports_var_ordered_range_descriptor(descriptor, table) {
            self.tree
                .insert(
                    encode_ordered_var_key(descriptor, values.iter().copied())?,
                    tuple_id.get(),
                )
                .map_err(map_btree_error)?;
        }
        Ok(())
    }

    pub(crate) fn remove_row(
        &self,
        descriptor: &IndexStorageDescriptor,
        table: &TableStorageDescriptor,
        row: &Row,
        tuple_id: TupleId,
    ) -> DbResult<bool> {
        let values = key_values_for_row(descriptor, table, row)?;
        let mut removed = self
            .tree
            .delete(&encode_var_key(values.iter().copied())?, tuple_id.get())
            .map_err(map_btree_error)?;
        if supports_var_ordered_range_descriptor(descriptor, table) {
            removed |= self
                .tree
                .delete(
                    &encode_ordered_var_key(descriptor, values.iter().copied())?,
                    tuple_id.get(),
                )
                .map_err(map_btree_error)?;
        }
        Ok(removed)
    }

    pub(crate) fn exact_values<'a>(
        &self,
        values: impl IntoIterator<Item = &'a Value>,
    ) -> DbResult<Vec<TupleId>> {
        Ok(self
            .tree
            .get_values(&encode_var_key(values)?)
            .map_err(map_btree_error)?
            .into_iter()
            .map(TupleId::new)
            .collect())
    }

    pub(crate) fn range_values(
        &self,
        descriptor: &IndexStorageDescriptor,
        key_range: &KeyRange,
    ) -> DbResult<Vec<TupleId>> {
        let bounds = ordered_var_bounds(descriptor, key_range)?;
        Ok(self
            .tree
            .range(bounds.lower.as_deref(), bounds.upper.as_deref(), None)
            .map_err(map_btree_error)?
            .into_iter()
            .map(|entry| TupleId::new(entry.value))
            .collect())
    }

    #[cfg(test)]
    pub(crate) fn stats(&self) -> DbResult<aiondb_buffer_pool::DiskVarBTreeStats> {
        self.tree.stats().map_err(map_btree_error)
    }
}

pub(crate) fn scan_mode(
    descriptor: &IndexStorageDescriptor,
    table: &TableStorageDescriptor,
    key_range: &KeyRange,
) -> Option<DiskOrderedScanMode> {
    lookup_plan(descriptor, table, key_range).map(|plan| plan.mode)
}

pub(crate) fn lookup_plan(
    descriptor: &IndexStorageDescriptor,
    table: &TableStorageDescriptor,
    key_range: &KeyRange,
) -> Option<DiskIndexLookupPlan> {
    let fixed_supported = supports_descriptor(descriptor, table);
    let var_exact_supported = supports_var_exact_descriptor(descriptor, table);
    let var_range_supported = supports_var_ordered_range_descriptor(descriptor, table);
    if let Some(kind) = fixed_supported
        .then(|| descriptor_ordered_fixed_kind(descriptor, table))
        .flatten()
    {
        if key_range_matches_ordered_fixed_kind(kind, key_range)
            && packed_key_range_bounds(key_range).ok().flatten().is_some()
        {
            return Some(DiskIndexLookupPlan {
                backend: DiskIndexLookupBackend::Fixed,
                mode: DiskOrderedScanMode::Ordered,
            });
        }
    }
    if var_exact_supported
        && exact_scalar_key_values(key_range)
            .is_some_and(|values| values_match_descriptor(descriptor, table, values))
    {
        return Some(DiskIndexLookupPlan {
            backend: DiskIndexLookupBackend::Var,
            mode: DiskOrderedScanMode::HashedExact,
        });
    }
    if var_range_supported
        && prefix_var_ordered_range(descriptor, table, key_range)
        && ordered_var_bounds(descriptor, key_range).is_ok()
    {
        return Some(DiskIndexLookupPlan {
            backend: DiskIndexLookupBackend::Var,
            mode: DiskOrderedScanMode::OrderedWithRecheck,
        });
    }
    if let Some(kind) = fixed_supported
        .then(|| descriptor_ordered_prefix_kind(descriptor, table))
        .flatten()
    {
        if key_range_matches_ordered_prefix_kind(kind, key_range)
            && packed_key_range_bounds(key_range).ok().flatten().is_some()
        {
            return Some(DiskIndexLookupPlan {
                backend: DiskIndexLookupBackend::Fixed,
                mode: DiskOrderedScanMode::OrderedWithRecheck,
            });
        }
    }
    None
}

pub(crate) fn registry_plan(
    descriptor: &IndexStorageDescriptor,
    table: &TableStorageDescriptor,
) -> DiskIndexRegistryPlan {
    DiskIndexRegistryPlan {
        build_fixed: supports_descriptor(descriptor, table),
        build_var: supports_var_exact_descriptor(descriptor, table),
    }
}

pub(crate) fn supports_descriptor(
    descriptor: &IndexStorageDescriptor,
    table: &TableStorageDescriptor,
) -> bool {
    if !descriptor_base_supported(descriptor, table) {
        return false;
    }
    if descriptor
        .key_columns
        .iter()
        .any(|column| column.nulls_first)
    {
        return false;
    }
    if let Some(kind) = descriptor_ordered_fixed_kind(descriptor, table) {
        return !descriptor.key_columns[0].nulls_first
            && table
                .columns
                .iter()
                .find(|column| column.column_id == descriptor.key_columns[0].column_id)
                .is_some_and(|column| {
                    !column.nullable
                        && matches!(
                            kind,
                            OrderedFixedKind::Boolean
                                | OrderedFixedKind::Int
                                | OrderedFixedKind::BigInt
                        )
                });
    }
    if descriptor_ordered_prefix_kind(descriptor, table).is_some() {
        return descriptor.key_columns.len() == 1
            && !descriptor.key_columns[0].descending
            && table
                .columns
                .iter()
                .find(|column| column.column_id == descriptor.key_columns[0].column_id)
                .is_some_and(|column| !column.nullable);
    }
    false
}

pub(crate) fn supports_var_exact_descriptor(
    descriptor: &IndexStorageDescriptor,
    table: &TableStorageDescriptor,
) -> bool {
    descriptor_base_supported(descriptor, table)
}

pub(crate) fn supports_var_ordered_range_descriptor(
    descriptor: &IndexStorageDescriptor,
    table: &TableStorageDescriptor,
) -> bool {
    descriptor_base_supported(descriptor, table)
}

pub(crate) fn can_fallback_to_logical_index(error: &DbError) -> bool {
    matches!(
        error.sqlstate(),
        SqlState::FeatureNotSupported | SqlState::ProgramLimitExceeded
    ) && (error.report().message.contains("disk ordered")
        || error.report().message.contains("disk var index"))
}

fn disk_scalar_type_supported(data_type: &DataType) -> bool {
    matches!(
        data_type,
        DataType::Boolean | DataType::Int | DataType::BigInt | DataType::Text | DataType::Uuid
    )
}

fn descriptor_base_supported(
    descriptor: &IndexStorageDescriptor,
    table: &TableStorageDescriptor,
) -> bool {
    if descriptor.gin
        || descriptor.hnsw_options.is_some()
        || descriptor.key_columns.is_empty()
        || !descriptor.include_columns.is_empty()
    {
        return false;
    }
    descriptor.key_columns.iter().all(|key_column| {
        table
            .columns
            .iter()
            .find(|column| column.column_id == key_column.column_id)
            .is_some_and(|column| disk_scalar_type_supported(&column.data_type))
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OrderedFixedKind {
    Boolean,
    Int,
    BigInt,
}

fn descriptor_ordered_fixed_kind(
    descriptor: &IndexStorageDescriptor,
    table: &TableStorageDescriptor,
) -> Option<OrderedFixedKind> {
    if descriptor.key_columns.len() != 1 {
        return None;
    }
    let key_col = descriptor.key_columns[0].column_id;
    table
        .columns
        .iter()
        .find(|column| column.column_id == key_col)
        .and_then(|column| match column.data_type {
            DataType::Boolean => Some(OrderedFixedKind::Boolean),
            DataType::Int => Some(OrderedFixedKind::Int),
            DataType::BigInt => Some(OrderedFixedKind::BigInt),
            _ => None,
        })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OrderedPrefixKind {
    Text,
    Uuid,
}

fn descriptor_ordered_prefix_kind(
    descriptor: &IndexStorageDescriptor,
    table: &TableStorageDescriptor,
) -> Option<OrderedPrefixKind> {
    if descriptor.key_columns.len() != 1 {
        return None;
    }
    let key_col = descriptor.key_columns[0].column_id;
    table
        .columns
        .iter()
        .find(|column| column.column_id == key_col)
        .and_then(|column| match column.data_type {
            DataType::Text => Some(OrderedPrefixKind::Text),
            DataType::Uuid => Some(OrderedPrefixKind::Uuid),
            _ => None,
        })
}

fn key_range_matches_ordered_fixed_kind(kind: OrderedFixedKind, key_range: &KeyRange) -> bool {
    bound_matches_ordered_fixed_kind(kind, &key_range.lower)
        && bound_matches_ordered_fixed_kind(kind, &key_range.upper)
}

fn bound_matches_ordered_fixed_kind(kind: OrderedFixedKind, bound: &Bound<Vec<Value>>) -> bool {
    match bound {
        Bound::Unbounded => true,
        Bound::Included(values) | Bound::Excluded(values) => matches!(
            (kind, values.as_slice()),
            (OrderedFixedKind::Boolean, [Value::Boolean(_)])
                | (OrderedFixedKind::Int, [Value::Int(_)])
                | (OrderedFixedKind::BigInt, [Value::BigInt(_)])
        ),
    }
}

fn key_range_matches_ordered_prefix_kind(kind: OrderedPrefixKind, key_range: &KeyRange) -> bool {
    bound_matches_ordered_prefix_kind(kind, &key_range.lower)
        && bound_matches_ordered_prefix_kind(kind, &key_range.upper)
}

fn bound_matches_ordered_prefix_kind(kind: OrderedPrefixKind, bound: &Bound<Vec<Value>>) -> bool {
    match bound {
        Bound::Unbounded => true,
        Bound::Included(values) | Bound::Excluded(values) => {
            matches!(
                (kind, values.as_slice()),
                (OrderedPrefixKind::Text, [Value::Text(_)])
                    | (OrderedPrefixKind::Uuid, [Value::Uuid(_)])
            )
        }
    }
}

fn prefix_var_ordered_range(
    descriptor: &IndexStorageDescriptor,
    table: &TableStorageDescriptor,
    key_range: &KeyRange,
) -> bool {
    bound_matches_prefix_var_range(descriptor, table, &key_range.lower)
        && bound_matches_prefix_var_range(descriptor, table, &key_range.upper)
}

fn bound_matches_prefix_var_range(
    descriptor: &IndexStorageDescriptor,
    table: &TableStorageDescriptor,
    bound: &Bound<Vec<Value>>,
) -> bool {
    match bound {
        Bound::Unbounded => true,
        Bound::Included(values) | Bound::Excluded(values) => {
            (1..=descriptor.key_columns.len()).contains(&values.len())
                && values_match_key_prefix_descriptor(descriptor, table, values)
                && !values.iter().any(|value| matches!(value, Value::Null))
        }
    }
}

fn key_values_for_row<'a>(
    descriptor: &IndexStorageDescriptor,
    table: &TableStorageDescriptor,
    row: &'a Row,
) -> DbResult<Vec<&'a Value>> {
    if !descriptor_base_supported(descriptor, table) {
        return Err(DbError::feature_not_supported(
            "disk ordered index supports scalar BOOL/INT/BIGINT/TEXT/UUID keys only",
        ));
    }
    descriptor
        .key_columns
        .iter()
        .map(|key_column| {
            let key_col: ColumnId = key_column.column_id;
            let ordinal = table
                .columns
                .iter()
                .position(|column| column.column_id == key_col)
                .ok_or_else(|| {
                    DbError::internal("disk ordered index key references unknown column")
                })?;
            row.values
                .get(ordinal)
                .ok_or_else(|| DbError::internal("row is missing disk ordered index key value"))
        })
        .collect()
}

fn pack_row_keys(
    descriptor: &IndexStorageDescriptor,
    table: &TableStorageDescriptor,
    values: &[&Value],
    tuple_id: TupleId,
) -> DbResult<Vec<u64>> {
    if descriptor_ordered_fixed_kind(descriptor, table).is_some() {
        return Ok(vec![pack_ordered_fixed_tuple_key(values[0], tuple_id)?]);
    }
    if descriptor_ordered_prefix_kind(descriptor, table).is_some() {
        return Ok(vec![
            pack_ordered_prefix_tuple_key(values[0], tuple_id)?,
            pack_namespaced_hash_tuple_key(values.iter().copied(), tuple_id)?,
        ]);
    }
    Ok(vec![pack_hash_tuple_key(values.iter().copied(), tuple_id)?])
}

fn pack_ordered_fixed_tuple_key(value: &Value, tuple_id: TupleId) -> DbResult<u64> {
    match value {
        Value::Boolean(_) => {
            let (lower, _) = packed_ordered_fixed_bounds(value)?;
            let tuple_low = u32::try_from(tuple_id.get()).map_err(|_| {
                DbError::program_limit(
                    "disk ordered BOOL index currently supports tuple ids <= u32::MAX",
                )
            })?;
            Ok(lower | u64::from(tuple_low))
        }
        Value::Int(_) => {
            let (lower, _) = packed_ordered_fixed_bounds(value)?;
            let tuple_low = u32::try_from(tuple_id.get()).map_err(|_| {
                DbError::program_limit(
                    "disk ordered INT index currently supports tuple ids <= u32::MAX",
                )
            })?;
            Ok(lower | u64::from(tuple_low))
        }
        Value::BigInt(_) => {
            let (lower, _) = packed_ordered_fixed_bounds(value)?;
            let tuple_low = u16::try_from(tuple_id.get()).map_err(|_| {
                DbError::program_limit(
                    "disk ordered BIGINT index currently supports tuple ids <= u16::MAX",
                )
            })?;
            Ok(lower | u64::from(tuple_low))
        }
        _ => Err(DbError::feature_not_supported(
            "disk ordered fixed-key index supports BOOL, INT and bounded BIGINT keys only",
        )),
    }
}

fn pack_ordered_prefix_tuple_key(value: &Value, tuple_id: TupleId) -> DbResult<u64> {
    const TUPLE_MASK: u64 = (1_u64 << 24) - 1;
    let tuple_low = tuple_id.get();
    if tuple_low > TUPLE_MASK {
        return Err(DbError::program_limit(
            "disk ordered prefix index currently supports tuple ids <= 2^24-1",
        ));
    }
    Ok(PREFIX_NAMESPACE | (value_ordered_prefix(value)? << 24) | tuple_low)
}

fn pack_hash_tuple_key<'a>(
    values: impl IntoIterator<Item = &'a Value>,
    tuple_id: TupleId,
) -> DbResult<u64> {
    let tuple_low = u32::try_from(tuple_id.get()).map_err(|_| {
        DbError::program_limit("disk ordered hashed index currently supports tuple ids <= u32::MAX")
    })?;
    let mut hash = Fnv1a64::new();
    for value in values {
        hash.write_u8(value_type_tag(value));
        hash_value(&mut hash, value)?;
        hash.write_u8(0xFF);
    }
    let prefix =
        u32::try_from(hash.finish() & u64::from(u32::MAX)).unwrap_or(u32::MAX) ^ 0x8000_0000;
    Ok((u64::from(prefix) << 32) | u64::from(tuple_low))
}

fn pack_namespaced_hash_tuple_key<'a>(
    values: impl IntoIterator<Item = &'a Value>,
    tuple_id: TupleId,
) -> DbResult<u64> {
    Ok(HASH_NAMESPACE | (pack_hash_tuple_key(values, tuple_id)? & HASH_PAYLOAD_MASK))
}

fn packed_ordered_fixed_bounds(value: &Value) -> DbResult<(u64, u64)> {
    match value {
        Value::Boolean(raw) => {
            let prefix = u64::from(u32::from(*raw)) << 32;
            Ok((prefix, prefix | u64::from(u32::MAX)))
        }
        Value::Int(raw) => {
            // Flip the sign bit so signed i32 order maps to unsigned order.
            let ordered = raw.cast_unsigned() ^ 0x8000_0000;
            let prefix = u64::from(ordered) << 32;
            Ok((prefix, prefix | u64::from(u32::MAX)))
        }
        Value::BigInt(raw) => {
            // Until variable-length page keys land, BIGINT ranges use a
            // bounded signed 48-bit value prefix plus a 16-bit tuple-id
            // suffix. Out-of-window values fall back to IndexData.
            const MIN: i64 = -(1_i64 << 47);
            const MAX: i64 = (1_i64 << 47) - 1;
            if !(MIN..=MAX).contains(raw) {
                return Err(DbError::feature_not_supported(
                    "disk ordered BIGINT range currently supports signed 48-bit key values",
                ));
            }
            let normalized = (*raw - MIN).cast_unsigned();
            let prefix = normalized << 16;
            Ok((prefix, prefix | u64::from(u16::MAX)))
        }
        _ => Err(DbError::feature_not_supported(
            "disk ordered index currently supports single-column BOOL/INT/BIGINT keys only",
        )),
    }
}

fn ordered_fixed_group_prefix(kind: OrderedFixedKind, packed_key: u64) -> u64 {
    match kind {
        OrderedFixedKind::Boolean | OrderedFixedKind::Int => packed_key >> 32,
        OrderedFixedKind::BigInt => packed_key >> 16,
    }
}

fn decode_ordered_fixed_group_prefix(kind: OrderedFixedKind, prefix: u64) -> DbResult<Value> {
    match kind {
        OrderedFixedKind::Boolean => Ok(Value::Boolean(prefix != 0)),
        OrderedFixedKind::Int => {
            let ordered = u32::try_from(prefix)
                .map_err(|_| DbError::internal("disk ordered INT index key prefix out of range"))?;
            Ok(Value::Int((ordered ^ 0x8000_0000).cast_signed()))
        }
        OrderedFixedKind::BigInt => {
            const MIN: i64 = -(1_i64 << 47);
            let normalized = i64::try_from(prefix).map_err(|_| {
                DbError::internal("disk ordered BIGINT index key prefix out of range")
            })?;
            Ok(Value::BigInt(normalized + MIN))
        }
    }
}

struct PackedKeyRangeBounds {
    lower: Option<u64>,
    upper: Option<u64>,
}

fn packed_key_range_bounds(key_range: &KeyRange) -> DbResult<Option<PackedKeyRangeBounds>> {
    if let Some(values) = exact_scalar_key_values(key_range) {
        if !matches!(
            values,
            [Value::Boolean(_) | Value::Int(_) | Value::BigInt(_)]
        ) {
            return Ok(Some(packed_hash_exact_bounds(values)?));
        }
    }

    if key_range_matches_ordered_prefix_kind(OrderedPrefixKind::Text, key_range)
        || key_range_matches_ordered_prefix_kind(OrderedPrefixKind::Uuid, key_range)
    {
        return packed_ordered_prefix_range_bounds(key_range);
    }

    let lower = match &key_range.lower {
        Bound::Unbounded => None,
        Bound::Included(values) => {
            let value = single_key_range_ordered_fixed(values)?;
            Some(packed_ordered_fixed_bounds(value)?.0)
        }
        Bound::Excluded(values) => {
            let Some(value) = next_ordered_fixed(single_key_range_ordered_fixed(values)?)? else {
                return Ok(None);
            };
            Some(packed_ordered_fixed_bounds(&value)?.0)
        }
    };
    let upper = match &key_range.upper {
        Bound::Unbounded => None,
        Bound::Included(values) => {
            let value = single_key_range_ordered_fixed(values)?;
            Some(packed_ordered_fixed_bounds(value)?.1)
        }
        Bound::Excluded(values) => {
            let Some(value) = previous_ordered_fixed(single_key_range_ordered_fixed(values)?)?
            else {
                return Ok(None);
            };
            Some(packed_ordered_fixed_bounds(&value)?.1)
        }
    };

    if lower.zip(upper).is_some_and(|(lower, upper)| lower > upper) {
        return Ok(None);
    }
    Ok(Some(PackedKeyRangeBounds { lower, upper }))
}

fn packed_ordered_prefix_range_bounds(
    key_range: &KeyRange,
) -> DbResult<Option<PackedKeyRangeBounds>> {
    let lower = match &key_range.lower {
        Bound::Unbounded => None,
        Bound::Included(values) | Bound::Excluded(values) => {
            let value = single_key_range_ordered_prefix(values)?;
            Some(packed_ordered_prefix_bounds(value)?.0)
        }
    };
    let upper = match &key_range.upper {
        Bound::Unbounded => None,
        Bound::Included(values) | Bound::Excluded(values) => {
            let value = single_key_range_ordered_prefix(values)?;
            Some(packed_ordered_prefix_bounds(value)?.1)
        }
    };

    if lower.zip(upper).is_some_and(|(lower, upper)| lower > upper) {
        return Ok(None);
    }
    Ok(Some(PackedKeyRangeBounds { lower, upper }))
}

fn packed_ordered_prefix_bounds(value: &Value) -> DbResult<(u64, u64)> {
    const TUPLE_MASK: u64 = (1_u64 << 24) - 1;
    let prefix = PREFIX_NAMESPACE | (value_ordered_prefix(value)? << 24);
    Ok((prefix, prefix | TUPLE_MASK))
}

fn value_ordered_prefix(value: &Value) -> DbResult<u64> {
    match value {
        Value::Text(text) => Ok(bytes_ordered_prefix(text.as_bytes())),
        Value::Uuid(bytes) => Ok(bytes_ordered_prefix(bytes)),
        _ => Err(DbError::feature_not_supported(
            "disk ordered prefix index supports TEXT and UUID key ranges only",
        )),
    }
}

const PREFIX_NAMESPACE: u64 = 0;
const HASH_NAMESPACE: u64 = 1_u64 << 63;
const HASH_PAYLOAD_MASK: u64 = (1_u64 << 63) - 1;
const ORDERED_PREFIX_MASK: u64 = (1_u64 << 39) - 1;

fn bytes_ordered_prefix(raw: &[u8]) -> u64 {
    let mut bytes = [0_u8; 5];
    for (slot, byte) in bytes.iter_mut().zip(raw) {
        *slot = *byte;
    }
    bytes
        .into_iter()
        .fold(0_u64, |acc, byte| (acc << 8) | u64::from(byte))
        >> 1
        & ORDERED_PREFIX_MASK
}

fn single_key_range_ordered_prefix(values: &[Value]) -> DbResult<&Value> {
    match values {
        [value @ (Value::Text(_) | Value::Uuid(_))] => Ok(value),
        _ => Err(DbError::feature_not_supported(
            "disk ordered prefix index supports single-column TEXT/UUID key ranges only",
        )),
    }
}

fn single_key_range_ordered_fixed(values: &[Value]) -> DbResult<&Value> {
    match values {
        [value @ (Value::Boolean(_) | Value::Int(_) | Value::BigInt(_))] => Ok(value),
        _ => Err(DbError::feature_not_supported(
            "disk ordered index currently supports single-column BOOL/INT/BIGINT key ranges only",
        )),
    }
}

fn next_ordered_fixed(value: &Value) -> DbResult<Option<Value>> {
    match value {
        Value::Boolean(false) => Ok(Some(Value::Boolean(true))),
        Value::Boolean(true) => Ok(None),
        Value::Int(value) => Ok(value.checked_add(1).map(Value::Int)),
        Value::BigInt(value) => Ok(value.checked_add(1).map(Value::BigInt)),
        _ => Err(DbError::feature_not_supported(
            "disk ordered index currently supports single-column BOOL/INT/BIGINT key ranges only",
        )),
    }
}

fn previous_ordered_fixed(value: &Value) -> DbResult<Option<Value>> {
    match value {
        Value::Boolean(true) => Ok(Some(Value::Boolean(false))),
        Value::Boolean(false) => Ok(None),
        Value::Int(value) => Ok(value.checked_sub(1).map(Value::Int)),
        Value::BigInt(value) => Ok(value.checked_sub(1).map(Value::BigInt)),
        _ => Err(DbError::feature_not_supported(
            "disk ordered index currently supports single-column BOOL/INT/BIGINT key ranges only",
        )),
    }
}

pub(crate) fn exact_scalar_key_values(key_range: &KeyRange) -> Option<&[Value]> {
    let Bound::Included(lower) = &key_range.lower else {
        return None;
    };
    let Bound::Included(upper) = &key_range.upper else {
        return None;
    };
    (lower == upper && lower.iter().all(value_supported)).then_some(lower.as_slice())
}

pub(crate) fn exact_point_key_range_for_row(
    descriptor: &IndexStorageDescriptor,
    table: &TableStorageDescriptor,
    row: &Row,
) -> DbResult<Option<KeyRange>> {
    if !descriptor_base_supported(descriptor, table) {
        return Ok(None);
    }
    let values = key_values_for_row(descriptor, table, row)?;
    if values.iter().any(|value| matches!(value, Value::Null)) {
        return Ok(None);
    }
    Ok(Some(KeyRange::point(
        values.into_iter().cloned().collect::<Vec<_>>(),
    )))
}

fn values_match_descriptor(
    descriptor: &IndexStorageDescriptor,
    table: &TableStorageDescriptor,
    values: &[Value],
) -> bool {
    values.len() == descriptor.key_columns.len()
        && descriptor
            .key_columns
            .iter()
            .zip(values)
            .all(|(key_column, value)| {
                table
                    .columns
                    .iter()
                    .find(|column| column.column_id == key_column.column_id)
                    .is_some_and(|column| value_matches_type(value, &column.data_type))
            })
}

fn values_match_key_prefix_descriptor(
    descriptor: &IndexStorageDescriptor,
    table: &TableStorageDescriptor,
    values: &[Value],
) -> bool {
    values.len() <= descriptor.key_columns.len()
        && descriptor
            .key_columns
            .iter()
            .take(values.len())
            .zip(values)
            .all(|(key_column, value)| {
                table
                    .columns
                    .iter()
                    .find(|column| column.column_id == key_column.column_id)
                    .is_some_and(|column| value_matches_type(value, &column.data_type))
            })
}

fn value_supported(value: &Value) -> bool {
    matches!(
        value,
        Value::Boolean(_) | Value::Int(_) | Value::BigInt(_) | Value::Text(_) | Value::Uuid(_)
    )
}

fn value_matches_type(value: &Value, data_type: &DataType) -> bool {
    matches!(
        (value, data_type),
        (Value::Boolean(_), DataType::Boolean)
            | (Value::Int(_), DataType::Int)
            | (Value::BigInt(_), DataType::BigInt)
            | (Value::Text(_), DataType::Text)
            | (Value::Uuid(_), DataType::Uuid)
    )
}

fn packed_hash_exact_bounds(values: &[Value]) -> DbResult<PackedKeyRangeBounds> {
    let use_namespaced_hash = matches!(values, [Value::Text(_) | Value::Uuid(_)]);
    let lower = if use_namespaced_hash {
        pack_namespaced_hash_tuple_key(values.iter(), TupleId::new(0))?
    } else {
        pack_hash_tuple_key(values.iter(), TupleId::new(0))?
    };
    let upper = if use_namespaced_hash {
        pack_namespaced_hash_tuple_key(values.iter(), TupleId::new(u64::from(u32::MAX)))?
    } else {
        pack_hash_tuple_key(values.iter(), TupleId::new(u64::from(u32::MAX)))?
    };
    Ok(PackedKeyRangeBounds {
        lower: Some(lower),
        upper: Some(upper),
    })
}

struct OrderedVarBounds {
    lower: Option<Vec<u8>>,
    upper: Option<Vec<u8>>,
}

fn ordered_var_bounds(
    descriptor: &IndexStorageDescriptor,
    key_range: &KeyRange,
) -> DbResult<OrderedVarBounds> {
    let lower = match &key_range.lower {
        Bound::Unbounded => Some(vec![0x02]),
        Bound::Included(values) | Bound::Excluded(values) => {
            Some(encode_ordered_var_bound(descriptor, values.iter(), false)?)
        }
    };
    let upper = match &key_range.upper {
        Bound::Unbounded => Some(vec![0x02, 0xFF]),
        Bound::Included(values) | Bound::Excluded(values) => {
            Some(encode_ordered_var_bound(descriptor, values.iter(), true)?)
        }
    };
    Ok(OrderedVarBounds { lower, upper })
}

fn encode_var_key<'a>(values: impl IntoIterator<Item = &'a Value>) -> DbResult<Vec<u8>> {
    let mut out = vec![0x01];
    for value in values {
        out.push(value_type_tag(value));
        match value {
            Value::Null => {}
            Value::Boolean(value) => out.push(u8::from(*value)),
            Value::Int(value) => {
                out.extend_from_slice(&((value.cast_unsigned() ^ 0x8000_0000).to_be_bytes()))
            }
            Value::BigInt(value) => {
                out.extend_from_slice(
                    &((value.cast_unsigned() ^ 0x8000_0000_0000_0000).to_be_bytes()),
                );
            }
            Value::Text(value) => {
                let bytes = value.as_bytes();
                let len = u32::try_from(bytes.len()).map_err(|_| {
                    DbError::program_limit("disk var index TEXT key component exceeds u32 length")
                })?;
                out.extend_from_slice(&len.to_be_bytes());
                out.extend_from_slice(bytes);
            }
            Value::Uuid(value) => out.extend_from_slice(value),
            _ => {
                return Err(DbError::feature_not_supported(
                    "disk var index supports BOOL/INT/BIGINT/TEXT/UUID keys only",
                ));
            }
        }
        out.push(0);
    }
    Ok(out)
}

fn encode_ordered_var_key<'a>(
    descriptor: &IndexStorageDescriptor,
    values: impl IntoIterator<Item = &'a Value>,
) -> DbResult<Vec<u8>> {
    encode_ordered_var_bound(descriptor, values, false)
}

fn encode_ordered_var_bound<'a>(
    descriptor: &IndexStorageDescriptor,
    values: impl IntoIterator<Item = &'a Value>,
    upper_prefix: bool,
) -> DbResult<Vec<u8>> {
    let mut out = vec![0x02];
    for (key_column, value) in descriptor.key_columns.iter().zip(values) {
        let mut component = Vec::new();
        encode_ordered_var_component(&mut component, value, key_column.nulls_first)?;
        if key_column.descending {
            for byte in &mut component {
                *byte = !*byte;
            }
            out.extend_from_slice(&component);
            out.push(0xFF);
        } else {
            out.extend_from_slice(&component);
            out.push(0x00);
        }
    }
    if upper_prefix {
        out.push(0xFF);
    }
    Ok(out)
}

fn encode_ordered_var_component(
    out: &mut Vec<u8>,
    value: &Value,
    nulls_first: bool,
) -> DbResult<()> {
    match value {
        Value::Null => out.push(if nulls_first { 0x00 } else { 0xFE }),
        Value::Boolean(value) => {
            out.push(0x10);
            out.push(u8::from(*value));
        }
        Value::Int(value) => {
            out.push(0x11);
            out.extend_from_slice(&((value.cast_unsigned() ^ 0x8000_0000).to_be_bytes()))
        }
        Value::BigInt(value) => {
            out.push(0x12);
            out.extend_from_slice(&((value.cast_unsigned() ^ 0x8000_0000_0000_0000).to_be_bytes()));
        }
        Value::Text(value) => {
            if value.as_bytes().contains(&0) {
                return Err(DbError::feature_not_supported(
                    "disk var ordered TEXT range keys do not support embedded NUL bytes",
                ));
            }
            out.push(0x13);
            out.extend_from_slice(value.as_bytes());
        }
        Value::Uuid(value) => {
            out.push(0x14);
            out.extend_from_slice(value);
        }
        _ => {
            return Err(DbError::feature_not_supported(
                "disk var ordered range index supports BOOL/INT/BIGINT/TEXT/UUID keys only",
            ));
        }
    }
    Ok(())
}

fn hash_value(hash: &mut Fnv1a64, value: &Value) -> DbResult<()> {
    match value {
        Value::Boolean(value) => hash.write_u8(u8::from(*value)),
        Value::Int(value) => hash.write_bytes(&value.to_be_bytes()),
        Value::BigInt(value) => hash.write_bytes(&value.to_be_bytes()),
        Value::Text(value) => hash.write_bytes(value.as_bytes()),
        Value::Uuid(value) => hash.write_bytes(value),
        _ => {
            return Err(DbError::feature_not_supported(
                "disk ordered hashed index supports BOOL/INT/BIGINT/TEXT/UUID keys only",
            ));
        }
    }
    Ok(())
}

fn value_type_tag(value: &Value) -> u8 {
    match value {
        Value::Boolean(_) => 1,
        Value::Int(_) => 2,
        Value::BigInt(_) => 3,
        Value::Text(_) => 4,
        Value::Uuid(_) => 5,
        _ => 0,
    }
}

struct Fnv1a64(u64);

impl Fnv1a64 {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    fn write_u8(&mut self, byte: u8) {
        self.0 ^= u64::from(byte);
        self.0 = self.0.wrapping_mul(0x0000_0100_0000_01B3);
    }

    fn write_bytes(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.write_u8(*byte);
        }
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

fn map_btree_error(error: aiondb_buffer_pool::BufferPoolError) -> DbError {
    DbError::storage_error(aiondb_core::SqlState::InternalError, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_buffer_pool::MemoryPageStore;

    fn index() -> (Arc<MemoryPageStore>, DiskOrderedIntIndex) {
        let store = Arc::new(MemoryPageStore::new());
        let pool = Arc::new(BufferPool::new(64, store.clone()));
        let index = DiskOrderedIntIndex::open_or_create(pool, 9001).unwrap();
        (store, index)
    }

    fn var_index() -> (Arc<MemoryPageStore>, DiskVarExactIndex) {
        let store = Arc::new(MemoryPageStore::new());
        let pool = Arc::new(BufferPool::new(64, store.clone()));
        let index = DiskVarExactIndex::open_or_create(pool, 9002).unwrap();
        (store, index)
    }

    fn text_table_and_index() -> (TableStorageDescriptor, IndexStorageDescriptor) {
        let table = TableStorageDescriptor {
            table_id: aiondb_core::RelationId::new(2),
            columns: vec![aiondb_storage_api::StorageColumn {
                column_id: ColumnId::new(1),
                data_type: DataType::Text,
                nullable: false,
            }],
            primary_key: None,
            shard_config: None,
        };
        let index = IndexStorageDescriptor {
            index_id: aiondb_core::IndexId::new(2),
            table_id: table.table_id,
            unique: false,
            nulls_not_distinct: false,
            gin: false,
            key_columns: vec![aiondb_storage_api::IndexKeyColumn {
                column_id: ColumnId::new(1),
                descending: false,
                nulls_first: false,
            }],
            include_columns: Vec::new(),
            hnsw_options: None,
            ivf_flat_options: None,
        };
        (table, index)
    }

    fn int_table_and_index(
        descending: bool,
        nulls_first: bool,
    ) -> (TableStorageDescriptor, IndexStorageDescriptor) {
        let table = TableStorageDescriptor {
            table_id: aiondb_core::RelationId::new(3),
            columns: vec![aiondb_storage_api::StorageColumn {
                column_id: ColumnId::new(1),
                data_type: DataType::Int,
                nullable: false,
            }],
            primary_key: None,
            shard_config: None,
        };
        let index = IndexStorageDescriptor {
            index_id: aiondb_core::IndexId::new(3),
            table_id: table.table_id,
            unique: false,
            nulls_not_distinct: false,
            gin: false,
            key_columns: vec![aiondb_storage_api::IndexKeyColumn {
                column_id: ColumnId::new(1),
                descending,
                nulls_first,
            }],
            include_columns: Vec::new(),
            hnsw_options: None,
            ivf_flat_options: None,
        };
        (table, index)
    }

    fn nullable_text_table_and_index(
        descending: bool,
        nulls_first: bool,
    ) -> (TableStorageDescriptor, IndexStorageDescriptor) {
        let table = TableStorageDescriptor {
            table_id: aiondb_core::RelationId::new(4),
            columns: vec![aiondb_storage_api::StorageColumn {
                column_id: ColumnId::new(1),
                data_type: DataType::Text,
                nullable: true,
            }],
            primary_key: None,
            shard_config: None,
        };
        let index = IndexStorageDescriptor {
            index_id: aiondb_core::IndexId::new(4),
            table_id: table.table_id,
            unique: false,
            nulls_not_distinct: false,
            gin: false,
            key_columns: vec![aiondb_storage_api::IndexKeyColumn {
                column_id: ColumnId::new(1),
                descending,
                nulls_first,
            }],
            include_columns: Vec::new(),
            hnsw_options: None,
            ivf_flat_options: None,
        };
        (table, index)
    }

    #[test]
    fn exact_lookup_returns_duplicate_tuple_ids_in_key_order() {
        let (_store, index) = index();
        index.insert(&Value::Int(7), TupleId::new(3)).unwrap();
        index.insert(&Value::Int(7), TupleId::new(1)).unwrap();
        index.insert(&Value::Int(8), TupleId::new(2)).unwrap();

        assert_eq!(
            index.exact(&Value::Int(7)).unwrap(),
            vec![TupleId::new(1), TupleId::new(3)]
        );
    }

    #[test]
    fn var_exact_index_serves_text_exact_lookup_with_duplicates() {
        let (_store, index) = var_index();
        let (table, descriptor) = text_table_and_index();
        index
            .bulk_load_rows(
                &descriptor,
                &table,
                (0..1_200)
                    .map(|idx| {
                        (
                            TupleId::new(idx + 1),
                            Row::new(vec![Value::Text(format!("k{idx:04}"))]),
                        )
                    })
                    .chain([
                        (
                            TupleId::new(2_001),
                            Row::new(vec![Value::Text("beta".to_owned())]),
                        ),
                        (
                            TupleId::new(2_002),
                            Row::new(vec![Value::Text("alpha".to_owned())]),
                        ),
                        (
                            TupleId::new(2_003),
                            Row::new(vec![Value::Text("beta".to_owned())]),
                        ),
                    ]),
            )
            .unwrap();

        assert!(index.stats().unwrap().internal_pages > 0);
        assert_eq!(
            index
                .exact_values([&Value::Text("beta".to_owned())])
                .unwrap(),
            vec![TupleId::new(2_001), TupleId::new(2_003)]
        );
        assert!(index
            .exact_values([&Value::Text("missing".to_owned())])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn var_exact_index_updates_text_entries() {
        let (_store, index) = var_index();
        let (table, descriptor) = text_table_and_index();
        let row = Row::new(vec![Value::Text("beta".to_owned())]);
        let tuple_id = TupleId::new(9);

        index
            .insert_row(&descriptor, &table, &row, tuple_id)
            .unwrap();
        assert_eq!(
            index
                .exact_values([&Value::Text("beta".to_owned())])
                .unwrap(),
            vec![tuple_id]
        );
        assert!(index
            .remove_row(&descriptor, &table, &row, tuple_id)
            .unwrap());
        assert!(index
            .exact_values([&Value::Text("beta".to_owned())])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn var_ordered_range_respects_descending_int_index_order() {
        let (_store, index) = var_index();
        let (table, descriptor) = int_table_and_index(true, false);
        index
            .bulk_load_rows(
                &descriptor,
                &table,
                [
                    (TupleId::new(1), Row::new(vec![Value::Int(1)])),
                    (TupleId::new(2), Row::new(vec![Value::Int(3)])),
                    (TupleId::new(3), Row::new(vec![Value::Int(2)])),
                ],
            )
            .unwrap();

        assert_eq!(
            index.range_values(&descriptor, &KeyRange::full()).unwrap(),
            vec![TupleId::new(2), TupleId::new(3), TupleId::new(1)]
        );
    }

    #[test]
    fn var_ordered_range_respects_nulls_last_for_text() {
        let (_store, index) = var_index();
        let (table, descriptor) = nullable_text_table_and_index(false, false);
        index
            .bulk_load_rows(
                &descriptor,
                &table,
                [
                    (TupleId::new(1), Row::new(vec![Value::Null])),
                    (
                        TupleId::new(2),
                        Row::new(vec![Value::Text("alpha".to_owned())]),
                    ),
                    (
                        TupleId::new(3),
                        Row::new(vec![Value::Text("beta".to_owned())]),
                    ),
                ],
            )
            .unwrap();

        assert_eq!(
            index.range_values(&descriptor, &KeyRange::full()).unwrap(),
            vec![TupleId::new(2), TupleId::new(3), TupleId::new(1)]
        );
    }

    #[test]
    fn var_ordered_range_respects_nulls_first_for_text() {
        let (_store, index) = var_index();
        let (table, descriptor) = nullable_text_table_and_index(false, true);
        index
            .bulk_load_rows(
                &descriptor,
                &table,
                [
                    (TupleId::new(1), Row::new(vec![Value::Null])),
                    (
                        TupleId::new(2),
                        Row::new(vec![Value::Text("alpha".to_owned())]),
                    ),
                    (
                        TupleId::new(3),
                        Row::new(vec![Value::Text("beta".to_owned())]),
                    ),
                ],
            )
            .unwrap();

        assert_eq!(
            index.range_values(&descriptor, &KeyRange::full()).unwrap(),
            vec![TupleId::new(1), TupleId::new(2), TupleId::new(3)]
        );
    }

    #[test]
    fn range_lookup_preserves_signed_int_order() {
        let (_store, index) = index();
        for (value, tuple) in [(-10, 1), (0, 2), (5, 3), (9, 4), (100, 5)] {
            index
                .insert(&Value::Int(value), TupleId::new(tuple))
                .unwrap();
        }

        assert_eq!(
            index
                .range(Some(&Value::Int(-1)), Some(&Value::Int(10)), None)
                .unwrap(),
            vec![TupleId::new(2), TupleId::new(3), TupleId::new(4)]
        );
    }

    #[test]
    fn range_lookup_preserves_bounded_bigint_order() {
        let (_store, index) = index();
        for (value, tuple) in [(-9_000_000_000_i64, 1), (-1, 2), (0, 3), (9_000_000_000, 4)] {
            index
                .insert(&Value::BigInt(value), TupleId::new(tuple))
                .unwrap();
        }

        assert_eq!(
            index
                .range(Some(&Value::BigInt(-10)), Some(&Value::BigInt(10)), None)
                .unwrap(),
            vec![TupleId::new(2), TupleId::new(3)]
        );
    }

    #[test]
    fn bounded_bigint_range_rejects_values_outside_physical_key_window() {
        let (_store, index) = index();
        let err = index
            .insert(&Value::BigInt(1_i64 << 50), TupleId::new(1))
            .unwrap_err();
        assert!(err.to_string().contains("signed 48-bit"));
    }

    #[test]
    fn range_lookup_preserves_boolean_order() {
        let (_store, index) = index();
        index
            .insert(&Value::Boolean(true), TupleId::new(3))
            .unwrap();
        index
            .insert(&Value::Boolean(false), TupleId::new(1))
            .unwrap();
        index
            .insert(&Value::Boolean(false), TupleId::new(2))
            .unwrap();

        assert_eq!(
            index
                .range(
                    Some(&Value::Boolean(false)),
                    Some(&Value::Boolean(true)),
                    None
                )
                .unwrap(),
            vec![TupleId::new(1), TupleId::new(2), TupleId::new(3)]
        );

        let range = KeyRange {
            lower: Bound::Excluded(vec![Value::Boolean(false)]),
            upper: Bound::Unbounded,
        };
        assert_eq!(
            index.scan_key_range(&range, None).unwrap(),
            vec![TupleId::new(3)]
        );
    }

    #[test]
    fn text_prefix_range_returns_candidates_for_recheck() {
        let (_store, index) = index();
        for (value, tuple) in [("alpha", 1), ("beta", 2), ("betamax", 3), ("zeta", 4)] {
            let tuple_id = TupleId::new(tuple);
            let key =
                pack_ordered_prefix_tuple_key(&Value::Text(value.to_owned()), tuple_id).unwrap();
            index.tree.insert(key, tuple_id.get()).unwrap();
        }

        let range = KeyRange {
            lower: Bound::Included(vec![Value::Text("beta".to_owned())]),
            upper: Bound::Included(vec![Value::Text("betaz".to_owned())]),
        };
        assert_eq!(
            index.scan_key_range(&range, None).unwrap(),
            vec![TupleId::new(2), TupleId::new(3)]
        );
    }

    #[test]
    fn text_exact_probe_uses_hash_key_not_prefix_window() {
        let (_store, index) = index();
        for (value, tuple) in [("beta", 1), ("betamax", 2), ("betaz", 3)] {
            let tuple_id = TupleId::new(tuple);
            let key =
                pack_namespaced_hash_tuple_key([&Value::Text(value.to_owned())], tuple_id).unwrap();
            index.tree.insert(key, tuple_id.get()).unwrap();
        }

        let range = KeyRange {
            lower: Bound::Included(vec![Value::Text("beta".to_owned())]),
            upper: Bound::Included(vec![Value::Text("beta".to_owned())]),
        };
        assert_eq!(
            index.scan_key_range(&range, None).unwrap(),
            vec![TupleId::new(1)]
        );
    }

    #[test]
    fn prefix_range_ignores_hash_namespace_entries() {
        let (_store, index) = index();
        let tuple_id = TupleId::new(1);
        let hash_key =
            pack_namespaced_hash_tuple_key([&Value::Text("beta".to_owned())], tuple_id).unwrap();
        index.tree.insert(hash_key, tuple_id.get()).unwrap();

        let range = KeyRange {
            lower: Bound::Included(vec![Value::Text("beta".to_owned())]),
            upper: Bound::Included(vec![Value::Text("betaz".to_owned())]),
        };
        assert!(index.scan_key_range(&range, None).unwrap().is_empty());
    }

    #[test]
    fn uuid_prefix_range_returns_candidates_for_recheck() {
        let (_store, index) = index();
        for (value, tuple) in [
            ([0x00, 0x00, 0x00, 0x01], 1),
            ([0x10, 0x00, 0x00, 0x00], 2),
            ([0x10, 0x00, 0x00, 0x01], 3),
            ([0x10, 0x00, 0x00, 0x01], 4),
            ([0x20, 0x00, 0x00, 0x00], 5),
        ] {
            let mut bytes = [if tuple == 4 { 0xFF } else { 0_u8 }; 16];
            bytes[..4].copy_from_slice(&value);
            let tuple_id = TupleId::new(tuple);
            let key = pack_ordered_prefix_tuple_key(&Value::Uuid(bytes), tuple_id).unwrap();
            index.tree.insert(key, tuple_id.get()).unwrap();
        }

        let mut lower = [0_u8; 16];
        lower[..4].copy_from_slice(&[0x10, 0x00, 0x00, 0x00]);
        let mut upper = [0_u8; 16];
        upper[..4].copy_from_slice(&[0x10, 0x00, 0x00, 0x7F]);
        let range = KeyRange {
            lower: Bound::Included(vec![Value::Uuid(lower)]),
            upper: Bound::Included(vec![Value::Uuid(upper)]),
        };
        assert_eq!(
            index.scan_key_range(&range, None).unwrap(),
            vec![TupleId::new(2), TupleId::new(3), TupleId::new(4)]
        );
    }

    #[test]
    fn prefix_tuple_id_window_error_is_logical_fallback_safe() {
        let (_store, index) = index();
        let descriptor = IndexStorageDescriptor {
            index_id: aiondb_core::IndexId::new(2),
            table_id: aiondb_core::RelationId::new(2),
            unique: false,
            nulls_not_distinct: false,
            gin: false,
            key_columns: vec![aiondb_storage_api::IndexKeyColumn {
                column_id: ColumnId::new(1),
                descending: false,
                nulls_first: false,
            }],
            include_columns: Vec::new(),
            hnsw_options: None,
            ivf_flat_options: None,
        };
        let table = TableStorageDescriptor {
            table_id: aiondb_core::RelationId::new(2),
            columns: vec![aiondb_storage_api::StorageColumn {
                column_id: ColumnId::new(1),
                data_type: DataType::Text,
                nullable: false,
            }],
            primary_key: None,
            shard_config: None,
        };
        let error = index
            .bulk_load_rows(
                &descriptor,
                &table,
                [(
                    TupleId::new(1_u64 << 24),
                    Row::new(vec![Value::Text("overflow".to_owned())]),
                )],
            )
            .unwrap_err();

        assert!(can_fallback_to_logical_index(&error));
    }

    #[test]
    fn scan_mode_does_not_treat_text_bounds_as_fixed_ordered_scan() {
        let descriptor = IndexStorageDescriptor {
            index_id: aiondb_core::IndexId::new(1),
            table_id: aiondb_core::RelationId::new(1),
            unique: false,
            nulls_not_distinct: false,
            gin: false,
            key_columns: vec![aiondb_storage_api::IndexKeyColumn {
                column_id: ColumnId::new(1),
                descending: false,
                nulls_first: false,
            }],
            include_columns: Vec::new(),
            hnsw_options: None,
            ivf_flat_options: None,
        };
        let table = TableStorageDescriptor {
            table_id: aiondb_core::RelationId::new(1),
            columns: vec![aiondb_storage_api::StorageColumn {
                column_id: ColumnId::new(1),
                data_type: DataType::Int,
                nullable: false,
            }],
            primary_key: None,
            shard_config: None,
        };
        let range = KeyRange {
            lower: Bound::Included(vec![Value::Text("not-an-int".to_owned())]),
            upper: Bound::Included(vec![Value::Text("not-an-int".to_owned())]),
        };

        assert_eq!(scan_mode(&descriptor, &table, &range), None);
    }

    #[test]
    fn lookup_plan_rejects_text_range_with_embedded_nul() {
        let (table, descriptor) = nullable_text_table_and_index(false, false);
        let range = KeyRange {
            lower: Bound::Included(vec![Value::Text("a\0b".to_owned())]),
            upper: Bound::Included(vec![Value::Text("z".to_owned())]),
        };

        assert_eq!(lookup_plan(&descriptor, &table, &range), None);
    }

    #[test]
    fn lookup_plan_prefers_var_backend_for_composite_exact() {
        let table = TableStorageDescriptor {
            table_id: aiondb_core::RelationId::new(5),
            columns: vec![
                aiondb_storage_api::StorageColumn {
                    column_id: ColumnId::new(1),
                    data_type: DataType::Int,
                    nullable: false,
                },
                aiondb_storage_api::StorageColumn {
                    column_id: ColumnId::new(2),
                    data_type: DataType::Text,
                    nullable: false,
                },
            ],
            primary_key: None,
            shard_config: None,
        };
        let descriptor = IndexStorageDescriptor {
            index_id: aiondb_core::IndexId::new(5),
            table_id: table.table_id,
            unique: false,
            nulls_not_distinct: false,
            gin: false,
            key_columns: vec![
                aiondb_storage_api::IndexKeyColumn {
                    column_id: ColumnId::new(1),
                    descending: false,
                    nulls_first: false,
                },
                aiondb_storage_api::IndexKeyColumn {
                    column_id: ColumnId::new(2),
                    descending: false,
                    nulls_first: false,
                },
            ],
            include_columns: Vec::new(),
            hnsw_options: None,
            ivf_flat_options: None,
        };
        let plan = lookup_plan(
            &descriptor,
            &table,
            &KeyRange::point(vec![Value::Int(7), Value::Text("bob".to_owned())]),
        )
        .unwrap();
        assert_eq!(plan.backend, DiskIndexLookupBackend::Var);
        assert_eq!(plan.mode, DiskOrderedScanMode::HashedExact);
    }

    #[test]
    fn registry_plan_builds_for_non_unique_simple_int_index() {
        let (table, descriptor) = int_table_and_index(false, false);
        let plan = registry_plan(&descriptor, &table);
        assert!(plan.build_fixed);
        assert!(plan.build_var);
    }

    #[test]
    fn registry_plan_builds_both_for_unique_simple_int_index() {
        let (table, mut descriptor) = int_table_and_index(false, false);
        descriptor.unique = true;
        let plan = registry_plan(&descriptor, &table);
        assert!(plan.build_fixed);
        assert!(plan.build_var);
    }

    #[test]
    fn remove_deletes_one_duplicate_entry() {
        let (_store, index) = index();
        index.insert(&Value::Int(4), TupleId::new(1)).unwrap();
        index.insert(&Value::Int(4), TupleId::new(2)).unwrap();
        index.insert(&Value::Int(4), TupleId::new(3)).unwrap();

        assert!(index.remove(&Value::Int(4), TupleId::new(2)).unwrap());
        assert!(!index.remove(&Value::Int(4), TupleId::new(2)).unwrap());
        assert_eq!(
            index.exact(&Value::Int(4)).unwrap(),
            vec![TupleId::new(1), TupleId::new(3)]
        );
    }

    #[test]
    fn survives_reopen() {
        let store = Arc::new(MemoryPageStore::new());
        {
            let pool = Arc::new(BufferPool::new(8, store.clone()));
            let index = DiskOrderedIntIndex::open_or_create(pool, 123).unwrap();
            for key in 0..20 {
                index
                    .insert(&Value::Int(key), TupleId::new((key + 1) as u64))
                    .unwrap();
            }
            index.flush().unwrap();
        }
        {
            let pool = Arc::new(BufferPool::new(8, store.clone()));
            let index = DiskOrderedIntIndex::open_or_create(pool, 123).unwrap();
            assert_eq!(
                index.exact(&Value::Int(11)).unwrap(),
                vec![TupleId::new(12)]
            );
        }
    }

    #[test]
    fn key_range_scan_honors_inclusive_and_exclusive_bounds() {
        let (_store, index) = index();
        for (value, tuple) in [(-2, 1), (-1, 2), (0, 3), (1, 4), (2, 5)] {
            index
                .insert(&Value::Int(value), TupleId::new(tuple))
                .unwrap();
        }

        let range = KeyRange {
            lower: Bound::Excluded(vec![Value::Int(-1)]),
            upper: Bound::Included(vec![Value::Int(2)]),
        };
        assert_eq!(
            index.scan_key_range(&range, Some(2)).unwrap(),
            vec![TupleId::new(3), TupleId::new(4)]
        );
    }

    #[test]
    fn key_range_scan_returns_empty_for_impossible_bounds() {
        let (_store, index) = index();
        index.insert(&Value::Int(1), TupleId::new(1)).unwrap();

        let range = KeyRange {
            lower: Bound::Excluded(vec![Value::Int(i32::MAX)]),
            upper: Bound::Unbounded,
        };
        assert!(index.scan_key_range(&range, None).unwrap().is_empty());

        let range = KeyRange {
            lower: Bound::Included(vec![Value::Int(2)]),
            upper: Bound::Included(vec![Value::Int(1)]),
        };
        assert!(index.scan_key_range(&range, None).unwrap().is_empty());
    }
}
