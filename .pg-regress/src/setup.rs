use std::collections::BTreeSet;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use aiondb_engine::{Engine, QueryEngine, SessionHandle};
use aiondb_parser::Statement;

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SetupReport {
    pub(crate) attempted: usize,
    pub(crate) succeeded: usize,
    pub(crate) failed: usize,
}

#[derive(Clone, Copy, Debug)]
struct OnekTenkScale {
    onek_rows: usize,
    onek2_rows: usize,
    tenk1_rows: usize,
    tenk2_rows: usize,
}

impl Default for OnekTenkScale {
    fn default() -> Self {
        Self {
            onek_rows: 1000,
            onek2_rows: 1000,
            tenk1_rows: 10000,
            tenk2_rows: 10000,
        }
    }
}

impl OnekTenkScale {
    fn is_default(&self) -> bool {
        self.onek_rows == 1000
            && self.onek2_rows == 1000
            && self.tenk1_rows == 10000
            && self.tenk2_rows == 10000
    }
}

fn onek_tenk_scale_for_suite(_suite_name: Option<&str>) -> OnekTenkScale {
    // Keep canonical PostgreSQL cardinalities for real apples-to-apples runs.
    OnekTenkScale::default()
}

/// Create all setup tables needed by the PG regression test suite.
///
/// These mirror the tables created by test_setup.sql in the PG source tree,
/// plus additional geometry and cross-test tables that individual test files
/// create but that are referenced by *other* test files running earlier
/// (alphabetically).
///
/// Types not supported by AionDB (point, polygon, path, lseg, line, box,
/// circle, name) are mapped to text.
pub(crate) fn run_setup(
    engine: &Engine,
    session: &SessionHandle,
    shadowed_objects: &BTreeSet<String>,
    required_objects: Option<&BTreeSet<String>>,
    suite_name: Option<&str>,
) -> SetupReport {
    let onek_tenk_scale = onek_tenk_scale_for_suite(suite_name);
    let mut report = SetupReport::default();
    for (stmt, meta) in SETUP_STMTS.iter().zip(setup_stmt_metas().iter()) {
        if should_skip_setup_stmt(meta, shadowed_objects, required_objects) {
            continue;
        }
        report.attempted += 1;
        if engine.execute_sql(session, stmt).is_ok() {
            report.succeeded += 1;
        } else {
            report.failed += 1;
        }
    }

    if setup_scope_includes(
        required_objects,
        &[
            "date_tbl",
            "interval_tbl",
            "time_tbl",
            "timetz_tbl",
            "timestamp_tbl",
            "timestamptz_tbl",
        ],
    ) {
        crate::setup_temporal::populate_temporal_tables(
            engine,
            session,
            shadowed_objects,
            &mut report,
        );
    }

    // Real optimization: batch the heavy seed/data phases in one transaction
    // to reduce commit/fsync churn while keeping WAL durability enabled.
    let mut bulk_setup_tx_started = false;
    if !engine.has_active_transaction(session).unwrap_or(false)
        && engine.execute_sql(session, "BEGIN").is_ok()
    {
        if engine
            .execute_sql(session, "SAVEPOINT aiondb_setup_probe")
            .is_ok()
            && engine
                .execute_sql(session, "RELEASE SAVEPOINT aiondb_setup_probe")
                .is_ok()
        {
            bulk_setup_tx_started = true;
        } else {
            let _ = engine.execute_sql(session, "ROLLBACK");
        }
    }

    // Load canonical PostgreSQL regression seed data when available.
    if setup_scope_includes(required_objects, &["onek", "onek2", "tenk1", "tenk2"]) {
        populate_onek_tenk(
            engine,
            session,
            shadowed_objects,
            required_objects,
            &onek_tenk_scale,
            &mut report,
        );
    }

    // Load person / emp / student / stud_emp hierarchy data.
    if setup_scope_includes(required_objects, &["person", "emp", "student", "stud_emp"]) {
        populate_person_hierarchy(
            engine,
            session,
            shadowed_objects,
            required_objects,
            &mut report,
        );
    }

    // Simulate a_star inheritance: copy child rows into a_star.
    if setup_scope_includes(
        required_objects,
        &["a_star", "b_star", "c_star", "d_star", "e_star", "f_star"],
    ) {
        populate_a_star_hierarchy(
            engine,
            session,
            shadowed_objects,
            required_objects,
            &mut report,
        );
    }

    // Collect statistics so the optimizer chooses index scans on the large
    // seed tables instead of sequential scans. Without this, correlated
    // subqueries on tenk1/tenk2 degrade to O(n²) and cause timeouts.
    for table in ["onek", "onek2", "tenk1", "tenk2"] {
        if !setup_scope_includes(required_objects, &[table]) {
            continue;
        }
        if !shadowed_objects.contains(table) {
            let sql = format!("ANALYZE {table}");
            let _ = run_setup_sql(engine, session, &mut report, &sql);
        }
    }

    if bulk_setup_tx_started && engine.execute_sql(session, "COMMIT").is_err() {
        let _ = engine.execute_sql(session, "ROLLBACK");
    }

    report
}

fn setup_scope_includes(required_objects: Option<&BTreeSet<String>>, names: &[&str]) -> bool {
    required_objects.is_none_or(|required| names.iter().any(|name| required.contains(*name)))
}

fn setup_object_required(required_objects: Option<&BTreeSet<String>>, name: &str) -> bool {
    required_objects.is_none_or(|required| required.contains(name))
}

#[derive(Clone, Debug, Default)]
struct SetupStmtMeta {
    table_name: Option<String>,
    index_name: Option<String>,
    index_table_name: Option<String>,
}

fn setup_stmt_metas() -> &'static [SetupStmtMeta] {
    static META: OnceLock<Vec<SetupStmtMeta>> = OnceLock::new();
    META.get_or_init(|| {
        SETUP_STMTS
            .iter()
            .map(|stmt| parse_setup_stmt_meta(stmt))
            .collect()
    })
}

fn parse_setup_stmt_meta(stmt: &str) -> SetupStmtMeta {
    let mut meta = SetupStmtMeta::default();
    let parsed = match aiondb_parser::parse_sql(stmt) {
        Ok(parsed) => parsed,
        Err(_) => return meta,
    };
    let Some(statement) = parsed.first() else {
        return meta;
    };
    match statement {
        Statement::CreateTable(create) => {
            meta.table_name = create
                .name
                .parts
                .last()
                .map(|name| name.to_ascii_lowercase());
        }
        Statement::CreateTableAs(ctas) => {
            meta.table_name = ctas.name.parts.last().map(|name| name.to_ascii_lowercase());
        }
        Statement::Insert(insert) => {
            meta.table_name = insert
                .table
                .parts
                .last()
                .map(|name| name.to_ascii_lowercase());
        }
        Statement::CreateIndex(create) => {
            meta.index_name = create
                .name
                .parts
                .last()
                .map(|name| name.to_ascii_lowercase());
            meta.index_table_name = create
                .table
                .parts
                .last()
                .map(|name| name.to_ascii_lowercase());
        }
        _ => {}
    }
    meta
}

fn should_skip_setup_stmt(
    meta: &SetupStmtMeta,
    shadowed_objects: &BTreeSet<String>,
    required_objects: Option<&BTreeSet<String>>,
) -> bool {
    if let Some(table) = meta.table_name.as_ref() {
        if shadowed_objects.contains(table) {
            return true;
        }
        if let Some(required) = required_objects {
            if !required.contains(table) {
                return true;
            }
        }
        return false;
    }
    if let Some(index) = meta.index_name.as_ref() {
        if shadowed_objects.contains(index) {
            return true;
        }
        if let Some(required) = required_objects {
            let table_is_required = meta
                .index_table_name
                .as_ref()
                .is_some_and(|table| required.contains(table));
            if !table_is_required && !required.contains(index) {
                return true;
            }
        }
    }
    false
}

#[allow(dead_code)]
fn is_identifier_char(ch: u8) -> bool {
    ch.is_ascii_alphanumeric() || ch == b'_'
}

#[allow(dead_code)]
fn sql_mentions_identifier(sql_lower: &str, ident_lower: &str) -> bool {
    if ident_lower.is_empty() {
        return false;
    }
    let hay = sql_lower.as_bytes();
    let needle = ident_lower.as_bytes();
    let mut start = 0usize;
    while let Some(pos) = sql_lower[start..].find(ident_lower) {
        let abs = start + pos;
        let before_ok = abs == 0 || !is_identifier_char(hay[abs - 1]);
        let end = abs + needle.len();
        let after_ok = end >= hay.len() || !is_identifier_char(hay[end]);
        if before_ok && after_ok {
            return true;
        }
        start = abs + 1;
    }
    false
}

#[allow(dead_code)]
pub(crate) fn infer_required_setup_objects(sql_content: &str) -> BTreeSet<String> {
    let sql_lower = sql_content.to_ascii_lowercase();
    let mut candidates: BTreeSet<String> = BTreeSet::new();
    for meta in setup_stmt_metas() {
        if let Some(table) = meta.table_name.as_ref() {
            candidates.insert(table.clone());
        }
        if let Some(table) = meta.index_table_name.as_ref() {
            candidates.insert(table.clone());
        }
    }
    // Populate helpers depend on these hierarchy table names even when not
    // explicitly present in SETUP_STMTS as direct DML targets.
    for name in [
        "person", "emp", "student", "stud_emp", "a_star", "b_star", "c_star", "d_star", "e_star",
        "f_star",
    ] {
        candidates.insert(name.to_owned());
    }

    let mut required = BTreeSet::new();
    for candidate in candidates {
        if sql_mentions_identifier(&sql_lower, &candidate) {
            required.insert(candidate);
        }
    }
    required
}

const SETUP_STMTS: &[&str] = &[
    // ── Core scalar-type tables (referenced by dozens of test files) ──
    "CREATE TABLE CHAR_TBL(f1 char(4))",
    "INSERT INTO char_tbl (f1) VALUES ('a'), ('ab'), ('abcd'), ('abcd')",
    "CREATE TABLE FLOAT8_TBL(f1 float8)",
    "INSERT INTO float8_tbl(f1) VALUES (0.0), (-34.84), (-1004.30), (-1.2345678901234e+200), (-1.2345678901234e-200)",
    "CREATE TABLE INT2_TBL(f1 int2)",
    "INSERT INTO int2_tbl(f1) VALUES (0), (1234), (-1234), (32767), (-32767)",
    "CREATE TABLE INT4_TBL(f1 int4)",
    "INSERT INTO int4_tbl(f1) VALUES (0), (123456), (-123456), (2147483647), (-2147483647)",
    "CREATE TABLE INT8_TBL(q1 int8, q2 int8)",
    "INSERT INTO int8_tbl VALUES (123, 456), (123, 4567890123456789), (4567890123456789, 123), (4567890123456789, 4567890123456789), (4567890123456789, -4567890123456789)",
    "CREATE TABLE TEXT_TBL (f1 text)",
    "INSERT INTO text_tbl VALUES ('doh!'), ('hi de ho neighbor')",
    "CREATE TABLE VARCHAR_TBL(f1 varchar(4))",
    "INSERT INTO varchar_tbl (f1) VALUES ('a'), ('ab'), ('abcd'), ('abcd')",
    // NOTE: FLOAT4_TBL is intentionally NOT here.  PG's test_setup.sql does
    // not create it either -- float4.sql creates its own FLOAT4_TBL and the
    // expected output expects that CREATE TABLE to succeed.

    "CREATE TABLE BOOLEAN_TBL(b boolean)",
    "INSERT INTO boolean_tbl VALUES (true), (false)",

    // ── onek / tenk tables (referenced ~2000+ times across all test files) ──
    // Schema only — data is loaded from canonical PG regression files when
    // available, with a deterministic fallback generator for local dev.
    "CREATE TABLE onek (unique1 int4, unique2 int4, two int4, four int4, ten int4, twenty int4, hundred int4, thousand int4, twothousand int4, fivethous int4, tenthous int4, odd int4, even int4, stringu1 name, stringu2 name, string4 name)",
    "CREATE TABLE onek2 (unique1 int4, unique2 int4, two int4, four int4, ten int4, twenty int4, hundred int4, thousand int4, twothousand int4, fivethous int4, tenthous int4, odd int4, even int4, stringu1 name, stringu2 name, string4 name)",
    "CREATE TABLE tenk1 (unique1 int4, unique2 int4, two int4, four int4, ten int4, twenty int4, hundred int4, thousand int4, twothousand int4, fivethous int4, tenthous int4, odd int4, even int4, stringu1 name, stringu2 name, string4 name)",
    "CREATE TABLE tenk2 (unique1 int4, unique2 int4, two int4, four int4, ten int4, twenty int4, hundred int4, thousand int4, twothousand int4, fivethous int4, tenthous int4, odd int4, even int4, stringu1 name, stringu2 name, string4 name)",

    // ── indexes expected by test_setup.sql (onek / tenk) ──
    // Without these, correlated subqueries on tenk1/tenk2 degrade to O(n²)
    // full scans, causing timeouts / OOM on 10 000-row tables.
    "CREATE INDEX onek_unique1 ON onek USING btree(unique1)",
    "CREATE INDEX onek_unique2 ON onek USING btree(unique2)",
    "CREATE INDEX onek_hundred ON onek USING btree(hundred)",
    "CREATE INDEX onek_stringu1 ON onek USING btree(stringu1)",
    "CREATE INDEX tenk1_unique1 ON tenk1 USING btree(unique1)",
    "CREATE INDEX tenk1_unique2 ON tenk1 USING btree(unique2)",
    "CREATE INDEX tenk1_hundred ON tenk1 USING btree(hundred)",
    "CREATE INDEX tenk1_thousand ON tenk1 (thousand)",
    "CREATE INDEX tenk1_thous_tenthous ON tenk1 (thousand, tenthous)",
    "CREATE INDEX tenk2_unique1 ON tenk2 USING btree(unique1)",
    "CREATE INDEX tenk2_unique2 ON tenk2 USING btree(unique2)",
    "CREATE INDEX tenk2_hundred ON tenk2 USING btree(hundred)",
    // Partial indexes on onek2 used by select.sql and create_index.sql.
    // These mirror the CREATE INDEX statements from the create_index
    // regression test so that queries exercising the partial-index
    // planner path find the expected objects regardless of whether
    // create_index.sql has been replayed first.
    "CREATE INDEX onek2_u1_prtl ON onek2 (unique1) WHERE unique1 < 20 or unique1 > 980",
    "CREATE INDEX onek2_u2_prtl ON onek2 (unique2) WHERE stringu1 < 'B'",
    "CREATE INDEX onek2_stu1_prtl ON onek2 (stringu1) WHERE onek2.stringu1 >= 'J' and onek2.stringu1 < 'K'",

    // ── person / emp / student / stud_emp hierarchy ──
    // PG uses INHERITS; we flatten all columns since AionDB has no inheritance.
    // PG "location point" -> text, PG "manager name" -> text.
    "CREATE TABLE person (name text, age int4, location text)",
    "CREATE TABLE emp (name text, age int4, location text, salary int4, manager text)",
    "CREATE TABLE student (name text, age int4, location text, gpa float8)",
    "CREATE TABLE stud_emp (name text, age int4, location text, salary int4, manager text, gpa float8, percent int4)",

    // ── road / ihighway / shighway ──
    // PG "thepath path" -> text.
    "CREATE TABLE road (name text, thepath text)",
    "CREATE TABLE ihighway (name text, thepath text)",
    "CREATE TABLE shighway (name text, thepath text, surface text)",

    // ── aggtest (25 references in aggregates.sql -- columns a, b) ──
    "CREATE TABLE aggtest (a int2, b float4)",
    "INSERT INTO aggtest VALUES (56, 7.8), (100, 99.097), (0, 0.09561), (42, 324.78)",

    // ── a_star / b_star / c_star / d_star / e_star / f_star hierarchy ──
    // ~125 references across 5 test files.
    // PG uses INHERITS; we flatten.  "cc name" -> text, "ff polygon" -> text.
    // Column names use the post-rename convention from create_misc.sql:
    //   a->aa, b->bb, c->cc, d->dd, e->ee, f->ff
    // After renaming, create_misc.sql also adds new columns:
    //   f_star gets f int4, e_star gets e int4, a_star gets a text
    "CREATE TABLE a_star (class char, aa int4, a text)",
    "CREATE TABLE b_star (class char, aa int4, bb text, a text)",
    "CREATE TABLE c_star (class char, aa int4, cc text, a text)",
    "CREATE TABLE d_star (class char, aa int4, bb text, cc text, dd float8, a text)",
    "CREATE TABLE e_star (class char, aa int4, cc text, ee int2, e int4, a text)",
    "CREATE TABLE f_star (class char, aa int4, cc text, ee int2, ff text, f int4, e int4, a text)",
    // Seed with canonical test data from create_misc.sql
    "INSERT INTO a_star (class, aa) VALUES ('a', 1), ('a', 2)",
    "INSERT INTO a_star (class) VALUES ('a')",
    "INSERT INTO b_star (class, aa, bb) VALUES ('b', 3, 'mumble')",
    "INSERT INTO b_star (class, aa) VALUES ('b', 4)",
    "INSERT INTO b_star (class, bb) VALUES ('b', 'bumble')",
    "INSERT INTO b_star (class) VALUES ('b')",
    "INSERT INTO c_star (class, aa, cc) VALUES ('c', 5, 'hi mom')",
    "INSERT INTO c_star (class, aa) VALUES ('c', 6)",
    "INSERT INTO c_star (class, cc) VALUES ('c', 'hi paul')",
    "INSERT INTO c_star (class) VALUES ('c')",
    "INSERT INTO d_star (class, aa, bb, cc, dd) VALUES ('d', 7, 'grumble', 'hi sunita', 0.0)",
    "INSERT INTO d_star (class, aa, bb, cc) VALUES ('d', 8, 'stumble', 'hi koko')",
    "INSERT INTO d_star (class, aa, bb, dd) VALUES ('d', 9, 'rumble', 1.1)",
    "INSERT INTO d_star (class, aa, cc, dd) VALUES ('d', 10, 'hi kristin', 10.01)",
    "INSERT INTO d_star (class, bb, cc, dd) VALUES ('d', 'crumble', 'hi boris', 100.001)",
    "INSERT INTO d_star (class, aa, bb) VALUES ('d', 11, 'fumble')",
    "INSERT INTO d_star (class, aa, cc) VALUES ('d', 12, 'hi avi')",
    "INSERT INTO d_star (class, aa, dd) VALUES ('d', 13, 1000.0001)",
    "INSERT INTO d_star (class, bb, cc) VALUES ('d', 'tumble', 'hi andrew')",
    "INSERT INTO d_star (class, bb, dd) VALUES ('d', 'humble', 10000.00001)",
    "INSERT INTO d_star (class, cc, dd) VALUES ('d', 'hi ginger', 100000.000001)",
    "INSERT INTO d_star (class, aa) VALUES ('d', 14)",
    "INSERT INTO d_star (class, bb) VALUES ('d', 'jumble')",
    "INSERT INTO d_star (class, cc) VALUES ('d', 'hi jolly')",
    "INSERT INTO d_star (class, dd) VALUES ('d', 1000000.0000001)",
    "INSERT INTO d_star (class) VALUES ('d')",
    "INSERT INTO e_star (class, aa, cc, ee) VALUES ('e', 15, 'hi carol', -1)",
    "INSERT INTO e_star (class, aa, cc) VALUES ('e', 16, 'hi bob')",
    "INSERT INTO e_star (class, aa, ee) VALUES ('e', 17, -2)",
    "INSERT INTO e_star (class, cc, ee) VALUES ('e', 'hi michelle', -3)",
    "INSERT INTO e_star (class, aa) VALUES ('e', 18)",
    "INSERT INTO e_star (class, cc) VALUES ('e', 'hi elisa')",
    "INSERT INTO e_star (class, ee) VALUES ('e', -4)",
    "INSERT INTO f_star (class, aa, cc, ee, ff) VALUES ('f', 19, 'hi claire', -5, '(1,3),(2,4)')",
    "INSERT INTO f_star (class, aa, cc, ee) VALUES ('f', 20, 'hi mike', -6)",
    "INSERT INTO f_star (class, aa, cc, ff) VALUES ('f', 21, 'hi marcel', '(11,44),(22,55),(33,66)')",
    "INSERT INTO f_star (class, aa, ee, ff) VALUES ('f', 22, -7, '(111,555),(222,666),(333,777),(444,888)')",
    "INSERT INTO f_star (class, cc, ee, ff) VALUES ('f', 'hi keith', -8, '(1111,3333),(2222,4444)')",
    "INSERT INTO f_star (class, aa, cc) VALUES ('f', 24, 'hi marc')",
    "INSERT INTO f_star (class, aa, ee) VALUES ('f', 25, -9)",
    "INSERT INTO f_star (class, aa, ff) VALUES ('f', 26, '(11111,33333),(22222,44444)')",
    "INSERT INTO f_star (class, cc, ee) VALUES ('f', 'hi allison', -10)",
    "INSERT INTO f_star (class, cc, ff) VALUES ('f', 'hi jeff', '(111111,333333),(222222,444444)')",
    "INSERT INTO f_star (class, ee, ff) VALUES ('f', -11, '(1111111,3333333),(2222222,4444444)')",
    "INSERT INTO f_star (class, aa) VALUES ('f', 27)",
    "INSERT INTO f_star (class, cc) VALUES ('f', 'hi carl')",
    "INSERT INTO f_star (class, ee) VALUES ('f', -12)",
    "INSERT INTO f_star (class, ff) VALUES ('f', '(11111111,33333333),(22222222,44444444)')",
    "INSERT INTO f_star (class) VALUES ('f')",

    // ── bt_*_heap tables (28 references in btree_index.sql) ──
    "CREATE TABLE bt_i4_heap (seqno int4, random int4)",
    "CREATE TABLE bt_name_heap (seqno text, random int4)",
    "CREATE TABLE bt_txt_heap (seqno text, random int4)",
    "CREATE TABLE bt_f8_heap (seqno float8, random int4)",

    // ── bool_test (8 references in aggregates.sql -- columns b1..b4) ──
    "CREATE TABLE bool_test (b1 boolean, b2 boolean, b3 boolean, b4 boolean)",
    "INSERT INTO bool_test VALUES (true, null, false, null)",
    "INSERT INTO bool_test VALUES (false, true, null, null)",
    "INSERT INTO bool_test VALUES (null, true, false, null)",

    // ── Timestamp / date / time / interval tables ──
    "CREATE TABLE TIMESTAMP_TBL (d1 timestamp(2) without time zone)",
    "CREATE TABLE TIMESTAMPTZ_TBL (d1 timestamp(2) with time zone)",
    "CREATE TABLE ABSTIME_TBL (f1 timestamp)",
    "CREATE TABLE RELTIME_TBL (f1 interval)",
    "CREATE TABLE DATE_TBL (f1 date)",
    "CREATE TABLE TIME_TBL (f1 time(2))",
    "CREATE TABLE TIMETZ_TBL (f1 time(2) with time zone)",
    "CREATE TABLE INTERVAL_TBL (f1 interval)",

    // ── Constraint test tables ──
    "CREATE TABLE FKTABLE (ftest1 int, ftest2 int, ftest3 int)",

    // ── Geometry tables ──
    // These are created by their own test files (lseg.sql, polygon.sql, etc.)
    // but geometry.sql runs earlier (alphabetically) and references them.
    // Keep POINT_TBL typed as point so INSERT-time input validation matches PG.
    "CREATE TABLE POINT_TBL(f1 point)",
    "INSERT INTO POINT_TBL(f1) VALUES
      ('(0.0,0.0)'),
      ('(-10.0,0.0)'),
      ('(-3.0,4.0)'),
      ('(5.1, 34.5)'),
      ('(-5.0,-12.0)'),
      ('(1e-300,-1e-300)'),
      ('(1e+300,Inf)'),
      ('(Inf,1e+300)'),
      (' ( Nan , NaN ) '),
      ('10.0,10.0')",

    // LSEG_TBL: keep native lseg type for create_index/geometry compat.
    "CREATE TABLE LSEG_TBL (s lseg)",
    "INSERT INTO LSEG_TBL VALUES ('[(1,2),(3,4)]')",
    "INSERT INTO LSEG_TBL VALUES ('(0,0),(6,6)')",
    "INSERT INTO LSEG_TBL VALUES ('10,-10 ,-3,-4')",
    "INSERT INTO LSEG_TBL VALUES ('[-1e6,2e2,3e5, -4e1]')",
    "INSERT INTO LSEG_TBL VALUES ('[(-10,2),(-10,3)]')",
    "INSERT INTO LSEG_TBL VALUES ('[(0,-20),(30,-20)]')",
    "INSERT INTO LSEG_TBL VALUES ('[(NaN,1),(NaN,90)]')",

    // LINE_TBL keeps PG type semantics via compat-cast validation.
    "CREATE TABLE LINE_TBL (s line)",
    "INSERT INTO LINE_TBL VALUES ('{0,-1,5}')",
    "INSERT INTO LINE_TBL VALUES ('{1,0,5}')",
    "INSERT INTO LINE_TBL VALUES ('{0,3,0}')",
    "INSERT INTO LINE_TBL VALUES (' (0,0), (6,6)')",
    "INSERT INTO LINE_TBL VALUES ('10,-10 ,-5,-4')",
    "INSERT INTO LINE_TBL VALUES ('[-1e6,2e2,3e5, -4e1]')",
    "INSERT INTO LINE_TBL VALUES ('{3,NaN,5}')",
    "INSERT INTO LINE_TBL VALUES ('{NaN,NaN,NaN}')",
    "INSERT INTO LINE_TBL VALUES ('[(1,3),(2,3)]')",

    // PATH_TBL keeps PG type semantics via compat-cast validation.
    "CREATE TABLE PATH_TBL (f1 path)",
    "INSERT INTO PATH_TBL VALUES ('[(1,2),(3,4)]')",
    "INSERT INTO PATH_TBL VALUES (' ( ( 1 , 2 ) , ( 3 , 4 ) ) ')",
    "INSERT INTO PATH_TBL VALUES ('[ (0,0),(3,0),(4,5),(1,6) ]')",
    "INSERT INTO PATH_TBL VALUES ('((1,2) ,(3,4 ))')",
    "INSERT INTO PATH_TBL VALUES ('1,2 ,3,4 ')",
    "INSERT INTO PATH_TBL VALUES (' [1,2,3, 4] ')",
    "INSERT INTO PATH_TBL VALUES ('((10,20))')",
    "INSERT INTO PATH_TBL VALUES ('[ 11,12,13,14 ]')",
    "INSERT INTO PATH_TBL VALUES ('( 11,12,13,14) ')",

    // POLYGON_TBL keeps PG type semantics via compat-cast validation.
    "CREATE TABLE POLYGON_TBL (f1 polygon)",
    "INSERT INTO POLYGON_TBL VALUES ('(2.0,0.0),(2.0,4.0),(0.0,0.0)')",
    "INSERT INTO POLYGON_TBL VALUES ('(3.0,1.0),(3.0,3.0),(1.0,0.0)')",
    "INSERT INTO POLYGON_TBL VALUES ('(1,2),(3,4),(5,6),(7,8)')",
    "INSERT INTO POLYGON_TBL VALUES ('(7,8),(5,6),(3,4),(1,2)')",
    "INSERT INTO POLYGON_TBL VALUES ('(1,2),(7,8),(5,6),(3,-4)')",
    "INSERT INTO POLYGON_TBL VALUES ('(0.0,0.0)')",
    "INSERT INTO POLYGON_TBL VALUES ('(0.0,1.0),(0.0,1.0)')",

    // BOX_TBL keeps native type for index/operator behavior parity.
    // Referenced by geometry.sql, create_index.sql, etc.
    "CREATE TABLE BOX_TBL (f1 box)",
    "INSERT INTO BOX_TBL (f1) VALUES ('(2.0,2.0,0.0,0.0)')",
    "INSERT INTO BOX_TBL (f1) VALUES ('(1.0,1.0,3.0,3.0)')",
    "INSERT INTO BOX_TBL (f1) VALUES ('((-8, 2), (-2, -10))')",
    "INSERT INTO BOX_TBL (f1) VALUES ('(2.5, 2.5, 2.5,3.5)')",
    "INSERT INTO BOX_TBL (f1) VALUES ('(3.0, 3.0,3.0,3.0)')",

    // CIRCLE_TBL keeps PG type semantics via compat-cast validation.
    // Referenced by geometry.sql (~35 times)
    "CREATE TABLE CIRCLE_TBL (f1 circle)",
    "INSERT INTO CIRCLE_TBL VALUES ('<(5,1),3>')",
    "INSERT INTO CIRCLE_TBL VALUES ('((1,2),100)')",
    "INSERT INTO CIRCLE_TBL VALUES (' 1 , 3 , 5 ')",
    "INSERT INTO CIRCLE_TBL VALUES (' ( ( 1 , 2 ) , 3 ) ')",
    "INSERT INTO CIRCLE_TBL VALUES (' ( 100 , 200 ) , 10 ')",
    "INSERT INTO CIRCLE_TBL VALUES (' < ( 100 , 1 ) , 115 > ')",
    "INSERT INTO CIRCLE_TBL VALUES ('<(3,5),0>')",
    "INSERT INTO CIRCLE_TBL VALUES ('<(3,5),NaN>')",

    // ── GiST/SP-GiST test tables ──
    "CREATE TABLE quad_point_tbl (id int, p text)",
    "CREATE TABLE quad_poly_tbl (id int, p polygon)",

    // ── fast_emp4000 (referenced by create_am.sql, created by create_index.sql) ──
    "CREATE TABLE fast_emp4000 (home_base box)",

    // ── Numeric test tables ──
    "CREATE TABLE num_data (id int, val numeric)",
    "CREATE TABLE num_exp_add (id1 int, id2 int, expected numeric)",
    "CREATE TABLE num_exp_sub (id1 int, id2 int, expected numeric)",
    "CREATE TABLE num_exp_mul (id1 int, id2 int, expected numeric)",
    "CREATE TABLE num_exp_div (id1 int, id2 int, expected numeric)",
    "CREATE TABLE num_exp_sqrt (id int, expected numeric)",
    "CREATE TABLE num_exp_ln (id int, expected numeric)",
    "CREATE TABLE num_exp_log10 (id int, expected numeric)",
    "CREATE TABLE num_exp_power_10_ln (id int, expected numeric)",
    "CREATE TABLE num_result (id1 int, id2 int, result numeric)",

    // ── Cluster test tables ──
    "CREATE TABLE clstr_tst (a int, b text, c text)",
    "INSERT INTO clstr_tst VALUES (1, 'a', 'x'), (2, 'b', 'y'), (3, 'c', 'z')",
    "CREATE TABLE clstr_tst_s (a int, b text)",

    // ── Miscellaneous tables referenced by multiple test files ──
    // hash_index test tables
    "CREATE TABLE hash_i4_heap (seqno int4, random int4)",
    "CREATE TABLE hash_name_heap (seqno text, random int4)",
    "CREATE TABLE hash_txt_heap (seqno text, random int4)",
    "CREATE TABLE hash_f8_heap (seqno float8, random int4)",
    // NOTE: fake pg_class/pg_authid tables removed -- AionDB provides
    // built-in pg_catalog.pg_class, pg_attribute, pg_type, etc.
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_stmt_table_name_handles_create_and_insert() {
        assert_eq!(
            parse_setup_stmt_meta("CREATE TABLE aggtest (a int)").table_name,
            Some("aggtest".to_owned())
        );
        assert_eq!(
            parse_setup_stmt_meta("INSERT INTO aggtest VALUES (1)").table_name,
            Some("aggtest".to_owned())
        );
    }

    #[test]
    fn shadowed_tables_skip_related_setup_statements() {
        let shadowed = BTreeSet::from([String::from("aggtest")]);
        let required = None;

        assert!(should_skip_setup_stmt(
            &parse_setup_stmt_meta("CREATE TABLE aggtest (a int)"),
            &shadowed,
            required
        ));
        assert!(should_skip_setup_stmt(
            &parse_setup_stmt_meta("INSERT INTO aggtest VALUES (1)"),
            &shadowed,
            required
        ));
        assert!(!should_skip_setup_stmt(
            &parse_setup_stmt_meta("CREATE TABLE other_tbl (a int)"),
            &shadowed,
            required
        ));
    }

    #[test]
    fn regression_data_candidates_live_under_pg_regress_data_dir() {
        assert!(regression_data_file_candidate("onek.data")
            .ends_with(Path::new(".pg-regress").join("data").join("onek.data")));
        assert!(regression_data_file_candidate("tenk.data")
            .ends_with(Path::new(".pg-regress").join("data").join("tenk.data")));
    }

    #[test]
    fn sql_quote_literal_escapes_single_quotes() {
        assert_eq!(
            sql_quote_literal("/tmp/pg seed's data.txt"),
            "'/tmp/pg seed''s data.txt'"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════
//  Programmatic data generation for onek / onek2 / tenk1 / tenk2
// ═══════════════════════════════════════════════════════════════════════

/// Number of rows per batch INSERT (keeps individual SQL statements manageable).
const BATCH_SIZE: usize = 200;

/// Generate the PG base-26 "stringu" encoding for a given value.
///
/// PG encodes values as 6-character uppercase strings where each character
/// position represents a base-26 digit: 'A'=0, 'B'=1, ... 'Z'=25.
/// The least-significant digit is at position 0 (leftmost).
///
/// Examples: 0 → "AAAAAA", 1 → "BAAAAA", 26 → "ABAAAA"
fn base26_name(mut val: usize) -> String {
    let mut chars = [b'A'; 6];
    for ch in &mut chars {
        *ch = b'A' + (val % 26) as u8;
        val /= 26;
    }
    // SAFETY: all bytes are valid ASCII uppercase letters.
    String::from_utf8(chars.to_vec()).unwrap()
}

/// Returns one of four cycling string4 values based on row index.
///
/// PG's convention: string4 cycles through "OOOOxx", "VVVVxx", "AAAAxx", "HHHHxx".
fn string4_value(idx: usize) -> &'static str {
    const VALUES: [&str; 4] = ["OOOOxx", "VVVVxx", "AAAAxx", "HHHHxx"];
    VALUES[idx % 4]
}

/// Build one VALUES tuple for a single row in the onek/tenk schema.
///
/// Columns: unique1, unique2, two, four, ten, twenty, hundred, thousand,
///          twothousand, fivethous, tenthous, odd, even,
///          stringu1, stringu2, string4
fn format_row(buf: &mut String, unique1: i32, unique2: i32) {
    let two = unique1 % 2;
    let four = unique1 % 4;
    let ten = unique1 % 10;
    let twenty = unique1 % 20;
    let hundred = unique1 % 100;
    let thousand = unique1 % 1000;
    let twothousand = unique1 % 2000;
    let fivethous = unique1 % 5000;
    let tenthous = unique1;
    // PG convention: "odd" is actually 2*(unique1%50), which produces even numbers,
    // and "even" is 2*(unique1%50)+1, which produces odd numbers.
    let odd = 2 * (unique1 % 50);
    let even = 2 * (unique1 % 50) + 1;
    let stringu1 = base26_name(unique1 as usize);
    let stringu2 = base26_name(unique2 as usize);
    let string4 = string4_value(unique1 as usize);

    let _ = write!(
        buf,
        "({},{},{},{},{},{},{},{},{},{},{},{},{},'{}','{}','{}')",
        unique1,
        unique2,
        two,
        four,
        ten,
        twenty,
        hundred,
        thousand,
        twothousand,
        fivethous,
        tenthous,
        odd,
        even,
        stringu1,
        stringu2,
        string4,
    );
}

/// Compute a deterministic permutation for unique2 values.
///
/// Uses `(i * 9973 + 3) % n` which is a full permutation when
/// gcd(9973, n) == 1.  Since 9973 is prime and n is 1000 or 10000,
/// this always produces a permutation (no duplicates).
fn permuted_unique2(unique1: usize, n: usize) -> i32 {
    ((unique1 * 9973 + 3) % n) as i32
}

/// Insert all rows for a given table using batched multi-row INSERT statements.
fn insert_rows(
    engine: &Engine,
    session: &SessionHandle,
    table: &str,
    n: usize,
    report: &mut SetupReport,
) {
    let mut start = 0;
    while start < n {
        let end = (start + BATCH_SIZE).min(n);
        let mut sql = format!(
            "INSERT INTO {} (unique1,unique2,two,four,ten,twenty,hundred,\
             thousand,twothousand,fivethous,tenthous,odd,even,\
             stringu1,stringu2,string4) VALUES ",
            table
        );
        for i in start..end {
            if i > start {
                sql.push(',');
            }
            let unique2 = permuted_unique2(i, n);
            format_row(&mut sql, i as i32, unique2);
        }
        let _ = run_setup_sql(engine, session, report, &sql);
        start = end;
    }
}

fn regression_data_file_candidate(file_name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("data")
        .join(file_name)
}

fn regression_data_file_path(file_name: &str) -> Option<PathBuf> {
    let path = regression_data_file_candidate(file_name);
    path.is_file().then_some(path)
}

fn sql_quote_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn run_setup_sql(
    engine: &Engine,
    session: &SessionHandle,
    report: &mut SetupReport,
    sql: &str,
) -> bool {
    const SETUP_SAVEPOINT: &str = "aiondb_setup_step";
    report.attempted += 1;
    let has_tx = engine.has_active_transaction(session).unwrap_or(false);
    let savepoint_active = has_tx
        && engine
            .execute_sql(session, &format!("SAVEPOINT {SETUP_SAVEPOINT}"))
            .is_ok();
    if engine.execute_sql(session, sql).is_ok() {
        report.succeeded += 1;
        if savepoint_active {
            let _ = engine.execute_sql(session, &format!("RELEASE SAVEPOINT {SETUP_SAVEPOINT}"));
        }
        true
    } else {
        report.failed += 1;
        if savepoint_active {
            let _ =
                engine.execute_sql(session, &format!("ROLLBACK TO SAVEPOINT {SETUP_SAVEPOINT}"));
            let _ = engine.execute_sql(session, &format!("RELEASE SAVEPOINT {SETUP_SAVEPOINT}"));
        }
        false
    }
}

fn copy_seed_file_into_table(
    engine: &Engine,
    session: &SessionHandle,
    table: &str,
    path: &Path,
    report: &mut SetupReport,
) -> bool {
    let sql = format!(
        "COPY {table} FROM {}",
        sql_quote_literal(&path.to_string_lossy())
    );
    if run_setup_sql(engine, session, report, &sql) {
        return true;
    }
    insert_seed_file_into_table(engine, session, table, path, report)
}

fn insert_seed_file_into_table(
    engine: &Engine,
    session: &SessionHandle,
    table: &str,
    path: &Path,
    report: &mut SetupReport,
) -> bool {
    const INSERT_BATCH_SIZE: usize = 200;

    let Ok(content) = std::fs::read_to_string(path) else {
        report.failed += 1;
        return false;
    };
    let mut all_ok = true;
    let mut batch = Vec::with_capacity(INSERT_BATCH_SIZE);

    for line in content.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            continue;
        }
        batch.push(trimmed.to_owned());
        if batch.len() >= INSERT_BATCH_SIZE {
            if !insert_seed_batch(engine, session, table, &batch, report) {
                all_ok = false;
            }
            batch.clear();
        }
    }

    if !batch.is_empty() && !insert_seed_batch(engine, session, table, &batch, report) {
        all_ok = false;
    }

    all_ok
}

fn insert_seed_batch(
    engine: &Engine,
    session: &SessionHandle,
    table: &str,
    rows: &[String],
    report: &mut SetupReport,
) -> bool {
    let mut sql = format!("INSERT INTO {table} VALUES ");
    for (index, row) in rows.iter().enumerate() {
        if index > 0 {
            sql.push(',');
        }
        sql.push('(');
        for (field_index, field) in row.split('\t').enumerate() {
            if field_index > 0 {
                sql.push(',');
            }
            if field == "\\N" {
                sql.push_str("NULL");
            } else {
                sql.push_str(&sql_quote_literal(field));
            }
        }
        sql.push(')');
    }
    run_setup_sql(engine, session, report, &sql)
}

fn populate_onek_tenk_from_regression_data(
    engine: &Engine,
    session: &SessionHandle,
    shadowed_tables: &BTreeSet<String>,
    required_objects: Option<&BTreeSet<String>>,
    report: &mut SetupReport,
) -> bool {
    let Some(onek_path) = regression_data_file_path("onek.data") else {
        return false;
    };
    let Some(tenk_path) = regression_data_file_path("tenk.data") else {
        return false;
    };

    let mut attempted_copies = 0usize;
    let mut all_copies_ok = true;

    for table in ["onek", "onek2"] {
        if shadowed_tables.contains(table) || !setup_object_required(required_objects, table) {
            continue;
        }
        attempted_copies += 1;
        if !copy_seed_file_into_table(engine, session, table, &onek_path, report) {
            all_copies_ok = false;
        }
    }

    for table in ["tenk1", "tenk2"] {
        if shadowed_tables.contains(table) || !setup_object_required(required_objects, table) {
            continue;
        }
        attempted_copies += 1;
        if !copy_seed_file_into_table(engine, session, table, &tenk_path, report) {
            all_copies_ok = false;
        }
    }

    attempted_copies > 0 && all_copies_ok
}

fn truncate_onek_tenk_tables(
    engine: &Engine,
    session: &SessionHandle,
    shadowed_tables: &BTreeSet<String>,
    required_objects: Option<&BTreeSet<String>>,
    report: &mut SetupReport,
) {
    let mut tables: Vec<&str> = Vec::new();
    for table in ["onek", "onek2", "tenk1", "tenk2"] {
        if !shadowed_tables.contains(table) && setup_object_required(required_objects, table) {
            tables.push(table);
        }
    }
    if tables.is_empty() {
        return;
    }
    let sql = format!("TRUNCATE {}", tables.join(", "));
    let _ = run_setup_sql(engine, session, report, &sql);
}

fn populate_generated_table(
    engine: &Engine,
    session: &SessionHandle,
    shadowed_tables: &BTreeSet<String>,
    required_objects: Option<&BTreeSet<String>>,
    table: &str,
    row_count: usize,
    report: &mut SetupReport,
) {
    if shadowed_tables.contains(table) || !setup_object_required(required_objects, table) {
        return;
    }
    insert_rows(engine, session, table, row_count, report);
}

/// Populate onek/onek2/tenk1/tenk2 with canonical PostgreSQL regression seed
/// files when present, otherwise fall back to the deterministic local generator.
fn populate_onek_tenk(
    engine: &Engine,
    session: &SessionHandle,
    shadowed_tables: &BTreeSet<String>,
    required_objects: Option<&BTreeSet<String>>,
    scale: &OnekTenkScale,
    report: &mut SetupReport,
) {
    if scale.is_default()
        && populate_onek_tenk_from_regression_data(
            engine,
            session,
            shadowed_tables,
            required_objects,
            report,
        )
    {
        return;
    }

    // COPY from canonical files is best-effort; when unavailable or rejected
    // by the engine, reset any partial state and fall back to deterministic
    // generated data so regress suites never run on empty tenk/onek tables.
    truncate_onek_tenk_tables(engine, session, shadowed_tables, required_objects, report);
    populate_generated_table(
        engine,
        session,
        shadowed_tables,
        required_objects,
        "onek",
        scale.onek_rows,
        report,
    );
    populate_generated_table(
        engine,
        session,
        shadowed_tables,
        required_objects,
        "onek2",
        scale.onek2_rows,
        report,
    );
    populate_generated_table(
        engine,
        session,
        shadowed_tables,
        required_objects,
        "tenk1",
        scale.tenk1_rows,
        report,
    );
    populate_generated_table(
        engine,
        session,
        shadowed_tables,
        required_objects,
        "tenk2",
        scale.tenk2_rows,
        report,
    );
}

/// Populate person / emp / student / stud_emp tables with canonical PG
/// regression seed data.
///
/// PostgreSQL uses table inheritance (`INHERITS`) so that `person*` queries
/// automatically include rows from child tables (emp, student, stud_emp).
/// AionDB does not support inheritance, so we simulate the effect by:
///   1. Loading each table with its own data file via COPY.
///   2. Copying (name, age, location) from the child tables into `person`
///      so that `FROM person*` (which AionDB treats as just `FROM person`)
///      returns the union of all hierarchy data.
fn populate_person_hierarchy(
    engine: &Engine,
    session: &SessionHandle,
    shadowed_tables: &BTreeSet<String>,
    required_objects: Option<&BTreeSet<String>>,
    report: &mut SetupReport,
) {
    let person_path = regression_data_file_path("person.data");
    let emp_path = regression_data_file_path("emp.data");
    let student_path = regression_data_file_path("student.data");
    let stud_emp_path = regression_data_file_path("stud_emp.data");

    // Load each table with its own data.
    if let Some(ref path) = person_path {
        if !shadowed_tables.contains("person") && setup_object_required(required_objects, "person")
        {
            copy_seed_file_into_table(engine, session, "person", path, report);
        }
    }
    if let Some(ref path) = emp_path {
        if !shadowed_tables.contains("emp") && setup_object_required(required_objects, "emp") {
            copy_seed_file_into_table(engine, session, "emp", path, report);
        }
    }
    if let Some(ref path) = student_path {
        if !shadowed_tables.contains("student")
            && setup_object_required(required_objects, "student")
        {
            copy_seed_file_into_table(engine, session, "student", path, report);
        }
    }
    if let Some(ref path) = stud_emp_path {
        if !shadowed_tables.contains("stud_emp")
            && setup_object_required(required_objects, "stud_emp")
        {
            copy_seed_file_into_table(engine, session, "stud_emp", path, report);
        }
    }

    // Simulate inheritance: copy child rows into the person table so that
    // `FROM person*` (treated as `FROM person`) returns all hierarchy data.
    if !shadowed_tables.contains("person") && setup_object_required(required_objects, "person") {
        for child in &["emp", "student", "stud_emp"] {
            if !shadowed_tables.contains(*child) && setup_object_required(required_objects, child) {
                let sql = format!(
                    "INSERT INTO person (name, age, location) SELECT name, age, location FROM {child}"
                );
                run_setup_sql(engine, session, report, &sql);
            }
        }
    }
}

/// Simulate a_star inheritance hierarchy.
///
/// In PostgreSQL, `a_star` is the root of an inheritance tree:
///   a_star -> b_star, c_star
///   b_star, c_star -> d_star
///   c_star -> e_star -> f_star
///
/// Queries against `a_star` (or `a_star*`) return rows from ALL descendants.
/// We simulate this by copying (class, aa) from every child table into a_star.
fn populate_a_star_hierarchy(
    engine: &Engine,
    session: &SessionHandle,
    shadowed_tables: &BTreeSet<String>,
    required_objects: Option<&BTreeSet<String>>,
    report: &mut SetupReport,
) {
    if shadowed_tables.contains("a_star") || !setup_object_required(required_objects, "a_star") {
        return;
    }
    for child in &["b_star", "c_star", "d_star", "e_star", "f_star"] {
        if !shadowed_tables.contains(*child) && setup_object_required(required_objects, child) {
            let sql = format!("INSERT INTO a_star (class, aa) SELECT class, aa FROM {child}");
            run_setup_sql(engine, session, report, &sql);
        }
    }
}
