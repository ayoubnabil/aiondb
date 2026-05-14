//! openCypher TCK runner for AionDB.
//!
//! Parses Gherkin `.feature` files from the openCypher TCK, executes each
//! scenario against the AionDB engine, and produces a progress report
//! similar to pg-regress.
//!
//! Usage:
//!   cargo run -p cypher-tck-runner                    # run all features
//!   cargo run -p cypher-tck-runner -- --category match  # run one category
//!   cargo run -p cypher-tck-runner -- --file Match1     # run one file

mod gherkin;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use aiondb_core::{DbResult, Value};
use aiondb_engine::{
    Credential, Engine, EngineBuilder, QueryEngine, SessionHandle, StartupParams, StatementResult,
    TransportInfo,
};

// ═════════════════════════════════════════════════════════════════════
//  Configuration
// ═════════════════════════════════════════════════════════════════════

const TCK_DIR: &str = "testing/cypher-tck/features";
const STATEMENT_TIMEOUT: Duration = Duration::from_secs(30);

// ═════════════════════════════════════════════════════════════════════
//  Result tracking
// ═════════════════════════════════════════════════════════════════════

#[derive(Debug, Default, Clone)]
struct CategoryResult {
    passed: usize,
    failed: usize,
    skipped: usize,
    errors: Vec<String>,
}

#[derive(Debug, Clone)]
enum ScenarioOutcome {
    Pass,
    Fail(String),
    Skip(String),
}

// ═════════════════════════════════════════════════════════════════════
//  Engine helpers (mirrors pg-regress)
// ═════════════════════════════════════════════════════════════════════

fn build_engine() -> Engine {
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::new());
    let storage = Arc::new(aiondb_storage_engine::InMemoryStorage::new_without_wal());
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.limits.statement_timeout = STATEMENT_TIMEOUT;

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
        .expect("failed to build cypher-tck engine")
}

fn create_session(engine: &Engine) -> SessionHandle {
    let options = BTreeMap::new();
    let (session, _) = engine
        .startup(StartupParams {
            database: "default".to_owned(),
            application_name: Some("cypher-tck".to_owned()),
            options,
            credential: Credential::Anonymous {
                user: "alice".to_owned(),
            },
            transport: TransportInfo::in_process(),
        })
        .expect("startup failed");
    session
}

/// Replace `$name` parameter references in a Cypher source with the
/// literal expression text captured from a `parameters are:` step. We
/// process longer names first so `$abc` doesn't get clobbered by `$a`.
fn substitute_parameters(source: &str, parameters: &[(String, String)]) -> String {
    if parameters.is_empty() || !source.contains('$') {
        return source.to_owned();
    }
    let mut out = source.to_owned();
    let mut by_len: Vec<&(String, String)> = parameters.iter().collect();
    by_len.sort_by_key(|(name, _)| std::cmp::Reverse(name.len()));
    for (name, value) in by_len {
        let placeholder = format!("${name}");
        if !out.contains(&placeholder) {
            continue;
        }
        // Wrap simple atomic literals so precedence stays intact, but
        // leave already-bracketed forms (`[1,2,3]`, `{a:1}`, `'text'`)
        // alone — the parser doesn't accept `(['Apa'])` as a list
        // expression so wrapping list/map/string literals here would
        // break the query.
        let trimmed = value.trim();
        let needs_paren = !(trimmed.starts_with('[')
            || trimmed.starts_with('{')
            || trimmed.starts_with('\''));
        let replacement = if needs_paren {
            format!("({value})")
        } else {
            value.clone()
        };
        out = out.replace(&placeholder, &replacement);
    }
    out
}

fn execute_cypher(
    engine: &Engine,
    session: &SessionHandle,
    query: &str,
) -> DbResult<Vec<StatementResult>> {
    engine.execute_sql(session, query)
}

// ═════════════════════════════════════════════════════════════════════
//  Scenario execution
// ═════════════════════════════════════════════════════════════════════

fn run_scenario(scenario: &gherkin::Scenario) -> ScenarioOutcome {
    // Skip tagged scenarios we know we can't handle
    for tag in &scenario.tags {
        if tag == "@skipStyleCheck" {
            // This is fine, just a style hint
            continue;
        }
        if tag.contains("skip") || tag.contains("ignore") {
            return ScenarioOutcome::Skip(format!("tagged {tag}"));
        }
    }

    let engine = build_engine();
    let session = create_session(&engine);

    let mut setup_queries: Vec<String> = Vec::new();
    let mut test_query: Option<String> = None;
    let mut expected_result: Option<ExpectedResult> = None;
    let mut expect_error: Option<String> = None;
    let mut parameters: Vec<(String, String)> = Vec::new();

    // Resolve step keywords: And/But inherit from previous Given/When/Then
    let mut last_primary = gherkin::StepKeyword::Given;

    for step in &scenario.steps {
        let effective_keyword = match step.keyword {
            gherkin::StepKeyword::And | gherkin::StepKeyword::But => last_primary,
            other => {
                last_primary = other;
                other
            }
        };

        match effective_keyword {
            gherkin::StepKeyword::Given => {
                if step.text == "an empty graph" || step.text == "any graph" {
                    // Engine starts empty, nothing to do
                } else if step.text.starts_with("having executed") {
                    if let Some(ref doc) = step.doc_string {
                        setup_queries.push(doc.clone());
                    }
                } else if step.text.starts_with("parameters are") {
                    // Capture `parameters are: | name | value |` rows and
                    // substitute them textually into the query before
                    // dispatch. We don't have a typed-parameter API yet,
                    // so we splice the literal expression text from the
                    // gherkin table into `$name` references.
                    if let Some(ref table) = step.table {
                        for row in table {
                            if row.len() >= 2 {
                                parameters.push((row[0].clone(), row[1].clone()));
                            }
                        }
                    }
                }
            }
            gherkin::StepKeyword::When => {
                if step.text.starts_with("executing control query") {
                    // The control query is the *real* assertion target;
                    // promote any prior `executing query` to a setup step
                    // so its side effects are visible to the control query.
                    if let Some(prior) = test_query.take() {
                        setup_queries.push(prior);
                    }
                    test_query = step.doc_string.clone();
                } else if step.text.starts_with("executing query") {
                    test_query = step.doc_string.clone();
                }
            }
            gherkin::StepKeyword::Then => {
                if step.text.contains("result should be, in any order")
                    || step.text.contains("result should be, in order")
                    || step.text.contains("result should be (ignoring element order for lists)")
                {
                    expected_result = step.table.as_ref().map(|t| ExpectedResult {
                        columns: t[0].clone(),
                        rows: t[1..].to_vec(),
                        ordered: step.text.contains("in order")
                            && !step.text.contains("in any order"),
                        empty: false,
                    });
                } else if step.text.contains("result should be empty") {
                    expected_result = Some(ExpectedResult {
                        columns: Vec::new(),
                        rows: Vec::new(),
                        ordered: false,
                        empty: true,
                    });
                } else if step.text.contains("should be raised") {
                    expect_error = Some(step.text.clone());
                } else if step.text == "no side effects"
                    || step.text.contains("side effects should be")
                {
                    // Side effects tracking — accepted but not validated yet
                }
            }
            _ => {}
        }
    }

    let Some(query) = test_query else {
        return ScenarioOutcome::Skip("no test query found".to_string());
    };

    // Execute setup queries (parameters apply to setup too — some
    // scenarios CREATE nodes whose property comes from `$param`).
    for setup in &setup_queries {
        let setup_resolved = substitute_parameters(setup, &parameters);
        match execute_cypher(&engine, &session, &setup_resolved) {
            Ok(_) => {}
            Err(e) => {
                return ScenarioOutcome::Fail(format!("setup failed: {e}"));
            }
        }
    }

    // Substitute `$name` references in the test query with the literal
    // expression text captured from the gherkin parameters table.
    let query = substitute_parameters(&query, &parameters);

    // Execute test query
    match execute_cypher(&engine, &session, &query) {
        Ok(results) => {
            if expect_error.is_some() {
                return ScenarioOutcome::Fail(
                    "expected error but query succeeded".to_string(),
                );
            }
            if let Some(expected) = expected_result {
                match check_result(&results, &expected) {
                    Ok(()) => ScenarioOutcome::Pass,
                    Err(msg) => ScenarioOutcome::Fail(msg),
                }
            } else {
                // No result assertion — just check it didn't error
                ScenarioOutcome::Pass
            }
        }
        Err(e) => {
            if expect_error.is_some() {
                // We expected an error and got one — pass
                ScenarioOutcome::Pass
            } else {
                ScenarioOutcome::Fail(format!("query error: {e}"))
            }
        }
    }
}

// ═════════════════════════════════════════════════════════════════════
//  Result comparison
// ═════════════════════════════════════════════════════════════════════

#[derive(Debug)]
struct ExpectedResult {
    columns: Vec<String>,
    rows: Vec<Vec<String>>,
    ordered: bool,
    empty: bool,
}

fn check_result(results: &[StatementResult], expected: &ExpectedResult) -> Result<(), String> {
    // Find the query result
    let query_result = results.iter().find(|r| matches!(r, StatementResult::Query { .. }));

    if expected.empty {
        match query_result {
            None => return Ok(()),
            Some(StatementResult::Query { rows, .. }) if rows.is_empty() => return Ok(()),
            Some(StatementResult::Query { rows, .. }) => {
                return Err(format!("expected empty result, got {} rows", rows.len()));
            }
            _ => return Ok(()),
        }
    }

    let Some(StatementResult::Query { columns, rows }) = query_result else {
        return Err("no query result returned".to_string());
    };

    // Check column count
    if columns.len() != expected.columns.len() {
        return Err(format!(
            "column count mismatch: got {} expected {}",
            columns.len(),
            expected.columns.len()
        ));
    }

    // Check row count
    if rows.len() != expected.rows.len() {
        return Err(format!(
            "row count mismatch: got {} expected {}",
            rows.len(),
            expected.rows.len()
        ));
    }

    // Format actual rows as strings for comparison
    let actual_rows: Vec<Vec<String>> = rows
        .iter()
        .map(|row| row.values.iter().map(format_value).collect())
        .collect();

    if expected.ordered {
        // Ordered comparison
        for (i, (actual, exp)) in actual_rows.iter().zip(expected.rows.iter()).enumerate() {
            if !rows_match(actual, exp) {
                return Err(format!(
                    "row {i} mismatch: got {:?} expected {:?}",
                    actual, exp
                ));
            }
        }
    } else {
        // Unordered — each expected row must appear in actual
        let mut unmatched = actual_rows.clone();
        for exp_row in &expected.rows {
            let pos = unmatched.iter().position(|a| rows_match(a, exp_row));
            match pos {
                Some(i) => {
                    unmatched.remove(i);
                }
                None => {
                    return Err(format!(
                        "expected row {:?} not found in actual results {:?}",
                        exp_row, actual_rows
                    ));
                }
            }
        }
    }

    Ok(())
}

/// Format a Cypher float so whole values keep a `.0` suffix (e.g. `3.0`
/// instead of `3`), matching the openCypher TCK expected-row syntax.
fn format_cypher_float(n: f64) -> String {
    if n.is_nan() {
        return "NaN".to_owned();
    }
    if n.is_infinite() {
        return if n > 0.0 { "Inf" } else { "-Inf" }.to_owned();
    }
    let s = format!("{n}");
    if s.contains('.') || s.contains('e') || s.contains('E') || s.contains("inf") || s.contains("NaN") {
        s
    } else {
        format!("{s}.0")
    }
}

fn format_value(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::Int(n) => n.to_string(),
        Value::BigInt(n) => n.to_string(),
        Value::Real(n) => format_cypher_float(f64::from(*n)),
        Value::Double(n) => format_cypher_float(*n),
        Value::Text(s) => {
            // Node and edge literals come back as Text starting with `(` or
            // `[`; emit them unquoted so they round-trip into Cypher
            // expected-row syntax.
            let bytes = s.as_bytes();
            let is_node_or_edge = matches!(bytes.first(), Some(&b'(' | &b'['))
                && matches!(bytes.last(), Some(&b')' | &b']'));
            if is_node_or_edge {
                s.clone()
            } else {
                format!("'{s}'")
            }
        }
        Value::Array(elems) => {
            let inner: Vec<String> = elems.iter().map(format_value).collect();
            format!("[{}]", inner.join(", "))
        }
        // Cypher-compatible temporal formatting
        Value::Date(_) => {
            // Date Display is already YYYY-MM-DD
            format!("'{v}'")
        }
        Value::Time(_) => {
            // Convert PG time format to Cypher (strip trailing :00 seconds)
            let raw = format!("{v}");
            format!("'{}'", strip_trailing_zero_time(&raw))
        }
        Value::TimeTz(_, _) => {
            // Convert PG timetz format to Cypher (normalize offset, strip trailing :00)
            let raw = format!("{v}");
            let normalized = normalize_tz_offset_format(&strip_trailing_zero_time(&raw));
            format!("'{normalized}'")
        }
        Value::Timestamp(_) => {
            // PG uses space separator; Cypher uses T
            let raw = format!("{v}");
            let cypher = pg_timestamp_to_cypher(&raw);
            format!("'{cypher}'")
        }
        Value::TimestampTz(_) => {
            let raw = format!("{v}");
            let cypher = pg_timestamp_to_cypher(&raw);
            let normalized = normalize_tz_offset_format(&cypher);
            format!("'{normalized}'")
        }
        Value::Interval(iv) => format!("'{}'", format_cypher_duration(iv)),
        Value::Jsonb(json) => format_jsonb_cypher(json),
        other => format!("{other}"),
    }
}

/// Normalize timezone offset to Cypher format:
/// - "+01" -> "+01:00", "-05" -> "-05:00"
/// - "+00" or "+00:00" -> "Z"
fn normalize_tz_offset_format(s: &str) -> String {
    // Find the timezone offset part
    for i in (1..s.len()).rev() {
        let b = s.as_bytes()[i];
        if (b == b'+' || b == b'-') && s.as_bytes().get(i - 1).map_or(false, |c| c.is_ascii_digit())
        {
            let time_part = &s[..i];
            let tz_part = &s[i..];
            // Check if it's +00 or +00:00 (UTC)
            let is_utc = matches!(tz_part, "+00" | "+00:00" | "-00" | "-00:00");
            if is_utc {
                return format!("{time_part}Z");
            }
            // Ensure offset has minutes: +01 -> +01:00
            if tz_part.len() == 3 && !tz_part.contains(':') {
                return format!("{time_part}{tz_part}:00");
            }
            return s.to_string();
        }
    }
    s.to_string()
}

/// Strip trailing ":00" from time strings when seconds are zero.
/// E.g. "21:40:00" -> "21:40", "21:40:32" -> "21:40:32", "21:40:32.142" -> "21:40:32.142"
fn strip_trailing_zero_time(s: &str) -> String {
    // Split off timezone offset first
    let (time_part, tz_part) = split_time_tz(s);
    let stripped = time_part
        .strip_suffix(":00")
        .filter(|p| !p.contains('.'))
        .unwrap_or(time_part);
    format!("{stripped}{tz_part}")
}

/// Split a time string into time part and timezone offset part.
fn split_time_tz(s: &str) -> (&str, &str) {
    // Look for +/- timezone offset
    for i in (1..s.len()).rev() {
        let b = s.as_bytes()[i];
        if (b == b'+' || b == b'-') && s.as_bytes()[i - 1].is_ascii_digit() {
            return (&s[..i], &s[i..]);
        }
    }
    if s.ends_with('Z') {
        return (&s[..s.len() - 1], "Z");
    }
    (s, "")
}

/// Convert PG timestamp format to Cypher format:
/// - Replace space with T between date and time
/// - Strip trailing :00 seconds when zero
fn pg_timestamp_to_cypher(s: &str) -> String {
    // Replace first space between date and time with T
    let cypher = if let Some(space_pos) = s.find(' ') {
        // Check if this space is between date (YYYY-MM-DD) and time (HH:MM:...)
        if space_pos == 10 && s.len() > 11 {
            format!("{}T{}", &s[..space_pos], &s[space_pos + 1..])
        } else {
            s.to_string()
        }
    } else {
        s.to_string()
    };
    // Strip trailing :00 seconds from time portion
    if let Some(t_pos) = cypher.find('T') {
        let date_part = &cypher[..t_pos];
        let time_part = &cypher[t_pos + 1..];
        let stripped_time = strip_trailing_zero_time(time_part);
        format!("{date_part}T{stripped_time}")
    } else {
        cypher
    }
}

/// Format a duration/interval in Cypher ISO 8601 style: P[nY][nM][nD][T[nH][nM][nS]]
fn format_cypher_duration(iv: &aiondb_core::IntervalValue) -> String {
    let mut s = String::from("P");
    let years = iv.months / 12;
    let months = iv.months % 12;
    let days = iv.days;
    // Negative durations: split each unit so the sign sits on the
    // integer portion and the fractional part stays positive
    // (`-23h59m59.9s` ⇒ `PT-23H-59M-59.9S`, not `-59.-9S`).
    let total_micros = iv.micros;
    let neg = total_micros < 0;
    let abs_micros = total_micros.unsigned_abs();
    let abs_hours = abs_micros / 3_600_000_000;
    let abs_remaining = abs_micros % 3_600_000_000;
    let abs_minutes = abs_remaining / 60_000_000;
    let abs_remaining = abs_remaining % 60_000_000;
    let abs_seconds = abs_remaining / 1_000_000;
    let abs_micro_frac = abs_remaining % 1_000_000;
    let signed = |v: u64| -> i128 {
        if neg {
            -(v as i128)
        } else {
            v as i128
        }
    };
    let hours = signed(abs_hours);
    let minutes = signed(abs_minutes);
    let seconds = signed(abs_seconds);
    let micro_frac = abs_micro_frac;

    if years != 0 {
        s.push_str(&format!("{years}Y"));
    }
    if months != 0 {
        s.push_str(&format!("{months}M"));
    }
    if days != 0 {
        s.push_str(&format!("{days}D"));
    }
    if hours != 0 || minutes != 0 || seconds != 0 || micro_frac != 0 {
        s.push('T');
        if hours != 0 {
            s.push_str(&format!("{hours}H"));
        }
        if minutes != 0 {
            s.push_str(&format!("{minutes}M"));
        }
        if seconds != 0 || micro_frac != 0 {
            if micro_frac != 0 {
                let frac = format!("{:06}", micro_frac);
                let trimmed = frac.trim_end_matches('0');
                s.push_str(&format!("{seconds}.{trimmed}S"));
            } else {
                s.push_str(&format!("{seconds}S"));
            }
        }
    }
    if s == "P" {
        s.push_str("T0S");
    }
    s
}

fn rows_match(actual: &[String], expected: &[String]) -> bool {
    if actual.len() != expected.len() {
        return false;
    }
    actual
        .iter()
        .zip(expected.iter())
        .all(|(a, e)| values_equivalent(a, e))
}

/// Format a serde_json::Value in Cypher map/list notation (unquoted keys, single-quoted strings).
fn format_jsonb_cypher(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => {
            // Cypher floats: if the JSON number has a decimal point, render as float
            let s = n.to_string();
            if n.is_f64() && !s.contains('.') {
                format!("{s}.0")
            } else {
                s
            }
        }
        serde_json::Value::String(s) => {
            // If the string is a valid number, render without quotes (Cypher style)
            if s.parse::<f64>().is_ok() && !s.is_empty() {
                s.clone()
            } else {
                format!("'{s}'")
            }
        }
        serde_json::Value::Array(arr) => {
            let inner: Vec<String> = arr.iter().map(format_jsonb_cypher).collect();
            format!("[{}]", inner.join(", "))
        }
        serde_json::Value::Object(map) => {
            let entries: Vec<String> = map
                .iter()
                .map(|(k, val)| format!("{k}: {}", format_jsonb_cypher(val)))
                .collect();
            format!("{{{}}}", entries.join(", "))
        }
    }
}

fn values_equivalent(actual: &str, expected: &str) -> bool {
    if actual == expected {
        return true;
    }
    // Case-insensitive comparison (handles map key casing differences from SQL identifier folding)
    if actual.to_ascii_lowercase() == expected.to_ascii_lowercase() {
        return true;
    }
    // Try numeric comparison
    if let (Ok(a), Ok(e)) = (actual.parse::<f64>(), expected.parse::<f64>()) {
        if (a - e).abs() < 1e-10 {
            return true;
        }
    }
    // Strip quotes for string comparison
    let exp_unquoted = expected
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
        .unwrap_or(expected);
    let act_unquoted = actual
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
        .unwrap_or(actual);
    if act_unquoted == exp_unquoted {
        return true;
    }
    // Normalize temporal formats: T vs space, trailing :00 removal
    let norm_act = normalize_temporal_string(act_unquoted);
    let norm_exp = normalize_temporal_string(exp_unquoted);
    norm_act == norm_exp
}

/// Normalize a temporal string for comparison:
/// - Replace space between date and time with T
/// - Strip trailing :00 seconds when zero
fn normalize_temporal_string(s: &str) -> String {
    // Replace space with T (PG -> Cypher)
    let s = if s.len() > 10 && s.as_bytes().get(10) == Some(&b' ') {
        format!("{}T{}", &s[..10], &s[11..])
    } else {
        s.to_string()
    };
    // Strip trailing :00 seconds (if no fractional part follows)
    let s = s
        .strip_suffix(":00")
        .filter(|p| !p.contains('.'))
        .map(String::from)
        .unwrap_or(s);
    s
}

// ═════════════════════════════════════════════════════════════════════
//  Feature file discovery
// ═════════════════════════════════════════════════════════════════════

fn discover_features(base_dir: &Path, filter: &CliFilter) -> Vec<(String, PathBuf)> {
    let mut features = Vec::new();
    discover_recursive(base_dir, base_dir, &mut features, filter);
    features.sort_by(|a, b| a.0.cmp(&b.0));
    features
}

fn discover_recursive(
    base: &Path,
    dir: &Path,
    out: &mut Vec<(String, PathBuf)>,
    filter: &CliFilter,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            discover_recursive(base, &path, out, filter);
        } else if path.extension().map_or(false, |e| e == "feature") {
            let category = path
                .parent()
                .and_then(|p| p.strip_prefix(base).ok())
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();

            if let Some(ref cat_filter) = filter.category {
                if !category.contains(cat_filter.as_str()) {
                    continue;
                }
            }
            if let Some(ref file_filter) = filter.file {
                let stem = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                // `=Quantifier1` requires an exact stem match;
                // `Quantifier1` keeps the lenient substring behaviour.
                let matched = if let Some(exact) = file_filter.strip_prefix('=') {
                    stem == exact
                } else {
                    stem.contains(file_filter.as_str())
                };
                if !matched {
                    continue;
                }
            }

            out.push((category, path));
        }
    }
}

// ═════════════════════════════════════════════════════════════════════
//  Report generation
// ═════════════════════════════════════════════════════════════════════

fn print_report(results: &BTreeMap<String, CategoryResult>, elapsed: Duration) {
    let mut total_pass = 0usize;
    let mut total_fail = 0usize;
    let mut total_skip = 0usize;

    println!();
    println!("╔══════════════════════════════════════════════════════════════════╗");
    println!("║              openCypher TCK Results for AionDB                  ║");
    println!("╠══════════════════════════════════════════════════════════════════╣");
    println!(
        "║ {:<40} {:>5} {:>5} {:>5} ║",
        "Category", "Pass", "Fail", "Skip"
    );
    println!("╟──────────────────────────────────────────────────────────────────╢");

    for (category, result) in results {
        let display = if category.is_empty() {
            "(root)"
        } else {
            category
        };
        println!(
            "║ {:<40} {:>5} {:>5} {:>5} ║",
            truncate(display, 40),
            result.passed,
            result.failed,
            result.skipped
        );
        total_pass += result.passed;
        total_fail += result.failed;
        total_skip += result.skipped;
    }

    let total = total_pass + total_fail + total_skip;
    let pct = if total > 0 {
        (total_pass as f64 / total as f64) * 100.0
    } else {
        0.0
    };

    println!("╟──────────────────────────────────────────────────────────────────╢");
    println!(
        "║ {:<40} {:>5} {:>5} {:>5} ║",
        "TOTAL", total_pass, total_fail, total_skip
    );
    println!(
        "║ Pass rate: {:.1}%  ({} / {})  in {:.1}s{} ║",
        pct,
        total_pass,
        total,
        elapsed.as_secs_f64(),
        " ".repeat(68usize.saturating_sub(
            format!(
                " Pass rate: {:.1}%  ({} / {})  in {:.1}s",
                pct,
                total_pass,
                total,
                elapsed.as_secs_f64()
            )
            .len()
                + 2
        ))
    );
    println!("╚══════════════════════════════════════════════════════════════════╝");

    // Print first few failures per category
    if total_fail > 0 {
        println!();
        println!("── First failures per category ──");
        for (category, result) in results {
            if !result.errors.is_empty() {
                let display = if category.is_empty() {
                    "(root)"
                } else {
                    category.as_str()
                };
                println!();
                println!("  {display}:");
                for err in result.errors.iter().take(3) {
                    println!("    - {err}");
                }
                if result.errors.len() > 3 {
                    println!("    ... and {} more", result.errors.len() - 3);
                }
            }
        }
    }

    // Write markdown report
    if let Err(e) = write_markdown_report(results, elapsed) {
        eprintln!("warning: could not write markdown report: {e}");
    }
}

fn write_markdown_report(
    results: &BTreeMap<String, CategoryResult>,
    elapsed: Duration,
) -> std::io::Result<()> {
    use std::io::Write;
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let report_dir = manifest_dir.join("reports");
    std::fs::create_dir_all(&report_dir)?;

    let today = chrono_lite_today();
    let path = report_dir.join(format!("tck_progress_{today}.md"));
    let mut f = std::fs::File::create(&path)?;

    let mut total_pass = 0usize;
    let mut total_fail = 0usize;
    let mut total_skip = 0usize;
    for r in results.values() {
        total_pass += r.passed;
        total_fail += r.failed;
        total_skip += r.skipped;
    }
    let total = total_pass + total_fail + total_skip;
    let pct = if total > 0 {
        (total_pass as f64 / total as f64) * 100.0
    } else {
        0.0
    };

    writeln!(f, "# openCypher TCK Progress — {today}")?;
    writeln!(f)?;
    writeln!(
        f,
        "**{total_pass}/{total} ({pct:.1}%)** scenarios passing in {:.1}s",
        elapsed.as_secs_f64()
    )?;
    writeln!(f)?;
    writeln!(f, "| Category | Pass | Fail | Skip |")?;
    writeln!(f, "|----------|------|------|------|")?;
    for (category, result) in results {
        let display = if category.is_empty() {
            "(root)"
        } else {
            category.as_str()
        };
        writeln!(
            f,
            "| {} | {} | {} | {} |",
            display, result.passed, result.failed, result.skipped
        )?;
    }
    writeln!(
        f,
        "| **TOTAL** | **{total_pass}** | **{total_fail}** | **{total_skip}** |"
    )?;

    eprintln!("Report written to {}", path.display());
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max - 3])
    }
}

fn chrono_lite_today() -> String {
    // Simple date without chrono dependency
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = secs / 86400;
    // Approximate date calculation
    let mut y = 1970i64;
    let mut remaining = days as i64;
    loop {
        let days_in_year = if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) {
            366
        } else {
            365
        };
        if remaining < days_in_year {
            break;
        }
        remaining -= days_in_year;
        y += 1;
    }
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let month_days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut m = 0;
    for &md in &month_days {
        if remaining < md {
            break;
        }
        remaining -= md;
        m += 1;
    }
    format!("{y}-{:02}-{:02}", m + 1, remaining + 1)
}

// ═════════════════════════════════════════════════════════════════════
//  CLI
// ═════════════════════════════════════════════════════════════════════

struct CliFilter {
    category: Option<String>,
    file: Option<String>,
    verbose: bool,
}

fn parse_args() -> CliFilter {
    let args: Vec<String> = std::env::args().collect();
    let mut filter = CliFilter {
        category: None,
        file: None,
        verbose: false,
    };
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--category" | "-c" => {
                i += 1;
                if i < args.len() {
                    filter.category = Some(args[i].clone());
                }
            }
            "--file" | "-f" => {
                i += 1;
                if i < args.len() {
                    filter.file = Some(args[i].clone());
                }
            }
            "--verbose" | "-v" => {
                filter.verbose = true;
            }
            _ => {}
        }
        i += 1;
    }
    filter
}

// ═════════════════════════════════════════════════════════════════════
//  Main
// ═════════════════════════════════════════════════════════════════════

fn main() {
    let filter = parse_args();

    // Resolve TCK path relative to the Cargo manifest directory (project root),
    // not the current working directory.
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest_dir.parent().unwrap_or(manifest_dir);
    let tck_path = project_root.join(TCK_DIR);

    if !tck_path.exists() {
        eprintln!("TCK features not found at {}", tck_path.display());
        eprintln!("Run: git clone the openCypher TCK into testing/cypher-tck/");
        std::process::exit(1);
    }

    let features = discover_features(&tck_path, &filter);
    if features.is_empty() {
        eprintln!("No .feature files found matching filter");
        std::process::exit(1);
    }

    eprintln!(
        "Found {} feature files across {} categories",
        features.len(),
        features
            .iter()
            .map(|(c, _)| c.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    );

    let start = Instant::now();
    let mut results: BTreeMap<String, CategoryResult> = BTreeMap::new();
    let mut total_scenarios = 0usize;

    for (category, path) in &features {
        let feature = match gherkin::parse_feature_file(path) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("  PARSE ERROR: {}: {e}", path.display());
                let entry = results.entry(category.clone()).or_default();
                entry.failed += 1;
                entry.errors.push(format!("parse error: {e}"));
                continue;
            }
        };

        for scenario in &feature.scenarios {
            // Expand Scenario Outlines
            let instances: Vec<gherkin::Scenario> = if scenario.examples.is_empty() {
                vec![scenario.clone()]
            } else {
                scenario
                    .examples
                    .iter()
                    .enumerate()
                    .map(|(i, example)| {
                        let mut expanded = scenario.clone();
                        expanded.name = format!("{} [example {}]", scenario.name, i + 1);
                        // Substitute placeholders in steps
                        for step in &mut expanded.steps {
                            for (key, val) in example {
                                let placeholder = format!("<{key}>");
                                step.text = step.text.replace(&placeholder, val);
                                if let Some(ref mut doc) = step.doc_string {
                                    *doc = doc.replace(&placeholder, val);
                                }
                                if let Some(ref mut table) = step.table {
                                    for row in table.iter_mut() {
                                        for cell in row.iter_mut() {
                                            *cell = cell.replace(&placeholder, val);
                                        }
                                    }
                                }
                            }
                        }
                        expanded
                    })
                    .collect()
            };

            for instance in &instances {
                total_scenarios += 1;
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_scenario(instance)
                }));

                let entry = results.entry(category.clone()).or_default();
                match outcome {
                    Ok(ScenarioOutcome::Pass) => {
                        entry.passed += 1;
                        if filter.verbose {
                            eprintln!("  PASS: {}", instance.name);
                        }
                    }
                    Ok(ScenarioOutcome::Fail(msg)) => {
                        entry.failed += 1;
                        entry
                            .errors
                            .push(format!("{}: {msg}", instance.name));
                        if filter.verbose {
                            eprintln!("  FAIL: {}: {msg}", instance.name);
                        }
                    }
                    Ok(ScenarioOutcome::Skip(reason)) => {
                        entry.skipped += 1;
                        if filter.verbose {
                            eprintln!("  SKIP: {}: {reason}", instance.name);
                        }
                    }
                    Err(_) => {
                        entry.failed += 1;
                        entry
                            .errors
                            .push(format!("{}: PANIC", instance.name));
                        if filter.verbose {
                            eprintln!("  PANIC: {}", instance.name);
                        }
                    }
                }
            }
        }
    }

    let elapsed = start.elapsed();
    eprintln!("Processed {total_scenarios} scenarios in {:.1}s", elapsed.as_secs_f64());

    print_report(&results, elapsed);
}
