//! `cargo xtask check-noops` - ADR-0003 guard.
//!
//! Role: verify that the `CompatCommand` list (the single source of
//! accepted `CommandNoOp` tags) does not exceed the frozen baseline
//! without explicit justification via an ADR in `docs/adr/NNNN-noop-*.md`.
//!
//! Fail-mode:
//! - any new tag not present in `BASELINE` and without an ADR → error.
//! - any removed tag → ok (feature actually implemented or rejected).
//!
//! Usage:
//! ```bash
//! cargo xtask check-noops                # strict, exits 1 on unauthorized drift
//! cargo xtask check-noops --json         # emits structured JSON for the score
//! cargo xtask check-noops --update-baseline  # regenerates the baseline (rare)
//! ```
//!
//! Voir ADR-0003 et ADR-0013.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

/// Active baseline as of 2026-04-24. Any tag outside this list requires
/// an ADR in `docs/adr/*-noop-*.md` that mentions it, or the lint fails.
///
/// `CompatCommand::ALL` was brought under the <15 target by ADR-0016
/// and then the variants removed from the matrix were removed from the
/// compat API.
pub const BASELINE: &[&str] = &[
    // Intentionally empty: no matrix-backed compat enum variant remains.
    // Matrix-only tags (`ALTER TABLE`, `GRANT`, `REVOKE`) are tracked by
    // `check-compat-hooks`, not by `CompatCommand::ALL`.
];

#[derive(Clone, Debug)]
pub struct CheckNoopsOptions {
    pub json: bool,
    pub update_baseline: bool,
}

pub fn parse_args(args: &[String]) -> Result<CheckNoopsOptions, String> {
    let mut json = false;
    let mut update_baseline = false;
    for a in args {
        match a.as_str() {
            "--json" => json = true,
            "--update-baseline" => update_baseline = true,
            "--help" | "-h" => {
                println!(
                    "Usage: cargo xtask check-noops [--json] [--update-baseline]\n\n\
Verifies that no new CompatCommand tag has been added without an ADR.\n\
See ADR-0003 and docs/adr/README.md for the contract."
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown flag: {other}")),
        }
    }
    Ok(CheckNoopsOptions {
        json,
        update_baseline,
    })
}

pub fn run(opts: CheckNoopsOptions) -> Result<(), String> {
    let repo_root = repo_root();
    let command_src = repo_root.join("crates/aiondb-pg-compat/src/command.rs");
    let adr_dir = repo_root.join("docs/adr");

    let current = extract_current_tags(&command_src)?;
    let baseline: BTreeSet<String> = BASELINE.iter().map(|s| (*s).to_owned()).collect();

    let added: BTreeSet<String> = current.difference(&baseline).cloned().collect();
    let removed: BTreeSet<String> = baseline.difference(&current).cloned().collect();

    let adr_mentions = read_adr_mentions(&adr_dir)?;
    let unjustified_added: Vec<String> = added
        .iter()
        .filter(|tag| !adr_mentions.contains_key(tag.as_str()))
        .cloned()
        .collect();

    let score = conformance_score(current.len(), unjustified_added.len());

    if opts.json {
        print_json(
            &current,
            &baseline,
            &added,
            &removed,
            &unjustified_added,
            &adr_mentions,
            score,
        );
    } else {
        print_human(
            &current,
            &baseline,
            &added,
            &removed,
            &unjustified_added,
            score,
        );
    }

    if opts.update_baseline {
        eprintln!(
            "note: --update-baseline is advisory; edit BASELINE in \
             xtask/src/check_noops.rs manually to regenerate."
        );
        return Ok(());
    }

    if !unjustified_added.is_empty() {
        return Err(format!(
            "{} compat tag(s) added without ADR: {:?}\n\
             Add an ADR at docs/adr/NNNN-noop-<tag>.md or remove the tag.",
            unjustified_added.len(),
            unjustified_added
        ));
    }
    Ok(())
}

fn repo_root() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // CARGO_MANIFEST_DIR points to xtask/, parent is repo root
    p.pop();
    p
}

fn extract_current_tags(path: &Path) -> Result<BTreeSet<String>, String> {
    let src = fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;

    // Parse `CompatCommand::ALL`, then resolve each `Self::Variant` entry
    // through the `as_tag` match body. There are currently no active
    // variants, so the expected set is empty.
    let all_variants = extract_all_variants(&src);
    let tag_by_variant = extract_as_tag_map(&src);
    let mut tags = BTreeSet::new();
    for variant in all_variants {
        let tag = tag_by_variant.get(&variant).ok_or_else(|| {
            format!("CompatCommand::ALL references {variant}, but as_tag has no matching arm")
        })?;
        tags.insert(tag.clone());
    }
    Ok(tags)
}

fn extract_all_variants(src: &str) -> Vec<String> {
    let mut variants = Vec::new();
    let mut in_all = false;
    for line in src.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("pub const ALL:") {
            in_all = true;
            if trimmed.contains("&[]") {
                break;
            }
            continue;
        }
        if in_all {
            if trimmed.starts_with("];") {
                break;
            }
            if let Some(rest) = trimmed.strip_prefix("Self::") {
                let end = rest
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .unwrap_or(rest.len());
                if end > 0 {
                    variants.push(rest[..end].to_owned());
                }
            }
        }
    }
    variants
}

fn extract_as_tag_map(src: &str) -> HashMap<String, String> {
    let mut tags = HashMap::new();
    let mut in_as_tag = false;
    for line in src.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("pub fn as_tag(") {
            in_as_tag = true;
            continue;
        }
        if in_as_tag {
            if trimmed.starts_with('}') && !trimmed.contains("=>") {
                if trimmed == "}" {
                    in_as_tag = false;
                }
                continue;
            }
            // Arm shape: `Self::Variant => "TAG STRING",`.
            if let Some(arrow_idx) = trimmed.find("=>") {
                let lhs = trimmed[..arrow_idx].trim();
                let Some(variant) = lhs.strip_prefix("Self::") else {
                    continue;
                };
                let rhs = &trimmed[arrow_idx + 2..];
                if let Some(open) = rhs.find('"') {
                    let rest = &rhs[open + 1..];
                    if let Some(end) = rest.find('"') {
                        tags.insert(variant.to_owned(), rest[..end].to_owned());
                    }
                }
            }
        }
    }
    tags
}

fn read_adr_mentions(adr_dir: &Path) -> Result<HashMap<String, PathBuf>, String> {
    let mut out = HashMap::new();
    let Ok(entries) = fs::read_dir(adr_dir) else {
        return Ok(out);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.contains("-noop-") {
            continue;
        }
        let Ok(body) = fs::read_to_string(&path) else {
            continue;
        };
        for tag in BASELINE
            .iter()
            .copied()
            .chain(ALL_KNOWN_EXTRA_TAGS.iter().copied())
        {
            if body.contains(&format!("\"{tag}\"")) || body.contains(tag) {
                out.insert(tag.to_owned(), path.clone());
            }
        }
    }
    Ok(out)
}

/// Placeholder for future tags that an ADR might mention even before they
/// land in `command.rs`. Empty by design for now.
const ALL_KNOWN_EXTRA_TAGS: &[&str] = &[];

fn conformance_score(current: usize, unjustified: usize) -> u32 {
    if current == 0 {
        return 100;
    }
    let violation_ratio = (unjustified as f64) / (current as f64);
    ((1.0 - violation_ratio) * 100.0).round().clamp(0.0, 100.0) as u32
}

fn print_human(
    current: &BTreeSet<String>,
    baseline: &BTreeSet<String>,
    added: &BTreeSet<String>,
    removed: &BTreeSet<String>,
    unjustified_added: &[String],
    score: u32,
) {
    println!("== check-noops (ADR-0003) ==");
    println!("current tags: {}", current.len());
    println!("baseline tags: {}", baseline.len());
    println!(
        "added (vs baseline): {} {}",
        added.len(),
        if added.is_empty() { "" } else { "(NEW)" }
    );
    for t in added {
        println!("  + {t}");
    }
    println!("removed (vs baseline): {}", removed.len());
    for t in removed {
        println!("  - {t}");
    }
    if !unjustified_added.is_empty() {
        println!("\nUNJUSTIFIED ADDITIONS (ADR missing):");
        for t in unjustified_added {
            println!("  ! {t}");
        }
    }
    println!("\nconformance score: {score}/100");
}

fn print_json(
    current: &BTreeSet<String>,
    baseline: &BTreeSet<String>,
    added: &BTreeSet<String>,
    removed: &BTreeSet<String>,
    unjustified_added: &[String],
    adr_mentions: &HashMap<String, PathBuf>,
    score: u32,
) {
    let mut out = String::from("{\n");
    out.push_str(&format!("  \"current_count\": {},\n", current.len()));
    out.push_str(&format!("  \"baseline_count\": {},\n", baseline.len()));
    out.push_str("  \"added\": [");
    out.push_str(
        &added
            .iter()
            .map(|t| format!("\"{}\"", escape_json(t)))
            .collect::<Vec<_>>()
            .join(", "),
    );
    out.push_str("],\n");
    out.push_str("  \"removed\": [");
    out.push_str(
        &removed
            .iter()
            .map(|t| format!("\"{}\"", escape_json(t)))
            .collect::<Vec<_>>()
            .join(", "),
    );
    out.push_str("],\n");
    out.push_str("  \"unjustified_added\": [");
    out.push_str(
        &unjustified_added
            .iter()
            .map(|t| format!("\"{}\"", escape_json(t)))
            .collect::<Vec<_>>()
            .join(", "),
    );
    out.push_str("],\n");
    out.push_str(&format!("  \"adr_mentions\": {},\n", adr_mentions.len()));
    out.push_str(&format!("  \"conformance_score\": {score}\n"));
    out.push('}');
    println!("{out}");
}

fn escape_json(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conformance_full_when_no_violations() {
        assert_eq!(conformance_score(50, 0), 100);
    }

    #[test]
    fn conformance_drops_proportionally() {
        // 5/50 = 10% violations → 90
        assert_eq!(conformance_score(50, 5), 90);
    }

    #[test]
    fn conformance_zero_when_all_violations() {
        assert_eq!(conformance_score(10, 10), 0);
    }
}
