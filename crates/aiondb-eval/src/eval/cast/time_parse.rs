use time::Time;

use super::date_time_helpers::strip_tz_suffix;

const MICROS_PER_DAY: u64 = 86_400_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TimeParseError {
    Invalid,
    OutOfRange,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ParsedTime {
    pub(super) time: Time,
    pub(super) display_end_of_day: bool,
}

impl ParsedTime {
    const fn midnight() -> Self {
        Self {
            time: Time::MIDNIGHT,
            display_end_of_day: false,
        }
    }

    pub(super) const fn end_of_day() -> Self {
        Self {
            time: Time::MIDNIGHT,
            display_end_of_day: true,
        }
    }
}

pub(super) fn parse_pg_time_components(input: &str) -> Result<ParsedTime, TimeParseError> {
    let had_t_prefix = input.starts_with('T');
    let input = input.strip_prefix('T').unwrap_or(input).trim();
    if input.eq_ignore_ascii_case("allballs") {
        return Ok(ParsedTime::midnight());
    }

    let input = strip_tz_suffix(input).trim();
    let (input, meridiem) = split_meridiem_suffix(input);
    let parsed = parse_time_core(input, had_t_prefix)?;
    let hour = match meridiem {
        Some(is_pm) => apply_meridiem(parsed.hour, is_pm)?,
        None => parsed.hour,
    };

    normalize_time(
        hour,
        parsed.minute,
        parsed.second,
        parsed.fraction.as_deref(),
    )
}

#[derive(Clone, Debug)]
struct ParsedTimeCore {
    hour: u32,
    minute: u32,
    second: u32,
    fraction: Option<String>,
}

fn parse_time_core(input: &str, had_t_prefix: bool) -> Result<ParsedTimeCore, TimeParseError> {
    let (main, fraction) = match input.split_once('.') {
        Some((time, fractional)) => {
            let fractional = strip_tz_suffix(fractional).trim();
            (time, Some(fractional.to_owned()))
        }
        None => (input, None),
    };

    let parts: Vec<&str> = main.split(':').collect();
    let (hour, minute, second) = match parts.len() {
        3 => (
            parse_component(parts[0])?,
            parse_component(parts[1])?,
            parse_component(parts[2])?,
        ),
        2 => (parse_component(parts[0])?, parse_component(parts[1])?, 0),
        1 => {
            let digits = parts[0];
            if had_t_prefix && digits.len() == 2 {
                return Err(TimeParseError::Invalid);
            }
            match digits.len() {
                2 => (parse_component(digits)?, 0, 0),
                4 => (
                    parse_component(&digits[..2])?,
                    parse_component(&digits[2..4])?,
                    0,
                ),
                6 => (
                    parse_component(&digits[..2])?,
                    parse_component(&digits[2..4])?,
                    parse_component(&digits[4..6])?,
                ),
                _ => return Err(TimeParseError::Invalid),
            }
        }
        _ => return Err(TimeParseError::Invalid),
    };

    Ok(ParsedTimeCore {
        hour,
        minute,
        second,
        fraction,
    })
}

fn parse_component(component: &str) -> Result<u32, TimeParseError> {
    component
        .parse::<u32>()
        .map_err(|_| TimeParseError::Invalid)
}

fn split_meridiem_suffix(input: &str) -> (&str, Option<bool>) {
    let trimmed = input.trim_end();
    let upper = trimmed.to_ascii_uppercase();
    for (suffix, is_pm) in [("PM", true), ("AM", false)] {
        if let Some(base) = upper.strip_suffix(suffix) {
            if base.is_empty() {
                continue;
            }
            let base_len = base.len();
            let original_base = &trimmed[..base_len];
            if original_base
                .chars()
                .last()
                .is_some_and(|ch| ch.is_ascii_digit() || ch.is_ascii_whitespace())
            {
                return (original_base.trim_end(), Some(is_pm));
            }
        }
    }
    (trimmed, None)
}

fn apply_meridiem(hour: u32, is_pm: bool) -> Result<u32, TimeParseError> {
    if hour > 12 {
        return Err(TimeParseError::Invalid);
    }

    Ok(match (hour, is_pm) {
        (12, false) => 0,
        (12, true) => 12,
        (value, true) => value + 12,
        (value, false) => value,
    })
}

fn normalize_time(
    hour: u32,
    minute: u32,
    mut second: u32,
    fraction: Option<&str>,
) -> Result<ParsedTime, TimeParseError> {
    let (micros, carry_second) = round_fraction_to_micros(fraction)?;
    second += carry_second;

    if minute > 59 || hour > 24 || second > 60 {
        return Err(TimeParseError::OutOfRange);
    }

    if hour == 24 {
        return if minute == 0 && second == 0 && micros == 0 {
            Ok(ParsedTime::end_of_day())
        } else {
            Err(TimeParseError::OutOfRange)
        };
    }

    if second == 60 {
        return if hour == 23 && minute == 59 && micros == 0 {
            Ok(ParsedTime::end_of_day())
        } else {
            Err(TimeParseError::OutOfRange)
        };
    }

    let total_micros = (((u64::from(hour) * 60 + u64::from(minute)) * 60 + u64::from(second))
        * 1_000_000)
        + u64::from(micros);
    if total_micros == MICROS_PER_DAY {
        return Ok(ParsedTime::end_of_day());
    }
    if total_micros > MICROS_PER_DAY {
        return Err(TimeParseError::OutOfRange);
    }

    let hour = u8::try_from(hour).map_err(|_| TimeParseError::OutOfRange)?;
    let minute = u8::try_from(minute).map_err(|_| TimeParseError::OutOfRange)?;
    let second = u8::try_from(second).map_err(|_| TimeParseError::OutOfRange)?;
    let time = Time::from_hms_micro(hour, minute, second, micros)
        .map_err(|_| TimeParseError::OutOfRange)?;
    Ok(ParsedTime {
        time,
        display_end_of_day: false,
    })
}

fn round_fraction_to_micros(fraction: Option<&str>) -> Result<(u32, u32), TimeParseError> {
    let Some(fraction) = fraction else {
        return Ok((0, 0));
    };
    if fraction.is_empty() || !fraction.chars().all(|ch| ch.is_ascii_digit()) {
        return Err(TimeParseError::Invalid);
    }

    let digits = fraction.as_bytes();
    let mut micros = 0u32;
    for idx in 0..6 {
        let digit = digits.get(idx).copied().unwrap_or(b'0') - b'0';
        micros = micros * 10 + u32::from(digit);
    }
    if digits.get(6).is_some_and(|digit| *digit >= b'5') {
        micros += 1;
        if micros == 1_000_000 {
            return Ok((0, 1));
        }
    }

    Ok((micros, 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_edge_times_up_to_end_of_day() {
        for input in ["23:59:59.9999999", "23:59:60", "24:00:00"] {
            let parsed = parse_pg_time_components(input).expect("time");
            assert!(parsed.display_end_of_day, "input={input}");
            assert_eq!(parsed.time, Time::MIDNIGHT, "input={input}");
        }
    }

    #[test]
    fn rejects_out_of_range_end_of_day_literals() {
        for input in ["24:00:00.01", "23:59:60.01", "24:01:00", "25:00:00"] {
            assert_eq!(
                parse_pg_time_components(input),
                Err(TimeParseError::OutOfRange)
            );
        }
    }
}
