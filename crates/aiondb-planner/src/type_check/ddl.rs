use super::*;

fn expression_mentions_column(expr_sql: &str, column_name: &str) -> bool {
    let needle = column_name.to_ascii_lowercase();
    let mut token = String::new();
    for ch in expr_sql.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            token.push(ch.to_ascii_lowercase());
        } else if !token.is_empty() {
            if token == needle {
                return true;
            }
            token.clear();
        }
    }
    !token.is_empty() && token == needle
}

fn resolved_fk_reference_name(ref_table: &aiondb_parser::ObjectName) -> String {
    let default_schema = aiondb_eval::current_session_context().current_schema;
    match ref_table.parts.as_slice() {
        [relation] => {
            aiondb_catalog::QualifiedName::new(default_schema.as_deref(), relation).to_string()
        }
        [schema, relation] => {
            let resolved_schema = if schema.eq_ignore_ascii_case("public")
                && default_schema
                    .as_deref()
                    .map(|schema_name: &str| schema_name.to_ascii_lowercase().starts_with("db_"))
                    .unwrap_or(false)
            {
                default_schema.as_deref().unwrap_or(schema.as_str())
            } else {
                schema.as_str()
            };
            aiondb_catalog::QualifiedName::qualified(resolved_schema, relation).to_string()
        }
        _ => ref_table.parts.join("."),
    }
}

impl TypeChecker {
    fn referenced_columns_or_primary_key(
        &self,
        ref_table: &str,
        ref_columns: &[String],
    ) -> DbResult<Vec<String>> {
        if !ref_columns.is_empty() {
            return Ok(ref_columns.to_vec());
        }

        let qualified = match ref_table.split_once('.') {
            Some((schema, name)) => aiondb_catalog::QualifiedName::qualified(schema, name),
            None => aiondb_catalog::QualifiedName::unqualified(ref_table),
        };
        let table = self
            .catalog
            .get_table(TxnId::default(), &qualified)?
            .ok_or_else(|| {
                DbError::bind_error(
                    SqlState::UndefinedTable,
                    format!("relation \"{ref_table}\" does not exist"),
                )
            })?;

        Ok(table
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
            .unwrap_or_default())
    }

    pub fn type_check_create_index(
        &self,
        create_index: &BoundCreateIndex,
    ) -> DbResult<TypedCreateIndex> {
        if create_index.hnsw_params.is_some() && create_index.gin {
            return Err(DbError::bind_error(
                SqlState::InvalidParameterValue,
                "index cannot be both vector ANN and GIN",
            ));
        }

        if create_index.hnsw_params.is_some() {
            if create_index.key_columns.len() != 1 {
                return Err(DbError::bind_error(
                    SqlState::InvalidParameterValue,
                    "vector indexes require exactly one key column",
                ));
            }
            if !matches!(
                create_index.key_columns[0].data_type,
                DataType::Vector { .. }
            ) {
                return Err(DbError::bind_error(
                    SqlState::DatatypeMismatch,
                    "vector indexes are only supported on VECTOR columns",
                ));
            }
        }

        if create_index.gin {
            if create_index.key_columns.len() != 1 {
                return Err(DbError::bind_error(
                    SqlState::InvalidParameterValue,
                    "GIN indexes require exactly one key column",
                ));
            }
            if !matches!(
                create_index.key_columns[0].data_type,
                DataType::Jsonb | DataType::Text
            ) {
                return Err(DbError::bind_error(
                    SqlState::DatatypeMismatch,
                    "GIN indexes are only supported on JSONB or TEXT columns",
                ));
            }
        }

        if create_index.hnsw_params.is_none() && !create_index.gin {
            let has_vector = create_index
                .key_columns
                .iter()
                .any(|column| matches!(column.data_type, DataType::Vector { .. }));
            if has_vector {
                return Err(DbError::bind_error(
                    SqlState::DatatypeMismatch,
                    "VECTOR columns require USING hnsw or USING ivfflat for indexing",
                ));
            }
            let has_jsonb = create_index
                .key_columns
                .iter()
                .any(|column| column.data_type == DataType::Jsonb);
            if has_jsonb {
                return Err(DbError::bind_error(
                    SqlState::DatatypeMismatch,
                    "JSONB columns require USING gin for indexing",
                ));
            }
        }

        Ok(TypedCreateIndex {
            index_name: create_index.index_name.to_string(),
            table_id: create_index.relation.table_id,
            key_columns: create_index
                .key_columns
                .iter()
                .map(|column| LogicalIndexColumnPlan {
                    column_id: column.column_id,
                    descending: false,
                    nulls_first: false,
                })
                .collect(),
            key_expressions: create_index.key_expressions.clone(),
            hnsw_params: create_index.hnsw_params.clone(),
            gin: create_index.gin,
            unique: create_index.unique,
            nulls_not_distinct: create_index.nulls_not_distinct,
            concurrently: create_index.concurrently,
        })
    }

    pub fn type_check_truncate_table(
        &self,
        truncate_table: &BoundTruncateTable,
    ) -> DbResult<TypedTruncateTable> {
        Ok(TypedTruncateTable {
            table_id: truncate_table.relation.table_id,
        })
    }

    pub fn type_check_drop_table(&self, drop_table: &BoundDropTable) -> DbResult<TypedDropTable> {
        Ok(TypedDropTable {
            table_id: drop_table.relation.table_id,
            cascade: drop_table.cascade,
        })
    }

    pub fn type_check_drop_index(&self, drop_index: &BoundDropIndex) -> DbResult<TypedDropIndex> {
        Ok(TypedDropIndex {
            index_ids: drop_index
                .indexes
                .iter()
                .map(|index| index.index_id)
                .collect(),
        })
    }

    pub fn type_check_drop_sequence(
        &self,
        drop_sequence: &BoundDropSequence,
    ) -> DbResult<TypedDropSequence> {
        Ok(TypedDropSequence {
            sequence_id: drop_sequence.sequence.sequence_id,
        })
    }

    pub fn type_check_alter_table(
        &self,
        alter_table: &BoundAlterTable,
    ) -> DbResult<TypedAlterTable> {
        match alter_table {
            BoundAlterTable::AddColumn(add_column) => {
                self.type_check_alter_table_add_column(add_column)
            }
            BoundAlterTable::DropColumn(drop_column) => {
                self.type_check_alter_table_drop_column(drop_column)
            }
            BoundAlterTable::RenameTable(rename) => self.type_check_alter_table_rename(rename),
            BoundAlterTable::RenameColumn(rename_col) => {
                self.type_check_alter_table_rename_column(rename_col)
            }
            BoundAlterTable::SetDefault(set_default) => {
                self.type_check_alter_table_set_default(set_default)
            }
            BoundAlterTable::DropDefault(drop_default) => {
                self.type_check_alter_table_drop_default(drop_default)
            }
            BoundAlterTable::SetNotNull(set_not_null) => {
                self.type_check_alter_table_set_not_null(set_not_null)
            }
            BoundAlterTable::DropNotNull(drop_not_null) => {
                self.type_check_alter_table_drop_not_null(drop_not_null)
            }
            BoundAlterTable::AddConstraint(add_constraint) => {
                self.type_check_alter_table_add_constraint(add_constraint)
            }
            BoundAlterTable::DropConstraint(drop_constraint) => {
                self.type_check_alter_table_drop_constraint(drop_constraint)
            }
            BoundAlterTable::AlterColumnType(alter_col_type) => {
                self.type_check_alter_table_alter_column_type(alter_col_type)
            }
            BoundAlterTable::NoOp => Ok(TypedAlterTable::NoOp),
        }
    }

    fn type_check_alter_table_add_column(
        &self,
        add_column: &BoundAlterTableAddColumn,
    ) -> DbResult<TypedAlterTable> {
        let column_def = &add_column.column_def;
        let default = if let Some(ref default_expr) = column_def.default {
            if self::expr_contains_parameter(default_expr) {
                return Err(DbError::Bind(Box::new(ErrorReport::new(
                    SqlState::SyntaxError,
                    "DEFAULT expressions cannot contain parameters",
                ))));
            }

            let mut params = self.make_parameter_types();
            let typed = infer_expr_with_expected(
                default_expr,
                None,
                &column_def.data_type,
                column_def.nullable,
                &mut params,
                None,
                None,
            )?;
            validate_assignment_expr(
                &typed,
                &column_def.data_type,
                column_def.nullable,
                false,
                "DEFAULT",
            )?;
            Some(super::serialize_expr(default_expr)?)
        } else {
            None
        };

        Ok(TypedAlterTable::AddColumn(TypedAlterTableAddColumn {
            table_id: add_column.relation.table_id,
            column: LogicalColumnPlan {
                name: column_def.name.clone(),
                data_type: column_def.data_type.clone(),
                raw_type_name: column_def.raw_type_name.clone(),
                text_type_modifier: column_def.text_type_modifier,
                nullable: column_def.nullable,
                has_default: default.is_some(),
            },
            default,
        }))
    }

    fn type_check_alter_table_drop_column(
        &self,
        drop_column: &BoundAlterTableDropColumn,
    ) -> DbResult<TypedAlterTable> {
        let table = &drop_column.relation;
        let column = &drop_column.column;
        let table_name = table.name.object_name();

        if table
            .primary_key
            .as_ref()
            .is_some_and(|pk| pk.contains(&column.column_id))
        {
            let pkey_name = format!("\"{}_pkey\"", table_name);
            return Err(DbError::bind_error(
                SqlState::DependentObjectsStillExist,
                format!(
                    "cannot drop column {} of table {} because other objects depend on it",
                    column.name, table_name
                ),
            )
            .with_client_detail(format!(
                "constraint {} on table {} depends on column {} of table {}",
                pkey_name, table_name, column.name, table_name
            ))
            .with_client_hint("Use DROP ... CASCADE to drop the dependent objects too."));
        }

        if let Some(fk) = table.foreign_keys.iter().find(|fk| {
            fk.columns
                .iter()
                .any(|name| name.eq_ignore_ascii_case(&column.name))
                || fk
                    .referenced_columns
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(&column.name))
        }) {
            let fk_name = fk.effective_name(table_name);
            return Err(DbError::bind_error(
                SqlState::DependentObjectsStillExist,
                format!(
                    "cannot drop column {} of table {} because other objects depend on it",
                    column.name, table_name
                ),
            )
            .with_client_detail(format!(
                "constraint {} on table {} depends on column {} of table {}",
                fk_name, table_name, column.name, table_name
            ))
            .with_client_hint("Use DROP ... CASCADE to drop the dependent objects too."));
        }

        if let Some(check_constraint) = table
            .check_constraints
            .iter()
            .find(|check| expression_mentions_column(&check.expression, &column.name))
        {
            let check_name = check_constraint.name.as_deref().unwrap_or("<unnamed>");
            return Err(DbError::bind_error(
                SqlState::DependentObjectsStillExist,
                format!(
                    "cannot drop column {} of table {} because other objects depend on it",
                    column.name, table_name
                ),
            )
            .with_client_detail(format!(
                "constraint {} on table {} depends on column {} of table {}",
                check_name, table_name, column.name, table_name
            ))
            .with_client_hint("Use DROP ... CASCADE to drop the dependent objects too."));
        }

        Ok(TypedAlterTable::DropColumn(TypedAlterTableDropColumn {
            table_id: drop_column.relation.table_id,
            column_id: drop_column.column.column_id,
        }))
    }

    fn type_check_alter_table_rename(
        &self,
        rename: &BoundAlterTableRename,
    ) -> DbResult<TypedAlterTable> {
        Ok(TypedAlterTable::RenameTable(TypedAlterTableRename {
            table_id: rename.relation.table_id,
            new_name: rename.new_name.clone(),
        }))
    }

    fn type_check_alter_table_rename_column(
        &self,
        rename_col: &BoundAlterTableRenameColumn,
    ) -> DbResult<TypedAlterTable> {
        Ok(TypedAlterTable::RenameColumn(TypedAlterTableRenameColumn {
            table_id: rename_col.relation.table_id,
            old_column_id: rename_col.old_column.column_id,
            new_column_name: rename_col.new_name.clone(),
        }))
    }

    fn type_check_alter_table_set_default(
        &self,
        set_default: &BoundAlterTableSetDefault,
    ) -> DbResult<TypedAlterTable> {
        let default_expr = &set_default.default;
        if self::expr_contains_parameter(default_expr) {
            return Err(DbError::Bind(Box::new(ErrorReport::new(
                SqlState::SyntaxError,
                "DEFAULT expressions cannot contain parameters",
            ))));
        }

        let mut params = self.make_parameter_types();
        let typed = infer_expr_with_expected(
            default_expr,
            None,
            &set_default.column.data_type,
            set_default.column.nullable,
            &mut params,
            None,
            None,
        )?;
        validate_assignment_expr(
            &typed,
            &set_default.column.data_type,
            set_default.column.nullable,
            false,
            "DEFAULT",
        )?;
        let serialized = super::serialize_expr(default_expr)?;

        Ok(TypedAlterTable::SetDefault(TypedAlterTableSetDefault {
            table_id: set_default.relation.table_id,
            column_id: set_default.column.column_id,
            default_expr: serialized,
        }))
    }

    fn type_check_alter_table_drop_default(
        &self,
        drop_default: &BoundAlterTableDropDefault,
    ) -> DbResult<TypedAlterTable> {
        Ok(TypedAlterTable::DropDefault(TypedAlterTableDropDefault {
            table_id: drop_default.relation.table_id,
            column_id: drop_default.column.column_id,
        }))
    }

    fn type_check_alter_table_set_not_null(
        &self,
        set_not_null: &BoundAlterTableSetNotNull,
    ) -> DbResult<TypedAlterTable> {
        Ok(TypedAlterTable::SetNotNull(TypedAlterTableSetNotNull {
            table_id: set_not_null.relation.table_id,
            column_id: set_not_null.column.column_id,
        }))
    }

    fn type_check_alter_table_drop_not_null(
        &self,
        drop_not_null: &BoundAlterTableDropNotNull,
    ) -> DbResult<TypedAlterTable> {
        if drop_not_null
            .relation
            .primary_key
            .as_ref()
            .is_some_and(|pk| pk.contains(&drop_not_null.column.column_id))
        {
            return Err(DbError::bind_error(
                SqlState::InvalidTableDefinition,
                format!(
                    "column \"{}\" is in a primary key",
                    drop_not_null.column.name
                ),
            ));
        }
        Ok(TypedAlterTable::DropNotNull(TypedAlterTableDropNotNull {
            table_id: drop_not_null.relation.table_id,
            column_id: drop_not_null.column.column_id,
        }))
    }

    fn type_check_alter_table_add_constraint(
        &self,
        add_constraint: &BoundAlterTableAddConstraint,
    ) -> DbResult<TypedAlterTable> {
        let table_id = add_constraint.relation.table_id;
        let constraint = &add_constraint.constraint;

        let (
            constraint_type,
            constraint_name,
            columns,
            check_expr,
            ref_table,
            ref_columns,
            on_delete,
            on_update,
            on_delete_set_columns,
            on_update_set_columns,
            match_type,
        ) = match constraint {
            aiondb_parser::TableConstraint::PrimaryKey { name, columns, .. } => (
                "PRIMARY KEY".to_owned(),
                name.clone(),
                columns.clone(),
                None,
                None,
                Vec::new(),
                aiondb_core::FkAction::NoAction,
                aiondb_core::FkAction::NoAction,
                Vec::new(),
                Vec::new(),
                aiondb_core::FkMatchType::Simple,
            ),
            aiondb_parser::TableConstraint::Unique { name, columns, .. } => (
                "UNIQUE".to_owned(),
                name.clone(),
                columns.clone(),
                None,
                None,
                Vec::new(),
                aiondb_core::FkAction::NoAction,
                aiondb_core::FkAction::NoAction,
                Vec::new(),
                Vec::new(),
                aiondb_core::FkMatchType::Simple,
            ),
            aiondb_parser::TableConstraint::Check { name, expr, .. } => {
                let serialized = super::serialize_expr(expr)?;
                (
                    "CHECK".to_owned(),
                    name.clone(),
                    Vec::new(),
                    Some(serialized),
                    None,
                    Vec::new(),
                    aiondb_core::FkAction::NoAction,
                    aiondb_core::FkAction::NoAction,
                    Vec::new(),
                    Vec::new(),
                    aiondb_core::FkMatchType::Simple,
                )
            }
            aiondb_parser::TableConstraint::ForeignKey {
                name,
                columns,
                ref_table,
                ref_columns,
                on_delete,
                on_update,
                on_delete_set_columns,
                on_update_set_columns,
                match_type,
                ..
            } => {
                let ref_table_str = resolved_fk_reference_name(ref_table);
                let resolved_ref_columns =
                    self.referenced_columns_or_primary_key(&ref_table_str, ref_columns)?;
                (
                    "FOREIGN KEY".to_owned(),
                    name.clone(),
                    columns.clone(),
                    None,
                    Some(ref_table_str),
                    resolved_ref_columns,
                    *on_delete,
                    *on_update,
                    on_delete_set_columns.clone(),
                    on_update_set_columns.clone(),
                    *match_type,
                )
            }
        };

        Ok(TypedAlterTable::AddConstraint(
            TypedAlterTableAddConstraint {
                table_id,
                constraint_type,
                constraint_name,
                columns,
                check_expr,
                ref_table,
                ref_columns,
                on_delete,
                on_update,
                on_delete_set_columns,
                on_update_set_columns,
                match_type,
            },
        ))
    }

    fn type_check_alter_table_drop_constraint(
        &self,
        drop_constraint: &BoundAlterTableDropConstraint,
    ) -> DbResult<TypedAlterTable> {
        Ok(TypedAlterTable::DropConstraint(
            TypedAlterTableDropConstraint {
                table_id: drop_constraint.relation.table_id,
                constraint_name: drop_constraint.constraint_name.clone(),
            },
        ))
    }

    fn type_check_alter_table_alter_column_type(
        &self,
        alter_col_type: &BoundAlterTableAlterColumnType,
    ) -> DbResult<TypedAlterTable> {
        Ok(TypedAlterTable::AlterColumnType(
            TypedAlterTableAlterColumnType {
                table_id: alter_col_type.relation.table_id,
                column_id: alter_col_type.column.column_id,
                new_type: alter_col_type.new_type.clone(),
                raw_type_name: alter_col_type.raw_type_name.clone(),
                text_type_modifier: alter_col_type.text_type_modifier,
            },
        ))
    }

    pub fn type_check_create_node_label(
        &self,
        bound: &BoundCreateNodeLabel,
    ) -> DbResult<TypedCreateNodeLabel> {
        Ok(TypedCreateNodeLabel {
            label: bound.label.clone(),
            table_id: bound.table.table_id,
        })
    }

    pub fn type_check_create_edge_label(
        &self,
        bound: &BoundCreateEdgeLabel,
    ) -> DbResult<TypedCreateEdgeLabel> {
        Ok(TypedCreateEdgeLabel {
            label: bound.label.clone(),
            table_id: bound.table.table_id,
            source_label: bound.source_label.clone(),
            target_label: bound.target_label.clone(),
            endpoints: bound.endpoints.clone(),
        })
    }

    pub fn type_check_drop_node_label(
        &self,
        bound: &BoundDropNodeLabel,
    ) -> DbResult<TypedDropNodeLabel> {
        Ok(TypedDropNodeLabel {
            label: bound.label.clone(),
        })
    }

    pub fn type_check_drop_edge_label(
        &self,
        bound: &BoundDropEdgeLabel,
    ) -> DbResult<TypedDropEdgeLabel> {
        Ok(TypedDropEdgeLabel {
            label: bound.label.clone(),
        })
    }

    pub fn type_check_create_role(&self, bound: &BoundCreateRole) -> DbResult<TypedCreateRole> {
        let mut login = false;
        let mut superuser = false;
        let mut password = None;
        let mut inherit = true;
        let mut createdb = false;
        let mut createrole = false;
        let mut replication = false;
        let mut bypassrls = false;
        let mut connection_limit: i64 = -1;
        let mut valid_until: Option<String> = None;

        for option in &bound.options {
            match option {
                aiondb_parser::RoleOption::Login => login = true,
                aiondb_parser::RoleOption::Nologin => login = false,
                aiondb_parser::RoleOption::Superuser => superuser = true,
                aiondb_parser::RoleOption::Nosuperuser => superuser = false,
                aiondb_parser::RoleOption::Password(pw) => password = Some(pw.clone()),
                aiondb_parser::RoleOption::PasswordNull => password = None,
                aiondb_parser::RoleOption::Inherit => inherit = true,
                aiondb_parser::RoleOption::Noinherit => inherit = false,
                aiondb_parser::RoleOption::Createdb => createdb = true,
                aiondb_parser::RoleOption::Nocreatedb => createdb = false,
                aiondb_parser::RoleOption::Createrole => createrole = true,
                aiondb_parser::RoleOption::Nocreaterole => createrole = false,
                aiondb_parser::RoleOption::Replication => replication = true,
                aiondb_parser::RoleOption::Noreplication => replication = false,
                aiondb_parser::RoleOption::Bypassrls => bypassrls = true,
                aiondb_parser::RoleOption::Nobypassrls => bypassrls = false,
                aiondb_parser::RoleOption::ConnectionLimit(n) => connection_limit = *n,
                aiondb_parser::RoleOption::ValidUntil(s) => valid_until = Some(s.clone()),
            }
        }

        Ok(TypedCreateRole {
            name: bound.name.clone(),
            login,
            superuser,
            password,
            inherit,
            createdb,
            createrole,
            replication,
            bypassrls,
            connection_limit,
            valid_until,
        })
    }

    pub fn type_check_drop_role(&self, bound: &BoundDropRole) -> DbResult<TypedDropRole> {
        Ok(TypedDropRole {
            name: bound.name.clone(),
        })
    }

    pub fn type_check_alter_role(&self, bound: &BoundAlterRole) -> DbResult<TypedAlterRole> {
        let mut login = bound.current_role.login;
        let mut superuser = bound.current_role.superuser;
        let mut new_password = None;
        let mut inherit = bound.current_role.inherit;
        let mut createdb = bound.current_role.createdb;
        let mut createrole = bound.current_role.createrole;
        let mut replication = bound.current_role.replication;
        let mut bypassrls = bound.current_role.bypassrls;
        let mut connection_limit = bound.current_role.connection_limit;
        let mut valid_until = bound.current_role.valid_until.clone();

        for option in &bound.options {
            match option {
                aiondb_parser::RoleOption::Login => login = true,
                aiondb_parser::RoleOption::Nologin => login = false,
                aiondb_parser::RoleOption::Superuser => superuser = true,
                aiondb_parser::RoleOption::Nosuperuser => superuser = false,
                aiondb_parser::RoleOption::Password(pw) => new_password = Some(pw.clone()),
                aiondb_parser::RoleOption::PasswordNull => {
                    return Err(DbError::bind_error(
                        SqlState::FeatureNotSupported,
                        "ALTER ROLE ... PASSWORD NULL is not supported",
                    ));
                }
                aiondb_parser::RoleOption::Inherit => inherit = true,
                aiondb_parser::RoleOption::Noinherit => inherit = false,
                aiondb_parser::RoleOption::Createdb => createdb = true,
                aiondb_parser::RoleOption::Nocreatedb => createdb = false,
                aiondb_parser::RoleOption::Createrole => createrole = true,
                aiondb_parser::RoleOption::Nocreaterole => createrole = false,
                aiondb_parser::RoleOption::Replication => replication = true,
                aiondb_parser::RoleOption::Noreplication => replication = false,
                aiondb_parser::RoleOption::Bypassrls => bypassrls = true,
                aiondb_parser::RoleOption::Nobypassrls => bypassrls = false,
                aiondb_parser::RoleOption::ConnectionLimit(n) => connection_limit = *n,
                aiondb_parser::RoleOption::ValidUntil(s) => valid_until = Some(s.clone()),
            }
        }

        Ok(TypedAlterRole {
            name: bound.name.clone(),
            login,
            superuser,
            current_password_hash: bound.current_role.password_hash.clone(),
            new_password,
            inherit,
            createdb,
            createrole,
            replication,
            bypassrls,
            connection_limit,
            valid_until,
        })
    }

    pub fn type_check_grant(&self, bound: &BoundGrant) -> DbResult<TypedGrant> {
        let privileges = convert_privileges(&bound.privileges);
        let target = convert_grant_target(
            &bound.target,
            self.session_context.current_schema.as_deref(),
        );
        self.validate_acl_privileges_for_target(&privileges, &target)?;
        Ok(TypedGrant {
            privileges,
            target,
            role_name: bound.role_name.clone(),
        })
    }

    pub fn type_check_revoke(&self, bound: &BoundRevoke) -> DbResult<TypedRevoke> {
        let privileges = convert_privileges(&bound.privileges);
        let target = convert_grant_target(
            &bound.target,
            self.session_context.current_schema.as_deref(),
        );
        self.validate_acl_privileges_for_target(&privileges, &target)?;
        Ok(TypedRevoke {
            privileges,
            target,
            role_name: bound.role_name.clone(),
        })
    }

    pub fn type_check_create_schema(
        &self,
        bound: &BoundCreateSchema,
    ) -> DbResult<TypedCreateSchema> {
        Ok(TypedCreateSchema {
            name: bound.name.clone(),
        })
    }

    pub fn type_check_drop_schema(&self, bound: &BoundDropSchema) -> DbResult<TypedDropSchema> {
        Ok(TypedDropSchema {
            schema_id: bound.schema_id,
            name: bound.name.clone(),
            cascade: bound.cascade,
        })
    }

    fn validate_acl_privileges_for_target(
        &self,
        privileges: &[CatalogPrivilege],
        target: &PrivilegeTarget,
    ) -> DbResult<()> {
        let target_kind = self.classify_acl_target_kind(target)?;
        for privilege in privileges {
            if *privilege == CatalogPrivilege::All {
                continue;
            }
            if acl_privilege_allowed_for_target_kind(*privilege, target_kind) {
                continue;
            }
            return Err(DbError::bind_error(
                SqlState::InvalidParameterValue,
                format!(
                    "invalid privilege type {} for {}",
                    acl_privilege_name(*privilege),
                    target_kind.label()
                ),
            ));
        }
        Ok(())
    }

    fn classify_acl_target_kind(&self, target: &PrivilegeTarget) -> DbResult<AclTargetKind> {
        match target {
            PrivilegeTarget::Table(name) => {
                let txn_id = TxnId::default();
                if self.catalog.get_sequence(txn_id, name)?.is_some() {
                    return Ok(AclTargetKind::Sequence);
                }
                Ok(AclTargetKind::TableLike)
            }
            PrivilegeTarget::Function(_) => Ok(AclTargetKind::Function),
            PrivilegeTarget::Schema(_) => Ok(AclTargetKind::Schema),
            PrivilegeTarget::Database(_) => Ok(AclTargetKind::Database),
            PrivilegeTarget::Role(_) => Ok(AclTargetKind::RoleMembership),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AclTargetKind {
    TableLike,
    Sequence,
    Function,
    Schema,
    Database,
    RoleMembership,
}

impl AclTargetKind {
    fn label(self) -> &'static str {
        match self {
            Self::TableLike => "table",
            Self::Sequence => "sequence",
            Self::Function => "function",
            Self::Schema => "schema",
            Self::Database => "database",
            Self::RoleMembership => "role",
        }
    }
}

fn acl_privilege_allowed_for_target_kind(
    privilege: CatalogPrivilege,
    target_kind: AclTargetKind,
) -> bool {
    match target_kind {
        AclTargetKind::TableLike => matches!(
            privilege,
            CatalogPrivilege::Select
                | CatalogPrivilege::Insert
                | CatalogPrivilege::Update
                | CatalogPrivilege::Delete
                | CatalogPrivilege::Truncate
                | CatalogPrivilege::References
                | CatalogPrivilege::Trigger
        ),
        AclTargetKind::Sequence => matches!(
            privilege,
            CatalogPrivilege::Select | CatalogPrivilege::Update | CatalogPrivilege::Usage
        ),
        AclTargetKind::Function => matches!(privilege, CatalogPrivilege::Execute),
        AclTargetKind::Schema => {
            // Strict PG accepts only USAGE/CREATE on schema. ORM migration
            // tools commonly emit `GRANT SELECT ON ALL TABLES IN SCHEMA s`,
            // which the parser currently lowers to a plain Schema target —
            // accept the per-table privilege keywords here as a permissive
            // alias so those scripts run end-to-end. Future work:
            // dedicated AllTablesInSchema variant that expands per table.
            matches!(
                privilege,
                CatalogPrivilege::Usage
                    | CatalogPrivilege::Create
                    | CatalogPrivilege::Select
                    | CatalogPrivilege::Insert
                    | CatalogPrivilege::Update
                    | CatalogPrivilege::Delete
                    | CatalogPrivilege::Truncate
                    | CatalogPrivilege::References
                    | CatalogPrivilege::Trigger
            )
        }
        AclTargetKind::Database => matches!(
            privilege,
            CatalogPrivilege::Connect | CatalogPrivilege::Create | CatalogPrivilege::Temporary
        ),
        AclTargetKind::RoleMembership => matches!(privilege, CatalogPrivilege::Usage),
    }
}

fn acl_privilege_name(privilege: CatalogPrivilege) -> &'static str {
    match privilege {
        CatalogPrivilege::Select => "SELECT",
        CatalogPrivilege::Insert => "INSERT",
        CatalogPrivilege::Update => "UPDATE",
        CatalogPrivilege::Delete => "DELETE",
        CatalogPrivilege::Create => "CREATE",
        CatalogPrivilege::Usage => "USAGE",
        CatalogPrivilege::All => "ALL",
        CatalogPrivilege::Execute => "EXECUTE",
        CatalogPrivilege::Trigger => "TRIGGER",
        CatalogPrivilege::References => "REFERENCES",
        CatalogPrivilege::Connect => "CONNECT",
        CatalogPrivilege::Temporary => "TEMPORARY",
        CatalogPrivilege::Truncate => "TRUNCATE",
    }
}

fn convert_privileges(privileges: &[aiondb_parser::Privilege]) -> Vec<CatalogPrivilege> {
    privileges
        .iter()
        .map(|p| match p {
            aiondb_parser::Privilege::Select => CatalogPrivilege::Select,
            aiondb_parser::Privilege::Insert => CatalogPrivilege::Insert,
            aiondb_parser::Privilege::Update => CatalogPrivilege::Update,
            aiondb_parser::Privilege::Delete => CatalogPrivilege::Delete,
            aiondb_parser::Privilege::Create => CatalogPrivilege::Create,
            aiondb_parser::Privilege::Usage => CatalogPrivilege::Usage,
            aiondb_parser::Privilege::All => CatalogPrivilege::All,
            aiondb_parser::Privilege::Execute => CatalogPrivilege::Execute,
            aiondb_parser::Privilege::Trigger => CatalogPrivilege::Trigger,
            aiondb_parser::Privilege::References => CatalogPrivilege::References,
            aiondb_parser::Privilege::Connect => CatalogPrivilege::Connect,
            aiondb_parser::Privilege::Temporary => CatalogPrivilege::Temporary,
            aiondb_parser::Privilege::Truncate => CatalogPrivilege::Truncate,
        })
        .collect()
}

fn convert_grant_target(
    target: &aiondb_parser::GrantTarget,
    default_schema: Option<&str>,
) -> PrivilegeTarget {
    match target {
        aiondb_parser::GrantTarget::Table(name) => {
            let qn = match name.parts.as_slice() {
                [schema, table] => QualifiedName::qualified(schema, table),
                [table] => QualifiedName::new(default_schema, table),
                _ => QualifiedName::unqualified(name.parts.join(".")),
            };
            PrivilegeTarget::Table(qn)
        }
        aiondb_parser::GrantTarget::Function(target) => {
            let qn = match target.name.parts.as_slice() {
                [schema, function] => QualifiedName::qualified(schema, function),
                [function] => QualifiedName::new(default_schema, function),
                _ => QualifiedName::unqualified(target.name.parts.join(".")),
            };
            PrivilegeTarget::Function(aiondb_catalog::FunctionPrivilegeTarget {
                name: qn,
                arg_types: target.arg_types.clone(),
            })
        }
        aiondb_parser::GrantTarget::Schema(name) => PrivilegeTarget::Schema(name.clone()),
        aiondb_parser::GrantTarget::Database(name) => PrivilegeTarget::Database(name.clone()),
        aiondb_parser::GrantTarget::Role(name) => PrivilegeTarget::Role(name.clone()),
    }
}
