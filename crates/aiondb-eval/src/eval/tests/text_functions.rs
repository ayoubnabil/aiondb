use super::*;
use aiondb_plan::ScalarFunction;

// =====================================================================
// Helper to build scalar function expressions
// =====================================================================

fn sfn(func: ScalarFunction, args: Vec<TypedExpr>, dt: DataType) -> TypedExpr {
    TypedExpr::scalar_function(func, args, dt, false)
}

fn sfn_nullable(func: ScalarFunction, args: Vec<TypedExpr>, dt: DataType) -> TypedExpr {
    TypedExpr::scalar_function(func, args, dt, true)
}

// =====================================================================
// UPPER
// =====================================================================

#[test]
fn upper_normal() {
    let expr = sfn(
        ScalarFunction::Upper,
        vec![lit_text("hello")],
        DataType::Text,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text("HELLO".into()));
}

#[test]
fn upper_null() {
    let expr = sfn_nullable(ScalarFunction::Upper, vec![lit_null()], DataType::Text);
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn upper_empty() {
    let expr = sfn(ScalarFunction::Upper, vec![lit_text("")], DataType::Text);
    assert_eq!(eval(&expr).unwrap(), Value::Text(String::new()));
}

#[test]
fn upper_unicode() {
    let expr = sfn(
        ScalarFunction::Upper,
        vec![lit_text("cafe\u{0301}")],
        DataType::Text,
    );
    // upper on cafe + combining accent
    let result = eval(&expr).unwrap();
    assert!(matches!(result, Value::Text(_)));
}

// =====================================================================
// LOWER
// =====================================================================

#[test]
fn lower_normal() {
    let expr = sfn(
        ScalarFunction::Lower,
        vec![lit_text("HELLO")],
        DataType::Text,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text("hello".into()));
}

#[test]
fn lower_null() {
    let expr = sfn_nullable(ScalarFunction::Lower, vec![lit_null()], DataType::Text);
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn lower_mixed_case() {
    let expr = sfn(
        ScalarFunction::Lower,
        vec![lit_text("HeLLo WoRLd")],
        DataType::Text,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text("hello world".into()));
}

// =====================================================================
// LENGTH / CHAR_LENGTH
// =====================================================================

#[test]
fn length_normal() {
    let expr = sfn(
        ScalarFunction::Length,
        vec![lit_text("hello")],
        DataType::Int,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Int(5));
}

#[test]
fn length_null() {
    let expr = sfn_nullable(ScalarFunction::Length, vec![lit_null()], DataType::Int);
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn length_empty() {
    let expr = sfn(ScalarFunction::Length, vec![lit_text("")], DataType::Int);
    assert_eq!(eval(&expr).unwrap(), Value::Int(0));
}

#[test]
fn length_unicode() {
    // "caf\u{00e9}" is 4 characters, 5 bytes
    let expr = sfn(
        ScalarFunction::Length,
        vec![lit_text("caf\u{00e9}")],
        DataType::Int,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Int(4));
}

#[test]
fn char_length_same_as_length() {
    let expr = sfn(
        ScalarFunction::CharLength,
        vec![lit_text("hello")],
        DataType::Int,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Int(5));
}

// =====================================================================
// OCTET_LENGTH
// =====================================================================

#[test]
fn octet_length_ascii() {
    let expr = sfn(
        ScalarFunction::OctetLength,
        vec![lit_text("hello")],
        DataType::Int,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Int(5));
}

#[test]
fn octet_length_null() {
    let expr = sfn_nullable(ScalarFunction::OctetLength, vec![lit_null()], DataType::Int);
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn octet_length_unicode() {
    // "caf\u{00e9}" is 5 bytes in UTF-8
    let expr = sfn(
        ScalarFunction::OctetLength,
        vec![lit_text("caf\u{00e9}")],
        DataType::Int,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Int(5));
}

// =====================================================================
// SUBSTRING
// =====================================================================

#[test]
fn substring_two_args() {
    // substring('hello', 2) -> 'ello' (1-based)
    let expr = sfn(
        ScalarFunction::Substring,
        vec![lit_text("hello"), lit_int(2)],
        DataType::Text,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text("ello".into()));
}

#[test]
fn substring_three_args() {
    // substring('hello world', 7, 5) -> 'world'
    let expr = sfn(
        ScalarFunction::Substring,
        vec![lit_text("hello world"), lit_int(7), lit_int(5)],
        DataType::Text,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text("world".into()));
}

#[test]
fn substring_null() {
    let expr = sfn_nullable(
        ScalarFunction::Substring,
        vec![lit_null(), lit_int(1)],
        DataType::Text,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn substring_start_before_one() {
    // PostgreSQL: substring('hello', 0, 3) -> 'he' (start < 1 reduces length)
    let expr = sfn(
        ScalarFunction::Substring,
        vec![lit_text("hello"), lit_int(0), lit_int(3)],
        DataType::Text,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text("he".into()));
}

#[test]
fn substring_beyond_end() {
    let expr = sfn(
        ScalarFunction::Substring,
        vec![lit_text("hello"), lit_int(3), lit_int(100)],
        DataType::Text,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text("llo".into()));
}

#[test]
fn substring_regex_returns_first_capture_group() {
    let expr = sfn(
        ScalarFunction::Substring,
        vec![lit_text("(2,10)"), lit_text(r",(\d+)\)")],
        DataType::Text,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text("10".into()));
}

#[test]
fn substring_regex_without_capture_returns_full_match() {
    let expr = sfn(
        ScalarFunction::Substring,
        vec![lit_text("(2,10)"), lit_text(r"\(\d+,\d+\)")],
        DataType::Text,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text("(2,10)".into()));
}

#[test]
fn substring_extreme_negative_start_does_not_overflow() {
    let expr = sfn(
        ScalarFunction::Substring,
        vec![lit_text("hello"), lit_int(i32::MIN), lit_int(1)],
        DataType::Text,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text(String::new()));
}

#[test]
fn substring_extreme_bigint_bounds_do_not_overflow() {
    let expr = sfn(
        ScalarFunction::Substring,
        vec![
            lit_text("hello"),
            TypedExpr::literal(Value::BigInt(i64::MIN), DataType::BigInt, false),
            TypedExpr::literal(Value::BigInt(i64::MAX), DataType::BigInt, false),
        ],
        DataType::Text,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text(String::new()));
}

// =====================================================================
// TRIM / LTRIM / RTRIM
// =====================================================================

#[test]
fn trim_normal() {
    let expr = sfn(
        ScalarFunction::Trim,
        vec![lit_text("  hello  ")],
        DataType::Text,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text("hello".into()));
}

#[test]
fn trim_null() {
    let expr = sfn_nullable(ScalarFunction::Trim, vec![lit_null()], DataType::Text);
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn trim_no_whitespace() {
    let expr = sfn(
        ScalarFunction::Trim,
        vec![lit_text("hello")],
        DataType::Text,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text("hello".into()));
}

#[test]
fn ltrim_normal() {
    let expr = sfn(
        ScalarFunction::Ltrim,
        vec![lit_text("  hello  ")],
        DataType::Text,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text("hello  ".into()));
}

#[test]
fn ltrim_null() {
    let expr = sfn_nullable(ScalarFunction::Ltrim, vec![lit_null()], DataType::Text);
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn ltrim_empty() {
    let expr = sfn(ScalarFunction::Ltrim, vec![lit_text("")], DataType::Text);
    assert_eq!(eval(&expr).unwrap(), Value::Text(String::new()));
}

#[test]
fn rtrim_normal() {
    let expr = sfn(
        ScalarFunction::Rtrim,
        vec![lit_text("  hello  ")],
        DataType::Text,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text("  hello".into()));
}

#[test]
fn rtrim_null() {
    let expr = sfn_nullable(ScalarFunction::Rtrim, vec![lit_null()], DataType::Text);
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn rtrim_only_whitespace() {
    let expr = sfn(ScalarFunction::Rtrim, vec![lit_text("   ")], DataType::Text);
    assert_eq!(eval(&expr).unwrap(), Value::Text(String::new()));
}

// =====================================================================
// REPLACE
// =====================================================================

#[test]
fn replace_normal() {
    let expr = sfn(
        ScalarFunction::Replace,
        vec![lit_text("hello world"), lit_text("world"), lit_text("rust")],
        DataType::Text,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text("hello rust".into()));
}

#[test]
fn replace_null() {
    let expr = sfn_nullable(
        ScalarFunction::Replace,
        vec![lit_null(), lit_text("a"), lit_text("b")],
        DataType::Text,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn replace_no_match() {
    let expr = sfn(
        ScalarFunction::Replace,
        vec![lit_text("hello"), lit_text("xyz"), lit_text("abc")],
        DataType::Text,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text("hello".into()));
}

#[test]
fn replace_multiple_occurrences() {
    let expr = sfn(
        ScalarFunction::Replace,
        vec![lit_text("aaa"), lit_text("a"), lit_text("bb")],
        DataType::Text,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text("bbbbbb".into()));
}

// =====================================================================
// STRPOS
// =====================================================================

#[test]
fn strpos_found() {
    let expr = sfn(
        ScalarFunction::Strpos,
        vec![lit_text("hello world"), lit_text("world")],
        DataType::Int,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Int(7));
}

#[test]
fn strpos_not_found() {
    let expr = sfn(
        ScalarFunction::Strpos,
        vec![lit_text("hello"), lit_text("xyz")],
        DataType::Int,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Int(0));
}

#[test]
fn strpos_null() {
    let expr = sfn_nullable(
        ScalarFunction::Strpos,
        vec![lit_null(), lit_text("a")],
        DataType::Int,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn strpos_at_start() {
    let expr = sfn(
        ScalarFunction::Strpos,
        vec![lit_text("hello"), lit_text("h")],
        DataType::Int,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Int(1));
}

// =====================================================================
// LEFT / RIGHT
// =====================================================================

#[test]
fn left_normal() {
    let expr = sfn(
        ScalarFunction::Left,
        vec![lit_text("hello"), lit_int(3)],
        DataType::Text,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text("hel".into()));
}

#[test]
fn left_null() {
    let expr = sfn_nullable(
        ScalarFunction::Left,
        vec![lit_null(), lit_int(3)],
        DataType::Text,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn left_negative() {
    // left('hello', -2) -> 'hel' (all but last 2)
    let expr = sfn(
        ScalarFunction::Left,
        vec![lit_text("hello"), lit_int(-2)],
        DataType::Text,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text("hel".into()));
}

#[test]
fn right_normal() {
    let expr = sfn(
        ScalarFunction::Right,
        vec![lit_text("hello"), lit_int(3)],
        DataType::Text,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text("llo".into()));
}

#[test]
fn right_null() {
    let expr = sfn_nullable(
        ScalarFunction::Right,
        vec![lit_null(), lit_int(3)],
        DataType::Text,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn right_negative() {
    // right('hello', -2) -> 'llo' (all but first 2)
    let expr = sfn(
        ScalarFunction::Right,
        vec![lit_text("hello"), lit_int(-2)],
        DataType::Text,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text("llo".into()));
}

#[test]
fn left_bigint_out_of_i32_range_errors() {
    let expr = sfn(
        ScalarFunction::Left,
        vec![
            lit_text("hello"),
            TypedExpr::literal(Value::BigInt(i64::MAX), DataType::BigInt, false),
        ],
        DataType::Text,
    );
    assert!(eval(&expr).is_err());
}

// =====================================================================
// REPEAT
// =====================================================================

#[test]
fn repeat_normal() {
    let expr = sfn(
        ScalarFunction::Repeat,
        vec![lit_text("ab"), lit_int(3)],
        DataType::Text,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text("ababab".into()));
}

#[test]
fn repeat_null() {
    let expr = sfn_nullable(
        ScalarFunction::Repeat,
        vec![lit_null(), lit_int(3)],
        DataType::Text,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn repeat_zero() {
    let expr = sfn(
        ScalarFunction::Repeat,
        vec![lit_text("ab"), lit_int(0)],
        DataType::Text,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text(String::new()));
}

#[test]
fn repeat_negative() {
    let expr = sfn(
        ScalarFunction::Repeat,
        vec![lit_text("ab"), lit_int(-1)],
        DataType::Text,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text(String::new()));
}

// =====================================================================
// REVERSE
// =====================================================================

#[test]
fn reverse_normal() {
    let expr = sfn(
        ScalarFunction::Reverse,
        vec![lit_text("hello")],
        DataType::Text,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text("olleh".into()));
}

#[test]
fn reverse_null() {
    let expr = sfn_nullable(ScalarFunction::Reverse, vec![lit_null()], DataType::Text);
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn reverse_empty() {
    let expr = sfn(ScalarFunction::Reverse, vec![lit_text("")], DataType::Text);
    assert_eq!(eval(&expr).unwrap(), Value::Text(String::new()));
}

// =====================================================================
// STARTS_WITH
// =====================================================================

#[test]
fn starts_with_true() {
    let expr = sfn(
        ScalarFunction::StartsWith,
        vec![lit_text("hello world"), lit_text("hello")],
        DataType::Boolean,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

#[test]
fn starts_with_false() {
    let expr = sfn(
        ScalarFunction::StartsWith,
        vec![lit_text("hello world"), lit_text("world")],
        DataType::Boolean,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(false));
}

#[test]
fn starts_with_null() {
    let expr = sfn_nullable(
        ScalarFunction::StartsWith,
        vec![lit_null(), lit_text("hello")],
        DataType::Boolean,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Null);
}

#[test]
fn starts_with_empty_prefix() {
    let expr = sfn(
        ScalarFunction::StartsWith,
        vec![lit_text("hello"), lit_text("")],
        DataType::Boolean,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Boolean(true));
}

// =====================================================================
// CONCAT (function form)
// =====================================================================

#[test]
fn concat_func_normal() {
    let expr = sfn(
        ScalarFunction::ConcatFunc,
        vec![lit_text("hello"), lit_text(" "), lit_text("world")],
        DataType::Text,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text("hello world".into()));
}

#[test]
fn concat_func_skips_null() {
    let expr = sfn(
        ScalarFunction::ConcatFunc,
        vec![lit_text("a"), lit_null(), lit_text("b")],
        DataType::Text,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text("ab".into()));
}

#[test]
fn concat_func_all_null() {
    let expr = sfn(
        ScalarFunction::ConcatFunc,
        vec![lit_null(), lit_null()],
        DataType::Text,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text(String::new()));
}

#[test]
fn concat_func_mixed_types() {
    let expr = sfn(
        ScalarFunction::ConcatFunc,
        vec![lit_text("count="), lit_int(42)],
        DataType::Text,
    );
    assert_eq!(eval(&expr).unwrap(), Value::Text("count=42".into()));
}

#[test]
fn concat_func_no_args() {
    let expr = sfn(ScalarFunction::ConcatFunc, vec![], DataType::Text);
    assert_eq!(eval(&expr).unwrap(), Value::Text(String::new()));
}
