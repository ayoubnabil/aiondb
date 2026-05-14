#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    pub fn merge(self, other: Span) -> Self {
        Self {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_zero_width_span() {
        let span = Span::new(0, 0);
        assert_eq!(span.start, 0);
        assert_eq!(span.end, 0);
    }

    #[test]
    fn new_normal_span() {
        let span = Span::new(5, 10);
        assert_eq!(span.start, 5);
        assert_eq!(span.end, 10);
    }

    #[test]
    fn merge_disjoint_spans() {
        let a = Span::new(0, 5);
        let b = Span::new(10, 15);
        let merged = a.merge(b);
        assert_eq!(merged.start, 0);
        assert_eq!(merged.end, 15);
    }

    #[test]
    fn merge_overlapping_spans() {
        let a = Span::new(3, 8);
        let b = Span::new(5, 12);
        let merged = a.merge(b);
        assert_eq!(merged.start, 3);
        assert_eq!(merged.end, 12);
    }

    #[test]
    fn merge_identical_spans() {
        let a = Span::new(4, 9);
        let b = Span::new(4, 9);
        let merged = a.merge(b);
        assert_eq!(merged, a);
        assert_eq!(merged, b);
    }

    #[test]
    fn merge_reversed_order() {
        // second span starts before first
        let a = Span::new(10, 20);
        let b = Span::new(2, 7);
        let merged = a.merge(b);
        assert_eq!(merged.start, 2);
        assert_eq!(merged.end, 20);
    }

    #[test]
    fn default_produces_zero_span() {
        let span = Span::default();
        assert_eq!(span.start, 0);
        assert_eq!(span.end, 0);
    }

    #[test]
    fn copy_semantics() {
        let a = Span::new(1, 5);
        let b = a; // Copy
        assert_eq!(a, b);
        // a is still usable after copy
        assert_eq!(a.start, 1);
        assert_eq!(b.end, 5);
    }
}
