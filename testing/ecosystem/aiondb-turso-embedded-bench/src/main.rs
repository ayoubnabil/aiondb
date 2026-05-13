use aiondb_embedded::{
    Database as AionDatabase, StatementResult as AionStatementResult, Value as AionValue,
};
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy)]
enum Scenario {
    PointId,
    FeedUser,
    CountUser,
    RangeFilter,
    SortTopLikes,
    AggregateUsers,
    BigResult,
    InsertSingle,
    UpdatePoint,
    SqlJoinFeed,
    SqlJoinAggregate,
}

impl Scenario {
    fn all() -> &'static [Self] {
        &[
            Self::PointId,
            Self::FeedUser,
            Self::CountUser,
            Self::RangeFilter,
            Self::SortTopLikes,
            Self::AggregateUsers,
            Self::BigResult,
            Self::InsertSingle,
            Self::UpdatePoint,
            Self::SqlJoinFeed,
            Self::SqlJoinAggregate,
        ]
    }

    fn label(self) -> &'static str {
        match self {
            Self::PointId => "point_id",
            Self::FeedUser => "feed_user",
            Self::CountUser => "count_user",
            Self::RangeFilter => "range_filter",
            Self::SortTopLikes => "sort_top_likes",
            Self::AggregateUsers => "aggregate_users",
            Self::BigResult => "big_result",
            Self::InsertSingle => "insert_single",
            Self::UpdatePoint => "update_point",
            Self::SqlJoinFeed => "sql_join_feed",
            Self::SqlJoinAggregate => "sql_join_aggregate",
        }
    }
}

#[derive(Serialize)]
struct Summary {
    ops: u64,
    tps: f64,
    avg_ms: f64,
    p95_ms: f64,
    result_checksum: u64,
    errors: u64,
    first_error: Option<String>,
}

#[derive(Default)]
struct RawStats {
    ops: u64,
    errors: u64,
    first_error: Option<String>,
    latency_sum_ms: f64,
    result_checksum: u64,
    samples: Vec<f64>,
}

#[derive(Serialize)]
struct Row {
    engine: String,
    scenario: String,
    summary: Summary,
}

#[derive(Serialize)]
struct Report {
    metadata: serde_json::Value,
    results: Vec<Row>,
    ratios: BTreeMap<String, serde_json::Value>,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let users = env_i32("EMBED_BENCH_USERS", 500);
    let posts = env_i32("EMBED_BENCH_POSTS", 10_000);
    let seconds = env_f64("EMBED_BENCH_SECONDS", 2.0);
    let out = env::var("EMBED_BENCH_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("tmp/aion_turso_embedded.json"));
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent)?;
    }

    let aion = setup_aion(users, posts).context("setup AionDB embedded")?;
    let turso = setup_turso(users, posts)
        .await
        .context("setup Turso embedded")?;

    verify_aion_matches_turso(&aion, &turso, users, posts)
        .await
        .context("correctness preflight")?;

    let mut rows = Vec::new();
    for scenario in Scenario::all() {
        eprintln!("bench aiondb_embedded {}", scenario.label());
        let summary = summarize(bench_aion(&aion, *scenario, users, posts, seconds), seconds);
        rows.push(Row {
            engine: "aiondb_embedded".to_owned(),
            scenario: scenario.label().to_owned(),
            summary,
        });
    }
    for scenario in Scenario::all() {
        eprintln!("bench turso_embedded {}", scenario.label());
        let summary = summarize(
            bench_turso(&turso, *scenario, users, posts, seconds).await,
            seconds,
        );
        rows.push(Row {
            engine: "turso_embedded".to_owned(),
            scenario: scenario.label().to_owned(),
            summary,
        });
    }

    let report = Report {
        metadata: serde_json::json!({
            "users": users,
            "posts": posts,
            "seconds": seconds,
            "mode": "in_memory_embedded_single_connection",
            "turso": "crate turso 0.5.3, in-process SQLite-compatible Rust engine",
            "aiondb": "aiondb-embedded Database::in_memory()",
            "correctness": "preflight compares AionDB and Turso query rows, timed loop validates cardinality and sentinels",
            "created_unix_s": SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        }),
        ratios: build_ratios(&rows),
        results: rows,
    };
    let text = serde_json::to_string_pretty(&report)?;
    fs::write(&out, &text)?;
    println!("{text}");
    Ok(())
}

fn setup_aion(
    users: i32,
    posts: i32,
) -> Result<aiondb_embedded::Connection<aiondb_embedded::Engine>> {
    let db = AionDatabase::in_memory()?;
    let conn = db.connect_anonymous("default", "bench")?;
    conn.execute(
        "CREATE TABLE users (id INT PRIMARY KEY, name TEXT NOT NULL, tenant_id INT NOT NULL, age INT NOT NULL);
         CREATE TABLE posts (id INT PRIMARY KEY, user_id INT NOT NULL, title TEXT NOT NULL, body TEXT NOT NULL, likes INT NOT NULL, created_at INT NOT NULL);
         CREATE INDEX posts_user_idx ON posts(user_id);
         CREATE INDEX posts_likes_idx ON posts(likes);
         CREATE TABLE probe_inserts (id INT PRIMARY KEY, user_id INT NOT NULL, title TEXT NOT NULL, body TEXT NOT NULL, likes INT NOT NULL);",
    )?;
    seed_aion(&conn, users, posts)?;
    Ok(conn)
}

async fn setup_turso(users: i32, posts: i32) -> Result<turso::Connection> {
    let db = turso::Builder::new_local(":memory:").build().await?;
    let conn = db.connect()?;
    conn.execute_batch(
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, tenant_id INTEGER NOT NULL, age INTEGER NOT NULL);
         CREATE TABLE posts (id INTEGER PRIMARY KEY, user_id INTEGER NOT NULL, title TEXT NOT NULL, body TEXT NOT NULL, likes INTEGER NOT NULL, created_at INTEGER NOT NULL);
         CREATE INDEX posts_user_idx ON posts(user_id);
         CREATE INDEX posts_likes_idx ON posts(likes);
         CREATE TABLE probe_inserts (id INTEGER PRIMARY KEY, user_id INTEGER NOT NULL, title TEXT NOT NULL, body TEXT NOT NULL, likes INTEGER NOT NULL);",
    )
    .await?;
    seed_turso(&conn, users, posts).await?;
    Ok(conn)
}

async fn verify_aion_matches_turso(
    aion: &aiondb_embedded::Connection<aiondb_embedded::Engine>,
    turso: &turso::Connection,
    users: i32,
    posts: i32,
) -> Result<()> {
    for scenario in Scenario::all()
        .iter()
        .copied()
        .filter(|scenario| is_query(*scenario))
    {
        for iteration in [1, 7, 123] {
            let sql = sql_for(scenario, iteration, users, posts, 0);
            let aion_rows = collect_aion_query_rows(
                scenario,
                &aion
                    .execute(&sql)
                    .with_context(|| format!("execute AionDB preflight {}", scenario.label()))?,
            )
            .with_context(|| format!("collect AionDB preflight {}", scenario.label()))?;
            let turso_rows = collect_turso_query_rows(turso, scenario, &sql)
                .await
                .with_context(|| format!("collect Turso preflight {}", scenario.label()))?;
            if aion_rows != turso_rows {
                bail!(
                    "preflight mismatch for {} iteration {iteration}: AionDB {} vs Turso {}",
                    scenario.label(),
                    preview_rows(&aion_rows),
                    preview_rows(&turso_rows)
                );
            }
        }
    }

    let insert_sql = sql_for(Scenario::InsertSingle, -1, users, posts, -10_000);
    validate_aion_result(
        Scenario::InsertSingle,
        -1,
        users,
        posts,
        &aion.execute(&insert_sql).context("execute AionDB insert preflight")?,
    )?;
    validate_command_result(
        Scenario::InsertSingle,
        turso
            .execute(&insert_sql, ())
            .await
            .context("execute Turso insert preflight")?,
    )?;

    let update_sql = sql_for(Scenario::UpdatePoint, -1, users, posts, 0);
    validate_aion_result(
        Scenario::UpdatePoint,
        -1,
        users,
        posts,
        &aion.execute(&update_sql).context("execute AionDB update preflight")?,
    )?;
    validate_command_result(
        Scenario::UpdatePoint,
        turso
            .execute(&update_sql, ())
            .await
            .context("execute Turso update preflight")?,
    )?;

    Ok(())
}

fn seed_aion(
    conn: &aiondb_embedded::Connection<aiondb_embedded::Engine>,
    users: i32,
    posts: i32,
) -> Result<()> {
    for start in (1..=users).step_by(500) {
        let end = (start + 499).min(users);
        let mut sql = String::new();
        for id in start..=end {
            sql.push_str(&format!(
                "INSERT INTO users VALUES ({id}, 'user-{id}', {}, {});",
                (id % 8) + 1,
                18 + (id % 60)
            ));
        }
        conn.execute(&sql)?;
    }
    for start in (1..=posts).step_by(250) {
        let end = (start + 249).min(posts);
        let mut sql = String::new();
        for id in start..=end {
            let user_id = ((id - 1) % users) + 1;
            sql.push_str(&format!(
                "INSERT INTO posts VALUES ({id}, {user_id}, 'post-{id}', 'body text needle{} repeated payload {id}', {}, {id});",
                id % 97,
                (id * 17) % 10_000
            ));
        }
        conn.execute(&sql)?;
    }
    Ok(())
}

async fn seed_turso(conn: &turso::Connection, users: i32, posts: i32) -> Result<()> {
    for start in (1..=users).step_by(500) {
        let end = (start + 499).min(users);
        let mut sql = String::new();
        for id in start..=end {
            sql.push_str(&format!(
                "INSERT INTO users VALUES ({id}, 'user-{id}', {}, {});",
                (id % 8) + 1,
                18 + (id % 60)
            ));
        }
        conn.execute_batch(&sql).await?;
    }
    for start in (1..=posts).step_by(250) {
        let end = (start + 249).min(posts);
        let mut sql = String::new();
        for id in start..=end {
            let user_id = ((id - 1) % users) + 1;
            sql.push_str(&format!(
                "INSERT INTO posts VALUES ({id}, {user_id}, 'post-{id}', 'body text needle{} repeated payload {id}', {}, {id});",
                id % 97,
                (id * 17) % 10_000
            ));
        }
        conn.execute_batch(&sql).await?;
    }
    Ok(())
}

fn bench_aion(
    conn: &aiondb_embedded::Connection<aiondb_embedded::Engine>,
    scenario: Scenario,
    users: i32,
    posts: i32,
    seconds: f64,
) -> RawStats {
    let deadline = Instant::now() + Duration::from_secs_f64(seconds);
    let mut stats = RawStats::default();
    let mut iteration = 0_i32;
    while Instant::now() < deadline {
        iteration += 1;
        let sql = sql_for(scenario, iteration, users, posts, 0);
        let before = Instant::now();
        match conn
            .execute(&sql)
            .context("execute AionDB statement")
            .and_then(|results| validate_aion_result(scenario, iteration, users, posts, &results))
        {
            Ok(checksum) => record_ok(&mut stats, before.elapsed(), checksum),
            Err(error) => record_err(&mut stats, format!("{error:?}")),
        }
    }
    stats
}

async fn bench_turso(
    conn: &turso::Connection,
    scenario: Scenario,
    users: i32,
    posts: i32,
    seconds: f64,
) -> RawStats {
    let deadline = Instant::now() + Duration::from_secs_f64(seconds);
    let mut stats = RawStats::default();
    let mut iteration = 0_i32;
    while Instant::now() < deadline {
        iteration += 1;
        let sql = sql_for(scenario, iteration, users, posts, 100_000_000);
        let before = Instant::now();
        let result = if is_query(scenario) {
            run_turso_query(conn, scenario, iteration, users, posts, &sql).await
        } else {
            conn.execute(&sql, ())
                .await
                .context("execute Turso statement")
                .and_then(|rows_affected| validate_command_result(scenario, rows_affected))
        };
        match result {
            Ok(checksum) => record_ok(&mut stats, before.elapsed(), checksum),
            Err(error) => record_err(&mut stats, format!("{error:?}")),
        }
    }
    stats
}

async fn run_turso_query(
    conn: &turso::Connection,
    scenario: Scenario,
    iteration: i32,
    users: i32,
    posts: i32,
    sql: &str,
) -> Result<u64> {
    let mut rows = conn.query(sql, ()).await?;
    let kinds = query_column_kinds(scenario)?;
    let mut row_count = 0_usize;
    let mut first_text: Option<String> = None;
    let mut first_i64: Option<i64> = None;
    let mut count_value: Option<i64> = None;
    let mut checksum = checksum_seed(scenario);
    while let Some(row) = rows.next().await? {
        for (idx, kind) in kinds.iter().copied().enumerate() {
            match kind {
                BenchColumnKind::Int => {
                    let value = row.get::<i64>(idx)?;
                    if row_count == 0 && idx == 0 {
                        match scenario {
                            Scenario::FeedUser | Scenario::CountUser | Scenario::BigResult => {
                                first_i64 = Some(value);
                            }
                            _ => {}
                        }
                    }
                    if row_count == 0 && idx == 0 && matches!(scenario, Scenario::CountUser) {
                        count_value = Some(value);
                    }
                    checksum = checksum_i64(checksum, value);
                }
                BenchColumnKind::Text => {
                    let value = row.get::<String>(idx)?;
                    if row_count == 0 && idx == 0 && matches!(scenario, Scenario::PointId) {
                        first_text = Some(value.clone());
                    }
                    checksum = checksum_str(checksum, &value);
                }
            }
        }
        row_count += 1;
    }
    validate_query_result(
        "turso_embedded",
        scenario,
        iteration,
        users,
        posts,
        row_count,
        first_text.as_deref(),
        first_i64,
        count_value,
    )?;
    Ok(checksum)
}

fn sql_for(
    scenario: Scenario,
    iteration: i32,
    users: i32,
    posts: i32,
    insert_offset: i32,
) -> String {
    match scenario {
        Scenario::PointId => {
            let id = ((iteration * 17).rem_euclid(posts)) + 1;
            format!("SELECT title FROM posts WHERE id = {id} LIMIT 1")
        }
        Scenario::FeedUser => {
            let user_id = (iteration.rem_euclid(users)) + 1;
            format!("SELECT id, title, likes FROM posts WHERE user_id = {user_id} ORDER BY id DESC LIMIT 20")
        }
        Scenario::CountUser => {
            let user_id = (iteration.rem_euclid(users)) + 1;
            format!("SELECT count(*) FROM posts WHERE user_id = {user_id}")
        }
        Scenario::RangeFilter => {
            let low = (iteration * 37).rem_euclid(9_000);
            let high = low + 500;
            format!("SELECT id, likes FROM posts WHERE likes >= {low} AND likes < {high} ORDER BY likes LIMIT 50")
        }
        Scenario::SortTopLikes => "SELECT id, title, likes FROM posts ORDER BY likes DESC LIMIT 20".to_owned(),
        Scenario::AggregateUsers => {
            "SELECT user_id, count(*) FROM posts GROUP BY user_id ORDER BY user_id".to_owned()
        }
        Scenario::BigResult => "SELECT id, title, body, likes FROM posts ORDER BY id LIMIT 1000".to_owned(),
        Scenario::InsertSingle => {
            let id = insert_offset + 1_000_000 + iteration;
            let user_id = (iteration.rem_euclid(users)) + 1;
            format!("INSERT INTO probe_inserts VALUES ({id}, {user_id}, 'insert-{id}', 'insert body {id}', {})", iteration.rem_euclid(10_000))
        }
        Scenario::UpdatePoint => {
            let id = ((iteration * 13).rem_euclid(posts)) + 1;
            format!("UPDATE posts SET likes = likes + 1 WHERE id = {id}")
        }
        Scenario::SqlJoinFeed => {
            let user_id = (iteration.rem_euclid(users)) + 1;
            format!("SELECT u.name, p.id, p.title FROM users u JOIN posts p ON u.id = p.user_id WHERE u.id = {user_id} ORDER BY p.id DESC LIMIT 20")
        }
        Scenario::SqlJoinAggregate => {
            "SELECT u.tenant_id, count(*) FROM users u JOIN posts p ON u.id = p.user_id GROUP BY u.tenant_id ORDER BY u.tenant_id".to_owned()
        }
    }
}

fn is_query(scenario: Scenario) -> bool {
    !matches!(scenario, Scenario::InsertSingle | Scenario::UpdatePoint)
}

fn validate_aion_result(
    scenario: Scenario,
    iteration: i32,
    users: i32,
    posts: i32,
    results: &[AionStatementResult],
) -> Result<u64> {
    match scenario {
        Scenario::InsertSingle | Scenario::UpdatePoint => {
            let [AionStatementResult::Command { rows_affected, .. }] = results else {
                bail!("aiondb_embedded {} returned non-command result", scenario.label());
            };
            validate_command_result(scenario, *rows_affected)
        }
        _ => {
            let [AionStatementResult::Query { rows, .. }] = results else {
                bail!("aiondb_embedded {} returned non-query result", scenario.label());
            };
            let checksum = checksum_aion_query_rows(scenario, rows)?;
            let first_text = match scenario {
                Scenario::PointId => rows
                    .first()
                    .and_then(|row| row.values.first())
                    .and_then(aion_text_value),
                _ => None,
            };
            let first_i64 = match scenario {
                Scenario::FeedUser | Scenario::BigResult => rows
                    .first()
                    .and_then(|row| row.values.first())
                    .and_then(aion_i64_value),
                _ => None,
            };
            let count_value = match scenario {
                Scenario::CountUser => rows
                    .first()
                    .and_then(|row| row.values.first())
                    .and_then(aion_i64_value),
                _ => None,
            };
            validate_query_result(
                "aiondb_embedded",
                scenario,
                iteration,
                users,
                posts,
                rows.len(),
                first_text,
                first_i64,
                count_value,
            )?;
            Ok(checksum)
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum BenchCell {
    Int(i64),
    Text(String),
}

#[derive(Clone, Copy)]
enum BenchColumnKind {
    Int,
    Text,
}

fn collect_aion_query_rows(
    scenario: Scenario,
    results: &[AionStatementResult],
) -> Result<Vec<Vec<BenchCell>>> {
    let [AionStatementResult::Query { rows, .. }] = results else {
        bail!("aiondb_embedded {} returned non-query result", scenario.label());
    };
    let kinds = query_column_kinds(scenario)?;
    rows.iter()
        .map(|row| {
            if row.values.len() != kinds.len() {
                bail!(
                    "aiondb_embedded {} returned {} columns, expected {}",
                    scenario.label(),
                    row.values.len(),
                    kinds.len()
                );
            }
            row.values
                .iter()
                .zip(kinds)
                .map(|(value, kind)| aion_cell(value, *kind))
                .collect()
        })
        .collect()
}

async fn collect_turso_query_rows(
    conn: &turso::Connection,
    scenario: Scenario,
    sql: &str,
) -> Result<Vec<Vec<BenchCell>>> {
    let kinds = query_column_kinds(scenario)?;
    let mut rows = conn.query(sql, ()).await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        let mut values = Vec::with_capacity(kinds.len());
        for (idx, kind) in kinds.iter().copied().enumerate() {
            values.push(match kind {
                BenchColumnKind::Int => BenchCell::Int(row.get::<i64>(idx)?),
                BenchColumnKind::Text => BenchCell::Text(row.get::<String>(idx)?),
            });
        }
        out.push(values);
    }
    Ok(out)
}

fn checksum_aion_query_rows(scenario: Scenario, rows: &[aiondb_core::Row]) -> Result<u64> {
    let kinds = query_column_kinds(scenario)?;
    let mut checksum = checksum_seed(scenario);
    for row in rows {
        if row.values.len() != kinds.len() {
            bail!(
                "aiondb_embedded {} returned {} columns, expected {}",
                scenario.label(),
                row.values.len(),
                kinds.len()
            );
        }
        for (value, kind) in row.values.iter().zip(kinds.iter().copied()) {
            checksum = match (value, kind) {
                (AionValue::Int(value), BenchColumnKind::Int) => {
                    checksum_i64(checksum, i64::from(*value))
                }
                (AionValue::BigInt(value), BenchColumnKind::Int) => checksum_i64(checksum, *value),
                (AionValue::Text(value), BenchColumnKind::Text) => checksum_str(checksum, value),
                _ => bail!("unexpected AionDB value {value:?}"),
            };
        }
    }
    Ok(checksum)
}

fn query_column_kinds(scenario: Scenario) -> Result<&'static [BenchColumnKind]> {
    match scenario {
        Scenario::PointId => Ok(&[BenchColumnKind::Text]),
        Scenario::FeedUser => Ok(&[
            BenchColumnKind::Int,
            BenchColumnKind::Text,
            BenchColumnKind::Int,
        ]),
        Scenario::CountUser => Ok(&[BenchColumnKind::Int]),
        Scenario::RangeFilter => Ok(&[BenchColumnKind::Int, BenchColumnKind::Int]),
        Scenario::SortTopLikes => Ok(&[
            BenchColumnKind::Int,
            BenchColumnKind::Text,
            BenchColumnKind::Int,
        ]),
        Scenario::AggregateUsers => Ok(&[BenchColumnKind::Int, BenchColumnKind::Int]),
        Scenario::BigResult => Ok(&[
            BenchColumnKind::Int,
            BenchColumnKind::Text,
            BenchColumnKind::Text,
            BenchColumnKind::Int,
        ]),
        Scenario::SqlJoinFeed => Ok(&[
            BenchColumnKind::Text,
            BenchColumnKind::Int,
            BenchColumnKind::Text,
        ]),
        Scenario::SqlJoinAggregate => Ok(&[BenchColumnKind::Int, BenchColumnKind::Int]),
        Scenario::InsertSingle | Scenario::UpdatePoint => {
            bail!("{} is not a query scenario", scenario.label())
        }
    }
}

fn aion_cell(value: &AionValue, kind: BenchColumnKind) -> Result<BenchCell> {
    match (value, kind) {
        (AionValue::Int(value), BenchColumnKind::Int) => Ok(BenchCell::Int(i64::from(*value))),
        (AionValue::BigInt(value), BenchColumnKind::Int) => Ok(BenchCell::Int(*value)),
        (AionValue::Text(value), BenchColumnKind::Text) => Ok(BenchCell::Text(value.clone())),
        _ => bail!("unexpected AionDB value {value:?}"),
    }
}

fn preview_rows(rows: &[Vec<BenchCell>]) -> String {
    let preview_len = rows.len().min(3);
    format!("{} rows {:?}", rows.len(), &rows[..preview_len])
}

fn validate_command_result(scenario: Scenario, rows_affected: u64) -> Result<u64> {
    if rows_affected != 1 {
        bail!(
            "{} affected {rows_affected} rows, expected 1",
            scenario.label()
        );
    }
    Ok(checksum_u64(checksum_seed(scenario), rows_affected))
}

fn validate_query_result(
    engine: &str,
    scenario: Scenario,
    iteration: i32,
    users: i32,
    posts: i32,
    row_count: usize,
    first_text: Option<&str>,
    first_i64: Option<i64>,
    count_value: Option<i64>,
) -> Result<()> {
    match expected_row_count(scenario, iteration, users, posts) {
        ExpectedRows::Exact(expected) if row_count != expected => {
            bail!(
                "{engine} {} returned {row_count} rows, expected {expected}",
                scenario.label()
            );
        }
        ExpectedRows::Between { min, max } if row_count < min || row_count > max => {
            bail!(
                "{engine} {} returned {row_count} rows, expected between {min} and {max}",
                scenario.label()
            );
        }
        _ => {}
    }

    match scenario {
        Scenario::PointId => {
            let id = ((iteration * 17).rem_euclid(posts)) + 1;
            let expected = format!("post-{id}");
            if first_text != Some(expected.as_str()) {
                bail!(
                    "{engine} point_id first title {:?}, expected {expected}",
                    first_text
                );
            }
        }
        Scenario::FeedUser => {
            let user_id = (iteration.rem_euclid(users)) + 1;
            let expected = last_post_id_for_user(user_id, users, posts);
            if first_i64 != Some(i64::from(expected)) {
                bail!(
                    "{engine} feed_user first id {:?}, expected {expected}",
                    first_i64
                );
            }
        }
        Scenario::CountUser => {
            let user_id = (iteration.rem_euclid(users)) + 1;
            let expected = i64::from(post_count_for_user(user_id, users, posts));
            if count_value != Some(expected) {
                bail!(
                    "{engine} count_user value {:?}, expected {expected}",
                    count_value
                );
            }
        }
        Scenario::BigResult => {
            if first_i64 != Some(1) {
                bail!("{engine} big_result first id {:?}, expected 1", first_i64);
            }
        }
        _ => {}
    }

    Ok(())
}

enum ExpectedRows {
    Exact(usize),
    Between { min: usize, max: usize },
}

fn expected_row_count(scenario: Scenario, iteration: i32, users: i32, posts: i32) -> ExpectedRows {
    match scenario {
        Scenario::PointId | Scenario::CountUser => ExpectedRows::Exact(1),
        Scenario::FeedUser | Scenario::SqlJoinFeed => {
            let user_id = (iteration.rem_euclid(users)) + 1;
            ExpectedRows::Exact(post_count_for_user(user_id, users, posts).min(20).max(0) as usize)
        },
        Scenario::SortTopLikes => ExpectedRows::Exact(posts.min(20).max(0) as usize),
        Scenario::RangeFilter if posts >= 10_000 => ExpectedRows::Exact(50),
        Scenario::RangeFilter => ExpectedRows::Between {
            min: 0,
            max: posts.min(50).max(0) as usize,
        },
        Scenario::AggregateUsers => ExpectedRows::Exact(users.max(0) as usize),
        Scenario::BigResult => ExpectedRows::Exact(posts.min(1000).max(0) as usize),
        Scenario::SqlJoinAggregate => ExpectedRows::Exact(users.clamp(0, 8) as usize),
        Scenario::InsertSingle | Scenario::UpdatePoint => ExpectedRows::Exact(0),
    }
}

fn post_count_for_user(user_id: i32, users: i32, posts: i32) -> i32 {
    if users <= 0 || posts <= 0 || user_id <= 0 || user_id > users {
        return 0;
    }
    let base = posts / users;
    let extra = posts % users;
    base + i32::from(user_id <= extra)
}

fn last_post_id_for_user(user_id: i32, users: i32, posts: i32) -> i32 {
    let count = post_count_for_user(user_id, users, posts);
    user_id + (count - 1).max(0) * users
}

fn aion_text_value(value: &AionValue) -> Option<&str> {
    match value {
        AionValue::Text(text) => Some(text),
        _ => None,
    }
}

fn aion_i64_value(value: &AionValue) -> Option<i64> {
    match value {
        AionValue::Int(value) => Some(i64::from(*value)),
        AionValue::BigInt(value) => Some(*value),
        _ => None,
    }
}

fn record_ok(stats: &mut RawStats, elapsed: Duration, result_checksum: u64) {
    stats.ops += 1;
    let ms = elapsed.as_secs_f64() * 1000.0;
    stats.latency_sum_ms += ms;
    stats.result_checksum ^= result_checksum.rotate_left((stats.ops % 63) as u32);
    stats.samples.push(ms);
}

fn record_err(stats: &mut RawStats, error: String) {
    stats.errors += 1;
    if stats.first_error.is_none() {
        stats.first_error = Some(error.chars().take(500).collect());
    }
}

fn summarize(mut raw: RawStats, seconds: f64) -> Summary {
    raw.samples.sort_by(|a, b| a.total_cmp(b));
    let avg = if raw.ops == 0 {
        0.0
    } else {
        raw.latency_sum_ms / raw.ops as f64
    };
    Summary {
        ops: raw.ops,
        tps: round3(raw.ops as f64 / seconds),
        avg_ms: round3(avg),
        p95_ms: round3(percentile(&raw.samples, 0.95)),
        result_checksum: raw.result_checksum,
        errors: raw.errors,
        first_error: raw.first_error,
    }
}

fn checksum_seed(scenario: Scenario) -> u64 {
    checksum_str(0xcbf2_9ce4_8422_2325, scenario.label())
}

fn checksum_i64(checksum: u64, value: i64) -> u64 {
    checksum_u64(checksum, value as u64)
}

fn checksum_str(mut checksum: u64, value: &str) -> u64 {
    checksum = checksum_u64(checksum, value.len() as u64);
    for byte in value.bytes() {
        checksum = checksum_u64(checksum, u64::from(byte));
    }
    checksum
}

fn checksum_u64(mut checksum: u64, value: u64) -> u64 {
    checksum ^= value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    checksum = checksum.rotate_left(27).wrapping_mul(0x94d0_49bb_1331_11eb);
    checksum
}

fn percentile(samples: &[f64], q: f64) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let idx = ((samples.len() as f64 * q).ceil() as usize).saturating_sub(1);
    samples[idx.min(samples.len() - 1)]
}

fn build_ratios(rows: &[Row]) -> BTreeMap<String, serde_json::Value> {
    let mut ratios = BTreeMap::new();
    for aion in rows.iter().filter(|row| row.engine == "aiondb_embedded") {
        if let Some(turso) = rows
            .iter()
            .find(|row| row.engine == "turso_embedded" && row.scenario == aion.scenario)
        {
            if turso.summary.tps > 0.0 {
                ratios.insert(
                    aion.scenario.clone(),
                    serde_json::json!({
                        "aion_tps": aion.summary.tps,
                        "turso_tps": turso.summary.tps,
                        "aion_over_turso": round3(aion.summary.tps / turso.summary.tps),
                        "aion_p95_ms": aion.summary.p95_ms,
                        "turso_p95_ms": turso.summary.p95_ms,
                    }),
                );
            }
        }
    }
    ratios
}

fn env_i32(name: &str, default: i32) -> i32 {
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

fn round3(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}
