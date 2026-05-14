use super::*;
use aiondb_core::{TidValue, VectorValue};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use time::{Date, Month, PrimitiveDateTime, Time};

fn compute_hash(key: &ValueHashKey) -> u64 {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish()
}

// =====================================================================
// build_hash_key: basic variant coverage
// =====================================================================

#[test]
fn hash_key_null() {
    let key = build_hash_key(&Value::Null).unwrap();
    assert_eq!(key, ValueHashKey::Null);
}

#[test]
fn hash_key_int_42() {
    let key = build_hash_key(&Value::Int(42)).unwrap();
    assert_eq!(key, ValueHashKey::Int(42));
}

#[test]
fn hash_key_int_zero() {
    let key = build_hash_key(&Value::Int(0)).unwrap();
    assert_eq!(key, ValueHashKey::Int(0));
}

#[test]
fn hash_key_int_negative() {
    let key = build_hash_key(&Value::Int(-1)).unwrap();
    assert_eq!(key, ValueHashKey::Int(-1));
}

#[test]
fn hash_key_int_max() {
    let key = build_hash_key(&Value::Int(i32::MAX)).unwrap();
    assert_eq!(key, ValueHashKey::Int(i32::MAX));
}

#[test]
fn hash_key_int_min() {
    let key = build_hash_key(&Value::Int(i32::MIN)).unwrap();
    assert_eq!(key, ValueHashKey::Int(i32::MIN));
}

#[test]
fn hash_key_bigint_100() {
    let key = build_hash_key(&Value::BigInt(100)).unwrap();
    assert_eq!(key, ValueHashKey::BigInt(100));
}

#[test]
fn hash_key_bigint_max() {
    let key = build_hash_key(&Value::BigInt(i64::MAX)).unwrap();
    assert_eq!(key, ValueHashKey::BigInt(i64::MAX));
}

#[test]
fn hash_key_bigint_min() {
    let key = build_hash_key(&Value::BigInt(i64::MIN)).unwrap();
    assert_eq!(key, ValueHashKey::BigInt(i64::MIN));
}

#[test]
fn hash_key_real_1_5() {
    let key = build_hash_key(&Value::Real(1.5)).unwrap();
    assert_eq!(key, ValueHashKey::Real(1.5f32.to_bits()));
}

#[test]
fn hash_key_real_negative() {
    let key = build_hash_key(&Value::Real(-3.14)).unwrap();
    assert_eq!(key, ValueHashKey::Real((-3.14f32).to_bits()));
}

#[test]
fn hash_key_double_2_5() {
    let key = build_hash_key(&Value::Double(2.5)).unwrap();
    assert_eq!(key, ValueHashKey::Double(2.5f64.to_bits()));
}

#[test]
fn hash_key_double_negative() {
    let key = build_hash_key(&Value::Double(-100.0)).unwrap();
    assert_eq!(key, ValueHashKey::Double((-100.0f64).to_bits()));
}

#[test]
fn hash_key_numeric() {
    let nv = NumericValue::new(12345, 3);
    let key = build_hash_key(&Value::Numeric(nv.clone())).unwrap();
    assert_eq!(key, ValueHashKey::Numeric(nv));
}

#[test]
fn hash_key_numeric_zero() {
    let nv = NumericValue::new(0, 0);
    let key = build_hash_key(&Value::Numeric(nv.clone())).unwrap();
    assert_eq!(key, ValueHashKey::Numeric(nv));
}

#[test]
fn hash_key_numeric_negative() {
    let nv = NumericValue::new(-999, 2);
    let key = build_hash_key(&Value::Numeric(nv.clone())).unwrap();
    assert_eq!(key, ValueHashKey::Numeric(nv));
}

#[test]
fn hash_key_text() {
    let key = build_hash_key(&Value::Text("hello".into())).unwrap();
    assert_eq!(key, ValueHashKey::Text("hello".into()));
}

#[test]
fn hash_key_text_empty() {
    let key = build_hash_key(&Value::Text(String::new())).unwrap();
    assert_eq!(key, ValueHashKey::Text(String::new()));
}

#[test]
fn hash_key_text_unicode() {
    let key = build_hash_key(&Value::Text("\u{1F600}".into())).unwrap();
    assert_eq!(key, ValueHashKey::Text("\u{1F600}".into()));
}

#[test]
fn hash_key_boolean_true() {
    let key = build_hash_key(&Value::Boolean(true)).unwrap();
    assert_eq!(key, ValueHashKey::Boolean(true));
}

#[test]
fn hash_key_boolean_false() {
    let key = build_hash_key(&Value::Boolean(false)).unwrap();
    assert_eq!(key, ValueHashKey::Boolean(false));
}

#[test]
fn hash_key_boolean_true_ne_false() {
    let k_true = build_hash_key(&Value::Boolean(true)).unwrap();
    let k_false = build_hash_key(&Value::Boolean(false)).unwrap();
    assert_ne!(k_true, k_false);
}

#[test]
fn hash_key_blob() {
    let key = build_hash_key(&Value::Blob(vec![0xDE, 0xAD])).unwrap();
    assert_eq!(key, ValueHashKey::Blob(vec![0xDE, 0xAD]));
}

#[test]
fn hash_key_blob_empty() {
    let key = build_hash_key(&Value::Blob(vec![])).unwrap();
    assert_eq!(key, ValueHashKey::Blob(vec![]));
}

#[test]
fn hash_key_timestamp() {
    let dt = PrimitiveDateTime::new(
        Date::from_calendar_date(2024, Month::March, 15).unwrap(),
        Time::from_hms(12, 30, 0).unwrap(),
    );
    let key = build_hash_key(&Value::Timestamp(dt)).unwrap();
    assert_eq!(key, ValueHashKey::Timestamp(dt));
}

#[test]
fn hash_key_date() {
    let d = Date::from_calendar_date(2024, Month::June, 15).unwrap();
    let key = build_hash_key(&Value::Date(d)).unwrap();
    assert_eq!(key, ValueHashKey::Date(d));
}

#[test]
fn hash_key_interval() {
    let iv = IntervalValue::new(12, 30, 1_000_000);
    let key = build_hash_key(&Value::Interval(iv.clone())).unwrap();
    assert_eq!(key, ValueHashKey::Interval(iv));
}

#[test]
fn hash_key_interval_all_zero() {
    let iv = IntervalValue::new(0, 0, 0);
    let key = build_hash_key(&Value::Interval(iv.clone())).unwrap();
    assert_eq!(key, ValueHashKey::Interval(iv));
}

#[test]
fn hash_key_interval_negative_months() {
    let iv = IntervalValue::new(-5, 10, 500);
    let key = build_hash_key(&Value::Interval(iv.clone())).unwrap();
    assert_eq!(key, ValueHashKey::Interval(iv));
}

#[test]
fn hash_key_tid() {
    let tid = TidValue::new(42, 7);
    let key = build_hash_key(&Value::Tid(tid)).unwrap();
    assert_eq!(key, ValueHashKey::Tid(tid));
}

// =====================================================================
// Vector -> error (not supported)
// =====================================================================

#[test]
fn hash_key_vector_error() {
    let vv = VectorValue::new(2, vec![1.0, 2.0]);
    assert!(build_hash_key(&Value::Vector(vv)).is_err());
}

#[test]
fn hash_key_vector_empty_error() {
    let vv = VectorValue::new(0, vec![]);
    assert!(build_hash_key(&Value::Vector(vv)).is_err());
}

// =====================================================================
// canonical_f32: 0.0 and -0.0 produce SAME hash key
// =====================================================================

#[test]
fn real_positive_zero_and_negative_zero_same_key() {
    let k1 = build_hash_key(&Value::Real(0.0)).unwrap();
    let k2 = build_hash_key(&Value::Real(-0.0)).unwrap();
    assert_eq!(k1, k2);
}

#[test]
fn real_positive_zero_and_negative_zero_same_hash() {
    let k1 = build_hash_key(&Value::Real(0.0)).unwrap();
    let k2 = build_hash_key(&Value::Real(-0.0)).unwrap();
    assert_eq!(compute_hash(&k1), compute_hash(&k2));
}

// =====================================================================
// canonical_f32: NaN produces consistent hash key
// =====================================================================

#[test]
fn real_nan_consistent_key() {
    let k1 = build_hash_key(&Value::Real(f32::NAN)).unwrap();
    let k2 = build_hash_key(&Value::Real(f32::NAN)).unwrap();
    assert_eq!(k1, k2);
}

#[test]
fn real_nan_different_representations_same_key() {
    let nan1 = f32::from_bits(0x7FC00000);
    let nan2 = f32::from_bits(0x7FC00001);
    assert!(nan1.is_nan());
    assert!(nan2.is_nan());
    let k1 = build_hash_key(&Value::Real(nan1)).unwrap();
    let k2 = build_hash_key(&Value::Real(nan2)).unwrap();
    assert_eq!(k1, k2);
}

#[test]
fn real_nan_same_hash() {
    let nan1 = f32::from_bits(0x7FC00000);
    let nan2 = f32::from_bits(0x7FC00001);
    let k1 = build_hash_key(&Value::Real(nan1)).unwrap();
    let k2 = build_hash_key(&Value::Real(nan2)).unwrap();
    assert_eq!(compute_hash(&k1), compute_hash(&k2));
}

// =====================================================================
// canonical_f64: 0.0 and -0.0 produce SAME hash key
// =====================================================================

#[test]
fn double_positive_zero_and_negative_zero_same_key() {
    let k1 = build_hash_key(&Value::Double(0.0)).unwrap();
    let k2 = build_hash_key(&Value::Double(-0.0)).unwrap();
    assert_eq!(k1, k2);
}

#[test]
fn double_positive_zero_and_negative_zero_same_hash() {
    let k1 = build_hash_key(&Value::Double(0.0)).unwrap();
    let k2 = build_hash_key(&Value::Double(-0.0)).unwrap();
    assert_eq!(compute_hash(&k1), compute_hash(&k2));
}

// =====================================================================
// canonical_f64: NaN produces consistent hash key
// =====================================================================

#[test]
fn double_nan_consistent_key() {
    let k1 = build_hash_key(&Value::Double(f64::NAN)).unwrap();
    let k2 = build_hash_key(&Value::Double(f64::NAN)).unwrap();
    assert_eq!(k1, k2);
}

#[test]
fn double_nan_different_representations_same_key() {
    let nan1 = f64::from_bits(0x7FF8000000000000);
    let nan2 = f64::from_bits(0x7FF8000000000001);
    assert!(nan1.is_nan());
    assert!(nan2.is_nan());
    let k1 = build_hash_key(&Value::Double(nan1)).unwrap();
    let k2 = build_hash_key(&Value::Double(nan2)).unwrap();
    assert_eq!(k1, k2);
}

#[test]
fn double_nan_same_hash() {
    let nan1 = f64::from_bits(0x7FF8000000000000);
    let nan2 = f64::from_bits(0x7FF8000000000001);
    let k1 = build_hash_key(&Value::Double(nan1)).unwrap();
    let k2 = build_hash_key(&Value::Double(nan2)).unwrap();
    assert_eq!(compute_hash(&k1), compute_hash(&k2));
}

// =====================================================================
// Hash consistency: same value produces same key multiple times
// =====================================================================

#[test]
fn hash_consistency_int() {
    let v = Value::Int(42);
    let k1 = build_hash_key(&v).unwrap();
    let k2 = build_hash_key(&v).unwrap();
    let k3 = build_hash_key(&v).unwrap();
    assert_eq!(k1, k2);
    assert_eq!(k2, k3);
    assert_eq!(compute_hash(&k1), compute_hash(&k2));
    assert_eq!(compute_hash(&k2), compute_hash(&k3));
}

#[test]
fn hash_consistency_text() {
    let v = Value::Text("consistency".into());
    let k1 = build_hash_key(&v).unwrap();
    let k2 = build_hash_key(&v).unwrap();
    assert_eq!(k1, k2);
    assert_eq!(compute_hash(&k1), compute_hash(&k2));
}

#[test]
fn hash_consistency_boolean() {
    let v = Value::Boolean(true);
    let k1 = build_hash_key(&v).unwrap();
    let k2 = build_hash_key(&v).unwrap();
    assert_eq!(k1, k2);
    assert_eq!(compute_hash(&k1), compute_hash(&k2));
}

#[test]
fn hash_consistency_null() {
    let k1 = build_hash_key(&Value::Null).unwrap();
    let k2 = build_hash_key(&Value::Null).unwrap();
    assert_eq!(k1, k2);
    assert_eq!(compute_hash(&k1), compute_hash(&k2));
}

#[test]
fn hash_consistency_blob() {
    let v = Value::Blob(vec![1, 2, 3, 4, 5]);
    let k1 = build_hash_key(&v).unwrap();
    let k2 = build_hash_key(&v).unwrap();
    assert_eq!(k1, k2);
    assert_eq!(compute_hash(&k1), compute_hash(&k2));
}

#[test]
fn hash_consistency_double() {
    let v = Value::Double(std::f64::consts::PI);
    let k1 = build_hash_key(&v).unwrap();
    let k2 = build_hash_key(&v).unwrap();
    assert_eq!(k1, k2);
    assert_eq!(compute_hash(&k1), compute_hash(&k2));
}

#[test]
fn hash_consistency_timestamp() {
    let dt = PrimitiveDateTime::new(
        Date::from_calendar_date(2024, Month::January, 1).unwrap(),
        Time::from_hms(0, 0, 0).unwrap(),
    );
    let v = Value::Timestamp(dt);
    let k1 = build_hash_key(&v).unwrap();
    let k2 = build_hash_key(&v).unwrap();
    assert_eq!(k1, k2);
    assert_eq!(compute_hash(&k1), compute_hash(&k2));
}

#[test]
fn hash_consistency_date() {
    let d = Date::from_calendar_date(2000, Month::December, 31).unwrap();
    let v = Value::Date(d);
    let k1 = build_hash_key(&v).unwrap();
    let k2 = build_hash_key(&v).unwrap();
    assert_eq!(k1, k2);
    assert_eq!(compute_hash(&k1), compute_hash(&k2));
}

#[test]
fn hash_consistency_interval() {
    let iv = IntervalValue::new(1, 2, 3);
    let v = Value::Interval(iv);
    let k1 = build_hash_key(&v).unwrap();
    let k2 = build_hash_key(&v).unwrap();
    assert_eq!(k1, k2);
    assert_eq!(compute_hash(&k1), compute_hash(&k2));
}

// =====================================================================
// Different values produce different keys
// =====================================================================

#[test]
fn different_ints_different_keys() {
    let k1 = build_hash_key(&Value::Int(1)).unwrap();
    let k2 = build_hash_key(&Value::Int(2)).unwrap();
    assert_ne!(k1, k2);
}

#[test]
fn different_bigints_different_keys() {
    let k1 = build_hash_key(&Value::BigInt(100)).unwrap();
    let k2 = build_hash_key(&Value::BigInt(200)).unwrap();
    assert_ne!(k1, k2);
}

#[test]
fn different_texts_different_keys() {
    let k1 = build_hash_key(&Value::Text("abc".into())).unwrap();
    let k2 = build_hash_key(&Value::Text("def".into())).unwrap();
    assert_ne!(k1, k2);
}

#[test]
fn different_doubles_different_keys() {
    let k1 = build_hash_key(&Value::Double(1.0)).unwrap();
    let k2 = build_hash_key(&Value::Double(2.0)).unwrap();
    assert_ne!(k1, k2);
}

#[test]
fn different_reals_different_keys() {
    let k1 = build_hash_key(&Value::Real(1.0)).unwrap();
    let k2 = build_hash_key(&Value::Real(2.0)).unwrap();
    assert_ne!(k1, k2);
}

#[test]
fn different_blobs_different_keys() {
    let k1 = build_hash_key(&Value::Blob(vec![1])).unwrap();
    let k2 = build_hash_key(&Value::Blob(vec![2])).unwrap();
    assert_ne!(k1, k2);
}

#[test]
fn different_dates_different_keys() {
    let d1 = Date::from_calendar_date(2024, Month::January, 1).unwrap();
    let d2 = Date::from_calendar_date(2024, Month::January, 2).unwrap();
    let k1 = build_hash_key(&Value::Date(d1)).unwrap();
    let k2 = build_hash_key(&Value::Date(d2)).unwrap();
    assert_ne!(k1, k2);
}

#[test]
fn different_timestamps_different_keys() {
    let dt1 = PrimitiveDateTime::new(
        Date::from_calendar_date(2024, Month::January, 1).unwrap(),
        Time::from_hms(0, 0, 0).unwrap(),
    );
    let dt2 = PrimitiveDateTime::new(
        Date::from_calendar_date(2024, Month::January, 1).unwrap(),
        Time::from_hms(0, 0, 1).unwrap(),
    );
    let k1 = build_hash_key(&Value::Timestamp(dt1)).unwrap();
    let k2 = build_hash_key(&Value::Timestamp(dt2)).unwrap();
    assert_ne!(k1, k2);
}

#[test]
fn different_intervals_different_keys() {
    let k1 = build_hash_key(&Value::Interval(IntervalValue::new(1, 0, 0))).unwrap();
    let k2 = build_hash_key(&Value::Interval(IntervalValue::new(2, 0, 0))).unwrap();
    assert_ne!(k1, k2);
}

#[test]
fn different_numerics_different_keys() {
    let k1 = build_hash_key(&Value::Numeric(NumericValue::new(100, 2))).unwrap();
    let k2 = build_hash_key(&Value::Numeric(NumericValue::new(200, 2))).unwrap();
    assert_ne!(k1, k2);
}

// =====================================================================
// Cross-type keys are always different (Int vs BigInt with same value)
// =====================================================================

#[test]
fn int_42_ne_bigint_42_key() {
    let k1 = build_hash_key(&Value::Int(42)).unwrap();
    let k2 = build_hash_key(&Value::BigInt(42)).unwrap();
    assert_ne!(k1, k2);
}

#[test]
fn null_ne_int_0_key() {
    let k1 = build_hash_key(&Value::Null).unwrap();
    let k2 = build_hash_key(&Value::Int(0)).unwrap();
    assert_ne!(k1, k2);
}

#[test]
fn real_1_ne_double_1_key() {
    let k1 = build_hash_key(&Value::Real(1.0)).unwrap();
    let k2 = build_hash_key(&Value::Double(1.0)).unwrap();
    assert_ne!(k1, k2);
}

// =====================================================================
// ValueHashKey: Eq and Hash property tests
// =====================================================================

#[test]
fn value_hash_key_eq_reflexive() {
    let key = ValueHashKey::Int(42);
    assert_eq!(key, key);
}

#[test]
fn value_hash_key_eq_symmetric() {
    let a = ValueHashKey::Text("hello".into());
    let b = ValueHashKey::Text("hello".into());
    assert_eq!(a, b);
    assert_eq!(b, a);
}

#[test]
fn value_hash_key_eq_transitive() {
    let a = ValueHashKey::BigInt(99);
    let b = ValueHashKey::BigInt(99);
    let c = ValueHashKey::BigInt(99);
    assert_eq!(a, b);
    assert_eq!(b, c);
    assert_eq!(a, c);
}

#[test]
fn value_hash_key_equal_implies_same_hash() {
    let a = ValueHashKey::Double(3.14f64.to_bits());
    let b = ValueHashKey::Double(3.14f64.to_bits());
    assert_eq!(a, b);
    assert_eq!(compute_hash(&a), compute_hash(&b));
}

#[test]
fn value_hash_key_clone_equals_original() {
    let key = ValueHashKey::Blob(vec![1, 2, 3]);
    let cloned = key.clone();
    assert_eq!(key, cloned);
    assert_eq!(compute_hash(&key), compute_hash(&cloned));
}

// =====================================================================
// canonical_f32 / canonical_f64 unit tests (direct)
// =====================================================================

#[test]
fn canonical_f32_positive_zero() {
    assert_eq!(canonical_f32(0.0f32), 0.0f32.to_bits());
}

#[test]
fn canonical_f32_negative_zero() {
    assert_eq!(canonical_f32(-0.0f32), 0.0f32.to_bits());
}

#[test]
fn canonical_f32_normal_value() {
    assert_eq!(canonical_f32(1.5f32), 1.5f32.to_bits());
}

#[test]
fn canonical_f32_nan() {
    let result = canonical_f32(f32::NAN);
    assert_eq!(result, f32::NAN.to_bits());
}

#[test]
fn canonical_f32_negative_nan() {
    let neg_nan = f32::from_bits(0xFFC00000);
    assert!(neg_nan.is_nan());
    let result = canonical_f32(neg_nan);
    assert_eq!(result, f32::NAN.to_bits());
}

#[test]
fn canonical_f32_infinity() {
    assert_eq!(canonical_f32(f32::INFINITY), f32::INFINITY.to_bits());
}

#[test]
fn canonical_f32_neg_infinity() {
    assert_eq!(
        canonical_f32(f32::NEG_INFINITY),
        f32::NEG_INFINITY.to_bits()
    );
}

#[test]
fn canonical_f64_positive_zero() {
    assert_eq!(canonical_f64(0.0f64), 0.0f64.to_bits());
}

#[test]
fn canonical_f64_negative_zero() {
    assert_eq!(canonical_f64(-0.0f64), 0.0f64.to_bits());
}

#[test]
fn canonical_f64_normal_value() {
    assert_eq!(canonical_f64(2.5f64), 2.5f64.to_bits());
}

#[test]
fn canonical_f64_nan() {
    let result = canonical_f64(f64::NAN);
    assert_eq!(result, f64::NAN.to_bits());
}

#[test]
fn canonical_f64_negative_nan() {
    let neg_nan = f64::from_bits(0xFFF8000000000000);
    assert!(neg_nan.is_nan());
    let result = canonical_f64(neg_nan);
    assert_eq!(result, f64::NAN.to_bits());
}

#[test]
fn canonical_f64_infinity() {
    assert_eq!(canonical_f64(f64::INFINITY), f64::INFINITY.to_bits());
}

#[test]
fn canonical_f64_neg_infinity() {
    assert_eq!(
        canonical_f64(f64::NEG_INFINITY),
        f64::NEG_INFINITY.to_bits()
    );
}

// =====================================================================
// Edge case: real infinity keys are different from each other
// =====================================================================

#[test]
fn real_inf_ne_neg_inf_key() {
    let k1 = build_hash_key(&Value::Real(f32::INFINITY)).unwrap();
    let k2 = build_hash_key(&Value::Real(f32::NEG_INFINITY)).unwrap();
    assert_ne!(k1, k2);
}

#[test]
fn double_inf_ne_neg_inf_key() {
    let k1 = build_hash_key(&Value::Double(f64::INFINITY)).unwrap();
    let k2 = build_hash_key(&Value::Double(f64::NEG_INFINITY)).unwrap();
    assert_ne!(k1, k2);
}

// =====================================================================
// Edge case: NaN key is different from 0.0 key
// =====================================================================

#[test]
fn real_nan_ne_zero_key() {
    let k1 = build_hash_key(&Value::Real(f32::NAN)).unwrap();
    let k2 = build_hash_key(&Value::Real(0.0)).unwrap();
    assert_ne!(k1, k2);
}

#[test]
fn double_nan_ne_zero_key() {
    let k1 = build_hash_key(&Value::Double(f64::NAN)).unwrap();
    let k2 = build_hash_key(&Value::Double(0.0)).unwrap();
    assert_ne!(k1, k2);
}

// =====================================================================
// Edge case: NaN key is different from infinity key
// =====================================================================

#[test]
fn real_nan_ne_inf_key() {
    let k1 = build_hash_key(&Value::Real(f32::NAN)).unwrap();
    let k2 = build_hash_key(&Value::Real(f32::INFINITY)).unwrap();
    assert_ne!(k1, k2);
}

#[test]
fn double_nan_ne_inf_key() {
    let k1 = build_hash_key(&Value::Double(f64::NAN)).unwrap();
    let k2 = build_hash_key(&Value::Double(f64::INFINITY)).unwrap();
    assert_ne!(k1, k2);
}

// =====================================================================
// Use in HashMap: hash key correctness
// =====================================================================

#[test]
fn hash_key_usable_in_hashmap() {
    use std::collections::HashMap;
    let mut map: HashMap<ValueHashKey, i32> = HashMap::new();

    let k1 = build_hash_key(&Value::Int(1)).unwrap();
    let k2 = build_hash_key(&Value::Int(2)).unwrap();
    let k_null = build_hash_key(&Value::Null).unwrap();

    map.insert(k1.clone(), 10);
    map.insert(k2.clone(), 20);
    map.insert(k_null.clone(), 30);

    assert_eq!(map.get(&k1), Some(&10));
    assert_eq!(map.get(&k2), Some(&20));
    assert_eq!(map.get(&k_null), Some(&30));

    let k1_again = build_hash_key(&Value::Int(1)).unwrap();
    assert_eq!(map.get(&k1_again), Some(&10));
}

#[test]
fn hash_key_real_zero_usable_in_hashmap_with_neg_zero() {
    use std::collections::HashMap;
    let mut map: HashMap<ValueHashKey, &str> = HashMap::new();

    let k_pos = build_hash_key(&Value::Real(0.0)).unwrap();
    map.insert(k_pos, "zero");

    let k_neg = build_hash_key(&Value::Real(-0.0)).unwrap();
    assert_eq!(map.get(&k_neg), Some(&"zero"));
}

#[test]
fn hash_key_double_zero_usable_in_hashmap_with_neg_zero() {
    use std::collections::HashMap;
    let mut map: HashMap<ValueHashKey, &str> = HashMap::new();

    let k_pos = build_hash_key(&Value::Double(0.0)).unwrap();
    map.insert(k_pos, "dzero");

    let k_neg = build_hash_key(&Value::Double(-0.0)).unwrap();
    assert_eq!(map.get(&k_neg), Some(&"dzero"));
}

#[test]
fn hash_key_nan_reals_map_to_same_entry() {
    use std::collections::HashMap;
    let mut map: HashMap<ValueHashKey, &str> = HashMap::new();

    let nan1 = f32::from_bits(0x7FC00000);
    let nan2 = f32::from_bits(0x7FC00001);

    let k1 = build_hash_key(&Value::Real(nan1)).unwrap();
    map.insert(k1, "nan_entry");

    let k2 = build_hash_key(&Value::Real(nan2)).unwrap();
    assert_eq!(map.get(&k2), Some(&"nan_entry"));
}

#[test]
fn hash_key_nan_doubles_map_to_same_entry() {
    use std::collections::HashMap;
    let mut map: HashMap<ValueHashKey, &str> = HashMap::new();

    let nan1 = f64::from_bits(0x7FF8000000000000);
    let nan2 = f64::from_bits(0x7FF8000000000001);

    let k1 = build_hash_key(&Value::Double(nan1)).unwrap();
    map.insert(k1, "dnan_entry");

    let k2 = build_hash_key(&Value::Double(nan2)).unwrap();
    assert_eq!(map.get(&k2), Some(&"dnan_entry"));
}
