use aiondb_core::{
    temporal::{is_infinity_date, timestamp_infinity_label},
    DateOrder, DateStyleFamily, DateStyleSetting, TimeZoneSetting,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionFormat {
    date_style: DateStyleSetting,
    timezone: TimeZoneSetting,
}

impl Default for SessionFormat {
    fn default() -> Self {
        Self {
            date_style: DateStyleSetting::parse("Postgres, MDY"),
            timezone: TimeZoneSetting::parse("PST8PDT"),
        }
    }
}

impl SessionFormat {
    pub(crate) fn apply_sql(&mut self, sql: &str) {
        let collapsed = collapse_sql(sql);

        if collapsed.starts_with("reset datestyle") {
            self.date_style = DateStyleSetting::parse("Postgres, MDY");
            return;
        }
        if collapsed.starts_with("set datestyle") {
            if let Some(value) = extract_setting_value(sql) {
                self.date_style = DateStyleSetting::parse_with_base(&value, self.date_style);
            }
            return;
        }

        if collapsed.starts_with("reset timezone") || collapsed.starts_with("reset time zone") {
            self.timezone = TimeZoneSetting::parse("PST8PDT");
            return;
        }
        if collapsed.starts_with("set timezone") || collapsed.starts_with("set time zone") {
            if let Some(value) = extract_setting_value(sql) {
                self.timezone = TimeZoneSetting::parse(&value);
            }
        }
    }

    pub(crate) fn format_date(&self, date: time::Date) -> String {
        if is_infinity_date(date) {
            return if date.year() < 0 {
                "-infinity".to_owned()
            } else {
                "infinity".to_owned()
            };
        }
        format_date_by_style(date, self.date_style)
    }

    pub(crate) fn format_timestamp(&self, dt: &time::PrimitiveDateTime) -> String {
        if let Some(sentinel) = timestamp_sentinel(dt.date(), dt.time()) {
            return sentinel.to_owned();
        }
        format_timestamp_by_style(dt.date(), dt.time(), self.date_style, None)
    }

    pub(crate) fn format_timestamptz(&self, odt: &time::OffsetDateTime) -> String {
        if let Some(sentinel) = timestamp_sentinel(odt.date(), odt.time()) {
            return sentinel.to_owned();
        }
        let (offset, label) = self.timezone.parts_for_utc(*odt);
        let local = odt.to_offset(offset);
        let zone = match self.date_style.family() {
            DateStyleFamily::Iso | DateStyleFamily::Sql => Some(format_offset_label(offset)),
            DateStyleFamily::Postgres | DateStyleFamily::German => Some(label),
        };
        format_timestamp_by_style(local.date(), local.time(), self.date_style, zone.as_deref())
    }

    pub(crate) fn format_timetz(&self, time: &time::Time, offset: &time::UtcOffset) -> String {
        let _ = self;
        format!("{}{}", format_time(*time), format_offset_label(*offset))
    }

    pub(crate) fn format_end_of_day_time(&self) -> String {
        let _ = self;
        "24:00:00".to_owned()
    }

    pub(crate) fn format_end_of_day_timetz(&self, offset: &time::UtcOffset) -> String {
        format!("24:00:00{}", format_offset_label(*offset))
    }
}

const DOW_NAMES: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const MONTH_NAMES: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn collapse_sql(sql: &str) -> String {
    sql.split_whitespace()
        .map(|part| part.trim_matches(';').to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(" ")
}

fn extract_setting_value(sql: &str) -> Option<String> {
    let trimmed = sql.trim().trim_end_matches(';').trim();
    if let Some((_, rest)) = trimmed.split_once('=') {
        return Some(unquote_setting(rest.trim()));
    }
    let lower = trimmed.to_ascii_lowercase();
    if let Some(idx) = lower.find(" to ") {
        return Some(unquote_setting(trimmed[idx + 4..].trim()));
    }
    for prefix in [
        "set time zone ",
        "set timezone ",
        "set datestyle ",
        "set intervalstyle ",
        "set local intervalstyle ",
    ] {
        if lower.starts_with(prefix) {
            return Some(unquote_setting(trimmed[prefix.len()..].trim()));
        }
    }
    None
}

fn unquote_setting(value: &str) -> String {
    value.trim_matches('\'').trim_matches('"').trim().to_owned()
}

fn timestamp_sentinel(date: time::Date, time: time::Time) -> Option<&'static str> {
    timestamp_infinity_label(date, time)
}

fn format_date_by_style(date: time::Date, style: DateStyleSetting) -> String {
    let (year, bc) = display_year(date.year());
    let month = date.month() as u8;
    let day = date.day();
    let body = match style.family() {
        DateStyleFamily::Postgres => match style.order() {
            DateOrder::Mdy => format!("{month:02}-{day:02}-{year:04}"),
            DateOrder::Dmy => format!("{day:02}-{month:02}-{year:04}"),
            DateOrder::Ymd => format!("{year:04}-{month:02}-{day:02}"),
        },
        DateStyleFamily::Iso => format!("{year:04}-{month:02}-{day:02}"),
        DateStyleFamily::Sql => match style.order() {
            DateOrder::Mdy => format!("{month:02}/{day:02}/{year:04}"),
            DateOrder::Dmy => format!("{day:02}/{month:02}/{year:04}"),
            DateOrder::Ymd => format!("{year:04}/{month:02}/{day:02}"),
        },
        DateStyleFamily::German => format!("{day:02}.{month:02}.{year:04}"),
    };
    if bc {
        format!("{body} BC")
    } else {
        body
    }
}

fn format_timestamp_by_style(
    date: time::Date,
    time: time::Time,
    style: DateStyleSetting,
    zone_label: Option<&str>,
) -> String {
    let (year, bc) = display_year(date.year());
    let month = date.month() as u8;
    let day = date.day();
    let dow = DOW_NAMES[date.weekday().number_days_from_monday() as usize];
    let month_name = MONTH_NAMES[(date.month() as usize) - 1];
    let time_part = format_time(time);

    let body = match style.family() {
        DateStyleFamily::Postgres => match style.order() {
            DateOrder::Mdy => format!("{dow} {month_name} {day:02} {time_part} {year:04}"),
            DateOrder::Dmy => format!("{dow} {day:02} {month_name} {time_part} {year:04}"),
            DateOrder::Ymd => format!("{dow} {year:04} {month_name} {day:02} {time_part}"),
        },
        DateStyleFamily::Iso => format!("{year:04}-{month:02}-{day:02} {time_part}"),
        DateStyleFamily::Sql => match style.order() {
            DateOrder::Mdy => format!("{month:02}/{day:02}/{year:04} {time_part}"),
            DateOrder::Dmy => format!("{day:02}/{month:02}/{year:04} {time_part}"),
            DateOrder::Ymd => format!("{year:04}/{month:02}/{day:02} {time_part}"),
        },
        DateStyleFamily::German => format!("{day:02}.{month:02}.{year:04} {time_part}"),
    };

    let with_zone = match zone_label {
        Some(label) => format!("{body} {label}"),
        None => body,
    };
    if bc {
        format!("{with_zone} BC")
    } else {
        with_zone
    }
}

fn display_year(year: i32) -> (i32, bool) {
    if year <= 0 {
        (1 - year, true)
    } else {
        (year, false)
    }
}

fn format_time(time: time::Time) -> String {
    let micros = time.microsecond();
    if micros == 0 {
        format!(
            "{:02}:{:02}:{:02}",
            time.hour(),
            time.minute(),
            time.second()
        )
    } else {
        let fraction = format!("{micros:06}");
        let trimmed = fraction.trim_end_matches('0');
        format!(
            "{:02}:{:02}:{:02}.{trimmed}",
            time.hour(),
            time.minute(),
            time.second()
        )
    }
}

fn format_offset_label(offset: time::UtcOffset) -> String {
    let total_seconds = offset.whole_seconds();
    let sign = if total_seconds < 0 { '-' } else { '+' };
    let total_seconds = total_seconds.unsigned_abs();
    let hours = total_seconds / 3_600;
    let minutes = (total_seconds % 3_600) / 60;
    if minutes == 0 {
        format!("{sign}{hours:02}")
    } else {
        format!("{sign}{hours:02}:{minutes:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_set_time_zone_without_to_keyword() {
        assert_eq!(
            extract_setting_value("SET TIME ZONE 'CST7CDT,M4.1.0,M10.5.0';"),
            Some("CST7CDT,M4.1.0,M10.5.0".to_owned())
        );
    }
}
