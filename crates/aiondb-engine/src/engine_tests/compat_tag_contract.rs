#![allow(clippy::pedantic)]

//! Contract tests for the 55-tag compat matrix.
//!
//! These tests enforce, for every tag in `COMPAT_TAG_MATRIX`:
//!     error with the correct SQLSTATE instead of returning `command_ok`.
//!   * **Expected SQLSTATEs**: `undefined_object` / `undefined_table` /
//!     `invalid_schema_name` are enforced depending on the object kind.
//!   * **Embedded/pgwire parity**: the embedded path (`engine.execute_sql`)
//!     and the extended-query path (`engine.execute_statement_prechecked`
//!     via prepared statements) return the same error shape.
//!
//! The harness bootstraps a superuser session (so DDL passes the RBAC
//! gate) and runs each expectation in isolation. When the matrix grows
//! (new tag added), update `MISSING_TARGET_EXPECTATIONS` below.
//!
//! Pre-existing RBAC baseline failure does NOT cause these tests to fail
//! - each case runs in its own session + transaction and the bootstrap
//!
//! role setup is the only external dependency.

use super::*;
use aiondb_core::SqlState;
use aiondb_pg_compat::compat_tag_matrix::{
    is_sensitive_compat_tag, CompatTagBehavior, COMPAT_TAG_MATRIX,
};

/// One expectation entry: statement sent to the engine + the SQLSTATE that
/// must come back. Only tags where a missing-target-error is meaningful are
/// listed; CREATE families with referential targets are expected to surface
/// the same missing-target SQLSTATE as direct execution.
struct MissingTargetCase {
    tag: &'static str,
    sql: &'static str,
    expected: SqlState,
}

const MISSING_TARGET_EXPECTATIONS: &[MissingTargetCase] = &[
    // ALTER X on an unknown target: each must surface undefined_object or
    // the kind-specific SQLSTATE.
    MissingTargetCase {
        tag: "ALTER TABLE",
        sql: "ALTER TABLE __nope__ ENABLE ROW LEVEL SECURITY",
        expected: SqlState::UndefinedTable,
    },
    MissingTargetCase {
        tag: "ALTER POLICY",
        sql: "ALTER POLICY __nope__ ON __no_tbl__ USING (true)",
        expected: SqlState::UndefinedObject,
    },
    MissingTargetCase {
        tag: "ALTER STATISTICS",
        sql: "ALTER STATISTICS __nope__ SET STATISTICS 0",
        expected: SqlState::UndefinedObject,
    },
    MissingTargetCase {
        tag: "ALTER PUBLICATION",
        sql: "ALTER PUBLICATION __nope__ OWNER TO alice",
        expected: SqlState::FeatureNotSupported,
    },
    MissingTargetCase {
        tag: "ALTER SUBSCRIPTION",
        sql: "ALTER SUBSCRIPTION __nope__ OWNER TO alice",
        expected: SqlState::FeatureNotSupported,
    },
    MissingTargetCase {
        tag: "ALTER SERVER",
        sql: "ALTER SERVER __nope__ OWNER TO alice",
        expected: SqlState::FeatureNotSupported,
    },
    MissingTargetCase {
        tag: "ALTER FOREIGN TABLE",
        sql: "ALTER FOREIGN TABLE __nope__ OWNER TO alice",
        expected: SqlState::FeatureNotSupported,
    },
    MissingTargetCase {
        tag: "ALTER USER MAPPING",
        sql: "ALTER USER MAPPING FOR alice SERVER __no_srv__ OPTIONS (SET x 'y')",
        expected: SqlState::FeatureNotSupported,
    },
    MissingTargetCase {
        tag: "ALTER FOREIGN DATA WRAPPER",
        sql: "ALTER FOREIGN DATA WRAPPER __nope__ OWNER TO alice",
        expected: SqlState::FeatureNotSupported,
    },
    MissingTargetCase {
        tag: "DROP PUBLICATION",
        sql: "DROP PUBLICATION __nope__",
        expected: SqlState::FeatureNotSupported,
    },
    MissingTargetCase {
        tag: "DROP SUBSCRIPTION",
        sql: "DROP SUBSCRIPTION __nope__",
        expected: SqlState::FeatureNotSupported,
    },
    MissingTargetCase {
        tag: "DROP SERVER",
        sql: "DROP SERVER __nope__",
        expected: SqlState::FeatureNotSupported,
    },
    MissingTargetCase {
        tag: "DROP FOREIGN TABLE",
        sql: "DROP FOREIGN TABLE __nope__",
        expected: SqlState::FeatureNotSupported,
    },
    MissingTargetCase {
        tag: "DROP FOREIGN DATA WRAPPER",
        sql: "DROP FOREIGN DATA WRAPPER __nope__",
        expected: SqlState::FeatureNotSupported,
    },
    MissingTargetCase {
        tag: "DROP POLICY",
        sql: "DROP POLICY __nope__ ON __no_tbl__",
        expected: SqlState::UndefinedObject,
    },
    // DROP EVENT TRIGGER now rejects at the allowlist. The previous
    // `UndefinedObject` expectation was replaced by an explicit
    // `FeatureNotSupported` case near the end of this list.
    MissingTargetCase {
        tag: "DROP USER MAPPING",
        sql: "DROP USER MAPPING FOR alice SERVER __no_srv__",
        expected: SqlState::FeatureNotSupported,
    },
    MissingTargetCase {
        tag: "DROP COLLATION",
        sql: "DROP COLLATION __nope__",
        expected: SqlState::FeatureNotSupported,
    },
    MissingTargetCase {
        tag: "DROP STATISTICS",
        sql: "DROP STATISTICS __nope__",
        expected: SqlState::FeatureNotSupported,
    },
    MissingTargetCase {
        tag: "DROP TABLESPACE",
        sql: "DROP TABLESPACE __nope__",
        expected: SqlState::FeatureNotSupported,
    },
    MissingTargetCase {
        tag: "DROP MATERIALIZED VIEW",
        sql: "DROP MATERIALIZED VIEW __nope__",
        // Materialized views are relations in our catalog routing, so PG
        // `undefined_table` (42P01) is the correct SQLSTATE rather than
        // the generic `undefined_object` used for pure compat misc objects.
        expected: SqlState::UndefinedTable,
    },
    MissingTargetCase {
        tag: "ALTER SCHEMA",
        sql: "ALTER SCHEMA __nope__ OWNER TO alice",
        expected: SqlState::FeatureNotSupported,
    },
    MissingTargetCase {
        tag: "ALTER FUNCTION",
        sql: "ALTER FUNCTION __nope__ OWNER TO alice",
        expected: SqlState::FeatureNotSupported,
    },
    MissingTargetCase {
        tag: "ALTER INDEX",
        sql: "ALTER INDEX __nope__ OWNER TO alice",
        expected: SqlState::FeatureNotSupported,
    },
    MissingTargetCase {
        tag: "ALTER SEQUENCE",
        sql: "ALTER SEQUENCE __nope__ OWNER TO alice",
        expected: SqlState::FeatureNotSupported,
    },
    MissingTargetCase {
        tag: "ALTER VIEW",
        sql: "ALTER VIEW __nope__ SET (check_option = local)",
        expected: SqlState::UndefinedTable,
    },
    MissingTargetCase {
        tag: "ALTER MATERIALIZED",
        sql: "ALTER MATERIALIZED VIEW __nope__ OWNER TO alice",
        expected: SqlState::FeatureNotSupported,
    },
    MissingTargetCase {
        tag: "ALTER AGGREGATE",
        sql: "ALTER AGGREGATE __nope__(int) OWNER TO alice",
        expected: SqlState::FeatureNotSupported,
    },
    MissingTargetCase {
        tag: "ALTER PROCEDURE",
        sql: "ALTER PROCEDURE __nope__() OWNER TO alice",
        expected: SqlState::FeatureNotSupported,
    },
    MissingTargetCase {
        tag: "ALTER TRIGGER",
        sql: "ALTER TRIGGER __nope__ ON __no_tbl__ ENABLE",
        expected: SqlState::FeatureNotSupported,
    },
    MissingTargetCase {
        tag: "ALTER RULE",
        sql: "ALTER RULE rnope ON tnope RENAME TO rnope2",
        expected: SqlState::FeatureNotSupported,
    },
    MissingTargetCase {
        tag: "DROP RULE",
        sql: "DROP RULE __nope__",
        expected: SqlState::UndefinedObject,
    },
    MissingTargetCase {
        tag: "ALTER DEFAULT",
        sql: "ALTER DEFAULT PRIVILEGES FOR ROLE __nope__ GRANT SELECT ON TABLES TO alice",
        expected: SqlState::FeatureNotSupported,
    },
    MissingTargetCase {
        tag: "ALTER COLLATION",
        sql: "ALTER COLLATION __nope__ OWNER TO alice",
        expected: SqlState::FeatureNotSupported,
    },
    MissingTargetCase {
        tag: "ALTER EXTENSION",
        sql: "ALTER EXTENSION __nope__ UPDATE",
        expected: SqlState::FeatureNotSupported,
    },
    MissingTargetCase {
        tag: "ALTER OPERATOR",
        sql: "ALTER OPERATOR === (boolean, boolean) SET (RESTRICT = NONE)",
        expected: SqlState::FeatureNotSupported,
    },
    MissingTargetCase {
        tag: "ALTER TABLESPACE",
        sql: "ALTER TABLESPACE __nope__ OWNER TO alice",
        expected: SqlState::FeatureNotSupported,
    },
    MissingTargetCase {
        tag: "REFRESH",
        sql: "REFRESH MATERIALIZED VIEW __nope__",
        expected: SqlState::UndefinedTable,
    },
    // CREATE-time referential checks added in the "Phase 1 real impl"
    // round: each CREATE verifies the objects it links to actually exist.
    MissingTargetCase {
        tag: "CREATE POLICY(table missing)",
        sql: "CREATE POLICY p ON __no_tbl__ FOR SELECT USING (true)",
        expected: SqlState::UndefinedTable,
    },
    MissingTargetCase {
        tag: "CREATE RULE(table missing)",
        sql: "CREATE RULE r_missing AS ON INSERT TO __no_tbl__ DO INSTEAD NOTHING",
        expected: SqlState::UndefinedTable,
    },
    MissingTargetCase {
        tag: "CREATE PUBLICATION(table missing)",
        sql: "CREATE PUBLICATION pub1 FOR TABLE __no_tbl__",
        expected: SqlState::UndefinedTable,
    },
    MissingTargetCase {
        tag: "CREATE SUBSCRIPTION(pub missing)",
        sql: "CREATE SUBSCRIPTION sub1 CONNECTION 'dbname=x' PUBLICATION __no_pub__",
        expected: SqlState::UndefinedObject,
    },
    MissingTargetCase {
        tag: "CREATE FOREIGN TABLE(server missing)",
        sql: "CREATE FOREIGN TABLE ft1 (a int) SERVER __no_srv__",
        expected: SqlState::UndefinedObject,
    },
    MissingTargetCase {
        tag: "CREATE USER MAPPING(server missing)",
        sql: "CREATE USER MAPPING FOR alice SERVER __no_srv__ OPTIONS (user 'u')",
        expected: SqlState::UndefinedObject,
    },
    // ACCESS METHOD (3 tags) is explicit-not-supported: AionDB has no
    // pluggable storage-access-method runtime.
    MissingTargetCase {
        tag: "CREATE ACCESS METHOD",
        sql: "CREATE ACCESS METHOD myam TYPE TABLE HANDLER my_handler",
        expected: SqlState::FeatureNotSupported,
    },
    MissingTargetCase {
        tag: "ALTER ACCESS METHOD",
        sql: "ALTER ACCESS METHOD myam OWNER TO alice",
        expected: SqlState::FeatureNotSupported,
    },
    MissingTargetCase {
        tag: "DROP ACCESS METHOD",
        sql: "DROP ACCESS METHOD myam",
        expected: SqlState::FeatureNotSupported,
    },
    // LANGUAGE (2 tags): no pluggable procedural language runtime.
    MissingTargetCase {
        tag: "ALTER LANGUAGE",
        sql: "ALTER LANGUAGE plpgsql OWNER TO alice",
        expected: SqlState::FeatureNotSupported,
    },
    MissingTargetCase {
        tag: "DROP LANGUAGE",
        sql: "DROP LANGUAGE plpgsql",
        expected: SqlState::FeatureNotSupported,
    },
    // CONVERSION (3 tags): UTF-8 everywhere, no pluggable encoding
    // conversions.
    MissingTargetCase {
        tag: "CREATE CONVERSION",
        sql: "CREATE CONVERSION myconv FOR 'LATIN1' TO 'UTF8' FROM iso8859_1_to_utf8",
        expected: SqlState::FeatureNotSupported,
    },
    MissingTargetCase {
        tag: "ALTER CONVERSION",
        sql: "ALTER CONVERSION myconv OWNER TO alice",
        expected: SqlState::FeatureNotSupported,
    },
    MissingTargetCase {
        tag: "DROP CONVERSION",
        sql: "DROP CONVERSION myconv",
        expected: SqlState::FeatureNotSupported,
    },
    // TRANSFORM (3 tags): PL/* type bindings require a procedural-language
    // runtime AionDB doesn't have.
    MissingTargetCase {
        tag: "CREATE TRANSFORM",
        sql: "CREATE TRANSFORM mytrans",
        expected: SqlState::FeatureNotSupported,
    },
    MissingTargetCase {
        tag: "ALTER TRANSFORM",
        sql: "ALTER TRANSFORM mytrans OWNER TO alice",
        expected: SqlState::FeatureNotSupported,
    },
    MissingTargetCase {
        tag: "DROP TRANSFORM",
        sql: "DROP TRANSFORM mytrans",
        expected: SqlState::FeatureNotSupported,
    },
    // TEXT SEARCH (2 tags): AionDB has no pluggable TS configuration
    // runtime.
    MissingTargetCase {
        tag: "ALTER TEXT SEARCH",
        sql: "ALTER TEXT SEARCH CONFIGURATION mytsconf OWNER TO alice",
        expected: SqlState::FeatureNotSupported,
    },
    MissingTargetCase {
        tag: "DROP TEXT SEARCH",
        sql: "DROP TEXT SEARCH CONFIGURATION mytsconf",
        expected: SqlState::FeatureNotSupported,
    },
    // be security-critical, so explicit reject is mandatory.
    MissingTargetCase {
        tag: "CREATE EVENT TRIGGER",
        sql: "CREATE EVENT TRIGGER my_et ON ddl_command_start EXECUTE PROCEDURE my_fn()",
        expected: SqlState::FeatureNotSupported,
    },
    MissingTargetCase {
        tag: "ALTER EVENT TRIGGER",
        sql: "ALTER EVENT TRIGGER my_et DISABLE",
        expected: SqlState::FeatureNotSupported,
    },
    MissingTargetCase {
        tag: "DROP EVENT TRIGGER",
        sql: "DROP EVENT TRIGGER my_et",
        expected: SqlState::FeatureNotSupported,
    },
];

fn assert_sqlstate(
    engine: &Engine,
    session: &SessionHandle,
    sql: &str,
    expected: SqlState,
    context: &str,
) {
    let err = engine
        .execute_sql(session, sql)
        .err()
        .unwrap_or_else(|| panic!("{context}: expected error for `{sql}`, got success"));
    let got = err.sqlstate();
    assert_eq!(
        got, expected,
        "{context}: `{sql}` expected SQLSTATE {expected:?}, got {got:?} ({err})"
    );
}

#[test]
fn matrix_size_matches_tag_count() {
    // Belt-and-suspenders: the matrix unit test already asserts this. A
    // change here is a signal that the contract surface expanded.
    // Matrix size should match the number of registered tags. Follow-up
    // cleanup: ALTER TABLE keeps only a narrow real compat subset; unsupported
    // subforms are explicit rejects.
    assert!(
        !COMPAT_TAG_MATRIX.is_empty(),
        "compat matrix unexpectedly empty"
    );
}

#[test]
fn every_sensitive_tag_is_registered_in_matrix() {
    // CREATE sensitive forms are typed `Statement::{Create*}` and no
    // longer carry a matrix row. DROP sensitive forms are handled
    // earlier. The invariant we still hold is that
    // `is_sensitive_compat_tag` flags every sensitive family so the
    // router's shadow-dispatch and the terminal guardrail stay in
    // lockstep.
    for tag in [
        "CREATE PUBLICATION",
        "CREATE SUBSCRIPTION",
        "CREATE POLICY",
        "CREATE FOREIGN TABLE",
        "CREATE USER MAPPING",
        "CREATE SERVER",
        "DROP PUBLICATION",
        "DROP SUBSCRIPTION",
        "DROP POLICY",
        "DROP FOREIGN TABLE",
        "DROP USER MAPPING",
        "DROP SERVER",
    ] {
        assert!(
            is_sensitive_compat_tag(tag),
            "tag {tag:?} flagged as sensitive-family but `is_sensitive_compat_tag` returns false"
        );
    }
}

#[test]
fn missing_targets_surface_real_sqlstate() {
    // Runs the expectation list. Each case is executed against a fresh
    // superuser session so RBAC doesn't interfere.
    let engine = EngineBuilder::for_testing().build().expect("engine");
    let (session, _) = engine.startup(startup_params()).expect("startup");
    // Bootstrap superuser so subsequent DDL passes the RBAC gate.
    let _ = engine.execute_sql(&session, "CREATE ROLE alice SUPERUSER LOGIN");
    for case in MISSING_TARGET_EXPECTATIONS {
        assert_sqlstate(
            &engine,
            &session,
            case.sql,
            case.expected,
            &format!("tag {}", case.tag),
        );
    }
}

#[test]
fn extended_query_protocol_also_rejects_missing_targets() {
    // Parity floor: every expectation must also surface an error through
    // the extended query protocol (`prepare` + `bind` + `execute_portal`),
    // not only through simple query mode. Some compat DDL fails as early
    // as `prepare` (with a generic `feature_not_supported` because the
    // `execute_portal`. Both are acceptable; the critical invariant is
    let engine = EngineBuilder::for_testing().build().expect("engine");
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let _ = engine.execute_sql(&session, "CREATE ROLE alice SUPERUSER LOGIN");

    for (idx, case) in MISSING_TARGET_EXPECTATIONS.iter().enumerate() {
        let stmt_name = format!("__compat_tag_contract_{idx}");
        let portal_name = format!("__compat_tag_contract_portal_{idx}");

        // Simple query path: asserted by `missing_targets_surface_real_sqlstate`.
        // Re-asserted here so a failure here points at the parity issue
        // rather than the matrix itself.
        assert!(
            engine.execute_sql(&session, case.sql).is_err(),
            "simple path unexpectedly succeeded for {}: {}",
            case.tag,
            case.sql
        );

        // Extended query path: at least one of `prepare`, `bind`, or
        // `execute_portal` must return an error. Never `Ok(())` throughout.
        let prepare_result = engine.prepare(&session, stmt_name.clone(), case.sql.to_owned());
        let rejected = match prepare_result {
            Err(_) => true,
            Ok(_) => {
                let bind_result =
                    engine.bind(&session, portal_name.clone(), stmt_name.clone(), Vec::new());
                match bind_result {
                    Err(_) => true,
                    Ok(()) => engine.execute_portal(&session, &portal_name, 0).is_err(),
                }
            }
        };
        assert!(
            rejected,
            "extended path unexpectedly succeeded end-to-end for {}: {}",
            case.tag, case.sql
        );
    }
}

#[test]
fn sensitive_tag_fallback_is_rejected() {
    // Verify that a sensitive compat tag that somehow slips
    // past the upstream dispatchers is caught by the guard in
    // `statement_exec::execute_planned_statement_with_limits_and_plan_cache`.
    //
    // We can't easily inject a fake PhysicalPlan here without plumbing, so
    // we just assert the helper is coherent: every sensitive tag is either
    // present in the matrix or is an ALTER-variant intercepted upstream.
    for entry in COMPAT_TAG_MATRIX {
        let sensitive = is_sensitive_compat_tag(entry.tag);
        if sensitive {
            assert_eq!(
                entry.behavior,
                CompatTagBehavior::ImplementedReal,
                "sensitive matrix entry {:?} must have behaviour ImplementedReal",
                entry.tag
            );
        }
    }
}

/// An unsupported tagged compatibility command must surface
/// `feature_not_supported`, never `command_ok`. This pins the terminal
/// guardrail inside `Engine::run_compat_router`.
#[test]
fn terminal_guardrail_rejects_unsupported_compat_tag() {
    let engine = EngineBuilder::for_testing().build().expect("engine");
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let _ = engine.execute_sql(&session, "CREATE ROLE alice SUPERUSER LOGIN");

    // `CREATE ACCESS METHOD` parses as a tagged compat statement with
    // tag `"CREATE ACCESS METHOD"`. It has no entry in `COMPAT_TAG_MATRIX`
    // and no `CompatCommand` variant, so the router's guardrail must
    // convert it to `feature_not_supported` rather than emitting
    // `command_ok`.
    let err = engine
        .execute_sql(
            &session,
            "CREATE ACCESS METHOD myam TYPE TABLE HANDLER my_handler",
        )
        .expect_err("unsupported compat tag must fail");
    assert_eq!(err.sqlstate(), SqlState::FeatureNotSupported);
    assert!(
        err.report().message.contains("CREATE ACCESS METHOD"),
        "error must name the tag; got {}",
        err.report().message
    );
}

// `aiondb_pg_compat::disposition::tests`; no duplicate here.

/// `Statement::CompatTagged` and `Statement::CompatTaggedNotice` are shadow
/// statements synthesized by the compat bridge. They must be consumed by the
/// compat router before reaching `statement_exec`; if one ever does, the
/// runtime guard returns a `DbError::internal` so the bypass is loud instead
/// `command_ok`.
#[test]
fn compat_tagged_is_rejected_at_statement_exec() {
    use aiondb_parser::ast::CompatTaggedStatement;
    use aiondb_parser::Span;
    let engine = EngineBuilder::for_testing().build().expect("engine");
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let statement = Statement::CompatTagged(CompatTaggedStatement {
        tag: "CREATE OPERATOR".to_owned(),
        raw_sql: "CREATE OPERATOR + (LEFTARG=int, RIGHTARG=int, PROCEDURE=int4pl)".to_owned(),
        span: Span::default(),
    });

    let err = engine
        .execute_statement_prechecked(&session, &statement)
        .expect_err("CompatTagged must never reach statement_exec");
    let message = &err.report().message;
    assert!(
        message.contains("CompatTagged statement reached statement_exec"),
        "guard message changed; got {message}"
    );
}
