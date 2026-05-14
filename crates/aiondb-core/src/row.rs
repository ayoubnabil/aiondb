use crate::Value;

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Row {
    pub values: Vec<Value>,
}

impl Row {
    #[must_use]
    pub fn new(values: Vec<Value>) -> Self {
        Self { values }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Value> {
        self.values.iter()
    }

    #[must_use]
    pub fn into_values(self) -> Vec<Value> {
        self.values
    }
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
mod tests {
    use super::*;
    use crate::{IntervalValue, NumericValue, VectorValue};

    // ---------------------------------------------------------------
    // Row::new
    // ---------------------------------------------------------------

    #[test]
    fn row_new_empty_vec() {
        let r = Row::new(vec![]);
        assert_eq!(r.len(), 0);
        assert!(r.is_empty());
    }

    #[test]
    fn row_new_one_value() {
        let r = Row::new(vec![Value::Int(1)]);
        assert_eq!(r.len(), 1);
        assert!(!r.is_empty());
    }

    #[test]
    fn row_new_multiple_values() {
        let r = Row::new(vec![
            Value::Int(1),
            Value::Text("hello".to_string()),
            Value::Boolean(true),
        ]);
        assert_eq!(r.len(), 3);
    }

    #[test]
    fn row_new_all_nulls() {
        let r = Row::new(vec![Value::Null, Value::Null, Value::Null]);
        assert_eq!(r.len(), 3);
    }

    #[test]
    fn row_new_mixed_types() {
        let r = Row::new(vec![
            Value::Null,
            Value::Int(42),
            Value::BigInt(i64::MAX),
            Value::Real(3.14),
            Value::Double(2.718),
            Value::Numeric(NumericValue::new(100, 2)),
            Value::Text("text".to_string()),
            Value::Boolean(false),
            Value::Blob(vec![0xFF]),
            Value::Interval(IntervalValue::new(1, 2, 3)),
            Value::Vector(VectorValue::new(2, vec![1.0, 2.0])),
        ]);
        assert_eq!(r.len(), 11);
    }

    // ---------------------------------------------------------------
    // Row::len() and Row::is_empty()
    // ---------------------------------------------------------------

    #[test]
    fn row_len_matches_values_count() {
        for count in 0..=10 {
            let vals: Vec<Value> = (0..count)
                .map(|i| Value::Int(i32::try_from(i).unwrap_or(i32::MAX)))
                .collect();
            let r = Row::new(vals);
            assert_eq!(r.len(), count);
        }
    }

    #[test]
    fn row_is_empty_true_for_empty() {
        assert!(Row::new(vec![]).is_empty());
    }

    #[test]
    fn row_is_empty_false_for_non_empty() {
        assert!(!Row::new(vec![Value::Null]).is_empty());
    }

    // ---------------------------------------------------------------
    // Row::iter()
    // ---------------------------------------------------------------

    #[test]
    fn row_iter_yields_all_values_in_order() {
        let vals = vec![Value::Int(1), Value::Int(2), Value::Int(3)];
        let r = Row::new(vals.clone());
        let collected: Vec<&Value> = r.iter().collect();
        assert_eq!(collected.len(), 3);
        assert_eq!(*collected[0], Value::Int(1));
        assert_eq!(*collected[1], Value::Int(2));
        assert_eq!(*collected[2], Value::Int(3));
    }

    #[test]
    fn row_iter_empty_row_yields_nothing() {
        let r = Row::new(vec![]);
        assert_eq!(r.iter().count(), 0);
    }

    #[test]
    fn row_iter_single_element() {
        let r = Row::new(vec![Value::Boolean(true)]);
        let mut it = r.iter();
        assert_eq!(it.next(), Some(&Value::Boolean(true)));
        assert_eq!(it.next(), None);
    }

    // ---------------------------------------------------------------
    // Row::into_values()
    // ---------------------------------------------------------------

    #[test]
    fn row_into_values_returns_inner_vec() {
        let vals = vec![Value::Int(10), Value::Text("abc".to_string())];
        let r = Row::new(vals.clone());
        let extracted = r.into_values();
        assert_eq!(extracted, vals);
    }

    #[test]
    fn row_into_values_empty() {
        let r = Row::new(vec![]);
        let extracted = r.into_values();
        assert!(extracted.is_empty());
    }

    // ---------------------------------------------------------------
    // Default trait
    // ---------------------------------------------------------------

    #[test]
    fn row_default_produces_empty_row() {
        let r = Row::default();
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
    }

    #[test]
    fn row_default_equals_empty_new() {
        assert_eq!(Row::default(), Row::new(vec![]));
    }

    // ---------------------------------------------------------------
    // PartialEq
    // ---------------------------------------------------------------

    #[test]
    fn row_eq_same_rows_equal() {
        let a = Row::new(vec![Value::Int(1), Value::Text("x".to_string())]);
        let b = Row::new(vec![Value::Int(1), Value::Text("x".to_string())]);
        assert_eq!(a, b);
    }

    #[test]
    fn row_eq_different_values_not_equal() {
        let a = Row::new(vec![Value::Int(1)]);
        let b = Row::new(vec![Value::Int(2)]);
        assert_ne!(a, b);
    }

    #[test]
    fn row_eq_different_lengths_not_equal() {
        let a = Row::new(vec![Value::Int(1)]);
        let b = Row::new(vec![Value::Int(1), Value::Int(2)]);
        assert_ne!(a, b);
    }

    #[test]
    fn row_eq_empty_rows_equal() {
        assert_eq!(Row::new(vec![]), Row::new(vec![]));
    }

    #[test]
    fn row_eq_same_types_different_order() {
        let a = Row::new(vec![Value::Int(1), Value::Int(2)]);
        let b = Row::new(vec![Value::Int(2), Value::Int(1)]);
        assert_ne!(a, b);
    }

    #[test]
    fn row_eq_null_vs_int() {
        let a = Row::new(vec![Value::Null]);
        let b = Row::new(vec![Value::Int(0)]);
        assert_ne!(a, b);
    }

    // ---------------------------------------------------------------
    // Clone
    // ---------------------------------------------------------------

    #[test]
    fn row_clone_produces_equal_row() {
        let r = Row::new(vec![
            Value::Int(42),
            Value::Text("hello".to_string()),
            Value::Null,
        ]);
        let r2 = r.clone();
        assert_eq!(r, r2);
    }

    #[test]
    fn row_clone_empty() {
        let r = Row::new(vec![]);
        assert_eq!(r, r.clone());
    }

    #[test]
    fn row_clone_independence() {
        let r = Row::new(vec![Value::Text("original".to_string())]);
        let r2 = r.clone();
        // Modifying via into_values on one should not affect the other
        let vals = r.into_values();
        assert_eq!(vals[0], Value::Text("original".to_string()));
        assert_eq!(r2.len(), 1);
    }

    // ---------------------------------------------------------------
    // NEW: Row with every Value type
    // ---------------------------------------------------------------

    #[test]
    fn row_with_all_value_types_complete() {
        use time::{Date, Month, PrimitiveDateTime, Time};
        let dt = PrimitiveDateTime::new(
            Date::from_calendar_date(2024, Month::March, 15).unwrap(),
            Time::from_hms(10, 30, 0).unwrap(),
        );
        let d = Date::from_calendar_date(2024, Month::June, 1).unwrap();
        let row = Row::new(vec![
            Value::Null,
            Value::Int(i32::MIN),
            Value::BigInt(i64::MAX),
            Value::Real(f32::NAN),
            Value::Double(f64::INFINITY),
            Value::Numeric(NumericValue::new(i128::MAX, u32::MAX)),
            Value::Text(String::new()),
            Value::Boolean(false),
            Value::Blob(vec![]),
            Value::Timestamp(dt),
            Value::Date(d),
            Value::Interval(IntervalValue::new(-1, 30, -500)),
            Value::Vector(VectorValue::new(0, vec![])),
        ]);
        assert_eq!(row.len(), 13);
        assert!(!row.is_empty());
    }

    #[test]
    fn row_iter_with_all_types_preserves_order() {
        use time::{Date, Month};
        let d = Date::from_calendar_date(2024, Month::January, 1).unwrap();
        let row = Row::new(vec![Value::Null, Value::Int(42), Value::Date(d)]);
        let items: Vec<&Value> = row.iter().collect();
        assert_eq!(*items[0], Value::Null);
        assert_eq!(*items[1], Value::Int(42));
        assert_eq!(*items[2], Value::Date(d));
    }

    // ---------------------------------------------------------------
    // NEW: Single-element row operations
    // ---------------------------------------------------------------

    #[test]
    fn single_element_row_null() {
        let r = Row::new(vec![Value::Null]);
        assert_eq!(r.len(), 1);
        assert!(!r.is_empty());
        let vals = r.into_values();
        assert_eq!(vals, vec![Value::Null]);
    }

    #[test]
    fn single_element_row_int() {
        let r = Row::new(vec![Value::Int(42)]);
        assert_eq!(r.len(), 1);
        let mut it = r.iter();
        assert_eq!(it.next(), Some(&Value::Int(42)));
        assert_eq!(it.next(), None);
    }

    #[test]
    fn single_element_row_text() {
        let r = Row::new(vec![Value::Text("hello".to_string())]);
        assert_eq!(r.len(), 1);
        assert_eq!(r.into_values(), vec![Value::Text("hello".to_string())]);
    }

    #[test]
    fn single_element_row_bigint() {
        let r = Row::new(vec![Value::BigInt(i64::MAX)]);
        assert_eq!(r.len(), 1);
        assert_eq!(*r.iter().next().unwrap(), Value::BigInt(i64::MAX));
    }

    #[test]
    fn single_element_row_blob() {
        let r = Row::new(vec![Value::Blob(vec![0xFF; 100])]);
        assert_eq!(r.len(), 1);
        let vals = r.into_values();
        if let Value::Blob(ref b) = vals[0] {
            assert_eq!(b.len(), 100);
        } else {
            panic!("expected Blob");
        }
    }

    #[test]
    fn single_element_row_vector() {
        let r = Row::new(vec![Value::Vector(VectorValue::new(
            3,
            vec![1.0, 2.0, 3.0],
        ))]);
        assert_eq!(r.len(), 1);
    }

    // ---------------------------------------------------------------
    // NEW: Empty row edge cases
    // ---------------------------------------------------------------

    #[test]
    fn empty_row_iter_count_is_zero() {
        let r = Row::new(vec![]);
        assert_eq!(r.iter().count(), 0);
    }

    #[test]
    fn empty_row_into_values_is_empty_vec() {
        let r = Row::new(vec![]);
        let v = r.into_values();
        assert!(v.is_empty());
        assert_eq!(v.len(), 0);
    }

    #[test]
    fn empty_row_clone_is_empty() {
        let r = Row::new(vec![]);
        let r2 = r.clone();
        assert!(r2.is_empty());
        assert_eq!(r, r2);
    }

    #[test]
    fn empty_row_debug_is_not_empty_string() {
        let r = Row::new(vec![]);
        let dbg = format!("{r:?}");
        assert!(!dbg.is_empty());
    }

    // ---------------------------------------------------------------
    // NEW: Row iter() edge cases
    // ---------------------------------------------------------------

    #[test]
    fn row_iter_multiple_calls_are_independent() {
        let r = Row::new(vec![Value::Int(1), Value::Int(2)]);
        let c1: Vec<&Value> = r.iter().collect();
        let c2: Vec<&Value> = r.iter().collect();
        assert_eq!(c1, c2);
    }

    #[test]
    fn row_iter_partial_consumption() {
        let r = Row::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
        let mut it = r.iter();
        assert_eq!(it.next(), Some(&Value::Int(1)));
        // Don't consume the rest - that's fine
        assert_eq!(it.count(), 2); // remaining items
    }

    #[test]
    fn row_iter_with_nulls_mixed() {
        let r = Row::new(vec![Value::Null, Value::Int(1), Value::Null, Value::Int(2)]);
        let items: Vec<&Value> = r.iter().collect();
        assert_eq!(items.len(), 4);
        assert_eq!(*items[0], Value::Null);
        assert_eq!(*items[1], Value::Int(1));
        assert_eq!(*items[2], Value::Null);
        assert_eq!(*items[3], Value::Int(2));
    }

    // ---------------------------------------------------------------
    // NEW: Row into_values() preserves exact values
    // ---------------------------------------------------------------

    #[test]
    fn row_into_values_preserves_blob_content() {
        let blob_data = vec![0xCA, 0xFE, 0xBA, 0xBE];
        let r = Row::new(vec![Value::Blob(blob_data.clone())]);
        let vals = r.into_values();
        assert_eq!(vals[0], Value::Blob(blob_data));
    }

    #[test]
    fn row_into_values_preserves_numeric() {
        let nv = NumericValue::new(i128::MAX, 10);
        let r = Row::new(vec![Value::Numeric(nv.clone())]);
        let vals = r.into_values();
        assert_eq!(vals[0], Value::Numeric(nv));
    }

    #[test]
    fn row_into_values_preserves_interval() {
        let iv = IntervalValue::new(-1, 30, -999_999);
        let r = Row::new(vec![Value::Interval(iv.clone())]);
        let vals = r.into_values();
        assert_eq!(vals[0], Value::Interval(iv));
    }

    // ---------------------------------------------------------------
    // NEW: Row PartialEq edge cases
    // ---------------------------------------------------------------

    #[test]
    fn row_eq_with_nan_is_equal() {
        let r1 = Row::new(vec![Value::Real(f32::NAN)]);
        let r2 = Row::new(vec![Value::Real(f32::NAN)]);
        assert_eq!(r1, r2);
    }

    #[test]
    fn row_eq_with_infinity_is_equal() {
        let r1 = Row::new(vec![Value::Double(f64::INFINITY)]);
        let r2 = Row::new(vec![Value::Double(f64::INFINITY)]);
        assert_eq!(r1, r2);
    }

    #[test]
    fn row_eq_default_vs_empty_new() {
        let r1 = Row::default();
        let r2 = Row::new(vec![]);
        assert_eq!(r1, r2);
    }

    #[test]
    fn row_eq_different_types_same_position() {
        let r1 = Row::new(vec![Value::Int(42)]);
        let r2 = Row::new(vec![Value::BigInt(42)]);
        assert_ne!(r1, r2);
    }

    // ---------------------------------------------------------------
    // NEW: Row Clone with complex values
    // ---------------------------------------------------------------

    #[test]
    fn row_clone_with_large_text() {
        let text = "x".repeat(50_000);
        let r = Row::new(vec![Value::Text(text.clone())]);
        let r2 = r.clone();
        assert_eq!(r, r2);
        if let Value::Text(ref s) = r2.values[0] {
            assert_eq!(s.len(), 50_000);
        }
    }

    #[test]
    fn row_clone_with_vector() {
        let vv = VectorValue::new(3, vec![1.0, 2.0, 3.0]);
        let r = Row::new(vec![Value::Vector(vv.clone())]);
        let r2 = r.clone();
        assert_eq!(r, r2);
    }

    #[test]
    fn row_clone_with_many_values() {
        let vals: Vec<Value> = (0..100).map(Value::Int).collect();
        let r = Row::new(vals);
        let r2 = r.clone();
        assert_eq!(r, r2);
        assert_eq!(r2.len(), 100);
    }

    // ---------------------------------------------------------------
    // NEW: Row Debug
    // ---------------------------------------------------------------

    #[test]
    fn row_debug_contains_values() {
        let r = Row::new(vec![Value::Int(42)]);
        let dbg = format!("{r:?}");
        assert!(dbg.contains("42"));
    }

    #[test]
    fn row_debug_empty() {
        let r = Row::new(vec![]);
        let dbg = format!("{r:?}");
        assert!(dbg.contains("Row"));
    }

    // ---------------------------------------------------------------
    // NEW: Row values field direct access
    // ---------------------------------------------------------------

    #[test]
    fn row_values_field_accessible() {
        let r = Row::new(vec![Value::Int(1), Value::Int(2)]);
        assert_eq!(r.values.len(), 2);
        assert_eq!(r.values[0], Value::Int(1));
        assert_eq!(r.values[1], Value::Int(2));
    }

    #[test]
    fn row_values_field_empty() {
        let r = Row::new(vec![]);
        assert!(r.values.is_empty());
    }
}
