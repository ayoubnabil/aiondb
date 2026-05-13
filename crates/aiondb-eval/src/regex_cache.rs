// Per-thread compiled-regex cache. Hot scalar paths (`~`, `~*`, `regexp_*`,
// `LIKE` with regex translation, jsonpath `like_regex`) used to recompile the
// pattern once per row. With this cache the compile is amortised across the
// scan; misses still fall through to `regex` directly so behaviour is
// unchanged on first use or when the pattern varies row-to-row.
//
// Thread-local intentionally - avoids Mutex contention across worker threads,
// and the upper bound caps memory at a few MB even with many shapes.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use regex::{Regex, RegexBuilder};

const CACHE_CAP: usize = 256;

#[derive(Clone, Copy, Default, Hash, PartialEq, Eq)]
struct Flags {
    case_insensitive: bool,
    dot_matches_newline: bool,
    multi_line: bool,
    swap_greed: bool,
    ignore_whitespace: bool,
}

// Flags-keyed two-level map: outer HashMap keyed on Flags (Copy), inner
// HashMap<String, ...> looked up via &str through Borrow<str>. This avoids
// allocating a fresh String key on every cache hit (the previous design
// built a `Key { pattern: pattern.to_string(), flags }` even on hot path).
type FlagsBucket = HashMap<String, Result<Arc<Regex>, String>>;

thread_local! {
    static CACHE: RefCell<HashMap<Flags, FlagsBucket>> =
        RefCell::new(HashMap::with_capacity(4));
}

fn lookup_or_build<F>(pattern: &str, flags: Flags, build: F) -> Result<Arc<Regex>, String>
where
    F: FnOnce(&str, Flags) -> Result<Regex, regex::Error>,
{
    CACHE.with(|cell| {
        let mut map = cell.borrow_mut();
        if let Some(bucket) = map.get(&flags) {
            if let Some(hit) = bucket.get(pattern) {
                return hit.clone();
            }
        }
        let total: usize = map.values().map(HashMap::len).sum();
        if total >= CACHE_CAP {
            map.clear();
        }
        let entry = match build(pattern, flags) {
            Ok(re) => Ok(Arc::new(re)),
            Err(err) => Err(err.to_string()),
        };
        map.entry(flags)
            .or_default()
            .insert(pattern.to_string(), entry.clone());
        entry
    })
}

fn build_with(pattern: &str, flags: Flags) -> Result<Regex, regex::Error> {
    // Bound NFA / DFA memory so an adversarial pattern cannot drive the
    // compiled regex into hundreds of MiB. The Rust regex engine is linear in
    // execution time, but compile-time memory is unbounded by default
    // (audit eval F2). 16 MiB per cache entry is generous for legitimate
    // user patterns.
    const REGEX_SIZE_LIMIT_BYTES: usize = 16 * 1024 * 1024;
    const REGEX_DFA_SIZE_LIMIT_BYTES: usize = 16 * 1024 * 1024;
    RegexBuilder::new(pattern)
        .case_insensitive(flags.case_insensitive)
        .dot_matches_new_line(flags.dot_matches_newline)
        .multi_line(flags.multi_line)
        .swap_greed(flags.swap_greed)
        .ignore_whitespace(flags.ignore_whitespace)
        .size_limit(REGEX_SIZE_LIMIT_BYTES)
        .dfa_size_limit(REGEX_DFA_SIZE_LIMIT_BYTES)
        .build()
}

/// Cached regex with the default flags (case-sensitive, single-line).
pub fn get(pattern: &str) -> Result<Arc<Regex>, String> {
    lookup_or_build(pattern, Flags::default(), build_with)
}

/// Cached regex with a single case-insensitivity toggle - covers the bulk of
/// the scalar regex call sites (`~`, `~*`, `regexp_match`).
pub fn get_ci(pattern: &str, case_insensitive: bool) -> Result<Arc<Regex>, String> {
    lookup_or_build(
        pattern,
        Flags {
            case_insensitive,
            ..Flags::default()
        },
        build_with,
    )
}

/// PostgreSQL `regexp_*` flag string - `i` ci, `s` dot-all, `m` multiline,
/// `n` lines (Postgres uses `n` for newline-sensitive in some paths), `g`
/// global (not a regex flag - caller handles), `x` whitespace-ignore.
pub fn get_pg_flags(pattern: &str, pg_flags: &str) -> Result<Arc<Regex>, String> {
    let mut flags = Flags::default();
    for ch in pg_flags.chars() {
        match ch {
            'i' => flags.case_insensitive = true,
            'c' => flags.case_insensitive = false,
            's' => flags.dot_matches_newline = true,
            'm' | 'n' => flags.multi_line = true,
            'p' => {
                flags.dot_matches_newline = false;
                flags.multi_line = false;
            }
            'w' => flags.multi_line = true,
            'x' => flags.ignore_whitespace = true,
            'g' => {} // global handled by caller
            _ => {}
        }
    }
    lookup_or_build(pattern, flags, build_with)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_same_arc_on_hit() {
        let a = get("^abc$").expect("compile");
        let b = get("^abc$").expect("compile");
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn case_insensitive_distinct_from_sensitive() {
        let a = get_ci("Abc", false).expect("compile");
        let b = get_ci("Abc", true).expect("compile");
        assert!(!Arc::ptr_eq(&a, &b));
        assert!(b.is_match("ABC"));
        assert!(!a.is_match("ABC"));
    }

    #[test]
    fn invalid_pattern_is_cached_as_error() {
        let e1 = get("([").unwrap_err();
        let e2 = get("([").unwrap_err();
        assert_eq!(e1, e2);
    }

    #[test]
    fn pg_flags_parse_i_and_s() {
        let re = get_pg_flags("a.b", "is").expect("compile");
        assert!(re.is_match("A\nB"));
    }
}
