//! `cargo xtask neo4j-analyze` - analyze Neo4j CSV exports before migration.

use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const DEFAULT_REPORT: &str = "target/compat/neo4j-analyze-report.json";

#[derive(Clone, Debug)]
pub struct Neo4jAnalyzeOptions {
    input: PathBuf,
    report: PathBuf,
}

#[derive(Clone, Debug, Default, Serialize)]
struct PropertyStats {
    rows_present: usize,
    inferred_types: BTreeSet<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
struct LabelStats {
    nodes: usize,
    properties: BTreeMap<String, PropertyStats>,
}

#[derive(Clone, Debug, Default, Serialize)]
struct RelationshipStats {
    relationships: usize,
    properties: BTreeMap<String, PropertyStats>,
}

#[derive(Clone, Debug, Default, Serialize)]
struct VolumeStats {
    files_scanned: usize,
    node_rows: usize,
    relationship_rows: usize,
}

#[derive(Clone, Debug, Default, Serialize)]
struct SchemaStats {
    index_statements: Vec<String>,
    constraint_statements: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
struct Neo4jAnalyzeReport {
    input: String,
    volumes: VolumeStats,
    labels: BTreeMap<String, LabelStats>,
    relationship_types: BTreeMap<String, RelationshipStats>,
    schema: SchemaStats,
    risks: Vec<String>,
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

pub fn parse_args(args: &[String]) -> Result<Neo4jAnalyzeOptions, String> {
    let mut input = None;
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
            "--report" => {
                i += 1;
                report = PathBuf::from(
                    args.get(i)
                        .ok_or_else(|| "--report requires a value".to_owned())?,
                );
            }
            "--help" | "-h" => {
                println!(
                    "Usage: cargo xtask neo4j-analyze --input <EXPORT_DIR> [--report <PATH>]\n\n\
Analyzes Neo4j admin/import-style CSV exports plus optional schema.cypher."
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown flag: {other}")),
        }
        i += 1;
    }
    Ok(Neo4jAnalyzeOptions {
        input: input.ok_or_else(|| "--input is required".to_owned())?,
        report,
    })
}

pub fn run(opts: Neo4jAnalyzeOptions) -> Result<(), String> {
    let root = repo_root();
    let input = absolutize(&root, &opts.input);
    let report_path = absolutize(&root, &opts.report);
    let report = analyze_export(&input)?;
    write_json(&report_path, &report)?;
    println!("== neo4j-analyze ==");
    println!("input: {}", input.display());
    println!("report: {}", report_path.display());
    println!(
        "nodes: {} relationships: {} labels: {} relationship types: {}",
        report.volumes.node_rows,
        report.volumes.relationship_rows,
        report.labels.len(),
        report.relationship_types.len()
    );
    println!("risks: {}", report.risks.len());
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

fn analyze_export(input: &Path) -> Result<Neo4jAnalyzeReport, String> {
    if !input.is_dir() {
        return Err(format!("{} is not a directory", input.display()));
    }
    let mut report = Neo4jAnalyzeReport {
        input: input.display().to_string(),
        ..Neo4jAnalyzeReport::default()
    };
    let mut entries = fs::read_dir(input)
        .map_err(|error| format!("reading {}: {error}", input.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("reading {}: {error}", input.display()))?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.eq_ignore_ascii_case("schema.cypher") {
            scan_schema(&path, &mut report)?;
        } else if path.extension().is_some_and(|ext| ext == "csv") {
            scan_csv(&path, &mut report)?;
        }
    }
    if report.volumes.files_scanned == 0 {
        report
            .risks
            .push("no CSV export files were found".to_owned());
    }
    if report.schema.index_statements.is_empty() {
        report
            .risks
            .push("no index metadata found; migration sizing may miss lookup indexes".to_owned());
    }
    if report.schema.constraint_statements.is_empty() {
        report.risks.push(
            "no constraint metadata found; uniqueness/existence semantics need manual review"
                .to_owned(),
        );
    }
    Ok(report)
}

fn scan_csv(path: &Path, report: &mut Neo4jAnalyzeReport) -> Result<(), String> {
    let src =
        fs::read_to_string(path).map_err(|error| format!("reading {}: {error}", path.display()))?;
    let mut lines = src.lines();
    let Some(header_line) = lines.next() else {
        report.risks.push(format!("{} is empty", path.display()));
        return Ok(());
    };
    let header = CsvHeader::parse(header_line)?;
    let is_relationship = header.start_idx.is_some() && header.end_idx.is_some();
    let is_node = header.id_idx.is_some() && !is_relationship;
    if !is_node && !is_relationship {
        report.risks.push(format!(
            "{} is not recognized as a Neo4j node or relationship CSV",
            path.display()
        ));
        return Ok(());
    }

    report.volumes.files_scanned += 1;
    for (line_no, line) in lines.enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let fields = parse_csv_line(line)
            .map_err(|error| format!("{}:{}: {error}", path.display(), line_no + 2))?;
        if fields.len() != header.columns.len() {
            report.risks.push(format!(
                "{}:{} has {} fields but header has {}",
                path.display(),
                line_no + 2,
                fields.len(),
                header.columns.len()
            ));
            continue;
        }
        if is_relationship {
            scan_relationship_row(&header, &fields, report);
        } else {
            scan_node_row(&header, &fields, report);
        }
    }
    Ok(())
}

fn scan_node_row(header: &CsvHeader, fields: &[String], report: &mut Neo4jAnalyzeReport) {
    report.volumes.node_rows += 1;
    let labels = header
        .label_idx
        .and_then(|idx| fields.get(idx))
        .map(|value| split_multi(value, ';'))
        .filter(|labels| !labels.is_empty())
        .unwrap_or_else(|| vec!["<unlabeled>".to_owned()]);
    for label in labels {
        let label_stats = report.labels.entry(label).or_default();
        label_stats.nodes += 1;
        scan_properties(header, fields, &mut label_stats.properties);
    }
}

fn scan_relationship_row(header: &CsvHeader, fields: &[String], report: &mut Neo4jAnalyzeReport) {
    report.volumes.relationship_rows += 1;
    let rel_type = header
        .type_idx
        .and_then(|idx| fields.get(idx))
        .filter(|value| !value.trim().is_empty())
        .cloned()
        .unwrap_or_else(|| "<unknown>".to_owned());
    let rel_stats = report.relationship_types.entry(rel_type).or_default();
    rel_stats.relationships += 1;
    scan_properties(header, fields, &mut rel_stats.properties);
}

fn scan_properties(
    header: &CsvHeader,
    fields: &[String],
    properties: &mut BTreeMap<String, PropertyStats>,
) {
    for (idx, raw_name) in header.columns.iter().enumerate() {
        if is_meta_column(raw_name) {
            continue;
        }
        let Some(value) = fields.get(idx) else {
            continue;
        };
        if value.trim().is_empty() {
            continue;
        }
        let name = property_name(raw_name);
        let stats = properties.entry(name).or_default();
        stats.rows_present += 1;
        stats.inferred_types.insert(infer_type(value));
    }
}

fn scan_schema(path: &Path, report: &mut Neo4jAnalyzeReport) -> Result<(), String> {
    let src =
        fs::read_to_string(path).map_err(|error| format!("reading {}: {error}", path.display()))?;
    for line in src.lines() {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("create index") {
            report.schema.index_statements.push(trimmed.to_owned());
        } else if lower.starts_with("create constraint") {
            report.schema.constraint_statements.push(trimmed.to_owned());
        }
    }
    Ok(())
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

fn property_name(raw: &str) -> String {
    raw.split(':').next().unwrap_or(raw).trim().to_owned()
}

fn split_multi(value: &str, sep: char) -> Vec<String> {
    value
        .split(sep)
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn infer_type(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.eq_ignore_ascii_case("true") || trimmed.eq_ignore_ascii_case("false") {
        "boolean".to_owned()
    } else if trimmed.parse::<i64>().is_ok() {
        "integer".to_owned()
    } else if trimmed.parse::<f64>().is_ok() {
        "float".to_owned()
    } else if trimmed.contains(';') {
        "list".to_owned()
    } else {
        "string".to_owned()
    }
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
            '"' => {
                in_quotes = !in_quotes;
            }
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

fn write_json(path: &Path, report: &Neo4jAnalyzeReport) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("creating {}: {error}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(report)
        .map_err(|error| format!("serializing Neo4j analyze report: {error}"))?;
    fs::write(path, format!("{json}\n"))
        .map_err(|error| format!("writing {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analyzes_labels_relationships_properties_and_schema() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(
            dir.path().join("nodes.csv"),
            ":ID,:LABEL,name,age:int,active:boolean\n1,Person,Alice,42,true\n2,Person;Admin,Bob,7,false\n",
        )
        .expect("nodes");
        fs::write(
            dir.path().join("relationships.csv"),
            ":START_ID,:END_ID,:TYPE,since:int\n1,2,KNOWS,2020\n",
        )
        .expect("rels");
        fs::write(
            dir.path().join("schema.cypher"),
            "CREATE INDEX person_name IF NOT EXISTS FOR (n:Person) ON (n.name);\nCREATE CONSTRAINT person_id IF NOT EXISTS FOR (n:Person) REQUIRE n.id IS UNIQUE;\n",
        )
        .expect("schema");

        let report = analyze_export(dir.path()).expect("analyze");
        assert_eq!(report.volumes.node_rows, 2);
        assert_eq!(report.volumes.relationship_rows, 1);
        assert_eq!(report.labels["Person"].nodes, 2);
        assert_eq!(report.labels["Admin"].nodes, 1);
        assert_eq!(report.relationship_types["KNOWS"].relationships, 1);
        assert_eq!(report.schema.index_statements.len(), 1);
        assert_eq!(report.schema.constraint_statements.len(), 1);
    }

    #[test]
    fn handles_large_graph_smoke() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut nodes = String::from(":ID,:LABEL,name,score:float\n");
        let mut rels = String::from(":START_ID,:END_ID,:TYPE,weight:float\n");
        for i in 0..5_000 {
            nodes.push_str(&format!("{i},Person,user-{i},{}\n", i as f64 / 10.0));
            if i > 0 {
                rels.push_str(&format!("{},{},FOLLOWS,1.0\n", i - 1, i));
            }
        }
        fs::write(dir.path().join("nodes.csv"), nodes).expect("nodes");
        fs::write(dir.path().join("relationships.csv"), rels).expect("rels");

        let report = analyze_export(dir.path()).expect("analyze");
        assert_eq!(report.volumes.node_rows, 5_000);
        assert_eq!(report.volumes.relationship_rows, 4_999);
        assert_eq!(
            report.labels["Person"].properties["score"].rows_present,
            5_000
        );
        assert_eq!(
            report.relationship_types["FOLLOWS"].properties["weight"].rows_present,
            4_999
        );
    }
}
