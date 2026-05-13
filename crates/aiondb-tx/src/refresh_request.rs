//! Refresh requests.
//!
//! When a write's commit ts is pushed forward by the timestamp cache,
//! the originating transaction needs to **refresh** its read spans :
//! re-verify that no concurrent reader has observed a value above the
//! new commit ts. If any span has been re-read at a conflicting ts,
//! the txn restarts. Otherwise the commit proceeds at the bumped ts.
//!
//! This module provides the bookkeeping and the verification routine.

use std::sync::Arc;

use crate::hlc::HlcTimestamp;
use crate::intent_registry::IntentRangeId;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefreshSpan {
    pub range: IntentRangeId,
    pub start_key: Vec<u8>,
    pub end_key: Vec<u8>,
    pub original_read_ts: HlcTimestamp,
}

#[derive(Clone, Debug, Default)]
pub struct RefreshRequest {
    spans: Vec<RefreshSpan>,
}

impl RefreshRequest {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, span: RefreshSpan) {
        self.spans.push(span);
    }

    pub fn spans(&self) -> &[RefreshSpan] {
        &self.spans
    }
}

/// Verifies a refresh request against an external "max read at" oracle.
/// The oracle is typically backed by [`crate::timestamp_cache::TimestampCache`].
#[derive(Clone)]
pub struct RefreshVerifier {
    oracle: Arc<dyn Fn(&RefreshSpan) -> HlcTimestamp + Send + Sync>,
}

impl RefreshVerifier {
    pub fn new(oracle: Arc<dyn Fn(&RefreshSpan) -> HlcTimestamp + Send + Sync>) -> Self {
        Self { oracle }
    }

    pub fn verify(&self, request: &RefreshRequest, new_commit_ts: HlcTimestamp) -> RefreshOutcome {
        let mut conflicts = Vec::new();
        for span in request.spans() {
            let observed = (self.oracle)(span);
            // Conflict iff someone read above the original read_ts but
            // still below the new commit_ts. That window is exactly the
            // refresh interval.
            if observed > span.original_read_ts && observed < new_commit_ts {
                conflicts.push((span.clone(), observed));
            }
        }
        if conflicts.is_empty() {
            RefreshOutcome::Succeeded
        } else {
            RefreshOutcome::Conflict { conflicts }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RefreshOutcome {
    Succeeded,
    Conflict {
        conflicts: Vec<(RefreshSpan, HlcTimestamp)>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(wall: u64) -> HlcTimestamp {
        HlcTimestamp::new(wall, 0)
    }

    fn span() -> RefreshSpan {
        RefreshSpan {
            range: IntentRangeId(1),
            start_key: b"a".to_vec(),
            end_key: b"m".to_vec(),
            original_read_ts: ts(100),
        }
    }

    #[test]
    fn no_conflict_when_no_new_reads_above_original() {
        let mut req = RefreshRequest::new();
        req.add(span());
        let verifier = RefreshVerifier::new(Arc::new(|_| ts(100)));
        assert_eq!(verifier.verify(&req, ts(200)), RefreshOutcome::Succeeded);
    }

    #[test]
    fn conflict_when_observed_in_refresh_interval() {
        let mut req = RefreshRequest::new();
        req.add(span());
        let verifier = RefreshVerifier::new(Arc::new(|_| ts(150)));
        match verifier.verify(&req, ts(200)) {
            RefreshOutcome::Conflict { conflicts } => {
                assert_eq!(conflicts.len(), 1);
                assert_eq!(conflicts[0].1, ts(150));
            }
            other => panic!("expected Conflict, got {other:?}"),
        }
    }

    #[test]
    fn observation_above_new_commit_is_not_a_conflict() {
        // Means another writer already won at an even higher ts; our
        // refresh isn't what blocks us.
        let mut req = RefreshRequest::new();
        req.add(span());
        let verifier = RefreshVerifier::new(Arc::new(|_| ts(500)));
        assert_eq!(verifier.verify(&req, ts(200)), RefreshOutcome::Succeeded);
    }

    #[test]
    fn observation_at_or_below_original_read_is_not_a_conflict() {
        let mut req = RefreshRequest::new();
        req.add(span());
        let verifier = RefreshVerifier::new(Arc::new(|_| ts(50)));
        assert_eq!(verifier.verify(&req, ts(200)), RefreshOutcome::Succeeded);
    }

    #[test]
    fn empty_request_always_succeeds() {
        let req = RefreshRequest::new();
        let verifier = RefreshVerifier::new(Arc::new(|_| ts(9999)));
        assert_eq!(verifier.verify(&req, ts(200)), RefreshOutcome::Succeeded);
    }
}
