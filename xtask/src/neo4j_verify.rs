//! `cargo xtask neo4j-verify` - verify Neo4j export parity after AionDB import.

use aiondb_core::{Row, Value};
use aiondb_engine::{
    Credential, Engine, EngineBuilder, QueryEngine, SecretString, StartupParams, StatementResult,
    TransportInfo,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const DEFAULT_REPORT: &str = "target/compat/neo4j-verify-report.json";

#[derive(Clone, Debug)]
pub struct Neo4jVerifyOptions {
    input: PathBuf,
    sql: PathBuf,
    report: PathBuf,
}

#[derive(Clone, Debug, Default)]
struct ExpectedGraph {
    nodes: BTreeMap<String, ExpectedTable>,
    relationships: BTreeMap<String, ExpectedRelationship>,
    node_labels_by_id: BTreeMap<String, Vec<String>>,
    failed_rows: Vec<String>,
}

#[derive(Clone, Debug, Default)]
struct ExpectedTable {
    columns: BTreeMap<String, String>,
    rows: Vec<BTreeMap<String, String>>,
}

#[derive(Clone, Debug, Default)]
struct ExpectedRelationship {
    source_label: String,
    target_label: String,
    columns: BTreeMap<String, String>,
    rows: Vec<BTreeMap<String, String>>,
}

#[derive(Clone, Debug, Serialize)]
struct TableParity {
    table: String,
    expected_count: usize,
    actual_count: usize,
    expected_checksum: String,
    actual_checksum: String,
    ok: bool,
}

#[derive(Debug, Serialize)]
struct SampleQueryParity {
    relationship: String,
    query: String,
    expected_rows: usize,
    actual_rows: usize,
    ok: bool,
}

#[derive(Debug, Serialize)]
struct Neo4jVerifyReport {
    input: String,
    sql: String,
    count_parity: Vec<TableParity>,
    checksum_parity: Vec<TableParity>,
    sample_query_parity: Vec<SampleQueryParity>,
    failed_rows: Vec<String>,
    ok: bool,
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

pub fn parse_args(args: &[String]) -> Result<Neo4jVerifyOptions, String> {
    let mut input = None;
    let mut sql = None;
    let mut report = PathBuf::from(DEFAULT_REPORT);
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
            "--help" | "-h" => {
                println!(
                    "Usage: cargo xtask neo4j-verify --input <EXPORT_DIR> --sql <IMPORT_SQL> [--report <PATH>]\n\n\
Executes AionDB import SQL and verifies count, checksum and relationship sample-query parity."
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown flag: {other}")),
        }
        i += 1;
    }
    Ok(Neo4jVerifyOptions {
        input: input.ok_or_else(|| "--input is required".to_owned())?,
        sql: sql.ok_or_else(|| "--sql is required".to_owned())?,
        report,
    })
}

pub fn run(opts: Neo4jVerifyOptions) -> Result<(), String> {
    let root = repo_root();
    let input = absolutize(&root, &opts.input);
    let sql_path = absolutize(&root, &opts.sql);
    let report_path = absolutize(&root, &opts.report);
    let report = verify(&input, &sql_path)?;
    write_json(&report_path, &report)?;
    println!("== neo4j-verify ==");
    println!("input: {}", input.display());
    println!("sql: {}", sql_path.display());
    println!("report: {}", report_path.display());
    println!(
        "tables: {} samples: {} ok: {}",
        report.count_parity.len(),
        report.sample_query_parity.len(),
        report.ok
    );
    if report.ok {
        Ok(())
    } else {
        Err(format!(
            "Neo4j verify failed; see {}",
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

fn verify(input: &Path, sql_path: &Path) -> Result<Neo4jVerifyReport, String> {
    let expected = expected_graph(input)?;
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

    let mut count_parity = Vec::new();
    let mut checksum_parity = Vec::new();
    for (name, table) in &expected.nodes {
        let parity = table_parity(&engine, &session, name, &table.columns, &table.rows)?;
        count_parity.push(parity.clone());
        checksum_parity.push(parity);
    }
    for (name, rel) in &expected.relationships {
        let parity = table_parity(&engine, &session, name, &rel.columns, &rel.rows)?;
        count_parity.push(parity.clone());
        checksum_parity.push(parity);
    }
    let sample_query_parity = relationship_samples(&engine, &session, &expected)?;
    let ok = expected.failed_rows.is_empty()
        && count_parity
            .iter()
            .all(|item| item.expected_count == item.actual_count)
        && checksum_parity
            .iter()
            .all(|item| item.expected_checksum == item.actual_checksum)
        && sample_query_parity.iter().all(|item| item.ok);
    Ok(Neo4jVerifyReport {
        input: input.display().to_string(),
        sql: sql_path.display().to_string(),
        count_parity,
        checksum_parity,
        sample_query_parity,
        failed_rows: expected.failed_rows,
        ok,
    })
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
    graph.node_labels_by_id.insert(id.clone(), labels.clone());
    for label in labels {
        let table = graph.nodes.entry(label).or_default();
        table.columns.insert("id".to_owned(), "BIGINT".to_owned());
        let mut row = BTreeMap::from([("id".to_owned(), id.clone())]);
        collect_properties(header, fields, &mut table.columns, &mut row);
        table.rows.push(row);
    }
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
    let rel = graph.relationships.entry(rel_type).or_insert_with(|| {
        let mut columns = BTreeMap::new();
        columns.insert("source_id".to_owned(), "BIGINT".to_owned());
        columns.insert("target_id".to_owned(), "BIGINT".to_owned());
        ExpectedRelationship {
            source_label,
            target_label,
            columns,
            rows: Vec::new(),
        }
    });
    let mut row = BTreeMap::from([
        ("source_id".to_owned(), source_id),
        ("target_id".to_owned(), target_id),
    ]);
    collect_properties(header, fields, &mut rel.columns, &mut row);
    rel.rows.push(row);
}

fn collect_properties(
    header: &CsvHeader,
    fields: &[String],
    columns: &mut BTreeMap<String, String>,
    row: &mut BTreeMap<String, String>,
) {
    for (idx, raw_name) in header.columns.iter().enumerate() {
        if is_meta_column(raw_name) {
            continue;
        }
        let (name, data_type) = sql_column(raw_name);
        columns.entry(name.clone()).or_insert(data_type);
        row.insert(name, fields[idx].clone());
    }
}

fn table_parity(
    engine: &Engine,
    session: &aiondb_engine::SessionHandle,
    table: &str,
    columns: &BTreeMap<String, String>,
    expected_rows: &[BTreeMap<String, String>],
) -> Result<TableParity, String> {
    let ordered = ordered_columns(columns);
    let actual_rows = query_rows(
        engine,
        session,
        &format!(
            "SELECT {} FROM {} ORDER BY {}",
            ordered
                .iter()
                .map(|name| quote_ident(name))
                .collect::<Vec<_>>()
                .join(", "),
            quote_ident(table),
            quote_ident(&ordered[0])
        ),
    )?;
    let expected_norm = expected_rows
        .iter()
        .map(|row| {
            ordered
                .iter()
                .map(|col| {
                    normalize_source(
                        row.get(col).map(String::as_str).unwrap_or(""),
                        columns.get(col).unwrap(),
                    )
                })
                .collect::<Vec<_>>()
                .join("|")
        })
        .collect::<BTreeSet<_>>();
    let actual_norm = actual_rows
        .iter()
        .map(|row| {
            row.values
                .iter()
                .map(normalize_value)
                .collect::<Vec<_>>()
                .join("|")
        })
        .collect::<BTreeSet<_>>();
    let expected_checksum = checksum(expected_norm.iter());
    let actual_checksum = checksum(actual_norm.iter());
    Ok(TableParity {
        table: table.to_owned(),
        expected_count: expected_rows.len(),
        actual_count: actual_rows.len(),
        expected_checksum: expected_checksum.clone(),
        actual_checksum: actual_checksum.clone(),
        ok: expected_rows.len() == actual_rows.len() && expected_checksum == actual_checksum,
    })
}

fn relationship_samples(
    engine: &Engine,
    session: &aiondb_engine::SessionHandle,
    expected: &ExpectedGraph,
) -> Result<Vec<SampleQueryParity>, String> {
    let mut out = Vec::new();
    for (name, rel) in &expected.relationships {
        let Some(first) = rel.rows.first() else {
            continue;
        };
        let source_id = first.get("source_id").map(String::as_str).unwrap_or("");
        let target_id = first.get("target_id").map(String::as_str).unwrap_or("");
        let query = format!(
            "SELECT COUNT(*) FROM {} r JOIN {} s ON s.\"id\" = r.\"source_id\" JOIN {} t ON t.\"id\" = r.\"target_id\" WHERE r.\"source_id\" = {} AND r.\"target_id\" = {}",
            quote_ident(name),
            quote_ident(&rel.source_label),
            quote_ident(&rel.target_label),
            source_id,
            target_id
        );
        let actual_rows = scalar_count(engine, session, &query)? as usize;
        out.push(SampleQueryParity {
            relationship: name.clone(),
            query,
            expected_rows: 1,
            actual_rows,
            ok: actual_rows == 1,
        });
    }
    Ok(out)
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

fn is_meta_column(name: &str) -> bool {
    let normalized = name.trim().to_ascii_uppercase();
    normalized.starts_with(':')
        || normalized.ends_with(":ID")
        || normalized.ends_with(":LABEL")
        || normalized.ends_with(":START_ID")
        || normalized.ends_with(":END_ID")
        || normalized.ends_with(":TYPE")
}

fn sql_column(raw: &str) -> (String, String) {
    let (name, type_hint) = raw.split_once(':').unwrap_or((raw, ""));
    let data_type = match type_hint.to_ascii_lowercase().as_str() {
        "int" | "long" => "BIGINT",
        "float" | "double" => "DOUBLE",
        "boolean" | "bool" => "BOOLEAN",
        _ => "TEXT",
    };
    (sanitize_ident(name), data_type.to_owned())
}

fn ordered_columns(columns: &BTreeMap<String, String>) -> Vec<String> {
    let mut ordered = columns.keys().cloned().collect::<Vec<_>>();
    ordered.sort_by_key(|col| match col.as_str() {
        "id" => (0_u8, String::new()),
        "source_id" => (0, String::new()),
        "target_id" => (1, String::new()),
        other => (2, other.to_owned()),
    });
    ordered
}

fn normalize_source(value: &str, data_type: &str) -> String {
    if value.trim().is_empty() {
        return "null".to_owned();
    }
    match data_type {
        "BIGINT" => value.trim().parse::<i64>().unwrap_or(0).to_string(),
        "DOUBLE" => value.trim().parse::<f64>().unwrap_or(0.0).to_string(),
        "BOOLEAN" => value.trim().to_ascii_lowercase(),
        _ => value.to_owned(),
    }
}

fn normalize_value(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Int(value) => value.to_string(),
        Value::BigInt(value) => value.to_string(),
        Value::Real(value) => f64::from(*value).to_string(),
        Value::Double(value) => value.to_string(),
        Value::Boolean(value) => value.to_string(),
        Value::Text(value) => value.clone(),
        other => format!("{other:?}"),
    }
}

fn checksum<'a>(rows: impl Iterator<Item = &'a String>) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for row in rows {
        for byte in row.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash ^= b'\n' as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
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
        application_name: Some("neo4j-verify".to_owned()),
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
    fn verifies_count_checksum_and_sample_query_parity() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(
            dir.path().join("nodes.csv"),
            ":ID,:LABEL,name,age:int\n1,Person,Alice,42\n2,Person,Bob,35\n",
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
             CREATE TABLE \"person\" (\"id\" BIGINT NOT NULL, \"age\" BIGINT, \"name\" TEXT);\
             INSERT INTO \"person\" (\"id\", \"age\", \"name\") VALUES (1, 42, 'Alice');\
             INSERT INTO \"person\" (\"id\", \"age\", \"name\") VALUES (2, 35, 'Bob');\
             CREATE NODE LABEL \"person\" ON \"person\";\
             CREATE TABLE \"knows\" (\"source_id\" BIGINT NOT NULL, \"target_id\" BIGINT NOT NULL, \"since\" BIGINT);\
             INSERT INTO \"knows\" (\"source_id\", \"target_id\", \"since\") VALUES (1, 2, 2020);\
             CREATE EDGE LABEL \"knows\" ON \"knows\" SOURCE \"person\" TARGET \"person\";\
             COMMIT;",
        )
        .expect("sql");
        let report = verify(dir.path(), &sql).expect("verify");
        assert!(report.ok, "{report:#?}");
        assert_eq!(report.count_parity.len(), 2);
        assert_eq!(report.sample_query_parity.len(), 1);
    }
}
