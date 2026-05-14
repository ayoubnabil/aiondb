use super::*;
use crate::eval::pg_format::compile::compile_pg_format;

#[test]
fn space_and_separator_runs_do_not_consume_each_other() {
    let input = "0097/Feb/16 --> 08:14:30";
    let format = compile_pg_format("YYYY/Mon/DD --> HH:MI:SS");
    let fields = apply_format(input, &format).expect("fields");
    let (timestamp, _) = build_timestamp_components(&fields, input).expect("timestamp");

    assert_eq!(timestamp.year(), 97);
    assert_eq!(timestamp.month(), Month::February);
    assert_eq!(timestamp.day(), 16);
    assert_eq!(timestamp.hour(), 8);
    assert_eq!(timestamp.minute(), 14);
    assert_eq!(timestamp.second(), 30);
}

#[test]
fn unmatched_format_letters_behave_like_flexible_separators() {
    let input = "2011 12 18";
    let format = compile_pg_format("YYYYxMMxDD");
    let fields = apply_format(input, &format).expect("fields");
    let date = build_date(&fields, input).expect("date");

    assert_eq!(
        date,
        Date::from_calendar_date(2011, Month::December, 18).unwrap()
    );
}

#[test]
fn fractional_seconds_are_optional_when_ff_is_present() {
    let input = "2018-11-02 12:34:56";
    let format = compile_pg_format("YYYY-MM-DD HH24:MI:SS.FF6");
    let fields = apply_format(input, &format).expect("fields");
    let (timestamp, _) = build_timestamp_components(&fields, input).expect("timestamp");

    assert_eq!(
        timestamp,
        PrimitiveDateTime::new(
            Date::from_calendar_date(2018, Month::November, 2).unwrap(),
            Time::from_hms(12, 34, 56).unwrap(),
        )
    );
}

#[test]
fn fractional_seconds_round_half_up_for_ff4() {
    let input = "2018-11-02 12:34:56.12345";
    let format = compile_pg_format("YYYY-MM-DD HH24:MI:SS.FF4");
    let fields = apply_format(input, &format).expect("fields");
    let (timestamp, _) = build_timestamp_components(&fields, input).expect("timestamp");

    assert_eq!(
        timestamp,
        PrimitiveDateTime::new(
            Date::from_calendar_date(2018, Month::November, 2).unwrap(),
            Time::from_hms_micro(12, 34, 56, 123_500).unwrap(),
        )
    );
}

#[test]
fn fractional_seconds_round_carry_into_next_second() {
    let input = "2018-11-02 12:34:56.99995";
    let format = compile_pg_format("YYYY-MM-DD HH24:MI:SS.FF4");
    let fields = apply_format(input, &format).expect("fields");
    let (timestamp, _) = build_timestamp_components(&fields, input).expect("timestamp");

    assert_eq!(
        timestamp,
        PrimitiveDateTime::new(
            Date::from_calendar_date(2018, Month::November, 2).unwrap(),
            Time::from_hms(12, 34, 57).unwrap(),
        )
    );
}

#[test]
fn century_uses_raw_year_suffix_instead_of_inferred_century() {
    let input = "3 4 21 01";
    let format = compile_pg_format("W MM CC YY");
    let fields = apply_format(input, &format).expect("fields");
    let date = build_date(&fields, input).expect("date");

    assert_eq!(
        date,
        Date::from_calendar_date(2001, Month::April, 15).unwrap()
    );
}

#[test]
fn hh12_rejects_24_hour_values_even_without_meridiem() {
    let input = "2016-06-13 15:50:55";
    let format = compile_pg_format("YYYY-MM-DD HH:MI:SS");
    let fields = apply_format(input, &format).expect("fields");
    let error = build_timestamp_components(&fields, input).expect_err("invalid hour");

    assert_eq!(error, FormatError::InvalidHourFor12Clock(15));
}

#[test]
fn repeated_separator_slots_can_shift_timezone_sign_consumption() {
    let single = compile_pg_format("YYYY TZH");
    let fields = apply_format("2000 -10", &single).expect("single fields");
    let (_, offset) = build_timestamp_components(&fields, "2000 -10").expect("single ts");
    assert_eq!(offset.unwrap(), UtcOffset::from_hms(-10, 0, 0).unwrap());

    let doubled = compile_pg_format("YYYY  TZH");
    let fields = apply_format("2000 -10", &doubled).expect("double fields");
    let (_, offset) = build_timestamp_components(&fields, "2000 -10").expect("double ts");
    assert_eq!(offset.unwrap(), UtcOffset::from_hms(10, 0, 0).unwrap());
}

#[test]
fn repeated_separator_slots_preserve_internal_spaces_between_punctuation() {
    let accept = compile_pg_format("YYYY   MON");
    let fields = apply_format("2000 + + JUN", &accept).expect("fields");
    let date = build_date(&fields, "2000 + + JUN").expect("date");
    assert_eq!(
        date,
        Date::from_calendar_date(2000, Month::June, 1).unwrap()
    );

    let reject = compile_pg_format("YYYY  MON");
    let error = apply_format("2000 + + JUN", &reject).expect_err("invalid fields");
    assert!(matches!(
        error,
        FormatError::InvalidValue { field: "MON", .. }
    ));
}

#[test]
fn repeated_separator_slots_accept_adjacent_punctuation_runs() {
    let format = compile_pg_format("YYYY  MON");
    let fields = apply_format("2000 ++ JUN", &format).expect("fields");
    let date = build_date(&fields, "2000 ++ JUN").expect("date");
    assert_eq!(
        date,
        Date::from_calendar_date(2000, Month::June, 1).unwrap()
    );
}

#[test]
fn separator_letters_can_match_literal_or_whitespace_runs() {
    let literal = compile_pg_format("YYYYxMMxDD");
    let fields = apply_format("2011x 12x 18", &literal).expect("fields");
    let date = build_date(&fields, "2011x 12x 18").expect("date");
    assert_eq!(
        date,
        Date::from_calendar_date(2011, Month::December, 18).unwrap()
    );

    let spaced = apply_format("2011 12 18", &literal).expect("spaced fields");
    let date = build_date(&spaced, "2011 12 18").expect("spaced date");
    assert_eq!(
        date,
        Date::from_calendar_date(2011, Month::December, 18).unwrap()
    );
}

#[test]
fn separator_letters_prefer_consuming_whitespace_when_present() {
    let format = compile_pg_format("YYYYxMMxDD");
    let error = apply_format("2011 x12 x18", &format).expect_err("invalid fields");
    assert!(matches!(
        error,
        FormatError::InvalidValue { field: "MM", .. }
    ));
}

#[test]
fn exact_mode_punctuation_accepts_other_separator_chars() {
    let format = compile_pg_format("FXYY:Mon:DD");
    let fields = apply_format("97/Feb/16", &format).expect("fields");
    let date = build_date(&fields, "97/Feb/16").expect("date");
    assert_eq!(
        date,
        Date::from_calendar_date(1997, Month::February, 16).unwrap()
    );
}

#[test]
fn year_sign_after_separator_is_not_treated_as_part_of_year() {
    let format = compile_pg_format("DY DD MON YYYY");
    let fields = apply_format("Fri 1-Jan-1999", &format).expect("fields");
    let date = build_date(&fields, "Fri 1-Jan-1999").expect("date");
    assert_eq!(
        date,
        Date::from_calendar_date(1999, Month::January, 1).unwrap()
    );
}

#[test]
fn escaped_quote_literals_accept_double_backslash_quote_sequences() {
    let input = "15 \"text between quote marks\" 98 54 45";
    let format = compile_pg_format(r#"HH24 \\"text between quote marks\\" YY MI SS"#);
    let fields = apply_format(input, &format).expect("fields");
    let (timestamp, _) = build_timestamp_components(&fields, input).expect("timestamp");

    assert_eq!(
        timestamp,
        PrimitiveDateTime::new(
            Date::from_calendar_date(1998, Month::January, 1).unwrap(),
            Time::from_hms(15, 54, 45).unwrap(),
        )
    );
}

#[test]
fn escaped_quote_literals_accept_sql_path_terminating_quote_sequence() {
    let input = "15 \"text between quote marks\" 98 54 45";
    let format = compile_pg_format(r#"HH24 \"text between quote marks\"" YY MI SS"#);
    let fields = apply_format(input, &format).expect("fields");
    let (timestamp, _) = build_timestamp_components(&fields, input).expect("timestamp");

    assert_eq!(
        timestamp,
        PrimitiveDateTime::new(
            Date::from_calendar_date(1998, Month::January, 1).unwrap(),
            Time::from_hms(15, 54, 45).unwrap(),
        )
    );
}

#[test]
fn quoted_literals_accept_embedded_escaped_quotes() {
    let input = "15 \"text between quote marks\" 98 54 45";
    let format = compile_pg_format(r#"HH24 "\"text between quote marks\"" YY MI SS"#);
    let fields = apply_format(input, &format).expect("fields");
    let (timestamp, _) = build_timestamp_components(&fields, input).expect("timestamp");

    assert_eq!(
        timestamp,
        PrimitiveDateTime::new(
            Date::from_calendar_date(1998, Month::January, 1).unwrap(),
            Time::from_hms(15, 54, 45).unwrap(),
        )
    );
}

#[test]
fn uppercase_mon_token_keeps_uppercase_label_for_errors() {
    let format = compile_pg_format("YYYY  MON");
    let error = apply_format("2000 + + JUN", &format).expect_err("invalid fields");
    assert!(matches!(
        error,
        FormatError::InvalidValue { field: "MON", .. }
    ));
}

#[test]
fn year_zero_maps_to_first_bc_year() {
    let input = "0000-02-01";
    let format = compile_pg_format("YYYY-MM-DD");
    let fields = apply_format(input, &format).expect("fields");
    let date = build_date(&fields, input).expect("date");

    assert_eq!(date.year(), 0);
    assert_eq!(date.month(), Month::February);
    assert_eq!(date.day(), 1);
}
