use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aiondb_config::{RuntimeConfig, StorageBackend};
use aiondb_engine::{
    Credential, Engine, EngineBuilder, QueryEngine, StartupParams, StatementResult, TransportInfo,
    Value,
};
use aiondb_security::AllowAllAuthorizer;

fn main() -> Result<(), String> {
    let args = Args::parse();
    let _ = fs::remove_dir_all(&args.data_dir);
    fs::create_dir_all(&args.data_dir)
        .map_err(|error| format!("create {}: {error}", args.data_dir.display()))?;

    let mut runtime = RuntimeConfig::default();
    runtime.storage.backend = args.backend;
    runtime.storage.data_dir = args.data_dir.clone();
    runtime.storage.table_pool_frames = args.table_frames;
    runtime.storage.snapshot_pool_frames = args.snapshot_frames;
    runtime.storage.max_open_files = 64;

    let started = Instant::now();
    let engine = EngineBuilder::new_with_config(args.data_dir.clone(), runtime.clone())
        .map_err(|error| format!("open engine: {error}"))?
        .with_authorizer(Arc::new(AllowAllAuthorizer))
        .with_allow_ephemeral_users(true)
        .build()
        .map_err(|error| format!("build engine: {error}"))?;
    let session = startup(&engine)?;

    eprintln!("persistence-microbench: create schema");
    exec(
        &engine,
        &session,
        "CREATE TABLE persist_items (id INT NOT NULL, bucket INT NOT NULL, payload TEXT)",
    )?;
    exec(
        &engine,
        &session,
        "CREATE INDEX persist_items_id_idx ON persist_items (id)",
    )?;

    eprintln!("persistence-microbench: insert {} rows", args.rows);
    let insert_started = Instant::now();
    exec(&engine, &session, "BEGIN")?;
    for chunk_start in (0..args.rows).step_by(args.chunk_size) {
        let chunk_end = (chunk_start + args.chunk_size).min(args.rows);
        let mut sql = String::from("INSERT INTO persist_items VALUES ");
        for id in chunk_start..chunk_end {
            if id > chunk_start {
                sql.push(',');
            }
            let bucket = id % 97;
            sql.push_str(&format!("({id}, {bucket}, 'payload_{id}')"));
        }
        exec(&engine, &session, &sql)?;
    }
    exec(&engine, &session, "COMMIT")?;
    let insert_elapsed = insert_started.elapsed();

    eprintln!("persistence-microbench: mutate indexed table");
    let mutation_started = Instant::now();
    let update_end = (args.rows / 20).max(1);
    let delete_start = args.rows / 2;
    let delete_end = (delete_start + (args.rows / 30).max(1)).min(args.rows);
    exec(
        &engine,
        &session,
        &format!(
            "UPDATE persist_items SET bucket = bucket + 1000 WHERE id >= 0 AND id < {update_end}"
        ),
    )?;
    exec(
        &engine,
        &session,
        &format!("DELETE FROM persist_items WHERE id >= {delete_start} AND id < {delete_end}"),
    )?;
    let mutation_elapsed = mutation_started.elapsed();

    eprintln!("persistence-microbench: checkpoint");
    let checkpoint_started = Instant::now();
    exec(&engine, &session, "CHECKPOINT")?;
    let checkpoint_elapsed = checkpoint_started.elapsed();
    let row_count_before = scalar_i64(&engine, &session, "SELECT COUNT(*) FROM persist_items")?;
    drop(engine);

    eprintln!("persistence-microbench: reopen");
    let reopen_started = Instant::now();
    let reopened = EngineBuilder::new_with_config(args.data_dir.clone(), runtime)
        .map_err(|error| format!("reopen engine: {error}"))?
        .with_authorizer(Arc::new(AllowAllAuthorizer))
        .with_allow_ephemeral_users(true)
        .build()
        .map_err(|error| format!("rebuild engine: {error}"))?;
    let reopened_session = startup(&reopened)?;
    let reopen_elapsed = reopen_started.elapsed();

    let row_count_after = scalar_i64(
        &reopened,
        &reopened_session,
        "SELECT COUNT(*) FROM persist_items",
    )?;
    if row_count_after != row_count_before {
        return Err(format!(
            "row count changed across restart: before={row_count_before} after={row_count_after}"
        ));
    }

    eprintln!("persistence-microbench: lookup after restart");
    let lookup_started = Instant::now();
    let mut lookup_latencies = Vec::with_capacity(args.lookups);
    let mut lookup_hits = 0_i64;
    for n in 0..args.lookups {
        let id = ((n * 7919) % args.rows).max(1);
        let query_started = Instant::now();
        let rows = query_row_count(
            &reopened,
            &reopened_session,
            &format!("SELECT id FROM persist_items WHERE id = {id}"),
        )?;
        lookup_latencies.push(query_started.elapsed());
        lookup_hits += i64::try_from(rows).unwrap_or(i64::MAX);
    }
    let lookup_elapsed = lookup_started.elapsed();

    let data_bytes = dir_size(&args.data_dir)?;
    let wal_bytes = first_non_empty_dir_size(&[
        args.data_dir.join("wal"),
        args.data_dir.join("disk").join("wal"),
    ])?;
    let checkpoint_bytes = first_non_empty_dir_size(&[
        args.data_dir.join("wal").join("table_pages"),
        args.data_dir.join("checkpoints").join("table_pages"),
        args.data_dir
            .join("disk")
            .join("checkpoints")
            .join("table_pages"),
    ])?;
    let index_bytes = first_non_empty_dir_size(&[
        args.data_dir.join("wal").join("index_pages"),
        args.data_dir.join("checkpoints").join("index_pages"),
        args.data_dir
            .join("disk")
            .join("checkpoints")
            .join("index_pages"),
    ])?;

    fs::create_dir_all("tmp").map_err(|error| format!("create tmp: {error}"))?;
    let json = format!(
        concat!(
            "{{\n",
            "  \"rows_inserted\": {rows},\n",
            "  \"rows_after_restart\": {rows_after},\n",
            "  \"insert_tps\": {insert_tps:.3},\n",
            "  \"mutation_seconds\": {mutation_s:.6},\n",
            "  \"checkpoint_seconds\": {checkpoint_s:.6},\n",
            "  \"reopen_seconds\": {reopen_s:.6},\n",
            "  \"lookup_tps_after_restart\": {lookup_tps:.3},\n",
            "  \"lookup_p95_ms_after_restart\": {lookup_p95:.3},\n",
            "  \"lookup_hits\": {lookup_hits},\n",
            "  \"data_bytes\": {data_bytes},\n",
            "  \"wal_bytes\": {wal_bytes},\n",
            "  \"table_page_bytes\": {checkpoint_bytes},\n",
            "  \"index_page_bytes\": {index_bytes},\n",
            "  \"total_seconds\": {total_s:.6},\n",
            "  \"data_dir\": \"{data_dir}\"\n",
            "}}\n"
        ),
        rows = args.rows,
        rows_after = row_count_after,
        insert_tps = args.rows as f64 / seconds(insert_elapsed),
        mutation_s = seconds(mutation_elapsed),
        checkpoint_s = seconds(checkpoint_elapsed),
        reopen_s = seconds(reopen_elapsed),
        lookup_tps = args.lookups as f64 / seconds(lookup_elapsed),
        lookup_p95 = percentile_ms(&mut lookup_latencies, 0.95),
        lookup_hits = lookup_hits,
        data_bytes = data_bytes,
        wal_bytes = wal_bytes,
        checkpoint_bytes = checkpoint_bytes,
        index_bytes = index_bytes,
        total_s = seconds(started.elapsed()),
        data_dir = args.data_dir.display(),
    );
    fs::write("tmp/persistence_microbench_latest.json", &json)
        .map_err(|error| format!("write benchmark json: {error}"))?;
    print!("{json}");
    Ok(())
}

struct Args {
    data_dir: PathBuf,
    rows: usize,
    lookups: usize,
    chunk_size: usize,
    table_frames: usize,
    snapshot_frames: usize,
    backend: StorageBackend,
}

impl Args {
    fn parse() -> Self {
        let mut rows = 10_000;
        let mut lookups = 1_000;
        let mut chunk_size = 500;
        let mut table_frames = 32;
        let mut snapshot_frames = 16;
        let mut backend = StorageBackend::Disk;
        let mut data_dir = default_data_dir();
        let mut iter = std::env::args().skip(1);
        while let Some(arg) = iter.next() {
            let Some(value) = iter.next() else {
                continue;
            };
            match arg.as_str() {
                "--rows" => rows = value.parse().unwrap_or(rows),
                "--lookups" => lookups = value.parse().unwrap_or(lookups),
                "--chunk-size" => chunk_size = value.parse().unwrap_or(chunk_size),
                "--table-frames" => table_frames = value.parse().unwrap_or(table_frames),
                "--snapshot-frames" => snapshot_frames = value.parse().unwrap_or(snapshot_frames),
                "--backend" => {
                    backend = if value.eq_ignore_ascii_case("durable")
                        || value.eq_ignore_ascii_case("wal")
                    {
                        StorageBackend::Durable
                    } else {
                        StorageBackend::Disk
                    };
                }
                "--data-dir" => data_dir = PathBuf::from(value),
                _ => {}
            }
        }
        Self {
            data_dir,
            rows,
            lookups,
            chunk_size: chunk_size.max(1),
            table_frames: table_frames.max(1),
            snapshot_frames: snapshot_frames.max(1),
            backend,
        }
    }
}

fn default_data_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("aiondb_persistence_microbench_{nanos}"))
}

fn startup(engine: &Engine) -> Result<aiondb_engine::SessionHandle, String> {
    engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("persistence-microbench".to_owned()),
            options: BTreeMap::new(),
            credential: Credential::Anonymous {
                user: "alice".to_owned(),
            },
            transport: TransportInfo::in_process(),
        })
        .map(|(session, _)| session)
        .map_err(|error| format!("startup: {error}"))
}

fn exec(engine: &Engine, session: &aiondb_engine::SessionHandle, sql: &str) -> Result<(), String> {
    engine
        .execute_sql(session, sql)
        .map(|_| ())
        .map_err(|error| format!("sql failed `{sql}`: {error}"))
}

fn scalar_i64(
    engine: &Engine,
    session: &aiondb_engine::SessionHandle,
    sql: &str,
) -> Result<i64, String> {
    let results = engine
        .execute_sql(session, sql)
        .map_err(|error| format!("sql failed `{sql}`: {error}"))?;
    for result in results {
        if let StatementResult::Query { rows, .. } = result {
            let Some(row) = rows.first() else {
                return Err(format!("query returned no rows: {sql}"));
            };
            let Some(value) = row.values.first() else {
                return Err(format!("query returned no columns: {sql}"));
            };
            return match value {
                Value::Int(value) => Ok(i64::from(*value)),
                Value::BigInt(value) => Ok(*value),
                other => Err(format!("query returned non-integer {other:?}: {sql}")),
            };
        }
    }
    Err(format!("query returned no result set: {sql}"))
}

fn query_row_count(
    engine: &Engine,
    session: &aiondb_engine::SessionHandle,
    sql: &str,
) -> Result<usize, String> {
    let results = engine
        .execute_sql(session, sql)
        .map_err(|error| format!("sql failed `{sql}`: {error}"))?;
    for result in results {
        if let StatementResult::Query { rows, .. } = result {
            return Ok(rows.len());
        }
    }
    Err(format!("query returned no result set: {sql}"))
}

fn dir_size(path: &Path) -> Result<u64, String> {
    if !path.exists() {
        return Ok(0);
    }
    let mut total = 0_u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(path) = stack.pop() {
        let metadata =
            fs::metadata(&path).map_err(|error| format!("stat {}: {error}", path.display()))?;
        if metadata.is_file() {
            total = total.saturating_add(metadata.len());
        } else if metadata.is_dir() {
            for entry in
                fs::read_dir(&path).map_err(|error| format!("read {}: {error}", path.display()))?
            {
                stack.push(entry.map_err(|error| format!("dir entry: {error}"))?.path());
            }
        }
    }
    Ok(total)
}

fn first_non_empty_dir_size(paths: &[PathBuf]) -> Result<u64, String> {
    for path in paths {
        let bytes = dir_size(path)?;
        if bytes > 0 {
            return Ok(bytes);
        }
    }
    Ok(0)
}

fn seconds(duration: Duration) -> f64 {
    duration.as_secs_f64().max(0.000_001)
}

fn percentile_ms(values: &mut [Duration], percentile: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_unstable();
    let idx = ((values.len() - 1) as f64 * percentile).round() as usize;
    values[idx].as_secs_f64() * 1_000.0
}
