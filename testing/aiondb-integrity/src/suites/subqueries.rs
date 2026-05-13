use crate::harness::SuiteResult;

const SETUP: &str = "
    CREATE TABLE sq_items (id INTEGER PRIMARY KEY, name TEXT, price INTEGER, category TEXT);
    INSERT INTO sq_items VALUES
        (1, 'widget', 10, 'A'),
        (2, 'gadget', 25, 'A'),
        (3, 'thing', 15, 'B'),
        (4, 'doodad', 30, 'B'),
        (5, 'gizmo', 20, 'A');
";

pub fn run_all() -> SuiteResult {
    super::run_scalar_battery(SETUP, &[
        // Scalar subquery
        ("scalar_sub",
            "SELECT name FROM sq_items WHERE price = (SELECT max(price) FROM sq_items)",
            "doodad"),
        // IN subquery
        ("in_sub",
            "SELECT count(*) FROM sq_items WHERE category IN (SELECT category FROM sq_items WHERE price > 20)",
            "5"),
        // NOT IN subquery
        ("not_in_sub",
            "SELECT count(*) FROM sq_items WHERE id NOT IN (SELECT id FROM sq_items WHERE price < 15)",
            "4"),
        // EXISTS
        ("exists_sub",
            "SELECT count(*) FROM sq_items i WHERE EXISTS (SELECT 1 FROM sq_items i2 WHERE i2.category = i.category AND i2.id <> i.id)",
            "5"),
        // NOT EXISTS
        ("not_exists_sub",
            "SELECT count(*) FROM sq_items i WHERE NOT EXISTS (SELECT 1 FROM sq_items i2 WHERE i2.category = i.category AND i2.price > i.price)",
            "2"),
        // Correlated subquery in SELECT
        ("correlated_select",
            "SELECT name FROM sq_items i WHERE price = (SELECT max(price) FROM sq_items i2 WHERE i2.category = i.category) ORDER BY name LIMIT 1",
            "doodad"),
        // Subquery in FROM
        ("from_sub",
            "SELECT count(*) FROM (SELECT * FROM sq_items WHERE price > 15) sub",
            "3"),
        // Derived table with alias
        ("derived_agg",
            "SELECT max(sub.total) FROM (SELECT category, sum(price) AS total FROM sq_items GROUP BY category) sub",
            "55"),
        // Nested subqueries
        ("nested_sub",
            "SELECT name FROM sq_items WHERE price > (SELECT avg(price) FROM sq_items WHERE category = (SELECT category FROM sq_items WHERE id = 1)) ORDER BY name LIMIT 1",
            "doodad"),
    ])
}
