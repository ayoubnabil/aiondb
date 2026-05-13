use std::fs;
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

const DEFAULT_REPORT_PATH: &str = "target/compat/checkworld-lite.json";
const DEFAULT_MEMORY_LIMIT_MB: u64 = 5120;

struct Suite {
    name: &'static str,
    filter: &'static str,
    category: &'static str,
    reason: &'static str,
    env: &'static [(&'static str, &'static str)],
}

const SUITES: &[Suite] = &[
    Suite {
        name: "transactions_basic",
        filter: "transactions_basic",
        category: "core",
        reason: "basic transaction begin/commit semantics",
        env: &[],
    },
    Suite {
        name: "transactions_isolation",
        filter: "transactions_isolation",
        category: "concurrency",
        reason: "transaction visibility and isolation semantics",
        env: &[],
    },
    Suite {
        name: "transactions_rollback",
        filter: "transactions_rollback",
        category: "core",
        reason: "rollback and savepoint behavior",
        env: &[],
    },
    Suite {
        name: "ddl_create_drop",
        filter: "ddl_create_drop",
        category: "core",
        reason: "CREATE/DROP DDL semantics",
        env: &[],
    },
    Suite {
        name: "ddl_alter",
        filter: "ddl_alter",
        category: "core",
        reason: "ALTER DDL semantics",
        env: &[],
    },
    Suite {
        name: "ddl_constraints",
        filter: "ddl_constraints",
        category: "core",
        reason: "constraint DDL and enforcement coverage",
        env: &[],
    },
    Suite {
        name: "ddl_indexes",
        filter: "ddl_indexes",
        category: "core",
        reason: "index DDL and index-backed query behavior",
        env: &[],
    },
    Suite {
        name: "dml_insert",
        filter: "dml_insert",
        category: "core",
        reason: "INSERT behavior",
        env: &[],
    },
    Suite {
        name: "dml_update",
        filter: "dml_update",
        category: "core",
        reason: "UPDATE behavior",
        env: &[],
    },
    Suite {
        name: "dml_delete",
        filter: "dml_delete",
        category: "core",
        reason: "DELETE behavior",
        env: &[],
    },
    Suite {
        name: "dml_upsert",
        filter: "dml_upsert",
        category: "core",
        reason: "UPSERT / conflict handling behavior",
        env: &[],
    },
    Suite {
        name: "dml_returning",
        filter: "dml_returning",
        category: "core",
        reason: "RETURNING behavior for DML",
        env: &[],
    },
    Suite {
        name: "select_basic",
        filter: "select_basic",
        category: "core",
        reason: "basic SELECT semantics",
        env: &[],
    },
    Suite {
        name: "select_where",
        filter: "select_where",
        category: "core",
        reason: "WHERE predicate semantics",
        env: &[],
    },
    Suite {
        name: "select_orderby",
        filter: "select_orderby",
        category: "core",
        reason: "ORDER BY semantics",
        env: &[],
    },
    Suite {
        name: "select_groupby",
        filter: "select_groupby",
        category: "core",
        reason: "GROUP BY semantics",
        env: &[],
    },
    Suite {
        name: "select_having",
        filter: "select_having",
        category: "core",
        reason: "HAVING semantics",
        env: &[],
    },
    Suite {
        name: "select_limit_offset",
        filter: "select_limit_offset",
        category: "core",
        reason: "LIMIT/OFFSET semantics",
        env: &[],
    },
    Suite {
        name: "select_distinct",
        filter: "select_distinct",
        category: "core",
        reason: "DISTINCT semantics",
        env: &[],
    },
    Suite {
        name: "joins",
        filter: "joins",
        category: "core",
        reason: "join semantics",
        env: &[],
    },
    Suite {
        name: "subqueries",
        filter: "subqueries",
        category: "core",
        reason: "subquery semantics",
        env: &[],
    },
    Suite {
        name: "expressions",
        filter: "expressions",
        category: "core",
        reason: "expression evaluation semantics",
        env: &[],
    },
    Suite {
        name: "aggregates",
        filter: "aggregates",
        category: "core",
        reason: "aggregate semantics",
        env: &[],
    },
    Suite {
        name: "null_handling",
        filter: "null_handling",
        category: "core",
        reason: "NULL and three-valued logic behavior",
        env: &[],
    },
    Suite {
        name: "strings",
        filter: "strings",
        category: "core",
        reason: "string expression semantics",
        env: &[],
    },
    Suite {
        name: "cte",
        filter: "cte",
        category: "core",
        reason: "CTE semantics",
        env: &[],
    },
    Suite {
        name: "set_operations",
        filter: "set_operations",
        category: "core",
        reason: "UNION/INTERSECT/EXCEPT semantics",
        env: &[],
    },
    Suite {
        name: "case_expressions",
        filter: "case_expressions",
        category: "core",
        reason: "CASE expression semantics",
        env: &[],
    },
    Suite {
        name: "views",
        filter: "views",
        category: "core",
        reason: "view definition and query semantics",
        env: &[],
    },
    Suite {
        name: "sequences",
        filter: "sequences",
        category: "core",
        reason: "sequence behavior",
        env: &[],
    },
    Suite {
        name: "race_concurrency",
        filter: "race_concurrency",
        category: "concurrency",
        reason: "multi-session races and catalog/data consistency under concurrent work",
        env: &[],
    },
    Suite {
        name: "persistence_restart_cycles",
        filter: "persistence_restart_cycles",
        category: "recovery",
        reason: "durable reopen cycles after DDL/DML",
        env: &[],
    },
    Suite {
        name: "corruption_wal",
        filter: "corruption_wal",
        category: "recovery",
        reason: "WAL corruption detection and safe reopen behavior",
        env: &[],
    },
    Suite {
        name: "fuzz_crash",
        filter: "fuzz_crash",
        category: "crash",
        reason: "random SQL crash-resistance without protocol/API assumptions",
        env: &[],
    },
    Suite {
        name: "diff_sqlite_corpus",
        filter: "diff_sqlite_corpus",
        category: "differential",
        reason: "SQLite differential corpus for common SQL behavior",
        env: &[],
    },
    Suite {
        name: "diff_sqlite_stateful_fuzz",
        filter: "diff_sqlite_stateful_fuzz",
        category: "differential",
        reason: "stateful SQLite differential fuzzing",
        env: &[],
    },
    Suite {
        name: "diff_sqlite_history_replay",
        filter: "diff_sqlite_history_replay",
        category: "differential",
        reason: "SQLite differential history replay",
        env: &[],
    },
    Suite {
        name: "diff_sqlite_expr_matrix",
        filter: "diff_sqlite_expr_matrix",
        category: "differential",
        reason: "SQLite differential expression matrix",
        env: &[],
    },
    Suite {
        name: "diff_sqlite_predicate_matrix",
        filter: "diff_sqlite_predicate_matrix",
        category: "differential",
        reason: "SQLite differential predicate matrix",
        env: &[],
    },
    Suite {
        name: "diff_sqlite_group_order_matrix",
        filter: "diff_sqlite_group_order_matrix",
        category: "differential",
        reason: "SQLite differential GROUP/ORDER matrix",
        env: &[],
    },
    Suite {
        name: "diff_sqlite_join_matrix",
        filter: "diff_sqlite_join_matrix",
        category: "differential",
        reason: "SQLite differential join matrix",
        env: &[],
    },
    Suite {
        name: "diff_sqlite_cte_union_matrix",
        filter: "diff_sqlite_cte_union_matrix",
        category: "differential",
        reason: "SQLite differential CTE/UNION matrix",
        env: &[],
    },
    Suite {
        name: "diff_sqlite_error_class_matrix",
        filter: "diff_sqlite_error_class_matrix",
        category: "differential",
        reason: "SQLite differential error-class matrix",
        env: &[],
    },
    Suite {
        name: "diff_sqlite_txn_metamorphic",
        filter: "diff_sqlite_txn_metamorphic",
        category: "differential",
        reason: "SQLite differential transaction metamorphic tests",
        env: &[],
    },
    Suite {
        name: "diff_sqlite_null_semantics_matrix",
        filter: "diff_sqlite_null_semantics_matrix",
        category: "differential",
        reason: "SQLite differential NULL semantics matrix",
        env: &[],
    },
    Suite {
        name: "diff_sqlite_string_matrix",
        filter: "diff_sqlite_string_matrix",
        category: "differential",
        reason: "SQLite differential string matrix",
        env: &[],
    },
    Suite {
        name: "diff_sqlite_3vl_matrix",
        filter: "diff_sqlite_3vl_matrix",
        category: "differential",
        reason: "SQLite differential three-valued logic matrix",
        env: &[],
    },
    Suite {
        name: "diff_sqlite_schema_churn",
        filter: "diff_sqlite_schema_churn",
        category: "differential",
        reason: "SQLite differential schema churn tests",
        env: &[],
    },
    Suite {
        name: "diff_sqlite_numeric_cast_matrix",
        filter: "diff_sqlite_numeric_cast_matrix",
        category: "differential",
        reason: "SQLite differential numeric/cast matrix",
        env: &[],
    },
    Suite {
        name: "diff_sqlite_set_ops_matrix",
        filter: "diff_sqlite_set_ops_matrix",
        category: "differential",
        reason: "SQLite differential set operations matrix",
        env: &[],
    },
    Suite {
        name: "diff_sqlite_rollback_storm",
        filter: "diff_sqlite_rollback_storm",
        category: "differential",
        reason: "SQLite differential rollback storm",
        env: &[],
    },
    Suite {
        name: "diff_sqlite_case_agg_matrix",
        filter: "diff_sqlite_case_agg_matrix",
        category: "differential",
        reason: "SQLite differential CASE/aggregate matrix",
        env: &[],
    },
    Suite {
        name: "diff_sqlite_distinct_having_matrix",
        filter: "diff_sqlite_distinct_having_matrix",
        category: "differential",
        reason: "SQLite differential DISTINCT/HAVING matrix",
        env: &[],
    },
    Suite {
        name: "diff_sqlite_correlated_subquery_matrix",
        filter: "diff_sqlite_correlated_subquery_matrix",
        category: "differential",
        reason: "SQLite differential correlated subquery matrix",
        env: &[],
    },
    Suite {
        name: "diff_sqlite_mutation_checkpoint_matrix",
        filter: "diff_sqlite_mutation_checkpoint_matrix",
        category: "differential",
        reason: "SQLite differential mutation/checkpoint matrix",
        env: &[],
    },
    Suite {
        name: "diff_sqlite_sampling_order_stability",
        filter: "diff_sqlite_sampling_order_stability",
        category: "differential",
        reason: "SQLite differential sampling/order stability",
        env: &[],
    },
    Suite {
        name: "diff_sqlite_exists_in_notin_matrix",
        filter: "diff_sqlite_exists_in_notin_matrix",
        category: "differential",
        reason: "SQLite differential EXISTS/IN/NOT IN matrix",
        env: &[],
    },
    Suite {
        name: "diff_sqlite_union_projection_stress",
        filter: "diff_sqlite_union_projection_stress",
        category: "differential",
        reason: "SQLite differential union projection stress",
        env: &[],
    },
    Suite {
        name: "diff_sqlite_multi_join_aggregate_matrix",
        filter: "diff_sqlite_multi_join_aggregate_matrix",
        category: "differential",
        reason: "SQLite differential multi-join aggregate matrix",
        env: &[],
    },
    Suite {
        name: "diff_sqlite_transaction_script_fuzz",
        filter: "diff_sqlite_transaction_script_fuzz",
        category: "differential",
        reason: "SQLite differential transaction script fuzzing",
        env: &[],
    },
];

pub(crate) struct CheckworldLiteOptions {
    report_path: PathBuf,
    suite_filter: Option<String>,
    continue_on_error: bool,
    json: bool,
    memory_limit_mb: u64,
}

#[derive(Debug)]
struct SuiteOutcome {
    name: &'static str,
    category: &'static str,
    reason: &'static str,
    ok: bool,
    passed: usize,
    skipped: usize,
    failed: usize,
    elapsed: Duration,
    stderr_tail: String,
}

pub(crate) fn parse_args(args: &[String]) -> Result<CheckworldLiteOptions, String> {
    let mut report_path = PathBuf::from(DEFAULT_REPORT_PATH);
    let mut suite_filter = None;
    let mut continue_on_error = false;
    let mut json = false;
    let mut memory_limit_mb = DEFAULT_MEMORY_LIMIT_MB;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--report" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| "--report requires a path".to_owned())?;
                report_path = PathBuf::from(value);
            }
            "--suite" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| "--suite requires a suite/category filter".to_owned())?;
                suite_filter = Some(value.clone());
            }
            "--continue-on-error" => continue_on_error = true,
            "--json" => json = true,
            "--memory-limit-mb" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| "--memory-limit-mb requires a positive integer".to_owned())?;
                memory_limit_mb = value
                    .parse::<u64>()
                    .map_err(|_| "--memory-limit-mb requires a positive integer".to_owned())?;
                if memory_limit_mb == 0 {
                    return Err("--memory-limit-mb must be greater than zero".to_owned());
                }
            }
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            other => {
                return Err(format!(
                    "unknown flag for checkworld-lite: {other}\n\nRun `cargo xtask checkworld-lite --help` for usage."
                ));
            }
        }
        index += 1;
    }

    Ok(CheckworldLiteOptions {
        report_path,
        suite_filter,
        continue_on_error,
        json,
        memory_limit_mb,
    })
}

pub(crate) fn print_usage() {
    println!(
        "\
Usage: cargo xtask checkworld-lite [OPTIONS]

Run the AionDB check-world-lite campaign: concurrency, crash, restart and WAL
recovery tests only. This intentionally excludes PostgreSQL client/API,
contrib, ORM, psql, and architecture-specific check-world suites.

Options:
  --suite <FILTER>       Run suites whose name or category contains FILTER
  --report <PATH>        Write JSON report (default: target/compat/checkworld-lite.json)
  --memory-limit-mb <N>  Cap each child suite process virtual memory (default: 5120)
  --continue-on-error    Run remaining suites after a failure
  --json                 Print the JSON report to stdout as well
  -h, --help             Print this help"
    );
}

pub(crate) fn run(opts: CheckworldLiteOptions) -> ExitCode {
    let selected: Vec<&Suite> = SUITES
        .iter()
        .filter(|suite| {
            opts.suite_filter
                .as_ref()
                .map_or(true, |filter| suite.name.contains(filter) || suite.category.contains(filter))
        })
        .collect();

    if selected.is_empty() {
        eprintln!("error: no checkworld-lite suite matched the requested filter");
        return ExitCode::FAILURE;
    }

    println!("=== AionDB checkworld-lite ===");
    println!("scope: concurrency, crash, restart, WAL/recovery");
    println!("excluded: PostgreSQL contrib, TAP/API/client/tooling, ORM/driver ecosystem tests");
    println!(
        "oom guard: each suite is capped at {} MiB virtual memory via prlimit",
        opts.memory_limit_mb
    );
    println!();

    let started = Instant::now();
    let mut outcomes = Vec::new();
    let mut any_failed = false;

    for (idx, suite) in selected.iter().enumerate() {
        let label = format!("[{}/{}] {}", idx + 1, selected.len(), suite.name);
        let dots = ".".repeat(34_usize.saturating_sub(label.len()));
        print!("{label} {dots} ");
        use std::io::Write;
        let _ = std::io::stdout().flush();

        let outcome = run_suite(suite, opts.memory_limit_mb);
        if outcome.ok {
            println!(
                "OK ({} passed, {} skipped, {:.1}s)",
                outcome.passed,
                outcome.skipped,
                outcome.elapsed.as_secs_f64()
            );
        } else {
            println!(
                "FAIL ({} passed, {} failed, {:.1}s)",
                outcome.passed,
                outcome.failed,
                outcome.elapsed.as_secs_f64()
            );
            any_failed = true;
        }

        outcomes.push(outcome);
        if any_failed && !opts.continue_on_error {
            break;
        }
    }

    let elapsed = started.elapsed();
    let report = render_report_json(&outcomes, elapsed, opts.memory_limit_mb);
    if let Some(parent) = opts.report_path.parent() {
        if let Err(error) = fs::create_dir_all(parent) {
            eprintln!("error: create report dir failed: {error}");
            return ExitCode::FAILURE;
        }
    }
    if let Err(error) = fs::write(&opts.report_path, &report) {
        eprintln!(
            "error: write report failed ({}): {error}",
            opts.report_path.display()
        );
        return ExitCode::FAILURE;
    }

    let passed_suites = outcomes.iter().filter(|outcome| outcome.ok).count();
    let total_suites = outcomes.len();
    let passed_tests: usize = outcomes.iter().map(|outcome| outcome.passed).sum();
    let failed_tests: usize = outcomes.iter().map(|outcome| outcome.failed).sum();
    let scored_tests = passed_tests + failed_tests;
    let suite_pct = percent(passed_suites, total_suites);
    let test_pct = percent(passed_tests, scored_tests);

    println!();
    println!(
        "Result: suites {passed_suites}/{total_suites} ({suite_pct:.1}%), scored tests {passed_tests}/{scored_tests} ({test_pct:.1}%)"
    );
    println!("Report: {}", opts.report_path.display());

    if opts.json {
        println!("{report}");
    }

    if any_failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn run_suite(suite: &'static Suite, memory_limit_mb: u64) -> SuiteOutcome {
    let started = Instant::now();
    let memory_limit_bytes = memory_limit_mb.saturating_mul(1024).saturating_mul(1024);
    let mut command = Command::new("prlimit");
    command.args([
        &format!("--as={memory_limit_bytes}"),
        "cargo",
        "run",
        "-q",
        "--manifest-path",
        "testing/aiondb-integrity/Cargo.toml",
        "--",
        suite.filter,
    ]);
    for (key, value) in suite.env {
        command.env(key, value);
    }

    let output = command.output();
    let elapsed = started.elapsed();
    match output {
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let (passed, skipped, failed) = parse_integrity_summary(&stderr);
            SuiteOutcome {
                name: suite.name,
                category: suite.category,
                reason: suite.reason,
                ok: output.status.success() && failed == 0,
                passed,
                skipped,
                failed,
                elapsed,
                stderr_tail: tail_lines(&stderr, 18),
            }
        }
        Err(error) => SuiteOutcome {
            name: suite.name,
            category: suite.category,
            reason: suite.reason,
            ok: false,
            passed: 0,
            skipped: 0,
            failed: 1,
            elapsed,
            stderr_tail: format!("failed to spawn aiondb-integrity under prlimit: {error}"),
        },
    }
}

fn parse_integrity_summary(stderr: &str) -> (usize, usize, usize) {
    for line in stderr.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("Tests:") {
            continue;
        }
        let mut numbers = Vec::new();
        for token in trimmed.split_whitespace() {
            if let Ok(value) = token.parse::<usize>() {
                numbers.push(value);
            }
        }
        if numbers.len() >= 3 {
            return (numbers[0], numbers[1], numbers[2]);
        }
    }
    (0, 0, 1)
}

fn tail_lines(value: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = value.lines().collect();
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n")
}

fn render_report_json(
    outcomes: &[SuiteOutcome],
    elapsed: Duration,
    memory_limit_mb: u64,
) -> String {
    let passed_suites = outcomes.iter().filter(|outcome| outcome.ok).count();
    let total_suites = outcomes.len();
    let passed_tests: usize = outcomes.iter().map(|outcome| outcome.passed).sum();
    let skipped_tests: usize = outcomes.iter().map(|outcome| outcome.skipped).sum();
    let failed_tests: usize = outcomes.iter().map(|outcome| outcome.failed).sum();
    let scored_tests = passed_tests + failed_tests;

    let suites_json = outcomes
        .iter()
        .map(|outcome| {
            format!(
                "{{\"name\":\"{}\",\"category\":\"{}\",\"reason\":\"{}\",\"status\":\"{}\",\"passed\":{},\"skipped\":{},\"failed\":{},\"elapsed_seconds\":{:.3},\"stderr_tail\":\"{}\"}}",
                json_escape(outcome.name),
                json_escape(outcome.category),
                json_escape(outcome.reason),
                if outcome.ok { "passed" } else { "failed" },
                outcome.passed,
                outcome.skipped,
                outcome.failed,
                outcome.elapsed.as_secs_f64(),
                json_escape(&outcome.stderr_tail)
            )
        })
        .collect::<Vec<_>>()
        .join(",");

    format!(
        "{{\n  \"scope\":\"checkworld-lite\",\n  \"included\":\"concurrency, crash, restart, WAL/recovery\",\n  \"excluded\":\"PostgreSQL contrib, TAP/API/client/tooling, ORM/driver ecosystem tests\",\n  \"memory_limit_mb\":{},\n  \"suite_score\":{{\"passed\":{},\"total\":{},\"percent\":{:.2}}},\n  \"test_score\":{{\"passed\":{},\"failed\":{},\"skipped\":{},\"scored_total\":{},\"percent\":{:.2}}},\n  \"elapsed_seconds\":{:.3},\n  \"suites\":[{}]\n}}\n",
        memory_limit_mb,
        passed_suites,
        total_suites,
        percent(passed_suites, total_suites),
        passed_tests,
        failed_tests,
        skipped_tests,
        scored_tests,
        percent(passed_tests, scored_tests),
        elapsed.as_secs_f64(),
        suites_json
    )
}

fn percent(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 * 100.0 / denominator as f64
    }
}

fn json_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}
