#![allow(clippy::collapsible_if)]

mod alter_drop;
mod create_other;
mod create_table;
mod drop_stmt;
mod table_elements;

use aiondb_core::{DbError, DbResult, SqlState};

use crate::{
    ast::{
        AlterTableAction, AlterTableStatement, ColumnDef, CreateExtensionStatement,
        CreateIndexStatement, CreateSequenceStatement, CreateTableStatement, CreateViewStatement,
        DropIndexStatement, DropSequenceStatement, DropTableStatement, DropViewStatement, Expr,
        Literal, ObjectName, PartitionBound, PartitionByClause, PartitionStrategy, SelectStatement,
        SetOperationStatement, SetOperationType, Statement, TableConstraint,
        TruncateTableStatement,
    },
    keywords::Keyword,
    tokens::TokenKind,
    Parser,
};

impl Parser {
    pub(crate) fn parse_create_statement(&mut self) -> DbResult<Statement> {
        // Check if this is a Cypher CREATE (followed by '(' for node pattern).
        if self.index + 1 < self.tokens.len()
            && matches!(self.tokens[self.index + 1].kind, TokenKind::LParen)
        {
            return Ok(Statement::Cypher(self.parse_cypher_statement()?));
        }

        let create = self.expect_keyword(Keyword::Create)?;

        // Handle CREATE OR REPLACE FUNCTION/VIEW/TRIGGER/...
        if self.current().kind == TokenKind::Keyword(Keyword::Or) {
            let saved = self.index;
            self.advance(); // consume OR
            if self.consume_keyword(Keyword::Replace).is_some() {
                // Parse optional CREATE modifiers.
                let mut temporary = false;
                let mut unsupported_modifier_span = None;
                while matches!(
                    self.current().kind,
                    TokenKind::Keyword(
                        Keyword::Temp
                            | Keyword::Temporary
                            | Keyword::Unlogged
                            | Keyword::Local
                            | Keyword::Global
                    )
                ) {
                    if matches!(
                        self.current().kind,
                        TokenKind::Keyword(Keyword::Temp | Keyword::Temporary)
                    ) {
                        temporary = true;
                    } else if unsupported_modifier_span.is_none() {
                        unsupported_modifier_span = Some(self.current().span);
                    }
                    self.advance();
                }
                let _ = self.consume_keyword(Keyword::Recursive);
                if let Some(mod_span) = unsupported_modifier_span {
                    return self.unsupported_feature(
                        mod_span,
                        "CREATE OR REPLACE with UNLOGGED/LOCAL/GLOBAL modifiers is not supported",
                    );
                }
                if self.current().kind == TokenKind::Keyword(Keyword::Function) {
                    if temporary {
                        return self.unsupported_feature(
                            create.span.merge(self.current().span),
                            "CREATE OR REPLACE TEMP FUNCTION is not supported",
                        );
                    }
                    return self.parse_create_function_statement(create, true);
                }
                if self.current().kind == TokenKind::Keyword(Keyword::View) {
                    return self.parse_create_view_statement(create, true, temporary);
                }
                if self.current().kind == TokenKind::Keyword(Keyword::Trigger) {
                    if temporary {
                        return self.unsupported_feature(
                            create.span.merge(self.current().span),
                            "CREATE OR REPLACE TEMP TRIGGER is not supported",
                        );
                    }
                    return self.parse_create_trigger_statement_with_replace(create, true);
                }
                if temporary {
                    return self.unsupported_feature(
                        create.span.merge(self.current().span),
                        "CREATE OR REPLACE TEMP is only supported for VIEW",
                    );
                }
                // Everything else: typed compat catch-all. The engine will
                // either route a supported family or fail during compat
                // preparation; do not emit generic CompatTagged here.
                let mut span = create.span.merge(self.current().span);
                self.skip_to_semicolon(&mut span);
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                return Ok(Statement::CreateOrReplaceCompat(
                    crate::ast::CompatOrReplaceStatement { raw_sql, span },
                ));
            }
            // Not OR REPLACE, backtrack
            self.index = saved;
        }

        let mut saw_modifier = false;
        let mut temporary = false;
        let mut unlogged = false;
        while matches!(
            self.current().kind,
            TokenKind::Keyword(
                Keyword::Temp
                    | Keyword::Temporary
                    | Keyword::Unlogged
                    | Keyword::Local
                    | Keyword::Global
            )
        ) {
            saw_modifier = true;
            if matches!(
                self.current().kind,
                TokenKind::Keyword(Keyword::Temp | Keyword::Temporary)
            ) {
                temporary = true;
            }
            if matches!(self.current().kind, TokenKind::Keyword(Keyword::Unlogged)) {
                unlogged = true;
            }
            self.advance();
        }
        if saw_modifier
            && !matches!(
                self.current().kind,
                TokenKind::Keyword(
                    Keyword::Table
                        | Keyword::View
                        | Keyword::Recursive
                        | Keyword::Sequence
                        | Keyword::Index
                        | Keyword::Materialized
                        | Keyword::Foreign
                )
            )
        {
            return self.unsupported_feature(
                create.span.merge(self.previous_span()),
                "CREATE with TEMP/TEMPORARY/UNLOGGED/LOCAL/GLOBAL modifiers is not supported for this object type",
            );
        }
        if self.consume_keyword(Keyword::Recursive).is_some() {
            if self.current().kind == TokenKind::Keyword(Keyword::View) {
                return self.parse_create_view_statement(create, false, temporary);
            }
        }

        match self.current().kind {
            TokenKind::Keyword(Keyword::Table) => {
                self.parse_create_table_statement(create, temporary, unlogged)
            }
            TokenKind::Keyword(Keyword::Unique) => {
                // CREATE UNIQUE INDEX ...
                self.advance(); // consume UNIQUE
                self.parse_create_index_statement(create, true)
            }
            TokenKind::Keyword(Keyword::Index) => self.parse_create_index_statement(create, false),
            TokenKind::Keyword(Keyword::Sequence) => self.parse_create_sequence_statement(create),
            TokenKind::Keyword(Keyword::View) => {
                self.parse_create_view_statement(create, false, temporary)
            }
            TokenKind::Keyword(Keyword::Node) => self.parse_create_node_label_statement(create),
            TokenKind::Keyword(Keyword::Edge) => self.parse_create_edge_label_statement(create),
            TokenKind::Keyword(Keyword::Role | Keyword::User | Keyword::Group) => {
                // CREATE USER MAPPING FOR ... - typed AST. Body
                // re-parsed from `raw_sql`.
                if matches!(&self.current().kind, TokenKind::Keyword(Keyword::User))
                    && self.index + 1 < self.tokens.len()
                    && matches!(&self.tokens[self.index + 1].kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("mapping"))
                {
                    let mut span = create.span.merge(self.current().span);
                    self.advance(); // consume USER
                    self.skip_to_semicolon(&mut span);
                    let raw_sql = self
                        .source
                        .get(span.start..span.end)
                        .unwrap_or_default()
                        .to_owned();
                    return Ok(Statement::CreateUserMapping(
                        crate::ast::CompatSimpleCreateStatement { raw_sql, span },
                    ));
                }
                self.parse_create_role_statement(create)
            }
            TokenKind::Keyword(Keyword::Function) => {
                self.parse_create_function_statement(create, false)
            }
            TokenKind::Keyword(Keyword::Trigger) => {
                self.parse_create_trigger_statement(create)
            }
            TokenKind::Keyword(Keyword::Constraint)
                if self.index + 1 < self.tokens.len()
                    && matches!(self.tokens[self.index + 1].kind, TokenKind::Keyword(Keyword::Trigger)) =>
            {
                self.parse_create_trigger_statement(create)
            }
            TokenKind::Keyword(Keyword::Tenant) => self.parse_create_tenant_statement(create),
            TokenKind::Keyword(Keyword::Schema) => self.parse_create_schema_statement(create),
            TokenKind::Keyword(Keyword::Type) => self.parse_create_type_statement(create),
            // CREATE DATABASE [IF NOT EXISTS] name [options…]. The option
            // tail is interpreted by the engine from the raw SQL.
            TokenKind::Keyword(Keyword::Database) => {
                let mut span = create.span.merge(self.current().span);
                self.advance(); // consume DATABASE
                // Optional IF NOT EXISTS
                if self.consume_keyword(Keyword::If).is_some() {
                    self.expect_keyword(Keyword::Not)?;
                    self.expect_keyword(Keyword::Exists)?;
                }
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
                    _ => return self.syntax_error_current("expected database name after CREATE DATABASE"),
                };
                self.skip_to_semicolon(&mut span);
                Ok(Statement::CreateDatabase(crate::ast::DatabaseStatement {
                    name,
                    span,
                }))
            }
            // CREATE DOMAIN name …; the body (AS type, DEFAULT, CHECK, …)
            // is re-parsed from `raw_sql` by the compatibility type handler.
            TokenKind::Keyword(Keyword::Domain) => {
                let mut span = create.span.merge(self.current().span);
                self.advance(); // consume DOMAIN
                let name = self.parse_object_name()?;
                span = span.merge(name.span);
                self.skip_to_semicolon(&mut span);
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                Ok(Statement::CreateDomain(crate::ast::TypeStatement {
                    name,
                    raw_sql,
                    span,
                }))
            }
            // CREATE AGGREGATE ... - typed AST.
            TokenKind::Keyword(Keyword::Aggregate) => {
                let mut span = create.span.merge(self.current().span);
                self.advance(); // consume AGGREGATE
                self.skip_to_semicolon(&mut span);
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                Ok(Statement::CreateAggregate(
                    crate::ast::CompatSimpleCreateStatement { raw_sql, span },
                ))
            }
            // validation (missing rightarg/procedure, SETOF args, invalid
            // attributes emit WARNING, etc.)
            TokenKind::Keyword(Keyword::Operator) => {
                let mut span = create.span.merge(self.current().span);
                self.advance(); // consume OPERATOR
                // CREATE OPERATOR FAMILY / CLASS → typed.
                if matches!(&self.current().kind,
                    TokenKind::Identifier(s) if s.eq_ignore_ascii_case("family") || s.eq_ignore_ascii_case("class"))
                {
                    self.skip_to_semicolon(&mut span);
                    let raw_sql = self
                        .source
                        .get(span.start..span.end)
                        .unwrap_or_default()
                        .to_owned();
                    return Ok(Statement::CreateOperator(
                        crate::ast::CompatSimpleCreateStatement { raw_sql, span },
                    ));
                }
                self.parse_create_operator_body(&mut span)
            }
            // CREATE MATERIALIZED VIEW name AS SELECT ... → treated as CREATE TABLE AS
            TokenKind::Keyword(Keyword::Materialized) => {
                self.advance(); // consume MATERIALIZED
                if !matches!(self.current().kind, TokenKind::Keyword(Keyword::View)) {
                    return self.unsupported_feature(
                        self.previous_span(),
                        "CREATE MATERIALIZED requires VIEW",
                    );
                }
                self.advance(); // consume VIEW
                // IF NOT EXISTS
                if self.consume_keyword(Keyword::If).is_some() {
                    self.expect_keyword(Keyword::Not)?;
                    self.expect_keyword(Keyword::Exists)?;
                }
                let name = self.parse_object_name()?;
                // Skip optional column aliases: CREATE MATERIALIZED VIEW v(col1, col2) AS ...
                if matches!(self.current().kind, TokenKind::LParen)
                    && !matches!(self.current().kind, TokenKind::Keyword(Keyword::As))
                {
                    // Peek ahead: if this looks like a column alias list (not a WITH/USING option),
                    // skip the balanced parens
                    let saved = self.index;
                    self.advance(); // consume '('
                    let mut depth: u32 = 1;
                    let mut looks_like_aliases = true;
                    while depth > 0 && !self.is_eof() {
                        match self.current().kind {
                            TokenKind::LParen => { depth += 1; self.advance(); }
                            TokenKind::RParen => { depth -= 1; self.advance(); }
                            TokenKind::Eof => break,
                            _ => { self.advance(); }
                        }
                    }
                    // If after closing paren we see AS, USING, WITH, or TABLESPACE, it was aliases
                    if !matches!(
                        self.current().kind,
                        TokenKind::Keyword(Keyword::As | Keyword::Using | Keyword::With)
                    ) && !matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("tablespace"))
                    {
                        looks_like_aliases = false;
                    }
                    if !looks_like_aliases {
                        self.index = saved;
                    }
                }
                // Skip optional USING <access_method>
                if self.consume_keyword(Keyword::Using).is_some() {
                    if matches!(
                        self.current().kind,
                        TokenKind::Identifier(_) | TokenKind::Keyword(_)
                    ) {
                        self.advance();
                    }
                }
                // Skip optional WITH (option, ...) clause
                if self.consume_keyword(Keyword::With).is_some() {
                    if matches!(self.current().kind, TokenKind::LParen) {
                        let mut depth = 1;
                        self.advance(); // consume '('
                        while depth > 0 {
                            match self.current().kind {
                                TokenKind::LParen => { depth += 1; self.advance(); }
                                TokenKind::RParen => { depth -= 1; self.advance(); }
                                TokenKind::Eof => break,
                                _ => { self.advance(); }
                            }
                        }
                    }
                }
                // Skip optional TABLESPACE name
                if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("tablespace")) {
                    self.advance(); // skip TABLESPACE
                    let _ = self.expect_identifier()?; // skip tablespace name
                }
                self.expect_keyword(Keyword::As)?;
                let inner_result = if matches!(self.current().kind, TokenKind::Keyword(Keyword::Values)) {
                    self.parse_values_as_statement()
                } else if matches!(self.current().kind, TokenKind::Keyword(Keyword::With)) {
                    self.parse_with_statement()
                } else {
                    self.parse_select_statement()
                };
                match inner_result {
                    Ok(Statement::Select(query)) => {
                        let mut span = create.span.merge(name.span).merge(query.span);
                        // Skip optional WITH [NO] DATA
                        let mut with_no_data = false;
                        if self.consume_keyword(Keyword::With).is_some() {
                            if self.consume_keyword(Keyword::No).is_some() {
                                with_no_data = true;
                            }
                            if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("data")) {
                                self.advance();
                            }
                        }
                        self.skip_to_semicolon(&mut span);
                        Ok(Statement::CreateTableAs(
                            crate::ast::CreateTableAsStatement {
                                name,
                                query,
                                temporary: false,
                                if_not_exists: false,
                                with_no_data,
                                column_aliases: Vec::new(),
                                span,
                            },
                        ))
                    }
                    Ok(_) | Err(_) => {
                        self.unsupported_feature(
                            create.span.merge(name.span),
                            "CREATE MATERIALIZED VIEW requires a SELECT query",
                        )
                    }
                }
            }
            // CREATE FOREIGN TABLE ... - typed AST.
            TokenKind::Keyword(Keyword::Foreign) => {
                let mut span = create.span.merge(self.current().span);
                self.advance(); // consume FOREIGN
                let is_data_wrapper = matches!(&self.current().kind, TokenKind::Keyword(Keyword::Data))
                    || self.current_is_identifier_ci("data");
                if is_data_wrapper {
                    self.advance();
                    if self.current_is_identifier_ci("wrapper") {
                        self.advance();
                    }
                    self.skip_to_semicolon(&mut span);
                    let raw_sql = self
                        .source
                        .get(span.start..span.end)
                        .unwrap_or_default()
                        .to_owned();
                    return Ok(Statement::CreateForeignDataWrapper(
                        crate::ast::CompatSimpleCreateStatement { raw_sql, span },
                    ));
                }
                self.skip_to_semicolon(&mut span);
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                Ok(Statement::CreateForeignTable(
                    crate::ast::CompatSimpleCreateStatement { raw_sql, span },
                ))
            }
            // CREATE CAST (src AS tgt) WITH …; body is re-parsed from
            // `raw_sql` by the compatibility type handler.
            TokenKind::Keyword(Keyword::Cast) => {
                let mut span = create.span.merge(self.current().span);
                self.advance(); // consume CAST
                self.skip_to_semicolon(&mut span);
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                Ok(Statement::CreateCast(crate::ast::CompatCastStatement {
                    raw_sql,
                    if_exists: false,
                    span,
                }))
            }
            // CREATE PROCEDURE keeps the raw SQL for the compatibility layer.
            TokenKind::Keyword(Keyword::Procedure) => {
                let mut span = create.span.merge(self.current().span);
                self.advance(); // consume PROCEDURE
                self.skip_to_semicolon(&mut span);
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                Ok(Statement::CreateProcedure(
                    crate::ast::CompatSimpleCreateStatement { raw_sql, span },
                ))
            }
            // CREATE LANGUAGE / SYSTEM / TEXT / DEFAULT - compatibility
            // ExplicitNotSupported).
            TokenKind::Keyword(
                Keyword::Language
                | Keyword::System
                | Keyword::Text
                | Keyword::Default,
            ) => {
                let is_text_search = matches!(self.current().kind, TokenKind::Keyword(Keyword::Text))
                    && self.index + 1 < self.tokens.len()
                    && matches!(
                        self.tokens[self.index + 1].kind,
                        TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("search")
                    );
                let kw = match &self.current().kind {
                    TokenKind::Keyword(kw) => kw.name().to_uppercase(),
                    _ => return self.syntax_error_current("unexpected CREATE target"),
                };
                let mut span = create.span.merge(self.current().span);
                self.advance(); // consume the keyword
                let tag = if is_text_search {
                    span = span.merge(self.current().span);
                    self.advance(); // consume SEARCH
                    "CREATE TEXT SEARCH".to_owned()
                } else {
                    format!("CREATE {kw}")
                };
                self.skip_to_semicolon(&mut span);
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                Ok(self.pg_compat_utility_statement(tag, None, raw_sql, span))
            }
            // Handle CREATE <identifier> for: EXTENSION, POLICY,
            // COLLATION, STATISTICS, SERVER, CONVERSION, TABLESPACE,
            // PUBLICATION, ACCESS METHOD, TRANSFORM, EVENT TRIGGER, etc.
            // CREATE RULE name …; body is re-parsed from raw SQL by
            // `compat::rules`.
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("rule") => {
                let mut span = create.span.merge(self.current().span);
                self.advance(); // consume RULE
                let name = match &self.current().kind {
                    TokenKind::Identifier(n) => {
                        let name = n.clone();
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
                    _ => String::new(),
                };
                self.skip_to_semicolon(&mut span);
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                Ok(Statement::CreateRule(crate::ast::CompatRuleStatement {
                    name,
                    if_exists: false,
                    raw_sql,
                    span,
                }))
            }
            // CREATE STATISTICS ... - typed AST.
            TokenKind::Keyword(Keyword::Statistics) => {
                let mut span = create.span.merge(self.current().span);
                self.advance(); // consume STATISTICS
                self.validate_create_statistics_body()?;
                span = span.merge(self.previous_span());
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                Ok(Statement::CreateStatistics(
                    crate::ast::CompatSimpleCreateStatement { raw_sql, span },
                ))
            }
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("statistics") => {
                let mut span = create.span.merge(self.current().span);
                self.advance(); // consume STATISTICS
                self.validate_create_statistics_body()?;
                span = span.merge(self.previous_span());
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                Ok(Statement::CreateStatistics(
                    crate::ast::CompatSimpleCreateStatement { raw_sql, span },
                ))
            }
            // CREATE POLICY ... - typed AST. Body re-parsed from
            // `raw_sql` by compat::ddl.
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("policy") => {
                let mut span = create.span.merge(self.current().span);
                self.advance(); // consume POLICY
                self.skip_to_semicolon(&mut span);
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                Ok(Statement::CreatePolicy(
                    crate::ast::CompatSimpleCreateStatement { raw_sql, span },
                ))
            }
            // CREATE EXTENSION [IF NOT EXISTS] name [WITH] [SCHEMA schema] [VERSION version] [CASCADE]
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("extension") => {
                let mut span = create.span.merge(self.current().span);
                self.advance(); // consume EXTENSION

                // IF NOT EXISTS
                let if_not_exists = if self.consume_keyword(Keyword::If).is_some() {
                    let _ = self.consume_keyword(Keyword::Not);
                    let _ = self.consume_keyword(Keyword::Exists);
                    true
                } else {
                    false
                };

                // Extension name (identifier or string)
                let name = match &self.current().kind {
                    TokenKind::Identifier(_) | TokenKind::Keyword(_) => {
                        let token = self.advance();
                        let n = match token.kind {
                            TokenKind::Identifier(n) => n,
                            TokenKind::Keyword(kw) => kw.name().to_lowercase(),
                            _ => {
                                return self.syntax_error(token.span, "expected extension name");
                            }
                        };
                        span = span.merge(token.span);
                        n
                    }
                    TokenKind::String(_) => {
                        let token = self.advance();
                        let TokenKind::String(n) = token.kind else {
                            return self.syntax_error(token.span, "expected extension name");
                        };
                        span = span.merge(token.span);
                        n
                    }
                    _ => return Err(self.unexpected_token("extension name")),
                };

                // Optional WITH keyword (ignored per PG spec)
                let _ = self.consume_keyword(Keyword::With);

                // Parse optional clauses: SCHEMA, VERSION, CASCADE
                let mut schema = None;
                let mut version = None;
                let mut cascade = false;
                loop {
                    match &self.current().kind {
                        TokenKind::Keyword(Keyword::Schema) => {
                            span = span.merge(self.advance().span);
                            let (s, s_span) = self.expect_identifier()?;
                            span = span.merge(s_span);
                            schema = Some(s);
                        }
                        TokenKind::Keyword(Keyword::Cascade) => {
                            span = span.merge(self.advance().span);
                            cascade = true;
                        }
                        TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("version") => {
                            span = span.merge(self.advance().span);
                            match &self.current().kind {
                                TokenKind::String(_) => {
                                    let token = self.advance();
                                    let TokenKind::String(v) = token.kind else {
                                        return self.syntax_error(
                                            token.span,
                                            "expected version string",
                                        );
                                    };
                                    span = span.merge(token.span);
                                    version = Some(v);
                                }
                                TokenKind::Identifier(_) => {
                                    let token = self.advance();
                                    let TokenKind::Identifier(v) = token.kind else {
                                        return self.syntax_error(
                                            token.span,
                                            "expected version string",
                                        );
                                    };
                                    span = span.merge(token.span);
                                    version = Some(v);
                                }
                                _ => return Err(self.unexpected_token("version string")),
                            }
                        }
                        _ => break,
                    }
                }

                Ok(Statement::CreateExtension(CreateExtensionStatement {
                    name,
                    if_not_exists,
                    schema,
                    version,
                    cascade,
                    span,
                }))
            }
            // CREATE CONVERSION ... → unsupported. No CompatCommand should be
            // introduced without a typed Statement variant.
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("conversion") => {
                self.unsupported_feature(self.current().span, "CREATE CONVERSION is not supported")
            }
            // CREATE COLLATION ... - typed AST.
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("collation") => {
                let mut span = create.span.merge(self.current().span);
                self.advance(); // consume COLLATION
                self.skip_to_semicolon(&mut span);
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                Ok(Statement::CreateCollation(
                    crate::ast::CompatSimpleCreateStatement { raw_sql, span },
                ))
            }
            // CREATE ACCESS METHOD ... → unsupported.
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("access") => {
                self.unsupported_feature(self.current().span, "CREATE ACCESS METHOD is not supported")
            }
            // CREATE PUBLICATION ... - typed AST.
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("publication") => {
                let mut span = create.span.merge(self.current().span);
                self.advance(); // consume PUBLICATION
                self.skip_to_semicolon(&mut span);
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                Ok(Statement::CreatePublication(
                    crate::ast::CompatSimpleCreateStatement { raw_sql, span },
                ))
            }
            // CREATE SERVER ... - typed AST.
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("server") => {
                let mut span = create.span.merge(self.current().span);
                self.advance(); // consume SERVER
                self.skip_to_semicolon(&mut span);
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                Ok(Statement::CreateServer(
                    crate::ast::CompatSimpleCreateStatement { raw_sql, span },
                ))
            }
            // CREATE TABLESPACE ... - typed AST.
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("tablespace") => {
                let mut span = create.span.merge(self.current().span);
                self.advance(); // consume TABLESPACE
                self.skip_to_semicolon(&mut span);
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                Ok(Statement::CreateTablespace(
                    crate::ast::CompatSimpleCreateStatement { raw_sql, span },
                ))
            }
            // CREATE TRANSFORM ... → unsupported.
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("transform") => {
                self.unsupported_feature(self.current().span, "CREATE TRANSFORM is not supported")
            }
            // CREATE EVENT TRIGGER ... → unsupported.
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("event") => {
                self.unsupported_feature(self.current().span, "CREATE EVENT TRIGGER is not supported")
            }
            // CREATE SUBSCRIPTION ... - typed AST.
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("subscription") => {
                let mut span = create.span.merge(self.current().span);
                self.advance(); // consume SUBSCRIPTION
                self.skip_to_semicolon(&mut span);
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                Ok(Statement::CreateSubscription(
                    crate::ast::CompatSimpleCreateStatement { raw_sql, span },
                ))
            }
            // CREATE TYPE ... → typed AST.
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("type") => {
                let mut span = create.span.merge(self.current().span);
                self.advance(); // consume TYPE
                let name = self.parse_object_name()?;
                span = span.merge(name.span);
                self.skip_to_semicolon(&mut span);
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                Ok(Statement::CreateType(crate::ast::TypeStatement {
                    name,
                    raw_sql,
                    span,
                }))
            }
            // many object kinds we don't model; clients rely on this being a
            TokenKind::Identifier(ref s) => {
                if matches!(
                    self.tokens.get(self.index + 1).map(|t| &t.kind),
                    Some(TokenKind::Semicolon | TokenKind::Eof)
                ) {
                    return self.syntax_error_at_current_token();
                }
                let desc = s.to_uppercase();
                let mut span = create.span.merge(self.current().span);
                self.advance();
                self.skip_to_semicolon(&mut span);
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                Ok(self.compat_tagged_statement(format!("CREATE {desc}"), raw_sql, span))
            }
            // EOF after bare CREATE → error
            TokenKind::Eof => self.syntax_error_current(
                "expected TABLE, INDEX, SEQUENCE, VIEW, SCHEMA, NODE LABEL, EDGE LABEL, ROLE, FUNCTION, TRIGGER, or TENANT after CREATE",
            ),
            _ => {
                let desc = match &self.current().kind {
                    TokenKind::Keyword(kw) => kw.name().to_uppercase(),
                    _ => format!("{:?}", self.current().kind),
                };
                let mut span = create.span.merge(self.current().span);
                self.advance();
                self.skip_to_semicolon(&mut span);
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                Ok(self.compat_tagged_statement(format!("CREATE {desc}"), raw_sql, span))
            }
        }
    }

    /// Validate the body of `CREATE STATISTICS …` after the
    /// `CREATE STATISTICS` keywords have been consumed.
    ///
    /// Mirrors PostgreSQL syntax: `[IF NOT EXISTS] [name] [(kind, ...)]
    /// ON expr [, ...] FROM table`. Returns a syntax/feature error at
    /// the offending token when the body is malformed; on success the
    /// cursor is positioned just after the FROM table identifier.
    pub(crate) fn validate_create_statistics_body(&mut self) -> DbResult<()> {
        let _ = self.consume_if_not_exists_clause()?;

        let has_name = matches!(
            self.current().kind,
            TokenKind::Identifier(_) | TokenKind::Keyword(_)
        ) && !matches!(self.current().kind, TokenKind::Keyword(Keyword::On))
            && !matches!(self.current().kind, TokenKind::Keyword(Keyword::From))
            && !self.current_is_identifier_ci("on");
        if has_name {
            let _ = self.parse_object_name()?;
        }

        if matches!(self.current().kind, TokenKind::LParen) {
            self.advance();
            loop {
                let (ident, _) = self.expect_identifier()?;
                let lc = ident.to_ascii_lowercase();
                if !matches!(lc.as_str(), "ndistinct" | "dependencies" | "mcv") {
                    return Err(DbError::bind_error(
                        SqlState::FeatureNotSupported,
                        format!("unrecognized statistics kind \"{lc}\""),
                    ));
                }
                if !self.consume_kind(&TokenKind::Comma) {
                    break;
                }
            }
            self.expect_token(&TokenKind::RParen)?;
        }

        if !matches!(self.current().kind, TokenKind::Keyword(Keyword::On)) {
            return self.syntax_error_at_current_token();
        }
        self.advance();

        loop {
            self.validate_statistics_key_item()?;
            if !self.consume_kind(&TokenKind::Comma) {
                break;
            }
        }

        if !matches!(self.current().kind, TokenKind::Keyword(Keyword::From)) {
            return self.syntax_error_at_current_token();
        }
        self.advance();

        let _ = self.parse_object_name()?;
        Ok(())
    }

    fn validate_statistics_key_item(&mut self) -> DbResult<()> {
        if matches!(self.current().kind, TokenKind::LParen) {
            self.advance();
            let _ = self.parse_expr()?;
            if matches!(self.current().kind, TokenKind::Comma) {
                return self.syntax_error_at_current_token();
            }
            self.expect_token(&TokenKind::RParen)?;
            Ok(())
        } else if matches!(
            self.current().kind,
            TokenKind::Identifier(_) | TokenKind::Keyword(_)
        ) {
            let ident_idx = self.index;
            let _ = self.expect_identifier()?;
            if matches!(self.current().kind, TokenKind::LParen) {
                self.index = ident_idx;
                let _ = self.parse_expr()?;
                return Ok(());
            }
            match self.current().kind {
                TokenKind::Comma | TokenKind::Keyword(Keyword::From) | TokenKind::Semicolon => {
                    Ok(())
                }
                _ => self.syntax_error_at_current_token(),
            }
        } else {
            self.syntax_error_at_current_token()
        }
    }
}
