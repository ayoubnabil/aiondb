#![allow(
    clippy::collapsible_if,
    clippy::doc_markdown,
    clippy::redundant_closure
)]

use super::*;
use crate::identifier::is_system_column_name;

fn pgvector_operator_class_name(name: &str) -> Option<String> {
    let class_name = name.rsplit('.').next().unwrap_or(name).to_ascii_lowercase();
    match class_name.as_str() {
        "vector_l2_ops"
        | "vector_ip_ops"
        | "vector_cosine_ops"
        | "vector_l1_ops"
        | "halfvec_l2_ops"
        | "halfvec_ip_ops"
        | "halfvec_cosine_ops"
        | "halfvec_l1_ops"
        | "sparsevec_l2_ops"
        | "sparsevec_ip_ops"
        | "sparsevec_cosine_ops"
        | "sparsevec_l1_ops"
        | "bit_hamming_ops"
        | "bit_jaccard_ops" => Some(class_name),
        _ => None,
    }
}

impl Parser {
    pub(super) fn parse_create_index_statement(
        &mut self,
        create: crate::tokens::Token,
        unique: bool,
    ) -> DbResult<Statement> {
        let index = self.expect_keyword(Keyword::Index)?;
        // Skip CONCURRENTLY if present
        let concurrently = self.consume_keyword(Keyword::Concurrently).is_some();
        let if_not_exists = self.consume_if_not_exists_clause()?;
        // Index name is optional: CREATE INDEX ON table (...) is valid
        let auto_name = self.current().kind == TokenKind::Keyword(Keyword::On);
        if if_not_exists && auto_name {
            return self.syntax_error_current("syntax error at or near \"ON\"");
        }
        let placeholder_name = if auto_name {
            ObjectName {
                parts: vec!["__placeholder__".to_owned()],
                span: index.span,
            }
        } else {
            self.parse_object_name()?
        };
        let on = self.expect_keyword(Keyword::On)?;
        // Skip optional ONLY (e.g., CREATE INDEX ... ON ONLY table_name ...)
        let _ = self.consume_keyword(Keyword::Only);
        let table = self.parse_object_name()?;

        // Optional: USING method. Keep parser coverage aligned with
        // compatibility suites; unsupported methods should be rejected at
        // planning/binding time when needed.
        let method = if self.consume_keyword(Keyword::Using).is_some() {
            let (method_name, _) = self.expect_identifier()?;
            match method_name.to_ascii_lowercase().as_str() {
                "btree" => Some(crate::ast::IndexMethod::BTree),
                "hnsw" => Some(crate::ast::IndexMethod::Hnsw),
                "ivfflat" | "ivf_flat" => Some(crate::ast::IndexMethod::IvfFlat),
                "gin" => Some(crate::ast::IndexMethod::Gin),
                "gist" => Some(crate::ast::IndexMethod::Gist),
                "spgist" => Some(crate::ast::IndexMethod::SpGist),
                "brin" => Some(crate::ast::IndexMethod::Brin),
                "hash" => Some(crate::ast::IndexMethod::Hash),
                other => {
                    return self.unsupported_feature(
                        create.span,
                        format!("index method '{other}' is not supported"),
                    );
                }
            }
        } else {
            None
        };

        let mut name = placeholder_name;

        self.expect_token(&TokenKind::LParen)?;

        let mut columns = Vec::new();
        let mut key_expressions: Vec<Option<String>> = Vec::new();
        let mut operator_classes: Vec<Option<String>> = Vec::new();
        loop {
            // Expression indexes: skip parenthesized expressions
            if self.current().kind == TokenKind::LParen {
                // Skip balanced parens to consume the expression
                let start_span = self.current().span;
                let mut depth = 0i32;
                let mut saw_row_record = false;
                let mut saw_system_col = false;
                loop {
                    match &self.current().kind {
                        TokenKind::Keyword(Keyword::Row) => {
                            saw_row_record = true;
                            self.advance();
                        }
                        TokenKind::Identifier(ident) => {
                            if ident.eq_ignore_ascii_case("row") {
                                saw_row_record = true;
                            }
                            if is_system_column_name(ident) {
                                saw_system_col = true;
                            }
                            self.advance();
                        }
                        TokenKind::LParen => {
                            depth += 1;
                            self.advance();
                        }
                        TokenKind::RParen if depth > 1 => {
                            depth -= 1;
                            self.advance();
                        }
                        TokenKind::RParen => {
                            self.advance();
                            break;
                        }
                        TokenKind::Eof => break,
                        _ => {
                            self.advance();
                        }
                    }
                }
                let placeholder = if saw_row_record {
                    "__expr_row_record__"
                } else if saw_system_col {
                    "__expr_system_col__"
                } else {
                    "__expr__"
                };
                let expression_sql = self
                    .source
                    .get(start_span.start + 1..self.previous_span().end.saturating_sub(1))
                    .map(str::trim)
                    .filter(|expr| !expr.is_empty())
                    .map(ToOwned::to_owned);
                columns.push(ObjectName {
                    parts: vec![placeholder.to_owned()],
                    span: start_span,
                });
                key_expressions.push(expression_sql);
            } else {
                let col = self.parse_object_name()?;
                // If '(' follows, this is an expression index (e.g., lower(col)) - skip balanced parens
                // and use a placeholder name instead of the function name.
                if self.current().kind == TokenKind::LParen {
                    let expr_span = col.span;
                    let expr_start = expr_span.start;
                    let mut saw_row_record = col
                        .parts
                        .last()
                        .is_some_and(|name| name.eq_ignore_ascii_case("row"));
                    let mut saw_system_col = col
                        .parts
                        .last()
                        .is_some_and(|name| is_system_column_name(name));
                    let mut depth = 1u32;
                    self.advance(); // consume '('
                    while !self.is_eof() && depth > 0 {
                        match &self.current().kind {
                            TokenKind::Keyword(Keyword::Row) => {
                                saw_row_record = true;
                            }
                            TokenKind::Identifier(ident) => {
                                if ident.eq_ignore_ascii_case("row") {
                                    saw_row_record = true;
                                }
                                if is_system_column_name(ident) {
                                    saw_system_col = true;
                                }
                            }
                            TokenKind::LParen => depth += 1,
                            TokenKind::RParen => depth -= 1,
                            _ => {}
                        }
                        self.advance();
                    }
                    let placeholder = if saw_row_record {
                        "__expr_row_record__"
                    } else if saw_system_col {
                        "__expr_system_col__"
                    } else {
                        "__expr__"
                    };
                    let expression_sql = self
                        .source
                        .get(expr_start..self.previous_span().end)
                        .map(str::trim)
                        .filter(|expr| !expr.is_empty())
                        .map(ToOwned::to_owned);
                    columns.push(ObjectName {
                        parts: vec![placeholder.to_owned()],
                        span: expr_span,
                    });
                    key_expressions.push(expression_sql);
                } else {
                    columns.push(col);
                    key_expressions.push(None);
                }
            }
            // Skip optional operator class names (with optional params), ASC/DESC, NULLS FIRST/LAST
            let mut operator_class = None;
            loop {
                match &self.current().kind {
                    TokenKind::Keyword(Keyword::Asc | Keyword::Desc) => {
                        self.advance();
                    }
                    TokenKind::Keyword(Keyword::Nulls) => {
                        self.advance();
                        let _ = self.consume_keyword(Keyword::First);
                        let _ = self.consume_keyword(Keyword::Last);
                    }
                    TokenKind::Identifier(ident) => {
                        let mut name = ident.clone();
                        self.advance();
                        while self.consume_kind(&TokenKind::Dot) {
                            if let Ok((suffix, _)) = self.expect_identifier() {
                                name = format!("{name}.{suffix}");
                            } else {
                                break;
                            }
                        }
                        if operator_class.is_none() {
                            operator_class = pgvector_operator_class_name(&name);
                        }
                        // Operator class may have parameters: ops(key = value, ...)
                        if self.current().kind == TokenKind::LParen {
                            let mut depth = 1u32;
                            self.advance(); // consume '('
                            while !self.is_eof() && depth > 0 {
                                match self.current().kind {
                                    TokenKind::LParen => depth += 1,
                                    TokenKind::RParen => depth -= 1,
                                    _ => {}
                                }
                                self.advance();
                            }
                        }
                    }
                    _ => break,
                }
            }
            operator_classes.push(operator_class);
            if self.consume_kind(&TokenKind::Comma) {
                continue;
            }
            break;
        }

        let rparen = self.expect_token(&TokenKind::RParen)?;
        let mut span_end = rparen.span;

        let mut nulls_not_distinct = false;
        // Optional: NULLS [NOT] DISTINCT (PostgreSQL 15+)
        if matches!(self.current().kind, TokenKind::Keyword(Keyword::Nulls)) {
            self.advance(); // NULLS
            let has_not = self.consume_keyword(Keyword::Not).is_some(); // optional NOT
            if matches!(self.current().kind, TokenKind::Keyword(Keyword::Distinct)) {
                self.advance();
                nulls_not_distinct = has_not;
            }
            span_end = self.previous_span();
        }

        // Optional: INCLUDE (col1, col2, ...) - covering index columns
        if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("include"))
        {
            self.advance(); // skip INCLUDE
            if self.consume_kind(&TokenKind::LParen) {
                loop {
                    let _ = self.parse_object_name()?;
                    if !self.consume_kind(&TokenKind::Comma) {
                        break;
                    }
                }
                let inc_rparen = self.expect_token(&TokenKind::RParen)?;
                span_end = inc_rparen.span;
            }
        }

        // Optional: WITH (key = value, ...)
        let mut with_options = Vec::new();
        if self.consume_keyword(Keyword::With).is_some() {
            if self.consume_kind(&TokenKind::LParen) {
                loop {
                    let (mut key, _) = self.expect_identifier()?;
                    // Handle dotted keys: ns.key (e.g., not_existing_ns.fillfactor)
                    while self.consume_kind(&TokenKind::Dot) {
                        if let Ok((suffix, _)) = self.expect_identifier() {
                            key = format!("{key}.{suffix}");
                        }
                    }
                    self.expect_token(&TokenKind::Eq)?;
                    let value = match &self.current().kind {
                        TokenKind::Integer(n) => {
                            let value = crate::ast::IndexOptionValue::Integer(*n);
                            self.advance();
                            value
                        }
                        TokenKind::String(s) => {
                            let value = crate::ast::IndexOptionValue::String(s.clone());
                            self.advance();
                            value
                        }
                        _ => {
                            // Non-integer / non-string values (booleans, etc.)
                            // - skip the token and record it as a zero-valued
                            // integer to preserve existing behaviour.
                            self.advance();
                            crate::ast::IndexOptionValue::Integer(0)
                        }
                    };
                    with_options.push(crate::ast::IndexWithOption { key, value });
                    if self.consume_kind(&TokenKind::Comma) {
                        continue;
                    }
                    break;
                }
                let with_rparen = self.expect_token(&TokenKind::RParen)?;
                span_end = with_rparen.span;
            }
        }

        // Optional: TABLESPACE name
        if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("tablespace"))
        {
            self.advance(); // skip TABLESPACE
            let _ = self.expect_identifier()?; // skip tablespace name
            span_end = self.previous_span();
        }

        // Optional: WHERE condition (partial index)
        if self.consume_keyword(Keyword::Where).is_some() {
            let saved = self.index;
            match self.parse_expr() {
                Ok(_) => {
                    span_end = self.previous_span();
                }
                Err(_) => {
                    // Complex WHERE expression - skip to end
                    self.index = saved;
                    self.skip_to_semicolon(&mut span_end);
                }
            }
        }

        if auto_name {
            let table_name = table.parts.last().cloned().unwrap_or_default();
            let key_name_parts = columns
                .iter()
                .filter_map(|column| column.parts.last().cloned())
                .filter(|name| {
                    !matches!(
                        name.as_str(),
                        "__expr__" | "__expr_row_record__" | "__expr_system_col__"
                    )
                })
                .collect::<Vec<_>>();
            let auto_part = if key_name_parts.is_empty() {
                "idx".to_owned()
            } else {
                format!("{}_idx", key_name_parts.join("_"))
            };
            name = ObjectName {
                parts: vec![format!("{table_name}_{auto_part}")],
                span: index.span,
            };
        }

        Ok(Statement::CreateIndex(CreateIndexStatement {
            name: name.clone(),
            table: table.clone(),
            columns,
            key_expressions,
            operator_classes,
            method,
            unique,
            concurrently,
            nulls_not_distinct,
            with_options,
            if_not_exists,
            span: create
                .span
                .merge(index.span)
                .merge(name.span)
                .merge(on.span)
                .merge(table.span)
                .merge(span_end),
        }))
    }

    pub(super) fn parse_create_sequence_statement(
        &mut self,
        create: crate::tokens::Token,
    ) -> DbResult<Statement> {
        let sequence = self.expect_keyword(Keyword::Sequence)?;
        let if_not_exists = self.consume_if_not_exists_clause()?;
        let name = self.parse_object_name()?;
        // Skip trailing sequence options: INCREMENT [BY] n, MINVALUE n,
        // MAXVALUE n, START [WITH] n, CACHE n, [NO] CYCLE, OWNED BY ...,
        // AS type, USING method, RESTART [WITH n], etc.
        let mut span_end = name.span;
        self.skip_to_semicolon(&mut span_end);
        Ok(Statement::CreateSequence(CreateSequenceStatement {
            name: name.clone(),
            if_not_exists,
            span: create.span.merge(sequence.span).merge(span_end),
        }))
    }

    pub(super) fn parse_create_view_statement(
        &mut self,
        create: crate::tokens::Token,
        or_replace: bool,
        temporary: bool,
    ) -> DbResult<Statement> {
        let view = self.expect_keyword(Keyword::View)?;
        let if_not_exists = self.consume_if_not_exists_clause()?;
        let name = self.parse_object_name()?;
        // Parse optional column aliases: CREATE VIEW v (col1, col2) AS ...
        let column_aliases = if self.current().kind == TokenKind::LParen
            && !matches!(self.current().kind, TokenKind::Keyword(Keyword::As))
        {
            self.advance(); // consume '('
            let mut aliases = Vec::new();
            loop {
                match &self.current().kind {
                    TokenKind::RParen => {
                        self.advance();
                        break;
                    }
                    TokenKind::Comma => {
                        self.advance();
                    }
                    TokenKind::Identifier(s) => {
                        aliases.push(s.clone());
                        self.advance();
                    }
                    TokenKind::Keyword(kw) => {
                        // Keywords used as identifiers (e.g., "value", "name")
                        aliases.push(kw.name().to_lowercase());
                        self.advance();
                    }
                    TokenKind::Eof => break,
                    _ => {
                        self.advance();
                    }
                }
            }
            aliases
        } else {
            Vec::new()
        };
        // Skip optional USING <access_method> (e.g., CREATE VIEW v USING heap2 AS ...)
        if self.consume_keyword(Keyword::Using).is_some() {
            // Consume the access method name
            if matches!(
                self.current().kind,
                TokenKind::Identifier(_) | TokenKind::Keyword(_)
            ) {
                self.advance();
            }
        }
        // Skip optional WITH (option, ...) clause
        if self.consume_keyword(Keyword::With).is_some() {
            if self.consume_kind(&TokenKind::LParen) {
                let mut depth = 1;
                while depth > 0 {
                    match self.current().kind {
                        TokenKind::LParen => {
                            depth += 1;
                            self.advance();
                        }
                        TokenKind::RParen => {
                            depth -= 1;
                            self.advance();
                        }
                        TokenKind::Eof => break,
                        _ => {
                            self.advance();
                        }
                    }
                }
            }
        }
        self.expect_keyword(Keyword::As)?;
        let inner_result = if matches!(self.current().kind, TokenKind::Keyword(Keyword::Values)) {
            self.parse_values_as_statement()
                .and_then(|stmt| self.maybe_parse_set_operation(stmt))
        } else if matches!(self.current().kind, TokenKind::Keyword(Keyword::With)) {
            self.parse_with_statement()
                .and_then(|stmt| self.maybe_parse_set_operation(stmt))
        } else {
            self.parse_select_statement()
                .and_then(|stmt| self.maybe_parse_set_operation(stmt))
        };
        match inner_result {
            Ok(Statement::Select(query)) => {
                let mut span = create
                    .span
                    .merge(view.span)
                    .merge(name.span)
                    .merge(query.span);
                let check_option = self.parse_trailing_view_check_option(&mut span);
                self.skip_to_semicolon(&mut span);
                Ok(Statement::CreateView(CreateViewStatement {
                    name,
                    query,
                    temporary,
                    if_not_exists,
                    or_replace,
                    column_aliases,
                    override_sql: None,
                    check_option,
                    span,
                }))
            }
            Ok(Statement::SetOperation(set_op)) => {
                // Multi-row VALUES or other set operations.
                // Preserve the original query text when possible. This keeps
                // complex expressions intact (instead of debug-format fallbacks)
                // and avoids reparsing invalid view SQL later.
                let sql = self
                    .source
                    .get(set_op.span.start..set_op.span.end)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map_or_else(|| reconstruct_set_op_sql(&set_op), ToOwned::to_owned);
                let dummy_query = extract_first_select(&set_op);
                let mut span = create
                    .span
                    .merge(view.span)
                    .merge(name.span)
                    .merge(set_op.span);
                let check_option = self.parse_trailing_view_check_option(&mut span);
                self.skip_to_semicolon(&mut span);
                Ok(Statement::CreateView(CreateViewStatement {
                    name,
                    query: dummy_query,
                    temporary,
                    if_not_exists,
                    or_replace,
                    column_aliases,
                    override_sql: Some(sql),
                    check_option,
                    span,
                }))
            }
            Ok(_) => {
                // Complex query that we can't parse - unsupported
                self.unsupported_feature(
                    create.span,
                    "CREATE VIEW with this query form is not supported",
                )
            }
            Err(error) => Err(error),
        }
    }

    fn parse_trailing_view_check_option(
        &mut self,
        span: &mut crate::Span,
    ) -> Option<crate::ast::ViewCheckOptionClause> {
        self.consume_keyword(Keyword::With)?;
        let mode = if self.consume_keyword(Keyword::Local).is_some() {
            Some(crate::ast::ViewCheckOptionClause::Local)
        } else if self.current_is_identifier_ci("cascaded") {
            self.advance();
            Some(crate::ast::ViewCheckOptionClause::Cascaded)
        } else {
            Some(crate::ast::ViewCheckOptionClause::Cascaded)
        }?;
        if let Some(token) = self.consume_keyword(Keyword::Check) {
            *span = span.merge(token.span);
        } else {
            return None;
        }
        if self.current_is_identifier_ci("option") {
            let token = self.advance();
            *span = span.merge(token.span);
            return Some(mode);
        }
        None
    }

    pub(super) fn parse_drop_view_statement(
        &mut self,
        drop: crate::tokens::Token,
    ) -> DbResult<Statement> {
        let view = self.expect_keyword(Keyword::View)?;
        // Skip optional MATERIALIZED keyword
        let _ = self.consume_keyword(Keyword::Materialized);
        let if_exists = self.consume_if_exists_clause()?;
        let name = self.parse_object_name()?;
        let mut span = drop.span.merge(view.span).merge(name.span);
        // Skip additional comma-separated view names
        while self.consume_kind(&TokenKind::Comma) {
            let extra = self.parse_object_name()?;
            span = span.merge(extra.span);
        }
        // Consume optional CASCADE/RESTRICT
        if let Some(tok) = self.consume_drop_behavior_keyword() {
            span = span.merge(tok.span);
        }
        Ok(Statement::DropView(DropViewStatement {
            name: name.clone(),
            if_exists,
            span,
        }))
    }

    /// Parse `CREATE TYPE name AS (field type, ...)` (composite),
    /// `CREATE TYPE name AS ENUM ('val', ...)` (enum),
    ///
    /// Emits `Statement::CreateType` with the parsed object name. The
    /// composite / enum body is re-parsed from the raw SQL by the
    /// compatibility handler.
    pub(super) fn parse_create_type_statement(
        &mut self,
        create: crate::tokens::Token,
    ) -> DbResult<Statement> {
        let type_kw = self.expect_keyword(Keyword::Type)?;
        let mut span = create.span.merge(type_kw.span);
        let name = self.parse_object_name()?;
        span = span.merge(name.span);
        // Shell type: CREATE TYPE name; (no AS, no parenthesized body).
        // Fallthrough to the generic branch which also consumes to `;`.
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

    /// Parse `DROP TYPE [IF EXISTS] name [, ...] [CASCADE | RESTRICT]`.
    ///
    /// Emits `Statement::DropType`. The `notice` payload mirrors the
    /// historical compatibility-stub behaviour: for IF EXISTS the parser
    /// pre-formats the
    /// `type "<name>" does not exist, skipping` message so the compat
    /// handler can surface it verbatim if the catalog lookup misses.
    pub(super) fn parse_drop_type_statement(
        &mut self,
        drop: crate::tokens::Token,
        if_exists_from_caller: bool,
    ) -> DbResult<Statement> {
        let type_kw = self.expect_keyword(Keyword::Type)?;
        let mut span = drop.span.merge(type_kw.span);
        let if_exists = self.consume_if_exists_clause_with_preconsumed(if_exists_from_caller)?;
        let first = self.parse_object_name()?;
        span = span.merge(first.span);
        let obj_name = first.parts.last().cloned().unwrap_or_default();
        let mut names = vec![first];
        while self.consume_kind(&TokenKind::Comma) {
            let extra = self.parse_object_name()?;
            span = span.merge(extra.span);
            names.push(extra);
        }
        let notice = if if_exists {
            Some(format!("type \"{obj_name}\" does not exist, skipping"))
        } else {
            None
        };
        let raw_sql = self
            .source
            .get(span.start..span.end)
            .unwrap_or_default()
            .to_owned();
        Ok(Statement::DropType(crate::ast::DropTypeStatement {
            names,
            if_exists,
            notice,
            raw_sql,
            span,
        }))
    }

    /// Parse the body of `CREATE OPERATOR <name> (...)` and validate PG rules.
    ///
    /// The `OPERATOR` keyword has already been consumed; `span` reflects the
    /// `CREATE OPERATOR` tokens. Emits PG-compatible errors and warnings:
    /// - `ERROR: syntax error at or near "=>"` when the operator name is `=>`.
    /// - `ERROR: operator right argument type must be specified` for postfix
    ///   operators (leftarg only).
    /// - `ERROR: SETOF type not allowed for operator argument` for SETOF args.
    /// - `ERROR: operator argument types must be specified` when neither arg
    ///   is provided.
    /// - `ERROR: operator function must be specified` when no procedure/
    ///   function attribute is present.
    /// - `WARNING: operator attribute "<name>" not recognized` for unknown
    ///   attributes (multiple WARNINGs are joined with '\n' in the notice).
    pub(super) fn parse_create_operator_body(
        &mut self,
        span: &mut crate::span::Span,
    ) -> DbResult<Statement> {
        // `=>` is reserved in PG; mimic the syntax error before consuming body.
        if matches!(self.current().kind, TokenKind::FatArrow) {
            return Err(DbError::syntax_error("syntax error at or near \"=>\"")
                .with_position(self.current().span.start + 1));
        }
        // Consume the operator name: a run of operator-character tokens,
        // optionally schema-qualified (ident '.' op).  Walk until '('.
        while !self.is_eof() && !matches!(self.current().kind, TokenKind::LParen) {
            *span = span.merge(self.advance().span);
        }
        // Attribute list
        let mut warnings: Vec<String> = Vec::new();
        let mut has_leftarg = false;
        let mut has_rightarg = false;
        let mut leftarg_setof = false;
        let mut rightarg_setof = false;
        let mut has_proc = false;
        if matches!(self.current().kind, TokenKind::LParen) {
            *span = span.merge(self.advance().span); // consume '('
            let mut depth: u32 = 1;
            while depth > 0 && !self.is_eof() {
                let attr_name_opt: Option<String> = match &self.current().kind {
                    TokenKind::Identifier(name) => Some(name.clone()),
                    TokenKind::Keyword(k) => Some(k.name().to_owned()),
                    _ => None,
                };
                if let Some(name) = attr_name_opt {
                    *span = span.merge(self.advance().span);
                    if matches!(self.current().kind, TokenKind::Eq) {
                        *span = span.merge(self.advance().span); // consume '='
                        let lower = name.to_ascii_lowercase();
                        let value_is_setof =
                            matches!(self.current().kind, TokenKind::Keyword(Keyword::Setof));
                        match lower.as_str() {
                            "leftarg" => {
                                has_leftarg = true;
                                leftarg_setof = value_is_setof;
                            }
                            "rightarg" => {
                                has_rightarg = true;
                                rightarg_setof = value_is_setof;
                            }
                            "procedure" | "function" => {
                                has_proc = true;
                            }
                            "commutator" | "negator" | "restrict" | "join" | "hashes"
                            | "merges" | "sort1" | "sort2" | "ltcmp" | "gtcmp" => {
                                // known attributes: accepted but not evaluated
                            }
                            _ => {
                                warnings.push(format!(
                                    "WARNING: operator attribute \"{name}\" not recognized"
                                ));
                            }
                        }
                    }
                } else {
                    match self.current().kind {
                        TokenKind::LParen => depth += 1,
                        TokenKind::RParen => {
                            depth -= 1;
                            if depth == 0 {
                                *span = span.merge(self.advance().span);
                                break;
                            }
                        }
                        _ => {}
                    }
                    *span = span.merge(self.advance().span);
                }
            }
        }
        self.skip_to_semicolon(span);

        // Validation (errors take precedence over warnings).
        if leftarg_setof || rightarg_setof {
            return Err(DbError::parse_error(
                SqlState::FeatureNotSupported,
                "SETOF type not allowed for operator argument",
            ));
        }
        if has_leftarg && !has_rightarg {
            return Err(DbError::parse_error(
                SqlState::FeatureNotSupported,
                "operator right argument type must be specified",
            )
            .with_client_detail("Postfix operators are not supported."));
        }
        if !has_leftarg && !has_rightarg {
            return Err(DbError::parse_error(
                SqlState::InvalidParameterValue,
                "operator argument types must be specified",
            ));
        }
        if !has_proc {
            return Err(DbError::parse_error(
                SqlState::InvalidParameterValue,
                "operator function must be specified",
            ));
        }

        let _notice = if warnings.is_empty() {
            None
        } else {
            Some(warnings.join("\n"))
        };
        let raw_sql = self
            .source
            .get(span.start..span.end)
            .unwrap_or_default()
            .to_owned();
        Ok(Statement::CreateOperator(
            crate::ast::CompatSimpleCreateStatement {
                raw_sql,
                span: *span,
            },
        ))
    }
}

/// Reconstruct a SQL string from a `SetOperationStatement`.
fn reconstruct_set_op_sql(set_op: &SetOperationStatement) -> String {
    fn fmt_stmt(stmt: &Statement) -> String {
        match stmt {
            Statement::Select(sel) => fmt_select(sel),
            Statement::SetOperation(sop) => reconstruct_set_op_sql(sop),
            _ => String::new(),
        }
    }

    fn fmt_select(sel: &SelectStatement) -> String {
        let mut s = String::from("SELECT ");
        for (i, item) in sel.items.iter().enumerate() {
            if i > 0 {
                s.push_str(", ");
            }
            s.push_str(&fmt_expr(&item.expr));
            if let Some(ref alias) = item.alias {
                s.push_str(" AS ");
                s.push_str(alias);
            }
        }
        if let Some(ref from) = sel.from {
            s.push_str(" FROM ");
            s.push_str(&from.parts.join("."));
        }
        s
    }

    fn fmt_expr(expr: &Expr) -> String {
        match expr {
            Expr::Literal(lit, _) => match lit {
                Literal::Integer(n) => n.to_string(),
                Literal::String(s) => format!("'{}'", s.replace('\'', "''")),
                Literal::Boolean(b) => if *b { "TRUE" } else { "FALSE" }.to_owned(),
                Literal::Null => "NULL".to_owned(),
                Literal::NumericLit(s) => s.clone(),
            },
            Expr::Identifier(name) => name.parts.join("."),
            Expr::UnaryOp {
                op: crate::ast::UnaryOperator::Minus,
                expr: inner,
                ..
            } => format!("(-{})", fmt_expr(inner)),
            Expr::Cast {
                expr: inner,
                data_type,
                ..
            } => format!("CAST({} AS {})", fmt_expr(inner), data_type),
            _ => format!("{expr:?}"),
        }
    }

    let op_str = match set_op.op {
        SetOperationType::Union => {
            if set_op.all {
                " UNION ALL "
            } else {
                " UNION "
            }
        }
        SetOperationType::Intersect => {
            if set_op.all {
                " INTERSECT ALL "
            } else {
                " INTERSECT "
            }
        }
        SetOperationType::Except => {
            if set_op.all {
                " EXCEPT ALL "
            } else {
                " EXCEPT "
            }
        }
    };

    format!(
        "{}{}{}",
        fmt_stmt(&set_op.left),
        op_str,
        fmt_stmt(&set_op.right)
    )
}

/// Extract the first `SelectStatement` from a set operation tree (left-deep).
fn extract_first_select(set_op: &SetOperationStatement) -> SelectStatement {
    match set_op.left.as_ref() {
        Statement::Select(sel) => sel.clone(),
        Statement::SetOperation(inner) => extract_first_select(inner),
        _ => SelectStatement {
            row_lock: None,
            ctes: Vec::new(),
            distinct: crate::ast::DistinctKind::All,
            items: Vec::new(),
            from: None,
            from_alias: None,
            from_span: None,
            joins: Vec::new(),
            selection: None,
            where_span: None,
            group_by: Vec::new(),
            group_by_items: Vec::new(),
            group_by_span: None,
            having: None,
            having_span: None,
            window_definitions: Vec::new(),
            order_by: Vec::new(),
            order_by_span: None,
            limit: None,
            limit_span: None,
            offset: None,
            offset_span: None,
            span: set_op.span,
        },
    }
}
