use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

/// A single CI step with its name and the command to execute.
struct Step {
    name: &'static str,
    cmd: &'static str,
    args: &'static [&'static str],
    env: &'static [(&'static str, &'static str)],
}

const STEPS: &[Step] = &[
    Step {
        name: "fmt",
        cmd: "cargo",
        args: &["fmt", "--all", "--", "--check"],
        env: &[],
    },
    Step {
        name: "clippy",
        cmd: "cargo",
        args: &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
        env: &[],
    },
    Step {
        name: "test-workspace",
        cmd: "cargo",
        args: &[
            "test",
            "--workspace",
            "--tests",
            "--exclude",
            "aiondb-engine",
        ],
        env: &[],
    },
    Step {
        name: "test-engine",
        cmd: "cargo",
        args: &["test", "-p", "aiondb-engine", "--tests"],
        env: &[("AIONDB_PERF_BUDGET_MULTIPLIER", "2")],
    },
    Step {
        name: "doc",
        cmd: "cargo",
        args: &["doc", "--workspace", "--no-deps"],
        env: &[("RUSTDOCFLAGS", "-D warnings")],
    },
    Step {
        name: "workspace-lints",
        cmd: "cargo",
        args: &["run", "-q", "-p", "xtask", "--", "workspace-lints"],
        env: &[],
    },
    Step {
        name: "file-limits",
        cmd: "cargo",
        args: &["run", "-q", "-p", "xtask", "--", "file-limits"],
        env: &[],
    },
    Step {
        name: "runtime-crash-lints",
        cmd: "cargo",
        args: &["run", "-q", "-p", "xtask", "--", "runtime-crash-lints"],
        env: &[],
    },
    Step {
        name: "storage-upgrade-matrix",
        cmd: "cargo",
        args: &["run", "-q", "-p", "xtask", "--", "storage-upgrade-matrix"],
        env: &[],
    },
];

/// Parsed CLI options for `test-matrix`.
pub(crate) struct TestMatrixOptions {
    step_filter: Option<String>,
    continue_on_error: bool,
    perf_budget_multiplier: Option<String>,
}

pub(crate) fn parse_args(args: &[String]) -> Result<TestMatrixOptions, String> {
    let mut step_filter: Option<String> = None;
    let mut continue_on_error = false;
    let mut perf_budget_multiplier: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--step" => {
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| "--step requires a value".to_owned())?;
                let valid_names: Vec<&str> = STEPS.iter().map(|s| s.name).collect();
                if !valid_names.contains(&value.as_str()) {
                    return Err(format!(
                        "unknown step '{value}'. Valid steps: {}",
                        valid_names.join(", ")
                    ));
                }
                step_filter = Some(value.clone());
            }
            "--continue-on-error" => {
                continue_on_error = true;
            }
            "--perf-budget-multiplier" => {
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| "--perf-budget-multiplier requires a value".to_owned())?;
                let parsed = value
                    .parse::<f64>()
                    .map_err(|_| "--perf-budget-multiplier must be a number".to_owned())?;
                if parsed <= 0.0 {
                    return Err("--perf-budget-multiplier must be > 0".to_owned());
                }
                perf_budget_multiplier = Some(value.clone());
            }
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            other => {
                return Err(format!(
                    "unknown flag for test-matrix: {other}\n\nRun `cargo xtask test-matrix --help` for usage."
                ));
            }
        }
        i += 1;
    }

    Ok(TestMatrixOptions {
        step_filter,
        continue_on_error,
        perf_budget_multiplier,
    })
}

pub(crate) fn print_usage() {
    println!(
        "\
Usage: cargo xtask test-matrix [OPTIONS]

Run the full local CI matrix.

Options:
  --step <NAME>        Run only the named step (fmt, clippy, test-workspace, test-engine, doc, workspace-lints, file-limits, runtime-crash-lints, storage-upgrade-matrix)
  --continue-on-error  Run all steps even if one fails
  --perf-budget-multiplier <X>
                       Override AIONDB_PERF_BUDGET_MULTIPLIER for the test-engine step (must be > 0)
  -h, --help           Print this help message"
    );
}

pub(crate) fn run(opts: TestMatrixOptions) -> ExitCode {
    let steps: Vec<&Step> = match &opts.step_filter {
        Some(name) => STEPS.iter().filter(|s| s.name == name.as_str()).collect(),
        None => STEPS.iter().collect(),
    };

    let total = steps.len();

    println!("=== AionDB test-matrix ===");

    let mut results: Vec<(&str, bool, Duration)> = Vec::new();
    let mut any_failed = false;

    for (i, step) in steps.iter().enumerate() {
        let label = format!("[{}/{}] {}", i + 1, total, step.name);
        let dot_count = 30_usize.saturating_sub(label.len());
        let dots = ".".repeat(dot_count);
        print!("{label} {dots} ");
        use std::io::Write;
        let _ = std::io::stdout().flush();

        let (ok, elapsed) = run_step(step, opts.perf_budget_multiplier.as_deref());
        let time_str = format_duration(elapsed);

        if ok {
            println!("OK ({time_str})");
        } else {
            println!("FAIL ({time_str})");
            any_failed = true;
        }

        results.push((step.name, ok, elapsed));

        if !ok && !opts.continue_on_error {
            break;
        }
    }

    let passed = results.iter().filter(|(_, ok, _)| *ok).count();
    let ran = results.len();

    println!();
    if any_failed {
        let failed_names: Vec<&str> = results
            .iter()
            .filter(|(_, ok, _)| !ok)
            .map(|(name, _, _)| *name)
            .collect();
        println!(
            "Result: {passed}/{ran} passed (failed: {})",
            failed_names.join(", ")
        );
        ExitCode::FAILURE
    } else {
        println!("Result: {passed}/{ran} passed");
        ExitCode::SUCCESS
    }
}

/// Format a `Duration` as a human-readable string like "1.2s".
fn format_duration(d: Duration) -> String {
    let secs = d.as_secs_f64();
    if secs < 60.0 {
        format!("{secs:.1}s")
    } else {
        let mins = secs / 60.0;
        format!("{mins:.1}m")
    }
}

/// Run a single step. Returns `true` on success.
fn run_step(step: &Step, perf_budget_multiplier: Option<&str>) -> (bool, Duration) {
    let start = Instant::now();

    let mut cmd = Command::new(step.cmd);
    cmd.args(step.args);
    for &(key, val) in step.env {
        cmd.env(key, val);
    }
    if step.name == "test-engine" {
        if let Some(multiplier) = perf_budget_multiplier {
            cmd.env("AIONDB_PERF_BUDGET_MULTIPLIER", multiplier);
        }
    }

    let status = cmd.status();
    let elapsed = start.elapsed();

    match status {
        Ok(s) if s.success() => (true, elapsed),
        Ok(_) | Err(_) => (false, elapsed),
    }
}
