use aiondb_core::{DbError, DbResult, Value};

use super::math::{c_erf, c_erfc};
use super::value_convert::{f64_to_i64, i64_to_f64};
use super::{expect_at_least_args, to_f64};

/// Apply a single-argument f64→f64 function, wrapping the result in `Value::Double`.
fn unary_f64(args: &[Value], f: impl FnOnce(f64) -> f64) -> DbResult<Value> {
    let x = to_f64(&args[0])?;
    Ok(Value::Double(f(x)))
}

pub(crate) fn eval_trig(name: &str, args: &[Value]) -> DbResult<Value> {
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    match name {
        "sin" => unary_f64(args, f64::sin),
        "cos" => unary_f64(args, f64::cos),
        "tan" => unary_f64(args, f64::tan),
        "asin" => unary_f64(args, f64::asin),
        "acos" => unary_f64(args, f64::acos),
        "atan" => unary_f64(args, f64::atan),
        "sinh" => unary_f64(args, f64::sinh),
        "cosh" => unary_f64(args, f64::cosh),
        "tanh" => unary_f64(args, f64::tanh),
        "asinh" => unary_f64(args, f64::asinh),
        "sind" => unary_f64(args, sind_pg),
        "cosd" => unary_f64(args, cosd_pg),
        "tand" => unary_f64(args, tand_pg),
        "asind" => unary_f64(args, asind_pg),
        "acosd" => unary_f64(args, acosd_pg),
        "atand" => unary_f64(args, |x| x.atan().to_degrees()),
        "cotd" => unary_f64(args, cotd_pg),
        "degrees" => unary_f64(args, f64::to_degrees),
        "radians" => unary_f64(args, f64::to_radians),
        "cbrt" => unary_f64(args, f64::cbrt),
        "erf" => unary_f64(args, c_erf),
        "erfc" => unary_f64(args, c_erfc),
        "acosh" => {
            let f = to_f64(&args[0])?;
            if f < 1.0 {
                return Err(DbError::internal("input is out of range"));
            }
            Ok(Value::Double(f.acosh()))
        }
        "atanh" => {
            let f = to_f64(&args[0])?;
            if f <= -1.0 || f >= 1.0 {
                return Err(DbError::internal("input is out of range"));
            }
            Ok(Value::Double(f.atanh()))
        }
        "atan2" => {
            expect_at_least_args(args, 2, "atan2()")?;
            let y = to_f64(&args[0])?;
            let x = to_f64(&args[1])?;
            Ok(Value::Double(y.atan2(x)))
        }
        "atan2d" => {
            expect_at_least_args(args, 2, "atan2d()")?;
            let y = to_f64(&args[0])?;
            let x = to_f64(&args[1])?;
            Ok(Value::Double(atan2d_pg(y, x)))
        }
        "cot" => {
            let f = to_f64(&args[0])?;
            let s = f.sin();
            if s == 0.0 {
                return Err(DbError::internal("input is out of range"));
            }
            Ok(Value::Double(f.cos() / s))
        }
        _ => Err(DbError::internal(format!("unknown trig function: {name}"))),
    }
}

// =====================================================================
// PostgreSQL-compatible degree trig functions with exact special values
// =====================================================================

/// Normalize an angle in degrees to `[0, 360)`, returning `NAN` for
/// non-finite inputs.  Shared by `sind_pg`, `cosd_pg`, `tand_pg`, `cotd_pg`.
fn normalize_degrees(x: f64) -> Option<f64> {
    if x.is_nan() || x.is_infinite() {
        return None;
    }
    let mut a = x % 360.0;
    if a < 0.0 {
        a += 360.0;
    }
    Some(a)
}

fn trunc_f64_to_i64(value: f64) -> Option<i64> {
    if !value.is_finite() {
        return None;
    }
    let truncated = value.trunc();
    if truncated < i64_to_f64(i64::MIN) || truncated >= i64_to_f64(i64::MAX) {
        return None;
    }
    f64_to_i64(truncated).ok()
}

/// `PostgreSQL` `sind()` - sine of angle in degrees with exact special-case values
fn sind_pg(x: f64) -> f64 {
    let Some(a) = normalize_degrees(x) else {
        return f64::NAN;
    };
    let Some(a_int) = trunc_f64_to_i64(a) else {
        return x.to_radians().sin();
    };
    match a_int {
        0 | 180 | 360 => 0.0,
        30 => 0.5,
        90 => 1.0,
        150 => 0.5,
        210 => -0.5,
        270 => -1.0,
        330 => -0.5,
        _ => x.to_radians().sin(),
    }
}

/// `PostgreSQL` `cosd()` - cosine of angle in degrees with exact special-case values
fn cosd_pg(x: f64) -> f64 {
    let Some(a) = normalize_degrees(x) else {
        return f64::NAN;
    };
    let Some(a_int) = trunc_f64_to_i64(a) else {
        return x.to_radians().cos();
    };
    match a_int {
        0 | 360 => 1.0,
        60 => 0.5,
        90 | 270 => 0.0,
        120 => -0.5,
        180 => -1.0,
        240 => -0.5,
        300 => 0.5,
        _ => x.to_radians().cos(),
    }
}

/// `PostgreSQL` `asind()` - arc sine returning degrees with exact special-case values
fn asind_pg(x: f64) -> f64 {
    if x == 0.0 {
        return 0.0;
    }
    if x == 1.0 {
        return 90.0;
    }
    if x == -1.0 {
        return -90.0;
    }
    if x == 0.5 {
        return 30.0;
    }
    if x == -0.5 {
        return -30.0;
    }
    x.asin().to_degrees()
}

/// `PostgreSQL` `acosd()` - arc cosine returning degrees with exact special-case values
fn acosd_pg(x: f64) -> f64 {
    if x == 0.0 {
        return 90.0;
    }
    if x == 1.0 {
        return 0.0;
    }
    if x == -1.0 {
        return 180.0;
    }
    if x == 0.5 {
        return 60.0;
    }
    if x == -0.5 {
        return 120.0;
    }
    x.acos().to_degrees()
}

/// `PostgreSQL` `atan2d()` - two-argument arc tangent returning degrees with exact values
fn atan2d_pg(y: f64, x: f64) -> f64 {
    if y == 0.0 && x > 0.0 {
        return 0.0;
    }
    if y == 0.0 && x < 0.0 {
        return 180.0;
    }
    if y > 0.0 && x == 0.0 {
        return 90.0;
    }
    if y < 0.0 && x == 0.0 {
        return -90.0;
    }
    y.atan2(x).to_degrees()
}

/// Helper for `tand_pg` and `cotd_pg`: check exact special angles from a
/// normalized degree value.  Returns `Some(exact)` when hit.
fn exact_angle(a: f64, table: &[(i64, f64)]) -> Option<f64> {
    let a_int = trunc_f64_to_i64(a)?;
    if (a - i64_to_f64(a_int)).abs() < 1e-9 {
        let rem = a_int % 360;
        for &(angle, val) in table {
            if rem == angle {
                return Some(val);
            }
        }
    }
    None
}

/// Compute sin/cos ratio with infinity handling for zero denominator.
fn sincos_ratio(numer: f64, denom: f64) -> f64 {
    if denom == 0.0 {
        if numer < 0.0 {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        }
    } else {
        numer / denom
    }
}

/// `PostgreSQL` `tand()` - tangent of angle in degrees with exact special-case values.
fn tand_pg(x: f64) -> f64 {
    let Some(a) = normalize_degrees(x) else {
        return f64::NAN;
    };
    static TABLE: &[(i64, f64)] = &[
        (0, 0.0),
        (180, 0.0),
        (45, 1.0),
        (225, 1.0),
        (135, -1.0),
        (315, -1.0),
        (90, f64::INFINITY),
        (270, f64::NEG_INFINITY),
    ];
    if let Some(v) = exact_angle(a, TABLE) {
        return v;
    }
    sincos_ratio(sind_pg(x), cosd_pg(x))
}

/// `PostgreSQL` `cotd()` - cotangent of angle in degrees with exact special-case values.
fn cotd_pg(x: f64) -> f64 {
    let Some(a) = normalize_degrees(x) else {
        return f64::NAN;
    };
    static TABLE: &[(i64, f64)] = &[
        (90, 0.0),
        (270, 0.0),
        (45, 1.0),
        (225, 1.0),
        (135, -1.0),
        (315, -1.0),
        (0, f64::INFINITY),
        (180, f64::NEG_INFINITY),
    ];
    if let Some(v) = exact_angle(a, TABLE) {
        return v;
    }
    sincos_ratio(cosd_pg(x), sind_pg(x))
}

// =====================================================================
// Additional math-related Generic functions
// =====================================================================
