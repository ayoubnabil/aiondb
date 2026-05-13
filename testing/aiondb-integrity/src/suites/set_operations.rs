use crate::harness::SuiteResult;

const SETUP: &str = "
    CREATE TABLE set_a (id INTEGER, val TEXT);
    CREATE TABLE set_b (id INTEGER, val TEXT);
    INSERT INTO set_a VALUES (1,'x'),(2,'y'),(3,'z'),(2,'y');
    INSERT INTO set_b VALUES (2,'y'),(3,'z'),(4,'w');
";

pub fn run_all() -> SuiteResult {
    super::run_scalar_battery(SETUP, &[
        // UNION (dedup)
        ("union_dedup",
            "SELECT count(*) FROM (SELECT * FROM set_a UNION SELECT * FROM set_b) sub",
            "4"),

        // UNION ALL (no dedup)
        ("union_all",
            "SELECT count(*) FROM (SELECT * FROM set_a UNION ALL SELECT * FROM set_b) sub",
            "7"),

        // INTERSECT
        ("intersect",
            "SELECT count(*) FROM (SELECT * FROM set_a INTERSECT SELECT * FROM set_b) sub",
            "2"),

        // EXCEPT
        ("except",
            "SELECT count(*) FROM (SELECT * FROM set_a EXCEPT SELECT * FROM set_b) sub",
            "1"),

        // EXCEPT reversed
        ("except_reversed",
            "SELECT count(*) FROM (SELECT * FROM set_b EXCEPT SELECT * FROM set_a) sub",
            "1"),

        // UNION with ORDER BY
        ("union_order",
            "SELECT val FROM (SELECT val FROM set_a UNION SELECT val FROM set_b) sub ORDER BY val LIMIT 1",
            "w"),

        // Chained set ops
        ("union_chain",
            "SELECT count(*) FROM (
                SELECT id FROM set_a
                UNION
                SELECT id FROM set_b
                UNION
                SELECT 5 AS id
            ) sub",
            "5"),

        // Empty set
        ("union_empty",
            "SELECT count(*) FROM (
                SELECT * FROM set_a WHERE 1=0
                UNION ALL
                SELECT * FROM set_b
            ) sub",
            "3"),

        // INTERSECT empty
        ("intersect_empty",
            "SELECT count(*) FROM (
                SELECT * FROM set_a WHERE 1=0
                INTERSECT
                SELECT * FROM set_b
            ) sub",
            "0"),
    ])
}
