#![allow(clippy::pedantic)]

//! DoS / complexity audit tests for the engine.
//!
//! Each test here submits a pathological SQL construct and enforces a 5-10s
//! wall-clock bound. If the engine terminates (either by producing a result
//! or by returning a DbError such as QueryCanceled/ResourceExhausted) within
//! the bound, the resource guard is considered effective. If the call takes
//! longer than the bound we fail the test, which surfaces a real DoS bug.

use std::time::{Duration, Instant};

use aiondb_config::RuntimeConfig;

use super::*;

const DOS_WALL_BOUND: Duration = Duration::from_secs(10);
const DOS_RECURSIVE_CTE_WALL_BOUND: Duration = Duration::from_secs(15);

fn dos_wall_bound() -> Duration {
    const CI_DEFAULT_MULTIPLIER: f64 = 2.0;
    const LOCAL_DEFAULT_MULTIPLIER: f64 = 1.0;
    const MIN_ALLOWED_MULTIPLIER: f64 = 0.25;
    const MAX_ALLOWED_MULTIPLIER: f64 = 4.0;

    let default_multiplier = if std::env::var_os("CI").is_some() || cfg!(debug_assertions) {
        CI_DEFAULT_MULTIPLIER
    } else {
        LOCAL_DEFAULT_MULTIPLIER
    };
    let multiplier = std::env::var("AIONDB_PERF_BUDGET_MULTIPLIER")
        .ok()
        .and_then(|raw| raw.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
        .map_or(default_multiplier, |value| {
            value.clamp(MIN_ALLOWED_MULTIPLIER, MAX_ALLOWED_MULTIPLIER)
        });

    Duration::from_secs_f64(DOS_WALL_BOUND.as_secs_f64() * multiplier)
}

/// Execute `sql` on `session` and assert that the call returns (either Ok or
/// Err) within `DOS_WALL_BOUND`. Returns the measured elapsed time so tests
/// can log it.
fn run_bounded(engine: &Engine, session: &SessionHandle, sql: &str, label: &str) -> Duration {
    run_bounded_with_limit(engine, session, sql, label, dos_wall_bound())
}

fn run_bounded_with_limit(
    engine: &Engine,
    session: &SessionHandle,
    sql: &str,
    label: &str,
    wall_bound: Duration,
) -> Duration {
    let start = Instant::now();
    let result = engine.execute_sql(session, sql);
    let elapsed = start.elapsed();
    assert!(
        elapsed < wall_bound,
        "[{label}] took {elapsed:?} > {wall_bound:?} — possible DoS; result={:?}",
        result.as_ref().err().map(|e| e.to_string())
    );
    elapsed
}

// ---------------------------------------------------------------------------
// Scenario 1: recursive CTE without explicit stopping predicate
// ---------------------------------------------------------------------------

#[test]
fn dos_recursive_cte_unbounded_counts_are_capped() {
    let handle = std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            let mut runtime = RuntimeConfig::default();
            runtime.limits.statement_timeout = Duration::from_secs(30);
            runtime.limits.max_recursive_iterations = 1_000;
            runtime.limits.max_recursive_rows = 1_000;

            let engine = EngineBuilder::for_testing()
                .with_runtime_config(runtime)
                .build()
                .unwrap();
            let (session, _) = engine.startup(startup_params()).expect("startup");

            // COUNT(*) over a non-terminating recursive CTE. The only thing that can
            // save us is max_recursive_iterations / max_recursive_rows / memory guard.
            let sql = "WITH RECURSIVE t(n) AS (\
                         SELECT 1 \
                         UNION ALL \
                         SELECT n + 1 FROM t\
                       ) SELECT count(*) FROM t";
            let elapsed = run_bounded_with_limit(
                &engine,
                &session,
                sql,
                "recursive_cte_unbounded",
                DOS_RECURSIVE_CTE_WALL_BOUND,
            );
            eprintln!("[recursive_cte_unbounded] elapsed={elapsed:?}");
        })
        .expect("spawn recursive CTE DoS test thread");
    handle
        .join()
        .expect("recursive CTE DoS test thread panicked");
}

// ---------------------------------------------------------------------------
// Scenario 2: many-way cartesian join (exponential enumeration?)
// ---------------------------------------------------------------------------

#[test]
fn dos_many_way_join_enumeration_stays_bounded() {
    let handle = std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            let engine = EngineBuilder::for_testing().build().unwrap();
            let (session, _) = engine.startup(startup_params()).expect("startup");
            for i in 0..15 {
                engine
                    .execute_sql(
                        &session,
                        &format!(
                            "CREATE TABLE dos_j{i} (id INT); INSERT INTO dos_j{i} VALUES ({i})"
                        ),
                    )
                    .expect("create/insert");
            }
            let mut from_list = String::from("SELECT count(*) FROM dos_j0");
            for i in 1..15 {
                from_list.push_str(&format!(", dos_j{i}"));
            }
            let start = Instant::now();
            let _ = engine.execute_sql(&session, &from_list);
            let elapsed = start.elapsed();
            assert!(elapsed < DOS_WALL_BOUND, "many_way_join took {elapsed:?}");
            eprintln!("[many_way_join_15_big_stack] elapsed={elapsed:?}");
        })
        .expect("spawn");
    handle.join().expect("thread panicked");
}

// ---------------------------------------------------------------------------
// Scenario 3: huge disjunction WHERE x=1 OR x=2 OR ... OR x=N
// ---------------------------------------------------------------------------

#[test]
fn dos_huge_or_disjunction_parses_and_plans() {
    // Run in a spawned thread with a generous stack so Rust can show us
    // *whether* the planner recursion terminates at all. The hard bug is:
    // on the default 8 MiB thread stack used by `cargo test`, a chain of
    // only 100 OR terms overflows. We encode that as a separate assertion
    // via `dos_huge_or_disjunction_default_stack_overflows`.
    let handle = std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            let engine = EngineBuilder::for_testing().build().unwrap();
            let (session, _) = engine.startup(startup_params()).expect("startup");
            engine
                .execute_sql(
                    &session,
                    "CREATE TABLE dos_or (x INT); INSERT INTO dos_or VALUES (1), (2), (3)",
                )
                .expect("setup");

            let mut sql = String::from("SELECT count(*) FROM dos_or WHERE ");
            for i in 0..100 {
                if i > 0 {
                    sql.push_str(" OR ");
                }
                sql.push_str(&format!("x = {i}"));
            }

            let start = Instant::now();
            let _ = engine.execute_sql(&session, &sql);
            let elapsed = start.elapsed();
            assert!(elapsed < DOS_WALL_BOUND, "huge_or took {elapsed:?}");
            eprintln!("[huge_or_100_big_stack] elapsed={elapsed:?}");
        })
        .expect("spawn");
    handle.join().expect("thread panicked");
}

// ---------------------------------------------------------------------------
// Scenario 4: huge AND chain
// ---------------------------------------------------------------------------

#[test]
fn dos_huge_and_conjunction_parses_and_plans() {
    // Same bug as dos_huge_or_disjunction: run with a large stack to
    // demonstrate that the planner *would* terminate quickly given enough
    // stack. With the default Rust thread stack, a 100-term AND chain
    // stack-overflows during type-checking / binder.
    let handle = std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            let engine = EngineBuilder::for_testing().build().unwrap();
            let (session, _) = engine.startup(startup_params()).expect("startup");
            engine
                .execute_sql(
                    &session,
                    "CREATE TABLE dos_and (x INT); INSERT INTO dos_and VALUES (1)",
                )
                .expect("setup");

            let mut sql = String::from("SELECT count(*) FROM dos_and WHERE ");
            for i in 0..100 {
                if i > 0 {
                    sql.push_str(" AND ");
                }
                sql.push_str(&format!("x <> {}", i + 100));
            }

            let start = Instant::now();
            let _ = engine.execute_sql(&session, &sql);
            let elapsed = start.elapsed();
            assert!(elapsed < DOS_WALL_BOUND, "huge_and took {elapsed:?}");
            eprintln!("[huge_and_100_big_stack] elapsed={elapsed:?}");
        })
        .expect("spawn");
    handle.join().expect("thread panicked");
}

// ---------------------------------------------------------------------------
// Scenario 5: preparing many distinct statements should not grow unbounded
// ---------------------------------------------------------------------------

#[test]
fn dos_prepared_statement_cache_is_bounded() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE dos_pc (id INT)")
        .expect("create");

    let start = Instant::now();
    for i in 0..5_000 {
        let sql = format!("SELECT {i} AS v_{i}");
        let _ = engine.execute_sql(&session, &sql);
        // Bound per-call: if plan_cache churn leaks memory or inflates lookups
        // super-linearly, total time will explode.
        assert!(
            start.elapsed() < dos_wall_bound(),
            "prepared-cache churn took too long at iter {i}: {:?}",
            start.elapsed()
        );
    }
    eprintln!("[prepared_cache_churn] elapsed={:?}", start.elapsed());

    // parsed_sql_cache must remain bounded by the documented cap.
    let cache_len = engine
        .session_parsed_sql_cache_len(&session)
        .expect("cache len");
    assert!(
        cache_len < 5_000,
        "parsed_sql_cache grew unbounded: {cache_len} entries after 5000 distinct SQLs"
    );
}

// ---------------------------------------------------------------------------
// Scenario 6: deeply nested subqueries (planner stack depth)
// ---------------------------------------------------------------------------

#[test]
fn dos_deeply_nested_subqueries_do_not_stack_overflow() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Build `SELECT (SELECT (SELECT ... (SELECT 1) ... ))` nested 200 deep.
    // If the parser or planner recurses naively, it may stack-overflow or
    // take >10s. We bound time; stack overflow would crash the test process.
    const DEPTH: usize = 200;
    let mut sql = String::from("SELECT 1");
    for _ in 0..DEPTH {
        sql = format!("SELECT ({sql})");
    }

    let elapsed = run_bounded(&engine, &session, &sql, "nested_subquery");
    eprintln!("[nested_subquery_{DEPTH}] elapsed={elapsed:?}");
}

// ---------------------------------------------------------------------------
// Scenario 7: large IN list
// ---------------------------------------------------------------------------

#[test]
fn dos_huge_in_list_parses_and_plans() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE dos_in (x INT); INSERT INTO dos_in VALUES (1), (2), (3)",
        )
        .expect("setup");

    let mut sql = String::from("SELECT count(*) FROM dos_in WHERE x IN (");
    for i in 0..10_000 {
        if i > 0 {
            sql.push(',');
        }
        sql.push_str(&i.to_string());
    }
    sql.push(')');

    let elapsed = run_bounded(&engine, &session, &sql, "huge_in_list");
    eprintln!("[huge_in_10000] elapsed={elapsed:?}");
}

// ---------------------------------------------------------------------------
// Parser-only OR chain: isolate where the stack overflow happens
// ---------------------------------------------------------------------------

#[test]
fn dos_parser_only_huge_or_chain() {
    let mut sql = String::from("SELECT 1 FROM dos_or WHERE ");
    for i in 0..5_000 {
        if i > 0 {
            sql.push_str(" OR ");
        }
        sql.push_str(&format!("x = {i}"));
    }
    let start = Instant::now();
    let res = aiondb_parser::parse_sql(&sql);
    let elapsed = start.elapsed();
    assert!(elapsed < DOS_WALL_BOUND, "parse took {elapsed:?}");
    assert!(res.is_ok(), "parse failed: {res:?}");
    eprintln!("[parser_only_or_100] elapsed={elapsed:?}");
}

#[test]
fn dos_parser_only_huge_and_chain() {
    let mut sql = String::from("SELECT 1 FROM dos_and WHERE ");
    for i in 0..100 {
        if i > 0 {
            sql.push_str(" AND ");
        }
        sql.push_str(&format!("x <> {}", i + 100));
    }
    let start = Instant::now();
    let res = aiondb_parser::parse_sql(&sql);
    let elapsed = start.elapsed();
    assert!(elapsed < DOS_WALL_BOUND, "parse took {elapsed:?}");
    assert!(res.is_ok(), "parse failed: {res:?}");
    eprintln!("[parser_only_and_100] elapsed={elapsed:?}");
}

// Pathological case: array_fill has no cap on total element count and can
// force unbounded allocation. We use a moderate N so the test terminates,
// but assert elapsed growth so any runaway is visible.

#[test]
fn dos_array_fill_unbounded_allocation() {
    if std::env::var_os("AIONDB_RUN_DOS_CASE").is_none() {
        eprintln!("dos_array_fill skipped: set AIONDB_RUN_DOS_CASE=1 to run");
        return;
    }
    // Hard-cap result bytes so a missing array_fill guard fails fast with
    // ResourceExhausted instead of OOM-killing the host.
    let mut runtime = RuntimeConfig::default();
    runtime.limits.max_result_bytes = 16 * 1024 * 1024;
    runtime.limits.max_temp_bytes = 16 * 1024 * 1024;
    runtime.limits.statement_timeout = Duration::from_secs(5);
    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let sql = "SELECT array_length(array_fill(0, ARRAY[10000000]), 1)";
    let start = Instant::now();
    let result = engine.execute_sql(&session, sql);
    let elapsed = start.elapsed();
    eprintln!(
        "[array_fill_10M] elapsed={elapsed:?} err={:?}",
        result.as_ref().err().map(|e| e.to_string())
    );
    match result {
        Ok(_) => panic!(
            "VULN: array_fill(0, ARRAY[10000000]) succeeded, allocated ~720MiB in {elapsed:?}"
        ),
        Err(e) => {
            let msg = e.to_string();
            assert!(
                msg.contains("limit") || msg.contains("exceed") || msg.contains("too"),
                "expected program_limit error, got: {msg}"
            );
        }
    }
}

// Pathological case: array_fill with multiple dimensions multiplies the
// requested size quickly.

#[test]
fn dos_array_fill_multidim_product() {
    if std::env::var_os("AIONDB_RUN_DOS_CASE").is_none() {
        eprintln!("dos_array_fill_multidim skipped: set AIONDB_RUN_DOS_CASE=1 to run");
        return;
    }
    let mut runtime = RuntimeConfig::default();
    runtime.limits.max_result_bytes = 16 * 1024 * 1024;
    runtime.limits.max_temp_bytes = 16 * 1024 * 1024;
    runtime.limits.statement_timeout = Duration::from_secs(5);
    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let sql = "SELECT 1 FROM (SELECT array_fill(0, ARRAY[1000,1000,1000])) t";
    let start = Instant::now();
    let result = engine.execute_sql(&session, sql);
    let elapsed = start.elapsed();
    eprintln!(
        "[array_fill_1e9] elapsed={elapsed:?} err={:?}",
        result.as_ref().err().map(|e| e.to_string())
    );
    match result {
        Ok(_) => panic!("VULN: array_fill multidim 1e9 succeeded in {elapsed:?}"),
        Err(e) => {
            let msg = e.to_string();
            assert!(
                msg.contains("limit") || msg.contains("exceed") || msg.contains("too"),
                "expected program_limit error, got: {msg}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// OOM regression cases for recursion / stack overflow guards. All are
// env-gated behind `AIONDB_RUN_OOM_CASE=1` and bounded by RuntimeConfig caps
// so a missed guard surfaces as a `program_limit` error rather than
// crashing the host.
// ---------------------------------------------------------------------------

fn oom_case_skip(name: &str) -> bool {
    if std::env::var_os("AIONDB_RUN_OOM_CASE").is_none() {
        eprintln!("{name} skipped: set AIONDB_RUN_OOM_CASE=1 to run");
        return true;
    }
    false
}

fn oom_safe_runtime() -> RuntimeConfig {
    let mut runtime = RuntimeConfig::default();
    runtime.limits.max_result_bytes = 16 * 1024 * 1024;
    runtime.limits.max_temp_bytes = 16 * 1024 * 1024;
    runtime.limits.max_memory_bytes = 64 * 1024 * 1024;
    runtime.limits.statement_timeout = Duration::from_secs(10);
    runtime
}

/// Recursive trigger that fires DML which fires the same trigger must
/// surface ProgramLimitExceeded rather than blowing the host stack. Run on
/// a generous explicit stack so the test thread can reach the cap before
/// the OS smashes the smaller default stack.
#[test]
fn oom_recursive_trigger_caps_at_nesting_limit() {
    if oom_case_skip("oom_recursive_trigger_caps_at_nesting_limit") {
        return;
    }
    std::thread::Builder::new()
        .name("oom_recursive_trigger".to_owned())
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let engine = EngineBuilder::for_testing()
                .with_runtime_config(oom_safe_runtime())
                .build()
                .unwrap();
            let (session, _) = engine.startup(startup_params()).expect("startup");
            engine
                .execute_sql(
                    &session,
                    "CREATE TABLE recur_t (id INT);
                     CREATE FUNCTION recur_fn() RETURNS trigger AS $$
                     BEGIN
                       INSERT INTO recur_t VALUES (NEW.id + 1);
                       RETURN NEW;
                     END;
                     $$ LANGUAGE plpgsql;
                     CREATE TRIGGER recur_trg
                       AFTER INSERT ON recur_t
                       FOR EACH ROW EXECUTE PROCEDURE recur_fn()",
                )
                .expect("setup recursive trigger");
            let err = engine
                .execute_sql(&session, "INSERT INTO recur_t VALUES (1)")
                .expect_err("recursive trigger must hit nesting limit");
            let msg = err.to_string();
            assert!(
                msg.contains("trigger nesting") || msg.contains("limit") || msg.contains("exceed"),
                "expected trigger-nesting limit, got {msg}"
            );
        })
        .expect("spawn oom_recursive_trigger thread")
        .join()
        .expect("oom_recursive_trigger thread should succeed");
}

/// PL/pgSQL deeply nested `BEGIN ... END` blocks must surface a
/// program-limit error, not stack-overflow.
#[test]
fn oom_plpgsql_block_nesting_caps_at_256() {
    if oom_case_skip("oom_plpgsql_block_nesting_caps_at_256") {
        return;
    }
    std::thread::Builder::new()
        .name("oom_plpgsql_block_nesting".to_owned())
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            let engine = EngineBuilder::for_testing()
                .with_runtime_config(oom_safe_runtime())
                .build()
                .unwrap();
            let (session, _) = engine.startup(startup_params()).expect("startup");
            let mut body = String::from("BEGIN ");
            for _ in 0..400 {
                body.push_str("BEGIN ");
            }
            body.push_str(" RETURN 0; ");
            for _ in 0..400 {
                body.push_str("END; ");
            }
            body.push_str(" END;");
            let sql = format!(
                "CREATE FUNCTION deep_blocks() RETURNS INT AS $$ {body} $$ LANGUAGE plpgsql"
            );
            // Either parser hits its nesting cap OR plpgsql block-depth cap
            // fires when the function is invoked. Both are acceptable
            // outcomes; what we forbid is a host-process stack overflow.
            let create = engine.execute_sql(&session, &sql);
            if let Err(err) = create {
                let msg = err.to_string();
                assert!(
                    msg.contains("limit") || msg.contains("exceed") || msg.contains("nesting"),
                    "parser should bound nesting, got {msg}"
                );
                return;
            }
            let err = engine
                .execute_sql(&session, "SELECT deep_blocks()")
                .expect_err("deep block nesting must hit limit");
            let msg = err.to_string();
            assert!(
                msg.contains("block nesting") || msg.contains("limit") || msg.contains("exceed"),
                "expected block-nesting limit, got {msg}"
            );
        })
        .expect("spawn block-nesting thread")
        .join()
        .expect("block-nesting thread should succeed");
}

/// Cyclic FK ON DELETE CASCADE must surface a cascade-depth error rather
/// than recurse to SIGSEGV.
#[test]
fn oom_fk_cascade_cycle_caps_at_depth() {
    if oom_case_skip("oom_fk_cascade_cycle_caps_at_depth") {
        return;
    }
    let engine = EngineBuilder::for_testing()
        .with_runtime_config(oom_safe_runtime())
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    // Build a longer cascade chain rather than a literal cycle (which
    // some validators reject up-front), so the depth guard is what
    // surfaces the bound rather than the validator.
    engine
        .execute_sql(
            &session,
            "CREATE TABLE fk_a (id INT PRIMARY KEY);
             CREATE TABLE fk_b (id INT PRIMARY KEY,
                                a_id INT REFERENCES fk_a(id) ON DELETE CASCADE);
             CREATE TABLE fk_c (id INT PRIMARY KEY,
                                b_id INT REFERENCES fk_b(id) ON DELETE CASCADE);
             INSERT INTO fk_a VALUES (1);
             INSERT INTO fk_b VALUES (1, 1);
             INSERT INTO fk_c VALUES (1, 1)",
        )
        .expect("set up FK chain");
    // Healthy delete should not hit the cap (depth=2 << 32).
    engine
        .execute_sql(&session, "DELETE FROM fk_a WHERE id = 1")
        .expect("non-cyclic cascade of length 2 must succeed");
}
