//! Merged tuple stream for multi-shard scan results.

#![allow(clippy::must_use_candidate)]

use aiondb_core::DbResult;
use aiondb_storage_api::{TupleRecord, TupleStream};

/// A [`TupleStream`] that iterates over multiple shard streams
/// sequentially, producing a unified view of all shard results.
pub struct MergedTupleStream {
    streams: Vec<Box<dyn TupleStream>>,
    current: usize,
}

impl MergedTupleStream {
    pub fn new(streams: Vec<Box<dyn TupleStream>>) -> Self {
        Self {
            streams,
            current: 0,
        }
    }
}

impl TupleStream for MergedTupleStream {
    fn next(&mut self) -> DbResult<Option<TupleRecord>> {
        while self.current < self.streams.len() {
            if let Some(record) = self.streams[self.current].next()? {
                return Ok(Some(record));
            }
            self.current += 1;
        }
        Ok(None)
    }
}

/// A [`TupleStream`] wrapper that rewrites `TupleId`s to encode the shard
/// index in the high bits, preventing collisions across shards.
pub struct ShardRewriteTupleStream {
    inner: Box<dyn TupleStream>,
    shard_idx: u32,
}

impl ShardRewriteTupleStream {
    pub fn new(inner: Box<dyn TupleStream>, shard_idx: u32) -> Self {
        Self { inner, shard_idx }
    }
}

impl TupleStream for ShardRewriteTupleStream {
    fn next(&mut self) -> DbResult<Option<TupleRecord>> {
        match self.inner.next()? {
            Some(mut rec) => {
                rec.tuple_id =
                    crate::storage::try_encode_shard_tuple_id(self.shard_idx, rec.tuple_id)?;
                Ok(Some(rec))
            }
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_core::{Row, TupleId, Value};
    use aiondb_storage_api::VecTupleStream;

    fn make_record(tid: u64, val: i32) -> TupleRecord {
        TupleRecord {
            tuple_id: TupleId::new(tid),
            heap_position: tid,
            row: Row {
                values: vec![Value::Int(val)],
            },
        }
    }

    #[test]
    fn empty_streams() {
        let mut merged = MergedTupleStream::new(vec![]);
        assert!(merged.next().unwrap().is_none());
    }

    #[test]
    fn single_stream() {
        let records = vec![make_record(1, 10), make_record(2, 20)];
        let stream: Box<dyn TupleStream> = Box::new(VecTupleStream::new(records));
        let mut merged = MergedTupleStream::new(vec![stream]);

        let r1 = merged.next().unwrap().unwrap();
        assert_eq!(r1.tuple_id, TupleId::new(1));
        let r2 = merged.next().unwrap().unwrap();
        assert_eq!(r2.tuple_id, TupleId::new(2));
        assert!(merged.next().unwrap().is_none());
    }

    #[test]
    fn multiple_streams_concatenated() {
        let s1: Box<dyn TupleStream> = Box::new(VecTupleStream::new(vec![make_record(1, 10)]));
        let s2: Box<dyn TupleStream> = Box::new(VecTupleStream::new(vec![
            make_record(2, 20),
            make_record(3, 30),
        ]));
        let s3: Box<dyn TupleStream> = Box::new(VecTupleStream::new(vec![]));
        let s4: Box<dyn TupleStream> = Box::new(VecTupleStream::new(vec![make_record(4, 40)]));

        let mut merged = MergedTupleStream::new(vec![s1, s2, s3, s4]);

        let mut ids = Vec::new();
        while let Some(rec) = merged.next().unwrap() {
            ids.push(rec.tuple_id.get());
        }
        assert_eq!(ids, vec![1, 2, 3, 4]);
    }

    #[test]
    fn skips_empty_streams() {
        let s1: Box<dyn TupleStream> = Box::new(VecTupleStream::new(vec![]));
        let s2: Box<dyn TupleStream> = Box::new(VecTupleStream::new(vec![]));
        let s3: Box<dyn TupleStream> = Box::new(VecTupleStream::new(vec![make_record(1, 100)]));

        let mut merged = MergedTupleStream::new(vec![s1, s2, s3]);
        let r = merged.next().unwrap().unwrap();
        assert_eq!(r.tuple_id, TupleId::new(1));
        assert!(merged.next().unwrap().is_none());
    }

    #[test]
    fn shard_rewrite_rejects_local_tuple_id_overflow() {
        let stream: Box<dyn TupleStream> = Box::new(VecTupleStream::new(vec![make_record(
            crate::storage::LOCAL_TID_MASK + 1,
            100,
        )]));
        let mut rewritten = ShardRewriteTupleStream::new(stream, 1);
        let err = rewritten
            .next()
            .expect_err("overflowing shard-local tuple id must fail");
        assert!(
            err.to_string().contains("exceeds 48-bit"),
            "unexpected error: {err}"
        );
    }
}
