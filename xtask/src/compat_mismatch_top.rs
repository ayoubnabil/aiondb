//! `cargo xtask compat-mismatch-top` - summarize persisted pg-regress mismatches.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

const DEFAULT_INPUT: &str = "target/compat/pg-regress-mismatches.jsonl";

#[derive(Clone, Debug)]
pub struct CompatMismatchTopOptions {
    input: PathBuf,
    limit: usize,
    json: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct MismatchSummary {
    total: usize,
    by_category: BTreeMap<String, usize>,
    by_suite: BTreeMap<String, usize>,
}

pub fn parse_args(args: &[String]) -> Result<CompatMismatchTopOptions, String> {
    let mut input = PathBuf::from(DEFAULT_INPUT);
    let mut limit = 20usize;
    let mut json = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--input" => {
                i += 1;
                input = PathBuf::from(
                    args.get(i)
                        .ok_or_else(|| "--input requires a value".to_owned())?,
                );
            }
            "--limit" => {
                i += 1;
                limit = args
                    .get(i)
                    .ok_or_else(|| "--limit requires a value".to_owned())?
                    .parse::<usize>()
                    .map_err(|error| format!("invalid --limit: {error}"))?
                    .max(1);
            }
            "--json" => json = true,
            "--help" | "-h" => {
                println!(
                    "Usage: cargo xtask compat-mismatch-top [--input <PATH>] [--limit <N>] [--json]\n\n\
Reads pg-regress mismatch JSONL and prints top mismatch categories and suites."
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown flag: {other}")),
        }
        i += 1;
    }
    Ok(CompatMismatchTopOptions { input, limit, json })
}

pub fn run(opts: CompatMismatchTopOptions) -> Result<(), String> {
    let root = repo_root();
    let input = if opts.input.is_absolute() {
        opts.input.clone()
    } else {
        root.join(&opts.input)
    };
    let src = fs::read_to_string(&input)
        .map_err(|error| format!("reading {}: {error}", input.display()))?;
    let summary = summarize_jsonl(&src)?;
    if opts.json {
        print_json(&summary, opts.limit);
    } else {
        print_human(&summary, opts.limit, &input);
    }
    Ok(())
}

fn repo_root() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p
}

fn summarize_jsonl(src: &str) -> Result<MismatchSummary, String> {
    let mut summary = MismatchSummary::default();
    for (idx, line) in src.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(trimmed)
            .map_err(|error| format!("line {}: invalid JSON: {error}", idx + 1))?;
        let category = value
            .get("category")
            .and_then(Value::as_str)
            .unwrap_or("<missing category>");
        let suite = value
            .get("suite")
            .and_then(Value::as_str)
            .unwrap_or("<missing suite>");
        summary.total += 1;
        *summary.by_category.entry(category.to_owned()).or_default() += 1;
        *summary.by_suite.entry(suite.to_owned()).or_default() += 1;
    }
    Ok(summary)
}

fn ranked(map: &BTreeMap<String, usize>, limit: usize) -> Vec<(&str, usize)> {
    let mut rows: Vec<_> = map
        .iter()
        .map(|(key, count)| (key.as_str(), *count))
        .collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    rows.truncate(limit);
    rows
}

fn print_human(summary: &MismatchSummary, limit: usize, input: &std::path::Path) {
    println!("== compat-mismatch-top ==");
    println!("input: {}", input.display());
    println!("total mismatches: {}", summary.total);
    println!("top categories:");
    for (category, count) in ranked(&summary.by_category, limit) {
        println!("  {count:>6}  {category}");
    }
    println!("top suites:");
    for (suite, count) in ranked(&summary.by_suite, limit) {
        println!("  {count:>6}  {suite}");
    }
}

fn print_json(summary: &MismatchSummary, limit: usize) {
    println!(
        "{{\"total\":{},\"top_categories\":{},\"top_suites\":{}}}",
        summary.total,
        json_rows(ranked(&summary.by_category, limit)),
        json_rows(ranked(&summary.by_suite, limit))
    );
}

fn json_rows(rows: Vec<(&str, usize)>) -> String {
    format!(
        "[{}]",
        rows.into_iter()
            .map(|(key, count)| format!("{{\"name\":\"{}\",\"count\":{count}}}", json_escape(key)))
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn json_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarizes_categories_and_suites() {
        let src = r#"
{"suite":"select","category":"output differs","repro_hash":"a"}
{"suite":"select","category":"output differs","repro_hash":"b"}
{"suite":"join","category":"unexpected error","repro_hash":"c"}
"#;
        let summary = summarize_jsonl(src).expect("summary");
        assert_eq!(summary.total, 3);
        assert_eq!(summary.by_category["output differs"], 2);
        assert_eq!(summary.by_suite["select"], 2);
    }
}
