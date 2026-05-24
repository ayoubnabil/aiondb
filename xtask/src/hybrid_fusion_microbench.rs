use std::path::Path;
use std::process::Command;

pub(crate) struct HybridFusionMicrobenchOptions {
    check_only: bool,
}

pub(crate) fn parse_args(args: &[String]) -> Result<HybridFusionMicrobenchOptions, String> {
    let mut check_only = false;
    for arg in args {
        match arg.as_str() {
            "--check" => check_only = true,
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            other => {
                return Err(format!(
                    "unknown flag for hybrid-fusion-microbench: {other}\n\nRun `cargo xtask hybrid-fusion-microbench --help` for usage."
                ));
            }
        }
    }
    Ok(HybridFusionMicrobenchOptions { check_only })
}

pub(crate) fn print_usage() {
    println!(
        "\
Usage: cargo xtask hybrid-fusion-microbench [--check]

Run the hybrid/vector fusion microbenchmark under `benchmarks/hybrid-fusion-micro`.
The benchmark reads its tunables from environment variables such as
FUSION_CANDIDATES, FUSION_K, FUSION_ITERS, JSON_CANDIDATES, and JSON_ITERS.

Options:
    --check      Compile the microbenchmark without running it
  -h, --help    Print this help message"
    );
}

pub(crate) fn run(opts: HybridFusionMicrobenchOptions) -> Result<(), String> {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| "failed to resolve workspace root from CARGO_MANIFEST_DIR".to_owned())?;
    let benchmark_dir = workspace_root.join("benchmarks/hybrid-fusion-micro");
    let mut command = Command::new("cargo");
    command.current_dir(&benchmark_dir);
    if opts.check_only {
        command.args(["check", "--quiet"]);
    } else {
        command.args(["run", "--release", "--quiet"]);
    }
    let status = command.status().map_err(|error| {
        format!(
            "failed to run cargo in {}: {error}",
            benchmark_dir.display()
        )
    })?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "hybrid fusion microbenchmark exited with status {status}"
        ))
    }
}
