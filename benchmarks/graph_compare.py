#!/usr/bin/env python3
"""End-to-end graph traversal benchmark: AionDB vs SurrealDB.

This intentionally benchmarks the servers through their normal client protocols:
PostgreSQL wire protocol for AionDB, HTTP SQL endpoint for SurrealDB.
"""

from __future__ import annotations

import json
import os
import socket
import statistics
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable

import psycopg
import requests


ROOT = Path(__file__).resolve().parents[1]
AION_BIN = ROOT / "target" / "release" / "aiondb"
SURREAL_BIN = Path("/home/depinfo/.surrealdb/surreal")
AION_USER = "admin"
AION_PASSWORD = "Benchpass123!"
BENCH_NS = "bench"
BENCH_DB = "bench"


@dataclass
class TimedResult:
    name: str
    median_ms: float
    p95_ms: float
    min_ms: float
    max_ms: float
    samples: int
    result_sample: Any


def free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def percentile(values: list[float], q: float) -> float:
    values = sorted(values)
    if not values:
        return float("nan")
    idx = min(len(values) - 1, max(0, round((len(values) - 1) * q)))
    return values[idx]


def measure(name: str, fn: Callable[[], Any], warmups: int = 5, runs: int = 30) -> TimedResult:
    for _ in range(warmups):
        fn()
    values: list[float] = []
    sample: Any = None
    for _ in range(runs):
        start = time.perf_counter()
        sample = fn()
        values.append((time.perf_counter() - start) * 1000.0)
    return TimedResult(
        name=name,
        median_ms=statistics.median(values),
        p95_ms=percentile(values, 0.95),
        min_ms=min(values),
        max_ms=max(values),
        samples=runs,
        result_sample=sample,
    )


def generate_graph(nodes: int, fanout: int) -> tuple[list[tuple[int, int]], list[tuple[int, int, int]]]:
    node_rows = [(i, i % 97) for i in range(nodes)]
    edge_rows: list[tuple[int, int, int]] = []
    offsets = [1, 7, 31, 127, 509, 1021, 4093, 8191]
    for src in range(nodes):
        seen: set[int] = set()
        for j in range(fanout):
            dst = (src + offsets[j % len(offsets)] + (src * (j + 3)) % nodes) % nodes
            if dst == src:
                dst = (dst + 1) % nodes
            if dst in seen:
                dst = (dst + j + 1) % nodes
            seen.add(dst)
            edge_rows.append((src, dst, (src + dst + j) % 100 + 1))
    return node_rows, edge_rows


def start_aion(port: int, log_path: Path) -> subprocess.Popen[str]:
    env = os.environ.copy()
    env["AIONDB_BENCH_MODE"] = "1"
    env["AIONDB_BOOTSTRAP_USER"] = AION_USER
    env["AIONDB_BOOTSTRAP_PASSWORD"] = AION_PASSWORD
    cmd = [
        str(AION_BIN),
        "--ephemeral",
        "--listen-addr",
        f"127.0.0.1:{port}",
        "--no-observability",
    ]
    log = log_path.open("w", encoding="utf-8")
    return subprocess.Popen(cmd, cwd=ROOT, env=env, stdout=log, stderr=subprocess.STDOUT, text=True)


def start_surreal(port: int, log_path: Path) -> subprocess.Popen[str]:
    cmd = [
        str(SURREAL_BIN),
        "start",
        "--no-banner",
        "--unauthenticated",
        "--bind",
        f"127.0.0.1:{port}",
        "memory",
    ]
    log = log_path.open("w", encoding="utf-8")
    return subprocess.Popen(cmd, cwd=ROOT, stdout=log, stderr=subprocess.STDOUT, text=True)


def wait_aion(port: int, timeout_s: float = 20.0) -> psycopg.Connection[Any]:
    deadline = time.time() + timeout_s
    last_error: Exception | None = None
    dsn = (
        f"host=127.0.0.1 port={port} dbname=default user={AION_USER} "
        f"password={AION_PASSWORD} sslmode=disable"
    )
    while time.time() < deadline:
        try:
            conn = psycopg.connect(dsn, autocommit=True)
            with conn.cursor() as cur:
                cur.execute("SELECT 1")
                cur.fetchone()
            return conn
        except Exception as exc:  # noqa: BLE001 - readiness loop
            last_error = exc
            time.sleep(0.25)
    raise RuntimeError(f"AionDB did not become ready: {last_error}")


def surreal_sql(port: int, sql: str, timeout_s: float = 60.0) -> Any:
    url = f"http://127.0.0.1:{port}/sql"
    headers = {
        "Accept": "application/json",
        "Content-Type": "application/surrealql",
        "NS": BENCH_NS,
        "DB": BENCH_DB,
        "Surreal-NS": BENCH_NS,
        "Surreal-DB": BENCH_DB,
    }
    if not sql.lstrip().upper().startswith("USE "):
        sql = f"USE NS {BENCH_NS} DB {BENCH_DB};\n{sql}"
    response = requests.post(url, headers=headers, data=sql.encode("utf-8"), timeout=timeout_s)
    response.raise_for_status()
    payload = response.json()
    if isinstance(payload, list):
        for entry in payload:
            if isinstance(entry, dict) and entry.get("status") not in {None, "OK"}:
                raise RuntimeError(f"SurrealDB SQL error: {entry}")
        return payload
    if isinstance(payload, dict) and payload.get("status") not in {None, "OK"}:
        raise RuntimeError(f"SurrealDB SQL error: {payload}")
    return payload


def wait_surreal(port: int, timeout_s: float = 20.0) -> None:
    deadline = time.time() + timeout_s
    last_error: Exception | None = None
    while time.time() < deadline:
        try:
            surreal_sql(port, "RETURN 1;", timeout_s=2.0)
            return
        except Exception as exc:  # noqa: BLE001 - readiness loop
            last_error = exc
            time.sleep(0.25)
    raise RuntimeError(f"SurrealDB did not become ready: {last_error}")


def setup_aion(conn: psycopg.Connection[Any], nodes: list[tuple[int, int]], edges: list[tuple[int, int, int]]) -> dict[str, Any]:
    start = time.perf_counter()
    notes: list[str] = []
    with conn.cursor() as cur:
        cur.execute("CREATE TABLE nodes (id INT NOT NULL, payload INT)")
        cur.execute("CREATE TABLE edges (source_id INT NOT NULL, target_id INT NOT NULL, weight INT)")
        try:
            cur.execute("CREATE NODE LABEL node ON nodes")
            cur.execute("CREATE EDGE LABEL edge ON edges SOURCE node TARGET node")
        except Exception as exc:  # graph labels are useful for algorithms, not required for SQL traversals
            notes.append(f"graph label DDL skipped: {exc}")
        cur.executemany("INSERT INTO nodes (id, payload) VALUES (%s, %s)", nodes)
        cur.executemany("INSERT INTO edges (source_id, target_id, weight) VALUES (%s, %s, %s)", edges)
        try:
            cur.execute("CREATE INDEX edges_source_idx ON edges (source_id)")
            cur.execute("CREATE INDEX edges_target_idx ON edges (target_id)")
        except Exception as exc:
            notes.append(f"edge indexes skipped: {exc}")
    return {"load_ms": (time.perf_counter() - start) * 1000.0, "notes": notes}


def setup_surreal(port: int, nodes: list[tuple[int, int]], edges: list[tuple[int, int, int]]) -> dict[str, Any]:
    start = time.perf_counter()
    surreal_sql(
        port,
        "\n".join(
            [
                "DEFINE TABLE node SCHEMALESS;",
                "DEFINE TABLE edge TYPE RELATION FROM node TO node SCHEMALESS;",
            ]
        ),
    )

    chunk_size = 250
    for idx in range(0, len(nodes), chunk_size):
        statements = [f"CREATE node:{node_id} SET payload = {payload};" for node_id, payload in nodes[idx : idx + chunk_size]]
        surreal_sql(port, "\n".join(statements))

    for idx in range(0, len(edges), chunk_size):
        statements = [
            f"RELATE node:{src}->edge->node:{dst} SET weight = {weight};"
            for src, dst, weight in edges[idx : idx + chunk_size]
        ]
        surreal_sql(port, "\n".join(statements), timeout_s=120.0)

    return {"load_ms": (time.perf_counter() - start) * 1000.0, "notes": []}


def scalar_from_surreal(payload: Any) -> Any:
    if isinstance(payload, list) and payload:
        result = payload[-1].get("result") if isinstance(payload[-1], dict) else payload[-1]
        if isinstance(result, list) and len(result) == 1:
            return result[0]
        return result
    return payload


def build_surreal_count_query(port: int, path: str, source: int) -> tuple[str, Callable[[], Any]]:
    candidates = [
        f"RETURN array::len(node:{source}{path});",
        f"RETURN count(node:{source}{path});",
    ]
    for query in candidates:
        try:
            surreal_sql(port, query)
            return query, lambda q=query: scalar_from_surreal(surreal_sql(port, q))
        except Exception:
            pass

    query = f"RETURN node:{source}{path};"

    def fallback() -> int:
        value = scalar_from_surreal(surreal_sql(port, query))
        return len(value) if isinstance(value, list) else 0

    return query, fallback


def aion_scalar(conn: psycopg.Connection[Any], sql: str, param: int | None = None) -> Any:
    with conn.cursor() as cur:
        if param is None:
            cur.execute(sql)
        else:
            cur.execute(sql, (param,))
        row = cur.fetchone()
        return row[0] if row else None


def run_benchmark(nodes_count: int = 1000, fanout: int = 4) -> dict[str, Any]:
    if not AION_BIN.exists():
        raise FileNotFoundError(f"AionDB binary missing: {AION_BIN}")
    if not SURREAL_BIN.exists():
        raise FileNotFoundError(f"SurrealDB binary missing: {SURREAL_BIN}")

    nodes, edges = generate_graph(nodes_count, fanout)
    source = min(42, nodes_count - 1)
    timestamp = time.strftime("%Y%m%d-%H%M%S")
    aion_port = free_port()
    surreal_port = free_port()
    aion_log = Path("/tmp") / f"aiondb-graph-bench-{timestamp}.log"
    surreal_log = Path("/tmp") / f"surrealdb-graph-bench-{timestamp}.log"
    procs: list[subprocess.Popen[str]] = []
    conn: psycopg.Connection[Any] | None = None
    try:
        aion_proc = start_aion(aion_port, aion_log)
        surreal_proc = start_surreal(surreal_port, surreal_log)
        procs.extend([aion_proc, surreal_proc])
        conn = wait_aion(aion_port)
        wait_surreal(surreal_port)

        aion_setup = setup_aion(conn, nodes, edges)
        surreal_setup = setup_surreal(surreal_port, nodes, edges)

        workloads: list[TimedResult] = []
        skipped: dict[str, str] = {}
        aion_queries = {
            "1-hop": "SELECT COUNT(*) FROM edges WHERE source_id = %s",
            "2-hop": (
                "SELECT COUNT(*) FROM edges e1 "
                "JOIN edges e2 ON e2.source_id = e1.target_id "
                "WHERE e1.source_id = %s"
            ),
            "3-hop": (
                "SELECT COUNT(*) FROM edges e1 "
                "JOIN edges e2 ON e2.source_id = e1.target_id "
                "JOIN edges e3 ON e3.source_id = e2.target_id "
                "WHERE e1.source_id = %s"
            ),
        }
        surreal_paths = {
            "1-hop": "->edge->node",
            "2-hop": "->edge->node->edge->node",
            "3-hop": "->edge->node->edge->node->edge->node",
        }
        aion_cypher_queries = {
            "1-hop": f"MATCH (a:node {{id: {source}}})-[:edge]->(b:node) RETURN count(b)",
            "2-hop": (
                f"MATCH (a:node {{id: {source}}})-[:edge]->(b:node)-[:edge]->(c:node) "
                "RETURN count(c)"
            ),
            "3-hop": (
                f"MATCH (a:node {{id: {source}}})-[:edge]->(b:node)-[:edge]->(c:node)-[:edge]->(d:node) "
                "RETURN count(d)"
            ),
        }
        surreal_query_text: dict[str, str] = {}

        for hop_name in ("1-hop", "2-hop", "3-hop"):
            workloads.append(
                measure(
                    f"aiondb_sql_{hop_name}",
                    lambda sql=aion_queries[hop_name]: aion_scalar(conn, sql, source),  # type: ignore[arg-type]
                )
            )
            try:
                aion_scalar(conn, aion_cypher_queries[hop_name])
                workloads.append(
                    measure(
                        f"aiondb_match_{hop_name}",
                        lambda sql=aion_cypher_queries[hop_name]: aion_scalar(conn, sql),  # type: ignore[arg-type]
                    )
                )
            except Exception as exc:
                skipped[f"aiondb_match_{hop_name}"] = str(exc)
            query_text, surreal_fn = build_surreal_count_query(surreal_port, surreal_paths[hop_name], source)
            surreal_query_text[hop_name] = query_text
            workloads.append(measure(f"surrealdb_graph_{hop_name}", surreal_fn))

        aion_extra: dict[str, Any] = {}
        try:
            aion_extra["pagerank"] = measure(
                "aiondb_pagerank_top10",
                lambda: aion_scalar(
                    conn,  # type: ignore[arg-type]
                    "CALL graph.pageRank() YIELD nodeId, score RETURN nodeId, score LIMIT 10",
                ),
                warmups=1,
                runs=5,
            ).__dict__
        except Exception as exc:
            aion_extra["pagerank_skipped"] = str(exc)

        return {
            "benchmark": "aiondb_vs_surrealdb_graph",
            "mode": "end_to_end_server_protocol",
            "dataset": {"nodes": len(nodes), "edges": len(edges), "fanout": fanout, "source": source},
            "skipped": skipped,
            "aiondb": {"port": aion_port, "log": str(aion_log), "setup": aion_setup, "extra": aion_extra},
            "surrealdb": {
                "port": surreal_port,
                "log": str(surreal_log),
                "setup": surreal_setup,
                "queries": surreal_query_text,
            },
            "results": [result.__dict__ for result in workloads],
        }
    finally:
        if conn is not None:
            conn.close()
        for proc in procs:
            if proc.poll() is None:
                proc.terminate()
        deadline = time.time() + 5
        for proc in procs:
            if proc.poll() is None:
                try:
                    proc.wait(timeout=max(0.1, deadline - time.time()))
                except subprocess.TimeoutExpired:
                    proc.kill()


def print_table(results: dict[str, Any]) -> None:
    print(json.dumps(results, indent=2, sort_keys=True))
    print("\nSummary (median ms, lower is better)")
    print("| workload | median_ms | p95_ms | sample |")
    print("|---|---:|---:|---:|")
    for row in results["results"]:
        print(f"| {row['name']} | {row['median_ms']:.3f} | {row['p95_ms']:.3f} | {row['result_sample']} |")


def main() -> int:
    nodes = int(os.environ.get("GRAPH_BENCH_NODES", "1000"))
    fanout = int(os.environ.get("GRAPH_BENCH_FANOUT", "4"))
    results = run_benchmark(nodes, fanout)
    out_path = Path("/tmp") / f"graph-benchmark-{time.strftime('%Y%m%d-%H%M%S')}.json"
    out_path.write_text(json.dumps(results, indent=2, sort_keys=True), encoding="utf-8")
    results["result_file"] = str(out_path)
    print_table(results)
    return 0


if __name__ == "__main__":
    sys.exit(main())
