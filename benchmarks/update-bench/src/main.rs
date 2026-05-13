//! UPDATE access-path microbench.
//!
//! Spins an in-memory AionDB, populates a table with N rows + indexes,
//! then runs each UPDATE shape repeatedly with timing. Reports per-row
//! cost so the access-path A/B comparisons are obvious. The
//! `_indexed` and `_seq` rows in each pair operate on twinned columns
//! (`id`/`nx`, `a`/`na`, `b`/`nb`) carrying identical values; only
//! one set is covered by an index. The delta is therefore the pure
//! index-access-path-vs-seq-scan win.

use std::env;
use std::time::Instant;

use anyhow::{Context, Result};
use aiondb_embedded::{Database, StatementResult};

fn run_sql(conn: &aiondb_embedded::Connection<aiondb_embedded::Engine>, sql: &str) -> Result<()> {
    conn.execute(sql).with_context(|| format!("execute: {sql}"))?;
    Ok(())
}

fn count_rows(
    conn: &aiondb_embedded::Connection<aiondb_embedded::Engine>,
    sql: &str,
) -> Result<u64> {
    let mut total = 0u64;
    for r in conn.execute(sql)? {
        if let StatementResult::Command { rows_affected, .. } = r {
            total += rows_affected;
        }
    }
    Ok(total)
}

fn time_update(
    label: &str,
    conn: &aiondb_embedded::Connection<aiondb_embedded::Engine>,
    sql: &str,
    iters: usize,
) -> Result<f64> {
    let _ = conn.execute(sql)?;
    let start = Instant::now();
    let mut total_rows = 0u64;
    for _ in 0..iters {
        total_rows += count_rows(conn, sql)?;
    }
    let elapsed = start.elapsed();
    let us_per_row = elapsed.as_micros() as f64 / total_rows.max(1) as f64;
    println!(
        "{label:<22} iters={iters:>4} rows={total_rows:>9} elapsed={:>8.3}ms  {:>12.0} rows/s  {:>8.3} us/row",
        elapsed.as_secs_f64() * 1000.0,
        total_rows as f64 / elapsed.as_secs_f64(),
        us_per_row,
    );
    Ok(us_per_row)
}

fn populate(
    conn: &aiondb_embedded::Connection<aiondb_embedded::Engine>,
    rows: usize,
) -> Result<()> {
    run_sql(conn, "DROP TABLE IF EXISTS t;")?;
    run_sql(conn, "DROP TABLE IF EXISTS t_with_check;")?;
    // Twin columns: indexed (id, a, b) and non-indexed (nx, na, nb)
    // mirror each other so we can compare the same predicate shape on
    // an indexed vs non-indexed column.
    run_sql(
        conn,
        "CREATE TABLE t (id INT PRIMARY KEY, nx INT, a INT, b INT, na INT, nb INT, v INT);",
    )?;
    run_sql(conn, "CREATE INDEX t_a_b ON t(a, b);")?;
    let mut sql = String::with_capacity(rows * 32);
    sql.push_str("INSERT INTO t VALUES ");
    for i in 0..rows {
        if i > 0 {
            sql.push_str(", ");
        }
        let a = i % 100;
        let b = (i / 100) % 100;
        sql.push_str(&format!("({i}, {i}, {a}, {b}, {a}, {b}, 0)"));
    }
    sql.push(';');
    run_sql(conn, &sql)?;

    // Same shape but with a CHECK constraint touching only `c_checked`,
    // so an UPDATE on `v` should skip the per-row CHECK evaluation.
    run_sql(
        conn,
        "CREATE TABLE t_with_check (id INT PRIMARY KEY, c_checked INT CHECK (c_checked >= 0), v INT);",
    )?;
    let mut sql = String::with_capacity(rows * 24);
    sql.push_str("INSERT INTO t_with_check VALUES ");
    for i in 0..rows {
        if i > 0 {
            sql.push_str(", ");
        }
        sql.push_str(&format!("({i}, {i}, 0)"));
    }
    sql.push(';');
    run_sql(conn, &sql)?;
    Ok(())
}

fn pair(
    label_idx: &str,
    sql_idx: &str,
    label_seq: &str,
    sql_seq: &str,
    conn: &aiondb_embedded::Connection<aiondb_embedded::Engine>,
    iters: usize,
    seq_iters: usize,
) -> Result<()> {
    let idx_us = time_update(label_idx, conn, sql_idx, iters)?;
    let seq_us = time_update(label_seq, conn, sql_seq, seq_iters)?;
    let speedup = if idx_us > 0.0 { seq_us / idx_us } else { 0.0 };
    println!(
        "                       => speedup {speedup:>5.2}x  ({label_seq} / {label_idx})\n"
    );
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    let rows: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(20_000);
    let iters: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(30);
    let seq_iters = (iters / 5).max(2);

    println!("# AionDB UPDATE microbench");
    println!("# rows={rows}  iters={iters}  seq_iters={seq_iters}");

    let db = Database::in_memory()?;
    let conn = db.connect_anonymous("default", "app")?;
    populate(&conn, rows)?;

    println!("\n## eq fast path (existing)");
    let _ = time_update(
        "eq_pk_single",
        &conn,
        "UPDATE t SET v = v + 1 WHERE id = 12345;",
        iters,
    )?;

    let mut in_list_sql = String::from("UPDATE t SET v = v + 1 WHERE id IN (");
    for i in 0..32 {
        if i > 0 {
            in_list_sql.push_str(", ");
        }
        in_list_sql.push_str(&(100 + i * 7).to_string());
    }
    in_list_sql.push_str(");");
    let _ = time_update("in_list_32_pk", &conn, &in_list_sql, iters)?;

    println!("\n## range access path A/B (new)");
    pair(
        "range_id_500",
        "UPDATE t SET v = v + 1 WHERE id >= 7000 AND id <= 7499;",
        "range_nx_seq_500",
        "UPDATE t SET v = v + 1 WHERE nx >= 7000 AND nx <= 7499;",
        &conn,
        iters,
        seq_iters,
    )?;

    pair(
        "between_id_500",
        "UPDATE t SET v = v + 1 WHERE id BETWEEN 12000 AND 12499;",
        "between_nx_seq_500",
        "UPDATE t SET v = v + 1 WHERE nx BETWEEN 12000 AND 12499;",
        &conn,
        iters,
        seq_iters,
    )?;

    pair(
        "single_lt_id",
        "UPDATE t SET v = v + 1 WHERE id < 500;",
        "single_lt_nx_seq",
        "UPDATE t SET v = v + 1 WHERE nx < 500;",
        &conn,
        iters,
        seq_iters,
    )?;

    println!("\n## no-op write skip A/B (custom AionDB)");
    // `SET v = v` is a no-op: new value equals old value. The custom
    // skip should bypass the storage write, the FK action chain, and
    // the index-update preflight. Compare against `SET v = v + 1`
    // which actually changes the column.
    pair(
        "noop_write_skip",
        "UPDATE t SET v = v WHERE id BETWEEN 14000 AND 14499;",
        "real_write_baseline",
        "UPDATE t SET v = v + 1 WHERE id BETWEEN 14000 AND 14499;",
        &conn,
        iters,
        iters,
    )?;

    println!("\n## OR-of-eq → IN access path A/B (new)");
    pair(
        "or_eq_id_indexed",
        "UPDATE t SET v = v + 1 WHERE id = 100 OR id = 200 OR id = 300 OR id = 400 OR id = 500;",
        "or_eq_nx_seq",
        "UPDATE t SET v = v + 1 WHERE nx = 100 OR nx = 200 OR nx = 300 OR nx = 400 OR nx = 500;",
        &conn,
        iters,
        seq_iters,
    )?;

    println!("\n## CHECK skip A/B (PG attno bitmap parity)");
    // UPDATE that touches `v` only - CHECK on `c_checked` does not
    // need re-evaluation. Compare against UPDATE that touches the
    // checked column.
    pair(
        "check_skip_v_only",
        "UPDATE t_with_check SET v = v + 1 WHERE id BETWEEN 6000 AND 6499;",
        "check_eval_c_checked",
        "UPDATE t_with_check SET c_checked = c_checked + 1 WHERE id BETWEEN 6000 AND 6499;",
        &conn,
        iters,
        iters,
    )?;

    println!("\n## UPDATE … FROM hash join A/B (new)");
    // Build a `src` table the FROM side joins against on an equi-join
    // key. The hash-join path detects `t.id = src.target_id` and
    // collapses the per-target cross-product walk to an O(1) bucket
    // lookup; the cross-join baseline below uses a non-equi-join
    // predicate (`t.v < src.payload`) to force the legacy
    // `for_each_from_combination` cross product, then reports the
    // delta. Cross-join is O(N × M); on the small N=300 / M=200
    // workload the bench keeps it tractable so we can compute a
    // ratio.
    run_sql(&conn, "DROP TABLE IF EXISTS src;")?;
    run_sql(&conn, "CREATE TABLE src (target_id INT, payload INT);")?;
    let mut sql = String::with_capacity(2000 * 24);
    sql.push_str("INSERT INTO src VALUES ");
    for i in 0..2_000 {
        if i > 0 {
            sql.push_str(", ");
        }
        sql.push_str(&format!("({}, {})", i * 10, i + 100));
    }
    sql.push(';');
    run_sql(&conn, &sql)?;
    let _ = time_update(
        "update_from_hashjoin",
        &conn,
        "UPDATE t SET v = src.payload FROM src WHERE t.id = src.target_id;",
        (iters / 5).max(2),
    )?;

    // Smaller workload to bound the cross-join baseline runtime.
    run_sql(&conn, "DROP TABLE IF EXISTS small_t;")?;
    run_sql(&conn, "DROP TABLE IF EXISTS small_src;")?;
    run_sql(
        &conn,
        "CREATE TABLE small_t (id INT PRIMARY KEY, v INT);",
    )?;
    run_sql(
        &conn,
        "CREATE TABLE small_src (target_id INT, payload INT);",
    )?;
    let mut sql = String::with_capacity(300 * 16);
    sql.push_str("INSERT INTO small_t VALUES ");
    for i in 0..300 {
        if i > 0 {
            sql.push_str(", ");
        }
        sql.push_str(&format!("({i}, 0)"));
    }
    sql.push(';');
    run_sql(&conn, &sql)?;
    let mut sql = String::with_capacity(200 * 16);
    sql.push_str("INSERT INTO small_src VALUES ");
    for i in 0..200 {
        if i > 0 {
            sql.push_str(", ");
        }
        sql.push_str(&format!("({i}, {})", i + 1000));
    }
    sql.push(';');
    run_sql(&conn, &sql)?;
    pair(
        "small_hashjoin_eq",
        "UPDATE small_t SET v = small_src.payload FROM small_src WHERE small_t.id = small_src.target_id;",
        "small_crossjoin_baseline",
        // No equi-join clause => hash-join detection fails => legacy
        // cross-join path. The semantic effect is "first src row with
        // higher payload wins"; row count differs but the per-target
        // work is the cross-product, which is what we measure.
        "UPDATE small_t SET v = small_src.payload FROM small_src WHERE small_t.v < small_src.payload;",
        &conn,
        (iters / 5).max(2),
        (iters / 5).max(2),
    )?;

    println!("\n## composite-eq access path A/B (new)");
    // The (a, b) index covers the indexed shape; (na, nb) have no
    // index so the same predicate falls through to the seq scan.
    pair(
        "comp_a_b_indexed",
        "UPDATE t SET v = v + 1 WHERE a = 5 AND b = 5;",
        "comp_na_nb_seq",
        "UPDATE t SET v = v + 1 WHERE na = 5 AND nb = 5;",
        &conn,
        iters,
        seq_iters,
    )?;

    Ok(())
}
