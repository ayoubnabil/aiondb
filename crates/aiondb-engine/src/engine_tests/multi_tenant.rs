use super::*;

// ===================================================================
// CREATE TENANT
// ===================================================================

#[test]
fn create_tenant_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "CREATE TENANT acme")
        .expect("create tenant");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "CREATE TENANT".to_owned(),
            rows_affected: 0,
        }]
    );
}

#[test]
fn create_tenant_duplicate_fails() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TENANT acme")
        .expect("create tenant");
    let err = engine
        .execute_sql(&session, "CREATE TENANT acme")
        .expect_err("duplicate tenant");
    let msg = format!("{err}");
    assert!(msg.contains("already exists"), "unexpected error: {msg}");
}

#[test]
fn create_tenant_creates_schema() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TENANT acme")
        .expect("create tenant");

    // The tenant schema "tenant_acme" should now exist.
    let results = engine
        .execute_sql(
            &session,
            "SELECT schema_name FROM information_schema.schemata WHERE schema_name = 'tenant_acme'",
        )
        .expect("query schemata");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1, "expected tenant schema to exist");
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

// ===================================================================
// SET TENANT
// ===================================================================

#[test]
fn set_tenant_routes_unqualified_names() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TENANT acme")
        .expect("create tenant");
    engine
        .execute_sql(&session, "SET TENANT acme")
        .expect("set tenant");

    // Create a table without schema qualification -- it should land in
    // the tenant schema "tenant_acme".
    engine
        .execute_sql(
            &session,
            "CREATE TABLE orders (id INT PRIMARY KEY, amount INT NOT NULL)",
        )
        .expect("create table in tenant schema");

    let tenant_tables = engine
        .execute_sql(
            &session,
            "SELECT table_schema FROM information_schema.tables \
             WHERE table_schema = 'tenant_acme' AND table_name = 'orders'",
        )
        .expect("query tenant table metadata");
    match &tenant_tables[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1, "expected orders in tenant_acme");
            assert_eq!(rows[0].values[0], Value::Text("tenant_acme".to_owned()));
        }
        other => panic!("expected query result, got {other:?}"),
    }

    let public_tables = engine
        .execute_sql(
            &session,
            "SELECT table_schema FROM information_schema.tables \
             WHERE table_schema = 'public' AND table_name = 'orders'",
        )
        .expect("query public table metadata");
    match &public_tables[0] {
        StatementResult::Query { rows, .. } => {
            assert!(rows.is_empty(), "orders should not leak into public");
        }
        other => panic!("expected query result, got {other:?}"),
    }

    // The table should be accessible through the tenant schema.
    engine
        .execute_sql(&session, "INSERT INTO orders (id, amount) VALUES (1, 100)")
        .expect("insert");
    let results = engine
        .execute_sql(&session, "SELECT id, amount FROM orders")
        .expect("select");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn set_tenant_nonexistent_fails() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(&session, "SET TENANT ghost")
        .expect_err("nonexistent tenant");
    let msg = format!("{err}");
    assert!(msg.contains("does not exist"), "unexpected error: {msg}");
}

// ===================================================================
// DROP TENANT
// ===================================================================

#[test]
fn drop_tenant_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TENANT acme")
        .expect("create tenant");
    let results = engine
        .execute_sql(&session, "DROP TENANT acme")
        .expect("drop tenant");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "DROP TENANT".to_owned(),
            rows_affected: 0,
        }]
    );
}

#[test]
fn drop_tenant_nonexistent_fails() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(&session, "DROP TENANT ghost")
        .expect_err("drop nonexistent tenant");
    let msg = format!("{err}");
    assert!(msg.contains("does not exist"), "unexpected error: {msg}");
}

#[test]
fn drop_tenant_cascades_tables() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TENANT acme")
        .expect("create tenant");
    engine
        .execute_sql(&session, "SET TENANT acme")
        .expect("set tenant");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE widgets (id INT PRIMARY KEY, name TEXT)",
        )
        .expect("create table");

    // Clear tenant context before dropping
    // (we cannot drop the active tenant)
    // Create a second tenant to switch away
    engine
        .execute_sql(&session, "CREATE TENANT other")
        .expect("create other tenant");
    engine
        .execute_sql(&session, "SET TENANT other")
        .expect("switch tenant");

    engine
        .execute_sql(&session, "DROP TENANT acme")
        .expect("drop tenant with tables");

    // The schema should no longer exist.
    let err = engine
        .execute_sql(&session, "SET TENANT acme")
        .expect_err("tenant should be gone");
    let msg = format!("{err}");
    assert!(msg.contains("does not exist"), "unexpected error: {msg}");
}

#[test]
fn drop_active_tenant_fails() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TENANT acme")
        .expect("create tenant");
    engine
        .execute_sql(&session, "SET TENANT acme")
        .expect("set tenant");

    let err = engine
        .execute_sql(&session, "DROP TENANT acme")
        .expect_err("should not drop active tenant");
    assert_eq!(
        err.sqlstate(),
        aiondb_core::SqlState::ObjectNotInPrerequisiteState
    );
    let msg = format!("{err}");
    assert!(msg.contains("active tenant"), "unexpected error: {msg}");
}

// ===================================================================
// Tenant isolation
// ===================================================================

#[test]
fn tenant_isolation_separate_tables() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Create two tenants
    engine
        .execute_sql(&session, "CREATE TENANT alpha")
        .expect("create alpha");
    engine
        .execute_sql(&session, "CREATE TENANT beta")
        .expect("create beta");

    // Create a table in alpha
    engine
        .execute_sql(&session, "SET TENANT alpha")
        .expect("set alpha");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT PRIMARY KEY, val INT NOT NULL)",
        )
        .expect("create items in alpha");
    engine
        .execute_sql(&session, "INSERT INTO items (id, val) VALUES (1, 10)")
        .expect("insert into alpha");

    // Create same-named table in beta with different data
    engine
        .execute_sql(&session, "SET TENANT beta")
        .expect("set beta");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT PRIMARY KEY, val INT NOT NULL)",
        )
        .expect("create items in beta");
    engine
        .execute_sql(&session, "INSERT INTO items (id, val) VALUES (2, 20)")
        .expect("insert into beta");

    // Query from beta -- should only see beta data
    let results = engine
        .execute_sql(&session, "SELECT id, val FROM items")
        .expect("select from beta");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values, vec![Value::Int(2), Value::Int(20)]);
        }
        other => panic!("expected query result, got {other:?}"),
    }

    // Switch back to alpha -- should only see alpha data
    engine
        .execute_sql(&session, "SET TENANT alpha")
        .expect("switch to alpha");
    let results = engine
        .execute_sql(&session, "SELECT id, val FROM items")
        .expect("select from alpha");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values, vec![Value::Int(1), Value::Int(10)]);
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn qualified_name_bypasses_tenant_routing() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Create a table in the public schema
    engine
        .execute_sql(
            &session,
            "CREATE TABLE public.global_config (key TEXT NOT NULL, val TEXT NOT NULL)",
        )
        .expect("create public table");

    engine
        .execute_sql(&session, "CREATE TENANT acme")
        .expect("create tenant");
    engine
        .execute_sql(&session, "SET TENANT acme")
        .expect("set tenant");

    // Even with an active tenant, fully qualified names should work
    engine
        .execute_sql(
            &session,
            "INSERT INTO public.global_config (key, val) VALUES ('k1', 'v1')",
        )
        .expect("insert into public table while tenant is active");

    let results = engine
        .execute_sql(&session, "SELECT key, val FROM public.global_config")
        .expect("select from public table");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

// ===================================================================
// Information schema filtering by tenant
// ===================================================================

#[test]
fn information_schema_filtered_by_tenant() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Create a table in public
    engine
        .execute_sql(&session, "CREATE TABLE public.shared (id INT PRIMARY KEY)")
        .expect("create public table");

    // Create two tenants with their own tables
    engine
        .execute_sql(&session, "CREATE TENANT alpha")
        .expect("create alpha");
    engine
        .execute_sql(&session, "SET TENANT alpha")
        .expect("set alpha");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT PRIMARY KEY, name TEXT NOT NULL)",
        )
        .expect("create items in alpha");

    engine
        .execute_sql(&session, "CREATE TENANT beta")
        .expect("create beta");

    // Switch to beta and create a table there
    engine
        .execute_sql(&session, "SET TENANT beta")
        .expect("set beta");
    engine
        .execute_sql(&session, "CREATE TABLE orders (id INT PRIMARY KEY)")
        .expect("create orders in beta");

    // Switch back to alpha
    engine
        .execute_sql(&session, "SET TENANT alpha")
        .expect("set alpha");

    // information_schema.tables should only show alpha's tables
    let results = engine
        .execute_sql(
            &session,
            "SELECT table_schema, table_name FROM information_schema.tables",
        )
        .expect("query tables");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1, "tenant alpha should see only its own table");
            assert_eq!(rows[0].values[0], Value::Text("tenant_alpha".to_owned()));
            assert_eq!(rows[0].values[1], Value::Text("items".to_owned()));
        }
        other => panic!("expected query result, got {other:?}"),
    }

    // information_schema.schemata should show tenant_alpha + information_schema only
    let results = engine
        .execute_sql(
            &session,
            "SELECT schema_name FROM information_schema.schemata",
        )
        .expect("query schemata");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            let names: Vec<_> = rows.iter().map(|r| r.values[0].clone()).collect();
            assert!(
                names.contains(&Value::Text("tenant_alpha".to_owned())),
                "should see tenant_alpha"
            );
            assert!(
                names.contains(&Value::Text("information_schema".to_owned())),
                "should see information_schema"
            );
            assert!(
                !names.contains(&Value::Text("public".to_owned())),
                "should not see public"
            );
            assert!(
                !names.contains(&Value::Text("tenant_beta".to_owned())),
                "should not see tenant_beta"
            );
        }
        other => panic!("expected query result, got {other:?}"),
    }

    // information_schema.columns should only show alpha's columns
    let results = engine
        .execute_sql(
            &session,
            "SELECT table_name, column_name FROM information_schema.columns",
        )
        .expect("query columns");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            // items table has 2 columns (id, name)
            assert_eq!(rows.len(), 2, "should see only alpha's table columns");
            for row in rows {
                assert_eq!(row.values[0], Value::Text("items".to_owned()));
            }
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

// ===================================================================
// pg_catalog filtering by tenant
// ===================================================================

#[test]
fn pg_catalog_filtered_by_tenant() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Create a table in public
    engine
        .execute_sql(&session, "CREATE TABLE public.shared (id INT PRIMARY KEY)")
        .expect("create public table");

    // Create tenant and table
    engine
        .execute_sql(&session, "CREATE TENANT alpha")
        .expect("create alpha");
    engine
        .execute_sql(&session, "SET TENANT alpha")
        .expect("set alpha");
    engine
        .execute_sql(&session, "CREATE TABLE items (id INT PRIMARY KEY)")
        .expect("create items in alpha");

    // pg_catalog.pg_class should only show alpha's objects
    // pg_class columns: oid(0), relname(1), relnamespace(2), relkind(3), relowner(4)
    let results = engine
        .execute_sql(&session, "SELECT * FROM pg_catalog.pg_class")
        .expect("query pg_class");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            let names: Vec<_> = rows
                .iter()
                .map(|r| r.values[1].clone()) // relname is at index 1
                .collect();
            assert!(
                names.contains(&Value::Text("items".to_owned())),
                "should see items, got: {names:?}"
            );
            assert!(
                !names.contains(&Value::Text("shared".to_owned())),
                "should not see public.shared"
            );
        }
        other => panic!("expected query result, got {other:?}"),
    }

    // pg_catalog.pg_namespace should show tenant_alpha and system schemas, not public
    // pg_namespace columns: oid(0), nspname(1), nspowner(2)
    let results = engine
        .execute_sql(&session, "SELECT * FROM pg_catalog.pg_namespace")
        .expect("query pg_namespace");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            let names: Vec<_> = rows
                .iter()
                .map(|r| r.values[1].clone()) // nspname is at index 1
                .collect();
            assert!(
                names.contains(&Value::Text("tenant_alpha".to_owned())),
                "should see tenant_alpha"
            );
            assert!(
                names.contains(&Value::Text("pg_catalog".to_owned())),
                "should see pg_catalog"
            );
            assert!(
                !names.contains(&Value::Text("public".to_owned())),
                "should not see public"
            );
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

// ===================================================================
// Cross-tenant access prevention
// ===================================================================

#[test]
fn cross_tenant_access_denied() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Create two tenants with tables
    engine
        .execute_sql(&session, "CREATE TENANT alpha")
        .expect("create alpha");
    engine
        .execute_sql(&session, "CREATE TENANT beta")
        .expect("create beta");

    engine
        .execute_sql(&session, "SET TENANT alpha")
        .expect("set alpha");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT PRIMARY KEY, val INT NOT NULL)",
        )
        .expect("create items in alpha");

    engine
        .execute_sql(&session, "SET TENANT beta")
        .expect("set beta");
    engine
        .execute_sql(&session, "CREATE TABLE orders (id INT PRIMARY KEY)")
        .expect("create orders in beta");

    // While beta is active, try to access alpha's schema
    let err = engine
        .execute_sql(&session, "SELECT * FROM tenant_alpha.items")
        .expect_err("should deny cross-tenant access");
    let msg = format!("{err}");
    assert!(
        msg.contains("cross-tenant access denied"),
        "unexpected error: {msg}"
    );

    // While beta is active, accessing own schema explicitly should work
    engine
        .execute_sql(&session, "SELECT * FROM tenant_beta.orders")
        .expect("own schema access should work");

    // Accessing public schema should still work (not blocked)
    engine
        .execute_sql(&session, "CREATE TABLE public.global (id INT PRIMARY KEY)")
        .expect("public schema access should work");
}

// ===================================================================
// Tenant-scoped sequences
// ===================================================================

#[test]
fn tenant_sees_own_sequences() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TENANT alpha")
        .expect("create alpha");
    engine
        .execute_sql(&session, "SET TENANT alpha")
        .expect("set alpha");

    // CREATE SEQUENCE routes to the tenant schema via default_schema.
    engine
        .execute_sql(&session, "CREATE SEQUENCE alpha_seq")
        .expect("create sequence");

    // Verify the sequence was created in the tenant schema and resolves via
    // the tenant search_path.
    let results = engine
        .execute_sql(&session, "SELECT nextval('alpha_seq')")
        .expect("nextval from alpha");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::BigInt(1));
        }
        other => panic!("expected query result, got {other:?}"),
    }

    // Create a second tenant
    engine
        .execute_sql(&session, "CREATE TENANT beta")
        .expect("create beta");
    engine
        .execute_sql(&session, "SET TENANT beta")
        .expect("set beta");

    // Beta cannot see the sequence with an unqualified name
    // (unqualified defaults to public, where it doesn't exist)
    let err = engine
        .execute_sql(&session, "SELECT nextval('alpha_seq')")
        .expect_err("should not find alpha's sequence");
    let msg = format!("{err}");
    assert!(msg.contains("does not exist"), "unexpected error: {msg}");

    // Beta can create its own sequence with the same unqualified name
    engine
        .execute_sql(&session, "CREATE SEQUENCE alpha_seq")
        .expect("create beta's own alpha_seq");
    let results = engine
        .execute_sql(&session, "SELECT nextval('alpha_seq')")
        .expect("nextval from beta");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            // Beta's sequence starts at 1 (independent from alpha's)
            assert_eq!(rows[0].values[0], Value::BigInt(1));
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

// ===================================================================
// No tenant sees everything
// ===================================================================

#[test]
fn no_tenant_sees_everything() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Create a public table
    engine
        .execute_sql(&session, "CREATE TABLE public.shared (id INT PRIMARY KEY)")
        .expect("create public table");

    // Create tenant with a table
    engine
        .execute_sql(&session, "CREATE TENANT alpha")
        .expect("create alpha");
    engine
        .execute_sql(&session, "SET TENANT alpha")
        .expect("set alpha");
    engine
        .execute_sql(&session, "CREATE TABLE items (id INT PRIMARY KEY)")
        .expect("create items");

    // Create another tenant with a table
    engine
        .execute_sql(&session, "CREATE TENANT beta")
        .expect("create beta");
    engine
        .execute_sql(&session, "SET TENANT beta")
        .expect("set beta");
    engine
        .execute_sql(&session, "CREATE TABLE orders (id INT PRIMARY KEY)")
        .expect("create orders");

    // Clear tenant context by opening a fresh session without setting a tenant.
    let (session2, _) = engine.startup(startup_params()).expect("startup2");

    // Without a tenant set, information_schema.tables should show all tables
    let results = engine
        .execute_sql(
            &session2,
            "SELECT table_schema, table_name FROM information_schema.tables",
        )
        .expect("query all tables");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            let schemas: Vec<_> = rows.iter().map(|r| r.values[0].clone()).collect();
            let names: Vec<_> = rows.iter().map(|r| r.values[1].clone()).collect();
            assert!(
                schemas.contains(&Value::Text("public".to_owned())),
                "should see public schema"
            );
            assert!(
                names.contains(&Value::Text("shared".to_owned())),
                "should see shared table"
            );
            assert!(
                schemas.contains(&Value::Text("tenant_alpha".to_owned())),
                "should see tenant_alpha"
            );
            assert!(
                schemas.contains(&Value::Text("tenant_beta".to_owned())),
                "should see tenant_beta"
            );
        }
        other => panic!("expected query result, got {other:?}"),
    }

    // information_schema.schemata should show all schemas
    let results = engine
        .execute_sql(
            &session2,
            "SELECT schema_name FROM information_schema.schemata",
        )
        .expect("query all schemata");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            let names: Vec<_> = rows.iter().map(|r| r.values[0].clone()).collect();
            assert!(names.contains(&Value::Text("public".to_owned())));
            assert!(names.contains(&Value::Text("tenant_alpha".to_owned())));
            assert!(names.contains(&Value::Text("tenant_beta".to_owned())));
            assert!(names.contains(&Value::Text("information_schema".to_owned())));
        }
        other => panic!("expected query result, got {other:?}"),
    }
}
