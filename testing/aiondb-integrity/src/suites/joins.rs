use crate::harness::SuiteResult;

const SETUP: &str = "
    CREATE TABLE j_users (id INTEGER PRIMARY KEY, name TEXT);
    CREATE TABLE j_orders (id INTEGER PRIMARY KEY, user_id INTEGER, amount INTEGER);
    CREATE TABLE j_tags (id INTEGER PRIMARY KEY, name TEXT);
    CREATE TABLE j_order_tags (order_id INTEGER, tag_id INTEGER);

    INSERT INTO j_users VALUES (1, 'alice'), (2, 'bob'), (3, 'charlie');
    INSERT INTO j_orders VALUES (10, 1, 100), (11, 1, 200), (12, 2, 50);
    INSERT INTO j_tags VALUES (1, 'urgent'), (2, 'vip'), (3, 'bulk');
    INSERT INTO j_order_tags VALUES (10, 1), (10, 2), (11, 1), (12, 3);
";

pub fn run_all() -> SuiteResult {
    super::run_scalar_battery(SETUP, &[
        // INNER JOIN
        ("inner_join_basic",
            "SELECT count(*) FROM j_users u JOIN j_orders o ON u.id = o.user_id",
            "3"),
        ("inner_join_filter",
            "SELECT u.name FROM j_users u JOIN j_orders o ON u.id = o.user_id WHERE o.amount > 100",
            "alice"),
        ("inner_join_no_match",
            "SELECT count(*) FROM j_users u JOIN j_orders o ON u.id = o.user_id WHERE u.name = 'charlie'",
            "0"),
        // LEFT JOIN
        ("left_join_basic",
            "SELECT count(*) FROM j_users u LEFT JOIN j_orders o ON u.id = o.user_id",
            "4"),
        ("left_join_null",
            "SELECT o.id FROM j_users u LEFT JOIN j_orders o ON u.id = o.user_id WHERE u.name = 'charlie'",
            "NULL"),
        // RIGHT JOIN
        ("right_join_basic",
            "SELECT count(*) FROM j_orders o RIGHT JOIN j_users u ON o.user_id = u.id",
            "4"),
        // CROSS JOIN
        ("cross_join",
            "SELECT count(*) FROM j_users CROSS JOIN j_tags",
            "9"),
        // Self join
        ("self_join",
            "SELECT count(*) FROM j_orders o1 JOIN j_orders o2 ON o1.user_id = o2.user_id WHERE o1.id < o2.id",
            "1"),
        // Multi-table join
        ("multi_join",
            "SELECT count(*) FROM j_orders o JOIN j_order_tags ot ON o.id = ot.order_id JOIN j_tags t ON ot.tag_id = t.id",
            "4"),
        // Join with aggregation
        ("join_agg",
            "SELECT u.name FROM j_users u JOIN j_orders o ON u.id = o.user_id GROUP BY u.name HAVING sum(o.amount) > 100",
            "alice"),
        // Natural join
        ("natural_join",
            "SELECT count(*) FROM j_tags NATURAL JOIN (SELECT 1 AS id, 'urgent' AS name) sub",
            "1"),
        // Join with subquery
        ("join_subquery",
            "SELECT count(*) FROM j_users u JOIN (SELECT user_id, sum(amount) AS total FROM j_orders GROUP BY user_id) s ON u.id = s.user_id WHERE s.total > 100",
            "1"),
    ])
}
