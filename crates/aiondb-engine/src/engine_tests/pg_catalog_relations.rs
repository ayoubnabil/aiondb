use super::*;

#[test]
fn multiple_tables_reflected_in_pg_class() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t1 (a INT NOT NULL); \
             CREATE TABLE t2 (b TEXT, c BOOLEAN)",
        )
        .expect("create tables");

    // Filter by public namespace to count only user tables (not system catalog entries).
    let class_rows = query_rows(
        &engine,
        &session,
        "SELECT * FROM pg_catalog.pg_class \
         WHERE relnamespace = (SELECT oid FROM pg_namespace WHERE nspname = 'public') \
           AND relkind = 'r'",
    );
    assert_eq!(class_rows.len(), 2, "expected 2 user tables in pg_class");

    let attr_rows = query_rows(&engine, &session, "SELECT * FROM pg_catalog.pg_attribute");
    // t1 has 1 column, t2 has 2 columns = 3 user attrs (plus system catalog).
    let user_attr_rows: Vec<_> = attr_rows
        .iter()
        .filter(|r| {
            let n = text_col(r, 1);
            n == "a" || n == "b" || n == "c"
        })
        .collect();
    assert_eq!(user_attr_rows.len(), 3, "expected 3 user attributes total");
}

#[test]
fn pg_class_attribute_join_with_schema_qualified_refs() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE join_test (id INT NOT NULL, name TEXT NOT NULL)",
        )
        .expect("create table");

    // Test aliased JOIN
    let rows = query_rows(
        &engine,
        &session,
        "SELECT c.relname, a.attname \
         FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_attribute a ON a.attrelid = c.oid \
         WHERE c.relname = 'join_test'",
    );
    assert!(!rows.is_empty(), "aliased JOIN should return rows");
    assert_eq!(text_col(&rows[0], 0), "join_test");

    // Test schema-qualified unaliased JOIN
    let rows2 = query_rows(
        &engine,
        &session,
        "SELECT pg_catalog.pg_class.relname, pg_catalog.pg_attribute.attname \
         FROM pg_catalog.pg_class \
         JOIN pg_catalog.pg_attribute \
           ON pg_catalog.pg_attribute.attrelid = pg_catalog.pg_class.oid \
         WHERE pg_catalog.pg_class.relname = 'join_test'",
    );
    assert!(
        !rows2.is_empty(),
        "schema-qualified JOIN should return rows"
    );
    assert_eq!(text_col(&rows2[0], 0), "join_test");

    // Test format_type with schema-qualified column refs in a JOIN (SQLAlchemy pattern)
    let rows3 = query_rows(
        &engine,
        &session,
        "SELECT pg_catalog.pg_attribute.attname, \
                pg_catalog.format_type(pg_catalog.pg_attribute.atttypid, pg_catalog.pg_attribute.atttypmod) \
         FROM pg_catalog.pg_class \
         JOIN pg_catalog.pg_attribute \
           ON pg_catalog.pg_attribute.attrelid = pg_catalog.pg_class.oid \
         WHERE pg_catalog.pg_class.relname = 'join_test'",
    );
    assert!(!rows3.is_empty(), "format_type in JOIN should return rows");
}

#[test]
fn pg_class_relkind_filters_support_postgres_any_all_array_forms() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE sqlalchemy_any_filter_test (id INT NOT NULL)",
        )
        .expect("create table");

    let any_rows = query_rows(
        &engine,
        &session,
        "SELECT DISTINCT pg_catalog.pg_class.relname \
         FROM pg_catalog.pg_class \
         JOIN pg_catalog.pg_namespace \
           ON pg_catalog.pg_namespace.oid = pg_catalog.pg_class.relnamespace \
         WHERE pg_catalog.pg_class.relkind = ANY(ARRAY['r', 'p', 'f', 'v', 'm']) \
           AND pg_catalog.pg_class.relname = 'sqlalchemy_any_filter_test'",
    );
    assert_eq!(
        any_rows.len(),
        1,
        "ANY(array) filter should match the table"
    );
    assert_eq!(text_col(&any_rows[0], 0), "sqlalchemy_any_filter_test");

    let all_rows = query_rows(
        &engine,
        &session,
        "SELECT DISTINCT pg_catalog.pg_class.relname \
         FROM pg_catalog.pg_class \
         JOIN pg_catalog.pg_namespace \
           ON pg_catalog.pg_namespace.oid = pg_catalog.pg_class.relnamespace \
         WHERE pg_catalog.pg_class.relkind != ALL(ARRAY['i', 'S']) \
           AND pg_catalog.pg_class.relname = 'sqlalchemy_any_filter_test'",
    );
    assert_eq!(
        all_rows.len(),
        1,
        "!= ALL(array) filter should match the table"
    );
    assert_eq!(text_col(&all_rows[0], 0), "sqlalchemy_any_filter_test");
}

#[test]
fn pg_statio_all_tables_exposes_visible_user_relations_for_orm_introspection() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE sequelize_statio_probe (id INT PRIMARY KEY, title TEXT)",
        )
        .expect("create table");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT schemaname, relname
         FROM pg_catalog.pg_statio_all_tables
         WHERE relname = 'sequelize_statio_probe'",
    );
    assert_eq!(rows.len(), 1, "expected one pg_statio_all_tables row");
    assert_eq!(text_col(&rows[0], 0), "public");
    assert_eq!(text_col(&rows[0], 1), "sequelize_statio_probe");
}

#[test]
fn prisma_pg_constraint_not_in_filter_survives_three_way_catalog_join() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE xtask_prisma_users_test ( \
                 id INT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, \
                 email VARCHAR(255) NOT NULL UNIQUE \
             ); \
             CREATE TABLE xtask_prisma_posts_test ( \
                 id INT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, \
                 slug VARCHAR(80) NOT NULL UNIQUE, \
                 title VARCHAR(140) NOT NULL, \
                 user_id INT NOT NULL, \
                 CONSTRAINT xtask_prisma_posts_test_user_title_uniq UNIQUE (user_id, title), \
                 CONSTRAINT xtask_prisma_posts_test_user_id_fkey \
                     FOREIGN KEY (user_id) REFERENCES xtask_prisma_users_test(id) \
                     ON UPDATE CASCADE ON DELETE RESTRICT \
             );",
        )
        .expect("create prisma-like tables");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT constr.conname, constr.contype \
           FROM pg_constraint constr \
           JOIN pg_class tableinfo ON tableinfo.oid = constr.conrelid \
           JOIN pg_namespace schemainfo ON schemainfo.oid = tableinfo.relnamespace \
          WHERE schemainfo.nspname = 'public' \
            AND constr.contype NOT IN ('p', 'u', 'f') \
          ORDER BY constr.conname",
    );
    assert!(
        rows.is_empty(),
        "three-way pg_catalog join should preserve contype NOT IN filter, got {rows:?}"
    );
}

#[test]
fn prisma_pg_constraint_foreign_key_actions_reflect_catalog_codes() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE fk_parent_probe (id INT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY); \
             CREATE TABLE fk_child_probe ( \
                 id INT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, \
                 parent_id INT NOT NULL, \
                 CONSTRAINT fk_child_probe_parent_fkey \
                     FOREIGN KEY (parent_id) REFERENCES fk_parent_probe(id) \
                     ON UPDATE CASCADE ON DELETE RESTRICT \
             );",
        )
        .expect("create fk probe tables");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT conname, confdeltype, confupdtype \
           FROM pg_constraint \
          WHERE conname = 'fk_child_probe_parent_fkey'",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(text_col(&rows[0], 0), "fk_child_probe_parent_fkey");
    assert_eq!(text_col(&rows[0], 1), "r");
    assert_eq!(text_col(&rows[0], 2), "c");
}

#[test]
fn prisma_pg_index_catalog_vectors_keep_zero_based_subscripts() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE prisma_index_probe ( \
                 id INT PRIMARY KEY, \
                 user_id INT NOT NULL, \
                 title TEXT NOT NULL, \
                 slug TEXT UNIQUE, \
                 UNIQUE (user_id, title) \
             );",
        )
        .expect("create prisma index probe");

    let rows = query_rows(
        &engine,
        &session,
        "WITH rawindex AS ( \
             SELECT indrelid, \
                    indexrelid, \
                    indisunique, \
                    indisprimary, \
                    unnest(indkey) AS indkeyid, \
                    generate_subscripts(indkey, 1) AS indkeyidx, \
                    unnest(indclass) AS indclass, \
                    unnest(indoption) AS indoption \
               FROM pg_index \
              WHERE indpred IS NULL \
                AND NOT indisexclusion \
         ) \
         SELECT indexinfo.relname AS index_name, \
                columninfo.attname AS column_name, \
                rawindex.indkeyidx AS column_index \
           FROM rawindex \
           JOIN pg_class AS tableinfo ON tableinfo.oid = rawindex.indrelid \
           JOIN pg_class AS indexinfo ON indexinfo.oid = rawindex.indexrelid \
           JOIN pg_namespace AS schemainfo ON schemainfo.oid = tableinfo.relnamespace \
      LEFT JOIN pg_attribute AS columninfo \
             ON columninfo.attrelid = tableinfo.oid \
            AND columninfo.attnum = rawindex.indkeyid \
          WHERE schemainfo.nspname = 'public' \
            AND tableinfo.relname = 'prisma_index_probe' \
       ORDER BY index_name, column_index",
    );

    assert_eq!(
        rows.iter()
            .map(|row| (
                text_col(row, 0).to_owned(),
                text_col(row, 1).to_owned(),
                int_col(row, 2),
            ))
            .collect::<Vec<_>>(),
        vec![
            ("prisma_index_probe_pkey".to_owned(), "id".to_owned(), 0,),
            (
                "prisma_index_probe_slug_unique".to_owned(),
                "slug".to_owned(),
                0,
            ),
            (
                "prisma_index_probe_user_id_title_unique".to_owned(),
                "user_id".to_owned(),
                0,
            ),
            (
                "prisma_index_probe_user_id_title_unique".to_owned(),
                "title".to_owned(),
                1,
            ),
        ]
    );
}

#[test]
fn typeorm_index_reflection_preserves_composite_column_order() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE xtask_typeorm_diff_users ( \
                 id INT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, \
                 email VARCHAR(190) NOT NULL UNIQUE, \
                 name VARCHAR(140) NOT NULL \
             ); \
             CREATE TABLE xtask_typeorm_diff_posts ( \
                 id INT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, \
                 slug VARCHAR(190) NOT NULL UNIQUE, \
                 title VARCHAR(190) NOT NULL, \
                 published BOOLEAN NOT NULL DEFAULT false, \
                 summary VARCHAR(60), \
                 user_id INT NOT NULL REFERENCES xtask_typeorm_diff_users(id) ON DELETE CASCADE \
             ); \
             CREATE INDEX idx_xtask_typeorm_diff_posts_user_id ON xtask_typeorm_diff_posts (user_id); \
             CREATE UNIQUE INDEX xtask_typeorm_diff_posts_user_title_uniq \
                 ON xtask_typeorm_diff_posts (user_id, title);",
        )
        .expect("create typeorm diff tables");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT \"ix\".\"relname\" AS \"constraint_name\", \
                \"a\".\"attname\" AS \"column_name\" \
           FROM \"pg_class\" \"t\" \
           INNER JOIN \"pg_index\" \"i\" ON \"i\".\"indrelid\" = \"t\".\"oid\" \
           INNER JOIN \"pg_attribute\" \"a\" ON \"a\".\"attrelid\" = \"t\".\"oid\" AND \"a\".\"attnum\" = ANY (\"i\".\"indkey\") \
           INNER JOIN \"pg_namespace\" \"ns\" ON \"ns\".\"oid\" = \"t\".\"relnamespace\" \
           INNER JOIN \"pg_class\" \"ix\" ON \"ix\".\"oid\" = \"i\".\"indexrelid\" \
          WHERE \"t\".\"relkind\" IN ('r', 'p') \
            AND \"ix\".\"relkind\" IN ('i', 'I') \
            AND \"ns\".\"nspname\" = 'public' \
            AND \"t\".\"relname\" = 'xtask_typeorm_diff_posts' \
            AND \"ix\".\"relname\" = 'xtask_typeorm_diff_posts_user_title_uniq'",
    );

    assert_eq!(
        rows.iter()
            .map(|row| (text_col(row, 0).to_owned(), text_col(row, 1).to_owned()))
            .collect::<Vec<_>>(),
        vec![
            (
                "xtask_typeorm_diff_posts_user_title_uniq".to_owned(),
                "user_id".to_owned(),
            ),
            (
                "xtask_typeorm_diff_posts_user_title_uniq".to_owned(),
                "title".to_owned(),
            ),
        ]
    );
}

#[test]
fn simple_case_coerces_boolean_text_branches_for_pg_catalog_shapes() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let rows = query_rows(
        &engine,
        &session,
        "SELECT CASE TRUE WHEN 't' THEN 'TRUE' ELSE 'FALSE' END, \
                CASE FALSE WHEN 't' THEN 'TRUE' ELSE 'FALSE' END",
    );
    assert_eq!(text_col(&rows[0], 0), "TRUE");
    assert_eq!(text_col(&rows[0], 1), "FALSE");
}

#[test]
fn sqlalchemy_pk_reflection_join_matches_pg_attribute_columns() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE orm_parent ( \
                 id INT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, \
                 name TEXT NOT NULL UNIQUE, \
                 note TEXT DEFAULT 'x' \
             ); \
             CREATE TABLE orm_child ( \
                 id INT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, \
                 parent_id INT NOT NULL REFERENCES orm_parent(id), \
                 slug TEXT NOT NULL, \
                 qty INT DEFAULT 0, \
                 CONSTRAINT orm_child_slug_unique UNIQUE (slug) \
             );",
        )
        .expect("create tables");

    let pk_rows = query_rows(
        &engine,
        &session,
        "SELECT con.conname, a.attname \
         FROM pg_catalog.pg_attribute a \
         JOIN ( \
           SELECT c.conrelid AS conrelid, \
                  c.conname AS conname, \
                  unnest(i.indkey) AS attnum, \
                  generate_subscripts(i.indkey, 1) AS ord \
           FROM pg_catalog.pg_constraint c \
           JOIN pg_catalog.pg_index i ON c.conindid = i.indexrelid \
           WHERE c.contype = 'p' \
             AND c.conrelid IN ('orm_parent'::regclass, 'orm_child'::regclass) \
         ) con ON a.attnum = con.attnum AND a.attrelid = con.conrelid \
         WHERE con.conrelid IN ('orm_parent'::regclass, 'orm_child'::regclass) \
         ORDER BY con.conname, con.ord",
    );
    assert_eq!(pk_rows.len(), 2, "expected both PK columns to be visible");
    assert_eq!(text_col(&pk_rows[0], 1), "id");
    assert_eq!(text_col(&pk_rows[1], 1), "id");

    let unique_rows = query_rows(
        &engine,
        &session,
        "SELECT con.conname, a.attname \
         FROM pg_catalog.pg_attribute a \
         JOIN ( \
           SELECT c.conrelid AS conrelid, \
                  c.conname AS conname, \
                  unnest(i.indkey) AS attnum, \
                  generate_subscripts(i.indkey, 1) AS ord \
           FROM pg_catalog.pg_constraint c \
           JOIN pg_catalog.pg_index i ON c.conindid = i.indexrelid \
           WHERE c.contype = 'u' \
             AND c.conrelid IN ('orm_parent'::regclass, 'orm_child'::regclass) \
         ) con ON a.attnum = con.attnum AND a.attrelid = con.conrelid \
         WHERE con.conrelid IN ('orm_parent'::regclass, 'orm_child'::regclass) \
         ORDER BY con.conname, con.ord",
    );
    assert_eq!(
        unique_rows.len(),
        2,
        "expected both UNIQUE constraint columns to be visible"
    );
    assert_eq!(text_col(&unique_rows[0], 1), "slug");
    assert_eq!(text_col(&unique_rows[1], 1), "name");
}

#[test]
fn sqlalchemy_fk_reflection_with_repeated_pg_namespace_binding_returns_one_row() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE orm_parent ( \
                 id INT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, \
                 name TEXT NOT NULL UNIQUE \
             ); \
             CREATE TABLE orm_child ( \
                 id INT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, \
                 parent_id INT NOT NULL REFERENCES orm_parent(id), \
                 slug TEXT NOT NULL, \
                 CONSTRAINT orm_child_slug_unique UNIQUE (slug) \
             );",
        )
        .expect("create tables");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT pg_catalog.pg_class.relname, \
                pg_catalog.pg_constraint.conname, \
                CASE WHEN (pg_catalog.pg_constraint.oid IS NOT NULL) \
                    THEN pg_catalog.pg_get_constraintdef(pg_catalog.pg_constraint.oid, true) \
                END AS anon_1, \
                nsp_ref.nspname, \
                pg_catalog.pg_description.description \
         FROM pg_catalog.pg_class \
         LEFT OUTER JOIN pg_catalog.pg_constraint \
           ON pg_catalog.pg_class.oid = pg_catalog.pg_constraint.conrelid \
          AND pg_catalog.pg_constraint.contype = 'f' \
         LEFT OUTER JOIN pg_catalog.pg_class AS cls_ref \
           ON cls_ref.oid = pg_catalog.pg_constraint.confrelid \
         LEFT OUTER JOIN pg_catalog.pg_namespace AS nsp_ref \
           ON cls_ref.relnamespace = nsp_ref.oid \
         LEFT OUTER JOIN pg_catalog.pg_description \
           ON pg_catalog.pg_description.objoid = pg_catalog.pg_constraint.oid \
         JOIN pg_catalog.pg_namespace \
           ON pg_catalog.pg_namespace.oid = pg_catalog.pg_class.relnamespace \
         WHERE pg_catalog.pg_class.relkind = ANY (ARRAY['r'::VARCHAR, 'p'::VARCHAR, 'f'::VARCHAR, 'v'::VARCHAR, 'm'::VARCHAR]) \
           AND pg_catalog.pg_table_is_visible(pg_catalog.pg_class.oid) \
           AND pg_catalog.pg_namespace.nspname != 'pg_catalog'::VARCHAR \
           AND pg_catalog.pg_class.relname IN ('orm_child'::VARCHAR) \
         ORDER BY pg_catalog.pg_class.relname, pg_catalog.pg_constraint.conname",
    );
    assert_eq!(rows.len(), 1, "expected exactly one reflected FK row");
    assert_eq!(text_col(&rows[0], 0), "orm_child");
    assert_eq!(text_col(&rows[0], 1), "orm_child_parent_id_fkey");
    assert_eq!(
        text_col(&rows[0], 2),
        "FOREIGN KEY (parent_id) REFERENCES orm_parent(id)"
    );
    assert_eq!(text_col(&rows[0], 3), "public");
}

#[test]
fn sqlalchemy_unique_constraint_names_match_underlying_index_names() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE orm_parent ( \
                 id INT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, \
                 name TEXT NOT NULL UNIQUE \
             ); \
             CREATE TABLE orm_child ( \
                 id INT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, \
                 parent_id INT NOT NULL REFERENCES orm_parent(id), \
                 slug TEXT NOT NULL UNIQUE \
             );",
        )
        .expect("create tables");

    let information_schema_rows = query_rows(
        &engine,
        &session,
        "SELECT constraint_name \
           FROM information_schema.table_constraints \
          WHERE constraint_type = 'UNIQUE' \
            AND table_name IN ('orm_parent', 'orm_child') \
          ORDER BY constraint_name",
    );
    let information_schema_names = information_schema_rows
        .iter()
        .map(|row| text_col(row, 0).to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        information_schema_names,
        vec![
            "orm_child_slug_unique".to_owned(),
            "orm_parent_name_unique".to_owned(),
        ]
    );
}

#[test]
fn sqlalchemy_foreign_key_reflection_preserves_explicit_composite_constraint_names() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA comp; \
             CREATE TABLE comp.parent ( \
                 a INT NOT NULL, \
                 b INT NOT NULL, \
                 CONSTRAINT parent_pk PRIMARY KEY (a, b) \
             ); \
             CREATE TABLE comp.child ( \
                 id INT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, \
                 pa INT NOT NULL, \
                 pb INT NOT NULL, \
                 CONSTRAINT child_parent_fk FOREIGN KEY (pa, pb) REFERENCES comp.parent(a, b) \
             );",
        )
        .expect("create composite fk tables");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT conname, pg_catalog.pg_get_constraintdef(oid, true) \
           FROM pg_catalog.pg_constraint \
          WHERE conrelid = 'comp.child'::regclass \
            AND contype = 'f'",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(text_col(&rows[0], 0), "child_parent_fk");
    assert_eq!(
        text_col(&rows[0], 1),
        "FOREIGN KEY (pa, pb) REFERENCES comp.parent(a, b)"
    );
}

#[test]
fn pg_get_constraintdef_stays_aligned_when_plain_unique_indexes_exist() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE orm_align_users ( \
                 id INT NOT NULL PRIMARY KEY, \
                 email TEXT NOT NULL \
             ); \
             CREATE UNIQUE INDEX orm_align_users_email_idx ON orm_align_users (email); \
             CREATE TABLE orm_align_posts ( \
                 id INT NOT NULL PRIMARY KEY, \
                 user_id INT NOT NULL REFERENCES orm_align_users(id) \
             );",
        )
        .expect("create alignment probe tables");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT conname, contype, pg_catalog.pg_get_constraintdef(oid, true) \
           FROM pg_catalog.pg_constraint \
          WHERE conrelid IN ('orm_align_users'::regclass, 'orm_align_posts'::regclass) \
          ORDER BY oid",
    );
    assert_eq!(rows.len(), 3);
    assert_eq!(text_col(&rows[0], 0), "orm_align_users_pkey");
    assert_eq!(text_col(&rows[0], 1), "p");
    assert_eq!(text_col(&rows[0], 2), "PRIMARY KEY (id)");
    assert_eq!(text_col(&rows[1], 0), "orm_align_posts_pkey");
    assert_eq!(text_col(&rows[1], 1), "p");
    assert_eq!(text_col(&rows[1], 2), "PRIMARY KEY (id)");
    assert_eq!(text_col(&rows[2], 0), "orm_align_posts_user_id_fkey");
    assert_eq!(text_col(&rows[2], 1), "f");
    assert_eq!(
        text_col(&rows[2], 2),
        "FOREIGN KEY (user_id) REFERENCES orm_align_users(id)"
    );
}

#[test]
fn pg_table_is_visible_hides_non_search_path_relations_for_orm_reflection() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE visible_public (id INT); \
             CREATE TABLE shadowed_name (id INT); \
             CREATE SCHEMA app; \
             CREATE TABLE app.hidden_app (id INT); \
             CREATE TABLE app.shadowed_name (id INT);",
        )
        .expect("create visibility probe tables");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT c.relname, pg_catalog.pg_table_is_visible(c.oid) \
           FROM pg_catalog.pg_class c \
           JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
          WHERE n.nspname IN ('public', 'app') \
            AND c.relname IN ('visible_public', 'hidden_app', 'shadowed_name') \
          ORDER BY c.relname, n.nspname",
    );
    assert_eq!(rows.len(), 4);
    assert_eq!(text_col(&rows[0], 0), "hidden_app");
    assert_eq!(rows[0].values[1], aiondb_core::Value::Boolean(false));
    assert_eq!(text_col(&rows[1], 0), "shadowed_name");
    assert_eq!(rows[1].values[1], aiondb_core::Value::Boolean(false));
    assert_eq!(text_col(&rows[2], 0), "shadowed_name");
    assert_eq!(rows[2].values[1], aiondb_core::Value::Boolean(true));
    assert_eq!(text_col(&rows[3], 0), "visible_public");
    assert_eq!(rows[3].values[1], aiondb_core::Value::Boolean(true));

    engine
        .execute_sql(&session, "SET search_path TO app, public")
        .expect("set search path");
    let search_path_rows = query_rows(
        &engine,
        &session,
        "SELECT c.relname, pg_catalog.pg_table_is_visible(c.oid) \
           FROM pg_catalog.pg_class c \
           JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
          WHERE n.nspname IN ('public', 'app') \
            AND c.relname IN ('visible_public', 'hidden_app', 'shadowed_name') \
          ORDER BY c.relname, n.nspname",
    );
    assert_eq!(search_path_rows.len(), 4);
    assert_eq!(
        search_path_rows[0].values[1],
        aiondb_core::Value::Boolean(true)
    );
    assert_eq!(
        search_path_rows[1].values[1],
        aiondb_core::Value::Boolean(true)
    );
    assert_eq!(
        search_path_rows[2].values[1],
        aiondb_core::Value::Boolean(false)
    );
    assert_eq!(
        search_path_rows[3].values[1],
        aiondb_core::Value::Boolean(true)
    );
}

#[test]
fn sqlalchemy_column_reflection_default_subquery_reads_pg_attrdef_for_each_outer_row() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE orm_parent ( \
                 id INT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, \
                 name TEXT NOT NULL UNIQUE, \
                 note TEXT DEFAULT 'x' \
             ); \
             CREATE TABLE orm_child ( \
                 id INT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, \
                 parent_id INT NOT NULL REFERENCES orm_parent(id), \
                 slug TEXT NOT NULL, \
                 qty INT DEFAULT 0 \
             ); \
             CREATE TABLE orm_grandchild ( \
                 id INT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, \
                 child_id INT NOT NULL REFERENCES orm_child(id), \
                 code TEXT NOT NULL \
             );",
        )
        .expect("create tables");

    let attrdef_rows = query_rows(
        &engine,
        &session,
        "SELECT adrelid, adnum, adbin \
           FROM pg_catalog.pg_attrdef \
          ORDER BY adrelid, adnum",
    );
    assert_eq!(attrdef_rows.len(), 5, "expected all defaults in pg_attrdef");

    let scalar_rows = query_rows(
        &engine,
        &session,
        "SELECT \
            (SELECT count(*) FROM pg_catalog.pg_attrdef WHERE adrelid = 'orm_child'::regclass AND adnum = 1), \
            (SELECT count(*) FROM pg_catalog.pg_attrdef WHERE adrelid = 'orm_child'::regclass AND adnum = 4), \
            (SELECT count(*) FROM pg_catalog.pg_attrdef WHERE adrelid = 'orm_grandchild'::regclass AND adnum = 1), \
            (SELECT count(*) FROM pg_catalog.pg_attrdef WHERE adrelid = 'orm_parent'::regclass AND adnum = 1)",
    );
    assert_eq!(scalar_rows.len(), 1);
    assert_eq!(bigint_col(&scalar_rows[0], 0), 1);
    assert_eq!(bigint_col(&scalar_rows[0], 1), 1);
    assert_eq!(bigint_col(&scalar_rows[0], 2), 1);
    assert_eq!(bigint_col(&scalar_rows[0], 3), 1);

    let rows = query_rows(
        &engine,
        &session,
        "SELECT c.relname, \
                a.attnum, \
                a.attname, \
                a.atthasdef, \
                (SELECT count(*) \
                   FROM pg_catalog.pg_attrdef d \
                  WHERE d.adrelid = a.attrelid \
                    AND d.adnum = a.attnum) AS def_count, \
                (SELECT pg_catalog.pg_get_expr(d.adbin, d.adrelid) \
                   FROM pg_catalog.pg_attrdef d \
                  WHERE d.adrelid = a.attrelid \
                    AND d.adnum = a.attnum \
                    AND a.atthasdef) AS def_expr \
           FROM pg_catalog.pg_class c \
           LEFT JOIN pg_catalog.pg_attribute a \
             ON c.oid = a.attrelid \
            AND a.attnum > 0 \
            AND NOT a.attisdropped \
          WHERE c.relname IN ('orm_parent', 'orm_child', 'orm_grandchild') \
          ORDER BY c.relname, a.attnum",
    );

    let actual = rows
        .iter()
        .map(|row| {
            (
                text_col(row, 0).to_owned(),
                int_col(row, 1),
                text_col(row, 2).to_owned(),
                bool_col(row, 3),
                bigint_col(row, 4),
                row.values[5].clone(),
            )
        })
        .collect::<Vec<_>>();

    assert_eq!(
        actual,
        vec![
            (
                "orm_child".to_owned(),
                1,
                "id".to_owned(),
                true,
                1,
                Value::Text("nextval('orm_child_id_seq')".to_owned()),
            ),
            (
                "orm_child".to_owned(),
                2,
                "parent_id".to_owned(),
                false,
                0,
                Value::Null,
            ),
            (
                "orm_child".to_owned(),
                3,
                "slug".to_owned(),
                false,
                0,
                Value::Null,
            ),
            (
                "orm_child".to_owned(),
                4,
                "qty".to_owned(),
                true,
                1,
                Value::Text("0".to_owned()),
            ),
            (
                "orm_grandchild".to_owned(),
                1,
                "id".to_owned(),
                true,
                1,
                Value::Text("nextval('orm_grandchild_id_seq')".to_owned()),
            ),
            (
                "orm_grandchild".to_owned(),
                2,
                "child_id".to_owned(),
                false,
                0,
                Value::Null,
            ),
            (
                "orm_grandchild".to_owned(),
                3,
                "code".to_owned(),
                false,
                0,
                Value::Null,
            ),
            (
                "orm_parent".to_owned(),
                1,
                "id".to_owned(),
                true,
                1,
                Value::Text("nextval('orm_parent_id_seq')".to_owned()),
            ),
            (
                "orm_parent".to_owned(),
                2,
                "name".to_owned(),
                false,
                0,
                Value::Null,
            ),
            (
                "orm_parent".to_owned(),
                3,
                "note".to_owned(),
                true,
                1,
                Value::Text("'x'".to_owned()),
            ),
        ]
    );
}

#[test]
fn django_fk_reflection_correlated_catalog_subquery_keeps_outer_oid_value() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE django_ref_parent (id INT PRIMARY KEY); \
             CREATE TABLE django_ref_child ( \
                 id INT PRIMARY KEY, \
                 parent_id INT NOT NULL REFERENCES django_ref_parent(id) \
             );",
        )
        .expect("create django reflection tables");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT c.conname, \
                (SELECT relname FROM pg_catalog.pg_class WHERE oid = c.confrelid) \
           FROM pg_catalog.pg_constraint c \
           JOIN pg_catalog.pg_class cl ON c.conrelid = cl.oid \
          WHERE cl.relname = 'django_ref_child' \
          ORDER BY 1",
    );

    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0].values,
        vec![
            Value::Text("django_ref_child_parent_id_fkey".to_owned()),
            Value::Text("django_ref_parent".to_owned()),
        ]
    );
    assert_eq!(
        rows[1].values,
        vec![Value::Text("django_ref_child_pkey".to_owned()), Value::Null]
    );
}

#[test]
fn prepared_django_constraint_reflection_supports_with_ordinality() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE django_portal_parent (id INT PRIMARY KEY); \
             CREATE TABLE django_portal_child ( \
                 id INT PRIMARY KEY, \
                 parent_id INT NOT NULL REFERENCES django_portal_parent(id), \
                 slug TEXT UNIQUE \
             );",
        )
        .expect("create portal reflection tables");

    let sql = "SELECT c.conname, \
                      array( \
                          SELECT attname \
                            FROM unnest(c.conkey) WITH ORDINALITY cols(colid, arridx) \
                            JOIN pg_attribute AS ca ON cols.colid = ca.attnum \
                           WHERE ca.attrelid = c.conrelid \
                           ORDER BY cols.arridx \
                      ), \
                      c.contype, \
                      (SELECT fkc.relname || '.' || fka.attname \
                         FROM pg_attribute AS fka \
                         JOIN pg_class AS fkc ON fka.attrelid = fkc.oid \
                        WHERE fka.attrelid = c.confrelid \
                          AND fka.attnum = c.confkey[1]), \
                      cl.reloptions \
                 FROM pg_constraint AS c \
                 JOIN pg_class AS cl ON c.conrelid = cl.oid \
                WHERE cl.relname = $1 \
                  AND pg_catalog.pg_table_is_visible(cl.oid) \
                ORDER BY 1";
    engine
        .prepare(
            &session,
            "django_constraints_stmt".to_owned(),
            sql.to_owned(),
        )
        .expect("prepare django constraint reflection");
    engine
        .bind(
            &session,
            "django_constraints_portal".to_owned(),
            "django_constraints_stmt".to_owned(),
            vec![Value::Text("django_portal_child".to_owned())],
        )
        .expect("bind django constraint reflection");
    let batch = engine
        .execute_portal(&session, "django_constraints_portal", 0)
        .expect("execute django constraint reflection");

    assert_eq!(batch.rows.len(), 3);
    assert_eq!(
        text_col(&batch.rows[0], 0),
        "django_portal_child_parent_id_fkey"
    );
    assert_eq!(
        &batch.rows[0].values[1],
        &Value::Array(vec![Value::Text("parent_id".to_owned())])
    );
    assert_eq!(text_col(&batch.rows[0], 3), "django_portal_parent.id");
    assert_eq!(batch.rows[0].values[4], Value::Null);
}

#[test]
fn deferred_prepared_django_constraint_reflection_supports_with_ordinality() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE django_deferred_parent (id INT PRIMARY KEY); \
             CREATE TABLE django_deferred_child ( \
                 id INT PRIMARY KEY, \
                 parent_id INT NOT NULL REFERENCES django_deferred_parent(id), \
                 slug TEXT UNIQUE \
             );",
        )
        .expect("create deferred reflection tables");

    let sql = "SELECT c.conname, \
                      array( \
                          SELECT attname \
                            FROM unnest(c.conkey) WITH ORDINALITY cols(colid, arridx) \
                            JOIN pg_attribute AS ca ON cols.colid = ca.attnum \
                           WHERE ca.attrelid = c.conrelid \
                           ORDER BY cols.arridx \
                      ), \
                      c.contype, \
                      (SELECT fkc.relname || '.' || fka.attname \
                         FROM pg_attribute AS fka \
                         JOIN pg_class AS fkc ON fka.attrelid = fkc.oid \
                        WHERE fka.attrelid = c.confrelid \
                          AND fka.attnum = c.confkey[1]), \
                      cl.reloptions \
                 FROM pg_constraint AS c \
                 JOIN pg_class AS cl ON c.conrelid = cl.oid \
                WHERE cl.relname = $1 \
                  AND pg_catalog.pg_table_is_visible(cl.oid) \
                ORDER BY 1";
    engine
        .prepare(
            &session,
            "django_constraints_deferred_stmt".to_owned(),
            sql.to_owned(),
        )
        .expect("prepare deferred django constraint reflection");
    let (batch, notices) = engine
        .execute_prepared_statement_with_notices(
            &session,
            "django_constraints_deferred_stmt".to_owned(),
            vec![Value::Text("django_deferred_child".to_owned())],
            0,
        )
        .expect("execute deferred django constraint reflection");

    assert!(notices.is_empty());
    assert_eq!(batch.rows.len(), 3);
    assert_eq!(
        text_col(&batch.rows[0], 0),
        "django_deferred_child_parent_id_fkey"
    );
    assert_eq!(
        &batch.rows[0].values[1],
        &Value::Array(vec![Value::Text("parent_id".to_owned())])
    );
    assert_eq!(text_col(&batch.rows[0], 3), "django_deferred_parent.id");
    assert_eq!(batch.rows[0].values[4], Value::Null);
}

#[test]
fn hinted_prepared_django_constraint_reflection_supports_with_ordinality() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE django_hint_parent (id INT PRIMARY KEY); \
             CREATE TABLE django_hint_child ( \
                 id INT PRIMARY KEY, \
                 parent_id INT NOT NULL REFERENCES django_hint_parent(id), \
                 slug TEXT UNIQUE \
             );",
        )
        .expect("create hinted reflection tables");

    let sql = "SELECT c.conname, \
                      array( \
                          SELECT attname \
                            FROM unnest(c.conkey) WITH ORDINALITY cols(colid, arridx) \
                            JOIN pg_attribute AS ca ON cols.colid = ca.attnum \
                           WHERE ca.attrelid = c.conrelid \
                           ORDER BY cols.arridx \
                      ), \
                      c.contype, \
                      (SELECT fkc.relname || '.' || fka.attname \
                         FROM pg_attribute AS fka \
                         JOIN pg_class AS fkc ON fka.attrelid = fkc.oid \
                        WHERE fka.attrelid = c.confrelid \
                          AND fka.attnum = c.confkey[1]), \
                      cl.reloptions \
                 FROM pg_constraint AS c \
                 JOIN pg_class AS cl ON c.conrelid = cl.oid \
                WHERE cl.relname = $1 \
                  AND pg_catalog.pg_table_is_visible(cl.oid) \
                ORDER BY 1";
    engine
        .prepare_with_param_hints(
            &session,
            "django_constraints_hinted_stmt".to_owned(),
            sql.to_owned(),
            vec![Some(DataType::Text)],
        )
        .expect("prepare hinted django constraint reflection");
    let (batch, notices) = engine
        .execute_prepared_statement_with_notices(
            &session,
            "django_constraints_hinted_stmt".to_owned(),
            vec![Value::Text("django_hint_child".to_owned())],
            0,
        )
        .expect("execute hinted django constraint reflection");

    assert!(notices.is_empty());
    assert_eq!(batch.rows.len(), 3);
    assert_eq!(
        text_col(&batch.rows[0], 0),
        "django_hint_child_parent_id_fkey"
    );
    assert_eq!(
        &batch.rows[0].values[1],
        &Value::Array(vec![Value::Text("parent_id".to_owned())])
    );
    assert_eq!(text_col(&batch.rows[0], 3), "django_hint_parent.id");
    assert_eq!(batch.rows[0].values[4], Value::Null);
}

#[test]
fn compat_execute_django_constraint_reflection_supports_with_ordinality() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE django_compat_parent (id INT PRIMARY KEY); \
             CREATE TABLE django_compat_child ( \
                 id INT PRIMARY KEY, \
                 parent_id INT NOT NULL REFERENCES django_compat_parent(id), \
                 slug TEXT UNIQUE \
             );",
        )
        .expect("create compat reflection tables");

    engine
        .execute_sql(
            &session,
            "PREPARE django_constraints_compat(text) AS \
             SELECT c.conname, \
                    array( \
                        SELECT attname \
                          FROM unnest(c.conkey) WITH ORDINALITY cols(colid, arridx) \
                          JOIN pg_attribute AS ca ON cols.colid = ca.attnum \
                         WHERE ca.attrelid = c.conrelid \
                         ORDER BY cols.arridx \
                    ), \
                    c.contype, \
                    (SELECT fkc.relname || '.' || fka.attname \
                       FROM pg_attribute AS fka \
                       JOIN pg_class AS fkc ON fka.attrelid = fkc.oid \
                      WHERE fka.attrelid = c.confrelid \
                        AND fka.attnum = c.confkey[1]), \
                    cl.reloptions \
               FROM pg_constraint AS c \
               JOIN pg_class AS cl ON c.conrelid = cl.oid \
              WHERE cl.relname = $1 \
                AND pg_catalog.pg_table_is_visible(cl.oid) \
              ORDER BY 1",
        )
        .expect("compat prepare django constraint reflection");

    let results = engine
        .execute_sql(
            &session,
            "EXECUTE django_constraints_compat('django_compat_child')",
        )
        .expect("compat execute django constraint reflection");
    let Some(StatementResult::Query { rows, .. }) = results.first() else {
        panic!("expected query result from compat execute");
    };

    assert_eq!(rows.len(), 3);
    assert_eq!(text_col(&rows[0], 0), "django_compat_child_parent_id_fkey");
    assert_eq!(
        &rows[0].values[1],
        &Value::Array(vec![Value::Text("parent_id".to_owned())])
    );
    assert_eq!(text_col(&rows[0], 3), "django_compat_parent.id");
    assert_eq!(rows[0].values[4], Value::Null);
}

#[test]
fn unnamed_prepared_django_constraint_reflection_supports_with_ordinality() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE django_unnamed_parent (id INT PRIMARY KEY); \
             CREATE TABLE django_unnamed_child ( \
                 id INT PRIMARY KEY, \
                 parent_id INT NOT NULL REFERENCES django_unnamed_parent(id), \
                 slug TEXT UNIQUE \
             );",
        )
        .expect("create unnamed reflection tables");

    let sql = "SELECT c.conname, \
                      array( \
                          SELECT attname \
                            FROM unnest(c.conkey) WITH ORDINALITY cols(colid, arridx) \
                            JOIN pg_attribute AS ca ON cols.colid = ca.attnum \
                           WHERE ca.attrelid = c.conrelid \
                           ORDER BY cols.arridx \
                      ), \
                      c.contype, \
                      (SELECT fkc.relname || '.' || fka.attname \
                         FROM pg_attribute AS fka \
                         JOIN pg_class AS fkc ON fka.attrelid = fkc.oid \
                        WHERE fka.attrelid = c.confrelid \
                          AND fka.attnum = c.confkey[1]), \
                      cl.reloptions \
                 FROM pg_constraint AS c \
                 JOIN pg_class AS cl ON c.conrelid = cl.oid \
                WHERE cl.relname = $1 \
                  AND pg_catalog.pg_table_is_visible(cl.oid)";
    engine
        .prepare_with_param_hints(&session, String::new(), sql.to_owned(), vec![None])
        .expect("prepare unnamed django constraint reflection");
    let (batch, notices) = engine
        .execute_prepared_statement_with_notices(
            &session,
            String::new(),
            vec![Value::Text("django_unnamed_child".to_owned())],
            0,
        )
        .expect("execute unnamed django constraint reflection");

    assert!(notices.is_empty());
    assert_eq!(batch.rows.len(), 3);
    let names: Vec<&str> = batch.rows.iter().map(|row| text_col(row, 0)).collect();
    assert!(names.contains(&"django_unnamed_child_parent_id_fkey"));
}

#[test]
fn simple_sql_django_constraint_reflection_supports_with_ordinality() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE django_simple_parent (id INT PRIMARY KEY); \
             CREATE TABLE django_simple_child ( \
                 id INT PRIMARY KEY, \
                 parent_id INT NOT NULL REFERENCES django_simple_parent(id), \
                 slug TEXT UNIQUE \
             );",
        )
        .expect("create simple reflection tables");

    let results = engine
        .execute_sql(
            &session,
            "SELECT c.conname, \
                    array( \
                        SELECT attname \
                          FROM unnest(c.conkey) WITH ORDINALITY cols(colid, arridx) \
                          JOIN pg_attribute AS ca ON cols.colid = ca.attnum \
                         WHERE ca.attrelid = c.conrelid \
                         ORDER BY cols.arridx \
                    ), \
                    c.contype, \
                    (SELECT fkc.relname || '.' || fka.attname \
                       FROM pg_attribute AS fka \
                       JOIN pg_class AS fkc ON fka.attrelid = fkc.oid \
                      WHERE fka.attrelid = c.confrelid \
                        AND fka.attnum = c.confkey[1]), \
                    cl.reloptions \
               FROM pg_constraint AS c \
               JOIN pg_class AS cl ON c.conrelid = cl.oid \
              WHERE cl.relname = 'django_simple_child' \
                AND pg_catalog.pg_table_is_visible(cl.oid)",
        )
        .expect("simple sql django constraint reflection");
    let Some(StatementResult::Query { rows, .. }) = results.first() else {
        panic!("expected query result from simple SQL reflection");
    };

    assert_eq!(rows.len(), 3);
    let names: Vec<&str> = rows.iter().map(|row| text_col(row, 0)).collect();
    assert!(names.contains(&"django_simple_child_pkey"));
}

#[test]
fn unnamed_literal_prepared_django_constraint_reflection_supports_with_ordinality() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE django_client_parent (id INT PRIMARY KEY); \
             CREATE TABLE django_client_child ( \
                 id INT PRIMARY KEY, \
                 parent_id INT NOT NULL REFERENCES django_client_parent(id), \
                 slug TEXT UNIQUE \
             );",
        )
        .expect("create client cursor reflection tables");

    let sql = "SELECT c.conname, \
                      array( \
                          SELECT attname \
                            FROM unnest(c.conkey) WITH ORDINALITY cols(colid, arridx) \
                            JOIN pg_attribute AS ca ON cols.colid = ca.attnum \
                           WHERE ca.attrelid = c.conrelid \
                           ORDER BY cols.arridx \
                      ), \
                      c.contype, \
                      (SELECT fkc.relname || '.' || fka.attname \
                         FROM pg_attribute AS fka \
                         JOIN pg_class AS fkc ON fka.attrelid = fkc.oid \
                        WHERE fka.attrelid = c.confrelid \
                          AND fka.attnum = c.confkey[1]), \
                      cl.reloptions \
                 FROM pg_constraint AS c \
                 JOIN pg_class AS cl ON c.conrelid = cl.oid \
                WHERE cl.relname = 'django_client_child' \
                  AND pg_catalog.pg_table_is_visible(cl.oid)";
    engine
        .prepare(&session, String::new(), sql.to_owned())
        .expect("prepare unnamed literal django constraint reflection");
    let (batch, notices) = engine
        .execute_prepared_statement_with_notices(&session, String::new(), vec![], 0)
        .expect("execute unnamed literal django constraint reflection");

    assert!(notices.is_empty());
    assert_eq!(batch.rows.len(), 3);
    let names: Vec<&str> = batch.rows.iter().map(|row| text_col(row, 0)).collect();
    assert!(names.contains(&"django_client_child_parent_id_fkey"));
}

#[test]
fn sqlalchemy_enum_reflection_query_sees_public_enum_types_with_labels() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session_a, _) = engine.startup(startup_params()).expect("startup A");
    engine
        .execute_sql(
            &session_a,
            "CREATE TYPE status_enum_auto AS ENUM ('pending', 'ready', 'done'); \
             CREATE TABLE enum_probe (id INT PRIMARY KEY, status status_enum_auto NOT NULL);",
        )
        .expect("create enum and table");

    let (session_b, _) = engine.startup(startup_params()).expect("startup B");
    let rows = query_rows(
        &engine,
        &session_b,
        "SELECT pg_catalog.pg_type.typname AS name, \
                pg_catalog.pg_type_is_visible(pg_catalog.pg_type.oid) AS visible, \
                pg_catalog.pg_namespace.nspname AS schema, \
                lbl_agg.labels AS labels \
           FROM pg_catalog.pg_type \
           JOIN pg_catalog.pg_namespace \
             ON pg_catalog.pg_namespace.oid = pg_catalog.pg_type.typnamespace \
           LEFT OUTER JOIN ( \
                SELECT pg_catalog.pg_enum.enumtypid AS enumtypid, \
                       array_agg(CAST(pg_catalog.pg_enum.enumlabel AS TEXT) ORDER BY pg_catalog.pg_enum.enumsortorder) AS labels \
                  FROM pg_catalog.pg_enum \
                 GROUP BY pg_catalog.pg_enum.enumtypid \
           ) AS lbl_agg \
             ON pg_catalog.pg_type.oid = lbl_agg.enumtypid \
          WHERE pg_catalog.pg_type.typtype = 'e' \
            AND pg_catalog.pg_namespace.nspname = 'public' \
          ORDER BY pg_catalog.pg_namespace.nspname, pg_catalog.pg_type.typname",
    );
    assert_eq!(rows.len(), 1, "expected reflected enum row");
    assert_eq!(text_col(&rows[0], 0), "status_enum_auto");
    assert!(bool_col(&rows[0], 1), "enum should be visible");
    assert_eq!(text_col(&rows[0], 2), "public");
    assert_eq!(
        rows[0].values[3],
        Value::Array(vec![
            Value::Text("pending".to_owned()),
            Value::Text("ready".to_owned()),
            Value::Text("done".to_owned()),
        ])
    );

    let column_rows = query_rows(
        &engine,
        &session_b,
        "SELECT a.attname, pg_catalog.format_type(a.atttypid, a.atttypmod) \
           FROM pg_catalog.pg_class c \
           JOIN pg_catalog.pg_attribute a ON a.attrelid = c.oid \
          WHERE c.relname = 'enum_probe' \
            AND a.attnum > 0 \
            AND NOT a.attisdropped \
          ORDER BY a.attnum",
    );
    assert_eq!(column_rows.len(), 2);
    assert_eq!(text_col(&column_rows[0], 0), "id");
    assert_eq!(text_col(&column_rows[1], 0), "status");
    assert_eq!(text_col(&column_rows[1], 1), "status_enum_auto");
}

#[test]
fn prepared_pg_catalog_column_reflection_keeps_enum_format_type() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session_a, _) = engine.startup(startup_params()).expect("startup A");
    engine
        .execute_sql(
            &session_a,
            "CREATE TYPE portal_status_enum AS ENUM ('draft', 'live', 'archived'); \
             CREATE TABLE portal_enum_probe (id INT PRIMARY KEY, status portal_status_enum NOT NULL);",
        )
        .expect("create enum and table");

    let (session_b, _) = engine.startup(startup_params()).expect("startup B");
    engine
        .prepare(
            &session_b,
            "reflect_enum_cols".to_owned(),
            "SELECT pg_catalog.pg_attribute.attname, \
                    pg_catalog.format_type(pg_catalog.pg_attribute.atttypid, pg_catalog.pg_attribute.atttypmod) \
               FROM pg_catalog.pg_class \
               LEFT OUTER JOIN pg_catalog.pg_attribute \
                 ON pg_catalog.pg_class.oid = pg_catalog.pg_attribute.attrelid \
                AND pg_catalog.pg_attribute.attnum > 0 \
                AND NOT pg_catalog.pg_attribute.attisdropped \
               JOIN pg_catalog.pg_namespace \
                 ON pg_catalog.pg_namespace.oid = pg_catalog.pg_class.relnamespace \
              WHERE pg_catalog.pg_class.relkind = ANY (ARRAY['r','p','f','v','m']) \
                AND pg_catalog.pg_namespace.nspname = 'public' \
                AND pg_catalog.pg_class.relname = $1 \
              ORDER BY pg_catalog.pg_class.relname, pg_catalog.pg_attribute.attnum"
                .to_owned(),
        )
        .expect("prepare enum reflection query");
    engine
        .bind(
            &session_b,
            "reflect_enum_cols_portal".to_owned(),
            "reflect_enum_cols".to_owned(),
            vec![Value::Text("portal_enum_probe".to_owned())],
        )
        .expect("bind enum reflection query");
    let batch = engine
        .execute_portal(&session_b, "reflect_enum_cols_portal", 0)
        .expect("execute enum reflection portal");
    assert_eq!(batch.rows.len(), 2);
    assert_eq!(text_col(&batch.rows[0], 0), "id");
    assert_eq!(text_col(&batch.rows[0], 1), "integer");
    assert_eq!(text_col(&batch.rows[1], 0), "status");
    assert_eq!(text_col(&batch.rows[1], 1), "portal_status_enum");
}

#[test]
fn schema_qualified_enums_report_namespace_and_visibility_for_orm_reflection() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA app; \
             CREATE TYPE app.status_enum_s AS ENUM ('draft', 'live'); \
             CREATE TABLE app.enum_s_probe (id INT PRIMARY KEY, status app.status_enum_s NOT NULL);",
        )
        .expect("create schema-qualified enum");

    let enum_rows = query_rows(
        &engine,
        &session,
        "SELECT pg_catalog.pg_type.typname AS name, \
                pg_catalog.pg_type_is_visible(pg_catalog.pg_type.oid) AS visible, \
                pg_catalog.pg_namespace.nspname AS schema, \
                lbl_agg.labels AS labels \
           FROM pg_catalog.pg_type \
           JOIN pg_catalog.pg_namespace ON pg_catalog.pg_namespace.oid = pg_catalog.pg_type.typnamespace \
           LEFT OUTER JOIN ( \
                SELECT pg_catalog.pg_enum.enumtypid AS enumtypid, \
                       array_agg(CAST(pg_catalog.pg_enum.enumlabel AS TEXT) ORDER BY pg_catalog.pg_enum.enumsortorder) AS labels \
                  FROM pg_catalog.pg_enum \
                 GROUP BY pg_catalog.pg_enum.enumtypid \
           ) AS lbl_agg ON pg_catalog.pg_type.oid = lbl_agg.enumtypid \
          WHERE pg_catalog.pg_type.typtype = 'e' \
            AND pg_catalog.pg_namespace.nspname = 'app'",
    );
    assert_eq!(enum_rows.len(), 1);
    assert_eq!(text_col(&enum_rows[0], 0), "status_enum_s");
    assert!(
        !bool_col(&enum_rows[0], 1),
        "non-public enum should not be visible"
    );
    assert_eq!(text_col(&enum_rows[0], 2), "app");
    assert_eq!(
        enum_rows[0].values[3],
        Value::Array(vec![
            Value::Text("draft".to_owned()),
            Value::Text("live".to_owned()),
        ])
    );

    let column_rows = query_rows(
        &engine,
        &session,
        "SELECT pg_catalog.pg_attribute.attname, \
                pg_catalog.format_type(pg_catalog.pg_attribute.atttypid, pg_catalog.pg_attribute.atttypmod) \
           FROM pg_catalog.pg_class \
           JOIN pg_catalog.pg_attribute ON pg_catalog.pg_class.oid = pg_catalog.pg_attribute.attrelid \
          WHERE pg_catalog.pg_class.relname = 'enum_s_probe' \
            AND pg_catalog.pg_attribute.attnum > 0 \
            AND NOT pg_catalog.pg_attribute.attisdropped \
          ORDER BY pg_catalog.pg_attribute.attnum",
    );
    assert_eq!(column_rows.len(), 2);
    assert_eq!(text_col(&column_rows[1], 1), "app.status_enum_s");
}

#[test]
fn sqlalchemy_domain_reflection_query_sees_schema_qualified_domains_with_constraints() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session_a, _) = engine.startup(startup_params()).expect("startup A");
    engine
        .execute_sql(
            &session_a,
            "CREATE SCHEMA dom; \
             CREATE DOMAIN dom.email_t AS TEXT DEFAULT 'n/a' CHECK (POSITION('@' IN VALUE) > 1); \
             CREATE TABLE dom.users ( \
                 id INT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, \
                 email dom.email_t NOT NULL, \
                 note TEXT DEFAULT 'x' \
             );",
        )
        .expect("create schema-qualified domain");

    let (session_b, _) = engine.startup(startup_params()).expect("startup B");
    let domain_rows = query_rows(
        &engine,
        &session_b,
        "SELECT pg_catalog.pg_type.typname, \
                pg_catalog.pg_namespace.nspname, \
                pg_catalog.pg_type.typtype, \
                pg_catalog.format_type(pg_catalog.pg_type.typbasetype, pg_catalog.pg_type.typtypmod), \
                pg_catalog.pg_type.typdefault, \
                pg_catalog.pg_type_is_visible(pg_catalog.pg_type.oid) \
           FROM pg_catalog.pg_type \
           JOIN pg_catalog.pg_namespace ON pg_catalog.pg_namespace.oid = pg_catalog.pg_type.typnamespace \
          WHERE pg_catalog.pg_type.typtype = 'd' \
            AND pg_catalog.pg_type.typname = 'email_t'",
    );
    assert_eq!(domain_rows.len(), 1);
    assert_eq!(text_col(&domain_rows[0], 0), "email_t");
    assert_eq!(text_col(&domain_rows[0], 1), "dom");
    assert_eq!(text_col(&domain_rows[0], 2), "d");
    assert_eq!(text_col(&domain_rows[0], 3), "text");
    assert_eq!(text_col(&domain_rows[0], 4), "'n/a'");
    assert!(!bool_col(&domain_rows[0], 5));

    let constraint_rows = query_rows(
        &engine,
        &session_b,
        "SELECT pg_catalog.pg_get_constraintdef(pg_catalog.pg_constraint.oid, true) \
           FROM pg_catalog.pg_constraint \
           JOIN pg_catalog.pg_type ON pg_catalog.pg_type.oid = pg_catalog.pg_constraint.contypid \
          WHERE pg_catalog.pg_type.typname = 'email_t' \
          ORDER BY pg_catalog.pg_constraint.conname",
    );
    assert_eq!(constraint_rows.len(), 1);
    assert_eq!(
        text_col(&constraint_rows[0], 0),
        "CHECK (POSITION('@' IN VALUE) > 1)"
    );

    let column_rows = query_rows(
        &engine,
        &session_b,
        "SELECT pg_catalog.pg_attribute.attname, \
                pg_catalog.format_type(pg_catalog.pg_attribute.atttypid, pg_catalog.pg_attribute.atttypmod) \
           FROM pg_catalog.pg_class \
           JOIN pg_catalog.pg_attribute ON pg_catalog.pg_class.oid = pg_catalog.pg_attribute.attrelid \
          WHERE pg_catalog.pg_class.relname = 'users' \
            AND pg_catalog.pg_attribute.attnum > 0 \
            AND NOT pg_catalog.pg_attribute.attisdropped \
          ORDER BY pg_catalog.pg_attribute.attnum",
    );
    assert_eq!(column_rows.len(), 3);
    assert_eq!(text_col(&column_rows[1], 0), "email");
    assert_eq!(text_col(&column_rows[1], 1), "dom.email_t");
}
