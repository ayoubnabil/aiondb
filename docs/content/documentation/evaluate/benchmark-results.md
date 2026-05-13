---
title: Benchmark Results
order: 71
---

# Benchmark Results

<div class="bench-kpi-grid">
<div class="bench-kpi-card"><strong>full-all-20260512T192959Z</strong><span>Run</span></div>
<div class="bench-kpi-card"><strong>76/76</strong><span>surreal-suite tests</span></div>
<div class="bench-kpi-card"><strong>228/228</strong><span>engine measurements</span></div>
<div class="bench-kpi-card"><strong>3</strong><span>server engines</span></div>
</div>

This page currently publishes the full **surreal-suite server benchmark**:
AionDB over PostgreSQL wire, SurrealDB over WebSocket, and the PostgreSQL stack.
It does **not** include the other benchmark families in the repository
(`embedded-compare`, `pgbench`, `crud-bench-official`, `tpch`, `tpcds`, or
`job`) in this snapshot.

<div class="bench-visual">
<div class="bench-toolbar">
<label>Category <select id="bench-category"></select></label>
<label>Metric <select id="bench-metric"><option value="ops">ops/s</option><option value="ratio">vs pgstack</option></select></label>
<label>Scale <select id="bench-scale"><option value="linear">linear</option><option value="log">log</option></select></label>
<label>Sort <select id="bench-sort"><option value="test">test</option><option value="aiondb">AionDB</option><option value="surrealdb">SurrealDB</option><option value="pgstack">pgstack</option></select></label>
</div>
<div class="bench-legend" id="bench-legend"></div>
<div class="bench-canvas-wrap">
<div id="bench-chart" class="bench-chart-host"></div>
<div class="bench-tooltip" id="bench-tooltip" hidden></div>
</div>
</div>
<script id="bench-results-data" type="application/json" data-src="/documentation/evaluate/benchmark-results.json?v=full-all-20260512T192959Z-tablefix"></script>
<script src="/d3.min.js" defer></script>
<script src="/benchmark-results.js?v=full-all-20260512T192959Z-aiondb-sort" defer></script>

<p class="bench-download">
Download the latest benchmark snapshot as JSON: <a href="/documentation/evaluate/benchmark-results.json?v=full-all-20260512T192959Z-tablefix" download>benchmark-results.json</a>
</p>

## Raw Summary

Values are measured throughput in **operations per second** (`ops/s`) with mean
per-operation latency (`ms`) shown next to each cell. The last column is the
ratio of AionDB ops/s over the PostgreSQL stack ops/s (`>1×` means AionDB is
faster than the PostgreSQL stack on that test).

This snapshot compares AionDB server, SurrealDB WS, and the PostgreSQL stack.
CockroachDB was not included in this run.

<div class="bench-rank-legend" aria-label="AionDB ranking legend">
<span><i class="bench-rank-win"></i>AionDB 1<sup>st</sup> (fastest of the three)</span>
<span><i class="bench-rank-mid"></i>AionDB 2<sup>nd</sup> (beats SurrealDB only)</span>
<span><i class="bench-rank-loss"></i>AionDB last (loses to SurrealDB)</span>
</div>

<table class="bench-summary-table">
<thead>
<tr>
<th>Test</th>
<th>Category</th>
<th>AionDB <small>(ops/s · ms)</small></th>
<th>SurrealDB WS <small>(ops/s · ms)</small></th>
<th>PostgreSQL stack <small>(ops/s · ms)</small></th>
<th>AionDB / pgstack</th>
</tr>
</thead>
<tbody id="bench-summary-body"></tbody>
</table>
