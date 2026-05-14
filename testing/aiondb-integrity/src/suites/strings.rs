use crate::harness::SuiteResult;

pub fn run_all() -> SuiteResult {
    super::run_scalar_battery(
        "",
        &[
            // Basic ops
            ("empty_string", "SELECT ''", ""),
            ("single_char", "SELECT 'x'", "x"),
            ("long_string", "SELECT repeat('a', 1000)", &"a".repeat(1000)),
            ("concat_empty", "SELECT '' || 'x'", "x"),
            ("concat_both_empty", "SELECT '' || ''", ""),
            // Length
            ("length_empty", "SELECT length('')", "0"),
            ("length_ascii", "SELECT length('hello')", "5"),
            // Case
            ("upper", "SELECT upper('hello world')", "HELLO WORLD"),
            ("lower", "SELECT lower('HELLO WORLD')", "hello world"),
            ("initcap", "SELECT initcap('hello world')", "Hello World"),
            // Substring
            ("substr_from", "SELECT substring('hello' FROM 2)", "ello"),
            (
                "substr_from_for",
                "SELECT substring('hello' FROM 2 FOR 3)",
                "ell",
            ),
            (
                "substr_beyond",
                "SELECT substring('hi' FROM 1 FOR 100)",
                "hi",
            ),
            // Trim
            ("trim_both", "SELECT trim('  hello  ')", "hello"),
            (
                "trim_leading",
                "SELECT ltrim('  hello  ') || '|'",
                "hello  |",
            ),
            (
                "trim_trailing",
                "SELECT '|' || rtrim('  hello  ') || '|'",
                "|  hello|",
            ),
            ("btrim", "SELECT btrim('xxhelloxx', 'x')", "hello"),
            // Position / strpos
            ("strpos_found", "SELECT strpos('hello', 'lo')", "4"),
            ("strpos_not_found", "SELECT strpos('hello', 'xyz')", "0"),
            ("strpos_ell", "SELECT strpos('hello', 'ell')", "2"),
            // Replace
            (
                "replace_basic",
                "SELECT replace('hello', 'l', 'r')",
                "herro",
            ),
            ("replace_none", "SELECT replace('hello', 'x', 'y')", "hello"),
            // replace with empty string: AionDB inserts between chars (PG compat)
            (
                "replace_empty",
                "SELECT replace('hello', '', 'x')",
                "xhxexlxlxox",
            ),
            // Repeat
            ("repeat_zero", "SELECT repeat('ab', 0)", ""),
            ("repeat_one", "SELECT repeat('ab', 1)", "ab"),
            ("repeat_three", "SELECT repeat('ab', 3)", "ababab"),
            // Left / Right
            ("left_3", "SELECT left('hello', 3)", "hel"),
            ("left_0", "SELECT left('hello', 0)", ""),
            ("left_over", "SELECT left('hi', 100)", "hi"),
            ("right_3", "SELECT right('hello', 3)", "llo"),
            ("right_0", "SELECT right('hello', 0)", ""),
            // Reverse
            ("reverse", "SELECT reverse('hello')", "olleh"),
            ("reverse_empty", "SELECT reverse('')", ""),
            // lpad / rpad
            ("lpad", "SELECT lpad('hi', 5, '*')", "***hi"),
            ("rpad", "SELECT rpad('hi', 5, '*')", "hi***"),
            ("lpad_truncate", "SELECT lpad('hello', 3, '*')", "hel"),
            // MD5
            (
                "md5",
                "SELECT md5('hello')",
                "5d41402abc4b2a76b9719d911017c592",
            ),
            // Quote
            ("quote_literal", "SELECT quote_literal('hello')", "'hello'"),
            ("quote_ident", "SELECT quote_ident('hello')", "hello"),
            // ASCII / CHR
            ("ascii", "SELECT ascii('A')", "65"),
            ("chr", "SELECT chr(65)", "A"),
            // Escape sequences
            ("escape_quote", "SELECT 'it''s'", "it's"),
            (
                "escape_backslash",
                "SELECT E'line1\\nline2'",
                "line1\nline2",
            ),
        ],
    )
}
