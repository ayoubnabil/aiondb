use std::{collections::HashMap, sync::OnceLock};

use time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time, UtcOffset, Weekday};

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TimeZoneSetting {
    raw: String,
    rule: TimeZoneRule,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
enum TimeZoneRule {
    Fixed(FixedTimeZone),
    Daylight(DaylightTimeZone),
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
struct FixedTimeZone {
    offset: UtcOffset,
    label: String,
    show_value: String,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
struct DaylightTimeZone {
    standard_abbr: String,
    daylight_abbr: String,
    standard_offset: UtcOffset,
    daylight_offset: UtcOffset,
    rules: DstRules,
    show_value: String,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
enum DstRules {
    Us,
    Eu,
    Posix {
        start: TransitionRule,
        end: TransitionRule,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
struct TransitionRule {
    month: Month,
    week: u8,
    weekday: Weekday,
}

impl TimeZoneSetting {
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        Self::try_parse(raw).unwrap_or_else(|| Self::fixed(UtcOffset::UTC, "UTC", "UTC"))
    }

    #[must_use]
    pub fn try_parse(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Some(Self::fixed(UtcOffset::UTC, "UTC", "UTC"));
        }
        if let Some(setting) = parse_canonical_fixed_display(trimmed) {
            return Some(setting);
        }
        if let Some(setting) = parse_named_timezone(trimmed) {
            return Some(setting);
        }
        if let Some(setting) = parse_posix_timezone(trimmed) {
            return Some(setting);
        }
        if let Some(setting) = parse_prefixed_offset_timezone(trimmed) {
            return Some(setting);
        }
        if let Some(setting) = parse_abbreviation_timezone(trimmed) {
            return Some(setting);
        }
        if let Some(setting) = parse_sql_fixed_offset(trimmed) {
            return Some(setting);
        }
        None
    }

    #[must_use]
    pub fn show_value(&self) -> String {
        match &self.rule {
            TimeZoneRule::Fixed(rule) => rule.show_value.clone(),
            TimeZoneRule::Daylight(rule) => rule.show_value.clone(),
        }
    }

    #[must_use]
    pub fn offset_for_date(&self, date: Date) -> UtcOffset {
        self.offset_for_local(PrimitiveDateTime::new(date, Time::MIDNIGHT))
    }

    #[must_use]
    pub fn offset_for_local(&self, local: PrimitiveDateTime) -> UtcOffset {
        match &self.rule {
            TimeZoneRule::Fixed(rule) => rule.offset,
            TimeZoneRule::Daylight(rule) => {
                if rule.is_daylight(local) {
                    rule.daylight_offset
                } else {
                    rule.standard_offset
                }
            }
        }
    }

    #[must_use]
    pub fn apply_to_local(&self, local: PrimitiveDateTime) -> OffsetDateTime {
        local.assume_offset(self.offset_for_local(local))
    }

    #[must_use]
    pub fn parts_for_utc(&self, timestamp: OffsetDateTime) -> (UtcOffset, String) {
        let mut offset = self.offset_for_date(timestamp.date());
        for _ in 0..3 {
            // `to_offset` panics when the shifted local datetime would overflow
            // the representable range. In pg-regress timestamp edge cases we
            // prefer graceful degradation over process panic.
            let Some(local) = timestamp.checked_to_offset(offset) else {
                let fallback_local = timestamp.to_offset(UtcOffset::UTC);
                let fallback_local_ts =
                    PrimitiveDateTime::new(fallback_local.date(), fallback_local.time());
                return (offset, self.label_for_local(fallback_local_ts));
            };
            let local_timestamp = PrimitiveDateTime::new(local.date(), local.time());
            let resolved = self.offset_for_local(local_timestamp);
            if resolved == offset {
                return (offset, self.label_for_local(local_timestamp));
            }
            offset = resolved;
        }

        let local = timestamp
            .checked_to_offset(offset)
            .unwrap_or_else(|| timestamp.to_offset(UtcOffset::UTC));
        (
            offset,
            self.label_for_local(PrimitiveDateTime::new(local.date(), local.time())),
        )
    }

    #[must_use]
    pub fn label_for_local(&self, local: PrimitiveDateTime) -> String {
        match &self.rule {
            TimeZoneRule::Fixed(rule) => rule.label.clone(),
            TimeZoneRule::Daylight(rule) => {
                if rule.is_daylight(local) {
                    rule.daylight_abbr.clone()
                } else {
                    rule.standard_abbr.clone()
                }
            }
        }
    }

    fn fixed(offset: UtcOffset, label: &str, show_value: &str) -> Self {
        Self {
            raw: show_value.to_owned(),
            rule: TimeZoneRule::Fixed(FixedTimeZone {
                offset,
                label: label.to_owned(),
                show_value: show_value.to_owned(),
            }),
        }
    }
}

impl DaylightTimeZone {
    fn is_daylight(&self, local: PrimitiveDateTime) -> bool {
        let (start, end) = match &self.rules {
            DstRules::Us => match us_dst_transition_rules(local.year()) {
                Some((start, end)) => (
                    transition_datetime(local.year(), start),
                    transition_datetime(local.year(), end),
                ),
                None => return false,
            },
            DstRules::Eu => {
                let rules = eu_dst_transition_rules();
                (
                    transition_datetime(local.year(), rules.0),
                    transition_datetime(local.year(), rules.1),
                )
            }
            DstRules::Posix { start, end } => (
                transition_datetime(local.year(), *start),
                transition_datetime(local.year(), *end),
            ),
        };

        match (start, end) {
            (Some(start), Some(end)) if start < end => local >= start && local < end,
            (Some(start), Some(end)) => local >= start || local < end,
            _ => false,
        }
    }
}

fn us_dst_transition_rules(year: i32) -> Option<(TransitionRule, TransitionRule)> {
    if year < 1918 {
        return None;
    }

    let (start, end) = if year >= 2007 {
        (
            TransitionRule {
                month: Month::March,
                week: 2,
                weekday: Weekday::Sunday,
            },
            TransitionRule {
                month: Month::November,
                week: 1,
                weekday: Weekday::Sunday,
            },
        )
    } else if year >= 1987 {
        (
            TransitionRule {
                month: Month::April,
                week: 1,
                weekday: Weekday::Sunday,
            },
            TransitionRule {
                month: Month::October,
                week: 5,
                weekday: Weekday::Sunday,
            },
        )
    } else {
        (
            TransitionRule {
                month: Month::April,
                week: 5,
                weekday: Weekday::Sunday,
            },
            TransitionRule {
                month: Month::October,
                week: 5,
                weekday: Weekday::Sunday,
            },
        )
    };

    Some((start, end))
}

/// EU DST rules: last Sunday of March → last Sunday of October.
///
/// The actual EU switchover happens at 01:00 UTC, which is 02:00 CET or
/// 03:00 EEST.  `transition_datetime()` always produces 02:00 local, so
/// the result is exact for CET-based zones and close enough for others
/// (the ambiguity window is the same 1-hour gap/overlap as with US
/// rules).
fn eu_dst_transition_rules() -> (TransitionRule, TransitionRule) {
    let start = TransitionRule {
        month: Month::March,
        week: 5, // week=5 means "last" in this codebase
        weekday: Weekday::Sunday,
    };
    let end = TransitionRule {
        month: Month::October,
        week: 5,
        weekday: Weekday::Sunday,
    };
    (start, end)
}

// ---------------------------------------------------------------------------
// Comprehensive IANA timezone mapping table
// ---------------------------------------------------------------------------

/// DST flavour carried in the static table.  `None` means fixed offset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DstType {
    /// No DST – produce a `TimeZoneRule::Fixed`.
    None,
    /// US rules (2nd Sunday March → 1st Sunday November, post-2006).
    Us,
    /// EU rules (last Sunday March → last Sunday October).
    Eu,
    /// Southern-hemisphere inverted (1st Sunday October → 1st Sunday April).
    /// Used by Australia/Sydney, Australia/Melbourne, Pacific/Auckland, etc.
    SouthernHemisphere,
    /// Chile rules (1st Saturday of September → 1st Saturday of April).
    Chile,
}

type TimeZoneTableEntry = (
    &'static str,
    &'static str,
    &'static str,
    i8,
    i8,
    i8,
    i8,
    DstType,
);

static TIMEZONE_LOOKUP: OnceLock<HashMap<&'static str, &'static TimeZoneTableEntry>> =
    OnceLock::new();

/// `(IANA_NAME, std_abbr, dst_abbr, std_hours, std_minutes, dst_hours, dst_minutes, dst_type)`
///
/// All names are stored **uppercase** so lookup is a simple comparison after
/// `to_ascii_uppercase()`.
static TIMEZONE_TABLE: &[TimeZoneTableEntry] = &[
    // ── Americas ──────────────────────────────────────────────────────
    (
        "AMERICA/LOS_ANGELES",
        "PST",
        "PDT",
        -8,
        0,
        -7,
        0,
        DstType::Us,
    ),
    ("AMERICA/DENVER", "MST", "MDT", -7, 0, -6, 0, DstType::Us),
    ("AMERICA/CHICAGO", "CST", "CDT", -6, 0, -5, 0, DstType::Us),
    ("AMERICA/NEW_YORK", "EST", "EDT", -5, 0, -4, 0, DstType::Us),
    (
        "AMERICA/ANCHORAGE",
        "AKST",
        "AKDT",
        -9,
        0,
        -8,
        0,
        DstType::Us,
    ),
    ("AMERICA/PHOENIX", "MST", "MST", -7, 0, -7, 0, DstType::None),
    ("AMERICA/TORONTO", "EST", "EDT", -5, 0, -4, 0, DstType::Us),
    ("AMERICA/VANCOUVER", "PST", "PDT", -8, 0, -7, 0, DstType::Us),
    ("AMERICA/MONTREAL", "EST", "EDT", -5, 0, -4, 0, DstType::Us),
    (
        "AMERICA/SAO_PAULO",
        "BRT",
        "BRT",
        -3,
        0,
        -3,
        0,
        DstType::None,
    ),
    (
        "AMERICA/ARGENTINA/BUENOS_AIRES",
        "ART",
        "ART",
        -3,
        0,
        -3,
        0,
        DstType::None,
    ),
    (
        "AMERICA/MEXICO_CITY",
        "CST",
        "CST",
        -6,
        0,
        -6,
        0,
        DstType::None,
    ),
    ("AMERICA/BOGOTA", "COT", "COT", -5, 0, -5, 0, DstType::None),
    ("AMERICA/LIMA", "PET", "PET", -5, 0, -5, 0, DstType::None),
    (
        "AMERICA/SANTIAGO",
        "CLT",
        "CLST",
        -4,
        0,
        -3,
        0,
        DstType::Chile,
    ),
    ("AMERICA/WINNIPEG", "CST", "CDT", -6, 0, -5, 0, DstType::Us),
    ("AMERICA/EDMONTON", "MST", "MDT", -7, 0, -6, 0, DstType::Us),
    ("AMERICA/HALIFAX", "AST", "ADT", -4, 0, -3, 0, DstType::Us),
    (
        "AMERICA/ST_JOHNS",
        "NST",
        "NDT",
        -3,
        -30,
        -2,
        -30,
        DstType::Us,
    ),
    // ── Europe ────────────────────────────────────────────────────────
    ("EUROPE/LONDON", "GMT", "BST", 0, 0, 1, 0, DstType::Eu),
    ("EUROPE/PARIS", "CET", "CEST", 1, 0, 2, 0, DstType::Eu),
    ("EUROPE/BERLIN", "CET", "CEST", 1, 0, 2, 0, DstType::Eu),
    ("EUROPE/MADRID", "CET", "CEST", 1, 0, 2, 0, DstType::Eu),
    ("EUROPE/ROME", "CET", "CEST", 1, 0, 2, 0, DstType::Eu),
    ("EUROPE/AMSTERDAM", "CET", "CEST", 1, 0, 2, 0, DstType::Eu),
    ("EUROPE/BRUSSELS", "CET", "CEST", 1, 0, 2, 0, DstType::Eu),
    ("EUROPE/ZURICH", "CET", "CEST", 1, 0, 2, 0, DstType::Eu),
    ("EUROPE/VIENNA", "CET", "CEST", 1, 0, 2, 0, DstType::Eu),
    ("EUROPE/STOCKHOLM", "CET", "CEST", 1, 0, 2, 0, DstType::Eu),
    ("EUROPE/OSLO", "CET", "CEST", 1, 0, 2, 0, DstType::Eu),
    ("EUROPE/HELSINKI", "EET", "EEST", 2, 0, 3, 0, DstType::Eu),
    ("EUROPE/WARSAW", "CET", "CEST", 1, 0, 2, 0, DstType::Eu),
    ("EUROPE/PRAGUE", "CET", "CEST", 1, 0, 2, 0, DstType::Eu),
    ("EUROPE/BUDAPEST", "CET", "CEST", 1, 0, 2, 0, DstType::Eu),
    ("EUROPE/BUCHAREST", "EET", "EEST", 2, 0, 3, 0, DstType::Eu),
    ("EUROPE/ATHENS", "EET", "EEST", 2, 0, 3, 0, DstType::Eu),
    ("EUROPE/ISTANBUL", "TRT", "TRT", 3, 0, 3, 0, DstType::None),
    ("EUROPE/MOSCOW", "MSK", "MSK", 3, 0, 3, 0, DstType::None),
    ("EUROPE/KIEV", "EET", "EEST", 2, 0, 3, 0, DstType::Eu),
    ("EUROPE/KYIV", "EET", "EEST", 2, 0, 3, 0, DstType::Eu),
    ("EUROPE/LISBON", "WET", "WEST", 0, 0, 1, 0, DstType::Eu),
    ("EUROPE/DUBLIN", "GMT", "IST", 0, 0, 1, 0, DstType::Eu),
    ("EUROPE/COPENHAGEN", "CET", "CEST", 1, 0, 2, 0, DstType::Eu),
    // ── Asia ──────────────────────────────────────────────────────────
    ("ASIA/TOKYO", "JST", "JST", 9, 0, 9, 0, DstType::None),
    ("ASIA/SHANGHAI", "CST", "CST", 8, 0, 8, 0, DstType::None),
    ("ASIA/HONG_KONG", "HKT", "HKT", 8, 0, 8, 0, DstType::None),
    ("ASIA/SINGAPORE", "SGT", "SGT", 8, 0, 8, 0, DstType::None),
    ("ASIA/SEOUL", "KST", "KST", 9, 0, 9, 0, DstType::None),
    ("ASIA/TAIPEI", "CST", "CST", 8, 0, 8, 0, DstType::None),
    ("ASIA/KOLKATA", "IST", "IST", 5, 30, 5, 30, DstType::None),
    ("ASIA/CALCUTTA", "IST", "IST", 5, 30, 5, 30, DstType::None),
    ("ASIA/DUBAI", "GST", "GST", 4, 0, 4, 0, DstType::None),
    ("ASIA/BANGKOK", "ICT", "ICT", 7, 0, 7, 0, DstType::None),
    ("ASIA/JAKARTA", "WIB", "WIB", 7, 0, 7, 0, DstType::None),
    ("ASIA/KARACHI", "PKT", "PKT", 5, 0, 5, 0, DstType::None),
    ("ASIA/TEHRAN", "IRST", "IRDT", 3, 30, 4, 30, DstType::None),
    ("ASIA/JERUSALEM", "IST", "IDT", 2, 0, 3, 0, DstType::Eu),
    ("ASIA/KATHMANDU", "NPT", "NPT", 5, 45, 5, 45, DstType::None),
    ("ASIA/COLOMBO", "IST", "IST", 5, 30, 5, 30, DstType::None),
    ("ASIA/DHAKA", "BST", "BST", 6, 0, 6, 0, DstType::None),
    ("ASIA/RIYADH", "AST", "AST", 3, 0, 3, 0, DstType::None),
    ("ASIA/KUALA_LUMPUR", "MYT", "MYT", 8, 0, 8, 0, DstType::None),
    ("ASIA/MANILA", "PHT", "PHT", 8, 0, 8, 0, DstType::None),
    ("ASIA/HO_CHI_MINH", "ICT", "ICT", 7, 0, 7, 0, DstType::None),
    // ── Oceania ───────────────────────────────────────────────────────
    (
        "AUSTRALIA/SYDNEY",
        "AEST",
        "AEDT",
        10,
        0,
        11,
        0,
        DstType::SouthernHemisphere,
    ),
    (
        "AUSTRALIA/MELBOURNE",
        "AEST",
        "AEDT",
        10,
        0,
        11,
        0,
        DstType::SouthernHemisphere,
    ),
    ("AUSTRALIA/PERTH", "AWST", "AWST", 8, 0, 8, 0, DstType::None),
    (
        "AUSTRALIA/BRISBANE",
        "AEST",
        "AEST",
        10,
        0,
        10,
        0,
        DstType::None,
    ),
    (
        "AUSTRALIA/ADELAIDE",
        "ACST",
        "ACDT",
        9,
        30,
        10,
        30,
        DstType::SouthernHemisphere,
    ),
    (
        "AUSTRALIA/HOBART",
        "AEST",
        "AEDT",
        10,
        0,
        11,
        0,
        DstType::SouthernHemisphere,
    ),
    (
        "AUSTRALIA/DARWIN",
        "ACST",
        "ACST",
        9,
        30,
        9,
        30,
        DstType::None,
    ),
    (
        "PACIFIC/AUCKLAND",
        "NZST",
        "NZDT",
        12,
        0,
        13,
        0,
        DstType::SouthernHemisphere,
    ),
    (
        "PACIFIC/HONOLULU",
        "HST",
        "HST",
        -10,
        0,
        -10,
        0,
        DstType::None,
    ),
    ("PACIFIC/FIJI", "FJT", "FJT", 12, 0, 12, 0, DstType::None),
    (
        "PACIFIC/CHATHAM",
        "CHAST",
        "CHADT",
        12,
        45,
        13,
        45,
        DstType::SouthernHemisphere,
    ),
    // ── Africa ────────────────────────────────────────────────────────
    ("AFRICA/CAIRO", "EET", "EET", 2, 0, 2, 0, DstType::None),
    ("AFRICA/LAGOS", "WAT", "WAT", 1, 0, 1, 0, DstType::None),
    (
        "AFRICA/JOHANNESBURG",
        "SAST",
        "SAST",
        2,
        0,
        2,
        0,
        DstType::None,
    ),
    ("AFRICA/NAIROBI", "EAT", "EAT", 3, 0, 3, 0, DstType::None),
    (
        "AFRICA/CASABLANCA",
        "WET",
        "WEST",
        0,
        0,
        1,
        0,
        DstType::None,
    ),
    // ── Common aliases ────────────────────────────────────────────────
    ("US/PACIFIC", "PST", "PDT", -8, 0, -7, 0, DstType::Us),
    ("US/MOUNTAIN", "MST", "MDT", -7, 0, -6, 0, DstType::Us),
    ("US/CENTRAL", "CST", "CDT", -6, 0, -5, 0, DstType::Us),
    ("US/EASTERN", "EST", "EDT", -5, 0, -4, 0, DstType::Us),
    ("US/HAWAII", "HST", "HST", -10, 0, -10, 0, DstType::None),
    ("US/ALASKA", "AKST", "AKDT", -9, 0, -8, 0, DstType::Us),
    ("US/ARIZONA", "MST", "MST", -7, 0, -7, 0, DstType::None),
    ("CANADA/EASTERN", "EST", "EDT", -5, 0, -4, 0, DstType::Us),
    ("CANADA/CENTRAL", "CST", "CDT", -6, 0, -5, 0, DstType::Us),
    ("CANADA/PACIFIC", "PST", "PDT", -8, 0, -7, 0, DstType::Us),
    ("CANADA/MOUNTAIN", "MST", "MDT", -7, 0, -6, 0, DstType::Us),
    ("CANADA/ATLANTIC", "AST", "ADT", -4, 0, -3, 0, DstType::Us),
    ("GMT", "GMT", "GMT", 0, 0, 0, 0, DstType::None),
    ("UTC", "UTC", "UTC", 0, 0, 0, 0, DstType::None),
];

/// Southern-hemisphere DST rules used by most of Australia and New Zealand.
/// Clocks spring forward on the first Sunday of October and fall back on
/// the first Sunday of April.
fn southern_hemisphere_dst_rules() -> (TransitionRule, TransitionRule) {
    let start = TransitionRule {
        month: Month::October,
        week: 1,
        weekday: Weekday::Sunday,
    };
    let end = TransitionRule {
        month: Month::April,
        week: 1,
        weekday: Weekday::Sunday,
    };
    (start, end)
}

/// Chile DST rules: clocks spring forward on the first Saturday of
/// September and fall back on the first Saturday of April.
fn chile_dst_rules() -> (TransitionRule, TransitionRule) {
    let start = TransitionRule {
        month: Month::September,
        week: 1,
        weekday: Weekday::Saturday,
    };
    let end = TransitionRule {
        month: Month::April,
        week: 1,
        weekday: Weekday::Saturday,
    };
    (start, end)
}

/// Build a `TimeZoneSetting` from a table entry.
fn build_timezone_setting(raw: &str, entry: &TimeZoneTableEntry) -> Option<TimeZoneSetting> {
    let &(_, std_abbr, dst_abbr, std_h, std_m, dst_h, dst_m, dst_type) = entry;
    let standard_offset = UtcOffset::from_hms(std_h, std_m, 0).ok()?;

    if matches!(dst_type, DstType::None) {
        return Some(TimeZoneSetting {
            raw: raw.to_owned(),
            rule: TimeZoneRule::Fixed(FixedTimeZone {
                offset: standard_offset,
                label: std_abbr.to_owned(),
                show_value: raw.to_owned(),
            }),
        });
    }

    let daylight_offset = UtcOffset::from_hms(dst_h, dst_m, 0).ok()?;
    let rules = match dst_type {
        DstType::Us => DstRules::Us,
        DstType::Eu => DstRules::Eu,
        DstType::SouthernHemisphere => {
            let (start, end) = southern_hemisphere_dst_rules();
            DstRules::Posix { start, end }
        }
        DstType::Chile => {
            let (start, end) = chile_dst_rules();
            DstRules::Posix { start, end }
        }
        DstType::None => unreachable!(),
    };

    Some(TimeZoneSetting {
        raw: raw.to_owned(),
        rule: TimeZoneRule::Daylight(DaylightTimeZone {
            standard_abbr: std_abbr.to_owned(),
            daylight_abbr: dst_abbr.to_owned(),
            standard_offset,
            daylight_offset,
            rules,
            show_value: raw.to_owned(),
        }),
    })
}

fn named_timezone_lookup() -> &'static HashMap<&'static str, &'static TimeZoneTableEntry> {
    TIMEZONE_LOOKUP.get_or_init(|| {
        TIMEZONE_TABLE
            .iter()
            .map(|entry| (entry.0, entry))
            .collect::<HashMap<_, _>>()
    })
}

fn parse_named_timezone(raw: &str) -> Option<TimeZoneSetting> {
    let trimmed = raw.trim();
    let upper = trimmed.to_ascii_uppercase();
    let entry = named_timezone_lookup().get(upper.as_str())?;
    build_timezone_setting(trimmed, entry)
}

fn parse_posix_timezone(raw: &str) -> Option<TimeZoneSetting> {
    let trimmed = raw.trim();
    let std_len = trimmed
        .chars()
        .take_while(char::is_ascii_alphabetic)
        .count();
    if std_len < 3 {
        return None;
    }

    let standard_abbr = &trimmed[..std_len];
    let mut rest = &trimmed[std_len..];
    let offset_len = rest
        .chars()
        .take_while(|ch| ch.is_ascii_digit() || matches!(ch, '+' | '-' | ':' | '.'))
        .count();
    if offset_len == 0 {
        return None;
    }
    let standard_offset = parse_posix_offset(&rest[..offset_len])?;
    rest = &rest[offset_len..];

    let mut rules = DstRules::Us;
    let daylight_abbr = if rest.is_empty() {
        None
    } else {
        let comma = rest.find(',').unwrap_or(rest.len());
        let head = &rest[..comma];
        let dst_len = head.chars().take_while(char::is_ascii_alphabetic).count();
        if dst_len < 3 {
            return None;
        }
        let daylight_abbr = &head[..dst_len];
        if let Some(rule_text) = rest.get(comma + usize::from(comma < rest.len())..) {
            if !rule_text.is_empty() {
                let mut parts = rule_text.split(',');
                let start = parse_transition_rule(parts.next()?)?;
                let end = parse_transition_rule(parts.next()?)?;
                rules = DstRules::Posix { start, end };
            }
        }
        Some(daylight_abbr.to_owned())
    };

    let Some(daylight_abbr) = daylight_abbr else {
        return Some(TimeZoneSetting {
            raw: trimmed.to_owned(),
            rule: TimeZoneRule::Fixed(FixedTimeZone {
                offset: standard_offset,
                label: standard_abbr.to_ascii_uppercase(),
                show_value: trimmed.to_owned(),
            }),
        });
    };

    let daylight_offset =
        UtcOffset::from_whole_seconds(standard_offset.whole_seconds() + 3_600).ok()?;
    Some(TimeZoneSetting {
        raw: trimmed.to_owned(),
        rule: TimeZoneRule::Daylight(DaylightTimeZone {
            standard_abbr: standard_abbr.to_ascii_uppercase(),
            daylight_abbr: daylight_abbr.to_ascii_uppercase(),
            standard_offset,
            daylight_offset,
            rules,
            show_value: trimmed.to_owned(),
        }),
    })
}

fn parse_prefixed_offset_timezone(raw: &str) -> Option<TimeZoneSetting> {
    let upper = raw.trim().to_ascii_uppercase();
    for prefix in ["UTC", "GMT"] {
        if let Some(suffix) = upper.strip_prefix(prefix) {
            if suffix.is_empty() {
                return Some(TimeZoneSetting::fixed(UtcOffset::UTC, "UTC", raw.trim()));
            }
            let offset = parse_posix_offset(suffix)?;
            return Some(TimeZoneSetting {
                raw: raw.trim().to_owned(),
                rule: TimeZoneRule::Fixed(FixedTimeZone {
                    offset,
                    label: format_offset_label(offset),
                    show_value: raw.trim().to_owned(),
                }),
            });
        }
    }
    None
}

fn parse_abbreviation_timezone(raw: &str) -> Option<TimeZoneSetting> {
    let upper = raw.trim().to_ascii_uppercase();
    let (offset, label) = match upper.as_str() {
        "UTC" | "Z" => (UtcOffset::UTC, "UTC"),
        "EST" => (UtcOffset::from_hms(-5, 0, 0).ok()?, "EST"),
        "EDT" => (UtcOffset::from_hms(-4, 0, 0).ok()?, "EDT"),
        "CST" => (UtcOffset::from_hms(-6, 0, 0).ok()?, "CST"),
        "CDT" => (UtcOffset::from_hms(-5, 0, 0).ok()?, "CDT"),
        "MST" => (UtcOffset::from_hms(-7, 0, 0).ok()?, "MST"),
        "MDT" => (UtcOffset::from_hms(-6, 0, 0).ok()?, "MDT"),
        "PST" => (UtcOffset::from_hms(-8, 0, 0).ok()?, "PST"),
        "PDT" => (UtcOffset::from_hms(-7, 0, 0).ok()?, "PDT"),
        "CET" => (UtcOffset::from_hms(1, 0, 0).ok()?, "CET"),
        "CEST" => (UtcOffset::from_hms(2, 0, 0).ok()?, "CEST"),
        _ => return None,
    };
    Some(TimeZoneSetting::fixed(offset, label, raw.trim()))
}

fn parse_sql_fixed_offset(raw: &str) -> Option<TimeZoneSetting> {
    let offset = parse_direct_offset(raw)?;
    let label = format_offset_label(offset);
    let show_value = format!("<{label}>{}", format_offset_label(invert_offset(offset)));
    Some(TimeZoneSetting {
        raw: raw.trim().to_owned(),
        rule: TimeZoneRule::Fixed(FixedTimeZone {
            offset,
            label,
            show_value,
        }),
    })
}

fn parse_canonical_fixed_display(raw: &str) -> Option<TimeZoneSetting> {
    let inner = raw.strip_prefix('<')?;
    let (label, _) = inner.split_once('>')?;
    let offset = parse_direct_offset(label)?;
    Some(TimeZoneSetting {
        raw: raw.trim().to_owned(),
        rule: TimeZoneRule::Fixed(FixedTimeZone {
            offset,
            label: label.to_owned(),
            show_value: raw.trim().to_owned(),
        }),
    })
}

fn parse_posix_offset(raw: &str) -> Option<UtcOffset> {
    let direct = parse_direct_offset(raw)?;
    UtcOffset::from_whole_seconds(-direct.whole_seconds()).ok()
}

fn parse_direct_offset(raw: &str) -> Option<UtcOffset> {
    let trimmed = raw.trim();
    let (sign, rest) = if let Some(rest) = trimmed.strip_prefix('-') {
        (-1i32, rest)
    } else if let Some(rest) = trimmed.strip_prefix('+') {
        (1i32, rest)
    } else {
        (1, trimmed)
    };

    let total_seconds = if rest.contains(':') {
        let mut parts = rest.split(':');
        let hours_part = parts.next()?;
        let minutes_part = parts.next()?;
        let seconds_part = parts.next();
        if parts.next().is_some() {
            return None;
        }

        if [hours_part, minutes_part]
            .into_iter()
            .any(|part| part.is_empty() || !part.chars().all(|ch| ch.is_ascii_digit()))
        {
            return None;
        }

        if let Some(seconds_part) = seconds_part {
            if seconds_part.is_empty() || !seconds_part.chars().all(|ch| ch.is_ascii_digit()) {
                return None;
            }
        }

        let hours = hours_part.parse::<i32>().ok()?;
        let minutes = minutes_part.parse::<i32>().ok()?;
        let seconds = if let Some(seconds_part) = seconds_part {
            seconds_part.parse::<i32>().ok()?
        } else {
            0
        };
        if minutes >= 60 || seconds >= 60 {
            return None;
        }

        let base_seconds = i64::from(hours)
            .checked_mul(3_600)?
            .checked_add(i64::from(minutes).checked_mul(60)?)?
            .checked_add(i64::from(seconds))?;
        let signed = i64::from(sign).checked_mul(base_seconds)?;
        i32::try_from(signed).ok()?
    } else if rest.contains('.') {
        let value = rest.parse::<f64>().ok()?;
        let rounded = (f64::from(sign) * value * 3_600.0).round();
        if !rounded.is_finite() || rounded < f64::from(i32::MIN) || rounded > f64::from(i32::MAX) {
            return None;
        }
        let rounded_seconds = format!("{rounded:.0}").parse::<i64>().ok()?;
        i32::try_from(rounded_seconds).ok()?
    } else {
        if !rest.chars().all(|ch| ch.is_ascii_digit()) {
            return None;
        }
        let hours = rest.parse::<i32>().ok()?;
        let base_seconds = i64::from(hours).checked_mul(3_600)?;
        let signed = i64::from(sign).checked_mul(base_seconds)?;
        i32::try_from(signed).ok()?
    };

    UtcOffset::from_whole_seconds(total_seconds).ok()
}

fn invert_offset(offset: UtcOffset) -> UtcOffset {
    UtcOffset::from_whole_seconds(-offset.whole_seconds()).unwrap_or(UtcOffset::UTC)
}

fn parse_transition_rule(raw: &str) -> Option<TransitionRule> {
    let raw = raw.trim().split('/').next()?;
    let raw = raw.strip_prefix('M')?;
    let mut parts = raw.split('.');
    let month = Month::try_from(parts.next()?.parse::<u8>().ok()?).ok()?;
    let week = parts.next()?.parse::<u8>().ok()?;
    let weekday = match parts.next()?.parse::<u8>().ok()? {
        0 => Weekday::Sunday,
        1 => Weekday::Monday,
        2 => Weekday::Tuesday,
        3 => Weekday::Wednesday,
        4 => Weekday::Thursday,
        5 => Weekday::Friday,
        6 => Weekday::Saturday,
        _ => return None,
    };
    Some(TransitionRule {
        month,
        week,
        weekday,
    })
}

fn transition_datetime(year: i32, rule: TransitionRule) -> Option<PrimitiveDateTime> {
    let date = transition_date(year, rule)?;
    Some(PrimitiveDateTime::new(date, Time::from_hms(2, 0, 0).ok()?))
}

fn transition_date(year: i32, rule: TransitionRule) -> Option<Date> {
    if rule.week == 5 {
        return last_weekday_of_month(year, rule.month, rule.weekday);
    }

    let mut day = 1u8;
    let mut seen = 0u8;
    loop {
        let date = Date::from_calendar_date(year, rule.month, day).ok()?;
        if date.weekday() == rule.weekday {
            seen += 1;
            if seen == rule.week {
                return Some(date);
            }
        }
        day = day.checked_add(1)?;
    }
}

fn last_weekday_of_month(year: i32, month: Month, weekday: Weekday) -> Option<Date> {
    for day in (1..=31).rev() {
        let Ok(date) = Date::from_calendar_date(year, month, day) else {
            continue;
        };
        if date.weekday() == weekday {
            return Some(date);
        }
    }
    None
}

fn format_offset_label(offset: UtcOffset) -> String {
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
    fn parses_pacific_dst_labels() {
        let timezone = TimeZoneSetting::parse("PST8PDT");
        let winter = PrimitiveDateTime::new(
            Date::from_calendar_date(2024, Month::January, 1).unwrap(),
            Time::MIDNIGHT,
        );
        let summer = PrimitiveDateTime::new(
            Date::from_calendar_date(2024, Month::July, 1).unwrap(),
            Time::MIDNIGHT,
        );
        assert_eq!(timezone.label_for_local(winter), "PST");
        assert_eq!(timezone.label_for_local(summer), "PDT");
    }

    #[test]
    fn parses_custom_posix_rules() {
        let timezone = TimeZoneSetting::parse("CST7CDT,M4.1.0,M10.5.0");
        let april = PrimitiveDateTime::new(
            Date::from_calendar_date(2005, Month::April, 3).unwrap(),
            Time::from_hms(12, 0, 0).unwrap(),
        );
        let october = PrimitiveDateTime::new(
            Date::from_calendar_date(2005, Month::October, 30).unwrap(),
            Time::from_hms(12, 0, 0).unwrap(),
        );
        assert_eq!(timezone.label_for_local(april), "CDT");
        assert_eq!(timezone.label_for_local(october), "CST");
    }

    #[test]
    fn canonicalizes_sql_fixed_offsets() {
        let timezone = TimeZoneSetting::parse("-1.5");
        assert_eq!(timezone.show_value(), "<-01:30>+01:30");
        assert_eq!(
            timezone.offset_for_date(Date::from_calendar_date(2024, Month::January, 1).unwrap()),
            UtcOffset::from_hms(-1, -30, 0).unwrap()
        );
    }

    #[test]
    fn rejects_time_with_embedded_numeric_offset_as_timezone_token() {
        assert!(TimeZoneSetting::try_parse("00:00:00-08").is_none());
    }

    #[test]
    fn us_dst_rules_do_not_mark_pre_1918_dates_as_daylight() {
        let timezone = TimeZoneSetting::parse("PST8PDT");
        let historical = PrimitiveDateTime::new(
            Date::from_calendar_date(1582, Month::August, 21).unwrap(),
            Time::MIDNIGHT,
        );
        assert_eq!(timezone.label_for_local(historical), "PST");
    }

    #[test]
    fn us_dst_rules_keep_mid_march_2001_in_standard_time() {
        let timezone = TimeZoneSetting::parse("PST8PDT");
        let march_2001 = PrimitiveDateTime::new(
            Date::from_calendar_date(2001, Month::March, 15).unwrap(),
            Time::from_hms(12, 0, 0).unwrap(),
        );
        assert_eq!(timezone.label_for_local(march_2001), "PST");
    }

    #[test]
    fn us_dst_rules_mark_mid_march_2024_as_daylight_time() {
        let timezone = TimeZoneSetting::parse("PST8PDT");
        let march_2024 = PrimitiveDateTime::new(
            Date::from_calendar_date(2024, Month::March, 15).unwrap(),
            Time::from_hms(12, 0, 0).unwrap(),
        );
        assert_eq!(timezone.label_for_local(march_2024), "PDT");
    }

    #[test]
    fn parts_for_utc_resolve_pacific_summer_offsets() {
        let timezone = TimeZoneSetting::parse("PST8PDT");
        let pacific_local = PrimitiveDateTime::new(
            Date::from_calendar_date(1998, Month::July, 10).unwrap(),
            Time::from_hms(14, 32, 1).unwrap(),
        );
        let july_1998_utc = PrimitiveDateTime::new(
            Date::from_calendar_date(1998, Month::July, 10).unwrap(),
            Time::from_hms(21, 32, 1).unwrap(),
        )
        .assume_utc();
        let september_2002_utc = PrimitiveDateTime::new(
            Date::from_calendar_date(2002, Month::September, 23).unwrap(),
            Time::from_hms(1, 19, 20).unwrap(),
        )
        .assume_utc();

        assert_eq!(timezone.label_for_local(pacific_local), "PDT");
        assert_eq!(
            timezone.parts_for_utc(july_1998_utc),
            (UtcOffset::from_hms(-7, 0, 0).unwrap(), "PDT".to_owned())
        );
        assert_eq!(
            timezone.parts_for_utc(september_2002_utc),
            (UtcOffset::from_hms(-7, 0, 0).unwrap(), "PDT".to_owned())
        );
    }

    #[test]
    fn named_new_york_zone_marks_mid_summer_as_daylight_time() {
        let timezone = TimeZoneSetting::parse("America/New_York");
        let july_1997 = PrimitiveDateTime::new(
            Date::from_calendar_date(1997, Month::July, 10).unwrap(),
            Time::from_hms(17, 32, 1).unwrap(),
        );
        assert_eq!(timezone.label_for_local(july_1997), "EDT");
        assert_eq!(
            timezone.offset_for_local(july_1997),
            UtcOffset::from_hms(-4, 0, 0).unwrap()
        );
    }

    #[test]
    fn parse_europe_paris() {
        let tz = TimeZoneSetting::try_parse("Europe/Paris").expect("should parse");
        // Winter: CET +1
        let winter = PrimitiveDateTime::new(
            Date::from_calendar_date(2024, Month::January, 15).unwrap(),
            Time::from_hms(12, 0, 0).unwrap(),
        );
        assert_eq!(tz.label_for_local(winter), "CET");
        assert_eq!(
            tz.offset_for_local(winter),
            UtcOffset::from_hms(1, 0, 0).unwrap()
        );
        // Summer: CEST +2
        let summer = PrimitiveDateTime::new(
            Date::from_calendar_date(2024, Month::July, 15).unwrap(),
            Time::from_hms(12, 0, 0).unwrap(),
        );
        assert_eq!(tz.label_for_local(summer), "CEST");
        assert_eq!(
            tz.offset_for_local(summer),
            UtcOffset::from_hms(2, 0, 0).unwrap()
        );
    }

    #[test]
    fn parse_asia_tokyo() {
        let tz = TimeZoneSetting::try_parse("Asia/Tokyo").expect("should parse");
        // JST +9, no DST
        let winter = PrimitiveDateTime::new(
            Date::from_calendar_date(2024, Month::January, 15).unwrap(),
            Time::from_hms(12, 0, 0).unwrap(),
        );
        let summer = PrimitiveDateTime::new(
            Date::from_calendar_date(2024, Month::July, 15).unwrap(),
            Time::from_hms(12, 0, 0).unwrap(),
        );
        assert_eq!(tz.label_for_local(winter), "JST");
        assert_eq!(tz.label_for_local(summer), "JST");
        assert_eq!(
            tz.offset_for_local(winter),
            UtcOffset::from_hms(9, 0, 0).unwrap()
        );
        assert_eq!(
            tz.offset_for_local(summer),
            UtcOffset::from_hms(9, 0, 0).unwrap()
        );
    }

    #[test]
    fn parse_australia_sydney() {
        let tz = TimeZoneSetting::try_parse("Australia/Sydney").expect("should parse");
        // Southern hemisphere: DST in January, standard in July
        let january = PrimitiveDateTime::new(
            Date::from_calendar_date(2024, Month::January, 15).unwrap(),
            Time::from_hms(12, 0, 0).unwrap(),
        );
        let july = PrimitiveDateTime::new(
            Date::from_calendar_date(2024, Month::July, 15).unwrap(),
            Time::from_hms(12, 0, 0).unwrap(),
        );
        assert_eq!(tz.label_for_local(january), "AEDT");
        assert_eq!(
            tz.offset_for_local(january),
            UtcOffset::from_hms(11, 0, 0).unwrap()
        );
        assert_eq!(tz.label_for_local(july), "AEST");
        assert_eq!(
            tz.offset_for_local(july),
            UtcOffset::from_hms(10, 0, 0).unwrap()
        );
    }

    #[test]
    fn parse_unknown_still_returns_none() {
        assert!(TimeZoneSetting::try_parse("Narnia/Wardrobe").is_none());
        assert!(TimeZoneSetting::try_parse("Fake/Zone").is_none());
    }

    #[test]
    fn parse_invalid_timezone_falls_back_to_explicit_utc() {
        let tz = TimeZoneSetting::parse("Narnia/Wardrobe");
        assert_eq!(tz.show_value(), "UTC");
        assert_eq!(
            tz.offset_for_date(Date::from_calendar_date(2024, Month::January, 1).unwrap()),
            UtcOffset::UTC
        );
    }

    #[test]
    fn eu_dst_transition_dates() {
        let (start, end) = eu_dst_transition_rules();
        // EU DST transitions: last Sunday of March and last Sunday of October.
        let start_date = transition_date(2024, start).unwrap();
        assert_eq!(start_date.month(), Month::March);
        assert_eq!(start_date.day(), 31);
        assert_eq!(start_date.weekday(), Weekday::Sunday);

        let end_date = transition_date(2024, end).unwrap();
        assert_eq!(end_date.month(), Month::October);
        assert_eq!(end_date.day(), 27);
        assert_eq!(end_date.weekday(), Weekday::Sunday);
    }

    #[test]
    fn half_hour_offset_asia_kolkata() {
        let tz = TimeZoneSetting::try_parse("Asia/Kolkata").expect("should parse");
        let dt = PrimitiveDateTime::new(
            Date::from_calendar_date(2024, Month::June, 1).unwrap(),
            Time::from_hms(12, 0, 0).unwrap(),
        );
        assert_eq!(tz.label_for_local(dt), "IST");
        assert_eq!(
            tz.offset_for_local(dt),
            UtcOffset::from_hms(5, 30, 0).unwrap()
        );
    }

    #[test]
    fn case_insensitive_named_lookup() {
        // Mixed case should still parse
        let tz = TimeZoneSetting::try_parse("europe/paris");
        assert!(tz.is_some());
        let tz = TimeZoneSetting::try_parse("EUROPE/PARIS");
        assert!(tz.is_some());
        let tz = TimeZoneSetting::try_parse("Europe/Paris");
        assert!(tz.is_some());
    }

    #[test]
    fn existing_us_aliases_still_work() {
        // These previously worked; make sure they still do.
        for name in &[
            "US/Pacific",
            "US/Mountain",
            "US/Central",
            "US/Eastern",
            "America/Los_Angeles",
            "America/Denver",
            "America/Chicago",
            "America/New_York",
        ] {
            assert!(
                TimeZoneSetting::try_parse(name).is_some(),
                "{name} should parse"
            );
        }
    }
}
