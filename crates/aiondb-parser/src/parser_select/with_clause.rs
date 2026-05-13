use super::*;
use crate::ast::BinaryOperator;

impl Parser {
    /// Parse: `WITH [RECURSIVE] name [(col1, col2)] AS ( select ) [, ...] SELECT ...`
    pub(crate) fn parse_with_statement(&mut self) -> DbResult<Statement> {
        let with_token = self.expect_keyword(Keyword::With)?;
        // Track whether RECURSIVE was specified
        let is_recursive = self.consume_keyword(Keyword::Recursive).is_some();
        let mut ctes = Vec::new();

        loop {
            let (name, name_span) = self.expect_identifier()?;

            // Parse optional column aliases: `name(col1, col2)`
            let mut column_aliases = if self.consume_kind(&TokenKind::LParen) {
                let mut aliases = Vec::new();
                loop {
                    let (alias, _) = self.expect_identifier()?;
                    aliases.push(alias);
                    if !self.consume_kind(&TokenKind::Comma) {
                        break;
                    }
                }
                self.expect_token(&TokenKind::RParen)?;
                Some(aliases)
            } else {
                None
            };

            self.expect_keyword(Keyword::As)?;
            // Skip optional MATERIALIZED / NOT MATERIALIZED
            let _ = self.consume_keyword(Keyword::Not);
            let _ = self.consume_keyword(Keyword::Materialized);
            self.expect_token(&TokenKind::LParen)?;

            // Parse the CTE's inner statement (SELECT, VALUES, INSERT, UPDATE, DELETE).
            // Inject earlier CTEs so later CTEs can reference them.
            // Also handles set operations (UNION/INTERSECT/EXCEPT) inside CTE body.
            let inner_stmt = if matches!(self.current().kind, TokenKind::Keyword(Keyword::Values)) {
                let vs_stmt = self.parse_values_as_statement()?;
                self.maybe_parse_set_operation(vs_stmt)?
            } else if matches!(self.current().kind, TokenKind::Keyword(Keyword::With)) {
                let with_stmt = self.parse_with_statement()?;
                self.maybe_parse_set_operation(with_stmt)?
            } else if matches!(self.current().kind, TokenKind::Keyword(Keyword::Select)) {
                let select_stmt = self.parse_select_statement()?;
                self.maybe_parse_set_operation(select_stmt)?
            } else if matches!(
                self.current().kind,
                TokenKind::Keyword(Keyword::Insert | Keyword::Update | Keyword::Delete)
            ) {
                match self.current().kind {
                    TokenKind::Keyword(Keyword::Insert) => self.parse_insert_statement()?,
                    TokenKind::Keyword(Keyword::Update) => self.parse_update_statement()?,
                    TokenKind::Keyword(Keyword::Delete) => self.parse_delete_statement()?,
                    _ => unreachable!("keyword guard ensures only INSERT/UPDATE/DELETE"),
                }
            } else if matches!(self.current().kind, TokenKind::LParen) {
                // Parenthesized subquery inside CTE body: (SELECT ...)
                self.advance(); // consume '('
                let mut extra_parens = 0u32;
                while matches!(self.current().kind, TokenKind::LParen) {
                    self.advance();
                    extra_parens += 1;
                }
                let inner = if matches!(self.current().kind, TokenKind::Keyword(Keyword::Select)) {
                    let sel = self.parse_select_statement()?;
                    self.maybe_parse_set_operation(sel)?
                } else if matches!(self.current().kind, TokenKind::Keyword(Keyword::Values)) {
                    let vs = self.parse_values_as_statement()?;
                    self.maybe_parse_set_operation(vs)?
                } else if matches!(self.current().kind, TokenKind::Keyword(Keyword::With)) {
                    self.parse_with_statement()?
                } else {
                    self.parse_select_statement()?
                };
                for _ in 0..extra_parens {
                    let _ = self.consume_kind(&TokenKind::RParen);
                }
                // Consume the parenthesized arm closer when it is followed by
                // another ')' (CTE-body close) or by a set-op keyword (this
                // was a parenthesized set-op arm like `(SELECT ...) UNION ...`).
                // Otherwise leave it for the caller as the CTE-body closer.
                if matches!(self.current().kind, TokenKind::RParen)
                    && matches!(
                        self.tokens.get(self.index + 1).map(|token| &token.kind),
                        Some(TokenKind::RParen)
                            | Some(TokenKind::Keyword(
                                Keyword::Union | Keyword::Except | Keyword::Intersect
                            ))
                    )
                {
                    self.advance();
                }
                inner
            } else {
                self.parse_select_statement()?
            };
            // Handle set operations at the CTE body level:
            // e.g. (VALUES (...) UNION ALL SELECT ...) or ((SELECT ...) UNION (SELECT ...))
            let inner_stmt = self.maybe_parse_set_operation(inner_stmt)?;

            // For recursive CTEs, preserve both sides of the UNION [ALL].
            // The left side is the base (non-recursive) term and the right
            // side is the recursive term that references the CTE itself.
            let (mut query, mut recursive_term, union_all, recursive_union_sides_select) =
                if is_recursive {
                    match inner_stmt {
                        Statement::SetOperation(set_op)
                            if matches!(set_op.op, SetOperationType::Union) =>
                        {
                            let left_contains_self =
                                statement_contains_recursive_ref(set_op.left.as_ref(), &name);
                            let right_contains_self =
                                statement_contains_recursive_ref(set_op.right.as_ref(), &name);

                            // WITH RECURSIVE can contain non-recursive CTEs. Only split
                            // UNION into base/recursive terms when this CTE actually
                            // references itself on either side (the right side is the
                            // valid recursive location; left-side refs are validated later).
                            if !left_contains_self && !right_contains_self {
                                (Statement::SetOperation(set_op), None, false, None)
                            } else {
                                // PG accepts ORDER BY / LIMIT / OFFSET attached
                                // to a recursive CTE's UNION; they become
                                // (or ignored) downstream.

                                let base_stmt = *set_op.left;
                                let left_is_select = matches!(base_stmt, Statement::Select(_));
                                if !matches!(
                                    base_stmt,
                                    Statement::Select(_) | Statement::SetOperation(_)
                                ) {
                                    return self.syntax_error(
                                        name_span,
                                        "CTE body must be a SELECT statement",
                                    );
                                }

                                let recursive_stmt = *set_op.right;
                                let right_is_select =
                                    matches!(recursive_stmt, Statement::Select(_));
                                if !matches!(
                                    recursive_stmt,
                                    Statement::Select(_) | Statement::SetOperation(_)
                                ) {
                                    return self.syntax_error(
                                        name_span,
                                        "CTE body must be a SELECT statement",
                                    );
                                }

                                let recursive =
                                    self.wrap_recursive_term_as_select(recursive_stmt, name_span)?;
                                (
                                    base_stmt,
                                    Some(Box::new(recursive)),
                                    set_op.all,
                                    Some((left_is_select, right_is_select)),
                                )
                            }
                        }
                        other => {
                            // RECURSIVE applies to the whole WITH list; a given CTE can
                            // still be non-recursive and data-modifying.
                            if !matches!(
                                other,
                                Statement::Select(_)
                                    | Statement::SetOperation(_)
                                    | Statement::Insert(_)
                                    | Statement::Update(_)
                                    | Statement::Delete(_)
                            ) {
                                return self.syntax_error(
                                    name_span,
                                    "CTE body must be a SELECT statement",
                                );
                            }
                            (other, None, false, None)
                        }
                    }
                } else {
                    let query = inner_stmt;
                    if !matches!(
                        query,
                        Statement::Select(_)
                            | Statement::SetOperation(_)
                            | Statement::Insert(_)
                            | Statement::Update(_)
                            | Statement::Delete(_)
                    ) {
                        return self.syntax_error(name_span, "CTE body must be a SELECT statement");
                    }
                    (query, None, false, None)
                };
            let rparen = self.expect_token(&TokenKind::RParen)?;
            let cte_span = with_token.span.merge(rparen.span);

            let search_clause = self.parse_cte_search_clause()?;
            let cycle_clause = self.parse_cte_cycle_clause()?;
            self.validate_recursive_cte_search_cycle(
                &name,
                column_aliases.as_deref(),
                recursive_union_sides_select,
                recursive_term.as_deref(),
                search_clause.as_ref(),
                cycle_clause.as_ref(),
            )?;
            if let Some(search) = search_clause.as_ref() {
                self.rewrite_recursive_cte_search_clause(
                    &name,
                    &mut column_aliases,
                    &mut query,
                    recursive_term.as_mut(),
                    search,
                )?;
            }
            if let Some(cycle) = cycle_clause.as_ref() {
                self.rewrite_recursive_cte_cycle_clause(
                    &name,
                    &mut column_aliases,
                    &mut query,
                    recursive_term.as_mut(),
                    cycle,
                )?;
            }

            if ctes
                .iter()
                .any(|cte: &CteDefinition| cte.name.eq_ignore_ascii_case(&name))
            {
                return self.syntax_error(
                    name_span,
                    format!("WITH query name \"{name}\" specified more than once"),
                );
            }

            // A CTE inside `WITH RECURSIVE` is only actually recursive when
            // its body references itself at an unshadowed position. A
            // same-name CTE nested inside the body shadows the outer binding
            // and breaks the recursive link.
            let actually_recursive = is_recursive
                && (recursive_term.is_some()
                    || statement_contains_unshadowed_recursive_ref(&query, &name));
            ctes.push(CteDefinition {
                name,
                column_aliases,
                // `recursive` reflects actual self-reference, not just the
                // surface `WITH RECURSIVE` keyword. Shadowed inner CTEs flip
                // this back to false.
                recursive: actually_recursive,
                query: Box::new(query),
                recursive_term,
                union_all,
                span: cte_span,
            });

            if !self.consume_kind(&TokenKind::Comma) {
                break;
            }
        }

        // Now parse the main SELECT/VALUES/INSERT/UPDATE/DELETE, attaching the CTEs
        let main_stmt = if matches!(self.current().kind, TokenKind::Keyword(Keyword::Values)) {
            let values = self.parse_values_as_statement()?;
            self.maybe_parse_set_operation(values)?
        } else if matches!(self.current().kind, TokenKind::Keyword(Keyword::Insert)) {
            self.parse_insert_statement()?
        } else if matches!(self.current().kind, TokenKind::Keyword(Keyword::Update)) {
            self.parse_update_statement()?
        } else if matches!(self.current().kind, TokenKind::Keyword(Keyword::Delete)) {
            self.parse_delete_statement()?
        } else if matches!(self.current().kind, TokenKind::Keyword(Keyword::Select)) {
            let select = self.parse_select_statement()?;
            self.maybe_parse_set_operation(select)?
        } else {
            if is_recursive && matches!(self.current().kind, TokenKind::Keyword(Keyword::Merge)) {
                return self.unsupported_feature(
                    with_token.span,
                    "WITH RECURSIVE is not supported for MERGE statement",
                );
            }
            return self.unsupported_feature(
                with_token.span,
                "WITH ... followed by this statement type is not supported",
            );
        };
        match main_stmt {
            Statement::Select(mut s) => {
                // Merge WITH-clause CTEs with any inline CTEs (e.g. column-alias
                // wrapper CTEs created by `make_column_alias_cte`) that the inner
                // SELECT already carries.  The WITH-clause CTEs go first so that
                // wrapper CTEs (which may reference them) can resolve properly.
                let mut all_ctes = ctes;
                all_ctes.extend(s.ctes);
                s.ctes = all_ctes;
                s.span = with_token.span.merge(s.span);
                Ok(Statement::Select(s))
            }
            Statement::Insert(mut ins) => {
                // Attach CTEs to the INSERT's inner SELECT query so that
                // CTE names can be resolved (e.g. `WITH result AS (...) INSERT INTO t SELECT * FROM result`).
                if let Some(ref mut q) = ins.query {
                    let mut all_ctes = ctes;
                    all_ctes.extend(std::mem::take(&mut q.ctes));
                    q.ctes = all_ctes;
                }
                Ok(Statement::Insert(ins))
            }
            Statement::SetOperation(mut set_op) => {
                // `WITH ... <set-op>` scope applies to the whole set operation.
                // Attach CTEs to the leftmost branch so name resolution starts
                // from a branch that carries the full WITH scope.
                set_op.left = Box::new(attach_with_ctes_to_leftmost_branch(*set_op.left, &ctes));
                Ok(Statement::SetOperation(set_op))
            }
            Statement::Update(mut update) => {
                // Attach the WITH-clause CTEs so the binder can resolve
                // them in `from_tables`. Without this the CTE name
                // (`picked` in `WITH picked AS (...) UPDATE t FROM
                // picked …`) is silently dropped and resolution falls
                // through to the catalog, returning
                // `relation "picked" does not exist`.
                let mut all_ctes = ctes;
                all_ctes.extend(std::mem::take(&mut update.ctes));
                update.ctes = all_ctes;
                update.span = with_token.span.merge(update.span);
                Ok(Statement::Update(update))
            }
            other => {
                // For data-modifying CTEs (UPDATE/DELETE after WITH),
                // return the statement directly.
                Ok(other)
            }
        }
    }

    fn wrap_recursive_term_as_select(
        &self,
        statement: Statement,
        span: Span,
    ) -> DbResult<SelectStatement> {
        if let Statement::Select(select) = statement {
            return Ok(select);
        }

        let wrapper_name = "__aiondb_recursive_term".to_owned();
        Ok(SelectStatement {
            row_lock: None,
            ctes: vec![CteDefinition {
                name: wrapper_name.clone(),
                column_aliases: None,
                recursive: false,
                query: Box::new(statement),
                recursive_term: None,
                union_all: false,
                span,
            }],
            distinct: DistinctKind::All,
            items: vec![SelectItem {
                expr: Expr::Identifier(ObjectName {
                    parts: vec!["*".to_owned()],
                    span,
                }),
                alias: None,
                span,
            }],
            from: Some(ObjectName {
                parts: vec![wrapper_name],
                span,
            }),
            from_alias: None,
            from_span: Some(span),
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
            span,
        })
    }

    /// Recursively extract the leftmost `SelectStatement` from a `Statement`
    /// which may be deeply nested `SetOperation` chains (e.g. chained UNIONs).
    pub(crate) fn extract_leftmost_select(stmt: Statement) -> Option<SelectStatement> {
        match stmt {
            Statement::Select(s) => Some(s),
            Statement::SetOperation(set_op) => Self::extract_leftmost_select(*set_op.left),
            _ => None,
        }
    }

    fn parse_cte_search_clause(&mut self) -> DbResult<Option<CteSearchClauseSpec>> {
        let Some(search_tok) = self.consume_identifier_or_keyword_ci("search") else {
            return Ok(None);
        };

        let order = if self.consume_identifier_or_keyword_ci("depth").is_some() {
            CteSearchOrder::DepthFirst
        } else if self.consume_identifier_or_keyword_ci("breadth").is_some() {
            CteSearchOrder::BreadthFirst
        } else {
            return self.syntax_error(search_tok.span, "expected DEPTH or BREADTH after SEARCH");
        };
        if self.consume_identifier_or_keyword_ci("first").is_none() {
            return self.syntax_error(search_tok.span, "expected FIRST after SEARCH DEPTH/BREADTH");
        }
        if self.consume_identifier_or_keyword_ci("by").is_none() {
            return self.syntax_error(search_tok.span, "expected BY in SEARCH clause");
        }

        let mut by_columns = Vec::new();
        loop {
            let (column, span) = self.expect_identifier()?;
            by_columns.push((column, span));
            if self.consume_kind(&TokenKind::Comma) {
                continue;
            }
            break;
        }

        self.expect_keyword(Keyword::Set)?;
        let (sequence_column, sequence_span) = self.expect_identifier()?;
        Ok(Some(CteSearchClauseSpec {
            order,
            by_columns,
            sequence_column,
            span: search_tok.span.merge(sequence_span),
        }))
    }

    fn parse_cte_cycle_clause(&mut self) -> DbResult<Option<CteCycleClauseSpec>> {
        let Some(cycle_tok) = self
            .consume_keyword(Keyword::Cycle)
            .or_else(|| self.consume_identifier_or_keyword_ci("cycle"))
        else {
            return Ok(None);
        };

        let mut columns = Vec::new();
        loop {
            let (column, span) = self.expect_identifier()?;
            columns.push((column, span));
            if self.consume_kind(&TokenKind::Comma) {
                continue;
            }
            break;
        }

        self.expect_keyword(Keyword::Set)?;
        let (mark_column, _mark_span) = self.expect_identifier()?;

        let mark_value = if self.consume_keyword(Keyword::To).is_some() {
            Some(self.parse_expr()?)
        } else {
            None
        };
        let default_value = if self.consume_keyword(Keyword::Default).is_some() {
            Some(self.parse_expr()?)
        } else {
            None
        };

        self.expect_keyword(Keyword::Using)?;
        let (path_column, path_span) = self.expect_identifier()?;

        Ok(Some(CteCycleClauseSpec {
            columns,
            mark_column,
            mark_value,
            default_value,
            path_column,
            span: cycle_tok.span.merge(path_span),
        }))
    }

    fn validate_recursive_cte_search_cycle(
        &self,
        cte_name: &str,
        column_aliases: Option<&[String]>,
        recursive_union_sides_select: Option<(bool, bool)>,
        recursive_term: Option<&SelectStatement>,
        search_clause: Option<&CteSearchClauseSpec>,
        cycle_clause: Option<&CteCycleClauseSpec>,
    ) -> DbResult<()> {
        if search_clause.is_none() && cycle_clause.is_none() {
            return Ok(());
        }

        if let Some((left_is_select, right_is_select)) = recursive_union_sides_select {
            if !left_is_select {
                let span = search_clause
                    .map(|c| c.span)
                    .or_else(|| cycle_clause.map(|c| c.span))
                    .unwrap_or_default();
                return self.syntax_error(
                    span,
                    "with a SEARCH or CYCLE clause, the left side of the UNION must be a SELECT",
                );
            }
            if !right_is_select {
                let span = search_clause
                    .map(|c| c.span)
                    .or_else(|| cycle_clause.map(|c| c.span))
                    .unwrap_or_default();
                return self.syntax_error(
                    span,
                    "with a SEARCH or CYCLE clause, the right side of the UNION must be a SELECT",
                );
            }
        }

        let with_columns: Vec<String> = column_aliases.map_or_else(Vec::new, |cols| cols.to_vec());
        let mut with_columns_lower = std::collections::HashSet::new();
        for column in &with_columns {
            with_columns_lower.insert(column.to_ascii_lowercase());
        }

        if let Some(search) = search_clause {
            let mut seen = std::collections::HashSet::new();
            for (column, _span) in &search.by_columns {
                let lowered = column.to_ascii_lowercase();
                if !seen.insert(lowered.clone()) {
                    return self.syntax_error(
                        search.span,
                        format!("search column \"{column}\" specified more than once"),
                    );
                }
                if !with_columns.is_empty() && !with_columns_lower.contains(&lowered) {
                    return self.syntax_error(
                        search.span,
                        format!("search column \"{column}\" not in WITH query column list"),
                    );
                }
            }
            if with_columns_lower.contains(&search.sequence_column.to_ascii_lowercase()) {
                return self.syntax_error(
                    search.span,
                    format!(
                        "search sequence column name \"{}\" already used in WITH query column list",
                        search.sequence_column
                    ),
                );
            }
        }

        if let Some(cycle) = cycle_clause {
            let mut seen = std::collections::HashSet::new();
            for (column, _span) in &cycle.columns {
                let lowered = column.to_ascii_lowercase();
                if !seen.insert(lowered.clone()) {
                    return self.syntax_error(
                        cycle.span,
                        format!("cycle column \"{column}\" specified more than once"),
                    );
                }
                if !with_columns.is_empty() && !with_columns_lower.contains(&lowered) {
                    return self.syntax_error(
                        cycle.span,
                        format!("cycle column \"{column}\" not in WITH query column list"),
                    );
                }
            }

            if with_columns_lower.contains(&cycle.mark_column.to_ascii_lowercase()) {
                return self.syntax_error(
                    cycle.span,
                    format!(
                        "cycle mark column name \"{}\" already used in WITH query column list",
                        cycle.mark_column
                    ),
                );
            }
            if with_columns_lower.contains(&cycle.path_column.to_ascii_lowercase()) {
                return self.syntax_error(
                    cycle.span,
                    format!(
                        "cycle path column name \"{}\" already used in WITH query column list",
                        cycle.path_column
                    ),
                );
            }
            if cycle.mark_column.eq_ignore_ascii_case(&cycle.path_column) {
                return self.syntax_error(
                    cycle.span,
                    "cycle mark column name and cycle path column name are the same",
                );
            }
            if let (Some(mark), Some(default_value)) = (&cycle.mark_value, &cycle.default_value) {
                if let (Some(mark_type), Some(default_type)) =
                    (literal_type_name(mark), literal_type_name(default_value))
                {
                    if mark_type != default_type {
                        return self.syntax_error(
                            default_value.span(),
                            format!("CYCLE types {mark_type} and {default_type} cannot be matched"),
                        );
                    }
                }
            }
        }

        if let (Some(search), Some(cycle)) = (search_clause, cycle_clause) {
            if search
                .sequence_column
                .eq_ignore_ascii_case(&cycle.mark_column)
            {
                return self.syntax_error(
                    search.span,
                    "search sequence column name and cycle mark column name are the same",
                );
            }
            if search
                .sequence_column
                .eq_ignore_ascii_case(&cycle.path_column)
            {
                return self.syntax_error(
                    search.span,
                    "search sequence column name and cycle path column name are the same",
                );
            }
        }

        if let Some(recursive) = recursive_term {
            let has_top_level_ref = select_has_top_level_recursive_ref(recursive, cte_name);
            if select_contains_recursive_ref(recursive, cte_name) && !has_top_level_ref {
                let span = search_clause
                    .map(|c| c.span)
                    .or_else(|| cycle_clause.map(|c| c.span))
                    .unwrap_or(recursive.span);
                return self.syntax_error(
                    span,
                    format!(
                        "with a SEARCH or CYCLE clause, the recursive reference to WITH query \"{cte_name}\" must be at the top level of its right-hand SELECT"
                    ),
                );
            }
        }

        Ok(())
    }

    fn rewrite_recursive_cte_cycle_clause(
        &self,
        cte_name: &str,
        column_aliases: &mut Option<Vec<String>>,
        base_query: &mut Statement,
        recursive_term: Option<&mut Box<SelectStatement>>,
        cycle: &CteCycleClauseSpec,
    ) -> DbResult<()> {
        if column_aliases.is_none() {
            *column_aliases = infer_cte_column_aliases_from_base_query(base_query);
        }
        let Some(column_aliases_vec) = column_aliases.as_mut() else {
            return self.syntax_error(
                cycle.span,
                format!(
                    "WITH RECURSIVE query \"{cte_name}\" must specify a column list when using CYCLE",
                ),
            );
        };
        let Some(recursive_term) = recursive_term else {
            return self.syntax_error(cycle.span, "CYCLE clause requires a recursive query");
        };
        let Statement::Select(base_select) = base_query else {
            return self.syntax_error(
                cycle.span,
                "CYCLE clause requires a SELECT non-recursive term",
            );
        };

        let row_args: Vec<Expr> = cycle
            .columns
            .iter()
            .map(|(column, _)| {
                Expr::Identifier(ObjectName {
                    parts: vec![column.clone()],
                    span: cycle.span,
                })
            })
            .collect();
        let row_expr = Expr::FunctionCall {
            name: ObjectName {
                parts: vec!["row".to_owned()],
                span: cycle.span,
            },
            args: row_args.clone(),
            distinct: false,
            filter: None,
            span: cycle.span,
        };
        let any_path_expr = Expr::FunctionCall {
            name: ObjectName {
                parts: vec!["any".to_owned()],
                span: cycle.span,
            },
            args: vec![Expr::Identifier(ObjectName {
                parts: vec![cycle.path_column.clone()],
                span: cycle.span,
            })],
            distinct: false,
            filter: None,
            span: cycle.span,
        };
        let cycle_detected_expr = Expr::BinaryOp {
            left: Box::new(row_expr.clone()),
            op: BinaryOperator::Eq,
            right: Box::new(any_path_expr),
            span: cycle.span,
        };

        let mark_value = cycle
            .mark_value
            .clone()
            .unwrap_or(Expr::Literal(Literal::Boolean(true), cycle.span));
        let default_value = cycle
            .default_value
            .clone()
            .unwrap_or(Expr::Literal(Literal::Boolean(false), cycle.span));

        // Base term:
        //   mark_column := default_value
        //   path_column := ARRAY[ROW(cycle_cols...)]
        base_select.items.push(SelectItem {
            expr: default_value.clone(),
            alias: None,
            span: cycle.span,
        });
        base_select.items.push(SelectItem {
            expr: Expr::Array {
                elements: vec![row_expr.clone()],
                span: cycle.span,
            },
            alias: None,
            span: cycle.span,
        });

        // Recursive term:
        //   mark_column := CASE WHEN ROW(cycle_cols...) = ANY(path) THEN mark_value ELSE default_value END
        //   path_column := path || ROW(cycle_cols...)
        recursive_term.items.push(SelectItem {
            expr: Expr::CaseWhen {
                operand: None,
                conditions: vec![cycle_detected_expr],
                results: vec![mark_value],
                else_result: Some(Box::new(default_value.clone())),
                span: cycle.span,
            },
            alias: None,
            span: cycle.span,
        });
        recursive_term.items.push(SelectItem {
            expr: Expr::BinaryOp {
                left: Box::new(Expr::Identifier(ObjectName {
                    parts: vec![cycle.path_column.clone()],
                    span: cycle.span,
                })),
                op: BinaryOperator::Concat,
                right: Box::new(row_expr),
                span: cycle.span,
            },
            alias: None,
            span: cycle.span,
        });

        // Continue recursion only while we are at the default mark value.
        let continue_expr = Expr::BinaryOp {
            left: Box::new(Expr::Identifier(ObjectName {
                parts: vec![cycle.mark_column.clone()],
                span: cycle.span,
            })),
            op: BinaryOperator::Eq,
            right: Box::new(default_value),
            span: cycle.span,
        };
        recursive_term.selection = Some(match recursive_term.selection.take() {
            Some(existing) => Expr::BinaryOp {
                left: Box::new(existing),
                op: BinaryOperator::And,
                right: Box::new(continue_expr),
                span: cycle.span,
            },
            None => continue_expr,
        });
        recursive_term.where_span = Some(cycle.span);

        column_aliases_vec.push(cycle.mark_column.clone());
        column_aliases_vec.push(cycle.path_column.clone());
        Ok(())
    }

    fn rewrite_recursive_cte_search_clause(
        &self,
        cte_name: &str,
        column_aliases: &mut Option<Vec<String>>,
        base_query: &mut Statement,
        recursive_term: Option<&mut Box<SelectStatement>>,
        search: &CteSearchClauseSpec,
    ) -> DbResult<()> {
        if column_aliases.is_none() {
            *column_aliases = infer_cte_column_aliases_from_base_query(base_query);
        }
        let with_columns = column_aliases.as_deref().unwrap_or(&[]);
        let alias_index: std::collections::HashMap<String, usize> = with_columns
            .iter()
            .enumerate()
            .map(|(index, name)| (name.to_ascii_lowercase(), index))
            .collect();

        let Some(recursive_term) = recursive_term else {
            return self.syntax_error(search.span, "SEARCH clause requires a recursive query");
        };
        let Statement::Select(base_select) = base_query else {
            return self.syntax_error(
                search.span,
                "SEARCH clause requires a SELECT non-recursive term",
            );
        };

        let base_by_exprs = search
            .by_columns
            .iter()
            .map(|(column, _)| {
                map_search_column_expr(
                    base_select,
                    alias_index.get(&column.to_ascii_lowercase()).copied(),
                    column,
                    search.span,
                )
            })
            .collect::<Vec<_>>();
        let recursive_by_exprs = search
            .by_columns
            .iter()
            .map(|(column, _)| {
                map_search_column_expr(
                    recursive_term,
                    alias_index.get(&column.to_ascii_lowercase()).copied(),
                    column,
                    search.span,
                )
            })
            .collect::<Vec<_>>();

        let make_row_expr = |args: Vec<Expr>| Expr::FunctionCall {
            name: ObjectName {
                parts: vec!["row".to_owned()],
                span: search.span,
            },
            args,
            distinct: false,
            filter: None,
            span: search.span,
        };
        let sequence_ref = || {
            Expr::Identifier(ObjectName {
                parts: vec![search.sequence_column.clone()],
                span: search.span,
            })
        };

        let (base_sequence_expr, recursive_sequence_expr) = match search.order {
            CteSearchOrder::DepthFirst => {
                let base_row = make_row_expr(base_by_exprs);
                let recursive_row = make_row_expr(recursive_by_exprs);
                (
                    Expr::Array {
                        elements: vec![base_row],
                        span: search.span,
                    },
                    Expr::BinaryOp {
                        left: Box::new(sequence_ref()),
                        op: BinaryOperator::Concat,
                        right: Box::new(recursive_row),
                        span: search.span,
                    },
                )
            }
            CteSearchOrder::BreadthFirst => {
                let depth_expr = Expr::BinaryOp {
                    left: Box::new(Expr::Cast {
                        expr: Box::new(Expr::FunctionCall {
                            name: ObjectName {
                                parts: vec!["__aiondb_composite_field".to_owned()],
                                span: search.span,
                            },
                            args: vec![
                                sequence_ref(),
                                Expr::Literal(Literal::String("x".to_owned()), search.span),
                            ],
                            distinct: false,
                            filter: None,
                            span: search.span,
                        }),
                        data_type: aiondb_core::DataType::BigInt,
                        span: search.span,
                    }),
                    op: BinaryOperator::Add,
                    right: Box::new(Expr::Cast {
                        expr: Box::new(Expr::Literal(Literal::Integer(1), search.span)),
                        data_type: aiondb_core::DataType::BigInt,
                        span: search.span,
                    }),
                    span: search.span,
                };

                let mut base_row_args = Vec::with_capacity(base_by_exprs.len().saturating_add(1));
                base_row_args.push(Expr::Cast {
                    expr: Box::new(Expr::Literal(Literal::Integer(0), search.span)),
                    data_type: aiondb_core::DataType::BigInt,
                    span: search.span,
                });
                base_row_args.extend(base_by_exprs);

                let mut recursive_row_args =
                    Vec::with_capacity(recursive_by_exprs.len().saturating_add(1));
                recursive_row_args.push(depth_expr);
                recursive_row_args.extend(recursive_by_exprs);

                (
                    make_row_expr(base_row_args),
                    make_row_expr(recursive_row_args),
                )
            }
        };

        base_select.items.push(SelectItem {
            expr: base_sequence_expr,
            alias: None,
            span: search.span,
        });
        recursive_term.items.push(SelectItem {
            expr: recursive_sequence_expr,
            alias: None,
            span: search.span,
        });

        let Some(column_aliases_vec) = column_aliases.as_mut() else {
            return self.syntax_error(
                search.span,
                format!(
                    "WITH RECURSIVE query \"{cte_name}\" must specify a column list when using SEARCH",
                ),
            );
        };
        column_aliases_vec.push(search.sequence_column.clone());
        Ok(())
    }

    fn consume_identifier_or_keyword_ci(&mut self, expected: &str) -> Option<crate::tokens::Token> {
        match &self.current().kind {
            TokenKind::Identifier(value) if value.eq_ignore_ascii_case(expected) => {
                Some(self.advance())
            }
            TokenKind::Keyword(keyword) if keyword.name().eq_ignore_ascii_case(expected) => {
                Some(self.advance())
            }
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
struct CteSearchClauseSpec {
    order: CteSearchOrder,
    by_columns: Vec<(String, Span)>,
    sequence_column: String,
    span: Span,
}

#[derive(Clone, Copy, Debug)]
enum CteSearchOrder {
    DepthFirst,
    BreadthFirst,
}

#[derive(Clone, Debug)]
struct CteCycleClauseSpec {
    columns: Vec<(String, Span)>,
    mark_column: String,
    mark_value: Option<Expr>,
    default_value: Option<Expr>,
    path_column: String,
    span: Span,
}

fn infer_cte_column_aliases_from_base_query(base_query: &Statement) -> Option<Vec<String>> {
    let Statement::Select(select) = base_query else {
        return None;
    };
    let mut aliases = Vec::with_capacity(select.items.len());
    for (index, item) in select.items.iter().enumerate() {
        if let Some(alias) = &item.alias {
            aliases.push(alias.clone());
            continue;
        }
        match &item.expr {
            Expr::Identifier(identifier)
                if identifier.parts.len() == 1 && identifier.parts[0] != "*" =>
            {
                aliases.push(identifier.parts[0].clone());
            }
            _ => aliases.push(format!("column{}", index + 1)),
        }
    }
    Some(aliases)
}

fn map_search_column_expr(
    select: &SelectStatement,
    column_index: Option<usize>,
    column_name: &str,
    span: Span,
) -> Expr {
    if select.items.len() == 1 {
        if let Some(mapped) = map_star_item_to_column(&select.items[0].expr, column_name, span) {
            return mapped;
        }
    }

    if let Some(index) = column_index {
        if let Some(item) = select.items.get(index) {
            if let Some(mapped) = map_star_item_to_column(&item.expr, column_name, span) {
                return mapped;
            }
            return item.expr.clone();
        }
    }

    Expr::Identifier(ObjectName {
        parts: vec![column_name.to_owned()],
        span,
    })
}

fn map_star_item_to_column(expr: &Expr, column_name: &str, span: Span) -> Option<Expr> {
    let Expr::Identifier(identifier) = expr else {
        return None;
    };
    let last = identifier.parts.last()?;
    if last != "*" {
        return None;
    }
    if identifier.parts.len() == 1 {
        return Some(Expr::Identifier(ObjectName {
            parts: vec![column_name.to_owned()],
            span,
        }));
    }
    let mut parts = identifier.parts[..identifier.parts.len() - 1].to_vec();
    parts.push(column_name.to_owned());
    Some(Expr::Identifier(ObjectName { parts, span }))
}

fn literal_type_name(expr: &Expr) -> Option<&'static str> {
    match expr {
        Expr::Literal(Literal::Boolean(_), _) => Some("boolean"),
        Expr::Literal(Literal::Integer(_) | Literal::NumericLit(_), _) => Some("integer"),
        Expr::Literal(Literal::String(_), _) => Some("text"),
        Expr::Literal(Literal::Null, _) => None,
        _ => None,
    }
}

fn object_name_matches(name: &ObjectName, cte_name: &str) -> bool {
    name.parts
        .last()
        .is_some_and(|part| part.eq_ignore_ascii_case(cte_name))
}

fn select_has_top_level_recursive_ref(select: &SelectStatement, cte_name: &str) -> bool {
    // Inner WITH names shadow outer CTEs for the main SELECT and JOIN namespace.
    let shadowed_by_local_with = select
        .ctes
        .iter()
        .any(|cte| cte.name.eq_ignore_ascii_case(cte_name));
    if shadowed_by_local_with {
        return false;
    }

    if select
        .from
        .as_ref()
        .is_some_and(|name| object_name_matches(name, cte_name))
    {
        return true;
    }
    select
        .joins
        .iter()
        .any(|join| object_name_matches(&join.table, cte_name))
}

fn select_contains_recursive_ref(select: &SelectStatement, cte_name: &str) -> bool {
    if select_has_top_level_recursive_ref(select, cte_name) {
        return true;
    }
    for cte in &select.ctes {
        if statement_contains_recursive_ref(&cte.query, cte_name) {
            return true;
        }
        if cte
            .recursive_term
            .as_deref()
            .is_some_and(|term| select_contains_recursive_ref(term, cte_name))
        {
            return true;
        }
    }
    for item in &select.items {
        if expr_contains_recursive_ref(&item.expr, cte_name) {
            return true;
        }
    }
    if select
        .selection
        .as_ref()
        .is_some_and(|expr| expr_contains_recursive_ref(expr, cte_name))
    {
        return true;
    }
    if select
        .having
        .as_ref()
        .is_some_and(|expr| expr_contains_recursive_ref(expr, cte_name))
    {
        return true;
    }
    if select
        .group_by
        .iter()
        .any(|expr| expr_contains_recursive_ref(expr, cte_name))
    {
        return true;
    }
    if select
        .order_by
        .iter()
        .any(|item| expr_contains_recursive_ref(&item.expr, cte_name))
    {
        return true;
    }
    if select
        .window_definitions
        .iter()
        .flat_map(|window| {
            window
                .partition_by
                .iter()
                .cloned()
                .chain(window.order_by.iter().map(|item| item.expr.clone()))
                .collect::<Vec<_>>()
        })
        .any(|expr| expr_contains_recursive_ref(&expr, cte_name))
    {
        return true;
    }
    if select
        .limit
        .as_ref()
        .is_some_and(|expr| expr_contains_recursive_ref(expr, cte_name))
    {
        return true;
    }
    select
        .offset
        .as_ref()
        .is_some_and(|expr| expr_contains_recursive_ref(expr, cte_name))
}

/// Shadow-aware variant of [`select_contains_recursive_ref`]: a local `WITH`
/// binding with the same name as the outer CTE shadows it, so a reference to
/// that name inside the nested scope resolves to the inner CTE rather than
/// the outer recursive one.
fn select_contains_unshadowed_recursive_ref(select: &SelectStatement, cte_name: &str) -> bool {
    if select
        .ctes
        .iter()
        .any(|cte| cte.name.eq_ignore_ascii_case(cte_name))
    {
        return false;
    }
    select_contains_recursive_ref(select, cte_name)
}

fn statement_contains_unshadowed_recursive_ref(statement: &Statement, cte_name: &str) -> bool {
    match statement {
        Statement::Select(select) => select_contains_unshadowed_recursive_ref(select, cte_name),
        Statement::SetOperation(set_op) => {
            statement_contains_unshadowed_recursive_ref(&set_op.left, cte_name)
                || statement_contains_unshadowed_recursive_ref(&set_op.right, cte_name)
        }
        _ => false,
    }
}

fn statement_contains_recursive_ref(statement: &Statement, cte_name: &str) -> bool {
    match statement {
        Statement::Select(select) => select_contains_recursive_ref(select, cte_name),
        Statement::SetOperation(set_op) => {
            statement_contains_recursive_ref(&set_op.left, cte_name)
                || statement_contains_recursive_ref(&set_op.right, cte_name)
                || set_op
                    .order_by
                    .iter()
                    .any(|item| expr_contains_recursive_ref(&item.expr, cte_name))
                || set_op
                    .limit
                    .as_ref()
                    .is_some_and(|expr| expr_contains_recursive_ref(expr, cte_name))
                || set_op
                    .offset
                    .as_ref()
                    .is_some_and(|expr| expr_contains_recursive_ref(expr, cte_name))
        }
        _ => false,
    }
}

fn expr_contains_recursive_ref(expr: &Expr, cte_name: &str) -> bool {
    match expr {
        Expr::ArraySubquery { query, .. }
        | Expr::Subquery { query, .. }
        | Expr::Exists { query, .. } => select_contains_recursive_ref(query, cte_name),
        Expr::InSubquery { expr, query, .. } => {
            expr_contains_recursive_ref(expr, cte_name)
                || select_contains_recursive_ref(query, cte_name)
        }
        Expr::UnaryOp { expr, .. } | Expr::IsNull { expr, .. } | Expr::Cast { expr, .. } => {
            expr_contains_recursive_ref(expr, cte_name)
        }
        Expr::BinaryOp { left, right, .. } | Expr::IsDistinctFrom { left, right, .. } => {
            expr_contains_recursive_ref(left, cte_name)
                || expr_contains_recursive_ref(right, cte_name)
        }
        Expr::Like { expr, pattern, .. } => {
            expr_contains_recursive_ref(expr, cte_name)
                || expr_contains_recursive_ref(pattern, cte_name)
        }
        Expr::InList { expr, list, .. } => {
            expr_contains_recursive_ref(expr, cte_name)
                || list
                    .iter()
                    .any(|item| expr_contains_recursive_ref(item, cte_name))
        }
        Expr::Between {
            expr, low, high, ..
        } => {
            expr_contains_recursive_ref(expr, cte_name)
                || expr_contains_recursive_ref(low, cte_name)
                || expr_contains_recursive_ref(high, cte_name)
        }
        Expr::CaseWhen {
            operand,
            conditions,
            results,
            else_result,
            ..
        } => {
            operand
                .as_ref()
                .is_some_and(|expr| expr_contains_recursive_ref(expr, cte_name))
                || conditions
                    .iter()
                    .any(|expr| expr_contains_recursive_ref(expr, cte_name))
                || results
                    .iter()
                    .any(|expr| expr_contains_recursive_ref(expr, cte_name))
                || else_result
                    .as_ref()
                    .is_some_and(|expr| expr_contains_recursive_ref(expr, cte_name))
        }
        Expr::Array { elements, .. } => elements
            .iter()
            .any(|element| expr_contains_recursive_ref(element, cte_name)),
        Expr::FunctionCall { args, filter, .. } => {
            args.iter()
                .any(|arg| expr_contains_recursive_ref(arg, cte_name))
                || filter
                    .as_ref()
                    .is_some_and(|expr| expr_contains_recursive_ref(expr, cte_name))
        }
        Expr::WindowFunction {
            function,
            partition_by,
            order_by,
            ..
        } => {
            expr_contains_recursive_ref(function, cte_name)
                || partition_by
                    .iter()
                    .any(|expr| expr_contains_recursive_ref(expr, cte_name))
                || order_by
                    .iter()
                    .any(|item| expr_contains_recursive_ref(&item.expr, cte_name))
        }
        Expr::Literal(_, _)
        | Expr::Identifier(_)
        | Expr::Parameter { .. }
        | Expr::Default { .. }
        | Expr::CypherExists { .. }
        | Expr::CypherPatternComprehension { .. } => false,
    }
}

fn attach_with_ctes_to_leftmost_branch(
    mut statement: Statement,
    ctes: &[CteDefinition],
) -> Statement {
    if ctes.is_empty() {
        return statement;
    }

    match &mut statement {
        Statement::Select(select) => {
            // Keep WITH-clause CTEs first so parser-generated wrapper CTEs
            // inside SELECT (column-alias wrappers) can reference them.
            let mut all_ctes = ctes.to_vec();
            all_ctes.extend(std::mem::take(&mut select.ctes));
            select.ctes = all_ctes;
            statement
        }
        Statement::SetOperation(set_op) => {
            *set_op.left = attach_with_ctes_to_leftmost_branch(*set_op.left.clone(), ctes);
            statement
        }
        _ => statement,
    }
}
