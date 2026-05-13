"""
Django ORM + migrations compatibility smoke test.

Builds a temporary Django app, generates and applies real migrations
against AionDB, exercises ORM CRUD/introspection, then migrates back to
zero to validate rollback.
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import time
import traceback
from contextlib import nullcontext
from pathlib import Path
from urllib.parse import parse_qs, urlparse


def parse_database_url(url: str) -> dict:
    parsed = urlparse(url)
    if parsed.scheme not in {"postgres", "postgresql"}:
        raise ValueError(f"unsupported DATABASE_URL scheme: {parsed.scheme}")
    query = parse_qs(parsed.query)
    options = {}
    if "sslmode" in query and query["sslmode"]:
        options["sslmode"] = query["sslmode"][-1]
    return {
        "ENGINE": "django.db.backends.postgresql",
        "NAME": parsed.path.lstrip("/") or "default",
        "USER": parsed.username or "",
        "PASSWORD": parsed.password or "",
        "HOST": parsed.hostname or "",
        "PORT": str(parsed.port or 5432),
        "OPTIONS": options,
    }


def write_file(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


def app_config_source(app_label: str) -> str:
    return f"""from django.apps import AppConfig


class {app_label.title().replace("_", "")}Config(AppConfig):
    default_auto_field = "django.db.models.AutoField"
    name = "{app_label}"
"""


def initial_models_source() -> str:
    return """from django.db import models


class User(models.Model):
    email = models.EmailField(unique=True)
    name = models.CharField(max_length=80, null=True, blank=True)
    created_at = models.DateTimeField(auto_now_add=True)

    class Meta:
        db_table = "xtask_django_users"


class Post(models.Model):
    user = models.ForeignKey(User, on_delete=models.CASCADE, related_name="posts")
    slug = models.SlugField(unique=True)
    title = models.CharField(max_length=120)

    class Meta:
        db_table = "xtask_django_posts"
"""


def evolved_models_source() -> str:
    return """from django.db import models


class User(models.Model):
    email = models.EmailField(unique=True)
    handle = models.CharField(max_length=32, default="anon")
    name = models.CharField(max_length=80, default="", blank=True)
    created_at = models.DateTimeField(auto_now_add=True)

    class Meta:
        db_table = "xtask_django_users"


class Post(models.Model):
    user = models.ForeignKey(User, on_delete=models.CASCADE, related_name="posts")
    slug = models.SlugField(unique=True)
    category = models.CharField(max_length=40, default="general", db_index=True)
    headline = models.CharField(max_length=120)

    class Meta:
        db_table = "xtask_django_posts"
        constraints = [
            models.UniqueConstraint(
                fields=["user", "headline"],
                name="xtask_django_posts_user_headline_uniq",
            )
        ]
"""


def final_models_source() -> str:
    return """from django.db import models


class User(models.Model):
    email = models.EmailField(unique=True)
    handle = models.CharField(max_length=32, default="anon")
    name = models.CharField(max_length=80, default="", blank=True)
    created_at = models.DateTimeField(auto_now_add=True)

    class Meta:
        db_table = "xtask_django_users"


class Post(models.Model):
    user = models.ForeignKey(User, on_delete=models.CASCADE, related_name="posts")
    slug = models.SlugField(unique=True)
    category = models.CharField(max_length=60, default="general")
    headline = models.CharField(max_length=140, db_index=True)

    class Meta:
        db_table = "xtask_django_posts"
"""


def schema_evolution_migration_source(app_label: str) -> str:
    return f"""from django.db import migrations, models


class Migration(migrations.Migration):
    dependencies = [
        ("{app_label}", "0001_initial"),
    ]

    operations = [
        migrations.RenameField(
            model_name="post",
            old_name="title",
            new_name="headline",
        ),
        migrations.AddField(
            model_name="user",
            name="handle",
            field=models.CharField(default="anon", max_length=32),
        ),
        migrations.AlterField(
            model_name="user",
            name="name",
            field=models.CharField(blank=True, default="", max_length=80),
        ),
        migrations.AddField(
            model_name="post",
            name="category",
            field=models.CharField(db_index=True, default="general", max_length=40),
        ),
        migrations.AddConstraint(
            model_name="post",
            constraint=models.UniqueConstraint(
                fields=("user", "headline"),
                name="xtask_django_posts_user_headline_uniq",
            ),
        ),
    ]
"""


def final_schema_evolution_migration_source(app_label: str) -> str:
    return f"""from django.db import migrations, models


class Migration(migrations.Migration):
    dependencies = [
        ("{app_label}", "0002_schema_evolution"),
    ]

    operations = [
        migrations.RemoveConstraint(
            model_name="post",
            name="xtask_django_posts_user_headline_uniq",
        ),
        migrations.AlterField(
            model_name="post",
            name="category",
            field=models.CharField(default="general", max_length=60),
        ),
        migrations.AlterField(
            model_name="post",
            name="headline",
            field=models.CharField(db_index=True, max_length=140),
        ),
    ]
"""


def build_temp_app(root: Path, app_label: str) -> None:
    package_dir = root / app_label
    migrations_dir = package_dir / "migrations"
    write_file(package_dir / "__init__.py", "")
    write_file(package_dir / "apps.py", app_config_source(app_label))
    write_file(package_dir / "models.py", initial_models_source())
    write_file(migrations_dir / "__init__.py", "")


def evolve_temp_app(root: Path, app_label: str) -> None:
    package_dir = root / app_label
    migrations_dir = package_dir / "migrations"
    write_file(package_dir / "models.py", evolved_models_source())
    write_file(
        migrations_dir / "0002_schema_evolution.py",
        schema_evolution_migration_source(app_label),
    )


def finalize_temp_app(root: Path, app_label: str) -> None:
    package_dir = root / app_label
    migrations_dir = package_dir / "migrations"
    write_file(package_dir / "models.py", final_models_source())
    write_file(
        migrations_dir / "0003_reindex_and_alter.py",
        final_schema_evolution_migration_source(app_label),
    )


def run_evolved_phase_subprocess(
    database_url: str, temp_root: Path, app_label: str
) -> dict[str, object]:
    env = os.environ.copy()
    env["DJANGO_DATABASE_URL"] = database_url
    env["AIONDB_DJANGO_PHASE"] = "evolved"
    env["AIONDB_DJANGO_TEMP_ROOT"] = str(temp_root)
    env["AIONDB_DJANGO_APP_LABEL"] = app_label
    proc = subprocess.run(
        [sys.executable, __file__],
        check=False,
        capture_output=True,
        text=True,
        env=env,
    )
    if proc.returncode != 0:
        stdout = proc.stdout.strip()
        stderr = proc.stderr.strip()
        raise RuntimeError(
            f"evolved Django phase failed (code={proc.returncode})\nstdout:\n{stdout}\n\nstderr:\n{stderr}"
        )
    payload = json.loads(proc.stdout)
    if payload.get("status") != "pass":
        raise RuntimeError(f"unexpected evolved phase payload: {payload}")
    return payload["checks"]


def run_final_phase_subprocess(
    database_url: str, temp_root: Path, app_label: str
) -> dict[str, object]:
    env = os.environ.copy()
    env["DJANGO_DATABASE_URL"] = database_url
    env["AIONDB_DJANGO_PHASE"] = "final"
    env["AIONDB_DJANGO_TEMP_ROOT"] = str(temp_root)
    env["AIONDB_DJANGO_APP_LABEL"] = app_label
    proc = subprocess.run(
        [sys.executable, __file__],
        check=False,
        capture_output=True,
        text=True,
        env=env,
    )
    if proc.returncode != 0:
        stdout = proc.stdout.strip()
        stderr = proc.stderr.strip()
        raise RuntimeError(
            f"final Django phase failed (code={proc.returncode})\nstdout:\n{stdout}\n\nstderr:\n{stderr}"
        )
    payload = json.loads(proc.stdout)
    if payload.get("status") != "pass":
        raise RuntimeError(f"unexpected final phase payload: {payload}")
    return payload["checks"]


def run_evolved_phase(database_url: str, temp_root: Path, app_label: str) -> dict[str, object]:
    sys.path.insert(0, str(temp_root))

    from django.conf import settings

    if not settings.configured:
        settings.configure(
            SECRET_KEY="aiondb-django-compat",
            INSTALLED_APPS=[
                "django.contrib.contenttypes",
                f"{app_label}.apps.{app_label.title().replace('_', '')}Config",
            ],
            DATABASES={"default": parse_database_url(database_url)},
            DEFAULT_AUTO_FIELD="django.db.models.AutoField",
            USE_TZ=True,
            TIME_ZONE="UTC",
        )

    import django

    django.setup()

    from django.core.management import call_command
    from django.db import connection

    checks: dict[str, object] = {}
    trace_sql = os.environ.get("AIONDB_DJANGO_TRACE_SQL") == "1"
    traced_queries: list[dict[str, object]] = []

    def sql_tracer(execute, sql, params, many, context):
        traced_queries.append(
            {
                "sql": sql,
                "params": params,
                "many": many,
            }
        )
        return execute(sql, params, many, context)

    trace_ctx = connection.execute_wrapper(sql_tracer) if trace_sql else nullcontext()
    try:
        with trace_ctx:
            call_command("migrate", app_label, verbosity=0)
            checks["evolved_migration"] = "applied"

            with connection.cursor() as cursor:
                cursor.execute(
                    """
                    SELECT column_name, is_nullable, column_default
                    FROM information_schema.columns
                    WHERE table_name = 'xtask_django_users'
                    ORDER BY ordinal_position
                    """
                )
                user_columns = cursor.fetchall()
                cursor.execute(
                    """
                    SELECT column_name
                    FROM information_schema.columns
                    WHERE table_name = 'xtask_django_posts'
                    ORDER BY ordinal_position
                    """
                )
                post_columns = [row[0] for row in cursor.fetchall()]
                post_constraints = connection.introspection.get_constraints(
                    cursor, "xtask_django_posts"
                )
                cursor.execute(
                    "SELECT id FROM xtask_django_users WHERE email = %s",
                    ["alice@example.com"],
                )
                alice_id = cursor.fetchone()[0]
                cursor.execute(
                    "INSERT INTO xtask_django_posts (user_id, slug, headline, category) VALUES (%s, %s, %s, %s)",
                    [alice_id, "after-evolve", "Headline", "general"],
                )
                cursor.execute(
                    "SELECT headline FROM xtask_django_posts WHERE slug = %s",
                    ["after-evolve"],
                )
                evolved_headline = cursor.fetchone()[0]
                cursor.execute(
                    "SELECT category FROM xtask_django_posts WHERE slug = %s",
                    ["after-evolve"],
                )
                evolved_category = cursor.fetchone()[0]
                cursor.execute(
                    "INSERT INTO xtask_django_users (email, name, handle, created_at) VALUES (%s, %s, %s, CURRENT_TIMESTAMP) RETURNING id",
                    ["carol@example.com", "Carol", "carol",],
                )
                carol_id = cursor.fetchone()[0]
                cursor.execute(
                    "INSERT INTO xtask_django_posts (user_id, slug, headline, category) VALUES (%s, %s, %s, %s)",
                    [carol_id, "after-evolve-2", "Headline", "general"],
                )

            checks["evolved_user_columns"] = user_columns
            checks["evolved_post_columns"] = post_columns
            checks["evolved_post_constraints"] = sorted(post_constraints.keys())
            checks["evolved_headline"] = evolved_headline
            checks["evolved_category"] = evolved_category
            assert any(column[0] == "handle" for column in user_columns), user_columns
            assert any(column[0] == "name" and column[1] == "NO" for column in user_columns), user_columns
            assert "category" in post_columns, post_columns
            assert "headline" in post_columns, post_columns
            assert "title" not in post_columns, post_columns
            assert evolved_headline == "Headline"
            assert evolved_category == "general"
            assert any(
                data.get("unique")
                and data.get("columns") == ["user_id", "headline"]
                for data in post_constraints.values()
            ), post_constraints
            assert any(
                data.get("index") and data.get("columns") == ["category"]
                for data in post_constraints.values()
            ), post_constraints

            try:
                with connection.cursor() as cursor:
                    cursor.execute(
                        "UPDATE xtask_django_users SET name = NULL WHERE email = %s",
                        ["alice@example.com"],
                    )
            except Exception as exc:
                checks["evolved_not_null"] = type(exc).__name__
            else:
                raise AssertionError("expected NOT NULL enforcement for evolved user.name")

            try:
                with connection.cursor() as cursor:
                    cursor.execute(
                        "INSERT INTO xtask_django_posts (user_id, slug, headline, category) VALUES (%s, %s, %s, %s)",
                        [alice_id, "after-evolve-dup", "Headline", "general"],
                    )
            except Exception as exc:
                checks["evolved_unique_constraint"] = type(exc).__name__
            else:
                raise AssertionError(
                    "expected composite UNIQUE enforcement for evolved post(user, headline)"
                )

            finalize_temp_app(temp_root, app_label)
    except Exception:
        if trace_sql and traced_queries:
            print(
                json.dumps(
                    {"status": "trace", "traced_queries_tail": traced_queries[-20:]},
                    indent=2,
                    default=str,
                ),
                file=sys.stderr,
            )
        raise

    if trace_sql:
        checks["traced_queries_tail"] = traced_queries[-20:]

    return checks


def run_final_phase(database_url: str, temp_root: Path, app_label: str) -> dict[str, object]:
    sys.path.insert(0, str(temp_root))

    from django.conf import settings

    if not settings.configured:
        settings.configure(
            SECRET_KEY="aiondb-django-compat",
            INSTALLED_APPS=[
                "django.contrib.contenttypes",
                f"{app_label}.apps.{app_label.title().replace('_', '')}Config",
            ],
            DATABASES={"default": parse_database_url(database_url)},
            DEFAULT_AUTO_FIELD="django.db.models.AutoField",
            USE_TZ=True,
            TIME_ZONE="UTC",
        )

    import django

    django.setup()

    from django.core.management import call_command
    from django.db import connection

    checks: dict[str, object] = {}

    call_command("migrate", app_label, verbosity=0)
    checks["final_migration"] = "applied"

    with connection.cursor() as cursor:
        cursor.execute(
            """
            SELECT column_name, character_maximum_length
            FROM information_schema.columns
            WHERE table_name = 'xtask_django_posts'
            ORDER BY ordinal_position
            """
        )
        final_post_columns = cursor.fetchall()
        final_constraints = connection.introspection.get_constraints(
            cursor, "xtask_django_posts"
        )
        cursor.execute(
            "SELECT id FROM xtask_django_users WHERE email = %s",
            ["alice@example.com"],
        )
        alice_id = cursor.fetchone()[0]
        cursor.execute(
            "INSERT INTO xtask_django_posts (user_id, slug, headline, category) VALUES (%s, %s, %s, %s)",
            [alice_id, "after-final-dup", "Headline", "expanded-category"],
        )
        cursor.execute(
            "SELECT category FROM xtask_django_posts WHERE slug = %s",
            ["after-final-dup"],
        )
        final_category = cursor.fetchone()[0]
        cursor.execute(
            "DELETE FROM xtask_django_posts WHERE slug = %s",
            ["after-final-dup"],
        )

    checks["final_post_columns"] = final_post_columns
    checks["final_post_constraints"] = sorted(final_constraints.keys())
    checks["final_category"] = final_category
    assert any(
        column[0] == "category" and column[1] == 60 for column in final_post_columns
    ), final_post_columns
    assert any(
        column[0] == "headline" and column[1] == 140 for column in final_post_columns
    ), final_post_columns
    assert not any(
        data.get("unique") and data.get("columns") == ["user_id", "headline"]
        for data in final_constraints.values()
    ), final_constraints
    assert not any(
        data.get("index") and data.get("columns") == ["category"]
        for data in final_constraints.values()
    ), final_constraints
    assert any(
        data.get("index") and data.get("columns") == ["headline"]
        for data in final_constraints.values()
    ), final_constraints
    assert final_category == "expanded-category"

    call_command("migrate", app_label, "zero", verbosity=0)
    checks["rollback_migrations"] = "ok"

    with connection.cursor() as cursor:
        after_zero = set(connection.introspection.table_names(cursor))
    assert "xtask_django_users" not in after_zero, after_zero
    assert "xtask_django_posts" not in after_zero, after_zero
    checks["tables_after_zero"] = [
        name for name in sorted(after_zero) if name.startswith("xtask_django_")
    ]

    return checks


def run_checks(database_url: str) -> dict:
    temp_root = Path(tempfile.mkdtemp(prefix="aiondb-django-orm-"))
    app_label = f"xtask_django_app_{int(time.time() * 1000)}"
    build_temp_app(temp_root, app_label)
    sys.path.insert(0, str(temp_root))

    trace_sql = os.environ.get("AIONDB_DJANGO_TRACE_SQL") == "1"
    traced_queries: list[dict[str, object]] = []

    try:
        from django.conf import settings

        if not settings.configured:
            settings.configure(
                SECRET_KEY="aiondb-django-compat",
                INSTALLED_APPS=[
                    "django.contrib.contenttypes",
                    f"{app_label}.apps.{app_label.title().replace('_', '')}Config",
                ],
                DATABASES={"default": parse_database_url(database_url)},
                DEFAULT_AUTO_FIELD="django.db.models.AutoField",
                USE_TZ=True,
                TIME_ZONE="UTC",
            )

        import django

        django.setup()

        from django.core.management import call_command
        from django.db import connection, transaction
        from django.utils import timezone

        models_mod = __import__(f"{app_label}.models", fromlist=["Post", "User"])
        User = models_mod.User
        Post = models_mod.Post

        checks: dict[str, object] = {}

        def sql_tracer(execute, sql, params, many, context):
            traced_queries.append(
                {
                    "sql": sql,
                    "params": params,
                    "many": many,
                }
            )
            return execute(sql, params, many, context)

        trace_ctx = connection.execute_wrapper(sql_tracer) if trace_sql else nullcontext()

        with trace_ctx:
            call_command("migrate", "contenttypes", verbosity=0)
            call_command("makemigrations", app_label, verbosity=0)
            call_command("migrate", app_label, verbosity=0)
            checks["migrations"] = "applied"

            alice = User.objects.create(email="alice@example.com", name="Alice")
            Post.objects.create(user=alice, slug="hello-world", title="Hello")
            Post.objects.create(user=alice, slug="second-post", title="Second")

            loaded = list(
                Post.objects.select_related("user")
                .order_by("id")
                .values_list("slug", "user__email")
            )
            assert loaded == [
                ("hello-world", "alice@example.com"),
                ("second-post", "alice@example.com"),
            ], loaded
            checks["orm_select_related"] = loaded

            try:
                with transaction.atomic():
                    bob = User.objects.create(email="bob@example.com", name="Bob")
                    Post.objects.create(user=bob, slug="atomic-post", title="Atomic")
                    raise RuntimeError("rollback probe")
            except RuntimeError as exc:
                if str(exc) != "rollback probe":
                    raise
                checks["transaction_rollback"] = (
                    User.objects.filter(email="bob@example.com").count(),
                    Post.objects.filter(slug="atomic-post").count(),
                )
                assert checks["transaction_rollback"] == (0, 0)
            else:
                raise AssertionError("transaction rollback probe did not trigger")

            with connection.cursor() as cursor:
                tables = sorted(connection.introspection.table_names(cursor))
                constraints = connection.introspection.get_constraints(
                    cursor, Post._meta.db_table
                )
            assert "xtask_django_users" in tables
            assert "xtask_django_posts" in tables
            assert any(data.get("primary_key") for data in constraints.values()), constraints
            assert any(data.get("foreign_key") for data in constraints.values()), constraints
            assert any(
                data.get("unique") and "slug" in data.get("columns", [])
                for data in constraints.values()
            ), constraints
            checks["tables"] = [
                name
                for name in tables
                if name.startswith("xtask_django_") or name == "django_migrations"
            ]
            checks["constraint_names"] = sorted(constraints.keys())

            with connection.cursor() as cursor:
                cursor.execute("SELECT COUNT(*) FROM xtask_django_posts")
                checks["post_count"] = cursor.fetchone()[0]
                cursor.execute(
                    "SELECT app, name FROM django_migrations WHERE app = %s ORDER BY name",
                    [app_label],
                )
                checks["migration_rows"] = cursor.fetchall()
            assert checks["post_count"] == 2
            assert len(checks["migration_rows"]) >= 1

            checks["timestamp_sample"] = timezone.now().isoformat()

            evolve_temp_app(temp_root, app_label)
            checks.update(run_evolved_phase_subprocess(database_url, temp_root, app_label))
            checks.update(run_final_phase_subprocess(database_url, temp_root, app_label))

        if trace_sql:
            checks["traced_queries_tail"] = traced_queries[-20:]

        return checks
    except Exception:
        if trace_sql and traced_queries:
            print(
                json.dumps(
                    {"status": "trace", "traced_queries_tail": traced_queries[-20:]},
                    indent=2,
                    default=str,
                ),
                file=sys.stderr,
            )
        raise


def main() -> None:
    phase = os.environ.get("AIONDB_DJANGO_PHASE")
    if phase == "evolved":
        database_url = os.environ["DJANGO_DATABASE_URL"]
        temp_root = Path(os.environ["AIONDB_DJANGO_TEMP_ROOT"])
        app_label = os.environ["AIONDB_DJANGO_APP_LABEL"]
        try:
            checks = run_evolved_phase(database_url, temp_root, app_label)
            print(json.dumps({"status": "pass", "checks": checks}, indent=2))
        except Exception as exc:  # pragma: no cover - direct CLI smoke
            traceback.print_exc(file=sys.stderr)
            print(json.dumps({"status": "fail", "error": str(exc)}))
            sys.exit(1)
        return
    if phase == "final":
        database_url = os.environ["DJANGO_DATABASE_URL"]
        temp_root = Path(os.environ["AIONDB_DJANGO_TEMP_ROOT"])
        app_label = os.environ["AIONDB_DJANGO_APP_LABEL"]
        try:
            checks = run_final_phase(database_url, temp_root, app_label)
            print(json.dumps({"status": "pass", "checks": checks}, indent=2))
        except Exception as exc:  # pragma: no cover - direct CLI smoke
            traceback.print_exc(file=sys.stderr)
            print(json.dumps({"status": "fail", "error": str(exc)}))
            sys.exit(1)
        return

    database_url = os.environ.get("DJANGO_DATABASE_URL") or os.environ.get(
        "SQLALCHEMY_DATABASE_URL"
    )
    if not database_url:
        print("DJANGO_DATABASE_URL or SQLALCHEMY_DATABASE_URL not set", file=sys.stderr)
        sys.exit(1)

    try:
        checks = run_checks(database_url)
        print(json.dumps({"status": "pass", "checks": checks}, indent=2))
    except Exception as exc:  # pragma: no cover - direct CLI smoke
        traceback.print_exc(file=sys.stderr)
        print(json.dumps({"status": "fail", "error": str(exc)}))
        sys.exit(1)


if __name__ == "__main__":
    main()
