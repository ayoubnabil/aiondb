use crate::harness::SuiteResult;

const SETUP: &str = "
    CREATE TABLE agg_data (id INTEGER PRIMARY KEY, grp TEXT, val INTEGER, maybe_null INTEGER);
    INSERT INTO agg_data VALUES
        (1, 'A', 10, 10),
        (2, 'A', 20, NULL),
        (3, 'A', 30, 30),
        (4, 'B', 40, 40),
        (5, 'B', 50, NULL),
        (6, 'C', 60, 60);
";

pub fn run_all() -> SuiteResult {
    super::run_scalar_battery(
        SETUP,
        &[
            // count
            ("count_star", "SELECT count(*) FROM agg_data", "6"),
            ("count_col", "SELECT count(maybe_null) FROM agg_data", "4"),
            (
                "count_distinct",
                "SELECT count(DISTINCT grp) FROM agg_data",
                "3",
            ),
            (
                "count_empty",
                "SELECT count(*) FROM agg_data WHERE id > 100",
                "0",
            ),
            // sum
            ("sum_all", "SELECT sum(val) FROM agg_data", "210"),
            (
                "sum_group",
                "SELECT sum(val) FROM agg_data WHERE grp = 'A'",
                "60",
            ),
            ("sum_null", "SELECT sum(maybe_null) FROM agg_data", "140"),
            (
                "sum_empty",
                "SELECT sum(val) FROM agg_data WHERE id > 100",
                "NULL",
            ),
            // avg
            ("avg_all", "SELECT avg(val)::integer FROM agg_data", "35"),
            (
                "avg_null_skipped",
                "SELECT avg(maybe_null)::integer FROM agg_data",
                "35",
            ),
            // min / max
            ("min_all", "SELECT min(val) FROM agg_data", "10"),
            ("max_all", "SELECT max(val) FROM agg_data", "60"),
            ("min_text", "SELECT min(grp) FROM agg_data", "A"),
            ("max_text", "SELECT max(grp) FROM agg_data", "C"),
            (
                "min_empty",
                "SELECT min(val) FROM agg_data WHERE id > 100",
                "NULL",
            ),
            (
                "max_empty",
                "SELECT max(val) FROM agg_data WHERE id > 100",
                "NULL",
            ),
            // Group by
            (
                "group_count",
                "SELECT count(*) FROM (SELECT grp, count(*) FROM agg_data GROUP BY grp) sub",
                "3",
            ),
            (
                "group_sum_a",
                "SELECT sum(val) FROM agg_data GROUP BY grp HAVING grp = 'B'",
                "90",
            ),
            // Mixed aggregates (subquery for avg)
            (
                "mixed_sub",
                "SELECT count(*) FROM agg_data WHERE val > (SELECT avg(val) FROM agg_data)",
                "3",
            ),
            // bool_and / bool_or
            (
                "count_where_null",
                "SELECT count(*) FROM agg_data WHERE maybe_null IS NULL",
                "2",
            ),
            (
                "count_where_not_null",
                "SELECT count(*) FROM agg_data WHERE maybe_null IS NOT NULL",
                "4",
            ),
        ],
    )
}
