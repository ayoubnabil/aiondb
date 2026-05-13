#![allow(clippy::vec_init_then_push)]

use super::*;

impl Parser {
    pub(super) fn parse_identifier_or_function_expr(&mut self, name: ObjectName) -> DbResult<Expr> {
        // Special SQL-standard function syntax handling
        let fn_lower = name.parts.last().map(|s| s.to_ascii_lowercase());

        // EXTRACT(field FROM expr) → extract(field, expr)
        if fn_lower.as_deref() == Some("extract") {
            let field = self.parse_expr()?;
            self.expect_keyword(Keyword::From)?;
            let source = self.parse_expr()?;
            let rparen = self.expect_token(&TokenKind::RParen)?;
            return Ok(Expr::FunctionCall {
                span: name.span.merge(rparen.span),
                name,
                args: vec![field, source],
                distinct: false,
                filter: None,
            });
        }

        // SUBSTRING(expr FROM start [FOR length]) → substring(expr, start [, length])
        // Also handles: SUBSTRING(expr, start [, length])
        // Also handles: SUBSTRING(expr SIMILAR pattern ESCAPE escape) → substring(expr, pattern, escape)
        if fn_lower.as_deref() == Some("substring") {
            let expr_arg = self.parse_expr()?;
            if self.consume_keyword(Keyword::From).is_some() {
                let start = self.parse_expr()?;
                let mut func_args = vec![expr_arg, start];
                if self.consume_keyword(Keyword::For).is_some() {
                    func_args.push(self.parse_expr()?);
                }
                let rparen = self.expect_token(&TokenKind::RParen)?;
                return Ok(Expr::FunctionCall {
                    span: name.span.merge(rparen.span),
                    name,
                    args: func_args,
                    distinct: false,
                    filter: None,
                });
            }
            // SUBSTRING(expr SIMILAR pattern ESCAPE escape)
            if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("similar"))
            {
                self.advance(); // consume SIMILAR
                let pattern = self.parse_expr()?;
                let func_args = vec![expr_arg, pattern];
                self.reject_escape_clause()?;
                let rparen = self.expect_token(&TokenKind::RParen)?;
                return Ok(Expr::FunctionCall {
                    span: name.span.merge(rparen.span),
                    name,
                    args: func_args,
                    distinct: false,
                    filter: None,
                });
            }
            // SUBSTRING(expr FOR length) - no FROM, just FOR
            if self.consume_keyword(Keyword::For).is_some() {
                let length = self.parse_expr()?;
                // substring(expr, 1, length)
                let func_args = vec![
                    expr_arg,
                    Expr::Literal(crate::ast::Literal::Integer(1), length.span()),
                    length,
                ];
                let rparen = self.expect_token(&TokenKind::RParen)?;
                return Ok(Expr::FunctionCall {
                    span: name.span.merge(rparen.span),
                    name,
                    args: func_args,
                    distinct: false,
                    filter: None,
                });
            }
            // Fall through to normal comma-separated args
            let mut func_args = vec![expr_arg];
            while self.consume_kind(&TokenKind::Comma) {
                func_args.push(self.parse_expr()?);
            }
            let rparen = self.expect_token(&TokenKind::RParen)?;
            return Ok(Expr::FunctionCall {
                span: name.span.merge(rparen.span),
                name,
                args: func_args,
                distinct: false,
                filter: None,
            });
        }

        // TRIM([LEADING|TRAILING|BOTH] [chars] FROM expr) → trim(expr [, chars])
        if fn_lower.as_deref() == Some("trim") {
            // Skip optional LEADING/TRAILING/BOTH identifiers
            if matches!(&self.current().kind, TokenKind::Identifier(s) if matches!(s.to_ascii_lowercase().as_str(), "leading" | "trailing" | "both"))
            {
                self.advance();
            }
            // TRIM(FROM expr) - no chars
            if self.consume_keyword(Keyword::From).is_some() {
                let source = self.parse_expr()?;
                let rparen = self.expect_token(&TokenKind::RParen)?;
                return Ok(Expr::FunctionCall {
                    span: name.span.merge(rparen.span),
                    name,
                    args: vec![source],
                    distinct: false,
                    filter: None,
                });
            }
            let first = self.parse_expr()?;
            if self.consume_keyword(Keyword::From).is_some() {
                let source = self.parse_expr()?;
                let rparen = self.expect_token(&TokenKind::RParen)?;
                return Ok(Expr::FunctionCall {
                    span: name.span.merge(rparen.span),
                    name,
                    args: vec![source, first],
                    distinct: false,
                    filter: None,
                });
            }
            // Normal function call
            let mut func_args = vec![first];
            while self.consume_kind(&TokenKind::Comma) {
                func_args.push(self.parse_expr()?);
            }
            let rparen = self.expect_token(&TokenKind::RParen)?;
            return Ok(Expr::FunctionCall {
                span: name.span.merge(rparen.span),
                name,
                args: func_args,
                distinct: false,
                filter: None,
            });
        }

        // POSITION(expr IN expr) → position(expr, expr)
        if fn_lower.as_deref() == Some("position") {
            let needle = self.parse_additive_expr()?;
            if self.consume_keyword(Keyword::In).is_some() {
                let haystack = self.parse_additive_expr()?;
                let rparen = self.expect_token(&TokenKind::RParen)?;
                return Ok(Expr::FunctionCall {
                    span: name.span.merge(rparen.span),
                    name,
                    args: vec![needle, haystack],
                    distinct: false,
                    filter: None,
                });
            }
            // Fall through to normal
            let mut func_args = vec![needle];
            while self.consume_kind(&TokenKind::Comma) {
                func_args.push(self.parse_expr()?);
            }
            let rparen = self.expect_token(&TokenKind::RParen)?;
            return Ok(Expr::FunctionCall {
                span: name.span.merge(rparen.span),
                name,
                args: func_args,
                distinct: false,
                filter: None,
            });
        }

        // OVERLAY(string PLACING replacement FROM start [FOR count])
        // → overlay(string, replacement, start [, count])
        if fn_lower.as_deref() == Some("overlay") {
            let string_arg = self.parse_expr()?;
            // Check for SQL-standard PLACING syntax
            if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("placing"))
            {
                self.advance(); // consume PLACING
                let replacement = self.parse_expr()?;
                self.expect_keyword(Keyword::From)?;
                let start = self.parse_expr()?;
                let mut func_args = vec![string_arg, replacement, start];
                if self.consume_keyword(Keyword::For).is_some() {
                    func_args.push(self.parse_expr()?);
                }
                let rparen = self.expect_token(&TokenKind::RParen)?;
                return Ok(Expr::FunctionCall {
                    span: name.span.merge(rparen.span),
                    name,
                    args: func_args,
                    distinct: false,
                    filter: None,
                });
            }
            // Fall through to normal comma-separated args
            let mut func_args = vec![string_arg];
            while self.consume_kind(&TokenKind::Comma) {
                func_args.push(self.parse_expr()?);
            }
            let rparen = self.expect_token(&TokenKind::RParen)?;
            return Ok(Expr::FunctionCall {
                span: name.span.merge(rparen.span),
                name,
                args: func_args,
                distinct: false,
                filter: None,
            });
        }

        // XMLPARSE(CONTENT|DOCUMENT expr) → xmlparse(expr)
        // Skip the CONTENT/DOCUMENT mode keyword.
        if fn_lower.as_deref() == Some("xmlparse") {
            // Skip CONTENT or DOCUMENT
            if matches!(&self.current().kind, TokenKind::Identifier(s) if matches!(s.to_ascii_lowercase().as_str(), "content" | "document"))
            {
                self.advance();
            }
            let expr_arg = self.parse_expr()?;
            let rparen = self.expect_token(&TokenKind::RParen)?;
            return Ok(Expr::FunctionCall {
                span: name.span.merge(rparen.span),
                name,
                args: vec![expr_arg],
                distinct: false,
                filter: None,
            });
        }

        // XMLSERIALIZE(CONTENT|DOCUMENT expr AS type [INDENT|NO INDENT])
        // → xmlserialize(expr)
        if fn_lower.as_deref() == Some("xmlserialize") {
            // Skip CONTENT or DOCUMENT
            if matches!(&self.current().kind, TokenKind::Identifier(s) if matches!(s.to_ascii_lowercase().as_str(), "content" | "document"))
            {
                self.advance();
            }
            let expr_arg = self.parse_expr()?;
            // Skip AS type [INDENT|NO INDENT]
            if self.consume_keyword(Keyword::As).is_some() {
                let _ = self.parse_data_type()?;
            }
            // Skip optional INDENT / NO INDENT
            if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("indent"))
            {
                self.advance();
            } else if self.consume_keyword(Keyword::No).is_some() {
                // NO INDENT
                if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("indent"))
                {
                    self.advance();
                }
            }
            let rparen = self.expect_token(&TokenKind::RParen)?;
            return Ok(Expr::FunctionCall {
                span: name.span.merge(rparen.span),
                name,
                args: vec![expr_arg],
                distinct: false,
                filter: None,
            });
        }

        // XMLELEMENT(NAME ident, args...) → xmlelement(args...)
        // Skip the NAME keyword and element name.
        if fn_lower.as_deref() == Some("xmlelement") {
            let mut func_args = Vec::new();
            // Skip NAME identifier if present
            if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("name"))
            {
                self.advance(); // consume NAME
                                // Consume element name (identifier or keyword)
                if matches!(
                    self.current().kind,
                    TokenKind::Identifier(_) | TokenKind::Keyword(_)
                ) {
                    self.advance();
                }
            }
            // Parse remaining args
            if !self.consume_kind(&TokenKind::RParen) {
                // Skip leading comma after element name
                let _ = self.consume_kind(&TokenKind::Comma);
                if !self.consume_kind(&TokenKind::RParen) {
                    loop {
                        // Skip xmlattributes(...) blocks
                        if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("xmlattributes"))
                        {
                            self.advance(); // consume xmlattributes
                            if self.consume_kind(&TokenKind::LParen) {
                                let mut depth = 1u32;
                                while !self.is_eof() && depth > 0 {
                                    if self.current().kind == TokenKind::LParen {
                                        depth += 1;
                                    } else if self.current().kind == TokenKind::RParen {
                                        depth -= 1;
                                        if depth == 0 {
                                            self.advance();
                                            break;
                                        }
                                    }
                                    self.advance();
                                }
                            }
                        } else {
                            func_args.push(self.parse_expr()?);
                        }
                        if !self.consume_kind(&TokenKind::Comma) {
                            self.expect_token(&TokenKind::RParen)?;
                            break;
                        }
                    }
                }
            }
            return Ok(Expr::FunctionCall {
                span: name.span.merge(self.previous_span()),
                name,
                args: func_args,
                distinct: false,
                filter: None,
            });
        }

        // XMLROOT(xml_expr, VERSION value [, STANDALONE ...])
        // → xmlroot(xml_expr, value)
        if fn_lower.as_deref() == Some("xmlroot") {
            let xml_arg = self.parse_expr()?;
            let mut func_args = vec![xml_arg];
            if self.consume_kind(&TokenKind::Comma) {
                // Skip VERSION keyword
                if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("version"))
                {
                    self.advance();
                }
                // Handle VERSION NO VALUE
                if self.consume_keyword(Keyword::No).is_some() {
                    // NO VALUE → skip, use NULL
                    if matches!(
                        &self.current().kind,
                        TokenKind::Keyword(Keyword::Value | Keyword::Values)
                            | TokenKind::Identifier(_)
                    ) {
                        self.advance(); // consume VALUE
                    }
                    func_args.push(Expr::Literal(Literal::Null, self.previous_span()));
                } else {
                    func_args.push(self.parse_expr()?);
                }
                // Skip optional STANDALONE clause
                if self.consume_kind(&TokenKind::Comma) {
                    // Skip STANDALONE YES|NO|NO VALUE
                    while !self.is_eof()
                        && !matches!(
                            self.current().kind,
                            TokenKind::RParen | TokenKind::Semicolon
                        )
                    {
                        self.advance();
                    }
                }
            }
            let rparen = self.expect_token(&TokenKind::RParen)?;
            return Ok(Expr::FunctionCall {
                span: name.span.merge(rparen.span),
                name,
                args: func_args,
                distinct: false,
                filter: None,
            });
        }

        // XMLFOREST(expr [AS alias], ...) → xmlforest(expr, ...)
        // Skip AS alias after each expression.
        if fn_lower.as_deref() == Some("xmlforest") {
            let mut func_args = Vec::new();
            if !self.consume_kind(&TokenKind::RParen) {
                loop {
                    func_args.push(self.parse_expr()?);
                    // Skip optional AS alias
                    if self.consume_keyword(Keyword::As).is_some() {
                        let _ = self.expect_identifier();
                    }
                    if !self.consume_kind(&TokenKind::Comma) {
                        self.expect_token(&TokenKind::RParen)?;
                        break;
                    }
                }
            }
            return Ok(Expr::FunctionCall {
                span: name.span.merge(self.previous_span()),
                name,
                args: func_args,
                distinct: false,
                filter: None,
            });
        }

        // XMLPI(NAME target [, content]) → xmlpi(content)
        if fn_lower.as_deref() == Some("xmlpi") {
            let mut func_args = Vec::new();
            // Skip NAME keyword and target identifier
            if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("name"))
            {
                self.advance(); // consume NAME
                                // Consume target (identifier, keyword, or quoted identifier)
                if matches!(
                    self.current().kind,
                    TokenKind::Identifier(_) | TokenKind::Keyword(_)
                ) {
                    self.advance();
                }
            }
            if self.consume_kind(&TokenKind::Comma) {
                func_args.push(self.parse_expr()?);
            }
            let rparen = self.expect_token(&TokenKind::RParen)?;
            return Ok(Expr::FunctionCall {
                span: name.span.merge(rparen.span),
                name,
                args: func_args,
                distinct: false,
                filter: None,
            });
        }

        // XMLEXISTS(xpath_expr PASSING [BY REF] xml_expr [BY REF])
        // XMLTABLE(xpath_expr PASSING [BY REF] xml_expr [BY REF]
        //          COLUMNS col type [PATH xpath], ...)
        // → skip balanced parens as these use complex non-standard syntax
        if matches!(fn_lower.as_deref(), Some("xmlexists" | "xmltable")) {
            let mut func_args = Vec::new();
            // Parse first arg (xpath expression)
            func_args.push(self.parse_expr()?);
            // Skip remaining tokens including PASSING, COLUMNS, etc.
            // until balanced closing paren
            let mut paren_depth = 1u32;
            while !self.is_eof() && paren_depth > 0 {
                match self.current().kind {
                    TokenKind::LParen => {
                        paren_depth += 1;
                        self.advance();
                    }
                    TokenKind::RParen => {
                        paren_depth -= 1;
                        if paren_depth == 0 {
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
            return Ok(Expr::FunctionCall {
                span: name.span.merge(rparen.span),
                name,
                args: func_args,
                distinct: false,
                filter: None,
            });
        }

        let is_json_object = fn_lower.as_deref() == Some("json_object");
        let is_json_array = fn_lower.as_deref() == Some("json_array");
        let mut json_array_subquery_form = false;
        let mut json_object_keys: Vec<Expr> = Vec::new();
        let mut json_unique_clause = JsonUniqueClause::Unspecified;
        let mut json_returning_type: Option<DataType> = None;

        let mut args = Vec::new();
        let capture_make_interval_named_args = fn_lower.as_deref() == Some("make_interval");
        let mut make_interval_named_args: Vec<(String, Expr, crate::Span)> = Vec::new();
        let mut saw_variadic = false;
        let mut array_agg_order_descending = None;
        let mut jsonb_agg_ordered_payload: Option<(bool, bool, Vec<Expr>)> = None;
        let agg_distinct = self.consume_keyword(Keyword::Distinct).is_some();
        if !agg_distinct && is_aggregate_function_name(fn_lower.as_deref()) {
            // Allow explicit aggregate ALL qualifier, e.g. MIN(ALL col).
            // It is semantically the default and should be consumed so `ALL`
            // does not get parsed as an identifier argument.
            let _ = self.consume_keyword(Keyword::All);
        }
        // Handle SELECT inside function args: JSON_ARRAY(SELECT ...)
        if matches!(self.current().kind, TokenKind::Keyword(Keyword::Select)) {
            let query = self.parse_subquery_select()?;
            let rparen_span = self.previous_span();
            if is_json_array {
                json_array_subquery_form = true;
                args.push(Expr::ArraySubquery {
                    query: Box::new(query),
                    span: name.span.merge(rparen_span),
                });
            } else {
                args.push(Expr::Subquery {
                    query: Box::new(query),
                    span: name.span.merge(rparen_span),
                });
            }
            // Parse optional trailing SQL/JSON clauses after the subquery form.
            while !self.is_eof()
                && !matches!(
                    self.current().kind,
                    TokenKind::RParen | TokenKind::Semicolon
                )
            {
                if matches!(self.current().kind, TokenKind::Keyword(Keyword::Returning)) {
                    parse_sqljson_returning_clause(
                        self,
                        is_json_object || is_json_array,
                        &mut json_returning_type,
                    )?;
                } else {
                    self.advance();
                }
            }
            self.expect_token(&TokenKind::RParen)?;
        } else if self.current().kind == TokenKind::Star {
            let star_token = self.advance();
            args.push(Expr::Identifier(ObjectName {
                parts: vec!["*".to_string()],
                span: star_token.span,
            }));
            self.expect_token(&TokenKind::RParen)?;
        } else if !self.consume_kind(&TokenKind::RParen) {
            loop {
                // Handle RETURNING inside SQL/JSON function args:
                // e.g. JSON_OBJECT(RETURNING json FORMAT JSON)
                if matches!(self.current().kind, TokenKind::Keyword(Keyword::Returning)) {
                    parse_sqljson_returning_clause(
                        self,
                        is_json_object || is_json_array,
                        &mut json_returning_type,
                    )?;
                    if !self.consume_kind(&TokenKind::Comma) {
                        self.expect_token(&TokenKind::RParen)?;
                        break;
                    }
                    continue;
                }
                // Skip VARIADIC modifier before argument expressions.
                // e.g. func(VARIADIC '{1,2,3}'::int[])
                if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("variadic"))
                {
                    self.advance(); // consume VARIADIC
                    saw_variadic = true;
                }
                let expr = self.parse_expr()?;
                // Handle named arguments: name => value  OR  name := value
                // After parsing the name as expr, check for => or :=
                if self.consume_kind(&TokenKind::FatArrow) {
                    let value = self.parse_expr()?;
                    if capture_make_interval_named_args {
                        let Some(arg_name) = named_function_argument_name(&expr) else {
                            return self
                                .syntax_error(expr.span(), "expected named parameter identifier");
                        };
                        make_interval_named_args.push((arg_name.to_string(), value, expr.span()));
                    } else {
                        args.push(value);
                    }
                } else if self.current().kind == TokenKind::Colon
                    && self.index + 1 < self.tokens.len()
                    && self.tokens[self.index + 1].kind == TokenKind::Eq
                {
                    self.advance(); // consume ':'
                    self.advance(); // consume '='
                    let value = self.parse_expr()?;
                    if capture_make_interval_named_args {
                        let Some(arg_name) = named_function_argument_name(&expr) else {
                            return self
                                .syntax_error(expr.span(), "expected named parameter identifier");
                        };
                        make_interval_named_args.push((arg_name.to_string(), value, expr.span()));
                    } else {
                        args.push(value);
                    }
                } else if self.current().kind == TokenKind::Colon
                    && self.index + 1 < self.tokens.len()
                    && !matches!(self.tokens[self.index + 1].kind, TokenKind::Colon)
                {
                    // ':' key-value separator for JSON_OBJECT syntax
                    // e.g. JSON_OBJECT('key': value, ...)
                    self.advance(); // consume ':'
                    let value = self.parse_expr()?;
                    if is_json_object {
                        json_object_keys.push(expr.clone());
                    }
                    args.push(expr); // push key
                    args.push(value); // push value
                    parse_sqljson_format_clause(
                        self,
                        is_json_object || is_json_array,
                        args.last(),
                    )?;
                } else if self.consume_keyword(Keyword::Value).is_some() {
                    // VALUE key-value separator for JSON_OBJECT:
                    // e.g. JSON_OBJECT('key' VALUE expr, ...)
                    let value = self.parse_expr()?;
                    if is_json_object {
                        json_object_keys.push(expr.clone());
                    }
                    args.push(expr); // push key
                    args.push(value); // push value
                    parse_sqljson_format_clause(
                        self,
                        is_json_object || is_json_array,
                        args.last(),
                    )?;
                } else {
                    args.push(expr);
                }
                // Skip optional FORMAT JSON/TEXT clause after any argument
                parse_sqljson_format_clause(self, is_json_object || is_json_array, args.last())?;
                if self.consume_kind(&TokenKind::Comma) {
                    continue;
                }
                // Skip optional SQL/JSON trailing clauses before ')':
                // NULL ON NULL, ABSENT ON NULL, WITH UNIQUE [KEYS],
                // WITHOUT UNIQUE [KEYS]
                json_unique_clause = self.parse_json_trailing_clauses();
                // Handle ORDER BY inside aggregate function args:
                // e.g. string_agg(col, ',' ORDER BY col)
                if self.consume_keyword(Keyword::Order).is_some() {
                    self.expect_keyword(Keyword::By)?;
                    let mut order_items = Vec::new();
                    loop {
                        let order_expr = self.parse_expr()?;
                        let ascending = self.consume_keyword(Keyword::Asc).is_some();
                        let descending = if ascending {
                            false
                        } else {
                            self.consume_keyword(Keyword::Desc).is_some()
                        };
                        let mut nulls_first = None;
                        // USING operator: ORDER BY col USING ~<~
                        if self.consume_keyword(Keyword::Using).is_some() {
                            // Skip the operator token(s) until comma or RParen
                            while !self.is_eof()
                                && !matches!(
                                    self.current().kind,
                                    TokenKind::Comma
                                        | TokenKind::RParen
                                        | TokenKind::Semicolon
                                        | TokenKind::Keyword(Keyword::Nulls)
                                )
                            {
                                self.advance();
                            }
                        }
                        if self.consume_keyword(Keyword::Nulls).is_some() {
                            if self.consume_keyword(Keyword::First).is_some() {
                                nulls_first = Some(true);
                            } else if self.consume_keyword(Keyword::Last).is_some() {
                                nulls_first = Some(false);
                            }
                        }
                        order_items.push((order_expr, descending, nulls_first));
                        if !self.consume_kind(&TokenKind::Comma) {
                            break;
                        }
                    }
                    if fn_lower.as_deref() == Some("array_agg")
                        && args.len() == 1
                        && order_items.len() == 1
                        && same_array_agg_order_expr(&order_items[0].0, &args[0])
                    {
                        array_agg_order_descending = Some(order_items[0].1);
                    } else if (fn_lower.as_deref() == Some("json_agg")
                        || fn_lower.as_deref() == Some("jsonb_agg"))
                        && args.len() == 1
                        && !order_items.is_empty()
                    {
                        let descending = order_items[0].1;
                        if order_items
                            .iter()
                            .all(|(_, item_desc, _)| *item_desc == descending)
                        {
                            let mut payload_args = order_items
                                .into_iter()
                                .map(|(expr, _, nulls_first)| {
                                    jsonb_agg_order_key_expr(expr, descending, nulls_first)
                                })
                                .collect::<Vec<_>>();
                            payload_args.push(args[0].clone());
                            let is_jsonb_agg = fn_lower.as_deref() == Some("jsonb_agg");
                            jsonb_agg_ordered_payload =
                                Some((is_jsonb_agg, descending, payload_args));
                        }
                    }
                }
                // Handle RETURNING clause that may follow ORDER BY
                if matches!(self.current().kind, TokenKind::Keyword(Keyword::Returning)) {
                    parse_sqljson_returning_clause(
                        self,
                        is_json_object || is_json_array,
                        &mut json_returning_type,
                    )?;
                }
                // Reject WITHIN GROUP explicitly until ordered-set aggregate
                // semantics are modeled end-to-end.
                if self.consume_keyword(Keyword::Within).is_some() {
                    return self.unsupported_feature(
                        self.previous_span(),
                        "WITHIN GROUP ordered-set aggregates are not supported",
                    );
                }
                self.expect_token(&TokenKind::RParen)?;
                break;
            }
        }

        // Parse optional FILTER (WHERE condition)
        let agg_filter = if self.consume_keyword(Keyword::Filter).is_some() {
            self.expect_token(&TokenKind::LParen)?;
            self.expect_keyword(Keyword::Where)?;
            let filter_expr = self.parse_expr()?;
            self.expect_token(&TokenKind::RParen)?;
            Some(Box::new(filter_expr))
        } else {
            None
        };

        if capture_make_interval_named_args && !make_interval_named_args.is_empty() {
            let mut slots: Vec<Option<Expr>> = vec![None; 7];
            let mut highest_index = args.len().saturating_sub(1);
            for (index, arg) in args.into_iter().enumerate() {
                slots[index] = Some(arg);
            }
            for (arg_name, arg_value, arg_span) in make_interval_named_args {
                let Some(index) = make_interval_named_arg_index(&arg_name) else {
                    return self.syntax_error(
                        arg_span,
                        format!("unknown named argument \"{arg_name}\" for make_interval"),
                    );
                };
                if slots[index].is_some() {
                    return self.syntax_error(
                        arg_span,
                        format!("argument \"{arg_name}\" specified more than once"),
                    );
                }
                slots[index] = Some(arg_value);
                highest_index = highest_index.max(index);
            }
            args = (0..=highest_index)
                .map(|index| {
                    slots[index]
                        .take()
                        .unwrap_or(Expr::Literal(Literal::Integer(0), name.span))
                })
                .collect();
        }

        if is_json_object {
            for key_expr in &json_object_keys {
                validate_sqljson_object_key(self, key_expr)?;
            }
            if json_unique_clause == JsonUniqueClause::With {
                if let Some(duplicate_key) = first_duplicate_sqljson_key(&json_object_keys) {
                    let message = if matches!(json_returning_type, Some(DataType::Jsonb)) {
                        "duplicate JSON object key value".to_owned()
                    } else {
                        format!("duplicate JSON object key value: \"{duplicate_key}\"")
                    };
                    return self.syntax_error(name.span, message);
                }
            }
        }

        let mut name = name;
        if json_array_subquery_form {
            if let Some(last) = name.parts.last_mut() {
                "__aiondb_json_array_subquery".clone_into(last);
            }
        }
        if name.parts.last().is_some_and(|part| {
            part.eq_ignore_ascii_case("normalize") || part.eq_ignore_ascii_case("is_normalized")
        }) && args.len() >= 2
        {
            if let Expr::Identifier(form_name) = &args[1] {
                if form_name.parts.len() == 1 {
                    let form = form_name.parts[0].to_ascii_uppercase();
                    if matches!(form.as_str(), "NFC" | "NFD" | "NFKC" | "NFKD") {
                        args[1] = Expr::Literal(Literal::String(form), form_name.span);
                    }
                }
            }
        }
        let mut rewrote_builtin_variadic = false;
        if saw_variadic {
            if let Some(last) = name.parts.last_mut() {
                if last.eq_ignore_ascii_case("num_nulls") {
                    *last = "__aiondb_variadic_num_nulls".to_string();
                    rewrote_builtin_variadic = true;
                } else if last.eq_ignore_ascii_case("num_nonnulls") {
                    *last = "__aiondb_variadic_num_nonnulls".to_string();
                    rewrote_builtin_variadic = true;
                } else if last.eq_ignore_ascii_case("concat") {
                    *last = if args.len() > 1 {
                        "__aiondb_variadic_concat_ws".to_string()
                    } else {
                        "__aiondb_variadic_concat".to_string()
                    };
                    rewrote_builtin_variadic = true;
                } else if last.eq_ignore_ascii_case("concat_ws") {
                    *last = "__aiondb_variadic_concat_ws".to_string();
                    rewrote_builtin_variadic = true;
                } else if last.eq_ignore_ascii_case("format") {
                    *last = "__aiondb_variadic_format".to_string();
                    rewrote_builtin_variadic = true;
                }
            }
        }
        if saw_variadic && !rewrote_builtin_variadic {
            if let Some(last_arg) = args.pop() {
                let marker_span = last_arg.span();
                args.push(Expr::FunctionCall {
                    name: ObjectName {
                        parts: vec!["__aiondb_variadic_arg".to_owned()],
                        span: marker_span,
                    },
                    args: vec![last_arg],
                    distinct: false,
                    filter: None,
                    span: marker_span,
                });
            }
        }
        if let Some(last) = name.parts.last_mut() {
            if last.eq_ignore_ascii_case("array_agg") {
                match array_agg_order_descending {
                    Some(true) => *last = "__aiondb_array_agg_ordered_desc".to_string(),
                    Some(false) => *last = "__aiondb_array_agg_ordered_asc".to_string(),
                    None => {}
                }
            } else if last.eq_ignore_ascii_case("json_agg")
                || last.eq_ignore_ascii_case("jsonb_agg")
            {
                if let Some((is_jsonb_agg, descending, payload_args)) = jsonb_agg_ordered_payload {
                    let payload_span = payload_args
                        .first()
                        .map(|expr| expr.span())
                        .unwrap_or(name.span);
                    args = vec![Expr::FunctionCall {
                        name: ObjectName {
                            parts: vec!["jsonb_build_array".to_owned()],
                            span: payload_span,
                        },
                        args: payload_args,
                        distinct: false,
                        filter: None,
                        span: payload_span,
                    }];
                    *last = if is_jsonb_agg {
                        if descending {
                            "__aiondb_jsonb_agg_ordered_desc".to_string()
                        } else {
                            "__aiondb_jsonb_agg_ordered_asc".to_string()
                        }
                    } else {
                        if descending {
                            "__aiondb_json_agg_ordered_desc".to_string()
                        } else {
                            "__aiondb_json_agg_ordered_asc".to_string()
                        }
                    };
                }
            }
        }

        let func_call = Expr::FunctionCall {
            span: name.span.merge(self.previous_span()),
            name,
            args,
            distinct: agg_distinct,
            filter: agg_filter,
        };
        // Check for OVER clause (window function)
        if self.consume_keyword(Keyword::Over).is_some() {
            return self.parse_window_spec(func_call);
        }
        Ok(func_call)
    }
}

fn is_identifier_token(kind: &TokenKind, expected: &str) -> bool {
    matches!(kind, TokenKind::Identifier(name) if name.eq_ignore_ascii_case(expected))
}

fn parse_optional_sqljson_encoding(parser: &mut Parser) -> DbResult<Option<String>> {
    if !is_identifier_token(&parser.current().kind, "encoding") {
        return Ok(None);
    }
    parser.advance(); // consume ENCODING
    let (encoding_name, _) = parser.expect_identifier()?;
    Ok(Some(encoding_name))
}

fn validate_sqljson_encoding_clause(
    parser: &mut Parser,
    encoding: Option<&str>,
    is_bytea_target: bool,
    non_bytea_message: &str,
) -> DbResult<()> {
    let Some(encoding_name) = encoding else {
        return Ok(());
    };
    let lower = encoding_name.to_ascii_lowercase();
    if !matches!(lower.as_str(), "utf8" | "utf16" | "utf32") {
        return parser.syntax_error(
            parser.current().span,
            format!("unrecognized JSON encoding: {encoding_name}"),
        );
    }
    if !is_bytea_target {
        return parser.syntax_error(parser.current().span, non_bytea_message);
    }
    if lower != "utf8" {
        return parser.syntax_error(parser.current().span, "unsupported JSON encoding");
    }
    Ok(())
}

fn is_sqljson_format_terminator(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::RParen
            | TokenKind::Comma
            | TokenKind::Semicolon
            | TokenKind::Keyword(Keyword::Returning | Keyword::Null)
    )
}

fn unwrap_type_hint_expr(expr: &Expr) -> &Expr {
    let Expr::FunctionCall { name, args, .. } = expr else {
        return expr;
    };
    if name
        .parts
        .last()
        .is_some_and(|part| part.eq_ignore_ascii_case("__aiondb_type_hint"))
    {
        return args.first().unwrap_or(expr);
    }
    expr
}

fn expr_type_hint_name(expr: &Expr) -> Option<&str> {
    let Expr::FunctionCall { name, args, .. } = expr else {
        return None;
    };
    if !name
        .parts
        .last()
        .is_some_and(|part| part.eq_ignore_ascii_case("__aiondb_type_hint"))
    {
        return None;
    }
    match args.get(1) {
        Some(Expr::Literal(Literal::String(type_name), _)) => Some(type_name.as_str()),
        _ => None,
    }
}

fn expr_declared_type(expr: &Expr) -> Option<&DataType> {
    match unwrap_type_hint_expr(expr) {
        Expr::Cast { data_type, .. } => Some(data_type),
        _ => None,
    }
}

fn expr_is_declared_bytea(expr: &Expr) -> bool {
    if expr_type_hint_name(expr).is_some_and(|hint| hint.eq_ignore_ascii_case("bytea")) {
        return true;
    }
    matches!(expr_declared_type(expr), Some(DataType::Blob))
}

fn expr_allows_format_json(expr: &Expr) -> bool {
    if expr_type_hint_name(expr).is_some_and(|hint| {
        matches!(
            hint.to_ascii_lowercase().as_str(),
            "json"
                | "jsonb"
                | "text"
                | "varchar"
                | "character varying"
                | "character"
                | "char"
                | "bytea"
        )
    }) {
        return true;
    }
    match unwrap_type_hint_expr(expr) {
        Expr::Literal(Literal::String(_) | Literal::Null, _) => true,
        Expr::Cast { data_type, .. } => {
            matches!(data_type, DataType::Text | DataType::Blob | DataType::Jsonb)
        }
        _ => true,
    }
}

fn parse_sqljson_format_clause(
    parser: &mut Parser,
    validate: bool,
    value_expr: Option<&Expr>,
) -> DbResult<()> {
    if !is_identifier_token(&parser.current().kind, "format") {
        return Ok(());
    }
    parser.advance(); // consume FORMAT
    if !is_sqljson_format_terminator(&parser.current().kind)
        && !is_identifier_token(&parser.current().kind, "encoding")
        && !parser.is_json_clause_keyword()
    {
        parser.advance(); // consume JSON/TEXT target token
    }
    let encoding = parse_optional_sqljson_encoding(parser)?;
    while !parser.is_eof()
        && !is_sqljson_format_terminator(&parser.current().kind)
        && !parser.is_json_clause_keyword()
    {
        parser.advance();
    }
    if !validate {
        return Ok(());
    }
    if let Some(expr) = value_expr {
        validate_sqljson_encoding_clause(
            parser,
            encoding.as_deref(),
            expr_is_declared_bytea(expr),
            "JSON ENCODING clause is only allowed for bytea input type",
        )?;
        if encoding.is_none() && !expr_allows_format_json(expr) {
            return parser.syntax_error(
                expr.span(),
                "cannot use non-string types with explicit FORMAT JSON clause",
            );
        }
    }
    Ok(())
}

fn parse_sqljson_returning_clause(
    parser: &mut Parser,
    validate: bool,
    json_returning_type: &mut Option<DataType>,
) -> DbResult<()> {
    parser.expect_keyword(Keyword::Returning)?;
    let returning_type = parser.parse_data_type().ok();
    if validate {
        json_returning_type.clone_from(&returning_type);
    }
    let mut encoding = None;
    if is_identifier_token(&parser.current().kind, "format") {
        parser.advance(); // consume FORMAT
        if !matches!(
            parser.current().kind,
            TokenKind::RParen | TokenKind::Comma | TokenKind::Semicolon
        ) && !is_identifier_token(&parser.current().kind, "encoding")
        {
            parser.advance(); // consume JSON/TEXT target
        }
        encoding = parse_optional_sqljson_encoding(parser)?;
    }
    while !parser.is_eof()
        && !matches!(
            parser.current().kind,
            TokenKind::RParen | TokenKind::Comma | TokenKind::Semicolon
        )
    {
        parser.advance();
    }
    if !validate {
        return Ok(());
    }
    let is_bytea = matches!(returning_type, Some(DataType::Blob));
    validate_sqljson_encoding_clause(
        parser,
        encoding.as_deref(),
        is_bytea,
        "cannot set JSON encoding for non-bytea output types",
    )?;
    Ok(())
}

fn validate_sqljson_object_key(parser: &mut Parser, key_expr: &Expr) -> DbResult<()> {
    if matches!(key_expr, Expr::Literal(Literal::Null, _)) {
        return parser.syntax_error(key_expr.span(), "null value not allowed for object key");
    }
    let hinted_json = expr_type_hint_name(key_expr).is_some_and(|hint| {
        hint.eq_ignore_ascii_case("json") || hint.eq_ignore_ascii_case("jsonb")
    });
    if hinted_json
        || matches!(expr_declared_type(key_expr), Some(DataType::Jsonb))
        || matches!(unwrap_type_hint_expr(key_expr), Expr::Array { .. })
    {
        return parser.syntax_error(
            key_expr.span(),
            "key value must be scalar, not array, composite, or json",
        );
    }
    Ok(())
}

fn normalize_sqljson_unique_key(expr: &Expr) -> Option<String> {
    let expr = unwrap_type_hint_expr(expr);
    match expr {
        Expr::Literal(Literal::Integer(value), _) => Some(value.to_string()),
        Expr::Literal(Literal::NumericLit(value), _) => Some(value.clone()),
        Expr::Literal(Literal::String(value), _) => Some(value.clone()),
        Expr::Literal(Literal::Boolean(value), _) => Some(value.to_string()),
        Expr::Cast { expr, .. } => normalize_sqljson_unique_key(expr),
        _ => None,
    }
}

fn first_duplicate_sqljson_key(keys: &[Expr]) -> Option<String> {
    let mut seen = std::collections::HashSet::new();
    for key in keys {
        let normalized = normalize_sqljson_unique_key(key)?;
        if !seen.insert(normalized.clone()) {
            return Some(normalized);
        }
    }
    None
}

fn same_array_agg_order_expr(left: &Expr, right: &Expr) -> bool {
    match (left, right) {
        (Expr::Identifier(left_name), Expr::Identifier(right_name)) => {
            left_name.parts == right_name.parts
        }
        (
            Expr::Cast {
                expr: left_inner,
                data_type: left_type,
                ..
            },
            Expr::Cast {
                expr: right_inner,
                data_type: right_type,
                ..
            },
        ) => left_type == right_type && same_array_agg_order_expr(left_inner, right_inner),
        (Expr::Literal(left_lit, _), Expr::Literal(right_lit, _)) => left_lit == right_lit,
        _ => false,
    }
}

fn jsonb_agg_order_key_expr(expr: Expr, descending: bool, nulls_first: Option<bool>) -> Expr {
    let default_nulls_first = descending;
    let nulls_first = nulls_first.unwrap_or(default_nulls_first);
    let rank_uses_is_null = nulls_first == descending;
    let span = expr.span();
    let rank_expr = Expr::IsNull {
        expr: Box::new(expr.clone()),
        negated: !rank_uses_is_null,
        span,
    };
    Expr::FunctionCall {
        name: ObjectName {
            parts: vec!["jsonb_build_array".to_owned()],
            span,
        },
        args: vec![rank_expr, expr],
        distinct: false,
        filter: None,
        span,
    }
}

fn named_function_argument_name(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Identifier(name) if name.parts.len() == 1 => name.parts.first().map(String::as_str),
        _ => None,
    }
}

fn make_interval_named_arg_index(name: &str) -> Option<usize> {
    match name.to_ascii_lowercase().as_str() {
        "years" => Some(0),
        "months" => Some(1),
        "weeks" => Some(2),
        "days" => Some(3),
        "hours" => Some(4),
        "mins" => Some(5),
        "secs" => Some(6),
        _ => None,
    }
}

fn is_aggregate_function_name(name: Option<&str>) -> bool {
    matches!(
        name,
        Some(
            "count"
                | "sum"
                | "avg"
                | "min"
                | "max"
                | "array_agg"
                | "string_agg"
                | "json_agg"
                | "jsonb_agg"
                | "json_object_agg"
                | "jsonb_object_agg"
                | "bool_and"
                | "bool_or"
                | "every"
                | "bit_and"
                | "bit_or"
                | "bit_xor"
                | "stddev"
                | "stddev_pop"
                | "stddev_samp"
                | "variance"
                | "var_pop"
                | "var_samp"
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
                | "percentile_cont"
                | "percentile_disc"
                | "mode"
        )
    )
}
