mod check_adr;
mod check_compat_hooks;
mod check_noops;
mod compat_debt_report;
mod ecosystem_compat;
mod ecosystem_compat_trend;
mod file_limits;
mod pg_compat;
mod runtime_crash_lints;
mod storage_upgrade_matrix;
mod test_matrix;
mod workspace_lints;

use std::process::ExitCode;
use std::sync::Once;

enum Task {
    CheckAdr(check_adr::CheckAdrOptions),
    CheckCompatHooks(check_compat_hooks::CheckCompatHooksOptions),
    CheckNoops(check_noops::CheckNoopsOptions),
    CompatDebtReport(compat_debt_report::CompatDebtReportOptions),
    EcosystemCompat(ecosystem_compat::EcosystemCompatOptions),
    EcosystemCompatTrend(ecosystem_compat_trend::EcosystemCompatTrendOptions),
    TestMatrix(test_matrix::TestMatrixOptions),
    FileLimits(file_limits::FileLimitOptions),
    PgCompat(pg_compat::PgCompatOptions),
    RuntimeCrashLints,
    StorageUpgradeMatrix(storage_upgrade_matrix::StorageUpgradeMatrixOptions),
    WorkspaceLints,
}

fn parse_args() -> Result<Task, String> {
    let args: Vec<String> = std::env::args().collect();

    match args.get(1).map(String::as_str) {
        Some("check-adr") => check_adr::parse_args(&args[2..]).map(Task::CheckAdr),
        Some("check-compat-hooks") => {
            check_compat_hooks::parse_args(&args[2..]).map(Task::CheckCompatHooks)
        }
        Some("check-noops") => check_noops::parse_args(&args[2..]).map(Task::CheckNoops),
        Some("compat-debt-report") => {
            compat_debt_report::parse_args(&args[2..]).map(Task::CompatDebtReport)
        }
        Some("ecosystem-compat") => {
            ecosystem_compat::parse_args(&args[2..]).map(Task::EcosystemCompat)
        }
        Some("ecosystem-compat-trend") => {
            ecosystem_compat_trend::parse_args(&args[2..]).map(Task::EcosystemCompatTrend)
        }
        Some("test-matrix") => test_matrix::parse_args(&args[2..]).map(Task::TestMatrix),
        Some("file-limits") => file_limits::parse_args(&args[2..]).map(Task::FileLimits),
        Some("pg-compat") => pg_compat::parse_args(&args[2..]).map(Task::PgCompat),
        Some("runtime-crash-lints") => {
            runtime_crash_lints::parse_args(&args[2..]).map(|()| Task::RuntimeCrashLints)
        }
        Some("storage-upgrade-matrix") => {
            storage_upgrade_matrix::parse_args(&args[2..]).map(Task::StorageUpgradeMatrix)
        }
        Some("workspace-lints") => {
            workspace_lints::parse_args(&args[2..]).map(|()| Task::WorkspaceLints)
        }
        Some("--help" | "-h") | None => {
            print_usage();
            std::process::exit(0);
        }
        Some(other) => Err(format!(
            "unknown command: {other}\n\nRun with --help for usage."
        )),
    }
}

fn print_usage() {
    println!(
        "\
Usage:
  cargo xtask check-adr [--only <check>...] [--json]
  cargo xtask check-compat-hooks [--json]
  cargo xtask check-noops [--json] [--update-baseline]
  cargo xtask compat-debt-report [--json]
  cargo xtask ecosystem-compat [OPTIONS]
  cargo xtask ecosystem-compat-trend [OPTIONS]
  cargo xtask test-matrix [OPTIONS]
  cargo xtask file-limits [OPTIONS]
  cargo xtask pg-compat [OPTIONS]
  cargo xtask runtime-crash-lints
  cargo xtask storage-upgrade-matrix [OPTIONS]
  cargo xtask workspace-lints

Commands:
  check-adr     Run all 9 ADR lints (dispatch, compat-freeze, planner-path,
                internal-execute-sql, sql-string-match, sqlstate, budgets,
                dual-mode, ignores)
  check-compat-hooks  Block compat-layer growth (0 try_execute_compat_* hooks,
                      3 COMPAT_TAG_MATRIX entries). Additions require an ADR.
  check-noops   Enforce ADR-0003 (no new CommandNoOp without ADR)
  compat-debt-report  Snapshot of the PG-compat debt surface (matrix size,
                      parser sites, hooks, no-op plans, associated tests)
  ecosystem-compat  Run the driver / ORM PostgreSQL compatibility matrix
  ecosystem-compat-trend  Render a per-commit/per-suite trend over the JSONL history
  test-matrix   Run the local CI matrix
  file-limits   Enforce the architecture line limit across Rust source files
  pg-compat     Run the PostgreSQL regression compatibility runner
  runtime-crash-lints  Reject unwrap/expect/panic in production Rust code
  storage-upgrade-matrix  Copy old storage fixtures and run doctor/upgrade/doctor
  workspace-lints  Enforce the v0.1 workspace dependency contracts

Run `cargo xtask <COMMAND> --help` for command-specific usage."
    );
}

fn main() -> ExitCode {
    init_tracing();

    let task = match parse_args() {
        Ok(task) => task,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
    };

    match task {
        Task::CheckAdr(opts) => match check_adr::run(opts) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::FAILURE
            }
        },
        Task::CheckCompatHooks(opts) => match check_compat_hooks::run(opts) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::FAILURE
            }
        },
        Task::CheckNoops(opts) => match check_noops::run(opts) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::FAILURE
            }
        },
        Task::CompatDebtReport(opts) => match compat_debt_report::run(opts) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::FAILURE
            }
        },
        Task::EcosystemCompat(opts) => ecosystem_compat::run(opts),
        Task::EcosystemCompatTrend(opts) => match ecosystem_compat_trend::run(opts) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::FAILURE
            }
        },
        Task::TestMatrix(opts) => test_matrix::run(opts),
        Task::FileLimits(opts) => match file_limits::run(opts) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::FAILURE
            }
        },
        Task::PgCompat(opts) => pg_compat::run(opts),
        Task::RuntimeCrashLints => match runtime_crash_lints::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::FAILURE
            }
        },
        Task::StorageUpgradeMatrix(opts) => storage_upgrade_matrix::run(opts),
        Task::WorkspaceLints => match workspace_lints::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::FAILURE
            }
        },
    }
}

fn init_tracing() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
            )
            .with_target(false)
            .try_init();
    });
}
