use super::*;

impl Parser {
    pub(super) fn parse_primary_expr(&mut self) -> DbResult<Expr> {
        // Handle PostgreSQL `typename 'literal'` cast syntax (e.g. bool 't', int4 '42')
        if self.is_type_keyword_for_literal_cast() {
            let type_token = self.advance();
            // parse_data_type already consumed the keyword, but we consumed it manually.
            // Re-parse: push index back, call parse_data_type.
            self.index -= 1;
            let (
                data_type,
                pg_type_name_hint,
                interval_precision,
                interval_fields,
                temporal_precision,
                char_length,
            ) = self.parse_data_type_with_pg_type_name_hint()?;
            let TokenKind::String(string_val) = self.current().kind.clone() else {
                return self.syntax_error_current("expected string literal after type name");
            };
            let str_token = self.advance();
            let span = type_token.span.merge(str_token.span);
            let interval_fields =
                if matches!(type_token.kind, TokenKind::Keyword(Keyword::Interval)) {
                    self.parse_interval_field_spec().or(interval_fields)
                } else {
                    interval_fields
                };
            return Ok(self.build_cast_expression(
                Expr::Literal(Literal::String(string_val), str_token.span),
                data_type,
                pg_type_name_hint,
                interval_precision,
                interval_fields,
                temporal_precision,
                char_length,
                span,
            ));
        }

        // Handle identifier-based type literal casts (e.g. json '{}', point '(1,2)')
        if self.is_identifier_type_literal_cast() {
            let type_token = self.advance();
            self.index -= 1;
            let (
                data_type,
                pg_type_name_hint,
                interval_precision,
                interval_fields,
                temporal_precision,
                char_length,
            ) = self.parse_data_type_with_pg_type_name_hint()?;
            let TokenKind::String(string_val) = self.current().kind.clone() else {
                return self.syntax_error_current("expected string literal after type name");
            };
            let str_token = self.advance();
            let span = type_token.span.merge(str_token.span);
            return Ok(self.build_cast_expression(
                Expr::Literal(Literal::String(string_val), str_token.span),
                data_type,
                pg_type_name_hint,
                interval_precision,
                interval_fields,
                temporal_precision,
                char_length,
                span,
            ));
        }

        // Cypher mode: `"..."` is a string literal (not a quoted identifier).
        // We detect this by inspecting the source byte at the token's span
        // start, since both `"foo"` and `foo` lex to TokenKind::Identifier.
        if self.cypher_mode {
            if let TokenKind::Identifier(ref s) = self.current().kind {
                let s = s.clone();
                let span = self.current().span;
                if self.source.as_bytes().get(span.start).copied() == Some(b'"') {
                    self.advance();
                    return Ok(Expr::Literal(Literal::String(s), span));
                }
            }
        }
        // Cypher mode: map literal {key: value, ...}
        if self.cypher_mode && matches!(self.current().kind, TokenKind::LBrace) {
            return self.parse_cypher_map_literal();
        }
        // Cypher mode: [expr, ...] array literal or [x IN list WHERE pred | map] comprehension
        if self.cypher_mode && matches!(self.current().kind, TokenKind::LBracket) {
            // Check for list comprehension: [identifier IN ...]
            let is_comprehension = self.index + 2 < self.tokens.len()
                && matches!(
                    &self.tokens[self.index + 1].kind,
                    TokenKind::Identifier(_) | TokenKind::Keyword(_)
                )
                && matches!(
                    self.tokens[self.index + 2].kind,
                    TokenKind::Keyword(Keyword::In)
                );
            if is_comprehension {
                return self.parse_cypher_list_comprehension();
            }
            if self.looks_like_cypher_pattern_comprehension() {
                return self.parse_cypher_pattern_comprehension();
            }
            let start = self.advance(); // consume '['
            let mut elements = Vec::new();
            if !matches!(self.current().kind, TokenKind::RBracket) {
                elements.push(self.parse_expr()?);
                while self.consume_kind(&TokenKind::Comma) {
                    elements.push(self.parse_expr()?);
                }
            }
            let end = self.expect_token(&TokenKind::RBracket)?;
            return Ok(Expr::Array {
                elements,
                span: start.span.merge(end.span),
            });
        }
        // Cypher quantifiers: any/none/single/all(x IN list WHERE pred)
        if self.cypher_mode {
            if let Some(qexpr) = self.try_parse_cypher_quantifier()? {
                return Ok(qexpr);
            }
        }

        // Fast-path: for simple literals, advance first and match on the
        // owned token to avoid cloning the TokenKind.
        if matches!(
            self.current().kind,
            TokenKind::Integer(_)
                | TokenKind::NumericLiteral(_)
                | TokenKind::Parameter(_)
                | TokenKind::String(_)
                | TokenKind::Keyword(
                    Keyword::True | Keyword::False | Keyword::Null | Keyword::Default
                )
        ) {
            let token = self.advance();
            return match token.kind {
                TokenKind::Integer(value) => Ok(Expr::Literal(Literal::Integer(value), token.span)),
                TokenKind::NumericLiteral(value) => {
                    Ok(Expr::Literal(Literal::NumericLit(value), token.span))
                }
                TokenKind::Parameter(index) => Ok(Expr::Parameter {
                    index,
                    span: token.span,
                }),
                TokenKind::String(value) => Ok(Expr::Literal(Literal::String(value), token.span)),
                TokenKind::Keyword(Keyword::True) => {
                    Ok(Expr::Literal(Literal::Boolean(true), token.span))
                }
                TokenKind::Keyword(Keyword::False) => {
                    Ok(Expr::Literal(Literal::Boolean(false), token.span))
                }
                TokenKind::Keyword(Keyword::Null) => Ok(Expr::Literal(Literal::Null, token.span)),
                TokenKind::Keyword(Keyword::Default) => Ok(Expr::Default { span: token.span }),
                _ => unreachable!(),
            };
        }
        match self.current().kind {
            TokenKind::Keyword(Keyword::Array) => {
                let array_token = self.advance();
                self.parse_array_constructor(array_token)
            }
            // Cypher-style array literal: [expr, ...]
            TokenKind::LBracket => {
                let lbracket = self.advance();
                let elements = self.parse_array_elements()?;
                let rbracket = self.expect_token(&TokenKind::RBracket)?;
                let span = lbracket.span.merge(rbracket.span);
                Ok(Expr::Array { elements, span })
            }
            TokenKind::Keyword(Keyword::Cast) => {
                let cast_token = self.advance();
                self.expect_token(&TokenKind::LParen)?;
                let expr = self.parse_expr()?;
                self.expect_keyword(Keyword::As)?;
                let (
                    data_type,
                    pg_type_name_hint,
                    interval_precision,
                    interval_fields,
                    temporal_precision,
                    char_length,
                ) = self.parse_data_type_with_pg_type_name_hint()?;
                // Skip optional COLLATE clause: CAST(x AS text COLLATE "C")
                if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("collate"))
                {
                    self.advance(); // consume COLLATE
                                    // Consume collation name (can be a quoted string or dotted identifier)
                    if matches!(
                        self.current().kind,
                        TokenKind::Identifier(_) | TokenKind::String(_) | TokenKind::Keyword(_)
                    ) {
                        self.advance();
                        // Handle dotted collation: pg_catalog."C"
                        while self.consume_kind(&TokenKind::Dot) {
                            if matches!(
                                self.current().kind,
                                TokenKind::Identifier(_)
                                    | TokenKind::String(_)
                                    | TokenKind::Keyword(_)
                            ) {
                                self.advance();
                            }
                        }
                    }
                }
                let rparen = self.expect_token(&TokenKind::RParen)?;
                let span = cast_token.span.merge(rparen.span);
                Ok(self.build_cast_expression(
                    expr,
                    data_type,
                    pg_type_name_hint,
                    interval_precision,
                    interval_fields,
                    temporal_precision,
                    char_length,
                    span,
                ))
            }
            TokenKind::Keyword(Keyword::Case) => {
                let case_token = self.advance();

                // Check for simple CASE (CASE expr WHEN ...)
                let operand = if matches!(self.current().kind, TokenKind::Keyword(Keyword::When)) {
                    None
                } else {
                    Some(Box::new(self.parse_expr()?))
                };

                let mut conditions = Vec::new();
                let mut results = Vec::new();

                // Parse WHEN ... THEN ... pairs
                while self.consume_keyword(Keyword::When).is_some() {
                    conditions.push(self.parse_expr()?);
                    self.expect_keyword(Keyword::Then)?;
                    results.push(self.parse_expr()?);
                }

                if conditions.is_empty() {
                    return self.syntax_error_current("CASE requires at least one WHEN clause");
                }

                // Parse optional ELSE
                let else_result = if self.consume_keyword(Keyword::Else).is_some() {
                    Some(Box::new(self.parse_expr()?))
                } else {
                    None
                };

                let end_token = self.expect_keyword(Keyword::End)?;
                let span = case_token.span.merge(end_token.span);

                Ok(Expr::CaseWhen {
                    operand,
                    conditions,
                    results,
                    else_result,
                    span,
                })
            }
            TokenKind::Keyword(Keyword::Exists) => {
                let exists_token = self.advance();
                if self.cypher_mode && self.consume_kind(&TokenKind::LBrace) {
                    let query = self.parse_cypher_statement()?;
                    let rbrace = self.expect_token(&TokenKind::RBrace)?;
                    let span = exists_token.span.merge(rbrace.span);
                    return Ok(Expr::CypherExists {
                        query: Box::new(query),
                        negated: false,
                        span,
                    });
                }
                self.expect_token(&TokenKind::LParen)?;
                let query = self.parse_subquery_select()?;
                let rparen = self.expect_token(&TokenKind::RParen)?;
                let span = exists_token.span.merge(rparen.span);
                Ok(Expr::Exists {
                    query: Box::new(query),
                    negated: false,
                    span,
                })
            }
            TokenKind::LParen => {
                self.enter_paren_recursion()?;
                let result = self.parse_parenthesized_expr();
                self.leave_paren_recursion();
                result
            }
            TokenKind::Keyword(Keyword::Coalesce | Keyword::Nullif) => {
                let token = self.advance();
                let fn_name = match token.kind {
                    TokenKind::Keyword(Keyword::Coalesce) => "coalesce",
                    TokenKind::Keyword(Keyword::Nullif) => "nullif",
                    _ => {
                        return self
                            .syntax_error(token.span, "expected COALESCE or NULLIF function");
                    }
                };
                self.expect_token(&TokenKind::LParen)?;
                let mut args = Vec::new();
                loop {
                    args.push(self.parse_expr()?);
                    if self.consume_kind(&TokenKind::Comma) {
                        continue;
                    }
                    self.expect_token(&TokenKind::RParen)?;
                    break;
                }
                let name = ObjectName {
                    parts: vec![fn_name.to_owned()],
                    span: token.span,
                };
                Ok(Expr::FunctionCall {
                    span: token.span.merge(self.previous_span()),
                    name,
                    args,
                    distinct: false,
                    filter: None,
                })
            }
            // ALL(...) / SOME(...) - treat as function-like expressions
            // e.g.  33 = ALL('{1,2,33}'), x > ANY(ARRAY[1,2,3])
            TokenKind::Keyword(Keyword::All)
                if self.index + 1 < self.tokens.len()
                    && self.tokens[self.index + 1].kind == TokenKind::LParen =>
            {
                let token = self.advance();
                self.parse_reserved_keyword_as_func(token, "all")
            }
            TokenKind::Identifier(_) | TokenKind::Keyword(_)
                if matches!(self.current().kind, TokenKind::Identifier(_))
                    || matches!(self.current().kind, TokenKind::Keyword(kw) if kw.is_unreserved()) =>
            {
                // Handle `CURRENT OF cursor_name` (used in WHERE CURRENT OF).
                // Rewrite as TRUE since cursor-based positioning is not modeled.
                if matches!(self.current().kind, TokenKind::Keyword(Keyword::Current))
                    && self.index + 1 < self.tokens.len()
                    && matches!(
                        self.tokens[self.index + 1].kind,
                        TokenKind::Keyword(Keyword::Of)
                    )
                {
                    let start = self.advance(); // consume CURRENT
                    self.advance(); // consume OF
                    let mut span = start.span;
                    // Consume cursor name
                    if let Ok((_, id_span)) = self.expect_identifier() {
                        span = span.merge(id_span);
                    }
                    return Ok(Expr::Literal(Literal::Boolean(true), span));
                }

                // Handle SIMILAR TO: `expr SIMILAR TO pattern [ESCAPE 'c']`
                // SIMILAR is an identifier, not a keyword.
                if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("similar"))
                    && self.index + 1 < self.tokens.len()
                    && matches!(
                        self.tokens[self.index + 1].kind,
                        TokenKind::Keyword(Keyword::To)
                    )
                {
                    // This is handled at a higher level; fall through here.
                }

                let name = self.parse_object_name()?;
                if !self.consume_kind(&TokenKind::LParen) {
                    return Ok(Expr::Identifier(name));
                }

                self.parse_identifier_or_function_expr(name)
            }
            // Handle reserved keywords that can appear as function names
            // or identifiers in expression position.
            // e.g., SET('t'), RELEASE(x), LEFT('abc', 3), RIGHT('abc', 3),
            // TABLE(x), VALUES(x), CHECK(x), etc.
            // When followed by '(' treat as function call; otherwise treat as identifier.
            TokenKind::Keyword(
                Keyword::Set
                | Keyword::Release
                | Keyword::Show
                | Keyword::Reset
                | Keyword::Read
                | Keyword::Left
                | Keyword::Right
                | Keyword::Full
                | Keyword::Inner
                | Keyword::Outer
                | Keyword::Cross
                | Keyword::Join
                | Keyword::Table
                | Keyword::Values
                | Keyword::Check
                | Keyword::Primary
                | Keyword::Foreign
                | Keyword::References
                | Keyword::Unique
                | Keyword::View
                | Keyword::Constraint
                | Keyword::In
                | Keyword::Like
                | Keyword::Between
                | Keyword::Returning
                | Keyword::Ilike
                | Keyword::Select
                | Keyword::Insert
                | Keyword::Update
                | Keyword::Delete
                | Keyword::Drop
                | Keyword::Create
                | Keyword::Alter
                | Keyword::Add
                | Keyword::Column
                | Keyword::Explain
                | Keyword::Grant
                | Keyword::Revoke
                | Keyword::Truncate
                | Keyword::Into
                | Keyword::Intersect
                | Keyword::Except
                | Keyword::Union
                | Keyword::From
                | Keyword::Where
                | Keyword::Group
                | Keyword::Having
                | Keyword::Order
                | Keyword::Limit
                | Keyword::Offset
                | Keyword::On
                | Keyword::Or
                | Keyword::And
                | Keyword::Not
                | Keyword::Is
                | Keyword::Asc
                | Keyword::Desc
                | Keyword::By
                | Keyword::For
                | Keyword::To
                | Keyword::With
                | Keyword::Vacuum
                | Keyword::Copy
                | Keyword::Commit
                | Keyword::Rollback
                | Keyword::Begin
                | Keyword::End
                | Keyword::Then
                | Keyword::Else
                | Keyword::When
                | Keyword::Analyze,
            ) if self.index + 1 < self.tokens.len()
                && self.tokens[self.index + 1].kind == TokenKind::LParen =>
            {
                let token = self.advance();
                let fn_name = match token.kind {
                    TokenKind::Keyword(kw) => kw.name().to_lowercase(),
                    _ => {
                        return self.syntax_error(
                            token.span,
                            "expected reserved keyword before function call",
                        );
                    }
                };
                self.parse_reserved_keyword_as_func(token, &fn_name)
            }
            // Star (*) as a bare expression - used in select items after comma
            TokenKind::Star => {
                let star_token = self.advance();
                Ok(Expr::Identifier(ObjectName {
                    parts: vec!["*".to_owned()],
                    span: star_token.span,
                }))
            }
            // Psql variable interpolation: :'varname' or :{?varname}
            // Reject explicitly instead of fabricating placeholder values.
            TokenKind::Colon
                if self.index + 1 < self.tokens.len()
                    && matches!(self.tokens[self.index + 1].kind, TokenKind::String(_)) =>
            {
                let colon = self.advance(); // consume ':'
                self.advance(); // consume string
                self.unsupported_feature(colon.span, "psql variable interpolation is not supported")
            }
            // Colon followed by LBrace - psql :{?varname} syntax.
            // Reject explicitly instead of fabricating NULL.
            TokenKind::Colon
                if self.index + 1 < self.tokens.len()
                    && matches!(self.tokens[self.index + 1].kind, TokenKind::LBrace) =>
            {
                let start = self.advance(); // consume ':'
                self.advance(); // consume '{'
                                // Skip until '}' or semicolon
                while !self.is_eof()
                    && !matches!(
                        self.current().kind,
                        TokenKind::RBrace | TokenKind::Semicolon
                    )
                {
                    self.advance();
                }
                if self.current().kind == TokenKind::RBrace {
                    self.advance();
                }
                self.unsupported_feature(start.span, "psql variable interpolation is not supported")
            }
            // Reserved keywords that can appear bare (without '(') as column
            // names or identifiers in expression position.  The big list above
            // already handles the cases where these are followed by '('; this
            // handles the bare-identifier case.
            // Intentionally excludes statement-starting keywords (SELECT, INSERT,
            // UPDATE, DELETE, CREATE, ALTER, DROP) and clause-starting keywords
            // (FROM, WHERE, GROUP, HAVING, ORDER, LIMIT, OFFSET, WITH) so that
            // parse_expression() does not accidentally swallow statement bodies.
            TokenKind::Keyword(
                Keyword::Explain
                | Keyword::All
                | Keyword::Distinct
                | Keyword::Analyze
                | Keyword::Vacuum
                | Keyword::Copy
                | Keyword::Grant
                | Keyword::Revoke
                | Keyword::Truncate
                | Keyword::Set
                | Keyword::Release
                | Keyword::Show
                | Keyword::Reset
                | Keyword::Read
                | Keyword::Commit
                | Keyword::Rollback
                | Keyword::Begin
                | Keyword::End
                | Keyword::Then
                | Keyword::Else
                | Keyword::When
                | Keyword::Returning
                | Keyword::Left
                | Keyword::Right
                | Keyword::Full
                | Keyword::Inner
                | Keyword::Outer
                | Keyword::Cross
                | Keyword::Join
                | Keyword::On
                | Keyword::Table
                | Keyword::View
                | Keyword::Check
                | Keyword::Primary
                | Keyword::Foreign
                | Keyword::References
                | Keyword::Unique
                | Keyword::Constraint
                | Keyword::Column
                | Keyword::Add
                | Keyword::Asc
                | Keyword::Desc
                | Keyword::By
                | Keyword::For
                | Keyword::To
                | Keyword::In
                | Keyword::Is
                | Keyword::Like
                | Keyword::Between
                | Keyword::Ilike
                | Keyword::Values
                | Keyword::Into
                | Keyword::Intersect
                | Keyword::Except
                | Keyword::Union
                | Keyword::Not
                | Keyword::And
                | Keyword::Or,
            ) => {
                let token = self.advance();
                let name = match &token.kind {
                    TokenKind::Keyword(k) => k.name().to_lowercase(),
                    _ => {
                        return self.syntax_error(
                            token.span,
                            "expected reserved keyword in expression position",
                        );
                    }
                };
                Ok(Expr::Identifier(ObjectName {
                    parts: vec![name],
                    span: token.span,
                }))
            }
            // Emit a PG-compatible "syntax error at or near ..." diagnostic so
            // the error message (and caret position) match what PostgreSQL
            // reports when the primary-expression parser stalls on an
            // unexpected token (e.g., `SELECT 10 !=-;` → near ";").
            _ => self.syntax_error_at_current_token(),
        }
    }

    fn parse_cypher_map_literal(&mut self) -> DbResult<Expr> {
        let lbrace = self.expect_token(&TokenKind::LBrace)?;
        let mut args = Vec::new();
        if !matches!(self.current().kind, TokenKind::RBrace) {
            loop {
                let (_, key_span) = self.expect_identifier()?;
                // Cypher map keys are case-sensitive (`{notName: ...}`),
                // but the lexer folds unquoted identifiers to lowercase
                // for PG compat. Recover the original casing from the
                // source span so `map.notName` lookups match.
                let raw = self
                    .source
                    .get(key_span.start..key_span.end)
                    .map(str::to_owned)
                    .unwrap_or_default();
                let key = if raw.is_empty() {
                    self.previous_span();
                    String::new()
                } else if raw.starts_with('"') && raw.ends_with('"') && raw.len() >= 2 {
                    raw[1..raw.len() - 1].replace("\"\"", "\"")
                } else if raw.starts_with('`') && raw.ends_with('`') && raw.len() >= 2 {
                    raw[1..raw.len() - 1].to_owned()
                } else {
                    raw
                };
                args.push(Expr::Literal(Literal::String(key), key_span));
                self.expect_token(&TokenKind::Colon)?;
                let value = self.parse_expr()?;
                args.push(value);
                if !self.consume_kind(&TokenKind::Comma) {
                    break;
                }
            }
        }
        let rbrace = self.expect_token(&TokenKind::RBrace)?;
        let span = lbrace.span.merge(rbrace.span);
        Ok(Expr::FunctionCall {
            name: ObjectName {
                parts: vec!["jsonb_build_object".to_owned()],
                span: lbrace.span,
            },
            args,
            distinct: false,
            filter: None,
            span,
        })
    }

    fn parse_cypher_list_comprehension(&mut self) -> DbResult<Expr> {
        let start = self.advance(); // consume '['
        let (var, _) = self.expect_identifier()?;
        self.expect_keyword(Keyword::In)?;
        let list = self.parse_expr()?;
        let pred = if self.consume_keyword(Keyword::Where).is_some() {
            self.parse_expr()?
        } else {
            Expr::Literal(Literal::Boolean(true), start.span)
        };
        let map_expr = if self.consume_kind(&TokenKind::Pipe) {
            self.parse_expr()?
        } else {
            Expr::Identifier(ObjectName {
                parts: vec![var.clone()],
                span: start.span,
            })
        };
        let end = self.expect_token(&TokenKind::RBracket)?;
        let span = start.span.merge(end.span);
        Ok(Expr::FunctionCall {
            name: ObjectName {
                parts: vec!["__cypher_list_comprehension".to_owned()],
                span,
            },
            args: vec![
                Expr::Literal(Literal::String(var), start.span),
                list,
                pred,
                map_expr,
            ],
            distinct: false,
            filter: None,
            span,
        })
    }

    fn looks_like_cypher_pattern_comprehension(&self) -> bool {
        if !matches!(self.current().kind, TokenKind::LBracket) {
            return false;
        }
        let starts_like_pattern = match self.tokens.get(self.index + 1).map(|token| &token.kind) {
            Some(TokenKind::LParen) => true,
            Some(TokenKind::Identifier(name))
                if matches!(
                    self.tokens.get(self.index + 2).map(|token| &token.kind),
                    Some(TokenKind::LParen)
                ) && matches!(
                    name.to_ascii_lowercase().as_str(),
                    "shortestpath" | "allshortestpaths"
                ) =>
            {
                true
            }
            Some(TokenKind::Identifier(_) | TokenKind::Keyword(_))
                if matches!(
                    self.tokens.get(self.index + 2).map(|token| &token.kind),
                    Some(TokenKind::Eq)
                ) =>
            {
                true
            }
            _ => false,
        };
        if !starts_like_pattern {
            return false;
        }

        let mut bracket_depth = 0usize;
        let mut paren_depth = 0usize;
        let mut brace_depth = 0usize;
        for token in self.tokens.iter().skip(self.index) {
            match token.kind {
                TokenKind::LBracket => bracket_depth = bracket_depth.saturating_add(1),
                TokenKind::RBracket => {
                    if bracket_depth == 1 && paren_depth == 0 && brace_depth == 0 {
                        return false;
                    }
                    bracket_depth = bracket_depth.saturating_sub(1);
                    if bracket_depth == 0 {
                        return false;
                    }
                }
                TokenKind::LParen => paren_depth = paren_depth.saturating_add(1),
                TokenKind::RParen => paren_depth = paren_depth.saturating_sub(1),
                TokenKind::LBrace => brace_depth = brace_depth.saturating_add(1),
                TokenKind::RBrace => brace_depth = brace_depth.saturating_sub(1),
                TokenKind::Pipe if bracket_depth == 1 && paren_depth == 0 && brace_depth == 0 => {
                    return true;
                }
                _ => {}
            }
        }
        false
    }

    fn parse_cypher_pattern_comprehension(&mut self) -> DbResult<Expr> {
        let start = self.advance(); // consume '['
        let pattern = self.parse_cypher_path_pattern()?;
        let where_clause = if self.consume_keyword(Keyword::Where).is_some() {
            Some(Box::new(self.parse_expr()?))
        } else {
            None
        };
        self.expect_token(&TokenKind::Pipe)?;
        let map_expr = self.parse_expr()?;
        let end = self.expect_token(&TokenKind::RBracket)?;
        Ok(Expr::CypherPatternComprehension {
            pattern,
            where_clause,
            map_expr: Box::new(map_expr),
            span: start.span.merge(end.span),
        })
    }

    fn try_parse_cypher_quantifier(&mut self) -> DbResult<Option<Expr>> {
        let qname = match &self.current().kind {
            TokenKind::Identifier(s) => {
                let lower = s.to_ascii_lowercase();
                match lower.as_str() {
                    "any" | "single" => Some(lower),
                    _ => None,
                }
            }
            TokenKind::Keyword(Keyword::All) => Some("all".to_string()),
            TokenKind::Keyword(Keyword::None) => Some("none".to_string()),
            TokenKind::Keyword(Keyword::Any) => Some("any".to_string()),
            _ => None,
        };
        let Some(qname) = qname else {
            return Ok(None);
        };
        if self.index + 3 >= self.tokens.len() {
            return Ok(None);
        }
        if !matches!(self.tokens[self.index + 1].kind, TokenKind::LParen) {
            return Ok(None);
        }
        if !matches!(
            &self.tokens[self.index + 2].kind,
            TokenKind::Identifier(_) | TokenKind::Keyword(_)
        ) {
            return Ok(None);
        }
        if !matches!(
            self.tokens[self.index + 3].kind,
            TokenKind::Keyword(Keyword::In)
        ) {
            return Ok(None);
        }

        let fn_name = format!("__cypher_{qname}");
        let start_span = self.current().span;
        self.advance(); // function name
        self.advance(); // (
        let (var_name, _) = self.expect_identifier()?;
        self.expect_keyword(Keyword::In)?;
        let list_expr = self.parse_expr()?;
        let pred_expr = if self.consume_keyword(Keyword::Where).is_some() {
            self.parse_expr()?
        } else {
            Expr::Literal(Literal::Boolean(true), start_span)
        };
        let end = self.expect_token(&TokenKind::RParen)?;
        let span = start_span.merge(end.span);
        Ok(Some(Expr::FunctionCall {
            name: ObjectName {
                parts: vec![fn_name],
                span,
            },
            args: vec![
                Expr::Literal(Literal::String(var_name), start_span),
                list_expr,
                pred_expr,
            ],
            distinct: false,
            filter: None,
            span,
        }))
    }

    /// Parse the body of a parenthesized expression after `(` has been
    /// consumed by the caller.  Factored out of `parse_primary_expr` so
    /// the caller can wrap it with `enter_paren_recursion` /
    /// `leave_paren_recursion` in a single place.
    fn parse_parenthesized_expr(&mut self) -> DbResult<Expr> {
        let lparen = self.advance();
        // Detect scalar subquery: (SELECT ...) or (VALUES ...)
        // Also handles (SELECT ... UNION SELECT ...) set operations.
        if matches!(self.current().kind, TokenKind::Keyword(Keyword::Select)) {
            let query = self.parse_subquery_select()?;
            let rparen = self.expect_token(&TokenKind::RParen)?;
            let span = lparen.span.merge(rparen.span);
            return Ok(Expr::Subquery {
                query: Box::new(query),
                span,
            });
        }
        if matches!(self.current().kind, TokenKind::Keyword(Keyword::Values)) {
            let values_stmt = self.parse_values_as_statement()?;
            let rparen = self.expect_token(&TokenKind::RParen)?;
            let span = lparen.span.merge(rparen.span);
            let Some(query) = Self::extract_leftmost_select(values_stmt) else {
                return self.unsupported_feature(span, "unsupported scalar subquery");
            };
            return Ok(Expr::Subquery {
                query: Box::new(query),
                span,
            });
        }
        // CTE subquery: (WITH cte AS (...) SELECT ... [UNION ...])
        if matches!(self.current().kind, TokenKind::Keyword(Keyword::With)) {
            let with_stmt = self.parse_with_statement()?;
            // Handle set operations (UNION/INTERSECT/EXCEPT) after WITH ... SELECT
            let final_stmt = self.maybe_parse_set_operation(with_stmt)?;
            // If there's still a set operation that wasn't consumed (nested parens),
            // skip to the closing paren
            if matches!(
                self.current().kind,
                TokenKind::Keyword(Keyword::Union | Keyword::Intersect | Keyword::Except)
            ) {
                let mut depth = 1u32;
                while !self.is_eof() && depth > 0 {
                    match self.current().kind {
                        TokenKind::LParen => {
                            depth += 1;
                            self.advance();
                        }
                        TokenKind::RParen => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                            self.advance();
                        }
                        _ => {
                            self.advance();
                        }
                    }
                }
            }
            let rparen = self.expect_token(&TokenKind::RParen)?;
            let span = lparen.span.merge(rparen.span);
            let query = match final_stmt {
                Statement::Select(s) => s,
                Statement::SetOperation(set_op) => match *set_op.left {
                    Statement::Select(s) => s,
                    _ => {
                        return self
                            .syntax_error(span, "set-operation left operand must be a SELECT");
                    }
                },
                _ => {
                    return self.syntax_error(
                        span,
                        "subquery in expression must be a SELECT or set operation",
                    );
                }
            };
            return Ok(Expr::Subquery {
                query: Box::new(query),
                span,
            });
        }
        let expr = self.parse_expr()?;
        // If UNION/INTERSECT/EXCEPT follows, the inner expr is a
        // subquery participating in a set operation.  Wrap it as a
        // subquery and combine with the set-operation right-hand side,
        // then close with ')'.
        if matches!(
            self.current().kind,
            TokenKind::Keyword(Keyword::Union | Keyword::Intersect | Keyword::Except)
        ) {
            // Reconstruct the left subquery from the expression.
            // The expression should be a Subquery already; if not,
            // skip to closing paren.
            let mut depth = 1u32;
            while !self.is_eof() && depth > 0 {
                match self.current().kind {
                    TokenKind::LParen => {
                        depth += 1;
                        self.advance();
                    }
                    TokenKind::RParen => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                        self.advance();
                    }
                    _ => {
                        self.advance();
                    }
                }
            }
            let rparen = self.expect_token(&TokenKind::RParen)?;
            let span = lparen.span.merge(rparen.span);
            return match expr {
                sub @ Expr::Subquery { .. } => Ok(sub),
                _other => {
                    self.syntax_error(span, "set operation requires subqueries on both sides")
                }
            };
        }
        // Row constructor: (a, b, c) → FunctionCall { name: "row" }
        if self.current().kind == TokenKind::Comma {
            let mut elements = vec![expr];
            while self.consume_kind(&TokenKind::Comma) {
                // Handle trailing comma: (100, ) - stop before ')'
                if self.current().kind == TokenKind::RParen {
                    break;
                }
                elements.push(self.parse_expr()?);
            }
            let rparen = self.expect_token(&TokenKind::RParen)?;
            let span = lparen.span.merge(rparen.span);
            return Ok(Expr::FunctionCall {
                name: ObjectName {
                    parts: vec!["row".to_owned()],
                    span: lparen.span,
                },
                args: elements,
                distinct: false,
                filter: None,
                span,
            });
        }
        self.expect_token(&TokenKind::RParen)?;
        Ok(expr)
    }
}
