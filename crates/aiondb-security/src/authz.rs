#![allow(clippy::missing_errors_doc)]

use aiondb_core::{DatabaseId, DbResult, IndexId, RelationId, SchemaId, SequenceId};

use crate::AuthenticatedIdentity;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Action {
    Connect,
    Select,
    Insert,
    Update,
    Delete,
    Create,
    Drop,
    Alter,
    Execute,
    Usage,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum AccessTarget {
    Database(DatabaseId),
    Schema(SchemaId),
    Relation(RelationId),
    Index(IndexId),
    Sequence(SequenceId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessRequest {
    pub action: Action,
    pub target: Option<AccessTarget>,
}

#[allow(clippy::missing_errors_doc)]
pub trait Authorizer: Send + Sync {
    fn authorize(&self, identity: &AuthenticatedIdentity, request: &AccessRequest) -> DbResult<()>;

    fn is_noop(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_core::{DatabaseId, IndexId, RelationId, SchemaId, SequenceId};

    // --- Action: all variants distinct ---
    #[test]
    fn action_all_variants_distinct() {
        let variants = [
            Action::Connect,
            Action::Select,
            Action::Insert,
            Action::Update,
            Action::Delete,
            Action::Create,
            Action::Drop,
            Action::Alter,
            Action::Execute,
            Action::Usage,
        ];
        for i in 0..variants.len() {
            for j in (i + 1)..variants.len() {
                assert_ne!(
                    variants[i], variants[j],
                    "Action variants at index {i} and {j} should differ"
                );
            }
        }
    }

    // --- AccessTarget: all variants constructible ---
    #[test]
    fn access_target_all_variants_constructible() {
        let _db = AccessTarget::Database(DatabaseId::new(1));
        let _schema = AccessTarget::Schema(SchemaId::new(2));
        let _rel = AccessTarget::Relation(RelationId::new(3));
        let _idx = AccessTarget::Index(IndexId::new(4));
        let _seq = AccessTarget::Sequence(SequenceId::new(5));
    }

    // --- AccessRequest construction ---
    #[test]
    fn access_request_construction() {
        let req = AccessRequest {
            action: Action::Select,
            target: Some(AccessTarget::Relation(RelationId::new(42))),
        };
        assert_eq!(req.action, Action::Select);
        assert!(req.target.is_some());

        let req_no_target = AccessRequest {
            action: Action::Connect,
            target: None,
        };
        assert_eq!(req_no_target.action, Action::Connect);
        assert!(req_no_target.target.is_none());
    }

    // ===================================================================
    // NEW EDGE CASE TESTS
    // ===================================================================

    // --- Action: each variant equals itself ---

    #[test]
    fn action_connect_eq_self() {
        assert_eq!(Action::Connect, Action::Connect);
    }

    #[test]
    fn action_select_eq_self() {
        assert_eq!(Action::Select, Action::Select);
    }

    #[test]
    fn action_insert_eq_self() {
        assert_eq!(Action::Insert, Action::Insert);
    }

    #[test]
    fn action_update_eq_self() {
        assert_eq!(Action::Update, Action::Update);
    }

    #[test]
    fn action_delete_eq_self() {
        assert_eq!(Action::Delete, Action::Delete);
    }

    #[test]
    fn action_create_eq_self() {
        assert_eq!(Action::Create, Action::Create);
    }

    #[test]
    fn action_drop_eq_self() {
        assert_eq!(Action::Drop, Action::Drop);
    }

    #[test]
    fn action_alter_eq_self() {
        assert_eq!(Action::Alter, Action::Alter);
    }

    #[test]
    fn action_execute_eq_self() {
        assert_eq!(Action::Execute, Action::Execute);
    }

    #[test]
    fn action_usage_eq_self() {
        assert_eq!(Action::Usage, Action::Usage);
    }

    // --- Action: clone preserves variant ---

    #[test]
    fn action_clone_preserves_variant() {
        let a = Action::Drop;
        let b = a;
        assert_eq!(a, b);
    }

    // --- Action: copy semantics ---

    #[test]
    fn action_copy_semantics() {
        let a = Action::Alter;
        let b = a; // Copy
        assert_eq!(a, b);
    }

    // --- Action: Debug format for each variant ---

    #[test]
    fn action_debug_format_connect() {
        assert_eq!(format!("{:?}", Action::Connect), "Connect");
    }

    #[test]
    fn action_debug_format_select() {
        assert_eq!(format!("{:?}", Action::Select), "Select");
    }

    #[test]
    fn action_debug_format_insert() {
        assert_eq!(format!("{:?}", Action::Insert), "Insert");
    }

    #[test]
    fn action_debug_format_update() {
        assert_eq!(format!("{:?}", Action::Update), "Update");
    }

    #[test]
    fn action_debug_format_delete() {
        assert_eq!(format!("{:?}", Action::Delete), "Delete");
    }

    #[test]
    fn action_debug_format_create() {
        assert_eq!(format!("{:?}", Action::Create), "Create");
    }

    #[test]
    fn action_debug_format_drop() {
        assert_eq!(format!("{:?}", Action::Drop), "Drop");
    }

    #[test]
    fn action_debug_format_alter() {
        assert_eq!(format!("{:?}", Action::Alter), "Alter");
    }

    #[test]
    fn action_debug_format_execute() {
        assert_eq!(format!("{:?}", Action::Execute), "Execute");
    }

    #[test]
    fn action_debug_format_usage() {
        assert_eq!(format!("{:?}", Action::Usage), "Usage");
    }

    // --- AccessTarget: with zero IDs ---

    #[test]
    fn access_target_database_zero_id() {
        let t = AccessTarget::Database(DatabaseId::new(0));
        let dbg = format!("{t:?}");
        assert!(dbg.contains("Database"));
    }

    #[test]
    fn access_target_schema_zero_id() {
        let t = AccessTarget::Schema(SchemaId::new(0));
        let dbg = format!("{t:?}");
        assert!(dbg.contains("Schema"));
    }

    #[test]
    fn access_target_relation_zero_id() {
        let t = AccessTarget::Relation(RelationId::new(0));
        let dbg = format!("{t:?}");
        assert!(dbg.contains("Relation"));
    }

    #[test]
    fn access_target_index_zero_id() {
        let t = AccessTarget::Index(IndexId::new(0));
        let dbg = format!("{t:?}");
        assert!(dbg.contains("Index"));
    }

    #[test]
    fn access_target_sequence_zero_id() {
        let t = AccessTarget::Sequence(SequenceId::new(0));
        let dbg = format!("{t:?}");
        assert!(dbg.contains("Sequence"));
    }

    // --- AccessTarget: with large IDs ---

    #[test]
    fn access_target_database_large_id() {
        let _t = AccessTarget::Database(DatabaseId::new(u64::MAX));
    }

    #[test]
    fn access_target_relation_large_id() {
        let _t = AccessTarget::Relation(RelationId::new(u64::MAX));
    }

    // --- AccessTarget: clone ---

    #[test]
    fn access_target_clone() {
        let t = AccessTarget::Database(DatabaseId::new(42));
        let t2 = t.clone();
        let dbg = format!("{t2:?}");
        assert!(dbg.contains("Database"));
    }

    // --- AccessRequest: every action with every target type ---

    #[test]
    fn access_request_insert_on_relation() {
        let req = AccessRequest {
            action: Action::Insert,
            target: Some(AccessTarget::Relation(RelationId::new(1))),
        };
        assert_eq!(req.action, Action::Insert);
    }

    #[test]
    fn access_request_create_on_schema() {
        let req = AccessRequest {
            action: Action::Create,
            target: Some(AccessTarget::Schema(SchemaId::new(1))),
        };
        assert_eq!(req.action, Action::Create);
    }

    #[test]
    fn access_request_drop_on_index() {
        let req = AccessRequest {
            action: Action::Drop,
            target: Some(AccessTarget::Index(IndexId::new(1))),
        };
        assert_eq!(req.action, Action::Drop);
    }

    #[test]
    fn access_request_usage_on_sequence() {
        let req = AccessRequest {
            action: Action::Usage,
            target: Some(AccessTarget::Sequence(SequenceId::new(1))),
        };
        assert_eq!(req.action, Action::Usage);
    }

    #[test]
    fn access_request_execute_with_no_target() {
        let req = AccessRequest {
            action: Action::Execute,
            target: None,
        };
        assert_eq!(req.action, Action::Execute);
        assert!(req.target.is_none());
    }

    #[test]
    fn access_request_alter_on_database() {
        let req = AccessRequest {
            action: Action::Alter,
            target: Some(AccessTarget::Database(DatabaseId::new(1))),
        };
        assert_eq!(req.action, Action::Alter);
    }

    // --- AccessRequest: clone ---

    #[test]
    fn access_request_clone() {
        let req = AccessRequest {
            action: Action::Delete,
            target: Some(AccessTarget::Relation(RelationId::new(99))),
        };
        let req2 = req.clone();
        assert_eq!(req2.action, Action::Delete);
    }

    // --- AccessRequest: debug output ---

    #[test]
    fn access_request_debug_output() {
        let req = AccessRequest {
            action: Action::Update,
            target: None,
        };
        let dbg = format!("{req:?}");
        assert!(dbg.contains("Update"));
    }
}
