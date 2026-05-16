#!/usr/bin/env python3
"""AionDB vs Neo4j graph traversal benchmark.

This harness intentionally measures only graph/Cypher query shapes that both
engines can run without Neo4j GDS plugins. It uses:

- Neo4j over Bolt through the official Python driver.
- AionDB over PostgreSQL wire through psycopg.

Output is written under target/benchmarks/neo4j-graph-compare/<run-id>/.
"""

from __future__ import annotations

import argparse
import json
import os
import socket
import statistics
import subprocess
import sys
import time
from pathlib import Path

import neo4j
import psycopg


REPO_ROOT = Path(__file__).resolve().parents[2]
DEFAULT_OUT = REPO_ROOT / "target" / "benchmarks" / "neo4j-graph-compare"
NEO4J_PASSWORD = "BenchNeo4j42!"
AIONDB_PASSWORD = "BenchAion42!"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--rows", type=int, default=5_000)
    parser.add_argument("--degree", type=int, default=4)
    parser.add_argument("--warmup", type=int, default=5)
    parser.add_argument("--iterations", type=int, default=25)
    parser.add_argument("--neo4j-image", default="neo4j:5-community")
    parser.add_argument("--neo4j-container", default="aiondb-neo4j-graph-bench")
    parser.add_argument("--neo4j-http-port", type=int, default=17474)
    parser.add_argument("--neo4j-bolt-port", type=int, default=17687)
    parser.add_argument("--aiondb-port", type=int, default=15442)
    parser.add_argument("--out-dir", type=Path, default=DEFAULT_OUT)
    parser.add_argument("--keep-neo4j", action="store_true")
    return parser.parse_args()


def run(cmd: list[str], **kwargs) -> subprocess.CompletedProcess:
    check = kwargs.pop("check", True)
    return subprocess.run(cmd, text=True, check=check, **kwargs)


def percentile(values: list[float], pct: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    idx = min(len(ordered) - 1, int(round((pct / 100.0) * (len(ordered) - 1))))
    return ordered[idx]


def wait_port(host: str, port: int, timeout_s: float) -> None:
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        try:
            with socket.create_connection((host, port), timeout=0.5):
                return
        except OSError:
            time.sleep(0.2)
    raise TimeoutError(f"port did not open: {host}:{port}")


def wait_neo4j(uri: str) -> None:
    deadline = time.time() + 90
    last_error: Exception | None = None
    while time.time() < deadline:
        try:
            driver = neo4j.GraphDatabase.driver(uri, auth=("neo4j", NEO4J_PASSWORD))
            with driver.session(database="neo4j") as session:
                session.run("RETURN 1").consume()
            driver.close()
            return
        except Exception as exc:  # noqa: BLE001 - readiness loop
            last_error = exc
            time.sleep(1)
    raise RuntimeError(f"Neo4j did not become ready: {last_error}")


def start_neo4j(args: argparse.Namespace) -> None:
    run(["docker", "rm", "-f", args.neo4j_container], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
    run(
        [
            "docker",
            "run",
            "-d",
            "--name",
            args.neo4j_container,
            "-p",
            f"127.0.0.1:{args.neo4j_http_port}:7474",
            "-p",
            f"127.0.0.1:{args.neo4j_bolt_port}:7687",
            "-e",
            f"NEO4J_AUTH=neo4j/{NEO4J_PASSWORD}",
            "-e",
            "NEO4J_server_memory_heap_initial__size=512m",
            "-e",
            "NEO4J_server_memory_heap_max__size=512m",
            "-e",
            "NEO4J_server_memory_pagecache_size=256m",
            args.neo4j_image,
        ],
        stdout=subprocess.DEVNULL,
    )
    wait_neo4j(f"bolt://127.0.0.1:{args.neo4j_bolt_port}")


def stop_neo4j(args: argparse.Namespace) -> None:
    if not args.keep_neo4j:
        run(["docker", "rm", "-f", args.neo4j_container], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)


def start_aiondb(args: argparse.Namespace, run_dir: Path) -> subprocess.Popen:
    binary = REPO_ROOT / "target" / "release" / "aiondb"
    if not binary.exists():
        run(["cargo", "build", "--release", "-p", "aiondb-server", "--bin", "aiondb"], cwd=REPO_ROOT)
    log_path = run_dir / "aiondb.log"
    env = os.environ.copy()
    env.update(
        {
            "AIONDB_PGWIRE_LISTEN_ADDR": f"127.0.0.1:{args.aiondb_port}",
            "AIONDB_BOOTSTRAP_USER": "bench",
            "AIONDB_BOOTSTRAP_PASSWORD": AIONDB_PASSWORD,
            "AIONDB_BENCH_MODE": "1",
            "AIONDB_LIMITS_STATEMENT_TIMEOUT_MS": "0",
            "AIONDB_LIMITS_MAX_RESULT_ROWS": "2000000",
            "AIONDB_LIMITS_MAX_MEMORY_BYTES": "1073741824",
            "AIONDB_ENGINE_POOL_WORKER_THREADS": "8",
            "AIONDB_PGWIRE_TLS_MODE": "disable",
        }
    )
    log = log_path.open("w", encoding="utf-8")
    proc = subprocess.Popen([str(binary), "--ephemeral"], cwd=REPO_ROOT, env=env, stdout=log, stderr=subprocess.STDOUT)
    try:
        wait_port("127.0.0.1", args.aiondb_port, 30)
    except Exception:
        proc.terminate()
        raise
    return proc


def stop_aiondb(proc: subprocess.Popen | None) -> None:
    if proc is None:
        return
    proc.terminate()
    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=5)


def people(rows: int) -> list[dict[str, object]]:
    return [
        {
            "id": i,
            "number": i % 100,
            "category": f"c{i % 10}",
            "name": f"person-{i}",
        }
        for i in range(1, rows + 1)
    ]


def edges(rows: int, degree: int) -> list[dict[str, object]]:
    out: list[dict[str, object]] = []
    edge_id = 1
    for source in range(1, rows + 1):
        for d in range(1, degree + 1):
            target = ((source + d * d - 1) % rows) + 1
            if target == source:
                target = (target % rows) + 1
            out.append(
                {
                    "id": edge_id,
                    "source_id": source,
                    "target_id": target,
                    "weight": (source * (d + 3)) % 50,
                    "relation": "friend" if d % 2 else "ref",
                }
            )
            edge_id += 1
    return out


def chunks(items: list[dict[str, object]], size: int):
    for i in range(0, len(items), size):
        yield items[i : i + size]


def insert_values_sql(table: str, cols: list[str], batch: list[dict[str, object]]) -> str:
    def literal(value: object) -> str:
        if isinstance(value, str):
            return "'" + value.replace("'", "''") + "'"
        return str(value)

    values = ", ".join(
        "(" + ", ".join(literal(row[col]) for col in cols) + ")" for row in batch
    )
    return f"INSERT INTO {table} ({', '.join(cols)}) VALUES {values}"


def load_aiondb(conn: psycopg.Connection, nodes: list[dict[str, object]], rels: list[dict[str, object]]) -> None:
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
            CREATE INDEX knows_weight_idx ON knows (weight);
            """
        )
        for batch in chunks(nodes, 500):
            cur.execute(insert_values_sql("person", ["id", "number", "category", "name"], batch))
        for batch in chunks(rels, 500):
            cur.execute(insert_values_sql("knows", ["id", "source_id", "target_id", "weight", "relation"], batch))
    conn.commit()


def load_neo4j(driver: neo4j.Driver, nodes: list[dict[str, object]], rels: list[dict[str, object]]) -> None:
    with driver.session(database="neo4j") as session:
        session.run("MATCH (n) DETACH DELETE n").consume()
        session.run("CREATE CONSTRAINT person_id IF NOT EXISTS FOR (p:Person) REQUIRE p.id IS UNIQUE").consume()
        session.run("CREATE INDEX knows_weight IF NOT EXISTS FOR ()-[r:KNOWS]-() ON (r.weight)").consume()
        session.run("CALL db.awaitIndexes()").consume()
        for batch in chunks(nodes, 1_000):
            session.run(
                """
                UNWIND $rows AS row
                CREATE (:Person {
                    id: row.id,
                    number: row.number,
                    category: row.category,
                    name: row.name
                })
                """,
                rows=batch,
            ).consume()
        for batch in chunks(rels, 1_000):
            session.run(
                """
                UNWIND $rows AS row
                MATCH (a:Person {id: row.source_id})
                MATCH (b:Person {id: row.target_id})
                CREATE (a)-[:KNOWS {
                    id: row.id,
                    weight: row.weight,
                    relation: row.relation
                }]->(b)
                """,
                rows=batch,
            ).consume()


def query_cases(rows: int) -> list[tuple[str, str]]:
    target = min(rows, 128)
    return [
        ("out_depth1", "MATCH (a:Person {id: 1})-[:KNOWS]->(b:Person) RETURN count(b)"),
        ("out_depth2", "MATCH (a:Person {id: 1})-[:KNOWS]->(b:Person)-[:KNOWS]->(c:Person) RETURN count(c)"),
        ("out_depth3", "MATCH (a:Person {id: 1})-[:KNOWS]->(b:Person)-[:KNOWS]->(c:Person)-[:KNOWS]->(d:Person) RETURN count(d)"),
        ("in_depth1", "MATCH (a:Person)-[:KNOWS]->(b:Person {id: 2}) RETURN count(a)"),
        ("edge_filter", "MATCH (:Person)-[e:KNOWS]->(b:Person) WHERE e.weight > 10 RETURN count(b)"),
        ("multi_out_where", "MATCH (a:Person)-[:KNOWS]->(b:Person), (a)-[:KNOWS]->(c:Person) WHERE b.number > 20 AND b.id <> c.id RETURN count(*)"),
        ("variable_len_4", "MATCH (a:Person {id: 1})-[:KNOWS*..4]->(b:Person) RETURN count(b)"),
        ("shortest_path", f"MATCH p = shortestPath((a:Person {{id: 1}})-[:KNOWS*..6]->(b:Person {{id: {target}}})) RETURN count(p)"),
    ]


def time_query(fn, warmup: int, iterations: int) -> tuple[list[float], object]:
    result = None
    for _ in range(warmup):
        result = fn()
    timings: list[float] = []
    for _ in range(iterations):
        start = time.perf_counter()
        result = fn()
        timings.append((time.perf_counter() - start) * 1000.0)
    return timings, result


def normalize_result(rows: object) -> list[list[object]]:
    normalized: list[list[object]] = []
    for row in rows:
        values = list(row)
        out = []
        for value in values:
            if isinstance(value, str) and value.lstrip("-").isdigit():
                out.append(int(value))
            else:
                out.append(value)
        normalized.append(out)
    return normalized


def summarize(values: list[float]) -> dict[str, float]:
    return {
        "mean_ms": statistics.fmean(values),
        "p50_ms": statistics.median(values),
        "p95_ms": percentile(values, 95),
        "min_ms": min(values),
        "max_ms": max(values),
    }


def main() -> int:
    args = parse_args()
    run_id = time.strftime("%Y%m%d-%H%M%S")
    run_dir = args.out_dir / run_id
    run_dir.mkdir(parents=True, exist_ok=True)

    nodes = people(args.rows)
    rels = edges(args.rows, args.degree)
    aiondb_proc: subprocess.Popen | None = None
    results: list[dict[str, object]] = []

    try:
        start_neo4j(args)
        aiondb_proc = start_aiondb(args, run_dir)

        aiondb_conn = psycopg.connect(
            host="127.0.0.1",
            port=args.aiondb_port,
            dbname="default",
            user="bench",
            password=AIONDB_PASSWORD,
            sslmode="disable",
            autocommit=False,
        )
        neo4j_driver = neo4j.GraphDatabase.driver(
            f"bolt://127.0.0.1:{args.neo4j_bolt_port}",
            auth=("neo4j", NEO4J_PASSWORD),
        )

        load_aiondb(aiondb_conn, nodes, rels)
        load_neo4j(neo4j_driver, nodes, rels)

        with aiondb_conn.cursor() as cur, neo4j_driver.session(database="neo4j") as neo4j_session:
            for name, query in query_cases(args.rows):
                def run_aiondb(q=query):
                    cur.execute(q)
                    return cur.fetchall()

                def run_neo4j(q=query):
                    return [record.values() for record in neo4j_session.run(q)]

                aion_times, aion_result = time_query(run_aiondb, args.warmup, args.iterations)
                neo_times, neo_result = time_query(run_neo4j, args.warmup, args.iterations)
                aion_normalized = normalize_result(aion_result)
                neo_normalized = normalize_result(neo_result)
                results.append(
                    {
                        "name": name,
                        "query": query,
                        "aiondb": summarize(aion_times),
                        "neo4j": summarize(neo_times),
                        "aiondb_result": aion_normalized,
                        "neo4j_result": neo_normalized,
                        "result_parity": aion_normalized == neo_normalized,
                        "ratio_aiondb_vs_neo4j_p50": statistics.median(aion_times)
                        / max(statistics.median(neo_times), 0.000001),
                    }
                )

        neo4j_driver.close()
        aiondb_conn.close()

        report = {
            "run_id": run_id,
            "rows": args.rows,
            "degree": args.degree,
            "edges": len(rels),
            "warmup": args.warmup,
            "iterations": args.iterations,
            "neo4j_image": args.neo4j_image,
            "clients": {"aiondb": "psycopg/pgwire", "neo4j": "neo4j-python-driver/bolt"},
            "results": results,
        }
        (run_dir / "report.json").write_text(json.dumps(report, indent=2), encoding="utf-8")
        write_summary(run_dir / "summary.tsv", results)
        print_summary(report, run_dir)
        return 0
    finally:
        stop_aiondb(aiondb_proc)
        stop_neo4j(args)


def write_summary(path: Path, results: list[dict[str, object]]) -> None:
    lines = ["query\taiondb_p50_ms\tneo4j_p50_ms\tratio_aiondb_vs_neo4j\tparity"]
    for row in results:
        a = row["aiondb"]
        n = row["neo4j"]
        lines.append(
            f"{row['name']}\t{a['p50_ms']:.3f}\t{n['p50_ms']:.3f}\t{row['ratio_aiondb_vs_neo4j_p50']:.2f}\t{row['result_parity']}"
        )
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def print_summary(report: dict[str, object], run_dir: Path) -> None:
    print(f"== neo4j graph compare ==")
    print(f"rows={report['rows']} edges={report['edges']} warmup={report['warmup']} iterations={report['iterations']}")
    print(f"out={run_dir}")
    print("query                         aiondb_p50_ms  neo4j_p50_ms  ratio  parity")
    for row in report["results"]:
        a = row["aiondb"]
        n = row["neo4j"]
        print(
            f"{row['name']:<30} {a['p50_ms']:>12.3f} {n['p50_ms']:>13.3f} {row['ratio_aiondb_vs_neo4j_p50']:>6.2f}  {row['result_parity']}"
        )


if __name__ == "__main__":
    sys.exit(main())
