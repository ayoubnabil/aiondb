use std::borrow::Cow;
use std::collections::BTreeSet;

use aiondb_engine::{Engine, QueryEngine, SessionHandle};

use crate::setup::SetupReport;

pub(crate) fn populate_temporal_tables(
    engine: &Engine,
    session: &SessionHandle,
    shadowed_tables: &BTreeSet<String>,
    report: &mut SetupReport,
) {
    run_group(
        engine,
        session,
        report,
        shadowed_tables,
        "date_tbl",
        DATE_TBL_SETUP,
    );
    run_group(
        engine,
        session,
        report,
        shadowed_tables,
        "interval_tbl",
        INTERVAL_TBL_SETUP,
    );
    run_group(
        engine,
        session,
        report,
        shadowed_tables,
        "time_tbl",
        TIME_TBL_SETUP,
    );
    run_group(
        engine,
        session,
        report,
        shadowed_tables,
        "timetz_tbl",
        TIMETZ_TBL_SETUP,
    );
    run_group(
        engine,
        session,
        report,
        shadowed_tables,
        "timestamp_tbl",
        TIMESTAMP_TBL_SETUP,
    );
    run_group(
        engine,
        session,
        report,
        shadowed_tables,
        "timestamptz_tbl",
        TIMESTAMPTZ_TBL_SETUP,
    );
}

fn run_group(
    engine: &Engine,
    session: &SessionHandle,
    report: &mut SetupReport,
    shadowed_tables: &BTreeSet<String>,
    table_name: &str,
    statements: &[&str],
) {
    if shadowed_tables.contains(table_name) {
        return;
    }

    for stmt in statements {
        report.attempted += 1;
        let effective = effective_seed_statement(table_name, stmt);
        if engine.execute_sql(session, effective.as_ref()).is_ok() {
            report.succeeded += 1;
        } else {
            report.failed += 1;
        }
    }
}

fn effective_seed_statement<'a>(table_name: &str, stmt: &'a str) -> Cow<'a, str> {
    match table_name {
        "time_tbl" => wrap_temporal_seed_insert(
            stmt,
            "INSERT INTO TIME_TBL VALUES (",
            "time(2) without time zone",
        ),
        "timetz_tbl" => wrap_temporal_seed_insert(
            stmt,
            "INSERT INTO TIMETZ_TBL VALUES (",
            "time(2) with time zone",
        ),
        "timestamp_tbl" => wrap_temporal_seed_insert(
            stmt,
            "INSERT INTO TIMESTAMP_TBL VALUES (",
            "timestamp(2) without time zone",
        ),
        "timestamptz_tbl" => wrap_temporal_seed_insert(
            stmt,
            "INSERT INTO TIMESTAMPTZ_TBL VALUES (",
            "timestamp(2) with time zone",
        ),
        _ => Cow::Borrowed(stmt),
    }
}

fn wrap_temporal_seed_insert<'a>(stmt: &'a str, prefix: &str, type_name: &str) -> Cow<'a, str> {
    let trimmed = stmt.trim();
    let Some(literal) = trimmed
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_suffix(')'))
    else {
        return Cow::Borrowed(stmt);
    };
    Cow::Owned(format!("{prefix}{type_name} {literal})"))
}

// Keep these seed sets aligned with the rows that survive into the steady-state
// tables in PostgreSQL's date/time regression files after their local setup
// transactions and truncates complete.
const DATE_TBL_SETUP: &[&str] = &[
    "INSERT INTO DATE_TBL VALUES ('1957-04-09')",
    "INSERT INTO DATE_TBL VALUES ('1957-06-13')",
    "INSERT INTO DATE_TBL VALUES ('1996-02-28')",
    "INSERT INTO DATE_TBL VALUES ('1996-02-29')",
    "INSERT INTO DATE_TBL VALUES ('1996-03-01')",
    "INSERT INTO DATE_TBL VALUES ('1996-03-02')",
    "INSERT INTO DATE_TBL VALUES ('1997-02-28')",
    "INSERT INTO DATE_TBL VALUES ('1997-03-01')",
    "INSERT INTO DATE_TBL VALUES ('1997-03-02')",
    "INSERT INTO DATE_TBL VALUES ('2000-04-01')",
    "INSERT INTO DATE_TBL VALUES ('2000-04-02')",
    "INSERT INTO DATE_TBL VALUES ('2000-04-03')",
    "INSERT INTO DATE_TBL VALUES ('2038-04-08')",
    "INSERT INTO DATE_TBL VALUES ('2039-04-09')",
    "INSERT INTO DATE_TBL VALUES ('2040-04-10')",
    "INSERT INTO DATE_TBL VALUES ('2040-04-10 BC')",
];

const INTERVAL_TBL_SETUP: &[&str] = &[
    "INSERT INTO INTERVAL_TBL (f1) VALUES ('@ 1 minute')",
    "INSERT INTO INTERVAL_TBL (f1) VALUES ('@ 5 hour')",
    "INSERT INTO INTERVAL_TBL (f1) VALUES ('@ 10 day')",
    "INSERT INTO INTERVAL_TBL (f1) VALUES ('@ 34 year')",
    "INSERT INTO INTERVAL_TBL (f1) VALUES ('@ 3 months')",
    "INSERT INTO INTERVAL_TBL (f1) VALUES ('@ 14 seconds ago')",
    "INSERT INTO INTERVAL_TBL (f1) VALUES ('1 day 2 hours 3 minutes 4 seconds')",
    "INSERT INTO INTERVAL_TBL (f1) VALUES ('6 years')",
    "INSERT INTO INTERVAL_TBL (f1) VALUES ('5 months')",
    "INSERT INTO INTERVAL_TBL (f1) VALUES ('5 months 12 hours')",
];

const TIME_TBL_SETUP: &[&str] = &[
    "INSERT INTO TIME_TBL VALUES ('00:00')",
    "INSERT INTO TIME_TBL VALUES ('01:00')",
    "INSERT INTO TIME_TBL VALUES ('02:03 PST')",
    "INSERT INTO TIME_TBL VALUES ('11:59 EDT')",
    "INSERT INTO TIME_TBL VALUES ('12:00')",
    "INSERT INTO TIME_TBL VALUES ('12:01')",
    "INSERT INTO TIME_TBL VALUES ('23:59')",
    "INSERT INTO TIME_TBL VALUES ('11:59:59.99 PM')",
    "INSERT INTO TIME_TBL VALUES ('2003-03-07 15:36:39 America/New_York')",
    "INSERT INTO TIME_TBL VALUES ('2003-07-07 15:36:39 America/New_York')",
];

const TIMETZ_TBL_SETUP: &[&str] = &[
    "INSERT INTO TIMETZ_TBL VALUES ('00:01 PDT')",
    "INSERT INTO TIMETZ_TBL VALUES ('01:00 PDT')",
    "INSERT INTO TIMETZ_TBL VALUES ('02:03 PDT')",
    "INSERT INTO TIMETZ_TBL VALUES ('07:07 PST')",
    "INSERT INTO TIMETZ_TBL VALUES ('08:08 EDT')",
    "INSERT INTO TIMETZ_TBL VALUES ('11:59 PDT')",
    "INSERT INTO TIMETZ_TBL VALUES ('12:00 PDT')",
    "INSERT INTO TIMETZ_TBL VALUES ('12:01 PDT')",
    "INSERT INTO TIMETZ_TBL VALUES ('23:59 PDT')",
    "INSERT INTO TIMETZ_TBL VALUES ('11:59:59.99 PM PDT')",
    "INSERT INTO TIMETZ_TBL VALUES ('2003-03-07 15:36:39 America/New_York')",
    "INSERT INTO TIMETZ_TBL VALUES ('2003-07-07 15:36:39 America/New_York')",
];

const TIMESTAMP_TBL_SETUP: &[&str] = &[
    "INSERT INTO TIMESTAMP_TBL VALUES ('-infinity')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('infinity')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('epoch')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('Mon Feb 10 17:32:01 1997 PST')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('Mon Feb 10 17:32:01.000001 1997 PST')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('Mon Feb 10 17:32:01.999999 1997 PST')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('Mon Feb 10 17:32:01.4 1997 PST')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('Mon Feb 10 17:32:01.5 1997 PST')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('Mon Feb 10 17:32:01.6 1997 PST')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('1997-01-02')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('1997-01-02 03:04:05')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('1997-02-10 17:32:01-08')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('1997-02-10 17:32:01-0800')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('1997-02-10 17:32:01 -08:00')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('19970210 173201 -0800')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('1997-06-10 17:32:01 -07:00')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('2001-09-22T18:19:20')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('2000-03-15 08:14:01 GMT+8')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('2000-03-15 13:14:02 GMT-1')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('2000-03-15 12:14:03 GMT-2')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('2000-03-15 03:14:04 PST+8')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('2000-03-15 02:14:05 MST+7:00')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('Feb 10 17:32:01 1997 -0800')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('Feb 10 17:32:01 1997')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('Feb 10 5:32PM 1997')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('1997/02/10 17:32:01-0800')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('1997-02-10 17:32:01 PST')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('Feb-10-1997 17:32:01 PST')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('02-10-1997 17:32:01 PST')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('19970210 173201 PST')",
    "SET DATESTYLE TO YMD",
    "INSERT INTO TIMESTAMP_TBL VALUES ('97FEB10 5:32:01PM UTC')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('97/02/10 17:32:01 UTC')",
    "RESET DATESTYLE",
    "INSERT INTO TIMESTAMP_TBL VALUES ('1997.041 17:32:01 UTC')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('19970210 173201 America/New_York')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('1997-06-10 18:32:01 PDT')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('Feb 10 17:32:01 1997')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('Feb 11 17:32:01 1997')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('Feb 12 17:32:01 1997')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('Feb 13 17:32:01 1997')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('Feb 14 17:32:01 1997')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('Feb 15 17:32:01 1997')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('Feb 16 17:32:01 1997')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('Feb 16 17:32:01 0097 BC')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('Feb 16 17:32:01 0097')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('Feb 16 17:32:01 0597')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('Feb 16 17:32:01 1097')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('Feb 16 17:32:01 1697')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('Feb 16 17:32:01 1797')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('Feb 16 17:32:01 1897')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('Feb 16 17:32:01 1997')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('Feb 16 17:32:01 2097')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('Feb 28 17:32:01 1996')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('Feb 29 17:32:01 1996')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('Mar 01 17:32:01 1996')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('Dec 30 17:32:01 1996')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('Dec 31 17:32:01 1996')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('Jan 01 17:32:01 1997')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('Feb 28 17:32:01 1997')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('Mar 01 17:32:01 1997')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('Dec 30 17:32:01 1997')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('Dec 31 17:32:01 1997')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('Dec 31 17:32:01 1999')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('Jan 01 17:32:01 2000')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('Dec 31 17:32:01 2000')",
    "INSERT INTO TIMESTAMP_TBL VALUES ('Jan 01 17:32:01 2001')",
];

const TIMESTAMPTZ_TBL_SETUP: &[&str] = &[
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('-infinity')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('infinity')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('epoch')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('Mon Feb 10 17:32:01 1997 PST')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('Mon Feb 10 17:32:01.000001 1997 PST')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('Mon Feb 10 17:32:01.999999 1997 PST')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('Mon Feb 10 17:32:01.4 1997 PST')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('Mon Feb 10 17:32:01.5 1997 PST')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('Mon Feb 10 17:32:01.6 1997 PST')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('1997-01-02')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('1997-01-02 03:04:05')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('1997-02-10 17:32:01-08')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('1997-02-10 17:32:01-0800')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('1997-02-10 17:32:01 -08:00')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('19970210 173201 -0800')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('1997-06-10 17:32:01 -07:00')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('2001-09-22T18:19:20')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('2000-03-15 08:14:01 GMT+8')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('2000-03-15 13:14:02 GMT-1')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('2000-03-15 12:14:03 GMT-2')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('2000-03-15 03:14:04 PST+8')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('2000-03-15 02:14:05 MST+7:00')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('Feb 10 17:32:01 1997 -0800')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('Feb 10 17:32:01 1997')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('Feb 10 5:32PM 1997')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('1997/02/10 17:32:01-0800')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('1997-02-10 17:32:01 PST')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('Feb-10-1997 17:32:01 PST')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('02-10-1997 17:32:01 PST')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('19970210 173201 PST')",
    "SET DATESTYLE TO YMD",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('97FEB10 5:32:01PM UTC')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('97/02/10 17:32:01 UTC')",
    "RESET DATESTYLE",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('1997.041 17:32:01 UTC')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('19970210 173201 America/New_York')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('19970710 173201 America/New_York')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('1997-06-10 18:32:01 PDT')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('Feb 10 17:32:01 1997')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('Feb 11 17:32:01 1997')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('Feb 12 17:32:01 1997')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('Feb 13 17:32:01 1997')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('Feb 14 17:32:01 1997')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('Feb 15 17:32:01 1997')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('Feb 16 17:32:01 1997')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('Feb 16 17:32:01 0097 BC')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('Feb 16 17:32:01 0097')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('Feb 16 17:32:01 0597')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('Feb 16 17:32:01 1097')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('Feb 16 17:32:01 1697')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('Feb 16 17:32:01 1797')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('Feb 16 17:32:01 1897')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('Feb 16 17:32:01 1997')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('Feb 16 17:32:01 2097')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('Feb 28 17:32:01 1996')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('Feb 29 17:32:01 1996')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('Mar 01 17:32:01 1996')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('Dec 30 17:32:01 1996')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('Dec 31 17:32:01 1996')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('Jan 01 17:32:01 1997')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('Feb 28 17:32:01 1997')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('Mar 01 17:32:01 1997')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('Dec 30 17:32:01 1997')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('Dec 31 17:32:01 1997')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('Dec 31 17:32:01 1999')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('Jan 01 17:32:01 2000')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('Dec 31 17:32:01 2000')",
    "INSERT INTO TIMESTAMPTZ_TBL VALUES ('Jan 01 17:32:01 2001')",
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use aiondb_catalog_store::CatalogStore;
    use aiondb_core::Value;
    use aiondb_engine::{
        Credential, EngineBuilder, QueryEngine, StartupParams, StatementResult, TransportInfo,
    };
    use aiondb_storage_engine::InMemoryStorage;

    fn build_engine_and_session() -> (aiondb_engine::Engine, aiondb_engine::SessionHandle) {
        let catalog = Arc::new(CatalogStore::new());
        let storage = Arc::new(InMemoryStorage::new_without_wal());
        let engine = EngineBuilder::for_testing()
            .with_catalog_txn(catalog.clone())
            .with_catalog_reader(catalog.clone())
            .with_catalog_writer(catalog.clone())
            .with_sequence_manager(catalog)
            .with_storage_ddl(storage.clone())
            .with_storage_dml(storage.clone())
            .with_storage_txn(storage)
            .build()
            .expect("engine");

        let mut options = BTreeMap::new();
        options.insert("timezone".to_owned(), "PST8PDT".to_owned());
        options.insert("datestyle".to_owned(), "Postgres, MDY".to_owned());
        let (session, _) = engine
            .startup(StartupParams {
                database: "default".to_owned(),
                application_name: Some("pg-regress-tests".to_owned()),
                options,
                credential: Credential::Anonymous {
                    user: "alice".to_owned(),
                },
                transport: TransportInfo::in_process(),
            })
            .expect("startup");

        (engine, session)
    }

    fn query_count(
        engine: &aiondb_engine::Engine,
        session: &aiondb_engine::SessionHandle,
        sql: &str,
    ) -> i64 {
        let results = engine.execute_sql(session, sql).expect("query");
        let StatementResult::Query { rows, .. } = &results[0] else {
            panic!("expected query result");
        };
        match rows[0].values[0] {
            Value::Int(value) => i64::from(value),
            Value::BigInt(value) => value,
            ref other => panic!("unexpected count value: {other:?}"),
        }
    }

    #[test]
    fn timestamptz_seed_statements_all_apply_cleanly() {
        let (engine, session) = build_engine_and_session();
        engine
            .execute_sql(
                &session,
                "CREATE TABLE TIMESTAMPTZ_TBL (d1 timestamp(2) with time zone)",
            )
            .expect("create timestamptz table");

        let failures: Vec<String> = TIMESTAMPTZ_TBL_SETUP
            .iter()
            .filter_map(|stmt| {
                let effective = effective_seed_statement("timestamptz_tbl", stmt);
                engine
                    .execute_sql(&session, effective.as_ref())
                    .err()
                    .map(|error| format!("{stmt} => {}", error.report().message))
            })
            .collect();

        assert!(
            failures.is_empty(),
            "timestamptz seed failures:\n{}",
            failures.join("\n")
        );
    }

    #[test]
    fn timestamptz_seed_row_count_matches_horology_expectation() {
        let (engine, session) = build_engine_and_session();
        engine
            .execute_sql(
                &session,
                "CREATE TABLE TIMESTAMPTZ_TBL (d1 timestamp(2) with time zone)",
            )
            .expect("create timestamptz table");
        for stmt in TIMESTAMPTZ_TBL_SETUP {
            let effective = effective_seed_statement("timestamptz_tbl", stmt);
            engine
                .execute_sql(&session, effective.as_ref())
                .expect("seed statement");
        }

        assert_eq!(
            query_count(&engine, &session, "SELECT count(*) FROM TIMESTAMPTZ_TBL"),
            66
        );
    }

    #[test]
    fn timestamp_interval_window_row_count_matches_horology_expectation() {
        let (engine, session) = build_engine_and_session();
        engine
            .execute_sql(
                &session,
                "CREATE TABLE TIMESTAMP_TBL (d1 timestamp(2) without time zone)",
            )
            .expect("create timestamp table");
        engine
            .execute_sql(&session, "CREATE TABLE INTERVAL_TBL (f1 interval)")
            .expect("create interval table");
        for stmt in TIMESTAMP_TBL_SETUP {
            let effective = effective_seed_statement("timestamp_tbl", stmt);
            engine
                .execute_sql(&session, effective.as_ref())
                .expect("timestamp seed");
        }
        for stmt in INTERVAL_TBL_SETUP {
            engine.execute_sql(&session, stmt).expect("interval seed");
        }

        assert_eq!(
            query_count(
                &engine,
                &session,
                "SELECT count(*) \
                 FROM TIMESTAMP_TBL t, INTERVAL_TBL i \
                 WHERE t.d1 BETWEEN '1990-01-01' AND '2001-01-01' \
                   AND i.f1 BETWEEN '00:00' AND '23:00'"
            ),
            104
        );
    }
}
