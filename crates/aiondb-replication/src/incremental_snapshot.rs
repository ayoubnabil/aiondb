//! Incremental snapshot streamer.
//!
//! Streams snapshot bytes to a follower as a sequence of chunks.
//! Each chunk carries a sequential index; the follower acks the
//! index back. If the connection drops, the sender resumes from
//! `last_acked + 1` instead of restarting from byte 0.

use std::sync::Arc;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotChunk {
    pub stream_id: u64,
    pub chunk_index: u64,
    pub bytes: Vec<u8>,
    pub final_chunk: bool,
}

#[derive(Clone, Debug, Default)]
pub struct IncrementalSnapshotter {
    inner: Arc<std::sync::Mutex<State>>,
}

#[derive(Default, Debug)]
struct State {
    streams: std::collections::BTreeMap<u64, StreamState>,
}

#[derive(Debug)]
struct StreamState {
    chunks: Vec<Vec<u8>>,
    last_acked: Option<u64>,
    finalized: bool,
}

impl IncrementalSnapshotter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn open(&self, stream_id: u64) {
        let mut g = self.inner.lock().unwrap();
        g.streams.insert(
            stream_id,
            StreamState {
                chunks: Vec::new(),
                last_acked: None,
                finalized: false,
            },
        );
    }

    pub fn push_chunk(&self, stream_id: u64, bytes: Vec<u8>) -> u64 {
        let mut g = self.inner.lock().unwrap();
        let s = g.streams.get_mut(&stream_id).expect("stream open");
        s.chunks.push(bytes);
        (s.chunks.len() - 1) as u64
    }

    pub fn finalize(&self, stream_id: u64) {
        let mut g = self.inner.lock().unwrap();
        if let Some(s) = g.streams.get_mut(&stream_id) {
            s.finalized = true;
        }
    }

    pub fn ack(&self, stream_id: u64, chunk_index: u64) -> bool {
        let mut g = self.inner.lock().unwrap();
        let Some(s) = g.streams.get_mut(&stream_id) else {
            return false;
        };
        match s.last_acked {
            Some(last) if chunk_index <= last => return false,
            _ => {}
        }
        if (chunk_index as usize) >= s.chunks.len() {
            return false;
        }
        s.last_acked = Some(chunk_index);
        true
    }

    pub fn next_to_send(&self, stream_id: u64) -> Option<SnapshotChunk> {
        let g = self.inner.lock().unwrap();
        let s = g.streams.get(&stream_id)?;
        let idx = s.last_acked.map(|i| i + 1).unwrap_or(0);
        if (idx as usize) >= s.chunks.len() {
            return None;
        }
        let final_chunk = s.finalized && (idx as usize) == s.chunks.len() - 1;
        Some(SnapshotChunk {
            stream_id,
            chunk_index: idx,
            bytes: s.chunks[idx as usize].clone(),
            final_chunk,
        })
    }

    pub fn progress(&self, stream_id: u64) -> Option<(u64, u64)> {
        let g = self.inner.lock().unwrap();
        let s = g.streams.get(&stream_id)?;
        let total = s.chunks.len() as u64;
        let acked = s.last_acked.map(|i| i + 1).unwrap_or(0);
        Some((acked, total))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_and_push_chunks() {
        let s = IncrementalSnapshotter::new();
        s.open(1);
        let i = s.push_chunk(1, vec![1, 2, 3]);
        assert_eq!(i, 0);
    }

    #[test]
    fn next_to_send_returns_first_unacked() {
        let s = IncrementalSnapshotter::new();
        s.open(1);
        s.push_chunk(1, vec![1]);
        s.push_chunk(1, vec![2]);
        let c = s.next_to_send(1).unwrap();
        assert_eq!(c.chunk_index, 0);
        s.ack(1, 0);
        let c2 = s.next_to_send(1).unwrap();
        assert_eq!(c2.chunk_index, 1);
    }

    #[test]
    fn final_chunk_flag_set_after_finalize() {
        let s = IncrementalSnapshotter::new();
        s.open(1);
        s.push_chunk(1, vec![1]);
        s.push_chunk(1, vec![2]);
        s.finalize(1);
        s.ack(1, 0);
        let c = s.next_to_send(1).unwrap();
        assert!(c.final_chunk);
    }

    #[test]
    fn ack_cannot_regress() {
        let s = IncrementalSnapshotter::new();
        s.open(1);
        s.push_chunk(1, vec![1]);
        s.push_chunk(1, vec![2]);
        assert!(s.ack(1, 1));
        assert!(!s.ack(1, 0));
    }

    #[test]
    fn progress_reports_acked_total() {
        let s = IncrementalSnapshotter::new();
        s.open(1);
        for i in 0..3 {
            s.push_chunk(1, vec![i]);
        }
        s.ack(1, 1);
        let (acked, total) = s.progress(1).unwrap();
        assert_eq!(acked, 2);
        assert_eq!(total, 3);
    }

    #[test]
    fn next_after_complete_is_none() {
        let s = IncrementalSnapshotter::new();
        s.open(1);
        s.push_chunk(1, vec![1]);
        s.ack(1, 0);
        assert!(s.next_to_send(1).is_none());
    }
}
