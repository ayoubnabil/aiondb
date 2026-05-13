//! `cargo xtask compat-baseline` - generate the PG80 strict baseline report.

use serde::Serialize;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const DEFAULT_OUTPUT: &str = "target/compat/postgres-compat-baseline.json";
const DEFAULT_ECOSYSTEM_REPORT: &str = "target/compat/ecosystem-compat-baseline.json";
const DEFAULT_PG_REGRESS_PROGRESS_DIR: &str = "target/compat/pg-regress-baseline";
const DEFAULT_MISMATCH_JSONL: &str = "target/compat/pg-regress-baseline-mismatches.jsonl";

#[derive(Clone, Debug)]
pub struct CompatBaselineOptions {
    output: PathBuf,
    pg_regress_file: Option<String>,
    skip_ecosystem: bool,
}

#[derive(Debug, Serialize)]
struct CommandRecord {
    name: String,
    command: Vec<String>,
    env: Vec<(String, String)>,
    exit_code: Option<i32>,
    success: bool,
    duration_ms: u128,
    stdout_path: String,
    stderr_path: String,
}

#[derive(Debug, Serialize)]
struct PgRegressSuite {
    name: String,
    matched: usize,
    total: usize,
}

#[derive(Debug, Serialize)]
struct PgRegressSkip {
    name: String,
    reason: String,
}

#[derive(Debug, Serialize, Default)]
struct PgRegressSummary {
    matched: usize,
    total: usize,
    suites: Vec<PgRegressSuite>,
    skips: Vec<PgRegressSkip>,
}

pub fn parse_args(args: &[String]) -> Result<CompatBaselineOptions, String> {
    let mut output = PathBuf::from(DEFAULT_OUTPUT);
    let mut pg_regress_file = None;
    let mut skip_ecosystem = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--output" => {
                i += 1;
                output = PathBuf::from(
                    args.get(i)
                        .ok_or_else(|| "--output requires a value".to_owned())?,
                );
            }
            "--pg-regress-file" => {
                i += 1;
                pg_regress_file = Some(
                    args.get(i)
                        .ok_or_else(|| "--pg-regress-file requires a value".to_owned())?
                        .clone(),
                );
            }
            "--skip-ecosystem" => {
                skip_ecosystem = true;
            }
            "--help" | "-h" => {
                println!(
                    "Usage: cargo xtask compat-baseline [--output <PATH>] [--pg-regress-file <NAME>] [--skip-ecosystem]\n\n\
Generates target/compat/postgres-compat-baseline.json with commit, runner config, scores, skips and timeouts."
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown flag: {other}")),
        }
        i += 1;
    }
    Ok(CompatBaselineOptions {
        output,
        pg_regress_file,
        skip_ecosystem,
    })
}

pub fn run(opts: CompatBaselineOptions) -> Result<(), String> {
    let root = repo_root();
    let output_path = absolutize(&root, &opts.output);
    let out_dir = output_path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", output_path.display()))?
        .to_path_buf();
    fs::create_dir_all(&out_dir)
        .map_err(|error| format!("creating {}: {error}", out_dir.display()))?;

    let commit_sha = command_text(&root, "git", &["rev-parse", "HEAD"])?;
    let status_short = command_text(&root, "git", &["status", "--short"])?;

    let mut commands = Vec::new();

    let debt = run_json_xtask(&root, &out_dir, "compat-debt-report", &["--json"], &[])?;
    commands.push(debt.0);

    let real_score = run_json_xtask(&root, &out_dir, "compat-real-score", &["--json"], &[])?;
    commands.push(real_score.0);

    let ecosystem_report_path = absolutize(&root, Path::new(DEFAULT_ECOSYSTEM_REPORT));
    let ecosystem = if opts.skip_ecosystem {
        json!({"skipped_by_baseline_tool": true})
    } else {
        let args = [
            "run",
            "-q",
            "-p",
            "xtask",
            "--",
            "ecosystem-compat",
            "--strict",
            "--report",
            ecosystem_report_path
                .to_str()
                .ok_or_else(|| "ecosystem report path is not UTF-8".to_owned())?,
        ];
        let record = run_command(&root, &out_dir, "ecosystem-compat", "cargo", &args, &[])?;
        commands.push(record);
        read_json_or_note(&ecosystem_report_path)
    };

    let progress_dir = absolutize(&root, Path::new(DEFAULT_PG_REGRESS_PROGRESS_DIR));
    let mismatch_jsonl = absolutize(&root, Path::new(DEFAULT_MISMATCH_JSONL));
    let mut pg_env = vec![
        (
            "PG_REGRESS_PROGRESS_DIR".to_owned(),
            progress_dir.display().to_string(),
        ),
        (
            "PG_REGRESS_MISMATCH_JSONL".to_owned(),
            mismatch_jsonl.display().to_string(),
        ),
    ];
    if let Some(file) = &opts.pg_regress_file {
        pg_env.push(("PG_REGRESS_FILE".to_owned(), file.clone()));
    }
    let pg_args = [
        "run",
        "-q",
        "--manifest-path",
        ".pg-regress/Cargo.toml",
        "--bin",
        "pg-regress",
    ];
    let pg_record = run_command(&root, &out_dir, "pg-regress", "cargo", &pg_args, &pg_env)?;
    let pg_stderr = fs::read_to_string(&pg_record.stderr_path).unwrap_or_default();
    let pg_stdout = fs::read_to_string(&pg_record.stdout_path).unwrap_or_default();
    let pg_summary = parse_pg_regress_summary(&format!("{pg_stdout}\n{pg_stderr}"));
    commands.push(pg_record);

    let report = json!({
        "generated_at_unix_ms": unix_time_millis(),
        "commit_sha": commit_sha.trim(),
        "dirty_status_short": status_short.lines().collect::<Vec<_>>(),
        "policy": {
            "strict_score": "matched / total only",
            "skips_count_as_failures": true,
            "intentional_noops_excluded_from_real_score": true,
            "local_pg_regress_probes_excluded": true,
        },
        "runner_config": {
            "pg_regress": {
                "command": "cargo run -q --manifest-path .pg-regress/Cargo.toml --bin pg-regress",
                "file_filter": opts.pg_regress_file,
                "progress_dir": progress_dir.display().to_string(),
                "mismatch_jsonl": mismatch_jsonl.display().to_string(),
                "statement_timeout_ms": 10_000,
                "include_local_probes": false,
                "compat_intercepts": false,
                "similar_matching": false
            },
            "ecosystem": {
                "command": "cargo run -q -p xtask -- ecosystem-compat --strict --report target/compat/ecosystem-compat-baseline.json",
                "strict": !opts.skip_ecosystem
            }
        },
        "pg_regress": pg_summary,
        "ecosystem": ecosystem,
        "compat_debt_report": debt.1,
        "compat_real_score": real_score.1,
        "commands": commands,
    });

    write_json(&output_path, &report)?;
    println!("== compat-baseline ==");
    println!("report: {}", output_path.display());
    println!("commit: {}", commit_sha.trim());
    println!(
        "pg-regress: {}/{} matched, {} skipped",
        report["pg_regress"]["matched"].as_u64().unwrap_or(0),
        report["pg_regress"]["total"].as_u64().unwrap_or(0),
        report["pg_regress"]["skips"].as_array().map_or(0, Vec::len)
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

fn command_text(root: &Path, program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|error| format!("running {program}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "{program} {} failed with status {:?}: {}",
            args.join(" "),
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn run_json_xtask(
    root: &Path,
    out_dir: &Path,
    name: &str,
    xtask_args: &[&str],
    envs: &[(String, String)],
) -> Result<(CommandRecord, Value), String> {
    let mut args = vec!["run", "-q", "-p", "xtask", "--", name];
    args.extend_from_slice(xtask_args);
    let record = run_command(root, out_dir, name, "cargo", &args, envs)?;
    let stdout = fs::read_to_string(&record.stdout_path).unwrap_or_default();
    let parsed = serde_json::from_str(&stdout).unwrap_or_else(|error| {
        json!({
            "parse_error": error.to_string(),
            "stdout_path": record.stdout_path,
            "stderr_path": record.stderr_path
        })
    });
    Ok((record, parsed))
}

fn run_command(
    root: &Path,
    out_dir: &Path,
    name: &str,
    program: &str,
    args: &[&str],
    envs: &[(String, String)],
) -> Result<CommandRecord, String> {
    let started = Instant::now();
    let output = Command::new(program)
        .args(args)
        .envs(envs.iter().map(|(key, value)| (key, value)))
        .current_dir(root)
        .output()
        .map_err(|error| format!("running {name}: {error}"))?;
    let duration_ms = started.elapsed().as_millis();
    let stdout_path = out_dir.join(format!("{name}.stdout.txt"));
    let stderr_path = out_dir.join(format!("{name}.stderr.txt"));
    fs::write(&stdout_path, &output.stdout)
        .map_err(|error| format!("writing {}: {error}", stdout_path.display()))?;
    fs::write(&stderr_path, &output.stderr)
        .map_err(|error| format!("writing {}: {error}", stderr_path.display()))?;
    Ok(CommandRecord {
        name: name.to_owned(),
        command: std::iter::once(program.to_owned())
            .chain(args.iter().map(|arg| (*arg).to_owned()))
            .collect(),
        env: envs.to_vec(),
        exit_code: output.status.code(),
        success: output.status.success(),
        duration_ms,
        stdout_path: stdout_path.display().to_string(),
        stderr_path: stderr_path.display().to_string(),
    })
}

fn read_json_or_note(path: &Path) -> Value {
    match fs::read_to_string(path) {
        Ok(src) => serde_json::from_str(&src).unwrap_or_else(
            |error| json!({"parse_error": error.to_string(), "path": path.display().to_string()}),
        ),
        Err(error) => {
            json!({"missing_report": path.display().to_string(), "error": error.to_string()})
        }
    }
}

fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    let json = serde_json::to_string_pretty(value)
        .map_err(|error| format!("serializing baseline report: {error}"))?;
    fs::write(path, format!("{json}\n"))
        .map_err(|error| format!("writing {}: {error}", path.display()))
}

fn parse_pg_regress_summary(output: &str) -> PgRegressSummary {
    let mut summary = PgRegressSummary::default();
    for line in output.lines() {
        if let Some(rest) = line.strip_prefix("RESULT|") {
            let parts: Vec<&str> = rest.split('|').collect();
            if parts.len() >= 3 {
                let matched = parts[1].parse::<usize>().unwrap_or(0);
                let total = parts[2].parse::<usize>().unwrap_or(0);
                summary.matched += matched;
                summary.total += total;
                summary.suites.push(PgRegressSuite {
                    name: parts[0].to_owned(),
                    matched,
                    total,
                });
            }
        } else if let Some(rest) = line.strip_prefix("SKIP|") {
            let mut parts = rest.splitn(2, '|');
            let name = parts.next().unwrap_or("").to_owned();
            let reason = parts.next().unwrap_or("").to_owned();
            summary.skips.push(PgRegressSkip { name, reason });
        }
    }
    summary
}

fn unix_time_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pg_regress_result_and_skip_lines() {
        let summary = parse_pg_regress_summary(
            "RESULT|errors|63|87\nSKIP|lock|hard excluded\nRESULT|create_table|2|10\n",
        );
        assert_eq!(summary.matched, 65);
        assert_eq!(summary.total, 97);
        assert_eq!(summary.suites.len(), 2);
        assert_eq!(summary.skips[0].name, "lock");
    }
}
