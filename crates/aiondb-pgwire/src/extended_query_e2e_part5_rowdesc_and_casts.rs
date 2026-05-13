use super::*;

#[tokio::test]
async fn extended_describe_statement_reports_direct_column_origin() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-rowdesc-origin-stmt".to_owned()),
            options: std::collections::BTreeMap::new(),
            credential: Credential::Anonymous {
                user: "test".to_owned(),
            },
            transport: TransportInfo::in_process(),
        })
        .expect("startup seed session");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE t_rowdesc_origin_stmt (id INT, name TEXT)",
        )
        .expect("create rowdesc origin table");
    let expected_oid = match engine
        .execute_sql(
            &session,
            "SELECT oid FROM pg_class WHERE relname = 't_rowdesc_origin_stmt'",
        )
        .expect("lookup pg_class oid")
        .into_iter()
        .next()
        .expect("oid row")
    {
        StatementResult::Query { rows, .. } => match rows
            .first()
            .and_then(|row| row.values.first())
            .expect("oid value")
        {
            Value::Int(oid) => u32::try_from(*oid).expect("positive oid"),
            other => panic!("expected oid int, got {other:?}"),
        },
        other => panic!("expected oid query result, got {other:?}"),
    };

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s1",
        "SELECT id FROM t_rowdesc_origin_stmt",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s1"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run().await.expect("describe direct column statement");

    let origin_info = backend_messages(conn.writer_ref())
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| parse_row_description_origin_info(payload))
        .expect("row description");
    assert_eq!(origin_info, vec![(expected_oid, 1)]);
}

#[tokio::test]
async fn extended_describe_statement_reports_joined_alias_column_origin() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-rowdesc-origin-join".to_owned()),
            options: std::collections::BTreeMap::new(),
            credential: Credential::Anonymous {
                user: "test".to_owned(),
            },
            transport: TransportInfo::in_process(),
        })
        .expect("startup seed session");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE t_rowdesc_join_left (id INT, ref_id INT)",
        )
        .expect("create left join table");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE t_rowdesc_join_right (id INT, name TEXT)",
        )
        .expect("create right join table");
    let expected_oid = match engine
        .execute_sql(
            &session,
            "SELECT oid FROM pg_class WHERE relname = 't_rowdesc_join_right'",
        )
        .expect("lookup pg_class oid")
        .into_iter()
        .next()
        .expect("oid row")
    {
        StatementResult::Query { rows, .. } => match rows
            .first()
            .and_then(|row| row.values.first())
            .expect("oid value")
        {
            Value::Int(oid) => u32::try_from(*oid).expect("positive oid"),
            other => panic!("expected oid int, got {other:?}"),
        },
        other => panic!("expected oid query result, got {other:?}"),
    };

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_join_origin",
        "SELECT r.name FROM t_rowdesc_join_left AS l JOIN t_rowdesc_join_right AS r ON l.ref_id = r.id",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_join_origin"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe joined alias column statement");

    let origin_info = backend_messages(conn.writer_ref())
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| parse_row_description_origin_info(payload))
        .expect("row description");
    assert_eq!(origin_info, vec![(expected_oid, 2)]);
}

#[tokio::test]
async fn extended_describe_statement_reports_update_from_returning_source_origin() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-rowdesc-origin-update-from".to_owned()),
            options: std::collections::BTreeMap::new(),
            credential: Credential::Anonymous {
                user: "test".to_owned(),
            },
            transport: TransportInfo::in_process(),
        })
        .expect("startup seed session");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE t_rowdesc_update_target (id INT, src_id INT); \
             CREATE TABLE t_rowdesc_update_source (id INT, payload TEXT)",
        )
        .expect("create update-from tables");
    let expected_oid = match engine
        .execute_sql(
            &session,
            "SELECT oid FROM pg_class WHERE relname = 't_rowdesc_update_source'",
        )
        .expect("lookup pg_class oid")
        .into_iter()
        .next()
        .expect("oid row")
    {
        StatementResult::Query { rows, .. } => match rows
            .first()
            .and_then(|row| row.values.first())
            .expect("oid value")
        {
            Value::Int(oid) => u32::try_from(*oid).expect("positive oid"),
            other => panic!("expected oid int, got {other:?}"),
        },
        other => panic!("expected oid query result, got {other:?}"),
    };

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_update_from_origin",
        "UPDATE t_rowdesc_update_target AS t \
         SET src_id = s.id \
         FROM t_rowdesc_update_source AS s \
         WHERE t.src_id = s.id \
         RETURNING s.payload",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_update_from_origin"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe update from returning source statement");

    let origin_info = backend_messages(conn.writer_ref())
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| parse_row_description_origin_info(payload))
        .expect("row description");
    assert_eq!(origin_info, vec![(expected_oid, 2)]);
}

#[tokio::test]
async fn extended_describe_statement_reports_delete_using_returning_source_origin() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-rowdesc-origin-delete-using".to_owned()),
            options: std::collections::BTreeMap::new(),
            credential: Credential::Anonymous {
                user: "test".to_owned(),
            },
            transport: TransportInfo::in_process(),
        })
        .expect("startup seed session");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE t_rowdesc_delete_target (id INT, src_id INT); \
             CREATE TABLE t_rowdesc_delete_source (id INT, payload TEXT)",
        )
        .expect("create delete-using tables");
    let expected_oid = match engine
        .execute_sql(
            &session,
            "SELECT oid FROM pg_class WHERE relname = 't_rowdesc_delete_source'",
        )
        .expect("lookup pg_class oid")
        .into_iter()
        .next()
        .expect("oid row")
    {
        StatementResult::Query { rows, .. } => match rows
            .first()
            .and_then(|row| row.values.first())
            .expect("oid value")
        {
            Value::Int(oid) => u32::try_from(*oid).expect("positive oid"),
            other => panic!("expected oid int, got {other:?}"),
        },
        other => panic!("expected oid query result, got {other:?}"),
    };

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_delete_using_origin",
        "DELETE FROM t_rowdesc_delete_target AS t \
         USING t_rowdesc_delete_source AS s \
         WHERE t.src_id = s.id \
         RETURNING s.payload",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_delete_using_origin"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe delete using returning source statement");

    let origin_info = backend_messages(conn.writer_ref())
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| parse_row_description_origin_info(payload))
        .expect("row description");
    assert_eq!(origin_info, vec![(expected_oid, 2)]);
}

#[tokio::test]
async fn extended_describe_statement_leaves_expression_origin_empty() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-rowdesc-origin-expr".to_owned()),
            options: std::collections::BTreeMap::new(),
            credential: Credential::Anonymous {
                user: "test".to_owned(),
            },
            transport: TransportInfo::in_process(),
        })
        .expect("startup seed session");
    engine
        .execute_sql(&session, "CREATE TABLE t_rowdesc_origin_expr (id INT)")
        .expect("create expression origin table");

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_expr",
        "SELECT id + 1 FROM t_rowdesc_origin_expr",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_expr"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run().await.expect("describe expression statement");

    let origin_info = backend_messages(conn.writer_ref())
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| parse_row_description_origin_info(payload))
        .expect("row description");
    assert_eq!(origin_info, vec![(0, 0)]);
}

#[tokio::test]
async fn extended_describe_statement_preserves_explicit_varchar_param_oid() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s_varchar_hint", "SELECT $1", &[1043]));
    input.extend(build_describe_bytes(b'S', "s_varchar_hint"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should preserve explicit varchar param oid");

    let param_oids = backend_messages(conn.writer_ref())
        .iter()
        .find(|(tag, _)| *tag == b't')
        .map(|(_, payload)| parse_parameter_description_oids(payload))
        .expect("parameter description");
    assert_eq!(param_oids, vec![1043]);
}

#[tokio::test]
async fn extended_describe_statement_accepts_partial_parse_oid_list() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_partial_oids",
        "SELECT $1, $2::TEXT",
        &[23],
    ));
    input.extend(build_describe_bytes(b'S', "s_partial_oids"));
    input.extend(build_bind_bytes(
        "p_partial_oids",
        "s_partial_oids",
        &[0, 0],
        &[Some(b"42"), Some(b"hello")],
        &[],
    ));
    input.extend(build_execute_bytes("p_partial_oids", 0));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("partial Parse OID list should describe and execute");

    let messages = backend_messages(conn.writer_ref());
    let param_oids = messages
        .iter()
        .find(|(tag, _)| *tag == b't')
        .map(|(_, payload)| parse_parameter_description_oids(payload))
        .expect("parameter description");
    assert_eq!(param_oids, vec![23, 25]);

    let data_row = messages
        .iter()
        .find(|(tag, _)| *tag == b'D')
        .map(|(_, payload)| payload.as_slice())
        .expect("data row");
    assert_eq!(
        parse_data_row_columns(data_row),
        vec![Some(b"42".to_vec()), Some(b"hello".to_vec())]
    );
}

#[tokio::test]
async fn extended_describe_statement_preserves_explicit_bpchar_array_param_oid() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_bpchar_array_hint",
        "SELECT cardinality($1)",
        &[1014],
    ));
    input.extend(build_describe_bytes(b'S', "s_bpchar_array_hint"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should preserve explicit bpchar[] param oid");

    let param_oids = backend_messages(conn.writer_ref())
        .iter()
        .find(|(tag, _)| *tag == b't')
        .map(|(_, payload)| parse_parameter_description_oids(payload))
        .expect("parameter description");
    assert_eq!(param_oids, vec![1014]);
}

#[tokio::test]
async fn extended_describe_statement_preserves_explicit_varchar_cast_oid() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_varchar_cast",
        "SELECT CAST('abc' AS VARCHAR)",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_varchar_cast"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should preserve explicit varchar cast oid");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(1043, -1)]
    );
}

#[tokio::test]
async fn extended_describe_statement_preserves_explicit_char_cast_typmod() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_char_cast",
        "SELECT CAST('x' AS CHAR(3))",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_char_cast"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should preserve explicit char cast typmod");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(1042, 7)]
    );
}

#[tokio::test]
async fn extended_describe_statement_preserves_pg_catalog_fast_path_cast_metadata() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_pg_catalog_casts",
        "SELECT CAST(typname AS VARCHAR), CAST(typtype AS CHAR(3)) FROM pg_catalog.pg_type ORDER BY oid LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_catalog_casts"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("pg_catalog fast-path cast describe should succeed");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(1043, -1), (1042, 7)]
    );
}
