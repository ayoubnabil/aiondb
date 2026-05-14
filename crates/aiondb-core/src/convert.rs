//! Saturating numeric-width conversions used across many crates.

/// Convert `usize` to `u64`, saturating to `u64::MAX` on overflow.
#[inline]
#[must_use]
pub fn usize_to_u64_saturating(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// Convert `usize` to `i64`, saturating to `i64::MAX` on overflow.
#[inline]
#[must_use]
pub fn usize_to_i64_saturating(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// Convert `usize` to `u32`, saturating to `u32::MAX` on overflow.
#[inline]
#[must_use]
pub fn usize_to_u32_saturating(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// Convert `u64` to `u32`, saturating to `u32::MAX` on overflow.
#[inline]
#[must_use]
pub fn u64_to_u32_saturating(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// Convert `usize` to `i16`, saturating to `i16::MAX` on overflow.
#[inline]
#[must_use]
pub fn usize_to_i16_saturating(value: usize) -> i16 {
    i16::try_from(value).unwrap_or(i16::MAX)
}

/// Convert `usize` to `i32`, saturating to `i32::MAX` on overflow.
#[inline]
#[must_use]
pub fn usize_to_i32_saturating(value: usize) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

/// Convert `u32` to `usize`, saturating to `usize::MAX` on overflow.
#[inline]
#[must_use]
pub fn u32_to_usize_saturating(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

/// Convert `u32` to `i32`, saturating to `i32::MAX` on overflow.
#[inline]
#[must_use]
pub fn u32_to_i32_saturating(value: u32) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

/// Convert `u64` to `usize`, saturating to `usize::MAX` on overflow
/// (relevant only on 32-bit platforms; on 64-bit `usize == u64`).
#[inline]
#[must_use]
pub fn u64_to_usize_saturating(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usize_to_u64_basic() {
        assert_eq!(usize_to_u64_saturating(0), 0);
        assert_eq!(usize_to_u64_saturating(100), 100);
        if std::mem::size_of::<usize>() > 8 {
            assert_eq!(usize_to_u64_saturating(usize::MAX), u64::MAX);
        }
    }

    #[test]
    fn usize_to_u32_basic() {
        assert_eq!(usize_to_u32_saturating(0), 0);
        assert_eq!(usize_to_u32_saturating(100), 100);
        if std::mem::size_of::<usize>() > 4 {
            assert_eq!(usize_to_u32_saturating(u32::MAX as usize + 1), u32::MAX);
        }
    }

    #[test]
    fn usize_to_i64_basic() {
        assert_eq!(usize_to_i64_saturating(0), 0);
        assert_eq!(usize_to_i64_saturating(100), 100);
        if std::mem::size_of::<usize>() > 8 {
            assert_eq!(usize_to_i64_saturating(usize::MAX), i64::MAX);
        }
    }

    #[test]
    fn usize_to_i16_basic() {
        assert_eq!(usize_to_i16_saturating(0), 0);
        assert_eq!(usize_to_i16_saturating(100), 100);
        assert_eq!(usize_to_i16_saturating(40_000), i16::MAX);
    }

    #[test]
    fn usize_to_i32_basic() {
        assert_eq!(usize_to_i32_saturating(0), 0);
        assert_eq!(usize_to_i32_saturating(100), 100);
        if std::mem::size_of::<usize>() > 4 {
            assert_eq!(usize_to_i32_saturating(usize::MAX), i32::MAX);
        }
    }

    #[test]
    fn u32_to_usize_basic() {
        assert_eq!(u32_to_usize_saturating(0), 0);
        assert_eq!(u32_to_usize_saturating(100), 100);
    }

    #[test]
    fn u32_to_i32_basic() {
        assert_eq!(u32_to_i32_saturating(0), 0);
        assert_eq!(u32_to_i32_saturating(100), 100);
        assert_eq!(u32_to_i32_saturating(u32::MAX), i32::MAX);
    }
}
