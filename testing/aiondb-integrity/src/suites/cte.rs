use crate::harness::SuiteResult;

const SETUP: &str = "
    CREATE TABLE cte_data (id INTEGER PRIMARY KEY, parent_id INTEGER, name TEXT, val INTEGER);
    INSERT INTO cte_data VALUES
        (1, NULL, 'root', 100),
        (2, 1, 'child1', 50),
        (3, 1, 'child2', 30),
        (4, 2, 'grandchild1', 20),
        (5, 2, 'grandchild2', 10),
        (6, 3, 'grandchild3', 40);
";

pub fn run_all() -> SuiteResult {
    super::run_scalar_battery(SETUP, &[
        // Simple CTE
        ("cte_simple",
            "WITH t AS (SELECT * FROM cte_data WHERE val > 30) SELECT count(*) FROM t",
            "3"),

        // CTE with aggregation
        ("cte_agg",
            "WITH sums AS (SELECT parent_id, sum(val) AS total FROM cte_data WHERE parent_id IS NOT NULL GROUP BY parent_id) SELECT max(total) FROM sums",
            "80"),

        // Multiple CTEs joined in main query
        ("cte_multi",
            "WITH
                parents AS (SELECT count(*) AS c FROM cte_data WHERE parent_id IS NULL),
                children AS (SELECT count(*) AS c FROM cte_data WHERE parent_id IS NOT NULL)
            SELECT p.c + ch.c FROM parents p, children ch",
            "6"),

        // CTE referencing another CTE
        ("cte_chain",
            "WITH
                base AS (SELECT id, val FROM cte_data WHERE val > 20),
                filtered AS (SELECT * FROM base WHERE val < 100)
            SELECT count(*) FROM filtered",
            "3"),

        // CTE used in single query
        ("cte_reuse",
            "WITH t AS (SELECT val FROM cte_data)
            SELECT sum(val) - min(val) FROM t",
            "240"),

        // CTE with join
        ("cte_join",
            "WITH parents AS (SELECT id, name FROM cte_data WHERE parent_id IS NULL)
            SELECT count(*) FROM cte_data c JOIN parents p ON c.parent_id = p.id",
            "2"),

        // Recursive CTE (if supported)
        ("cte_recursive",
            "WITH RECURSIVE tree AS (
                SELECT id, name, 0 AS depth FROM cte_data WHERE id = 1
                UNION ALL
                SELECT c.id, c.name, t.depth + 1 FROM cte_data c JOIN tree t ON c.parent_id = t.id
            ) SELECT count(*) FROM tree",
            "6"),

        // Subquery for above-avg filter (no CTE in subquery)
        ("cte_above_avg",
            "SELECT count(*) FROM cte_data WHERE val > (SELECT avg(val) FROM cte_data)",
            "2"),
    ])
}
