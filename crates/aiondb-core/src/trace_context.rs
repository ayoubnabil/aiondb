//! Distributed trace context.
//!
//! Lightweight propagation of `(trace_id, span_id, parent_span_id)`
//! across cluster boundaries. Compatible with the W3C Trace Context
//! header format so external tooling (OpenTelemetry, Jaeger) can
//! ingest the same identifiers.
//!
//! Generators are deterministic when seeded so tests can assert on
//! produced ids.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// 128-bit trace id.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TraceId(pub u128);

impl TraceId {
    pub fn new(value: u128) -> Self {
        Self(value)
    }
    pub fn is_invalid(&self) -> bool {
        self.0 == 0
    }
}

impl std::fmt::Display for TraceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:032x}", self.0)
    }
}

/// 64-bit span id.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SpanId(pub u64);

impl SpanId {
    pub fn new(value: u64) -> Self {
        Self(value)
    }
    pub fn is_invalid(&self) -> bool {
        self.0 == 0
    }
}

impl std::fmt::Display for SpanId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct TraceContext {
    pub trace_id: TraceId,
    pub span_id: SpanId,
    pub parent_span_id: Option<SpanId>,
}

impl TraceContext {
    pub fn new(trace_id: TraceId, span_id: SpanId, parent_span_id: Option<SpanId>) -> Self {
        Self {
            trace_id,
            span_id,
            parent_span_id,
        }
    }

    /// Encode as W3C `traceparent` header.
    /// `00-<trace_id_32_hex>-<span_id_16_hex>-01`
    pub fn to_traceparent(&self) -> String {
        format!("00-{}-{}-01", self.trace_id, self.span_id)
    }

    /// Parse a W3C `traceparent` header. Returns `None` on malformed
    /// input.
    pub fn from_traceparent(header: &str) -> Option<Self> {
        let parts: Vec<&str> = header.split('-').collect();
        if parts.len() != 4 || parts[0] != "00" {
            return None;
        }
        let trace_id = u128::from_str_radix(parts[1], 16).ok()?;
        let span_id = u64::from_str_radix(parts[2], 16).ok()?;
        Some(Self {
            trace_id: TraceId(trace_id),
            span_id: SpanId(span_id),
            parent_span_id: None,
        })
    }

    /// Spawn a child span sharing the same trace id, with the current
    /// span as parent.
    pub fn child(&self, child_span_id: SpanId) -> Self {
        Self {
            trace_id: self.trace_id,
            span_id: child_span_id,
            parent_span_id: Some(self.span_id),
        }
    }
}

/// Generator for fresh span ids.
#[derive(Clone, Debug)]
pub struct SpanIdGenerator {
    counter: Arc<AtomicU64>,
}

impl SpanIdGenerator {
    pub fn new(seed: u64) -> Self {
        Self {
            counter: Arc::new(AtomicU64::new(seed.max(1))),
        }
    }

    pub fn next(&self) -> SpanId {
        SpanId(self.counter.fetch_add(1, Ordering::SeqCst))
    }
}

impl Default for SpanIdGenerator {
    fn default() -> Self {
        Self::new(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traceparent_round_trips() {
        let ctx = TraceContext::new(
            TraceId(0x1234_5678_9ABC_DEF0_1122_3344_5566_7788),
            SpanId(0xCAFE_BABE_DEAD_BEEF),
            None,
        );
        let header = ctx.to_traceparent();
        assert!(header.starts_with("00-"));
        let parsed = TraceContext::from_traceparent(&header).unwrap();
        assert_eq!(parsed.trace_id, ctx.trace_id);
        assert_eq!(parsed.span_id, ctx.span_id);
    }

    #[test]
    fn malformed_header_returns_none() {
        assert!(TraceContext::from_traceparent("garbage").is_none());
        assert!(TraceContext::from_traceparent("01-deadbeef-cafe-01").is_none());
    }

    #[test]
    fn child_preserves_trace_id_and_records_parent() {
        let parent = TraceContext::new(TraceId(42), SpanId(7), None);
        let child = parent.child(SpanId(99));
        assert_eq!(child.trace_id, parent.trace_id);
        assert_eq!(child.parent_span_id, Some(SpanId(7)));
        assert_eq!(child.span_id, SpanId(99));
    }

    #[test]
    fn span_generator_is_monotonic_and_thread_safe() {
        let gen = SpanIdGenerator::new(1);
        let mut handles = Vec::new();
        for _ in 0..8 {
            let g = gen.clone();
            handles.push(std::thread::spawn(move || {
                let mut local = Vec::new();
                for _ in 0..1000 {
                    local.push(g.next());
                }
                local
            }));
        }
        let mut all: Vec<SpanId> = handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect();
        all.sort();
        for w in all.windows(2) {
            assert!(w[0] < w[1], "duplicate span id: {:?}", w);
        }
    }

    #[test]
    fn trace_id_format_pads_to_32_hex_chars() {
        let id = TraceId(0xCAFE);
        assert_eq!(id.to_string().len(), 32);
        assert!(id.to_string().ends_with("cafe"));
    }
}
