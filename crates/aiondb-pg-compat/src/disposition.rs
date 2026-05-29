//! Typed classification of every statement the engine hands to the
//! compat layer.
//!
//! Historically the engine's query-API cascade made routing decisions by
//! calling a handful of `is_*` helpers that each inspected `Statement`
//! variants and compatibility tag strings. Drift between those helpers
//! could route a statement through one helper, miss its post-hook in
//! another, and emit `command_ok` without running the real side effect.
//!
//! Centralised decision:
//!
//! * [`CompatDisposition`] - the buckets every statement falls into
//!   before execution (`Native`, `CompatHandler`, post-statement hook for
//!   sensitive CREATE families and shared DROP IF EXISTS, etc.).

use aiondb_parser::Statement;

use crate::command::CompatCommand;
use crate::compat_tag_matrix::{compat_tag_behavior, CompatTagBehavior};

/// Exclusive routing buckets for the compat surface. One statement maps
/// to exactly one variant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompatDisposition {
    /// Not a compat statement - run through the native planner/executor.
    Native,
    /// Registered compat command with a typed handler. The inner
    /// `CompatCommand` identifies the variant so the dispatcher can fan
    /// out without re-parsing the tag string.
    CompatHandler(CompatCommand),
    /// Sentinel for the `CompatTagBehavior::IntentionalNoop` bucket;
    /// the router rejects with `feature_not_supported`.
    IntentionalNoop,
    /// Tag reached the compat surface but has no registered behaviour.
    /// The caller must surface `feature_not_supported`.
    Reject,
}

impl CompatDisposition {
    #[must_use]
    pub const fn is_native(self) -> bool {
        matches!(self, Self::Native)
    }

    /// Extract the typed `CompatCommand` when the disposition is
    /// `CompatHandler`; `None` otherwise.
    #[must_use]
    pub const fn compat_command(self) -> Option<CompatCommand> {
        match self {
            Self::CompatHandler(command) => Some(command),
            _ => None,
        }
    }
}

/// Classify a parsed `Statement` into exactly one `CompatDisposition`.
///
/// Non-compat statements default to `Native`. Compatibility tags are resolved
/// against:
///   1. `CompatCommand::from_tag` - registered active compat command →
///      `CompatHandler(command)`. The active baseline is currently empty.
///      `Reject`) or `ExplicitNotSupported` (→ `Reject`).
///   3. Fallback for tags that live in the matrix as `ImplementedReal`
///      but do not map to a `CompatCommand` variant - those keep
///      flowing through the engine's tag-driven dispatch,
///      so we return `Native` to let the normal cascade handle them (the
///      engine still routes via `compat_tag_dispatch`).
#[must_use]
pub fn classify(statement: &Statement) -> CompatDisposition {
    if matches!(
        statement,
        Statement::CreateType(_)
            | Statement::AlterType(_)
            | Statement::DropType(_)
            | Statement::CreateDomain(_)
            | Statement::AlterDomain(_)
            | Statement::DropDomain(_)
            | Statement::CreateCast(_)
            | Statement::DropCast(_)
            | Statement::CreateRule(_)
            | Statement::AlterRule(_)
            | Statement::DropRule(_)
            | Statement::CreatePolicy(_)
            | Statement::AlterPolicy(_)
            | Statement::DropPolicy(_)
            | Statement::CreatePublication(_)
            | Statement::AlterPublication(_)
            | Statement::DropPublication(_)
            | Statement::CreateSubscription(_)
            | Statement::AlterSubscription(_)
            | Statement::DropSubscription(_)
            | Statement::CreateServer(_)
            | Statement::AlterServer(_)
            | Statement::DropServer(_)
            | Statement::CreateUserMapping(_)
            | Statement::AlterUserMapping(_)
            | Statement::DropUserMapping(_)
            | Statement::CreateForeignTable(_)
            | Statement::AlterForeignTable(_)
            | Statement::DropForeignTable(_)
            | Statement::CreateForeignDataWrapper(_)
            | Statement::AlterForeignDataWrapper(_)
            | Statement::DropForeignDataWrapper(_)
            | Statement::CreateCollation(_)
            | Statement::AlterCollation(_)
            | Statement::DropCollation(_)
            | Statement::CreateStatistics(_)
            | Statement::AlterStatistics(_)
            | Statement::DropStatistics(_)
            | Statement::CreateTablespace(_)
            | Statement::AlterTablespace(_)
            | Statement::DropTablespace(_)
    ) {
        return CompatDisposition::Native;
    }
    let Some(tag) = statement.compat_tag() else {
        return CompatDisposition::Native;
    };
    if let Some(command) = CompatCommand::from_tag(tag) {
        return CompatDisposition::CompatHandler(command);
    }
    match compat_tag_behavior(tag) {
        CompatTagBehavior::IntentionalNoop => CompatDisposition::Reject,
        CompatTagBehavior::ExplicitNotSupported => CompatDisposition::Reject,
        // The tag has an `ImplementedReal` matrix entry but no typed
        // variant. Tag-string dispatch still handles it; surface
        // `Native` so routing continues.
        CompatTagBehavior::ImplementedReal => CompatDisposition::Native,
    }
}

/// Which post-statement hook (if any) the engine must run for a
/// compatibility tag it handles inline in the compat cascade.
///
/// * `None` - the tag does not require special post-statement work.
/// * `CreateWithPostHook` - engine-owned compat CREATE families where the
///   post-hook validates referenced targets and persists the compat
///   registry entry.
/// * `DropIfExistsWithPostHook` - DROP tags that first fall through
///   the drop-if-exists notice handler and then run
///   the post-hook before emitting `command_ok` (DROP CAST,
///   DROP AGGREGATE, DROP OPERATOR).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompatNoopPostHook {
    None,
    CreateWithPostHook,
    DropIfExistsWithPostHook,
}

/// Return the post-hook class for a given compatibility tag. Unknown
/// tags default to `None`. Single source of truth - replaces the older
/// `engine/query_api.rs`.
#[must_use]
pub fn noop_post_hook(tag: &str) -> CompatNoopPostHook {
    match tag {
        // POLICY: real RLS qual injection + catalog persistence.
        // STATISTICS: pg_statistic_ext views consume the rows.
        // AGGREGATE: compat_aggregate_rewrites is a real rewrite path
        // used by SELECT planning.
        // The remaining session-only stubs (PROCEDURE/PUBLICATION/
        // SUBSCRIPTION/SERVER/USER MAPPING/FOREIGN TABLE/
        // FOREIGN DATA WRAPPER) are not routed through this hook, so
        // the compat router falls through to the terminal
        // `unsupported_compatibility_command` reject instead of
        // emitting a fake success tag.
        "CREATE AGGREGATE" | "CREATE POLICY" | "CREATE STATISTICS" | "CREATE TEXT SEARCH" => {
            CompatNoopPostHook::CreateWithPostHook
        }
        // DROP AGGREGATE: cleans up the rewrite registry.
        // DROP OPERATOR: typed-family dispatcher uses this hook to emit
        // the IF EXISTS notice.
        // DROP PROCEDURE is not routed through this hook either.
        "DROP AGGREGATE" | "DROP OPERATOR" => CompatNoopPostHook::DropIfExistsWithPostHook,
        _ => CompatNoopPostHook::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command_noop(tag: &'static str) -> Statement {
        Statement::CompatParserStub {
            tag: tag.to_owned(),
            notice: None,
            span: aiondb_parser::Span::default(),
        }
    }

    #[test]
    fn classify_command_noop_with_variant_routes_to_compat_handler() {
        // Every typed family (TYPE/DOMAIN/CAST/RULE/DATABASE +
        // POLICY/PUBLICATION/… + AGGREGATE/PROCEDURE/OPERATOR/…) is
        // parsed as `Statement::*` directly, so raw compatibility tags
        // in those families classify as `Reject`. ALTER TABLE keeps a
        // narrow implemented compat subset, so parser stubs stay
        // `Native` and are handled by the engine compat cascade.
        let statement = command_noop("ALTER TABLE");
        assert_eq!(classify(&statement), CompatDisposition::Native);
    }

    #[test]
    fn classify_compat_noop_tag_as_reject() {
        let statement = command_noop("GRANT");
        assert_eq!(classify(&statement), CompatDisposition::Reject);
    }

    #[test]
    fn classify_unregistered_tag_rejects() {
        let statement = command_noop("CREATE GALAXY");
        assert_eq!(classify(&statement), CompatDisposition::Reject);
    }

    #[test]
    fn classify_non_commandnoop_is_native() {
        let statement = Statement::Begin {
            mode: Default::default(),
            read_only: None,
            deferrable: None,
            span: aiondb_parser::Span::default(),
        };
        assert_eq!(classify(&statement), CompatDisposition::Native);
    }

    #[test]
    fn classify_typed_pg_object_command_is_native() {
        let statements =
            aiondb_parser::parse_sql("CREATE FOREIGN TABLE ft (id int) SERVER srv").expect("parse");
        assert_eq!(classify(&statements[0]), CompatDisposition::Native);
    }

    #[test]
    fn classify_matrix_only_tag_is_native() {
        // "ALTER TABLE" is matrix-registered as ImplementedReal.
        let statement = command_noop("ALTER TABLE");
        assert_eq!(classify(&statement), CompatDisposition::Native);
    }

    #[test]
    fn noop_post_hook_covers_create_and_drop_tags() {
        assert_eq!(
            noop_post_hook("CREATE AGGREGATE"),
            CompatNoopPostHook::CreateWithPostHook
        );
        assert_eq!(
            noop_post_hook("DROP AGGREGATE"),
            CompatNoopPostHook::DropIfExistsWithPostHook
        );
        assert_eq!(
            noop_post_hook("CREATE POLICY"),
            CompatNoopPostHook::CreateWithPostHook
        );
        assert_eq!(
            noop_post_hook("CREATE TEXT SEARCH"),
            CompatNoopPostHook::CreateWithPostHook
        );
        assert_eq!(noop_post_hook("SELECT"), CompatNoopPostHook::None);
    }

    /// There are no active `CompatCommand` variants left. Implemented real
    /// tags are routed by the engine compat cascade, not by command variants.
    #[test]
    fn no_active_variants_classify_as_compat_handler() {
        assert!(CompatCommand::ALL.is_empty());
    }
}
