use super::*;

#[tokio::test]
async fn extended_describe_statement_preserves_direct_oid_param_oid() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s_oid_param", "SELECT $1", &[26]));
    input.extend(build_describe_bytes(b'S', "s_oid_param"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should preserve direct oid parameter oid");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(26, -1)]
    );
}

#[tokio::test]
async fn extended_describe_statement_preserves_direct_regproc_param_oid() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s_regproc_param", "SELECT $1", &[24]));
    input.extend(build_describe_bytes(b'S', "s_regproc_param"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should preserve direct regproc parameter oid");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(24, -1)]
    );
}

#[tokio::test]
async fn extended_describe_statement_does_not_preserve_param_alias_across_explicit_cast() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_casted_varchar_param",
        "SELECT $1::TEXT",
        &[1043],
    ));
    input.extend(build_describe_bytes(b'S', "s_casted_varchar_param"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement with explicit cast should succeed");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(25, -1)]
    );
}

#[tokio::test]
async fn extended_describe_portal_preserves_direct_varchar_param_oid() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s_varchar_portal", "SELECT $1", &[1043]));
    input.extend(build_bind_bytes(
        "p_varchar_portal",
        "s_varchar_portal",
        &[0],
        &[Some(b"hello")],
        &[],
    ));
    input.extend(build_describe_bytes(b'P', "p_varchar_portal"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe portal should preserve direct varchar parameter oid");

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
async fn extended_describe_portal_reports_direct_column_origin() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-rowdesc-origin-portal".to_owned()),
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
            "CREATE TABLE t_rowdesc_origin_portal (id INT, name TEXT)",
        )
        .expect("create portal origin table");
    let expected_oid = match engine
        .execute_sql(
            &session,
            "SELECT oid FROM pg_class WHERE relname = 't_rowdesc_origin_portal'",
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
        "s_portal",
        "SELECT name FROM t_rowdesc_origin_portal",
        &[],
    ));
    input.extend(build_bind_bytes("p_portal", "s_portal", &[], &[], &[]));
    input.extend(build_describe_bytes(b'P', "p_portal"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run().await.expect("describe direct column portal");

    let origin_info = backend_messages(conn.writer_ref())
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| parse_row_description_origin_info(payload))
        .expect("row description");
    assert_eq!(origin_info, vec![(expected_oid, 2)]);
}

#[tokio::test]
async fn extended_describe_portal_reports_update_from_returning_source_origin() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-rowdesc-origin-portal-update-from".to_owned()),
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
            "CREATE TABLE t_rowdesc_portal_update_target (id INT, src_id INT); \
             CREATE TABLE t_rowdesc_portal_update_source (id INT, payload TEXT)",
        )
        .expect("create portal update-from tables");
    let expected_oid = match engine
        .execute_sql(
            &session,
            "SELECT oid FROM pg_class WHERE relname = 't_rowdesc_portal_update_source'",
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
        "s_portal_update_from_origin",
        "UPDATE t_rowdesc_portal_update_target AS t \
         SET src_id = s.id \
         FROM t_rowdesc_portal_update_source AS s \
         WHERE t.src_id = s.id \
         RETURNING s.payload",
        &[],
    ));
    input.extend(build_bind_bytes(
        "p_portal_update_from_origin",
        "s_portal_update_from_origin",
        &[],
        &[],
        &[],
    ));
    input.extend(build_describe_bytes(b'P', "p_portal_update_from_origin"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe portal update from returning source");

    let origin_info = backend_messages(conn.writer_ref())
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| parse_row_description_origin_info(payload))
        .expect("row description");
    assert_eq!(origin_info, vec![(expected_oid, 2)]);
}

#[tokio::test]
async fn extended_describe_portal_reports_delete_using_returning_source_origin() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pgwire-rowdesc-origin-portal-delete-using".to_owned()),
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
            "CREATE TABLE t_rowdesc_portal_delete_target (id INT, src_id INT); \
             CREATE TABLE t_rowdesc_portal_delete_source (id INT, payload TEXT)",
        )
        .expect("create portal delete-using tables");
    let expected_oid = match engine
        .execute_sql(
            &session,
            "SELECT oid FROM pg_class WHERE relname = 't_rowdesc_portal_delete_source'",
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
        "s_portal_delete_using_origin",
        "DELETE FROM t_rowdesc_portal_delete_target AS t \
         USING t_rowdesc_portal_delete_source AS s \
         WHERE t.src_id = s.id \
         RETURNING s.payload",
        &[],
    ));
    input.extend(build_bind_bytes(
        "p_portal_delete_using_origin",
        "s_portal_delete_using_origin",
        &[],
        &[],
        &[],
    ));
    input.extend(build_describe_bytes(b'P', "p_portal_delete_using_origin"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe portal delete using returning source");

    let origin_info = backend_messages(conn.writer_ref())
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| parse_row_description_origin_info(payload))
        .expect("row description");
    assert_eq!(origin_info, vec![(expected_oid, 2)]);
}

#[tokio::test]
async fn extended_describe_portal_preserves_direct_regproc_param_oid() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s_regproc_portal", "SELECT $1", &[24]));
    input.extend(build_bind_bytes(
        "p_regproc_portal",
        "s_regproc_portal",
        &[0],
        &[Some(b"123")],
        &[],
    ));
    input.extend(build_describe_bytes(b'P', "p_regproc_portal"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe portal should preserve direct regproc parameter oid");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(24, -1)]
    );
}

#[tokio::test]
async fn extended_describe_statement_preserves_direct_reg_alias_param_oids() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_reg_alias_params",
        "SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9, $10",
        &[2202, 2203, 2204, 2205, 2206, 3734, 3769, 4089, 4096, 4191],
    ));
    input.extend(build_describe_bytes(b'S', "s_reg_alias_params"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should preserve direct reg* parameter oids");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![
            (2202, -1),
            (2203, -1),
            (2204, -1),
            (2205, -1),
            (2206, -1),
            (3734, -1),
            (3769, -1),
            (4089, -1),
            (4096, -1),
            (4191, -1),
        ]
    );
}

#[tokio::test]
async fn extended_describe_statement_preserves_direct_regclass_array_param_oid() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_regclass_array_param",
        "SELECT $1",
        &[2210],
    ));
    input.extend(build_describe_bytes(b'S', "s_regclass_array_param"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should preserve direct regclass[] parameter oid");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(2210, -1)]
    );
}

#[tokio::test]
async fn extended_describe_portal_preserves_direct_regclass_param_oid() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes("s_regclass_portal", "SELECT $1", &[2205]));
    input.extend(build_bind_bytes(
        "p_regclass_portal",
        "s_regclass_portal",
        &[0],
        &[Some(b"123")],
        &[],
    ));
    input.extend(build_describe_bytes(b'P', "p_regclass_portal"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe portal should preserve direct regclass parameter oid");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(2205, -1)]
    );
}

#[tokio::test]
async fn extended_describe_statement_preserves_mixed_direct_param_alias_oids() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_mixed_param_aliases",
        "SELECT $1, $2, $3, $4",
        &[19, 1043, 1015, 26],
    ));
    input.extend(build_describe_bytes(b'S', "s_mixed_param_aliases"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should preserve mixed direct parameter aliases");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(19, -1), (1043, -1), (1015, -1), (26, -1)]
    );
}

#[tokio::test]
async fn extended_describe_statement_preserves_pg_language_name_oid() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_pg_language_name",
        "SELECT lanname FROM pg_catalog.pg_language ORDER BY oid LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_language_name"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should preserve pg_language name oid");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(19, -1)]
    );
}

#[tokio::test]
async fn extended_describe_statement_preserves_pg_database_acl_array_oid() {
    let engine = Arc::new(EngineBuilder::for_testing().build().expect("build engine"));

    let mut input = build_startup_bytes();
    input.extend(build_parse_bytes(
        "s_pg_database_acl",
        "SELECT datacl FROM pg_catalog.pg_database LIMIT 1",
        &[],
    ));
    input.extend(build_describe_bytes(b'S', "s_pg_database_acl"));
    input.extend(build_sync_bytes());
    input.extend(build_terminate_bytes());

    let reader = Cursor::new(input);
    let writer: Vec<u8> = Vec::new();
    let mut conn = Connection::new(engine, reader, writer, 1, 42, CancelRegistry::new());
    conn.run()
        .await
        .expect("describe statement should preserve pg_database acl array oid");

    let messages = backend_messages(conn.writer_ref());
    let row_description = messages
        .iter()
        .find(|(tag, _)| *tag == b'T')
        .map(|(_, payload)| payload.as_slice())
        .expect("row description");
    assert_eq!(
        parse_row_description_type_info(row_description),
        vec![(1009, -1)]
    );
}
