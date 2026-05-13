//! `cargo xtask check-runner-cheats` - guard compatibility runner scoring.
//!
//! This is a static lint for the measurement layer. It intentionally targets
//! narrow, high-risk patterns instead of judging feature compatibility.

use std::fs;
use std::path::{Path, PathBuf};

const SCAN_ROOTS: &[&str] = &[
    "xtask/src",
    ".pg-regress/src",
    ".pg-regress/run_all_safe.sh",
];

#[derive(Clone, Debug)]
pub struct CheckRunnerCheatsOptions {
    pub json: bool,
}

#[derive(Debug, Default)]
struct Report {
    scanned_files: usize,
    violations: Vec<String>,
}

pub fn parse_args(args: &[String]) -> Result<CheckRunnerCheatsOptions, String> {
    let mut json = false;
    for a in args {
        match a.as_str() {
            "--json" => json = true,
            "--help" | "-h" => {
                println!(
                    "Usage: cargo xtask check-runner-cheats [--json]\n\n\
Rejects compatibility runner scoring shortcuts: non-empty hard exclusions, \
0/0 success, skipped suites counted as pass, and global mismatch tolerances."
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown flag: {other}")),
        }
    }
    Ok(CheckRunnerCheatsOptions { json })
}

pub fn run(opts: CheckRunnerCheatsOptions) -> Result<(), String> {
    let root = repo_root();
    let files = runner_files(&root);
    let mut report = Report::default();

    for path in files {
        if path.ends_with("xtask/src/check_runner_cheats.rs") {
            continue;
        }
        let Ok(src) = fs::read_to_string(&path) else {
            continue;
        };
        report.scanned_files += 1;
        scan_file(&root, &path, &src, &mut report.violations);
    }

    if opts.json {
        print_json(&report);
    } else {
        print_human(&report);
    }

    if report.violations.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} compatibility runner cheat pattern(s)",
            report.violations.len()
        ))
    }
}

fn repo_root() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p
}

fn runner_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for scan_root in SCAN_ROOTS {
        let path = root.join(scan_root);
        if path.is_file() {
            out.push(path);
        } else {
            collect_files(&path, &mut out);
        }
    }
    out.sort();
    out.dedup();
    out
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out);
        } else if matches!(path.extension().and_then(|e| e.to_str()), Some("rs" | "sh")) {
            out.push(path);
        }
    }
}

fn scan_file(root: &Path, path: &Path, src: &str, violations: &mut Vec<String>) {
    let rel = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned();
    let lines: Vec<&str> = src.lines().collect();

    if let Some(line) = non_empty_hard_exclusion_line(&lines) {
        violations.push(format!(
            "{rel}:{line}: hard-coded compatibility exclusions must stay empty or move to a documented task/config report"
        ));
    }

    for (idx, line) in lines.iter().enumerate() {
        let line_no = idx + 1;
        let normalized = line.split("//").next().unwrap_or(line).trim();
        let lower = normalized.to_ascii_lowercase();
        if normalized.is_empty() || normalized.starts_with('#') {
            continue;
        }

        let prev = lines
            .get(idx.saturating_sub(1))
            .copied()
            .unwrap_or_default();
        let next = lines.get(idx + 1).copied().unwrap_or_default();
        if counts_zero_total_as_success(prev, normalized, next) {
            violations.push(format!(
                "{rel}:{line_no}: possible 0/0 success; require an explicit total > 0 guard"
            ));
        }
        if skipped_status_counted_as_pass(normalized) {
            violations.push(format!(
                "{rel}:{line_no}: skipped compatibility suite appears to be counted as passed"
            ));
        }
        if global_tolerance_switch(&lower) {
            violations.push(format!(
                "{rel}:{line_no}: global runner tolerance switch is forbidden"
            ));
        }
        if success_on_empty_suite(normalized, Some(prev)) {
            violations.push(format!(
                "{rel}:{line_no}: empty compatibility suite must fail, not succeed"
            ));
        }
    }
}

fn non_empty_hard_exclusion_line(lines: &[&str]) -> Option<usize> {
    for (idx, line) in lines.iter().enumerate() {
        if !line.contains("HARD_EXCLUDED_FILES") {
            continue;
        }
        let trimmed = line.trim();
        if trimmed.contains("&[]") {
            return None;
        }
        if trimmed.ends_with("&[") || trimmed.ends_with("= &[") {
            for (offset, next) in lines.iter().enumerate().skip(idx + 1) {
                let next = next.trim();
                if next.starts_with("];") {
                    return None;
                }
                if next.starts_with('"') {
                    return Some(offset + 1);
                }
            }
        } else if trimmed.contains("&[") && trimmed.contains('"') {
            return Some(idx + 1);
        }
    }
    None
}

fn counts_zero_total_as_success(previous: &str, line: &str, next: &str) -> bool {
    let compact = line.replace(' ', "");
    let context = format!("{previous} {line} {next}").replace(' ', "");
    let compares_equal = compact.contains("matched==total")
        || compact.contains("$matched\"-eq\"$total")
        || compact.contains("$matched-eq$total")
        || compact.contains("*m==*t")
        || compact.contains("m==t");
    compares_equal
        && !context.contains("total>0")
        && !context.contains("*t>0")
        && !context.contains("*total>0")
        && !context.contains("$total\"-gt0")
}

fn skipped_status_counted_as_pass(line: &str) -> bool {
    let compact = line.replace(' ', "");
    (compact.contains("SuiteStatus::Skipped") || compact.contains("status:Skipped"))
        && (compact.contains("SuiteStatus::Passed") || compact.contains("passed+="))
}

fn global_tolerance_switch(lower: &str) -> bool {
    let suspicious_name = lower.contains("allow_mismatch")
        || lower.contains("ignore_mismatch")
        || lower.contains("ignore_failure")
        || lower.contains("tolerate_failure")
        || lower.contains("force_success");
    suspicious_name && (lower.contains("env") || lower.contains("var") || lower.contains("const"))
}

fn success_on_empty_suite(line: &str, previous_line: Option<&str>) -> bool {
    let prev = previous_line.unwrap_or_default().replace(' ', "");
    let current = line.replace(' ', "");
    (prev.contains("TOTAL\"-eq0") || prev.contains("total==0") || prev.contains("is_empty()"))
        && (current.contains("exit0") || current.contains("ExitCode::SUCCESS"))
}

fn print_human(report: &Report) {
    println!("== check-runner-cheats ==");
    println!("scanned files: {}", report.scanned_files);
    if report.violations.is_empty() {
        println!("result: ok");
    } else {
        println!("result: failed");
        for violation in &report.violations {
            println!("- {violation}");
        }
    }
}

fn print_json(report: &Report) {
    println!(
        "{{\"scanned_files\":{},\"violations\":[{}]}}",
        report.scanned_files,
        report
            .violations
            .iter()
            .map(|v| format!("\"{}\"", json_escape(v)))
            .collect::<Vec<_>>()
            .join(",")
    );
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
    fn matched_equals_total_requires_positive_total() {
        assert!(counts_zero_total_as_success(
            "",
            "if matched == total { passed += 1; }",
            ""
        ));
        assert!(!counts_zero_total_as_success(
            "",
            "if matched == total && total > 0 { passed += 1; }",
            ""
        ));
        assert!(!counts_zero_total_as_success(
            "*total > 0",
            "if matched == total { passed += 1; }",
            ""
        ));
    }

    #[test]
    fn detects_non_empty_hard_exclusions() {
        let lines = [
            "const HARD_EXCLUDED_FILES: &[&str] = &[",
            "    \"lock\",",
            "];",
        ];
        assert_eq!(non_empty_hard_exclusion_line(&lines), Some(2));
        assert_eq!(
            non_empty_hard_exclusion_line(&["const HARD_EXCLUDED_FILES: &[&str] = &[];"]),
            None
        );
    }

    #[test]
    fn empty_suite_success_is_rejected() {
        assert!(success_on_empty_suite(
            "return ExitCode::SUCCESS;",
            Some("if suites.is_empty() {")
        ));
        assert!(!success_on_empty_suite(
            "return ExitCode::FAILURE;",
            Some("if suites.is_empty() {")
        ));
    }
}
