#!/usr/bin/env python3
import base64
import json
import os
import urllib.error
import urllib.request


def request_json(method: str, url: str, auth: str | None = None, payload: dict | None = None) -> tuple[int, dict]:
    headers = {"content-type": "application/json"} if payload is not None else {}
    if auth is not None:
        headers["authorization"] = auth
    body = None if payload is None else json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(url, data=body, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=5.0) as response:
            return response.status, json.loads(response.read().decode("utf-8"))
    except urllib.error.HTTPError as exc:
        return exc.code, json.loads(exc.read().decode("utf-8"))


def main() -> int:
    base_url = os.environ["AIONDB_QUERY_API_BASE"]
    user = os.environ["AIONDB_QUERY_API_USER"]
    password = os.environ["AIONDB_QUERY_API_PASSWORD"]
    auth = "Basic " + base64.b64encode(f"{user}:{password}".encode("utf-8")).decode("ascii")

    status, discovery = request_json("GET", f"{base_url}/", None, None)
    if status != 200 or discovery.get("compatibility") != "neo4j-query-api-subset":
        raise RuntimeError(f"unexpected discovery payload: status={status}, body={discovery!r}")

    status, query = request_json(
        "POST",
        f"{base_url}/db/default/query/v2",
        auth,
        {"statement": "SELECT 1 AS one"},
    )
    if status != 200 or query["data"]["fields"] != ["one"] or query["data"]["values"] != [[1]]:
        raise RuntimeError(f"unexpected query payload: status={status}, body={query!r}")

    status, params = request_json(
        "POST",
        f"{base_url}/db/default/query/v2",
        auth,
        {
            "statement": "SELECT $value::INT AS one",
            "parameters": {"value": 1},
        },
    )
    if status != 200 or params["data"]["values"] != [[1]]:
        raise RuntimeError(f"unexpected parameter payload: status={status}, body={params!r}")

    status, cypher_error = request_json(
        "POST",
        f"{base_url}/db/default/query/v2",
        auth,
        {"statement": "SELEC 1"},
    )
    if (
        status != 400
        or not isinstance(cypher_error, dict)
        or cypher_error.get("code") != "42601"
        or "error" not in cypher_error
    ):
        raise RuntimeError(
            f"unexpected cypher error payload: status={status}, body={cypher_error!r}"
        )

    status, tx_commit = request_json(
        "POST",
        f"{base_url}/db/default/tx/commit",
        auth,
        {
            "statements": [
                {"statement": "CREATE TABLE qapi_commit (id INT)"},
                {
                    "statement": "INSERT INTO qapi_commit VALUES ($id)",
                    "parameters": {"id": 7},
                },
                {"statement": "SELECT id FROM qapi_commit"},
            ]
        },
    )
    if status != 200 or tx_commit["results"][2]["values"] != [[7]]:
        raise RuntimeError(f"unexpected tx commit payload: status={status}, body={tx_commit!r}")

    status, tx_begin = request_json(
        "POST",
        f"{base_url}/db/default/tx",
        auth,
        {"statements": [{"statement": "CREATE TABLE qapi_lifecycle (id INT)"}]},
    )
    tx_id = tx_begin.get("txId")
    if status != 200 or not tx_id:
        raise RuntimeError(f"unexpected tx begin payload: status={status}, body={tx_begin!r}")

    status, tx_continue = request_json(
        "POST",
        f"{base_url}/db/default/tx/{tx_id}",
        auth,
        {
            "statements": [
                {
                    "statement": "INSERT INTO qapi_lifecycle VALUES ($id)",
                    "parameters": {"id": 9},
                }
            ]
        },
    )
    if status != 200:
        raise RuntimeError(f"unexpected tx continue payload: status={status}, body={tx_continue!r}")

    status, tx_id_commit = request_json(
        "POST",
        f"{base_url}/db/default/tx/{tx_id}/commit",
        auth,
        {"statements": [{"statement": "SELECT id FROM qapi_lifecycle"}]},
    )
    if status != 200 or tx_id_commit["summary"]["committed"] is not True:
        raise RuntimeError(f"unexpected tx id commit payload: status={status}, body={tx_id_commit!r}")

    print(
        json.dumps(
            {
                "details": "Neo4j Query API wrapper completed discovery, query, parameter, error and transaction probes",
                "checks": [
                    "discovery",
                    "basic_auth",
                    "query_v2",
                    "named_parameters",
                    "cypher_error",
                    "tx_commit",
                    "tx_lifecycle",
                ],
                "capabilities": {
                    "http_transport": True,
                    "single_statement_query_v2": True,
                    "explicit_transactions": True,
                    "error_payloads": True,
                    "read_write_smoke": True,
                    "database_scope": "default",
                    "auth_mode": "basic",
                },
                "discovery": {
                    "name": discovery.get("name"),
                    "compatibility": discovery.get("compatibility"),
                    "auth": discovery.get("auth"),
                    "notes": discovery.get("notes"),
                },
                "base_url": base_url,
            }
        )
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as exc:  # pragma: no cover - smoke script
        print(json.dumps({"error": str(exc)}))
        raise
