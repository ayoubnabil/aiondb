use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aiondb_embedded::{
    Database as AionDatabase, Engine as AionEngine, StatementResult as AionStatementResult,
};
use anyhow::{Context, Result};
use serde::Serialize;
use surrealdb::engine::any::{connect, Any};
use surrealdb::opt::auth::Root;
use surrealdb::opt::Config as SurrealConfig;
use surrealdb::Surreal;
use tokio_postgres::{Client as PgClient, NoTls, SimpleQueryMessage};

const TENANT_COUNT: u64 = 32;
const REGION_COUNT: u64 = 16;
const SEGMENT_COUNT: u64 = 8;
const CATEGORY_COUNT: u64 = 48;
const DIST_SEED: u64 = 14_695_981_039_346_656_037;

#[derive(Clone, Copy, Debug)]
enum ScenarioKind {
    InsertAppend,
    PointCustomer,
    RangeOrders,
    TenantOrderRollup,
    JoinCustomerOrders,
    JoinOrderItemsProducts,
    RevenueBySegment,
    UpdateOrderStatus,
    UpdateProductStock,
    VectorTopK,
    HybridVectorFilter,
}

impl ScenarioKind {
    fn label(self) -> &'static str {
        match self {
            Self::InsertAppend => "insert_append",
            Self::PointCustomer => "point_customer",
            Self::RangeOrders => "range_orders",
            Self::TenantOrderRollup => "tenant_order_rollup",
            Self::JoinCustomerOrders => "join_customer_orders",
            Self::JoinOrderItemsProducts => "join_order_items_products",
            Self::RevenueBySegment => "revenue_by_segment",
            Self::UpdateOrderStatus => "update_order_status",
            Self::UpdateProductStock => "update_product_stock",
            Self::VectorTopK => "vector_topk",
            Self::HybridVectorFilter => "hybrid_vector_filter",
        }
    }

    fn category(self) -> &'static str {
        match self {
            Self::InsertAppend | Self::UpdateOrderStatus | Self::UpdateProductStock => "write",
            Self::PointCustomer | Self::RangeOrders => "lookup",
            Self::TenantOrderRollup | Self::RevenueBySegment => "analytics",
            Self::JoinCustomerOrders | Self::JoinOrderItemsProducts => "join",
            Self::VectorTopK | Self::HybridVectorFilter => "vector",
        }
    }
}

#[derive(Clone, Debug)]
struct CaseSpec {
    kind: ScenarioKind,
    variant: u32,
    label: String,
    category: &'static str,
}

impl CaseSpec {
    fn new(kind: ScenarioKind, variant: u32) -> Self {
        Self {
            kind,
            variant,
            label: format!("{}_v{:03}", kind.label(), variant),
            category: kind.category(),
        }
    }
}

fn build_cases() -> Vec<CaseSpec> {
    let plan = [
        (ScenarioKind::InsertAppend, 16_u32),
        (ScenarioKind::PointCustomer, 18_u32),
        (ScenarioKind::RangeOrders, 18_u32),
        (ScenarioKind::TenantOrderRollup, 20_u32),
        (ScenarioKind::JoinCustomerOrders, 24_u32),
        (ScenarioKind::JoinOrderItemsProducts, 24_u32),
        (ScenarioKind::RevenueBySegment, 24_u32),
        (ScenarioKind::UpdateOrderStatus, 16_u32),
        (ScenarioKind::UpdateProductStock, 16_u32),
        (ScenarioKind::VectorTopK, 24_u32),
        (ScenarioKind::HybridVectorFilter, 24_u32),
    ];
    let mut cases = Vec::new();
    for (kind, count) in plan {
        for variant in 1..=count {
            cases.push(CaseSpec::new(kind, variant));
        }
    }
    cases
}

#[derive(Clone, Debug, Serialize)]
struct DatasetSpec {
    customers: usize,
    products: usize,
    orders: usize,
    line_items_per_order: usize,
    batch_size: usize,
}

impl DatasetSpec {
    fn line_items_total(&self) -> usize {
        self.orders.saturating_mul(self.line_items_per_order)
    }
}

#[derive(Debug)]
struct Config {
    run_id: String,
    profile: String,
    dataset: DatasetSpec,
    warmup_iterations: usize,
    measure_iterations: usize,
    out: PathBuf,
    engine_filter: Option<Vec<String>>,
    postgres_url: Option<String>,
    cockroach_url: Option<String>,
    surreal_url: String,
    surreal_user: String,
    surreal_pass: String,
}

impl Config {
    fn from_env() -> Self {
        let profile = env::var("AIONDB_COMPARE_PROFILE").unwrap_or_else(|_| "medium".to_owned());
        let (customers, products, orders, items_per_order, warmup, measure, batch) =
            match profile.as_str() {
                "smoke" => (1_000, 2_000, 8_000, 4, 1, 4, 200),
                "large" => (50_000, 100_000, 400_000, 4, 2, 10, 400),
                "xlarge" => (100_000, 200_000, 1_000_000, 4, 2, 8, 500),
                _ => (10_000, 20_000, 80_000, 4, 1, 8, 300),
            };
        let customers = env_usize("AIONDB_COMPARE_CUSTOMERS", customers);
        let products = env_usize("AIONDB_COMPARE_PRODUCTS", products);
        let orders = env_usize("AIONDB_COMPARE_ORDERS", orders);
        let line_items_per_order = env_usize("AIONDB_COMPARE_ITEMS_PER_ORDER", items_per_order);
        let batch_size = env_usize("AIONDB_COMPARE_BATCH", batch);
        let warmup_iterations = env_usize("AIONDB_COMPARE_WARMUP", warmup);
        let measure_iterations = env_usize("AIONDB_COMPARE_MEASURE", measure);
        let run_id = run_id();
        let out = env::var("AIONDB_COMPARE_OUT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| default_output_path(&run_id));
        let engine_filter = env::var("AIONDB_COMPARE_ENGINES").ok().map(|raw| {
            raw.split(',')
                .map(|part| part.trim().to_ascii_lowercase())
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
        });
        Self {
            run_id,
            profile,
            dataset: DatasetSpec {
                customers,
                products,
                orders,
                line_items_per_order,
                batch_size,
            },
            warmup_iterations,
            measure_iterations,
            out,
            engine_filter,
            postgres_url: env::var("POSTGRES_BENCH_URL").ok(),
            cockroach_url: env::var("COCKROACH_BENCH_URL").ok(),
            surreal_url: env::var("SURREAL_BENCH_URL")
                .unwrap_or_else(|_| "mem://?sync=never".to_owned()),
            surreal_user: env::var("SURREAL_BENCH_USER").unwrap_or_else(|_| "root".to_owned()),
            surreal_pass: env::var("SURREAL_BENCH_PASS").unwrap_or_else(|_| "root".to_owned()),
        }
    }

    fn engine_enabled(&self, name: &str) -> bool {
        self.engine_filter
            .as_ref()
            .map(|items| items.iter().any(|item| item == name))
            .unwrap_or(true)
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
struct ResultRow {
    engine: String,
    scenario: String,
    category: String,
    summary: Summary,
}

#[derive(Serialize)]
struct Report {
    metadata: serde_json::Value,
    results: Vec<ResultRow>,
    ratios: BTreeMap<String, serde_json::Value>,
}

struct AionBench {
    conn: aiondb_embedded::Connection<AionEngine>,
}

struct PgBench {
    schema: String,
    client: PgClient,
}

struct SurrealBench {
    db: Surreal<Any>,
}

enum BenchEngine {
    Aion(AionBench),
    Postgres(PgBench),
    Surreal(SurrealBench),
}

struct BenchTarget {
    label: String,
    note: String,
    engine: Option<BenchEngine>,
}

impl BenchEngine {
    async fn setup(&self, dataset: &DatasetSpec) -> Result<()> {
        match self {
            Self::Aion(engine) => setup_aion(engine, dataset),
            Self::Postgres(engine) => setup_pg(engine, dataset).await,
            Self::Surreal(engine) => setup_surreal(engine, dataset).await,
        }
    }

    async fn run_case(
        &self,
        case: &CaseSpec,
        iteration: u64,
        dataset: &DatasetSpec,
    ) -> Result<u64> {
        match self {
            Self::Aion(engine) => run_aion_case(engine, case, iteration, dataset),
            Self::Postgres(engine) => run_pg_case(engine, case, iteration, dataset).await,
            Self::Surreal(engine) => run_surreal_case(engine, case, iteration, dataset).await,
        }
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let config = Config::from_env();
    let cases = build_cases();
    if let Some(parent) = config.out.parent() {
        fs::create_dir_all(parent)?;
    }

    eprintln!(
        "profile={} customers={} products={} orders={} line_items={} batch={} warmup={} measure={}",
        config.profile,
        config.dataset.customers,
        config.dataset.products,
        config.dataset.orders,
        config.dataset.line_items_total(),
        config.dataset.batch_size,
        config.warmup_iterations,
        config.measure_iterations,
    );
    eprintln!("case_count={}", cases.len());

    let mut targets = build_targets(&config).await;
    let mut results = Vec::new();

    for target in targets.iter_mut() {
        if let Some(engine) = target.engine.as_ref() {
            eprintln!("setup {}", target.label);
            match engine.setup(&config.dataset).await {
                Ok(()) => {
                    for case in &cases {
                        for warmup_idx in 0..config.warmup_iterations {
                            let _ = engine
                                .run_case(case, 1_000_000 + warmup_idx as u64, &config.dataset)
                                .await;
                        }
                        let summary = measure_engine(
                            engine,
                            case,
                            &config.dataset,
                            config.measure_iterations,
                        )
                        .await;
                        print_score(&target.label, case, &summary);
                        results.push(ResultRow {
                            engine: target.label.clone(),
                            scenario: case.label.clone(),
                            category: case.category.to_owned(),
                            summary,
                        });
                    }
                }
                Err(error) => {
                    let reason = format!("setup failed: {error:#}");
                    for case in &cases {
                        results.push(ResultRow {
                            engine: target.label.clone(),
                            scenario: case.label.clone(),
                            category: case.category.to_owned(),
                            summary: fail_summary(reason.clone()),
                        });
                    }
                }
            }
        } else {
            for case in &cases {
                results.push(ResultRow {
                    engine: target.label.clone(),
                    scenario: case.label.clone(),
                    category: case.category.to_owned(),
                    summary: skip_summary(target.note.clone()),
                });
            }
        }
    }

    let report = Report {
        metadata: serde_json::json!({
            "run_id": config.run_id,
            "profile": config.profile,
            "dataset": &config.dataset,
            "case_count": cases.len(),
            "warmup_iterations": config.warmup_iterations,
            "measure_iterations": config.measure_iterations,
            "output": config.out.display().to_string(),
            "created_unix_s": SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
            "notes": [
                "AionDB is measured in embedded mode.",
                "SurrealDB uses embedded mem by default unless SURREAL_BENCH_URL is overridden.",
                "PostgreSQL and CockroachDB use tokio-postgres simple_query with literal SQL.",
                "Vector workloads use native vector functions on AionDB/SurrealDB and scalar distance expressions on PostgreSQL/CockroachDB."
            ]
        }),
        ratios: build_ratios(&results, &cases),
        results,
    };
    let text = serde_json::to_string_pretty(&report)?;
    fs::write(&config.out, &text)?;
    println!("trace={}", config.out.display());
    println!("{text}");
    Ok(())
}

async fn build_targets(config: &Config) -> Vec<BenchTarget> {
    let mut targets = Vec::new();

    if config.engine_enabled("aiondb_embedded") {
        match AionDatabase::in_memory()
            .context("create AionDB embedded database")
            .and_then(|db| Ok(db.connect_anonymous("default", "cross-engine-bench")?))
        {
            Ok(conn) => targets.push(BenchTarget {
                label: "aiondb_embedded".to_owned(),
                note: "embedded in-process".to_owned(),
                engine: Some(BenchEngine::Aion(AionBench { conn })),
            }),
            Err(error) => targets.push(BenchTarget {
                label: "aiondb_embedded".to_owned(),
                note: format!("init failed: {error:#}"),
                engine: None,
            }),
        }
    }

    if config.engine_enabled("surrealdb_embedded_mem") {
        let root = Root {
            username: config.surreal_user.clone(),
            password: config.surreal_pass.clone(),
        };
        let surreal = async {
            let db = connect((
                config.surreal_url.as_str(),
                SurrealConfig::new().user(root.clone()),
            ))
            .await?;
            db.signin(root.clone()).await?;
            db.use_ns(format!("bench_ns_{}", config.run_id))
                .use_db(format!("bench_db_{}", config.run_id))
                .await?;
            Ok::<_, surrealdb::Error>(db)
        }
        .await;
        match surreal {
            Ok(db) => targets.push(BenchTarget {
                label: "surrealdb_embedded_mem".to_owned(),
                note: config.surreal_url.clone(),
                engine: Some(BenchEngine::Surreal(SurrealBench { db })),
            }),
            Err(error) => targets.push(BenchTarget {
                label: "surrealdb_embedded_mem".to_owned(),
                note: format!("init failed: {error}"),
                engine: None,
            }),
        }
    }

    if config.engine_enabled("postgresql") {
        if let Some(url) = &config.postgres_url {
            match new_pg("postgresql", url, &config.run_id).await {
                Ok(engine) => targets.push(BenchTarget {
                    label: "postgresql".to_owned(),
                    note: "tokio-postgres".to_owned(),
                    engine: Some(BenchEngine::Postgres(engine)),
                }),
                Err(error) => targets.push(BenchTarget {
                    label: "postgresql".to_owned(),
                    note: format!("init failed: {error:#}"),
                    engine: None,
                }),
            }
        } else {
            targets.push(BenchTarget {
                label: "postgresql".to_owned(),
                note: "POSTGRES_BENCH_URL not set".to_owned(),
                engine: None,
            });
        }
    }

    if config.engine_enabled("cockroachdb") {
        if let Some(url) = &config.cockroach_url {
            match new_pg("cockroachdb", url, &config.run_id).await {
                Ok(engine) => targets.push(BenchTarget {
                    label: "cockroachdb".to_owned(),
                    note: "tokio-postgres".to_owned(),
                    engine: Some(BenchEngine::Postgres(engine)),
                }),
                Err(error) => targets.push(BenchTarget {
                    label: "cockroachdb".to_owned(),
                    note: format!("init failed: {error:#}"),
                    engine: None,
                }),
            }
        } else {
            targets.push(BenchTarget {
                label: "cockroachdb".to_owned(),
                note: "COCKROACH_BENCH_URL not set".to_owned(),
                engine: None,
            });
        }
    }

    targets
}

async fn new_pg(label: &'static str, url: &str, run_id: &str) -> Result<PgBench> {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .with_context(|| format!("connect {label}"))?;
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            eprintln!("{label} connection error: {error}");
        }
    });
    Ok(PgBench {
        schema: format!(
            "bench_{}_{}",
            label.replace('-', "_"),
            run_id.replace('-', "_")
        ),
        client,
    })
}

async fn measure_engine(
    engine: &BenchEngine,
    case: &CaseSpec,
    dataset: &DatasetSpec,
    iterations: usize,
) -> Summary {
    let mut stats = RawStats::default();
    let started = Instant::now();
    for idx in 0..iterations {
        let before = Instant::now();
        let result = engine
            .run_case(case, 1 + idx as u64, dataset)
            .await
            .with_context(|| format!("scenario {}", case.label));
        record_result(&mut stats, before.elapsed(), result);
    }
    summarize(stats, started.elapsed())
}

fn setup_aion(engine: &AionBench, dataset: &DatasetSpec) -> Result<()> {
    engine.conn.execute(
        "CREATE TABLE customers (
             id INT PRIMARY KEY,
             tenant_id INT NOT NULL,
             region_id INT NOT NULL,
             segment_id INT NOT NULL,
             credit_score INT NOT NULL,
             name TEXT NOT NULL
         );
         CREATE TABLE products (
             id INT PRIMARY KEY,
             tenant_id INT NOT NULL,
             category_id INT NOT NULL,
             price_cents INT NOT NULL,
             stock_qty INT NOT NULL,
             embedding_0 FLOAT NOT NULL,
             embedding_1 FLOAT NOT NULL,
             embedding_2 FLOAT NOT NULL,
             embedding_3 FLOAT NOT NULL,
             embedding VECTOR(4)
         );
         CREATE TABLE orders (
             id INT PRIMARY KEY,
             customer_id INT NOT NULL,
             tenant_id INT NOT NULL,
             status INT NOT NULL,
             total_cents INT NOT NULL,
             created_day INT NOT NULL
         );
         CREATE TABLE line_items (
             id INT PRIMARY KEY,
             order_id INT NOT NULL,
             product_id INT NOT NULL,
             customer_id INT NOT NULL,
             tenant_id INT NOT NULL,
             segment_id INT NOT NULL,
             category_id INT NOT NULL,
             quantity INT NOT NULL,
             price_cents INT NOT NULL,
             discount_bp INT NOT NULL
         );
         CREATE TABLE bench_append (
             id INT PRIMARY KEY,
             tenant_id INT NOT NULL,
             customer_id INT NOT NULL,
             total_cents INT NOT NULL,
             title TEXT NOT NULL
         );
         CREATE INDEX customers_tenant_idx ON customers(tenant_id);
         CREATE INDEX products_tenant_idx ON products(tenant_id);
         CREATE INDEX products_category_idx ON products(category_id);
         CREATE INDEX orders_customer_idx ON orders(customer_id);
         CREATE INDEX orders_tenant_idx ON orders(tenant_id);
         CREATE INDEX line_items_order_idx ON line_items(order_id);
         CREATE INDEX line_items_product_idx ON line_items(product_id);
         CREATE INDEX line_items_tenant_idx ON line_items(tenant_id);",
    )?;

    load_customers_aion(engine, dataset)?;
    load_products_aion(engine, dataset)?;
    load_orders_aion(engine, dataset)?;
    load_line_items_aion(engine, dataset)?;
    Ok(())
}

async fn setup_pg(engine: &PgBench, dataset: &DatasetSpec) -> Result<()> {
    let sql = format!(
        "DROP SCHEMA IF EXISTS {schema} CASCADE;
         CREATE SCHEMA {schema};
         CREATE TABLE {schema}.customers (
             id BIGINT PRIMARY KEY,
             tenant_id INT NOT NULL,
             region_id INT NOT NULL,
             segment_id INT NOT NULL,
             credit_score INT NOT NULL,
             name TEXT NOT NULL
         );
         CREATE TABLE {schema}.products (
             id BIGINT PRIMARY KEY,
             tenant_id INT NOT NULL,
             category_id INT NOT NULL,
             price_cents INT NOT NULL,
             stock_qty INT NOT NULL,
             embedding_0 DOUBLE PRECISION NOT NULL,
             embedding_1 DOUBLE PRECISION NOT NULL,
             embedding_2 DOUBLE PRECISION NOT NULL,
             embedding_3 DOUBLE PRECISION NOT NULL
         );
         CREATE TABLE {schema}.orders (
             id BIGINT PRIMARY KEY,
             customer_id BIGINT NOT NULL,
             tenant_id INT NOT NULL,
             status INT NOT NULL,
             total_cents INT NOT NULL,
             created_day INT NOT NULL
         );
         CREATE TABLE {schema}.line_items (
             id BIGINT PRIMARY KEY,
             order_id BIGINT NOT NULL,
             product_id BIGINT NOT NULL,
             customer_id BIGINT NOT NULL,
             tenant_id INT NOT NULL,
             segment_id INT NOT NULL,
             category_id INT NOT NULL,
             quantity INT NOT NULL,
             price_cents INT NOT NULL,
             discount_bp INT NOT NULL
         );
         CREATE TABLE {schema}.bench_append (
             id BIGINT PRIMARY KEY,
             tenant_id INT NOT NULL,
             customer_id BIGINT NOT NULL,
             total_cents INT NOT NULL,
             title TEXT NOT NULL
         );
         CREATE INDEX customers_tenant_idx ON {schema}.customers(tenant_id);
         CREATE INDEX products_tenant_idx ON {schema}.products(tenant_id);
         CREATE INDEX products_category_idx ON {schema}.products(category_id);
         CREATE INDEX orders_customer_idx ON {schema}.orders(customer_id);
         CREATE INDEX orders_tenant_idx ON {schema}.orders(tenant_id);
         CREATE INDEX line_items_order_idx ON {schema}.line_items(order_id);
         CREATE INDEX line_items_product_idx ON {schema}.line_items(product_id);
         CREATE INDEX line_items_tenant_idx ON {schema}.line_items(tenant_id);",
        schema = engine.schema
    );
    engine.client.simple_query(&sql).await?;
    load_customers_pg(engine, dataset).await?;
    load_products_pg(engine, dataset).await?;
    load_orders_pg(engine, dataset).await?;
    load_line_items_pg(engine, dataset).await?;
    Ok(())
}

async fn setup_surreal(engine: &SurrealBench, dataset: &DatasetSpec) -> Result<()> {
    engine
        .db
        .query(
            "DEFINE INDEX customers_tenant_idx ON TABLE customers COLUMNS tenant_id;
             DEFINE INDEX products_tenant_idx ON TABLE products COLUMNS tenant_id;
             DEFINE INDEX products_category_idx ON TABLE products COLUMNS category_id;
             DEFINE INDEX orders_customer_idx ON TABLE orders COLUMNS customer_id;
             DEFINE INDEX orders_tenant_idx ON TABLE orders COLUMNS tenant_id;
             DEFINE INDEX line_items_order_idx ON TABLE line_items COLUMNS order_id;
             DEFINE INDEX line_items_product_idx ON TABLE line_items COLUMNS product_id;
             DEFINE INDEX line_items_tenant_idx ON TABLE line_items COLUMNS tenant_id;",
        )
        .await?;
    load_customers_surreal(engine, dataset).await?;
    load_products_surreal(engine, dataset).await?;
    load_orders_surreal(engine, dataset).await?;
    load_line_items_surreal(engine, dataset).await?;
    Ok(())
}

fn load_customers_aion(engine: &AionBench, dataset: &DatasetSpec) -> Result<()> {
    for start in (1..=dataset.customers as u64).step_by(dataset.batch_size) {
        let end = (start + dataset.batch_size as u64 - 1).min(dataset.customers as u64);
        let mut sql = String::new();
        for id in start..=end {
            let row = customer_row(id);
            sql.push_str(&format!(
                "INSERT INTO customers VALUES ({}, {}, {}, {}, {}, '{}');",
                row.id, row.tenant_id, row.region_id, row.segment_id, row.credit_score, row.name
            ));
        }
        engine.conn.execute(&sql)?;
    }
    Ok(())
}

fn load_products_aion(engine: &AionBench, dataset: &DatasetSpec) -> Result<()> {
    for start in (1..=dataset.products as u64).step_by(dataset.batch_size) {
        let end = (start + dataset.batch_size as u64 - 1).min(dataset.products as u64);
        let mut sql = String::new();
        for id in start..=end {
            let row = product_row(id);
            let vector = format!(
                "'[{:.6},{:.6},{:.6},{:.6}]'",
                row.embedding[0], row.embedding[1], row.embedding[2], row.embedding[3]
            );
            sql.push_str(&format!(
                "INSERT INTO products VALUES ({}, {}, {}, {}, {}, {:.6}, {:.6}, {:.6}, {:.6}, {});",
                row.id,
                row.tenant_id,
                row.category_id,
                row.price_cents,
                row.stock_qty,
                row.embedding[0],
                row.embedding[1],
                row.embedding[2],
                row.embedding[3],
                vector
            ));
        }
        engine.conn.execute(&sql)?;
    }
    Ok(())
}

fn load_orders_aion(engine: &AionBench, dataset: &DatasetSpec) -> Result<()> {
    for start in (1..=dataset.orders as u64).step_by(dataset.batch_size) {
        let end = (start + dataset.batch_size as u64 - 1).min(dataset.orders as u64);
        let mut sql = String::new();
        for id in start..=end {
            let row = order_row(id, dataset);
            sql.push_str(&format!(
                "INSERT INTO orders VALUES ({}, {}, {}, {}, {}, {});",
                row.id,
                row.customer_id,
                row.tenant_id,
                row.status,
                row.total_cents,
                row.created_day
            ));
        }
        engine.conn.execute(&sql)?;
    }
    Ok(())
}

fn load_line_items_aion(engine: &AionBench, dataset: &DatasetSpec) -> Result<()> {
    let total = dataset.line_items_total() as u64;
    for start in (1..=total).step_by(dataset.batch_size) {
        let end = (start + dataset.batch_size as u64 - 1).min(total);
        let mut sql = String::new();
        for id in start..=end {
            let row = line_item_row(id, dataset);
            sql.push_str(&format!(
                "INSERT INTO line_items VALUES ({}, {}, {}, {}, {}, {}, {}, {}, {}, {});",
                row.id,
                row.order_id,
                row.product_id,
                row.customer_id,
                row.tenant_id,
                row.segment_id,
                row.category_id,
                row.quantity,
                row.price_cents,
                row.discount_bp
            ));
        }
        engine.conn.execute(&sql)?;
    }
    Ok(())
}

async fn load_customers_pg(engine: &PgBench, dataset: &DatasetSpec) -> Result<()> {
    for start in (1..=dataset.customers as u64).step_by(dataset.batch_size) {
        let end = (start + dataset.batch_size as u64 - 1).min(dataset.customers as u64);
        let mut sql = String::new();
        for id in start..=end {
            let row = customer_row(id);
            sql.push_str(&format!(
                "INSERT INTO {schema}.customers VALUES ({}, {}, {}, {}, {}, '{}');",
                row.id,
                row.tenant_id,
                row.region_id,
                row.segment_id,
                row.credit_score,
                row.name,
                schema = engine.schema
            ));
        }
        engine.client.simple_query(&sql).await?;
    }
    Ok(())
}

async fn load_products_pg(engine: &PgBench, dataset: &DatasetSpec) -> Result<()> {
    for start in (1..=dataset.products as u64).step_by(dataset.batch_size) {
        let end = (start + dataset.batch_size as u64 - 1).min(dataset.products as u64);
        let mut sql = String::new();
        for id in start..=end {
            let row = product_row(id);
            sql.push_str(&format!(
                "INSERT INTO {schema}.products VALUES ({}, {}, {}, {}, {}, {:.6}, {:.6}, {:.6}, {:.6});",
                row.id,
                row.tenant_id,
                row.category_id,
                row.price_cents,
                row.stock_qty,
                row.embedding[0],
                row.embedding[1],
                row.embedding[2],
                row.embedding[3],
                schema = engine.schema
            ));
        }
        engine.client.simple_query(&sql).await?;
    }
    Ok(())
}

async fn load_orders_pg(engine: &PgBench, dataset: &DatasetSpec) -> Result<()> {
    for start in (1..=dataset.orders as u64).step_by(dataset.batch_size) {
        let end = (start + dataset.batch_size as u64 - 1).min(dataset.orders as u64);
        let mut sql = String::new();
        for id in start..=end {
            let row = order_row(id, dataset);
            sql.push_str(&format!(
                "INSERT INTO {schema}.orders VALUES ({}, {}, {}, {}, {}, {});",
                row.id,
                row.customer_id,
                row.tenant_id,
                row.status,
                row.total_cents,
                row.created_day,
                schema = engine.schema
            ));
        }
        engine.client.simple_query(&sql).await?;
    }
    Ok(())
}

async fn load_line_items_pg(engine: &PgBench, dataset: &DatasetSpec) -> Result<()> {
    let total = dataset.line_items_total() as u64;
    for start in (1..=total).step_by(dataset.batch_size) {
        let end = (start + dataset.batch_size as u64 - 1).min(total);
        let mut sql = String::new();
        for id in start..=end {
            let row = line_item_row(id, dataset);
            sql.push_str(&format!(
                "INSERT INTO {schema}.line_items VALUES ({}, {}, {}, {}, {}, {}, {}, {}, {}, {});",
                row.id,
                row.order_id,
                row.product_id,
                row.customer_id,
                row.tenant_id,
                row.segment_id,
                row.category_id,
                row.quantity,
                row.price_cents,
                row.discount_bp,
                schema = engine.schema
            ));
        }
        engine.client.simple_query(&sql).await?;
    }
    Ok(())
}

async fn load_customers_surreal(engine: &SurrealBench, dataset: &DatasetSpec) -> Result<()> {
    for start in (1..=dataset.customers as u64).step_by(dataset.batch_size) {
        let end = (start + dataset.batch_size as u64 - 1).min(dataset.customers as u64);
        let mut sql = String::new();
        for id in start..=end {
            let row = customer_row(id);
            sql.push_str(&format!(
                "CREATE customers:{id} SET id = {id}, tenant_id = {}, region_id = {}, segment_id = {}, credit_score = {}, name = '{}';",
                row.tenant_id, row.region_id, row.segment_id, row.credit_score, row.name
            ));
        }
        engine.db.query(&sql).await?;
    }
    Ok(())
}

async fn load_products_surreal(engine: &SurrealBench, dataset: &DatasetSpec) -> Result<()> {
    for start in (1..=dataset.products as u64).step_by(dataset.batch_size) {
        let end = (start + dataset.batch_size as u64 - 1).min(dataset.products as u64);
        let mut sql = String::new();
        for id in start..=end {
            let row = product_row(id);
            sql.push_str(&format!(
                "CREATE products:{id} SET id = {id}, tenant_id = {}, category_id = {}, price_cents = {}, stock_qty = {}, embedding_0 = {:.6}, embedding_1 = {:.6}, embedding_2 = {:.6}, embedding_3 = {:.6}, embedding = [{:.6}, {:.6}, {:.6}, {:.6}], name = 'product-{}';",
                row.tenant_id,
                row.category_id,
                row.price_cents,
                row.stock_qty,
                row.embedding[0],
                row.embedding[1],
                row.embedding[2],
                row.embedding[3],
                row.embedding[0],
                row.embedding[1],
                row.embedding[2],
                row.embedding[3],
                row.id
            ));
        }
        engine.db.query(&sql).await?;
    }
    Ok(())
}

async fn load_orders_surreal(engine: &SurrealBench, dataset: &DatasetSpec) -> Result<()> {
    for start in (1..=dataset.orders as u64).step_by(dataset.batch_size) {
        let end = (start + dataset.batch_size as u64 - 1).min(dataset.orders as u64);
        let mut sql = String::new();
        for id in start..=end {
            let row = order_row(id, dataset);
            sql.push_str(&format!(
                "CREATE orders:{id} SET id = {id}, customer_id = {}, customer_ref = customers:{}, tenant_id = {}, status = {}, total_cents = {}, created_day = {};",
                row.customer_id, row.customer_id, row.tenant_id, row.status, row.total_cents, row.created_day
            ));
        }
        engine.db.query(&sql).await?;
    }
    Ok(())
}

async fn load_line_items_surreal(engine: &SurrealBench, dataset: &DatasetSpec) -> Result<()> {
    let total = dataset.line_items_total() as u64;
    for start in (1..=total).step_by(dataset.batch_size) {
        let end = (start + dataset.batch_size as u64 - 1).min(total);
        let mut sql = String::new();
        for id in start..=end {
            let row = line_item_row(id, dataset);
            sql.push_str(&format!(
                "CREATE line_items:{id} SET id = {id}, order_id = {}, order_ref = orders:{}, product_id = {}, product_ref = products:{}, customer_id = {}, customer_ref = customers:{}, tenant_id = {}, segment_id = {}, category_id = {}, quantity = {}, price_cents = {}, discount_bp = {};",
                row.order_id,
                row.order_id,
                row.product_id,
                row.product_id,
                row.customer_id,
                row.customer_id,
                row.tenant_id,
                row.segment_id,
                row.category_id,
                row.quantity,
                row.price_cents,
                row.discount_bp
            ));
        }
        engine.db.query(&sql).await?;
    }
    Ok(())
}

fn run_aion_case(
    engine: &AionBench,
    case: &CaseSpec,
    iteration: u64,
    dataset: &DatasetSpec,
) -> Result<u64> {
    let sql = aion_sql(case, iteration, dataset);
    let results = engine.conn.execute(&sql)?;
    Ok(checksum_aion(&results))
}

async fn run_pg_case(
    engine: &PgBench,
    case: &CaseSpec,
    iteration: u64,
    dataset: &DatasetSpec,
) -> Result<u64> {
    let sql = pg_sql(engine, case, iteration, dataset);
    let rows = engine.client.simple_query(&sql).await?;
    Ok(checksum_pg(&rows))
}

async fn run_surreal_case(
    engine: &SurrealBench,
    case: &CaseSpec,
    iteration: u64,
    dataset: &DatasetSpec,
) -> Result<u64> {
    let sql = surreal_sql(case, iteration, dataset);
    let mut response = engine.db.query(sql).await?;
    let value: surrealdb::types::Value = response.take(0usize)?;
    Ok(mix_str(DIST_SEED, &format!("{value:?}")))
}

fn variant_stride(base: u64, variant: u32) -> u64 {
    base + (variant as u64 * 2) + 1
}

fn variant_limit(variant: u32, base: u64, step: u64, buckets: u32) -> u64 {
    base + u64::from((variant - 1) % buckets) * step
}

fn variant_span(variant: u32) -> u64 {
    [2_500, 5_000, 10_000, 20_000, 40_000, 80_000][(variant as usize - 1) % 6]
}

fn vector_limit(variant: u32) -> u64 {
    variant_limit(variant, 10, 5, 8)
}

fn aion_sql(case: &CaseSpec, iteration: u64, dataset: &DatasetSpec) -> String {
    match case.kind {
        ScenarioKind::InsertAppend => {
            let id = append_id(iteration + u64::from(case.variant) * 1_000_000);
            let customer_id =
                ((iteration * variant_stride(17, case.variant)) % dataset.customers as u64) + 1;
            let tenant_id = tenant_for(customer_id);
            let total_cents = 1_000 + (scramble(iteration ^ u64::from(case.variant)) % 150_000);
            format!(
                "INSERT INTO bench_append VALUES ({id}, {tenant_id}, {customer_id}, {total_cents}, 'append-{}-{id}')",
                case.variant
            )
        }
        ScenarioKind::PointCustomer => {
            let id = ((iteration * variant_stride(17, case.variant)) % dataset.customers as u64) + 1;
            format!("SELECT name, credit_score FROM customers WHERE id = {id} LIMIT 1")
        }
        ScenarioKind::RangeOrders => {
            let low = 1_000 + ((iteration * variant_stride(97, case.variant)) % 160_000);
            let high = low + variant_span(case.variant);
            let limit = variant_limit(case.variant, 25, 25, 8);
            format!(
                "SELECT id, total_cents, status FROM orders WHERE total_cents >= {low} AND total_cents < {high} ORDER BY total_cents, id LIMIT {limit}"
            )
        }
        ScenarioKind::TenantOrderRollup => match case.variant % 4 {
            1 => "SELECT tenant_id, count(*) AS order_count, sum(total_cents) AS gross FROM orders GROUP BY tenant_id ORDER BY tenant_id".to_owned(),
            2 => "SELECT status, count(*) AS order_count, avg(total_cents) AS avg_ticket FROM orders GROUP BY status ORDER BY status".to_owned(),
            3 => {
                let day = 30 + u64::from(case.variant) * 5;
                format!("SELECT tenant_id, status, count(*) AS order_count, sum(total_cents) AS gross FROM orders WHERE created_day <= {day} GROUP BY tenant_id, status ORDER BY tenant_id, status")
            }
            _ => {
                let tenant_id = ((iteration * variant_stride(3, case.variant)) % TENANT_COUNT) + 1;
                format!("SELECT status, count(*) AS order_count, sum(total_cents) AS gross FROM orders WHERE tenant_id = {tenant_id} GROUP BY status ORDER BY status")
            }
        }
        ScenarioKind::JoinCustomerOrders => {
            let customer_id =
                ((iteration * variant_stride(31, case.variant)) % dataset.customers as u64) + 1;
            let limit = variant_limit(case.variant, 20, 10, 8);
            let status = ((u64::from(case.variant) - 1) % 5) + 1;
            format!(
                "SELECT o.id, o.total_cents, c.name FROM orders o JOIN customers c ON c.id = o.customer_id WHERE o.customer_id = {customer_id} AND o.status = {status} ORDER BY o.created_day DESC LIMIT {limit}"
            )
        }
        ScenarioKind::JoinOrderItemsProducts => {
            let order_id =
                ((iteration * variant_stride(41, case.variant)) % dataset.orders as u64) + 1;
            let limit = variant_limit(case.variant, 25, 25, 8);
            let category_id = ((u64::from(case.variant) * 5) % CATEGORY_COUNT) + 1;
            format!(
                "SELECT li.id, li.order_id, li.quantity, p.price_cents, p.category_id FROM line_items li JOIN products p ON p.id = li.product_id WHERE li.order_id = {order_id} AND p.category_id >= {category_id} ORDER BY li.id LIMIT {limit}"
            )
        }
        ScenarioKind::RevenueBySegment => {
            let tenant_id = ((iteration * variant_stride(7, case.variant)) % TENANT_COUNT) + 1;
            let threshold = 24 + ((u64::from(case.variant) * 5) % 24);
            format!(
                "SELECT c.segment_id, count(*) AS line_count, sum(li.quantity * li.price_cents * (10000 - li.discount_bp) / 10000) AS revenue \
                 FROM line_items li JOIN orders o ON o.id = li.order_id JOIN customers c ON c.id = o.customer_id \
                 WHERE o.tenant_id = {tenant_id} AND li.category_id >= {threshold} GROUP BY c.segment_id ORDER BY revenue DESC, c.segment_id LIMIT 16"
            )
        }
        ScenarioKind::UpdateOrderStatus => {
            let order_id =
                ((iteration * variant_stride(11, case.variant)) % dataset.orders as u64) + 1;
            let status = ((iteration * 3 + u64::from(case.variant)) % 5) + 1;
            format!("UPDATE orders SET status = {status} WHERE id = {order_id}")
        }
        ScenarioKind::UpdateProductStock => {
            let tenant_id = ((iteration * variant_stride(5, case.variant)) % TENANT_COUNT) + 1;
            let category_id =
                ((iteration * variant_stride(13, case.variant)) % CATEGORY_COUNT) + 1;
            format!(
                "UPDATE products SET stock_qty = stock_qty - 1 WHERE tenant_id = {tenant_id} AND category_id = {category_id} AND stock_qty > 0"
            )
        }
        ScenarioKind::VectorTopK => {
            let q = query_vector(iteration + u64::from(case.variant) * 97);
            let limit = vector_limit(case.variant);
            format!(
                "SELECT id, category_id, l2_distance(embedding, '[{:.6},{:.6},{:.6},{:.6}]') AS dist FROM products ORDER BY dist LIMIT {limit}",
                q[0], q[1], q[2], q[3],
            )
        }
        ScenarioKind::HybridVectorFilter => {
            let tenant_id = ((iteration * variant_stride(5, case.variant)) % TENANT_COUNT) + 1;
            let category_id =
                ((iteration * variant_stride(13, case.variant)) % CATEGORY_COUNT) + 1;
            let q = query_vector(iteration + u64::from(case.variant) * 131);
            let limit = vector_limit(case.variant);
            format!(
                "SELECT id, category_id, l2_distance(embedding, '[{:.6},{:.6},{:.6},{:.6}]') AS dist \
                 FROM products WHERE tenant_id = {tenant_id} AND category_id = {category_id} ORDER BY dist LIMIT {limit}",
                q[0], q[1], q[2], q[3],
            )
        }
    }
}

fn pg_sql(engine: &PgBench, case: &CaseSpec, iteration: u64, dataset: &DatasetSpec) -> String {
    let customers = format!("{}.customers", engine.schema);
    let products = format!("{}.products", engine.schema);
    let orders = format!("{}.orders", engine.schema);
    let line_items = format!("{}.line_items", engine.schema);
    let bench_append = format!("{}.bench_append", engine.schema);
    match case.kind {
        ScenarioKind::InsertAppend => {
            let id = append_id(iteration + u64::from(case.variant) * 1_000_000);
            let customer_id =
                ((iteration * variant_stride(17, case.variant)) % dataset.customers as u64) + 1;
            let tenant_id = tenant_for(customer_id);
            let total_cents = 1_000 + (scramble(iteration ^ u64::from(case.variant)) % 150_000);
            format!(
                "INSERT INTO {bench_append} VALUES ({id}, {tenant_id}, {customer_id}, {total_cents}, 'append-{}-{id}')",
                case.variant
            )
        }
        ScenarioKind::PointCustomer => {
            let id = ((iteration * variant_stride(17, case.variant)) % dataset.customers as u64) + 1;
            format!("SELECT name, credit_score FROM {customers} WHERE id = {id} LIMIT 1")
        }
        ScenarioKind::RangeOrders => {
            let low = 1_000 + ((iteration * variant_stride(97, case.variant)) % 160_000);
            let high = low + variant_span(case.variant);
            let limit = variant_limit(case.variant, 25, 25, 8);
            format!(
                "SELECT id, total_cents, status FROM {orders} WHERE total_cents >= {low} AND total_cents < {high} ORDER BY total_cents, id LIMIT {limit}"
            )
        }
        ScenarioKind::TenantOrderRollup => match case.variant % 4 {
            1 => format!(
                "SELECT tenant_id, count(*) AS order_count, sum(total_cents) AS gross FROM {orders} GROUP BY tenant_id ORDER BY tenant_id"
            ),
            2 => format!(
                "SELECT status, count(*) AS order_count, avg(total_cents) AS avg_ticket FROM {orders} GROUP BY status ORDER BY status"
            ),
            3 => {
                let day = 30 + u64::from(case.variant) * 5;
                format!("SELECT tenant_id, status, count(*) AS order_count, sum(total_cents) AS gross FROM {orders} WHERE created_day <= {day} GROUP BY tenant_id, status ORDER BY tenant_id, status")
            }
            _ => {
                let tenant_id = ((iteration * variant_stride(3, case.variant)) % TENANT_COUNT) + 1;
                format!("SELECT status, count(*) AS order_count, sum(total_cents) AS gross FROM {orders} WHERE tenant_id = {tenant_id} GROUP BY status ORDER BY status")
            }
        }
        ScenarioKind::JoinCustomerOrders => {
            let customer_id =
                ((iteration * variant_stride(31, case.variant)) % dataset.customers as u64) + 1;
            let limit = variant_limit(case.variant, 20, 10, 8);
            let status = ((u64::from(case.variant) - 1) % 5) + 1;
            format!(
                "SELECT o.id, o.total_cents, c.name FROM {orders} o JOIN {customers} c ON c.id = o.customer_id WHERE o.customer_id = {customer_id} AND o.status = {status} ORDER BY o.created_day DESC LIMIT {limit}"
            )
        }
        ScenarioKind::JoinOrderItemsProducts => {
            let order_id =
                ((iteration * variant_stride(41, case.variant)) % dataset.orders as u64) + 1;
            let limit = variant_limit(case.variant, 25, 25, 8);
            let category_id = ((u64::from(case.variant) * 5) % CATEGORY_COUNT) + 1;
            format!(
                "SELECT li.id, li.order_id, li.quantity, p.price_cents, p.category_id FROM {line_items} li JOIN {products} p ON p.id = li.product_id WHERE li.order_id = {order_id} AND p.category_id >= {category_id} ORDER BY li.id LIMIT {limit}"
            )
        }
        ScenarioKind::RevenueBySegment => {
            let tenant_id = ((iteration * variant_stride(7, case.variant)) % TENANT_COUNT) + 1;
            let threshold = 24 + ((u64::from(case.variant) * 5) % 24);
            format!(
                "SELECT c.segment_id, count(*) AS line_count, sum(li.quantity * li.price_cents * (10000 - li.discount_bp) / 10000) AS revenue \
                 FROM {line_items} li JOIN {orders} o ON o.id = li.order_id JOIN {customers} c ON c.id = o.customer_id \
                 WHERE o.tenant_id = {tenant_id} AND li.category_id >= {threshold} GROUP BY c.segment_id ORDER BY revenue DESC, c.segment_id LIMIT 16"
            )
        }
        ScenarioKind::UpdateOrderStatus => {
            let order_id =
                ((iteration * variant_stride(11, case.variant)) % dataset.orders as u64) + 1;
            let status = ((iteration * 3 + u64::from(case.variant)) % 5) + 1;
            format!("UPDATE {orders} SET status = {status} WHERE id = {order_id}")
        }
        ScenarioKind::UpdateProductStock => {
            let tenant_id = ((iteration * variant_stride(5, case.variant)) % TENANT_COUNT) + 1;
            let category_id =
                ((iteration * variant_stride(13, case.variant)) % CATEGORY_COUNT) + 1;
            format!(
                "UPDATE {products} SET stock_qty = stock_qty - 1 WHERE tenant_id = {tenant_id} AND category_id = {category_id} AND stock_qty > 0"
            )
        }
        ScenarioKind::VectorTopK => {
            let q = query_vector(iteration + u64::from(case.variant) * 97);
            let limit = vector_limit(case.variant);
            format!(
                "SELECT id, category_id, \
                 ((embedding_0 - {q0}) * (embedding_0 - {q0}) + \
                  (embedding_1 - {q1}) * (embedding_1 - {q1}) + \
                  (embedding_2 - {q2}) * (embedding_2 - {q2}) + \
                  (embedding_3 - {q3}) * (embedding_3 - {q3})) AS dist \
                 FROM {products} ORDER BY dist LIMIT {limit}",
                q0 = q[0],
                q1 = q[1],
                q2 = q[2],
                q3 = q[3]
            )
        }
        ScenarioKind::HybridVectorFilter => {
            let tenant_id = ((iteration * variant_stride(5, case.variant)) % TENANT_COUNT) + 1;
            let category_id =
                ((iteration * variant_stride(13, case.variant)) % CATEGORY_COUNT) + 1;
            let q = query_vector(iteration + u64::from(case.variant) * 131);
            let limit = vector_limit(case.variant);
            format!(
                "SELECT id, category_id, \
                 ((embedding_0 - {q0}) * (embedding_0 - {q0}) + \
                  (embedding_1 - {q1}) * (embedding_1 - {q1}) + \
                  (embedding_2 - {q2}) * (embedding_2 - {q2}) + \
                  (embedding_3 - {q3}) * (embedding_3 - {q3})) AS dist \
                 FROM {products} WHERE tenant_id = {tenant_id} AND category_id = {category_id} ORDER BY dist LIMIT {limit}",
                q0 = q[0],
                q1 = q[1],
                q2 = q[2],
                q3 = q[3]
            )
        }
    }
}

fn surreal_sql(case: &CaseSpec, iteration: u64, dataset: &DatasetSpec) -> String {
    match case.kind {
        ScenarioKind::InsertAppend => {
            let id = append_id(iteration + u64::from(case.variant) * 1_000_000);
            let customer_id =
                ((iteration * variant_stride(17, case.variant)) % dataset.customers as u64) + 1;
            let tenant_id = tenant_for(customer_id);
            let total_cents = 1_000 + (scramble(iteration ^ u64::from(case.variant)) % 150_000);
            format!(
                "CREATE bench_append:{id} SET id = {id}, tenant_id = {tenant_id}, customer_id = {customer_id}, total_cents = {total_cents}, title = 'append-{}-{id}'",
                case.variant
            )
        }
        ScenarioKind::PointCustomer => {
            let id = ((iteration * variant_stride(17, case.variant)) % dataset.customers as u64) + 1;
            format!("SELECT name, credit_score FROM customers:{id}")
        }
        ScenarioKind::RangeOrders => {
            let low = 1_000 + ((iteration * variant_stride(97, case.variant)) % 160_000);
            let high = low + variant_span(case.variant);
            let limit = variant_limit(case.variant, 25, 25, 8);
            format!(
                "SELECT id, total_cents, status FROM orders WHERE total_cents >= {low} AND total_cents < {high} ORDER BY total_cents, id LIMIT {limit}"
            )
        }
        ScenarioKind::TenantOrderRollup => match case.variant % 4 {
            1 => "SELECT tenant_id, count() AS order_count, math::sum(total_cents) AS gross FROM orders GROUP BY tenant_id ORDER BY tenant_id".to_owned(),
            2 => "SELECT status, count() AS order_count, math::mean(total_cents) AS avg_ticket FROM orders GROUP BY status ORDER BY status".to_owned(),
            3 => {
                let day = 30 + u64::from(case.variant) * 5;
                format!("SELECT tenant_id, status, count() AS order_count, math::sum(total_cents) AS gross FROM orders WHERE created_day <= {day} GROUP BY tenant_id, status ORDER BY tenant_id, status")
            }
            _ => {
                let tenant_id = ((iteration * variant_stride(3, case.variant)) % TENANT_COUNT) + 1;
                format!("SELECT status, count() AS order_count, math::sum(total_cents) AS gross FROM orders WHERE tenant_id = {tenant_id} GROUP BY status ORDER BY status")
            }
        }
        ScenarioKind::JoinCustomerOrders => {
            let customer_id =
                ((iteration * variant_stride(31, case.variant)) % dataset.customers as u64) + 1;
            let limit = variant_limit(case.variant, 20, 10, 8);
            format!(
                "SELECT id, total_cents, customer_ref.name AS customer_name FROM orders WHERE customer_id = {customer_id} ORDER BY created_day DESC LIMIT {limit} FETCH customer_ref"
            )
        }
        ScenarioKind::JoinOrderItemsProducts => {
            let order_id =
                ((iteration * variant_stride(41, case.variant)) % dataset.orders as u64) + 1;
            let limit = variant_limit(case.variant, 25, 25, 8);
            let category_id = ((u64::from(case.variant) * 5) % CATEGORY_COUNT) + 1;
            format!(
                "SELECT id, order_id, quantity, product_ref.price_cents AS product_price_cents, category_id FROM line_items WHERE order_id = {order_id} AND category_id >= {category_id} ORDER BY id LIMIT {limit} FETCH product_ref"
            )
        }
        ScenarioKind::RevenueBySegment => {
            let tenant_id = ((iteration * variant_stride(7, case.variant)) % TENANT_COUNT) + 1;
            let threshold = 24 + ((u64::from(case.variant) * 5) % 24);
            format!(
                "SELECT segment_id, count() AS line_count, math::sum(quantity * price_cents * (10000 - discount_bp) / 10000) AS revenue FROM line_items WHERE tenant_id = {tenant_id} AND category_id >= {threshold} GROUP BY segment_id ORDER BY revenue DESC, segment_id LIMIT 16"
            )
        }
        ScenarioKind::UpdateOrderStatus => {
            let order_id =
                ((iteration * variant_stride(11, case.variant)) % dataset.orders as u64) + 1;
            let status = ((iteration * 3 + u64::from(case.variant)) % 5) + 1;
            format!("UPDATE orders:{order_id} SET status = {status}")
        }
        ScenarioKind::UpdateProductStock => {
            let tenant_id = ((iteration * variant_stride(5, case.variant)) % TENANT_COUNT) + 1;
            let category_id =
                ((iteration * variant_stride(13, case.variant)) % CATEGORY_COUNT) + 1;
            format!(
                "UPDATE products SET stock_qty = stock_qty - 1 WHERE tenant_id = {tenant_id} AND category_id = {category_id} AND stock_qty > 0"
            )
        }
        ScenarioKind::VectorTopK => {
            let q = query_vector(iteration + u64::from(case.variant) * 97);
            let limit = vector_limit(case.variant);
            format!(
                "SELECT id, category_id, vector::distance::euclidean(embedding, [{:.6}, {:.6}, {:.6}, {:.6}]) AS dist FROM products ORDER BY dist LIMIT {limit}",
                q[0], q[1], q[2], q[3],
            )
        }
        ScenarioKind::HybridVectorFilter => {
            let tenant_id = ((iteration * variant_stride(5, case.variant)) % TENANT_COUNT) + 1;
            let category_id =
                ((iteration * variant_stride(13, case.variant)) % CATEGORY_COUNT) + 1;
            let q = query_vector(iteration + u64::from(case.variant) * 131);
            let limit = vector_limit(case.variant);
            format!(
                "SELECT id, category_id, vector::distance::euclidean(embedding, [{:.6}, {:.6}, {:.6}, {:.6}]) AS dist FROM products WHERE tenant_id = {tenant_id} AND category_id = {category_id} ORDER BY dist LIMIT {limit}",
                q[0], q[1], q[2], q[3],
            )
        }
    }
}

#[derive(Clone)]
struct CustomerRow {
    id: u64,
    tenant_id: u64,
    region_id: u64,
    segment_id: u64,
    credit_score: u64,
    name: String,
}

#[derive(Clone)]
struct ProductRow {
    id: u64,
    tenant_id: u64,
    category_id: u64,
    price_cents: u64,
    stock_qty: u64,
    embedding: [f64; 4],
}

#[derive(Clone)]
struct OrderRow {
    id: u64,
    customer_id: u64,
    tenant_id: u64,
    status: u64,
    total_cents: u64,
    created_day: u64,
}

#[derive(Clone)]
struct LineItemRow {
    id: u64,
    order_id: u64,
    product_id: u64,
    customer_id: u64,
    tenant_id: u64,
    segment_id: u64,
    category_id: u64,
    quantity: u64,
    price_cents: u64,
    discount_bp: u64,
}

fn customer_row(id: u64) -> CustomerRow {
    let s = scramble(id);
    CustomerRow {
        id,
        tenant_id: tenant_for(id),
        region_id: ((s >> 8) % REGION_COUNT) + 1,
        segment_id: ((s >> 16) % SEGMENT_COUNT) + 1,
        credit_score: 300 + (s % 551),
        name: format!("customer-{id}"),
    }
}

fn product_row(id: u64) -> ProductRow {
    let s = scramble(id ^ 0x9e37_79b9);
    ProductRow {
        id,
        tenant_id: tenant_for(id),
        category_id: ((s >> 6) % CATEGORY_COUNT) + 1,
        price_cents: 500 + (s % 80_000),
        stock_qty: 10 + ((s >> 21) % 2_000),
        embedding: [
            unit_component(s, 0),
            unit_component(s, 1),
            unit_component(s, 2),
            unit_component(s, 3),
        ],
    }
}

fn order_row(id: u64, dataset: &DatasetSpec) -> OrderRow {
    let s = scramble(id ^ 0xa5a5_5a5a);
    let customer_id = ((id * 13 + 7) % dataset.customers as u64) + 1;
    OrderRow {
        id,
        customer_id,
        tenant_id: tenant_for(customer_id),
        status: (s % 5) + 1,
        total_cents: 1_000 + ((s >> 5) % 200_000),
        created_day: (s % 365) + 1,
    }
}

fn line_item_row(id: u64, dataset: &DatasetSpec) -> LineItemRow {
    let order_id = ((id - 1) / dataset.line_items_per_order as u64) + 1;
    let slot = ((id - 1) % dataset.line_items_per_order as u64) + 1;
    let order = order_row(order_id, dataset);
    let customer = customer_row(order.customer_id);
    let product_id = product_id_for(order.tenant_id, order_id, slot, dataset.products as u64);
    let product = product_row(product_id);
    let s = scramble(id ^ 0xf00d_cafe);
    LineItemRow {
        id,
        order_id,
        product_id,
        customer_id: order.customer_id,
        tenant_id: order.tenant_id,
        segment_id: customer.segment_id,
        category_id: product.category_id,
        quantity: (s % 5) + 1,
        price_cents: product.price_cents,
        discount_bp: (s >> 9) % 2_000,
    }
}

fn product_id_for(tenant_id: u64, order_id: u64, slot: u64, total_products: u64) -> u64 {
    let products_per_tenant = ((total_products + TENANT_COUNT - 1) / TENANT_COUNT).max(1);
    let local = scramble(order_id ^ (slot << 8)) % products_per_tenant;
    let mut id = local * TENANT_COUNT + tenant_id;
    if id == 0 {
        id = tenant_id;
    }
    if id > total_products {
        id = tenant_id.min(total_products.max(1));
    }
    id
}

fn query_vector(iteration: u64) -> [f64; 4] {
    let s = scramble(iteration ^ 0x1234_5678);
    [
        unit_component(s, 0),
        unit_component(s, 1),
        unit_component(s, 2),
        unit_component(s, 3),
    ]
}

fn checksum_aion(results: &[AionStatementResult]) -> u64 {
    let mut checksum = DIST_SEED;
    for result in results {
        match result {
            AionStatementResult::Query { rows, .. } => {
                checksum = mix(checksum, rows.len() as u64);
                for row in rows.iter().take(16) {
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

fn checksum_pg(rows: &[SimpleQueryMessage]) -> u64 {
    let mut checksum = DIST_SEED;
    for row in rows {
        match row {
            SimpleQueryMessage::Row(row) => {
                checksum = mix(checksum, row.len() as u64);
                for idx in 0..row.len().min(16) {
                    checksum = mix_str(checksum, row.get(idx).unwrap_or_default());
                }
            }
            other => {
                checksum = mix_str(checksum, &format!("{other:?}"));
            }
        }
    }
    checksum
}

fn record_result(stats: &mut RawStats, elapsed: Duration, result: Result<u64>) {
    match result {
        Ok(checksum) => {
            stats.ops += 1;
            let elapsed_ms = elapsed.as_secs_f64() * 1_000.0;
            stats.latency_sum_ms += elapsed_ms;
            stats.samples_ms.push(elapsed_ms);
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

fn summarize(mut stats: RawStats, elapsed: Duration) -> Summary {
    stats
        .samples_ms
        .sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    let avg_ms = if stats.ops == 0 {
        0.0
    } else {
        stats.latency_sum_ms / stats.ops as f64
    };
    Summary {
        status: if stats.ops > 0 { "OK" } else { "FAIL" }.to_owned(),
        ops: stats.ops,
        ops_per_sec: if elapsed.as_secs_f64() > 0.0 {
            stats.ops as f64 / elapsed.as_secs_f64()
        } else {
            0.0
        },
        avg_ms,
        p95_ms: percentile(&stats.samples_ms, 0.95),
        errors: stats.errors,
        checksum: stats.checksum,
        first_error: stats.first_error,
    }
}

fn percentile(values: &[f64], pct: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let idx = ((values.len() - 1) as f64 * pct).round() as usize;
    values[idx.min(values.len() - 1)]
}

fn skip_summary(reason: String) -> Summary {
    Summary {
        status: "SKIP".to_owned(),
        ops: 0,
        ops_per_sec: 0.0,
        avg_ms: 0.0,
        p95_ms: 0.0,
        errors: 0,
        checksum: 0,
        first_error: Some(reason),
    }
}

fn fail_summary(reason: String) -> Summary {
    Summary {
        status: "FAIL".to_owned(),
        ops: 0,
        ops_per_sec: 0.0,
        avg_ms: 0.0,
        p95_ms: 0.0,
        errors: 1,
        checksum: 0,
        first_error: Some(reason),
    }
}

fn build_ratios(rows: &[ResultRow], cases: &[CaseSpec]) -> BTreeMap<String, serde_json::Value> {
    let mut ratios = BTreeMap::new();
    for case in cases {
        let label = case.label.as_str();
        let aion = ops_for(rows, "aiondb_embedded", label);
        let surreal = ops_for(rows, "surrealdb_embedded_mem", label);
        let postgres = ops_for(rows, "postgresql", label);
        let cockroach = ops_for(rows, "cockroachdb", label);
        ratios.insert(
            case.label.clone(),
            serde_json::json!({
                "aiondb_embedded_ops_per_sec": aion,
                "surrealdb_embedded_mem_ops_per_sec": surreal,
                "postgresql_ops_per_sec": postgres,
                "cockroachdb_ops_per_sec": cockroach,
                "aiondb_vs_postgresql": ratio(aion, postgres),
                "aiondb_vs_cockroachdb": ratio(aion, cockroach),
                "aiondb_vs_surrealdb": ratio(aion, surreal),
            }),
        );
    }
    ratios
}

fn ops_for(rows: &[ResultRow], engine: &str, scenario: &str) -> f64 {
    rows.iter()
        .find(|row| row.engine == engine && row.scenario == scenario)
        .map(|row| row.summary.ops_per_sec)
        .unwrap_or(0.0)
}

fn ratio(left: f64, right: f64) -> Option<f64> {
    if right > 0.0 {
        Some(left / right)
    } else {
        None
    }
}

fn print_score(engine: &str, case: &CaseSpec, summary: &Summary) {
    println!(
        "score\t{}\t{}\t{}\t{:.2} ops/s\tavg {:.3} ms\tp95 {:.3} ms",
        engine, case.label, summary.status, summary.ops_per_sec, summary.avg_ms, summary.p95_ms
    );
    if let Some(error) = &summary.first_error {
        println!("error\t{}\t{}\t{}", engine, case.label, error);
    }
}

fn append_id(iteration: u64) -> u64 {
    1_000_000_000 + iteration
}

fn tenant_for(id: u64) -> u64 {
    ((id - 1) % TENANT_COUNT) + 1
}

fn scramble(mut x: u64) -> u64 {
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
    x ^= x >> 33;
    x = x.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    x ^ (x >> 33)
}

fn unit_component(seed: u64, lane: u32) -> f64 {
    let value = scramble(seed ^ ((lane as u64 + 1) * 0x9e37_79b9_7f4a_7c15));
    (value % 10_000) as f64 / 10_000.0
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

fn run_id() -> String {
    let epoch_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_millis();
    format!("{epoch_ms}-{}", std::process::id())
}

fn default_output_path(run_id: &str) -> PathBuf {
    PathBuf::from(format!("../results/cross-engine-compare-{run_id}.json"))
}
