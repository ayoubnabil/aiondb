use super::*;
use aiondb_parser::identifier::is_system_column_name;

fn alter_table_action_label(action: &AlterTableAction) -> &'static str {
    match action {
        AlterTableAction::AddColumn { .. } => "ADD COLUMN",
        AlterTableAction::DropColumn { .. } => "DROP COLUMN",
        AlterTableAction::RenameTable { .. } => "RENAME",
        AlterTableAction::RenameColumn { .. } => "RENAME COLUMN",
        AlterTableAction::SetDefault { .. } => "ALTER COLUMN ... SET DEFAULT",
        AlterTableAction::DropDefault { .. } => "ALTER COLUMN ... DROP DEFAULT",
        AlterTableAction::SetNotNull { .. } => "ALTER COLUMN ... SET NOT NULL",
        AlterTableAction::DropNotNull { .. } => "ALTER COLUMN ... DROP NOT NULL",
        AlterTableAction::AddConstraint { .. } => "ADD CONSTRAINT",
        AlterTableAction::DropConstraint { .. } => "DROP CONSTRAINT",
        AlterTableAction::AlterColumnType { .. } => "ALTER COLUMN ... TYPE",
        AlterTableAction::RenameConstraint { .. } => "RENAME CONSTRAINT",
    }
}

fn require_column(
    relation: &TableDescriptor,
    column_name: &str,
    span_start: usize,
) -> DbResult<ColumnDescriptor> {
    relation
        .column_by_name(column_name)
        .cloned()
        .ok_or_else(|| {
            DbError::bind_error(
                SqlState::UndefinedColumn,
                format!(
                    "column \"{}\" of relation \"{}\" does not exist",
                    column_name,
                    relation.name.object_name()
                ),
            )
            .with_position(span_start + 1)
        })
}

fn serialize_check_expr(expr: &aiondb_parser::Expr) -> DbResult<String> {
    crate::type_check::serialize_expr(expr)
}

fn parse_distance_option(raw: &str) -> DbResult<aiondb_plan::HnswPlanDistanceMetric> {
    match raw.to_lowercase().as_str() {
        "l2" | "euclidean" => Ok(aiondb_plan::HnswPlanDistanceMetric::L2),
        "cosine" => Ok(aiondb_plan::HnswPlanDistanceMetric::Cosine),
        "ip" | "inner_product" | "dot" => Ok(aiondb_plan::HnswPlanDistanceMetric::InnerProduct),
        "l1" | "manhattan" => Ok(aiondb_plan::HnswPlanDistanceMetric::Manhattan),
        other => Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!("unknown distance metric \"{other}\" for option \"distance\""),
        )
        .with_client_detail("Supported values: l2, cosine, inner_product, manhattan.".to_owned())),
    }
}

fn parse_vector_operator_class_metric(raw: &str) -> Option<aiondb_plan::HnswPlanDistanceMetric> {
    match raw
        .rsplit('.')
        .next()
        .unwrap_or(raw)
        .to_ascii_lowercase()
        .as_str()
    {
        "vector_l2_ops" | "halfvec_l2_ops" | "sparsevec_l2_ops" => {
            Some(aiondb_plan::HnswPlanDistanceMetric::L2)
        }
        "vector_cosine_ops" | "halfvec_cosine_ops" | "sparsevec_cosine_ops" => {
            Some(aiondb_plan::HnswPlanDistanceMetric::Cosine)
        }
        "vector_ip_ops" | "halfvec_ip_ops" | "sparsevec_ip_ops" => {
            Some(aiondb_plan::HnswPlanDistanceMetric::InnerProduct)
        }
        "vector_l1_ops" | "halfvec_l1_ops" | "sparsevec_l1_ops" => {
            Some(aiondb_plan::HnswPlanDistanceMetric::Manhattan)
        }
        _ => None,
    }
}

fn parse_quantization_option(raw: &str) -> DbResult<aiondb_plan::HnswPlanQuantization> {
    match raw.to_lowercase().as_str() {
        "none" | "raw" | "f32" => Ok(aiondb_plan::HnswPlanQuantization::None),
        "sq" | "scalar" => Ok(aiondb_plan::HnswPlanQuantization::Scalar),
        "bq" | "binary" => Ok(aiondb_plan::HnswPlanQuantization::Binary),
        "pq" | "product" => Ok(aiondb_plan::HnswPlanQuantization::Product),
        other => Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!("unknown quantization kind \"{other}\" for option \"quantization\""),
        )
        .with_client_detail("Supported values: none, sq, bq, pq.".to_owned())),
    }
}

fn referenced_columns_or_primary_key(table: &TableDescriptor, explicit: &[String]) -> Vec<String> {
    if !explicit.is_empty() {
        return explicit.to_vec();
    }

    table
        .primary_key
        .as_ref()
        .map(|primary_key| {
            primary_key
                .iter()
                .filter_map(|column_id| {
                    table
                        .columns
                        .iter()
                        .find(|column| column.column_id == *column_id)
                        .map(|column| column.name.clone())
                })
                .collect()
        })
        .unwrap_or_default()
}

fn index_method_name(method: aiondb_parser::IndexMethod) -> &'static str {
    match method {
        aiondb_parser::IndexMethod::BTree => "btree",
        aiondb_parser::IndexMethod::Hnsw => "hnsw",
        aiondb_parser::IndexMethod::IvfFlat => "ivfflat",
        aiondb_parser::IndexMethod::Gin => "gin",
        aiondb_parser::IndexMethod::Gist => "gist",
        aiondb_parser::IndexMethod::SpGist => "spgist",
        aiondb_parser::IndexMethod::Brin => "brin",
        aiondb_parser::IndexMethod::Hash => "hash",
    }
}

fn typed_table_type_name(type_name: &ObjectName) -> String {
    type_name.parts.join(".").to_ascii_lowercase()
}

fn geometric_compat_type_name(raw_type_name: Option<&str>) -> Option<String> {
    let raw = raw_type_name?;
    let normalized = aiondb_eval::normalize_compat_type_name(raw);
    match normalized.as_str() {
        "point" | "box" | "line" | "lseg" | "path" | "polygon" | "circle" => Some(normalized),
        _ => None,
    }
}

/// Return the (normalised) name of a registered domain when `raw_type_name`
/// references one, traversing dotted-schema-qualified names by taking the
/// trailing segment (PG resolves domain names through the search path).
fn domain_compat_type_name(raw_type_name: Option<&str>) -> Option<String> {
    let raw = raw_type_name?;
    let normalized = aiondb_eval::normalize_compat_type_name(raw);
    let bare = normalized
        .rsplit_once('.')
        .map_or(normalized.as_str(), |(_, tail)| tail);
    let session_ctx = current_session_context();
    session_ctx
        .domain_defs
        .iter()
        .find(|d| d.name.eq_ignore_ascii_case(bare))
        .map(|d| d.name.clone())
}

/// Return the canonical name of a registered ENUM-kind compat user type
/// (composite types and shell types, which carry empty `enum_labels`,
/// return `None`).
fn enum_compat_type_name(raw_type_name: Option<&str>) -> Option<String> {
    let raw = raw_type_name?;
    let normalized = aiondb_eval::normalize_compat_type_name(raw);
    let bare = normalized
        .rsplit_once('.')
        .map_or(normalized.as_str(), |(_, tail)| tail);
    let session_ctx = current_session_context();
    let entry = session_ctx
        .compat_user_types
        .iter()
        .find(|t| t.name.eq_ignore_ascii_case(bare))?;
    if entry.enum_labels.is_empty() {
        return None;
    }
    Some(entry.name.clone())
}

/// Resolve a column whose declared type is a user-defined identifier (e.g.
/// a domain name) to its concrete base `DataType` and `TextTypeModifier`.
/// Follows the domain chain so nested domains inherit the most specific
/// length modifier. Returns the original pair unchanged when the raw name
/// is not a known domain.
fn resolve_domain_column_type(
    raw_type_name: Option<&str>,
    fallback_data_type: DataType,
    fallback_modifier: Option<TextTypeModifier>,
) -> (DataType, Option<TextTypeModifier>) {
    let Some(raw) = raw_type_name else {
        return (fallback_data_type, fallback_modifier);
    };
    let mut name = raw.to_ascii_lowercase();
    if let Some(idx) = name.rfind('.') {
        name = name[idx + 1..].to_owned();
    }
    let mut data_type = fallback_data_type;
    let mut modifier = fallback_modifier;
    let mut is_array_domain = false;
    let session_ctx = current_session_context();
    let mut current = name;
    for _ in 0..32 {
        let Some(def) = session_ctx
            .domain_defs
            .iter()
            .find(|d| d.name.eq_ignore_ascii_case(&current))
            .cloned()
        else {
            break;
        };
        let mut base = def.base_type.to_ascii_lowercase();
        // Detect array domain and strip the "[]" suffix for the base-type
        // lookup. Array domains apply their length modifier to element
        // values, not to the whole array text, so suppress the per-column
        // modifier here (PG enforces it during array element construction,
        // which AionDB doesn't currently model granularly).
        if base.ends_with("[]") {
            is_array_domain = true;
            base = base.trim_end_matches("[]").trim().to_owned();
        }
        if let Some(len) = def.char_length {
            if !is_array_domain
                && (modifier.is_none()
                    || matches!(modifier, Some(TextTypeModifier::VarChar { .. })))
            {
                modifier = Some(TextTypeModifier::VarChar { length: len });
            }
        }
        if session_ctx
            .domain_defs
            .iter()
            .any(|d| d.name.eq_ignore_ascii_case(&base))
        {
            current = base;
            continue;
        }
        let base_dt = match base.as_str() {
            "int4" | "int" | "integer" => DataType::Int,
            "int2" | "smallint" => DataType::Int,
            "int8" | "bigint" => DataType::BigInt,
            "float4" | "real" => DataType::Real,
            "float8" | "double precision" | "double" => DataType::Double,
            "numeric" | "decimal" => DataType::Numeric,
            "bool" | "boolean" => DataType::Boolean,
            "date" => DataType::Date,
            "time" => DataType::Time,
            "timetz" => DataType::TimeTz,
            "timestamp" => DataType::Timestamp,
            "timestamptz" => DataType::TimestampTz,
            "interval" => DataType::Interval,
            "bytea" => DataType::Blob,
            "uuid" => DataType::Uuid,
            "jsonb" | "json" => DataType::Jsonb,
            _ => DataType::Text,
        };
        data_type = if is_array_domain {
            // Keep the fallback data_type when it's already an array; else
            // wrap the inferred base scalar.
            if matches!(data_type, DataType::Array(_)) {
                data_type
            } else {
                DataType::Array(Box::new(base_dt))
            }
        } else {
            base_dt
        };
        break;
    }
    if is_array_domain {
        // Don't propagate a scalar varchar modifier onto an array column.
        modifier = None;
    }
    (data_type, modifier)
}

fn typed_table_composite_columns(type_name: &ObjectName) -> DbResult<Vec<BoundCreateColumn>> {
    let normalized = typed_table_type_name(type_name);
    let session_context = current_session_context();
    let Some(user_type) = session_context
        .compat_user_types
        .iter()
        .find(|entry| entry.name == normalized)
    else {
        return Err(DbError::bind_error(
            SqlState::WrongObjectType,
            format!(
                "type \"{}\" is not a composite type",
                type_name.parts.join(".")
            ),
        ));
    };
    if user_type.composite_fields.is_empty() {
        return Err(DbError::bind_error(
            SqlState::WrongObjectType,
            format!(
                "type \"{}\" is not a composite type",
                type_name.parts.join(".")
            ),
        ));
    }

    Ok(user_type
        .composite_fields
        .iter()
        .map(|field| BoundCreateColumn {
            name: field.name.clone(),
            data_type: field.data_type.clone(),
            raw_type_name: field.raw_type_name.clone(),
            text_type_modifier: None,
            nullable: true,
            default: None,
            identity: None,
        })
        .collect())
}

impl Binder {
    /// Check whether an index with the given qualified name exists anywhere
    /// in the catalog.
    pub(super) fn index_exists(
        &self,
        txn_id: TxnId,
        index_name: &QualifiedName,
        default_schema: Option<&str>,
    ) -> DbResult<bool> {
        let schema_name = index_name
            .schema_name()
            .unwrap_or(default_schema.unwrap_or("public"));
        let schema = self
            .catalog
            .get_schema(txn_id, &QualifiedName::unqualified(schema_name))?;
        let Some(schema) = schema else {
            return Ok(false);
        };
        for table in self.catalog.list_tables(txn_id, schema.schema_id)? {
            for index in self.catalog.list_indexes(txn_id, table.table_id)? {
                if index.schema_id == schema.schema_id
                    && index
                        .name
                        .name
                        .eq_ignore_ascii_case(index_name.object_name())
                {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    pub(super) fn index_exists_for_name(
        &self,
        txn_id: TxnId,
        name: &ObjectName,
        default_schema: Option<&str>,
    ) -> DbResult<bool> {
        for candidate in relation_lookup_candidates(name, default_schema)? {
            if self.index_exists(txn_id, &candidate, default_schema)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(super) fn bind_create_table_as(
        &self,
        ctas: &CreateTableAsStatement,
        txn_id: TxnId,
        default_schema: Option<&str>,
    ) -> DbResult<BoundCreateTableAs> {
        let relation_name =
            qualified_create_table_name(&ctas.name, default_schema, ctas.temporary)?;
        let query = self.bind_select(&ctas.query, txn_id, default_schema)?;
        Ok(BoundCreateTableAs {
            relation_name,
            query,
            with_no_data: ctas.with_no_data,
            column_aliases: ctas.column_aliases.clone(),
        })
    }

    pub(super) fn bind_create_table(
        &self,
        create_table: &CreateTableStatement,
        txn_id: TxnId,
        default_schema: Option<&str>,
    ) -> DbResult<BoundCreateTable> {
        // ------------------------------------------------------------------
        // Schema validation: temp tables in non-temp schemas, unlogged in temp schema
        // ------------------------------------------------------------------
        if create_table.temporary {
            if let Some(schema) = create_table.name.parts.first() {
                if create_table.name.parts.len() > 1
                    && !schema.eq_ignore_ascii_case(PG_TEMP_SCHEMA_NAME)
                {
                    return Err(DbError::bind_error(
                        SqlState::InvalidTableDefinition,
                        "cannot create temporary relation in non-temporary schema",
                    )
                    .with_position(create_table.name.span.start + 1));
                }
            }
        }
        if create_table.unlogged {
            if let Some(schema) = create_table.name.parts.first() {
                if create_table.name.parts.len() > 1
                    && schema.eq_ignore_ascii_case(PG_TEMP_SCHEMA_NAME)
                {
                    return Err(DbError::bind_error(
                        SqlState::InvalidTableDefinition,
                        "only temporary relations may be created in temporary schemas",
                    )
                    .with_position(create_table.name.span.start + 1));
                }
            }
        }

        // ------------------------------------------------------------------
        // Pseudo-type column validation
        // ------------------------------------------------------------------
        let pseudo_types = [
            "unknown",
            "record",
            "void",
            "any",
            "anyarray",
            "anyelement",
            "anynonarray",
            "anyenum",
            "anyrange",
            "anymultirange",
            "anycompatible",
            "anycompatiblearray",
            "anycompatiblenonarray",
            "anycompatiblerange",
            "anycompatiblemultirange",
            "internal",
            "language_handler",
            "fdw_handler",
            "table_am_handler",
            "tsm_handler",
            "index_am_handler",
            "event_trigger",
            "trigger",
            "pg_ddl_command",
        ];
        for col in &create_table.columns {
            let type_name = col.data_type.to_string().to_ascii_lowercase();
            if pseudo_types.iter().any(|pt| type_name == *pt) {
                return Err(DbError::bind_error(
                    SqlState::InvalidTableDefinition,
                    format!("column \"{}\" has pseudo-type {}", col.name, type_name),
                ));
            }
        }

        // Resolve inherited/LIKE parent columns up-front so validations that
        // run before inherited-column materialization still see the effective
        // column set.
        let mut inherited_column_names: Vec<String> = Vec::new();
        for parent_name in &create_table.inherits {
            let error_name = relation_error_name(parent_name, default_schema)?;
            let (_, parent_table) = resolve_table_in_search_path(
                self.catalog.as_ref(),
                txn_id,
                parent_name,
                default_schema,
            )?
            .ok_or_else(|| undefined_table(parent_name, &error_name))?;
            for col in &parent_table.columns {
                let lower = col.name.to_ascii_lowercase();
                if !inherited_column_names
                    .iter()
                    .any(|existing| existing == &lower)
                {
                    inherited_column_names.push(lower);
                }
            }
        }

        // ------------------------------------------------------------------
        // Partitioned table validations (table has PARTITION BY clause)
        // ------------------------------------------------------------------
        if let Some(ref pb) = create_table.partition_by {
            // List partition strategy cannot have more than one column
            if pb.strategy == aiondb_parser::ast::PartitionStrategy::List && pb.columns.len() > 1 {
                return Err(DbError::bind_error(
                    SqlState::InvalidTableDefinition,
                    "cannot use \"list\" partition strategy with more than one column",
                ));
            }

            // Exclusion constraints not supported on partitioned tables
            if create_table.has_exclusion_constraint {
                return Err(DbError::bind_error(
                    SqlState::FeatureNotSupported,
                    "exclusion constraints are not supported on partitioned tables",
                ));
            }

            // Validate partition key columns exist and are not system columns
            let system_columns = ["xmin", "xmax", "cmin", "cmax", "ctid", "tableoid"];
            let mut column_names_set: Vec<String> = create_table
                .columns
                .iter()
                .map(|c| c.name.to_ascii_lowercase())
                .collect();
            for inherited in &inherited_column_names {
                if !column_names_set.iter().any(|cn| cn == inherited) {
                    column_names_set.push(inherited.clone());
                }
            }
            for col_name in &pb.columns {
                let lower = col_name.to_ascii_lowercase();
                if system_columns.iter().any(|sc| lower == *sc) {
                    return Err(DbError::bind_error(
                        SqlState::InvalidTableDefinition,
                        format!("cannot use system column \"{col_name}\" in partition key"),
                    ));
                }
                // Only validate if the column name looks like a plain identifier
                // (not a function call or expression)
                if !lower.is_empty()
                    && lower.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    && !column_names_set
                        .iter()
                        .any(|cn| cn.eq_ignore_ascii_case(&lower))
                {
                    return Err(DbError::bind_error(
                        SqlState::UndefinedColumn,
                        format!("column \"{col_name}\" named in partition key does not exist"),
                    ));
                }
            }

            // Storage parameters on partitioned tables
            if create_table.has_storage_params {
                return Err(DbError::bind_error(
                    SqlState::InvalidParameterValue,
                    "cannot specify storage parameters for a partitioned table",
                )
                .with_client_hint(
                    "Specify storage parameters for each partition, not the parent.".to_owned(),
                ));
            }
        }

        // ------------------------------------------------------------------
        // Cannot inherit from a partitioned table
        // (We detect this by checking if the parent table has partition_by set,
        // for now - this validation would need catalog support)
        // ------------------------------------------------------------------

        if let Some(type_name) = create_table.typed_table_of.as_ref() {
            if let Some(options) = create_table.typed_table_options.as_ref() {
                if !options.trim().is_empty() {
                    return Err(DbError::bind_error(
                        SqlState::FeatureNotSupported,
                        "typed table column options are not supported",
                    ));
                }
            }

            let columns = typed_table_composite_columns(type_name)?;
            return Ok(BoundCreateTable {
                relation_name: qualified_create_table_name(
                    &create_table.name,
                    default_schema,
                    create_table.temporary,
                )?,
                columns,
                typed_table_of: Some(typed_table_type_name(type_name)),
                primary_key_columns: Vec::new(),
                unique_constraints: Vec::new(),
                foreign_keys: Vec::new(),
                check_constraints: Vec::new(),
                shard_key_columns: Vec::new(),
                shard_count: None,
                notices: Vec::new(),
            });
        }

        // ------------------------------------------------------------------
        // DEFAULT expression validation
        // ------------------------------------------------------------------
        for col in &create_table.columns {
            if let Some(ref default_expr) = col.default {
                validate_default_expr(default_expr)?;
            }
        }

        let mut primary_key_columns = Vec::new();
        let mut unique_constraints = Vec::new();
        let mut foreign_keys = Vec::new();
        let mut check_constraints = Vec::new();

        let column_names: Vec<String> = create_table
            .columns
            .iter()
            .map(|c| c.name.to_ascii_lowercase())
            .chain(inherited_column_names.iter().cloned())
            .collect();

        // Collect inline PRIMARY KEY from column definitions
        for column in &create_table.columns {
            if column.primary_key {
                primary_key_columns.push(column.name.clone());
            }
            if column.unique {
                unique_constraints.push(BoundUniqueConstraint {
                    columns: vec![column.name.clone()],
                    name: None,
                });
            }
            for check_expr in &column.inline_checks {
                let serialized = serialize_check_expr(check_expr)?;
                check_constraints.push((None, serialized));
            }
            for inline_ref in &column.inline_references {
                if !inline_ref.on_update_set_columns.is_empty() {
                    let action_name = match inline_ref.on_update {
                        aiondb_core::FkAction::SetDefault => "SET DEFAULT",
                        _ => "SET NULL",
                    };
                    return Err(DbError::bind_error(
                        SqlState::FeatureNotSupported,
                        format!(
                            "a column list with {action_name} is only supported for ON DELETE actions"
                        ),
                    ));
                }
                for target_col in &inline_ref.on_delete_set_columns {
                    if !column.name.eq_ignore_ascii_case(target_col) {
                        if !column_names
                            .iter()
                            .any(|name| name.eq_ignore_ascii_case(target_col))
                        {
                            return Err(DbError::bind_error(
                                SqlState::UndefinedColumn,
                                format!(
                                    "column \"{target_col}\" referenced in foreign key constraint does not exist"
                                ),
                            ));
                        }
                        return Err(DbError::bind_error(
                            SqlState::InvalidColumnReference,
                            format!(
                                "column \"{target_col}\" referenced in ON DELETE SET action must be part of foreign key"
                            ),
                        ));
                    }
                }
                // Self-referential FK: the table being created may legitimately
                // reference itself; defer resolution and use locally-known PK.
                let table_self_name = create_table
                    .name
                    .parts
                    .last()
                    .map(String::as_str)
                    .unwrap_or("");
                let ref_self_name = inline_ref
                    .ref_table
                    .parts
                    .last()
                    .map(String::as_str)
                    .unwrap_or("");
                if !table_self_name.is_empty()
                    && ref_self_name.eq_ignore_ascii_case(table_self_name)
                {
                    let resolved_ref_columns = if inline_ref.ref_columns.is_empty() {
                        // Use the table's own PK columns gathered so far (best-effort).
                        primary_key_columns.clone()
                    } else {
                        inline_ref.ref_columns.clone()
                    };
                    foreign_keys.push(BoundForeignKey {
                        columns: vec![column.name.clone()],
                        referenced_table: table_self_name.to_string(),
                        referenced_columns: resolved_ref_columns,
                        on_delete: inline_ref.on_delete,
                        on_update: inline_ref.on_update,
                        on_delete_set_columns: inline_ref.on_delete_set_columns.clone(),
                        on_update_set_columns: inline_ref.on_update_set_columns.clone(),
                        match_type: inline_ref.match_type,
                        name: None,
                    });
                    continue;
                }
                let ref_error_name = relation_error_name(&inline_ref.ref_table, default_schema)?;
                let (ref_name, ref_descriptor) = resolve_table_in_search_path(
                    self.catalog.as_ref(),
                    txn_id,
                    &inline_ref.ref_table,
                    default_schema,
                )?
                .ok_or_else(|| undefined_table(&inline_ref.ref_table, &ref_error_name))?;
                let resolved_ref_columns =
                    referenced_columns_or_primary_key(&ref_descriptor, &inline_ref.ref_columns);
                for ref_col in &resolved_ref_columns {
                    if ref_descriptor.column_by_name(ref_col).is_none() {
                        return Err(undefined_column(inline_ref.span.start + 1, ref_col));
                    }
                }
                foreign_keys.push(BoundForeignKey {
                    columns: vec![column.name.clone()],
                    referenced_table: ref_name.to_string(),
                    referenced_columns: resolved_ref_columns,
                    on_delete: inline_ref.on_delete,
                    on_update: inline_ref.on_update,
                    on_delete_set_columns: inline_ref.on_delete_set_columns.clone(),
                    on_update_set_columns: inline_ref.on_update_set_columns.clone(),
                    match_type: inline_ref.match_type,
                    name: None,
                });
            }
        }

        // Collect table-level constraints
        for constraint in &create_table.constraints {
            match constraint {
                aiondb_parser::TableConstraint::PrimaryKey { columns, .. } => {
                    primary_key_columns.clone_from(columns);
                }
                aiondb_parser::TableConstraint::Unique { name, columns, .. } => {
                    unique_constraints.push(BoundUniqueConstraint {
                        columns: columns.clone(),
                        name: name.clone(),
                    });
                }
                aiondb_parser::TableConstraint::Check { name, expr, .. } => {
                    let serialized = serialize_check_expr(expr)?;
                    check_constraints.push((name.clone(), serialized));
                }
                aiondb_parser::TableConstraint::ForeignKey {
                    name: fk_name,
                    columns,
                    ref_table,
                    ref_columns,
                    on_delete,
                    on_update,
                    on_delete_set_columns,
                    on_update_set_columns,
                    match_type,
                    span,
                    ..
                } => {
                    if !on_update_set_columns.is_empty() {
                        let action_name = match on_update {
                            aiondb_core::FkAction::SetDefault => "SET DEFAULT",
                            _ => "SET NULL",
                        };
                        return Err(DbError::bind_error(
                            SqlState::FeatureNotSupported,
                            format!(
                                "a column list with {action_name} is only supported for ON DELETE actions"
                            ),
                        ));
                    }
                    // Validate that FK columns exist in the new table.
                    // System columns (ctid/tableoid/xmin/xmax/cmin/cmax/oid)
                    // cannot be used in foreign keys; PG raises a dedicated
                    // error before complaining about a missing column.
                    for col in columns {
                        if is_system_column_name(col) {
                            return Err(DbError::bind_error(
                                aiondb_core::SqlState::FeatureNotSupported,
                                "system columns cannot be used in foreign keys",
                            ));
                        }
                        if !column_names.iter().any(|c| c.eq_ignore_ascii_case(col)) {
                            return Err(undefined_column(span.start + 1, col));
                        }
                    }
                    for target_col in on_delete_set_columns {
                        if !column_names
                            .iter()
                            .any(|name| name.eq_ignore_ascii_case(target_col))
                        {
                            return Err(DbError::bind_error(
                                SqlState::UndefinedColumn,
                                format!(
                                    "column \"{target_col}\" referenced in foreign key constraint does not exist"
                                ),
                            ));
                        }
                        if !columns
                            .iter()
                            .any(|col| col.eq_ignore_ascii_case(target_col))
                        {
                            return Err(DbError::bind_error(
                                SqlState::InvalidColumnReference,
                                format!(
                                    "column \"{target_col}\" referenced in ON DELETE SET action must be part of foreign key"
                                ),
                            ));
                        }
                    }

                    // Self-referential FK: the table being created can
                    // reference itself; defer catalog resolution and use the
                    // locally-known column list (the ones in `column_names`
                    // for column existence, plus the PK columns gathered so
                    // far when no ref columns were named).
                    let table_self_name = create_table
                        .name
                        .parts
                        .last()
                        .map(String::as_str)
                        .unwrap_or("");
                    let ref_self_name = ref_table.parts.last().map(String::as_str).unwrap_or("");
                    let is_self_ref = !table_self_name.is_empty()
                        && ref_self_name.eq_ignore_ascii_case(table_self_name);
                    let (ref_name_string, resolved_ref_columns): (String, Vec<String>) =
                        if is_self_ref {
                            let resolved = if ref_columns.is_empty() {
                                primary_key_columns.clone()
                            } else {
                                ref_columns.clone()
                            };
                            // Validate referenced columns exist locally (or PK).
                            for ref_col in &resolved {
                                if is_system_column_name(ref_col) {
                                    return Err(DbError::bind_error(
                                        aiondb_core::SqlState::FeatureNotSupported,
                                        "system columns cannot be used in foreign keys",
                                    ));
                                }
                                if !column_names.iter().any(|c| c.eq_ignore_ascii_case(ref_col)) {
                                    return Err(undefined_column(span.start + 1, ref_col));
                                }
                            }
                            (table_self_name.to_string(), resolved)
                        } else {
                            // Validate that the referenced table exists
                            let ref_error_name = relation_error_name(ref_table, default_schema)?;
                            let (ref_name, ref_descriptor) = resolve_table_in_search_path(
                                self.catalog.as_ref(),
                                txn_id,
                                ref_table,
                                default_schema,
                            )?
                            .ok_or_else(|| undefined_table(ref_table, &ref_error_name))?;
                            let resolved =
                                referenced_columns_or_primary_key(&ref_descriptor, ref_columns);

                            // Validate that the referenced columns exist; reject
                            // system columns first so the error matches PG.
                            for ref_col in &resolved {
                                if is_system_column_name(ref_col) {
                                    return Err(DbError::bind_error(
                                        aiondb_core::SqlState::FeatureNotSupported,
                                        "system columns cannot be used in foreign keys",
                                    ));
                                }
                                if ref_descriptor.column_by_name(ref_col).is_none() {
                                    return Err(undefined_column(span.start + 1, ref_col));
                                }
                            }
                            (ref_name.to_string(), resolved)
                        };

                    foreign_keys.push(BoundForeignKey {
                        columns: columns.clone(),
                        referenced_table: ref_name_string,
                        referenced_columns: resolved_ref_columns,
                        on_delete: *on_delete,
                        on_update: *on_update,
                        on_delete_set_columns: on_delete_set_columns.clone(),
                        on_update_set_columns: on_update_set_columns.clone(),
                        match_type: *match_type,
                        name: fk_name.clone(),
                    });
                }
            }
        }

        // Resolve inherited columns from INHERITS(parent, ...) clause.
        // For each parent table, copy its columns into the child table
        // (prepended before the child's own columns), mimicking PG
        // single-inheritance column flattening.
        let mut inherited_columns: Vec<BoundCreateColumn> = Vec::new();
        let mut merged_inherited_defaults: std::collections::HashMap<String, aiondb_parser::Expr> =
            std::collections::HashMap::new();
        let mut notices: Vec<String> = Vec::new();
        for parent_name in &create_table.inherits {
            let error_name = relation_error_name(parent_name, default_schema)?;
            let (_, parent_table) = resolve_table_in_search_path(
                self.catalog.as_ref(),
                txn_id,
                parent_name,
                default_schema,
            )?
            .ok_or_else(|| undefined_table(parent_name, &error_name))?;
            for col in &parent_table.columns {
                // Skip if the child already declares a column with the same name
                let child_has_column = create_table
                    .columns
                    .iter()
                    .any(|c| c.name.eq_ignore_ascii_case(&col.name));
                let already_inherited = inherited_columns
                    .iter()
                    .any(|c| c.name.eq_ignore_ascii_case(&col.name));
                let dominated = child_has_column || already_inherited;
                // Multiple parents contributing the same column (without an
                // override from the child) trigger PG's NOTICE about merging
                // multiple inherited definitions.
                if already_inherited && !child_has_column {
                    notices.push(format!(
                        "merging multiple inherited definitions of column \"{}\"",
                        col.name
                    ));
                }
                if child_has_column {
                    notices.push(format!(
                        "merging column \"{}\" with inherited definition",
                        col.name
                    ));
                    if let Some(default_expr) = col
                        .default_value
                        .as_ref()
                        .and_then(|sql| aiondb_parser::parse_expression(sql).ok())
                    {
                        merged_inherited_defaults
                            .entry(col.name.to_ascii_lowercase())
                            .or_insert(default_expr);
                    }
                }
                if !dominated {
                    // Re-parse the parent's serialized default expression
                    // (if any) back into an AST Expr so it flows through
                    // type-checking normally.
                    let default_expr = col
                        .default_value
                        .as_ref()
                        .and_then(|sql| aiondb_parser::parse_expression(sql).ok());
                    inherited_columns.push(BoundCreateColumn {
                        name: col.name.clone(),
                        data_type: col.data_type.clone(),
                        raw_type_name: col.raw_type_name.clone(),
                        text_type_modifier: col.text_type_modifier,
                        nullable: col.nullable,
                        default: default_expr,
                        identity: None,
                    });
                }
            }
        }

        // Build final column list: inherited columns first, then child's own.
        let table_name = create_table.name.parts.last().map_or("", String::as_str);
        let schema_name = if create_table.name.parts.len() > 1 {
            create_table.name.parts.first().map(String::as_str)
        } else {
            None
        };
        let mut bound_columns: Vec<BoundCreateColumn> = inherited_columns;
        for column in &create_table.columns {
            let default = if column.identity.is_some() && column.default.is_none() {
                // GENERATED {ALWAYS|BY DEFAULT} AS IDENTITY: synthesize a
                // nextval('tablename_colname_seq') default so the column
                // auto-increments through the normal sequence machinery.
                let seq_base_name = format!("{}_{}_seq", table_name, column.name);
                let seq_name = schema_name.map_or(seq_base_name.clone(), |schema_name| {
                    format!("{schema_name}.{seq_base_name}")
                });
                let escaped_seq_name = seq_name.replace('\'', "''");
                Some(
                    aiondb_parser::parse_expression(&format!("nextval('{escaped_seq_name}')"))
                        .map_err(|err| {
                            DbError::internal(format!(
                                "failed to parse identity default expression: {err}"
                            ))
                        })?,
                )
            } else {
                column.default.clone().or_else(|| {
                    merged_inherited_defaults
                        .get(&column.name.to_ascii_lowercase())
                        .cloned()
                })
            };
            let (resolved_data_type, resolved_modifier) = resolve_domain_column_type(
                column.raw_type_name.as_deref(),
                column.data_type.clone(),
                column.text_type_modifier,
            );
            bound_columns.push(BoundCreateColumn {
                name: column.name.clone(),
                data_type: resolved_data_type.clone(),
                raw_type_name: column.raw_type_name.clone(),
                text_type_modifier: resolved_modifier,
                nullable: column.nullable,
                default,
                identity: column.identity.clone(),
            });

            if let Some(geom_type) = geometric_compat_type_name(column.raw_type_name.as_deref()) {
                let quoted_column = aiondb_parser::identifier::quote_identifier(&column.name);
                let check_sql = format!(
                    "{quoted_column} IS NULL OR __aiondb_compat_cast({quoted_column}, 'text', '{geom_type}') IS NOT NULL"
                );
                check_constraints.push((None, check_sql));
            } else if let Some(enum_name) = enum_compat_type_name(column.raw_type_name.as_deref()) {
                // ENUM type: route assignments through `__aiondb_compat_cast`
                // so unknown labels surface PG's `invalid_text_representation`
                // SQLSTATE (22P02) instead of a generic CHECK violation.
                let quoted_column = aiondb_parser::identifier::quote_identifier(&column.name);
                let escaped_target = aiondb_core::escape_sql_literal(&enum_name);
                let check_sql = format!(
                    "__aiondb_compat_cast({quoted_column}, 'text', '{escaped_target}') IS NOT NULL OR {quoted_column} IS NULL"
                );
                check_constraints.push((None, check_sql));
            } else if let Some(domain_name) =
                domain_compat_type_name(column.raw_type_name.as_deref())
            {
                // Funnel every assignment to a domain-typed column through
                // `__aiondb_compat_cast`, which walks the domain chain and
                // raises CheckViolation / NotNullViolation on the first
                // violation. The trailing `OR col IS NULL` only fires for
                // domains that allow NULL - for NOT NULL domains the cast
                // raises before the OR is reached.
                let quoted_column = aiondb_parser::identifier::quote_identifier(&column.name);
                let source_type = aiondb_eval::compat_type_name_for_data_type(&resolved_data_type);
                let escaped_domain = aiondb_core::escape_sql_literal(&domain_name);
                let escaped_source = aiondb_core::escape_sql_literal(&source_type);
                let check_sql = format!(
                    "__aiondb_compat_cast({quoted_column}, '{escaped_source}', '{escaped_domain}') IS NOT NULL OR {quoted_column} IS NULL"
                );
                check_constraints.push((None, check_sql));
            }
        }

        let shard_key_columns =
            extract_and_validate_shard_key(&create_table.storage_params, &bound_columns)?;
        let shard_count = extract_and_validate_shard_count(&create_table.storage_params)?;

        Ok(BoundCreateTable {
            relation_name: qualified_create_table_name(
                &create_table.name,
                default_schema,
                create_table.temporary,
            )?,
            columns: bound_columns,
            typed_table_of: None,
            primary_key_columns,
            unique_constraints,
            foreign_keys,
            check_constraints,
            shard_key_columns,
            shard_count,
            notices,
        })
    }

    pub(super) fn bind_create_sequence(
        &self,
        create_sequence: &CreateSequenceStatement,
        default_schema: Option<&str>,
    ) -> DbResult<BoundCreateSequence> {
        Ok(BoundCreateSequence {
            sequence_name: qualified_name_with_default(&create_sequence.name, default_schema)?,
        })
    }

    pub(super) fn bind_create_index(
        &self,
        create_index: &CreateIndexStatement,
        txn_id: TxnId,
        default_schema: Option<&str>,
    ) -> DbResult<BoundCreateIndex> {
        if let Some(method) = create_index.method {
            if !matches!(
                method,
                aiondb_parser::IndexMethod::BTree
                    | aiondb_parser::IndexMethod::Hnsw
                    | aiondb_parser::IndexMethod::IvfFlat
                    | aiondb_parser::IndexMethod::Gin
                    | aiondb_parser::IndexMethod::Gist
                    | aiondb_parser::IndexMethod::SpGist
                    | aiondb_parser::IndexMethod::Brin
                    | aiondb_parser::IndexMethod::Hash
            ) {
                return Err(DbError::feature_not_supported(format!(
                    "index method '{}' is not supported",
                    index_method_name(method)
                )));
            }
        }

        // Validate fillfactor bounds before proceeding.
        for opt in &create_index.with_options {
            if opt.key.eq_ignore_ascii_case("fillfactor") {
                let value = opt.as_integer().ok_or_else(|| {
                    DbError::bind_error(
                        SqlState::InvalidParameterValue,
                        "option \"fillfactor\" requires an integer value",
                    )
                })?;
                if !(10..=100).contains(&value) {
                    return Err(DbError::bind_error(
                        SqlState::InvalidParameterValue,
                        format!("value {value} out of bounds for option \"fillfactor\""),
                    )
                    .with_client_detail(
                        "Valid values are between \"10\" and \"100\".".to_owned(),
                    ));
                }
            }
        }

        let relation_error_name = relation_error_name(&create_index.table, default_schema)?;
        let (_, relation) = resolve_table_in_search_path(
            self.catalog.as_ref(),
            txn_id,
            &create_index.table,
            default_schema,
        )?
        .ok_or_else(|| undefined_table(&create_index.table, &relation_error_name))?;
        let index_name = if create_index.name.parts.len() == 1 {
            QualifiedName::new(
                relation.name.schema_name(),
                create_index.name.parts.last().map_or("", String::as_str),
            )
        } else {
            qualified_name_with_default(&create_index.name, default_schema)?
        };

        let mut saw_expr_row_record = false;
        let mut saw_expr_system_col = false;
        let mut key_columns = Vec::new();
        let mut key_expressions = Vec::new();
        for (idx, column_name) in create_index.columns.iter().enumerate() {
            let maybe_expression = create_index
                .key_expressions
                .get(idx)
                .and_then(|expr| expr.as_ref());
            if let Some(expression_sql) = maybe_expression {
                let marker = column_name.parts.last().map_or("", String::as_str);
                if marker == "__expr_row_record__" {
                    saw_expr_row_record = true;
                } else if marker == "__expr_system_col__" {
                    saw_expr_system_col = true;
                }
                if create_index.method == Some(aiondb_parser::IndexMethod::Gin) {
                    if let Some(column) = extract_to_tsvector_column(expression_sql, &relation) {
                        key_columns.push(column);
                        continue;
                    }
                }
                key_expressions.push(expression_sql.clone());
                continue;
            }

            let column = column_name.parts.last().map_or("", String::as_str);
            key_columns.push(
                relation
                    .column_by_name(column)
                    .cloned()
                    .ok_or_else(|| undefined_column(column_name.span.start + 1, column))?,
            );
        }

        if saw_expr_row_record {
            return Err(DbError::bind_error(
                SqlState::DatatypeMismatch,
                "column \"row\" has pseudo-type record",
            ));
        }
        if saw_expr_system_col {
            return Err(DbError::feature_not_supported(
                "index creation on system columns is not supported",
            ));
        }
        if key_columns.is_empty() && key_expressions.is_empty() && !create_index.columns.is_empty()
        {
            return Err(DbError::feature_not_supported(
                "expression indexes are not supported",
            ));
        }
        if key_columns.is_empty() && !create_index.columns.is_empty() {
            // Expression-only indexes are supported by the executor runtime.
        }

        let vector_ann_method = matches!(
            create_index.method,
            Some(aiondb_parser::IndexMethod::Hnsw | aiondb_parser::IndexMethod::IvfFlat)
        );
        let hnsw_params = if vector_ann_method {
            let mut opts = aiondb_plan::HnswPlanOptions::default();
            if let Some(metric) = create_index
                .operator_classes
                .iter()
                .filter_map(|class| class.as_deref())
                .find_map(parse_vector_operator_class_metric)
            {
                opts.distance_metric = metric;
            }
            for opt in &create_index.with_options {
                match opt.key.to_lowercase().as_str() {
                    "m" => {
                        let raw = opt.as_integer().ok_or_else(|| {
                            DbError::bind_error(
                                SqlState::InvalidParameterValue,
                                "option \"m\" requires an integer value",
                            )
                        })?;
                        let parsed = u32::try_from(raw).map_err(|_| {
                            DbError::bind_error(
                                SqlState::InvalidParameterValue,
                                format!("value {raw} out of range for option \"m\""),
                            )
                        })?;
                        if !(2..=1024).contains(&parsed) {
                            return Err(DbError::bind_error(
                                SqlState::InvalidParameterValue,
                                format!("value {raw} out of bounds for option \"m\""),
                            )
                            .with_client_detail(
                                "Valid values are between \"2\" and \"1024\".".to_owned(),
                            ));
                        }
                        opts.m = parsed;
                    }
                    "ef_construction" => {
                        let raw = opt.as_integer().ok_or_else(|| {
                            DbError::bind_error(
                                SqlState::InvalidParameterValue,
                                "option \"ef_construction\" requires an integer value",
                            )
                        })?;
                        let parsed = u32::try_from(raw).map_err(|_| {
                            DbError::bind_error(
                                SqlState::InvalidParameterValue,
                                format!("value {raw} out of range for option \"ef_construction\""),
                            )
                        })?;
                        if parsed == 0 || parsed > 65_536 {
                            return Err(DbError::bind_error(
                                SqlState::InvalidParameterValue,
                                format!("value {raw} out of bounds for option \"ef_construction\""),
                            )
                            .with_client_detail(
                                "Valid values are between \"1\" and \"65536\".".to_owned(),
                            ));
                        }
                        opts.ef_construction = parsed;
                    }
                    "distance" | "distance_metric" => {
                        let s = opt.as_string().ok_or_else(|| {
                            DbError::bind_error(
                                SqlState::InvalidParameterValue,
                                "option \"distance\" requires a string value",
                            )
                        })?;
                        opts.distance_metric = parse_distance_option(s)?;
                    }
                    "quantization" => {
                        let s = opt.as_string().ok_or_else(|| {
                            DbError::bind_error(
                                SqlState::InvalidParameterValue,
                                "option \"quantization\" requires a string value",
                            )
                        })?;
                        opts.quantization = parse_quantization_option(s)?;
                    }
                    "prenormalised" | "prenormalized" | "normalized" | "normalised" => {
                        let parsed = if let Some(n) = opt.as_integer() {
                            match n {
                                0 => false,
                                1 => true,
                                _ => {
                                    return Err(DbError::bind_error(
                                        SqlState::InvalidParameterValue,
                                        format!(
                                            "option \"prenormalised\" only accepts 0/1 or \
                                             true/false, got {n}"
                                        ),
                                    ));
                                }
                            }
                        } else if let Some(s) = opt.as_string() {
                            match s.to_ascii_lowercase().as_str() {
                                "true" | "on" | "yes" | "t" => true,
                                "false" | "off" | "no" | "f" => false,
                                other => {
                                    return Err(DbError::bind_error(
                                        SqlState::InvalidParameterValue,
                                        format!(
                                            "option \"prenormalised\" expects a boolean, got \
                                             {other:?}"
                                        ),
                                    ));
                                }
                            }
                        } else {
                            return Err(DbError::bind_error(
                                SqlState::InvalidParameterValue,
                                "option \"prenormalised\" expects a boolean value",
                            ));
                        };
                        opts.prenormalised = parsed;
                    }
                    "lists" if create_index.method == Some(aiondb_parser::IndexMethod::IvfFlat) => {
                        let raw = opt.as_integer().ok_or_else(|| {
                            DbError::bind_error(
                                SqlState::InvalidParameterValue,
                                "option \"lists\" requires an integer value",
                            )
                        })?;
                        if raw <= 0 {
                            return Err(DbError::bind_error(
                                SqlState::InvalidParameterValue,
                                format!("value {raw} out of bounds for option \"lists\""),
                            )
                            .with_client_detail(
                                "Valid values are greater than \"0\".".to_owned(),
                            ));
                        }
                    }
                    _ => {}
                }
            }
            Some(opts)
        } else {
            None
        };

        // Compatibility fallback: accept `USING gin` on unsupported key types
        // (e.g. arrays in pg_regress create_index) by creating a regular
        // B-Tree-backed index instead of failing during bind/type-check.
        let gin = create_index.method == Some(aiondb_parser::IndexMethod::Gin)
            && key_columns.len() == 1
            && matches!(key_columns[0].data_type, DataType::Jsonb | DataType::Text);

        Ok(BoundCreateIndex {
            index_name,
            relation,
            key_columns,
            key_expressions,
            hnsw_params,
            gin,
            unique: create_index.unique,
            nulls_not_distinct: create_index.nulls_not_distinct,
            concurrently: create_index.concurrently,
        })
    }

    pub(super) fn bind_truncate_table(
        &self,
        truncate_table: &TruncateTableStatement,
        txn_id: TxnId,
        default_schema: Option<&str>,
    ) -> DbResult<BoundTruncateTable> {
        let relation_error_name = relation_error_name(&truncate_table.name, default_schema)?;
        let (_, relation) = resolve_table_in_search_path(
            self.catalog.as_ref(),
            txn_id,
            &truncate_table.name,
            default_schema,
        )?
        .ok_or_else(|| undefined_table(&truncate_table.name, &relation_error_name))?;

        Ok(BoundTruncateTable { relation })
    }

    pub(super) fn bind_drop_table(
        &self,
        drop_table: &DropTableStatement,
        txn_id: TxnId,
        default_schema: Option<&str>,
    ) -> DbResult<BoundDropTable> {
        let relation_error_name = relation_error_name(&drop_table.name, default_schema)?;
        let (_, relation) = resolve_table_in_search_path(
            self.catalog.as_ref(),
            txn_id,
            &drop_table.name,
            default_schema,
        )?
        .ok_or_else(|| {
            DbError::bind_error(
                SqlState::UndefinedTable,
                format!("table \"{relation_error_name}\" does not exist"),
            )
            .with_position(drop_table.name.span.start + 1)
        })?;

        Ok(BoundDropTable {
            relation,
            cascade: drop_table.cascade,
        })
    }

    pub(super) fn bind_drop_index(
        &self,
        drop_index: &DropIndexStatement,
        txn_id: TxnId,
        default_schema: Option<&str>,
    ) -> DbResult<BoundDropIndex> {
        let mut indexes = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for name in std::iter::once(&drop_index.name).chain(drop_index.extra_names.iter()) {
            let index_name = relation_error_name(name, default_schema)?;
            let mut found: Option<IndexDescriptor> = None;
            for candidate in relation_lookup_candidates(name, default_schema)? {
                let schema_name = candidate
                    .schema_name()
                    .unwrap_or(default_schema.unwrap_or("public"));
                let Some(schema) = self
                    .catalog
                    .get_schema(txn_id, &QualifiedName::unqualified(schema_name))?
                else {
                    continue;
                };

                for table in self.catalog.list_tables(txn_id, schema.schema_id)? {
                    for index in self.catalog.list_indexes(txn_id, table.table_id)? {
                        if index.schema_id == schema.schema_id
                            && index
                                .name
                                .name
                                .eq_ignore_ascii_case(candidate.object_name())
                        {
                            found = Some(index);
                            break;
                        }
                    }
                    if found.is_some() {
                        break;
                    }
                }
                if found.is_some() {
                    break;
                }
            }

            match found {
                Some(index) => {
                    if seen.insert(index.index_id) {
                        indexes.push(index);
                    }
                }
                None if !drop_index.if_exists => return Err(undefined_index(&index_name)),
                None => {}
            }
        }

        Ok(BoundDropIndex { indexes })
    }

    pub(super) fn bind_drop_sequence(
        &self,
        drop_sequence: &DropSequenceStatement,
        txn_id: TxnId,
        default_schema: Option<&str>,
    ) -> DbResult<BoundDropSequence> {
        let sequence_name = relation_error_name(&drop_sequence.name, default_schema)?;
        let (_, sequence) = resolve_sequence_in_search_path(
            self.catalog.as_ref(),
            txn_id,
            &drop_sequence.name,
            default_schema,
        )?
        .ok_or_else(|| undefined_sequence(&sequence_name))?;

        Ok(BoundDropSequence { sequence })
    }

    pub(super) fn bind_alter_table(
        &self,
        alter_table: &AlterTableStatement,
        txn_id: TxnId,
        default_schema: Option<&str>,
    ) -> DbResult<BoundAlterTable> {
        let relation_error_name = relation_error_name(&alter_table.table, default_schema)?;
        let resolved = resolve_table_in_search_path(
            self.catalog.as_ref(),
            txn_id,
            &alter_table.table,
            default_schema,
        )?;
        let (_, relation) = match resolved {
            Some(r) => r,
            None => {
                // If the target name resolves to a view, PostgreSQL errors with
                // "ALTER action ... cannot be performed on relation \"X\"" /
                // "DETAIL: This operation is not supported for views."
                if let Some((view_qname, _)) = resolve_view_in_search_path(
                    self.catalog.as_ref(),
                    txn_id,
                    &alter_table.table,
                    default_schema,
                )? {
                    let action_label = alter_table_action_label(&alter_table.action);
                    return Err(DbError::feature_not_supported(format!(
                        "ALTER action {action_label} cannot be performed on relation \"{}\"",
                        view_qname.object_name()
                    ))
                    .with_client_detail("This operation is not supported for views.".to_owned()));
                }
                return Err(undefined_table(&alter_table.table, &relation_error_name));
            }
        };

        let is_typed_table = self
            .catalog
            .get_table_type_name(txn_id, relation.table_id)?
            .is_some();
        if is_typed_table {
            match &alter_table.action {
                AlterTableAction::AddColumn { .. } => {
                    return Err(DbError::bind_error(
                        SqlState::FeatureNotSupported,
                        "cannot add column to typed tables",
                    ));
                }
                AlterTableAction::DropColumn { .. } => {
                    return Err(DbError::bind_error(
                        SqlState::FeatureNotSupported,
                        "cannot drop column from typed tables",
                    ));
                }
                AlterTableAction::RenameColumn { .. } => {
                    return Err(DbError::bind_error(
                        SqlState::FeatureNotSupported,
                        "cannot rename column of typed tables",
                    ));
                }
                AlterTableAction::AlterColumnType { .. } => {
                    return Err(DbError::bind_error(
                        SqlState::FeatureNotSupported,
                        "cannot alter column type of typed tables",
                    ));
                }
                _ => {}
            }
        }

        match &alter_table.action {
            AlterTableAction::AddColumn {
                column: col_def,
                if_not_exists,
            } => {
                if is_system_column_name(&col_def.name) {
                    return Err(DbError::bind_error(
                        SqlState::DuplicateColumn,
                        format!(
                            "column name \"{}\" conflicts with a system column name",
                            col_def.name
                        ),
                    ));
                }
                if relation.column_by_name(&col_def.name).is_some() {
                    if *if_not_exists {
                        return Ok(BoundAlterTable::NoOp);
                    }
                    return Err(duplicate_column(col_def.span.start + 1, &col_def.name));
                }

                let (resolved_data_type, resolved_modifier) = resolve_domain_column_type(
                    col_def.raw_type_name.as_deref(),
                    col_def.data_type.clone(),
                    col_def.text_type_modifier,
                );
                Ok(BoundAlterTable::AddColumn(BoundAlterTableAddColumn {
                    relation,
                    column_def: BoundCreateColumn {
                        name: col_def.name.clone(),
                        data_type: resolved_data_type,
                        raw_type_name: col_def.raw_type_name.clone(),
                        text_type_modifier: resolved_modifier,
                        nullable: col_def.nullable,
                        default: col_def.default.clone(),
                        identity: col_def.identity.clone(),
                    },
                }))
            }
            AlterTableAction::DropColumn {
                name,
                if_exists,
                span,
            } => {
                let column = match relation.column_by_name(name).cloned() {
                    Some(c) => c,
                    None if *if_exists => return Ok(BoundAlterTable::NoOp),
                    None => {
                        if is_system_column_name(name) {
                            return Err(DbError::bind_error(
                                SqlState::FeatureNotSupported,
                                format!("cannot drop system column \"{name}\""),
                            )
                            .with_position(span.start + 1));
                        }
                        return Err(DbError::bind_error(
                            SqlState::UndefinedColumn,
                            format!(
                                "column \"{}\" of relation \"{}\" does not exist",
                                name,
                                relation.name.object_name()
                            ),
                        )
                        .with_position(span.start + 1));
                    }
                };

                Ok(BoundAlterTable::DropColumn(BoundAlterTableDropColumn {
                    relation,
                    column,
                }))
            }
            AlterTableAction::RenameTable { new_name, .. } => {
                Ok(BoundAlterTable::RenameTable(BoundAlterTableRename {
                    relation,
                    new_name: new_name.clone(),
                }))
            }
            AlterTableAction::RenameColumn {
                old_name,
                new_name,
                span,
            } => {
                let old_column = require_column(&relation, old_name, span.start)?;

                Ok(BoundAlterTable::RenameColumn(BoundAlterTableRenameColumn {
                    relation,
                    old_column,
                    new_name: new_name.clone(),
                }))
            }
            AlterTableAction::SetDefault {
                column,
                default,
                span,
            } => {
                let col = require_column(&relation, column, span.start)?;

                Ok(BoundAlterTable::SetDefault(BoundAlterTableSetDefault {
                    relation,
                    column: col,
                    default: default.clone(),
                }))
            }
            AlterTableAction::DropDefault { column, span } => {
                let col = require_column(&relation, column, span.start)?;

                Ok(BoundAlterTable::DropDefault(BoundAlterTableDropDefault {
                    relation,
                    column: col,
                }))
            }
            AlterTableAction::SetNotNull { column, span } => {
                let col = require_column(&relation, column, span.start)?;

                Ok(BoundAlterTable::SetNotNull(BoundAlterTableSetNotNull {
                    relation,
                    column: col,
                }))
            }
            AlterTableAction::DropNotNull { column, span } => {
                let col = require_column(&relation, column, span.start)?;

                Ok(BoundAlterTable::DropNotNull(BoundAlterTableDropNotNull {
                    relation,
                    column: col,
                }))
            }
            AlterTableAction::AddConstraint { constraint, .. } => Ok(
                BoundAlterTable::AddConstraint(BoundAlterTableAddConstraint {
                    relation,
                    constraint: constraint.clone(),
                }),
            ),
            AlterTableAction::DropConstraint { name, .. } => Ok(BoundAlterTable::DropConstraint(
                BoundAlterTableDropConstraint {
                    relation,
                    constraint_name: name.clone(),
                },
            )),
            AlterTableAction::AlterColumnType {
                column_name,
                new_type,
                raw_type_name,
                text_type_modifier,
                span,
            } => {
                let col = require_column(&relation, column_name, span.start)?;

                Ok(BoundAlterTable::AlterColumnType(
                    BoundAlterTableAlterColumnType {
                        relation,
                        column: col,
                        new_type: new_type.clone(),
                        raw_type_name: raw_type_name.clone(),
                        text_type_modifier: *text_type_modifier,
                    },
                ))
            }
            AlterTableAction::RenameConstraint { .. } => Err(DbError::bind_error(
                SqlState::FeatureNotSupported,
                "ALTER TABLE ... RENAME CONSTRAINT is not supported",
            )),
        }
    }

    pub(super) fn bind_create_role(&self, s: &CreateRoleStatement) -> DbResult<BoundCreateRole> {
        if !s.in_roles.is_empty() || !s.role_members.is_empty() || !s.admin_members.is_empty() {
            return Err(DbError::bind_error(
                SqlState::FeatureNotSupported,
                "CREATE ROLE membership clauses (IN ROLE/ROLE/ADMIN/USER) are not supported",
            ));
        }

        Ok(BoundCreateRole {
            name: s.name.clone(),
            options: s.options.clone(),
        })
    }

    pub(super) fn bind_drop_role(&self, s: &DropRoleStatement) -> DbResult<BoundDropRole> {
        Ok(BoundDropRole {
            name: s.name.clone(),
        })
    }

    pub(super) fn bind_alter_role(
        &self,
        s: &AlterRoleStatement,
        txn_id: TxnId,
    ) -> DbResult<BoundAlterRole> {
        let current_role = self
            .catalog
            .get_role(txn_id, &s.name)?
            .ok_or_else(|| undefined_role(&s.name))?;

        Ok(BoundAlterRole {
            name: s.name.clone(),
            current_role,
            options: s.options.clone(),
        })
    }

    pub(super) fn bind_grant(
        &self,
        s: &GrantStatement,
        txn_id: TxnId,
        default_schema: Option<&str>,
    ) -> DbResult<BoundGrant> {
        self.validate_acl_target_exists(&s.target, txn_id, default_schema)?;
        Ok(BoundGrant {
            privileges: s.privileges.clone(),
            target: s.target.clone(),
            role_name: s.role_name.clone(),
        })
    }

    pub(super) fn bind_analyze(
        &self,
        table: Option<&aiondb_parser::ObjectName>,
        txn_id: TxnId,
        default_schema: Option<&str>,
    ) -> DbResult<BoundAnalyze> {
        match table {
            Some(name) => {
                let relation_error_name = relation_error_name(name, default_schema)?;
                let (_, relation) = resolve_table_in_search_path(
                    self.catalog.as_ref(),
                    txn_id,
                    name,
                    default_schema,
                )?
                .ok_or_else(|| undefined_table(name, &relation_error_name))?;
                Ok(BoundAnalyze {
                    table_id: relation.table_id,
                })
            }
            None => {
                // Bare `ANALYZE` (no table) is a whole-database stat refresh
                // in PG. AionDB has no per-database statistics yet, so emit a
                // RelationId(0) sentinel that the executor short-circuits as
                // a no-op success — matching PG's "ANALYZE" tag.
                Ok(BoundAnalyze {
                    table_id: RelationId::new(0),
                })
            }
        }
    }

    pub(super) fn bind_vacuum(
        &self,
        table: Option<&aiondb_parser::ObjectName>,
        txn_id: TxnId,
        default_schema: Option<&str>,
    ) -> DbResult<BoundVacuum> {
        match table {
            Some(name) => {
                let relation_error_name = relation_error_name(name, default_schema)?;
                let (_, relation) = resolve_table_in_search_path(
                    self.catalog.as_ref(),
                    txn_id,
                    name,
                    default_schema,
                )?
                .ok_or_else(|| undefined_table(name, &relation_error_name))?;
                Ok(BoundVacuum {
                    table_id: relation.table_id,
                })
            }
            None => Ok(BoundVacuum {
                // Bare VACUUM no-op sentinel; see bind_analyze comment.
                table_id: RelationId::new(0),
            }),
        }
    }

    pub(super) fn bind_revoke(
        &self,
        s: &RevokeStatement,
        txn_id: TxnId,
        default_schema: Option<&str>,
    ) -> DbResult<BoundRevoke> {
        self.validate_acl_target_exists(&s.target, txn_id, default_schema)?;
        Ok(BoundRevoke {
            privileges: s.privileges.clone(),
            target: s.target.clone(),
            role_name: s.role_name.clone(),
        })
    }

    fn validate_acl_target_exists(
        &self,
        target: &aiondb_parser::GrantTarget,
        txn_id: TxnId,
        default_schema: Option<&str>,
    ) -> DbResult<()> {
        match target {
            aiondb_parser::GrantTarget::Table(name) => {
                let relation_name = relation_error_name(name, default_schema)?;
                let table = resolve_table_in_search_path(
                    self.catalog.as_ref(),
                    txn_id,
                    name,
                    default_schema,
                )?;
                if table.is_none()
                    && resolve_view_in_search_path(
                        self.catalog.as_ref(),
                        txn_id,
                        name,
                        default_schema,
                    )?
                    .is_none()
                {
                    return Err(undefined_table(name, &relation_name));
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn bind_lock(
        &self,
        lock: &LockStatement,
        txn_id: TxnId,
        default_schema: Option<&str>,
    ) -> DbResult<super::BoundLock> {
        let mut table_ids = Vec::with_capacity(lock.tables.len());
        for name in &lock.tables {
            if lock_target_is_pg_catalog_table(name) {
                return Err(DbError::feature_not_supported(
                    "unsupported compatibility command: LOCK",
                ));
            }
            let relation_error_name = relation_error_name(name, default_schema)?;
            let (_, relation) =
                resolve_table_in_search_path(self.catalog.as_ref(), txn_id, name, default_schema)?
                    .ok_or_else(|| undefined_table(name, &relation_error_name))?;
            table_ids.push(relation.table_id);
        }
        Ok(super::BoundLock {
            table_ids,
            mode: lock.mode,
            nowait: lock.nowait,
        })
    }
}

fn extract_to_tsvector_column(
    expression_sql: &str,
    relation: &TableDescriptor,
) -> Option<ColumnDescriptor> {
    let expr = aiondb_parser::parse_expression(expression_sql).ok()?;
    let aiondb_parser::Expr::FunctionCall { name, args, .. } = expr else {
        return None;
    };
    if !name
        .parts
        .last()
        .is_some_and(|part| part.eq_ignore_ascii_case("to_tsvector"))
    {
        return None;
    }
    let column_arg = match args.as_slice() {
        [arg] => arg,
        [_, arg] => arg,
        _ => return None,
    };
    let aiondb_parser::Expr::Identifier(column_name) = column_arg else {
        return None;
    };
    let column = column_name.parts.last()?;
    relation.column_by_name(column).cloned()
}

fn lock_target_is_pg_catalog_table(name: &ObjectName) -> bool {
    match name.parts.as_slice() {
        [schema, table] => {
            crate::pg_catalog::is_pg_catalog(schema)
                && crate::pg_catalog::is_pg_catalog_table(table)
        }
        [table] => crate::pg_catalog::is_pg_catalog_table(table),
        _ => false,
    }
}

/// Check whether the function name refers to a known aggregate function.
fn is_aggregate_function(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "avg"
            | "sum"
            | "count"
            | "min"
            | "max"
            | "array_agg"
            | "string_agg"
            | "bool_and"
            | "bool_or"
            | "every"
            | "bit_and"
            | "bit_or"
            | "corr"
            | "covar_pop"
            | "covar_samp"
            | "regr_avgx"
            | "regr_avgy"
            | "regr_count"
            | "regr_intercept"
            | "regr_r2"
            | "regr_slope"
            | "regr_sxx"
            | "regr_sxy"
            | "regr_syy"
            | "stddev"
            | "stddev_pop"
            | "stddev_samp"
            | "variance"
            | "var_pop"
            | "var_samp"
            | "xmlagg"
            | "json_agg"
            | "jsonb_agg"
            | "json_object_agg"
            | "jsonb_object_agg"
            | "mode"
            | "percentile_cont"
            | "percentile_disc"
    )
}

/// Check whether the function name refers to a known set-returning function.
fn is_set_returning_function(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "generate_series"
            | "generate_subscripts"
            | "unnest"
            | "regexp_matches"
            | "regexp_split_to_table"
            | "json_each"
            | "json_each_text"
            | "json_array_elements"
            | "json_array_elements_text"
            | "json_object_keys"
            | "jsonb_each"
            | "jsonb_each_text"
            | "jsonb_array_elements"
            | "jsonb_array_elements_text"
            | "jsonb_object_keys"
            | "ts_stat"
            | "ts_token_type"
            | "ts_parse"
            | "aclexplode"
            | "json_to_recordset"
            | "jsonb_to_recordset"
    )
}

/// Validate that a DEFAULT expression does not contain aggregates, SRFs,
/// or column references.
fn validate_default_expr(expr: &Expr) -> DbResult<()> {
    validate_default_expr_inner(expr, false)
}

#[allow(clippy::only_used_in_recursion)]
fn validate_default_expr_inner(expr: &Expr, inside_agg: bool) -> DbResult<()> {
    match expr {
        Expr::Identifier(name) => {
            // Column references are not allowed in DEFAULT expressions, but
            // SQL-standard niladic functions parsed as bare identifiers
            // (CURRENT_TIMESTAMP, CURRENT_DATE, NOW, …) and the boolean
            // /null literal aliases are fine. Without this allow-list every
            // PG-shaped table that defaults a timestamp column to
            // CURRENT_TIMESTAMP fails to bind.
            if !name.parts.is_empty() {
                let first = name.parts[0].to_ascii_lowercase();
                let is_constant = matches!(first.as_str(), "true" | "false" | "null");
                let is_niladic_fn = matches!(
                    first.as_str(),
                    "current_timestamp"
                        | "current_date"
                        | "current_time"
                        | "localtimestamp"
                        | "localtime"
                        | "current_user"
                        | "session_user"
                        | "current_role"
                        | "current_catalog"
                        | "current_schema"
                        | "user"
                        | "now"
                );
                if !is_constant && !is_niladic_fn {
                    return Err(DbError::bind_error(
                        SqlState::InvalidTableDefinition,
                        "cannot use column reference in DEFAULT expression",
                    )
                    .with_position(name.span.start + 1));
                }
            }
            Ok(())
        }
        Expr::FunctionCall {
            name, args, span, ..
        } => {
            let func_name = name.parts.last().map_or("", String::as_str);
            if is_aggregate_function(func_name) {
                // Check if any argument is a column reference first
                for arg in args {
                    if let Expr::Identifier(ref id_name) = arg {
                        if !id_name.parts.is_empty() {
                            let first = &id_name.parts[0];
                            if !first.eq_ignore_ascii_case("true")
                                && !first.eq_ignore_ascii_case("false")
                                && !first.eq_ignore_ascii_case("null")
                            {
                                return Err(DbError::bind_error(
                                    SqlState::InvalidTableDefinition,
                                    "cannot use column reference in DEFAULT expression",
                                )
                                .with_position(id_name.span.start + 1));
                            }
                        }
                    }
                }
                return Err(DbError::bind_error(
                    SqlState::GroupingError,
                    "aggregate functions are not allowed in DEFAULT expressions",
                )
                .with_position(span.start + 1));
            }
            if is_set_returning_function(func_name) {
                return Err(DbError::bind_error(
                    SqlState::FeatureNotSupported,
                    "set-returning functions are not allowed in DEFAULT expressions",
                )
                .with_position(span.start + 1));
            }
            for arg in args {
                validate_default_expr_inner(arg, inside_agg)?;
            }
            Ok(())
        }
        Expr::BinaryOp { left, right, .. } => {
            validate_default_expr_inner(left, inside_agg)?;
            validate_default_expr_inner(right, inside_agg)
        }
        Expr::UnaryOp { expr: inner, .. } => validate_default_expr_inner(inner, inside_agg),
        Expr::Cast { expr: inner, .. } => validate_default_expr_inner(inner, inside_agg),
        Expr::Subquery { span, .. }
        | Expr::InSubquery { span, .. }
        | Expr::ArraySubquery { span, .. }
        | Expr::Exists { span, .. } => Err(DbError::bind_error(
            SqlState::FeatureNotSupported,
            "cannot use subquery in DEFAULT expression",
        )
        .with_position(span.start + 1)),
        _ => Ok(()),
    }
}

fn qualified_create_table_name(
    name: &ObjectName,
    default_schema: Option<&str>,
    temporary: bool,
) -> DbResult<QualifiedName> {
    if temporary {
        if let [relation] = name.parts.as_slice() {
            return Ok(QualifiedName::qualified(PG_TEMP_SCHEMA_NAME, relation));
        }
    }

    qualified_name_with_default(name, default_schema)
}

/// Extract and validate shard key column names from WITH (shard_key = 'col1,col2').
///
/// Returns an error if the specified columns do not exist in the table.
/// Returns an empty vec if no shard_key parameter is present (no sharding).
fn extract_and_validate_shard_key(
    params: &[(String, String)],
    columns: &[BoundCreateColumn],
) -> DbResult<Vec<String>> {
    let raw = params
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("shard_key"))
        .map(|(_, v)| v.as_str());
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    let names: Vec<String> = raw
        .split(',')
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect();
    if names.is_empty() {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            "shard_key must specify at least one column name",
        ));
    }
    for name in &names {
        if !columns.iter().any(|c| c.name.eq_ignore_ascii_case(name)) {
            return Err(DbError::bind_error(
                SqlState::UndefinedColumn,
                format!("shard key column '{name}' does not exist in the table definition"),
            ));
        }
    }
    Ok(names)
}

/// Extract and validate shard count from WITH (shard_count = N).
///
/// Returns an error if the value is present but not a valid positive integer.
/// Returns `None` if no shard_count parameter is present.
fn extract_and_validate_shard_count(params: &[(String, String)]) -> DbResult<Option<u32>> {
    let raw = params
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("shard_count"))
        .map(|(_, v)| v.as_str());
    let Some(raw) = raw else {
        return Ok(None);
    };
    let count: u32 = raw.parse().map_err(|_| {
        DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!("invalid shard_count value: '{raw}' (expected a positive integer)"),
        )
    })?;
    if count == 0 {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            "shard_count must be >= 1",
        ));
    }
    if count > aiondb_catalog::MAX_CATALOG_SHARD_COUNT {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!(
                "shard_count must be <= {}",
                aiondb_catalog::MAX_CATALOG_SHARD_COUNT
            ),
        ));
    }
    Ok(Some(count))
}

#[cfg(test)]
mod shard_param_tests {
    use super::*;

    #[test]
    fn shard_count_rejects_tuple_id_encoding_overflow() {
        let params = [("shard_count".to_owned(), "65537".to_owned())];

        let err = extract_and_validate_shard_count(&params)
            .expect_err("shard_count above TupleId encoding capacity must fail");

        assert!(
            err.to_string().contains("shard_count must be <= 65536"),
            "unexpected error: {err}"
        );
    }
}
