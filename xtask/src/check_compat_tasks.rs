//! `cargo xtask check-compat-tasks` - validate the PG80 task ledger.
//!
//! The gate is intentionally strict around `status = "done"`: a task can only
//! be closed when it carries completion metadata and executable evidence.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

const TASK_FILE: &str = "docs/compat/tasks_80.toml";
const VALID_STATUSES: &[&str] = &["open", "in_progress", "blocked", "done", "rejected"];
const REQUIRED_KEYS: &[&str] = &[
    "id",
    "title",
    "area",
    "month",
    "status",
    "kind",
    "forbidden_shortcuts",
    "evidence_required",
];
const DUAL_MODE_EXCEPTION_IDS: &[&str] = &[
    // Measurement / CI tasks validate tooling rather than SQL behavior.
    "PG80-002", "PG80-003", "PG80-004", "PG80-005", "PG80-006", "PG80-007",
];

#[derive(Clone, Debug)]
pub struct CheckCompatTasksOptions {
    pub json: bool,
}

#[derive(Clone, Debug, Default)]
struct TaskRecord {
    line: usize,
    id: Option<String>,
    title: Option<String>,
    status: Option<String>,
    completed_at: Option<String>,
    completed_by: Option<String>,
    completion_commit: Option<String>,
    evidence: Vec<String>,
    evidence_required: Vec<String>,
    keys: BTreeSet<String>,
}

#[derive(Debug, Default)]
struct ValidationReport {
    total: usize,
    done: usize,
    open: usize,
    in_progress: usize,
    blocked: usize,
    rejected: usize,
    errors: Vec<String>,
}

pub fn parse_args(args: &[String]) -> Result<CheckCompatTasksOptions, String> {
    let mut json = false;
    for a in args {
        match a.as_str() {
            "--json" => json = true,
            "--help" | "-h" => {
                println!(
                    "Usage: cargo xtask check-compat-tasks [--json]\n\n\
Validates docs/compat/tasks_80.toml and rejects closed PG80 tasks without \
completion metadata and executable evidence."
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown flag: {other}")),
        }
    }
    Ok(CheckCompatTasksOptions { json })
}

pub fn run(opts: CheckCompatTasksOptions) -> Result<(), String> {
    let repo_root = repo_root();
    let path = repo_root.join(TASK_FILE);
    let src = fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let tasks = parse_tasks(&src)?;
    let report = validate_tasks(&tasks);

    if opts.json {
        print_json(&report);
    } else {
        print_human(&report);
    }

    if report.errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} compatibility task ledger error(s)",
            report.errors.len()
        ))
    }
}

fn repo_root() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p
}

fn parse_tasks(src: &str) -> Result<Vec<TaskRecord>, String> {
    let mut tasks = Vec::new();
    let mut current: Option<TaskRecord> = None;
    let mut pending_array: Option<(String, usize, String)> = None;

    for (idx, raw_line) in src.lines().enumerate() {
        let line_no = idx + 1;
        let line = strip_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }

        if let Some((key, start_line, mut value)) = pending_array.take() {
            value.push(' ');
            value.push_str(line);
            if value.trim_end().ends_with(']') {
                let task = current
                    .as_mut()
                    .ok_or_else(|| format!("line {line_no}: array outside task"))?;
                assign_value(task, &key, &value, start_line)?;
            } else {
                pending_array = Some((key, start_line, value));
            }
            continue;
        }

        if line == "[[task]]" {
            if let Some(task) = current.take() {
                tasks.push(task);
            }
            current = Some(TaskRecord {
                line: line_no,
                ..TaskRecord::default()
            });
            continue;
        }

        let Some(task) = current.as_mut() else {
            continue;
        };
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("line {line_no}: expected key = value"));
        };
        let key = key.trim();
        let value = value.trim();
        if value.starts_with('[') && !value.ends_with(']') {
            task.keys.insert(key.to_owned());
            pending_array = Some((key.to_owned(), line_no, value.to_owned()));
        } else {
            assign_value(task, key, value, line_no)?;
        }
    }

    if let Some((key, start_line, _)) = pending_array {
        return Err(format!(
            "line {start_line}: unterminated string array for `{key}`"
        ));
    }

    if let Some(task) = current {
        tasks.push(task);
    }
    Ok(tasks)
}

fn assign_value(
    task: &mut TaskRecord,
    key: &str,
    value: &str,
    line_no: usize,
) -> Result<(), String> {
    task.keys.insert(key.to_owned());
    match key {
        "id" => task.id = Some(parse_string(value, line_no)?),
        "title" => task.title = Some(parse_string(value, line_no)?),
        "status" => task.status = Some(parse_string(value, line_no)?),
        "completed_at" => task.completed_at = Some(parse_string(value, line_no)?),
        "completed_by" => task.completed_by = Some(parse_string(value, line_no)?),
        "completion_commit" => task.completion_commit = Some(parse_string(value, line_no)?),
        "evidence" => task.evidence = parse_string_array(value, line_no)?,
        "evidence_required" => task.evidence_required = parse_string_array(value, line_no)?,
        _ => {}
    }
    Ok(())
}

fn strip_comment(line: &str) -> &str {
    let mut in_string = false;
    for (idx, ch) in line.char_indices() {
        match ch {
            '"' => in_string = !in_string,
            '#' if !in_string => return &line[..idx],
            _ => {}
        }
    }
    line
}

fn parse_string(value: &str, line: usize) -> Result<String, String> {
    let value = value.trim();
    if !value.starts_with('"') || !value.ends_with('"') || value.len() < 2 {
        return Err(format!("line {line}: expected quoted string"));
    }
    Ok(value[1..value.len() - 1].to_owned())
}

fn parse_string_array(value: &str, line: usize) -> Result<Vec<String>, String> {
    let value = value.trim();
    if !value.starts_with('[') || !value.ends_with(']') {
        return Err(format!("line {line}: expected string array"));
    }
    let inner = value[1..value.len() - 1].trim();
    if inner.is_empty() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    for part in inner.split(',') {
        out.push(parse_string(part.trim(), line)?);
    }
    Ok(out)
}

fn validate_tasks(tasks: &[TaskRecord]) -> ValidationReport {
    let mut report = ValidationReport {
        total: tasks.len(),
        ..ValidationReport::default()
    };
    let mut ids = BTreeSet::new();

    for task in tasks {
        let id = task.id.as_deref().unwrap_or("<missing id>");
        for key in REQUIRED_KEYS {
            if !task.keys.contains(*key) {
                report.errors.push(format!(
                    "{id} line {}: missing required key `{key}`",
                    task.line
                ));
            }
        }

        if let Some(id_value) = task.id.as_ref() {
            if !ids.insert(id_value.clone()) {
                report.errors.push(format!("{id}: duplicate task id"));
            }
            if !id_value.starts_with("PG80-") {
                report
                    .errors
                    .push(format!("{id}: id must use the PG80- prefix"));
            }
        }

        let status = task.status.as_deref().unwrap_or("<missing>");
        match status {
            "open" => report.open += 1,
            "in_progress" => report.in_progress += 1,
            "blocked" => report.blocked += 1,
            "done" => {
                report.done += 1;
                validate_done_task(task, id, &mut report.errors);
            }
            "rejected" => report.rejected += 1,
            other => {
                report.errors.push(format!(
                    "{id}: invalid status `{other}`; expected one of {:?}",
                    VALID_STATUSES
                ));
            }
        }

        if task.evidence_required.is_empty() {
            report
                .errors
                .push(format!("{id}: evidence_required must not be empty"));
        }
    }

    report
}

fn validate_done_task(task: &TaskRecord, id: &str, errors: &mut Vec<String>) {
    match task.completed_at.as_deref() {
        Some(date) if is_iso_date(date) => {}
        Some(date) => errors.push(format!("{id}: completed_at `{date}` must be YYYY-MM-DD")),
        None => errors.push(format!("{id}: done task missing completed_at")),
    }
    if task.completed_by.as_deref().unwrap_or("").trim().is_empty() {
        errors.push(format!("{id}: done task missing completed_by"));
    }
    if task
        .completion_commit
        .as_deref()
        .unwrap_or("")
        .trim()
        .is_empty()
    {
        errors.push(format!("{id}: done task missing completion_commit"));
    }
    if task.evidence.is_empty() {
        errors.push(format!("{id}: done task missing evidence"));
    }
    for item in &task.evidence {
        let lower = item.to_ascii_lowercase();
        if lower == "cargo check" || lower == "cargo check -q" {
            errors.push(format!(
                "{id}: evidence `{item}` is insufficient; cargo check alone is not proof"
            ));
        }
    }
    validate_dual_mode_evidence(task, id, errors);
}

fn validate_dual_mode_evidence(task: &TaskRecord, id: &str, errors: &mut Vec<String>) {
    let required_text = task.evidence_required.join(" ").to_ascii_lowercase();
    let requires_embedded = required_text.contains("embedded");
    let requires_pgwire = required_text.contains("pgwire");
    if !(requires_embedded || requires_pgwire) || DUAL_MODE_EXCEPTION_IDS.contains(&id) {
        return;
    }

    let evidence_text = task.evidence.join(" ").to_ascii_lowercase();
    if requires_embedded && !evidence_text.contains("embedded") {
        errors.push(format!(
            "{id}: done task requires embedded evidence but evidence does not mention embedded"
        ));
    }
    if requires_pgwire && !evidence_text.contains("pgwire") {
        errors.push(format!(
            "{id}: done task requires pgwire evidence but evidence does not mention pgwire"
        ));
    }
}

fn is_iso_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(idx, b)| idx == 4 || idx == 7 || b.is_ascii_digit())
}

fn print_human(report: &ValidationReport) {
    println!("== check-compat-tasks ==");
    println!("tasks: {}", report.total);
    println!(
        "status: open={} in_progress={} blocked={} rejected={} done={}",
        report.open, report.in_progress, report.blocked, report.rejected, report.done
    );
    if report.errors.is_empty() {
        println!("result: ok");
    } else {
        println!("result: failed");
        for error in &report.errors {
            println!("- {error}");
        }
    }
}

fn print_json(report: &ValidationReport) {
    println!(
        "{{\"total\":{},\"open\":{},\"in_progress\":{},\"blocked\":{},\"rejected\":{},\"done\":{},\"errors\":[{}]}}",
        report.total,
        report.open,
        report.in_progress,
        report.blocked,
        report.rejected,
        report.done,
        report
            .errors
            .iter()
            .map(|e| format!("\"{}\"", json_escape(e)))
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
    fn rejects_done_task_without_evidence() {
        let tasks = parse_tasks(
            r#"
[[task]]
id = "PG80-999"
title = "Bad"
area = "measurement"
month = 1
status = "done"
kind = "ci"
forbidden_shortcuts = ["done without tests"]
evidence_required = ["cargo xtask check-compat-tasks"]
"#,
        )
        .expect("parse task");

        let report = validate_tasks(&tasks);
        assert!(
            report
                .errors
                .iter()
                .any(|error| error.contains("missing evidence")),
            "{:?}",
            report.errors
        );
    }

    #[test]
    fn accepts_done_task_with_completion_metadata() {
        let tasks = parse_tasks(
            r#"
[[task]]
id = "PG80-999"
title = "Good"
area = "measurement"
month = 1
status = "done"
kind = "ci"
forbidden_shortcuts = ["done without tests"]
evidence_required = ["cargo xtask check-compat-tasks"]
completed_at = "2026-04-24"
completed_by = "unit-test"
completion_commit = "pending"
evidence = ["cargo test -q -p xtask check_compat_tasks", "cargo run -q -p xtask -- check-compat-tasks"]
"#,
        )
        .expect("parse task");

        let report = validate_tasks(&tasks);
        assert_eq!(report.errors, Vec::<String>::new());
        assert_eq!(report.done, 1);
    }

    #[test]
    fn done_task_with_dual_mode_requirement_needs_both_modes() {
        let tasks = parse_tasks(
            r#"
[[task]]
id = "PG80-999"
title = "Needs dual mode"
area = "qa"
month = 1
status = "done"
kind = "real-implementation"
forbidden_shortcuts = ["embedded-only feature"]
evidence_required = ["embedded test", "pgwire test"]
completed_at = "2026-04-24"
completed_by = "unit-test"
completion_commit = "pending"
evidence = ["cargo test -q -p aiondb-test-kit embedded_only"]
"#,
        )
        .expect("parse task");

        let report = validate_tasks(&tasks);
        assert!(
            report
                .errors
                .iter()
                .any(|error| error.contains("requires pgwire evidence")),
            "{:?}",
            report.errors
        );
    }

    #[test]
    fn done_task_with_dual_mode_requirement_accepts_both_modes() {
        let tasks = parse_tasks(
            r#"
[[task]]
id = "PG80-999"
title = "Has dual mode"
area = "qa"
month = 1
status = "done"
kind = "real-implementation"
forbidden_shortcuts = ["embedded-only feature"]
evidence_required = ["embedded test", "pgwire test"]
completed_at = "2026-04-24"
completed_by = "unit-test"
completion_commit = "pending"
evidence = ["embedded scenario test", "pgwire scenario test"]
"#,
        )
        .expect("parse task");

        let report = validate_tasks(&tasks);
        assert_eq!(report.errors, Vec::<String>::new());
    }
}
