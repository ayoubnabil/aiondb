use super::*;
use aiondb_core::Value;
use time::{Date, Month, Time};

#[test]
fn date_from_iso_string() {
    let result = eval_cypher_date(&[Value::Text("2015-06-24".into())]).unwrap();
    let expected = Date::from_calendar_date(2015, Month::June, 24).unwrap();
    assert_eq!(result, Value::Date(expected));
}

#[test]
fn date_from_compact_string() {
    let result = eval_cypher_date(&[Value::Text("20150624".into())]).unwrap();
    let expected = Date::from_calendar_date(2015, Month::June, 24).unwrap();
    assert_eq!(result, Value::Date(expected));
}

#[test]
fn date_from_year_only() {
    let result = eval_cypher_date(&[Value::Text("2020".into())]).unwrap();
    let expected = Date::from_calendar_date(2020, Month::January, 1).unwrap();
    assert_eq!(result, Value::Date(expected));
}

#[test]
fn date_from_year_month() {
    let result = eval_cypher_date(&[Value::Text("2020-03".into())]).unwrap();
    let expected = Date::from_calendar_date(2020, Month::March, 1).unwrap();
    assert_eq!(result, Value::Date(expected));
}

#[test]
fn date_from_map() {
    let map = serde_json::json!({"year": 2015, "month": 6, "day": 24});
    let result = eval_cypher_date(&[Value::Jsonb(map)]).unwrap();
    let expected = Date::from_calendar_date(2015, Month::June, 24).unwrap();
    assert_eq!(result, Value::Date(expected));
}

#[test]
fn date_from_week_map() {
    let map = serde_json::json!({"year": 2015, "week": 1, "dayOfWeek": 3});
    let result = eval_cypher_date(&[Value::Jsonb(map)]).unwrap();
    let expected = Date::from_iso_week_date(2015, 1, time::Weekday::Wednesday).unwrap();
    assert_eq!(result, Value::Date(expected));
}

#[test]
fn date_from_ordinal_string() {
    let result = eval_cypher_date(&[Value::Text("2015-175".into())]).unwrap();
    let expected = Date::from_ordinal_date(2015, 175).unwrap();
    assert_eq!(result, Value::Date(expected));
}

#[test]
fn date_null_passthrough() {
    let result = eval_cypher_date(&[Value::Null]).unwrap();
    assert_eq!(result, Value::Null);
}

#[test]
fn date_invalid_string_returns_error() {
    let result = eval_cypher_date(&[Value::Text("not-a-date".into())]);
    assert!(result.is_err());
}

#[test]
fn date_rejects_integer() {
    let result = eval_cypher_date(&[Value::BigInt(42)]);
    assert!(result.is_err());
}

#[test]
fn time_from_string() {
    let result = eval_cypher_time(&[Value::Text("21:40:32.142".into())]).unwrap();
    let expected_time = Time::from_hms_micro(21, 40, 32, 142000).unwrap();
    assert_eq!(result, Value::TimeTz(expected_time, UtcOffset::UTC));
}

#[test]
fn time_from_string_with_offset() {
    let result = eval_cypher_time(&[Value::Text("21:40:32+01:00".into())]).unwrap();
    let expected_time = Time::from_hms_micro(21, 40, 32, 0).unwrap();
    let expected_offset = UtcOffset::from_hms(1, 0, 0).unwrap();
    assert_eq!(result, Value::TimeTz(expected_time, expected_offset));
}

#[test]
fn time_from_map() {
    let map = serde_json::json!({"hour": 12, "minute": 30, "second": 45});
    let result = eval_cypher_time(&[Value::Jsonb(map)]).unwrap();
    let expected_time = Time::from_hms_micro(12, 30, 45, 0).unwrap();
    assert_eq!(result, Value::TimeTz(expected_time, UtcOffset::UTC));
}

#[test]
fn localtime_from_string() {
    let result = eval_cypher_localtime(&[Value::Text("14:30:00".into())]).unwrap();
    let expected_time = Time::from_hms_micro(14, 30, 0, 0).unwrap();
    assert_eq!(result, Value::Time(expected_time));
}

#[test]
fn localtime_strips_timezone() {
    let result = eval_cypher_localtime(&[Value::Text("14:30:00+05:00".into())]).unwrap();
    let expected_time = Time::from_hms_micro(14, 30, 0, 0).unwrap();
    assert_eq!(result, Value::Time(expected_time));
}

#[test]
fn datetime_from_string() {
    let result = eval_cypher_datetime(&[Value::Text("2015-06-24T12:50:35.556".into())]).unwrap();
    if let Value::TimestampTz(odt) = result {
        assert_eq!(odt.year(), 2015);
        assert_eq!(odt.month(), Month::June);
        assert_eq!(odt.day(), 24);
        assert_eq!(odt.hour(), 12);
        assert_eq!(odt.minute(), 50);
        assert_eq!(odt.second(), 35);
    } else {
        panic!("Expected TimestampTz, got {result:?}");
    }
}

#[test]
fn datetime_from_map() {
    let map = serde_json::json!({"year": 2020, "month": 1, "day": 15, "hour": 8});
    let result = eval_cypher_datetime(&[Value::Jsonb(map)]).unwrap();
    if let Value::TimestampTz(odt) = result {
        assert_eq!(odt.year(), 2020);
        assert_eq!(odt.month(), Month::January);
        assert_eq!(odt.day(), 15);
        assert_eq!(odt.hour(), 8);
    } else {
        panic!("Expected TimestampTz, got {result:?}");
    }
}

#[test]
fn datetime_null_passthrough() {
    let result = eval_cypher_datetime(&[Value::Null]).unwrap();
    assert_eq!(result, Value::Null);
}

#[test]
fn localdatetime_from_string() {
    let result =
        eval_cypher_localdatetime(&[Value::Text("2015-07-21T21:40:32.142".into())]).unwrap();
    if let Value::Timestamp(ts) = result {
        assert_eq!(ts.year(), 2015);
        assert_eq!(ts.month(), Month::July);
        assert_eq!(ts.day(), 21);
        assert_eq!(ts.hour(), 21);
        assert_eq!(ts.minute(), 40);
    } else {
        panic!("Expected Timestamp, got {result:?}");
    }
}

#[test]
fn duration_from_iso_string() {
    let result = eval_cypher_duration(&[Value::Text("P14DT16H12M".into())]).unwrap();
    if let Value::Interval(iv) = result {
        assert_eq!(iv.months, 0);
        assert_eq!(iv.days, 14);
        assert_eq!(iv.micros, 58_320_000_000);
    } else {
        panic!("Expected Interval, got {result:?}");
    }
}

#[test]
fn duration_from_year_month_string() {
    let result = eval_cypher_duration(&[Value::Text("P1Y2M".into())]).unwrap();
    if let Value::Interval(iv) = result {
        assert_eq!(iv.months, 14);
        assert_eq!(iv.days, 0);
        assert_eq!(iv.micros, 0);
    } else {
        panic!("Expected Interval, got {result:?}");
    }
}

#[test]
fn duration_from_map() {
    let map = serde_json::json!({"days": 14, "hours": 16, "minutes": 12});
    let result = eval_cypher_duration(&[Value::Jsonb(map)]).unwrap();
    if let Value::Interval(iv) = result {
        assert_eq!(iv.months, 0);
        assert_eq!(iv.days, 14);
        assert_eq!(iv.micros, 58_320_000_000);
    } else {
        panic!("Expected Interval, got {result:?}");
    }
}

#[test]
fn duration_requires_argument() {
    let result = eval_cypher_duration(&[]);
    assert!(result.is_err());
}

#[test]
fn duration_null_passthrough() {
    let result = eval_cypher_duration(&[Value::Null]).unwrap();
    assert_eq!(result, Value::Null);
}
