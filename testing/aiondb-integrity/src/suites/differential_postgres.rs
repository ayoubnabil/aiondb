use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use aiondb_embedded::Connection;
use aiondb_engine::Engine;

use crate::harness::{SuiteResult, SuiteStats, TestDb};

pub fn run_optional() -> SuiteResult {
    let Some(dsn) = std::env::var("AIONDB_INTEGRITY_POSTGRES_DSN").ok() else {
        return Ok(SuiteStats {
            passed: 0,
            skipped: 1,
        });
    };

    let psql_bin = std::env::var("AIONDB_INTEGRITY_PSQL_BIN").unwrap_or_else(|_| "psql".to_owned());
    if !psql_available(&psql_bin) {
        return Ok(SuiteStats {
            passed: 0,
            skipped: 1,
        });
    }

    let suffix = unique_suffix();
    let table = format!("integrity_pg_cmp_{suffix}");
    let db = TestDb::new();
    let aion = db.conn();

    let mut passed = 0_usize;
    let mut failures = Vec::new();

    let setup = vec![
        format!("DROP TABLE IF EXISTS {table}"),
        format!(
            "CREATE TABLE {table} (id INTEGER PRIMARY KEY, grp INTEGER, amount INTEGER, tag TEXT)"
        ),
    ];
    for sql in &setup {
        if let Err(error) = exec_both_command(&aion, &psql_bin, &dsn, sql) {
            failures.push(format!("setup `{sql}` failed: {error}"));
        } else {
            passed += 1;
        }
    }

    if failures.is_empty() {
        for id in 1..=120_u32 {
            let grp = (id % 8) + 1;
            let amount = ((id as i64 * 19_i64) % 300_i64) - 80_i64;
            let tag = format!("g{grp}");
            let sql = format!(
                "INSERT INTO {table} (id, grp, amount, tag) VALUES ({id}, {grp}, {amount}, '{tag}')"
            );
            if let Err(error) = exec_both_command(&aion, &psql_bin, &dsn, &sql) {
                failures.push(format!("insert row {id} failed: {error}"));
                break;
            }
            passed += 1;
        }
    }

    if failures.is_empty() {
        let queries = vec![
            format!("SELECT count(*) FROM {table}"),
            format!("SELECT grp, count(*), coalesce(sum(amount), 0) FROM {table} GROUP BY grp ORDER BY grp"),
            format!("SELECT count(*) FROM {table} WHERE amount > 150"),
            format!("SELECT count(*) FROM {table} WHERE amount < 0"),
            format!("SELECT id, amount FROM {table} ORDER BY amount DESC, id ASC LIMIT 15"),
            format!("SELECT grp, max(amount), min(amount) FROM {table} GROUP BY grp ORDER BY grp"),
            format!("SELECT count(*) FROM {table} t WHERE EXISTS (SELECT 1 FROM {table} x WHERE x.grp = t.grp AND x.amount > t.amount)"),
            format!("SELECT count(DISTINCT grp) FROM {table}"),
            format!("SELECT coalesce(sum(amount), 0) FROM {table} WHERE grp = 3"),
            format!("SELECT grp, avg(amount) FROM {table} GROUP BY grp ORDER BY grp"),
        ];

        for sql in &queries {
            match compare_query(&aion, &psql_bin, &dsn, sql) {
                Ok(()) => passed += 1,
                Err(error) => failures.push(format!("query `{sql}` mismatch: {error}")),
            }
        }
    }

    if failures.is_empty() {
        for threshold in (-80_i64..=220_i64).step_by(20) {
            let matrix = [
                format!("SELECT count(*) FROM {table} WHERE amount >= {threshold}"),
                format!("SELECT count(*) FROM {table} WHERE amount < {threshold}"),
                format!(
                    "SELECT grp, count(*), coalesce(sum(amount), 0) \
                     FROM {table} WHERE amount >= {threshold} GROUP BY grp ORDER BY grp"
                ),
                format!(
                    "SELECT id, amount FROM {table} \
                     WHERE amount BETWEEN {threshold} AND ({threshold} + 90) \
                     ORDER BY amount DESC, id ASC LIMIT 20"
                ),
            ];
            for sql in &matrix {
                match compare_query(&aion, &psql_bin, &dsn, sql) {
                    Ok(()) => passed += 1,
                    Err(error) => failures.push(format!(
                        "threshold matrix threshold={threshold} query `{sql}` mismatch: {error}"
                    )),
                }
            }
            if failures.len() >= 80 {
                failures
                    .push("... postgres threshold matrix truncated after 80 mismatches".to_owned());
                break;
            }
        }
    }

    if failures.is_empty() {
        for grp in 1_u32..=8_u32 {
            let per_group_queries = [
                format!("SELECT count(*), coalesce(sum(amount), 0), coalesce(avg(amount), 0) FROM {table} WHERE grp = {grp}"),
                format!("SELECT id, amount FROM {table} WHERE grp = {grp} ORDER BY id LIMIT 25"),
                format!(
                    "SELECT count(*) FROM {table} t \
                     WHERE t.grp = {grp} AND EXISTS \
                     (SELECT 1 FROM {table} x WHERE x.grp = t.grp AND x.amount > t.amount)"
                ),
            ];
            for sql in &per_group_queries {
                match compare_query(&aion, &psql_bin, &dsn, sql) {
                    Ok(()) => passed += 1,
                    Err(error) => failures.push(format!(
                        "group matrix grp={grp} query `{sql}` mismatch: {error}"
                    )),
                }
            }
            if failures.len() >= 80 {
                failures.push("... postgres group matrix truncated after 80 mismatches".to_owned());
                break;
            }
        }
    }

    if failures.is_empty() {
        for limit in 1_u32..=30_u32 {
            let sample_queries = [
                format!("SELECT id, grp, amount FROM {table} ORDER BY amount DESC, id ASC LIMIT {limit}"),
                format!("SELECT id, grp, amount FROM {table} ORDER BY amount ASC, id DESC LIMIT {limit}"),
            ];
            for sql in &sample_queries {
                match compare_query(&aion, &psql_bin, &dsn, sql) {
                    Ok(()) => passed += 1,
                    Err(error) => failures.push(format!(
                        "limit matrix limit={limit} query `{sql}` mismatch: {error}"
                    )),
                }
            }
            if failures.len() >= 80 {
                failures.push("... postgres limit matrix truncated after 80 mismatches".to_owned());
                break;
            }
        }
    }

    if failures.is_empty() {
        for step in 0_u32..220_u32 {
            let id = (step % 120) + 1;
            let grp = ((step * 5) % 8) + 1;
            let amount = ((step as i64 * 37_i64) % 420_i64) - 120_i64;
            let mutate_sql = if step % 6 == 0 {
                format!("DELETE FROM {table} WHERE id = {id}")
            } else {
                format!(
                    "INSERT INTO {table} (id, grp, amount, tag) VALUES ({id}, {grp}, {amount}, 'm{grp}') \
                     ON CONFLICT (id) DO UPDATE SET grp = EXCLUDED.grp, amount = EXCLUDED.amount, tag = EXCLUDED.tag"
                )
            };
            if let Err(error) = exec_both_command(&aion, &psql_bin, &dsn, &mutate_sql) {
                failures.push(format!(
                    "postgres mutation step={step} sql `{mutate_sql}` failed: {error}"
                ));
                break;
            }
            passed += 1;

            if step % 15 == 0 {
                let checks = [
                    format!("SELECT count(*), count(DISTINCT id), coalesce(sum(amount), 0) FROM {table}"),
                    format!("SELECT grp, count(*), coalesce(sum(amount), 0) FROM {table} GROUP BY grp ORDER BY grp"),
                    format!("SELECT id, grp, amount FROM {table} ORDER BY id LIMIT 30"),
                ];
                for sql in &checks {
                    match compare_query(&aion, &psql_bin, &dsn, sql) {
                        Ok(()) => passed += 1,
                        Err(error) => failures.push(format!(
                            "postgres mutation step={step} check `{sql}` mismatch: {error}"
                        )),
                    }
                }
                if failures.len() >= 80 {
                    failures.push(
                        "... postgres mutation matrix truncated after 80 mismatches".to_owned(),
                    );
                    break;
                }
            }
        }
    }

    let _ = exec_pg_command(&psql_bin, &dsn, &format!("DROP TABLE IF EXISTS {table}"));

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

fn psql_available(psql_bin: &str) -> bool {
    Command::new(psql_bin)
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn exec_both_command(
    aion: &Connection<Engine>,
    psql_bin: &str,
    dsn: &str,
    sql: &str,
) -> Result<(), String> {
    aion.execute(sql)
        .map_err(|error| format!("aiondb command failed: {error}"))?;
    exec_pg_command(psql_bin, dsn, sql).map_err(|error| format!("postgres command failed: {error}"))
}

fn compare_query(
    aion: &Connection<Engine>,
    psql_bin: &str,
    dsn: &str,
    sql: &str,
) -> Result<(), String> {
    let left = TestDb::query_strings(aion, sql)
        .map_err(|error| format!("aiondb query failed: {error}"))?;
    let right = exec_pg_query(psql_bin, dsn, sql)?;

    let normalized_left = normalize_matrix(left);
    let normalized_right = normalize_matrix(right);
    if normalized_left == normalized_right {
        Ok(())
    } else {
        Err(format!(
            "rows differ: aiondb={} postgres={}",
            truncate(&format!("{normalized_left:?}"), 220),
            truncate(&format!("{normalized_right:?}"), 220)
        ))
    }
}

fn exec_pg_command(psql_bin: &str, dsn: &str, sql: &str) -> Result<(), String> {
    let output = Command::new(psql_bin)
        .arg("-X")
        .arg("-v")
        .arg("ON_ERROR_STOP=1")
        .arg("-d")
        .arg(dsn)
        .arg("-c")
        .arg(sql)
        .output()
        .map_err(|error| format!("spawn psql failed: {error}"))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        Err(stderr)
    }
}

fn exec_pg_query(psql_bin: &str, dsn: &str, sql: &str) -> Result<Vec<Vec<String>>, String> {
    let output = Command::new(psql_bin)
        .arg("-X")
        .arg("-A")
        .arg("-t")
        .arg("-F")
        .arg("\t")
        .arg("-v")
        .arg("ON_ERROR_STOP=1")
        .arg("-d")
        .arg(dsn)
        .arg("-c")
        .arg(sql)
        .output()
        .map_err(|error| format!("spawn psql failed: {error}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        return Err(stderr);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut rows = Vec::new();
    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let cols = line
            .split('\t')
            .map(|value| normalize_scalar(value.to_owned()))
            .collect::<Vec<_>>();
        rows.push(cols);
    }
    Ok(rows)
}

fn normalize_matrix(rows: Vec<Vec<String>>) -> Vec<Vec<String>> {
    rows.into_iter()
        .map(|row| row.into_iter().map(normalize_scalar).collect())
        .collect()
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
        if float_value.fract() == 0.0 && float_value.abs() < 1e18 {
            return format!("{int_part}", int_part = float_value as i64);
        }
        return format!("{float_value}");
    }
    value
}

fn unique_suffix() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}_{}", std::process::id(), now)
}

fn truncate(text: &str, max_len: usize) -> String {
    if text.len() <= max_len {
        text.to_owned()
    } else {
        format!("{}...", &text[..max_len])
    }
}
