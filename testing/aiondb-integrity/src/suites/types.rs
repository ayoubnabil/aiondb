use crate::harness::{SuiteResult, SuiteStats, TestDb};

pub fn run_basic() -> SuiteResult {
    super::run_scalar_battery(
        "",
        &[
            // Integer types
            ("int_literal", "SELECT 42", "42"),
            ("int_negative", "SELECT -1", "-1"),
            ("int_zero", "SELECT 0", "0"),
            ("bigint_literal", "SELECT 9999999999::bigint", "9999999999"),
            (
                "bigint_negative",
                "SELECT -9999999999::bigint",
                "-9999999999",
            ),
            // Boolean
            ("bool_true", "SELECT true", "t"),
            ("bool_false", "SELECT false", "f"),
            ("bool_not", "SELECT NOT true", "f"),
            ("bool_and", "SELECT true AND false", "f"),
            ("bool_or", "SELECT true OR false", "t"),
            // Text
            ("text_literal", "SELECT 'hello'", "hello"),
            ("text_empty", "SELECT ''", ""),
            ("text_concat", "SELECT 'a' || 'b'", "ab"),
            ("text_length", "SELECT length('hello')", "5"),
            // Float
            ("real_literal", "SELECT 3.14::real", "3.14"),
            ("double_literal", "SELECT 3.14::double precision", "3.14"),
            ("real_zero", "SELECT 0.0::real", "0"),
            // Numeric
            ("numeric_literal", "SELECT 123.456::numeric", "123.456"),
            ("numeric_integer", "SELECT 100::numeric", "100"),
            // Null
            ("null_literal", "SELECT NULL", "NULL"),
            ("null_cast_int", "SELECT NULL::integer", "NULL"),
            ("null_cast_text", "SELECT NULL::text", "NULL"),
        ],
    )
}

pub fn run_coercion() -> SuiteResult {
    super::run_scalar_battery(
        "",
        &[
            // int -> bigint
            ("int_to_bigint", "SELECT 42::bigint", "42"),
            // int -> numeric
            ("int_to_numeric", "SELECT 42::numeric", "42"),
            // int -> double
            ("int_to_double", "SELECT 42::double precision", "42"),
            // int -> text
            ("int_to_text", "SELECT 42::text", "42"),
            // bool -> text
            ("bool_to_text", "SELECT true::text", "true"),
            // text -> int
            ("text_to_int", "SELECT '123'::integer", "123"),
            // text -> bigint
            ("text_to_bigint", "SELECT '123'::bigint", "123"),
            // int arithmetic promotion
            (
                "int_plus_bigint",
                "SELECT 1 + 9999999999::bigint",
                "10000000000",
            ),
            // numeric vs int
            ("numeric_plus_int", "SELECT 1.5::numeric + 2", "3.5"),
        ],
    )
}

pub fn run_boundaries() -> SuiteResult {
    let db = TestDb::new();
    let conn = db.conn();
    let mut passed = 0_usize;
    let mut failures = Vec::new();

    let boundary_tests: Vec<(&str, &str, Result<&str, ()>)> = vec![
        // INT boundaries
        ("int_max", "SELECT 2147483647", Ok("2147483647")),
        ("int_min", "SELECT -2147483648", Ok("-2147483648")),
        // BIGINT boundaries
        (
            "bigint_max",
            "SELECT 9223372036854775807::bigint",
            Ok("9223372036854775807"),
        ),
        (
            "bigint_min",
            "SELECT (-9223372036854775807 - 1)::bigint",
            Ok("-9223372036854775808"),
        ),
        // INT overflow - AionDB promotes to bigint
        ("int_overflow", "SELECT 2147483647 + 1", Ok("2147483648")),
        ("int_underflow", "SELECT -2147483648 - 1", Ok("-2147483649")),
        // BIGINT overflow should error
        (
            "bigint_overflow",
            "SELECT 9223372036854775807::bigint + 1",
            Err(()),
        ),
        // Division by zero
        ("div_by_zero_int", "SELECT 1 / 0", Err(())),
        ("div_by_zero_double", "SELECT 1.0 / 0.0", Err(())),
        // Empty string cast to int should error
        ("empty_str_to_int", "SELECT ''::integer", Err(())),
        // Non-numeric string to int should error
        ("bad_str_to_int", "SELECT 'abc'::integer", Err(())),
    ];

    for (name, sql, expected) in &boundary_tests {
        match (conn.execute(sql), expected) {
            (Ok(results), Ok(expected_val)) => {
                let got = extract_scalar(&results);
                if got.as_deref() == Some(*expected_val) {
                    passed += 1;
                } else {
                    failures.push(format!(
                        "{name}: expected '{expected_val}', got '{}'",
                        got.unwrap_or_else(|| "NO_RESULT".to_owned())
                    ));
                }
            }
            (Err(_), Err(())) => passed += 1,
            (Ok(_), Err(())) => {
                failures.push(format!("{name}: expected error but got success"));
            }
            (Err(e), Ok(expected_val)) => {
                failures.push(format!("{name}: expected '{expected_val}', got error: {e}"));
            }
        }
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

fn extract_scalar(results: &[aiondb_engine::StatementResult]) -> Option<String> {
    use aiondb_engine::StatementResult;
    for r in results {
        if let StatementResult::Query { rows, .. } = r {
            if let Some(row) = rows.first() {
                if let Some(val) = row.values.first() {
                    return Some(crate::harness::engine::value_to_string(val));
                }
            }
        }
    }
    None
}
