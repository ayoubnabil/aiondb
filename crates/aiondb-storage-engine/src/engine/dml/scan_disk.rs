use std::collections::HashSet;

use aiondb_core::DbResult;
use aiondb_core::Value;
use aiondb_storage_api::{Bound, KeyRange};

use super::super::super::{disk_ordered_index, InMemoryStorage, IndexData, TableView};

pub(super) fn key_range_is_exact_non_null_point(
    key_range: &KeyRange,
    expected_key_len: usize,
) -> bool {
    match (&key_range.lower, &key_range.upper) {
        (Bound::Included(lower), Bound::Included(upper)) if lower == upper => {
            lower.len() == expected_key_len
                && !lower.iter().any(|value| matches!(value, Value::Null))
        }
        _ => false,
    }
}

pub(super) fn disk_ordered_candidate_tuple_ids(
    storage: &InMemoryStorage,
    index: &IndexData,
    table_view: &TableView<'_>,
    temp_index_owner: Option<aiondb_core::TxnId>,
    state: &super::super::super::StorageState,
    latest_snapshot: bool,
    key_range: &KeyRange,
) -> DbResult<
    Option<(
        Vec<aiondb_core::TupleId>,
        disk_ordered_index::DiskOrderedScanMode,
    )>,
> {
    if !latest_snapshot {
        return Ok(None);
    }
    let descriptor = table_view.descriptor();
    let Some(plan) = disk_ordered_index::lookup_plan(&index.descriptor, descriptor, key_range)
    else {
        return Ok(None);
    };
    if temp_index_owner.is_some()
        && matches!(table_view, TableView::Created(_))
        && !matches!(
            plan.backend,
            disk_ordered_index::DiskIndexLookupBackend::Var
        )
    {
        return Ok(None);
    }
    let mode = plan.mode;

    if let Some(txn) = temp_index_owner {
        if let Some(values) = disk_ordered_index::exact_scalar_key_values(key_range) {
            let disk_indexes = storage.pending_disk_var_exact_indexes.read();
            if let Some(disk_index) = disk_indexes.get(&(txn, index.descriptor.index_id)).cloned() {
                let mut candidate_ids = disk_index.exact_values(values.iter())?;
                dedupe_tuple_ids(&mut candidate_ids);
                return Ok(Some((
                    candidate_ids,
                    disk_ordered_index::DiskOrderedScanMode::HashedExact,
                )));
            }
        }
    }

    let mut candidate_ids = match plan.backend {
        disk_ordered_index::DiskIndexLookupBackend::Var => {
            let disk_index = if let Some(txn) = temp_index_owner {
                let existing = {
                    let disk_indexes = storage.pending_disk_var_exact_indexes.read();
                    disk_indexes.get(&(txn, index.descriptor.index_id)).cloned()
                };
                if existing.is_none() {
                    storage.build_pending_disk_var_exact_index_if_supported(
                        state,
                        txn,
                        index.descriptor.index_id,
                        index,
                    )?;
                }
                let disk_indexes = storage.pending_disk_var_exact_indexes.read();
                disk_indexes.get(&(txn, index.descriptor.index_id)).cloned()
            } else {
                let existing = {
                    let disk_indexes = storage.disk_var_exact_indexes.read();
                    disk_indexes.get(&index.descriptor.index_id).cloned()
                };
                if existing.is_none() {
                    storage.build_disk_var_exact_index_if_supported(
                        state,
                        index.descriptor.index_id,
                        &index.descriptor,
                    )?;
                }
                let disk_indexes = storage.disk_var_exact_indexes.read();
                disk_indexes.get(&index.descriptor.index_id).cloned()
            };
            let Some(disk_index) = disk_index else {
                return Ok(None);
            };
            if matches!(mode, disk_ordered_index::DiskOrderedScanMode::HashedExact) {
                let Some(values) = disk_ordered_index::exact_scalar_key_values(key_range) else {
                    return Ok(None);
                };
                disk_index.exact_values(values.iter())?
            } else {
                disk_index.range_values(&index.descriptor, key_range)?
            }
        }
        disk_ordered_index::DiskIndexLookupBackend::Fixed => {
            let disk_index = if let Some(txn) = temp_index_owner {
                let existing = {
                    let disk_indexes = storage.pending_disk_ordered_indexes.read();
                    disk_indexes.get(&(txn, index.descriptor.index_id)).cloned()
                };
                if existing.is_none() {
                    storage.build_pending_disk_ordered_index_if_supported(
                        state,
                        txn,
                        index.descriptor.index_id,
                        index,
                    )?;
                }
                let disk_indexes = storage.pending_disk_ordered_indexes.read();
                disk_indexes.get(&(txn, index.descriptor.index_id)).cloned()
            } else {
                let existing = {
                    let disk_indexes = storage.disk_ordered_indexes.read();
                    disk_indexes.get(&index.descriptor.index_id).cloned()
                };
                if existing.is_none() {
                    storage.build_disk_ordered_index_if_supported(
                        state,
                        index.descriptor.index_id,
                        &index.descriptor,
                    )?;
                }
                let disk_indexes = storage.disk_ordered_indexes.read();
                disk_indexes.get(&index.descriptor.index_id).cloned()
            };
            if let Some(disk_index) = disk_index {
                disk_index.scan_key_range(key_range, None)?
            } else if let Some(txn) = temp_index_owner {
                let disk_indexes = storage.pending_disk_var_exact_indexes.read();
                let Some(disk_index) = disk_indexes.get(&(txn, index.descriptor.index_id)).cloned()
                else {
                    return Ok(None);
                };
                disk_index.range_values(&index.descriptor, key_range)?
            } else {
                return Ok(None);
            }
        }
    };
    dedupe_tuple_ids(&mut candidate_ids);
    Ok(Some((candidate_ids, mode)))
}

pub(super) fn disk_ordered_candidate_tuple_ids_limited(
    storage: &InMemoryStorage,
    index: &IndexData,
    table_view: &TableView<'_>,
    temp_index_owner: Option<aiondb_core::TxnId>,
    state: &super::super::super::StorageState,
    latest_snapshot: bool,
    key_range: &KeyRange,
    descending: bool,
    limit: usize,
) -> DbResult<
    Option<(
        Vec<aiondb_core::TupleId>,
        disk_ordered_index::DiskOrderedScanMode,
    )>,
> {
    if !latest_snapshot || limit == 0 {
        return Ok(None);
    }
    let descriptor = table_view.descriptor();
    let Some(plan) = disk_ordered_index::lookup_plan(&index.descriptor, descriptor, key_range)
    else {
        return Ok(None);
    };
    if !matches!(
        plan.backend,
        disk_ordered_index::DiskIndexLookupBackend::Fixed
    ) {
        return Ok(None);
    }
    if temp_index_owner.is_some() && matches!(table_view, TableView::Created(_)) {
        return Ok(None);
    }

    let disk_index = if let Some(txn) = temp_index_owner {
        let existing = {
            let disk_indexes = storage.pending_disk_ordered_indexes.read();
            disk_indexes.get(&(txn, index.descriptor.index_id)).cloned()
        };
        if existing.is_none() {
            storage.build_pending_disk_ordered_index_if_supported(
                state,
                txn,
                index.descriptor.index_id,
                index,
            )?;
        }
        let disk_indexes = storage.pending_disk_ordered_indexes.read();
        disk_indexes.get(&(txn, index.descriptor.index_id)).cloned()
    } else {
        let existing = {
            let disk_indexes = storage.disk_ordered_indexes.read();
            disk_indexes.get(&index.descriptor.index_id).cloned()
        };
        if existing.is_none() {
            storage.build_disk_ordered_index_if_supported(
                state,
                index.descriptor.index_id,
                &index.descriptor,
            )?;
        }
        let disk_indexes = storage.disk_ordered_indexes.read();
        disk_indexes.get(&index.descriptor.index_id).cloned()
    };

    let Some(disk_index) = disk_index else {
        return Ok(None);
    };
    let mut candidate_ids =
        disk_index.scan_key_range_ordered(key_range, Some(limit), descending)?;
    dedupe_tuple_ids(&mut candidate_ids);
    Ok(Some((candidate_ids, plan.mode)))
}

pub(super) fn dedupe_tuple_ids(candidate_ids: &mut Vec<aiondb_core::TupleId>) {
    if candidate_ids.len() > 1 {
        let mut seen = HashSet::with_capacity(candidate_ids.len());
        candidate_ids.retain(|tuple_id| seen.insert(*tuple_id));
    }
}
