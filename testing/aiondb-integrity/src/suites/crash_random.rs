use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aiondb_config::StorageBackend;
use aiondb_embedded::{Database, RuntimeConfig};
use aiondb_engine::{Engine, EngineBuilder};
use aiondb_security::AllowAllAuthorizer;

use std::sync::Arc;

use crate::harness::{SuiteResult, SuiteStats, TestDb};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CrashBackend {
    Durable,
    Disk,
}

impl CrashBackend {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "durable" | "wal" => Some(Self::Durable),
            "disk" => Some(Self::Disk),
            _ => None,
        }
    }

    fn from_env() -> Result<Self, String> {
        let raw = std::env::var("AIONDB_INTEGRITY_CRASH_BACKEND")
            .unwrap_or_else(|_| "durable".to_owned());
        Self::parse(&raw).ok_or_else(|| {
            format!("invalid AIONDB_INTEGRITY_CRASH_BACKEND '{raw}' (expected 'durable' or 'disk')")
        })
    }

    fn as_worker_arg(self) -> &'static str {
        match self {
            Self::Durable => "durable",
            Self::Disk => "disk",
        }
    }

    fn wal_dir(self, data_dir: &Path) -> PathBuf {
        match self {
            Self::Durable => data_dir.join("wal"),
            Self::Disk => data_dir.join("disk").join("wal"),
        }
    }
}

pub fn run_all() -> SuiteResult {
    let rounds = env_usize("AIONDB_INTEGRITY_CRASH_ROUNDS", 20);
    let min_kill_ms = env_u64("AIONDB_INTEGRITY_CRASH_MIN_KILL_MS", 20);
    let max_kill_ms = env_u64("AIONDB_INTEGRITY_CRASH_MAX_KILL_MS", 320).max(min_kill_ms + 1);
    let backend = match CrashBackend::from_env() {
        Ok(backend) => backend,
        Err(error) => return Err(vec![error]),
    };

    let mut passed = 0_usize;
    let mut failures = Vec::new();
    let mut seed = initial_seed();

    let temp = match TempDirGuard::new("crash_random_kill") {
        Ok(temp) => temp,
        Err(error) => return Err(vec![error]),
    };
    let data_dir = temp.path().to_path_buf();

    if let Err(error) = initialize_schema(&data_dir, backend) {
        return Err(vec![format!("schema init failed ({backend:?}): {error}")]);
    }

    for round in 0..rounds {
        let worker_seed = next_u64(&mut seed);
        let mut child = match spawn_worker(&data_dir, worker_seed, backend) {
            Ok(child) => child,
            Err(error) => {
                failures.push(format!("round {round}: worker spawn failed: {error}"));
                break;
            }
        };

        let spread = max_kill_ms.saturating_sub(min_kill_ms);
        let sleep_ms = min_kill_ms + (next_u64(&mut seed) % spread.max(1));
        thread::sleep(Duration::from_millis(sleep_ms));

        // Intentional hard stop to simulate crash/power-loss style interruption.
        let _ = child.kill();
        let _ = child.wait();

        if let Err(error) = verify_wal_presence(&data_dir, backend) {
            failures.push(format!("round {round}: WAL check failed: {error}"));
            continue;
        }
        if let Err(error) = verify_integrity(&data_dir, backend) {
            failures.push(format!("round {round}: integrity check failed: {error}"));
            continue;
        }

        passed += 1;
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

pub fn run_worker(path: &Path, seed: u64, max_steps: usize, backend_arg: Option<&str>) -> i32 {
    let backend = backend_arg
        .and_then(CrashBackend::parse)
        .unwrap_or(CrashBackend::Durable);

    let db = match open_database(path, backend) {
        Ok(db) => db,
        Err(_) => return 11,
    };
    let conn = match db.connect_anonymous("default", "integrity-crash-worker") {
        Ok(conn) => conn,
        Err(_) => return 12,
    };

    if conn
        .execute(
            "CREATE TABLE IF NOT EXISTS crash_kv (\
             k INTEGER PRIMARY KEY, \
             v BIGINT NOT NULL)",
        )
        .is_err()
    {
        return 13;
    }
    if conn
        .execute("CREATE TABLE IF NOT EXISTS crash_log (delta BIGINT NOT NULL)")
        .is_err()
    {
        return 14;
    }

    let mut rng = seed ^ 0xD00D_F00D_1234_5678;
    for _ in 0..max_steps {
        let key = (next_u64(&mut rng) % 512) + 1;
        let delta = (next_u64(&mut rng) % 201) as i64 - 100;

        let tx_sql = format!(
            "BEGIN; \
             INSERT INTO crash_log (delta) VALUES ({delta}); \
             INSERT INTO crash_kv (k, v) VALUES ({key}, {delta}) \
             ON CONFLICT (k) DO UPDATE SET v = crash_kv.v + ({delta}); \
             COMMIT;"
        );

        if conn.execute(&tx_sql).is_err() {
            let _ = conn.execute("ROLLBACK");
        }
    }

    0
}

fn open_database(path: &Path, backend: CrashBackend) -> Result<Database<Engine>, String> {
    let mut runtime_config = RuntimeConfig::default();
    runtime_config.storage.backend = match backend {
        CrashBackend::Durable => StorageBackend::Durable,
        CrashBackend::Disk => StorageBackend::Disk,
    };

    let builder = EngineBuilder::new_with_config(path.to_path_buf(), runtime_config)
        .map_err(|error| format!("open {backend:?} backend database failed: {error}"))?;
    let engine = builder
        .with_authorizer(Arc::new(AllowAllAuthorizer))
        .with_allow_ephemeral_users(true)
        .build()
        .map_err(|error| format!("build {backend:?} backend engine failed: {error}"))?;
    Ok(Database::new(Arc::new(engine)))
}

fn initialize_schema(path: &Path, backend: CrashBackend) -> Result<(), String> {
    let db = open_database(path, backend)?;
    let conn = db
        .connect_anonymous("default", "integrity-crash-init")
        .map_err(|error| format!("init connect failed: {error}"))?;
    TestDb::exec_ok(
        &conn,
        "CREATE TABLE IF NOT EXISTS crash_kv (k INTEGER PRIMARY KEY, v BIGINT NOT NULL)",
    );
    TestDb::exec_ok(
        &conn,
        "CREATE TABLE IF NOT EXISTS crash_log (delta BIGINT NOT NULL)",
    );
    Ok(())
}

fn verify_wal_presence(path: &Path, backend: CrashBackend) -> Result<(), String> {
    let wal_dir = backend.wal_dir(path);
    let entries = fs::read_dir(&wal_dir)
        .map_err(|error| format!("read WAL dir failed ({}): {error}", wal_dir.display()))?;

    let mut found = false;
    for entry in entries {
        let entry = entry.map_err(|error| format!("read WAL entry failed: {error}"))?;
        let file_path = entry.path();
        if !file_path.is_file() {
            continue;
        }
        let Some(name) = file_path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.starts_with("wal_")
            || !std::path::Path::new(name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("log"))
        {
            continue;
        }
        let metadata = fs::metadata(&file_path)
            .map_err(|error| format!("stat WAL file failed ({}): {error}", file_path.display()))?;
        if metadata.len() > 0 {
            found = true;
            break;
        }
    }

    if found {
        Ok(())
    } else {
        Err(format!(
            "no non-empty wal_*.log segment found in {}",
            wal_dir.display()
        ))
    }
}

fn verify_integrity(path: &Path, backend: CrashBackend) -> Result<(), String> {
    let db = open_database(path, backend).map_err(|error| format!("reopen failed: {error}"))?;
    let conn = db
        .connect_anonymous("default", "integrity-crash-check")
        .map_err(|error| format!("reopen connect failed: {error}"))?;

    let sums = TestDb::query_strings(
        &conn,
        "SELECT \
            coalesce((SELECT sum(v) FROM crash_kv), 0), \
            coalesce((SELECT sum(delta) FROM crash_log), 0), \
            (SELECT count(*) FROM crash_log), \
            (SELECT count(*) FROM crash_kv), \
            (SELECT count(*) FROM crash_log WHERE delta IS NULL), \
            (SELECT count(*) FROM crash_kv WHERE v IS NULL)",
    )
    .map_err(|error| format!("aggregate integrity query failed: {error}"))?;

    let row = sums
        .first()
        .ok_or_else(|| "aggregate integrity query returned no row".to_owned())?;

    let kv_sum = parse_i64_field(row, 0, "kv_sum")?;
    let log_sum = parse_i64_field(row, 1, "log_sum")?;
    let log_count = parse_i64_field(row, 2, "log_count")?;
    let kv_count = parse_i64_field(row, 3, "kv_count")?;
    let log_nulls = parse_i64_field(row, 4, "log_nulls")?;
    let kv_nulls = parse_i64_field(row, 5, "kv_nulls")?;

    if log_nulls != 0 || kv_nulls != 0 {
        return Err(format!(
            "unexpected NULL payloads detected: log_nulls={log_nulls}, kv_nulls={kv_nulls}"
        ));
    }
    if kv_sum != log_sum {
        return Err(format!(
            "sum mismatch after crash/reopen: kv_sum={kv_sum}, log_sum={log_sum}, log_count={log_count}, kv_count={kv_count}"
        ));
    }

    Ok(())
}

fn parse_i64_field(row: &[String], index: usize, label: &str) -> Result<i64, String> {
    row.get(index)
        .ok_or_else(|| format!("missing field {label} at index {index}"))?
        .parse::<i64>()
        .map_err(|error| format!("parse field {label} failed: {error}"))
}

fn spawn_worker(
    path: &Path,
    seed: u64,
    backend: CrashBackend,
) -> Result<std::process::Child, String> {
    let exe = std::env::current_exe()
        .map_err(|error| format!("resolve current executable failed: {error}"))?;
    Command::new(exe)
        .arg("__crash_random_worker")
        .arg(path)
        .arg(seed.to_string())
        .arg("500000")
        .arg(backend.as_worker_arg())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("spawn crash worker failed: {error}"))
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn initial_seed() -> u64 {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0xA10D_B0DB);
    ts ^ (std::process::id() as u64).wrapping_mul(0x9E37_79B9)
}

fn next_u64(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    *state
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
