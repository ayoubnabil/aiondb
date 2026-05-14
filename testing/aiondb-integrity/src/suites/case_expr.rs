use crate::harness::SuiteResult;

pub fn run_all() -> SuiteResult {
    super::run_scalar_battery("", &[
        // Simple CASE
        ("case_simple_match",
            "SELECT CASE 1 WHEN 1 THEN 'one' WHEN 2 THEN 'two' ELSE 'other' END",
            "one"),
        ("case_simple_else",
            "SELECT CASE 3 WHEN 1 THEN 'one' WHEN 2 THEN 'two' ELSE 'other' END",
            "other"),
        ("case_simple_no_else",
            "SELECT CASE 3 WHEN 1 THEN 'one' WHEN 2 THEN 'two' END",
            "NULL"),

        // Searched CASE
        ("case_searched_true",
            "SELECT CASE WHEN 1 > 0 THEN 'pos' WHEN 1 < 0 THEN 'neg' ELSE 'zero' END",
            "pos"),
        ("case_searched_second",
            "SELECT CASE WHEN 1 < 0 THEN 'neg' WHEN 1 > 0 THEN 'pos' ELSE 'zero' END",
            "pos"),
        ("case_searched_else",
            "SELECT CASE WHEN 1 < 0 THEN 'neg' ELSE 'non-neg' END",
            "non-neg"),

        // CASE with expressions
        ("case_expr",
            "SELECT CASE WHEN 2 + 2 = 4 THEN 'math works' ELSE 'broken' END",
            "math works"),

        // CASE with NULL
        ("case_null_operand",
            "SELECT CASE NULL WHEN NULL THEN 'match' ELSE 'no match' END",
            "no match"),
        ("case_null_condition",
            "SELECT CASE WHEN NULL::boolean THEN 'yes' ELSE 'no' END",
            "no"),
        ("case_is_null",
            "SELECT CASE WHEN NULL IS NULL THEN 'null' ELSE 'not null' END",
            "null"),

        // Nested CASE
        ("case_nested",
            "SELECT CASE WHEN 1 = 1 THEN CASE WHEN 2 = 2 THEN 'deep' ELSE 'nope' END ELSE 'outer' END",
            "deep"),

        // CASE in arithmetic
        ("case_arith",
            "SELECT 10 + CASE WHEN true THEN 5 ELSE 0 END",
            "15"),

        // CASE with aggregates context
        ("case_in_query",
            "SELECT CASE WHEN count(*) > 0 THEN 'has rows' ELSE 'empty' END FROM (SELECT 1) sub",
            "has rows"),
    ])
}
