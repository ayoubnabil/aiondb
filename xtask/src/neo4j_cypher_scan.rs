//! `cargo xtask neo4j-cypher-scan` - classify Neo4j Cypher query compatibility.

use aiondb_parser::{
    parse_sql, CypherClause, CypherMergeClause, CypherPathFunction, CypherPathPattern,
    CypherRelPattern, CypherStatement, Statement,
};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

const DEFAULT_REPORT: &str = "target/compat/neo4j-cypher-scan-report.json";

#[derive(Clone, Debug)]
pub struct Neo4jCypherScanOptions {
    input: PathBuf,
    report: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Compatibility {
    Supported,
    NeedsRewrite,
    Unsupported,
}

#[derive(Debug, Serialize)]
struct QueryFinding {
    source: String,
    statement_index: usize,
    compatibility: Compatibility,
    reasons: Vec<String>,
    query: String,
}

#[derive(Debug, Serialize)]
struct Neo4jCypherScanReport {
    input: String,
    summary: ScanSummary,
    findings: Vec<QueryFinding>,
}

#[derive(Debug, Default, Serialize)]
struct ScanSummary {
    total: usize,
    supported: usize,
    needs_rewrite: usize,
    unsupported: usize,
}

pub fn parse_args(args: &[String]) -> Result<Neo4jCypherScanOptions, String> {
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
                    "Usage: cargo xtask neo4j-cypher-scan --input <FILE_OR_DIR> [--report <PATH>]\n\n\
Classifies Cypher queries as supported, needs_rewrite, or unsupported."
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown flag: {other}")),
        }
        i += 1;
    }
    Ok(Neo4jCypherScanOptions {
        input: input.ok_or_else(|| "--input is required".to_owned())?,
        report,
    })
}

pub fn run(opts: Neo4jCypherScanOptions) -> Result<(), String> {
    let root = repo_root();
    let input = absolutize(&root, &opts.input);
    let report_path = absolutize(&root, &opts.report);
    let report = scan_input(&input)?;
    write_json(&report_path, &report)?;
    println!("== neo4j-cypher-scan ==");
    println!("input: {}", input.display());
    println!("report: {}", report_path.display());
    println!(
        "queries: {} supported={} needs_rewrite={} unsupported={}",
        report.summary.total,
        report.summary.supported,
        report.summary.needs_rewrite,
        report.summary.unsupported
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

fn scan_input(input: &Path) -> Result<Neo4jCypherScanReport, String> {
    let mut findings = Vec::new();
    for path in query_files(input)? {
        let src = fs::read_to_string(&path)
            .map_err(|error| format!("reading {}: {error}", path.display()))?;
        for (idx, query) in split_queries(&src).into_iter().enumerate() {
            findings.push(classify_query(&path.display().to_string(), idx + 1, &query));
        }
    }
    let mut summary = ScanSummary::default();
    for finding in &findings {
        summary.total += 1;
        match finding.compatibility {
            Compatibility::Supported => summary.supported += 1,
            Compatibility::NeedsRewrite => summary.needs_rewrite += 1,
            Compatibility::Unsupported => summary.unsupported += 1,
        }
    }
    Ok(Neo4jCypherScanReport {
        input: input.display().to_string(),
        summary,
        findings,
    })
}

fn query_files(input: &Path) -> Result<Vec<PathBuf>, String> {
    if input.is_file() {
        return Ok(vec![input.to_path_buf()]);
    }
    if !input.is_dir() {
        return Err(format!("{} is not a file or directory", input.display()));
    }
    let mut files = fs::read_dir(input)
        .map_err(|error| format!("reading {}: {error}", input.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("reading {}: {error}", input.display()))?
        .into_iter()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .is_some_and(|ext| matches!(ext.to_str(), Some("cypher" | "cql" | "txt")))
        })
        .collect::<Vec<_>>();
    files.sort();
    Ok(files)
}

fn split_queries(src: &str) -> Vec<String> {
    src.split(';')
        .map(str::trim)
        .filter(|query| !query.is_empty() && !query.starts_with("//"))
        .map(ToOwned::to_owned)
        .collect()
}

fn classify_query(source: &str, statement_index: usize, query: &str) -> QueryFinding {
    let mut reasons = Vec::new();
    let compatibility = match parse_sql(query) {
        Ok(statements) if statements.len() == 1 => match &statements[0] {
            Statement::Cypher(stmt) => classify_statement(stmt, &mut reasons),
            _ => {
                reasons.push("not routed as a Cypher statement".to_owned());
                Compatibility::Unsupported
            }
        },
        Ok(statements) => {
            reasons.push(format!(
                "expected one statement, parsed {}",
                statements.len()
            ));
            Compatibility::Unsupported
        }
        Err(error) => {
            reasons.push(format!("parse error: {error}"));
            Compatibility::Unsupported
        }
    };
    QueryFinding {
        source: source.to_owned(),
        statement_index,
        compatibility,
        reasons,
        query: query.to_owned(),
    }
}

fn classify_statement(stmt: &CypherStatement, reasons: &mut Vec<String>) -> Compatibility {
    if stmt.union.is_some() {
        reasons
            .push("UNION requires migration rewrite into separate queries or SQL UNION".to_owned());
    }
    for clause in &stmt.clauses {
        match clause {
            CypherClause::Match(m) => {
                if m.optional {
                    reasons.push(
                        "OPTIONAL MATCH should be reviewed for null-extension parity".to_owned(),
                    );
                }
                for pattern in &m.patterns {
                    classify_pattern(pattern, reasons);
                }
            }
            CypherClause::Create(c) => {
                for pattern in &c.patterns {
                    classify_pattern(pattern, reasons);
                }
            }
            CypherClause::Merge(m) => classify_merge(m, reasons),
            CypherClause::Set(_) | CypherClause::Delete(_) | CypherClause::Remove(_) => {
                reasons.push(
                    "write mutation clauses need rewrite review for AionDB graph table layout"
                        .to_owned(),
                );
            }
            CypherClause::Unwind(_) | CypherClause::With(_) => {
                reasons.push(
                    "pipeline clause needs rewrite review for variable scope and row expansion"
                        .to_owned(),
                );
            }
            CypherClause::Call(c) => {
                reasons.push(format!(
                    "Neo4j procedure CALL {} is not portable to AionDB",
                    c.procedure
                ));
                return Compatibility::Unsupported;
            }
            CypherClause::Foreach(_) => {
                reasons
                    .push("FOREACH mutation loops are not supported by AionDB Cypher".to_owned());
                return Compatibility::Unsupported;
            }
            CypherClause::Return(_) => {}
        }
    }
    if reasons.iter().any(|r| {
        r.contains("shortestPath")
            || r.contains("allShortestPaths")
            || r.contains("procedure CALL")
            || r.contains("FOREACH")
    }) {
        Compatibility::Unsupported
    } else if reasons.is_empty() {
        reasons.push("parsed into supported AionDB Cypher subset".to_owned());
        Compatibility::Supported
    } else {
        Compatibility::NeedsRewrite
    }
}

fn classify_merge(merge: &CypherMergeClause, reasons: &mut Vec<String>) {
    classify_pattern(&merge.pattern, reasons);
    if !merge.pattern.rels.is_empty() {
        reasons.push("MERGE relationships require explicit import/rewrite".to_owned());
    }
    if !merge.actions.is_empty() {
        reasons.push("MERGE ON CREATE/ON MATCH actions need rewrite review".to_owned());
    }
}

fn classify_pattern(pattern: &CypherPathPattern, reasons: &mut Vec<String>) {
    if let Some(func) = pattern.path_function {
        reasons.push(match func {
            CypherPathFunction::ShortestPath => {
                "shortestPath requires graph algorithm support".to_owned()
            }
            CypherPathFunction::AllShortestPaths => {
                "allShortestPaths requires graph algorithm support".to_owned()
            }
        });
    }
    for rel in &pattern.rels {
        classify_rel(rel, reasons);
    }
}

fn classify_rel(rel: &CypherRelPattern, reasons: &mut Vec<String>) {
    if rel.variable_length {
        reasons.push(
            "variable-length relationships need rewrite or bounded traversal validation".to_owned(),
        );
    }
    if !rel.rel_types_alt.is_empty() {
        reasons.push(
            "alternate relationship types need rewrite into explicit OR/list handling".to_owned(),
        );
    }
    if !rel.properties.is_empty() {
        reasons.push("relationship property pattern filters need rewrite review".to_owned());
    }
}

fn write_json(path: &Path, report: &Neo4jCypherScanReport) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("creating {}: {error}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(report)
        .map_err(|error| format!("serializing Cypher scan report: {error}"))?;
    fs::write(path, format!("{json}\n"))
        .map_err(|error| format!("writing {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_supported_needs_rewrite_and_unsupported_queries() {
        let supported = classify_query("inline", 1, "MATCH (n:Person) RETURN n");
        assert_eq!(supported.compatibility, Compatibility::Supported);

        let rewrite = classify_query("inline", 2, "MATCH (a)-[*1..3]->(b) RETURN b");
        assert_eq!(rewrite.compatibility, Compatibility::NeedsRewrite);

        let unsupported = classify_query("inline", 3, "CALL db.labels() YIELD label RETURN label");
        assert_eq!(unsupported.compatibility, Compatibility::Unsupported);
    }

    #[test]
    fn scans_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(
            dir.path().join("queries.cypher"),
            "MATCH (n:Person) RETURN n;\nMATCH (a)-[*1..3]->(b) RETURN b;\nCALL db.labels() YIELD label RETURN label;\n",
        )
        .expect("queries");
        let report = scan_input(dir.path()).expect("scan");
        assert_eq!(report.summary.total, 3);
        assert_eq!(report.summary.supported, 1);
        assert_eq!(report.summary.needs_rewrite, 1);
        assert_eq!(report.summary.unsupported, 1);
    }
}
