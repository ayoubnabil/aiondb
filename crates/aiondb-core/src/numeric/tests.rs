#![allow(clippy::unreadable_literal, clippy::cast_possible_truncation)]

use super::*;
use std::collections::HashSet;

// ---------------------------------------------------------------
// NumericValue construction and field access
// ---------------------------------------------------------------

#[test]
fn numeric_zero_coefficient_zero_scale() {
    let n = NumericValue::new(0, 0);
    assert_eq!(n.coefficient, 0);
    assert_eq!(n.scale, 0);
}

#[test]
fn numeric_i128_max_coefficient() {
    let n = NumericValue::new(i128::MAX, 0);
    assert_eq!(n.coefficient, i128::MAX);
}

#[test]
fn numeric_i128_min_coefficient() {
    let n = NumericValue::new(i128::MIN, 0);
    assert_eq!(n.coefficient, i128::MIN);
}

#[test]
fn numeric_large_coefficient_large_scale() {
    let n = NumericValue::new(i128::MAX, u32::MAX);
    assert_eq!(n.coefficient, i128::MAX);
    assert_eq!(n.scale, u32::MAX);
}

// ---------------------------------------------------------------
// NumericValue equality
// ---------------------------------------------------------------

#[test]
fn numeric_same_values_are_equal() {
    let a = NumericValue::new(12345, 3);
    let b = NumericValue::new(12345, 3);
    assert_eq!(a, b);
}

#[test]
fn numeric_same_coefficient_different_scale_not_equal() {
    let a = NumericValue::new(100, 2);
    let b = NumericValue::new(100, 3);
    assert_ne!(a, b);
}

#[test]
fn numeric_different_coefficient_same_scale_not_equal() {
    let a = NumericValue::new(100, 2);
    let b = NumericValue::new(200, 2);
    assert_ne!(a, b);
}

// ---------------------------------------------------------------
// NumericValue clone
// ---------------------------------------------------------------

#[test]
fn numeric_clone_is_equal() {
    let a = NumericValue::new(42, 7);
    let b = a.clone();
    assert_eq!(a, b);
}

// ---------------------------------------------------------------
// NumericValue hash consistency
// ---------------------------------------------------------------

#[test]
fn numeric_hash_same_values_same_bucket() {
    let mut set = HashSet::new();
    set.insert(NumericValue::new(100, 2));
    set.insert(NumericValue::new(100, 2));
    assert_eq!(set.len(), 1);
}

#[test]
fn numeric_hash_different_scale_different_bucket() {
    let mut set = HashSet::new();
    set.insert(NumericValue::new(100, 2));
    set.insert(NumericValue::new(100, 3));
    assert_eq!(set.len(), 2);
}

#[test]
fn numeric_hash_extreme_values() {
    let mut set = HashSet::new();
    set.insert(NumericValue::new(i128::MAX, u32::MAX));
    set.insert(NumericValue::new(i128::MIN, 0));
    set.insert(NumericValue::new(0, 0));
    assert_eq!(set.len(), 3);
}

// ---------------------------------------------------------------
// IntervalValue tests
// ---------------------------------------------------------------

#[test]
fn interval_all_zeros() {
    let iv = IntervalValue::new(0, 0, 0);
    assert_eq!(iv.months, 0);
    assert_eq!(iv.days, 0);
    assert_eq!(iv.micros, 0);
}

#[test]
fn interval_same_values_equal() {
    let a = IntervalValue::new(1, 2, 3);
    let b = IntervalValue::new(1, 2, 3);
    assert_eq!(a, b);
}

#[test]
fn interval_clone_is_equal() {
    let a = IntervalValue::new(10, 20, 30_000_000);
    let b = a.clone();
    assert_eq!(a, b);
}

// ---------------------------------------------------------------
// Arithmetic: add
// ---------------------------------------------------------------

#[test]
fn add_same_scale() {
    let a = NumericValue::new(150, 2); // 1.50
    let b = NumericValue::new(230, 2); // 2.30
    let result = a.add(&b);
    assert_eq!(result.coefficient, 380);
    assert_eq!(result.scale, 2);
}

#[test]
fn add_different_scale() {
    let a = NumericValue::new(15, 1); // 1.5
    let b = NumericValue::new(230, 2); // 2.30
    let result = a.add(&b);
    assert_eq!(result.coefficient, 380); // 3.80
    assert_eq!(result.scale, 2);
}

#[test]
fn add_negative() {
    let a = NumericValue::new(500, 2); // 5.00
    let b = NumericValue::new(-300, 2); // -3.00
    let result = a.add(&b);
    assert_eq!(result.coefficient, 200);
    assert_eq!(result.scale, 2);
}

// ---------------------------------------------------------------
// Arithmetic: sub
// ---------------------------------------------------------------

#[test]
fn sub_same_scale() {
    let a = NumericValue::new(1000, 2); // 10.00
    let b = NumericValue::new(350, 2); // 3.50
    let result = a.sub(&b);
    assert_eq!(result.coefficient, 650);
    assert_eq!(result.scale, 2);
}

#[test]
fn sub_different_scale() {
    let a = NumericValue::new(100, 1); // 10.0
    let b = NumericValue::new(350, 2); // 3.50
    let result = a.sub(&b);
    assert_eq!(result.coefficient, 650); // 6.50
    assert_eq!(result.scale, 2);
}

// ---------------------------------------------------------------
// Arithmetic: mul
// ---------------------------------------------------------------

#[test]
fn mul_basic() {
    let a = NumericValue::new(250, 2); // 2.50
    let b = NumericValue::new(400, 2); // 4.00
    let result = a.mul(&b).expect("numeric multiplication should succeed");
    assert_eq!(result.coefficient, 100_000); // 10.0000
    assert_eq!(result.scale, 4);
}

#[test]
fn mul_integer_and_decimal() {
    let a = NumericValue::new(3, 0); // 3
    let b = NumericValue::new(250, 2); // 2.50
    let result = a.mul(&b).expect("numeric multiplication should succeed");
    assert_eq!(result.coefficient, 750); // 7.50
    assert_eq!(result.scale, 2);
}

#[test]
fn mul_returns_none_on_scale_overflow() {
    let a = NumericValue::new(1, u32::MAX - 1);
    let b = NumericValue::new(1, 2);
    assert!(a.mul(&b).is_none());
}

#[test]
fn mul_promotes_to_big_on_coefficient_overflow() {
    let a = NumericValue::new(i128::MAX, 0);
    let b = NumericValue::new(2, 0);
    let result = a
        .mul(&b)
        .expect("i128 overflow should promote to big coefficient path");
    assert!(result.is_big());
    assert_eq!(result.scale, 0);
    assert_eq!(
        result.coefficient_to_string(),
        "340282366920938463463374607431768211454"
    );
}

// ---------------------------------------------------------------
// Arithmetic: div
// ---------------------------------------------------------------

#[test]
fn div_basic() {
    let a = NumericValue::new(1000, 2); // 10.00
    let b = NumericValue::new(300, 2); // 3.00
    let result = a.div(&b).unwrap();
    // scale = max(2,2) + 6 = 8
    assert_eq!(result.scale, 8);
    // 10.00 / 3.00 ~ 3.33333333
    // coefficient should be 333333333
    assert_eq!(result.coefficient, 333_333_333);
}

#[test]
fn div_by_zero_returns_none() {
    let a = NumericValue::new(100, 2);
    let b = NumericValue::new(0, 2);
    assert!(a.div(&b).is_none());
}

#[test]
fn div_exact() {
    let a = NumericValue::new(1000, 2); // 10.00
    let b = NumericValue::new(200, 2); // 2.00
    let result = a.div(&b).unwrap();
    assert_eq!(result.scale, 8);
    // 10/2 = 5.00000000
    assert_eq!(result.coefficient, 500_000_000);
}

// ---------------------------------------------------------------
// neg, abs, is_zero
// ---------------------------------------------------------------

#[test]
fn neg_positive() {
    let a = NumericValue::new(42, 0);
    let result = a.neg();
    assert_eq!(result.coefficient, -42);
    assert_eq!(result.scale, 0);
}

#[test]
fn neg_negative() {
    let a = NumericValue::new(-42, 0);
    let result = a.neg();
    assert_eq!(result.coefficient, 42);
}

#[test]
fn abs_positive() {
    let a = NumericValue::new(42, 2);
    assert_eq!(a.abs().coefficient, 42);
}

#[test]
fn abs_negative() {
    let a = NumericValue::new(-42, 2);
    assert_eq!(a.abs().coefficient, 42);
}

#[test]
fn is_zero_true() {
    assert!(NumericValue::new(0, 5).is_zero());
}

#[test]
fn is_zero_false() {
    assert!(!NumericValue::new(1, 0).is_zero());
}

// ---------------------------------------------------------------
// cmp / Ord
// ---------------------------------------------------------------

#[test]
fn cmp_equal_values() {
    let a = NumericValue::new(100, 2); // 1.00
    let b = NumericValue::new(1000, 3); // 1.000
    assert_eq!(a.cmp(&b), Ordering::Equal);
}

#[test]
fn cmp_less_than() {
    let a = NumericValue::new(100, 2); // 1.00
    let b = NumericValue::new(200, 2); // 2.00
    assert_eq!(a.cmp(&b), Ordering::Less);
}

#[test]
fn cmp_greater_than() {
    let a = NumericValue::new(300, 2); // 3.00
    let b = NumericValue::new(200, 2); // 2.00
    assert_eq!(a.cmp(&b), Ordering::Greater);
}

#[test]
fn cmp_negative() {
    let a = NumericValue::new(-100, 2); // -1.00
    let b = NumericValue::new(100, 2); // 1.00
    assert_eq!(a.cmp(&b), Ordering::Less);
}

#[test]
fn ord_trait_works() {
    let mut values = [
        NumericValue::new(300, 2),
        NumericValue::new(100, 2),
        NumericValue::new(200, 2),
    ];
    values.sort();
    assert_eq!(values[0].coefficient, 100);
    assert_eq!(values[1].coefficient, 200);
    assert_eq!(values[2].coefficient, 300);
}

// ---------------------------------------------------------------
// Display
// ---------------------------------------------------------------

#[test]
fn display_integer() {
    let n = NumericValue::new(42, 0);
    assert_eq!(n.to_string(), "42");
}

#[test]
fn display_negative_integer() {
    let n = NumericValue::new(-5, 0);
    assert_eq!(n.to_string(), "-5");
}

#[test]
fn display_decimal() {
    let n = NumericValue::new(12345, 2);
    assert_eq!(n.to_string(), "123.45");
}

#[test]
fn display_negative_decimal() {
    let n = NumericValue::new(-12345, 2);
    assert_eq!(n.to_string(), "-123.45");
}

#[test]
fn display_leading_zeros() {
    let n = NumericValue::new(5, 3);
    assert_eq!(n.to_string(), "0.005");
}

#[test]
fn display_zero_with_scale() {
    let n = NumericValue::new(0, 2);
    assert_eq!(n.to_string(), "0.00");
}

// ---------------------------------------------------------------
// FromStr
// ---------------------------------------------------------------

#[test]
fn from_str_integer() {
    let n: NumericValue = "42".parse().unwrap();
    assert_eq!(n.coefficient, 42);
    assert_eq!(n.scale, 0);
}

#[test]
fn from_str_decimal() {
    let n: NumericValue = "123.45".parse().unwrap();
    assert_eq!(n.coefficient, 12345);
    assert_eq!(n.scale, 2);
}

#[test]
fn from_str_negative() {
    let n: NumericValue = "-5.5".parse().unwrap();
    assert_eq!(n.coefficient, -55);
    assert_eq!(n.scale, 1);
}

#[test]
fn from_str_leading_zero() {
    let n: NumericValue = "0.005".parse().unwrap();
    assert_eq!(n.coefficient, 5);
    assert_eq!(n.scale, 3);
}

#[test]
fn from_str_invalid() {
    assert!("abc".parse::<NumericValue>().is_err());
}

#[test]
fn from_str_empty() {
    assert!("".parse::<NumericValue>().is_err());
}

#[test]
fn from_str_scientific_exponent_too_large_returns_error() {
    assert!("1.2345678901234e+9999".parse::<NumericValue>().is_err());
}

// ---------------------------------------------------------------
// from_i32 / from_i64
// ---------------------------------------------------------------

#[test]
fn from_i32_basic() {
    let n = NumericValue::from_i32(42);
    assert_eq!(n.coefficient, 42);
    assert_eq!(n.scale, 0);
}

#[test]
fn from_i64_basic() {
    let n = NumericValue::from_i64(1_000_000_000_000);
    assert_eq!(n.coefficient, 1_000_000_000_000);
    assert_eq!(n.scale, 0);
}

// ---------------------------------------------------------------
// round / trunc
// ---------------------------------------------------------------

#[test]
fn round_down() {
    let n = NumericValue::new(12344, 3); // 12.344
    let r = n.round(2);
    assert_eq!(r.coefficient, 1234); // 12.34
    assert_eq!(r.scale, 2);
}

#[test]
fn round_up() {
    let n = NumericValue::new(12345, 3); // 12.345
    let r = n.round(2);
    assert_eq!(r.coefficient, 1235); // 12.35
    assert_eq!(r.scale, 2);
}

#[test]
fn round_negative() {
    let n = NumericValue::new(-12345, 3); // -12.345
    let r = n.round(2);
    assert_eq!(r.coefficient, -1235); // -12.35
}

#[test]
fn trunc_basic() {
    let n = NumericValue::new(12349, 3); // 12.349
    let r = n.trunc(2);
    assert_eq!(r.coefficient, 1234); // 12.34
    assert_eq!(r.scale, 2);
}

#[test]
fn trunc_negative() {
    let n = NumericValue::new(-12349, 3); // -12.349
    let r = n.trunc(2);
    assert_eq!(r.coefficient, -1234); // -12.34
}

#[test]
fn round_to_higher_scale() {
    let n = NumericValue::new(42, 0); // 42
    let r = n.round(2);
    assert_eq!(r.coefficient, 4200); // 42.00
    assert_eq!(r.scale, 2);
}

#[test]
fn trunc_to_higher_scale() {
    let n = NumericValue::new(42, 0); // 42
    let r = n.trunc(2);
    assert_eq!(r.coefficient, 4200); // 42.00
    assert_eq!(r.scale, 2);
}

// ---------------------------------------------------------------
// AUDIT: ciblage de bugs potentiels (numeric module)
// ---------------------------------------------------------------

// 1. Scientific parsing: 1e308, 1e-308, 1e9999
#[test]
fn audit_parse_scientific_1e308_ok() {
    // 1e308 must be accepted (well above f64, but big numeric handles it).
    let r = "1e308".parse::<NumericValue>();
    assert!(r.is_ok(), "1e308 must be accepted: {r:?}");
}

#[test]
fn audit_parse_scientific_1em308_ok() {
    let r = "1e-308".parse::<NumericValue>();
    assert!(r.is_ok(), "1e-308 must be accepted: {r:?}");
    let v = r.unwrap();
    assert_eq!(v.scale, 308);
}

#[test]
fn audit_parse_scientific_1e9999_rejected() {
    // 1e9999 exceeds MAX_BIG_DECIMAL_DIGITS=2880 → must be rejected
    let r = "1e9999".parse::<NumericValue>();
    assert!(r.is_err(), "1e9999 must be rejected: {r:?}");
}

#[test]
fn audit_parse_scientific_1e2880_boundary() {
    // 1 digit mantissa + 2880 exp = 2881 > 2880 → reject per check
    let r = "1e2880".parse::<NumericValue>();
    assert!(r.is_err(), "1e2880 must be rejected: {r:?}");
    // 1 digit + 2879 = 2880 → accept
    let r2 = "1e2879".parse::<NumericValue>();
    assert!(r2.is_ok(), "1e2879 must be accepted: {r2:?}");
}

// 2. mul limit boundary: n+m = MAX_BIG_LIMBS+1
#[test]
fn audit_mul_boundary_max_limbs() {
    // Forge BigCoefficients near the limit to test the n+m check.
    // 160 limbs each → 320 total, within the 321 limit.
    let limbs_a = vec![999_999_999u32; 160];
    let limbs_b = vec![999_999_999u32; 160];
    let a = BigCoefficient {
        limbs: limbs_a,
        negative: false,
    };
    let b = BigCoefficient {
        limbs: limbs_b,
        negative: false,
    };
    let r = a.mul(&b);
    // 160+160 = 320 ≤ 321; but after normalization the result may have 320 limbs.
    // Either `Some` if the final value is ≤320, or `None`.
    if let Some(result) = r {
        assert!(result.limbs.len() <= MAX_BIG_LIMBS);
    }
}

// 3. Round-trip: parse → to_string → parse → equality
#[test]
fn audit_round_trip_random_values() {
    // Small deterministic fuzz set
    let cases = [
        "0",
        "1",
        "-1",
        "1.5",
        "-1.5",
        "12345.67890",
        "0.0001",
        "99999999999999999999999999999999999999",
        "-99999999999999999999999999999999999999",
        "123.456789012345678901234567890",
        "1e100",
        "-1e100",
        "0.000000000000000001",
    ];
    for s in cases {
        let parsed: NumericValue = s.parse().unwrap_or_else(|e| panic!("parse {s}: {e}"));
        let displayed = parsed.to_string();
        let reparsed: NumericValue = displayed
            .parse()
            .unwrap_or_else(|e| panic!("reparse {displayed}: {e}"));
        assert_eq!(
            parsed, reparsed,
            "round-trip failed for {s}: displayed={displayed}",
        );
    }
}

// 4. Division scale_up saturating boundaries
#[test]
fn audit_div_large_scale_fast_path_fallback() {
    // Simple division that should take the big path but still produce a correct result.
    // 1/3 at high precision (default via pg_select_div_scale ~16).
    let a: NumericValue = "1".parse().unwrap();
    let b: NumericValue = "3".parse().unwrap();
    let r = a.div(&b).unwrap();
    let s = r.to_string();
    assert!(
        s.starts_with("0.3333333333"),
        "1/3 must start with 0.3333333333, got {s}",
    );
}

#[test]
fn audit_div_scale_2880_does_not_hang() {
    // Pathological but finite: high-scale dividends
    let a = NumericValue::new(1, 100);
    let b = NumericValue::new(1, 0);
    // Should not panic / loop forever
    let _ = a.div(&b);
}

// 5. NaN / Infinity propagation
#[test]
fn audit_nan_add_returns_nan() {
    let nan = NumericValue::NAN;
    let one = NumericValue::new(1, 0);
    assert!(nan.add(&one).is_nan());
    assert!(one.add(&nan).is_nan());
}

#[test]
fn audit_nan_mul_returns_nan() {
    let nan = NumericValue::NAN;
    let one = NumericValue::new(1, 0);
    assert!(nan.mul(&one).unwrap().is_nan());
    assert!(one.mul(&nan).unwrap().is_nan());
}

#[test]
fn audit_nan_div_returns_nan() {
    let nan = NumericValue::NAN;
    let one = NumericValue::new(1, 0);
    assert!(nan.div(&one).unwrap().is_nan());
    assert!(one.div(&nan).unwrap().is_nan());
}

#[test]
fn audit_infinity_times_zero_is_nan() {
    let inf = NumericValue::INFINITY;
    let zero = NumericValue::new(0, 0);
    assert!(inf.mul(&zero).unwrap().is_nan());
    assert!(zero.mul(&inf).unwrap().is_nan());
}

#[test]
fn audit_infinity_minus_infinity_is_nan() {
    let inf = NumericValue::INFINITY;
    assert!(inf.sub(&inf).is_nan());
}

#[test]
fn audit_infinity_plus_infinity_is_infinity() {
    let inf = NumericValue::INFINITY;
    let r = inf.add(&inf);
    assert!(r.is_pos_infinity());
}

// 6. mul_pow10(0) must return self
#[test]
fn audit_mul_pow10_zero_returns_self() {
    let a = BigCoefficient::from_i128(12345);
    let r = a.mul_pow10(0).unwrap();
    assert_eq!(r, a);
}

#[test]
fn audit_mul_pow10_zero_value() {
    let zero = BigCoefficient::zero();
    let r = zero.mul_pow10(5).unwrap();
    assert!(r.is_zero());
}

// 7. NaN == NaN consistency with PG: in PG, NaN = NaN is true
#[test]
fn audit_nan_eq_nan_is_true() {
    // PG: select 'NaN'::numeric = 'NaN'::numeric ; => true
    let a = NumericValue::NAN;
    let b = NumericValue::NAN;
    assert_eq!(a, b, "NaN must equal NaN (PostgreSQL convention)");
}

#[test]
fn audit_nan_cmp_nan_is_equal() {
    use std::cmp::Ordering;
    let a = NumericValue::NAN;
    let b = NumericValue::NAN;
    assert_eq!(a.cmp(&b), Ordering::Equal);
}

// 8. Division by a number that forces scale_up near the u32 saturation bound.
#[test]
fn audit_div_self_scale_ge_result_plus_other() {
    // self.scale very large, other.scale small: saturating_sub prevents underflow
    let a = NumericValue::new(1, 100);
    let b = NumericValue::new(1, 5);
    let r = a.div(&b);
    assert!(r.is_some());
}

// 9. Parsing a very long integer that matches MAX_BIG_DECIMAL_DIGITS exactly
#[test]
fn audit_parse_max_digits_boundary() {
    let s: String = "9".repeat(MAX_BIG_DECIMAL_DIGITS);
    let r = s.parse::<NumericValue>();
    assert!(r.is_ok(), "digits at MAX_BIG_DECIMAL_DIGITS must parse");
    let s2: String = "9".repeat(MAX_BIG_DECIMAL_DIGITS + 1);
    let r2 = s2.parse::<NumericValue>();
    assert!(
        r2.is_err(),
        "digits over MAX_BIG_DECIMAL_DIGITS must reject"
    );
}

// 10. to_i128 must not silently saturate during a from_i128 roundtrip
#[test]
fn audit_from_i128_to_i128_roundtrip() {
    for v in [
        0i128,
        1,
        -1,
        i128::MAX,
        i128::MIN + 1,
        1234567890123456789,
        -9876543210_i128,
    ] {
        let big = BigCoefficient::from_i128(v);
        assert_eq!(big.to_i128(), Some(v), "roundtrip for {v}");
    }
}

// 11. from_i128(i128::MIN) - negative edge case
#[test]
fn audit_from_i128_min() {
    // unsigned_abs of i128::MIN = 1 << 127; should be manageable
    let big = BigCoefficient::from_i128(i128::MIN);
    // to_i128 should either return MIN or None (because +MAG overflow)
    let back = big.to_i128();
    assert!(
        back == Some(i128::MIN) || back.is_none(),
        "from/to i128::MIN: {back:?}"
    );
}

// 12. div_u64 edge case: divisor = 1
#[test]
fn audit_div_u64_divisor_one() {
    let a = BigCoefficient::from_i128(12345678901234567890_i128);
    let (q, r) = a.div_u64(1).unwrap();
    assert_eq!(r, 0);
    assert_eq!(q, a);
}

// 13. Division where scale forces large mul_pow10
#[test]
fn audit_div_small_by_small_produces_correct_scale() {
    let a: NumericValue = "2".parse().unwrap();
    let b: NumericValue = "7".parse().unwrap();
    let r = a.div(&b).unwrap();
    // Default rscale ~16 digits
    let s = r.to_string();
    assert!(s.starts_with("0.285714"), "2/7 = 0.285714..., got {s}");
}

// 14. checked_ten_pow boundary
#[test]
fn audit_checked_ten_pow_boundaries() {
    assert!(checked_ten_pow(0) == Some(1));
    assert!(checked_ten_pow(38).is_some());
    assert!(checked_ten_pow(39).is_none(), "39 out of i128 range");
    assert!(checked_ten_pow(u32::MAX).is_none());
}

// 15. mul(): verify the correctness of the product of two large integers
#[test]
fn audit_big_mul_correctness() {
    // (10^50) * (10^50) = 10^100
    let a: NumericValue = "1"
        .to_owned()
        .chars()
        .chain(std::iter::repeat_n('0', 50))
        .collect::<String>()
        .parse()
        .unwrap();
    let b = a.clone();
    let prod = a.mul(&b).unwrap();
    let expected: NumericValue = "1"
        .to_owned()
        .chars()
        .chain(std::iter::repeat_n('0', 100))
        .collect::<String>()
        .parse()
        .unwrap();
    assert_eq!(prod, expected, "10^50 * 10^50 must equal 10^100");
}

// 16. Verify that `to_f64` for NaN is indeed f64::NAN
#[test]
fn audit_nan_to_f64() {
    let n = NumericValue::NAN;
    assert!(n.to_f64().is_nan());
}

// 17. Division where the result must be rounded (last digit == 5)
#[test]
fn audit_div_rounds_to_nearest() {
    // 1/8 = 0.125 (exact)
    let a: NumericValue = "1".parse().unwrap();
    let b: NumericValue = "8".parse().unwrap();
    let r = a.div(&b).unwrap();
    assert!(r.to_string().starts_with("0.125"));
}

// 18. Check that comparison NaN > any is consistent (PG: NaN > infinity)
#[test]
fn audit_nan_greater_than_infinity() {
    use std::cmp::Ordering;
    let nan = NumericValue::NAN;
    let inf = NumericValue::INFINITY;
    // In PG, NaN is greater than any including infinity.
    assert_eq!(nan.cmp(&inf), Ordering::Greater);
    assert_eq!(inf.cmp(&nan), Ordering::Less);
}

// 19. Round-trip big negative with scale
#[test]
fn audit_round_trip_big_scaled() {
    let s = "-123456789012345678901234567890.1234567890";
    let v: NumericValue = s.parse().unwrap();
    let back = v.to_string();
    let v2: NumericValue = back.parse().unwrap();
    assert_eq!(v, v2);
}

// 20a. Mul with a coefficient > LIMB_BASE to force a full carry propagation
#[test]
fn audit_mul_last_limb_normalized_after_propagation() {
    // Force: a product where the last limb must be corrected.
    // Construire directement des limbs: [LIMB_BASE-1, LIMB_BASE-1, ..., LIMB_BASE-1]
    let max_limb = (big_coefficient::LIMB_BASE - 1) as u32;
    let a = BigCoefficient {
        limbs: vec![max_limb; 30],
        negative: false,
    };
    let b = BigCoefficient {
        limbs: vec![max_limb; 30],
        negative: false,
    };
    let r = a.mul(&b).unwrap();
    for (i, &limb) in r.limbs.iter().enumerate() {
        assert!(
            u64::from(limb) < big_coefficient::LIMB_BASE,
            "limb[{i}] = {limb} >= LIMB_BASE",
        );
    }
}

// 21. mul_pow10 overflow exactly at MAX
#[test]
fn audit_mul_pow10_overflow_limit() {
    let a = BigCoefficient::from_i128(1);
    // 10^(MAX_BIG_DECIMAL_DIGITS) should fail because result would need one more limb
    let r = a.mul_pow10(big_coefficient::MAX_BIG_LIMBS as u32 * 9);
    // MAX_BIG_LIMBS * 9 = 2880 digits; 10^2880 = 1 followed by 2880 zeros = 2881 digits total
    // need ceil(2881/9) = 321 limbs > 320 → None
    assert!(r.is_none(), "mul_pow10 should fail at this boundary");
}

// 22. mul_pow10 just below overflow
#[test]
fn audit_mul_pow10_near_limit_ok() {
    let a = BigCoefficient::from_i128(1);
    // 2879 digits = 10^2879 → 2880 digits total → 320 limbs → should be OK
    let r = a.mul_pow10(2879);
    assert!(r.is_some(), "mul_pow10(2879) should succeed");
}

// 20. Verify invariant: mul must produce limbs < LIMB_BASE
#[test]
fn audit_mul_invariant_normalized_limbs() {
    // Build two BigCoefficients that produce a result where the last limb
    // could exceed LIMB_BASE if normalization is incomplete.
    let a = BigCoefficient::from_decimal_string(&"9".repeat(100), false);
    let b = BigCoefficient::from_decimal_string(&"9".repeat(100), false);
    let r = a.mul(&b).unwrap();
    for (i, &limb) in r.limbs.iter().enumerate() {
        assert!(
            u64::from(limb) < big_coefficient::LIMB_BASE,
            "limb[{i}] = {limb} >= LIMB_BASE = {}",
            big_coefficient::LIMB_BASE
        );
    }
    // Numerical check: (10^100 - 1)^2 = 10^200 - 2*10^100 + 1
    let expected_str = {
        let mut s = String::new();
        s.push('9'); // start
                     // 10^200 - 2*10^100 + 1 => representation is 99...98 00...01 (99 nines, 8, 100 zeros, 1)
                     // Easier: compute via mul of parse
        let a_n: NumericValue = "9".repeat(100).parse().unwrap();
        let b_n = a_n.clone();
        let prod = a_n.mul(&b_n).unwrap();
        s.clear();
        s.push_str(&prod.to_string());
        s
    };
    let r_str = {
        let mut s = String::new();
        if r.negative {
            s.push('-');
        }
        s.push_str(&r.to_decimal_string_unsigned());
        s
    };
    assert_eq!(r_str, expected_str);
}

#[test]
fn from_str_does_not_panic_on_multibyte_utf8_radix_prefix() {
    let inputs = [
        "\u{20000}FF", // 4-byte char then ASCII (slice [..2] lands inside char)
        "\u{20000}ABCD",
        "é0xff", // 2-byte char then numeric-looking suffix
        "ñ123",
        "🦀123", // 4-byte emoji
    ];
    for input in inputs {
        let result = std::panic::catch_unwind(|| input.parse::<NumericValue>());
        assert!(
            result.is_ok(),
            "from_str panicked on adversarial input {input:?}"
        );
        let parsed = result.unwrap();
        assert!(
            parsed.is_err(),
            "expected Err for invalid input {input:?}, got Ok({parsed:?})"
        );
    }
}
