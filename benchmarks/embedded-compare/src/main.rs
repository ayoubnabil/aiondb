use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use aiondb_embedded::{
    Database as AionDatabase, Engine as AionEngine, StatementResult as AionStatementResult,
};
use serde::Serialize;
use surrealdb::engine::any::{connect, Any};
use surrealdb::opt::auth::Root;
use surrealdb::opt::Config;
use surrealdb::Surreal;

static RUN_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum EngineName {
    AionDb,
    SurrealDb,
}

impl EngineName {
    fn label(self) -> &'static str {
        match self {
            Self::AionDb => "aiondb_embedded",
            Self::SurrealDb => "surrealdb_embedded_mem",
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Scenario {
    Create,
    ReadPoint,
    UpdatePoint,
    CountAll,
    LimitUser,
    RangeOrder,
    GroupCount,
    OrderBy,
    BigResult,
    GraphOut,
    VectorTopK,
    HybridVectorFilter,
}

impl Scenario {
    fn all() -> &'static [Self] {
        &[
            Self::Create,
            Self::ReadPoint,
            Self::UpdatePoint,
            Self::CountAll,
            Self::LimitUser,
            Self::RangeOrder,
            Self::GroupCount,
            Self::OrderBy,
            Self::BigResult,
            Self::GraphOut,
            Self::VectorTopK,
            Self::HybridVectorFilter,
        ]
    }

    fn label(self) -> &'static str {
        match self {
            Self::Create => "[C]reate",
            Self::ReadPoint => "[R]ead::point_id",
            Self::UpdatePoint => "[U]pdate::point_id",
            Self::CountAll => "[S]can::count_all",
            Self::LimitUser => "[S]can::limit_user_order",
            Self::RangeOrder => "[S]can::range_order_limit",
            Self::GroupCount => "[S]can::group_count",
            Self::OrderBy => "[S]can::order_by_limit",
            Self::BigResult => "[S]can::big_result_1000",
            Self::GraphOut => "[S]can::graph_out_depth1",
            Self::VectorTopK => "[S]can::vector_l2_topk",
            Self::HybridVectorFilter => "[S]can::hybrid_filter_vector",
        }
    }

    fn category(self) -> &'static str {
        match self {
            Self::Create | Self::ReadPoint | Self::UpdatePoint => "crud",
            Self::GraphOut => "graph",
            Self::VectorTopK | Self::HybridVectorFilter => "vector",
            _ => "scan",
        }
    }
}

#[derive(Default)]
struct RawStats {
    ops: u64,
    errors: u64,
    latency_sum_ms: f64,
    samples_ms: Vec<f64>,
    checksum: u64,
    first_error: Option<String>,
}

#[derive(Serialize)]
struct Summary {
    status: String,
    ops: u64,
    ops_per_sec: f64,
    avg_ms: f64,
    p95_ms: f64,
    errors: u64,
    checksum: u64,
    first_error: Option<String>,
}

#[derive(Serialize)]
struct Row {
    engine: String,
    scenario: String,
    category: String,
    summary: Summary,
}

#[derive(Serialize)]
struct Report {
    metadata: serde_json::Value,
    results: Vec<Row>,
    ratios: BTreeMap<String, serde_json::Value>,
}

struct AionBench {
    conn: aiondb_embedded::Connection<AionEngine>,
}

struct SurrealBench {
    db: Surreal<Any>,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let rows = env_usize("EMBED_BENCH_ROWS", 2_000);
    let warmup_secs = env_f64("EMBED_BENCH_WARMUP_SECONDS", 1.0);
    let measure_secs = env_f64("EMBED_BENCH_SECONDS", 3.0);
    let out = env::var("EMBED_BENCH_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| default_output_path());
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent)?;
    }

    eprintln!("setup aiondb_embedded rows={rows}");
    let aion = setup_aion(rows).context("setup AionDB embedded")?;
    eprintln!("setup surrealdb_embedded_mem rows={rows}");
    let surreal = setup_surreal(rows).await.context("setup SurrealDB embedded")?;

    let mut results = Vec::new();
    for scenario in Scenario::all().iter().copied() {
        eprintln!("warmup {} {}", EngineName::AionDb.label(), scenario.label());
        let _ = bench_aion(&aion, scenario, rows, warmup_secs);
        eprintln!("measure {} {}", EngineName::AionDb.label(), scenario.label());
        let summary = summarize(bench_aion(&aion, scenario, rows, measure_secs), measure_secs);
        print_score(EngineName::AionDb, scenario, &summary);
        results.push(Row {
            engine: EngineName::AionDb.label().to_owned(),
            scenario: scenario.label().to_owned(),
            category: scenario.category().to_owned(),
            summary,
        });

        eprintln!("warmup {} {}", EngineName::SurrealDb.label(), scenario.label());
        let _ = bench_surreal(&surreal, scenario, rows, warmup_secs).await;
        eprintln!("measure {} {}", EngineName::SurrealDb.label(), scenario.label());
        let summary =
            summarize(bench_surreal(&surreal, scenario, rows, measure_secs).await, measure_secs);
        print_score(EngineName::SurrealDb, scenario, &summary);
        results.push(Row {
            engine: EngineName::SurrealDb.label().to_owned(),
            scenario: scenario.label().to_owned(),
            category: scenario.category().to_owned(),
            summary,
        });
    }

    let report = Report {
        metadata: serde_json::json!({
            "run_id": run_id(),
            "rows": rows,
            "warmup_seconds_per_engine_per_case": warmup_secs,
            "measure_seconds_per_engine_per_case": measure_secs,
            "aiondb": "aiondb-embedded Database::in_memory(), release build",
            "surrealdb": "surrealdb 3.0.5 embedded mem://?sync=never, release build",
            "created_unix_s": SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
            "note": "Local in-process benchmark. SQL/Cypher/SurrealQL are equivalent logical workloads, not a formal official SurrealDB crud-bench result.",
        }),
        ratios: build_ratios(&results),
        results,
    };
    let text = serde_json::to_string_pretty(&report)?;
    fs::write(&out, &text)?;
    println!("trace={}", out.display());
    println!("{text}");
    Ok(())
}

fn setup_aion(rows: usize) -> Result<AionBench> {
    let db = AionDatabase::in_memory()?;
    let conn = db.connect_anonymous("default", "embedded-bench")?;
    conn.execute(
        "CREATE TABLE record (
             id INT PRIMARY KEY,
             user_id INT NOT NULL,
             tenant_id INT NOT NULL,
             likes INT NOT NULL,
             title TEXT NOT NULL,
             body TEXT NOT NULL,
             embedding VECTOR(2)
         );
         CREATE INDEX record_user_idx ON record(user_id);
         CREATE INDEX record_likes_idx ON record(likes);
         CREATE TABLE bench_insert (
             id INT PRIMARY KEY,
             user_id INT NOT NULL,
             likes INT NOT NULL,
             title TEXT NOT NULL
         );
         CREATE TABLE related (
             source_id INT NOT NULL,
             target_id INT NOT NULL,
             weight INT NOT NULL
         );
         CREATE NODE LABEL rec ON record;
         CREATE EDGE LABEL related_to ON related SOURCE rec TARGET rec;",
    )?;

    for start in (1..=rows).step_by(200) {
        let end = (start + 199).min(rows);
        let mut sql = String::new();
        for id in start..=end {
            let user_id = ((id - 1) % 200) + 1;
            let tenant_id = (id % 16) + 1;
            let likes = (id * 17) % 10_000;
            let x = (id % 100) as f32 / 100.0;
            let y = ((id * 7) % 100) as f32 / 100.0;
            sql.push_str(&format!(
                "INSERT INTO record VALUES ({id}, {user_id}, {tenant_id}, {likes}, 'title-{id}', 'body text payload {id}', '[{x:.4},{y:.4}]');"
            ));
        }
        conn.execute(&sql)?;
    }

    for start in (1..=rows).step_by(200) {
        let end = (start + 199).min(rows);
        let mut sql = String::new();
        for id in start..=end {
            let target = (id % rows) + 1;
            sql.push_str(&format!(
                "INSERT INTO related VALUES ({id}, {target}, {});",
                id % 10
            ));
        }
        conn.execute(&sql)?;
    }

    Ok(AionBench { conn })
}

async fn setup_surreal(rows: usize) -> Result<SurrealBench> {
    let root = Root {
        username: "root".to_owned(),
        password: "root".to_owned(),
    };
    let db = connect(("mem://?sync=never", Config::new().user(root.clone()))).await?;
    db.signin(root).await?;
    db.use_ns("embedded_bench").use_db("embedded_bench").await?;
    db.query(
        "DEFINE INDEX record_user_idx ON TABLE record COLUMNS user_id;
         DEFINE INDEX record_likes_idx ON TABLE record COLUMNS likes;",
    )
    .await?;

    for start in (1..=rows).step_by(200) {
        let end = (start + 199).min(rows);
        let mut sql = String::new();
        for id in start..=end {
            let user_id = ((id - 1) % 200) + 1;
            let tenant_id = (id % 16) + 1;
            let likes = (id * 17) % 10_000;
            let x = (id % 100) as f32 / 100.0;
            let y = ((id * 7) % 100) as f32 / 100.0;
            sql.push_str(&format!(
                "CREATE record:{id} SET rid = {id}, user_id = {user_id}, tenant_id = {tenant_id}, likes = {likes}, title = 'title-{id}', body = 'body text payload {id}', embedding = [{x:.4}, {y:.4}];"
            ));
        }
        db.query(&sql).await?;
    }

    for start in (1..=rows).step_by(200) {
        let end = (start + 199).min(rows);
        let mut sql = String::new();
        for id in start..=end {
            let target = (id % rows) + 1;
            sql.push_str(&format!(
                "RELATE record:{id}->related_to->record:{target} SET weight = {};",
                id % 10
            ));
        }
        db.query(&sql).await?;
    }

    Ok(SurrealBench { db })
}

fn bench_aion(bench: &AionBench, scenario: Scenario, rows: usize, seconds: f64) -> RawStats {
    let deadline = Instant::now() + Duration::from_secs_f64(seconds);
    let mut stats = RawStats::default();
    let mut iteration = 0_u64;
    let run_offset = fresh_run_offset();
    while Instant::now() < deadline {
        iteration += 1;
        let sql = aion_sql(scenario, iteration, rows, run_offset);
        let before = Instant::now();
        let result = bench
            .conn
            .execute(&sql)
            .with_context(|| format!("AionDB execute {sql}"))
            .map(|results| checksum_aion(&results));
        record_result(&mut stats, before.elapsed(), result);
    }
    stats
}

async fn bench_surreal(
    bench: &SurrealBench,
    scenario: Scenario,
    rows: usize,
    seconds: f64,
) -> RawStats {
    let deadline = Instant::now() + Duration::from_secs_f64(seconds);
    let mut stats = RawStats::default();
    let mut iteration = 0_u64;
    let run_offset = fresh_run_offset();
    while Instant::now() < deadline {
        iteration += 1;
        let sql = surreal_sql(scenario, iteration, rows, run_offset);
        let before = Instant::now();
        let result = surreal_query_checksum(&bench.db, &sql)
            .await
            .with_context(|| format!("SurrealDB execute {sql}"));
        record_result(&mut stats, before.elapsed(), result);
    }
    stats
}

fn aion_sql(scenario: Scenario, iteration: u64, rows: usize, run_offset: u64) -> String {
    match scenario {
        Scenario::Create => {
            let id = 1_000_000_u64 + (run_offset % 1_000) * 1_000_000 + iteration;
            let user_id = (iteration % 200) + 1;
            format!(
                "INSERT INTO bench_insert VALUES ({id}, {user_id}, {}, 'insert-{id}')",
                iteration % 10_000
            )
        }
        Scenario::ReadPoint => {
            let id = ((iteration * 17) as usize % rows) + 1;
            format!("SELECT title FROM record WHERE id = {id} LIMIT 1")
        }
        Scenario::UpdatePoint => {
            let id = ((iteration * 13) as usize % rows) + 1;
            format!("UPDATE record SET likes = {} WHERE id = {id}", iteration % 10_000)
        }
        Scenario::CountAll => "SELECT count(*) FROM record".to_owned(),
        Scenario::LimitUser => {
            let user_id = (iteration % 200) + 1;
            format!(
                "SELECT id, title, likes FROM record WHERE user_id = {user_id} ORDER BY id DESC LIMIT 20"
            )
        }
        Scenario::RangeOrder => {
            let low = (iteration * 37) % 9_000;
            let high = low + 500;
            format!(
                "SELECT id, likes FROM record WHERE likes >= {low} AND likes < {high} ORDER BY likes LIMIT 50"
            )
        }
        Scenario::GroupCount => {
            "SELECT tenant_id, count(*) FROM record GROUP BY tenant_id ORDER BY tenant_id".to_owned()
        }
        Scenario::OrderBy => "SELECT id, title, likes FROM record ORDER BY likes DESC LIMIT 50".to_owned(),
        Scenario::BigResult => {
            "SELECT id, title, body, likes FROM record ORDER BY id LIMIT 1000".to_owned()
        }
        Scenario::GraphOut => {
            let id = ((iteration * 19) as usize % rows) + 1;
            format!(
                "MATCH (a:rec)-[:related_to]->(b:rec) WHERE a.id = {id} RETURN b.id LIMIT 20"
            )
        }
        Scenario::VectorTopK => {
            "SELECT id, likes, l2_distance(embedding, '[1.0,0.0]') AS dist FROM record ORDER BY dist LIMIT 20"
                .to_owned()
        }
        Scenario::HybridVectorFilter => {
            let tenant_id = (iteration % 16) + 1;
            format!(
                "SELECT id, likes, l2_distance(embedding, '[1.0,0.0]') AS dist FROM record WHERE tenant_id = {tenant_id} ORDER BY dist LIMIT 20"
            )
        }
    }
}

fn surreal_sql(scenario: Scenario, iteration: u64, rows: usize, run_offset: u64) -> String {
    match scenario {
        Scenario::Create => {
            let id = 1_000_000_u64 + (run_offset % 1_000) * 1_000_000 + iteration;
            let user_id = (iteration % 200) + 1;
            format!(
                "CREATE bench_insert:{id} SET rid = {id}, user_id = {user_id}, likes = {}, title = 'insert-{id}'",
                iteration % 10_000
            )
        }
        Scenario::ReadPoint => {
            let id = ((iteration * 17) as usize % rows) + 1;
            format!("SELECT title FROM record:{id}")
        }
        Scenario::UpdatePoint => {
            let id = ((iteration * 13) as usize % rows) + 1;
            format!("UPDATE record:{id} SET likes = {}", iteration % 10_000)
        }
        Scenario::CountAll => "SELECT count() FROM record GROUP ALL".to_owned(),
        Scenario::LimitUser => {
            let user_id = (iteration % 200) + 1;
            format!(
                "SELECT rid, title, likes FROM record WHERE user_id = {user_id} ORDER BY rid DESC LIMIT 20"
            )
        }
        Scenario::RangeOrder => {
            let low = (iteration * 37) % 9_000;
            let high = low + 500;
            format!(
                "SELECT rid, likes FROM record WHERE likes >= {low} AND likes < {high} ORDER BY likes LIMIT 50"
            )
        }
        Scenario::GroupCount => {
            "SELECT tenant_id, count() FROM record GROUP BY tenant_id ORDER BY tenant_id".to_owned()
        }
        Scenario::OrderBy => {
            "SELECT rid, title, likes FROM record ORDER BY likes DESC LIMIT 50".to_owned()
        }
        Scenario::BigResult => {
            "SELECT rid, title, body, likes FROM record ORDER BY rid LIMIT 1000".to_owned()
        }
        Scenario::GraphOut => {
            let id = ((iteration * 19) as usize % rows) + 1;
            format!("SELECT rid FROM record:{id}->related_to->record LIMIT 20")
        }
        Scenario::VectorTopK => {
            "SELECT rid, likes, vector::distance::euclidean(embedding, [1.0, 0.0]) AS dist FROM record ORDER BY dist LIMIT 20"
                .to_owned()
        }
        Scenario::HybridVectorFilter => {
            let tenant_id = (iteration % 16) + 1;
            format!(
                "SELECT rid, likes, vector::distance::euclidean(embedding, [1.0, 0.0]) AS dist FROM record WHERE tenant_id = {tenant_id} ORDER BY dist LIMIT 20"
            )
        }
    }
}

fn checksum_aion(results: &[AionStatementResult]) -> u64 {
    let mut checksum = 14_695_981_039_346_656_037_u64;
    for result in results {
        match result {
            AionStatementResult::Query { rows, .. } => {
                checksum = mix(checksum, rows.len() as u64);
                for row in rows.iter().take(8) {
                    for value in &row.values {
                        checksum = mix_str(checksum, &format!("{value}"));
                    }
                }
            }
            AionStatementResult::Command { rows_affected, .. } => {
                checksum = mix(checksum, *rows_affected);
            }
            other => {
                checksum = mix_str(checksum, &format!("{other:?}"));
            }
        }
    }
    checksum
}

async fn surreal_query_checksum(db: &Surreal<Any>, sql: &str) -> Result<u64> {
    let mut response = db.query(sql).await?;
    let value: surrealdb::types::Value = response.take(0usize)?;
    Ok(mix_str(
        14_695_981_039_346_656_037_u64,
        &format!("{value:?}"),
    ))
}

fn record_result(stats: &mut RawStats, elapsed: Duration, result: Result<u64>) {
    match result {
        Ok(checksum) => {
            stats.ops += 1;
            stats.latency_sum_ms += elapsed.as_secs_f64() * 1_000.0;
            stats.samples_ms.push(elapsed.as_secs_f64() * 1_000.0);
            stats.checksum = mix(stats.checksum, checksum);
        }
        Err(error) => {
            stats.errors += 1;
            if stats.first_error.is_none() {
                stats.first_error = Some(format!("{error:#}"));
            }
        }
    }
}

fn summarize(mut stats: RawStats, seconds: f64) -> Summary {
    stats
        .samples_ms
        .sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    let avg_ms = if stats.ops == 0 {
        0.0
    } else {
        stats.latency_sum_ms / stats.ops as f64
    };
    let p95_ms = percentile(&stats.samples_ms, 0.95);
    Summary {
        status: if stats.ops > 0 { "OK" } else { "FAIL" }.to_owned(),
        ops: stats.ops,
        ops_per_sec: stats.ops as f64 / seconds,
        avg_ms,
        p95_ms,
        errors: stats.errors,
        checksum: stats.checksum,
        first_error: stats.first_error,
    }
}

fn percentile(samples: &[f64], pct: f64) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let idx = ((samples.len() - 1) as f64 * pct).round() as usize;
    samples[idx.min(samples.len() - 1)]
}

fn print_score(engine: EngineName, scenario: Scenario, summary: &Summary) {
    println!(
        "score\t{}\t{}\t{}\t{:.2} ops/s\tavg {:.3} ms\tp95 {:.3} ms",
        engine.label(),
        scenario.label(),
        summary.status,
        summary.ops_per_sec,
        summary.avg_ms,
        summary.p95_ms
    );
    if let Some(error) = &summary.first_error {
        println!("error\t{}\t{}\t{}", engine.label(), scenario.label(), error);
    }
}

fn build_ratios(rows: &[Row]) -> BTreeMap<String, serde_json::Value> {
    let mut ratios = BTreeMap::new();
    for scenario in Scenario::all() {
        let label = scenario.label();
        let aion = rows
            .iter()
            .find(|row| row.engine == EngineName::AionDb.label() && row.scenario == label)
            .map(|row| row.summary.ops_per_sec)
            .unwrap_or(0.0);
        let surreal = rows
            .iter()
            .find(|row| row.engine == EngineName::SurrealDb.label() && row.scenario == label)
            .map(|row| row.summary.ops_per_sec)
            .unwrap_or(0.0);
        let ratio = if surreal > 0.0 { Some(aion / surreal) } else { None };
        ratios.insert(
            label.to_owned(),
            serde_json::json!({
                "aiondb_ops_per_sec": aion,
                "surrealdb_ops_per_sec": surreal,
                "aiondb_vs_surrealdb": ratio,
            }),
        );
    }
    ratios
}

fn mix(seed: u64, value: u64) -> u64 {
    seed ^ value
        .wrapping_add(0x9e37_79b9_7f4a_7c15)
        .wrapping_add(seed << 6)
        .wrapping_add(seed >> 2)
}

fn mix_str(mut seed: u64, text: &str) -> u64 {
    for byte in text.as_bytes() {
        seed = mix(seed, u64::from(*byte));
    }
    seed
}

fn env_usize(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn env_f64(name: &str, default: f64) -> f64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn fresh_run_offset() -> u64 {
    RUN_COUNTER.fetch_add(1, Ordering::Relaxed)
}

fn run_id() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    format!("embedded-compare-{secs}")
}

fn default_output_path() -> PathBuf {
    PathBuf::from("../../benchmarks/.state/embedded-compare").join(format!("{}.json", run_id()))
}
