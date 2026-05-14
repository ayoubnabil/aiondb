use aiondb_embedded::Connection;
use aiondb_engine::{Engine, StatementResult, Value};
use rusqlite::types::Value as SqliteValue;
use rusqlite::Connection as SqliteConnection;

use crate::harness::{SuiteResult, SuiteStats, TestDb};

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Command,
    Query(Vec<Vec<String>>),
    Error(String),
}

pub fn run_corpus() -> SuiteResult {
    let db = TestDb::new();
    let aion = db.conn();
    let mut sqlite = match SqliteConnection::open_in_memory() {
        Ok(conn) => conn,
        Err(error) => return Err(vec![format!("sqlite setup failed: {error}")]),
    };

    let mut passed = 0_usize;
    let mut failures = Vec::new();

    for sql in setup_statements() {
        match compare_sql(&aion, &mut sqlite, &sql) {
            Ok(()) => passed += 1,
            Err(error) => failures.push(format!("setup mismatch for `{sql}`: {error}")),
        }
        if failures.len() >= 50 {
            failures.push("... setup failures truncated after 50 mismatches".to_owned());
            break;
        }
    }

    if failures.is_empty() {
        for (name, sql) in build_query_corpus() {
            match compare_sql(&aion, &mut sqlite, &sql) {
                Ok(()) => passed += 1,
                Err(error) => failures.push(format!("{name}: {error}")),
            }
            if failures.len() >= 80 {
                failures.push("... query failures truncated after 80 mismatches".to_owned());
                break;
            }
        }
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

pub fn run_stateful_fuzz(iterations: usize) -> SuiteResult {
    let db = TestDb::new();
    let aion = db.conn();
    let mut sqlite = match SqliteConnection::open_in_memory() {
        Ok(conn) => conn,
        Err(error) => return Err(vec![format!("sqlite setup failed: {error}")]),
    };

    let mut passed = 0_usize;
    let mut failures = Vec::new();
    let mut rng = FuzzRng::seeded(0xA10D_D1FF_F002);

    let bootstrap = [
        "CREATE TABLE diff_fuzz (id INTEGER PRIMARY KEY, v INTEGER NOT NULL, tag TEXT)",
        "CREATE INDEX diff_fuzz_v_idx ON diff_fuzz (v)",
    ];
    for sql in bootstrap {
        match compare_sql(&aion, &mut sqlite, sql) {
            Ok(()) => passed += 1,
            Err(error) => failures.push(format!("bootstrap mismatch for `{sql}`: {error}")),
        }
    }

    if !failures.is_empty() {
        return Err(failures);
    }

    for i in 0..iterations {
        let action = rng.next_range(0, 100);
        let sql = if action < 50 {
            let id = rng.next_range(1, 240);
            let value = rng.next_i64(-50_000, 50_000);
            let tag = format!("t{}", rng.next_range(0, 80));
            format!(
                "INSERT INTO diff_fuzz (id, v, tag) VALUES ({id}, {value}, '{tag}') \
                 ON CONFLICT (id) DO UPDATE SET v = EXCLUDED.v, tag = EXCLUDED.tag"
            )
        } else if action < 75 {
            let id = rng.next_range(1, 240);
            let delta = rng.next_i64(-99, 99);
            format!("UPDATE diff_fuzz SET v = v + ({delta}) WHERE id = {id}")
        } else if action < 87 {
            let id = rng.next_range(1, 240);
            format!("DELETE FROM diff_fuzz WHERE id = {id}")
        } else if action < 94 {
            let low = rng.next_i64(-2_000, 2_000);
            let high = low + rng.next_i64(0, 2_000);
            format!(
                "SELECT count(*), coalesce(sum(v), 0), coalesce(min(v), 0), coalesce(max(v), 0) \
                 FROM diff_fuzz WHERE v BETWEEN {low} AND {high}"
            )
        } else {
            let n = rng.next_range(1, 30);
            format!("SELECT id, v, coalesce(tag, 'NULL') FROM diff_fuzz ORDER BY id LIMIT {n}")
        };

        match compare_sql(&aion, &mut sqlite, &sql) {
            Ok(()) => passed += 1,
            Err(error) => failures.push(format!("iter {i} sql `{}`: {error}", truncate(&sql, 180))),
        }

        if i % 25 == 0 {
            let checks = [
                "SELECT count(*), coalesce(sum(v), 0), coalesce(min(v), 0), coalesce(max(v), 0) FROM diff_fuzz",
                "SELECT id, v, coalesce(tag, 'NULL') FROM diff_fuzz ORDER BY id LIMIT 120",
            ];
            for check in checks {
                match compare_sql(&aion, &mut sqlite, check) {
                    Ok(()) => passed += 1,
                    Err(error) => {
                        failures.push(format!("iter {i} check mismatch `{check}`: {error}"))
                    }
                }
            }
        }

        if failures.len() >= 80 {
            failures.push(format!("... truncated after 80 mismatches (iteration {i})"));
            break;
        }
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

pub fn run_expression_matrix() -> SuiteResult {
    let db = TestDb::new();
    let aion = db.conn();
    let mut sqlite = match SqliteConnection::open_in_memory() {
        Ok(conn) => conn,
        Err(error) => return Err(vec![format!("sqlite setup failed: {error}")]),
    };

    let mut passed = 0_usize;
    let mut failures = Vec::new();

    for left in -48_i64..=48_i64 {
        for right in -48_i64..=48_i64 {
            let expr_cases = [
                format!("SELECT ({left}) + ({right}), ({left}) - ({right}), ({left}) * ({right})"),
                format!("SELECT ({left}) < ({right}), ({left}) <= ({right}), ({left}) = ({right}), ({left}) <> ({right})"),
                format!("SELECT abs({left}), abs({right}), ({left}) % 7, ({right}) % 11"),
                format!("SELECT CASE WHEN ({left}) > ({right}) THEN 'gt' WHEN ({left}) = ({right}) THEN 'eq' ELSE 'lt' END"),
            ];

            for sql in &expr_cases {
                match compare_sql(&aion, &mut sqlite, sql) {
                    Ok(()) => passed += 1,
                    Err(error) => failures.push(format!(
                        "expr left={left} right={right}: {error} (sql: {sql})"
                    )),
                }
            }

            if right != 0 {
                let sql = format!("SELECT ({left}) / ({right})");
                match compare_sql(&aion, &mut sqlite, &sql) {
                    Ok(()) => passed += 1,
                    Err(error) => failures.push(format!(
                        "div left={left} right={right}: {error} (sql: {sql})"
                    )),
                }
            }

            if failures.len() >= 120 {
                failures.push("... expression matrix truncated after 120 mismatches".to_owned());
                break;
            }
        }
        if failures.len() >= 120 {
            break;
        }
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

pub fn run_history_replay(steps: usize) -> SuiteResult {
    let db = TestDb::new();
    let aion = db.conn();
    let mut sqlite = match SqliteConnection::open_in_memory() {
        Ok(conn) => conn,
        Err(error) => return Err(vec![format!("sqlite setup failed: {error}")]),
    };

    let mut passed = 0_usize;
    let mut failures = Vec::new();
    let mut rng = FuzzRng::seeded(0xA10D_1570_F111);

    let bootstrap = [
        "CREATE TABLE hist_accounts (id INTEGER PRIMARY KEY, balance INTEGER NOT NULL, version INTEGER NOT NULL, tag TEXT)",
        "CREATE TABLE hist_ledger (id INTEGER PRIMARY KEY, from_id INTEGER, to_id INTEGER, amount INTEGER NOT NULL, kind TEXT NOT NULL)",
    ];
    for sql in bootstrap {
        match compare_sql(&aion, &mut sqlite, sql) {
            Ok(()) => passed += 1,
            Err(error) => failures.push(format!("bootstrap mismatch for `{sql}`: {error}")),
        }
    }

    if !failures.is_empty() {
        return Err(failures);
    }

    for id in 1..=60_u64 {
        let balance = 100_000_i64 + (id as i64 * 17_i64);
        let sql = format!(
            "INSERT INTO hist_accounts (id, balance, version, tag) VALUES ({id}, {balance}, 1, 'seed')"
        );
        match compare_sql(&aion, &mut sqlite, &sql) {
            Ok(()) => passed += 1,
            Err(error) => failures.push(format!("seed account {id} mismatch: {error}")),
        }
    }
    if !failures.is_empty() {
        return Err(failures);
    }

    let mut next_ledger_id = 1_u64;
    for step in 0..steps {
        let action = rng.next_range(0, 100);
        if action < 50 {
            let from_id = rng.next_range(1, 61);
            let mut to_id = rng.next_range(1, 61);
            if to_id == from_id {
                to_id = if to_id == 60 { 1 } else { to_id + 1 };
            }
            let amount = rng.next_i64(1, 700);
            let tx_sql = format!(
                "BEGIN; \
                 UPDATE hist_accounts SET balance = balance - ({amount}), version = version + 1 WHERE id = {from_id}; \
                 UPDATE hist_accounts SET balance = balance + ({amount}), version = version + 1 WHERE id = {to_id}; \
                 INSERT INTO hist_ledger (id, from_id, to_id, amount, kind) VALUES ({next_ledger_id}, {from_id}, {to_id}, {amount}, 'transfer'); \
                 COMMIT;"
            );
            next_ledger_id += 1;
            match compare_sql(&aion, &mut sqlite, &tx_sql) {
                Ok(()) => passed += 1,
                Err(error) => failures.push(format!("step {step} transfer mismatch: {error}")),
            }
        } else if action < 72 {
            let id = rng.next_range(1, 61);
            let delta = rng.next_i64(-200, 200);
            let sql = format!(
                "UPDATE hist_accounts SET balance = balance + ({delta}), version = version + 1 WHERE id = {id}"
            );
            match compare_sql(&aion, &mut sqlite, &sql) {
                Ok(()) => passed += 1,
                Err(error) => failures.push(format!("step {step} delta mismatch: {error}")),
            }
        } else if action < 88 {
            let id = rng.next_range(1, 61);
            let tag = format!("tag_{}", rng.next_range(0, 100));
            let sql = format!("UPDATE hist_accounts SET tag = '{tag}' WHERE id = {id}");
            match compare_sql(&aion, &mut sqlite, &sql) {
                Ok(()) => passed += 1,
                Err(error) => failures.push(format!("step {step} tag mismatch: {error}")),
            }
        } else {
            let id = rng.next_range(1, 61);
            let snapshot_sql = format!(
                "SELECT id, balance, version, coalesce(tag, 'NULL') FROM hist_accounts WHERE id = {id}"
            );
            match compare_sql(&aion, &mut sqlite, &snapshot_sql) {
                Ok(()) => passed += 1,
                Err(error) => failures.push(format!("step {step} point-check mismatch: {error}")),
            }
        }

        if step % 15 == 0 {
            let checks = [
                "SELECT count(*), coalesce(sum(balance), 0), coalesce(sum(version), 0) FROM hist_accounts",
                "SELECT count(*), coalesce(sum(amount), 0) FROM hist_ledger",
                "SELECT coalesce(min(balance), 0), coalesce(max(balance), 0) FROM hist_accounts",
                "SELECT id, balance, version FROM hist_accounts ORDER BY id LIMIT 20",
            ];
            for check in checks {
                match compare_sql(&aion, &mut sqlite, check) {
                    Ok(()) => passed += 1,
                    Err(error) => {
                        failures.push(format!("step {step} invariant mismatch `{check}`: {error}"))
                    }
                }
            }
        }

        if failures.len() >= 120 {
            failures.push(format!(
                "... history replay truncated after 120 mismatches at step {step}"
            ));
            break;
        }
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

pub fn run_predicate_matrix() -> SuiteResult {
    let db = TestDb::new();
    let aion = db.conn();
    let mut sqlite = match SqliteConnection::open_in_memory() {
        Ok(conn) => conn,
        Err(error) => return Err(vec![format!("sqlite setup failed: {error}")]),
    };

    let mut passed = 0_usize;
    let mut failures = Vec::new();

    for sql in setup_statements() {
        match compare_sql(&aion, &mut sqlite, &sql) {
            Ok(()) => passed += 1,
            Err(error) => failures.push(format!("setup mismatch for `{sql}`: {error}")),
        }
    }
    if !failures.is_empty() {
        return Err(failures);
    }

    for low in (-120_i64..=220_i64).step_by(10) {
        for high in (-100_i64..=260_i64).step_by(10) {
            let q1 = format!(
                "SELECT count(*), coalesce(sum(amount), 0) FROM diff_orders WHERE amount BETWEEN {low} AND {high}"
            );
            let q2 = format!(
                "SELECT count(*) FROM diff_orders WHERE amount >= {low} AND amount <= {high} AND status <> 'cancelled'"
            );
            let q3 = format!(
                "SELECT count(*), count(DISTINCT user_id) FROM diff_orders WHERE amount > {low} AND amount < {high}"
            );
            let queries = [q1, q2, q3];

            for sql in &queries {
                match compare_sql(&aion, &mut sqlite, sql) {
                    Ok(()) => passed += 1,
                    Err(error) => failures.push(format!(
                        "predicate low={low} high={high}: {error} (sql: {sql})"
                    )),
                }
            }

            if failures.len() >= 120 {
                failures.push("... predicate matrix truncated after 120 mismatches".to_owned());
                break;
            }
        }
        if failures.len() >= 120 {
            break;
        }
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

pub fn run_group_order_matrix() -> SuiteResult {
    let db = TestDb::new();
    let aion = db.conn();
    let mut sqlite = match SqliteConnection::open_in_memory() {
        Ok(conn) => conn,
        Err(error) => return Err(vec![format!("sqlite setup failed: {error}")]),
    };

    let mut passed = 0_usize;
    let mut failures = Vec::new();

    for sql in setup_statements() {
        match compare_sql(&aion, &mut sqlite, &sql) {
            Ok(()) => passed += 1,
            Err(error) => failures.push(format!("setup mismatch for `{sql}`: {error}")),
        }
    }
    if !failures.is_empty() {
        return Err(failures);
    }

    for limit in 1_u64..=90_u64 {
        let q1 = format!(
            "SELECT id, amount FROM diff_orders ORDER BY amount DESC, id ASC LIMIT {limit}"
        );
        let q2 = format!(
            "SELECT user_id, count(*), coalesce(sum(amount), 0) \
             FROM diff_orders GROUP BY user_id HAVING count(*) >= 5 ORDER BY user_id LIMIT {limit}"
        );
        let q3 = format!(
            "SELECT status, coalesce(avg(amount), 0), coalesce(min(amount), 0), coalesce(max(amount), 0) \
             FROM diff_orders GROUP BY status ORDER BY status LIMIT {limit}"
        );
        let queries = [q1, q2, q3];
        for sql in &queries {
            match compare_sql(&aion, &mut sqlite, sql) {
                Ok(()) => passed += 1,
                Err(error) => {
                    failures.push(format!("group/order limit={limit}: {error} (sql: {sql})"))
                }
            }
        }
        if failures.len() >= 120 {
            failures.push("... group/order matrix truncated after 120 mismatches".to_owned());
            break;
        }
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

pub fn run_join_matrix() -> SuiteResult {
    let db = TestDb::new();
    let aion = db.conn();
    let mut sqlite = match SqliteConnection::open_in_memory() {
        Ok(conn) => conn,
        Err(error) => return Err(vec![format!("sqlite setup failed: {error}")]),
    };

    let mut passed = 0_usize;
    let mut failures = Vec::new();

    for sql in setup_statements() {
        match compare_sql(&aion, &mut sqlite, &sql) {
            Ok(()) => passed += 1,
            Err(error) => failures.push(format!("setup mismatch for `{sql}`: {error}")),
        }
    }
    if !failures.is_empty() {
        return Err(failures);
    }

    const STATUSES: &[&str] = &["new", "paid", "shipped", "cancelled"];
    for threshold in (-120_i64..=300_i64).step_by(8) {
        for status in STATUSES {
            let q1 = format!(
                "SELECT count(*) FROM diff_users u JOIN diff_orders o ON u.id = o.user_id \
                 WHERE o.amount >= {threshold} AND o.status = '{status}'"
            );
            let q2 = format!(
                "SELECT count(*) FROM diff_users u WHERE EXISTS ( \
                 SELECT 1 FROM diff_orders o WHERE o.user_id = u.id AND o.amount >= {threshold} AND o.status = '{status}')"
            );
            let q3 = format!(
                "SELECT u.id, coalesce(sum(o.amount), 0) FROM diff_users u \
                 LEFT JOIN diff_orders o ON u.id = o.user_id \
                 WHERE o.status = '{status}' OR o.status IS NULL \
                 GROUP BY u.id ORDER BY u.id LIMIT 20"
            );

            let queries = [q1, q2, q3];
            for sql in &queries {
                match compare_sql(&aion, &mut sqlite, sql) {
                    Ok(()) => passed += 1,
                    Err(error) => {
                        failures.push(format!(
                            "join threshold={threshold} status={status}: {error} (sql: {sql})"
                        ));
                    }
                }
            }

            if failures.len() >= 120 {
                failures.push("... join matrix truncated after 120 mismatches".to_owned());
                break;
            }
        }
        if failures.len() >= 120 {
            break;
        }
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

pub fn run_cte_union_matrix() -> SuiteResult {
    let db = TestDb::new();
    let aion = db.conn();
    let mut sqlite = match SqliteConnection::open_in_memory() {
        Ok(conn) => conn,
        Err(error) => return Err(vec![format!("sqlite setup failed: {error}")]),
    };

    let mut passed = 0_usize;
    let mut failures = Vec::new();

    for sql in setup_statements() {
        match compare_sql(&aion, &mut sqlite, &sql) {
            Ok(()) => passed += 1,
            Err(error) => failures.push(format!("setup mismatch for `{sql}`: {error}")),
        }
    }
    if !failures.is_empty() {
        return Err(failures);
    }

    for base in (1_u64..=120_u64).step_by(3) {
        let top = (base + 30).min(420);
        let mid = ((base + top) / 2).max(base + 1);
        let q1 = format!(
            "WITH a AS (SELECT id, amount FROM diff_orders WHERE id BETWEEN {base} AND {mid}), \
                  b AS (SELECT id, amount FROM diff_orders WHERE id BETWEEN {mid} AND {top}) \
             SELECT count(*), coalesce(sum(amount), 0) FROM (SELECT * FROM a UNION ALL SELECT * FROM b) t"
        );
        let q2 = format!(
            "WITH c AS (SELECT user_id, count(*) AS c FROM diff_orders WHERE id BETWEEN {base} AND {top} GROUP BY user_id) \
             SELECT count(*), coalesce(sum(c), 0), coalesce(max(c), 0) FROM c"
        );
        let q3 = format!(
            "SELECT count(*) FROM ( \
               SELECT user_id FROM diff_orders WHERE id BETWEEN {base} AND {mid} \
               UNION \
               SELECT user_id FROM diff_orders WHERE id BETWEEN {mid} AND {top} \
             ) u"
        );

        let queries = [q1, q2, q3];
        for sql in &queries {
            match compare_sql(&aion, &mut sqlite, sql) {
                Ok(()) => passed += 1,
                Err(error) => failures.push(format!("cte/union base={base}: {error} (sql: {sql})")),
            }
        }

        if failures.len() >= 120 {
            failures.push("... cte/union matrix truncated after 120 mismatches".to_owned());
            break;
        }
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

pub fn run_error_class_matrix() -> SuiteResult {
    let db = TestDb::new();
    let aion = db.conn();
    let mut sqlite = match SqliteConnection::open_in_memory() {
        Ok(conn) => conn,
        Err(error) => return Err(vec![format!("sqlite setup failed: {error}")]),
    };

    let mut passed = 0_usize;
    let mut failures = Vec::new();

    let bootstrap = [
        "CREATE TABLE err_t (id INTEGER PRIMARY KEY, v INTEGER NOT NULL)",
        "INSERT INTO err_t VALUES (1, 10)",
        "INSERT INTO err_t VALUES (2, 20)",
    ];
    for sql in bootstrap {
        match compare_sql(&aion, &mut sqlite, sql) {
            Ok(()) => passed += 1,
            Err(error) => failures.push(format!("bootstrap mismatch for `{sql}`: {error}")),
        }
    }
    if !failures.is_empty() {
        return Err(failures);
    }

    let mut cases = vec![
        "SELEC 1".to_owned(),
        "SELECT * FROM definitely_missing_table".to_owned(),
        "INSERT INTO err_t VALUES (1, 999)".to_owned(),
        "UPDATE err_t SET missing_col = 1 WHERE id = 1".to_owned(),
    ];

    for i in 0_u64..=120_u64 {
        cases.push(format!("SELECT * FROM missing_table_{i}"));
        cases.push(format!(
            "UPDATE err_t SET v = v + 1 WHERE missing_col_{i} = 1"
        ));
    }

    for sql in &cases {
        match compare_sql(&aion, &mut sqlite, sql) {
            Ok(()) => passed += 1,
            Err(error) => failures.push(format!("error-class mismatch for `{sql}`: {error}")),
        }
        if failures.len() >= 120 {
            failures.push("... error-class matrix truncated after 120 mismatches".to_owned());
            break;
        }
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

pub fn run_txn_metamorphic(iterations: usize) -> SuiteResult {
    let db = TestDb::new();
    let aion = db.conn();
    let mut sqlite = match SqliteConnection::open_in_memory() {
        Ok(conn) => conn,
        Err(error) => return Err(vec![format!("sqlite setup failed: {error}")]),
    };

    let mut passed = 0_usize;
    let mut failures = Vec::new();
    let mut rng = FuzzRng::seeded(0xA10D_7A11_0F22);

    let bootstrap = [
        "CREATE TABLE tx_meta (id INTEGER PRIMARY KEY, val INTEGER NOT NULL)",
        "INSERT INTO tx_meta VALUES (1, 1000), (2, 1000), (3, 1000), (4, 1000), (5, 1000)",
    ];
    for sql in bootstrap {
        match compare_sql(&aion, &mut sqlite, sql) {
            Ok(()) => passed += 1,
            Err(error) => failures.push(format!("bootstrap mismatch for `{sql}`: {error}")),
        }
    }
    if !failures.is_empty() {
        return Err(failures);
    }

    for i in 0..iterations {
        let from = rng.next_range(1, 6);
        let mut to = rng.next_range(1, 6);
        if to == from {
            to = if to == 5 { 1 } else { to + 1 };
        }
        let amount = rng.next_i64(1, 50);
        let should_rollback = rng.next_range(0, 100) < 30;
        let tx_sql = if should_rollback {
            format!(
                "BEGIN; \
                 UPDATE tx_meta SET val = val - ({amount}) WHERE id = {from}; \
                 UPDATE tx_meta SET val = val + ({amount}) WHERE id = {to}; \
                 ROLLBACK;"
            )
        } else {
            format!(
                "BEGIN; \
                 UPDATE tx_meta SET val = val - ({amount}) WHERE id = {from}; \
                 UPDATE tx_meta SET val = val + ({amount}) WHERE id = {to}; \
                 COMMIT;"
            )
        };

        match compare_sql(&aion, &mut sqlite, &tx_sql) {
            Ok(()) => passed += 1,
            Err(error) => failures.push(format!("iter {i} tx mismatch: {error}")),
        }

        if i % 10 == 0 {
            let checks = [
                "SELECT count(*), coalesce(sum(val), 0), coalesce(min(val), 0), coalesce(max(val), 0) FROM tx_meta",
                "SELECT id, val FROM tx_meta ORDER BY id",
            ];
            for sql in checks {
                match compare_sql(&aion, &mut sqlite, sql) {
                    Ok(()) => passed += 1,
                    Err(error) => {
                        failures.push(format!("iter {i} check mismatch `{sql}`: {error}"))
                    }
                }
            }
        }

        if failures.len() >= 120 {
            failures.push(format!(
                "... txn metamorphic truncated after 120 mismatches at iter {i}"
            ));
            break;
        }
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

pub fn run_null_semantics_matrix() -> SuiteResult {
    let db = TestDb::new();
    let aion = db.conn();
    let mut sqlite = match SqliteConnection::open_in_memory() {
        Ok(conn) => conn,
        Err(error) => return Err(vec![format!("sqlite setup failed: {error}")]),
    };

    let mut passed = 0_usize;
    let mut failures = Vec::new();

    let bootstrap = ["CREATE TABLE null_mx (id INTEGER PRIMARY KEY, a INTEGER, b INTEGER, t TEXT)"];
    for sql in bootstrap {
        match compare_sql(&aion, &mut sqlite, sql) {
            Ok(()) => passed += 1,
            Err(error) => failures.push(format!("bootstrap mismatch for `{sql}`: {error}")),
        }
    }
    if !failures.is_empty() {
        return Err(failures);
    }

    let mut id = 1_u64;
    for a in -8_i64..=8_i64 {
        for b in -8_i64..=8_i64 {
            let ta = if a % 3 == 0 {
                "NULL".to_owned()
            } else {
                a.to_string()
            };
            let tb = if b % 4 == 0 {
                "NULL".to_owned()
            } else {
                b.to_string()
            };
            let txt = if (a + b) % 5 == 0 {
                "NULL".to_owned()
            } else {
                format!("'s{}'", (a * 31 + b * 17).abs())
            };
            let sql = format!("INSERT INTO null_mx VALUES ({id}, {ta}, {tb}, {txt})");
            id += 1;
            match compare_sql(&aion, &mut sqlite, &sql) {
                Ok(()) => passed += 1,
                Err(error) => failures.push(format!("insert null matrix mismatch: {error}")),
            }
        }
    }

    for k in -20_i64..=20_i64 {
        let q1 = "SELECT count(*) FROM null_mx WHERE a IS NULL OR b IS NULL".to_owned();
        let q2 = format!("SELECT count(*) FROM null_mx WHERE coalesce(a, {k}) = coalesce(b, {k})");
        let q3 = format!("SELECT count(*) FROM null_mx WHERE nullif(a, {k}) IS NULL");
        let q4 = "SELECT count(*) FROM null_mx WHERE (a = b) IS NULL".to_owned();
        let q5 =
            format!("SELECT coalesce(sum(coalesce(a, {k}) - coalesce(b, {k})), 0) FROM null_mx");
        let q6 = "SELECT count(*) FROM null_mx WHERE t IS NULL".to_owned();
        let queries = [q1, q2, q3, q4, q5, q6];
        for sql in &queries {
            match compare_sql(&aion, &mut sqlite, sql) {
                Ok(()) => passed += 1,
                Err(error) => failures.push(format!("null semantics k={k}: {error} (sql: {sql})")),
            }
        }
        if failures.len() >= 120 {
            failures.push("... null semantics matrix truncated after 120 mismatches".to_owned());
            break;
        }
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

pub fn run_string_matrix() -> SuiteResult {
    let db = TestDb::new();
    let aion = db.conn();
    let mut sqlite = match SqliteConnection::open_in_memory() {
        Ok(conn) => conn,
        Err(error) => return Err(vec![format!("sqlite setup failed: {error}")]),
    };

    let mut passed = 0_usize;
    let mut failures = Vec::new();

    let bootstrap = ["CREATE TABLE str_mx (id INTEGER PRIMARY KEY, s TEXT)"];
    for sql in bootstrap {
        match compare_sql(&aion, &mut sqlite, sql) {
            Ok(()) => passed += 1,
            Err(error) => failures.push(format!("bootstrap mismatch for `{sql}`: {error}")),
        }
    }
    if !failures.is_empty() {
        return Err(failures);
    }

    for id in 1_u64..=280_u64 {
        let payload = match id % 7 {
            0 => format!("'Alpha_{}'", id),
            1 => format!("' beta {} '", id),
            2 => format!("'x{}x{}x'", id % 10, id % 17),
            3 => "''".to_owned(),
            4 => "'mixEDCase'".to_owned(),
            5 => "'  spaced  text  '".to_owned(),
            _ => format!("'n{}'", id * 13),
        };
        let sql = format!("INSERT INTO str_mx VALUES ({id}, {payload})");
        match compare_sql(&aion, &mut sqlite, &sql) {
            Ok(()) => passed += 1,
            Err(error) => failures.push(format!("insert string matrix mismatch: {error}")),
        }
    }

    for n in 1_u64..=120_u64 {
        let q1 = format!(
            "SELECT count(*) FROM str_mx WHERE length(s) >= {}",
            (n % 12) + 1
        );
        let q2 = "SELECT count(*) FROM str_mx WHERE lower(s) LIKE 'a%' OR upper(s) LIKE '%X%'"
            .to_owned();
        let q3 = "SELECT count(*) FROM str_mx WHERE trim(s) <> s".to_owned();
        let q4 = format!(
            "SELECT id, substr(s, 1, {}) FROM str_mx ORDER BY id LIMIT 20",
            (n % 8) + 1
        );
        let q5 = "SELECT count(*), coalesce(sum(length(s)), 0) FROM str_mx".to_owned();
        let queries = [q1, q2, q3, q4, q5];
        for sql in &queries {
            match compare_sql(&aion, &mut sqlite, sql) {
                Ok(()) => passed += 1,
                Err(error) => failures.push(format!("string matrix n={n}: {error} (sql: {sql})")),
            }
        }
        if failures.len() >= 120 {
            failures.push("... string matrix truncated after 120 mismatches".to_owned());
            break;
        }
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

pub fn run_three_valued_logic_matrix() -> SuiteResult {
    let db = TestDb::new();
    let aion = db.conn();
    let mut sqlite = match SqliteConnection::open_in_memory() {
        Ok(conn) => conn,
        Err(error) => return Err(vec![format!("sqlite setup failed: {error}")]),
    };

    let mut passed = 0_usize;
    let mut failures = Vec::new();

    let values = ["NULL", "FALSE", "TRUE"];
    for x in &values {
        for y in &values {
            let exprs = [
                format!("({x}) AND ({y})"),
                format!("({x}) OR ({y})"),
                format!("NOT ({x})"),
                format!("({x}) = ({y})"),
                format!("({x}) <> ({y})"),
                format!("({x}) IS NULL"),
                format!("({x}) IS NOT NULL"),
            ];

            for expr in &exprs {
                let sql = format!(
                    "SELECT CASE \
                        WHEN ({expr}) THEN 'T' \
                        WHEN ({expr}) IS NULL THEN 'N' \
                        ELSE 'F' \
                     END"
                );
                match compare_sql(&aion, &mut sqlite, &sql) {
                    Ok(()) => passed += 1,
                    Err(error) => failures.push(format!("3vl x={x} y={y} expr={expr}: {error}")),
                }
            }

            if failures.len() >= 120 {
                failures.push("... 3vl matrix truncated after 120 mismatches".to_owned());
                break;
            }
        }
        if failures.len() >= 120 {
            break;
        }
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

pub fn run_schema_churn_matrix(iterations: usize) -> SuiteResult {
    let db = TestDb::new();
    let aion = db.conn();
    let mut sqlite = match SqliteConnection::open_in_memory() {
        Ok(conn) => conn,
        Err(error) => return Err(vec![format!("sqlite setup failed: {error}")]),
    };

    let mut passed = 0_usize;
    let mut failures = Vec::new();

    let bootstrap = [
        "CREATE TABLE sch_churn (id INTEGER PRIMARY KEY, a INTEGER NOT NULL, b INTEGER NOT NULL, note TEXT)",
    ];
    for sql in bootstrap {
        match compare_sql(&aion, &mut sqlite, sql) {
            Ok(()) => passed += 1,
            Err(error) => failures.push(format!("bootstrap mismatch for `{sql}`: {error}")),
        }
    }
    if !failures.is_empty() {
        return Err(failures);
    }

    for id in 1_u64..=220_u64 {
        let a = ((id * 17) % 1000) as i64 - 300;
        let b = ((id * 31) % 1500) as i64 - 500;
        let sql = format!("INSERT INTO sch_churn VALUES ({id}, {a}, {b}, 'n{id}')");
        match compare_sql(&aion, &mut sqlite, &sql) {
            Ok(()) => passed += 1,
            Err(error) => failures.push(format!("seed row {id} mismatch: {error}")),
        }
    }
    if !failures.is_empty() {
        return Err(failures);
    }

    let mut rng = FuzzRng::seeded(0xA10D_5C48_3A11);
    for i in 0..iterations {
        let action = rng.next_range(0, 100);
        let sql = if action < 16 {
            "CREATE INDEX IF NOT EXISTS sch_churn_a_idx ON sch_churn (a)".to_owned()
        } else if action < 32 {
            "CREATE INDEX IF NOT EXISTS sch_churn_b_idx ON sch_churn (b)".to_owned()
        } else if action < 40 {
            "DROP INDEX IF EXISTS sch_churn_a_idx".to_owned()
        } else if action < 48 {
            "DROP INDEX IF EXISTS sch_churn_b_idx".to_owned()
        } else if action < 70 {
            let id = rng.next_range(1, 221);
            let da = rng.next_i64(-40, 40);
            let db = rng.next_i64(-60, 60);
            format!("UPDATE sch_churn SET a = a + ({da}), b = b + ({db}) WHERE id = {id}")
        } else if action < 82 {
            let id = rng.next_range(1, 221);
            let na = rng.next_i64(-1000, 1000);
            let nb = rng.next_i64(-1500, 1500);
            format!(
                "INSERT INTO sch_churn (id, a, b, note) VALUES ({id}, {na}, {nb}, 'u{id}') \
                 ON CONFLICT (id) DO UPDATE SET a = EXCLUDED.a, b = EXCLUDED.b, note = EXCLUDED.note"
            )
        } else {
            let t = rng.next_i64(-400, 700);
            format!(
                "SELECT count(*), coalesce(sum(a), 0), coalesce(sum(b), 0) FROM sch_churn WHERE a >= {t}"
            )
        };

        match compare_sql(&aion, &mut sqlite, &sql) {
            Ok(()) => passed += 1,
            Err(error) => failures.push(format!(
                "iter {i} schema-churn mismatch: {error} (sql: {sql})"
            )),
        }

        if i % 20 == 0 {
            let checks = [
                "SELECT count(*), count(DISTINCT id) FROM sch_churn",
                "SELECT coalesce(min(a), 0), coalesce(max(a), 0), coalesce(min(b), 0), coalesce(max(b), 0) FROM sch_churn",
            ];
            for check in checks {
                match compare_sql(&aion, &mut sqlite, check) {
                    Ok(()) => passed += 1,
                    Err(error) => failures.push(format!(
                        "iter {i} schema-churn check mismatch `{check}`: {error}"
                    )),
                }
            }
        }

        if failures.len() >= 120 {
            failures.push(format!(
                "... schema-churn truncated after 120 mismatches at iter {i}"
            ));
            break;
        }
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

pub fn run_numeric_cast_matrix() -> SuiteResult {
    let db = TestDb::new();
    let aion = db.conn();
    let mut sqlite = match SqliteConnection::open_in_memory() {
        Ok(conn) => conn,
        Err(error) => return Err(vec![format!("sqlite setup failed: {error}")]),
    };

    let mut passed = 0_usize;
    let mut failures = Vec::new();

    for a in -90_i64..=90_i64 {
        for b in -20_i64..=20_i64 {
            let q1 = format!(
                "SELECT CAST({a} AS INTEGER), CAST({a}.25 AS REAL), CAST('{a}' AS INTEGER), abs({a})"
            );
            let q2 = format!(
                "SELECT ({a}) + ({b}), ({a}) - ({b}), ({a}) * ({b}), ({a}) % 7, ({b}) % 11"
            );
            let q3 = format!("SELECT CAST(({a}) + ({b}) AS INTEGER), CAST(({a}) * ({b}) AS REAL)");
            let queries = [q1, q2, q3];
            for sql in &queries {
                match compare_sql(&aion, &mut sqlite, sql) {
                    Ok(()) => passed += 1,
                    Err(error) => {
                        failures.push(format!("numeric a={a} b={b}: {error} (sql: {sql})"))
                    }
                }
            }

            if b != 0 {
                let q4 = format!("SELECT ({a}) / ({b}), CAST(({a}) AS REAL) / ({b})");
                match compare_sql(&aion, &mut sqlite, &q4) {
                    Ok(()) => passed += 1,
                    Err(error) => {
                        failures.push(format!("numeric division a={a} b={b}: {error} (sql: {q4})"))
                    }
                }
            }

            if failures.len() >= 120 {
                failures.push("... numeric/cast matrix truncated after 120 mismatches".to_owned());
                break;
            }
        }
        if failures.len() >= 120 {
            break;
        }
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

pub fn run_set_ops_matrix() -> SuiteResult {
    let db = TestDb::new();
    let aion = db.conn();
    let mut sqlite = match SqliteConnection::open_in_memory() {
        Ok(conn) => conn,
        Err(error) => return Err(vec![format!("sqlite setup failed: {error}")]),
    };

    let mut passed = 0_usize;
    let mut failures = Vec::new();

    let bootstrap = [
        "CREATE TABLE set_a (id INTEGER PRIMARY KEY, v INTEGER NOT NULL)",
        "CREATE TABLE set_b (id INTEGER PRIMARY KEY, v INTEGER NOT NULL)",
    ];
    for sql in bootstrap {
        match compare_sql(&aion, &mut sqlite, sql) {
            Ok(()) => passed += 1,
            Err(error) => failures.push(format!("bootstrap mismatch for `{sql}`: {error}")),
        }
    }
    if !failures.is_empty() {
        return Err(failures);
    }

    for id in 1_u64..=420_u64 {
        let va = ((id * 37) % 900) as i64 - 320;
        let vb = ((id * 53) % 1100) as i64 - 380;
        let s1 = format!("INSERT INTO set_a VALUES ({id}, {va})");
        let s2 = format!("INSERT INTO set_b VALUES ({id}, {vb})");
        for sql in [s1, s2] {
            match compare_sql(&aion, &mut sqlite, &sql) {
                Ok(()) => passed += 1,
                Err(error) => failures.push(format!("seed set-op row {id} mismatch: {error}")),
            }
        }
    }
    if !failures.is_empty() {
        return Err(failures);
    }

    for lo in (-320_i64..=320_i64).step_by(16) {
        let hi = lo + 220;
        let q1 = format!(
            "SELECT count(*) FROM (SELECT v FROM set_a WHERE v BETWEEN {lo} AND {hi} UNION SELECT v FROM set_b WHERE v BETWEEN {lo} AND {hi}) u"
        );
        let q2 = format!(
            "SELECT count(*) FROM (SELECT v FROM set_a WHERE v BETWEEN {lo} AND {hi} INTERSECT SELECT v FROM set_b WHERE v BETWEEN {lo} AND {hi}) i"
        );
        let q3 = format!(
            "SELECT count(*) FROM (SELECT v FROM set_a WHERE v BETWEEN {lo} AND {hi} EXCEPT SELECT v FROM set_b WHERE v BETWEEN {lo} AND {hi}) e"
        );
        let q4 = format!(
            "SELECT count(*) FROM (SELECT v FROM set_a WHERE v BETWEEN {lo} AND {hi} UNION ALL SELECT v FROM set_b WHERE v BETWEEN {lo} AND {hi}) ua"
        );
        for sql in [q1, q2, q3, q4] {
            match compare_sql(&aion, &mut sqlite, &sql) {
                Ok(()) => passed += 1,
                Err(error) => {
                    failures.push(format!("set-ops lo={lo} hi={hi}: {error} (sql: {sql})"))
                }
            }
        }
        if failures.len() >= 120 {
            failures.push("... set-ops matrix truncated after 120 mismatches".to_owned());
            break;
        }
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

pub fn run_rollback_storm(iterations: usize) -> SuiteResult {
    let db = TestDb::new();
    let aion = db.conn();
    let mut sqlite = match SqliteConnection::open_in_memory() {
        Ok(conn) => conn,
        Err(error) => return Err(vec![format!("sqlite setup failed: {error}")]),
    };

    let mut passed = 0_usize;
    let mut failures = Vec::new();
    let mut rng = FuzzRng::seeded(0xA10D_BAAC_5501);

    let bootstrap = ["CREATE TABLE rb_storm (id INTEGER PRIMARY KEY, v INTEGER NOT NULL)"];
    for sql in bootstrap {
        match compare_sql(&aion, &mut sqlite, sql) {
            Ok(()) => passed += 1,
            Err(error) => failures.push(format!("bootstrap mismatch for `{sql}`: {error}")),
        }
    }
    if !failures.is_empty() {
        return Err(failures);
    }

    for id in 1_u64..=80_u64 {
        let sql = format!("INSERT INTO rb_storm VALUES ({id}, 1000)");
        match compare_sql(&aion, &mut sqlite, &sql) {
            Ok(()) => passed += 1,
            Err(error) => failures.push(format!("seed rb row {id} mismatch: {error}")),
        }
    }
    if !failures.is_empty() {
        return Err(failures);
    }

    for i in 0..iterations {
        let from = rng.next_range(1, 81);
        let mut to = rng.next_range(1, 81);
        if to == from {
            to = if to == 80 { 1 } else { to + 1 };
        }
        let amount = rng.next_i64(1, 40);
        let rollback = (i % 3) == 0;
        let tx_sql = if rollback {
            format!(
                "BEGIN; \
                 UPDATE rb_storm SET v = v - ({amount}) WHERE id = {from}; \
                 UPDATE rb_storm SET v = v + ({amount}) WHERE id = {to}; \
                 ROLLBACK;"
            )
        } else {
            format!(
                "BEGIN; \
                 UPDATE rb_storm SET v = v - ({amount}) WHERE id = {from}; \
                 UPDATE rb_storm SET v = v + ({amount}) WHERE id = {to}; \
                 COMMIT;"
            )
        };
        match compare_sql(&aion, &mut sqlite, &tx_sql) {
            Ok(()) => passed += 1,
            Err(error) => failures.push(format!("rollback storm iter {i}: {error}")),
        }

        if i % 15 == 0 {
            for check in [
                "SELECT count(*), coalesce(sum(v), 0), coalesce(min(v), 0), coalesce(max(v), 0) FROM rb_storm",
                "SELECT id, v FROM rb_storm ORDER BY id LIMIT 30",
            ] {
                match compare_sql(&aion, &mut sqlite, check) {
                    Ok(()) => passed += 1,
                    Err(error) => failures.push(format!("rollback storm iter {i} check mismatch `{check}`: {error}")),
                }
            }
        }

        if failures.len() >= 120 {
            failures.push(format!(
                "... rollback storm truncated after 120 mismatches at iter {i}"
            ));
            break;
        }
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

pub fn run_case_aggregation_matrix() -> SuiteResult {
    let db = TestDb::new();
    let aion = db.conn();
    let mut sqlite = match SqliteConnection::open_in_memory() {
        Ok(conn) => conn,
        Err(error) => return Err(vec![format!("sqlite setup failed: {error}")]),
    };

    let mut passed = 0_usize;
    let mut failures = Vec::new();

    for sql in setup_statements() {
        match compare_sql(&aion, &mut sqlite, &sql) {
            Ok(()) => passed += 1,
            Err(error) => failures.push(format!("setup mismatch for `{sql}`: {error}")),
        }
    }
    if !failures.is_empty() {
        return Err(failures);
    }

    const STATUSES: &[&str] = &["new", "paid", "shipped", "cancelled"];
    for age_cut in (18_i64..=70_i64).step_by(2) {
        for amount_cut in (-120_i64..=320_i64).step_by(20) {
            for status in STATUSES {
                let q1 = format!(
                    "SELECT \
                        sum(CASE WHEN o.amount >= {amount_cut} THEN 1 ELSE 0 END), \
                        sum(CASE WHEN o.amount < {amount_cut} THEN 1 ELSE 0 END), \
                        coalesce(sum(CASE WHEN o.status = '{status}' THEN o.amount ELSE 0 END), 0) \
                     FROM diff_orders o"
                );
                let q2 = format!(
                    "SELECT \
                        count(*), \
                        sum(CASE WHEN u.age >= {age_cut} THEN 1 ELSE 0 END), \
                        sum(CASE WHEN u.city IN ('paris', 'lyon') THEN 1 ELSE 0 END) \
                     FROM diff_users u"
                );
                let q3 = format!(
                    "SELECT \
                        u.city, \
                        sum(CASE WHEN o.amount >= {amount_cut} THEN 1 ELSE 0 END), \
                        coalesce(sum(CASE WHEN o.status = '{status}' THEN o.amount ELSE 0 END), 0) \
                     FROM diff_users u JOIN diff_orders o ON u.id = o.user_id \
                     GROUP BY u.city ORDER BY u.city"
                );
                let queries = [q1, q2, q3];
                for sql in &queries {
                    match compare_sql(&aion, &mut sqlite, sql) {
                        Ok(()) => passed += 1,
                        Err(error) => failures.push(format!(
                            "case-agg age_cut={age_cut} amount_cut={amount_cut} status={status}: {error} (sql: {sql})"
                        )),
                    }
                }

                if failures.len() >= 120 {
                    failures.push(
                        "... case/aggregation matrix truncated after 120 mismatches".to_owned(),
                    );
                    break;
                }
            }
            if failures.len() >= 120 {
                break;
            }
        }
        if failures.len() >= 120 {
            break;
        }
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

pub fn run_distinct_having_matrix() -> SuiteResult {
    let db = TestDb::new();
    let aion = db.conn();
    let mut sqlite = match SqliteConnection::open_in_memory() {
        Ok(conn) => conn,
        Err(error) => return Err(vec![format!("sqlite setup failed: {error}")]),
    };

    let mut passed = 0_usize;
    let mut failures = Vec::new();

    for sql in setup_statements() {
        match compare_sql(&aion, &mut sqlite, &sql) {
            Ok(()) => passed += 1,
            Err(error) => failures.push(format!("setup mismatch for `{sql}`: {error}")),
        }
    }
    if !failures.is_empty() {
        return Err(failures);
    }

    for min_amt in (-120_i64..=320_i64).step_by(20) {
        for min_cnt in 1_i64..=12_i64 {
            let q1 = format!(
                "SELECT count(*) FROM (SELECT DISTINCT user_id FROM diff_orders WHERE amount >= {min_amt}) d"
            );
            let q2 = format!(
                "SELECT user_id, count(*) AS c FROM diff_orders WHERE amount >= {min_amt} GROUP BY user_id HAVING count(*) >= {min_cnt} ORDER BY user_id"
            );
            let q3 = format!(
                "SELECT status, count(DISTINCT user_id), coalesce(sum(amount), 0) \
                 FROM diff_orders WHERE amount >= {min_amt} GROUP BY status HAVING count(*) >= {min_cnt} ORDER BY status"
            );
            for sql in [q1, q2, q3] {
                match compare_sql(&aion, &mut sqlite, &sql) {
                    Ok(()) => passed += 1,
                    Err(error) => failures.push(format!(
                        "distinct/having min_amt={min_amt} min_cnt={min_cnt}: {error} (sql: {sql})"
                    )),
                }
            }
            if failures.len() >= 120 {
                failures
                    .push("... distinct/having matrix truncated after 120 mismatches".to_owned());
                break;
            }
        }
        if failures.len() >= 120 {
            break;
        }
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

pub fn run_correlated_subquery_matrix() -> SuiteResult {
    let db = TestDb::new();
    let aion = db.conn();
    let mut sqlite = match SqliteConnection::open_in_memory() {
        Ok(conn) => conn,
        Err(error) => return Err(vec![format!("sqlite setup failed: {error}")]),
    };

    let mut passed = 0_usize;
    let mut failures = Vec::new();

    for sql in setup_statements() {
        match compare_sql(&aion, &mut sqlite, &sql) {
            Ok(()) => passed += 1,
            Err(error) => failures.push(format!("setup mismatch for `{sql}`: {error}")),
        }
    }
    if !failures.is_empty() {
        return Err(failures);
    }

    for amt in (-120_i64..=280_i64).step_by(10) {
        let q1 = format!(
            "SELECT count(*) FROM diff_users u \
             WHERE EXISTS (SELECT 1 FROM diff_orders o WHERE o.user_id = u.id AND o.amount >= {amt})"
        );
        let q2 = format!(
            "SELECT count(*) FROM diff_users u \
             WHERE NOT EXISTS (SELECT 1 FROM diff_orders o WHERE o.user_id = u.id AND o.amount >= {amt})"
        );
        let q3 = format!(
            "SELECT u.id, \
                    (SELECT count(*) FROM diff_orders o WHERE o.user_id = u.id AND o.amount >= {amt}) AS c \
             FROM diff_users u ORDER BY u.id LIMIT 30"
        );
        let q4 = "SELECT count(*) FROM diff_orders o \
             WHERE o.amount >= (SELECT avg(o2.amount) FROM diff_orders o2 WHERE o2.user_id = o.user_id)"
            .to_owned();

        for sql in [q1, q2, q3, q4] {
            match compare_sql(&aion, &mut sqlite, &sql) {
                Ok(()) => passed += 1,
                Err(error) => failures.push(format!("correlated amt={amt}: {error} (sql: {sql})")),
            }
        }

        if failures.len() >= 120 {
            failures
                .push("... correlated subquery matrix truncated after 120 mismatches".to_owned());
            break;
        }
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

pub fn run_mutation_checkpoint_matrix(iterations: usize) -> SuiteResult {
    let db = TestDb::new();
    let aion = db.conn();
    let mut sqlite = match SqliteConnection::open_in_memory() {
        Ok(conn) => conn,
        Err(error) => return Err(vec![format!("sqlite setup failed: {error}")]),
    };

    let mut passed = 0_usize;
    let mut failures = Vec::new();
    let mut rng = FuzzRng::seeded(0xA10D_4D71_55AA);

    for sql in [
        "CREATE TABLE mut_cp (id INTEGER PRIMARY KEY, x INTEGER NOT NULL, y INTEGER NOT NULL, grp INTEGER NOT NULL)",
        "CREATE INDEX mut_cp_grp_idx ON mut_cp (grp)",
    ] {
        match compare_sql(&aion, &mut sqlite, sql) {
            Ok(()) => passed += 1,
            Err(error) => failures.push(format!("bootstrap mismatch for `{sql}`: {error}")),
        }
    }
    if !failures.is_empty() {
        return Err(failures);
    }

    for id in 1_u64..=300_u64 {
        let x = ((id * 19) % 1100) as i64 - 330;
        let y = ((id * 23) % 1300) as i64 - 410;
        let grp = (id % 15) + 1;
        let sql = format!("INSERT INTO mut_cp VALUES ({id}, {x}, {y}, {grp})");
        match compare_sql(&aion, &mut sqlite, &sql) {
            Ok(()) => passed += 1,
            Err(error) => failures.push(format!("seed mismatch id={id}: {error}")),
        }
    }
    if !failures.is_empty() {
        return Err(failures);
    }

    for i in 0..iterations {
        let action = rng.next_range(0, 100);
        let sql = if action < 42 {
            let id = rng.next_range(1, 301);
            let dx = rng.next_i64(-70, 70);
            let dy = rng.next_i64(-70, 70);
            format!("UPDATE mut_cp SET x = x + ({dx}), y = y + ({dy}) WHERE id = {id}")
        } else if action < 58 {
            let id = rng.next_range(1, 301);
            let nx = rng.next_i64(-1000, 1000);
            let ny = rng.next_i64(-1200, 1200);
            let g = rng.next_range(1, 16);
            format!(
                "INSERT INTO mut_cp (id, x, y, grp) VALUES ({id}, {nx}, {ny}, {g}) \
                 ON CONFLICT (id) DO UPDATE SET x = EXCLUDED.x, y = EXCLUDED.y, grp = EXCLUDED.grp"
            )
        } else if action < 72 {
            let id = rng.next_range(1, 301);
            format!("DELETE FROM mut_cp WHERE id = {id}")
        } else if action < 84 {
            let id = rng.next_range(1, 301);
            let nx = rng.next_i64(-900, 900);
            let ny = rng.next_i64(-900, 900);
            let g = rng.next_range(1, 16);
            format!("INSERT INTO mut_cp VALUES ({id}, {nx}, {ny}, {g})")
        } else {
            let t = rng.next_i64(-400, 600);
            format!(
                "SELECT count(*), coalesce(sum(x), 0), coalesce(sum(y), 0) FROM mut_cp WHERE x >= {t}"
            )
        };

        match compare_sql(&aion, &mut sqlite, &sql) {
            Ok(()) => passed += 1,
            Err(error) => failures.push(format!("mut/checkpoint iter {i}: {error} (sql: {sql})")),
        }

        if i % 20 == 0 {
            let checks = [
                "SELECT count(*), count(DISTINCT id) FROM mut_cp",
                "SELECT grp, count(*), coalesce(sum(x), 0), coalesce(sum(y), 0) FROM mut_cp GROUP BY grp ORDER BY grp",
                "SELECT id, x, y, grp FROM mut_cp ORDER BY id LIMIT 40",
            ];
            for check in checks {
                match compare_sql(&aion, &mut sqlite, check) {
                    Ok(()) => passed += 1,
                    Err(error) => failures.push(format!(
                        "mut/checkpoint iter {i} check mismatch `{check}`: {error}"
                    )),
                }
            }
        }

        if failures.len() >= 120 {
            failures.push(format!(
                "... mut/checkpoint matrix truncated after 120 mismatches at iter {i}"
            ));
            break;
        }
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

pub fn run_sampling_order_stability() -> SuiteResult {
    let db = TestDb::new();
    let aion = db.conn();
    let mut sqlite = match SqliteConnection::open_in_memory() {
        Ok(conn) => conn,
        Err(error) => return Err(vec![format!("sqlite setup failed: {error}")]),
    };

    let mut passed = 0_usize;
    let mut failures = Vec::new();

    for sql in setup_statements() {
        match compare_sql(&aion, &mut sqlite, &sql) {
            Ok(()) => passed += 1,
            Err(error) => failures.push(format!("setup mismatch for `{sql}`: {error}")),
        }
    }
    if !failures.is_empty() {
        return Err(failures);
    }

    for limit in 1_u64..=80_u64 {
        for modv in 2_u64..=12_u64 {
            let q1 = format!(
                "SELECT id, amount FROM diff_orders WHERE (id % {modv}) = 0 ORDER BY amount DESC, id ASC LIMIT {limit}"
            );
            let q2 = format!(
                "SELECT id, amount FROM diff_orders WHERE (id % {modv}) = 1 ORDER BY amount ASC, id DESC LIMIT {limit}"
            );
            let q3 = format!(
                "SELECT user_id, count(*), coalesce(sum(amount), 0) FROM diff_orders WHERE (id % {modv}) <= 1 GROUP BY user_id ORDER BY user_id LIMIT {limit}"
            );
            for sql in [q1, q2, q3] {
                match compare_sql(&aion, &mut sqlite, &sql) {
                    Ok(()) => passed += 1,
                    Err(error) => failures.push(format!(
                        "sampling/order limit={limit} mod={modv}: {error} (sql: {sql})"
                    )),
                }
            }
            if failures.len() >= 120 {
                failures
                    .push("... sampling/order matrix truncated after 120 mismatches".to_owned());
                break;
            }
        }
        if failures.len() >= 120 {
            break;
        }
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

pub fn run_exists_in_notin_matrix() -> SuiteResult {
    let db = TestDb::new();
    let aion = db.conn();
    let mut sqlite = match SqliteConnection::open_in_memory() {
        Ok(conn) => conn,
        Err(error) => return Err(vec![format!("sqlite setup failed: {error}")]),
    };

    let mut passed = 0_usize;
    let mut failures = Vec::new();

    for sql in setup_statements() {
        match compare_sql(&aion, &mut sqlite, &sql) {
            Ok(()) => passed += 1,
            Err(error) => failures.push(format!("setup mismatch for `{sql}`: {error}")),
        }
    }
    if !failures.is_empty() {
        return Err(failures);
    }

    for min_amt in (-120_i64..=320_i64).step_by(20) {
        for min_age in (18_i64..=68_i64).step_by(5) {
            let q1 = format!(
                "SELECT count(*) FROM diff_users u \
                 WHERE EXISTS (SELECT 1 FROM diff_orders o WHERE o.user_id = u.id AND o.amount >= {min_amt})"
            );
            let q2 = format!(
                "SELECT count(*) FROM diff_users WHERE id IN \
                 (SELECT user_id FROM diff_orders WHERE amount >= {min_amt})"
            );
            let q3 = format!(
                "SELECT count(*) FROM diff_users \
                 WHERE id NOT IN (SELECT user_id FROM diff_orders WHERE amount >= {min_amt})"
            );
            let q4 = format!(
                "SELECT city, count(*) FROM diff_users \
                 WHERE age >= {min_age} AND id IN \
                 (SELECT user_id FROM diff_orders WHERE amount >= {min_amt}) \
                 GROUP BY city ORDER BY city"
            );
            for sql in [q1, q2, q3, q4] {
                match compare_sql(&aion, &mut sqlite, &sql) {
                    Ok(()) => passed += 1,
                    Err(error) => failures.push(format!(
                        "exists/in/notin min_amt={min_amt} min_age={min_age}: {error} (sql: {sql})"
                    )),
                }
            }

            if failures.len() >= 120 {
                failures
                    .push("... exists/in/notin matrix truncated after 120 mismatches".to_owned());
                break;
            }
        }
        if failures.len() >= 120 {
            break;
        }
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

pub fn run_union_projection_stress() -> SuiteResult {
    let db = TestDb::new();
    let aion = db.conn();
    let mut sqlite = match SqliteConnection::open_in_memory() {
        Ok(conn) => conn,
        Err(error) => return Err(vec![format!("sqlite setup failed: {error}")]),
    };

    let mut passed = 0_usize;
    let mut failures = Vec::new();

    for sql in setup_statements() {
        match compare_sql(&aion, &mut sqlite, &sql) {
            Ok(()) => passed += 1,
            Err(error) => failures.push(format!("setup mismatch for `{sql}`: {error}")),
        }
    }
    if !failures.is_empty() {
        return Err(failures);
    }

    for low in (-120_i64..=260_i64).step_by(20) {
        let high = low + 120;
        for limit in [5_u64, 10_u64, 20_u64, 40_u64] {
            let q1 = format!(
                "SELECT user_id FROM diff_orders WHERE amount BETWEEN {low} AND {high} \
                 UNION \
                 SELECT id FROM diff_users WHERE age BETWEEN 22 AND 60 \
                 ORDER BY 1 LIMIT {limit}"
            );
            let q2 = format!(
                "SELECT user_id FROM diff_orders WHERE amount BETWEEN {low} AND {high} \
                 EXCEPT \
                 SELECT id FROM diff_users WHERE city IN ('paris', 'lyon') \
                 ORDER BY 1 LIMIT {limit}"
            );
            let q3 = format!(
                "SELECT user_id FROM diff_orders WHERE amount BETWEEN {low} AND {high} \
                 INTERSECT \
                 SELECT id FROM diff_users WHERE age >= 30 \
                 ORDER BY 1 LIMIT {limit}"
            );
            for sql in [q1, q2, q3] {
                match compare_sql(&aion, &mut sqlite, &sql) {
                    Ok(()) => passed += 1,
                    Err(error) => failures.push(format!(
                        "union/projection low={low} high={high} limit={limit}: {error} (sql: {sql})"
                    )),
                }
            }
            if failures.len() >= 120 {
                failures
                    .push("... union/projection stress truncated after 120 mismatches".to_owned());
                break;
            }
        }
        if failures.len() >= 120 {
            break;
        }
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

pub fn run_multi_join_aggregate_matrix() -> SuiteResult {
    let db = TestDb::new();
    let aion = db.conn();
    let mut sqlite = match SqliteConnection::open_in_memory() {
        Ok(conn) => conn,
        Err(error) => return Err(vec![format!("sqlite setup failed: {error}")]),
    };

    let mut passed = 0_usize;
    let mut failures = Vec::new();

    for sql in setup_statements() {
        match compare_sql(&aion, &mut sqlite, &sql) {
            Ok(()) => passed += 1,
            Err(error) => failures.push(format!("setup mismatch for `{sql}`: {error}")),
        }
    }
    if !failures.is_empty() {
        return Err(failures);
    }

    for min_age in (18_i64..=66_i64).step_by(4) {
        for min_amt in (-120_i64..=320_i64).step_by(20) {
            for min_cnt in 1_i64..=6_i64 {
                let q1 = format!(
                    "SELECT u.city, o.status, count(*), coalesce(sum(o.amount), 0) \
                     FROM diff_users u JOIN diff_orders o ON u.id = o.user_id \
                     WHERE u.age >= {min_age} AND o.amount >= {min_amt} \
                     GROUP BY u.city, o.status ORDER BY u.city, o.status"
                );
                let q2 = format!(
                    "SELECT o.user_id, count(*), coalesce(sum(o.amount), 0), coalesce(avg(o.amount), 0) \
                     FROM diff_orders o JOIN diff_users u ON u.id = o.user_id \
                     WHERE u.age >= {min_age} AND o.amount >= {min_amt} \
                     GROUP BY o.user_id HAVING count(*) >= {min_cnt} ORDER BY o.user_id"
                );
                let q3 = format!(
                    "SELECT u.city, count(DISTINCT o.user_id), coalesce(sum(o.amount), 0) \
                     FROM diff_users u JOIN diff_orders o ON u.id = o.user_id \
                     WHERE u.age >= {min_age} AND o.amount >= {min_amt} \
                     GROUP BY u.city HAVING count(*) >= {min_cnt} ORDER BY u.city"
                );

                for sql in [q1, q2, q3] {
                    match compare_sql(&aion, &mut sqlite, &sql) {
                        Ok(()) => passed += 1,
                        Err(error) => failures.push(format!(
                            "multi-join-agg min_age={min_age} min_amt={min_amt} min_cnt={min_cnt}: {error} (sql: {sql})"
                        )),
                    }
                }

                if failures.len() >= 120 {
                    failures.push(
                        "... multi-join aggregate matrix truncated after 120 mismatches".to_owned(),
                    );
                    break;
                }
            }
            if failures.len() >= 120 {
                break;
            }
        }
        if failures.len() >= 120 {
            break;
        }
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

pub fn run_transaction_script_fuzz(iterations: usize) -> SuiteResult {
    let db = TestDb::new();
    let aion = db.conn();
    let mut sqlite = match SqliteConnection::open_in_memory() {
        Ok(conn) => conn,
        Err(error) => return Err(vec![format!("sqlite setup failed: {error}")]),
    };

    let mut passed = 0_usize;
    let mut failures = Vec::new();
    let mut rng = FuzzRng::seeded(0xA10D_7A97_2026);

    for sql in [
        "CREATE TABLE tx_fuzz (id INTEGER PRIMARY KEY, bal INTEGER NOT NULL, bucket INTEGER NOT NULL)",
        "CREATE INDEX tx_fuzz_bucket_idx ON tx_fuzz (bucket)",
    ] {
        match compare_sql(&aion, &mut sqlite, sql) {
            Ok(()) => passed += 1,
            Err(error) => failures.push(format!("bootstrap mismatch for `{sql}`: {error}")),
        }
    }
    if !failures.is_empty() {
        return Err(failures);
    }

    for id in 1_u64..=100_u64 {
        let bal = ((id * 23) % 500) as i64 - 120;
        let bucket = (id % 10) + 1;
        let sql = format!("INSERT INTO tx_fuzz VALUES ({id}, {bal}, {bucket})");
        match compare_sql(&aion, &mut sqlite, &sql) {
            Ok(()) => passed += 1,
            Err(error) => failures.push(format!("seed mismatch id={id}: {error}")),
        }
    }
    if !failures.is_empty() {
        return Err(failures);
    }

    for i in 0..iterations {
        let action = rng.next_range(0, 100);
        let sql = if action < 45 {
            let from = rng.next_range(1, 101);
            let mut to = rng.next_range(1, 101);
            if to == from {
                to = if to == 100 { 1 } else { to + 1 };
            }
            let amount = rng.next_i64(1, 70);
            format!(
                "BEGIN; \
                 UPDATE tx_fuzz SET bal = bal - ({amount}) WHERE id = {from}; \
                 UPDATE tx_fuzz SET bal = bal + ({amount}) WHERE id = {to}; \
                 COMMIT;"
            )
        } else if action < 70 {
            let from = rng.next_range(1, 101);
            let mut to = rng.next_range(1, 101);
            if to == from {
                to = if to == 100 { 1 } else { to + 1 };
            }
            let amount = rng.next_i64(1, 90);
            format!(
                "BEGIN; \
                 UPDATE tx_fuzz SET bal = bal - ({amount}) WHERE id = {from}; \
                 UPDATE tx_fuzz SET bal = bal + ({amount}) WHERE id = {to}; \
                 ROLLBACK;"
            )
        } else if action < 82 {
            let id = rng.next_range(1, 101);
            let bal = rng.next_i64(-400, 400);
            let bucket = rng.next_range(1, 11);
            format!(
                "INSERT INTO tx_fuzz (id, bal, bucket) VALUES ({id}, {bal}, {bucket}) \
                 ON CONFLICT (id) DO UPDATE SET bal = EXCLUDED.bal, bucket = EXCLUDED.bucket"
            )
        } else if action < 92 {
            let id = rng.next_range(1, 101);
            let bal = rng.next_i64(-600, 600);
            let bucket = rng.next_range(1, 11);
            format!(
                "BEGIN; \
                 DELETE FROM tx_fuzz WHERE id = {id}; \
                 INSERT INTO tx_fuzz (id, bal, bucket) VALUES ({id}, {bal}, {bucket}); \
                 COMMIT;"
            )
        } else {
            let bucket = rng.next_range(1, 11);
            format!(
                "SELECT count(*), coalesce(sum(bal), 0), coalesce(avg(bal), 0) \
                 FROM tx_fuzz WHERE bucket = {bucket}"
            )
        };

        match compare_sql(&aion, &mut sqlite, &sql) {
            Ok(()) => passed += 1,
            Err(error) => failures.push(format!("tx-script-fuzz iter {i}: {error} (sql: {sql})")),
        }

        if i % 17 == 0 {
            for check in [
                "SELECT count(*), count(DISTINCT id), coalesce(sum(bal), 0) FROM tx_fuzz",
                "SELECT bucket, count(*), coalesce(sum(bal), 0) FROM tx_fuzz GROUP BY bucket ORDER BY bucket",
                "SELECT id, bal, bucket FROM tx_fuzz ORDER BY id LIMIT 35",
            ] {
                match compare_sql(&aion, &mut sqlite, check) {
                    Ok(()) => passed += 1,
                    Err(error) => failures.push(format!("tx-script-fuzz iter {i} check mismatch `{check}`: {error}")),
                }
            }
        }

        if failures.len() >= 120 {
            failures.push(format!(
                "... tx-script-fuzz truncated after 120 mismatches at iter {i}"
            ));
            break;
        }
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

fn setup_statements() -> Vec<String> {
    let mut statements = vec![
        "CREATE TABLE diff_users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER, city TEXT)"
            .to_owned(),
        "CREATE TABLE diff_orders (id INTEGER PRIMARY KEY, user_id INTEGER, amount INTEGER, status TEXT)"
            .to_owned(),
        "CREATE INDEX diff_orders_user_idx ON diff_orders (user_id)".to_owned(),
        "CREATE INDEX diff_orders_amount_idx ON diff_orders (amount)".to_owned(),
    ];

    const CITIES: &[&str] = &["paris", "lyon", "lille", "nice", "nantes", "rennes"];
    for id in 1..=40_u32 {
        let age = 18 + ((id * 7) % 51);
        let city = CITIES[((id - 1) as usize) % CITIES.len()];
        let sql = format!(
            "INSERT INTO diff_users (id, name, age, city) VALUES ({id}, 'u{id}', {age}, '{city}')"
        );
        statements.push(sql);
    }

    const STATUS: &[&str] = &["new", "paid", "shipped", "cancelled"];
    for id in 1..=420_u32 {
        let user_id = ((id - 1) % 40) + 1;
        let amount = ((id as i64 * 37_i64) % 500_i64) - 120_i64;
        let status = STATUS[((id - 1) as usize) % STATUS.len()];
        let sql = format!(
            "INSERT INTO diff_orders (id, user_id, amount, status) VALUES ({id}, {user_id}, {amount}, '{status}')"
        );
        statements.push(sql);
    }

    statements
}

fn build_query_corpus() -> Vec<(String, String)> {
    let mut cases: Vec<(String, String)> = vec![
        (
            "row_count_users".to_owned(),
            "SELECT count(*) FROM diff_users".to_owned(),
        ),
        (
            "row_count_orders".to_owned(),
            "SELECT count(*) FROM diff_orders".to_owned(),
        ),
        (
            "join_count".to_owned(),
            "SELECT count(*) FROM diff_users u JOIN diff_orders o ON u.id = o.user_id".to_owned(),
        ),
        (
            "join_filter".to_owned(),
            "SELECT count(*) FROM diff_users u JOIN diff_orders o ON u.id = o.user_id WHERE o.amount > 250"
                .to_owned(),
        ),
        (
            "city_group".to_owned(),
            "SELECT city, count(*) FROM diff_users GROUP BY city ORDER BY city".to_owned(),
        ),
        (
            "status_group".to_owned(),
            "SELECT status, count(*), coalesce(sum(amount), 0) FROM diff_orders GROUP BY status ORDER BY status"
                .to_owned(),
        ),
        (
            "max_amount_user".to_owned(),
            "SELECT user_id, max(amount) FROM diff_orders GROUP BY user_id ORDER BY user_id LIMIT 12"
                .to_owned(),
        ),
        (
            "subquery_in".to_owned(),
            "SELECT count(*) FROM diff_users WHERE id IN (SELECT user_id FROM diff_orders WHERE amount > 300)"
                .to_owned(),
        ),
        (
            "subquery_exists".to_owned(),
            "SELECT count(*) FROM diff_users u WHERE EXISTS (SELECT 1 FROM diff_orders o WHERE o.user_id = u.id AND o.amount < -60)"
                .to_owned(),
        ),
        (
            "distinct_city".to_owned(),
            "SELECT distinct city FROM diff_users ORDER BY city".to_owned(),
        ),
        (
            "order_by_limit".to_owned(),
            "SELECT id, amount FROM diff_orders ORDER BY amount DESC, id ASC LIMIT 25".to_owned(),
        ),
        (
            "nullif_coalesce".to_owned(),
            "SELECT count(*), coalesce(sum(nullif(amount, 0)), 0) FROM diff_orders".to_owned(),
        ),
        (
            "case_bucket".to_owned(),
            "SELECT CASE WHEN amount < 0 THEN 'neg' WHEN amount = 0 THEN 'zero' ELSE 'pos' END AS bucket, count(*) \
             FROM diff_orders GROUP BY bucket ORDER BY bucket"
                .to_owned(),
        ),
    ];

    for age in (18..=68).step_by(2) {
        cases.push((
            format!("age_filter_{age}"),
            format!("SELECT count(*) FROM diff_users WHERE age >= {age}"),
        ));
    }

    for uid in 1..=40 {
        cases.push((
            format!("sum_by_user_{uid:02}"),
            format!("SELECT coalesce(sum(amount), 0) FROM diff_orders WHERE user_id = {uid}"),
        ));
    }

    for threshold in (-100..=300).step_by(20) {
        cases.push((
            format!("threshold_{threshold}"),
            format!("SELECT count(*) FROM diff_orders WHERE amount >= {threshold}"),
        ));
    }

    for limit in 1..=25 {
        cases.push((
            format!("top_limit_{limit:02}"),
            format!(
                "SELECT id, amount FROM diff_orders ORDER BY amount DESC, id ASC LIMIT {limit}"
            ),
        ));
    }

    for low_id in (1..=36).step_by(4) {
        let high_id = low_id + 5;
        cases.push((
            format!("window_between_{low_id:02}_{high_id:02}"),
            format!(
                "SELECT id, amount FROM diff_orders WHERE id BETWEEN {low_id} AND {high_id} ORDER BY id ASC"
            ),
        ));
    }

    for n in -40..=40 {
        cases.push((
            format!("arith_{n:03}"),
            format!("SELECT ({n}) + ({n} * 2), ({n}) * ({n}) - 3"),
        ));
    }

    cases
}

fn compare_sql(
    aion: &Connection<Engine>,
    sqlite: &mut SqliteConnection,
    sql: &str,
) -> Result<(), String> {
    let aion_outcome = run_aion(aion, sql);
    let sqlite_outcome = run_sqlite(sqlite, sql);
    match (aion_outcome, sqlite_outcome) {
        (Outcome::Command, Outcome::Command) => Ok(()),
        (Outcome::Query(lhs), Outcome::Query(rhs)) => {
            if rows_equivalent(&lhs, &rhs) {
                Ok(())
            } else {
                Err(format!(
                    "query rows differ\n  aiondb={}\n  sqlite={}",
                    truncate(&format!("{lhs:?}"), 240),
                    truncate(&format!("{rhs:?}"), 240)
                ))
            }
        }
        (Outcome::Error(lhs), Outcome::Error(rhs)) => {
            let left_class = classify_error(&lhs);
            let right_class = classify_error(&rhs);
            if left_class == right_class {
                Ok(())
            } else {
                Err(format!(
                    "error class mismatch: aiondb={left_class} ({}) vs sqlite={right_class} ({})",
                    truncate(&lhs, 140),
                    truncate(&rhs, 140)
                ))
            }
        }
        (lhs, rhs) => Err(format!("outcome mismatch: aiondb={lhs:?}, sqlite={rhs:?}")),
    }
}

fn rows_equivalent(left: &[Vec<String>], right: &[Vec<String>]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    for (lrow, rrow) in left.iter().zip(right.iter()) {
        if lrow.len() != rrow.len() {
            return false;
        }
        for (lcell, rcell) in lrow.iter().zip(rrow.iter()) {
            if !cells_equivalent(lcell, rcell) {
                return false;
            }
        }
    }
    true
}

fn cells_equivalent(left: &str, right: &str) -> bool {
    if left == right {
        return true;
    }
    if let (Ok(li), Ok(ri)) = (left.parse::<i64>(), right.parse::<i64>()) {
        return li == ri;
    }
    if let (Ok(lf), Ok(rf)) = (left.parse::<f64>(), right.parse::<f64>()) {
        if !lf.is_finite() || !rf.is_finite() {
            return lf == rf;
        }
        let abs = (lf - rf).abs();
        if abs <= 1e-6 {
            return true;
        }
        let denom = lf.abs().max(rf.abs()).max(1.0);
        return (abs / denom) <= 1e-6;
    }
    false
}

fn run_aion(conn: &Connection<Engine>, sql: &str) -> Outcome {
    match conn.execute(sql) {
        Ok(results) => {
            let mut query_rows: Vec<Vec<String>> = Vec::new();
            let mut saw_query = false;
            for result in &results {
                if let StatementResult::Query { rows, .. } = result {
                    saw_query = true;
                    for row in rows {
                        let rendered: Vec<String> =
                            row.values.iter().map(format_aion_value).collect();
                        query_rows.push(rendered);
                    }
                }
            }
            if saw_query {
                Outcome::Query(query_rows)
            } else {
                Outcome::Command
            }
        }
        Err(error) => Outcome::Error(format!("{error}")),
    }
}

fn run_sqlite(conn: &mut SqliteConnection, sql: &str) -> Outcome {
    if is_query_statement(sql) {
        let mut statement = match conn.prepare(sql) {
            Ok(statement) => statement,
            Err(error) => return Outcome::Error(error.to_string()),
        };
        let col_count = statement.column_count();
        let mut stream = match statement.query([]) {
            Ok(stream) => stream,
            Err(error) => return Outcome::Error(error.to_string()),
        };

        let mut out = Vec::new();
        loop {
            match stream.next() {
                Ok(Some(row)) => {
                    let mut values = Vec::with_capacity(col_count);
                    for idx in 0..col_count {
                        let value: SqliteValue = match row.get(idx) {
                            Ok(value) => value,
                            Err(error) => return Outcome::Error(error.to_string()),
                        };
                        values.push(format_sqlite_value(value));
                    }
                    out.push(values);
                }
                Ok(None) => break,
                Err(error) => return Outcome::Error(error.to_string()),
            }
        }
        Outcome::Query(out)
    } else {
        match conn.execute_batch(sql) {
            Ok(()) => Outcome::Command,
            Err(error) => Outcome::Error(error.to_string()),
        }
    }
}

fn format_aion_value(value: &Value) -> String {
    normalize_scalar(crate::harness::engine::value_to_string(value))
}

fn format_sqlite_value(value: SqliteValue) -> String {
    match value {
        SqliteValue::Null => "NULL".to_owned(),
        SqliteValue::Integer(v) => v.to_string(),
        SqliteValue::Real(v) => normalize_number(v),
        SqliteValue::Text(v) => normalize_scalar(v),
        SqliteValue::Blob(bytes) => {
            let mut out = String::from("X'");
            for byte in bytes {
                out.push_str(&format!("{byte:02X}"));
            }
            out.push('\'');
            out
        }
    }
}

fn normalize_scalar(value: String) -> String {
    if value.eq_ignore_ascii_case("null") {
        return "NULL".to_owned();
    }
    if value.eq_ignore_ascii_case("t") || value.eq_ignore_ascii_case("true") {
        return "1".to_owned();
    }
    if value.eq_ignore_ascii_case("f") || value.eq_ignore_ascii_case("false") {
        return "0".to_owned();
    }
    if let Ok(int_value) = value.parse::<i64>() {
        return int_value.to_string();
    }
    if let Ok(float_value) = value.parse::<f64>() {
        return normalize_number(float_value);
    }
    value
}

fn normalize_number(value: f64) -> String {
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 1e18 {
        format!("{int_part}", int_part = value as i64)
    } else {
        format!("{value}")
    }
}

fn classify_error(message: &str) -> &'static str {
    let text = message.to_ascii_lowercase();
    if text.contains("syntax") || text.contains("parse") {
        "syntax"
    } else if text.contains("unique")
        || text.contains("primary key")
        || text.contains("constraint")
        || text.contains("duplicate")
    {
        "constraint"
    } else if text.contains("no such table")
        || text.contains("no such column")
        || text.contains("does not exist")
        || text.contains("unknown table")
        || text.contains("unknown column")
    {
        "missing_object"
    } else if text.contains("datatype")
        || text.contains("type mismatch")
        || text.contains("cannot cast")
        || text.contains("invalid input syntax")
    {
        "type"
    } else if text.contains("division by zero") {
        "division_by_zero"
    } else if text.contains("overflow") {
        "overflow"
    } else {
        "other"
    }
}

fn is_query_statement(sql: &str) -> bool {
    let trimmed = sql.trim_start();
    let Some(keyword) = leading_ascii_keyword(trimmed) else {
        return false;
    };
    keyword.eq_ignore_ascii_case("select")
        || keyword.eq_ignore_ascii_case("values")
        || keyword.eq_ignore_ascii_case("with")
        || keyword.eq_ignore_ascii_case("pragma")
        || keyword.eq_ignore_ascii_case("explain")
}

fn leading_ascii_keyword(sql: &str) -> Option<&str> {
    let bytes = sql.as_bytes();
    if bytes.is_empty() || !bytes[0].is_ascii_alphabetic() {
        return None;
    }
    let mut end = 1_usize;
    while end < bytes.len() && bytes[end].is_ascii_alphabetic() {
        end += 1;
    }
    Some(&sql[..end])
}

fn truncate(text: &str, max_len: usize) -> String {
    if text.len() <= max_len {
        text.to_owned()
    } else {
        format!("{}...", &text[..max_len])
    }
}

struct FuzzRng {
    state: u64,
}

impl FuzzRng {
    fn seeded(seed: u64) -> Self {
        Self {
            state: if seed == 0 { 1 } else { seed },
        }
    }

    fn next_u64(&mut self) -> u64 {
        self.state ^= self.state << 13;
        self.state ^= self.state >> 7;
        self.state ^= self.state << 17;
        self.state
    }

    fn next_range(&mut self, lo: u64, hi: u64) -> u64 {
        if lo >= hi {
            lo
        } else {
            lo + (self.next_u64() % (hi - lo))
        }
    }

    fn next_i64(&mut self, lo: i64, hi: i64) -> i64 {
        if lo >= hi {
            lo
        } else {
            let span = (hi - lo) as u64;
            lo + (self.next_u64() % span) as i64
        }
    }
}
