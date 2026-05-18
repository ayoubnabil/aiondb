#!/usr/bin/env python3
import json
import os

from neo4j import GraphDatabase


def main() -> int:
    uri = os.environ["NEO4J_URI"]
    user = os.environ["NEO4J_USER"]
    password = os.environ["NEO4J_PASSWORD"]

    driver = GraphDatabase.driver(uri, auth=(user, password), connection_timeout=5.0)
    try:
        with driver.session() as session:
            result = session.run("RETURN 1 AS one, 'ok' AS status")
            if result.keys() != ["one", "status"]:
                raise RuntimeError(f"unexpected result keys: {result.keys()!r}")
            record = result.single()
            if record is None:
                raise RuntimeError("neo4j driver returned no row for RETURN probe")
            one = record["one"]
            status = record["status"]
            if one != 1 or status != "ok":
                raise RuntimeError(
                    f"unexpected neo4j driver payload: one={one!r}, status={status!r}"
                )

            param_record = session.run(
                "RETURN $one AS one, $status AS status",
                one=1,
                status="ok",
            ).single()
            if param_record is None or param_record["one"] != 1 or param_record["status"] != "ok":
                raise RuntimeError(f"unexpected parameter payload: {param_record!r}")

            unwind_record = session.run(
                "UNWIND [1, 2, 3] AS x RETURN sum(x) AS total"
            ).single()
            if unwind_record is None or unwind_record["total"] != 6:
                raise RuntimeError(f"unexpected UNWIND payload: {unwind_record!r}")

            try:
                session.run("RETURN )").consume()
            except Exception as exc:
                error_text = str(exc)
                if not error_text:
                    raise RuntimeError("cypher syntax error returned an empty exception") from exc
            else:
                raise RuntimeError("expected cypher syntax error was not raised")
    finally:
        driver.close()

    print(
        json.dumps(
            {
                "details": "Neo4j Python driver connected over Bolt and completed read-only RETURN, parameter, UNWIND and error probes",
                "checks": [
                    "bolt_connect",
                    "auth",
                    "session",
                    "return_probe",
                    "result_keys",
                    "parameters",
                    "unwind",
                    "cypher_error",
                ],
                "queries": [
                    "return_probe",
                    "parameter_probe",
                    "unwind_probe",
                    "cypher_error_probe",
                ],
                "capabilities": {
                    "read_only": True,
                    "write_supported": False,
                    "access_mode": "read",
                    "database_scope": "default",
                },
                "uri": uri,
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
