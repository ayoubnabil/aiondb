use crate::*;

// ---------------------------------------------------------------------------
// CREATE VIEW WITH options
// ---------------------------------------------------------------------------

#[test]
fn create_view_with_security_barrier_equals_true() {
    let stmt = parse_prepared_statement("CREATE VIEW v1 WITH (security_barrier=true) AS SELECT 1")
        .expect("parse");
    assert!(matches!(stmt, Statement::CreateView(_)));
}

#[test]
fn create_view_with_security_barrier_bare() {
    let stmt =
        parse_prepared_statement("CREATE VIEW v1 WITH (security_barrier) AS SELECT * FROM t")
            .expect("parse");
    assert!(matches!(stmt, Statement::CreateView(_)));
}

#[test]
fn create_view_with_security_invoker() {
    let stmt =
        parse_prepared_statement("CREATE VIEW v1 WITH (security_invoker) AS SELECT * FROM t")
            .expect("parse");
    assert!(matches!(stmt, Statement::CreateView(_)));
}

#[test]
fn create_view_with_check_option_trailing() {
    let stmt = parse_prepared_statement(
        "CREATE VIEW bv1 WITH (security_barrier) AS SELECT * FROM b1 WHERE a > 0 WITH CHECK OPTION",
    )
    .expect("parse");
    assert!(matches!(stmt, Statement::CreateView(_)));
}

// ---------------------------------------------------------------------------
// LOCK TABLE
// ---------------------------------------------------------------------------

#[test]
fn lock_table_access_share() {
    let stmt = parse_prepared_statement("LOCK TABLE t IN ACCESS SHARE MODE")
        .expect("LOCK TABLE should parse");
    assert!(matches!(stmt, Statement::Lock(_)));
}

#[test]
fn lock_table_exclusive() {
    let stmt = parse_prepared_statement("LOCK TABLE t IN EXCLUSIVE MODE")
        .expect("LOCK TABLE should parse");
    assert!(matches!(stmt, Statement::Lock(_)));
}

#[test]
fn lock_table_multiple() {
    let statements = parse_sql("LOCK TABLE t1; LOCK TABLE t2 IN ROW SHARE MODE;")
        .expect("LOCK TABLE statements should parse");
    assert_eq!(statements.len(), 2);
    assert!(matches!(&statements[0], Statement::Lock(_)));
    assert!(matches!(&statements[1], Statement::Lock(_)));
}

// ---------------------------------------------------------------------------
// DO $$ ... $$ anonymous block
// ---------------------------------------------------------------------------

#[test]
fn do_anonymous_block() {
    let stmt = parse_prepared_statement("DO $$ BEGIN RAISE NOTICE 'hello'; END $$").expect("parse");
    assert!(matches!(stmt, Statement::DoStmt { .. }));
}

#[test]
fn do_tagged_dollar() {
    let stmt = parse_prepared_statement("DO $body$ BEGIN RAISE NOTICE 'hello'; END $body$")
        .expect("parse");
    assert!(matches!(stmt, Statement::DoStmt { .. }));
}

#[test]
fn do_with_language() {
    let stmt =
        parse_prepared_statement("DO LANGUAGE plpgsql $$ BEGIN NULL; END $$").expect("parse");
    assert!(matches!(stmt, Statement::DoStmt { .. }));
}

// ---------------------------------------------------------------------------
// PREPARED transaction first-class AST nodes
// ---------------------------------------------------------------------------

#[test]
fn prepare_transaction_is_typed() {
    let stmt = parse_prepared_statement("PREPARE TRANSACTION 'gid-1'")
        .expect("PREPARE TRANSACTION should parse");
    assert!(matches!(stmt, Statement::PrepareTransaction { ref gid, .. } if gid == "gid-1"));
}

#[test]
fn commit_prepared_is_typed() {
    let stmt =
        parse_prepared_statement("COMMIT PREPARED 'gid-1'").expect("COMMIT PREPARED should parse");
    assert!(matches!(stmt, Statement::CommitPrepared { ref gid, .. } if gid == "gid-1"));
}

#[test]
fn rollback_prepared_is_typed() {
    let stmt = parse_prepared_statement("ROLLBACK PREPARED 'gid-1'")
        .expect("ROLLBACK PREPARED should parse");
    assert!(matches!(
        stmt,
        Statement::RollbackPrepared { ref gid, .. } if gid == "gid-1"
    ));
}

// ---------------------------------------------------------------------------
// Compat DDL forms handled by typed parser variants
// ---------------------------------------------------------------------------

#[test]
fn create_statistics_is_typed_compat() {
    let stmt = parse_prepared_statement("CREATE STATISTICS s1 ON a, b FROM t1").expect("parse");
    assert!(matches!(stmt, Statement::CreateStatistics(_)));
}

#[test]
fn drop_statistics_is_typed_compat() {
    let stmt = parse_prepared_statement("DROP STATISTICS IF EXISTS s1").expect("parse");
    assert!(matches!(stmt, Statement::DropStatistics(_)));
}

#[test]
fn alter_statistics_is_typed_compat() {
    let stmt = parse_prepared_statement("ALTER STATISTICS s1 RENAME TO s2").expect("parse");
    assert!(matches!(stmt, Statement::AlterStatistics(_)));
}

#[test]
fn drop_tablespace_is_typed_compat() {
    let stmt = parse_prepared_statement("DROP TABLESPACE IF EXISTS ts1").expect("parse");
    assert!(matches!(stmt, Statement::DropTablespace(_)));
}

#[test]
fn alter_tablespace_is_typed_compat() {
    let stmt = parse_prepared_statement("ALTER TABLESPACE ts1 RENAME TO ts2").expect("parse");
    assert!(matches!(stmt, Statement::AlterTablespace(_)));
}

#[test]
fn sensitive_misc_drop_alter_forms_are_typed_compat() {
    for (sql, expected) in [
        ("DROP POLICY IF EXISTS p1", "drop policy"),
        ("ALTER POLICY p1 ON t RENAME TO p2", "alter policy"),
        ("DROP PUBLICATION IF EXISTS pub1", "drop publication"),
        ("ALTER PUBLICATION pub1 OWNER TO alice", "alter publication"),
        ("DROP SUBSCRIPTION IF EXISTS sub1", "drop subscription"),
        (
            "ALTER SUBSCRIPTION sub1 OWNER TO alice",
            "alter subscription",
        ),
        ("DROP SERVER IF EXISTS srv1", "drop server"),
        ("ALTER SERVER srv1 OWNER TO alice", "alter server"),
        ("DROP COLLATION IF EXISTS coll1", "drop collation"),
        ("ALTER COLLATION coll1 OWNER TO alice", "alter collation"),
        ("CREATE FOREIGN DATA WRAPPER fdw1", "create fdw"),
        (
            "ALTER FOREIGN DATA WRAPPER fdw1 OWNER TO alice",
            "alter fdw",
        ),
        ("DROP FOREIGN DATA WRAPPER IF EXISTS fdw1", "drop fdw"),
        (
            "ALTER USER MAPPING FOR alice SERVER srv1 OPTIONS (SET x 'y')",
            "alter user mapping",
        ),
        (
            "DROP USER MAPPING FOR alice SERVER srv1",
            "drop user mapping",
        ),
        (
            "ALTER FOREIGN TABLE ft1 OWNER TO alice",
            "alter foreign table",
        ),
        ("DROP FOREIGN TABLE IF EXISTS ft1", "drop foreign table"),
        ("ALTER CONVERSION myconv OWNER TO alice", "compat tagged"),
        ("ALTER ACCESS METHOD myam OWNER TO alice", "compat tagged"),
        ("ALTER TRANSFORM mytrans OWNER TO alice", "compat tagged"),
        ("ALTER EVENT TRIGGER my_et DISABLE", "compat tagged"),
    ] {
        let stmt = parse_prepared_statement(sql).expect(sql);
        match expected {
            "drop policy" => assert!(matches!(stmt, Statement::DropPolicy(_))),
            "alter policy" => assert!(matches!(stmt, Statement::AlterPolicy(_))),
            "drop publication" => assert!(matches!(stmt, Statement::DropPublication(_))),
            "alter publication" => assert!(matches!(stmt, Statement::AlterPublication(_))),
            "drop subscription" => assert!(matches!(stmt, Statement::DropSubscription(_))),
            "alter subscription" => assert!(matches!(stmt, Statement::AlterSubscription(_))),
            "drop server" => assert!(matches!(stmt, Statement::DropServer(_))),
            "alter server" => assert!(matches!(stmt, Statement::AlterServer(_))),
            "drop collation" => assert!(matches!(stmt, Statement::DropCollation(_))),
            "alter collation" => assert!(matches!(stmt, Statement::AlterCollation(_))),
            "create fdw" => assert!(matches!(stmt, Statement::CreateForeignDataWrapper(_))),
            "alter fdw" => assert!(matches!(stmt, Statement::AlterForeignDataWrapper(_))),
            "drop fdw" => assert!(matches!(stmt, Statement::DropForeignDataWrapper(_))),
            "alter user mapping" => assert!(matches!(stmt, Statement::AlterUserMapping(_))),
            "drop user mapping" => assert!(matches!(stmt, Statement::DropUserMapping(_))),
            "alter foreign table" => assert!(matches!(stmt, Statement::AlterForeignTable(_))),
            "drop foreign table" => assert!(matches!(stmt, Statement::DropForeignTable(_))),
            "compat tagged" => assert!(matches!(stmt, Statement::CompatTagged(_))),
            _ => unreachable!(),
        }
    }

    for sql in [
        "CREATE CONVERSION myconv FOR 'LATIN1' TO 'UTF8' FROM f",
        "DROP CONVERSION myconv",
        "CREATE ACCESS METHOD myam TYPE TABLE HANDLER h",
        "DROP ACCESS METHOD myam",
        "CREATE TRANSFORM mytrans",
        "DROP TRANSFORM mytrans",
        "CREATE EVENT TRIGGER my_et ON ddl_command_start EXECUTE PROCEDURE f()",
        "DROP EVENT TRIGGER my_et",
    ] {
        assert!(parse_prepared_statement(sql).is_err(), "{sql}");
    }
}

// ---------------------------------------------------------------------------
// ALTER TYPE ... ADD VALUE
// ---------------------------------------------------------------------------

#[test]
fn alter_type_add_value() {
    let stmt = parse_prepared_statement("ALTER TYPE mood ADD VALUE 'glad'").expect("parse");
    assert!(matches!(stmt, Statement::AlterType(_)));
    assert_eq!(stmt.compat_tag(), Some("ALTER TYPE"));
}

#[test]
fn alter_type_add_value_if_not_exists() {
    let stmt =
        parse_prepared_statement("ALTER TYPE mood ADD VALUE IF NOT EXISTS 'glad'").expect("parse");
    assert!(matches!(stmt, Statement::AlterType(_)));
    assert_eq!(stmt.compat_tag(), Some("ALTER TYPE"));
}

#[test]
fn alter_type_rename_value() {
    let stmt =
        parse_prepared_statement("ALTER TYPE mood RENAME VALUE 'ok' TO 'meh'").expect("parse");
    assert!(matches!(stmt, Statement::AlterType(_)));
    assert_eq!(stmt.compat_tag(), Some("ALTER TYPE"));
}

// ---------------------------------------------------------------------------
// COMMENT ON
// ---------------------------------------------------------------------------

#[test]
fn comment_on_table() {
    let stmt = parse_prepared_statement("COMMENT ON TABLE t IS 'description'").expect("parse");
    assert!(
        matches!(stmt, Statement::Comment(ref c) if c.object_type == "TABLE" && c.text.as_deref() == Some("description"))
    );
}

#[test]
fn comment_on_column() {
    let stmt =
        parse_prepared_statement("COMMENT ON COLUMN t.col IS 'column description'").expect("parse");
    assert!(
        matches!(stmt, Statement::Comment(ref c) if c.object_type == "COLUMN" && c.text.as_deref() == Some("column description"))
    );
}

#[test]
fn comment_on_function() {
    let stmt =
        parse_prepared_statement("COMMENT ON FUNCTION myfunc(int) IS 'does stuff'").expect("parse");
    assert!(
        matches!(stmt, Statement::Comment(ref c) if c.object_type == "FUNCTION" && c.text.as_deref() == Some("does stuff"))
    );
}

#[test]
fn comment_on_null() {
    let stmt = parse_prepared_statement("COMMENT ON TABLE t IS NULL").expect("parse");
    assert!(
        matches!(stmt, Statement::Comment(ref c) if c.object_type == "TABLE" && c.text.is_none())
    );
}

// ---------------------------------------------------------------------------
// PREPARE / EXECUTE / DEALLOCATE
// ---------------------------------------------------------------------------

#[test]
fn prepare_statement() {
    let stmt = parse_prepared_statement("PREPARE stmt AS SELECT 1").expect("parse");
    assert!(matches!(stmt, Statement::PrepareStmt { .. }));
}

#[test]
fn prepare_with_types() {
    let stmt = parse_prepared_statement("PREPARE stmt(int, text) AS SELECT $1, $2").expect("parse");
    assert!(matches!(stmt, Statement::PrepareStmt { .. }));
}

#[test]
fn execute_simple() {
    let stmt = parse_prepared_statement("EXECUTE stmt").expect("parse");
    assert!(matches!(stmt, Statement::ExecuteStmt { .. }));
}

#[test]
fn execute_with_params() {
    let stmt = parse_prepared_statement("EXECUTE stmt(1, 'hello')").expect("parse");
    assert!(matches!(stmt, Statement::ExecuteStmt { .. }));
}

#[test]
fn deallocate_simple() {
    let stmt = parse_prepared_statement("DEALLOCATE stmt").expect("parse");
    assert!(matches!(stmt, Statement::DeallocateStmt { .. }));
}

#[test]
fn deallocate_all() {
    let stmt = parse_prepared_statement("DEALLOCATE ALL").expect("parse");
    assert!(matches!(stmt, Statement::DeallocateStmt { .. }));
}

#[test]
fn declare_statement() {
    let stmt = parse_prepared_statement("DECLARE c CURSOR FOR SELECT 1").expect("parse");
    assert!(matches!(stmt, Statement::DeclareStmt { .. }));
}

#[test]
fn fetch_statement() {
    let stmt = parse_prepared_statement("FETCH ALL FROM c").expect("parse");
    assert!(matches!(stmt, Statement::FetchStmt { .. }));
}

#[test]
fn move_statement() {
    let stmt = parse_prepared_statement("MOVE NEXT FROM c").expect("parse");
    assert!(matches!(stmt, Statement::MoveStmt { .. }));
}

#[test]
fn close_statement() {
    let stmt = parse_prepared_statement("CLOSE c").expect("parse");
    assert!(matches!(stmt, Statement::CloseStmt { .. }));
}

// ---------------------------------------------------------------------------
// LISTEN / NOTIFY / UNLISTEN
// ---------------------------------------------------------------------------

#[test]
fn listen() {
    let stmt = parse_prepared_statement("LISTEN channel").expect("LISTEN should parse");
    assert!(matches!(stmt, Statement::Listen { .. }));
}

#[test]
fn notify() {
    let stmt = parse_prepared_statement("NOTIFY channel").expect("NOTIFY should parse");
    assert!(matches!(stmt, Statement::Notify { .. }));
}

#[test]
fn notify_with_payload() {
    let stmt = parse_prepared_statement("NOTIFY channel, 'payload'").expect("NOTIFY should parse");
    assert!(matches!(stmt, Statement::Notify { .. }));
}

#[test]
fn unlisten_star() {
    let stmt = parse_prepared_statement("UNLISTEN *").expect("UNLISTEN should parse");
    assert!(matches!(stmt, Statement::Unlisten { channel: None, .. }));
}

#[test]
fn unlisten_channel() {
    let stmt = parse_prepared_statement("UNLISTEN channel").expect("UNLISTEN should parse");
    assert!(matches!(
        stmt,
        Statement::Unlisten {
            channel: Some(_),
            ..
        }
    ));
}

// ---------------------------------------------------------------------------
// DISCARD
// ---------------------------------------------------------------------------

#[test]
fn discard_all() {
    let stmt = parse_prepared_statement("DISCARD ALL").expect("parse");
    assert!(matches!(
        stmt,
        Statement::Discard(crate::ast::DiscardStatement {
            target: crate::ast::DiscardTarget::All,
            ..
        })
    ));
}

#[test]
fn discard_temp() {
    let stmt = parse_prepared_statement("DISCARD TEMP").expect("parse");
    assert!(matches!(
        stmt,
        Statement::Discard(crate::ast::DiscardStatement {
            target: crate::ast::DiscardTarget::Temporary,
            ..
        })
    ));
}

#[test]
fn discard_temporary() {
    let stmt = parse_prepared_statement("DISCARD TEMPORARY").expect("parse");
    assert!(matches!(
        stmt,
        Statement::Discard(crate::ast::DiscardStatement {
            target: crate::ast::DiscardTarget::Temporary,
            ..
        })
    ));
}

#[test]
fn discard_plans() {
    let stmt = parse_prepared_statement("DISCARD PLANS").expect("parse");
    assert!(matches!(
        stmt,
        Statement::Discard(crate::ast::DiscardStatement {
            target: crate::ast::DiscardTarget::Plans,
            ..
        })
    ));
}

#[test]
fn discard_sequences() {
    let stmt = parse_prepared_statement("DISCARD SEQUENCES").expect("parse");
    assert!(matches!(
        stmt,
        Statement::Discard(crate::ast::DiscardStatement {
            target: crate::ast::DiscardTarget::Sequences,
            ..
        })
    ));
}

// ---------------------------------------------------------------------------
// Task #12 - parser strictness: obvious garbage must produce a syntax
// ---------------------------------------------------------------------------

#[test]
fn parser_rejects_bare_unknown_identifier() {
    // `WOBBLE` is not a statement-starting keyword; must be a syntax error,
    // not a compatibility stub.
    let err = parse_prepared_statement("WOBBLE everything")
        .expect_err("bare unknown identifier must be rejected");
    let report = match err {
        aiondb_core::DbError::Parse(report) => report,
        other => panic!("expected Parse error, got {other:?}"),
    };
    assert_eq!(report.sqlstate, aiondb_core::SqlState::SyntaxError);
}

#[test]
fn parser_rejects_truncated_drop() {
    let err = parse_prepared_statement("DROP").expect_err("truncated DROP must be rejected");
    assert!(matches!(err, aiondb_core::DbError::Parse(_)));
}

#[test]
fn parser_rejects_half_create_not_silent_noop() {
    let err = parse_prepared_statement("CREATE SOMETHING").expect_err("partial CREATE must error");
    assert!(matches!(err, aiondb_core::DbError::Parse(_)));
}
