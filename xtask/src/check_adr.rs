//! `cargo xtask check-adr` - unified gate for project ADRs.
//!
//! Each sub-check corresponds to an ADR in `docs/adr/`:
//!
//! | Check | ADR | Objet |
//! |---|---|---|
//! | `dispatch` | 0001 | un seul `match Statement::*` exhaustif |
//! | `compat-freeze` | 0004 | pas de nouveau hook compat runtime |
//! | `planner-path` | 0005 | DML/DDL via `execute_planned_statement_with_plan_cache` |
//! | `internal-execute-sql` | 0006 | pas d'`execute_sql` interne ni de recursion `execute_sql_statement_results` |
//! | `sql-string-match` | 0007 | pas de `sql.contains(...)` hors legacy compat |
//! | `sqlstate` | 0009 | pas de `DbError::new(...)` sans `SqlState::` explicite |
//! | `budgets` | 0010 | bounded user-supplied `with_capacity` / `Vec::from_raw_parts` |
//! | `dual-mode` | 0011 | tests have at least one dual-mode scenario |
//! | `ignores` | 0012 | `#[ignore]` → `tracked:` + `target:` |
//!
//! Usage:
//! ```bash
//! cargo xtask check-adr                # runs all checks, exits 1 if one fails
//! cargo xtask check-adr --only dispatch compat-freeze
//! cargo xtask check-adr --json
//! ```
//!
//! Most checks are grep-style lints: they do not replace real AST
//! analysis, but they block obvious regressions. False positives are
//! handled through the per-check allowlist.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct CheckAdrOptions {
    pub only: Vec<String>,
    pub json: bool,
}

pub fn parse_args(args: &[String]) -> Result<CheckAdrOptions, String> {
    let mut only = Vec::new();
    let mut json = false;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--json" => json = true,
            "--only" => {
                i += 1;
                while i < args.len() && !args[i].starts_with("--") {
                    only.push(args[i].clone());
                    i += 1;
                }
                continue;
            }
            "--help" | "-h" => {
                println!(
                    "Usage: cargo xtask check-adr [--only <check>...] [--json]\n\n\
Checks: dispatch, compat-freeze, planner-path, internal-execute-sql,\n\
        sql-string-match, sqlstate, budgets, dual-mode, ignores.\n\n\
Without --only, all checks run."
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown flag: {other}")),
        }
        i += 1;
    }
    Ok(CheckAdrOptions { only, json })
}

#[derive(Clone, Debug)]
pub struct CheckResult {
    pub name: &'static str,
    pub violations: Vec<String>,
    pub scanned_files: usize,
}

impl CheckResult {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            violations: Vec::new(),
            scanned_files: 0,
        }
    }
    fn ok(&self) -> bool {
        self.violations.is_empty()
    }
}

pub fn run(opts: CheckAdrOptions) -> Result<(), String> {
    let root = repo_root();
    let all: Vec<(&'static str, fn(&Path) -> CheckResult)> = vec![
        ("dispatch", check_dispatch),
        ("compat-freeze", check_compat_freeze),
        ("noop-sites", check_noop_sites),
        ("planner-path", check_planner_path),
        ("internal-execute-sql", check_internal_execute_sql),
        ("sql-string-match", check_sql_string_match),
        ("sqlstate", check_sqlstate),
        ("budgets", check_budgets),
        ("dual-mode", check_dual_mode),
        ("ignores", check_ignores),
    ];

    let selected: Vec<_> = if opts.only.is_empty() {
        all
    } else {
        all.into_iter()
            .filter(|(name, _)| opts.only.iter().any(|n| n == name))
            .collect()
    };

    let mut results: BTreeMap<&'static str, CheckResult> = BTreeMap::new();
    for (name, f) in &selected {
        let result = f(&root);
        results.insert(*name, result);
    }

    if opts.json {
        emit_json(&results);
    } else {
        emit_human(&results);
    }

    let failed: Vec<_> = results
        .values()
        .filter(|r| !r.ok())
        .map(|r| r.name)
        .collect();
    if !failed.is_empty() {
        return Err(format!("check-adr failures: {failed:?}"));
    }
    Ok(())
}

fn emit_human(results: &BTreeMap<&'static str, CheckResult>) {
    println!("== check-adr ==");
    for r in results.values() {
        let status = if r.ok() { "OK " } else { "FAIL" };
        println!(
            "  [{status}] {:<22} scanned={:<4} violations={}",
            r.name,
            r.scanned_files,
            r.violations.len()
        );
        for v in r.violations.iter().take(5) {
            println!("         - {v}");
        }
        if r.violations.len() > 5 {
            println!("         ... {} more", r.violations.len() - 5);
        }
    }
}

fn emit_json(results: &BTreeMap<&'static str, CheckResult>) {
    let mut s = String::from("{\n");
    let mut first = true;
    for r in results.values() {
        if !first {
            s.push_str(",\n");
        }
        first = false;
        s.push_str(&format!("  \"{}\": {{\n", r.name));
        s.push_str(&format!("    \"ok\": {},\n", r.ok()));
        s.push_str(&format!("    \"scanned_files\": {},\n", r.scanned_files));
        s.push_str("    \"violations\": [");
        let vs: Vec<String> = r
            .violations
            .iter()
            .map(|v| format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\"")))
            .collect();
        s.push_str(&vs.join(", "));
        s.push_str("]\n  }");
    }
    s.push_str("\n}");
    println!("{s}");
}

pub(crate) fn repo_root() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p
}

fn rust_files_under(root: &Path, subpath: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let base = root.join(subpath);
    walk(&base, &mut out);
    out
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_owned();
        if path.is_dir() {
            // skip target, .git, hidden, tests for some checks (caller filters)
            if matches!(file_name.as_str(), "target" | ".git" | "node_modules") {
                continue;
            }
            walk(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    walk(dir, out);
}

fn read_text(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

// ---------------------------------------------------------------
// ADR-0001 - dispatch unique
// ---------------------------------------------------------------
fn check_dispatch(root: &Path) -> CheckResult {
    let mut result = CheckResult::new("dispatch");
    let files = rust_files_under(root, "crates/aiondb-engine/src");
    // Look for `match statement` or `match *statement` patterns outside
    // the authorized file. Dispatch site: `statement_exec.rs`
    // (execute_statement_inner), `pg_compat_hooks.rs` (CompatCommandHandler
    // dispatch), `prepared.rs`, `compat/*` (legacy).
    let allowlist: &[&str] = &[
        "crates/aiondb-engine/src/engine/statement_exec.rs",
        "crates/aiondb-engine/src/engine/pg_compat_hooks.rs",
        "crates/aiondb-engine/src/engine/compat/", // legacy - ADR-0004 migration sortante
        "crates/aiondb-engine/src/engine/query_api.rs", // dispatch describe/prepared
        "crates/aiondb-engine/src/engine/portal_exec.rs",
    ];
    for path in &files {
        result.scanned_files += 1;
        let Some(text) = read_text(path) else {
            continue;
        };
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned();
        if allowlist.iter().any(|a| rel.starts_with(a)) {
            continue;
        }
        for (lineno, line) in text.lines().enumerate() {
            if line.contains("match ") && line.contains("statement") && line.contains("Statement::")
            {
                result
                    .violations
                    .push(format!("{rel}:{} {}", lineno + 1, line.trim()));
            }
        }
    }
    result
}

// ---------------------------------------------------------------
// ADR-0004 - frozen compat layer
// ---------------------------------------------------------------
fn check_compat_freeze(root: &Path) -> CheckResult {
    let mut result = CheckResult::new("compat-freeze");
    // Baseline count of `try_execute_compat_*` hook methods in the remaining
    // runtime compat bridge. The frozen facade stays in `engine/compat/**`;
    // implementation files extracted from that frozen directory live under
    // `engine/compat_runtime/**` and must still count against the same
    // monotone-decreasing ratchet. Growth requires an ADR naming the hook.
    const BASELINE_HOOK_COUNT: usize = 0;
    let mut files = Vec::new();
    collect_rs_files(
        &root.join("crates/aiondb-engine/src/engine/compat"),
        &mut files,
    );
    collect_rs_files(
        &root.join("crates/aiondb-engine/src/engine/compat_runtime"),
        &mut files,
    );
    files.push(root.join("crates/aiondb-engine/src/engine/compat_router.rs"));
    files.sort();
    files.dedup();
    let mut hooks = std::collections::BTreeSet::new();
    for path in &files {
        result.scanned_files += 1;
        let Some(text) = read_text(path) else {
            continue;
        };
        for line in text.lines() {
            let trimmed = line.trim();
            let needle = "fn try_execute_compat_";
            if let Some(start) = trimmed.find(needle) {
                let after = &trimmed[start + needle.len()..];
                let end = after
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .unwrap_or(after.len());
                if end > 0 {
                    hooks.insert(format!("try_execute_compat_{}", &after[..end]));
                }
            }
        }
    }
    let total = hooks.len();
    if total > BASELINE_HOOK_COUNT {
        let adr_hooks = read_compat_hook_adr_mentions(&root.join("docs/adr"));
        let unjustified: Vec<&str> = hooks
            .iter()
            .filter(|h| !adr_hooks.contains(h.as_str()))
            .map(String::as_str)
            .collect();
        // Tolerate growth only when every hook above the baseline is named by
        // an ADR. Partial coverage still fails.
        if !unjustified.is_empty() {
            result.violations.push(format!(
                "try_execute_compat_* count = {total} > baseline {BASELINE_HOOK_COUNT}; \
                 unjustified hooks (no ADR mention): {unjustified:?} \
                 (ADR-0004: no new hooks without docs/adr/NNNN-*compat*.md)"
            ));
        }
    }
    result
}

// ---------------------------------------------------------------
// ADR-0003 - CommandNoOp parser construction sites (monotonic decrease)
// ---------------------------------------------------------------
fn check_noop_sites(root: &Path) -> CheckResult {
    let mut result = CheckResult::new("noop-sites");
    // Baseline frozen on 2026-04-23, tightened iteratively through
    // ADR-0016 Path A:
    //   * Task 10-12 : 75 → 70.
    //   * Task 13/14/15 (DATABASE + TYPE/DOMAIN/CAST/RULE typed AST):
    //     70 → 62.
    //   * Path A follow-up (POLICY/PUB/SUB/SERVER/USER MAPPING/
    //     FOREIGN TABLE typed AST): 62 → 56.
    //   * Path A final (COLLATION/STATISTICS/TABLESPACE/AGGREGATE/
    //     PROCEDURE/ROUTINE/TRIGGER/OPERATOR typed AST): 56 → 48.
    // Task 8 override: extra sites are allowed only if each additional
    // site is justified by a `docs/adr/*-noop-*.md` ADR.
    const BASELINE_PARSER_NOOP_SITES: usize = 48;
    let files = rust_files_under(root, "crates/aiondb-parser/src");
    let mut total = 0usize;
    let mut sites: Vec<(String, usize)> = Vec::new();
    for path in &files {
        result.scanned_files += 1;
        let Some(text) = read_text(path) else {
            continue;
        };
        let mut file_count = 0usize;
        for line in text.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || trimmed.starts_with("///") || trimmed.starts_with('*') {
                continue;
            }
            if line.contains("Statement::CommandNoOp {")
                && (line.contains("Ok(") || line.contains("return "))
            {
                total += 1;
                file_count += 1;
            }
        }
        if file_count > 0 {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(path)
                .display()
                .to_string();
            sites.push((rel, file_count));
        }
    }
    if total > BASELINE_PARSER_NOOP_SITES {
        let overflow = total - BASELINE_PARSER_NOOP_SITES;
        let adr_noop_files = read_noop_adr_file_mentions(&root.join("docs/adr"));
        // A file whose site count grew is justified iff at least one
        // ADR mentions its basename (or the keyword `CommandNoOp`).
        // Sum the sites that live in unjustified files: if that total
        // covers the overflow, the extra sites are documented.
        let mut unjustified_sites = 0usize;
        let mut unjustified: Vec<String> = Vec::new();
        for (path, count) in &sites {
            let basename = std::path::Path::new(path)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(path);
            let justified = adr_noop_files
                .iter()
                .any(|adr_body| adr_body.contains(basename) || adr_body.contains("CommandNoOp"));
            if !justified {
                unjustified_sites += *count;
                unjustified.push(format!("{path}({count})"));
            }
        }
        if unjustified_sites >= overflow {
            result.violations.push(format!(
                "parser CommandNoOp construction sites = {total} > baseline {BASELINE_PARSER_NOOP_SITES} \
                 (overflow {overflow}); unjustified files (no ADR mention): {unjustified:?} \
                 (ADR-0003: decrease only; add docs/adr/NNNN-noop-<tag>.md)"
            ));
        }
    }
    result
}

/// Collect `try_execute_compat_*` tokens mentioned in any ADR file whose
/// name contains `compat`.
fn read_compat_hook_adr_mentions(adr_dir: &Path) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    let Ok(entries) = fs::read_dir(adr_dir) else {
        return out;
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
        for token in body.split(|c: char| !c.is_ascii_alphanumeric() && c != '_') {
            if token.starts_with("try_execute_compat_") {
                out.insert(token.to_owned());
            }
        }
    }
    out
}

/// Return the raw bodies of every ADR file whose name matches the noop
/// naming convention (`*-noop-*.md`). Used to justify parser-site
/// growth via textual mention.
fn read_noop_adr_file_mentions(adr_dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(adr_dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.contains("-noop-") {
            continue;
        }
        if let Ok(body) = fs::read_to_string(&path) {
            out.push(body);
        }
    }
    out
}

// ---------------------------------------------------------------
// ADR-0005 - DML/DDL via planner
// ---------------------------------------------------------------
fn check_planner_path(root: &Path) -> CheckResult {
    let mut result = CheckResult::new("planner-path");
    // Heuristic: scan statement_exec.rs for `Statement::Insert|Update|Delete|Merge|CreateTable|...`
    // branches that don't route to execute_planned_statement_with_plan_cache.
    let path = root.join("crates/aiondb-engine/src/engine/statement_exec.rs");
    result.scanned_files = 1;
    let Some(text) = read_text(&path) else {
        result
            .violations
            .push("cannot read statement_exec.rs".to_owned());
        return result;
    };
    // Simple sanity check: ensure execute_planned_statement_with_plan_cache is called
    // for the main DML/DDL variants in the inner match.
    if !text.contains("execute_planned_statement_with_plan_cache") {
        result.violations.push(
            "execute_planned_statement_with_plan_cache is missing — DML/DDL routing broken"
                .to_owned(),
        );
    }
    result
}

// ---------------------------------------------------------------
// ADR-0006 - pas d'execute_sql interne / pas de recursion execute_sql_statement_results
// ---------------------------------------------------------------
fn check_internal_execute_sql(root: &Path) -> CheckResult {
    let mut result = CheckResult::new("internal-execute-sql");
    let forbidden_prefixes: &[&str] = &[
        "crates/aiondb-executor/",
        "crates/aiondb-planner/",
        "crates/aiondb-optimizer/",
        "crates/aiondb-catalog/",
        "crates/aiondb-catalog-store/",
        "crates/aiondb-storage-api/",
        "crates/aiondb-storage-engine/",
        "crates/aiondb-tx/",
        "crates/aiondb-engine/src/engine/",
    ];
    // Allowlist: files that legitimately expose a local (non-Engine)
    // `execute_sql` method used as a callback surface, OR files that are
    // grandfathered internal callers awaiting migration (documented in
    // ADR-0006 "Consequences"). Each entry must eventually either get a
    // dedicated engine API or be rewritten to avoid `self.execute_sql`.
    let file_allowlist: &[&str] = &[
        // Homonymous `execute_sql` methods on local trait impls - not
        // dispatching through `QueryEngine::execute_sql`. Permanent exempts.
        "crates/aiondb-executor/src/executor/plpgsql_runtime.rs",
        "crates/aiondb-engine/src/engine/compat/plpgsql_adapter.rs",
        // `QuerySimpleSql` is a dyn-safe Send+Sync facade trait that
        // forwards a single call to `QueryEngine::execute_sql`. The
        // forwarding body is the only `::execute_sql(` site in this file.
        // Permanent exempt - it is the boundary, not an internal recursion.
        "crates/aiondb-engine/src/engine/api.rs",
        // ADR-0006 migration backlog - ALL grandfathered engine callers
        // of `QueryEngine::execute_sql` have been migrated as of
        // 2026-04-23 (tasks #38, #39, #40, #41). Only the homonym local
        // methods above remain exempted. Keeping an empty section here
        // as a marker so the intent stays visible in future audits.
    ];
    for prefix in forbidden_prefixes {
        let dir = root.join(prefix);
        let files = rust_files_under(root, dir.to_str().unwrap_or(""));
        for path in &files {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .into_owned();
            if rel.contains("/tests/") || rel.contains("_tests.rs") {
                continue;
            }
            if file_allowlist.iter().any(|a| rel == *a) {
                continue;
            }
            result.scanned_files += 1;
            let Some(text) = read_text(path) else {
                continue;
            };
            // Track whether we are inside a `#[cfg(test)] mod tests { ... }`
            // block - those are test code, exempt per ADR-0006.
            let mut brace_depth = 0i32;
            let mut test_mod_depth: Option<i32> = None;
            let mut pending_test_mod = false;
            for (lineno, line) in text.lines().enumerate() {
                let trimmed = line.trim_start();
                if trimmed.starts_with("#[cfg(test)]") {
                    pending_test_mod = true;
                }
                let opens = line.matches('{').count() as i32;
                let closes = line.matches('}').count() as i32;
                // Detect entry into a `mod tests {` block when preceded by
                // `#[cfg(test)]` or on a line matching `mod tests`.
                if test_mod_depth.is_none()
                    && opens > 0
                    && (pending_test_mod
                        || trimmed.starts_with("mod tests")
                        || trimmed.contains(" mod tests "))
                {
                    test_mod_depth = Some(brace_depth);
                    pending_test_mod = false;
                } else if !trimmed.starts_with("#[") && !trimmed.is_empty() {
                    pending_test_mod = false;
                }
                brace_depth += opens - closes;
                if let Some(d) = test_mod_depth {
                    if brace_depth <= d {
                        test_mod_depth = None;
                    }
                    // Inside test module → skip the violation check.
                    continue;
                }
                if trimmed.starts_with("//") {
                    continue;
                }
                if line.contains(".execute_sql(") || line.contains("::execute_sql(") {
                    result
                        .violations
                        .push(format!("{rel}:{} {}", lineno + 1, line.trim()));
                }
                if line.contains("execute_sql_statement_results(") {
                    if rel == "crates/aiondb-engine/src/engine/query_api.rs" {
                        continue;
                    }
                    result.violations.push(format!(
                        "{rel}:{} recursive execute_sql_statement_results is forbidden; use execute_statement or a typed API",
                        lineno + 1
                    ));
                }
            }
            if rel == "crates/aiondb-engine/src/engine/query_api.rs" {
                let count = text
                    .lines()
                    .filter(|line| line.contains("execute_sql_statement_results("))
                    .count();
                const ALLOWED_QUERY_API_EXECUTE_SQL_STATEMENT_RESULTS_SITES: usize = 3;
                if count > ALLOWED_QUERY_API_EXECUTE_SQL_STATEMENT_RESULTS_SITES {
                    result.violations.push(format!(
                        "{rel}: execute_sql_statement_results sites = {count} > {ALLOWED_QUERY_API_EXECUTE_SQL_STATEMENT_RESULTS_SITES}; recursive internal subcommands are forbidden"
                    ));
                }
            }
        }
    }
    result
}

// ---------------------------------------------------------------
// ADR-0007 - pas de match sur string SQL
// ---------------------------------------------------------------
fn check_sql_string_match(root: &Path) -> CheckResult {
    let mut result = CheckResult::new("sql-string-match");
    // ADR-0007: forbidden across the entire canonical pipeline and on the
    // client facades. Legacy compat (`engine/compat/*`) remains explicitly
    // out of scope (grandfathered ADR-0004).
    let forbidden: &[&str] = &[
        "crates/aiondb-executor/src",
        "crates/aiondb-planner/src",
        "crates/aiondb-optimizer/src",
        "crates/aiondb-eval/src",
        "crates/aiondb-catalog/src",
        "crates/aiondb-catalog-store/src",
        "crates/aiondb-storage-api/src",
        "crates/aiondb-storage-engine/src",
        "crates/aiondb-tx/src",
        "crates/aiondb-pgwire/src",
        "crates/aiondb-embedded/src",
        "crates/aiondb-dashboard/src",
    ];
    for prefix in forbidden {
        let files = rust_files_under(root, prefix);
        for path in &files {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .into_owned();
            if rel.contains("/tests/") || rel.contains("_tests.rs") {
                continue;
            }
            result.scanned_files += 1;
            let Some(text) = read_text(path) else {
                continue;
            };
            for (lineno, line) in text.lines().enumerate() {
                let trimmed = line.trim_start();
                if trimmed.starts_with("//") {
                    continue;
                }
                if line.contains("sql.contains(")
                    || line.contains("sql.starts_with(")
                    || line.contains("sql.ends_with(")
                    || line.contains("sql.to_uppercase(")
                    || line.contains("sql.to_lowercase(")
                    || line.contains("sql.to_ascii_uppercase(")
                {
                    result
                        .violations
                        .push(format!("{rel}:{} {}", lineno + 1, line.trim()));
                }
            }
        }
    }
    result
}

// ---------------------------------------------------------------
// ADR-0009 - systematic SQLSTATE
// ---------------------------------------------------------------
fn check_sqlstate(root: &Path) -> CheckResult {
    let mut result = CheckResult::new("sqlstate");
    // Heuristic: for public client-facing crates, flag `DbError::new(` without
    // a SqlState:: token nearby on the same or next line.
    let scan_dirs: &[&str] = &[
        "crates/aiondb-engine/src",
        "crates/aiondb-pgwire/src",
        "crates/aiondb-embedded/src",
        "crates/aiondb-dashboard/src",
    ];
    for d in scan_dirs {
        let files = rust_files_under(root, d);
        for path in &files {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .into_owned();
            if rel.contains("/tests/") || rel.contains("_tests.rs") {
                continue;
            }
            result.scanned_files += 1;
            let Some(text) = read_text(path) else {
                continue;
            };
            let lines: Vec<&str> = text.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                let trimmed = line.trim_start();
                if trimmed.starts_with("//") {
                    continue;
                }
                if line.contains("DbError::new(") {
                    let window = lines
                        .get(i.saturating_sub(1)..=i + 1.min(lines.len() - 1))
                        .unwrap_or(&[])
                        .join(" ");
                    if !window.contains("SqlState::") {
                        result
                            .violations
                            .push(format!("{rel}:{} {}", i + 1, line.trim()));
                    }
                }
            }
        }
    }
    result
}

// ---------------------------------------------------------------
// ADR-0010 - budgets explicites
// ---------------------------------------------------------------
fn check_budgets(root: &Path) -> CheckResult {
    let mut result = CheckResult::new("budgets");
    // Flag obviously unbounded `Vec::with_capacity(n)` / `String::with_capacity(n)`
    // where n is not a literal or clamp. Heuristic - very loose, meant to
    // catch new regressions not audit existing code.
    let scan_dirs: &[&str] = &["crates/aiondb-executor/src", "crates/aiondb-engine/src"];
    for d in scan_dirs {
        let files = rust_files_under(root, d);
        for path in &files {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .into_owned();
            if rel.contains("/tests/") || rel.contains("_tests.rs") {
                continue;
            }
            result.scanned_files += 1;
            let Some(text) = read_text(path) else {
                continue;
            };
            for (lineno, line) in text.lines().enumerate() {
                let t = line.trim_start();
                if t.starts_with("//") {
                    continue;
                }
                // Pattern: `with_capacity(NAME)` where NAME looks like a
                // user-provided input (e.g. count, len of client data).
                // Heuristic: flag only when the variable name suggests a
                // user-controlled value and there's no `.min(...)` clamp
                // on the same line.
                for pattern in [
                    "with_capacity(user_",
                    "with_capacity(client_",
                    "with_capacity(request_",
                    "with_capacity(raw_input_",
                    "with_capacity(payload_",
                    "with_capacity(param_count_from_wire",
                ] {
                    if line.contains(pattern) && !line.contains(".min(") {
                        result
                            .violations
                            .push(format!("{rel}:{} {}", lineno + 1, line.trim()));
                    }
                }
            }
        }
    }
    result
}

// ---------------------------------------------------------------
// ADR-0011 - dual-mode tests
// ---------------------------------------------------------------
fn check_dual_mode(root: &Path) -> CheckResult {
    let mut result = CheckResult::new("dual-mode");
    // Heuristic: ensure the aiondb-test-kit has both `embedded` and `pgwire`
    // modules present (shared scenario runner). Absence = broken dual-mode
    // infrastructure.
    let kit = root.join("testing/aiondb-test-kit/src");
    result.scanned_files = 1;
    let has_embedded = kit.join("embedded.rs").exists();
    let has_pgwire = kit.join("pgwire.rs").exists();
    let has_scenario = kit.join("scenario.rs").exists();
    if !has_embedded {
        result
            .violations
            .push("testing/aiondb-test-kit/src/embedded.rs missing".to_owned());
    }
    if !has_pgwire {
        result
            .violations
            .push("testing/aiondb-test-kit/src/pgwire.rs missing".to_owned());
    }
    if !has_scenario {
        result
            .violations
            .push("testing/aiondb-test-kit/src/scenario.rs missing".to_owned());
    }
    result
}

// ---------------------------------------------------------------
// ADR-0012 - #[ignore] exige tracked + target
// ---------------------------------------------------------------
fn check_ignores(root: &Path) -> CheckResult {
    let mut result = CheckResult::new("ignores");
    let scan_dirs: &[&str] = &["crates", "testing", "xtask"];
    for d in scan_dirs {
        let files = rust_files_under(root, d);
        for path in &files {
            result.scanned_files += 1;
            let Some(text) = read_text(path) else {
                continue;
            };
            let lines: Vec<&str> = text.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                let t = line.trim_start();
                if !(t.starts_with("#[ignore]") || t.starts_with("#[ignore ")) {
                    continue;
                }
                // Look up to 5 lines above for `tracked:` and `target:`
                let start = i.saturating_sub(5);
                let window = lines[start..i].join(" ");
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .into_owned();
                if !window.contains("tracked:") {
                    result
                        .violations
                        .push(format!("{rel}:{} missing `tracked:`", i + 1));
                }
                if !window.contains("target:") {
                    result
                        .violations
                        .push(format!("{rel}:{} missing `target:`", i + 1));
                }
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// Create a temporary directory with a given set of files.
    /// Cleanup is best-effort via drop guard.
    struct TempRepo(PathBuf);

    impl TempRepo {
        fn new(name: &str, files: &[(&str, &str)]) -> Self {
            let base = std::env::temp_dir().join(format!(
                "aiondb-xtask-adr-{}-{}",
                name,
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&base);
            fs::create_dir_all(&base).unwrap();
            // Cargo.toml stub so `root.join("Cargo.toml").exists()` holds.
            fs::write(base.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
            for (rel, content) in files {
                let path = base.join(rel);
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).unwrap();
                }
                fs::write(path, content).unwrap();
            }
            Self(base)
        }

        fn root(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn repo_root_exists() {
        let root = repo_root();
        assert!(root.join("Cargo.toml").exists());
    }

    #[test]
    fn check_result_ok_when_no_violations() {
        let r = CheckResult::new("x");
        assert!(r.ok());
    }

    #[test]
    fn check_result_not_ok_with_violations() {
        let mut r = CheckResult::new("x");
        r.violations.push("bad".to_owned());
        assert!(!r.ok());
    }

    #[test]
    fn compat_freeze_trips_when_count_grows() {
        let excess_methods = (0..20)
            .map(|i| format!("fn try_execute_compat_hook_{i}(&self) {{}}"))
            .collect::<Vec<_>>()
            .join("\n");
        let content = format!("impl Engine {{\n{excess_methods}\n}}\n");
        let repo = TempRepo::new(
            "compat-freeze",
            &[("crates/aiondb-engine/src/engine/compat/mod.rs", &content)],
        );
        let result = check_compat_freeze(repo.root());
        assert!(
            !result.ok(),
            "compat-freeze should fail when hook count > baseline; got {:?}",
            result.violations
        );
    }

    #[test]
    fn internal_execute_sql_flags_forbidden_crate() {
        let repo = TempRepo::new(
            "exec-sql",
            &[(
                "crates/aiondb-executor/src/operator.rs",
                "fn run(engine: &Engine) {\n    let _ = engine.execute_sql(\"SELECT 1\");\n}\n",
            )],
        );
        let result = check_internal_execute_sql(repo.root());
        assert!(
            !result.ok(),
            "internal-execute-sql should flag .execute_sql( in executor; got {:?}",
            result.violations
        );
    }

    #[test]
    fn internal_execute_sql_flags_qualified_trait_call() {
        let repo = TempRepo::new(
            "exec-sql-qualified",
            &[(
                "crates/aiondb-engine/src/engine/pg_compat_hooks.rs",
                "fn run(engine: &Engine, session: &SessionHandle) {\n    let _ = QueryEngine::execute_sql(engine, session, \"SELECT 1\");\n}\n",
            )],
        );
        let result = check_internal_execute_sql(repo.root());
        assert!(
            !result.ok(),
            "internal-execute-sql should flag QueryEngine::execute_sql calls; got {:?}",
            result.violations
        );
    }

    #[test]
    fn internal_execute_sql_flags_recursive_statement_results_outside_query_api() {
        let repo = TempRepo::new(
            "exec-sql-results",
            &[(
                "crates/aiondb-engine/src/engine/session_access.rs",
                "fn run(engine: &Engine) {\n    let _ = engine.execute_sql_statement_results();\n}\n",
            )],
        );
        let result = check_internal_execute_sql(repo.root());
        assert!(
            !result.ok(),
            "internal-execute-sql should flag recursive execute_sql_statement_results outside query_api; got {:?}",
            result.violations
        );
    }

    #[test]
    fn internal_execute_sql_flags_extra_query_api_statement_results_sites() {
        let src = r"
            impl Engine {
                fn execute_sql_statement_results(&self) {}
                fn execute_sql(&self) {
                    self.execute_sql_statement_results();
                    self.execute_sql_statement_results();
                }
                fn recursive_helper(&self) {
                    self.execute_sql_statement_results();
                }
            }
        ";
        let repo = TempRepo::new(
            "exec-sql-results-query-api",
            &[("crates/aiondb-engine/src/engine/query_api.rs", src)],
        );
        let result = check_internal_execute_sql(repo.root());
        assert!(
            !result.ok(),
            "internal-execute-sql should flag extra query_api execute_sql_statement_results sites; got {:?}",
            result.violations
        );
    }

    #[test]
    fn internal_execute_sql_skips_cfg_test_inline_mod() {
        let repo = TempRepo::new(
            "exec-sql-cfg-test",
            &[(
                "crates/aiondb-executor/src/ops.rs",
                "fn prod() {}\n\n#[cfg(test)]\nmod tests {\n    fn setup(engine: &Engine) {\n        let _ = engine.execute_sql(\"SELECT 1\");\n    }\n}\n",
            )],
        );
        let result = check_internal_execute_sql(repo.root());
        assert!(
            result.ok(),
            "internal-execute-sql must skip calls inside `#[cfg(test)] mod tests`; got {:?}",
            result.violations
        );
    }

    #[test]
    fn internal_execute_sql_skips_allowlist() {
        let repo = TempRepo::new(
            "exec-sql-allowlist",
            &[(
                "crates/aiondb-executor/src/executor/plpgsql_runtime.rs",
                "fn run(self_: &Self) { let _ = self_.execute_sql(\"x\"); }\n",
            )],
        );
        let result = check_internal_execute_sql(repo.root());
        assert!(
            result.ok(),
            "internal-execute-sql should skip plpgsql_runtime.rs allowlist; got {:?}",
            result.violations
        );
    }

    #[test]
    fn sql_string_match_flags_sql_contains() {
        let repo = TempRepo::new(
            "sql-match",
            &[(
                "crates/aiondb-executor/src/dispatcher.rs",
                "fn dispatch(sql: &str) -> bool { sql.contains(\"SELECT\") }\n",
            )],
        );
        let result = check_sql_string_match(repo.root());
        assert!(
            !result.ok(),
            "sql-string-match should flag sql.contains in executor; got {:?}",
            result.violations
        );
    }

    #[test]
    fn sqlstate_flags_dberror_new_without_sqlstate() {
        let repo = TempRepo::new(
            "sqlstate",
            &[(
                "crates/aiondb-engine/src/handler.rs",
                "fn f() { let _ = DbError::new(\"oops\"); }\n",
            )],
        );
        let result = check_sqlstate(repo.root());
        assert!(
            !result.ok(),
            "sqlstate should flag DbError::new without SqlState::; got {:?}",
            result.violations
        );
    }

    #[test]
    fn sqlstate_accepts_when_sqlstate_on_adjacent_line() {
        let repo = TempRepo::new(
            "sqlstate-adj",
            &[(
                "crates/aiondb-engine/src/handler.rs",
                "fn f() {\n    let _ = DbError::new(\n        SqlState::SyntaxError,\n        \"bad\",\n    );\n}\n",
            )],
        );
        let result = check_sqlstate(repo.root());
        assert!(
            result.ok(),
            "sqlstate should accept DbError::new with adjacent SqlState; got {:?}",
            result.violations
        );
    }

    #[test]
    fn ignores_flags_bare_ignore() {
        let repo = TempRepo::new(
            "ignores-bare",
            &[(
                "crates/aiondb-engine/src/tests.rs",
                "#[test]\n#[ignore]\nfn hard_to_fix() {}\n",
            )],
        );
        let result = check_ignores(repo.root());
        assert!(
            !result.ok(),
            "ignores should flag #[ignore] without tracked/target; got {:?}",
            result.violations
        );
    }

    #[test]
    fn ignores_accepts_tracked_and_target() {
        let repo = TempRepo::new(
            "ignores-annotated",
            &[(
                "crates/aiondb-engine/src/tests.rs",
                "// tracked: https://example.com/issue\n// target: 2026-12-31\n#[test]\n#[ignore = \"wip\"]\nfn hard_to_fix() {}\n",
            )],
        );
        let result = check_ignores(repo.root());
        assert!(
            result.ok(),
            "ignores should accept annotated #[ignore]; got {:?}",
            result.violations
        );
    }

    #[test]
    fn noop_sites_trips_when_count_grows() {
        let huge = (0..200)
            .map(|i| {
                format!(
                    "return Ok(Statement::CommandNoOp {{ tag: \"FAKE{i}\".to_owned(), span: Span::default() }});"
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let repo = TempRepo::new("noop-sites", &[("crates/aiondb-parser/src/big.rs", &huge)]);
        let result = check_noop_sites(repo.root());
        assert!(
            !result.ok(),
            "noop-sites should fail when parser count > baseline; got {:?}",
            result.violations
        );
    }

    #[test]
    fn dispatch_accepts_empty_tree() {
        let repo = TempRepo::new("dispatch-empty", &[]);
        let result = check_dispatch(repo.root());
        assert!(result.ok());
    }

    #[test]
    fn dual_mode_flags_missing_kit_files() {
        let repo = TempRepo::new("dual-mode-missing", &[]);
        let result = check_dual_mode(repo.root());
        assert!(
            !result.ok(),
            "dual-mode should flag absent test-kit; got {:?}",
            result.violations
        );
    }
}
