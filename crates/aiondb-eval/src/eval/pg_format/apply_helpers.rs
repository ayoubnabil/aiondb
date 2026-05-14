//! Small utility helpers factored out of `apply.rs` to keep it under the
//! line-count limit.

use std::convert::TryFrom;

use time::{Date, Month};

pub(super) fn preview_value(input: &str, pos: usize, width: usize) -> String {
    input[pos..].chars().take(width.max(1)).collect::<String>()
}

pub(super) fn preview_chunk(input: &str, pos: usize) -> String {
    let mut value = String::new();
    for ch in input[pos..].chars() {
        if ch.is_ascii_whitespace() && !value.is_empty() {
            break;
        }
        value.push(ch);
    }
    if value.is_empty() {
        input[pos..].to_string()
    } else {
        value
    }
}

pub(super) fn remaining_digits(input: &str, pos: usize) -> usize {
    input[pos..].bytes().take_while(u8::is_ascii_digit).count()
}

#[allow(clippy::many_single_char_names)]
pub(super) fn julian_to_date(jd: i64) -> Result<Date, ()> {
    let jd = jd + 32044;
    let g = jd / 146097;
    let dg = jd % 146097;
    let c = (dg / 36524 + 1) * 3 / 4;
    let dc = dg - c * 36524;
    let b = dc / 1461;
    let db = dc % 1461;
    let a = (db / 365 + 1) * 3 / 4;
    let da = db - a * 365;
    let y = g * 400 + c * 100 + b * 4 + a;
    let m = (da * 5 + 308) / 153 - 2;
    let d = da - (m + 4) * 153 / 5 + 122;
    // `y` is an i64 derived from the Julian day number; very large Julian
    // day inputs (parsed as u32, max ~4.3 billion) can yield years well
    // outside the i32 range, so we reject them instead of wrapping.
    let year_i64 = y - 4800 + (m + 2) / 12;
    let year = i32::try_from(year_i64).map_err(|_| ())?;
    // (m+2) % 12 is 0..=11, +1 gives 1..=12 - fits in u8 safely.
    let month = u8::try_from((m + 2) % 12 + 1).map_err(|_| ())?;
    // `d+1` represents the day of month (1..=31) - fits in u8 safely.
    let day = u8::try_from(d + 1).map_err(|_| ())?;
    let month = Month::try_from(month).map_err(|_| ())?;
    Date::from_calendar_date(year, month, day).map_err(|_| ())
}
