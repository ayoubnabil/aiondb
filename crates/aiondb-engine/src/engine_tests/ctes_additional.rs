use aiondb_core::Value;

use super::*;

fn setup_items(engine: &Engine, session: &SessionHandle) {
    engine
        .execute_sql(
            session,
            "CREATE TABLE items (id INT, name TEXT, category TEXT, price INT); \
             INSERT INTO items VALUES \
                (1, 'apple', 'fruit', 2), \
                (2, 'banana', 'fruit', 1), \
                (3, 'carrot', 'vegetable', 3), \
                (4, 'date', 'fruit', 5), \
                (5, 'eggplant', 'vegetable', 4)",
        )
        .expect("setup items");
}

#[test]
fn cte_empty_result_join_returns_nothing() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_items(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "WITH nothing AS (SELECT id, name FROM items WHERE 1 = 0) \
         SELECT * FROM nothing",
    );
    assert_eq!(rows.len(), 0);
}

#[test]
fn cte_shadows_base_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_items(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "WITH items AS (SELECT 999 AS id, 'shadow' AS name) \
         SELECT id, name FROM items",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Int(999));
    assert_eq!(rows[0].values[1], Value::Text("shadow".to_owned()));
}

#[test]
fn nested_with_inner_cte_shadows_outer_cte() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "WITH x AS (SELECT 1 AS id) \
         SELECT * \
         FROM (WITH x AS (SELECT 2 AS id) SELECT id FROM x) AS inner_x",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values, vec![Value::Int(2)]);
}

#[test]
fn cte_with_max_aggregation() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    setup_items(&engine, &session);

    let rows = query_rows(
        &engine,
        &session,
        "WITH max_prices AS (\
             SELECT category, max(price) AS max_price FROM items GROUP BY category\
         ) \
         SELECT category, max_price FROM max_prices ORDER BY category",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::Text("fruit".to_owned()));
    assert_eq!(rows[0].values[1], Value::Int(5));
    assert_eq!(rows[1].values[0], Value::Text("vegetable".to_owned()));
    assert_eq!(rows[1].values[1], Value::Int(4));
}
