use std::collections::HashMap;

use aiondb_catalog::{
    CatalogPrivilege, FunctionPrivilegeTarget, IndexDescriptor, IndexKeyColumn, PrivilegeTarget,
    QualifiedName, SortOrder, TableDescriptor,
};
use aiondb_core::{ColumnId, IndexId, RelationId};
use aiondb_eval::{current_session_context, visible_session_schema_name, EvalSessionContext};
use aiondb_planner::pg_catalog::{synthetic_table_id, table_name_for_synthetic_id};

use crate::ExecutionContext;

pub(super) fn format_index_definition(index: &IndexDescriptor, table: &TableDescriptor) -> String {
    format_index_definition_with_expressions(index, table, None)
}

pub(super) fn format_index_definition_with_expressions(
    index: &IndexDescriptor,
    table: &TableDescriptor,
    extra_expressions: Option<&[String]>,
) -> String {
    let schema_name = quote_identifier(table.name.schema_name().unwrap_or("public"));
    let column_names = table_column_names(table);
    let method = match index.kind {
        aiondb_catalog::IndexKind::BTree => "btree",
        aiondb_catalog::IndexKind::Hash => "hash",
        aiondb_catalog::IndexKind::GiST => "gist",
        aiondb_catalog::IndexKind::Gin => "gin",
        aiondb_catalog::IndexKind::Brin => "brin",
        aiondb_catalog::IndexKind::Hnsw => "hnsw",
        _ => "btree",
    };
    let mut key_parts = index
        .key_columns
        .iter()
        .map(|column| format_index_key_column(&column_names, column))
        .collect::<Vec<_>>();
    if let Some(expressions) = extra_expressions {
        key_parts.extend(expressions.iter().map(|expr| format!("({expr})")));
    }
    let key_columns = key_parts.join(", ");
    let include_clause = if index.include_columns.is_empty() {
        String::new()
    } else {
        let included = index
            .include_columns
            .iter()
            .map(|column_id| {
                column_names.get(column_id).map_or_else(
                    || quote_identifier(&format!("column_{}", column_id.get())),
                    |column_name| quote_identifier(column_name),
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!(" INCLUDE ({included})")
    };
    format!(
        "CREATE {}INDEX {} ON {}.{} USING {} ({}){}",
        if index.unique { "UNIQUE " } else { "" },
        quote_identifier(index.name.object_name()),
        schema_name,
        quote_identifier(table.name.object_name()),
        method,
        key_columns,
        include_clause
    )
}

fn table_column_names(table: &TableDescriptor) -> HashMap<ColumnId, &str> {
    table
        .columns
        .iter()
        .map(|column| (column.column_id, column.name.as_str()))
        .collect()
}

pub(super) fn format_index_key_column(
    column_names: &HashMap<ColumnId, &str>,
    key: &IndexKeyColumn,
) -> String {
    let column_name = column_names.get(&key.column_id).map_or_else(
        || format!("column_{}", key.column_id.get()),
        |column_name| (*column_name).to_owned(),
    );
    let sort_order = match key.sort_order {
        SortOrder::Ascending => "ASC",
        SortOrder::Descending => "DESC",
        _ => "ASC",
    };
    let nulls = if key.nulls_first {
        "NULLS FIRST"
    } else {
        "NULLS LAST"
    };
    format!("{} {sort_order} {nulls}", quote_identifier(&column_name))
}

pub(super) fn parse_text_qualified_name(input: &str) -> QualifiedName {
    let parts = parse_identifier_components(input, '.');
    match parts.as_slice() {
        [] => QualifiedName::unqualified(""),
        [name] => QualifiedName::unqualified(name.clone()),
        [schema, name, rest @ ..] => {
            let mut object_name = name.clone();
            for part in rest {
                object_name.push('.');
                object_name.push_str(part);
            }
            QualifiedName::qualified(schema.clone(), object_name)
        }
    }
}

pub(super) fn resolve_builtin_relation_oid(name: &str) -> Option<i32> {
    let candidate = parse_text_qualified_name(name);
    match candidate.schema_name() {
        Some(schema_name)
            if !schema_name.eq_ignore_ascii_case("pg_catalog")
                && !schema_name.eq_ignore_ascii_case("information_schema") =>
        {
            return None;
        }
        _ => {}
    }
    let object_name = candidate.object_name().to_ascii_lowercase();
    if let Some(core_oid) = builtin_core_relation_oid(&object_name) {
        return Some(core_oid);
    }
    synthetic_table_id(&object_name).and_then(|oid| i32::try_from(oid).ok())
}

pub(super) fn builtin_relation_name_for_oid(oid: i32) -> Option<&'static str> {
    if let Some(name) = builtin_core_relation_name(oid) {
        return Some(name);
    }
    u64::try_from(oid)
        .ok()
        .and_then(table_name_for_synthetic_id)
}

// NOTE: this only knows pg_class and pg_authid because the executor uses two
// OID spaces internally: canonical PostgreSQL OIDs (1247, 2615, …) are emitted
// as `pg_class.oid` for client compatibility, while AionDB's synthetic 60_000+
// ids drive the virtual-table materialisers in the planner. Extending this
// table to all of SYSTEM_CATALOG_TABLES makes `to_regclass()` return canonical
// OIDs but breaks `SELECT * FROM pg_catalog.pg_attribute` because the synthetic
// dispatcher then receives a canonical OID it cannot route. A proper fix needs
// a translation layer at the client-facing surface — tracked separately.
fn builtin_core_relation_oid(name: &str) -> Option<i32> {
    match name {
        "pg_class" => Some(1259),
        "pg_authid" => Some(1260),
        _ => None,
    }
}

fn builtin_core_relation_name(oid: i32) -> Option<&'static str> {
    match oid {
        1259 => Some("pg_class"),
        1260 => Some("pg_authid"),
        _ => None,
    }
}

// PostgreSQL reserves OIDs below `FirstNormalObjectId` (16_384) for system
// catalogs, and treats indexes as living in a higher band; we shift our
// internal ids into those ranges so that `oid` values exposed via pg_catalog
// look plausible to clients (psql `\d`, ORMs that compare OIDs).
pub(super) fn relation_id_to_oid(relation_id: RelationId) -> i32 {
    i32::try_from(relation_id.get())
        .unwrap_or(i32::MAX)
        .saturating_add(16_384)
}

pub(super) fn index_id_to_oid(index_id: IndexId) -> i32 {
    i32::try_from(index_id.get())
        .unwrap_or(i32::MAX)
        .saturating_add(32_768)
}

pub(super) fn format_relation_name(name: &QualifiedName) -> String {
    match name.schema_name() {
        Some(schema_name) => {
            let visible_schema = visible_session_schema_name(schema_name);
            if visible_schema.eq_ignore_ascii_case("public")
                || visible_schema.eq_ignore_ascii_case("pg_catalog")
            {
                quote_identifier(name.object_name())
            } else {
                format!(
                    "{}.{}",
                    quote_identifier(&visible_schema),
                    quote_identifier(name.object_name())
                )
            }
        }
        None => quote_identifier(name.object_name()),
    }
}

pub(super) fn format_qualified_relation_name(name: &QualifiedName) -> String {
    match name.schema_name() {
        Some(schema_name) => {
            let visible_schema = visible_session_schema_name(schema_name);
            format!(
                "{}.{}",
                quote_identifier(&visible_schema),
                quote_identifier(name.object_name())
            )
        }
        None => quote_identifier(name.object_name()),
    }
}

pub(super) fn eval_session_context(context: &ExecutionContext) -> EvalSessionContext {
    let mut eval_session = current_session_context();
    let (datestyle, timezone, intervalstyle) = (
        context.resolve_session_setting("datestyle"),
        context.resolve_session_setting("timezone"),
        context.resolve_session_setting("intervalstyle"),
    );
    let temporal = EvalSessionContext::from_settings_with_interval_style(
        datestyle.as_deref(),
        timezone.as_deref(),
        intervalstyle.as_deref(),
    );
    eval_session.date_order = temporal.date_order;
    eval_session.date_style = temporal.date_style;
    eval_session.timezone = temporal.timezone;
    eval_session.interval_style = temporal.interval_style;
    eval_session
}

pub(super) fn index_lookup_schemas(
    candidate: &QualifiedName,
    context: &ExecutionContext,
) -> Vec<String> {
    let mut schemas = Vec::new();
    if let Some(schema_name) = candidate.schema_name() {
        let resolved_schema = if schema_name.eq_ignore_ascii_case("public") {
            session_search_path_schemas(context)
                .into_iter()
                .next()
                .filter(|active_schema| active_schema.to_ascii_lowercase().starts_with("db_"))
                .unwrap_or_else(|| schema_name.to_owned())
        } else {
            schema_name.to_owned()
        };
        push_unique_schema_name(&mut schemas, resolved_schema);
        return schemas;
    }

    for schema_name in session_search_path_schemas(context) {
        push_unique_schema_name(&mut schemas, schema_name);
    }
    for schema_name in ["pg_catalog", "public", "information_schema"] {
        push_unique_schema_name(&mut schemas, schema_name.to_owned());
    }
    schemas
}

pub(super) fn session_search_path_schemas(context: &ExecutionContext) -> Vec<String> {
    let Some(search_path) = context.resolve_session_setting("search_path") else {
        return Vec::new();
    };
    let current_user = context.current_user_name();
    let mut schemas = Vec::new();
    for component in parse_identifier_components(&search_path, ',') {
        let schema_name = if component == "$user" {
            current_user.clone().unwrap_or_default()
        } else {
            component
        };
        push_unique_schema_name(&mut schemas, schema_name);
    }
    schemas
}

pub(super) fn push_unique_schema_name(schemas: &mut Vec<String>, schema_name: String) {
    if schema_name.is_empty()
        || schemas
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&schema_name))
    {
        return;
    }
    schemas.push(schema_name);
}

pub(super) fn parse_identifier_components(input: &str, separator: char) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut chars = input.trim().chars().peekable();
    let mut in_quotes = false;
    let mut quoted = false;

    while let Some(ch) = chars.next() {
        match ch {
            '"' => {
                if in_quotes {
                    if matches!(chars.peek(), Some('"')) {
                        chars.next();
                        current.push('"');
                    } else {
                        in_quotes = false;
                        quoted = true;
                    }
                } else if current.trim().is_empty() {
                    in_quotes = true;
                } else {
                    current.push(ch);
                }
            }
            _ if ch == separator && !in_quotes => {
                parts.push(normalize_identifier_component(&current, quoted));
                current.clear();
                quoted = false;
            }
            _ => current.push(ch),
        }
    }

    parts.push(normalize_identifier_component(&current, quoted));
    parts
}

pub(super) fn normalize_identifier_component(component: &str, quoted: bool) -> String {
    let trimmed = component.trim();
    if quoted {
        trimmed.to_owned()
    } else {
        trimmed.to_ascii_lowercase()
    }
}

pub(super) use aiondb_parser::identifier::quote_identifier;

pub(super) fn privilege_covers_execute(privilege: &CatalogPrivilege) -> bool {
    matches!(privilege, CatalogPrivilege::Execute | CatalogPrivilege::All)
}

pub(super) fn function_target_name(function_spec: &str) -> FunctionPrivilegeTarget {
    let probe_sql = format!("GRANT EXECUTE ON FUNCTION {function_spec} TO __aiondb_acl_probe");
    if let Ok(aiondb_parser::Statement::Grant(grant)) =
        aiondb_parser::parse_prepared_statement(&probe_sql)
    {
        if let aiondb_parser::GrantTarget::Function(target) = grant.target {
            let name = match target.name.parts.as_slice() {
                [schema, function] => QualifiedName::qualified(schema, function),
                [function] => QualifiedName::unqualified(function),
                _ => QualifiedName::unqualified(target.name.parts.join(".")),
            };
            return FunctionPrivilegeTarget {
                name,
                arg_types: target.arg_types,
            };
        }
    }

    let bare_name = function_spec
        .split_once('(')
        .map_or(function_spec, |(name, _)| name);
    FunctionPrivilegeTarget {
        name: QualifiedName::parse(bare_name.trim()),
        arg_types: None,
    }
}

pub(super) fn function_target_matches(
    target: &PrivilegeTarget,
    function_name: &FunctionPrivilegeTarget,
) -> bool {
    match target {
        PrivilegeTarget::Function(granted) => {
            granted
                .name
                .name
                .eq_ignore_ascii_case(&function_name.name.name)
                && match (&granted.name.schema, &function_name.name.schema) {
                    (Some(left), Some(right)) => left.eq_ignore_ascii_case(right),
                    // When the probed function is schema-qualified, do not
                    // match grants that lack schema qualification to avoid
                    // cross-schema false positives.
                    (None, Some(_)) => false,
                    (_, None) => true,
                }
                && match (&granted.arg_types, &function_name.arg_types) {
                    (Some(left), Some(right)) => left == right,
                    _ => true,
                }
        }
        _ => false,
    }
}

pub(super) fn inherited_role_names(
    role_name: &str,
    privileges: &[aiondb_catalog::PrivilegeDescriptor],
) -> Vec<String> {
    let mut inherited = vec![role_name.to_owned()];
    for descriptor in privileges {
        if let PrivilegeTarget::Role(member_of) = &descriptor.target {
            inherited.push(member_of.clone());
        }
    }
    inherited
}

pub(super) fn role_has_builtin_execute(
    role_names: &[String],
    function_name: &FunctionPrivilegeTarget,
) -> bool {
    let builtins = [
        "pg_ls_logicalsnapdir",
        "pg_ls_logicalmapdir",
        "pg_ls_replslotdir",
    ];
    let role_matches = role_names
        .iter()
        .any(|role| role.eq_ignore_ascii_case("pg_monitor"));
    role_matches
        && builtins
            .iter()
            .any(|builtin| function_name.name.name.eq_ignore_ascii_case(builtin))
}

/// Parse a comma-separated PostgreSQL privilege name list, as accepted by
/// `has_table_privilege`/`has_schema_privilege`/etc. Each entry may carry a
/// trailing `WITH GRANT OPTION` which is stripped and treated as the base
/// privilege (we have no grant-option column, so the two are equivalent).
/// Returns `None` when any entry is unrecognised.
pub(super) fn parse_privilege_name_list(
    input: &str,
    allowed: &[CatalogPrivilege],
) -> Option<Vec<CatalogPrivilege>> {
    let mut parsed = Vec::new();
    for raw in input.split(',') {
        let mut name = raw.trim().to_ascii_lowercase();
        if let Some(stripped) = name.strip_suffix("with grant option") {
            name = stripped.trim_end().to_owned();
        }
        let privilege = match name.as_str() {
            "select" => CatalogPrivilege::Select,
            "insert" => CatalogPrivilege::Insert,
            "update" => CatalogPrivilege::Update,
            "delete" => CatalogPrivilege::Delete,
            "truncate" => CatalogPrivilege::Truncate,
            "references" => CatalogPrivilege::References,
            "trigger" => CatalogPrivilege::Trigger,
            "usage" => CatalogPrivilege::Usage,
            "create" => CatalogPrivilege::Create,
            "execute" => CatalogPrivilege::Execute,
            "connect" => CatalogPrivilege::Connect,
            "temporary" | "temp" => CatalogPrivilege::Temporary,
            _ => return None,
        };
        if !allowed.contains(&privilege) {
            return None;
        }
        if !parsed.contains(&privilege) {
            parsed.push(privilege);
        }
    }
    if parsed.is_empty() {
        None
    } else {
        Some(parsed)
    }
}

pub(super) fn privilege_covers(granted: &CatalogPrivilege, required: &CatalogPrivilege) -> bool {
    matches!(granted, CatalogPrivilege::All) || granted == required
}

pub(super) fn table_privilege_target_matches(
    target: &PrivilegeTarget,
    table: &TableDescriptor,
) -> bool {
    match target {
        PrivilegeTarget::Table(name) => {
            name.name.eq_ignore_ascii_case(&table.name.name)
                && match (&name.schema, &table.name.schema) {
                    (Some(granted), Some(actual)) => granted.eq_ignore_ascii_case(actual),
                    // If the target relation is schema-qualified, require a
                    // schema-qualified grant target to avoid leaking grants
                    // from same-named tables in other schemas.
                    (None, Some(_)) => false,
                    (_, None) => true,
                }
        }
        _ => false,
    }
}

pub(super) fn schema_privilege_target_matches(target: &PrivilegeTarget, schema_name: &str) -> bool {
    matches!(target, PrivilegeTarget::Schema(name) if name.eq_ignore_ascii_case(schema_name))
}

pub(super) fn database_privilege_target_matches(
    target: &PrivilegeTarget,
    database_name: &str,
) -> bool {
    matches!(
        target,
        PrivilegeTarget::Database(name) if name.eq_ignore_ascii_case(database_name)
    )
}

pub(super) fn sequence_privilege_target_matches(
    target: &PrivilegeTarget,
    table: &TableDescriptor,
) -> bool {
    table_privilege_target_matches(target, table)
}

pub(super) fn text_arg_to_role(value: &aiondb_core::Value) -> Option<String> {
    match value {
        aiondb_core::Value::Text(text) => Some(text.trim_matches('"').to_owned()),
        aiondb_core::Value::Int(oid) => {
            resolve_role_name_from_oid(*oid).or_else(|| Some(oid.to_string()))
        }
        aiondb_core::Value::BigInt(oid) => {
            let oid = i32::try_from(*oid).ok()?;
            resolve_role_name_from_oid(oid).or_else(|| Some(oid.to_string()))
        }
        _ => None,
    }
}

fn resolve_role_name_from_oid(oid: i32) -> Option<String> {
    let context = current_session_context();
    context.role_names_by_oid.get(&oid).cloned()
}

pub(super) fn role_name_from_oid(oid: i32) -> Option<String> {
    resolve_role_name_from_oid(oid)
}

pub(super) fn format_value_for_error(value: &aiondb_core::Value) -> String {
    match value {
        aiondb_core::Value::Text(text) => text.clone(),
        other => other.to_string(),
    }
}

pub(super) fn table_descriptor_owner_matches(table: &TableDescriptor, role_name: &str) -> bool {
    table
        .owner
        .as_ref()
        .is_some_and(|owner| owner.eq_ignore_ascii_case(role_name))
}

/// Returns true when every comma-separated entry in `list` is the legacy
/// "rule" privilege keyword (optionally followed by ` with grant option`).
/// PostgreSQL continues to accept `rule` in `has_table_privilege` but
/// always returns false - the privilege was removed in 8.2.
pub(super) fn privilege_name_list_is_only_rule(list: &str) -> bool {
    let mut saw_entry = false;
    for part in list.split(',') {
        saw_entry = true;
        let mut name = part.trim().to_ascii_lowercase();
        if let Some(stripped) = name.strip_suffix("with grant option") {
            name = stripped.trim_end().to_owned();
        }
        if name != "rule" {
            return false;
        }
    }
    saw_entry
}

pub(super) fn table_column_at_one_based(table: &TableDescriptor, idx: i64) -> bool {
    if idx < 1 {
        return false;
    }
    let zero_based = match usize::try_from(idx - 1) {
        Ok(index) => index,
        Err(_) => return false,
    };
    zero_based < table.columns.len()
}
