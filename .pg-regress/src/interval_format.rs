use aiondb_core::IntervalValue;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IntervalStyle {
    Postgres,
    PostgresVerbose,
    SqlStandard,
    Iso8601,
}

#[derive(Clone, Copy, Debug)]
struct SignedIntervalParts {
    months: i128,
    days: i128,
    micros: i128,
}

impl SignedIntervalParts {
    fn from_interval(iv: &IntervalValue) -> Self {
        Self {
            months: i128::from(iv.months),
            days: i128::from(iv.days),
            micros: i128::from(iv.micros),
        }
    }

    fn negated(self) -> Self {
        Self {
            months: -self.months,
            days: -self.days,
            micros: -self.micros,
        }
    }

    fn is_zero(self) -> bool {
        self.months == 0 && self.days == 0 && self.micros == 0
    }
}

pub(crate) fn format_interval(iv: &IntervalValue, style: IntervalStyle) -> String {
    match style {
        IntervalStyle::Postgres => format_pg_interval_postgres(iv),
        IntervalStyle::PostgresVerbose => format_pg_interval_verbose(iv),
        IntervalStyle::SqlStandard => format_sql_standard_interval(iv),
        IntervalStyle::Iso8601 => format_iso_8601_interval(iv),
    }
}

pub(crate) fn format_pg_interval_verbose(iv: &IntervalValue) -> String {
    let original = SignedIntervalParts::from_interval(iv);
    if original.is_zero() {
        return "@ 0".to_owned();
    }

    let use_ago = leading_nonzero_sign(original) < 0;
    let display = if use_ago {
        original.negated()
    } else {
        original
    };
    let mixed_signs = has_mixed_signs(display);
    let mut parts = Vec::new();

    append_verbose_months(&mut parts, display.months, mixed_signs);
    append_verbose_days(&mut parts, display.days, mixed_signs);
    append_verbose_time(&mut parts, display.micros, mixed_signs);

    let mut rendered = format!("@ {}", parts.join(" "));
    if use_ago {
        rendered.push_str(" ago");
    }
    rendered
}

pub(crate) fn format_pg_interval_postgres(iv: &IntervalValue) -> String {
    let parts = SignedIntervalParts::from_interval(iv);
    let mut rendered = Vec::new();
    let mut last_date_sign = 0i8;

    let abs_months = parts.months.unsigned_abs();
    let years = (abs_months / 12) as i128;
    let mons = (abs_months % 12) as i128;
    let month_sign = signum(parts.months);

    if years != 0 {
        rendered.push(format_postgres_date_part(
            years,
            month_sign,
            last_date_sign,
            "year",
            "years",
        ));
        last_date_sign = month_sign;
    }
    if mons != 0 {
        rendered.push(format_postgres_date_part(
            mons,
            month_sign,
            last_date_sign,
            "mon",
            "mons",
        ));
        last_date_sign = month_sign;
    }
    if parts.days != 0 {
        let day_sign = signum(parts.days);
        rendered.push(format_postgres_date_part(
            parts.days.unsigned_abs() as i128,
            day_sign,
            last_date_sign,
            "day",
            "days",
        ));
        last_date_sign = day_sign;
    }

    if parts.micros != 0 || rendered.is_empty() {
        let sign = if parts.micros < 0 {
            "-"
        } else if !rendered.is_empty() && last_date_sign < 0 {
            "+"
        } else {
            ""
        };
        rendered.push(format!(
            "{sign}{}",
            format_hms(parts.micros.unsigned_abs(), true)
        ));
    }

    rendered.join(" ")
}

fn format_sql_standard_interval(iv: &IntervalValue) -> String {
    let parts = SignedIntervalParts::from_interval(iv);
    if parts.is_zero() {
        return "0".to_owned();
    }

    let mut year = parts.months / 12;
    let mut month = parts.months % 12;
    let mut day = parts.days;
    let mut hour = micros_hours(parts.micros);
    let mut minute = micros_minutes(parts.micros);
    let mut second = micros_seconds(parts.micros);
    let mut fraction = micros_fraction(parts.micros);

    let has_negative =
        year < 0 || month < 0 || day < 0 || hour < 0 || minute < 0 || second < 0 || fraction < 0;
    let has_positive =
        year > 0 || month > 0 || day > 0 || hour > 0 || minute > 0 || second > 0 || fraction > 0;
    let has_year_month = year != 0 || month != 0;
    let has_day_time = day != 0 || hour != 0 || minute != 0 || second != 0 || fraction != 0;
    let sql_standard_value = !(has_negative && has_positive) && !(has_year_month && has_day_time);

    if has_negative && sql_standard_value {
        year = -year;
        month = -month;
        day = -day;
        hour = -hour;
        minute = -minute;
        second = -second;
        fraction = -fraction;
        return format!(
            "-{}",
            format_sql_standard_components(year, month, day, hour, minute, second, fraction)
        );
    }

    if sql_standard_value {
        return format_sql_standard_components(year, month, day, hour, minute, second, fraction);
    }

    format_sql_standard_explicit(parts.months, parts.days, parts.micros)
}

fn format_sql_standard_components(
    year: i128,
    month: i128,
    day: i128,
    hour: i128,
    minute: i128,
    second: i128,
    fraction: i128,
) -> String {
    if year != 0 || month != 0 {
        return format!("{}-{}", year, month.unsigned_abs());
    }
    if day != 0 {
        return format!(
            "{day} {}",
            format_sql_clock(hour.abs(), minute.unsigned_abs(), second, fraction)
        );
    }
    format_sql_clock(hour, minute.unsigned_abs(), second, fraction)
}

fn format_sql_standard_explicit(months: i128, days: i128, micros: i128) -> String {
    let year_month = if months == 0 {
        "+0-0".to_owned()
    } else {
        format_sql_year_month(months, true)
    };
    let day = if days == 0 {
        "+0".to_owned()
    } else {
        format!("{:+}", days)
    };
    let time = if micros == 0 {
        "+0:00:00".to_owned()
    } else {
        format_sql_time(micros, true)
    };
    format!("{year_month} {day} {time}")
}

fn format_sql_year_month(months: i128, explicit_sign: bool) -> String {
    let abs_months = months.unsigned_abs();
    let years = abs_months / 12;
    let mons = abs_months % 12;
    let sign = if months < 0 {
        "-"
    } else if explicit_sign {
        "+"
    } else {
        ""
    };
    format!("{sign}{years}-{mons}")
}

fn format_sql_time(micros: i128, explicit_sign: bool) -> String {
    let sign = if micros < 0 {
        "-"
    } else if explicit_sign {
        "+"
    } else {
        ""
    };
    format!("{sign}{}", format_sql_time_abs(micros.unsigned_abs()))
}

fn format_sql_time_abs(micros_abs: u128) -> String {
    let hours = micros_abs / 3_600_000_000;
    let mins = (micros_abs % 3_600_000_000) / 60_000_000;
    let secs = (micros_abs % 60_000_000) / 1_000_000;
    let frac = micros_abs % 1_000_000;
    if frac == 0 {
        format!("{hours}:{mins:02}:{secs:02}")
    } else {
        format!(
            "{hours}:{mins:02}:{}",
            format_seconds_with_fraction(secs, frac)
        )
    }
}

fn format_sql_clock(hour: i128, minute: u128, second: i128, fraction: i128) -> String {
    let second_abs = second.unsigned_abs();
    let fraction_abs = fraction.unsigned_abs();
    if fraction_abs == 0 {
        format!("{hour}:{minute:02}:{second_abs:02}")
    } else {
        format!(
            "{hour}:{minute:02}:{}",
            format_seconds_with_fraction(second_abs, fraction_abs)
        )
    }
}

fn micros_hours(micros: i128) -> i128 {
    let abs = micros.unsigned_abs();
    let hours = (abs / 3_600_000_000) as i128;
    if micros < 0 {
        -hours
    } else {
        hours
    }
}

fn micros_minutes(micros: i128) -> i128 {
    let abs = micros.unsigned_abs();
    let minutes = ((abs % 3_600_000_000) / 60_000_000) as i128;
    if micros < 0 {
        -minutes
    } else {
        minutes
    }
}

fn micros_seconds(micros: i128) -> i128 {
    let abs = micros.unsigned_abs();
    let seconds = ((abs % 60_000_000) / 1_000_000) as i128;
    if micros < 0 {
        -seconds
    } else {
        seconds
    }
}

fn micros_fraction(micros: i128) -> i128 {
    let abs = micros.unsigned_abs();
    let fraction = (abs % 1_000_000) as i128;
    if micros < 0 {
        -fraction
    } else {
        fraction
    }
}

fn format_iso_8601_interval(iv: &IntervalValue) -> String {
    let parts = SignedIntervalParts::from_interval(iv);
    if parts.is_zero() {
        return "PT0S".to_owned();
    }

    let mut out = String::from("P");
    let abs_months = parts.months.unsigned_abs();
    let years = abs_months / 12;
    let mons = abs_months % 12;
    append_iso_component(&mut out, signum(parts.months), years, "Y");
    append_iso_component(&mut out, signum(parts.months), mons, "M");
    append_iso_component(&mut out, signum(parts.days), parts.days.unsigned_abs(), "D");

    if parts.micros != 0 {
        out.push('T');
        let micros_abs = parts.micros.unsigned_abs();
        let hours = micros_abs / 3_600_000_000;
        let mins = (micros_abs % 3_600_000_000) / 60_000_000;
        let secs = (micros_abs % 60_000_000) / 1_000_000;
        let frac = micros_abs % 1_000_000;
        let sign = signum(parts.micros);
        append_iso_component(&mut out, sign, hours, "H");
        append_iso_component(&mut out, sign, mins, "M");
        if frac != 0 || secs != 0 || (hours == 0 && mins == 0) {
            let sign_prefix = if sign < 0 { "-" } else { "" };
            if frac == 0 {
                out.push_str(&format!("{sign_prefix}{secs}S"));
            } else {
                out.push_str(&format!(
                    "{sign_prefix}{}S",
                    format_iso_seconds_with_fraction(secs, frac)
                ));
            }
        }
    }

    out
}

fn append_iso_component(out: &mut String, sign: i8, value: u128, suffix: &str) {
    if value == 0 {
        return;
    }
    if sign < 0 {
        out.push('-');
    }
    out.push_str(&value.to_string());
    out.push_str(suffix);
}

fn append_verbose_months(parts: &mut Vec<String>, months: i128, mixed_signs: bool) {
    if months == 0 {
        return;
    }

    let abs_months = months.unsigned_abs();
    let years = abs_months / 12;
    let mons = abs_months % 12;
    let sign = verbose_sign_prefix(months, mixed_signs);
    if years != 0 {
        parts.push(format!(
            "{sign}{years} {}",
            if years == 1 { "year" } else { "years" }
        ));
    }
    if mons != 0 {
        parts.push(format!(
            "{sign}{mons} {}",
            if mons == 1 { "mon" } else { "mons" }
        ));
    }
}

fn append_verbose_days(parts: &mut Vec<String>, days: i128, mixed_signs: bool) {
    if days == 0 {
        return;
    }

    parts.push(format!(
        "{}{} {}",
        verbose_sign_prefix(days, mixed_signs),
        days.unsigned_abs(),
        if days.unsigned_abs() == 1 {
            "day"
        } else {
            "days"
        }
    ));
}

fn append_verbose_time(parts: &mut Vec<String>, micros: i128, mixed_signs: bool) {
    if micros == 0 {
        return;
    }

    let sign = verbose_sign_prefix(micros, mixed_signs);
    let total_micros = micros.unsigned_abs();
    let hours = total_micros / 3_600_000_000;
    let mins = (total_micros % 3_600_000_000) / 60_000_000;
    let secs = (total_micros % 60_000_000) / 1_000_000;
    let frac = total_micros % 1_000_000;

    if hours != 0 {
        parts.push(format!(
            "{sign}{hours} {}",
            if hours == 1 { "hour" } else { "hours" }
        ));
    }
    if mins != 0 {
        parts.push(format!(
            "{sign}{mins} {}",
            if mins == 1 { "min" } else { "mins" }
        ));
    }
    if secs != 0 || frac != 0 {
        if frac == 0 {
            parts.push(format!(
                "{sign}{secs} {}",
                if secs == 1 { "sec" } else { "secs" }
            ));
        } else {
            parts.push(format!(
                "{sign}{} secs",
                format_verbose_seconds_with_fraction(secs, frac)
            ));
        }
    }
}

fn verbose_sign_prefix(value: i128, mixed_signs: bool) -> &'static str {
    if mixed_signs && value < 0 {
        "-"
    } else {
        ""
    }
}

fn format_postgres_date_part(
    abs_value: i128,
    sign: i8,
    last_date_sign: i8,
    singular: &str,
    plural: &str,
) -> String {
    let prefix = if sign < 0 {
        "-"
    } else if last_date_sign < 0 {
        "+"
    } else {
        ""
    };
    let unit = if sign > 0 && abs_value == 1 {
        singular
    } else {
        plural
    };
    format!("{prefix}{abs_value} {unit}")
}

fn leading_nonzero_sign(parts: SignedIntervalParts) -> i8 {
    if parts.months != 0 {
        return signum(parts.months);
    }
    if parts.days != 0 {
        return signum(parts.days);
    }
    signum(parts.micros)
}

fn has_mixed_signs(parts: SignedIntervalParts) -> bool {
    let mut saw_positive = false;
    let mut saw_negative = false;
    for value in [parts.months, parts.days, parts.micros] {
        if value > 0 {
            saw_positive = true;
        } else if value < 0 {
            saw_negative = true;
        }
    }
    saw_positive && saw_negative
}

fn signum(value: i128) -> i8 {
    if value < 0 {
        -1
    } else if value > 0 {
        1
    } else {
        0
    }
}

fn format_hms(micros_abs: u128, pad_hours_two_digits: bool) -> String {
    let hours = micros_abs / 3_600_000_000;
    let mins = (micros_abs % 3_600_000_000) / 60_000_000;
    let secs = (micros_abs % 60_000_000) / 1_000_000;
    let frac = micros_abs % 1_000_000;
    let hour_part = if pad_hours_two_digits {
        format!("{hours:02}")
    } else {
        hours.to_string()
    };

    if frac == 0 {
        format!("{hour_part}:{mins:02}:{secs:02}")
    } else {
        format!(
            "{hour_part}:{mins:02}:{}",
            format_seconds_with_fraction(secs, frac)
        )
    }
}

fn format_seconds_with_fraction(secs: u128, frac: u128) -> String {
    let frac_text = format!("{frac:06}");
    let trimmed = frac_text.trim_end_matches('0');
    format!("{secs:02}.{trimmed}")
}

fn format_verbose_seconds_with_fraction(secs: u128, frac: u128) -> String {
    let frac_text = format!("{frac:06}");
    let trimmed = frac_text.trim_end_matches('0');
    format!("{secs}.{trimmed}")
}

fn format_iso_seconds_with_fraction(secs: u128, frac: u128) -> String {
    let frac_text = format!("{frac:06}");
    let trimmed = frac_text.trim_end_matches('0');
    format!("{secs}.{trimmed}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postgres_time_sign_follows_last_negative_date_part() {
        let rendered = format_pg_interval_postgres(&IntervalValue::new(-10, 1, 85_512_340_000));
        assert_eq!(rendered, "-10 mons +1 day 23:45:12.34");
    }

    #[test]
    fn postgres_verbose_can_factor_leading_negative_into_ago() {
        let rendered = format_pg_interval_verbose(&IntervalValue::new(-10, -3, 14_106_700_000));
        assert_eq!(rendered, "@ 10 mons 3 days -3 hours -55 mins -6.7 secs ago");
    }

    #[test]
    fn sql_standard_formats_explicit_mixed_groups() {
        let rendered = format_sql_standard_interval(&IntervalValue::new(14, -3, 14_706_789_000));
        assert_eq!(rendered, "+1-2 -3 +4:05:06.789");
    }

    #[test]
    fn sql_standard_formats_pure_day_time_without_year_month_group() {
        let rendered = format_sql_standard_interval(&IntervalValue::new(0, -1, 85_512_340_000));
        assert_eq!(rendered, "-1 23:45:12.34");
    }

    #[test]
    fn sql_standard_formats_mixed_day_time_with_explicit_signs() {
        let rendered = format_sql_standard_interval(&IntervalValue::new(0, -1, 3_600_000_000));
        assert_eq!(rendered, "+0-0 -1 +1:00:00");
    }

    #[test]
    fn iso_8601_formats_extreme_negative_components() {
        let rendered = format_iso_8601_interval(&IntervalValue::new(i32::MIN, i32::MIN, i64::MIN));
        assert_eq!(
            rendered,
            "P-178956970Y-8M-2147483648DT-2562047788H-54.775808S"
        );
    }
}
