use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use aiondb_embedded::Database;

use crate::harness::{SuiteResult, SuiteStats, TestDb};

pub fn run_restart_cycles() -> SuiteResult {
    let mut passed = 0_usize;
    let mut failures = Vec::new();

    let temp = match TempDirGuard::new("persistence_restart_cycles") {
        Ok(t) => t,
        Err(error) => return Err(vec![error]),
    };
    let path = temp.path().to_path_buf();

    let mut expected: BTreeMap<u64, i64> = BTreeMap::new();
    const CYCLES: u64 = 8;
    const STEPS_PER_CYCLE: u64 = 16;

    for cycle in 0..CYCLES {
        match run_cycle(&path, cycle, &mut expected, STEPS_PER_CYCLE) {
            Ok(cycle_passed) => passed += cycle_passed,
            Err(error) => {
                failures.push(format!("cycle {cycle}: {error}"));
                if failures.len() >= 60 {
                    failures
                        .push("... persistence failures truncated after 60 mismatches".to_owned());
                    break;
                }
            }
        }
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

fn run_cycle(
    data_dir: &Path,
    cycle: u64,
    expected: &mut BTreeMap<u64, i64>,
    steps: u64,
) -> Result<usize, String> {
    let mut passed = 0_usize;

    {
        let db = Database::open(data_dir)
            .map_err(|error| format!("open durable database failed: {error}"))?;
        let conn = db
            .connect_anonymous("default", "integrity-persistence-writer")
            .map_err(|error| format!("writer connect failed: {error}"))?;
        TestDb::exec_ok(
            &conn,
            "CREATE TABLE IF NOT EXISTS persist_hist (id INTEGER PRIMARY KEY, v INTEGER NOT NULL)",
        );

        for step in 0..steps {
            let id = ((cycle * 37 + step * 13) % 240) + 1;
            let delta = ((cycle as i64 * 17 + step as i64 * 29) % 101) - 50;
            let sql = format!(
                "INSERT INTO persist_hist (id, v) VALUES ({id}, {delta}) \
                 ON CONFLICT (id) DO UPDATE SET v = persist_hist.v + ({delta})"
            );
            conn.execute(&sql)
                .map_err(|error| format!("upsert failed at step {step}: {error}"))?;
            let current = expected.get(&id).copied().unwrap_or(0);
            expected.insert(id, current + delta);

            if step % 9 == 0 {
                let delete_id = ((cycle * 19 + step * 11) % 240) + 1;
                let del_sql = format!("DELETE FROM persist_hist WHERE id = {delete_id}");
                conn.execute(&del_sql)
                    .map_err(|error| format!("delete failed at step {step}: {error}"))?;
                expected.remove(&delete_id);
            }
            passed += 1;
        }
    }

    {
        let db = Database::open(data_dir)
            .map_err(|error| format!("reopen durable database failed: {error}"))?;
        let conn = db
            .connect_anonymous("default", "integrity-persistence-reader")
            .map_err(|error| format!("reader connect failed: {error}"))?;

        let rows = TestDb::query_strings(
            &conn,
            "SELECT count(*), coalesce(sum(v), 0) FROM persist_hist",
        )
        .map_err(|error| format!("aggregate read failed: {error}"))?;
        let first = rows
            .first()
            .ok_or_else(|| "aggregate read returned no row".to_owned())?;
        let observed_count = first
            .first()
            .ok_or_else(|| "aggregate row missing count".to_owned())?
            .parse::<usize>()
            .map_err(|error| format!("count parse failed: {error}"))?;
        let observed_sum = first
            .get(1)
            .ok_or_else(|| "aggregate row missing sum".to_owned())?
            .parse::<i64>()
            .map_err(|error| format!("sum parse failed: {error}"))?;

        let expected_count = expected.len();
        let expected_sum: i64 = expected.values().copied().sum();
        if observed_count != expected_count {
            return Err(format!(
                "count mismatch after reopen: expected {expected_count}, got {observed_count}"
            ));
        }
        if observed_sum != expected_sum {
            return Err(format!(
                "sum mismatch after reopen: expected {expected_sum}, got {observed_sum}"
            ));
        }
        passed += 1;

        for sample in 0_u64..20_u64 {
            let id = ((cycle * 53 + sample * 31) % 240) + 1;
            let sql = format!("SELECT v FROM persist_hist WHERE id = {id}");
            let observed = TestDb::query_strings(&conn, &sql)
                .map_err(|error| format!("sample read failed for id {id}: {error}"))?;
            let expected_value = expected.get(&id).copied();
            match (observed.first(), expected_value) {
                (Some(row), Some(v)) => {
                    let got = row
                        .first()
                        .ok_or_else(|| format!("sample row missing value for id {id}"))?
                        .parse::<i64>()
                        .map_err(|error| {
                            format!("sample value parse failed for id {id}: {error}")
                        })?;
                    if got != v {
                        return Err(format!(
                            "sample mismatch for id {id}: expected {v}, got {got}"
                        ));
                    }
                }
                (None, None) => {}
                (Some(_), None) => {
                    return Err(format!(
                        "sample mismatch for id {id}: expected no row, got row"
                    ));
                }
                (None, Some(v)) => {
                    return Err(format!(
                        "sample mismatch for id {id}: expected value {v}, got no row"
                    ));
                }
            }
            passed += 1;
        }

        for threshold in [-200_i64, -100, 0, 100, 200] {
            let sql = format!(
                "SELECT count(*), coalesce(sum(v), 0) FROM persist_hist WHERE v >= {threshold}"
            );
            let rows = TestDb::query_strings(&conn, &sql)
                .map_err(|error| format!("threshold aggregate failed ({threshold}): {error}"))?;
            let row = rows
                .first()
                .ok_or_else(|| format!("threshold aggregate returned no row ({threshold})"))?;
            let observed_count = row
                .first()
                .ok_or_else(|| format!("threshold aggregate missing count ({threshold})"))?
                .parse::<usize>()
                .map_err(|error| format!("threshold count parse failed ({threshold}): {error}"))?;
            let observed_sum = row
                .get(1)
                .ok_or_else(|| format!("threshold aggregate missing sum ({threshold})"))?
                .parse::<i64>()
                .map_err(|error| format!("threshold sum parse failed ({threshold}): {error}"))?;

            let mut expected_count = 0_usize;
            let mut expected_sum_filtered = 0_i64;
            for value in expected.values() {
                if *value >= threshold {
                    expected_count += 1;
                    expected_sum_filtered += *value;
                }
            }

            if observed_count != expected_count {
                return Err(format!(
                    "threshold count mismatch ({threshold}): expected {expected_count}, got {observed_count}"
                ));
            }
            if observed_sum != expected_sum_filtered {
                return Err(format!(
                    "threshold sum mismatch ({threshold}): expected {expected_sum_filtered}, got {observed_sum}"
                ));
            }
            passed += 1;
        }
    }

    Ok(passed)
}

struct TempDirGuard {
    path: PathBuf,
}

impl TempDirGuard {
    fn new(prefix: &str) -> Result<Self, String> {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| format!("clock error: {error}"))?
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "aiondb_integrity_{}_{}_{}",
            sanitize(prefix),
            std::process::id(),
            ts
        ));
        fs::create_dir_all(&path)
            .map_err(|error| format!("create temp dir failed ({}): {error}", path.display()))?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn sanitize(input: &str) -> String {
    input
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect()
}
