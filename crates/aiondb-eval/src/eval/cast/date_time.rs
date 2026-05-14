#![allow(
    clippy::manual_range_contains,
    clippy::match_result_ok,
    clippy::doc_markdown
)]

pub(super) use super::date_time_helpers::*;
use aiondb_core::{DateOrder, PgDate, Value};

pub(super) const MONTHS: [(&str, u8); 12] = [
    ("jan", 1),
    ("feb", 2),
    ("mar", 3),
    ("apr", 4),
    ("may", 5),
    ("jun", 6),
    ("jul", 7),
    ("aug", 8),
    ("sep", 9),
    ("oct", 10),
    ("nov", 11),
    ("dec", 12),
];

/// Case-insensitive ASCII `starts_with`. Lets the hyphenated-month-name
/// date parser scan its argument against the `MONTHS` table without
/// allocating a `to_ascii_lowercase()` copy of every input field on
/// every text→date cast.
#[inline]
fn starts_with_ascii_ci(s: &str, prefix: &str) -> bool {
    let p = prefix.as_bytes();
    let bytes = s.as_bytes();
    bytes.len() >= p.len() && bytes[..p.len()].eq_ignore_ascii_case(p)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DateParseError {
    Invalid,
    OutOfRange,
    Datestyle,
    FieldOutOfRange,
}

/// Parse a date string supporting many PostgreSQL formats.
pub(super) fn parse_date_components(s: &str) -> Result<time::Date, ()> {
    // Handle special values
    if s.eq_ignore_ascii_case("epoch") {
        return time::Date::from_calendar_date(1970, time::Month::January, 1).map_err(|_| ());
    }

    // Strip BC suffix for later negation
    let (s_clean, is_bc) = strip_bc(s);
    let s = s_clean.trim();

    if is_bc {
        if let Some(date) = try_parse_bc_numeric_date(s) {
            return Ok(date);
        }
    }

    // Try YYYY-MM-DD or MM-DD-YYYY with hyphens
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() == 3 {
        if let (Ok(a), Ok(b), Ok(c)) = (
            parts[0].parse::<i32>(),
            parts[1].parse::<u8>(),
            parts[2].parse::<i32>(),
        ) {
            // Determine which part is the year based on value/length
            if parts[0].len() >= 4 || (a > 31 && c <= 31) {
                // YYYY-MM-DD: c is the day, must fit in u8 (0..=31 already checked above).
                if let Ok(day) = u8::try_from(c) {
                    return calendar_date_with_era(
                        normalize_year_token(parts[0], a),
                        b,
                        day,
                        is_bc,
                    );
                }
            }
            if parts[2].len() >= 4 || c > 31 {
                // Month/day ordering follows the current DateStyle session order.
                if let Ok(a_u8) = u8::try_from(a) {
                    if let Ok(date) = ordered_month_day_year(a_u8, b, c, is_bc) {
                        return Ok(date);
                    }
                }
            }
            // Default to YY-MM-DD
            if let Ok(c_u8) = u8::try_from(c) {
                return calendar_date_with_era(infer_century(a), b, c_u8, is_bc);
            }
        }
        // Try DD-Mon-YYYY or Mon-DD-YYYY with hyphens
        if let Some(d) = try_parse_hyphenated_month_name(parts[0], parts[1], parts[2]) {
            return if is_bc { apply_bc_to_date(d) } else { Ok(d) };
        }
    }

    // Try slash-separated: YYYY/MM/DD, MM/DD/YYYY, DD/MM/YYYY, or Mon/DD/YYYY
    let slash_parts: Vec<&str> = s.split('/').collect();
    if slash_parts.len() == 3 {
        // Try YYYY/MM/DD first (year is >255 or 4+ digits)
        if let (Ok(year), Ok(month_num), Ok(day)) = (
            slash_parts[0].parse::<i32>(),
            slash_parts[1].parse::<u8>(),
            slash_parts[2].parse::<u8>(),
        ) {
            if slash_parts[0].len() >= 4 || year > 31 {
                if let Ok(month) = time::Month::try_from(month_num) {
                    if let Ok(d) = time::Date::from_calendar_date(
                        normalize_year_token(slash_parts[0], year),
                        month,
                        day,
                    ) {
                        return Ok(d);
                    }
                }
            }
        }
        if let (Ok(a), Ok(b), Ok(year)) = (
            slash_parts[0].parse::<u8>(),
            slash_parts[1].parse::<u8>(),
            slash_parts[2].parse::<i32>(),
        ) {
            if let Ok(date) = ordered_month_day_year(a, b, year, is_bc) {
                return Ok(date);
            }
        }
        // Try YY/Mon/DD or Mon/DD/YYYY with month names
        if let Some(d) =
            try_parse_hyphenated_month_name(slash_parts[0], slash_parts[1], slash_parts[2])
        {
            return if is_bc { apply_bc_to_date(d) } else { Ok(d) };
        }
    }

    // Try dot-separated: YYYY.MM.DD, DD.MM.YYYY, or MM.DD.YYYY
    let dot_parts: Vec<&str> = s.split('.').collect();
    if dot_parts.len() == 3 {
        if let (Ok(a), Ok(b), Ok(c)) = (
            dot_parts[0].parse::<i32>(),
            dot_parts[1].parse::<u8>(),
            dot_parts[2].parse::<i32>(),
        ) {
            // YYYY.MM.DD (year first if 4 digits or > 31)
            if dot_parts[0].len() >= 4 || a > 31 {
                if let Ok(month) = time::Month::try_from(b) {
                    if let Ok(day) = u8::try_from(c) {
                        if let Ok(d) = time::Date::from_calendar_date(
                            normalize_year_token(dot_parts[0], a),
                            month,
                            day,
                        ) {
                            return Ok(d);
                        }
                    }
                }
            }
            if let Ok(a_u8) = u8::try_from(a) {
                if let Ok(date) = ordered_month_day_year(a_u8, b, c, is_bc) {
                    return Ok(date);
                }
            }
        }
    }

    // Try YYYY.DDD (ordinal day)
    if dot_parts.len() == 2 {
        if let (Ok(year), Ok(day_of_year)) =
            (dot_parts[0].parse::<i32>(), dot_parts[1].parse::<u16>())
        {
            if day_of_year >= 1 && day_of_year <= 366 {
                return time::Date::from_ordinal_date(infer_century(year), day_of_year)
                    .map_err(|_| ());
            }
        }
    }

    // Try compact YYYYMMDD or YYMMDD
    if s.len() == 8 && s.chars().all(|c| c.is_ascii_digit()) {
        let year: i32 = s[..4].parse().map_err(|_| ())?;
        let month: u8 = s[4..6].parse().map_err(|_| ())?;
        let day: u8 = s[6..8].parse().map_err(|_| ())?;
        let month = time::Month::try_from(month).map_err(|_| ())?;
        return time::Date::from_calendar_date(year, month, day).map_err(|_| ());
    }
    // Extended compact format: Y...YMMDD (year width > 4)
    if s.len() > 8 && s.chars().all(|c| c.is_ascii_digit()) {
        let year: i32 = s[..s.len() - 4].parse().map_err(|_| ())?;
        let month: u8 = s[s.len() - 4..s.len() - 2].parse().map_err(|_| ())?;
        let day: u8 = s[s.len() - 2..].parse().map_err(|_| ())?;
        let month = time::Month::try_from(month).map_err(|_| ())?;
        return time::Date::from_calendar_date(year, month, day).map_err(|_| ());
    }
    if s.len() == 6 && s.chars().all(|c| c.is_ascii_digit()) {
        let year: i32 = s[..2].parse().map_err(|_| ())?;
        let month: u8 = s[2..4].parse().map_err(|_| ())?;
        let day: u8 = s[4..6].parse().map_err(|_| ())?;
        let month = time::Month::try_from(month).map_err(|_| ())?;
        return time::Date::from_calendar_date(infer_century(year), month, day).map_err(|_| ());
    }

    // Try Julian date (J followed by number)
    if let Some(rest) = s.strip_prefix('J') {
        if let Ok(jd) = rest.parse::<i64>() {
            return julian_to_date(jd);
        }
    }

    // Try month-name formats like "Jan 01" or "January 1".
    if let Some(date) = try_parse_month_name_date(s) {
        return if is_bc {
            apply_bc_to_date(date)
        } else {
            Ok(date)
        };
    }

    // Try space-separated numeric: "YYYY MM DD", "MM DD YYYY", "DD MM YYYY"
    let space_parts: Vec<&str> = s.split_whitespace().collect();
    if space_parts.len() == 3
        && space_parts
            .iter()
            .all(|p| p.chars().all(|c| c.is_ascii_digit()))
    {
        // All three parts are numeric
        let a: i32 = space_parts[0].parse().map_err(|_| ())?;
        let b: u8 = space_parts[1].parse().map_err(|_| ())?;
        let c: i32 = space_parts[2].parse().map_err(|_| ())?;

        // If first part looks like year (>31 or 4 digits)
        if a > 31 || space_parts[0].len() == 4 {
            // YYYY MM DD
            if let Ok(c_u8) = u8::try_from(c) {
                return calendar_date_with_era(a, b, c_u8, is_bc);
            }
        }
        // If last part looks like year (>31 or 4 digits)
        if c > 31 || space_parts[2].len() >= 4 {
            if let Ok(a_u8) = u8::try_from(a) {
                if let Ok(date) = ordered_month_day_year(a_u8, b, c, is_bc) {
                    return Ok(date);
                }
            }
        }
        // Two-digit year at end: assume MM DD YY
        if space_parts[2].len() <= 2 && a <= 12 {
            if let Ok(a_u8) = u8::try_from(a) {
                return ordered_month_day_year(a_u8, b, c, is_bc);
            }
        }
    }

    Err(())
}

fn session_date_order() -> DateOrder {
    crate::eval::session::current_date_order()
}

fn calendar_date_with_era(year: i32, month: u8, day: u8, is_bc: bool) -> Result<time::Date, ()> {
    let year = if is_bc { 1 - year } else { year };
    let month = time::Month::try_from(month).map_err(|_| ())?;
    time::Date::from_calendar_date(year, month, day).map_err(|_| ())
}

fn ordered_month_day_year(first: u8, second: u8, year: i32, is_bc: bool) -> Result<time::Date, ()> {
    let year = infer_century(year);
    match session_date_order() {
        DateOrder::Dmy => calendar_date_with_era(year, second, first, is_bc),
        DateOrder::Mdy | DateOrder::Ymd => calendar_date_with_era(year, first, second, is_bc),
    }
}

fn normalize_year_token(token: &str, year: i32) -> i32 {
    if token.trim_start_matches('-').len() <= 2 {
        infer_century(year)
    } else {
        year
    }
}

fn apply_bc_to_date(date: time::Date) -> Result<time::Date, ()> {
    time::Date::from_calendar_date(1 - date.year(), date.month(), date.day()).map_err(|_| ())
}

fn try_parse_bc_numeric_date(s: &str) -> Option<time::Date> {
    let separators = ['-', '/', '.'];
    let separator = separators
        .into_iter()
        .find(|candidate| s.contains(*candidate))?;
    let parts: Vec<&str> = s.split(separator).collect();
    if parts.len() != 3 {
        return None;
    }

    let year: i32 = parts[0].parse().ok()?;
    let month: u8 = parts[1].parse().ok()?;
    let day: u8 = parts[2].parse().ok()?;
    calendar_date_with_era(year, month, day, true).ok()
}

fn try_parse_hyphenated_month_name(a: &str, b: &str, c: &str) -> Option<time::Date> {
    // Mon-DD-YYYY (e.g., Jan-08-1999)
    for &(name, num) in &MONTHS {
        if starts_with_ascii_ci(a, name) {
            if let (Some(day), Some(year)) = (b.parse::<u8>().ok(), c.parse::<i32>().ok()) {
                if let Ok(month) = time::Month::try_from(num) {
                    if let Ok(d) = time::Date::from_calendar_date(infer_century(year), month, day) {
                        return Some(d);
                    }
                }
            }
        }
    }
    // YYYY-Mon-DD or YY-Mon-DD (e.g., 1999-Jan-08, 99-Jan-08)
    // Try this before DD-Mon-YYYY so that "99-Jan-08" parses as year=99, day=08
    // rather than day=99 (which would fail).
    if let Ok(year) = a.parse::<i32>() {
        for &(name, num) in &MONTHS {
            if starts_with_ascii_ci(b, name) {
                if let Some(day) = c.parse::<u8>().ok() {
                    if let Ok(month) = time::Month::try_from(num) {
                        if let Ok(d) =
                            time::Date::from_calendar_date(infer_century(year), month, day)
                        {
                            return Some(d);
                        }
                    }
                }
            }
        }
    }
    // DD-Mon-YYYY (e.g., 08-Jan-1999)
    if let Ok(day) = a.parse::<u8>() {
        for &(name, num) in &MONTHS {
            if starts_with_ascii_ci(b, name) {
                if let Some(year) = c.parse::<i32>().ok() {
                    if let Ok(month) = time::Month::try_from(num) {
                        if let Ok(d) =
                            time::Date::from_calendar_date(infer_century(year), month, day)
                        {
                            return Some(d);
                        }
                    }
                }
            }
        }
    }
    // YY-DD-Mon (e.g., 99-08-Jan) - rare but tested
    if let Ok(raw_year) = a.parse::<i32>() {
        for &(name, num) in &MONTHS {
            if starts_with_ascii_ci(c, name) {
                if let Some(day) = b.parse::<u8>().ok() {
                    if let Ok(month) = time::Month::try_from(num) {
                        if let Ok(d) =
                            time::Date::from_calendar_date(infer_century(raw_year), month, day)
                        {
                            return Some(d);
                        }
                    }
                }
            }
        }
    }
    None
}

pub(super) fn try_parse_month_name_date(s: &str) -> Option<time::Date> {
    let s = s.replace(',', " ");
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() < 3 {
        return None;
    }

    // Mon DD YYYY (e.g., "Jan 08", "January 1")
    let lower = parts[0].to_ascii_lowercase();
    for &(name, num) in &MONTHS {
        if lower.starts_with(name) {
            if let (Some(day), Some(year)) =
                (parts[1].parse::<u8>().ok(), parts[2].parse::<i32>().ok())
            {
                if let Ok(month) = time::Month::try_from(num) {
                    if let Ok(d) = time::Date::from_calendar_date(infer_century(year), month, day) {
                        return Some(d);
                    }
                }
            }
        }
    }

    // YYYY Mon DD or YY Mon DD (e.g., "1999 Jan 08", "99 Jan 08")
    // Try this before DD Mon YYYY so that "99 Jan 08" parses as year=99, day=08.
    if let Ok(year) = parts[0].parse::<i32>() {
        let lower1 = parts[1].to_ascii_lowercase();
        for &(name, num) in &MONTHS {
            if lower1.starts_with(name) {
                if let Some(day) = parts[2].parse::<u8>().ok() {
                    if let Ok(month) = time::Month::try_from(num) {
                        if let Ok(d) =
                            time::Date::from_calendar_date(infer_century(year), month, day)
                        {
                            return Some(d);
                        }
                    }
                }
            }
        }
        // YYYY DD Mon (e.g., "1999 08 Jan")
        if let Ok(day) = parts[1].parse::<u8>() {
            let lower2 = parts[2].to_ascii_lowercase();
            for &(name, num) in &MONTHS {
                if lower2.starts_with(name) {
                    if let Ok(month) = time::Month::try_from(num) {
                        if let Ok(d) =
                            time::Date::from_calendar_date(infer_century(year), month, day)
                        {
                            return Some(d);
                        }
                    }
                }
            }
        }
    }

    // DD Mon YYYY (e.g., "08 Jan 1999")
    if let Ok(day) = parts[0].parse::<u8>() {
        let lower = parts[1].to_ascii_lowercase();
        for &(name, num) in &MONTHS {
            if lower.starts_with(name) {
                if let Some(year) = parts[2].parse::<i32>().ok() {
                    if let Ok(month) = time::Month::try_from(num) {
                        if let Ok(d) =
                            time::Date::from_calendar_date(infer_century(year), month, day)
                        {
                            return Some(d);
                        }
                    }
                }
            }
        }
    }

    None
}

pub(super) fn parse_date(s: &str) -> Result<Value, DateParseError> {
    if let Some(date) = parse_date_literal_strict(s)? {
        return Ok(Value::Date(date));
    }

    if let Ok(date) = parse_date_components(s) {
        if !pg_date_tuple_in_range(date.year(), date.month(), date.day()) {
            return Err(DateParseError::OutOfRange);
        }
        return Ok(Value::Date(date));
    }

    parse_large_pg_date(s).map(Value::LargeDate)
}

fn parse_date_literal_strict(raw: &str) -> Result<Option<time::Date>, DateParseError> {
    let (s_clean, is_bc) = strip_bc(raw);
    let trimmed = s_clean.trim();

    if let Some(date) = try_parse_numeric_date_literal(trimmed, is_bc)? {
        return Ok(Some(date));
    }
    if let Some(date) = try_parse_month_name_date_literal(trimmed, is_bc)? {
        return Ok(Some(date));
    }

    Ok(None)
}

fn try_parse_numeric_date_literal(
    trimmed: &str,
    is_bc: bool,
) -> Result<Option<time::Date>, DateParseError> {
    for separator in ['/', '-', ' '] {
        let parts: Vec<&str> = if separator == ' ' {
            trimmed.split_whitespace().collect()
        } else {
            trimmed.split(separator).collect()
        };
        if parts.len() != 3
            || parts.iter().any(|part| part.is_empty())
            || !parts
                .iter()
                .all(|part| part.chars().all(|ch| ch.is_ascii_digit()))
        {
            continue;
        }

        let [first, second, third] = [&parts[0], &parts[1], &parts[2]];
        let first_num = first.parse::<i32>().map_err(|_| DateParseError::Invalid)?;
        let second_num = second.parse::<u8>().map_err(|_| DateParseError::Invalid)?;
        let third_num = third.parse::<i32>().map_err(|_| DateParseError::Invalid)?;
        let order = session_date_order();

        // YYYY-MM-DD or YYYY MM DD
        if first.len() >= 4 {
            let third_u8 = u8::try_from(third_num).map_err(|_| DateParseError::FieldOutOfRange)?;
            return build_year_first_numeric_date(
                trimmed, first, first_num, second_num, third_u8, is_bc,
            );
        }

        // MM/DD/YYYY or DD/MM/YYYY depending DateStyle.
        if third.len() >= 4 || third_num > 31 {
            let first_u8 = u8::try_from(first_num).map_err(|_| DateParseError::FieldOutOfRange)?;
            return match order {
                DateOrder::Ymd => Err(DateParseError::Datestyle),
                DateOrder::Dmy => build_ambiguous_ordered_date(
                    third, third_num, second_num, first_u8, is_bc, true,
                ),
                DateOrder::Mdy => build_ambiguous_ordered_date(
                    third, third_num, first_u8, second_num, is_bc, true,
                ),
            }
            .map(Some);
        }

        let two_digit = first.len() <= 2 && third.len() <= 2;
        if !two_digit {
            continue;
        }

        if first_num > 31 && !matches!(order, DateOrder::Ymd) {
            return Err(DateParseError::Datestyle);
        }

        let first_u8 = u8::try_from(first_num).map_err(|_| DateParseError::FieldOutOfRange)?;
        let third_u8 = u8::try_from(third_num).map_err(|_| DateParseError::FieldOutOfRange)?;
        return match order {
            DateOrder::Ymd => {
                build_date_with_year_token(first, first_num, second_num, third_u8, is_bc)
            }
            DateOrder::Dmy => {
                build_date_with_year_token(third, third_num, second_num, first_u8, is_bc)
            }
            DateOrder::Mdy => {
                build_date_with_year_token(third, third_num, first_u8, second_num, is_bc)
            }
        }
        .map(Some);
    }

    Ok(None)
}

fn try_parse_month_name_date_literal(
    trimmed: &str,
    is_bc: bool,
) -> Result<Option<time::Date>, DateParseError> {
    let normalized = trimmed.replace(',', " ");
    let (parts, separator_is_space) = if normalized.split_whitespace().count() == 3 {
        (normalized.split_whitespace().collect::<Vec<_>>(), true)
    } else {
        let hyphen_parts = trimmed.split('-').collect::<Vec<_>>();
        if hyphen_parts.len() == 3 {
            (hyphen_parts, false)
        } else {
            return Ok(None);
        }
    };

    if parts.len() != 3 {
        return Ok(None);
    }

    let month_idx = parts.iter().position(|part| month_number(part).is_some());
    let Some(month_idx) = month_idx else {
        return Ok(None);
    };
    let month = month_number(parts[month_idx]).ok_or(DateParseError::Invalid)?;
    let order = session_date_order();

    let parse_num = |token: &str| token.parse::<i32>().map_err(|_| DateParseError::Invalid);

    match month_idx {
        0 => {
            let day = parts[1]
                .parse::<u8>()
                .map_err(|_| DateParseError::Invalid)?;
            let year = parse_num(parts[2])?;
            if parts[2].len() >= 4 {
                build_date_with_year_token(parts[2], year, month, day, is_bc).map(Some)
            } else if matches!(order, DateOrder::Ymd) {
                Err(DateParseError::Datestyle)
            } else {
                build_date_with_year_token_no_bc_century(parts[2], year, month, day, is_bc)
                    .map(Some)
            }
        }
        1 => {
            let first = parse_num(parts[0])?;
            let third = parse_num(parts[2])?;
            if parts[0].len() >= 4 {
                let third_u8 = u8::try_from(third).map_err(|_| DateParseError::FieldOutOfRange)?;
                build_date_with_year_token(parts[0], first, month, third_u8, is_bc).map(Some)
            } else if parts[2].len() >= 4 {
                let first_u8 = u8::try_from(first).map_err(|_| DateParseError::FieldOutOfRange)?;
                build_date_with_year_token(parts[2], third, month, first_u8, is_bc).map(Some)
            } else if matches!(order, DateOrder::Ymd) {
                let third_u8 = u8::try_from(third).map_err(|_| DateParseError::FieldOutOfRange)?;
                build_ambiguous_year_month_day(parts[0], first, month, third_u8, is_bc).map(Some)
            } else if separator_is_space && matches!(order, DateOrder::Mdy) && first > 31 {
                Err(DateParseError::Invalid)
            } else {
                let first_u8 = u8::try_from(first).map_err(|_| DateParseError::FieldOutOfRange)?;
                build_ambiguous_day_month_year(parts[2], third, month, first_u8, is_bc, true)
                    .map(Some)
            }
        }
        2 => {
            if !separator_is_space {
                return Err(DateParseError::Invalid);
            }
            let first = parse_num(parts[0])?;
            let second = parts[1]
                .parse::<u8>()
                .map_err(|_| DateParseError::Invalid)?;
            if parts[0].len() >= 4 {
                build_date_with_year_token(parts[0], first, month, second, is_bc).map(Some)
            } else if matches!(order, DateOrder::Ymd) {
                build_ambiguous_year_month_day(parts[0], first, month, second, is_bc).map(Some)
            } else {
                Err(DateParseError::Invalid)
            }
        }
        _ => Ok(None),
    }
}

fn build_ambiguous_ordered_date(
    year_token: &str,
    year: i32,
    month: u8,
    day: u8,
    is_bc: bool,
    datestyle_on_component_overflow: bool,
) -> Result<time::Date, DateParseError> {
    if datestyle_on_component_overflow && (month > 12 || day > 31) {
        return Err(DateParseError::Datestyle);
    }
    build_date_with_year_token(year_token, year, month, day, is_bc)
}

fn build_ambiguous_year_month_day(
    year_token: &str,
    year: i32,
    month: u8,
    day: u8,
    is_bc: bool,
) -> Result<time::Date, DateParseError> {
    if month > 12 || day > 31 {
        return Err(DateParseError::Datestyle);
    }
    build_date_with_year_token(year_token, year, month, day, is_bc)
}

fn build_ambiguous_day_month_year(
    year_token: &str,
    year: i32,
    month: u8,
    day: u8,
    is_bc: bool,
    datestyle_on_component_overflow: bool,
) -> Result<time::Date, DateParseError> {
    if datestyle_on_component_overflow && day > 31 {
        return Err(DateParseError::Datestyle);
    }
    build_date_with_year_token_no_bc_century(year_token, year, month, day, is_bc)
}

fn build_date_with_year_token(
    year_token: &str,
    year: i32,
    month: u8,
    day: u8,
    is_bc: bool,
) -> Result<time::Date, DateParseError> {
    build_date_impl(year_token, year, month, day, is_bc, true)
}

fn build_date_with_year_token_no_bc_century(
    year_token: &str,
    year: i32,
    month: u8,
    day: u8,
    is_bc: bool,
) -> Result<time::Date, DateParseError> {
    build_date_impl(year_token, year, month, day, is_bc, false)
}

fn build_year_first_numeric_date(
    raw: &str,
    year_token: &str,
    year: i32,
    month: u8,
    day: u8,
    is_bc: bool,
) -> Result<Option<time::Date>, DateParseError> {
    match build_date_with_year_token(year_token, year, month, day, is_bc) {
        Ok(date) => Ok(Some(date)),
        Err(DateParseError::FieldOutOfRange) if !is_bc && parse_large_pg_date(raw).is_ok() => {
            Ok(None)
        }
        Err(err) => Err(err),
    }
}

fn build_date_impl(
    year_token: &str,
    year: i32,
    month: u8,
    day: u8,
    is_bc: bool,
    infer_short_year_for_bc: bool,
) -> Result<time::Date, DateParseError> {
    let month_enum = time::Month::try_from(month).map_err(|_| DateParseError::FieldOutOfRange)?;
    let normalized_year = normalize_literal_year(year_token, year, is_bc, infer_short_year_for_bc);
    let astronomical_year = if is_bc {
        1 - normalized_year
    } else {
        normalized_year
    };

    if !pg_date_tuple_in_range(astronomical_year, month_enum, day) {
        return Err(DateParseError::OutOfRange);
    }

    match time::Date::from_calendar_date(astronomical_year, month_enum, day) {
        Ok(date) => Ok(date),
        Err(_)
            if !is_bc
                && time::Date::from_calendar_date(normalized_year, month_enum, day).is_err() =>
        {
            Err(DateParseError::FieldOutOfRange)
        }
        Err(_) => Err(DateParseError::FieldOutOfRange),
    }
}

fn normalize_literal_year(
    year_token: &str,
    year: i32,
    is_bc: bool,
    infer_short_year_for_bc: bool,
) -> i32 {
    let short_year = year_token.trim_start_matches('-').len() <= 2;
    if short_year && (!is_bc || infer_short_year_for_bc) {
        infer_century(year)
    } else {
        year
    }
}

fn month_number(token: &str) -> Option<u8> {
    let lower = token.to_ascii_lowercase();
    MONTHS
        .iter()
        .find(|(name, _)| lower.starts_with(name))
        .map(|(_, month)| *month)
}

fn parse_large_pg_date(raw: &str) -> Result<PgDate, DateParseError> {
    let (s_clean, is_bc) = strip_bc(raw);
    if is_bc {
        return Err(DateParseError::Invalid);
    }

    let trimmed = s_clean.trim();
    let parts: Vec<&str> = trimmed.split('-').collect();
    if parts.len() != 3 {
        return Err(DateParseError::Invalid);
    }

    let year_text = parts[0];
    if year_text.len() < 4 || !year_text.chars().all(|ch| ch.is_ascii_digit()) {
        return Err(DateParseError::Invalid);
    }

    let year = year_text
        .parse::<i32>()
        .map_err(|_| DateParseError::Invalid)?;
    let month_num = parts[1]
        .parse::<u8>()
        .map_err(|_| DateParseError::Invalid)?;
    let day = parts[2]
        .parse::<u8>()
        .map_err(|_| DateParseError::Invalid)?;
    let month = time::Month::try_from(month_num).map_err(|_| DateParseError::Invalid)?;

    if !pg_date_in_range(year, month, day) {
        return Err(DateParseError::OutOfRange);
    }

    if time::Date::from_calendar_date(year, month, day).is_ok() {
        return Err(DateParseError::Invalid);
    }

    PgDate::from_calendar_date(year, month, day).map_err(|_| DateParseError::Invalid)
}

fn pg_date_in_range(year: i32, month: time::Month, day: u8) -> bool {
    pg_date_tuple_in_range(year, month, day)
}

fn pg_date_tuple_in_range(year: i32, month: time::Month, day: u8) -> bool {
    let lower = (-4_713, u8::from(time::Month::November), 24u8);
    let upper = (5_874_897, u8::from(time::Month::December), 31u8);
    let current = (year, u8::from(month), day);
    current >= lower && current <= upper
}
