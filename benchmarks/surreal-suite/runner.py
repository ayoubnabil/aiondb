#!/usr/bin/env python3
"""Run a SurrealDB article-style benchmark matrix.

This harness intentionally keeps the benchmark logic local and explicit:

* SurrealDB is reached through JSON-RPC over WebSocket.
* AionDB is reached through PostgreSQL wire.
* PostgreSQL stack is reached through PostgreSQL wire and uses pgvector / AGE
  where those extensions are installed.

Each test is warmed up on every selected engine, then measured for N fixed-time
iterations. Raw per-phase traces and summary files are written to the run dir.
"""

from __future__ import annotations

import argparse
import asyncio
import csv
import json
import math
import os
import platform
import statistics
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Callable

import psycopg
import websockets


SqlFn = Callable[[str, int], str]


@dataclass(frozen=True)
class TestSpec:
    name: str
    category: str
    sql: SqlFn | None = None
    aiondb: SqlFn | None = None
    pgstack: SqlFn | None = None
    surrealdb: SqlFn | None = None
    prepare_sql: Callable[[str], list[str]] | None = None
    prepare_surrealdb: Callable[[], list[str]] | None = None


class EngineError(Exception):
    pass


class OperationTimeout(EngineError):
    pass


class PgEngine:
    def __init__(self, name: str, dsn: str):
        self.name = name
        self.conn = psycopg.connect(dsn, autocommit=True)
        self.timeout_ms: int | None = None
        self.timeout_supported = True

    def close(self) -> None:
        self.conn.close()

    def configure_timeout(self, timeout_s: float | None) -> None:
        if not self.timeout_supported or timeout_s is None:
            return
        timeout_ms = max(1, int(timeout_s * 1000))
        if self.timeout_ms == timeout_ms:
            return
        try:
            with self.conn.cursor() as cur:
                cur.execute(f"SET statement_timeout = {timeout_ms}")
            self.timeout_ms = timeout_ms
        except Exception:
            self.timeout_supported = False

    def execute(self, query: str, timeout_s: float | None = None) -> None:
        self.configure_timeout(timeout_s)
        with self.conn.cursor() as cur:
            cur.execute(query)
            if cur.description is not None:
                cur.fetchall()

    def script(self, statements: list[str], timeout_s: float | None = None) -> None:
        for statement in statements:
            if statement.strip():
                self.execute(statement, timeout_s)


class DisabledEngine:
    def __init__(self, name: str, reason: str):
        self.name = name
        self.reason = reason

    def close(self) -> None:
        return None

    def execute(self, query: str, timeout_s: float | None = None) -> None:
        raise EngineError(self.reason)

    def script(self, statements: list[str], timeout_s: float | None = None) -> None:
        raise EngineError(self.reason)


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
            await self._call_on(
                ws,
                "signin",
                [{"username": self.username, "password": self.password}],
            )
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

    def execute(self, query: str, timeout_s: float | None = None) -> None:
        try:
            if timeout_s is None:
                self.loop.run_until_complete(self._call("query", [query]))
            else:
                self.loop.run_until_complete(
                    asyncio.wait_for(self._call("query", [query]), timeout=timeout_s)
                )
        except asyncio.TimeoutError as exc:
            self.loop.run_until_complete(self._reconnect())
            raise OperationTimeout(f"operation timed out after {timeout_s:.3f}s") from exc

    def script(self, statements: list[str], timeout_s: float | None = None) -> None:
        for statement in statements:
            if statement.strip():
                self.execute(statement, timeout_s)


def sql_string(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def vector(i: int) -> str:
    a = (i % 97) / 97.0
    b = ((i * 7) % 89) / 89.0
    c = ((i * 13) % 83) / 83.0
    return f"[{a:.6f},{b:.6f},{c:.6f}]"


def words(i: int) -> str:
    base = ["hello", "world", "foo", "bar", "database", "query", "index", "graph"]
    return " ".join(base[(i + j) % len(base)] for j in range(16))


def sql_common_setup(rows: int, engine: str, include_graph_ddl: bool = True) -> list[str]:
    if engine == "aiondb":
        vector_type = "VECTOR(3)"
    elif engine == "cockroach":
        # Cockroach in this harness run does not rely on pgvector/HNSW tests.
        # Keep a placeholder column type compatible with general CRUD/scan/index SQL.
        vector_type = "TEXT"
    else:
        vector_type = "vector(3)"
    stmts = [
        "DROP TABLE IF EXISTS record CASCADE",
        "DROP TABLE IF EXISTS person CASCADE",
        "DROP TABLE IF EXISTS knows CASCADE",
        f"""
        CREATE TABLE record (
            id INT PRIMARY KEY,
            number INT NOT NULL,
            number2 INT NOT NULL,
            category TEXT NOT NULL,
            words TEXT NOT NULL,
            payload TEXT NOT NULL,
            tags TEXT,
            embedding {vector_type}
        )
        """,
        f"""
        CREATE TABLE person (
            id INT PRIMARY KEY,
            number INT NOT NULL,
            category TEXT NOT NULL,
            words TEXT NOT NULL,
            embedding {vector_type}
        )
        """,
        """
        CREATE TABLE knows (
            id INT PRIMARY KEY,
            source_id INT NOT NULL,
            target_id INT NOT NULL,
            weight INT NOT NULL,
            relation TEXT NOT NULL
        )
        """,
    ]
    if engine == "aiondb" and include_graph_ddl:
        stmts.extend(
            [
                "CREATE NODE LABEL Person ON person",
                "CREATE EDGE LABEL KNOWS ON knows SOURCE Person TARGET Person",
            ]
        )
    record_values = []
    person_values = []
    edge_values = []
    for i in range(1, rows + 1):
        category = f"c{i % 10}"
        w = words(i)
        payload = f"payload-{i}-" + ("x" * 96)
        tags = f"t{i % 5},t{(i + 1) % 5}"
        record_values.append(
            f"({i},{i % 100},{(i * 3) % 100},{sql_string(category)},"
            f"{sql_string(w)},{sql_string(payload)},{sql_string(tags)},'{vector(i)}')"
        )
        person_values.append(
            f"({i},{i % 100},{sql_string(category)},{sql_string(w)},'{vector(i)}')"
        )
        edge_values.append(
            f"({i},{i},{(i % rows) + 1},{i % 50},{sql_string('friend' if i % 2 else 'ref')})"
        )
        if i % 3 == 0:
            edge_values.append(
                f"({rows + i},{i},{((i + 7) % rows) + 1},{(i * 2) % 50},{sql_string('ref')})"
            )
    for chunk in chunks(record_values, 250):
        stmts.append("INSERT INTO record VALUES " + ",".join(chunk))
    for chunk in chunks(person_values, 250):
        stmts.append("INSERT INTO person VALUES " + ",".join(chunk))
    for chunk in chunks(edge_values, 250):
        stmts.append("INSERT INTO knows VALUES " + ",".join(chunk))
    return stmts


def pgstack_setup(rows: int) -> list[str]:
    return [
        "CREATE EXTENSION IF NOT EXISTS vector",
        "CREATE EXTENSION IF NOT EXISTS age",
        "LOAD 'age'",
        'SET search_path = ag_catalog, "$user", public',
    ] + sql_common_setup(rows, "pgstack") + [
        "SELECT create_graph('aionbench')",
        (
            "SELECT * FROM cypher('aionbench', $$ "
            "UNWIND range(1, %d) AS i "
            "CREATE (:Person {id: i, number: i %% 100, category: 'c' + toString(i %% 10)}) "
            "$$) AS (v agtype)"
        )
        % rows,
        (
            "SELECT * FROM cypher('aionbench', $$ "
            "UNWIND range(1, %d) AS i "
            "MATCH (a:Person {id: i}), (b:Person {id: CASE WHEN i = %d THEN 1 ELSE i + 1 END}) "
            "CREATE (a)-[:KNOWS {weight: i %% 50}]->(b) "
            "$$) AS (v agtype)"
        )
        % (rows, rows),
    ]


def surreal_setup(rows: int) -> list[str]:
    stmts = [
        "REMOVE TABLE record",
        "REMOVE TABLE person",
        "REMOVE TABLE knows",
        "DEFINE TABLE record SCHEMALESS",
        "DEFINE TABLE person SCHEMALESS",
        "DEFINE TABLE knows SCHEMALESS TYPE RELATION FROM person TO person",
    ]
    record_stmts = []
    person_stmts = []
    edge_stmts = []
    for i in range(1, rows + 1):
        category = f"c{i % 10}"
        record_stmts.append(
            f"CREATE record:{i} SET uid={i}, number={i % 100}, number2={(i * 3) % 100}, "
            f"category={sql_string(category)}, words={sql_string(words(i))}, "
            f"payload={sql_string('payload-' + str(i) + '-' + ('x' * 96))}, "
            f"tags=['t{i % 5}','t{(i + 1) % 5}'], embedding={vector(i)}"
        )
        person_stmts.append(
            f"CREATE person:{i} SET uid={i}, number={i % 100}, category={sql_string(category)}, "
            f"words={sql_string(words(i))}, embedding={vector(i)}"
        )
        edge_stmts.append(
            f"RELATE person:{i}->knows->person:{(i % rows) + 1} "
            f"SET weight={i % 50}, relation={sql_string('friend' if i % 2 else 'ref')}"
        )
        if i % 3 == 0:
            edge_stmts.append(
                f"RELATE person:{i}->knows->person:{((i + 7) % rows) + 1} "
                f"SET weight={(i * 2) % 50}, relation='ref'"
            )
    for chunk in chunks(record_stmts, 100):
        stmts.append(";".join(chunk) + ";")
    for chunk in chunks(person_stmts, 100):
        stmts.append(";".join(chunk) + ";")
    for chunk in chunks(edge_stmts, 100):
        stmts.append(";".join(chunk) + ";")
    return stmts


def chunks(values: list[str], size: int):
    for i in range(0, len(values), size):
        yield values[i : i + size]


def cypher_pg(query: str, columns: str = "v agtype") -> str:
    return f"SELECT * FROM cypher('aionbench', $${query}$$) AS ({columns})"


def sql_create(_: str, n: int) -> str:
    i = 1_000_000 + n
    return (
        "INSERT INTO record VALUES "
        f"({i},{i % 100},{(i * 3) % 100},'cx','hello world','payload','t1','{vector(i)}') "
        "ON CONFLICT (id) DO NOTHING"
    )


def aion_create(_: str, n: int) -> str:
    return sql_create(_, n)


def surreal_create(_: str, n: int) -> str:
    i = 1_000_000 + n
    return (
        f"UPSERT record:{i} SET number={i % 100}, number2={(i * 3) % 100}, "
        f"category='cx', words='hello world', payload='payload', tags=['t1'], embedding={vector(i)}"
    )


def sql_read(_: str, n: int) -> str:
    return f"SELECT * FROM record WHERE id = {(n % 2000) + 1}"


def surreal_read(_: str, n: int) -> str:
    return f"SELECT * FROM record:{(n % 2000) + 1}"


def sql_update(_: str, n: int) -> str:
    return f"UPDATE record SET number2 = number2 + 1 WHERE id = {(n % 2000) + 1}"


def surreal_update(_: str, n: int) -> str:
    return f"UPDATE record:{(n % 2000) + 1} SET number2 += 1"


def sql_scan(name: str, _: int) -> str:
    queries = {
        "count_all": "SELECT count(*) FROM record",
        "limit_id": "SELECT id FROM record LIMIT 100",
        "limit_all": "SELECT * FROM record LIMIT 100",
        "limit_start_id": "SELECT id FROM record OFFSET 1000 LIMIT 100",
        "limit_start_all": "SELECT * FROM record OFFSET 1000 LIMIT 100",
        "select_where_id": "SELECT id FROM record WHERE id = 42",
        "select_where_id_eq": "SELECT * FROM record WHERE id = 42",
        "select_where_gt": "SELECT * FROM record WHERE number > 50",
        "select_where_in": "SELECT * FROM record WHERE number IN (1,2,3,4,5)",
        "select_where_multi_and": "SELECT * FROM record WHERE number > 20 AND number2 < 80",
        "select_where_order_limit": "SELECT * FROM record WHERE number > 20 ORDER BY number LIMIT 100",
        "select_where_order_desc_limit": "SELECT * FROM record WHERE number > 20 ORDER BY number DESC LIMIT 100",
        "select_where_multi_order_limit": "SELECT * FROM record WHERE number > 20 AND number2 < 80 ORDER BY number, number2 LIMIT 100",
        "select_omit_limit": "SELECT id, number, category FROM record LIMIT 100",
        "select_fields_where_limit": "SELECT id, number FROM record WHERE category = 'c1' LIMIT 100",
        "select_order_by": "SELECT * FROM record ORDER BY number LIMIT 100",
        "select_order_by_multi": "SELECT * FROM record ORDER BY number, number2 LIMIT 100",
        "select_group_count": "SELECT category, count(*) FROM record GROUP BY category",
        "select_group_sum": "SELECT category, sum(number) FROM record GROUP BY category",
        "select_group_avg": "SELECT category, avg(number) FROM record GROUP BY category",
        "select_group_multi_agg": "SELECT category, count(*), sum(number), avg(number2) FROM record GROUP BY category",
        "select_group_all": "SELECT category, number, count(*) FROM record GROUP BY category, number",
        "select_group_order_limit": "SELECT category, count(*) AS c FROM record GROUP BY category ORDER BY c DESC LIMIT 5",
        "select_group_where": "SELECT category, count(*) FROM record WHERE number > 20 GROUP BY category",
        "select_group_dedup_agg": "SELECT category, count(DISTINCT number) FROM record GROUP BY category",
        "select_split": "SELECT id, unnest(string_to_array(tags, ',')) FROM record LIMIT 100",
        "select_fetch": "SELECT p.*, k.target_id FROM person p JOIN knows k ON k.source_id = p.id LIMIT 100",
        "select_fetch_where_limit": "SELECT p.*, k.target_id FROM person p JOIN knows k ON k.source_id = p.id WHERE p.number > 20 LIMIT 100",
        "subquery_inline": "SELECT * FROM record WHERE id IN (SELECT id FROM record WHERE number < 10)",
        "subquery_count": "SELECT count(*) FROM (SELECT id FROM record WHERE number < 10) s",
        "subquery_from": "SELECT s.category, count(*) FROM (SELECT * FROM record WHERE number < 90) s GROUP BY s.category",
        "pipeline_filter_group_order": "SELECT category, count(*) AS c FROM record WHERE number > 20 GROUP BY category ORDER BY c DESC LIMIT 5",
        "index_standard": "SELECT * FROM record WHERE number = 21",
        "index_composite": "SELECT * FROM record WHERE number = 21 AND category = 'c1'",
        "index_range_merged": "SELECT * FROM record WHERE number >= 18 AND number <= 21",
        "index_in": "SELECT * FROM record WHERE number IN (1,2,3,4,5)",
    }
    return queries[name]


def surreal_scan(name: str, _: int) -> str:
    queries = {
        "count_all": "SELECT count() FROM record GROUP ALL",
        "limit_id": "SELECT id FROM record LIMIT 100",
        "limit_all": "SELECT * FROM record LIMIT 100",
        "limit_start_id": "SELECT id FROM record START 1000 LIMIT 100",
        "limit_start_all": "SELECT * FROM record START 1000 LIMIT 100",
        "select_where_id": "SELECT id FROM record WHERE id = record:42",
        "select_where_id_eq": "SELECT * FROM record WHERE id = record:42",
        "select_where_gt": "SELECT * FROM record WHERE number > 50",
        "select_where_in": "SELECT * FROM record WHERE number IN [1,2,3,4,5]",
        "select_where_multi_and": "SELECT * FROM record WHERE number > 20 AND number2 < 80",
        "select_where_order_limit": "SELECT * FROM record WHERE number > 20 ORDER BY number LIMIT 100",
        "select_where_order_desc_limit": "SELECT * FROM record WHERE number > 20 ORDER BY number DESC LIMIT 100",
        "select_where_multi_order_limit": "SELECT * FROM record WHERE number > 20 AND number2 < 80 ORDER BY number, number2 LIMIT 100",
        "select_omit_limit": "SELECT * OMIT payload FROM record LIMIT 100",
        "select_fields_where_limit": "SELECT id, number FROM record WHERE category = 'c1' LIMIT 100",
        "select_order_by": "SELECT * FROM record ORDER BY number LIMIT 100",
        "select_order_by_multi": "SELECT * FROM record ORDER BY number, number2 LIMIT 100",
        "select_group_count": "SELECT category, count() FROM record GROUP BY category",
        "select_group_sum": "SELECT category, math::sum(number) FROM record GROUP BY category",
        "select_group_avg": "SELECT category, math::mean(number) FROM record GROUP BY category",
        "select_group_multi_agg": "SELECT category, count(), math::sum(number), math::mean(number2) FROM record GROUP BY category",
        "select_group_all": "SELECT category, number, count() FROM record GROUP BY category, number",
        "select_group_order_limit": "SELECT category, count() AS c FROM record GROUP BY category ORDER BY c DESC LIMIT 5",
        "select_group_where": "SELECT category, count() FROM record WHERE number > 20 GROUP BY category",
        "select_group_dedup_agg": "SELECT category, array::len(array::distinct(number)) FROM record GROUP BY category",
        "select_split": "SELECT tags FROM record SPLIT tags LIMIT 100",
        "select_fetch": "SELECT *, ->knows->person AS friends FROM person LIMIT 100",
        "select_fetch_where_limit": "SELECT *, ->knows->person AS friends FROM person WHERE number > 20 LIMIT 100",
        "subquery_inline": "SELECT * FROM record WHERE id IN (SELECT VALUE id FROM record WHERE number < 10)",
        "subquery_count": "SELECT count() FROM (SELECT id FROM record WHERE number < 10) GROUP ALL",
        "subquery_from": "SELECT category, count() FROM (SELECT * FROM record WHERE number < 90) GROUP BY category",
        "pipeline_filter_group_order": "SELECT category, count() AS c FROM record WHERE number > 20 GROUP BY category ORDER BY c DESC LIMIT 5",
        "index_standard": "SELECT * FROM record WHERE number = 21",
        "index_composite": "SELECT * FROM record WHERE number = 21 AND category = 'c1'",
        "index_range_merged": "SELECT * FROM record WHERE number >= 18 AND number <= 21",
        "index_in": "SELECT * FROM record WHERE number IN [1,2,3,4,5]",
    }
    return queries[name]


def prepare_index_sql(kind: str) -> Callable[[str], list[str]]:
    def inner(engine: str) -> list[str]:
        idx = f"idx_{kind}"
        field = {
            "standard": "number",
            "composite": "number, category",
            "range_merged": "number",
            "in": "number",
        }[kind]
        return [f"DROP INDEX IF EXISTS {idx}", f"CREATE INDEX {idx} ON record ({field})"]

    return inner


def prepare_index_surreal(kind: str) -> Callable[[], list[str]]:
    def inner() -> list[str]:
        idx = f"idx_{kind}"
        fields = {
            "standard": "number",
            "composite": "number, category",
            "range_merged": "number",
            "in": "number",
        }[kind]
        return [f"REMOVE INDEX {idx} ON TABLE record", f"DEFINE INDEX {idx} ON TABLE record FIELDS {fields}"]

    return inner


def build_index_sql(kind: str) -> SqlFn:
    def query(engine: str, n: int) -> str:
        idx = f"idx_build_{kind}"
        field = "number, category" if kind == "composite" else "number"
        return f"DROP INDEX IF EXISTS {idx}; CREATE INDEX {idx} ON record ({field})"

    return query


def build_index_surreal(kind: str) -> SqlFn:
    def query(_: str, n: int) -> str:
        idx = f"idx_build_{kind}"
        fields = "number, category" if kind == "composite" else "number"
        return f"REMOVE INDEX {idx} ON TABLE record; DEFINE INDEX {idx} ON TABLE record FIELDS {fields}"

    return query


def remove_index_sql(kind: str) -> SqlFn:
    def query(engine: str, n: int) -> str:
        idx = f"idx_remove_{kind}"
        field = "number, category" if kind == "composite" else "number"
        return f"CREATE INDEX IF NOT EXISTS {idx} ON record ({field}); DROP INDEX {idx}"

    return query


def remove_index_surreal(kind: str) -> SqlFn:
    def query(_: str, n: int) -> str:
        idx = f"idx_remove_{kind}"
        fields = "number, category" if kind == "composite" else "number"
        return f"DEFINE INDEX IF NOT EXISTS {idx} ON TABLE record FIELDS {fields}; REMOVE INDEX {idx} ON TABLE record"

    return query


def graph_aion(name: str, _: int) -> str:
    queries = {
        "graph_out_depth1": "MATCH (a:Person {id: 1})-[:KNOWS]->(b:Person) RETURN b.id",
        "graph_out_depth2": "MATCH (a:Person {id: 1})-[:KNOWS]->(b:Person)-[:KNOWS]->(c:Person) RETURN c.id",
        "graph_out_depth3": "MATCH (a:Person {id: 1})-[:KNOWS]->(b:Person)-[:KNOWS]->(c:Person)-[:KNOWS]->(d:Person) RETURN d.id",
        "graph_in_depth1": "MATCH (a:Person)-[:KNOWS]->(b:Person {id: 2}) RETURN a.id",
        "graph_bidirectional": "MATCH (a:Person)-[:KNOWS]-(b:Person {id: 2}) RETURN a.id",
        "graph_edge_filter": "MATCH (a:Person)-[e:KNOWS]->(b:Person) WHERE e.weight > 10 RETURN b.id LIMIT 100",
        "graph_multi_out": "MATCH (a:Person)-[:KNOWS]->(b:Person), (a)-[:KNOWS]->(c:Person) RETURN b.id, c.id LIMIT 100",
        "graph_multi_out_where": "MATCH (a:Person)-[:KNOWS]->(b:Person), (a)-[:KNOWS]->(c:Person) WHERE b.number > 20 RETURN b.id, c.id LIMIT 100",
        "graph_multi_count": "MATCH (a:Person)-[:KNOWS]->(b:Person) RETURN count(b)",
        "graph_depth2_limit": "MATCH (a:Person)-[:KNOWS]->(b:Person)-[:KNOWS]->(c:Person) RETURN c.id LIMIT 100",
        "graph_sub_where": "MATCH (a:Person)-[:KNOWS]->(b:Person) WHERE b.number > 20 RETURN b.id LIMIT 100",
        "graph_sub_group_all": "MATCH (a:Person)-[:KNOWS]->(b:Person) RETURN b.category, count(b)",
        "graph_sub_group_by": "MATCH (a:Person)-[:KNOWS]->(b:Person) RETURN b.category, count(b)",
    }
    return queries[name]


def graph_pg(name: str, n: int) -> str:
    return cypher_pg(graph_aion(name, n), "v agtype")


def graph_surreal(name: str, _: int) -> str:
    queries = {
        "graph_out_depth1": "SELECT ->knows->person.id FROM person:1",
        "graph_out_depth2": "SELECT ->knows->person->knows->person.id FROM person:1",
        "graph_out_depth3": "SELECT ->knows->person->knows->person->knows->person.id FROM person:1",
        "graph_in_depth1": "SELECT <-knows<-person.id FROM person:2",
        "graph_bidirectional": "SELECT <-knows<-person.id, ->knows->person.id FROM person:2",
        "graph_edge_filter": "SELECT ->knows[WHERE weight > 10]->person.id FROM person LIMIT 100",
        "graph_multi_out": "SELECT ->knows->person.id AS a, ->knows->person.id AS b FROM person LIMIT 100",
        "graph_multi_out_where": "SELECT ->knows->person[WHERE number > 20].id FROM person LIMIT 100",
        "graph_multi_count": "SELECT count(->knows->person) FROM person GROUP ALL",
        "graph_depth2_limit": "SELECT ->knows->person->knows->person.id FROM person:1 LIMIT 100",
        "graph_sub_where": "SELECT ->knows->person[WHERE number > 20].id FROM person:1",
        "graph_sub_group_all": "SELECT category, count() FROM person GROUP BY category",
        "graph_sub_group_by": "SELECT category, count() FROM person GROUP BY category",
    }
    return queries[name]


def hybrid_graph_property_aion(_: str, __: int) -> str:
    return "MATCH (a:Person {id: 1})-[:KNOWS]->(b:Person) WHERE b.number > 20 RETURN b.id LIMIT 100"


def hybrid_graph_property_pg(_: str, __: int) -> str:
    return cypher_pg(
        "MATCH (a:Person {id: 1})-[:KNOWS]->(b:Person) "
        "WHERE b.number > 20 RETURN b.id LIMIT 100",
        "v agtype",
    )


def hybrid_graph_property_surreal(_: str, __: int) -> str:
    return "SELECT ->knows->person[WHERE number > 20].id FROM person:1"


def complex_vector_join_sql(engine: str, _: int) -> str:
    distance = (
        "l2_distance(r.embedding, '[1.0,0.0,0.0]')"
        if engine == "aiondb"
        else "r.embedding <-> '[1.0,0.0,0.0]'"
    )
    return (
        "SELECT r.id, p.category, q.category, k.weight, "
        f"{distance} AS distance "
        "FROM record r "
        "JOIN person p ON p.id = r.id "
        "JOIN knows k ON k.source_id = p.id "
        "JOIN person q ON q.id = k.target_id "
        "WHERE p.number BETWEEN 10 AND 80 "
        "AND k.weight BETWEEN 5 AND 35 "
        "AND (q.category = 'c1' OR q.category = 'c2' OR q.category = 'c3') "
        f"ORDER BY {distance}, k.weight DESC LIMIT 50"
    )


def complex_vector_join_surreal(_: str, __: int) -> str:
    return (
        "SELECT uid, category, vector::distance::knn() AS distance "
        "FROM record "
        "WHERE embedding <|50,200|> [1.0,0.0,0.0] "
        "AND uid IN ("
        "SELECT VALUE uid FROM person "
        "WHERE number >= 10 AND number <= 80 "
        "AND array::len(->knows[WHERE weight >= 5 AND weight <= 35]->person"
        "[WHERE category IN ['c1','c2','c3']]) > 0"
        ") "
        "ORDER BY distance LIMIT 50"
    )


def complex_graph_two_hop_aion(_: str, __: int) -> str:
    return (
        "MATCH (a:Person)-[e1:KNOWS]->(b:Person)-[e2:KNOWS]->(c:Person) "
        "WHERE a.number > 10 AND a.number < 80 "
        "AND e1.weight > 5 "
        "AND (b.category = 'c1' OR b.category = 'c2' OR b.category = 'c3') "
        "RETURN b.category, count(c) LIMIT 10"
    )


def complex_graph_two_hop_pg(_: str, __: int) -> str:
    return cypher_pg(
        "MATCH (a:Person)-[e1:KNOWS]->(b:Person)-[e2:KNOWS]->(c:Person) "
        "WHERE a.number > 10 AND a.number < 80 "
        "AND e1.weight > 5 "
        "AND (b.category = 'c1' OR b.category = 'c2' OR b.category = 'c3') "
        "RETURN b.category, count(c) LIMIT 10",
        "category agtype, c agtype",
    )


def complex_graph_two_hop_surreal(_: str, __: int) -> str:
    return (
        "SELECT category, count() AS c FROM person "
        "WHERE number > 10 AND number < 80 "
        "AND array::len(->knows[WHERE weight > 5]->person"
        "[WHERE category IN ['c1','c2','c3']]->knows->person) > 0 "
        "GROUP BY category ORDER BY c DESC LIMIT 10"
    )


def complex_relational_fanout_sql(_: str, __: int) -> str:
    return (
        "SELECT p.category, q.category AS target_category, "
        "count(*) AS edges, avg(k.weight) AS avg_weight, "
        "count(DISTINCT r.number) AS distinct_numbers "
        "FROM person p "
        "JOIN record r ON r.id = p.id "
        "JOIN knows k ON k.source_id = p.id "
        "JOIN person q ON q.id = k.target_id "
        "JOIN record rq ON rq.id = q.id "
        "WHERE r.number BETWEEN 10 AND 90 "
        "AND rq.number2 BETWEEN 15 AND 85 "
        "GROUP BY p.category, q.category "
        "ORDER BY edges DESC, avg_weight DESC LIMIT 25"
    )


def complex_relational_fanout_surreal(_: str, __: int) -> str:
    return (
        "SELECT category, count() AS c, math::mean(number) AS avg_number "
        "FROM person "
        "WHERE number >= 10 AND number <= 90 "
        "AND array::len(->knows->person[WHERE number >= 5 AND number <= 95]) > 0 "
        "GROUP BY category ORDER BY c DESC LIMIT 25"
    )


def fulltext_sql(engine: str, _: int) -> str:
    if engine == "aiondb":
        return (
            "SELECT * FROM full_text_top_k_hits("
            "'public.record','words','hello',1000,'plain','english',0.0,'{}'::jsonb)"
        )
    return "SELECT id FROM record WHERE to_tsvector('english', words) @@ to_tsquery('english', 'hello') LIMIT 1000"


def fulltext_surreal(_: str, __: int) -> str:
    return "SELECT id FROM record WHERE words @@ 'hello' LIMIT 1000"


def prepare_fulltext_sql(engine: str) -> list[str]:
    if engine == "aiondb":
        return ["DROP INDEX IF EXISTS idx_fulltext", "CREATE INDEX idx_fulltext ON record USING gin (words)"]
    return [
        "DROP INDEX IF EXISTS idx_fulltext",
        "CREATE INDEX idx_fulltext ON record USING GIN (to_tsvector('english', words))",
    ]


def prepare_fulltext_surreal() -> list[str]:
    return [
        "REMOVE INDEX idx_fulltext ON TABLE record",
        "DEFINE ANALYZER bench_analyzer TOKENIZERS blank FILTERS lowercase,snowball(english)",
        "DEFINE INDEX idx_fulltext ON TABLE record FIELDS words SEARCH ANALYZER bench_analyzer BM25",
    ]


def vector_sql(engine: str, _: int) -> str:
    if engine == "aiondb":
        return "SELECT id FROM record ORDER BY l2_distance(embedding, '[1.0,0.0,0.0]') LIMIT 10"
    return "SELECT id FROM record ORDER BY embedding <-> '[1.0,0.0,0.0]' LIMIT 10"


def vector_surreal(_: str, __: int) -> str:
    return (
        "SELECT id, vector::distance::knn() AS dist "
        "FROM record WHERE embedding <|10,100|> [1.0,0.0,0.0] ORDER BY dist"
    )


def hybrid_vector_category_sql(engine: str, _: int) -> str:
    if engine == "aiondb":
        return (
            "SELECT id FROM record WHERE category = 'c1' "
            "ORDER BY l2_distance(embedding, '[1.0,0.0,0.0]') LIMIT 10"
        )
    return (
        "SELECT id FROM record WHERE category = 'c1' "
        "ORDER BY embedding <-> '[1.0,0.0,0.0]' LIMIT 10"
    )


def hybrid_vector_numeric_sql(engine: str, _: int) -> str:
    if engine == "aiondb":
        return (
            "SELECT id FROM record WHERE number BETWEEN 10 AND 40 "
            "ORDER BY l2_distance(embedding, '[1.0,0.0,0.0]') LIMIT 10"
        )
    return (
        "SELECT id FROM record WHERE number BETWEEN 10 AND 40 "
        "ORDER BY embedding <-> '[1.0,0.0,0.0]' LIMIT 10"
    )


def hybrid_vector_category_surreal(_: str, __: int) -> str:
    return (
        "SELECT id, vector::distance::knn() AS dist FROM record "
        "WHERE category = 'c1' AND embedding <|10,100|> [1.0,0.0,0.0] ORDER BY dist"
    )


def hybrid_vector_numeric_surreal(_: str, __: int) -> str:
    return (
        "SELECT id, vector::distance::knn() AS dist FROM record "
        "WHERE number >= 10 AND number <= 40 AND embedding <|10,100|> [1.0,0.0,0.0] ORDER BY dist"
    )


def prepare_hnsw_sql(engine: str) -> list[str]:
    if engine == "aiondb":
        return ["DROP INDEX IF EXISTS idx_hnsw", "CREATE INDEX idx_hnsw ON record USING hnsw (embedding)"]
    return [
        "DROP INDEX IF EXISTS idx_hnsw",
        "CREATE INDEX idx_hnsw ON record USING hnsw (embedding vector_l2_ops)",
    ]


def prepare_hnsw_surreal() -> list[str]:
    return [
        "REMOVE INDEX idx_hnsw ON TABLE record",
        "DEFINE INDEX idx_hnsw ON TABLE record FIELDS embedding HNSW DIMENSION 3 DIST EUCLIDEAN",
    ]


def build_hnsw_sql(engine: str, _: int) -> str:
    return "; ".join(prepare_hnsw_sql(engine))


def build_hnsw_surreal(_: str, __: int) -> str:
    return "; ".join(prepare_hnsw_surreal())


def remove_hnsw_sql(engine: str, _: int) -> str:
    return "; ".join(prepare_hnsw_sql(engine) + ["DROP INDEX idx_hnsw"])


def remove_hnsw_surreal(_: str, __: int) -> str:
    return "; ".join(prepare_hnsw_surreal() + ["REMOVE INDEX idx_hnsw ON TABLE record"])


def tests() -> list[TestSpec]:
    specs = [
        TestSpec("[C]reate", "crud", sql=aion_create, pgstack=sql_create, surrealdb=surreal_create),
        TestSpec("[R]ead", "crud", sql=sql_read, surrealdb=surreal_read),
        TestSpec("[U]pdate", "crud", sql=sql_update, surrealdb=surreal_update),
    ]
    scan_names = [
        "count_all",
        "limit_id",
        "limit_all",
        "limit_start_id",
        "limit_start_all",
        "select_where_id",
        "select_where_id_eq",
        "select_where_gt",
        "select_where_in",
        "select_where_multi_and",
        "select_where_order_limit",
        "select_where_order_desc_limit",
        "select_where_multi_order_limit",
        "select_omit_limit",
        "select_fields_where_limit",
        "select_order_by",
        "select_order_by_multi",
        "select_group_count",
        "select_group_sum",
        "select_group_avg",
        "select_group_multi_agg",
        "select_group_all",
        "select_group_order_limit",
        "select_group_where",
        "select_group_dedup_agg",
        "select_split",
        "select_fetch",
        "select_fetch_where_limit",
        "subquery_inline",
        "subquery_count",
        "subquery_from",
        "pipeline_filter_group_order",
    ]
    specs.extend(
        TestSpec(f"[S]can::{name} (2000)", "scan", sql=lambda e, n, name=name: sql_scan(name, n), surrealdb=lambda e, n, name=name: surreal_scan(name, n))
        for name in scan_names
    )
    graph_names = [
        "graph_out_depth1",
        "graph_out_depth2",
        "graph_out_depth3",
        "graph_in_depth1",
        "graph_bidirectional",
        "graph_edge_filter",
        "graph_multi_out",
        "graph_multi_out_where",
        "graph_multi_count",
        "graph_depth2_limit",
        "graph_sub_where",
        "graph_sub_group_all",
        "graph_sub_group_by",
    ]
    specs.extend(
        TestSpec(
            f"[S]can::{name} (2000)",
            "graph",
            aiondb=lambda e, n, name=name: graph_aion(name, n),
            pgstack=lambda e, n, name=name: graph_pg(name, n),
            surrealdb=lambda e, n, name=name: graph_surreal(name, n),
        )
        for name in graph_names
    )
    for kind in ["standard", "composite", "range_merged", "in"]:
        specs.extend(
            [
                TestSpec(
                    f"[S]can::index_{kind} (2000)",
                    "index",
                    sql=lambda e, n, kind=kind: sql_scan(f"index_{kind}", n),
                    surrealdb=lambda e, n, kind=kind: surreal_scan(f"index_{kind}", n),
                ),
                TestSpec(
                    f"[I]ndex::index_{kind}",
                    "index",
                    sql=build_index_sql(kind),
                    surrealdb=build_index_surreal(kind),
                ),
                TestSpec(
                    f"[S]can::index_{kind}::indexed (2000)",
                    "index",
                    sql=lambda e, n, kind=kind: sql_scan(f"index_{kind}", n),
                    surrealdb=lambda e, n, kind=kind: surreal_scan(f"index_{kind}", n),
                    prepare_sql=prepare_index_sql(kind),
                    prepare_surrealdb=prepare_index_surreal(kind),
                ),
                TestSpec(
                    f"[R]emoveIndex::index_{kind}",
                    "index",
                    sql=remove_index_sql(kind),
                    surrealdb=remove_index_surreal(kind),
                ),
            ]
        )
    specs.extend(
        [
            TestSpec(
                "[I]ndex::index_fulltext",
                "fulltext",
                sql=lambda e, n: "; ".join(prepare_fulltext_sql(e)),
                surrealdb=lambda e, n: "; ".join(prepare_fulltext_surreal()),
            ),
            TestSpec(
                "[S]can::index_fulltext::indexed (2000)",
                "fulltext",
                sql=fulltext_sql,
                surrealdb=fulltext_surreal,
                prepare_sql=prepare_fulltext_sql,
                prepare_surrealdb=prepare_fulltext_surreal,
            ),
            TestSpec(
                "[R]emoveIndex::index_fulltext",
                "fulltext",
                sql=lambda e, n: "; ".join(prepare_fulltext_sql(e) + ["DROP INDEX idx_fulltext"]),
                surrealdb=lambda e, n: "; ".join(prepare_fulltext_surreal() + ["REMOVE INDEX idx_fulltext ON TABLE record"]),
            ),
            TestSpec("[I]ndex::index_hnsw", "vector", sql=build_hnsw_sql, surrealdb=build_hnsw_surreal),
            TestSpec(
                "[S]can::index_hnsw::indexed (2000)",
                "vector",
                sql=vector_sql,
                surrealdb=vector_surreal,
                prepare_sql=prepare_hnsw_sql,
                prepare_surrealdb=prepare_hnsw_surreal,
            ),
            TestSpec(
                "[S]can::hybrid_vector_category::indexed (2000)",
                "hybrid",
                sql=hybrid_vector_category_sql,
                surrealdb=hybrid_vector_category_surreal,
                prepare_sql=prepare_hnsw_sql,
                prepare_surrealdb=prepare_hnsw_surreal,
            ),
            TestSpec(
                "[S]can::hybrid_vector_numeric::indexed (2000)",
                "hybrid",
                sql=hybrid_vector_numeric_sql,
                surrealdb=hybrid_vector_numeric_surreal,
                prepare_sql=prepare_hnsw_sql,
                prepare_surrealdb=prepare_hnsw_surreal,
            ),
            TestSpec(
                "[S]can::hybrid_graph_property_filter (2000)",
                "hybrid",
                aiondb=hybrid_graph_property_aion,
                pgstack=hybrid_graph_property_pg,
                surrealdb=hybrid_graph_property_surreal,
            ),
            TestSpec(
                "[Complex]::vector_join_graph_filter_rank (5000)",
                "complex",
                sql=complex_vector_join_sql,
                surrealdb=complex_vector_join_surreal,
                prepare_sql=prepare_hnsw_sql,
                prepare_surrealdb=prepare_hnsw_surreal,
            ),
            TestSpec(
                "[Complex]::graph_two_hop_filter_aggregate (5000)",
                "complex",
                aiondb=complex_graph_two_hop_aion,
                pgstack=complex_graph_two_hop_pg,
                surrealdb=complex_graph_two_hop_surreal,
            ),
            TestSpec(
                "[Complex]::relational_fanout_join_aggregate (5000)",
                "complex",
                sql=complex_relational_fanout_sql,
                surrealdb=complex_relational_fanout_surreal,
            ),
            TestSpec("[R]emoveIndex::index_hnsw", "vector", sql=remove_hnsw_sql, surrealdb=remove_hnsw_surreal),
        ]
    )
    return specs


def select_query(spec: TestSpec, engine_name: str, op: int) -> str:
    fn = {
        "aiondb": spec.aiondb or spec.sql,
        "cockroach": spec.sql,
        "pgstack": spec.pgstack or spec.sql,
        "surrealdb": spec.surrealdb,
    }.get(engine_name)
    if fn is None:
        raise EngineError(f"no query for {engine_name}")
    return fn(engine_name, op)


def prepare_test(spec: TestSpec, engine_name: str, engine) -> None:
    if engine_name == "surrealdb" and spec.prepare_surrealdb is not None:
        engine.script(spec.prepare_surrealdb())
    elif engine_name in ("aiondb", "pgstack") and spec.prepare_sql is not None:
        engine.script(spec.prepare_sql(engine_name))


def run_phase(
    engine_name: str,
    engine,
    spec: TestSpec,
    seconds: float,
    trace: Path,
    operation_timeout_s: float | None,
) -> dict:
    count = 0
    durations_ms: list[float] = []
    deadline = time.perf_counter() + seconds
    error = ""
    status = "OK"
    with trace.open("w", encoding="utf-8") as log:
        log.write(
            f"engine={engine_name}\ntest={spec.name}\nseconds={seconds}\n"
            f"operation_timeout_seconds={operation_timeout_s}\n"
        )
        try:
            while time.perf_counter() < deadline:
                query = select_query(spec, engine_name, count)
                start = time.perf_counter()
                engine.execute(query, operation_timeout_s)
                elapsed_ms = (time.perf_counter() - start) * 1000.0
                durations_ms.append(elapsed_ms)
                count += 1
        except OperationTimeout as exc:
            status = "TIMEOUT" if count == 0 else "FAIL"
            error = str(exc)
            log.write("\nTIMEOUT:\n")
            log.write(error)
            log.write("\n")
        except Exception as exc:  # noqa: BLE001 - benchmark must preserve engine error text
            status = "UNSUPPORTED" if count == 0 else "FAIL"
            error = str(exc)
            log.write("\nERROR:\n")
            log.write(error)
            log.write("\n")
    elapsed = max(seconds, sum(durations_ms) / 1000.0)
    mean_ms = statistics.fmean(durations_ms) if durations_ms else math.nan
    p95_ms = percentile(durations_ms, 0.95)
    p99_ms = percentile(durations_ms, 0.99)
    ops = count / elapsed if elapsed > 0 else 0.0
    return {
        "engine": engine_name,
        "test": spec.name,
        "category": spec.category,
        "status": status,
        "ops": ops,
        "mean_ms": mean_ms,
        "p95_ms": p95_ms,
        "p99_ms": p99_ms,
        "count": count,
        "error": error,
    }


def percentile(values: list[float], q: float) -> float:
    if not values:
        return math.nan
    values = sorted(values)
    idx = min(len(values) - 1, max(0, int(round((len(values) - 1) * q))))
    return values[idx]


def write_csv(path: Path, rows: list[dict]) -> None:
    fields = [
        "phase",
        "iteration",
        "engine",
        "test",
        "category",
        "status",
        "ops",
        "mean_ms",
        "p95_ms",
        "p99_ms",
        "count",
        "error",
    ]
    with path.open("w", newline="", encoding="utf-8") as handle:
        writer = csv.DictWriter(handle, fieldnames=fields)
        writer.writeheader()
        writer.writerows(rows)


def write_summary(run_dir: Path, measured: list[dict]) -> None:
    grouped: dict[tuple[str, str], list[dict]] = {}
    for row in measured:
        grouped.setdefault((row["test"], row["engine"]), []).append(row)
    summary = []
    for (test, engine), rows in sorted(grouped.items()):
        ok = [r for r in rows if r["status"] == "OK"]
        status = "OK" if len(ok) == len(rows) else rows[-1]["status"]
        summary.append(
            {
                "test": test,
                "engine": engine,
                "iterations": len(rows),
                "ok_iterations": len(ok),
                "status": status,
                "ops_avg": statistics.fmean([r["ops"] for r in ok]) if ok else math.nan,
                "mean_ms_avg": statistics.fmean([r["mean_ms"] for r in ok]) if ok else math.nan,
                "p95_ms_avg": statistics.fmean([r["p95_ms"] for r in ok]) if ok else math.nan,
                "p99_ms_avg": statistics.fmean([r["p99_ms"] for r in ok]) if ok else math.nan,
            }
        )
    with (run_dir / "summary.tsv").open("w", newline="", encoding="utf-8") as handle:
        fields = [
            "test",
            "engine",
            "iterations",
            "ok_iterations",
            "status",
            "ops_avg",
            "mean_ms_avg",
            "p95_ms_avg",
            "p99_ms_avg",
        ]
        writer = csv.DictWriter(handle, fieldnames=fields, delimiter="\t")
        writer.writeheader()
        writer.writerows(summary)
    with (run_dir / "summary.md").open("w", encoding="utf-8") as handle:
        handle.write("# Surreal Suite Summary\n\n")
        handle.write("| Test | Engine | OK/Iterations | Status | OPS avg | Mean ms avg | p95 ms avg | p99 ms avg |\n")
        handle.write("| --- | --- | ---: | --- | ---: | ---: | ---: | ---: |\n")
        for row in summary:
            handle.write(
                f"| {row['test']} | {row['engine']} | {row['ok_iterations']}/{row['iterations']} "
                f"| {row['status']} | {fmt(row['ops_avg'])} | {fmt(row['mean_ms_avg'])} "
                f"| {fmt(row['p95_ms_avg'])} | {fmt(row['p99_ms_avg'])} |\n"
            )


def fmt(value: float) -> str:
    return "-" if math.isnan(value) else f"{value:.3f}"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--run-id", required=True)
    parser.add_argument("--run-dir", required=True)
    parser.add_argument("--engines", required=True)
    parser.add_argument("--rows", type=int, required=True)
    parser.add_argument("--warmup-seconds", type=float, required=True)
    parser.add_argument("--iterations", type=int, required=True)
    parser.add_argument("--duration-seconds", type=float, required=True)
    parser.add_argument("--operation-timeout-seconds", type=float, default=20.0)
    parser.add_argument("--tests", default="all")
    parser.add_argument("--surreal-url", required=True)
    parser.add_argument("--surreal-user", required=True)
    parser.add_argument("--surreal-pass", required=True)
    parser.add_argument("--surreal-ns", required=True)
    parser.add_argument("--surreal-db", required=True)
    parser.add_argument("--aiondb-dsn", required=True)
    parser.add_argument("--cockroach-dsn", default="")
    parser.add_argument("--pgstack-dsn", required=True)
    args = parser.parse_args()

    run_dir = Path(args.run_dir)
    trace_dir = run_dir / "traces"
    trace_dir.mkdir(parents=True, exist_ok=True)

    selected_engines = args.engines.split()
    all_specs = tests()
    if args.tests != "all":
        wanted = set(args.tests.split(","))
        all_specs = [
            s
            for s in all_specs
            if s.name in wanted
            or s.category in wanted
            or safe_name(s.name) in wanted
            or s.name.split("::")[-1].split()[0] in wanted
        ]

    engines = {}
    try:
        if "surrealdb" in selected_engines:
            try:
                engines["surrealdb"] = SurrealEngine(
                    args.surreal_url,
                    args.surreal_user,
                    args.surreal_pass,
                    args.surreal_ns,
                    args.surreal_db,
                )
                engines["surrealdb"].script(surreal_setup(args.rows))
            except Exception as exc:  # noqa: BLE001
                engines["surrealdb"] = DisabledEngine("surrealdb", f"surrealdb setup failed: {exc}")
        graph_selected = any(spec.category == "graph" or "graph_" in spec.name for spec in all_specs)
        if "aiondb" in selected_engines:
            try:
                engines["aiondb"] = PgEngine("aiondb", args.aiondb_dsn)
                engines["aiondb"].script(
                    sql_common_setup(args.rows, "aiondb", include_graph_ddl=graph_selected),
                    args.operation_timeout_seconds,
                )
            except Exception as exc:  # noqa: BLE001
                engines["aiondb"] = DisabledEngine("aiondb", f"aiondb setup failed: {exc}")
        if "cockroach" in selected_engines:
            try:
                engines["cockroach"] = PgEngine("cockroach", args.cockroach_dsn)
                engines["cockroach"].script(
                    sql_common_setup(args.rows, "cockroach", include_graph_ddl=False),
                    args.operation_timeout_seconds,
                )
            except Exception as exc:  # noqa: BLE001
                engines["cockroach"] = DisabledEngine("cockroach", f"cockroach setup failed: {exc}")
        if "pgstack" in selected_engines:
            try:
                engines["pgstack"] = PgEngine("pgstack", args.pgstack_dsn)
                engines["pgstack"].script(pgstack_setup(args.rows), args.operation_timeout_seconds)
            except Exception as exc:  # noqa: BLE001
                engines["pgstack"] = DisabledEngine("pgstack", f"pgstack setup failed: {exc}")

        metadata = {
            "run_id": args.run_id,
            "rows": args.rows,
            "warmup_seconds": args.warmup_seconds,
            "iterations": args.iterations,
            "duration_seconds": args.duration_seconds,
            "operation_timeout_seconds": args.operation_timeout_seconds,
            "engines": selected_engines,
            "tests": [s.name for s in all_specs],
            "protocols": {
                "surrealdb": "WebSocket JSON-RPC",
                "aiondb": "PostgreSQL wire",
                "cockroach": "PostgreSQL wire",
                "pgstack": "PostgreSQL wire + pgvector + Apache AGE/Cypher",
            },
            "python": platform.python_version(),
            "platform": platform.platform(),
            "repo_commit": os.popen("git rev-parse HEAD 2>/dev/null").read().strip(),
        }
        (run_dir / "metadata.json").write_text(json.dumps(metadata, indent=2), encoding="utf-8")

        rows = []
        for spec in all_specs:
            for engine_name in selected_engines:
                engine = engines[engine_name]
                try:
                    prepare_test(spec, engine_name, engine)
                except Exception as exc:  # noqa: BLE001
                    trace = trace_dir / f"prepare-{engine_name}-{safe_name(spec.name)}.log"
                    trace.write_text(str(exc), encoding="utf-8")
                warm = run_phase(
                    engine_name,
                    engine,
                    spec,
                    args.warmup_seconds,
                    trace_dir / f"warmup-{engine_name}-{safe_name(spec.name)}.log",
                    args.operation_timeout_seconds,
                )
                warm.update({"phase": "warmup", "iteration": 0})
                rows.append(warm)
                write_csv(run_dir / "raw_results.csv", rows)
            for iteration in range(1, args.iterations + 1):
                for engine_name in selected_engines:
                    engine = engines[engine_name]
                    try:
                        prepare_test(spec, engine_name, engine)
                    except Exception as exc:  # noqa: BLE001
                        trace = trace_dir / f"prepare-{iteration}-{engine_name}-{safe_name(spec.name)}.log"
                        trace.write_text(str(exc), encoding="utf-8")
                    measured = run_phase(
                        engine_name,
                        engine,
                        spec,
                        args.duration_seconds,
                        trace_dir / f"iter{iteration}-{engine_name}-{safe_name(spec.name)}.log",
                        args.operation_timeout_seconds,
                    )
                    measured.update({"phase": "measure", "iteration": iteration})
                    rows.append(measured)
                    write_csv(run_dir / "raw_results.csv", rows)
                    write_summary(run_dir, [r for r in rows if r["phase"] == "measure"])
                    print(
                        f"{spec.name}\t{engine_name}\titer={iteration}\t"
                        f"{measured['status']}\tops={fmt(measured['ops'])}\tmean_ms={fmt(measured['mean_ms'])}",
                        flush=True,
                    )
        write_csv(run_dir / "raw_results.csv", rows)
        write_summary(run_dir, [r for r in rows if r["phase"] == "measure"])
    finally:
        for engine in engines.values():
            engine.close()
    return 0


def safe_name(name: str) -> str:
    return "".join(ch if ch.isalnum() else "_" for ch in name).strip("_")[:120]


if __name__ == "__main__":
    raise SystemExit(main())
