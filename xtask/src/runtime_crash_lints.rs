use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use proc_macro2::Span;
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{
    Attribute, ExprMethodCall, ImplItemFn, ItemConst, ItemFn, ItemImpl, ItemMacro, ItemMod,
    ItemStatic, ItemTrait, Macro, Meta, Token, TraitItemFn,
};

const SOURCE_ROOTS: &[&str] = &["crates"];
const SKIP_DIRS: &[&str] = &[
    ".git",
    "target",
    "tests",
    "benches",
    "examples",
    "fuzz_smoke",
];
const SKIP_PATH_COMPONENTS: &[&str] = &["engine_tests"];
const SKIP_FILE_PREFIXES: &[&str] = &["tests_", "test_"];
const SKIP_FILE_CONTAINS: &[&str] = &["_e2e_"];
const SKIP_FILE_SUFFIXES: &[&str] = &["_tests.rs", "_test.rs", "_e2e.rs"];
const SKIP_FILE_NAMES: &[&str] = &["tests.rs"];

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum ForbiddenKind {
    Unwrap,
    Expect,
    Panic,
}

impl ForbiddenKind {
    fn label(self) -> &'static str {
        match self {
            Self::Unwrap => "unwrap()",
            Self::Expect => "expect()",
            Self::Panic => "panic!()",
        }
    }
}

#[derive(Debug, Eq, PartialEq, Ord, PartialOrd)]
struct Violation {
    path: PathBuf,
    line: usize,
    column: usize,
    kind: ForbiddenKind,
}

pub(crate) fn parse_args(args: &[String]) -> Result<(), String> {
    match args {
        [] => Ok(()),
        [flag] if matches!(flag.as_str(), "--help" | "-h") => {
            print_usage();
            std::process::exit(0);
        }
        [other, ..] => Err(format!(
            "unknown flag for runtime-crash-lints: {other}\n\nRun `cargo xtask runtime-crash-lints --help` for usage."
        )),
    }
}

pub(crate) fn print_usage() {
    println!(
        "\
Usage: cargo xtask runtime-crash-lints

Reject `.unwrap()`, `.expect()`, and `panic!()` in production Rust sources
under `crates/`, while excluding test-only files and items guarded by
`#[cfg(test)]`, `#[test]`, or `#[tokio::test]`."
    );
}

pub(crate) fn run() -> Result<(), String> {
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
        violations.extend(scan_file(&workspace_root, &path)?);
    }
    let actual_allowlist_entries: HashSet<String> =
        violations.iter().map(allowlist_entry).collect();
    let mut stale_allowlist_entries: Vec<_> = allowlist
        .difference(&actual_allowlist_entries)
        .cloned()
        .collect();
    stale_allowlist_entries.sort();

    violations.retain(|violation| !allowlist.contains(&allowlist_entry(violation)));

    violations.sort();
    if violations.is_empty() && stale_allowlist_entries.is_empty() {
        println!("No runtime crash lint violations found in production Rust sources.");
        return Ok(());
    }

    if !violations.is_empty() {
        println!("Runtime crash lint violations:");
        for violation in &violations {
            let relative = violation
                .path
                .strip_prefix(&workspace_root)
                .unwrap_or(&violation.path);
            println!(
                "  {}:{}:{}  {}",
                relative.display(),
                violation.line,
                violation.column,
                violation.kind.label()
            );
        }
    }

    if !stale_allowlist_entries.is_empty() {
        println!("Stale runtime crash lint allowlist entries:");
        for entry in &stale_allowlist_entries {
            println!("  {entry}");
        }
    }

    Err("runtime crash lint violations or stale allowlist entries detected".to_owned())
}

fn load_allowlist(workspace_root: &Path) -> HashSet<String> {
    let path = workspace_root.join(".runtime-crash-lints-allow");
    let Ok(content) = fs::read_to_string(&path) else {
        return HashSet::new();
    };
    content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(String::from)
        .collect()
}

fn allowlist_entry(violation: &Violation) -> String {
    let path = violation.path.to_string_lossy().replace('\\', "/");
    format!("{}:{}:{}", path, violation.line, violation.kind.label())
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

        if file_type.is_file()
            && entry_path.extension() == Some(OsStr::new("rs"))
            && !should_skip_file(&entry_path)
        {
            files.push(entry_path);
        }
    }

    Ok(())
}

fn should_skip_dir(name: &OsStr) -> bool {
    SKIP_DIRS.iter().any(|dir| name == OsStr::new(dir))
}

fn should_skip_file(path: &Path) -> bool {
    let Some(file_name) = path.file_name().and_then(OsStr::to_str) else {
        return true;
    };
    if SKIP_FILE_NAMES.contains(&file_name) {
        return true;
    }
    if SKIP_FILE_PREFIXES
        .iter()
        .any(|prefix| file_name.starts_with(prefix))
    {
        return true;
    }
    if SKIP_FILE_CONTAINS
        .iter()
        .any(|needle| file_name.contains(needle))
    {
        return true;
    }
    if SKIP_FILE_SUFFIXES
        .iter()
        .any(|suffix| file_name.ends_with(suffix))
    {
        return true;
    }
    path.components().any(|component| {
        let value = component.as_os_str();
        SKIP_DIRS.iter().any(|segment| value == OsStr::new(segment))
            || SKIP_PATH_COMPONENTS
                .iter()
                .any(|segment| value == OsStr::new(segment))
    })
}

fn scan_file(workspace_root: &Path, path: &Path) -> Result<Vec<Violation>, String> {
    let source = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let parsed = syn::parse_file(&source)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))?;

    if is_test_only_attrs(&parsed.attrs) {
        return Ok(Vec::new());
    }

    let mut visitor = RuntimeCrashVisitor::new(workspace_root, path);
    visitor.visit_file(&parsed);
    Ok(visitor.violations)
}

fn is_test_only_attrs(attrs: &[Attribute]) -> bool {
    attrs.iter().any(is_test_only_attr)
}

fn is_test_only_attr(attr: &Attribute) -> bool {
    if attr.path().is_ident("test") {
        return true;
    }

    if attr
        .path()
        .segments
        .last()
        .is_some_and(|segment| segment.ident == "test")
        && attr.path().segments.len() > 1
    {
        return true;
    }

    if !attr.path().is_ident("cfg") {
        return false;
    }

    let parser = Punctuated::<Meta, Token![,]>::parse_terminated;
    let Ok(items) = parser.parse2(attr.meta.require_list().map_or_else(
        |_| proc_macro2::TokenStream::new(),
        |list| list.tokens.clone(),
    )) else {
        return false;
    };

    !items.is_empty() && items.iter().all(meta_requires_test)
}

fn meta_requires_test(meta: &Meta) -> bool {
    match meta {
        Meta::Path(path) => path.is_ident("test"),
        Meta::List(list) if list.path.is_ident("all") => {
            parse_nested_meta_list(list).iter().any(meta_requires_test)
        }
        Meta::List(list) if list.path.is_ident("any") => {
            let nested = parse_nested_meta_list(list);
            !nested.is_empty() && nested.iter().all(meta_requires_test)
        }
        Meta::List(list) if list.path.is_ident("cfg") => {
            let nested = parse_nested_meta_list(list);
            !nested.is_empty() && nested.iter().all(meta_requires_test)
        }
        Meta::List(list) if list.path.is_ident("not") => false,
        Meta::List(_) | Meta::NameValue(_) => false,
    }
}

fn parse_nested_meta_list(list: &syn::MetaList) -> Vec<Meta> {
    let parser = Punctuated::<Meta, Token![,]>::parse_terminated;
    parser
        .parse2(list.tokens.clone())
        .map(|items| items.into_iter().collect())
        .unwrap_or_default()
}

struct RuntimeCrashVisitor<'a> {
    workspace_root: &'a Path,
    path: &'a Path,
    violations: Vec<Violation>,
}

impl<'a> RuntimeCrashVisitor<'a> {
    fn new(workspace_root: &'a Path, path: &'a Path) -> Self {
        Self {
            workspace_root,
            path,
            violations: Vec::new(),
        }
    }

    fn record(&mut self, span: Span, kind: ForbiddenKind) {
        let start = span.start();
        self.violations.push(Violation {
            path: self
                .path
                .strip_prefix(self.workspace_root)
                .map_or_else(|_| self.path.to_path_buf(), Path::to_path_buf),
            line: start.line,
            column: start.column + 1,
            kind,
        });
    }
}

impl Visit<'_> for RuntimeCrashVisitor<'_> {
    fn visit_item_impl(&mut self, node: &ItemImpl) {
        if is_test_only_attrs(&node.attrs) {
            return;
        }
        visit::visit_item_impl(self, node);
    }

    fn visit_item_trait(&mut self, node: &ItemTrait) {
        if is_test_only_attrs(&node.attrs) {
            return;
        }
        visit::visit_item_trait(self, node);
    }

    fn visit_item_mod(&mut self, node: &ItemMod) {
        if is_test_only_attrs(&node.attrs) {
            return;
        }
        if let Some((_, items)) = &node.content {
            for item in items {
                self.visit_item(item);
            }
        }
    }

    fn visit_item_macro(&mut self, node: &ItemMacro) {
        if is_test_only_attrs(&node.attrs) {
            return;
        }
        visit::visit_item_macro(self, node);
    }

    fn visit_item_fn(&mut self, node: &ItemFn) {
        if is_test_only_attrs(&node.attrs) {
            return;
        }
        visit::visit_block(self, &node.block);
    }

    fn visit_impl_item_fn(&mut self, node: &ImplItemFn) {
        if is_test_only_attrs(&node.attrs) {
            return;
        }
        visit::visit_block(self, &node.block);
    }

    fn visit_trait_item_fn(&mut self, node: &TraitItemFn) {
        if is_test_only_attrs(&node.attrs) {
            return;
        }
        visit::visit_trait_item_fn(self, node);
    }

    fn visit_item_const(&mut self, node: &ItemConst) {
        if is_test_only_attrs(&node.attrs) {
            return;
        }
        visit::visit_item_const(self, node);
    }

    fn visit_item_static(&mut self, node: &ItemStatic) {
        if is_test_only_attrs(&node.attrs) {
            return;
        }
        visit::visit_item_static(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &ExprMethodCall) {
        match node.method.to_string().as_str() {
            "unwrap" => self.record(node.method.span(), ForbiddenKind::Unwrap),
            "expect" => self.record(node.method.span(), ForbiddenKind::Expect),
            _ => {}
        }
        visit::visit_expr_method_call(self, node);
    }

    fn visit_macro(&mut self, node: &Macro) {
        if node.path.is_ident("panic") {
            self.record(node.path.span(), ForbiddenKind::Panic);
        }
        visit::visit_macro(self, node);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_file(source: &str) -> syn::File {
        syn::parse_file(source).expect("test source should parse")
    }

    #[test]
    fn cfg_attribute_detection_matches_test_only_cases() {
        let file = parse_file(
            r#"
            #[cfg(test)]
            fn case_one() {}

            #[cfg(all(test, feature = "x"))]
            fn case_two() {}

            #[cfg(any(test, feature = "x"))]
            fn case_three() {}

            #[tokio::test]
            async fn async_case() {}
            "#,
        );

        let attrs: Vec<Vec<Attribute>> = file
            .items
            .into_iter()
            .map(|item| match item {
                syn::Item::Fn(item_fn) => item_fn.attrs,
                _ => Vec::new(),
            })
            .collect();

        assert!(is_test_only_attrs(&attrs[0]));
        assert!(is_test_only_attrs(&attrs[1]));
        assert!(!is_test_only_attrs(&attrs[2]));
        assert!(is_test_only_attrs(&attrs[3]));
    }

    #[test]
    fn visitor_ignores_test_items_and_reports_prod_violations() {
        let file = parse_file(
            r#"
            fn prod() {
                value.unwrap();
                value.expect("msg");
                panic!("boom");
            }

            #[cfg(test)]
            mod tests {
                fn helper() {
                    test_value.unwrap();
                    panic!("skip");
                }
            }

            #[test]
            fn unit_case() {
                other.expect("skip");
            }

            struct Demo;

            impl Demo {
                fn prod_method(&self) {
                    item.unwrap();
                }

                #[cfg(test)]
                fn helper(&self) {
                    helper.unwrap();
                }
            }

            #[cfg(test)]
            impl Demo {
                fn helper_impl(&self) {
                    another.unwrap();
                }
            }
            "#,
        );

        let workspace_root = Path::new("/tmp/workspace");
        let path = Path::new("/tmp/workspace/crates/demo/src/lib.rs");
        let mut visitor = RuntimeCrashVisitor::new(workspace_root, path);
        visitor.visit_file(&file);

        let kinds: Vec<ForbiddenKind> = visitor.violations.iter().map(|v| v.kind).collect();
        assert_eq!(
            kinds,
            vec![
                ForbiddenKind::Unwrap,
                ForbiddenKind::Expect,
                ForbiddenKind::Panic,
                ForbiddenKind::Unwrap,
            ]
        );
    }

    #[test]
    fn skip_file_filters_test_naming_patterns() {
        assert!(should_skip_file(Path::new(
            "crates/aiondb-pgwire/src/extended_query_e2e.rs"
        )));
        assert!(should_skip_file(Path::new(
            "crates/aiondb-buffer-pool/src/pool_tests.rs"
        )));
        assert!(should_skip_file(Path::new(
            "crates/aiondb-plan/src/physical/tests/core_types.rs"
        )));
        assert!(!should_skip_file(Path::new(
            "crates/aiondb-engine/src/catalog_auth.rs"
        )));
    }
}
