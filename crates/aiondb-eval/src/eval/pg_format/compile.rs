use super::model::{CompiledFormat, FieldKind, FieldSpec, FormatItem};

pub(super) fn compile_pg_format(fmt: &str) -> CompiledFormat {
    let chars: Vec<char> = fmt.chars().collect();
    let mut items = Vec::new();
    let mut i = 0usize;
    let mut exact_mode = false;
    let mut uses_gregorian = false;
    let mut uses_iso = false;

    while i < chars.len() {
        let mut fill_mode = false;
        loop {
            if starts_with_ci(&chars, i, "FM") {
                fill_mode = true;
                i += 2;
                continue;
            }
            if starts_with_ci(&chars, i, "FX") {
                exact_mode = true;
                i += 2;
                continue;
            }
            break;
        }
        if i >= chars.len() {
            break;
        }

        if starts_with_escaped_quote(&chars, i) {
            let (literal, next) = parse_escaped_quote_literal(&chars, i);
            items.push(FormatItem::Literal(literal));
            i = next;
            continue;
        }

        if chars[i] == '"' {
            let (literal, next) = parse_quoted_literal(&chars, i);
            items.push(FormatItem::Literal(literal));
            i = next;
            continue;
        }

        if chars[i].is_ascii_whitespace() {
            items.push(FormatItem::Separator(chars[i]));
            i += 1;
            continue;
        }

        let field = if starts_with_ci(&chars, i, "Y,YYY") {
            Some(FieldSpec {
                kind: FieldKind::Year {
                    width: 4,
                    iso: false,
                    comma: true,
                },
                fill_mode,
                label: "YYYY",
                token_len: 5,
            })
        } else if starts_with_ci(&chars, i, "IYYY") {
            uses_iso = true;
            Some(FieldSpec {
                kind: FieldKind::Year {
                    width: 4,
                    iso: true,
                    comma: false,
                },
                fill_mode,
                label: "IYYY",
                token_len: 4,
            })
        } else if starts_with_ci(&chars, i, "YYYY") {
            uses_gregorian = true;
            Some(FieldSpec {
                kind: FieldKind::Year {
                    width: 4,
                    iso: false,
                    comma: false,
                },
                fill_mode,
                label: "YYYY",
                token_len: 4,
            })
        } else if starts_with_ci(&chars, i, "IYY") {
            uses_iso = true;
            Some(FieldSpec {
                kind: FieldKind::Year {
                    width: 3,
                    iso: true,
                    comma: false,
                },
                fill_mode,
                label: "IYY",
                token_len: 3,
            })
        } else if starts_with_ci(&chars, i, "YYY") {
            uses_gregorian = true;
            Some(FieldSpec {
                kind: FieldKind::Year {
                    width: 3,
                    iso: false,
                    comma: false,
                },
                fill_mode,
                label: "YYY",
                token_len: 3,
            })
        } else if starts_with_ci(&chars, i, "IY") {
            uses_iso = true;
            Some(FieldSpec {
                kind: FieldKind::Year {
                    width: 2,
                    iso: true,
                    comma: false,
                },
                fill_mode,
                label: "IY",
                token_len: 2,
            })
        } else if starts_with_ci(&chars, i, "YY") {
            uses_gregorian = true;
            Some(FieldSpec {
                kind: FieldKind::Year {
                    width: 2,
                    iso: false,
                    comma: false,
                },
                fill_mode,
                label: "YY",
                token_len: 2,
            })
        } else if starts_with_ci(&chars, i, "IW") {
            uses_iso = true;
            Some(FieldSpec {
                kind: FieldKind::IsoWeek,
                fill_mode,
                label: "IW",
                token_len: 2,
            })
        } else if starts_with_ci(&chars, i, "IDDD") {
            uses_iso = true;
            Some(FieldSpec {
                kind: FieldKind::IsoDayOfYear,
                fill_mode,
                label: "IDDD",
                token_len: 4,
            })
        } else if starts_with_ci(&chars, i, "ID") {
            uses_iso = true;
            Some(FieldSpec {
                kind: FieldKind::IsoDayOfWeek,
                fill_mode,
                label: "ID",
                token_len: 2,
            })
        } else if starts_with_ci(&chars, i, "I") {
            uses_iso = true;
            Some(FieldSpec {
                kind: FieldKind::Year {
                    width: 1,
                    iso: true,
                    comma: false,
                },
                fill_mode,
                label: "I",
                token_len: 1,
            })
        } else if starts_with_ci(&chars, i, "HH24") {
            Some(FieldSpec {
                kind: FieldKind::Hour24,
                fill_mode,
                label: "HH24",
                token_len: 4,
            })
        } else if starts_with_ci(&chars, i, "HH12") {
            Some(FieldSpec {
                kind: FieldKind::Hour12,
                fill_mode,
                label: "HH12",
                token_len: 4,
            })
        } else if starts_with_ci(&chars, i, "HH") {
            Some(FieldSpec {
                kind: FieldKind::Hour12,
                fill_mode,
                label: "HH",
                token_len: 2,
            })
        } else if starts_with_ci(&chars, i, "SSSSS") {
            Some(FieldSpec {
                kind: FieldKind::SecondsOfDay,
                fill_mode,
                label: "SSSSS",
                token_len: 5,
            })
        } else if starts_with_ci(&chars, i, "SSSS") {
            Some(FieldSpec {
                kind: FieldKind::SecondsOfDay,
                fill_mode,
                label: "SSSS",
                token_len: 4,
            })
        } else if starts_with_ci(&chars, i, "MONTH") {
            Some(FieldSpec {
                kind: FieldKind::MonthFull,
                fill_mode,
                label: month_full_label(&chars, i),
                token_len: 5,
            })
        } else if starts_with_ci(&chars, i, "MON") {
            Some(FieldSpec {
                kind: FieldKind::MonthAbbr,
                fill_mode,
                label: month_abbr_label(&chars, i),
                token_len: 3,
            })
        } else if starts_with_ci(&chars, i, "MISS") {
            items.push(FormatItem::Field(FieldSpec {
                kind: FieldKind::Minute,
                fill_mode,
                label: "MI",
                token_len: 2,
            }));
            items.push(FormatItem::Field(FieldSpec {
                kind: FieldKind::Second,
                fill_mode,
                label: "SS",
                token_len: 2,
            }));
            i += 4;
            continue;
        } else if starts_with_ci(&chars, i, "MM") {
            Some(FieldSpec {
                kind: FieldKind::MonthNumber,
                fill_mode,
                label: "MM",
                token_len: 2,
            })
        } else if starts_with_ci(&chars, i, "DDD") {
            uses_gregorian = true;
            Some(FieldSpec {
                kind: FieldKind::GregorianDayOfYear,
                fill_mode,
                label: "DDD",
                token_len: 3,
            })
        } else if starts_with_ci(&chars, i, "DD") {
            uses_gregorian = true;
            Some(FieldSpec {
                kind: FieldKind::Day,
                fill_mode,
                label: "DD",
                token_len: 2,
            })
        } else if starts_with_ci(&chars, i, "DY") {
            Some(FieldSpec {
                kind: FieldKind::DayOfWeekShort,
                fill_mode,
                label: "DY",
                token_len: 2,
            })
        } else if starts_with_ci(&chars, i, "DAY") {
            Some(FieldSpec {
                kind: FieldKind::DayOfWeekFull,
                fill_mode,
                label: "Day",
                token_len: 3,
            })
        } else if starts_with_ci(&chars, i, "WW") {
            uses_gregorian = true;
            Some(FieldSpec {
                kind: FieldKind::GregorianWeek,
                fill_mode,
                label: "WW",
                token_len: 2,
            })
        } else if starts_with_ci(&chars, i, "W") {
            Some(FieldSpec {
                kind: FieldKind::WeekOfMonth,
                fill_mode,
                label: "W",
                token_len: 1,
            })
        } else if starts_with_ci(&chars, i, "CC") {
            uses_gregorian = true;
            Some(FieldSpec {
                kind: FieldKind::Century,
                fill_mode,
                label: "CC",
                token_len: 2,
            })
        } else if starts_with_ci(&chars, i, "J") {
            uses_gregorian = true;
            Some(FieldSpec {
                kind: FieldKind::JulianDay,
                fill_mode,
                label: "J",
                token_len: 1,
            })
        } else if starts_with_ci(&chars, i, "TZH") {
            Some(FieldSpec {
                kind: FieldKind::TzHour,
                fill_mode,
                label: "TZH",
                token_len: 3,
            })
        } else if starts_with_ci(&chars, i, "TZM") {
            Some(FieldSpec {
                kind: FieldKind::TzMinute,
                fill_mode,
                label: "TZM",
                token_len: 3,
            })
        } else if starts_with_ci(&chars, i, "TZ") {
            Some(FieldSpec {
                kind: FieldKind::TzName,
                fill_mode,
                label: "TZ",
                token_len: 2,
            })
        } else if starts_with_ci(&chars, i, "MS") {
            Some(FieldSpec {
                kind: FieldKind::Millisecond,
                fill_mode,
                label: "MS",
                token_len: 2,
            })
        } else if starts_with_ci(&chars, i, "FF") {
            let precision = chars
                .get(i + 2)
                .and_then(|ch| ch.to_digit(10))
                .filter(|digit| (1..=6).contains(digit))
                .and_then(|digit| u8::try_from(digit).ok())
                .unwrap_or(6);
            let advance = if precision == 6 && chars.get(i + 2) != Some(&'6') {
                2
            } else {
                3
            };
            items.push(FormatItem::Field(FieldSpec {
                kind: FieldKind::FractionalSecond { precision },
                fill_mode,
                label: "FF",
                token_len: advance,
            }));
            i += advance;
            continue;
        } else if starts_with_ci(&chars, i, "MI") {
            Some(FieldSpec {
                kind: FieldKind::Minute,
                fill_mode,
                label: "MI",
                token_len: 2,
            })
        } else if starts_with_ci(&chars, i, "SS") {
            Some(FieldSpec {
                kind: FieldKind::Second,
                fill_mode,
                label: "SS",
                token_len: 2,
            })
        } else if starts_with_ci(&chars, i, "RM") {
            Some(FieldSpec {
                kind: FieldKind::MonthRoman,
                fill_mode,
                label: "RM",
                token_len: 2,
            })
        } else if starts_with_ci(&chars, i, "B.C.") || starts_with_ci(&chars, i, "A.D.") {
            Some(FieldSpec {
                kind: FieldKind::BcAd,
                fill_mode,
                label: "BC",
                token_len: 4,
            })
        } else if starts_with_ci(&chars, i, "BC") || starts_with_ci(&chars, i, "AD") {
            Some(FieldSpec {
                kind: FieldKind::BcAd,
                fill_mode,
                label: "BC",
                token_len: 2,
            })
        } else if starts_with_ci(&chars, i, "P.M.")
            || starts_with_ci(&chars, i, "A.M.")
            || starts_with_ci(&chars, i, "PM")
            || starts_with_ci(&chars, i, "AM")
        {
            Some(FieldSpec {
                kind: FieldKind::Meridiem,
                fill_mode,
                label: "PM",
                token_len: if starts_with_ci(&chars, i, "P.M.") || starts_with_ci(&chars, i, "A.M.")
                {
                    4
                } else {
                    2
                },
            })
        } else if starts_with_ci(&chars, i, "TH") || starts_with_ci(&chars, i, "th") {
            Some(FieldSpec {
                kind: FieldKind::OrdinalSuffix,
                fill_mode,
                label: "TH",
                token_len: 2,
            })
        } else if starts_with_ci(&chars, i, "Q") {
            Some(FieldSpec {
                kind: FieldKind::QuarterIgnored,
                fill_mode,
                label: "Q",
                token_len: 1,
            })
        } else if starts_with_ci(&chars, i, "D") {
            uses_gregorian = true;
            Some(FieldSpec {
                kind: FieldKind::GregorianDayOfWeek,
                fill_mode,
                label: "D",
                token_len: 1,
            })
        } else if starts_with_ci(&chars, i, "Y") {
            uses_gregorian = true;
            Some(FieldSpec {
                kind: FieldKind::Year {
                    width: 1,
                    iso: false,
                    comma: false,
                },
                fill_mode,
                label: "Y",
                token_len: 1,
            })
        } else {
            None
        };

        if let Some(field) = field {
            i += field.token_len;
            items.push(FormatItem::Field(field));
            continue;
        }

        if chars[i].is_ascii_punctuation() {
            items.push(FormatItem::Separator(chars[i]));
            i += 1;
            continue;
        }

        items.push(FormatItem::Separator(chars[i]));
        i += 1;
    }

    CompiledFormat {
        items,
        exact_mode,
        uses_gregorian,
        uses_iso,
    }
}

fn starts_with_ci(chars: &[char], start: usize, needle: &str) -> bool {
    chars[start..]
        .iter()
        .zip(needle.chars())
        .all(|(lhs, rhs)| lhs.eq_ignore_ascii_case(&rhs))
        && chars.len().saturating_sub(start) >= needle.len()
}

fn starts_with_escaped_quote(chars: &[char], start: usize) -> bool {
    escaped_quote_width(chars, start).is_some()
}

fn parse_quoted_literal(chars: &[char], start: usize) -> (String, usize) {
    let mut literal = String::new();
    let mut i = start + 1;
    while i < chars.len() {
        if let Some(width) = escaped_quote_width(chars, i) {
            literal.push('"');
            i += width;
            continue;
        }
        if chars[i] == '"' {
            i += 1;
            return (literal, i);
        }
        literal.push(chars[i]);
        i += 1;
    }
    (literal, i)
}

fn parse_escaped_quote_literal(chars: &[char], start: usize) -> (String, usize) {
    let mut literal = String::from("\"");
    let mut i = start + escaped_quote_width(chars, start).unwrap_or(2);
    while i < chars.len() {
        if let Some(width) = escaped_quote_width(chars, i) {
            literal.push('"');
            i += width;
            if chars.get(i) == Some(&'"') {
                i += 1;
            }
            return (literal, i);
        }
        literal.push(chars[i]);
        i += 1;
    }
    (literal, chars.len())
}

fn escaped_quote_width(chars: &[char], start: usize) -> Option<usize> {
    if chars.get(start) == Some(&'\\') && chars.get(start + 1) == Some(&'"') {
        Some(2)
    } else if chars.get(start) == Some(&'\\')
        && chars.get(start + 1) == Some(&'\\')
        && chars.get(start + 2) == Some(&'"')
    {
        Some(3)
    } else {
        None
    }
}

fn month_abbr_label(chars: &[char], start: usize) -> &'static str {
    if chars
        .get(start..start + 3)
        .is_some_and(|token| token.iter().all(char::is_ascii_uppercase))
    {
        "MON"
    } else {
        "Mon"
    }
}

fn month_full_label(chars: &[char], start: usize) -> &'static str {
    if chars
        .get(start..start + 5)
        .is_some_and(|token| token.iter().all(char::is_ascii_uppercase))
    {
        "MONTH"
    } else {
        "Month"
    }
}
