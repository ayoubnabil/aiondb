//! `cargo xtask compat-burndown` - generate weekly PG80 burn-down reports.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

const TASK_FILE: &str = "docs/compat/tasks_80.toml";
const MATRIX_FILE: &str = "crates/aiondb-pg-compat/src/compat_tag_matrix.rs";
const DEFAULT_JSON: &str = "target/compat/weekly-burndown.json";
const DEFAULT_MARKDOWN: &str = "target/compat/weekly-burndown.md";

#[derive(Clone, Debug)]
pub struct CompatBurndownOptions {
    json_out: PathBuf,
    markdown_out: PathBuf,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Task {
    id: String,
    title: String,
    area: String,
    month: u32,
    status: String,
    kind: String,
}

#[derive(Debug, Default)]
struct Score {
    total: usize,
    raw_matched: usize,
    real_matched: usize,
    intentional_noop_tags: Vec<String>,
}

#[derive(Debug)]
struct Burndown {
    total_tasks: usize,
    done: usize,
    open: usize,
    in_progress: usize,
    blocked: usize,
    rejected: usize,
    by_area_open: BTreeMap<String, usize>,
    score: Score,
    top_blockers: Vec<Task>,
}

pub fn parse_args(args: &[String]) -> Result<CompatBurndownOptions, String> {
    let mut json_out = PathBuf::from(DEFAULT_JSON);
    let mut markdown_out = PathBuf::from(DEFAULT_MARKDOWN);
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json-out" => {
                i += 1;
                json_out = PathBuf::from(
                    args.get(i)
                        .ok_or_else(|| "--json-out requires a value".to_owned())?,
                );
            }
            "--markdown-out" => {
                i += 1;
                markdown_out = PathBuf::from(
                    args.get(i)
                        .ok_or_else(|| "--markdown-out requires a value".to_owned())?,
                );
            }
            "--help" | "-h" => {
                println!(
                    "Usage: cargo xtask compat-burndown [--json-out <PATH>] [--markdown-out <PATH>]\n\n\
Generates weekly PG80 burn-down JSON and Markdown reports."
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown flag: {other}")),
        }
        i += 1;
    }
    Ok(CompatBurndownOptions {
        json_out,
        markdown_out,
    })
}

pub fn run(opts: CompatBurndownOptions) -> Result<(), String> {
    let root = repo_root();
    let tasks_src = fs::read_to_string(root.join(TASK_FILE))
        .map_err(|error| format!("reading {TASK_FILE}: {error}"))?;
    let matrix_src = fs::read_to_string(root.join(MATRIX_FILE))
        .map_err(|error| format!("reading {MATRIX_FILE}: {error}"))?;
    let tasks = parse_tasks(&tasks_src)?;
    let score = parse_score(&matrix_src)?;
    let report = build_burndown(tasks, score);

    let json_path = absolutize(&root, &opts.json_out);
    let markdown_path = absolutize(&root, &opts.markdown_out);
    write_file(&json_path, &render_json(&report))?;
    write_file(&markdown_path, &render_markdown(&report))?;
    println!("== compat-burndown ==");
    println!("json: {}", json_path.display());
    println!("markdown: {}", markdown_path.display());
    println!(
        "tasks: total={} done={} open={} blocked={}",
        report.total_tasks, report.done, report.open, report.blocked
    );
    println!("top blockers: {}", report.top_blockers.len());
    Ok(())
}

fn repo_root() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p
}

fn absolutize(root: &std::path::Path, path: &std::path::Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn write_file(path: &std::path::Path, contents: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("creating {}: {error}", parent.display()))?;
    }
    fs::write(path, contents).map_err(|error| format!("writing {}: {error}", path.display()))
}

fn parse_tasks(src: &str) -> Result<Vec<Task>, String> {
    let mut tasks = Vec::new();
    let mut current = Task::default();
    let mut in_task = false;
    for line in src.lines() {
        let trimmed = line.trim();
        if trimmed == "[[task]]" {
            if in_task {
                tasks.push(current);
                current = Task::default();
            }
            in_task = true;
            continue;
        }
        if !in_task {
            continue;
        }
        if let Some(value) = parse_string_field(trimmed, "id") {
            current.id = value;
        } else if let Some(value) = parse_string_field(trimmed, "title") {
            current.title = value;
        } else if let Some(value) = parse_string_field(trimmed, "area") {
            current.area = value;
        } else if let Some(value) = parse_string_field(trimmed, "status") {
            current.status = value;
        } else if let Some(value) = parse_string_field(trimmed, "kind") {
            current.kind = value;
        } else if let Some(value) = parse_u32_field(trimmed, "month") {
            current.month = value;
        }
    }
    if in_task {
        tasks.push(current);
    }
    if tasks.is_empty() {
        return Err("no tasks found".to_owned());
    }
    Ok(tasks)
}

fn parse_string_field(line: &str, key: &str) -> Option<String> {
    let rest = line.strip_prefix(&format!("{key} = \""))?;
    let end = rest.find('"')?;
    Some(rest[..end].to_owned())
}

fn parse_u32_field(line: &str, key: &str) -> Option<u32> {
    let rest = line.strip_prefix(&format!("{key} = "))?;
    rest.trim().parse::<u32>().ok()
}

fn parse_rust_tag_line(line: &str) -> Option<String> {
    let rest = line.strip_prefix("tag: \"")?;
    let end = rest.find('"')?;
    Some(rest[..end].to_owned())
}

fn parse_score(src: &str) -> Result<Score, String> {
    let mut score = Score::default();
    let mut current_tag: Option<String> = None;
    for line in src.lines() {
        let trimmed = line.trim();
        if let Some(tag) = parse_rust_tag_line(trimmed) {
            current_tag = Some(tag);
            continue;
        }
        if trimmed.contains("behavior: CompatTagBehavior::ImplementedReal") {
            score.total += 1;
            score.raw_matched += 1;
            score.real_matched += 1;
            current_tag = None;
        } else if trimmed.contains("behavior: CompatTagBehavior::IntentionalNoop") {
            score.total += 1;
            score.raw_matched += 1;
            let Some(tag) = current_tag.take() else {
                return Err("IntentionalNoop entry without tag".to_owned());
            };
            score.intentional_noop_tags.push(tag);
        } else if trimmed.contains("behavior: CompatTagBehavior::ExplicitNotSupported") {
            score.total += 1;
            current_tag = None;
        }
    }
    score.intentional_noop_tags.sort();
    Ok(score)
}

fn build_burndown(tasks: Vec<Task>, score: Score) -> Burndown {
    let mut report = Burndown {
        total_tasks: tasks.len(),
        done: 0,
        open: 0,
        in_progress: 0,
        blocked: 0,
        rejected: 0,
        by_area_open: BTreeMap::new(),
        score,
        top_blockers: Vec::new(),
    };
    for task in &tasks {
        match task.status.as_str() {
            "done" => report.done += 1,
            "open" => {
                report.open += 1;
                *report.by_area_open.entry(task.area.clone()).or_default() += 1;
            }
            "in_progress" => report.in_progress += 1,
            "blocked" => {
                report.blocked += 1;
                *report.by_area_open.entry(task.area.clone()).or_default() += 1;
            }
            "rejected" => report.rejected += 1,
            _ => {}
        }
    }
    let mut blockers: Vec<Task> = tasks
        .into_iter()
        .filter(|task| matches!(task.status.as_str(), "open" | "blocked" | "in_progress"))
        .collect();
    blockers.sort_by(|a, b| {
        a.month
            .cmp(&b.month)
            .then_with(|| status_rank(&a.status).cmp(&status_rank(&b.status)))
            .then_with(|| a.id.cmp(&b.id))
    });
    blockers.truncate(20);
    report.top_blockers = blockers;
    report
}

fn status_rank(status: &str) -> u8 {
    match status {
        "blocked" => 0,
        "in_progress" => 1,
        "open" => 2,
        _ => 3,
    }
}

fn pct(part: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        part as f64 / total as f64 * 100.0
    }
}

fn render_json(report: &Burndown) -> String {
    format!(
        "{{\n  \"tasks\": {{\"total\": {}, \"done\": {}, \"open\": {}, \"in_progress\": {}, \"blocked\": {}, \"rejected\": {}}},\n  \"score\": {{\"total\": {}, \"raw_matched\": {}, \"raw_pct\": {:.2}, \"real_matched\": {}, \"real_pct\": {:.2}, \"intentional_noop_tags\": [{}]}},\n  \"open_by_area\": {},\n  \"top_blockers\": [{}]\n}}\n",
        report.total_tasks,
        report.done,
        report.open,
        report.in_progress,
        report.blocked,
        report.rejected,
        report.score.total,
        report.score.raw_matched,
        pct(report.score.raw_matched, report.score.total),
        report.score.real_matched,
        pct(report.score.real_matched, report.score.total),
        report
            .score
            .intentional_noop_tags
            .iter()
            .map(|tag| format!("\"{}\"", json_escape(tag)))
            .collect::<Vec<_>>()
            .join(", "),
        json_map(&report.by_area_open),
        report
            .top_blockers
            .iter()
            .map(|task| format!(
                "{{\"id\":\"{}\",\"title\":\"{}\",\"area\":\"{}\",\"month\":{},\"status\":\"{}\",\"kind\":\"{}\"}}",
                json_escape(&task.id),
                json_escape(&task.title),
                json_escape(&task.area),
                task.month,
                json_escape(&task.status),
                json_escape(&task.kind)
            ))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn json_map(map: &BTreeMap<String, usize>) -> String {
    format!(
        "{{{}}}",
        map.iter()
            .map(|(key, value)| format!("\"{}\": {}", json_escape(key), value))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn render_markdown(report: &Burndown) -> String {
    let mut out = String::new();
    out.push_str("# PG80 Weekly Compatibility Burn-down\n\n");
    out.push_str(&format!(
        "- Tasks: **{} done** / {} total; {} open, {} in progress, {} blocked, {} rejected.\n",
        report.done,
        report.total_tasks,
        report.open,
        report.in_progress,
        report.blocked,
        report.rejected
    ));
    out.push_str(&format!(
        "- Raw matrix score: **{}/{} ({:.1}%)**.\n",
        report.score.raw_matched,
        report.score.total,
        pct(report.score.raw_matched, report.score.total)
    ));
    out.push_str(&format!(
        "- Real matrix score: **{}/{} ({:.1}%)** after excluding no-op tags: {}.\n\n",
        report.score.real_matched,
        report.score.total,
        pct(report.score.real_matched, report.score.total),
        if report.score.intentional_noop_tags.is_empty() {
            "<none>".to_owned()
        } else {
            report.score.intentional_noop_tags.join(", ")
        }
    ));
    out.push_str("## Open By Area\n\n");
    for (area, count) in &report.by_area_open {
        out.push_str(&format!("- `{area}`: {count}\n"));
    }
    out.push_str("\n## Top Blockers\n\n");
    out.push_str("| ID | Month | Area | Status | Kind | Title |\n");
    out.push_str("|---|---:|---|---|---|---|\n");
    for task in &report.top_blockers {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            task.id,
            task.month,
            task.area,
            task.status,
            task.kind,
            task.title.replace('|', "\\|")
        ));
    }
    out
}

fn json_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_burndown_with_top_blockers() {
        let tasks = vec![
            Task {
                id: "PG80-001".to_owned(),
                title: "Open".to_owned(),
                area: "measurement".to_owned(),
                month: 1,
                status: "open".to_owned(),
                kind: "ci".to_owned(),
            },
            Task {
                id: "PG80-002".to_owned(),
                title: "Done".to_owned(),
                area: "measurement".to_owned(),
                month: 1,
                status: "done".to_owned(),
                kind: "ci".to_owned(),
            },
        ];
        let report = build_burndown(
            tasks,
            Score {
                total: 3,
                raw_matched: 3,
                real_matched: 3,
                intentional_noop_tags: vec![],
            },
        );
        assert_eq!(report.done, 1);
        assert_eq!(report.open, 1);
        assert_eq!(report.top_blockers[0].id, "PG80-001");
        assert!(render_markdown(&report).contains("Top Blockers"));
        assert!(render_json(&report).contains("\"real_matched\": 3"));
    }
}
