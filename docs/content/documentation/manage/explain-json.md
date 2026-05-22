---
title: Explain JSON
order: 62
---

# Explain JSON

AionDB exposes a structured `EXPLAIN` payload for graph and hybrid queries.

> New in v0.3: vector search joins the same product story as SQL and graph, with pgvector-style SQL, HNSW, IVF-flat, Qdrant-style filters, and benchmarked recall/latency. See [What's New in v0.3](/documentation/project/whats-new-v0-3.html).

Supported forms:

```sql
EXPLAIN (FORMAT JSON)
MATCH (a)-[:KNOWS]->(b)
RETURN b.id;

EXPLAIN (ANALYZE, FORMAT JSON)
MATCH (a)-[:KNOWS]->(b)
RETURN b.id;
```

`FORMAT JSON` returns a single-row JSON document instead of the usual multi-line text plan. `ANALYZE` keeps the same JSON shape and adds runtime data such as actual rows, clause input/output rows, selectivity, and lightweight timings.

## Versioned contract

The payload is versioned:

- `schema_version = 1`
- `format_kind = "aiondb.explain_json"`

Clients should tolerate additive fields and reject only on incompatible `schema_version` or `format_kind`.

## Top-level shape

| Field | Meaning |
| --- | --- |
| `query_plan_lines` | Full text `EXPLAIN` output preserved as an array of lines. |
| `plan_lines` | Non-graph `EXPLAIN` lines. |
| `structural_plan_lines` | `plan_lines` without runtime summary lines such as `Execution:` or `Rows Returned:`. |
| `graph_lines` | Human-readable graph observability lines. |
| `plan_overview` | Stable summary of the non-graph plan root and primary operator. |
| `graph_summary` | Stable machine-readable summary of graph risk, pivots, joins, and drift. |
| `graph_detail` | Clause-level and pattern-level graph details. |
| `execution_summary` | Runtime summary when `ANALYZE` is used. |

## `plan_overview`

`plan_overview` is meant to give a small stable SQL-side summary for tooling and UI.

Fields:

- `root_line`
- `root_kind`
- `primary_operator_line`
- `primary_operator_kind`
- `plan_category`
- `plan_subcategory`
- `line_count`
- `structural_line_count`
- `graph_line_count`

Current `plan_category` values include:

- `join`
- `scan`
- `sort`
- `aggregate`
- `limit`
- `project`
- `other`

Current `plan_subcategory` values include:

- `nested_loop`
- `hash_join`
- `merge_join`
- `index_scan`
- `seq_scan`
- `sort`
- `aggregate`
- `limit`
- `project`
- `query_wrapper`
- `other`

## `graph_summary`

`graph_summary` is the compact machine-readable graph health block.

Important fields include:

- `severity`
- `pivotable_patterns`
- `fragile_pivots`
- `blocked_pivots`
- `selected_non_leftmost`
- `selected_non_leftmost_source`
- `pivot_driver_metrics_source`
- `multi_pattern_clauses`
- `correlated_clauses`
- `shared_anchor_clauses`
- `correlated_shared_anchor`
- `correlated_non_shared`
- `shared_anchor_uncorrelated`
- `independent_multi_scan`
- `drift_patterns`
- `high_drift_patterns`
- `drift_metrics_source`
- `risky_join_clauses`
- `high_risk_join_clauses`
- `join_risk_metrics_source`
- `max_fanout`

Current `severity` values are:

- `ok`
- `watch`
- `risk`

## `graph_detail`

`graph_detail` contains:

- `summary`
- `clauses[]`

Each clause can expose:

- `kind`
- `clause_index`
- `optional`
- `patterns`
- `actual_input_rows`
- `actual_output_rows`
- `actual_selectivity`
- `actual_time_ms`
- `join_risk`
- `pattern_details[]`

`join_risk` can expose:

- `severity`
- `fanout`
- `basis`
- `join_risk_source`
- `correlated`
- `correlated_source`
- `shared_anchor`
- `shared_anchor_source`
- `join_shape`
- `join_shape_source`
- `patterns`

Each pattern detail can expose:

- `estimated_rows`
- `actual_rows`
- `estimate_error_ratio`
- `estimated_selectivity`
- `actual_selectivity`
- `actual_time_ms`
- `seed`
- `seed_mode`
- `seed_mode_source`
- `seed_binding_state`
- `seed_binding_state_source`
- `correlated_vars`
- `correlated_vars_source`
 - `seed_constraints`
 - `seed_constraints_source`
- `pattern_runtime_strategy`
- `pattern_runtime_strategy_source`
- `pattern_runtime_reason`
- `pattern_runtime_reason_source`
- `pivot_driver`
- `pivot_driver_source`
- `pivot_reason`
- `pivot_reason_source`
- `pivot_decision`
- `pivot_decision_source`
- `pivot_margin`
- `pivot_competition`
- `pivot_scores`
 - `first_rel`
 - `first_rel_source`
 - `first_rel_mode`
 - `first_rel_mode_source`
 - `first_rel_constraints`
 - `first_rel_constraints_source`
 - `bound_vars`
 - `bound_vars_source`
- `shape`
- `shape_source`
- `flags`
- `flags_source`
- `warning_severity`

## Provenance fields

The graph payload carries explicit provenance for the most important runtime-facing values.

Common summary-level provenance fields:

- `query_runtime_source`
- `selected_non_leftmost_source`
- `pivot_driver_metrics_source`
- `drift_metrics_source`
- `join_risk_metrics_source`

Common clause and pattern provenance fields:

- `runtime_strategy_source`
- `join_risk.join_risk_source`
- `join_risk.correlated_source`
- `join_risk.shared_anchor_source`
- `join_risk.join_shape_source`
- `pattern_runtime_strategy_source`
- `pattern_runtime_reason_source`
- `seed_mode_source`
- `seed_binding_state_source`
- `correlated_vars_source`
- `seed_constraints_source`
- `pivot_driver_source`
- `pivot_reason_source`
- `pivot_decision_source`
- `first_rel_source`
- `first_rel_mode_source`
- `first_rel_constraints_source`
- `bound_vars_source`
- `flags_source`
- `shape_source`

Current values are:

- `observed`
- `inferred`
- `mixed`
- `unavailable`

Practical reading:

- plain `EXPLAIN (FORMAT JSON)` mostly yields `inferred` or `unavailable`
- `EXPLAIN (ANALYZE, FORMAT JSON)` can yield `observed` or `mixed`

`observed` means the engine saw the behavior at runtime. `inferred` means the value was derived from static plan shape or planner metadata. `mixed` means the final summary combines both static and runtime signals. `unavailable` means the metric is only meaningful under `ANALYZE` and no runtime evidence exists.

## `execution_summary`

`execution_summary` is present in both modes, but runtime values are only populated under `ANALYZE`.

Fields:

- `kind`
- `rows_returned`
- `memory_used_bytes`

Under plain `EXPLAIN (FORMAT JSON)`, these runtime fields can be `null`.

## Consuming the payload

From a SQL client such as `psql`, `FORMAT JSON` returns a single text cell that contains the JSON document. That path is useful for ad hoc inspection, shell tooling, and compatibility with existing SQL clients.

Inside the engine, prefer the structured helpers instead of reparsing text output:

- `QueryEngine::execute_explain_graph_summary_json(session, sql, analyze)`
- `QueryEngine::execute_explain_graph_detail_json(session, sql, analyze)`

Those helpers:

- prepend `EXPLAIN` or `EXPLAIN ANALYZE`;
- execute the statement;
- extract the structured graph payload;
- return `serde_json::Value`.

Minimal Rust sketch:

```rust
use aiondb_engine::engine::api::QueryEngine;

fn load_graph_summary(
    engine: &dyn QueryEngine,
    session: &aiondb_engine::session::SessionHandle,
) -> aiondb_core::error::DbResult<serde_json::Value> {
    engine.execute_explain_graph_summary_json(
        session,
        "MATCH (a)-[:KNOWS]->(b) RETURN b.id",
        true,
    )
}
```

For UI or telemetry work:

- use `graph_summary` for badges, coarse severity, and top-level warnings;
- use `graph_detail` for clause and pattern drill-down;
- use `plan_overview` for quick SQL plan labeling;
- keep `query_plan_lines` only for raw rendering or debugging.

Prefer the provenance companions when deciding how strongly to present a signal:

- treat `observed` as runtime evidence;
- treat `inferred` as planner guidance;
- treat `mixed` as a combined summary, not a pure runtime fact;
- treat `unavailable` as absence of runtime evidence rather than a negative result.

## Text `EXPLAIN` provenance

The plain multi-line text `EXPLAIN` surface now also exposes provenance on the most important human-readable lines.

Typical examples:

- `Graph Summary Severity: ... source=inferred|observed|mixed`
- `Graph Planner Warning: ... source=inferred|observed`
- `Graph Pivot Hint: ... source=inferred|observed`
- `Graph Pivot Note: ... source=inferred|observed`
- `Graph Join Hint: ... source=inferred`
- `Graph Access Summary: ... source=inferred`
- `Graph Access Warning: ... source=inferred`
- `Graph Procedure Summary: ... source=inferred`
- `Graph Drift Summary: ... source=observed`
- `Graph Join Fanout Summary: ... source=observed`

Use the text form for ad hoc debugging and operator review. Use the JSON form for product logic, telemetry ingestion, or UI state.

## Example

Abbreviated payload:

```json
{
  "schema_version": 1,
  "format_kind": "aiondb.explain_json",
  "plan_overview": {
    "root_kind": "Cypher Query",
    "primary_operator_kind": "Nested Loop",
    "plan_category": "join",
    "plan_subcategory": "nested_loop"
  },
  "graph_summary": {
    "severity": "watch",
    "fragile_pivots": 1,
    "pivot_driver_metrics_source": "inferred",
    "drift_metrics_source": "unavailable",
    "risky_join_clauses": 0,
    "join_risk_metrics_source": "unavailable",
    "max_fanout": null
  },
  "graph_detail": {
    "summary": {
      "severity": "watch"
    },
    "clauses": [
      {
        "kind": "PipelineMatch",
        "pattern_details": [
          {
            "pattern_runtime_strategy": "left_to_right_node_seed",
            "pattern_runtime_strategy_source": "observed",
            "seed_mode": "label_scan",
            "pivot_decision": "retained_leftmost",
            "pivot_decision_source": "inferred"
          }
        ]
      }
    ]
  },
  "execution_summary": {
    "kind": "Query",
    "rows_returned": 1,
    "memory_used_bytes": 5283
  }
}
```

## Reading common graph cases

Two patterns matter in practice:

- correlated fanout around an already bound anchor;
- independent multi-pattern scans that behave like a product.

### Correlated shared-anchor fanout

Example query:

```sql
EXPLAIN (ANALYZE, FORMAT JSON)
MATCH (a)-[:KNOWS]->(b), (a)-[:KNOWS]->(c)
RETURN a.id, b.id, c.id;
```

Typical signals to expect:

- `graph_summary.correlated_clauses > 0`
- `graph_summary.shared_anchor_clauses > 0`
- `graph_summary.correlated_shared_anchor > 0`
- `graph_detail.clauses[*].join_risk.join_shape = "correlated_shared_anchor"`
- `graph_detail.clauses[*].join_risk.fanout` noticeably above `1.0` when the anchor has broad adjacency

How to read it:

- the clause is not an accidental cartesian product;
- it is reusing an existing anchor and expanding multiple branches from it;
- the main risk is adjacency fanout, not a missing join predicate.

When this shape becomes expensive, look first at:

- early filters on the anchor node;
- branch selectivity;
- whether the broadest branch can be narrowed earlier.

### Independent multi-scan

Example query:

```sql
EXPLAIN (ANALYZE, FORMAT JSON)
MATCH (a:Person), (b:Company)
RETURN a.id, b.id;
```

Typical signals to expect:

- `graph_summary.multi_pattern_clauses > 0`
- `graph_summary.independent_multi_scan > 0`
- `graph_summary.correlated_clauses = 0`
- `graph_detail.clauses[*].join_risk.join_shape = "independent_multi_scan"`
- `graph_detail.clauses[*].join_risk.shared_anchor = false`

How to read it:

- the clause does not share bindings between patterns;
- the planner is dealing with independent branches;
- high fanout here often means the query shape itself is broad, not just that one branch has bad local selectivity.

When this shape is surprising, verify first that:

- a real join predicate is not missing;
- the query was intended to enumerate a product;
- labels and property predicates are selective enough before the join point.

### Pattern-level seed and pivot signals

Within `graph_detail.clauses[*].pattern_details[*]`, the fields below are the quickest way to understand why one branch is expensive:

- `seed_mode`
- `seed_binding_state`
- `pivot_reason`
- `pivot_decision`
- `pivot_margin`
- `pivot_competition`
- `warning_severity`

Practical reading:

- `seed_binding_state = "prebound"` usually means an expand-from-bound-node shape;
- `seed_mode = "label_scan"` under drift or fanout is usually the first thing to challenge;
- `pivot_margin = 0` means the chosen seed was not clearly better than its runner-up;
- `warning_severity = "high"` means that pattern deserves direct inspection before tuning the rest of the plan.

## `watch` versus `risk`

Use the top-level `graph_summary.severity` as the first triage signal:

- `ok`: no elevated graph planning signal is currently visible;
- `watch`: the plan has a shape worth monitoring, but not yet a severe runtime symptom;
- `risk`: the query already shows a strong sign of bad fanout or estimate instability.

Typical `watch` situations:

- `fragile_pivots > 0` without severe runtime fanout;
- `selected_non_leftmost > 0` because the planner had to reorder locally;
- `independent_multi_scan > 0` with only moderate clause fanout.

Typical `risk` situations:

- `high_risk_join_clauses > 0`
- `high_drift_patterns > 0`
- a fragile pivot combined with another strong warning signal

### Example `risk` case

Example query shape:

```sql
EXPLAIN (ANALYZE, FORMAT JSON)
MATCH (a:Person), (b:Company)
RETURN a.id, b.id;
```

If both branches are broad enough, the JSON can move from `watch` to `risk`:

```json
{
  "graph_summary": {
    "severity": "risk",
    "independent_multi_scan": 1,
    "risky_join_clauses": 1,
    "high_risk_join_clauses": 1,
    "max_fanout": 9.0
  }
}
```

How to read it:

- the danger is no longer hypothetical;
- the clause already multiplies rows aggressively at runtime;
- query-shape changes are usually more urgent than micro-tuning one branch.

In practice, treat `risk` as a prompt to inspect:

- whether the clause is intentionally enumerating a product;
- whether a missing predicate should connect the branches;
- whether label or property filters can be pushed before the join point;
- whether the broad clause should be split or reshaped.

## UI and tooling guidance

The JSON payload is meant to support both:

- a compact summary view;
- a deeper clause and pattern inspection view.

### Stable fields for summary cards

For a top-level UI summary, prefer:

- `graph_summary.severity`
- `graph_summary.fragile_pivots`
- `graph_summary.risky_join_clauses`
- `graph_summary.high_risk_join_clauses`
- `graph_summary.high_drift_patterns`
- `graph_summary.max_fanout`
- `plan_overview.plan_category`
- `plan_overview.plan_subcategory`
- `execution_summary.rows_returned`

Those fields are the best compact signals for:

- whether the graph part looks healthy;
- whether the plan shape is broad or unstable;
- whether the runtime result is already showing severe fanout.

### Fields for drill-down views

For an expandable detail panel, prefer:

- `graph_detail.clauses[*].join_risk`
- `graph_detail.clauses[*].actual_input_rows`
- `graph_detail.clauses[*].actual_output_rows`
- `graph_detail.clauses[*].actual_selectivity`
- `graph_detail.clauses[*].actual_time_ms`
- `graph_detail.clauses[*].pattern_details[*].seed_mode`
- `graph_detail.clauses[*].pattern_details[*].seed_binding_state`
- `graph_detail.clauses[*].pattern_details[*].pivot_reason`
- `graph_detail.clauses[*].pattern_details[*].pivot_decision`
- `graph_detail.clauses[*].pattern_details[*].pivot_margin`
- `graph_detail.clauses[*].pattern_details[*].estimate_error_ratio`
- `graph_detail.clauses[*].pattern_details[*].warning_severity`

Those are the fields that usually explain why the summary is red or yellow.

### Text lines versus structured fields

Treat the JSON objects as the stable contract.

Do not build product logic on:

- `graph_lines`
- `query_plan_lines`
- free-form text extracted from `Graph Query Summary`, `Graph Join Hint`, `Graph Plan Hint`, or similar lines

Those lines are still useful for:

- raw rendering;
- debugging;
- copy/paste into bug reports;
- quick local inspection in SQL clients.

But the structured contract should be preferred for:

- UI badges
- alerting
- planner feedback loops
- telemetry enrichment

### External compatibility posture

This JSON contract is intended for AionDB-native tooling. It is versioned, but it is not a cross-database interoperability format.

That means:

- additive fields are expected over time;
- category and severity values should be treated as AionDB-specific;
- consumers should be tolerant to unknown keys;
- consumers should not assume another database will emit the same shape or semantics.

## Suggested UI mappings

The payload does not prescribe UI colors or wording, but using a consistent mapping across tools makes the output easier to compare.

### Severity

Suggested mapping:

| Field value | Suggested label | Suggested tone |
| --- | --- | --- |
| `ok` | Healthy | neutral/green |
| `watch` | Watch | caution/yellow |
| `risk` | Risk | strong warning/red |

Use `graph_summary.severity` for the top-level badge. If clause-level or pattern-level warnings are shown, keep them subordinate to the top-level severity instead of inventing a second competing global status.

### Join shape

Suggested mapping for `graph_detail.clauses[*].join_risk.join_shape`:

| Field value | Suggested label | Practical meaning |
| --- | --- | --- |
| `correlated_shared_anchor` | Correlated star | Multiple branches expanding from the same bound anchor |
| `correlated_non_shared` | Correlated multi-branch | Correlated clause without a single shared star anchor |
| `shared_anchor_uncorrelated` | Uncorrelated star | Shared local anchor, but no incoming correlation from an earlier binding |
| `independent_multi_scan` | Independent multi-scan | Clause behaves like a product across independent branches |

### Seed mode

Suggested mapping for `graph_detail.clauses[*].pattern_details[*].seed_mode`:

| Field value | Suggested label | Practical meaning |
| --- | --- | --- |
| `id_constrained` | ID constrained | Pattern starts from a highly selective id-based seed |
| `indexed` | Indexed seed | Pattern starts from an indexed property path |
| `range_constrained` | Range constrained | Pattern starts from a range-filtered seed |
| `label_scan` | Label scan | Pattern starts from a label-wide scan |
| `anonymous_scan` | Anonymous scan | Pattern starts from an unconstrained anonymous scan |

When a UI needs only one compact warning signal at pattern level, `seed_mode = "label_scan"` plus `warning_severity = "high"` is the most important combination to highlight first.

### Seed binding state

Suggested mapping for `seed_binding_state`:

| Field value | Suggested label | Practical meaning |
| --- | --- | --- |
| `prebound` | Prebound | Expand from an already bound variable |
| `fresh` | Fresh seed | New seed introduced at this pattern |
| `anonymous` | Anonymous seed | Seed is not carried as a named variable |
| `unknown` | Unknown | Engine could not classify binding state precisely |

### Suggested display order

For a compact pattern card, this order works well:

1. `shape`
2. `warning_severity`
3. `seed_mode`
4. `seed_binding_state`
5. `pivot_decision`
6. `actual_rows`
7. `estimate_error_ratio`
8. `actual_time_ms`

That order keeps the structural explanation ahead of the raw numbers.

## Consumer checklist

If you are building a UI, telemetry adapter, or planner feedback client on top of this payload, keep the client logic conservative.

### Validate first

On receipt:

1. check that the payload is valid JSON;
2. check `format_kind == "aiondb.explain_json"`;
3. check `schema_version`;
4. tolerate unknown top-level and nested fields.

Reject the payload only when:

- `format_kind` is not recognized;
- `schema_version` is newer than the consumer can safely handle;
- required fields for the specific feature are missing or of the wrong type.

### Prefer structured fields over text

For product logic:

- use `graph_summary`, `graph_detail`, `plan_overview`, and `execution_summary`;
- do not parse `graph_lines` or `query_plan_lines` to recover structured state if the JSON field already exists.

Use text lines only for:

- raw display;
- debugging;
- issue reports;
- fallback visibility in generic SQL clients.

### Log enough context

When storing or forwarding the payload, log at least:

- `schema_version`
- `format_kind`
- `graph_summary.severity`
- `plan_overview.plan_category`
- `plan_overview.plan_subcategory`
- whether the source was `EXPLAIN` or `EXPLAIN ANALYZE`

That is usually enough to keep old snapshots interpretable after the format evolves.

### Handle unknown enum-like values safely

Fields such as:

- `severity`
- `join_shape`
- `seed_mode`
- `seed_binding_state`
- `plan_category`
- `plan_subcategory`

should be treated as open sets, not closed enums.

If a value is unknown:

- preserve it in logs or raw displays;
- map it to a generic fallback label in the UI;
- avoid failing the whole consumer unless that field is mandatory for a narrow feature.

### Degrade by feature, not globally

If a client cannot interpret:

- one clause field;
- one pattern field;
- one join classification;

it should usually keep the rest of the payload usable.

Good fallback examples:

- hide one badge, but keep the rest of the plan visible;
- show `Unknown` for one classifier, but keep row and timing data;
- skip one drill-down panel, but keep summary cards.

Bad fallback example:

- rejecting the entire payload because one new nested field appeared.

### Keep runtime expectations explicit

Do not assume runtime fields are always available.

Under plain `EXPLAIN (FORMAT JSON)`:

- `execution_summary.kind` can be `null`;
- clause and pattern `actual_*` fields can be missing or `null`;
- drift and fanout signals can be weaker than under `ANALYZE`.

If a feature requires runtime truth, gate it explicitly on:

- `EXPLAIN ANALYZE`;
- or the presence of the specific `actual_*` fields it needs.

## Schema evolution policy

`schema_version = 1` is the current contract version.

The intended compatibility rule is:

- additive changes keep the same `schema_version`;
- incompatible semantic or structural changes require a new `schema_version`.

### Changes that should remain compatible within version 1

Examples:

- adding a new top-level field;
- adding a new nested field under `graph_summary`, `graph_detail`, `plan_overview`, or `execution_summary`;
- adding a new classifier value such as a new `join_shape`, `seed_mode`, or `plan_subcategory`;
- adding more detail to existing arrays such as `clauses[]` or `pattern_details[]`;
- populating an existing optional field in more cases than before.

Clients are expected to tolerate those changes without failing.

### Changes that should require a new schema version

Examples:

- renaming an existing field;
- changing the meaning of an existing field incompatibly;
- changing a field type in a way that breaks existing consumers;
- removing a field that version 1 documented as part of the contract;
- replacing one object shape with a materially different one.

If such a change is necessary, the producer should:

- increment `schema_version`;
- document the new version explicitly;
- keep older consumers able to reject the payload cleanly.

### Recommended producer discipline

When extending the payload:

1. prefer adding fields over rewriting existing ones;
2. keep enum-like fields open for future values;
3. keep text lines secondary to structured fields;
4. update examples and tests when the structured contract changes.

### Recommended consumer discipline

When reading the payload:

1. branch first on `format_kind`;
2. then branch on `schema_version`;
3. treat unknown fields as ignorable by default;
4. treat unknown enum-like values as display fallbacks, not fatal errors.

This keeps version 1 usable even as the graph observability surface grows.
