#![allow(
    clippy::redundant_closure,
    clippy::uninlined_format_args,
    clippy::doc_markdown
)]

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use crate::binder::views::{view_check_option_mode, ViewCheckOptionMode};
use crate::pg_catalog::matview::parse_matview_sidecar;
use aiondb_catalog::{
    CatalogPrivilege, CatalogReader, ColumnDescriptor, PrivilegeTarget, QualifiedName,
    SequenceDescriptor, TableDescriptor, TriggerEventDescriptor, TriggerTimingDescriptor,
    ViewDescriptor,
};
use aiondb_core::{
    ColumnId, DataType, DbError, DbResult, RelationId, SchemaId, SqlState, TxnId, Value,
};
use aiondb_parser::identifier::is_system_column_name;
use aiondb_parser::{Expr, SelectStatement, Statement};
use aiondb_plan::{LogicalPlan, ResultField, TypedExpr};

mod constraints;
mod filtering;
pub(crate) mod query_helpers;
mod triggers;
mod virtual_query;
use query_helpers::{is_star_expr, rows_to_typed};

/// Recognized `information_schema` virtual table names (lowercased).
const TABLES_TABLE: &str = "tables";
const COLUMNS_TABLE: &str = "columns";
const SCHEMATA_TABLE: &str = "schemata";
const VIEWS_TABLE: &str = "views";
const SEQUENCES_TABLE: &str = "sequences";
const TRIGGERS_TABLE: &str = "triggers";
const FOREIGN_DATA_WRAPPERS_TABLE: &str = "foreign_data_wrappers";
const FOREIGN_DATA_WRAPPER_OPTIONS_TABLE: &str = "foreign_data_wrapper_options";
const FOREIGN_SERVERS_TABLE: &str = "foreign_servers";
const FOREIGN_SERVER_OPTIONS_TABLE: &str = "foreign_server_options";
const USER_MAPPINGS_TABLE: &str = "user_mappings";
const USER_MAPPING_OPTIONS_TABLE: &str = "user_mapping_options";
const FOREIGN_TABLES_TABLE: &str = "foreign_tables";
const FOREIGN_TABLE_OPTIONS_TABLE: &str = "foreign_table_options";
const USAGE_PRIVILEGES_TABLE: &str = "usage_privileges";
const ROLE_USAGE_GRANTS_TABLE: &str = "role_usage_grants";
const TABLE_CONSTRAINTS_TABLE: &str = "table_constraints";
const KEY_COLUMN_USAGE_TABLE: &str = "key_column_usage";
const REFERENTIAL_CONSTRAINTS_TABLE: &str = "referential_constraints";
const CONSTRAINT_COLUMN_USAGE_TABLE: &str = "constraint_column_usage";
const TABLE_PRIVILEGES_TABLE: &str = "table_privileges";
const ROLE_TABLE_GRANTS_TABLE: &str = "role_table_grants";
const ROUTINES_TABLE: &str = "routines";
const PARAMETERS_TABLE: &str = "parameters";
const DOMAINS_TABLE: &str = "domains";
const APPLICABLE_ROLES_TABLE: &str = "applicable_roles";
const ENABLED_ROLES_TABLE: &str = "enabled_roles";
const CHARACTER_SETS_TABLE: &str = "character_sets";
const COLLATIONS_TABLE: &str = "collations";

/// Returns `true` if the given schema name refers to `information_schema`.
pub(crate) fn is_information_schema(schema: &str) -> bool {
    schema.eq_ignore_ascii_case("information_schema")
}

pub(crate) fn build_select_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    select: &SelectStatement,
    default_schema: Option<&str>,
    database_name: Option<&str>,
) -> DbResult<Option<LogicalPlan>> {
    virtual_query::build_select_plan(catalog, txn_id, select, default_schema, database_name)
}

/// Build a `LogicalPlan::ProjectValues` for the requested `information_schema`
/// virtual table. Returns `None` if the table name is not recognized.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn build_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    table_name: &str,
    default_schema: Option<&str>,
    database_name: Option<&str>,
) -> DbResult<Option<LogicalPlan>> {
    let lower = table_name.to_ascii_lowercase();
    match lower.as_str() {
        TABLES_TABLE => build_tables_plan(catalog, txn_id, default_schema, database_name).map(Some),
        COLUMNS_TABLE => {
            build_columns_plan(catalog, txn_id, default_schema, database_name).map(Some)
        }
        SCHEMATA_TABLE => {
            build_schemata_plan(catalog, txn_id, default_schema, database_name).map(Some)
        }
        VIEWS_TABLE => build_views_plan(catalog, txn_id, default_schema, database_name).map(Some),
        SEQUENCES_TABLE => {
            build_sequences_plan(catalog, txn_id, default_schema, database_name).map(Some)
        }
        TRIGGERS_TABLE => {
            triggers::build_triggers_plan(catalog, txn_id, default_schema, database_name).map(Some)
        }
        FOREIGN_DATA_WRAPPERS_TABLE => {
            build_foreign_data_wrappers_plan(catalog, txn_id, default_schema, database_name)
                .map(Some)
        }
        FOREIGN_DATA_WRAPPER_OPTIONS_TABLE => {
            build_foreign_data_wrapper_options_plan(catalog, txn_id, default_schema, database_name)
                .map(Some)
        }
        FOREIGN_SERVERS_TABLE => {
            build_foreign_servers_plan(catalog, txn_id, default_schema, database_name).map(Some)
        }
        FOREIGN_SERVER_OPTIONS_TABLE => {
            build_foreign_server_options_plan(catalog, txn_id, default_schema, database_name)
                .map(Some)
        }
        USER_MAPPINGS_TABLE => {
            build_user_mappings_plan(catalog, txn_id, default_schema, database_name).map(Some)
        }
        USER_MAPPING_OPTIONS_TABLE => {
            build_user_mapping_options_plan(catalog, txn_id, default_schema, database_name)
                .map(Some)
        }
        FOREIGN_TABLES_TABLE => {
            build_foreign_tables_plan(catalog, txn_id, default_schema, database_name).map(Some)
        }
        FOREIGN_TABLE_OPTIONS_TABLE => {
            build_foreign_table_options_plan(catalog, txn_id, default_schema, database_name)
                .map(Some)
        }
        USAGE_PRIVILEGES_TABLE => {
            build_usage_privileges_plan(catalog, txn_id, default_schema, database_name).map(Some)
        }
        ROLE_USAGE_GRANTS_TABLE => {
            build_role_usage_grants_plan(catalog, txn_id, default_schema, database_name).map(Some)
        }
        TABLE_CONSTRAINTS_TABLE => constraints::build_table_constraints_plan(
            catalog,
            txn_id,
            default_schema,
            database_name,
        )
        .map(Some),
        KEY_COLUMN_USAGE_TABLE => {
            constraints::build_key_column_usage_plan(catalog, txn_id, default_schema, database_name)
                .map(Some)
        }
        REFERENTIAL_CONSTRAINTS_TABLE => constraints::build_referential_constraints_plan(
            catalog,
            txn_id,
            default_schema,
            database_name,
        )
        .map(Some),
        CONSTRAINT_COLUMN_USAGE_TABLE => constraints::build_constraint_column_usage_plan(
            catalog,
            txn_id,
            default_schema,
            database_name,
        )
        .map(Some),
        TABLE_PRIVILEGES_TABLE => {
            constraints::build_table_privileges_plan(catalog, txn_id, default_schema, database_name)
                .map(Some)
        }
        ROLE_TABLE_GRANTS_TABLE => constraints::build_role_table_grants_plan(
            catalog,
            txn_id,
            default_schema,
            database_name,
        )
        .map(Some),
        ROUTINES_TABLE => {
            constraints::build_routines_plan(catalog, txn_id, default_schema, database_name)
                .map(Some)
        }
        PARAMETERS_TABLE => Ok(Some(constraints::build_empty_parameters_plan()?)),
        DOMAINS_TABLE => Ok(Some(constraints::build_empty_domains_plan(database_name)?)),
        APPLICABLE_ROLES_TABLE => Ok(Some(constraints::build_applicable_roles_plan(
            catalog, txn_id,
        )?)),
        ENABLED_ROLES_TABLE => Ok(Some(constraints::build_enabled_roles_plan(
            catalog, txn_id,
        )?)),
        CHARACTER_SETS_TABLE => Ok(Some(constraints::build_character_sets_plan(database_name)?)),
        COLLATIONS_TABLE => Ok(Some(constraints::build_collations_plan(database_name)?)),
        _ => Ok(None),
    }
}

/// Return the `ResultField` descriptors for a given `information_schema` table.
/// Used by `describe` to return column metadata without executing.
pub(crate) fn output_fields_for(table_name: &str) -> Option<Vec<ResultField>> {
    let lower = table_name.to_ascii_lowercase();
    match lower.as_str() {
        TABLES_TABLE => Some(tables_output_fields()),
        COLUMNS_TABLE => Some(columns_output_fields()),
        SCHEMATA_TABLE => Some(schemata_output_fields()),
        VIEWS_TABLE => Some(views_output_fields()),
        SEQUENCES_TABLE => Some(sequences_output_fields()),
        TRIGGERS_TABLE => Some(triggers::triggers_output_fields()),
        FOREIGN_DATA_WRAPPERS_TABLE => Some(foreign_data_wrappers_output_fields()),
        FOREIGN_DATA_WRAPPER_OPTIONS_TABLE => Some(foreign_data_wrapper_options_output_fields()),
        FOREIGN_SERVERS_TABLE => Some(foreign_servers_output_fields()),
        FOREIGN_SERVER_OPTIONS_TABLE => Some(foreign_server_options_output_fields()),
        USER_MAPPINGS_TABLE => Some(user_mappings_output_fields()),
        USER_MAPPING_OPTIONS_TABLE => Some(user_mapping_options_output_fields()),
        FOREIGN_TABLES_TABLE => Some(foreign_tables_output_fields()),
        FOREIGN_TABLE_OPTIONS_TABLE => Some(foreign_table_options_output_fields()),
        USAGE_PRIVILEGES_TABLE => Some(usage_privileges_output_fields()),
        ROLE_USAGE_GRANTS_TABLE => Some(role_usage_grants_output_fields()),
        TABLE_CONSTRAINTS_TABLE => Some(constraints::table_constraints_output_fields()),
        KEY_COLUMN_USAGE_TABLE => Some(constraints::key_column_usage_output_fields()),
        REFERENTIAL_CONSTRAINTS_TABLE => Some(constraints::referential_constraints_output_fields()),
        CONSTRAINT_COLUMN_USAGE_TABLE => Some(constraints::constraint_column_usage_output_fields()),
        TABLE_PRIVILEGES_TABLE => Some(constraints::table_privileges_output_fields()),
        ROLE_TABLE_GRANTS_TABLE => Some(constraints::role_table_grants_output_fields()),
        ROUTINES_TABLE => Some(constraints::routines_output_fields()),
        PARAMETERS_TABLE => Some(constraints::parameters_output_fields()),
        DOMAINS_TABLE => Some(constraints::domains_output_fields()),
        APPLICABLE_ROLES_TABLE => Some(constraints::applicable_roles_output_fields()),
        ENABLED_ROLES_TABLE => Some(constraints::enabled_roles_output_fields()),
        CHARACTER_SETS_TABLE => Some(constraints::character_sets_output_fields()),
        COLLATIONS_TABLE => Some(constraints::collations_output_fields()),
        _ => None,
    }
}

pub(crate) fn build_table_descriptor(table_name: &str) -> Option<TableDescriptor> {
    let fields = output_fields_for(table_name)?;
    let columns = fields
        .iter()
        .enumerate()
        .map(|(i, field)| aiondb_catalog::ColumnDescriptor {
            column_id: ColumnId::new(u64::try_from(i).unwrap_or(u64::MAX).saturating_add(1)),
            name: field.name.clone(),
            data_type: field.data_type.clone(),
            raw_type_name: None,
            text_type_modifier: field.text_type_modifier,
            nullable: field.nullable,
            ordinal_position: u32::try_from(i).unwrap_or(u32::MAX).saturating_add(1),
            default_value: None,
        })
        .collect();

    Some(TableDescriptor {
        table_id: RelationId::new(synthetic_table_id(table_name)?),
        schema_id: SchemaId::new(61_000),
        name: QualifiedName::new(Some("information_schema"), table_name),
        columns,
        primary_key: None,
        foreign_keys: Vec::new(),
        check_constraints: Vec::new(),
        shard_config: None,
        identity_columns: Vec::new(),
        owner: None,
    })
}

fn synthetic_table_id(table_name: &str) -> Option<u64> {
    Some(match table_name.to_ascii_lowercase().as_str() {
        TABLES_TABLE => 61_001,
        COLUMNS_TABLE => 61_002,
        SCHEMATA_TABLE => 61_003,
        VIEWS_TABLE => 61_004,
        SEQUENCES_TABLE => 61_005,
        TRIGGERS_TABLE => 61_006,
        FOREIGN_DATA_WRAPPERS_TABLE => 61_007,
        FOREIGN_DATA_WRAPPER_OPTIONS_TABLE => 61_008,
        FOREIGN_SERVERS_TABLE => 61_009,
        FOREIGN_SERVER_OPTIONS_TABLE => 61_010,
        USER_MAPPINGS_TABLE => 61_011,
        USER_MAPPING_OPTIONS_TABLE => 61_012,
        FOREIGN_TABLES_TABLE => 61_013,
        FOREIGN_TABLE_OPTIONS_TABLE => 61_014,
        USAGE_PRIVILEGES_TABLE => 61_015,
        ROLE_USAGE_GRANTS_TABLE => 61_016,
        TABLE_CONSTRAINTS_TABLE => 61_017,
        KEY_COLUMN_USAGE_TABLE => 61_018,
        REFERENTIAL_CONSTRAINTS_TABLE => 61_019,
        CONSTRAINT_COLUMN_USAGE_TABLE => 61_020,
        TABLE_PRIVILEGES_TABLE => 61_021,
        ROLE_TABLE_GRANTS_TABLE => 61_022,
        ROUTINES_TABLE => 61_023,
        PARAMETERS_TABLE => 61_024,
        DOMAINS_TABLE => 61_025,
        APPLICABLE_ROLES_TABLE => 61_026,
        ENABLED_ROLES_TABLE => 61_027,
        CHARACTER_SETS_TABLE => 61_028,
        COLLATIONS_TABLE => 61_029,
        _ => return None,
    })
}

pub fn table_name_for_synthetic_id(id: u64) -> Option<&'static str> {
    static NAMES: &[&str] = &[
        TABLES_TABLE,
        COLUMNS_TABLE,
        SCHEMATA_TABLE,
        VIEWS_TABLE,
        SEQUENCES_TABLE,
        TRIGGERS_TABLE,
        FOREIGN_DATA_WRAPPERS_TABLE,
        FOREIGN_DATA_WRAPPER_OPTIONS_TABLE,
        FOREIGN_SERVERS_TABLE,
        FOREIGN_SERVER_OPTIONS_TABLE,
        USER_MAPPINGS_TABLE,
        USER_MAPPING_OPTIONS_TABLE,
        FOREIGN_TABLES_TABLE,
        FOREIGN_TABLE_OPTIONS_TABLE,
        USAGE_PRIVILEGES_TABLE,
        ROLE_USAGE_GRANTS_TABLE,
        TABLE_CONSTRAINTS_TABLE,
        KEY_COLUMN_USAGE_TABLE,
        REFERENTIAL_CONSTRAINTS_TABLE,
        CONSTRAINT_COLUMN_USAGE_TABLE,
        TABLE_PRIVILEGES_TABLE,
        ROLE_TABLE_GRANTS_TABLE,
        ROUTINES_TABLE,
        PARAMETERS_TABLE,
        DOMAINS_TABLE,
        APPLICABLE_ROLES_TABLE,
        ENABLED_ROLES_TABLE,
        CHARACTER_SETS_TABLE,
        COLLATIONS_TABLE,
    ];

    NAMES
        .iter()
        .copied()
        .find(|name| synthetic_table_id(name).is_some_and(|table_id| table_id == id))
}

// ---------------------------------------------------------------
// information_schema.schemata
// ---------------------------------------------------------------

fn schemata_output_fields() -> Vec<ResultField> {
    vec![
        ResultField {
            name: "catalog_name".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "schema_name".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "schema_owner".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
    ]
}

#[cfg_attr(not(test), allow(dead_code))]
fn build_schemata_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
    database_name: Option<&str>,
) -> DbResult<LogicalPlan> {
    let output_fields = schemata_output_fields();
    let rows = rows_to_typed(
        &output_fields,
        build_schemata_rows(catalog, txn_id, default_schema, database_name)?,
    );

    Ok(LogicalPlan::ProjectValues {
        output_fields,
        rows,
        order_by: Vec::new(),
        limit: None,
        offset: None,
    })
}

// ---------------------------------------------------------------
// information_schema.tables
// ---------------------------------------------------------------

fn tables_output_fields() -> Vec<ResultField> {
    vec![
        ResultField {
            name: "table_catalog".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "table_schema".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "table_name".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "table_type".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "is_insertable_into".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
    ]
}

#[cfg_attr(not(test), allow(dead_code))]
fn build_tables_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
    database_name: Option<&str>,
) -> DbResult<LogicalPlan> {
    let output_fields = tables_output_fields();
    let rows = rows_to_typed(
        &output_fields,
        build_tables_rows(catalog, txn_id, default_schema, database_name)?,
    );

    Ok(LogicalPlan::ProjectValues {
        output_fields,
        rows,
        order_by: Vec::new(),
        limit: None,
        offset: None,
    })
}

// ---------------------------------------------------------------
// information_schema.views
// ---------------------------------------------------------------

fn views_output_fields() -> Vec<ResultField> {
    vec![
        ResultField {
            name: "table_catalog".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "table_schema".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "table_name".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "view_definition".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "check_option".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "is_updatable".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "is_insertable_into".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "is_trigger_updatable".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "is_trigger_deletable".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "is_trigger_insertable_into".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
    ]
}

#[cfg_attr(not(test), allow(dead_code))]
fn build_views_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
    database_name: Option<&str>,
) -> DbResult<LogicalPlan> {
    let output_fields = views_output_fields();
    let rows = rows_to_typed(
        &output_fields,
        build_views_rows(catalog, txn_id, default_schema, database_name)?,
    );

    Ok(LogicalPlan::ProjectValues {
        output_fields,
        rows,
        order_by: Vec::new(),
        limit: None,
        offset: None,
    })
}

// ---------------------------------------------------------------
// information_schema.sequences
// ---------------------------------------------------------------

fn sequences_output_fields() -> Vec<ResultField> {
    vec![
        ResultField {
            name: "sequence_catalog".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "sequence_schema".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "sequence_name".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "data_type".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "numeric_precision".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "numeric_precision_radix".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "numeric_scale".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "start_value".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "minimum_value".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "maximum_value".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "increment".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "cycle_option".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
    ]
}

#[cfg_attr(not(test), allow(dead_code))]
fn build_sequences_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
    database_name: Option<&str>,
) -> DbResult<LogicalPlan> {
    let output_fields = sequences_output_fields();
    let rows = rows_to_typed(
        &output_fields,
        build_sequences_rows(catalog, txn_id, default_schema, database_name)?,
    );

    Ok(LogicalPlan::ProjectValues {
        output_fields,
        rows,
        order_by: Vec::new(),
        limit: None,
        offset: None,
    })
}

#[inline]
fn text(s: &str) -> TypedExpr {
    TypedExpr::literal(Value::Text(s.to_owned()), DataType::Text, false)
}

#[inline]
fn null_text() -> TypedExpr {
    TypedExpr::literal(Value::Null, DataType::Text, true)
}

#[derive(Clone, Debug)]
struct FdwInfo {
    owner: String,
    options: Vec<(String, String)>,
}

#[derive(Clone, Debug)]
struct ForeignServerInfo {
    name: String,
    owner: String,
    fdw_name: String,
    server_type: Option<String>,
    version: Option<String>,
    options: Vec<(String, String)>,
}

#[derive(Clone, Debug)]
struct UserMappingInfo {
    role: String,
    server: String,
    options: Vec<(String, String)>,
}

#[derive(Clone, Debug)]
struct ForeignTableInfo {
    name: String,
    schema: String,
    server: String,
    options: Vec<(String, String)>,
}

#[derive(Clone, Debug, Default)]
struct FdwSnapshot {
    current_user: String,
    fdws: BTreeMap<String, FdwInfo>,
    servers: BTreeMap<String, ForeignServerInfo>,
    user_mappings: Vec<UserMappingInfo>,
    foreign_tables: BTreeMap<String, ForeignTableInfo>,
}

fn info_schema_catalog_name(database_name: Option<&str>) -> String {
    match database_name {
        Some(name) if name.eq_ignore_ascii_case("default") => "regression".to_owned(),
        Some(name) => name.to_owned(),
        None => "regression".to_owned(),
    }
}

fn normalize_role_name(role: &str) -> String {
    if role.eq_ignore_ascii_case("public") {
        "public".to_owned()
    } else {
        role.to_ascii_lowercase()
    }
}

fn information_schema_role(role: &str) -> String {
    if role.eq_ignore_ascii_case("public") {
        "PUBLIC".to_owned()
    } else {
        role.to_owned()
    }
}

fn parse_mapping_key(name: &str) -> Option<(String, String)> {
    let (role, server) = name.split_once('@')?;
    Some((normalize_role_name(role), server.to_ascii_lowercase()))
}

fn parse_compat_options_joined(joined: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for entry in joined.split(',') {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some((k, v)) = trimmed.split_once('=') {
            out.push((k.trim().to_owned(), v.trim().to_owned()));
        }
    }
    out
}

fn split_top_level_csv(input: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    let bytes = input.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    let mut paren_depth: i32 = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if in_single {
            if b == b'\'' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                    i += 2;
                    continue;
                }
                in_single = false;
            }
            i += 1;
            continue;
        }
        if in_double {
            if b == b'"' {
                in_double = false;
            }
            i += 1;
            continue;
        }
        match b {
            b'\'' => {
                in_single = true;
                i += 1;
            }
            b'"' => {
                in_double = true;
                i += 1;
            }
            b'(' => {
                paren_depth += 1;
                i += 1;
            }
            b')' => {
                paren_depth -= 1;
                i += 1;
            }
            b',' if paren_depth == 0 => {
                let piece = input[start..i].trim();
                if !piece.is_empty() {
                    items.push(piece.to_owned());
                }
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    let tail = input[start..].trim();
    if !tail.is_empty() {
        items.push(tail.to_owned());
    }
    items
}

fn unquote_sql_token(token: &str) -> String {
    let trimmed = token.trim();
    if let Some(inner) = trimmed.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        return inner.replace("\"\"", "\"");
    }
    if let Some(inner) = trimmed
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
    {
        return inner.replace("''", "'");
    }
    trimmed.to_owned()
}

fn parse_options_clause_from_create_sql(sql: &str) -> Vec<(String, String)> {
    let lower = sql.to_ascii_lowercase();
    let Some(options_pos) = lower.find(" options ") else {
        return Vec::new();
    };
    let tail = &sql[options_pos..];
    let Some(open_rel) = tail.find('(') else {
        return Vec::new();
    };
    let open_idx = options_pos + open_rel;
    let mut close_idx = None;
    let mut depth = 0i32;
    let mut in_single = false;
    let mut in_double = false;
    let bytes = sql.as_bytes();
    let mut i = open_idx;
    while i < bytes.len() {
        let b = bytes[i];
        if in_single {
            if b == b'\'' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                    i += 2;
                    continue;
                }
                in_single = false;
            }
            i += 1;
            continue;
        }
        if in_double {
            if b == b'"' {
                in_double = false;
            }
            i += 1;
            continue;
        }
        match b {
            b'\'' => {
                in_single = true;
            }
            b'"' => {
                in_double = true;
            }
            b'(' => {
                depth += 1;
            }
            b')' => {
                depth -= 1;
                if depth == 0 {
                    close_idx = Some(i);
                    break;
                }
            }
            _ => {}
        }
        i += 1;
    }
    let Some(close_idx) = close_idx else {
        return Vec::new();
    };
    let inner = &sql[open_idx + 1..close_idx];
    let mut out = Vec::new();
    for item in split_top_level_csv(inner) {
        let mut chars = item.char_indices().peekable();
        let mut key_end = None;
        let mut in_sq = false;
        let mut in_dq = false;
        while let Some((idx, ch)) = chars.next() {
            if in_sq {
                if ch == '\'' {
                    if chars.peek().is_some_and(|(_, next)| *next == '\'') {
                        let _ = chars.next();
                    } else {
                        in_sq = false;
                    }
                }
                continue;
            }
            if in_dq {
                if ch == '"' {
                    in_dq = false;
                }
                continue;
            }
            match ch {
                '\'' => in_sq = true,
                '"' => in_dq = true,
                c if c.is_whitespace() => {
                    key_end = Some(idx);
                    break;
                }
                _ => {}
            }
        }
        let Some(key_end) = key_end else {
            continue;
        };
        let key = unquote_sql_token(&item[..key_end]);
        let value = unquote_sql_token(&item[key_end..]);
        if !key.is_empty() {
            out.push((key, value));
        }
    }
    out
}

fn option_value_for_display(
    mapping_role: &str,
    raw_value: &str,
    show_sensitive_values: bool,
) -> String {
    if show_sensitive_values || mapping_role.eq_ignore_ascii_case("public") {
        raw_value.to_owned()
    } else {
        String::new()
    }
}

fn build_fdw_snapshot() -> FdwSnapshot {
    aiondb_eval::with_current_session_context(|context| {
        let mut snapshot = FdwSnapshot {
            current_user: context
                .current_user
                .as_deref()
                .map(normalize_role_name)
                .unwrap_or_else(|| "public".to_owned()),
            ..FdwSnapshot::default()
        };

        for ((kind, name), create_sql) in context.compat_misc_objects.iter() {
            let (owner, schema, _state, options_joined, _tablespace, version) = context
                .compat_misc_attrs
                .get(&(kind.clone(), name.clone()))
                .cloned()
                .unwrap_or_default();
            let mut options = parse_compat_options_joined(&options_joined);
            if options.is_empty() {
                options = parse_options_clause_from_create_sql(create_sql);
            }
            if kind == "CREATE FOREIGN DATA WRAPPER" {
                snapshot.fdws.insert(
                    name.to_ascii_lowercase(),
                    FdwInfo {
                        owner: normalize_role_name(&owner),
                        options,
                    },
                );
            } else if kind == "CREATE SERVER" {
                let mut server_type = None;
                let mut fdw_name = String::new();
                let mut visible_options = Vec::new();
                for (k, v) in options {
                    match k.as_str() {
                        "type" => server_type = Some(v),
                        "fdw" => fdw_name = v.to_ascii_lowercase(),
                        _ => visible_options.push((k, v)),
                    }
                }
                snapshot.servers.insert(
                    name.to_ascii_lowercase(),
                    ForeignServerInfo {
                        name: name.to_ascii_lowercase(),
                        owner: normalize_role_name(&owner),
                        fdw_name,
                        server_type,
                        version: if version.is_empty() {
                            None
                        } else {
                            Some(version.clone())
                        },
                        options: visible_options,
                    },
                );
            } else if kind == "CREATE USER MAPPING" {
                if let Some((role, server)) = parse_mapping_key(name) {
                    snapshot.user_mappings.push(UserMappingInfo {
                        role,
                        server,
                        options,
                    });
                }
            } else if kind == "CREATE FOREIGN TABLE" {
                let mut table_server = String::new();
                let mut table_options = Vec::new();
                for (k, v) in options {
                    if k == "server" {
                        table_server = v.to_ascii_lowercase();
                    } else if !matches!(k.as_str(), "columns" | "inherits") {
                        table_options.push((k, v));
                    }
                }
                snapshot.foreign_tables.insert(
                    name.to_ascii_lowercase(),
                    ForeignTableInfo {
                        name: name.to_ascii_lowercase(),
                        schema: if schema.is_empty() {
                            "public".to_owned()
                        } else {
                            schema.to_ascii_lowercase()
                        },
                        server: table_server,
                        options: table_options,
                    },
                );
            }
        }

        snapshot
    })
}

fn list_effective_roles(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    current_role: &str,
) -> DbResult<BTreeSet<String>> {
    let mut effective = BTreeSet::new();
    let mut queue = vec![normalize_role_name(current_role)];

    while let Some(role) = queue.pop() {
        if !effective.insert(role.clone()) {
            continue;
        }
        for privilege in catalog.get_privileges(txn_id, &role)? {
            if let PrivilegeTarget::Role(parent) = privilege.target {
                let normalized = normalize_role_name(&parent);
                if !effective.contains(&normalized) {
                    queue.push(normalized);
                }
            }
        }
    }

    effective.insert("public".to_owned());
    Ok(effective)
}

fn is_superuser(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    role_name: &str,
) -> DbResult<bool> {
    Ok(catalog
        .get_role(txn_id, role_name)?
        .is_some_and(|role| role.superuser))
}

fn collect_server_acl_state(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    snapshot: &FdwSnapshot,
    current_role: &str,
    effective_roles: &BTreeSet<String>,
    is_superuser: bool,
) -> DbResult<(BTreeSet<String>, BTreeSet<String>)> {
    let mut usable_servers = BTreeSet::new();
    let mut grantable_servers = BTreeSet::new();

    for server in snapshot.servers.values() {
        let owner = normalize_role_name(&server.owner);
        if is_superuser || effective_roles.contains(&owner) {
            usable_servers.insert(server.name.clone());
            grantable_servers.insert(server.name.clone());
        }
    }

    let all_roles = catalog.list_roles(txn_id)?;
    let current_role = normalize_role_name(current_role);
    for role in all_roles {
        let role_name = normalize_role_name(&role.name);
        for privilege in catalog.get_privileges(txn_id, &role.name)? {
            let PrivilegeTarget::Schema(object_name) = privilege.target else {
                continue;
            };
            let object_name = object_name.to_ascii_lowercase();
            if !snapshot.servers.contains_key(&object_name) {
                continue;
            }
            if !matches!(
                privilege.privilege,
                CatalogPrivilege::Usage | CatalogPrivilege::All
            ) {
                continue;
            }
            if effective_roles.contains(&role_name) || is_superuser {
                usable_servers.insert(object_name.clone());
                if privilege.privilege == CatalogPrivilege::All || role_name == current_role {
                    grantable_servers.insert(object_name);
                }
            }
        }
    }

    Ok((usable_servers, grantable_servers))
}

// ---------------------------------------------------------------
// information_schema.foreign_data_wrappers / foreign_data_wrapper_options
// ---------------------------------------------------------------

fn foreign_data_wrappers_output_fields() -> Vec<ResultField> {
    vec![
        ResultField {
            name: "foreign_data_wrapper_catalog".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "foreign_data_wrapper_name".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "authorization_identifier".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "library_name".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "foreign_data_wrapper_language".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
    ]
}

#[cfg_attr(not(test), allow(dead_code))]
fn build_foreign_data_wrappers_plan(
    _catalog: &Arc<dyn CatalogReader>,
    _txn_id: TxnId,
    _default_schema: Option<&str>,
    database_name: Option<&str>,
) -> DbResult<LogicalPlan> {
    let output_fields = foreign_data_wrappers_output_fields();
    let catalog_name = info_schema_catalog_name(database_name);
    let snapshot = build_fdw_snapshot();
    let mut rows: Vec<Vec<TypedExpr>> = Vec::new();
    for (fdw_name, fdw) in snapshot.fdws {
        rows.push(vec![
            text(&catalog_name),
            text(&fdw_name),
            if fdw.owner.is_empty() {
                null_text()
            } else {
                text(&fdw.owner)
            },
            null_text(),
            text("c"),
        ]);
    }
    Ok(LogicalPlan::ProjectValues {
        output_fields,
        rows,
        order_by: Vec::new(),
        limit: None,
        offset: None,
    })
}

fn foreign_data_wrapper_options_output_fields() -> Vec<ResultField> {
    vec![
        ResultField {
            name: "foreign_data_wrapper_catalog".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "foreign_data_wrapper_name".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "option_name".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "option_value".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
    ]
}

#[cfg_attr(not(test), allow(dead_code))]
fn build_foreign_data_wrapper_options_plan(
    _catalog: &Arc<dyn CatalogReader>,
    _txn_id: TxnId,
    _default_schema: Option<&str>,
    database_name: Option<&str>,
) -> DbResult<LogicalPlan> {
    let output_fields = foreign_data_wrapper_options_output_fields();
    let catalog_name = info_schema_catalog_name(database_name);
    let snapshot = build_fdw_snapshot();
    let mut rows: Vec<Vec<TypedExpr>> = Vec::new();
    for (fdw_name, fdw) in snapshot.fdws {
        for (k, v) in fdw.options {
            if matches!(k.as_str(), "handler" | "validator") {
                continue;
            }
            rows.push(vec![
                text(&catalog_name),
                text(&fdw_name),
                text(&k),
                text(&v),
            ]);
        }
    }
    Ok(LogicalPlan::ProjectValues {
        output_fields,
        rows,
        order_by: Vec::new(),
        limit: None,
        offset: None,
    })
}

// ---------------------------------------------------------------
// information_schema.foreign_servers
// ---------------------------------------------------------------

fn foreign_servers_output_fields() -> Vec<ResultField> {
    vec![
        ResultField {
            name: "foreign_server_catalog".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "foreign_server_name".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "foreign_data_wrapper_catalog".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "foreign_data_wrapper_name".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "foreign_server_type".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "foreign_server_version".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "authorization_identifier".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
    ]
}

fn build_foreign_servers_plan(
    _catalog: &Arc<dyn CatalogReader>,
    _txn_id: TxnId,
    _default_schema: Option<&str>,
    database_name: Option<&str>,
) -> DbResult<LogicalPlan> {
    let catalog_name = info_schema_catalog_name(database_name);
    let snapshot = build_fdw_snapshot();
    let mut rows: Vec<Vec<TypedExpr>> = Vec::new();
    for server in snapshot.servers.values() {
        rows.push(vec![
            text(&catalog_name),
            text(&server.name),
            text(&catalog_name),
            if server.fdw_name.is_empty() {
                null_text()
            } else {
                text(&server.fdw_name)
            },
            match &server.server_type {
                Some(t) => text(t),
                None => null_text(),
            },
            match &server.version {
                Some(v) => text(v),
                None => null_text(),
            },
            if server.owner.is_empty() {
                null_text()
            } else {
                text(&server.owner)
            },
        ]);
    }
    Ok(LogicalPlan::ProjectValues {
        output_fields: foreign_servers_output_fields(),
        rows,
        order_by: Vec::new(),
        limit: None,
        offset: None,
    })
}

fn foreign_server_options_output_fields() -> Vec<ResultField> {
    vec![
        ResultField {
            name: "foreign_server_catalog".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "foreign_server_name".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "option_name".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "option_value".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
    ]
}

fn build_foreign_server_options_plan(
    _catalog: &Arc<dyn CatalogReader>,
    _txn_id: TxnId,
    _default_schema: Option<&str>,
    database_name: Option<&str>,
) -> DbResult<LogicalPlan> {
    let catalog_name = info_schema_catalog_name(database_name);
    let snapshot = build_fdw_snapshot();
    let mut rows: Vec<Vec<TypedExpr>> = Vec::new();
    for server in snapshot.servers.values() {
        for (k, v) in &server.options {
            rows.push(vec![
                text(&catalog_name),
                text(&server.name),
                text(k),
                text(v),
            ]);
        }
    }
    Ok(LogicalPlan::ProjectValues {
        output_fields: foreign_server_options_output_fields(),
        rows,
        order_by: Vec::new(),
        limit: None,
        offset: None,
    })
}

fn user_mappings_output_fields() -> Vec<ResultField> {
    vec![
        ResultField {
            name: "authorization_identifier".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "foreign_server_catalog".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "foreign_server_name".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
    ]
}

fn build_user_mappings_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    _default_schema: Option<&str>,
    database_name: Option<&str>,
) -> DbResult<LogicalPlan> {
    let catalog_name = info_schema_catalog_name(database_name);
    let snapshot = build_fdw_snapshot();
    let effective_roles = list_effective_roles(catalog, txn_id, &snapshot.current_user)?;
    let superuser = is_superuser(catalog, txn_id, &snapshot.current_user)?;
    let (usable_servers, _) = collect_server_acl_state(
        catalog,
        txn_id,
        &snapshot,
        &snapshot.current_user,
        &effective_roles,
        superuser,
    )?;

    let mut rows: Vec<Vec<TypedExpr>> = Vec::new();
    for mapping in &snapshot.user_mappings {
        if !superuser
            && (!usable_servers.contains(&mapping.server)
                || (!effective_roles.contains(&mapping.role)
                    && !mapping.role.eq_ignore_ascii_case("public")))
        {
            continue;
        }
        rows.push(vec![
            text(&information_schema_role(&mapping.role)),
            text(&catalog_name),
            text(&mapping.server),
        ]);
    }
    Ok(LogicalPlan::ProjectValues {
        output_fields: user_mappings_output_fields(),
        rows,
        order_by: Vec::new(),
        limit: None,
        offset: None,
    })
}

fn user_mapping_options_output_fields() -> Vec<ResultField> {
    vec![
        ResultField {
            name: "authorization_identifier".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "foreign_server_catalog".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "foreign_server_name".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "option_name".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "option_value".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
    ]
}

fn build_user_mapping_options_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    _default_schema: Option<&str>,
    database_name: Option<&str>,
) -> DbResult<LogicalPlan> {
    let catalog_name = info_schema_catalog_name(database_name);
    let snapshot = build_fdw_snapshot();
    let effective_roles = list_effective_roles(catalog, txn_id, &snapshot.current_user)?;
    let superuser = is_superuser(catalog, txn_id, &snapshot.current_user)?;
    let (usable_servers, grantable_servers) = collect_server_acl_state(
        catalog,
        txn_id,
        &snapshot,
        &snapshot.current_user,
        &effective_roles,
        superuser,
    )?;

    let mut rows: Vec<Vec<TypedExpr>> = Vec::new();
    for mapping in &snapshot.user_mappings {
        if !superuser && !usable_servers.contains(&mapping.server) {
            continue;
        }
        let role_visible = effective_roles.contains(&mapping.role)
            || mapping.role.eq_ignore_ascii_case("public")
            || grantable_servers.contains(&mapping.server);
        if !superuser && !role_visible {
            continue;
        }
        let show_value = superuser
            || effective_roles.contains(&mapping.role)
            || mapping.role.eq_ignore_ascii_case("public");
        for (k, v) in &mapping.options {
            rows.push(vec![
                text(&information_schema_role(&mapping.role)),
                text(&catalog_name),
                text(&mapping.server),
                text(k),
                text(&option_value_for_display(&mapping.role, v, show_value)),
            ]);
        }
    }
    Ok(LogicalPlan::ProjectValues {
        output_fields: user_mapping_options_output_fields(),
        rows,
        order_by: Vec::new(),
        limit: None,
        offset: None,
    })
}

fn foreign_tables_output_fields() -> Vec<ResultField> {
    vec![
        ResultField {
            name: "foreign_table_catalog".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "foreign_table_schema".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "foreign_table_name".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "foreign_server_catalog".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "foreign_server_name".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
    ]
}

fn build_foreign_tables_plan(
    _catalog: &Arc<dyn CatalogReader>,
    _txn_id: TxnId,
    _default_schema: Option<&str>,
    database_name: Option<&str>,
) -> DbResult<LogicalPlan> {
    let catalog_name = info_schema_catalog_name(database_name);
    let snapshot = build_fdw_snapshot();
    let mut rows = Vec::new();
    for table in snapshot.foreign_tables.values() {
        if table.server.is_empty() {
            continue;
        }
        rows.push(vec![
            text(&catalog_name),
            text(&table.schema),
            text(&table.name),
            text(&catalog_name),
            text(&table.server),
        ]);
    }
    Ok(LogicalPlan::ProjectValues {
        output_fields: foreign_tables_output_fields(),
        rows,
        order_by: Vec::new(),
        limit: None,
        offset: None,
    })
}

fn foreign_table_options_output_fields() -> Vec<ResultField> {
    vec![
        ResultField {
            name: "foreign_table_catalog".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "foreign_table_schema".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "foreign_table_name".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "option_name".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "option_value".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
    ]
}

fn build_foreign_table_options_plan(
    _catalog: &Arc<dyn CatalogReader>,
    _txn_id: TxnId,
    _default_schema: Option<&str>,
    database_name: Option<&str>,
) -> DbResult<LogicalPlan> {
    let catalog_name = info_schema_catalog_name(database_name);
    let snapshot = build_fdw_snapshot();
    let mut rows = Vec::new();
    for table in snapshot.foreign_tables.values() {
        if table.server.is_empty() {
            continue;
        }
        for (k, v) in &table.options {
            rows.push(vec![
                text(&catalog_name),
                text(&table.schema),
                text(&table.name),
                text(k),
                text(v),
            ]);
        }
    }
    Ok(LogicalPlan::ProjectValues {
        output_fields: foreign_table_options_output_fields(),
        rows,
        order_by: Vec::new(),
        limit: None,
        offset: None,
    })
}

fn usage_privileges_output_fields() -> Vec<ResultField> {
    vec![
        ResultField {
            name: "grantor".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "grantee".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "object_catalog".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "object_schema".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "object_name".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "object_type".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "privilege_type".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "is_grantable".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
    ]
}

fn build_usage_privileges_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    _default_schema: Option<&str>,
    database_name: Option<&str>,
) -> DbResult<LogicalPlan> {
    let catalog_name = info_schema_catalog_name(database_name);
    let snapshot = build_fdw_snapshot();
    let effective_roles = list_effective_roles(catalog, txn_id, &snapshot.current_user)?;
    let superuser = is_superuser(catalog, txn_id, &snapshot.current_user)?;

    let mut object_owners: BTreeMap<(String, String), String> = BTreeMap::new();
    for (fdw_name, fdw) in &snapshot.fdws {
        object_owners.insert(
            ("FOREIGN DATA WRAPPER".to_owned(), fdw_name.clone()),
            normalize_role_name(&fdw.owner),
        );
    }
    for (server_name, server) in &snapshot.servers {
        object_owners.insert(
            ("FOREIGN SERVER".to_owned(), server_name.clone()),
            normalize_role_name(&server.owner),
        );
    }

    let mut grant_rows: BTreeMap<(String, String, String, String), bool> = BTreeMap::new();
    for ((object_type, object_name), owner) in &object_owners {
        grant_rows.insert(
            (
                owner.clone(),
                owner.clone(),
                object_type.clone(),
                object_name.clone(),
            ),
            true,
        );
    }

    for role in catalog.list_roles(txn_id)? {
        let grantee = normalize_role_name(&role.name);
        for privilege in catalog.get_privileges(txn_id, &role.name)? {
            let PrivilegeTarget::Schema(target_name) = privilege.target else {
                continue;
            };
            let target_name = target_name.to_ascii_lowercase();
            let object_type = if snapshot.fdws.contains_key(&target_name) {
                "FOREIGN DATA WRAPPER".to_owned()
            } else if snapshot.servers.contains_key(&target_name) {
                "FOREIGN SERVER".to_owned()
            } else {
                continue;
            };
            if !matches!(
                privilege.privilege,
                CatalogPrivilege::Usage | CatalogPrivilege::All
            ) {
                continue;
            }
            let owner = object_owners
                .get(&(object_type.clone(), target_name.clone()))
                .cloned()
                .unwrap_or_default();
            let is_grantable = privilege.privilege == CatalogPrivilege::All || grantee == owner;
            let key = (owner, grantee.clone(), object_type, target_name);
            grant_rows
                .entry(key)
                .and_modify(|existing| *existing = *existing || is_grantable)
                .or_insert(is_grantable);
        }
    }

    let mut rows = Vec::new();
    for ((grantor, grantee, object_type, object_name), is_grantable) in grant_rows {
        if !superuser && !effective_roles.contains(&grantor) && !effective_roles.contains(&grantee)
        {
            continue;
        }
        rows.push(vec![
            text(&information_schema_role(&grantor)),
            text(&information_schema_role(&grantee)),
            text(&catalog_name),
            null_text(),
            text(&object_name),
            text(&object_type),
            text("USAGE"),
            text(if is_grantable { "YES" } else { "NO" }),
        ]);
    }

    Ok(LogicalPlan::ProjectValues {
        output_fields: usage_privileges_output_fields(),
        rows,
        order_by: Vec::new(),
        limit: None,
        offset: None,
    })
}

fn role_usage_grants_output_fields() -> Vec<ResultField> {
    usage_privileges_output_fields()
}

fn build_role_usage_grants_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
    database_name: Option<&str>,
) -> DbResult<LogicalPlan> {
    let usage_plan = build_usage_privileges_plan(catalog, txn_id, default_schema, database_name)?;
    let LogicalPlan::ProjectValues {
        rows,
        order_by,
        limit,
        offset,
        ..
    } = usage_plan
    else {
        return Err(DbError::internal(
            "usage_privileges plan unexpectedly not a ProjectValues",
        ));
    };
    Ok(LogicalPlan::ProjectValues {
        output_fields: role_usage_grants_output_fields(),
        rows,
        order_by,
        limit,
        offset,
    })
}

// ---------------------------------------------------------------
// information_schema.columns
// ---------------------------------------------------------------

fn columns_output_fields() -> Vec<ResultField> {
    vec![
        ResultField {
            name: "table_catalog".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "table_schema".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "table_name".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "column_name".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "ordinal_position".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "column_default".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "is_nullable".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "data_type".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "is_generated".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "generation_expression".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "is_identity".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "identity_generation".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "identity_start".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "identity_increment".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "identity_maximum".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "identity_minimum".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "identity_cycle".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "is_updatable".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "character_maximum_length".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "character_octet_length".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "character_set_catalog".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "numeric_precision".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "numeric_precision_radix".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "numeric_scale".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "datetime_precision".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "interval_type".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "interval_precision".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "udt_catalog".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "udt_schema".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "udt_name".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "dtd_identifier".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
    ]
}

#[cfg_attr(not(test), allow(dead_code))]
fn build_columns_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
    database_name: Option<&str>,
) -> DbResult<LogicalPlan> {
    let output_fields = columns_output_fields();
    let rows = rows_to_typed(
        &output_fields,
        build_columns_rows(catalog, txn_id, default_schema, database_name)?,
    );

    Ok(LogicalPlan::ProjectValues {
        output_fields,
        rows,
        order_by: Vec::new(),
        limit: None,
        offset: None,
    })
}

// ---------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------

fn build_schemata_rows(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
    database_name: Option<&str>,
) -> DbResult<Vec<Vec<Value>>> {
    let catalog_name = database_name.unwrap_or("aiondb");
    let mut rows = Vec::new();
    let mut seen = BTreeSet::new();
    for (schema_name, _) in list_user_schemas(catalog, txn_id, default_schema)? {
        if seen.insert(schema_name.to_ascii_lowercase()) {
            rows.push(vec![
                Value::Text(catalog_name.to_owned()),
                Value::Text(schema_name),
                Value::Null,
            ]);
        }
    }
    if seen.insert("information_schema".to_owned()) {
        rows.push(vec![
            Value::Text(catalog_name.to_owned()),
            Value::Text("information_schema".to_owned()),
            Value::Null,
        ]);
    }
    Ok(rows)
}

fn build_tables_rows(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
    database_name: Option<&str>,
) -> DbResult<Vec<Vec<Value>>> {
    let catalog_name = database_name.unwrap_or("aiondb");
    let tables = list_user_tables(catalog, txn_id, default_schema)?;
    let visibility = TableVisibility::for_current_session(catalog, txn_id)?;
    let mut rows = Vec::with_capacity(tables.len());

    for table in &tables {
        if !visibility.can_see_table(table) {
            continue;
        }
        let schema_name =
            visible_schema_name(table.name.schema_name().unwrap_or("public"), default_schema);
        rows.push(vec![
            Value::Text(catalog_name.to_owned()),
            Value::Text(schema_name.clone()),
            Value::Text(table.name.object_name().to_owned()),
            Value::Text("BASE TABLE".to_owned()),
            Value::Text("YES".to_owned()),
        ]);
    }

    // Also include views in the tables listing (PG does this)
    for (_, schema_id) in list_user_schemas(catalog, txn_id, default_schema)? {
        let views = catalog.list_views(txn_id, schema_id)?;
        for view in views {
            if parse_matview_sidecar(&view).is_some() {
                continue;
            }
            if !visibility.can_see_view(&view) {
                continue;
            }
            let schema_name =
                visible_schema_name(view.name.schema_name().unwrap_or("public"), default_schema);
            let metadata = analyze_view(&view, catalog, txn_id);
            rows.push(vec![
                Value::Text(catalog_name.to_owned()),
                Value::Text(schema_name),
                Value::Text(view.name.object_name().to_owned()),
                Value::Text("VIEW".to_owned()),
                Value::Text(yes_no(metadata.is_insertable_into)),
            ]);
        }
    }

    Ok(rows)
}

/// PG: information_schema views project only relations the current user has
/// any privilege on. Without this filter a non-priv role enumerates every
/// table name + schema in the catalog (metadata leak).
struct TableVisibility {
    superuser: bool,
    effective_roles: BTreeSet<String>,
    privilege_targets: BTreeSet<(Option<String>, String)>,
}

impl TableVisibility {
    fn for_current_session(catalog: &Arc<dyn CatalogReader>, txn_id: TxnId) -> DbResult<Self> {
        let current_user = aiondb_eval::with_current_session_context(|ctx| {
            ctx.current_user.as_deref().map(normalize_role_name)
        });
        let Some(current_user) = current_user else {
            return Ok(Self {
                superuser: true,
                effective_roles: BTreeSet::new(),
                privilege_targets: BTreeSet::new(),
            });
        };
        if catalog.get_role(txn_id, &current_user)?.is_none() {
            return Ok(Self {
                superuser: true,
                effective_roles: BTreeSet::new(),
                privilege_targets: BTreeSet::new(),
            });
        }
        let superuser = is_superuser(catalog, txn_id, &current_user)?;
        let effective_roles = list_effective_roles(catalog, txn_id, &current_user)?;
        let mut privilege_targets: BTreeSet<(Option<String>, String)> = BTreeSet::new();
        if !superuser {
            for role in &effective_roles {
                if role.eq_ignore_ascii_case("pg_read_all_data")
                    || role.eq_ignore_ascii_case("pg_write_all_data")
                {
                    // Predefined PG roles: bypass per-relation filter when
                    // membership grants global read/write.
                    privilege_targets.clear();
                    return Ok(Self {
                        superuser: true,
                        effective_roles,
                        privilege_targets,
                    });
                }
                for descriptor in catalog.get_privileges(txn_id, role)? {
                    if let PrivilegeTarget::Table(name) = descriptor.target {
                        privilege_targets.insert((
                            name.schema.as_ref().map(|s| s.to_ascii_lowercase()),
                            name.name.to_ascii_lowercase(),
                        ));
                    }
                }
            }
        }
        Ok(Self {
            superuser,
            effective_roles,
            privilege_targets,
        })
    }

    fn can_see_table(&self, table: &TableDescriptor) -> bool {
        if self.superuser {
            return true;
        }
        if let Some(owner) = table.owner.as_deref() {
            if self
                .effective_roles
                .iter()
                .any(|r| r.eq_ignore_ascii_case(owner))
            {
                return true;
            }
        }
        let schema = table.name.schema.as_ref().map(|s| s.to_ascii_lowercase());
        let name = table.name.name.to_ascii_lowercase();
        self.privilege_targets
            .contains(&(schema.clone(), name.clone()))
            || self.privilege_targets.contains(&(None, name))
    }

    fn can_see_view(&self, view: &ViewDescriptor) -> bool {
        if self.superuser {
            return true;
        }
        let schema = view.name.schema.as_ref().map(|s| s.to_ascii_lowercase());
        let name = view.name.name.to_ascii_lowercase();
        self.privilege_targets
            .contains(&(schema.clone(), name.clone()))
            || self.privilege_targets.contains(&(None, name))
    }
}

fn build_columns_rows(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
    database_name: Option<&str>,
) -> DbResult<Vec<Vec<Value>>> {
    let catalog_name = database_name.unwrap_or("aiondb");
    let tables = list_user_tables(catalog, txn_id, default_schema)?;
    let visibility = TableVisibility::for_current_session(catalog, txn_id)?;
    let owned_sequences = list_owned_sequences(catalog, txn_id, default_schema)?;
    let mut rows = Vec::new();

    for table in &tables {
        if !visibility.can_see_table(table) {
            continue;
        }
        let schema_name =
            visible_schema_name(table.name.schema_name().unwrap_or("public"), default_schema);
        for column in &table.columns {
            let identity = column_identity_metadata(table, column, &owned_sequences);
            rows.push(vec![
                Value::Text(catalog_name.to_owned()),
                Value::Text(schema_name.clone()),
                Value::Text(table.name.object_name().to_owned()),
                Value::Text(column.name.clone()),
                Value::Int(i32::try_from(column.ordinal_position).unwrap_or(i32::MAX)),
                identity.column_default_value(column.default_value.as_deref()),
                Value::Text(if column.nullable { "YES" } else { "NO" }.to_owned()),
                Value::Text(column_info_schema_data_type(column)),
                Value::Text("NEVER".to_owned()),
                Value::Null,
                Value::Text(yes_no(identity.is_identity)),
                identity
                    .identity_generation
                    .as_ref()
                    .map_or(Value::Null, |value| Value::Text(value.clone())),
                identity
                    .identity_start
                    .as_ref()
                    .map_or(Value::Null, |value| Value::Text(value.clone())),
                identity
                    .identity_increment
                    .as_ref()
                    .map_or(Value::Null, |value| Value::Text(value.clone())),
                identity
                    .identity_maximum
                    .as_ref()
                    .map_or(Value::Null, |value| Value::Text(value.clone())),
                identity
                    .identity_minimum
                    .as_ref()
                    .map_or(Value::Null, |value| Value::Text(value.clone())),
                Value::Text(identity.identity_cycle.clone()),
                Value::Text("YES".to_owned()),
                column_character_maximum_length(column).map_or(Value::Null, Value::Int),
                Value::Null,
                Value::Null,
                column_numeric_precision(column).map_or(Value::Null, Value::Int),
                column_numeric_precision_radix(column).map_or(Value::Null, Value::Int),
                column_numeric_scale(column).map_or(Value::Null, Value::Int),
                column_datetime_precision(&column.data_type).map_or(Value::Null, Value::Int),
                Value::Null,
                Value::Null,
                Value::Text(catalog_name.to_owned()),
                Value::Text("pg_catalog".to_owned()),
                Value::Text(column_udt_name(column)),
                Value::Text(column.ordinal_position.to_string()),
            ]);
        }
    }

    for (_, schema_id) in list_user_schemas(catalog, txn_id, default_schema)? {
        let views = catalog.list_views(txn_id, schema_id)?;
        for view in views {
            if parse_matview_sidecar(&view).is_some() {
                continue;
            }
            let schema_name =
                visible_schema_name(view.name.schema_name().unwrap_or("public"), default_schema);
            let metadata = analyze_view(&view, catalog, txn_id);
            for (index, column) in view.columns.iter().enumerate() {
                let is_updatable = metadata
                    .column_is_updatable
                    .get(index)
                    .copied()
                    .unwrap_or(false);
                rows.push(vec![
                    Value::Text(catalog_name.to_owned()),
                    Value::Text(schema_name.clone()),
                    Value::Text(view.name.object_name().to_owned()),
                    Value::Text(column.name.clone()),
                    Value::Int(i32::try_from(column.ordinal_position).unwrap_or(i32::MAX)),
                    column
                        .default_value
                        .as_ref()
                        .map_or(Value::Null, |value| Value::Text(value.clone())),
                    Value::Text(if column.nullable { "YES" } else { "NO" }.to_owned()),
                    Value::Text(pg_type_name(&column.data_type)),
                    Value::Text("NEVER".to_owned()),
                    Value::Null,
                    Value::Text("NO".to_owned()),
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Text("NO".to_owned()),
                    Value::Text(yes_no(is_updatable)),
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    column_numeric_precision(column).map_or(Value::Null, Value::Int),
                    column_numeric_precision_radix(column).map_or(Value::Null, Value::Int),
                    column_numeric_scale(column).map_or(Value::Null, Value::Int),
                    column_datetime_precision(&column.data_type).map_or(Value::Null, Value::Int),
                    Value::Null,
                    Value::Null,
                    Value::Text(catalog_name.to_owned()),
                    Value::Text("pg_catalog".to_owned()),
                    Value::Text(column_udt_name(column)),
                    Value::Text(column.ordinal_position.to_string()),
                ]);
            }
        }
    }

    Ok(rows)
}

fn build_views_rows(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
    database_name: Option<&str>,
) -> DbResult<Vec<Vec<Value>>> {
    let catalog_name = database_name.unwrap_or("aiondb");
    let visibility = TableVisibility::for_current_session(catalog, txn_id)?;
    let mut rows = Vec::new();

    for (_, schema_id) in list_user_schemas(catalog, txn_id, default_schema)? {
        let views = catalog.list_views(txn_id, schema_id)?;
        for view in views {
            if parse_matview_sidecar(&view).is_some() {
                continue;
            }
            if !visibility.can_see_view(&view) {
                continue;
            }
            let schema_name =
                visible_schema_name(view.name.schema_name().unwrap_or("public"), default_schema);
            let metadata = analyze_view(&view, catalog, txn_id);
            rows.push(vec![
                Value::Text(catalog_name.to_owned()),
                Value::Text(schema_name),
                Value::Text(view.name.object_name().to_owned()),
                Value::Text(view.query_sql.clone()),
                Value::Text(metadata.check_option),
                Value::Text(yes_no(metadata.is_updatable)),
                Value::Text(yes_no(metadata.is_insertable_into)),
                Value::Text("NO".to_owned()),
                Value::Text("NO".to_owned()),
                Value::Text("NO".to_owned()),
            ]);
        }
    }

    Ok(rows)
}

fn build_sequences_rows(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
    database_name: Option<&str>,
) -> DbResult<Vec<Vec<Value>>> {
    let catalog_name = database_name.unwrap_or("aiondb");
    let mut rows = Vec::new();

    for (schema_name, schema_id) in list_user_schemas(catalog, txn_id, default_schema)? {
        let sequences = catalog.list_sequences(txn_id, schema_id)?;
        for sequence in sequences {
            if sequence.owned_by.is_some() {
                continue;
            }
            rows.push(vec![
                Value::Text(catalog_name.to_owned()),
                Value::Text(schema_name.clone()),
                Value::Text(sequence.name.object_name().to_owned()),
                Value::Text(pg_type_name(&sequence.data_type)),
                sequence_numeric_precision(&sequence.data_type).map_or(Value::Null, Value::Int),
                sequence_numeric_precision(&sequence.data_type)
                    .map_or(Value::Null, |_| Value::Int(2)),
                Value::Int(0),
                Value::Text(sequence.start_value.to_string()),
                Value::Text(sequence.min_value.to_string()),
                Value::Text(sequence.max_value.to_string()),
                Value::Text(sequence.increment_by.to_string()),
                Value::Text(yes_no(sequence.cycle)),
            ]);
        }
    }

    Ok(rows)
}

/// List all user tables in the public and tenant schemas.
/// When `default_schema` is a tenant schema, only that tenant's tables are returned.
fn list_user_tables(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
) -> DbResult<Vec<TableDescriptor>> {
    let mut tables = Vec::new();
    for (_, schema_id) in list_user_schemas(catalog, txn_id, default_schema)? {
        tables.extend(catalog.list_tables(txn_id, schema_id)?);
    }
    Ok(tables)
}

fn list_owned_sequences(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
) -> DbResult<Vec<SequenceDescriptor>> {
    let mut sequences = Vec::new();
    for (_, schema_id) in list_user_schemas(catalog, txn_id, default_schema)? {
        sequences.extend(
            catalog
                .list_sequences(txn_id, schema_id)?
                .into_iter()
                .filter(|sequence| sequence.owned_by.is_some()),
        );
    }
    Ok(sequences)
}

fn list_user_schemas(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
) -> DbResult<Vec<(String, SchemaId)>> {
    let tenant_filter = tenant_schema_filter(default_schema);
    let mut schemas = Vec::new();
    let mut seen = BTreeSet::new();
    for schema in catalog.list_schemas(txn_id)? {
        let lc = schema.name.to_ascii_lowercase();
        if matches!(
            lc.as_str(),
            "pg_catalog" | "information_schema" | "pg_toast"
        ) {
            continue;
        }
        if let Some(filter) = tenant_filter.as_deref() {
            if !schema.name.eq_ignore_ascii_case(filter) {
                continue;
            }
        }
        if seen.insert(schema.schema_id) {
            schemas.push((
                visible_schema_name(&schema.name, default_schema),
                schema.schema_id,
            ));
        }
    }
    Ok(schemas)
}

/// If `default_schema` starts with `tenant_`, return it as the tenant filter.
/// Otherwise return `None` (no tenant filtering).
fn tenant_schema_filter(default_schema: Option<&str>) -> Option<String> {
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

fn sequence_numeric_precision(data_type: &DataType) -> Option<i32> {
    match data_type {
        DataType::Int => Some(32),
        DataType::BigInt => Some(64),
        _ => None,
    }
}

fn numeric_raw_type_precision_scale(raw_type_name: Option<&str>) -> Option<(i32, i32)> {
    let raw = raw_type_name?.trim().to_ascii_lowercase();
    let suffix = raw.strip_prefix("numeric(")?.strip_suffix(')')?;
    if let Some((precision, scale)) = suffix.split_once(',') {
        let precision = precision.trim().parse::<i32>().ok()?;
        let scale = scale.trim().parse::<i32>().ok()?;
        return Some((precision, scale));
    }
    let precision = suffix.trim().parse::<i32>().ok()?;
    Some((precision, 0))
}

fn text_raw_type_length(raw_type_name: Option<&str>) -> Option<i32> {
    let raw = raw_type_name?.trim().to_ascii_lowercase();
    if let Some(suffix) = raw
        .strip_prefix("varchar(")
        .and_then(|s| s.strip_suffix(')'))
    {
        return suffix.trim().parse::<i32>().ok();
    }
    if let Some(suffix) = raw
        .strip_prefix("character varying(")
        .and_then(|s| s.strip_suffix(')'))
    {
        return suffix.trim().parse::<i32>().ok();
    }
    if let Some(suffix) = raw.strip_prefix("char(").and_then(|s| s.strip_suffix(')')) {
        return suffix.trim().parse::<i32>().ok();
    }
    if let Some(suffix) = raw
        .strip_prefix("character(")
        .and_then(|s| s.strip_suffix(')'))
    {
        return suffix.trim().parse::<i32>().ok();
    }
    if let Some(suffix) = raw
        .strip_prefix("bpchar(")
        .and_then(|s| s.strip_suffix(')'))
    {
        return suffix.trim().parse::<i32>().ok();
    }
    None
}

fn column_character_maximum_length(column: &ColumnDescriptor) -> Option<i32> {
    match column.text_type_modifier {
        Some(
            aiondb_core::TextTypeModifier::Char { length }
            | aiondb_core::TextTypeModifier::VarChar { length },
        ) => Some(i32::try_from(length).unwrap_or(i32::MAX)),
        _ => text_raw_type_length(column.raw_type_name.as_deref()),
    }
}

fn column_numeric_precision(column: &ColumnDescriptor) -> Option<i32> {
    if let Some((precision, _)) = numeric_raw_type_precision_scale(column.raw_type_name.as_deref())
    {
        return Some(precision);
    }
    if matches!(
        column.raw_type_name.as_deref().map(|raw| raw.trim().to_ascii_lowercase()),
        Some(raw) if matches!(raw.as_str(), "smallint" | "int2")
    ) {
        return Some(16);
    }
    match &column.data_type {
        DataType::Int => Some(32),
        DataType::BigInt => Some(64),
        DataType::Real => Some(24),
        DataType::Double => Some(53),
        _ => None,
    }
}

fn column_numeric_precision_radix(column: &ColumnDescriptor) -> Option<i32> {
    if numeric_raw_type_precision_scale(column.raw_type_name.as_deref()).is_some() {
        return Some(10);
    }
    if matches!(
        column.raw_type_name.as_deref().map(|raw| raw.trim().to_ascii_lowercase()),
        Some(raw) if matches!(raw.as_str(), "smallint" | "int2")
    ) {
        return Some(2);
    }
    match &column.data_type {
        DataType::Int | DataType::BigInt | DataType::Real | DataType::Double => Some(2),
        _ => None,
    }
}

fn column_numeric_scale(column: &ColumnDescriptor) -> Option<i32> {
    if let Some((_, scale)) = numeric_raw_type_precision_scale(column.raw_type_name.as_deref()) {
        return Some(scale);
    }
    match &column.data_type {
        DataType::Int | DataType::BigInt => Some(0),
        _ => None,
    }
}

fn column_datetime_precision(data_type: &DataType) -> Option<i32> {
    match data_type {
        DataType::Time | DataType::TimeTz | DataType::Timestamp | DataType::TimestampTz => Some(6),
        _ => None,
    }
}

fn reflected_raw_udt_name(raw_type_name: &str) -> Option<&'static str> {
    match raw_type_name.trim().to_ascii_lowercase().as_str() {
        "smallint" | "int2" => Some("int2"),
        "smallint[]" | "int2[]" => Some("_int2"),
        "integer[]" | "int4[]" => Some("_int4"),
        "bigint[]" | "int8[]" => Some("_int8"),
        "boolean[]" | "bool[]" => Some("_bool"),
        "text[]" => Some("_text"),
        "uuid[]" => Some("_uuid"),
        "jsonb[]" => Some("_jsonb"),
        "varchar" | "character varying" => Some("varchar"),
        "varchar[]" | "character varying[]" => Some("_varchar"),
        "\"char\"" | "char" | "character" | "bpchar" => Some("bpchar"),
        "\"char\"[]" | "char[]" | "character[]" | "bpchar[]" => Some("_bpchar"),
        _ => None,
    }
}

fn reflected_raw_data_type_name(raw_type_name: &str) -> Option<&'static str> {
    match raw_type_name.trim().to_ascii_lowercase().as_str() {
        "smallint" | "int2" => Some("smallint"),
        "smallint[]"
        | "int2[]"
        | "integer[]"
        | "int4[]"
        | "bigint[]"
        | "int8[]"
        | "boolean[]"
        | "bool[]"
        | "text[]"
        | "uuid[]"
        | "jsonb[]"
        | "varchar[]"
        | "character varying[]"
        | "\"char\"[]"
        | "char[]"
        | "character[]"
        | "bpchar[]" => Some("ARRAY"),
        "varchar" | "character varying" => Some("character varying"),
        "\"char\"" | "char" | "character" | "bpchar" => Some("character"),
        _ => None,
    }
}

fn column_udt_name(column: &ColumnDescriptor) -> String {
    if let Some(raw_type_name) = column.raw_type_name.as_deref() {
        if let Some(name) = reflected_raw_udt_name(raw_type_name) {
            return name.to_owned();
        }
    }
    match &column.data_type {
        DataType::Int => "int4".to_owned(),
        DataType::BigInt => "int8".to_owned(),
        DataType::Real => "float4".to_owned(),
        DataType::Double => "float8".to_owned(),
        DataType::Numeric => "numeric".to_owned(),
        DataType::Money => "money".to_owned(),
        DataType::Text => "text".to_owned(),
        DataType::Boolean => "bool".to_owned(),
        DataType::Blob => "bytea".to_owned(),
        DataType::Timestamp => "timestamp".to_owned(),
        DataType::Date => "date".to_owned(),
        DataType::Time => "time".to_owned(),
        DataType::TimeTz => "timetz".to_owned(),
        DataType::Interval => "interval".to_owned(),
        DataType::Tid => "tid".to_owned(),
        DataType::Uuid => "uuid".to_owned(),
        DataType::TimestampTz => "timestamptz".to_owned(),
        DataType::PgLsn => "pg_lsn".to_owned(),
        DataType::Jsonb => "jsonb".to_owned(),
        DataType::MacAddr => "macaddr".to_owned(),
        DataType::MacAddr8 => "macaddr8".to_owned(),
        DataType::Vector { .. } => "vector".to_owned(),
        DataType::Array(inner) => format!(
            "_{}",
            column_udt_name(&ColumnDescriptor {
                column_id: aiondb_core::ColumnId::default(),
                name: String::new(),
                data_type: inner.as_ref().clone(),
                raw_type_name: None,
                text_type_modifier: None,
                nullable: true,
                ordinal_position: 0,
                default_value: None,
            })
        ),
    }
}

fn column_info_schema_data_type(column: &ColumnDescriptor) -> String {
    if let Some(raw_type_name) = column.raw_type_name.as_deref() {
        if let Some(name) = reflected_raw_data_type_name(raw_type_name) {
            return name.to_owned();
        }
    }
    pg_type_name(&column.data_type)
}

#[derive(Debug, Clone)]
struct ViewMetadata {
    check_option: String,
    is_updatable: bool,
    is_insertable_into: bool,
    is_deletable: bool,
    column_is_updatable: Vec<bool>,
}

fn analyze_view(
    view: &ViewDescriptor,
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
) -> ViewMetadata {
    analyze_view_inner(view, catalog, txn_id, 0)
}

fn analyze_view_inner(
    view: &ViewDescriptor,
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    depth: usize,
) -> ViewMetadata {
    let Some(select) = parse_view_select(&view.query_sql) else {
        let insertable = view_has_instead_of_trigger(
            catalog.as_ref(),
            txn_id,
            view,
            TriggerEventDescriptor::Insert,
        );
        let updatable = view_has_instead_of_trigger(
            catalog.as_ref(),
            txn_id,
            view,
            TriggerEventDescriptor::Update,
        );
        let deletable = view_has_instead_of_trigger(
            catalog.as_ref(),
            txn_id,
            view,
            TriggerEventDescriptor::Delete,
        );
        return ViewMetadata {
            check_option: view_check_option_text(view),
            is_updatable: updatable,
            is_insertable_into: insertable,
            is_deletable: deletable,
            column_is_updatable: vec![false; view.columns.len()],
        };
    };

    let mut is_simple = is_simple_updatable_view(&select) && depth <= 64;
    if is_simple {
        for item in &select.items {
            if expr_has_aggregate(&item.expr)
                || expr_has_window(&item.expr)
                || expr_has_srf(&item.expr)
            {
                is_simple = false;
                break;
            }
        }
    }
    if is_simple {
        if let Some(from) = &select.from {
            if let Ok(candidates) =
                crate::binder::views::view_relation_lookup_candidates(view, from)
            {
                let mut found_relation = false;
                for table_name in candidates {
                    if catalog
                        .get_table(txn_id, &table_name)
                        .ok()
                        .flatten()
                        .is_some()
                    {
                        found_relation = true;
                        break;
                    }
                    if let Ok(Some(inner_view)) = catalog.get_view(txn_id, &table_name) {
                        let inner_meta =
                            analyze_view_inner(&inner_view, catalog, txn_id, depth + 1);
                        if !inner_meta.is_updatable
                            || !inner_meta.is_insertable_into
                            || !inner_meta.is_deletable
                        {
                            is_simple = false;
                        }
                        found_relation = true;
                        break;
                    }
                }
                if !found_relation {
                    is_simple = false;
                }
            } else {
                is_simple = false;
            }
        } else {
            is_simple = false;
        }
    }

    let mut column_is_updatable = vec![false; view.columns.len()];
    let mut is_insertable_into = is_simple;
    let mut is_updatable = is_simple;
    let mut is_deletable = is_simple;
    if is_simple {
        if select.items.len() == 1 && is_star_expr(&select.items[0].expr) {
            column_is_updatable.fill(true);
        } else {
            for (index, item) in select.items.iter().enumerate() {
                if index >= column_is_updatable.len() {
                    break;
                }
                column_is_updatable[index] = matches!(&item.expr, Expr::Identifier(name)
                    if name.parts.last().is_some_and(|column| !is_system_column_name(column)));
            }
        }
    } else {
        is_insertable_into = view_has_instead_of_trigger(
            catalog.as_ref(),
            txn_id,
            view,
            TriggerEventDescriptor::Insert,
        );
        is_updatable = view_has_instead_of_trigger(
            catalog.as_ref(),
            txn_id,
            view,
            TriggerEventDescriptor::Update,
        );
        is_deletable = view_has_instead_of_trigger(
            catalog.as_ref(),
            txn_id,
            view,
            TriggerEventDescriptor::Delete,
        );
    }

    ViewMetadata {
        check_option: view_check_option_text(view),
        is_updatable,
        is_insertable_into,
        is_deletable,
        column_is_updatable,
    }
}

fn view_check_option_text(view: &ViewDescriptor) -> String {
    match view_check_option_mode(view) {
        ViewCheckOptionMode::None => "NONE".to_owned(),
        ViewCheckOptionMode::Local => "LOCAL".to_owned(),
        ViewCheckOptionMode::Cascaded => "CASCADED".to_owned(),
    }
}

fn view_has_instead_of_trigger(
    catalog: &dyn CatalogReader,
    txn_id: TxnId,
    view: &ViewDescriptor,
    required_event: TriggerEventDescriptor,
) -> bool {
    let trigger_target = view.name.to_string();
    let mut triggers = catalog
        .list_triggers(txn_id, &trigger_target)
        .unwrap_or_default();
    if triggers.is_empty() {
        triggers = catalog
            .list_triggers(txn_id, view.name.object_name())
            .unwrap_or_default();
    }
    triggers.iter().any(|trigger| {
        trigger.timing == TriggerTimingDescriptor::InsteadOf && trigger.event == required_event
    })
}

fn expr_has_aggregate(expr: &Expr) -> bool {
    match expr {
        Expr::FunctionCall { name, .. } => {
            let func = name
                .parts
                .last()
                .map(|s| s.to_ascii_lowercase())
                .unwrap_or_default();
            matches!(
                func.as_str(),
                "count"
                    | "sum"
                    | "avg"
                    | "min"
                    | "max"
                    | "array_agg"
                    | "string_agg"
                    | "bool_and"
                    | "bool_or"
                    | "every"
                    | "bit_and"
                    | "bit_or"
                    | "xmlagg"
            )
        }
        _ => false,
    }
}

fn expr_has_window(expr: &Expr) -> bool {
    matches!(expr, Expr::WindowFunction { .. })
}

fn expr_has_srf(expr: &Expr) -> bool {
    match expr {
        Expr::FunctionCall { name, .. } => {
            let func = name
                .parts
                .last()
                .map(|s| s.to_ascii_lowercase())
                .unwrap_or_default();
            matches!(
                func.as_str(),
                "generate_series"
                    | "generate_subscripts"
                    | "unnest"
                    | "json_array_elements"
                    | "jsonb_array_elements"
                    | "json_each"
                    | "jsonb_each"
                    | "regexp_matches"
                    | "regexp_split_to_table"
                    | "string_to_table"
            )
        }
        _ => false,
    }
}

fn parse_view_select(query_sql: &str) -> Option<SelectStatement> {
    let statements = aiondb_parser::parse_sql(query_sql).ok()?;
    let statement = statements.into_iter().next()?;
    let Statement::Select(select) = statement else {
        return None;
    };
    Some(select)
}

fn is_simple_updatable_view(select: &SelectStatement) -> bool {
    select.from.is_some()
        && select.ctes.is_empty()
        && select.joins.is_empty()
        && select.group_by.is_empty()
        && select.having.is_none()
        && matches!(select.distinct, aiondb_parser::DistinctKind::All)
        && select.limit.is_none()
        && select.offset.is_none()
}

fn yes_no(value: bool) -> String {
    if value {
        "YES".to_owned()
    } else {
        "NO".to_owned()
    }
}

#[derive(Debug, Clone)]
struct ColumnIdentityMetadata {
    is_identity: bool,
    identity_generation: Option<String>,
    identity_start: Option<String>,
    identity_increment: Option<String>,
    identity_maximum: Option<String>,
    identity_minimum: Option<String>,
    identity_cycle: String,
}

impl ColumnIdentityMetadata {
    fn plain_column() -> Self {
        Self {
            is_identity: false,
            identity_generation: None,
            identity_start: None,
            identity_increment: None,
            identity_maximum: None,
            identity_minimum: None,
            identity_cycle: "NO".to_owned(),
        }
    }

    fn column_default_value(&self, default_value: Option<&str>) -> Value {
        if self.is_identity {
            return Value::Null;
        }
        default_value.map_or_else(
            || Value::Null,
            |value| Value::Text(normalize_reflected_column_default(value)),
        )
    }
}

fn normalize_reflected_column_default(default_value: &str) -> String {
    let trimmed = default_value.trim();
    if let Some(sequence_name) = trimmed
        .strip_prefix("nextval('")
        .and_then(|rest| rest.strip_suffix("')"))
    {
        return format!("nextval('{sequence_name}'::regclass)");
    }

    match default_value.trim().to_ascii_lowercase().as_str() {
        "false" => "false".to_owned(),
        "true" => "true".to_owned(),
        "current_timestamp" => "CURRENT_TIMESTAMP".to_owned(),
        "current_date" => "CURRENT_DATE".to_owned(),
        "current_time" => "CURRENT_TIME".to_owned(),
        "localtimestamp" => "LOCALTIMESTAMP".to_owned(),
        "localtime" => "LOCALTIME".to_owned(),
        "current_user" => "CURRENT_USER".to_owned(),
        "session_user" => "SESSION_USER".to_owned(),
        "current_role" => "CURRENT_ROLE".to_owned(),
        "current_catalog" => "CURRENT_CATALOG".to_owned(),
        "current_schema" => "CURRENT_SCHEMA".to_owned(),
        "user" => "USER".to_owned(),
        _ => default_value.to_owned(),
    }
}

fn column_identity_metadata(
    table: &TableDescriptor,
    column: &aiondb_catalog::ColumnDescriptor,
    owned_sequences: &[SequenceDescriptor],
) -> ColumnIdentityMetadata {
    let Some(identity_column) = table.identity_column(column.ordinal_position) else {
        return ColumnIdentityMetadata::plain_column();
    };
    if identity_column.implicit_serial
        || column_uses_serial_type_sugar(column.raw_type_name.as_deref())
    {
        return ColumnIdentityMetadata::plain_column();
    }
    let sequence = find_identity_sequence_for_column(table, column, owned_sequences);
    ColumnIdentityMetadata {
        is_identity: true,
        identity_generation: Some(identity_column.generation.as_sql().to_owned()),
        identity_start: sequence.map(|value| value.start_value.to_string()),
        identity_increment: sequence.map(|value| value.increment_by.to_string()),
        identity_maximum: sequence.map(|value| value.max_value.to_string()),
        identity_minimum: sequence.map(|value| value.min_value.to_string()),
        identity_cycle: sequence.map_or_else(|| "NO".to_owned(), |value| yes_no(value.cycle)),
    }
}

fn column_uses_serial_type_sugar(raw_type_name: Option<&str>) -> bool {
    matches!(
        raw_type_name
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("serial" | "serial2" | "serial4" | "serial8" | "smallserial" | "bigserial")
    )
}

fn find_identity_sequence_for_column<'a>(
    table: &TableDescriptor,
    column: &aiondb_catalog::ColumnDescriptor,
    owned_sequences: &'a [SequenceDescriptor],
) -> Option<&'a SequenceDescriptor> {
    owned_sequences
        .iter()
        .find(|sequence| sequence.owned_by == Some((table.table_id, column.column_id)))
}

/// Map an `AionDB` `DataType` to the PostgreSQL-compatible type name string
/// used in `information_schema.columns.data_type`.
fn pg_type_name(dt: &DataType) -> String {
    match dt {
        DataType::Vector {
            dims: 0,
            element_type: aiondb_core::VectorElementType::Float16,
        } => "halfvec".to_owned(),
        DataType::Vector {
            dims,
            element_type: aiondb_core::VectorElementType::Float16,
        } => format!("halfvec({dims})"),
        DataType::Vector {
            dims: 0,
            element_type: aiondb_core::VectorElementType::Float32,
        } => "vector".to_owned(),
        DataType::Vector {
            dims,
            element_type: aiondb_core::VectorElementType::Float32,
        } => format!("vector({dims})"),
        DataType::Vector { dims, element_type } => format!("vector({dims}, {element_type})"),
        DataType::Array(_) => "ARRAY".to_owned(),
        other => other.pg_type_name().to_owned(),
    }
}

#[cfg(test)]
mod tests;
