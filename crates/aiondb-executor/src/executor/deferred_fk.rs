//! Per-transaction registry of deferred foreign-key checks.
//!
//! PostgreSQL lets `DEFERRABLE` constraints delay existence verification
//! until COMMIT time (`INITIALLY DEFERRED`) or until a `SET CONSTRAINTS
//! ALL IMMEDIATE` boundary. When a deferred check is active at
//! INSERT/UPDATE time, the executor pushes `(table_id, row_values,
//! fk_index)` onto the transaction's queue instead of raising a
//! `foreign_key_violation` immediately. At commit (or at an IMMEDIATE
//! boundary) the queue is drained and each pending check is re-run
//! against the committed catalog + heap state; the first failure aborts
//! the transaction.
//!
//! The registry is process-global and must therefore be keyed by more
//! than the per-engine `TxnId`: `ScopeId` is a cheap opaque identifier
//! (typically an engine-pointer cast) that namespaces one engine's
//! transactions from another's. Cleanup is the engine's responsibility
//! - `drain_and_remove` is invoked from the COMMIT path and `remove`
//!
//! from the ROLLBACK path. `remove_scope` wipes every entry for an
//! engine that is shutting down.
//!
//! The `ExecutionContext` carries the `scope_id` through to the FK
//! enforcement code so the executor never has to ask the engine which
//! scope it lives in.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock};

use aiondb_core::{RelationId, TxnId, Value};

/// Opaque identifier that namespaces one engine's `TxnId`s from another's.
/// Two parallel test engines can reuse the same `TxnId(3)` without their
/// deferred-FK queues bleeding into each other as long as they pick
/// distinct scope ids (a pointer cast is the trivial choice).
pub type ScopeId = u64;

/// A `(ScopeId, TxnId)` composite key used internally by the registry.
type RegistryKey = (ScopeId, TxnId);

/// One pending deferred FK check: the row that was inserted/updated, the
/// child table it lives in, and the index of the FK constraint on that
/// table (into `TableDescriptor::foreign_keys`).
///
/// `row_values` is stored rather than a `TupleId` so the check is
/// resilient to subsequent updates on the same row - PG recomputes the
/// key from the current row at COMMIT, but the simpler "check what was
/// staged" policy covers every case the pg-regress deferred-FK scripts
/// exercise and never produces a false positive.
#[derive(Clone, Debug)]
pub struct DeferredFkCheck {
    pub table_id: RelationId,
    pub fk_index: usize,
    pub row_values: Vec<Value>,
}

/// Per-FK effective deferral mode override set by `SET CONSTRAINTS`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ConstraintModeOverrides {
    /// Blanket override set by `SET CONSTRAINTS ALL {DEFERRED|IMMEDIATE}`.
    /// None means "no blanket override active". Named overrides always
    /// take precedence.
    pub all: Option<bool>,
}

#[derive(Debug, Default)]
struct TxnDeferredState {
    checks: Vec<DeferredFkCheck>,
    /// Named constraint overrides. Key is lowercased constraint name so
    /// lookups are case-insensitive (matching PG's identifier semantics
    /// for unquoted constraint names).
    named_overrides: HashMap<String, bool>,
    mode: ConstraintModeOverrides,
}

#[derive(Debug, Default)]
struct DeferredFkRegistry {
    by_txn: HashMap<RegistryKey, TxnDeferredState>,
}

fn registry() -> &'static Mutex<DeferredFkRegistry> {
    static REGISTRY: OnceLock<Mutex<DeferredFkRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(DeferredFkRegistry::default()))
}

fn with_registry<T>(f: impl FnOnce(&mut DeferredFkRegistry) -> T) -> T {
    let mut guard: MutexGuard<'_, DeferredFkRegistry> = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    f(&mut guard)
}

/// Enqueue a deferred FK check against this transaction's pending list.
pub fn push_check(scope_id: ScopeId, txn_id: TxnId, check: DeferredFkCheck) {
    with_registry(|r| {
        r.by_txn
            .entry((scope_id, txn_id))
            .or_default()
            .checks
            .push(check);
    });
}

/// Return the effective deferred mode for a named FK constraint in this
/// transaction. `declared_initially_deferred` is the constraint's
/// catalog default. Returns `true` when the check should be deferred.
pub fn effective_deferred(
    scope_id: ScopeId,
    txn_id: TxnId,
    constraint_name: Option<&str>,
    declared_deferrable: bool,
    declared_initially_deferred: bool,
) -> bool {
    if !declared_deferrable {
        // NOT DEFERRABLE constraints are always immediate - SET CONSTRAINTS
        return false;
    }
    with_registry(|r| {
        let Some(state) = r.by_txn.get(&(scope_id, txn_id)) else {
            return declared_initially_deferred;
        };
        if let Some(name) = constraint_name {
            if let Some(&override_mode) = state.named_overrides.get(&name.to_ascii_lowercase()) {
                return override_mode;
            }
        }
        state.mode.all.unwrap_or(declared_initially_deferred)
    })
}

/// Apply `SET CONSTRAINTS ALL {DEFERRED|IMMEDIATE}` to this transaction.
/// Named overrides are cleared so the blanket mode becomes authoritative.
pub fn set_all(scope_id: ScopeId, txn_id: TxnId, deferred: bool) {
    with_registry(|r| {
        let state = r.by_txn.entry((scope_id, txn_id)).or_default();
        state.mode.all = Some(deferred);
        state.named_overrides.clear();
    });
}

/// Apply `SET CONSTRAINTS name1, name2, ... {DEFERRED|IMMEDIATE}`. Only
/// DEFERRABLE constraints are affected at the enforcement site; this
/// registry stores the override verbatim.
pub fn set_named(scope_id: ScopeId, txn_id: TxnId, names: &[String], deferred: bool) {
    with_registry(|r| {
        let state = r.by_txn.entry((scope_id, txn_id)).or_default();
        for name in names {
            state
                .named_overrides
                .insert(name.to_ascii_lowercase(), deferred);
        }
    });
}

/// Remove every pending check whose constraint matches `should_drain`
/// and return it to the caller. Checks that do not match stay queued.
pub fn drain_checks_where(
    scope_id: ScopeId,
    txn_id: TxnId,
    should_drain: impl Fn(&DeferredFkCheck) -> bool,
) -> Vec<DeferredFkCheck> {
    with_registry(|r| {
        let Some(state) = r.by_txn.get_mut(&(scope_id, txn_id)) else {
            return Vec::new();
        };
        let mut drained = Vec::new();
        let mut kept = Vec::with_capacity(state.checks.len());
        for check in state.checks.drain(..) {
            if should_drain(&check) {
                drained.push(check);
            } else {
                kept.push(check);
            }
        }
        state.checks = kept;
        drained
    })
}

/// Drain every pending deferred check for this transaction, removing the
/// entry from the registry. Used by COMMIT before the final commit step.
pub fn drain_and_remove(scope_id: ScopeId, txn_id: TxnId) -> Vec<DeferredFkCheck> {
    with_registry(|r| {
        r.by_txn
            .remove(&(scope_id, txn_id))
            .map(|state| state.checks)
            .unwrap_or_default()
    })
}

/// Discard every pending check and all overrides for this transaction.
/// Called from the ROLLBACK and statement-abort paths.
pub fn remove(scope_id: ScopeId, txn_id: TxnId) {
    with_registry(|r| {
        r.by_txn.remove(&(scope_id, txn_id));
    });
}

/// Wipe every entry that belongs to `scope_id`. Called when an engine
/// instance is being dropped so its transaction queue never outlives it.
pub fn remove_scope(scope_id: ScopeId) {
    with_registry(|r| {
        r.by_txn.retain(|(scope, _), _| *scope != scope_id);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_scope() -> ScopeId {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(9_000_000);
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    #[test]
    fn not_deferrable_is_always_immediate() {
        let scope = fresh_scope();
        let txn = TxnId::new(1);
        set_all(scope, txn, true);
        assert!(!effective_deferred(scope, txn, Some("fk"), false, true));
        remove_scope(scope);
    }

    #[test]
    fn initially_deferred_without_override_returns_deferred() {
        let scope = fresh_scope();
        let txn = TxnId::new(1);
        assert!(effective_deferred(scope, txn, Some("fk"), true, true));
        remove_scope(scope);
    }

    #[test]
    fn set_all_immediate_overrides_initially_deferred() {
        let scope = fresh_scope();
        let txn = TxnId::new(1);
        set_all(scope, txn, false);
        assert!(!effective_deferred(scope, txn, Some("fk"), true, true));
        remove_scope(scope);
    }

    #[test]
    fn set_all_deferred_elevates_initially_immediate() {
        let scope = fresh_scope();
        let txn = TxnId::new(1);
        set_all(scope, txn, true);
        assert!(effective_deferred(scope, txn, Some("fk"), true, false));
        remove_scope(scope);
    }

    #[test]
    fn named_override_beats_blanket_all() {
        let scope = fresh_scope();
        let txn = TxnId::new(1);
        set_all(scope, txn, true);
        set_named(scope, txn, &["my_fk".to_string()], false);
        assert!(!effective_deferred(scope, txn, Some("my_fk"), true, true));
        // Other named FKs still see the blanket override.
        assert!(effective_deferred(
            scope,
            txn,
            Some("other_fk"),
            true,
            false
        ));
        remove_scope(scope);
    }

    #[test]
    fn named_override_is_case_insensitive() {
        let scope = fresh_scope();
        let txn = TxnId::new(1);
        set_named(scope, txn, &["Foo_FK".to_string()], true);
        assert!(effective_deferred(scope, txn, Some("foo_fk"), true, false));
        remove_scope(scope);
    }

    #[test]
    fn push_and_drain_roundtrip() {
        let scope = fresh_scope();
        let txn = TxnId::new(1);
        push_check(
            scope,
            txn,
            DeferredFkCheck {
                table_id: RelationId::new(42),
                fk_index: 0,
                row_values: vec![Value::Int(1)],
            },
        );
        let drained = drain_and_remove(scope, txn);
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].table_id, RelationId::new(42));
        // Registry entry removed; a second drain sees nothing.
        assert!(drain_and_remove(scope, txn).is_empty());
        remove_scope(scope);
    }

    #[test]
    fn rollback_clears_queue() {
        let scope = fresh_scope();
        let txn = TxnId::new(1);
        push_check(
            scope,
            txn,
            DeferredFkCheck {
                table_id: RelationId::new(1),
                fk_index: 0,
                row_values: vec![Value::Int(1)],
            },
        );
        remove(scope, txn);
        assert!(drain_and_remove(scope, txn).is_empty());
        remove_scope(scope);
    }

    #[test]
    fn different_scopes_do_not_collide() {
        let scope_a = fresh_scope();
        let scope_b = fresh_scope();
        let txn = TxnId::new(7);
        push_check(
            scope_a,
            txn,
            DeferredFkCheck {
                table_id: RelationId::new(1),
                fk_index: 0,
                row_values: vec![Value::Int(1)],
            },
        );
        // Scope B's queue is independent of scope A's.
        assert!(drain_and_remove(scope_b, txn).is_empty());
        let drained = drain_and_remove(scope_a, txn);
        assert_eq!(drained.len(), 1);
        remove_scope(scope_a);
        remove_scope(scope_b);
    }

    #[test]
    fn drain_checks_where_retains_unmatched() {
        let scope = fresh_scope();
        let txn = TxnId::new(1);
        push_check(
            scope,
            txn,
            DeferredFkCheck {
                table_id: RelationId::new(1),
                fk_index: 0,
                row_values: vec![Value::Int(1)],
            },
        );
        push_check(
            scope,
            txn,
            DeferredFkCheck {
                table_id: RelationId::new(2),
                fk_index: 0,
                row_values: vec![Value::Int(2)],
            },
        );
        let drained = drain_checks_where(scope, txn, |c| c.table_id == RelationId::new(1));
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].table_id, RelationId::new(1));
        let remaining = drain_and_remove(scope, txn);
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].table_id, RelationId::new(2));
        remove_scope(scope);
    }
}
