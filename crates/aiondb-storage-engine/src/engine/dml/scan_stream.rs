use aiondb_core::DbResult;
use aiondb_storage_api::{TupleRecord, TupleStream};

pub(super) struct KeyedTupleStream<K> {
    records: std::vec::IntoIter<(K, TupleRecord)>,
}

impl<K> KeyedTupleStream<K> {
    pub(super) fn new(records: Vec<(K, TupleRecord)>) -> Self {
        Self {
            records: records.into_iter(),
        }
    }
}

impl<K: Send> TupleStream for KeyedTupleStream<K> {
    fn next(&mut self) -> DbResult<Option<TupleRecord>> {
        Ok(self.records.next().map(|(_, record)| record))
    }
}
