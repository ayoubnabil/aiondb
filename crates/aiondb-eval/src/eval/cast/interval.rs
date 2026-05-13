use super::interval_iso::*;
use crate::eval::scalar_functions::value_convert::i128_to_f64;
use crate::eval::session::EvalIntervalStyle;
use aiondb_core::{DataType, DbError, DbResult, ErrorReport, IntervalValue, SqlState, Value};

pub(super) const MONTHS_MIN: i128 = i32::MIN as i128;
pub(super) const MONTHS_MAX: i128 = i32::MAX as i128;
pub(super) const DAYS_MIN: i128 = i32::MIN as i128;
pub(super) const DAYS_MAX: i128 = i32::MAX as i128;
pub(super) const MICROS_MIN: i128 = i64::MIN as i128;
pub(super) const MICROS_MAX: i128 = i64::MAX as i128;
pub(super) use crate::eval::DAY_MICROS_I128 as DAY_MICROS;
pub(super) const HOUR_MICROS: i128 = 3_600_000_000;
pub(super) const MINUTE_MICROS: i128 = 60_000_000;
pub(super) const SECOND_MICROS: i128 = 1_000_000;
pub(super) const MILLISECOND_MICROS: i128 = 1_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum IntervalParseError {
    InvalidSyntax,
    FieldValueOutOfRange,
    OutOfRange,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum IntervalInputField {
    Millennium,
    Century,
    Decade,
    Year,
    Month,
    Week,
    Day,
    Hour,
    Minute,
    Second,
    Millisecond,
    Microsecond,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum IntervalFieldQualifier {
    Year,
    Month,
    Day,
    Hour,
    Minute,
    Second,
}

#[derive(Default)]
struct SeenIntervalFields {
    time_token: bool,
    millennium: bool,
    century: bool,
    decade: bool,
    year: bool,
    month: bool,
    week: bool,
    day: bool,
    hour: bool,
    minute: bool,
    second: bool,
    millisecond: bool,
    microsecond: bool,
    fractional_second: bool,
    fractional_millisecond: bool,
}

impl SeenIntervalFields {
    fn note(&mut self, field: IntervalInputField) -> Result<(), IntervalParseError> {
        let already_seen = match field {
            IntervalInputField::Millennium => &mut self.millennium,
            IntervalInputField::Century => &mut self.century,
            IntervalInputField::Decade => &mut self.decade,
            IntervalInputField::Year => &mut self.year,
            IntervalInputField::Month => &mut self.month,
            IntervalInputField::Week => &mut self.week,
            IntervalInputField::Day => &mut self.day,
            IntervalInputField::Hour => &mut self.hour,
            IntervalInputField::Minute => &mut self.minute,
            IntervalInputField::Second => &mut self.second,
            IntervalInputField::Millisecond => &mut self.millisecond,
            IntervalInputField::Microsecond => &mut self.microsecond,
        };
        if *already_seen {
            return Err(IntervalParseError::InvalidSyntax);
        }
        *already_seen = true;
        Ok(())
    }

    fn note_unit(
        &mut self,
        field: IntervalInputField,
        value: f64,
    ) -> Result<(), IntervalParseError> {
        if self.time_token
            && matches!(
                field,
                IntervalInputField::Hour
                    | IntervalInputField::Minute
                    | IntervalInputField::Second
                    | IntervalInputField::Millisecond
                    | IntervalInputField::Microsecond
            )
        {
            return Err(IntervalParseError::InvalidSyntax);
        }

        match field {
            IntervalInputField::Second => {
                if self.second || (value.fract() != 0.0 && (self.millisecond || self.microsecond)) {
                    return Err(IntervalParseError::InvalidSyntax);
                }
                self.second = true;
                self.fractional_second = value.fract() != 0.0;
            }
            IntervalInputField::Millisecond => {
                if self.millisecond || self.fractional_second {
                    return Err(IntervalParseError::InvalidSyntax);
                }
                if value.fract() != 0.0 && self.microsecond {
                    return Err(IntervalParseError::InvalidSyntax);
                }
                self.millisecond = true;
                self.fractional_millisecond = value.fract() != 0.0;
            }
            IntervalInputField::Microsecond => {
                if self.microsecond || self.fractional_second || self.fractional_millisecond {
                    return Err(IntervalParseError::InvalidSyntax);
                }
                self.microsecond = true;
            }
            other => self.note(other)?,
        }
        Ok(())
    }

    fn note_time_token(&mut self, token: &str) -> Result<(), IntervalParseError> {
        let split_subsec = match (token.find('.'), token.rfind(':')) {
            (Some(dot_pos), Some(last_colon_pos)) => dot_pos > last_colon_pos,
            _ => token.contains('.'),
        };
        let (main, subsec) = if split_subsec {
            match token.split_once('.') {
                Some((m, f)) => (m, Some(f)),
                None => (token, None),
            }
        } else {
            (token, None)
        };
        let parts: Vec<&str> = main.split(':').collect();
        if parts.len() < 2 || parts.len() > 3 {
            return Err(IntervalParseError::InvalidSyntax);
        }
        if self.time_token
            || self.hour
            || self.minute
            || self.second
            || self.millisecond
            || self.microsecond
        {
            return Err(IntervalParseError::InvalidSyntax);
        }
        self.time_token = true;
        self.hour = true;
        self.minute = true;
        if parts.len() == 3 || subsec.is_some() {
            self.second = true;
            if subsec.is_some() {
                self.fractional_second = true;
            }
        }
        Ok(())
    }
}

impl IntervalParseError {
    fn into_db_error(self, input: &str) -> DbError {
        match self {
            Self::InvalidSyntax => DbError::invalid_datetime_syntax("interval", input),
            Self::FieldValueOutOfRange => DbError::from_report(ErrorReport::new(
                SqlState::NumericValueOutOfRange,
                format!("interval field value out of range: \"{input}\""),
            )),
            Self::OutOfRange => DbError::from_report(ErrorReport::new(
                SqlState::NumericValueOutOfRange,
                "interval out of range",
            )),
        }
    }
}

pub(super) fn parse_interval(input: &str) -> DbResult<Value> {
    parse_interval_internal(input).map_err(|error| error.into_db_error(input))
}

pub(crate) fn cast_interval_with_fields(
    value: Value,
    start_field: &str,
    end_field: Option<&str>,
    second_precision: Option<u32>,
) -> DbResult<Value> {
    if matches!(value, Value::Null) {
        return Ok(Value::Null);
    }

    let start = parse_interval_field_qualifier(start_field)?;
    let end = match end_field {
        Some(field) => parse_interval_field_qualifier(field)?,
        None => start,
    };

    match value {
        Value::Text(input) => {
            parse_interval_with_fields_internal(&input, start, end, second_precision)
                .map(Value::Interval)
                .map_err(|error| error.into_db_error(&input))
        }
        Value::Interval(interval) => {
            truncate_interval_with_fields(&interval, end, second_precision)
                .map(Value::Interval)
                .map_err(interval_field_runtime_error)
        }
        other => {
            let casted = super::cast_value(other, &DataType::Interval)?;
            let Value::Interval(interval) = casted else {
                return Err(DbError::internal(
                    "__aiondb_interval_fields() expected cast to interval to succeed",
                ));
            };
            truncate_interval_with_fields(&interval, end, second_precision)
                .map(Value::Interval)
                .map_err(interval_field_runtime_error)
        }
    }
}

fn parse_interval_internal(input: &str) -> Result<Value, IntervalParseError> {
    parse_interval_internal_impl(input, true)
}

fn parse_interval_internal_impl(
    input: &str,
    allow_sql_standard_force_negative: bool,
) -> Result<Value, IntervalParseError> {
    let s = input.trim();
    if s.is_empty() {
        return Err(IntervalParseError::InvalidSyntax);
    }

    if allow_sql_standard_force_negative {
        if let Some(positive_literal) = sql_standard_force_negative_literal(s) {
            let Value::Interval(interval) = parse_interval_internal_impl(&positive_literal, false)?
            else {
                return Err(IntervalParseError::InvalidSyntax);
            };
            return Ok(Value::Interval(IntervalValue::new(
                interval
                    .months
                    .checked_neg()
                    .ok_or(IntervalParseError::OutOfRange)?,
                interval
                    .days
                    .checked_neg()
                    .ok_or(IntervalParseError::OutOfRange)?,
                interval
                    .micros
                    .checked_neg()
                    .ok_or(IntervalParseError::OutOfRange)?,
            )));
        }
    }

    // Strip leading '@' (PostgreSQL optional prefix)
    let s = s.strip_prefix('@').map_or(s, |r| r.trim());

    // Try ISO 8601 duration format: P[n]Y[n]M[n]DT[n]H[n]M[n]S
    if s.starts_with('P') || s.starts_with('p') {
        return parse_iso8601_interval(s);
    }

    // Try internal display format first: "Nm Nd Nus"
    if let Some(v) = try_internal_interval_format(s) {
        return Ok(v);
    }

    // Try Y-M shorthand: "1-2" means 1 year 2 months
    if let Some(v) = try_year_month_shorthand(s)? {
        return Ok(v);
    }

    let mut months: i128 = 0;
    let mut days: i128 = 0;
    let mut micros: i128 = 0;
    let mut negate = false;
    let mut seen_fields = SeenIntervalFields::default();

    let tokens: Vec<&str> = s.split_whitespace().collect();
    let mut i = 0;
    while i < tokens.len() {
        let token = tokens[i];

        if token == "@" {
            i += 1;
            continue;
        }

        if matches!(token, "+" | "-") {
            let sign = token;
            let next = tokens
                .get(i + 1)
                .copied()
                .ok_or(IntervalParseError::InvalidSyntax)?;
            let signed_token = format!("{sign}{next}");

            if next.contains(':') {
                let unsigned = signed_token.trim_start_matches(['+', '-']);
                seen_fields.note_time_token(unsigned)?;
                let val = parse_time_to_micros(unsigned)?;
                let signed = if sign == "-" { -val } else { val };
                add_bounded(
                    &mut micros,
                    signed,
                    MICROS_MIN,
                    MICROS_MAX,
                    IntervalParseError::FieldValueOutOfRange,
                )?;
                i += 2;
                continue;
            }

            let unit = tokens
                .get(i + 2)
                .map(|value| value.to_ascii_lowercase())
                .ok_or(IntervalParseError::InvalidSyntax)?;
            if let Some(error) = standalone_year_overflow_error(
                &signed_token,
                &unit,
                months,
                days,
                micros,
                tokens.len().saturating_sub(i + 3),
            ) {
                return Err(error);
            }
            let num = parse_interval_number(&signed_token)?;
            if apply_interval_unit_str(&signed_token, &unit, &mut months, &mut days, &mut micros)? {
                if let Some(field) = interval_input_field(&unit) {
                    seen_fields.note_unit(field, num)?;
                }
                i += 3;
                continue;
            }

            return Err(IntervalParseError::InvalidSyntax);
        }

        if token.eq_ignore_ascii_case("ago") {
            negate = true;
            i += 1;
            continue;
        }

        let time_part = token.strip_prefix('+').unwrap_or(token);
        if time_part.contains(':') {
            let neg = token.starts_with('-');
            let unsigned = token.trim_start_matches(['+', '-']);
            seen_fields.note_time_token(unsigned)?;
            let val = parse_time_to_micros(unsigned)?;
            let signed = if neg { -val } else { val };
            add_bounded(
                &mut micros,
                signed,
                MICROS_MIN,
                MICROS_MAX,
                IntervalParseError::FieldValueOutOfRange,
            )?;
            i += 1;
            continue;
        }

        if let Some(total_months) = try_year_month_token(token)? {
            seen_fields.note(IntervalInputField::Year)?;
            seen_fields.note(IntervalInputField::Month)?;
            add_bounded(
                &mut months,
                total_months,
                MONTHS_MIN,
                MONTHS_MAX,
                IntervalParseError::OutOfRange,
            )?;
            i += 1;
            continue;
        }

        if let Some((num_str, unit)) = split_compact_numeric_unit(token) {
            if let Some(error) = standalone_year_overflow_error(
                num_str,
                unit,
                months,
                days,
                micros,
                tokens.len().saturating_sub(i + 1),
            ) {
                return Err(error);
            }
            let num = parse_interval_number(num_str)?;
            if apply_interval_unit_str(num_str, unit, &mut months, &mut days, &mut micros)? {
                if let Some(field) = interval_input_field(unit) {
                    seen_fields.note_unit(field, num)?;
                }
                i += 1;
                continue;
            }
        }

        if i + 1 < tokens.len() {
            let next_lower = tokens[i + 1].to_ascii_lowercase();
            if let Ok(num) = token.parse::<f64>() {
                if let Some(error) = standalone_year_overflow_error(
                    token,
                    &next_lower,
                    months,
                    days,
                    micros,
                    tokens.len().saturating_sub(i + 2),
                ) {
                    return Err(error);
                }
                if apply_interval_unit_str(token, &next_lower, &mut months, &mut days, &mut micros)?
                {
                    if let Some(field) = interval_input_field(&next_lower) {
                        seen_fields.note_unit(field, num)?;
                    }
                    i += 2;
                    continue;
                }
            }
        }

        if let Ok(num) = token.parse::<f64>() {
            if i + 1 < tokens.len() {
                let next = tokens[i + 1];
                let next_time = next
                    .strip_prefix('+')
                    .or_else(|| next.strip_prefix('-'))
                    .unwrap_or(next);
                if next_time.contains(':') {
                    add_days_with_fraction(
                        num,
                        &mut days,
                        &mut micros,
                        IntervalParseError::FieldValueOutOfRange,
                    )?;
                    i += 1;
                    continue;
                }
                if next.parse::<f64>().is_ok() {
                    return Err(IntervalParseError::InvalidSyntax);
                }
            }

            let whole_seconds = scaled_to_bounded_i128(
                num,
                i128_to_f64(SECOND_MICROS),
                MICROS_MIN,
                MICROS_MAX,
                IntervalParseError::FieldValueOutOfRange,
            )?;
            add_bounded(
                &mut micros,
                whole_seconds,
                MICROS_MIN,
                MICROS_MAX,
                IntervalParseError::FieldValueOutOfRange,
            )?;
            i += 1;
            continue;
        }

        return Err(IntervalParseError::InvalidSyntax);
    }

    if negate {
        months = negate_bounded(
            months,
            MONTHS_MIN,
            MONTHS_MAX,
            IntervalParseError::FieldValueOutOfRange,
        )?;
        days = negate_bounded(
            days,
            DAYS_MIN,
            DAYS_MAX,
            IntervalParseError::FieldValueOutOfRange,
        )?;
        micros = negate_bounded(
            micros,
            MICROS_MIN,
            MICROS_MAX,
            IntervalParseError::FieldValueOutOfRange,
        )?;
    }

    Ok(Value::Interval(IntervalValue::new(
        i32::try_from(months).map_err(|_| IntervalParseError::OutOfRange)?,
        i32::try_from(days).map_err(|_| IntervalParseError::OutOfRange)?,
        i64::try_from(micros).map_err(|_| IntervalParseError::OutOfRange)?,
    )))
}

fn sql_standard_force_negative_literal(input: &str) -> Option<String> {
    if crate::eval::session::current_interval_style() != EvalIntervalStyle::SqlStandard {
        return None;
    }
    let trimmed = input.trim();
    if trimmed.is_empty()
        || trimmed.starts_with('P')
        || trimmed.starts_with('p')
        || trimmed.contains("ago")
    {
        return None;
    }

    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    let first_idx = tokens.iter().position(|token| *token != "@")?;
    let first = tokens[first_idx];
    let replacement = if first == "-" {
        ""
    } else {
        first.strip_prefix('-')?
    };

    let has_later_explicit_sign = tokens.iter().skip(first_idx + 1).any(|token| {
        *token == "+" || *token == "-" || token.starts_with('+') || token.starts_with('-')
    });
    if has_later_explicit_sign {
        return None;
    }

    let mut rebuilt = Vec::with_capacity(tokens.len());
    for (idx, token) in tokens.iter().enumerate() {
        if idx == first_idx {
            if !replacement.is_empty() {
                rebuilt.push(replacement);
            }
        } else {
            rebuilt.push(token);
        }
    }
    Some(rebuilt.join(" "))
}

fn standalone_year_overflow_error(
    num_str: &str,
    unit: &str,
    months: i128,
    days: i128,
    micros: i128,
    remaining_tokens: usize,
) -> Option<IntervalParseError> {
    if remaining_tokens != 0 || months != 0 || days != 0 || micros != 0 {
        return None;
    }

    // Avoid `to_ascii_lowercase()` String alloc - match the small
    // year-token list via `eq_ignore_ascii_case`.
    const YEAR_TOKENS: &[&str] = &["year", "years", "yr", "yrs", "y"];
    if !YEAR_TOKENS.iter().any(|t| unit.eq_ignore_ascii_case(t)) {
        return None;
    }

    let value = parse_interval_integer(num_str).ok()?;
    let total_months = value.checked_mul(12)?;
    if !(MONTHS_MIN..=MONTHS_MAX).contains(&total_months)
        && (i128::from(i32::MIN)..=i128::from(i32::MAX)).contains(&value)
    {
        // Report this as a field-level overflow so the error carries the
        // offending input token, matching the day/month overflow path and
        // PostgreSQL's message shape.
        Some(IntervalParseError::FieldValueOutOfRange)
    } else {
        None
    }
}

fn parse_interval_with_fields_internal(
    input: &str,
    start: IntervalFieldQualifier,
    end: IntervalFieldQualifier,
    second_precision: Option<u32>,
) -> Result<IntervalValue, IntervalParseError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(IntervalParseError::InvalidSyntax);
    }

    let mut interval = if is_year_month_field(start) && is_year_month_field(end) {
        parse_year_month_interval_with_fields(trimmed, start, end)?
    } else {
        parse_day_time_interval_with_fields(trimmed, start, end)?
    };

    interval = truncate_interval_with_fields(&interval, end, None)?;
    if let Some(precision) = second_precision {
        interval = round_interval_second_precision(&interval, precision)?;
    }
    Ok(interval)
}

fn parse_year_month_interval_with_fields(
    input: &str,
    start: IntervalFieldQualifier,
    end: IntervalFieldQualifier,
) -> Result<IntervalValue, IntervalParseError> {
    let months = match (start, end) {
        (IntervalFieldQualifier::Year, IntervalFieldQualifier::Year) => {
            parse_interval_integer(input)?
                .checked_mul(12)
                .ok_or(IntervalParseError::OutOfRange)?
        }
        (IntervalFieldQualifier::Month, IntervalFieldQualifier::Month) => {
            parse_interval_integer(input)?
        }
        (IntervalFieldQualifier::Year, IntervalFieldQualifier::Month) => {
            if let Some((years, months)) = split_year_month_literal(input) {
                years
                    .checked_mul(12)
                    .and_then(|value| value.checked_add(months))
                    .ok_or(IntervalParseError::OutOfRange)?
            } else {
                parse_interval_integer(input)?
            }
        }
        _ => return Err(IntervalParseError::InvalidSyntax),
    };
    build_interval_value(months, 0, 0)
}

fn parse_day_time_interval_with_fields(
    input: &str,
    start: IntervalFieldQualifier,
    end: IntervalFieldQualifier,
) -> Result<IntervalValue, IntervalParseError> {
    let tokens: Vec<&str> = input.split_whitespace().collect();
    let (days, micros) = match tokens.as_slice() {
        [single] if start == IntervalFieldQualifier::Day && end == IntervalFieldQualifier::Day => {
            (parse_interval_integer(single)?, 0)
        }
        [single] => (0, parse_day_time_payload(single, start, end)?),
        [day_token, payload] => {
            let days = parse_interval_integer(day_token)?;
            let micros = parse_day_time_payload(payload, start, end)?;
            (days, micros)
        }
        _ => return Err(IntervalParseError::InvalidSyntax),
    };
    build_interval_value(0, days, micros)
}

fn parse_day_time_payload(
    token: &str,
    start: IntervalFieldQualifier,
    end: IntervalFieldQualifier,
) -> Result<i128, IntervalParseError> {
    if token.contains(':') {
        return parse_interval_time_payload(token, start, end);
    }

    match (start, end) {
        (
            IntervalFieldQualifier::Hour | IntervalFieldQualifier::Day,
            IntervalFieldQualifier::Hour,
        ) => parse_interval_integer(token)?
            .checked_mul(HOUR_MICROS)
            .ok_or(IntervalParseError::OutOfRange),
        (IntervalFieldQualifier::Minute, IntervalFieldQualifier::Minute) => {
            parse_interval_integer(token)?
                .checked_mul(MINUTE_MICROS)
                .ok_or(IntervalParseError::OutOfRange)
        }
        (IntervalFieldQualifier::Second, IntervalFieldQualifier::Second) => {
            let numeric = parse_interval_exact_number(token)?;
            exact_scaled_to_bounded_i128_rounded(
                &numeric,
                SECOND_MICROS,
                MICROS_MIN,
                MICROS_MAX,
                IntervalParseError::FieldValueOutOfRange,
            )
        }
        _ => Err(IntervalParseError::InvalidSyntax),
    }
}

fn parse_interval_time_payload(
    token: &str,
    start: IntervalFieldQualifier,
    end: IntervalFieldQualifier,
) -> Result<i128, IntervalParseError> {
    let negative = token.starts_with('-');
    let unsigned = token.trim_start_matches(['+', '-']);
    let (main, subsec) = match unsigned.split_once('.') {
        Some((head, tail)) => (head, Some(tail)),
        None => (unsigned, None),
    };
    let parts: Vec<&str> = main.split(':').collect();
    if parts.len() < 2 || parts.len() > 3 {
        return Err(IntervalParseError::InvalidSyntax);
    }

    let fields: Vec<IntervalFieldQualifier> = match parts.len() {
        2 => match (start, end, subsec.is_some()) {
            (IntervalFieldQualifier::Minute, IntervalFieldQualifier::Second, _) => {
                vec![
                    IntervalFieldQualifier::Minute,
                    IntervalFieldQualifier::Second,
                ]
            }
            (_, IntervalFieldQualifier::Hour, _) => {
                vec![IntervalFieldQualifier::Hour, IntervalFieldQualifier::Minute]
            }
            (_, IntervalFieldQualifier::Second, true) => {
                vec![
                    IntervalFieldQualifier::Minute,
                    IntervalFieldQualifier::Second,
                ]
            }
            _ => vec![IntervalFieldQualifier::Hour, IntervalFieldQualifier::Minute],
        },
        3 => vec![
            IntervalFieldQualifier::Hour,
            IntervalFieldQualifier::Minute,
            IntervalFieldQualifier::Second,
        ],
        _ => return Err(IntervalParseError::InvalidSyntax),
    };

    if end == IntervalFieldQualifier::Minute && parts.len() < 2 {
        return Err(IntervalParseError::InvalidSyntax);
    }
    if end == IntervalFieldQualifier::Second
        && parts.len() == 1
        && start != IntervalFieldQualifier::Second
    {
        return Err(IntervalParseError::InvalidSyntax);
    }
    if subsec.is_some()
        && *fields.last().unwrap_or(&IntervalFieldQualifier::Second)
            != IntervalFieldQualifier::Second
    {
        return Err(IntervalParseError::InvalidSyntax);
    }

    let mut total = 0i128;
    for (idx, part) in parts.iter().enumerate() {
        let component = part
            .parse::<i128>()
            .map_err(|_| IntervalParseError::InvalidSyntax)?;
        total = match fields[idx] {
            IntervalFieldQualifier::Hour => total
                .checked_add(
                    component
                        .checked_mul(HOUR_MICROS)
                        .ok_or(IntervalParseError::OutOfRange)?,
                )
                .ok_or(IntervalParseError::OutOfRange)?,
            IntervalFieldQualifier::Minute => total
                .checked_add(
                    component
                        .checked_mul(MINUTE_MICROS)
                        .ok_or(IntervalParseError::OutOfRange)?,
                )
                .ok_or(IntervalParseError::OutOfRange)?,
            IntervalFieldQualifier::Second => total
                .checked_add(
                    component
                        .checked_mul(SECOND_MICROS)
                        .ok_or(IntervalParseError::OutOfRange)?,
                )
                .ok_or(IntervalParseError::OutOfRange)?,
            _ => return Err(IntervalParseError::InvalidSyntax),
        };
    }
    if let Some(frac) = subsec {
        let padded = format!("{frac:0<6}");
        let micros = padded[..6]
            .parse::<i128>()
            .map_err(|_| IntervalParseError::InvalidSyntax)?;
        total = total
            .checked_add(micros)
            .ok_or(IntervalParseError::OutOfRange)?;
    }

    if negative {
        negate_bounded(
            total,
            MICROS_MIN,
            MICROS_MAX,
            IntervalParseError::OutOfRange,
        )
    } else {
        if !(MICROS_MIN..=MICROS_MAX).contains(&total) {
            return Err(IntervalParseError::OutOfRange);
        }
        Ok(total)
    }
}

fn truncate_interval_with_fields(
    interval: &IntervalValue,
    end: IntervalFieldQualifier,
    second_precision: Option<u32>,
) -> Result<IntervalValue, IntervalParseError> {
    let mut months = i128::from(interval.months);
    let mut days = i128::from(interval.days);
    let mut micros = i128::from(interval.micros);

    match end {
        IntervalFieldQualifier::Year => {
            months = (months / 12) * 12;
            days = 0;
            micros = 0;
        }
        IntervalFieldQualifier::Month => {
            days = 0;
            micros = 0;
        }
        IntervalFieldQualifier::Day => {
            micros = 0;
        }
        IntervalFieldQualifier::Hour => {
            micros = truncate_toward_zero(micros, HOUR_MICROS);
        }
        IntervalFieldQualifier::Minute => {
            micros = truncate_toward_zero(micros, MINUTE_MICROS);
        }
        IntervalFieldQualifier::Second => {}
    }

    let mut truncated = build_interval_value(months, days, micros)?;
    if let Some(precision) = second_precision {
        truncated = round_interval_second_precision(&truncated, precision)?;
    }
    Ok(truncated)
}

fn round_interval_second_precision(
    interval: &IntervalValue,
    precision: u32,
) -> Result<IntervalValue, IntervalParseError> {
    if precision > 6 {
        return Err(IntervalParseError::InvalidSyntax);
    }
    let rounding_unit = 10_i128.pow(6 - precision);
    let micros = i128::from(interval.micros);
    let rounded_micros = if micros >= 0 {
        ((micros + rounding_unit / 2) / rounding_unit) * rounding_unit
    } else {
        ((micros - rounding_unit / 2) / rounding_unit) * rounding_unit
    };
    let day_delta = rounded_micros / DAY_MICROS;
    let remaining_micros = rounded_micros % DAY_MICROS;
    build_interval_value(
        i128::from(interval.months),
        i128::from(interval.days)
            .checked_add(day_delta)
            .ok_or(IntervalParseError::OutOfRange)?,
        remaining_micros,
    )
}

fn truncate_toward_zero(value: i128, unit: i128) -> i128 {
    (value / unit) * unit
}

pub(super) fn build_interval_value(
    months: i128,
    days: i128,
    micros: i128,
) -> Result<IntervalValue, IntervalParseError> {
    Ok(IntervalValue::new(
        i32::try_from(months).map_err(|_| IntervalParseError::OutOfRange)?,
        i32::try_from(days).map_err(|_| IntervalParseError::OutOfRange)?,
        i64::try_from(micros).map_err(|_| IntervalParseError::OutOfRange)?,
    ))
}

pub(super) fn parse_interval_integer(input: &str) -> Result<i128, IntervalParseError> {
    input
        .parse::<i128>()
        .map_err(|_| IntervalParseError::InvalidSyntax)
}

fn split_year_month_literal(input: &str) -> Option<(i128, i128)> {
    let (sign, body) = if let Some(rest) = input.strip_prefix('-') {
        (-1i128, rest)
    } else if let Some(rest) = input.strip_prefix('+') {
        (1i128, rest)
    } else {
        (1i128, input)
    };
    let (years, months) = body.split_once('-')?;
    let years = years.parse::<i128>().ok()?;
    let months = months.parse::<i128>().ok()?;
    Some((years.checked_mul(sign)?, months.checked_mul(sign)?))
}

pub(super) fn parse_interval_field_qualifier(field: &str) -> DbResult<IntervalFieldQualifier> {
    // Skip `to_ascii_lowercase()` String alloc - six-token match via
    // `eq_ignore_ascii_case`.
    if field.eq_ignore_ascii_case("year") {
        return Ok(IntervalFieldQualifier::Year);
    }
    if field.eq_ignore_ascii_case("month") {
        return Ok(IntervalFieldQualifier::Month);
    }
    if field.eq_ignore_ascii_case("day") {
        return Ok(IntervalFieldQualifier::Day);
    }
    if field.eq_ignore_ascii_case("hour") {
        return Ok(IntervalFieldQualifier::Hour);
    }
    if field.eq_ignore_ascii_case("minute") {
        return Ok(IntervalFieldQualifier::Minute);
    }
    if field.eq_ignore_ascii_case("second") {
        return Ok(IntervalFieldQualifier::Second);
    }
    Err(DbError::internal(format!(
        "__aiondb_interval_fields() unknown interval field qualifier: {field}"
    )))
}

pub(super) fn is_year_month_field(field: IntervalFieldQualifier) -> bool {
    matches!(
        field,
        IntervalFieldQualifier::Year | IntervalFieldQualifier::Month
    )
}

pub(super) fn interval_field_runtime_error(error: IntervalParseError) -> DbError {
    match error {
        IntervalParseError::OutOfRange => IntervalParseError::OutOfRange.into_db_error("interval"),
        IntervalParseError::FieldValueOutOfRange => {
            IntervalParseError::FieldValueOutOfRange.into_db_error("interval")
        }
        IntervalParseError::InvalidSyntax => DbError::internal(
            "__aiondb_interval_fields() produced invalid interval syntax for non-text input",
        ),
    }
}

fn try_internal_interval_format(s: &str) -> Option<Value> {
    // "Nm Nd Nus" - internal Display format
    let mut months = 0i32;
    let mut days = 0i32;
    let mut micros = 0i64;
    let mut matched_any = false;
    for part in s.split_whitespace() {
        if let Some(val) = part.strip_suffix("us") {
            micros = val.parse().ok()?;
            matched_any = true;
        } else if let Some(val) = part.strip_suffix('d') {
            if val.chars().all(|c| c.is_ascii_digit() || c == '-') {
                days = val.parse().ok()?;
                matched_any = true;
            } else {
                return None;
            }
        } else if let Some(val) = part.strip_suffix('m') {
            if val.chars().all(|c| c.is_ascii_digit() || c == '-') {
                months = val.parse().ok()?;
                matched_any = true;
            } else {
                return None;
            }
        } else {
            return None;
        }
    }
    if matched_any {
        Some(Value::Interval(IntervalValue::new(months, days, micros)))
    } else {
        None
    }
}

fn try_year_month_shorthand(s: &str) -> Result<Option<Value>, IntervalParseError> {
    let Some(total_months) = try_year_month_token(s)? else {
        return Ok(None);
    };
    Ok(Some(Value::Interval(IntervalValue::new(
        i32::try_from(total_months).map_err(|_| IntervalParseError::OutOfRange)?,
        0,
        0,
    ))))
}

fn try_year_month_token(token: &str) -> Result<Option<i128>, IntervalParseError> {
    let (sign, rest) = if let Some(rest) = token.strip_prefix('+') {
        (1i128, rest)
    } else if let Some(rest) = token.strip_prefix('-') {
        (-1i128, rest)
    } else {
        (1i128, token)
    };
    let Some((years_str, months_str)) = rest.split_once('-') else {
        return Ok(None);
    };
    if years_str.is_empty()
        || months_str.is_empty()
        || !years_str.chars().all(|ch| ch.is_ascii_digit())
        || !months_str.chars().all(|ch| ch.is_ascii_digit())
    {
        return Ok(None);
    }

    let years = years_str
        .parse::<i128>()
        .map_err(|_| IntervalParseError::OutOfRange)?;
    let months = months_str
        .parse::<i128>()
        .map_err(|_| IntervalParseError::OutOfRange)?;
    let total_months = years
        .checked_mul(12)
        .and_then(|value| value.checked_add(months))
        .and_then(|value| value.checked_mul(sign))
        .ok_or(IntervalParseError::OutOfRange)?;
    if !(MONTHS_MIN..=MONTHS_MAX).contains(&total_months) {
        return Err(IntervalParseError::OutOfRange);
    }

    Ok(Some(total_months))
}
