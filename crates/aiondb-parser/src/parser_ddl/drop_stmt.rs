use aiondb_core::{DbError, DbResult};

use crate::{
    ast::{DropExtensionStatement, DropTableStatement, Statement},
    keywords::Keyword,
    tokens::TokenKind,
    Parser,
};

impl Parser {
    pub(crate) fn parse_drop_statement(&mut self) -> DbResult<Statement> {
        let drop = self.expect_keyword(Keyword::Drop)?;
        // Handle DROP IF EXISTS for any target type by consuming IF EXISTS early
        // then falling through to normal dispatch.
        let saved_idx = self.index;
        let mut if_exists_consumed = false;
        if self.consume_keyword(Keyword::If).is_some() {
            if self.consume_keyword(Keyword::Exists).is_some() {
                // IF EXISTS consumed - continue to the target keyword below.
                if_exists_consumed = true;
            } else {
                // Just IF without EXISTS - backtrack
                self.index = saved_idx;
            }
        }
        let result = match self.current().kind {
            TokenKind::Keyword(Keyword::Table) => self.parse_drop_table_statement(drop),
            TokenKind::Keyword(Keyword::Index) => self.parse_drop_index_statement(drop),
            TokenKind::Keyword(Keyword::Sequence) => self.parse_drop_sequence_statement(drop),
            TokenKind::Keyword(Keyword::View) => self.parse_drop_view_statement(drop),
            TokenKind::Keyword(Keyword::Node) => self.parse_drop_node_label_statement(drop),
            TokenKind::Keyword(Keyword::Edge) => self.parse_drop_edge_label_statement(drop),
            TokenKind::Keyword(Keyword::Role | Keyword::Group) => {
                self.parse_drop_role_statement(drop)
            }
            TokenKind::Keyword(Keyword::Function) => {
                self.parse_drop_function_statement(drop, if_exists_consumed)
            }
            TokenKind::Keyword(Keyword::Trigger) => {
                // Pass the if_exists flag consumed at the top
                let mut result = self.parse_drop_trigger_statement(drop)?;
                if if_exists_consumed {
                    if let Statement::DropTrigger(ref mut s) = result {
                        s.if_exists = true;
                    }
                }
                Ok(result)
            }
            TokenKind::Keyword(Keyword::Tenant) => self.parse_drop_tenant_statement(drop),
            TokenKind::Keyword(Keyword::Schema) => self.parse_drop_schema_statement(drop),
            TokenKind::Keyword(Keyword::Type) => {
                self.parse_drop_type_statement(drop, if_exists_consumed)
            }
            TokenKind::Keyword(Keyword::Owned) => self.parse_drop_owned_tail(drop.span),
            // DROP DATABASE [IF EXISTS] name. Flags the IF EXISTS bit so
            // the engine can decide between skip-notice and `UndefinedObject`.
            TokenKind::Keyword(Keyword::Database) => {
                let mut span = drop.span.merge(self.current().span);
                self.advance(); // consume DATABASE
                let mut if_exists = if_exists_consumed;
                if !if_exists && self.consume_keyword(Keyword::If).is_some() {
                    self.expect_keyword(Keyword::Exists)?;
                    if_exists = true;
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
                    _ => return self.syntax_error_current("expected database name after DROP DATABASE"),
                };
                self.skip_to_semicolon(&mut span);
                Ok(Statement::DropDatabase(
                    crate::ast::DropDatabaseStatement { name, if_exists, span },
                ))
            }
            // DROP DOMAIN [IF EXISTS] name [, …]. Body is re-parsed from
            // raw SQL by the compatibility type handler.
            TokenKind::Keyword(Keyword::Domain) => {
                let mut span = drop.span.merge(self.current().span);
                self.advance(); // consume DOMAIN
                let if_exists =
                    self.consume_if_exists_lenient_with_preconsumed(if_exists_consumed);
                let notice = if if_exists {
                    let name = self.peek_noop_object_name();
                    Some(format!("type \"{name}\" does not exist, skipping"))
                } else {
                    None
                };
                // The rest of the statement (name list, CASCADE /
                // RESTRICT) is captured in the span; compat::types will
                // re-parse from the raw SQL.
                self.skip_to_semicolon(&mut span);
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                Ok(Statement::DropDomain(crate::ast::DropTypeStatement {
                    names: Vec::new(),
                    if_exists,
                    notice,
                    raw_sql,
                    span,
                }))
            }
            TokenKind::Keyword(Keyword::User) => {
                if self.index + 1 < self.tokens.len()
                    && matches!(&self.tokens[self.index + 1].kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("mapping"))
                {
                    let mut span = drop.span.merge(self.current().span);
                    self.advance(); // consume USER
                    self.skip_to_semicolon(&mut span);
                    let raw_sql = self
                        .source
                        .get(span.start..span.end)
                        .unwrap_or_default()
                        .to_owned();
                    return Ok(Statement::DropUserMapping(
                        crate::ast::CompatSimpleCreateStatement { raw_sql, span },
                    ));
                }
                self.parse_drop_role_statement(drop)
            }
            TokenKind::Keyword(Keyword::Aggregate) => {
                let mut span = drop.span.merge(self.current().span);
                self.advance(); // consume AGGREGATE
                let if_exists =
                    self.consume_if_exists_lenient_with_preconsumed(if_exists_consumed);
                if matches!(self.current().kind, TokenKind::Semicolon | TokenKind::Eof) {
                    return self.syntax_error_at_current_token();
                }
                if matches!(self.current().kind, TokenKind::Integer(_)) {
                    return self.syntax_error_at_current_token();
                }
                let name = self.parse_object_name()?;
                let aggregate_name = name.parts.join(".");
                span = span.merge(name.span);
                if !self.consume_kind(&TokenKind::LParen) {
                    return self.syntax_error_at_current_token();
                }
                span = span.merge(self.previous_span());
                if matches!(self.current().kind, TokenKind::RParen) {
                    return self.syntax_error_at_current_token();
                }
                // Parse argument list shape so malformed statements fail with syntax errors.
                while !self.is_eof() && !matches!(self.current().kind, TokenKind::RParen) {
                    self.advance();
                }
                if !self.consume_kind(&TokenKind::RParen) {
                    return self.syntax_error_at_current_token();
                }
                span = span.merge(self.previous_span());
                let _notice = if if_exists {
                    Some(format!("aggregate {aggregate_name}() does not exist, skipping"))
                } else {
                    None
                };
                self.skip_to_semicolon(&mut span);
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                Ok(Statement::DropAggregate(
                    crate::ast::CompatSimpleCreateStatement { raw_sql, span },
                ))
            }
            TokenKind::Keyword(Keyword::Operator) => {
                let mut span = drop.span.merge(self.current().span);
                self.advance(); // consume OPERATOR
                let if_exists =
                    self.consume_if_exists_lenient_with_preconsumed(if_exists_consumed);
                if matches!(
                    &self.current().kind,
                    TokenKind::Identifier(s)
                        if s.eq_ignore_ascii_case("class") || s.eq_ignore_ascii_case("family")
                ) {
                    let notice = if if_exists {
                        let name = self.peek_noop_object_name();
                        Some(format!("operator {name} does not exist, skipping"))
                    } else {
                        None
                    };
                    self.skip_to_semicolon(&mut span);
                    let raw_sql = self
                        .source
                        .get(span.start..span.end)
                        .unwrap_or_default()
                        .to_owned();
                    let _ = notice;
                    return Ok(Statement::DropOperator(
                        crate::ast::CompatSimpleCreateStatement {
                            raw_sql,
                            span,
                        },
                    ));
                }
                if matches!(self.current().kind, TokenKind::Semicolon | TokenKind::Eof) {
                    return self.syntax_error_at_current_token();
                }
                if matches!(self.current().kind, TokenKind::LParen) {
                    return self.syntax_error_at_current_token();
                }
                let operator_name_start = self.index;
                while !self.is_eof() && !matches!(self.current().kind, TokenKind::LParen) {
                    if matches!(self.current().kind, TokenKind::Semicolon | TokenKind::Comma) {
                        return self.syntax_error_at_current_token();
                    }
                    self.advance();
                    span = span.merge(self.previous_span());
                }
                let operator_name_end = self.index;
                if !self.consume_kind(&TokenKind::LParen) {
                    return self.syntax_error_at_current_token();
                }
                let operator_name = if operator_name_end > operator_name_start {
                    let start = self.tokens[operator_name_start].span.start;
                    let end = self.tokens[operator_name_end - 1].span.end;
                    self.source
                        .get(start..end)
                        .map_or_else(
                            || {
                                crate::tokens::tokens_to_sql(
                                    &self.tokens[operator_name_start..operator_name_end],
                                )
                            },
                            str::to_owned,
                        )
                        .replace(char::is_whitespace, "")
                } else {
                    String::new()
                };
                span = span.merge(self.previous_span());
                if matches!(self.current().kind, TokenKind::Comma | TokenKind::RParen) {
                    return self.syntax_error_at_current_token();
                }

                // First operand type.
                let arg1_start = self.index;
                while !self.is_eof()
                    && !matches!(self.current().kind, TokenKind::Comma | TokenKind::RParen)
                {
                    self.advance();
                }
                let arg1_end = self.index;
                if matches!(self.current().kind, TokenKind::RParen) {
                    return Err(
                        DbError::syntax_error("missing argument")
                            .with_position(self.current().span.start + 1)
                            .with_client_hint(
                                "Use NONE to denote the missing argument of a unary operator.",
                            ),
                    );
                }
                if !self.consume_kind(&TokenKind::Comma) {
                    return self.syntax_error_at_current_token();
                }
                if matches!(self.current().kind, TokenKind::RParen) {
                    return self.syntax_error_at_current_token();
                }
                // Second operand type.
                let arg2_start = self.index;
                while !self.is_eof() && !matches!(self.current().kind, TokenKind::RParen) {
                    self.advance();
                }
                let arg2_end = self.index;
                if !self.consume_kind(&TokenKind::RParen) {
                    return self.syntax_error_at_current_token();
                }
                span = span.merge(self.previous_span());
                // Without IF EXISTS, PG raises a hard error (postfix_not_supported
                // or operator_does_not_exist). AionDB does not persist an
                // operator catalog so we mimic PG's messages for the non-postfix
                // reference.
                if !if_exists {
                    fn pg_fmt(raw: &str) -> String {
                        let t = raw.trim().to_ascii_lowercase();
                        if t == "none" {
                            return String::new();
                        }
                        match t.as_str() {
                            "int4" | "integer" | "int" => "integer".to_owned(),
                            "int8" | "bigint" => "bigint".to_owned(),
                            "int2" | "smallint" => "smallint".to_owned(),
                            "float4" | "real" => "real".to_owned(),
                            "float8" | "double" => "double precision".to_owned(),
                            "bool" | "boolean" => "boolean".to_owned(),
                            other => other.to_owned(),
                        }
                    }
                    let arg1 = crate::tokens::tokens_to_sql(
                        &self.tokens[arg1_start..arg1_end],
                    )
                    .replace(' ', "");
                    let arg2 = crate::tokens::tokens_to_sql(
                        &self.tokens[arg2_start..arg2_end],
                    )
                    .replace(' ', "");
                    let left = pg_fmt(&arg1);
                    let right = pg_fmt(&arg2);
                    if !left.is_empty() && right.is_empty() {
                        return Err(DbError::parse_error(
                            aiondb_core::SqlState::FeatureNotSupported,
                            "postfix operators are not supported",
                        ));
                    }
                    // Prefix unary form (`NONE, type`) and infix form are
                    // persist an operator catalog to prove existence here.
                    let _ = (operator_name, left, right);
                    self.skip_to_semicolon(&mut span);
                    let raw_sql = self
                        .source
                        .get(span.start..span.end)
                        .unwrap_or_default()
                        .to_owned();
                    return Ok(Statement::DropOperator(
                        crate::ast::CompatSimpleCreateStatement {
                        raw_sql,
                        span,
                        },
                    ));
                }
                let notice = if if_exists {
                    // For OPERATOR, PG emits: operator @#@ does not exist, skipping
                    Some(format!("operator {operator_name} does not exist, skipping"))
                } else {
                    None
                };
                self.skip_to_semicolon(&mut span);
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                let _ = notice;
                Ok(Statement::DropOperator(
                    crate::ast::CompatSimpleCreateStatement {
                        raw_sql,
                        span,
                    },
                ))
            }
            // DROP MATERIALIZED VIEW ... → treated as DROP TABLE
            TokenKind::Keyword(Keyword::Materialized) => {
                self.advance(); // consume MATERIALIZED
                if matches!(self.current().kind, TokenKind::Keyword(Keyword::View)) {
                    self.advance(); // consume VIEW
                    let if_exists = if if_exists_consumed {
                        true
                    } else if self.consume_keyword(Keyword::If).is_some() {
                        self.expect_keyword(Keyword::Exists)?;
                        true
                    } else {
                        false
                    };
                    let name = self.parse_object_name()?;
                    let mut span = drop.span.merge(name.span);
                    self.skip_to_semicolon(&mut span);
                    Ok(Statement::DropTable(DropTableStatement {
                        name: name.clone(),
                        extra_names: Vec::new(),
                        if_exists,
                        cascade: false,
                        span,
                    }))
                } else {
                    self.unsupported_feature(self.previous_span(), "DROP MATERIALIZED requires VIEW")
                }
            }
            // DROP FOREIGN TABLE ... / DROP FOREIGN DATA WRAPPER ...
            TokenKind::Keyword(Keyword::Foreign) => {
                let mut span = drop.span.merge(self.current().span);
                self.advance(); // consume FOREIGN
                // Peek to distinguish FOREIGN TABLE vs FOREIGN DATA WRAPPER
                let is_data_wrapper = match &self.current().kind {
                    TokenKind::Keyword(Keyword::Data) => true,
                    TokenKind::Identifier(s) if s.eq_ignore_ascii_case("data") => true,
                    _ => false,
                };
                let (tag, notice) = if is_data_wrapper {
                    self.advance(); // consume DATA
                    if matches!(&self.current().kind, TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("wrapper")) {
                        self.advance();
                    }
                    let if_exists =
                        self.consume_if_exists_lenient_with_preconsumed(if_exists_consumed);
                    let notice = if if_exists {
                        let name = self.peek_noop_object_name();
                        Some(format!(
                            "foreign-data wrapper \"{name}\" does not exist, skipping"
                        ))
                    } else {
                        None
                    };
                    ("DROP FOREIGN DATA WRAPPER".to_owned(), notice)
                } else {
                    if matches!(self.current().kind, TokenKind::Keyword(Keyword::Table)) {
                        self.advance();
                    }
                    let if_exists =
                        self.consume_if_exists_lenient_with_preconsumed(if_exists_consumed);
                    let notice = if if_exists {
                        let name = self.peek_noop_object_name();
                        Some(format!("foreign table \"{name}\" does not exist, skipping"))
                    } else {
                        None
                    };
                    ("DROP FOREIGN TABLE".to_owned(), notice)
                };
                self.skip_to_semicolon(&mut span);
                let _notice = notice;
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                match tag.as_str() {
                    "DROP FOREIGN DATA WRAPPER" => Ok(Statement::DropForeignDataWrapper(
                        crate::ast::CompatSimpleCreateStatement { raw_sql, span },
                    )),
                    "DROP FOREIGN TABLE" => Ok(Statement::DropForeignTable(
                        crate::ast::CompatSimpleCreateStatement { raw_sql, span },
                    )),
                    _ => Ok(self.pg_compat_utility_statement(tag, None, raw_sql, span)),
                }
            }
            // DROP CAST / DROP PROCEDURE / DROP LANGUAGE / DROP TEXT SEARCH ...
            TokenKind::Keyword(
                Keyword::Cast
                    | Keyword::Procedure
                    | Keyword::Language
                    | Keyword::System
                    | Keyword::Text
                    | Keyword::Statistics,
            ) => {
                let kw = match &self.current().kind {
                    TokenKind::Keyword(kw) => kw.name().to_uppercase(),
                    _ => return self.syntax_error_current("unexpected DROP target"),
                };
                let mut span = drop.span.merge(self.current().span);
                self.advance(); // consume keyword
                let if_exists_after_kw = self.consume_keyword(Keyword::If).is_some() && {
                    let _ = self.consume_keyword(Keyword::Exists);
                    true
                };
                let if_exists = if_exists_consumed || if_exists_after_kw;
                let notice = if if_exists {
                    match kw.as_str() {
                        "LANGUAGE" => {
                            let name = self.peek_noop_object_name();
                            Some(format!("language \"{name}\" does not exist, skipping"))
                        }
                        "CAST" => Some("cast does not exist, skipping".to_owned()),
                        "PROCEDURE" => {
                            let name = self.peek_noop_object_name();
                            Some(format!("procedure {name}() does not exist, skipping"))
                        }
                        "TEXT" => {
                            let sub = if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("search"))
                            {
                                self.advance();
                                let st = self.peek_noop_object_name().to_uppercase();
                                self.advance();
                                st
                            } else {
                                String::new()
                            };
                            let text_if_exists =
                                if_exists
                                    || (self.consume_keyword(Keyword::If).is_some() && {
                                        let _ = self.consume_keyword(Keyword::Exists);
                                        true
                                    });
                            if text_if_exists {
                                let name = self.peek_noop_object_name();
                                let kind = match sub.as_str() {
                                    "PARSER" => "text search parser",
                                    "DICTIONARY" => "text search dictionary",
                                    "TEMPLATE" => "text search template",
                                    "CONFIGURATION" => "text search configuration",
                                    _ => "text search object",
                                };
                                Some(format!("{kind} \"{name}\" does not exist, skipping"))
                            } else {
                                None
                            }
                        }
                        _ => None,
                    }
                } else if kw == "TEXT" {
                    let sub = if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("search"))
                    {
                        self.advance();
                        let st = self.peek_noop_object_name().to_uppercase();
                        self.advance();
                        st
                    } else {
                        String::new()
                    };
                    let text_if_exists = self.consume_keyword(Keyword::If).is_some() && {
                        let _ = self.consume_keyword(Keyword::Exists);
                        true
                    };
                    if text_if_exists {
                        let name = self.peek_noop_object_name();
                        let kind = match sub.as_str() {
                            "PARSER" => "text search parser",
                            "DICTIONARY" => "text search dictionary",
                            "TEMPLATE" => "text search template",
                            "CONFIGURATION" => "text search configuration",
                            _ => "text search object",
                        };
                        Some(format!("{kind} \"{name}\" does not exist, skipping"))
                    } else {
                        None
                    }
                } else {
                    None
                };
                self.skip_to_semicolon(&mut span);
                let tag = if kw == "TEXT" {
                    "DROP TEXT SEARCH".to_owned()
                } else {
                    format!("DROP {kw}")
                };
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                if tag == "DROP STATISTICS" {
                    Ok(Statement::DropStatistics(
                        crate::ast::CompatSimpleCreateStatement { raw_sql, span },
                    ))
                } else if tag == "DROP CAST" {
                    let _ = notice;
                    Ok(Statement::DropCast(crate::ast::CompatCastStatement {
                        raw_sql,
                        if_exists,
                        span,
                    }))
                } else {
                    Ok(self.pg_compat_utility_statement(tag, notice, raw_sql, span))
                }
            }
            // Handle DROP <identifier> for: EXTENSION, POLICY,
            // COLLATION, STATISTICS, SERVER, TABLESPACE, PUBLICATION,
            // ACCESS METHOD, TRANSFORM, EVENT TRIGGER, etc.
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("rule") => {
                let mut span = drop.span.merge(self.current().span);
                self.advance();
                let if_exists =
                    self.consume_if_exists_lenient_with_preconsumed(if_exists_consumed);
                if matches!(self.current().kind, TokenKind::Semicolon | TokenKind::Eof) {
                    return self.syntax_error_at_current_token();
                }
                if matches!(self.current().kind, TokenKind::Integer(_)) {
                    return self.syntax_error_at_current_token();
                }
                let notice = if if_exists {
                    let name = self.peek_noop_object_name();
                    Some(format!("rule \"{name}\" does not exist, skipping"))
                } else {
                    None
                };
                self.skip_to_semicolon(&mut span);
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                let name = self.peek_noop_object_name();
                let _ = notice;
                Ok(Statement::DropRule(crate::ast::CompatRuleStatement {
                    name,
                    if_exists,
                    raw_sql,
                    span,
                }))
            }
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("statistics") => {
                let mut span = drop.span.merge(self.current().span);
                self.advance();
                let if_exists =
                    self.consume_if_exists_lenient_with_preconsumed(if_exists_consumed);
                let notice = if if_exists {
                    let name = self.peek_noop_object_name();
                    Some(format!("statistics object \"{name}\" does not exist, skipping"))
                } else {
                    None
                };
                self.skip_to_semicolon(&mut span);
                let _notice = notice;
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                Ok(Statement::DropStatistics(
                    crate::ast::CompatSimpleCreateStatement { raw_sql, span },
                ))
            }
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("policy") => {
                let mut span = drop.span.merge(self.current().span);
                self.advance();
                let if_exists =
                    self.consume_if_exists_lenient_with_preconsumed(if_exists_consumed);
                let notice = if if_exists {
                    let name = self.peek_noop_object_name();
                    Some(format!("policy \"{name}\" does not exist, skipping"))
                } else {
                    None
                };
                self.skip_to_semicolon(&mut span);
                let _notice = notice;
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                Ok(Statement::DropPolicy(
                    crate::ast::CompatSimpleCreateStatement { raw_sql, span },
                ))
            }
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("owned") => {
                self.parse_drop_owned_tail(drop.span)
            }
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("extension") => {
                let mut span = drop.span.merge(self.current().span);
                self.advance();
                let if_exists =
                    self.consume_if_exists_lenient_with_preconsumed(if_exists_consumed);

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

                let cascade = if self.consume_keyword(Keyword::Cascade).is_some() {
                    span = span.merge(self.previous_span());
                    true
                } else {
                    if self.consume_keyword(Keyword::Restrict).is_some() {
                        span = span.merge(self.previous_span());
                    }
                    false
                };

                Ok(Statement::DropExtension(DropExtensionStatement {
                    name,
                    if_exists,
                    cascade,
                    span,
                }))
            }
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("conversion") => {
                self.unsupported_feature(self.current().span, "DROP CONVERSION is not supported")
            }
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("collation") => {
                let mut span = drop.span.merge(self.current().span);
                self.advance();
                let if_exists =
                    self.consume_if_exists_lenient_with_preconsumed(if_exists_consumed);
                let notice = if if_exists {
                    let name = self.peek_noop_object_name();
                    Some(format!("collation \"{name}\" does not exist, skipping"))
                } else {
                    None
                };
                self.skip_to_semicolon(&mut span);
                let _notice = notice;
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                Ok(Statement::DropCollation(
                    crate::ast::CompatSimpleCreateStatement { raw_sql, span },
                ))
            }
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("access") => {
                self.unsupported_feature(self.current().span, "DROP ACCESS METHOD is not supported")
            }
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("publication") => {
                let mut span = drop.span.merge(self.current().span);
                self.advance();
                let if_exists =
                    self.consume_if_exists_lenient_with_preconsumed(if_exists_consumed);
                let notice = if if_exists {
                    let name = self.peek_noop_object_name();
                    Some(format!("publication \"{name}\" does not exist, skipping"))
                } else {
                    None
                };
                self.skip_to_semicolon(&mut span);
                let _notice = notice;
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                Ok(Statement::DropPublication(
                    crate::ast::CompatSimpleCreateStatement { raw_sql, span },
                ))
            }
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("server") => {
                let mut span = drop.span.merge(self.current().span);
                self.advance();
                let if_exists =
                    self.consume_if_exists_lenient_with_preconsumed(if_exists_consumed);
                let notice = if if_exists {
                    let name = self.peek_noop_object_name();
                    Some(format!("server \"{name}\" does not exist, skipping"))
                } else {
                    None
                };
                self.skip_to_semicolon(&mut span);
                let _notice = notice;
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                Ok(Statement::DropServer(
                    crate::ast::CompatSimpleCreateStatement { raw_sql, span },
                ))
            }
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("tablespace") => {
                let mut span = drop.span.merge(self.current().span);
                self.advance();
                let if_exists =
                    self.consume_if_exists_lenient_with_preconsumed(if_exists_consumed);
                let notice = if if_exists {
                    let name = self.peek_noop_object_name();
                    Some(format!("tablespace \"{name}\" does not exist, skipping"))
                } else {
                    None
                };
                self.skip_to_semicolon(&mut span);
                let _notice = notice;
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                Ok(Statement::DropTablespace(
                    crate::ast::CompatSimpleCreateStatement { raw_sql, span },
                ))
            }
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("subscription") => {
                let mut span = drop.span.merge(self.current().span);
                self.advance();
                let if_exists =
                    self.consume_if_exists_lenient_with_preconsumed(if_exists_consumed);
                let notice = if if_exists {
                    let name = self.peek_noop_object_name();
                    Some(format!("subscription \"{name}\" does not exist, skipping"))
                } else {
                    None
                };
                self.skip_to_semicolon(&mut span);
                let _notice = notice;
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                Ok(Statement::DropSubscription(
                    crate::ast::CompatSimpleCreateStatement { raw_sql, span },
                ))
            }
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("transform") => {
                self.unsupported_feature(self.current().span, "DROP TRANSFORM is not supported")
            }
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("event") => {
                self.unsupported_feature(self.current().span, "DROP EVENT TRIGGER is not supported")
            }
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("routine") => {
                let mut span = drop.span.merge(self.current().span);
                self.advance();
                let _if_exists =
                    self.consume_if_exists_lenient_with_preconsumed(if_exists_consumed);
                self.skip_to_semicolon(&mut span);
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                Ok(Statement::DropRoutine(
                    crate::ast::CompatSimpleCreateStatement { raw_sql, span },
                ))
            }
            // many object kinds we don't model).
            TokenKind::Identifier(ref s) => {
                let desc = s.to_uppercase();
                let mut span = drop.span.merge(self.current().span);
                self.advance();
                self.skip_to_semicolon(&mut span);
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                Ok(self.compat_tagged_statement(format!("DROP {desc}"), raw_sql, span))
            }
            TokenKind::Eof => self.syntax_error_current(
                "expected TABLE, INDEX, SEQUENCE, VIEW, SCHEMA, NODE LABEL, EDGE LABEL, ROLE, FUNCTION, TRIGGER, or TENANT after DROP",
            ),
            _ => {
                let desc = match &self.current().kind {
                    TokenKind::Keyword(kw) => kw.name().to_uppercase(),
                    _ => format!("{:?}", self.current().kind),
                };
                let mut span = drop.span.merge(self.current().span);
                self.advance();
                self.skip_to_semicolon(&mut span);
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                Ok(self.compat_tagged_statement(format!("DROP {desc}"), raw_sql, span))
            }
        };
        if result.is_ok() {
            let _ = self.consume_keyword(Keyword::Cascade);
            let _ = self.consume_keyword(Keyword::Restrict);
            while self.consume_kind(&TokenKind::Comma) {
                while !self.is_eof()
                    && !matches!(self.current().kind, TokenKind::Comma | TokenKind::Semicolon)
                {
                    self.advance();
                }
            }
        }
        result
    }
}
