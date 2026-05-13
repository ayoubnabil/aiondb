use std::collections::BTreeMap;

use aiondb_core::{DbError, DbResult, Row, Value};

const INLINE_VALUE_THRESHOLD: usize = 96;
const OVERFLOW_PAGE_BYTES: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub(crate) struct OverflowPageId(u64);

impl OverflowPageId {
    fn new(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct StoredRow {
    values: Vec<StoredValue>,
}

impl StoredRow {
    pub(crate) fn values(&self) -> &[StoredValue] {
        &self.values
    }
}

#[derive(Clone, Debug)]
pub(crate) enum StoredValue {
    Inline(Value),
    Overflow(OverflowValue),
}

#[derive(Clone, Debug)]
pub(crate) struct OverflowValue {
    first_page_id: OverflowPageId,
    len: usize,
    kind: OverflowKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OverflowKind {
    Text,
    Blob,
}

#[derive(Clone, Debug)]
struct OverflowPage {
    next: Option<OverflowPageId>,
    payload: Vec<u8>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct OverflowStore {
    next_page_id: u64,
    pages: BTreeMap<OverflowPageId, OverflowPage>,
}

impl OverflowStore {
    pub(crate) fn store_row(&mut self, row: &Row) -> StoredRow {
        let values = row
            .values
            .iter()
            .map(|value| self.store_value(value))
            .collect();
        StoredRow { values }
    }

    /// Consuming variant of [`Self::store_row`]: moves each `Value`
    /// directly into the StoredValue::Inline variant when it doesn't
    /// need overflow storage. Saves a `String` / `Vec<u8>` clone per
    /// row on hot insert paths where the caller hands over an owned
    /// `Row` (no borrow needed afterwards). Mirrors PG's
    /// `heap_form_tuple` consume-the-datum pattern.
    pub(crate) fn store_row_owned(&mut self, row: Row) -> StoredRow {
        let values = row
            .values
            .into_iter()
            .map(|value| self.store_value_owned(value))
            .collect();
        StoredRow { values }
    }

    fn store_value_owned(&mut self, value: Value) -> StoredValue {
        match value {
            Value::Text(text) if text.len() > INLINE_VALUE_THRESHOLD => {
                StoredValue::Overflow(self.store_bytes(OverflowKind::Text, text.as_bytes()))
            }
            Value::Blob(bytes) if bytes.len() > INLINE_VALUE_THRESHOLD => {
                StoredValue::Overflow(self.store_bytes(OverflowKind::Blob, &bytes))
            }
            other => StoredValue::Inline(other),
        }
    }

    pub(crate) fn load_row(&self, row: &StoredRow) -> DbResult<Row> {
        let values = row
            .values()
            .iter()
            .map(|value| self.load_value(value))
            .collect::<DbResult<Vec<_>>>()?;
        Ok(Row::new(values))
    }

    pub(crate) fn load_row_projected(
        &self,
        row: &StoredRow,
        projection_ordinals: &[usize],
    ) -> DbResult<Row> {
        let mut values = Vec::with_capacity(projection_ordinals.len());
        for ordinal in projection_ordinals {
            let value = row
                .values()
                .get(*ordinal)
                .ok_or_else(|| DbError::internal("row is missing projected value"))?;
            values.push(self.load_value(value)?);
        }
        Ok(Row::new(values))
    }

    /// Decode a single column value out of a stored row without
    /// allocating a one-element `Row`. Used by per-row filter
    /// pushdowns where only the predicate column needs to be
    /// materialised before deciding to fetch the rest of the tuple.
    pub(crate) fn load_value_at(&self, row: &StoredRow, ordinal: usize) -> DbResult<Value> {
        let value = row
            .values()
            .get(ordinal)
            .ok_or_else(|| DbError::internal("row is missing projected value"))?;
        self.load_value(value)
    }

    pub(crate) fn stored_value_matches_any_filter(
        &self,
        value: &StoredValue,
        filter_values: &[Value],
    ) -> DbResult<bool> {
        // Fast path: when the stored value is `Inline`, compare it
        // against the filter values WITHOUT cloning. The previous
        // implementation always cloned via `load_value` for every
        // tuple in the scan — for INT columns the clone is cheap
        // but the per-row Value-enum dispatch + DbResult unwrapping
        // measurably dominates a 20k-row UPDATE-with-filter pushdown
        // (mesured ~365 ns/row before this change for a tight
        // `WHERE nx = literal` shape with one matching tuple). Only
        // overflow values pay the page-walk + alloc cost.
        match value {
            StoredValue::Inline(inline) => Ok(filter_values
                .iter()
                .any(|filter_value| values_match_eq_filter(inline, filter_value))),
            StoredValue::Overflow(_) => {
                let loaded = self.load_value(value)?;
                Ok(filter_values
                    .iter()
                    .any(|filter_value| values_match_eq_filter(&loaded, filter_value)))
            }
        }
    }

    pub(crate) fn release_row(&mut self, row: &StoredRow) {
        for value in row.values() {
            self.release_value(value);
        }
    }

    /// Estimate the in-memory byte footprint of the overflow store.
    ///
    /// Each page carries a `Vec<u8>` payload (up to `OVERFLOW_PAGE_BYTES`)
    /// plus `BTreeMap` node overhead.
    pub(crate) fn estimated_bytes(&self) -> u64 {
        // BTreeMap per-entry overhead (~64 bytes) + OverflowPage struct
        // (next: 16 bytes option, payload Vec header: 24 bytes).
        const PER_PAGE_OVERHEAD: u64 = 104;
        self.pages.values().fold(0u64, |bytes, page| {
            let payload_len = u64::try_from(page.payload.len()).unwrap_or(u64::MAX);
            bytes.saturating_add(PER_PAGE_OVERHEAD.saturating_add(payload_len))
        })
    }

    #[cfg(test)]
    pub(crate) fn page_count(&self) -> usize {
        self.pages.len()
    }

    fn store_value(&mut self, value: &Value) -> StoredValue {
        match value {
            Value::Text(text) if text.len() > INLINE_VALUE_THRESHOLD => {
                StoredValue::Overflow(self.store_bytes(OverflowKind::Text, text.as_bytes()))
            }
            Value::Blob(bytes) if bytes.len() > INLINE_VALUE_THRESHOLD => {
                StoredValue::Overflow(self.store_bytes(OverflowKind::Blob, bytes))
            }
            _ => StoredValue::Inline(value.clone()),
        }
    }

    fn load_value(&self, value: &StoredValue) -> DbResult<Value> {
        match value {
            StoredValue::Inline(value) => Ok(value.clone()),
            StoredValue::Overflow(overflow) => {
                let bytes = self.read_bytes(overflow)?;
                match overflow.kind {
                    OverflowKind::Text => String::from_utf8(bytes)
                        .map(Value::Text)
                        .map_err(|_| DbError::internal("invalid UTF-8 in overflow text value")),
                    OverflowKind::Blob => Ok(Value::Blob(bytes)),
                }
            }
        }
    }

    fn release_value(&mut self, value: &StoredValue) {
        let StoredValue::Overflow(overflow) = value else {
            return;
        };
        let mut current = Some(overflow.first_page_id);
        while let Some(page_id) = current {
            current = self.pages.remove(&page_id).and_then(|page| page.next);
        }
    }

    fn store_bytes(&mut self, kind: OverflowKind, bytes: &[u8]) -> OverflowValue {
        let mut first_page_id = None;
        let mut previous_page_id = None;
        for chunk in bytes.chunks(OVERFLOW_PAGE_BYTES) {
            let page_id = self.allocate_page_id();
            self.pages.insert(
                page_id,
                OverflowPage {
                    next: None,
                    payload: chunk.to_vec(),
                },
            );
            if let Some(previous_page_id) = previous_page_id {
                if let Some(previous_page) = self.pages.get_mut(&previous_page_id) {
                    previous_page.next = Some(page_id);
                }
            }
            first_page_id.get_or_insert(page_id);
            previous_page_id = Some(page_id);
        }
        OverflowValue {
            first_page_id: first_page_id.unwrap_or_else(|| self.allocate_page_id()),
            len: bytes.len(),
            kind,
        }
    }

    fn read_bytes(&self, overflow: &OverflowValue) -> DbResult<Vec<u8>> {
        let mut bytes = Vec::with_capacity(overflow.len);
        let mut current = Some(overflow.first_page_id);
        while let Some(page_id) = current {
            let page = self
                .pages
                .get(&page_id)
                .ok_or_else(|| DbError::internal("overflow page is missing"))?;
            bytes.extend_from_slice(&page.payload);
            if bytes.len() >= overflow.len {
                bytes.truncate(overflow.len);
                return Ok(bytes);
            }
            current = page.next;
        }
        Err(DbError::internal("overflow page chain is truncated"))
    }

    fn allocate_page_id(&mut self) -> OverflowPageId {
        self.next_page_id += 1;
        OverflowPageId::new(self.next_page_id)
    }
}

fn values_match_eq_filter(value: &Value, filter_value: &Value) -> bool {
    if matches!(value, Value::Null) || matches!(filter_value, Value::Null) {
        return false;
    }
    match (value, filter_value) {
        (Value::Int(left), Value::BigInt(right)) => i64::from(*left) == *right,
        (Value::BigInt(left), Value::Int(right)) => *left == i64::from(*right),
        _ => value == filter_value,
    }
}
