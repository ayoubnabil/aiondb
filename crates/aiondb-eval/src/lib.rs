#![allow(
    clippy::assigning_clones,
    clippy::cast_lossless,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::doc_markdown,
    clippy::float_cmp,
    clippy::items_after_statements,
    clippy::manual_let_else,
    clippy::manual_midpoint,
    clippy::match_same_arms,
    clippy::map_unwrap_or,
    clippy::missing_errors_doc,
    clippy::must_use_candidate,
    clippy::needless_pass_by_value,
    clippy::redundant_closure_for_method_calls,
    clippy::semicolon_if_nothing_returned,
    clippy::similar_names,
    clippy::single_match_else,
    clippy::struct_excessive_bools,
    clippy::too_many_lines,
    clippy::trivially_copy_pass_by_ref,
    clippy::uninlined_format_args,
    clippy::unnecessary_wraps,
    clippy::unreadable_literal,
    clippy::wildcard_imports
)]

pub mod async_notify;
pub mod cancel;
pub mod coercions;
pub mod eval;
pub mod functions;
pub mod hash_key;
mod plpgsql_compat_cursors;
mod regex_cache;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

pub use coercions::coerce_value;
pub use eval::scalar_functions::eval_full_text_match_rank;
pub use eval::{
    compare_runtime_values, compat_display_type_name, compat_type_name_for_data_type,
    current_database_name, current_date_order, current_interval_style, current_lo_session_key,
    current_schema_name, current_search_path_schemas, current_session_context,
    current_temporal_session_context, current_time_zone, enforce_domain_constraints,
    eval_pg_ls_dir_with_base_dir, eval_pg_read_binary_file_with_base_dir,
    eval_pg_read_file_with_base_dir, global_compat_constraint_defs, global_compat_index_defs,
    is_builtin_compat_type, normalize_compat_type_name, scale_interval,
    set_global_compat_definition_caches, sql_like_match, try_canonicalize_range_or_multirange_text,
    validate_geometric_compat_literal, visible_session_schema_name, with_current_session_context,
    with_session_context, ClusterDatabaseSummary, CompatCastContext, CompatCastMethod,
    CompatUserCast, CompatUserType, CompatUserTypeField, DomainConstraint, DomainDef,
    EvalSessionContext, EvalTemporalSessionContext, ExpressionEvaluator,
};
pub use functions::{FunctionInfo, FunctionRegistry};
pub use hash_key::{build_hash_key, ValueHashKey};
pub use plpgsql_compat_cursors::{
    plpgsql_clear_compat_cursors, plpgsql_close_compat_cursor, plpgsql_fetch_compat_cursor,
    plpgsql_move_compat_cursor, plpgsql_store_compat_cursor,
};

static PG_STATISTICS_OBJDEF_REGISTRY: OnceLock<Mutex<HashMap<i32, String>>> = OnceLock::new();

pub fn register_pg_statistics_objdef(oid: i32, definition: String) {
    let registry = PG_STATISTICS_OBJDEF_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut guard) = registry.lock() {
        guard.insert(oid, definition);
    }
}

pub fn lookup_pg_statistics_objdef(oid: i32) -> Option<String> {
    PG_STATISTICS_OBJDEF_REGISTRY
        .get()
        .and_then(|registry| registry.lock().ok()?.get(&oid).cloned())
}

pub fn lookup_pg_statistics_objdef_columns(oid: i32) -> Option<String> {
    let definition = lookup_pg_statistics_objdef(oid)?;
    let after_prefix = definition.strip_prefix("CREATE STATISTICS ")?;
    let on_index = after_prefix.find(" ON ")?;
    let after_name = &after_prefix[on_index + " ON ".len()..];
    let from_index = after_name.rfind(" FROM ")?;
    Some(format!("ON {}", after_name[..from_index].trim()))
}

// ---------------------------------------------------------------------------
// Extension registry integration
// ---------------------------------------------------------------------------

// Thread-local reference to the extension registry, set by the engine before
// executing queries so that the eval layer can dispatch extension functions.
std::thread_local! {
    static EXTENSION_REGISTRY: std::cell::RefCell<Option<Arc<aiondb_extension::ExtensionRegistry>>> =
        const { std::cell::RefCell::new(None) };
}

struct ExtensionRegistryGuard(Option<Arc<aiondb_extension::ExtensionRegistry>>);

impl Drop for ExtensionRegistryGuard {
    fn drop(&mut self) {
        EXTENSION_REGISTRY.with(|cell| {
            *cell.borrow_mut() = self.0.take();
        });
    }
}

/// Install the extension registry for the current thread.  Called by the
/// engine before running queries.
pub fn set_extension_registry(registry: Arc<aiondb_extension::ExtensionRegistry>) {
    EXTENSION_REGISTRY.with(|cell| {
        *cell.borrow_mut() = Some(registry);
    });
}

/// Install the extension registry for the current thread while running `f`,
/// then restore the previous registry afterwards.
pub fn with_extension_registry<T>(
    registry: Arc<aiondb_extension::ExtensionRegistry>,
    f: impl FnOnce() -> T,
) -> T {
    let previous = EXTENSION_REGISTRY.with(|cell| cell.replace(Some(registry)));
    let _guard = ExtensionRegistryGuard(previous);
    f()
}

/// Retrieve the extension registry for the current thread, if set.
pub fn extension_registry() -> Option<Arc<aiondb_extension::ExtensionRegistry>> {
    EXTENSION_REGISTRY.with(|cell| cell.borrow().clone())
}

// ---------------------------------------------------------------------------
// User-function inlining stack
// ---------------------------------------------------------------------------
//
// Tracks the chain of user-defined functions whose bodies are currently being
// compiled or evaluated on this thread. The planner consults this stack when
// resolving CREATE CAST overrides so it can avoid substituting a cast with the
// very function whose body it is in the middle of compiling - without this
// guard, casts whose implementor uses the cast operator on its parameter would
// recurse infinitely (the canonical example is `CAST (int4 AS text) WITH
// FUNCTION fn` whose body contains `$1::text`).
std::thread_local! {
    static INLINING_USER_FUNCTIONS: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// RAII guard returned by [`enter_inlining_user_function`].
pub struct InliningUserFunctionGuard {
    _private: (),
}

impl Drop for InliningUserFunctionGuard {
    fn drop(&mut self) {
        INLINING_USER_FUNCTIONS.with(|stack| {
            stack.borrow_mut().pop();
        });
    }
}

/// Push `name` onto the user-function inlining stack for the current thread.
/// The returned guard pops the entry when dropped.
#[must_use]
pub fn enter_inlining_user_function(name: &str) -> InliningUserFunctionGuard {
    INLINING_USER_FUNCTIONS.with(|stack| {
        stack.borrow_mut().push(name.to_owned());
    });
    InliningUserFunctionGuard { _private: () }
}

/// Returns true when `name` is anywhere on the user-function inlining stack
/// for the current thread (case-insensitive comparison, matching PG's
/// case-insensitive function name resolution).
#[must_use]
pub fn is_inlining_user_function(name: &str) -> bool {
    INLINING_USER_FUNCTIONS.with(|stack| {
        stack
            .borrow()
            .iter()
            .any(|entry| entry.eq_ignore_ascii_case(name))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_extension_registry_restores_previous_registry() {
        let initial = Arc::new(aiondb_extension::ExtensionRegistry::new());
        let nested = Arc::new(aiondb_extension::ExtensionRegistry::new());

        set_extension_registry(Arc::clone(&initial));
        with_extension_registry(Arc::clone(&nested), || {
            let current = extension_registry().expect("nested registry");
            assert!(Arc::ptr_eq(&current, &nested));
        });

        let restored = extension_registry().expect("restored registry");
        assert!(Arc::ptr_eq(&restored, &initial));
    }
}
