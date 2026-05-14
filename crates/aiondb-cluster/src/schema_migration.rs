//! Distributed schema migration coordinator.
//!
//! Implements the multi-phase online schema change protocol made
//! famous by Google's F1 / Spanner / CockroachDB :
//!
//! ```text
//!   DELETE-ONLY -> WRITE-ONLY -> WRITE-AND-READ -> PUBLIC
//! ```
//!
//! At each phase boundary the coordinator pauses until every node has
//! acknowledged the transition, ensuring that no node observes the
//! schema at two contradictory phases simultaneously. A rollback
//! flow is provided so half-applied migrations can be reversed
//! cleanly.
//!
//! This module is the migration **state machine** -- the actual DDL
//! application lives in `aiondb-catalog`. Coordinators consult this
//! module to decide what nodes can do at each step.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use aiondb_core::{DbError, DbResult};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum SchemaPhase {
    /// Element does not exist yet from any node's view.
    Absent,
    /// Element exists, only deletes are routed to it.
    DeleteOnly,
    /// Element exists, deletes and writes are routed to it; reads
    /// must NOT yet observe it.
    WriteOnly,
    /// Element exists, all writes routed to it, reads also see it.
    WriteAndRead,
    /// Element is fully promoted. Equivalent to a finalized state.
    Public,
    /// Migration was rolled back.
    Aborted,
}

impl SchemaPhase {
    pub fn next(self) -> Option<SchemaPhase> {
        Some(match self {
            SchemaPhase::Absent => SchemaPhase::DeleteOnly,
            SchemaPhase::DeleteOnly => SchemaPhase::WriteOnly,
            SchemaPhase::WriteOnly => SchemaPhase::WriteAndRead,
            SchemaPhase::WriteAndRead => SchemaPhase::Public,
            SchemaPhase::Public | SchemaPhase::Aborted => return None,
        })
    }
}

#[derive(Clone, Debug)]
struct MigrationState {
    phase: SchemaPhase,
    /// Nodes that have acked the current phase.
    acked_nodes: HashSet<u64>,
    expected_nodes: HashSet<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct SchemaMigrationCoordinator {
    inner: Arc<std::sync::Mutex<BTreeMap<String, MigrationState>>>,
}

impl SchemaMigrationCoordinator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a migration for `element_key` (e.g. `"table:users:column:age"`).
    pub fn start(
        &self,
        element_key: impl Into<String>,
        expected_nodes: HashSet<u64>,
    ) -> DbResult<()> {
        let key = element_key.into();
        let mut guard = self.lock();
        if guard.contains_key(&key) {
            return Err(DbError::internal(format!(
                "migration for {key} already in progress"
            )));
        }
        guard.insert(
            key,
            MigrationState {
                phase: SchemaPhase::Absent,
                acked_nodes: HashSet::new(),
                expected_nodes,
            },
        );
        Ok(())
    }

    pub fn phase(&self, key: &str) -> Option<SchemaPhase> {
        self.lock().get(key).map(|s| s.phase)
    }

    /// A node acknowledges the current phase. Returns the new phase
    /// when every expected node has ack'd, else returns `None`.
    pub fn ack(&self, key: &str, node_id: u64) -> DbResult<Option<SchemaPhase>> {
        let mut guard = self.lock();
        let state = guard
            .get_mut(key)
            .ok_or_else(|| DbError::internal(format!("unknown migration {key}")))?;
        state.acked_nodes.insert(node_id);
        if state.acked_nodes == state.expected_nodes {
            let next = state.phase.next().ok_or_else(|| {
                DbError::internal(format!("migration {key} is already in terminal phase"))
            })?;
            state.phase = next;
            state.acked_nodes.clear();
            return Ok(Some(next));
        }
        Ok(None)
    }

    /// Force-advance to the next phase (admin override). Resets acks.
    pub fn advance(&self, key: &str) -> DbResult<SchemaPhase> {
        let mut guard = self.lock();
        let state = guard
            .get_mut(key)
            .ok_or_else(|| DbError::internal(format!("unknown migration {key}")))?;
        let next = state
            .phase
            .next()
            .ok_or_else(|| DbError::internal(format!("migration {key} is already terminal")))?;
        state.phase = next;
        state.acked_nodes.clear();
        Ok(next)
    }

    /// Abort an in-flight migration.
    pub fn abort(&self, key: &str) -> DbResult<()> {
        let mut guard = self.lock();
        let state = guard
            .get_mut(key)
            .ok_or_else(|| DbError::internal(format!("unknown migration {key}")))?;
        state.phase = SchemaPhase::Aborted;
        Ok(())
    }

    /// Snapshot of every active migration.
    pub fn snapshot(&self) -> Vec<(String, SchemaPhase)> {
        self.lock()
            .iter()
            .map(|(k, s)| (k.clone(), s.phase))
            .collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, MigrationState>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_phase_progression_advances_only_when_every_node_acks() {
        let c = SchemaMigrationCoordinator::new();
        let mut expected = HashSet::new();
        expected.insert(1);
        expected.insert(2);
        c.start("table:users:add_column", expected.clone()).unwrap();
        assert_eq!(c.phase("table:users:add_column"), Some(SchemaPhase::Absent));

        assert_eq!(c.ack("table:users:add_column", 1).unwrap(), None);
        assert_eq!(
            c.ack("table:users:add_column", 2).unwrap(),
            Some(SchemaPhase::DeleteOnly)
        );
        assert_eq!(
            c.phase("table:users:add_column"),
            Some(SchemaPhase::DeleteOnly)
        );

        c.ack("table:users:add_column", 1).unwrap();
        c.ack("table:users:add_column", 2).unwrap();
        c.ack("table:users:add_column", 1).unwrap();
        c.ack("table:users:add_column", 2).unwrap();
        c.ack("table:users:add_column", 1).unwrap();
        c.ack("table:users:add_column", 2).unwrap();
        assert_eq!(c.phase("table:users:add_column"), Some(SchemaPhase::Public));
    }

    #[test]
    fn abort_freezes_state() {
        let c = SchemaMigrationCoordinator::new();
        let mut expected = HashSet::new();
        expected.insert(1);
        c.start("t", expected).unwrap();
        c.abort("t").unwrap();
        assert_eq!(c.phase("t"), Some(SchemaPhase::Aborted));
        assert!(c.advance("t").is_err());
    }

    #[test]
    fn duplicate_start_is_rejected() {
        let c = SchemaMigrationCoordinator::new();
        c.start("t", HashSet::new()).unwrap();
        assert!(c.start("t", HashSet::new()).is_err());
    }

    #[test]
    fn snapshot_lists_every_active_migration() {
        let c = SchemaMigrationCoordinator::new();
        c.start("a", HashSet::new()).unwrap();
        c.start("b", HashSet::new()).unwrap();
        let snap = c.snapshot();
        assert_eq!(snap.len(), 2);
        assert!(snap.iter().any(|(k, _)| k == "a"));
        assert!(snap.iter().any(|(k, _)| k == "b"));
    }

    #[test]
    fn admin_advance_skips_acks() {
        let c = SchemaMigrationCoordinator::new();
        c.start("t", HashSet::new()).unwrap();
        assert_eq!(c.advance("t").unwrap(), SchemaPhase::DeleteOnly);
        assert_eq!(c.advance("t").unwrap(), SchemaPhase::WriteOnly);
    }
}
