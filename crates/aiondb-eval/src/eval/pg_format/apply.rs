use std::borrow::Cow;
use std::convert::TryFrom;

use time::{Date, Duration, Month, PrimitiveDateTime, Time, UtcOffset, Weekday};

use super::error::FormatError;
use super::model::{CompiledFormat, FieldKind, FieldSpec, FormatItem, Meridiem, ParsedFields};

const MONTH_NAMES_FULL: [(&str, u8); 12] = [
    ("january", 1),
    ("february", 2),
    ("march", 3),
    ("april", 4),
    ("may", 5),
    ("june", 6),
    ("july", 7),
    ("august", 8),
    ("september", 9),
    ("october", 10),
    ("november", 11),
    ("december", 12),
];

const MONTH_NAMES_ABBR: [(&str, u8); 12] = [
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

const WEEKDAY_NAMES_FULL: [&str; 7] = [
    "sunday",
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
];

const ROMAN_MONTHS: [(&str, u8); 12] = [
    ("i", 1),
    ("ii", 2),
    ("iii", 3),
    ("iv", 4),
    ("v", 5),
    ("vi", 6),
    ("vii", 7),
    ("viii", 8),
    ("ix", 9),
    ("x", 10),
    ("xi", 11),
    ("xii", 12),
];

#[inline]
fn starts_with_ignore_ascii_case_at(input: &str, pos: usize, prefix: &str) -> bool {
    input
        .get(pos..)
        .and_then(|tail| tail.get(..prefix.len()))
        .is_some_and(|chunk| chunk.eq_ignore_ascii_case(prefix))
}

pub(super) fn apply_format(
    input: &str,
    format: &CompiledFormat,
) -> Result<ParsedFields, FormatError> {
    if format.uses_gregorian && format.uses_iso {
        return Err(FormatError::InvalidCombination);
    }

    let mut state = ParsedFields {
        tz_sign: 1,
        ..ParsedFields::default()
    };
    let mut pos = 0usize;
    let mut prev_was_field = false;
    let mut prev_was_separator = false;

    if !format.exact_mode {
        pos = skip_spaces(input, pos);
    }

    for (index, item) in format.items.iter().enumerate() {
        match item {
            FormatItem::Separator(fmt_ch) => {
                let next_index = next_non_separator_index(&format.items, index + 1);
                let next_item = next_index.and_then(|next| format.items.get(next));
                let remaining_slots = count_remaining_separator_slots(&format.items, index);
                pos = consume_separator_slot(
                    input,
                    pos,
                    *fmt_ch,
                    next_item,
                    !prev_was_separator,
                    remaining_slots,
                    format.exact_mode,
                )?;
                prev_was_field = false;
                prev_was_separator = true;
            }
            FormatItem::Literal(literal) => {
                pos = consume_literal(input, pos, literal)?;
                prev_was_field = false;
                prev_was_separator = false;
            }
            FormatItem::Field(field) => {
                let next_item = format.items.get(index + 1);
                let adjacent_field = matches!(next_item, Some(FormatItem::Field(_)));
                pos = parse_field(
                    input,
                    pos,
                    field,
                    adjacent_field,
                    &mut state,
                    prev_was_field,
                )?;
                prev_was_field = true;
                prev_was_separator = false;
            }
        }
    }

    Ok(state)
}

pub(super) fn build_date(fields: &ParsedFields, input: &str) -> Result<Date, FormatError> {
    if let Some(julian_day) = fields.julian_day {
        return julian_to_date(julian_day).map_err(|()| FormatError::FieldOutOfRange(input.into()));
    }

    if fields.iso_year.is_some() || fields.iso_week.is_some() || fields.iso_day.is_some() {
        let iso_year = finalize_year(fields.iso_year.unwrap_or(1), false, false);
        if let Some(day_of_year) = fields.iso_day_of_year {
            let week_one = Date::from_iso_week_date(iso_year, 1, Weekday::Monday)
                .map_err(|_| FormatError::FieldOutOfRange(input.into()))?;
            return week_one
                .checked_add(Duration::days(i64::from(day_of_year.saturating_sub(1))))
                .ok_or_else(|| FormatError::FieldOutOfRange(input.into()));
        }
        let week = fields.iso_week.unwrap_or(1);
        let weekday = iso_weekday(fields.iso_day.unwrap_or(1))
            .ok_or_else(|| FormatError::FieldOutOfRange(input.into()))?;
        return Date::from_iso_week_date(iso_year, week, weekday)
            .map_err(|_| FormatError::FieldOutOfRange(input.into()));
    }

    let year = resolve_pg_year(fields)?;
    if let Some(day_of_year) = fields.day_of_year {
        return Date::from_ordinal_date(year, day_of_year)
            .map_err(|_| FormatError::FieldOutOfRange(input.into()));
    }
    if let Some(week) = fields.week_of_year {
        let date = build_from_gregorian_week(year, week, fields.weekday.unwrap_or(1))
            .ok_or_else(|| FormatError::FieldOutOfRange(input.into()))?;
        return Ok(date);
    }

    let month = Month::try_from(fields.month.unwrap_or(1))
        .map_err(|_| FormatError::FieldOutOfRange(input.into()))?;
    let day = if let Some(week_of_month) = fields.week_of_month {
        week_of_month
            .saturating_sub(1)
            .saturating_mul(7)
            .saturating_add(1)
    } else {
        fields.day.unwrap_or(1)
    };
    Date::from_calendar_date(year, month, day)
        .map_err(|_| FormatError::FieldOutOfRange(input.into()))
}

fn build_time_components(
    fields: &ParsedFields,
    input: &str,
) -> Result<(u8, u8, u8, u32), FormatError> {
    let (hour, minute, second) = if let Some(seconds_of_day) = fields.seconds_of_day {
        if seconds_of_day >= 86_400 {
            return Err(FormatError::FieldOutOfRange(input.into()));
        }
        (
            u8::try_from(seconds_of_day / 3600)
                .map_err(|_| FormatError::FieldOutOfRange(input.into()))?,
            u8::try_from((seconds_of_day % 3600) / 60)
                .map_err(|_| FormatError::FieldOutOfRange(input.into()))?,
            u8::try_from(seconds_of_day % 60)
                .map_err(|_| FormatError::FieldOutOfRange(input.into()))?,
        )
    } else {
        let mut hour = fields.hour.unwrap_or(0);
        if fields.hour_is_12 && hour > 12 {
            return Err(FormatError::InvalidHourFor12Clock(hour));
        }
        if let Some(meridiem) = fields.meridiem {
            hour = match (hour, meridiem) {
                (12, Meridiem::Am) => 0,
                (12, Meridiem::Pm) => 12,
                (value, Meridiem::Pm) => value.saturating_add(12),
                (value, Meridiem::Am) => value,
            };
        }
        (hour, fields.minute.unwrap_or(0), fields.second.unwrap_or(0))
    };
    Ok((hour, minute, second, fields.microsecond.unwrap_or(0)))
}

pub(super) fn build_offset(fields: &ParsedFields) -> Result<Option<UtcOffset>, FormatError> {
    if fields.tz_hour.is_none() && fields.tz_minute.is_none() {
        return Ok(None);
    }
    let sign = if fields.tz_sign < 0 { -1 } else { 1 };
    let hours = i8::try_from(fields.tz_hour.unwrap_or(0))
        .map_err(|_| FormatError::FieldOutOfRange(String::new()))?
        .checked_mul(sign)
        .ok_or_else(|| FormatError::FieldOutOfRange(String::new()))?;
    let minutes = i8::try_from(fields.tz_minute.unwrap_or(0))
        .map_err(|_| FormatError::FieldOutOfRange(String::new()))?
        .checked_mul(sign)
        .ok_or_else(|| FormatError::FieldOutOfRange(String::new()))?;
    UtcOffset::from_hms(hours, minutes, 0)
        .map(Some)
        .map_err(|_| FormatError::FieldOutOfRange(String::new()))
}

fn parse_field(
    input: &str,
    mut pos: usize,
    field: &FieldSpec,
    adjacent_field: bool,
    state: &mut ParsedFields,
    prev_was_field: bool,
) -> Result<usize, FormatError> {
    if prev_was_field || !adjacent_field {
        pos = skip_spaces(input, pos);
    }
    // PostgreSQL skips non-alphanumeric separator characters in the input
    // when two format fields are adjacent (no explicit separator between them).
    let textual_month_field = matches!(
        field.kind,
        FieldKind::MonthAbbr | FieldKind::MonthFull | FieldKind::MonthRoman
    );
    if prev_was_field && !textual_month_field {
        pos = skip_input_separators(input, pos);
    }

    match field.kind {
        FieldKind::Year { width, iso, comma } => {
            let (negative, parsed, next) = parse_year(
                input,
                pos,
                width,
                field.fill_mode,
                adjacent_field,
                comma,
                field.label,
            )?;
            if iso {
                assign_value(&mut state.iso_year, infer_year(parsed, width), field.label)?;
            } else {
                assign_value(&mut state.year_component, parsed, field.label)?;
                state.year_component_width = Some(width);
                state.year_negative = negative;
            }
            Ok(next)
        }
        FieldKind::MonthNumber => {
            let (value, next) =
                parse_uint(input, pos, 2, field.fill_mode, adjacent_field, field.label)?;
            assign_value(
                &mut state.month,
                u8::try_from(value).map_err(|_| FormatError::FieldOutOfRange(input.into()))?,
                field.label,
            )?;
            Ok(next)
        }
        FieldKind::MonthAbbr => {
            let (value, next) = parse_text_month(input, pos, false, field.label)?;
            assign_value(&mut state.month, value, field.label)?;
            Ok(next)
        }
        FieldKind::MonthFull => {
            let (value, next) = parse_text_month(input, pos, true, field.label)?;
            assign_value(&mut state.month, value, field.label)?;
            Ok(next)
        }
        FieldKind::MonthRoman => {
            let (value, next) = parse_roman_month(input, pos, field.label)?;
            assign_value(&mut state.month, value, field.label)?;
            Ok(next)
        }
        FieldKind::Day => {
            let (value, next) =
                parse_uint(input, pos, 2, field.fill_mode, adjacent_field, field.label)?;
            assign_value(
                &mut state.day,
                u8::try_from(value).map_err(|_| FormatError::FieldOutOfRange(input.into()))?,
                field.label,
            )?;
            Ok(next)
        }
        FieldKind::DayOfWeekShort => {
            consume_weekday_short(input, pos)?;
            Ok(pos + 3)
        }
        FieldKind::DayOfWeekFull => consume_weekday_full(input, pos),
        FieldKind::GregorianWeek => {
            let (value, next) =
                parse_uint(input, pos, 2, field.fill_mode, adjacent_field, field.label)?;
            assign_value(
                &mut state.week_of_year,
                u8::try_from(value).map_err(|_| FormatError::FieldOutOfRange(input.into()))?,
                field.label,
            )?;
            Ok(next)
        }
        FieldKind::GregorianDayOfWeek => {
            let (value, next) =
                parse_uint(input, pos, 1, field.fill_mode, adjacent_field, field.label)?;
            assign_value(
                &mut state.weekday,
                u8::try_from(value).map_err(|_| FormatError::FieldOutOfRange(input.into()))?,
                field.label,
            )?;
            Ok(next)
        }
        FieldKind::GregorianDayOfYear => {
            let (value, next) =
                parse_uint(input, pos, 3, field.fill_mode, adjacent_field, field.label)?;
            assign_value(
                &mut state.day_of_year,
                u16::try_from(value).map_err(|_| FormatError::FieldOutOfRange(input.into()))?,
                field.label,
            )?;
            Ok(next)
        }
        FieldKind::IsoWeek => {
            let (value, next) =
                parse_uint(input, pos, 2, field.fill_mode, adjacent_field, field.label)?;
            assign_value(
                &mut state.iso_week,
                u8::try_from(value).map_err(|_| FormatError::FieldOutOfRange(input.into()))?,
                field.label,
            )?;
            Ok(next)
        }
        FieldKind::IsoDayOfWeek => {
            let (value, next) =
                parse_uint(input, pos, 1, field.fill_mode, adjacent_field, field.label)?;
            assign_value(
                &mut state.iso_day,
                u8::try_from(value).map_err(|_| FormatError::FieldOutOfRange(input.into()))?,
                field.label,
            )?;
            Ok(next)
        }
        FieldKind::IsoDayOfYear => {
            let (value, next) =
                parse_uint(input, pos, 3, field.fill_mode, adjacent_field, field.label)?;
            assign_value(
                &mut state.iso_day_of_year,
                u16::try_from(value).map_err(|_| FormatError::FieldOutOfRange(input.into()))?,
                field.label,
            )?;
            Ok(next)
        }
        FieldKind::WeekOfMonth => {
            let (value, next) =
                parse_uint(input, pos, 1, field.fill_mode, adjacent_field, field.label)?;
            assign_value(
                &mut state.week_of_month,
                u8::try_from(value).map_err(|_| FormatError::FieldOutOfRange(input.into()))?,
                field.label,
            )?;
            Ok(next)
        }
        FieldKind::QuarterIgnored => {
            let (_, next) = parse_uint(input, pos, 1, true, adjacent_field, field.label)?;
            Ok(next)
        }
        FieldKind::Century => {
            let (value, next) =
                parse_uint(input, pos, 2, field.fill_mode, adjacent_field, field.label)?;
            assign_value(
                &mut state.century,
                i32::try_from(value).map_err(|_| FormatError::FieldOutOfRange(input.into()))?,
                field.label,
            )?;
            Ok(next)
        }
        FieldKind::JulianDay => {
            let (value, next) = parse_unbounded_uint(input, pos, field.label)?;
            state.julian_day = Some(i64::from(value));
            Ok(next)
        }
        FieldKind::Hour24 => {
            let (value, next) =
                parse_uint(input, pos, 2, field.fill_mode, adjacent_field, field.label)?;
            assign_value(
                &mut state.hour,
                u8::try_from(value).map_err(|_| FormatError::FieldOutOfRange(input.into()))?,
                field.label,
            )?;
            Ok(next)
        }
        FieldKind::Hour12 => {
            let (value, next) =
                parse_uint(input, pos, 2, field.fill_mode, adjacent_field, field.label)?;
            assign_value(
                &mut state.hour,
                u8::try_from(value).map_err(|_| FormatError::FieldOutOfRange(input.into()))?,
                field.label,
            )?;
            state.hour_is_12 = true;
            Ok(next)
        }
        FieldKind::Minute => {
            let (value, next) =
                parse_uint(input, pos, 2, field.fill_mode, adjacent_field, field.label)?;
            assign_value(
                &mut state.minute,
                u8::try_from(value).map_err(|_| FormatError::FieldOutOfRange(input.into()))?,
                field.label,
            )?;
            Ok(next)
        }
        FieldKind::Second => {
            let (value, next) =
                parse_uint(input, pos, 2, field.fill_mode, adjacent_field, field.label)?;
            assign_value(
                &mut state.second,
                u8::try_from(value).map_err(|_| FormatError::FieldOutOfRange(input.into()))?,
                field.label,
            )?;
            Ok(next)
        }
        FieldKind::SecondsOfDay => {
            let (value, next) = parse_unbounded_uint(input, pos, field.label)?;
            state.seconds_of_day = Some(value);
            Ok(next)
        }
        FieldKind::Millisecond => {
            let (value, next) =
                parse_uint(input, pos, 3, field.fill_mode, adjacent_field, field.label)?;
            state.microsecond = Some(value.saturating_mul(1_000));
            Ok(next)
        }
        FieldKind::FractionalSecond { precision } => {
            let (value, next) = parse_fractional_second(input, pos, precision)?;
            state.microsecond = Some(value);
            Ok(next)
        }
        FieldKind::Meridiem => {
            let (value, next) = parse_meridiem(input, pos)?;
            state.meridiem = Some(value);
            Ok(next)
        }
        FieldKind::BcAd => {
            let (bc, next) = parse_bc_ad(input, pos)?;
            state.bc = bc;
            Ok(next)
        }
        FieldKind::TzHour => {
            let (sign, value, next) = parse_tz_hour(input, pos)?;
            state.tz_sign = sign;
            state.tz_hour = Some(value);
            Ok(next)
        }
        FieldKind::TzMinute => {
            let (value, next) = parse_uint(input, pos, 2, true, adjacent_field, field.label)?;
            state.tz_minute =
                Some(u8::try_from(value).map_err(|_| FormatError::FieldOutOfRange(input.into()))?);
            Ok(next)
        }
        FieldKind::TzName => Err(FormatError::UnsupportedField("TZ")),
        FieldKind::OrdinalSuffix => {
            let next = consume_alpha(input, pos, 2).ok_or_else(|| FormatError::InvalidValue {
                field: "TH",
                value: preview_value(input, pos, 2),
                detail: Cow::Borrowed(
                    "The given value did not match any of the allowed values for this field.",
                ),
                hint: None,
            })?;
            Ok(next)
        }
    }
}

fn consume_literal(input: &str, pos: usize, literal: &str) -> Result<usize, FormatError> {
    if starts_with_ignore_ascii_case_at(input, pos, literal) {
        Ok(pos + literal.len())
    } else {
        Err(FormatError::InvalidValue {
            field: "literal",
            value: preview_chunk(input, pos),
            detail: Cow::Borrowed(
                "The given value did not match any of the allowed values for this field.",
            ),
            hint: None,
        })
    }
}

fn consume_separator_slot(
    input: &str,
    pos: usize,
    fmt_ch: char,
    next_item: Option<&FormatItem>,
    first_slot_in_run: bool,
    remaining_slots_in_run: usize,
    exact_mode: bool,
) -> Result<usize, FormatError> {
    if exact_mode {
        return consume_exact_separator_slot(input, pos, fmt_ch);
    }

    let start = pos;
    let mut current = pos;
    if first_slot_in_run {
        current = skip_spaces(input, current);
    }

    if !first_slot_in_run
        && input
            .as_bytes()
            .get(current)
            .is_some_and(u8::is_ascii_whitespace)
    {
        return Ok(skip_spaces(input, current));
    }

    if current >= input.len() {
        return Ok(current);
    }

    let Some(ch) = input[current..].chars().next() else {
        return Ok(current);
    };

    if is_sign_char(ch) && next_item.is_some_and(target_accepts_sign) {
        if remaining_slots_in_run == 1 {
            return Ok(current);
        }
        return Ok(current + ch.len_utf8());
    }

    if current > start && fmt_ch.is_ascii_alphanumeric() {
        return Ok(current);
    }

    if target_starts_here(input, current, next_item) {
        if current > start || !first_slot_in_run {
            return Ok(current);
        }
        return Ok(current);
    }

    if ch.eq_ignore_ascii_case(&fmt_ch) {
        return Ok(current + ch.len_utf8());
    }

    if !ch.is_ascii_alphanumeric() {
        return Ok(current + ch.len_utf8());
    }

    Ok(current)
}

fn consume_exact_separator_slot(
    input: &str,
    pos: usize,
    fmt_ch: char,
) -> Result<usize, FormatError> {
    if fmt_ch.is_ascii_whitespace() {
        let next = skip_spaces(input, pos);
        if next > pos {
            return Ok(next);
        }
    } else if fmt_ch.is_ascii_punctuation() {
        if let Some(ch) = input[pos..].chars().next() {
            if !ch.is_ascii_alphanumeric() && !ch.is_ascii_whitespace() {
                return Ok(pos + ch.len_utf8());
            }
        }
    } else if input[pos..].starts_with(fmt_ch) {
        return Ok(pos + fmt_ch.len_utf8());
    }
    Err(FormatError::InvalidValue {
        field: "separator",
        value: preview_chunk(input, pos),
        detail: Cow::Borrowed(
            "The given value did not match any of the allowed values for this field.",
        ),
        hint: None,
    })
}

fn next_non_separator_index(items: &[FormatItem], start: usize) -> Option<usize> {
    items[start..]
        .iter()
        .position(|item| !matches!(item, FormatItem::Separator(_)))
        .map(|offset| start + offset)
}

fn count_remaining_separator_slots(items: &[FormatItem], index: usize) -> usize {
    items[index..]
        .iter()
        .take_while(|item| matches!(item, FormatItem::Separator(_)))
        .count()
}

fn target_starts_here(input: &str, pos: usize, next_item: Option<&FormatItem>) -> bool {
    match next_item {
        Some(FormatItem::Literal(literal)) => starts_with_ignore_ascii_case_at(input, pos, literal),
        Some(FormatItem::Field(field)) => field_starts_here(input, pos, field),
        _ => false,
    }
}

fn field_starts_here(input: &str, pos: usize, field: &FieldSpec) -> bool {
    let Some(ch) = input[pos..].chars().next() else {
        return false;
    };
    match field.kind {
        FieldKind::MonthAbbr
        | FieldKind::MonthFull
        | FieldKind::MonthRoman
        | FieldKind::DayOfWeekShort
        | FieldKind::DayOfWeekFull
        | FieldKind::Meridiem
        | FieldKind::BcAd
        | FieldKind::OrdinalSuffix => ch.is_ascii_alphabetic(),
        FieldKind::TzHour => ch.is_ascii_digit() || is_sign_char(ch),
        FieldKind::Year { .. } => ch.is_ascii_digit(),
        _ => ch.is_ascii_digit(),
    }
}

fn target_accepts_sign(item: &FormatItem) -> bool {
    matches!(
        item,
        FormatItem::Field(FieldSpec {
            kind: FieldKind::TzHour,
            ..
        })
    )
}

fn is_sign_char(ch: char) -> bool {
    ch == '+' || ch == '-'
}

fn consume_weekday_short(input: &str, pos: usize) -> Result<(), FormatError> {
    if consume_alpha(input, pos, 3).is_some() {
        Ok(())
    } else {
        Err(FormatError::InvalidValue {
            field: "DY",
            value: preview_value(input, pos, 3),
            detail: Cow::Borrowed(
                "The given value did not match any of the allowed values for this field.",
            ),
            hint: None,
        })
    }
}

fn consume_weekday_full(input: &str, pos: usize) -> Result<usize, FormatError> {
    for day in WEEKDAY_NAMES_FULL {
        if starts_with_ignore_ascii_case_at(input, pos, day) {
            return Ok(pos + day.len());
        }
    }
    Err(FormatError::InvalidValue {
        field: "Day",
        value: preview_chunk(input, pos),
        detail: Cow::Borrowed(
            "The given value did not match any of the allowed values for this field.",
        ),
        hint: None,
    })
}

fn parse_text_month(
    input: &str,
    pos: usize,
    full: bool,
    field: &'static str,
) -> Result<(u8, usize), FormatError> {
    let names = if full {
        &MONTH_NAMES_FULL[..]
    } else {
        &MONTH_NAMES_ABBR[..]
    };
    for &(name, month) in names {
        if starts_with_ignore_ascii_case_at(input, pos, name) {
            return Ok((month, pos + name.len()));
        }
    }
    Err(FormatError::InvalidValue {
        field,
        value: preview_chunk(input, pos),
        detail: Cow::Borrowed(
            "The given value did not match any of the allowed values for this field.",
        ),
        hint: None,
    })
}

fn parse_roman_month(
    input: &str,
    pos: usize,
    field: &'static str,
) -> Result<(u8, usize), FormatError> {
    for &(roman, month) in ROMAN_MONTHS.iter().rev() {
        if starts_with_ignore_ascii_case_at(input, pos, roman) {
            return Ok((month, pos + roman.len()));
        }
    }
    Err(FormatError::InvalidValue {
        field,
        value: preview_chunk(input, pos),
        detail: Cow::Borrowed(
            "The given value did not match any of the allowed values for this field.",
        ),
        hint: None,
    })
}

fn parse_meridiem(input: &str, pos: usize) -> Result<(Meridiem, usize), FormatError> {
    if starts_with_ignore_ascii_case_at(input, pos, "P.M.") {
        Ok((Meridiem::Pm, pos + 4))
    } else if starts_with_ignore_ascii_case_at(input, pos, "A.M.") {
        Ok((Meridiem::Am, pos + 4))
    } else if starts_with_ignore_ascii_case_at(input, pos, "PM") {
        Ok((Meridiem::Pm, pos + 2))
    } else if starts_with_ignore_ascii_case_at(input, pos, "AM") {
        Ok((Meridiem::Am, pos + 2))
    } else {
        Err(FormatError::InvalidValue {
            field: "PM",
            value: preview_chunk(input, pos),
            detail: Cow::Borrowed(
                "The given value did not match any of the allowed values for this field.",
            ),
            hint: None,
        })
    }
}

fn parse_bc_ad(input: &str, pos: usize) -> Result<(bool, usize), FormatError> {
    if starts_with_ignore_ascii_case_at(input, pos, "B.C.") {
        Ok((true, pos + 4))
    } else if starts_with_ignore_ascii_case_at(input, pos, "A.D.") {
        Ok((false, pos + 4))
    } else if starts_with_ignore_ascii_case_at(input, pos, "BC") {
        Ok((true, pos + 2))
    } else if starts_with_ignore_ascii_case_at(input, pos, "AD") {
        Ok((false, pos + 2))
    } else {
        Err(FormatError::InvalidValue {
            field: "BC",
            value: preview_chunk(input, pos),
            detail: Cow::Borrowed(
                "The given value did not match any of the allowed values for this field.",
            ),
            hint: None,
        })
    }
}

fn parse_tz_hour(input: &str, pos: usize) -> Result<(i8, u8, usize), FormatError> {
    let bytes = input.as_bytes();
    let sign = match bytes.get(pos).copied() {
        Some(b'-') => -1,
        Some(b'+') => 1,
        _ => 1,
    };
    let start = if sign == 1 && bytes.get(pos) != Some(&b'+') {
        pos
    } else {
        pos + 1
    };
    let (value, next) = parse_uint(input, start, 2, true, false, "TZH")?;
    let value = u8::try_from(value).map_err(|_| FormatError::FieldOutOfRange(input.into()))?;
    Ok((sign, value, next))
}

fn parse_fractional_second(
    input: &str,
    pos: usize,
    precision: u8,
) -> Result<(u32, usize), FormatError> {
    let bytes = input.as_bytes();
    let mut end = pos;
    while end < input.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    if end == pos {
        return Ok((0, pos));
    }
    let digits = &input[pos..end];
    if digits.len() > 6 {
        return Err(FormatError::FieldOutOfRange(input.to_string()));
    }
    let keep = usize::from(precision.min(u8::try_from(digits.len()).unwrap_or(u8::MAX)));
    let mut value: u32 = digits[..keep]
        .parse()
        .map_err(|_| FormatError::InvalidValue {
            field: "FF",
            value: digits[..keep].to_string(),
            detail: Cow::Borrowed("Value must be an integer."),
            hint: None,
        })?;
    if digits.len() > keep
        && digits
            .as_bytes()
            .get(keep)
            .is_some_and(|digit| *digit >= b'5')
    {
        value = value.saturating_add(1);
    }
    let keep_u32 = u32::try_from(keep).unwrap_or(u32::MAX);
    let micros = value.saturating_mul(10u32.pow(6u32.saturating_sub(keep_u32)));
    Ok((micros, end))
}

fn parse_year(
    input: &str,
    pos: usize,
    width: u8,
    fill_mode: bool,
    adjacent_field: bool,
    comma: bool,
    field: &'static str,
) -> Result<(bool, i32, usize), FormatError> {
    let bytes = input.as_bytes();
    let mut start = pos;
    let negative = bytes.get(start) == Some(&b'-');
    if negative {
        start += 1;
    }
    if start >= input.len() {
        return Err(FormatError::SourceStringTooShort {
            field,
            required: usize::from(width),
            remaining: 0,
        });
    }
    let mut end = start;
    if comma {
        while end < input.len()
            && (input.as_bytes()[end].is_ascii_digit() || input.as_bytes()[end] == b',')
        {
            end += 1;
        }
    } else if fill_mode || !adjacent_field {
        while end < input.len() && input.as_bytes()[end].is_ascii_digit() {
            end += 1;
        }
    } else {
        let max = start + usize::from(width);
        while end < input.len() && end < max && input.as_bytes()[end].is_ascii_digit() {
            end += 1;
        }
    }
    if end == start {
        return Err(FormatError::InvalidValue {
            field,
            value: preview_value(input, pos, usize::from(width)),
            detail: Cow::Borrowed("Value must be an integer."),
            hint: None,
        });
    }
    let year_slice = &input[start..end];
    let (parsed_len, value) = if comma {
        let mut digits = 0usize;
        let mut acc: i32 = 0;
        for byte in year_slice.bytes() {
            if byte == b',' {
                continue;
            }
            digits += 1;
            let digit = i32::from(byte - b'0');
            acc = acc
                .checked_mul(10)
                .and_then(|value| value.checked_add(digit))
                .ok_or(FormatError::YearOutOfRange)?;
        }
        if digits == 0 {
            return Err(FormatError::YearOutOfRange);
        }
        (digits, acc)
    } else {
        let value = year_slice
            .parse::<i32>()
            .map_err(|_| FormatError::YearOutOfRange)?;
        (year_slice.len(), value)
    };
    if !fill_mode && adjacent_field && parsed_len < usize::from(width) {
        if parsed_len == remaining_digits(input, start) {
            return Err(FormatError::SourceStringTooShort {
                field,
                required: usize::from(width),
                remaining: parsed_len,
            });
        }
        return Err(FormatError::InvalidValue {
            field,
            value: preview_value(input, start, usize::from(width)),
            detail: Cow::Owned(format!(
                "Field requires {width} characters, but only {parsed_len} could be parsed."
            )),
            hint: Some("If your source string is not fixed-width, try using the \"FM\" modifier."),
        });
    }
    Ok((negative, value, end))
}

fn parse_uint(
    input: &str,
    pos: usize,
    width: usize,
    fill_mode: bool,
    adjacent_field: bool,
    field: &'static str,
) -> Result<(u32, usize), FormatError> {
    let bytes = input.as_bytes();
    let mut end = pos;
    let max = if fill_mode || !adjacent_field {
        input.len()
    } else {
        pos + width
    };
    while end < input.len() && end < max && bytes[end].is_ascii_digit() {
        end += 1;
    }
    if end == pos {
        return Err(FormatError::InvalidValue {
            field,
            value: preview_value(input, pos, width),
            detail: Cow::Borrowed("Value must be an integer."),
            hint: None,
        });
    }
    if !fill_mode && adjacent_field && end - pos < width {
        if remaining_digits(input, pos) == end - pos && pos + (end - pos) >= input.len() {
            return Err(FormatError::SourceStringTooShort {
                field,
                required: width,
                remaining: end - pos,
            });
        }
        return Err(FormatError::InvalidValue {
            field,
            value: preview_value(input, pos, width),
            detail: Cow::Owned(format!(
                "Field requires {width} characters, but only {} could be parsed.",
                end - pos
            )),
            hint: Some("If your source string is not fixed-width, try using the \"FM\" modifier."),
        });
    }
    let value = input[pos..end]
        .parse::<u32>()
        .map_err(|_| FormatError::InvalidValue {
            field,
            value: preview_value(input, pos, width),
            detail: Cow::Borrowed("Value must be an integer."),
            hint: None,
        })?;
    Ok((value, end))
}

fn parse_unbounded_uint(
    input: &str,
    pos: usize,
    field: &'static str,
) -> Result<(u32, usize), FormatError> {
    let bytes = input.as_bytes();
    let mut end = pos;
    while end < input.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    if end == pos {
        return Err(FormatError::InvalidValue {
            field,
            value: preview_chunk(input, pos),
            detail: Cow::Borrowed("Value must be an integer."),
            hint: None,
        });
    }
    let value = input[pos..end]
        .parse::<u32>()
        .map_err(|_| FormatError::YearOutOfRange)?;
    Ok((value, end))
}

fn assign_value<T: Copy + Eq>(
    slot: &mut Option<T>,
    value: T,
    field: &'static str,
) -> Result<(), FormatError> {
    match slot {
        Some(existing) if *existing != value => Err(FormatError::ConflictingField(field)),
        Some(_) => Ok(()),
        None => {
            *slot = Some(value);
            Ok(())
        }
    }
}

fn resolve_pg_year(fields: &ParsedFields) -> Result<i32, FormatError> {
    let raw_year = fields.year_component.unwrap_or(1);
    let raw_width = fields.year_component_width.unwrap_or(4);
    let mut year = if let Some(century) = fields.century {
        let suffix = match raw_width {
            1 => raw_year.rem_euclid(10),
            2 => raw_year.rem_euclid(100),
            3 => raw_year.rem_euclid(1000),
            _ => raw_year.rem_euclid(100),
        };
        (century - 1)
            .checked_mul(100)
            .and_then(|value| value.checked_add(suffix))
            .ok_or(FormatError::YearOutOfRange)?
    } else {
        infer_year(raw_year, raw_width)
    };
    if fields.year_negative {
        // A leading minus sign in the input means the year is already in
        // astronomical notation (negative = before epoch), so we return it
        // directly without the BC-style +1 adjustment.
        year = -year;
        return Ok(year);
    }
    Ok(finalize_year(year, fields.bc, true))
}

fn finalize_year(year: i32, bc: bool, pg_style: bool) -> i32 {
    let mut pg_year = year;
    if bc {
        pg_year = -pg_year;
    }
    if !pg_style {
        return pg_year;
    }
    if pg_year < 0 {
        pg_year + 1
    } else {
        pg_year
    }
}

fn infer_year(value: i32, width: u8) -> i32 {
    if width >= 3 {
        // 3- and 4-digit (or more) year values are used as-is (PostgreSQL behavior)
        value
    } else {
        match width {
            1 => 2000 + value,
            2 => {
                if value < 70 {
                    2000 + value
                } else {
                    1900 + value
                }
            }
            _ => value,
        }
    }
}

fn build_from_gregorian_week(year: i32, week: u8, day: u8) -> Option<Date> {
    let jan1 = Date::from_calendar_date(year, Month::January, 1).ok()?;
    let jan1_monday_based = i64::from(jan1.weekday().number_days_from_monday()) + 1;
    let ordinal = i64::from(week.saturating_sub(1)) * 7 + i64::from(day) - jan1_monday_based + 1;
    if ordinal < 1 {
        return None;
    }
    jan1.checked_add(Duration::days(ordinal - 1))
}

fn iso_weekday(value: u8) -> Option<Weekday> {
    match value {
        1 => Some(Weekday::Monday),
        2 => Some(Weekday::Tuesday),
        3 => Some(Weekday::Wednesday),
        4 => Some(Weekday::Thursday),
        5 => Some(Weekday::Friday),
        6 => Some(Weekday::Saturday),
        7 => Some(Weekday::Sunday),
        _ => None,
    }
}

fn skip_spaces(input: &str, mut pos: usize) -> usize {
    while pos < input.len() && input.as_bytes()[pos].is_ascii_whitespace() {
        pos += 1;
    }
    pos
}

/// Skip non-alphanumeric separator characters in the input (e.g. '/', '-', '.')
/// when transitioning between adjacent format fields with no explicit separator.
fn skip_input_separators(input: &str, mut pos: usize) -> usize {
    while pos < input.len() {
        let b = input.as_bytes()[pos];
        if b.is_ascii_alphanumeric() || b.is_ascii_whitespace() {
            break;
        }
        pos += 1;
    }
    pos
}

fn consume_alpha(input: &str, pos: usize, count: usize) -> Option<usize> {
    let bytes = input.as_bytes();
    if pos + count > input.len() {
        return None;
    }
    bytes[pos..pos + count]
        .iter()
        .all(u8::is_ascii_alphabetic)
        .then_some(pos + count)
}

use super::apply_helpers::{julian_to_date, preview_chunk, preview_value, remaining_digits};

pub(super) fn build_timestamp_components(
    fields: &ParsedFields,
    input: &str,
) -> Result<(PrimitiveDateTime, Option<UtcOffset>), FormatError> {
    let date = build_date(fields, input)?;
    let (hour, minute, second, microsecond) = build_time_components(fields, input)?;
    let time = Time::from_hms(hour, minute, second)
        .map_err(|_| FormatError::FieldOutOfRange(input.into()))?;
    let offset = build_offset(fields)?;
    let timestamp = PrimitiveDateTime::new(date, time)
        .checked_add(time::Duration::microseconds(i64::from(microsecond)))
        .ok_or_else(|| FormatError::FieldOutOfRange(input.into()))?;
    Ok((timestamp, offset))
}

#[cfg(test)]
#[path = "apply_tests.rs"]
mod tests;
