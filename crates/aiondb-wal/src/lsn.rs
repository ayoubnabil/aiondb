use std::fmt;

/// Log Sequence Number -- monotonically increasing position in the WAL.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Lsn(u64);

impl Lsn {
    pub const ZERO: Lsn = Lsn(0);

    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    /// Parse an LSN from decimal (`12345`) or PostgreSQL-style hex (`0/1A3F`).
    ///
    /// Trailing statement delimiters (`;`, `,`) are ignored so values can be
    /// copied directly from SQL or logs.
    pub fn from_str_value(value: &str) -> Option<Self> {
        let trimmed = value.trim().trim_end_matches(';').trim_end_matches(',');
        if let Some((high, low)) = trimmed.split_once('/') {
            // PostgreSQL LSN halves are 32-bit hexadecimal values.
            let high = u32::from_str_radix(high.trim_start_matches("0x"), 16).ok()?;
            let low = u32::from_str_radix(low.trim_start_matches("0x"), 16).ok()?;
            return Some(Self((u64::from(high) << 32) | u64::from(low)));
        }
        trimmed.parse::<u64>().ok().map(Self)
    }

    /// Advance the LSN by the given number of bytes.
    /// Uses saturating arithmetic on overflow.
    pub fn advance(self, bytes: u64) -> Self {
        Self(self.0.saturating_add(bytes))
    }

    /// Advance the LSN, returning `None` when the position would overflow.
    pub fn checked_advance(self, bytes: u64) -> Option<Self> {
        self.0.checked_add(bytes).map(Self)
    }
}

impl fmt::Display for Lsn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:016X}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_constant_is_zero() {
        assert_eq!(Lsn::ZERO.get(), 0);
    }

    #[test]
    fn new_returns_value() {
        assert_eq!(Lsn::new(42).get(), 42);
    }

    #[test]
    fn new_zero() {
        assert_eq!(Lsn::new(0).get(), 0);
    }

    #[test]
    fn new_u64_max() {
        assert_eq!(Lsn::new(u64::MAX).get(), u64::MAX);
    }

    #[test]
    fn get_returns_inner() {
        let lsn = Lsn::new(123_456);
        assert_eq!(lsn.get(), 123_456);
    }

    #[test]
    fn advance_adds_bytes() {
        let lsn = Lsn::new(100);
        assert_eq!(lsn.advance(50).get(), 150);
    }

    #[test]
    fn advance_zero_is_identity() {
        let lsn = Lsn::new(42);
        assert_eq!(lsn.advance(0), lsn);
    }

    #[test]
    fn advance_from_zero() {
        assert_eq!(Lsn::ZERO.advance(10).get(), 10);
    }

    #[test]
    fn display_zero_padded_hex() {
        assert_eq!(Lsn::ZERO.to_string(), "0000000000000000");
    }

    #[test]
    fn display_nonzero() {
        assert_eq!(Lsn::new(255).to_string(), "00000000000000FF");
    }

    #[test]
    fn display_large_value() {
        assert_eq!(Lsn::new(u64::MAX).to_string(), "FFFFFFFFFFFFFFFF");
    }

    #[test]
    fn ord_less_than() {
        assert!(Lsn::new(1) < Lsn::new(2));
    }

    #[test]
    fn ord_greater_than() {
        assert!(Lsn::new(10) > Lsn::new(5));
    }

    #[test]
    fn ord_equal() {
        assert!((Lsn::new(7) >= Lsn::new(7)));
        assert!((Lsn::new(7) <= Lsn::new(7)));
    }

    #[test]
    fn default_is_zero() {
        assert_eq!(Lsn::default(), Lsn::ZERO);
    }

    #[test]
    fn default_get_is_zero() {
        assert_eq!(Lsn::default().get(), 0);
    }

    #[test]
    fn copy_semantics() {
        let a = Lsn::new(42);
        let b = a;
        assert_eq!(a, b);
    }

    #[test]
    fn clone_equals_original() {
        let a = Lsn::new(999);
        assert_eq!(a, a.clone());
    }

    #[test]
    fn hash_same_values_consistent() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(Lsn::new(42));
        set.insert(Lsn::new(42));
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn hash_different_values_distinct() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(Lsn::new(1));
        set.insert(Lsn::new(2));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn zero_equals_new_zero() {
        assert_eq!(Lsn::ZERO, Lsn::new(0));
    }

    #[test]
    fn advance_chained() {
        let lsn = Lsn::ZERO.advance(10).advance(20).advance(30);
        assert_eq!(lsn.get(), 60);
    }

    #[test]
    fn checked_advance_reports_overflow() {
        assert_eq!(
            Lsn::new(u64::MAX - 1).checked_advance(1),
            Some(Lsn::new(u64::MAX))
        );
        assert_eq!(Lsn::new(u64::MAX).checked_advance(1), None);
    }

    #[test]
    fn from_str_value_parses_decimal() {
        assert_eq!(Lsn::from_str_value("42"), Some(Lsn::new(42)));
    }

    #[test]
    fn from_str_value_parses_pg_hex() {
        assert_eq!(Lsn::from_str_value("0/1A3F"), Some(Lsn::new(0x1A3F)));
        assert_eq!(
            Lsn::from_str_value("0x1/0x10,"),
            Some(Lsn::new((1u64 << 32) | 0x10))
        );
    }

    #[test]
    fn from_str_value_rejects_invalid() {
        assert_eq!(Lsn::from_str_value("abc"), None);
    }

    #[test]
    fn from_str_value_rejects_oversized_pg_half() {
        assert_eq!(Lsn::from_str_value("100000000/0"), None);
        assert_eq!(Lsn::from_str_value("0/100000000"), None);
    }
}
