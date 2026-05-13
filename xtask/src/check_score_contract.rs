//! `cargo xtask check-score-contract` - enforce the PG80 score contract.

use std::fs;
use std::path::PathBuf;

const CONTRACT_PATH: &str = "docs/compat/score_contract.md";
const REQUIRED_CONTRACT_PHRASES: &[&str] = &[
    "matched / total",
    "total = 0 is never success",
    "skipped suites",
    "timed out files or suites",
    "panics or crashes",
    "CommandNoOp",
    "Only `matched` and `total` participate in the strict score",
    "query table rows are compared as a multiset unless the SQL contains",
    "error message wording is not part of the score",
    "SQLSTATE must match",
];

#[derive(Clone, Debug)]
pub struct CheckScoreContractOptions {
    pub json: bool,
}

#[derive(Debug, Default)]
struct Report {
    violations: Vec<String>,
}

pub fn parse_args(args: &[String]) -> Result<CheckScoreContractOptions, String> {
    let mut json = false;
    for a in args {
        match a.as_str() {
            "--json" => json = true,
            "--help" | "-h" => {
                println!(
                    "Usage: cargo xtask check-score-contract [--json]\n\n\
Validates docs/compat/score_contract.md and the strict matched/total score \
invariants used by compatibility measurement."
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown flag: {other}")),
        }
    }
    Ok(CheckScoreContractOptions { json })
}

pub fn run(opts: CheckScoreContractOptions) -> Result<(), String> {
    let root = repo_root();
    let mut report = Report::default();

    let contract_path = root.join(CONTRACT_PATH);
    let contract = match fs::read_to_string(&contract_path) {
        Ok(contract) => contract,
        Err(error) => {
            report
                .violations
                .push(format!("reading {}: {error}", contract_path.display()));
            String::new()
        }
    };
    for phrase in REQUIRED_CONTRACT_PHRASES {
        if !contract.contains(phrase) {
            report.violations.push(format!(
                "{CONTRACT_PATH}: missing required phrase `{phrase}`"
            ));
        }
    }

    validate_runner_source(&root, &mut report);

    if opts.json {
        print_json(&report);
    } else {
        print_human(&report);
    }

    if report.violations.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} score contract violation(s)",
            report.violations.len()
        ))
    }
}

fn repo_root() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p
}

fn validate_runner_source(root: &std::path::Path, report: &mut Report) {
    let pg_runner = root.join(".pg-regress/src/pg_regress_strict.rs");
    let Ok(src) = fs::read_to_string(&pg_runner) else {
        report.violations.push(format!(
            "{}: missing pg-regress runner",
            pg_runner.display()
        ));
        return;
    };
    let required = [
        "total_matched as f64 / total_stmts as f64",
        "*total > 0",
        "if total_stmts == 0",
        "SKIP|{}|{}",
        "match_contract=semantic_result_sqlstate",
        "semantic_error_outputs_match",
        "sql_requires_deterministic_row_order",
    ];
    for phrase in required {
        if !src.contains(phrase) {
            report.violations.push(format!(
                ".pg-regress/src/pg_regress_strict.rs: missing score-contract guard `{phrase}`"
            ));
        }
    }
}

fn print_human(report: &Report) {
    println!("== check-score-contract ==");
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
        "{{\"violations\":[{}]}}",
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
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct ScoreInput {
        matched: usize,
        total: usize,
        skipped: usize,
        timeouts: usize,
        panics: usize,
    }

    #[derive(Debug, Clone, Copy, PartialEq)]
    enum ScoreOutcome {
        Valid { rate: f64 },
        InvalidZeroTotal,
    }

    fn strict_score(input: ScoreInput) -> ScoreOutcome {
        if input.total == 0 {
            return ScoreOutcome::InvalidZeroTotal;
        }
        ScoreOutcome::Valid {
            rate: input.matched as f64 / input.total as f64,
        }
    }

    #[test]
    fn strict_score_uses_matched_divided_by_total() {
        let outcome = strict_score(ScoreInput {
            matched: 7,
            total: 10,
            skipped: 99,
            timeouts: 99,
            panics: 99,
        });
        assert_eq!(outcome, ScoreOutcome::Valid { rate: 0.7 });
    }

    #[test]
    fn strict_score_rejects_zero_total() {
        let outcome = strict_score(ScoreInput {
            matched: 0,
            total: 0,
            skipped: 1,
            timeouts: 0,
            panics: 0,
        });
        assert_eq!(outcome, ScoreOutcome::InvalidZeroTotal);
    }

    #[test]
    fn skipped_timeout_and_panic_do_not_increase_rate() {
        let clean = strict_score(ScoreInput {
            matched: 1,
            total: 4,
            skipped: 0,
            timeouts: 0,
            panics: 0,
        });
        let noisy = strict_score(ScoreInput {
            matched: 1,
            total: 4,
            skipped: 10,
            timeouts: 10,
            panics: 10,
        });
        assert_eq!(clean, noisy);
    }
}
