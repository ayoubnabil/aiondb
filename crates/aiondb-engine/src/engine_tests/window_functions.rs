use aiondb_core::Value;

use super::*;

// ---------------------------------------------------------------
// ROW_NUMBER
// ---------------------------------------------------------------

#[test]
fn row_number_over_empty_window() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE t_rn (id INT, name TEXT); \
         INSERT INTO t_rn VALUES (10, 'a'), (20, 'b'), (30, 'c'); \
         SELECT id, ROW_NUMBER() OVER () AS rn FROM t_rn ORDER BY id",
    );
    assert_eq!(rows.len(), 3);
    // Each row gets a row number 1..3; since OVER () has no ORDER BY,
    // the row numbers are assigned in some order. After ORDER BY id,
    // we just verify that all three row numbers {1,2,3} appear.
    let mut rns: Vec<i64> = rows
        .iter()
        .map(|r| match &r.values[1] {
            Value::BigInt(v) => *v,
            other => panic!("expected BigInt, got {other:?}"),
        })
        .collect();
    rns.sort_unstable();
    assert_eq!(rns, vec![1, 2, 3]);
}

#[test]
fn row_number_with_order_by() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE t_rn2 (val INT); \
         INSERT INTO t_rn2 VALUES (30), (10), (20); \
         SELECT val, ROW_NUMBER() OVER (ORDER BY val) AS rn \
         FROM t_rn2 ORDER BY val",
    );
    assert_eq!(rows.len(), 3);
    // Ordered by val ASC: 10 -> rn=1, 20 -> rn=2, 30 -> rn=3
    assert_eq!(rows[0].values[0], Value::Int(10));
    assert_eq!(rows[0].values[1], Value::BigInt(1));
    assert_eq!(rows[1].values[0], Value::Int(20));
    assert_eq!(rows[1].values[1], Value::BigInt(2));
    assert_eq!(rows[2].values[0], Value::Int(30));
    assert_eq!(rows[2].values[1], Value::BigInt(3));
}

#[test]
fn row_number_with_partition() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE employees (name TEXT, dept TEXT, salary INT); \
         INSERT INTO employees VALUES \
           ('alice', 'eng', 100), \
           ('bob', 'eng', 90), \
           ('carol', 'sales', 80), \
           ('dave', 'sales', 85), \
           ('eve', 'sales', 70); \
         SELECT name, dept, salary, \
                ROW_NUMBER() OVER (PARTITION BY dept ORDER BY salary DESC) AS rn \
         FROM employees \
         ORDER BY dept, salary DESC",
    );
    assert_eq!(rows.len(), 5);

    // eng partition (ordered by salary DESC): alice(100)->1, bob(90)->2
    assert_eq!(rows[0].values[0], Value::Text("alice".to_owned()));
    assert_eq!(rows[0].values[1], Value::Text("eng".to_owned()));
    assert_eq!(rows[0].values[2], Value::Int(100));
    assert_eq!(rows[0].values[3], Value::BigInt(1));
    assert_eq!(rows[1].values[0], Value::Text("bob".to_owned()));
    assert_eq!(rows[1].values[1], Value::Text("eng".to_owned()));
    assert_eq!(rows[1].values[2], Value::Int(90));
    assert_eq!(rows[1].values[3], Value::BigInt(2));

    // sales partition (ordered by salary DESC): dave(85)->1, carol(80)->2, eve(70)->3
    assert_eq!(rows[2].values[0], Value::Text("dave".to_owned()));
    assert_eq!(rows[2].values[1], Value::Text("sales".to_owned()));
    assert_eq!(rows[2].values[2], Value::Int(85));
    assert_eq!(rows[2].values[3], Value::BigInt(1));
    assert_eq!(rows[3].values[0], Value::Text("carol".to_owned()));
    assert_eq!(rows[3].values[1], Value::Text("sales".to_owned()));
    assert_eq!(rows[3].values[2], Value::Int(80));
    assert_eq!(rows[3].values[3], Value::BigInt(2));
    assert_eq!(rows[4].values[0], Value::Text("eve".to_owned()));
    assert_eq!(rows[4].values[1], Value::Text("sales".to_owned()));
    assert_eq!(rows[4].values[2], Value::Int(70));
    assert_eq!(rows[4].values[3], Value::BigInt(3));
}

// ---------------------------------------------------------------
// RANK
// ---------------------------------------------------------------

#[test]
fn rank_with_ties() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE scores (player TEXT, score INT); \
         INSERT INTO scores VALUES \
           ('alice', 90), ('bob', 80), ('carol', 90), ('dave', 70), ('eve', 80); \
         SELECT player, score, RANK() OVER (ORDER BY score DESC) AS rnk \
         FROM scores \
         ORDER BY score DESC, player",
    );
    assert_eq!(rows.len(), 5);

    // score DESC: 90,90,80,80,70  -> ranks: 1,1,3,3,5
    assert_eq!(rows[0].values[0], Value::Text("alice".to_owned()));
    assert_eq!(rows[0].values[1], Value::Int(90));
    assert_eq!(rows[0].values[2], Value::BigInt(1));

    assert_eq!(rows[1].values[0], Value::Text("carol".to_owned()));
    assert_eq!(rows[1].values[1], Value::Int(90));
    assert_eq!(rows[1].values[2], Value::BigInt(1));

    assert_eq!(rows[2].values[0], Value::Text("bob".to_owned()));
    assert_eq!(rows[2].values[1], Value::Int(80));
    assert_eq!(rows[2].values[2], Value::BigInt(3));

    assert_eq!(rows[3].values[0], Value::Text("eve".to_owned()));
    assert_eq!(rows[3].values[1], Value::Int(80));
    assert_eq!(rows[3].values[2], Value::BigInt(3));

    assert_eq!(rows[4].values[0], Value::Text("dave".to_owned()));
    assert_eq!(rows[4].values[1], Value::Int(70));
    assert_eq!(rows[4].values[2], Value::BigInt(5));
}

// ---------------------------------------------------------------
// DENSE_RANK
// ---------------------------------------------------------------

#[test]
fn dense_rank_with_ties() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE scores2 (player TEXT, score INT); \
         INSERT INTO scores2 VALUES \
           ('alice', 90), ('bob', 80), ('carol', 90), ('dave', 70), ('eve', 80); \
         SELECT player, score, DENSE_RANK() OVER (ORDER BY score DESC) AS drnk \
         FROM scores2 \
         ORDER BY score DESC, player",
    );
    assert_eq!(rows.len(), 5);

    // score DESC: 90,90,80,80,70  -> dense_ranks: 1,1,2,2,3
    assert_eq!(rows[0].values[0], Value::Text("alice".to_owned()));
    assert_eq!(rows[0].values[1], Value::Int(90));
    assert_eq!(rows[0].values[2], Value::BigInt(1));

    assert_eq!(rows[1].values[0], Value::Text("carol".to_owned()));
    assert_eq!(rows[1].values[1], Value::Int(90));
    assert_eq!(rows[1].values[2], Value::BigInt(1));

    assert_eq!(rows[2].values[0], Value::Text("bob".to_owned()));
    assert_eq!(rows[2].values[1], Value::Int(80));
    assert_eq!(rows[2].values[2], Value::BigInt(2));

    assert_eq!(rows[3].values[0], Value::Text("eve".to_owned()));
    assert_eq!(rows[3].values[1], Value::Int(80));
    assert_eq!(rows[3].values[2], Value::BigInt(2));

    assert_eq!(rows[4].values[0], Value::Text("dave".to_owned()));
    assert_eq!(rows[4].values[1], Value::Int(70));
    assert_eq!(rows[4].values[2], Value::BigInt(3));
}

// ---------------------------------------------------------------
// SUM OVER PARTITION
// ---------------------------------------------------------------

#[test]
fn ntile_over_ordered_partition() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE ntile_scores (player TEXT, score INT); \
         INSERT INTO ntile_scores VALUES \
           ('alice', 100), ('bob', 90), ('carol', 80), ('dave', 70), ('eve', 60); \
         SELECT player, score, NTILE(3) OVER (ORDER BY score DESC) AS bucket \
         FROM ntile_scores \
         ORDER BY score DESC",
    );
    assert_eq!(rows.len(), 5);

    assert_eq!(rows[0].values[2], Value::BigInt(1));
    assert_eq!(rows[1].values[2], Value::BigInt(1));
    assert_eq!(rows[2].values[2], Value::BigInt(2));
    assert_eq!(rows[3].values[2], Value::BigInt(2));
    assert_eq!(rows[4].values[2], Value::BigInt(3));
}

#[test]
fn lag_and_lead_support_offsets_and_defaults() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE lag_lead_sales (day INT, amount INT); \
         INSERT INTO lag_lead_sales VALUES (1, 100), (2, 125), (3, 150); \
         SELECT day, amount, \
                LAG(amount) OVER (ORDER BY day) AS prev_amount, \
                LEAD(amount, 2, 0) OVER (ORDER BY day) AS amount_in_two_days \
         FROM lag_lead_sales \
         ORDER BY day",
    );
    assert_eq!(rows.len(), 3);

    assert_eq!(rows[0].values[2], Value::Null);
    assert_eq!(rows[0].values[3], Value::Int(150));

    assert_eq!(rows[1].values[2], Value::Int(100));
    assert_eq!(rows[1].values[3], Value::Int(0));

    assert_eq!(rows[2].values[2], Value::Int(125));
    assert_eq!(rows[2].values[3], Value::Int(0));
}

#[test]
fn first_and_last_value_follow_partition_order() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // With ORDER BY, default frame is RANGE BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW.
    // first_value returns the first value of the frame (partition start).
    // last_value returns the last value of the frame (current row's peer group end).
    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE temps (city TEXT, day INT, temp INT); \
         INSERT INTO temps VALUES \
           ('paris', 1, 10), ('paris', 2, 12), ('paris', 3, 8), \
           ('rome', 1, 20), ('rome', 2, 22); \
         SELECT city, day, temp, \
                FIRST_VALUE(temp) OVER (PARTITION BY city ORDER BY day) AS first_temp, \
                LAST_VALUE(temp) OVER (PARTITION BY city ORDER BY day) AS last_temp \
         FROM temps \
         ORDER BY city, day",
    );
    assert_eq!(rows.len(), 5);

    // paris: first_value is always 10 (first row of partition)
    // last_value is the current row's temp (frame ends at current row)
    assert_eq!(rows[0].values[3], Value::Int(10));
    assert_eq!(rows[0].values[4], Value::Int(10)); // day=1, frame=[day 1]
    assert_eq!(rows[1].values[3], Value::Int(10));
    assert_eq!(rows[1].values[4], Value::Int(12)); // day=2, frame=[day 1..2]
    assert_eq!(rows[2].values[3], Value::Int(10));
    assert_eq!(rows[2].values[4], Value::Int(8)); // day=3, frame=[day 1..3]

    // rome: first_value is always 20
    assert_eq!(rows[3].values[3], Value::Int(20));
    assert_eq!(rows[3].values[4], Value::Int(20)); // day=1, frame=[day 1]
    assert_eq!(rows[4].values[3], Value::Int(20));
    assert_eq!(rows[4].values[4], Value::Int(22)); // day=2, frame=[day 1..2]
}

#[test]
fn sum_over_partition() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE items (category TEXT, val INT); \
         INSERT INTO items VALUES \
           ('a', 10), ('a', 20), ('b', 5), ('b', 15), ('b', 25); \
         SELECT category, val, SUM(val) OVER (PARTITION BY category) AS total \
         FROM items \
         ORDER BY category, val",
    );
    assert_eq!(rows.len(), 5);

    // Category 'a': sum = 30
    assert_eq!(rows[0].values[0], Value::Text("a".to_owned()));
    assert_eq!(rows[0].values[1], Value::Int(10));
    assert_eq!(rows[0].values[2], Value::Int(30));

    assert_eq!(rows[1].values[0], Value::Text("a".to_owned()));
    assert_eq!(rows[1].values[1], Value::Int(20));
    assert_eq!(rows[1].values[2], Value::Int(30));

    // Category 'b': sum = 45
    assert_eq!(rows[2].values[0], Value::Text("b".to_owned()));
    assert_eq!(rows[2].values[1], Value::Int(5));
    assert_eq!(rows[2].values[2], Value::Int(45));

    assert_eq!(rows[3].values[0], Value::Text("b".to_owned()));
    assert_eq!(rows[3].values[1], Value::Int(15));
    assert_eq!(rows[3].values[2], Value::Int(45));

    assert_eq!(rows[4].values[0], Value::Text("b".to_owned()));
    assert_eq!(rows[4].values[1], Value::Int(25));
    assert_eq!(rows[4].values[2], Value::Int(45));
}

// ---------------------------------------------------------------
// COUNT OVER PARTITION
// ---------------------------------------------------------------

#[test]
fn count_over_partition() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE items2 (category TEXT, val INT); \
         INSERT INTO items2 VALUES \
           ('a', 10), ('a', 20), ('b', 5), ('b', 15), ('b', 25); \
         SELECT category, val, COUNT(*) OVER (PARTITION BY category) AS cnt \
         FROM items2 \
         ORDER BY category, val",
    );
    assert_eq!(rows.len(), 5);

    // Category 'a': count = 2
    assert_eq!(rows[0].values[0], Value::Text("a".to_owned()));
    assert_eq!(rows[0].values[2], Value::BigInt(2));

    assert_eq!(rows[1].values[0], Value::Text("a".to_owned()));
    assert_eq!(rows[1].values[2], Value::BigInt(2));

    // Category 'b': count = 3
    assert_eq!(rows[2].values[0], Value::Text("b".to_owned()));
    assert_eq!(rows[2].values[2], Value::BigInt(3));

    assert_eq!(rows[3].values[0], Value::Text("b".to_owned()));
    assert_eq!(rows[3].values[2], Value::BigInt(3));

    assert_eq!(rows[4].values[0], Value::Text("b".to_owned()));
    assert_eq!(rows[4].values[2], Value::BigInt(3));
}

// ---------------------------------------------------------------
// AVG OVER PARTITION
// ---------------------------------------------------------------

#[test]
fn avg_over_partition() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE items3 (category TEXT, val INT); \
         INSERT INTO items3 VALUES \
           ('a', 10), ('a', 20), ('b', 6), ('b', 12), ('b', 24); \
         SELECT category, val, AVG(val) OVER (PARTITION BY category) AS avg_val \
         FROM items3 \
         ORDER BY category, val",
    );
    assert_eq!(rows.len(), 5);

    // Category 'a': avg = 15.0
    assert_eq!(rows[0].values[0], Value::Text("a".to_owned()));
    assert_eq!(rows[0].values[2], Value::Double(15.0));

    assert_eq!(rows[1].values[0], Value::Text("a".to_owned()));
    assert_eq!(rows[1].values[2], Value::Double(15.0));

    // Category 'b': avg = 14.0
    assert_eq!(rows[2].values[0], Value::Text("b".to_owned()));
    assert_eq!(rows[2].values[2], Value::Double(14.0));

    assert_eq!(rows[3].values[0], Value::Text("b".to_owned()));
    assert_eq!(rows[3].values[2], Value::Double(14.0));

    assert_eq!(rows[4].values[0], Value::Text("b".to_owned()));
    assert_eq!(rows[4].values[2], Value::Double(14.0));
}

// ---------------------------------------------------------------
// MIN / MAX OVER PARTITION
// ---------------------------------------------------------------

#[test]
fn min_max_over_partition() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE items4 (category TEXT, val INT); \
         INSERT INTO items4 VALUES \
           ('a', 10), ('a', 20), ('b', 5), ('b', 15), ('b', 25); \
         SELECT category, val, \
                MIN(val) OVER (PARTITION BY category) AS min_val, \
                MAX(val) OVER (PARTITION BY category) AS max_val \
         FROM items4 \
         ORDER BY category, val",
    );
    assert_eq!(rows.len(), 5);

    // Category 'a': min=10, max=20
    assert_eq!(rows[0].values[0], Value::Text("a".to_owned()));
    assert_eq!(rows[0].values[2], Value::Int(10));
    assert_eq!(rows[0].values[3], Value::Int(20));

    assert_eq!(rows[1].values[0], Value::Text("a".to_owned()));
    assert_eq!(rows[1].values[2], Value::Int(10));
    assert_eq!(rows[1].values[3], Value::Int(20));

    // Category 'b': min=5, max=25
    assert_eq!(rows[2].values[0], Value::Text("b".to_owned()));
    assert_eq!(rows[2].values[2], Value::Int(5));
    assert_eq!(rows[2].values[3], Value::Int(25));

    assert_eq!(rows[3].values[0], Value::Text("b".to_owned()));
    assert_eq!(rows[3].values[2], Value::Int(5));
    assert_eq!(rows[3].values[3], Value::Int(25));

    assert_eq!(rows[4].values[0], Value::Text("b".to_owned()));
    assert_eq!(rows[4].values[2], Value::Int(5));
    assert_eq!(rows[4].values[3], Value::Int(25));
}

// ---------------------------------------------------------------
// Multiple window functions in single query
// ---------------------------------------------------------------

#[test]
fn multiple_window_functions() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE sales (region TEXT, amount INT); \
         INSERT INTO sales VALUES \
           ('east', 100), ('east', 200), ('west', 50), ('west', 150); \
         SELECT region, amount, \
                ROW_NUMBER() OVER (PARTITION BY region ORDER BY amount) AS rn, \
                SUM(amount) OVER (PARTITION BY region) AS total \
         FROM sales \
         ORDER BY region, amount",
    );
    assert_eq!(rows.len(), 4);

    // east: amount=100 rn=1 total=300, amount=200 rn=2 total=300
    assert_eq!(rows[0].values[0], Value::Text("east".to_owned()));
    assert_eq!(rows[0].values[1], Value::Int(100));
    assert_eq!(rows[0].values[2], Value::BigInt(1));
    assert_eq!(rows[0].values[3], Value::Int(300));

    assert_eq!(rows[1].values[0], Value::Text("east".to_owned()));
    assert_eq!(rows[1].values[1], Value::Int(200));
    assert_eq!(rows[1].values[2], Value::BigInt(2));
    assert_eq!(rows[1].values[3], Value::Int(300));

    // west: amount=50 rn=1 total=200, amount=150 rn=2 total=200
    assert_eq!(rows[2].values[0], Value::Text("west".to_owned()));
    assert_eq!(rows[2].values[1], Value::Int(50));
    assert_eq!(rows[2].values[2], Value::BigInt(1));
    assert_eq!(rows[2].values[3], Value::Int(200));

    assert_eq!(rows[3].values[0], Value::Text("west".to_owned()));
    assert_eq!(rows[3].values[1], Value::Int(150));
    assert_eq!(rows[3].values[2], Value::BigInt(2));
    assert_eq!(rows[3].values[3], Value::Int(200));
}

// ---------------------------------------------------------------
// Window function with WHERE clause
// ---------------------------------------------------------------

#[test]
fn window_with_where_clause() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE orders (id INT, status TEXT, total INT); \
         INSERT INTO orders VALUES \
           (1, 'paid', 100), (2, 'paid', 200), (3, 'pending', 50), \
           (4, 'paid', 150), (5, 'pending', 75); \
         SELECT id, total, ROW_NUMBER() OVER (ORDER BY total) AS rn \
         FROM orders \
         WHERE status = 'paid' \
         ORDER BY total",
    );
    // Only 'paid' rows: ids 1(100), 4(150), 2(200)
    assert_eq!(rows.len(), 3);

    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[0].values[1], Value::Int(100));
    assert_eq!(rows[0].values[2], Value::BigInt(1));

    assert_eq!(rows[1].values[0], Value::Int(4));
    assert_eq!(rows[1].values[1], Value::Int(150));
    assert_eq!(rows[1].values[2], Value::BigInt(2));

    assert_eq!(rows[2].values[0], Value::Int(2));
    assert_eq!(rows[2].values[1], Value::Int(200));
    assert_eq!(rows[2].values[2], Value::BigInt(3));
}

// ---------------------------------------------------------------
// ROW_NUMBER with empty partition (single partition, no rows)
// ---------------------------------------------------------------

#[test]
fn row_number_on_empty_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE empty_wf (id INT); \
         SELECT id, ROW_NUMBER() OVER (ORDER BY id) AS rn FROM empty_wf",
    );
    assert_eq!(rows.len(), 0);
}

// ---------------------------------------------------------------
// RANK with all ties
// ---------------------------------------------------------------

#[test]
fn rank_all_ties() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE ties (val INT); \
         INSERT INTO ties VALUES (5), (5), (5); \
         SELECT val, RANK() OVER (ORDER BY val) AS rnk FROM ties ORDER BY val",
    );
    assert_eq!(rows.len(), 3);
    // All same value => all rank 1
    for row in &rows {
        assert_eq!(row.values[1], Value::BigInt(1));
    }
}

// ---------------------------------------------------------------
// DENSE_RANK with three distinct groups
// ---------------------------------------------------------------

#[test]
fn dense_rank_three_groups() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE dr3 (val INT); \
         INSERT INTO dr3 VALUES (10), (20), (20), (30), (30), (30); \
         SELECT val, DENSE_RANK() OVER (ORDER BY val) AS dr FROM dr3 ORDER BY val",
    );
    assert_eq!(rows.len(), 6);
    assert_eq!(rows[0].values[1], Value::BigInt(1)); // 10
    assert_eq!(rows[1].values[1], Value::BigInt(2)); // 20
    assert_eq!(rows[2].values[1], Value::BigInt(2)); // 20
    assert_eq!(rows[3].values[1], Value::BigInt(3)); // 30
    assert_eq!(rows[4].values[1], Value::BigInt(3)); // 30
    assert_eq!(rows[5].values[1], Value::BigInt(3)); // 30
}

// ---------------------------------------------------------------
// SUM OVER with PARTITION BY and ORDER BY combined
// ---------------------------------------------------------------

#[test]
fn sum_over_partition_with_order() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // With ORDER BY, SUM is cumulative (running sum).
    // Default frame: RANGE BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW.
    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE sales_po (region TEXT, month INT, amount INT); \
         INSERT INTO sales_po VALUES \
           ('east', 1, 100), ('east', 2, 200), \
           ('west', 1, 50), ('west', 2, 150); \
         SELECT region, month, amount, \
                SUM(amount) OVER (PARTITION BY region ORDER BY month) AS total \
         FROM sales_po ORDER BY region, month",
    );
    assert_eq!(rows.len(), 4);
    // east: month=1 => 100, month=2 => 300
    assert_eq!(rows[0].values[3], Value::Int(100));
    assert_eq!(rows[1].values[3], Value::Int(300));
    // west: month=1 => 50, month=2 => 200
    assert_eq!(rows[2].values[3], Value::Int(50));
    assert_eq!(rows[3].values[3], Value::Int(200));
}

// ---------------------------------------------------------------
// COUNT(*) OVER without PARTITION BY (whole table)
// ---------------------------------------------------------------

#[test]
fn count_over_whole_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE cnt_all (val INT); \
         INSERT INTO cnt_all VALUES (10), (20), (30); \
         SELECT val, COUNT(*) OVER () AS total FROM cnt_all ORDER BY val",
    );
    assert_eq!(rows.len(), 3);
    for row in &rows {
        assert_eq!(row.values[1], Value::BigInt(3));
    }
}

// ---------------------------------------------------------------
// LAG/LEAD with default value
// ---------------------------------------------------------------

#[test]
fn lag_with_default_value() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE lag_def (id INT, val INT); \
         INSERT INTO lag_def VALUES (1, 10), (2, 20), (3, 30); \
         SELECT id, LAG(val, 1, -1) OVER (ORDER BY id) AS prev \
         FROM lag_def ORDER BY id",
    );
    assert_eq!(rows.len(), 3);
    // id=1 has no previous => default -1
    assert_eq!(rows[0].values[1], Value::Int(-1));
    assert_eq!(rows[1].values[1], Value::Int(10));
    assert_eq!(rows[2].values[1], Value::Int(20));
}

#[test]
fn lead_with_default_value() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE lead_def (id INT, val INT); \
         INSERT INTO lead_def VALUES (1, 10), (2, 20), (3, 30); \
         SELECT id, LEAD(val, 1, 999) OVER (ORDER BY id) AS nxt \
         FROM lead_def ORDER BY id",
    );
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].values[1], Value::Int(20));
    assert_eq!(rows[1].values[1], Value::Int(30));
    // id=3 has no next => default 999
    assert_eq!(rows[2].values[1], Value::Int(999));
}

// ---------------------------------------------------------------
// NTILE with more buckets than rows
// ---------------------------------------------------------------

#[test]
fn ntile_more_buckets_than_rows() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE ntile_small (val INT); \
         INSERT INTO ntile_small VALUES (1), (2); \
         SELECT val, NTILE(5) OVER (ORDER BY val) AS bucket \
         FROM ntile_small ORDER BY val",
    );
    assert_eq!(rows.len(), 2);
    // 2 rows into 5 buckets => each row gets its own bucket: 1, 2
    assert_eq!(rows[0].values[1], Value::BigInt(1));
    assert_eq!(rows[1].values[1], Value::BigInt(2));
}

// ---------------------------------------------------------------
// Multiple window functions in same SELECT (different windows)
// ---------------------------------------------------------------

#[test]
fn multiple_window_functions_different_windows() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "CREATE TABLE mwf (dept TEXT, val INT); \
         INSERT INTO mwf VALUES ('a', 10), ('a', 20), ('b', 30); \
         SELECT dept, val, \
                ROW_NUMBER() OVER (ORDER BY val) AS global_rn, \
                RANK() OVER (PARTITION BY dept ORDER BY val DESC) AS dept_rank \
         FROM mwf ORDER BY val",
    );
    assert_eq!(rows.len(), 3);
    // global row numbers: 1,2,3
    assert_eq!(rows[0].values[2], Value::BigInt(1)); // val=10
    assert_eq!(rows[1].values[2], Value::BigInt(2)); // val=20
    assert_eq!(rows[2].values[2], Value::BigInt(3)); // val=30
                                                     // dept ranks: a: 20->1, 10->2. b: 30->1
    assert_eq!(rows[0].values[3], Value::BigInt(2)); // val=10, dept a, rank 2
    assert_eq!(rows[1].values[3], Value::BigInt(1)); // val=20, dept a, rank 1
    assert_eq!(rows[2].values[3], Value::BigInt(1)); // val=30, dept b, rank 1
}
