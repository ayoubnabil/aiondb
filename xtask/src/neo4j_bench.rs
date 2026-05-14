//! `cargo xtask neo4j-bench` - benchmark Neo4j export queries against AionDB import SQL.

use aiondb_core::{Row, Value};
use aiondb_engine::{
    Credential, Engine, EngineBuilder, QueryEngine, SecretString, StartupParams, StatementResult,
    TransportInfo,
};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

const DEFAULT_REPORT: &str = "target/compat/neo4j-bench-report.json";
const DEFAULT_ITERATIONS: usize = 25;

#[derive(Clone, Debug)]
pub struct Neo4jBenchOptions {
    input: PathBuf,
    sql: PathBuf,
    report: PathBuf,
    iterations: usize,
}

#[derive(Clone, Debug, Default)]
struct ExpectedGraph {
    relationships: BTreeMap<String, ExpectedRelationship>,
    node_labels_by_id: BTreeMap<String, Vec<String>>,
    failed_rows: Vec<String>,
}

#[derive(Clone, Debug, Default)]
struct ExpectedRelationship {
    source_label: String,
    target_label: String,
    rows: Vec<BTreeMap<String, String>>,
}

#[derive(Debug, Serialize)]
struct Neo4jBenchReport {
    input: String,
    sql: String,
    iterations: usize,
    queries: Vec<BenchQueryReport>,
    summary: BenchSummary,
}

#[derive(Debug, Serialize)]
struct BenchQueryReport {
    name: String,
    logical_query: String,
    aiondb_sql: String,
    source_result: i64,
    aiondb_result: i64,
    result_parity: bool,
    source_p50_us: u128,
    source_p95_us: u128,
    aiondb_p50_us: u128,
    aiondb_p95_us: u128,
}

#[derive(Debug, Serialize)]
struct BenchSummary {
    queries: usize,
    parity_ok: usize,
    ok: bool,
}

#[derive(Debug)]
struct BenchCase {
    name: String,
    logical_query: String,
    aiondb_sql: String,
    relationship: String,
    source_id: String,
    target_id: String,
}

#[derive(Debug)]
struct CsvHeader {
    columns: Vec<String>,
    id_idx: Option<usize>,
    label_idx: Option<usize>,
    start_idx: Option<usize>,
    end_idx: Option<usize>,
    type_idx: Option<usize>,
}

pub fn parse_args(args: &[String]) -> Result<Neo4jBenchOptions, String> {
    let mut input = None;
    let mut sql = None;
    let mut report = PathBuf::from(DEFAULT_REPORT);
    let mut iterations = DEFAULT_ITERATIONS;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--input" => {
                i += 1;
                input = Some(PathBuf::from(
                    args.get(i)
                        .ok_or_else(|| "--input requires a value".to_owned())?,
                ));
            }
            "--sql" => {
                i += 1;
                sql = Some(PathBuf::from(
                    args.get(i)
                        .ok_or_else(|| "--sql requires a value".to_owned())?,
                ));
            }
            "--report" => {
                i += 1;
                report = PathBuf::from(
                    args.get(i)
                        .ok_or_else(|| "--report requires a value".to_owned())?,
                );
            }
            "--iterations" => {
                i += 1;
                iterations = args
                    .get(i)
                    .ok_or_else(|| "--iterations requires a value".to_owned())?
                    .parse::<usize>()
                    .map_err(|error| format!("invalid --iterations: {error}"))?;
            }
            "--help" | "-h" => {
                println!(
                    "Usage: cargo xtask neo4j-bench --input <EXPORT_DIR> --sql <IMPORT_SQL> [--report <PATH>] [--iterations <N>]\n\n\
Runs the same logical relationship queries against the source export model and imported AionDB SQL, then reports result parity and p50/p95 latency."
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown flag: {other}")),
        }
        i += 1;
    }
    if iterations == 0 {
        return Err("--iterations must be greater than 0".to_owned());
    }
    Ok(Neo4jBenchOptions {
        input: input.ok_or_else(|| "--input is required".to_owned())?,
        sql: sql.ok_or_else(|| "--sql is required".to_owned())?,
        report,
        iterations,
    })
}

pub fn run(opts: Neo4jBenchOptions) -> Result<(), String> {
    let root = repo_root();
    let input = absolutize(&root, &opts.input);
    let sql_path = absolutize(&root, &opts.sql);
    let report_path = absolutize(&root, &opts.report);
    let report = benchmark(&input, &sql_path, opts.iterations)?;
    write_json(&report_path, &report)?;
    println!("== neo4j-bench ==");
    println!("input: {}", input.display());
    println!("sql: {}", sql_path.display());
    println!("report: {}", report_path.display());
    println!(
        "queries: {} parity_ok: {} ok: {}",
        report.summary.queries, report.summary.parity_ok, report.summary.ok
    );
    if report.summary.ok {
        Ok(())
    } else {
        Err(format!(
            "Neo4j benchmark parity failed; see {}",
            report_path.display()
        ))
    }
}

fn repo_root() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p
}

fn absolutize(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn benchmark(input: &Path, sql_path: &Path, iterations: usize) -> Result<Neo4jBenchReport, String> {
    let expected = expected_graph(input)?;
    if !expected.failed_rows.is_empty() {
        return Err(format!(
            "Neo4j export has {} failed CSV row(s): {}",
            expected.failed_rows.len(),
            expected.failed_rows.join("; ")
        ));
    }
    let cases = bench_cases(&expected)?;
    if cases.is_empty() {
        return Err("Neo4j benchmark requires at least one relationship sample".to_owned());
    }

    let engine = EngineBuilder::for_testing()
        .build()
        .map_err(|error| error.to_string())?;
    let (session, _) = engine
        .startup(startup_params())
        .map_err(|error| error.to_string())?;
    let import_sql = fs::read_to_string(sql_path)
        .map_err(|error| format!("reading {}: {error}", sql_path.display()))?;
    engine
        .execute_sql(&session, &import_sql)
        .map_err(|error| error.to_string())?;

    let mut reports = Vec::new();
    for case in &cases {
        reports.push(run_case(&engine, &session, &expected, case, iterations)?);
    }
    let parity_ok = reports.iter().filter(|query| query.result_parity).count();
    let ok = parity_ok == reports.len();
    Ok(Neo4jBenchReport {
        input: input.display().to_string(),
        sql: sql_path.display().to_string(),
        iterations,
        summary: BenchSummary {
            queries: reports.len(),
            parity_ok,
            ok,
        },
        queries: reports,
    })
}

fn bench_cases(expected: &ExpectedGraph) -> Result<Vec<BenchCase>, String> {
    let mut cases = Vec::new();
    for (name, rel) in &expected.relationships {
        let Some(first) = rel.rows.first() else {
            continue;
        };
        let source_id = first.get("source_id").cloned().unwrap_or_default();
        let target_id = first.get("target_id").cloned().unwrap_or_default();
        if source_id.is_empty() || target_id.is_empty() {
            return Err(format!(
                "relationship {name} is missing source_id or target_id"
            ));
        }
        let logical_query = format!(
            "relationship_exists(type={name}, source_id={source_id}, target_id={target_id})"
        );
        let aiondb_sql = format!(
            "SELECT COUNT(*) FROM {} r JOIN {} s ON s.\"id\" = r.\"source_id\" JOIN {} t ON t.\"id\" = r.\"target_id\" WHERE r.\"source_id\" = {} AND r.\"target_id\" = {}",
            quote_ident(name),
            quote_ident(&rel.source_label),
            quote_ident(&rel.target_label),
            source_id,
            target_id
        );
        cases.push(BenchCase {
            name: format!("relationship_exists:{name}:{source_id}->{target_id}"),
            logical_query,
            aiondb_sql,
            relationship: name.clone(),
            source_id,
            target_id,
        });
    }
    Ok(cases)
}

fn run_case(
    engine: &Engine,
    session: &aiondb_engine::SessionHandle,
    expected: &ExpectedGraph,
    case: &BenchCase,
    iterations: usize,
) -> Result<BenchQueryReport, String> {
    let mut source_durations = Vec::with_capacity(iterations);
    let mut aiondb_durations = Vec::with_capacity(iterations);
    let mut source_result = 0;
    let mut aiondb_result = 0;

    for _ in 0..iterations {
        let started = Instant::now();
        source_result = source_relationship_count(expected, case)?;
        source_durations.push(started.elapsed().as_micros());

        let started = Instant::now();
        aiondb_result = scalar_count(engine, session, &case.aiondb_sql)?;
        aiondb_durations.push(started.elapsed().as_micros());
    }

    Ok(BenchQueryReport {
        name: case.name.clone(),
        logical_query: case.logical_query.clone(),
        aiondb_sql: case.aiondb_sql.clone(),
        source_result,
        aiondb_result,
        result_parity: source_result == aiondb_result,
        source_p50_us: percentile_us(source_durations.clone(), 50),
        source_p95_us: percentile_us(source_durations, 95),
        aiondb_p50_us: percentile_us(aiondb_durations.clone(), 50),
        aiondb_p95_us: percentile_us(aiondb_durations, 95),
    })
}

fn source_relationship_count(expected: &ExpectedGraph, case: &BenchCase) -> Result<i64, String> {
    let rel = expected
        .relationships
        .get(&case.relationship)
        .ok_or_else(|| format!("unknown relationship {}", case.relationship))?;
    Ok(rel
        .rows
        .iter()
        .filter(|row| {
            row.get("source_id") == Some(&case.source_id)
                && row.get("target_id") == Some(&case.target_id)
        })
        .count() as i64)
}

fn percentile_us(mut values: Vec<u128>, percentile: usize) -> u128 {
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable();
    let idx = ((values.len() - 1) * percentile).div_ceil(100);
    values[idx.min(values.len() - 1)]
}

fn expected_graph(input: &Path) -> Result<ExpectedGraph, String> {
    let mut graph = ExpectedGraph::default();
    let mut entries = fs::read_dir(input)
        .map_err(|error| format!("reading {}: {error}", input.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("reading {}: {error}", input.display()))?;
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "csv") {
            scan_csv(&path, &mut graph)?;
        }
    }
    Ok(graph)
}

fn scan_csv(path: &Path, graph: &mut ExpectedGraph) -> Result<(), String> {
    let src =
        fs::read_to_string(path).map_err(|error| format!("reading {}: {error}", path.display()))?;
    let file = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("<csv>");
    let mut lines = src.lines();
    let Some(header_line) = lines.next() else {
        return Ok(());
    };
    let header = CsvHeader::parse(header_line)?;
    let is_relationship = header.start_idx.is_some() && header.end_idx.is_some();
    for (line_no, line) in lines.enumerate() {
        let line_no = line_no + 2;
        if line.trim().is_empty() {
            continue;
        }
        let fields = match parse_csv_line(line) {
            Ok(fields) => fields,
            Err(error) => {
                graph.failed_rows.push(format!("{file}:{line_no}: {error}"));
                continue;
            }
        };
        if fields.len() != header.columns.len() {
            graph.failed_rows.push(format!(
                "{file}:{line_no}: field count {} != header count {}",
                fields.len(),
                header.columns.len()
            ));
            continue;
        }
        if is_relationship {
            scan_relationship_row(&header, &fields, graph);
        } else {
            scan_node_row(&header, &fields, graph);
        }
    }
    Ok(())
}

fn scan_node_row(header: &CsvHeader, fields: &[String], graph: &mut ExpectedGraph) {
    let Some(id_idx) = header.id_idx else {
        return;
    };
    let id = fields[id_idx].trim().to_owned();
    let labels = header
        .label_idx
        .and_then(|idx| fields.get(idx))
        .map(|value| split_multi(value, ';'))
        .filter(|labels| !labels.is_empty())
        .unwrap_or_else(|| vec!["unlabeled".to_owned()]);
    let labels = labels
        .iter()
        .map(|label| sanitize_ident(label))
        .collect::<Vec<_>>();
    graph.node_labels_by_id.insert(id, labels);
}

fn scan_relationship_row(header: &CsvHeader, fields: &[String], graph: &mut ExpectedGraph) {
    let (Some(start_idx), Some(end_idx), Some(type_idx)) =
        (header.start_idx, header.end_idx, header.type_idx)
    else {
        return;
    };
    let source_id = fields[start_idx].trim().to_owned();
    let target_id = fields[end_idx].trim().to_owned();
    let rel_type = sanitize_ident(&fields[type_idx]);
    let source_label = graph
        .node_labels_by_id
        .get(&source_id)
        .and_then(|labels| labels.first())
        .cloned()
        .unwrap_or_else(|| "unlabeled".to_owned());
    let target_label = graph
        .node_labels_by_id
        .get(&target_id)
        .and_then(|labels| labels.first())
        .cloned()
        .unwrap_or_else(|| "unlabeled".to_owned());
    let rel = graph
        .relationships
        .entry(rel_type)
        .or_insert_with(|| ExpectedRelationship {
            source_label,
            target_label,
            rows: Vec::new(),
        });
    rel.rows.push(BTreeMap::from([
        ("source_id".to_owned(), source_id),
        ("target_id".to_owned(), target_id),
    ]));
}

impl CsvHeader {
    fn parse(line: &str) -> Result<Self, String> {
        let columns = parse_csv_line(line)?;
        let mut header = CsvHeader {
            columns,
            id_idx: None,
            label_idx: None,
            start_idx: None,
            end_idx: None,
            type_idx: None,
        };
        for (idx, col) in header.columns.iter().enumerate() {
            let normalized = col.trim().to_ascii_uppercase();
            if normalized == ":ID" || normalized.ends_with(":ID") {
                header.id_idx = Some(idx);
            } else if normalized == ":LABEL" || normalized.ends_with(":LABEL") {
                header.label_idx = Some(idx);
            } else if normalized == ":START_ID" || normalized.ends_with(":START_ID") {
                header.start_idx = Some(idx);
            } else if normalized == ":END_ID" || normalized.ends_with(":END_ID") {
                header.end_idx = Some(idx);
            } else if normalized == ":TYPE" || normalized.ends_with(":TYPE") {
                header.type_idx = Some(idx);
            }
        }
        Ok(header)
    }
}

fn sanitize_ident(value: &str) -> String {
    let mut out = String::new();
    for ch in value.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if ch == '_' {
            out.push(ch);
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        "unnamed".to_owned()
    } else if trimmed.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
        format!("n_{trimmed}")
    } else {
        trimmed.to_owned()
    }
}

fn quote_ident(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn split_multi(value: &str, sep: char) -> Vec<String> {
    value
        .split(sep)
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn parse_csv_line(line: &str) -> Result<Vec<String>, String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut chars = line.chars().peekable();
    let mut in_quotes = false;
    while let Some(ch) = chars.next() {
        match ch {
            '"' if in_quotes && chars.peek() == Some(&'"') => {
                chars.next();
                current.push('"');
            }
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => {
                fields.push(current);
                current = String::new();
            }
            _ => current.push(ch),
        }
    }
    if in_quotes {
        return Err("unterminated CSV quote".to_owned());
    }
    fields.push(current);
    Ok(fields)
}

fn query_rows(
    engine: &Engine,
    session: &aiondb_engine::SessionHandle,
    sql: &str,
) -> Result<Vec<Row>, String> {
    let result = engine
        .execute_sql(session, sql)
        .map_err(|error| error.to_string())?;
    match result.last() {
        Some(StatementResult::Query { rows, .. }) => Ok(rows.clone()),
        other => Err(format!("expected query result for {sql}, got {other:?}")),
    }
}

fn scalar_count(
    engine: &Engine,
    session: &aiondb_engine::SessionHandle,
    sql: &str,
) -> Result<i64, String> {
    let rows = query_rows(engine, session, sql)?;
    match rows.first().and_then(|row| row.values.first()) {
        Some(Value::BigInt(value)) => Ok(*value),
        other => Err(format!("expected count bigint for {sql}, got {other:?}")),
    }
}

fn startup_params() -> StartupParams {
    StartupParams {
        database: "default".to_owned(),
        application_name: Some("neo4j-bench".to_owned()),
        options: BTreeMap::new(),
        credential: Credential::CleartextPassword {
            user: "xtask".to_owned(),
            password: SecretString::new("xtask".to_owned()),
        },
        transport: TransportInfo::in_process(),
    }
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("creating {}: {error}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(value).map_err(|error| error.to_string())?;
    fs::write(path, format!("{json}\n"))
        .map_err(|error| format!("writing {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_parity_and_latency_for_relationship_samples() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(
            dir.path().join("nodes.csv"),
            ":ID,:LABEL,name\n1,Person,Alice\n2,Person,Bob\n",
        )
        .expect("nodes");
        fs::write(
            dir.path().join("rels.csv"),
            ":START_ID,:END_ID,:TYPE,since:int\n1,2,KNOWS,2020\n",
        )
        .expect("rels");
        let sql = dir.path().join("import.sql");
        fs::write(
            &sql,
            "BEGIN;\
             CREATE TABLE \"person\" (\"id\" BIGINT NOT NULL, \"name\" TEXT);\
             INSERT INTO \"person\" (\"id\", \"name\") VALUES (1, 'Alice');\
             INSERT INTO \"person\" (\"id\", \"name\") VALUES (2, 'Bob');\
             CREATE NODE LABEL \"person\" ON \"person\";\
             CREATE TABLE \"knows\" (\"source_id\" BIGINT NOT NULL, \"target_id\" BIGINT NOT NULL, \"since\" BIGINT);\
             INSERT INTO \"knows\" (\"source_id\", \"target_id\", \"since\") VALUES (1, 2, 2020);\
             CREATE EDGE LABEL \"knows\" ON \"knows\" SOURCE \"person\" TARGET \"person\";\
             COMMIT;",
        )
        .expect("sql");

        let report = benchmark(dir.path(), &sql, 3).expect("benchmark");

        assert!(report.summary.ok, "{report:#?}");
        assert_eq!(report.summary.queries, 1);
        assert_eq!(report.summary.parity_ok, 1);
        assert_eq!(report.queries[0].source_result, 1);
        assert_eq!(report.queries[0].aiondb_result, 1);
        assert!(report.queries[0].result_parity);
        assert!(report.queries[0]
            .logical_query
            .contains("relationship_exists"));
    }

    #[test]
    fn percentile_uses_nearest_rank_ceiling() {
        assert_eq!(percentile_us(vec![1, 2, 3, 4], 50), 3);
        assert_eq!(percentile_us(vec![1, 2, 3, 4], 95), 4);
        assert_eq!(percentile_us(vec![7], 95), 7);
    }
}
