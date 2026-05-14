use std::cmp::Ordering;
use std::fmt;

use time::Month;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub struct PgDate {
    year: i32,
    month: Month,
    day: u8,
}

impl PgDate {
    #[allow(clippy::missing_errors_doc, clippy::result_unit_err)]
    pub fn from_calendar_date(year: i32, month: Month, day: u8) -> Result<Self, ()> {
        if day == 0 || day > days_in_month(year, month) {
            return Err(());
        }
        Ok(Self { year, month, day })
    }

    #[must_use]
    pub const fn year(self) -> i32 {
        self.year
    }

    #[must_use]
    pub const fn month(self) -> Month {
        self.month
    }

    #[must_use]
    pub const fn day(self) -> u8 {
        self.day
    }

    #[must_use]
    pub fn try_to_time_date(self) -> Option<time::Date> {
        time::Date::from_calendar_date(self.year, self.month, self.day).ok()
    }

    #[must_use]
    pub fn cmp_time_date(self, other: time::Date) -> Ordering {
        self.cmp(&Self::from(other))
    }

    #[must_use]
    pub fn days_since(self, other: Self) -> i64 {
        self.julian_day() - other.julian_day()
    }

    #[must_use]
    pub fn julian_day(self) -> i64 {
        julian_day(self.year, u8::from(self.month), self.day)
    }
}

impl From<time::Date> for PgDate {
    fn from(value: time::Date) -> Self {
        Self {
            year: value.year(),
            month: value.month(),
            day: value.day(),
        }
    }
}

impl Ord for PgDate {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.year, u8::from(self.month), self.day).cmp(&(
            other.year,
            u8::from(other.month),
            other.day,
        ))
    }
}

impl PartialOrd for PgDate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for PgDate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02}",
            self.year,
            u8::from(self.month),
            self.day
        )
    }
}

fn days_in_month(year: i32, month: Month) -> u8 {
    match month {
        Month::January
        | Month::March
        | Month::May
        | Month::July
        | Month::August
        | Month::October
        | Month::December => 31,
        Month::April | Month::June | Month::September | Month::November => 30,
        Month::February => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
    }
}

fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn julian_day(year: i32, month: u8, day: u8) -> i64 {
    let month = i64::from(month);
    let day = i64::from(day);
    let a = (14 - month) / 12;
    let y = i64::from(year) + 4800 - a;
    let m = month + 12 * a - 3;
    day + ((153 * m + 2) / 5) + 365 * y + (y / 4) - (y / 100) + (y / 400) - 32_045
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_large_postgres_years() {
        let date = PgDate::from_calendar_date(2_202_020, Month::October, 5).expect("pg date");
        assert_eq!(date.to_string(), "2202020-10-05");
        assert!(date.try_to_time_date().is_none());
    }

    #[test]
    fn orders_after_regular_dates() {
        let large = PgDate::from_calendar_date(2_202_020, Month::October, 5).expect("pg date");
        let small = time::Date::from_calendar_date(2020, Month::October, 5).expect("date");
        assert_eq!(large.cmp_time_date(small), Ordering::Greater);
    }
}
