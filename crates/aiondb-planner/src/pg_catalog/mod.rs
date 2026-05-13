#![allow(clippy::doc_markdown)]

use std::{collections::HashMap, sync::Arc};

use aiondb_catalog::{CatalogReader, ColumnDescriptor, QualifiedName, TableDescriptor};
use aiondb_core::{
    compat_function_oid, compat_role_oid, convert::usize_to_u32_saturating,
    convert::usize_to_u64_saturating, ColumnId, DataType, DbError, DbResult, RelationId, SchemaId,
    SqlState, TextTypeModifier, TxnId, Value, COMPAT_BOOTSTRAP_ROLE_NAME,
    COMPAT_INFORMATION_SCHEMA_NAMESPACE_OID, COMPAT_PG_CATALOG_NAMESPACE_OID,
    COMPAT_PUBLIC_NAMESPACE_OID,
};
use aiondb_eval::with_current_session_context;
use aiondb_parser::SelectStatement;
use aiondb_plan::{LogicalPlan, ResultField, TypedExpr};

pub mod core_tables;
mod extra_tables;
pub(crate) mod matview;
mod pg_catalog_extra2;
mod pg_operator;
mod pg_proc_data;
mod virtual_query;
mod virtual_query_helpers;

// Single source of truth for these OIDs lives in `aiondb_core::pg_compat`
// so the planner, evaluator and pgwire layer cannot drift apart.
pub(crate) use aiondb_core::{
    COMPAT_PGVECTOR_HALFVEC_ARRAY_OID, COMPAT_PGVECTOR_HALFVEC_OID, COMPAT_PGVECTOR_HNSW_AM_OID,
    COMPAT_PGVECTOR_IVFFLAT_AM_OID, COMPAT_PGVECTOR_SPARSEVEC_ARRAY_OID,
    COMPAT_PGVECTOR_SPARSEVEC_OID, COMPAT_PGVECTOR_VECTOR_ARRAY_OID, COMPAT_PGVECTOR_VECTOR_OID,
    COMPAT_PG_BIT_ARRAY_OID, COMPAT_PG_BIT_OID, COMPAT_PG_VARBIT_ARRAY_OID, COMPAT_PG_VARBIT_OID,
};

pub(crate) fn compat_pgvector_operator_oid(left_type_oid: i32, op_name: &str) -> i32 {
    compat_function_oid(&format!("operator:pgvector:{left_type_oid}:{op_name}"))
}

pub(crate) fn compat_pgvector_function_oid(name: &str, argtypes: &str) -> i32 {
    compat_function_oid(&format!("function:pgvector:{name}({argtypes})"))
}

pub(crate) fn compat_pgvector_opclass_oid(am_oid: i32, opcname: &str) -> i32 {
    compat_function_oid(&format!("opclass:pgvector:{am_oid}:{opcname}"))
}

// ---------------------------------------------------------------
// Well-known PostgreSQL OIDs for the virtual namespace rows
// ---------------------------------------------------------------

const PUBLIC_NAMESPACE_OID: i32 = COMPAT_PUBLIC_NAMESPACE_OID;
const PG_CATALOG_NAMESPACE_OID: i32 = COMPAT_PG_CATALOG_NAMESPACE_OID;
const INFORMATION_SCHEMA_NAMESPACE_OID: i32 = COMPAT_INFORMATION_SCHEMA_NAMESPACE_OID;

#[inline]
pub(super) fn u64_to_i32_saturating(value: u64) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

/// Recognized `pg_catalog` virtual table names (lowercased).
/// Only tables backed by real catalog data or correct static data are listed.
const PG_NAMESPACE: &str = "pg_namespace";
const PG_CLASS: &str = "pg_class";
const PG_ATTRIBUTE: &str = "pg_attribute";
const PG_TYPE: &str = "pg_type";
const PG_INDEX: &str = "pg_index";
const PG_CONSTRAINT: &str = "pg_constraint";
const PG_AM: &str = "pg_am";
const PG_INDEXES: &str = "pg_indexes";
const PG_VIEWS: &str = "pg_views";
const PG_TABLES: &str = "pg_tables";
const PG_USER: &str = "pg_user";
const PG_SHADOW: &str = "pg_shadow";
const PG_REPLICATION_SLOTS: &str = "pg_replication_slots";
const PG_STAT_REPLICATION: &str = "pg_stat_replication";
const PG_REPLICATION_ORIGIN: &str = "pg_replication_origin";
const PG_STAT_ALL_TABLES: &str = "pg_stat_all_tables";
const PG_STATIO_ALL_TABLES: &str = "pg_statio_all_tables";
const PG_STAT_USER_TABLES: &str = "pg_stat_user_tables";
const PG_STATIO_USER_TABLES: &str = "pg_statio_user_tables";
const PG_STAT_USER_FUNCTIONS: &str = "pg_stat_user_functions";
const PG_STATS: &str = "pg_stats";
const PG_RULES: &str = "pg_rules";
const PG_AUTH_MEMBERS: &str = "pg_auth_members";
const PG_PREPARED_XACTS: &str = "pg_prepared_xacts";
const PG_TS_CONFIG: &str = "pg_ts_config";
const PG_TS_DICT: &str = "pg_ts_dict";
const PG_TS_PARSER: &str = "pg_ts_parser";
const PG_TS_TEMPLATE: &str = "pg_ts_template";
const PG_AUTHID: &str = "pg_authid";
const PG_ROLES: &str = "pg_roles";
const PG_PROC: &str = "pg_proc";
const PG_DEPEND: &str = "pg_depend";
const PG_DESCRIPTION: &str = "pg_description";
const PG_SECLABEL: &str = "pg_seclabel";
const PG_COMPAT_OBJECT_ATTRS: &str = "pg_compat_object_attrs";
const PG_COMPAT_TRIGGER_STATE: &str = "pg_compat_trigger_state";
const PG_INIT_PRIVS: &str = "pg_init_privs";
const PG_AVAILABLE_EXTENSION_VERSIONS: &str = "pg_available_extension_versions";
const PG_AVAILABLE_EXTENSIONS: &str = "pg_available_extensions";
const PG_BACKEND_MEMORY_CONTEXTS: &str = "pg_backend_memory_contexts";
const PG_CONFIG: &str = "pg_config";
const PG_CURSORS: &str = "pg_cursors";
const PG_DATABASE: &str = "pg_database";
const PG_PARTITIONED_TABLE: &str = "pg_partitioned_table";
const PG_FILE_SETTINGS: &str = "pg_file_settings";
const PG_HBA_FILE_RULES: &str = "pg_hba_file_rules";
const PG_IDENT_FILE_MAPPINGS: &str = "pg_ident_file_mappings";
const PG_LOCKS: &str = "pg_locks";
const PG_PREPARED_STATEMENTS: &str = "pg_prepared_statements";
const PG_STAT_STATEMENTS: &str = "pg_stat_statements";
const PG_STAT_USER_INDEXES: &str = "pg_stat_user_indexes";
const PG_STATIO_USER_INDEXES: &str = "pg_statio_user_indexes";

#[inline]
fn is_hidden_compat_regtype_entry(name: &str) -> bool {
    let normalized = aiondb_eval::normalize_compat_type_name(name);
    normalized.is_empty() || normalized.starts_with("__aiondb_")
}

#[inline]
fn compat_regtype_array_map_key(name: &str) -> String {
    let normalized = aiondb_eval::normalize_compat_type_name(name);
    if normalized.is_empty() || normalized.ends_with("[]") || normalized.starts_with('_') {
        return normalized;
    }
    format!("{normalized}[]")
}

#[inline]
fn compat_regtype_array_legacy_map_key(name: &str) -> String {
    let normalized = aiondb_eval::normalize_compat_type_name(name);
    if normalized.is_empty() || normalized.starts_with('_') {
        return normalized;
    }
    format!("_{normalized}")
}

fn compat_domain_identity(schema_name: Option<&str>, name: &str) -> String {
    match schema_name {
        Some(schema_name) if !schema_name.is_empty() => {
            format!(
                "{}.{}",
                schema_name.to_ascii_lowercase(),
                name.to_ascii_lowercase()
            )
        }
        _ => name.to_ascii_lowercase(),
    }
}

fn compat_domain_oid(schema_name: Option<&str>, name: &str) -> i32 {
    aiondb_core::compat_function_oid(&format!(
        "domain:{}",
        compat_domain_identity(schema_name, name)
    ))
}

fn compat_type_oid_by_name(name: &str) -> i32 {
    let normalized = aiondb_eval::normalize_compat_type_name(name);
    match normalized.as_str() {
        "bit" => return COMPAT_PG_BIT_OID,
        "varbit" | "bit varying" => return COMPAT_PG_VARBIT_OID,
        "vector" => return COMPAT_PGVECTOR_VECTOR_OID,
        "halfvec" => return COMPAT_PGVECTOR_HALFVEC_OID,
        "sparsevec" => return COMPAT_PGVECTOR_SPARSEVEC_OID,
        "bit[]" => return COMPAT_PG_BIT_ARRAY_OID,
        "varbit[]" => return COMPAT_PG_VARBIT_ARRAY_OID,
        "vector[]" => return COMPAT_PGVECTOR_VECTOR_ARRAY_OID,
        "halfvec[]" => return COMPAT_PGVECTOR_HALFVEC_ARRAY_OID,
        "sparsevec[]" => return COMPAT_PGVECTOR_SPARSEVEC_ARRAY_OID,
        _ => {}
    }
    if let Some(entry) = PG_TYPE_ENTRIES
        .iter()
        .find(|entry| entry.name.eq_ignore_ascii_case(&normalized))
    {
        return entry.oid;
    }
    aiondb_eval::with_current_session_context(|ctx| {
        if let Some(user_type) = ctx
            .compat_user_types
            .iter()
            .find(|entry| entry.name.eq_ignore_ascii_case(&normalized))
        {
            return user_type.oid;
        }
        let bare_name = normalized
            .rsplit_once('.')
            .map_or(normalized.as_str(), |(_, tail)| tail);
        ctx.domain_defs
            .iter()
            .find(|entry| entry.name.eq_ignore_ascii_case(bare_name))
            .map(|entry| compat_domain_oid(entry.schema_name.as_deref(), &entry.name))
            .unwrap_or(0)
    })
}
const PG_SETTINGS: &str = "pg_settings";
const PG_STAT_ACTIVITY: &str = "pg_stat_activity";
const PG_STAT_DATABASE: &str = "pg_stat_database";
const PG_STAT_BGWRITER: &str = "pg_stat_bgwriter";
const PG_STAT_ARCHIVER: &str = "pg_stat_archiver";
const PG_STAT_IO: &str = "pg_stat_io";
const PG_STAT_SLRU: &str = "pg_stat_slru";
const PG_STAT_WAL: &str = "pg_stat_wal";
const PG_STAT_WAL_RECEIVER: &str = "pg_stat_wal_receiver";
const PG_TIMEZONE_ABBREVS: &str = "pg_timezone_abbrevs";
const PG_TIMEZONE_NAMES: &str = "pg_timezone_names";
const PG_OPERATOR: &str = "pg_operator";
const PG_CAST: &str = "pg_cast";
const PG_AGGREGATE: &str = "pg_aggregate";
const PG_AMOP: &str = "pg_amop";
const PG_AMPROC: &str = "pg_amproc";
const PG_OPCLASS: &str = "pg_opclass";
const PG_OPFAMILY: &str = "pg_opfamily";
const PG_CONVERSION: &str = "pg_conversion";
const PG_LANGUAGE: &str = "pg_language";
const PG_COLLATION: &str = "pg_collation";
const PG_TABLESPACE: &str = "pg_tablespace";
const PG_RANGE: &str = "pg_range";
const PG_ENUM: &str = "pg_enum";
const PG_TRIGGER: &str = "pg_trigger";
const PG_REWRITE: &str = "pg_rewrite";
const PG_INHERITS: &str = "pg_inherits";
const PG_SHDESCRIPTION: &str = "pg_shdescription";
const PG_EXTENSION: &str = "pg_extension";
const PG_PUBLICATION: &str = "pg_publication";
const PG_PUBLICATION_NAMESPACE: &str = "pg_publication_namespace";
const PG_PUBLICATION_REL: &str = "pg_publication_rel";
const PG_SUBSCRIPTION: &str = "pg_subscription";
const PG_EVENT_TRIGGER: &str = "pg_event_trigger";
const PG_FOREIGN_SERVER: &str = "pg_foreign_server";
const PG_FOREIGN_TABLE: &str = "pg_foreign_table";
const PG_FOREIGN_DATA_WRAPPER: &str = "pg_foreign_data_wrapper";
const PG_USER_MAPPINGS: &str = "pg_user_mappings";
const PG_USER_MAPPING: &str = "pg_user_mapping";
const PG_DB_ROLE_SETTING: &str = "pg_db_role_setting";
const PG_MATVIEWS: &str = "pg_matviews";
const PG_POLICY: &str = "pg_policy";
const PG_SEQUENCE: &str = "pg_sequence";
const PG_SEQUENCES: &str = "pg_sequences";
const PG_STATISTIC: &str = "pg_statistic";
const PG_STATISTIC_EXT: &str = "pg_statistic_ext";
const PG_STATISTIC_EXT_DATA: &str = "pg_statistic_ext_data";
const PG_STATS_EXT: &str = "pg_stats_ext";
const PG_STATS_EXT_EXPRS: &str = "pg_stats_ext_exprs";
const PG_ATTRDEF: &str = "pg_attrdef";
const PG_DEFAULT_ACL: &str = "pg_default_acl";
const PG_SHDEPEND: &str = "pg_shdepend";
const PG_SHMEM_ALLOCATIONS: &str = "pg_shmem_allocations";
const PG_LARGEOBJECT: &str = "pg_largeobject";
const PG_LARGEOBJECT_METADATA: &str = "pg_largeobject_metadata";

/// Returns `true` if the given schema name refers to `pg_catalog`.
pub(crate) fn is_pg_catalog(schema: &str) -> bool {
    schema.eq_ignore_ascii_case("pg_catalog")
}

/// Returns `true` when `table_name` (unqualified) matches a known `pg_catalog`
/// virtual table.  Used by the planner to short-circuit unqualified references
/// that match the `PostgreSQL` search path convention.
pub(crate) fn is_pg_catalog_table(table_name: &str) -> bool {
    synthetic_table_id(table_name).is_some()
}

/// Compatibility helper for polymorphic `pg_statistic` columns.
/// PostgreSQL exposes `stavaluesN` as `anyarray` pseudo-type entries whose
/// concrete element type varies by statistic kind.
pub(crate) fn compat_pseudotype_for_column_identifier(
    name_parts: &[String],
) -> Option<&'static str> {
    let column = name_parts.last()?.to_ascii_lowercase();
    if matches!(
        column.as_str(),
        "stavalues1" | "stavalues2" | "stavalues3" | "stavalues4" | "stavalues5"
    ) {
        return Some("anyarray");
    }
    None
}

/// Build a `LogicalPlan::ProjectValues` for the requested `pg_catalog` virtual
/// table.  Returns `None` if the table name is not recognized.
fn build_base_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    table_name: &str,
    default_schema: Option<&str>,
    session_user: Option<&str>,
    database_name: Option<&str>,
) -> DbResult<Option<LogicalPlan>> {
    let owner_oid = catalog_owner_oid(catalog, txn_id, session_user)?;
    let lower = table_name.to_ascii_lowercase();
    match lower.as_str() {
        PG_NAMESPACE => {
            build_pg_namespace_plan(catalog, txn_id, default_schema, owner_oid).map(Some)
        }
        PG_CLASS => {
            core_tables::build_pg_class_plan(catalog, txn_id, default_schema, owner_oid).map(Some)
        }
        PG_ATTRIBUTE => {
            core_tables::build_pg_attribute_plan(catalog, txn_id, default_schema).map(Some)
        }
        PG_TYPE => build_pg_type_plan(catalog, txn_id).map(Some),
        PG_INDEX => core_tables::build_pg_index_plan(catalog, txn_id, default_schema).map(Some),
        PG_CONSTRAINT => {
            core_tables::build_pg_constraint_plan(catalog, txn_id, default_schema).map(Some)
        }
        PG_AM => extra_tables::build_pg_am_plan().map(Some),
        PG_INDEXES => {
            extra_tables::build_pg_indexes_plan(catalog, txn_id, default_schema).map(Some)
        }
        PG_VIEWS => extra_tables::build_pg_views_plan(catalog, txn_id, default_schema).map(Some),
        PG_TABLES => extra_tables::build_pg_tables_plan(catalog, txn_id, default_schema).map(Some),
        PG_USER => extra_tables::build_pg_user_plan(catalog, txn_id).map(Some),
        PG_SHADOW => extra_tables::build_pg_shadow_plan(catalog, txn_id).map(Some),
        PG_REPLICATION_SLOTS => extra_tables::build_pg_replication_slots_plan().map(Some),
        PG_STAT_REPLICATION => extra_tables::build_pg_stat_replication_plan().map(Some),
        PG_REPLICATION_ORIGIN => extra_tables::build_pg_replication_origin_plan().map(Some),
        PG_STAT_ALL_TABLES => {
            extra_tables::build_pg_stat_all_tables_plan(catalog, txn_id, default_schema).map(Some)
        }
        PG_STATIO_ALL_TABLES => {
            extra_tables::build_pg_statio_all_tables_plan(catalog, txn_id, default_schema).map(Some)
        }
        PG_STAT_USER_TABLES => {
            extra_tables::build_pg_stat_user_tables_plan(catalog, txn_id, default_schema).map(Some)
        }
        PG_STATIO_USER_TABLES => {
            extra_tables::build_pg_statio_user_tables_plan(catalog, txn_id, default_schema)
                .map(Some)
        }
        PG_STAT_USER_FUNCTIONS => {
            extra_tables::build_pg_stat_user_functions_plan(catalog, txn_id).map(Some)
        }
        PG_STATS => extra_tables::build_pg_stats_plan().map(Some),
        PG_RULES => extra_tables::build_pg_rules_plan().map(Some),
        PG_AUTH_MEMBERS => extra_tables::build_pg_auth_members_plan(catalog, txn_id).map(Some),
        PG_PREPARED_XACTS => extra_tables::build_pg_prepared_xacts_plan().map(Some),
        PG_TS_CONFIG => extra_tables::build_pg_ts_config_plan(catalog, txn_id).map(Some),
        PG_TS_DICT => extra_tables::build_pg_ts_dict_plan(catalog, txn_id).map(Some),
        PG_TS_PARSER => extra_tables::build_pg_ts_parser_plan(catalog, txn_id).map(Some),
        PG_TS_TEMPLATE => extra_tables::build_pg_ts_template_plan(catalog, txn_id).map(Some),
        PG_AUTHID => extra_tables::build_pg_authid_plan(catalog, txn_id, session_user).map(Some),
        PG_ROLES => extra_tables::build_pg_authid_plan(catalog, txn_id, session_user).map(Some),
        PG_PROC => {
            pg_proc_data::build_pg_proc_plan_with_catalog(catalog, txn_id, owner_oid).map(Some)
        }
        PG_DEPEND => extra_tables::build_pg_depend_plan().map(Some),
        PG_DESCRIPTION => extra_tables::build_pg_description_plan().map(Some),
        PG_SECLABEL => extra_tables::build_pg_seclabel_plan().map(Some),
        PG_COMPAT_OBJECT_ATTRS => extra_tables::build_pg_compat_object_attrs_plan().map(Some),
        PG_COMPAT_TRIGGER_STATE => extra_tables::build_pg_compat_trigger_state_plan().map(Some),
        PG_INIT_PRIVS => extra_tables::build_pg_init_privs_plan().map(Some),
        PG_AVAILABLE_EXTENSION_VERSIONS => {
            extra_tables::build_pg_available_extension_versions_plan().map(Some)
        }
        PG_AVAILABLE_EXTENSIONS => extra_tables::build_pg_available_extensions_plan().map(Some),
        PG_BACKEND_MEMORY_CONTEXTS => {
            extra_tables::build_pg_backend_memory_contexts_plan().map(Some)
        }
        PG_CONFIG => extra_tables::build_pg_config_plan().map(Some),
        PG_CURSORS => extra_tables::build_pg_cursors_plan().map(Some),
        PG_DATABASE => {
            extra_tables::build_pg_database_plan(catalog, txn_id, database_name, owner_oid)
                .map(Some)
        }
        PG_PARTITIONED_TABLE => extra_tables::build_pg_partitioned_table_plan().map(Some),
        PG_FILE_SETTINGS => extra_tables::build_pg_file_settings_plan().map(Some),
        PG_HBA_FILE_RULES => extra_tables::build_pg_hba_file_rules_plan().map(Some),
        PG_IDENT_FILE_MAPPINGS => extra_tables::build_pg_ident_file_mappings_plan().map(Some),
        PG_LOCKS => extra_tables::build_pg_locks_plan().map(Some),
        PG_PREPARED_STATEMENTS => extra_tables::build_pg_prepared_statements_plan().map(Some),
        PG_STAT_STATEMENTS => extra_tables::build_pg_stat_statements_plan().map(Some),
        PG_STAT_USER_INDEXES => extra_tables::build_pg_stat_user_indexes_plan().map(Some),
        PG_STATIO_USER_INDEXES => extra_tables::build_pg_statio_user_indexes_plan().map(Some),
        PG_SETTINGS => extra_tables::build_pg_settings_plan().map(Some),
        PG_STAT_ACTIVITY => extra_tables::build_pg_stat_activity_plan(owner_oid).map(Some),
        PG_STAT_DATABASE => {
            extra_tables::build_pg_stat_database_plan(database_name, owner_oid).map(Some)
        }
        PG_STAT_BGWRITER => extra_tables::build_pg_stat_bgwriter_plan().map(Some),
        PG_STAT_ARCHIVER => extra_tables::build_pg_stat_archiver_plan().map(Some),
        PG_STAT_IO => extra_tables::build_pg_stat_io_plan().map(Some),
        PG_STAT_SLRU => extra_tables::build_pg_stat_slru_plan().map(Some),
        PG_STAT_WAL => extra_tables::build_pg_stat_wal_plan().map(Some),
        PG_STAT_WAL_RECEIVER => extra_tables::build_pg_stat_wal_receiver_plan().map(Some),
        PG_TIMEZONE_ABBREVS => extra_tables::build_pg_timezone_abbrevs_plan().map(Some),
        PG_TIMEZONE_NAMES => extra_tables::build_pg_timezone_names_plan().map(Some),
        PG_OPERATOR => pg_operator::build_pg_operator_plan(owner_oid).map(Some),
        PG_CAST => pg_catalog_extra2::build_pg_cast_plan().map(Some),
        PG_AGGREGATE => pg_catalog_extra2::build_pg_aggregate_plan().map(Some),
        PG_AMOP => pg_catalog_extra2::build_pg_amop_plan().map(Some),
        PG_AMPROC => pg_catalog_extra2::build_pg_amproc_plan().map(Some),
        PG_OPCLASS => pg_catalog_extra2::build_pg_opclass_plan().map(Some),
        PG_OPFAMILY => pg_catalog_extra2::build_pg_opfamily_plan().map(Some),
        PG_CONVERSION => pg_catalog_extra2::build_pg_conversion_plan(owner_oid).map(Some),
        PG_LANGUAGE => pg_catalog_extra2::build_pg_language_plan(owner_oid).map(Some),
        PG_COLLATION => pg_catalog_extra2::build_pg_collation_plan(owner_oid).map(Some),
        PG_TABLESPACE => pg_catalog_extra2::build_pg_tablespace_plan(owner_oid).map(Some),
        PG_RANGE => pg_catalog_extra2::build_pg_range_plan().map(Some),
        PG_ENUM => pg_catalog_extra2::build_pg_enum_plan_with_catalog(catalog, txn_id).map(Some),
        PG_TRIGGER => {
            pg_catalog_extra2::build_pg_trigger_plan(catalog, txn_id, default_schema).map(Some)
        }
        PG_REWRITE => pg_catalog_extra2::build_pg_rewrite_plan().map(Some),
        PG_INHERITS => pg_catalog_extra2::build_pg_inherits_plan().map(Some),
        PG_SHDESCRIPTION => pg_catalog_extra2::build_pg_shdescription_plan().map(Some),
        PG_EXTENSION => pg_catalog_extra2::build_pg_extension_plan().map(Some),
        PG_EVENT_TRIGGER => pg_catalog_extra2::build_pg_event_trigger_plan().map(Some),
        PG_FOREIGN_SERVER => pg_catalog_extra2::build_pg_foreign_server_plan().map(Some),
        PG_FOREIGN_TABLE => pg_catalog_extra2::build_pg_foreign_table_plan().map(Some),
        PG_FOREIGN_DATA_WRAPPER => {
            pg_catalog_extra2::build_pg_foreign_data_wrapper_plan().map(Some)
        }
        PG_DB_ROLE_SETTING => pg_catalog_extra2::build_pg_db_role_setting_plan().map(Some),
        PG_USER_MAPPINGS => pg_catalog_extra2::build_pg_user_mappings_plan().map(Some),
        PG_USER_MAPPING => pg_catalog_extra2::build_pg_user_mapping_plan().map(Some),
        PG_MATVIEWS => {
            pg_catalog_extra2::build_pg_matviews_plan(catalog, txn_id, default_schema).map(Some)
        }
        PG_POLICY => pg_catalog_extra2::build_pg_policy_plan().map(Some),
        PG_PUBLICATION => pg_catalog_extra2::build_pg_publication_plan().map(Some),
        PG_PUBLICATION_NAMESPACE => {
            pg_catalog_extra2::build_pg_publication_namespace_plan().map(Some)
        }
        PG_PUBLICATION_REL => pg_catalog_extra2::build_pg_publication_rel_plan().map(Some),
        PG_SUBSCRIPTION => pg_catalog_extra2::build_pg_subscription_plan().map(Some),
        PG_SEQUENCE => {
            pg_catalog_extra2::build_pg_sequence_plan(catalog, txn_id, default_schema).map(Some)
        }
        PG_SEQUENCES => {
            pg_catalog_extra2::build_pg_sequences_plan(catalog, txn_id, default_schema).map(Some)
        }
        PG_STATISTIC => pg_catalog_extra2::build_pg_statistic_plan().map(Some),
        PG_STATISTIC_EXT => pg_catalog_extra2::build_pg_statistic_ext_plan().map(Some),
        PG_STATISTIC_EXT_DATA => pg_catalog_extra2::build_pg_statistic_ext_data_plan().map(Some),
        PG_STATS_EXT => pg_catalog_extra2::build_pg_stats_ext_plan().map(Some),
        PG_STATS_EXT_EXPRS => pg_catalog_extra2::build_pg_stats_ext_exprs_plan().map(Some),
        PG_ATTRDEF => core_tables::build_pg_attrdef_plan(catalog, txn_id, default_schema).map(Some),
        PG_DEFAULT_ACL => Ok(Some(build_pg_default_acl_plan()?)),
        PG_SHDEPEND => Ok(Some(project_values(pg_shdepend_fields(), Vec::new()))),
        PG_SHMEM_ALLOCATIONS => Ok(Some(project_values(
            pg_shmem_allocations_fields(),
            Vec::new(),
        ))),
        PG_LARGEOBJECT => Ok(Some(project_values(pg_largeobject_fields(), Vec::new()))),
        PG_LARGEOBJECT_METADATA => Ok(Some(project_values(
            pg_largeobject_metadata_fields(),
            Vec::new(),
        ))),
        _ => Ok(None),
    }
}

/// Build a `LogicalPlan::ProjectValues` for the named `pg_catalog` table.
/// Returns `None` when the table name is not recognized.
///
/// Exposed publicly so that the engine can materialize virtual table rows
/// when the normal binder/optimizer path produces a `SeqScan` on a
/// synthetic pg_catalog `RelationId`.
pub fn build_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    table_name: &str,
    default_schema: Option<&str>,
    session_user: Option<&str>,
    database_name: Option<&str>,
) -> DbResult<Option<LogicalPlan>> {
    build_base_plan(
        catalog,
        txn_id,
        table_name,
        default_schema,
        session_user,
        database_name,
    )
}

pub(crate) fn build_select_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    select: &SelectStatement,
    default_schema: Option<&str>,
    session_user: Option<&str>,
    database_name: Option<&str>,
) -> DbResult<Option<LogicalPlan>> {
    virtual_query::build_select_plan(
        catalog,
        txn_id,
        select,
        default_schema,
        session_user,
        database_name,
    )
}

/// Return the `ResultField` descriptors for a given `pg_catalog` table.
/// Used by `describe` to return column metadata without executing.
pub(crate) fn output_fields_for(table_name: &str) -> Option<Vec<ResultField>> {
    let lower = table_name.to_ascii_lowercase();
    match lower.as_str() {
        PG_NAMESPACE => Some(pg_namespace_fields()),
        PG_CLASS => Some(core_tables::pg_class_fields()),
        PG_ATTRIBUTE => Some(core_tables::pg_attribute_fields()),
        PG_TYPE => Some(pg_type_fields()),
        PG_INDEX => Some(core_tables::pg_index_fields()),
        PG_CONSTRAINT => Some(core_tables::pg_constraint_fields()),
        PG_AM => Some(extra_tables::pg_am_fields()),
        PG_INDEXES => Some(extra_tables::pg_indexes_fields()),
        PG_VIEWS => Some(extra_tables::pg_views_fields()),
        PG_TABLES => Some(extra_tables::pg_tables_fields()),
        PG_USER => Some(extra_tables::pg_user_fields()),
        PG_SHADOW => Some(extra_tables::pg_shadow_fields()),
        PG_REPLICATION_SLOTS => Some(extra_tables::pg_replication_slots_fields()),
        PG_STAT_REPLICATION => Some(extra_tables::pg_stat_replication_fields()),
        PG_REPLICATION_ORIGIN => Some(extra_tables::pg_replication_origin_fields()),
        PG_STAT_ALL_TABLES => Some(extra_tables::pg_stat_all_tables_fields()),
        PG_STATIO_ALL_TABLES => Some(extra_tables::pg_statio_all_tables_fields()),
        PG_STAT_USER_TABLES => Some(extra_tables::pg_stat_user_tables_fields()),
        PG_STATIO_USER_TABLES => Some(extra_tables::pg_statio_user_tables_fields()),
        PG_STAT_USER_FUNCTIONS => Some(extra_tables::pg_stat_user_functions_fields()),
        PG_STATS => Some(extra_tables::pg_stats_fields()),
        PG_RULES => Some(extra_tables::pg_rules_fields()),
        PG_AUTH_MEMBERS => Some(extra_tables::pg_auth_members_fields()),
        PG_PREPARED_XACTS => Some(extra_tables::pg_prepared_xacts_fields()),
        PG_TS_CONFIG => Some(extra_tables::pg_ts_config_fields()),
        PG_TS_DICT => Some(extra_tables::pg_ts_dict_fields()),
        PG_TS_PARSER => Some(extra_tables::pg_ts_parser_fields()),
        PG_TS_TEMPLATE => Some(extra_tables::pg_ts_template_fields()),
        PG_AUTHID => Some(extra_tables::pg_authid_fields()),
        PG_ROLES => Some(extra_tables::pg_authid_fields()),
        PG_PROC => Some(pg_proc_data::pg_proc_fields()),
        PG_DEPEND => Some(extra_tables::pg_depend_fields()),
        PG_DESCRIPTION => Some(extra_tables::pg_description_fields()),
        PG_SECLABEL => Some(extra_tables::pg_seclabel_fields()),
        PG_COMPAT_OBJECT_ATTRS => Some(extra_tables::pg_compat_object_attrs_fields()),
        PG_COMPAT_TRIGGER_STATE => Some(extra_tables::pg_compat_trigger_state_fields()),
        PG_INIT_PRIVS => Some(extra_tables::pg_init_privs_fields()),
        PG_AVAILABLE_EXTENSION_VERSIONS => {
            Some(extra_tables::pg_available_extension_versions_fields())
        }
        PG_AVAILABLE_EXTENSIONS => Some(extra_tables::pg_available_extensions_fields()),
        PG_BACKEND_MEMORY_CONTEXTS => Some(extra_tables::pg_backend_memory_contexts_fields()),
        PG_CONFIG => Some(extra_tables::pg_config_fields()),
        PG_CURSORS => Some(extra_tables::pg_cursors_fields()),
        PG_DATABASE => Some(extra_tables::pg_database_fields()),
        PG_PARTITIONED_TABLE => Some(extra_tables::pg_partitioned_table_fields()),
        PG_FILE_SETTINGS => Some(extra_tables::pg_file_settings_fields()),
        PG_HBA_FILE_RULES => Some(extra_tables::pg_hba_file_rules_fields()),
        PG_IDENT_FILE_MAPPINGS => Some(extra_tables::pg_ident_file_mappings_fields()),
        PG_LOCKS => Some(extra_tables::pg_locks_fields()),
        PG_PREPARED_STATEMENTS => Some(extra_tables::pg_prepared_statements_fields()),
        PG_STAT_STATEMENTS => Some(extra_tables::pg_stat_statements_fields()),
        PG_STAT_USER_INDEXES => Some(extra_tables::pg_stat_user_indexes_fields()),
        PG_STATIO_USER_INDEXES => Some(extra_tables::pg_statio_user_indexes_fields()),
        PG_SETTINGS => Some(extra_tables::pg_settings_fields()),
        PG_STAT_ACTIVITY => Some(extra_tables::pg_stat_activity_fields()),
        PG_STAT_DATABASE => Some(extra_tables::pg_stat_database_fields()),
        PG_STAT_BGWRITER => Some(extra_tables::pg_stat_bgwriter_fields()),
        PG_STAT_ARCHIVER => Some(extra_tables::pg_stat_archiver_fields()),
        PG_STAT_IO => Some(extra_tables::pg_stat_io_fields()),
        PG_STAT_SLRU => Some(extra_tables::pg_stat_slru_fields()),
        PG_STAT_WAL => Some(extra_tables::pg_stat_wal_fields()),
        PG_STAT_WAL_RECEIVER => Some(extra_tables::pg_stat_wal_receiver_fields()),
        PG_TIMEZONE_ABBREVS => Some(extra_tables::pg_timezone_abbrevs_fields()),
        PG_TIMEZONE_NAMES => Some(extra_tables::pg_timezone_names_fields()),
        PG_OPERATOR => Some(pg_operator::pg_operator_fields()),
        PG_CAST => Some(pg_catalog_extra2::pg_cast_fields()),
        PG_AGGREGATE => Some(pg_catalog_extra2::pg_aggregate_fields()),
        PG_AMOP => Some(pg_catalog_extra2::pg_amop_fields()),
        PG_AMPROC => Some(pg_catalog_extra2::pg_amproc_fields()),
        PG_OPCLASS => Some(pg_catalog_extra2::pg_opclass_fields()),
        PG_OPFAMILY => Some(pg_catalog_extra2::pg_opfamily_fields()),
        PG_CONVERSION => Some(pg_catalog_extra2::pg_conversion_fields()),
        PG_LANGUAGE => Some(pg_catalog_extra2::pg_language_fields()),
        PG_COLLATION => Some(pg_catalog_extra2::pg_collation_fields()),
        PG_TABLESPACE => Some(pg_catalog_extra2::pg_tablespace_fields()),
        PG_RANGE => Some(pg_catalog_extra2::pg_range_fields()),
        PG_ENUM => Some(pg_catalog_extra2::pg_enum_fields()),
        PG_TRIGGER => Some(pg_catalog_extra2::pg_trigger_fields()),
        PG_REWRITE => Some(pg_catalog_extra2::pg_rewrite_fields()),
        PG_INHERITS => Some(pg_catalog_extra2::pg_inherits_fields()),
        PG_SHDESCRIPTION => Some(pg_catalog_extra2::pg_shdescription_fields()),
        PG_EXTENSION => Some(pg_catalog_extra2::pg_extension_fields()),
        PG_EVENT_TRIGGER => Some(pg_catalog_extra2::pg_event_trigger_fields()),
        PG_FOREIGN_SERVER => Some(pg_catalog_extra2::pg_foreign_server_fields()),
        PG_FOREIGN_TABLE => Some(pg_catalog_extra2::pg_foreign_table_fields()),
        PG_FOREIGN_DATA_WRAPPER => Some(pg_catalog_extra2::pg_foreign_data_wrapper_fields()),
        PG_DB_ROLE_SETTING => Some(pg_catalog_extra2::pg_db_role_setting_fields()),
        PG_USER_MAPPINGS => Some(pg_catalog_extra2::pg_user_mappings_fields()),
        PG_USER_MAPPING => Some(pg_catalog_extra2::pg_user_mapping_fields()),
        PG_MATVIEWS => Some(pg_catalog_extra2::pg_matviews_fields()),
        PG_POLICY => Some(pg_catalog_extra2::pg_policy_fields()),
        PG_PUBLICATION => Some(pg_catalog_extra2::pg_publication_fields()),
        PG_PUBLICATION_NAMESPACE => Some(pg_catalog_extra2::pg_publication_namespace_fields()),
        PG_PUBLICATION_REL => Some(pg_catalog_extra2::pg_publication_rel_fields()),
        PG_SUBSCRIPTION => Some(pg_catalog_extra2::pg_subscription_fields()),
        PG_SEQUENCE => Some(pg_catalog_extra2::pg_sequence_fields()),
        PG_SEQUENCES => Some(pg_catalog_extra2::pg_sequences_fields()),
        PG_STATISTIC => Some(pg_catalog_extra2::pg_statistic_fields()),
        PG_STATISTIC_EXT => Some(pg_catalog_extra2::pg_statistic_ext_fields()),
        PG_STATISTIC_EXT_DATA => Some(pg_catalog_extra2::pg_statistic_ext_data_fields()),
        PG_STATS_EXT => Some(pg_catalog_extra2::pg_stats_ext_fields()),
        PG_STATS_EXT_EXPRS => Some(pg_catalog_extra2::pg_stats_ext_exprs_fields()),
        PG_ATTRDEF => Some(core_tables::pg_attrdef_fields()),
        PG_DEFAULT_ACL => Some(pg_default_acl_fields()),
        PG_SHDEPEND => Some(pg_shdepend_fields()),
        PG_SHMEM_ALLOCATIONS => Some(pg_shmem_allocations_fields()),
        PG_LARGEOBJECT => Some(pg_largeobject_fields()),
        PG_LARGEOBJECT_METADATA => Some(pg_largeobject_metadata_fields()),
        _ => None,
    }
}

// ---------------------------------------------------------------
// pg_catalog.pg_namespace
// ---------------------------------------------------------------

fn pg_namespace_fields() -> Vec<ResultField> {
    vec![
        oid_field("oid"),
        name_field("nspname"),
        oid_field("nspowner"),
        ResultField {
            name: "nspacl".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Text)),
            text_type_modifier: None,
            nullable: true,
        },
    ]
}

fn build_pg_namespace_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
    owner_oid: i32,
) -> DbResult<LogicalPlan> {
    let tenant_filter = tenant_schema_filter(default_schema);
    let output_fields = pg_namespace_fields();

    // Walk every role's privileges and group GRANTs on schemas by
    // schema name. `pg_namespace` exposes `nspacl` so schema GRANTs are
    // visible to `\dn+` and ORM introspection.
    let nspacl_by_schema: std::collections::HashMap<String, Vec<String>> = {
        let mut per_role: std::collections::HashMap<(String, String), String> =
            std::collections::HashMap::new();
        for role in catalog.list_roles(txn_id)? {
            for privilege in catalog.get_privileges(txn_id, &role.name)? {
                let aiondb_catalog::PrivilegeTarget::Schema(schema) = privilege.target else {
                    continue;
                };
                let chars = match privilege.privilege {
                    aiondb_catalog::CatalogPrivilege::Usage => "U",
                    aiondb_catalog::CatalogPrivilege::Create => "C",
                    aiondb_catalog::CatalogPrivilege::All => "UC",
                    _ => continue,
                };
                per_role
                    .entry((schema, role.name.clone()))
                    .or_default()
                    .push_str(chars);
            }
        }
        let mut out: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for ((schema, role), chars) in per_role {
            let mut seen: Vec<char> = Vec::new();
            for ch in chars.chars() {
                if !seen.contains(&ch) {
                    seen.push(ch);
                }
            }
            let compressed: String = seen.into_iter().collect();
            out.entry(schema)
                .or_default()
                .push(format!("{role}={compressed}/"));
        }
        out
    };

    let mut rows = Vec::new();

    // Walk every catalog schema. Public is pinned to its compat OID; user
    // schemas derive their OID deterministically from the schema_id. The
    // tenant filter restricts visibility to the active tenant when set.
    for schema in catalog.list_schemas(txn_id)? {
        if !schema_visible_with_tenant_filter(&schema.name, tenant_filter.as_deref()) {
            continue;
        }
        let ns_oid = if schema.name.eq_ignore_ascii_case("public") {
            PUBLIC_NAMESPACE_OID
        } else {
            u64_to_i32_saturating(schema.schema_id.get()).saturating_add(16384)
        };
        let acl = nspacl_by_schema.get(&schema.name).cloned();
        let visible_name = visible_schema_name(&schema.name, default_schema);
        rows.push(namespace_row(ns_oid, &visible_name, owner_oid, acl));
    }

    rows.push(namespace_row(
        PG_CATALOG_NAMESPACE_OID,
        "pg_catalog",
        owner_oid,
        None,
    ));
    rows.push(namespace_row(
        INFORMATION_SCHEMA_NAMESPACE_OID,
        "information_schema",
        owner_oid,
        None,
    ));
    Ok(project_values(output_fields, rows))
}

fn namespace_row(
    oid: i32,
    name: &str,
    owner_oid: i32,
    nspacl: Option<Vec<String>>,
) -> Vec<TypedExpr> {
    let nspacl_literal = match nspacl {
        Some(entries) if !entries.is_empty() => {
            let values: Vec<aiondb_core::Value> =
                entries.into_iter().map(aiondb_core::Value::Text).collect();
            extra_tables::typed_array_literal(values, DataType::Text)
        }
        _ => null_literal(DataType::Array(Box::new(DataType::Text))),
    };
    vec![
        int_literal(oid),
        text_literal(name),
        int_literal(owner_oid),
        nspacl_literal,
    ]
}

fn pg_default_acl_fields() -> Vec<ResultField> {
    vec![
        oid_field("oid"),
        oid_field("defaclrole"),
        oid_field("defaclnamespace"),
        internal_char_field("defaclobjtype"),
        ResultField {
            name: "defaclacl".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Text)),
            text_type_modifier: None,
            nullable: true,
        },
    ]
}

/// Build `pg_default_acl` from the session compat registry. Each row
/// corresponds to a previous `ALTER DEFAULT PRIVILEGES` statement whose key
/// is stored as `("ALTER DEFAULT PRIVILEGES", "role/schema/object_type")`.
fn build_pg_default_acl_plan() -> DbResult<LogicalPlan> {
    let rows = with_current_session_context(|context| {
        let mut rows: Vec<Vec<TypedExpr>> = Vec::new();
        let mut oid_counter: i32 = 70_000;
        for ((kind, _), (_, _, _, options_joined, _, _)) in context.compat_misc_attrs.iter() {
            if kind != "ALTER DEFAULT PRIVILEGES" {
                continue;
            }
            let target_role = options_joined
                .split(',')
                .map(str::trim)
                .find_map(|pair| pair.strip_prefix("target_role=").map(str::to_owned));
            let schema = options_joined
                .split(',')
                .map(str::trim)
                .find_map(|pair| pair.strip_prefix("schema=").map(str::to_owned));
            let action = options_joined
                .split(',')
                .map(str::trim)
                .find_map(|pair| pair.strip_prefix("action=").map(str::to_owned))
                .unwrap_or_else(|| "grant".to_owned());
            let privileges = options_joined
                .split(',')
                .map(str::trim)
                .find_map(|pair| pair.strip_prefix("privileges=").map(str::to_owned))
                .unwrap_or_default();
            let object_type = options_joined
                .split(',')
                .map(str::trim)
                .find_map(|pair| pair.strip_prefix("object_type=").map(str::to_owned))
                .unwrap_or_else(|| "TABLES".to_owned());
            let grantees = options_joined
                .split(',')
                .map(str::trim)
                .find_map(|pair| pair.strip_prefix("grantees=").map(str::to_owned))
                .unwrap_or_default();
            let with_grant_option = options_joined
                .split(',')
                .map(str::trim)
                .any(|pair| pair == "with_grant_option=true");

            let defaclobjtype = match object_type.to_ascii_uppercase().as_str() {
                kind if kind.starts_with("TABLES") || kind.starts_with("TABLE") => "r",
                kind if kind.starts_with("SEQUENCES") || kind.starts_with("SEQUENCE") => "S",
                kind if kind.starts_with("FUNCTIONS") || kind.starts_with("ROUTINES") => "f",
                kind if kind.starts_with("TYPES") => "T",
                _ => "r",
            };
            let defaclrole = target_role
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(aiondb_core::compat_role_oid)
                .unwrap_or(0);
            let defaclnamespace = 0i32; // schema → namespace oid would require catalog lookup; leave as 0
            let _ = schema;

            // Build an ACL text[]: each entry formatted as
            // `grantee=privs/grantor` like PG's pg_default_acl.defaclacl.
            let grantor = target_role.clone().unwrap_or_default();
            let privs_compact = privileges
                .split(',')
                .map(str::trim)
                .map(|p| privilege_to_acl_char(p, action == "revoke"))
                .collect::<String>();
            let acl_elements: Vec<aiondb_core::Value> = grantees
                .split(',')
                .map(str::trim)
                .filter(|g| !g.is_empty())
                .map(|grantee| {
                    let grant_flag = if with_grant_option { "*" } else { "" };
                    let grantee_text = if grantee.eq_ignore_ascii_case("public") {
                        ""
                    } else {
                        grantee
                    };
                    aiondb_core::Value::Text(format!(
                        "{grantee_text}={privs_compact}{grant_flag}/{grantor}"
                    ))
                })
                .collect();

            rows.push(vec![
                int_literal(oid_counter),
                int_literal(defaclrole),
                int_literal(defaclnamespace),
                text_literal(defaclobjtype),
                if acl_elements.is_empty() {
                    null_literal(DataType::Array(Box::new(DataType::Text)))
                } else {
                    extra_tables::typed_array_literal(acl_elements, DataType::Text)
                },
            ]);
            oid_counter += 1;
        }
        rows
    });
    Ok(project_values(pg_default_acl_fields(), rows))
}

/// Compress a privilege name to its canonical ACL letter:
/// SELECT=r, INSERT=a, UPDATE=w, DELETE=d, TRUNCATE=D, REFERENCES=x,
/// TRIGGER=t, EXECUTE=X, USAGE=U, CONNECT=c, CREATE=C, TEMPORARY=T.
fn privilege_to_acl_char(name: &str, _revoke: bool) -> &'static str {
    match name
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_uppercase()
        .as_str()
    {
        "SELECT" => "r",
        "INSERT" => "a",
        "UPDATE" => "w",
        "DELETE" => "d",
        "TRUNCATE" => "D",
        "REFERENCES" => "x",
        "TRIGGER" => "t",
        "EXECUTE" => "X",
        "USAGE" => "U",
        "CONNECT" => "c",
        "CREATE" => "C",
        "TEMPORARY" | "TEMP" => "T",
        "ALL" => "arwdDxtXUCcT",
        _ => "",
    }
}

fn pg_shdepend_fields() -> Vec<ResultField> {
    vec![
        oid_field("dbid"),
        oid_field("classid"),
        oid_field("objid"),
        int_field("objsubid"),
        oid_field("refclassid"),
        oid_field("refobjid"),
        int_field("refobjsubid"),
        internal_char_field("deptype"),
    ]
}

fn pg_shmem_allocations_fields() -> Vec<ResultField> {
    vec![
        text_field("name"),
        bigint_field("off"),
        bigint_field("size"),
        bigint_field("allocated_size"),
    ]
}

fn pg_largeobject_fields() -> Vec<ResultField> {
    vec![
        oid_field("loid"),
        int_field("pageno"),
        ResultField {
            name: "data".to_owned(),
            data_type: DataType::Blob,
            text_type_modifier: None,
            nullable: false,
        },
    ]
}

fn pg_largeobject_metadata_fields() -> Vec<ResultField> {
    vec![
        oid_field("oid"),
        oid_field("lomowner"),
        ResultField {
            name: "lomacl".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Text)),
            text_type_modifier: None,
            nullable: true,
        },
    ]
}

fn catalog_owner_oid(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    session_user: Option<&str>,
) -> DbResult<i32> {
    let roles = catalog.list_roles(txn_id)?;
    if let Some(user) = session_user {
        return Ok(compat_role_oid(user));
    }
    if let Some(role) = roles.iter().find(|role| role.superuser) {
        return Ok(compat_role_oid(&role.name));
    }
    if let Some(role) = roles.first() {
        return Ok(compat_role_oid(&role.name));
    }
    Ok(compat_role_oid(
        session_user.unwrap_or(COMPAT_BOOTSTRAP_ROLE_NAME),
    ))
}

// ---------------------------------------------------------------
// pg_catalog.pg_type
// ---------------------------------------------------------------

fn pg_type_fields() -> Vec<ResultField> {
    vec![
        oid_field("oid"),
        name_field("typname"),
        oid_field("typarray"),
        oid_field("typnamespace"),
        int_field("typlen"),
        internal_char_field("typdelim"),
        internal_char_field("typtype"),
        oid_field("typbasetype"),
        oid_field("typcollation"),
        oid_field("typrelid"),
        oid_field("typelem"),
        bool_field("typnotnull"),
        int_field("typtypmod"),
        int_field("typndims"),
        internal_char_field("typcategory"),
        bool_field("typispreferred"),
        bool_field("typisdefined"),
        ResultField {
            name: "typinput".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: Some(TextTypeModifier::RegProc),
            nullable: false,
        },
        ResultField {
            name: "typoutput".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: Some(TextTypeModifier::RegProc),
            nullable: false,
        },
        ResultField {
            name: "typreceive".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: Some(TextTypeModifier::RegProc),
            nullable: false,
        },
        ResultField {
            name: "typsend".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: Some(TextTypeModifier::RegProc),
            nullable: false,
        },
        ResultField {
            name: "typmodin".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: Some(TextTypeModifier::RegProc),
            nullable: false,
        },
        ResultField {
            name: "typmodout".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: Some(TextTypeModifier::RegProc),
            nullable: false,
        },
        ResultField {
            name: "typanalyze".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: Some(TextTypeModifier::RegProc),
            nullable: false,
        },
        ResultField {
            name: "typsubscript".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: Some(TextTypeModifier::RegProc),
            nullable: false,
        },
        oid_field("typowner"),
        internal_char_field("typalign"),
        internal_char_field("typstorage"),
        nullable_text_field("typdefault"),
        ResultField {
            name: "typacl".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Text)),
            text_type_modifier: None,
            nullable: true,
        },
    ]
}

/// Static type row entries following `PostgreSQL` conventions.
struct PgTypeEntry {
    oid: i32,
    name: &'static str,
    array_oid: i32,
    len: i32,
    delimiter: &'static str,
    typtype: &'static str,
    typcollation: i32,
    typelem: i32,
    typcategory: &'static str,
    typispreferred: bool,
    typinput: &'static str,
    typoutput: &'static str,
    typalign: &'static str,
    typstorage: &'static str,
}

fn pg_type_input_proc_oid(entry: &PgTypeEntry) -> i32 {
    match entry.oid {
        COMPAT_PGVECTOR_VECTOR_OID => compat_pgvector_function_oid("vector_in", "2275"),
        COMPAT_PGVECTOR_HALFVEC_OID => compat_pgvector_function_oid("halfvec_in", "2275"),
        COMPAT_PGVECTOR_SPARSEVEC_OID => compat_pgvector_function_oid("sparsevec_in", "2275"),
        _ => compat_function_oid(entry.typinput),
    }
}

fn pg_type_output_proc_oid(entry: &PgTypeEntry) -> i32 {
    match entry.oid {
        COMPAT_PGVECTOR_VECTOR_OID => {
            compat_pgvector_function_oid("vector_out", &COMPAT_PGVECTOR_VECTOR_OID.to_string())
        }
        COMPAT_PGVECTOR_HALFVEC_OID => {
            compat_pgvector_function_oid("halfvec_out", &COMPAT_PGVECTOR_HALFVEC_OID.to_string())
        }
        COMPAT_PGVECTOR_SPARSEVEC_OID => compat_pgvector_function_oid(
            "sparsevec_out",
            &COMPAT_PGVECTOR_SPARSEVEC_OID.to_string(),
        ),
        _ => compat_function_oid(entry.typoutput),
    }
}

/// Helper macro to define a base type with standard defaults.
macro_rules! pg_type {
    ($oid:expr, $name:expr, $arr:expr, $len:expr, $cat:expr, $pref:expr, $in:expr, $out:expr, $align:expr, $stor:expr) => {
        PgTypeEntry {
            oid: $oid,
            name: $name,
            array_oid: $arr,
            len: $len,
            delimiter: ",",
            typtype: "b",
            typcollation: 0,
            typelem: 0,
            typcategory: $cat,
            typispreferred: $pref,
            typinput: $in,
            typoutput: $out,
            typalign: $align,
            typstorage: $stor,
        }
    };
}

const PG_TYPE_ENTRIES: &[PgTypeEntry] = &[
    pg_type!(16, "bool", 1000, 1, "B", true, "boolin", "boolout", "c", "p"),
    pg_type!(17, "bytea", 1001, -1, "U", false, "byteain", "byteaout", "i", "x"),
    pg_type!(20, "int8", 1016, 8, "N", false, "int8in", "int8out", "d", "p"),
    pg_type!(21, "int2", 1005, 2, "N", false, "int2in", "int2out", "s", "p"),
    pg_type!(23, "int4", 1007, 4, "N", false, "int4in", "int4out", "i", "p"),
    pg_type!(25, "text", 1009, -1, "S", true, "textin", "textout", "i", "x"),
    pg_type!(
        COMPAT_PG_BIT_OID,
        "bit",
        COMPAT_PG_BIT_ARRAY_OID,
        -1,
        "V",
        false,
        "bit_in",
        "bit_out",
        "i",
        "x"
    ),
    pg_type!(
        COMPAT_PG_VARBIT_OID,
        "varbit",
        COMPAT_PG_VARBIT_ARRAY_OID,
        -1,
        "V",
        true,
        "varbit_in",
        "varbit_out",
        "i",
        "x"
    ),
    pg_type!(
        COMPAT_PGVECTOR_VECTOR_OID,
        "vector",
        COMPAT_PGVECTOR_VECTOR_ARRAY_OID,
        -1,
        "U",
        false,
        "vector_in",
        "vector_out",
        "i",
        "x"
    ),
    pg_type!(
        COMPAT_PGVECTOR_HALFVEC_OID,
        "halfvec",
        COMPAT_PGVECTOR_HALFVEC_ARRAY_OID,
        -1,
        "U",
        false,
        "halfvec_in",
        "halfvec_out",
        "i",
        "x"
    ),
    pg_type!(
        COMPAT_PGVECTOR_SPARSEVEC_OID,
        "sparsevec",
        COMPAT_PGVECTOR_SPARSEVEC_ARRAY_OID,
        -1,
        "U",
        false,
        "sparsevec_in",
        "sparsevec_out",
        "i",
        "x"
    ),
    PgTypeEntry {
        oid: 26,
        name: "oid",
        array_oid: 1028,
        len: 4,
        delimiter: ",",
        typtype: "b",
        typcollation: 0,
        typelem: 0,
        typcategory: "N",
        typispreferred: false,
        typinput: "oidin",
        typoutput: "oidout",
        typalign: "i",
        typstorage: "p",
    },
    pg_type!(
        700,
        "float4",
        1021,
        4,
        "N",
        false,
        "float4in",
        "float4out",
        "i",
        "p"
    ),
    pg_type!(
        701,
        "float8",
        1022,
        8,
        "N",
        true,
        "float8in",
        "float8out",
        "d",
        "p"
    ),
    pg_type!(
        1042,
        "bpchar",
        1014,
        -1,
        "S",
        false,
        "bpcharin",
        "bpcharout",
        "i",
        "x"
    ),
    pg_type!(
        1043,
        "varchar",
        1015,
        -1,
        "S",
        false,
        "varcharin",
        "varcharout",
        "i",
        "x"
    ),
    pg_type!(1082, "date", 1182, 4, "D", false, "date_in", "date_out", "i", "p"),
    pg_type!(1083, "time", 1183, 8, "D", false, "time_in", "time_out", "d", "p"),
    pg_type!(
        1114,
        "timestamp",
        1115,
        8,
        "D",
        false,
        "timestamp_in",
        "timestamp_out",
        "d",
        "p"
    ),
    pg_type!(
        1184,
        "timestamptz",
        1185,
        8,
        "D",
        true,
        "timestamptz_in",
        "timestamptz_out",
        "d",
        "p"
    ),
    pg_type!(
        1186,
        "interval",
        1187,
        16,
        "T",
        true,
        "interval_in",
        "interval_out",
        "d",
        "p"
    ),
    PgTypeEntry {
        oid: 1700,
        name: "numeric",
        array_oid: 1231,
        len: -1,
        delimiter: ",",
        typtype: "b",
        typcollation: 0,
        typelem: 0,
        typcategory: "N",
        typispreferred: false,
        typinput: "numeric_in",
        typoutput: "numeric_out",
        typalign: "i",
        typstorage: "m",
    },
    pg_type!(2950, "uuid", 2951, 16, "U", false, "uuid_in", "uuid_out", "c", "p"),
    pg_type!(
        3802,
        "jsonb",
        3807,
        -1,
        "U",
        false,
        "jsonb_in",
        "jsonb_out",
        "i",
        "x"
    ),
    // Text-collatable types get typcollation = 100 (default collation OID)
    PgTypeEntry {
        oid: 19,
        name: "name",
        array_oid: 1003,
        len: 64,
        delimiter: ",",
        typtype: "b",
        typcollation: 950,
        typelem: 18,
        typcategory: "S",
        typispreferred: false,
        typinput: "namein",
        typoutput: "nameout",
        typalign: "c",
        typstorage: "p",
    },
    PgTypeEntry {
        oid: 2205,
        name: "regclass",
        array_oid: 2210,
        len: 4,
        delimiter: ",",
        typtype: "b",
        typcollation: 0,
        typelem: 0,
        typcategory: "N",
        typispreferred: false,
        typinput: "regclassin",
        typoutput: "regclassout",
        typalign: "i",
        typstorage: "p",
    },
];

fn build_pg_type_plan(catalog: &Arc<dyn CatalogReader>, txn_id: TxnId) -> DbResult<LogicalPlan> {
    let output_fields = pg_type_fields();
    let owner_oid = compat_role_oid(COMPAT_BOOTSTRAP_ROLE_NAME);
    let schema_namespace_oids = catalog
        .list_schemas(txn_id)?
        .into_iter()
        .map(|schema| {
            let namespace_oid = if schema.schema_id.get() == 1 {
                PUBLIC_NAMESPACE_OID
            } else {
                u64_to_i32_saturating(schema.schema_id.get()).saturating_add(16384)
            };
            (schema.name.to_ascii_lowercase(), namespace_oid)
        })
        .collect::<HashMap<_, _>>();
    let mut rows: Vec<Vec<TypedExpr>> = PG_TYPE_ENTRIES
        .iter()
        .map(|entry| {
            // Text/string types get typcollation = 100 (default collation OID)
            let typcoll = if entry.typcollation != 0 {
                entry.typcollation
            } else if entry.typcategory == "S" {
                100
            } else {
                0
            };
            vec![
                int_literal(entry.oid),
                text_literal(entry.name),
                int_literal(entry.array_oid),
                int_literal(PG_CATALOG_NAMESPACE_OID),
                int_literal(entry.len),
                text_literal(entry.delimiter),
                text_literal(entry.typtype),                 // typtype
                int_literal(0),                              // typbasetype
                int_literal(typcoll),                        // typcollation
                int_literal(0),                              // typrelid
                int_literal(entry.typelem),                  // typelem
                bool_literal(false),                         // typnotnull
                int_literal(-1),                             // typtypmod
                int_literal(0),                              // typndims
                text_literal(entry.typcategory),             // typcategory
                bool_literal(entry.typispreferred),          // typispreferred
                bool_literal(true),                          // typisdefined
                int_literal(pg_type_input_proc_oid(entry)),  // typinput
                int_literal(pg_type_output_proc_oid(entry)), // typoutput
                int_literal(0),                              // typreceive
                int_literal(0),                              // typsend
                int_literal(0),                              // typmodin
                int_literal(0),                              // typmodout
                int_literal(0),                              // typanalyze
                int_literal(0),                              // typsubscript
                int_literal(owner_oid),                      // typowner
                text_literal(entry.typalign),                // typalign
                text_literal(entry.typstorage),              // typstorage
                null_literal(DataType::Text),                // typdefault
                null_literal(DataType::Array(Box::new(DataType::Text))), // typacl
            ]
        })
        .collect();

    let compat_type_rows = with_current_session_context(|context| {
        context
            .compat_user_types
            .iter()
            .filter(|entry| !is_hidden_compat_regtype_entry(&entry.name))
            .map(|entry| {
                let array_map_key = compat_regtype_array_map_key(&entry.name);
                let array_legacy_key = compat_regtype_array_legacy_map_key(&entry.name);
                let typarray = context
                    .compat_user_types
                    .iter()
                    .find(|candidate| {
                        candidate.name == array_map_key || candidate.name == array_legacy_key
                    })
                    .map_or(0, |candidate| candidate.oid);
                let typelem = if entry.name.starts_with('_') {
                    context
                        .compat_user_types
                        .iter()
                        .find(|candidate| candidate.name == entry.name[1..])
                        .map_or(0, |candidate| candidate.oid)
                } else {
                    0
                };
                let typtype = if !entry.enum_labels.is_empty() {
                    "e"
                } else if !entry.composite_fields.is_empty() || typarray != 0 {
                    "c"
                } else {
                    "b"
                };
                let typcategory = if entry.name.starts_with('_') {
                    "A"
                } else if !entry.enum_labels.is_empty() {
                    "E"
                } else if !entry.composite_fields.is_empty() || typarray != 0 {
                    "C"
                } else {
                    "U"
                };
                vec![
                    int_literal(entry.oid),
                    text_literal(&entry.name),
                    int_literal(typarray),
                    int_literal(
                        entry
                            .schema_name
                            .as_deref()
                            .and_then(|schema_name| {
                                schema_namespace_oids
                                    .get(&schema_name.to_ascii_lowercase())
                                    .copied()
                            })
                            .unwrap_or(PUBLIC_NAMESPACE_OID),
                    ),
                    int_literal(-1),                                         // typlen
                    text_literal(","),                                       // typdelim
                    text_literal(typtype),                                   // typtype
                    int_literal(0),                                          // typbasetype
                    int_literal(0),                                          // typcollation
                    int_literal(0),                                          // typrelid
                    int_literal(typelem),                                    // typelem
                    bool_literal(false),                                     // typnotnull
                    int_literal(-1),                                         // typtypmod
                    int_literal(0),                                          // typndims
                    text_literal(typcategory),                               // typcategory
                    bool_literal(false),                                     // typispreferred
                    bool_literal(true),                                      // typisdefined
                    int_literal(0),                                          // typinput
                    int_literal(0),                                          // typoutput
                    int_literal(0),                                          // typreceive
                    int_literal(0),                                          // typsend
                    int_literal(0),                                          // typmodin
                    int_literal(0),                                          // typmodout
                    int_literal(0),                                          // typanalyze
                    int_literal(0),                                          // typsubscript
                    int_literal(owner_oid),                                  // typowner
                    text_literal("i"),                                       // typalign
                    text_literal("x"),                                       // typstorage
                    null_literal(DataType::Text),                            // typdefault
                    null_literal(DataType::Array(Box::new(DataType::Text))), // typacl
                ]
            })
            .collect::<Vec<_>>()
    });
    rows.extend(compat_type_rows);

    let compat_domain_rows = with_current_session_context(|context| {
        context
            .domain_defs
            .iter()
            .map(|entry| {
                let typnamespace = entry
                    .schema_name
                    .as_deref()
                    .and_then(|schema_name| {
                        schema_namespace_oids
                            .get(&schema_name.to_ascii_lowercase())
                            .copied()
                    })
                    .unwrap_or(PUBLIC_NAMESPACE_OID);
                let typbasetype = compat_type_oid_by_name(&entry.base_type);
                let typcollation = if matches!(
                    aiondb_eval::normalize_compat_type_name(&entry.base_type).as_str(),
                    "text" | "varchar" | "bpchar" | "char" | "character" | "name"
                ) {
                    100
                } else {
                    0
                };
                vec![
                    int_literal(compat_domain_oid(entry.schema_name.as_deref(), &entry.name)),
                    text_literal(&entry.name),
                    int_literal(0), // typarray
                    int_literal(typnamespace),
                    int_literal(-1),              // typlen
                    text_literal(","),            // typdelim
                    text_literal("d"),            // typtype
                    int_literal(typbasetype),     // typbasetype
                    int_literal(typcollation),    // typcollation
                    int_literal(0),               // typrelid
                    int_literal(0),               // typelem
                    bool_literal(entry.not_null), // typnotnull
                    int_literal(-1),              // typtypmod
                    int_literal(0),               // typndims
                    text_literal("U"),            // typcategory
                    bool_literal(false),          // typispreferred
                    bool_literal(true),           // typisdefined
                    int_literal(0),               // typinput
                    int_literal(0),               // typoutput
                    int_literal(0),               // typreceive
                    int_literal(0),               // typsend
                    int_literal(0),               // typmodin
                    int_literal(0),               // typmodout
                    int_literal(0),               // typanalyze
                    int_literal(0),               // typsubscript
                    int_literal(owner_oid),       // typowner
                    text_literal("i"),            // typalign
                    text_literal("x"),            // typstorage
                    entry
                        .default_expr
                        .as_ref()
                        .map_or_else(|| null_literal(DataType::Text), |value| text_literal(value)), // typdefault
                    null_literal(DataType::Array(Box::new(DataType::Text))), // typacl
                ]
            })
            .collect::<Vec<_>>()
    });
    rows.extend(compat_domain_rows);

    Ok(project_values(output_fields, rows))
}

// ---------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------

/// True if `schema_name` denotes a system / built-in schema whose objects
/// must not appear in pg_class / pg_attribute / pg_views as user objects.
fn is_system_schema_name(schema_name: &str) -> bool {
    matches!(
        schema_name.to_ascii_lowercase().as_str(),
        "pg_catalog" | "information_schema" | "pg_toast"
    )
}

/// True if a user-visible schema should be included given the active tenant
/// filter. When a tenant is active, only that tenant's schema is visible.
/// When no tenant is active, every non-system schema is visible.
fn schema_visible_with_tenant_filter(schema_name: &str, tenant_filter: Option<&str>) -> bool {
    if is_system_schema_name(schema_name) {
        return false;
    }
    match tenant_filter {
        Some(filter) => schema_name.eq_ignore_ascii_case(filter),
        None => true,
    }
}

/// List user tables, filtered by the active tenant schema when set.
pub(super) fn list_user_tables(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
) -> DbResult<Vec<TableDescriptor>> {
    let tenant_filter = tenant_schema_filter(default_schema);
    let mut tables = Vec::new();
    for schema in catalog.list_schemas(txn_id)? {
        if !schema_visible_with_tenant_filter(&schema.name, tenant_filter.as_deref()) {
            continue;
        }
        tables.extend(catalog.list_tables(txn_id, schema.schema_id)?);
    }
    Ok(tables)
}

/// List user sequences, filtered by the active tenant schema when set.
pub(super) fn list_user_sequences(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
) -> DbResult<Vec<aiondb_catalog::SequenceDescriptor>> {
    let tenant_filter = tenant_schema_filter(default_schema);
    let mut sequences = Vec::new();
    for schema in catalog.list_schemas(txn_id)? {
        if !schema_visible_with_tenant_filter(&schema.name, tenant_filter.as_deref()) {
            continue;
        }
        sequences.extend(catalog.list_sequences(txn_id, schema.schema_id)?);
    }
    Ok(sequences)
}

/// If `default_schema` starts with `tenant_`, return it as the tenant filter.
pub(super) fn tenant_schema_filter(default_schema: Option<&str>) -> Option<String> {
    default_schema
        .filter(|s| s.starts_with("tenant_") || s.starts_with("db_"))
        .map(|s| s.to_owned())
}

pub(super) fn visible_schema_name(schema_name: &str, default_schema: Option<&str>) -> String {
    if default_schema
        .is_some_and(|schema| schema.starts_with("db_") && schema_name.eq_ignore_ascii_case(schema))
    {
        "public".to_owned()
    } else {
        schema_name.to_owned()
    }
}

/// Deterministic OID derived from the table's `RelationId`.
pub(super) fn relation_id_to_oid(table: &TableDescriptor) -> i32 {
    // Offset by 16384 (first user OID in PostgreSQL) to avoid clashing with
    // system OIDs.
    u64_to_i32_saturating(table.table_id.get()).saturating_add(16384)
}

/// Deterministic OID derived from an index's `IndexId`.
fn index_id_to_oid(idx: &aiondb_catalog::IndexDescriptor) -> i32 {
    u64_to_i32_saturating(idx.index_id.get()).saturating_add(32768)
}

/// Deterministic OID derived from a sequence's `SequenceId`.
pub(super) fn sequence_id_to_oid(sequence: &aiondb_catalog::SequenceDescriptor) -> i32 {
    u64_to_i32_saturating(sequence.sequence_id.get()).saturating_add(49152)
}

/// Map a table's `SchemaId` to the corresponding namespace OID.
fn schema_id_to_namespace_oid(table: &TableDescriptor) -> i32 {
    // The in-memory catalog assigns SchemaId(1) to "public".
    // We map that to the well-known PUBLIC_NAMESPACE_OID.
    // For any other schema we derive a deterministic OID.
    let sid = table.schema_id.get();
    if sid == 1 {
        PUBLIC_NAMESPACE_OID
    } else {
        u64_to_i32_saturating(sid).saturating_add(16384)
    }
}

/// Crate-visible facade re-exporting `is_primary_key_index` for the
/// information_schema constraint views.
pub(crate) fn is_primary_key_index_for_info_schema(
    table: &TableDescriptor,
    idx: &aiondb_catalog::IndexDescriptor,
) -> bool {
    is_primary_key_index(table, idx)
}

/// Returns `true` when the given index covers exactly the table's primary key
/// columns.
fn is_primary_key_index(table: &TableDescriptor, idx: &aiondb_catalog::IndexDescriptor) -> bool {
    let Some(pk) = &table.primary_key else {
        return false;
    };
    if idx.key_columns.len() != pk.len() {
        return false;
    }
    idx.key_columns
        .iter()
        .zip(pk.iter())
        .all(|(kc, pk_col)| kc.column_id == *pk_col)
}

// ---------------------------------------------------------------
// Typed-expression builder helpers
// ---------------------------------------------------------------

pub(super) fn text_literal(s: &str) -> TypedExpr {
    TypedExpr::literal(Value::Text(s.to_owned()), DataType::Text, false)
}

pub(super) fn int_literal(n: i32) -> TypedExpr {
    TypedExpr::literal(Value::Int(n), DataType::Int, false)
}

pub(super) fn bigint_literal(value: i64) -> TypedExpr {
    TypedExpr::literal(Value::BigInt(value), DataType::BigInt, false)
}

pub(super) fn double_literal(value: f64) -> TypedExpr {
    TypedExpr::literal(Value::Double(value), DataType::Double, false)
}

pub(super) fn bool_literal(b: bool) -> TypedExpr {
    TypedExpr::literal(Value::Boolean(b), DataType::Boolean, false)
}

pub(super) fn null_literal(data_type: DataType) -> TypedExpr {
    TypedExpr::literal(Value::Null, data_type, true)
}

pub(super) fn text_field(name: &str) -> ResultField {
    ResultField {
        name: name.to_owned(),
        data_type: DataType::Text,
        text_type_modifier: None,
        nullable: false,
    }
}

pub(super) fn name_field(name: &str) -> ResultField {
    ResultField {
        name: name.to_owned(),
        data_type: DataType::Text,
        text_type_modifier: Some(TextTypeModifier::Name),
        nullable: false,
    }
}

pub(super) fn internal_char_field(name: &str) -> ResultField {
    ResultField {
        name: name.to_owned(),
        data_type: DataType::Text,
        text_type_modifier: Some(TextTypeModifier::InternalChar),
        nullable: false,
    }
}

pub(super) fn int_field(name: &str) -> ResultField {
    ResultField {
        name: name.to_owned(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: false,
    }
}

pub(super) fn oid_field(name: &str) -> ResultField {
    ResultField {
        name: name.to_owned(),
        data_type: DataType::Int,
        text_type_modifier: Some(TextTypeModifier::Oid),
        nullable: false,
    }
}

pub(super) fn bool_field(name: &str) -> ResultField {
    ResultField {
        name: name.to_owned(),
        data_type: DataType::Boolean,
        text_type_modifier: None,
        nullable: false,
    }
}

pub(super) fn double_field(name: &str) -> ResultField {
    ResultField {
        name: name.to_owned(),
        data_type: DataType::Double,
        text_type_modifier: None,
        nullable: false,
    }
}

pub(super) fn nullable_int_field(name: &str) -> ResultField {
    ResultField {
        name: name.to_owned(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable: true,
    }
}

pub(super) fn nullable_oid_field(name: &str) -> ResultField {
    ResultField {
        name: name.to_owned(),
        data_type: DataType::Int,
        text_type_modifier: Some(TextTypeModifier::Oid),
        nullable: true,
    }
}

pub(super) fn bigint_field(name: &str) -> ResultField {
    ResultField {
        name: name.to_owned(),
        data_type: DataType::BigInt,
        text_type_modifier: None,
        nullable: false,
    }
}

pub(super) fn nullable_double_field(name: &str) -> ResultField {
    ResultField {
        name: name.to_owned(),
        data_type: DataType::Double,
        text_type_modifier: None,
        nullable: true,
    }
}

pub(super) fn nullable_timestamp_field(name: &str) -> ResultField {
    ResultField {
        name: name.to_owned(),
        data_type: DataType::TimestampTz,
        text_type_modifier: None,
        nullable: true,
    }
}

pub(super) fn nullable_text_field(name: &str) -> ResultField {
    ResultField {
        name: name.to_owned(),
        data_type: DataType::Text,
        text_type_modifier: None,
        nullable: true,
    }
}

pub(super) fn nullable_name_field(name: &str) -> ResultField {
    ResultField {
        name: name.to_owned(),
        data_type: DataType::Text,
        text_type_modifier: Some(TextTypeModifier::Name),
        nullable: true,
    }
}

pub(super) fn project_values(
    output_fields: Vec<ResultField>,
    rows: Vec<Vec<TypedExpr>>,
) -> LogicalPlan {
    LogicalPlan::ProjectValues {
        output_fields,
        rows,
        order_by: Vec::new(),
        limit: None,
        offset: None,
    }
}

/// Build a synthetic `TableDescriptor` for a pg_catalog virtual table.
/// This allows the binder to resolve pg_catalog tables in JOINs and subqueries.
pub(crate) fn build_table_descriptor(table_name: &str) -> Option<TableDescriptor> {
    let fields = output_fields_for(table_name)?;
    let columns = fields
        .iter()
        .enumerate()
        .map(|(i, f)| ColumnDescriptor {
            column_id: ColumnId::new(usize_to_u64_saturating(i.saturating_add(1))),
            name: f.name.clone(),
            data_type: f.data_type.clone(),
            raw_type_name: None,
            text_type_modifier: f.text_type_modifier,
            nullable: f.nullable,
            ordinal_position: usize_to_u32_saturating(i.saturating_add(1)),
            default_value: None,
        })
        .collect();
    Some(TableDescriptor {
        table_id: RelationId::new(synthetic_table_id(table_name)?),
        schema_id: SchemaId::new(u64::try_from(PG_CATALOG_NAMESPACE_OID).unwrap_or(0)),
        name: QualifiedName::new(Some("pg_catalog"), table_name),
        columns,
        primary_key: None,
        foreign_keys: Vec::new(),
        check_constraints: Vec::new(),
        shard_config: None,
        identity_columns: Vec::new(),
        owner: None,
    })
}

/// Return the table name for a synthetic `RelationId`, or `None` if the id
/// does not correspond to a known pg_catalog virtual table.
pub fn table_name_for_synthetic_id(id: u64) -> Option<&'static str> {
    static NAMES: &[&str] = &[
        PG_NAMESPACE,
        PG_CLASS,
        PG_ATTRIBUTE,
        PG_TYPE,
        PG_INDEX,
        PG_CONSTRAINT,
        PG_AM,
        PG_INDEXES,
        PG_STAT_ALL_TABLES,
        PG_STATIO_ALL_TABLES,
        PG_STATS,
        PG_RULES,
        PG_AUTH_MEMBERS,
        PG_PREPARED_XACTS,
        PG_TS_CONFIG,
        PG_TS_DICT,
        PG_TS_PARSER,
        PG_TS_TEMPLATE,
        PG_AUTHID,
        PG_ROLES,
        PG_PROC,
        PG_DEPEND,
        PG_DESCRIPTION,
        PG_INIT_PRIVS,
        PG_AVAILABLE_EXTENSION_VERSIONS,
        PG_AVAILABLE_EXTENSIONS,
        PG_BACKEND_MEMORY_CONTEXTS,
        PG_CONFIG,
        PG_CURSORS,
        PG_DATABASE,
        PG_FILE_SETTINGS,
        PG_HBA_FILE_RULES,
        PG_IDENT_FILE_MAPPINGS,
        PG_LOCKS,
        PG_PREPARED_STATEMENTS,
        PG_STAT_STATEMENTS,
        PG_STAT_USER_INDEXES,
        PG_STATIO_USER_INDEXES,
        PG_SETTINGS,
        PG_STAT_ACTIVITY,
        PG_STAT_SLRU,
        PG_STAT_WAL,
        PG_STAT_WAL_RECEIVER,
        PG_TIMEZONE_ABBREVS,
        PG_TIMEZONE_NAMES,
        PG_OPERATOR,
        PG_CAST,
        PG_AGGREGATE,
        PG_AMOP,
        PG_AMPROC,
        PG_OPCLASS,
        PG_OPFAMILY,
        PG_CONVERSION,
        PG_LANGUAGE,
        PG_COLLATION,
        PG_TABLESPACE,
        PG_RANGE,
        PG_ENUM,
        PG_TRIGGER,
        PG_REWRITE,
        PG_INHERITS,
        PG_SHDESCRIPTION,
        PG_EXTENSION,
        PG_EVENT_TRIGGER,
        PG_FOREIGN_SERVER,
        PG_FOREIGN_TABLE,
        PG_FOREIGN_DATA_WRAPPER,
        PG_DB_ROLE_SETTING,
        PG_USER_MAPPINGS,
        PG_USER_MAPPING,
        PG_MATVIEWS,
        PG_POLICY,
        PG_SEQUENCE,
        PG_SEQUENCES,
        PG_STATISTIC,
        PG_STATISTIC_EXT,
        PG_STATISTIC_EXT_DATA,
        PG_STATS_EXT,
        PG_STATS_EXT_EXPRS,
        PG_ATTRDEF,
        PG_PARTITIONED_TABLE,
        PG_STAT_USER_TABLES,
        PG_STATIO_USER_TABLES,
        PG_STAT_USER_FUNCTIONS,
        PG_STAT_DATABASE,
        PG_STAT_BGWRITER,
        PG_STAT_ARCHIVER,
        PG_STAT_IO,
        PG_VIEWS,
        PG_TABLES,
        PG_USER,
        PG_SHADOW,
        PG_REPLICATION_SLOTS,
        PG_STAT_REPLICATION,
        PG_REPLICATION_ORIGIN,
        PG_DEFAULT_ACL,
        PG_SHDEPEND,
        PG_SHMEM_ALLOCATIONS,
        PG_LARGEOBJECT,
        PG_LARGEOBJECT_METADATA,
        PG_SECLABEL,
        PG_COMPAT_OBJECT_ATTRS,
        PG_COMPAT_TRIGGER_STATE,
        PG_PUBLICATION,
        PG_PUBLICATION_NAMESPACE,
        PG_PUBLICATION_REL,
        PG_SUBSCRIPTION,
    ];
    NAMES
        .iter()
        .copied()
        .find(|name| synthetic_table_id(name) == Some(id))
}

/// Return the synthetic `RelationId` for a pg_catalog table, or `None` if
/// unrecognized.  Used by the engine to detect virtual table scans.
pub fn synthetic_table_id(table_name: &str) -> Option<u64> {
    Some(match table_name.to_ascii_lowercase().as_str() {
        PG_NAMESPACE => 60_001,
        PG_CLASS => 60_002,
        PG_ATTRIBUTE => 60_003,
        PG_TYPE => 60_004,
        PG_INDEX => 60_005,
        PG_CONSTRAINT => 60_006,
        PG_AM => 60_007,
        PG_INDEXES => 60_008,
        PG_STAT_ALL_TABLES => 60_009,
        PG_STATIO_ALL_TABLES => 60_224,
        PG_STATS => 60_010,
        PG_RULES => 60_011,
        PG_AUTH_MEMBERS => 60_012,
        PG_PREPARED_XACTS => 60_013,
        PG_TS_CONFIG => 60_014,
        PG_TS_DICT => 60_015,
        PG_TS_PARSER => 60_016,
        PG_TS_TEMPLATE => 60_093,
        PG_AUTHID => 60_017,
        PG_ROLES => 60_018,
        PG_PROC => 60_019,
        PG_DEPEND => 60_020,
        PG_DESCRIPTION => 60_021,
        PG_INIT_PRIVS => 60_022,
        PG_AVAILABLE_EXTENSION_VERSIONS => 60_023,
        PG_AVAILABLE_EXTENSIONS => 60_024,
        PG_BACKEND_MEMORY_CONTEXTS => 60_025,
        PG_CONFIG => 60_026,
        PG_CURSORS => 60_027,
        PG_DATABASE => 60_028,
        PG_FILE_SETTINGS => 60_029,
        PG_HBA_FILE_RULES => 60_030,
        PG_IDENT_FILE_MAPPINGS => 60_031,
        PG_LOCKS => 60_032,
        PG_PREPARED_STATEMENTS => 60_033,
        PG_STAT_STATEMENTS => 60_215,
        PG_STAT_USER_INDEXES => 60_216,
        PG_STATIO_USER_INDEXES => 60_217,
        PG_SETTINGS => 60_034,
        PG_STAT_ACTIVITY => 60_035,
        PG_STAT_SLRU => 60_036,
        PG_STAT_WAL => 60_037,
        PG_STAT_WAL_RECEIVER => 60_038,
        PG_TIMEZONE_ABBREVS => 60_039,
        PG_TIMEZONE_NAMES => 60_040,
        PG_OPERATOR => 60_041,
        PG_CAST => 60_042,
        PG_AGGREGATE => 60_043,
        PG_AMOP => 60_044,
        PG_AMPROC => 60_045,
        PG_OPCLASS => 60_046,
        PG_OPFAMILY => 60_047,
        PG_CONVERSION => 60_048,
        PG_LANGUAGE => 60_049,
        PG_COLLATION => 60_050,
        PG_TABLESPACE => 60_051,
        PG_RANGE => 60_052,
        PG_ENUM => 60_053,
        PG_TRIGGER => 60_054,
        PG_REWRITE => 60_055,
        PG_INHERITS => 60_056,
        PG_SHDESCRIPTION => 60_057,
        PG_EXTENSION => 60_058,
        PG_EVENT_TRIGGER => 60_059,
        PG_FOREIGN_SERVER => 60_060,
        PG_FOREIGN_TABLE => 60_091,
        PG_FOREIGN_DATA_WRAPPER => 60_061,
        PG_DB_ROLE_SETTING => 60_092,
        PG_USER_MAPPINGS => 60_097,
        PG_USER_MAPPING => 60_213,
        PG_MATVIEWS => 60_098,
        PG_POLICY => 60_062,
        PG_SEQUENCE => 60_063,
        PG_SEQUENCES => 60_214,
        PG_STATISTIC => 60_064,
        PG_ATTRDEF => 60_065,
        PG_PARTITIONED_TABLE => 60_066,
        PG_STAT_USER_TABLES => 60_067,
        PG_STATIO_USER_TABLES => 60_068,
        PG_STAT_USER_FUNCTIONS => 60_069,
        PG_STAT_DATABASE => 60_070,
        PG_STAT_BGWRITER => 60_071,
        PG_STAT_ARCHIVER => 60_072,
        PG_STAT_IO => 60_073,
        PG_VIEWS => 60_074,
        PG_TABLES => 60_218,
        PG_USER => 60_219,
        PG_SHADOW => 60_220,
        PG_REPLICATION_SLOTS => 60_221,
        PG_STAT_REPLICATION => 60_222,
        PG_REPLICATION_ORIGIN => 60_223,
        PG_DEFAULT_ACL => 60_075,
        PG_SHDEPEND => 60_076,
        PG_SHMEM_ALLOCATIONS => 60_077,
        PG_LARGEOBJECT => 60_078,
        PG_LARGEOBJECT_METADATA => 60_079,
        PG_SECLABEL => 60_080,
        PG_COMPAT_OBJECT_ATTRS => 60_081,
        PG_COMPAT_TRIGGER_STATE => 60_082,
        PG_PUBLICATION => 60_083,
        PG_SUBSCRIPTION => 60_084,
        PG_STATISTIC_EXT => 60_085,
        PG_STATISTIC_EXT_DATA => 60_086,
        PG_STATS_EXT => 60_087,
        PG_STATS_EXT_EXPRS => 60_088,
        PG_PUBLICATION_NAMESPACE => 60_089,
        PG_PUBLICATION_REL => 60_090,
        _ => return None,
    })
}

#[cfg(test)]
mod tests;
