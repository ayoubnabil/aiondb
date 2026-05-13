use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const DEFAULT_HISTORY_PATH: &str = "tmp/clean_prod/ecosystem_compat_history.jsonl";
const DEFAULT_LIMIT: usize = 10;

#[derive(Debug, Clone)]
pub(crate) struct EcosystemCompatTrendOptions {
    history_path: PathBuf,
    limit: usize,
    json: bool,
    since: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct HistoryEntry {
    #[serde(default)]
    generated_at_unix_ms: u128,
    #[serde(default)]
    generated_at_iso8601: String,
    #[serde(default)]
    git_commit_full: Option<String>,
    #[serde(default)]
    git_commit_short: Option<String>,
    #[serde(default)]
    git_dirty: Option<bool>,
    #[serde(default)]
    listen_addr: String,
    #[serde(default)]
    strict: bool,
    summary: HistorySummary,
    #[serde(default)]
    suites: Vec<HistorySuite>,
}

#[derive(Debug, Deserialize, Serialize)]
struct HistorySummary {
    #[serde(default)]
    passed: usize,
    #[serde(default)]
    failed: usize,
    #[serde(default)]
    skipped: usize,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct HistorySuite {
    name: String,
    status: String,
    #[serde(default)]
    duration_ms: u128,
}

pub(crate) fn parse_args(args: &[String]) -> Result<EcosystemCompatTrendOptions, String> {
    let mut history_path = PathBuf::from(DEFAULT_HISTORY_PATH);
    let mut limit = DEFAULT_LIMIT;
    let mut json = false;
    let mut since: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--history" => {
                i += 1;
                history_path = PathBuf::from(
                    args.get(i)
                        .ok_or_else(|| "--history requires a value".to_owned())?,
                );
            }
            "--limit" => {
                i += 1;
                let raw = args
                    .get(i)
                    .ok_or_else(|| "--limit requires a value".to_owned())?;
                limit = raw
                    .parse::<usize>()
                    .map_err(|error| format!("--limit must be a positive integer: {error}"))?;
                if limit == 0 {
                    return Err("--limit must be greater than zero".to_owned());
                }
            }
            "--json" => {
                json = true;
            }
            "--since" => {
                i += 1;
                let raw = args
                    .get(i)
                    .ok_or_else(|| "--since requires a value".to_owned())?
                    .clone();
                if raw.is_empty() {
                    return Err("--since requires a non-empty commit prefix".to_owned());
                }
                since = Some(raw);
            }
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            other => {
                return Err(format!(
                    "unknown flag for ecosystem-compat-trend: {other}\n\nRun `cargo xtask ecosystem-compat-trend --help` for usage."
                ));
            }
        }
        i += 1;
    }

    Ok(EcosystemCompatTrendOptions {
        history_path,
        limit,
        json,
        since,
    })
}

pub(crate) fn run(opts: EcosystemCompatTrendOptions) -> Result<(), String> {
    let entries = load_history(&opts.history_path)?;
    let filtered = apply_filters(entries, opts.since.as_deref(), opts.limit);

    if opts.json {
        let payload = serde_json::json!({
            "history_path": opts.history_path.display().to_string(),
            "limit": opts.limit,
            "since": opts.since,
            "runs": filtered,
        });
        let rendered = serde_json::to_string_pretty(&payload)
            .map_err(|error| format!("failed to render trend JSON: {error}"))?;
        println!("{rendered}");
    } else {
        render_table(&opts.history_path, &filtered);
    }

    Ok(())
}

fn print_usage() {
    println!(
        "\
Usage: cargo xtask ecosystem-compat-trend [OPTIONS]

Render a trend view over the JSONL history written by `cargo xtask ecosystem-compat`.
Each line in the history file represents one run keyed by commit and timestamp.

Options:
  --history <PATH>  Path to the JSONL history (default: tmp/clean_prod/ecosystem_compat_history.jsonl)
  --limit <N>       Show the last N runs (default: 10, must be > 0)
  --since <COMMIT>  Drop entries older than the first occurrence of <COMMIT>
                    (matches against full or short SHA prefix)
  --json            Emit JSON instead of a human-readable table
  -h, --help        Print this help message"
    );
}

fn load_history(path: &Path) -> Result<Vec<HistoryEntry>, String> {
    let raw = fs::read_to_string(path)
        .map_err(|error| format!("failed to read history file {}: {error}", path.display()))?;
    let mut entries = Vec::new();
    for (idx, line) in raw.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let entry: HistoryEntry = serde_json::from_str(trimmed).map_err(|error| {
            format!(
                "failed to parse history line {} in {}: {error}",
                idx + 1,
                path.display()
            )
        })?;
        entries.push(entry);
    }
    Ok(entries)
}

fn apply_filters(
    mut entries: Vec<HistoryEntry>,
    since: Option<&str>,
    limit: usize,
) -> Vec<HistoryEntry> {
    if let Some(prefix) = since {
        if let Some(start_idx) = entries
            .iter()
            .position(|entry| commit_matches(entry, prefix))
        {
            entries.drain(..start_idx);
        } else {
            entries.clear();
        }
    }
    if entries.len() > limit {
        let drop = entries.len() - limit;
        entries.drain(..drop);
    }
    entries
}

fn commit_matches(entry: &HistoryEntry, prefix: &str) -> bool {
    let short_match = entry
        .git_commit_short
        .as_deref()
        .is_some_and(|sha| sha.starts_with(prefix));
    let full_match = entry
        .git_commit_full
        .as_deref()
        .is_some_and(|sha| sha.starts_with(prefix));
    short_match || full_match
}

fn render_table(history_path: &Path, entries: &[HistoryEntry]) {
    println!("=== AionDB ecosystem compatibility trend ===");
    println!("history: {}", history_path.display());
    if entries.is_empty() {
        println!("(no entries match the requested filters)");
        return;
    }

    let suite_names = collect_suite_names(entries);
    let header_pre = ["#", "date", "commit", "strict", "P/F/S"];
    let mut headers: Vec<String> = header_pre.iter().map(|s| (*s).to_owned()).collect();
    for name in &suite_names {
        headers.push(name.clone());
    }

    let rows: Vec<Vec<String>> = entries
        .iter()
        .enumerate()
        .map(|(idx, entry)| build_row(idx + 1, entry, &suite_names))
        .collect();

    let widths = compute_column_widths(&headers, &rows);
    print_row(&headers, &widths);
    print_separator(&widths);
    for row in &rows {
        print_row(row, &widths);
    }
    println!("\nlegend: P=passed, F=failed, S=skipped, -=suite absent in that run");
}

fn collect_suite_names(entries: &[HistoryEntry]) -> Vec<String> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut order: Vec<String> = Vec::new();
    for entry in entries {
        for suite in &entry.suites {
            if seen.insert(suite.name.clone()) {
                order.push(suite.name.clone());
            }
        }
    }
    order
}

fn build_row(index: usize, entry: &HistoryEntry, suite_names: &[String]) -> Vec<String> {
    let date = if entry.generated_at_iso8601.is_empty() {
        "-".to_owned()
    } else {
        entry.generated_at_iso8601.clone()
    };
    let commit = match entry.git_commit_short.as_deref() {
        Some(short) => {
            let mut display = short.to_owned();
            if entry.git_dirty == Some(true) {
                display.push('*');
            }
            display
        }
        None => "-".to_owned(),
    };
    let strict_marker = if entry.strict { "yes" } else { "no" };
    let pfs = format!(
        "{}/{}/{}",
        entry.summary.passed, entry.summary.failed, entry.summary.skipped
    );

    let mut row = vec![
        index.to_string(),
        date,
        commit,
        strict_marker.to_owned(),
        pfs,
    ];
    for suite_name in suite_names {
        let cell = entry
            .suites
            .iter()
            .find(|suite| suite.name == *suite_name)
            .map_or_else(|| "-".to_owned(), |suite| status_letter(&suite.status));
        row.push(cell);
    }
    row
}

fn status_letter(status: &str) -> String {
    match status {
        "passed" => "P".to_owned(),
        "failed" => "F".to_owned(),
        "skipped" => "S".to_owned(),
        other => other.chars().take(1).collect::<String>().to_uppercase(),
    }
}

fn compute_column_widths(headers: &[String], rows: &[Vec<String>]) -> Vec<usize> {
    let mut widths: Vec<usize> = headers.iter().map(String::len).collect();
    for row in rows {
        for (idx, cell) in row.iter().enumerate() {
            if idx >= widths.len() {
                widths.push(cell.len());
            } else if cell.len() > widths[idx] {
                widths[idx] = cell.len();
            }
        }
    }
    widths
}

fn print_row(cells: &[String], widths: &[usize]) {
    let mut parts = Vec::with_capacity(cells.len());
    for (idx, cell) in cells.iter().enumerate() {
        let width = widths.get(idx).copied().unwrap_or(cell.len());
        parts.push(format!("{cell:<width$}"));
    }
    println!("{}", parts.join("  "));
}

fn print_separator(widths: &[usize]) {
    let parts: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
    println!("{}", parts.join("  "));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(
        commit_short: Option<&str>,
        commit_full: Option<&str>,
        suites: Vec<(&str, &str)>,
        passed: usize,
        failed: usize,
        skipped: usize,
    ) -> HistoryEntry {
        HistoryEntry {
            generated_at_unix_ms: 0,
            generated_at_iso8601: "1970-01-01T00:00:00.000Z".to_owned(),
            git_commit_full: commit_full.map(ToOwned::to_owned),
            git_commit_short: commit_short.map(ToOwned::to_owned),
            git_dirty: Some(false),
            listen_addr: "127.0.0.1:0".to_owned(),
            strict: false,
            summary: HistorySummary {
                passed,
                failed,
                skipped,
            },
            suites: suites
                .into_iter()
                .map(|(name, status)| HistorySuite {
                    name: name.to_owned(),
                    status: status.to_owned(),
                    duration_ms: 0,
                })
                .collect(),
        }
    }

    #[test]
    fn parse_args_defaults() {
        let opts = parse_args(&[]).expect("default args should parse");
        assert_eq!(opts.history_path, PathBuf::from(DEFAULT_HISTORY_PATH));
        assert_eq!(opts.limit, DEFAULT_LIMIT);
        assert!(!opts.json);
        assert!(opts.since.is_none());
    }

    #[test]
    fn parse_args_rejects_zero_limit() {
        let error =
            parse_args(&["--limit".to_owned(), "0".to_owned()]).expect_err("limit 0 should fail");
        assert!(error.contains("--limit"));
    }

    #[test]
    fn parse_args_accepts_since_and_limit_and_json() {
        let opts = parse_args(&[
            "--limit".to_owned(),
            "3".to_owned(),
            "--since".to_owned(),
            "abcdef".to_owned(),
            "--json".to_owned(),
            "--history".to_owned(),
            "tmp/custom.jsonl".to_owned(),
        ])
        .expect("args should parse");
        assert_eq!(opts.limit, 3);
        assert_eq!(opts.since.as_deref(), Some("abcdef"));
        assert!(opts.json);
        assert_eq!(opts.history_path, PathBuf::from("tmp/custom.jsonl"));
    }

    #[test]
    fn apply_filters_limits_to_last_n() {
        let entries = vec![
            entry(
                Some("aaaaaa000000"),
                Some("aaaaaa000000ffff"),
                vec![],
                1,
                0,
                0,
            ),
            entry(
                Some("bbbbbb000000"),
                Some("bbbbbb000000ffff"),
                vec![],
                1,
                0,
                0,
            ),
            entry(
                Some("cccccc000000"),
                Some("cccccc000000ffff"),
                vec![],
                1,
                0,
                0,
            ),
        ];
        let filtered = apply_filters(entries, None, 2);
        assert_eq!(filtered.len(), 2);
        assert_eq!(
            filtered[0].git_commit_short.as_deref(),
            Some("bbbbbb000000")
        );
        assert_eq!(
            filtered[1].git_commit_short.as_deref(),
            Some("cccccc000000")
        );
    }

    #[test]
    fn apply_filters_respects_since_prefix() {
        let entries = vec![
            entry(
                Some("aaaaaa000000"),
                Some("aaaaaa000000ffff"),
                vec![],
                1,
                0,
                0,
            ),
            entry(
                Some("bbbbbb000000"),
                Some("bbbbbb000000ffff"),
                vec![],
                1,
                0,
                0,
            ),
            entry(
                Some("cccccc000000"),
                Some("cccccc000000ffff"),
                vec![],
                1,
                0,
                0,
            ),
        ];
        let filtered = apply_filters(entries, Some("bbbb"), 10);
        assert_eq!(filtered.len(), 2);
        assert_eq!(
            filtered[0].git_commit_short.as_deref(),
            Some("bbbbbb000000")
        );
    }

    #[test]
    fn apply_filters_unknown_since_returns_empty() {
        let entries = vec![entry(
            Some("aaaaaa000000"),
            Some("aaaaaa000000ffff"),
            vec![],
            1,
            0,
            0,
        )];
        let filtered = apply_filters(entries, Some("ffff"), 10);
        assert!(filtered.is_empty());
    }

    #[test]
    fn collect_suite_names_preserves_first_seen_order() {
        let entries = vec![
            entry(
                None,
                None,
                vec![("psql-libpq", "passed"), ("diesel", "skipped")],
                1,
                0,
                1,
            ),
            entry(
                None,
                None,
                vec![("diesel", "passed"), ("psycopg", "passed")],
                2,
                0,
                0,
            ),
        ];
        let names = collect_suite_names(&entries);
        assert_eq!(names, vec!["psql-libpq", "diesel", "psycopg"]);
    }

    #[test]
    fn build_row_marks_missing_suites_as_dash() {
        let row_entry = entry(
            Some("aabbccdd1122"),
            Some("aabbccdd1122eeff"),
            vec![("psql-libpq", "passed")],
            1,
            0,
            0,
        );
        let suite_names = vec!["psql-libpq".to_owned(), "diesel".to_owned()];
        let row = build_row(7, &row_entry, &suite_names);
        assert_eq!(row[0], "7");
        assert_eq!(row[2], "aabbccdd1122");
        assert_eq!(row[4], "1/0/0");
        assert_eq!(row[5], "P");
        assert_eq!(row[6], "-");
    }

    #[test]
    fn build_row_appends_dirty_marker() {
        let mut entry = entry(
            Some("aabbccdd1122"),
            Some("aabbccdd1122eeff"),
            vec![],
            0,
            0,
            0,
        );
        entry.git_dirty = Some(true);
        let row = build_row(1, &entry, &[]);
        assert_eq!(row[2], "aabbccdd1122*");
    }

    #[test]
    fn status_letter_normalizes_known_statuses() {
        assert_eq!(status_letter("passed"), "P");
        assert_eq!(status_letter("failed"), "F");
        assert_eq!(status_letter("skipped"), "S");
        assert_eq!(status_letter("unknown_state"), "U");
    }
}
