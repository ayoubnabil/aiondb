use aiondb_core::temporal::{is_infinity_timestamp, is_infinity_timestamptz};
use aiondb_core::{DbError, DbResult, Value};
use time::{Date, OffsetDateTime, PrimitiveDateTime, Time};

use super::scalar_functions::expect_args;

use super::DAY_MICROS_I64 as DAY_MICROS;

fn precision_arg(args: &[Value]) -> DbResult<u32> {
    let precision = match &args[1] {
        Value::Int(value) => *value,
        Value::BigInt(value) => i32::try_from(*value).map_err(|_| {
            DbError::internal("__aiondb_temporal_precision() precision must fit in int4")
        })?,
        _ => {
            return Err(DbError::internal(
                "__aiondb_temporal_precision() precision must be integer",
            ))
        }
    };
    if !(0..=6).contains(&precision) {
        return Err(DbError::internal(
            "__aiondb_temporal_precision() precision must be between 0 and 6",
        ));
    }
    u32::try_from(precision)
        .map_err(|_| DbError::internal("__aiondb_temporal_precision() precision out of range"))
}

fn timestamp_out_of_range() -> DbError {
    DbError::internal("timestamp out of range during temporal precision rounding")
}

fn rounded_micros(value: i64, precision: u32) -> i64 {
    if precision >= 6 {
        return value;
    }
    let unit = 10_i64.pow(6 - precision);
    ((value + unit / 2) / unit) * unit
}

fn day_micros(time: Time) -> i64 {
    i64::from(time.hour()) * 3_600_000_000
        + i64::from(time.minute()) * 60_000_000
        + i64::from(time.second()) * 1_000_000
        + i64::from(time.microsecond())
}

fn time_from_day_micros(value: i64) -> DbResult<Time> {
    let hour = u8::try_from(value / 3_600_000_000).map_err(|_| timestamp_out_of_range())?;
    let minute =
        u8::try_from((value % 3_600_000_000) / 60_000_000).map_err(|_| timestamp_out_of_range())?;
    let second =
        u8::try_from((value % 60_000_000) / 1_000_000).map_err(|_| timestamp_out_of_range())?;
    let micro = u32::try_from(value % 1_000_000).map_err(|_| timestamp_out_of_range())?;
    Time::from_hms_micro(hour, minute, second, micro).map_err(|_| timestamp_out_of_range())
}

fn next_day(date: Date) -> DbResult<Date> {
    date.next_day().ok_or_else(timestamp_out_of_range)
}

fn round_time_precision(value: Time, precision: u32) -> DbResult<Time> {
    if precision >= 6 {
        return Ok(value);
    }
    let rounded = rounded_micros(day_micros(value), precision) % DAY_MICROS;
    time_from_day_micros(rounded)
}

fn round_timestamp_precision(
    value: PrimitiveDateTime,
    precision: u32,
) -> DbResult<PrimitiveDateTime> {
    if precision >= 6 || is_infinity_timestamp(value) {
        return Ok(value);
    }
    let rounded = rounded_micros(day_micros(value.time()), precision);
    let day_delta = rounded / DAY_MICROS;
    let micros_of_day = rounded % DAY_MICROS;
    let date = match day_delta {
        0 => value.date(),
        1 => next_day(value.date())?,
        _ => return Err(timestamp_out_of_range()),
    };
    Ok(PrimitiveDateTime::new(
        date,
        time_from_day_micros(micros_of_day)?,
    ))
}

fn round_timestamptz_precision(value: OffsetDateTime, precision: u32) -> DbResult<OffsetDateTime> {
    if precision >= 6 || is_infinity_timestamptz(value) {
        return Ok(value);
    }
    let offset = value.offset();
    let rounded_local = round_timestamp_precision(
        PrimitiveDateTime::new(value.date(), value.time()),
        precision,
    )?;
    Ok(rounded_local.assume_offset(offset))
}

pub(crate) fn eval_temporal_precision(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "__aiondb_temporal_precision")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }

    let precision = precision_arg(args)?;
    match &args[0] {
        Value::Time(value) => Ok(Value::Time(round_time_precision(*value, precision)?)),
        Value::TimeTz(value, offset) => Ok(Value::TimeTz(
            round_time_precision(*value, precision)?,
            *offset,
        )),
        Value::Timestamp(value) => Ok(Value::Timestamp(round_timestamp_precision(
            *value, precision,
        )?)),
        Value::TimestampTz(value) => Ok(Value::TimestampTz(round_timestamptz_precision(
            *value, precision,
        )?)),
        _ => Err(DbError::internal(
            "__aiondb_temporal_precision() first argument must be temporal",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::{Month, UtcOffset};

    fn date(year: i32, month: Month, day: u8) -> Date {
        Date::from_calendar_date(year, month, day).expect("valid date")
    }

    #[test]
    fn timestamp_precision_rounds_to_next_second() {
        let value = PrimitiveDateTime::new(
            date(1997, Month::February, 10),
            Time::from_hms_micro(17, 32, 1, 999_999).expect("time"),
        );
        let rounded = round_timestamp_precision(value, 2).expect("rounded timestamp");
        assert_eq!(
            rounded,
            PrimitiveDateTime::new(
                date(1997, Month::February, 10),
                Time::from_hms(17, 32, 2).expect("time")
            )
        );
    }

    #[test]
    fn timestamptz_precision_preserves_offset() {
        let value = PrimitiveDateTime::new(
            date(1997, Month::February, 10),
            Time::from_hms_micro(23, 59, 59, 999_999).expect("time"),
        )
        .assume_offset(UtcOffset::from_hms(-8, 0, 0).expect("offset"));
        let rounded = round_timestamptz_precision(value, 2).expect("rounded timestamptz");
        assert_eq!(rounded.offset(), value.offset());
        assert_eq!(rounded.date(), date(1997, Month::February, 11));
        assert_eq!(rounded.time(), Time::MIDNIGHT);
    }
}
