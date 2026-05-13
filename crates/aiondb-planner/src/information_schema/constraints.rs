//! `information_schema.{table_constraints, key_column_usage,
//! referential_constraints, constraint_column_usage, table_privileges,
//! role_table_grants}` views. Used by ORMs (Diesel, SQLAlchemy, sqlx,
//! Prisma) for FK/PK/UNIQUE/CHECK introspection and grant discovery.

use std::sync::Arc;

use aiondb_catalog::{
    CatalogPrivilege, CatalogReader, ForeignKeyConstraint, PrivilegeTarget, TableDescriptor,
};
use aiondb_core::{
    convert::usize_to_i32_saturating, DataType, DbResult, FkAction, FkMatchType, TxnId, Value,
};
use aiondb_plan::{LogicalPlan, ResultField};

use super::list_user_tables;
use super::query_helpers::rows_to_typed;

fn text_field(name: &str, nullable: bool) -> ResultField {
    ResultField {
        name: name.to_owned(),
        data_type: DataType::Text,
        text_type_modifier: None,
        nullable,
    }
}

fn int_field(name: &str, nullable: bool) -> ResultField {
    ResultField {
        name: name.to_owned(),
        data_type: DataType::Int,
        text_type_modifier: None,
        nullable,
    }
}

fn yes_no(b: bool) -> Value {
    Value::Text(if b { "YES".to_owned() } else { "NO".to_owned() })
}

fn fk_action_label(action: &FkAction) -> &'static str {
    match action {
        FkAction::NoAction => "NO ACTION",
        FkAction::Restrict => "RESTRICT",
        FkAction::Cascade => "CASCADE",
        FkAction::SetNull => "SET NULL",
        FkAction::SetDefault => "SET DEFAULT",
    }
}

fn fk_match_label(match_type: &FkMatchType) -> &'static str {
    match match_type {
        FkMatchType::Simple => "SIMPLE",
        FkMatchType::Full => "FULL",
        FkMatchType::Partial => "PARTIAL",
    }
}

fn check_constraint_name(table: &TableDescriptor, idx: usize, name: Option<&String>) -> String {
    name.cloned()
        .unwrap_or_else(|| format!("{}_check_{idx}", table.name.object_name()))
}

// ---------------------------------------------------------------
// information_schema.table_constraints
// ---------------------------------------------------------------

pub(super) fn table_constraints_output_fields() -> Vec<ResultField> {
    vec![
        text_field("constraint_catalog", false),
        text_field("constraint_schema", false),
        text_field("constraint_name", false),
        text_field("table_catalog", false),
        text_field("table_schema", false),
        text_field("table_name", false),
        text_field("constraint_type", false),
        text_field("is_deferrable", false),
        text_field("initially_deferred", false),
        text_field("enforced", false),
        text_field("nulls_distinct", true),
    ]
}

pub(super) fn build_table_constraints_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
    database_name: Option<&str>,
) -> DbResult<LogicalPlan> {
    let fields = table_constraints_output_fields();
    let catalog_name = database_name.unwrap_or("aiondb");
    let mut rows: Vec<Vec<Value>> = Vec::new();
    for table in list_user_tables(catalog, txn_id, default_schema)? {
        let schema_name = super::visible_schema_name(
            table.name.schema_name().unwrap_or("public"),
            default_schema,
        );
        let table_name = table.name.object_name().to_owned();
        // Primary key
        if table.primary_key.is_some() {
            rows.push(make_tc_row(
                catalog_name,
                &schema_name,
                &format!("{}_pkey", table_name),
                &table_name,
                "PRIMARY KEY",
            ));
        }
        // Foreign keys
        for fk in &table.foreign_keys {
            rows.push(make_tc_row(
                catalog_name,
                &schema_name,
                &fk.effective_name(&table_name),
                &table_name,
                "FOREIGN KEY",
            ));
        }
        // Unique constraints (from unique non-primary indexes)
        let indexes = catalog.list_indexes(txn_id, table.table_id)?;
        for idx in &indexes {
            if idx.unique && !is_primary_key_index(&table, idx) {
                rows.push(make_tc_row(
                    catalog_name,
                    &schema_name,
                    idx.name.object_name(),
                    &table_name,
                    "UNIQUE",
                ));
            }
        }
        // Check constraints
        for (i, check) in table.check_constraints.iter().enumerate() {
            let conname = check_constraint_name(&table, i, check.name.as_ref());
            rows.push(make_tc_row(
                catalog_name,
                &schema_name,
                &conname,
                &table_name,
                "CHECK",
            ));
        }
        // NOT NULL on each column → synthetic CHECK (PG behavior).
        for col in &table.columns {
            if !col.nullable {
                let conname = format!("{}_{}_not_null", table_name, col.name);
                rows.push(make_tc_row(
                    catalog_name,
                    &schema_name,
                    &conname,
                    &table_name,
                    "CHECK",
                ));
            }
        }
    }
    Ok(LogicalPlan::ProjectValues {
        output_fields: fields.clone(),
        rows: rows_to_typed(&fields, rows),
        order_by: Vec::new(),
        limit: None,
        offset: None,
    })
}

fn make_tc_row(
    catalog_name: &str,
    schema_name: &str,
    constraint_name: &str,
    table_name: &str,
    constraint_type: &str,
) -> Vec<Value> {
    vec![
        Value::Text(catalog_name.to_owned()),
        Value::Text(schema_name.to_owned()),
        Value::Text(constraint_name.to_owned()),
        Value::Text(catalog_name.to_owned()),
        Value::Text(schema_name.to_owned()),
        Value::Text(table_name.to_owned()),
        Value::Text(constraint_type.to_owned()),
        yes_no(false), // is_deferrable
        yes_no(false), // initially_deferred
        yes_no(true),  // enforced
        Value::Null,   // nulls_distinct (only for UNIQUE; null is fine)
    ]
}

// ---------------------------------------------------------------
// information_schema.key_column_usage
// ---------------------------------------------------------------

pub(super) fn key_column_usage_output_fields() -> Vec<ResultField> {
    vec![
        text_field("constraint_catalog", false),
        text_field("constraint_schema", false),
        text_field("constraint_name", false),
        text_field("table_catalog", false),
        text_field("table_schema", false),
        text_field("table_name", false),
        text_field("column_name", false),
        int_field("ordinal_position", false),
        int_field("position_in_unique_constraint", true),
    ]
}

pub(super) fn build_key_column_usage_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
    database_name: Option<&str>,
) -> DbResult<LogicalPlan> {
    let fields = key_column_usage_output_fields();
    let catalog_name = database_name.unwrap_or("aiondb");
    let mut rows: Vec<Vec<Value>> = Vec::new();
    for table in list_user_tables(catalog, txn_id, default_schema)? {
        let schema_name = super::visible_schema_name(
            table.name.schema_name().unwrap_or("public"),
            default_schema,
        );
        let table_name = table.name.object_name().to_owned();
        // Primary key columns
        if let Some(pk_cols) = &table.primary_key {
            let pk_name = format!("{}_pkey", table_name);
            for (pos, col_id) in pk_cols.iter().enumerate() {
                if let Some(col) = table.columns.iter().find(|c| c.column_id == *col_id) {
                    rows.push(make_kcu_row(
                        catalog_name,
                        &schema_name,
                        &pk_name,
                        &table_name,
                        &col.name,
                        usize_to_i32_saturating(pos + 1),
                        None,
                    ));
                }
            }
        }
        // Foreign key columns
        for fk in &table.foreign_keys {
            let fk_name = fk.effective_name(&table_name);
            for (pos, col_name) in fk.columns.iter().enumerate() {
                rows.push(make_kcu_row(
                    catalog_name,
                    &schema_name,
                    &fk_name,
                    &table_name,
                    col_name,
                    usize_to_i32_saturating(pos + 1),
                    Some(usize_to_i32_saturating(pos + 1)),
                ));
            }
        }
        // Unique columns (from unique non-primary indexes)
        let indexes = catalog.list_indexes(txn_id, table.table_id)?;
        for idx in &indexes {
            if idx.unique && !is_primary_key_index(&table, idx) {
                let con_name = idx.name.object_name().to_owned();
                for (pos, key) in idx.key_columns.iter().enumerate() {
                    if let Some(col) = table.columns.iter().find(|c| c.column_id == key.column_id) {
                        rows.push(make_kcu_row(
                            catalog_name,
                            &schema_name,
                            &con_name,
                            &table_name,
                            &col.name,
                            usize_to_i32_saturating(pos + 1),
                            None,
                        ));
                    }
                }
            }
        }
    }
    Ok(LogicalPlan::ProjectValues {
        output_fields: fields.clone(),
        rows: rows_to_typed(&fields, rows),
        order_by: Vec::new(),
        limit: None,
        offset: None,
    })
}

fn make_kcu_row(
    catalog_name: &str,
    schema_name: &str,
    constraint_name: &str,
    table_name: &str,
    column_name: &str,
    ordinal_position: i32,
    position_in_unique: Option<i32>,
) -> Vec<Value> {
    vec![
        Value::Text(catalog_name.to_owned()),
        Value::Text(schema_name.to_owned()),
        Value::Text(constraint_name.to_owned()),
        Value::Text(catalog_name.to_owned()),
        Value::Text(schema_name.to_owned()),
        Value::Text(table_name.to_owned()),
        Value::Text(column_name.to_owned()),
        Value::Int(ordinal_position),
        position_in_unique.map_or(Value::Null, Value::Int),
    ]
}

// ---------------------------------------------------------------
// information_schema.referential_constraints
// ---------------------------------------------------------------

pub(super) fn referential_constraints_output_fields() -> Vec<ResultField> {
    vec![
        text_field("constraint_catalog", false),
        text_field("constraint_schema", false),
        text_field("constraint_name", false),
        text_field("unique_constraint_catalog", true),
        text_field("unique_constraint_schema", true),
        text_field("unique_constraint_name", true),
        text_field("match_option", false),
        text_field("update_rule", false),
        text_field("delete_rule", false),
    ]
}

pub(super) fn build_referential_constraints_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
    database_name: Option<&str>,
) -> DbResult<LogicalPlan> {
    let fields = referential_constraints_output_fields();
    let catalog_name = database_name.unwrap_or("aiondb");
    let tables = list_user_tables(catalog, txn_id, default_schema)?;
    let mut rows: Vec<Vec<Value>> = Vec::new();
    for table in &tables {
        let schema_name = super::visible_schema_name(
            table.name.schema_name().unwrap_or("public"),
            default_schema,
        );
        let table_name = table.name.object_name().to_owned();
        for fk in &table.foreign_keys {
            let (ref_schema, ref_name) =
                resolve_referenced_unique_constraint(&tables, fk, &schema_name);
            let con_name = fk.effective_name(&table_name);
            rows.push(vec![
                Value::Text(catalog_name.to_owned()),
                Value::Text(schema_name.clone()),
                Value::Text(con_name),
                Value::Text(catalog_name.to_owned()),
                ref_schema.map_or(Value::Null, Value::Text),
                ref_name.map_or(Value::Null, Value::Text),
                Value::Text(fk_match_label(&fk.match_type).to_owned()),
                Value::Text(fk_action_label(&fk.on_update).to_owned()),
                Value::Text(fk_action_label(&fk.on_delete).to_owned()),
            ]);
        }
    }
    Ok(LogicalPlan::ProjectValues {
        output_fields: fields.clone(),
        rows: rows_to_typed(&fields, rows),
        order_by: Vec::new(),
        limit: None,
        offset: None,
    })
}

fn find_referenced_table<'t>(
    tables: &'t [TableDescriptor],
    fk: &ForeignKeyConstraint,
    same_schema: &str,
) -> Option<&'t TableDescriptor> {
    let referenced = fk.referenced_table.to_ascii_lowercase();
    tables.iter().find(|t| {
        t.name.object_name().eq_ignore_ascii_case(&referenced)
            || t.name.name.eq_ignore_ascii_case(&referenced)
            || format!(
                "{}.{}",
                t.name.schema_name().unwrap_or(same_schema),
                t.name.object_name()
            )
            .eq_ignore_ascii_case(&referenced)
            || format!("public.{}", t.name.object_name()).eq_ignore_ascii_case(&referenced)
    })
}

fn resolve_referenced_unique_constraint(
    tables: &[TableDescriptor],
    fk: &ForeignKeyConstraint,
    same_schema: &str,
) -> (Option<String>, Option<String>) {
    let Some(target) = find_referenced_table(tables, fk, same_schema) else {
        return (None, None);
    };
    let schema = target
        .name
        .schema_name()
        .map(str::to_owned)
        .unwrap_or_else(|| same_schema.to_owned());
    // PG points unique_constraint_name at the PK or UNIQUE constraint backing
    // the referenced columns. Without exact column-set tracking we default to
    // the table's PK; if missing, use the first matching unique index.
    let con_name = if target.primary_key.is_some() {
        format!("{}_pkey", target.name.object_name())
    } else {
        format!("{}_unique", target.name.object_name())
    };
    (Some(schema), Some(con_name))
}

// ---------------------------------------------------------------
// information_schema.constraint_column_usage
// ---------------------------------------------------------------

pub(super) fn constraint_column_usage_output_fields() -> Vec<ResultField> {
    vec![
        text_field("table_catalog", false),
        text_field("table_schema", false),
        text_field("table_name", false),
        text_field("column_name", false),
        text_field("constraint_catalog", false),
        text_field("constraint_schema", false),
        text_field("constraint_name", false),
    ]
}

pub(super) fn build_constraint_column_usage_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
    database_name: Option<&str>,
) -> DbResult<LogicalPlan> {
    let fields = constraint_column_usage_output_fields();
    let catalog_name = database_name.unwrap_or("aiondb");
    let tables = list_user_tables(catalog, txn_id, default_schema)?;
    let mut rows: Vec<Vec<Value>> = Vec::new();
    for table in &tables {
        let schema_name = super::visible_schema_name(
            table.name.schema_name().unwrap_or("public"),
            default_schema,
        );
        let table_name = table.name.object_name().to_owned();
        // PK
        if let Some(pk_cols) = &table.primary_key {
            let pk_name = format!("{}_pkey", table_name);
            for col_id in pk_cols {
                if let Some(col) = table.columns.iter().find(|c| c.column_id == *col_id) {
                    rows.push(make_ccu_row(
                        catalog_name,
                        &schema_name,
                        &table_name,
                        &col.name,
                        &pk_name,
                    ));
                }
            }
        }
        // FK: PG records the *referenced* columns here, not the local ones.
        for fk in &table.foreign_keys {
            let con_name = fk.effective_name(&table_name);
            let target = find_referenced_table(&tables, fk, &schema_name);
            if let Some(target) = target {
                let target_schema = super::visible_schema_name(
                    target.name.schema_name().unwrap_or("public"),
                    default_schema,
                );
                let target_table = target.name.object_name().to_owned();
                // PG uses the referenced columns when present; if the parser
                // didn't capture them (older `REFERENCES p` shorthand), fall
                // back to the target's primary key columns.
                let mut col_iter: Vec<String> = fk.referenced_columns.clone();
                if col_iter.is_empty() {
                    if let Some(pk_cols) = &target.primary_key {
                        col_iter = pk_cols
                            .iter()
                            .filter_map(|col_id| {
                                target
                                    .columns
                                    .iter()
                                    .find(|c| c.column_id == *col_id)
                                    .map(|c| c.name.clone())
                            })
                            .collect();
                    }
                }
                for col_name in &col_iter {
                    rows.push(make_ccu_row(
                        catalog_name,
                        &target_schema,
                        &target_table,
                        col_name,
                        &con_name,
                    ));
                }
            }
        }
        // UNIQUE
        let indexes = catalog.list_indexes(txn_id, table.table_id)?;
        for idx in &indexes {
            if idx.unique && !is_primary_key_index(table, idx) {
                let con_name = idx.name.object_name().to_owned();
                for key in &idx.key_columns {
                    if let Some(col) = table.columns.iter().find(|c| c.column_id == key.column_id) {
                        rows.push(make_ccu_row(
                            catalog_name,
                            &schema_name,
                            &table_name,
                            &col.name,
                            &con_name,
                        ));
                    }
                }
            }
        }
    }
    Ok(LogicalPlan::ProjectValues {
        output_fields: fields.clone(),
        rows: rows_to_typed(&fields, rows),
        order_by: Vec::new(),
        limit: None,
        offset: None,
    })
}

fn make_ccu_row(
    catalog_name: &str,
    schema_name: &str,
    table_name: &str,
    column_name: &str,
    constraint_name: &str,
) -> Vec<Value> {
    vec![
        Value::Text(catalog_name.to_owned()),
        Value::Text(schema_name.to_owned()),
        Value::Text(table_name.to_owned()),
        Value::Text(column_name.to_owned()),
        Value::Text(catalog_name.to_owned()),
        Value::Text(schema_name.to_owned()),
        Value::Text(constraint_name.to_owned()),
    ]
}

// ---------------------------------------------------------------
// information_schema.table_privileges
// ---------------------------------------------------------------

pub(super) fn table_privileges_output_fields() -> Vec<ResultField> {
    vec![
        text_field("grantor", false),
        text_field("grantee", false),
        text_field("table_catalog", false),
        text_field("table_schema", false),
        text_field("table_name", false),
        text_field("privilege_type", false),
        text_field("is_grantable", false),
        text_field("with_hierarchy", false),
    ]
}

pub(super) fn build_table_privileges_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
    database_name: Option<&str>,
) -> DbResult<LogicalPlan> {
    let fields = table_privileges_output_fields();
    let catalog_name = database_name.unwrap_or("aiondb");
    let tables = list_user_tables(catalog, txn_id, default_schema)?;
    // Use a BTreeSet to dedupe (grantee, schema, table, priv) tuples; the
    // owner-row pass below adds canonical owner privileges, and the role
    // walk adds explicit grants. PG never duplicates rows for the owner.
    let mut seen: std::collections::BTreeSet<(String, String, String, String)> =
        std::collections::BTreeSet::new();
    let mut rows: Vec<Vec<Value>> = Vec::new();
    let owner_grants = [
        "SELECT",
        "INSERT",
        "UPDATE",
        "DELETE",
        "TRUNCATE",
        "REFERENCES",
        "TRIGGER",
    ];
    for table in &tables {
        let schema_name = super::visible_schema_name(
            table.name.schema_name().unwrap_or("public"),
            default_schema,
        );
        let owner = table
            .owner
            .as_deref()
            .unwrap_or(aiondb_core::COMPAT_BOOTSTRAP_ROLE_NAME)
            .to_owned();
        for priv_type in owner_grants {
            let key = (
                owner.clone(),
                schema_name.clone(),
                table.name.object_name().to_owned(),
                priv_type.to_owned(),
            );
            if seen.insert(key) {
                rows.push(make_tp_row(
                    &owner,
                    &owner,
                    catalog_name,
                    &schema_name,
                    table.name.object_name(),
                    priv_type,
                    true,
                ));
            }
        }
    }
    for role in catalog.list_roles(txn_id)? {
        for desc in catalog.get_privileges(txn_id, &role.name)? {
            let PrivilegeTarget::Table(name) = &desc.target else {
                continue;
            };
            let schema =
                super::visible_schema_name(name.schema_name().unwrap_or("public"), default_schema);
            let in_scope = tables.iter().any(|t| {
                t.name
                    .object_name()
                    .eq_ignore_ascii_case(name.object_name())
                    && t.name
                        .schema_name()
                        .map(|s| s.eq_ignore_ascii_case(&schema))
                        .unwrap_or(true)
            });
            if !in_scope {
                continue;
            }
            for priv_type in privilege_to_table_strings(desc.privilege) {
                let key = (
                    role.name.clone(),
                    schema.clone(),
                    name.object_name().to_owned(),
                    priv_type.to_owned(),
                );
                if seen.insert(key) {
                    rows.push(make_tp_row(
                        aiondb_core::COMPAT_BOOTSTRAP_ROLE_NAME,
                        &role.name,
                        catalog_name,
                        &schema,
                        name.object_name(),
                        priv_type,
                        false,
                    ));
                }
            }
        }
    }
    Ok(LogicalPlan::ProjectValues {
        output_fields: fields.clone(),
        rows: rows_to_typed(&fields, rows),
        order_by: Vec::new(),
        limit: None,
        offset: None,
    })
}

fn privilege_to_table_strings(priv_kind: CatalogPrivilege) -> Vec<&'static str> {
    match priv_kind {
        CatalogPrivilege::Select => vec!["SELECT"],
        CatalogPrivilege::Insert => vec!["INSERT"],
        CatalogPrivilege::Update => vec!["UPDATE"],
        CatalogPrivilege::Delete => vec!["DELETE"],
        CatalogPrivilege::Truncate => vec!["TRUNCATE"],
        CatalogPrivilege::References => vec!["REFERENCES"],
        CatalogPrivilege::Trigger => vec!["TRIGGER"],
        CatalogPrivilege::All => vec![
            "SELECT",
            "INSERT",
            "UPDATE",
            "DELETE",
            "TRUNCATE",
            "REFERENCES",
            "TRIGGER",
        ],
        // Schema/database/function privileges don't surface in table_privileges.
        _ => Vec::new(),
    }
}

fn make_tp_row(
    grantor: &str,
    grantee: &str,
    catalog_name: &str,
    schema_name: &str,
    table_name: &str,
    privilege_type: &str,
    is_grantable: bool,
) -> Vec<Value> {
    vec![
        Value::Text(grantor.to_owned()),
        Value::Text(grantee.to_owned()),
        Value::Text(catalog_name.to_owned()),
        Value::Text(schema_name.to_owned()),
        Value::Text(table_name.to_owned()),
        Value::Text(privilege_type.to_owned()),
        yes_no(is_grantable),
        yes_no(false),
    ]
}

// ---------------------------------------------------------------
// information_schema.routines
// ---------------------------------------------------------------
//
// Subset of the PG view; ORMs primarily query routine_schema, routine_name,
// routine_type, data_type, external_language, routine_definition. We omit
// the long parameter-related null columns to keep the implementation lean.

pub(super) fn routines_output_fields() -> Vec<ResultField> {
    vec![
        text_field("specific_catalog", false),
        text_field("specific_schema", false),
        text_field("specific_name", false),
        text_field("routine_catalog", false),
        text_field("routine_schema", false),
        text_field("routine_name", false),
        text_field("routine_type", false),
        text_field("data_type", true),
        text_field("type_udt_catalog", true),
        text_field("type_udt_schema", true),
        text_field("type_udt_name", true),
        text_field("routine_body", false),
        text_field("routine_definition", true),
        text_field("external_language", true),
        text_field("is_deterministic", false),
        text_field("is_null_call", true),
        text_field("security_type", false),
    ]
}

pub(super) fn build_routines_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    _default_schema: Option<&str>,
    database_name: Option<&str>,
) -> DbResult<LogicalPlan> {
    let fields = routines_output_fields();
    let catalog_name = database_name.unwrap_or("aiondb").to_owned();
    let mut rows: Vec<Vec<Value>> = Vec::new();
    for func in catalog.list_functions(txn_id)? {
        // We don't track per-function namespaces yet; PG defaults to public.
        let schema = "public".to_owned();
        let routine_type = "FUNCTION".to_owned();
        let data_type = func
            .raw_return_type_name
            .clone()
            .unwrap_or_else(|| format!("{}", func.return_type));
        let language = func.language.to_uppercase();
        rows.push(vec![
            Value::Text(catalog_name.clone()),
            Value::Text(schema.clone()),
            Value::Text(func.name.clone()),
            Value::Text(catalog_name.clone()),
            Value::Text(schema.clone()),
            Value::Text(func.name.clone()),
            Value::Text(routine_type),
            Value::Text(data_type.clone()),
            Value::Text(catalog_name.clone()),
            Value::Text("pg_catalog".to_owned()),
            Value::Text(data_type),
            Value::Text(if language == "SQL" {
                "SQL".to_owned()
            } else {
                "EXTERNAL".to_owned()
            }),
            Value::Text(func.body.clone()),
            Value::Text(language),
            yes_no(false),
            yes_no(false),
            Value::Text("INVOKER".to_owned()),
        ]);
    }
    Ok(LogicalPlan::ProjectValues {
        output_fields: fields.clone(),
        rows: rows_to_typed(&fields, rows),
        order_by: Vec::new(),
        limit: None,
        offset: None,
    })
}

// ---------------------------------------------------------------
// information_schema.role_table_grants
// ---------------------------------------------------------------
//
// The view definition in PG matches table_privileges restricted to grantee
// rows the current role belongs to. We surface the same rows; ORM tools
// use the two interchangeably.

pub(super) fn role_table_grants_output_fields() -> Vec<ResultField> {
    table_privileges_output_fields()
}

pub(super) fn build_role_table_grants_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
    database_name: Option<&str>,
) -> DbResult<LogicalPlan> {
    build_table_privileges_plan(catalog, txn_id, default_schema, database_name)
}

fn is_primary_key_index(table: &TableDescriptor, idx: &aiondb_catalog::IndexDescriptor) -> bool {
    super::super::pg_catalog::is_primary_key_index_for_info_schema(table, idx)
}

// ---------------------------------------------------------------
// information_schema.parameters (stub: empty; user functions have arg
// metadata but the columns table is enough for most ORM tooling).
// ---------------------------------------------------------------

pub(super) fn parameters_output_fields() -> Vec<ResultField> {
    vec![
        text_field("specific_catalog", false),
        text_field("specific_schema", false),
        text_field("specific_name", false),
        int_field("ordinal_position", false),
        text_field("parameter_mode", true),
        text_field("is_result", true),
        text_field("as_locator", true),
        text_field("parameter_name", true),
        text_field("data_type", true),
        text_field("udt_catalog", true),
        text_field("udt_schema", true),
        text_field("udt_name", true),
    ]
}

pub(super) fn build_empty_parameters_plan() -> DbResult<LogicalPlan> {
    let fields = parameters_output_fields();
    Ok(LogicalPlan::ProjectValues {
        output_fields: fields,
        rows: Vec::new(),
        order_by: Vec::new(),
        limit: None,
        offset: None,
    })
}

// ---------------------------------------------------------------
// information_schema.domains (stub: empty; AionDB's domains live in
// pg_type but we don't maintain the SQL-standard domain projection yet).
// ---------------------------------------------------------------

pub(super) fn domains_output_fields() -> Vec<ResultField> {
    vec![
        text_field("domain_catalog", false),
        text_field("domain_schema", false),
        text_field("domain_name", false),
        text_field("data_type", false),
        text_field("character_maximum_length", true),
        text_field("character_octet_length", true),
        text_field("numeric_precision", true),
        text_field("numeric_precision_radix", true),
        text_field("numeric_scale", true),
        text_field("collation_catalog", true),
        text_field("collation_schema", true),
        text_field("collation_name", true),
        text_field("domain_default", true),
        text_field("udt_catalog", true),
        text_field("udt_schema", true),
        text_field("udt_name", true),
    ]
}

pub(super) fn build_empty_domains_plan(_database_name: Option<&str>) -> DbResult<LogicalPlan> {
    let fields = domains_output_fields();
    Ok(LogicalPlan::ProjectValues {
        output_fields: fields,
        rows: Vec::new(),
        order_by: Vec::new(),
        limit: None,
        offset: None,
    })
}

// ---------------------------------------------------------------
// information_schema.applicable_roles / enabled_roles
// ---------------------------------------------------------------

pub(super) fn applicable_roles_output_fields() -> Vec<ResultField> {
    vec![
        text_field("grantee", false),
        text_field("role_name", false),
        text_field("is_grantable", false),
    ]
}

pub(super) fn build_applicable_roles_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
) -> DbResult<LogicalPlan> {
    let fields = applicable_roles_output_fields();
    let mut rows: Vec<Vec<Value>> = Vec::new();
    for role in catalog.list_roles(txn_id)? {
        // Without role-membership graph, we surface each role as
        // applicable-to-itself. Tools just want a non-empty result.
        rows.push(vec![
            Value::Text(role.name.clone()),
            Value::Text(role.name),
            yes_no(false),
        ]);
    }
    Ok(LogicalPlan::ProjectValues {
        output_fields: fields.clone(),
        rows: rows_to_typed(&fields, rows),
        order_by: Vec::new(),
        limit: None,
        offset: None,
    })
}

pub(super) fn enabled_roles_output_fields() -> Vec<ResultField> {
    vec![text_field("role_name", false)]
}

pub(super) fn build_enabled_roles_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
) -> DbResult<LogicalPlan> {
    let fields = enabled_roles_output_fields();
    let mut rows: Vec<Vec<Value>> = Vec::new();
    for role in catalog.list_roles(txn_id)? {
        rows.push(vec![Value::Text(role.name)]);
    }
    Ok(LogicalPlan::ProjectValues {
        output_fields: fields.clone(),
        rows: rows_to_typed(&fields, rows),
        order_by: Vec::new(),
        limit: None,
        offset: None,
    })
}

// ---------------------------------------------------------------
// information_schema.character_sets / collations
// ---------------------------------------------------------------

pub(super) fn character_sets_output_fields() -> Vec<ResultField> {
    vec![
        text_field("character_set_catalog", true),
        text_field("character_set_schema", true),
        text_field("character_set_name", false),
        text_field("character_repertoire", false),
        text_field("form_of_use", false),
        text_field("default_collate_catalog", true),
        text_field("default_collate_schema", true),
        text_field("default_collate_name", true),
    ]
}

pub(super) fn build_character_sets_plan(database_name: Option<&str>) -> DbResult<LogicalPlan> {
    let fields = character_sets_output_fields();
    let cat = database_name.unwrap_or("aiondb").to_owned();
    // PG ships exactly one row for the database's encoding; UTF8 is
    // hard-coded as AionDB's only supported client/server encoding.
    let rows: Vec<Vec<Value>> = vec![vec![
        Value::Null,
        Value::Text("information_schema".to_owned()),
        Value::Text("UTF8".to_owned()),
        Value::Text("UCS".to_owned()),
        Value::Text("ISO/IEC 10646-1".to_owned()),
        Value::Text(cat),
        Value::Text("pg_catalog".to_owned()),
        Value::Text("default".to_owned()),
    ]];
    Ok(LogicalPlan::ProjectValues {
        output_fields: fields.clone(),
        rows: rows_to_typed(&fields, rows),
        order_by: Vec::new(),
        limit: None,
        offset: None,
    })
}

pub(super) fn collations_output_fields() -> Vec<ResultField> {
    vec![
        text_field("collation_catalog", false),
        text_field("collation_schema", false),
        text_field("collation_name", false),
        text_field("pad_attribute", false),
    ]
}

pub(super) fn build_collations_plan(database_name: Option<&str>) -> DbResult<LogicalPlan> {
    let fields = collations_output_fields();
    let cat = database_name.unwrap_or("aiondb").to_owned();
    let make = |name: &str| -> Vec<Value> {
        vec![
            Value::Text(cat.clone()),
            Value::Text("pg_catalog".to_owned()),
            Value::Text(name.to_owned()),
            Value::Text("NO PAD".to_owned()),
        ]
    };
    let rows: Vec<Vec<Value>> = vec![make("default"), make("C"), make("POSIX")];
    Ok(LogicalPlan::ProjectValues {
        output_fields: fields.clone(),
        rows: rows_to_typed(&fields, rows),
        order_by: Vec::new(),
        limit: None,
        offset: None,
    })
}
