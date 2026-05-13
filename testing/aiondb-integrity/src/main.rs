mod fuzzer;
mod harness;
mod suites;

use std::process::ExitCode;
use std::time::Instant;

use harness::{Report, SuiteResult};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args
        .get(1)
        .is_some_and(|arg| arg == "__corruption_reopen_probe")
    {
        let Some(path) = args.get(2) else {
            return ExitCode::from(2);
        };
        let code = suites::corruption::run_reopen_probe(std::path::Path::new(path));
        return ExitCode::from(code as u8);
    }
    if args
        .get(1)
        .is_some_and(|arg| arg == "__crash_random_worker")
    {
        let Some(path) = args.get(2) else {
            return ExitCode::from(2);
        };
        let seed = args
            .get(3)
            .and_then(|raw| raw.parse::<u64>().ok())
            .unwrap_or(0);
        let max_steps = args
            .get(4)
            .and_then(|raw| raw.parse::<usize>().ok())
            .unwrap_or(500_000);
        let backend = args.get(5).map(String::as_str);
        let code =
            suites::crash_random::run_worker(std::path::Path::new(path), seed, max_steps, backend);
        return ExitCode::from(code as u8);
    }

    let filter = args.get(1).map(String::as_str);

    let started = Instant::now();
    let mut report = Report::new();

    let runners: Vec<(&str, fn() -> SuiteResult)> = vec![
        // --- deterministic batteries ---
        ("types_basic", suites::types::run_basic),
        ("types_coercion", suites::types::run_coercion),
        ("types_boundaries", suites::types::run_boundaries),
        ("ddl_create_drop", suites::ddl::run_create_drop),
        ("ddl_alter", suites::ddl::run_alter),
        ("ddl_constraints", suites::ddl::run_constraints),
        ("ddl_indexes", suites::ddl::run_indexes),
        ("dml_insert", suites::dml::run_insert),
        ("dml_update", suites::dml::run_update),
        ("dml_delete", suites::dml::run_delete),
        ("dml_upsert", suites::dml::run_upsert),
        ("dml_returning", suites::dml::run_returning),
        ("select_basic", suites::select::run_basic),
        ("select_where", suites::select::run_where),
        ("select_orderby", suites::select::run_orderby),
        ("select_groupby", suites::select::run_groupby),
        ("select_having", suites::select::run_having),
        ("select_limit_offset", suites::select::run_limit_offset),
        ("select_distinct", suites::select::run_distinct),
        ("joins", suites::joins::run_all),
        ("subqueries", suites::subqueries::run_all),
        ("expressions", suites::expressions::run_all),
        ("aggregates", suites::aggregates::run_all),
        ("transactions_basic", suites::transactions::run_basic),
        (
            "transactions_isolation",
            suites::transactions::run_isolation,
        ),
        ("transactions_rollback", suites::transactions::run_rollback),
        ("null_handling", suites::null_handling::run_all),
        ("strings", suites::strings::run_all),
        ("cte", suites::cte::run_all),
        ("set_operations", suites::set_operations::run_all),
        ("case_expressions", suites::case_expr::run_all),
        ("views", suites::views::run_all),
        ("sequences", suites::sequences::run_all),
        // --- integrity hardening ---
        ("race_concurrency", suites::race::run_all),
        (
            "persistence_restart_cycles",
            suites::persistence::run_restart_cycles,
        ),
        ("crash_random_kill", suites::crash_random::run_all),
        ("corruption_wal", suites::corruption::run_all),
        (
            "diff_sqlite_corpus",
            suites::differential_sqlite::run_corpus,
        ),
        ("diff_sqlite_stateful_fuzz", || {
            suites::differential_sqlite::run_stateful_fuzz(4200)
        }),
        ("diff_sqlite_history_replay", || {
            suites::differential_sqlite::run_history_replay(3200)
        }),
        (
            "diff_sqlite_expr_matrix",
            suites::differential_sqlite::run_expression_matrix,
        ),
        (
            "diff_sqlite_predicate_matrix",
            suites::differential_sqlite::run_predicate_matrix,
        ),
        (
            "diff_sqlite_group_order_matrix",
            suites::differential_sqlite::run_group_order_matrix,
        ),
        (
            "diff_sqlite_join_matrix",
            suites::differential_sqlite::run_join_matrix,
        ),
        (
            "diff_sqlite_cte_union_matrix",
            suites::differential_sqlite::run_cte_union_matrix,
        ),
        (
            "diff_sqlite_error_class_matrix",
            suites::differential_sqlite::run_error_class_matrix,
        ),
        ("diff_sqlite_txn_metamorphic", || {
            suites::differential_sqlite::run_txn_metamorphic(6200)
        }),
        (
            "diff_sqlite_null_semantics_matrix",
            suites::differential_sqlite::run_null_semantics_matrix,
        ),
        (
            "diff_sqlite_string_matrix",
            suites::differential_sqlite::run_string_matrix,
        ),
        (
            "diff_sqlite_3vl_matrix",
            suites::differential_sqlite::run_three_valued_logic_matrix,
        ),
        ("diff_sqlite_schema_churn", || {
            suites::differential_sqlite::run_schema_churn_matrix(4000)
        }),
        (
            "diff_sqlite_numeric_cast_matrix",
            suites::differential_sqlite::run_numeric_cast_matrix,
        ),
        (
            "diff_sqlite_set_ops_matrix",
            suites::differential_sqlite::run_set_ops_matrix,
        ),
        ("diff_sqlite_rollback_storm", || {
            suites::differential_sqlite::run_rollback_storm(6200)
        }),
        (
            "diff_sqlite_case_agg_matrix",
            suites::differential_sqlite::run_case_aggregation_matrix,
        ),
        (
            "diff_sqlite_distinct_having_matrix",
            suites::differential_sqlite::run_distinct_having_matrix,
        ),
        (
            "diff_sqlite_correlated_subquery_matrix",
            suites::differential_sqlite::run_correlated_subquery_matrix,
        ),
        ("diff_sqlite_mutation_checkpoint_matrix", || {
            suites::differential_sqlite::run_mutation_checkpoint_matrix(5000)
        }),
        (
            "diff_sqlite_sampling_order_stability",
            suites::differential_sqlite::run_sampling_order_stability,
        ),
        (
            "diff_sqlite_exists_in_notin_matrix",
            suites::differential_sqlite::run_exists_in_notin_matrix,
        ),
        (
            "diff_sqlite_union_projection_stress",
            suites::differential_sqlite::run_union_projection_stress,
        ),
        (
            "diff_sqlite_multi_join_aggregate_matrix",
            suites::differential_sqlite::run_multi_join_aggregate_matrix,
        ),
        ("diff_sqlite_transaction_script_fuzz", || {
            suites::differential_sqlite::run_transaction_script_fuzz(5200)
        }),
        (
            "diff_postgres_optional",
            suites::differential_postgres::run_optional,
        ),
        // --- fuzzer ---
        ("fuzz_expressions", || fuzzer::run_expression_fuzz(5000)),
        ("fuzz_queries", || fuzzer::run_query_fuzz(2000)),
        ("fuzz_ddl_dml", || fuzzer::run_ddl_dml_fuzz(1000)),
        ("fuzz_crash", || fuzzer::run_crash_resistance(3000)),
    ];

    for (name, runner) in &runners {
        if let Some(f) = filter {
            if !name.contains(f) {
                continue;
            }
        }
        eprint!("  {name:<30}");
        let t = Instant::now();
        let result = runner();
        let elapsed = t.elapsed();
        match &result {
            Ok(stats) => {
                eprintln!(
                    " OK  ({} passed, {} skipped) [{:.1}s]",
                    stats.passed,
                    stats.skipped,
                    elapsed.as_secs_f64()
                );
            }
            Err(failures) => {
                eprintln!(
                    " FAIL ({} failures) [{:.1}s]",
                    failures.len(),
                    elapsed.as_secs_f64()
                );
                for f in failures {
                    eprintln!("      FAIL: {f}");
                }
            }
        }
        report.record(name, result);
    }

    let elapsed = started.elapsed();
    eprintln!();
    report.summary(elapsed);

    if report.all_passed() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
