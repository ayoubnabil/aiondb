"""
Alembic autogenerate compatibility test.

Creates a small SQLAlchemy metadata graph against AionDB, reflects it via
Alembic's autogenerate pipeline, and asserts there are no false-positive
diffs for common PostgreSQL ORM types such as unbounded VARCHAR and ENUM.
"""
import json
import os
import sys
import traceback

from sqlalchemy import (
    ARRAY,
    Column,
    DateTime,
    Enum,
    ForeignKey,
    Integer,
    MetaData,
    Numeric,
    SmallInteger,
    String,
    Table,
    UniqueConstraint,
    create_engine,
    inspect,
    text,
)

try:
    from alembic.autogenerate import produce_migrations
    from alembic.migration import MigrationContext
except Exception as exc:  # pragma: no cover - direct CLI smoke
    print(json.dumps({"status": "skip", "reason": f"alembic import failed: {exc}"}))
    sys.exit(0)


def dump_op(op):
    data = {"type": type(op).__name__}
    if hasattr(op, "ops"):
        data["ops"] = [dump_op(child) for child in op.ops]
    if hasattr(op, "table_name"):
        data["table_name"] = op.table_name
    if hasattr(op, "column_name"):
        data["column_name"] = op.column_name
    if hasattr(op, "modify_type"):
        data["modify_type"] = str(op.modify_type)
    if hasattr(op, "existing_type"):
        data["existing_type"] = str(op.existing_type)
    return data


def run_checks(engine):
    metadata = MetaData()
    status_enum = Enum("draft", "live", name="xtask_alembic_status_enum")
    Table(
        "xtask_alembic_users",
        metadata,
        Column("id", Integer, primary_key=True),
        Column("email", String, nullable=False),
        Column("status", status_enum, nullable=False),
        Column("created_at", DateTime(timezone=True), nullable=False, server_default=text("CURRENT_TIMESTAMP")),
    )
    Table(
        "xtask_alembic_posts",
        metadata,
        Column("id", Integer, primary_key=True),
        Column("user_id", Integer, ForeignKey("xtask_alembic_users.id"), nullable=False),
        Column("title", String, nullable=False, server_default=text("'untitled'")),
        Column("score", Numeric(5, 3), nullable=False, server_default=text("0")),
    )
    Table(
        "xtask_alembic_scalars",
        metadata,
        Column("id", Integer, primary_key=True),
        Column("s16", SmallInteger, nullable=False),
        Column("tags", ARRAY(Integer), nullable=False),
    )
    app_status_enum = Enum("draft", "live", name="xtask_alembic_status_enum_app", schema="app")
    Table(
        "xtask_alembic_parent_app",
        metadata,
        Column("id", Integer, primary_key=True),
        Column("email", String, nullable=False, unique=True),
        schema="app",
    )
    Table(
        "xtask_alembic_child_app",
        metadata,
        Column("id", Integer, primary_key=True),
        Column(
            "parent_id",
            Integer,
            ForeignKey("app.xtask_alembic_parent_app.id"),
            nullable=False,
        ),
        Column("title", String, nullable=False, server_default=text("'untitled'")),
        Column("status", app_status_enum, nullable=False),
        UniqueConstraint(
            "parent_id",
            "title",
            name="uq_xtask_alembic_child_parent_title",
        ),
        schema="app",
    )

    with engine.begin() as conn:
        conn.execute(text("DROP TABLE IF EXISTS app.xtask_alembic_child_app"))
        conn.execute(text("DROP TABLE IF EXISTS app.xtask_alembic_parent_app"))
        conn.execute(text("DROP TABLE IF EXISTS xtask_alembic_scalars"))
        conn.execute(text("DROP TABLE IF EXISTS xtask_alembic_posts"))
        conn.execute(text("DROP TABLE IF EXISTS xtask_alembic_users"))
        conn.execute(text("DROP TYPE IF EXISTS app.xtask_alembic_status_enum_app"))
        conn.execute(text("DROP TYPE IF EXISTS xtask_alembic_status_enum"))
        conn.execute(text("DROP SCHEMA IF EXISTS app CASCADE"))
        conn.execute(text("CREATE SCHEMA app"))
        metadata.create_all(conn)

    try:
        with engine.connect() as conn:
            info_schema = conn.execute(
                text(
                    "SELECT column_name, data_type, udt_name, numeric_precision, numeric_scale "
                    "FROM information_schema.columns "
                    "WHERE (table_schema = 'public' AND table_name IN ('xtask_alembic_users', 'xtask_alembic_posts', 'xtask_alembic_scalars')) "
                    "   OR (table_schema = 'app' AND table_name IN ('xtask_alembic_parent_app', 'xtask_alembic_child_app')) "
                    "ORDER BY table_schema, table_name, ordinal_position"
                )
            ).fetchall()
            pg_attr = conn.execute(
                text(
                    "SELECT c.relname, a.attname, pg_catalog.format_type(a.atttypid, a.atttypmod) "
                    "FROM pg_catalog.pg_class c "
                    "JOIN pg_catalog.pg_attribute a ON a.attrelid = c.oid "
                    "WHERE c.relname IN ('xtask_alembic_users', 'xtask_alembic_posts', 'xtask_alembic_scalars', 'xtask_alembic_parent_app', 'xtask_alembic_child_app') "
                    "AND a.attnum > 0 AND NOT a.attisdropped "
                    "ORDER BY c.relname, a.attnum"
                )
            ).fetchall()
            ctx = MigrationContext.configure(
                conn,
                opts={
                    "target_metadata": metadata,
                    "compare_type": True,
                    "compare_server_default": True,
                    "include_schemas": True,
                },
            )
            app_uniques = inspect(engine).get_unique_constraints(
                "xtask_alembic_child_app", schema="app"
            )
            score_column = next(
                col
                for col in inspect(engine).get_columns("xtask_alembic_posts")
                if col["name"] == "score"
            )
            scalar_columns = {
                col["name"]: {
                    "type": str(col["type"]),
                    "class": type(col["type"]).__name__,
                    "dimensions": getattr(col["type"], "dimensions", None),
                }
                for col in inspect(engine).get_columns("xtask_alembic_scalars")
            }
            migrations = produce_migrations(ctx, metadata)
            ops = [dump_op(op) for op in migrations.upgrade_ops.ops]

        return {
            "info_schema": [list(row) for row in info_schema],
            "pg_attr": [list(row) for row in pg_attr],
            "score_column": {
                "type": str(score_column["type"]),
                "precision": getattr(score_column["type"], "precision", None),
                "scale": getattr(score_column["type"], "scale", None),
            },
            "scalar_columns": scalar_columns,
            "app_unique_constraints": app_uniques,
            "ops": ops,
        }
    finally:
        with engine.begin() as conn:
            conn.execute(text("DROP TABLE IF EXISTS app.xtask_alembic_child_app"))
            conn.execute(text("DROP TABLE IF EXISTS app.xtask_alembic_parent_app"))
            conn.execute(text("DROP TABLE IF EXISTS xtask_alembic_scalars"))
            conn.execute(text("DROP TABLE IF EXISTS xtask_alembic_posts"))
            conn.execute(text("DROP TABLE IF EXISTS xtask_alembic_users"))
            conn.execute(text("DROP TYPE IF EXISTS app.xtask_alembic_status_enum_app"))
            conn.execute(text("DROP TYPE IF EXISTS xtask_alembic_status_enum"))
            conn.execute(text("DROP SCHEMA IF EXISTS app CASCADE"))


def main():
    url = os.environ.get("SQLALCHEMY_DATABASE_URL")
    if not url:
        print("SQLALCHEMY_DATABASE_URL not set", file=sys.stderr)
        sys.exit(1)

    engine = create_engine(url, future=True)
    try:
        results = run_checks(engine)
        assert results["ops"] == [], f"unexpected alembic diffs: {results['ops']}"
        assert results["scalar_columns"]["s16"]["type"] == "SMALLINT"
        assert results["scalar_columns"]["tags"]["type"] == "ARRAY"
        print(json.dumps({"status": "pass", "checks": results}, indent=2))
    except Exception as exc:  # pragma: no cover - direct CLI smoke
        traceback.print_exc(file=sys.stderr)
        print(json.dumps({"status": "fail", "error": str(exc)}))
        sys.exit(1)
    finally:
        engine.dispose()


if __name__ == "__main__":
    main()
