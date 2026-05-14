use super::*;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

fn compute_hash(key: &ValueHashKey) -> u64 {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish()
}

// =====================================================================
// NEW: canonical_f32 subnormal numbers
// =====================================================================

#[test]
fn canonical_f32_smallest_subnormal() {
    let subnormal = f32::from_bits(1);
    assert!(subnormal > 0.0);
    assert!(subnormal.is_finite());
    assert_eq!(canonical_f32(subnormal), subnormal.to_bits());
}

#[test]
fn canonical_f32_largest_subnormal() {
    let subnormal = f32::from_bits(0x007FFFFF);
    assert!(subnormal > 0.0);
    assert!(subnormal.is_finite());
    assert_eq!(canonical_f32(subnormal), subnormal.to_bits());
}

#[test]
fn canonical_f32_negative_subnormal() {
    let subnormal = f32::from_bits(0x80000001);
    assert!(subnormal < 0.0);
    assert!(subnormal.is_finite());
    assert_eq!(canonical_f32(subnormal), subnormal.to_bits());
}

#[test]
fn canonical_f32_subnormal_not_treated_as_zero() {
    let subnormal = f32::from_bits(1);
    let zero_bits = canonical_f32(0.0f32);
    let sub_bits = canonical_f32(subnormal);
    assert_ne!(zero_bits, sub_bits);
}

// =====================================================================
// NEW: canonical_f64 subnormal numbers
// =====================================================================

#[test]
fn canonical_f64_smallest_subnormal() {
    let subnormal = f64::from_bits(1);
    assert!(subnormal > 0.0);
    assert!(subnormal.is_finite());
    assert_eq!(canonical_f64(subnormal), subnormal.to_bits());
}

#[test]
fn canonical_f64_largest_subnormal() {
    let subnormal = f64::from_bits(0x000FFFFFFFFFFFFF);
    assert!(subnormal > 0.0);
    assert!(subnormal.is_finite());
    assert_eq!(canonical_f64(subnormal), subnormal.to_bits());
}

#[test]
fn canonical_f64_negative_subnormal() {
    let subnormal = f64::from_bits(0x8000000000000001);
    assert!(subnormal < 0.0);
    assert!(subnormal.is_finite());
    assert_eq!(canonical_f64(subnormal), subnormal.to_bits());
}

#[test]
fn canonical_f64_subnormal_not_treated_as_zero() {
    let subnormal = f64::from_bits(1);
    let zero_bits = canonical_f64(0.0f64);
    let sub_bits = canonical_f64(subnormal);
    assert_ne!(zero_bits, sub_bits);
}

// =====================================================================
// NEW: f32 signaling NaN variants
// =====================================================================

#[test]
fn canonical_f32_signaling_nan() {
    let snan = f32::from_bits(0x7F800001);
    assert!(snan.is_nan());
    assert_eq!(canonical_f32(snan), f32::NAN.to_bits());
}

#[test]
fn canonical_f64_signaling_nan() {
    let snan = f64::from_bits(0x7FF0000000000001);
    assert!(snan.is_nan());
    assert_eq!(canonical_f64(snan), f64::NAN.to_bits());
}

#[test]
fn real_signaling_nan_same_key_as_quiet_nan() {
    let snan = f32::from_bits(0x7F800001);
    let qnan = f32::NAN;
    let k1 = build_hash_key(&Value::Real(snan)).unwrap();
    let k2 = build_hash_key(&Value::Real(qnan)).unwrap();
    assert_eq!(k1, k2);
}

#[test]
fn double_signaling_nan_same_key_as_quiet_nan() {
    let snan = f64::from_bits(0x7FF0000000000001);
    let qnan = f64::NAN;
    let k1 = build_hash_key(&Value::Double(snan)).unwrap();
    let k2 = build_hash_key(&Value::Double(qnan)).unwrap();
    assert_eq!(k1, k2);
}

// =====================================================================
// NEW: Hash key for subnormal float values
// =====================================================================

#[test]
fn hash_key_real_subnormal() {
    let subnormal = f32::from_bits(1);
    let key = build_hash_key(&Value::Real(subnormal)).unwrap();
    assert_eq!(key, ValueHashKey::Real(subnormal.to_bits()));
}

#[test]
fn hash_key_double_subnormal() {
    let subnormal = f64::from_bits(1);
    let key = build_hash_key(&Value::Double(subnormal)).unwrap();
    assert_eq!(key, ValueHashKey::Double(subnormal.to_bits()));
}

#[test]
fn hash_key_real_subnormal_different_from_zero() {
    let k_sub = build_hash_key(&Value::Real(f32::from_bits(1))).unwrap();
    let k_zero = build_hash_key(&Value::Real(0.0)).unwrap();
    assert_ne!(k_sub, k_zero);
}

#[test]
fn hash_key_double_subnormal_different_from_zero() {
    let k_sub = build_hash_key(&Value::Double(f64::from_bits(1))).unwrap();
    let k_zero = build_hash_key(&Value::Double(0.0)).unwrap();
    assert_ne!(k_sub, k_zero);
}

// =====================================================================
// NEW: Collision resistance across value types with same-ish values
// =====================================================================

#[test]
fn null_ne_boolean_false_key() {
    let k1 = build_hash_key(&Value::Null).unwrap();
    let k2 = build_hash_key(&Value::Boolean(false)).unwrap();
    assert_ne!(k1, k2);
}

#[test]
fn null_ne_text_empty_key() {
    let k1 = build_hash_key(&Value::Null).unwrap();
    let k2 = build_hash_key(&Value::Text(String::new())).unwrap();
    assert_ne!(k1, k2);
}

#[test]
fn null_ne_blob_empty_key() {
    let k1 = build_hash_key(&Value::Null).unwrap();
    let k2 = build_hash_key(&Value::Blob(vec![])).unwrap();
    assert_ne!(k1, k2);
}

#[test]
fn int_0_ne_bigint_0_key() {
    let k1 = build_hash_key(&Value::Int(0)).unwrap();
    let k2 = build_hash_key(&Value::BigInt(0)).unwrap();
    assert_ne!(k1, k2);
}

#[test]
fn real_0_ne_double_0_key() {
    let k1 = build_hash_key(&Value::Real(0.0)).unwrap();
    let k2 = build_hash_key(&Value::Double(0.0)).unwrap();
    assert_ne!(k1, k2);
}

#[test]
fn text_0_ne_int_0_key() {
    let k1 = build_hash_key(&Value::Text("0".into())).unwrap();
    let k2 = build_hash_key(&Value::Int(0)).unwrap();
    assert_ne!(k1, k2);
}

#[test]
fn boolean_true_ne_int_1_key() {
    let k1 = build_hash_key(&Value::Boolean(true)).unwrap();
    let k2 = build_hash_key(&Value::Int(1)).unwrap();
    assert_ne!(k1, k2);
}

#[test]
fn boolean_false_ne_int_0_key() {
    let k1 = build_hash_key(&Value::Boolean(false)).unwrap();
    let k2 = build_hash_key(&Value::Int(0)).unwrap();
    assert_ne!(k1, k2);
}

#[test]
fn text_hello_ne_blob_hello_key() {
    let k1 = build_hash_key(&Value::Text("hello".into())).unwrap();
    let k2 = build_hash_key(&Value::Blob(b"hello".to_vec())).unwrap();
    assert_ne!(k1, k2);
}

// =====================================================================
// NEW: Hash key for extreme numeric values
// =====================================================================

#[test]
fn hash_key_numeric_i128_max() {
    let nv = NumericValue::new(i128::MAX, 0);
    let key = build_hash_key(&Value::Numeric(nv.clone())).unwrap();
    assert_eq!(key, ValueHashKey::Numeric(nv));
}

#[test]
fn hash_key_numeric_i128_min() {
    let nv = NumericValue::new(i128::MIN, 0);
    let key = build_hash_key(&Value::Numeric(nv.clone())).unwrap();
    assert_eq!(key, ValueHashKey::Numeric(nv));
}

#[test]
fn hash_key_numeric_max_scale() {
    let nv = NumericValue::new(1, u32::MAX);
    let key = build_hash_key(&Value::Numeric(nv.clone())).unwrap();
    assert_eq!(key, ValueHashKey::Numeric(nv));
}

#[test]
fn hash_key_numeric_i128_max_different_from_i128_min() {
    let k1 = build_hash_key(&Value::Numeric(NumericValue::new(i128::MAX, 0))).unwrap();
    let k2 = build_hash_key(&Value::Numeric(NumericValue::new(i128::MIN, 0))).unwrap();
    assert_ne!(k1, k2);
}

// =====================================================================
// NEW: Hash key for extreme interval values
// =====================================================================

#[test]
fn hash_key_interval_extreme_max() {
    let iv = IntervalValue::new(i32::MAX, i32::MAX, i64::MAX);
    let key = build_hash_key(&Value::Interval(iv.clone())).unwrap();
    assert_eq!(key, ValueHashKey::Interval(iv));
}

#[test]
fn hash_key_interval_extreme_min() {
    let iv = IntervalValue::new(i32::MIN, i32::MIN, i64::MIN);
    let key = build_hash_key(&Value::Interval(iv.clone())).unwrap();
    assert_eq!(key, ValueHashKey::Interval(iv));
}

#[test]
fn hash_key_interval_mixed_signs() {
    let iv = IntervalValue::new(-1, 30, -500_000);
    let key = build_hash_key(&Value::Interval(iv.clone())).unwrap();
    assert_eq!(key, ValueHashKey::Interval(iv));
}

// =====================================================================
// NEW: Hash key for real/double edge values
// =====================================================================

#[test]
fn hash_key_real_max() {
    let key = build_hash_key(&Value::Real(f32::MAX)).unwrap();
    assert_eq!(key, ValueHashKey::Real(f32::MAX.to_bits()));
}

#[test]
fn hash_key_real_min_positive() {
    let key = build_hash_key(&Value::Real(f32::MIN_POSITIVE)).unwrap();
    assert_eq!(key, ValueHashKey::Real(f32::MIN_POSITIVE.to_bits()));
}

#[test]
fn hash_key_real_epsilon() {
    let key = build_hash_key(&Value::Real(f32::EPSILON)).unwrap();
    assert_eq!(key, ValueHashKey::Real(f32::EPSILON.to_bits()));
}

#[test]
fn hash_key_double_max() {
    let key = build_hash_key(&Value::Double(f64::MAX)).unwrap();
    assert_eq!(key, ValueHashKey::Double(f64::MAX.to_bits()));
}

#[test]
fn hash_key_double_min_positive() {
    let key = build_hash_key(&Value::Double(f64::MIN_POSITIVE)).unwrap();
    assert_eq!(key, ValueHashKey::Double(f64::MIN_POSITIVE.to_bits()));
}

#[test]
fn hash_key_double_epsilon() {
    let key = build_hash_key(&Value::Double(f64::EPSILON)).unwrap();
    assert_eq!(key, ValueHashKey::Double(f64::EPSILON.to_bits()));
}

#[test]
fn hash_key_real_infinity() {
    let key = build_hash_key(&Value::Real(f32::INFINITY)).unwrap();
    assert_eq!(key, ValueHashKey::Real(f32::INFINITY.to_bits()));
}

#[test]
fn hash_key_real_neg_infinity() {
    let key = build_hash_key(&Value::Real(f32::NEG_INFINITY)).unwrap();
    assert_eq!(key, ValueHashKey::Real(f32::NEG_INFINITY.to_bits()));
}

#[test]
fn hash_key_double_infinity() {
    let key = build_hash_key(&Value::Double(f64::INFINITY)).unwrap();
    assert_eq!(key, ValueHashKey::Double(f64::INFINITY.to_bits()));
}

#[test]
fn hash_key_double_neg_infinity() {
    let key = build_hash_key(&Value::Double(f64::NEG_INFINITY)).unwrap();
    assert_eq!(key, ValueHashKey::Double(f64::NEG_INFINITY.to_bits()));
}

// =====================================================================
// NEW: ValueHashKey Debug is meaningful
// =====================================================================

#[test]
fn value_hash_key_debug_not_empty() {
    let key = ValueHashKey::Int(42);
    let dbg = format!("{key:?}");
    assert!(!dbg.is_empty());
    assert!(dbg.contains("42"));
}

#[test]
fn value_hash_key_debug_null() {
    let key = ValueHashKey::Null;
    let dbg = format!("{key:?}");
    assert!(dbg.contains("Null"));
}

#[test]
fn value_hash_key_debug_text() {
    let key = ValueHashKey::Text("hello".into());
    let dbg = format!("{key:?}");
    assert!(dbg.contains("hello"));
}

// =====================================================================
// NEW: HashMap with many different value types
// =====================================================================

#[test]
fn hash_key_diverse_types_in_single_hashmap() {
    use std::collections::HashMap;
    let mut map: HashMap<ValueHashKey, &str> = HashMap::new();

    map.insert(build_hash_key(&Value::Null).unwrap(), "null");
    map.insert(build_hash_key(&Value::Int(1)).unwrap(), "int");
    map.insert(build_hash_key(&Value::BigInt(1)).unwrap(), "bigint");
    map.insert(build_hash_key(&Value::Real(1.0)).unwrap(), "real");
    map.insert(build_hash_key(&Value::Double(1.0)).unwrap(), "double");
    map.insert(
        build_hash_key(&Value::Numeric(NumericValue::new(1, 0))).unwrap(),
        "numeric",
    );
    map.insert(build_hash_key(&Value::Text("1".into())).unwrap(), "text");
    map.insert(build_hash_key(&Value::Boolean(true)).unwrap(), "bool");
    map.insert(build_hash_key(&Value::Blob(vec![1])).unwrap(), "blob");

    assert_eq!(map.len(), 9);

    assert_eq!(
        map.get(&build_hash_key(&Value::Null).unwrap()),
        Some(&"null")
    );
    assert_eq!(
        map.get(&build_hash_key(&Value::Int(1)).unwrap()),
        Some(&"int")
    );
    assert_eq!(
        map.get(&build_hash_key(&Value::BigInt(1)).unwrap()),
        Some(&"bigint"),
    );
    assert_eq!(
        map.get(&build_hash_key(&Value::Boolean(true)).unwrap()),
        Some(&"bool"),
    );
}

// =====================================================================
// NEW: Same value re-hashed many times stays consistent
// =====================================================================

#[test]
fn hash_consistency_real_many_times() {
    let v = Value::Real(std::f32::consts::PI);
    let first = build_hash_key(&v).unwrap();
    for _ in 0..100 {
        assert_eq!(build_hash_key(&v).unwrap(), first);
    }
}

#[test]
fn hash_consistency_numeric_many_times() {
    let v = Value::Numeric(NumericValue::new(123456, 4));
    let first = build_hash_key(&v).unwrap();
    let first_hash = compute_hash(&first);
    for _ in 0..100 {
        let key = build_hash_key(&v).unwrap();
        assert_eq!(key, first);
        assert_eq!(compute_hash(&key), first_hash);
    }
}

// =====================================================================
// NEW: Adjacent integer values have different keys
// =====================================================================

#[test]
fn hash_key_adjacent_ints_different() {
    for i in -5..5 {
        let k1 = build_hash_key(&Value::Int(i)).unwrap();
        let k2 = build_hash_key(&Value::Int(i + 1)).unwrap();
        assert_ne!(
            k1,
            k2,
            "Int({}) and Int({}) should have different keys",
            i,
            i + 1
        );
    }
}

#[test]
fn hash_key_adjacent_bigints_different() {
    for i in -5i64..5 {
        let k1 = build_hash_key(&Value::BigInt(i)).unwrap();
        let k2 = build_hash_key(&Value::BigInt(i + 1)).unwrap();
        assert_ne!(k1, k2);
    }
}

// =====================================================================
// NEW: Text with only whitespace differences
// =====================================================================

#[test]
fn hash_key_text_space_vs_empty() {
    let k1 = build_hash_key(&Value::Text(" ".into())).unwrap();
    let k2 = build_hash_key(&Value::Text(String::new())).unwrap();
    assert_ne!(k1, k2);
}

#[test]
fn hash_key_text_case_sensitive() {
    let k1 = build_hash_key(&Value::Text("Hello".into())).unwrap();
    let k2 = build_hash_key(&Value::Text("hello".into())).unwrap();
    assert_ne!(k1, k2);
}

// =====================================================================
// NEW: Blob byte-level differences
// =====================================================================

#[test]
fn hash_key_blob_single_byte_diff() {
    let k1 = build_hash_key(&Value::Blob(vec![0x00])).unwrap();
    let k2 = build_hash_key(&Value::Blob(vec![0x01])).unwrap();
    assert_ne!(k1, k2);
}

#[test]
fn hash_key_blob_same_bytes_different_length() {
    let k1 = build_hash_key(&Value::Blob(vec![1, 2])).unwrap();
    let k2 = build_hash_key(&Value::Blob(vec![1, 2, 0])).unwrap();
    assert_ne!(k1, k2);
}
