use super::*;
use aiondb_plan::ScalarFunction;

// =====================================================================
// Helper to build scalar function expressions
// =====================================================================

fn sfn(func: ScalarFunction, args: Vec<TypedExpr>, dt: DataType) -> TypedExpr {
    TypedExpr::scalar_function(func, args, dt, false)
}

fn sfn_nullable(func: ScalarFunction, args: Vec<TypedExpr>, dt: DataType) -> TypedExpr {
    TypedExpr::scalar_function(func, args, dt, true)
}

// =====================================================================
// NOW / CURRENT_TIMESTAMP
// =====================================================================

#[test]
fn now_returns_timestamptz() {
    let expr = sfn(ScalarFunction::Now, vec![], DataType::TimestampTz);
    let result = eval(&expr).unwrap();
    assert!(matches!(result, Value::TimestampTz(_)));
}

#[test]
fn current_timestamp_returns_timestamptz() {
    let expr = sfn(
        ScalarFunction::CurrentTimestamp,
        vec![],
        DataType::TimestampTz,
    );
    let result = eval(&expr).unwrap();
    assert!(matches!(result, Value::TimestampTz(_)));
}

#[test]
fn now_and_current_timestamp_are_close() {
    let expr_now = sfn(ScalarFunction::Now, vec![], DataType::TimestampTz);
    let expr_ct = sfn(
        ScalarFunction::CurrentTimestamp,
        vec![],
        DataType::TimestampTz,
    );
    let now = eval(&expr_now).unwrap();
    let ct = eval(&expr_ct).unwrap();
    assert!(matches!(now, Value::TimestampTz(_)));
    assert!(matches!(ct, Value::TimestampTz(_)));
}

// =====================================================================
// CURRENT_DATE
// =====================================================================

#[test]
fn current_date_returns_date() {
    let expr = sfn(ScalarFunction::CurrentDate, vec![], DataType::Date);
    let result = eval(&expr).unwrap();
    assert!(matches!(result, Value::Date(_)));
}

#[test]
fn current_date_is_today() {
    let expr = sfn(ScalarFunction::CurrentDate, vec![], DataType::Date);
    let result = eval(&expr).unwrap();
    if let Value::Date(d) = result {
        let now = time::OffsetDateTime::now_utc();
        assert_eq!(d.year(), now.year());
        assert_eq!(d.month(), now.month());
        assert_eq!(d.day(), now.day());
    } else {
        panic!("expected Date");
    }
}

// =====================================================================
// DATE_PART
// =====================================================================

#[test]
fn date_part_year_from_timestamp() {
    let ts = lit_timestamp(2024, Month::March, 15, 10, 30, 45);
    let expr = sfn(
        ScalarFunction::DatePart,
        vec![lit_text("year"), ts],
        DataType::Double,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Double(2024.0));
}

#[test]
fn date_part_month_from_timestamp() {
    let ts = lit_timestamp(2024, Month::March, 15, 10, 30, 45);
    let expr = sfn(
        ScalarFunction::DatePart,
        vec![lit_text("month"), ts],
        DataType::Double,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Double(3.0));
}

#[test]
fn date_part_day_from_timestamp() {
    let ts = lit_timestamp(2024, Month::March, 15, 10, 30, 45);
    let expr = sfn(
        ScalarFunction::DatePart,
        vec![lit_text("day"), ts],
        DataType::Double,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Double(15.0));
}

#[test]
fn date_part_hour_from_timestamp() {
    let ts = lit_timestamp(2024, Month::March, 15, 10, 30, 45);
    let expr = sfn(
        ScalarFunction::DatePart,
        vec![lit_text("hour"), ts],
        DataType::Double,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Double(10.0));
}

#[test]
fn date_part_minute_from_timestamp() {
    let ts = lit_timestamp(2024, Month::March, 15, 10, 30, 45);
    let expr = sfn(
        ScalarFunction::DatePart,
        vec![lit_text("minute"), ts],
        DataType::Double,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Double(30.0));
}

#[test]
fn date_part_second_from_timestamp() {
    let ts = lit_timestamp(2024, Month::March, 15, 10, 30, 45);
    let expr = sfn(
        ScalarFunction::DatePart,
        vec![lit_text("second"), ts],
        DataType::Double,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Double(45.0));
}

#[test]
fn date_part_null() {
    let expr = sfn_nullable(
        ScalarFunction::DatePart,
        vec![lit_text("year"), lit_null()],
        DataType::Double,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn date_part_from_date() {
    let d = lit_date(2024, Month::June, 20);
    let expr = sfn(
        ScalarFunction::DatePart,
        vec![lit_text("year"), d],
        DataType::Double,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Double(2024.0));
}

#[test]
fn date_part_day_from_date() {
    let d = lit_date(2024, Month::June, 20);
    let expr = sfn(
        ScalarFunction::DatePart,
        vec![lit_text("day"), d],
        DataType::Double,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Double(20.0));
}

#[test]
fn date_part_from_interval() {
    // 2 years 3 months 10 days
    let iv = lit_interval(27, 10, 0);
    let expr = sfn(
        ScalarFunction::DatePart,
        vec![lit_text("year"), iv],
        DataType::Double,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Double(2.0));
}

#[test]
fn date_part_month_from_interval() {
    // 27 months = 2 years 3 months
    let iv = lit_interval(27, 10, 0);
    let expr = sfn(
        ScalarFunction::DatePart,
        vec![lit_text("month"), iv],
        DataType::Double,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Double(3.0));
}

#[test]
fn date_part_unknown_field() {
    let ts = lit_timestamp(2024, Month::March, 15, 10, 30, 45);
    let expr = sfn(
        ScalarFunction::DatePart,
        vec![lit_text("foobar"), ts],
        DataType::Double,
    );
    assert!(eval(&expr).is_err());
}

// =====================================================================
// DATE_TRUNC
// =====================================================================

#[test]
fn date_trunc_year() {
    let ts = lit_timestamp(2024, Month::March, 15, 10, 30, 45);
    let expr = sfn(
        ScalarFunction::DateTrunc,
        vec![lit_text("year"), ts],
        DataType::Timestamp,
    );
    let expected_dt = PrimitiveDateTime::new(
        Date::from_calendar_date(2024, Month::January, 1).unwrap(),
        Time::from_hms(0, 0, 0).unwrap(),
    );
    assert_eq!(eval(&expr).unwrap(), Value::Timestamp(expected_dt));
}

#[test]
fn date_trunc_month() {
    let ts = lit_timestamp(2024, Month::March, 15, 10, 30, 45);
    let expr = sfn(
        ScalarFunction::DateTrunc,
        vec![lit_text("month"), ts],
        DataType::Timestamp,
    );
    let expected_dt = PrimitiveDateTime::new(
        Date::from_calendar_date(2024, Month::March, 1).unwrap(),
        Time::from_hms(0, 0, 0).unwrap(),
    );
    assert_eq!(eval(&expr).unwrap(), Value::Timestamp(expected_dt));
}

#[test]
fn date_trunc_day() {
    let ts = lit_timestamp(2024, Month::March, 15, 10, 30, 45);
    let expr = sfn(
        ScalarFunction::DateTrunc,
        vec![lit_text("day"), ts],
        DataType::Timestamp,
    );
    let expected_dt = PrimitiveDateTime::new(
        Date::from_calendar_date(2024, Month::March, 15).unwrap(),
        Time::from_hms(0, 0, 0).unwrap(),
    );
    assert_eq!(eval(&expr).unwrap(), Value::Timestamp(expected_dt));
}

#[test]
fn date_trunc_hour() {
    let ts = lit_timestamp(2024, Month::March, 15, 10, 30, 45);
    let expr = sfn(
        ScalarFunction::DateTrunc,
        vec![lit_text("hour"), ts],
        DataType::Timestamp,
    );
    let expected_dt = PrimitiveDateTime::new(
        Date::from_calendar_date(2024, Month::March, 15).unwrap(),
        Time::from_hms(10, 0, 0).unwrap(),
    );
    assert_eq!(eval(&expr).unwrap(), Value::Timestamp(expected_dt));
}

#[test]
fn date_trunc_minute() {
    let ts = lit_timestamp(2024, Month::March, 15, 10, 30, 45);
    let expr = sfn(
        ScalarFunction::DateTrunc,
        vec![lit_text("minute"), ts],
        DataType::Timestamp,
    );
    let expected_dt = PrimitiveDateTime::new(
        Date::from_calendar_date(2024, Month::March, 15).unwrap(),
        Time::from_hms(10, 30, 0).unwrap(),
    );
    assert_eq!(eval(&expr).unwrap(), Value::Timestamp(expected_dt));
}

#[test]
fn date_trunc_null() {
    let expr = sfn_nullable(
        ScalarFunction::DateTrunc,
        vec![lit_text("year"), lit_null()],
        DataType::Timestamp,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn date_trunc_unknown_field() {
    let ts = lit_timestamp(2024, Month::March, 15, 10, 30, 45);
    let expr = sfn(
        ScalarFunction::DateTrunc,
        vec![lit_text("foobar"), ts],
        DataType::Timestamp,
    );
    assert!(eval(&expr).is_err());
}

// =====================================================================
// AGE
// =====================================================================

#[test]
fn age_same_timestamp() {
    let ts1 = lit_timestamp(2024, Month::March, 15, 10, 30, 45);
    let ts2 = lit_timestamp(2024, Month::March, 15, 10, 30, 45);
    let expr = sfn(ScalarFunction::Age, vec![ts1, ts2], DataType::Interval);
    assert_eq!(
        eval(&expr).unwrap(),
        Value::Interval(IntervalValue::new(0, 0, 0))
    );
}

#[test]
fn age_one_year_apart() {
    let ts1 = lit_timestamp(2025, Month::March, 15, 10, 0, 0);
    let ts2 = lit_timestamp(2024, Month::March, 15, 10, 0, 0);
    let expr = sfn(ScalarFunction::Age, vec![ts1, ts2], DataType::Interval);
    let result = eval(&expr).unwrap();
    if let Value::Interval(iv) = result {
        assert_eq!(iv.months, 12);
        assert_eq!(iv.days, 0);
    } else {
        panic!("expected Interval");
    }
}

#[test]
fn age_null_returns_null() {
    let ts = lit_timestamp(2024, Month::March, 15, 10, 0, 0);
    let expr = sfn_nullable(
        ScalarFunction::Age,
        vec![ts, lit_null()],
        DataType::Interval,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn age_partial_month() {
    let ts1 = lit_timestamp(2024, Month::March, 20, 10, 0, 0);
    let ts2 = lit_timestamp(2024, Month::February, 15, 10, 0, 0);
    let expr = sfn(ScalarFunction::Age, vec![ts1, ts2], DataType::Interval);
    let result = eval(&expr).unwrap();
    if let Value::Interval(iv) = result {
        assert_eq!(iv.months, 1);
        assert_eq!(iv.days, 5);
    } else {
        panic!("expected Interval");
    }
}

// =====================================================================
// TO_CHAR
// =====================================================================

#[test]
fn to_char_basic_format() {
    let ts = lit_timestamp(2024, Month::March, 15, 10, 30, 45);
    let expr = sfn(
        ScalarFunction::ToChar,
        vec![ts, lit_text("YYYY-MM-DD")],
        DataType::Text,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text("2024-03-15".into()));
}

#[test]
fn to_char_with_time() {
    let ts = lit_timestamp(2024, Month::March, 15, 10, 30, 45);
    let expr = sfn(
        ScalarFunction::ToChar,
        vec![ts, lit_text("YYYY-MM-DD HH24:MI:SS")],
        DataType::Text,
    );
    assert_eq!(
        eval(&expr).unwrap(),
        Value::Text("2024-03-15 10:30:45".into())
    );
}

#[test]
fn to_char_null() {
    let expr = sfn_nullable(
        ScalarFunction::ToChar,
        vec![lit_null(), lit_text("YYYY")],
        DataType::Text,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn to_char_just_year() {
    let ts = lit_timestamp(2024, Month::March, 15, 10, 30, 45);
    let expr = sfn(
        ScalarFunction::ToChar,
        vec![ts, lit_text("YYYY")],
        DataType::Text,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text("2024".into()));
}
