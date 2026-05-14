#!/usr/bin/env python3
"""Merge improved AionDB benchmark results into the docs dashboard."""

from __future__ import annotations

import argparse
import csv
import json
import re
import subprocess
from datetime import datetime, timezone
from pathlib import Path


DATA_RE = re.compile(
    r'(<script id="bench-results-data" type="application/json">)(.*?)(</script>)',
    re.DOTALL,
)


def load_dashboard(path: Path) -> tuple[str, str, str, dict]:
    text = path.read_text(encoding="utf-8")
    match = DATA_RE.search(text)
    if not match:
        raise SystemExit(f"missing benchmark JSON script in {path}")
    payload = json.loads(match.group(2))
    return text, match.group(1), match.group(3), payload


def measured_ok_aiondb_rows(run_dir: Path) -> list[dict]:
    raw_path = run_dir / "raw_results.csv"
    if not raw_path.exists():
        raise SystemExit(f"missing {raw_path}")
    rows: list[dict] = []
    with raw_path.open(encoding="utf-8", newline="") as handle:
        for row in csv.DictReader(handle):
            if (
                row.get("phase") == "measure"
                and row.get("engine") == "aiondb"
                and row.get("status") == "OK"
            ):
                rows.append(row)
    return rows


def as_float(value: str | None) -> float | None:
    if value in (None, "", "-"):
        return None
    parsed = float(value)
    return parsed if parsed == parsed else None


def merge_best_aiondb(payload: dict, rows: list[dict], run_id: str) -> list[dict]:
    tests = {test["test"]: test for test in payload.get("tests", [])}
    improvements: list[dict] = []
    for row in rows:
        test_name = row["test"]
        test = tests.get(test_name)
        if test is None:
            continue
        current = test.setdefault("engines", {}).get("aiondb")
        if current is None:
            continue
        new_ops = as_float(row.get("ops"))
        old_ops = as_float(str(current.get("ops")))
        old_status = current.get("status")
        if new_ops is None:
            continue
        if old_status == "OK":
            if old_ops is None or new_ops <= old_ops:
                continue
            ratio = new_ops / old_ops if old_ops > 0 else None
        else:
            ratio = None
        mean_ms = as_float(row.get("mean_ms"))
        current.update(
            {
                "status": "OK",
                "ops": new_ops,
                "mean_ms": mean_ms,
                "source_run_id": run_id,
                "updated_at": datetime.now(timezone.utc)
                .replace(microsecond=0)
                .isoformat()
                .replace("+00:00", "Z"),
            }
        )
        improvements.append(
            {
                "test": test_name,
                "old_status": old_status,
                "old_ops": old_ops,
                "new_ops": new_ops,
                "ratio": ratio,
            }
        )

    if improvements:
        history = payload.setdefault("aiondb_best_update_runs", [])
        history.append(
            {
                "run_id": run_id,
                "updated_at": datetime.now(timezone.utc)
                .replace(microsecond=0)
                .isoformat()
                .replace("+00:00", "Z"),
                "improvements": improvements,
            }
        )
        del history[:-20]
        base_run = payload.get("run_id", "unknown")
        suffix = f"+best-{run_id}"
        if suffix not in base_run:
            payload["run_id"] = f"{base_run}{suffix}"
    return improvements


def write_dashboard(path: Path, text: str, open_tag: str, close_tag: str, payload: dict) -> None:
    counts: dict[str, int] = {}
    for test in payload.get("tests", []):
        for row in test.get("engines", {}).values():
            status = row.get("status", "MISSING")
            counts[status] = counts.get(status, 0) + 1
    payload["status_counts"] = counts
    data = json.dumps(payload, ensure_ascii=False, separators=(",", ":"), allow_nan=False)
    escaped = data.replace("</", "<\\/")
    updated = DATA_RE.sub(f"{open_tag}{escaped}{close_tag}", text, count=1)
    path.write_text(updated, encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("run_dir", type=Path)
    parser.add_argument(
        "--docs-page",
        type=Path,
        default=Path("docs/content/documentation/evaluate/benchmark-results.md"),
    )
    parser.add_argument("--build-site", action="store_true")
    args = parser.parse_args()

    metadata_path = args.run_dir / "metadata.json"
    metadata = json.loads(metadata_path.read_text(encoding="utf-8")) if metadata_path.exists() else {}
    run_id = metadata.get("run_id") or args.run_dir.name

    text, open_tag, close_tag, payload = load_dashboard(args.docs_page)
    improvements = merge_best_aiondb(payload, measured_ok_aiondb_rows(args.run_dir), run_id)
    if not improvements:
        print("dashboard unchanged: no improved AionDB measurements")
        return 0

    write_dashboard(args.docs_page, text, open_tag, close_tag, payload)
    for item in improvements:
        previous = item.get("old_status") or "missing"
        old_ops = item.get("old_ops")
        old_label = f"{old_ops:.3f}" if isinstance(old_ops, float) else previous
        print(
            f"dashboard improved: {item['test']} "
            f"{old_label} -> {item['new_ops']:.3f} ops/s"
        )

    if args.build_site:
        subprocess.run(["python3", "build.py"], cwd="docs", check=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
