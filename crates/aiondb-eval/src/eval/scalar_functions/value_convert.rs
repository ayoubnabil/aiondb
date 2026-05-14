//! Centralised `Value` → primitive conversion helpers and common error
//! factories.
//!
//! These small functions were duplicated across several files in the
//! `scalar_functions` module tree.  Collecting them here avoids drift and
//! makes it easier to keep the error messages consistent.

use aiondb_core::{DbError, DbResult, ErrorReport, SqlState, Value};

// ── Common out-of-range error factories ─────────────────────────────────
//
// These were duplicated across operators/, cast/, scalar_functions/ files.

pub(crate) fn timestamp_out_of_range() -> DbError {
    DbError::from_report(ErrorReport::new(
        SqlState::NumericValueOutOfRange,
        "timestamp out of range",
    ))
}

pub(crate) fn interval_out_of_range() -> DbError {
    DbError::from_report(ErrorReport::new(
        SqlState::NumericValueOutOfRange,
        "interval out of range",
    ))
}

pub(crate) fn pg_lsn_out_of_range() -> DbError {
    DbError::from_report(ErrorReport::new(
        SqlState::NumericValueOutOfRange,
        "pg_lsn out of range",
    ))
}

// ── Value → i32 ─────────────────────────────────────────────────────────

/// Convert a `Value` that is expected to be an integer into an `i32`.
///
/// Accepts `Int` and `BigInt` (with a checked narrowing).  All other
/// variants produce an error.
pub(crate) fn value_to_i32(val: &Value) -> DbResult<i32> {
    match val {
        Value::Int(v) => Ok(*v),
        Value::BigInt(v) => i32::try_from(*v).map_err(|_| {
            DbError::from_report(ErrorReport::new(
                SqlState::NumericValueOutOfRange,
                "integer out of range",
            ))
        }),
        _ => Err(DbError::internal("expected integer argument")),
    }
}

/// Lenient `Value` → `i32` used by `generate_series` which also accepts
/// floating-point, numeric and text representations.
pub(crate) fn value_to_i32_coercing(v: &Value) -> DbResult<i32> {
    match v {
        Value::Int(n) => Ok(*n),
        Value::BigInt(n) => i32::try_from(*n).map_err(|_| {
            DbError::internal("bigint value out of range for integer generate_series")
        }),
        Value::Real(n) => f64_to_i32(f64::from(*n)),
        Value::Double(n) => f64_to_i32(*n),
        Value::Numeric(n) => {
            let s = n.to_string();
            let f: f64 = s.parse().unwrap_or(0.0);
            f64_to_i32(f)
        }
        Value::Text(s) => s
            .trim()
            .parse::<i32>()
            .map_err(|_| DbError::internal(format!("cannot convert '{s}' to integer"))),
        _ => Err(DbError::internal(
            "generate_series: unsupported argument type",
        )),
    }
}

// ── Value → i64 ─────────────────────────────────────────────────────────

/// Lenient `Value` → `i64` used by `generate_series` which also accepts
/// floating-point, numeric and text representations.
pub(crate) fn value_to_i64_coercing(v: &Value) -> DbResult<i64> {
    match v {
        Value::Int(n) => Ok(i64::from(*n)),
        Value::BigInt(n) => Ok(*n),
        Value::Real(n) => f64_to_i64(f64::from(*n)),
        Value::Double(n) => f64_to_i64(*n),
        Value::Numeric(n) => {
            let s = n.to_string();
            let f: f64 = s.parse().unwrap_or(0.0);
            f64_to_i64(f)
        }
        Value::Text(s) => s
            .trim()
            .parse::<i64>()
            .map_err(|_| DbError::internal(format!("cannot convert '{s}' to bigint"))),
        _ => Err(DbError::internal(
            "generate_series: unsupported argument type",
        )),
    }
}

// ── Primitive narrowing helpers ─────────────────────────────────────────

/// Checked `i64` → `i32` with a caller-supplied error message and
/// `program_limit` SQL state (used by text functions).
pub(crate) fn checked_i64_to_i32(n: i64, message: &str) -> DbResult<i32> {
    i32::try_from(n).map_err(|_| DbError::program_limit(message))
}

/// Checked `usize` → `i32` with a caller-supplied error message and
/// `program_limit` SQL state (used by text functions).
pub(crate) fn checked_usize_to_i32(n: usize, message: &str) -> DbResult<i32> {
    i32::try_from(n).map_err(|_| DbError::program_limit(message))
}

/// Safely convert an `f64` to `i32`, returning an out-of-range error when
/// the value is outside the `i32` representable range.
pub(crate) fn f64_to_i32(v: f64) -> DbResult<i32> {
    if !v.is_finite() || v < f64::from(i32::MIN) || v > f64::from(i32::MAX) {
        return Err(DbError::from_report(ErrorReport::new(
            SqlState::NumericValueOutOfRange,
            "integer out of range",
        )));
    }
    format!("{:.0}", v.trunc()).parse::<i32>().map_err(|_| {
        DbError::from_report(ErrorReport::new(
            SqlState::NumericValueOutOfRange,
            "integer out of range",
        ))
    })
}

/// Safely convert an `f64` to `i64`, returning an out-of-range error when
/// the value is outside the representable range.
pub(crate) fn f64_to_i64(v: f64) -> DbResult<i64> {
    // i64::MIN is exactly representable as f64, but i64::MAX rounds up,
    // so we use `>=` for the upper bound.
    if !v.is_finite() || v < i64_to_f64(i64::MIN) || v >= i64_to_f64(i64::MAX) {
        return Err(DbError::from_report(ErrorReport::new(
            SqlState::NumericValueOutOfRange,
            "bigint out of range",
        )));
    }
    format!("{:.0}", v.trunc()).parse::<i64>().map_err(|_| {
        DbError::from_report(ErrorReport::new(
            SqlState::NumericValueOutOfRange,
            "bigint out of range",
        ))
    })
}

/// Checked `i64` → `i32` conversion via the standard `TryFrom` path,
/// returning a `NumericValueOutOfRange` error on overflow.
pub(crate) fn i64_to_i32(v: i64) -> DbResult<i32> {
    i32::try_from(v).map_err(|_| {
        DbError::from_report(ErrorReport::new(
            SqlState::NumericValueOutOfRange,
            "integer out of range",
        ))
    })
}

#[inline]
pub(crate) fn i32_to_f32(v: i32) -> f32 {
    v.to_string().parse::<f32>().unwrap_or_else(|_| {
        if v.is_negative() {
            f32::NEG_INFINITY
        } else {
            f32::INFINITY
        }
    })
}

#[inline]
pub(crate) fn i64_to_f32(v: i64) -> f32 {
    v.to_string().parse::<f32>().unwrap_or_else(|_| {
        if v.is_negative() {
            f32::NEG_INFINITY
        } else {
            f32::INFINITY
        }
    })
}

#[inline]
pub(crate) fn i64_to_f64(v: i64) -> f64 {
    v.to_string().parse::<f64>().unwrap_or_else(|_| {
        if v.is_negative() {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        }
    })
}

#[inline]
pub(crate) fn u64_to_f64(v: u64) -> f64 {
    v.to_string().parse::<f64>().unwrap_or(f64::INFINITY)
}

#[inline]
pub(crate) fn usize_to_f32(v: usize) -> f32 {
    v.to_string().parse::<f32>().unwrap_or(f32::INFINITY)
}

#[inline]
pub(crate) fn i128_to_f64(v: i128) -> f64 {
    v.to_string().parse::<f64>().unwrap_or_else(|_| {
        if v.is_negative() {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        }
    })
}

pub(crate) fn f64_to_i128_rounded(v: f64, error: impl Fn() -> DbError) -> DbResult<i128> {
    if !v.is_finite() {
        return Err(error());
    }
    format!("{:.0}", v.round())
        .parse::<i128>()
        .map_err(|_| error())
}

pub(crate) fn f64_to_u32_rounded_saturating(v: f64) -> u32 {
    if !v.is_finite() || v <= 0.0 {
        return 0;
    }
    let clamped = v.round().min(f64::from(u32::MAX));
    format!("{clamped:.0}").parse::<u32>().unwrap_or(u32::MAX)
}

pub(crate) fn f64_to_i64_trunc_saturating(v: f64) -> i64 {
    if !v.is_finite() {
        return if v.is_sign_negative() {
            i64::MIN
        } else {
            i64::MAX
        };
    }
    let min = i64_to_f64(i64::MIN);
    let max = i64_to_f64(i64::MAX);
    let clamped = v.trunc().clamp(min, max);
    format!("{clamped:.0}").parse::<i64>().unwrap_or_else(|_| {
        if clamped.is_sign_negative() {
            i64::MIN
        } else {
            i64::MAX
        }
    })
}

// ── Saturating narrowing ──────────────────────────────────────────────

/// Saturating conversion to `i32`, clamping to `i32::MAX` on overflow.
/// Used when a length, count, or scale must be returned as a PostgreSQL
/// integer column.
#[inline]
pub(crate) fn to_i32_saturating(n: impl TryInto<i32>) -> i32 {
    n.try_into().unwrap_or(i32::MAX)
}

// ── Best-effort coercion ────────────────────────────────────────────────

/// Try to extract an `i32` from a `Value`, including lossy paths through
/// `Real`, `Double` and `Numeric`.  Returns `None` when the value cannot
/// be represented.
pub(crate) fn coerce_i32_like(value: &Value) -> Option<i32> {
    match value {
        Value::Int(n) => Some(*n),
        Value::BigInt(n) => i32::try_from(*n).ok(),
        Value::Real(f) => f64_to_i32(f64::from(*f)).ok(),
        Value::Double(f) => f64_to_i32(*f).ok(),
        Value::Numeric(n) => n.to_string().parse::<i32>().ok().or(Some(0)),
        _ => None,
    }
}

/// Extract an `i32` from a `Value` that is expected to hold an integer
/// (with a caller-supplied error message).
pub(crate) fn expect_i32_value(value: &Value, message: &str) -> DbResult<i32> {
    match value {
        Value::Int(n) => Ok(*n),
        Value::BigInt(n) => checked_i64_to_i32(*n, message),
        _ => Err(DbError::internal(message)),
    }
}
