#![allow(clippy::unnested_or_patterns, clippy::collapsible_if)]

mod from_ref;
mod select_helpers;
mod with_clause;

use aiondb_core::DbResult;

use select_helpers::flatten_group_by_items;

use crate::{
    ast::{
        CreateTableAsStatement, CteDefinition, DistinctKind, Expr, GroupByItem, GroupBySet,
        JoinClause, JoinType, Literal, ObjectName, OrderByItem, RowLockClause, SelectItem,
        SelectStatement, SetOperationStatement, SetOperationType, Statement,
    },
    keywords::Keyword,
    tokens::TokenKind,
    Parser, Span,
};

impl Parser {
    /// Parse a SELECT without trailing ORDER BY / LIMIT / OFFSET.
    /// Used as the right-hand side of a set operation so those clauses
    /// bind to the combined result rather than the individual SELECT.
    pub(crate) fn parse_select_bare(&mut self) -> DbResult<Statement> {
        let select = self.expect_keyword(Keyword::Select)?;
        let distinct = self.parse_distinct_kind()?;
        let mut items = Vec::new();

        if self.current().kind == TokenKind::Star {
            let star_token = self.advance();
            items.push(SelectItem {
                expr: Expr::Identifier(ObjectName {
                    parts: vec!["*".to_owned()],
                    span: star_token.span,
                }),
                alias: None,
                span: select.span.merge(star_token.span),
            });
        }
        if !items.is_empty() && !self.consume_kind(&TokenKind::Comma) {
            // No more select items after *, skip to FROM
        } else if !matches!(
            self.current().kind,
            TokenKind::Keyword(Keyword::From)
                | TokenKind::Semicolon
                | TokenKind::Eof
                | TokenKind::Keyword(Keyword::Union)
                | TokenKind::Keyword(Keyword::Intersect)
                | TokenKind::Keyword(Keyword::Except)
        ) {
            loop {
                // Handle * as a select item (e.g., SELECT ctid, cmin, * FROM t)
                if self.current().kind == TokenKind::Star {
                    let star_token = self.advance();
                    items.push(SelectItem {
                        expr: Expr::Identifier(ObjectName {
                            parts: vec!["*".to_owned()],
                            span: star_token.span,
                        }),
                        alias: None,
                        span: select.span.merge(star_token.span),
                    });
                    if !self.consume_kind(&TokenKind::Comma) {
                        break;
                    }
                    continue;
                }
                let expr = self.parse_expr()?;
                let mut item_span = select.span.merge(expr.span());
                // Reject WITHIN GROUP until ordered-set aggregate semantics are implemented.
                self.reject_within_group()?;
                let alias = if self.consume_keyword(Keyword::As).is_some() {
                    // Accept keywords (TRUE, FALSE, NULL, etc.) as aliases too
                    let (alias, alias_span) = match &self.current().kind {
                        TokenKind::Keyword(kw) => {
                            let name = kw.name().to_lowercase();
                            let token = self.advance();
                            (name, token.span)
                        }
                        _ => self.expect_identifier()?,
                    };
                    item_span = item_span.merge(alias_span);
                    Some(alias)
                } else {
                    None
                };
                items.push(SelectItem {
                    expr,
                    alias,
                    span: item_span,
                });
                if !self.consume_kind(&TokenKind::Comma) {
                    break;
                }
            }
        }
        // PG compatibility: empty target list followed by FROM is valid for
        // plain/all selects (`SELECT FROM t`, `SELECT ALL FROM t`) but not for
        // DISTINCT variants (`SELECT DISTINCT FROM t` is a syntax error).
        if items.is_empty()
            && !matches!(distinct, DistinctKind::All)
            && matches!(self.current().kind, TokenKind::Keyword(Keyword::From))
        {
            return self.syntax_error_at_current_token();
        }

        let mut span = items
            .last()
            .map_or(select.span, |item| select.span.merge(item.span));
        let mut extra_ctes: Vec<CteDefinition> = Vec::new();
        let (from, from_alias, from_span) = if self.consume_keyword(Keyword::From).is_some() {
            let _ = self.consume_keyword(Keyword::Only);
            let (relation, alias, ctes, col_aliases) = self.parse_from_table_ref()?;
            span = span.merge(relation.span);
            extra_ctes.extend(ctes);
            if let Some(aliases) = col_aliases {
                if let Some(ref alias_name) = alias {
                    let wrapper_cte =
                        Self::make_column_alias_cte(alias_name, &relation, &aliases, relation.span);
                    extra_ctes.push(wrapper_cte);
                    let aliased_name = ObjectName {
                        parts: vec![alias_name.clone()],
                        span: relation.span,
                    };
                    (Some(aliased_name), None, Some(span))
                } else {
                    (Some(relation), alias, Some(span))
                }
            } else {
                (Some(relation), alias, Some(span))
            }
        } else {
            (None, None, None)
        };

        let (selection, where_span) =
            if let Some(where_token) = self.consume_keyword(Keyword::Where) {
                let selection = self.parse_expr()?;
                span = span.merge(where_token.span).merge(selection.span());
                (Some(selection), Some(where_token.span))
            } else {
                (None, None)
            };
        let (group_by, group_by_items, group_by_span) =
            if let Some(group_token) = self.consume_keyword(Keyword::Group) {
                let by_token = self.expect_keyword(Keyword::By)?;
                let mut gb_structured = Vec::new();
                let mut gb_span = group_token.span.merge(by_token.span);
                // Skip optional ALL/DISTINCT modifier (PG 16 GROUP BY ALL/DISTINCT)
                if let Some(t) = self.consume_keyword(Keyword::All) {
                    gb_span = gb_span.merge(t.span);
                } else if let Some(t) = self.consume_keyword(Keyword::Distinct) {
                    gb_span = gb_span.merge(t.span);
                }
                loop {
                    if self.is_group_by_set_constructor() {
                        let item = self.parse_group_by_structured_item()?;
                        gb_span = gb_span.merge(self.previous_span());
                        gb_structured.push(item);
                    } else if self.current().kind == TokenKind::LParen
                        && self.index + 1 < self.tokens.len()
                        && self.tokens[self.index + 1].kind == TokenKind::RParen
                    {
                        // GROUP BY () - empty grouping set
                        let lp = self.advance();
                        self.advance(); // consume ')'
                        gb_span = gb_span.merge(lp.span);
                        gb_structured.push(GroupByItem::Empty);
                    } else {
                        let expr = self.parse_expr()?;
                        gb_span = gb_span.merge(expr.span());
                        gb_structured.push(GroupByItem::Plain(expr));
                    }
                    if !self.consume_kind(&TokenKind::Comma) {
                        break;
                    }
                }
                span = span.merge(gb_span);
                // Build flat group_by for backward compatibility
                let flat = flatten_group_by_items(&gb_structured);
                (flat, gb_structured, Some(gb_span))
            } else {
                (Vec::new(), Vec::new(), None)
            };
        let (having, having_span) =
            if let Some(having_token) = self.consume_keyword(Keyword::Having) {
                let expr = self.parse_expr()?;
                let h_span = having_token.span.merge(expr.span());
                span = span.merge(h_span);
                (Some(expr), Some(h_span))
            } else {
                (None, None)
            };

        Ok(Statement::Select(SelectStatement {
            row_lock: None,
            ctes: extra_ctes,
            distinct,
            items,
            from,
            from_alias,
            from_span,
            joins: Vec::new(),
            selection,
            where_span,
            group_by,
            group_by_items,
            group_by_span,
            having,
            having_span,
            window_definitions: Vec::new(),
            order_by: Vec::new(),
            order_by_span: None,
            limit: None,
            limit_span: None,
            offset: None,
            offset_span: None,
            span,
        }))
    }

    pub(crate) fn parse_select_statement(&mut self) -> DbResult<Statement> {
        let select = self.expect_keyword(Keyword::Select)?;
        let distinct = self.parse_distinct_kind()?;
        let mut items = Vec::new();

        // Track SELECT INTO target (detected either before or after select items)
        let mut into_name: Option<ObjectName> = None;
        let mut into_temporary = false;

        if matches!(self.current().kind, TokenKind::Keyword(Keyword::Into)) {
            // SELECT INTO [TEMP|TEMPORARY] [TABLE] name ... → CREATE TABLE AS
            self.advance(); // consume INTO
            into_temporary = self.consume_keyword(Keyword::Temp).is_some()
                || self.consume_keyword(Keyword::Temporary).is_some();
            let _ = self.consume_keyword(Keyword::Table); // optional TABLE keyword
            into_name = Some(self.parse_object_name()?);
            // Fall through to parse select items and the rest of the SELECT body
        }

        if self.current().kind == TokenKind::Star {
            let star_token = self.advance();
            items.push(SelectItem {
                expr: Expr::Identifier(ObjectName {
                    parts: vec!["*".to_owned()],
                    span: star_token.span,
                }),
                alias: None,
                span: select.span.merge(star_token.span),
            });
        }
        if !items.is_empty() && !self.consume_kind(&TokenKind::Comma) {
            // No more select items after *, skip to FROM
        } else if !matches!(
            self.current().kind,
            TokenKind::Keyword(Keyword::From)
                | TokenKind::Semicolon
                | TokenKind::Eof
                | TokenKind::Keyword(Keyword::Union)
                | TokenKind::Keyword(Keyword::Intersect)
                | TokenKind::Keyword(Keyword::Except)
        ) {
            loop {
                // Handle * as a select item (e.g., SELECT ctid, cmin, * FROM t)
                if self.current().kind == TokenKind::Star {
                    let star_token = self.advance();
                    items.push(SelectItem {
                        expr: Expr::Identifier(ObjectName {
                            parts: vec!["*".to_owned()],
                            span: star_token.span,
                        }),
                        alias: None,
                        span: select.span.merge(star_token.span),
                    });
                    if !self.consume_kind(&TokenKind::Comma) {
                        break;
                    }
                    continue;
                }
                let expr = self.parse_expr()?;
                let mut item_span = select.span.merge(expr.span());
                // Handle COLLATION FOR (expr) - special PG syntax
                if matches!(&expr, Expr::Identifier(name) if name.parts.len() == 1 && name.parts[0].eq_ignore_ascii_case("collation"))
                    && matches!(self.current().kind, TokenKind::Keyword(Keyword::For))
                {
                    self.advance(); // consume FOR
                                    // Skip balanced parentheses: FOR (expr)
                    if self.consume_kind(&TokenKind::LParen) {
                        let mut depth = 1u32;
                        while depth > 0 && !self.is_eof() {
                            match self.current().kind {
                                TokenKind::LParen => {
                                    depth += 1;
                                    self.advance();
                                }
                                TokenKind::RParen => {
                                    depth -= 1;
                                    self.advance();
                                }
                                _ => {
                                    self.advance();
                                }
                            }
                        }
                    }
                    item_span = select.span.merge(self.previous_span());
                }
                // Reject WITHIN GROUP until ordered-set aggregate semantics are implemented.
                self.reject_within_group()?;
                let alias = if self.consume_keyword(Keyword::As).is_some() {
                    let (alias, alias_span) = self.expect_identifier()?;
                    item_span = item_span.merge(alias_span);
                    Some(alias)
                } else if self.is_bare_select_alias() {
                    // Bare column alias without AS (e.g., `sum(x) total`)
                    if let Ok((alias, alias_span)) = self.expect_identifier() {
                        item_span = item_span.merge(alias_span);
                        Some(alias)
                    } else {
                        None
                    }
                } else {
                    None
                };

                items.push(SelectItem {
                    expr,
                    alias,
                    span: item_span,
                });

                if !self.consume_kind(&TokenKind::Comma) {
                    break;
                }
            }
        }

        // PG compatibility: empty target list followed by FROM is valid for
        // plain/all selects (`SELECT FROM t`, `SELECT ALL FROM t`) but not for
        // DISTINCT variants (`SELECT DISTINCT FROM t` is a syntax error).
        if items.is_empty()
            && !matches!(distinct, DistinctKind::All)
            && matches!(self.current().kind, TokenKind::Keyword(Keyword::From))
        {
            return self.syntax_error_at_current_token();
        }

        let mut span = items
            .last()
            .map_or(select.span, |item| select.span.merge(item.span));

        if into_name.is_none() && self.consume_keyword(Keyword::Into).is_some() {
            // SELECT items INTO [TEMP|TEMPORARY] [TABLE] name FROM ... → Create Table As
            into_temporary = self.consume_keyword(Keyword::Temp).is_some()
                || self.consume_keyword(Keyword::Temporary).is_some();
            let _ = self.consume_keyword(Keyword::Table); // optional TABLE keyword
            into_name = Some(self.parse_object_name()?);
            // Fall through to parse the remaining SELECT body (FROM, WHERE, etc.)
        }

        let mut extra_ctes: Vec<CteDefinition> = Vec::new();
        let (from, from_alias, from_span) = if self.consume_keyword(Keyword::From).is_some() {
            let _ = self.consume_keyword(Keyword::Only);
            let (relation, alias, ctes, col_aliases) = self.parse_from_table_ref()?;
            span = span.merge(relation.span);
            extra_ctes.extend(ctes);
            if let Some(aliases) = col_aliases {
                if let Some(ref alias_name) = alias {
                    let wrapper_cte =
                        Self::make_column_alias_cte(alias_name, &relation, &aliases, relation.span);
                    extra_ctes.push(wrapper_cte);
                    let aliased_name = ObjectName {
                        parts: vec![alias_name.clone()],
                        span: relation.span,
                    };
                    (Some(aliased_name), None, Some(span))
                } else {
                    (Some(relation), alias, Some(span))
                }
            } else {
                (Some(relation), alias, Some(span))
            }
        } else {
            (None, None, None)
        };

        // Parse joins: explicit (INNER/LEFT/RIGHT/CROSS JOIN) and implicit (FROM t1, t2)
        // Both types can be interleaved: FROM t1 JOIN t2 ON ..., t3, t4 JOIN t5 ON ...
        let mut joins = Vec::new();
        if from.is_some() {
            loop {
                // Handle implicit joins: FROM t1 a, t2 b, t3
                while self.consume_kind(&TokenKind::Comma) {
                    let join_start = self.previous_span();
                    let (mut table, mut alias, ctes, col_aliases) = self.parse_from_table_ref()?;
                    extra_ctes.extend(ctes);
                    // Handle column aliases on join targets (e.g. `v AS v2(x2)`)
                    if let Some(aliases) = col_aliases {
                        if let Some(ref alias_name) = alias {
                            let wrapper_cte = Self::make_column_alias_cte(
                                alias_name, &table, &aliases, table.span,
                            );
                            extra_ctes.push(wrapper_cte);
                            table = ObjectName {
                                parts: vec![alias_name.clone()],
                                span: table.span,
                            };
                            alias = None;
                        }
                    }
                    let join_span = join_start.merge(table.span);
                    span = span.merge(join_span);
                    joins.push(JoinClause {
                        join_type: JoinType::Cross,
                        table,
                        alias,
                        condition: None,
                        using_columns: Vec::new(),
                        using_alias: None,
                        natural: false,
                        span: join_span,
                    });
                }

                // Handle explicit joins
                let join_start_span = self.current().span;
                // Check for NATURAL prefix
                let natural = self.consume_keyword(Keyword::Natural).is_some();
                let join_type = if self.consume_keyword(Keyword::Inner).is_some() {
                    self.expect_keyword(Keyword::Join)?;
                    Some(JoinType::Inner)
                } else if self.consume_keyword(Keyword::Left).is_some() {
                    self.consume_keyword(Keyword::Outer); // optional OUTER
                    self.expect_keyword(Keyword::Join)?;
                    Some(JoinType::Left)
                } else if self.consume_keyword(Keyword::Right).is_some() {
                    self.consume_keyword(Keyword::Outer); // optional OUTER
                    self.expect_keyword(Keyword::Join)?;
                    Some(JoinType::Right)
                } else if self.consume_keyword(Keyword::Full).is_some() {
                    self.consume_keyword(Keyword::Outer); // optional OUTER
                    self.expect_keyword(Keyword::Join)?;
                    Some(JoinType::Full)
                } else if self.consume_keyword(Keyword::Cross).is_some() {
                    self.expect_keyword(Keyword::Join)?;
                    Some(JoinType::Cross)
                } else if self.consume_keyword(Keyword::Join).is_some() {
                    Some(JoinType::Inner) // bare JOIN = INNER JOIN
                } else {
                    if natural {
                        return self.syntax_error_current("expected JOIN after NATURAL");
                    }
                    None
                };

                let Some(jt) = join_type else {
                    break;
                };

                let (mut table, mut alias, ctes, col_aliases) = self.parse_from_table_ref()?;
                extra_ctes.extend(ctes);
                // Handle column aliases on join targets
                if let Some(aliases) = col_aliases {
                    if let Some(ref alias_name) = alias {
                        let wrapper_cte =
                            Self::make_column_alias_cte(alias_name, &table, &aliases, table.span);
                        extra_ctes.push(wrapper_cte);
                        table = ObjectName {
                            parts: vec![alias_name.clone()],
                            span: table.span,
                        };
                        alias = None;
                    }
                }
                let (condition, using_columns, using_alias) = if natural || jt == JoinType::Cross {
                    // NATURAL JOIN and CROSS JOIN have no ON/USING clause
                    (None, Vec::new(), None)
                } else if self.consume_keyword(Keyword::Using).is_some() {
                    // USING (col1, col2, ...) [AS alias]
                    self.expect_token(&TokenKind::LParen)?;
                    let mut cols = Vec::new();
                    loop {
                        let (col_name, _) = self.expect_identifier()?;
                        cols.push(col_name);
                        if !self.consume_kind(&TokenKind::Comma) {
                            break;
                        }
                    }
                    self.expect_token(&TokenKind::RParen)?;
                    let using_alias = if self.consume_keyword(Keyword::As).is_some() {
                        Some(self.expect_identifier()?.0)
                    } else {
                        None
                    };
                    (None, cols, using_alias)
                } else if self.consume_keyword(Keyword::On).is_some() {
                    (Some(self.parse_expr()?), Vec::new(), None)
                } else {
                    // No ON/USING clause - treat as implicit cross join
                    (None, Vec::new(), None)
                };

                let join_span =
                    join_start_span.merge(condition.as_ref().map_or(table.span, |c| c.span()));
                span = span.merge(join_span);
                joins.push(JoinClause {
                    join_type: jt,
                    table,
                    alias,
                    condition,
                    using_columns,
                    using_alias,
                    natural,
                    span: join_span,
                });
            }
        }

        let (selection, where_span) = if let Some(where_token) =
            self.consume_keyword(Keyword::Where)
        {
            let selection = self.parse_expr()?;
            span = span.merge(where_token.span).merge(selection.span());
            // Skip trailing ISNULL/NOTNULL postfix operators (PG extension)
            while matches!(&self.current().kind, TokenKind::Identifier(s) if matches!(s.to_ascii_lowercase().as_str(), "isnull" | "notnull"))
            {
                span = span.merge(self.advance().span);
            }
            (Some(selection), Some(where_token.span))
        } else {
            (None, None)
        };
        let (group_by, group_by_items, group_by_span) =
            if let Some(group_token) = self.consume_keyword(Keyword::Group) {
                let by_token = self.expect_keyword(Keyword::By)?;
                let mut gb_structured = Vec::new();
                let mut gb_span = group_token.span.merge(by_token.span);
                // Skip optional ALL/DISTINCT modifier (PG 16 GROUP BY ALL/DISTINCT)
                if let Some(t) = self.consume_keyword(Keyword::All) {
                    gb_span = gb_span.merge(t.span);
                } else if let Some(t) = self.consume_keyword(Keyword::Distinct) {
                    gb_span = gb_span.merge(t.span);
                }
                loop {
                    if self.is_group_by_set_constructor() {
                        let item = self.parse_group_by_structured_item()?;
                        gb_span = gb_span.merge(self.previous_span());
                        gb_structured.push(item);
                    } else if self.current().kind == TokenKind::LParen
                        && self.index + 1 < self.tokens.len()
                        && self.tokens[self.index + 1].kind == TokenKind::RParen
                    {
                        // GROUP BY () - empty grouping set
                        let lp = self.advance();
                        self.advance(); // consume ')'
                        gb_span = gb_span.merge(lp.span);
                        gb_structured.push(GroupByItem::Empty);
                    } else {
                        let expr = self.parse_expr()?;
                        gb_span = gb_span.merge(expr.span());
                        gb_structured.push(GroupByItem::Plain(expr));
                    }
                    if !self.consume_kind(&TokenKind::Comma) {
                        break;
                    }
                }
                span = span.merge(gb_span);
                let flat = flatten_group_by_items(&gb_structured);
                (flat, gb_structured, Some(gb_span))
            } else {
                (Vec::new(), Vec::new(), None)
            };
        let (having, having_span) =
            if let Some(having_token) = self.consume_keyword(Keyword::Having) {
                let expr = self.parse_expr()?;
                let h_span = having_token.span.merge(expr.span());
                span = span.merge(h_span);
                (Some(expr), Some(h_span))
            } else {
                (None, None)
            };
        // Parse optional WINDOW clause: WINDOW w AS (...), ...
        let mut window_definitions = Vec::new();
        if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("window"))
        {
            self.advance(); // consume WINDOW
            loop {
                if self.is_eof() || matches!(self.current().kind, TokenKind::Semicolon) {
                    break;
                }
                let (win_name, _) = self.expect_identifier()?;
                let _ = self.consume_keyword(Keyword::As);
                // Parse window spec inside parens using a dummy function
                // to reuse parse_window_spec, or parse inline.
                let mut win_partition_by = Vec::new();
                let mut win_order_by = Vec::new();
                if self.consume_kind(&TokenKind::LParen) {
                    // Parse optional base window name reference
                    if matches!(self.current().kind, TokenKind::Identifier(_))
                        && !matches!(
                            &self.current().kind,
                            TokenKind::Identifier(s) if matches!(
                                s.to_ascii_uppercase().as_str(),
                                "PARTITION" | "ORDER" | "ROWS" | "RANGE" | "GROUPS"
                            )
                        )
                    {
                        self.advance(); // consume base window name
                    }
                    if self.consume_keyword(Keyword::Partition).is_some() {
                        self.expect_keyword(Keyword::By)?;
                        loop {
                            win_partition_by.push(self.parse_expr()?);
                            if !self.consume_kind(&TokenKind::Comma) {
                                break;
                            }
                            if matches!(self.current().kind, TokenKind::Keyword(Keyword::Order)) {
                                break;
                            }
                        }
                    }
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
                            win_order_by.push(OrderByItem {
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
                    // Skip frame clause and exclude clause inside WINDOW def
                    if matches!(
                        self.current().kind,
                        TokenKind::Keyword(Keyword::Rows | Keyword::Range | Keyword::Groups)
                    ) {
                        self.advance();
                        self.parse_frame_bound()?;
                    }
                    if self.consume_keyword(Keyword::Exclude).is_some() {
                        if self.consume_keyword(Keyword::Current).is_some() {
                            let _ = self.consume_keyword(Keyword::Row);
                        } else if self.consume_keyword(Keyword::Group).is_some() {
                        } else if self.consume_keyword(Keyword::No).is_some() {
                            let _ = self.advance();
                        } else {
                            self.advance();
                        }
                    }
                    self.expect_token(&TokenKind::RParen)?;
                }
                window_definitions.push(crate::ast::WindowDefinition {
                    name: win_name,
                    partition_by: win_partition_by,
                    order_by: win_order_by,
                });
                if !self.consume_kind(&TokenKind::Comma) {
                    break;
                }
            }
        }

        let (order_by, order_by_span) =
            if let Some(order_token) = self.consume_keyword(Keyword::Order) {
                let by_token = self.expect_keyword(Keyword::By)?;
                let mut items = Vec::new();
                let mut order_span = order_token.span.merge(by_token.span);

                loop {
                    let expr = self.parse_expr()?;
                    let mut item_span = expr.span();
                    let descending = if self.consume_keyword(Keyword::Desc).is_some() {
                        item_span = item_span.merge(self.previous_span());
                        true
                    } else if self.consume_keyword(Keyword::Asc).is_some() {
                        item_span = item_span.merge(self.previous_span());
                        false
                    } else if self.consume_keyword(Keyword::Using).is_some() {
                        // ORDER BY ... USING <operator> - PG extension
                        // Map > to DESC, everything else to ASC
                        let op_token = self.advance();
                        item_span = item_span.merge(op_token.span);
                        matches!(op_token.kind, TokenKind::Gt)
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
                    order_span = order_span.merge(item_span);
                    items.push(OrderByItem {
                        expr,
                        descending,
                        nulls_first,
                        span: item_span,
                    });

                    if !self.consume_kind(&TokenKind::Comma) {
                        break;
                    }
                }

                span = span.merge(order_span);
                (items, Some(order_span))
            } else {
                (Vec::new(), None)
            };
        let mut limit: Option<Expr> = None;
        let mut limit_span: Option<Span> = None;
        let mut offset: Option<Expr> = None;
        let mut offset_span: Option<Span> = None;
        let mut fetch_with_ties = false;
        loop {
            if let Some(limit_token) = self.consume_keyword(Keyword::Limit) {
                if limit_span.is_some() {
                    return self.syntax_error(limit_token.span, "LIMIT specified more than once");
                }
                if self.consume_keyword(Keyword::All).is_some() {
                    // LIMIT ALL = no limit
                    limit = None;
                    limit_span = None;
                } else {
                    let expr = self.parse_expr()?;
                    let span_with_value = limit_token.span.merge(expr.span());
                    span = span.merge(span_with_value);
                    limit = Some(expr);
                    limit_span = Some(span_with_value);
                }
                continue;
            }

            if let Some(offset_token) = self.consume_keyword(Keyword::Offset) {
                if offset_span.is_some() {
                    return self.syntax_error(offset_token.span, "OFFSET specified more than once");
                }
                let expr = self.parse_expr()?;
                let span_with_value = offset_token.span.merge(expr.span());
                span = span.merge(span_with_value);
                // Skip optional ROW/ROWS after OFFSET expr
                let _ = self.consume_keyword(Keyword::Row);
                let _ = self.consume_keyword(Keyword::Rows);
                offset = Some(expr);
                offset_span = Some(span_with_value);
                continue;
            }

            // FETCH {FIRST|NEXT} [count] {ROW|ROWS} {ONLY|WITH TIES}
            if let Some(fetch_token) = self.consume_keyword(Keyword::Fetch) {
                if limit_span.is_some() {
                    return self.syntax_error(
                        fetch_token.span,
                        "LIMIT and FETCH clauses cannot be specified together",
                    );
                }
                // Skip FIRST or NEXT
                let _ = self.consume_keyword(Keyword::First);
                let _ = self.consume_keyword(Keyword::Next);
                // Parse optional count (defaults to 1)
                let (mut count_expr, fetch_span) = if matches!(
                    self.current().kind,
                    TokenKind::Keyword(Keyword::Row | Keyword::Rows)
                ) {
                    // No count specified, default to 1
                    let one = Expr::Literal(crate::ast::Literal::Integer(1), fetch_token.span);
                    (one, fetch_token.span)
                } else {
                    let expr = self.parse_expr()?;
                    let s = fetch_token.span.merge(expr.span());
                    (expr, s)
                };
                // Skip ROW/ROWS
                let _ = self.consume_keyword(Keyword::Row);
                let _ = self.consume_keyword(Keyword::Rows);
                // Accept ONLY or WITH TIES.
                if self.consume_keyword(Keyword::Only).is_none()
                    && self.consume_keyword(Keyword::With).is_some()
                {
                    let (ident, ties_span) = self.expect_identifier()?;
                    if !ident.eq_ignore_ascii_case("ties") {
                        return self.syntax_error_current("expected TIES after WITH");
                    }
                    if order_by.is_empty() {
                        return self.syntax_error(
                            fetch_token.span.merge(ties_span),
                            "WITH TIES cannot be specified without ORDER BY clause",
                        );
                    }
                    if matches!(count_expr, Expr::Literal(crate::ast::Literal::Null, _)) {
                        return self.syntax_error(
                            fetch_token.span.merge(ties_span),
                            "row count cannot be null in FETCH FIRST ... WITH TIES clause",
                        );
                    }
                    // PostgreSQL accepts arbitrary expressions here, including
                    // ones containing NULL constants; preserve compatibility by
                    // treating unknown NULL constants as numeric zeros in this
                    // clause so type resolution succeeds at bind time.
                    count_expr = rewrite_fetch_count_null_literals(count_expr);
                    fetch_with_ties = true;
                }
                span = span.merge(fetch_span);
                limit = Some(count_expr);
                limit_span = Some(fetch_span);
                continue;
            }

            break;
        }

        // Skip row-locking clauses (FOR UPDATE/SHARE/NO KEY UPDATE/KEY SHARE).
        // Only treat FOR as a locking clause if followed by UPDATE/SHARE/NO/KEY/READ
        // (not for e.g. COLLATION FOR (expr) which ends with FOR followed by '(')
        let mut row_lock = None;
        if matches!(self.current().kind, TokenKind::Keyword(Keyword::For)) {
            let saved = self.index;
            let for_token = self.advance(); // consume FOR
            let is_locking = matches!(
                self.current().kind,
                TokenKind::Keyword(Keyword::Update | Keyword::No | Keyword::Key | Keyword::Read)
            ) || matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("share"));
            if is_locking {
                let clause = match &self.current().kind {
                    TokenKind::Keyword(Keyword::Update) => "FOR UPDATE",
                    TokenKind::Keyword(Keyword::No) => "FOR NO KEY UPDATE",
                    TokenKind::Keyword(Keyword::Key) => "FOR KEY SHARE",
                    TokenKind::Keyword(Keyword::Read) => "FOR SHARE",
                    TokenKind::Identifier(s) if s.eq_ignore_ascii_case("share") => "FOR SHARE",
                    _ => "FOR UPDATE",
                };
                if !group_by_items.is_empty() {
                    return Err(aiondb_core::DbError::syntax_error(
                        "FOR UPDATE is not allowed with GROUP BY clause",
                    ));
                }
                if self.set_op_depth > 0 {
                    return Err(aiondb_core::DbError::syntax_error(format!(
                        "{clause} is not allowed with UNION/INTERSECT/EXCEPT"
                    )));
                }
                let mut saw_skip_locked = false;
                // Skip until end of statement, semicolon, or closing paren (subquery boundary)
                while !self.is_eof()
                    && !matches!(
                        self.current().kind,
                        TokenKind::Semicolon | TokenKind::RParen
                    )
                {
                    if matches!(self.current().kind, TokenKind::Keyword(Keyword::Skip))
                        && self.index + 1 < self.tokens.len()
                        && (matches!(
                            self.tokens[self.index + 1].kind,
                            TokenKind::Keyword(Keyword::Lock)
                        ) || matches!(&self.tokens[self.index + 1].kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("locked")))
                    {
                        saw_skip_locked = true;
                    }
                    self.advance();
                }
                if fetch_with_ties && saw_skip_locked {
                    return Err(aiondb_core::DbError::syntax_error(
                        "SKIP LOCKED and WITH TIES options cannot be used together",
                    ));
                }
                row_lock = Some(RowLockClause {
                    skip_locked: saw_skip_locked,
                    span: for_token.span,
                });
            } else {
                // Not a locking clause (e.g. COLLATION FOR), restore position
                self.index = saved;
            }
        }

        // Resolve named window references in select items and ORDER BY.
        let items = resolve_window_refs_in_items(items, &window_definitions);
        let order_by = resolve_window_refs_in_order_by(order_by, &window_definitions);

        let select_stmt = SelectStatement {
            row_lock,
            ctes: extra_ctes,
            distinct,
            items,
            from,
            from_alias,
            from_span,
            joins,
            selection,
            where_span,
            group_by,
            group_by_items,
            group_by_span,
            having,
            having_span,
            window_definitions,
            order_by,
            order_by_span,
            limit,
            limit_span,
            offset,
            offset_span,
            span,
        };

        if let Some(name) = into_name {
            let ctas_span = select.span.merge(select_stmt.span);
            Ok(Statement::CreateTableAs(CreateTableAsStatement {
                name,
                query: select_stmt,
                temporary: into_temporary,
                if_not_exists: false,
                with_no_data: false,
                column_aliases: Vec::new(),
                span: ctas_span,
            }))
        } else {
            Ok(Statement::Select(select_stmt))
        }
    }
}

/// Resolve named window references (OVER w) in select items by looking up
/// the WINDOW clause definitions and merging `partition_by/order_by`.
fn resolve_window_refs_in_items(
    items: Vec<SelectItem>,
    defs: &[crate::ast::WindowDefinition],
) -> Vec<SelectItem> {
    if defs.is_empty() {
        return items;
    }
    items
        .into_iter()
        .map(|item| SelectItem {
            expr: resolve_window_refs_expr(item.expr, defs),
            alias: item.alias,
            span: item.span,
        })
        .collect()
}

fn resolve_window_refs_in_order_by(
    items: Vec<OrderByItem>,
    defs: &[crate::ast::WindowDefinition],
) -> Vec<OrderByItem> {
    if defs.is_empty() {
        return items;
    }
    items
        .into_iter()
        .map(|item| OrderByItem {
            expr: resolve_window_refs_expr(item.expr, defs),
            descending: item.descending,
            nulls_first: item.nulls_first,
            span: item.span,
        })
        .collect()
}

fn resolve_window_refs_expr(expr: Expr, defs: &[crate::ast::WindowDefinition]) -> Expr {
    match expr {
        Expr::WindowFunction {
            function,
            mut partition_by,
            mut order_by,
            window_name,
            span,
        } => {
            // Resolve the window name: merge definition's partition_by/order_by
            if let Some(ref name) = window_name {
                if let Some(def) = defs.iter().find(|d| d.name.eq_ignore_ascii_case(name)) {
                    // The definition's clauses serve as the base; any local
                    // clauses extend them (PG semantics).
                    if partition_by.is_empty() {
                        partition_by.clone_from(&def.partition_by);
                    }
                    if order_by.is_empty() {
                        order_by.clone_from(&def.order_by);
                    }
                }
            }
            // Recursively resolve within the function expression
            Expr::WindowFunction {
                function: Box::new(resolve_window_refs_expr(*function, defs)),
                partition_by: partition_by
                    .into_iter()
                    .map(|e| resolve_window_refs_expr(e, defs))
                    .collect(),
                order_by: order_by
                    .into_iter()
                    .map(|item| OrderByItem {
                        expr: resolve_window_refs_expr(item.expr, defs),
                        descending: item.descending,
                        nulls_first: item.nulls_first,
                        span: item.span,
                    })
                    .collect(),
                window_name: None, // resolved, no longer needed
                span,
            }
        }
        // Recurse into sub-expressions
        Expr::BinaryOp {
            left,
            op,
            right,
            span,
        } => Expr::BinaryOp {
            left: Box::new(resolve_window_refs_expr(*left, defs)),
            op,
            right: Box::new(resolve_window_refs_expr(*right, defs)),
            span,
        },
        Expr::UnaryOp { op, expr, span } => Expr::UnaryOp {
            op,
            expr: Box::new(resolve_window_refs_expr(*expr, defs)),
            span,
        },
        Expr::Cast {
            expr,
            data_type,
            span,
        } => Expr::Cast {
            expr: Box::new(resolve_window_refs_expr(*expr, defs)),
            data_type,
            span,
        },
        Expr::FunctionCall {
            name,
            args,
            distinct,
            filter,
            span,
        } => Expr::FunctionCall {
            name,
            args: args
                .into_iter()
                .map(|a| resolve_window_refs_expr(a, defs))
                .collect(),
            distinct,
            filter: filter.map(|f| Box::new(resolve_window_refs_expr(*f, defs))),
            span,
        },
        Expr::CaseWhen {
            operand,
            conditions,
            results,
            else_result,
            span,
        } => Expr::CaseWhen {
            operand: operand.map(|o| Box::new(resolve_window_refs_expr(*o, defs))),
            conditions: conditions
                .into_iter()
                .map(|c| resolve_window_refs_expr(c, defs))
                .collect(),
            results: results
                .into_iter()
                .map(|r| resolve_window_refs_expr(r, defs))
                .collect(),
            else_result: else_result.map(|e| Box::new(resolve_window_refs_expr(*e, defs))),
            span,
        },
        // Leaf expressions: return unchanged
        other => other,
    }
}

fn rewrite_fetch_count_null_literals(expr: Expr) -> Expr {
    match expr {
        Expr::Literal(crate::ast::Literal::Null, span) => {
            Expr::Literal(crate::ast::Literal::Integer(0), span)
        }
        Expr::UnaryOp { op, expr, span } => Expr::UnaryOp {
            op,
            expr: Box::new(rewrite_fetch_count_null_literals(*expr)),
            span,
        },
        Expr::BinaryOp {
            left,
            op,
            right,
            span,
        } => Expr::BinaryOp {
            left: Box::new(rewrite_fetch_count_null_literals(*left)),
            op,
            right: Box::new(rewrite_fetch_count_null_literals(*right)),
            span,
        },
        Expr::FunctionCall {
            name,
            args,
            distinct,
            filter,
            span,
        } => Expr::FunctionCall {
            name,
            args: args
                .into_iter()
                .map(rewrite_fetch_count_null_literals)
                .collect(),
            distinct,
            filter: filter.map(|expr| Box::new(rewrite_fetch_count_null_literals(*expr))),
            span,
        },
        Expr::CaseWhen {
            operand,
            conditions,
            results,
            else_result,
            span,
        } => Expr::CaseWhen {
            operand: operand.map(|expr| Box::new(rewrite_fetch_count_null_literals(*expr))),
            conditions: conditions
                .into_iter()
                .map(rewrite_fetch_count_null_literals)
                .collect(),
            results: results
                .into_iter()
                .map(rewrite_fetch_count_null_literals)
                .collect(),
            else_result: else_result.map(|expr| Box::new(rewrite_fetch_count_null_literals(*expr))),
            span,
        },
        other => other,
    }
}
