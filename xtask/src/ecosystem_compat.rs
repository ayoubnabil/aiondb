use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aiondb_engine::{
    Credential, Engine, EngineBuilder, QueryEngine, SecretString, StartupParams, TransportInfo,
};
use aiondb_pgwire::server::{PgWireConfig, PgWireServer};
use serde::Serialize;
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::sync::watch;

const DEFAULT_REPORT_PATH: &str = "tmp/clean_prod/ecosystem_compat.json";
const DEFAULT_HISTORY_PATH: &str = "tmp/clean_prod/ecosystem_compat_history.jsonl";
const DEFAULT_DATABASE: &str = "default";
const DEFAULT_USER: &str = "xtask";
const DEFAULT_PASSWORD: &str = "xtask";
const DEFAULT_HOST: &str = "127.0.0.1";
const SERVER_READY_TIMEOUT: Duration = Duration::from_secs(10);
const SERVER_READY_POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug)]
pub(crate) struct EcosystemCompatOptions {
    suite_filter: Option<String>,
    report_path: PathBuf,
    history_path: Option<PathBuf>,
    strict: bool,
    list_only: bool,
}

#[derive(Clone, Copy)]
struct SuiteDefinition {
    name: &'static str,
    description: &'static str,
    run: fn(&SuiteContext) -> SuiteRecord,
}

struct SuiteContext {
    workspace_root: PathBuf,
    database_url: String,
    sqlalchemy_database_url: String,
    engine: Arc<Engine>,
    scratch_dir: PathBuf,
}

struct ServerHarness {
    engine: Arc<Engine>,
    port: u16,
    shutdown_tx: watch::Sender<bool>,
    thread_handle: Option<thread::JoinHandle<()>>,
    error_slot: Arc<Mutex<Option<String>>>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum SuiteStatus {
    Passed,
    Failed,
    Skipped,
}

#[derive(Debug, Serialize)]
struct SuiteRecord {
    name: String,
    description: String,
    status: SuiteStatus,
    duration_ms: u128,
    details: String,
    checks: Vec<String>,
    command: Option<String>,
    payload: Option<Value>,
    stdout: Option<String>,
    stderr: Option<String>,
}

#[derive(Debug, Serialize)]
struct EcosystemCompatReport {
    generated_at_unix_ms: u128,
    generated_at_iso8601: String,
    git_commit_full: Option<String>,
    git_commit_short: Option<String>,
    git_dirty: Option<bool>,
    listen_addr: String,
    strict: bool,
    report_path: String,
    history_path: Option<String>,
    summary: Summary,
    suites: Vec<SuiteRecord>,
}

#[derive(Debug, Serialize)]
struct Summary {
    passed: usize,
    failed: usize,
    skipped: usize,
}

#[derive(Debug, Clone, Default)]
struct CommitInfo {
    full: Option<String>,
    short: Option<String>,
    dirty: Option<bool>,
}

#[derive(Debug)]
struct CommandCapture {
    status_code: Option<i32>,
    stdout: String,
    stderr: String,
}

#[derive(Debug)]
struct JsonScriptResult {
    payload: Value,
    stdout: String,
    stderr: String,
    command: String,
}

pub(crate) fn parse_args(args: &[String]) -> Result<EcosystemCompatOptions, String> {
    let mut suite_filter = None;
    let mut report_path = PathBuf::from(DEFAULT_REPORT_PATH);
    let mut history_override: Option<PathBuf> = None;
    let mut history_disabled = false;
    let mut strict = false;
    let mut list_only = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--suite" => {
                i += 1;
                suite_filter = Some(
                    args.get(i)
                        .ok_or_else(|| "--suite requires a value".to_owned())?
                        .clone(),
                );
            }
            "--report" => {
                i += 1;
                report_path = PathBuf::from(
                    args.get(i)
                        .ok_or_else(|| "--report requires a value".to_owned())?,
                );
            }
            "--history" => {
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| "--history requires a value".to_owned())?;
                history_override = Some(PathBuf::from(value));
                history_disabled = false;
            }
            "--no-history" => {
                history_disabled = true;
                history_override = None;
            }
            "--strict" => {
                strict = true;
            }
            "--list" => {
                list_only = true;
            }
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            other => {
                return Err(format!(
                    "unknown flag for ecosystem-compat: {other}\n\nRun `cargo xtask ecosystem-compat --help` for usage."
                ));
            }
        }
        i += 1;
    }

    if let Some(name) = &suite_filter {
        if !suite_definitions()
            .iter()
            .any(|suite| suite.name == name.as_str())
        {
            let valid = suite_definitions()
                .iter()
                .map(|suite| suite.name)
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!("unknown suite '{name}'. Valid suites: {valid}"));
        }
    }

    let history_path = if history_disabled {
        None
    } else {
        Some(history_override.unwrap_or_else(|| PathBuf::from(DEFAULT_HISTORY_PATH)))
    };

    Ok(EcosystemCompatOptions {
        suite_filter,
        report_path,
        history_path,
        strict,
        list_only,
    })
}

pub(crate) fn run(opts: EcosystemCompatOptions) -> ExitCode {
    if opts.list_only {
        for suite in suite_definitions() {
            println!("{:<22} {}", suite.name, suite.description);
        }
        return ExitCode::SUCCESS;
    }

    let workspace_root = match workspace_root() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
    };

    let scratch = match TempDir::new_in(workspace_root.join("target")) {
        Ok(dir) => dir,
        Err(error) => {
            eprintln!("error: failed to create scratch dir under target/: {error}");
            return ExitCode::FAILURE;
        }
    };

    let harness = match ServerHarness::start() {
        Ok(harness) => harness,
        Err(error) => {
            eprintln!("error: failed to start pgwire harness: {error}");
            return ExitCode::FAILURE;
        }
    };

    // Create a test role so network clients can authenticate with a password.
    {
        let (session, _) = harness
            .engine
            .startup(startup_params("xtask-setup"))
            .expect("setup startup");
        let _ = harness.engine.execute_sql(
            &session,
            "CREATE ROLE xtask WITH LOGIN PASSWORD 'xtask' SUPERUSER",
        );
        let _ = harness.engine.terminate(session.clone());
    }

    let ctx = SuiteContext {
        workspace_root: workspace_root.clone(),
        database_url: format!(
            "postgresql://{DEFAULT_USER}:{DEFAULT_PASSWORD}@{DEFAULT_HOST}:{port}/{DEFAULT_DATABASE}?sslmode=disable",
            port = harness.port
        ),
        sqlalchemy_database_url: format!(
            "postgresql+psycopg://{DEFAULT_USER}:{DEFAULT_PASSWORD}@{DEFAULT_HOST}:{port}/{DEFAULT_DATABASE}?sslmode=disable",
            port = harness.port
        ),
        engine: Arc::clone(&harness.engine),
        scratch_dir: scratch.path().to_path_buf(),
    };

    let suites: Vec<SuiteDefinition> = suite_definitions()
        .iter()
        .copied()
        .filter(|suite| match opts.suite_filter.as_ref() {
            Some(name) => suite.name == name.as_str(),
            None => true,
        })
        .collect();

    let records: Vec<SuiteRecord> = suites.iter().map(|suite| (suite.run)(&ctx)).collect();

    let summary = Summary {
        passed: records
            .iter()
            .filter(|record| matches!(record.status, SuiteStatus::Passed))
            .count(),
        failed: records
            .iter()
            .filter(|record| matches!(record.status, SuiteStatus::Failed))
            .count(),
        skipped: records
            .iter()
            .filter(|record| matches!(record.status, SuiteStatus::Skipped))
            .count(),
    };

    let generated_at_unix_ms = unix_time_millis();
    let commit_info = capture_commit_info(&workspace_root);

    let report = EcosystemCompatReport {
        generated_at_unix_ms,
        generated_at_iso8601: format_iso8601_utc(generated_at_unix_ms),
        git_commit_full: commit_info.full.clone(),
        git_commit_short: commit_info.short.clone(),
        git_dirty: commit_info.dirty,
        listen_addr: format!("{DEFAULT_HOST}:{}", harness.port),
        strict: opts.strict,
        report_path: opts.report_path.display().to_string(),
        history_path: opts
            .history_path
            .as_ref()
            .map(|path| path.display().to_string()),
        summary,
        suites: records,
    };

    if let Err(error) = write_report(&opts.report_path, &report) {
        eprintln!("error: failed to write report: {error}");
        return ExitCode::FAILURE;
    }

    if let Some(history_path) = opts.history_path.as_ref() {
        if let Err(error) = append_history_entry(history_path, &report) {
            eprintln!("error: failed to append history entry: {error}");
            return ExitCode::FAILURE;
        }
    }

    print_report_summary(&report);

    let has_failed = report
        .suites
        .iter()
        .any(|record| matches!(record.status, SuiteStatus::Failed));
    let has_skipped = report
        .suites
        .iter()
        .any(|record| matches!(record.status, SuiteStatus::Skipped));
    if has_failed || (opts.strict && has_skipped) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn print_usage() {
    println!(
        "\
Usage: cargo xtask ecosystem-compat [OPTIONS]

Run the official driver / ORM PostgreSQL compatibility matrix against an
ephemeral in-process AionDB pgwire server.

Options:
  --suite <NAME>    Run only a single suite
  --report <PATH>   Override the JSON report path (default: tmp/clean_prod/ecosystem_compat.json)
  --history <PATH>  Override the JSONL history path (default: tmp/clean_prod/ecosystem_compat_history.jsonl)
  --no-history      Do not append a per-run history record
  --strict          Treat skipped suites as failures
  --list            Print the available suites
  -h, --help        Print this help message"
    );
}

fn suite_definitions() -> &'static [SuiteDefinition] {
    &[
        SuiteDefinition {
            name: "psql-libpq",
            description: "Smoke test via psql/libpq: connect, prepare, tx rollback, information_schema, SQLSTATE",
            run: run_psql_suite,
        },
        SuiteDefinition {
            name: "psycopg",
            description: "Python psycopg v3 parameter binding, rollback semantics and SQLSTATE propagation",
            run: run_psycopg_suite,
        },
        SuiteDefinition {
            name: "sqlalchemy",
            description: "SQLAlchemy reflection + bound parameters over psycopg",
            run: run_sqlalchemy_suite,
        },
        SuiteDefinition {
            name: "django",
            description: "Django ORM migrations, introspection, constraints and rollback",
            run: run_django_suite,
        },
        SuiteDefinition {
            name: "node-postgres",
            description: "node-postgres prepared parameters, rollback semantics and introspection",
            run: run_node_postgres_suite,
        },
        SuiteDefinition {
            name: "prisma-introspection",
            description: "Prisma db pull introspection against a live AionDB schema",
            run: run_prisma_suite,
        },
        SuiteDefinition {
            name: "diesel",
            description: "Diesel PgConnection SQL query, bind parameters, rollback, introspection and error class",
            run: run_diesel_suite,
        },
    ]
}

impl ServerHarness {
    fn start() -> Result<Self, String> {
        let port = reserve_tcp_port()?;
        let engine = Arc::new(
            EngineBuilder::for_testing()
                .build()
                .map_err(|error| error.to_string())?,
        );
        let config = PgWireConfig {
            bind_address: DEFAULT_HOST.to_owned(),
            port,
            require_tls: false,
            ..PgWireConfig::default()
        };
        let server = Arc::new(PgWireServer::new_plain(Arc::clone(&engine), config));
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let error_slot = Arc::new(Mutex::new(None));
        let error_slot_for_thread = Arc::clone(&error_slot);
        let server_for_thread = Arc::clone(&server);

        let thread_handle = thread::spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    *error_slot_for_thread.lock().expect("poisoned error slot") =
                        Some(format!("failed to build tokio runtime: {error}"));
                    return;
                }
            };

            runtime.block_on(async move {
                if let Err(error) = server_for_thread.start(shutdown_rx).await {
                    *error_slot_for_thread.lock().expect("poisoned error slot") =
                        Some(error.to_string());
                }
            });
        });

        wait_for_server_ready(port, &error_slot)?;

        Ok(Self {
            engine,
            port,
            shutdown_tx,
            thread_handle: Some(thread_handle),
            error_slot,
        })
    }
}

impl Drop for ServerHarness {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(true);
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
        let _ = &self.error_slot;
    }
}

fn run_psql_suite(ctx: &SuiteContext) -> SuiteRecord {
    let start = Instant::now();
    let Some(psql) = find_in_path("psql") else {
        return skipped_record(
            "psql-libpq",
            suite_description("psql-libpq"),
            start.elapsed(),
            "psql was not found in PATH",
        );
    };

    let smoke_sql = ctx.workspace_root.join("testing/ecosystem/psql/smoke.sql");
    let error_sql = ctx
        .workspace_root
        .join("testing/ecosystem/psql/undefined_table.sql");
    let envs = vec![
        ("DATABASE_URL", ctx.database_url.clone()),
        ("PGAPPNAME", "aiondb-ecosystem-compat".to_owned()),
    ];

    let smoke_args = vec![
        "-X".to_owned(),
        "-q".to_owned(),
        "-t".to_owned(),
        "-A".to_owned(),
        "-v".to_owned(),
        "ON_ERROR_STOP=1".to_owned(),
        ctx.database_url.clone(),
        "-f".to_owned(),
        smoke_sql.display().to_string(),
    ];
    let smoke_command = command_display(&psql, &smoke_args);
    let smoke = match run_command(&psql, &smoke_args, &envs, &ctx.workspace_root) {
        Ok(output) => output,
        Err(error) => {
            return failed_record(
                "psql-libpq",
                suite_description("psql-libpq"),
                start.elapsed(),
                format!("failed to invoke psql: {error}"),
                vec![],
                Some(smoke_command),
                None,
                None,
                None,
            );
        }
    };
    if smoke.status_code != Some(0) {
        return failed_record(
            "psql-libpq",
            suite_description("psql-libpq"),
            start.elapsed(),
            format!(
                "psql smoke run exited with status {:?}",
                smoke.status_code.unwrap_or_default()
            ),
            vec![],
            Some(smoke_command),
            None,
            Some(smoke.stdout),
            Some(smoke.stderr),
        );
    }

    let observed_lines = normalized_lines(&smoke.stdout);
    let expected_lines = ["bob", "id", "name", "2"];
    if observed_lines.as_slice() != expected_lines {
        return failed_record(
            "psql-libpq",
            suite_description("psql-libpq"),
            start.elapsed(),
            format!("unexpected psql output: expected {expected_lines:?}, got {observed_lines:?}"),
            vec![
                "connect".to_owned(),
                "prepared_statement".to_owned(),
                "transaction_rollback".to_owned(),
                "information_schema".to_owned(),
            ],
            Some(smoke_command),
            Some(json!({ "observed_lines": observed_lines })),
            Some(smoke.stdout),
            Some(smoke.stderr),
        );
    }

    let error_args = vec![
        "-X".to_owned(),
        "-q".to_owned(),
        "-v".to_owned(),
        "ON_ERROR_STOP=1".to_owned(),
        "-v".to_owned(),
        "VERBOSITY=verbose".to_owned(),
        ctx.database_url.clone(),
        "-f".to_owned(),
        error_sql.display().to_string(),
    ];
    let error_command = command_display(&psql, &error_args);
    let error_output = match run_command(&psql, &error_args, &envs, &ctx.workspace_root) {
        Ok(output) => output,
        Err(error) => {
            return failed_record(
                "psql-libpq",
                suite_description("psql-libpq"),
                start.elapsed(),
                format!("failed to invoke psql verbose error probe: {error}"),
                vec![],
                Some(error_command),
                None,
                None,
                None,
            );
        }
    };
    if error_output.status_code == Some(0) {
        return failed_record(
            "psql-libpq",
            suite_description("psql-libpq"),
            start.elapsed(),
            "expected undefined-table probe to fail, but psql exited successfully".to_owned(),
            vec!["sqlstate".to_owned()],
            Some(error_command),
            None,
            Some(error_output.stdout),
            Some(error_output.stderr),
        );
    }
    let combined_error_output = format!("{}\n{}", error_output.stdout, error_output.stderr);
    if !combined_error_output.contains("42P01") {
        return failed_record(
            "psql-libpq",
            suite_description("psql-libpq"),
            start.elapsed(),
            "undefined-table probe did not surface SQLSTATE 42P01".to_owned(),
            vec!["sqlstate".to_owned()],
            Some(error_command),
            Some(json!({ "combined_output": combined_error_output })),
            Some(error_output.stdout),
            Some(error_output.stderr),
        );
    }

    passed_record(
        "psql-libpq",
        suite_description("psql-libpq"),
        start.elapsed(),
        "psql/libpq completed smoke workflow and surfaced SQLSTATE 42P01".to_owned(),
        vec![
            "connect".to_owned(),
            "prepared_statement".to_owned(),
            "transaction_rollback".to_owned(),
            "information_schema".to_owned(),
            "sqlstate".to_owned(),
        ],
        Some(error_command),
        Some(json!({
            "observed_lines": observed_lines,
            "sqlstate_probe": "42P01"
        })),
        Some(error_output.stdout),
        Some(error_output.stderr),
    )
}

fn run_psycopg_suite(ctx: &SuiteContext) -> SuiteRecord {
    run_python_json_suite(
        ctx,
        "psycopg",
        &["psycopg"],
        "testing/ecosystem/python/psycopg_smoke.py",
        &[("DATABASE_URL", ctx.database_url.clone())],
    )
}

fn run_sqlalchemy_suite(ctx: &SuiteContext) -> SuiteRecord {
    run_python_json_suite(
        ctx,
        "sqlalchemy",
        &["sqlalchemy", "psycopg"],
        "testing/ecosystem/python/sqlalchemy_orm_compat.py",
        &[
            ("DATABASE_URL", ctx.database_url.clone()),
            (
                "SQLALCHEMY_DATABASE_URL",
                ctx.sqlalchemy_database_url.clone(),
            ),
        ],
    )
}

fn run_django_suite(ctx: &SuiteContext) -> SuiteRecord {
    run_python_json_suite(
        ctx,
        "django",
        &["django", "psycopg"],
        "testing/ecosystem/python/django_orm_compat.py",
        &[("DJANGO_DATABASE_URL", ctx.database_url.clone())],
    )
}

fn run_node_postgres_suite(ctx: &SuiteContext) -> SuiteRecord {
    let start = Instant::now();
    let Some(node) = find_in_path("node") else {
        return skipped_record(
            "node-postgres",
            suite_description("node-postgres"),
            start.elapsed(),
            "node was not found in PATH",
        );
    };
    let node_resolve_base = ctx.workspace_root.join("target/compat/node-tools");
    if let Err(reason) = ensure_node_packages(&node, &node_resolve_base, &["pg"]) {
        return skipped_record(
            "node-postgres",
            suite_description("node-postgres"),
            start.elapsed(),
            reason,
        );
    }

    let script = ctx
        .workspace_root
        .join("testing/ecosystem/node/node_postgres_smoke.mjs");
    let args = vec![script.display().to_string()];
    let envs = vec![
        ("DATABASE_URL", ctx.database_url.clone()),
        (
            "AIONDB_NODE_RESOLVE_BASE",
            node_resolve_base.display().to_string(),
        ),
    ];
    match run_json_script_command(
        "node-postgres",
        suite_description("node-postgres"),
        &node,
        &args,
        &envs,
        &ctx.workspace_root,
        start,
    ) {
        Ok(record) => record,
        Err(record) => *record,
    }
}

fn run_prisma_suite(ctx: &SuiteContext) -> SuiteRecord {
    let start = Instant::now();
    let Some(prisma) = resolve_prisma_command(&ctx.workspace_root) else {
        return skipped_record(
            "prisma-introspection",
            suite_description("prisma-introspection"),
            start.elapsed(),
            "prisma CLI was not found (checked node_modules/.bin/prisma and PATH)",
        );
    };

    if let Err(error) = run_engine_sql(
        &ctx.engine,
        "prisma-introspection",
        "DROP TABLE IF EXISTS xtask_prisma_users; \
         CREATE TABLE xtask_prisma_users (id INT NOT NULL, name TEXT NOT NULL)",
    ) {
        return failed_record(
            "prisma-introspection",
            suite_description("prisma-introspection"),
            start.elapsed(),
            format!("failed to seed schema for Prisma introspection: {error}"),
            vec![],
            None,
            None,
            None,
            None,
        );
    }

    let prisma_dir = ctx.scratch_dir.join("prisma");
    if let Err(error) = fs::create_dir_all(&prisma_dir) {
        return failed_record(
            "prisma-introspection",
            suite_description("prisma-introspection"),
            start.elapsed(),
            format!("failed to create Prisma scratch dir: {error}"),
            vec![],
            None,
            None,
            None,
            None,
        );
    }
    let schema_template = ctx
        .workspace_root
        .join("testing/ecosystem/prisma/schema.prisma");
    let schema_path = prisma_dir.join("schema.prisma");
    if let Err(error) = fs::copy(&schema_template, &schema_path) {
        return failed_record(
            "prisma-introspection",
            suite_description("prisma-introspection"),
            start.elapsed(),
            format!("failed to copy Prisma schema template: {error}"),
            vec![],
            None,
            None,
            None,
            None,
        );
    }

    let args = vec![
        "db".to_owned(),
        "pull".to_owned(),
        "--schema".to_owned(),
        schema_path.display().to_string(),
        "--url".to_owned(),
        ctx.database_url.clone(),
    ];
    let envs = vec![("DATABASE_URL", ctx.database_url.clone())];
    let command = command_display(&prisma, &args);
    let output = match run_command(&prisma, &args, &envs, &ctx.workspace_root) {
        Ok(output) => output,
        Err(error) => {
            return failed_record(
                "prisma-introspection",
                suite_description("prisma-introspection"),
                start.elapsed(),
                format!("failed to invoke prisma db pull: {error}"),
                vec![],
                Some(command),
                None,
                None,
                None,
            );
        }
    };
    if output.status_code != Some(0) {
        return failed_record(
            "prisma-introspection",
            suite_description("prisma-introspection"),
            start.elapsed(),
            format!(
                "prisma db pull exited with status {:?}",
                output.status_code.unwrap_or_default()
            ),
            vec!["schema_pull".to_owned()],
            Some(command),
            None,
            Some(output.stdout),
            Some(output.stderr),
        );
    }

    let schema_contents = match fs::read_to_string(&schema_path) {
        Ok(contents) => contents,
        Err(error) => {
            return failed_record(
                "prisma-introspection",
                suite_description("prisma-introspection"),
                start.elapsed(),
                format!("failed to read generated Prisma schema: {error}"),
                vec!["schema_pull".to_owned()],
                Some(command),
                Some(json!({ "schema_path": schema_path.display().to_string() })),
                Some(output.stdout),
                Some(output.stderr),
            );
        }
    };

    if !schema_contents.contains("xtask_prisma_users") {
        return failed_record(
            "prisma-introspection",
            suite_description("prisma-introspection"),
            start.elapsed(),
            "Prisma schema pull succeeded but the generated schema does not reference xtask_prisma_users".to_owned(),
            vec!["schema_pull".to_owned()],
            Some(command),
            Some(json!({ "schema_path": schema_path.display().to_string() })),
            Some(output.stdout),
            Some(output.stderr),
        );
    }

    passed_record(
        "prisma-introspection",
        suite_description("prisma-introspection"),
        start.elapsed(),
        "Prisma db pull introspected the live AionDB schema".to_owned(),
        vec!["schema_pull".to_owned(), "information_schema".to_owned()],
        Some(command),
        Some(json!({
            "schema_path": schema_path.display().to_string(),
            "contains_table_marker": true
        })),
        Some(output.stdout),
        Some(output.stderr),
    )
}

fn run_diesel_suite(ctx: &SuiteContext) -> SuiteRecord {
    let start = Instant::now();
    let Some(cargo) = find_in_path("cargo") else {
        return skipped_record(
            "diesel",
            suite_description("diesel"),
            start.elapsed(),
            "cargo was not found in PATH",
        );
    };
    let manifest_path = ctx
        .workspace_root
        .join("testing/ecosystem/diesel-smoke/Cargo.toml");
    let args = vec![
        "run".to_owned(),
        "-q".to_owned(),
        "--manifest-path".to_owned(),
        manifest_path.display().to_string(),
    ];
    run_json_script_command(
        "diesel",
        suite_description("diesel"),
        &cargo,
        &args,
        &[("DATABASE_URL", ctx.database_url.clone())],
        &ctx.workspace_root,
        start,
    )
    .unwrap_or_else(|record| *record)
}

fn run_python_json_suite(
    ctx: &SuiteContext,
    suite_name: &str,
    modules: &[&str],
    script_rel_path: &str,
    envs: &[(&str, String)],
) -> SuiteRecord {
    let start = Instant::now();
    let Some(python) = find_in_path("python3") else {
        return skipped_record(
            suite_name,
            suite_description(suite_name),
            start.elapsed(),
            "python3 was not found in PATH",
        );
    };

    if let Err(reason) = ensure_python_modules(&python, modules, &ctx.workspace_root) {
        return skipped_record(
            suite_name,
            suite_description(suite_name),
            start.elapsed(),
            reason,
        );
    }

    let script = ctx.workspace_root.join(script_rel_path);
    let args = vec![script.display().to_string()];
    match run_json_script_command(
        suite_name,
        suite_description(suite_name),
        &python,
        &args,
        envs,
        &ctx.workspace_root,
        start,
    ) {
        Ok(record) => record,
        Err(record) => *record,
    }
}

fn run_json_script_command(
    suite_name: &str,
    description: String,
    program: &Path,
    args: &[String],
    envs: &[(&str, String)],
    cwd: &Path,
    started_at: Instant,
) -> Result<SuiteRecord, Box<SuiteRecord>> {
    let command = command_display(program, args);
    let result = match run_command(program, args, envs, cwd) {
        Ok(output) => output,
        Err(error) => {
            return Err(Box::new(failed_record(
                suite_name,
                description.clone(),
                started_at.elapsed(),
                format!("failed to invoke command: {error}"),
                vec![],
                Some(command),
                None,
                None,
                None,
            )));
        }
    };

    if result.status_code != Some(0) {
        return Err(Box::new(failed_record(
            suite_name,
            description.clone(),
            started_at.elapsed(),
            format!(
                "command exited with status {:?}",
                result.status_code.unwrap_or_default()
            ),
            vec![],
            Some(command),
            None,
            Some(result.stdout),
            Some(result.stderr),
        )));
    }

    let parsed = match parse_json_script_output(&result, &command) {
        Ok(parsed) => parsed,
        Err(error) => {
            return Err(Box::new(failed_record(
                suite_name,
                description.clone(),
                started_at.elapsed(),
                error,
                vec![],
                Some(command),
                None,
                Some(result.stdout),
                Some(result.stderr),
            )));
        }
    };

    let checks = parsed
        .payload
        .get("checks")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let details = parsed
        .payload
        .get("details")
        .and_then(Value::as_str)
        .unwrap_or("driver script completed successfully")
        .to_owned();

    Ok(passed_record(
        suite_name,
        description,
        started_at.elapsed(),
        details,
        checks,
        Some(parsed.command),
        Some(parsed.payload),
        Some(parsed.stdout),
        Some(parsed.stderr),
    ))
}

fn parse_json_script_output(
    result: &CommandCapture,
    command: &str,
) -> Result<JsonScriptResult, String> {
    let payload = serde_json::from_str::<Value>(&result.stdout)
        .map_err(|error| format!("command produced non-JSON stdout: {error}"))?;
    Ok(JsonScriptResult {
        payload,
        stdout: result.stdout.clone(),
        stderr: result.stderr.clone(),
        command: command.to_owned(),
    })
}

fn ensure_python_modules(python: &Path, modules: &[&str], cwd: &Path) -> Result<(), String> {
    for module in modules {
        let args = vec!["-c".to_owned(), format!("import {module}")];
        let output = run_command(python, &args, &[], cwd)
            .map_err(|error| format!("failed to probe python module '{module}': {error}"))?;
        if output.status_code != Some(0) {
            return Err(format!(
                "python module '{module}' is unavailable: {}",
                stderr_or_stdout(&output)
            ));
        }
    }
    Ok(())
}

fn ensure_node_packages(node: &Path, cwd: &Path, packages: &[&str]) -> Result<(), String> {
    for package in packages {
        let args = vec![
            "-e".to_owned(),
            format!("require.resolve('{package}/package.json')"),
        ];
        let output = run_command(node, &args, &[], cwd)
            .map_err(|error| format!("failed to probe node package '{package}': {error}"))?;
        if output.status_code != Some(0) {
            return Err(format!(
                "node package '{package}' is unavailable: {}",
                stderr_or_stdout(&output)
            ));
        }
    }
    Ok(())
}

fn resolve_prisma_command(workspace_root: &Path) -> Option<PathBuf> {
    let compat_entry =
        workspace_root.join("target/compat/node-tools/node_modules/prisma/build/index.js");
    if compat_entry.is_file() {
        return Some(compat_entry);
    }
    let compat_local = workspace_root.join("target/compat/node-tools/node_modules/.bin/prisma");
    if compat_local.is_file() {
        return Some(compat_local);
    }
    let direct_entry = workspace_root.join("node_modules/prisma/build/index.js");
    if direct_entry.is_file() {
        return Some(direct_entry);
    }
    let local = workspace_root.join("node_modules/.bin/prisma");
    if local.is_file() {
        return Some(local);
    }
    find_in_path("prisma")
}

fn run_command(
    program: &Path,
    args: &[String],
    envs: &[(&str, String)],
    cwd: &Path,
) -> Result<CommandCapture, String> {
    let mut command = if program.extension().is_some_and(|ext| ext == "js") {
        let node = find_in_path("node")
            .ok_or_else(|| "node not found in PATH (needed to run .js scripts)".to_owned())?;
        let mut cmd = Command::new(node);
        cmd.arg(program);
        cmd
    } else {
        Command::new(program)
    };
    command.args(args).current_dir(cwd);
    for (key, value) in envs {
        command.env(key, value);
    }
    let output = command
        .output()
        .map_err(|error| format!("{}: {error}", command_display(program, args)))?;
    Ok(CommandCapture {
        status_code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    })
}

fn wait_for_server_ready(port: u16, error_slot: &Arc<Mutex<Option<String>>>) -> Result<(), String> {
    let deadline = Instant::now() + SERVER_READY_TIMEOUT;
    let addr = format!("{DEFAULT_HOST}:{port}");
    while Instant::now() < deadline {
        if TcpStream::connect(&addr).is_ok() {
            return Ok(());
        }
        if let Some(error) = error_slot.lock().expect("poisoned error slot").clone() {
            return Err(error);
        }
        thread::sleep(SERVER_READY_POLL_INTERVAL);
    }
    Err(format!(
        "timed out waiting for pgwire server on {addr} after {SERVER_READY_TIMEOUT:?}"
    ))
}

fn reserve_tcp_port() -> Result<u16, String> {
    let listener = TcpListener::bind((DEFAULT_HOST, 0))
        .map_err(|error| format!("failed to reserve tcp port on {DEFAULT_HOST}: {error}"))?;
    listener
        .local_addr()
        .map(|addr| addr.port())
        .map_err(|error| format!("failed to read reserved tcp port: {error}"))
}

fn run_engine_sql(engine: &Engine, application_name: &str, sql: &str) -> Result<(), String> {
    let (session, _) = engine
        .startup(startup_params(application_name))
        .map_err(|error| format!("engine startup failed: {error}"))?;
    let sql_result = engine
        .execute_sql(&session, sql)
        .map(|_| ())
        .map_err(|error| format!("engine execute_sql failed: {error}"));
    let terminate_result = engine
        .terminate(session)
        .map_err(|error| format!("engine terminate failed: {error}"));

    match (sql_result, terminate_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(()) | Err(_)) => Err(error),
        (Ok(()), Err(error)) => Err(error),
    }
}

fn startup_params(application_name: &str) -> StartupParams {
    StartupParams {
        database: DEFAULT_DATABASE.to_owned(),
        application_name: Some(application_name.to_owned()),
        options: BTreeMap::new(),
        credential: Credential::CleartextPassword {
            user: DEFAULT_USER.to_owned(),
            password: SecretString::new(DEFAULT_PASSWORD.to_owned()),
        },
        transport: TransportInfo::in_process(),
    }
}

fn workspace_root() -> Result<PathBuf, String> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "failed to determine workspace root from xtask manifest".to_owned())
}

fn write_report(path: &Path, report: &EcosystemCompatReport) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create report directory {}: {error}",
                parent.display()
            )
        })?;
    }
    let json = serde_json::to_string_pretty(report)
        .map_err(|error| format!("failed to serialize compatibility report: {error}"))?;
    fs::write(path, json)
        .map_err(|error| format!("failed to write report {}: {error}", path.display()))
}

fn append_history_entry(path: &Path, report: &EcosystemCompatReport) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "failed to create history directory {}: {error}",
                    parent.display()
                )
            })?;
        }
    }
    let entry = build_history_entry(report);
    let mut line = serde_json::to_string(&entry)
        .map_err(|error| format!("failed to serialize history entry: {error}"))?;
    line.push('\n');
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| {
            format!(
                "failed to open history file {} for append: {error}",
                path.display()
            )
        })?;
    file.write_all(line.as_bytes())
        .map_err(|error| format!("failed to write history entry: {error}"))
}

fn build_history_entry(report: &EcosystemCompatReport) -> Value {
    let suites: Vec<Value> = report
        .suites
        .iter()
        .map(|suite| {
            json!({
                "name": suite.name,
                "status": suite.status,
                "duration_ms": suite.duration_ms,
            })
        })
        .collect();
    json!({
        "generated_at_unix_ms": report.generated_at_unix_ms,
        "generated_at_iso8601": report.generated_at_iso8601,
        "git_commit_full": report.git_commit_full,
        "git_commit_short": report.git_commit_short,
        "git_dirty": report.git_dirty,
        "listen_addr": report.listen_addr,
        "strict": report.strict,
        "summary": {
            "passed": report.summary.passed,
            "failed": report.summary.failed,
            "skipped": report.summary.skipped,
        },
        "suites": suites,
    })
}

fn capture_commit_info(workspace_root: &Path) -> CommitInfo {
    let full = run_git_capture(workspace_root, &["rev-parse", "HEAD"])
        .map(|stdout| stdout.trim().to_owned())
        .filter(|sha| !sha.is_empty());
    let short = full
        .as_deref()
        .map(|sha| sha.chars().take(12).collect::<String>());
    let dirty = run_git_capture(workspace_root, &["status", "--porcelain"])
        .map(|stdout| !stdout.trim().is_empty());
    CommitInfo { full, short, dirty }
}

fn run_git_capture(cwd: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn format_iso8601_utc(unix_ms: u128) -> String {
    let total_secs = (unix_ms / 1_000) as i64;
    let ms = (unix_ms % 1_000) as u32;
    let secs_of_day = total_secs.rem_euclid(86_400) as u32;
    let h = secs_of_day / 3_600;
    let mi = (secs_of_day % 3_600) / 60;
    let s = secs_of_day % 60;
    let days = total_secs.div_euclid(86_400);
    let (y, mo, d) = civil_from_unix_days(days);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{ms:03}Z")
}

fn civil_from_unix_days(days: i64) -> (i64, u32, u32) {
    // Howard Hinnant's algorithm; see http://howardhinnant.github.io/date_algorithms.html.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d)
}

fn print_report_summary(report: &EcosystemCompatReport) {
    println!("=== AionDB ecosystem compatibility ===");
    println!("listen_addr: {}", report.listen_addr);
    println!("generated_at: {}", report.generated_at_iso8601);
    if let Some(short) = report.git_commit_short.as_deref() {
        let dirty_marker = match report.git_dirty {
            Some(true) => " (dirty)",
            _ => "",
        };
        println!("commit: {short}{dirty_marker}");
    }
    println!(
        "summary: {} passed, {} failed, {} skipped",
        report.summary.passed, report.summary.failed, report.summary.skipped
    );
    for suite in &report.suites {
        println!(
            "- {:<22} {:<7} {}",
            suite.name,
            match suite.status {
                SuiteStatus::Passed => "PASS",
                SuiteStatus::Failed => "FAIL",
                SuiteStatus::Skipped => "SKIP",
            },
            suite.details
        );
    }
    println!("report: {}", report.report_path);
    if let Some(history) = report.history_path.as_deref() {
        println!("history: {history}");
    }
}

fn suite_description(name: &str) -> String {
    suite_definitions()
        .iter()
        .find(|suite| suite.name == name)
        .map_or_else(
            || "unknown suite".to_owned(),
            |suite| suite.description.to_owned(),
        )
}

fn normalized_lines(output: &str) -> Vec<String> {
    output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn stderr_or_stdout(output: &CommandCapture) -> String {
    if output.stderr.is_empty() {
        output.stdout.clone()
    } else {
        output.stderr.clone()
    }
}

fn command_display(program: &Path, args: &[String]) -> String {
    let mut parts = Vec::with_capacity(args.len() + 1);
    parts.push(program.display().to_string());
    parts.extend(args.iter().cloned());
    parts.join(" ")
}

fn find_in_path(program: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    for dir in env::split_paths(&path) {
        let candidate = dir.join(program);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn unix_time_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis())
}

fn passed_record(
    name: &str,
    description: String,
    elapsed: Duration,
    details: String,
    checks: Vec<String>,
    command: Option<String>,
    payload: Option<Value>,
    stdout: Option<String>,
    stderr: Option<String>,
) -> SuiteRecord {
    build_record(
        name,
        description,
        SuiteStatus::Passed,
        elapsed,
        details,
        checks,
        command,
        payload,
        stdout,
        stderr,
    )
}

fn failed_record(
    name: &str,
    description: String,
    elapsed: Duration,
    details: String,
    checks: Vec<String>,
    command: Option<String>,
    payload: Option<Value>,
    stdout: Option<String>,
    stderr: Option<String>,
) -> SuiteRecord {
    build_record(
        name,
        description,
        SuiteStatus::Failed,
        elapsed,
        details,
        checks,
        command,
        payload,
        stdout,
        stderr,
    )
}

fn skipped_record(
    name: &str,
    description: String,
    elapsed: Duration,
    details: impl Into<String>,
) -> SuiteRecord {
    build_record(
        name,
        description,
        SuiteStatus::Skipped,
        elapsed,
        details.into(),
        vec![],
        None,
        None,
        None,
        None,
    )
}

fn build_record(
    name: &str,
    description: String,
    status: SuiteStatus,
    elapsed: Duration,
    details: String,
    checks: Vec<String>,
    command: Option<String>,
    payload: Option<Value>,
    stdout: Option<String>,
    stderr: Option<String>,
) -> SuiteRecord {
    SuiteRecord {
        name: name.to_owned(),
        description,
        status,
        duration_ms: elapsed.as_millis(),
        details,
        checks,
        command,
        payload,
        stdout,
        stderr,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_accepts_suite_and_report() {
        let opts = parse_args(&[
            "--suite".to_owned(),
            "psql-libpq".to_owned(),
            "--report".to_owned(),
            "tmp/report.json".to_owned(),
            "--strict".to_owned(),
        ])
        .expect("args should parse");

        assert_eq!(opts.suite_filter.as_deref(), Some("psql-libpq"));
        assert_eq!(opts.report_path, PathBuf::from("tmp/report.json"));
        assert!(opts.strict);
        assert!(!opts.list_only);
        assert_eq!(opts.history_path, Some(PathBuf::from(DEFAULT_HISTORY_PATH)));
    }

    #[test]
    fn parse_args_rejects_unknown_suite() {
        let error = parse_args(&["--suite".to_owned(), "missing".to_owned()])
            .expect_err("unknown suite should fail");
        assert!(error.contains("unknown suite"));
    }

    #[test]
    fn parse_args_history_override_and_disable() {
        let opts = parse_args(&[
            "--history".to_owned(),
            "tmp/custom_history.jsonl".to_owned(),
        ])
        .expect("args should parse");
        assert_eq!(
            opts.history_path,
            Some(PathBuf::from("tmp/custom_history.jsonl"))
        );

        let opts = parse_args(&["--no-history".to_owned()]).expect("args should parse");
        assert!(opts.history_path.is_none());

        let opts = parse_args(&[
            "--history".to_owned(),
            "tmp/custom_history.jsonl".to_owned(),
            "--no-history".to_owned(),
        ])
        .expect("args should parse");
        assert!(
            opts.history_path.is_none(),
            "trailing --no-history must clear history"
        );
    }

    #[test]
    fn parse_args_defaults_use_clean_prod_paths() {
        let opts = parse_args(&[]).expect("default args should parse");
        assert_eq!(opts.report_path, PathBuf::from(DEFAULT_REPORT_PATH));
        assert_eq!(opts.history_path, Some(PathBuf::from(DEFAULT_HISTORY_PATH)));
        assert_eq!(DEFAULT_REPORT_PATH, "tmp/clean_prod/ecosystem_compat.json");
        assert_eq!(
            DEFAULT_HISTORY_PATH,
            "tmp/clean_prod/ecosystem_compat_history.jsonl"
        );
    }

    #[test]
    fn normalized_lines_ignores_blank_rows() {
        assert_eq!(
            normalized_lines("alice\n\n bob \n"),
            vec!["alice".to_owned(), "bob".to_owned()]
        );
    }

    #[test]
    fn iso8601_utc_formats_known_epochs() {
        assert_eq!(format_iso8601_utc(0), "1970-01-01T00:00:00.000Z");
        // 2026-05-03T12:34:56.789Z = 20576 days * 86400 + 45296 seconds + 789 ms.
        let unix_ms: u128 = 1_777_811_696_789;
        assert_eq!(format_iso8601_utc(unix_ms), "2026-05-03T12:34:56.789Z");
        // 2000-02-29 leap day handling.
        let leap_ms: u128 = 951_782_400_000;
        assert_eq!(format_iso8601_utc(leap_ms), "2000-02-29T00:00:00.000Z");
    }

    #[test]
    fn history_entry_records_summary_and_per_suite_status() {
        let report = EcosystemCompatReport {
            generated_at_unix_ms: 1_000,
            generated_at_iso8601: "1970-01-01T00:00:01.000Z".to_owned(),
            git_commit_full: Some("abcdef0123456789abcdef0123456789abcdef01".to_owned()),
            git_commit_short: Some("abcdef012345".to_owned()),
            git_dirty: Some(false),
            listen_addr: "127.0.0.1:55555".to_owned(),
            strict: true,
            report_path: "tmp/clean_prod/ecosystem_compat.json".to_owned(),
            history_path: Some("tmp/clean_prod/ecosystem_compat_history.jsonl".to_owned()),
            summary: Summary {
                passed: 1,
                failed: 0,
                skipped: 1,
            },
            suites: vec![
                SuiteRecord {
                    name: "psql-libpq".to_owned(),
                    description: "psql".to_owned(),
                    status: SuiteStatus::Passed,
                    duration_ms: 42,
                    details: String::new(),
                    checks: vec![],
                    command: None,
                    payload: None,
                    stdout: None,
                    stderr: None,
                },
                SuiteRecord {
                    name: "diesel".to_owned(),
                    description: "diesel".to_owned(),
                    status: SuiteStatus::Skipped,
                    duration_ms: 7,
                    details: String::new(),
                    checks: vec![],
                    command: None,
                    payload: None,
                    stdout: None,
                    stderr: None,
                },
            ],
        };

        let entry = build_history_entry(&report);
        assert_eq!(entry["summary"]["passed"], json!(1));
        assert_eq!(entry["summary"]["failed"], json!(0));
        assert_eq!(entry["summary"]["skipped"], json!(1));
        assert_eq!(entry["git_commit_short"], json!("abcdef012345"));
        assert_eq!(entry["git_dirty"], json!(false));
        assert_eq!(
            entry["generated_at_iso8601"],
            json!("1970-01-01T00:00:01.000Z")
        );
        let suites = entry["suites"].as_array().expect("suites array");
        assert_eq!(suites.len(), 2);
        assert_eq!(suites[0]["name"], json!("psql-libpq"));
        assert_eq!(suites[0]["status"], json!("passed"));
        assert_eq!(suites[1]["status"], json!("skipped"));
        assert_eq!(suites[0]["duration_ms"], json!(42));
    }

    #[test]
    fn append_history_entry_writes_one_line_per_run() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("nested/history.jsonl");
        let report = EcosystemCompatReport {
            generated_at_unix_ms: 1,
            generated_at_iso8601: "1970-01-01T00:00:00.001Z".to_owned(),
            git_commit_full: None,
            git_commit_short: None,
            git_dirty: None,
            listen_addr: "127.0.0.1:0".to_owned(),
            strict: false,
            report_path: "/dev/null".to_owned(),
            history_path: Some(path.display().to_string()),
            summary: Summary {
                passed: 0,
                failed: 0,
                skipped: 0,
            },
            suites: vec![],
        };

        append_history_entry(&path, &report).expect("first append");
        append_history_entry(&path, &report).expect("second append");
        let contents = fs::read_to_string(&path).expect("read history");
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2, "two appends must produce two lines");
        for line in lines {
            let parsed: Value = serde_json::from_str(line).expect("each line is valid json");
            assert_eq!(
                parsed["generated_at_iso8601"],
                json!("1970-01-01T00:00:00.001Z")
            );
        }
    }
}
