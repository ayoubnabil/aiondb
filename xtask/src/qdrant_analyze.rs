//! `cargo xtask qdrant-analyze` - analyze Qdrant JSON exports before migration.

use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const DEFAULT_REPORT: &str = "target/compat/qdrant-analyze-report.json";

#[derive(Clone, Debug)]
pub struct QdrantAnalyzeOptions {
    input: PathBuf,
    report: PathBuf,
}

#[derive(Debug, Default, Serialize)]
struct QdrantAnalyzeReport {
    input: String,
    files_scanned: usize,
    collections: BTreeMap<String, CollectionReport>,
    risks: Vec<String>,
}

#[derive(Debug, Default, Serialize)]
struct CollectionReport {
    points: usize,
    vector_dimensions: BTreeMap<String, VectorDimensionReport>,
    payload_schema: BTreeMap<String, PayloadFieldReport>,
}

#[derive(Debug, Default, Serialize)]
struct VectorDimensionReport {
    declared_dims: Option<usize>,
    observed_dimensions: BTreeMap<usize, usize>,
    mismatches: Vec<String>,
}

#[derive(Debug, Default, Serialize)]
struct PayloadFieldReport {
    rows_present: usize,
    inferred_types: BTreeSet<String>,
}

pub fn parse_args(args: &[String]) -> Result<QdrantAnalyzeOptions, String> {
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
                    "Usage: cargo xtask qdrant-analyze --input <EXPORT_DIR> [--report <PATH>]\n\n\
Analyzes Qdrant collection JSON/JSONL exports, including point counts, vector dimensions and payload schema."
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown flag: {other}")),
        }
        i += 1;
    }
    Ok(QdrantAnalyzeOptions {
        input: input.ok_or_else(|| "--input is required".to_owned())?,
        report,
    })
}

pub fn run(opts: QdrantAnalyzeOptions) -> Result<(), String> {
    let root = repo_root();
    let input = absolutize(&root, &opts.input);
    let report_path = absolutize(&root, &opts.report);
    let report = analyze_export(&input)?;
    write_json(&report_path, &report)?;
    println!("== qdrant-analyze ==");
    println!("input: {}", input.display());
    println!("report: {}", report_path.display());
    println!(
        "files: {} collections: {} points: {} risks: {}",
        report.files_scanned,
        report.collections.len(),
        report
            .collections
            .values()
            .map(|collection| collection.points)
            .sum::<usize>(),
        report.risks.len()
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

fn analyze_export(input: &Path) -> Result<QdrantAnalyzeReport, String> {
    if !input.is_dir() {
        return Err(format!("{} is not a directory", input.display()));
    }
    let mut report = QdrantAnalyzeReport {
        input: input.display().to_string(),
        ..QdrantAnalyzeReport::default()
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
        match path.extension().and_then(|ext| ext.to_str()) {
            Some("json") => {
                scan_json_file(&path, &mut report)?;
                report.files_scanned += 1;
            }
            Some("jsonl") | Some("ndjson") => {
                scan_jsonl_file(&path, &mut report)?;
                report.files_scanned += 1;
            }
            _ => {}
        }
    }
    if report.files_scanned == 0 {
        report
            .risks
            .push("no Qdrant JSON or JSONL export files were found".to_owned());
    }
    for (collection_name, collection) in &report.collections {
        if collection.vector_dimensions.is_empty() {
            report
                .risks
                .push(format!("collection {collection_name} has no vectors"));
        }
        if collection.payload_schema.is_empty() {
            report.risks.push(format!(
                "collection {collection_name} has no payload schema"
            ));
        }
    }
    Ok(report)
}

fn scan_json_file(path: &Path, report: &mut QdrantAnalyzeReport) -> Result<(), String> {
    let src =
        fs::read_to_string(path).map_err(|error| format!("reading {}: {error}", path.display()))?;
    let value: Value = serde_json::from_str(&src)
        .map_err(|error| format!("parsing {}: {error}", path.display()))?;
    scan_json_value(path, &value, report)
}

fn scan_jsonl_file(path: &Path, report: &mut QdrantAnalyzeReport) -> Result<(), String> {
    let src =
        fs::read_to_string(path).map_err(|error| format!("reading {}: {error}", path.display()))?;
    let collection_name = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(sanitize_ident)
        .unwrap_or_else(|| "collection".to_owned());
    for (line_no, line) in src.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let point: Value = serde_json::from_str(line)
            .map_err(|error| format!("parsing {}:{}: {error}", path.display(), line_no + 1))?;
        let collection = report
            .collections
            .entry(collection_name.clone())
            .or_default();
        scan_point(
            collection,
            &point,
            &format!("{}:{}", path.display(), line_no + 1),
        );
    }
    Ok(())
}

fn scan_json_value(
    path: &Path,
    value: &Value,
    report: &mut QdrantAnalyzeReport,
) -> Result<(), String> {
    if let Some(collections) = value.get("collections").and_then(Value::as_array) {
        for collection in collections {
            scan_collection_value(path, collection, report)?;
        }
        return Ok(());
    }
    if value.get("points").is_some()
        || value.get("result").is_some()
        || value.get("vectors").is_some()
    {
        scan_collection_value(path, value, report)?;
        return Ok(());
    }
    if let Some(points) = value.as_array() {
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(sanitize_ident)
            .unwrap_or_else(|| "collection".to_owned());
        let collection = report.collections.entry(name).or_default();
        for (idx, point) in points.iter().enumerate() {
            scan_point(
                collection,
                point,
                &format!("{}:{}", path.display(), idx + 1),
            );
        }
        return Ok(());
    }
    report.risks.push(format!(
        "{} is not recognized as a Qdrant collection export",
        path.display()
    ));
    Ok(())
}

fn scan_collection_value(
    path: &Path,
    value: &Value,
    report: &mut QdrantAnalyzeReport,
) -> Result<(), String> {
    let name = collection_name(path, value);
    let declared = declared_dimensions(value);
    let points = collection_points(value);
    let collection = report.collections.entry(name.clone()).or_default();
    for (vector_name, dims) in declared {
        collection
            .vector_dimensions
            .entry(vector_name)
            .or_default()
            .declared_dims
            .get_or_insert(dims);
    }
    for (idx, point) in points.iter().enumerate() {
        scan_point(
            collection,
            point,
            &format!("{}:{name}:{}", path.display(), idx + 1),
        );
    }
    Ok(())
}

fn collection_name(path: &Path, value: &Value) -> String {
    value
        .get("collection")
        .or_else(|| value.get("name"))
        .and_then(Value::as_str)
        .map(sanitize_ident)
        .or_else(|| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .map(sanitize_ident)
        })
        .unwrap_or_else(|| "collection".to_owned())
}

fn declared_dimensions(value: &Value) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for candidate in [
        value.pointer("/config/params/vectors"),
        value.pointer("/params/vectors"),
        value.get("vectors"),
    ]
    .into_iter()
    .flatten()
    {
        collect_declared_dimensions(candidate, "", &mut out);
    }
    out
}

fn collect_declared_dimensions(value: &Value, name: &str, out: &mut BTreeMap<String, usize>) {
    if let Some(size) = value.get("size").and_then(Value::as_u64) {
        out.insert(vector_name(name), size as usize);
        return;
    }
    if let Some(obj) = value.as_object() {
        for (key, child) in obj {
            collect_declared_dimensions(child, key, out);
        }
    }
}

fn collection_points(value: &Value) -> Vec<&Value> {
    value
        .get("points")
        .or_else(|| value.pointer("/result/points"))
        .and_then(Value::as_array)
        .map(|points| points.iter().collect())
        .unwrap_or_default()
}

fn scan_point(collection: &mut CollectionReport, point: &Value, location: &str) {
    collection.points += 1;
    let point_id = point
        .get("id")
        .map(format_json_scalar)
        .unwrap_or_else(|| location.to_owned());
    if let Some(vector) = point.get("vector").or_else(|| point.get("vectors")) {
        scan_vectors(collection, vector, &point_id);
    }
    if let Some(payload) = point.get("payload").and_then(Value::as_object) {
        for (key, value) in payload {
            scan_payload_field(collection, key, value);
        }
    }
}

fn scan_vectors(collection: &mut CollectionReport, value: &Value, point_id: &str) {
    if let Some(vector) = numeric_vector(value) {
        observe_dims(collection, "", vector.len(), point_id);
        return;
    }
    if let Some(obj) = value.as_object() {
        for (name, child) in obj {
            if let Some(vector) = child
                .get("vector")
                .and_then(numeric_vector)
                .or_else(|| numeric_vector(child))
            {
                observe_dims(collection, name, vector.len(), point_id);
            }
        }
    }
}

fn observe_dims(collection: &mut CollectionReport, name: &str, dims: usize, point_id: &str) {
    let report = collection
        .vector_dimensions
        .entry(vector_name(name))
        .or_default();
    *report.observed_dimensions.entry(dims).or_default() += 1;
    if let Some(declared) = report.declared_dims {
        if declared != dims {
            report.mismatches.push(format!(
                "point {point_id} has dims {dims}, declared dims {declared}"
            ));
        }
    }
}

fn scan_payload_field(collection: &mut CollectionReport, key: &str, value: &Value) {
    let field = collection.payload_schema.entry(key.to_owned()).or_default();
    field.rows_present += 1;
    field.inferred_types.insert(infer_json_type(value));
}

fn numeric_vector(value: &Value) -> Option<&Vec<Value>> {
    let array = value.as_array()?;
    if array.iter().all(|item| item.as_f64().is_some()) {
        Some(array)
    } else {
        None
    }
}

fn infer_json_type(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(_) => "boolean".to_owned(),
        Value::Number(number) if number.is_i64() || number.is_u64() => "integer".to_owned(),
        Value::Number(_) => "float".to_owned(),
        Value::String(_) => "string".to_owned(),
        Value::Array(values) => {
            let types = values.iter().map(infer_json_type).collect::<BTreeSet<_>>();
            format!("array<{}>", types.into_iter().collect::<Vec<_>>().join("|"))
        }
        Value::Object(_) => "object".to_owned(),
    }
}

fn format_json_scalar(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        other => other.to_string(),
    }
}

fn vector_name(name: &str) -> String {
    if name.is_empty() {
        "default".to_owned()
    } else {
        sanitize_ident(name)
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
    fn reports_collections_dimensions_and_payload_schema() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(
            dir.path().join("collections.json"),
            r#"{
              "collections": [{
                "name": "docs",
                "config": {"params": {"vectors": {"size": 3, "distance": "Cosine"}}},
                "points": [
                  {"id": 1, "vector": [0.1, 0.2, 0.3], "payload": {"tenant": "a", "views": 10, "published": true}},
                  {"id": 2, "vector": [0.4, 0.5, 0.6], "payload": {"tenant": "b", "tags": ["x", "y"]}}
                ]
              }]
            }"#,
        )
        .expect("fixture");

        let report = analyze_export(dir.path()).expect("analyze");

        assert_eq!(report.files_scanned, 1);
        let docs = report.collections.get("docs").expect("docs collection");
        assert_eq!(docs.points, 2);
        let dims = docs
            .vector_dimensions
            .get("default")
            .expect("default vector");
        assert_eq!(dims.declared_dims, Some(3));
        assert_eq!(dims.observed_dimensions.get(&3), Some(&2));
        assert!(dims.mismatches.is_empty());
        assert_eq!(docs.payload_schema["tenant"].rows_present, 2);
        assert!(docs.payload_schema["tenant"]
            .inferred_types
            .contains("string"));
        assert!(docs.payload_schema["views"]
            .inferred_types
            .contains("integer"));
        assert!(docs.payload_schema["published"]
            .inferred_types
            .contains("boolean"));
        assert!(docs.payload_schema["tags"]
            .inferred_types
            .contains("array<string>"));
    }

    #[test]
    fn flags_declared_dimension_mismatch() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(
            dir.path().join("bad.json"),
            r#"{
              "name": "bad",
              "params": {"vectors": {"size": 4}},
              "points": [{"id": "p1", "vector": [1.0, 2.0, 3.0], "payload": {"kind": "bad"}}]
            }"#,
        )
        .expect("fixture");

        let report = analyze_export(dir.path()).expect("analyze");
        let dims = &report.collections["bad"].vector_dimensions["default"];
        assert_eq!(dims.declared_dims, Some(4));
        assert_eq!(dims.observed_dimensions.get(&3), Some(&1));
        assert_eq!(dims.mismatches.len(), 1);
    }
}
