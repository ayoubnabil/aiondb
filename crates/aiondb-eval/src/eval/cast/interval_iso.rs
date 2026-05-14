use crate::eval::scalar_functions::value_convert::i128_to_f64;
use aiondb_core::{IntervalValue, NumericValue, Value};

use super::interval::*;

pub(super) fn parse_iso8601_interval(s: &str) -> Result<Value, IntervalParseError> {
    let s = &s[1..];
    let mut months: i128 = 0;
    let mut days: i128 = 0;
    let mut micros: i128 = 0;

    let (date_part, time_part) = if let Some(t_pos) = s.find('T').or_else(|| s.find('t')) {
        (&s[..t_pos], Some(&s[t_pos + 1..]))
    } else {
        (s, None)
    };

    if !date_part.is_empty() {
        if iso_date_part_has_designators(date_part) {
            parse_iso_date_components(date_part, &mut months, &mut days, &mut micros)?;
        } else {
            parse_iso_alternative_date(date_part, &mut months, &mut days)?;
        }
    }

    if let Some(tp) = time_part {
        if iso_time_part_has_designators(tp) {
            parse_iso_time_components(tp, &mut micros)?;
        } else {
            parse_iso_alternative_time(tp, &mut micros)?;
        }
    }

    Ok(Value::Interval(IntervalValue::new(
        i32::try_from(months).map_err(|_| IntervalParseError::OutOfRange)?,
        i32::try_from(days).map_err(|_| IntervalParseError::OutOfRange)?,
        i64::try_from(micros).map_err(|_| IntervalParseError::OutOfRange)?,
    )))
}

pub(super) fn iso_date_part_has_designators(s: &str) -> bool {
    s.chars()
        .any(|ch| matches!(ch, 'Y' | 'y' | 'M' | 'm' | 'W' | 'w' | 'D' | 'd'))
}

pub(super) fn iso_time_part_has_designators(s: &str) -> bool {
    s.chars()
        .any(|ch| matches!(ch, 'H' | 'h' | 'M' | 'm' | 'S' | 's'))
}

pub(super) fn parse_iso_alternative_date(
    s: &str,
    months: &mut i128,
    days: &mut i128,
) -> Result<(), IntervalParseError> {
    let (years, month, day) = if s.contains('-') {
        let parts = split_iso_alternative_components(s, '-');
        if parts.iter().any(|part| part.is_empty()) {
            return Err(IntervalParseError::InvalidSyntax);
        }

        match parts.as_slice() {
            [year] => (parse_iso_alternative_i32_component(year)?, None, None),
            [year, month] => (
                parse_iso_alternative_i32_component(year)?,
                Some(parse_iso_alternative_i32_component(month)?),
                None,
            ),
            [year, month, day] => (
                parse_iso_alternative_i32_component(year)?,
                Some(parse_iso_alternative_i32_component(month)?),
                Some(parse_iso_alternative_i32_component(day)?),
            ),
            _ => return Err(IntervalParseError::InvalidSyntax),
        }
    } else if let Some((years, month, day)) = parse_compact_iso_alternative_date(s)? {
        (years, month, day)
    } else {
        (parse_iso_alternative_i32_component(s)?, None, None)
    };

    let total_months = years
        .checked_mul(12)
        .and_then(|value| value.checked_add(month.unwrap_or(0)))
        .ok_or(IntervalParseError::OutOfRange)?;
    add_bounded(
        months,
        total_months,
        MONTHS_MIN,
        MONTHS_MAX,
        IntervalParseError::OutOfRange,
    )?;
    if let Some(day) = day {
        add_bounded(
            days,
            day,
            DAYS_MIN,
            DAYS_MAX,
            IntervalParseError::OutOfRange,
        )?;
    }
    Ok(())
}

pub(super) fn parse_iso_alternative_time(
    s: &str,
    micros: &mut i128,
) -> Result<(), IntervalParseError> {
    if s.is_empty() {
        return Err(IntervalParseError::InvalidSyntax);
    }

    let delta = if s.contains(':') {
        parse_flexible_iso_time_to_micros(s)?
    } else if let Some(compact) = parse_compact_iso_alternative_time_to_micros(s)? {
        compact
    } else {
        parse_bare_iso_alternative_hours_to_micros(s)?
    };

    add_bounded(
        micros,
        delta,
        MICROS_MIN,
        MICROS_MAX,
        IntervalParseError::OutOfRange,
    )
}

pub(super) fn parse_compact_iso_alternative_date(
    s: &str,
) -> Result<Option<(i128, Option<i128>, Option<i128>)>, IntervalParseError> {
    let (sign, digits) = split_signed_digits(s)?;
    let parsed = match digits.len() {
        6 => Some((
            parse_iso_alternative_i32_component(&digits[..4])? * sign,
            Some(parse_iso_alternative_i32_component(&digits[4..6])? * sign),
            None,
        )),
        8 => Some((
            parse_iso_alternative_i32_component(&digits[..4])? * sign,
            Some(parse_iso_alternative_i32_component(&digits[4..6])? * sign),
            Some(parse_iso_alternative_i32_component(&digits[6..8])? * sign),
        )),
        _ => None,
    };
    Ok(parsed)
}

pub(super) fn parse_compact_iso_alternative_time_to_micros(
    s: &str,
) -> Result<Option<i128>, IntervalParseError> {
    let (main, frac) = match s.split_once('.') {
        Some((main, frac)) => (main, Some(frac)),
        None => (s, None),
    };
    let (sign, digits) = split_signed_digits(main)?;
    if !matches!(digits.len(), 2 | 4 | 6) {
        return Ok(None);
    }

    let hours = parse_iso_alternative_integer_component(&digits[..2])?;
    let minutes = if digits.len() >= 4 {
        parse_iso_alternative_integer_component(&digits[2..4])?
    } else {
        0
    };
    let seconds = if digits.len() == 6 {
        let mut seconds = digits[4..6].to_owned();
        if let Some(frac) = frac {
            seconds.push('.');
            seconds.push_str(frac);
        }
        parse_iso_alternative_seconds_component(&seconds)?
    } else {
        if frac.is_some() {
            return Err(IntervalParseError::FieldValueOutOfRange);
        }
        0
    };

    let total = hours
        .checked_mul(HOUR_MICROS)
        .and_then(|value| value.checked_add(minutes.checked_mul(MINUTE_MICROS)?))
        .and_then(|value| value.checked_add(seconds))
        .ok_or(IntervalParseError::FieldValueOutOfRange)?;
    Ok(Some(total * sign))
}

pub(super) fn split_signed_digits(s: &str) -> Result<(i128, &str), IntervalParseError> {
    if s.is_empty() {
        return Err(IntervalParseError::InvalidSyntax);
    }

    let (sign, digits) = if let Some(rest) = s.strip_prefix('-') {
        (-1, rest)
    } else if let Some(rest) = s.strip_prefix('+') {
        (1, rest)
    } else {
        (1, s)
    };
    if digits.is_empty() || !digits.chars().all(|ch| ch.is_ascii_digit()) {
        return Err(IntervalParseError::InvalidSyntax);
    }
    Ok((sign, digits))
}

pub(super) fn split_iso_alternative_components(s: &str, separator: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    for (idx, ch) in s.char_indices() {
        if ch == separator && idx != 0 {
            parts.push(&s[start..idx]);
            start = idx + ch.len_utf8();
        }
    }
    parts.push(&s[start..]);
    parts
}

pub(super) fn parse_iso_alternative_i32_component(value: &str) -> Result<i128, IntervalParseError> {
    let parsed = parse_iso_alternative_integer_component(value)?;
    if !(i32::MIN as i128..=i32::MAX as i128).contains(&parsed) {
        return Err(IntervalParseError::FieldValueOutOfRange);
    }
    Ok(parsed)
}

pub(super) fn parse_iso_alternative_integer_component(
    value: &str,
) -> Result<i128, IntervalParseError> {
    if value.is_empty() {
        return Err(IntervalParseError::InvalidSyntax);
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_digit() || matches!(ch, '+' | '-' | '.'))
    {
        return Err(IntervalParseError::InvalidSyntax);
    }
    if value.contains('.') {
        return Err(IntervalParseError::FieldValueOutOfRange);
    }
    value
        .parse::<i128>()
        .map_err(|_| IntervalParseError::FieldValueOutOfRange)
}

pub(super) fn parse_flexible_iso_time_to_micros(s: &str) -> Result<i128, IntervalParseError> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() < 2 || parts.len() > 3 || parts.iter().any(|part| part.is_empty()) {
        return Err(IntervalParseError::InvalidSyntax);
    }

    let hours = parse_iso_alternative_integer_component(parts[0])?;
    let minutes = parse_iso_alternative_integer_component(parts[1])?;
    let seconds = if parts.len() == 3 {
        parse_iso_alternative_seconds_component(parts[2])?
    } else {
        if parts[1].contains('.') {
            return Err(IntervalParseError::FieldValueOutOfRange);
        }
        0
    };

    let total = hours
        .checked_mul(HOUR_MICROS)
        .and_then(|value| value.checked_add(minutes.checked_mul(MINUTE_MICROS)?))
        .and_then(|value| value.checked_add(seconds))
        .ok_or(IntervalParseError::FieldValueOutOfRange)?;
    if !(MICROS_MIN..=MICROS_MAX).contains(&total) {
        return Err(IntervalParseError::FieldValueOutOfRange);
    }
    Ok(total)
}

pub(super) fn parse_iso_alternative_seconds_component(
    value: &str,
) -> Result<i128, IntervalParseError> {
    if value.is_empty() {
        return Err(IntervalParseError::InvalidSyntax);
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_digit() || matches!(ch, '+' | '-' | '.'))
    {
        return Err(IntervalParseError::InvalidSyntax);
    }
    let numeric =
        parse_interval_exact_number(value).map_err(|_| IntervalParseError::FieldValueOutOfRange)?;
    exact_scaled_to_bounded_i128_rounded(
        &numeric,
        SECOND_MICROS,
        MICROS_MIN,
        MICROS_MAX,
        IntervalParseError::FieldValueOutOfRange,
    )
}

pub(super) fn parse_bare_iso_alternative_hours_to_micros(
    s: &str,
) -> Result<i128, IntervalParseError> {
    if !s
        .chars()
        .all(|ch| ch.is_ascii_digit() || matches!(ch, '+' | '-' | '.'))
    {
        return Err(IntervalParseError::InvalidSyntax);
    }
    let value = parse_interval_number(s)?;
    if !value.is_finite() {
        return Err(IntervalParseError::FieldValueOutOfRange);
    }
    let whole_hours = trunc_to_bounded_i128(
        value,
        MICROS_MIN / HOUR_MICROS,
        MICROS_MAX / HOUR_MICROS,
        IntervalParseError::FieldValueOutOfRange,
    )?;
    let fractional_micros = rounded_scaled_to_bounded_i128(
        value - i128_to_f64(whole_hours),
        i128_to_f64(HOUR_MICROS),
        -HOUR_MICROS,
        HOUR_MICROS,
        IntervalParseError::FieldValueOutOfRange,
    )?;
    let total = whole_hours
        .checked_mul(HOUR_MICROS)
        .and_then(|hours| hours.checked_add(fractional_micros))
        .ok_or(IntervalParseError::FieldValueOutOfRange)?;
    if !(MICROS_MIN..=MICROS_MAX).contains(&total) {
        return Err(IntervalParseError::FieldValueOutOfRange);
    }
    Ok(total)
}

pub(super) fn rounded_scaled_to_bounded_i128(
    value: f64,
    scale: f64,
    min: i128,
    max: i128,
    error: IntervalParseError,
) -> Result<i128, IntervalParseError> {
    let scaled = value * scale;
    if !scaled.is_finite() {
        return Err(error);
    }
    let rounded = scaled.round();
    if rounded < i128_to_f64(min) || rounded > i128_to_f64(max) {
        return Err(error);
    }
    format!("{rounded:.0}").parse::<i128>().map_err(|_| error)
}

pub(super) fn parse_iso_date_components(
    s: &str,
    months: &mut i128,
    days: &mut i128,
    micros: &mut i128,
) -> Result<(), IntervalParseError> {
    let mut num_buf = String::new();
    for ch in s.chars() {
        match ch {
            'Y' | 'y' => {
                if num_buf.is_empty() {
                    return Err(IntervalParseError::InvalidSyntax);
                }
                let n = parse_interval_number(&num_buf)?;
                let error = IntervalParseError::FieldValueOutOfRange;
                let delta = if n.fract() == 0.0 {
                    trunc_to_bounded_i128(n, i32::MIN as i128, i32::MAX as i128, error)?
                        .checked_mul(12)
                        .ok_or(error)?
                } else {
                    scaled_to_bounded_i128(n, 12.0, MONTHS_MIN, MONTHS_MAX, error)?
                };
                add_bounded(months, delta, MONTHS_MIN, MONTHS_MAX, error)?;
                num_buf.clear();
            }
            'M' | 'm' => {
                if num_buf.is_empty() {
                    return Err(IntervalParseError::InvalidSyntax);
                }
                let n = parse_interval_number(&num_buf)?;
                let delta = trunc_to_bounded_i128(
                    n,
                    MONTHS_MIN,
                    MONTHS_MAX,
                    IntervalParseError::FieldValueOutOfRange,
                )?;
                add_bounded(
                    months,
                    delta,
                    MONTHS_MIN,
                    MONTHS_MAX,
                    IntervalParseError::FieldValueOutOfRange,
                )?;
                if n.fract() != 0.0 {
                    add_days_with_fraction(
                        n.fract() * 30.0,
                        days,
                        micros,
                        IntervalParseError::FieldValueOutOfRange,
                    )?;
                }
                num_buf.clear();
            }
            'W' | 'w' => {
                if num_buf.is_empty() {
                    return Err(IntervalParseError::InvalidSyntax);
                }
                let n = parse_interval_number(&num_buf)?;
                let error = IntervalParseError::FieldValueOutOfRange;
                add_days_with_fraction(n * 7.0, days, micros, error)?;
                num_buf.clear();
            }
            'D' | 'd' => {
                if num_buf.is_empty() {
                    return Err(IntervalParseError::InvalidSyntax);
                }
                let n = parse_interval_number(&num_buf)?;
                add_days_with_fraction(n, days, micros, IntervalParseError::FieldValueOutOfRange)?;
                num_buf.clear();
            }
            _ if ch.is_ascii_digit()
                || ch == '.'
                || ch == '-'
                || ch == '+'
                || ch == 'e'
                || ch == 'E' =>
            {
                num_buf.push(ch);
            }
            _ => return Err(IntervalParseError::InvalidSyntax),
        }
    }

    if !num_buf.is_empty() {
        let n = parse_interval_number(&num_buf)?;
        add_days_with_fraction(n, days, micros, IntervalParseError::FieldValueOutOfRange)?;
    }

    Ok(())
}

pub(super) fn parse_iso_time_components(
    s: &str,
    micros: &mut i128,
) -> Result<(), IntervalParseError> {
    if s.contains(':') {
        let val = parse_time_to_micros(s)?;
        add_bounded(
            micros,
            val,
            MICROS_MIN,
            MICROS_MAX,
            IntervalParseError::FieldValueOutOfRange,
        )?;
        return Ok(());
    }

    let mut num_buf = String::new();
    for ch in s.chars() {
        match ch {
            'H' | 'h' => {
                if num_buf.is_empty() {
                    return Err(IntervalParseError::InvalidSyntax);
                }
                let n = parse_interval_number(&num_buf)?;
                let delta = scaled_to_bounded_i128(
                    n,
                    i128_to_f64(HOUR_MICROS),
                    MICROS_MIN,
                    MICROS_MAX,
                    IntervalParseError::FieldValueOutOfRange,
                )?;
                add_bounded(
                    micros,
                    delta,
                    MICROS_MIN,
                    MICROS_MAX,
                    IntervalParseError::FieldValueOutOfRange,
                )?;
                num_buf.clear();
            }
            'M' | 'm' => {
                if num_buf.is_empty() {
                    return Err(IntervalParseError::InvalidSyntax);
                }
                let n = parse_interval_number(&num_buf)?;
                let delta = scaled_to_bounded_i128(
                    n,
                    i128_to_f64(MINUTE_MICROS),
                    MICROS_MIN,
                    MICROS_MAX,
                    IntervalParseError::FieldValueOutOfRange,
                )?;
                add_bounded(
                    micros,
                    delta,
                    MICROS_MIN,
                    MICROS_MAX,
                    IntervalParseError::FieldValueOutOfRange,
                )?;
                num_buf.clear();
            }
            'S' | 's' => {
                if num_buf.is_empty() {
                    return Err(IntervalParseError::InvalidSyntax);
                }
                let n = parse_interval_number(&num_buf)?;
                let delta = scaled_to_bounded_i128(
                    n,
                    i128_to_f64(SECOND_MICROS),
                    MICROS_MIN,
                    MICROS_MAX,
                    IntervalParseError::FieldValueOutOfRange,
                )?;
                add_bounded(
                    micros,
                    delta,
                    MICROS_MIN,
                    MICROS_MAX,
                    IntervalParseError::FieldValueOutOfRange,
                )?;
                num_buf.clear();
            }
            _ if ch.is_ascii_digit()
                || ch == '.'
                || ch == '-'
                || ch == '+'
                || ch == 'e'
                || ch == 'E' =>
            {
                num_buf.push(ch);
            }
            _ => return Err(IntervalParseError::InvalidSyntax),
        }
    }

    if !num_buf.is_empty() {
        let n = parse_interval_number(&num_buf)?;
        let delta = scaled_to_bounded_i128(
            n,
            i128_to_f64(SECOND_MICROS),
            MICROS_MIN,
            MICROS_MAX,
            IntervalParseError::FieldValueOutOfRange,
        )?;
        add_bounded(
            micros,
            delta,
            MICROS_MIN,
            MICROS_MAX,
            IntervalParseError::FieldValueOutOfRange,
        )?;
    }

    Ok(())
}

pub(super) fn parse_time_to_micros(s: &str) -> Result<i128, IntervalParseError> {
    if let (Some(dot_pos), Some(last_colon_pos)) = (s.find('.'), s.rfind(':')) {
        if dot_pos < last_colon_pos {
            let first_colon_pos = s.find(':').unwrap_or(last_colon_pos);
            return if dot_pos < first_colon_pos {
                Err(IntervalParseError::FieldValueOutOfRange)
            } else {
                Err(IntervalParseError::InvalidSyntax)
            };
        }
    }
    let (main, subsec) = match s.split_once('.') {
        Some((m, f)) => (m, Some(f)),
        None => (s, None),
    };
    let parts: Vec<&str> = main.split(':').collect();
    if parts.len() < 2 || parts.len() > 3 {
        return Err(IntervalParseError::InvalidSyntax);
    }

    let hours = parts[0]
        .parse::<i128>()
        .map_err(|_| IntervalParseError::InvalidSyntax)?;
    let minutes = parts[1]
        .parse::<i128>()
        .map_err(|_| IntervalParseError::InvalidSyntax)?;
    let seconds = if parts.len() == 3 {
        parts[2]
            .parse::<i128>()
            .map_err(|_| IntervalParseError::InvalidSyntax)?
    } else {
        0
    };
    let frac_micros = if let Some(frac) = subsec {
        let padded = format!("{frac:0<6}");
        padded[..6]
            .parse::<i128>()
            .map_err(|_| IntervalParseError::InvalidSyntax)?
    } else {
        0
    };

    let total = hours
        .checked_mul(HOUR_MICROS)
        .and_then(|value| value.checked_add(minutes * MINUTE_MICROS))
        .and_then(|value| value.checked_add(seconds * SECOND_MICROS))
        .and_then(|value| value.checked_add(frac_micros))
        .ok_or(IntervalParseError::FieldValueOutOfRange)?;
    if !(MICROS_MIN..=MICROS_MAX).contains(&total) {
        return Err(IntervalParseError::FieldValueOutOfRange);
    }

    Ok(total)
}

pub(super) fn parse_interval_number(value: &str) -> Result<f64, IntervalParseError> {
    value
        .parse::<f64>()
        .map_err(|_| IntervalParseError::InvalidSyntax)
}

pub(super) fn parse_interval_exact_number(value: &str) -> Result<NumericValue, IntervalParseError> {
    value
        .parse::<NumericValue>()
        .map_err(|_| IntervalParseError::InvalidSyntax)
}

pub(super) fn pow10_i128(
    scale: u32,
    error: IntervalParseError,
) -> Result<i128, IntervalParseError> {
    let mut factor = 1i128;
    for _ in 0..scale {
        factor = factor.checked_mul(10).ok_or(error)?;
    }
    Ok(factor)
}

pub(super) fn exact_scaled_to_bounded_i128_rounded(
    value: &NumericValue,
    multiplier: i128,
    min: i128,
    max: i128,
    error: IntervalParseError,
) -> Result<i128, IntervalParseError> {
    if value.is_special() || value.is_big() {
        return Err(error);
    }
    let divisor = pow10_i128(value.scale, error)?;
    let scaled = value.coefficient.checked_mul(multiplier).ok_or(error)?;
    let half = divisor / 2;
    let rounded = if scaled >= 0 {
        scaled.checked_add(half).ok_or(error)? / divisor
    } else {
        scaled.checked_sub(half).ok_or(error)? / divisor
    };
    if !(min..=max).contains(&rounded) {
        return Err(error);
    }
    Ok(rounded)
}

pub(super) fn split_compact_numeric_unit(token: &str) -> Option<(&str, &str)> {
    let unit_start = token
        .char_indices()
        .find_map(|(idx, ch)| ch.is_ascii_alphabetic().then_some(idx))?;
    (unit_start > 0).then_some((&token[..unit_start], &token[unit_start..]))
}

pub(super) fn interval_input_field(unit: &str) -> Option<IntervalInputField> {
    // Skip the `to_ascii_lowercase()` String alloc on the
    // already-lowercase fast path. Only walk the input once to
    // detect any uppercase byte and allocate then.
    let normalized: std::borrow::Cow<'_, str> = if unit.bytes().any(|b| b.is_ascii_uppercase()) {
        std::borrow::Cow::Owned(unit.to_ascii_lowercase())
    } else {
        std::borrow::Cow::Borrowed(unit)
    };
    match normalized.as_ref() {
        "millennium" | "millenniums" | "millennia" => Some(IntervalInputField::Millennium),
        "century" | "centuries" => Some(IntervalInputField::Century),
        "decade" | "decades" => Some(IntervalInputField::Decade),
        "year" | "years" | "yr" | "yrs" | "y" => Some(IntervalInputField::Year),
        "month" | "months" | "mon" | "mons" => Some(IntervalInputField::Month),
        "week" | "weeks" | "w" => Some(IntervalInputField::Week),
        "day" | "days" | "d" => Some(IntervalInputField::Day),
        "hour" | "hours" | "hr" | "hrs" | "h" => Some(IntervalInputField::Hour),
        "minute" | "minutes" | "min" | "mins" => Some(IntervalInputField::Minute),
        "second" | "seconds" | "sec" | "secs" | "s" => Some(IntervalInputField::Second),
        "millisecond" | "milliseconds" | "msec" | "msecs" | "ms" => {
            Some(IntervalInputField::Millisecond)
        }
        "microsecond" | "microseconds" | "usec" | "usecs" | "us" => {
            Some(IntervalInputField::Microsecond)
        }
        _ => None,
    }
}

pub(super) fn trunc_to_bounded_i128(
    value: f64,
    min: i128,
    max: i128,
    error: IntervalParseError,
) -> Result<i128, IntervalParseError> {
    let truncated = value.trunc();
    if !truncated.is_finite() || truncated < i128_to_f64(min) || truncated > i128_to_f64(max) {
        return Err(error);
    }
    format!("{truncated:.0}").parse::<i128>().map_err(|_| error)
}

pub(super) fn scaled_to_bounded_i128(
    value: f64,
    scale: f64,
    min: i128,
    max: i128,
    error: IntervalParseError,
) -> Result<i128, IntervalParseError> {
    trunc_to_bounded_i128(value * scale, min, max, error)
}

pub(super) fn add_bounded(
    total: &mut i128,
    delta: i128,
    min: i128,
    max: i128,
    error: IntervalParseError,
) -> Result<(), IntervalParseError> {
    let updated = total.checked_add(delta).ok_or(error)?;
    if !(min..=max).contains(&updated) {
        return Err(error);
    }
    *total = updated;
    Ok(())
}

pub(super) fn add_days_with_fraction(
    value: f64,
    days: &mut i128,
    micros: &mut i128,
    error: IntervalParseError,
) -> Result<(), IntervalParseError> {
    let whole_days = trunc_to_bounded_i128(value, DAYS_MIN, DAYS_MAX, error)?;
    add_bounded(days, whole_days, DAYS_MIN, DAYS_MAX, error)?;
    let fractional_days = value.fract();
    if fractional_days != 0.0 {
        let delta = rounded_scaled_to_bounded_i128(
            fractional_days,
            i128_to_f64(DAY_MICROS),
            MICROS_MIN,
            MICROS_MAX,
            error,
        )?;
        add_bounded(micros, delta, MICROS_MIN, MICROS_MAX, error)?;
    }
    Ok(())
}

pub(super) fn negate_bounded(
    value: i128,
    min: i128,
    max: i128,
    error: IntervalParseError,
) -> Result<i128, IntervalParseError> {
    let negated = value.checked_neg().ok_or(error)?;
    if !(min..=max).contains(&negated) {
        return Err(error);
    }
    Ok(negated)
}

pub(super) fn apply_interval_unit(
    num: f64,
    unit: &str,
    months: &mut i128,
    days: &mut i128,
    micros: &mut i128,
) -> Result<bool, IntervalParseError> {
    let unit = unit.to_ascii_lowercase();
    match unit.as_str() {
        "millennium" | "millenniums" | "millennia" => {
            let error = IntervalParseError::FieldValueOutOfRange;
            let delta = if num.fract() == 0.0 {
                trunc_to_bounded_i128(num, i32::MIN as i128, i32::MAX as i128, error)?
                    .checked_mul(12_000)
                    .ok_or(error)?
            } else {
                scaled_to_bounded_i128(num, 12_000.0, MONTHS_MIN, MONTHS_MAX, error)?
            };
            add_bounded(months, delta, MONTHS_MIN, MONTHS_MAX, error)?;
            Ok(true)
        }
        "century" | "centuries" => {
            let error = IntervalParseError::FieldValueOutOfRange;
            let delta = if num.fract() == 0.0 {
                trunc_to_bounded_i128(num, i32::MIN as i128, i32::MAX as i128, error)?
                    .checked_mul(1_200)
                    .ok_or(error)?
            } else {
                scaled_to_bounded_i128(num, 1_200.0, MONTHS_MIN, MONTHS_MAX, error)?
            };
            add_bounded(months, delta, MONTHS_MIN, MONTHS_MAX, error)?;
            Ok(true)
        }
        "decade" | "decades" => {
            let error = IntervalParseError::FieldValueOutOfRange;
            let delta = if num.fract() == 0.0 {
                trunc_to_bounded_i128(num, i32::MIN as i128, i32::MAX as i128, error)?
                    .checked_mul(120)
                    .ok_or(error)?
            } else {
                scaled_to_bounded_i128(num, 120.0, MONTHS_MIN, MONTHS_MAX, error)?
            };
            add_bounded(months, delta, MONTHS_MIN, MONTHS_MAX, error)?;
            Ok(true)
        }
        "year" | "years" | "yr" | "yrs" | "y" => {
            let error = IntervalParseError::FieldValueOutOfRange;
            let delta = if num.fract() == 0.0 {
                trunc_to_bounded_i128(num, i32::MIN as i128, i32::MAX as i128, error)?
                    .checked_mul(12)
                    .ok_or(error)?
            } else {
                scaled_to_bounded_i128(num, 12.0, MONTHS_MIN, MONTHS_MAX, error)?
            };
            add_bounded(months, delta, MONTHS_MIN, MONTHS_MAX, error)?;
            Ok(true)
        }
        "month" | "months" | "mon" | "mons" => {
            let whole_months = trunc_to_bounded_i128(
                num,
                MONTHS_MIN,
                MONTHS_MAX,
                IntervalParseError::FieldValueOutOfRange,
            )?;
            add_bounded(
                months,
                whole_months,
                MONTHS_MIN,
                MONTHS_MAX,
                IntervalParseError::FieldValueOutOfRange,
            )?;
            if num.fract() != 0.0 {
                add_days_with_fraction(
                    num.fract() * 30.0,
                    days,
                    micros,
                    IntervalParseError::FieldValueOutOfRange,
                )?;
            }
            Ok(true)
        }
        "week" | "weeks" | "w" => {
            let error = IntervalParseError::FieldValueOutOfRange;
            add_days_with_fraction(num * 7.0, days, micros, error)?;
            Ok(true)
        }
        "day" | "days" | "d" => {
            add_days_with_fraction(num, days, micros, IntervalParseError::FieldValueOutOfRange)?;
            Ok(true)
        }
        "hour" | "hours" | "hr" | "hrs" | "h" => {
            let delta = scaled_to_bounded_i128(
                num,
                i128_to_f64(HOUR_MICROS),
                MICROS_MIN,
                MICROS_MAX,
                IntervalParseError::FieldValueOutOfRange,
            )?;
            add_bounded(
                micros,
                delta,
                MICROS_MIN,
                MICROS_MAX,
                IntervalParseError::FieldValueOutOfRange,
            )?;
            Ok(true)
        }
        "minute" | "minutes" | "min" | "mins" => {
            let delta = scaled_to_bounded_i128(
                num,
                i128_to_f64(MINUTE_MICROS),
                MICROS_MIN,
                MICROS_MAX,
                IntervalParseError::FieldValueOutOfRange,
            )?;
            add_bounded(
                micros,
                delta,
                MICROS_MIN,
                MICROS_MAX,
                IntervalParseError::FieldValueOutOfRange,
            )?;
            Ok(true)
        }
        "second" | "seconds" | "sec" | "secs" | "s" => {
            let delta = scaled_to_bounded_i128(
                num,
                i128_to_f64(SECOND_MICROS),
                MICROS_MIN,
                MICROS_MAX,
                IntervalParseError::FieldValueOutOfRange,
            )?;
            add_bounded(
                micros,
                delta,
                MICROS_MIN,
                MICROS_MAX,
                IntervalParseError::FieldValueOutOfRange,
            )?;
            Ok(true)
        }
        "millisecond" | "milliseconds" | "msec" | "msecs" | "ms" => {
            let delta = scaled_to_bounded_i128(
                num,
                i128_to_f64(MILLISECOND_MICROS),
                MICROS_MIN,
                MICROS_MAX,
                IntervalParseError::FieldValueOutOfRange,
            )?;
            add_bounded(
                micros,
                delta,
                MICROS_MIN,
                MICROS_MAX,
                IntervalParseError::FieldValueOutOfRange,
            )?;
            Ok(true)
        }
        "microsecond" | "microseconds" | "usec" | "usecs" | "us" => {
            let delta = trunc_to_bounded_i128(
                num,
                MICROS_MIN,
                MICROS_MAX,
                IntervalParseError::FieldValueOutOfRange,
            )?;
            add_bounded(
                micros,
                delta,
                MICROS_MIN,
                MICROS_MAX,
                IntervalParseError::FieldValueOutOfRange,
            )?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

pub(super) fn apply_interval_unit_str(
    num_str: &str,
    unit: &str,
    months: &mut i128,
    days: &mut i128,
    micros: &mut i128,
) -> Result<bool, IntervalParseError> {
    if let Ok(value) = parse_interval_exact_number(num_str) {
        let unit = unit.to_ascii_lowercase();
        let exact_delta = match unit.as_str() {
            "hour" | "hours" | "hr" | "hrs" | "h" => Some((
                exact_scaled_to_bounded_i128_rounded(
                    &value,
                    HOUR_MICROS,
                    MICROS_MIN,
                    MICROS_MAX,
                    IntervalParseError::FieldValueOutOfRange,
                )?,
                "micros",
                IntervalParseError::FieldValueOutOfRange,
            )),
            "minute" | "minutes" | "min" | "mins" => Some((
                exact_scaled_to_bounded_i128_rounded(
                    &value,
                    MINUTE_MICROS,
                    MICROS_MIN,
                    MICROS_MAX,
                    IntervalParseError::FieldValueOutOfRange,
                )?,
                "micros",
                IntervalParseError::FieldValueOutOfRange,
            )),
            "second" | "seconds" | "sec" | "secs" | "s" => Some((
                exact_scaled_to_bounded_i128_rounded(
                    &value,
                    SECOND_MICROS,
                    MICROS_MIN,
                    MICROS_MAX,
                    IntervalParseError::FieldValueOutOfRange,
                )?,
                "micros",
                IntervalParseError::FieldValueOutOfRange,
            )),
            "millisecond" | "milliseconds" | "msec" | "msecs" | "ms" => Some((
                exact_scaled_to_bounded_i128_rounded(
                    &value,
                    MILLISECOND_MICROS,
                    MICROS_MIN,
                    MICROS_MAX,
                    IntervalParseError::FieldValueOutOfRange,
                )?,
                "micros",
                IntervalParseError::FieldValueOutOfRange,
            )),
            "microsecond" | "microseconds" | "usec" | "usecs" | "us" => Some((
                exact_scaled_to_bounded_i128_rounded(
                    &value,
                    1,
                    MICROS_MIN,
                    MICROS_MAX,
                    IntervalParseError::FieldValueOutOfRange,
                )?,
                "micros",
                IntervalParseError::FieldValueOutOfRange,
            )),
            _ => None,
        };

        if let Some((delta, target, error)) = exact_delta {
            match target {
                "months" => add_bounded(months, delta, MONTHS_MIN, MONTHS_MAX, error)?,
                "days" => add_bounded(days, delta, DAYS_MIN, DAYS_MAX, error)?,
                "micros" => add_bounded(micros, delta, MICROS_MIN, MICROS_MAX, error)?,
                _ => unreachable!(),
            }
            return Ok(true);
        }
    }

    let num = parse_interval_number(num_str)?;
    apply_interval_unit(num, unit, months, days, micros)
}
