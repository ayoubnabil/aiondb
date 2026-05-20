//! AionDB vector benchmark harness.
//!
//! Drives the AionDB storage engine through `aiondb_storage_engine` to
//! measure build time, recall, and per-query latency for the three vector
//! index families AionDB ships in v0.3:
//!
//! - HNSW raw (`quantization=none`)
//! - HNSW + Product Quantization (`pq`, m=`dims/8`, k=256)
//! - IVF-flat (`nlist=64`, `nprobe=8`)
//!
//! Brute-force exact ranking provides the recall ground truth. Optional
//! adapters compare against pgvector (`--features pgvector`, env
//! `PGVECTOR_URL`) and Qdrant (`--features qdrant`, env `QDRANT_URL`).
//! Both connectors stay behind feature flags so the default build never
//! requires the external services.

use std::time::{Duration, Instant};

use aiondb_core::{ColumnId, DataType, IndexId, RelationId, Row, Value, VectorElementType};
use aiondb_storage_api::{
    HnswStorageOptions, IndexKeyColumn, IndexStorageDescriptor, IvfFlatStorageOptions,
    StorageColumn, StorageDDL, StorageDML, StoredQuantizationKind, StoredVectorMetric,
    TableStorageDescriptor,
};
use aiondb_storage_engine::InMemoryStorage;
use anyhow::Result;
use serde::Serialize;
use tokio::runtime::Builder;

const DIMS: usize = 96;
const DATASET_SIZE: usize = 5_000;
const QUERY_COUNT: usize = 200;
const TOP_K: usize = 10;

fn main() -> Result<()> {
    let runtime = Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(run())
}

async fn run() -> Result<()> {
    let dataset = build_dataset(DATASET_SIZE, DIMS, 1);
    let queries = build_dataset(QUERY_COUNT, DIMS, 7919);
    let ground_truth = brute_force_top_k(&dataset, &queries, TOP_K);

    let mut report = Report::new(DATASET_SIZE, DIMS, QUERY_COUNT, TOP_K);
    report.add(bench_aiondb_hnsw_raw(&dataset, &queries, &ground_truth)?);
    report.add(bench_aiondb_hnsw_pq(&dataset, &queries, &ground_truth)?);
    report.add(bench_aiondb_ivf_flat(&dataset, &queries, &ground_truth, 64, 8)?);
    report.add(bench_aiondb_ivf_flat(&dataset, &queries, &ground_truth, 64, 32)?);
    report.add(bench_aiondb_brute_force(&dataset, &queries, &ground_truth)?);

    #[cfg(feature = "pgvector")]
    if let Some(url) = std::env::var("PGVECTOR_URL").ok() {
        match pgvector::bench(&url, &dataset, &queries, &ground_truth).await {
            Ok(result) => report.add(result),
            Err(err) => eprintln!("pgvector benchmark skipped: {err}"),
        }
    }
    #[cfg(feature = "qdrant")]
    if let Some(url) = std::env::var("QDRANT_URL").ok() {
        match qdrant::bench(&url, &dataset, &queries, &ground_truth).await {
            Ok(result) => report.add(result),
            Err(err) => eprintln!("qdrant benchmark skipped: {err}"),
        }
    }

    report.render_table();
    report.emit_json();
    Ok(())
}

// ---------------------------------------------------------------------------
// Synthetic dataset + ground truth
// ---------------------------------------------------------------------------

fn build_dataset(count: usize, dims: usize, seed_offset: u64) -> Vec<Vec<f32>> {
    (0..count)
        .map(|i| deterministic_vector((i as u64) + seed_offset, dims))
        .collect()
}

fn deterministic_vector(seed: u64, dims: usize) -> Vec<f32> {
    let mut state = seed
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(0xBF58_476D_1CE4_E5B9);
    (0..dims)
        .map(|_| {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            let sample = state.wrapping_mul(0x2545_F491_4F6C_DD1D);
            let unit = ((sample >> 40) as f32) / ((1u32 << 24) as f32);
            unit * 2.0 - 1.0
        })
        .collect()
}

fn brute_force_top_k(
    dataset: &[Vec<f32>],
    queries: &[Vec<f32>],
    k: usize,
) -> Vec<Vec<usize>> {
    queries
        .iter()
        .map(|q| {
            let mut scored: Vec<(f32, usize)> = dataset
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    let d: f32 = v
                        .iter()
                        .zip(q.iter())
                        .map(|(a, b)| (a - b).powi(2))
                        .sum();
                    (d, i)
                })
                .collect();
            scored.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
            scored.into_iter().take(k).map(|(_, i)| i).collect()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Report + timing helpers
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize)]
struct BackendResult {
    backend: String,
    build_ms: u128,
    recall_at_k: f64,
    p50_us: u128,
    p95_us: u128,
    p99_us: u128,
    mean_us: u128,
}

#[derive(Debug, Serialize)]
struct Report {
    dataset_size: usize,
    dims: usize,
    queries: usize,
    top_k: usize,
    results: Vec<BackendResult>,
}

impl Report {
    fn new(dataset_size: usize, dims: usize, queries: usize, top_k: usize) -> Self {
        Self {
            dataset_size,
            dims,
            queries,
            top_k,
            results: Vec::new(),
        }
    }

    fn add(&mut self, result: BackendResult) {
        self.results.push(result);
    }

    fn render_table(&self) {
        println!(
            "\nvector-compare  (n={}  d={}  queries={}  k={})\n",
            self.dataset_size, self.dims, self.queries, self.top_k
        );
        println!(
            "{:<28} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10}",
            "backend", "build_ms", "recall@k", "mean_us", "p50_us", "p95_us", "p99_us"
        );
        println!("{}", "-".repeat(94));
        for result in &self.results {
            println!(
                "{:<28} {:>10} {:>10.3} {:>10} {:>10} {:>10} {:>10}",
                result.backend,
                result.build_ms,
                result.recall_at_k,
                result.mean_us,
                result.p50_us,
                result.p95_us,
                result.p99_us
            );
        }
    }

    fn emit_json(&self) {
        if std::env::var("EMIT_JSON").is_ok() {
            println!(
                "\n{}",
                serde_json::to_string_pretty(self).expect("json serialization")
            );
        }
    }
}

fn percentile(samples: &mut Vec<u128>, p: f64) -> u128 {
    if samples.is_empty() {
        return 0;
    }
    samples.sort_unstable();
    let idx = ((samples.len() as f64 - 1.0) * p).round() as usize;
    samples[idx]
}

fn percentiles(latencies: &[Duration]) -> (u128, u128, u128, u128) {
    let mut micros: Vec<u128> = latencies.iter().map(|d| d.as_micros()).collect();
    let mean = if micros.is_empty() {
        0
    } else {
        micros.iter().sum::<u128>() / (micros.len() as u128)
    };
    let p50 = percentile(&mut micros, 0.50);
    let p95 = percentile(&mut micros, 0.95);
    let p99 = percentile(&mut micros, 0.99);
    (mean, p50, p95, p99)
}

fn recall(actual: &[Vec<usize>], expected: &[Vec<usize>]) -> f64 {
    if expected.is_empty() {
        return 0.0;
    }
    let mut hits = 0usize;
    let mut total = 0usize;
    for (a, e) in actual.iter().zip(expected.iter()) {
        let a_set: std::collections::BTreeSet<_> = a.iter().copied().collect();
        for id in e {
            if a_set.contains(id) {
                hits += 1;
            }
            total += 1;
        }
    }
    hits as f64 / total as f64
}

// ---------------------------------------------------------------------------
// AionDB benchmarks
// ---------------------------------------------------------------------------

fn make_table_descriptor(table_id: RelationId, dims: usize) -> TableStorageDescriptor {
    TableStorageDescriptor {
        table_id,
        columns: vec![
            StorageColumn {
                column_id: ColumnId::new(1),
                data_type: DataType::Int,
                nullable: false,
            },
            StorageColumn {
                column_id: ColumnId::new(2),
                data_type: DataType::Vector {
                    dims: dims as u32,
                    element_type: VectorElementType::Float32,
                },
                nullable: false,
            },
        ],
        primary_key: None,
        shard_config: None,
    }
}

fn make_index_descriptor(
    index_id: IndexId,
    table_id: RelationId,
    hnsw_options: Option<HnswStorageOptions>,
    ivf_flat_options: Option<IvfFlatStorageOptions>,
) -> IndexStorageDescriptor {
    IndexStorageDescriptor {
        index_id,
        table_id,
        unique: false,
        nulls_not_distinct: false,
        gin: false,
        key_columns: vec![IndexKeyColumn {
            column_id: ColumnId::new(2),
            descending: false,
            nulls_first: false,
        }],
        include_columns: vec![],
        hnsw_options,
        ivf_flat_options,
    }
}

fn insert_dataset(
    storage: &InMemoryStorage,
    table_id: RelationId,
    dataset: &[Vec<f32>],
    dims: usize,
) -> Result<()> {
    use aiondb_core::TxnId;
    use aiondb_core::VectorValue;
    for (i, vector) in dataset.iter().enumerate() {
        let row = Row::new(vec![
            Value::Int((i as i32) + 1),
            Value::Vector(VectorValue {
                dims: dims as u32,
                values: vector.clone(),
            }),
        ]);
        storage.insert(TxnId::default(), table_id, row)?;
    }
    Ok(())
}

fn search_actual_indices(
    storage: &InMemoryStorage,
    index_id: IndexId,
    queries: &[Vec<f32>],
    _ef: usize,
) -> Result<(Vec<Vec<usize>>, Vec<Duration>)> {
    use aiondb_core::TxnId;
    use aiondb_tx::Snapshot;
    let mut actual = Vec::with_capacity(queries.len());
    let mut latencies = Vec::with_capacity(queries.len());
    let snapshot = Snapshot::new(TxnId::default(), TxnId::default(), Vec::new());
    for query in queries {
        let start = Instant::now();
        let mut stream = StorageDML::vector_search(
            storage,
            TxnId::default(),
            &snapshot,
            index_id,
            query,
            TOP_K,
            128,
            None,
            None,
            None,
        )?;
        let mut row_ids = Vec::with_capacity(TOP_K);
        while let Some(record) = stream.next()? {
            if let Some(Value::Int(id)) = record.row.values.first() {
                let zero_based = (*id - 1).max(0) as usize;
                row_ids.push(zero_based);
            }
        }
        latencies.push(start.elapsed());
        actual.push(row_ids);
    }
    Ok((actual, latencies))
}

fn bench_aiondb_hnsw_raw(
    dataset: &[Vec<f32>],
    queries: &[Vec<f32>],
    ground_truth: &[Vec<usize>],
) -> Result<BackendResult> {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(1);
    let index_id = IndexId::new(101);
    storage.create_table_storage(
        aiondb_core::TxnId::default(),
        &make_table_descriptor(table_id, DIMS),
    )?;
    insert_dataset(&storage, table_id, dataset, DIMS)?;
    let build_start = Instant::now();
    storage.create_index_storage(
        aiondb_core::TxnId::default(),
        &make_index_descriptor(
            index_id,
            table_id,
            Some(HnswStorageOptions {
                m: 16,
                ef_construction: 100,
                distance_metric: StoredVectorMetric::L2,
                quantization: StoredQuantizationKind::None,
                prenormalised: false,
            }),
            None,
        ),
    )?;
    let build_ms = build_start.elapsed().as_millis();
    let (actual, latencies) = search_actual_indices(&storage, index_id, queries, 128)?;
    let (mean, p50, p95, p99) = percentiles(&latencies);
    Ok(BackendResult {
        backend: "aiondb hnsw (raw)".to_owned(),
        build_ms,
        recall_at_k: recall(&actual, ground_truth),
        p50_us: p50,
        p95_us: p95,
        p99_us: p99,
        mean_us: mean,
    })
}

fn bench_aiondb_hnsw_pq(
    dataset: &[Vec<f32>],
    queries: &[Vec<f32>],
    ground_truth: &[Vec<usize>],
) -> Result<BackendResult> {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(1);
    let index_id = IndexId::new(102);
    storage.create_table_storage(
        aiondb_core::TxnId::default(),
        &make_table_descriptor(table_id, DIMS),
    )?;
    insert_dataset(&storage, table_id, dataset, DIMS)?;
    let build_start = Instant::now();
    storage.create_index_storage(
        aiondb_core::TxnId::default(),
        &make_index_descriptor(
            index_id,
            table_id,
            Some(HnswStorageOptions {
                m: 16,
                ef_construction: 100,
                distance_metric: StoredVectorMetric::L2,
                quantization: StoredQuantizationKind::Product,
                prenormalised: false,
            }),
            None,
        ),
    )?;
    let build_ms = build_start.elapsed().as_millis();
    let (actual, latencies) = search_actual_indices(&storage, index_id, queries, 128)?;
    let (mean, p50, p95, p99) = percentiles(&latencies);
    Ok(BackendResult {
        backend: "aiondb hnsw (pq)".to_owned(),
        build_ms,
        recall_at_k: recall(&actual, ground_truth),
        p50_us: p50,
        p95_us: p95,
        p99_us: p99,
        mean_us: mean,
    })
}

fn bench_aiondb_ivf_flat(
    dataset: &[Vec<f32>],
    queries: &[Vec<f32>],
    ground_truth: &[Vec<usize>],
    nlist: u32,
    nprobe: u32,
) -> Result<BackendResult> {
    let storage = InMemoryStorage::new_without_wal();
    let table_id = RelationId::new(1);
    let index_id = IndexId::new(103);
    storage.create_table_storage(
        aiondb_core::TxnId::default(),
        &make_table_descriptor(table_id, DIMS),
    )?;
    insert_dataset(&storage, table_id, dataset, DIMS)?;
    let build_start = Instant::now();
    storage.create_index_storage(
        aiondb_core::TxnId::default(),
        &make_index_descriptor(
            index_id,
            table_id,
            None,
            Some(IvfFlatStorageOptions {
                nlist,
                nprobe,
                distance_metric: StoredVectorMetric::L2,
            }),
        ),
    )?;
    let build_ms = build_start.elapsed().as_millis();
    let (actual, latencies) = search_actual_indices(&storage, index_id, queries, 128)?;
    let (mean, p50, p95, p99) = percentiles(&latencies);
    Ok(BackendResult {
        backend: format!("aiondb ivf-flat (nlist={nlist},nprobe={nprobe})"),
        build_ms,
        recall_at_k: recall(&actual, ground_truth),
        p50_us: p50,
        p95_us: p95,
        p99_us: p99,
        mean_us: mean,
    })
}

fn bench_aiondb_brute_force(
    dataset: &[Vec<f32>],
    queries: &[Vec<f32>],
    ground_truth: &[Vec<usize>],
) -> Result<BackendResult> {
    let mut latencies = Vec::with_capacity(queries.len());
    let mut actual = Vec::with_capacity(queries.len());
    for q in queries {
        let start = Instant::now();
        let mut scored: Vec<(f32, usize)> = dataset
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let d: f32 = v
                    .iter()
                    .zip(q.iter())
                    .map(|(a, b)| (a - b).powi(2))
                    .sum();
                (d, i)
            })
            .collect();
        scored.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        latencies.push(start.elapsed());
        actual.push(scored.into_iter().take(TOP_K).map(|(_, i)| i).collect());
    }
    let (mean, p50, p95, p99) = percentiles(&latencies);
    Ok(BackendResult {
        backend: "brute-force (exact)".to_owned(),
        build_ms: 0,
        recall_at_k: recall(&actual, ground_truth),
        p50_us: p50,
        p95_us: p95,
        p99_us: p99,
        mean_us: mean,
    })
}

// ---------------------------------------------------------------------------
// pgvector adapter (optional)
// ---------------------------------------------------------------------------

#[cfg(feature = "pgvector")]
mod pgvector {
    use super::*;
    use tokio_postgres::NoTls;

    pub async fn bench(
        url: &str,
        dataset: &[Vec<f32>],
        queries: &[Vec<f32>],
        ground_truth: &[Vec<usize>],
    ) -> Result<BackendResult> {
        let (client, conn) = tokio_postgres::connect(url, NoTls).await?;
        tokio::spawn(async move {
            if let Err(err) = conn.await {
                eprintln!("pgvector connection error: {err}");
            }
        });

        client
            .batch_execute("CREATE EXTENSION IF NOT EXISTS vector")
            .await?;
        client
            .batch_execute("DROP TABLE IF EXISTS aiondb_vector_compare")
            .await?;
        let create = format!(
            "CREATE TABLE aiondb_vector_compare (id BIGINT PRIMARY KEY, embedding vector({}))",
            DIMS
        );
        client.batch_execute(&create).await?;

        // pgvector ships its own vector type that tokio-postgres doesn't
        // understand natively; we sidestep the OID mapping by inlining the
        // vector literal into the SQL string. The values are
        // synthesized locally and are not user input.
        for (i, vector) in dataset.iter().enumerate() {
            let literal = vector_literal(vector);
            let stmt = format!(
                "INSERT INTO aiondb_vector_compare VALUES ({}, '{}'::vector)",
                (i as i64) + 1,
                literal
            );
            client.batch_execute(&stmt).await?;
        }

        let build_start = Instant::now();
        client
            .batch_execute(
                "CREATE INDEX aiondb_vector_compare_hnsw_idx ON aiondb_vector_compare \
                 USING hnsw (embedding vector_l2_ops) WITH (m = 16, ef_construction = 100)",
            )
            .await?;
        let build_ms = build_start.elapsed().as_millis();
        // Match the query-time breadth AionDB and Qdrant use so recall
        // comparisons aren't capped by pgvector's default ef_search=40.
        client
            .batch_execute("SET hnsw.ef_search = 128")
            .await?;

        let mut latencies = Vec::with_capacity(queries.len());
        let mut actual = Vec::with_capacity(queries.len());
        for query in queries {
            let literal = vector_literal(query);
            let sql = format!(
                "SELECT id FROM aiondb_vector_compare \
                 ORDER BY embedding <-> '{}'::vector LIMIT {}",
                literal, TOP_K
            );
            let start = Instant::now();
            let rows = client.query(&sql, &[]).await?;
            latencies.push(start.elapsed());
            let ids: Vec<usize> = rows
                .iter()
                .map(|row| (row.get::<_, i64>(0) - 1) as usize)
                .collect();
            actual.push(ids);
        }
        let (mean, p50, p95, p99) = percentiles(&latencies);
        Ok(BackendResult {
            backend: "pgvector hnsw".to_owned(),
            build_ms,
            recall_at_k: recall(&actual, ground_truth),
            p50_us: p50,
            p95_us: p95,
            p99_us: p99,
            mean_us: mean,
        })
    }

    fn vector_literal(vector: &[f32]) -> String {
        let body = vector
            .iter()
            .map(|v| format!("{:.6}", v))
            .collect::<Vec<_>>()
            .join(",");
        format!("[{}]", body)
    }
}

// ---------------------------------------------------------------------------
// Qdrant adapter (optional)
// ---------------------------------------------------------------------------

#[cfg(feature = "qdrant")]
mod qdrant {
    use super::*;
    use serde_json::json;

    pub async fn bench(
        base_url: &str,
        dataset: &[Vec<f32>],
        queries: &[Vec<f32>],
        ground_truth: &[Vec<usize>],
    ) -> Result<BackendResult> {
        let collection = "aiondb_vector_compare";
        let client = reqwest::Client::new();
        let _ = client
            .delete(format!("{base_url}/collections/{collection}"))
            .send()
            .await;

        let create_resp = client
            .put(format!("{base_url}/collections/{collection}"))
            .json(&json!({
                "vectors": {
                    "size": DIMS,
                    "distance": "Euclid",
                },
            }))
            .send()
            .await?;
        if !create_resp.status().is_success() {
            return Err(anyhow::anyhow!(
                "Qdrant create collection failed: {}",
                create_resp.status()
            ));
        }

        let build_start = Instant::now();
        // Qdrant indexes vectors as they arrive; treat the upsert wall time
        // as the build cost for parity with pgvector / aiondb.
        let points: Vec<serde_json::Value> = dataset
            .iter()
            .enumerate()
            .map(|(i, v)| {
                json!({
                    "id": i as u64 + 1,
                    "vector": v,
                })
            })
            .collect();
        let upsert_resp = client
            .put(format!(
                "{base_url}/collections/{collection}/points?wait=true"
            ))
            .json(&json!({ "points": points }))
            .send()
            .await?;
        if !upsert_resp.status().is_success() {
            return Err(anyhow::anyhow!(
                "Qdrant upsert failed: {}",
                upsert_resp.status()
            ));
        }
        let build_ms = build_start.elapsed().as_millis();

        let mut latencies = Vec::with_capacity(queries.len());
        let mut actual = Vec::with_capacity(queries.len());
        for query in queries {
            let body = json!({
                "vector": query,
                "limit": TOP_K,
                "with_payload": false,
                "with_vector": false,
            });
            let start = Instant::now();
            let resp = client
                .post(format!("{base_url}/collections/{collection}/points/search"))
                .json(&body)
                .send()
                .await?
                .error_for_status()?;
            let parsed: serde_json::Value = resp.json().await?;
            latencies.push(start.elapsed());
            let ids: Vec<usize> = parsed
                .get("result")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|hit| hit.get("id").and_then(|v| v.as_u64()))
                        .map(|id| (id as usize).saturating_sub(1))
                        .collect()
                })
                .unwrap_or_default();
            actual.push(ids);
        }
        let (mean, p50, p95, p99) = percentiles(&latencies);
        Ok(BackendResult {
            backend: "qdrant hnsw".to_owned(),
            build_ms,
            recall_at_k: recall(&actual, ground_truth),
            p50_us: p50,
            p95_us: p95,
            p99_us: p99,
            mean_us: mean,
        })
    }
}
