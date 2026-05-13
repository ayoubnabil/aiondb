use std::process::{Command, ExitCode};

pub(crate) struct PgCompatOptions {
    file_filter: Option<String>,
    debug_mode: bool,
    release: bool,
}

pub(crate) fn parse_args(args: &[String]) -> Result<PgCompatOptions, String> {
    let mut file_filter = None;
    let mut debug_mode = false;
    let mut release = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--file" => {
                i += 1;
                file_filter = Some(
                    args.get(i)
                        .ok_or_else(|| "--file requires a value".to_owned())?
                        .clone(),
                );
            }
            "--debug" => {
                debug_mode = true;
            }
            "--release" => {
                release = true;
            }
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            other => {
                return Err(format!(
                    "unknown flag for pg-compat: {other}\n\nRun `cargo xtask pg-compat --help` for usage."
                ));
            }
        }
        i += 1;
    }

    Ok(PgCompatOptions {
        file_filter,
        debug_mode,
        release,
    })
}

pub(crate) fn run(opts: PgCompatOptions) -> ExitCode {
    let mut cmd = Command::new("cargo");
    cmd.arg("run")
        .arg("--manifest-path")
        .arg(".pg-regress/Cargo.toml")
        .arg("--bin")
        .arg("pg-regress");

    if opts.release {
        cmd.arg("--release");
    }

    if let Some(file_filter) = &opts.file_filter {
        cmd.env("PG_REGRESS_FILE", file_filter);
    }
    if opts.debug_mode {
        cmd.env("PG_REGRESS_DEBUG", "1");
    }

    match cmd.status() {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(_) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("error: failed to launch PostgreSQL compatibility runner: {error}");
            ExitCode::FAILURE
        }
    }
}

fn print_usage() {
    println!(
        "\
Usage: cargo xtask pg-compat [OPTIONS]

Run the PostgreSQL regression compatibility runner stored under `.pg-regress/`.

Options:
  --file <NAME>  Run a single regression file (sets `PG_REGRESS_FILE`)
  --debug        Enable mismatch details (sets `PG_REGRESS_DEBUG=1`)
  --release      Run the runner in release mode
  -h, --help     Print this help message"
    );
}
