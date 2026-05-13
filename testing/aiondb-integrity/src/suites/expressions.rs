use crate::harness::SuiteResult;

pub fn run_all() -> SuiteResult {
    super::run_scalar_battery(
        "",
        &[
            // Arithmetic
            ("add_int", "SELECT 2 + 3", "5"),
            ("sub_int", "SELECT 10 - 7", "3"),
            ("mul_int", "SELECT 6 * 7", "42"),
            ("div_int", "SELECT 15 / 4", "3"),
            ("mod_int", "SELECT 17 % 5", "2"),
            ("neg_int", "SELECT -42", "-42"),
            ("double_neg", "SELECT -(-42)", "42"),
            ("parens", "SELECT (2 + 3) * 4", "20"),
            ("precedence", "SELECT 2 + 3 * 4", "14"),
            ("nested_parens", "SELECT ((1 + 2) * (3 + 4))", "21"),
            // Comparison returning bool
            ("cmp_eq_true", "SELECT 1 = 1", "t"),
            ("cmp_eq_false", "SELECT 1 = 2", "f"),
            ("cmp_neq", "SELECT 1 <> 2", "t"),
            ("cmp_lt", "SELECT 1 < 2", "t"),
            ("cmp_gt", "SELECT 2 > 1", "t"),
            // String operators
            ("concat", "SELECT 'hello' || ' ' || 'world'", "hello world"),
            ("upper", "SELECT upper('hello')", "HELLO"),
            ("lower", "SELECT lower('HELLO')", "hello"),
            ("trim", "SELECT trim('  hello  ')", "hello"),
            ("ltrim", "SELECT ltrim('  hello')", "hello"),
            ("rtrim", "SELECT rtrim('hello  ')", "hello"),
            ("substr", "SELECT substring('hello' FROM 2 FOR 3)", "ell"),
            ("position", "SELECT strpos('hello', 'lo')", "4"),
            ("replace", "SELECT replace('hello', 'l', 'r')", "herro"),
            ("repeat", "SELECT repeat('ab', 3)", "ababab"),
            ("left", "SELECT left('hello', 3)", "hel"),
            ("right", "SELECT right('hello', 3)", "llo"),
            ("char_length", "SELECT char_length('hello')", "5"),
            ("octet_length", "SELECT octet_length('hello')", "5"),
            // Math functions
            ("abs_pos", "SELECT abs(42)", "42"),
            ("abs_neg", "SELECT abs(-42)", "42"),
            ("ceil", "SELECT ceil(4.2)", "5"),
            ("floor", "SELECT floor(4.8)", "4"),
            ("round", "SELECT round(4.5)", "5"),
            ("round_places", "SELECT round(4.567, 2)", "4.57"),
            ("sign_pos", "SELECT sign(42)", "1"),
            ("sign_neg", "SELECT sign(-42)", "-1"),
            ("sign_zero", "SELECT sign(0)", "0"),
            ("power", "SELECT power(2, 10)", "1024"),
            ("sqrt", "SELECT sqrt(144)", "12"),
            ("mod_func", "SELECT mod(17, 5)", "2"),
            // COALESCE / NULLIF
            ("coalesce_first", "SELECT coalesce(1, 2, 3)", "1"),
            ("coalesce_null", "SELECT coalesce(NULL, 2, 3)", "2"),
            ("coalesce_all_null", "SELECT coalesce(NULL, NULL)", "NULL"),
            ("nullif_same", "SELECT nullif(1, 1)", "NULL"),
            ("nullif_diff", "SELECT nullif(1, 2)", "1"),
            // GREATEST / LEAST
            ("greatest", "SELECT greatest(1, 5, 3)", "5"),
            ("least", "SELECT least(1, 5, 3)", "1"),
            // Type cast expressions
            ("cast_int", "SELECT CAST('42' AS INTEGER)", "42"),
            ("cast_text", "SELECT CAST(42 AS TEXT)", "42"),
            ("cast_bool", "SELECT CAST('true' AS BOOLEAN)", "t"),
            ("cast_double", "SELECT CAST(42 AS DOUBLE PRECISION)", "42"),
            // Boolean expressions
            ("bool_and_short", "SELECT false AND (1/0 = 1)", "f"),
            ("bool_or_short", "SELECT true OR (1/0 = 1)", "t"),
        ],
    )
}
