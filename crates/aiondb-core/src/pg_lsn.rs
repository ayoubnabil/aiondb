use std::fmt;

#[derive(
    Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd, serde::Serialize, serde::Deserialize,
)]
pub struct PgLsnValue(u64);

impl PgLsnValue {
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }

    #[must_use]
    pub fn parse(input: &str) -> Option<Self> {
        let (hi, lo) = input.split_once('/')?;
        if hi.is_empty() || lo.is_empty() || hi.len() > 8 || lo.len() > 8 {
            return None;
        }
        if !hi.chars().all(|ch| ch.is_ascii_hexdigit())
            || !lo.chars().all(|ch| ch.is_ascii_hexdigit())
        {
            return None;
        }
        let hi = u32::from_str_radix(hi, 16).ok()?;
        let lo = u32::from_str_radix(lo, 16).ok()?;
        Some(Self((u64::from(hi) << 32) | u64::from(lo)))
    }

    #[must_use]
    pub fn checked_add_signed(self, delta: i128) -> Option<Self> {
        // `delta.unsigned_abs()` widens to u128 and is panic-free even when
        // `delta == i128::MIN`, where unary `-delta` would otherwise overflow.
        let raw = if delta >= 0 {
            self.0.checked_add(u64::try_from(delta).ok()?)?
        } else {
            self.0
                .checked_sub(u64::try_from(delta.unsigned_abs()).ok()?)?
        };
        Some(Self(raw))
    }
}

impl fmt::Display for PgLsnValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hi = u32::try_from((self.0 >> 32) & u64::from(u32::MAX)).map_err(|_| fmt::Error)?;
        let lo = u32::try_from(self.0 & u64::from(u32::MAX)).map_err(|_| fmt::Error)?;
        write!(f, "{hi:X}/{lo:X}")
    }
}

#[cfg(test)]
#[allow(clippy::cast_lossless)]
mod tests {
    use super::PgLsnValue;

    #[test]
    fn parses_and_formats() {
        let value = PgLsnValue::parse("FFFFFFFF/FFFFFFFE").unwrap();
        assert_eq!(value.to_string(), "FFFFFFFF/FFFFFFFE");
    }

    #[test]
    fn rejects_invalid_inputs() {
        for input in [
            "",
            "16AE7F7",
            "G/0",
            "-1/0",
            " 0/12345678",
            "ABCD/",
            "/ABCD",
        ] {
            assert!(PgLsnValue::parse(input).is_none(), "{input}");
        }
    }

    #[test]
    fn adds_signed_offsets() {
        let value = PgLsnValue::parse("0/10").unwrap();
        assert_eq!(value.checked_add_signed(5).unwrap().to_string(), "0/15");
        assert_eq!(value.checked_add_signed(-5).unwrap().to_string(), "0/B");
        assert!(value.checked_add_signed(-i128::from(u64::MAX)).is_none());
    }

    #[test]
    fn checked_add_signed_does_not_panic_on_i128_min_delta() {
        let value = PgLsnValue::parse("FFFFFFFF/FFFFFFFE").unwrap();
        let result = std::panic::catch_unwind(|| value.checked_add_signed(i128::MIN));
        assert!(
            result.is_ok(),
            "checked_add_signed panicked on delta = i128::MIN"
        );
        assert_eq!(
            result.unwrap(),
            None,
            "i128::MIN delta cannot fit in u64 magnitude — must yield None"
        );
    }
}
