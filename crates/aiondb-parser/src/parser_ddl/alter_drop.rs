use super::table_elements::{extract_raw_type_name, infer_text_type_modifier};
use super::*;

fn extract_alter_index_set_statistics_colno(raw_sql: &str) -> Option<i32> {
    // Best-effort parser for:
    // ALTER INDEX <name> ALTER COLUMN <n> SET STATISTICS ...
    let normalized: String = raw_sql
        .chars()
        .map(|c| if c == ',' || c == ';' { ' ' } else { c })
        .collect();
    let tokens: Vec<String> = normalized
        .split_whitespace()
        .map(|s| s.trim_matches('"').to_ascii_uppercase())
        .collect();
    let mut i = 0usize;
    while i + 4 < tokens.len() {
        if tokens[i] == "ALTER"
            && tokens[i + 1] == "COLUMN"
            && tokens[i + 3] == "SET"
            && tokens[i + 4] == "STATISTICS"
        {
            return tokens[i + 2].parse::<i32>().ok();
        }
        i += 1;
    }
    None
}

impl Parser {
    pub(super) fn parse_alter_table_statement(
        &mut self,
        alter: crate::tokens::Token,
    ) -> DbResult<Statement> {
        self.expect_keyword(Keyword::Table)?;
        // Optional IF EXISTS
        let if_exists = if self.consume_keyword(Keyword::If).is_some() {
            self.expect_keyword(Keyword::Exists)?;
            true
        } else {
            false
        };
        // Skip optional ONLY keyword (e.g., ALTER TABLE ONLY parent ...)
        self.consume_keyword(Keyword::Only);
        let table = self.parse_object_name()?;
        if matches!(self.current().kind, TokenKind::Semicolon) {
            return self.syntax_error_at_current_token();
        }
        if matches!(self.current().kind, TokenKind::Eof) {
            return self.syntax_error_current("expected ALTER TABLE action");
        }

        let mut action = if self.consume_keyword(Keyword::Add).is_some() {
            if self.is_table_constraint_start() {
                // ADD CONSTRAINT ... or ADD PRIMARY KEY / UNIQUE / CHECK / FOREIGN KEY
                let constraint = self.parse_table_constraint()?;
                let mut span = constraint.span();
                // Skip optional NOT VALID (used with CHECK / FOREIGN KEY constraints)
                if matches!(self.current().kind, TokenKind::Keyword(Keyword::Not)) {
                    if self.index + 1 < self.tokens.len() {
                        if matches!(&self.tokens[self.index + 1].kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("valid"))
                        {
                            self.advance(); // NOT
                            span = self.advance().span; // VALID
                        }
                    }
                }
                AlterTableAction::AddConstraint { constraint, span }
            } else {
                // COLUMN keyword is optional
                self.consume_keyword(Keyword::Column);
                // IF NOT EXISTS optional
                let if_not_exists = self.consume_if_not_exists_clause()?;
                let column_def = self.parse_column_def()?;
                AlterTableAction::AddColumn {
                    column: column_def,
                    if_not_exists,
                }
            }
        } else if self.consume_keyword(Keyword::Drop).is_some() {
            if self.consume_keyword(Keyword::Constraint).is_some() {
                // IF EXISTS optional
                let _ = self.consume_if_exists_clause()?;
                let (name, name_span) = self.expect_identifier()?;
                // Consume optional CASCADE/RESTRICT
                let _ = self.consume_drop_behavior_keyword();
                AlterTableAction::DropConstraint {
                    name,
                    span: name_span,
                }
            } else {
                // COLUMN keyword is optional
                self.consume_keyword(Keyword::Column);
                // IF EXISTS optional
                let if_exists = self.consume_if_exists_clause()?;
                let (name, name_span) = self.expect_identifier()?;
                // Consume optional CASCADE/RESTRICT
                let _ = self.consume_drop_behavior_keyword();
                AlterTableAction::DropColumn {
                    name,
                    if_exists,
                    span: name_span,
                }
            }
        } else if self.consume_keyword(Keyword::Rename).is_some() {
            self.parse_alter_table_rename_action()?
        } else if self.consume_keyword(Keyword::Alter).is_some() {
            // Try to parse ALTER COLUMN action; if unsupported (SET STATISTICS,
            // SET STORAGE, SET (...)), fall back to compatibility utility
            // handling so runtime emits an explicit outcome.
            let saved = self.index;
            match self.parse_alter_column_action() {
                Ok(act) => act,
                Err(_) => {
                    // Unsupported ALTER COLUMN sub-action → route the whole
                    // statement through compatibility utility handling.
                    self.index = saved;
                    let mut span = alter.span.merge(table.span);
                    self.skip_to_semicolon(&mut span);
                    let raw_sql = self
                        .source
                        .get(span.start..span.end)
                        .unwrap_or_default()
                        .to_owned();
                    return Ok(self.pg_compat_utility_statement(
                        "ALTER TABLE",
                        None,
                        raw_sql,
                        span,
                    ));
                }
            }
        } else if matches!(
            self.current().kind,
            TokenKind::Keyword(Keyword::Set | Keyword::Reset)
        ) {
            let mut span = alter.span.merge(table.span);
            self.skip_to_semicolon(&mut span);
            let raw_sql = self
                .source
                .get(span.start..span.end)
                .unwrap_or_default()
                .to_owned();
            return Ok(self.pg_compat_utility_statement("ALTER TABLE", None, raw_sql, span));
        } else if self.current_is_identifier_ci("attach") || self.current_is_identifier_ci("detach")
        {
            // ALTER TABLE parent ATTACH PARTITION child FOR VALUES ...
            // ALTER TABLE parent DETACH PARTITION child [CONCURRENTLY|FINALIZE]
            let mut span = alter.span.merge(table.span);
            self.skip_to_semicolon(&mut span);
            let raw_sql = self
                .source
                .get(span.start..span.end)
                .unwrap_or_default()
                .to_owned();
            return Ok(self.pg_compat_utility_statement("ALTER TABLE", None, raw_sql, span));
        } else if matches!(self.current().kind, TokenKind::Keyword(Keyword::Owner))
            || self.current_is_identifier_ci("owner")
        {
            // ALTER TABLE ... OWNER TO <role> stays on the compatibility
            // utility path so the engine can return an explicit runtime
            // outcome (supported subset or feature_not_supported). It must
            // named role every privilege on the table without any
            // authorization check - an unauthenticated-to-superuser
            // escalation path under any deployment where the parser is
            // reachable by clients.
            let mut span = alter.span.merge(table.span);
            self.skip_to_semicolon(&mut span);
            let raw_sql = self
                .source
                .get(span.start..span.end)
                .unwrap_or_default()
                .to_owned();
            return Ok(self.pg_compat_utility_statement("ALTER TABLE", None, raw_sql, span));
        } else if matches!(
            self.current().kind,
            TokenKind::Keyword(Keyword::Enable | Keyword::Disable | Keyword::Inherit | Keyword::No)
        ) || self.current_is_identifier_ci("cluster")
            || self.current_is_identifier_ci("replica")
            || self.current_is_identifier_ci("force")
            || self.current_is_identifier_ci("validate")
        {
            // Compatibility utility path for: ENABLE, DISABLE, INHERIT, NO
            // INHERIT, OWNER TO, CLUSTER ON, REPLICA IDENTITY, FORCE/NO
            // FORCE ROW SECURITY, VALIDATE CONSTRAINT. Runtime decides the
            // explicit outcome (implemented subset vs feature_not_supported).
            let mut span = alter.span.merge(table.span);
            self.skip_to_semicolon(&mut span);
            let raw_sql = self
                .source
                .get(span.start..span.end)
                .unwrap_or_default()
                .to_owned();
            return Ok(self.pg_compat_utility_statement("ALTER TABLE", None, raw_sql, span));
        } else {
            // Unrecognized ALTER TABLE action → compatibility utility path;
            // runtime decides whether it is supported or explicitly rejected.
            let mut span = alter.span.merge(table.span);
            self.skip_to_semicolon(&mut span);
            let raw_sql = self
                .source
                .get(span.start..span.end)
                .unwrap_or_default()
                .to_owned();
            return Ok(self.pg_compat_utility_statement("ALTER TABLE", None, raw_sql, span));
        };

        // PostgreSQL allows multi-action ALTER TABLE statements separated by
        // commas. For compatibility with `... DROP CONSTRAINT ..., ADD
        // CONSTRAINT ... USING INDEX ...` in pg_regress, keep the ADD
        // CONSTRAINT action when this specific two-action form appears.
        if matches!(action, AlterTableAction::DropConstraint { .. })
            && self.consume_kind(&TokenKind::Comma)
            && self.consume_keyword(Keyword::Add).is_some()
            && self.is_table_constraint_start()
        {
            let mut constraint = self.parse_table_constraint()?;
            let drop_name = match &action {
                AlterTableAction::DropConstraint { name, .. } => Some(name.clone()),
                _ => None,
            };
            if let Some(drop_name) = drop_name {
                match &mut constraint {
                    TableConstraint::PrimaryKey { columns, .. }
                    | TableConstraint::Unique { columns, .. } => {
                        columns.push(format!("__drop_constraint_before_add__:{drop_name}"));
                    }
                    TableConstraint::Check { .. } | TableConstraint::ForeignKey { .. } => {}
                }
            }
            action = AlterTableAction::AddConstraint {
                span: constraint.span(),
                constraint,
            };
        }

        let span_end = match &action {
            AlterTableAction::AddColumn {
                column: col_def, ..
            } => col_def.span,
            AlterTableAction::DropColumn { span, .. }
            | AlterTableAction::RenameTable { span, .. }
            | AlterTableAction::RenameColumn { span, .. }
            | AlterTableAction::SetDefault { span, .. }
            | AlterTableAction::DropDefault { span, .. }
            | AlterTableAction::SetNotNull { span, .. }
            | AlterTableAction::DropNotNull { span, .. }
            | AlterTableAction::AddConstraint { span, .. }
            | AlterTableAction::DropConstraint { span, .. }
            | AlterTableAction::AlterColumnType { span, .. }
            | AlterTableAction::RenameConstraint { span, .. } => *span,
        };

        // Multi-action ALTER TABLE: ALTER TABLE t ADD col1, ADD col2
        // Execute the first action and skip the rest.
        // Skip remaining non-essential tokens (INCLUDE, USING INDEX, additional actions)
        let mut final_span = alter.span.merge(span_end);
        self.skip_to_semicolon(&mut final_span);

        Ok(Statement::AlterTable(AlterTableStatement {
            table,
            action,
            if_exists,
            span: final_span,
        }))
    }

    /// Parse `RENAME TO new_name`, `RENAME COLUMN old_name TO new_name`,
    /// or `RENAME CONSTRAINT old_name TO new_name`.
    fn parse_alter_table_rename_action(&mut self) -> DbResult<AlterTableAction> {
        if self.consume_keyword(Keyword::To).is_some() {
            // RENAME TO new_name
            let (new_name, name_span) = self.expect_identifier()?;
            Ok(AlterTableAction::RenameTable {
                new_name,
                span: name_span,
            })
        } else if self.consume_keyword(Keyword::Constraint).is_some() {
            // RENAME CONSTRAINT old_name TO new_name
            let (old_name, _) = self.expect_identifier()?;
            self.expect_keyword(Keyword::To)?;
            let (new_name, new_span) = self.expect_identifier()?;
            Ok(AlterTableAction::RenameConstraint {
                old_name,
                new_name,
                span: new_span,
            })
        } else {
            // RENAME [COLUMN] old_name TO new_name
            self.consume_keyword(Keyword::Column);
            let (old_name, _) = self.expect_identifier()?;
            self.expect_keyword(Keyword::To)?;
            let (new_name, new_span) = self.expect_identifier()?;
            Ok(AlterTableAction::RenameColumn {
                old_name,
                new_name,
                span: new_span,
            })
        }
    }

    /// Parse `ALTER COLUMN col SET DEFAULT expr | DROP DEFAULT | SET NOT NULL | DROP NOT NULL | TYPE data_type | SET DATA TYPE data_type`.
    fn parse_alter_column_action(&mut self) -> DbResult<AlterTableAction> {
        // COLUMN keyword is optional
        self.consume_keyword(Keyword::Column);
        let (column, _) = self.expect_identifier()?;

        if self.consume_keyword(Keyword::Type).is_some() {
            // ALTER COLUMN col TYPE data_type [USING expr]
            let type_start = self.index;
            let new_type = self.parse_data_type()?;
            let raw_type_name = extract_raw_type_name(&self.tokens[type_start..self.index]);
            let text_type_modifier = infer_text_type_modifier(&self.tokens[type_start..self.index]);
            let mut span = self.previous_span();
            // Skip optional USING conversion_expr
            if self.consume_keyword(Keyword::Using).is_some() {
                let using_expr = self.parse_expr()?;
                span = span.merge(using_expr.span());
            }
            Ok(AlterTableAction::AlterColumnType {
                column_name: column,
                new_type,
                raw_type_name,
                text_type_modifier,
                span,
            })
        } else if self.consume_keyword(Keyword::Set).is_some() {
            if self.consume_keyword(Keyword::Data).is_some() {
                // ALTER COLUMN col SET DATA TYPE data_type [USING expr]
                self.expect_keyword(Keyword::Type)?;
                let type_start = self.index;
                let new_type = self.parse_data_type()?;
                let raw_type_name = extract_raw_type_name(&self.tokens[type_start..self.index]);
                let text_type_modifier =
                    infer_text_type_modifier(&self.tokens[type_start..self.index]);
                let mut span = self.previous_span();
                // Skip optional USING conversion_expr
                if self.consume_keyword(Keyword::Using).is_some() {
                    let using_expr = self.parse_expr()?;
                    span = span.merge(using_expr.span());
                }
                Ok(AlterTableAction::AlterColumnType {
                    column_name: column,
                    new_type,
                    raw_type_name,
                    text_type_modifier,
                    span,
                })
            } else if self.consume_keyword(Keyword::Default).is_some() {
                let expr = self.parse_expr()?;
                let span = expr.span();
                Ok(AlterTableAction::SetDefault {
                    column,
                    default: expr,
                    span,
                })
            } else if self.consume_keyword(Keyword::Not).is_some() {
                let null_token = self.expect_keyword(Keyword::Null)?;
                Ok(AlterTableAction::SetNotNull {
                    column,
                    span: null_token.span,
                })
            } else {
                self.syntax_error_current("expected DEFAULT, NOT NULL, or DATA TYPE after SET")
            }
        } else if self.consume_keyword(Keyword::Drop).is_some() {
            if self.consume_keyword(Keyword::Default).is_some() {
                let span = self.previous_span();
                Ok(AlterTableAction::DropDefault { column, span })
            } else if self.consume_keyword(Keyword::Not).is_some() {
                let null_token = self.expect_keyword(Keyword::Null)?;
                Ok(AlterTableAction::DropNotNull {
                    column,
                    span: null_token.span,
                })
            } else {
                self.syntax_error_current("expected DEFAULT or NOT NULL after DROP")
            }
        } else {
            self.syntax_error_current("expected SET, DROP, or TYPE after ALTER COLUMN name")
        }
    }

    /// Parse `ALTER TYPE name ADD VALUE ...`, `ALTER TYPE name RENAME ...`,
    /// `ALTER TYPE name SET (...)`, `ALTER TYPE name OWNER TO ...`, etc.
    ///
    /// Emits `Statement::AlterType`. The alter action (`ADD ATTRIBUTE`,
    /// `RENAME`, `OWNER TO`, …) is re-parsed from the raw SQL by the
    /// compatibility type handler.
    pub(super) fn parse_alter_type_statement(
        &mut self,
        alter: crate::tokens::Token,
    ) -> DbResult<Statement> {
        let type_kw = self.expect_keyword(Keyword::Type)?;
        let mut span = alter.span.merge(type_kw.span);
        let name = self.parse_object_name()?;
        span = span.merge(name.span);
        self.skip_to_semicolon(&mut span);
        let raw_sql = self
            .source
            .get(span.start..span.end)
            .unwrap_or_default()
            .to_owned();
        Ok(Statement::AlterType(crate::ast::TypeStatement {
            name,
            raw_sql,
            span,
        }))
    }

    pub(super) fn parse_drop_table_statement(
        &mut self,
        drop: crate::tokens::Token,
    ) -> DbResult<Statement> {
        let table = self.expect_keyword(Keyword::Table)?;
        let if_exists = self.consume_if_exists_clause()?;
        let name = self.parse_object_name()?;
        let mut span = drop.span.merge(table.span).merge(name.span);
        // Collect additional comma-separated table names: DROP TABLE t1, t2, t3
        let mut extra_names = Vec::new();
        while self.consume_kind(&TokenKind::Comma) {
            let extra = self.parse_object_name()?;
            span = span.merge(extra.span);
            extra_names.push(extra);
        }
        // Consume optional CASCADE/RESTRICT
        let mut cascade = false;
        if let Some(tok) = self.consume_drop_behavior_keyword() {
            if matches!(tok.kind, TokenKind::Keyword(Keyword::Cascade)) {
                cascade = true;
            }
            span = span.merge(tok.span);
        }
        Ok(Statement::DropTable(DropTableStatement {
            name: name.clone(),
            extra_names,
            if_exists,
            cascade,
            span,
        }))
    }

    pub(super) fn parse_drop_index_statement(
        &mut self,
        drop: crate::tokens::Token,
    ) -> DbResult<Statement> {
        let index = self.expect_keyword(Keyword::Index)?;
        let concurrently = self.consume_keyword(Keyword::Concurrently).is_some();
        let if_exists = self.consume_if_exists_clause()?;
        let name = self.parse_object_name()?;
        let mut span = drop.span.merge(index.span).merge(name.span);
        let mut extra_names = Vec::new();
        while self.consume_kind(&TokenKind::Comma) {
            let extra = self.parse_object_name()?;
            span = span.merge(extra.span);
            extra_names.push(extra);
        }
        if concurrently && !extra_names.is_empty() {
            return Err(DbError::bind_error(
                SqlState::FeatureNotSupported,
                "DROP INDEX CONCURRENTLY does not support dropping multiple objects",
            ));
        }
        // Consume optional CASCADE/RESTRICT
        if let Some(tok) = self.consume_drop_behavior_keyword() {
            span = span.merge(tok.span);
        }
        Ok(Statement::DropIndex(DropIndexStatement {
            name: name.clone(),
            extra_names,
            if_exists,
            span,
        }))
    }

    pub(super) fn parse_drop_sequence_statement(
        &mut self,
        drop: crate::tokens::Token,
    ) -> DbResult<Statement> {
        let sequence = self.expect_keyword(Keyword::Sequence)?;
        let if_exists = self.consume_if_exists_clause()?;
        let name = self.parse_object_name()?;
        let mut span = drop.span.merge(sequence.span).merge(name.span);
        // Skip additional comma-separated sequence names
        while self.consume_kind(&TokenKind::Comma) {
            let extra = self.parse_object_name()?;
            span = span.merge(extra.span);
        }
        // Consume optional CASCADE/RESTRICT
        if let Some(tok) = self.consume_drop_behavior_keyword() {
            span = span.merge(tok.span);
        }
        Ok(Statement::DropSequence(DropSequenceStatement {
            name: name.clone(),
            if_exists,
            span,
        }))
    }
    pub(crate) fn parse_alter_statement(&mut self) -> DbResult<Statement> {
        let alter = self.expect_keyword(Keyword::Alter)?;
        // ALTER SYSTEM SET name = value / ALTER SYSTEM RESET name|ALL
        if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("system"))
            || matches!(self.current().kind, TokenKind::Keyword(Keyword::System))
        {
            return self.parse_alter_system_statement(alter.span);
        }
        match self.current().kind {
            TokenKind::Keyword(Keyword::Table) => self.parse_alter_table_statement(alter),
            TokenKind::Keyword(Keyword::Role | Keyword::User | Keyword::Group) => {
                if matches!(&self.current().kind, TokenKind::Keyword(Keyword::User))
                    && self.index + 1 < self.tokens.len()
                    && matches!(&self.tokens[self.index + 1].kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("mapping"))
                {
                    let mut span = alter.span.merge(self.current().span);
                    self.advance(); // consume USER
                    self.skip_to_semicolon(&mut span);
                    let raw_sql = self
                        .source
                        .get(span.start..span.end)
                        .unwrap_or_default()
                        .to_owned();
                    return Ok(Statement::AlterUserMapping(
                        crate::ast::CompatSimpleCreateStatement { raw_sql, span },
                    ));
                }
                self.parse_alter_role_statement(alter)
            }
            TokenKind::Keyword(Keyword::Type) => self.parse_alter_type_statement(alter),
            // ALTER DATABASE name [options…]. Options are interpreted by
            // the engine from the raw SQL.
            TokenKind::Keyword(Keyword::Database) => {
                let mut span = alter.span.merge(self.current().span);
                self.advance(); // consume DATABASE
                let name = match &self.current().kind {
                    TokenKind::Identifier(s) => {
                        let name = s.clone();
                        span = span.merge(self.current().span);
                        self.advance();
                        name
                    }
                    TokenKind::Keyword(kw) => {
                        let name = kw.name().to_owned();
                        span = span.merge(self.current().span);
                        self.advance();
                        name
                    }
                    _ => {
                        return self
                            .syntax_error_current("expected database name after ALTER DATABASE");
                    }
                };
                self.skip_to_semicolon(&mut span);
                Ok(Statement::AlterDatabase(crate::ast::DatabaseStatement {
                    name,
                    span,
                }))
            }
            // ALTER INDEX / ALTER SEQUENCE stay on the compatibility utility
            // path so runtime can enforce the supported subset explicitly.
            TokenKind::Keyword(Keyword::Index | Keyword::Sequence) => {
                let kw = match &self.current().kind {
                    TokenKind::Keyword(kw) => kw.name().to_uppercase(),
                    _ => return self.syntax_error_current("unexpected ALTER target"),
                };
                let mut span = alter.span.merge(self.current().span);
                self.advance(); // consume INDEX/SEQUENCE
                self.skip_to_semicolon(&mut span);
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                // Validate column number for `ALTER INDEX X ALTER COLUMN N SET STATISTICS`.
                // PG enforces 1..=32767 on the column ordinal at parse time.
                if kw == "INDEX" {
                    if let Some(n) = extract_alter_index_set_statistics_colno(&raw_sql) {
                        if !(1..=32767).contains(&n) {
                            return Err(aiondb_core::DbError::parse_error(
                                aiondb_core::SqlState::InvalidParameterValue,
                                "column number must be in range from 1 to 32767",
                            ));
                        }
                    }
                }
                Ok(self.pg_compat_utility_statement(format!("ALTER {kw}"), None, raw_sql, span))
            }
            // ALTER DOMAIN name …; body is re-parsed from raw SQL by the
            // compatibility type handler.
            TokenKind::Keyword(Keyword::Domain) => {
                let mut span = alter.span.merge(self.current().span);
                self.advance(); // consume DOMAIN
                let name = self.parse_object_name()?;
                span = span.merge(name.span);
                self.skip_to_semicolon(&mut span);
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                Ok(Statement::AlterDomain(crate::ast::TypeStatement {
                    name,
                    raw_sql,
                    span,
                }))
            }
            // ALTER TRIGGER name ON table RENAME TO new_name
            TokenKind::Keyword(Keyword::Trigger) => {
                self.advance(); // consume TRIGGER
                                // Parse: name ON [ONLY] table RENAME TO new_name
                let (name, _) = self.expect_identifier()?;
                self.expect_keyword(Keyword::On)?;
                // Check for ONLY keyword (not supported, should error)
                if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("only"))
                    || matches!(self.current().kind, TokenKind::Keyword(Keyword::Only))
                {
                    // PG: syntax error at or near "only"
                    return self.syntax_error_current("syntax error at or near \"only\"");
                }
                let table = self.parse_object_name()?;
                // Expect RENAME TO
                if self.consume_keyword(Keyword::Rename).is_some() {
                    self.expect_keyword(Keyword::To)?;
                    let (new_name, new_name_span) = self.expect_identifier()?;
                    return Ok(Statement::AlterTriggerRename(
                        crate::ast::AlterTriggerRenameStatement {
                            name,
                            table,
                            new_name,
                            span: alter.span.merge(new_name_span),
                        },
                    ));
                }
                // Other ALTER TRIGGER forms (e.g. ENABLE/DISABLE) -
                // typed AST.
                let mut span = alter.span.merge(self.previous_span());
                self.skip_to_semicolon(&mut span);
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                Ok(Statement::AlterTriggerCompat(
                    crate::ast::CompatSimpleCreateStatement { raw_sql, span },
                ))
            }
            TokenKind::Keyword(
                Keyword::View
                | Keyword::Function
                | Keyword::Aggregate
                | Keyword::Operator
                | Keyword::Schema
                | Keyword::Materialized
                | Keyword::Foreign
                | Keyword::Procedure
                | Keyword::System
                | Keyword::Default
                | Keyword::Language
                | Keyword::Text
                | Keyword::Owned
                | Keyword::Statistics,
            ) => {
                let kw = match &self.current().kind {
                    TokenKind::Keyword(kw) => kw.name().to_uppercase(),
                    _ => return self.syntax_error_current("unexpected ALTER target"),
                };
                let mut span = alter.span.merge(self.current().span);
                self.advance(); // consume the keyword
                                // Multi-word families: FOREIGN {TABLE | DATA WRAPPER},
                                // MATERIALIZED VIEW, TEXT SEARCH {CONFIGURATION | DICTIONARY
                                // | PARSER | TEMPLATE} - fold the suffix into the tag so
                                // downstream dispatch matches the right entry in
                                // `compat_tag_matrix.rs`.
                let tag = match kw.as_str() {
                    "FOREIGN" => {
                        if self.consume_keyword(Keyword::Table).is_some()
                            || self
                                .current_is_identifier_ci("table")
                                .then(|| self.advance())
                                .is_some()
                        {
                            "ALTER FOREIGN TABLE".to_owned()
                        } else if matches!(&self.current().kind, TokenKind::Keyword(Keyword::Data))
                            || self.current_is_identifier_ci("data")
                        {
                            self.advance();
                            if self.current_is_identifier_ci("wrapper") {
                                self.advance();
                            }
                            "ALTER FOREIGN DATA WRAPPER".to_owned()
                        } else {
                            "ALTER FOREIGN".to_owned()
                        }
                    }
                    "MATERIALIZED" => {
                        let _ = self.consume_keyword(Keyword::View);
                        "ALTER MATERIALIZED VIEW".to_owned()
                    }
                    "TEXT" => {
                        if self.current_is_identifier_ci("search") {
                            self.advance();
                        }
                        "ALTER TEXT SEARCH".to_owned()
                    }
                    other => format!("ALTER {other}"),
                };
                self.skip_to_semicolon(&mut span);
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                match tag.as_str() {
                    "ALTER FOREIGN TABLE" => Ok(Statement::AlterForeignTable(
                        crate::ast::CompatSimpleCreateStatement { raw_sql, span },
                    )),
                    "ALTER FOREIGN DATA WRAPPER" => Ok(Statement::AlterForeignDataWrapper(
                        crate::ast::CompatSimpleCreateStatement { raw_sql, span },
                    )),
                    "ALTER STATISTICS" => Ok(Statement::AlterStatistics(
                        crate::ast::CompatSimpleCreateStatement { raw_sql, span },
                    )),
                    _ => Ok(self.pg_compat_utility_statement(tag, None, raw_sql, span)),
                }
            }
            // ALTER <identifier> for: EXTENSION, POLICY, RULE,
            // STATISTICS, COLLATION, CONVERSION, PUBLICATION, SUBSCRIPTION,
            TokenKind::Identifier(ref s)
                if s.eq_ignore_ascii_case("statistics")
                    || s.eq_ignore_ascii_case("policy")
                    || s.eq_ignore_ascii_case("extension")
                    || s.eq_ignore_ascii_case("rule")
                    || s.eq_ignore_ascii_case("collation")
                    || s.eq_ignore_ascii_case("conversion")
                    || s.eq_ignore_ascii_case("publication")
                    || s.eq_ignore_ascii_case("subscription")
                    || s.eq_ignore_ascii_case("server")
                    || s.eq_ignore_ascii_case("tablespace")
                    || s.eq_ignore_ascii_case("event")
                    || s.eq_ignore_ascii_case("access")
                    || s.eq_ignore_ascii_case("transform") =>
            {
                let name = match &self.current().kind {
                    TokenKind::Identifier(s) => s.to_uppercase(),
                    _ => return self.syntax_error_current("unexpected ALTER target"),
                };
                let mut span = alter.span.merge(self.current().span);
                self.advance(); // consume the identifier
                self.skip_to_semicolon(&mut span);
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                match name.as_str() {
                    "POLICY" => Ok(Statement::AlterPolicy(
                        crate::ast::CompatSimpleCreateStatement { raw_sql, span },
                    )),
                    "PUBLICATION" => Ok(Statement::AlterPublication(
                        crate::ast::CompatSimpleCreateStatement { raw_sql, span },
                    )),
                    "SUBSCRIPTION" => Ok(Statement::AlterSubscription(
                        crate::ast::CompatSimpleCreateStatement { raw_sql, span },
                    )),
                    "SERVER" => Ok(Statement::AlterServer(
                        crate::ast::CompatSimpleCreateStatement { raw_sql, span },
                    )),
                    "COLLATION" => Ok(Statement::AlterCollation(
                        crate::ast::CompatSimpleCreateStatement { raw_sql, span },
                    )),
                    "STATISTICS" => Ok(Statement::AlterStatistics(
                        crate::ast::CompatSimpleCreateStatement { raw_sql, span },
                    )),
                    "TABLESPACE" => Ok(Statement::AlterTablespace(
                        crate::ast::CompatSimpleCreateStatement { raw_sql, span },
                    )),
                    _ => Ok(self.compat_tagged_statement(format!("ALTER {name}"), raw_sql, span)),
                }
            }
            TokenKind::Identifier(ref s) => {
                let name = s.to_uppercase();
                let mut span = alter.span.merge(self.current().span);
                self.advance();
                self.skip_to_semicolon(&mut span);
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                Ok(self.compat_tagged_statement(format!("ALTER {name}"), raw_sql, span))
            }
            // EOF after bare ALTER → error
            TokenKind::Eof => self.syntax_error_current("expected TABLE or ROLE after ALTER"),
            _ => {
                let desc = match &self.current().kind {
                    TokenKind::Keyword(kw) => kw.name().to_uppercase(),
                    _ => format!("{:?}", self.current().kind),
                };
                let mut span = alter.span.merge(self.current().span);
                self.advance();
                self.skip_to_semicolon(&mut span);
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                Ok(self.compat_tagged_statement(format!("ALTER {desc}"), raw_sql, span))
            }
        }
    }

    pub(crate) fn parse_alter_system_statement(
        &mut self,
        alter_span: crate::Span,
    ) -> DbResult<Statement> {
        use crate::ast::{AlterSystemAction, AlterSystemStatement};
        let mut span = alter_span;
        match &self.current().kind {
            TokenKind::Identifier(s) if s.eq_ignore_ascii_case("system") => {
                span = span.merge(self.advance().span);
            }
            TokenKind::Keyword(Keyword::System) => {
                span = span.merge(self.advance().span);
            }
            _ => return self.syntax_error_current("expected SYSTEM after ALTER"),
        }

        let action = if self.consume_keyword(Keyword::Set).is_some() {
            let (name, name_span) = self.expect_identifier()?;
            span = span.merge(name_span);
            if matches!(self.current().kind, TokenKind::Eq) {
                span = span.merge(self.advance().span);
            } else {
                let _ = self.consume_keyword(Keyword::To);
            }
            let value = match &self.current().kind {
                TokenKind::String(s) => s.clone(),
                TokenKind::Identifier(s) => s.clone(),
                TokenKind::Integer(i) => i.to_string(),
                _ => String::new(),
            };
            if !matches!(self.current().kind, TokenKind::Semicolon | TokenKind::Eof) {
                span = span.merge(self.advance().span);
            }
            AlterSystemAction::Set { name, value }
        } else if matches!(self.current().kind, TokenKind::Keyword(Keyword::Reset))
            || matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("reset"))
        {
            span = span.merge(self.advance().span);
            if self.consume_keyword(Keyword::All).is_some() {
                span = span.merge(self.previous_span());
                AlterSystemAction::ResetAll
            } else {
                let (name, name_span) = self.expect_identifier()?;
                span = span.merge(name_span);
                AlterSystemAction::Reset { name }
            }
        } else {
            return self.syntax_error_current("expected SET or RESET after ALTER SYSTEM");
        };

        self.skip_to_semicolon(&mut span);
        Ok(Statement::AlterSystem(AlterSystemStatement {
            action,
            span,
        }))
    }

    pub(crate) fn parse_truncate_statement(&mut self) -> DbResult<Statement> {
        let truncate = self.expect_keyword(Keyword::Truncate)?;
        // TABLE keyword is optional (PostgreSQL allows both TRUNCATE TABLE t and TRUNCATE t)
        // ONLY keyword is also optional (TRUNCATE [TABLE] [ONLY] t)
        self.consume_keyword(Keyword::Table);
        self.consume_keyword(Keyword::Only);
        let name = self.parse_object_name()?;
        let mut span = truncate.span.merge(name.span);
        // Collect additional comma-separated table names: TRUNCATE t1, t2
        let mut extra_names = Vec::new();
        while self.consume_kind(&TokenKind::Comma) {
            let _ = self.consume_keyword(Keyword::Only);
            let extra = self.parse_object_name()?;
            span = span.merge(extra.span);
            extra_names.push(extra);
        }
        // Skip trailing CASCADE/RESTRICT/RESTART IDENTITY/CONTINUE IDENTITY
        while !self.is_eof() && !matches!(self.current().kind, TokenKind::Semicolon) {
            span = span.merge(self.advance().span);
        }
        Ok(Statement::TruncateTable(TruncateTableStatement {
            name: name.clone(),
            extra_names,
            span,
        }))
    }
}
