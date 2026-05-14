mod setup;
mod setup_temporal;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as FmtWrite;
use std::panic;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use aiondb_core::value::pg_jsonb_to_string;
use aiondb_core::Value;
use aiondb_engine::{
    Credential, Engine, EngineBuilder, QueryEngine, SessionHandle, StartupParams, StatementResult,
    TransportInfo,
};
use aiondb_parser::Statement;

const HARD_EXCLUDED_FILES: &[&str] = &[];
const MISSING_SQL_ECHO_SENTINEL: &str = "__PG_REGRESS_MISSING_SQL_ECHO__";
const PG_REGRESS_STATEMENT_TIMEOUT: Duration = Duration::from_secs(60);
const PG_REGRESS_FILE_TIMEOUT: Duration = Duration::from_secs(60);
const PG_REGRESS_THREAD_TIMEOUT: Duration = Duration::from_secs(65);

fn env_var_truthy(name: &str) -> bool {
    std::env::var(name).ok().is_some_and(|value| {
        let normalized = value.trim().to_ascii_lowercase();
        matches!(normalized.as_str(), "1" | "true" | "yes" | "on")
    })
}

fn main() {
    let builder = std::thread::Builder::new().stack_size(32 * 1024 * 1024);
    let handler = builder.spawn(real_main).expect("Failed to spawn thread");
    handler.join().expect("Thread panicked");
}

/// Detail about a single mismatch, used in debug mode.
struct MismatchDetail {
    sql: String,
    expected: String,
    actual: String,
    category: String,
}

#[derive(Clone, Debug)]
struct ParsedSqlStmt {
    sql: String,
    suppress_output: bool,
    /// When true, the output rows of this query are executed as SQL
    /// statements (psql `\gexec` behavior).
    gexec: bool,
}

fn real_main() {
    unsafe {
        std::env::set_var("TZ", "PST8PDT");
    }
    let debug_mode = std::env::var("PG_REGRESS_DEBUG").is_ok();
    let file_filter = std::env::var("PG_REGRESS_FILE").ok();

    let base_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let sql_dir = base_dir.join("sql");
    let expected_dir = base_dir.join("expected");

    // Collect all .sql files — each has a matching .out file
    let mut all_sql_files: Vec<PathBuf> = std::fs::read_dir(&sql_dir)
        .expect("cannot read sql/")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "sql"))
        .collect();
    all_sql_files.sort();

    let excluded_files: Vec<String> = all_sql_files
        .iter()
        .filter_map(|path| {
            let name = path.file_stem().unwrap().to_str().unwrap();
            HARD_EXCLUDED_FILES
                .contains(&name)
                .then(|| name.to_string())
        })
        .collect();
    let mut sql_files: Vec<PathBuf> = all_sql_files
        .into_iter()
        .filter(|path| {
            let name = path.file_stem().unwrap().to_str().unwrap();
            !HARD_EXCLUDED_FILES.contains(&name)
        })
        .collect();

    // If PG_REGRESS_FILE is set, only run that single file
    if let Some(ref filter) = file_filter {
        sql_files.retain(|p| p.file_stem().unwrap().to_str().unwrap() == filter.as_str());
        if sql_files.is_empty() {
            if excluded_files.iter().any(|name| name == filter) {
                eprintln!(
                    "ERROR: PG_REGRESS_FILE={} is hard-excluded by the runner ({})",
                    filter,
                    excluded_files.join(", ")
                );
                return;
            }
            eprintln!(
                "ERROR: no .sql file found matching PG_REGRESS_FILE={}",
                filter
            );
            return;
        }
        if debug_mode {
            eprintln!("DEBUG: filtering to single file: {}", filter);
        }
    }

    let total_files = sql_files.len();
    let mut total_stmts = 0usize;
    let mut total_matched = 0usize;
    let mut file_results: Vec<(String, usize, usize)> = Vec::new();
    let mut mismatch_cat_map: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut missing_expected_files: Vec<String> = Vec::new();
    let mut total_unmatched_sql = 0usize;

    let start = Instant::now();

    for sql_path in &sql_files {
        let name = sql_path.file_stem().unwrap().to_str().unwrap().to_string();
        let out_path = expected_dir.join(format!("{}.out", name));

        if !out_path.exists() {
            eprintln!("SKIP|{}|no .out file", name);
            missing_expected_files.push(name.clone());
            file_results.push((name, 0, 0));
            continue;
        }

        eprintln!("FILE|{}", name);

        let sql_content = match std::fs::read_to_string(sql_path) {
            Ok(c) => c,
            Err(_) => {
                eprintln!("SKIP|{}|non-UTF-8 .sql file", name);
                file_results.push((name, 0, 0));
                continue;
            }
        };
        let out_content = match std::fs::read_to_string(&out_path) {
            Ok(c) => c,
            Err(_) => {
                eprintln!("SKIP|{}|non-UTF-8 .out file", name);
                file_results.push((name, 0, 0));
                continue;
            }
        };

        // Parse SQL statements from .sql file
        let sql_stmts = parse_sql_file(&sql_content);
        // Extract expected outputs from .out file by matching SQL echoes
        let test_cases = match_sql_to_expected(&sql_stmts, &out_content);
        let unmatched_sql_count = test_cases
            .iter()
            .filter(|(_, expected)| expected == MISSING_SQL_ECHO_SENTINEL)
            .count();
        total_unmatched_sql += unmatched_sql_count;
        if unmatched_sql_count > 0 {
            eprintln!("UNMATCHED|{}|{}", name, unmatched_sql_count);
        }
        let shadowed_setup_objects = setup_shadowed_objects_in_file(&sql_stmts);
        let case_count = test_cases.len();

        // Run in separate thread with timeout
        let want_debug = debug_mode;
        let handle = thread::spawn(move || {
            let engine = build_engine();
            let session = create_session(&engine);
            let _ = setup::run_setup(&engine, &session, &shadowed_setup_objects, None, None);

            let mut matched = 0usize;
            let mut mismatches: Vec<String> = Vec::new();
            let mut debug_details: Vec<MismatchDetail> = Vec::new();
            let file_start = Instant::now();

            for (stmt, expected) in &test_cases {
                if file_start.elapsed() > PG_REGRESS_FILE_TIMEOUT {
                    break;
                }

                let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                    engine.execute_sql(&session, &stmt.sql)
                }));

                let actual = match result {
                    Ok(Ok(results)) => {
                        if stmt.suppress_output {
                            String::new()
                        } else {
                            let mut output = format_results(&results);
                            if stmt.gexec {
                                let gexec_out = execute_gexec(&engine, &session, &results);
                                eprintln!(
                                    "DEBUG GEXEC: sql={:?}, gexec_output_len={}",
                                    &stmt.sql[..50.min(stmt.sql.len())],
                                    gexec_out.len()
                                );
                                output.push_str(&gexec_out);
                            }
                            output
                        }
                    }
                    Ok(Err(e)) => format_error(&e),
                    Err(_) => "PANIC\n".to_string(),
                };

                if outputs_match(&stmt.sql, &actual, expected) {
                    matched += 1;
                } else {
                    let cat = classify_mismatch(expected, &actual);
                    // Capture debug details: first 10 per category, max 50 total
                    // (shows a diverse sample of mismatch types)
                    let cat_count = debug_details.iter().filter(|d| d.category == cat).count();
                    if want_debug && debug_details.len() < 300 && cat_count < 250 {
                        debug_details.push(MismatchDetail {
                            sql: stmt.sql.clone(),
                            expected: expected.clone(),
                            actual: actual.clone(),
                            category: cat.clone(),
                        });
                    }
                    mismatches.push(cat);
                }
            }

            let _ = engine.terminate(session);
            (matched, mismatches, debug_details)
        });

        let wait_start = Instant::now();
        let join_result = loop {
            if handle.is_finished() {
                break handle.join();
            }
            if wait_start.elapsed() > PG_REGRESS_THREAD_TIMEOUT {
                break Err(Box::new("thread timeout") as Box<dyn std::any::Any + Send>);
            }
            thread::sleep(Duration::from_millis(100));
        };

        let (matched, mismatches, debug_details) = match join_result {
            Ok((m, mm, dd)) => (m, mm, dd),
            Err(_) => {
                eprintln!("TIMEOUT|{}", name);
                (0, vec!["TIMEOUT".to_string()], Vec::new())
            }
        };

        // Print debug details if enabled
        if debug_mode && !debug_details.is_empty() {
            println!("\n{}", "~".repeat(80));
            println!(
                "  DEBUG: {} — first {} mismatch(es)",
                name,
                debug_details.len()
            );
            println!("{}", "~".repeat(80));
            for (i, d) in debug_details.iter().enumerate() {
                println!("\n  --- Mismatch #{} [{}] ---", i + 1, d.category);
                println!("  SQL: {}", d.sql.lines().next().unwrap_or(""));
                if d.sql.lines().count() > 1 {
                    for extra in d.sql.lines().skip(1).take(2) {
                        println!("       {}", extra);
                    }
                    if d.sql.lines().count() > 3 {
                        println!("       ... ({} lines total)", d.sql.lines().count());
                    }
                }
                println!("  EXPECTED (first 5 lines):");
                if d.expected.is_empty() {
                    println!("    (empty)");
                } else {
                    for line in d.expected.lines().take(5) {
                        println!("    |{}", line);
                    }
                    let exp_lines = d.expected.lines().count();
                    if exp_lines > 5 {
                        println!("    ... ({} lines total)", exp_lines);
                    }
                }
                println!("  ACTUAL (first 5 lines):");
                if d.actual.trim().is_empty() {
                    println!("    (empty)");
                } else {
                    for line in d.actual.lines().take(5) {
                        println!("    |{}", line);
                    }
                    let act_lines = d.actual.lines().count();
                    if act_lines > 5 {
                        println!("    ... ({} lines total)", act_lines);
                    }
                }
            }
            println!("{}", "~".repeat(80));
        }

        for cat in &mismatches {
            *mismatch_cat_map.entry(cat.clone()).or_insert(0) += 1;
        }

        total_stmts += case_count;
        total_matched += matched;
        file_results.push((name, matched, case_count));
    }

    let elapsed = start.elapsed();

    // ── Output report ──
    println!("\n{}", "=".repeat(80));
    println!("  PG16 Regression — AionDB Output-Compared Compatibility Report");
    println!("{}", "=".repeat(80));

    println!(
        "\n{:<40} {:>8} {:>8} {:>8}",
        "Test File", "Match", "Total", "Rate"
    );
    println!("{}", "-".repeat(80));

    for (name, matched, total) in &file_results {
        if *total == 0 {
            continue;
        }
        let rate = *matched as f64 / *total as f64 * 100.0;
        let marker = if *matched == *total {
            " OK"
        } else if *matched > 0 {
            "   "
        } else {
            " !!"
        };
        println!(
            "{:<40} {:>8} {:>8} {:>6.1}%{}",
            name, matched, total, rate, marker
        );
    }

    println!("{}", "-".repeat(80));

    let pct = if total_stmts > 0 {
        total_matched as f64 / total_stmts as f64 * 100.0
    } else {
        0.0
    };

    let files_perfect = file_results
        .iter()
        .filter(|(_, m, t)| *t > 0 && *m == *t)
        .count();

    println!("\n{}", "=".repeat(80));
    println!("  SUMMARY (output-compared, counted files only)");
    println!("{}", "=".repeat(80));
    println!("  Counted files:     {}", total_files);
    println!("  Hard exclusions:   {}", excluded_files.len());
    if !excluded_files.is_empty() {
        println!("  Excluded names:    {}", excluded_files.join(", "));
    }
    println!("  Missing expected:  {}", missing_expected_files.len());
    if !missing_expected_files.is_empty() {
        println!("  Missing .out:      {}", missing_expected_files.join(", "));
    }
    println!("  Unmatched SQL:     {}", total_unmatched_sql);
    println!(
        "  Fully matching:    {} ({:.1}%)",
        files_perfect,
        files_perfect as f64 / total_files as f64 * 100.0
    );
    println!("  Total statements:  {}", total_stmts);
    println!(
        "  Output matched:    {} / {} ({:.1}%)",
        total_matched, total_stmts, pct
    );
    println!("  Time:              {:.2}s", elapsed.as_secs_f64());
    println!("{}", "=".repeat(80));

    // Top 20 best files
    println!("\n  TOP 20 BEST-MATCHING FILES:");
    println!("{}", "-".repeat(55));
    let mut ranked: Vec<_> = file_results
        .iter()
        .filter(|(_, _, t)| *t > 3)
        .map(|(n, m, t)| (n.as_str(), *m, *t, *m as f64 / *t as f64 * 100.0))
        .collect();
    ranked.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap());
    for (name, m, t, rate) in ranked.iter().take(20) {
        println!("  {:<35} {:>4}/{:>4}  ({:>5.1}%)", name, m, t, rate);
    }

    // Top 20 worst files
    println!("\n  TOP 20 WORST-MATCHING FILES:");
    println!("{}", "-".repeat(55));
    ranked.sort_by(|a, b| a.3.partial_cmp(&b.3).unwrap());
    for (name, m, t, rate) in ranked.iter().take(20) {
        println!("  {:<35} {:>4}/{:>4}  ({:>5.1}%)", name, m, t, rate);
    }

    // Mismatch categories
    println!("\n{}", "=".repeat(80));
    println!("  MISMATCH CATEGORIES");
    println!("{}", "=".repeat(80));
    let mut cats: Vec<_> = mismatch_cat_map.into_iter().collect();
    cats.sort_by(|a, b| b.1.cmp(&a.1));
    let total_mm: usize = cats.iter().map(|(_, c)| c).sum();
    for (cat, count) in cats.iter().take(200) {
        let pct = *count as f64 / total_mm.max(1) as f64 * 100.0;
        println!("  {:>6} ({:>5.1}%)  {}", count, pct, cat);
    }

    // Count column-does-not-exist errors specifically
    let col_dne: usize = cats
        .iter()
        .filter(|(c, _)| c.contains("column") && c.contains("does not exist"))
        .map(|(_, n)| n)
        .sum();
    println!("\n  COLUMN-DOES-NOT-EXIST total: {}", col_dne);
    println!("{}", "=".repeat(80));
}

// ═══════════════════════════════════════════════════════════════════════
//  Parse .sql file into individual SQL statements
// ═══════════════════════════════════════════════════════════════════════

fn parse_sql_file(content: &str) -> Vec<ParsedSqlStmt> {
    let mut stmts: Vec<ParsedSqlStmt> = Vec::new();
    let mut current = String::new();
    let mut in_block_comment = false; // tracks multi-line /* */ between stmts
                                      // Count gexec for debug
    let gexec_count = content
        .lines()
        .filter(|l| l.trim().starts_with("\\gexec"))
        .count();
    eprintln!(
        "DEBUG: parse_sql_file lines={} gexec_count={}",
        content.lines().count(),
        gexec_count
    );

    for line in content.lines() {
        let trimmed = line.trim();

        // Track multi-line block comments between statements
        if in_block_comment {
            if trimmed.contains("*/") {
                in_block_comment = false;
            }
            continue;
        }

        // When not currently building a statement, skip blanks/comments/meta-commands
        if current.trim().is_empty() {
            if trimmed.is_empty() || trimmed.starts_with("--") {
                continue;
            }
            // Handle \gexec when not inside a statement: retroactively mark
            // the previous statement for gexec.
            if trimmed.starts_with("\\gexec") {
                if let Some(last) = stmts.last_mut() {
                    last.gexec = true;
                    eprintln!(
                        "DEBUG GEXEC PARSED: marking stmt={:?}",
                        &last.sql[..50.min(last.sql.len())]
                    );
                }
                continue;
            }
            if trimmed.starts_with('\\') {
                continue;
            }
            // Skip standalone block comment (single-line /* ... */)
            if trimmed.starts_with("/*") && trimmed.ends_with("*/") {
                continue;
            }
            // Start of multi-line block comment between statements
            if trimmed.starts_with("/*") {
                in_block_comment = true;
                continue;
            }
        } else {
            // Inside a multi-line statement:
            // PRESERVE embedded comment lines — the .out file echoes them as
            // part of the multi-line SQL block, so we need them for matching.
            // Only psql meta-commands break the current statement.
            if trimmed.starts_with('\\') {
                let sql = current.trim().to_string();
                if !sql.is_empty() {
                    stmts.push(ParsedSqlStmt {
                        sql,
                        suppress_output: trimmed.starts_with("\\gset"),
                        gexec: trimmed.starts_with("\\gexec"),
                    });
                }
                current.clear();
                continue;
            }
        }

        if let Some(sql_prefix) = split_inline_gset_sql(line) {
            if !current.is_empty() {
                current.push('\n');
            }
            current.push_str(sql_prefix);
            let sql = current.trim().to_string();
            if !sql.is_empty() {
                stmts.push(ParsedSqlStmt {
                    sql,
                    suppress_output: true,
                    gexec: false,
                });
            }
            current.clear();
            continue;
        }

        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(line);

        if stmt_is_complete(current.trim()) {
            stmts.push(ParsedSqlStmt {
                sql: current.trim().to_string(),
                suppress_output: false,
                gexec: false,
            });
            current.clear();
        }
    }

    let remaining = current.trim().to_string();
    if !remaining.is_empty() {
        stmts.push(ParsedSqlStmt {
            sql: remaining,
            suppress_output: false,
            gexec: false,
        });
    }

    stmts
}

fn split_inline_gset_sql(line: &str) -> Option<&str> {
    let trimmed = line.trim_end();
    let bytes = trimmed.as_bytes();
    let mut index = 0;
    let mut in_single_quote = false;
    let mut in_double_quote = false;

    while index < bytes.len() {
        match bytes[index] {
            b'\'' if !in_double_quote => {
                if in_single_quote && bytes.get(index + 1) == Some(&b'\'') {
                    index += 2;
                    continue;
                }
                in_single_quote = !in_single_quote;
            }
            b'"' if !in_single_quote => {
                if in_double_quote && bytes.get(index + 1) == Some(&b'"') {
                    index += 2;
                    continue;
                }
                in_double_quote = !in_double_quote;
            }
            b'-' if !in_single_quote && !in_double_quote && bytes.get(index + 1) == Some(&b'-') => {
                break;
            }
            b'\\' if !in_single_quote && !in_double_quote => {
                let rest = &trimmed[index..];
                if rest.starts_with("\\gset") {
                    let after = rest["\\gset".len()..].chars().next();
                    if after.is_none_or(char::is_whitespace) {
                        return Some(trimmed[..index].trim_end());
                    }
                }
            }
            _ => {}
        }
        index += 1;
    }

    None
}

fn normalize_sql_echo_line(line: &str) -> String {
    if let Some(prefix) = split_inline_gset_sql(line) {
        prefix.trim_end().to_owned()
    } else {
        line.trim_end().to_owned()
    }
}

fn tables_created_in_file(sql_stmts: &[ParsedSqlStmt]) -> BTreeSet<String> {
    let mut tables = BTreeSet::new();
    for stmt in sql_stmts {
        if is_temporary_table_create(&stmt.sql) {
            continue;
        }
        let Ok(parsed) = aiondb_parser::parse_sql(&stmt.sql) else {
            continue;
        };
        for parsed_stmt in parsed {
            match parsed_stmt {
                Statement::CreateTable(create) => {
                    if let Some(name) = create.name.parts.last() {
                        tables.insert(name.to_ascii_lowercase());
                    }
                }
                Statement::CreateTableAs(ctas) => {
                    if let Some(name) = ctas.name.parts.last() {
                        tables.insert(name.to_ascii_lowercase());
                    }
                }
                _ => {}
            }
        }
    }
    tables
}

fn indexes_created_in_file(sql_stmts: &[ParsedSqlStmt]) -> BTreeSet<String> {
    let mut indexes = BTreeSet::new();
    for stmt in sql_stmts {
        let Ok(parsed) = aiondb_parser::parse_sql(&stmt.sql) else {
            continue;
        };
        for parsed_stmt in parsed {
            if let Statement::CreateIndex(create) = parsed_stmt {
                if let Some(name) = create.name.parts.last() {
                    indexes.insert(name.to_ascii_lowercase());
                }
            }
        }
    }
    indexes
}

fn setup_shadowed_objects_in_file(sql_stmts: &[ParsedSqlStmt]) -> BTreeSet<String> {
    let mut objects = tables_created_in_file(sql_stmts);
    objects.extend(indexes_created_in_file(sql_stmts));
    objects
}

#[allow(dead_code)]
fn tables_loaded_from_external_copy(sql_stmts: &[ParsedSqlStmt]) -> BTreeSet<String> {
    sql_stmts
        .iter()
        .filter_map(|stmt| table_loaded_from_external_copy(&stmt.sql))
        .collect()
}

#[allow(dead_code)]
fn table_loaded_from_external_copy(sql: &str) -> Option<String> {
    let trimmed = sql.trim_start();
    let lower = trimmed.to_ascii_lowercase();
    if !lower.starts_with("copy ") || !lower.contains(" from ") || lower.contains(" from stdin") {
        return None;
    }

    let table = trimmed.split_whitespace().nth(1)?;
    let table = table
        .trim_matches(|c: char| c == '"' || c == ',' || c == ';')
        .rsplit('.')
        .next()?;
    Some(table.to_ascii_lowercase())
}

fn is_temporary_table_create(sql: &str) -> bool {
    let tokens: Vec<String> = sql
        .split_whitespace()
        .take(4)
        .map(|token| {
            token
                .trim_matches(|c: char| c == '"' || c == '(')
                .to_ascii_lowercase()
        })
        .collect();

    matches!(
        tokens.as_slice(),
        [create, temp, table, ..]
            if create == "create"
                && table == "table"
                && matches!(temp.as_str(), "temp" | "temporary")
    )
}

/// Check whether a SQL string is complete (ends with `;` outside quotes/dollar-quotes).
fn stmt_is_complete(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return false;
    }

    let upper_trimmed = s.trim_start().to_ascii_uppercase();
    let track_begin_atomic = upper_trimmed.starts_with("CREATE FUNCTION")
        || upper_trimmed.starts_with("CREATE OR REPLACE FUNCTION")
        || upper_trimmed.starts_with("CREATE PROCEDURE")
        || upper_trimmed.starts_with("CREATE OR REPLACE PROCEDURE");

    let mut in_sq = false;
    let mut in_dq = false;
    let mut in_dollar = false;
    let mut in_block_comment = 0i32; // nesting depth
    let mut dollar_tag = String::new();
    let mut paren_depth = 0i32;
    let mut saw_begin_atomic = false;
    let mut begin_atomic_depth = 0i32;
    let mut pending_begin_keyword = false;
    let mut last_semi = false;

    let mut i = 0;
    while i < bytes.len() {
        if in_dollar {
            if bytes[i] == b'$' {
                let remaining = &s[i..];
                let close_tag = format!("${}$", dollar_tag);
                if remaining.starts_with(&close_tag) {
                    i += close_tag.len();
                    in_dollar = false;
                    continue;
                }
            }
            i += 1;
            continue;
        }
        if in_block_comment > 0 {
            if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
                in_block_comment += 1;
                i += 2;
            } else if bytes[i] == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                in_block_comment -= 1;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if in_sq {
            if bytes[i] == b'\'' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                    i += 2; // escaped quote
                    continue;
                }
                in_sq = false;
            }
            i += 1;
            continue;
        }
        if in_dq {
            if bytes[i] == b'"' {
                in_dq = false;
            }
            i += 1;
            continue;
        }
        match bytes[i] {
            b'\'' => {
                in_sq = true;
                i += 1;
            }
            b'"' => {
                in_dq = true;
                i += 1;
            }
            b'$' => {
                let remaining = &s[i + 1..];
                if let Some(end) = remaining.find('$') {
                    let tag = &remaining[..end];
                    if tag.is_empty() || tag.chars().all(|c| c.is_alphanumeric() || c == '_') {
                        dollar_tag = tag.to_string();
                        in_dollar = true;
                        i += 2 + end;
                        continue;
                    }
                }
                i += 1;
            }
            b'-' if i + 1 < bytes.len() && bytes[i + 1] == b'-' => {
                // Line comment: skip to end of current line.
                // Everything after -- is a comment, so it doesn't affect last_semi.
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                // skip the newline itself
                if i < bytes.len() {
                    i += 1;
                }
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                in_block_comment += 1;
                i += 2;
            }
            b'(' => {
                paren_depth += 1;
                last_semi = false;
                i += 1;
            }
            b')' => {
                if paren_depth > 0 {
                    paren_depth -= 1;
                }
                last_semi = false;
                i += 1;
            }
            b';' => {
                last_semi = paren_depth == 0 && (!saw_begin_atomic || begin_atomic_depth == 0);
                i += 1;
            }
            b if b.is_ascii_alphabetic() || b == b'_' => {
                let start = i;
                i += 1;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                if track_begin_atomic {
                    let token = s[start..i].to_ascii_uppercase();
                    if pending_begin_keyword {
                        if token == "ATOMIC" {
                            saw_begin_atomic = true;
                            begin_atomic_depth += 1;
                        }
                        pending_begin_keyword = false;
                    }
                    if token == "BEGIN" {
                        pending_begin_keyword = true;
                    } else if token == "END" && begin_atomic_depth > 0 {
                        begin_atomic_depth -= 1;
                    }
                }
                last_semi = false;
            }
            b if !b.is_ascii_whitespace() => {
                pending_begin_keyword = false;
                last_semi = false;
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }
    last_semi
        && !in_sq
        && !in_dq
        && !in_dollar
        && in_block_comment == 0
        && paren_depth == 0
        && (!saw_begin_atomic || begin_atomic_depth == 0)
}

// ═══════════════════════════════════════════════════════════════════════
//  Match SQL statements to their expected output in the .out file
// ═══════════════════════════════════════════════════════════════════════

/// Normalize whitespace in a line for fuzzy comparison: collapse runs of
/// whitespace (tabs and spaces) into a single space and trim both ends.
fn normalize_ws(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut last_was_ws = true; // start true to trim leading
    for ch in s.chars() {
        if ch == ' ' || ch == '\t' {
            if !last_was_ws {
                result.push(' ');
            }
            last_was_ws = true;
        } else {
            result.push(ch);
            last_was_ws = false;
        }
    }
    // trim trailing space from whitespace collapse
    if result.ends_with(' ') {
        result.pop();
    }
    result
}

/// Returns true if the line looks like a psql meta-command echo (e.g. `\d tablename`).
fn is_psql_metacmd(line: &str) -> bool {
    let trimmed = line.trim();
    if !trimmed.starts_with('\\') {
        return false;
    }
    // Common psql meta-commands that appear in .out echoes
    // \d, \d+, \dt, \di, \ds, \dT, \pset, \set, \copy, \a, \t, \x, \g, \i, \o
    // But NOT lines like "\_" that might occur in data
    if trimmed.len() < 2 {
        return false;
    }
    let second = trimmed.as_bytes()[1];
    // Meta-commands start with a letter after the backslash
    second.is_ascii_alphabetic()
}

/// Returns true if the line looks like output from `\d` (table description):
/// - Table/Index/View headers like "  Table "public.foo""
/// - Column header rows starting with " Column |"
/// - Separator lines like "--------+---------"
/// - Index/constraint/inherit footers like "Indexes:", "Check constraints:", etc.
/// - Indented constraint lines like '    "con1" CHECK ...'
fn is_metacmd_output(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    // Header line from \d: e.g., "  Table "public.foo""
    // These start with spaces and contain a quoted schema-qualified name
    if (trimmed.starts_with("Table \"")
        || trimmed.starts_with("Index \"")
        || trimmed.starts_with("View \"")
        || trimmed.starts_with("Sequence \"")
        || trimmed.starts_with("Materialized view \"")
        || trimmed.starts_with("Composite type \"")
        || trimmed.starts_with("Partitioned table \"")
        || trimmed.starts_with("Partitioned index \""))
        && line.starts_with(' ')
    {
        return true;
    }
    // Footer/annotation lines from \d output
    let footers = [
        "View definition:",
        "Indexes:",
        "Check constraints:",
        "Foreign-key constraints:",
        "Referenced by:",
        "Inherits:",
        "Number of child tables:",
        "Triggers:",
        "Policies:",
        "Rules:",
        "Publications:",
        "Not-null constraints:",
        "Partition key:",
        "Partitions:",
        "Number of partitions:",
        "Child tables:",
        "Tablespace:",
        "Options:",
        "Replica Identity:",
        "Access method:",
        "Typed table of type:",
    ];
    for f in &footers {
        if trimmed.starts_with(f) {
            return true;
        }
    }
    false
}

/// Detect and skip a psql meta-command block starting at `start_idx` in out_lines.
/// Returns the line index PAST the meta-command and its output, or start_idx if
/// the line is not a meta-command.
fn skip_metacmd_block(out_lines: &[&str], start_idx: usize) -> usize {
    if start_idx >= out_lines.len() {
        return start_idx;
    }
    if !is_psql_metacmd(out_lines[start_idx]) {
        return start_idx;
    }
    // Skip the meta-command line itself
    let mut i = start_idx + 1;
    // Skip everything until we find a line that looks like a SQL echo.
    // \d output consists of: header, column headers with pipes, separator
    // lines with dashes/plus, data rows, and footer annotations.
    while i < out_lines.len() {
        let line = out_lines[i];
        let trimmed = line.trim();
        // Blank line: skip it (part of \d output formatting)
        if trimmed.is_empty() {
            i += 1;
            continue;
        }
        // If it's another metacmd, stop (the caller will handle it)
        if is_psql_metacmd(line) {
            break;
        }
        // If it looks like \d output header/footer patterns, consume it
        if is_metacmd_output(line) {
            i += 1;
            continue;
        }
        // Separator lines: all dashes and plus signs (may or may not start with space)
        let is_separator = !trimmed.is_empty() && trimmed.chars().all(|c| c == '-' || c == '+');
        if is_separator {
            i += 1;
            continue;
        }
        // Lines with pipes (column data from \d output)
        if trimmed.contains('|') && (line.starts_with(' ') || trimmed.starts_with('(')) {
            i += 1;
            continue;
        }
        // Indented lines: part of \d output (constraint details, etc.)
        if line.starts_with(' ') {
            i += 1;
            continue;
        }
        // Index method info lines from \d+
        if trimmed.starts_with("btree,")
            || trimmed.starts_with("hash,")
            || trimmed.starts_with("gist,")
            || trimmed.starts_with("gin,")
            || trimmed.starts_with("brin,")
            || trimmed.starts_with("spgist,")
            || trimmed.starts_with("unique,")
            || trimmed.starts_with("unique nulls not distinct,")
            || trimmed.starts_with("primary key,")
        {
            i += 1;
            continue;
        }
        // Row count like "(N rows)" or "(N row)" in \d context
        if trimmed.starts_with('(')
            && trimmed.ends_with(')')
            && (trimmed.contains(" row") || trimmed.contains(" rows"))
        {
            i += 1;
            continue;
        }
        // Otherwise, this line is not part of the metacmd output
        break;
    }
    i
}

/// SQL echo matching should treat blank/comment-only lines as ignorable because
/// pg_regress .out echoes often normalize them away.
fn is_ignorable_sql_echo_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.is_empty() || is_any_comment(line)
}

/// Try to match a parsed SQL statement against .out lines starting at `start_idx`.
/// Matching remains strict on non-comment, non-blank SQL lines, while allowing
/// ignorable lines (blank/comments) to appear/disappear between echoed lines.
fn try_match_sql_echo_at(
    out_lines: &[&str],
    start_idx: usize,
    stmt: &ParsedSqlStmt,
    fuzzy_ws: bool,
) -> Option<(usize, usize)> {
    let stmt_lines: Vec<String> = stmt
        .sql
        .lines()
        .map(normalize_sql_echo_line)
        .filter(|line| !is_ignorable_sql_echo_line(line))
        .collect();
    if stmt_lines.is_empty() {
        return None;
    }

    let mut out_idx = start_idx;
    while out_idx < out_lines.len() && is_ignorable_sql_echo_line(out_lines[out_idx]) {
        out_idx += 1;
    }
    if out_idx >= out_lines.len() {
        return None;
    }
    let echo_start = out_idx;

    for stmt_line in &stmt_lines {
        while out_idx < out_lines.len() {
            let out_line = out_lines[out_idx];
            if is_psql_metacmd(out_line) {
                return None;
            }
            if is_ignorable_sql_echo_line(out_line) {
                out_idx += 1;
                continue;
            }
            break;
        }
        if out_idx >= out_lines.len() {
            return None;
        }

        let out_line = normalize_sql_echo_line(out_lines[out_idx]);
        let matches = if fuzzy_ws {
            normalize_ws(&out_line) == normalize_ws(stmt_line)
        } else {
            out_line == *stmt_line
        };
        if !matches {
            return None;
        }
        out_idx += 1;
    }

    while out_idx < out_lines.len() && is_ignorable_sql_echo_line(out_lines[out_idx]) {
        out_idx += 1;
    }

    Some((echo_start, out_idx))
}

fn match_sql_to_expected(
    sql_stmts: &[ParsedSqlStmt],
    out_content: &str,
) -> Vec<(ParsedSqlStmt, String)> {
    let out_lines: Vec<&str> = out_content.lines().collect();
    let mut pairs = Vec::new();

    // Step 1: find the position (line number) where each SQL echo starts in the .out file
    let mut echo_positions: Vec<(usize, usize)> = Vec::new(); // (start_line, end_line)
    let mut search_from = 0;

    for stmt in sql_stmts {
        let mut found = false;
        // Try exact match first, then fuzzy match
        for pass in 0..2 {
            let mut start = search_from;
            while start < out_lines.len() {
                // Keep SQL matching aligned when psql meta-commands are adjacent
                // to SQL echoes (e.g. \d+ between statements).
                if is_psql_metacmd(out_lines[start]) {
                    let next = skip_metacmd_block(&out_lines, start);
                    start = if next > start { next } else { start + 1 };
                    continue;
                }
                if let Some((echo_start, echo_end)) =
                    try_match_sql_echo_at(&out_lines, start, stmt, pass == 1)
                {
                    echo_positions.push((echo_start, echo_end));
                    search_from = echo_end;
                    found = true;
                    break;
                }
                start += 1;
            }
            if found {
                break;
            }
        }
        if !found {
            // Mark as not found
            echo_positions.push((usize::MAX, usize::MAX));
        }
    }

    // Step 2: extract expected output between consecutive SQL echoes
    for (idx, stmt) in sql_stmts.iter().enumerate() {
        let (echo_start, echo_end) = echo_positions[idx];

        // Output starts right after the SQL echo
        let output_start = echo_end;

        // Output ends at the start of the next found SQL echo, or the next
        // metacmd, or EOF
        let output_end = echo_positions[idx + 1..]
            .iter()
            .find(|(s, _)| *s != usize::MAX)
            .map(|(s, _)| *s)
            .unwrap_or(out_lines.len());

        // Collect output lines, skipping psql meta-command blocks
        let expected = if echo_start == usize::MAX {
            MISSING_SQL_ECHO_SENTINEL.to_owned()
        } else {
            let mut filtered_lines: Vec<&str> = Vec::new();
            let mut li = output_start;
            while li < output_end {
                let line = out_lines[li];
                // If this line is a psql meta-command, skip the entire block
                if is_psql_metacmd(line) {
                    li = skip_metacmd_block(&out_lines, li);
                    continue;
                }
                filtered_lines.push(line);
                li += 1;
            }
            let raw = filtered_lines.join("\n");
            trim_between_stmts(&raw)
        };

        // Build the SQL to execute: strip embedded comment lines (which are
        // preserved for echo-matching but should not be sent to the engine)
        let exec_sql: String = stmt
            .sql
            .lines()
            .filter(|l| !l.trim().starts_with("--"))
            .collect::<Vec<_>>()
            .join("\n");
        let exec_sql = exec_sql.trim().to_string();
        if exec_sql.is_empty() {
            continue;
        }

        pairs.push((
            ParsedSqlStmt {
                sql: exec_sql,
                suppress_output: stmt.suppress_output,
                gexec: stmt.gexec,
            },
            expected,
        ));
    }

    pairs
}

/// Returns true if the line is a SQL comment (e.g. "-- this is a comment").
/// Returns false for PG separator lines like "------+------" or "--------".
fn is_sql_comment(line: &str) -> bool {
    let trimmed = line.trim();
    if !trimmed.starts_with("--") {
        return false;
    }
    // A SQL comment is "-- " followed by text, or just "--"
    // A separator line is all dashes and plus signs: "------+------", "----------"
    // Check: after the leading "--", is everything just dashes, plus, or whitespace?
    let rest = &trimmed[2..];
    if rest.is_empty() {
        return true; // just "--" is a comment
    }
    // If rest contains only '-', '+', whitespace → it's a separator, not a comment
    let is_separator = rest
        .chars()
        .all(|c| c == '-' || c == '+' || c.is_whitespace());
    !is_separator
}

/// Returns true if the line is a standalone block comment like `/* ... */`.
fn is_block_comment(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with("/*") || trimmed.starts_with('*') || trimmed.ends_with("*/")
}

/// Returns true if the line is any kind of comment (SQL line comment or block comment).
fn is_any_comment(line: &str) -> bool {
    is_sql_comment(line) || is_block_comment(line)
}

/// Trim leading/trailing blank lines and comment lines from the gap between
/// two SQL statement echoes, yielding just the expected output.
fn trim_between_stmts(s: &str) -> String {
    let lines: Vec<&str> = s.lines().collect();

    // Find first non-blank, non-comment line
    let start = lines
        .iter()
        .position(|l| {
            let t = l.trim();
            !t.is_empty() && !is_any_comment(l)
        })
        .unwrap_or(lines.len());

    // Find last non-blank, non-comment line
    let end = lines
        .iter()
        .rposition(|l| {
            let t = l.trim();
            !t.is_empty() && !is_any_comment(l)
        })
        .map(|i| i + 1)
        .unwrap_or(start);

    if start >= end {
        return String::new();
    }

    // Collect lines, filtering out comment-only lines within the output
    let filtered: Vec<&str> = lines[start..end]
        .iter()
        .copied()
        .filter(|l| !is_any_comment(l))
        .collect();

    filtered.join("\n")
}

// ═══════════════════════════════════════════════════════════════════════
//  Format AionDB results to PG text format
// ═══════════════════════════════════════════════════════════════════════

fn format_results(results: &[StatementResult]) -> String {
    let mut out = String::new();
    for r in results {
        match r {
            StatementResult::Query { columns, rows } => {
                format_query_result(&mut out, columns, rows);
            }
            StatementResult::Command { .. } => {
                // PG regression tests run with QUIET=true by default:
                // command tags are NOT displayed. No output for DDL/DML.
            }
            StatementResult::CopyIn { .. } => {}
            StatementResult::CopyOut {
                data,
                column_count: _,
            } => {
                out.push_str(data);
            }
            StatementResult::Notice { message } => {
                if message == "there is no transaction in progress"
                    || message == "there is already a transaction in progress"
                {
                    out.push_str("WARNING:  ");
                    out.push_str(message);
                    out.push('\n');
                    continue;
                }
                out.push_str("NOTICE:  ");
                out.push_str(message);
                out.push('\n');
            }
        }
    }
    out
}

/// Implement psql `\gexec`: take each row from the query results,
/// extract the first column's text value, execute it as SQL, and
/// return the combined output (echo of each statement + results).
fn execute_gexec(
    engine: &aiondb_engine::Engine,
    session: &aiondb_engine::SessionHandle,
    results: &[StatementResult],
) -> String {
    let mut out = String::new();
    for r in results {
        let StatementResult::Query { rows, .. } = r else {
            continue;
        };
        for row in rows {
            let Some(val) = row.values.first() else {
                continue;
            };
            let sql_text = match val {
                Value::Text(s) => s.clone(),
                Value::Null => continue,
                other => format!("{other}"),
            };
            let sql_text = sql_text.trim().to_string();
            if sql_text.is_empty() {
                continue;
            }
            // Echo the SQL statement (like psql does)
            out.push_str(&sql_text);
            out.push('\n');
            // Execute it
            match engine.execute_sql(session, &sql_text) {
                Ok(exec_results) => {
                    let exec_output = format_results(&exec_results);
                    out.push_str(&exec_output);
                }
                Err(e) => {
                    out.push_str(&format_error(&e));
                }
            }
        }
    }
    out
}

fn format_query_result(
    out: &mut String,
    columns: &[aiondb_engine::ResultColumn],
    rows: &[aiondb_core::Row],
) {
    if columns.is_empty() && rows.is_empty() {
        return;
    }

    let col_names: Vec<&str> = columns.iter().map(|c| c.name.as_str()).collect();

    let ncols = col_names.len();

    // Format all cell values, padding/truncating to match column count
    let formatted_rows: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            (0..ncols)
                .map(|i| {
                    let v = row.values.get(i).unwrap_or(&Value::Null);
                    format_value(v, columns.get(i))
                })
                .collect()
        })
        .collect();

    // Calculate column widths (min = column name length)
    let mut widths: Vec<usize> = col_names.iter().map(|n| n.len()).collect();
    for row in &formatted_rows {
        for (i, cell) in row.iter().enumerate() {
            if i < widths.len() {
                widths[i] = widths[i].max(cell.len());
            }
        }
    }

    // Header: PG center-aligns column names in (w+2) width, separated by "|"
    let header: Vec<String> = col_names
        .iter()
        .enumerate()
        .map(|(i, n)| {
            let w = widths.get(i).copied().unwrap_or(n.len());
            pad_center(n, w + 2)
        })
        .collect();
    let _ = writeln!(out, "{}", header.join("|"));

    // Separator: "------+------"
    let sep: Vec<String> = widths.iter().map(|w| "-".repeat(*w + 2)).collect();
    let _ = writeln!(out, "{}", sep.join("+"));

    // Data rows
    for row in &formatted_rows {
        let cells: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(i, cell)| {
                let w = widths.get(i).copied().unwrap_or(cell.len());
                // PG aligns based on data type, not value content
                let right_align = columns
                    .get(i)
                    .map(|c| is_numeric_type(&c.data_type))
                    .unwrap_or(false);
                if right_align {
                    pad_right(cell, w)
                } else {
                    pad_left(cell, w)
                }
            })
            .collect();
        let _ = writeln!(out, " {}", cells.join(" | "));
    }

    // Row count footer
    let nrows = rows.len();
    if nrows == 1 {
        let _ = writeln!(out, "(1 row)");
    } else {
        let _ = writeln!(out, "({} rows)", nrows);
    }
}

/// Center text in a field of width `w` (PG header style)
fn pad_center(s: &str, w: usize) -> String {
    if s.len() >= w {
        return s.to_string();
    }
    let total_pad = w - s.len();
    let left_pad = total_pad / 2;
    let right_pad = total_pad - left_pad;
    let mut result = String::with_capacity(w);
    result.extend(std::iter::repeat_n(' ', left_pad));
    result.push_str(s);
    result.extend(std::iter::repeat_n(' ', right_pad));
    result
}

/// Left-align text in a field of width `w`
fn pad_left(s: &str, w: usize) -> String {
    if s.len() >= w {
        s.to_string()
    } else {
        let mut result = s.to_string();
        result.extend(std::iter::repeat_n(' ', w - s.len()));
        result
    }
}

/// Right-align text in a field of width `w`
fn pad_right(s: &str, w: usize) -> String {
    if s.len() >= w {
        s.to_string()
    } else {
        let mut result = String::with_capacity(w);
        result.extend(std::iter::repeat_n(' ', w - s.len()));
        result.push_str(s);
        result
    }
}

/// PG right-aligns numeric types in tabular output.
fn is_numeric_type(dt: &aiondb_core::DataType) -> bool {
    matches!(
        dt,
        aiondb_core::DataType::Int
            | aiondb_core::DataType::BigInt
            | aiondb_core::DataType::Real
            | aiondb_core::DataType::Double
            | aiondb_core::DataType::Numeric
    )
}

#[allow(dead_code)]
fn is_numeric_display(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return false;
    }
    let first = trimmed.as_bytes()[0];
    if first.is_ascii_digit() {
        return true;
    }
    if first == b'-' && trimmed.len() > 1 && trimmed.as_bytes()[1].is_ascii_digit() {
        return true;
    }
    false
}

fn format_value(v: &Value, _col: Option<&aiondb_engine::ResultColumn>) -> String {
    match v {
        Value::Null => String::new(),
        Value::Boolean(b) => if *b { "t" } else { "f" }.to_string(),
        Value::Int(n) => n.to_string(),
        Value::BigInt(n) => n.to_string(),
        Value::Real(f) => {
            if f.is_nan() {
                "NaN".to_string()
            } else if f.is_infinite() {
                if *f > 0.0 { "Infinity" } else { "-Infinity" }.to_string()
            } else {
                format!("{}", f)
            }
        }
        Value::Double(d) => {
            if d.is_nan() {
                "NaN".to_string()
            } else if d.is_infinite() {
                if *d > 0.0 { "Infinity" } else { "-Infinity" }.to_string()
            } else {
                format!("{}", d)
            }
        }
        Value::Numeric(n) => format!("{}", n),
        Value::Text(s) => s.clone(),
        Value::Date(d) => {
            let (year, month, day) = (d.year(), d.month() as u8, d.day());
            format!("{year:04}-{month:02}-{day:02}")
        }
        Value::LargeDate(d) => d.to_string(),
        Value::Time(t) => format_pg_time(t),
        Value::TimeTz(time, offset) => format!("{time}{offset}"),
        Value::Timestamp(ts) => format_pg_timestamp(ts),
        Value::TimestampTz(ts) => format_pg_timestamptz(ts),
        Value::Interval(iv) => format!("{}", iv),
        Value::Blob(b) => {
            let hex: String = b.iter().map(|byte| format!("{:02x}", byte)).collect();
            format!("\\x{}", hex)
        }
        Value::Jsonb(j) => pg_jsonb_to_string(j),
        Value::Uuid(u) => {
            format!(
                "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
                u[0], u[1], u[2], u[3], u[4], u[5], u[6], u[7],
                u[8], u[9], u[10], u[11], u[12], u[13], u[14], u[15]
            )
        }
        Value::MacAddr(v) => v.to_string(),
        Value::MacAddr8(v) => v.to_string(),
        Value::PgLsn(v) => v.to_string(),
        Value::Vector(v) => format!("{:?}", v.values),
        Value::Array(arr) => {
            let elems: Vec<String> = arr.iter().map(|v| format_value(v, None)).collect();
            format!("{{{}}}", elems.join(","))
        }
        Value::Tid(tid) => format!("{}", tid),
        Value::Money(cents) => {
            let abs = cents.unsigned_abs();
            let dollars = abs / 100;
            let frac = abs % 100;
            if *cents < 0 {
                format!("-${dollars}.{frac:02}")
            } else {
                format!("${dollars}.{frac:02}")
            }
        }
    }
}

/// Format a `Time` in PG style: `HH:MM:SS` or `HH:MM:SS.ffffff` (trailing zeros trimmed).
fn format_pg_time(t: &time::Time) -> String {
    let micro = t.microsecond();
    if micro == 0 {
        format!("{:02}:{:02}:{:02}", t.hour(), t.minute(), t.second())
    } else {
        let frac = format!("{micro:06}");
        let trimmed = frac.trim_end_matches('0');
        format!(
            "{:02}:{:02}:{:02}.{trimmed}",
            t.hour(),
            t.minute(),
            t.second()
        )
    }
}

/// Format a `PrimitiveDateTime` in PG style: `YYYY-MM-DD HH:MM:SS[.ffffff]`.
fn format_pg_timestamp(dt: &time::PrimitiveDateTime) -> String {
    let (year, month, day) = (dt.year(), dt.month() as u8, dt.day());
    let (hour, minute, second) = (dt.hour(), dt.minute(), dt.second());
    let micro = dt.microsecond();
    if micro == 0 {
        format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}")
    } else {
        let frac = format!("{micro:06}");
        let trimmed = frac.trim_end_matches('0');
        format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}.{trimmed}")
    }
}

/// Format an `OffsetDateTime` in PG style: `YYYY-MM-DD HH:MM:SS[.ffffff]+OO[:MM]`.
fn format_pg_timestamptz(odt: &time::OffsetDateTime) -> String {
    let (year, month, day) = (odt.year(), odt.month() as u8, odt.day());
    let (hour, minute, second) = (odt.hour(), odt.minute(), odt.second());
    let micro = odt.microsecond();
    let offset = odt.offset();
    let (oh, om, _) = offset.as_hms();
    let base = if micro == 0 {
        format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}")
    } else {
        let frac = format!("{micro:06}");
        let trimmed = frac.trim_end_matches('0');
        format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}.{trimmed}")
    };
    if om == 0 {
        format!("{base}{oh:+03}")
    } else {
        let abs_om = om.unsigned_abs();
        format!("{base}{oh:+03}:{abs_om:02}")
    }
}

fn format_error(e: &aiondb_core::DbError) -> String {
    // PG regression output shows only the message text, not the SQLSTATE code.
    let report = e.report();
    let mut out = format!("ERROR:  {}\n", report.message);
    if let Some(hint) = &report.client_hint {
        out.push_str("HINT:  ");
        out.push_str(hint);
        out.push('\n');
    }
    out
}

// ═══════════════════════════════════════════════════════════════════════
//  Compare outputs
// ═══════════════════════════════════════════════════════════════════════

fn outputs_match(_sql: &str, actual: &str, expected: &str) -> bool {
    if expected == MISSING_SQL_ECHO_SENTINEL {
        return false;
    }

    let actual = normalize_output(actual);
    let expected = normalize_output(expected);
    actual == expected
}

fn explain_outputs_match(sql: &str, actual: &str, expected: &str) -> bool {
    if !sql.trim_start().to_ascii_uppercase().starts_with("EXPLAIN") {
        return false;
    }
    let actual = normalize_output(actual);
    let expected = normalize_output(expected);
    is_query_plan_table(&actual) && is_query_plan_table(&expected)
}

fn is_query_plan_table(output: &str) -> bool {
    let mut lines = output.lines();
    let Some(header) = lines.next() else {
        return false;
    };
    let Some(separator) = lines.next() else {
        return false;
    };
    if !header.contains("QUERY PLAN") || !separator.contains('-') {
        return false;
    }
    lines.any(|line| !line.trim().is_empty() && !line.trim_start().starts_with('('))
}

fn normalize_output(s: &str) -> String {
    let lines: Vec<&str> = s.lines().map(|l| l.trim_end()).collect();

    let start = lines.iter().position(|l| !l.is_empty()).unwrap_or(0);
    let end = lines
        .iter()
        .rposition(|l| !l.is_empty())
        .map(|i| i + 1)
        .unwrap_or(0);

    if start >= end {
        return String::new();
    }

    // Strip LINE N: context lines and their associated caret (^) pointer lines
    // from PostgreSQL error output.
    let mut filtered: Vec<&str> = Vec::new();
    let mut i = start;
    while i < end {
        let line = lines[i];
        if line.starts_with("LINE ") && line.contains(": ") {
            // Skip the LINE context line
            i += 1;
            // Skip the caret line if present (line of spaces ending with ^)
            if i < end && lines[i].trim() == "^" {
                i += 1;
            }
            continue;
        }
        filtered.push(line);
        i += 1;
    }

    filtered.join("\n")
}

fn classify_mismatch(expected: &str, actual: &str) -> String {
    let exp = expected.trim();
    let act = actual.trim();
    if exp == MISSING_SQL_ECHO_SENTINEL {
        return "missing expected SQL echo in .out".to_string();
    }

    if act.starts_with("PANIC") {
        return "PANIC".to_string();
    }
    if act.starts_with("ERROR:") && exp.starts_with("ERROR:") {
        return "wrong error message".to_string();
    }
    if act.starts_with("ERROR:") && !exp.starts_with("ERROR:") {
        let msg = act.lines().next().unwrap_or(act);
        let short = if msg.len() > 60 { &msg[..60] } else { msg };
        return format!("unexpected error: {}", short);
    }
    if !act.starts_with("ERROR:") && exp.starts_with("ERROR:") {
        return "missing expected error".to_string();
    }
    if act.is_empty() && !exp.is_empty() {
        return "no output (expected some)".to_string();
    }
    if !act.is_empty() && exp.is_empty() {
        return "unexpected output (expected none)".to_string();
    }
    "output differs".to_string()
}

// ═══════════════════════════════════════════════════════════════════════
//  Engine helpers
// ═══════════════════════════════════════════════════════════════════════

fn build_engine() -> Engine {
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::new());
    let storage = Arc::new(aiondb_storage_engine::InMemoryStorage::new_without_wal());
    let mut runtime = aiondb_config::RuntimeConfig::default();
    // The in-process regression runner executes heavier seed statements than
    // ordinary unit tests; 30s is too tight for files like bitmapops.
    runtime.limits.statement_timeout = PG_REGRESS_STATEMENT_TIMEOUT;

    EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .with_catalog_txn(catalog.clone())
        .with_catalog_reader(catalog.clone())
        .with_catalog_writer(catalog.clone())
        .with_sequence_manager(catalog)
        .with_storage_ddl(storage.clone())
        .with_storage_dml(storage.clone())
        .with_storage_txn(storage)
        .build()
        .expect("failed to build pg-regress engine")
}

fn create_session(engine: &Engine) -> SessionHandle {
    let mut options = BTreeMap::new();
    options.insert("timezone".to_owned(), "PST8PDT".to_owned());
    options.insert("datestyle".to_owned(), "Postgres, MDY".to_owned());
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("pg-regress".to_owned()),
            options,
            credential: Credential::Anonymous {
                user: "alice".to_owned(),
            },
            transport: TransportInfo::in_process(),
        })
        .expect("startup failed");
    session
}
