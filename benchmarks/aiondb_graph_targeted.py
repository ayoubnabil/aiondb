#!/usr/bin/env python3
from __future__ import annotations

import json
import os
import socket
import statistics
import subprocess
import time
from pathlib import Path

import psycopg


ROOT = Path(__file__).resolve().parents[1]
AION_BIN = ROOT / "target" / "release" / "aiondb"
OUT_ROOT = ROOT / "target" / "benchmarks" / "aiondb-graph-targeted"
PORT = 15442
PASSWORD = "BenchAion42!"


def wait_port(port: int, timeout_s: float = 30.0) -> None:
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.5):
                return
        except OSError:
            time.sleep(0.2)
    raise RuntimeError(f"port did not open: 127.0.0.1:{port}")


def summarize(values: list[float]) -> dict[str, float]:
    return {
        "p50_ms": statistics.median(values),
        "min_ms": min(values),
        "max_ms": max(values),
        "mean_ms": statistics.fmean(values),
    }


def time_query(cur: psycopg.Cursor, query: str, warmup: int, iterations: int) -> tuple[dict[str, float], list[tuple]]:
    rows = []
    for _ in range(warmup):
        cur.execute(query)
        rows = cur.fetchall()
    values: list[float] = []
    for _ in range(iterations):
        start = time.perf_counter()
        cur.execute(query)
        rows = cur.fetchall()
        values.append((time.perf_counter() - start) * 1000.0)
    return summarize(values), rows


def literal(value: object) -> str:
    if isinstance(value, str):
        return "'" + value.replace("'", "''") + "'"
    return str(value)


def insert_values_sql(table: str, cols: list[str], rows: list[dict[str, object]]) -> str:
    values = ", ".join(
        "(" + ", ".join(literal(row[col]) for col in cols) + ")" for row in rows
    )
    return f"INSERT INTO {table} ({', '.join(cols)}) VALUES {values}"


def build_dataset(rows: int, degree: int) -> tuple[list[dict[str, object]], list[dict[str, object]]]:
    nodes = [
        {"id": i, "number": i % 100, "category": f"c{i % 10}", "name": f"person-{i}"}
        for i in range(1, rows + 1)
    ]
    rels: list[dict[str, object]] = []
    edge_id = 1
    for source in range(1, rows + 1):
        for d in range(1, degree + 1):
            target = ((source + d * d - 1) % rows) + 1
            if target == source:
                target = (target % rows) + 1
            rels.append(
                {
                    "id": edge_id,
                    "source_id": source,
                    "target_id": target,
                    "weight": (source * (d + 3)) % 50,
                    "relation": "friend" if d % 2 else "ref",
                }
            )
            edge_id += 1
    return nodes, rels


def main() -> int:
    rows = int(os.environ.get("AION_BENCH_ROWS", "2000"))
    degree = int(os.environ.get("AION_BENCH_DEGREE", "6"))
    warmup = int(os.environ.get("AION_BENCH_WARMUP", "2"))
    iterations = int(os.environ.get("AION_BENCH_ITERS", "8"))

    OUT_ROOT.mkdir(parents=True, exist_ok=True)
    run_id = time.strftime("%Y%m%d-%H%M%S")
    run_dir = OUT_ROOT / run_id
    run_dir.mkdir(parents=True, exist_ok=True)

    env = os.environ.copy()
    env.update(
        {
            "AIONDB_PGWIRE_LISTEN_ADDR": f"127.0.0.1:{PORT}",
            "AIONDB_BOOTSTRAP_USER": "bench",
            "AIONDB_BOOTSTRAP_PASSWORD": PASSWORD,
            "AIONDB_BENCH_MODE": "1",
            "AIONDB_LIMITS_STATEMENT_TIMEOUT_MS": "0",
            "AIONDB_LIMITS_MAX_RESULT_ROWS": "2000000",
            "AIONDB_LIMITS_MAX_MEMORY_BYTES": "1073741824",
            "AIONDB_ENGINE_POOL_WORKER_THREADS": "8",
            "AIONDB_PGWIRE_TLS_MODE": "disable",
        }
    )

    server_log = (run_dir / "aiondb.log").open("w", encoding="utf-8")
    proc = subprocess.Popen(
        [str(AION_BIN), "--ephemeral"],
        cwd=ROOT,
        env=env,
        stdout=server_log,
        stderr=subprocess.STDOUT,
    )

    try:
        wait_port(PORT)
        conn = psycopg.connect(
            host="127.0.0.1",
            port=PORT,
            dbname="default",
            user="bench",
            password=PASSWORD,
            sslmode="disable",
            autocommit=False,
        )
        try:
            nodes, rels = build_dataset(rows, degree)
            with conn.cursor() as cur:
                cur.execute(
                    """
                    CREATE TABLE person (
                        id INT PRIMARY KEY,
                        number INT NOT NULL,
                        category TEXT NOT NULL,
                        name TEXT NOT NULL
                    );
                    CREATE TABLE knows (
                        id INT PRIMARY KEY,
                        source_id INT NOT NULL,
                        target_id INT NOT NULL,
                        weight INT NOT NULL,
                        relation TEXT NOT NULL
                    );
                    CREATE NODE LABEL Person ON person;
                    CREATE EDGE LABEL KNOWS ON knows SOURCE Person TARGET Person;
                    CREATE INDEX knows_source_idx ON knows (source_id);
                    CREATE INDEX knows_target_idx ON knows (target_id);
                    CREATE INDEX person_number_idx ON person (number);
                    """
                )
                for i in range(0, len(nodes), 250):
                    cur.execute(insert_values_sql("person", ["id", "number", "category", "name"], nodes[i : i + 250]))
                for i in range(0, len(rels), 250):
                    cur.execute(
                        insert_values_sql(
                            "knows",
                            ["id", "source_id", "target_id", "weight", "relation"],
                            rels[i : i + 250],
                        )
                    )
            conn.commit()

            queries = [
                (
                    "unanchored_twohop_end_filter",
                    "MATCH (a:Person)-[:KNOWS]->(b:Person)-[:KNOWS]->(c:Person) "
                    "WHERE c.number > 63 RETURN count(c)",
                ),
                (
                    "multi_out_count_distinct",
                    "MATCH (a:Person)-[:KNOWS]->(b:Person), (a)-[:KNOWS]->(c:Person) "
                    "WHERE b.number > 20 AND b.id <> c.id RETURN count(DISTINCT c.id)",
                ),
                (
                    "multi_out_return_limit",
                    "MATCH (a:Person)-[:KNOWS]->(b:Person), (a)-[:KNOWS]->(c:Person) "
                    "RETURN b.id, c.id ORDER BY b.id, c.id LIMIT 100",
                ),
            ]

            results = []
            with conn.cursor() as cur:
                for name, query in queries:
                    timing, sample = time_query(cur, query, warmup, iterations)
                    results.append(
                        {
                            "name": name,
                            "query": query,
                            "aiondb": timing,
                            "result_sample": [list(row) for row in sample[:10]],
                        }
                    )

            report = {
                "run_id": run_id,
                "rows": rows,
                "degree": degree,
                "warmup": warmup,
                "iterations": iterations,
                "results": results,
            }
            (run_dir / "report.json").write_text(json.dumps(report, indent=2), encoding="utf-8")
            print(json.dumps(report, indent=2))
            print(f"RUN_DIR={run_dir}")
        finally:
            conn.close()
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=5)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
