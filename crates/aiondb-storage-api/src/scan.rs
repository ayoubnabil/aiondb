#![allow(
    clippy::cast_possible_wrap,
    clippy::missing_errors_doc,
    clippy::must_use_candidate
)]

use aiondb_core::DbResult;

use crate::TupleRecord;

#[allow(clippy::missing_errors_doc)]
pub trait TupleStream: Send {
    fn next(&mut self) -> DbResult<Option<TupleRecord>>;
}

pub struct VecTupleStream {
    records: std::vec::IntoIter<TupleRecord>,
}

pub struct OnceTupleStream {
    record: Option<TupleRecord>,
}

impl VecTupleStream {
    pub fn new(records: Vec<TupleRecord>) -> Self {
        Self {
            records: records.into_iter(),
        }
    }
}

impl OnceTupleStream {
    pub fn new(record: TupleRecord) -> Self {
        Self {
            record: Some(record),
        }
    }
}

impl TupleStream for VecTupleStream {
    fn next(&mut self) -> DbResult<Option<TupleRecord>> {
        Ok(self.records.next())
    }
}

impl TupleStream for OnceTupleStream {
    fn next(&mut self) -> DbResult<Option<TupleRecord>> {
        Ok(self.record.take())
    }
}

/// Adapter that filters an inner `TupleStream` so each parallel worker only
/// sees a deterministic share of records, partitioned by `tuple_id % num_workers`.
/// Used by the Gather node to split a sequential scan across worker threads
/// without changing storage layout.
pub struct PartitionFilterStream {
    inner: Box<dyn TupleStream>,
    worker_id: u32,
    num_workers: u32,
}

impl PartitionFilterStream {
    pub fn new(inner: Box<dyn TupleStream>, worker_id: u32, num_workers: u32) -> Self {
        debug_assert!(num_workers >= 1);
        debug_assert!(worker_id < num_workers.max(1));
        Self {
            inner,
            worker_id,
            num_workers: num_workers.max(1),
        }
    }
}

impl TupleStream for PartitionFilterStream {
    fn next(&mut self) -> DbResult<Option<TupleRecord>> {
        while let Some(record) = self.inner.next()? {
            let bucket = (record.tuple_id.get() % u64::from(self.num_workers)) as u32;
            if bucket == self.worker_id {
                return Ok(Some(record));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_core::{Row, TupleId, Value};

    fn tuple_record(id: u64) -> TupleRecord {
        TupleRecord {
            tuple_id: TupleId::new(id),
            heap_position: id,
            row: Row::new(vec![Value::BigInt(i64::try_from(id).unwrap_or(i64::MAX))]),
        }
    }

    #[test]
    fn once_tuple_stream_yields_record_once() {
        let record = tuple_record(7);
        let mut stream = OnceTupleStream::new(record.clone());

        assert_eq!(stream.next().unwrap(), Some(record));
        assert_eq!(stream.next().unwrap(), None);
    }

    #[test]
    fn partition_filter_stream_disjoint_union_covers_all_records() {
        let records: Vec<TupleRecord> = (1..=20).map(tuple_record).collect();
        let workers = 4u32;
        let mut union: Vec<u64> = Vec::new();
        for worker_id in 0..workers {
            let mut stream = PartitionFilterStream::new(
                Box::new(VecTupleStream::new(records.clone())),
                worker_id,
                workers,
            );
            while let Some(record) = stream.next().unwrap() {
                let tid = record.tuple_id.get();
                assert_eq!(
                    (tid % u64::from(workers)) as u32,
                    worker_id,
                    "worker {worker_id} got tuple {tid}"
                );
                union.push(tid);
            }
        }
        union.sort_unstable();
        let expected: Vec<u64> = (1..=20).collect();
        assert_eq!(
            union, expected,
            "disjoint workers must cover every tuple once"
        );
    }

    #[test]
    fn vec_tuple_stream_yields_records_in_order() {
        let records = vec![tuple_record(1), tuple_record(2)];
        let mut stream = VecTupleStream::new(records.clone());

        assert_eq!(stream.next().unwrap(), Some(records[0].clone()));
        assert_eq!(stream.next().unwrap(), Some(records[1].clone()));
        assert_eq!(stream.next().unwrap(), None);
    }
}
