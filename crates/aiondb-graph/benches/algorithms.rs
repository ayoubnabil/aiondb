//! Criterion benchmark suite for the graph algorithm engine.
//!
//! Covers the perf-critical algorithm families across a range of graph sizes
//! so regressions (and improvements) are measurable. Graphs are generated
//! deterministically from a seeded LCG so runs are comparable across machines
//! and commits without pulling an RNG dependency.
//!
//! Run all:           `cargo bench -p aiondb-graph`
//! Run one family:    `cargo bench -p aiondb-graph -- pagerank`
//!
//! Sizes and sample counts are tuned so the full suite completes in a few
//! minutes; the `O(V*E)` centralities are capped at small `V` accordingly.

// Benchmarks are throwaway harness code, not shipped API: relax pedantic and
// the closure-style lints (the closures instantiate generic algorithm fns to
// a concrete `&CsrGraph`, so they are not actually redundant).
#![allow(clippy::pedantic, clippy::redundant_closure)]

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use aiondb_graph::algorithms::{
    all_pairs, bellman_ford, centrality, community, connected_components, dijkstra, fast_rp, kcore,
    label_propagation, leiden, longest_path, node2vec, pagerank, sllpa, topological_sort, triangle,
    yen, CsrGraph, WeightedCsrGraph,
};

/// Minimal deterministic LCG (Knuth MMIX constants). No external RNG dep.
struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Self(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        // Mixing the high bits back in improves the low-bit quality the
        // modulo below depends on.
        self.0 ^ (self.0 >> 31)
    }

    fn below(&mut self, bound: u32) -> u32 {
        if bound == 0 {
            return 0;
        }
        (self.next_u64() % u64::from(bound)) as u32
    }
}

/// Build a symmetric (undirected) CSR graph with `n` nodes and roughly
/// `avg_degree` out-edges per node. Each random edge is inserted in both
/// directions so community / triangle / component algorithms see real
/// structure. Self-loops are skipped.
fn build_graph(n: u32, avg_degree: u32, seed: u64) -> CsrGraph {
    let mut rng = Lcg::new(seed);
    let mut edges: Vec<(u32, u32)> = Vec::with_capacity((n as usize) * (avg_degree as usize) * 2);
    for u in 0..n {
        for _ in 0..avg_degree {
            let v = rng.below(n);
            if v != u {
                edges.push((u, v));
                edges.push((v, u));
            }
        }
    }
    CsrGraph::from_edges(n, &edges)
}

const SEED: u64 = 0x00C0_FFEE_CAFE_0042;

/// Benchmark one algorithm over several graph sizes.
fn sweep<R>(
    c: &mut Criterion,
    name: &str,
    sizes: &[u32],
    avg_degree: u32,
    sample_size: usize,
    run: impl Fn(&CsrGraph) -> R,
) {
    let mut group = c.benchmark_group(name);
    group.sample_size(sample_size);
    for &n in sizes {
        let graph = build_graph(n, avg_degree, SEED);
        group.throughput(Throughput::Elements(u64::from(n)));
        group.bench_with_input(BenchmarkId::from_parameter(n), &graph, |b, g| {
            b.iter(|| black_box(run(black_box(g))));
        });
    }
    group.finish();
}

fn bench_traversal_family(c: &mut Criterion) {
    let sizes = [1_000_u32, 10_000, 50_000];
    sweep(c, "pagerank", &sizes, 8, 30, |g| {
        pagerank::pagerank_default(g)
    });
    sweep(c, "personalized_pagerank", &sizes, 8, 30, |g| {
        pagerank::personalized_pagerank_default(g, &[0])
    });
    sweep(c, "connected_components", &sizes, 8, 50, |g| {
        connected_components::connected_components(g)
    });
    sweep(c, "label_propagation", &sizes, 8, 30, |g| {
        label_propagation::label_propagation(g)
    });
    sweep(c, "core_numbers", &sizes, 8, 50, |g| kcore::core_numbers(g));
    // FastRP: O(iters * E * dimension); heavier per node, so fewer samples.
    sweep(c, "fast_rp", &sizes, 8, 15, |g| fast_rp::fast_rp_default(g));
    // Node2Vec biased walk corpus: lighter walk config keeps the bench bounded.
    let n2v_sizes = [1_000_u32, 5_000, 20_000];
    sweep(c, "node2vec", &n2v_sizes, 8, 10, |g| {
        node2vec::node2vec_walks(
            g,
            &node2vec::Node2VecConfig {
                walk_length: 20,
                walks_per_node: 3,
                ..node2vec::Node2VecConfig::default()
            },
        )
    });
}

fn bench_community_family(c: &mut Criterion) {
    let sizes = [1_000_u32, 5_000, 20_000];
    sweep(c, "louvain", &sizes, 10, 20, |g| community::louvain(g));
    sweep(c, "leiden", &sizes, 10, 20, |g| leiden::leiden(g));
}

fn bench_triangle_family(c: &mut Criterion) {
    // O(V * d^2): keep degree and size modest.
    let sizes = [1_000_u32, 5_000, 15_000];
    sweep(c, "triangle_count", &sizes, 8, 30, |g| {
        triangle::triangle_count(g)
    });
}

fn bench_centrality_family(c: &mut Criterion) {
    // O(V * E) Brandes / BFS-from-every-source: small V, few samples.
    let sizes = [300_u32, 600, 1_200];
    sweep(c, "betweenness_centrality", &sizes, 6, 10, |g| {
        centrality::betweenness_centrality(g)
    });
    sweep(c, "closeness_centrality", &sizes, 6, 10, |g| {
        centrality::closeness_centrality(g)
    });
}

/// Directed weighted graph with positive weights in `[1, 10]` for the
/// shortest-path benchmarks (the negative-cycle path is exceptional, not the
/// throughput case worth measuring).
fn build_weighted(n: u32, avg_degree: u32, seed: u64) -> WeightedCsrGraph {
    let mut rng = Lcg::new(seed);
    let mut edges: Vec<(u32, u32, f64)> = Vec::with_capacity((n as usize) * (avg_degree as usize));
    for u in 0..n {
        for _ in 0..avg_degree {
            let v = rng.below(n);
            if v != u {
                let w = f64::from(1 + rng.below(10));
                edges.push((u, v, w));
            }
        }
    }
    WeightedCsrGraph::from_edges(n, &edges)
}

fn bench_weighted_paths(c: &mut Criterion) {
    // O(V * E) with early exit: keep sizes / degree modest.
    let sizes = [1_000_u32, 5_000, 15_000];
    let mut group = c.benchmark_group("bellman_ford");
    group.sample_size(20);
    for &n in &sizes {
        let graph = build_weighted(n, 6, SEED);
        group.throughput(Throughput::Elements(u64::from(n)));
        group.bench_with_input(BenchmarkId::from_parameter(n), &graph, |b, g| {
            b.iter(|| black_box(bellman_ford::bellman_ford(black_box(g), 0)));
        });
    }
    group.finish();

    let mut dgroup = c.benchmark_group("dijkstra");
    dgroup.sample_size(30);
    for &n in &sizes {
        let graph = build_weighted(n, 6, SEED);
        dgroup.throughput(Throughput::Elements(u64::from(n)));
        dgroup.bench_with_input(BenchmarkId::from_parameter(n), &graph, |b, g| {
            b.iter(|| black_box(dijkstra::dijkstra(black_box(g), 0)));
        });
    }
    dgroup.finish();

    // Yen's k-shortest is O(k * V * (E + V log V)): small graphs, small k.
    let yen_sizes = [200_u32, 500, 1_000];
    let mut ygroup = c.benchmark_group("yen_k_shortest");
    ygroup.sample_size(10);
    for &n in &yen_sizes {
        let graph = build_weighted(n, 6, SEED);
        ygroup.throughput(Throughput::Elements(u64::from(n)));
        ygroup.bench_with_input(BenchmarkId::from_parameter(n), &graph, |b, g| {
            b.iter(|| black_box(yen::yen_k_shortest_paths(black_box(g), 0, n - 1, 5)));
        });
    }
    ygroup.finish();

    let mut wgroup = c.benchmark_group("weighted_pagerank");
    wgroup.sample_size(20);
    for &n in &[1_000_u32, 10_000, 50_000] {
        let graph = build_weighted(n, 8, SEED);
        wgroup.throughput(Throughput::Elements(u64::from(n)));
        wgroup.bench_with_input(BenchmarkId::from_parameter(n), &graph, |b, g| {
            b.iter(|| black_box(pagerank::weighted_pagerank_default(black_box(g))));
        });
    }
    wgroup.finish();
}

fn bench_new_algorithms(c: &mut Criterion) {
    let sizes = [1_000_u32, 10_000, 50_000];
    sweep(c, "katz_centrality", &sizes, 8, 20, |g| {
        centrality::katz_centrality(g, None, None, Some(50), Some(1e-9))
    });
    sweep(c, "topological_sort", &sizes, 8, 50, |g| {
        topological_sort::topological_sort(g)
    });
    sweep(c, "longest_path", &sizes, 8, 50, |g| {
        longest_path::longest_path(g)
    });
    sweep(c, "sllpa", &[1_000_u32, 5_000, 20_000], 8, 10, |g| {
        sllpa::sllpa_dominant(
            g,
            &sllpa::SllpaConfig {
                iterations: 20,
                ..sllpa::SllpaConfig::default()
            },
        )
    });
    sweep(
        c,
        "sampled_betweenness",
        &[1_000_u32, 5_000, 20_000],
        6,
        10,
        |g| centrality::betweenness_centrality_sampled(g, 50, 7),
    );
    sweep(
        c,
        "all_pairs_shortest_paths",
        &[300_u32, 600, 1_200],
        6,
        10,
        |g| all_pairs::all_pairs_shortest_paths(g),
    );
}

criterion_group!(
    benches,
    bench_traversal_family,
    bench_community_family,
    bench_triangle_family,
    bench_centrality_family,
    bench_weighted_paths,
    bench_new_algorithms,
);
criterion_main!(benches);
