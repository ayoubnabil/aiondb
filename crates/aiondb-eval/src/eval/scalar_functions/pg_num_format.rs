//\! PostgreSQL-compatible numeric formatting for to_char().
use crate::eval::scalar_functions::value_convert::{f64_to_i32, f64_to_i64_trunc_saturating};
use aiondb_core::convert::usize_to_i32_saturating;

fn trunc_clamped_f64_to_i64(value: f64) -> i64 {
    f64_to_i64_trunc_saturating(value)
}

fn floor_f64_to_i32(value: f64) -> i32 {
    let floored = value
        .floor()
        .clamp(f64::from(i32::MIN), f64::from(i32::MAX));
    f64_to_i32(floored).unwrap_or_else(|_| {
        if floored.is_sign_negative() {
            i32::MIN
        } else {
            i32::MAX
        }
    })
}

/// Replace the ASCII char at `pos` (a char/byte index) of `s` with
/// `replacement` (an ASCII char). Numeric `to_char` output is always
/// pure ASCII, so char positions and byte positions coincide.
fn replace_ascii_char_in_place(s: &mut String, pos: usize, replacement: char) {
    let mut buf = [0u8; 4];
    let replacement_str = replacement.encode_utf8(&mut buf);
    s.replace_range(pos..=pos, replacement_str);
}

pub(crate) fn pg_format_number(value: f64, fmt: &str) -> String {
    if let Some(fast) = fast_zero_padded_fill_mode(value, fmt) {
        return fast;
    }

    // ── Parse format string into tokens ──
    let tokens = parse_num_format(fmt);

    // ── Check for EEEE (scientific notation) mode ──
    if tokens.iter().any(|t| matches!(t, NumFmtToken::Eeee)) {
        return pg_format_number_scientific(value, &tokens);
    }

    // ── Guard against non-finite inputs ──
    if value.is_nan() {
        return " NaN".to_string();
    }
    if value.is_infinite() {
        return if value < 0.0 {
            " -Infinity".to_string()
        } else {
            "  Infinity".to_string()
        };
    }

    // ── Extract formatting flags ──
    let is_negative = value < 0.0 || (value == 0.0 && value.is_sign_negative());
    let abs_value = value.abs();

    let mut fill_mode = false; // FM
    let mut sign_mode = NumSignMode::Default;
    let mut int_digit_positions = 0usize; // count of 9/0 before decimal
    let mut frac_digit_positions = 0usize; // count of 9/0 after decimal
    let mut has_decimal = false;
    let mut _has_leading_zeros = false; // any '0' in integer part
    let mut frac_has_leading_zeros = false; // any '0' in fraction part
    let mut frac_min_digits = 0usize; // minimum fractional digits guaranteed by trailing '0' positions
    let mut group_positions: Vec<usize> = Vec::new(); // positions (from right) of G/,

    // First pass: extract metadata from tokens
    let mut _digit_index = 0usize;
    let mut past_decimal = false;
    for tok in &tokens {
        match tok {
            NumFmtToken::FM => fill_mode = true,
            NumFmtToken::S => {
                if sign_mode == NumSignMode::Default {
                    sign_mode = NumSignMode::SAnchor;
                }
            }
            NumFmtToken::SG => sign_mode = NumSignMode::SG,
            NumFmtToken::PR => sign_mode = NumSignMode::PR,
            NumFmtToken::MI => {
                if sign_mode == NumSignMode::Default {
                    sign_mode = NumSignMode::MI;
                }
            }
            NumFmtToken::Decimal => {
                has_decimal = true;
                past_decimal = true;
            }
            NumFmtToken::Digit9 => {
                if past_decimal {
                    frac_digit_positions += 1;
                } else {
                    int_digit_positions += 1;
                }
                _digit_index += 1;
            }
            NumFmtToken::Digit0 => {
                if past_decimal {
                    frac_digit_positions += 1;
                    frac_has_leading_zeros = true;
                } else {
                    int_digit_positions += 1;
                    _has_leading_zeros = true;
                }
                _digit_index += 1;
            }
            NumFmtToken::Group => {
                if !past_decimal {
                    // Record how many integer digit positions follow this comma
                    // (computed in the second pass).
                    group_positions.push(0); // placeholder
                }
            }
            NumFmtToken::TH | NumFmtToken::Th => {}
            _ => {}
        }
    }

    // Compute frac_min_digits: the position (1-based) of the LAST Digit0
    // in the fractional part.  In FM mode, all fractional digits up to and
    // including the last '0' position are always shown; trailing '9'
    // positions with zero values can be trimmed.
    {
        let mut in_frac = false;
        let mut frac_pos = 0usize;
        for tok in &tokens {
            match tok {
                NumFmtToken::Decimal => in_frac = true,
                NumFmtToken::Digit0 if in_frac => {
                    frac_pos += 1;
                    frac_min_digits = frac_pos;
                }
                NumFmtToken::Digit9 if in_frac => {
                    frac_pos += 1;
                }
                _ => {}
            }
        }
    }

    // Recompute group_positions: position from the right of integer digits
    {
        let mut gidx = 0usize;
        let mut _int_pos = 0usize;
        let mut past_dec = false;
        for tok in &tokens {
            match tok {
                NumFmtToken::Decimal => past_dec = true,
                NumFmtToken::Digit9 | NumFmtToken::Digit0 => {
                    if !past_dec {
                        _int_pos += 1;
                    }
                }
                NumFmtToken::Group => {
                    if !past_dec && gidx < group_positions.len() {
                        // Count how many int digit positions are AFTER this group
                        let mut after = 0;
                        let mut seen_dec = false;
                        let remaining = tokens.iter().skip(
                            tokens
                                .iter()
                                .position(|t| std::ptr::eq(t, tok))
                                .unwrap_or(0)
                                + 1,
                        );
                        for rt in remaining {
                            match rt {
                                NumFmtToken::Decimal => {
                                    seen_dec = true;
                                    break;
                                }
                                NumFmtToken::Digit9 | NumFmtToken::Digit0 => after += 1,
                                _ => {}
                            }
                        }
                        if !seen_dec {
                            // All remaining are integer digits
                        }
                        group_positions[gidx] = after;
                        gidx += 1;
                    }
                }
                _ => {}
            }
        }
    }

    // ── Format the number ──
    let rounded = if has_decimal {
        let factor = 10f64.powi(usize_to_i32_saturating(frac_digit_positions));
        (abs_value * factor).round() / factor
    } else {
        abs_value.round()
    };

    let int_part = trunc_clamped_f64_to_i64(rounded);
    let int_str = int_part.abs().to_string();
    let frac_str = if has_decimal && frac_digit_positions > 0 {
        let frac = rounded.fract().abs();
        let frac_raw = format!("{frac:.frac_digit_positions$}");
        // frac_raw is like "0.123" - extract digits after "0."
        if let Some(dot_pos) = frac_raw.find('.') {
            let digits = &frac_raw[dot_pos + 1..];
            // Pad or truncate to frac_digit_positions
            let mut s = digits.to_string();
            while s.len() < frac_digit_positions {
                s.push('0');
            }
            s.truncate(frac_digit_positions);
            s
        } else {
            "0".repeat(frac_digit_positions)
        }
    } else {
        String::new()
    };

    // ── Build integer digit string with proper padding ──
    //
    // Each position in the integer format is either '9' (blank if leading)
    // or '0' (always shown).  We need to produce a string where leading
    // positions that are '9' show as spaces, leading '0' positions show as
    // '0', and significant digits show as themselves.
    let total_int_digits = int_digit_positions;
    let mut padded_int = String::new();
    if int_str.len() > total_int_digits {
        // Overflow: PG fills with '#'
        return pg_num_overflow(fmt);
    }
    {
        // Collect which integer positions are '0' vs '9' format (left to right).
        // In PostgreSQL, once a '0' format is seen, all subsequent positions
        // (even '9') also show '0' for leading zeros - the '0' floods right.
        let mut int_format_is_zero: Vec<bool> = Vec::with_capacity(total_int_digits);
        let mut past_dec_scan = false;
        let mut seen_zero_fmt = false;
        for tok in &tokens {
            match tok {
                NumFmtToken::Decimal => past_dec_scan = true,
                NumFmtToken::Digit0 if !past_dec_scan => {
                    seen_zero_fmt = true;
                    int_format_is_zero.push(true);
                }
                NumFmtToken::Digit9 if !past_dec_scan => {
                    int_format_is_zero.push(seen_zero_fmt);
                }
                _ => {}
            }
        }
        let pad_count = total_int_digits - int_str.len();
        // Positions 0..pad_count are padding; pad_count.. are from int_str
        let mut significant_seen = false;
        for pos in 0..total_int_digits {
            let digit_char = if pos < pad_count {
                '0' // padding position
            } else {
                int_str.as_bytes()[pos - pad_count] as char
            };
            if digit_char != '0' {
                significant_seen = true;
            }
            if significant_seen {
                padded_int.push(digit_char);
            } else {
                // Leading zero: show '0' if format is '0', space if '9'
                let is_zero_fmt = int_format_is_zero.get(pos).copied().unwrap_or(false);
                padded_int.push(if is_zero_fmt { '0' } else { ' ' });
            }
        }
    }

    // ── Assemble output from tokens ──
    let mut result = String::new();
    let mut int_digit_idx = 0usize;
    let mut frac_digit_idx = 0usize;
    let mut past_dec_out = false;
    // Track the position of the first significant number digit in `result`
    // so that the default sign placement finds the correct location (not a
    // digit inside a literal text segment).
    let mut first_num_digit_pos: Option<usize> = None;

    // Determine sign character
    let sign_char = if is_negative { '-' } else { '+' };

    // Check if S is at the start (ignoring FM)
    let s_at_start = tokens
        .iter()
        .find(|t| !matches!(t, NumFmtToken::FM))
        .is_some_and(|t| matches!(t, NumFmtToken::S));

    for tok in &tokens {
        match tok {
            NumFmtToken::FM => {} // Already handled
            NumFmtToken::S => {
                if sign_mode == NumSignMode::SAnchor {
                    if s_at_start {
                        // When S is at the start, we place a space placeholder.
                        // The sign will be repositioned adjacent to the first
                        // significant digit after the full number is assembled.
                        result.push(' ');
                    } else {
                        // S at end: emit sign directly
                        result.push(sign_char);
                    }
                }
            }
            NumFmtToken::SG => {
                result.push(sign_char);
            }
            NumFmtToken::MI => {
                if is_negative {
                    result.push('-');
                } else {
                    result.push(' ');
                }
            }
            NumFmtToken::PR => {} // Handled at the end
            NumFmtToken::L => {
                // Locale currency symbol - empty in C/POSIX locale.
                // PostgreSQL uses lconv->currency_symbol which is "" in C locale.
            }
            NumFmtToken::Decimal => {
                past_dec_out = true;
                result.push('.');
            }
            NumFmtToken::Digit9 | NumFmtToken::Digit0 => {
                if past_dec_out {
                    if frac_digit_idx < frac_str.len() {
                        result.push(frac_str.as_bytes()[frac_digit_idx] as char);
                    } else if matches!(tok, NumFmtToken::Digit0) || frac_has_leading_zeros {
                        result.push('0');
                    } else {
                        result.push(' ');
                    }
                    frac_digit_idx += 1;
                } else {
                    if int_digit_idx < padded_int.len() {
                        let ch = padded_int.as_bytes()[int_digit_idx] as char;
                        // Track first non-space character from a digit position
                        // for correct default sign placement.
                        if first_num_digit_pos.is_none() && ch != ' ' {
                            first_num_digit_pos = Some(result.len());
                        }
                        result.push(ch);
                    }
                    int_digit_idx += 1;
                }
            }
            NumFmtToken::Group => {
                if past_dec_out {
                    result.push(',');
                } else {
                    // Insert comma only if there's a non-space digit to the left
                    let left_has_digit = result
                        .chars()
                        .rev()
                        .take_while(|c| *c != ',')
                        .any(|c| c.is_ascii_digit());
                    if left_has_digit {
                        result.push(',');
                    } else {
                        result.push(' ');
                    }
                }
            }
            NumFmtToken::Space => result.push(' '),
            NumFmtToken::Literal(s) => result.push_str(s),
            NumFmtToken::TH | NumFmtToken::Th => {
                // PostgreSQL omits ordinal suffixes for negative values.
                if !is_negative {
                    let suffix = ordinal_suffix(int_part.unsigned_abs());
                    if matches!(tok, NumFmtToken::TH) {
                        result.push_str(&suffix.to_uppercase());
                    } else {
                        result.push_str(suffix);
                    }
                }
            }
            NumFmtToken::Eeee => {} // Handled by early return to pg_format_number_scientific
        }
    }

    // ── Apply sign mode wrapping ──
    if sign_mode == NumSignMode::PR {
        if is_negative {
            let trimmed = result.trim();
            let inner = trimmed.to_string();
            let pad = result.len() - trimmed.len();
            result = format!(
                "{:>width$}",
                format!("<{inner}>"),
                width = inner.len() + 2 + pad
            );
        } else if !fill_mode {
            result.push(' ');
        }
    }

    // ── Handle S-at-start sign placement ──
    // PostgreSQL places the sign adjacent to the first significant digit,
    // not at the absolute start of the string. The numeric format
    // result is always pure ASCII (digits, separators, sign), so char
    // positions and ASCII sign chars (`+`, `-`, space) line up with
    // byte positions and we can splice via `replace_range` instead of
    // walking `result.chars().collect::<Vec<char>>()` twice
    // (collect + into_iter().collect()).
    if sign_mode == NumSignMode::SAnchor && s_at_start {
        if let Some(digit_pos) = first_num_digit_pos {
            if digit_pos > 0 {
                let replace_pos = digit_pos - 1;
                replace_ascii_char_in_place(&mut result, replace_pos, sign_char);
            } else {
                result.insert(0, sign_char);
            }
        } else if let Some(pos) = result.find(' ') {
            replace_ascii_char_in_place(&mut result, pos, sign_char);
        }
    }

    // ── Handle default sign (no S/SG/PR/MI) ──
    // PostgreSQL reserves a sign position adjacent to the first significant
    // number digit.  We use `first_num_digit_pos` (tracked during assembly)
    // so that digit characters inside literal text segments are not confused
    // with actual number digits.  When there is a padding space just before
    // the first number digit, the sign replaces it (keeping total width
    // unchanged).  Otherwise the sign character is prepended.
    if sign_mode == NumSignMode::Default {
        let sign_ch = if is_negative { '-' } else { ' ' };
        if let Some(pos) = first_num_digit_pos {
            if pos > 0 && result.as_bytes()[pos - 1] == b' ' {
                // Replace the existing padding space just before the first digit
                let mut chars: Vec<char> = result.chars().collect();
                chars[pos - 1] = sign_ch;
                result = chars.into_iter().collect();
            } else {
                // Insert sign immediately before the first number digit.
                // This keeps literal text at the start, e.g. "foo 100" not " foo100".
                result.insert(pos, sign_ch);
            }
        } else if is_negative {
            result.insert(0, '-');
        } else {
            result.insert(0, ' ');
        }
    }

    // ── FM: strip trailing fractional zeros and leading/trailing spaces ──
    if fill_mode {
        if has_decimal {
            if let Some(dot_pos) = result.rfind('.') {
                // Strip any trailing ordinal suffix before trimming zeros
                let mut suffix_to_restore = String::new();
                let after_dot = &result[dot_pos + 1..];
                let digits_end = after_dot
                    .rfind(|c: char| c.is_ascii_digit())
                    .map_or(0, |p| p + 1);
                if digits_end < after_dot.len() {
                    suffix_to_restore = after_dot[digits_end..].to_string();
                }
                let numeric_after_dot = &after_dot[..digits_end];

                // Strip trailing zeros, but keep at least `frac_min_digits`
                // fractional digits (those from '0' format positions).
                let trimmed_frac = numeric_after_dot.trim_end_matches('0');
                let keep = trimmed_frac.len().max(frac_min_digits);
                let new_end = dot_pos + 1 + keep;
                result.truncate(new_end);
                // If the format had no fractional digit positions at all
                // (e.g. 'FM999.'), remove the trailing dot.
                if frac_digit_positions == 0 && result.ends_with('.') {
                    result.pop();
                }
                // In FM mode with ordinal suffix: only re-add if there are
                // remaining fractional digits (PG drops ordinal when trailing
                // digits are fully trimmed).
                if !suffix_to_restore.is_empty() && keep > 0 {
                    result.push_str(&suffix_to_restore);
                }
            }
        }
        let trimmed = result.trim().to_string();
        result = trimmed;
        // PostgreSQL FM mode always shows at least one "0" digit for the
        // integer part when the value is zero and the format has integer
        // digit positions.  After trimming spaces, if the result starts
        // with '.' (or sign + '.'), insert a '0' before the decimal point.
        if int_part == 0 && int_digit_positions > 0 {
            // Find where to insert the zero: right before the '.'
            if let Some(dot_idx) = result.find('.') {
                // Check if the character before dot is a sign or is the start
                if dot_idx == 0 {
                    result.insert(0, '0');
                } else {
                    let before_dot = result.as_bytes()[dot_idx - 1];
                    if !before_dot.is_ascii_digit() {
                        result.insert(dot_idx, '0');
                    }
                }
            } else if !result.chars().any(|c| c.is_ascii_digit()) {
                // No decimal point and no digits at all - insert '0'
                result.push('0');
            }
        }
    }

    result
}

/// Fast-path for the extremely common `to_char(int_like, '000...FM')` shape.
///
/// This avoids tokenization and the generic formatter pipeline for fixed-width,
/// zero-padded integer output with fill mode, e.g. `000000000000FM`.
fn fast_zero_padded_fill_mode(value: f64, fmt: &str) -> Option<String> {
    let bytes = fmt.as_bytes();
    if bytes.len() < 3 {
        return None;
    }
    let suffix = &bytes[bytes.len() - 2..];
    if !(suffix.eq_ignore_ascii_case(b"FM")) {
        return None;
    }
    let width = bytes.len() - 2;
    if width == 0 || !bytes[..width].iter().all(|b| *b == b'0') {
        return None;
    }
    if value.is_nan() || value.is_infinite() {
        return None;
    }

    // Keep sign behavior aligned with the generic code.
    let is_negative = value < 0.0 || (value == 0.0 && value.is_sign_negative());
    let rounded_abs = value.abs().round();
    let int_part = trunc_clamped_f64_to_i64(rounded_abs);
    let digits = int_part.to_string();

    if digits.len() > width {
        return Some(pg_num_overflow(fmt));
    }

    let mut out = String::with_capacity(width + usize::from(is_negative));
    if is_negative {
        out.push('-');
    }
    for _ in 0..(width - digits.len()) {
        out.push('0');
    }
    out.push_str(&digits);
    Some(out)
}

/// Format a number in scientific notation for the EEEE format pattern.
///
/// PostgreSQL's `to_char(value, '9.999EEEE')` produces output like ` 3.434e+07`.
/// The number of fractional digits is determined by the 9/0 positions after the
/// decimal point.  The integer part always has exactly one digit.  When the
/// value doesn't fit (integer part requires more than the available positions),
/// the output is filled with `#`.
fn pg_format_number_scientific(value: f64, tokens: &[NumFmtToken]) -> String {
    let is_negative = value < 0.0;
    let abs_value = value.abs();

    // Count fractional digit positions (9 or 0 after the decimal)
    let mut frac_digits = 0usize;
    let mut int_digits = 0usize;
    let mut has_decimal = false;
    let mut _total_width = 0usize;
    for tok in tokens {
        match tok {
            NumFmtToken::Digit9 | NumFmtToken::Digit0 => {
                if has_decimal {
                    frac_digits += 1;
                } else {
                    int_digits += 1;
                }
                _total_width += 1;
            }
            NumFmtToken::Decimal => {
                has_decimal = true;
                _total_width += 1;
            }
            NumFmtToken::Eeee => {
                // The EEEE token itself takes up space for e.g. "e+07" (4 chars)
            }
            NumFmtToken::FM
            | NumFmtToken::S
            | NumFmtToken::SG
            | NumFmtToken::PR
            | NumFmtToken::MI
            | NumFmtToken::TH
            | NumFmtToken::Th => {}
            NumFmtToken::Space => _total_width += 1,
            _ => {}
        }
    }

    // Check if the integer part has more positions than 1 (with EEEE, PG
    // still shows only 1 integer digit, but if format has more, # overflow)
    if int_digits > 1 {
        // When the integer portion of the format has more than 1 digit
        // position, PostgreSQL still formats normally but with scientific
        // notation.  However, the actual integer digits shown are determined
        // by int_digits (not always 1).
    }

    if abs_value == 0.0 {
        // Special case: 0 formats as " 0.000e+00" etc.
        let sign_str = if is_negative { "-" } else { " " };
        let frac_str = "0".repeat(frac_digits);
        let result = if has_decimal {
            format!("{sign_str}0.{frac_str}e+00")
        } else {
            format!("{sign_str}0e+00")
        };
        return result;
    }

    if abs_value.is_nan() {
        return " NaN".to_string();
    }
    if abs_value.is_infinite() {
        return if is_negative {
            " -Infinity".to_string()
        } else {
            "  Infinity".to_string()
        };
    }

    // Compute mantissa and exponent.
    // `abs_value` is finite and non-zero here; log10 is normally in [-308, 308].
    let exponent = floor_f64_to_i32(abs_value.log10());
    let mantissa = abs_value / 10f64.powi(exponent);

    // Round mantissa to the required number of fractional digits.
    let factor = 10f64.powi(usize_to_i32_saturating(frac_digits));
    let rounded_mantissa = (mantissa * factor).round() / factor;

    // If rounding pushed mantissa to >= 10, adjust
    let (final_mantissa, final_exponent) = if rounded_mantissa >= 10.0 {
        (rounded_mantissa / 10.0, exponent + 1)
    } else {
        (rounded_mantissa, exponent)
    };

    let sign_str = if is_negative { "-" } else { " " };
    let mant_str = if has_decimal {
        format!("{final_mantissa:.frac_digits$}")
    } else {
        format!("{final_mantissa:.0}")
    };

    format!("{sign_str}{mant_str}e{final_exponent:+03}")
}

#[derive(Debug, PartialEq, Clone)]
enum NumFmtToken {
    Digit9,
    Digit0,
    Decimal,
    Group,
    S,
    SG,
    PR,
    MI,
    FM,
    TH,
    Th,
    L,
    Eeee,
    Space,
    Literal(String),
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum NumSignMode {
    Default,
    SAnchor,
    SG,
    PR,
    MI,
}

fn parse_num_format(fmt: &str) -> Vec<NumFmtToken> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = fmt.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        let next = chars.get(i + 1).copied();
        match ch {
            '9' => {
                tokens.push(NumFmtToken::Digit9);
                i += 1;
            }
            '0' => {
                tokens.push(NumFmtToken::Digit0);
                i += 1;
            }
            '.' | 'D' | 'd' => {
                tokens.push(NumFmtToken::Decimal);
                i += 1;
            }
            ',' | 'G' | 'g' => {
                tokens.push(NumFmtToken::Group);
                i += 1;
            }
            'S' | 's' => {
                if next == Some('G') || next == Some('g') {
                    tokens.push(NumFmtToken::SG);
                    i += 2;
                } else {
                    tokens.push(NumFmtToken::S);
                    i += 1;
                }
            }
            'P' | 'p' => {
                if next == Some('R') || next == Some('r') {
                    tokens.push(NumFmtToken::PR);
                    i += 2;
                } else {
                    tokens.push(NumFmtToken::Literal(ch.to_string()));
                    i += 1;
                }
            }
            'M' | 'm' => {
                if next == Some('I') || next == Some('i') {
                    tokens.push(NumFmtToken::MI);
                    i += 2;
                } else {
                    tokens.push(NumFmtToken::Literal(ch.to_string()));
                    i += 1;
                }
            }
            'F' | 'f' => {
                if next == Some('M') || next == Some('m') {
                    tokens.push(NumFmtToken::FM);
                    i += 2;
                } else {
                    tokens.push(NumFmtToken::Literal(ch.to_string()));
                    i += 1;
                }
            }
            'T' => {
                if next == Some('H') {
                    tokens.push(NumFmtToken::TH);
                    i += 2;
                } else if next == Some('h') {
                    tokens.push(NumFmtToken::Th);
                    i += 2;
                } else {
                    tokens.push(NumFmtToken::Literal(ch.to_string()));
                    i += 1;
                }
            }
            't' => {
                if next == Some('H') || next == Some('h') {
                    tokens.push(NumFmtToken::Th);
                    i += 2;
                } else {
                    tokens.push(NumFmtToken::Literal(ch.to_string()));
                    i += 1;
                }
            }
            '"' => {
                // Quoted literal text
                i += 1;
                let mut lit = String::new();
                while i < chars.len() && chars[i] != '"' {
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        i += 1;
                        lit.push(chars[i]);
                    } else {
                        lit.push(chars[i]);
                    }
                    i += 1;
                }
                if i < chars.len() {
                    i += 1; // skip closing quote
                }
                tokens.push(NumFmtToken::Literal(lit));
            }
            'E' | 'e' => {
                // Check for EEEE (scientific notation)
                if i + 3 < chars.len()
                    && (chars[i + 1] == 'E' || chars[i + 1] == 'e')
                    && (chars[i + 2] == 'E' || chars[i + 2] == 'e')
                    && (chars[i + 3] == 'E' || chars[i + 3] == 'e')
                {
                    tokens.push(NumFmtToken::Eeee);
                    i += 4;
                } else {
                    tokens.push(NumFmtToken::Literal(ch.to_string()));
                    i += 1;
                }
            }
            'L' | 'l' => {
                tokens.push(NumFmtToken::L);
                i += 1;
            }
            ' ' => {
                tokens.push(NumFmtToken::Space);
                i += 1;
            }
            _ => {
                tokens.push(NumFmtToken::Literal(ch.to_string()));
                i += 1;
            }
        }
    }
    tokens
}

fn ordinal_suffix(n: u64) -> &'static str {
    let last_two = n % 100;
    let last_one = n % 10;
    if (11..=13).contains(&last_two) {
        "th"
    } else {
        match last_one {
            1 => "st",
            2 => "nd",
            3 => "rd",
            _ => "th",
        }
    }
}

fn pg_num_overflow(fmt: &str) -> String {
    // When the number doesn't fit, PG fills digit positions with '#' but
    // preserves the decimal point.  For example, format 'MI99.99' overflows
    // as ' ##.##' (the MI contributes a space, digits become #, decimal kept).
    let tokens = parse_num_format(fmt);
    let mut out = String::new();
    for tok in &tokens {
        match tok {
            NumFmtToken::Digit9 | NumFmtToken::Digit0 => out.push('#'),
            NumFmtToken::Decimal => out.push('.'),
            NumFmtToken::Group => {}         // commas suppressed on overflow
            NumFmtToken::S => out.push('#'), // sign position becomes #
            NumFmtToken::SG => out.push('#'),
            NumFmtToken::MI => out.push(' '),
            NumFmtToken::PR => out.push(' '),
            NumFmtToken::FM => {}
            NumFmtToken::TH | NumFmtToken::Th => {}
            NumFmtToken::L => {}
            NumFmtToken::Eeee => out.push_str("####"),
            NumFmtToken::Space => out.push(' '),
            NumFmtToken::Literal(s) => out.push_str(s),
        }
    }
    out
}
