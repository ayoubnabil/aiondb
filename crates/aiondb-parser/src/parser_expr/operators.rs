use super::*;

enum QualifiedComparisonOperator {
    Binary(BinaryOperator),
    Function(&'static str),
}

impl Parser {
    pub(super) fn parse_or_expr(&mut self) -> DbResult<Expr> {
        let mut expr = self.parse_xor_expr()?;

        while self.consume_keyword(Keyword::Or).is_some() {
            let right = self.parse_xor_expr()?;
            let span = expr.span().merge(right.span());
            expr = Expr::BinaryOp {
                left: Box::new(expr),
                op: BinaryOperator::Or,
                right: Box::new(right),
                span,
            };
        }

        Ok(expr)
    }

    /// Parse Cypher XOR expressions (precedence between OR and AND).
    ///
    /// In SQL mode this is a pass-through to `parse_and_expr`.
    /// In Cypher mode, `a XOR b` is desugared to `(a OR b) AND NOT (a AND b)`.
    fn parse_xor_expr(&mut self) -> DbResult<Expr> {
        let mut expr = self.parse_and_expr()?;

        while self.cypher_mode
            && matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("xor"))
        {
            self.advance(); // consume XOR
            let right = self.parse_and_expr()?;
            let span = expr.span().merge(right.span());
            // Desugar: a XOR b  →  (a OR b) AND NOT (a AND b)
            let or_part = Expr::BinaryOp {
                left: Box::new(expr.clone()),
                op: BinaryOperator::Or,
                right: Box::new(right.clone()),
                span,
            };
            let and_part = Expr::BinaryOp {
                left: Box::new(expr),
                op: BinaryOperator::And,
                right: Box::new(right),
                span,
            };
            let not_and = Expr::UnaryOp {
                op: UnaryOperator::Not,
                expr: Box::new(and_part),
                span,
            };
            expr = Expr::BinaryOp {
                left: Box::new(or_part),
                op: BinaryOperator::And,
                right: Box::new(not_and),
                span,
            };
        }

        Ok(expr)
    }

    fn parse_and_expr(&mut self) -> DbResult<Expr> {
        let mut expr = self.parse_not_expr()?;

        while self.consume_keyword(Keyword::And).is_some() {
            let right = self.parse_not_expr()?;
            let span = expr.span().merge(right.span());
            expr = Expr::BinaryOp {
                left: Box::new(expr),
                op: BinaryOperator::And,
                right: Box::new(right),
                span,
            };
        }

        Ok(expr)
    }

    fn parse_not_expr(&mut self) -> DbResult<Expr> {
        if let Some(token) = self.consume_keyword(Keyword::Not) {
            // NOT EXISTS (...) → Exists { negated: true }
            if matches!(self.current().kind, TokenKind::Keyword(Keyword::Exists)) {
                self.advance(); // consume EXISTS
                if self.cypher_mode && self.consume_kind(&TokenKind::LBrace) {
                    let query = self.parse_cypher_statement()?;
                    let rbrace = self.expect_token(&TokenKind::RBrace)?;
                    let span = token.span.merge(rbrace.span);
                    return Ok(Expr::CypherExists {
                        query: Box::new(query),
                        negated: true,
                        span,
                    });
                }
                self.expect_token(&TokenKind::LParen)?;
                let query = self.parse_subquery_select()?;
                let rparen = self.expect_token(&TokenKind::RParen)?;
                let span = token.span.merge(rparen.span);
                return Ok(Expr::Exists {
                    query: Box::new(query),
                    negated: true,
                    span,
                });
            }
            self.enter_expr_recursion()?;
            let expr = self.parse_not_expr();
            self.leave_expr_recursion();
            let expr = expr?;
            let span = token.span.merge(expr.span());
            return Ok(Expr::UnaryOp {
                op: UnaryOperator::Not,
                expr: Box::new(expr),
                span,
            });
        }

        self.parse_comparison_expr()
    }

    fn parse_comparison_expr(&mut self) -> DbResult<Expr> {
        let mut expr = self.parse_concat_expr()?;

        // Cypher binds `IS [NOT] NULL` and `IN [list]` tighter than the
        // comparison operators (`= <> < ...`), so `false = true IS NULL`
        // parses as `false = (true IS NULL)`. PG's grammar puts those at
        // the comparison level, so only re-bind in cypher_mode.
        if self.cypher_mode {
            expr = self.maybe_consume_cypher_postfix_predicate(expr)?;
        }

        // PostgreSQL postfix predicates: `expr ISNULL` / `expr NOTNULL`.
        if let TokenKind::Identifier(identifier) = &self.current().kind {
            let normalized = identifier.to_ascii_lowercase();
            if normalized == "isnull" || normalized == "notnull" {
                let token = self.advance();
                let span = expr.span().merge(token.span);
                return Ok(Expr::IsNull {
                    expr: Box::new(expr),
                    negated: normalized == "notnull",
                    span,
                });
            }
        }

        // IS [NOT] NULL / IS [NOT] DISTINCT FROM / IS [NOT] TRUE/FALSE/UNKNOWN
        if self.consume_keyword(Keyword::Is).is_some() {
            let negated = self.consume_keyword(Keyword::Not).is_some();
            if self.consume_keyword(Keyword::Distinct).is_some() {
                self.expect_keyword(Keyword::From)?;
                let right = self.parse_concat_expr()?;
                let span = expr.span().merge(right.span());
                return Ok(Expr::IsDistinctFrom {
                    left: Box::new(expr),
                    right: Box::new(right),
                    negated,
                    span,
                });
            }
            // IS [NOT] TRUE/FALSE use DISTINCT semantics so NULL evaluates
            // like PostgreSQL instead of propagating through regular equality.
            if let Some(token) = self.consume_keyword(Keyword::True) {
                let span = expr.span().merge(token.span);
                return Ok(Expr::IsDistinctFrom {
                    left: Box::new(expr),
                    right: Box::new(Expr::Literal(Literal::Boolean(true), token.span)),
                    negated: !negated,
                    span,
                });
            }
            if let Some(token) = self.consume_keyword(Keyword::False) {
                let span = expr.span().merge(token.span);
                return Ok(Expr::IsDistinctFrom {
                    left: Box::new(expr),
                    right: Box::new(Expr::Literal(Literal::Boolean(false), token.span)),
                    negated: !negated,
                    span,
                });
            }
            // IS [NOT] UNKNOWN -> IS [NOT] NULL
            if matches!(self.current().kind, TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("unknown"))
            {
                let token = self.advance();
                let span = expr.span().merge(token.span);
                return Ok(Expr::IsNull {
                    expr: Box::new(expr),
                    negated,
                    span,
                });
            }
            // IS [NOT] JSON [VALUE|OBJECT|ARRAY|SCALAR] [WITH|WITHOUT UNIQUE [KEYS]]
            if matches!(self.current().kind, TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("json"))
            {
                let json_token = self.advance();
                let mut tail_span = json_token.span;
                let mut json_kind = "JSON".to_owned();
                let mut json_kind_span = json_token.span;
                match &self.current().kind {
                    TokenKind::Keyword(Keyword::Value) => {
                        let token = self.advance();
                        "VALUE".clone_into(&mut json_kind);
                        json_kind_span = token.span;
                        tail_span = tail_span.merge(token.span);
                    }
                    TokenKind::Keyword(Keyword::Array) => {
                        let token = self.advance();
                        "ARRAY".clone_into(&mut json_kind);
                        json_kind_span = token.span;
                        tail_span = tail_span.merge(token.span);
                    }
                    TokenKind::Identifier(name) if name.eq_ignore_ascii_case("object") => {
                        let token = self.advance();
                        "OBJECT".clone_into(&mut json_kind);
                        json_kind_span = token.span;
                        tail_span = tail_span.merge(token.span);
                    }
                    TokenKind::Identifier(name) if name.eq_ignore_ascii_case("scalar") => {
                        let token = self.advance();
                        "SCALAR".clone_into(&mut json_kind);
                        json_kind_span = token.span;
                        tail_span = tail_span.merge(token.span);
                    }
                    _ => {}
                }

                let mut unique_mode = "DEFAULT".to_owned();
                let mut unique_mode_span = tail_span;
                if let Some(with_token) = self.consume_keyword(Keyword::With) {
                    if !Self::is_unique_token(&self.current().kind) {
                        return self.syntax_error_current("expected UNIQUE");
                    }
                    let unique_token = self.advance();
                    tail_span = tail_span.merge(unique_token.span);
                    if Self::is_keys_token(&self.current().kind) {
                        let keys_token = self.advance();
                        tail_span = tail_span.merge(keys_token.span);
                    }
                    "WITH".clone_into(&mut unique_mode);
                    unique_mode_span = with_token.span.merge(unique_token.span);
                } else if let Some(without_token) = self.consume_keyword(Keyword::Without) {
                    if !Self::is_unique_token(&self.current().kind) {
                        return self.syntax_error_current("expected UNIQUE");
                    }
                    let unique_token = self.advance();
                    tail_span = tail_span.merge(unique_token.span);
                    if Self::is_keys_token(&self.current().kind) {
                        let keys_token = self.advance();
                        tail_span = tail_span.merge(keys_token.span);
                    }
                    "WITHOUT".clone_into(&mut unique_mode);
                    unique_mode_span = without_token.span.merge(unique_token.span);
                }

                let span = expr.span().merge(tail_span);
                let call = Expr::FunctionCall {
                    name: ObjectName {
                        parts: vec!["__aiondb_is_json".to_owned()],
                        span,
                    },
                    args: vec![
                        expr.clone(),
                        Expr::Literal(Literal::String(json_kind), json_kind_span),
                        Expr::Literal(Literal::String(unique_mode), unique_mode_span),
                    ],
                    distinct: false,
                    filter: None,
                    span,
                };
                if negated {
                    return Ok(Expr::UnaryOp {
                        op: UnaryOperator::Not,
                        expr: Box::new(call),
                        span,
                    });
                }
                return Ok(call);
            }
            // IS [NOT] DOCUMENT - XML well-formedness check.
            if matches!(self.current().kind, TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("document"))
            {
                let doc_span = self.current().span;
                return self.unsupported_feature(
                    doc_span,
                    "IS [NOT] DOCUMENT predicate is not yet supported",
                );
            }
            // IS [NOT] [NFC|NFD|NFKC|NFKD] NORMALIZED - Unicode normalization check.
            if matches!(self.current().kind, TokenKind::Identifier(ref s)
                if matches!(s.to_ascii_uppercase().as_str(), "NORMALIZED" | "NFC" | "NFD" | "NFKC" | "NFKD"))
            {
                let first = self.advance();
                let first_name = match &first.kind {
                    TokenKind::Identifier(name) => name.to_ascii_uppercase(),
                    _ => {
                        return self.syntax_error(
                            first.span,
                            "expected NORMALIZED or Unicode normalization identifier",
                        );
                    }
                };

                let mut args = vec![expr.clone()];
                let end_span = if first_name == "NORMALIZED" {
                    first.span
                } else {
                    let (name, span) = self.expect_identifier()?;
                    if !name.eq_ignore_ascii_case("normalized") {
                        return Err(self.unexpected_token("NORMALIZED"));
                    }
                    args.push(Expr::Literal(Literal::String(first_name), first.span));
                    span
                };

                let call = Expr::FunctionCall {
                    name: ObjectName {
                        parts: vec!["is_normalized".to_owned()],
                        span: expr.span().merge(end_span),
                    },
                    args,
                    distinct: false,
                    filter: None,
                    span: expr.span().merge(end_span),
                };

                if negated {
                    return Ok(Expr::UnaryOp {
                        op: UnaryOperator::Not,
                        expr: Box::new(call),
                        span: expr.span().merge(end_span),
                    });
                }

                return Ok(call);
            }
            let null_token = self.expect_keyword(Keyword::Null)?;
            let span = expr.span().merge(null_token.span);
            return Ok(Expr::IsNull {
                expr: Box::new(expr),
                negated,
                span,
            });
        }

        // NOT LIKE / NOT ILIKE / NOT IN / NOT BETWEEN
        if matches!(self.current().kind, TokenKind::Keyword(Keyword::Not)) {
            // Peek ahead to see if it's NOT LIKE, NOT ILIKE, NOT IN, or NOT BETWEEN
            if self.index + 1 < self.tokens.len() {
                match &self.tokens[self.index + 1].kind {
                    TokenKind::Keyword(Keyword::Like) => {
                        self.advance(); // consume NOT
                        self.advance(); // consume LIKE
                        let pattern = self.parse_concat_expr()?;
                        // Skip optional ESCAPE 'char'
                        self.consume_escape_clause()?;
                        let span = expr.span().merge(self.previous_span());
                        return Ok(Expr::Like {
                            expr: Box::new(expr),
                            pattern: Box::new(pattern),
                            negated: true,
                            case_insensitive: false,
                            span,
                        });
                    }
                    TokenKind::Keyword(Keyword::Ilike) => {
                        self.advance(); // consume NOT
                        self.advance(); // consume ILIKE
                        let pattern = self.parse_concat_expr()?;
                        // Skip optional ESCAPE 'char'
                        self.consume_escape_clause()?;
                        let span = expr.span().merge(self.previous_span());
                        return Ok(Expr::Like {
                            expr: Box::new(expr),
                            pattern: Box::new(pattern),
                            negated: true,
                            case_insensitive: true,
                            span,
                        });
                    }
                    TokenKind::Keyword(Keyword::In) => {
                        self.advance(); // consume NOT
                        self.advance(); // consume IN
                        return self.parse_in_list(expr, true);
                    }
                    TokenKind::Keyword(Keyword::Between) => {
                        self.advance(); // consume NOT
                        self.advance(); // consume BETWEEN
                        return self.parse_between(expr, true);
                    }
                    // NOT SIMILAR TO
                    TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("similar") => {
                        self.advance(); // consume NOT
                        self.advance(); // consume SIMILAR
                        let _ = self.consume_keyword(Keyword::To); // consume TO
                        let pattern = self.parse_concat_expr()?;
                        self.consume_escape_clause()?;
                        let span = expr.span().merge(self.previous_span());
                        return Ok(Expr::Like {
                            expr: Box::new(expr),
                            pattern: Box::new(pattern),
                            negated: true,
                            case_insensitive: false,
                            span,
                        });
                    }
                    _ => {}
                }
            }
        }

        // LIKE
        if self.consume_keyword(Keyword::Like).is_some() {
            let pattern = self.parse_concat_expr()?;
            // Skip optional ESCAPE 'char'
            self.consume_escape_clause()?;
            let span = expr.span().merge(self.previous_span());
            return Ok(Expr::Like {
                expr: Box::new(expr),
                pattern: Box::new(pattern),
                negated: false,
                case_insensitive: false,
                span,
            });
        }

        // ILIKE
        if self.consume_keyword(Keyword::Ilike).is_some() {
            let pattern = self.parse_concat_expr()?;
            // Skip optional ESCAPE 'char'
            self.consume_escape_clause()?;
            let span = expr.span().merge(self.previous_span());
            return Ok(Expr::Like {
                expr: Box::new(expr),
                pattern: Box::new(pattern),
                negated: false,
                case_insensitive: true,
                span,
            });
        }

        // SIMILAR TO
        if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("similar"))
            && self.index + 1 < self.tokens.len()
            && matches!(
                self.tokens[self.index + 1].kind,
                TokenKind::Keyword(Keyword::To)
            )
        {
            self.advance(); // consume SIMILAR
            self.advance(); // consume TO
            let pattern = self.parse_concat_expr()?;
            self.consume_escape_clause()?;
            let span = expr.span().merge(self.previous_span());
            return Ok(Expr::Like {
                expr: Box::new(expr),
                pattern: Box::new(pattern),
                negated: false,
                case_insensitive: false,
                span,
            });
        }

        // IN
        if self.consume_keyword(Keyword::In).is_some() {
            // Cypher: IN [list] without parentheses
            if self.cypher_mode && matches!(self.current().kind, TokenKind::LBracket) {
                let list = self.parse_expr()?;
                let span = expr.span().merge(list.span());
                return Ok(Expr::FunctionCall {
                    name: ObjectName {
                        parts: vec!["__cypher_in".to_owned()],
                        span,
                    },
                    args: vec![expr, list],
                    distinct: false,
                    filter: None,
                    span,
                });
            }
            return self.parse_in_list(expr, false);
        }

        // BETWEEN
        if self.consume_keyword(Keyword::Between).is_some() {
            return self.parse_between(expr, false);
        }

        // Cypher string predicates: STARTS WITH, ENDS WITH, CONTAINS
        if self.cypher_mode {
            if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("starts"))
                && self.index + 1 < self.tokens.len()
                && matches!(
                    self.tokens[self.index + 1].kind,
                    TokenKind::Keyword(Keyword::With)
                )
            {
                self.advance();
                self.advance();
                let rhs = self.parse_concat_expr()?;
                let span = expr.span().merge(rhs.span());
                return Ok(Expr::FunctionCall {
                    name: ObjectName {
                        parts: vec!["__cypher_starts_with".to_owned()],
                        span,
                    },
                    args: vec![expr, rhs],
                    distinct: false,
                    filter: None,
                    span,
                });
            }
            if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("ends"))
                && self.index + 1 < self.tokens.len()
                && matches!(
                    self.tokens[self.index + 1].kind,
                    TokenKind::Keyword(Keyword::With)
                )
            {
                self.advance();
                self.advance();
                let rhs = self.parse_concat_expr()?;
                let span = expr.span().merge(rhs.span());
                return Ok(Expr::FunctionCall {
                    name: ObjectName {
                        parts: vec!["__cypher_ends_with".to_owned()],
                        span,
                    },
                    args: vec![expr, rhs],
                    distinct: false,
                    filter: None,
                    span,
                });
            }
            if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("contains"))
            {
                self.advance();
                let rhs = self.parse_concat_expr()?;
                let span = expr.span().merge(rhs.span());
                return Ok(Expr::FunctionCall {
                    name: ObjectName {
                        parts: vec!["__cypher_contains".to_owned()],
                        span,
                    },
                    args: vec![expr, rhs],
                    distinct: false,
                    filter: None,
                    span,
                });
            }
        }

        // OVERLAPS - binary operator on row pairs: (a,b) OVERLAPS (c,d)
        if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("overlaps"))
        {
            self.advance(); // consume OVERLAPS
            let rhs = self.parse_concat_expr()?;
            let span = expr.span().merge(rhs.span());
            return Ok(Expr::FunctionCall {
                name: ObjectName {
                    parts: vec!["overlaps".to_owned()],
                    span,
                },
                args: vec![expr, rhs],
                distinct: false,
                filter: None,
                span,
            });
        }

        // Range/multirange custom operators that the lexer classifies as
        // `CustomOp`. Preserve their semantics by rewriting to explicit
        // function calls instead of collapsing into a generic geometric op.
        if matches!(self.current().kind, TokenKind::CustomOp) {
            let token = self.current().clone();
            let op_text = self
                .source
                .get(token.span.start..token.span.end)
                .unwrap_or_default()
                .trim();
            let fn_name = match op_text {
                "&<" => Some("range_not_extend_right"),
                "&>" => Some("range_not_extend_left"),
                "-|-" => Some("range_adjacent"),
                // inet/cidr containment operators - dispatched to scalar
                // functions because they don't fit the BinaryOperator enum.
                "<<=" => Some("inet_subnet_contained_by_or_equals"),
                ">>=" => Some("inet_subnet_contains_or_equals"),
                _ => None,
            };
            if let Some(fn_name) = fn_name {
                self.advance(); // consume custom operator
                let rhs = self.parse_concat_expr()?;
                let span = expr.span().merge(rhs.span());
                return Ok(Expr::FunctionCall {
                    name: ObjectName {
                        parts: vec![fn_name.to_owned()],
                        span,
                    },
                    args: vec![expr, rhs],
                    distinct: false,
                    filter: None,
                    span,
                });
            }
        }

        if self.current().kind == TokenKind::Keyword(Keyword::Operator) {
            return self.parse_qualified_operator_expr(expr);
        }

        // Standard comparison operators (including JSONB containment and key existence)
        let operator = match self.current().kind {
            TokenKind::Eq => Some(BinaryOperator::Eq),
            TokenKind::Gt => Some(BinaryOperator::Gt),
            TokenKind::Ge => Some(BinaryOperator::Ge),
            TokenKind::Lt => Some(BinaryOperator::Lt),
            TokenKind::Le => Some(BinaryOperator::Le),
            TokenKind::Ne => Some(BinaryOperator::Ne),
            TokenKind::Tilde => Some(BinaryOperator::RegexMatch),
            TokenKind::TildeStar => Some(BinaryOperator::RegexMatchInsensitive),
            TokenKind::ExclTilde => Some(BinaryOperator::NotRegexMatch),
            TokenKind::ExclTildeStar => Some(BinaryOperator::NotRegexMatchInsensitive),
            TokenKind::AtGt => Some(BinaryOperator::JsonContains),
            TokenKind::LtAt => Some(BinaryOperator::JsonContainedBy),
            TokenKind::Question => Some(BinaryOperator::JsonKeyExists),
            TokenKind::QuestionPipe => Some(BinaryOperator::JsonAnyKeyExists),
            TokenKind::QuestionAmpersand => Some(BinaryOperator::JsonAllKeysExist),
            TokenKind::AmpAmp => Some(BinaryOperator::ArrayOverlap),
            TokenKind::AtAt => Some(BinaryOperator::FullTextSearch),
            TokenKind::AtQuestion => Some(BinaryOperator::JsonPathExists),
            TokenKind::TildeEq => Some(BinaryOperator::GeometricEq),
            TokenKind::CustomOp => Some(BinaryOperator::GeometricEq),
            TokenKind::VecL2 => Some(BinaryOperator::VectorL2Distance),
            TokenKind::VecCosine => Some(BinaryOperator::VectorCosineDistance),
            TokenKind::VecNegInner => Some(BinaryOperator::VectorNegativeInnerProduct),
            TokenKind::VecL1 => Some(BinaryOperator::VectorL1Distance),
            TokenKind::VecHamming => Some(BinaryOperator::VectorHammingDistance),
            TokenKind::VecJaccard => Some(BinaryOperator::VectorJaccardDistance),
            _ => None,
        };

        if let Some(operator) = operator {
            self.advance();
            let mut right = self.parse_concat_expr()?;
            if self.cypher_mode {
                right = self.maybe_consume_cypher_postfix_predicate(right)?;
            }
            let span = expr.span().merge(right.span());
            let mut chained = Expr::BinaryOp {
                left: Box::new(expr),
                op: operator,
                right: Box::new(right.clone()),
                span,
            };
            // Cypher supports chained comparisons (`a < b < c` ≡ `a < b AND b < c`).
            // Greedily consume more comparison operators in cypher_mode, AND-ing
            // each pair-wise step into the result.
            if self.cypher_mode {
                let mut left_for_next = right;
                loop {
                    let next_op = match self.current().kind {
                        TokenKind::Eq => Some(BinaryOperator::Eq),
                        TokenKind::Gt => Some(BinaryOperator::Gt),
                        TokenKind::Ge => Some(BinaryOperator::Ge),
                        TokenKind::Lt => Some(BinaryOperator::Lt),
                        TokenKind::Le => Some(BinaryOperator::Le),
                        TokenKind::Ne => Some(BinaryOperator::Ne),
                        _ => None,
                    };
                    let Some(next_op) = next_op else { break };
                    self.advance();
                    let mut nr = self.parse_concat_expr()?;
                    nr = self.maybe_consume_cypher_postfix_predicate(nr)?;
                    let step_span = left_for_next.span().merge(nr.span());
                    let step = Expr::BinaryOp {
                        left: Box::new(left_for_next.clone()),
                        op: next_op,
                        right: Box::new(nr.clone()),
                        span: step_span,
                    };
                    let combined_span = chained.span().merge(step.span());
                    chained = Expr::BinaryOp {
                        left: Box::new(chained),
                        op: BinaryOperator::And,
                        right: Box::new(step),
                        span: combined_span,
                    };
                    left_for_next = nr;
                }
            }
            return Ok(chained);
        }

        Ok(expr)
    }

    fn parse_qualified_operator_expr(&mut self, left: Expr) -> DbResult<Expr> {
        self.expect_keyword(Keyword::Operator)?;
        self.expect_token(&TokenKind::LParen)?;

        while self.index + 1 < self.tokens.len()
            && matches!(
                self.current().kind,
                TokenKind::Identifier(_) | TokenKind::Keyword(_)
            )
            && self.tokens[self.index + 1].kind == TokenKind::Dot
        {
            self.advance();
            self.advance();
        }

        let Some(operator) = self.resolve_qualified_comparison_operator() else {
            return self.syntax_error_current("expected operator name");
        };
        let operator = operator?;
        self.expect_token(&TokenKind::RParen)?;

        let mut right = self.parse_concat_expr()?;
        if self.cypher_mode {
            right = self.maybe_consume_cypher_postfix_predicate(right)?;
        }
        let span = left.span().merge(right.span());

        Ok(match operator {
            QualifiedComparisonOperator::Binary(op) => Expr::BinaryOp {
                left: Box::new(left),
                op,
                right: Box::new(right),
                span,
            },
            QualifiedComparisonOperator::Function(name) => Expr::FunctionCall {
                name: ObjectName {
                    parts: vec![name.to_owned()],
                    span,
                },
                args: vec![left, right],
                distinct: false,
                filter: None,
                span,
            },
        })
    }

    fn resolve_qualified_comparison_operator(
        &mut self,
    ) -> Option<DbResult<QualifiedComparisonOperator>> {
        let token = self.current().clone();
        let operator = match token.kind {
            TokenKind::Eq => Some(QualifiedComparisonOperator::Binary(BinaryOperator::Eq)),
            TokenKind::Gt => Some(QualifiedComparisonOperator::Binary(BinaryOperator::Gt)),
            TokenKind::Ge => Some(QualifiedComparisonOperator::Binary(BinaryOperator::Ge)),
            TokenKind::Lt => Some(QualifiedComparisonOperator::Binary(BinaryOperator::Lt)),
            TokenKind::Le => Some(QualifiedComparisonOperator::Binary(BinaryOperator::Le)),
            TokenKind::Ne => Some(QualifiedComparisonOperator::Binary(BinaryOperator::Ne)),
            TokenKind::Tilde => Some(QualifiedComparisonOperator::Binary(
                BinaryOperator::RegexMatch,
            )),
            TokenKind::TildeStar => Some(QualifiedComparisonOperator::Binary(
                BinaryOperator::RegexMatchInsensitive,
            )),
            TokenKind::ExclTilde => Some(QualifiedComparisonOperator::Binary(
                BinaryOperator::NotRegexMatch,
            )),
            TokenKind::ExclTildeStar => Some(QualifiedComparisonOperator::Binary(
                BinaryOperator::NotRegexMatchInsensitive,
            )),
            TokenKind::AtGt => Some(QualifiedComparisonOperator::Binary(
                BinaryOperator::JsonContains,
            )),
            TokenKind::LtAt => Some(QualifiedComparisonOperator::Binary(
                BinaryOperator::JsonContainedBy,
            )),
            TokenKind::Question => Some(QualifiedComparisonOperator::Binary(
                BinaryOperator::JsonKeyExists,
            )),
            TokenKind::QuestionPipe => Some(QualifiedComparisonOperator::Binary(
                BinaryOperator::JsonAnyKeyExists,
            )),
            TokenKind::QuestionAmpersand => Some(QualifiedComparisonOperator::Binary(
                BinaryOperator::JsonAllKeysExist,
            )),
            TokenKind::AmpAmp => Some(QualifiedComparisonOperator::Binary(
                BinaryOperator::ArrayOverlap,
            )),
            TokenKind::AtAt => Some(QualifiedComparisonOperator::Binary(
                BinaryOperator::FullTextSearch,
            )),
            TokenKind::AtQuestion => Some(QualifiedComparisonOperator::Binary(
                BinaryOperator::JsonPathExists,
            )),
            TokenKind::TildeEq => Some(QualifiedComparisonOperator::Binary(
                BinaryOperator::GeometricEq,
            )),
            TokenKind::VecL2 => Some(QualifiedComparisonOperator::Binary(
                BinaryOperator::VectorL2Distance,
            )),
            TokenKind::VecCosine => Some(QualifiedComparisonOperator::Binary(
                BinaryOperator::VectorCosineDistance,
            )),
            TokenKind::VecNegInner => Some(QualifiedComparisonOperator::Binary(
                BinaryOperator::VectorNegativeInnerProduct,
            )),
            TokenKind::VecL1 => Some(QualifiedComparisonOperator::Binary(
                BinaryOperator::VectorL1Distance,
            )),
            TokenKind::VecHamming => Some(QualifiedComparisonOperator::Binary(
                BinaryOperator::VectorHammingDistance,
            )),
            TokenKind::VecJaccard => Some(QualifiedComparisonOperator::Binary(
                BinaryOperator::VectorJaccardDistance,
            )),
            TokenKind::CustomOp => {
                let op_text = self
                    .source
                    .get(token.span.start..token.span.end)
                    .unwrap_or_default()
                    .trim();
                match op_text {
                    "&<" => Some(QualifiedComparisonOperator::Function(
                        "range_not_extend_right",
                    )),
                    "&>" => Some(QualifiedComparisonOperator::Function(
                        "range_not_extend_left",
                    )),
                    "-|-" => Some(QualifiedComparisonOperator::Function("range_adjacent")),
                    "<<=" => Some(QualifiedComparisonOperator::Function(
                        "inet_subnet_contained_by_or_equals",
                    )),
                    ">>=" => Some(QualifiedComparisonOperator::Function(
                        "inet_subnet_contains_or_equals",
                    )),
                    _ => Some(QualifiedComparisonOperator::Binary(
                        BinaryOperator::GeometricEq,
                    )),
                }
            }
            _ => None,
        }?;

        self.advance();
        Some(Ok(operator))
    }

    fn parse_in_list(&mut self, expr: Expr, negated: bool) -> DbResult<Expr> {
        self.expect_token(&TokenKind::LParen)?;
        if self.consume_kind(&TokenKind::RParen) {
            let span = expr.span().merge(self.previous_span());
            return Ok(Expr::InList {
                expr: Box::new(expr),
                list: Vec::new(),
                negated,
                span,
            });
        }
        // Check for IN (SELECT ...) subquery, including set operations
        if matches!(self.current().kind, TokenKind::Keyword(Keyword::Select)) {
            let query = self.parse_subquery_select()?;
            let rparen = self.expect_token(&TokenKind::RParen)?;
            let span = expr.span().merge(rparen.span);
            return Ok(Expr::InSubquery {
                expr: Box::new(expr),
                query: Box::new(query),
                negated,
                span,
            });
        }
        const MAX_IN_LIST_SIZE: usize = 65_535;
        let mut list = Vec::new();
        loop {
            if list.len() >= MAX_IN_LIST_SIZE {
                return Err(aiondb_core::DbError::syntax_error(format!(
                    "IN list exceeds maximum size of {MAX_IN_LIST_SIZE} elements"
                )));
            }
            list.push(self.parse_expr()?);
            if !self.consume_kind(&TokenKind::Comma) {
                break;
            }
        }
        let rparen = self.expect_token(&TokenKind::RParen)?;
        let span = expr.span().merge(rparen.span);
        Ok(Expr::InList {
            expr: Box::new(expr),
            list,
            negated,
            span,
        })
    }

    fn parse_between(&mut self, expr: Expr, negated: bool) -> DbResult<Expr> {
        // BETWEEN SYMMETRIC low AND high reorders bounds at runtime: it
        // is equivalent to BETWEEN least(low,high) AND greatest(low,high).
        // Wrap the parsed bounds in least/greatest calls so the evaluator
        // receives correct ordering without having to plumb a SYMMETRIC
        // flag through the AST or the executor.
        let mut symmetric = false;
        if matches!(&self.current().kind, TokenKind::Identifier(s)
            if s.eq_ignore_ascii_case("symmetric") || s.eq_ignore_ascii_case("asymmetric"))
        {
            if let TokenKind::Identifier(s) = &self.current().kind {
                symmetric = s.eq_ignore_ascii_case("symmetric");
            }
            self.advance();
        }
        let low = self.parse_concat_expr()?;
        self.expect_keyword(Keyword::And)?;
        let high = self.parse_concat_expr()?;
        let span = expr.span().merge(high.span());
        let (final_low, final_high) = if symmetric {
            let low_span = low.span();
            let high_span = high.span();
            let bounds_span = low_span.merge(high_span);
            let least_call = Expr::FunctionCall {
                span: bounds_span,
                name: ObjectName {
                    parts: vec!["least".to_owned()],
                    span: bounds_span,
                },
                args: vec![low.clone(), high.clone()],
                distinct: false,
                filter: None,
            };
            let greatest_call = Expr::FunctionCall {
                span: bounds_span,
                name: ObjectName {
                    parts: vec!["greatest".to_owned()],
                    span: bounds_span,
                },
                args: vec![low, high],
                distinct: false,
                filter: None,
            };
            (least_call, greatest_call)
        } else {
            (low, high)
        };
        Ok(Expr::Between {
            expr: Box::new(expr),
            low: Box::new(final_low),
            high: Box::new(final_high),
            negated,
            span,
        })
    }

    pub(super) fn parse_additive_expr(&mut self) -> DbResult<Expr> {
        let mut expr = self.parse_bitwise_expr()?;

        loop {
            let operator = match self.current().kind {
                TokenKind::Plus => Some(BinaryOperator::Add),
                TokenKind::Minus => Some(BinaryOperator::Sub),
                TokenKind::ShiftLeft => Some(BinaryOperator::ShiftLeft),
                TokenKind::ShiftRight => Some(BinaryOperator::ShiftRight),
                _ => None,
            };
            let Some(operator) = operator else {
                break;
            };

            self.advance();
            let right = self.parse_bitwise_expr()?;
            let span = expr.span().merge(right.span());
            expr = Expr::BinaryOp {
                left: Box::new(expr),
                op: operator,
                right: Box::new(right),
                span,
            };
        }

        Ok(expr)
    }

    fn parse_bitwise_expr(&mut self) -> DbResult<Expr> {
        let mut expr = self.parse_json_access_expr()?;

        loop {
            let operator = match self.current().kind {
                TokenKind::Ampersand => Some(BinaryOperator::BitwiseAnd),
                // Cypher uses `|` as the projection separator inside list
                // comprehensions (`[ y IN xs WHERE pred | proj ]`), so
                // bitwise-or only kicks in for SQL-mode parsing.
                TokenKind::Pipe if !self.cypher_mode => Some(BinaryOperator::BitwiseOr),
                TokenKind::Hash => Some(BinaryOperator::BitwiseXor),
                _ => None,
            };
            let Some(operator) = operator else {
                break;
            };

            self.advance();
            let right = self.parse_json_access_expr()?;
            let span = expr.span().merge(right.span());
            expr = Expr::BinaryOp {
                left: Box::new(expr),
                op: operator,
                right: Box::new(right),
                span,
            };
        }

        Ok(expr)
    }

    /// Cypher-only postfix-predicate hook applied tighter than `=`/`<`/...
    /// Rewrites `expr IS [NOT] NULL` and (in the future) `expr IN list` into
    /// the same AST a comparison would produce, but anchored on the inner
    /// operand instead of the surrounding binary expression.
    fn maybe_consume_cypher_postfix_predicate(&mut self, mut expr: Expr) -> DbResult<Expr> {
        loop {
            // `IS [NOT] NULL` - anchored on the inner operand.
            if matches!(self.current().kind, TokenKind::Keyword(Keyword::Is)) {
                let mut probe = self.index + 1;
                if matches!(
                    self.tokens.get(probe).map(|t| &t.kind),
                    Some(TokenKind::Keyword(Keyword::Not))
                ) {
                    probe += 1;
                }
                let null_follows = matches!(
                    self.tokens.get(probe).map(|t| &t.kind),
                    Some(TokenKind::Keyword(Keyword::Null))
                );
                if null_follows {
                    self.advance();
                    let negated = self.consume_keyword(Keyword::Not).is_some();
                    let null_token = self.expect_keyword(Keyword::Null)?;
                    let span = expr.span().merge(null_token.span);
                    expr = Expr::IsNull {
                        expr: Box::new(expr),
                        negated,
                        span,
                    };
                    continue;
                }
            }
            // Cypher `expr IN list` - only the list-bracket form binds
            // tighter than comparison; PG `IN (subquery)` stays at the
            // comparison level for compatibility.
            if matches!(self.current().kind, TokenKind::Keyword(Keyword::In))
                && matches!(
                    self.tokens.get(self.index + 1).map(|t| &t.kind),
                    Some(TokenKind::LBracket)
                )
            {
                self.advance(); // consume IN
                let list = self.parse_expr()?;
                let span = expr.span().merge(list.span());
                expr = Expr::FunctionCall {
                    name: ObjectName {
                        parts: vec!["__cypher_in".to_owned()],
                        span,
                    },
                    args: vec![expr, list],
                    distinct: false,
                    filter: None,
                    span,
                };
                continue;
            }
            break;
        }
        Ok(expr)
    }

    fn parse_concat_expr(&mut self) -> DbResult<Expr> {
        let mut expr = self.parse_additive_expr()?;

        while self.consume_kind(&TokenKind::PipePipe) {
            let right = self.parse_additive_expr()?;
            let span = expr.span().merge(right.span());
            expr = Expr::BinaryOp {
                left: Box::new(expr),
                op: BinaryOperator::Concat,
                right: Box::new(right),
                span,
            };
        }

        Ok(expr)
    }

    fn parse_json_access_expr(&mut self) -> DbResult<Expr> {
        let mut left = self.parse_multiplicative_expr()?;
        loop {
            if self.current().kind == TokenKind::HashMinus {
                let op_span = self.current().span;
                self.advance();
                let right = self.parse_multiplicative_expr()?;
                let span = left.span().merge(right.span());
                let name = ObjectName {
                    parts: vec!["jsonb_delete_path".to_owned()],
                    span: op_span,
                };
                left = Expr::FunctionCall {
                    name,
                    args: vec![left, right],
                    distinct: false,
                    filter: None,
                    span,
                };
                continue;
            }
            let Some(op) = self.peek_json_access_op() else {
                break;
            };
            self.advance();
            let right = self.parse_multiplicative_expr()?;
            let span = left.span().merge(right.span());
            left = Expr::BinaryOp {
                left: Box::new(left),
                op,
                right: Box::new(right),
                span,
            };
        }
        Ok(left)
    }

    fn peek_json_access_op(&self) -> Option<BinaryOperator> {
        match self.current().kind {
            TokenKind::DoubleArrow => Some(BinaryOperator::JsonGetText),
            TokenKind::Arrow => Some(BinaryOperator::JsonGet),
            TokenKind::HashDoubleArrow => Some(BinaryOperator::JsonPathGetText),
            TokenKind::HashArrow => Some(BinaryOperator::JsonPathGet),
            _ => None,
        }
    }

    fn parse_multiplicative_expr(&mut self) -> DbResult<Expr> {
        let mut expr = self.parse_unary_expr()?;

        loop {
            let operator = match self.current().kind {
                TokenKind::Star => Some(BinaryOperator::Mul),
                TokenKind::Slash => Some(BinaryOperator::Div),
                TokenKind::Percent => Some(BinaryOperator::Mod),
                TokenKind::Caret => Some(BinaryOperator::Exp),
                _ => None,
            };
            let Some(operator) = operator else {
                break;
            };

            self.advance();
            let right = self.parse_unary_expr()?;
            let span = expr.span().merge(right.span());
            expr = Expr::BinaryOp {
                left: Box::new(expr),
                op: operator,
                right: Box::new(right),
                span,
            };
        }

        Ok(expr)
    }

    fn parse_unary_expr(&mut self) -> DbResult<Expr> {
        if self.current().kind == TokenKind::Plus {
            self.advance(); // consume '+'
            self.enter_expr_recursion()?;
            let result = self.parse_unary_expr();
            self.leave_expr_recursion();
            return result;
        }

        if self.current().kind == TokenKind::Minus {
            let token = self.advance();
            self.enter_expr_recursion()?;
            let expr = self.parse_unary_expr();
            self.leave_expr_recursion();
            let expr = expr?;
            let span = token.span.merge(expr.span());
            return Ok(Expr::UnaryOp {
                op: UnaryOperator::Minus,
                expr: Box::new(expr),
                span,
            });
        }

        if self.current().kind == TokenKind::Tilde {
            let token = self.advance();
            self.enter_expr_recursion()?;
            let expr = self.parse_unary_expr();
            self.leave_expr_recursion();
            let expr = expr?;
            let span = token.span.merge(expr.span());
            return Ok(Expr::UnaryOp {
                op: UnaryOperator::BitwiseNot,
                expr: Box::new(expr),
                span,
            });
        }

        if self.current().kind == TokenKind::At {
            let token = self.advance();
            self.enter_expr_recursion()?;
            let expr = self.parse_unary_expr();
            self.leave_expr_recursion();
            let expr = expr?;
            let span = token.span.merge(expr.span());
            return Ok(Expr::UnaryOp {
                op: UnaryOperator::Abs,
                expr: Box::new(expr),
                span,
            });
        }

        // Handle prefix operators: ?| (vertical), ?- (horizontal), @@ (center),
        // and generic CustomOp as unary prefix - treat as function call.
        // Also handle bare | (Pipe) as prefix for multi-char operators like |@|.
        if matches!(
            self.current().kind,
            TokenKind::QuestionPipe
                | TokenKind::AtAt
                | TokenKind::CustomOp
                | TokenKind::Excl
                | TokenKind::Hash
                | TokenKind::Pipe
        ) {
            let token = self.advance();
            self.enter_expr_recursion()?;
            let expr = self.parse_unary_expr();
            self.leave_expr_recursion();
            let expr = expr?;
            let span = token.span.merge(expr.span());
            // Detect |/ (square root) and ||/ (cube root) by token kind and span length
            let token_len = token.span.end - token.span.start;
            let op = if token.kind == TokenKind::CustomOp && token_len == 2 {
                // |/ square root
                UnaryOperator::SquareRoot
            } else if token.kind == TokenKind::CustomOp && token_len == 3 {
                // ||/ cube root
                UnaryOperator::CubeRoot
            } else {
                UnaryOperator::Abs
            };
            return Ok(Expr::UnaryOp {
                op,
                expr: Box::new(expr),
                span,
            });
        }

        self.parse_postfix_cast_expr()
    }
}
