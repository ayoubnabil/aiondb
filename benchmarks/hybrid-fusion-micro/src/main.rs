//! Microbenchmark for hybrid/vector top-K selection.
//!
//! This isolates hot top-K ordering steps used after dense vector and sparse
//! text searches have produced candidates. It compares the old full-sort shape
//! against the partial-select shape used by the executor.

use std::{env, hint::black_box, time::Instant};

#[derive(Clone)]
struct Entry {
    id: i64,
    fused_score: f64,
}

fn main() {
    let candidates = env_usize("FUSION_CANDIDATES", 200_000);
    let k = env_usize("FUSION_K", 50);
    let iters = env_usize("FUSION_ITERS", 20);
    let warmup = env_usize("FUSION_WARMUP", 3);

    let base = make_entries(candidates);
    let full_checksum = full_sort_top_k(base.clone(), k);
    let partial_checksum = partial_select_top_k(base.clone(), k);
    assert_eq!(
        full_checksum, partial_checksum,
        "partial top-K must match full-sort output"
    );

    for _ in 0..warmup {
        black_box(full_sort_top_k(base.clone(), k));
        black_box(partial_select_top_k(base.clone(), k));
    }

    let full_sort = measure(iters, || full_sort_top_k(base.clone(), k));
    let partial_select = measure(iters, || partial_select_top_k(base.clone(), k));
    let speedup = full_sort.as_secs_f64() / partial_select.as_secs_f64();

    println!("# AionDB hybrid/vector top-K microbench");
    println!("candidates={candidates} k={k} iters={iters} checksum={full_checksum}");
    println!("full_sort_ms={:.3}", full_sort.as_secs_f64() * 1_000.0);
    println!(
        "partial_select_ms={:.3}",
        partial_select.as_secs_f64() * 1_000.0
    );
    println!("speedup={speedup:.2}x");

    let vector_base = make_vector_scores(candidates);
    let vector_full_checksum = vector_full_sort_top_k(vector_base.clone(), k);
    let vector_partial_checksum = vector_partial_select_top_k(vector_base.clone(), k);
    let vector_heap_checksum = vector_bounded_heap_top_k(vector_base.clone(), k);
    assert_eq!(
        vector_full_checksum, vector_partial_checksum,
        "partial vector top-K must match full-sort output"
    );
    assert_eq!(
        vector_full_checksum, vector_heap_checksum,
        "bounded heap vector top-K must match full-sort output"
    );
    for _ in 0..warmup {
        black_box(vector_full_sort_top_k(vector_base.clone(), k));
        black_box(vector_partial_select_top_k(vector_base.clone(), k));
        black_box(vector_bounded_heap_top_k(vector_base.clone(), k));
    }
    let vector_full_sort = measure(iters, || vector_full_sort_top_k(vector_base.clone(), k));
    let vector_partial_select = measure(iters, || {
        vector_partial_select_top_k(vector_base.clone(), k)
    });
    let vector_bounded_heap = measure(iters, || vector_bounded_heap_top_k(vector_base.clone(), k));
    let vector_speedup = vector_full_sort.as_secs_f64() / vector_partial_select.as_secs_f64();
    let vector_heap_speedup = vector_full_sort.as_secs_f64() / vector_bounded_heap.as_secs_f64();
    println!(
        "vector_full_sort_ms={:.3}",
        vector_full_sort.as_secs_f64() * 1_000.0
    );
    println!(
        "vector_partial_select_ms={:.3}",
        vector_partial_select.as_secs_f64() * 1_000.0
    );
    println!("vector_speedup={vector_speedup:.2}x checksum={vector_full_checksum}");
    println!(
        "vector_bounded_heap_ms={:.3}",
        vector_bounded_heap.as_secs_f64() * 1_000.0
    );
    println!("vector_heap_speedup={vector_heap_speedup:.2}x checksum={vector_heap_checksum}");

    let json_candidates = env_usize("JSON_CANDIDATES", candidates.min(50_000));
    let json_iters = env_usize("JSON_ITERS", iters.min(10));
    let json_base = make_json_hits(json_candidates);
    let clone_all_checksum = json_clone_all_then_window(&json_base, k);
    let borrowed_checksum = json_borrow_then_window_clone(&json_base, k);
    assert_eq!(
        clone_all_checksum, borrowed_checksum,
        "borrowed JSON hit parse must match clone-all output"
    );
    for _ in 0..warmup {
        black_box(json_clone_all_then_window(&json_base, k));
        black_box(json_borrow_then_window_clone(&json_base, k));
    }
    let json_clone_all = measure(json_iters, || json_clone_all_then_window(&json_base, k));
    let json_borrowed = measure(json_iters, || json_borrow_then_window_clone(&json_base, k));
    let json_speedup = json_clone_all.as_secs_f64() / json_borrowed.as_secs_f64();
    println!("json_candidates={json_candidates} json_iters={json_iters}");
    println!(
        "json_clone_all_ms={:.3}",
        json_clone_all.as_secs_f64() * 1_000.0
    );
    println!(
        "json_borrow_window_ms={:.3}",
        json_borrowed.as_secs_f64() * 1_000.0
    );
    println!("json_speedup={json_speedup:.2}x checksum={clone_all_checksum}");

    let option_terms = env_usize("OPTIONS_FILTER_TERMS", 1_000);
    let options_json = make_options_json(option_terms);
    let options_clone_checksum = options_clone_parse_checksum(&options_json);
    let options_borrow_checksum = options_borrow_parse_checksum(&options_json);
    assert_eq!(
        options_clone_checksum, options_borrow_checksum,
        "borrowed options parsing must match clone parse output"
    );
    for _ in 0..warmup {
        black_box(options_clone_parse_checksum(&options_json));
        black_box(options_borrow_parse_checksum(&options_json));
    }
    let options_clone = measure(json_iters, || options_clone_parse_checksum(&options_json));
    let options_borrow = measure(json_iters, || options_borrow_parse_checksum(&options_json));
    let options_speedup = options_clone.as_secs_f64() / options_borrow.as_secs_f64();
    println!("options_filter_terms={option_terms}");
    println!(
        "options_clone_parse_ms={:.3}",
        options_clone.as_secs_f64() * 1_000.0
    );
    println!(
        "options_borrow_parse_ms={:.3}",
        options_borrow.as_secs_f64() * 1_000.0
    );
    println!("options_parse_speedup={options_speedup:.2}x checksum={options_clone_checksum}");

    let range_values = env_usize("RANGE_JSON_VALUES", 100_000);
    let range_json = make_range_json_array(range_values);
    let range_clone_checksum = range_clone_wrapped_checksum(&range_json);
    let range_borrow_checksum = range_borrow_direct_checksum(&range_json);
    assert_eq!(
        range_clone_checksum, range_borrow_checksum,
        "borrowed JSON range scan must match clone wrapped output"
    );
    for _ in 0..warmup {
        black_box(range_clone_wrapped_checksum(&range_json));
        black_box(range_borrow_direct_checksum(&range_json));
    }
    let range_clone = measure(json_iters, || range_clone_wrapped_checksum(&range_json));
    let range_borrow = measure(json_iters, || range_borrow_direct_checksum(&range_json));
    let range_speedup = range_clone.as_secs_f64() / range_borrow.as_secs_f64();
    println!("range_json_values={range_values}");
    println!(
        "range_clone_wrapped_ms={:.3}",
        range_clone.as_secs_f64() * 1_000.0
    );
    println!(
        "range_borrow_direct_ms={:.3}",
        range_borrow.as_secs_f64() * 1_000.0
    );
    println!("range_json_speedup={range_speedup:.2}x checksum={range_clone_checksum}");

    let text_values = env_usize("TEXT_FILTER_VALUES", 100_000);
    let text_filter_values = make_text_filter_values(text_values);
    let text_old_checksum = text_contains_old_checksum(&text_filter_values, "PAYLOAD");
    let text_new_checksum = text_contains_new_checksum(&text_filter_values, "payload");
    assert_eq!(
        text_old_checksum, text_new_checksum,
        "pre-lowered text filter must match old case-insensitive output"
    );
    for _ in 0..warmup {
        black_box(text_contains_old_checksum(&text_filter_values, "PAYLOAD"));
        black_box(text_contains_new_checksum(&text_filter_values, "payload"));
    }
    let text_old = measure(json_iters, || {
        text_contains_old_checksum(&text_filter_values, "PAYLOAD")
    });
    let text_new = measure(json_iters, || {
        text_contains_new_checksum(&text_filter_values, "payload")
    });
    let text_speedup = text_old.as_secs_f64() / text_new.as_secs_f64();
    println!("text_filter_values={text_values}");
    println!(
        "text_contains_old_ms={:.3}",
        text_old.as_secs_f64() * 1_000.0
    );
    println!(
        "text_contains_pre_lowered_ms={:.3}",
        text_new.as_secs_f64() * 1_000.0
    );
    println!("text_contains_speedup={text_speedup:.2}x checksum={text_old_checksum}");

    let min_should_conditions = env_usize("MIN_SHOULD_CONDITIONS", 1_000_000);
    let min_should_required = env_usize("MIN_SHOULD_REQUIRED", 100);
    let min_should_values = make_min_should_values(min_should_conditions);
    let min_should_count_checksum =
        min_should_count_all_checksum(&min_should_values, min_should_required);
    let min_should_short_checksum =
        min_should_short_circuit_checksum(&min_should_values, min_should_required);
    assert_eq!(
        min_should_count_checksum, min_should_short_checksum,
        "short-circuit min_should must match count-all output"
    );
    for _ in 0..warmup {
        black_box(min_should_count_all_checksum(
            &min_should_values,
            min_should_required,
        ));
        black_box(min_should_short_circuit_checksum(
            &min_should_values,
            min_should_required,
        ));
    }
    let min_should_count = measure(json_iters, || {
        min_should_count_all_checksum(&min_should_values, min_should_required)
    });
    let min_should_short = measure(json_iters, || {
        min_should_short_circuit_checksum(&min_should_values, min_should_required)
    });
    let min_should_speedup = min_should_count.as_secs_f64() / min_should_short.as_secs_f64();
    println!(
        "min_should_conditions={min_should_conditions} min_should_required={min_should_required}"
    );
    println!(
        "min_should_count_all_ms={:.3}",
        min_should_count.as_secs_f64() * 1_000.0
    );
    println!(
        "min_should_short_circuit_ms={:.3}",
        min_should_short.as_secs_f64() * 1_000.0
    );
    println!("min_should_speedup={min_should_speedup:.2}x checksum={min_should_count_checksum}");

    let impossible_required = min_should_conditions.saturating_add(1);
    let impossible_count_checksum =
        min_should_count_all_checksum(&min_should_values, impossible_required);
    let impossible_short_checksum =
        min_should_impossible_short_circuit_checksum(&min_should_values, impossible_required);
    assert_eq!(
        impossible_count_checksum, impossible_short_checksum,
        "impossible min_should short-circuit must match count-all output"
    );
    let impossible_count = measure(json_iters, || {
        min_should_count_all_checksum(&min_should_values, impossible_required)
    });
    let impossible_short = measure(json_iters, || {
        min_should_impossible_short_circuit_checksum(&min_should_values, impossible_required)
    });
    let impossible_speedup = impossible_count.as_secs_f64() / impossible_short.as_secs_f64();
    println!("min_should_impossible_required={impossible_required}");
    println!(
        "min_should_impossible_count_all_ms={:.3}",
        impossible_count.as_secs_f64() * 1_000.0
    );
    println!(
        "min_should_impossible_short_circuit_ms={:.3}",
        impossible_short.as_secs_f64() * 1_000.0
    );
    println!(
        "min_should_impossible_speedup={impossible_speedup:.2}x checksum={impossible_count_checksum}"
    );

    let zero_count_checksum = min_should_count_all_checksum(&min_should_values, 0);
    let zero_elided_checksum = min_should_zero_elided_checksum();
    assert_eq!(
        zero_count_checksum, zero_elided_checksum,
        "zero min_should elision must match count-all output"
    );
    let zero_count = measure(json_iters, || min_should_count_all_checksum(&min_should_values, 0));
    let zero_elided = measure(json_iters, min_should_zero_elided_checksum);
    let zero_speedup = zero_count.as_secs_f64() / zero_elided.as_secs_f64();
    println!("min_should_zero_required=0");
    println!(
        "min_should_zero_count_all_ms={:.3}",
        zero_count.as_secs_f64() * 1_000.0
    );
    println!(
        "min_should_zero_elided_ms={:.3}",
        zero_elided.as_secs_f64() * 1_000.0
    );
    println!("min_should_zero_speedup={zero_speedup:.2}x checksum={zero_count_checksum}");

    let min_should_index_rows = env_usize("MIN_SHOULD_INDEX_ROWS", min_should_conditions);
    let min_should_index_required = env_usize("MIN_SHOULD_INDEX_REQUIRED", 2);
    let min_should_index_lists = make_min_should_index_lists(min_should_index_rows);
    let indexed_scan_checksum =
        min_should_index_table_scan_checksum(min_should_index_rows, min_should_index_required);
    let indexed_prefilter_checksum =
        min_should_index_prefilter_checksum(&min_should_index_lists, min_should_index_required);
    assert_eq!(
        indexed_scan_checksum, indexed_prefilter_checksum,
        "indexed min_should prefilter must match table scan output"
    );
    for _ in 0..warmup {
        black_box(min_should_index_table_scan_checksum(
            min_should_index_rows,
            min_should_index_required,
        ));
        black_box(min_should_index_prefilter_checksum(
            &min_should_index_lists,
            min_should_index_required,
        ));
    }
    let indexed_scan = measure(json_iters, || {
        min_should_index_table_scan_checksum(min_should_index_rows, min_should_index_required)
    });
    let indexed_prefilter = measure(json_iters, || {
        min_should_index_prefilter_checksum(&min_should_index_lists, min_should_index_required)
    });
    let indexed_speedup = indexed_scan.as_secs_f64() / indexed_prefilter.as_secs_f64();
    println!(
        "min_should_index_rows={min_should_index_rows} min_should_index_required={min_should_index_required}"
    );
    println!(
        "min_should_index_table_scan_ms={:.3}",
        indexed_scan.as_secs_f64() * 1_000.0
    );
    println!(
        "min_should_index_prefilter_ms={:.3}",
        indexed_prefilter.as_secs_f64() * 1_000.0
    );
    println!(
        "min_should_index_speedup={indexed_speedup:.2}x checksum={indexed_scan_checksum}"
    );

    let min_should_exact_required = env_usize("MIN_SHOULD_INDEX_EXACT_REQUIRED", 1);
    let exact_validate_checksum = min_should_index_prefilter_validate_checksum(
        &min_should_index_lists,
        min_should_exact_required,
    );
    let exact_direct_checksum =
        min_should_index_prefilter_checksum(&min_should_index_lists, min_should_exact_required);
    assert_eq!(
        exact_validate_checksum, exact_direct_checksum,
        "exact indexed min_should output must match validated prefilter output"
    );
    for _ in 0..warmup {
        black_box(min_should_index_prefilter_validate_checksum(
            &min_should_index_lists,
            min_should_exact_required,
        ));
        black_box(min_should_index_prefilter_checksum(
            &min_should_index_lists,
            min_should_exact_required,
        ));
    }
    let exact_validate = measure(json_iters, || {
        min_should_index_prefilter_validate_checksum(
            &min_should_index_lists,
            min_should_exact_required,
        )
    });
    let exact_direct = measure(json_iters, || {
        min_should_index_prefilter_checksum(&min_should_index_lists, min_should_exact_required)
    });
    let exact_speedup = exact_validate.as_secs_f64() / exact_direct.as_secs_f64();
    println!("min_should_index_exact_required={min_should_exact_required}");
    println!(
        "min_should_index_validate_ms={:.3}",
        exact_validate.as_secs_f64() * 1_000.0
    );
    println!(
        "min_should_index_exact_return_ms={:.3}",
        exact_direct.as_secs_f64() * 1_000.0
    );
    println!(
        "min_should_index_exact_speedup={exact_speedup:.2}x checksum={exact_direct_checksum}"
    );

    let partial_should_rows = env_usize("PARTIAL_SHOULD_ROWS", min_should_index_rows);
    let partial_should_index =
        make_partial_should_index_list(partial_should_rows, PARTIAL_SHOULD_INDEXED_DIVISOR);
    let partial_old_checksum =
        partial_should_old_wasted_index_checksum(partial_should_rows, &partial_should_index);
    let partial_new_checksum = partial_should_new_table_scan_checksum(partial_should_rows);
    assert_eq!(
        partial_old_checksum, partial_new_checksum,
        "partial should skip must preserve table-scan output"
    );
    for _ in 0..warmup {
        black_box(partial_should_old_wasted_index_checksum(
            partial_should_rows,
            &partial_should_index,
        ));
        black_box(partial_should_new_table_scan_checksum(partial_should_rows));
    }
    let partial_old = measure(json_iters, || {
        partial_should_old_wasted_index_checksum(partial_should_rows, &partial_should_index)
    });
    let partial_new = measure(json_iters, || {
        partial_should_new_table_scan_checksum(partial_should_rows)
    });
    let partial_speedup = partial_old.as_secs_f64() / partial_new.as_secs_f64();
    println!("partial_should_rows={partial_should_rows}");
    println!(
        "partial_should_wasted_index_ms={:.3}",
        partial_old.as_secs_f64() * 1_000.0
    );
    println!(
        "partial_should_skip_index_ms={:.3}",
        partial_new.as_secs_f64() * 1_000.0
    );
    println!("partial_should_speedup={partial_speedup:.2}x checksum={partial_new_checksum}");

    let dense_hits = make_json_hits(json_candidates);
    let sparse_hits = make_json_hits(json_candidates);
    let old_rrf_checksum = rrf_old_clone_btree_full_sort(&dense_hits, &sparse_hits, k);
    let new_rrf_checksum = rrf_new_borrow_hash_partial(&dense_hits, &sparse_hits, k);
    assert_eq!(
        old_rrf_checksum, new_rrf_checksum,
        "optimized RRF pipeline must match old pipeline output"
    );
    for _ in 0..warmup {
        black_box(rrf_old_clone_btree_full_sort(&dense_hits, &sparse_hits, k));
        black_box(rrf_new_borrow_hash_partial(&dense_hits, &sparse_hits, k));
    }
    let old_rrf = measure(json_iters, || {
        rrf_old_clone_btree_full_sort(&dense_hits, &sparse_hits, k)
    });
    let new_rrf = measure(json_iters, || {
        rrf_new_borrow_hash_partial(&dense_hits, &sparse_hits, k)
    });
    let rrf_speedup = old_rrf.as_secs_f64() / new_rrf.as_secs_f64();
    println!("rrf_old_pipeline_ms={:.3}", old_rrf.as_secs_f64() * 1_000.0);
    println!("rrf_new_pipeline_ms={:.3}", new_rrf.as_secs_f64() * 1_000.0);
    println!("rrf_pipeline_speedup={rrf_speedup:.2}x checksum={old_rrf_checksum}");

    let old_dbsf_checksum = dbsf_old_vec_btree_full_sort(&dense_hits, &sparse_hits, k);
    let new_dbsf_checksum = dbsf_new_normalizer_hash_partial(&dense_hits, &sparse_hits, k);
    assert_eq!(
        old_dbsf_checksum, new_dbsf_checksum,
        "optimized DBSF pipeline must match old pipeline output"
    );
    for _ in 0..warmup {
        black_box(dbsf_old_vec_btree_full_sort(&dense_hits, &sparse_hits, k));
        black_box(dbsf_new_normalizer_hash_partial(
            &dense_hits,
            &sparse_hits,
            k,
        ));
    }
    let old_dbsf = measure(json_iters, || {
        dbsf_old_vec_btree_full_sort(&dense_hits, &sparse_hits, k)
    });
    let new_dbsf = measure(json_iters, || {
        dbsf_new_normalizer_hash_partial(&dense_hits, &sparse_hits, k)
    });
    let dbsf_speedup = old_dbsf.as_secs_f64() / new_dbsf.as_secs_f64();
    println!(
        "dbsf_old_pipeline_ms={:.3}",
        old_dbsf.as_secs_f64() * 1_000.0
    );
    println!(
        "dbsf_new_pipeline_ms={:.3}",
        new_dbsf.as_secs_f64() * 1_000.0
    );
    println!("dbsf_pipeline_speedup={dbsf_speedup:.2}x checksum={old_dbsf_checksum}");
}

fn env_usize(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn measure(mut iters: usize, mut f: impl FnMut() -> u64) -> std::time::Duration {
    let start = Instant::now();
    while iters > 0 {
        black_box(f());
        iters -= 1;
    }
    start.elapsed()
}

fn make_entries(count: usize) -> Vec<Entry> {
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    (0..count)
        .map(|index| {
            state ^= state << 7;
            state ^= state >> 9;
            state ^= state << 8;
            let score_bucket = (state % 1_000_003) as f64 / 1_000_003.0;
            Entry {
                id: i64::try_from(index).unwrap_or(i64::MAX),
                fused_score: score_bucket,
            }
        })
        .collect()
}

fn compare_entry(left: &Entry, right: &Entry) -> std::cmp::Ordering {
    right
        .fused_score
        .total_cmp(&left.fused_score)
        .then_with(|| left.id.cmp(&right.id))
}

fn checksum(values: &[Entry]) -> u64 {
    values.iter().fold(0xcbf2_9ce4_8422_2325, |acc, entry| {
        let id_bits = entry.id as u64;
        let score_bits = entry.fused_score.to_bits();
        acc.rotate_left(5) ^ id_bits.wrapping_mul(0x1000_0000_01b3) ^ score_bits
    })
}

fn full_sort_top_k(mut values: Vec<Entry>, k: usize) -> u64 {
    values.sort_by(compare_entry);
    values.truncate(k);
    checksum(&values)
}

fn partial_select_top_k(mut values: Vec<Entry>, k: usize) -> u64 {
    if values.len() > k {
        values.select_nth_unstable_by(k - 1, compare_entry);
        values.truncate(k);
    }
    values.sort_by(compare_entry);
    checksum(&values)
}

fn make_vector_scores(count: usize) -> Vec<(f64, i64)> {
    let mut state = 0xd6e8_feb8_6659_fd93_u64;
    (0..count)
        .map(|index| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let distance = (state % 10_000_019) as f64 / 10_000_019.0;
            (distance, i64::try_from(index).unwrap_or(i64::MAX))
        })
        .collect()
}

fn compare_vector_score(left: &(f64, i64), right: &(f64, i64)) -> std::cmp::Ordering {
    left.0
        .total_cmp(&right.0)
        .then_with(|| left.1.cmp(&right.1))
}

fn vector_checksum(values: &[(f64, i64)]) -> u64 {
    values.iter().fold(0x8422_2325_cbf2_9ce4, |acc, entry| {
        acc.rotate_left(7) ^ entry.0.to_bits() ^ (entry.1 as u64).wrapping_mul(0x9e37_79b9)
    })
}

fn vector_full_sort_top_k(mut values: Vec<(f64, i64)>, k: usize) -> u64 {
    values.sort_by(compare_vector_score);
    values.truncate(k);
    vector_checksum(&values)
}

fn vector_partial_select_top_k(mut values: Vec<(f64, i64)>, k: usize) -> u64 {
    if values.len() > k {
        values.select_nth_unstable_by(k - 1, compare_vector_score);
        values.truncate(k);
    }
    values.sort_by(compare_vector_score);
    vector_checksum(&values)
}

#[derive(Clone, Copy, Debug)]
struct HeapVectorScore {
    distance: f64,
    id: i64,
}

impl PartialEq for HeapVectorScore {
    fn eq(&self, other: &Self) -> bool {
        self.distance.total_cmp(&other.distance) == std::cmp::Ordering::Equal && self.id == other.id
    }
}

impl Eq for HeapVectorScore {}

impl PartialOrd for HeapVectorScore {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HeapVectorScore {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.distance
            .total_cmp(&other.distance)
            .then_with(|| self.id.cmp(&other.id))
    }
}

fn heap_vector_score_is_better(distance: f64, id: i64, other: &HeapVectorScore) -> bool {
    match distance.total_cmp(&other.distance) {
        std::cmp::Ordering::Less => true,
        std::cmp::Ordering::Greater => false,
        std::cmp::Ordering::Equal => id < other.id,
    }
}

fn vector_bounded_heap_top_k(values: Vec<(f64, i64)>, k: usize) -> u64 {
    let mut heap = std::collections::BinaryHeap::<HeapVectorScore>::new();
    for (distance, id) in values {
        if k == 0 {
            break;
        }
        let score = HeapVectorScore { distance, id };
        if heap.len() < k {
            heap.push(score);
        } else if let Some(mut worst) = heap.peek_mut() {
            if heap_vector_score_is_better(score.distance, score.id, &worst) {
                *worst = score;
            }
        }
    }
    let mut values = heap
        .into_vec()
        .into_iter()
        .map(|score| (score.distance, score.id))
        .collect::<Vec<_>>();
    values.sort_by(compare_vector_score);
    vector_checksum(&values)
}

fn make_json_hits(count: usize) -> Vec<serde_json::Value> {
    (0..count)
        .map(|index| {
            let mut payload = serde_json::Map::new();
            payload.insert(
                "tenant".to_owned(),
                serde_json::Value::Number(((index % 97) as u64).into()),
            );
            payload.insert(
                "title".to_owned(),
                serde_json::Value::String(format!("doc-{index}-hybrid-payload")),
            );
            payload.insert(
                "tag".to_owned(),
                serde_json::Value::String(format!("tag-{}", index % 13)),
            );

            let mut hit = serde_json::Map::new();
            hit.insert(
                "id".to_owned(),
                serde_json::Value::Number((index as u64).into()),
            );
            hit.insert(
                "score".to_owned(),
                serde_json::Value::from(((index * 37 + 11) % 10_007) as f64 / 10_007.0),
            );
            hit.insert("payload".to_owned(), serde_json::Value::Object(payload));
            serde_json::Value::Object(hit)
        })
        .collect()
}

fn json_window_checksum<'a>(
    hits: impl IntoIterator<Item = &'a serde_json::Map<String, serde_json::Value>>,
) -> u64 {
    hits.into_iter().fold(0xaf63_bd4c_8601_b7df, |acc, hit| {
        let id = hit
            .get("id")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let score_bits = hit
            .get("score")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0)
            .to_bits();
        let title_len = hit
            .get("payload")
            .and_then(serde_json::Value::as_object)
            .and_then(|payload| payload.get("title"))
            .and_then(serde_json::Value::as_str)
            .map_or(0_u64, |title| title.len() as u64);
        acc.rotate_left(9) ^ id ^ score_bits ^ title_len
    })
}

fn json_clone_all_then_window(values: &[serde_json::Value], k: usize) -> u64 {
    let cloned = values
        .iter()
        .filter_map(|value| value.as_object().cloned())
        .collect::<Vec<_>>();
    json_window_checksum(cloned.iter().take(k))
}

fn json_borrow_then_window_clone(values: &[serde_json::Value], k: usize) -> u64 {
    let borrowed = values
        .iter()
        .filter_map(serde_json::Value::as_object)
        .collect::<Vec<_>>();
    let retained = borrowed
        .into_iter()
        .take(k)
        .map(Clone::clone)
        .collect::<Vec<_>>();
    json_window_checksum(retained.iter())
}

fn make_options_json(filter_terms: usize) -> serde_json::Value {
    let must = (0..filter_terms)
        .map(|index| {
            serde_json::json!({
                "key": format!("tenant_{}", index % 64),
                "match": {"value": index % 97}
            })
        })
        .collect::<Vec<_>>();

    serde_json::json!({
        "metric": "cosine",
        "fusion": "dbsf",
        "source_k": 250,
        "offset": 5,
        "dense_weight": 1.0,
        "sparse_weight": 0.7,
        "filter": {"must": must}
    })
}

fn options_object_checksum(object: &serde_json::Map<String, serde_json::Value>) -> u64 {
    let mut acc = 0xd6e8_feb8_6659_fd93_u64;
    for (key, value) in object {
        acc = acc.rotate_left(7) ^ key.len() as u64;
        match value {
            serde_json::Value::String(text) => {
                acc ^= text.len() as u64;
            }
            serde_json::Value::Number(number) => {
                acc ^= number.as_u64().unwrap_or(0);
            }
            serde_json::Value::Object(nested) => {
                acc ^= options_object_checksum(nested);
            }
            serde_json::Value::Array(values) => {
                acc ^= values.len() as u64;
                for value in values {
                    if let Some(object) = value.as_object() {
                        acc ^= options_object_checksum(object);
                    }
                }
            }
            serde_json::Value::Bool(value) => {
                acc ^= u64::from(*value);
            }
            serde_json::Value::Null => {}
        }
    }
    acc
}

fn options_clone_parse_checksum(value: &serde_json::Value) -> u64 {
    let parsed = value.clone();
    options_object_checksum(parsed.as_object().expect("options object"))
}

fn options_borrow_parse_checksum(value: &serde_json::Value) -> u64 {
    options_object_checksum(value.as_object().expect("options object"))
}

fn make_range_json_array(count: usize) -> serde_json::Value {
    serde_json::Value::Array(
        (0..count)
            .map(|index| {
                if index % 17 == 0 {
                    serde_json::json!([index as f64 / 10.0, "nested"])
                } else if index % 11 == 0 {
                    serde_json::Value::String("skip".to_owned())
                } else {
                    serde_json::Value::from(index as f64 / 10.0)
                }
            })
            .collect(),
    )
}

fn range_value_is_match(value: f64) -> bool {
    value > 1_000.0 && value < 2_500.0
}

fn range_clone_wrapped_checksum(value: &serde_json::Value) -> u64 {
    let Some(values) = value.as_array() else {
        return 0;
    };
    values.iter().fold(0x8422_2325_cbf2_9ce4, |acc, value| {
        let matched = range_clone_wrapped_value_matches(value);
        acc.rotate_left(5) ^ u64::from(matched)
    })
}

fn range_clone_wrapped_value_matches(value: &serde_json::Value) -> bool {
    let cloned = value.clone();
    match &cloned {
        serde_json::Value::Array(values) => values.iter().any(range_clone_wrapped_value_matches),
        value => value.as_f64().is_some_and(range_value_is_match),
    }
}

fn range_borrow_direct_checksum(value: &serde_json::Value) -> u64 {
    let Some(values) = value.as_array() else {
        return 0;
    };
    values.iter().fold(0x8422_2325_cbf2_9ce4, |acc, value| {
        let matched = range_json_value_matches(value);
        acc.rotate_left(5) ^ u64::from(matched)
    })
}

fn range_json_value_matches(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Array(values) => values.iter().any(range_json_value_matches),
        value => value.as_f64().is_some_and(range_value_is_match),
    }
}

fn make_text_filter_values(count: usize) -> Vec<String> {
    (0..count)
        .map(|index| {
            if index % 3 == 0 {
                format!("Document {index} HYBRID Payload with mixed CASE")
            } else {
                format!("Document {index} dense sparse metadata")
            }
        })
        .collect()
}

fn text_contains_old_checksum(values: &[String], expected: &str) -> u64 {
    values.iter().fold(0xaf63_bd4c_8601_b7df, |acc, value| {
        let matched = value.to_lowercase().contains(&expected.to_lowercase());
        acc.rotate_left(7) ^ u64::from(matched)
    })
}

fn text_contains_new_checksum(values: &[String], expected_lower: &str) -> u64 {
    values.iter().fold(0xaf63_bd4c_8601_b7df, |acc, value| {
        let matched = if expected_lower.is_empty() {
            true
        } else if value.is_ascii() && expected_lower.is_ascii() {
            value
                .as_bytes()
                .windows(expected_lower.len())
                .any(|window| window.eq_ignore_ascii_case(expected_lower.as_bytes()))
        } else {
            value.to_lowercase().contains(expected_lower)
        };
        acc.rotate_left(7) ^ u64::from(matched)
    })
}

fn make_min_should_values(count: usize) -> Vec<bool> {
    (0..count)
        .map(|index| index == 0 || index % 97 == 0)
        .collect()
}

fn min_should_count_all_checksum(values: &[bool], min_count: usize) -> u64 {
    let matched = values.iter().filter(|value| **value).count();
    0x1000_0000_01b3 ^ u64::from(matched >= min_count)
}

fn min_should_short_circuit_checksum(values: &[bool], min_count: usize) -> u64 {
    if min_count == 0 {
        return 0x1000_0000_01b3 ^ 1;
    }
    if min_count > values.len() {
        return 0x1000_0000_01b3;
    }
    let mut matched = 0usize;
    for value in values {
        if *value {
            matched = matched.saturating_add(1);
            if matched >= min_count {
                return 0x1000_0000_01b3 ^ 1;
            }
        }
    }
    0x1000_0000_01b3
}

fn min_should_impossible_short_circuit_checksum(values: &[bool], min_count: usize) -> u64 {
    if min_count == 0 {
        return 0x1000_0000_01b3 ^ 1;
    }
    if min_count > values.len() {
        return 0x1000_0000_01b3;
    }
    min_should_short_circuit_checksum(values, min_count)
}

fn min_should_zero_elided_checksum() -> u64 {
    0x1000_0000_01b3 ^ 1
}

const MIN_SHOULD_INDEX_DIVISORS: [usize; 4] = [97, 101, 103, 107];
const PARTIAL_SHOULD_INDEXED_DIVISOR: usize = 97;
const PARTIAL_SHOULD_NONINDEXED_DIVISOR: usize = 89;

fn make_min_should_index_lists(row_count: usize) -> Vec<Vec<usize>> {
    MIN_SHOULD_INDEX_DIVISORS
        .iter()
        .map(|divisor| (0..row_count).filter(|row| row % divisor == 0).collect())
        .collect()
}

fn min_should_index_table_scan_checksum(row_count: usize, min_count: usize) -> u64 {
    let mut checksum = 0xaf63_bd4c_8601_b7df_u64;
    for row in 0..row_count {
        if min_should_index_row_matches(row, min_count) {
            checksum = checksum.rotate_left(7) ^ row as u64;
        }
    }
    checksum
}

fn min_should_index_prefilter_checksum(index_lists: &[Vec<usize>], min_count: usize) -> u64 {
    min_should_index_candidates(index_lists, min_count)
        .into_iter()
        .fold(0xaf63_bd4c_8601_b7df, |acc, row| {
            acc.rotate_left(7) ^ row as u64
        })
}

fn min_should_index_prefilter_validate_checksum(
    index_lists: &[Vec<usize>],
    min_count: usize,
) -> u64 {
    min_should_index_candidates(index_lists, min_count)
        .into_iter()
        .filter(|row| min_should_index_row_matches(*row, min_count))
        .fold(0xaf63_bd4c_8601_b7df, |acc, row| {
            acc.rotate_left(7) ^ row as u64
        })
}

fn min_should_index_candidates(index_lists: &[Vec<usize>], min_count: usize) -> Vec<usize> {
    if min_count == 0 {
        return Vec::new();
    }
    if min_count > index_lists.len() {
        return Vec::new();
    }
    let mut counts = std::collections::HashMap::<usize, usize>::new();
    let mut candidates = Vec::new();
    for list in index_lists {
        for row in list {
            let count = counts.entry(*row).or_insert(0);
            *count = count.saturating_add(1);
            if *count == min_count {
                candidates.push(*row);
            }
        }
    }
    candidates.sort_unstable();
    candidates
}

fn min_should_index_row_matches(row: usize, min_count: usize) -> bool {
    if min_count == 0 {
        return true;
    }
    let mut matched = 0usize;
    for divisor in MIN_SHOULD_INDEX_DIVISORS {
        if row % divisor == 0 {
            matched = matched.saturating_add(1);
            if matched >= min_count {
                return true;
            }
        }
    }
    false
}

fn make_partial_should_index_list(row_count: usize, divisor: usize) -> Vec<usize> {
    (0..row_count).filter(|row| row % divisor == 0).collect()
}

fn partial_should_old_wasted_index_checksum(row_count: usize, index_list: &[usize]) -> u64 {
    let wasted_index_scan = index_list.iter().fold(0xcbf2_9ce4_8422_2325_u64, |acc, row| {
        acc.rotate_left(5) ^ *row as u64
    });
    black_box(wasted_index_scan);
    partial_should_new_table_scan_checksum(row_count)
}

fn partial_should_new_table_scan_checksum(row_count: usize) -> u64 {
    (0..row_count)
        .filter(|row| {
            row % PARTIAL_SHOULD_INDEXED_DIVISOR == 0
                || row % PARTIAL_SHOULD_NONINDEXED_DIVISOR == 0
        })
        .fold(0xaf63_bd4c_8601_b7df, |acc, row| {
            acc.rotate_left(7) ^ row as u64
        })
}

#[derive(Clone, Default)]
struct RrfOwnedEntry {
    fused_score: f64,
    dense_rank: Option<usize>,
    sparse_rank: Option<usize>,
    payload: Option<serde_json::Value>,
}

#[derive(Clone, Copy, Default)]
struct RrfBorrowedEntry<'a> {
    fused_score: f64,
    dense_rank: Option<usize>,
    sparse_rank: Option<usize>,
    payload: Option<&'a serde_json::Value>,
}

fn hit_id(hit: &serde_json::Map<String, serde_json::Value>) -> i64 {
    hit.get("id")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0)
}

fn payload_title_len(payload: Option<&serde_json::Value>) -> u64 {
    payload
        .and_then(serde_json::Value::as_object)
        .and_then(|payload| payload.get("title"))
        .and_then(serde_json::Value::as_str)
        .map_or(0, |title| title.len() as u64)
}

fn rrf_owned_checksum(values: &[(i64, RrfOwnedEntry)]) -> u64 {
    values
        .iter()
        .fold(0x517c_c1b7_2722_0a95, |acc, (id, entry)| {
            acc.rotate_left(11)
                ^ (*id as u64)
                ^ entry.fused_score.to_bits()
                ^ (entry.dense_rank.unwrap_or(0) as u64)
                ^ ((entry.sparse_rank.unwrap_or(0) as u64) << 13)
                ^ payload_title_len(entry.payload.as_ref())
        })
}

fn rrf_borrowed_checksum(values: &[(i64, RrfBorrowedEntry<'_>)]) -> u64 {
    values
        .iter()
        .fold(0x517c_c1b7_2722_0a95, |acc, (id, entry)| {
            let materialized_payload = entry.payload.cloned();
            acc.rotate_left(11)
                ^ (*id as u64)
                ^ entry.fused_score.to_bits()
                ^ (entry.dense_rank.unwrap_or(0) as u64)
                ^ ((entry.sparse_rank.unwrap_or(0) as u64) << 13)
                ^ payload_title_len(materialized_payload.as_ref())
        })
}

fn rrf_owned_compare(
    left: &(i64, RrfOwnedEntry),
    right: &(i64, RrfOwnedEntry),
) -> std::cmp::Ordering {
    right
        .1
        .fused_score
        .total_cmp(&left.1.fused_score)
        .then_with(|| left.0.cmp(&right.0))
}

fn rrf_borrowed_compare(
    left: &(i64, RrfBorrowedEntry<'_>),
    right: &(i64, RrfBorrowedEntry<'_>),
) -> std::cmp::Ordering {
    right
        .1
        .fused_score
        .total_cmp(&left.1.fused_score)
        .then_with(|| left.0.cmp(&right.0))
}

fn rrf_old_clone_btree_full_sort(
    dense_hits: &[serde_json::Value],
    sparse_hits: &[serde_json::Value],
    k: usize,
) -> u64 {
    let dense = dense_hits
        .iter()
        .filter_map(|hit| hit.as_object().cloned())
        .collect::<Vec<_>>();
    let sparse = sparse_hits
        .iter()
        .filter_map(|hit| hit.as_object().cloned())
        .collect::<Vec<_>>();
    let mut fused = std::collections::BTreeMap::<i64, RrfOwnedEntry>::new();
    let mut seen_dense = std::collections::HashSet::new();
    for (rank, hit) in dense.iter().enumerate() {
        let id = hit_id(hit);
        if seen_dense.insert(id) {
            let rank_1 = rank.saturating_add(1);
            let entry = fused.entry(id).or_default();
            entry.fused_score += 1.0 / (60.0 + rank_1 as f64);
            entry.dense_rank = Some(rank_1);
            if entry.payload.is_none() {
                entry.payload = hit.get("payload").cloned();
            }
        }
    }
    let mut seen_sparse = std::collections::HashSet::new();
    for (rank, hit) in sparse.iter().enumerate() {
        let id = hit_id(hit);
        if seen_sparse.insert(id) {
            let rank_1 = rank.saturating_add(1);
            let entry = fused.entry(id).or_default();
            entry.fused_score += 1.0 / (60.0 + rank_1 as f64);
            entry.sparse_rank = Some(rank_1);
            if entry.payload.is_none() {
                entry.payload = hit.get("payload").cloned();
            }
        }
    }
    let mut ordered = fused.into_iter().collect::<Vec<_>>();
    ordered.sort_by(rrf_owned_compare);
    ordered.truncate(k);
    rrf_owned_checksum(&ordered)
}

fn rrf_new_borrow_hash_partial(
    dense_hits: &[serde_json::Value],
    sparse_hits: &[serde_json::Value],
    k: usize,
) -> u64 {
    let dense = dense_hits
        .iter()
        .filter_map(serde_json::Value::as_object)
        .collect::<Vec<_>>();
    let sparse = sparse_hits
        .iter()
        .filter_map(serde_json::Value::as_object)
        .collect::<Vec<_>>();
    let mut fused = std::collections::HashMap::<i64, RrfBorrowedEntry<'_>>::with_capacity(
        dense.len() + sparse.len(),
    );
    let mut seen_dense = std::collections::HashSet::with_capacity(dense.len());
    for (rank, hit) in dense.iter().copied().enumerate() {
        let id = hit_id(hit);
        if seen_dense.insert(id) {
            let rank_1 = rank.saturating_add(1);
            let entry = fused.entry(id).or_default();
            entry.fused_score += 1.0 / (60.0 + rank_1 as f64);
            entry.dense_rank = Some(rank_1);
            if entry.payload.is_none() {
                entry.payload = hit.get("payload");
            }
        }
    }
    let mut seen_sparse = std::collections::HashSet::with_capacity(sparse.len());
    for (rank, hit) in sparse.iter().copied().enumerate() {
        let id = hit_id(hit);
        if seen_sparse.insert(id) {
            let rank_1 = rank.saturating_add(1);
            let entry = fused.entry(id).or_default();
            entry.fused_score += 1.0 / (60.0 + rank_1 as f64);
            entry.sparse_rank = Some(rank_1);
            if entry.payload.is_none() {
                entry.payload = hit.get("payload");
            }
        }
    }
    let mut ordered = fused.into_iter().collect::<Vec<_>>();
    if ordered.len() > k {
        ordered.select_nth_unstable_by(k - 1, rrf_borrowed_compare);
        ordered.truncate(k);
    }
    ordered.sort_by(rrf_borrowed_compare);
    rrf_borrowed_checksum(&ordered)
}

#[derive(Clone)]
struct DbsfOwnedSource {
    id: i64,
    rank: usize,
    raw_score: f64,
    payload: Option<serde_json::Value>,
}

#[derive(Clone, Copy)]
struct DbsfBorrowedSource<'a> {
    id: i64,
    rank: usize,
    raw_score: f64,
    payload: Option<&'a serde_json::Value>,
}

#[derive(Clone, Default)]
struct DbsfOwnedEntry {
    fused_score: f64,
    dense_rank: Option<usize>,
    sparse_rank: Option<usize>,
    dense_normalized_score: Option<f64>,
    sparse_normalized_score: Option<f64>,
    payload: Option<serde_json::Value>,
}

#[derive(Clone, Copy, Default)]
struct DbsfBorrowedEntry<'a> {
    fused_score: f64,
    dense_rank: Option<usize>,
    sparse_rank: Option<usize>,
    dense_normalized_score: Option<f64>,
    sparse_normalized_score: Option<f64>,
    payload: Option<&'a serde_json::Value>,
}

#[derive(Clone, Copy)]
struct DbsfNormalizer {
    low: f64,
    span: f64,
    fallback: Option<f64>,
}

impl DbsfNormalizer {
    fn constant(value: f64) -> Self {
        Self {
            low: 0.0,
            span: 1.0,
            fallback: Some(value),
        }
    }

    fn normalize(self, score: f64) -> f64 {
        if let Some(value) = self.fallback {
            value
        } else {
            ((score - self.low) / self.span).clamp(0.0, 1.0)
        }
    }
}

fn dbsf_collect_owned(values: &[serde_json::Value]) -> Vec<DbsfOwnedSource> {
    let hits = values
        .iter()
        .filter_map(|hit| hit.as_object().cloned())
        .collect::<Vec<_>>();
    let mut sources = Vec::with_capacity(hits.len());
    let mut seen = std::collections::HashSet::new();
    for (rank, hit) in hits.iter().enumerate() {
        let id = hit_id(hit);
        if seen.insert(id) {
            let raw_score = hit
                .get("score")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.0);
            sources.push(DbsfOwnedSource {
                id,
                rank: rank.saturating_add(1),
                raw_score,
                payload: hit.get("payload").cloned(),
            });
        }
    }
    sources
}

fn dbsf_collect_borrowed(values: &[serde_json::Value]) -> Vec<DbsfBorrowedSource<'_>> {
    let hits = values
        .iter()
        .filter_map(serde_json::Value::as_object)
        .collect::<Vec<_>>();
    let mut sources = Vec::with_capacity(hits.len());
    let mut seen = std::collections::HashSet::with_capacity(hits.len());
    for (rank, hit) in hits.iter().copied().enumerate() {
        let id = hit_id(hit);
        if seen.insert(id) {
            let raw_score = hit
                .get("score")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.0);
            sources.push(DbsfBorrowedSource {
                id,
                rank: rank.saturating_add(1),
                raw_score,
                payload: hit.get("payload"),
            });
        }
    }
    sources
}

fn dbsf_old_normalized_scores<T>(sources: &[T], raw_score: impl Fn(&T) -> f64) -> Vec<f64> {
    if sources.is_empty() {
        return Vec::new();
    }
    let mean = sources.iter().map(&raw_score).sum::<f64>() / sources.len() as f64;
    let variance = sources
        .iter()
        .map(|source| {
            let centered = raw_score(source) - mean;
            centered * centered
        })
        .sum::<f64>()
        / sources.len() as f64;
    let std_dev = variance.sqrt();
    if !mean.is_finite() || !std_dev.is_finite() {
        return vec![0.0; sources.len()];
    }
    if std_dev <= f64::EPSILON {
        return vec![0.5; sources.len()];
    }
    let low = mean - (3.0 * std_dev);
    let high = mean + (3.0 * std_dev);
    let span = high - low;
    if !low.is_finite() || !high.is_finite() || !span.is_finite() || span <= f64::EPSILON {
        return vec![0.5; sources.len()];
    }
    sources
        .iter()
        .map(|source| ((raw_score(source) - low) / span).clamp(0.0, 1.0))
        .collect()
}

fn dbsf_normalizer<T>(sources: &[T], raw_score: impl Fn(&T) -> f64) -> DbsfNormalizer {
    if sources.is_empty() {
        return DbsfNormalizer::constant(0.0);
    }
    let mean = sources.iter().map(&raw_score).sum::<f64>() / sources.len() as f64;
    let variance = sources
        .iter()
        .map(|source| {
            let centered = raw_score(source) - mean;
            centered * centered
        })
        .sum::<f64>()
        / sources.len() as f64;
    let std_dev = variance.sqrt();
    if !mean.is_finite() || !std_dev.is_finite() {
        return DbsfNormalizer::constant(0.0);
    }
    if std_dev <= f64::EPSILON {
        return DbsfNormalizer::constant(0.5);
    }
    let low = mean - (3.0 * std_dev);
    let high = mean + (3.0 * std_dev);
    let span = high - low;
    if !low.is_finite() || !high.is_finite() || !span.is_finite() || span <= f64::EPSILON {
        return DbsfNormalizer::constant(0.5);
    }
    DbsfNormalizer {
        low,
        span,
        fallback: None,
    }
}

fn dbsf_owned_checksum(values: &[(i64, DbsfOwnedEntry)]) -> u64 {
    values
        .iter()
        .fold(0x9e37_79b9_517c_c1b7, |acc, (id, entry)| {
            acc.rotate_left(13)
                ^ (*id as u64)
                ^ entry.fused_score.to_bits()
                ^ entry.dense_normalized_score.unwrap_or(0.0).to_bits()
                ^ entry.sparse_normalized_score.unwrap_or(0.0).to_bits()
                ^ payload_title_len(entry.payload.as_ref())
        })
}

fn dbsf_borrowed_checksum(values: &[(i64, DbsfBorrowedEntry<'_>)]) -> u64 {
    values
        .iter()
        .fold(0x9e37_79b9_517c_c1b7, |acc, (id, entry)| {
            let materialized_payload = entry.payload.cloned();
            acc.rotate_left(13)
                ^ (*id as u64)
                ^ entry.fused_score.to_bits()
                ^ entry.dense_normalized_score.unwrap_or(0.0).to_bits()
                ^ entry.sparse_normalized_score.unwrap_or(0.0).to_bits()
                ^ payload_title_len(materialized_payload.as_ref())
        })
}

fn dbsf_owned_compare(
    left: &(i64, DbsfOwnedEntry),
    right: &(i64, DbsfOwnedEntry),
) -> std::cmp::Ordering {
    right
        .1
        .fused_score
        .total_cmp(&left.1.fused_score)
        .then_with(|| left.0.cmp(&right.0))
}

fn dbsf_borrowed_compare(
    left: &(i64, DbsfBorrowedEntry<'_>),
    right: &(i64, DbsfBorrowedEntry<'_>),
) -> std::cmp::Ordering {
    right
        .1
        .fused_score
        .total_cmp(&left.1.fused_score)
        .then_with(|| left.0.cmp(&right.0))
}

fn dbsf_old_vec_btree_full_sort(
    dense_hits: &[serde_json::Value],
    sparse_hits: &[serde_json::Value],
    k: usize,
) -> u64 {
    let dense = dbsf_collect_owned(dense_hits);
    let sparse = dbsf_collect_owned(sparse_hits);
    let dense_normalized = dbsf_old_normalized_scores(&dense, |hit| hit.raw_score);
    let sparse_normalized = dbsf_old_normalized_scores(&sparse, |hit| hit.raw_score);
    let mut fused = std::collections::BTreeMap::<i64, DbsfOwnedEntry>::new();
    for (hit, normalized_score) in dense.iter().zip(dense_normalized.iter()) {
        let entry = fused.entry(hit.id).or_default();
        entry.fused_score += *normalized_score;
        entry.dense_rank = Some(hit.rank);
        entry.dense_normalized_score = Some(*normalized_score);
        if entry.payload.is_none() {
            entry.payload = hit.payload.clone();
        }
    }
    for (hit, normalized_score) in sparse.iter().zip(sparse_normalized.iter()) {
        let entry = fused.entry(hit.id).or_default();
        entry.fused_score += *normalized_score;
        entry.sparse_rank = Some(hit.rank);
        entry.sparse_normalized_score = Some(*normalized_score);
        if entry.payload.is_none() {
            entry.payload = hit.payload.clone();
        }
    }
    let mut ordered = fused.into_iter().collect::<Vec<_>>();
    ordered.sort_by(dbsf_owned_compare);
    ordered.truncate(k);
    dbsf_owned_checksum(&ordered)
}

fn dbsf_new_normalizer_hash_partial(
    dense_hits: &[serde_json::Value],
    sparse_hits: &[serde_json::Value],
    k: usize,
) -> u64 {
    let dense = dbsf_collect_borrowed(dense_hits);
    let sparse = dbsf_collect_borrowed(sparse_hits);
    let dense_normalizer = dbsf_normalizer(&dense, |hit| hit.raw_score);
    let sparse_normalizer = dbsf_normalizer(&sparse, |hit| hit.raw_score);
    let mut fused = std::collections::HashMap::<i64, DbsfBorrowedEntry<'_>>::with_capacity(
        dense.len() + sparse.len(),
    );
    for hit in &dense {
        let normalized_score = dense_normalizer.normalize(hit.raw_score);
        let entry = fused.entry(hit.id).or_default();
        entry.fused_score += normalized_score;
        entry.dense_rank = Some(hit.rank);
        entry.dense_normalized_score = Some(normalized_score);
        if entry.payload.is_none() {
            entry.payload = hit.payload;
        }
    }
    for hit in &sparse {
        let normalized_score = sparse_normalizer.normalize(hit.raw_score);
        let entry = fused.entry(hit.id).or_default();
        entry.fused_score += normalized_score;
        entry.sparse_rank = Some(hit.rank);
        entry.sparse_normalized_score = Some(normalized_score);
        if entry.payload.is_none() {
            entry.payload = hit.payload;
        }
    }
    let mut ordered = fused.into_iter().collect::<Vec<_>>();
    if ordered.len() > k {
        ordered.select_nth_unstable_by(k - 1, dbsf_borrowed_compare);
        ordered.truncate(k);
    }
    ordered.sort_by(dbsf_borrowed_compare);
    dbsf_borrowed_checksum(&ordered)
}
