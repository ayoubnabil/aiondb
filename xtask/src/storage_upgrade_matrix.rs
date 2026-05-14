use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const FIXTURE_VERSIONS: &[&str] = &["0.1", "0.2", "1.0", "1.1"];

pub(crate) struct StorageUpgradeMatrixOptions {
    fixture_root: PathBuf,
    scratch_root: PathBuf,
    aiondb_bin: Option<PathBuf>,
    strict_fixtures: bool,
}

pub(crate) fn parse_args(args: &[String]) -> Result<StorageUpgradeMatrixOptions, String> {
    let workspace_root = workspace_root()?;
    let mut fixture_root = workspace_root.join("testing/storage-upgrade-fixtures");
    let mut scratch_root = std::env::temp_dir().join("aiondb-storage-upgrade-matrix");
    let mut aiondb_bin = None;
    let mut strict_fixtures = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--fixture-root" => {
                i += 1;
                fixture_root = PathBuf::from(required_value(args, i, "--fixture-root")?);
            }
            "--scratch-root" => {
                i += 1;
                scratch_root = PathBuf::from(required_value(args, i, "--scratch-root")?);
            }
            "--aiondb-bin" => {
                i += 1;
                aiondb_bin = Some(PathBuf::from(required_value(args, i, "--aiondb-bin")?));
            }
            "--strict-fixtures" => {
                strict_fixtures = true;
            }
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            other => {
                return Err(format!(
                    "unknown flag for storage-upgrade-matrix: {other}\n\nRun `cargo xtask storage-upgrade-matrix --help` for usage."
                ));
            }
        }
        i += 1;
    }

    Ok(StorageUpgradeMatrixOptions {
        fixture_root,
        scratch_root,
        aiondb_bin,
        strict_fixtures,
    })
}

fn required_value(args: &[String], index: usize, flag: &str) -> Result<String, String> {
    args.get(index)
        .cloned()
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn print_usage() {
    println!(
        "\
Usage: cargo xtask storage-upgrade-matrix [OPTIONS]

Copy historical binary data-dir fixtures to a scratch directory and run:
doctor -> upgrade -> doctor.

Options:
    --fixture-root <PATH>   Fixture root (default: testing/storage-upgrade-fixtures)
    --scratch-root <PATH>   Scratch root (default: system temp/aiondb-storage-upgrade-matrix)
    --aiondb-bin <PATH>     Existing aiondb binary to test
    --strict-fixtures       Fail when any required fixture slot is missing
  -h, --help                Print this help message"
    );
}

pub(crate) fn run(opts: StorageUpgradeMatrixOptions) -> ExitCode {
    match run_inner(opts) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run_inner(opts: StorageUpgradeMatrixOptions) -> Result<(), String> {
    let workspace_root = workspace_root()?;
    let aiondb_bin = resolve_aiondb_binary(&workspace_root, opts.aiondb_bin.as_deref())?;
    fs::create_dir_all(&opts.scratch_root).map_err(|error| {
        format!(
            "failed to create scratch root {}: {error}",
            opts.scratch_root.display()
        )
    })?;

    let mut missing = Vec::new();
    let mut tested = 0usize;
    for version in FIXTURE_VERSIONS {
        let source = opts.fixture_root.join(version);
        if !source.is_dir() {
            println!("storage-upgrade-matrix: SKIP {version} missing fixture");
            missing.push(*version);
            continue;
        }

        let target = opts.scratch_root.join(version);
        reject_unsafe_scratch_target(&source, &target)?;
        replace_dir_from_fixture(&source, &target)?;
        println!(
            "storage-upgrade-matrix: CHECK {version} at {}",
            target.display()
        );
        run_aiondb(&aiondb_bin, "doctor", &target)?;
        run_aiondb(&aiondb_bin, "upgrade", &target)?;
        run_aiondb(&aiondb_bin, "doctor", &target)?;
        tested += 1;
    }

    if opts.strict_fixtures && !missing.is_empty() {
        return Err(format!(
            "missing required storage fixture slots: {}",
            missing.join(", ")
        ));
    }

    if tested == 0 {
        println!("storage-upgrade-matrix: no fixtures tested");
    } else {
        println!("storage-upgrade-matrix: tested {tested} fixture(s)");
    }
    Ok(())
}

fn reject_unsafe_scratch_target(source: &Path, target: &Path) -> Result<(), String> {
    let source = absolute_path(source)?;
    let target = absolute_path(target)?;
    if target == source || target.starts_with(&source) {
        return Err(format!(
            "scratch target {} must not be the source fixture or inside it",
            target.display()
        ));
    }
    Ok(())
}

fn absolute_path(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|current_dir| current_dir.join(path))
            .map_err(|error| format!("failed to resolve current directory: {error}"))
    }
}

fn resolve_aiondb_binary(
    workspace_root: &Path,
    configured: Option<&Path>,
) -> Result<PathBuf, String> {
    if let Some(path) = configured {
        if path.is_file() {
            return Ok(path.to_path_buf());
        }
        return Err(format!(
            "configured aiondb binary does not exist: {}",
            path.display()
        ));
    }

    let default_bin = workspace_root.join("target/debug/aiondb");
    if default_bin.is_file() {
        return Ok(default_bin);
    }

    println!("storage-upgrade-matrix: building aiondb binary");
    let status = Command::new("cargo")
        .args([
            "build",
            "--locked",
            "-p",
            "aiondb-server",
            "--bin",
            "aiondb",
        ])
        .current_dir(workspace_root)
        .status()
        .map_err(|error| format!("failed to run cargo build for aiondb: {error}"))?;
    if !status.success() {
        return Err(format!(
            "cargo build for aiondb failed with status {status}"
        ));
    }
    if default_bin.is_file() {
        Ok(default_bin)
    } else {
        Err(format!(
            "aiondb binary was not produced at {}",
            default_bin.display()
        ))
    }
}

fn replace_dir_from_fixture(source: &Path, target: &Path) -> Result<(), String> {
    if target.exists() {
        fs::remove_dir_all(target).map_err(|error| {
            format!("failed to clear scratch dir {}: {error}", target.display())
        })?;
    }
    copy_dir_recursive(source, target)
}

fn copy_dir_recursive(source: &Path, target: &Path) -> Result<(), String> {
    fs::create_dir_all(target)
        .map_err(|error| format!("failed to create {}: {error}", target.display()))?;
    let mut entries = fs::read_dir(source)
        .map_err(|error| format!("failed to read {}: {error}", source.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("failed to enumerate {}: {error}", source.display()))?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        let file_type = entry
            .file_type()
            .map_err(|error| format!("failed to stat {}: {error}", source_path.display()))?;
        if file_type.is_symlink() {
            return Err(format!(
                "fixture contains unsupported symlink: {}",
                source_path.display()
            ));
        }
        if file_type.is_dir() {
            copy_dir_recursive(&source_path, &target_path)?;
        } else if file_type.is_file() {
            fs::copy(&source_path, &target_path).map_err(|error| {
                format!(
                    "failed to copy {} to {}: {error}",
                    source_path.display(),
                    target_path.display()
                )
            })?;
        }
    }

    Ok(())
}

fn run_aiondb(aiondb_bin: &Path, command: &str, data_dir: &Path) -> Result<(), String> {
    let status = Command::new(aiondb_bin)
        .arg(command)
        .arg("--data-dir")
        .arg(data_dir)
        .status()
        .map_err(|error| format!("failed to run aiondb {command}: {error}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "aiondb {command} failed for {} with status {status}",
            data_dir.display()
        ))
    }
}

fn workspace_root() -> Result<PathBuf, String> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "failed to determine workspace root from xtask manifest path".to_owned())
}
