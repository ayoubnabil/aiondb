use aiondb_core::{
    temporal::{is_infinity_date, is_infinity_timestamp, is_infinity_timestamptz},
    DataType, DbError, DbResult, IntervalValue, Value,
};
use time::{PrimitiveDateTime, Time};

use super::expect_args;
use crate::eval::cast::cast_value;
use crate::eval::operators::temporal::{
    add_interval_to_date, add_interval_to_timestamp, add_interval_to_timestamptz,
};
use crate::eval::session::current_time_zone;

pub(super) fn eval_date_constructor(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "date")?;
    cast_constructor_arg(args, 0, &DataType::Date, "date")
}

pub(super) fn eval_timestamptz_constructor(args: &[Value]) -> DbResult<Value> {
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }

    match args {
        [value] => cast_value(value.clone(), &DataType::TimestampTz),
        [date_value, time_value] => {
            let Value::Date(date) = cast_value(date_value.clone(), &DataType::Date)? else {
                return Err(DbError::internal(
                    "timestamptz() first arg must be date-like",
                ));
            };

            if let Value::TimeTz(time, offset) = time_value {
                return Ok(Value::TimestampTz(
                    PrimitiveDateTime::new(date, *time).assume_offset(*offset),
                ));
            }

            let Value::Time(time) = cast_value(time_value.clone(), &DataType::Time)? else {
                return Err(DbError::internal(
                    "timestamptz() second arg must be time-like",
                ));
            };
            let timezone = current_time_zone();
            Ok(Value::TimestampTz(
                timezone.apply_to_local(PrimitiveDateTime::new(date, time)),
            ))
        }
        _ => Err(DbError::internal("timestamptz() requires 1 or 2 arguments")),
    }
}

pub(super) fn eval_isfinite(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "isfinite")?;
    if matches!(args[0], Value::Null) {
        return Ok(Value::Null);
    }

    let finite = match &args[0] {
        Value::Date(date) => !is_infinity_date(*date),
        Value::Timestamp(timestamp) => !is_infinity_timestamp(*timestamp),
        Value::TimestampTz(timestamp) => !is_infinity_timestamptz(*timestamp),
        _ => true,
    };
    Ok(Value::Boolean(finite))
}

pub(super) fn eval_overlaps(args: &[Value]) -> DbResult<Value> {
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }

    let ((left_start, left_end), (right_start, right_end)) = match args {
        [left, right] => (parse_overlap_pair(left)?, parse_overlap_pair(right)?),
        [a, b, c, d] => (
            build_overlap_pair(a.clone(), b.clone())?,
            build_overlap_pair(c.clone(), d.clone())?,
        ),
        _ => {
            return Err(DbError::internal(
                "overlaps() requires 2 row arguments or 4 scalar arguments",
            ));
        }
    };

    if left_start.kind != right_start.kind {
        return Err(DbError::internal(
            "overlaps() arguments must belong to the same temporal domain",
        ));
    }

    Ok(Value::Boolean(
        left_start.micros < right_end.micros && right_start.micros < left_end.micros,
    ))
}

fn cast_constructor_arg(
    args: &[Value],
    index: usize,
    data_type: &DataType,
    name: &str,
) -> DbResult<Value> {
    let value = args
        .get(index)
        .cloned()
        .ok_or_else(|| DbError::internal(format!("{name}() missing argument")))?;
    if matches!(value, Value::Null) {
        Ok(Value::Null)
    } else {
        cast_value(value, data_type)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OverlapPoint {
    kind: OverlapKind,
    micros: i128,
    source: OverlapSource,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OverlapKind {
    DateTime,
    Time,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OverlapSource {
    Timestamp(PrimitiveDateTime),
    TimestampTz(time::OffsetDateTime),
    Date(time::Date),
    Time(Time),
    TimeTz(Time, time::UtcOffset),
}

fn parse_overlap_pair(value: &Value) -> DbResult<(OverlapPoint, OverlapPoint)> {
    match value {
        Value::Text(text) => {
            let (left, right) = split_overlap_tuple(text)?;
            build_overlap_pair(parse_overlap_literal(left), parse_overlap_literal(right))
        }
        Value::Array(values) if values.len() == 2 => {
            build_overlap_pair(values[0].clone(), values[1].clone())
        }
        _ => Err(DbError::internal(
            "overlaps() row argument must be a two-element row value",
        )),
    }
}

fn build_overlap_pair(
    start: Value,
    end_or_interval: Value,
) -> DbResult<(OverlapPoint, OverlapPoint)> {
    let start = temporal_point(&start)?;
    let mut end = match &end_or_interval {
        Value::Interval(interval) => add_interval_to_point(start, interval)?,
        _ => temporal_point(&end_or_interval)?,
    };
    let mut start = start;
    if end.micros < start.micros {
        std::mem::swap(&mut start, &mut end);
    }
    Ok((start, end))
}

fn temporal_point(value: &Value) -> DbResult<OverlapPoint> {
    match value {
        Value::Timestamp(value) => Ok(OverlapPoint {
            kind: OverlapKind::DateTime,
            micros: value.assume_utc().unix_timestamp_nanos() / 1_000,
            source: OverlapSource::Timestamp(*value),
        }),
        Value::TimestampTz(value) => Ok(OverlapPoint {
            kind: OverlapKind::DateTime,
            micros: value.unix_timestamp_nanos() / 1_000,
            source: OverlapSource::TimestampTz(*value),
        }),
        Value::Date(value) => Ok(OverlapPoint {
            kind: OverlapKind::DateTime,
            micros: PrimitiveDateTime::new(*value, Time::MIDNIGHT)
                .assume_utc()
                .unix_timestamp_nanos()
                / 1_000,
            source: OverlapSource::Date(*value),
        }),
        Value::Time(value) => Ok(OverlapPoint {
            kind: OverlapKind::Time,
            micros: time_to_micros(*value),
            source: OverlapSource::Time(*value),
        }),
        Value::TimeTz(value, offset) => Ok(OverlapPoint {
            kind: OverlapKind::Time,
            micros: time_to_micros(*value) - i128::from(offset.whole_seconds()) * 1_000_000,
            source: OverlapSource::TimeTz(*value, *offset),
        }),
        Value::Text(text) => {
            for data_type in [
                DataType::TimestampTz,
                DataType::Timestamp,
                DataType::Date,
                DataType::TimeTz,
                DataType::Time,
            ] {
                if let Ok(parsed) = cast_value(Value::Text(text.clone()), &data_type) {
                    return temporal_point(&parsed);
                }
            }
            Err(DbError::internal(
                "overlaps() could not parse temporal endpoint",
            ))
        }
        _ => Err(DbError::internal(
            "overlaps() endpoints must be temporal values",
        )),
    }
}

fn add_interval_to_point(point: OverlapPoint, interval: &IntervalValue) -> DbResult<OverlapPoint> {
    match point.source {
        OverlapSource::Timestamp(value) => temporal_point(&Value::Timestamp(
            add_interval_to_timestamp(value, interval)?,
        )),
        OverlapSource::TimestampTz(value) => temporal_point(&Value::TimestampTz(
            add_interval_to_timestamptz(value, interval)?,
        )),
        OverlapSource::Date(value) => {
            temporal_point(&Value::Timestamp(add_interval_to_date(value, interval)?))
        }
        OverlapSource::Time(_) | OverlapSource::TimeTz(_, _) => {
            let delta = overlap_interval_delta(interval)?;
            Ok(OverlapPoint {
                micros: point
                    .micros
                    .checked_add(delta)
                    .ok_or_else(|| DbError::internal("overlaps() interval overflow"))?,
                ..point
            })
        }
    }
}

fn overlap_interval_delta(interval: &IntervalValue) -> DbResult<i128> {
    i128::from(interval.months)
        .checked_mul(30 * 86_400_000_000)
        .and_then(|value| value.checked_add(i128::from(interval.days) * 86_400_000_000))
        .and_then(|value| value.checked_add(i128::from(interval.micros)))
        .ok_or_else(|| DbError::internal("overlaps() interval overflow"))
}

fn time_to_micros(value: Time) -> i128 {
    i128::from(value.hour()) * 3_600_000_000
        + i128::from(value.minute()) * 60_000_000
        + i128::from(value.second()) * 1_000_000
        + i128::from(value.microsecond())
}

fn split_overlap_tuple(text: &str) -> DbResult<(&str, &str)> {
    let trimmed = text.trim();
    let inner = trimmed
        .strip_prefix('(')
        .and_then(|value| value.strip_suffix(')'))
        .ok_or_else(|| DbError::internal("overlaps() expected a row tuple literal"))?;

    let mut depth = 0usize;
    let mut in_quotes = false;
    let mut escape = false;
    for (index, ch) in inner.char_indices() {
        if in_quotes {
            if escape {
                escape = false;
                continue;
            }
            if ch == '\\' {
                escape = true;
                continue;
            }
            if ch == '"' {
                in_quotes = false;
            }
            continue;
        }
        match ch {
            '"' => in_quotes = true,
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                let left = inner[..index].trim();
                let right = inner[index + 1..].trim();
                return Ok((left, right));
            }
            _ => {}
        }
    }

    Err(DbError::internal(
        "overlaps() tuple literal must contain exactly two elements",
    ))
}

fn parse_overlap_literal(raw: &str) -> Value {
    let trimmed = raw.trim().trim_matches('"');
    for data_type in [
        DataType::TimestampTz,
        DataType::Timestamp,
        DataType::Date,
        DataType::TimeTz,
        DataType::Time,
        DataType::Interval,
    ] {
        if let Ok(value) = cast_value(Value::Text(trimmed.to_owned()), &data_type) {
            return value;
        }
    }
    Value::Text(trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_plan::{ScalarFunction, TypedExpr};

    #[test]
    fn overlap_tuple_prefers_time_over_interval_for_clock_literals() {
        let parsed = parse_overlap_literal("00:00");
        assert!(matches!(parsed, Value::Time(_) | Value::TimeTz(_, _)));
    }

    #[test]
    fn overlaps_accepts_time_row_pairs() {
        let result = eval_overlaps(&[
            Value::Text("(00:00,01:00)".to_owned()),
            Value::Text("(00:30,01:30)".to_owned()),
        ])
        .expect("overlaps should evaluate");
        assert_eq!(result, Value::Boolean(true));
    }

    #[test]
    fn overlaps_treats_touching_timestamp_ranges_as_false() {
        let start = Value::TimestampTz(
            PrimitiveDateTime::new(
                time::Date::from_calendar_date(2000, time::Month::November, 27).unwrap(),
                Time::MIDNIGHT,
            )
            .assume_offset(time::UtcOffset::from_hms(-8, 0, 0).unwrap()),
        );
        let length = Value::Interval(IntervalValue::new(0, 0, 12 * 3_600_000_000));
        let boundary = Value::TimestampTz(
            PrimitiveDateTime::new(
                time::Date::from_calendar_date(2000, time::Month::November, 27).unwrap(),
                Time::from_hms(12, 0, 0).unwrap(),
            )
            .assume_offset(time::UtcOffset::from_hms(-8, 0, 0).unwrap()),
        );
        let end = Value::TimestampTz(
            PrimitiveDateTime::new(
                time::Date::from_calendar_date(2000, time::Month::November, 30).unwrap(),
                Time::MIDNIGHT,
            )
            .assume_offset(time::UtcOffset::from_hms(-8, 0, 0).unwrap()),
        );

        let result = eval_overlaps(&[start, length, boundary, end]).expect("overlaps");
        assert_eq!(result, Value::Boolean(false));
    }

    #[test]
    fn overlaps_text_rows_treat_touching_timestamp_ranges_as_false() {
        let start = Value::TimestampTz(
            PrimitiveDateTime::new(
                time::Date::from_calendar_date(2000, time::Month::November, 27).unwrap(),
                Time::MIDNIGHT,
            )
            .assume_offset(time::UtcOffset::from_hms(-8, 0, 0).unwrap()),
        );
        let length = Value::Interval(IntervalValue::new(0, 0, 12 * 3_600_000_000));
        let boundary = Value::TimestampTz(
            PrimitiveDateTime::new(
                time::Date::from_calendar_date(2000, time::Month::November, 27).unwrap(),
                Time::from_hms(12, 0, 0).unwrap(),
            )
            .assume_offset(time::UtcOffset::from_hms(-8, 0, 0).unwrap()),
        );
        let end = Value::TimestampTz(
            PrimitiveDateTime::new(
                time::Date::from_calendar_date(2000, time::Month::November, 30).unwrap(),
                Time::MIDNIGHT,
            )
            .assume_offset(time::UtcOffset::from_hms(-8, 0, 0).unwrap()),
        );

        let left = Value::Text(format!("({start},{length})"));
        let right = Value::Text(format!("({boundary},{end})"));
        let left_text = left.to_string();
        let (left_raw_start, left_raw_end) =
            split_overlap_tuple(left_text.as_str()).expect("left tuple should split");
        assert_eq!(parse_overlap_literal(left_raw_start), start);
        assert_eq!(parse_overlap_literal(left_raw_end), length);
        let (left_start, left_end) = parse_overlap_pair(&left).expect("left pair should parse");
        let (right_start, right_end) = parse_overlap_pair(&right).expect("right pair should parse");
        assert_eq!(left_start.kind, OverlapKind::DateTime);
        assert_eq!(left_end.kind, OverlapKind::DateTime);
        assert_eq!(right_start.kind, OverlapKind::DateTime);
        assert_eq!(right_end.kind, OverlapKind::DateTime);
        assert_eq!(left_end.micros, right_start.micros);
        let result = eval_overlaps(&[left, right]).expect("overlaps");
        assert_eq!(result, Value::Boolean(false));
    }

    #[test]
    fn overlaps_text_rows_with_non_overlapping_timestamptz_endpoints_are_false() {
        let left_start = Value::TimestampTz(
            PrimitiveDateTime::new(
                time::Date::from_calendar_date(2000, time::Month::November, 26).unwrap(),
                Time::MIDNIGHT,
            )
            .assume_offset(time::UtcOffset::from_hms(-8, 0, 0).unwrap()),
        );
        let left_end = Value::TimestampTz(
            PrimitiveDateTime::new(
                time::Date::from_calendar_date(2000, time::Month::November, 27).unwrap(),
                Time::MIDNIGHT,
            )
            .assume_offset(time::UtcOffset::from_hms(-8, 0, 0).unwrap()),
        );
        let right_start = Value::TimestampTz(
            PrimitiveDateTime::new(
                time::Date::from_calendar_date(2000, time::Month::November, 27).unwrap(),
                Time::from_hms(12, 0, 0).unwrap(),
            )
            .assume_offset(time::UtcOffset::from_hms(-8, 0, 0).unwrap()),
        );
        let right_end = Value::TimestampTz(
            PrimitiveDateTime::new(
                time::Date::from_calendar_date(2000, time::Month::November, 30).unwrap(),
                Time::MIDNIGHT,
            )
            .assume_offset(time::UtcOffset::from_hms(-8, 0, 0).unwrap()),
        );

        let result = eval_overlaps(&[
            Value::Text(format!("({left_start},{left_end})")),
            Value::Text(format!("({right_start},{right_end})")),
        ])
        .expect("overlaps");
        assert_eq!(result, Value::Boolean(false));
    }

    #[test]
    fn overlaps_text_rows_with_non_overlapping_timestamp_endpoints_are_false() {
        let left_start = Value::Timestamp(PrimitiveDateTime::new(
            time::Date::from_calendar_date(2000, time::Month::November, 26).unwrap(),
            Time::MIDNIGHT,
        ));
        let left_end = Value::Timestamp(PrimitiveDateTime::new(
            time::Date::from_calendar_date(2000, time::Month::November, 27).unwrap(),
            Time::MIDNIGHT,
        ));
        let right_start = Value::Timestamp(PrimitiveDateTime::new(
            time::Date::from_calendar_date(2000, time::Month::November, 27).unwrap(),
            Time::from_hms(12, 0, 0).unwrap(),
        ));
        let right_end = Value::Timestamp(PrimitiveDateTime::new(
            time::Date::from_calendar_date(2000, time::Month::November, 30).unwrap(),
            Time::MIDNIGHT,
        ));

        let result = eval_overlaps(&[
            Value::Text(format!("({left_start},{left_end})")),
            Value::Text(format!("({right_start},{right_end})")),
        ])
        .expect("overlaps");
        assert_eq!(result, Value::Boolean(false));
    }

    #[test]
    fn overlaps_nested_row_functions_with_non_overlapping_timestamptz_endpoints_are_false() {
        let left = TypedExpr::scalar_function(
            ScalarFunction::Row,
            vec![
                TypedExpr::literal(
                    Value::TimestampTz(
                        PrimitiveDateTime::new(
                            time::Date::from_calendar_date(2000, time::Month::November, 26)
                                .unwrap(),
                            Time::MIDNIGHT,
                        )
                        .assume_offset(time::UtcOffset::from_hms(-8, 0, 0).unwrap()),
                    ),
                    DataType::TimestampTz,
                    false,
                ),
                TypedExpr::literal(
                    Value::TimestampTz(
                        PrimitiveDateTime::new(
                            time::Date::from_calendar_date(2000, time::Month::November, 27)
                                .unwrap(),
                            Time::MIDNIGHT,
                        )
                        .assume_offset(time::UtcOffset::from_hms(-8, 0, 0).unwrap()),
                    ),
                    DataType::TimestampTz,
                    false,
                ),
            ],
            DataType::Text,
            false,
        );
        let right = TypedExpr::scalar_function(
            ScalarFunction::Row,
            vec![
                TypedExpr::literal(
                    Value::TimestampTz(
                        PrimitiveDateTime::new(
                            time::Date::from_calendar_date(2000, time::Month::November, 27)
                                .unwrap(),
                            Time::from_hms(12, 0, 0).unwrap(),
                        )
                        .assume_offset(time::UtcOffset::from_hms(-8, 0, 0).unwrap()),
                    ),
                    DataType::TimestampTz,
                    false,
                ),
                TypedExpr::literal(
                    Value::TimestampTz(
                        PrimitiveDateTime::new(
                            time::Date::from_calendar_date(2000, time::Month::November, 30)
                                .unwrap(),
                            Time::MIDNIGHT,
                        )
                        .assume_offset(time::UtcOffset::from_hms(-8, 0, 0).unwrap()),
                    ),
                    DataType::TimestampTz,
                    false,
                ),
            ],
            DataType::Text,
            false,
        );
        let expr = TypedExpr::scalar_function(
            ScalarFunction::Generic("overlaps".to_owned()),
            vec![left, right],
            DataType::Boolean,
            false,
        );

        let result = crate::ExpressionEvaluator
            .evaluate(&expr)
            .expect("overlaps");
        assert_eq!(result, Value::Boolean(false));
    }
}
