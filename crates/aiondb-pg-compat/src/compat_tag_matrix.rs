//! Matrix of every compatibility tag the compat layer accepts, with its
//! guaranteed behaviour. Single source of truth for engine dispatch, the
//! pg-compat allowlist, and the documentation mirror at
//! `docs/compat/tag_matrix.toml`.
//!
//! Tag buckets:
//!   * `ImplementedReal` - a dedicated handler intercepts the command and
//!     applies real catalog/state effects. A parse failure on such a tag
//!     surfaces as `feature_not_supported`.
//!   * `ExplicitNotSupported` - the handler rejects the command with
//!     `feature_not_supported`, even on valid syntax (features AionDB
//!     genuinely cannot emulate).
//!   * `IntentionalNoop` - sentinel for tags the engine would otherwise
//!     silently acknowledge as a no-op. Gated to stay empty: any tag
//!     ending up here is a regression and trips the test in this file.
//!
//! Dispatch is a separate axis from behaviour: it names which routing
//! branch the engine uses to reach the handler. See `CompatDispatch`.
//!
//! Any tag reaching the engine without a matching entry here is a
//! regression: the allowlist check or the sensitive-tag guard surfaces
//! `feature_not_supported` instead of running an unspecified path.
//!
//! Previously lived in `aiondb-engine` under `pub(in crate::engine)`. Moved
//! to `aiondb-pg-compat` so the compat crate owns its own metadata and the
//! documentation file `docs/compat/tag_matrix.toml` can be validated
//! against it via a single unit test.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompatTagBehavior {
    ImplementedReal,
    ExplicitNotSupported,
    IntentionalNoop,
}

/// Routing class for a compatibility tag. This lets the direct compat
/// command handler replace its per-tag match arms by a uniform lookup
/// against `COMPAT_TAG_MATRIX`.
///
/// * `Dedicated` - the tag has a bespoke arm (its own parsing and state
///   mutation). Typical for tags like `ALTER TYPE`,
///   `CREATE OPERATOR` that can't share a uniform validator.
/// * `AlterMiscObject` - the tag is validated by
///   `Engine::validate_compat_alter_misc_object` followed by the shared
///   `apply_compat_alter_action` dispatcher.
/// * `DropMiscObject` - the tag is validated by
///   `Engine::validate_compat_drop_misc_object`.
/// * `TripwireOnly` - the tag only reaches the executor via the parser
///   fallback (malformed SQL). The sensitive-tag guard in `statement_exec`
///   surfaces `feature_not_supported`. No dispatch arm is expected to
///   accept it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompatDispatch {
    Dedicated,
    AlterMiscObject,
    DropMiscObject,
    TripwireOnly,
}

#[derive(Clone, Copy, Debug)]
pub struct CompatTagEntry {
    pub tag: &'static str,
    pub behavior: CompatTagBehavior,
    /// Routing class. Consulted by the engine dispatcher to decide how to
    /// handle the tag without hardcoding a per-tag match arm.
    pub dispatch: CompatDispatch,
    /// Handler pointer - `module::function` or prose note. Read by
    /// `pg_compat_object_attrs` / introspection tooling to display the
    /// responsible implementation for each compat command.
    pub handler: &'static str,
}

/// emits `feature_not_supported` instead of `command_ok`.
///
/// Sensitivity is derived by tag family (object kinds whose semantics
/// AionDB does not physically emulate: publication, subscription, policy,
/// FDW stack, event trigger, materialized view). Keeping it as a helper
/// rather than a per-entry flag keeps the matrix declaration compact and
/// avoids having to touch every entry when the sensitivity list evolves.
#[must_use]
pub fn is_sensitive_compat_tag(tag: &str) -> bool {
    matches!(
        tag,
        "CREATE PUBLICATION"
            | "ALTER PUBLICATION"
            | "DROP PUBLICATION"
            | "CREATE SUBSCRIPTION"
            | "ALTER SUBSCRIPTION"
            | "DROP SUBSCRIPTION"
            | "CREATE POLICY"
            | "ALTER POLICY"
            | "DROP POLICY"
            | "CREATE FOREIGN DATA WRAPPER"
            | "ALTER FOREIGN DATA WRAPPER"
            | "DROP FOREIGN DATA WRAPPER"
            | "CREATE FOREIGN TABLE"
            | "ALTER FOREIGN TABLE"
            | "DROP FOREIGN TABLE"
            | "ALTER MATERIALIZED"
            | "CREATE USER MAPPING"
            | "ALTER USER MAPPING"
            | "DROP USER MAPPING"
            | "CREATE SERVER"
            | "ALTER SERVER"
            | "DROP SERVER"
    )
}

/// Single source of truth for compatibility-command behaviour. Entries
/// cover every tag the engine intentionally accepts or tracks through the
/// compatibility pipeline. `ExplicitNotSupported` is the implicit default
/// for any unregistered tag, so the matrix only lists tags with real
/// behaviour - rejection doesn't need a dedicated entry. Keep in
/// dispatch in the direct compat command handler.
pub const COMPAT_TAG_MATRIX: &[CompatTagEntry] = &[
    // ─────────────────────────── ALTER TABLE ────────────────────────────
    CompatTagEntry {
        tag: "ALTER TABLE",
        behavior: CompatTagBehavior::ImplementedReal,
        dispatch: CompatDispatch::Dedicated,
        handler: "compat_runtime::ddl::validate_compat_alter_table_target + apply_compat_alter_table_trigger_state",
    },
    CompatTagEntry {
        tag: "CREATE TEXT SEARCH",
        behavior: CompatTagBehavior::ImplementedReal,
        dispatch: CompatDispatch::Dedicated,
        handler: "compat_runtime::ddl::record_compat_misc_create",
    },
    // Database, type/domain, collation, extension, FDW, policy, publication,
    // server, statistics, subscription, tablespace, user mapping, aggregate,
    // operator, trigger, cast, and rule families are handled by typed ASTs
    // or shared validators elsewhere in the compat layer.
    // GRANT/REVOKE compat stubs are explicit rejects. Real
    // `Statement::{Grant,Revoke}` paths continue to run through ACL logic.
    CompatTagEntry {
        tag: "GRANT",
        behavior: CompatTagBehavior::ExplicitNotSupported,
        dispatch: CompatDispatch::Dedicated,
        handler: "noop_validation::unsupported_compatibility_command (compat stub reject)",
    },
    CompatTagEntry {
        tag: "REVOKE",
        behavior: CompatTagBehavior::ExplicitNotSupported,
        dispatch: CompatDispatch::Dedicated,
        handler: "noop_validation::unsupported_compatibility_command (compat stub reject)",
    },
];

/// `true` when the tag is an `ALTER <KIND>` form routed through the
/// shared `validate_compat_alter_misc_object` validator. Tags that fail
/// explicitly are intentionally excluded.
#[must_use]
pub fn is_alter_misc_object_tag(tag: &str) -> bool {
    matches!(tag, "ALTER POLICY" | "ALTER STATISTICS")
}

/// `true` when the tag is a `DROP <KIND>` form routed through the shared
/// `validate_compat_drop_misc_object` validator.
#[must_use]
pub fn is_drop_misc_object_tag(tag: &str) -> bool {
    matches!(tag, "DROP POLICY")
}

/// `true` for any tag that the compat allowlist recognises as an
/// `ImplementedReal`. Union of the matrix and the misc-object intrinsic
/// stays aligned with the remaining intrinsic misc-object routes.
#[must_use]
pub fn is_allowlisted_misc_object_tag(tag: &str) -> bool {
    is_alter_misc_object_tag(tag) || is_drop_misc_object_tag(tag)
}

/// Return the matrix entry for a tag, if it is registered.
#[must_use]
pub fn compat_tag_entry(tag: &str) -> Option<&'static CompatTagEntry> {
    COMPAT_TAG_MATRIX.iter().find(|entry| entry.tag == tag)
}

/// Return the behaviour bucket for a tag. Unregistered tags are treated as
/// `ExplicitNotSupported` so the caller rejects them with
/// `feature_not_supported`.
#[must_use]
pub fn compat_tag_behavior(tag: &str) -> CompatTagBehavior {
    compat_tag_entry(tag)
        .map(|entry| entry.behavior)
        .unwrap_or(CompatTagBehavior::ExplicitNotSupported)
}

/// Return the dispatch class for a tag. Unregistered tags fall back to
/// `Dedicated` so the caller can decide whether to route via one of the
/// non-matrix arms (ALTER SCHEMA / ALTER VIEW / REFRESH / …) or reject
/// the command.
#[must_use]
pub fn compat_tag_dispatch(tag: &str) -> CompatDispatch {
    compat_tag_entry(tag)
        .map(|entry| entry.dispatch)
        .unwrap_or(CompatDispatch::Dedicated)
}

/// counts. Used by `cargo xtask check-compat-hooks` and the compat report.
#[must_use]
pub fn behavior_counts() -> (usize, usize, usize) {
    let mut implemented_real = 0usize;
    let mut explicit_not_supported = 0usize;
    let mut intentional_noop = 0usize;
    for entry in COMPAT_TAG_MATRIX {
        match entry.behavior {
            CompatTagBehavior::ImplementedReal => implemented_real += 1,
            CompatTagBehavior::ExplicitNotSupported => explicit_not_supported += 1,
            CompatTagBehavior::IntentionalNoop => intentional_noop += 1,
        }
    }
    (implemented_real, explicit_not_supported, intentional_noop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    /// Parse `name = "..."` lines from the TOML mirror. Naive extractor -
    /// sufficient because the TOML is flat and machine-generated.
    fn tags_from_docs_toml(toml: &str) -> Vec<String> {
        let mut out = Vec::new();
        for raw in toml.lines() {
            let line = raw.trim_start();
            if line.starts_with('#') {
                continue;
            }
            if let Some(rest) = line.strip_prefix("name = \"") {
                if let Some(end) = rest.find('"') {
                    out.push(rest[..end].to_owned());
                }
            }
        }
        out
    }

    fn read_docs_tag_matrix_toml() -> Option<String> {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/compat/tag_matrix.toml");
        std::fs::read_to_string(path).ok()
    }

    /// Lock the size of the matrix so additions require an explicit code
    /// review and the matching allowlist update. Bump this constant when
    /// adding new tags (and update `docs/compat/tag_matrix.toml`).
    const EXPECTED_MATRIX_SIZE: usize = 4;

    #[test]
    fn matrix_has_unique_tags() {
        let mut seen = BTreeSet::new();
        for entry in COMPAT_TAG_MATRIX {
            assert!(
                seen.insert(entry.tag),
                "duplicate tag in COMPAT_TAG_MATRIX: {}",
                entry.tag
            );
        }
    }

    #[test]
    fn matrix_size_is_locked() {
        assert_eq!(
            COMPAT_TAG_MATRIX.len(),
            EXPECTED_MATRIX_SIZE,
            "COMPAT_TAG_MATRIX size changed — update EXPECTED_MATRIX_SIZE \
             and docs/compat/tag_matrix.toml in sync"
        );
    }

    #[test]
    fn matrix_entries_are_well_formed() {
        for entry in COMPAT_TAG_MATRIX {
            assert!(
                !entry.handler.trim().is_empty(),
                "matrix entry for {:?} has empty handler reference",
                entry.tag
            );
            let _ = entry.behavior;
            let _ = entry.dispatch;
        }
    }

    #[test]
    fn matrix_dispatch_and_behavior_are_coherent() {
        for entry in COMPAT_TAG_MATRIX {
            match entry.dispatch {
                CompatDispatch::TripwireOnly => assert_eq!(
                    entry.behavior,
                    CompatTagBehavior::ExplicitNotSupported,
                    "{:?}: TripwireOnly dispatch requires ExplicitNotSupported behaviour",
                    entry.tag
                ),
                CompatDispatch::AlterMiscObject | CompatDispatch::DropMiscObject => {
                    assert_eq!(
                        entry.behavior,
                        CompatTagBehavior::ImplementedReal,
                        "{:?}: {:?} dispatch requires ImplementedReal behaviour",
                        entry.tag,
                        entry.dispatch
                    );
                }
                CompatDispatch::Dedicated => {}
            }
        }
    }

    #[test]
    fn behavior_counts_match_matrix_size() {
        let (real, explicit, noop) = behavior_counts();
        assert_eq!(real + explicit + noop, COMPAT_TAG_MATRIX.len());
    }

    /// Keep this check aligned with the executor-level allowlist const.
    #[test]
    fn intentional_noop_bucket_is_empty() {
        let matrix_intentional: BTreeSet<&str> = COMPAT_TAG_MATRIX
            .iter()
            .filter(|entry| entry.behavior == CompatTagBehavior::IntentionalNoop)
            .map(|entry| entry.tag)
            .collect();
        let executor_allowlist: BTreeSet<&str> = aiondb_core::COMPAT_EXECUTOR_INTENTIONAL_NOOP_TAGS
            .iter()
            .copied()
            .collect();
        assert_eq!(
            matrix_intentional, executor_allowlist,
            "matrix IntentionalNoop bucket must stay empty and mirror \
             COMPAT_EXECUTOR_INTENTIONAL_NOOP_TAGS"
        );
    }

    /// without explicit contract updates.
    #[test]
    fn intentional_noop_allowlist_is_gated() {
        let allowed: BTreeSet<&str> = BTreeSet::new();
        let matrix_intentional: BTreeSet<&str> = COMPAT_TAG_MATRIX
            .iter()
            .filter(|entry| entry.behavior == CompatTagBehavior::IntentionalNoop)
            .map(|entry| entry.tag)
            .collect();
        assert_eq!(
            matrix_intentional, allowed,
            "IntentionalNoop gate broken — this bucket must stay empty."
        );
    }

    /// The docs mirror `docs/compat/tag_matrix.toml` must list exactly the
    /// same tag set as `COMPAT_TAG_MATRIX`. Validation-only parity: when
    /// the matrix grows, update the TOML; the test tells you precisely
    /// which tags are missing or extra.
    #[test]
    fn docs_tag_matrix_toml_matches_matrix() {
        let Some(toml_source) = read_docs_tag_matrix_toml() else {
            // Some workspace snapshots intentionally omit docs/compat.
            // Keep build/test targets compilable in that layout.
            return;
        };
        let matrix_tags: BTreeSet<&str> = COMPAT_TAG_MATRIX.iter().map(|e| e.tag).collect();
        let toml_tags: BTreeSet<String> = tags_from_docs_toml(&toml_source).into_iter().collect();
        let toml_tags_ref: BTreeSet<&str> = toml_tags.iter().map(String::as_str).collect();

        let missing_in_toml: Vec<&&str> = matrix_tags.difference(&toml_tags_ref).collect();
        let extra_in_toml: Vec<&&str> = toml_tags_ref.difference(&matrix_tags).collect();

        assert!(
            missing_in_toml.is_empty() && extra_in_toml.is_empty(),
            "docs/compat/tag_matrix.toml drifted from COMPAT_TAG_MATRIX.\n  \
             missing in TOML: {missing_in_toml:?}\n  \
             extra in TOML:   {extra_in_toml:?}"
        );
    }
}
