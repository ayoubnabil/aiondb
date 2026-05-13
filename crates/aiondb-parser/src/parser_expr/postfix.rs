#![allow(clippy::collapsible_if, clippy::doc_markdown)]

use aiondb_core::{DbError, DbResult, SqlState};

use super::*;

const MAX_ARRAY_SUBSCRIPT_DIMENSIONS: usize = 6;

enum ReadSubscript {
    Index(Expr),
    Slice {
        lower: Expr,
        upper: Expr,
        lower_omitted: bool,
        upper_omitted: bool,
    },
}

impl Parser {
    pub(super) fn parse_postfix_cast_expr(&mut self) -> DbResult<Expr> {
        let mut expr = self.parse_primary_expr()?;

        loop {
            if self.consume_kind(&TokenKind::DoubleColon) {
                let (
                    data_type,
                    pg_type_name_hint,
                    interval_precision,
                    interval_fields,
                    temporal_precision,
                    char_length,
                ) = self.parse_data_type_with_pg_type_name_hint()?;
                let span = expr.span().merge(self.previous_span());
                expr = self.build_cast_expression(
                    expr,
                    data_type,
                    pg_type_name_hint,
                    interval_precision,
                    interval_fields,
                    temporal_precision,
                    char_length,
                    span,
                );
                continue;
            }

            // Cypher map projection: `n {.name, alias: expr, .*}`.
            if self.cypher_mode && self.current().kind == TokenKind::LBrace {
                expr = self.parse_cypher_map_projection(expr)?;
                continue;
            }

            // Array subscript: expr[index] or expr[lo:hi] or expr[:hi] or expr[lo:] or expr[:]
            if self.current().kind == TokenKind::LBracket {
                let subscripts = self.parse_read_subscripts()?;
                let next_depth = self.array_subscript_depth(&expr) + subscripts.len();
                if next_depth > MAX_ARRAY_SUBSCRIPT_DIMENSIONS {
                    return Err(DbError::program_limit(format!(
                        "number of array dimensions ({next_depth}) exceeds the maximum allowed ({MAX_ARRAY_SUBSCRIPT_DIMENSIONS})"
                    )));
                }
                expr = self.build_array_subscript_expr(expr, subscripts);
                continue;
            }

            // COLLATE "collation" - skip, return expr unchanged
            if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("collate"))
            {
                self.advance(); // consume COLLATE
                                // Skip collation name (identifier or quoted identifier, possibly dotted)
                let _ = self.parse_object_name()?;
                continue;
            }

            // Field access: expr.field_name or composite row expansion: expr.*
            // Handles (func(args)).field, (func(args)).*, (row_val).col, etc.
            if self.current().kind == TokenKind::Dot {
                // Check if this is .* (composite expansion)
                if self.index + 1 < self.tokens.len()
                    && self.tokens[self.index + 1].kind == TokenKind::Star
                {
                    self.advance(); // consume '.'
                    let _star_token = self.advance(); // consume '*'
                                                      // Rewrite (expr).* as just expr - we don't support row
                                                      // expansion, but keeping the original expression lets
                                                      // downstream stages work (e.g. avoids "SELECT * requires FROM").
                    continue;
                }
                // Check if this is .identifier (field access)
                let next_is_ident = if self.index + 1 < self.tokens.len() {
                    match &self.tokens[self.index + 1].kind {
                        TokenKind::Identifier(_) => true,
                        TokenKind::Keyword(k) => k.is_unreserved(),
                        _ => false,
                    }
                } else {
                    false
                };
                if next_is_ident {
                    self.advance(); // consume '.'
                    let (mut field, field_span) = self.expect_identifier()?;
                    // In Cypher, property names are case-sensitive. Recover
                    // the original casing from the source span (the lexer
                    // folds unquoted identifiers to lowercase for PG compat).
                    if self.cypher_mode {
                        if let Some(raw) = self.source.get(field_span.start..field_span.end) {
                            if let Some(inner) =
                                raw.strip_prefix('"').and_then(|s| s.strip_suffix('"'))
                            {
                                field = inner.replace("\"\"", "\"");
                            } else if let Some(inner) =
                                raw.strip_prefix('`').and_then(|s| s.strip_suffix('`'))
                            {
                                field = inner.to_owned();
                            } else if !raw.is_empty() {
                                field = raw.to_owned();
                            }
                        }
                    }
                    let span = expr.span().merge(field_span);
                    expr = match expr {
                        // In Cypher, preserve the original variable/property
                        // qualification so later stages can resolve `u.name`
                        // and `target.embedding` against the bound graph
                        // variable instead of falling back to an unqualified
                        // column lookup.
                        Expr::Identifier(name) if self.cypher_mode => {
                            let mut parts = name.parts;
                            parts.push(field);
                            Expr::Identifier(ObjectName { parts, span })
                        }
                        // Keep `table.column` style references on the old
                        // compatibility path by dropping the qualifier.
                        Expr::Identifier(_) => Expr::Identifier(ObjectName {
                            parts: vec![field],
                            span,
                        }),
                        // For actual postfix field access on a computed value
                        // such as `arr[1].f2`, preserve the base expression.
                        other => {
                            let jsonb_each_value_field = matches!(
                                &other,
                                Expr::FunctionCall { name, .. }
                                    if name
                                        .parts
                                        .last()
                                        .is_some_and(|part| part.eq_ignore_ascii_case("jsonb_each"))
                                        && field.eq_ignore_ascii_case("value")
                            );
                            let field_access = Expr::FunctionCall {
                                name: ObjectName {
                                    parts: vec!["__aiondb_composite_field".to_owned()],
                                    span,
                                },
                                args: vec![
                                    other,
                                    Expr::Literal(Literal::String(field), field_span),
                                ],
                                distinct: false,
                                filter: None,
                                span,
                            };
                            if jsonb_each_value_field {
                                Expr::Cast {
                                    expr: Box::new(field_access),
                                    data_type: DataType::Jsonb,
                                    span,
                                }
                            } else {
                                field_access
                            }
                        }
                    };
                    continue;
                }
            }

            // AT TIME ZONE 'timezone' - postfix timezone conversion.
            // Rewrite as a function call: timezone(zone_expr, expr).
            if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("at"))
                && self.index + 2 < self.tokens.len()
                && matches!(
                    self.tokens[self.index + 1].kind,
                    TokenKind::Keyword(Keyword::Time)
                )
                && matches!(
                    self.tokens[self.index + 2].kind,
                    TokenKind::Keyword(Keyword::Zone)
                )
            {
                self.advance(); // consume AT
                self.advance(); // consume TIME
                self.advance(); // consume ZONE
                let tz_expr = self.parse_primary_expr()?;
                let span = expr.span().merge(tz_expr.span());
                expr = Expr::FunctionCall {
                    name: ObjectName {
                        parts: vec!["timezone".to_owned()],
                        span,
                    },
                    args: vec![tz_expr, expr],
                    distinct: false,
                    filter: None,
                    span,
                };
                continue;
            }

            // Cypher label predicate: `expr:Label[:Label2...]`. Rewrite to a
            // chain of `__cypher_has_label(expr, 'Label')` AND'd together so
            // downstream match-aware/sql translators can pick the right
            // backing column (`__labels`).
            if self.current().kind == TokenKind::Colon
                && self.index + 1 < self.tokens.len()
                && matches!(
                    self.tokens[self.index + 1].kind,
                    TokenKind::Identifier(_) | TokenKind::Keyword(_)
                )
                && self.cypher_label_predicate_allowed(&expr)
            {
                let mut combined: Option<Expr> = None;
                while self.current().kind == TokenKind::Colon
                    && self.index + 1 < self.tokens.len()
                    && matches!(
                        self.tokens[self.index + 1].kind,
                        TokenKind::Identifier(_) | TokenKind::Keyword(_)
                    )
                {
                    self.advance(); // consume ':'
                    let (label, label_span) = self.expect_identifier()?;
                    let span = expr.span().merge(label_span);
                    let predicate = Expr::FunctionCall {
                        name: ObjectName {
                            parts: vec!["__cypher_has_label".to_owned()],
                            span,
                        },
                        args: vec![
                            expr.clone(),
                            Expr::Literal(Literal::String(label), label_span),
                        ],
                        distinct: false,
                        filter: None,
                        span,
                    };
                    combined = Some(match combined {
                        None => predicate,
                        Some(prev) => {
                            let span = prev.span().merge(predicate.span());
                            Expr::BinaryOp {
                                left: Box::new(prev),
                                op: BinaryOperator::And,
                                right: Box::new(predicate),
                                span,
                            }
                        }
                    });
                }
                let Some(combined_expr) = combined else {
                    return Err(DbError::parse_error(
                        SqlState::SyntaxError,
                        "expected at least one label predicate",
                    ));
                };
                expr = combined_expr;
                continue;
            }

            break;
        }

        Ok(expr)
    }

    /// Cypher label predicates `expr:Label` are only meaningful on bare
    /// identifiers - typing `1:foo` etc. should still parse as a column-cast
    /// or label range bound elsewhere, so restrict the rewrite to identifier
    /// receivers.
    fn cypher_label_predicate_allowed(&self, expr: &Expr) -> bool {
        matches!(expr, Expr::Identifier(_))
    }

    fn parse_cypher_map_projection(&mut self, base: Expr) -> DbResult<Expr> {
        let lbrace = self.expect_token(&TokenKind::LBrace)?;
        let mut args = vec![base.clone()];

        if !matches!(self.current().kind, TokenKind::RBrace) {
            loop {
                if self.consume_kind(&TokenKind::Dot) {
                    if self.consume_kind(&TokenKind::Star) {
                        args.push(Expr::Literal(
                            Literal::String("__all__".to_owned()),
                            self.previous_span(),
                        ));
                        args.push(Expr::Literal(Literal::Null, self.previous_span()));
                    } else {
                        let (field, field_span) = self.expect_identifier()?;
                        let key = self.cypher_source_identifier(field_span, &field);
                        args.push(Expr::Literal(Literal::String(key.clone()), field_span));
                        args.push(self.build_cypher_projection_property_expr(
                            base.clone(),
                            key,
                            field_span,
                        ));
                    }
                } else {
                    let (key, key_span) = self.expect_identifier()?;
                    let key = self.cypher_source_identifier(key_span, &key);
                    if self.consume_kind(&TokenKind::Colon) {
                        let value = self.parse_expr()?;
                        args.push(Expr::Literal(Literal::String(key), key_span));
                        args.push(value);
                    } else {
                        args.push(Expr::Literal(Literal::String(key.clone()), key_span));
                        args.push(Expr::Identifier(ObjectName {
                            parts: vec![key],
                            span: key_span,
                        }));
                    }
                }

                if !self.consume_kind(&TokenKind::Comma) {
                    break;
                }
            }
        }

        let rbrace = self.expect_token(&TokenKind::RBrace)?;
        let span = base.span().merge(lbrace.span).merge(rbrace.span);
        Ok(Expr::FunctionCall {
            name: ObjectName {
                parts: vec!["__cypher_map_projection".to_owned()],
                span,
            },
            args,
            distinct: false,
            filter: None,
            span,
        })
    }

    fn build_cypher_projection_property_expr(
        &self,
        base: Expr,
        property: String,
        property_span: crate::Span,
    ) -> Expr {
        let span = base.span().merge(property_span);
        match base {
            Expr::Identifier(mut name) => {
                name.parts.push(property);
                name.span = span;
                Expr::Identifier(name)
            }
            other => Expr::FunctionCall {
                name: ObjectName {
                    parts: vec!["__aiondb_composite_field".to_owned()],
                    span,
                },
                args: vec![
                    other,
                    Expr::Literal(Literal::String(property), property_span),
                ],
                distinct: false,
                filter: None,
                span,
            },
        }
    }

    fn cypher_source_identifier(&self, span: crate::Span, fallback: &str) -> String {
        let Some(raw) = self.source.get(span.start..span.end) else {
            return fallback.to_owned();
        };
        if let Some(inner) = raw.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
            inner.replace("\"\"", "\"")
        } else if let Some(inner) = raw.strip_prefix('`').and_then(|s| s.strip_suffix('`')) {
            inner.to_owned()
        } else if raw.is_empty() {
            fallback.to_owned()
        } else {
            raw.to_owned()
        }
    }

    fn parse_read_subscripts(&mut self) -> DbResult<Vec<ReadSubscript>> {
        let mut subscripts = Vec::new();
        while self.current().kind == TokenKind::LBracket {
            self.advance(); // consume '['
            let placeholder_span = self.current().span;
            let null_bound = || Expr::Literal(Literal::Null, placeholder_span);

            let consume_slice_separator = |p: &mut Self| -> bool {
                if p.current().kind == TokenKind::Colon {
                    p.advance();
                    return true;
                }
                // Cypher slice: `..` (two adjacent Dot tokens).
                if p.current().kind == TokenKind::Dot
                    && p.index + 1 < p.tokens.len()
                    && p.tokens[p.index + 1].kind == TokenKind::Dot
                {
                    p.advance();
                    p.advance();
                    return true;
                }
                false
            };

            let subscript = if consume_slice_separator(self) {
                let upper_omitted = self.current().kind == TokenKind::RBracket;
                let upper = if upper_omitted {
                    null_bound()
                } else {
                    self.parse_expr()?
                };
                ReadSubscript::Slice {
                    lower: null_bound(),
                    upper,
                    lower_omitted: true,
                    upper_omitted,
                }
            } else {
                let lower = self.parse_expr()?;
                if consume_slice_separator(self) {
                    let upper_omitted = self.current().kind == TokenKind::RBracket;
                    let upper = if upper_omitted {
                        null_bound()
                    } else {
                        self.parse_expr()?
                    };
                    ReadSubscript::Slice {
                        lower,
                        upper,
                        lower_omitted: false,
                        upper_omitted,
                    }
                } else {
                    ReadSubscript::Index(lower)
                }
            };

            self.expect_token(&TokenKind::RBracket)?;
            subscripts.push(subscript);
        }
        Ok(subscripts)
    }

    fn build_array_subscript_expr(&self, base: Expr, subscripts: Vec<ReadSubscript>) -> Expr {
        let span = subscripts
            .iter()
            .fold(base.span(), |acc, subscript| match subscript {
                ReadSubscript::Index(index) => acc.merge(index.span()),
                ReadSubscript::Slice { lower, upper, .. } => {
                    acc.merge(lower.span()).merge(upper.span())
                }
            });
        let has_slice = subscripts
            .iter()
            .any(|subscript| matches!(subscript, ReadSubscript::Slice { .. }));

        if !has_slice {
            let mut expr = base;
            for subscript in subscripts {
                let index = match subscript {
                    ReadSubscript::Index(index) => index,
                    ReadSubscript::Slice { lower, .. } => lower,
                };
                let expr_span = expr.span();
                let index_span = index.span();
                expr = Expr::FunctionCall {
                    name: ObjectName {
                        parts: vec!["array_get".to_owned()],
                        span: expr_span.merge(index_span),
                    },
                    args: vec![expr, index],
                    distinct: false,
                    filter: None,
                    span: expr_span.merge(index_span),
                };
            }
            return expr;
        }

        let mut args = Vec::with_capacity(1 + subscripts.len() * 4);
        args.push(base);
        for subscript in subscripts {
            match subscript {
                ReadSubscript::Index(index) => {
                    let index_span = index.span();
                    args.push(Expr::Literal(Literal::Integer(1), index_span));
                    args.push(index);
                    args.push(Expr::Literal(Literal::Boolean(false), index_span));
                    args.push(Expr::Literal(Literal::Boolean(false), index_span));
                }
                ReadSubscript::Slice {
                    lower,
                    upper,
                    lower_omitted,
                    upper_omitted,
                } => {
                    let flag_span = lower.span().merge(upper.span());
                    args.push(lower);
                    args.push(upper);
                    args.push(Expr::Literal(Literal::Boolean(lower_omitted), flag_span));
                    args.push(Expr::Literal(Literal::Boolean(upper_omitted), flag_span));
                }
            }
        }

        Expr::FunctionCall {
            name: ObjectName {
                parts: vec!["__aiondb_array_slice".to_owned()],
                span,
            },
            args,
            distinct: false,
            filter: None,
            span,
        }
    }

    fn array_subscript_depth(&self, expr: &Expr) -> usize {
        match expr {
            Expr::FunctionCall { name, args, .. }
                if name.parts.len() == 1
                    && (name.parts[0].eq_ignore_ascii_case("array_get")
                        || name.parts[0].eq_ignore_ascii_case("__aiondb_array_slice"))
                    && !args.is_empty() =>
            {
                if name.parts[0].eq_ignore_ascii_case("__aiondb_array_slice") && args.len() > 1 {
                    self.array_subscript_depth(&args[0]) + (args.len() - 1) / 4
                } else {
                    self.array_subscript_depth(&args[0]) + 1
                }
            }
            _ => 0,
        }
    }

    /// Check if the current keyword is a type name that can be used in
    /// the PostgreSQL `typename 'literal'` cast syntax.
    pub(super) fn is_type_keyword_for_literal_cast(&self) -> bool {
        let is_type_kw = matches!(
            self.current().kind,
            TokenKind::Keyword(
                Keyword::Int
                    | Keyword::Int2
                    | Keyword::Int4
                    | Keyword::Int8
                    | Keyword::Integer
                    | Keyword::SmallInt
                    | Keyword::BigInt
                    | Keyword::Float
                    | Keyword::Float4
                    | Keyword::Float8
                    | Keyword::Real
                    | Keyword::Double
                    | Keyword::Bool
                    | Keyword::Boolean
                    | Keyword::Char
                    | Keyword::Character
                    | Keyword::Varchar
                    | Keyword::Text
                    | Keyword::Numeric
                    | Keyword::Decimal
                    | Keyword::Date
                    | Keyword::Time
                    | Keyword::Timestamp
                    | Keyword::Timestamptz
                    | Keyword::Interval
                    | Keyword::Uuid
                    | Keyword::Jsonb
                    | Keyword::Blob
                    | Keyword::Bytea
                    | Keyword::Bit
                    | Keyword::Timetz
                    | Keyword::Serial
                    | Keyword::BigSerial
            )
        );
        if !is_type_kw {
            return false;
        }
        // Find how many tokens the type name consumes before the string literal.
        // For most types, the literal is at index+1.
        // For `TIMESTAMP/TIME WITH TIME ZONE 'str'` or
        // `TIMESTAMP/TIME WITHOUT TIME ZONE 'str'`, we must skip up to 3 extra
        // keywords (WITH/WITHOUT, TIME, ZONE).
        // For `DOUBLE PRECISION 'str'`, skip PRECISION.
        let mut probe = self.index + 1;
        let len = self.tokens.len();
        while probe < len {
            match &self.tokens[probe].kind {
                TokenKind::LParen => {
                    let mut depth = 1usize;
                    probe += 1;
                    while probe < len && depth > 0 {
                        match self.tokens[probe].kind {
                            TokenKind::LParen => depth += 1,
                            TokenKind::RParen => depth -= 1,
                            _ => {}
                        }
                        probe += 1;
                    }
                }
                TokenKind::Keyword(
                    Keyword::With
                    | Keyword::Without
                    | Keyword::Time
                    | Keyword::Zone
                    | Keyword::Precision
                    | Keyword::Varying,
                ) => {
                    probe += 1;
                }
                _ => break,
            }
        }
        probe < len && matches!(self.tokens[probe].kind, TokenKind::String(_))
    }

    /// Check if the current token is an identifier that names a recognized
    /// data type and is followed by a string literal, forming a PG-style
    /// literal cast (e.g. `json '{}'`, `point '(1,2)'`, `xml '<a/>'`).
    pub(super) fn is_identifier_type_literal_cast(&self) -> bool {
        let ident = match &self.current().kind {
            TokenKind::Identifier(s) => s.to_ascii_uppercase(),
            _ => return false,
        };
        // Only recognize identifier types that parse_identifier_type handles
        let is_known = matches!(
            ident.as_str(),
            "OID"
                | "NAME"
                | "MONEY"
                | "INET"
                | "CIDR"
                | "MACADDR"
                | "MACADDR8"
                | "POINT"
                | "BOX"
                | "LINE"
                | "LSEG"
                | "POLYGON"
                | "CIRCLE"
                | "PATH"
                | "JSON"
                | "XML"
                | "BIT"
                | "VARBIT"
                | "REGCLASS"
                | "REGTYPE"
                | "REGPROC"
                | "REGPROCEDURE"
                | "REGOPER"
                | "REGOPERATOR"
                | "REGNAMESPACE"
                | "REGROLE"
                | "REGCOLLATION"
                | "REGCONFIG"
                | "REGDICTIONARY"
                | "XID"
                | "XID8"
                | "CID"
                | "TID"
                | "INT2VECTOR"
                | "OIDVECTOR"
                | "HALFVEC"
                | "SPARSEVEC"
                | "REFCURSOR"
                | "RECORD"
                | "PG_LSN"
                | "PG_SNAPSHOT"
                | "TSQUERY"
                | "TSVECTOR"
                | "TXID_SNAPSHOT"
                | "TIMESTAMPTZ"
                | "TIMETZ"
        );
        if !is_known {
            return false;
        }
        // Check that the next token is a string literal
        let next = self.index + 1;
        // For BIT, also allow BIT VARYING '...'
        if ident == "BIT" {
            if next < self.tokens.len()
                && matches!(self.tokens[next].kind, TokenKind::Keyword(Keyword::Varying))
            {
                let after_varying = next + 1;
                return after_varying < self.tokens.len()
                    && matches!(self.tokens[after_varying].kind, TokenKind::String(_));
            }
        }
        next < self.tokens.len() && matches!(self.tokens[next].kind, TokenKind::String(_))
    }
}
