use crate::harness::SuiteResult;

pub fn run_all() -> SuiteResult {
    super::run_scalar_battery(
        "",
        &[
            // NULL arithmetic (cast to integer so operator resolves)
            ("null_plus", "SELECT NULL::integer + 1", "NULL"),
            ("null_minus", "SELECT NULL::integer - 1", "NULL"),
            ("null_mul", "SELECT NULL::integer * 1", "NULL"),
            ("null_div", "SELECT NULL::integer / 1", "NULL"),
            ("null_concat", "SELECT NULL || 'text'", "NULL"),
            // NULL comparisons (all should be NULL, not true/false)
            ("null_eq_null", "SELECT NULL = NULL", "NULL"),
            ("null_neq_null", "SELECT NULL <> NULL", "NULL"),
            ("null_lt", "SELECT NULL < 1", "NULL"),
            ("null_gt", "SELECT NULL > 1", "NULL"),
            ("int_eq_null", "SELECT 1 = NULL", "NULL"),
            // IS NULL / IS NOT NULL
            ("is_null", "SELECT NULL IS NULL", "t"),
            ("is_not_null", "SELECT NULL IS NOT NULL", "f"),
            ("val_is_null", "SELECT 1 IS NULL", "f"),
            ("val_is_not_null", "SELECT 1 IS NOT NULL", "t"),
            // NULL in boolean logic
            ("null_and_true", "SELECT NULL AND true", "NULL"),
            ("null_and_false", "SELECT NULL AND false", "f"),
            ("null_or_true", "SELECT NULL OR true", "t"),
            ("null_or_false", "SELECT NULL OR false", "NULL"),
            ("not_null", "SELECT NOT NULL", "NULL"),
            // COALESCE with NULL
            ("coalesce_null_first", "SELECT coalesce(NULL, 42)", "42"),
            (
                "coalesce_null_all",
                "SELECT coalesce(NULL, NULL, NULL)",
                "NULL",
            ),
            ("coalesce_value", "SELECT coalesce(1, NULL, 3)", "1"),
            // NULLIF
            ("nullif_match", "SELECT nullif(42, 42)", "NULL"),
            ("nullif_nomatch", "SELECT nullif(42, 0)", "42"),
            // NULL in CASE (must be boolean expression)
            (
                "case_null_bool",
                "SELECT CASE WHEN NULL::boolean THEN 'yes' ELSE 'no' END",
                "no",
            ),
            (
                "case_is_null",
                "SELECT CASE WHEN NULL IS NULL THEN 'yes' ELSE 'no' END",
                "yes",
            ),
            // NULL in IN
            ("in_null", "SELECT 1 IN (1, NULL)", "t"),
            ("in_null_miss", "SELECT 2 IN (1, NULL)", "NULL"),
            ("not_in_null", "SELECT 2 NOT IN (1, NULL)", "NULL"),
            // Aggregate ignoring NULL
            (
                "count_null",
                "SELECT count(x) FROM (SELECT NULL AS x UNION ALL SELECT 1) sub",
                "1",
            ),
            (
                "sum_null",
                "SELECT sum(x) FROM (SELECT NULL::integer AS x UNION ALL SELECT 1) sub",
                "1",
            ),
        ],
    )
}
