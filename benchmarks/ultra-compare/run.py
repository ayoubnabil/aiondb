#!/usr/bin/env python3
"""Composite long-run benchmark orchestrator for AionDB / Neo4j / SurrealDB.

This harness does not pretend that every engine is directly comparable on every
workload. Instead it orchestrates three benchmark families and emits one
consolidated report with explicit comparability boundaries:

1. `neo4j-graph`: AionDB vs Neo4j on Cypher graph traversal shapes.
2. `surreal-graph`: AionDB vs SurrealDB on protocol-level graph traversal shapes.
3. `surreal-suite`: AionDB vs SurrealDB vs PostgreSQL-stack on the existing
   article-style CRUD / scan / graph / vector matrix.

Outputs are written under `target/benchmarks/ultra-compare/<run-id>/`.
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import re
import shlex
import socket
import subprocess
import sys
import time
from pathlib import Path
from typing import Any


REPO_ROOT = Path(__file__).resolve().parents[2]
DEFAULT_OUT = REPO_ROOT / "target" / "benchmarks" / "ultra-compare"
COMPONENTS = ("neo4j-graph", "surreal-graph", "surreal-suite")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--run-id", default=time.strftime("%Y%m%d-%H%M%SZ", time.gmtime()))
    parser.add_argument("--out-dir", type=Path, default=DEFAULT_OUT)
    parser.add_argument(
        "--components",
        nargs="+",
        default=list(COMPONENTS),
        choices=COMPONENTS,
        help="Benchmark families to execute.",
    )
    parser.add_argument("--dry-run", action="store_true")

    parser.add_argument("--neo4j-rows", type=int, default=20_000)
    parser.add_argument("--neo4j-degree", type=int, default=6)
    parser.add_argument("--neo4j-warmup", type=int, default=5)
    parser.add_argument("--neo4j-iterations", type=int, default=35)
    parser.add_argument("--neo4j-image", default="neo4j:5-community")

    parser.add_argument("--surreal-graph-nodes", type=int, default=10_000)
    parser.add_argument("--surreal-graph-fanout", type=int, default=6)

    parser.add_argument("--suite-engines", default="surrealdb aiondb pgstack")
    parser.add_argument("--suite-rows", type=int, default=10_000)
    parser.add_argument("--suite-warmup-seconds", type=int, default=5)
    parser.add_argument("--suite-iterations", type=int, default=2)
    parser.add_argument("--suite-duration-seconds", type=int, default=45)
    parser.add_argument("--suite-operation-timeout-seconds", type=int, default=45)
    parser.add_argument("--suite-tests", default="all")
    return parser.parse_args()


def run_cmd(
    cmd: list[str],
    *,
    env: dict[str, str] | None = None,
    cwd: Path = REPO_ROOT,
    stdout_path: Path | None = None,
    stderr_path: Path | None = None,
) -> subprocess.CompletedProcess[str]:
    kwargs: dict[str, Any] = {
        "cwd": cwd,
        "env": env,
        "text": True,
        "check": True,
    }
    if stdout_path is None:
        kwargs["stdout"] = subprocess.PIPE
    else:
        stdout_path.parent.mkdir(parents=True, exist_ok=True)
        kwargs["stdout"] = stdout_path.open("w", encoding="utf-8")
    if stderr_path is None:
        kwargs["stderr"] = subprocess.PIPE
    else:
        stderr_path.parent.mkdir(parents=True, exist_ok=True)
        kwargs["stderr"] = stderr_path.open("w", encoding="utf-8")
    try:
        return subprocess.run(cmd, **kwargs)
    finally:
        for key in ("stdout", "stderr"):
            handle = kwargs.get(key)
            if handle not in {None, subprocess.PIPE} and hasattr(handle, "close"):
                handle.close()


def repo_commit() -> str:
    try:
        return run_cmd(["git", "rev-parse", "HEAD"]).stdout.strip()
    except Exception:
        return "unknown"


def free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def shell_join(values: list[str]) -> str:
    return " ".join(shlex.quote(v) for v in values)


def read_json(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def find_run_dir_from_output(text: str, prefix: str) -> Path | None:
    pattern = re.compile(rf"^{re.escape(prefix)}(.+)$", re.MULTILINE)
    match = pattern.search(text)
    if not match:
        return None
    return Path(match.group(1).strip())


def find_result_file_from_text(text: str) -> Path | None:
    match = re.search(r'"result_file"\s*:\s*"([^"]+)"', text)
    if not match:
        return None
    return Path(match.group(1))


def summarize_neo4j_graph(report: dict[str, Any]) -> dict[str, Any]:
    results = report.get("results", [])
    parity_failures = sum(1 for row in results if not row.get("result_parity", False))
    ratios = [row.get("ratio_aiondb_vs_neo4j_p50") for row in results if isinstance(row.get("ratio_aiondb_vs_neo4j_p50"), (int, float))]
    return {
        "queries": len(results),
        "parity_failures": parity_failures,
        "median_ratio_aiondb_vs_neo4j_p50": sorted(ratios)[len(ratios) // 2] if ratios else None,
    }


def summarize_surreal_graph(report: dict[str, Any]) -> dict[str, Any]:
    results = report.get("results", [])
    workloads = [row.get("name") for row in results]
    return {
        "workloads": len(results),
        "skipped": len(report.get("skipped", {})),
        "workload_names": workloads,
    }


def summarize_surreal_suite(run_dir: Path) -> dict[str, Any]:
    metadata = read_json(run_dir / "metadata.json")
    summary_path = run_dir / "summary.tsv"
    line_count = 0
    statuses: dict[str, int] = {}
    if summary_path.exists():
        lines = [line for line in summary_path.read_text(encoding="utf-8").splitlines() if line.strip()]
        line_count = max(0, len(lines) - 1)
        if lines:
            headers = lines[0].split("\t")
            status_idx = headers.index("status") if "status" in headers else None
            if status_idx is not None:
                for row in lines[1:]:
                    cols = row.split("\t")
                    status = cols[status_idx] if status_idx < len(cols) else "UNKNOWN"
                    statuses[status] = statuses.get(status, 0) + 1
    return {
        "engines": metadata.get("engines", []),
        "tests": len(metadata.get("tests", [])),
        "summary_rows": line_count,
        "statuses": statuses,
    }


def component_report(
    *,
    name: str,
    comparability: str,
    command: list[str],
    env_overrides: dict[str, str],
    artifact_dir: Path | None,
    summary: dict[str, Any] | None,
    status: str = "planned",
    notes: list[str] | None = None,
) -> dict[str, Any]:
    payload: dict[str, Any] = {
        "name": name,
        "status": status,
        "comparability": comparability,
        "command_argv": command,
        "command": shell_join(command),
        "env_overrides": env_overrides,
    }
    if artifact_dir is not None:
        payload["artifact_dir"] = str(artifact_dir)
    if summary is not None:
        payload["summary"] = summary
    if notes:
        payload["notes"] = notes
    return payload


def plan_components(args: argparse.Namespace, run_dir: Path) -> list[dict[str, Any]]:
    plan: list[dict[str, Any]] = []
    if "neo4j-graph" in args.components:
        if args.dry_run:
            ports = {
                "neo4j_http_port": 17474,
                "neo4j_bolt_port": 17687,
                "aiondb_port": 15442,
            }
        else:
            ports = {
                "neo4j_http_port": free_port(),
                "neo4j_bolt_port": free_port(),
                "aiondb_port": free_port(),
            }
        artifact_dir = run_dir / "neo4j-graph"
        command = [
            sys.executable,
            str(REPO_ROOT / "benchmarks" / "neo4j-graph-compare" / "run.py"),
            "--rows",
            str(args.neo4j_rows),
            "--degree",
            str(args.neo4j_degree),
            "--warmup",
            str(args.neo4j_warmup),
            "--iterations",
            str(args.neo4j_iterations),
            "--neo4j-image",
            args.neo4j_image,
            "--neo4j-http-port",
            str(ports["neo4j_http_port"]),
            "--neo4j-bolt-port",
            str(ports["neo4j_bolt_port"]),
            "--aiondb-port",
            str(ports["aiondb_port"]),
            "--out-dir",
            str(artifact_dir),
        ]
        plan.append(
            component_report(
                name="neo4j-graph",
                comparability="direct_graph_cypher",
                command=command,
                env_overrides={},
                artifact_dir=artifact_dir,
                summary={
                    "rows": args.neo4j_rows,
                    "degree": args.neo4j_degree,
                    "warmup": args.neo4j_warmup,
                    "iterations": args.neo4j_iterations,
                    **ports,
                },
                notes=[
                    "Compares AionDB vs Neo4j only.",
                    "Official Neo4j Python driver over Bolt vs psycopg over pgwire.",
                ],
            )
        )
    if "surreal-graph" in args.components:
        env_overrides = {
            "GRAPH_BENCH_NODES": str(args.surreal_graph_nodes),
            "GRAPH_BENCH_FANOUT": str(args.surreal_graph_fanout),
        }
        plan.append(
            component_report(
                name="surreal-graph",
                comparability="direct_graph_protocol_level",
                command=[sys.executable, str(REPO_ROOT / "benchmarks" / "graph_compare.py")],
                env_overrides=env_overrides,
                artifact_dir=run_dir / "surreal-graph",
                summary=env_overrides.copy(),
                notes=[
                    "Compares AionDB vs SurrealDB only.",
                    "Normal protocol paths: pgwire for AionDB, HTTP SQL for SurrealDB.",
                ],
            )
        )
    if "surreal-suite" in args.components:
        env_overrides = {
            "RUN_ID": f"{args.run_id}-surreal-suite",
            "RUN_DIR": str(run_dir / "surreal-suite"),
            "SURREAL_SUITE_ENGINES": args.suite_engines,
            "SURREAL_SUITE_ROWS": str(args.suite_rows),
            "SURREAL_SUITE_WARMUP_SECONDS": str(args.suite_warmup_seconds),
            "SURREAL_SUITE_ITERATIONS": str(args.suite_iterations),
            "SURREAL_SUITE_DURATION_SECONDS": str(args.suite_duration_seconds),
            "SURREAL_SUITE_OPERATION_TIMEOUT_SECONDS": str(args.suite_operation_timeout_seconds),
            "SURREAL_SUITE_TESTS": args.suite_tests,
            "SURREAL_SUITE_UPDATE_DOCS": "0",
        }
        plan.append(
            component_report(
                name="surreal-suite",
                comparability="matrix_workload_family",
                command=[str(REPO_ROOT / "benchmarks" / "surreal-suite" / "run.sh")],
                env_overrides=env_overrides,
                artifact_dir=run_dir / "surreal-suite",
                summary={
                    "engines": args.suite_engines.split(),
                    "rows": args.suite_rows,
                    "warmup_seconds": args.suite_warmup_seconds,
                    "iterations": args.suite_iterations,
                    "duration_seconds": args.suite_duration_seconds,
                    "tests": args.suite_tests,
                },
                notes=[
                    "Compares AionDB, SurrealDB, and optionally pgstack in one matrix.",
                    "Not a pure Neo4j comparison; intended to cover CRUD, scan, graph, and hybrid shapes.",
                ],
            )
        )
    return plan


def execute_component(component: dict[str, Any], run_dir: Path) -> dict[str, Any]:
    name = component["name"]
    stdout_path = run_dir / "logs" / f"{name}.stdout.log"
    stderr_path = run_dir / "logs" / f"{name}.stderr.log"
    env = os.environ.copy()
    env.update(component.get("env_overrides", {}))
    started = time.time()
    completed = run_cmd(component["command_argv"], env=env, stdout_path=stdout_path, stderr_path=stderr_path)
    elapsed_s = time.time() - started

    artifact_dir = Path(component["artifact_dir"])
    stdout_text = stdout_path.read_text(encoding="utf-8") if stdout_path.exists() else ""
    resolved_artifact = artifact_dir
    summary: dict[str, Any] | None = None

    if name == "neo4j-graph":
        nested_run_dir = find_run_dir_from_output(stdout_text, "out=")
        if nested_run_dir is not None:
            resolved_artifact = nested_run_dir
        summary = summarize_neo4j_graph(read_json(resolved_artifact / "report.json"))
    elif name == "surreal-graph":
        result_file = find_result_file_from_text(stdout_text)
        if result_file is None:
            raise RuntimeError("surreal-graph run completed but result_file was not found in stdout")
        resolved_artifact.mkdir(parents=True, exist_ok=True)
        target_report = resolved_artifact / "report.json"
        target_report.write_text(result_file.read_text(encoding="utf-8"), encoding="utf-8")
        summary = summarize_surreal_graph(read_json(target_report))
    elif name == "surreal-suite":
        summary = summarize_surreal_suite(resolved_artifact)

    component["status"] = "passed"
    component["artifact_dir"] = str(resolved_artifact)
    component["summary"] = summary
    component["exit_code"] = completed.returncode
    component["elapsed_seconds"] = round(elapsed_s, 3)
    component["stdout_log"] = str(stdout_path)
    component["stderr_log"] = str(stderr_path)
    return component


def write_manifest(run_dir: Path, payload: dict[str, Any]) -> None:
    (run_dir / "report.json").write_text(json.dumps(payload, indent=2), encoding="utf-8")


def main() -> int:
    args = parse_args()
    run_dir = args.out_dir / args.run_id
    run_dir.mkdir(parents=True, exist_ok=True)

    plan = plan_components(args, run_dir)
    manifest: dict[str, Any] = {
        "benchmark": "ultra-compare",
        "run_id": args.run_id,
        "repo_commit": repo_commit(),
        "generated_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "platform": {
            "python": platform.python_version(),
            "platform": platform.platform(),
        },
        "notes": [
            "This report aggregates benchmark families with different protocol paths and workload scopes.",
            "Use the comparability field before turning a ratio into a public claim.",
        ],
        "components": plan,
    }

    if args.dry_run:
        manifest["status"] = "planned"
        write_manifest(run_dir, manifest)
        print(json.dumps(manifest, indent=2))
        print(f"RUN_DIR={run_dir}")
        return 0

    executed: list[dict[str, Any]] = []
    for component in plan:
        executed.append(execute_component(component, run_dir))

    manifest["status"] = "passed"
    manifest["components"] = executed
    write_manifest(run_dir, manifest)
    print(json.dumps(manifest, indent=2))
    print(f"RUN_DIR={run_dir}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
