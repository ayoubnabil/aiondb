//! `cargo xtask check-ignored-tests` - audit `#[ignore]` test annotations.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const SCAN_DIRS: &[&str] = &["crates", "testing", "xtask"];

#[derive(Clone, Debug)]
pub struct CheckIgnoredTestsOptions {
    pub json: bool,
}

#[derive(Debug, Default)]
struct Report {
    scanned_files: usize,
    ignored_tests: usize,
    violations: Vec<String>,
}

pub fn parse_args(args: &[String]) -> Result<CheckIgnoredTestsOptions, String> {
    let mut json = false;
    for a in args {
        match a.as_str() {
            "--json" => json = true,
            "--help" | "-h" => {
                println!(
                    "Usage: cargo xtask check-ignored-tests [--json]\n\n\
Rejects #[ignore] tests without nearby tracked:/target: metadata, or with an \
expired target date."
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown flag: {other}")),
        }
    }
    Ok(CheckIgnoredTestsOptions { json })
}

pub fn run(opts: CheckIgnoredTestsOptions) -> Result<(), String> {
    let root = repo_root();
    let mut report = Report::default();
    let today = current_utc_day_number();

    for dir in SCAN_DIRS {
        for path in rust_files_under(&root.join(dir)) {
            if path.ends_with("xtask/src/check_ignored_tests.rs") {
                continue;
            }
            report.scanned_files += 1;
            let Ok(src) = fs::read_to_string(&path) else {
                continue;
            };
            scan_file(&root, &path, &src, today, &mut report);
        }
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
            "{} unjustified ignored test(s)",
            report.violations.len()
        ))
    }
}

fn repo_root() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p
}

fn rust_files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_rust_files(dir, &mut out);
    out
}

fn collect_rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.is_dir() {
            if matches!(name, "target" | ".git" | "node_modules") {
                continue;
            }
            collect_rust_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

fn scan_file(root: &Path, path: &Path, src: &str, today: i64, report: &mut Report) {
    let rel = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned();
    let lines: Vec<&str> = src.lines().collect();
    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if !(trimmed.starts_with("#[ignore]") || trimmed.starts_with("#[ignore ")) {
            continue;
        }
        report.ignored_tests += 1;
        let start = idx.saturating_sub(5);
        let window = lines[start..idx].join(" ");
        if !window.contains("tracked:") {
            report
                .violations
                .push(format!("{rel}:{} missing `tracked:`", idx + 1));
        }
        let Some(target) = extract_target_date(&window) else {
            report
                .violations
                .push(format!("{rel}:{} missing `target: YYYY-MM-DD`", idx + 1));
            continue;
        };
        match day_number_from_ymd(&target) {
            Some(target_day) if target_day >= today => {}
            Some(_) => report.violations.push(format!(
                "{rel}:{} expired ignore target `{target}`",
                idx + 1
            )),
            None => report.violations.push(format!(
                "{rel}:{} invalid ignore target `{target}`",
                idx + 1
            )),
        }
    }
}

fn extract_target_date(window: &str) -> Option<String> {
    let rest = window.split("target:").nth(1)?.trim_start();
    let date = rest
        .chars()
        .take_while(|ch| ch.is_ascii_digit() || *ch == '-')
        .collect::<String>();
    (!date.is_empty()).then_some(date)
}

fn current_utc_day_number() -> i64 {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    (seconds / 86_400) as i64
}

fn day_number_from_ymd(date: &str) -> Option<i64> {
    let mut parts = date.split('-');
    let year = parts.next()?.parse::<i64>().ok()?;
    let month = parts.next()?.parse::<i64>().ok()?;
    let day = parts.next()?.parse::<i64>().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(days_from_civil(year, month, day))
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn print_human(report: &Report) {
    println!("== check-ignored-tests ==");
    println!("scanned files: {}", report.scanned_files);
    println!("ignored tests: {}", report.ignored_tests);
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
        "{{\"scanned_files\":{},\"ignored_tests\":{},\"violations\":[{}]}}",
        report.scanned_files,
        report.ignored_tests,
        report
            .violations
            .iter()
            .map(|v| format!("\"{}\"", json_escape(v)))
            .collect::<Vec<_>>()
            .join(",")
    );
}

fn json_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_future_tracked_ignore() {
        let mut report = Report::default();
        scan_file(
            Path::new("/repo"),
            Path::new("/repo/crates/x/src/lib.rs"),
            "// tracked: issue-1\n// target: 2099-01-01\n#[test]\n#[ignore]\nfn slow() {}\n",
            days_from_civil(2026, 4, 24),
            &mut report,
        );
        assert_eq!(report.violations, Vec::<String>::new());
        assert_eq!(report.ignored_tests, 1);
    }

    #[test]
    fn rejects_expired_ignore() {
        let mut report = Report::default();
        scan_file(
            Path::new("/repo"),
            Path::new("/repo/crates/x/src/lib.rs"),
            "// tracked: issue-1\n// target: 2020-01-01\n#[test]\n#[ignore]\nfn slow() {}\n",
            days_from_civil(2026, 4, 24),
            &mut report,
        );
        assert!(report
            .violations
            .iter()
            .any(|violation| violation.contains("expired ignore target")));
    }
}
