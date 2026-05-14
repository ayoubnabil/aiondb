#![allow(clippy::doc_markdown)]

mod functions;
mod operators;
mod postfix;
mod primary;
mod window;

use aiondb_core::{DataType, DbError, DbResult};

use crate::{
    ast::{
        BinaryOperator, Expr, Literal, ObjectName, OrderByItem, SelectStatement, Statement,
        UnaryOperator,
    },
    keywords::Keyword,
    parser_types::IntervalFieldSpec,
    tokens::TokenKind,
    Parser,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum JsonUniqueClause {
    #[default]
    Unspecified,
    With,
    Without,
}

impl Parser {
    fn build_cast_expression(
        &self,
        expr: Expr,
        data_type: DataType,
        pg_type_name_hint: Option<String>,
        interval_precision: Option<u32>,
        interval_fields: Option<IntervalFieldSpec>,
        temporal_precision: Option<u32>,
        char_length: Option<u32>,
        span: crate::span::Span,
    ) -> Expr {
        let cast_source = self.maybe_wrap_interval_fields(expr, interval_fields, span);
        let cast_expr = if matches!(pg_type_name_hint.as_deref(), Some("char")) {
            Expr::FunctionCall {
                span,
                name: ObjectName {
                    parts: vec!["__aiondb_pg_char_cast".to_owned()],
                    span,
                },
                args: vec![cast_source],
                distinct: false,
                filter: None,
            }
        } else if matches!(pg_type_name_hint.as_deref(), Some("int2" | "smallint")) {
            // Route CAST(x AS int2/smallint) through the int2() function so
            // that overflow produces "smallint out of range" instead of the
            // generic "integer out of range" from the BigInt->Int cast path.
            Expr::FunctionCall {
                span,
                name: ObjectName {
                    parts: vec!["int2".to_owned()],
                    span,
                },
                args: vec![cast_source],
                distinct: false,
                filter: None,
            }
        } else if matches!(pg_type_name_hint.as_deref(), Some("oid")) {
            // Route CAST(x AS oid) through the oid() function so that
            // overflow produces "OID out of range".
            Expr::FunctionCall {
                span,
                name: ObjectName {
                    parts: vec!["oid".to_owned()],
                    span,
                },
                args: vec![cast_source],
                distinct: false,
                filter: None,
            }
        } else {
            Expr::Cast {
                expr: Box::new(cast_source),
                data_type,
                span,
            }
        };

        let cast_expr = self.maybe_wrap_type_hint(cast_expr, pg_type_name_hint, span);
        let cast_expr = self.maybe_wrap_interval_precision(cast_expr, interval_precision, span);
        let cast_expr = self.maybe_wrap_temporal_precision(cast_expr, temporal_precision, span);
        self.maybe_wrap_char_pad_length(cast_expr, char_length, span)
    }

    fn maybe_wrap_type_hint(
        &self,
        expr: Expr,
        pg_type_name_hint: Option<String>,
        span: crate::span::Span,
    ) -> Expr {
        let Some(pg_type_name_hint) = pg_type_name_hint else {
            return expr;
        };
        Expr::FunctionCall {
            span,
            name: ObjectName {
                parts: vec!["__aiondb_type_hint".to_owned()],
                span,
            },
            args: vec![
                expr,
                Expr::Literal(Literal::String(pg_type_name_hint), span),
            ],
            distinct: false,
            filter: None,
        }
    }

    fn maybe_wrap_interval_precision(
        &self,
        expr: Expr,
        interval_precision: Option<u32>,
        span: crate::span::Span,
    ) -> Expr {
        let Some(interval_precision) = interval_precision else {
            return expr;
        };
        Expr::FunctionCall {
            span,
            name: ObjectName {
                parts: vec!["__aiondb_interval_precision".to_owned()],
                span,
            },
            args: vec![
                expr,
                Expr::Literal(Literal::Integer(i64::from(interval_precision)), span),
            ],
            distinct: false,
            filter: None,
        }
    }

    fn maybe_wrap_temporal_precision(
        &self,
        expr: Expr,
        temporal_precision: Option<u32>,
        span: crate::span::Span,
    ) -> Expr {
        let Some(temporal_precision) = temporal_precision else {
            return expr;
        };
        Expr::FunctionCall {
            span,
            name: ObjectName {
                parts: vec!["__aiondb_temporal_precision".to_owned()],
                span,
            },
            args: vec![
                expr,
                Expr::Literal(Literal::Integer(i64::from(temporal_precision)), span),
            ],
            distinct: false,
            filter: None,
        }
    }

    fn maybe_wrap_char_pad_length(
        &self,
        expr: Expr,
        char_length: Option<u32>,
        span: crate::span::Span,
    ) -> Expr {
        let Some(char_length) = char_length else {
            return expr;
        };
        Expr::FunctionCall {
            span,
            name: ObjectName {
                parts: vec!["__aiondb_char_pad_length".to_owned()],
                span,
            },
            args: vec![
                expr,
                Expr::Literal(Literal::Integer(i64::from(char_length)), span),
            ],
            distinct: false,
            filter: None,
        }
    }

    fn maybe_wrap_interval_fields(
        &self,
        expr: Expr,
        interval_fields: Option<IntervalFieldSpec>,
        span: crate::span::Span,
    ) -> Expr {
        let Some(interval_fields) = interval_fields else {
            return expr;
        };
        let end_field = if interval_fields.start == interval_fields.end {
            Expr::Literal(Literal::Null, span)
        } else {
            Expr::Literal(
                Literal::String(interval_fields.end.as_str().to_owned()),
                span,
            )
        };
        let second_precision = interval_fields.second_precision.map_or_else(
            || Expr::Literal(Literal::Null, span),
            |precision| Expr::Literal(Literal::Integer(i64::from(precision)), span),
        );
        Expr::FunctionCall {
            span,
            name: ObjectName {
                parts: vec!["__aiondb_interval_fields".to_owned()],
                span,
            },
            args: vec![
                expr,
                Expr::Literal(
                    Literal::String(interval_fields.start.as_str().to_owned()),
                    span,
                ),
                end_field,
                second_precision,
            ],
            distinct: false,
            filter: None,
        }
    }

    /// Parse an ARRAY constructor expression.
    ///
    /// Supports three forms:
    /// - `ARRAY[expr, ...]`          - flat array
    /// - `ARRAY[[expr, ...], ...]`   - nested (multi-dimensional) array
    /// - `ARRAY(SELECT ...)`         - array from subquery
    fn parse_array_constructor(&mut self, array_token: crate::tokens::Token) -> DbResult<Expr> {
        // ARRAY(SELECT ...) - subquery form
        if self.current().kind == TokenKind::LParen {
            self.advance(); // consume '('
            let query = self.parse_subquery_select()?;
            let rparen = self.expect_token(&TokenKind::RParen)?;
            let span = array_token.span.merge(rparen.span);
            return Ok(Expr::ArraySubquery {
                query: Box::new(query),
                span,
            });
        }

        self.expect_token(&TokenKind::LBracket)?;
        let elements = self.parse_array_elements()?;
        let rbracket = self.expect_token(&TokenKind::RBracket)?;
        let span = array_token.span.merge(rbracket.span);
        Ok(Expr::Array { elements, span })
    }

    /// Parse a comma-separated list of array elements (expressions or
    /// sub-arrays delimited by `[...]`).  Stops before the closing `]`.
    fn parse_array_elements(&mut self) -> DbResult<Vec<Expr>> {
        let mut elements = Vec::new();
        if self.current().kind == TokenKind::RBracket {
            return Ok(elements);
        }
        loop {
            if elements.len() >= crate::MAX_ARRAY_ELEMENTS {
                return Err(DbError::program_limit(format!(
                    "number of array elements exceeds the maximum allowed ({})",
                    crate::MAX_ARRAY_ELEMENTS
                )));
            }
            if self.current().kind == TokenKind::LBracket {
                // Sub-array: [expr, ...] - guard against unbounded nesting.
                self.enter_expr_recursion()?;
                let lbracket = self.advance();
                let sub = self.parse_array_elements()?;
                let rbracket = self.expect_token(&TokenKind::RBracket)?;
                self.leave_expr_recursion();
                let span = lbracket.span.merge(rbracket.span);
                elements.push(Expr::Array {
                    elements: sub,
                    span,
                });
            } else {
                elements.push(self.parse_expr()?);
            }
            if !self.consume_kind(&TokenKind::Comma) {
                break;
            }
        }
        Ok(elements)
    }

    /// Parse a reserved keyword used as a function call: `keyword(args...)`.
    /// The keyword token has already been consumed; we expect `(` next.
    fn parse_reserved_keyword_as_func(
        &mut self,
        token: crate::tokens::Token,
        fn_name: &str,
    ) -> DbResult<Expr> {
        self.expect_token(&TokenKind::LParen)?;
        let mut args = Vec::new();
        // Handle subquery: ALL(SELECT ...) / ANY(SELECT ...)
        if matches!(self.current().kind, TokenKind::Keyword(Keyword::Select)) {
            let query = self.parse_subquery_select()?;
            args.push(Expr::Subquery {
                query: Box::new(query),
                span: token.span.merge(self.previous_span()),
            });
            self.expect_token(&TokenKind::RParen)?;
        } else if !self.consume_kind(&TokenKind::RParen) {
            loop {
                args.push(self.parse_expr()?);
                if self.consume_kind(&TokenKind::Comma) {
                    continue;
                }
                self.expect_token(&TokenKind::RParen)?;
                break;
            }
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
}

impl Parser {
    /// Parse a sub-SELECT statement and return the `SelectStatement` directly.
    /// Used for subquery expressions: (SELECT ...), IN (SELECT ...), EXISTS (SELECT ...).
    /// Also handles set operations: (SELECT ... UNION SELECT ...).
    fn parse_subquery_select(&mut self) -> DbResult<SelectStatement> {
        self.enter_subquery_recursion()?;
        let result = self.parse_subquery_select_inner();
        self.leave_subquery_recursion();
        result
    }

    fn parse_subquery_select_inner(&mut self) -> DbResult<SelectStatement> {
        // Handle WITH ... SELECT ... (CTE subquery)
        if matches!(self.current().kind, TokenKind::Keyword(Keyword::With)) {
            let with_stmt = self.parse_with_statement()?;
            return match with_stmt {
                Statement::Select(s) => Ok(s),
                _ => self.syntax_error_current("expected SELECT in subquery"),
            };
        }
        let stmt = self.parse_select_statement()?;
        match stmt {
            Statement::Select(select) => {
                if matches!(
                    self.current().kind,
                    TokenKind::Keyword(Keyword::Union | Keyword::Intersect | Keyword::Except)
                ) {
                    return self.unsupported_feature(
                        self.current().span,
                        "set operations (UNION/INTERSECT/EXCEPT) in subqueries are not yet supported",
                    );
                }
                Ok(select)
            }
            Statement::SetOperation(_) => self.unsupported_feature(
                self.current().span,
                "set operations (UNION/INTERSECT/EXCEPT) in subqueries are not yet supported",
            ),
            _ => self.syntax_error_current("expected SELECT in subquery"),
        }
    }

    pub(crate) fn parse_expr(&mut self) -> DbResult<Expr> {
        self.enter_expr_recursion()?;
        let result = self.parse_or_expr();
        self.leave_expr_recursion();
        result
    }

    /// Accept and discard a trailing `ESCAPE '<char>'` clause.
    ///
    /// Full ESCAPE semantics (overriding the default backslash) aren't yet
    /// plumbed through the LIKE evaluator, but rejecting the clause was
    /// blocking ORM tools (Knex, SQLAlchemy) that emit `LIKE x ESCAPE '\\'`
    /// purely to opt out of escape handling in the first place. We swallow
    /// the literal so binding succeeds; cases that rely on a non-default
    /// escape char will silently keep the default backslash semantics.
    fn consume_escape_clause(&mut self) -> DbResult<()> {
        if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("escape"))
        {
            self.advance();
            // Consume the escape literal: typically a string literal but
            // tolerate other simple tokens for resilience.
            match &self.current().kind {
                TokenKind::String(_) | TokenKind::Identifier(_) => {
                    self.advance();
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Check if the current token is a JSON-clause keyword that terminates
    /// a value expression inside JSON_OBJECT / JSON_OBJECTAGG calls.
    fn is_json_clause_keyword(&self) -> bool {
        match &self.current().kind {
            TokenKind::Identifier(s) => {
                matches!(s.to_ascii_lowercase().as_str(), "absent" | "keys")
            }
            TokenKind::Keyword(
                Keyword::Null | Keyword::With | Keyword::Without | Keyword::Unique,
            ) => true,
            _ => false,
        }
    }

    /// Check if a token is the UNIQUE keyword (reserved) or identifier.
    fn is_unique_token(kind: &TokenKind) -> bool {
        matches!(kind, TokenKind::Keyword(Keyword::Unique))
            || matches!(kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("unique"))
    }

    /// Check if a token is the KEYS identifier.
    fn is_keys_token(kind: &TokenKind) -> bool {
        matches!(kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("keys"))
    }

    /// Skip trailing SQL/JSON clauses before ')':
    /// NULL ON NULL, ABSENT ON NULL, WITH UNIQUE [KEYS],
    /// WITHOUT UNIQUE [KEYS], WITH UNIQUE KEYS RETURNING ...
    fn parse_json_trailing_clauses(&mut self) -> JsonUniqueClause {
        let mut unique_clause = JsonUniqueClause::Unspecified;
        loop {
            // NULL ON NULL
            if self.current().kind == TokenKind::Keyword(Keyword::Null) {
                if self.index + 1 < self.tokens.len()
                    && self.tokens[self.index + 1].kind == TokenKind::Keyword(Keyword::On)
                {
                    self.advance(); // consume NULL
                    self.advance(); // consume ON
                    let _ = self.consume_keyword(Keyword::Null); // consume NULL
                    continue;
                }
                break;
            }
            // ABSENT ON NULL
            if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("absent"))
            {
                if self.index + 1 < self.tokens.len()
                    && self.tokens[self.index + 1].kind == TokenKind::Keyword(Keyword::On)
                {
                    self.advance(); // consume ABSENT
                    self.advance(); // consume ON
                    let _ = self.consume_keyword(Keyword::Null); // consume NULL
                    continue;
                }
                break;
            }
            // WITH UNIQUE [KEYS]
            if self.current().kind == TokenKind::Keyword(Keyword::With) {
                if self.index + 1 < self.tokens.len()
                    && Self::is_unique_token(&self.tokens[self.index + 1].kind)
                {
                    self.advance(); // consume WITH
                    self.advance(); // consume UNIQUE
                    unique_clause = JsonUniqueClause::With;
                    // Optional KEYS
                    if Self::is_keys_token(&self.current().kind) {
                        self.advance();
                    }
                    continue;
                }
                break;
            }
            // WITHOUT UNIQUE [KEYS]
            if self.current().kind == TokenKind::Keyword(Keyword::Without) {
                if self.index + 1 < self.tokens.len()
                    && Self::is_unique_token(&self.tokens[self.index + 1].kind)
                {
                    self.advance(); // consume WITHOUT
                    self.advance(); // consume UNIQUE
                    unique_clause = JsonUniqueClause::Without;
                    if Self::is_keys_token(&self.current().kind) {
                        self.advance();
                    }
                    continue;
                }
                break;
            }
            break;
        }
        unique_clause
    }

    pub(crate) fn parse_object_name(&mut self) -> DbResult<ObjectName> {
        let (first, first_span) = self.expect_identifier()?;
        let mut parts = vec![first];
        let mut span = first_span;

        while self.consume_kind(&TokenKind::Dot) {
            // Handle qualified star: table.* or schema.table.*
            if self.current().kind == TokenKind::Star {
                let star_token = self.advance();
                parts.push("*".to_owned());
                span = span.merge(star_token.span);
                break;
            }
            let (part, part_span) = self.expect_identifier()?;
            parts.push(part);
            span = span.merge(part_span);
        }

        Ok(ObjectName { parts, span })
    }
}
