use std::fs;
use std::path::{Path, PathBuf};

struct DependencyRule {
    manifest_path: &'static str,
    crate_name: &'static str,
    allowed_local_deps: &'static [&'static str],
}

struct MsrvApiRule {
    pattern: &'static str,
    replacement: &'static str,
}

const WORKSPACE_LINT_RULES: &[DependencyRule] = &[
    DependencyRule {
        manifest_path: "crates/aiondb-core/Cargo.toml",
        crate_name: "aiondb-core",
        allowed_local_deps: &[],
    },
    DependencyRule {
        manifest_path: "crates/aiondb-config/Cargo.toml",
        crate_name: "aiondb-config",
        allowed_local_deps: &["aiondb-core", "aiondb-shard"],
    },
    DependencyRule {
        manifest_path: "crates/aiondb-security/Cargo.toml",
        crate_name: "aiondb-security",
        allowed_local_deps: &["aiondb-core"],
    },
    DependencyRule {
        manifest_path: "crates/aiondb-tx/Cargo.toml",
        crate_name: "aiondb-tx",
        allowed_local_deps: &["aiondb-core", "aiondb-ha"],
    },
    DependencyRule {
        manifest_path: "crates/aiondb-storage-api/Cargo.toml",
        crate_name: "aiondb-storage-api",
        allowed_local_deps: &["aiondb-core", "aiondb-tx"],
    },
    DependencyRule {
        manifest_path: "crates/aiondb-engine/Cargo.toml",
        crate_name: "aiondb-engine",
        allowed_local_deps: &[
            "aiondb-catalog",
            "aiondb-catalog-store",
            "aiondb-config",
            "aiondb-core",
            "aiondb-eval",
            "aiondb-executor",
            "aiondb-extension",
            "aiondb-fragment-transport",
            "aiondb-gpu",
            "aiondb-auth-audit",
            "aiondb-ha",
            "aiondb-cluster",
            "aiondb-optimizer",
            "aiondb-parser",
            "aiondb-pg-compat",
            "aiondb-plan",
            "aiondb-planner",
            "aiondb-plpgsql",
            "aiondb-security",
            "aiondb-schema-bridge",
            "aiondb-shard",
            "aiondb-storage-api",
            "aiondb-storage-engine",
            "aiondb-tx",
            "aiondb-wal",
        ],
    },
];

const MSRV_API_RULES: &[MsrvApiRule] = &[
    MsrvApiRule {
        pattern: concat!("Duration::", "from_mins("),
        replacement: "Duration::from_secs(60 * ...)",
    },
    MsrvApiRule {
        pattern: concat!("Duration::", "from_hours("),
        replacement: "Duration::from_secs(60 * 60 * ...)",
    },
    MsrvApiRule {
        pattern: concat!(".", "is_none_or("),
        replacement: ".map_or(true, ...)",
    },
];

pub(crate) fn parse_args(args: &[String]) -> Result<(), String> {
    match args {
        [] => Ok(()),
        [flag] if matches!(flag.as_str(), "--help" | "-h") => {
            print_usage();
            std::process::exit(0);
        }
        [other, ..] => Err(format!(
            "unknown flag for workspace-lints: {other}\n\nRun `cargo xtask workspace-lints --help` for usage."
        )),
    }
}

pub(crate) fn print_usage() {
    println!(
        "\
Usage: cargo xtask workspace-lints

Verify that the v0.1 core crates only depend on the local workspace crates
allowed by the architecture dependency graph, and reject source APIs newer
than the workspace rust-version."
    );
}

pub(crate) fn run() -> Result<(), String> {
    let workspace_root = workspace_root()?;
    let mut violations = Vec::new();

    for rule in WORKSPACE_LINT_RULES {
        let manifest_path = workspace_root.join(rule.manifest_path);
        let actual = parse_local_workspace_dependencies(&manifest_path)?;
        let allowed: std::collections::BTreeSet<&str> =
            rule.allowed_local_deps.iter().copied().collect();

        for dependency in actual {
            if !allowed.contains(dependency.as_str()) {
                violations.push(format!(
                    "{} depends on disallowed local crate '{}' ({})",
                    rule.crate_name,
                    dependency,
                    manifest_path
                        .strip_prefix(&workspace_root)
                        .unwrap_or(&manifest_path)
                        .display()
                ));
            }
        }
    }
    violations.extend(check_msrv_api_usage(&workspace_root)?);

    if violations.is_empty() {
        println!("Workspace dependency and MSRV API contracts are satisfied.");
        return Ok(());
    }

    println!("Workspace dependency contract violations:");
    for violation in violations {
        println!("  - {violation}");
    }
    Err("workspace dependency contract violations detected".to_owned())
}

fn workspace_root() -> Result<PathBuf, String> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "failed to determine workspace root from xtask manifest path".to_owned())
}

fn parse_local_workspace_dependencies(manifest_path: &Path) -> Result<Vec<String>, String> {
    let content = fs::read_to_string(manifest_path)
        .map_err(|error| format!("failed to read {}: {error}", manifest_path.display()))?;
    let mut in_dependency_table = false;
    let mut dependencies = Vec::new();

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if line.starts_with('[') && line.ends_with(']') {
            let header = &line[1..line.len() - 1];
            in_dependency_table = matches!(
                header,
                "dependencies" | "dev-dependencies" | "build-dependencies"
            ) || header.ends_with(".dependencies")
                || header.ends_with(".dev-dependencies")
                || header.ends_with(".build-dependencies");
            continue;
        }

        if !in_dependency_table {
            continue;
        }

        let Some((name, _)) = line.split_once('=') else {
            continue;
        };
        let name = name.trim();
        if name.starts_with("aiondb-") {
            dependencies.push(name.to_owned());
        }
    }

    dependencies.sort();
    dependencies.dedup();
    Ok(dependencies)
}

fn check_msrv_api_usage(workspace_root: &Path) -> Result<Vec<String>, String> {
    let mut files = Vec::new();
    collect_rust_files(&workspace_root.join("crates"), &mut files)?;
    collect_rust_files(&workspace_root.join("testing"), &mut files)?;
    collect_rust_files(&workspace_root.join("xtask"), &mut files)?;

    let mut violations = Vec::new();
    for file in files {
        let content = fs::read_to_string(&file)
            .map_err(|error| format!("failed to read {}: {error}", file.display()))?;
        for (line_index, line) in content.lines().enumerate() {
            for rule in MSRV_API_RULES {
                if line.contains(rule.pattern) {
                    let display_path = file.strip_prefix(workspace_root).unwrap_or(&file);
                    violations.push(format!(
                        "{}:{} uses `{}`; rust-version 1.75 requires `{}`",
                        display_path.display(),
                        line_index + 1,
                        rule.pattern,
                        rule.replacement
                    ));
                }
            }
        }
    }
    Ok(violations)
}

fn collect_rust_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in
        fs::read_dir(dir).map_err(|error| format!("failed to read {}: {error}", dir.display()))?
    {
        let entry =
            entry.map_err(|error| format!("failed to read {} entry: {error}", dir.display()))?;
        let path = entry.path();
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if file_name == "target" || file_name.starts_with('.') {
            continue;
        }
        let file_type = entry
            .file_type()
            .map_err(|error| format!("failed to stat {}: {error}", path.display()))?;
        if file_type.is_dir() {
            collect_rust_files(&path, out)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            out.push(path);
        }
    }
    Ok(())
}
