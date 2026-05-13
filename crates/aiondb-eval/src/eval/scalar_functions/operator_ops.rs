use super::geometric::{
    bounds_strict_left, bounds_strict_right, parse_box_text, parse_geometry_bounds,
    parse_line_coefficients, parse_lseg_text, parse_point_text, GeomPoint,
};
use super::range as range_ops;
use super::value_convert::i64_to_f64;
use aiondb_core::{DbError, DbResult, ErrorReport, MacAddr, MacAddr8, SqlState, Value};

fn null_if_too_few_args(args: &[Value], minimum: usize) -> Option<Value> {
    (args.len() < minimum).then_some(Value::Null)
}

#[inline]
fn i32_to_u32_wrapping(value: i32) -> u32 {
    u32::from_ne_bytes(value.to_ne_bytes())
}

#[inline]
fn nonneg_i64_to_u32_saturating(value: i64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn coerce_macaddr_operand(value: &Value, mac8: bool) -> Option<Value> {
    match (value, mac8) {
        (Value::MacAddr(value), false) => Some(Value::MacAddr(*value)),
        (Value::MacAddr8(value), true) => Some(Value::MacAddr8(*value)),
        (Value::Text(text), false) => MacAddr::parse(text).map(Value::MacAddr),
        (Value::Text(text), true) => MacAddr8::parse(text).map(Value::MacAddr8),
        _ => None,
    }
}

fn format_geom_number(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value == f64::INFINITY {
        return "Infinity".to_owned();
    }
    if value == f64::NEG_INFINITY {
        return "-Infinity".to_owned();
    }
    let mut text = format!("{value:.12}");
    while text.contains('.') && text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    if text == "-0" {
        return "0".to_owned();
    }
    text
}

fn format_geom_point(point: GeomPoint) -> String {
    format!(
        "({},{})",
        format_geom_number(point.x),
        format_geom_number(point.y)
    )
}

fn line_intersection_point(left: (f64, f64, f64), right: (f64, f64, f64)) -> Option<GeomPoint> {
    let (a1, b1, c1) = left;
    let (a2, b2, c2) = right;
    let det = a1 * b2 - a2 * b1;
    if det.abs() < 1e-12 {
        return None;
    }
    let x = (b1 * c2 - b2 * c1) / det;
    let y = (c1 * a2 - c2 * a1) / det;
    Some(GeomPoint { x, y })
}

fn point_on_segment(point: GeomPoint, a: GeomPoint, b: GeomPoint) -> bool {
    let eps = 1e-10;
    point.x >= a.x.min(b.x) - eps
        && point.x <= a.x.max(b.x) + eps
        && point.y >= a.y.min(b.y) - eps
        && point.y <= a.y.max(b.y) + eps
}

fn segment_intersection_point(
    left: (GeomPoint, GeomPoint),
    right: (GeomPoint, GeomPoint),
) -> Option<GeomPoint> {
    let (l1, l2) = left;
    let (r1, r2) = right;
    let a1 = l1.y - l2.y;
    let b1 = l2.x - l1.x;
    let c1 = l1.x * l2.y - l2.x * l1.y;
    let a2 = r1.y - r2.y;
    let b2 = r2.x - r1.x;
    let c2 = r1.x * r2.y - r2.x * r1.y;
    let intersection = line_intersection_point((a1, b1, c1), (a2, b2, c2))?;
    if point_on_segment(intersection, l1, l2) && point_on_segment(intersection, r1, r2) {
        Some(intersection)
    } else {
        None
    }
}

fn eval_regex_match_bool_with_options(
    args: &[Value],
    case_insensitive: bool,
    negate: bool,
) -> Value {
    if let Some(value) = null_if_too_few_args(args, 2) {
        return value;
    }
    match (&args[0], &args[1]) {
        (Value::Null, _) | (_, Value::Null) => Value::Null,
        (Value::Text(text), Value::Text(pattern)) => {
            // `~`, `~*`, `!~`, `!~*` are scalar operators applied per row.
            // Route through the cache so a `WHERE col ~ '^abc'` pattern gets
            // compiled once and reused across the scan instead of being
            // recompiled per row.
            match crate::regex_cache::get_ci(pattern, case_insensitive) {
                Ok(re) => Value::Boolean(re.is_match(text) ^ negate),
                Err(_) => Value::Boolean(false ^ negate),
            }
        }
        _ => Value::Boolean(false ^ negate),
    }
}

pub(super) fn eval_regex_match_bool(args: &[Value]) -> Value {
    eval_regex_match_bool_with_options(args, false, false)
}

pub(super) fn eval_regex_match_bool_insensitive(args: &[Value]) -> Value {
    eval_regex_match_bool_with_options(args, true, false)
}

pub(super) fn eval_regex_not_match_bool(args: &[Value]) -> Value {
    eval_regex_match_bool_with_options(args, false, true)
}

pub(super) fn eval_regex_not_match_bool_insensitive(args: &[Value]) -> Value {
    eval_regex_match_bool_with_options(args, true, true)
}

pub(super) fn eval_bitwise_not(args: &[Value]) -> Value {
    if let Some(value) = null_if_too_few_args(args, 1) {
        return value;
    }
    match &args[0] {
        Value::Null => Value::Null,
        Value::Int(v) => Value::Int(!v),
        Value::BigInt(v) => Value::BigInt(!v),
        Value::MacAddr(v) => Value::MacAddr(v.bitnot()),
        Value::MacAddr8(v) => Value::MacAddr8(v.bitnot()),
        _ => Value::Null,
    }
}

pub(super) fn eval_bitwise_and(args: &[Value]) -> Value {
    if let Some(value) = null_if_too_few_args(args, 2) {
        return value;
    }
    match (&args[0], &args[1]) {
        (Value::Null, _) | (_, Value::Null) => Value::Null,
        (Value::Int(a), Value::Int(b)) => Value::Int(a & b),
        (Value::BigInt(a), Value::BigInt(b)) => Value::BigInt(a & b),
        (Value::Int(a), Value::BigInt(b)) => Value::BigInt(i64::from(*a) & b),
        (Value::BigInt(a), Value::Int(b)) => Value::BigInt(a & i64::from(*b)),
        (
            left @ (Value::MacAddr(_) | Value::Text(_)),
            right @ (Value::MacAddr(_) | Value::Text(_)),
        ) => {
            match (
                coerce_macaddr_operand(left, false),
                coerce_macaddr_operand(right, false),
            ) {
                (Some(Value::MacAddr(a)), Some(Value::MacAddr(b))) => Value::MacAddr(a.bitand(b)),
                _ => Value::Null,
            }
        }
        (
            left @ (Value::MacAddr8(_) | Value::Text(_)),
            right @ (Value::MacAddr8(_) | Value::Text(_)),
        ) => {
            match (
                coerce_macaddr_operand(left, true),
                coerce_macaddr_operand(right, true),
            ) {
                (Some(Value::MacAddr8(a)), Some(Value::MacAddr8(b))) => {
                    Value::MacAddr8(a.bitand(b))
                }
                _ => Value::Null,
            }
        }
        _ => Value::Null,
    }
}

pub(super) fn eval_bitwise_or(args: &[Value]) -> Value {
    if let Some(value) = null_if_too_few_args(args, 2) {
        return value;
    }
    match (&args[0], &args[1]) {
        (Value::Null, _) | (_, Value::Null) => Value::Null,
        (Value::Int(a), Value::Int(b)) => Value::Int(a | b),
        (Value::BigInt(a), Value::BigInt(b)) => Value::BigInt(a | b),
        (Value::Int(a), Value::BigInt(b)) => Value::BigInt(i64::from(*a) | b),
        (Value::BigInt(a), Value::Int(b)) => Value::BigInt(a | i64::from(*b)),
        (
            left @ (Value::MacAddr(_) | Value::Text(_)),
            right @ (Value::MacAddr(_) | Value::Text(_)),
        ) => {
            match (
                coerce_macaddr_operand(left, false),
                coerce_macaddr_operand(right, false),
            ) {
                (Some(Value::MacAddr(a)), Some(Value::MacAddr(b))) => Value::MacAddr(a.bitor(b)),
                _ => Value::Null,
            }
        }
        (
            left @ (Value::MacAddr8(_) | Value::Text(_)),
            right @ (Value::MacAddr8(_) | Value::Text(_)),
        ) => {
            match (
                coerce_macaddr_operand(left, true),
                coerce_macaddr_operand(right, true),
            ) {
                (Some(Value::MacAddr8(a)), Some(Value::MacAddr8(b))) => Value::MacAddr8(a.bitor(b)),
                _ => Value::Null,
            }
        }
        _ => Value::Null,
    }
}

pub(super) fn eval_bitwise_xor(args: &[Value]) -> DbResult<Value> {
    if let Some(value) = null_if_too_few_args(args, 2) {
        return Ok(value);
    }
    match (&args[0], &args[1]) {
        (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
        (Value::Text(left), Value::Text(right))
            if parse_line_coefficients(left).is_ok() && parse_line_coefficients(right).is_ok() =>
        {
            let left_line = parse_line_coefficients(left)?;
            let right_line = parse_line_coefficients(right)?;
            Ok(line_intersection_point(left_line, right_line)
                .map_or(Value::Null, |point| Value::Text(format_geom_point(point))))
        }
        (Value::Text(left), Value::Text(right))
            if parse_lseg_text(left).is_ok() && parse_lseg_text(right).is_ok() =>
        {
            let left_seg = parse_lseg_text(left)?;
            let right_seg = parse_lseg_text(right)?;
            Ok(segment_intersection_point(left_seg, right_seg)
                .map_or(Value::Null, |point| Value::Text(format_geom_point(point))))
        }
        (Value::Text(left), Value::Text(right))
            if (parse_lseg_text(left).is_ok() && parse_point_text(right).is_ok())
                || (parse_point_text(left).is_ok() && parse_lseg_text(right).is_ok()) =>
        {
            let (left_name, right_name) = if parse_lseg_text(left).is_ok() {
                ("lseg", "point")
            } else {
                ("point", "lseg")
            };
            Err(DbError::from_report(
                ErrorReport::new(
                    SqlState::UndefinedFunction,
                    format!("operator does not exist: {left_name} # {right_name}"),
                )
                .with_client_hint(
                    "No operator matches the given name and argument types. You might need to add explicit type casts.",
                ),
            ))
        }
        (Value::Text(left), Value::Text(right))
            if parse_box_text(left).is_ok() && parse_box_text(right).is_ok() =>
        {
            let (la, lb) = parse_box_text(left)?;
            let (ra, rb) = parse_box_text(right)?;
            let xmin = la.x.min(lb.x).max(ra.x.min(rb.x));
            let xmax = la.x.max(lb.x).min(ra.x.max(rb.x));
            let ymin = la.y.min(lb.y).max(ra.y.min(rb.y));
            let ymax = la.y.max(lb.y).min(ra.y.max(rb.y));
            if xmin <= xmax && ymin <= ymax {
                let upper = format!(
                    "({},{})",
                    format_geom_number(xmax),
                    format_geom_number(ymax)
                );
                let lower = format!(
                    "({},{})",
                    format_geom_number(xmin),
                    format_geom_number(ymin)
                );
                Ok(Value::Text(format!("{upper},{lower}")))
            } else {
                Ok(Value::Null)
            }
        }
        (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a ^ b)),
        (Value::BigInt(a), Value::BigInt(b)) => Ok(Value::BigInt(a ^ b)),
        (Value::Int(a), Value::BigInt(b)) => Ok(Value::BigInt(i64::from(*a) ^ b)),
        (Value::BigInt(a), Value::Int(b)) => Ok(Value::BigInt(a ^ i64::from(*b))),
        (
            left @ (Value::MacAddr(_) | Value::Text(_)),
            right @ (Value::MacAddr(_) | Value::Text(_)),
        ) => {
            match (
                coerce_macaddr_operand(left, false),
                coerce_macaddr_operand(right, false),
            ) {
                (Some(Value::MacAddr(a)), Some(Value::MacAddr(b))) => {
                    Ok(Value::MacAddr(a.bitxor(b)))
                }
                _ => Ok(Value::Null),
            }
        }
        (
            left @ (Value::MacAddr8(_) | Value::Text(_)),
            right @ (Value::MacAddr8(_) | Value::Text(_)),
        ) => {
            match (
                coerce_macaddr_operand(left, true),
                coerce_macaddr_operand(right, true),
            ) {
                (Some(Value::MacAddr8(a)), Some(Value::MacAddr8(b))) => {
                    Ok(Value::MacAddr8(a.bitxor(b)))
                }
                _ => Ok(Value::Null),
            }
        }
        _ => Ok(Value::Null),
    }
}

pub(super) fn eval_shift_left(args: &[Value]) -> DbResult<Value> {
    if let Some(value) = null_if_too_few_args(args, 2) {
        return Ok(value);
    }
    match (&args[0], &args[1]) {
        (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
        // Integer bit-shift
        (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a.wrapping_shl(i32_to_u32_wrapping(*b)))),
        (Value::Int(a), Value::BigInt(b)) if *b >= 0 => {
            Ok(Value::Int(a.wrapping_shl(nonneg_i64_to_u32_saturating(*b))))
        }
        (Value::BigInt(a), Value::Int(b)) => {
            Ok(Value::BigInt(a.wrapping_shl(i32_to_u32_wrapping(*b))))
        }
        (Value::BigInt(a), Value::BigInt(b)) if *b >= 0 => Ok(Value::BigInt(
            a.wrapping_shl(nonneg_i64_to_u32_saturating(*b)),
        )),
        (Value::Text(left_text), Value::Text(right_text)) => {
            // PG `<<` on inet/cidr means "is strictly contained by the right
            // subnet". Tried before range/geometry fallbacks so an inet-only
            // pair never falls through to the boolean false default.
            if let Some(result) = inet_subnet_strictly_contained_by(left_text, right_text) {
                return Ok(Value::Boolean(result));
            }
            if range_ops::looks_like_range(left_text)
                || range_ops::looks_like_multirange(left_text)
                || range_ops::looks_like_range(right_text)
                || range_ops::looks_like_multirange(right_text)
            {
                return range_ops::eval_range_strictly_left_generic(&args[0], &args[1]);
            }
            if let (Ok(left_bounds), Ok(right_bounds)) = (
                parse_geometry_bounds(left_text),
                parse_geometry_bounds(right_text),
            ) {
                return Ok(Value::Boolean(bounds_strict_left(
                    left_bounds,
                    right_bounds,
                )));
            }
            Ok(Value::Boolean(false))
        }
        // For non-integer non-geometric types, << is treated as false.
        _ => Ok(Value::Boolean(false)),
    }
}

pub(super) fn eval_shift_right(args: &[Value]) -> DbResult<Value> {
    if let Some(value) = null_if_too_few_args(args, 2) {
        return Ok(value);
    }
    match (&args[0], &args[1]) {
        (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
        // Integer bit-shift
        (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a.wrapping_shr(i32_to_u32_wrapping(*b)))),
        (Value::Int(a), Value::BigInt(b)) if *b >= 0 => {
            Ok(Value::Int(a.wrapping_shr(nonneg_i64_to_u32_saturating(*b))))
        }
        (Value::BigInt(a), Value::Int(b)) => {
            Ok(Value::BigInt(a.wrapping_shr(i32_to_u32_wrapping(*b))))
        }
        (Value::BigInt(a), Value::BigInt(b)) if *b >= 0 => Ok(Value::BigInt(
            a.wrapping_shr(nonneg_i64_to_u32_saturating(*b)),
        )),
        (Value::Text(left_text), Value::Text(right_text)) => {
            // PG `>>` on inet/cidr means "strictly contains right subnet".
            // Tried before range/geometry so an inet-only pair never falls
            // through to the boolean false default.
            if let Some(result) = inet_subnet_strictly_contained_by(right_text, left_text) {
                return Ok(Value::Boolean(result));
            }
            if range_ops::looks_like_range(left_text)
                || range_ops::looks_like_multirange(left_text)
                || range_ops::looks_like_range(right_text)
                || range_ops::looks_like_multirange(right_text)
            {
                return range_ops::eval_range_strictly_right_generic(&args[0], &args[1]);
            }
            if let (Ok(left_bounds), Ok(right_bounds)) = (
                parse_geometry_bounds(left_text),
                parse_geometry_bounds(right_text),
            ) {
                return Ok(Value::Boolean(bounds_strict_right(
                    left_bounds,
                    right_bounds,
                )));
            }
            Ok(Value::Boolean(false))
        }
        // For non-integer non-geometric types, >> is treated as false.
        _ => Ok(Value::Boolean(false)),
    }
}

/// Parse `addr/prefix` into `(IpAddr, prefix_bits)`. When the input has no
/// `/N` suffix, the prefix defaults to the address-family maximum (32 for
/// IPv4, 128 for IPv6) - matching PG's `inet` semantics for prefix-less
/// addresses.
fn parse_inet_text(text: &str) -> Option<(std::net::IpAddr, u8)> {
    let (addr_str, prefix) = match text.split_once('/') {
        Some((a, p)) => (a, Some(p.parse::<u8>().ok()?)),
        None => (text, None),
    };
    let ip = addr_str.parse::<std::net::IpAddr>().ok()?;
    let max_prefix = match ip {
        std::net::IpAddr::V4(_) => 32,
        std::net::IpAddr::V6(_) => 128,
    };
    let prefix = prefix.unwrap_or(max_prefix);
    if prefix > max_prefix {
        return None;
    }
    Some((ip, prefix))
}

/// Mask `addr` to the leading `prefix` bits (high-bit first), zeroing the
/// remainder. Used to test whether the network portion of an address falls
/// inside another subnet.
fn mask_inet_addr(addr: std::net::IpAddr, prefix: u8) -> Option<std::net::IpAddr> {
    match addr {
        std::net::IpAddr::V4(v4) => {
            if prefix > 32 {
                return None;
            }
            let bits = u32::from_be_bytes(v4.octets());
            let mask = if prefix == 0 {
                0
            } else {
                u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0)
            };
            Some(std::net::IpAddr::V4((bits & mask).into()))
        }
        std::net::IpAddr::V6(v6) => {
            if prefix > 128 {
                return None;
            }
            let bits = u128::from_be_bytes(v6.octets());
            let mask = if prefix == 0 {
                0
            } else {
                u128::MAX.checked_shl(128 - u32::from(prefix)).unwrap_or(0)
            };
            Some(std::net::IpAddr::V6((bits & mask).into()))
        }
    }
}

/// Like [`inet_subnet_strictly_contained_by`] but `<=` includes the equal-
/// prefix case (left's network falls inside right with prefix `>=` right's).
fn inet_subnet_contained_by_or_equals_impl(left: &str, right: &str) -> Option<bool> {
    let (left_ip, left_prefix) = parse_inet_text(left)?;
    let (right_ip, right_prefix) = parse_inet_text(right)?;
    if std::mem::discriminant(&left_ip) != std::mem::discriminant(&right_ip) {
        return Some(false);
    }
    if left_prefix < right_prefix {
        return Some(false);
    }
    let left_masked = mask_inet_addr(left_ip, right_prefix)?;
    let right_masked = mask_inet_addr(right_ip, right_prefix)?;
    Some(left_masked == right_masked)
}

/// `inet_subnet_contained_by_or_equals(a, b)` → SQL `a <<= b`.
pub(crate) fn eval_inet_subnet_contained_by_or_equals(args: &[Value]) -> DbResult<Value> {
    if args.len() != 2 {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            "inet_subnet_contained_by_or_equals() expects 2 arguments",
        ));
    }
    match (&args[0], &args[1]) {
        (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
        (Value::Text(left), Value::Text(right)) => Ok(Value::Boolean(
            inet_subnet_contained_by_or_equals_impl(left, right).unwrap_or(false),
        )),
        _ => Ok(Value::Boolean(false)),
    }
}

/// `inet_subnet_contains_or_equals(a, b)` → SQL `a >>= b`.
pub(crate) fn eval_inet_subnet_contains_or_equals(args: &[Value]) -> DbResult<Value> {
    if args.len() != 2 {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            "inet_subnet_contains_or_equals() expects 2 arguments",
        ));
    }
    match (&args[0], &args[1]) {
        (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
        (Value::Text(left), Value::Text(right)) => Ok(Value::Boolean(
            // a >>= b ⇔ b <<= a
            inet_subnet_contained_by_or_equals_impl(right, left).unwrap_or(false),
        )),
        _ => Ok(Value::Boolean(false)),
    }
}

/// `left << right`: returns `Some(true)` when both are valid inet/cidr and
/// `left`'s prefix is strictly *more specific* (greater prefix length) AND
/// `left`'s network portion falls inside `right`'s subnet. Returns
/// `Some(false)` when both are valid inet but the relation does not hold.
/// Returns `None` when either side is not parseable as inet - caller falls
/// through to other semantics (range / geometry / boolean false).
fn inet_subnet_strictly_contained_by(left: &str, right: &str) -> Option<bool> {
    let (left_ip, left_prefix) = parse_inet_text(left)?;
    let (right_ip, right_prefix) = parse_inet_text(right)?;
    if std::mem::discriminant(&left_ip) != std::mem::discriminant(&right_ip) {
        return Some(false);
    }
    if left_prefix <= right_prefix {
        return Some(false);
    }
    let left_masked = mask_inet_addr(left_ip, right_prefix)?;
    let right_masked = mask_inet_addr(right_ip, right_prefix)?;
    Some(left_masked == right_masked)
}

pub(super) fn eval_exponent(args: &[Value]) -> DbResult<Value> {
    if let Some(value) = null_if_too_few_args(args, 2) {
        return Ok(value);
    }
    match (&args[0], &args[1]) {
        (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
        (Value::Int(a), Value::Int(b)) => {
            if *b >= 0 && *b < 32 {
                Ok(Value::Int(a.wrapping_pow(u32::try_from(*b).unwrap_or(0))))
            } else {
                Ok(Value::Double(f64::from(*a).powf(f64::from(*b))))
            }
        }
        (Value::Int(a), Value::BigInt(b)) => Ok(Value::Double(f64::from(*a).powf(i64_to_f64(*b)))),
        (Value::BigInt(a), Value::Int(b)) => {
            if *b >= 0 && *b < 64 {
                Ok(Value::BigInt(
                    a.wrapping_pow(u32::try_from(*b).unwrap_or(0)),
                ))
            } else {
                Ok(Value::Double(i64_to_f64(*a).powf(f64::from(*b))))
            }
        }
        (Value::BigInt(a), Value::BigInt(b)) => {
            Ok(Value::Double(i64_to_f64(*a).powf(i64_to_f64(*b))))
        }
        (Value::Real(a), Value::Real(b)) => Ok(Value::Double(f64::from(*a).powf(f64::from(*b)))),
        (Value::Real(a), Value::Int(b)) => Ok(Value::Double(f64::from(*a).powf(f64::from(*b)))),
        (Value::Int(a), Value::Real(b)) => Ok(Value::Double(f64::from(*a).powf(f64::from(*b)))),
        (Value::Double(a), Value::Double(b)) => Ok(Value::Double(a.powf(*b))),
        (Value::Int(a), Value::Double(b)) => Ok(Value::Double(f64::from(*a).powf(*b))),
        (Value::Double(a), Value::Int(b)) => Ok(Value::Double(a.powf(f64::from(*b)))),
        (Value::Double(a), Value::BigInt(b)) => Ok(Value::Double(a.powf(i64_to_f64(*b)))),
        (Value::BigInt(a), Value::Double(b)) => Ok(Value::Double(i64_to_f64(*a).powf(*b))),
        (Value::Real(a), Value::Double(b)) => Ok(Value::Double(f64::from(*a).powf(*b))),
        (Value::Double(a), Value::Real(b)) => Ok(Value::Double(a.powf(f64::from(*b)))),
        // For any combination involving Numeric, delegate to eval_power
        // which handles Numeric inputs properly (NaN, Infinity, proper precision).
        (Value::Numeric(_), _) | (_, Value::Numeric(_)) => super::math::eval_power(args),
        // Implicit text-to-double coercion for ^ operator (PostgreSQL compat)
        (Value::Double(a), Value::Text(s)) => {
            let b: f64 = s.trim().parse().map_err(|_| {
                DbError::internal(format!(
                    "invalid input syntax for type double precision: \"{s}\""
                ))
            })?;
            let result = a.powf(b);
            if result.is_infinite() && !a.is_infinite() && !b.is_infinite() {
                return Err(DbError::from_report(aiondb_core::ErrorReport::new(
                    aiondb_core::SqlState::NumericValueOutOfRange,
                    "value out of range: overflow",
                )));
            }
            Ok(Value::Double(result))
        }
        (Value::Text(s), Value::Double(b)) => {
            let a: f64 = s.trim().parse().map_err(|_| {
                DbError::internal(format!(
                    "invalid input syntax for type double precision: \"{s}\""
                ))
            })?;
            let result = a.powf(*b);
            if result.is_infinite() && !a.is_infinite() && !b.is_infinite() {
                return Err(DbError::from_report(aiondb_core::ErrorReport::new(
                    aiondb_core::SqlState::NumericValueOutOfRange,
                    "value out of range: overflow",
                )));
            }
            Ok(Value::Double(result))
        }
        (Value::Int(a), Value::Text(s)) => {
            let b: f64 = s.trim().parse().map_err(|_| {
                DbError::internal(format!(
                    "invalid input syntax for type double precision: \"{s}\""
                ))
            })?;
            Ok(Value::Double(f64::from(*a).powf(b)))
        }
        (Value::Real(a), Value::Text(s)) => {
            let b: f64 = s.trim().parse().map_err(|_| {
                DbError::internal(format!(
                    "invalid input syntax for type double precision: \"{s}\""
                ))
            })?;
            Ok(Value::Double(f64::from(*a).powf(b)))
        }
        _ => Ok(Value::Null),
    }
}

#[cfg(test)]
mod inet_subnet_tests {
    use super::*;

    fn shl(left: &str, right: &str) -> Value {
        eval_shift_left(&[Value::Text(left.to_owned()), Value::Text(right.to_owned())])
            .expect("eval_shift_left")
    }

    fn shr(left: &str, right: &str) -> Value {
        eval_shift_right(&[Value::Text(left.to_owned()), Value::Text(right.to_owned())])
            .expect("eval_shift_right")
    }

    #[test]
    fn ipv4_strict_subnet_contained() {
        // 192.168.1.5 in 192.168.1.0/24 with strict prefix length increase.
        assert_eq!(shl("192.168.1.5", "192.168.1.0/24"), Value::Boolean(true));
    }

    #[test]
    fn ipv4_same_prefix_is_not_strict_subset() {
        // 192.168.1.0/24 << 192.168.1.0/24 is false (not strictly contained).
        assert_eq!(
            shl("192.168.1.0/24", "192.168.1.0/24"),
            Value::Boolean(false)
        );
    }

    #[test]
    fn ipv4_outside_subnet_is_false() {
        assert_eq!(shl("10.0.0.1", "192.168.1.0/24"), Value::Boolean(false));
    }

    #[test]
    fn ipv6_strict_subnet_contained() {
        assert_eq!(shl("2001:db8::1", "2001:db8::/32"), Value::Boolean(true));
    }

    #[test]
    fn shift_right_is_inverse() {
        assert_eq!(shr("192.168.1.0/24", "192.168.1.5"), Value::Boolean(true));
    }

    #[test]
    fn cross_family_ipv4_vs_ipv6_is_false() {
        assert_eq!(shl("192.168.1.5", "2001:db8::/32"), Value::Boolean(false));
    }

    #[test]
    fn contained_by_or_equals_accepts_equal_prefix() {
        let r = eval_inet_subnet_contained_by_or_equals(&[
            Value::Text("192.168.1.0/24".to_owned()),
            Value::Text("192.168.1.0/24".to_owned()),
        ])
        .unwrap();
        assert_eq!(r, Value::Boolean(true));
    }

    #[test]
    fn contained_by_or_equals_rejects_outside() {
        let r = eval_inet_subnet_contained_by_or_equals(&[
            Value::Text("10.0.0.1".to_owned()),
            Value::Text("192.168.1.0/24".to_owned()),
        ])
        .unwrap();
        assert_eq!(r, Value::Boolean(false));
    }

    #[test]
    fn contains_or_equals_is_inverse() {
        let r = eval_inet_subnet_contains_or_equals(&[
            Value::Text("192.168.1.0/24".to_owned()),
            Value::Text("192.168.1.0/24".to_owned()),
        ])
        .unwrap();
        assert_eq!(r, Value::Boolean(true));
    }
}
