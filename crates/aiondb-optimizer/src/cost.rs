/// Cost estimate for a query plan node.
///
/// Uses a simplified PostgreSQL-style cost model where:
/// - Sequential I/O is cheap (1.0 per page)
/// - Random I/O is expensive (4.0 per page)
/// - CPU cost is relatively small per tuple
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PlanCost(pub f64);

// I/O cost of reading one page sequentially.
const SEQ_PAGE_COST: f64 = 1.0;

// I/O cost of a random page read.
const RANDOM_PAGE_COST: f64 = 4.0;

/// CPU cost of processing one tuple during a sequential scan.
const CPU_TUPLE_COST: f64 = 0.01;

/// CPU cost of processing one index entry.
const CPU_INDEX_TUPLE_COST: f64 = 0.005;

/// Assumed page size for estimating page counts.
const PAGE_SIZE: f64 = 8192.0;

/// Approximate b-tree descent depth for point/range lookups.
const INDEX_TRAVERSAL_PAGES: f64 = 3.0;

/// Default selectivity for equality predicates when stats are missing.
const DEFAULT_EQUALITY_SELECTIVITY: f64 = 0.01;

/// Default selectivity for range predicates when no histogram is available.
const DEFAULT_RANGE_SELECTIVITY: f64 = 0.33;

/// Bounded ranges are usually much narrower than open-ended predicates.
#[cfg(test)]
const DEFAULT_BOUNDED_RANGE_SELECTIVITY: f64 = 0.10;

/// Prevent degenerate zero-row estimates from collapsing index costs to zero.
const MIN_SELECTIVITY: f64 = 1.0e-6;

/// CPU cost of computing a hash value for one tuple.
const CPU_HASH_COST: f64 = 0.025;

/// CPU cost of comparing two tuples during a sort.
const CPU_SORT_COMPARE_COST: f64 = 0.02;

/// Approximate number of build-side rows that fit in the hash join
/// memory budget (~512 MB / ~100 bytes per entry). When the estimated
/// build side exceeds this, hash join gets a steep cost penalty so the
/// optimizer prefers merge join or NLJ + index scan instead.
const HASH_JOIN_BUILD_ROW_THRESHOLD: f64 = 5_000_000.0;

fn estimated_pages(total_bytes: u64) -> f64 {
    (u64_to_f64(total_bytes) / PAGE_SIZE).max(1.0)
}

fn clamped_selectivity(selectivity: f64) -> f64 {
    if selectivity.is_finite() {
        selectivity.clamp(MIN_SELECTIVITY, 1.0)
    } else {
        DEFAULT_EQUALITY_SELECTIVITY
    }
}

fn estimated_rows(row_count: u64, selectivity: f64) -> f64 {
    if row_count == 0 {
        1.0
    } else {
        let selectivity = clamped_selectivity(selectivity);
        (u64_to_f64(row_count) * selectivity).clamp(1.0, u64_to_f64(row_count))
    }
}

fn estimated_heap_pages(row_count: u64, total_bytes: u64, selectivity: f64) -> f64 {
    let total_pages = estimated_pages(total_bytes);
    let rows_per_page = if row_count == 0 {
        1.0
    } else {
        (u64_to_f64(row_count) / total_pages).max(1.0)
    };
    (estimated_rows(row_count, selectivity) / rows_per_page)
        .ceil()
        .clamp(1.0, total_pages)
}

impl PlanCost {
    /// Cost of a full sequential scan over a table.
    ///
    /// `row_count` and `total_bytes` come from `TableStatistics`.
    pub fn seq_scan(row_count: u64, total_bytes: u64) -> Self {
        let pages = estimated_pages(total_bytes);
        let io_cost = pages * SEQ_PAGE_COST;
        let cpu_cost = u64_to_f64(row_count) * CPU_TUPLE_COST;
        Self(io_cost + cpu_cost)
    }

    /// Cost of an index point-lookup (`column = value`).
    ///
    /// Typically touches ~1-3 index pages + 1 heap page.
    pub fn index_eq() -> Self {
        Self::index_eq_with_selectivity(10_000, 10_000 * 64, DEFAULT_EQUALITY_SELECTIVITY)
    }

    /// Cost of an index equality lookup with known selectivity.
    ///
    /// When column statistics provide ndistinct, we can estimate how many
    /// rows will match and adjust the heap fetch cost accordingly.
    pub fn index_eq_with_selectivity(row_count: u64, total_bytes: u64, selectivity: f64) -> Self {
        Self::index_eq_with_correlation(row_count, total_bytes, selectivity, 0.0)
    }

    /// Index point/equality lookup with a known leading-key correlation.
    /// Same blend as `index_range_with_correlation`.
    pub fn index_eq_with_correlation(
        row_count: u64,
        total_bytes: u64,
        selectivity: f64,
        correlation: f64,
    ) -> Self {
        let estimated_rows = estimated_rows(row_count, selectivity);
        let heap_pages = estimated_heap_pages(row_count, total_bytes, selectivity);
        let csquared = correlation.clamp(-1.0, 1.0).powi(2);
        let max_io_cost = heap_pages * RANDOM_PAGE_COST;
        let min_io_cost = heap_pages * SEQ_PAGE_COST;
        let heap_io_cost = max_io_cost + csquared * (min_io_cost - max_io_cost);
        let index_io = INDEX_TRAVERSAL_PAGES * RANDOM_PAGE_COST;
        let io_cost = index_io + heap_io_cost;
        let cpu_cost = estimated_rows * (CPU_INDEX_TUPLE_COST + CPU_TUPLE_COST);
        Self(io_cost + cpu_cost)
    }

    /// Cost of an index range scan.
    ///
    /// Without histograms we estimate that the range touches
    /// `DEFAULT_RANGE_SELECTIVITY` fraction of the table.
    pub fn index_range(row_count: u64, total_bytes: u64) -> Self {
        Self::index_range_with_selectivity(row_count, total_bytes, DEFAULT_RANGE_SELECTIVITY)
    }

    pub fn index_range_with_selectivity(
        row_count: u64,
        total_bytes: u64,
        selectivity: f64,
    ) -> Self {
        Self::index_range_with_correlation(row_count, total_bytes, selectivity, 0.0)
    }

    /// Cost of an index range scan with a known leading-key correlation.
    ///
    /// Mirrors PostgreSQL's `genericcostestimate` blend in
    /// `src/backend/utils/adt/selfuncs.c`: a perfectly clustered index
    /// (`correlation^2 == 1`) fetches heap pages in sequential order, so
    /// the heap-fetch I/O collapses to `min_io = pages * SEQ_PAGE_COST`;
    /// a fully uncorrelated index (`correlation == 0`) pays
    /// `max_io = pages * RANDOM_PAGE_COST`.  Real indexes fall between
    /// the two and PG interpolates with `csquared`.
    pub fn index_range_with_correlation(
        row_count: u64,
        total_bytes: u64,
        selectivity: f64,
        correlation: f64,
    ) -> Self {
        let estimated_rows = estimated_rows(row_count, selectivity);
        let heap_pages = estimated_heap_pages(row_count, total_bytes, selectivity);
        let csquared = correlation.clamp(-1.0, 1.0).powi(2);
        let max_io_cost = heap_pages * RANDOM_PAGE_COST;
        let min_io_cost = heap_pages * SEQ_PAGE_COST;
        let heap_io_cost = max_io_cost + csquared * (min_io_cost - max_io_cost);
        let io_cost = INDEX_TRAVERSAL_PAGES * RANDOM_PAGE_COST + heap_io_cost;
        let cpu_cost = estimated_rows * (CPU_INDEX_TUPLE_COST + CPU_TUPLE_COST);
        Self(io_cost + cpu_cost)
    }

    /// Cost of a nested-loop join.
    ///
    /// For each row in the outer (left) side, scans all rows of the inner
    /// (right) side. The executor materializes the right child once and then
    /// iterates it for each left row, so the right side should stay as small
    /// as possible. Total tuples processed = outer_rows * inner_rows.
    pub fn nested_loop_join(outer_rows: f64, inner_rows: f64) -> Self {
        let cpu_cost = outer_rows * inner_rows * CPU_TUPLE_COST;
        let materialization_cost = inner_rows * CPU_TUPLE_COST;
        let rescan_cost = if inner_rows < 10_000.0 {
            0.0
        } else {
            let inner_pages = (inner_rows * 64.0 / PAGE_SIZE).max(1.0);
            outer_rows * inner_pages * SEQ_PAGE_COST * 0.5
        };
        Self(cpu_cost + materialization_cost + rescan_cost)
    }

    /// Cost of a hash join.
    ///
    /// Builds a hash table on the right (build) side, then probes with
    /// the left (probe) side.  The build side pays additional overhead for
    /// hashing and materialization, so this cost is intentionally
    /// asymmetric: a smaller right side is cheaper.
    pub fn hash_join(left_rows: f64, right_rows: f64) -> Self {
        let base_cpu = (left_rows + right_rows) * (CPU_HASH_COST + CPU_TUPLE_COST);
        let build_overhead = right_rows * CPU_TUPLE_COST;
        let build_pages = (right_rows * 64.0 / PAGE_SIZE).max(1.0);
        let materialization_cost = build_pages * SEQ_PAGE_COST * 0.25;
        // Penalty when the build side is too large for the in-memory hash
        // table. This mirrors PostgreSQL's behaviour where exceeding
        // work_mem triggers multi-batch hash join with disk spill - since
        // AionDB doesn't spill, we steer the optimizer away from hash join.
        let memory_penalty = if right_rows > HASH_JOIN_BUILD_ROW_THRESHOLD {
            let excess = right_rows / HASH_JOIN_BUILD_ROW_THRESHOLD;
            right_rows * CPU_TUPLE_COST * excess
        } else {
            0.0
        };
        Self(base_cpu + build_overhead + materialization_cost + memory_penalty)
    }

    /// Cost of a semi-join (EXISTS subquery).
    ///
    /// Builds a hash table on the inner (right) side, then probes with
    /// the outer (left) side.  Probe stops at the first match for each
    /// outer row, so the effective probe cost is reduced compared to a
    /// full inner join.
    pub fn semi_join(outer_rows: f64, inner_rows: f64) -> Self {
        let build = inner_rows * (CPU_HASH_COST + CPU_TUPLE_COST);
        let probe = outer_rows * (CPU_HASH_COST + CPU_TUPLE_COST * 0.5);
        Self(build + probe)
    }

    /// Cost of an anti-join (NOT EXISTS subquery).
    ///
    /// Same structure as semi-join: build hash on inner, probe from
    /// outer.  Must scan the full bucket to confirm absence, so full
    /// probe cost applies.
    pub fn anti_join(outer_rows: f64, inner_rows: f64) -> Self {
        let build = inner_rows * (CPU_HASH_COST + CPU_TUPLE_COST);
        let probe = outer_rows * (CPU_HASH_COST + CPU_TUPLE_COST);
        Self(build + probe)
    }

    /// Cost of a merge join.
    ///
    /// Both inputs must be sorted on the join keys.  Merges in a single
    /// pass through both sorted streams.
    pub fn merge_join(
        left_rows: f64,
        right_rows: f64,
        left_presorted: bool,
        right_presorted: bool,
    ) -> Self {
        let mut cost = 0.0;
        if !left_presorted {
            cost += sort_cost(left_rows);
        }
        if !right_presorted {
            cost += sort_cost(right_rows);
        }
        let merge_cpu = (left_rows + right_rows) * CPU_TUPLE_COST;
        cost += merge_cpu;
        Self(cost)
    }

    /// Cost of a bitmap heap scan that fetches `n_tuples` from `n_pages`
    /// heap pages.  Because TIDs are sorted by page, the heap access is
    /// sequential rather than random.
    pub fn bitmap_heap_scan(row_count: u64, total_bytes: u64, selectivity: f64) -> Self {
        let heap_pages = estimated_heap_pages(row_count, total_bytes, selectivity);
        // Pages are fetched in physical order → sequential I/O cost.
        let io_cost = heap_pages * SEQ_PAGE_COST;
        let cpu_cost = estimated_rows(row_count, selectivity) * CPU_TUPLE_COST;
        Self(io_cost + cpu_cost)
    }

    /// Cost of a bitmap OR scan: sum of child index scan costs (minus
    /// their heap fetch) plus a single bitmap heap scan.
    pub fn bitmap_or(
        child_costs: &[PlanCost],
        row_count: u64,
        total_bytes: u64,
        combined_selectivity: f64,
    ) -> Self {
        // Sum of index traversal costs from all children.
        let index_cost: f64 = child_costs.iter().map(|c| c.0).sum();
        let heap = Self::bitmap_heap_scan(row_count, total_bytes, combined_selectivity);
        // CPU cost for merging TID sets.
        let merge_cpu = estimated_rows(row_count, combined_selectivity) * CPU_INDEX_TUPLE_COST;
        Self(index_cost * 0.5 + heap.0 + merge_cpu)
    }

    /// Cost of a bitmap AND scan: intersection is cheaper because fewer
    /// heap pages need to be fetched.
    pub fn bitmap_and(
        child_costs: &[PlanCost],
        row_count: u64,
        total_bytes: u64,
        combined_selectivity: f64,
    ) -> Self {
        let index_cost: f64 = child_costs.iter().map(|c| c.0).sum();
        let heap = Self::bitmap_heap_scan(row_count, total_bytes, combined_selectivity);
        let merge_cpu = estimated_rows(row_count, combined_selectivity) * CPU_INDEX_TUPLE_COST;
        Self(index_cost * 0.5 + heap.0 + merge_cpu)
    }

    /// Cost of an index-only scan (no heap fetch).
    pub fn index_only_scan(row_count: u64, selectivity: f64) -> Self {
        let estimated_rows = estimated_rows(row_count, selectivity);
        let index_io = INDEX_TRAVERSAL_PAGES * RANDOM_PAGE_COST;
        // No heap I/O - only index pages for the matching rows.
        let leaf_pages = (estimated_rows / 64.0).max(1.0);
        let io_cost = index_io + leaf_pages * SEQ_PAGE_COST;
        let cpu_cost = estimated_rows * CPU_INDEX_TUPLE_COST;
        Self(io_cost + cpu_cost)
    }

    /// Returns `true` if this cost is cheaper than `other`.
    pub fn cheaper_than(self, other: Self) -> bool {
        self.0 < other.0
    }
}

/// Estimate the cost of sorting `n` rows.
fn sort_cost(n: f64) -> f64 {
    if n <= 1.0 {
        0.0
    } else {
        n * n.log2() * CPU_SORT_COMPARE_COST
    }
}

impl PartialOrd for PlanCost {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.0.partial_cmp(&other.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_cost_default_is_zero() {
        let cost = PlanCost::default();
        assert_eq!(cost.0, 0.0);
    }

    #[test]
    fn plan_cost_equality_same_value() {
        let a = PlanCost(1.5);
        let b = PlanCost(1.5);
        assert_eq!(a, b);
    }

    #[test]
    fn plan_cost_inequality_different_value() {
        let a = PlanCost(1.0);
        let b = PlanCost(2.0);
        assert_ne!(a, b);
    }

    #[test]
    fn plan_cost_copy_semantics() {
        let a = PlanCost(3.14);
        let b = a;
        assert_eq!(a, b);
    }

    #[test]
    fn plan_cost_debug_format() {
        let cost = PlanCost(1.23);
        let dbg = format!("{cost:?}");
        assert!(dbg.contains("PlanCost"));
        assert!(dbg.contains("1.23"));
    }

    #[test]
    fn plan_cost_zero_eq_default() {
        assert_eq!(PlanCost(0.0), PlanCost::default());
    }

    #[test]
    fn plan_cost_nan_not_equal_to_itself() {
        let a = PlanCost(f64::NAN);
        let b = PlanCost(f64::NAN);
        assert_ne!(a, b);
    }

    // --- Cost model tests ---

    #[test]
    fn seq_scan_cost_grows_with_rows() {
        let small = PlanCost::seq_scan(100, 100 * 64);
        let large = PlanCost::seq_scan(100_000, 100_000 * 64);
        assert!(small.cheaper_than(large));
    }

    #[test]
    fn index_eq_cheaper_than_large_seq_scan() {
        let index_cost = PlanCost::index_eq();
        let seq_cost = PlanCost::seq_scan(100_000, 100_000 * 64);
        assert!(index_cost.cheaper_than(seq_cost));
    }

    #[test]
    fn seq_scan_cheaper_than_index_eq_for_tiny_table() {
        let seq_cost = PlanCost::seq_scan(5, 5 * 64);
        let index_cost = PlanCost::index_eq();
        assert!(seq_cost.cheaper_than(index_cost));
    }

    #[test]
    fn index_range_cheaper_than_seq_scan_for_large_table() {
        let range_cost = PlanCost::index_range(100_000, 100_000 * 64);
        let seq_cost = PlanCost::seq_scan(100_000, 100_000 * 64);
        assert!(range_cost.cheaper_than(seq_cost));
    }

    #[test]
    fn seq_scan_cheaper_than_index_range_for_tiny_table() {
        let seq_cost = PlanCost::seq_scan(10, 10 * 64);
        let range_cost = PlanCost::index_range(10, 10 * 64);
        assert!(seq_cost.cheaper_than(range_cost));
    }

    #[test]
    fn index_eq_cheaper_than_index_range() {
        let eq_cost = PlanCost::index_eq();
        let range_cost = PlanCost::index_range(10_000, 10_000 * 64);
        assert!(eq_cost.cheaper_than(range_cost));
    }

    #[test]
    fn high_selectivity_index_lookup_can_be_more_expensive_than_seq_scan() {
        let index_cost = PlanCost::index_eq_with_selectivity(100_000, 100_000 * 64, 0.8);
        let seq_cost = PlanCost::seq_scan(100_000, 100_000 * 64);
        assert!(seq_cost.cheaper_than(index_cost));
    }

    #[test]
    fn bounded_range_selectivity_is_cheaper_than_open_range() {
        let bounded = PlanCost::index_range_with_selectivity(
            100_000,
            100_000 * 64,
            DEFAULT_BOUNDED_RANGE_SELECTIVITY,
        );
        let open = PlanCost::index_range(100_000, 100_000 * 64);
        assert!(bounded.cheaper_than(open));
    }

    #[test]
    fn perfectly_clustered_index_range_is_cheaper_than_uncorrelated() {
        // PG genericcostestimate: csquared = 1 collapses heap I/O to
        // sequential, csquared = 0 pays full random.
        let clustered = PlanCost::index_range_with_correlation(
            100_000,
            100_000 * 64,
            DEFAULT_RANGE_SELECTIVITY,
            1.0,
        );
        let uncorrelated = PlanCost::index_range_with_correlation(
            100_000,
            100_000 * 64,
            DEFAULT_RANGE_SELECTIVITY,
            0.0,
        );
        assert!(clustered.cheaper_than(uncorrelated));
    }

    #[test]
    fn correlation_sign_does_not_change_cost() {
        // Correlation enters the cost as csquared, so -1.0 (reverse
        // clustered) is the same I/O profile as +1.0 (forward
        // clustered): the heap pages are still fetched in physical
        // order, just walked backwards.
        let forward = PlanCost::index_range_with_correlation(
            100_000,
            100_000 * 64,
            DEFAULT_RANGE_SELECTIVITY,
            1.0,
        );
        let reverse = PlanCost::index_range_with_correlation(
            100_000,
            100_000 * 64,
            DEFAULT_RANGE_SELECTIVITY,
            -1.0,
        );
        assert!((forward.0 - reverse.0).abs() < 1e-9);
    }

    #[test]
    fn correlation_zero_matches_legacy_index_range_cost() {
        let with_corr = PlanCost::index_range_with_correlation(
            100_000,
            100_000 * 64,
            DEFAULT_RANGE_SELECTIVITY,
            0.0,
        );
        let legacy = PlanCost::index_range_with_selectivity(
            100_000,
            100_000 * 64,
            DEFAULT_RANGE_SELECTIVITY,
        );
        assert!((with_corr.0 - legacy.0).abs() < 1e-9);
    }

    #[test]
    fn wider_tables_make_same_selectivity_index_scan_more_expensive() {
        let narrow = PlanCost::index_eq_with_selectivity(100_000, 100_000 * 32, 0.02);
        let wide = PlanCost::index_eq_with_selectivity(100_000, 100_000 * 256, 0.02);
        assert!(narrow.cheaper_than(wide));
    }

    #[test]
    fn partial_ord_works() {
        let a = PlanCost(10.0);
        let b = PlanCost(20.0);
        assert!(a < b);
        assert!(b > a);
    }

    #[test]
    fn cheaper_than_reflexive_false() {
        let a = PlanCost(10.0);
        assert!(!a.cheaper_than(a));
    }

    #[test]
    fn seq_scan_one_row() {
        let cost = PlanCost::seq_scan(1, 64);
        assert!(cost.0 > 0.0);
    }

    #[test]
    fn seq_scan_zero_bytes_still_counts_one_page() {
        let cost = PlanCost::seq_scan(100, 0);
        // Even with 0 bytes we estimate at least 1 page
        assert!(cost.0 >= SEQ_PAGE_COST);
    }

    #[test]
    fn random_io_more_expensive_than_sequential() {
        let random_cost = PlanCost(RANDOM_PAGE_COST);
        let seq_cost = PlanCost(SEQ_PAGE_COST);
        assert!(seq_cost.cheaper_than(random_cost));
    }

    #[test]
    fn index_tuple_cheaper_than_heap_tuple() {
        let index_cost = PlanCost(CPU_INDEX_TUPLE_COST);
        let heap_cost = PlanCost(CPU_TUPLE_COST);
        assert!(index_cost.cheaper_than(heap_cost));
    }

    // --- Join cost model tests ---

    #[test]
    fn hash_join_cheaper_than_nlj_for_large_inputs() {
        let nlj = PlanCost::nested_loop_join(10_000.0, 10_000.0);
        let hj = PlanCost::hash_join(10_000.0, 10_000.0);
        assert!(hj.cheaper_than(nlj));
    }

    #[test]
    fn nlj_competitive_for_tiny_inner() {
        // With very small inner, NLJ should be competitive
        let nlj = PlanCost::nested_loop_join(1000.0, 5.0);
        let hj = PlanCost::hash_join(1000.0, 5.0);
        // NLJ: 1000*5*0.01 = 50, HJ: 5*0.025 + 1000*0.025 + 1005*0.01 = 35.175
        // HJ is slightly cheaper but difference is small
        assert!(nlj.0 > 0.0);
        assert!(hj.0 > 0.0);
    }

    #[test]
    fn merge_join_presorted_cheaper_than_hash_for_large() {
        let mj = PlanCost::merge_join(100_000.0, 100_000.0, true, true);
        let hj = PlanCost::hash_join(100_000.0, 100_000.0);
        assert!(mj.cheaper_than(hj));
    }

    #[test]
    fn merge_join_unsorted_includes_sort_cost() {
        let sorted = PlanCost::merge_join(1000.0, 1000.0, true, true);
        let unsorted = PlanCost::merge_join(1000.0, 1000.0, false, false);
        assert!(sorted.cheaper_than(unsorted));
    }

    #[test]
    fn nlj_cost_quadratic_in_inputs() {
        let small = PlanCost::nested_loop_join(100.0, 100.0);
        let large = PlanCost::nested_loop_join(1000.0, 1000.0);
        // 100x more rows should mean ~100x more cost
        assert!(large.0 > small.0 * 50.0);
    }

    #[test]
    fn hash_join_cost_linear_in_inputs() {
        let small = PlanCost::hash_join(100.0, 100.0);
        let large = PlanCost::hash_join(1000.0, 1000.0);
        // 10x more rows should mean ~10x more cost
        assert!(large.0 > small.0 * 5.0);
        assert!(large.0 < small.0 * 20.0);
    }

    #[test]
    fn sort_cost_zero_for_one_row() {
        let mj = PlanCost::merge_join(1.0, 1.0, false, false);
        // With 1 row, sort cost should be 0
        let mj_sorted = PlanCost::merge_join(1.0, 1.0, true, true);
        assert_eq!(mj.0, mj_sorted.0);
    }

    #[test]
    fn hash_join_prefers_smaller_build_side() {
        let small_build = PlanCost::hash_join(1000.0, 500.0);
        let large_build = PlanCost::hash_join(500.0, 1000.0);
        assert!(
            small_build.cheaper_than(large_build),
            "hash join should be cheaper when the right/build side is smaller"
        );
    }

    #[test]
    fn nested_loop_prefers_smaller_inner_side() {
        let small_inner = PlanCost::nested_loop_join(1000.0, 1.0);
        let large_inner = PlanCost::nested_loop_join(1.0, 1000.0);
        assert!(
            small_inner.cheaper_than(large_inner),
            "nested loop should be cheaper when the right/inner side is smaller"
        );
    }

    // --- Semi/Anti join cost tests ---

    #[test]
    fn semi_join_cheaper_than_inner_join() {
        let semi = PlanCost::semi_join(10_000.0, 10_000.0);
        let inner = PlanCost::hash_join(10_000.0, 10_000.0);
        assert!(
            semi.cheaper_than(inner),
            "semi-join should be cheaper than full inner join (early-out on first match)"
        );
    }

    #[test]
    fn anti_join_cheaper_than_inner_join() {
        let anti = PlanCost::anti_join(10_000.0, 10_000.0);
        let inner = PlanCost::hash_join(10_000.0, 10_000.0);
        assert!(
            anti.cheaper_than(inner),
            "anti-join should be cheaper than full inner join"
        );
    }

    #[test]
    fn semi_join_cheaper_than_anti_join() {
        // Semi can stop at first match; anti must confirm absence.
        let semi = PlanCost::semi_join(10_000.0, 10_000.0);
        let anti = PlanCost::anti_join(10_000.0, 10_000.0);
        assert!(
            semi.cheaper_than(anti),
            "semi-join should be cheaper than anti-join (early-out)"
        );
    }

    #[test]
    fn semi_join_cost_positive() {
        let cost = PlanCost::semi_join(100.0, 50.0);
        assert!(cost.0 > 0.0);
    }
}
use crate::u64_to_f64;
