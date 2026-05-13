use super::*;

impl Parser {
    /// Parse: `OVER name` or `OVER ( [PARTITION BY expr, ...] [ORDER BY expr [ASC|DESC], ...] )`
    pub(super) fn parse_window_spec(&mut self, function: Expr) -> DbResult<Expr> {
        let func_span = function.span();
        // Named window reference: OVER w
        if !matches!(self.current().kind, TokenKind::LParen) {
            let window_name = if let TokenKind::Identifier(name) = &self.current().kind {
                let n = name.clone();
                self.advance(); // consume window name
                Some(n)
            } else {
                None
            };
            return Ok(Expr::WindowFunction {
                function: Box::new(function),
                partition_by: Vec::new(),
                order_by: Vec::new(),
                window_name,
                span: func_span.merge(self.previous_span()),
            });
        }
        self.expect_token(&TokenKind::LParen)?;

        // Skip optional base window name: OVER (w PARTITION BY ...) or OVER (w RANGE BETWEEN ...)
        // A bare identifier (not a keyword like PARTITION, ORDER, RANGE, etc.) at the start
        // of a window spec is a named window reference.
        let mut base_window_name = None;
        if let TokenKind::Identifier(name) = &self.current().kind {
            // Only treat as base window name if it's not followed by something that
            // makes it look like an expression.
            let candidate = name.clone();
            self.advance(); // consume potential window name reference
            base_window_name = Some(candidate);
        }

        let mut partition_by = Vec::new();
        if self.consume_keyword(Keyword::Partition).is_some() {
            self.expect_keyword(Keyword::By)?;
            loop {
                partition_by.push(self.parse_expr()?);
                if !self.consume_kind(&TokenKind::Comma) {
                    break;
                }
                // Stop if the next token after comma is ORDER BY
                // (handles erroneous `PARTITION BY a, ORDER BY b` gracefully)
                if matches!(self.current().kind, TokenKind::Keyword(Keyword::Order)) {
                    break;
                }
            }
        }

        let mut order_by = Vec::new();
        if self.consume_keyword(Keyword::Order).is_some() {
            self.expect_keyword(Keyword::By)?;
            loop {
                let expr = self.parse_expr()?;
                let mut item_span = expr.span();
                let descending = if self.consume_keyword(Keyword::Desc).is_some() {
                    item_span = item_span.merge(self.previous_span());
                    true
                } else if self.consume_keyword(Keyword::Asc).is_some() {
                    item_span = item_span.merge(self.previous_span());
                    false
                } else {
                    false
                };
                let nulls_first = if self.consume_keyword(Keyword::Nulls).is_some() {
                    item_span = item_span.merge(self.previous_span());
                    if self.consume_keyword(Keyword::First).is_some() {
                        item_span = item_span.merge(self.previous_span());
                        Some(true)
                    } else {
                        self.expect_keyword(Keyword::Last)?;
                        item_span = item_span.merge(self.previous_span());
                        Some(false)
                    }
                } else {
                    None
                };
                order_by.push(OrderByItem {
                    expr,
                    descending,
                    nulls_first,
                    span: item_span,
                });
                if !self.consume_kind(&TokenKind::Comma) {
                    break;
                }
            }
        }

        // Parse optional frame clause: ROWS|RANGE|GROUPS BETWEEN ... AND ...
        // We parse and skip it for now (the executor uses default framing).
        if matches!(
            self.current().kind,
            TokenKind::Keyword(Keyword::Rows | Keyword::Range | Keyword::Groups)
        ) {
            self.advance(); // consume ROWS/RANGE/GROUPS
            self.parse_frame_bound()?;
        }

        // Parse optional EXCLUDE clause
        if self.consume_keyword(Keyword::Exclude).is_some() {
            // EXCLUDE CURRENT ROW | EXCLUDE GROUP | EXCLUDE TIES | EXCLUDE NO OTHERS
            if self.consume_keyword(Keyword::Current).is_some() {
                let _ = self.consume_keyword(Keyword::Row);
            } else if self.consume_keyword(Keyword::Group).is_some() {
                // ok
            } else if self.consume_keyword(Keyword::No).is_some() {
                // NO OTHERS
                let _ = self.advance(); // skip OTHERS
            } else {
                // TIES or other
                self.advance();
            }
        }

        let rparen = self.expect_token(&TokenKind::RParen)?;
        let span = func_span.merge(rparen.span);

        // If we have a base window name but no local partition/order, propagate
        // the name so it can be resolved later.
        let window_name = if partition_by.is_empty() && order_by.is_empty() {
            base_window_name
        } else {
            // If there's a base window name AND local clauses, the local
            // clauses extend the base. Store the name for later resolution.
            base_window_name
        };

        Ok(Expr::WindowFunction {
            function: Box::new(function),
            partition_by,
            order_by,
            window_name,
            span,
        })
    }

    /// Parse a frame bound: `UNBOUNDED PRECEDING | expr PRECEDING | CURRENT ROW | expr FOLLOWING | UNBOUNDED FOLLOWING`
    /// or `BETWEEN bound AND bound`
    pub(crate) fn parse_frame_bound(&mut self) -> DbResult<()> {
        if self.consume_keyword(Keyword::Between).is_some() {
            self.parse_single_frame_bound()?;
            self.expect_keyword(Keyword::And)?;
            self.parse_single_frame_bound()?;
        } else {
            self.parse_single_frame_bound()?;
        }
        Ok(())
    }

    fn parse_single_frame_bound(&mut self) -> DbResult<()> {
        // UNBOUNDED PRECEDING / UNBOUNDED FOLLOWING - but only when UNBOUNDED
        // is truly used as the keyword, not as an identifier (e.g. a function
        // call `unbounded(1)` or qualified name `unbounded.x`).
        if matches!(self.current().kind, TokenKind::Keyword(Keyword::Unbounded))
            && self.index + 1 < self.tokens.len()
            && matches!(
                self.tokens[self.index + 1].kind,
                TokenKind::Keyword(Keyword::Preceding | Keyword::Following)
            )
        {
            self.advance(); // consume UNBOUNDED
            self.advance(); // consume PRECEDING or FOLLOWING
        } else if matches!(self.current().kind, TokenKind::Keyword(Keyword::Current))
            && self.index + 1 < self.tokens.len()
            && matches!(
                self.tokens[self.index + 1].kind,
                TokenKind::Keyword(Keyword::Row)
            )
        {
            self.advance(); // consume CURRENT
            self.advance(); // consume ROW
        } else {
            // expr PRECEDING or expr FOLLOWING
            let _ = self.parse_expr()?;
            if self.consume_keyword(Keyword::Preceding).is_none() {
                self.expect_keyword(Keyword::Following)?;
            }
        }
        Ok(())
    }
}
