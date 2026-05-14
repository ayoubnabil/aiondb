#![allow(clippy::doc_markdown)]

//! GIN (Generalized Inverted Index) support for:
//! - JSONB containment queries (`@>`)
//! - TEXT token lookup used by full-text hybrid operators.
//!
//! Extracts key-value path tokens from each JSONB document and maps them to
//! the set of tuple IDs that contain them. A containment query `col @> pattern`
//! extracts the same path tokens from `pattern` and intersects the posting
//! lists to produce candidate tuple IDs.

use std::collections::{BTreeMap, BTreeSet};

use aiondb_core::{DataType, DbError, DbResult, Row, TupleId, Value};
use aiondb_storage_api::{IndexStorageDescriptor, TableStorageDescriptor};

/// A single path token extracted from a JSONB document.
///
/// Tokens represent either:
/// - A key-value pair at any depth: `"path.to.key" = "value_repr"`
/// - A key existence: `"path.to.key" = EXISTS`
/// - An array element: `"path[]" = "element_repr"`
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct GinToken(pub String);

/// GIN index for JSONB containment and TEXT token lookup.
#[derive(Clone, Debug)]
pub(crate) struct GinIndex {
    pub(crate) descriptor: IndexStorageDescriptor,
    /// Inverted index: token -> set of tuple IDs containing that token.
    postings: BTreeMap<GinToken, BTreeSet<TupleId>>,
    /// Forward index: tuple_id -> set of tokens (for efficient removal).
    forward: BTreeMap<TupleId, BTreeSet<GinToken>>,
    /// Column ordinal in the table for the indexed column.
    column_ordinal: Option<usize>,
    /// Indexed column mode inferred from table schema.
    column_mode: Option<GinColumnMode>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GinColumnMode {
    Jsonb,
    Text,
}

impl GinIndex {
    pub(crate) fn new(descriptor: IndexStorageDescriptor) -> Self {
        Self {
            descriptor,
            postings: BTreeMap::new(),
            forward: BTreeMap::new(),
            column_ordinal: None,
            column_mode: None,
        }
    }

    /// Build a GIN index from existing rows.
    pub(crate) fn from_rows<I>(
        descriptor: &IndexStorageDescriptor,
        table_descriptor: &TableStorageDescriptor,
        rows: I,
    ) -> DbResult<Self>
    where
        I: IntoIterator<Item = (TupleId, Row)>,
    {
        let mut index = Self::new(descriptor.clone());
        for (tuple_id, row) in rows {
            index.insert_tuple(table_descriptor, tuple_id, &row)?;
        }
        Ok(index)
    }

    /// Insert a tuple into the GIN index.
    pub(crate) fn insert_tuple(
        &mut self,
        table_descriptor: &TableStorageDescriptor,
        tuple_id: TupleId,
        row: &Row,
    ) -> DbResult<()> {
        let value_tokens = self.extract_value_tokens(table_descriptor, row)?;
        let Some(tokens) = value_tokens else {
            return Ok(()); // NULL values are not indexed
        };
        for token in &tokens {
            self.postings
                .entry(token.clone())
                .or_default()
                .insert(tuple_id);
        }
        self.forward.insert(tuple_id, tokens);
        Ok(())
    }

    pub(crate) fn validate_insert_tuple(
        &self,
        table_descriptor: &TableStorageDescriptor,
        row: &Row,
    ) -> DbResult<()> {
        let (ordinal, mode) = self.resolve_column_info_for(table_descriptor)?;
        let value = row
            .values
            .get(ordinal)
            .ok_or_else(|| DbError::internal("row is missing indexed GIN value"))?;
        match (mode, value) {
            (GinColumnMode::Jsonb, Value::Jsonb(_)) | (_, Value::Null) => Ok(()),
            (GinColumnMode::Text, Value::Text(_)) => Ok(()),
            (GinColumnMode::Jsonb, _) => Err(DbError::internal("GIN indexed column is not JSONB")),
            (GinColumnMode::Text, _) => Err(DbError::internal("GIN indexed column is not TEXT")),
        }
    }

    /// Remove a tuple from the GIN index.
    pub(crate) fn remove_tuple(
        &mut self,
        table_descriptor: &TableStorageDescriptor,
        tuple_id: TupleId,
        _row: &Row,
    ) -> DbResult<()> {
        let _ = table_descriptor;
        let Some(tokens) = self.forward.remove(&tuple_id) else {
            return Ok(());
        };
        for token in &tokens {
            if let Some(posting) = self.postings.get_mut(token) {
                posting.remove(&tuple_id);
                if posting.is_empty() {
                    self.postings.remove(token);
                }
            }
        }
        Ok(())
    }

    /// Search the GIN index for tuples whose JSONB column contains the given
    /// pattern (i.e., `col @> pattern`).
    ///
    /// Returns candidate tuple IDs. These are exact matches when the pattern
    /// consists only of top-level key-value pairs and simple nested values.
    /// For complex containment (arrays, deep nesting), the caller must
    /// re-check the containment predicate against the actual rows.
    pub(crate) fn containment_search(&self, pattern: &serde_json::Value) -> Vec<TupleId> {
        let pattern_tokens = self.pattern_tokens(pattern);
        if pattern_tokens.is_empty() {
            // Empty pattern matches everything
            return self.forward.keys().copied().collect();
        }

        // Collect posting-list references, sorted smallest-first so the
        // initial set is as small as possible and subsequent retain() calls
        // do the least work.
        let mut postings: Vec<&BTreeSet<TupleId>> = Vec::with_capacity(pattern_tokens.len());
        for token in &pattern_tokens {
            if let Some(posting) = self.postings.get(token) {
                postings.push(posting);
            } else {
                return Vec::new(); // missing token => no results
            }
        }
        postings.sort_unstable_by_key(|p| p.len());

        let mut posting_iter = postings.into_iter();
        let Some(first_posting) = posting_iter.next() else {
            return self.forward.keys().copied().collect();
        };
        let mut result: BTreeSet<TupleId> = first_posting.clone();

        for posting in posting_iter {
            result.retain(|tid| posting.contains(tid));
            if result.is_empty() {
                break;
            }
        }

        result.into_iter().collect()
    }

    pub(crate) fn containment_search_limited(
        &self,
        pattern: &serde_json::Value,
        limit: usize,
    ) -> Vec<TupleId> {
        if limit == 0 {
            return Vec::new();
        }
        let pattern_tokens = self.pattern_tokens(pattern);
        if pattern_tokens.is_empty() {
            return self.forward.keys().copied().take(limit).collect();
        }

        let mut postings: Vec<&BTreeSet<TupleId>> = Vec::with_capacity(pattern_tokens.len());
        for token in &pattern_tokens {
            if let Some(posting) = self.postings.get(token) {
                postings.push(posting);
            } else {
                return Vec::new();
            }
        }
        postings.sort_unstable_by_key(|p| p.len());

        let Some((first, rest)) = postings.split_first() else {
            return self.forward.keys().copied().take(limit).collect();
        };
        let mut result = Vec::with_capacity(limit.min(first.len()));
        'candidate: for tuple_id in *first {
            for posting in rest {
                if !posting.contains(tuple_id) {
                    continue 'candidate;
                }
            }
            result.push(*tuple_id);
            if result.len() >= limit {
                break;
            }
        }
        result
    }

    fn pattern_tokens(&self, pattern: &serde_json::Value) -> BTreeSet<GinToken> {
        match self.column_mode {
            Some(GinColumnMode::Jsonb) => extract_tokens(pattern),
            Some(GinColumnMode::Text) => extract_text_pattern_tokens(pattern),
            None => {
                if self.forward.is_empty() {
                    BTreeSet::new()
                } else {
                    // Defensive fallback for partially initialized state.
                    extract_tokens(pattern)
                }
            }
        }
    }

    /// Estimate the in-memory byte footprint of this GIN index.
    pub(crate) fn estimated_bytes(&self) -> u64 {
        const PER_POSTING_ENTRY: u64 = 80;
        const PER_TOKEN_OVERHEAD: u64 = 96;
        const PER_FORWARD_ENTRY: u64 = 96;

        let mut bytes = 0u64;
        for (token, tids) in &self.postings {
            let token_len = u64::try_from(token.0.len()).unwrap_or(u64::MAX);
            let tids_len = u64::try_from(tids.len()).unwrap_or(u64::MAX);
            bytes = bytes.saturating_add(PER_TOKEN_OVERHEAD);
            bytes = bytes.saturating_add(token_len);
            bytes = bytes.saturating_add(tids_len.saturating_mul(PER_POSTING_ENTRY));
        }
        for tokens in self.forward.values() {
            bytes = bytes.saturating_add(PER_FORWARD_ENTRY);
            for token in tokens {
                let token_len = u64::try_from(token.0.len()).unwrap_or(u64::MAX);
                bytes = bytes.saturating_add(token_len.saturating_add(16));
            }
        }
        bytes
    }

    /// Extract indexed tokens from a row based on the indexed column mode.
    fn extract_value_tokens(
        &mut self,
        table_descriptor: &TableStorageDescriptor,
        row: &Row,
    ) -> DbResult<Option<BTreeSet<GinToken>>> {
        let (ordinal, mode) = self.resolve_column_info(table_descriptor)?;
        let value = row
            .values
            .get(ordinal)
            .ok_or_else(|| DbError::internal("row is missing indexed GIN value"))?;
        match (mode, value) {
            (GinColumnMode::Jsonb, Value::Jsonb(v)) => Ok(Some(extract_tokens(v))),
            (GinColumnMode::Text, Value::Text(text)) => Ok(Some(extract_text_tokens(text))),
            (_, Value::Null) => Ok(None),
            (GinColumnMode::Jsonb, _) => Err(DbError::internal("GIN indexed column is not JSONB")),
            (GinColumnMode::Text, _) => Err(DbError::internal("GIN indexed column is not TEXT")),
        }
    }

    fn resolve_column_info(
        &mut self,
        table_descriptor: &TableStorageDescriptor,
    ) -> DbResult<(usize, GinColumnMode)> {
        if let (Some(ordinal), Some(mode)) = (self.column_ordinal, self.column_mode) {
            return Ok((ordinal, mode));
        }
        let (ordinal, mode) = self.resolve_column_info_for(table_descriptor)?;
        self.column_ordinal = Some(ordinal);
        self.column_mode = Some(mode);
        Ok((ordinal, mode))
    }

    fn resolve_column_info_for(
        &self,
        table_descriptor: &TableStorageDescriptor,
    ) -> DbResult<(usize, GinColumnMode)> {
        if let (Some(ordinal), Some(mode)) = (self.column_ordinal, self.column_mode) {
            return Ok((ordinal, mode));
        }
        let key_column_id = self
            .descriptor
            .key_columns
            .first()
            .ok_or_else(|| DbError::internal("GIN index has no key column"))?
            .column_id;
        let ordinal = table_descriptor
            .columns
            .iter()
            .position(|c| c.column_id == key_column_id)
            .ok_or_else(|| DbError::internal("GIN key column not found in table"))?;
        let mode = match table_descriptor
            .columns
            .get(ordinal)
            .map(|column| &column.data_type)
        {
            Some(DataType::Jsonb) => GinColumnMode::Jsonb,
            Some(DataType::Text) => GinColumnMode::Text,
            _ => {
                return Err(DbError::internal(
                    "GIN indexes are only supported on JSONB or TEXT columns",
                ));
            }
        };
        Ok((ordinal, mode))
    }
}

/// Extract GIN tokens from a JSONB value.
///
/// Tokens are path-based strings that encode the structure of the JSON document.
/// This allows containment queries to be answered via posting list intersection.
fn extract_tokens(value: &serde_json::Value) -> BTreeSet<GinToken> {
    let mut tokens = BTreeSet::new();
    extract_tokens_recursive(value, &mut String::new(), &mut tokens, 0);
    tokens
}

/// Hard cap on the number of tokens extracted from a single JSONB value.
/// Bounds the BTreeSet allocation so a wide adversarial document
/// (`{"k0":1, "k1":2, …, "kN":N}` with N≈10^6) cannot OOM the GIN extractor
/// (audit storage B-5).
const MAX_GIN_TOKENS_PER_VALUE: usize = 8192;
const MAX_GIN_TOKEN_EXTRACT_DEPTH: usize = 256;

fn extract_tokens_recursive(
    value: &serde_json::Value,
    prefix: &mut String,
    tokens: &mut BTreeSet<GinToken>,
    depth: usize,
) {
    if tokens.len() >= MAX_GIN_TOKENS_PER_VALUE {
        return;
    }
    if depth >= MAX_GIN_TOKEN_EXTRACT_DEPTH {
        tokens.insert(GinToken(format!("v:{prefix}=<depth-limit>")));
        return;
    }
    match value {
        serde_json::Value::Object(map) => {
            for (key, val) in map {
                if tokens.len() >= MAX_GIN_TOKENS_PER_VALUE {
                    return;
                }
                let old_len = prefix.len();
                if !prefix.is_empty() {
                    prefix.push('.');
                }
                prefix.push_str(key);
                // Token for key existence
                tokens.insert(GinToken(format!("k:{prefix}")));
                // Recurse into value
                extract_tokens_recursive(val, prefix, tokens, depth + 1);
                prefix.truncate(old_len);
            }
        }
        serde_json::Value::Array(arr) => {
            let old_len = prefix.len();
            prefix.push_str("[]");
            for elem in arr {
                if tokens.len() >= MAX_GIN_TOKENS_PER_VALUE {
                    break;
                }
                extract_tokens_recursive(elem, prefix, tokens, depth + 1);
            }
            prefix.truncate(old_len);
        }
        serde_json::Value::String(s) => {
            tokens.insert(GinToken(format!("v:{prefix}={s}")));
        }
        serde_json::Value::Number(n) => {
            tokens.insert(GinToken(format!("v:{prefix}={n}")));
        }
        serde_json::Value::Bool(b) => {
            tokens.insert(GinToken(format!("v:{prefix}={b}")));
        }
        serde_json::Value::Null => {
            tokens.insert(GinToken(format!("v:{prefix}=null")));
        }
    }
}

fn extract_text_tokens(text: &str) -> BTreeSet<GinToken> {
    let mut tokens = BTreeSet::new();
    for word in tokenize_words(text) {
        tokens.insert(GinToken(format!("k:{word}")));
        let stemmed = english_stem(&word);
        if !stemmed.is_empty() {
            tokens.insert(GinToken(format!("k:{stemmed}")));
        }
    }
    tokens
}

fn extract_text_pattern_tokens(pattern: &serde_json::Value) -> BTreeSet<GinToken> {
    let mut tokens = BTreeSet::new();
    if let Some(object) = pattern.as_object() {
        for key in object.keys() {
            let normalized = key.trim().to_lowercase();
            if normalized.is_empty() {
                continue;
            }
            tokens.insert(GinToken(format!("k:{normalized}")));
        }
    }
    tokens
}

fn tokenize_words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '\'')
        .map(|word| word.trim_matches('\'').to_lowercase())
        .filter(|word| !word.is_empty())
        .collect()
}

fn english_stem(word: &str) -> String {
    let w = word.to_lowercase();
    if w.len() <= 2 {
        return w;
    }

    if let Some(base) = w.strip_suffix("ies") {
        if base.len() >= 2 {
            return format!("{base}i");
        }
    }
    if let Some(base) = w.strip_suffix("sses") {
        if !base.is_empty() {
            return format!("{base}ss");
        }
    }
    if let Some(base) = w.strip_suffix("ness") {
        if base.len() >= 2 {
            return base.to_string();
        }
    }
    if let Some(base) = w.strip_suffix("ment") {
        if base.len() >= 3 {
            return base.to_string();
        }
    }
    if let Some(base) = w.strip_suffix("ing") {
        if base.len() >= 3 {
            let bytes = base.as_bytes();
            if bytes.len() >= 2 && bytes[bytes.len() - 1] == bytes[bytes.len() - 2] {
                return base[..base.len() - 1].to_string();
            }
            return base.to_string();
        }
    }
    if let Some(base) = w.strip_suffix("tion") {
        if base.len() >= 2 {
            return base.to_string();
        }
    }
    if let Some(base) = w.strip_suffix("ation") {
        if base.len() >= 2 {
            return base.to_string();
        }
    }
    if let Some(base) = w.strip_suffix("ful") {
        if base.len() >= 2 {
            return base.to_string();
        }
    }
    if let Some(base) = w.strip_suffix("ous") {
        if base.len() >= 2 {
            return base.to_string();
        }
    }
    if let Some(base) = w.strip_suffix("ive") {
        if base.len() >= 2 {
            return base.to_string();
        }
    }
    if let Some(base) = w.strip_suffix("ly") {
        if base.len() >= 3 {
            return base.to_string();
        }
    }
    if let Some(base) = w.strip_suffix("ed") {
        if base.len() >= 3 {
            return base.to_string();
        }
    }
    if let Some(base) = w.strip_suffix("er") {
        if base.len() >= 3 {
            return base.to_string();
        }
    }
    if let Some(base) = w.strip_suffix("es") {
        if base.len() >= 3 {
            return base.to_string();
        }
    }
    if let Some(base) = w.strip_suffix('s') {
        if base.len() >= 3 && !base.ends_with('s') {
            return base.to_string();
        }
    }

    w
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_core::{ColumnId, IndexId, RelationId};
    use aiondb_storage_api::{
        IndexKeyColumn as StorageKeyColumn, IndexStorageDescriptor, StorageColumn,
        TableStorageDescriptor,
    };

    fn make_table_desc() -> TableStorageDescriptor {
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
                    data_type: DataType::Jsonb,
                    nullable: true,
                },
            ],
            primary_key: Some(vec![ColumnId::new(1)]),
            shard_config: None,
        }
    }

    fn make_index_desc() -> IndexStorageDescriptor {
        IndexStorageDescriptor {
            index_id: IndexId::new(200),
            table_id: RelationId::new(1),
            unique: false,
            nulls_not_distinct: false,
            gin: true,
            key_columns: vec![StorageKeyColumn {
                column_id: ColumnId::new(2),
                descending: false,
                nulls_first: false,
            }],
            include_columns: vec![],
            hnsw_options: None,
        }
    }

    fn make_row(id: i32, json: serde_json::Value) -> Row {
        Row::new(vec![Value::Int(id), Value::Jsonb(json)])
    }

    fn make_text_table_desc() -> TableStorageDescriptor {
        TableStorageDescriptor {
            table_id: RelationId::new(2),
            columns: vec![
                StorageColumn {
                    column_id: ColumnId::new(1),
                    data_type: DataType::Int,
                    nullable: false,
                },
                StorageColumn {
                    column_id: ColumnId::new(3),
                    data_type: DataType::Text,
                    nullable: true,
                },
            ],
            primary_key: Some(vec![ColumnId::new(1)]),
            shard_config: None,
        }
    }

    fn make_text_index_desc() -> IndexStorageDescriptor {
        IndexStorageDescriptor {
            index_id: IndexId::new(201),
            table_id: RelationId::new(2),
            unique: false,
            nulls_not_distinct: false,
            gin: true,
            key_columns: vec![StorageKeyColumn {
                column_id: ColumnId::new(3),
                descending: false,
                nulls_first: false,
            }],
            include_columns: vec![],
            hnsw_options: None,
        }
    }

    fn make_text_row(id: i32, body: &str) -> Row {
        Row::new(vec![Value::Int(id), Value::Text(body.to_owned())])
    }

    #[test]
    fn insert_and_search_basic() {
        let table_desc = make_table_desc();
        let index_desc = make_index_desc();
        let mut index = GinIndex::new(index_desc);

        index
            .insert_tuple(
                &table_desc,
                TupleId::new(1),
                &make_row(1, serde_json::json!({"name": "Alice", "age": 30})),
            )
            .unwrap();
        index
            .insert_tuple(
                &table_desc,
                TupleId::new(2),
                &make_row(2, serde_json::json!({"name": "Bob", "age": 25})),
            )
            .unwrap();
        index
            .insert_tuple(
                &table_desc,
                TupleId::new(3),
                &make_row(3, serde_json::json!({"name": "Alice", "city": "NYC"})),
            )
            .unwrap();

        // Search for {"name": "Alice"}
        let results = index.containment_search(&serde_json::json!({"name": "Alice"}));
        assert!(results.contains(&TupleId::new(1)));
        assert!(results.contains(&TupleId::new(3)));
        assert!(!results.contains(&TupleId::new(2)));
    }

    #[test]
    fn containment_search_no_matches() {
        let table_desc = make_table_desc();
        let index_desc = make_index_desc();
        let mut index = GinIndex::new(index_desc);

        index
            .insert_tuple(
                &table_desc,
                TupleId::new(1),
                &make_row(1, serde_json::json!({"name": "Alice"})),
            )
            .unwrap();

        let results = index.containment_search(&serde_json::json!({"name": "Bob"}));
        assert!(results.is_empty());
    }

    #[test]
    fn remove_tuple_works() {
        let table_desc = make_table_desc();
        let index_desc = make_index_desc();
        let mut index = GinIndex::new(index_desc);

        let row1 = make_row(1, serde_json::json!({"name": "Alice"}));
        index
            .insert_tuple(&table_desc, TupleId::new(1), &row1)
            .unwrap();

        let results = index.containment_search(&serde_json::json!({"name": "Alice"}));
        assert_eq!(results.len(), 1);

        index
            .remove_tuple(&table_desc, TupleId::new(1), &row1)
            .unwrap();

        let results = index.containment_search(&serde_json::json!({"name": "Alice"}));
        assert!(results.is_empty());
    }

    #[test]
    fn from_rows_builds_index() {
        let table_desc = make_table_desc();
        let index_desc = make_index_desc();

        let rows = vec![
            (
                TupleId::new(1),
                make_row(1, serde_json::json!({"color": "red"})),
            ),
            (
                TupleId::new(2),
                make_row(2, serde_json::json!({"color": "blue"})),
            ),
            (
                TupleId::new(3),
                make_row(3, serde_json::json!({"color": "red", "size": 10})),
            ),
        ];

        let index = GinIndex::from_rows(&index_desc, &table_desc, rows).unwrap();
        let results = index.containment_search(&serde_json::json!({"color": "red"}));
        assert_eq!(results.len(), 2);
        assert!(results.contains(&TupleId::new(1)));
        assert!(results.contains(&TupleId::new(3)));
    }

    #[test]
    fn null_values_not_indexed() {
        let table_desc = make_table_desc();
        let index_desc = make_index_desc();
        let mut index = GinIndex::new(index_desc);

        let row = Row::new(vec![Value::Int(1), Value::Null]);
        index
            .insert_tuple(&table_desc, TupleId::new(1), &row)
            .unwrap();

        let results = index.containment_search(&serde_json::json!({}));
        assert!(results.is_empty());
    }

    #[test]
    fn empty_pattern_matches_all() {
        let table_desc = make_table_desc();
        let index_desc = make_index_desc();
        let mut index = GinIndex::new(index_desc);

        index
            .insert_tuple(
                &table_desc,
                TupleId::new(1),
                &make_row(1, serde_json::json!({"a": 1})),
            )
            .unwrap();
        index
            .insert_tuple(
                &table_desc,
                TupleId::new(2),
                &make_row(2, serde_json::json!({"b": 2})),
            )
            .unwrap();

        // Empty object has no tokens -> matches all
        let results = index.containment_search(&serde_json::json!({}));
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn nested_containment() {
        let table_desc = make_table_desc();
        let index_desc = make_index_desc();
        let mut index = GinIndex::new(index_desc);

        index
            .insert_tuple(
                &table_desc,
                TupleId::new(1),
                &make_row(
                    1,
                    serde_json::json!({"address": {"city": "NYC", "zip": "10001"}}),
                ),
            )
            .unwrap();
        index
            .insert_tuple(
                &table_desc,
                TupleId::new(2),
                &make_row(
                    2,
                    serde_json::json!({"address": {"city": "LA", "zip": "90001"}}),
                ),
            )
            .unwrap();

        let results = index.containment_search(&serde_json::json!({"address": {"city": "NYC"}}));
        assert_eq!(results.len(), 1);
        assert!(results.contains(&TupleId::new(1)));
    }

    #[test]
    fn estimated_bytes_nonzero_for_nonempty() {
        let table_desc = make_table_desc();
        let index_desc = make_index_desc();
        let mut index = GinIndex::new(index_desc);

        index
            .insert_tuple(
                &table_desc,
                TupleId::new(1),
                &make_row(1, serde_json::json!({"key": "value"})),
            )
            .unwrap();

        assert!(index.estimated_bytes() > 0);
    }

    #[test]
    fn text_tokens_match_exact_terms() {
        let table_desc = make_text_table_desc();
        let index_desc = make_text_index_desc();
        let mut index = GinIndex::new(index_desc);

        index
            .insert_tuple(
                &table_desc,
                TupleId::new(1),
                &make_text_row(1, "The quick brown fox"),
            )
            .unwrap();
        index
            .insert_tuple(
                &table_desc,
                TupleId::new(2),
                &make_text_row(2, "slow turtle"),
            )
            .unwrap();

        let results = index.containment_search(&serde_json::json!({"quick": {}, "fox": {}}));
        assert_eq!(results, vec![TupleId::new(1)]);
    }

    #[test]
    fn text_tokens_include_stemmed_forms() {
        let table_desc = make_text_table_desc();
        let index_desc = make_text_index_desc();
        let mut index = GinIndex::new(index_desc);

        index
            .insert_tuple(
                &table_desc,
                TupleId::new(1),
                &make_text_row(1, "running runner runs"),
            )
            .unwrap();

        let results = index.containment_search(&serde_json::json!({"run": {}}));
        assert_eq!(results, vec![TupleId::new(1)]);
    }
}
