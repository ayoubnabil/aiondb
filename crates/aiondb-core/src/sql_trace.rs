//! Per-SQL-query trace correlation.
//!
//! Binds a [`TraceContext`] to one in-flight query so every log line,
//! metric and span shares the same trace id. Used by the SQL layer to
//! propagate context through gossip and Raft messages.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::trace_context::{SpanId, TraceContext, TraceId};

#[derive(Clone, Debug)]
pub struct QueryTrace {
    pub trace_id: TraceId,
    pub span_id: SpanId,
    pub query_text: String,
    pub started_at_us: u64,
    pub completed_at_us: Option<u64>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct QueryTraceRegistry {
    inner: Arc<std::sync::Mutex<BTreeMap<SpanId, QueryTrace>>>,
}

impl QueryTraceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn start(&self, ctx: TraceContext, query_text: impl Into<String>) {
        let started_at_us = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .ok()
            .and_then(|d| u64::try_from(d.as_micros()).ok())
            .unwrap_or(0);
        self.inner.lock().unwrap().insert(
            ctx.span_id,
            QueryTrace {
                trace_id: ctx.trace_id,
                span_id: ctx.span_id,
                query_text: query_text.into(),
                started_at_us,
                completed_at_us: None,
                error: None,
            },
        );
    }

    pub fn complete(&self, span: SpanId, error: Option<String>) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .ok()
            .and_then(|d| u64::try_from(d.as_micros()).ok())
            .unwrap_or(0);
        if let Some(t) = self.inner.lock().unwrap().get_mut(&span) {
            t.completed_at_us = Some(now);
            t.error = error;
        }
    }

    pub fn lookup(&self, span: SpanId) -> Option<QueryTrace> {
        self.inner.lock().unwrap().get(&span).cloned()
    }

    pub fn forget(&self, span: SpanId) {
        self.inner.lock().unwrap().remove(&span);
    }

    pub fn snapshot(&self) -> Vec<QueryTrace> {
        let guard = self.inner.lock().unwrap();
        guard.values().cloned().collect()
    }

    pub fn active_count(&self) -> usize {
        self.inner
            .lock()
            .unwrap()
            .values()
            .filter(|t| t.completed_at_us.is_none())
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_then_complete_marks_completion_time() {
        let r = QueryTraceRegistry::new();
        let ctx = TraceContext::new(TraceId(1), SpanId(7), None);
        r.start(ctx, "SELECT 1");
        std::thread::sleep(std::time::Duration::from_millis(2));
        r.complete(SpanId(7), None);
        let t = r.lookup(SpanId(7)).unwrap();
        assert!(t.completed_at_us.is_some());
        assert!(t.error.is_none());
    }

    #[test]
    fn complete_with_error_records_message() {
        let r = QueryTraceRegistry::new();
        let ctx = TraceContext::new(TraceId(1), SpanId(7), None);
        r.start(ctx, "SELECT crash");
        r.complete(SpanId(7), Some("boom".into()));
        let t = r.lookup(SpanId(7)).unwrap();
        assert_eq!(t.error.as_deref(), Some("boom"));
    }

    #[test]
    fn active_count_reflects_pending_queries() {
        let r = QueryTraceRegistry::new();
        let a = TraceContext::new(TraceId(1), SpanId(1), None);
        let b = TraceContext::new(TraceId(2), SpanId(2), None);
        r.start(a, "q1");
        r.start(b, "q2");
        assert_eq!(r.active_count(), 2);
        r.complete(SpanId(1), None);
        assert_eq!(r.active_count(), 1);
    }

    #[test]
    fn forget_drops_entry() {
        let r = QueryTraceRegistry::new();
        r.start(TraceContext::new(TraceId(1), SpanId(99), None), "q");
        r.forget(SpanId(99));
        assert!(r.lookup(SpanId(99)).is_none());
    }

    #[test]
    fn snapshot_returns_all_traces() {
        let r = QueryTraceRegistry::new();
        r.start(TraceContext::new(TraceId(1), SpanId(1), None), "a");
        r.start(TraceContext::new(TraceId(1), SpanId(2), None), "b");
        let snap = r.snapshot();
        assert_eq!(snap.len(), 2);
    }
}
