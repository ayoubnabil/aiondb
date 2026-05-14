//! Trace-aware request dispatch.
//!
//! Wraps a request with a trace context so per-shard executors can
//! correlate logs and per-span timings end-to-end.

use aiondb_core::trace_context::TraceContext;

#[derive(Clone, Debug)]
pub struct TracedRequest<T> {
    pub trace: TraceContext,
    pub payload: T,
}

impl<T> TracedRequest<T> {
    pub fn new(trace: TraceContext, payload: T) -> Self {
        Self { trace, payload }
    }

    pub fn map<U, F: FnOnce(T) -> U>(self, f: F) -> TracedRequest<U> {
        TracedRequest {
            trace: self.trace,
            payload: f(self.payload),
        }
    }
}

#[derive(Clone, Debug)]
pub struct TracedResponse<T> {
    pub trace: TraceContext,
    pub payload: T,
    pub elapsed_us: u64,
}

#[cfg(test)]
mod tests {
    use aiondb_core::trace_context::{SpanId, TraceContext, TraceId};

    use super::*;

    #[test]
    fn map_preserves_trace_context() {
        let ctx = TraceContext::new(TraceId(7), SpanId(13), None);
        let req = TracedRequest::new(ctx, 42u32);
        let mapped = req.map(|n| n * 2);
        assert_eq!(mapped.payload, 84);
        assert_eq!(mapped.trace.trace_id, TraceId(7));
    }

    #[test]
    fn response_carries_elapsed_time() {
        let ctx = TraceContext::new(TraceId(1), SpanId(2), None);
        let resp = TracedResponse {
            trace: ctx,
            payload: "ok",
            elapsed_us: 123,
        };
        assert_eq!(resp.elapsed_us, 123);
        assert_eq!(resp.payload, "ok");
    }

    #[test]
    fn traced_request_clones() {
        let ctx = TraceContext::new(TraceId(1), SpanId(2), None);
        let req = TracedRequest::new(ctx, vec![1u8, 2, 3]);
        let copy = req.clone();
        assert_eq!(copy.payload, vec![1, 2, 3]);
    }
}
