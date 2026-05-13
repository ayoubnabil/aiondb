use aiondb_catalog::SchemaDescriptor;
use aiondb_core::SchemaId;

use crate::{system_tables::SYSTEM_SCHEMA_NAMES, CatalogState, CatalogStore};

pub(crate) fn bootstrap_state(state: &mut CatalogState) {
    for schema_name in SYSTEM_SCHEMA_NAMES {
        let schema_id = CatalogStore::next_schema_id(state, SchemaId::default());
        let normalized_name = CatalogStore::normalize_identifier(schema_name);
        state.schemas_by_id.insert(
            schema_id,
            SchemaDescriptor {
                schema_id,
                name: (*schema_name).to_owned(),
            },
        );
        state.schema_names.insert(normalized_name, schema_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CatalogState;

    // ===================================================================
    // bootstrap_state: happy path
    // ===================================================================

    #[test]
    fn bootstrap_creates_all_system_schemas() {
        let mut state = CatalogState::default();
        bootstrap_state(&mut state);
        // Every schema declared in SYSTEM_SCHEMA_NAMES must be present.
        for schema_name in SYSTEM_SCHEMA_NAMES {
            let normalized = CatalogStore::normalize_identifier(schema_name);
            assert!(
                state.schema_names.contains_key(&normalized),
                "schema '{schema_name}' should exist after bootstrap"
            );
        }
    }

    #[test]
    fn bootstrap_public_schema_has_id_1() {
        let mut state = CatalogState::default();
        bootstrap_state(&mut state);
        let public_id = state.schema_names.get("public").copied();
        assert_eq!(public_id, Some(SchemaId::new(1)));
    }

    #[test]
    fn bootstrap_schema_count_matches_system_schema_names() {
        let mut state = CatalogState::default();
        bootstrap_state(&mut state);
        assert_eq!(state.schemas_by_id.len(), SYSTEM_SCHEMA_NAMES.len());
        assert_eq!(state.schema_names.len(), SYSTEM_SCHEMA_NAMES.len());
    }

    #[test]
    fn bootstrap_schema_descriptor_has_correct_name() {
        let mut state = CatalogState::default();
        bootstrap_state(&mut state);
        let public_id = state.schema_names["public"];
        let descriptor = state.schemas_by_id.get(&public_id).unwrap();
        assert_eq!(descriptor.name, "public");
        assert_eq!(descriptor.schema_id, public_id);
    }

    #[test]
    fn bootstrap_advances_next_schema_id() {
        let mut state = CatalogState::default();
        assert_eq!(state.next_schema_id, 1);
        bootstrap_state(&mut state);
        // After allocating SYSTEM_SCHEMA_NAMES.len() schemas, counter should advance.
        assert_eq!(state.next_schema_id, (SYSTEM_SCHEMA_NAMES.len() as u64) + 1);
    }

    // ===================================================================
    // bootstrap_state: does NOT touch non-schema collections
    // ===================================================================

    #[test]
    fn bootstrap_leaves_tables_empty() {
        let mut state = CatalogState::default();
        bootstrap_state(&mut state);
        assert!(state.tables_by_id.is_empty());
        assert!(state.table_names.is_empty());
    }

    #[test]
    fn bootstrap_leaves_indexes_empty() {
        let mut state = CatalogState::default();
        bootstrap_state(&mut state);
        assert!(state.indexes_by_id.is_empty());
        assert!(state.index_names.is_empty());
        assert!(state.indexes_by_table.is_empty());
    }

    #[test]
    fn bootstrap_leaves_sequences_empty() {
        let mut state = CatalogState::default();
        bootstrap_state(&mut state);
        assert!(state.sequences_by_id.is_empty());
        assert!(state.sequence_names.is_empty());
        assert!(state.sequence_values.is_empty());
    }

    #[test]
    fn bootstrap_leaves_statistics_empty() {
        let mut state = CatalogState::default();
        bootstrap_state(&mut state);
        assert!(state.statistics.is_empty());
    }

    #[test]
    fn bootstrap_does_not_change_revision() {
        let mut state = CatalogState::default();
        assert_eq!(state.revision, 0);
        bootstrap_state(&mut state);
        assert_eq!(state.revision, 0);
    }

    // ===================================================================
    // bootstrap_state: idempotency / double-call
    // ===================================================================

    #[test]
    fn bootstrap_called_twice_doubles_schemas() {
        // It will add duplicate schemas under new IDs because next_schema_id advances.
        let mut state = CatalogState::default();
        bootstrap_state(&mut state);
        let count_after_first = state.schemas_by_id.len();
        bootstrap_state(&mut state);
        // schema_names uses the same normalized key, so gets overwritten.
        assert_eq!(state.schema_names.len(), SYSTEM_SCHEMA_NAMES.len());
        // But schemas_by_id will have more entries (old + new IDs).
        assert_eq!(
            state.schemas_by_id.len(),
            count_after_first + SYSTEM_SCHEMA_NAMES.len()
        );
    }

    // ===================================================================
    // bootstrap_state: schema lookup is case-insensitive
    // ===================================================================

    #[test]
    fn bootstrap_schema_lookup_is_lowercased() {
        let mut state = CatalogState::default();
        bootstrap_state(&mut state);
        // "public" is stored lowercased; "PUBLIC" should not match directly.
        assert!(state.schema_names.contains_key("public"));
        assert!(!state.schema_names.contains_key("PUBLIC"));
    }
}
