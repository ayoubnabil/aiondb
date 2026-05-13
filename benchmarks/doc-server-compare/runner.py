#!/usr/bin/env python3
"""Server-mode equivalent of the embedded benchmark shown in the docs."""

from __future__ import annotations

import argparse
import asyncio
import csv
import json
import math
import platform
import statistics
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Callable

import psycopg
import websockets

PHASE_COUNTER = 0


@dataclass(frozen=True)
class Scenario:
    name: str
    category: str
    aion_sql: Callable[[int, int, int], str]
    surreal_sql: Callable[[int, int, int], str]


class EngineError(Exception):
    pass


class PgEngine:
    def __init__(self, dsn: str, timeout_s: float):
        self.conn = psycopg.connect(dsn, autocommit=True)
        self.timeout_ms = int(timeout_s * 1000)
        with self.conn.cursor() as cur:
            cur.execute(f"SET statement_timeout = {self.timeout_ms}")

    def close(self) -> None:
        self.conn.close()

    def execute(self, query: str) -> int:
        with self.conn.cursor() as cur:
            cur.execute(query)
            if cur.description is not None:
                rows = cur.fetchall()
                return len(rows)
            return cur.rowcount if cur.rowcount >= 0 else 0

    def script(self, statements: list[str]) -> None:
        for statement in statements:
            if statement.strip():
                self.execute(statement)


class SurrealEngine:
    def __init__(self, url: str, username: str, password: str, namespace: str, database: str):
        self.url = url
        self.username = username
        self.password = password
        self.namespace = namespace
        self.database = database
        self.loop = asyncio.new_event_loop()
        self.rpc_id = 0
        self.ws = self.loop.run_until_complete(self._connect())

    async def _connect(self):
        ws = await websockets.connect(self.url, max_size=None, ping_interval=None)
        try:
            await self._call_on(ws, "signin", [{"user": self.username, "pass": self.password}])
        except EngineError:
            await self._call_on(ws, "signin", [{"username": self.username, "password": self.password}])
        await self._call_on(ws, "use", [self.namespace, self.database])
        return ws

    async def _call_on(self, ws, method: str, params: list):
        self.rpc_id += 1
        await ws.send(json.dumps({"id": self.rpc_id, "method": method, "params": params}))
        response = json.loads(await ws.recv())
        if "error" in response:
            raise EngineError(json.dumps(response["error"], ensure_ascii=False))
        return response.get("result")

    async def _call(self, method: str, params: list):
        return await self._call_on(self.ws, method, params)

    async def _reconnect(self) -> None:
        try:
            await self.ws.close()
        except Exception:
            pass
        self.ws = await self._connect()

    def close(self) -> None:
        self.loop.run_until_complete(self.ws.close())
        self.loop.close()

    def execute(self, query: str, timeout_s: float | None = None) -> int:
        try:
            coro = self._call("query", [query])
            result = self.loop.run_until_complete(
                asyncio.wait_for(coro, timeout=timeout_s) if timeout_s else coro
            )
            return len(result) if isinstance(result, list) else 1
        except asyncio.TimeoutError as exc:
            self.loop.run_until_complete(self._reconnect())
            raise EngineError(f"operation timed out after {timeout_s:.3f}s") from exc

    def script(self, statements: list[str], timeout_s: float | None = None) -> None:
        for statement in statements:
            if statement.strip():
                self.execute(statement, timeout_s)


def sql_string(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def vec2(i: int) -> str:
    x = (i % 100) / 100.0
    y = ((i * 7) % 100) / 100.0
    return f"[{x:.4f},{y:.4f}]"


def aion_setup(rows: int) -> list[str]:
    stmts = [
        "DROP TABLE IF EXISTS related CASCADE",
        "DROP TABLE IF EXISTS bench_insert CASCADE",
        "DROP TABLE IF EXISTS record CASCADE",
        """
        CREATE TABLE record (
            id INT PRIMARY KEY,
            user_id INT NOT NULL,
            tenant_id INT NOT NULL,
            likes INT NOT NULL,
            title TEXT NOT NULL,
            body TEXT NOT NULL,
            embedding VECTOR(2)
        )
        """,
        "CREATE INDEX record_user_idx ON record(user_id)",
        "CREATE INDEX record_likes_idx ON record(likes)",
        """
        CREATE TABLE bench_insert (
            id INT PRIMARY KEY,
            user_id INT NOT NULL,
            likes INT NOT NULL,
            title TEXT NOT NULL
        )
        """,
        """
        CREATE TABLE related (
            source_id INT NOT NULL,
            target_id INT NOT NULL,
            weight INT NOT NULL
        )
        """,
        "CREATE NODE LABEL rec ON record",
        "CREATE EDGE LABEL related_to ON related SOURCE rec TARGET rec",
    ]
    record_values = []
    edge_values = []
    for i in range(1, rows + 1):
        user_id = ((i - 1) % 200) + 1
        tenant_id = (i % 16) + 1
        likes = (i * 17) % 10_000
        record_values.append(
            f"({i},{user_id},{tenant_id},{likes},"
            f"{sql_string(f'title-{i}')},{sql_string(f'body text payload {i}')},'{vec2(i)}')"
        )
        edge_values.append(f"({i},{(i % rows) + 1},{i % 10})")
    for chunk in chunks(record_values, 250):
        stmts.append("INSERT INTO record VALUES " + ",".join(chunk))
    for chunk in chunks(edge_values, 250):
        stmts.append("INSERT INTO related VALUES " + ",".join(chunk))
    return stmts


def surreal_setup(rows: int) -> list[str]:
    stmts = [
        "REMOVE TABLE related_to",
        "REMOVE TABLE bench_insert",
        "REMOVE TABLE record",
        "DEFINE TABLE record SCHEMALESS",
        "DEFINE TABLE bench_insert SCHEMALESS",
        "DEFINE TABLE related_to SCHEMALESS TYPE RELATION FROM record TO record",
        "DEFINE INDEX record_user_idx ON TABLE record FIELDS user_id",
        "DEFINE INDEX record_likes_idx ON TABLE record FIELDS likes",
    ]
    record_stmts = []
    edge_stmts = []
    for i in range(1, rows + 1):
        user_id = ((i - 1) % 200) + 1
        tenant_id = (i % 16) + 1
        likes = (i * 17) % 10_000
        record_stmts.append(
            f"CREATE record:{i} SET rid={i}, user_id={user_id}, tenant_id={tenant_id}, "
            f"likes={likes}, title={sql_string(f'title-{i}')}, "
            f"body={sql_string(f'body text payload {i}')}, embedding={vec2(i)}"
        )
        edge_stmts.append(
            f"RELATE record:{i}->related_to->record:{(i % rows) + 1} SET weight={i % 10}"
        )
    for chunk in chunks(record_stmts, 100):
        stmts.append(";".join(chunk) + ";")
    for chunk in chunks(edge_stmts, 100):
        stmts.append(";".join(chunk) + ";")
    return stmts


def chunks(values: list[str], size: int):
    for i in range(0, len(values), size):
        yield values[i : i + size]


def scenarios() -> list[Scenario]:
    return [
        Scenario("[C]reate", "crud", aion_create, surreal_create),
        Scenario("[R]ead::point_id", "crud", aion_read, surreal_read),
        Scenario("[U]pdate::point_id", "crud", aion_update, surreal_update),
        Scenario("[S]can::count_all", "scan", lambda *_: "SELECT count(*) FROM record", lambda *_: "SELECT count() FROM record GROUP ALL"),
        Scenario("[S]can::limit_user_order", "scan", aion_limit_user, surreal_limit_user),
        Scenario("[S]can::range_order_limit", "scan", aion_range_order, surreal_range_order),
        Scenario("[S]can::group_count", "scan", lambda *_: "SELECT tenant_id, count(*) FROM record GROUP BY tenant_id ORDER BY tenant_id", lambda *_: "SELECT tenant_id, count() FROM record GROUP BY tenant_id ORDER BY tenant_id"),
        Scenario("[S]can::order_by_limit", "scan", lambda *_: "SELECT id, title, likes FROM record ORDER BY likes DESC LIMIT 50", lambda *_: "SELECT rid, title, likes FROM record ORDER BY likes DESC LIMIT 50"),
        Scenario("[S]can::big_result_1000", "scan", lambda *_: "SELECT id, title, body, likes FROM record ORDER BY id LIMIT 1000", lambda *_: "SELECT rid, title, body, likes FROM record ORDER BY rid LIMIT 1000"),
        Scenario("[S]can::graph_out_depth1", "graph", aion_graph_out, surreal_graph_out),
        Scenario("[S]can::vector_l2_topk", "vector", lambda *_: "SELECT id, likes, l2_distance(embedding, '[1.0,0.0]') AS dist FROM record ORDER BY dist LIMIT 20", lambda *_: "SELECT rid, likes, vector::distance::euclidean(embedding, [1.0,0.0]) AS dist FROM record ORDER BY dist LIMIT 20"),
        Scenario("[S]can::hybrid_filter_vector", "vector", aion_hybrid_vector, surreal_hybrid_vector),
    ]


def aion_create(i: int, _rows: int, offset: int) -> str:
    ident = 1_000_000 + offset + i
    return f"INSERT INTO bench_insert VALUES ({ident},{(i % 200) + 1},{i % 10000},'insert-{ident}')"


def surreal_create(i: int, _rows: int, offset: int) -> str:
    ident = 1_000_000 + offset + i
    return f"CREATE bench_insert:{ident} SET rid={ident}, user_id={(i % 200) + 1}, likes={i % 10000}, title='insert-{ident}'"


def aion_read(i: int, rows: int, _offset: int) -> str:
    return f"SELECT title FROM record WHERE id = {((i * 17) % rows) + 1} LIMIT 1"


def surreal_read(i: int, rows: int, _offset: int) -> str:
    return f"SELECT title FROM record:{((i * 17) % rows) + 1}"


def aion_update(i: int, rows: int, _offset: int) -> str:
    return f"UPDATE record SET likes = {i % 10000} WHERE id = {((i * 13) % rows) + 1}"


def surreal_update(i: int, rows: int, _offset: int) -> str:
    return f"UPDATE record:{((i * 13) % rows) + 1} SET likes = {i % 10000}"


def aion_limit_user(i: int, _rows: int, _offset: int) -> str:
    return f"SELECT id, title, likes FROM record WHERE user_id = {(i % 200) + 1} ORDER BY id DESC LIMIT 20"


def surreal_limit_user(i: int, _rows: int, _offset: int) -> str:
    return f"SELECT rid, title, likes FROM record WHERE user_id = {(i % 200) + 1} ORDER BY rid DESC LIMIT 20"


def aion_range_order(i: int, _rows: int, _offset: int) -> str:
    low = (i * 37) % 9000
    return f"SELECT id, likes FROM record WHERE likes >= {low} AND likes < {low + 500} ORDER BY likes LIMIT 50"


def surreal_range_order(i: int, _rows: int, _offset: int) -> str:
    low = (i * 37) % 9000
    return f"SELECT rid, likes FROM record WHERE likes >= {low} AND likes < {low + 500} ORDER BY likes LIMIT 50"


def aion_graph_out(i: int, rows: int, _offset: int) -> str:
    ident = ((i * 19) % rows) + 1
    return f"MATCH (a:rec)-[:related_to]->(b:rec) WHERE a.id = {ident} RETURN b.id LIMIT 20"


def surreal_graph_out(i: int, rows: int, _offset: int) -> str:
    return f"SELECT rid FROM record:{((i * 19) % rows) + 1}->related_to->record LIMIT 20"


def aion_hybrid_vector(i: int, _rows: int, _offset: int) -> str:
    return f"SELECT id, likes, l2_distance(embedding, '[1.0,0.0]') AS dist FROM record WHERE tenant_id = {(i % 16) + 1} ORDER BY dist LIMIT 20"


def surreal_hybrid_vector(i: int, _rows: int, _offset: int) -> str:
    return f"SELECT rid, likes, vector::distance::euclidean(embedding, [1.0,0.0]) AS dist FROM record WHERE tenant_id = {(i % 16) + 1} ORDER BY dist LIMIT 20"


def run_phase(engine_name: str, execute, scenario: Scenario, rows: int, seconds: float, timeout_s: float) -> dict:
    global PHASE_COUNTER
    PHASE_COUNTER += 1
    deadline = time.perf_counter() + seconds
    count = 0
    durations = []
    error = ""
    offset = PHASE_COUNTER * 1_000_000
    while time.perf_counter() < deadline:
        query = scenario.aion_sql(count + 1, rows, offset) if engine_name == "aiondb" else scenario.surreal_sql(count + 1, rows, offset)
        start = time.perf_counter()
        try:
            execute(query, timeout_s) if engine_name == "surrealdb" else execute(query)
        except Exception as exc:  # noqa: BLE001
            error = str(exc)
            break
        durations.append((time.perf_counter() - start) * 1000.0)
        count += 1
    status = "OK" if count > 0 and not error else ("UNSUPPORTED" if count == 0 else "FAIL")
    elapsed = max(seconds, sum(durations) / 1000.0)
    return {
        "engine": engine_name,
        "test": scenario.name,
        "category": scenario.category,
        "status": status,
        "ops": count / elapsed if elapsed > 0 else 0.0,
        "mean_ms": statistics.fmean(durations) if durations else math.nan,
        "p95_ms": percentile(durations, 0.95),
        "count": count,
        "error": error,
    }


def percentile(values: list[float], q: float) -> float:
    if not values:
        return math.nan
    values = sorted(values)
    idx = min(len(values) - 1, max(0, int(round((len(values) - 1) * q))))
    return values[idx]


def fmt(value: float) -> str:
    return "-" if math.isnan(value) else f"{value:.3f}"


def write_outputs(run_dir: Path, metadata: dict, rows: list[dict]) -> None:
    run_dir.mkdir(parents=True, exist_ok=True)
    (run_dir / "metadata.json").write_text(json.dumps(metadata, indent=2), encoding="utf-8")
    fields = ["engine", "test", "category", "status", "ops", "mean_ms", "p95_ms", "count", "error"]
    with (run_dir / "summary.tsv").open("w", newline="", encoding="utf-8") as handle:
        writer = csv.DictWriter(handle, fieldnames=fields, delimiter="\t")
        writer.writeheader()
        writer.writerows(rows)
    report = {"metadata": metadata, "results": rows, "ratios": ratios(rows)}
    (run_dir / "results.json").write_text(json.dumps(report, indent=2), encoding="utf-8")


def ratios(rows: list[dict]) -> dict:
    by_test: dict[str, dict[str, dict]] = {}
    for row in rows:
        by_test.setdefault(row["test"], {})[row["engine"]] = row
    out = {}
    for test, engines in by_test.items():
        aion = engines.get("aiondb", {}).get("ops", 0.0)
        surreal = engines.get("surrealdb", {}).get("ops", 0.0)
        out[test] = {
            "aiondb_ops": aion,
            "surrealdb_ops": surreal,
            "aiondb_vs_surrealdb": (aion / surreal) if surreal else None,
        }
    return out


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--run-id", required=True)
    parser.add_argument("--run-dir", required=True)
    parser.add_argument("--rows", type=int, required=True)
    parser.add_argument("--warmup-seconds", type=float, required=True)
    parser.add_argument("--measure-seconds", type=float, required=True)
    parser.add_argument("--operation-timeout-seconds", type=float, required=True)
    parser.add_argument("--surreal-url", required=True)
    parser.add_argument("--surreal-user", required=True)
    parser.add_argument("--surreal-pass", required=True)
    parser.add_argument("--surreal-ns", required=True)
    parser.add_argument("--surreal-db", required=True)
    parser.add_argument("--aiondb-dsn", required=True)
    args = parser.parse_args()

    run_dir = Path(args.run_dir)
    metadata = {
        "run_id": args.run_id,
        "rows": args.rows,
        "warmup_seconds": args.warmup_seconds,
        "measure_seconds": args.measure_seconds,
        "operation_timeout_seconds": args.operation_timeout_seconds,
        "protocols": {"aiondb": "PostgreSQL wire", "surrealdb": "WebSocket JSON-RPC"},
        "platform": platform.platform(),
    }
    rows = []
    aion = PgEngine(args.aiondb_dsn, args.operation_timeout_seconds)
    surreal = SurrealEngine(args.surreal_url, args.surreal_user, args.surreal_pass, args.surreal_ns, args.surreal_db)
    try:
        aion.script(aion_setup(args.rows))
        surreal.script(surreal_setup(args.rows), args.operation_timeout_seconds)
        for scenario in scenarios():
            for engine_name, engine in (("aiondb", aion), ("surrealdb", surreal)):
                run_phase(
                    engine_name,
                    engine.execute,
                    scenario,
                    args.rows,
                    args.warmup_seconds,
                    args.operation_timeout_seconds,
                )
                measured = run_phase(
                    engine_name,
                    engine.execute,
                    scenario,
                    args.rows,
                    args.measure_seconds,
                    args.operation_timeout_seconds,
                )
                rows.append(measured)
                write_outputs(run_dir, metadata, rows)
                print(
                    f"{scenario.name}\t{engine_name}\t{measured['status']}\t"
                    f"ops={fmt(measured['ops'])}\tmean_ms={fmt(measured['mean_ms'])}",
                    flush=True,
                )
    finally:
        aion.close()
        surreal.close()
    write_outputs(run_dir, metadata, rows)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
