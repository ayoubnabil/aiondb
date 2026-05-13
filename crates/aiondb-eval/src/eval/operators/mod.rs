mod arith;
mod comparison;
pub(crate) mod temporal;

pub(crate) use arith::{
    eval_arith_div, eval_arith_mod, eval_arith_mul, eval_arith_sub, eval_negate,
};

#[cfg(test)]
pub(crate) use comparison::compare_numeric;
pub use comparison::compare_runtime_values;
pub(crate) use comparison::{as_nullable_bool, compare_values, interval_factor, numeric_to_f64};

use std::cmp::Ordering;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use self::temporal::{
    add_interval_to_timestamp, add_interval_to_timestamptz, apply_interval_calendar_to_date,
    sub_interval_from_timestamp, sub_interval_from_timestamptz,
};
use super::scalar_functions::range::looks_like_range;
use aiondb_core::{DbError, DbResult, ErrorReport, IntervalValue, NumericValue, SqlState, Value};

use super::money::{
    money_div_f64, money_div_i64, money_mul_f64, money_mul_i64, money_out_of_range,
    money_to_numeric, parse_money_text,
};
use super::scalar_functions::value_convert::{
    f64_to_i128_rounded, f64_to_i32, f64_to_i64, i128_to_f64, i32_to_f32, i64_to_f64,
    interval_out_of_range, pg_lsn_out_of_range, timestamp_out_of_range,
};

use super::{DAYS_PER_MONTH_I128, DAY_MICROS_I128, DAY_MICROS_I64};

#[cold]
fn date_out_of_range() -> DbError {
    DbError::from_report(ErrorReport::new(
        SqlState::NumericValueOutOfRange,
        "date out of range",
    ))
}

#[cold]
fn numeric_out_of_range() -> DbError {
    DbError::from_report(ErrorReport::new(
        SqlState::NumericValueOutOfRange,
        "numeric value out of range",
    ))
}

fn checked_numeric_binary_value(
    left: &NumericValue,
    right: &NumericValue,
    result: NumericValue,
) -> DbResult<Value> {
    if result.is_nan()
        && !left.is_nan()
        && !right.is_nan()
        && !left.is_infinite()
        && !right.is_infinite()
    {
        return Err(numeric_out_of_range());
    }
    Ok(Value::Numeric(result))
}

#[cold]
fn pg_lsn_nan_error(operation: &str) -> DbError {
    let message = match operation {
        "add" => "cannot add NaN to pg_lsn",
        "subtract" => "cannot subtract NaN from pg_lsn",
        _ => "invalid pg_lsn numeric operation",
    };
    DbError::from_report(ErrorReport::new(SqlState::NumericValueOutOfRange, message))
}

fn pg_lsn_offset_from_numeric(value: &NumericValue, operation: &str) -> DbResult<i128> {
    if value.is_nan() {
        return Err(pg_lsn_nan_error(operation));
    }
    if value.is_infinite() || value.scale != 0 {
        return Err(pg_lsn_out_of_range());
    }
    Ok(value.coefficient)
}

fn pg_lsn_offset_from_value(value: &Value, operation: &str) -> DbResult<i128> {
    match value {
        Value::Int(value) => Ok(i128::from(*value)),
        Value::BigInt(value) => Ok(i128::from(*value)),
        Value::Numeric(value) => pg_lsn_offset_from_numeric(value, operation),
        _ => Err(DbError::internal(format!(
            "unsupported pg_lsn offset type: {:?}",
            value.data_type()
        ))),
    }
}

fn promote_timestamp_to_timestamptz(timestamp: time::PrimitiveDateTime) -> time::OffsetDateTime {
    crate::eval::session::current_time_zone().apply_to_local(timestamp)
}

/// Checked conversion from f64 to i32 for interval field arithmetic.
fn checked_f64_to_i32(v: f64) -> DbResult<i32> {
    f64_to_i32(v).map_err(|_| interval_out_of_range())
}

fn checked_f64_to_i128_rounded(v: f64) -> DbResult<i128> {
    f64_to_i128_rounded(v, interval_out_of_range)
}

fn checked_f64_to_i64_rounded(v: f64) -> DbResult<i64> {
    let rounded = checked_f64_to_i128_rounded(v)?;
    if rounded < i128::from(i64::MIN) || rounded > i128::from(i64::MAX) {
        return Err(interval_out_of_range());
    }
    i64::try_from(rounded).map_err(|_| interval_out_of_range())
}

fn snap_near_integer(v: f64) -> f64 {
    let rounded = v.round();
    if (v - rounded).abs() < 1e-10 {
        rounded
    } else {
        v
    }
}

fn trunc_toward_zero_with_epsilon(v: f64) -> f64 {
    let snapped = snap_near_integer(v);
    if snapped.is_sign_negative() {
        snapped.ceil()
    } else {
        snapped.floor()
    }
}

fn interval_from_parts(months: i128, days: i128, micros: i128) -> DbResult<IntervalValue> {
    Ok(IntervalValue::new(
        i32::try_from(months).map_err(|_| interval_out_of_range())?,
        i32::try_from(days).map_err(|_| interval_out_of_range())?,
        i64::try_from(micros).map_err(|_| interval_out_of_range())?,
    ))
}

#[inline]
fn time_to_micros(time: &time::Time) -> i64 {
    i64::from(time.hour()) * 3_600_000_000
        + i64::from(time.minute()) * 60_000_000
        + i64::from(time.second()) * 1_000_000
        + i64::from(time.microsecond())
}

#[inline]
fn wrap_day_micros(total: i64) -> i64 {
    ((total % DAY_MICROS_I64) + DAY_MICROS_I64) % DAY_MICROS_I64
}

fn micros_to_time_wrapped(total: i64) -> DbResult<time::Time> {
    let total = wrap_day_micros(total);
    let hour = u8::try_from(total / 3_600_000_000)
        .map_err(|_| DbError::out_of_range("time", "time field value out of range"))?;
    let minute = u8::try_from((total % 3_600_000_000) / 60_000_000)
        .map_err(|_| DbError::out_of_range("time", "time field value out of range"))?;
    let second = u8::try_from((total % 60_000_000) / 1_000_000)
        .map_err(|_| DbError::out_of_range("time", "time field value out of range"))?;
    let micro = u32::try_from(total % 1_000_000)
        .map_err(|_| DbError::out_of_range("time", "time field value out of range"))?;
    time::Time::from_hms_micro(hour, minute, second, micro)
        .map_err(|_| DbError::out_of_range("time", "time field value out of range"))
}

#[inline]
fn timetz_utc_micros(time: &time::Time, offset: &time::UtcOffset) -> i64 {
    let offset_micros = i64::from(offset.whole_seconds()) * 1_000_000;
    wrap_day_micros(time_to_micros(time) - offset_micros)
}

#[inline]
fn timetz_order_key(time: &time::Time, offset: &time::UtcOffset) -> (i64, i64, i32) {
    let time_micros = time_to_micros(time);
    let offset_micros = i64::from(offset.whole_seconds()) * 1_000_000;
    (
        time_micros - offset_micros,
        time_micros,
        offset.whole_seconds(),
    )
}

pub(crate) fn interval_comparison_key(iv: &IntervalValue) -> i128 {
    (i128::from(iv.months) * DAYS_PER_MONTH_I128 + i128::from(iv.days)) * DAY_MICROS_I128
        + i128::from(iv.micros)
}

fn interval_from_total_micros(total_micros: i128) -> DbResult<IntervalValue> {
    let negative = total_micros < 0;
    let total_micros = total_micros.unsigned_abs();
    let day_micros = DAY_MICROS_I128.cast_unsigned();
    let days = i32::try_from(total_micros / day_micros).map_err(|_| interval_out_of_range())?;
    let micros = i64::try_from(total_micros % day_micros).map_err(|_| interval_out_of_range())?;

    Ok(IntervalValue::new(
        0,
        if negative { -days } else { days },
        if negative { -micros } else { micros },
    ))
}

pub fn scale_interval(iv: &IntervalValue, factor: f64) -> DbResult<IntervalValue> {
    if !factor.is_finite() {
        return Err(interval_out_of_range());
    }

    let months_raw = snap_near_integer(f64::from(iv.months) * factor);
    let months_trunc = trunc_toward_zero_with_epsilon(months_raw);
    let months = checked_f64_to_i32(months_trunc)?;

    let month_days_raw =
        snap_near_integer((months_raw - months_trunc) * i128_to_f64(DAYS_PER_MONTH_I128));
    let month_days_trunc = trunc_toward_zero_with_epsilon(month_days_raw);
    let month_days = checked_f64_to_i32(month_days_trunc)?;
    let month_day_frac = snap_near_integer(month_days_raw - month_days_trunc);

    let days_raw = snap_near_integer(f64::from(iv.days) * factor);
    let days_trunc = trunc_toward_zero_with_epsilon(days_raw);
    let days = checked_f64_to_i32(days_trunc)?;
    let day_frac = snap_near_integer(days_raw - days_trunc);

    let day_remainder_raw = snap_near_integer(month_day_frac + day_frac);
    let carry_days_trunc = trunc_toward_zero_with_epsilon(day_remainder_raw);
    let carry_days = checked_f64_to_i32(carry_days_trunc)?;
    let carry_day_frac = snap_near_integer(day_remainder_raw - carry_days_trunc);

    let carry_micros_from_days = carry_day_frac * i128_to_f64(DAY_MICROS_I128);
    let micros_raw = snap_near_integer(i64_to_f64(iv.micros) * factor + carry_micros_from_days);
    let micros = checked_f64_to_i64_rounded(micros_raw)?;

    interval_from_parts(
        i128::from(months),
        i128::from(month_days) + i128::from(days) + i128::from(carry_days),
        i128::from(micros),
    )
}

/// Coerce a text string to a numeric Value for implicit arithmetic coercion.
/// Tries: integer, bigint, float, numeric.
fn coerce_text_to_numeric(s: &str) -> DbResult<Value> {
    let t = s.trim();
    if t.is_empty() {
        return Err(DbError::invalid_input_syntax("integer", s));
    }
    if let Ok(v) = t.parse::<i32>() {
        return Ok(Value::Int(v));
    }
    if let Ok(v) = t.parse::<i64>() {
        return Ok(Value::BigInt(v));
    }
    if let Ok(v) = t.parse::<f64>() {
        return Ok(Value::Double(v));
    }
    if let Ok(v) = t.parse::<NumericValue>() {
        return Ok(Value::Numeric(v));
    }
    Err(DbError::internal(format!(
        "cannot coerce '{s}' to numeric type"
    )))
}

fn parse_ipv4_with_shorthand(text: &str) -> Option<Ipv4Addr> {
    if let Ok(ip) = text.parse::<Ipv4Addr>() {
        return Some(ip);
    }
    let parts = text
        .split('.')
        .map(|part| part.parse::<u8>().ok())
        .collect::<Option<Vec<_>>>()?;
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    let mut octets = [0_u8; 4];
    for (index, value) in parts.into_iter().enumerate() {
        octets[index] = value;
    }
    Some(Ipv4Addr::from(octets))
}

fn parse_network_text(text: &str) -> Option<(IpAddr, Option<u8>)> {
    let trimmed = text.trim();
    let (addr_text, prefix) =
        trimmed
            .split_once('/')
            .map_or((trimmed, None), |(addr, raw_prefix)| {
                let parsed = raw_prefix.trim().parse::<u8>().ok();
                (addr.trim(), parsed)
            });
    let ip = if let Ok(ip) = addr_text.parse::<IpAddr>() {
        ip
    } else {
        IpAddr::V4(parse_ipv4_with_shorthand(addr_text)?)
    };
    let max_prefix = if matches!(ip, IpAddr::V4(_)) { 32 } else { 128 };
    let prefix = match prefix {
        Some(value) if value <= max_prefix => Some(value),
        Some(_) => return None,
        None => None,
    };
    Some((ip, prefix))
}

fn add_offset_to_network(ip: IpAddr, offset: i128) -> DbResult<IpAddr> {
    match ip {
        IpAddr::V4(addr) => {
            let value = i128::from(u32::from(addr));
            let result = value
                .checked_add(offset)
                .filter(|sum| *sum >= 0 && *sum <= i128::from(u32::MAX))
                .ok_or_else(|| {
                    DbError::from_report(ErrorReport::new(
                        SqlState::NumericValueOutOfRange,
                        "inet result out of range",
                    ))
                })?;
            Ok(IpAddr::V4(Ipv4Addr::from(u32::try_from(result).map_err(
                |_| {
                    DbError::from_report(ErrorReport::new(
                        SqlState::NumericValueOutOfRange,
                        "inet result out of range",
                    ))
                },
            )?)))
        }
        IpAddr::V6(addr) => {
            let value = u128::from(addr);
            if offset >= 0 {
                let delta = u128::try_from(offset).map_err(|_| {
                    DbError::from_report(ErrorReport::new(
                        SqlState::NumericValueOutOfRange,
                        "inet result out of range",
                    ))
                })?;
                let result = value.checked_add(delta).ok_or_else(|| {
                    DbError::from_report(ErrorReport::new(
                        SqlState::NumericValueOutOfRange,
                        "inet result out of range",
                    ))
                })?;
                Ok(IpAddr::V6(Ipv6Addr::from(result)))
            } else {
                let delta = u128::try_from(offset.unsigned_abs()).map_err(|_| {
                    DbError::from_report(ErrorReport::new(
                        SqlState::NumericValueOutOfRange,
                        "inet result out of range",
                    ))
                })?;
                let result = value.checked_sub(delta).ok_or_else(|| {
                    DbError::from_report(ErrorReport::new(
                        SqlState::NumericValueOutOfRange,
                        "inet result out of range",
                    ))
                })?;
                Ok(IpAddr::V6(Ipv6Addr::from(result)))
            }
        }
    }
}

fn extract_network_offset(value: &Value) -> Option<i128> {
    match value {
        Value::Int(v) => Some(i128::from(*v)),
        Value::BigInt(v) => Some(i128::from(*v)),
        Value::Numeric(v) if v.scale == 0 => Some(v.coefficient),
        _ => None,
    }
}

fn eval_network_plus_numeric(text: &str, numeric: &Value) -> Option<DbResult<Value>> {
    let (ip, prefix) = parse_network_text(text)?;
    let offset = extract_network_offset(numeric)?;
    let updated = match add_offset_to_network(ip, offset) {
        Ok(ip) => ip,
        Err(error) => return Some(Err(error)),
    };
    let rendered = if let Some(prefix) = prefix {
        format!("{updated}/{prefix}")
    } else {
        updated.to_string()
    };
    Some(Ok(Value::Text(rendered)))
}

/// `inet - integer` - shift the address by `-N`. Reuses the same encoding
/// logic as the plus path: just negate the offset.
pub(crate) fn eval_network_minus_numeric(text: &str, numeric: &Value) -> Option<DbResult<Value>> {
    let (ip, prefix) = parse_network_text(text)?;
    let offset = extract_network_offset(numeric)?;
    let updated = match add_offset_to_network(ip, -offset) {
        Ok(ip) => ip,
        Err(error) => return Some(Err(error)),
    };
    let rendered = if let Some(prefix) = prefix {
        format!("{updated}/{prefix}")
    } else {
        updated.to_string()
    };
    Some(Ok(Value::Text(rendered)))
}

#[cfg(test)]
mod inet_arith_tests {
    use super::*;

    #[test]
    fn inet_minus_int_shifts_address() {
        let result = eval_network_minus_numeric("127.0.0.5", &Value::Int(4))
            .unwrap()
            .unwrap();
        assert_eq!(result, Value::Text("127.0.0.1".to_owned()));
    }

    #[test]
    fn inet_minus_int_preserves_prefix() {
        let result = eval_network_minus_numeric("192.168.1.5/24", &Value::Int(4))
            .unwrap()
            .unwrap();
        assert_eq!(result, Value::Text("192.168.1.1/24".to_owned()));
    }

    #[test]
    fn inet_minus_inet_returns_diff() {
        let result = eval_network_minus_network("127.0.0.5", "127.0.0.1")
            .unwrap()
            .unwrap();
        assert_eq!(result, Value::BigInt(4));
    }

    #[test]
    fn inet_minus_inet_negative_diff() {
        let result = eval_network_minus_network("127.0.0.1", "127.0.0.5")
            .unwrap()
            .unwrap();
        assert_eq!(result, Value::BigInt(-4));
    }

    #[test]
    fn inet_minus_inet_cross_family_returns_none() {
        // Mixed IPv4/IPv6 falls through to caller (None).
        assert!(eval_network_minus_network("127.0.0.1", "::1").is_none());
    }

    #[test]
    fn inet_minus_inet_ipv6() {
        let result = eval_network_minus_network("::5", "::1").unwrap().unwrap();
        assert_eq!(result, Value::BigInt(4));
    }
}

/// `inet - inet` returns the BIGINT difference between the two address
/// values, ignoring prefix length but requiring matching address family.
/// Result is positive when `left` is numerically greater than `right`,
/// negative otherwise, matching PG semantics.
pub(crate) fn eval_network_minus_network(
    left_text: &str,
    right_text: &str,
) -> Option<DbResult<Value>> {
    let (left_ip, _) = parse_network_text(left_text)?;
    let (right_ip, _) = parse_network_text(right_text)?;
    let diff: i128 = match (left_ip, right_ip) {
        (IpAddr::V4(l), IpAddr::V4(r)) => i128::from(u32::from(l)) - i128::from(u32::from(r)),
        (IpAddr::V6(l), IpAddr::V6(r)) => {
            // u128 difference fits in i128 since each side ≤ 2^128 - 1 and
            // we compute `l - r` which can be in [-(2^128-1), 2^128-1].
            // For PG compatibility we clamp to BigInt range and surface
            // overflow as an error since BigInt is i64.
            let lv = u128::from(l);
            let rv = u128::from(r);
            if lv >= rv {
                let unsigned_diff = lv - rv;
                if unsigned_diff > i128::MAX.cast_unsigned() {
                    return Some(Err(DbError::from_report(ErrorReport::new(
                        SqlState::NumericValueOutOfRange,
                        "inet - inet result out of range",
                    ))));
                }
                i128::try_from(unsigned_diff).ok()?
            } else {
                let unsigned_diff = rv - lv;
                if unsigned_diff > i128::MAX.cast_unsigned() {
                    return Some(Err(DbError::from_report(ErrorReport::new(
                        SqlState::NumericValueOutOfRange,
                        "inet - inet result out of range",
                    ))));
                }
                -i128::try_from(unsigned_diff).ok()?
            }
        }
        _ => return None, // mixed v4/v6 → caller falls through
    };
    let result = match i64::try_from(diff) {
        Ok(v) => v,
        Err(_) => {
            return Some(Err(DbError::from_report(ErrorReport::new(
                SqlState::NumericValueOutOfRange,
                "inet - inet result out of range",
            ))));
        }
    };
    Some(Ok(Value::BigInt(result)))
}

#[inline]
fn jsonb_type_rank(value: &serde_json::Value) -> u8 {
    match value {
        serde_json::Value::Null => 0,
        serde_json::Value::Bool(_) => 1,
        serde_json::Value::Number(_) => 2,
        serde_json::Value::String(_) => 3,
        serde_json::Value::Array(_) => 4,
        serde_json::Value::Object(_) => 5,
    }
}

fn compare_jsonb_numbers(left: &serde_json::Number, right: &serde_json::Number) -> Ordering {
    match (left.as_i64(), right.as_i64()) {
        (Some(l), Some(r)) => return l.cmp(&r),
        (Some(l), None) => {
            if let Some(r) = right.as_u64() {
                return if l < 0 {
                    Ordering::Less
                } else {
                    l.cast_unsigned().cmp(&r)
                };
            }
        }
        (None, Some(r)) => {
            if let Some(l) = left.as_u64() {
                return if r < 0 {
                    Ordering::Greater
                } else {
                    l.cmp(&r.cast_unsigned())
                };
            }
        }
        (None, None) => {}
    }

    if let (Some(l), Some(r)) = (left.as_u64(), right.as_u64()) {
        return l.cmp(&r);
    }

    if let (Some(l), Some(r)) = (left.as_f64(), right.as_f64()) {
        if let Some(ordering) = l.partial_cmp(&r) {
            return ordering;
        }
    }

    left.to_string().cmp(&right.to_string())
}

const MAX_JSONB_COMPARE_DEPTH: usize = 256;

fn compare_jsonb_values(left: &serde_json::Value, right: &serde_json::Value) -> DbResult<Ordering> {
    compare_jsonb_values_at_depth(left, right, 0)
}

fn compare_jsonb_values_at_depth(
    left: &serde_json::Value,
    right: &serde_json::Value,
    depth: usize,
) -> DbResult<Ordering> {
    if std::ptr::eq(left, right) {
        return Ok(Ordering::Equal);
    }

    let rank_cmp = jsonb_type_rank(left).cmp(&jsonb_type_rank(right));
    if rank_cmp != Ordering::Equal {
        return Ok(rank_cmp);
    }
    if depth >= MAX_JSONB_COMPARE_DEPTH {
        return Err(DbError::program_limit(format!(
            "JSONB comparison nesting depth exceeds limit {MAX_JSONB_COMPARE_DEPTH}"
        )));
    }

    Ok(match (left, right) {
        (serde_json::Value::Null, serde_json::Value::Null) => Ordering::Equal,
        (serde_json::Value::Bool(left), serde_json::Value::Bool(right)) => left.cmp(right),
        (serde_json::Value::Number(left), serde_json::Value::Number(right)) => {
            compare_jsonb_numbers(left, right)
        }
        (serde_json::Value::String(left), serde_json::Value::String(right)) => left.cmp(right),
        (serde_json::Value::Array(left), serde_json::Value::Array(right)) => {
            for (left_elem, right_elem) in left.iter().zip(right.iter()) {
                let ordering = compare_jsonb_values_at_depth(left_elem, right_elem, depth + 1)?;
                if ordering != Ordering::Equal {
                    return Ok(ordering);
                }
            }
            left.len().cmp(&right.len())
        }
        (serde_json::Value::Object(left), serde_json::Value::Object(right)) => {
            let mut left_entries = left.iter().collect::<Vec<_>>();
            let mut right_entries = right.iter().collect::<Vec<_>>();
            left_entries.sort_unstable_by_key(|(key, _)| *key);
            right_entries.sort_unstable_by_key(|(key, _)| *key);
            for ((left_key, left_val), (right_key, right_val)) in
                left_entries.iter().zip(right_entries.iter())
            {
                let key_cmp = left_key.cmp(right_key);
                if key_cmp != Ordering::Equal {
                    return Ok(key_cmp);
                }
                let value_cmp = compare_jsonb_values_at_depth(left_val, right_val, depth + 1)?;
                if value_cmp != Ordering::Equal {
                    return Ok(value_cmp);
                }
            }
            left_entries.len().cmp(&right_entries.len())
        }
        _ => Ordering::Equal,
    })
}

#[inline]
pub(super) fn eval_equality_comparison(
    left: &Value,
    right: &Value,
    negate: bool,
) -> DbResult<Value> {
    match values_equal(left, right)? {
        Some(value) => Ok(Value::Boolean(if negate { !value } else { value })),
        None => Ok(Value::Null),
    }
}

#[inline]
pub(super) fn values_equal(left: &Value, right: &Value) -> DbResult<Option<bool>> {
    match (left, right) {
        (Value::Null, _) | (_, Value::Null) => Ok(None),
        (Value::Jsonb(left), Value::Jsonb(right)) => {
            Ok(Some(compare_jsonb_values(left, right)? == Ordering::Equal))
        }
        _ => Ok(match compare_values(left, right)? {
            Some(Ordering::Equal) => Some(true),
            Some(_) => Some(false),
            None => None,
        }),
    }
}

#[inline]
pub(super) fn eval_ordering_comparison(
    left: &Value,
    right: &Value,
    predicate: impl FnOnce(Ordering) -> bool,
) -> DbResult<Value> {
    match compare_values(left, right)? {
        Some(ordering) => Ok(Value::Boolean(predicate(ordering))),
        None => Ok(Value::Null),
    }
}

#[inline]
fn nullable_bool_to_value(value: Option<bool>) -> Value {
    match value {
        Some(value) => Value::Boolean(value),
        None => Value::Null,
    }
}

#[inline]
fn sql_nullable_and(left: Option<bool>, right: Option<bool>) -> Option<bool> {
    match (left, right) {
        (Some(false), _) | (_, Some(false)) => Some(false),
        (Some(true), Some(true)) => Some(true),
        _ => None,
    }
}

#[inline]
fn sql_nullable_or(left: Option<bool>, right: Option<bool>) -> Option<bool> {
    match (left, right) {
        (Some(true), _) | (_, Some(true)) => Some(true),
        (Some(false), Some(false)) => Some(false),
        _ => None,
    }
}

#[inline]
fn sql_nullable_not(value: Option<bool>) -> Option<bool> {
    value.map(|value| !value)
}

#[inline]
pub(super) fn eval_logical_and_with_left(left: Option<bool>, right: &Value) -> DbResult<Value> {
    if left == Some(false) {
        return Ok(Value::Boolean(false));
    }
    let right = as_nullable_bool(right)?;
    Ok(nullable_bool_to_value(sql_nullable_and(left, right)))
}

#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn eval_logical_and(left: &Value, right: &Value) -> DbResult<Value> {
    eval_logical_and_with_left(as_nullable_bool(left)?, right)
}

#[inline]
pub(super) fn eval_logical_or_with_left(left: Option<bool>, right: &Value) -> DbResult<Value> {
    if left == Some(true) {
        return Ok(Value::Boolean(true));
    }
    let right = as_nullable_bool(right)?;
    Ok(nullable_bool_to_value(sql_nullable_or(left, right)))
}

#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn eval_logical_or(left: &Value, right: &Value) -> DbResult<Value> {
    eval_logical_or_with_left(as_nullable_bool(left)?, right)
}

#[inline]
pub(super) fn eval_logical_not(value: &Value) -> DbResult<Value> {
    Ok(nullable_bool_to_value(sql_nullable_not(as_nullable_bool(
        value,
    )?)))
}

pub(super) fn eval_arith_add(left: &Value, right: &Value) -> DbResult<Value> {
    match (left, right) {
        (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
        // PG compatibility: i32 overflow promotes to BigInt rather than error.
        (Value::Int(a), Value::Int(b)) => match a.checked_add(*b) {
            Some(result) => Ok(Value::Int(result)),
            None => Ok(Value::BigInt(i64::from(*a) + i64::from(*b))),
        },
        (Value::Int(a), Value::BigInt(b)) => i64::from(*a)
            .checked_add(*b)
            .map(Value::BigInt)
            .ok_or_else(|| DbError::internal("bigint out of range")),
        (Value::BigInt(a), Value::Int(b)) => a
            .checked_add(i64::from(*b))
            .map(Value::BigInt)
            .ok_or_else(|| DbError::internal("bigint out of range")),
        (Value::BigInt(a), Value::BigInt(b)) => a
            .checked_add(*b)
            .map(Value::BigInt)
            .ok_or_else(|| DbError::internal("bigint out of range")),
        (Value::Real(a), Value::Real(b)) => {
            let result = a + b;
            if result.is_infinite() && !a.is_infinite() && !b.is_infinite() {
                return Err(DbError::from_report(ErrorReport::new(
                    SqlState::NumericValueOutOfRange,
                    "value out of range: overflow",
                )));
            }
            Ok(Value::Real(result))
        }
        (Value::Double(a), Value::Double(b)) => {
            let result = a + b;
            if result.is_infinite() && !a.is_infinite() && !b.is_infinite() {
                return Err(DbError::from_report(ErrorReport::new(
                    SqlState::NumericValueOutOfRange,
                    "value out of range: overflow",
                )));
            }
            Ok(Value::Double(result))
        }
        (Value::Real(a), Value::Double(b)) => Ok(Value::Double(f64::from(*a) + b)),
        (Value::Double(a), Value::Real(b)) => Ok(Value::Double(a + f64::from(*b))),
        (Value::Int(a), Value::Real(b)) => Ok(Value::Real(i32_to_f32(*a) + b)),
        (Value::Real(a), Value::Int(b)) => Ok(Value::Real(a + i32_to_f32(*b))),
        (Value::Int(a), Value::Double(b)) => Ok(Value::Double(f64::from(*a) + b)),
        (Value::Double(a), Value::Int(b)) => Ok(Value::Double(a + f64::from(*b))),
        (Value::BigInt(a), Value::Real(b)) => Ok(Value::Double(i64_to_f64(*a) + f64::from(*b))),
        (Value::Real(a), Value::BigInt(b)) => Ok(Value::Double(f64::from(*a) + i64_to_f64(*b))),
        (Value::BigInt(a), Value::Double(b)) => Ok(Value::Double(i64_to_f64(*a) + b)),
        (Value::Double(a), Value::BigInt(b)) => Ok(Value::Double(a + i64_to_f64(*b))),
        (Value::Money(a), Value::Money(b)) => a
            .checked_add(*b)
            .map(Value::Money)
            .ok_or_else(money_out_of_range),
        (Value::Money(a), Value::Text(text)) => parse_money_text(text).and_then(|b| {
            a.checked_add(b)
                .map(Value::Money)
                .ok_or_else(money_out_of_range)
        }),
        (Value::Text(text), Value::Money(b)) => parse_money_text(text).and_then(|a| {
            a.checked_add(*b)
                .map(Value::Money)
                .ok_or_else(money_out_of_range)
        }),
        (Value::Numeric(a), Value::Numeric(b)) => checked_numeric_binary_value(a, b, a.add(b)),
        (Value::Int(a), Value::Numeric(b)) => {
            let a = NumericValue::new(i128::from(*a), 0);
            checked_numeric_binary_value(&a, b, a.add(b))
        }
        (Value::Numeric(a), Value::Int(b)) => {
            let b = NumericValue::new(i128::from(*b), 0);
            checked_numeric_binary_value(a, &b, a.add(&b))
        }
        (Value::BigInt(a), Value::Numeric(b)) => {
            let a = NumericValue::new(i128::from(*a), 0);
            checked_numeric_binary_value(&a, b, a.add(b))
        }
        (Value::Numeric(a), Value::BigInt(b)) => {
            let b = NumericValue::new(i128::from(*b), 0);
            checked_numeric_binary_value(a, &b, a.add(&b))
        }
        (Value::PgLsn(value), offset)
            if matches!(offset, Value::Int(_) | Value::BigInt(_) | Value::Numeric(_)) =>
        {
            let delta = pg_lsn_offset_from_value(offset, "add")?;
            value
                .checked_add_signed(delta)
                .map(Value::PgLsn)
                .ok_or_else(pg_lsn_out_of_range)
        }
        (offset, Value::PgLsn(value))
            if matches!(offset, Value::Int(_) | Value::BigInt(_) | Value::Numeric(_)) =>
        {
            let delta = pg_lsn_offset_from_value(offset, "add")?;
            value
                .checked_add_signed(delta)
                .map(Value::PgLsn)
                .ok_or_else(pg_lsn_out_of_range)
        }
        // Date + Int → Date (add days)
        (Value::Date(d), Value::Int(n)) => {
            let result = d
                .checked_add(time::Duration::days(i64::from(*n)))
                .ok_or_else(date_out_of_range)?;
            Ok(Value::Date(result))
        }
        (Value::Int(n), Value::Date(d)) => {
            let result = d
                .checked_add(time::Duration::days(i64::from(*n)))
                .ok_or_else(date_out_of_range)?;
            Ok(Value::Date(result))
        }
        // Timestamp + Interval → Timestamp
        (Value::Timestamp(ts), Value::Interval(iv))
        | (Value::Interval(iv), Value::Timestamp(ts)) => {
            Ok(Value::Timestamp(add_interval_to_timestamp(*ts, iv)?))
        }
        // Date + Interval → Date (Cypher semantics: drop time component).
        // We add months/days only - adding the duration's hours/minutes
        // first and then truncating would cross midnight backward when
        // `H/M/S` push the underlying timestamp into the previous day.
        (Value::Date(d), Value::Interval(iv)) | (Value::Interval(iv), Value::Date(d)) => {
            Ok(Value::Date(apply_interval_calendar_to_date(*d, iv, false)?))
        }
        // Date + BigInt → Date (add days)
        (Value::Date(d), Value::BigInt(n)) => {
            let result = d
                .checked_add(time::Duration::days(*n))
                .ok_or_else(date_out_of_range)?;
            Ok(Value::Date(result))
        }
        (Value::BigInt(n), Value::Date(d)) => {
            let result = d
                .checked_add(time::Duration::days(*n))
                .ok_or_else(date_out_of_range)?;
            Ok(Value::Date(result))
        }
        // TimestampTz + Interval → TimestampTz
        (Value::TimestampTz(odt), Value::Interval(iv))
        | (Value::Interval(iv), Value::TimestampTz(odt)) => {
            Ok(Value::TimestampTz(add_interval_to_timestamptz(*odt, iv)?))
        }
        // TimeTz + Interval → TimeTz
        (Value::TimeTz(time, offset), Value::Interval(iv))
        | (Value::Interval(iv), Value::TimeTz(time, offset)) => {
            let day_micros = i64::from(iv.days)
                .checked_mul(DAY_MICROS_I64)
                .ok_or_else(interval_out_of_range)?;
            let iv_micros = iv
                .micros
                .checked_add(day_micros)
                .ok_or_else(interval_out_of_range)?;
            Ok(Value::TimeTz(
                micros_to_time_wrapped(time_to_micros(time) + iv_micros)?,
                *offset,
            ))
        }
        // Time + Interval → Time
        (Value::Time(t), Value::Interval(iv)) | (Value::Interval(iv), Value::Time(t)) => {
            let base_micros = time_to_micros(t);
            let day_micros = i64::from(iv.days)
                .checked_mul(DAY_MICROS_I64)
                .ok_or_else(interval_out_of_range)?;
            let iv_micros = iv
                .micros
                .checked_add(day_micros)
                .ok_or_else(interval_out_of_range)?;
            Ok(Value::Time(micros_to_time_wrapped(base_micros + iv_micros)?))
        }
        // Interval + Interval → Interval
        (Value::Interval(a), Value::Interval(b)) => {
            let months = a.months.checked_add(b.months).ok_or_else(|| {
                DbError::out_of_range("interval", "interval field value out of range")
            })?;
            let days = a.days.checked_add(b.days).ok_or_else(|| {
                DbError::out_of_range("interval", "interval field value out of range")
            })?;
            let micros = a.micros.checked_add(b.micros).ok_or_else(|| {
                DbError::out_of_range("interval", "interval field value out of range")
            })?;
            Ok(Value::Interval(IntervalValue::new(months, days, micros)))
        }
        // Numeric + float → Double
        (Value::Numeric(a), Value::Double(b)) => Ok(Value::Double(numeric_to_f64(a) + b)),
        (Value::Double(a), Value::Numeric(b)) => Ok(Value::Double(a + numeric_to_f64(b))),
        (Value::Numeric(a), Value::Real(b)) => Ok(Value::Double(numeric_to_f64(a) + f64::from(*b))),
        (Value::Real(a), Value::Numeric(b)) => Ok(Value::Double(f64::from(*a) + numeric_to_f64(b))),
        // Boolean + numeric (PG: TRUE=1, FALSE=0)
        (Value::Boolean(a), Value::Int(b)) => i32::from(*a)
            .checked_add(*b)
            .map(Value::Int)
            .ok_or_else(|| DbError::internal("integer out of range")),
        (Value::Int(a), Value::Boolean(b)) => a
            .checked_add(i32::from(*b))
            .map(Value::Int)
            .ok_or_else(|| DbError::internal("integer out of range")),
        (Value::Boolean(a), Value::BigInt(b)) => i64::from(*a)
            .checked_add(*b)
            .map(Value::BigInt)
            .ok_or_else(|| DbError::internal("bigint out of range")),
        (Value::BigInt(a), Value::Boolean(b)) => a
            .checked_add(i64::from(*b))
            .map(Value::BigInt)
            .ok_or_else(|| DbError::internal("bigint out of range")),
        // Time + Time → error (ambiguous operator in PG)
        (Value::Time(_), Value::Time(_)) => {
            Err(DbError::from_report(
                ErrorReport::new(
                    SqlState::InternalError,
                    "operator is not unique: time without time zone + time without time zone",
                )
                .with_client_hint(
                    "Could not choose a best candidate operator. You might need to add explicit type casts.",
                ),
            ))
        }
        // Date + Time → Timestamp (PG compat)
        (Value::Date(d), Value::Time(t)) | (Value::Time(t), Value::Date(d)) => {
            Ok(Value::Timestamp(time::PrimitiveDateTime::new(*d, *t)))
        }
        // Date + TimeTz → TimestampTz
        (Value::Date(d), Value::TimeTz(t, offset)) | (Value::TimeTz(t, offset), Value::Date(d)) => {
            Ok(Value::TimestampTz(
                time::PrimitiveDateTime::new(*d, *t).assume_offset(*offset),
            ))
        }
        // PG: text + interval (or timestamp/date/time) implicitly coerces
        // the text to the temporal partner. Without this arm `'1 day' +
        // now()`, `interval '1 day' + '1 hour'`, etc. all bail with
        // "cannot perform arithmetic on … and TEXT".
        (Value::Text(s), Value::Interval(_)) => {
            let coerced = crate::eval::cast::cast_value(
                Value::Text(s.clone()),
                &aiondb_core::DataType::Interval,
            )?;
            eval_arith_add(&coerced, right)
        }
        (Value::Interval(_), Value::Text(s)) => {
            let coerced = crate::eval::cast::cast_value(
                Value::Text(s.clone()),
                &aiondb_core::DataType::Interval,
            )?;
            eval_arith_add(left, &coerced)
        }
        (Value::Text(s), Value::Timestamp(_)) => {
            let coerced = crate::eval::cast::cast_value(
                Value::Text(s.clone()),
                &aiondb_core::DataType::Interval,
            )?;
            eval_arith_add(&coerced, right)
        }
        (Value::Timestamp(_), Value::Text(s)) => {
            let coerced = crate::eval::cast::cast_value(
                Value::Text(s.clone()),
                &aiondb_core::DataType::Interval,
            )?;
            eval_arith_add(left, &coerced)
        }
        (Value::Text(s), Value::TimestampTz(_)) => {
            let coerced = crate::eval::cast::cast_value(
                Value::Text(s.clone()),
                &aiondb_core::DataType::Interval,
            )?;
            eval_arith_add(&coerced, right)
        }
        (Value::TimestampTz(_), Value::Text(s)) => {
            let coerced = crate::eval::cast::cast_value(
                Value::Text(s.clone()),
                &aiondb_core::DataType::Interval,
            )?;
            eval_arith_add(left, &coerced)
        }
        (Value::Text(s), Value::Date(_)) => {
            let coerced = crate::eval::cast::cast_value(
                Value::Text(s.clone()),
                &aiondb_core::DataType::Interval,
            )?;
            eval_arith_add(&coerced, right)
        }
        (Value::Date(_), Value::Text(s)) => {
            let coerced = crate::eval::cast::cast_value(
                Value::Text(s.clone()),
                &aiondb_core::DataType::Interval,
            )?;
            eval_arith_add(left, &coerced)
        }
        // inet/cidr (stored as text) + integer/numeric offset
        (Value::Text(s), right_val) if right_val.is_numeric_coercible() => {
            if let Some(result) = eval_network_plus_numeric(s, right_val) {
                return result;
            }
            let coerced = coerce_text_to_numeric(s)?;
            eval_arith_add(&coerced, right_val)
        }
        (left_val, Value::Text(s)) if left_val.is_numeric_coercible() => {
            if let Some(result) = eval_network_plus_numeric(s, left_val) {
                return result;
            }
            let coerced = coerce_text_to_numeric(s)?;
            eval_arith_add(left_val, &coerced)
        }
        (Value::Text(s1), Value::Text(s2)) => {
            use crate::eval::scalar_functions::range as rng;
            if rng::looks_like_multirange(s1) || rng::looks_like_multirange(s2) {
                return rng::eval_multirange_union(left, right);
            }
            if rng::looks_like_range(s1) && rng::looks_like_range(s2) {
                return rng::eval_range_union_op(left, right);
            }
            let c1 = coerce_text_to_numeric(s1)?;
            let c2 = coerce_text_to_numeric(s2)?;
            eval_arith_add(&c1, &c2)
        }
        // Array + scalar → element-wise addition
        (Value::Array(arr), scalar) => {
            let results: DbResult<Vec<Value>> = arr
                .iter()
                .map(|elem| eval_arith_add(elem, scalar))
                .collect();
            Ok(Value::Array(results?))
        }
        (scalar, Value::Array(arr)) => {
            let results: DbResult<Vec<Value>> = arr
                .iter()
                .map(|elem| eval_arith_add(scalar, elem))
                .collect();
            Ok(Value::Array(results?))
        }
        _ => Err(DbError::internal(format!(
            "cannot add {:?} and {:?}",
            left.data_type(),
            right.data_type()
        ))),
    }
}

pub(super) fn eval_json_get(left: &Value, right: &Value) -> Value {
    let Value::Jsonb(json) = left else {
        return Value::Null;
    };
    match right {
        Value::Text(key) => match json.get(key.as_str()) {
            Some(v) => Value::Jsonb(v.clone()),
            None => Value::Null,
        },
        Value::Int(idx) => {
            // Negative indices are out-of-bounds in PostgreSQL's `->` operator; return NULL.
            if *idx < 0 {
                return Value::Null;
            }
            let idx = usize::try_from(*idx).unwrap_or(usize::MAX);
            match json.get(idx) {
                Some(v) => Value::Jsonb(v.clone()),
                None => Value::Null,
            }
        }
        _ => Value::Null,
    }
}

pub(super) fn eval_json_get_text(left: &Value, right: &Value) -> Value {
    let Value::Jsonb(json) = left else {
        return Value::Null;
    };
    let result = match right {
        Value::Text(key) => json.get(key.as_str()),
        // Negative indices are out-of-bounds in PostgreSQL's `->>` operator; treat as NULL.
        Value::Int(idx) if *idx >= 0 => json.get(usize::try_from(*idx).unwrap_or(usize::MAX)),
        Value::Int(_) => None,
        _ => None,
    };
    match result {
        Some(serde_json::Value::Null) => Value::Null,
        Some(serde_json::Value::String(s)) => Value::Text(s.clone()),
        Some(v) => Value::Text(aiondb_core::value::pg_jsonb_to_string(v)),
        None => Value::Null,
    }
}

pub(super) fn eval_concat(left: &Value, right: &Value) -> DbResult<Value> {
    match (left, right) {
        (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
        (Value::Text(a), Value::Text(b)) => {
            let mut s = String::with_capacity(a.len() + b.len());
            s.push_str(a);
            s.push_str(b);
            Ok(Value::Text(s))
        }
        // JSONB || JSONB → merge/concat
        (Value::Jsonb(a), Value::Jsonb(b)) => {
            let merged = match (a, b) {
                (serde_json::Value::Array(arr_a), serde_json::Value::Array(arr_b)) => {
                    let mut merged = Vec::with_capacity(arr_a.len() + arr_b.len());
                    merged.extend(arr_a.iter().cloned());
                    merged.extend(arr_b.iter().cloned());
                    serde_json::Value::Array(merged)
                }
                (serde_json::Value::Object(map_a), serde_json::Value::Object(map_b)) => {
                    let mut merged = map_a.clone();
                    merged.extend(map_b.iter().map(|(k, v)| (k.clone(), v.clone())));
                    serde_json::Value::Object(merged)
                }
                (a_val, b_val) => serde_json::Value::Array(vec![a_val.clone(), b_val.clone()]),
            };
            Ok(Value::Jsonb(merged))
        }
        // JSONB || TEXT → parse TEXT as JSON and merge
        (Value::Jsonb(_), Value::Text(s)) => {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(s) {
                eval_concat(left, &Value::Jsonb(parsed))
            } else {
                Err(DbError::internal(format!(
                    "cannot concatenate JSONB with invalid JSON text '{s}'"
                )))
            }
        }
        (Value::Text(s), Value::Jsonb(_)) => {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(s) {
                eval_concat(&Value::Jsonb(parsed), right)
            } else {
                Err(DbError::internal(format!(
                    "cannot concatenate invalid JSON text '{s}' with JSONB"
                )))
            }
        }
        // Array || Array → concatenation (pre-allocated)
        (Value::Array(a), Value::Array(b)) => {
            let mut result = Vec::with_capacity(a.len() + b.len());
            result.extend(a.iter().cloned());
            result.extend(b.iter().cloned());
            Ok(Value::Array(result))
        }
        // BYTEA || BYTEA → byte concatenation (pre-allocated)
        (Value::Blob(a), Value::Blob(b)) => {
            let mut result = Vec::with_capacity(a.len() + b.len());
            result.extend_from_slice(a);
            result.extend_from_slice(b);
            Ok(Value::Blob(result))
        }
        // Array || elem → append (pre-allocated)
        (Value::Array(a), elem) => {
            let mut result = Vec::with_capacity(a.len() + 1);
            result.extend(a.iter().cloned());
            result.push(elem.clone());
            Ok(Value::Array(result))
        }
        // elem || Array → prepend (pre-allocated)
        (elem, Value::Array(b)) => {
            let mut result = Vec::with_capacity(1 + b.len());
            result.push(elem.clone());
            result.extend(b.iter().cloned());
            Ok(Value::Array(result))
        }
        // Any || Any → cast to text and concatenate (PG compat)
        (Value::Text(a), other) => {
            let other_s = other.to_string();
            let mut s = String::with_capacity(a.len() + other_s.len());
            s.push_str(a);
            s.push_str(&other_s);
            Ok(Value::Text(s))
        }
        (other, Value::Text(b)) => {
            let other_s = other.to_string();
            let mut s = String::with_capacity(other_s.len() + b.len());
            s.push_str(&other_s);
            s.push_str(b);
            Ok(Value::Text(s))
        }
        _ => Err(DbError::internal(format!(
            "cannot concatenate {:?} and {:?}",
            left.data_type(),
            right.data_type()
        ))),
    }
}

pub(super) fn eval_is_distinct_from(left: &Value, right: &Value, negated: bool) -> DbResult<Value> {
    let distinct = match (left, right) {
        (Value::Null, Value::Null) => false,
        (Value::Null, _) | (_, Value::Null) => true,
        _ => match values_equal(left, right) {
            Ok(Some(eq)) => !eq,
            Ok(None) => true,
            Err(error) if is_incomparable_values_error(&error) => true,
            Err(error) => return Err(error),
        },
    };
    Ok(Value::Boolean(if negated { !distinct } else { distinct }))
}

fn is_incomparable_values_error(error: &DbError) -> bool {
    error.sqlstate() == SqlState::InternalError
        && error.report().message.starts_with("cannot compare ")
}

pub(super) fn eval_is_null(value: &Value, negated: bool) -> Value {
    let is_null = matches!(value, Value::Null);
    Value::Boolean(if negated { !is_null } else { is_null })
}

pub(super) fn eval_like(
    value: &Value,
    pattern: &Value,
    negated: bool,
    case_insensitive: bool,
) -> DbResult<Value> {
    match (value, pattern) {
        (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
        (Value::Text(text), Value::Text(pat)) => {
            let matches = if case_insensitive {
                // `String::to_lowercase` allocates unconditionally even when
                // the input is already lower-cased ASCII. The dominant ILIKE
                // pattern shape (a literal like `'%foo%'`) is exactly that
                // case, and skipping the allocation cuts two heap allocs per
                // scanned row down to (at worst) one. Rely on the fact that
                // ASCII case folding ≡ Unicode case folding for pure-ASCII
                // strings with no uppercase letters.
                let text_lower = lowercase_for_like(text);
                let pat_lower = lowercase_for_like(pat);
                sql_like_match(text_lower.as_ref(), pat_lower.as_ref())
            } else {
                sql_like_match(text, pat)
            };
            Ok(Value::Boolean(if negated { !matches } else { matches }))
        }
        _ => Err(DbError::internal("LIKE requires text operands")),
    }
}

/// Return a borrowed slice when `s` is already lowercase ASCII, otherwise
/// fall back to `to_lowercase()` for Unicode-correct case folding.
#[inline]
fn lowercase_for_like(s: &str) -> std::borrow::Cow<'_, str> {
    if s.is_ascii() && !s.bytes().any(|b| b.is_ascii_uppercase()) {
        std::borrow::Cow::Borrowed(s)
    } else {
        std::borrow::Cow::Owned(s.to_lowercase())
    }
}

pub(super) fn float_to_i32(v: f64) -> DbResult<Value> {
    if v.is_nan() || v.is_infinite() {
        return Err(DbError::from_report(ErrorReport::new(
            SqlState::NumericValueOutOfRange,
            "integer out of range",
        )));
    }
    // PostgreSQL rounds to nearest integer using rint() semantics (round half to even)
    let rounded = v.round_ties_even();
    if rounded < f64::from(i32::MIN) || rounded > f64::from(i32::MAX) {
        return Err(DbError::from_report(ErrorReport::new(
            SqlState::NumericValueOutOfRange,
            "integer out of range",
        )));
    }
    Ok(Value::Int(f64_to_i32(rounded)?))
}

pub(super) fn float_to_i64(v: f64) -> DbResult<Value> {
    if v.is_nan() || v.is_infinite() {
        return Err(DbError::from_report(ErrorReport::new(
            SqlState::NumericValueOutOfRange,
            "bigint out of range",
        )));
    }
    // PostgreSQL rounds to nearest integer using rint() semantics (round half to even).
    // Use strict bounds: i64::MIN is exactly representable as f64, but i64::MAX is
    // not (it rounds up). We need to check that the rounded value is strictly less
    // than i64::MAX as f64 (which is 9223372036854776000, larger than i64::MAX).
    let rounded = v.round_ties_even();
    // i64::MIN is exactly representable, i64::MAX rounds up to 9223372036854776000.0
    if rounded < i64_to_f64(i64::MIN) || rounded >= i64_to_f64(i64::MAX) {
        return Err(DbError::from_report(ErrorReport::new(
            SqlState::NumericValueOutOfRange,
            "bigint out of range",
        )));
    }
    Ok(Value::BigInt(f64_to_i64(rounded)?))
}

pub fn sql_like_match(text: &str, pattern: &str) -> bool {
    // Fast path: when both text and pattern are pure ASCII we can work on
    // byte slices directly, avoiding the overhead of char-by-char iteration.
    if text.is_ascii() && pattern.is_ascii() {
        return sql_like_match_ascii(text.as_bytes(), pattern.as_bytes());
    }
    let pattern: Vec<char> = pattern.chars().collect();
    let m = pattern.len();
    let mut previous = vec![false; m + 1];
    let mut current = vec![false; m + 1];
    previous[0] = true;
    for j in 1..=m {
        if pattern[j - 1] == '%' {
            previous[j] = previous[j - 1];
        }
    }
    for text_char in text.chars() {
        current[0] = false;
        for j in 1..=m {
            match pattern[j - 1] {
                '%' => current[j] = previous[j] || current[j - 1],
                '_' => current[j] = previous[j - 1],
                c => current[j] = previous[j - 1] && text_char == c,
            }
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[m]
}

/// ASCII-only LIKE matching on raw byte slices.
fn sql_like_match_ascii(text: &[u8], pattern: &[u8]) -> bool {
    // Classify the pattern into the four dominant shapes that real
    // workloads use overwhelmingly more than mixed wildcards:
    //   * `foo`    -> exact match
    //   * `foo%`   -> prefix
    //   * `%foo`   -> suffix
    //   * `%foo%`  -> substring
    // For these we can answer in O(n + m) with `slice::starts_with`,
    // `ends_with`, or memmem-backed contains, instead of the O(n*m) DP.
    // Falls back to DP only when the pattern contains internal wildcards
    // (`%` not at the edges, or any `_`).
    if let Some(simple) = classify_simple_like_ascii(pattern) {
        return match simple {
            SimpleLikeAscii::Exact(lit) => text == lit,
            SimpleLikeAscii::Prefix(lit) => text.starts_with(lit),
            SimpleLikeAscii::Suffix(lit) => text.ends_with(lit),
            SimpleLikeAscii::Contains(lit) => {
                if lit.is_empty() {
                    true
                } else {
                    memmem_contains(text, lit)
                }
            }
            SimpleLikeAscii::MatchAll => true,
        };
    }

    let m = pattern.len();
    // Most LIKE patterns are short. Keep the two DP rows on the stack for
    // patterns up to a generous bound and fall back to a heap allocation
    // only for outliers, eliminating two `vec![false; m + 1]` allocations
    // per row scanned through a LIKE filter.
    const STACK_PATTERN_CAP: usize = 256;
    if m < STACK_PATTERN_CAP {
        let mut prev_buf = [false; STACK_PATTERN_CAP + 1];
        let mut curr_buf = [false; STACK_PATTERN_CAP + 1];
        let prev = &mut prev_buf[..=m];
        let curr = &mut curr_buf[..=m];
        return sql_like_match_ascii_dp(text, pattern, prev, curr);
    }
    let mut prev = vec![false; m + 1];
    let mut curr = vec![false; m + 1];
    sql_like_match_ascii_dp(text, pattern, &mut prev, &mut curr)
}

#[derive(Debug)]
enum SimpleLikeAscii<'a> {
    Exact(&'a [u8]),
    Prefix(&'a [u8]),
    Suffix(&'a [u8]),
    Contains(&'a [u8]),
    MatchAll,
}

/// Returns `Some(_)` only when the pattern is one of the four simple
/// shapes that admit a linear-time match. Patterns containing `_`, an
/// internal `%`, or any backslash are not handled here and fall through
/// to the DP path.
fn classify_simple_like_ascii(pattern: &[u8]) -> Option<SimpleLikeAscii<'_>> {
    if pattern.is_empty() {
        return Some(SimpleLikeAscii::Exact(pattern));
    }
    let starts_pct = pattern.first() == Some(&b'%');
    let ends_pct = pattern.last() == Some(&b'%');
    let inner_start = usize::from(starts_pct);
    let inner_end = pattern.len() - usize::from(ends_pct && pattern.len() > inner_start);
    let inner = &pattern[inner_start..inner_end];
    // Reject any inner wildcard or escape byte; only the outer `%`
    // anchors are allowed.
    for &b in inner {
        if b == b'%' || b == b'_' || b == b'\\' {
            return None;
        }
    }
    Some(match (starts_pct, ends_pct) {
        (false, false) => SimpleLikeAscii::Exact(inner),
        (false, true) => SimpleLikeAscii::Prefix(inner),
        (true, false) => SimpleLikeAscii::Suffix(inner),
        (true, true) => {
            if inner.is_empty() {
                SimpleLikeAscii::MatchAll
            } else {
                SimpleLikeAscii::Contains(inner)
            }
        }
    })
}

/// Substring search using the standard library's optimised
/// `str::contains` (which dispatches to a SIMD-friendly two-way
/// algorithm for needles of moderate length). The byte slices are ASCII
/// so the UTF-8 validation is a single linear pass that the optimiser
/// usually elides into the contains call.
#[inline]
fn memmem_contains(haystack: &[u8], needle: &[u8]) -> bool {
    match (std::str::from_utf8(haystack), std::str::from_utf8(needle)) {
        (Ok(h), Ok(n)) => h.contains(n),
        _ => false,
    }
}

#[inline]
fn sql_like_match_ascii_dp(
    text: &[u8],
    pattern: &[u8],
    previous: &mut [bool],
    current: &mut [bool],
) -> bool {
    let m = pattern.len();
    previous[0] = true;
    for entry in previous.iter_mut().take(m + 1).skip(1) {
        *entry = false;
    }
    for j in 1..=m {
        if pattern[j - 1] == b'%' {
            previous[j] = previous[j - 1];
        }
    }
    let mut prev: &mut [bool] = previous;
    let mut curr: &mut [bool] = current;
    for &text_byte in text {
        curr[0] = false;
        for j in 1..=m {
            match pattern[j - 1] {
                b'%' => curr[j] = prev[j] || curr[j - 1],
                b'_' => curr[j] = prev[j - 1],
                c => curr[j] = prev[j - 1] && text_byte == c,
            }
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[m]
}

pub(super) fn eval_between(
    value: &Value,
    low: &Value,
    high: &Value,
    negated: bool,
) -> DbResult<Value> {
    let ge_low = compare_values(value, low)?.map(|ordering| ordering.is_ge());
    eval_between_with_ge_low(value, high, ge_low, negated)
}

pub(super) fn eval_between_with_ge_low(
    value: &Value,
    high: &Value,
    ge_low: Option<bool>,
    negated: bool,
) -> DbResult<Value> {
    // FALSE AND x is always FALSE in SQL three-valued logic, so we can avoid
    // computing the upper-bound comparison for the common early-exit case.
    if ge_low == Some(false) {
        return Ok(Value::Boolean(negated));
    }
    let le_high = compare_values(value, high)?.map(|ordering| ordering.is_le());
    // x AND FALSE is always FALSE, including when x is NULL.
    if le_high == Some(false) {
        return Ok(Value::Boolean(negated));
    }

    let result = sql_nullable_and(ge_low, le_high);
    if negated {
        Ok(nullable_bool_to_value(sql_nullable_not(result)))
    } else {
        Ok(nullable_bool_to_value(result))
    }
}

#[cfg(test)]
mod tests;
