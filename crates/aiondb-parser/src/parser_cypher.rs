//! Cypher query parser for graph pattern matching.
//!
//! Supports: MATCH, OPTIONAL MATCH, WITH, CREATE, MERGE, SET, DELETE, DETACH DELETE, RETURN, CALL

#![allow(clippy::redundant_closure_for_method_calls)]

use aiondb_core::{DbError, DbResult};

use crate::{
    ast::{
        CypherCallClause, CypherClause, CypherCreateClause, CypherDeleteClause, CypherDirection,
        CypherForeachClause, CypherMatchClause, CypherMergeAction, CypherMergeClause,
        CypherNodePattern, CypherPathFunction, CypherPathPattern, CypherRelPattern,
        CypherRemoveClause, CypherRemoveItem, CypherReturnClause, CypherReturnItem,
        CypherSetClause, CypherSetItem, CypherStatement, CypherUnion, CypherUnwindClause,
        CypherWithClause, Expr, OrderByItem,
    },
    keywords::Keyword,
    tokens::TokenKind,
    Parser,
};

impl Parser {
    /// Parse a full Cypher statement.
    ///
    /// Entry point when the parser sees the MATCH, OPTIONAL, or CREATE keyword
    /// in a graph query context. A Cypher statement is composed of one or more
    /// clauses: MATCH, OPTIONAL MATCH, CREATE, MERGE, SET, DELETE, RETURN.
    pub(crate) fn parse_cypher_statement(&mut self) -> DbResult<CypherStatement> {
        if self.cypher_depth > 64 {
            return Err(DbError::syntax_error(
                "Cypher statement nesting too deep (maximum is 64)",
            ));
        }
        self.cypher_depth = self.cypher_depth.saturating_add(1);
        let prev_mode = self.cypher_mode;
        self.cypher_mode = true;
        let result = self.parse_cypher_statement_inner();
        self.cypher_mode = prev_mode;
        self.cypher_depth = self.cypher_depth.saturating_sub(1);
        result
    }

    fn parse_cypher_statement_inner(&mut self) -> DbResult<CypherStatement> {
        let start_span = self.current().span;
        let mut clauses = Vec::new();

        loop {
            let clause = if matches!(
                self.current().kind,
                TokenKind::Keyword(Keyword::Match | Keyword::Optional)
            ) {
                CypherClause::Match(self.parse_cypher_match_clause()?)
            } else if matches!(self.current().kind, TokenKind::Keyword(Keyword::Create)) {
                CypherClause::Create(self.parse_cypher_create_clause()?)
            } else if matches!(self.current().kind, TokenKind::Keyword(Keyword::Merge)) {
                CypherClause::Merge(self.parse_cypher_merge_clause()?)
            } else if matches!(self.current().kind, TokenKind::Keyword(Keyword::Set)) {
                CypherClause::Set(self.parse_cypher_set_clause()?)
            } else if matches!(
                self.current().kind,
                TokenKind::Keyword(Keyword::Delete | Keyword::Detach)
            ) {
                CypherClause::Delete(self.parse_cypher_delete_clause()?)
            } else if matches!(self.current().kind, TokenKind::Keyword(Keyword::Unwind)) {
                CypherClause::Unwind(self.parse_cypher_unwind_clause()?)
            } else if matches!(self.current().kind, TokenKind::Keyword(Keyword::Remove)) {
                CypherClause::Remove(self.parse_cypher_remove_clause()?)
            } else if matches!(self.current().kind, TokenKind::Keyword(Keyword::With)) {
                CypherClause::With(self.parse_cypher_with_clause()?)
            } else if matches!(self.current().kind, TokenKind::Keyword(Keyword::Return)) {
                CypherClause::Return(self.parse_cypher_return_clause()?)
            } else if matches!(self.current().kind, TokenKind::Keyword(Keyword::Call)) {
                CypherClause::Call(self.parse_cypher_call_clause()?)
            } else if matches!(self.current().kind, TokenKind::Keyword(Keyword::Foreach)) {
                CypherClause::Foreach(self.parse_cypher_foreach_clause()?)
            } else {
                break;
            };

            clauses.push(clause);

            // RETURN is always the last clause
            if matches!(clauses.last(), Some(CypherClause::Return(_))) {
                break;
            }
        }

        if clauses.is_empty() {
            return self.syntax_error_current("expected MATCH, CREATE, MERGE, CALL, or RETURN");
        }

        let clauses_end_span = clauses.last().map_or(start_span, |c| c.span());

        // Check for UNION [ALL]
        let union = if self.consume_keyword(Keyword::Union).is_some() {
            let union_span = self.previous_span();
            let all = self.consume_keyword(Keyword::All).is_some();
            let right = self.parse_cypher_statement()?;
            let span = union_span.merge(right.span);
            Some(Box::new(CypherUnion { all, right, span }))
        } else {
            None
        };

        let end_span = union.as_ref().map_or(clauses_end_span, |u| u.span);

        Ok(CypherStatement {
            clauses,
            union,
            span: start_span.merge(end_span),
        })
    }

    /// Parse MATCH or OPTIONAL MATCH clause.
    ///
    /// Grammar: [OPTIONAL] MATCH pattern [, pattern]* [WHERE expr]
    fn parse_cypher_match_clause(&mut self) -> DbResult<CypherMatchClause> {
        let optional = self.consume_keyword(Keyword::Optional).is_some();
        let match_token = self.expect_keyword(Keyword::Match)?;
        let start_span = if optional {
            // span already moved past OPTIONAL
            self.previous_span()
        } else {
            match_token.span
        };

        // Parse comma-separated path patterns
        let mut patterns = Vec::new();
        patterns.push(self.parse_cypher_path_pattern()?);
        while self.consume_kind(&TokenKind::Comma) {
            patterns.push(self.parse_cypher_path_pattern()?);
        }

        // Optional WHERE clause
        let where_clause = if self.consume_keyword(Keyword::Where).is_some() {
            Some(self.parse_expr()?)
        } else {
            None
        };

        let end_span = where_clause
            .as_ref()
            .map(|e| e.span())
            .or_else(|| patterns.last().map(|p| p.span))
            .unwrap_or(start_span);

        Ok(CypherMatchClause {
            optional,
            patterns,
            where_clause,
            span: start_span.merge(end_span),
        })
    }

    /// Parse a path pattern: `(a:Person)-[r:KNOWS]->(b:Person)`
    ///
    /// Also handles path-finding function wrappers:
    /// - `shortestPath((a)-[*..6]->(b))`
    /// - `allShortestPaths((a)-[*..6]->(b))`
    ///
    /// A path pattern is an alternating sequence of node and relationship
    /// patterns, always starting and ending with a node pattern.
    pub(crate) fn parse_cypher_path_pattern(&mut self) -> DbResult<CypherPathPattern> {
        let path_variable = if matches!(self.current().kind, TokenKind::Identifier(_))
            && self.index + 1 < self.tokens.len()
            && matches!(self.tokens[self.index + 1].kind, TokenKind::Eq)
        {
            let (name, _) = self.expect_identifier()?;
            self.expect_token(&TokenKind::Eq)?;
            Some(name)
        } else {
            None
        };

        // Check for shortestPath(...) or allShortestPaths(...) wrapper.
        let path_function = self.try_parse_path_function()?;
        let wrapped = path_function.is_some();

        let first_node = self.parse_cypher_node_pattern()?;
        let start_span = first_node.span;
        let mut nodes = vec![first_node];
        let mut rels = Vec::new();

        // Parse alternating rel-node pairs
        while self.is_rel_pattern_start() {
            let rel = self.parse_cypher_rel_pattern()?;
            rels.push(rel);
            let node = self.parse_cypher_node_pattern()?;
            nodes.push(node);
        }

        // Close the wrapper parenthesis if we opened one.
        if wrapped {
            self.expect_token(&TokenKind::RParen)?;
        }

        let end_span = nodes.last().map_or(start_span, |n| n.span);
        Ok(CypherPathPattern {
            path_function,
            nodes,
            rels,
            path_variable,
            span: start_span.merge(end_span),
        })
    }

    /// Try to consume a `shortestPath(` or `allShortestPaths(` prefix.
    ///
    /// Returns `Some(CypherPathFunction)` if found, `None` otherwise.
    /// On success the opening `(` has been consumed and the caller must
    /// consume the closing `)` after parsing the inner pattern.
    fn try_parse_path_function(&mut self) -> DbResult<Option<CypherPathFunction>> {
        // We need an identifier followed by `(`.
        let func = match &self.current().kind {
            TokenKind::Identifier(name) => {
                let lower = name.to_ascii_lowercase();
                match lower.as_str() {
                    "shortestpath" => Some(CypherPathFunction::ShortestPath),
                    "allshortestpaths" => Some(CypherPathFunction::AllShortestPaths),
                    _ => None,
                }
            }
            _ => None,
        };

        if let Some(f) = func {
            // Peek ahead: the next token must be `(` for this to be a path
            // function call (not just a variable named "shortestPath").
            if self.index + 1 < self.tokens.len()
                && matches!(self.tokens[self.index + 1].kind, TokenKind::LParen)
            {
                // Consume the identifier and the `(`.
                self.advance(); // identifier
                self.advance(); // `(`
                return Ok(Some(f));
            }
        }
        Ok(None)
    }

    /// Check if the current position looks like the start of a relationship
    /// pattern: `-[`, `--`, `->`, or `<-`.
    fn is_rel_pattern_start(&self) -> bool {
        match &self.current().kind {
            // `-` starts `-[...]->`, `-[...]-`, `-->`, `--`
            TokenKind::Minus => true,
            // `<` starts `<-[...]-`, `<--`
            TokenKind::Lt => {
                self.index + 1 < self.tokens.len()
                    && matches!(self.tokens[self.index + 1].kind, TokenKind::Minus)
            }
            _ => false,
        }
    }

    /// Parse a node pattern: `(variable:Label {prop: value})`
    ///
    /// Grammar: `(` [variable] [`:` Label] [`{` key `:` value `,` ... `}`] `)`
    fn parse_cypher_node_pattern(&mut self) -> DbResult<CypherNodePattern> {
        let lparen = self.expect_token(&TokenKind::LParen)?;

        let mut variable = None;
        let mut labels = Vec::new();
        let mut properties = Vec::new();

        // Check for variable name (identifier or keyword-as-identifier before `:` or `)` or `{`)
        if !matches!(
            self.current().kind,
            TokenKind::RParen | TokenKind::Colon | TokenKind::LBrace
        ) {
            let (name, _) = self.expect_identifier()?;
            variable = Some(name);
        }

        // Check for labels: `:Label` or `:Label1:Label2`
        while self.consume_kind(&TokenKind::Colon) {
            let (lbl, _) = self.expect_identifier()?;
            labels.push(lbl);
        }

        // Check for inline properties: `{key: value, ...}`
        if matches!(self.current().kind, TokenKind::LBrace) {
            properties = self.parse_cypher_properties()?;
        }

        let rparen = self.expect_token(&TokenKind::RParen)?;

        Ok(CypherNodePattern {
            variable,
            labels,
            properties,
            span: lparen.span.merge(rparen.span),
        })
    }

    /// Parse a relationship pattern: `-[variable:Type *min..max]->`
    ///
    /// Handles all three direction forms:
    /// - `-[...]->`  or `-->`       (Outgoing)
    /// - `<-[...]-`  or `<--`       (Incoming)
    /// - `-[...]-`   or `--`        (Both / undirected)
    ///
    /// Tokenization notes:
    /// - `->` is a single `Arrow` token
    /// - `<-` is `Lt` + `Minus`
    /// - `-->` is `Minus` + `Arrow`
    fn parse_cypher_rel_pattern(&mut self) -> DbResult<CypherRelPattern> {
        let start_span = self.current().span;

        // Determine left arrow head: `<-` means incoming
        let left_arrow = if self.consume_kind(&TokenKind::Lt) {
            self.expect_token(&TokenKind::Minus)?;
            true
        } else {
            // Consume the left dash `-`
            self.expect_token(&TokenKind::Minus)?;
            false
        };

        let mut variable = None;
        let mut rel_type = None;
        let mut variable_length = false;
        let mut min_hops = None;
        let mut max_hops = None;
        let mut properties = Vec::new();
        let mut rel_types_alt = Vec::new();

        // Check for bracket-enclosed details: `[...]`
        let has_brackets = self.consume_kind(&TokenKind::LBracket);
        if has_brackets {
            // Optional variable
            if !matches!(
                self.current().kind,
                TokenKind::RBracket | TokenKind::Colon | TokenKind::Star | TokenKind::LBrace
            ) {
                let (name, _) = self.expect_identifier()?;
                variable = Some(name);
            }

            // Optional type: `:TYPE` or `:TYPE1|TYPE2|...`
            if self.consume_kind(&TokenKind::Colon) {
                let (t, _) = self.expect_identifier()?;
                rel_type = Some(t);
                while self.consume_kind(&TokenKind::Pipe) {
                    let (alt, _) = self.expect_identifier()?;
                    rel_types_alt.push(alt);
                }
            }

            // Optional variable-length: `*`, `*2`, `*1..3`, `*..5`, `*2..`
            if self.consume_kind(&TokenKind::Star) {
                variable_length = true;
                let (mn, mx) = self.parse_cypher_var_length()?;
                min_hops = mn;
                max_hops = mx;
            }

            // Optional inline properties
            if matches!(self.current().kind, TokenKind::LBrace) {
                properties = self.parse_cypher_properties()?;
            }

            self.expect_token(&TokenKind::RBracket)?;
        }

        // Right side: `->` (Arrow token) for outgoing, `-` (Minus) for undirected
        let right_arrow = if self.consume_kind(&TokenKind::Arrow) {
            true
        } else {
            self.expect_token(&TokenKind::Minus)?;
            false
        };

        let direction = match (left_arrow, right_arrow) {
            (false, true) => CypherDirection::Outgoing,
            (true, false) => CypherDirection::Incoming,
            (false, false) => CypherDirection::Both,
            (true, true) => {
                return self.syntax_error(
                    start_span,
                    "relationship pattern cannot have arrows on both sides",
                );
            }
        };

        let end_span = self.previous_span();

        Ok(CypherRelPattern {
            variable,
            rel_type,
            rel_types_alt,
            direction,
            variable_length,
            min_hops,
            max_hops,
            properties,
            span: start_span.merge(end_span),
        })
    }

    /// Parse variable-length bounds after `*`: `1..3`, `..5`, `2..`, `2`, or empty.
    ///
    /// Tokenization notes:
    /// - `1..3` -> `Integer(1)`, `Dot`, `NumericLiteral(".3")` (since `.3` is a float)
    /// - `..5`  -> `Dot`, `NumericLiteral(".5")`
    /// - `2..`  -> `Integer(2)`, `Dot`, `Dot`
    /// - `3`    -> `Integer(3)`
    /// - (empty)
    fn parse_cypher_var_length(&mut self) -> DbResult<(Option<u32>, Option<u32>)> {
        let span = self.current().span;
        // Check for min value
        let min = if let TokenKind::Integer(n) = &self.current().kind {
            let val = u32::try_from(*n).map_err(|_| {
                DbError::syntax_error("variable-length bound too large")
                    .with_position(span.start + 1)
            })?;
            self.advance();
            Some(val)
        } else {
            None
        };

        // Check for `..` (range separator)
        if self.consume_kind(&TokenKind::Dot) {
            // After the first dot, we might have:
            // - Another Dot token (for `2..` or `2..<nothing>`)
            // - A NumericLiteral starting with `.` (for `1..3` which tokenizes as
            //   Integer(1), Dot, NumericLiteral(".3"))
            if self.consume_kind(&TokenKind::Dot) {
                // Two explicit dots. Check for max integer.
                let max = if let TokenKind::Integer(n) = &self.current().kind {
                    let val = u32::try_from(*n).map_err(|_| {
                        DbError::syntax_error("variable-length bound too large")
                            .with_position(span.start + 1)
                    })?;
                    self.advance();
                    Some(val)
                } else {
                    None
                };
                if let (Some(mn), Some(mx)) = (min, max) {
                    if mn > mx {
                        return self.syntax_error(span, "variable-length minimum exceeds maximum");
                    }
                }
                Ok((min, max))
            } else if let TokenKind::NumericLiteral(ref s) = self.current().kind {
                // The lexer consumed `.N` as a numeric literal.
                // Extract the integer value from the float representation.
                let s = s.clone();
                let max_val = s
                    .trim_start_matches('0')
                    .trim_start_matches('.')
                    .parse::<u32>()
                    .ok();
                self.advance();
                if let (Some(mn), Some(mx)) = (min, max_val) {
                    if mn > mx {
                        return self.syntax_error(span, "variable-length minimum exceeds maximum");
                    }
                }
                Ok((min, max_val))
            } else {
                // Just `N..` with no max
                Ok((min, None))
            }
        } else if let TokenKind::NumericLiteral(ref s) = self.current().kind {
            // Some lexer paths can tokenize `*1..3` as:
            // `*`, Integer(1), NumericLiteral(".3")
            // (without a standalone Dot token between min and max).
            if let Some(rest) = s.strip_prefix('.') {
                let max = if rest.is_empty() {
                    None
                } else {
                    Some(rest.parse::<u32>().map_err(|_| {
                        DbError::syntax_error("variable-length bound too large")
                            .with_position(span.start + 1)
                    })?)
                };
                self.advance();
                if let (Some(mn), Some(mx)) = (min, max) {
                    if mn > mx {
                        return self.syntax_error(span, "variable-length minimum exceeds maximum");
                    }
                }
                Ok((min, max))
            } else if min.is_some() {
                // Just `*N` means exactly N hops: min=N, max=N
                Ok((min, min))
            } else {
                // Gracefully accept `*.N` as max=N.
                let max = s.parse::<u32>().ok();
                self.advance();
                Ok((None, max))
            }
        } else if min.is_some() {
            // Just `*N` means exactly N hops: min=N, max=N
            Ok((min, min))
        } else {
            // Just `*` with no bounds
            Ok((None, None))
        }
    }

    /// Parse inline properties: `{key: value, key2: value2, ...}`
    fn parse_cypher_properties(&mut self) -> DbResult<Vec<(String, Expr)>> {
        self.expect_token(&TokenKind::LBrace)?;
        let mut props = Vec::new();

        if !matches!(self.current().kind, TokenKind::RBrace) {
            loop {
                let (key, _) = self.expect_identifier()?;
                self.expect_token(&TokenKind::Colon)?;
                let value = self.parse_expr()?;
                props.push((key, value));
                if !self.consume_kind(&TokenKind::Comma) {
                    break;
                }
            }
        }

        self.expect_token(&TokenKind::RBrace)?;
        Ok(props)
    }

    /// Parse a Cypher projection: shared logic for RETURN and WITH clauses.
    ///
    /// Handles: [DISTINCT] expr [AS alias], ... [ORDER BY ...] [SKIP n] [LIMIT n]
    fn parse_cypher_projection(
        &mut self,
    ) -> DbResult<(
        bool,
        Vec<CypherReturnItem>,
        Vec<OrderByItem>,
        Option<Expr>,
        Option<Expr>,
    )> {
        let distinct = self.consume_keyword(Keyword::Distinct).is_some();

        // Parse projection items
        let mut items = Vec::new();
        loop {
            let expr = self.parse_expr()?;
            let item_start = expr.span();
            let alias = if self.consume_keyword(Keyword::As).is_some() {
                let (name, _) = self.expect_identifier()?;
                Some(name)
            } else {
                None
            };
            let item_end = alias.as_ref().map_or(item_start, |_| self.previous_span());
            items.push(CypherReturnItem {
                expr,
                alias,
                span: item_start.merge(item_end),
            });
            if !self.consume_kind(&TokenKind::Comma) {
                break;
            }
        }

        // ORDER BY
        let order_by = if self.consume_keyword(Keyword::Order).is_some() {
            self.expect_keyword(Keyword::By)?;
            let mut order_items = Vec::new();
            loop {
                let expr = self.parse_expr()?;
                let mut item_span = expr.span();
                let descending = if self.consume_keyword(Keyword::Desc).is_some() {
                    item_span = item_span.merge(self.previous_span());
                    true
                } else if self.consume_keyword(Keyword::Asc).is_some() {
                    item_span = item_span.merge(self.previous_span());
                    false
                } else if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("descending"))
                {
                    self.advance();
                    item_span = item_span.merge(self.previous_span());
                    true
                } else if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("ascending"))
                {
                    self.advance();
                    item_span = item_span.merge(self.previous_span());
                    false
                } else {
                    false
                };
                let nulls_first = if self.consume_keyword(Keyword::Nulls).is_some() {
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
                order_items.push(OrderByItem {
                    expr,
                    descending,
                    nulls_first,
                    span: item_span,
                });
                if !self.consume_kind(&TokenKind::Comma) {
                    break;
                }
            }
            order_items
        } else {
            Vec::new()
        };

        // SKIP
        let skip = if self.consume_keyword(Keyword::Skip).is_some() {
            Some(self.parse_expr()?)
        } else {
            None
        };

        // LIMIT
        let limit = if self.consume_keyword(Keyword::Limit).is_some() {
            Some(self.parse_expr()?)
        } else {
            None
        };

        Ok((distinct, items, order_by, skip, limit))
    }

    /// Parse RETURN clause.
    ///
    /// Grammar: RETURN [DISTINCT] expr [AS alias] [, ...] [ORDER BY ...] [SKIP n] [LIMIT n]
    fn parse_cypher_return_clause(&mut self) -> DbResult<CypherReturnClause> {
        let return_token = self.expect_keyword(Keyword::Return)?;
        let (distinct, items, order_by, skip, limit) = self.parse_cypher_projection()?;

        let end_span = limit
            .as_ref()
            .map(|e| e.span())
            .or_else(|| skip.as_ref().map(|e| e.span()))
            .or_else(|| order_by.last().map(|o| o.span))
            .or_else(|| items.last().map(|i| i.span))
            .unwrap_or(return_token.span);

        Ok(CypherReturnClause {
            distinct,
            items,
            order_by,
            skip,
            limit,
            span: return_token.span.merge(end_span),
        })
    }

    /// Parse WITH clause (pipeline projection, similar to RETURN but continues the query).
    ///
    /// Grammar: WITH [DISTINCT] expr [AS alias], ... [ORDER BY ...] [SKIP n] [LIMIT n] [WHERE expr]
    fn parse_cypher_with_clause(&mut self) -> DbResult<CypherWithClause> {
        let with_token = self.expect_keyword(Keyword::With)?;
        let (distinct, items, order_by, skip, limit) = self.parse_cypher_projection()?;

        // WHERE (Cypher WITH supports WHERE after the projection)
        let where_clause = if self.consume_keyword(Keyword::Where).is_some() {
            Some(self.parse_expr()?)
        } else {
            None
        };

        let end_span = where_clause
            .as_ref()
            .map(|e| e.span())
            .or_else(|| limit.as_ref().map(|e| e.span()))
            .or_else(|| skip.as_ref().map(|e| e.span()))
            .or_else(|| order_by.last().map(|o| o.span))
            .or_else(|| items.last().map(|i| i.span))
            .unwrap_or(with_token.span);

        Ok(CypherWithClause {
            distinct,
            items,
            where_clause,
            order_by,
            skip,
            limit,
            span: with_token.span.merge(end_span),
        })
    }

    /// Parse CREATE clause (graph creation).
    ///
    /// Grammar: CREATE pattern [, pattern]*
    fn parse_cypher_create_clause(&mut self) -> DbResult<CypherCreateClause> {
        let create_token = self.expect_keyword(Keyword::Create)?;

        let mut patterns = Vec::new();
        patterns.push(self.parse_cypher_path_pattern()?);
        while self.consume_kind(&TokenKind::Comma) {
            patterns.push(self.parse_cypher_path_pattern()?);
        }

        let end_span = patterns.last().map_or(create_token.span, |p| p.span);
        Ok(CypherCreateClause {
            patterns,
            span: create_token.span.merge(end_span),
        })
    }

    /// Parse MERGE clause.
    ///
    /// Grammar: MERGE pattern [ON CREATE SET ...] [ON MATCH SET ...]
    fn parse_cypher_merge_clause(&mut self) -> DbResult<CypherMergeClause> {
        let merge_token = self.expect_keyword(Keyword::Merge)?;
        let pattern = self.parse_cypher_path_pattern()?;

        let mut actions = Vec::new();

        // Parse ON CREATE SET and ON MATCH SET actions
        while matches!(self.current().kind, TokenKind::Keyword(Keyword::On)) {
            let on_token = self.expect_keyword(Keyword::On)?;

            let on_create = if self.consume_keyword(Keyword::Create).is_some() {
                true
            } else {
                self.expect_keyword(Keyword::Match)?;
                false
            };

            self.expect_keyword(Keyword::Set)?;
            let items = self.parse_cypher_set_items()?;

            let end_span = items.last().map_or(on_token.span, |i| match i {
                CypherSetItem::Property { span, .. }
                | CypherSetItem::Label { span, .. }
                | CypherSetItem::ReplaceProperties { span, .. }
                | CypherSetItem::MergeProperties { span, .. } => *span,
            });

            actions.push(CypherMergeAction {
                on_create,
                items,
                span: on_token.span.merge(end_span),
            });
        }

        let end_span = actions.last().map_or(pattern.span, |a| a.span);

        Ok(CypherMergeClause {
            pattern,
            actions,
            span: merge_token.span.merge(end_span),
        })
    }

    /// Parse SET clause.
    ///
    /// Grammar: SET item [, item]*
    fn parse_cypher_set_clause(&mut self) -> DbResult<CypherSetClause> {
        let set_token = self.expect_keyword(Keyword::Set)?;
        let items = self.parse_cypher_set_items()?;

        let end_span = items.last().map_or(set_token.span, |i| match i {
            CypherSetItem::Property { span, .. }
            | CypherSetItem::Label { span, .. }
            | CypherSetItem::ReplaceProperties { span, .. }
            | CypherSetItem::MergeProperties { span, .. } => *span,
        });

        Ok(CypherSetClause {
            items,
            span: set_token.span.merge(end_span),
        })
    }

    /// Parse comma-separated SET items.
    ///
    /// Each item is either:
    /// - `variable.property = expr` (property assignment)
    /// - `variable:Label` (label set)
    fn parse_cypher_set_items(&mut self) -> DbResult<Vec<CypherSetItem>> {
        let mut items = Vec::new();
        loop {
            let (variable, var_span) = self.expect_identifier()?;

            if self.consume_kind(&TokenKind::Dot) {
                // Property assignment: variable.property = expr
                let (property, _) = self.expect_identifier()?;
                self.expect_token(&TokenKind::Eq)?;
                let expr = self.parse_expr()?;
                let end_span = expr.span();
                items.push(CypherSetItem::Property {
                    variable,
                    property,
                    expr,
                    span: var_span.merge(end_span),
                });
            } else if matches!(self.current().kind, TokenKind::Colon) {
                // Label set: variable:Label[:Label2:Label3...]
                // Consume all consecutive `:Label` pairs.
                loop {
                    if !self.consume_kind(&TokenKind::Colon) {
                        break;
                    }
                    let (label, label_span) = self.expect_identifier()?;
                    items.push(CypherSetItem::Label {
                        variable: variable.clone(),
                        label,
                        span: var_span.merge(label_span),
                    });
                }
            } else if self.consume_kind(&TokenKind::Plus) && self.consume_kind(&TokenKind::Eq) {
                // Merge properties: variable += {key: value, ...}
                let props = self.parse_cypher_properties()?;
                let end_span = self.previous_span();
                items.push(CypherSetItem::MergeProperties {
                    variable,
                    entries: props.into_iter().map(|(k, v)| (k, Box::new(v))).collect(),
                    span: var_span.merge(end_span),
                });
            } else if self.consume_kind(&TokenKind::Eq) {
                // Replace properties: variable = {key: value, ...}
                let props = self.parse_cypher_properties()?;
                let end_span = self.previous_span();
                items.push(CypherSetItem::ReplaceProperties {
                    variable,
                    entries: props.into_iter().map(|(k, v)| (k, Box::new(v))).collect(),
                    span: var_span.merge(end_span),
                });
            } else {
                return self.syntax_error(
                    var_span,
                    "expected '.', ':', '=', or '+=' after variable in SET",
                );
            }

            if !self.consume_kind(&TokenKind::Comma) {
                break;
            }
        }
        Ok(items)
    }

    /// Parse DELETE / DETACH DELETE clause.
    ///
    /// Grammar: [DETACH] DELETE variable [, variable]*
    fn parse_cypher_delete_clause(&mut self) -> DbResult<CypherDeleteClause> {
        let start_span = self.current().span;
        let detach = self.consume_keyword(Keyword::Detach).is_some();
        self.expect_keyword(Keyword::Delete)?;

        let mut variables = Vec::new();
        loop {
            let (name, _) = self.expect_identifier()?;
            variables.push(name);
            if !self.consume_kind(&TokenKind::Comma) {
                break;
            }
        }

        let end_span = self.previous_span();
        Ok(CypherDeleteClause {
            detach,
            variables,
            span: start_span.merge(end_span),
        })
    }

    /// Parse UNWIND clause.
    ///
    /// Grammar: UNWIND expr AS variable
    fn parse_cypher_unwind_clause(&mut self) -> DbResult<CypherUnwindClause> {
        let unwind_token = self.expect_keyword(Keyword::Unwind)?;
        let expr = self.parse_expr()?;
        self.expect_keyword(Keyword::As)?;
        let (variable, var_span) = self.expect_identifier()?;

        Ok(CypherUnwindClause {
            expr,
            variable,
            span: unwind_token.span.merge(var_span),
        })
    }

    /// Parse REMOVE clause.
    ///
    /// Grammar: REMOVE item [, item]*
    /// Each item is either:
    /// - `variable.property` (property removal)
    /// - `variable:Label` (label removal)
    fn parse_cypher_remove_clause(&mut self) -> DbResult<CypherRemoveClause> {
        let remove_token = self.expect_keyword(Keyword::Remove)?;
        let mut items = Vec::new();

        loop {
            let (variable, var_span) = self.expect_identifier()?;

            if self.consume_kind(&TokenKind::Dot) {
                // Property removal: variable.property
                let (property, prop_span) = self.expect_identifier()?;
                items.push(CypherRemoveItem::Property {
                    variable,
                    property,
                    span: var_span.merge(prop_span),
                });
            } else if self.consume_kind(&TokenKind::Colon) {
                // Label removal: variable:Label
                let (label, label_span) = self.expect_identifier()?;
                items.push(CypherRemoveItem::Label {
                    variable,
                    label,
                    span: var_span.merge(label_span),
                });
            } else {
                return self.syntax_error(var_span, "expected '.' or ':' after variable in REMOVE");
            }

            if !self.consume_kind(&TokenKind::Comma) {
                break;
            }
        }

        let end_span = items.last().map_or(remove_token.span, |i| match i {
            CypherRemoveItem::Property { span, .. } => *span,
            CypherRemoveItem::Label { span, .. } => *span,
        });

        Ok(CypherRemoveClause {
            items,
            span: remove_token.span.merge(end_span),
        })
    }

    /// Parse a CALL clause for procedure invocation.
    ///
    /// Grammar: CALL procedure.name(args) [YIELD item, ...]
    fn parse_cypher_call_clause(&mut self) -> DbResult<CypherCallClause> {
        let call_token = self.expect_keyword(Keyword::Call)?;
        let start_span = call_token.span;

        if self.consume_kind(&TokenKind::LBrace) {
            let subquery = self.parse_cypher_statement()?;
            let rbrace = self.expect_token(&TokenKind::RBrace)?;
            return Ok(CypherCallClause {
                procedure: String::new(),
                args: Vec::new(),
                yields: Vec::new(),
                subquery: Some(Box::new(subquery)),
                span: start_span.merge(rbrace.span),
            });
        }

        // Parse the procedure name: dotted identifier (e.g. db.labels)
        let (first_part, _) = self.expect_identifier()?;
        let mut procedure = first_part;

        while self.consume_kind(&TokenKind::Dot) {
            let (part, _) = self.expect_identifier()?;
            procedure.push('.');
            procedure.push_str(&part);
        }

        // Parse argument list: (args)
        self.expect_token(&TokenKind::LParen)?;
        let mut args = Vec::new();
        if !matches!(self.current().kind, TokenKind::RParen) {
            args.push(self.parse_expr()?);
            while self.consume_kind(&TokenKind::Comma) {
                args.push(self.parse_expr()?);
            }
        }
        let rparen_token = self.expect_token(&TokenKind::RParen)?;
        let mut end_span = rparen_token.span;

        // Parse optional YIELD clause
        let mut yields = Vec::new();
        if self.consume_keyword(Keyword::Yield).is_some() {
            let (first_yield, first_yield_span) = self.expect_identifier()?;
            yields.push(first_yield);
            end_span = first_yield_span;

            while self.consume_kind(&TokenKind::Comma) {
                let (yield_item, yield_span) = self.expect_identifier()?;
                yields.push(yield_item);
                end_span = yield_span;
            }
        }

        Ok(CypherCallClause {
            procedure,
            args,
            yields,
            subquery: None,
            span: start_span.merge(end_span),
        })
    }

    /// Parse a FOREACH clause.
    ///
    /// Grammar: FOREACH ( variable IN expr | clause [clause ...] )
    fn parse_cypher_foreach_clause(&mut self) -> DbResult<CypherForeachClause> {
        let foreach_token = self.expect_keyword(Keyword::Foreach)?;
        self.expect_token(&TokenKind::LParen)?;

        let (variable, _) = self.expect_identifier()?;

        self.expect_keyword(Keyword::In)?;
        let expr = self.parse_expr()?;

        // The pipe '|' separates the iteration expression from the body clauses.
        self.expect_token(&TokenKind::Pipe)?;

        let mut clauses = Vec::new();
        loop {
            let clause = if matches!(self.current().kind, TokenKind::Keyword(Keyword::Set)) {
                CypherClause::Set(self.parse_cypher_set_clause()?)
            } else if matches!(self.current().kind, TokenKind::Keyword(Keyword::Create)) {
                CypherClause::Create(self.parse_cypher_create_clause()?)
            } else if matches!(self.current().kind, TokenKind::Keyword(Keyword::Merge)) {
                CypherClause::Merge(self.parse_cypher_merge_clause()?)
            } else if matches!(self.current().kind, TokenKind::Keyword(Keyword::Delete))
                || matches!(self.current().kind, TokenKind::Keyword(Keyword::Detach))
            {
                CypherClause::Delete(self.parse_cypher_delete_clause()?)
            } else if matches!(self.current().kind, TokenKind::Keyword(Keyword::Remove)) {
                CypherClause::Remove(self.parse_cypher_remove_clause()?)
            } else if matches!(self.current().kind, TokenKind::Keyword(Keyword::Foreach)) {
                CypherClause::Foreach(self.parse_cypher_foreach_clause()?)
            } else {
                break;
            };
            clauses.push(clause);
        }

        let rparen = self.expect_token(&TokenKind::RParen)?;

        Ok(CypherForeachClause {
            variable,
            expr,
            clauses,
            span: foreach_token.span.merge(rparen.span),
        })
    }
}
