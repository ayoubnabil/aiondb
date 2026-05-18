use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
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
const BOLT_SMOKE_PASSWORD: &str = "Xtask-bolt-smoke1";
const DEFAULT_HOST: &str = "127.0.0.1";
const NEO4J_JS_DRIVER_BASE_ENV: &str = "AIONDB_NEO4J_JS_DRIVER_BASE";
const NEO4J_JAVA_DRIVER_JAR_ENV: &str = "AIONDB_NEO4J_JAVA_DRIVER_JAR";
const CYPHER_SHELL_PATH_ENV: &str = "AIONDB_CYPHER_SHELL";
const NEO4J_BOLT_LIMITATIONS_REF: &str =
    "docs/content/documentation/connect/ecosystem-integrations.md#current-bolt-compatibility-limitations";
const NEO4J_BOLT_SURFACE: &str = "bolt_compat";
const SERVER_READY_TIMEOUT: Duration = Duration::from_secs(10);
const SERVER_READY_POLL_INTERVAL: Duration = Duration::from_millis(50);
const EXTERNAL_SERVER_READY_TIMEOUT: Duration = Duration::from_secs(30);
const TCP_PORT_RESERVE_ATTEMPTS: usize = 8;
const TCP_PORT_RESERVE_RETRY_DELAY: Duration = Duration::from_millis(50);

#[derive(Debug)]
pub(crate) struct EcosystemCompatOptions {
    suite_filter: Option<String>,
    group_filter: Option<String>,
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
    pgwire_port: u16,
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

struct ExternalBoltHarness {
    child: std::process::Child,
    bolt_port: u16,
}

struct ExternalQueryApiHarness {
    child: std::process::Child,
    http_port: u16,
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
    group_filter: Option<String>,
    group_summary: Option<GroupSummary>,
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

#[derive(Debug, Serialize)]
struct GroupSummary {
    name: String,
    group_status: &'static str,
    suites_total: usize,
    passed: usize,
    failed: usize,
    skipped: usize,
    experimental_passed: usize,
    target_pending_provisioning: usize,
    blocked_suites: Vec<String>,
    fully_validated: bool,
    provisioning_hints: Vec<ProvisioningHint>,
}

#[derive(Debug, Serialize)]
struct ProvisioningHint {
    suite: String,
    tool: Option<String>,
    env: String,
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
    let mut group_filter = None;
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
            "--group" => {
                i += 1;
                group_filter = Some(
                    args.get(i)
                        .ok_or_else(|| "--group requires a value".to_owned())?
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

    if suite_filter.is_some() && group_filter.is_some() {
        return Err("use either --suite or --group, not both".to_owned());
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

    if let Some(name) = &group_filter {
        if !matches!(
            name.as_str(),
            "neo4j-p0" | "neo4j-http-p1" | "neo4j-browser-p0"
        ) {
            return Err(format!(
                "unknown group '{name}'. Valid groups: neo4j-p0, neo4j-http-p1, neo4j-browser-p0"
            ));
        }
    }

    let history_path = if history_disabled {
        None
    } else {
        Some(history_override.unwrap_or_else(|| PathBuf::from(DEFAULT_HISTORY_PATH)))
    };

    Ok(EcosystemCompatOptions {
        suite_filter,
        group_filter,
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
        println!(
            "{:<22} {}",
            "group:neo4j-p0",
            "Neo4j P0 wave: python driver, javascript driver, java driver, cypher-shell"
        );
        println!(
            "{:<22} {}",
            "group:neo4j-http-p1",
            "Neo4j HTTP P1 wave: query API discovery, query, parameters, transactions"
        );
        println!(
            "{:<22} {}",
            "group:neo4j-browser-p0",
            "Neo4j Browser P0 wave: Bolt procedure preflight compatibility"
        );
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
        pgwire_port: harness.port,
        engine: Arc::clone(&harness.engine),
        scratch_dir: scratch.path().to_path_buf(),
    };

    let suites: Vec<SuiteDefinition> = suite_definitions()
        .iter()
        .copied()
        .filter(|suite| match (opts.suite_filter.as_ref(), opts.group_filter.as_ref()) {
            (Some(name), _) => suite.name == name.as_str(),
            (None, Some(group)) if group == "neo4j-p0" => matches!(
                suite.name,
                "neo4j-python-bolt"
                    | "neo4j-javascript-bolt"
                    | "neo4j-java-bolt"
                    | "cypher-shell-bolt"
            ),
            (None, Some(group)) if group == "neo4j-http-p1" => {
                matches!(suite.name, "neo4j-query-api-http")
            }
            (None, Some(group)) if group == "neo4j-browser-p0" => {
                matches!(suite.name, "neo4j-browser-preflight-bolt")
            }
            (None, Some(_)) => false,
            (None, None) => true,
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
    let group_summary = build_group_summary(opts.group_filter.as_deref(), &records);

    let generated_at_unix_ms = unix_time_millis();
    let commit_info = capture_commit_info(&workspace_root);

    let report = EcosystemCompatReport {
        generated_at_unix_ms,
        generated_at_iso8601: format_iso8601_utc(generated_at_unix_ms),
        git_commit_full: commit_info.full.clone(),
        git_commit_short: commit_info.short.clone(),
        git_dirty: commit_info.dirty,
        listen_addr: format!("{DEFAULT_HOST}:{}", harness.port),
        group_filter: opts.group_filter.clone(),
        group_summary,
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
  --group <NAME>    Run a named suite group (currently: neo4j-p0, neo4j-http-p1, neo4j-browser-p0)
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
        SuiteDefinition {
            name: "neo4j-python-bolt",
            description: "Neo4j Python driver read-only Bolt smoke against the experimental compatibility listener",
            run: run_neo4j_python_bolt_suite,
        },
        SuiteDefinition {
            name: "neo4j-javascript-bolt",
            description: "Neo4j JavaScript driver read-only Bolt smoke against the experimental compatibility listener",
            run: run_neo4j_javascript_bolt_suite,
        },
        SuiteDefinition {
            name: "neo4j-query-api-http",
            description: "Neo4j Query API HTTP wrapper smoke against the experimental compatibility surface",
            run: run_neo4j_query_api_http_suite,
        },
        SuiteDefinition {
            name: "neo4j-java-bolt",
            description: "Neo4j Java driver read-only Bolt smoke against the experimental compatibility listener",
            run: run_neo4j_java_bolt_suite,
        },
        SuiteDefinition {
            name: "cypher-shell-bolt",
            description: "cypher-shell read-only Bolt smoke against the experimental compatibility listener",
            run: run_cypher_shell_bolt_suite,
        },
        SuiteDefinition {
            name: "neo4j-browser-preflight-bolt",
            description: "Browser-oriented Bolt procedure preflight smoke over cypher-shell against the experimental compatibility listener",
            run: run_neo4j_browser_preflight_bolt_suite,
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

fn run_neo4j_python_bolt_suite(ctx: &SuiteContext) -> SuiteRecord {
    let start = Instant::now();
    let Some(python) = find_in_path("python3") else {
        return skipped_record(
            "neo4j-python-bolt",
            suite_description("neo4j-python-bolt"),
            start.elapsed(),
            "python3 was not found in PATH",
        );
    };
    if let Err(reason) = ensure_python_modules(&python, &["neo4j"], &ctx.workspace_root) {
        return skipped_record(
            "neo4j-python-bolt",
            suite_description("neo4j-python-bolt"),
            start.elapsed(),
            reason,
        );
    }
    let driver_version = capture_python_module_version(&python, "neo4j", &ctx.workspace_root);

    let mut harness = match start_external_bolt_harness(ctx, "neo4j-python-bolt", start) {
        Ok(harness) => harness,
        Err(record) => return *record,
    };

    let script = ctx
        .workspace_root
        .join("testing/ecosystem/python/neo4j_bolt_smoke.py");
    let args = vec![script.display().to_string()];
    let envs = vec![
        (
            "NEO4J_URI",
            format!("bolt://{DEFAULT_HOST}:{}", harness.bolt_port),
        ),
        ("NEO4J_USER", DEFAULT_USER.to_owned()),
        ("NEO4J_PASSWORD", BOLT_SMOKE_PASSWORD.to_owned()),
    ];

    let result = run_json_script_command(
        "neo4j-python-bolt",
        suite_description("neo4j-python-bolt"),
        &python,
        &args,
        &envs,
        &ctx.workspace_root,
        start,
    );
    let _ = stop_child_process(&mut harness.child);
    let mut record = result.unwrap_or_else(|record| *record);
    attach_payload_metadata(
        &mut record,
        json!({
            "tool": "neo4j-python-driver",
            "driver_version": driver_version,
            "validation_state": "experimental",
            "surface": NEO4J_BOLT_SURFACE,
            "limitations_ref": NEO4J_BOLT_LIMITATIONS_REF,
        }),
    );
    record
}

fn run_neo4j_javascript_bolt_suite(ctx: &SuiteContext) -> SuiteRecord {
    let start = Instant::now();
    let Some(node) = find_in_path("node") else {
        return skipped_record(
            "neo4j-javascript-bolt",
            suite_description("neo4j-javascript-bolt"),
            start.elapsed(),
            "node was not found in PATH",
        );
    };
    let node_resolve_base = resolve_node_package_base(&ctx.workspace_root);
    if let Err(reason) = ensure_node_packages(&node, &node_resolve_base, &["neo4j-driver"]) {
        let mut record = skipped_record(
            "neo4j-javascript-bolt",
            suite_description("neo4j-javascript-bolt"),
            start.elapsed(),
            reason,
        );
        attach_payload_metadata(
            &mut record,
            json!({
                "tool": "neo4j-javascript-driver",
                "driver_version": Value::Null,
                "node_resolve_base": node_resolve_base.display().to_string(),
                "provisioning_env": NEO4J_JS_DRIVER_BASE_ENV,
                "validation_state": "target",
                "surface": NEO4J_BOLT_SURFACE,
                "limitations_ref": NEO4J_BOLT_LIMITATIONS_REF,
            }),
        );
        return record;
    }
    let driver_version = capture_node_package_version(&node, &node_resolve_base, "neo4j-driver");

    let mut harness = match start_external_bolt_harness(ctx, "neo4j-javascript-bolt", start) {
        Ok(harness) => harness,
        Err(record) => return *record,
    };

    let script = ctx
        .workspace_root
        .join("testing/ecosystem/node/neo4j_driver_smoke.mjs");
    let args = vec![script.display().to_string()];
    let envs = vec![
        (
            "NEO4J_URI",
            format!("bolt://{DEFAULT_HOST}:{}", harness.bolt_port),
        ),
        ("NEO4J_USER", DEFAULT_USER.to_owned()),
        ("NEO4J_PASSWORD", BOLT_SMOKE_PASSWORD.to_owned()),
        (
            "AIONDB_NODE_RESOLVE_BASE",
            node_resolve_base.display().to_string(),
        ),
    ];
    let result = run_json_script_command(
        "neo4j-javascript-bolt",
        suite_description("neo4j-javascript-bolt"),
        &node,
        &args,
        &envs,
        &ctx.workspace_root,
        start,
    );
    let _ = stop_child_process(&mut harness.child);
    let mut record = result.unwrap_or_else(|record| *record);
    let validation_state = if matches!(record.status, SuiteStatus::Passed) {
        "experimental"
    } else {
        "target"
    };
    attach_payload_metadata(
        &mut record,
        json!({
            "tool": "neo4j-javascript-driver",
            "driver_version": driver_version,
            "node_resolve_base": node_resolve_base.display().to_string(),
            "validation_state": validation_state,
            "surface": NEO4J_BOLT_SURFACE,
            "limitations_ref": NEO4J_BOLT_LIMITATIONS_REF,
        }),
    );
    record
}

fn run_neo4j_query_api_http_suite(ctx: &SuiteContext) -> SuiteRecord {
    let start = Instant::now();
    let Some(python) = find_in_path("python3") else {
        return skipped_record(
            "neo4j-query-api-http",
            suite_description("neo4j-query-api-http"),
            start.elapsed(),
            "python3 was not found in PATH",
        );
    };

    let mut harness = match start_external_query_api_harness(ctx, "neo4j-query-api-http", start) {
        Ok(harness) => harness,
        Err(record) => return *record,
    };

    let script = ctx
        .workspace_root
        .join("testing/ecosystem/python/neo4j_query_api_smoke.py");
    let args = vec![script.display().to_string()];
    let envs = vec![
        (
            "AIONDB_QUERY_API_BASE",
            format!("http://{DEFAULT_HOST}:{}", harness.http_port),
        ),
        ("AIONDB_QUERY_API_USER", DEFAULT_USER.to_owned()),
        ("AIONDB_QUERY_API_PASSWORD", BOLT_SMOKE_PASSWORD.to_owned()),
    ];

    let result = run_json_script_command(
        "neo4j-query-api-http",
        suite_description("neo4j-query-api-http"),
        &python,
        &args,
        &envs,
        &ctx.workspace_root,
        start,
    );
    let _ = stop_child_process(&mut harness.child);
    let mut record = result.unwrap_or_else(|record| *record);
    attach_payload_metadata(
        &mut record,
        json!({
            "tool": "neo4j-query-api-wrapper",
            "validation_state": "experimental",
            "surface": "query_api_wrapper",
            "limitations_ref": "docs/content/documentation/connect/ecosystem-integrations.md#current-query-api-compatibility-limitations",
        }),
    );
    record
}

fn run_cypher_shell_bolt_suite(ctx: &SuiteContext) -> SuiteRecord {
    let start = Instant::now();
    let Some(cypher_shell) = resolve_program_override(CYPHER_SHELL_PATH_ENV, "cypher-shell") else {
        let mut record = skipped_record(
            "cypher-shell-bolt",
            suite_description("cypher-shell-bolt"),
            start.elapsed(),
            format!(
                "cypher-shell was not found in PATH; set {CYPHER_SHELL_PATH_ENV} to a local client binary to provision this smoke"
            ),
        );
        attach_payload_metadata(
            &mut record,
            json!({
                "tool": "cypher-shell",
                "tool_version": Value::Null,
                "provisioning_env": CYPHER_SHELL_PATH_ENV,
                "validation_state": "target",
                "surface": NEO4J_BOLT_SURFACE,
                "limitations_ref": NEO4J_BOLT_LIMITATIONS_REF,
            }),
        );
        return record;
    };
    let cypher_shell_version =
        capture_command_first_line(&cypher_shell, &[String::from("--version")], &ctx.workspace_root);

    let mut harness = match start_external_bolt_harness(ctx, "cypher-shell-bolt", start) {
        Ok(harness) => harness,
        Err(record) => return *record,
    };

    let query = "SELECT 1 AS one, 'ok' AS status";
    let args = vec![
        "-a".to_owned(),
        format!("bolt://{DEFAULT_HOST}:{}", harness.bolt_port),
        "-u".to_owned(),
        DEFAULT_USER.to_owned(),
        "-p".to_owned(),
        BOLT_SMOKE_PASSWORD.to_owned(),
        "--non-interactive".to_owned(),
        "--format".to_owned(),
        "plain".to_owned(),
        query.to_owned(),
    ];
    let command = command_display(&cypher_shell, &args);
    let output = run_command(&cypher_shell, &args, &[], &ctx.workspace_root);
    let _ = stop_child_process(&mut harness.child);
    let elapsed = start.elapsed();
    let output = match output {
        Ok(output) => output,
        Err(error) => {
            return failed_record(
                "cypher-shell-bolt",
                suite_description("cypher-shell-bolt"),
                elapsed,
                format!("failed to execute cypher-shell: {error}"),
                vec!["bolt_connect".to_owned()],
                Some(command),
                None,
                None,
                None,
            );
        }
    };
    if output.status_code != Some(0) {
        return failed_record(
            "cypher-shell-bolt",
            suite_description("cypher-shell-bolt"),
            elapsed,
            format!(
                "cypher-shell exited with status {:?}: {}",
                output.status_code,
                stderr_or_stdout(&output)
            ),
            vec!["bolt_connect".to_owned(), "auth".to_owned()],
            Some(command),
            None,
            Some(output.stdout),
            Some(output.stderr),
        );
    }

    let stdout_lines = normalized_lines(&output.stdout);
    let stdout_joined = stdout_lines.join(" ");
    if !stdout_joined.contains('1') || !stdout_joined.contains("ok") {
        return failed_record(
            "cypher-shell-bolt",
            suite_description("cypher-shell-bolt"),
            elapsed,
            format!("unexpected cypher-shell output: {}", output.stdout.trim()),
            vec![
                "bolt_connect".to_owned(),
                "auth".to_owned(),
                "session".to_owned(),
                "return_probe".to_owned(),
            ],
            Some(command),
            None,
            Some(output.stdout),
            Some(output.stderr),
        );
    }

    passed_record(
        "cypher-shell-bolt",
        suite_description("cypher-shell-bolt"),
        elapsed,
        "cypher-shell connected over Bolt, completed a db.ping compatibility check, and completed a read-only SQL probe".to_owned(),
        vec![
            "bolt_connect".to_owned(),
            "auth".to_owned(),
            "db_ping".to_owned(),
            "session".to_owned(),
            "sql_probe".to_owned(),
        ],
        Some(command),
        Some(json!({
            "tool": "cypher-shell",
            "tool_version": cypher_shell_version,
            "validation_state": "experimental",
            "surface": NEO4J_BOLT_SURFACE,
            "limitations_ref": NEO4J_BOLT_LIMITATIONS_REF,
            "checks": ["bolt_connect", "auth", "db_ping", "session", "sql_probe"],
            "stdout_lines": stdout_lines,
        })),
        Some(output.stdout),
        Some(output.stderr),
    )
}

fn run_neo4j_browser_preflight_bolt_suite(ctx: &SuiteContext) -> SuiteRecord {
    let start = Instant::now();
    let Some(cypher_shell) = resolve_program_override(CYPHER_SHELL_PATH_ENV, "cypher-shell") else {
        let mut record = skipped_record(
            "neo4j-browser-preflight-bolt",
            suite_description("neo4j-browser-preflight-bolt"),
            start.elapsed(),
            format!(
                "cypher-shell was not found in PATH; set {CYPHER_SHELL_PATH_ENV} to a local client binary to provision this Browser preflight smoke"
            ),
        );
        attach_payload_metadata(
            &mut record,
            json!({
                "tool": "neo4j-browser",
                "validation_state": "target",
                "surface": NEO4J_BOLT_SURFACE,
                "limitations_ref": NEO4J_BOLT_LIMITATIONS_REF,
                "provisioning_env": CYPHER_SHELL_PATH_ENV,
                "evidence_kind": "browser_preflight_procedures",
            }),
        );
        return record;
    };

    let cypher_shell_version =
        capture_command_first_line(&cypher_shell, &[String::from("--version")], &ctx.workspace_root);
    let mut harness = match start_external_bolt_harness(ctx, "neo4j-browser-preflight-bolt", start) {
        Ok(harness) => harness,
        Err(record) => return *record,
    };

    let probes = [
        ("dbms_components", "CALL dbms.components()"),
        (
            "dbms_components_yield",
            "CALL dbms.components() YIELD name, versions, edition RETURN name, versions, edition",
        ),
        ("db_labels", "CALL db.labels()"),
        ("db_labels_yield", "CALL db.labels() YIELD label RETURN label"),
        ("db_relationship_types", "CALL db.relationshipTypes()"),
        (
            "db_relationship_types_yield",
            "CALL db.relationshipTypes() YIELD relationshipType RETURN relationshipType",
        ),
        ("db_property_keys", "CALL db.propertyKeys()"),
        (
            "db_property_keys_yield",
            "CALL db.propertyKeys() YIELD propertyKey RETURN propertyKey",
        ),
    ];
    let mut commands = Vec::with_capacity(probes.len());
    let mut outputs = BTreeMap::new();
    let mut checks = vec!["bolt_connect".to_owned(), "auth".to_owned(), "session".to_owned()];

    for (check_name, query) in probes {
        let args = vec![
            "-a".to_owned(),
            format!("bolt://{DEFAULT_HOST}:{}", harness.bolt_port),
            "-u".to_owned(),
            DEFAULT_USER.to_owned(),
            "-p".to_owned(),
            BOLT_SMOKE_PASSWORD.to_owned(),
            "--non-interactive".to_owned(),
            "--format".to_owned(),
            "plain".to_owned(),
            query.to_owned(),
        ];
        commands.push(command_display(&cypher_shell, &args));
        let output = match run_command(&cypher_shell, &args, &[], &ctx.workspace_root) {
            Ok(output) => output,
            Err(error) => {
                let _ = stop_child_process(&mut harness.child);
                return failed_record(
                    "neo4j-browser-preflight-bolt",
                    suite_description("neo4j-browser-preflight-bolt"),
                    start.elapsed(),
                    format!("failed to execute cypher-shell Browser preflight probe: {error}"),
                    checks,
                    None,
                    None,
                    None,
                    None,
                );
            }
        };
        if output.status_code != Some(0) {
            let _ = stop_child_process(&mut harness.child);
            return failed_record(
                "neo4j-browser-preflight-bolt",
                suite_description("neo4j-browser-preflight-bolt"),
                start.elapsed(),
                format!(
                    "Browser preflight probe `{query}` failed with status {:?}: {}",
                    output.status_code,
                    stderr_or_stdout(&output)
                ),
                checks,
                None,
                None,
                Some(output.stdout),
                Some(output.stderr),
            );
        }
        outputs.insert(
            check_name.to_owned(),
            json!({
                "query": query,
                "stdout_lines": normalized_lines(&output.stdout),
            }),
        );
        checks.push(check_name.to_owned());
    }

    let _ = stop_child_process(&mut harness.child);
    if !outputs["dbms_components"]["stdout_lines"]
        .as_array()
        .is_some_and(|lines| lines.iter().any(|line| line.as_str().is_some_and(|line| line.contains("Neo4j Kernel"))))
    {
        return failed_record(
            "neo4j-browser-preflight-bolt",
            suite_description("neo4j-browser-preflight-bolt"),
            start.elapsed(),
            "dbms.components output did not expose the expected compatibility row".to_owned(),
            checks,
            None,
            None,
            None,
            None,
        );
    }
    if !outputs["dbms_components_yield"]["stdout_lines"]
        .as_array()
        .is_some_and(|lines| lines.iter().any(|line| line.as_str().is_some_and(|line| line.contains("Neo4j Kernel"))))
    {
        return failed_record(
            "neo4j-browser-preflight-bolt",
            suite_description("neo4j-browser-preflight-bolt"),
            start.elapsed(),
            "dbms.components YIELD/RETURN projection did not expose the expected compatibility row"
                .to_owned(),
            checks,
            None,
            None,
            None,
            None,
        );
    }

    passed_record(
        "neo4j-browser-preflight-bolt",
        suite_description("neo4j-browser-preflight-bolt"),
        start.elapsed(),
        "Browser-oriented Bolt preflight procedures completed over cypher-shell".to_owned(),
        checks,
        None,
        Some(json!({
            "tool": "neo4j-browser",
            "tool_version": Value::Null,
            "driver_tool": "cypher-shell",
            "driver_tool_version": cypher_shell_version,
            "validation_state": "target",
            "surface": NEO4J_BOLT_SURFACE,
            "limitations_ref": NEO4J_BOLT_LIMITATIONS_REF,
            "evidence_kind": "browser_preflight_procedures",
            "queries": probes.iter().map(|(_, query)| *query).collect::<Vec<_>>(),
            "outputs": outputs,
            "commands": commands,
        })),
        None,
        None,
    )
}

fn run_neo4j_java_bolt_suite(ctx: &SuiteContext) -> SuiteRecord {
    let start = Instant::now();
    let Some(java) = find_in_path("java") else {
        return skipped_record(
            "neo4j-java-bolt",
            suite_description("neo4j-java-bolt"),
            start.elapsed(),
            "java was not found in PATH",
        );
    };
    let Some(javac) = find_in_path("javac") else {
        return skipped_record(
            "neo4j-java-bolt",
            suite_description("neo4j-java-bolt"),
            start.elapsed(),
            "javac was not found in PATH",
        );
    };
    let Some(driver_jar) = resolve_neo4j_java_driver_jar(&ctx.workspace_root) else {
        let mut record = skipped_record(
            "neo4j-java-bolt",
            suite_description("neo4j-java-bolt"),
            start.elapsed(),
            format!(
                "neo4j-java-driver jar was not found; set {NEO4J_JAVA_DRIVER_JAR_ENV} or provision target/compat/java-libs or ~/.m2 first"
            ),
        );
        attach_payload_metadata(
            &mut record,
            json!({
                "tool": "neo4j-java-driver",
                "driver_version": Value::Null,
                "driver_jar": Value::Null,
                "provisioning_env": NEO4J_JAVA_DRIVER_JAR_ENV,
                "validation_state": "target",
                "surface": NEO4J_BOLT_SURFACE,
                "limitations_ref": NEO4J_BOLT_LIMITATIONS_REF,
            }),
        );
        return record;
    };
    let driver_version = infer_neo4j_java_driver_version(&driver_jar);

    let build_dir = ctx.workspace_root.join("target/compat/java-build/neo4j-bolt-smoke");
    if let Err(error) = fs::create_dir_all(&build_dir) {
        return failed_record(
            "neo4j-java-bolt",
            suite_description("neo4j-java-bolt"),
            start.elapsed(),
            format!("failed to create java build dir '{}': {error}", build_dir.display()),
            vec![],
            None,
            None,
            None,
            None,
        );
    }

    let source = ctx
        .workspace_root
        .join("testing/ecosystem/java/Neo4jBoltSmoke.java");
    let driver_classpath = java_classpath_entries(&driver_jar);
    let driver_classpath_string = driver_classpath.join(":");
    let compile_args = vec![
        "-d".to_owned(),
        build_dir.display().to_string(),
        "-cp".to_owned(),
        driver_classpath_string.clone(),
        source.display().to_string(),
    ];
    let compile_command = command_display(&javac, &compile_args);
    let compile_output = match run_command(&javac, &compile_args, &[], &ctx.workspace_root) {
        Ok(output) => output,
        Err(error) => {
            return failed_record(
                "neo4j-java-bolt",
                suite_description("neo4j-java-bolt"),
                start.elapsed(),
                format!("failed to execute javac: {error}"),
                vec!["compile".to_owned()],
                Some(compile_command),
                None,
                None,
                None,
            );
        }
    };
    if compile_output.status_code != Some(0) {
        return failed_record(
            "neo4j-java-bolt",
            suite_description("neo4j-java-bolt"),
            start.elapsed(),
            format!(
                "javac exited with status {:?}: {}",
                compile_output.status_code,
                stderr_or_stdout(&compile_output)
            ),
            vec!["compile".to_owned()],
            Some(compile_command),
            None,
            Some(compile_output.stdout),
            Some(compile_output.stderr),
        );
    }

    let mut harness = match start_external_bolt_harness(ctx, "neo4j-java-bolt", start) {
        Ok(harness) => harness,
        Err(record) => return *record,
    };

    let classpath = format!("{}:{}", build_dir.display(), driver_classpath_string);
    let args = vec![
        "-cp".to_owned(),
        classpath.clone(),
        "Neo4jBoltSmoke".to_owned(),
        format!("bolt://{DEFAULT_HOST}:{}", harness.bolt_port),
        DEFAULT_USER.to_owned(),
        BOLT_SMOKE_PASSWORD.to_owned(),
    ];
    let command = command_display(&java, &args);
    let output = run_command(&java, &args, &[], &ctx.workspace_root);
    let _ = stop_child_process(&mut harness.child);
    let elapsed = start.elapsed();
    let output = match output {
        Ok(output) => output,
        Err(error) => {
            return failed_record(
                "neo4j-java-bolt",
                suite_description("neo4j-java-bolt"),
                elapsed,
                format!("failed to execute java smoke runner: {error}"),
                vec!["compile".to_owned(), "bolt_connect".to_owned()],
                Some(command),
                None,
                None,
                None,
            );
        }
    };
    if output.status_code != Some(0) {
        return failed_record(
            "neo4j-java-bolt",
            suite_description("neo4j-java-bolt"),
            elapsed,
            format!(
                "neo4j-java-driver smoke exited with status {:?}: {}",
                output.status_code,
                stderr_or_stdout(&output)
            ),
            vec![
                "compile".to_owned(),
                "bolt_connect".to_owned(),
                "auth".to_owned(),
            ],
            Some(command),
            None,
            Some(output.stdout),
            Some(output.stderr),
        );
    }

    let payload = match serde_json::from_str::<Value>(output.stdout.trim()) {
        Ok(payload) => payload,
        Err(error) => {
            return failed_record(
                "neo4j-java-bolt",
                suite_description("neo4j-java-bolt"),
                elapsed,
                format!("neo4j-java-driver smoke returned invalid JSON: {error}"),
                vec![
                    "compile".to_owned(),
                    "bolt_connect".to_owned(),
                    "auth".to_owned(),
                    "session".to_owned(),
                    "return_probe".to_owned(),
                ],
                Some(command),
                None,
                Some(output.stdout),
                Some(output.stderr),
            );
        }
    };

    let mut record = passed_record(
        "neo4j-java-bolt",
        suite_description("neo4j-java-bolt"),
        elapsed,
        payload
            .get("details")
            .and_then(Value::as_str)
            .unwrap_or("Neo4j Java driver connected over Bolt and completed a read-only RETURN probe")
            .to_owned(),
        vec![
            "compile".to_owned(),
            "bolt_connect".to_owned(),
            "auth".to_owned(),
            "session".to_owned(),
            "return_probe".to_owned(),
        ],
        Some(command),
        Some(payload),
        Some(output.stdout),
        Some(output.stderr),
    );
    let validation_state = if matches!(record.status, SuiteStatus::Passed) {
        "experimental"
    } else {
        "target"
    };
    attach_payload_metadata(
        &mut record,
        json!({
            "tool": "neo4j-java-driver",
            "driver_version": driver_version,
            "driver_jar": driver_jar.display().to_string(),
            "classpath_jars": driver_classpath,
            "validation_state": validation_state,
            "surface": NEO4J_BOLT_SURFACE,
            "limitations_ref": NEO4J_BOLT_LIMITATIONS_REF,
        }),
    );
    record
}

fn start_external_bolt_harness(
    ctx: &SuiteContext,
    suite_name: &str,
    start: Instant,
) -> Result<ExternalBoltHarness, Box<SuiteRecord>> {
    let pgwire_port = match reserve_distinct_tcp_port(&[ctx.pgwire_port]) {
        Ok(port) => port,
        Err(error) => {
            return Err(Box::new(failed_record(
                suite_name,
                suite_description(suite_name),
                start.elapsed(),
                format!("failed to reserve pgwire port: {error}"),
                vec![],
                None,
                None,
                None,
                None,
            )));
        }
    };
    let bolt_port = match reserve_distinct_tcp_port(&[ctx.pgwire_port, pgwire_port]) {
        Ok(port) => port,
        Err(error) => {
            return Err(Box::new(failed_record(
                suite_name,
                suite_description(suite_name),
                start.elapsed(),
                format!("failed to reserve Bolt port: {error}"),
                vec![],
                None,
                None,
                None,
                None,
            )));
        }
    };
    let stdout_path = ctx
        .scratch_dir
        .join(format!("{suite_name}.server.stdout.log"));
    let stderr_path = ctx
        .scratch_dir
        .join(format!("{suite_name}.server.stderr.log"));
    let (server_program, server_args) = match resolve_aiondb_server_command(&ctx.workspace_root) {
        Some(command) => command,
        None => {
            return Err(Box::new(skipped_record(
                suite_name,
                suite_description(suite_name),
                start.elapsed(),
                "neither target/debug/aiondb nor cargo was available to launch the Bolt compatibility server",
            )));
        }
    };
    let server_command = command_display(&server_program, &server_args);
    let server_envs = vec![
        ("AIONDB_BOOTSTRAP_USER".to_owned(), DEFAULT_USER.to_owned()),
        (
            "AIONDB_BOOTSTRAP_PASSWORD".to_owned(),
            BOLT_SMOKE_PASSWORD.to_owned(),
        ),
        (
            "AIONDB_PGWIRE_LISTEN_ADDR".to_owned(),
            format!("{DEFAULT_HOST}:{pgwire_port}"),
        ),
        ("AIONDB_ENABLE_BOLT_COMPAT".to_owned(), "true".to_owned()),
        (
            "AIONDB_BOLT_COMPAT_BIND".to_owned(),
            DEFAULT_HOST.to_owned(),
        ),
        (
            "AIONDB_BOLT_COMPAT_PORT".to_owned(),
            bolt_port.to_string(),
        ),
    ];
    let mut child = match spawn_logged_process(
        &server_program,
        &server_args,
        &server_envs,
        &ctx.workspace_root,
        &stdout_path,
        &stderr_path,
    ) {
        Ok(child) => child,
        Err(error) => {
            return Err(Box::new(failed_record(
                suite_name,
                suite_description(suite_name),
                start.elapsed(),
                format!("failed to start aiondb-server: {error}"),
                vec![],
                Some(server_command),
                None,
                None,
                None,
            )));
        }
    };
    if let Err(error) = wait_for_process_tcp_ready(
        &mut child,
        bolt_port,
        EXTERNAL_SERVER_READY_TIMEOUT,
        &stdout_path,
        &stderr_path,
    ) {
        let stdout = fs::read_to_string(&stdout_path).ok();
        let stderr = fs::read_to_string(&stderr_path).ok();
        let _ = stop_child_process(&mut child);
        return Err(Box::new(failed_record(
            suite_name,
            suite_description(suite_name),
            start.elapsed(),
            format!("Bolt compatibility listener did not become ready: {error}"),
            vec![],
            Some(server_command),
            Some(json!({
                "pgwire_port": pgwire_port,
                "bolt_port": bolt_port,
                "stdout_path": stdout_path.display().to_string(),
                "stderr_path": stderr_path.display().to_string()
            })),
            stdout,
            stderr,
        )));
    }
    Ok(ExternalBoltHarness {
        child,
        bolt_port,
    })
}

fn start_external_query_api_harness(
    ctx: &SuiteContext,
    suite_name: &str,
    start: Instant,
) -> Result<ExternalQueryApiHarness, Box<SuiteRecord>> {
    let pgwire_port = match reserve_distinct_tcp_port(&[ctx.pgwire_port]) {
        Ok(port) => port,
        Err(error) => {
            return Err(Box::new(failed_record(
                suite_name,
                suite_description(suite_name),
                start.elapsed(),
                format!("failed to reserve pgwire port: {error}"),
                vec![],
                None,
                None,
                None,
                None,
            )));
        }
    };
    let http_port = match reserve_distinct_tcp_port(&[ctx.pgwire_port, pgwire_port]) {
        Ok(port) => port,
        Err(error) => {
            return Err(Box::new(failed_record(
                suite_name,
                suite_description(suite_name),
                start.elapsed(),
                format!("failed to reserve observability http port: {error}"),
                vec![],
                None,
                None,
                None,
                None,
            )));
        }
    };
    let stdout_path = ctx
        .scratch_dir
        .join(format!("{suite_name}.server.stdout.log"));
    let stderr_path = ctx
        .scratch_dir
        .join(format!("{suite_name}.server.stderr.log"));
    let (server_program, mut server_args) = match resolve_aiondb_server_command(&ctx.workspace_root) {
        Some(command) => command,
        None => {
            return Err(Box::new(skipped_record(
                suite_name,
                suite_description(suite_name),
                start.elapsed(),
                "neither target/debug/aiondb nor cargo was available to launch the query api server",
            )));
        }
    };
    server_args.retain(|arg| arg != "--no-observability");
    let server_command = command_display(&server_program, &server_args);
    let server_envs = vec![
        ("AIONDB_BOOTSTRAP_USER".to_owned(), DEFAULT_USER.to_owned()),
        (
            "AIONDB_BOOTSTRAP_PASSWORD".to_owned(),
            BOLT_SMOKE_PASSWORD.to_owned(),
        ),
        (
            "AIONDB_PGWIRE_LISTEN_ADDR".to_owned(),
            format!("{DEFAULT_HOST}:{pgwire_port}"),
        ),
        (
            "AIONDB_OBSERVABILITY_BIND".to_owned(),
            DEFAULT_HOST.to_owned(),
        ),
        (
            "AIONDB_OBSERVABILITY_PORT".to_owned(),
            http_port.to_string(),
        ),
    ];
    let mut child = match spawn_logged_process(
        &server_program,
        &server_args,
        &server_envs,
        &ctx.workspace_root,
        &stdout_path,
        &stderr_path,
    ) {
        Ok(child) => child,
        Err(error) => {
            return Err(Box::new(failed_record(
                suite_name,
                suite_description(suite_name),
                start.elapsed(),
                format!("failed to start aiondb-server: {error}"),
                vec![],
                Some(server_command),
                None,
                None,
                None,
            )));
        }
    };
    if let Err(error) = wait_for_process_tcp_ready(
        &mut child,
        http_port,
        EXTERNAL_SERVER_READY_TIMEOUT,
        &stdout_path,
        &stderr_path,
    ) {
        let stdout = fs::read_to_string(&stdout_path).ok();
        let stderr = fs::read_to_string(&stderr_path).ok();
        let _ = stop_child_process(&mut child);
        return Err(Box::new(failed_record(
            suite_name,
            suite_description(suite_name),
            start.elapsed(),
            format!("query api listener did not become ready: {error}"),
            vec![],
            Some(server_command),
            Some(json!({
                "pgwire_port": pgwire_port,
                "http_port": http_port,
                "stdout_path": stdout_path.display().to_string(),
                "stderr_path": stderr_path.display().to_string()
            })),
            stdout,
            stderr,
        )));
    }
    Ok(ExternalQueryApiHarness {
        child,
        http_port,
    })
}

fn resolve_aiondb_server_command(workspace_root: &Path) -> Option<(PathBuf, Vec<String>)> {
    let debug_binary = workspace_root.join("target/debug/aiondb");
    if debug_binary.is_file() {
        return Some((
            debug_binary,
            vec!["--ephemeral".to_owned(), "--no-observability".to_owned()],
        ));
    }
    let cargo = find_in_path("cargo")?;
    Some((
        cargo,
        vec![
            "run".to_owned(),
            "-q".to_owned(),
            "-p".to_owned(),
            "aiondb-server".to_owned(),
            "--bin".to_owned(),
            "aiondb".to_owned(),
            "--".to_owned(),
            "--ephemeral".to_owned(),
            "--no-observability".to_owned(),
        ],
    ))
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
    if !cwd.is_dir() {
        return Err(format!(
            "node package base '{}' does not exist; set {NEO4J_JS_DRIVER_BASE_ENV} or provision target/compat/node-tools first",
            cwd.display()
        ));
    }
    let package_json = cwd.join("package.json");
    if !package_json.is_file() {
        return Err(format!(
            "node package base '{}' has no package.json; cannot resolve packages from it (set {NEO4J_JS_DRIVER_BASE_ENV} to another base if needed)",
            cwd.display()
        ));
    }
    for package in packages {
        let args = vec![
            "-e".to_owned(),
            format!(
                "const {{ createRequire }} = require('node:module');\
                 const requireFromBase = createRequire({base:?});\
                 requireFromBase.resolve('{package}/package.json');",
                base = package_json.display().to_string()
            ),
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

fn capture_python_module_version(python: &Path, module: &str, cwd: &Path) -> Option<String> {
    let args = vec![
        "-c".to_owned(),
        format!(
            "import importlib.metadata; print(importlib.metadata.version('{module}'))"
        ),
    ];
    capture_command_first_line(python, &args, cwd)
}

fn capture_node_package_version(node: &Path, base: &Path, package: &str) -> Option<String> {
    let package_json = base.join("package.json");
    let args = vec![
        "-e".to_owned(),
        format!(
            "const {{ createRequire }} = require('node:module');\
             const requireFromBase = createRequire({base:?});\
             console.log(requireFromBase('{package}/package.json').version);",
            base = package_json.display().to_string()
        ),
    ];
    capture_command_first_line(node, &args, base)
}

fn capture_command_first_line(program: &Path, args: &[String], cwd: &Path) -> Option<String> {
    let output = run_command(program, args, &[], cwd).ok()?;
    if output.status_code != Some(0) {
        return None;
    }
    normalized_lines(&output.stdout).into_iter().next()
}

fn infer_neo4j_java_driver_version(driver_jar: &Path) -> Option<String> {
    let file_name = driver_jar.file_name()?.to_str()?;
    let stripped = file_name
        .strip_prefix("neo4j-java-driver-")?
        .strip_suffix(".jar")?;
    Some(stripped.to_owned())
}

fn java_classpath_entries(driver_jar: &Path) -> Vec<String> {
    let mut entries = vec![driver_jar.display().to_string()];
    let Some(parent) = driver_jar.parent() else {
        return entries;
    };
    let Ok(read_dir) = fs::read_dir(parent) else {
        return entries;
    };
    let mut sibling_jars = read_dir
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path != driver_jar)
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("jar"))
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    sibling_jars.sort();
    entries.extend(sibling_jars);
    entries
}

fn attach_payload_metadata(record: &mut SuiteRecord, metadata: Value) {
    let Some(meta_obj) = metadata.as_object() else {
        return;
    };
    match record.payload.as_mut() {
        Some(Value::Object(payload)) => {
            for (key, value) in meta_obj {
                payload.insert(key.clone(), value.clone());
            }
        }
        Some(_) => {}
        None => {
            record.payload = Some(Value::Object(meta_obj.clone()));
        }
    }
}

fn resolve_node_package_base(workspace_root: &Path) -> PathBuf {
    env::var_os(NEO4J_JS_DRIVER_BASE_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root.join("target/compat/node-tools"))
}

fn resolve_program_override(env_key: &str, fallback_program: &str) -> Option<PathBuf> {
    if let Some(path) = env::var_os(env_key).map(PathBuf::from) {
        return Some(path);
    }
    find_in_path(fallback_program)
}

fn resolve_neo4j_java_driver_jar(workspace_root: &Path) -> Option<PathBuf> {
    if let Some(path) = env::var_os(NEO4J_JAVA_DRIVER_JAR_ENV).map(PathBuf::from) {
        return Some(path);
    }
    let compat_dir = workspace_root.join("target/compat/java-libs");
    if let Some(path) = newest_matching_file(&compat_dir, "neo4j-java-driver-", "jar") {
        return Some(path);
    }
    let m2_dir = env::var_os("HOME").map(PathBuf::from)?.join(".m2/repository/org/neo4j/driver");
    newest_matching_file(&m2_dir, "neo4j-java-driver-", "jar")
}

fn newest_matching_file(root: &Path, prefix: &str, extension: &str) -> Option<PathBuf> {
    if !root.exists() {
        return None;
    }
    let mut stack = vec![root.to_path_buf()];
    let mut best: Option<(PathBuf, SystemTime)> = None;
    while let Some(dir) = stack.pop() {
        let entries = fs::read_dir(&dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let file_name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
            if !file_name.starts_with(prefix)
                || path.extension().and_then(|ext| ext.to_str()) != Some(extension)
            {
                continue;
            }
            let modified = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            match &best {
                Some((_, best_modified)) if modified <= *best_modified => {}
                _ => best = Some((path, modified)),
            }
        }
    }
    best.map(|(path, _)| path)
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

fn spawn_logged_process(
    program: &Path,
    args: &[String],
    envs: &[(String, String)],
    cwd: &Path,
    stdout_path: &Path,
    stderr_path: &Path,
) -> Result<std::process::Child, String> {
    let stdout = fs::File::create(stdout_path)
        .map_err(|error| format!("creating {}: {error}", stdout_path.display()))?;
    let stderr = fs::File::create(stderr_path)
        .map_err(|error| format!("creating {}: {error}", stderr_path.display()))?;
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    for (key, value) in envs {
        command.env(key, value);
    }
    command
        .spawn()
        .map_err(|error| format!("{}: {error}", command_display(program, args)))
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

fn wait_for_process_tcp_ready(
    child: &mut std::process::Child,
    port: u16,
    timeout: Duration,
    stdout_path: &Path,
    stderr_path: &Path,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    let addr = format!("{DEFAULT_HOST}:{port}");
    while Instant::now() < deadline {
        if TcpStream::connect(&addr).is_ok() {
            return Ok(());
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("failed to poll child process: {error}"))?
        {
            let stdout = fs::read_to_string(stdout_path).unwrap_or_default();
            let stderr = fs::read_to_string(stderr_path).unwrap_or_default();
            return Err(format!(
                "process exited with status {:?}; stdout: {}; stderr: {}",
                status.code(),
                stdout,
                stderr
            ));
        }
        thread::sleep(SERVER_READY_POLL_INTERVAL);
    }
    Err(format!(
        "timed out waiting for tcp listener on {addr} after {timeout:?}"
    ))
}

fn stop_child_process(child: &mut std::process::Child) -> Result<(), String> {
    match child.try_wait() {
        Ok(Some(_)) => Ok(()),
        Ok(None) => {
            child
                .kill()
                .map_err(|error| format!("failed to kill child process: {error}"))?;
            child
                .wait()
                .map_err(|error| format!("failed to wait for child process: {error}"))?;
            Ok(())
        }
        Err(error) => Err(format!("failed to poll child process: {error}")),
    }
}

fn reserve_tcp_port() -> Result<u16, String> {
    let mut last_error = None;
    for attempt in 1..=TCP_PORT_RESERVE_ATTEMPTS {
        match TcpListener::bind((DEFAULT_HOST, 0)) {
            Ok(listener) => {
                return listener
                    .local_addr()
                    .map(|addr| addr.port())
                    .map_err(|error| format!("failed to read reserved tcp port: {error}"));
            }
            Err(error) => {
                last_error = Some(error.to_string());
                if attempt != TCP_PORT_RESERVE_ATTEMPTS {
                    thread::sleep(TCP_PORT_RESERVE_RETRY_DELAY);
                }
            }
        }
    }
    Err(format!(
        "failed to reserve tcp port on {DEFAULT_HOST} after {TCP_PORT_RESERVE_ATTEMPTS} attempts: {}",
        last_error.unwrap_or_else(|| "unknown error".to_owned())
    ))
}

fn reserve_distinct_tcp_port(used_ports: &[u16]) -> Result<u16, String> {
    for _ in 0..16 {
        let port = reserve_tcp_port()?;
        if !used_ports.contains(&port) {
            return Ok(port);
        }
    }
    Err("failed to reserve a distinct tcp port".to_owned())
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
        "group_filter": report.group_filter,
        "group_summary": report.group_summary,
        "strict": report.strict,
        "summary": {
            "passed": report.summary.passed,
            "failed": report.summary.failed,
            "skipped": report.summary.skipped,
        },
        "suites": suites,
    })
}

fn build_group_summary(group_filter: Option<&str>, records: &[SuiteRecord]) -> Option<GroupSummary> {
    let name = group_filter?;
    let blocked_suites = records
        .iter()
        .filter(|record| !matches!(record.status, SuiteStatus::Passed))
        .map(|record| record.name.clone())
        .collect::<Vec<_>>();
    let provisioning_hints = records
        .iter()
        .filter_map(|record| {
            let payload = record.payload.as_ref()?;
            let env = payload.get("provisioning_env")?.as_str()?.to_owned();
            let tool = payload
                .get("tool")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            Some(ProvisioningHint {
                suite: record.name.clone(),
                tool,
                env,
            })
        })
        .collect::<Vec<_>>();

    let experimental_passed = records
        .iter()
        .filter(|record| {
            matches!(record.status, SuiteStatus::Passed)
                && record
                    .payload
                    .as_ref()
                    .and_then(|payload| payload.get("validation_state"))
                    .and_then(Value::as_str)
                    == Some("experimental")
        })
        .count();
    let target_pending_provisioning = records
        .iter()
        .filter(|record| {
            matches!(record.status, SuiteStatus::Skipped)
                && record
                    .payload
                    .as_ref()
                    .and_then(|payload| payload.get("validation_state"))
                    .and_then(Value::as_str)
                    == Some("target")
                && record
                    .payload
                    .as_ref()
                    .and_then(|payload| payload.get("provisioning_env"))
                    .and_then(Value::as_str)
                    .is_some()
        })
        .count();
    let failed = records
        .iter()
        .filter(|record| matches!(record.status, SuiteStatus::Failed))
        .count();
    let skipped = records
        .iter()
        .filter(|record| matches!(record.status, SuiteStatus::Skipped))
        .count();
    let passed = records
        .iter()
        .filter(|record| matches!(record.status, SuiteStatus::Passed))
        .count();
    let fully_validated = failed == 0 && skipped == 0;
    let group_status = if failed > 0 {
        "failing"
    } else if skipped > 0 {
        "partial"
    } else {
        "passing"
    };

    Some(GroupSummary {
        name: name.to_owned(),
        group_status,
        suites_total: records.len(),
        passed,
        failed,
        skipped,
        experimental_passed,
        target_pending_provisioning,
        blocked_suites,
        fully_validated,
        provisioning_hints,
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
    if let Some(group) = report.group_summary.as_ref() {
        println!(
            "group: {} status={}, fully_validated={}, blocked_suites={}",
            group.name,
            group.group_status,
            group.fully_validated,
            group.blocked_suites.join(",")
        );
        if !group.provisioning_hints.is_empty() {
            let hints = group
                .provisioning_hints
                .iter()
                .map(|hint| match hint.tool.as_deref() {
                    Some(tool) => format!("{tool}->{env}", env = hint.env),
                    None => format!("{}->{}", hint.suite, hint.env),
                })
                .collect::<Vec<_>>()
                .join(", ");
            println!("group_provisioning_hints: {hints}");
        }
    }
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
        assert_eq!(opts.group_filter, None);
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
    fn parse_args_accepts_known_group() {
        let opts = parse_args(&[
            "--group".to_owned(),
            "neo4j-p0".to_owned(),
            "--report".to_owned(),
            "target/compat/neo4j-p0-smoke.json".to_owned(),
        ])
        .expect("group args should parse");

        assert_eq!(opts.suite_filter, None);
        assert_eq!(opts.group_filter.as_deref(), Some("neo4j-p0"));
        assert_eq!(
            opts.report_path,
            PathBuf::from("target/compat/neo4j-p0-smoke.json")
        );

        let opts = parse_args(&[
            "--group".to_owned(),
            "neo4j-http-p1".to_owned(),
        ])
        .expect("http group args should parse");
        assert_eq!(opts.group_filter.as_deref(), Some("neo4j-http-p1"));

        let opts = parse_args(&[
            "--group".to_owned(),
            "neo4j-browser-p0".to_owned(),
        ])
        .expect("browser group args should parse");
        assert_eq!(opts.group_filter.as_deref(), Some("neo4j-browser-p0"));
    }

    #[test]
    fn parse_args_rejects_unknown_group() {
        let error = parse_args(&["--group".to_owned(), "missing".to_owned()])
            .expect_err("unknown group should fail");
        assert!(error.contains("unknown group"));
    }

    #[test]
    fn parse_args_rejects_suite_and_group_together() {
        let error = parse_args(&[
            "--suite".to_owned(),
            "psql-libpq".to_owned(),
            "--group".to_owned(),
            "neo4j-p0".to_owned(),
        ])
        .expect_err("suite and group together should fail");
        assert!(error.contains("either --suite or --group"));
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
            group_filter: Some("neo4j-p0".to_owned()),
            group_summary: Some(GroupSummary {
                name: "neo4j-p0".to_owned(),
                group_status: "partial",
                suites_total: 2,
                passed: 1,
                failed: 0,
                skipped: 1,
                experimental_passed: 1,
                target_pending_provisioning: 1,
                blocked_suites: vec!["neo4j-javascript-bolt".to_owned()],
                fully_validated: false,
                provisioning_hints: vec![ProvisioningHint {
                    suite: "neo4j-javascript-bolt".to_owned(),
                    tool: Some("neo4j-javascript-driver".to_owned()),
                    env: "AIONDB_NEO4J_JS_DRIVER_BASE".to_owned(),
                }],
            }),
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
        assert_eq!(entry["group_filter"], json!("neo4j-p0"));
        assert_eq!(entry["group_summary"]["name"], json!("neo4j-p0"));
        assert_eq!(entry["group_summary"]["group_status"], json!("partial"));
        assert_eq!(entry["group_summary"]["experimental_passed"], json!(1));
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
            group_filter: None,
            group_summary: None,
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

    #[test]
    fn build_group_summary_tracks_provisioning_hints() {
        let records = vec![
            SuiteRecord {
                name: "neo4j-python-bolt".to_owned(),
                description: String::new(),
                status: SuiteStatus::Passed,
                duration_ms: 1,
                details: String::new(),
                checks: vec![],
                command: None,
                payload: Some(json!({
                    "tool": "neo4j-python-driver",
                    "validation_state": "experimental",
                })),
                stdout: None,
                stderr: None,
            },
            SuiteRecord {
                name: "neo4j-javascript-bolt".to_owned(),
                description: String::new(),
                status: SuiteStatus::Skipped,
                duration_ms: 1,
                details: String::new(),
                checks: vec![],
                command: None,
                payload: Some(json!({
                    "tool": "neo4j-javascript-driver",
                    "validation_state": "target",
                    "provisioning_env": "AIONDB_NEO4J_JS_DRIVER_BASE",
                })),
                stdout: None,
                stderr: None,
            },
        ];

        let summary = build_group_summary(Some("neo4j-p0"), &records).expect("group summary");
        assert_eq!(summary.name, "neo4j-p0");
        assert_eq!(summary.group_status, "partial");
        assert_eq!(summary.experimental_passed, 1);
        assert_eq!(summary.target_pending_provisioning, 1);
        assert_eq!(summary.blocked_suites, vec!["neo4j-javascript-bolt".to_owned()]);
        assert!(!summary.fully_validated);
        assert_eq!(summary.provisioning_hints.len(), 1);
        assert_eq!(
            summary.provisioning_hints[0].env,
            "AIONDB_NEO4J_JS_DRIVER_BASE"
        );
    }

    #[test]
    fn build_group_summary_marks_single_http_suite_as_passing() {
        let records = vec![SuiteRecord {
            name: "neo4j-query-api-http".to_owned(),
            description: String::new(),
            status: SuiteStatus::Passed,
            duration_ms: 1,
            details: String::new(),
            checks: vec![],
            command: None,
            payload: Some(json!({
                "tool": "neo4j-query-api-wrapper",
                "validation_state": "experimental",
                "surface": "query_api_wrapper",
            })),
            stdout: None,
            stderr: None,
        }];

        let summary =
            build_group_summary(Some("neo4j-http-p1"), &records).expect("http group summary");
        assert_eq!(summary.name, "neo4j-http-p1");
        assert_eq!(summary.group_status, "passing");
        assert_eq!(summary.experimental_passed, 1);
        assert_eq!(summary.target_pending_provisioning, 0);
        assert!(summary.blocked_suites.is_empty());
        assert!(summary.fully_validated);
        assert!(summary.provisioning_hints.is_empty());
    }

    #[test]
    fn build_group_summary_marks_browser_preflight_group_as_passing() {
        let records = vec![SuiteRecord {
            name: "neo4j-browser-preflight-bolt".to_owned(),
            description: String::new(),
            status: SuiteStatus::Passed,
            duration_ms: 1,
            details: String::new(),
            checks: vec![],
            command: None,
            payload: Some(json!({
                "tool": "neo4j-browser",
                "validation_state": "target",
                "surface": "bolt_compat",
                "evidence_kind": "browser_preflight_procedures",
            })),
            stdout: None,
            stderr: None,
        }];

        let summary = build_group_summary(Some("neo4j-browser-p0"), &records)
            .expect("browser group summary");
        assert_eq!(summary.name, "neo4j-browser-p0");
        assert_eq!(summary.group_status, "passing");
        assert_eq!(summary.experimental_passed, 0);
        assert_eq!(summary.target_pending_provisioning, 0);
        assert!(summary.blocked_suites.is_empty());
        assert!(summary.fully_validated);
        assert!(summary.provisioning_hints.is_empty());
    }
}
