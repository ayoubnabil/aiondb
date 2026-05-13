//! Facade over `aiondb_pg_compat::statement_policy`.
//!
//! This module no longer contains an implementation: all logic lives in
//! `aiondb-pg-compat` (see ADR-0004 on retiring the compat layer from the
//! engine side). Existing callers keep importing via
//! `super::statement_policy::*`; only the source moved.

pub(super) use aiondb_pg_compat::statement_policy::{
    is_acl_pseudo_role, normalize_acl_statement, reject_pg_database_catalog_update,
    statement_can_skip_catalog_txn_participant, statement_can_skip_storage_txn_participant,
    statement_requires_acl_normalization, statement_requires_implicit_transaction,
};
