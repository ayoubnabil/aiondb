//! `cargo xtask neo4j-import` - convert Neo4j CSV exports to AionDB graph SQL.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const DEFAULT_SQL_OUT: &str = "target/compat/neo4j-import.sql";
const DEFAULT_REPORT: &str = "target/compat/neo4j-import-report.json";
const DEFAULT_STATE: &str = "target/compat/neo4j-import-state.json";

#[derive(Clone, Debug)]
pub struct Neo4jImportOptions {
    input: PathBuf,
    sql_out: PathBuf,
    report: PathBuf,
    state: PathBuf,
}

#[derive(Clone, Debug, Default)]
struct ImportPlan {
    nodes: BTreeMap<String, TablePlan>,
    relationships: BTreeMap<String, RelationshipPlan>,
    node_labels_by_id: BTreeMap<String, Vec<String>>,
    failed_rows: Vec<FailedRow>,
    processed_files: Vec<String>,
    skipped_files: Vec<String>,
}

#[derive(Clone, Debug, Default)]
struct TablePlan {
    table_name: String,
    columns: BTreeMap<String, SqlColumn>,
    rows: Vec<BTreeMap<String, String>>,
}

#[derive(Clone, Debug, Default)]
struct RelationshipPlan {
    table_name: String,
    source_label: String,
    target_label: String,
    columns: BTreeMap<String, SqlColumn>,
    rows: Vec<BTreeMap<String, String>>,
}

#[derive(Clone, Debug)]
struct SqlColumn {
    name: String,
    data_type: String,
}

#[derive(Clone, Debug, Serialize)]
struct FailedRow {
    file: String,
    line: usize,
    reason: String,
}

#[derive(Debug, Serialize)]
struct Neo4jImportReport {
    input: String,
    sql_out: String,
    state: String,
    nodes_imported: usize,
    relationships_imported: usize,
    node_tables: BTreeMap<String, usize>,
    relationship_tables: BTreeMap<String, usize>,
    processed_files: Vec<String>,
    skipped_completed_files: Vec<String>,
    failed_rows: Vec<FailedRow>,
    resumable: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct ImportState {
    completed_files: BTreeSet<String>,
    node_labels_by_id: BTreeMap<String, Vec<String>>,
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

pub fn parse_args(args: &[String]) -> Result<Neo4jImportOptions, String> {
    let mut input = None;
    let mut sql_out = PathBuf::from(DEFAULT_SQL_OUT);
    let mut report = PathBuf::from(DEFAULT_REPORT);
    let mut state = PathBuf::from(DEFAULT_STATE);
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
            "--sql-out" => {
                i += 1;
                sql_out = PathBuf::from(
                    args.get(i)
                        .ok_or_else(|| "--sql-out requires a value".to_owned())?,
                );
            }
            "--report" => {
                i += 1;
                report = PathBuf::from(
                    args.get(i)
                        .ok_or_else(|| "--report requires a value".to_owned())?,
                );
            }
            "--state" => {
                i += 1;
                state = PathBuf::from(
                    args.get(i)
                        .ok_or_else(|| "--state requires a value".to_owned())?,
                );
            }
            "--help" | "-h" => {
                println!(
                    "Usage: cargo xtask neo4j-import --input <EXPORT_DIR> [--sql-out <PATH>] [--report <PATH>] [--state <PATH>]\n\n\
Converts Neo4j admin/import-style CSV exports into transaction-safe AionDB graph SQL."
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown flag: {other}")),
        }
        i += 1;
    }
    Ok(Neo4jImportOptions {
        input: input.ok_or_else(|| "--input is required".to_owned())?,
        sql_out,
        report,
        state,
    })
}

pub fn run(opts: Neo4jImportOptions) -> Result<(), String> {
    let root = repo_root();
    let input = absolutize(&root, &opts.input);
    let sql_out = absolutize(&root, &opts.sql_out);
    let report_path = absolutize(&root, &opts.report);
    let state_path = absolutize(&root, &opts.state);
    let state = read_state(&state_path)?;
    let mut plan = build_import_plan(&input, &state)?;
    if !plan.failed_rows.is_empty() {
        let report = build_report(&input, &sql_out, &state_path, &plan, true);
        write_json(&report_path, &report)?;
        return Err(format!(
            "Neo4j import found {} failed row(s); see {}",
            plan.failed_rows.len(),
            report_path.display()
        ));
    }

    let sql = render_sql(&plan);
    write_text(&sql_out, &sql)?;
    for file in &plan.processed_files {
        plan.skipped_files.retain(|skipped| skipped != file);
    }
    let mut new_state = state;
    new_state
        .completed_files
        .extend(plan.processed_files.iter().cloned());
    new_state
        .node_labels_by_id
        .extend(plan.node_labels_by_id.clone());
    write_json(&state_path, &new_state)?;
    let report = build_report(&input, &sql_out, &state_path, &plan, true);
    write_json(&report_path, &report)?;
    println!("== neo4j-import ==");
    println!("input: {}", input.display());
    println!("sql: {}", sql_out.display());
    println!("report: {}", report_path.display());
    println!(
        "nodes: {} relationships: {} processed_files: {} skipped_completed: {}",
        report.nodes_imported,
        report.relationships_imported,
        report.processed_files.len(),
        report.skipped_completed_files.len()
    );
    Ok(())
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

fn build_import_plan(input: &Path, state: &ImportState) -> Result<ImportPlan, String> {
    if !input.is_dir() {
        return Err(format!("{} is not a directory", input.display()));
    }
    let mut plan = ImportPlan {
        node_labels_by_id: state.node_labels_by_id.clone(),
        ..ImportPlan::default()
    };
    let mut entries = fs::read_dir(input)
        .map_err(|error| format!("reading {}: {error}", input.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("reading {}: {error}", input.display()))?;
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let path = entry.path();
        if !path.is_file() || path.extension().map_or(true, |ext| ext != "csv") {
            continue;
        }
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| format!("non UTF-8 filename: {}", path.display()))?
            .to_owned();
        if state.completed_files.contains(&file_name) {
            plan.skipped_files.push(file_name);
            continue;
        }
        scan_csv(&path, &file_name, &mut plan)?;
        plan.processed_files.push(file_name);
    }
    Ok(plan)
}

fn scan_csv(path: &Path, file_name: &str, plan: &mut ImportPlan) -> Result<(), String> {
    let src =
        fs::read_to_string(path).map_err(|error| format!("reading {}: {error}", path.display()))?;
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
            Err(reason) => {
                plan.failed_rows.push(FailedRow {
                    file: file_name.to_owned(),
                    line: line_no,
                    reason,
                });
                continue;
            }
        };
        if fields.len() != header.columns.len() {
            plan.failed_rows.push(FailedRow {
                file: file_name.to_owned(),
                line: line_no,
                reason: format!(
                    "field count {} does not match header count {}",
                    fields.len(),
                    header.columns.len()
                ),
            });
            continue;
        }
        if is_relationship {
            scan_relationship_row(&header, &fields, file_name, line_no, plan);
        } else {
            scan_node_row(&header, &fields, file_name, line_no, plan);
        }
    }
    Ok(())
}

fn scan_node_row(
    header: &CsvHeader,
    fields: &[String],
    file_name: &str,
    line_no: usize,
    plan: &mut ImportPlan,
) {
    let Some(id_idx) = header.id_idx else {
        plan.failed_rows.push(FailedRow {
            file: file_name.to_owned(),
            line: line_no,
            reason: "node row has no :ID column".to_owned(),
        });
        return;
    };
    let id = fields[id_idx].trim();
    if id.is_empty() {
        plan.failed_rows.push(FailedRow {
            file: file_name.to_owned(),
            line: line_no,
            reason: "node row has empty :ID".to_owned(),
        });
        return;
    }
    let labels = header
        .label_idx
        .and_then(|idx| fields.get(idx))
        .map(|value| split_multi(value, ';'))
        .filter(|labels| !labels.is_empty())
        .unwrap_or_else(|| vec!["unlabeled".to_owned()]);
    let sanitized_labels: Vec<String> = labels.iter().map(|label| sanitize_ident(label)).collect();
    plan.node_labels_by_id
        .insert(id.to_owned(), sanitized_labels.clone());
    for label in sanitized_labels {
        let table = plan
            .nodes
            .entry(label.clone())
            .or_insert_with(|| TablePlan {
                table_name: label.clone(),
                columns: BTreeMap::from([(
                    "id".to_owned(),
                    SqlColumn {
                        name: "id".to_owned(),
                        data_type: "BIGINT".to_owned(),
                    },
                )]),
                rows: Vec::new(),
            });
        let mut row = BTreeMap::from([("id".to_owned(), id.to_owned())]);
        collect_properties(header, fields, &mut table.columns, &mut row);
        table.rows.push(row);
    }
}

fn scan_relationship_row(
    header: &CsvHeader,
    fields: &[String],
    file_name: &str,
    line_no: usize,
    plan: &mut ImportPlan,
) {
    let (Some(start_idx), Some(end_idx), Some(type_idx)) =
        (header.start_idx, header.end_idx, header.type_idx)
    else {
        plan.failed_rows.push(FailedRow {
            file: file_name.to_owned(),
            line: line_no,
            reason: "relationship row requires :START_ID, :END_ID and :TYPE".to_owned(),
        });
        return;
    };
    let source_id = fields[start_idx].trim();
    let target_id = fields[end_idx].trim();
    let rel_type = fields[type_idx].trim();
    if source_id.is_empty() || target_id.is_empty() || rel_type.is_empty() {
        plan.failed_rows.push(FailedRow {
            file: file_name.to_owned(),
            line: line_no,
            reason: "relationship row has empty endpoint or :TYPE".to_owned(),
        });
        return;
    }
    let source_label = plan
        .node_labels_by_id
        .get(source_id)
        .and_then(|labels| labels.first())
        .cloned()
        .unwrap_or_else(|| "unlabeled".to_owned());
    let target_label = plan
        .node_labels_by_id
        .get(target_id)
        .and_then(|labels| labels.first())
        .cloned()
        .unwrap_or_else(|| "unlabeled".to_owned());
    let rel_label = sanitize_ident(rel_type);
    let table = plan
        .relationships
        .entry(rel_label.clone())
        .or_insert_with(|| RelationshipPlan {
            table_name: rel_label,
            source_label,
            target_label,
            columns: BTreeMap::from([
                (
                    "source_id".to_owned(),
                    SqlColumn {
                        name: "source_id".to_owned(),
                        data_type: "BIGINT".to_owned(),
                    },
                ),
                (
                    "target_id".to_owned(),
                    SqlColumn {
                        name: "target_id".to_owned(),
                        data_type: "BIGINT".to_owned(),
                    },
                ),
            ]),
            rows: Vec::new(),
        });
    let mut row = BTreeMap::from([
        ("source_id".to_owned(), source_id.to_owned()),
        ("target_id".to_owned(), target_id.to_owned()),
    ]);
    collect_properties(header, fields, &mut table.columns, &mut row);
    table.rows.push(row);
}

fn collect_properties(
    header: &CsvHeader,
    fields: &[String],
    columns: &mut BTreeMap<String, SqlColumn>,
    row: &mut BTreeMap<String, String>,
) {
    for (idx, raw_name) in header.columns.iter().enumerate() {
        if is_meta_column(raw_name) {
            continue;
        }
        let col = sql_column(raw_name);
        columns.entry(col.name.clone()).or_insert(col.clone());
        row.insert(col.name, fields[idx].clone());
    }
}

fn render_sql(plan: &ImportPlan) -> String {
    let mut sql = String::new();
    sql.push_str("BEGIN;\n");
    for table in plan.nodes.values() {
        render_create_table(&mut sql, &table.table_name, &table.columns);
        for row in &table.rows {
            render_insert(&mut sql, &table.table_name, &table.columns, row);
        }
        sql.push_str(&format!(
            "CREATE NODE LABEL {} ON {};\n",
            quote_ident(&table.table_name),
            quote_ident(&table.table_name)
        ));
    }
    for rel in plan.relationships.values() {
        render_create_table(&mut sql, &rel.table_name, &rel.columns);
        for row in &rel.rows {
            render_insert(&mut sql, &rel.table_name, &rel.columns, row);
        }
        sql.push_str(&format!(
            "CREATE EDGE LABEL {} ON {} SOURCE {} TARGET {};\n",
            quote_ident(&rel.table_name),
            quote_ident(&rel.table_name),
            quote_ident(&rel.source_label),
            quote_ident(&rel.target_label)
        ));
    }
    sql.push_str("COMMIT;\n");
    sql
}

fn render_create_table(sql: &mut String, table: &str, columns: &BTreeMap<String, SqlColumn>) {
    sql.push_str(&format!("CREATE TABLE {} (", quote_ident(table)));
    let ordered = ordered_columns(columns);
    let defs = ordered
        .iter()
        .map(|col| {
            let not_null = if matches!(col.name.as_str(), "id" | "source_id" | "target_id") {
                " NOT NULL"
            } else {
                ""
            };
            format!("{} {}{}", quote_ident(&col.name), col.data_type, not_null)
        })
        .collect::<Vec<_>>()
        .join(", ");
    sql.push_str(&defs);
    sql.push_str(");\n");
}

fn render_insert(
    sql: &mut String,
    table: &str,
    columns: &BTreeMap<String, SqlColumn>,
    row: &BTreeMap<String, String>,
) {
    let ordered = ordered_columns(columns);
    let names = ordered
        .iter()
        .map(|col| quote_ident(&col.name))
        .collect::<Vec<_>>()
        .join(", ");
    let values = ordered
        .iter()
        .map(|col| {
            sql_literal(
                row.get(&col.name).map(String::as_str).unwrap_or(""),
                &col.data_type,
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    sql.push_str(&format!(
        "INSERT INTO {} ({}) VALUES ({});\n",
        quote_ident(table),
        names,
        values
    ));
}

fn ordered_columns(columns: &BTreeMap<String, SqlColumn>) -> Vec<&SqlColumn> {
    let mut ordered = columns.values().collect::<Vec<_>>();
    ordered.sort_by_key(|col| match col.name.as_str() {
        "id" => (0_u8, ""),
        "source_id" => (0, ""),
        "target_id" => (1, ""),
        other => (2, other),
    });
    ordered
}

fn build_report(
    input: &Path,
    sql_out: &Path,
    state: &Path,
    plan: &ImportPlan,
    resumable: bool,
) -> Neo4jImportReport {
    Neo4jImportReport {
        input: input.display().to_string(),
        sql_out: sql_out.display().to_string(),
        state: state.display().to_string(),
        nodes_imported: plan.nodes.values().map(|table| table.rows.len()).sum(),
        relationships_imported: plan
            .relationships
            .values()
            .map(|table| table.rows.len())
            .sum(),
        node_tables: plan
            .nodes
            .iter()
            .map(|(name, table)| (name.clone(), table.rows.len()))
            .collect(),
        relationship_tables: plan
            .relationships
            .iter()
            .map(|(name, table)| (name.clone(), table.rows.len()))
            .collect(),
        processed_files: plan.processed_files.clone(),
        skipped_completed_files: plan.skipped_files.clone(),
        failed_rows: plan.failed_rows.clone(),
        resumable,
    }
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

fn sql_column(raw: &str) -> SqlColumn {
    let (name, type_hint) = raw.split_once(':').unwrap_or((raw, ""));
    SqlColumn {
        name: sanitize_ident(name),
        data_type: match type_hint.to_ascii_lowercase().as_str() {
            "int" | "long" => "BIGINT",
            "float" | "double" => "DOUBLE",
            "boolean" | "bool" => "BOOLEAN",
            _ => "TEXT",
        }
        .to_owned(),
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

fn sql_literal(value: &str, data_type: &str) -> String {
    if value.trim().is_empty() {
        return "NULL".to_owned();
    }
    match data_type {
        "BIGINT" | "DOUBLE" => value.trim().to_owned(),
        "BOOLEAN" if value.eq_ignore_ascii_case("true") => "TRUE".to_owned(),
        "BOOLEAN" if value.eq_ignore_ascii_case("false") => "FALSE".to_owned(),
        _ => format!("'{}'", value.replace('\'', "''")),
    }
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

fn read_state(path: &Path) -> Result<ImportState, String> {
    if !path.exists() {
        return Ok(ImportState::default());
    }
    let src =
        fs::read_to_string(path).map_err(|error| format!("reading {}: {error}", path.display()))?;
    serde_json::from_str(&src).map_err(|error| format!("parsing {}: {error}", path.display()))
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

fn write_text(path: &Path, value: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("creating {}: {error}", parent.display()))?;
    }
    fs::write(path, value).map_err(|error| format!("writing {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_core::Value;
    use aiondb_engine::{
        Credential, EngineBuilder, QueryEngine, SecretString, StartupParams, StatementResult,
        TransportInfo,
    };
    use std::collections::BTreeMap;

    #[test]
    fn imports_nodes_and_relationships_as_executable_graph_sql() {
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

        let plan = build_import_plan(dir.path(), &ImportState::default()).expect("plan");
        assert!(plan.failed_rows.is_empty());
        let sql = render_sql(&plan);
        let engine = EngineBuilder::for_testing().build().expect("engine");
        let (session, _) = engine.startup(startup_params()).expect("startup");
        engine.execute_sql(&session, &sql).expect("execute import");

        assert_eq!(count(&engine, &session, "SELECT COUNT(*) FROM person"), 2);
        assert_eq!(count(&engine, &session, "SELECT COUNT(*) FROM knows"), 1);
        let rows = query(
            &engine,
            &session,
            "SELECT source_id, target_id, since FROM knows",
        );
        assert_eq!(rows[0].values[0], Value::BigInt(1));
        assert_eq!(rows[0].values[1], Value::BigInt(2));
        assert_eq!(rows[0].values[2], Value::BigInt(2020));
    }

    #[test]
    fn skips_completed_files_for_resumable_import() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(
            dir.path().join("nodes.csv"),
            ":ID,:LABEL,name\n1,Person,Alice\n",
        )
        .expect("nodes");
        fs::write(
            dir.path().join("rels.csv"),
            ":START_ID,:END_ID,:TYPE\n1,1,KNOWS\n",
        )
        .expect("rels");
        let state = ImportState {
            completed_files: BTreeSet::from(["nodes.csv".to_owned()]),
            node_labels_by_id: BTreeMap::from([("1".to_owned(), vec!["person".to_owned()])]),
        };
        let plan = build_import_plan(dir.path(), &state).expect("plan");
        assert_eq!(plan.skipped_files, vec!["nodes.csv"]);
        assert_eq!(plan.processed_files, vec!["rels.csv"]);
        let sql = render_sql(&plan);
        assert!(
            sql.contains(
                "CREATE EDGE LABEL \"knows\" ON \"knows\" SOURCE \"person\" TARGET \"person\""
            ),
            "{sql}"
        );
    }

    fn startup_params() -> StartupParams {
        StartupParams {
            database: "default".to_owned(),
            application_name: Some("neo4j-import-test".to_owned()),
            options: BTreeMap::new(),
            credential: Credential::CleartextPassword {
                user: "xtask".to_owned(),
                password: SecretString::new("xtask".to_owned()),
            },
            transport: TransportInfo::in_process(),
        }
    }

    fn query(
        engine: &aiondb_engine::Engine,
        session: &aiondb_engine::SessionHandle,
        sql: &str,
    ) -> Vec<aiondb_core::Row> {
        let result = engine.execute_sql(session, sql).expect("query");
        match result.last().expect("result") {
            StatementResult::Query { rows, .. } => rows.clone(),
            other => panic!("expected query, got {other:?}"),
        }
    }

    fn count(
        engine: &aiondb_engine::Engine,
        session: &aiondb_engine::SessionHandle,
        sql: &str,
    ) -> i64 {
        match &query(engine, session, sql)[0].values[0] {
            Value::BigInt(value) => *value,
            other => panic!("expected count bigint, got {other:?}"),
        }
    }
}
