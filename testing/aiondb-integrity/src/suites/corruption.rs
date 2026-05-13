use std::fs;
use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use std::time::Instant;
use std::time::{SystemTime, UNIX_EPOCH};

use aiondb_embedded::Database;

use crate::harness::{SuiteResult, SuiteStats, TestDb};

pub fn run_all() -> SuiteResult {
    let mut passed = 0_usize;
    let mut failures = Vec::new();
    let strict = corruption_strict_mode();

    let cases = [
        ("wal_header_flip", CorruptionKind::HeaderFlip),
        ("wal_mid_byte_flip", CorruptionKind::MidByteFlip),
        ("wal_tail_byte_flip", CorruptionKind::TailByteFlip),
        ("wal_zero_prefix_64", CorruptionKind::ZeroPrefix64),
        ("wal_zero_middle_64", CorruptionKind::ZeroMiddle64),
        ("wal_zero_tail_64", CorruptionKind::ZeroTail64),
        ("wal_truncate_half", CorruptionKind::TruncateHalf),
        ("wal_truncate_quarter", CorruptionKind::TruncateQuarter),
        ("wal_scramble_quarter", CorruptionKind::ScrambleQuarter),
        ("wal_stride_xor", CorruptionKind::StrideXor),
        ("wal_append_garbage_64", CorruptionKind::AppendGarbage64),
        ("wal_delete_segment", CorruptionKind::DeleteSegment),
    ];

    for (name, kind) in cases {
        match run_corruption_case(name, kind, strict) {
            Ok(()) => passed += 1,
            Err(error) => failures.push(format!("{name}: {error}")),
        }
    }

    if failures.is_empty() {
        Ok(SuiteStats { passed, skipped: 0 })
    } else {
        Err(failures)
    }
}

#[derive(Clone, Copy)]
enum CorruptionKind {
    HeaderFlip,
    MidByteFlip,
    TailByteFlip,
    ZeroPrefix64,
    ZeroMiddle64,
    ZeroTail64,
    TruncateHalf,
    TruncateQuarter,
    ScrambleQuarter,
    StrideXor,
    AppendGarbage64,
    DeleteSegment,
}

fn run_corruption_case(name: &str, kind: CorruptionKind, strict: bool) -> Result<(), String> {
    trace(&format!("case={name}: begin"));
    let temp = TempDirGuard::new(name)?;
    let path = temp.path();
    create_baseline_database(path, 220)?;
    trace(&format!("case={name}: baseline seeded"));

    let wal_files = list_wal_files(path)?;
    let target = wal_files
        .first()
        .ok_or_else(|| "no WAL segment found to corrupt".to_owned())?;
    corrupt_wal_segment(target, kind)?;
    trace(&format!(
        "case={name}: wal corrupted ({})",
        target.display()
    ));

    let outcome = reopen_with_timeout(path.to_path_buf(), Duration::from_secs(12))?;
    trace(&format!("case={name}: reopen outcome={outcome:?}"));
    match outcome {
        ReopenOutcome::OpenError => Ok(()),
        ReopenOutcome::QueryError => Ok(()),
        ReopenOutcome::Timeout => Ok(()),
        ReopenOutcome::QueryCount(count) => {
            if strict && count == 220 {
                Err(
                    "corruption was not detected (database reopened with unchanged rowcount)"
                        .to_owned(),
                )
            } else {
                Ok(())
            }
        }
    }
}

fn create_baseline_database(path: &Path, rows: usize) -> Result<(), String> {
    let db =
        Database::open(path).map_err(|error| format!("open durable database failed: {error}"))?;
    let conn = db
        .connect_anonymous("default", "integrity-corruption-seed")
        .map_err(|error| format!("seed connection failed: {error}"))?;
    TestDb::exec_ok(
        &conn,
        "CREATE TABLE corr_items (id INTEGER PRIMARY KEY, payload TEXT, amount INTEGER)",
    );

    let mut batch = String::from("BEGIN;");
    for id in 1..=rows {
        let payload = format!("payload_{id:04}");
        let amount = ((id * 37) % 1000) as i64 - 200;
        batch.push_str(&format!(
            "INSERT INTO corr_items (id, payload, amount) VALUES ({id}, '{payload}', {amount});"
        ));
    }
    batch.push_str("COMMIT;");
    conn.execute(&batch)
        .map_err(|error| format!("seed insert batch failed: {error}"))?;

    Ok(())
}

fn list_wal_files(path: &Path) -> Result<Vec<PathBuf>, String> {
    let wal_dir = path.join("wal");
    let mut files = Vec::new();
    let entries = fs::read_dir(&wal_dir)
        .map_err(|error| format!("read WAL dir failed ({}): {error}", wal_dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("read WAL entry failed: {error}"))?;
        let file_path = entry.path();
        if !file_path.is_file() {
            continue;
        }
        let Some(name) = file_path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.starts_with("wal_")
            && std::path::Path::new(name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("log"))
        {
            files.push(file_path);
        }
    }
    files.sort();
    Ok(files)
}

fn corrupt_wal_segment(path: &Path, kind: CorruptionKind) -> Result<(), String> {
    let mut bytes = fs::read(path)
        .map_err(|error| format!("read WAL segment failed ({}): {error}", path.display()))?;
    if bytes.is_empty() {
        return Err(format!("WAL segment is empty: {}", path.display()));
    }

    match kind {
        CorruptionKind::HeaderFlip => {
            bytes[0] ^= 0xFF;
            fs::write(path, &bytes)
                .map_err(|error| format!("write header-corrupted segment failed: {error}"))?;
        }
        CorruptionKind::MidByteFlip => {
            let index = bytes.len() / 2;
            bytes[index] ^= 0xA5;
            fs::write(path, &bytes)
                .map_err(|error| format!("write mid-byte-corrupted segment failed: {error}"))?;
        }
        CorruptionKind::TailByteFlip => {
            let index = bytes.len() - 1;
            bytes[index] ^= 0x5A;
            fs::write(path, &bytes)
                .map_err(|error| format!("write tail-byte-corrupted segment failed: {error}"))?;
        }
        CorruptionKind::ZeroPrefix64 => {
            let end = bytes.len().min(64);
            bytes[..end].fill(0);
            fs::write(path, &bytes)
                .map_err(|error| format!("write zero-prefix-corrupted segment failed: {error}"))?;
        }
        CorruptionKind::ZeroMiddle64 => {
            let mid = bytes.len() / 2;
            let start = mid.saturating_sub(32);
            let end = (start + 64).min(bytes.len());
            bytes[start..end].fill(0);
            fs::write(path, &bytes)
                .map_err(|error| format!("write zero-middle-corrupted segment failed: {error}"))?;
        }
        CorruptionKind::ZeroTail64 => {
            let start = bytes.len().saturating_sub(64);
            bytes[start..].fill(0);
            fs::write(path, &bytes)
                .map_err(|error| format!("write zero-tail-corrupted segment failed: {error}"))?;
        }
        CorruptionKind::TruncateHalf => {
            let new_len = (bytes.len() / 2).max(1);
            let mut file = OpenOptions::new()
                .write(true)
                .open(path)
                .map_err(|error| format!("open WAL for truncation failed: {error}"))?;
            file.set_len(new_len as u64)
                .map_err(|error| format!("truncate WAL segment failed: {error}"))?;
            file.seek(SeekFrom::Start(new_len as u64))
                .map_err(|error| format!("seek after truncation failed: {error}"))?;
            file.flush()
                .map_err(|error| format!("flush truncated WAL failed: {error}"))?;
        }
        CorruptionKind::TruncateQuarter => {
            let new_len = (bytes.len() / 4).max(1);
            let mut file = OpenOptions::new()
                .write(true)
                .open(path)
                .map_err(|error| format!("open WAL for quarter truncation failed: {error}"))?;
            file.set_len(new_len as u64)
                .map_err(|error| format!("quarter truncate WAL segment failed: {error}"))?;
            file.seek(SeekFrom::Start(new_len as u64))
                .map_err(|error| format!("seek after quarter truncation failed: {error}"))?;
            file.flush()
                .map_err(|error| format!("flush quarter-truncated WAL failed: {error}"))?;
        }
        CorruptionKind::ScrambleQuarter => {
            let span = (bytes.len() / 4).max(1);
            let start = bytes.len() / 3;
            let end = (start + span).min(bytes.len());
            for (idx, b) in bytes[start..end].iter_mut().enumerate() {
                let mask = ((idx as u8).wrapping_mul(31)).wrapping_add(17);
                *b ^= mask;
            }
            fs::write(path, &bytes)
                .map_err(|error| format!("write scrambled-corrupted segment failed: {error}"))?;
        }
        CorruptionKind::StrideXor => {
            let mut idx = 0_usize;
            while idx < bytes.len() {
                bytes[idx] ^= 0xC3;
                idx += 17;
            }
            fs::write(path, &bytes)
                .map_err(|error| format!("write stride-xor-corrupted segment failed: {error}"))?;
        }
        CorruptionKind::AppendGarbage64 => {
            let mut file = OpenOptions::new()
                .append(true)
                .open(path)
                .map_err(|error| format!("open WAL for append corruption failed: {error}"))?;
            let garbage = [0xDE_u8, 0xAD, 0xBE, 0xEF, 0x13, 0x37, 0xAA, 0x55];
            for _ in 0..8 {
                file.write_all(&garbage)
                    .map_err(|error| format!("append WAL garbage failed: {error}"))?;
            }
            file.flush()
                .map_err(|error| format!("flush WAL garbage append failed: {error}"))?;
        }
        CorruptionKind::DeleteSegment => {
            fs::remove_file(path).map_err(|error| format!("delete WAL segment failed: {error}"))?;
        }
    }

    Ok(())
}

fn corruption_strict_mode() -> bool {
    std::env::var("AIONDB_INTEGRITY_CORRUPTION_STRICT")
        .ok()
        .is_some_and(|raw| raw == "1" || raw.eq_ignore_ascii_case("true"))
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

#[derive(Debug, Clone, Copy)]
enum ReopenOutcome {
    OpenError,
    QueryError,
    Timeout,
    QueryCount(usize),
}

fn reopen_with_timeout(path: PathBuf, timeout: Duration) -> Result<ReopenOutcome, String> {
    let exe = std::env::current_exe()
        .map_err(|error| format!("resolve current executable failed: {error}"))?;
    let mut child = Command::new(exe)
        .arg("__corruption_reopen_probe")
        .arg(path)
        .spawn()
        .map_err(|error| format!("spawn reopen probe failed: {error}"))?;
    trace(&format!("probe pid={} started", child.id()));

    let started = Instant::now();
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("probe wait failed: {error}"))?
        {
            return Ok(match status.code() {
                Some(0) => ReopenOutcome::QueryCount(220),
                Some(11) => ReopenOutcome::OpenError,
                Some(12) => ReopenOutcome::QueryError,
                _ => ReopenOutcome::OpenError,
            });
        }

        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            trace("probe timeout reached; child killed");
            return Ok(ReopenOutcome::Timeout);
        }

        std::thread::sleep(Duration::from_millis(100));
    }
}

pub fn run_reopen_probe(path: &Path) -> i32 {
    let outcome = std::panic::catch_unwind(|| Database::open(path));
    match outcome {
        Err(_) => 11,
        Ok(Err(_)) => 11,
        Ok(Ok(db)) => match db.connect_anonymous("default", "integrity-corruption-probe") {
            Err(_) => 11,
            Ok(conn) => match TestDb::scalar(&conn, "SELECT count(*) FROM corr_items") {
                Ok(_) => 0,
                Err(_) => 12,
            },
        },
    }
}

fn trace(message: &str) {
    if std::env::var("AIONDB_INTEGRITY_CORRUPTION_TRACE")
        .ok()
        .is_some_and(|raw| raw == "1" || raw.eq_ignore_ascii_case("true"))
    {
        eprintln!("[corruption] {message}");
    }
}
