use super::*;
use aiondb_core::{ColumnId, IndexId, RelationId, VectorValue};
use aiondb_storage_api::{
    IndexKeyColumn as StorageKeyColumn, IndexStorageDescriptor, StorageColumn,
    TableStorageDescriptor,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

fn recall_wall_bound() -> Duration {
    let base = Duration::from_secs(10);
    let multiplier = std::env::var("AIONDB_PERF_BUDGET_MULTIPLIER")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(1.0)
        .clamp(0.25, 4.0);

    Duration::from_secs_f64(base.as_secs_f64() * multiplier)
}

fn make_table_desc() -> TableStorageDescriptor {
    make_table_desc_with_dims(3)
}

fn make_table_desc_with_dims(dims: u32) -> TableStorageDescriptor {
    TableStorageDescriptor {
        table_id: RelationId::new(1),
        columns: vec![
            StorageColumn {
                column_id: ColumnId::new(1),
                data_type: aiondb_core::DataType::Int,
                nullable: false,
            },
            StorageColumn {
                column_id: ColumnId::new(2),
                data_type: aiondb_core::DataType::Vector {
                    dims,
                    element_type: aiondb_core::VectorElementType::Float32,
                },
                nullable: false,
            },
        ],
        primary_key: Some(vec![ColumnId::new(1)]),
        shard_config: None,
    }
}

fn make_index_desc() -> IndexStorageDescriptor {
    IndexStorageDescriptor {
        index_id: IndexId::new(100),
        table_id: RelationId::new(1),
        unique: false,
        nulls_not_distinct: false,
        gin: false,
        key_columns: vec![StorageKeyColumn {
            column_id: ColumnId::new(2),
            descending: false,
            nulls_first: false,
        }],
        include_columns: vec![],
        hnsw_options: None,
    }
}

fn make_row(id: i32, vector: Vec<f32>) -> Row {
    Row::new(vec![
        Value::Int(id),
        Value::Vector(VectorValue {
            dims: vector.len() as u32,
            values: vector,
        }),
    ])
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

fn brute_force_top_k(dataset: &[Vec<f32>], query: &[f32], k: usize) -> Vec<TupleId> {
    let mut scored = dataset
        .iter()
        .enumerate()
        .map(|(idx, vector)| {
            let distance = vector
                .iter()
                .zip(query.iter())
                .map(|(left, right)| {
                    let diff = left - right;
                    diff * diff
                })
                .sum::<f32>();
            (distance, TupleId::new(idx as u64 + 1))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|(left_distance, left_id), (right_distance, right_id)| {
        left_distance
            .total_cmp(right_distance)
            .then_with(|| left_id.cmp(right_id))
    });
    scored
        .into_iter()
        .take(k)
        .map(|(_, tuple_id)| tuple_id)
        .collect()
}

#[test]
fn insert_and_search_basic() {
    let table_desc = make_table_desc();
    let index_desc = make_index_desc();
    let mut index = HnswIndex::new(index_desc, 4, 20);

    let rows = vec![
        (TupleId::new(1), make_row(1, vec![1.0, 0.0, 0.0])),
        (TupleId::new(2), make_row(2, vec![0.0, 1.0, 0.0])),
        (TupleId::new(3), make_row(3, vec![0.0, 0.0, 1.0])),
        (TupleId::new(4), make_row(4, vec![1.0, 1.0, 0.0])),
        (TupleId::new(5), make_row(5, vec![0.0, 1.0, 1.0])),
    ];

    for (tid, row) in &rows {
        index.insert_tuple(&table_desc, *tid, row).unwrap();
    }

    // Search for nearest to [1.0, 0.0, 0.0] - should find TupleId(1) first.
    let (results, _stats) = index.search(&[1.0, 0.0, 0.0], 3, 10);
    assert!(!results.is_empty());
    assert_eq!(results[0], TupleId::new(1));
}

#[test]
fn remove_tuple_works() {
    let table_desc = make_table_desc();
    let index_desc = make_index_desc();
    let mut index = HnswIndex::new(index_desc, 4, 20);

    let row1 = make_row(1, vec![1.0, 0.0, 0.0]);
    let row2 = make_row(2, vec![0.0, 1.0, 0.0]);

    index
        .insert_tuple(&table_desc, TupleId::new(1), &row1)
        .unwrap();
    index
        .insert_tuple(&table_desc, TupleId::new(2), &row2)
        .unwrap();

    index
        .remove_tuple(&table_desc, TupleId::new(1), &row1)
        .unwrap();

    let (results, _stats) = index.search(&[1.0, 0.0, 0.0], 5, 10);
    assert!(!results.contains(&TupleId::new(1)));
    assert!(results.contains(&TupleId::new(2)));
}

#[test]
fn empty_index_search_returns_empty() {
    let index_desc = make_index_desc();
    let index = HnswIndex::new(index_desc, 4, 20);
    let (results, _stats) = index.search(&[1.0, 0.0, 0.0], 5, 10);
    assert!(results.is_empty());
}

#[test]
fn from_rows_builds_index() {
    let table_desc = make_table_desc();
    let index_desc = make_index_desc();

    let rows = vec![
        (TupleId::new(1), make_row(1, vec![1.0, 0.0, 0.0])),
        (TupleId::new(2), make_row(2, vec![0.0, 1.0, 0.0])),
        (TupleId::new(3), make_row(3, vec![0.0, 0.0, 1.0])),
    ];

    let index = HnswIndex::from_rows(&index_desc, &table_desc, 4, 20, rows).unwrap();
    let (results, _stats) = index.search(&[0.0, 1.0, 0.0], 1, 10);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0], TupleId::new(2));
}

#[test]
fn search_returns_stats() {
    let table_desc = make_table_desc();
    let index_desc = make_index_desc();
    let mut index = HnswIndex::new(index_desc, 4, 20);

    let rows = vec![
        (TupleId::new(1), make_row(1, vec![1.0, 0.0, 0.0])),
        (TupleId::new(2), make_row(2, vec![0.0, 1.0, 0.0])),
        (TupleId::new(3), make_row(3, vec![0.0, 0.0, 1.0])),
        (TupleId::new(4), make_row(4, vec![1.0, 1.0, 0.0])),
        (TupleId::new(5), make_row(5, vec![0.0, 1.0, 1.0])),
    ];

    for (tid, row) in &rows {
        index.insert_tuple(&table_desc, *tid, row).unwrap();
    }

    let (_results, stats) = index.search(&[1.0, 0.0, 0.0], 3, 10);
    assert!(stats.nodes_visited > 0, "nodes_visited should be non-zero");
    assert!(
        stats.distance_computations > 0,
        "distance_computations should be non-zero"
    );
    assert!(!stats.truncated, "search should not be truncated");
}

#[test]
fn search_stats_track_distance_computations() {
    let table_desc = make_table_desc();
    let index_desc_small = make_index_desc();
    let index_desc_large = IndexStorageDescriptor {
        index_id: IndexId::new(101),
        ..make_index_desc()
    };

    // Small index: 3 vectors.
    let mut small_index = HnswIndex::new(index_desc_small, 4, 20);
    for i in 1..=3u32 {
        let v = vec![i as f32, 0.0, 0.0];
        small_index
            .insert_tuple(
                &table_desc,
                TupleId::new(u64::from(i)),
                &make_row(i as i32, v),
            )
            .unwrap();
    }

    // Large index: 20 vectors.
    let mut large_index = HnswIndex::new(index_desc_large, 4, 20);
    for i in 1..=20u32 {
        let v = vec![i as f32, (i as f32).sin(), (i as f32).cos()];
        large_index
            .insert_tuple(
                &table_desc,
                TupleId::new(u64::from(i)),
                &make_row(i as i32, v),
            )
            .unwrap();
    }

    let (_r1, stats_small) = small_index.search(&[0.0, 0.0, 0.0], 3, 10);
    let (_r2, stats_large) = large_index.search(&[0.0, 0.0, 0.0], 3, 10);

    assert!(
        stats_large.distance_computations >= stats_small.distance_computations,
        "larger index should compute at least as many distances: large={} small={}",
        stats_large.distance_computations,
        stats_small.distance_computations,
    );
}

#[test]
fn recall_at_k_stays_high_against_bruteforce() {
    let dims = 64usize;
    let dataset_size = 10_000usize;
    let query_count = 200usize;
    let k = 10usize;
    let ef = 200usize;
    let m = 12u32;
    let ef_construction = 64u32;

    let table_desc = make_table_desc_with_dims(dims as u32);
    let index_desc = make_index_desc();
    let dataset = (0..dataset_size)
        .map(|idx| deterministic_vector(idx as u64 + 1, dims))
        .collect::<Vec<_>>();
    let rows = dataset
        .iter()
        .enumerate()
        .map(|(idx, vector)| {
            (
                TupleId::new(idx as u64 + 1),
                make_row(idx as i32 + 1, vector.clone()),
            )
        })
        .collect::<Vec<_>>();

    let index = HnswIndex::from_rows(&index_desc, &table_desc, m, ef_construction, rows).unwrap();

    let wall_start = Instant::now();
    let mut total_hits = 0usize;

    for query_idx in 0..query_count {
        let query = &dataset[(query_idx * 47) % dataset_size];
        let expected = brute_force_top_k(&dataset, query, k);
        let (actual, stats) = index.search(query, k, ef);

        assert!(!stats.truncated, "recall run should not truncate searches");

        let actual_set = actual
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        total_hits += expected
            .into_iter()
            .filter(|tuple_id| actual_set.contains(tuple_id))
            .count();
    }

    let recall = total_hits as f64 / (query_count * k) as f64;
    let wall_elapsed = wall_start.elapsed();
    assert!(
        wall_elapsed < recall_wall_bound(),
        "recall test took too long: {wall_elapsed:?}"
    );

    assert!(
        recall >= 0.95,
        "expected recall@{k} >= 0.95, got {recall:.3}"
    );
}

#[test]
fn summary_accumulates_across_searches() {
    let table_desc = make_table_desc();
    let index_desc = make_index_desc();
    let mut index = HnswIndex::new(index_desc, 4, 20);

    for i in 1..=10u32 {
        let v = vec![i as f32, 0.0, 0.0];
        index
            .insert_tuple(
                &table_desc,
                TupleId::new(u64::from(i)),
                &make_row(i as i32, v),
            )
            .unwrap();
    }

    // Perform multiple searches.
    let (_r1, s1) = index.search(&[0.0, 0.0, 0.0], 3, 10);
    let (_r2, s2) = index.search(&[5.0, 0.0, 0.0], 3, 10);
    let (_r3, s3) = index.search(&[10.0, 0.0, 0.0], 3, 10);

    let summary = index.search_stats_summary();
    assert_eq!(summary.total_searches, 3);
    assert_eq!(
        summary.total_nodes_visited,
        s1.nodes_visited + s2.nodes_visited + s3.nodes_visited,
    );
    assert_eq!(
        summary.total_distance_computations,
        s1.distance_computations + s2.distance_computations + s3.distance_computations,
    );
    assert_eq!(summary.node_count, 10);
    assert!(summary.layer_count > 0);
}

// --- Task A: Memory budget tests ---

#[test]
fn memory_usage_bytes_reports_nonzero() {
    let table_desc = make_table_desc();
    let index_desc = make_index_desc();
    let mut index = HnswIndex::new(index_desc, 4, 20);

    assert_eq!(index.memory_usage_bytes(), 0);

    index
        .insert_tuple(
            &table_desc,
            TupleId::new(1),
            &make_row(1, vec![1.0, 0.0, 0.0]),
        )
        .unwrap();

    assert!(index.memory_usage_bytes() > 0);
}

#[test]
fn memory_budget_blocks_insert() {
    let table_desc = make_table_desc();
    let index_desc = make_index_desc();
    let mut index = HnswIndex::new(index_desc, 4, 20);

    // Set a very small budget.
    index.set_memory_budget(Some(1));

    let result = index.insert_tuple(
        &table_desc,
        TupleId::new(1),
        &make_row(1, vec![1.0, 0.0, 0.0]),
    );
    assert!(
        result.is_err(),
        "insert should fail when budget is exceeded"
    );
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("memory budget exceeded"),
        "error should mention budget: {err_msg}"
    );
}

#[test]
fn memory_budget_allows_insert_within_budget() {
    let table_desc = make_table_desc();
    let index_desc = make_index_desc();
    let mut index = HnswIndex::new(index_desc, 4, 20);

    // Set a generous budget (1 MB).
    index.set_memory_budget(Some(1_000_000));

    for i in 1..=10u32 {
        index
            .insert_tuple(
                &table_desc,
                TupleId::new(u64::from(i)),
                &make_row(i as i32, vec![i as f32, 0.0, 0.0]),
            )
            .unwrap();
    }

    assert!(index.memory_usage_bytes() > 0);
    assert!(index.memory_usage_bytes() < 1_000_000);
}

#[test]
fn memory_budget_remaining_reports_correctly() {
    let table_desc = make_table_desc();
    let index_desc = make_index_desc();
    let mut index = HnswIndex::new(index_desc, 4, 20);

    // No budget set: should return None.
    assert!(index.memory_budget_remaining().is_none());

    index.set_memory_budget(Some(100_000));

    let remaining_before = index.memory_budget_remaining().unwrap();
    assert_eq!(remaining_before, 100_000);

    index
        .insert_tuple(
            &table_desc,
            TupleId::new(1),
            &make_row(1, vec![1.0, 0.0, 0.0]),
        )
        .unwrap();

    let remaining_after = index.memory_budget_remaining().unwrap();
    assert!(remaining_after < remaining_before);
}

// --- Task B: Latency budget tests ---

#[test]
fn search_with_generous_deadline_returns_results() {
    let table_desc = make_table_desc();
    let index_desc = make_index_desc();
    let mut index = HnswIndex::new(index_desc, 4, 20);

    for i in 1..=5u32 {
        index
            .insert_tuple(
                &table_desc,
                TupleId::new(u64::from(i)),
                &make_row(i as i32, vec![i as f32, 0.0, 0.0]),
            )
            .unwrap();
    }

    let (results, stats) =
        index.search_with_deadline(&[1.0, 0.0, 0.0], 3, 10, Some(Duration::from_secs(10)));
    assert!(!results.is_empty());
    assert!(!stats.truncated);
    assert_eq!(results[0], TupleId::new(1));
}

#[test]
fn search_with_zero_deadline_truncates() {
    let table_desc = make_table_desc();
    let index_desc = make_index_desc();
    let mut index = HnswIndex::new(index_desc, 4, 20);

    for i in 1..=20u32 {
        let v = vec![i as f32, (i as f32).sin(), (i as f32).cos()];
        index
            .insert_tuple(
                &table_desc,
                TupleId::new(u64::from(i)),
                &make_row(i as i32, v),
            )
            .unwrap();
    }

    // Deadline already in the past: should truncate immediately.
    let (_results, stats) =
        index.search_with_deadline(&[1.0, 0.0, 0.0], 3, 10, Some(Duration::from_nanos(0)));
    assert!(stats.truncated, "search with zero deadline should truncate");
}

#[test]
fn search_without_deadline_does_not_truncate() {
    let table_desc = make_table_desc();
    let index_desc = make_index_desc();
    let mut index = HnswIndex::new(index_desc, 4, 20);

    for i in 1..=5u32 {
        index
            .insert_tuple(
                &table_desc,
                TupleId::new(u64::from(i)),
                &make_row(i as i32, vec![i as f32, 0.0, 0.0]),
            )
            .unwrap();
    }

    let (_results, stats) = index.search_with_deadline(&[1.0, 0.0, 0.0], 3, 10, None);
    assert!(!stats.truncated);
}

#[test]
fn search_interruptible_returns_query_canceled() {
    let table_desc = make_table_desc();
    let index_desc = make_index_desc();
    let mut index = HnswIndex::new(index_desc, 4, 20);

    for i in 1..=128u32 {
        let v = vec![i as f32, (i as f32).sin(), (i as f32).cos()];
        index
            .insert_tuple(
                &table_desc,
                TupleId::new(u64::from(i)),
                &make_row(i as i32, v),
            )
            .unwrap();
    }

    let checks = AtomicUsize::new(0);
    let error = index
        .search_interruptible(
            &[1.0, 0.0, 0.0],
            5,
            64,
            None,
            None,
            Some(&|| {
                if checks.fetch_add(1, Ordering::Relaxed) >= 1 {
                    Err(DbError::query_canceled("session canceled"))
                } else {
                    Ok(())
                }
            }),
        )
        .expect_err("interrupt checker should cancel HNSW search");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::QueryCanceled);
}

#[test]
fn search_interruptible_rejects_dimension_mismatch_query() {
    let table_desc = make_table_desc();
    let index_desc = make_index_desc();
    let mut index = HnswIndex::new(index_desc, 4, 20);

    index
        .insert_tuple(
            &table_desc,
            TupleId::new(1),
            &make_row(1, vec![1.0, 0.0, 0.0]),
        )
        .unwrap();

    let err = index
        .search_interruptible(&[1.0, 0.0], 1, 10, None, None, None)
        .expect_err("mismatched query dims should be rejected");
    assert!(
        err.to_string()
            .contains("vector dimension mismatch: 3 vs 2"),
        "unexpected error: {err}"
    );
}

#[test]
fn search_interruptible_tuple_filter_restricts_results() {
    let table_desc = make_table_desc();
    let index_desc = make_index_desc();
    let mut index = HnswIndex::new(index_desc, 4, 20);

    index
        .insert_tuple(
            &table_desc,
            TupleId::new(1),
            &make_row(1, vec![0.0, 0.0, 0.0]),
        )
        .unwrap();
    index
        .insert_tuple(
            &table_desc,
            TupleId::new(2),
            &make_row(2, vec![0.1, 0.0, 0.0]),
        )
        .unwrap();
    index
        .insert_tuple(
            &table_desc,
            TupleId::new(3),
            &make_row(3, vec![0.2, 0.0, 0.0]),
        )
        .unwrap();

    let (ids, _stats) = index
        .search_interruptible(
            &[0.0, 0.0, 0.0],
            3,
            16,
            Some(&|tuple_id| tuple_id.get() % 2 == 0),
            None,
            None,
        )
        .expect("search with tuple filter");
    assert_eq!(ids, vec![TupleId::new(2)]);
}

#[test]
fn search_interruptible_tuple_filter_allows_empty_results() {
    let table_desc = make_table_desc();
    let index_desc = make_index_desc();
    let mut index = HnswIndex::new(index_desc, 4, 20);

    index
        .insert_tuple(
            &table_desc,
            TupleId::new(1),
            &make_row(1, vec![1.0, 0.0, 0.0]),
        )
        .unwrap();

    let (ids, _stats) = index
        .search_interruptible(&[1.0, 0.0, 0.0], 1, 8, Some(&|_| false), None, None)
        .expect("search with rejecting tuple filter");
    assert!(ids.is_empty());
}

#[test]
fn search_interruptible_rejects_non_finite_query_values() {
    let table_desc = make_table_desc();
    let index_desc = make_index_desc();
    let mut index = HnswIndex::new(index_desc, 4, 20);

    index
        .insert_tuple(
            &table_desc,
            TupleId::new(1),
            &make_row(1, vec![1.0, 0.0, 0.0]),
        )
        .unwrap();

    let err = index
        .search_interruptible(&[f32::NAN, 0.0, 1.0], 1, 10, None, None, None)
        .expect_err("non-finite query values should be rejected");
    assert!(
        err.to_string()
            .contains("HNSW search query contains non-finite values"),
        "unexpected error: {err}"
    );
}

#[test]
fn search_interruptible_rejects_excessive_candidate_budget() {
    let table_desc = make_table_desc();
    let index_desc = make_index_desc();
    let mut index = HnswIndex::new(index_desc, 4, 20);

    index
        .insert_tuple(
            &table_desc,
            TupleId::new(1),
            &make_row(1, vec![1.0, 0.0, 0.0]),
        )
        .unwrap();

    let err = index
        .search_interruptible(
            &[1.0, 0.0, 0.0],
            super::search::HNSW_MAX_EF_SEARCH.saturating_add(1),
            10,
            None,
            None,
            None,
        )
        .expect_err("candidate budget above hard cap must be rejected");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::ProgramLimitExceeded);
    assert!(
        err.to_string().contains("candidate budget exceeds limit"),
        "unexpected error: {err}"
    );
}

#[test]
fn hnsw_new_clamps_extreme_param_values() {
    let index = HnswIndex::new(make_index_desc(), u32::MAX, u32::MAX);
    assert_eq!(index.params.m, super::HNSW_MAX_M);
    assert_eq!(index.params.m_max0, super::HNSW_MAX_M * 2);
    assert_eq!(
        index.params.ef_construction,
        super::HNSW_MAX_EF_CONSTRUCTION
    );
}

#[test]
fn insert_tuple_rejects_vector_dimensions_above_safety_limit() {
    let dims = u32::try_from(super::HNSW_MAX_VECTOR_DIMENSIONS.saturating_add(1))
        .expect("HNSW dimension cap must fit u32");
    let table_desc = make_table_desc_with_dims(dims);
    let index_desc = make_index_desc();
    let mut index = HnswIndex::new(index_desc, 4, 20);
    let oversized_row = make_row(
        1,
        vec![0.0f32; super::HNSW_MAX_VECTOR_DIMENSIONS.saturating_add(1)],
    );

    let err = index
        .insert_tuple(&table_desc, TupleId::new(1), &oversized_row)
        .expect_err("oversized vectors must be rejected");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::ProgramLimitExceeded);
    assert!(
        err.to_string().contains("vector dimensions"),
        "unexpected error: {err}"
    );
}

// --- Task C: Index stats tests ---

#[test]
fn index_stats_tracks_inserts_and_deletes() {
    let table_desc = make_table_desc();
    let index_desc = make_index_desc();
    let mut index = HnswIndex::new(index_desc, 4, 20);

    let stats_before = index.index_stats();
    assert_eq!(stats_before.total_vectors, 0);
    assert_eq!(stats_before.total_inserts, 0);
    assert_eq!(stats_before.total_deletes, 0);
    assert_eq!(stats_before.total_searches, 0);

    let row1 = make_row(1, vec![1.0, 0.0, 0.0]);
    let row2 = make_row(2, vec![0.0, 1.0, 0.0]);
    let row3 = make_row(3, vec![0.0, 0.0, 1.0]);

    index
        .insert_tuple(&table_desc, TupleId::new(1), &row1)
        .unwrap();
    index
        .insert_tuple(&table_desc, TupleId::new(2), &row2)
        .unwrap();
    index
        .insert_tuple(&table_desc, TupleId::new(3), &row3)
        .unwrap();

    let stats_after_inserts = index.index_stats();
    assert_eq!(stats_after_inserts.total_vectors, 3);
    assert_eq!(stats_after_inserts.total_inserts, 3);
    assert_eq!(stats_after_inserts.total_deletes, 0);

    index
        .remove_tuple(&table_desc, TupleId::new(2), &row2)
        .unwrap();

    let stats_after_delete = index.index_stats();
    assert_eq!(stats_after_delete.total_vectors, 2);
    assert_eq!(stats_after_delete.total_inserts, 3);
    assert_eq!(stats_after_delete.total_deletes, 1);
}

#[test]
fn index_stats_tracks_searches() {
    let table_desc = make_table_desc();
    let index_desc = make_index_desc();
    let mut index = HnswIndex::new(index_desc, 4, 20);

    for i in 1..=5u32 {
        index
            .insert_tuple(
                &table_desc,
                TupleId::new(u64::from(i)),
                &make_row(i as i32, vec![i as f32, 0.0, 0.0]),
            )
            .unwrap();
    }

    index.search(&[0.0, 0.0, 0.0], 3, 10);
    index.search(&[5.0, 0.0, 0.0], 3, 10);

    let stats = index.index_stats();
    assert_eq!(stats.total_searches, 2);
    assert_eq!(stats.total_vectors, 5);
    assert_eq!(stats.total_inserts, 5);
    assert!(stats.memory_usage_bytes > 0);
    assert!(stats.memory_budget_bytes.is_none());
}

#[test]
fn index_stats_reports_memory_budget() {
    let index_desc = make_index_desc();
    let mut index = HnswIndex::new(index_desc, 4, 20);

    index.set_memory_budget(Some(500_000));

    let stats = index.index_stats();
    assert_eq!(stats.memory_budget_bytes, Some(500_000));
}

#[test]
fn default_metric_is_l2_and_no_quantization() {
    let index = HnswIndex::new(make_index_desc(), 4, 20);
    assert_eq!(index.metric(), aiondb_storage_api::StoredVectorMetric::L2);
    assert_eq!(
        index.quantization(),
        aiondb_storage_api::StoredQuantizationKind::None
    );
}

#[test]
fn from_descriptor_honors_hnsw_options() {
    use aiondb_storage_api::{HnswStorageOptions, StoredQuantizationKind, StoredVectorMetric};
    let mut desc = make_index_desc();
    desc.hnsw_options = Some(HnswStorageOptions {
        m: 4,
        ef_construction: 20,
        distance_metric: StoredVectorMetric::Cosine,
        quantization: StoredQuantizationKind::None,
        prenormalised: false,
    });
    let index = HnswIndex::from_descriptor(desc);
    assert_eq!(index.metric(), StoredVectorMetric::Cosine);
    assert_eq!(index.quantization(), StoredQuantizationKind::None);
}

#[test]
fn cosine_metric_ranks_direction_not_magnitude() {
    use aiondb_storage_api::{HnswStorageOptions, StoredQuantizationKind, StoredVectorMetric};
    let table_desc = make_table_desc();
    let mut desc = make_index_desc();
    desc.hnsw_options = Some(HnswStorageOptions {
        m: 4,
        ef_construction: 20,
        distance_metric: StoredVectorMetric::Cosine,
        quantization: StoredQuantizationKind::None,
        prenormalised: false,
    });
    let mut index = HnswIndex::from_descriptor(desc);

    // Three unit vectors along distinct axes.
    index
        .insert_tuple(
            &table_desc,
            TupleId::new(1),
            &make_row(1, vec![1.0, 0.0, 0.0]),
        )
        .unwrap();
    index
        .insert_tuple(
            &table_desc,
            TupleId::new(2),
            &make_row(2, vec![0.0, 1.0, 0.0]),
        )
        .unwrap();
    index
        .insert_tuple(
            &table_desc,
            TupleId::new(3),
            &make_row(3, vec![0.0, 0.0, 1.0]),
        )
        .unwrap();

    // Query colinear with tuple 2 but much larger magnitude.
    let (results, _stats) = index.search(&[0.0, 10.0, 0.0], 1, 10);
    assert_eq!(results, vec![TupleId::new(2)]);
}

#[test]
fn cosine_prenormalised_fast_path_ranks_correctly() {
    // With `prenormalised = true`, the index uses
    // `aiondb_vector::distance::cosine_distance_normalised` which collapses
    // cosine to `1 - dot`. Feed only unit-norm vectors and check ranking
    // matches the standard cosine path.
    use aiondb_storage_api::{HnswStorageOptions, StoredQuantizationKind, StoredVectorMetric};
    let table_desc = make_table_desc();
    let mut desc = make_index_desc();
    desc.hnsw_options = Some(HnswStorageOptions {
        m: 4,
        ef_construction: 20,
        distance_metric: StoredVectorMetric::Cosine,
        quantization: StoredQuantizationKind::None,
        prenormalised: true,
    });
    let mut index = HnswIndex::from_descriptor(desc);

    let inv_sqrt2 = 1.0 / std::f32::consts::SQRT_2;
    index
        .insert_tuple(
            &table_desc,
            TupleId::new(1),
            &make_row(1, vec![1.0, 0.0, 0.0]),
        )
        .unwrap();
    index
        .insert_tuple(
            &table_desc,
            TupleId::new(2),
            &make_row(2, vec![inv_sqrt2, inv_sqrt2, 0.0]),
        )
        .unwrap();
    index
        .insert_tuple(
            &table_desc,
            TupleId::new(3),
            &make_row(3, vec![0.0, 0.0, 1.0]),
        )
        .unwrap();

    // Query closest to tuple 2 by direction.
    let (results, _stats) = index.search(&[inv_sqrt2, inv_sqrt2, 0.0], 1, 10);
    assert_eq!(results, vec![TupleId::new(2)]);
}

#[test]
fn cosine_prenormalised_matches_full_cosine_topk() {
    // Build two HNSW indexes over the same corpus of unit-norm vectors -
    // one with `prenormalised = true` (cosine fast path), one without -
    // and verify the top-K results match across many queries. Catches any
    // drift between `cosine_distance` and `cosine_distance_normalised` on
    // truly normalised inputs.
    use aiondb_storage_api::{HnswStorageOptions, StoredQuantizationKind, StoredVectorMetric};

    const DIMS: usize = 32;
    const N: usize = 200;
    const K: usize = 10;
    const EF_SEARCH: usize = 64;
    const QUERIES: usize = 16;

    let table_desc = make_table_desc_with_dims(DIMS as u32);

    fn unit_norm(mut v: Vec<f32>) -> Vec<f32> {
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if n > 0.0 {
            for x in &mut v {
                *x /= n;
            }
        }
        v
    }

    let build = |prenormalised: bool| -> HnswIndex {
        let mut desc = make_index_desc();
        desc.hnsw_options = Some(HnswStorageOptions {
            m: 8,
            ef_construction: 80,
            distance_metric: StoredVectorMetric::Cosine,
            quantization: StoredQuantizationKind::None,
            prenormalised,
        });
        let mut index = HnswIndex::from_descriptor(desc);
        for id in 1..=(N as u64) {
            let v = unit_norm(deterministic_vector(id, DIMS));
            index
                .insert_tuple(&table_desc, TupleId::new(id), &make_row(id as i32, v))
                .unwrap();
        }
        index
    };

    let standard = build(false);
    let fast = build(true);

    for q in 0..QUERIES {
        let query = unit_norm(deterministic_vector(10_000 + q as u64, DIMS));
        let (std_ids, _) = standard.search(&query, K, EF_SEARCH);
        let (fast_ids, _) = fast.search(&query, K, EF_SEARCH);
        assert_eq!(
            std_ids, fast_ids,
            "cosine fast-path top-{K} diverged from standard cosine on query {q}"
        );
    }
}

#[test]
fn manhattan_metric_returns_nearest_neighbor() {
    use aiondb_storage_api::{HnswStorageOptions, StoredQuantizationKind, StoredVectorMetric};
    let table_desc = make_table_desc();
    let mut desc = make_index_desc();
    desc.hnsw_options = Some(HnswStorageOptions {
        m: 4,
        ef_construction: 20,
        distance_metric: StoredVectorMetric::Manhattan,
        quantization: StoredQuantizationKind::None,
        prenormalised: false,
    });
    let mut index = HnswIndex::from_descriptor(desc);

    index
        .insert_tuple(
            &table_desc,
            TupleId::new(1),
            &make_row(1, vec![0.0, 0.0, 0.0]),
        )
        .unwrap();
    index
        .insert_tuple(
            &table_desc,
            TupleId::new(2),
            &make_row(2, vec![1.0, 1.0, 1.0]),
        )
        .unwrap();
    index
        .insert_tuple(
            &table_desc,
            TupleId::new(3),
            &make_row(3, vec![3.0, 3.0, 3.0]),
        )
        .unwrap();

    let (results, _stats) = index.search(&[1.2, 1.2, 1.2], 1, 10);
    assert_eq!(results, vec![TupleId::new(2)]);
}

#[test]
fn inner_product_metric_returns_highest_dot() {
    use aiondb_storage_api::{HnswStorageOptions, StoredQuantizationKind, StoredVectorMetric};
    let table_desc = make_table_desc();
    let mut desc = make_index_desc();
    desc.hnsw_options = Some(HnswStorageOptions {
        m: 4,
        ef_construction: 20,
        distance_metric: StoredVectorMetric::InnerProduct,
        quantization: StoredQuantizationKind::None,
        prenormalised: false,
    });
    let mut index = HnswIndex::from_descriptor(desc);

    // Higher-magnitude aligned vectors should win because HNSW searches
    // by minimum distance and the distance is -dot(q, v).
    index
        .insert_tuple(
            &table_desc,
            TupleId::new(1),
            &make_row(1, vec![1.0, 0.0, 0.0]),
        )
        .unwrap();
    index
        .insert_tuple(
            &table_desc,
            TupleId::new(2),
            &make_row(2, vec![5.0, 0.0, 0.0]),
        )
        .unwrap();
    index
        .insert_tuple(
            &table_desc,
            TupleId::new(3),
            &make_row(3, vec![0.0, 1.0, 0.0]),
        )
        .unwrap();

    let (results, _stats) = index.search(&[1.0, 0.0, 0.0], 1, 10);
    assert_eq!(results, vec![TupleId::new(2)]);
}

#[test]
fn quantization_declared_but_storage_uses_raw() {
    use aiondb_storage_api::{HnswStorageOptions, StoredQuantizationKind, StoredVectorMetric};
    let mut desc = make_index_desc();
    desc.hnsw_options = Some(HnswStorageOptions {
        m: 4,
        ef_construction: 20,
        distance_metric: StoredVectorMetric::L2,
        quantization: StoredQuantizationKind::Scalar,
        prenormalised: false,
    });
    let index = HnswIndex::from_descriptor(desc);
    // The declared preference is round-tripped even though storage stays raw.
    assert_eq!(index.quantization(), StoredQuantizationKind::Scalar);
}

#[test]
fn binary_quantization_stores_codes_and_drops_raw_vectors() {
    use aiondb_storage_api::{HnswStorageOptions, StoredQuantizationKind, StoredVectorMetric};
    let table_desc = make_table_desc();
    let mut desc = make_index_desc();
    desc.hnsw_options = Some(HnswStorageOptions {
        m: 4,
        ef_construction: 20,
        distance_metric: StoredVectorMetric::L2,
        quantization: StoredQuantizationKind::Binary,
        prenormalised: false,
    });
    let mut index = HnswIndex::from_descriptor(desc);

    // Insert three points with distinct sign patterns; BQ hashes sign bits.
    index
        .insert_tuple(
            &table_desc,
            TupleId::new(1),
            &make_row(1, vec![1.0, 1.0, 1.0]),
        )
        .unwrap();
    index
        .insert_tuple(
            &table_desc,
            TupleId::new(2),
            &make_row(2, vec![-1.0, -1.0, -1.0]),
        )
        .unwrap();
    index
        .insert_tuple(
            &table_desc,
            TupleId::new(3),
            &make_row(3, vec![1.0, -1.0, 1.0]),
        )
        .unwrap();

    // A positive query matches tuple 1 (same sign pattern) under BQ Hamming.
    let (results, _stats) = index.search(&[0.5, 0.5, 0.5], 1, 10);
    assert_eq!(results, vec![TupleId::new(1)]);

    // A negative query matches tuple 2 (opposite signs of tuple 1).
    let (results, _stats) = index.search(&[-0.5, -0.5, -0.5], 1, 10);
    assert_eq!(results, vec![TupleId::new(2)]);
}

#[test]
fn compact_vector_pruning_uses_decoded_query_node() {
    let mut table_desc = make_table_desc();
    table_desc.columns[1].data_type = aiondb_core::DataType::Vector {
        dims: 3,
        element_type: aiondb_core::VectorElementType::Float16,
    };
    let mut index = HnswIndex::new(make_index_desc(), 1, 20);
    index.set_element_type(aiondb_core::VectorElementType::Float16);

    index
        .insert_tuple(
            &table_desc,
            TupleId::new(1),
            &make_row(1, vec![0.0, 0.0, 0.0]),
        )
        .unwrap();
    index
        .insert_tuple(
            &table_desc,
            TupleId::new(2),
            &make_row(2, vec![5.0, 0.0, 0.0]),
        )
        .unwrap();
    index
        .insert_tuple(
            &table_desc,
            TupleId::new(3),
            &make_row(3, vec![0.125, 0.0, 0.0]),
        )
        .unwrap();
    index
        .insert_tuple(
            &table_desc,
            TupleId::new(4),
            &make_row(4, vec![0.25, 0.0, 0.0]),
        )
        .unwrap();

    for node in index.nodes.values_mut() {
        node.neighbors[0].clear();
    }
    for neighbor_id in [TupleId::new(2), TupleId::new(3), TupleId::new(4)] {
        index.nodes.get_mut(&TupleId::new(1)).unwrap().neighbors[0].insert(neighbor_id);
        index.nodes.get_mut(&neighbor_id).unwrap().neighbors[0].insert(TupleId::new(1));
    }

    let target = index.nodes.get(&TupleId::new(1)).unwrap();
    assert!(target.vector.is_empty());
    assert!(target.compact_vector.is_some());

    index.prune_connections(TupleId::new(1), 0, 2);

    let kept: Vec<TupleId> = index.nodes[&TupleId::new(1)].neighbors[0]
        .iter()
        .copied()
        .collect();
    assert_eq!(kept, vec![TupleId::new(3), TupleId::new(4)]);
}

#[test]
fn cosine_metric_rejects_zero_magnitude_vectors() {
    use aiondb_storage_api::{HnswStorageOptions, StoredQuantizationKind, StoredVectorMetric};
    let table_desc = make_table_desc();
    let mut desc = make_index_desc();
    desc.hnsw_options = Some(HnswStorageOptions {
        m: 4,
        ef_construction: 20,
        distance_metric: StoredVectorMetric::Cosine,
        quantization: StoredQuantizationKind::None,
        prenormalised: false,
    });
    let mut index = HnswIndex::from_descriptor(desc);

    // Non-zero insertion succeeds.
    index
        .insert_tuple(
            &table_desc,
            TupleId::new(1),
            &make_row(1, vec![1.0, 0.0, 0.0]),
        )
        .unwrap();

    // Zero vector is rejected with a clear error.
    let err = index
        .insert_tuple(
            &table_desc,
            TupleId::new(2),
            &make_row(2, vec![0.0, 0.0, 0.0]),
        )
        .expect_err("zero vector should be rejected under cosine metric");
    assert!(
        err.to_string().contains("zero-magnitude"),
        "unexpected error: {err}"
    );
}

#[test]
fn l2_metric_accepts_zero_magnitude_vectors() {
    let table_desc = make_table_desc();
    let mut index = HnswIndex::new(make_index_desc(), 4, 20);

    // Under L2, zero vectors are valid (distance 0 to the origin).
    index
        .insert_tuple(
            &table_desc,
            TupleId::new(1),
            &make_row(1, vec![0.0, 0.0, 0.0]),
        )
        .unwrap();
}

/// Build an HNSW index from a synthetic dataset under the given quantization
/// mode and return the average recall@k against the exact brute-force top-k.
fn build_and_measure_recall_for_quantization(
    quantization: aiondb_storage_api::StoredQuantizationKind,
    k: usize,
    ef: usize,
) -> f64 {
    use aiondb_storage_api::{HnswStorageOptions, StoredVectorMetric};
    let dims = 64usize;
    let dataset_size = 4_000usize;
    let query_count = 100usize;
    let m = 12u32;
    let ef_construction = 64u32;

    let table_desc = make_table_desc_with_dims(dims as u32);
    let mut index_desc = make_index_desc();
    index_desc.hnsw_options = Some(HnswStorageOptions {
        m,
        ef_construction,
        distance_metric: StoredVectorMetric::L2,
        quantization,
        prenormalised: false,
    });

    let dataset = (0..dataset_size)
        .map(|idx| deterministic_vector(idx as u64 + 1, dims))
        .collect::<Vec<_>>();
    let rows = dataset
        .iter()
        .enumerate()
        .map(|(idx, vector)| {
            (
                TupleId::new(idx as u64 + 1),
                make_row(idx as i32 + 1, vector.clone()),
            )
        })
        .collect::<Vec<_>>();

    let index = HnswIndex::from_rows_with_options(&index_desc, &table_desc, rows).unwrap();

    let mut total_hits = 0usize;
    for query_idx in 0..query_count {
        let query = &dataset[(query_idx * 47) % dataset_size];
        let expected = brute_force_top_k(&dataset, query, k);
        let (actual, _stats) = index.search(query, k, ef);
        let actual_set = actual
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        total_hits += expected
            .into_iter()
            .filter(|tuple_id| actual_set.contains(tuple_id))
            .count();
    }

    total_hits as f64 / (query_count * k) as f64
}

#[test]
fn recall_at_k_with_scalar_quantization_meets_threshold() {
    let recall = build_and_measure_recall_for_quantization(
        aiondb_storage_api::StoredQuantizationKind::Scalar,
        10,
        128,
    );
    assert!(
        recall >= 0.90,
        "scalar-quantized HNSW recall@10 dropped to {recall:.3}"
    );
}

#[test]
fn recall_at_k_with_product_quantization_meets_threshold() {
    let recall = build_and_measure_recall_for_quantization(
        aiondb_storage_api::StoredQuantizationKind::Product,
        10,
        128,
    );
    assert!(
        recall >= 0.70,
        "product-quantized HNSW recall@10 dropped to {recall:.3}"
    );
}

#[test]
fn recall_at_k_with_binary_quantization_meets_threshold() {
    // Binary quantization drops the raw vector, so the in-index rescore
    // path cannot run. Recall is therefore intrinsically lower than SQ/PQ;
    // this baseline locks in the current behavior and will be revisited
    // once heap-fetched rescoring is wired through the executor.
    let recall = build_and_measure_recall_for_quantization(
        aiondb_storage_api::StoredQuantizationKind::Binary,
        10,
        128,
    );
    assert!(
        recall >= 0.20,
        "binary-quantized HNSW recall@10 dropped below regression floor: {recall:.3}"
    );
}

/// SQ / PQ indexes created on an empty table must eventually train the
/// codebook from accumulated live inserts. After enough inserts the codes
/// should be present on every node and search recall should match the
/// codebook-trained baseline.
fn assert_lazy_training_back_fills_codes(
    quantization: aiondb_storage_api::StoredQuantizationKind,
    expected_recall: f64,
) {
    use aiondb_storage_api::{HnswStorageOptions, StoredVectorMetric};
    let dims = 64usize;
    let dataset_size = 300usize;
    let m = 12u32;
    let ef_construction = 64u32;

    let table_desc = make_table_desc_with_dims(dims as u32);
    let mut index_desc = make_index_desc();
    index_desc.hnsw_options = Some(HnswStorageOptions {
        m,
        ef_construction,
        distance_metric: StoredVectorMetric::L2,
        quantization,
        prenormalised: false,
    });

    let dataset = (0..dataset_size)
        .map(|idx| deterministic_vector(idx as u64 + 1, dims))
        .collect::<Vec<_>>();

    let mut index = HnswIndex::from_descriptor(index_desc);
    for (idx, vector) in dataset.iter().enumerate() {
        let row = make_row(idx as i32 + 1, vector.clone());
        index
            .insert_tuple(&table_desc, TupleId::new(idx as u64 + 1), &row)
            .unwrap();
    }

    let mut total_hits = 0usize;
    let k = 10usize;
    let ef = 128usize;
    let query_count = 60usize;
    for query_idx in 0..query_count {
        let query = &dataset[(query_idx * 11) % dataset_size];
        let expected = brute_force_top_k(&dataset, query, k);
        let (actual, _stats) = index.search(query, k, ef);
        let actual_set = actual
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        total_hits += expected
            .into_iter()
            .filter(|tuple_id| actual_set.contains(tuple_id))
            .count();
    }
    let recall = total_hits as f64 / (query_count * k) as f64;
    assert!(
        recall >= expected_recall,
        "lazy-trained {kind:?} HNSW recall@{k} dropped to {recall:.3}",
        kind = quantization,
    );
}

#[test]
fn lazy_training_trains_scalar_quantizer_after_threshold_inserts() {
    assert_lazy_training_back_fills_codes(
        aiondb_storage_api::StoredQuantizationKind::Scalar,
        0.85,
    );
}

#[test]
fn lazy_training_trains_product_quantizer_after_threshold_inserts() {
    assert_lazy_training_back_fills_codes(
        aiondb_storage_api::StoredQuantizationKind::Product,
        0.55,
    );
}

#[test]
fn search_stats_report_quantization_and_rescored_candidates() {
    use aiondb_storage_api::{HnswStorageOptions, StoredVectorMetric};
    let dims = 32usize;
    let dataset_size = 300usize;
    let table_desc = make_table_desc_with_dims(dims as u32);
    let mut index_desc = make_index_desc();
    index_desc.hnsw_options = Some(HnswStorageOptions {
        m: 8,
        ef_construction: 32,
        distance_metric: StoredVectorMetric::L2,
        quantization: aiondb_storage_api::StoredQuantizationKind::Scalar,
        prenormalised: false,
    });
    let dataset = (0..dataset_size)
        .map(|idx| deterministic_vector(idx as u64 + 1, dims))
        .collect::<Vec<_>>();
    let rows = dataset
        .iter()
        .enumerate()
        .map(|(idx, vector)| {
            (
                TupleId::new(idx as u64 + 1),
                make_row(idx as i32 + 1, vector.clone()),
            )
        })
        .collect::<Vec<_>>();
    let index = HnswIndex::from_rows_with_options(&index_desc, &table_desc, rows).unwrap();
    let (results, stats) = index.search(&dataset[0], 5, 64);
    assert!(!results.is_empty(), "scalar-quantized search returned no rows");
    assert_eq!(
        stats.quantization,
        aiondb_storage_api::StoredQuantizationKind::Scalar,
        "search stats must remember the active quantization mode"
    );
    assert!(
        stats.rescored_candidates >= results.len() as u64,
        "rescored_candidates ({}) must be at least the number of returned rows ({})",
        stats.rescored_candidates,
        results.len()
    );
}

#[test]
fn search_stats_report_no_rescore_for_raw_index() {
    let table_desc = make_table_desc();
    let mut index = HnswIndex::new(make_index_desc(), 4, 20);
    for i in 1..=5u32 {
        let row = make_row(i as i32, vec![i as f32, 0.0, 0.0]);
        index
            .insert_tuple(&table_desc, TupleId::new(u64::from(i)), &row)
            .unwrap();
    }
    let (_results, stats) = index.search(&[1.0, 0.0, 0.0], 3, 10);
    assert_eq!(
        stats.quantization,
        aiondb_storage_api::StoredQuantizationKind::None,
    );
    assert_eq!(stats.rescored_candidates, 0);
    assert_eq!(stats.oversample_factor, 1);
    assert!(stats.effective_ef_search >= 3);
}

#[test]
fn search_stats_report_oversample_factor_for_quantized_index() {
    use aiondb_storage_api::{HnswStorageOptions, StoredVectorMetric};
    let dims = 32usize;
    let dataset_size = 300usize;
    let table_desc = make_table_desc_with_dims(dims as u32);
    let mut index_desc = make_index_desc();
    index_desc.hnsw_options = Some(HnswStorageOptions {
        m: 8,
        ef_construction: 32,
        distance_metric: StoredVectorMetric::L2,
        quantization: aiondb_storage_api::StoredQuantizationKind::Product,
        prenormalised: false,
    });
    let dataset = (0..dataset_size)
        .map(|idx| deterministic_vector(idx as u64 + 1, dims))
        .collect::<Vec<_>>();
    let rows = dataset
        .iter()
        .enumerate()
        .map(|(idx, vector)| {
            (
                TupleId::new(idx as u64 + 1),
                make_row(idx as i32 + 1, vector.clone()),
            )
        })
        .collect::<Vec<_>>();
    let index = HnswIndex::from_rows_with_options(&index_desc, &table_desc, rows).unwrap();
    let (_results, stats) = index.search(&dataset[0], 10, 64);
    assert_eq!(
        stats.quantization,
        aiondb_storage_api::StoredQuantizationKind::Product,
    );
    assert_eq!(stats.oversample_factor, 5);
    assert!(
        stats.effective_ef_search >= 50,
        "PQ search should widen ef_search to at least k * oversample (got {})",
        stats.effective_ef_search,
    );
}
