//! `cargo xtask compat-debt-report` - single-shot KPI snapshot of the
//! PG-compat debt surface.
//!
//! Collects, from the live source tree:
//!   * number of entries in `COMPAT_TAG_MATRIX` (by behaviour bucket),
//!   * parser sites emitting `Statement::CommandNoOp` and `CompatTagged*`,
//!   * engine hooks `fn try_execute_compat_*`,
//!   * no-op plan references (`LogicalPlan::CommandNoOp`,
//!     `PhysicalPlan::CommandNoOp`) outside of type definitions,
//!   * shadow-dispatch bridge sites (`CompatStatement` / legacy
//!     `CompatTagged` materialisation),
//!   * duplicate no-op validator definitions outside
//!     `aiondb-pg-compat::noop_validation`,
//!   * tags retired from the matrix but still documented as migration
//!     debt,
//!   * tests associated with the compat surface (`CommandNoOp` /
//!     `try_execute_compat_` references inside `#[test]` / `#[tokio::test]`
//!     bodies).
//!
//! The counts are purely observational: the command never fails on a
//! number, it only fails when the report cannot be produced. The other
//! xtasks (`check-compat-hooks`, `check-noops`) are the enforcement side.
//!
//! Usage:
//! ```bash
//! cargo xtask compat-debt-report           # human summary
//! cargo xtask compat-debt-report --json    # CI-friendly JSON
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct CompatDebtReportOptions {
    pub json: bool,
}

pub fn parse_args(args: &[String]) -> Result<CompatDebtReportOptions, String> {
    let mut json = false;
    for a in args {
        match a.as_str() {
            "--json" => json = true,
            "--help" | "-h" => {
                println!(
                    "Usage: cargo xtask compat-debt-report [--json]\n\n\
Snapshot of the PG-compat debt surface:\n\
  * COMPAT_TAG_MATRIX entries (by behaviour),\n\
  * retired tag comments,\n\
  * parser CommandNoOp / CompatTagged emission sites,\n\
  * try_execute_compat_* hooks,\n\
  * shadow-dispatch bridge sites,\n\
  * duplicate no-op validator definitions,\n\
  * no-op plan references,\n\
  * associated tests.\n\n\
Report-only: never fails on numbers, only on scan errors."
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown flag: {other}")),
        }
    }
    Ok(CompatDebtReportOptions { json })
}

#[derive(Default, Debug)]
struct MatrixStats {
    total: usize,
    implemented_real: usize,
    explicit_not_supported: usize,
    intentional_noop: usize,
    retired_tag_comments: usize,
    source_path: String,
}

#[derive(Default, Debug)]
struct ParserStats {
    emission_sites: usize,
    files: BTreeMap<String, usize>,
    compat_tagged_emission_sites: usize,
    compat_tagged_files: BTreeMap<String, usize>,
}

#[derive(Default, Debug)]
struct HookStats {
    total: usize,
    names: Vec<String>,
}

#[derive(Default, Debug)]
#[allow(clippy::struct_field_names)]
struct PlanStats {
    logical_refs: usize,
    physical_refs: usize,
    statement_refs: usize,
}

/// Task 19: deeper compat-debt surface - shadow dispatch count,
/// duplicate no-op validators, retired matrix comments, and typed
/// migration landing pads so one report can read "how much compat logic is
/// still orbital" at a glance.
#[derive(Default, Debug)]
struct CommandEnumStats {
    total_variants: usize,
    all_active: usize,
    retired_enum_variants: usize,
}

#[derive(Default, Debug)]
struct MigrationStats {
    shadow_dispatch_sites: usize,
    legacy_compat_tagged_materializers: usize,
    pg_object_command_refs: usize,
    pg_compat_utility_refs: usize,
}

#[derive(Default, Debug)]
struct ValidatorStats {
    canonical_noop_validation_defs: usize,
    duplicate_noop_validation_defs: usize,
    duplicate_files: BTreeMap<String, usize>,
}

#[derive(Default, Debug)]
struct TestStats {
    test_functions: usize,
    files: BTreeMap<String, usize>,
}

pub fn run(opts: CompatDebtReportOptions) -> Result<(), String> {
    let repo_root = repo_root();

    let matrix = scan_matrix(&repo_root)?;
    let parser = scan_parser_sites(&repo_root)?;
    let hooks = scan_compat_hooks(&repo_root)?;
    let plans = scan_plan_refs(&repo_root)?;
    let tests = scan_compat_tests(&repo_root)?;
    let command_enum = scan_command_enum(&repo_root)?;
    let migration = scan_migration_refs(&repo_root)?;
    let validators = scan_noop_validators(&repo_root)?;

    if opts.json {
        print_json(
            &matrix,
            &parser,
            &hooks,
            &plans,
            &tests,
            &command_enum,
            &migration,
            &validators,
        );
    } else {
        print_human(
            &matrix,
            &parser,
            &hooks,
            &plans,
            &tests,
            &command_enum,
            &migration,
            &validators,
        );
    }
    Ok(())
}

/// Parse the `CompatCommand` enum at `crates/aiondb-pg-compat/src/command.rs`
/// and count: total enum variants, `ALL` (active baseline), and the derived
/// "retired enum variants" count (variants absent from `ALL`).
fn scan_command_enum(root: &Path) -> Result<CommandEnumStats, String> {
    let path = root.join("crates/aiondb-pg-compat/src/command.rs");
    let src = fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let total_variants = count_enum_variants(&src, "pub enum CompatCommand {");
    let all_active = count_list_variants(&src, "pub const ALL:");
    let retired_enum_variants = total_variants.saturating_sub(all_active);
    Ok(CommandEnumStats {
        total_variants,
        all_active,
        retired_enum_variants,
    })
}

fn scan_migration_refs(root: &Path) -> Result<MigrationStats, String> {
    let crates = root.join("crates");
    let mut stats = MigrationStats::default();
    walk_rust(&crates, &mut |path, src| {
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .display()
            .to_string();
        if rel.ends_with("crates/aiondb-engine/src/engine/compat_router.rs") {
            stats.shadow_dispatch_sites += count_occurrences(src, "CompatStatement {");
            stats.legacy_compat_tagged_materializers +=
                count_occurrences(src, "Statement::CompatTagged(")
                    + count_occurrences(src, "Statement::CompatTaggedNotice(");
        }
        stats.pg_object_command_refs += count_occurrences(src, "PgObjectCommand");
        stats.pg_compat_utility_refs += count_occurrences(src, "PgCompatUtility");
    })?;
    Ok(stats)
}

fn scan_noop_validators(root: &Path) -> Result<ValidatorStats, String> {
    let crates = root.join("crates");
    let canonical = "crates/aiondb-pg-compat/src/noop_validation.rs";
    let needles = [
        "fn reject_invalid_noop_statement",
        "fn reject_invalid_noop_statement_sql",
        "fn unsupported_compatibility_command",
        "fn is_allowlisted_noop_tag",
        "fn is_supported_alter_table_noop",
    ];
    let mut stats = ValidatorStats::default();
    walk_rust(&crates, &mut |path, src| {
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .display()
            .to_string();
        let hits: usize = needles
            .iter()
            .map(|needle| count_occurrences(src, needle))
            .sum();
        if hits == 0 {
            return;
        }
        if rel == canonical {
            stats.canonical_noop_validation_defs += hits;
        } else {
            stats.duplicate_noop_validation_defs += hits;
            stats.duplicate_files.insert(rel, hits);
        }
    })?;
    Ok(stats)
}

/// Count variants in a top-level enum. Text-scan: counts lines that look
/// like `    Ident(...),` or `    Ident,` inside the block bounded by
/// `pub enum <Name> {` .. `}`.
fn count_enum_variants(src: &str, header: &str) -> usize {
    let Some(start) = src.find(header) else {
        return 0;
    };
    let after = &src[start + header.len()..];
    let Some(end) = find_matching_close_brace(after) else {
        return 0;
    };
    let body = &after[..end];
    let mut count = 0usize;
    for raw in body.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("//") || line.starts_with("///") {
            continue;
        }
        // A variant line starts with an uppercase letter and either ends in
        // `,` or introduces a struct-like variant with `{` / `(`.
        let first = match line.chars().next() {
            Some(c) => c,
            None => continue,
        };
        if !first.is_ascii_uppercase() {
            continue;
        }
        count += 1;
    }
    count
}

/// Count `Self::Ident` entries in a `pub const NAME: ... = &[ ... ]` slice.
fn count_list_variants(src: &str, header: &str) -> usize {
    let Some(start) = src.find(header) else {
        return 0;
    };
    let after = &src[start + header.len()..];
    let Some(close_idx) = after.find("];") else {
        return 0;
    };
    let body = &after[..close_idx];
    let mut count = 0usize;
    let mut rest = body;
    while let Some(idx) = rest.find("Self::") {
        count += 1;
        rest = &rest[idx + "Self::".len()..];
    }
    count
}

/// Walk forward tracking brace depth; return the index of the `}` that
/// closes the block the slice starts inside (i.e. after the enum header).
fn find_matching_close_brace(body: &str) -> Option<usize> {
    let mut depth = 1i32;
    for (i, ch) in body.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn repo_root() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p
}

/// Probe the two possible matrix locations: the legacy engine-internal
/// one and the post-migration pg-compat one. The first match wins so the
/// tool keeps working while the source moves.
fn matrix_candidates(root: &Path) -> Vec<PathBuf> {
    vec![
        root.join("crates/aiondb-pg-compat/src/compat_tag_matrix.rs"),
        root.join("crates/aiondb-engine/src/engine/compat_tag_matrix.rs"),
    ]
}

fn scan_matrix(root: &Path) -> Result<MatrixStats, String> {
    let candidates = matrix_candidates(root);
    let Some(path) = candidates.into_iter().find(|p| p.exists()) else {
        return Err("COMPAT_TAG_MATRIX source file not found in engine or pg-compat".into());
    };
    let src = fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let mut stats = MatrixStats {
        source_path: path
            .strip_prefix(root)
            .unwrap_or(&path)
            .display()
            .to_string(),
        ..MatrixStats::default()
    };
    for raw in src.lines() {
        let line = raw.trim();
        stats.retired_tag_comments += count_retired_tags_from_comment(line);
        if let Some(rest) = line.strip_prefix("behavior: CompatTagBehavior::") {
            let bucket: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphabetic())
                .collect();
            match bucket.as_str() {
                "ImplementedReal" => stats.implemented_real += 1,
                "ExplicitNotSupported" => stats.explicit_not_supported += 1,
                "IntentionalNoop" => stats.intentional_noop += 1,
                _ => {}
            }
        }
    }
    stats.total = stats.implemented_real + stats.explicit_not_supported + stats.intentional_noop;
    if stats.total == 0 {
        return Err(format!(
            "no matrix entries parsed from {} — tag matrix parser drifted",
            path.display()
        ));
    }
    Ok(stats)
}

fn count_retired_tags_from_comment(line: &str) -> usize {
    let Some(comment) = line.strip_prefix("//") else {
        return 0;
    };
    let lower = comment.to_ascii_lowercase();
    if !lower.contains("retired") {
        return 0;
    }
    if let Some(tags_idx) = lower.find(" tags)") {
        let before = &lower[..tags_idx];
        let digits_rev: String = before
            .chars()
            .rev()
            .skip_while(|c| c.is_ascii_whitespace())
            .take_while(|c| c.is_ascii_digit())
            .collect();
        let digits: String = digits_rev.chars().rev().collect();
        if let Ok(value) = digits.parse::<usize>() {
            return value;
        }
    }
    if lower.contains(" tag)") {
        return 1;
    }
    1
}

fn scan_parser_sites(root: &Path) -> Result<ParserStats, String> {
    let parser_src = root.join("crates/aiondb-parser/src");
    let mut stats = ParserStats::default();
    walk_rust(&parser_src, &mut |path, src| {
        // Skip test modules: we care about *emission* sites in production code.
        if is_test_file(path) {
            return;
        }
        let hits = count_occurrences(src, "Statement::CommandNoOp {");
        if hits > 0 {
            stats.emission_sites += hits;
            stats.files.insert(
                path.strip_prefix(root)
                    .unwrap_or(path)
                    .display()
                    .to_string(),
                hits,
            );
        }
        let compat_tagged_hits = count_occurrences(src, "Statement::CompatTagged(")
            + count_occurrences(src, "Statement::CompatTaggedNotice(");
        if compat_tagged_hits > 0 {
            stats.compat_tagged_emission_sites += compat_tagged_hits;
            stats.compat_tagged_files.insert(
                path.strip_prefix(root)
                    .unwrap_or(path)
                    .display()
                    .to_string(),
                compat_tagged_hits,
            );
        }
    })?;
    Ok(stats)
}

fn scan_compat_hooks(root: &Path) -> Result<HookStats, String> {
    let engine_src = root.join("crates/aiondb-engine/src");
    let mut names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    walk_rust(&engine_src, &mut |_, src| {
        for line in src.lines() {
            let trimmed = line.trim();
            let needle = "fn try_execute_compat_";
            if let Some(start) = trimmed.find(needle) {
                let after = &trimmed[start + needle.len()..];
                let end = after
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .unwrap_or(after.len());
                if end > 0 {
                    names.insert(format!("try_execute_compat_{}", &after[..end]));
                }
            }
        }
    })?;
    let mut hooks: Vec<String> = names.into_iter().collect();
    hooks.sort();
    Ok(HookStats {
        total: hooks.len(),
        names: hooks,
    })
}

fn scan_plan_refs(root: &Path) -> Result<PlanStats, String> {
    let crates = root.join("crates");
    let mut stats = PlanStats::default();
    walk_rust(&crates, &mut |_, src| {
        stats.logical_refs += count_occurrences(src, "LogicalPlan::CommandNoOp");
        stats.physical_refs += count_occurrences(src, "PhysicalPlan::CommandNoOp");
        stats.statement_refs += count_occurrences(src, "Statement::CommandNoOp");
    })?;
    Ok(stats)
}

fn scan_compat_tests(root: &Path) -> Result<TestStats, String> {
    let crates = root.join("crates");
    let mut stats = TestStats::default();
    walk_rust(&crates, &mut |path, src| {
        let count = count_compat_test_fns(src);
        if count > 0 {
            stats.test_functions += count;
            stats.files.insert(
                path.strip_prefix(root)
                    .unwrap_or(path)
                    .display()
                    .to_string(),
                count,
            );
        }
    })?;
    Ok(stats)
}

/// Count `#[test]` / `#[tokio::test]` functions whose body mentions a
/// compat marker. Text-based, good enough for a KPI snapshot (we do not
/// parse Rust).
fn count_compat_test_fns(src: &str) -> usize {
    let lines: Vec<&str> = src.lines().collect();
    let mut count = 0usize;
    let mut i = 0usize;
    while i < lines.len() {
        let trimmed = lines[i].trim();
        let is_test_attr = trimmed == "#[test]"
            || trimmed == "#[tokio::test]"
            || trimmed.starts_with("#[tokio::test(");
        if !is_test_attr {
            i += 1;
            continue;
        }
        // Find the next `fn ` line, then the function body span.
        let mut j = i + 1;
        while j < lines.len()
            && !lines[j].trim_start().starts_with("fn ")
            && !lines[j].trim_start().starts_with("async fn ")
            && !lines[j].trim_start().starts_with("pub fn ")
        {
            j += 1;
        }
        if j >= lines.len() {
            break;
        }
        let body_start = j;
        // Walk forward tracking brace balance to find the end of the body.
        let mut depth = 0i32;
        let mut started = false;
        let mut end = body_start;
        for (k, line) in lines.iter().enumerate().skip(body_start) {
            for ch in line.chars() {
                match ch {
                    '{' => {
                        depth += 1;
                        started = true;
                    }
                    '}' => depth -= 1,
                    _ => {}
                }
            }
            if started && depth <= 0 {
                end = k;
                break;
            }
        }
        let body = lines[body_start..=end.min(lines.len().saturating_sub(1))].join("\n");
        if body.contains("CommandNoOp") || body.contains("try_execute_compat_") {
            count += 1;
        }
        i = end + 1;
    }
    count
}

fn walk_rust(dir: &Path, visit: &mut dyn FnMut(&Path, &str)) -> Result<(), String> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if dir.exists() => return Err(format!("reading {}: {e}", dir.display())),
        Err(_) => return Ok(()),
    };
    for entry in entries {
        let entry = entry.map_err(|e| format!("reading entry: {e}"))?;
        let path = entry.path();
        if path.is_dir() {
            // Skip `target/` and hidden dirs to keep the walk cheap.
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name == "target" || name.starts_with('.') {
                    continue;
                }
            }
            walk_rust(&path, visit)?;
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("rs") {
            continue;
        }
        let src =
            fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
        visit(&path, &src);
    }
    Ok(())
}

fn is_test_file(path: &Path) -> bool {
    path.components()
        .any(|c| matches!(c.as_os_str().to_str(), Some("tests" | "test")))
}

fn count_occurrences(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let mut count = 0usize;
    let mut rest = haystack;
    while let Some(idx) = rest.find(needle) {
        count += 1;
        rest = &rest[idx + needle.len()..];
    }
    count
}

fn print_human(
    matrix: &MatrixStats,
    parser: &ParserStats,
    hooks: &HookStats,
    plans: &PlanStats,
    tests: &TestStats,
    command_enum: &CommandEnumStats,
    migration: &MigrationStats,
    validators: &ValidatorStats,
) {
    println!("== compat-debt-report ==");
    println!("source : {}", matrix.source_path);
    println!(
        "COMPAT_TAG_MATRIX : {} entries (implemented_real={}, intentional_noop={}, explicit_not_supported={}, retired_tag_comments={})",
        matrix.total,
        matrix.implemented_real,
        matrix.intentional_noop,
        matrix.explicit_not_supported,
        matrix.retired_tag_comments,
    );
    println!(
        "CompatCommand enum : {} variants (ALL active={}, retired_enum_variants={})",
        command_enum.total_variants, command_enum.all_active, command_enum.retired_enum_variants,
    );
    println!(
        "parser CommandNoOp emission sites : {} across {} file(s)",
        parser.emission_sites,
        parser.files.len()
    );
    for (path, hits) in &parser.files {
        println!("  {path} : {hits}");
    }
    println!(
        "parser CompatTagged emission sites : {} across {} file(s)",
        parser.compat_tagged_emission_sites,
        parser.compat_tagged_files.len()
    );
    for (path, hits) in &parser.compat_tagged_files {
        println!("  compat-tagged {path} : {hits}");
    }
    println!("engine try_execute_compat_* hooks : {}", hooks.total);
    println!(
        "plan references : Statement::CommandNoOp={}, LogicalPlan::CommandNoOp={}, PhysicalPlan::CommandNoOp={}",
        plans.statement_refs, plans.logical_refs, plans.physical_refs
    );
    println!(
        "migration landing pads : PgObjectCommand refs={}, PgCompatUtility refs={}, shadow-dispatch sites={}, legacy CompatTagged materializers={}",
        migration.pg_object_command_refs,
        migration.pg_compat_utility_refs,
        migration.shadow_dispatch_sites,
        migration.legacy_compat_tagged_materializers,
    );
    println!(
        "noop validators : canonical_defs={}, duplicate_defs={} across {} file(s)",
        validators.canonical_noop_validation_defs,
        validators.duplicate_noop_validation_defs,
        validators.duplicate_files.len()
    );
    for (path, hits) in &validators.duplicate_files {
        println!("  duplicate {path} : {hits}");
    }
    println!(
        "associated tests : {} test functions across {} file(s)",
        tests.test_functions,
        tests.files.len()
    );
    for (path, hits) in &tests.files {
        println!("  {path} : {hits}");
    }
}

fn print_json(
    matrix: &MatrixStats,
    parser: &ParserStats,
    hooks: &HookStats,
    plans: &PlanStats,
    tests: &TestStats,
    command_enum: &CommandEnumStats,
    migration: &MigrationStats,
    validators: &ValidatorStats,
) {
    let mut out = String::from("{\n");
    out.push_str(&format!(
        "  \"matrix\": {{\"total\": {}, \"implemented_real\": {}, \"intentional_noop\": {}, \"explicit_not_supported\": {}, \"retired_tag_comments\": {}, \"source\": \"{}\"}},\n",
        matrix.total,
        matrix.implemented_real,
        matrix.intentional_noop,
        matrix.explicit_not_supported,
        matrix.retired_tag_comments,
        escape(&matrix.source_path)
    ));
    out.push_str(&format!(
        "  \"command_enum\": {{\"total_variants\": {}, \"all_active\": {}, \"retired_enum_variants\": {}}},\n",
        command_enum.total_variants,
        command_enum.all_active,
        command_enum.retired_enum_variants,
    ));
    out.push_str(&format!(
        "  \"parser_noop_sites\": {{\"total\": {}, \"files\": {}}},\n",
        parser.emission_sites,
        json_map(&parser.files)
    ));
    out.push_str(&format!(
        "  \"parser_compat_tagged_sites\": {{\"total\": {}, \"files\": {}}},\n",
        parser.compat_tagged_emission_sites,
        json_map(&parser.compat_tagged_files)
    ));
    out.push_str(&format!(
        "  \"compat_hooks\": {{\"total\": {}, \"names\": {}}},\n",
        hooks.total,
        json_array(&hooks.names)
    ));
    out.push_str(&format!(
        "  \"plan_refs\": {{\"statement\": {}, \"logical\": {}, \"physical\": {}}},\n",
        plans.statement_refs, plans.logical_refs, plans.physical_refs
    ));
    out.push_str(&format!(
        "  \"migration\": {{\"pg_object_command_refs\": {}, \"pg_compat_utility_refs\": {}, \"shadow_dispatch_sites\": {}, \"legacy_compat_tagged_materializers\": {}}},\n",
        migration.pg_object_command_refs,
        migration.pg_compat_utility_refs,
        migration.shadow_dispatch_sites,
        migration.legacy_compat_tagged_materializers,
    ));
    out.push_str(&format!(
        "  \"noop_validators\": {{\"canonical_defs\": {}, \"duplicate_defs\": {}, \"duplicate_files\": {}}},\n",
        validators.canonical_noop_validation_defs,
        validators.duplicate_noop_validation_defs,
        json_map(&validators.duplicate_files)
    ));
    out.push_str(&format!(
        "  \"tests\": {{\"total\": {}, \"files\": {}}}\n",
        tests.test_functions,
        json_map(&tests.files)
    ));
    out.push('}');
    println!("{out}");
}

fn json_map(map: &BTreeMap<String, usize>) -> String {
    let mut out = String::from("{");
    let mut first = true;
    for (k, v) in map {
        if !first {
            out.push_str(", ");
        }
        first = false;
        out.push_str(&format!("\"{}\": {v}", escape(k)));
    }
    out.push('}');
    out
}

fn json_array(values: &[String]) -> String {
    let mut out = String::from("[");
    let mut first = true;
    for v in values {
        if !first {
            out.push_str(", ");
        }
        first = false;
        out.push_str(&format!("\"{}\"", escape(v)));
    }
    out.push(']');
    out
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_occurrences_non_overlapping() {
        assert_eq!(count_occurrences("aaaaa", "aa"), 2);
        assert_eq!(count_occurrences("abc", "z"), 0);
        assert_eq!(count_occurrences("", "x"), 0);
        assert_eq!(count_occurrences("anything", ""), 0);
    }

    #[test]
    fn counts_retired_tags_from_matrix_comments() {
        assert_eq!(
            count_retired_tags_from_comment("// DATABASE (3 tags) retired 2026-04-23"),
            3
        );
        assert_eq!(
            count_retired_tags_from_comment("// CREATE COLLATION retired 2026-04-24"),
            1
        );
        assert_eq!(count_retired_tags_from_comment("// active entry"), 0);
    }

    #[test]
    fn counts_compat_test_fns_simple() {
        let src = r#"
            #[test]
            fn a() { let _ = "CommandNoOp"; }

            #[test]
            fn b() { let _ = 1; }

            #[tokio::test]
            async fn c() { try_execute_compat_foo(); }
        "#;
        assert_eq!(count_compat_test_fns(src), 2);
    }

    #[test]
    fn detects_test_paths() {
        assert!(is_test_file(Path::new("crates/x/src/tests/foo.rs")));
        assert!(is_test_file(Path::new("crates/x/tests/bar.rs")));
        assert!(!is_test_file(Path::new("crates/x/src/lib.rs")));
    }
}
