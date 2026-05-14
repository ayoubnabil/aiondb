import json
import os

import psycopg

TABLE = "xtask_psycopg_users"


def main() -> None:
    database_url = os.environ["DATABASE_URL"]
    checks = [
        "connect",
        "parameter_binding",
        "transaction_rollback",
        "information_schema",
        "sqlstate",
    ]

    with psycopg.connect(database_url, autocommit=True) as conn:
        with conn.cursor() as cur:
            cur.execute(f"DROP TABLE IF EXISTS {TABLE}")
            cur.execute(f"CREATE TABLE {TABLE} (id INT NOT NULL, name TEXT NOT NULL)")
            cur.execute(
                f"INSERT INTO {TABLE} (id, name) VALUES (%s, %s), (%s, %s)",
                (1, "alice", 2, "bob"),
            )
            cur.execute(f"SELECT name FROM {TABLE} WHERE id = %s", (2,))
            lookup_name = cur.fetchone()[0]
            cur.execute(
                """
                SELECT column_name
                FROM information_schema.columns
                WHERE table_name = 'xtask_psycopg_users'
                ORDER BY column_name
                """
            )
            columns = [row[0] for row in cur.fetchall()]

        try:
            with conn.transaction():
                with conn.cursor() as cur:
                    cur.execute(
                        f"INSERT INTO {TABLE} (id, name) VALUES (%s, %s)",
                        (3, "carol"),
                    )
                    raise RuntimeError("force rollback")
        except RuntimeError:
            pass

        with conn.cursor() as cur:
            cur.execute(f"SELECT COUNT(*) FROM {TABLE}")
            count_after_rollback = cur.fetchone()[0]

            try:
                cur.execute("SELECT * FROM xtask_psycopg_missing")
            except psycopg.Error as error:
                sqlstate = error.sqlstate
            else:
                raise AssertionError("missing-table query unexpectedly succeeded")

            cur.execute(f"DROP TABLE {TABLE}")

    if lookup_name != "bob":
        raise AssertionError(f"expected lookup to return 'bob', got {lookup_name!r}")
    if columns != ["id", "name"]:
        raise AssertionError(f"unexpected information_schema columns: {columns!r}")
    if count_after_rollback != 2:
        raise AssertionError(
            f"rollback should leave 2 rows, got {count_after_rollback!r}"
        )
    if sqlstate != "42P01":
        raise AssertionError(f"expected SQLSTATE 42P01, got {sqlstate!r}")

    print(
        json.dumps(
            {
                "details": "psycopg executed parameter binding, rollback, information_schema and SQLSTATE checks",
                "checks": checks,
                "observed": {
                    "lookup_name": lookup_name,
                    "columns": columns,
                    "count_after_rollback": count_after_rollback,
                    "sqlstate": sqlstate,
                },
            }
        )
    )


if __name__ == "__main__":
    main()
