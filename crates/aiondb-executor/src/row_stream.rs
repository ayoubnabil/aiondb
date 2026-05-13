//! Streaming row iteration for pipeline execution.
//!
//! `RowStream` allows executor operators to produce rows lazily instead of
//! materializing the entire result set into a `Vec<Row>`.

use aiondb_core::{DbError, DbResult, Row};

const ROW_STREAM_COLLECT_ROWS_LIMIT: usize = 2_000_000;

/// A lazy, pull-based stream of rows from an executor operator.
///
/// Operators that implement `RowStream` produce rows one at a time
/// without materializing the entire result set.
pub trait RowStream: Send {
    /// Pull the next row from the stream.
    ///
    /// Returns `Ok(None)` when exhausted.
    fn next_row(&mut self) -> DbResult<Option<Row>>;

    /// Collect all remaining rows into a vector.
    ///
    /// This is the fallback for operators that need full materialization
    /// (e.g., sorts, window functions).
    fn collect_rows(&mut self) -> DbResult<Vec<Row>> {
        let mut rows = Vec::new();
        while let Some(row) = self.next_row()? {
            if rows.len() >= ROW_STREAM_COLLECT_ROWS_LIMIT {
                return Err(DbError::program_limit(
                    "row stream collection exceeded maximum row count",
                ));
            }
            rows.push(row);
        }
        Ok(rows)
    }

    /// Collect up to `limit` rows.
    fn collect_rows_limited(&mut self, limit: usize) -> DbResult<Vec<Row>> {
        let mut rows = Vec::with_capacity(limit.min(1024));
        while rows.len() < limit {
            match self.next_row()? {
                Some(row) => rows.push(row),
                None => break,
            }
        }
        Ok(rows)
    }
}

/// Adapter: wrap an existing `Vec<Row>` as a `RowStream`.
pub struct VecRowStream {
    rows: std::vec::IntoIter<Row>,
}

impl VecRowStream {
    pub fn new(rows: Vec<Row>) -> Self {
        Self {
            rows: rows.into_iter(),
        }
    }

    pub fn empty() -> Self {
        Self::new(Vec::new())
    }
}

impl RowStream for VecRowStream {
    fn next_row(&mut self) -> DbResult<Option<Row>> {
        Ok(self.rows.next())
    }

    fn collect_rows(&mut self) -> DbResult<Vec<Row>> {
        let remaining = self.rows.len();
        if remaining > ROW_STREAM_COLLECT_ROWS_LIMIT {
            return Err(DbError::program_limit(
                "row stream collection exceeded maximum row count",
            ));
        }
        Ok(self.rows.by_ref().collect())
    }
}

/// Adapter: apply a filter predicate to an existing stream.
pub struct FilterStream<S: RowStream> {
    inner: S,
    predicate: Box<dyn FnMut(&Row) -> DbResult<bool> + Send>,
}

impl<S: RowStream> FilterStream<S> {
    pub fn new(inner: S, predicate: impl FnMut(&Row) -> DbResult<bool> + Send + 'static) -> Self {
        Self {
            inner,
            predicate: Box::new(predicate),
        }
    }
}

impl<S: RowStream> RowStream for FilterStream<S> {
    fn next_row(&mut self) -> DbResult<Option<Row>> {
        loop {
            match self.inner.next_row()? {
                Some(row) => {
                    if (self.predicate)(&row)? {
                        return Ok(Some(row));
                    }
                }
                None => return Ok(None),
            }
        }
    }
}

/// Adapter: apply a projection (map) to each row.
pub struct MapStream<S: RowStream> {
    inner: S,
    mapper: Box<dyn FnMut(Row) -> DbResult<Row> + Send>,
}

impl<S: RowStream> MapStream<S> {
    pub fn new(inner: S, mapper: impl FnMut(Row) -> DbResult<Row> + Send + 'static) -> Self {
        Self {
            inner,
            mapper: Box::new(mapper),
        }
    }
}

impl<S: RowStream> RowStream for MapStream<S> {
    fn next_row(&mut self) -> DbResult<Option<Row>> {
        match self.inner.next_row()? {
            Some(row) => Ok(Some((self.mapper)(row)?)),
            None => Ok(None),
        }
    }
}

/// Adapter: limit the number of rows from a stream.
pub struct LimitStream<S: RowStream> {
    inner: S,
    remaining: usize,
}

impl<S: RowStream> LimitStream<S> {
    pub fn new(inner: S, limit: usize) -> Self {
        Self {
            inner,
            remaining: limit,
        }
    }
}

impl<S: RowStream> RowStream for LimitStream<S> {
    fn next_row(&mut self) -> DbResult<Option<Row>> {
        if self.remaining == 0 {
            return Ok(None);
        }
        self.remaining -= 1;
        self.inner.next_row()
    }
}

/// Adapter: skip the first N rows from a stream (OFFSET).
pub struct OffsetStream<S: RowStream> {
    inner: S,
    to_skip: usize,
    skipped: bool,
}

impl<S: RowStream> OffsetStream<S> {
    pub fn new(inner: S, offset: usize) -> Self {
        Self {
            inner,
            to_skip: offset,
            skipped: false,
        }
    }
}

impl<S: RowStream> RowStream for OffsetStream<S> {
    fn next_row(&mut self) -> DbResult<Option<Row>> {
        if !self.skipped {
            for _ in 0..self.to_skip {
                if self.inner.next_row()?.is_none() {
                    self.skipped = true;
                    return Ok(None);
                }
            }
            self.skipped = true;
        }
        self.inner.next_row()
    }
}

/// Adapter: wrap a storage `TupleStream` into a `RowStream`.
pub struct TupleRowStream {
    inner: Box<dyn aiondb_storage_api::TupleStream>,
}

impl TupleRowStream {
    pub fn new(inner: Box<dyn aiondb_storage_api::TupleStream>) -> Self {
        Self { inner }
    }
}

impl RowStream for TupleRowStream {
    fn next_row(&mut self) -> DbResult<Option<Row>> {
        match self.inner.next()? {
            Some(record) => Ok(Some(record.row)),
            None => Ok(None),
        }
    }
}

/// Boxed row stream for type erasure.
pub type BoxRowStream = Box<dyn RowStream>;

impl RowStream for BoxRowStream {
    fn next_row(&mut self) -> DbResult<Option<Row>> {
        (**self).next_row()
    }

    fn collect_rows(&mut self) -> DbResult<Vec<Row>> {
        (**self).collect_rows()
    }
}

/// Adapter: chain two streams together (UNION ALL).
pub struct ChainStream {
    first: BoxRowStream,
    second: BoxRowStream,
    first_exhausted: bool,
}

impl ChainStream {
    pub fn new(first: BoxRowStream, second: BoxRowStream) -> Self {
        Self {
            first,
            second,
            first_exhausted: false,
        }
    }
}

impl RowStream for ChainStream {
    fn next_row(&mut self) -> DbResult<Option<Row>> {
        if !self.first_exhausted {
            match self.first.next_row()? {
                Some(row) => return Ok(Some(row)),
                None => self.first_exhausted = true,
            }
        }
        self.second.next_row()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_core::{Row, Value};

    #[test]
    fn vec_stream_produces_all_rows() {
        let rows = vec![
            Row::new(vec![Value::Int(1)]),
            Row::new(vec![Value::Int(2)]),
            Row::new(vec![Value::Int(3)]),
        ];
        let mut stream = VecRowStream::new(rows);
        assert_eq!(
            stream.next_row().unwrap(),
            Some(Row::new(vec![Value::Int(1)]))
        );
        assert_eq!(
            stream.next_row().unwrap(),
            Some(Row::new(vec![Value::Int(2)]))
        );
        assert_eq!(
            stream.next_row().unwrap(),
            Some(Row::new(vec![Value::Int(3)]))
        );
        assert_eq!(stream.next_row().unwrap(), None);
    }

    #[test]
    fn empty_stream_returns_none() {
        let mut stream = VecRowStream::empty();
        assert_eq!(stream.next_row().unwrap(), None);
    }

    #[test]
    fn collect_rows_gathers_all() {
        let rows = vec![Row::new(vec![Value::Int(1)]), Row::new(vec![Value::Int(2)])];
        let mut stream = VecRowStream::new(rows.clone());
        let collected = stream.collect_rows().unwrap();
        assert_eq!(collected, rows);
    }

    #[test]
    fn filter_stream_removes_rows() {
        let rows = vec![
            Row::new(vec![Value::Int(1)]),
            Row::new(vec![Value::Int(2)]),
            Row::new(vec![Value::Int(3)]),
            Row::new(vec![Value::Int(4)]),
        ];
        let inner = VecRowStream::new(rows);
        let mut stream = FilterStream::new(inner, |row| {
            Ok(matches!(row.values.first(), Some(Value::Int(n)) if n % 2 == 0))
        });
        let collected = stream.collect_rows().unwrap();
        assert_eq!(collected.len(), 2);
        assert_eq!(collected[0].values[0], Value::Int(2));
        assert_eq!(collected[1].values[0], Value::Int(4));
    }

    #[test]
    fn map_stream_transforms_rows() {
        let rows = vec![Row::new(vec![Value::Int(1)]), Row::new(vec![Value::Int(2)])];
        let inner = VecRowStream::new(rows);
        let mut stream = MapStream::new(inner, |row| {
            let val = match row.values.first() {
                Some(Value::Int(n)) => Value::Int(n * 10),
                _ => Value::Null,
            };
            Ok(Row::new(vec![val]))
        });
        let collected = stream.collect_rows().unwrap();
        assert_eq!(collected[0].values[0], Value::Int(10));
        assert_eq!(collected[1].values[0], Value::Int(20));
    }

    #[test]
    fn limit_stream_caps_output() {
        let rows = vec![
            Row::new(vec![Value::Int(1)]),
            Row::new(vec![Value::Int(2)]),
            Row::new(vec![Value::Int(3)]),
        ];
        let inner = VecRowStream::new(rows);
        let mut stream = LimitStream::new(inner, 2);
        let collected = stream.collect_rows().unwrap();
        assert_eq!(collected.len(), 2);
    }

    #[test]
    fn offset_stream_skips_rows() {
        let rows = vec![
            Row::new(vec![Value::Int(1)]),
            Row::new(vec![Value::Int(2)]),
            Row::new(vec![Value::Int(3)]),
        ];
        let inner = VecRowStream::new(rows);
        let mut stream = OffsetStream::new(inner, 1);
        let collected = stream.collect_rows().unwrap();
        assert_eq!(collected.len(), 2);
        assert_eq!(collected[0].values[0], Value::Int(2));
    }

    #[test]
    fn chain_stream_concatenates() {
        let first = vec![Row::new(vec![Value::Int(1)])];
        let second = vec![Row::new(vec![Value::Int(2)])];
        let mut stream = ChainStream::new(
            Box::new(VecRowStream::new(first)),
            Box::new(VecRowStream::new(second)),
        );
        let collected = stream.collect_rows().unwrap();
        assert_eq!(collected.len(), 2);
        assert_eq!(collected[0].values[0], Value::Int(1));
        assert_eq!(collected[1].values[0], Value::Int(2));
    }

    #[test]
    fn limit_zero_returns_nothing() {
        let rows = vec![Row::new(vec![Value::Int(1)])];
        let inner = VecRowStream::new(rows);
        let mut stream = LimitStream::new(inner, 0);
        assert_eq!(stream.next_row().unwrap(), None);
    }

    #[test]
    fn offset_beyond_length_returns_empty() {
        let rows = vec![Row::new(vec![Value::Int(1)])];
        let inner = VecRowStream::new(rows);
        let mut stream = OffsetStream::new(inner, 100);
        assert_eq!(stream.next_row().unwrap(), None);
    }

    #[test]
    fn collect_rows_limited() {
        let rows = vec![
            Row::new(vec![Value::Int(1)]),
            Row::new(vec![Value::Int(2)]),
            Row::new(vec![Value::Int(3)]),
        ];
        let mut stream = VecRowStream::new(rows);
        let collected = stream.collect_rows_limited(2).unwrap();
        assert_eq!(collected.len(), 2);
    }
}
