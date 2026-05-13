use super::{date_out_of_range, interval_out_of_range, timestamp_out_of_range};
use aiondb_core::{
    temporal::{
        is_infinity_timestamp, is_infinity_timestamptz, pg_timestamp_max, pg_timestamp_min,
        pg_timestamptz_max, pg_timestamptz_min,
    },
    DbResult, IntervalValue,
};

pub(crate) fn add_interval_to_timestamp(
    timestamp: time::PrimitiveDateTime,
    interval: &IntervalValue,
) -> DbResult<time::PrimitiveDateTime> {
    apply_interval_to_timestamp(timestamp, interval, false)
}

pub(crate) fn sub_interval_from_timestamp(
    timestamp: time::PrimitiveDateTime,
    interval: &IntervalValue,
) -> DbResult<time::PrimitiveDateTime> {
    apply_interval_to_timestamp(timestamp, interval, true)
}

pub(crate) fn add_interval_to_date(
    date: time::Date,
    interval: &IntervalValue,
) -> DbResult<time::PrimitiveDateTime> {
    add_interval_to_timestamp(
        time::PrimitiveDateTime::new(date, time::Time::MIDNIGHT),
        interval,
    )
}

/// Cypher Date ± Duration: apply months/days only, returning a Date.
/// Skipping the duration's time component avoids the spurious day
/// rollover when subtracting H/M/S from midnight.
pub(crate) fn apply_interval_calendar_to_date(
    date: time::Date,
    interval: &IntervalValue,
    subtract: bool,
) -> DbResult<time::Date> {
    let (months, days, _micros) = signed_interval_parts(interval, subtract)?;
    let shifted = apply_calendar_fields(
        time::PrimitiveDateTime::new(date, time::Time::MIDNIGHT),
        months,
        days,
    )?;
    Ok(shifted.date())
}

#[cfg(test)]
pub(crate) fn sub_interval_from_date(
    date: time::Date,
    interval: &IntervalValue,
) -> DbResult<time::PrimitiveDateTime> {
    sub_interval_from_timestamp(
        time::PrimitiveDateTime::new(date, time::Time::MIDNIGHT),
        interval,
    )
}

pub(crate) fn add_interval_to_timestamptz(
    timestamp: time::OffsetDateTime,
    interval: &IntervalValue,
) -> DbResult<time::OffsetDateTime> {
    apply_interval_to_timestamptz(timestamp, interval, false)
}

pub(crate) fn sub_interval_from_timestamptz(
    timestamp: time::OffsetDateTime,
    interval: &IntervalValue,
) -> DbResult<time::OffsetDateTime> {
    apply_interval_to_timestamptz(timestamp, interval, true)
}

fn apply_interval_to_timestamp(
    timestamp: time::PrimitiveDateTime,
    interval: &IntervalValue,
    subtract: bool,
) -> DbResult<time::PrimitiveDateTime> {
    if is_infinity_timestamp(timestamp) {
        return Ok(timestamp);
    }
    let (months, days, micros) = signed_interval_parts(interval, subtract)?;
    let shifted = apply_calendar_fields(timestamp, months, days)?;
    let shifted = shifted
        .checked_add(time::Duration::microseconds(micros))
        .ok_or_else(timestamp_out_of_range)?;
    if shifted < pg_timestamp_min() || shifted > pg_timestamp_max() {
        return Err(timestamp_out_of_range());
    }
    Ok(shifted)
}

fn apply_interval_to_timestamptz(
    timestamp: time::OffsetDateTime,
    interval: &IntervalValue,
    subtract: bool,
) -> DbResult<time::OffsetDateTime> {
    if is_infinity_timestamptz(timestamp) {
        return Ok(timestamp);
    }
    let (months, days, micros) = signed_interval_parts(interval, subtract)?;
    let timezone = crate::eval::session::current_time_zone();
    let (offset, _) = timezone.parts_for_utc(timestamp);
    let local = timestamp.to_offset(offset);
    let local_timestamp = time::PrimitiveDateTime::new(local.date(), local.time());
    let shifted_local = apply_calendar_fields(local_timestamp, months, days)?;
    let shifted = timezone.apply_to_local(shifted_local);
    let shifted = shifted
        .checked_add(time::Duration::microseconds(micros))
        .ok_or_else(timestamp_out_of_range)?;
    if shifted < pg_timestamptz_min() || shifted > pg_timestamptz_max() {
        return Err(timestamp_out_of_range());
    }
    Ok(shifted)
}

fn apply_calendar_fields(
    timestamp: time::PrimitiveDateTime,
    months: i32,
    days: i32,
) -> DbResult<time::PrimitiveDateTime> {
    let date = add_months_clamped(timestamp.date(), months)?;
    let date = date
        .checked_add(time::Duration::days(i64::from(days)))
        .ok_or_else(date_out_of_range)?;
    Ok(time::PrimitiveDateTime::new(date, timestamp.time()))
}

fn signed_interval_parts(interval: &IntervalValue, subtract: bool) -> DbResult<(i32, i32, i64)> {
    if !subtract {
        return Ok((interval.months, interval.days, interval.micros));
    }

    Ok((
        interval
            .months
            .checked_neg()
            .ok_or_else(interval_out_of_range)?,
        interval
            .days
            .checked_neg()
            .ok_or_else(interval_out_of_range)?,
        interval
            .micros
            .checked_neg()
            .ok_or_else(interval_out_of_range)?,
    ))
}

fn add_months_clamped(date: time::Date, delta_months: i32) -> DbResult<time::Date> {
    if delta_months == 0 {
        return Ok(date);
    }

    let total_months = i64::from(date.year())
        .checked_mul(12)
        .and_then(|value| value.checked_add(i64::from(u8::from(date.month())) - 1))
        .and_then(|value| value.checked_add(i64::from(delta_months)))
        .ok_or_else(date_out_of_range)?;

    let year = i32::try_from(total_months.div_euclid(12)).map_err(|_| date_out_of_range())?;
    let month_number =
        u8::try_from(total_months.rem_euclid(12) + 1).map_err(|_| date_out_of_range())?;
    let month = time::Month::try_from(month_number).map_err(|_| date_out_of_range())?;
    let day = date.day().min(days_in_month(year, month)?);

    time::Date::from_calendar_date(year, month, day).map_err(|_| date_out_of_range())
}

fn days_in_month(year: i32, month: time::Month) -> DbResult<u8> {
    (28..=31)
        .rev()
        .find(|&day| time::Date::from_calendar_date(year, month, day).is_ok())
        .ok_or_else(date_out_of_range)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::{with_session_context, EvalSessionContext};
    use time::{Date, Month, PrimitiveDateTime, Time, UtcOffset};

    #[test]
    fn timestamp_interval_uses_calendar_months() {
        let timestamp = PrimitiveDateTime::new(
            Date::from_calendar_date(2024, Month::January, 31).unwrap(),
            Time::from_hms(8, 15, 0).unwrap(),
        );
        let shifted =
            add_interval_to_timestamp(timestamp, &IntervalValue::new(1, 0, 0)).expect("shift");

        assert_eq!(
            shifted,
            PrimitiveDateTime::new(
                Date::from_calendar_date(2024, Month::February, 29).unwrap(),
                Time::from_hms(8, 15, 0).unwrap(),
            )
        );
    }

    #[test]
    fn date_interval_subtracts_calendar_years_without_thirty_day_approximation() {
        let shifted = sub_interval_from_date(
            Date::from_calendar_date(2024, Month::March, 31).unwrap(),
            &IntervalValue::new(12, 0, 0),
        )
        .expect("shift");

        assert_eq!(
            shifted,
            PrimitiveDateTime::new(
                Date::from_calendar_date(2023, Month::March, 31).unwrap(),
                Time::MIDNIGHT,
            )
        );
    }

    #[test]
    fn timestamptz_interval_preserves_local_clock_across_dst_boundaries() {
        let context = EvalSessionContext::from_settings(Some("ISO, MDY"), Some("PST8PDT"));
        with_session_context(context, || {
            let timestamp = PrimitiveDateTime::new(
                Date::from_calendar_date(2024, Month::March, 9).unwrap(),
                Time::from_hms(12, 0, 0).unwrap(),
            )
            .assume_offset(UtcOffset::from_hms(-8, 0, 0).unwrap());

            let shifted = add_interval_to_timestamptz(timestamp, &IntervalValue::new(0, 1, 0))
                .expect("shift");

            assert_eq!(
                shifted.date(),
                Date::from_calendar_date(2024, Month::March, 10).unwrap()
            );
            assert_eq!(shifted.time(), Time::from_hms(12, 0, 0).unwrap());
            assert_eq!(shifted.offset(), UtcOffset::from_hms(-7, 0, 0).unwrap());
        });
    }

    #[test]
    fn timestamptz_absolute_hours_follow_elapsed_time_across_dst_boundaries() {
        let context = EvalSessionContext::from_settings(Some("ISO, MDY"), Some("PST8PDT"));
        with_session_context(context, || {
            let timestamp = PrimitiveDateTime::new(
                Date::from_calendar_date(2024, Month::March, 9).unwrap(),
                Time::from_hms(12, 0, 0).unwrap(),
            )
            .assume_offset(UtcOffset::from_hms(-8, 0, 0).unwrap());

            let shifted = add_interval_to_timestamptz(
                timestamp,
                &IntervalValue::new(0, 0, 24 * 3_600_000_000),
            )
            .expect("shift");

            let local = shifted.to_offset(UtcOffset::from_hms(-7, 0, 0).unwrap());
            assert_eq!(
                local.date(),
                Date::from_calendar_date(2024, Month::March, 10).unwrap()
            );
            assert_eq!(local.time(), Time::from_hms(13, 0, 0).unwrap());
        });
    }

    #[test]
    fn timestamptz_year_interval_preserves_pacific_summer_local_time() {
        let context = EvalSessionContext::from_settings(Some("ISO, MDY"), Some("PST8PDT"));
        with_session_context(context, || {
            let timestamp = PrimitiveDateTime::new(
                Date::from_calendar_date(2001, Month::September, 22).unwrap(),
                Time::from_hms(18, 19, 20).unwrap(),
            )
            .assume_offset(UtcOffset::from_hms(-7, 0, 0).unwrap());

            let shifted = add_interval_to_timestamptz(timestamp, &IntervalValue::new(12, 0, 0))
                .expect("shift");

            assert_eq!(
                shifted,
                PrimitiveDateTime::new(
                    Date::from_calendar_date(2002, Month::September, 22).unwrap(),
                    Time::from_hms(18, 19, 20).unwrap(),
                )
                .assume_offset(UtcOffset::from_hms(-7, 0, 0).unwrap())
            );
        });
    }
}
