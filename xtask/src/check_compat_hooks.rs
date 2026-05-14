//! `cargo xtask check-compat-hooks` - stop the extension of the compat layer.
//!
//! Role: lock down both compat surfaces:
//!   * le nombre de fonctions `fn try_execute_compat_*` dans
//!     `crates/aiondb-engine/src/` cannot exceed the baseline
//!     sans ADR `docs/adr/NNNN-*compat-hook*.md` ;
//!   * la taille de `COMPAT_TAG_MATRIX` (via le const
//!     `EXPECTED_MATRIX_SIZE` in the Rust test) cannot exceed the
//!     baseline.
//!
//! Fail-mode :
//!   * any addition beyond the baseline without an ADR → exit 1.
//!   * any removal (hook removed or refactored) → OK.
//!
//! Usage :
//! ```bash
//! cargo xtask check-compat-hooks           # strict
//! cargo xtask check-compat-hooks --json    # JSON pour la CI
//! ```

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

/// Number of `fn try_execute_compat_*` frozen on 2026-04-23.
/// Any addition above this bound requires an ADR. Baseline tightened
/// four times:
/// * 27 → 16 after the CompatRouter refactor (Task 7, 2026-04-23).
/// * 16 → 15 quand `try_execute_compat_do_also_rule_dml` → `execute_*`.
/// * 15 → 13 after the `object_address_do_warning_block` →
///   `detect_*` et `alter_table_noop_command` → `match_*`.
/// * 13 → 12 (2026-04-24) - rename
///   `try_execute_compat_revoke_role_command` →
///   `handle_compat_revoke_role_option_for` (helper interne, pas un
///   hook de dispatch). Short-term target hit.
/// * 12 → 5 (2026-04-24) - direct/drop-if-exists/type-drop paths
///   renamed to result helpers.
/// * 5 → 0 (2026-04-24) - remaining cursor, DO block, prepared, rule
///   command and rule DML hooks no longer expose `try_execute_compat_*`
///   entrypoint names.
const COMPAT_HOOK_BASELINE: usize = 0;

/// Maximum size of `COMPAT_TAG_MATRIX` frozen. Aligned with
/// `EXPECTED_MATRIX_SIZE` dans `crates/aiondb-pg-compat/src/compat_tag_matrix.rs`.
/// Any increase requires an ADR. Baseline ratcheted over the course of
/// ADR-0016:
///   * 2026-04-23 Task 14 - DATABASE family (3 tags): 57 → 54.
///   * 2026-04-23 Task 13/15 - TYPE/DOMAIN/CAST/RULE/OR-REPLACE (12
///     tags): 54 → 42.
///   * 2026-04-24 Path B - misc-object dispatch consolidation (22
///     tags): 42 → 20.
///   * 2026-04-24 Path A follow-up - 6 sensitive CREATE forms typed
///     as `Statement::Create{Policy,Publication,Subscription,Server,
///     UserMapping,ForeignTable}`: 20 → 14.
///   * 2026-04-24 Path A final - 11 remaining compat families typed
///     (COLLATION, STATISTICS, TABLESPACE, AGGREGATE, PROCEDURE,
///     ROUTINE, TRIGGER, OPERATOR): 14 → 3.
///   * 2026-05-01 - CREATE TEXT SEARCH remains matrix-tracked because
///     it records real session metadata that is not yet represented by a
///     typed AST family; rust/doc matrix already pins EXPECTED_MATRIX_SIZE=4.
const TAG_MATRIX_BASELINE: usize = 4;

#[derive(Clone, Debug)]
pub struct CheckCompatHooksOptions {
    pub json: bool,
}

pub fn parse_args(args: &[String]) -> Result<CheckCompatHooksOptions, String> {
    let mut json = false;
    for a in args {
        match a.as_str() {
            "--json" => json = true,
            "--help" | "-h" => {
                println!(
                    "Usage: cargo xtask check-compat-hooks [--json]\n\n\
Verifies that the compat layer does not grow beyond the frozen baseline:\n\
  * {COMPAT_HOOK_BASELINE} `try_execute_compat_*` hooks\n\
  * {TAG_MATRIX_BASELINE} entries in COMPAT_TAG_MATRIX\n\n\
Any new addition requires an ADR at docs/adr/NNNN-*compat*.md."
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown flag: {other}")),
        }
    }
    Ok(CheckCompatHooksOptions { json })
}

/// Short-term KPI target. Original goal (2026-04-22) was `75 → 35
/// tags`. Reached 2026-04-24 via ADR-0016 Path A + Path B (now 20).
/// New stretch target: 15 (retire the remaining ad-hoc CREATE entries
/// or fold them into typed Statement variants).
const MATRIX_TARGET: usize = 15;

/// Short-term KPI target for `try_execute_compat_*` hooks: zero.
const COMPAT_HOOK_TARGET: usize = 0;

#[derive(Default, Clone, Debug)]
struct MatrixBreakdown {
    implemented_real: usize,
    explicit_not_supported: usize,
    intentional_noop: usize,
    dispatch_dedicated: usize,
    dispatch_alter_misc: usize,
    dispatch_drop_misc: usize,
    dispatch_tripwire: usize,
}

impl MatrixBreakdown {
    fn total(&self) -> usize {
        self.implemented_real + self.explicit_not_supported + self.intentional_noop
    }
}

pub fn run(opts: CheckCompatHooksOptions) -> Result<(), String> {
    let repo_root = repo_root();
    let engine_src = repo_root.join("crates/aiondb-engine/src");
    let matrix_src = repo_root.join("crates/aiondb-pg-compat/src/compat_tag_matrix.rs");
    let adr_dir = repo_root.join("docs/adr");

    let hooks = collect_compat_hooks(&engine_src)?;
    let hook_count = hooks.len();
    let matrix_size = read_matrix_size(&matrix_src)?;
    let breakdown = read_matrix_breakdown(&matrix_src)?;
    let adr_mentions = read_adr_mentions(&adr_dir)?;

    let hook_overflow = hook_count.saturating_sub(COMPAT_HOOK_BASELINE);
    let matrix_overflow = matrix_size.saturating_sub(TAG_MATRIX_BASELINE);

    // A hook addition is justified iff an ADR file names it explicitly.
    let unjustified_hooks: Vec<String> = hooks
        .iter()
        .filter(|hook| !adr_mentions.contains_key(hook.as_str()))
        .cloned()
        .collect();
    let unjustified_overflow = if hook_overflow == 0 {
        Vec::new()
    } else {
        // When over baseline, every hook beyond what is ADR-justified counts
        // against conformance.
        unjustified_hooks
            .iter()
            .take(hook_overflow)
            .cloned()
            .collect()
    };

    if opts.json {
        print_json(
            hook_count,
            matrix_size,
            hook_overflow,
            matrix_overflow,
            &unjustified_overflow,
            &breakdown,
        );
    } else {
        print_human(
            hook_count,
            matrix_size,
            hook_overflow,
            matrix_overflow,
            &unjustified_overflow,
            &breakdown,
        );
    }

    if hook_overflow > 0 && !unjustified_overflow.is_empty() {
        return Err(format!(
            "{} compat hook(s) exceed baseline ({}). Unjustified: {:?}.\n\
             Add an ADR at docs/adr/NNNN-compat-hook-<name>.md or remove the hook.",
            hook_overflow, COMPAT_HOOK_BASELINE, unjustified_overflow
        ));
    }
    if matrix_overflow > 0 {
        return Err(format!(
            "COMPAT_TAG_MATRIX has {} entries, baseline is {}.\n\
             Update EXPECTED_MATRIX_SIZE + docs/compat/tag_matrix.toml + \
             docs/adr/NNNN-compat-tag-<tag>.md.",
            matrix_size, TAG_MATRIX_BASELINE
        ));
    }
    if breakdown.total() != matrix_size {
        return Err(format!(
            "matrix breakdown sum ({}) differs from matrix size ({}); \
             the behaviour parser missed an entry — check \
             `read_matrix_breakdown`.",
            breakdown.total(),
            matrix_size
        ));
    }
    if breakdown.intentional_noop > 0 {
        return Err(format!(
            "COMPAT_TAG_MATRIX reintroduced {} IntentionalNoop tag(s). \
             Compatibility stubs must be implemented for real or rejected \
             explicitly before release.",
            breakdown.intentional_noop
        ));
    }
    Ok(())
}

fn repo_root() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p
}

/// Walk the engine source tree and collect every `fn try_execute_compat_*`
/// function name (deduplicated, stable order).
fn collect_compat_hooks(root: &Path) -> Result<BTreeSet<String>, String> {
    let mut out = BTreeSet::new();
    collect_hooks_recursive(root, &mut out)?;
    Ok(out)
}

fn collect_hooks_recursive(dir: &Path, out: &mut BTreeSet<String>) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(|e| format!("reading {}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| format!("reading entry: {e}"))?;
        let path = entry.path();
        if path.is_dir() {
            collect_hooks_recursive(&path, out)?;
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("rs") {
            continue;
        }
        let src =
            fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
        for line in src.lines() {
            let trimmed = line.trim();
            // Match `fn try_execute_compat_<name>(` optionally prefixed by
            // visibility modifiers.
            let needle = "fn try_execute_compat_";
            if let Some(start) = trimmed.find(needle) {
                let after = &trimmed[start + needle.len()..];
                let end = after
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .unwrap_or(after.len());
                if end > 0 {
                    out.insert(format!("try_execute_compat_{}", &after[..end]));
                }
            }
        }
    }
    Ok(())
}

fn read_matrix_size(path: &Path) -> Result<usize, String> {
    let src = fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    // Extract `const EXPECTED_MATRIX_SIZE: usize = N;`
    for line in src.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("const EXPECTED_MATRIX_SIZE: usize = ") {
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if !digits.is_empty() {
                return digits
                    .parse::<usize>()
                    .map_err(|e| format!("parsing EXPECTED_MATRIX_SIZE: {e}"));
            }
        }
    }
    Err(format!(
        "could not find EXPECTED_MATRIX_SIZE in {}",
        path.display()
    ))
}

/// Scan the matrix source file and tally `behavior:` / `dispatch:` markers.
/// Parsing is deliberately text-based so the xtask stays dependency-free -
/// the matrix file is a plain Rust const array with one `behavior:` and one
/// `dispatch:` line per entry, so the signals are unambiguous.
fn read_matrix_breakdown(path: &Path) -> Result<MatrixBreakdown, String> {
    let src = fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let mut breakdown = MatrixBreakdown::default();
    for raw in src.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("behavior: CompatTagBehavior::") {
            let bucket: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphabetic())
                .collect();
            match bucket.as_str() {
                "ImplementedReal" => breakdown.implemented_real += 1,
                "ExplicitNotSupported" => breakdown.explicit_not_supported += 1,
                "IntentionalNoop" => breakdown.intentional_noop += 1,
                _ => {}
            }
        } else if let Some(rest) = line.strip_prefix("dispatch: CompatDispatch::") {
            let bucket: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphabetic())
                .collect();
            match bucket.as_str() {
                "Dedicated" => breakdown.dispatch_dedicated += 1,
                "AlterMiscObject" => breakdown.dispatch_alter_misc += 1,
                "DropMiscObject" => breakdown.dispatch_drop_misc += 1,
                "TripwireOnly" => breakdown.dispatch_tripwire += 1,
                _ => {}
            }
        }
    }
    Ok(breakdown)
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
        if !name.contains("compat") {
            continue;
        }
        let Ok(body) = fs::read_to_string(&path) else {
            continue;
        };
        // Look for `try_execute_compat_*` mentions in the ADR body.
        for token in body.split(|c: char| !c.is_ascii_alphanumeric() && c != '_') {
            if token.starts_with("try_execute_compat_") {
                out.insert(token.to_owned(), path.clone());
            }
        }
    }
    Ok(out)
}

fn pct(n: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        (n as f64 / total as f64) * 100.0
    }
}

fn print_human(
    hook_count: usize,
    matrix_size: usize,
    hook_overflow: usize,
    matrix_overflow: usize,
    unjustified_overflow: &[String],
    breakdown: &MatrixBreakdown,
) {
    println!("== check-compat-hooks ==");
    println!(
        "try_execute_compat_* hooks : {hook_count} (baseline {COMPAT_HOOK_BASELINE}, target {COMPAT_HOOK_TARGET}, overflow {hook_overflow})"
    );
    println!(
        "COMPAT_TAG_MATRIX entries : {matrix_size} (baseline {TAG_MATRIX_BASELINE}, target {MATRIX_TARGET}, overflow {matrix_overflow})"
    );
    let total = breakdown.total();
    println!(
        "  by behaviour            : implemented_real={} ({:.1}%), explicit_not_supported={} ({:.1}%), intentional_noop={} ({:.1}%)",
        breakdown.implemented_real,
        pct(breakdown.implemented_real, total),
        breakdown.explicit_not_supported,
        pct(breakdown.explicit_not_supported, total),
        breakdown.intentional_noop,
        pct(breakdown.intentional_noop, total),
    );
    println!(
        "  by dispatch             : dedicated={}, alter_misc_object={}, drop_misc_object={}, tripwire_only={}",
        breakdown.dispatch_dedicated,
        breakdown.dispatch_alter_misc,
        breakdown.dispatch_drop_misc,
        breakdown.dispatch_tripwire,
    );
    let matrix_to_target = matrix_size.saturating_sub(MATRIX_TARGET);
    let hooks_to_target = hook_count.saturating_sub(COMPAT_HOOK_TARGET);
    println!(
        "KPIs : {matrix_to_target} tags to retire to reach short-term target ({MATRIX_TARGET}); \
         {hooks_to_target} hooks to retire to reach short-term target ({COMPAT_HOOK_TARGET})."
    );
    if !unjustified_overflow.is_empty() {
        println!("\nUNJUSTIFIED HOOK ADDITIONS (ADR missing):");
        for hook in unjustified_overflow {
            println!("  ! {hook}");
        }
    }
}

fn print_json(
    hook_count: usize,
    matrix_size: usize,
    hook_overflow: usize,
    matrix_overflow: usize,
    unjustified_overflow: &[String],
    breakdown: &MatrixBreakdown,
) {
    let total = breakdown.total();
    let mut out = String::from("{\n");
    out.push_str(&format!("  \"hook_count\": {hook_count},\n"));
    out.push_str(&format!("  \"hook_baseline\": {COMPAT_HOOK_BASELINE},\n"));
    out.push_str(&format!("  \"hook_target\": {COMPAT_HOOK_TARGET},\n"));
    out.push_str(&format!("  \"hook_overflow\": {hook_overflow},\n"));
    out.push_str(&format!(
        "  \"hooks_to_target\": {},\n",
        hook_count.saturating_sub(COMPAT_HOOK_TARGET)
    ));
    out.push_str(&format!("  \"matrix_size\": {matrix_size},\n"));
    out.push_str(&format!("  \"matrix_baseline\": {TAG_MATRIX_BASELINE},\n"));
    out.push_str(&format!("  \"matrix_target\": {MATRIX_TARGET},\n"));
    out.push_str(&format!("  \"matrix_overflow\": {matrix_overflow},\n"));
    out.push_str(&format!(
        "  \"matrix_to_target\": {},\n",
        matrix_size.saturating_sub(MATRIX_TARGET)
    ));
    out.push_str("  \"tags_by_behavior\": {\n");
    out.push_str(&format!(
        "    \"implemented_real\": {{\"count\": {}, \"pct\": {:.2}}},\n",
        breakdown.implemented_real,
        pct(breakdown.implemented_real, total)
    ));
    out.push_str(&format!(
        "    \"explicit_not_supported\": {{\"count\": {}, \"pct\": {:.2}}},\n",
        breakdown.explicit_not_supported,
        pct(breakdown.explicit_not_supported, total)
    ));
    out.push_str(&format!(
        "    \"intentional_noop\": {{\"count\": {}, \"pct\": {:.2}}}\n",
        breakdown.intentional_noop,
        pct(breakdown.intentional_noop, total)
    ));
    out.push_str("  },\n");
    out.push_str("  \"tags_by_dispatch\": {\n");
    out.push_str(&format!(
        "    \"dedicated\": {},\n",
        breakdown.dispatch_dedicated
    ));
    out.push_str(&format!(
        "    \"alter_misc_object\": {},\n",
        breakdown.dispatch_alter_misc
    ));
    out.push_str(&format!(
        "    \"drop_misc_object\": {},\n",
        breakdown.dispatch_drop_misc
    ));
    out.push_str(&format!(
        "    \"tripwire_only\": {}\n",
        breakdown.dispatch_tripwire
    ));
    out.push_str("  },\n");
    out.push_str("  \"unjustified_overflow\": [");
    out.push_str(
        &unjustified_overflow
            .iter()
            .map(|h| format!("\"{h}\""))
            .collect::<Vec<_>>()
            .join(", "),
    );
    out.push_str("]\n}");
    println!("{out}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::absurd_extreme_comparisons, clippy::assertions_on_constants)]
    fn baseline_constants_are_documented() {
        // If these fail, bump them intentionally and add the matching ADR.
        assert!(COMPAT_HOOK_BASELINE <= 30, "hook baseline drift too high");
        assert!(TAG_MATRIX_BASELINE <= 80, "matrix baseline drift too high");
    }
}
