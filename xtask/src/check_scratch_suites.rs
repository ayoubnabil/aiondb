//! `cargo xtask check-scratch-suites` - keep local pg-regress probes out of score.

use std::fs;
use std::path::PathBuf;

const STRICT_RUNNER: &str = ".pg-regress/src/pg_regress_strict.rs";
const SHELL_RUNNER: &str = ".pg-regress/run_all_safe.sh";

#[derive(Clone, Debug)]
pub struct CheckScratchSuitesOptions {
    pub json: bool,
}

#[derive(Debug, Default)]
struct Report {
    local_probe_count: usize,
    violations: Vec<String>,
}

pub fn parse_args(args: &[String]) -> Result<CheckScratchSuitesOptions, String> {
    let mut json = false;
    for a in args {
        match a.as_str() {
            "--json" => json = true,
            "--help" | "-h" => {
                println!(
                    "Usage: cargo xtask check-scratch-suites [--json]\n\n\
Verifies that local pg-regress probes are excluded from official scoring by \
default and reported separately."
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown flag: {other}")),
        }
    }
    Ok(CheckScratchSuitesOptions { json })
}

pub fn run(opts: CheckScratchSuitesOptions) -> Result<(), String> {
    let root = repo_root();
    let strict_path = root.join(STRICT_RUNNER);
    let shell_path = root.join(SHELL_RUNNER);
    let strict_src = fs::read_to_string(&strict_path)
        .map_err(|error| format!("reading {}: {error}", strict_path.display()))?;
    let shell_src = fs::read_to_string(&shell_path)
        .map_err(|error| format!("reading {}: {error}", shell_path.display()))?;

    let mut report = Report {
        local_probe_count: parse_rust_probe_list(&strict_src).len(),
        violations: Vec::new(),
    };
    validate_runner_source(&strict_src, &shell_src, &mut report);

    if opts.json {
        print_json(&report);
    } else {
        print_human(&report);
    }

    if report.violations.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} scratch-suite hygiene violation(s)",
            report.violations.len()
        ))
    }
}

fn repo_root() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p
}

fn validate_runner_source(strict_src: &str, shell_src: &str, report: &mut Report) {
    let probes = parse_rust_probe_list(strict_src);
    if probes.is_empty() {
        report
            .violations
            .push("LOCAL_PROBE_SUITES must not be empty".to_owned());
    }
    for required in [
        "probe_join_lateral_values",
        "zz_oom_repro",
        "zz_with_block_239_266",
    ] {
        if !probes.iter().any(|probe| probe == required) {
            report
                .violations
                .push(format!("LOCAL_PROBE_SUITES missing `{required}`"));
        }
    }
    for phrase in [
        "PG_REGRESS_INCLUDE_LOCAL_PROBES",
        "if !include_local_probes",
        "excluded from official score",
        "local_probe_exclusions",
    ] {
        if !strict_src.contains(phrase) {
            report
                .violations
                .push(format!("{STRICT_RUNNER}: missing `{phrase}`"));
        }
    }
    for phrase in [
        "PG_REGRESS_INCLUDE_LOCAL_PROBES",
        "Mode suites: official (local probes exclus)",
        "LOCAL_PROBE_SUITES",
    ] {
        if !shell_src.contains(phrase) {
            report
                .violations
                .push(format!("{SHELL_RUNNER}: missing `{phrase}`"));
        }
    }
}

fn parse_rust_probe_list(src: &str) -> Vec<String> {
    let Some(start) = src.find("const LOCAL_PROBE_SUITES") else {
        return Vec::new();
    };
    let rest = &src[start..];
    let Some(open) = rest.find("&[") else {
        return Vec::new();
    };
    let rest = &rest[open + 2..];
    let Some(close) = rest.find("];") else {
        return Vec::new();
    };
    rest[..close]
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            let value = trimmed.strip_prefix('"')?;
            let end = value.find('"')?;
            Some(value[..end].to_owned())
        })
        .collect()
}

fn print_human(report: &Report) {
    println!("== check-scratch-suites ==");
    println!("local probe suites: {}", report.local_probe_count);
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
        "{{\"local_probe_suites\":{},\"violations\":[{}]}}",
        report.local_probe_count,
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
    fn parses_rust_probe_list() {
        let src = r#"
const LOCAL_PROBE_SUITES: &[&str] = &[
    "with_probe",
    "zz_oom_repro",
];
"#;
        assert_eq!(
            parse_rust_probe_list(src),
            vec!["with_probe".to_owned(), "zz_oom_repro".to_owned()]
        );
    }
}
