#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FieldKind {
    Year { width: u8, iso: bool, comma: bool },
    MonthNumber,
    MonthAbbr,
    MonthFull,
    MonthRoman,
    Day,
    DayOfWeekShort,
    DayOfWeekFull,
    GregorianWeek,
    GregorianDayOfWeek,
    GregorianDayOfYear,
    IsoWeek,
    IsoDayOfWeek,
    IsoDayOfYear,
    WeekOfMonth,
    QuarterIgnored,
    Century,
    JulianDay,
    Hour24,
    Hour12,
    Minute,
    Second,
    SecondsOfDay,
    Meridiem,
    BcAd,
    TzHour,
    TzMinute,
    TzName,
    Millisecond,
    FractionalSecond { precision: u8 },
    OrdinalSuffix,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FieldSpec {
    pub kind: FieldKind,
    pub fill_mode: bool,
    pub label: &'static str,
    pub token_len: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FormatItem {
    Field(FieldSpec),
    Separator(char),
    Literal(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompiledFormat {
    pub items: Vec<FormatItem>,
    pub exact_mode: bool,
    pub uses_gregorian: bool,
    pub uses_iso: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Meridiem {
    Am,
    Pm,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct ParsedFields {
    pub year_component: Option<i32>,
    pub year_component_width: Option<u8>,
    pub year_negative: bool,
    pub month: Option<u8>,
    pub day: Option<u8>,
    pub week_of_year: Option<u8>,
    pub weekday: Option<u8>,
    pub day_of_year: Option<u16>,
    pub iso_year: Option<i32>,
    pub iso_week: Option<u8>,
    pub iso_day: Option<u8>,
    pub iso_day_of_year: Option<u16>,
    pub week_of_month: Option<u8>,
    pub century: Option<i32>,
    pub julian_day: Option<i64>,
    pub hour: Option<u8>,
    pub hour_is_12: bool,
    pub minute: Option<u8>,
    pub second: Option<u8>,
    pub seconds_of_day: Option<u32>,
    pub microsecond: Option<u32>,
    pub meridiem: Option<Meridiem>,
    pub bc: bool,
    pub tz_sign: i8,
    pub tz_hour: Option<u8>,
    pub tz_minute: Option<u8>,
}
