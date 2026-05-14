use super::*;

// =======================================================================
// 8. Limit parity tests (MISS-D1)
// =======================================================================

#[tokio::test]
async fn limit_select_many_rows_ordered() -> DbResult<()> {
    let mut insert_values = String::new();
    for i in 1..=100 {
        if !insert_values.is_empty() {
            insert_values.push_str(", ");
        }
        insert_values.push_str(&format!("({i}, 'row_{i}')"));
    }
    let setup = format!(
        "CREATE TABLE lim_many (id INT, label TEXT); \
             INSERT INTO lim_many VALUES {insert_values}"
    );
    let scenario = SqlScenario::new(
        "limit_many_rows_ordered",
        "SELECT id, label FROM lim_many ORDER BY id ASC",
    )
    .with_setup_sql(setup);
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn limit_select_with_limit_and_offset() -> DbResult<()> {
    let mut insert_values = String::new();
    for i in 1..=20 {
        if !insert_values.is_empty() {
            insert_values.push_str(", ");
        }
        insert_values.push_str(&format!("({i})"));
    }
    let setup = format!(
        "CREATE TABLE lim_off (id INT); \
             INSERT INTO lim_off VALUES {insert_values}"
    );
    let scenario = SqlScenario::new(
        "limit_select_limit_offset",
        "SELECT id FROM lim_off ORDER BY id ASC LIMIT 5 OFFSET 10",
    )
    .with_setup_sql(setup);
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn limit_insert_large_row_count() -> DbResult<()> {
    let mut insert_values = String::new();
    for i in 1..=200 {
        if !insert_values.is_empty() {
            insert_values.push_str(", ");
        }
        insert_values.push_str(&format!("({i})"));
    }
    let scenario = SqlScenario::new(
        "limit_insert_large",
        format!(
            "INSERT INTO lim_bulk VALUES {insert_values}; \
                 SELECT COUNT(*) AS cnt FROM lim_bulk"
        ),
    )
    .with_setup_sql("CREATE TABLE lim_bulk (id INT)");
    assert_scenario_matches(&scenario).await
}

#[tokio::test]
async fn limit_aggregate_on_large_dataset() -> DbResult<()> {
    let mut insert_values = String::new();
    for i in 1..=100 {
        if !insert_values.is_empty() {
            insert_values.push_str(", ");
        }
        insert_values.push_str(&format!("({i})"));
    }
    let setup = format!(
        "CREATE TABLE lim_agg (val INT); \
             INSERT INTO lim_agg VALUES {insert_values}"
    );
    let scenario = SqlScenario::new(
        "limit_aggregate_large",
        "SELECT COUNT(*) AS cnt, SUM(val) AS total, MIN(val) AS lo, MAX(val) AS hi \
             FROM lim_agg",
    )
    .with_setup_sql(setup);
    assert_scenario_matches(&scenario).await
}

// =======================================================================
// 9. Error parity on limits (MISS-D1)
// =======================================================================

#[tokio::test]
async fn error_sql_exceeding_max_length() -> DbResult<()> {
    // Build a SQL string that exceeds the 16 MiB MAX_SQL_LENGTH.
    // Use a SELECT with a massive repeated column list.
    let repeated = "1,".repeat(16 * 1024 * 1024);
    let long_sql = format!("SELECT {repeated}1");
    let scenario = SqlScenario::new("err_max_sql_length", &long_sql).expect_error();
    assert_scenario_matches(&scenario).await
}

#[test]
fn error_deeply_nested_expression() -> DbResult<()> {
    // Build an expression with 129 levels of parenthesised nesting.
    // The parser rejects this with a syntax error once expression
    // depth exceeds 128.  Debug builds use considerably more stack
    // per frame, so we run everything on a thread with a generous
    // 64 MiB stack and create a tokio runtime whose worker threads
    // also have 64 MiB stacks (the pgwire connection is spawned
    // onto a worker thread).
    let depth = 129;
    let open: String = "(".repeat(depth);
    let close: String = ")".repeat(depth);
    let sql = format!("SELECT {open}1{close}");

    let scenario = SqlScenario::new("err_deeply_nested", &sql).expect_error();

    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .thread_stack_size(64 * 1024 * 1024)
                .enable_all()
                .build()
                .expect("build runtime with large stack");
            rt.block_on(async { assert_scenario_matches(&scenario).await })
        })
        .expect("spawn thread")
        .join()
        .expect("join thread")
}
