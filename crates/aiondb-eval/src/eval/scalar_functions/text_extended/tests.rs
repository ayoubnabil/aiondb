use super::*;
use time::{Date, Month, Time};

// ---- initcap ----
#[test]
fn test_initcap_basic() {
    let args = [Value::Text("hello world".into())];
    let result = eval_initcap(&args).unwrap();
    assert_eq!(result, Value::Text("Hello World".into()));
}

#[test]
fn test_initcap_mixed() {
    let args = [Value::Text("hELLO wORLD".into())];
    let result = eval_initcap(&args).unwrap();
    assert_eq!(result, Value::Text("Hello World".into()));
}

#[test]
fn test_initcap_null() {
    let args = [Value::Null];
    let result = eval_initcap(&args).unwrap();
    assert_eq!(result, Value::Null);
}

// ---- split_part ----
#[test]
fn test_split_part_basic() {
    let args = [
        Value::Text("abc~@~def~@~ghi".into()),
        Value::Text("~@~".into()),
        Value::Int(2),
    ];
    let result = eval_split_part(&args).unwrap();
    assert_eq!(result, Value::Text("def".into()));
}

#[test]
fn test_split_part_out_of_range() {
    let args = [
        Value::Text("a.b.c".into()),
        Value::Text(".".into()),
        Value::Int(5),
    ];
    let result = eval_split_part(&args).unwrap();
    assert_eq!(result, Value::Text(String::new()));
}

#[test]
fn test_split_part_first() {
    let args = [
        Value::Text("a,b,c".into()),
        Value::Text(",".into()),
        Value::Int(1),
    ];
    let result = eval_split_part(&args).unwrap();
    assert_eq!(result, Value::Text("a".into()));
}

// ---- translate ----
#[test]
fn test_translate_basic() {
    let args = [
        Value::Text("12345".into()),
        Value::Text("143".into()),
        Value::Text("ax".into()),
    ];
    let result = eval_translate(&args).unwrap();
    // '1'->pos 0->'a', '2'->not found->'2', '3'->pos 2->deleted,
    // '4'->pos 1->'x', '5'->not found->'5'
    assert_eq!(result, Value::Text("a2x5".into()));
}

#[test]
fn test_translate_delete() {
    let args = [
        Value::Text("hello".into()),
        Value::Text("lo".into()),
        Value::Text(String::new()),
    ];
    let result = eval_translate(&args).unwrap();
    assert_eq!(result, Value::Text("he".into()));
}

// ---- overlay ----
#[test]
fn test_overlay_basic() {
    let args = [
        Value::Text("Txxxxas".into()),
        Value::Text("hom".into()),
        Value::Int(2),
        Value::Int(4),
    ];
    let result = eval_overlay(&args).unwrap();
    assert_eq!(result, Value::Text("Thomas".into()));
}

#[test]
fn test_overlay_default_count() {
    let args = [
        Value::Text("abcdef".into()),
        Value::Text("XY".into()),
        Value::Int(3),
    ];
    let result = eval_overlay(&args).unwrap();
    // overlay('abcdef' placing 'XY' from 3) => 'ab' + 'XY' + 'ef'
    assert_eq!(result, Value::Text("abXYef".into()));
}

// ---- bit_length ----
#[test]
fn test_bit_length_ascii() {
    let args = [Value::Text("hello".into())];
    let result = eval_bit_length(&args).unwrap();
    assert_eq!(result, Value::Int(40));
}

#[test]
fn test_bit_length_empty() {
    let args = [Value::Text(String::new())];
    let result = eval_bit_length(&args).unwrap();
    assert_eq!(result, Value::Int(0));
}

// ---- chr ----
#[test]
fn test_chr_basic() {
    let args = [Value::Int(65)];
    let result = eval_chr(&args).unwrap();
    assert_eq!(result, Value::Text("A".into()));
}

#[test]
fn test_chr_zero_error() {
    let args = [Value::Int(0)];
    let result = eval_chr(&args);
    assert!(result.is_err());
}

// ---- ascii ----
#[test]
fn test_ascii_basic() {
    let args = [Value::Text("A".into())];
    let result = eval_ascii(&args).unwrap();
    assert_eq!(result, Value::Int(65));
}

#[test]
fn test_ascii_empty() {
    let args = [Value::Text(String::new())];
    let result = eval_ascii(&args).unwrap();
    assert_eq!(result, Value::Int(0));
}

// ---- md5 ----
#[test]
fn test_md5_empty() {
    let args = [Value::Text(String::new())];
    let result = eval_md5(&args).unwrap();
    assert_eq!(
        result,
        Value::Text("d41d8cd98f00b204e9800998ecf8427e".into())
    );
}

#[test]
fn test_md5_hello() {
    let args = [Value::Text("hello".into())];
    let result = eval_md5(&args).unwrap();
    assert_eq!(
        result,
        Value::Text("5d41402abc4b2a76b9719d911017c592".into())
    );
}

#[test]
fn test_md5_abc() {
    let args = [Value::Text("abc".into())];
    let result = eval_md5(&args).unwrap();
    assert_eq!(
        result,
        Value::Text("900150983cd24fb0d6963f7d28e17f72".into())
    );
}

// ---- quote_literal ----
#[test]
fn test_quote_literal_basic() {
    let args = [Value::Text("hello".into())];
    let result = eval_quote_literal(&args).unwrap();
    assert_eq!(result, Value::Text("'hello'".into()));
}

#[test]
fn test_quote_literal_with_quote() {
    let args = [Value::Text("it's".into())];
    let result = eval_quote_literal(&args).unwrap();
    assert_eq!(result, Value::Text("'it''s'".into()));
}

#[test]
fn test_quote_literal_null() {
    let args = [Value::Null];
    let result = eval_quote_literal(&args).unwrap();
    assert_eq!(result, Value::Null);
}

#[test]
fn test_quote_literal_with_backslash_uses_escape_syntax() {
    let args = [Value::Text("\\".into())];
    let result = eval_quote_literal(&args).unwrap();
    assert_eq!(result, Value::Text("E'\\\\'".into()));
}

// ---- quote_ident ----
#[test]
fn test_quote_ident_basic() {
    let args = [Value::Text("my_col".into())];
    let result = eval_quote_ident(&args).unwrap();
    // PG does not quote simple lowercase identifiers
    assert_eq!(result, Value::Text("my_col".into()));
}

#[test]
fn test_composite_field_extracts_cypher_date_property() {
    let date = Date::from_calendar_date(2026, Month::March, 18).unwrap();
    let result = eval_composite_field(&[Value::Date(date), Value::Text("year".into())]).unwrap();
    assert_eq!(result, Value::BigInt(2026));
}

#[test]
fn test_composite_field_extracts_cypher_time_property() {
    let time = Time::from_hms_micro(12, 34, 56, 789_000).unwrap();
    let result =
        eval_composite_field(&[Value::Time(time), Value::Text("millisecond".into())]).unwrap();
    assert_eq!(result, Value::BigInt(789));
}

#[test]
fn test_composite_field_extracts_jsonb_each_key_alias() {
    let result = eval_composite_field(&[
        Value::Text("(line,\"{\\\"f\\\": 1}\")".into()),
        Value::Text("key".into()),
    ])
    .unwrap();
    assert_eq!(result, Value::Text("line".into()));
}

#[test]
fn test_composite_field_extracts_jsonb_each_value_alias() {
    let result = eval_composite_field(&[
        Value::Text("(line,\"{\\\"f\\\": 1}\")".into()),
        Value::Text("value".into()),
    ])
    .unwrap();
    assert_eq!(result, Value::Text("{\"f\": 1}".into()));
}

#[test]
fn test_quote_ident_with_dquote() {
    let args = [Value::Text("a\"b".into())];
    let result = eval_quote_ident(&args).unwrap();
    assert_eq!(result, Value::Text("\"a\"\"b\"".into()));
}

// ---- quote_nullable ----
#[test]
fn test_quote_nullable_null() {
    let args = [Value::Null];
    let result = eval_quote_nullable(&args).unwrap();
    assert_eq!(result, Value::Text("NULL".into()));
}

#[test]
fn test_quote_nullable_text() {
    let args = [Value::Text("hello".into())];
    let result = eval_quote_nullable(&args).unwrap();
    assert_eq!(result, Value::Text("'hello'".into()));
}

#[test]
fn test_quote_nullable_with_quote() {
    let args = [Value::Text("it's".into())];
    let result = eval_quote_nullable(&args).unwrap();
    assert_eq!(result, Value::Text("'it''s'".into()));
}

// ---- to_hex ----
#[test]
fn test_to_hex_int() {
    let args = [Value::Int(255)];
    let result = eval_to_hex(&args).unwrap();
    assert_eq!(result, Value::Text("ff".into()));
}

#[test]
fn test_to_hex_bigint() {
    let args = [Value::BigInt(4_294_967_295)];
    let result = eval_to_hex(&args).unwrap();
    assert_eq!(result, Value::Text("ffffffff".into()));
}

#[test]
fn test_to_hex_zero() {
    let args = [Value::Int(0)];
    let result = eval_to_hex(&args).unwrap();
    assert_eq!(result, Value::Text("0".into()));
}

// ---- regexp_replace ----
#[test]
fn test_regexp_replace_first() {
    let args = [
        Value::Text("foobarbaz".into()),
        Value::Text("b..".into()),
        Value::Text("X".into()),
    ];
    let result = eval_regexp_replace(&args).unwrap();
    assert_eq!(result, Value::Text("fooXbaz".into()));
}

#[test]
fn test_regexp_replace_global() {
    let args = [
        Value::Text("foobarbaz".into()),
        Value::Text("b..".into()),
        Value::Text("X".into()),
        Value::Text("g".into()),
    ];
    let result = eval_regexp_replace(&args).unwrap();
    assert_eq!(result, Value::Text("fooXX".into()));
}

#[test]
fn test_regexp_replace_case_insensitive() {
    let args = [
        Value::Text("Hello World".into()),
        Value::Text("hello".into()),
        Value::Text("hi".into()),
        Value::Text("i".into()),
    ];
    let result = eval_regexp_replace(&args).unwrap();
    assert_eq!(result, Value::Text("hi World".into()));
}

#[test]
fn test_regexp_replace_with_backref() {
    let args = [
        Value::Text("abc 123".into()),
        Value::Text("([a-z]+) ([0-9]+)".into()),
        Value::Text("\\2 \\1".into()),
    ];
    let result = eval_regexp_replace(&args).unwrap();
    assert_eq!(result, Value::Text("123 abc".into()));
}

// ---- regexp_match ----
#[test]
fn test_regexp_match_full_match() {
    let args = [Value::Text("foobarbaz".into()), Value::Text("bar".into())];
    let result = eval_regexp_match(&args).unwrap();
    // PG returns text[] - no capture groups means single-element array with full match
    assert_eq!(result, Value::Array(vec![Value::Text("bar".into())]));
}

#[test]
fn test_regexp_match_capture_group() {
    let args = [
        Value::Text("foobarbaz".into()),
        Value::Text("foo(b..)baz".into()),
    ];
    let result = eval_regexp_match(&args).unwrap();
    // PG returns text[] of captured groups
    assert_eq!(result, Value::Array(vec![Value::Text("bar".into())]));
}

#[test]
fn test_regexp_match_no_match() {
    let args = [Value::Text("hello".into()), Value::Text("xyz".into())];
    let result = eval_regexp_match(&args).unwrap();
    assert_eq!(result, Value::Null);
}

#[test]
fn test_regexp_match_case_insensitive() {
    let args = [
        Value::Text("Hello World".into()),
        Value::Text("hello".into()),
        Value::Text("i".into()),
    ];
    let result = eval_regexp_match(&args).unwrap();
    // PG returns text[] - full match as single-element array
    assert_eq!(result, Value::Array(vec![Value::Text("Hello".into())]));
}

// ---- encode ----
#[test]
fn test_encode_hex() {
    let args = [
        Value::Blob(vec![0xDE, 0xAD, 0xBE, 0xEF]),
        Value::Text("hex".into()),
    ];
    let result = eval_encode(&args).unwrap();
    assert_eq!(result, Value::Text("deadbeef".into()));
}

#[test]
fn test_encode_base64() {
    let args = [Value::Blob(b"hello".to_vec()), Value::Text("base64".into())];
    let result = eval_encode(&args).unwrap();
    assert_eq!(result, Value::Text("aGVsbG8=".into()));
}

#[test]
fn test_encode_text_input() {
    let args = [Value::Text("hello".into()), Value::Text("hex".into())];
    let result = eval_encode(&args).unwrap();
    assert_eq!(result, Value::Text("68656c6c6f".into()));
}

// ---- decode ----
#[test]
fn test_decode_hex() {
    let args = [Value::Text("deadbeef".into()), Value::Text("hex".into())];
    let result = eval_decode(&args).unwrap();
    assert_eq!(result, Value::Blob(vec![0xDE, 0xAD, 0xBE, 0xEF]));
}

#[test]
fn test_decode_base64() {
    let args = [Value::Text("aGVsbG8=".into()), Value::Text("base64".into())];
    let result = eval_decode(&args).unwrap();
    assert_eq!(result, Value::Blob(b"hello".to_vec()));
}

#[test]
fn test_decode_invalid_hex() {
    let args = [Value::Text("xyz".into()), Value::Text("hex".into())];
    let result = eval_decode(&args);
    assert!(result.is_err());
}

#[test]
fn audit_decode_hex_panics_on_multibyte_utf8() {
    // "한X" is 4 bytes: 3 for '한' + 1 for 'X'. Byte index 2 falls inside '한'.
    let args = [Value::Text("한X".into()), Value::Text("hex".into())];
    let outcome = std::panic::catch_unwind(|| eval_decode(&args));
    assert!(
        outcome.is_ok(),
        "decode(hex) panicked on multibyte input (byte-index slicing bug)",
    );
}

#[test]
fn audit_decode_escape_panics_on_large_digits() {
    let args = [Value::Text(r"\999".into()), Value::Text("escape".into())];
    let outcome = std::panic::catch_unwind(|| eval_decode(&args));
    assert!(
        outcome.is_ok(),
        "decode(escape) panicked on \\999 (u8 overflow in digit decode)",
    );
}
