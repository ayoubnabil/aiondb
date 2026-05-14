use time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time, UtcOffset};

fn fixed_date(year: i32, month: Month, day: u8) -> Date {
    match Date::from_calendar_date(year, month, day) {
        Ok(date) => date,
        Err(_) => Date::MIN,
    }
}

fn fixed_time(hour: u8, minute: u8, second: u8, micros: u32) -> Time {
    match Time::from_hms_micro(hour, minute, second, micros) {
        Ok(time) => time,
        Err(_) => Time::MIDNIGHT,
    }
}

#[must_use]
pub fn neg_infinity_date() -> Date {
    Date::MIN
}

#[must_use]
pub fn pos_infinity_date() -> Date {
    Date::MAX
}

#[must_use]
pub fn is_infinity_date(date: Date) -> bool {
    date == neg_infinity_date() || date == pos_infinity_date()
}

#[must_use]
pub fn neg_infinity_timestamp() -> PrimitiveDateTime {
    PrimitiveDateTime::new(neg_infinity_date(), Time::MIDNIGHT)
}

#[must_use]
pub fn pos_infinity_timestamp() -> PrimitiveDateTime {
    PrimitiveDateTime::new(pos_infinity_date(), Time::MIDNIGHT)
}

#[must_use]
pub fn is_infinity_timestamp(timestamp: PrimitiveDateTime) -> bool {
    timestamp.time() == Time::MIDNIGHT && is_infinity_date(timestamp.date())
}

#[must_use]
pub fn neg_infinity_timestamptz() -> OffsetDateTime {
    neg_infinity_timestamp().assume_utc()
}

#[must_use]
pub fn pos_infinity_timestamptz() -> OffsetDateTime {
    pos_infinity_timestamp().assume_utc()
}

#[must_use]
pub fn is_infinity_timestamptz(timestamp: OffsetDateTime) -> bool {
    is_infinity_timestamp(PrimitiveDateTime::new(timestamp.date(), timestamp.time()))
}

#[must_use]
pub fn timestamp_infinity_label(date: Date, time: Time) -> Option<&'static str> {
    if time != Time::MIDNIGHT {
        return None;
    }
    if date == neg_infinity_date() {
        Some("-infinity")
    } else if date == pos_infinity_date() {
        Some("infinity")
    } else {
        None
    }
}

#[must_use]
pub fn pg_timestamp_min() -> PrimitiveDateTime {
    PrimitiveDateTime::new(fixed_date(-4713, Month::November, 24), Time::MIDNIGHT)
}

#[must_use]
pub fn pg_timestamp_max() -> PrimitiveDateTime {
    PrimitiveDateTime::new(
        fixed_date(294_276, Month::December, 31),
        fixed_time(23, 59, 59, 999_999),
    )
}

#[must_use]
pub fn pg_timestamptz_min() -> OffsetDateTime {
    pg_timestamp_min().assume_offset(UtcOffset::UTC)
}

#[must_use]
pub fn pg_timestamptz_max() -> OffsetDateTime {
    pg_timestamp_max().assume_offset(UtcOffset::UTC)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infinity_sentinels_live_outside_pg_timestamp_range() {
        assert!(neg_infinity_timestamp() < pg_timestamp_min());
        assert!(pos_infinity_timestamp() > pg_timestamp_max());
    }

    #[test]
    fn timestamp_infinity_labels_only_midnight_sentinels() {
        assert_eq!(
            timestamp_infinity_label(neg_infinity_date(), Time::MIDNIGHT),
            Some("-infinity")
        );
        assert_eq!(
            timestamp_infinity_label(pos_infinity_date(), Time::MIDNIGHT),
            Some("infinity")
        );
        assert_eq!(
            timestamp_infinity_label(pg_timestamp_min().date(), Time::MIDNIGHT),
            None
        );
    }
}
