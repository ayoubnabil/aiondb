#!/usr/bin/env python3
"""Inject the embedded benchmark snapshot into the documentation page."""

from __future__ import annotations

import argparse
import html
import json
from pathlib import Path


START = '<span hidden data-embedded-bench-start="true"></span>'
END = '<span hidden data-embedded-bench-end="true"></span>'
LEGACY_START = "<!-- embedded-bench-start -->"
LEGACY_END = "<!-- embedded-bench-end -->"


def fmt_float(value: float) -> float:
    return round(float(value), 6)


def load_payload(trace_path: Path) -> dict:
    report = json.loads(trace_path.read_text())
    metadata = report.get("metadata", {})

    by_test: dict[str, dict] = {}
    for row in report.get("results", []):
        test = row["scenario"]
        summary = row["summary"]
        by_test.setdefault(
            test,
            {
                "test": test,
                "category": row.get("category", "other"),
                "engines": {},
            },
        )
        by_test[test]["engines"][row["engine"]] = {
            "status": summary.get("status", "UNKNOWN"),
            "ops": fmt_float(summary.get("ops_per_sec", 0.0)),
            "mean_ms": fmt_float(summary.get("avg_ms", 0.0)),
            "p95_ms": fmt_float(summary.get("p95_ms", 0.0)),
        }

    tests = list(by_test.values())
    tests.sort(key=lambda row: (row["category"], row["test"]))

    return {
        "run_id": metadata.get("run_id", trace_path.stem),
        "rows": metadata.get("rows"),
        "warmup_seconds": metadata.get("warmup_seconds_per_engine_per_case"),
        "measure_seconds": metadata.get("measure_seconds_per_engine_per_case"),
        "trace": str(trace_path),
        "baseline_engine": "surrealdb_embedded_mem",
        "engines": [
            {"id": "aiondb_embedded", "label": "AionDB embedded"},
            {"id": "surrealdb_embedded_mem", "label": "SurrealDB embedded mem"},
        ],
        "tests": tests,
    }


def geometric_mean_ratio(payload: dict) -> float | None:
    import math

    logs: list[float] = []
    for test in payload["tests"]:
        aion = test["engines"].get("aiondb_embedded")
        surreal = test["engines"].get("surrealdb_embedded_mem")
        if (
            aion
            and surreal
            and aion.get("status") == "OK"
            and surreal.get("status") == "OK"
            and surreal.get("ops", 0) > 0
        ):
            logs.append(math.log(aion["ops"] / surreal["ops"]))
    if not logs:
        return None
    return math.exp(sum(logs) / len(logs))


def render_section(payload: dict) -> str:
    data = json.dumps(payload, separators=(",", ":"), ensure_ascii=False).replace("</", "<\\/")
    wins = 0
    comparable = 0
    for test in payload["tests"]:
        aion = test["engines"].get("aiondb_embedded")
        surreal = test["engines"].get("surrealdb_embedded_mem")
        if (
            aion
            and surreal
            and aion.get("status") == "OK"
            and surreal.get("status") == "OK"
            and surreal.get("ops", 0) > 0
        ):
            comparable += 1
            wins += int(aion["ops"] >= surreal["ops"])

    gm = geometric_mean_ratio(payload)
    gm_label = f"{gm:.2f}x" if gm is not None else "-"
    trace = html.escape(payload["trace"])

    return f"""{START}

## Embedded Results

This section compares the local in-process engines only: AionDB embedded release
build against SurrealDB embedded memory mode. The trace is kept at
`{trace}`.

<div class="bench-kpi-grid">
<div class="bench-kpi-card"><strong>{html.escape(str(payload["run_id"]))}</strong><span>Run</span></div>
<div class="bench-kpi-card"><strong>{payload["rows"]}</strong><span>Rows</span></div>
<div class="bench-kpi-card"><strong>{wins}/{comparable}</strong><span>AionDB wins</span></div>
<div class="bench-kpi-card"><strong>{gm_label}</strong><span>Geo mean vs SurrealDB</span></div>
</div>

<div class="bench-visual">
<div class="bench-toolbar">
<label>Category <select id="embedded-bench-category"></select></label>
<label>Metric <select id="embedded-bench-metric"><option value="ops">ops/s</option><option value="ratio">vs SurrealDB</option></select></label>
<label>Scale <select id="embedded-bench-scale"><option value="linear">linear</option><option value="log">log</option></select></label>
<label>Sort <select id="embedded-bench-sort"><option value="test">test</option><option value="aiondb_embedded">AionDB</option><option value="surrealdb_embedded_mem">SurrealDB</option></select></label>
</div>
<div class="bench-canvas-wrap">
<div id="embedded-bench-chart" class="bench-chart-host"></div>
</div>
</div>
<script id="embedded-bench-results-data" type="application/json">{data}</script>

## Embedded Raw Summary

Values are measured throughput in **operations per second** (`ops/s`) with mean
and p95 latency (`ms`) shown next to each cell. The last column is the ratio of
AionDB embedded ops/s over SurrealDB embedded ops/s.

<table class="bench-summary-table">
<thead>
<tr>
<th>Test</th>
<th>Category</th>
<th>AionDB embedded <small>(ops/s · avg · p95)</small></th>
<th>SurrealDB embedded <small>(ops/s · avg · p95)</small></th>
<th>AionDB / SurrealDB</th>
</tr>
</thead>
<tbody id="embedded-bench-summary-body"></tbody>
</table>

{END}
"""


def inject(page: str, section: str) -> str:
    if START in page and END in page:
        before = page[: page.index(START)]
        after = page[page.index(END) + len(END) :]
        return before.rstrip() + "\n\n" + section.rstrip() + "\n" + after
    if LEGACY_START in page and LEGACY_END in page:
        before = page[: page.index(LEGACY_START)]
        after = page[page.index(LEGACY_END) + len(LEGACY_END) :]
        return before.rstrip() + "\n\n" + section.rstrip() + "\n" + after
    return page.rstrip() + "\n\n" + section.rstrip() + "\n"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("trace", type=Path)
    parser.add_argument(
        "--docs-page",
        type=Path,
        default=Path("docs/content/documentation/evaluate/benchmark-results.md"),
    )
    args = parser.parse_args()

    payload = load_payload(args.trace)
    section = render_section(payload)
    page = args.docs_page.read_text()
    args.docs_page.write_text(inject(page, section))
    print(f"updated {args.docs_page} from {args.trace}")


if __name__ == "__main__":
    main()
