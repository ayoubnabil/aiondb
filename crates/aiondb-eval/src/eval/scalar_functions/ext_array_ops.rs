//\! Array operator functions and geometric helpers extracted from ext.rs.

use super::ext::{coerce_to_array, concatenate_array_values};
use super::geometric::{bounds_overlap, parse_geometry_bounds};
use crate::eval::operators::values_equal;
use aiondb_core::{DbError, DbResult, Value};

pub(crate) fn eval_array_contains_op(left: &Value, right: &Value) -> DbResult<Value> {
    match (left, right) {
        (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
        _ => {
            let haystack = coerce_to_array(left);
            let needle = coerce_to_array(right);
            match (haystack, needle) {
                (Some(haystack), Some(needle)) => {
                    for elem in needle.iter() {
                        let mut found = false;
                        for h in haystack.iter() {
                            if values_equal(h, elem)? == Some(true) {
                                found = true;
                                break;
                            }
                        }
                        if !found {
                            return Ok(Value::Boolean(false));
                        }
                    }
                    Ok(Value::Boolean(true))
                }
                _ => Err(DbError::internal("@> on arrays requires array operands")),
            }
        }
    }
}
/// Evaluate array overlap: do the two arrays share at least one element?
/// Also handles geometric box overlap and range overlap via `&&`.
pub(crate) fn eval_array_overlap_op(left: &Value, right: &Value) -> DbResult<Value> {
    match (left, right) {
        (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
        (Value::Text(ls), Value::Text(rs)) => {
            // PG semantics: `inet && inet` is true when one network is
            // contained in the other (equal counts). Detect parseable inet
            // pairs first so the geometric/range/box fallbacks don't
            // misinterpret addresses.
            if let Some(result) = inet_overlaps(ls, rs) {
                return Ok(Value::Boolean(result));
            }
            if let (Ok(left_bounds), Ok(right_bounds)) =
                (parse_geometry_bounds(ls), parse_geometry_bounds(rs))
            {
                return Ok(Value::Boolean(bounds_overlap(left_bounds, right_bounds)));
            }
            // Range overlap
            if super::range::looks_like_range(ls) && super::range::looks_like_range(rs) {
                return super::range::eval_range_overlap(left, right);
            }
            if super::range::looks_like_range(ls) && super::range::looks_like_multirange(rs) {
                return super::range::eval_range_overlaps_multirange(left, right);
            }
            if super::range::looks_like_multirange(ls) && super::range::looks_like_range(rs) {
                return super::range::eval_multirange_overlaps_range(left, right);
            }
            if super::range::looks_like_multirange(ls) && super::range::looks_like_multirange(rs) {
                return super::range::eval_multirange_overlaps_multirange(left, right);
            }
            // Geometric box overlap
            if looks_like_box(ls) && looks_like_box(rs) {
                return match (parse_box_coords(ls), parse_box_coords(rs)) {
                    (Some((ax1, ay1, ax2, ay2)), Some((bx1, by1, bx2, by2))) => {
                        let a_xmin = ax1.min(ax2);
                        let a_xmax = ax1.max(ax2);
                        let a_ymin = ay1.min(ay2);
                        let a_ymax = ay1.max(ay2);
                        let b_xmin = bx1.min(bx2);
                        let b_xmax = bx1.max(bx2);
                        let b_ymin = by1.min(by2);
                        let b_ymax = by1.max(by2);
                        let overlaps = a_xmin <= b_xmax
                            && a_xmax >= b_xmin
                            && a_ymin <= b_ymax
                            && a_ymax >= b_ymin;
                        Ok(Value::Boolean(overlaps))
                    }
                    _ => Err(DbError::internal("could not parse box operands for &&")),
                };
            }
            Err(DbError::internal("&& on arrays requires array operands"))
        }
        // Range overlap
        _ => {
            let a = coerce_to_array(left);
            let b = coerce_to_array(right);
            match (a, b) {
                (Some(a), Some(b)) => {
                    for ea in a.iter() {
                        for eb in b.iter() {
                            if values_equal(ea, eb)? == Some(true) {
                                return Ok(Value::Boolean(true));
                            }
                        }
                    }
                    Ok(Value::Boolean(false))
                }
                _ => Err(DbError::internal("&& on arrays requires array operands")),
            }
        }
    }
}
/// Check if a text value looks like a `PostgreSQL` box literal.
/// Matches formats: `box(point(x1,y1),point(x2,y2))` and `(x1,y1),(x2,y2)`.
fn looks_like_box(s: &str) -> bool {
    let s = s.trim();
    s.starts_with("box(") || (s.starts_with('(') && s.contains("),("))
}
/// Parse a box text representation into four coordinates (x1, y1, x2, y2).
/// Supports formats:
///   - `box(point(x1,y1),point(x2,y2))`
///   - `box((x1,y1),(x2,y2))`
///   - `(x1,y1),(x2,y2)`
fn parse_box_coords(s: &str) -> Option<(f64, f64, f64, f64)> {
    let s = s.trim();
    // Extract the inner coordinate text
    let inner = if let Some(rest) = s.strip_prefix("box(") {
        rest.strip_suffix(')')? // remove trailing ')'
    } else {
        s
    };
    // Now inner should be like "point(x1,y1),point(x2,y2)" or "(x1,y1),(x2,y2)"
    // Find the split between the two points
    let (p1, p2) = if let Some(rest) = inner.strip_prefix("point(") {
        // "point(x1,y1),point(x2,y2)"
        let close = rest.find(')')?;
        let coords1 = &rest[..close];
        let remaining = &rest[close + 1..]; // ",point(x2,y2)"
        let remaining = remaining.strip_prefix(',')?;
        let remaining = remaining.strip_prefix("point(")?;
        let coords2 = remaining.strip_suffix(')')?;
        (coords1, coords2)
    } else if let Some(inner_trimmed) = inner.strip_prefix('(') {
        // "(x1,y1),(x2,y2)"
        // "x1,y1),(x2,y2)"
        let close = inner_trimmed.find(')')?;
        let coords1 = &inner_trimmed[..close];
        let remaining = &inner_trimmed[close + 1..]; // ",(x2,y2)"
        let remaining = remaining.strip_prefix(',')?;
        let remaining = remaining.strip_prefix('(')?;
        let coords2 = remaining.strip_suffix(')')?;
        (coords1, coords2)
    } else {
        return None;
    };
    let (x1, y1) = parse_point_pair(p1)?;
    let (x2, y2) = parse_point_pair(p2)?;
    Some((x1, y1, x2, y2))
}
/// Parse "x,y" into (f64, f64).
fn parse_point_pair(s: &str) -> Option<(f64, f64)> {
    let comma = s.find(',')?;
    let x: f64 = s[..comma].trim().parse().ok()?;
    let y: f64 = s[comma + 1..].trim().parse().ok()?;
    Some((x, y))
}
/// Evaluate array concatenation for the || operator.
///
/// Takes ownership of both operands to avoid unnecessary cloning.
pub(crate) fn eval_array_concat_op(left: Value, right: Value) -> DbResult<Value> {
    match (left, right) {
        (Value::Null, Value::Null) => Ok(Value::Null),
        (Value::Null, arr @ Value::Array(_)) => Ok(arr),
        (arr @ Value::Array(_), Value::Null) => Ok(arr),
        (Value::Array(l), Value::Array(r)) => concatenate_array_values(l, r),
        // Element append: arr || elem
        (Value::Array(mut l), elem) => {
            l.push(elem);
            Ok(Value::Array(l))
        }
        // Element prepend: elem || arr
        (elem, Value::Array(mut r)) => {
            r.insert(0, elem);
            Ok(Value::Array(r))
        }
        _ => Err(DbError::internal(
            "|| on arrays requires at least one array operand",
        )),
    }
}

/// `a && b` for inet/cidr: returns `Some(true)` when both parse as inet AND
/// one network is contained in the other (or they're equal). Returns
/// `Some(false)` when both parse as inet but the relation does not hold.
/// Returns `None` when either side fails to parse - caller falls through to
/// geometry/range/array semantics.
fn inet_overlaps(left: &str, right: &str) -> Option<bool> {
    fn parse(text: &str) -> Option<(std::net::IpAddr, u8)> {
        let (addr_text, prefix_text) = text
            .split_once('/')
            .map_or((text, None), |(a, p)| (a, Some(p)));
        let ip = addr_text.parse::<std::net::IpAddr>().ok()?;
        let max_prefix = match ip {
            std::net::IpAddr::V4(_) => 32,
            std::net::IpAddr::V6(_) => 128,
        };
        let prefix = match prefix_text {
            Some(p) => p.parse::<u8>().ok().filter(|v| *v <= max_prefix)?,
            None => max_prefix,
        };
        Some((ip, prefix))
    }
    fn mask(addr: std::net::IpAddr, prefix: u8) -> std::net::IpAddr {
        match addr {
            std::net::IpAddr::V4(v4) => {
                let bits = u32::from_be_bytes(v4.octets());
                let m = if prefix == 0 {
                    0
                } else {
                    u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0)
                };
                std::net::IpAddr::V4((bits & m).into())
            }
            std::net::IpAddr::V6(v6) => {
                let bits = u128::from_be_bytes(v6.octets());
                let m = if prefix == 0 {
                    0
                } else {
                    u128::MAX.checked_shl(128 - u32::from(prefix)).unwrap_or(0)
                };
                std::net::IpAddr::V6((bits & m).into())
            }
        }
    }
    let (l_ip, l_prefix) = parse(left)?;
    let (r_ip, r_prefix) = parse(right)?;
    if std::mem::discriminant(&l_ip) != std::mem::discriminant(&r_ip) {
        return Some(false);
    }
    // Two networks overlap iff masking both with the smaller prefix yields
    // the same network identifier.
    let common_prefix = l_prefix.min(r_prefix);
    Some(mask(l_ip, common_prefix) == mask(r_ip, common_prefix))
}

#[cfg(test)]
mod inet_overlap_tests {
    use super::*;

    fn overlap(l: &str, r: &str) -> Value {
        eval_array_overlap_op(&Value::Text(l.to_owned()), &Value::Text(r.to_owned()))
            .expect("eval_array_overlap_op")
    }

    #[test]
    fn ipv4_overlapping_subnets_are_true() {
        // /24 contains /28 - they overlap.
        assert_eq!(
            overlap("192.168.1.0/24", "192.168.1.16/28"),
            Value::Boolean(true)
        );
    }

    #[test]
    fn ipv4_disjoint_subnets_are_false() {
        assert_eq!(
            overlap("192.168.1.0/24", "192.168.2.0/24"),
            Value::Boolean(false)
        );
    }

    #[test]
    fn ipv4_equal_subnets_overlap() {
        assert_eq!(
            overlap("192.168.1.0/24", "192.168.1.0/24"),
            Value::Boolean(true)
        );
    }

    #[test]
    fn ipv6_overlapping() {
        assert_eq!(
            overlap("2001:db8::/32", "2001:db8:abcd::/48"),
            Value::Boolean(true)
        );
    }

    #[test]
    fn cross_family_does_not_overlap() {
        assert_eq!(overlap("192.168.1.0/24", "::1/128"), Value::Boolean(false));
    }
}
