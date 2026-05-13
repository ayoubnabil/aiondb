use std::borrow::Cow;

use aiondb_core::{DbError, ErrorReport, SqlState};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FormatError {
    UnsupportedField(&'static str),
    InvalidCombination,
    SourceStringTooShort {
        field: &'static str,
        required: usize,
        remaining: usize,
    },
    InvalidValue {
        field: &'static str,
        value: String,
        // Use Cow so dynamic detail strings can be carried as owned values
        // and dropped on error rendering instead of leaked via String::leak()
        // (audit pg_format F1 — authenticated-user-triggerable slow leak).
        detail: Cow<'static, str>,
        hint: Option<&'static str>,
    },
    ConflictingField(&'static str),
    InvalidHourFor12Clock(u8),
    FieldOutOfRange(String),
    YearOutOfRange,
}

impl FormatError {
    pub(crate) fn into_db_error(self, _input: &str) -> DbError {
        match self {
            Self::UnsupportedField(field) => DbError::from_report(ErrorReport::new(
                SqlState::FeatureNotSupported,
                format!("formatting field \"{field}\" is only supported in to_char"),
            )),
            Self::InvalidCombination => DbError::from_report(
                ErrorReport::new(
                    SqlState::InvalidDatetimeFormat,
                    "invalid combination of date conventions",
                )
                .with_client_hint(
                    "Do not mix Gregorian and ISO week date conventions in a formatting template.",
                ),
            ),
            Self::SourceStringTooShort {
                field,
                required,
                remaining,
            } => DbError::from_report(
                ErrorReport::new(
                    SqlState::InvalidDatetimeFormat,
                    format!("source string too short for \"{field}\" formatting field"),
                )
                .with_client_detail(format!(
                    "Field requires {required} characters, but only {remaining} remain."
                ))
                .with_client_hint(
                    "If your source string is not fixed-width, try using the \"FM\" modifier.",
                ),
            ),
            Self::InvalidValue {
                field,
                value,
                detail,
                hint,
            } => {
                let report = ErrorReport::new(
                    SqlState::InvalidDatetimeFormat,
                    format!("invalid value \"{value}\" for \"{field}\""),
                )
                .with_client_detail(detail);
                DbError::from_report(match hint {
                    Some(hint) => report.with_client_hint(hint),
                    None => report,
                })
            }
            Self::ConflictingField(field) => DbError::from_report(
                ErrorReport::new(
                    SqlState::InvalidDatetimeFormat,
                    format!("conflicting values for \"{field}\" field in formatting string"),
                )
                .with_client_detail(
                    "This value contradicts a previous setting for the same field type.",
                ),
            ),
            Self::InvalidHourFor12Clock(hour) => DbError::from_report(
                ErrorReport::new(
                    SqlState::InvalidDatetimeFormat,
                    format!("hour \"{hour}\" is invalid for the 12-hour clock"),
                )
                .with_client_hint("Use the 24-hour clock, or give an hour between 1 and 12."),
            ),
            Self::FieldOutOfRange(value) => DbError::from_report(ErrorReport::new(
                SqlState::InvalidDatetimeFormat,
                format!("date/time field value out of range: \"{value}\""),
            )),
            Self::YearOutOfRange => DbError::from_report(
                ErrorReport::new(
                    SqlState::InvalidDatetimeFormat,
                    "value for \"YYYY\" in source string is out of range",
                )
                .with_client_detail("Value must be in the range -2147483648 to 2147483647."),
            ),
        }
    }
}
