use super::*;

fn fresh_engine_and_session() -> (Engine, SessionHandle) {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    (engine, session)
}

fn open_extra_session(engine: &Engine) -> SessionHandle {
    let (session, _) = engine.startup(startup_params()).expect("startup");
    session
}

#[test]
fn listen_and_notify_same_session_immediate() {
    let (engine, session) = fresh_engine_and_session();

    let results = engine
        .execute_sql(&session, "LISTEN events;")
        .expect("LISTEN");
    assert!(matches!(&results[0], StatementResult::Command { tag, .. } if tag == "LISTEN"));

    let results = engine
        .execute_sql(&session, "NOTIFY events, 'hello';")
        .expect("NOTIFY");
    assert!(matches!(&results[0], StatementResult::Command { tag, .. } if tag == "NOTIFY"));

    let notifications = engine.notification_bus().drain_for(&session);
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].channel, "events");
    assert_eq!(notifications[0].payload, "hello");
}

#[test]
fn notify_inside_explicit_txn_buffers_until_commit() {
    let (engine, session) = fresh_engine_and_session();

    engine
        .execute_sql(&session, "LISTEN events;")
        .expect("LISTEN");
    engine.execute_sql(&session, "BEGIN;").expect("BEGIN");
    engine
        .execute_sql(&session, "NOTIFY events, 'buffered';")
        .expect("NOTIFY");

    assert!(
        engine.notification_bus().drain_for(&session).is_empty(),
        "NOTIFY inside txn must not deliver before COMMIT",
    );

    engine.execute_sql(&session, "COMMIT;").expect("COMMIT");

    let notifications = engine.notification_bus().drain_for(&session);
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].payload, "buffered");
}

#[test]
fn notify_inside_rolled_back_txn_is_discarded() {
    let (engine, session) = fresh_engine_and_session();

    engine
        .execute_sql(&session, "LISTEN events;")
        .expect("LISTEN");
    engine.execute_sql(&session, "BEGIN;").expect("BEGIN");
    engine
        .execute_sql(&session, "NOTIFY events, 'rolled';")
        .expect("NOTIFY");
    engine.execute_sql(&session, "ROLLBACK;").expect("ROLLBACK");

    assert!(
        engine.notification_bus().drain_for(&session).is_empty(),
        "NOTIFY in rolled-back txn must not deliver",
    );
}

#[test]
fn cross_session_listen_notify_delivers() {
    let (engine, subscriber) = fresh_engine_and_session();
    let publisher = open_extra_session(&engine);

    engine
        .execute_sql(&subscriber, "LISTEN events;")
        .expect("LISTEN");
    engine
        .execute_sql(&publisher, "NOTIFY events, 'cross';")
        .expect("NOTIFY");

    let delivered = engine.notification_bus().drain_for(&subscriber);
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].channel, "events");
    assert_eq!(delivered[0].payload, "cross");

    assert!(
        engine.notification_bus().drain_for(&publisher).is_empty(),
        "publisher is not subscribed; outbox empty",
    );
}

#[test]
fn unlisten_stops_delivery() {
    let (engine, session) = fresh_engine_and_session();

    engine
        .execute_sql(&session, "LISTEN events;")
        .expect("LISTEN");
    engine
        .execute_sql(&session, "UNLISTEN events;")
        .expect("UNLISTEN");
    engine
        .execute_sql(&session, "NOTIFY events, 'after-unlisten';")
        .expect("NOTIFY");

    assert!(
        engine.notification_bus().drain_for(&session).is_empty(),
        "UNLISTEN removes subscription",
    );
}

#[test]
fn unlisten_wildcard_drops_all_channels() {
    let (engine, session) = fresh_engine_and_session();

    engine.execute_sql(&session, "LISTEN a;").expect("LISTEN a");
    engine.execute_sql(&session, "LISTEN b;").expect("LISTEN b");
    engine
        .execute_sql(&session, "UNLISTEN *;")
        .expect("wildcard");

    engine.execute_sql(&session, "NOTIFY a;").expect("NOTIFY a");
    engine.execute_sql(&session, "NOTIFY b;").expect("NOTIFY b");

    assert!(
        engine.notification_bus().drain_for(&session).is_empty(),
        "UNLISTEN * clears every subscription",
    );
}

#[test]
fn pg_notify_scalar_delivers_through_eval_context() {
    let (engine, subscriber) = fresh_engine_and_session();
    let publisher = open_extra_session(&engine);

    engine
        .execute_sql(&subscriber, "LISTEN events;")
        .expect("LISTEN");

    let rows = query_rows(
        &engine,
        &publisher,
        "SELECT pg_notify('events', 'from-select');",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Null);

    let delivered = engine.notification_bus().drain_for(&subscriber);
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].channel, "events");
    assert_eq!(delivered[0].payload, "from-select");
}

#[test]
fn pg_notify_scalar_inside_explicit_txn_buffers_until_commit() {
    let (engine, session) = fresh_engine_and_session();

    engine
        .execute_sql(&session, "LISTEN events;")
        .expect("LISTEN");
    engine.execute_sql(&session, "BEGIN;").expect("BEGIN");
    query_rows(
        &engine,
        &session,
        "SELECT pg_notify('events', 'buffered-select');",
    );

    assert!(
        engine.notification_bus().drain_for(&session).is_empty(),
        "pg_notify inside txn must not deliver before COMMIT",
    );

    engine.execute_sql(&session, "COMMIT;").expect("COMMIT");

    let delivered = engine.notification_bus().drain_for(&session);
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].payload, "buffered-select");
}

#[test]
fn pg_notification_helpers_read_current_session_state() {
    let (engine, session) = fresh_engine_and_session();

    engine.execute_sql(&session, "LISTEN b;").expect("LISTEN b");
    engine.execute_sql(&session, "LISTEN a;").expect("LISTEN a");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT pg_listening_channels(), pg_notification_queue_usage();",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].values[0],
        Value::Array(vec![Value::Text("a".into()), Value::Text("b".into())])
    );
    assert_eq!(rows[0].values[1], Value::Double(0.0));
}
