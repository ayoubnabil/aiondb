use super::*;

// ===================================================================
// plan() API - DDL through public interface
// ===================================================================

#[test]
fn plan_create_table() {
    let planner = Planner::default();
    let plan =
        plan_with_catalog("CREATE TABLE t (id INT NOT NULL, val TEXT)", &planner).expect("plan");
    assert!(matches!(plan, LogicalPlan::CreateTable { .. }));
}

#[test]
fn plan_create_table_uses_default_schema() {
    let planner = Planner::default();
    let stmt = parse_prepared_statement("CREATE TABLE items (id INT NOT NULL)").expect("parse");
    let plan = planner
        .plan(PlanRequest {
            statement: &stmt,
            txn_id: TxnId::default(),
            default_schema: Some("tenant_acme".to_owned()),
            current_user: None,
            session_user: None,
            database_name: None,
            datestyle: None,
            timezone: None,
        })
        .expect("plan");

    match plan {
        LogicalPlan::CreateTable { relation_name, .. } => {
            assert_eq!(relation_name, "tenant_acme.items");
        }
        other => panic!("expected CreateTable, got {other:?}"),
    }
}

#[test]
fn plan_create_sequence() {
    let planner = Planner::default();
    let plan = plan_with_catalog("CREATE SEQUENCE my_seq", &planner).expect("plan");
    assert!(matches!(plan, LogicalPlan::CreateSequence { .. }));
}

#[test]
fn plan_pg_object_utility_commands() {
    let planner = Planner::default();
    let cases = [
        (
            "CREATE TYPE mood AS ENUM ('sad', 'ok')",
            aiondb_plan::PgObjectAction::Create,
            aiondb_plan::PgObjectKind::Type,
            "CREATE TYPE",
        ),
        (
            "DROP DOMAIN IF EXISTS positive_int",
            aiondb_plan::PgObjectAction::Drop,
            aiondb_plan::PgObjectKind::Domain,
            "DROP DOMAIN",
        ),
        (
            "ALTER PUBLICATION pub SET (publish = 'insert')",
            aiondb_plan::PgObjectAction::Alter,
            aiondb_plan::PgObjectKind::Publication,
            "ALTER PUBLICATION",
        ),
        (
            "CREATE FOREIGN TABLE ft (id int) SERVER srv",
            aiondb_plan::PgObjectAction::Create,
            aiondb_plan::PgObjectKind::ForeignTable,
            "CREATE FOREIGN TABLE",
        ),
    ];

    for (sql, expected_action, expected_kind, expected_tag) in cases {
        let plan = plan_with_catalog(sql, &planner).expect("plan");
        match plan {
            LogicalPlan::PgObjectCommand {
                action, kind, tag, ..
            } => {
                assert_eq!(action, expected_action);
                assert_eq!(kind, expected_kind);
                assert_eq!(tag, expected_tag);
            }
            other => panic!("expected PgObjectCommand for {sql}, got {other:?}"),
        }
    }
}

/// Dual-mode coverage for every compatibility family. The contract: each
/// tag now has a typed AST variant that routes through the binder to
/// `LogicalPlan::PgObjectCommand`, never back to a parser-stub shortcut.
#[test]
fn every_compat_family_routes_through_pg_object_command() {
    use aiondb_plan::{PgObjectAction, PgObjectKind};
    let planner = Planner::default();
    let cases: &[(&str, PgObjectAction, PgObjectKind, &str)] = &[
        // TYPE family.
        (
            "CREATE TYPE color AS ENUM ('r','g','b')",
            PgObjectAction::Create,
            PgObjectKind::Type,
            "CREATE TYPE",
        ),
        (
            "ALTER TYPE color ADD VALUE 'y'",
            PgObjectAction::Alter,
            PgObjectKind::Type,
            "ALTER TYPE",
        ),
        (
            "DROP TYPE color",
            PgObjectAction::Drop,
            PgObjectKind::Type,
            "DROP TYPE",
        ),
        // DOMAIN family.
        (
            "CREATE DOMAIN pos_int AS INTEGER CHECK (VALUE > 0)",
            PgObjectAction::Create,
            PgObjectKind::Domain,
            "CREATE DOMAIN",
        ),
        (
            "ALTER DOMAIN pos_int DROP NOT NULL",
            PgObjectAction::Alter,
            PgObjectKind::Domain,
            "ALTER DOMAIN",
        ),
        (
            "DROP DOMAIN pos_int",
            PgObjectAction::Drop,
            PgObjectKind::Domain,
            "DROP DOMAIN",
        ),
        // RULE family.
        (
            "CREATE RULE r_ins AS ON INSERT TO t DO INSTEAD NOTHING",
            PgObjectAction::Create,
            PgObjectKind::Rule,
            "CREATE RULE",
        ),
        (
            "DROP RULE r_ins ON t",
            PgObjectAction::Drop,
            PgObjectKind::Rule,
            "DROP RULE",
        ),
        // POLICY / PUBLICATION / SUBSCRIPTION / SERVER / USER MAPPING / FOREIGN TABLE
        (
            "CREATE POLICY p ON t FOR SELECT USING (true)",
            PgObjectAction::Create,
            PgObjectKind::Policy,
            "CREATE POLICY",
        ),
        (
            "DROP POLICY p ON t",
            PgObjectAction::Drop,
            PgObjectKind::Policy,
            "DROP POLICY",
        ),
        (
            "CREATE PUBLICATION pub FOR ALL TABLES",
            PgObjectAction::Create,
            PgObjectKind::Publication,
            "CREATE PUBLICATION",
        ),
        (
            "DROP PUBLICATION pub",
            PgObjectAction::Drop,
            PgObjectKind::Publication,
            "DROP PUBLICATION",
        ),
        (
            "CREATE SUBSCRIPTION sub CONNECTION 'host=x' PUBLICATION pub",
            PgObjectAction::Create,
            PgObjectKind::Subscription,
            "CREATE SUBSCRIPTION",
        ),
        (
            "DROP SUBSCRIPTION sub",
            PgObjectAction::Drop,
            PgObjectKind::Subscription,
            "DROP SUBSCRIPTION",
        ),
        (
            "CREATE SERVER s FOREIGN DATA WRAPPER fdw",
            PgObjectAction::Create,
            PgObjectKind::Server,
            "CREATE SERVER",
        ),
        (
            "DROP SERVER s",
            PgObjectAction::Drop,
            PgObjectKind::Server,
            "DROP SERVER",
        ),
        (
            "CREATE USER MAPPING FOR alice SERVER s",
            PgObjectAction::Create,
            PgObjectKind::UserMapping,
            "CREATE USER MAPPING",
        ),
        (
            "DROP USER MAPPING FOR alice SERVER s",
            PgObjectAction::Drop,
            PgObjectKind::UserMapping,
            "DROP USER MAPPING",
        ),
        (
            "CREATE FOREIGN TABLE ft (id int) SERVER s",
            PgObjectAction::Create,
            PgObjectKind::ForeignTable,
            "CREATE FOREIGN TABLE",
        ),
        (
            "DROP FOREIGN TABLE ft",
            PgObjectAction::Drop,
            PgObjectKind::ForeignTable,
            "DROP FOREIGN TABLE",
        ),
        // Misc-object families.
        (
            "CREATE COLLATION c (locale = 'en_US')",
            PgObjectAction::Create,
            PgObjectKind::Collation,
            "CREATE COLLATION",
        ),
        (
            "DROP COLLATION c",
            PgObjectAction::Drop,
            PgObjectKind::Collation,
            "DROP COLLATION",
        ),
        (
            "CREATE STATISTICS s ON a FROM t",
            PgObjectAction::Create,
            PgObjectKind::Statistics,
            "CREATE STATISTICS",
        ),
        (
            "DROP STATISTICS s",
            PgObjectAction::Drop,
            PgObjectKind::Statistics,
            "DROP STATISTICS",
        ),
        (
            "CREATE TABLESPACE ts LOCATION '/var/ts'",
            PgObjectAction::Create,
            PgObjectKind::Tablespace,
            "CREATE TABLESPACE",
        ),
        (
            "DROP TABLESPACE ts",
            PgObjectAction::Drop,
            PgObjectKind::Tablespace,
            "DROP TABLESPACE",
        ),
    ];

    for (sql, expected_action, expected_kind, expected_tag) in cases {
        let plan = plan_with_catalog(sql, &planner)
            .unwrap_or_else(|err| panic!("plan {sql:?} failed: {err}"));
        match plan {
            LogicalPlan::PgObjectCommand {
                action, kind, tag, ..
            } => {
                assert_eq!(&action, expected_action, "action mismatch for {sql}");
                assert_eq!(&kind, expected_kind, "kind mismatch for {sql}");
                assert_eq!(&tag, expected_tag, "tag mismatch for {sql}");
            }
            LogicalPlan::InternalNoOp { .. } | LogicalPlan::PgCompatUtility { .. } => {
                panic!(
                    "compat family {sql:?} reached the plan as a NoOp/Utility; it must route through PgObjectCommand"
                );
            }
            other => panic!("expected PgObjectCommand for {sql}, got {other:?}"),
        }
    }
}

/// A parser-emitted compatibility stub must never bind to a plan.
/// Compatibility tags are intercepted by the engine router before planning,
/// so such a stub reaching this layer signals a routing bug. The binder
#[test]
fn compat_parser_stub_rejected_at_planner() {
    let planner = Planner::default();
    let stmt = Statement::CompatParserStub {
        tag: "CREATE EVENT TRIGGER".to_owned(),
        notice: None,
        span: Span::default(),
    };
    let err = plan_statement_with_catalog(&stmt, &planner)
        .expect_err("compat parser stub must not produce a plan");
    assert_eq!(err.sqlstate(), SqlState::FeatureNotSupported);
    assert!(
        err.to_string().contains("CREATE EVENT TRIGGER"),
        "error must echo the offending tag; got {err}"
    );
}

/// `Statement::CompatTagged` / `CompatTaggedNotice` are shadow statements
/// planner either; the default catch-all surfaces a hard error rather than
#[test]
fn compat_tagged_rejected_at_planner() {
    use aiondb_parser::ast::CompatTaggedStatement;
    let planner = Planner::default();
    let stmt = Statement::CompatTagged(CompatTaggedStatement {
        tag: "CREATE OPERATOR".to_owned(),
        raw_sql: "CREATE OPERATOR + (LEFTARG=int, RIGHTARG=int, PROCEDURE=int4pl)".to_owned(),
        span: Span::default(),
    });
    let _ = plan_statement_with_catalog(&stmt, &planner)
        .expect_err("CompatTagged must not produce a plan");
}

#[test]
fn plan_create_view_from_information_schema_views() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let plan = plan_with_catalog(
        "CREATE VIEW v AS \
         SELECT table_name, is_updatable FROM information_schema.views",
        &planner,
    )
    .expect("plan");

    match plan {
        LogicalPlan::CreateView {
            view_name, columns, ..
        } => {
            assert_eq!(view_name, "v");
            assert_eq!(columns.len(), 2);
            assert_eq!(columns[0].name, "table_name");
            assert_eq!(columns[1].name, "is_updatable");
        }
        other => panic!("expected CreateView, got {other:?}"),
    }
}

#[test]
fn plan_drop_table_with_catalog() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let plan = plan_with_catalog("DROP TABLE users", &planner).expect("plan");
    assert!(matches!(plan, LogicalPlan::DropTable { .. }));
}

#[test]
fn plan_drop_nonexistent_table_errors() {
    let planner = Planner::default();
    let err = plan_with_catalog("DROP TABLE nonexistent", &planner).expect_err("should fail");
    assert_eq!(err.sqlstate(), SqlState::UndefinedTable);
}

#[test]
fn plan_alter_role_preserves_unspecified_fields() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let plan = plan_with_catalog("ALTER ROLE admin NOLOGIN", &planner).expect("plan");

    match plan {
        LogicalPlan::AlterRole {
            name,
            login,
            superuser,
            current_password_hash,
            new_password,
            ..
        } => {
            assert_eq!(name, "admin");
            assert!(!login);
            assert!(superuser);
            assert_eq!(current_password_hash.as_deref(), Some("stored-hash"));
            assert!(new_password.is_none());
        }
        other => panic!("expected AlterRole, got {other:?}"),
    }
}

#[test]
fn plan_create_role_password_null_keeps_password_unset() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let plan = plan_with_catalog("CREATE ROLE app_user LOGIN PASSWORD NULL", &planner)
        .expect("plan should succeed");
    match plan {
        LogicalPlan::CreateRole {
            name,
            login,
            password,
            ..
        } => {
            assert_eq!(name, "app_user");
            assert!(login);
            assert!(password.is_none());
        }
        other => panic!("expected CreateRole, got {other:?}"),
    }
}

#[test]
fn plan_create_role_membership_clauses_are_explicitly_rejected() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let err = plan_with_catalog("CREATE ROLE app_user IN ROLE reporting", &planner)
        .expect_err("should fail");
    assert_eq!(err.sqlstate(), SqlState::FeatureNotSupported);
    assert!(
        err.to_string()
            .contains("CREATE ROLE membership clauses (IN ROLE/ROLE/ADMIN/USER) are not supported"),
        "unexpected error: {err}"
    );
}

#[test]
fn plan_alter_table_rename_constraint_is_explicitly_rejected() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let err = plan_with_catalog(
        "ALTER TABLE users RENAME CONSTRAINT users_pkey TO users_pk_new",
        &planner,
    )
    .expect_err("should fail");
    assert_eq!(err.sqlstate(), SqlState::FeatureNotSupported);
    assert!(
        err.to_string()
            .contains("ALTER TABLE ... RENAME CONSTRAINT is not supported"),
        "unexpected error: {err}"
    );
}

#[test]
fn plan_alter_role_password_null_is_explicitly_rejected() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let err =
        plan_with_catalog("ALTER ROLE admin PASSWORD NULL", &planner).expect_err("should fail");
    assert_eq!(err.sqlstate(), SqlState::FeatureNotSupported);
    assert!(
        err.to_string()
            .contains("ALTER ROLE ... PASSWORD NULL is not supported"),
        "unexpected error: {err}"
    );
}

// ===================================================================
// plan() API - DML through public interface
// ===================================================================

#[test]
fn plan_select_literal() {
    let planner = Planner::default();
    let plan = plan_with_catalog("SELECT 42", &planner).expect("plan");
    match &plan {
        LogicalPlan::ProjectOnce { outputs, .. } => {
            assert_eq!(outputs.len(), 1);
        }
        other => panic!("expected ProjectOnce, got {other:?}"),
    }
}

#[test]
fn plan_select_from_table() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let plan = plan_with_catalog("SELECT id, name FROM users", &planner).expect("plan");
    match &plan {
        LogicalPlan::ProjectTable { outputs, .. } => {
            assert_eq!(outputs.len(), 2);
            assert_eq!(outputs[0].field.name, "id");
            assert_eq!(outputs[1].field.name, "name");
        }
        other => panic!("expected ProjectTable, got {other:?}"),
    }
}

#[test]
fn plan_select_from_pg_catalog_applies_projection_and_filter() {
    let planner = Planner::default();
    let plan = plan_with_catalog(
        "SELECT typname AS name, oid FROM pg_type WHERE oid = to_regtype('int4') ORDER BY oid",
        &planner,
    )
    .expect("plan");

    match &plan {
        LogicalPlan::ProjectValues {
            output_fields,
            rows,
            ..
        } => {
            assert_eq!(output_fields.len(), 2);
            assert_eq!(output_fields[0].name, "name");
            assert_eq!(output_fields[1].name, "oid");
            assert_eq!(rows.len(), 1);
        }
        other => panic!("expected ProjectValues, got {other:?}"),
    }
}

#[test]
fn plan_select_from_pg_catalog_falls_back_when_virtual_fast_path_is_too_restrictive() {
    let planner = Planner::default();
    let plan = plan_with_catalog(
        "SELECT lower(typname) AS name \
         FROM pg_type \
         WHERE coalesce(typname, '') = 'int4' \
         ORDER BY upper(typname)",
        &planner,
    )
    .expect("plan");

    match &plan {
        LogicalPlan::ProjectSource {
            outputs, order_by, ..
        } => {
            assert_eq!(outputs.len(), 1);
            assert_eq!(outputs[0].field.name, "name");
            assert_eq!(order_by.len(), 1);
        }
        other => panic!("expected ProjectSource via binder fallback, got {other:?}"),
    }
}

#[test]
fn plan_insert() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let plan = plan_with_catalog("INSERT INTO users (id, name) VALUES (1, 'Alice')", &planner)
        .expect("plan");
    assert!(matches!(plan, LogicalPlan::InsertValues { .. }));
}

#[test]
fn plan_update() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let plan =
        plan_with_catalog("UPDATE users SET name = 'Bob' WHERE id = 1", &planner).expect("plan");
    assert!(matches!(plan, LogicalPlan::UpdateTable { .. }));
}

#[test]
fn plan_delete() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let plan = plan_with_catalog("DELETE FROM users WHERE id = 1", &planner).expect("plan");
    assert!(matches!(plan, LogicalPlan::DeleteFromTable { .. }));
}

#[test]
fn lateral_subquery_on_ctid_keeps_outer_column_reference() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let plan = plan_with_catalog(
        "SELECT t.ctid, t2.c \
         FROM tidrangescan t, \
         LATERAL (SELECT count(*) c FROM tidrangescan t2 WHERE t2.ctid <= t.ctid) t2 \
         WHERE t.ctid < '(1,0)'",
        &planner,
    )
    .expect("plan");

    assert!(
        format!("{plan:#?}").contains("OuterColumnRef"),
        "expected lateral plan to retain an outer column reference, got: {plan:#?}"
    );
}

#[test]
fn correlated_subquery_keeps_qualified_outer_reference_when_inner_aliases_same_table() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let plan = plan_with_catalog(
        "SELECT id, \
                (SELECT count(*) FROM users x WHERE x.id < users.id) \
           FROM users",
        &planner,
    )
    .expect("plan");

    assert!(
        format!("{plan:#?}").contains("OuterColumnRef"),
        "expected correlated plan to retain qualified outer reference, got: {plan:#?}"
    );
}

#[test]
fn correlated_subquery_allows_outer_refs_inside_from_clause_srf_arguments() {
    let planner = Planner::new(Arc::new(TestCatalog));
    plan_with_catalog(
        "SELECT 1 \
           FROM (SELECT ARRAY[1,2] AS prattrs) pr \
          WHERE EXISTS (SELECT 1 FROM generate_series(1, array_upper(pr.prattrs, 1)) s)",
        &planner,
    )
    .expect("plan");
}

#[test]
fn correlated_subquery_allows_outer_refs_inside_wrapper_srf_arguments() {
    let planner = Planner::new(Arc::new(TestCatalog));
    plan_with_catalog(
        "SELECT 1 \
           FROM (SELECT '{\"a\":1}'::jsonb AS j) t \
          WHERE EXISTS (SELECT 1 FROM jsonb_each(t.j) x)",
        &planner,
    )
    .expect("plan");
}

#[test]
fn correlated_subquery_allows_outer_refs_inside_pg_options_to_table_arguments() {
    let planner = Planner::new(Arc::new(TestCatalog));
    plan_with_catalog(
        "SELECT 1 \
           FROM (SELECT ARRAY['a=1']::text[] AS opts) t \
          WHERE EXISTS (SELECT 1 FROM pg_catalog.pg_options_to_table(t.opts) x)",
        &planner,
    )
    .expect("plan");
}

// ===================================================================
// describe() API
// ===================================================================

#[test]
fn describe_select_literals() {
    let planner = Planner::default();
    let stmt = parse_prepared_statement("SELECT 1, 'text'").expect("parse");
    let desc = planner
        .describe(PlanRequest {
            statement: &stmt,
            txn_id: TxnId::default(),
            default_schema: None,
            current_user: None,
            session_user: None,
            database_name: None,
            datestyle: None,
            timezone: None,
        })
        .expect("describe");
    assert_eq!(desc.output_fields.len(), 2);
    assert_eq!(desc.output_fields[0].data_type, DataType::Int);
    assert_eq!(desc.output_fields[1].data_type, DataType::Text);
    assert!(desc.param_types.is_empty());
}

#[test]
fn describe_ddl_returns_no_output_fields() {
    let planner = Planner::default();
    let stmt = parse_prepared_statement("CREATE TABLE t (id INT NOT NULL)").expect("parse");
    let desc = planner
        .describe(PlanRequest {
            statement: &stmt,
            txn_id: TxnId::default(),
            default_schema: None,
            current_user: None,
            session_user: None,
            database_name: None,
            datestyle: None,
            timezone: None,
        })
        .expect("describe");
    assert!(desc.output_fields.is_empty());
    assert!(desc.param_types.is_empty());
}

#[test]
fn describe_select_with_params() {
    let planner = Planner::default();
    let stmt = parse_prepared_statement("SELECT $1 = 'text'").expect("parse");
    let desc = planner
        .describe(PlanRequest {
            statement: &stmt,
            txn_id: TxnId::default(),
            default_schema: None,
            current_user: None,
            session_user: None,
            database_name: None,
            datestyle: None,
            timezone: None,
        })
        .expect("describe");
    assert_eq!(desc.param_types.len(), 1);
    assert_eq!(desc.param_types[0], DataType::Text);
}

#[test]
fn describe_select_limit_offset_params_infers_bigint() {
    let planner = Planner::default();
    let stmt = parse_prepared_statement("SELECT 1 LIMIT $1 OFFSET $2").expect("parse");
    let desc = planner
        .describe(PlanRequest {
            statement: &stmt,
            txn_id: TxnId::default(),
            default_schema: None,
            current_user: None,
            session_user: None,
            database_name: None,
            datestyle: None,
            timezone: None,
        })
        .expect("describe");
    assert_eq!(desc.param_types, vec![DataType::BigInt, DataType::BigInt]);
}

#[test]
fn describe_update_arithmetic_with_params_infers_types_from_columns() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let stmt =
        parse_prepared_statement("UPDATE users SET id = id + $1 WHERE id = $2").expect("parse");
    let desc = planner
        .describe(PlanRequest {
            statement: &stmt,
            txn_id: TxnId::default(),
            default_schema: None,
            current_user: None,
            session_user: None,
            database_name: None,
            datestyle: None,
            timezone: None,
        })
        .expect("describe");
    assert_eq!(desc.param_types, vec![DataType::Int, DataType::Int]);
}

#[test]
fn describe_select_bare_parameter_accepts_param_hint() {
    let planner = Planner::default();
    let stmt = parse_prepared_statement("SELECT $1").expect("parse");
    let make_request = || PlanRequest {
        statement: &stmt,
        txn_id: TxnId::default(),
        default_schema: None,
        current_user: None,
        session_user: None,
        database_name: None,
        datestyle: None,
        timezone: None,
    };

    let Err(err) = planner.describe(make_request()) else {
        panic!("describe should fail");
    };
    assert!(
        err.to_string()
            .contains("could not infer data type of parameter $1"),
        "unexpected error: {err}"
    );

    let desc = planner
        .describe_with_param_hints(make_request(), Some(&[Some(DataType::Int)]))
        .expect("describe with hints");
    assert_eq!(desc.param_types, vec![DataType::Int]);
    assert_eq!(desc.output_fields.len(), 1);
    assert_eq!(desc.output_fields[0].data_type, DataType::Int);
}

#[test]
fn describe_select_with_table() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let stmt = parse_prepared_statement("SELECT id, name FROM users").expect("parse");
    let desc = planner
        .describe(PlanRequest {
            statement: &stmt,
            txn_id: TxnId::default(),
            default_schema: None,
            current_user: None,
            session_user: None,
            database_name: None,
            datestyle: None,
            timezone: None,
        })
        .expect("describe");
    assert_eq!(desc.output_fields.len(), 2);
    assert_eq!(desc.output_fields[0].data_type, DataType::Int);
    assert!(!desc.output_fields[0].nullable);
    assert_eq!(desc.output_fields[1].data_type, DataType::Text);
    assert!(desc.output_fields[1].nullable);
}

#[test]
fn describe_select_from_pg_catalog_uses_projected_fields() {
    let planner = Planner::default();
    let stmt = parse_prepared_statement(
        "SELECT typname AS name, oid::regtype::text AS regtype FROM pg_type",
    )
    .expect("parse");
    let desc = planner
        .describe(PlanRequest {
            statement: &stmt,
            txn_id: TxnId::default(),
            default_schema: None,
            current_user: None,
            session_user: None,
            database_name: None,
            datestyle: None,
            timezone: None,
        })
        .expect("describe");

    assert_eq!(desc.output_fields.len(), 2);
    assert_eq!(desc.output_fields[0].name, "name");
    assert_eq!(desc.output_fields[0].data_type, DataType::Text);
    assert_eq!(desc.output_fields[1].name, "regtype");
    assert_eq!(desc.output_fields[1].data_type, DataType::Text);
}

#[test]
fn describe_pg_catalog_query_collects_params_from_derived_table_cte() {
    let planner = Planner::default();
    let stmt = parse_prepared_statement(
        "SELECT pg_catalog.pg_type.typname \
         FROM pg_catalog.pg_type \
         LEFT JOIN ( \
             SELECT pg_catalog.pg_constraint.contypid AS contypid \
             FROM pg_catalog.pg_constraint \
             WHERE pg_catalog.pg_constraint.contypid != $1::INTEGER \
         ) AS domain_constraints \
           ON pg_catalog.pg_type.oid = domain_constraints.contypid \
         WHERE pg_catalog.pg_type.typtype = $2::VARCHAR",
    )
    .expect("parse");
    let desc = planner
        .describe(PlanRequest {
            statement: &stmt,
            txn_id: TxnId::default(),
            default_schema: None,
            current_user: None,
            session_user: None,
            database_name: None,
            datestyle: None,
            timezone: None,
        })
        .expect("describe");

    assert_eq!(desc.output_fields.len(), 1);
    assert_eq!(desc.output_fields[0].name, "typname");
    assert_eq!(desc.output_fields[0].data_type, DataType::Text);
    assert_eq!(desc.param_types, vec![DataType::Int, DataType::Text]);
}

#[test]
fn describe_pg_catalog_query_collects_params_for_sqlalchemy_domain_reflection_shape() {
    let planner = Planner::default();
    let stmt = parse_prepared_statement(
        "SELECT pg_catalog.pg_type.typname AS name, \
                pg_catalog.format_type(pg_catalog.pg_type.typbasetype, pg_catalog.pg_type.typtypmod) AS attype, \
                NOT pg_catalog.pg_type.typnotnull AS nullable, \
                pg_catalog.pg_type.typdefault AS default_value, \
                pg_catalog.pg_type_is_visible(pg_catalog.pg_type.oid) AS visible, \
                pg_catalog.pg_namespace.nspname AS schema, \
                domain_constraints.condefs, \
                domain_constraints.connames, \
                pg_catalog.pg_collation.collname \
         FROM pg_catalog.pg_type \
         JOIN pg_catalog.pg_namespace ON pg_catalog.pg_namespace.oid = pg_catalog.pg_type.typnamespace \
         LEFT OUTER JOIN pg_catalog.pg_collation ON pg_catalog.pg_type.typcollation = pg_catalog.pg_collation.oid \
         LEFT OUTER JOIN ( \
             SELECT pg_catalog.pg_constraint.contypid AS contypid, \
                    array_agg(pg_catalog.pg_get_constraintdef(pg_catalog.pg_constraint.oid, $1)) AS condefs, \
                    array_agg(CAST(pg_catalog.pg_constraint.conname AS TEXT)) AS connames \
             FROM pg_catalog.pg_constraint \
             WHERE pg_catalog.pg_constraint.contypid != $2::INTEGER \
             GROUP BY pg_catalog.pg_constraint.contypid \
         ) AS domain_constraints ON pg_catalog.pg_type.oid = domain_constraints.contypid \
         WHERE pg_catalog.pg_type.typtype = $3::VARCHAR \
         ORDER BY pg_catalog.pg_namespace.nspname, pg_catalog.pg_type.typname",
    )
    .expect("parse");
    let Statement::Select(select) = &stmt else {
        panic!("expected SELECT statement");
    };
    let mut seen = BTreeSet::new();
    collect_select_parameters(select, &mut seen);
    assert_eq!(seen.into_iter().collect::<Vec<_>>(), vec![1, 2, 3]);
    let desc = planner
        .describe(PlanRequest {
            statement: &stmt,
            txn_id: TxnId::default(),
            default_schema: None,
            current_user: None,
            session_user: None,
            database_name: None,
            datestyle: None,
            timezone: None,
        })
        .expect("describe");

    assert_eq!(desc.output_fields.len(), 9);
    assert_eq!(
        desc.param_types,
        vec![DataType::Boolean, DataType::Int, DataType::Text]
    );
}

#[test]
fn describe_pg_catalog_query_falls_back_from_virtual_fast_path_for_param_types() {
    let planner = Planner::default();
    let stmt = parse_prepared_statement(
        "SELECT lower(typname) AS name \
         FROM pg_type \
         WHERE coalesce(oid, $1::INTEGER) = 23 \
         ORDER BY upper(typname)",
    )
    .expect("parse");
    let desc = planner
        .describe(PlanRequest {
            statement: &stmt,
            txn_id: TxnId::default(),
            default_schema: None,
            current_user: None,
            session_user: None,
            database_name: None,
            datestyle: None,
            timezone: None,
        })
        .expect("describe");

    assert_eq!(desc.output_fields.len(), 1);
    assert_eq!(desc.output_fields[0].name, "name");
    assert_eq!(desc.output_fields[0].data_type, DataType::Text);
    assert_eq!(desc.param_types, vec![DataType::Int]);
}

#[test]
fn describe_information_schema_query_falls_back_from_virtual_fast_path_for_param_types() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let stmt = parse_prepared_statement(
        "SELECT lower(column_name) AS name \
         FROM information_schema.columns \
         WHERE coalesce(ordinal_position, $1::INTEGER) = 1 \
         ORDER BY upper(column_name)",
    )
    .expect("parse");
    let desc = planner
        .describe(PlanRequest {
            statement: &stmt,
            txn_id: TxnId::default(),
            default_schema: None,
            current_user: None,
            session_user: None,
            database_name: None,
            datestyle: None,
            timezone: None,
        })
        .expect("describe");

    assert_eq!(desc.output_fields.len(), 1);
    assert_eq!(desc.output_fields[0].name, "name");
    assert_eq!(desc.output_fields[0].data_type, DataType::Text);
    assert_eq!(desc.param_types, vec![DataType::Int]);
}

// ===================================================================
// EmptyCatalog tests
// ===================================================================

#[test]
fn empty_catalog_get_table_returns_none() {
    let catalog = EmptyCatalog;
    let result = catalog
        .get_table(TxnId::default(), &QualifiedName::unqualified("test"))
        .expect("ok");
    assert!(result.is_none());
}

#[test]
fn empty_catalog_get_table_by_id_returns_none() {
    let catalog = EmptyCatalog;
    let result = catalog
        .get_table_by_id(TxnId::default(), RelationId::new(1))
        .expect("ok");
    assert!(result.is_none());
}

#[test]
fn empty_catalog_list_tables_returns_empty() {
    let catalog = EmptyCatalog;
    let result = catalog
        .list_tables(TxnId::default(), SchemaId::new(1))
        .expect("ok");
    assert!(result.is_empty());
}

#[test]
fn empty_catalog_list_indexes_returns_empty() {
    let catalog = EmptyCatalog;
    let result = catalog
        .list_indexes(TxnId::default(), RelationId::new(1))
        .expect("ok");
    assert!(result.is_empty());
}

#[test]
fn empty_catalog_get_schema_returns_none() {
    let catalog = EmptyCatalog;
    let result = catalog
        .get_schema(TxnId::default(), &QualifiedName::unqualified("public"))
        .expect("ok");
    assert!(result.is_none());
}

#[test]
fn empty_catalog_get_index_returns_none() {
    let catalog = EmptyCatalog;
    let result = catalog
        .get_index(TxnId::default(), aiondb_core::IndexId::new(1))
        .expect("ok");
    assert!(result.is_none());
}

#[test]
fn empty_catalog_get_sequence_returns_none() {
    let catalog = EmptyCatalog;
    let result = catalog
        .get_sequence(TxnId::default(), &QualifiedName::unqualified("seq"))
        .expect("ok");
    assert!(result.is_none());
}

#[test]
fn empty_catalog_get_statistics_returns_none() {
    let catalog = EmptyCatalog;
    let result = catalog
        .get_statistics(TxnId::default(), RelationId::new(1))
        .expect("ok");
    assert!(result.is_none());
}

// ===================================================================
// Error: unsupported SQL through planner
// ===================================================================

#[test]
fn select_undefined_table_errors() {
    let planner = Planner::default();
    let err = plan_with_catalog("SELECT * FROM nonexistent", &planner).expect_err("should fail");
    assert_eq!(err.sqlstate(), SqlState::UndefinedTable);
}

#[test]
fn insert_into_undefined_table_errors() {
    let planner = Planner::default();
    let err = plan_with_catalog("INSERT INTO nonexistent (id) VALUES (1)", &planner)
        .expect_err("should fail");
    assert_eq!(err.sqlstate(), SqlState::UndefinedTable);
}

#[test]
fn delete_from_undefined_table_errors() {
    let planner = Planner::default();
    let err = plan_with_catalog("DELETE FROM nonexistent", &planner).expect_err("should fail");
    assert_eq!(err.sqlstate(), SqlState::UndefinedTable);
}

#[test]
fn update_undefined_table_errors() {
    let planner = Planner::default();
    let err =
        plan_with_catalog("UPDATE nonexistent SET id = 1", &planner).expect_err("should fail");
    assert_eq!(err.sqlstate(), SqlState::UndefinedTable);
}

// ===================================================================
// Multiple expressions in SELECT
// ===================================================================

#[test]
fn plan_select_multiple_expressions() {
    let planner = Planner::default();
    let plan = plan_with_catalog("SELECT 1, 'hello', TRUE, NULL", &planner).expect("plan");
    match plan {
        LogicalPlan::ProjectOnce { outputs, .. } => {
            assert_eq!(outputs.len(), 4);
        }
        other => panic!("expected ProjectOnce, got {other:?}"),
    }
}

#[test]
fn plan_select_with_where_and_table() {
    let planner = Planner::new(Arc::new(TestCatalog));
    let plan = plan_with_catalog("SELECT id FROM users WHERE id = 1", &planner).expect("plan");
    match plan {
        LogicalPlan::ProjectTable { filter, .. } => {
            assert!(filter.is_some());
        }
        other => panic!("expected ProjectTable, got {other:?}"),
    }
}

// ===================================================================
// StatementDescription struct
// ===================================================================

#[test]
fn statement_description_fields_accessible() {
    let desc = StatementDescription {
        output_fields: vec![ResultField {
            name: "col".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        }],
        output_origins: vec![None],
        param_types: vec![DataType::Text],
    };
    assert_eq!(desc.output_fields.len(), 1);
    assert_eq!(desc.param_types.len(), 1);
}
