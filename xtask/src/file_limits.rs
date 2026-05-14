use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

const DEFAULT_MAX_LINES: usize = 1400;
const SOURCE_ROOTS: &[&str] = &["crates", "testing", "xtask"];
const SKIP_DIRS: &[&str] = &[".git", "target"];

/// Parsed CLI options for `file-limits`.
pub(crate) struct FileLimitOptions {
    pub(crate) max_lines: usize,
}

struct FileViolation {
    path: PathBuf,
    line_count: usize,
    limit: usize,
}

pub(crate) fn parse_args(args: &[String]) -> Result<FileLimitOptions, String> {
    let mut max_lines = DEFAULT_MAX_LINES;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--max-lines" => {
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| "--max-lines requires a value".to_owned())?;
                max_lines = value.parse::<usize>().map_err(|_| {
                    format!("invalid value for --max-lines: '{value}' is not a positive integer")
                })?;
                if max_lines == 0 {
                    return Err("--max-lines must be greater than zero".to_owned());
                }
            }
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            other => {
                return Err(format!(
                    "unknown flag for file-limits: {other}\n\nRun `cargo xtask file-limits --help` for usage."
                ));
            }
        }
        i += 1;
    }

    Ok(FileLimitOptions { max_lines })
}

pub(crate) fn print_usage() {
    println!(
        "\
Usage: cargo xtask file-limits [OPTIONS]

Enforce the architecture line limit across Rust source files in `crates/`,
`testing/`, and `xtask/`.

Options:
    --max-lines <N>      Override the maximum allowed line count (default: 1400)
  -h, --help           Print this help message"
    );
}

pub(crate) fn run(opts: FileLimitOptions) -> Result<(), String> {
    let workspace_root = workspace_root()?;
    let source_files = discover_source_files(&workspace_root).map_err(|error| {
        format!(
            "failed to discover Rust source files under {}: {error}",
            workspace_root.display()
        )
    })?;

    let allowlist = load_allowlist(&workspace_root);

    let mut violations = Vec::new();
    for path in source_files {
        let line_count = count_lines(&path)
            .map_err(|error| format!("failed to count lines in {}: {error}", path.display()))?;
        if line_count > opts.max_lines {
            let relative = path
                .strip_prefix(&workspace_root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            if let Some(allow) = allowlist.get(&relative) {
                if allow
                    .max_lines
                    .map_or(true, |max_lines| line_count <= max_lines)
                {
                    continue;
                }
                violations.push(FileViolation {
                    path,
                    line_count,
                    limit: allow.max_lines.unwrap_or(opts.max_lines),
                });
                continue;
            }
            violations.push(FileViolation {
                path,
                line_count,
                limit: opts.max_lines,
            });
        }
    }

    violations.sort_by(|left, right| {
        right
            .line_count
            .cmp(&left.line_count)
            .then_with(|| left.path.cmp(&right.path))
    });

    if violations.is_empty() {
        println!(
            "All Rust source files are within the {limit}-line limit.",
            limit = opts.max_lines
        );
        return Ok(());
    }

    println!(
        "Found {} Rust source files over their configured line limits:",
        violations.len()
    );
    for violation in violations {
        let relative = violation
            .path
            .strip_prefix(&workspace_root)
            .unwrap_or(&violation.path);
        println!(
            "  {:>5} / {:>5}  {}",
            violation.line_count,
            violation.limit,
            relative.display()
        );
    }

    Err("file-size architecture limit exceeded".to_owned())
}

fn workspace_root() -> Result<PathBuf, String> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "failed to determine workspace root from xtask manifest path".to_owned())
}

fn discover_source_files(workspace_root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    for root in SOURCE_ROOTS {
        let path = workspace_root.join(root);
        if path.exists() {
            collect_source_files(&path, &mut files)?;
        }
    }

    files.sort();
    Ok(files)
}

fn collect_source_files(path: &Path, files: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let mut entries = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let file_type = entry.file_type()?;
        let entry_path = entry.path();

        if file_type.is_dir() {
            if should_skip_dir(entry.file_name().as_os_str()) {
                continue;
            }
            collect_source_files(&entry_path, files)?;
            continue;
        }

        if file_type.is_file() && entry_path.extension() == Some(OsStr::new("rs")) {
            files.push(entry_path);
        }
    }

    Ok(())
}

fn should_skip_dir(name: &OsStr) -> bool {
    SKIP_DIRS.iter().any(|dir| name == OsStr::new(dir))
}

#[derive(Clone, Copy, Debug)]
struct AllowlistEntry {
    max_lines: Option<usize>,
}

/// Load `.file-limits-allow` from workspace root. Each non-empty,
/// non-comment line is either:
///   * `path/to/file.rs` - temporary full exemption.
///   * `path/to/file.rs <= N` - exemption capped at N lines; growth fails CI.
fn load_allowlist(workspace_root: &Path) -> std::collections::HashMap<String, AllowlistEntry> {
    let path = workspace_root.join(".file-limits-allow");
    let Ok(content) = fs::read_to_string(&path) else {
        return std::collections::HashMap::new();
    };
    content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(parse_allowlist_line)
        .collect()
}

fn parse_allowlist_line(line: &str) -> Option<(String, AllowlistEntry)> {
    if let Some((path, max_lines)) = line.split_once("<=") {
        let max_lines = max_lines.trim().parse::<usize>().ok()?;
        return Some((
            path.trim().to_owned(),
            AllowlistEntry {
                max_lines: Some(max_lines),
            },
        ));
    }
    Some((line.to_owned(), AllowlistEntry { max_lines: None }))
}

fn count_lines(path: &Path) -> std::io::Result<usize> {
    let reader = BufReader::new(File::open(path)?);
    reader
        .lines()
        .try_fold(0usize, |count, line| line.map(|_| count + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_allowlist_line_without_cap_keeps_full_exemption() {
        let (path, entry) = parse_allowlist_line("crates/aiondb-engine/src/engine/query_api.rs")
            .expect("allowlist entry");
        assert_eq!(path, "crates/aiondb-engine/src/engine/query_api.rs");
        assert_eq!(entry.max_lines, None);
    }

    #[test]
    fn parse_allowlist_line_with_cap_records_growth_limit() {
        let (path, entry) =
            parse_allowlist_line("crates/aiondb-engine/src/engine/query_api.rs <= 5272")
                .expect("capped allowlist entry");
        assert_eq!(path, "crates/aiondb-engine/src/engine/query_api.rs");
        assert_eq!(entry.max_lines, Some(5272));
    }

    #[test]
    fn parse_allowlist_line_with_invalid_cap_is_rejected() {
        assert!(
            parse_allowlist_line("crates/aiondb-engine/src/engine/query_api.rs <= many").is_none()
        );
    }
}
