#!/usr/bin/env python3
"""Render a docs benchmark results page from a surreal-suite run directory."""

from __future__ import annotations

import argparse
import csv
import json
import math
from collections import defaultdict
from pathlib import Path


ENGINE_LABELS = {
    "aiondb": "AionDB",
    "surrealdb": "SurrealDB WS",
    "pgstack": "PostgreSQL stack",
}

ENGINE_ORDER = ["aiondb", "surrealdb", "pgstack"]


def fmt_num(value: float) -> str:
    if math.isnan(value):
        return "-"
    if value >= 1000:
        return f"{value:,.0f}"
    if value >= 100:
        return f"{value:.0f}"
    if value >= 10:
        return f"{value:.1f}"
    return f"{value:.2f}"


def fmt_ratio(value: float) -> str:
    if math.isnan(value):
        return "-"
    return f"{value:.2f}x"


def geometric_mean(values: list[float]) -> float:
    positive = [v for v in values if v > 0 and not math.isnan(v)]
    if not positive:
        return math.nan
    return math.exp(sum(math.log(v) for v in positive) / len(positive))


def read_rows(run_dir: Path) -> tuple[dict, list[dict]]:
    metadata_path = run_dir / "metadata.json"
    metadata = json.loads(metadata_path.read_text(encoding="utf-8")) if metadata_path.exists() else {}
    raw_path = run_dir / "raw_results.csv"
    rows = list(csv.DictReader(raw_path.open(encoding="utf-8"))) if raw_path.exists() else []
    return metadata, rows


def measured_rows(rows: list[dict]) -> list[dict]:
    return [row for row in rows if row.get("phase") == "measure"]


def group_latest(rows: list[dict]) -> dict[str, dict[str, dict]]:
    grouped: dict[str, dict[str, dict]] = defaultdict(dict)
    for row in rows:
        grouped[row["test"]][row["engine"]] = row
    return grouped


def ratio_sets(grouped: dict[str, dict[str, dict]]) -> tuple[dict[str, list[float]], dict[str, dict[str, list[float]]]]:
    ratios: dict[str, list[float]] = {"aiondb": [], "surrealdb": []}
    by_category: dict[str, dict[str, list[float]]] = defaultdict(lambda: {"aiondb": [], "surrealdb": []})
    for engines in grouped.values():
        if not all(engine in engines for engine in ("aiondb", "surrealdb", "pgstack")):
            continue
        if not all(engines[engine]["status"] == "OK" for engine in ("aiondb", "surrealdb", "pgstack")):
            continue
        pg_ops = float(engines["pgstack"]["ops"])
        if pg_ops <= 0:
            continue
        category = engines["pgstack"]["category"]
        for engine in ("aiondb", "surrealdb"):
            ratio = float(engines[engine]["ops"]) / pg_ops
            ratios[engine].append(ratio)
            by_category[category][engine].append(ratio)
    return ratios, by_category


def status_counts(rows: list[dict]) -> dict[str, int]:
    counts: dict[str, int] = defaultdict(int)
    for row in rows:
        counts[row["status"]] += 1
    return dict(counts)


def optional_float(value: str) -> float | None:
    if value in ("", "-"):
        return None
    parsed = float(value)
    return parsed if math.isfinite(parsed) else None


def chart_data(metadata: dict, rows: list[dict], grouped: dict[str, dict[str, dict]]) -> dict:
    total_tests = len(metadata.get("tests", []))
    engines = metadata.get("engines", ENGINE_ORDER)
    iterations = int(metadata.get("iterations", 1))
    expected = total_tests * len(engines) * iterations if total_tests else len(rows)
    done = len(rows)
    counts = status_counts(rows)
    tests = []
    for test in sorted(grouped):
        engines = grouped[test]
        if not engines:
            continue
        category = next(iter(engines.values()))["category"]
        engine_data = {}
        for engine in ENGINE_ORDER:
            if engine not in engines:
                continue
            row = engines[engine]
            engine_data[engine] = {
                "status": row["status"],
                "ops": float(row["ops"]),
                "mean_ms": optional_float(row["mean_ms"]),
            }
        tests.append({"test": test, "category": category, "engines": engine_data})
    return {
        "run_id": metadata.get("run_id", "unknown"),
        "measured": done,
        "expected": expected,
        "status_counts": counts,
        "engines": [{"id": engine, "label": ENGINE_LABELS[engine]} for engine in ENGINE_ORDER],
        "tests": tests,
    }


def interactive_chart(payload: dict) -> str:
    data = json.dumps(payload, ensure_ascii=False, separators=(",", ":"), allow_nan=False)
    escaped_data = data.replace("</", "<\\/")
    return "\n".join(
        [
            '<div class="bench-visual">',
            '<div class="bench-toolbar">',
            '<label>Category <select id="bench-category"></select></label>',
            '<label>Metric <select id="bench-metric"><option value="ops">ops/s</option><option value="ratio">vs pgstack</option></select></label>',
            '<label>Scale <select id="bench-scale"><option value="linear">linear</option><option value="log">log</option></select></label>',
            '<label>Sort <select id="bench-sort"><option value="test">test</option><option value="aiondb">AionDB</option><option value="surrealdb">SurrealDB</option><option value="pgstack">pgstack</option></select></label>',
            "</div>",
            '<div class="bench-legend" id="bench-legend"></div>',
            '<div class="bench-canvas-wrap">',
            '<canvas id="bench-chart" width="1180" height="520"></canvas>',
            '<div class="bench-tooltip" id="bench-tooltip" hidden></div>',
            "</div>",
            "</div>",
            f'<script id="bench-results-data" type="application/json">{escaped_data}</script>',
            '<script src="/benchmark-results.js" defer></script>',
        ]
    )


def table_summary() -> str:
    return "\n".join(
        [
            "## Raw Summary",
            "",
            "Values are measured throughput in **operations per second** (`ops/s`)",
            "with mean per-operation latency (`ms`) shown next to each cell. The",
            "last column is the ratio of AionDB ops/s over the PostgreSQL stack",
            "ops/s (`>1×` means AionDB is faster than the PostgreSQL stack on that",
            "test).",
            "",
            '<div class="bench-rank-legend" aria-label="AionDB ranking legend">',
            '<span><i class="bench-rank-win"></i>AionDB 1<sup>st</sup> (fastest of the three)</span>',
            '<span><i class="bench-rank-mid"></i>AionDB 2<sup>nd</sup> (beats SurrealDB only)</span>',
            '<span><i class="bench-rank-loss"></i>AionDB last (loses to SurrealDB)</span>',
            "</div>",
            "",
            '<table class="bench-summary-table">',
            "<thead>",
            "<tr>",
            "<th>Test</th>",
            "<th>Category</th>",
            "<th>AionDB <small>(ops/s · ms)</small></th>",
            "<th>SurrealDB WS <small>(ops/s · ms)</small></th>",
            "<th>PostgreSQL stack <small>(ops/s · ms)</small></th>",
            "<th>AionDB / pgstack</th>",
            "</tr>",
            "</thead>",
            '<tbody id="bench-summary-body"></tbody>',
            "</table>",
        ]
    )


def render(run_dir: Path) -> str:
    metadata, rows = read_rows(run_dir)
    measured = measured_rows(rows)
    grouped = group_latest(measured)
    payload = chart_data(metadata, measured, grouped)
    return "\n\n".join(
        [
            "---\ntitle: Benchmark Results\norder: 71\n---",
            "# Benchmark Results",
            interactive_chart(payload),
            table_summary(),
            "",
        ]
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("run_dir", type=Path)
    parser.add_argument(
        "--out",
        type=Path,
        default=Path("docs/content/documentation/evaluate/benchmark-results.md"),
    )
    args = parser.parse_args()
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(render(args.run_dir), encoding="utf-8")
    print(f"wrote {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
