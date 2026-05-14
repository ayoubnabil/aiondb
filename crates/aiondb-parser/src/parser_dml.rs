#![allow(clippy::if_same_then_else, clippy::assigning_clones)]

use aiondb_core::{DbError, DbResult};

use crate::{
    ast::{
        CteDefinition, DeleteStatement, DistinctKind, Expr, InsertStatement, MergeAction,
        MergeSource, MergeStatement, MergeWhenClause, ObjectName, OnConflict, OnConflictAction,
        SelectItem, SelectStatement, Statement, UpdateAssignment, UpdateStatement,
    },
    keywords::Keyword,
    tokens::TokenKind,
    Parser,
};

#[derive(Clone)]
pub(crate) enum AssignmentTarget {
    Plain {
        column: String,
        span: crate::span::Span,
        /// True when the original target included a composite subfield
        /// (e.g. `f3.if1`).  Used to validate that DEFAULT is
        /// not used for subfield assignments.
        is_subfield: bool,
    },
    ArraySubscript {
        column: String,
        subscripts: Vec<AssignmentSubscript>,
        composite_field: Option<String>,
        span: crate::span::Span,
    },
}

#[derive(Clone)]
pub(crate) enum AssignmentSubscript {
    Index(Expr),
    Slice {
        lower: Option<Expr>,
        upper: Option<Expr>,
    },
}

impl AssignmentTarget {
    fn span(&self) -> crate::span::Span {
        match self {
            Self::Plain { span, .. } | Self::ArraySubscript { span, .. } => *span,
        }
    }
}

impl Parser {
    pub(crate) fn parse_insert_statement(&mut self) -> DbResult<Statement> {
        let insert = self.expect_keyword(Keyword::Insert)?;
        self.expect_keyword(Keyword::Into)?;
        let table = self.parse_object_name()?;
        let mut columns = Vec::new();
        let mut insert_targets = Vec::new();
        let mut span = insert.span.merge(table.span);

        // Parse optional table alias: INSERT INTO t AS alias ...
        let table_alias = if self.consume_keyword(Keyword::As).is_some() {
            let (alias_name, _alias_span) = self.expect_identifier()?;
            Some(alias_name)
        } else {
            None
        };

        if self.current().kind == TokenKind::LParen {
            // Distinguish between:
            //   INSERT INTO t (col1, col2) VALUES ...   -- column list
            //   INSERT INTO t (SELECT ...)              -- parenthesized subquery
            //   INSERT INTO t (col1, col2) (SELECT ...) -- column list + parenthesized subquery
            // Peek: if ( is immediately followed by SELECT/WITH, treat as subquery
            let next_idx = self.index + 1;
            let is_subquery = next_idx < self.tokens.len()
                && matches!(
                    self.tokens[next_idx].kind,
                    TokenKind::Keyword(Keyword::Select | Keyword::With)
                );
            if is_subquery {
                // Parenthesized subquery: INSERT INTO t (SELECT ...)
                self.advance(); // consume '('
                let inner_stmt = self.parse_select_statement()?;
                let _ = self.consume_kind(&TokenKind::RParen);
                if let Statement::Select(query) = inner_stmt {
                    span = span.merge(query.span);
                    let on_conflict = self.parse_on_conflict_clause(&mut span)?;
                    let returning = self.parse_returning_clause(&mut span)?;
                    return Ok(Statement::Insert(InsertStatement {
                        table,
                        table_alias: table_alias.clone(),
                        columns,
                        rows: Vec::new(),
                        query: Some(query),
                        on_conflict,
                        returning,
                        span,
                    }));
                }
            } else {
                // Column list
                self.advance(); // consume '('
                loop {
                    insert_targets.push(self.parse_assignment_target()?);
                    if !self.consume_kind(&TokenKind::Comma) {
                        break;
                    }
                }
                let rparen = self.expect_token(&TokenKind::RParen)?;
                span = span.merge(rparen.span);
                columns = self.insert_targets_to_columns(&insert_targets);
            }
        }

        if let Some(default) = self.consume_keyword(Keyword::Default) {
            let values = self.expect_keyword(Keyword::Values)?;
            span = span.merge(default.span).merge(values.span);
            let returning = self.parse_returning_clause(&mut span)?;
            return Ok(Statement::Insert(InsertStatement {
                table,
                table_alias: table_alias.clone(),
                columns,
                rows: vec![vec![]],
                query: None,
                on_conflict: None,
                returning,
                span,
            }));
        }

        // Handle SELECT (including parenthesized) and WITH as subquery source
        if matches!(
            self.current().kind,
            TokenKind::Keyword(Keyword::Select | Keyword::With)
        ) {
            let inner_stmt = if matches!(self.current().kind, TokenKind::Keyword(Keyword::With)) {
                self.parse_with_statement()?
            } else {
                self.parse_select_statement()?
            };
            // Support UNION/INTERSECT/EXCEPT after SELECT in INSERT context
            let inner_stmt = self.maybe_parse_set_operation(inner_stmt)?;
            let query = Self::insert_stmt_to_select(inner_stmt);
            if let Some(query) = query {
                span = span.merge(query.span);
                let on_conflict = self.parse_on_conflict_clause(&mut span)?;
                let returning = self.parse_returning_clause(&mut span)?;
                // Rewrite column list + query when array subscript targets
                // are present (e.g. INSERT INTO t (f2[1], f2[2]) SELECT 7,8).
                let (columns, query) =
                    self.rewrite_insert_select_targets(&insert_targets, columns, query);
                return Ok(Statement::Insert(InsertStatement {
                    table,
                    table_alias: table_alias.clone(),
                    columns,
                    rows: Vec::new(),
                    query: Some(query),
                    on_conflict,
                    returning,
                    span,
                }));
            }
        }

        // Parenthesized subquery after column list: INSERT INTO t (a,b) (SELECT ...)
        if self.current().kind == TokenKind::LParen {
            let next_idx = self.index + 1;
            let is_subquery = next_idx < self.tokens.len()
                && matches!(
                    self.tokens[next_idx].kind,
                    TokenKind::Keyword(Keyword::Select | Keyword::With)
                );
            if is_subquery {
                self.advance(); // consume '('
                let inner_stmt = self.parse_select_statement()?;
                let _ = self.consume_kind(&TokenKind::RParen);
                if let Statement::Select(query) = inner_stmt {
                    span = span.merge(query.span);
                    let on_conflict = self.parse_on_conflict_clause(&mut span)?;
                    let returning = self.parse_returning_clause(&mut span)?;
                    let (columns, query) =
                        self.rewrite_insert_select_targets(&insert_targets, columns, query);
                    return Ok(Statement::Insert(InsertStatement {
                        table,
                        table_alias: table_alias.clone(),
                        columns,
                        rows: Vec::new(),
                        query: Some(query),
                        on_conflict,
                        returning,
                        span,
                    }));
                }
            }
        }

        // Skip optional OVERRIDING SYSTEM VALUE / OVERRIDING USER VALUE
        if self.consume_keyword(Keyword::Overriding).is_some() {
            // consume SYSTEM or USER
            let _ = self.advance();
            // consume VALUE
            let _ = self.consume_keyword(Keyword::Value);
        }

        let values = self.expect_keyword(Keyword::Values)?;

        let mut rows = Vec::new();
        span = span.merge(values.span);
        let mut expected_width: Option<usize> = None;
        let mut total_cells = 0usize;

        loop {
            let lparen = self.expect_token(&TokenKind::LParen)?;
            // Pre-size each row's expr Vec to the previously-seen
            // width (after the first row); subsequent rows must match.
            let mut row = Vec::with_capacity(expected_width.unwrap_or(0));
            loop {
                row.push(self.parse_expr()?);
                if !self.consume_kind(&TokenKind::Comma) {
                    break;
                }
            }
            let rparen = self.expect_token(&TokenKind::RParen)?;
            span = span.merge(lparen.span).merge(rparen.span);
            if row.len() > crate::MAX_VALUES_COLUMNS {
                return Err(DbError::program_limit(format!(
                    "VALUES row exceeds maximum number of columns ({})",
                    crate::MAX_VALUES_COLUMNS
                )));
            }
            if let Some(width) = expected_width {
                if row.len() != width {
                    return self.syntax_error(span, "VALUES lists must all be the same length");
                }
            } else {
                expected_width = Some(row.len());
            }
            if rows.len() >= crate::MAX_VALUES_ROWS {
                return Err(DbError::program_limit(format!(
                    "VALUES exceeds maximum number of rows ({})",
                    crate::MAX_VALUES_ROWS
                )));
            }
            total_cells = total_cells.saturating_add(row.len());
            if total_cells > crate::MAX_VALUES_CELLS {
                return Err(DbError::program_limit(format!(
                    "VALUES exceeds maximum number of cells ({})",
                    crate::MAX_VALUES_CELLS
                )));
            }
            rows.push(row);

            if !self.consume_kind(&TokenKind::Comma) {
                break;
            }
        }

        // Validate: DEFAULT cannot be used for composite subfield targets.
        // PostgreSQL: "cannot set a subfield to DEFAULT"
        if !insert_targets.is_empty() {
            for (i, target) in insert_targets.iter().enumerate() {
                if let AssignmentTarget::Plain {
                    is_subfield: true,
                    span: target_span,
                    ..
                } = target
                {
                    for row in &rows {
                        if let Some(Expr::Default { .. }) = row.get(i) {
                            return Err(DbError::syntax_error("cannot set a subfield to DEFAULT")
                                .with_position(target_span.start + 1));
                        }
                    }
                }
            }
        }

        if !insert_targets.is_empty() {
            let (rewritten_columns, rewritten_rows) =
                self.rewrite_insert_values_targets(&insert_targets, rows);
            columns = rewritten_columns;
            rows = rewritten_rows;
        }

        let on_conflict = self.parse_on_conflict_clause(&mut span)?;
        let returning = self.parse_returning_clause(&mut span)?;

        Ok(Statement::Insert(InsertStatement {
            table,
            table_alias,
            columns,
            rows,
            query: None,
            on_conflict,
            returning,
            span,
        }))
    }

    /// Convert a parsed statement into a `SelectStatement` suitable for
    /// `InsertStatement.query`.  If the statement is a plain `SELECT`, use
    /// it directly.  If it is a `SetOperation` (UNION / INTERSECT / EXCEPT),
    /// wrap it as a CTE so the insert can consume a single `SelectStatement`:
    ///
    ///   WITH __`insert_setop` AS (<set-operation>)
    ///   SELECT * FROM __`insert_setop`
    fn insert_stmt_to_select(stmt: Statement) -> Option<SelectStatement> {
        match stmt {
            Statement::Select(query) => Some(query),
            Statement::SetOperation(set_op) => {
                let set_op_span = set_op.span;
                let cte_name = "__insert_setop".to_owned();
                let cte = CteDefinition {
                    name: cte_name.clone(),
                    column_aliases: None,
                    recursive: false,
                    query: Box::new(Statement::SetOperation(set_op)),
                    recursive_term: None,
                    union_all: false,
                    span: set_op_span,
                };
                let from_name = ObjectName {
                    parts: vec![cte_name],
                    span: set_op_span,
                };
                Some(SelectStatement {
                    row_lock: None,
                    ctes: vec![cte],
                    distinct: DistinctKind::All,
                    items: vec![SelectItem {
                        expr: Expr::Identifier(ObjectName {
                            parts: vec!["*".to_owned()],
                            span: set_op_span,
                        }),
                        alias: None,
                        span: set_op_span,
                    }],
                    from: Some(from_name),
                    from_alias: None,
                    from_span: Some(set_op_span),
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
                    span: set_op_span,
                })
            }
            _ => None,
        }
    }

    pub(crate) fn parse_delete_statement(&mut self) -> DbResult<Statement> {
        let delete = self.expect_keyword(Keyword::Delete)?;
        self.expect_keyword(Keyword::From)?;
        if let Some(only_token) = self.consume_keyword(Keyword::Only) {
            return self.unsupported_feature(
                only_token.span,
                "DELETE ... ONLY is not supported".to_string(),
            );
        }
        let table = self.parse_object_name()?;
        let table_alias = self.parse_optional_dml_target_alias(Keyword::Using);
        let mut span = delete.span.merge(table.span);

        // Parse optional USING clause: DELETE FROM t USING other [alias], ... WHERE ...
        let using_tables = self.parse_dml_from_tables(Keyword::Using, &mut span)?;

        let (selection, where_span) =
            if let Some(where_token) = self.consume_keyword(Keyword::Where) {
                let selection = self.parse_expr()?;
                span = span.merge(where_token.span).merge(selection.span());
                (Some(selection), Some(where_token.span))
            } else {
                (None, None)
            };

        let returning = self.parse_returning_clause(&mut span)?;

        Ok(Statement::Delete(DeleteStatement {
            table,
            table_alias,
            using_tables,
            selection,
            where_span,
            returning,
            span,
        }))
    }

    pub(crate) fn parse_update_statement(&mut self) -> DbResult<Statement> {
        let update = self.expect_keyword(Keyword::Update)?;
        if let Some(only_token) = self.consume_keyword(Keyword::Only) {
            return self.unsupported_feature(
                only_token.span,
                "UPDATE ... ONLY is not supported".to_string(),
            );
        }
        let table = self.parse_object_name()?;
        let table_alias = self.parse_optional_dml_target_alias(Keyword::Set);
        self.expect_keyword(Keyword::Set)?;

        let mut assignments = Vec::new();
        let mut span = update.span.merge(table.span);

        loop {
            // Multi-column SET: (col1, col2, ...) = (expr1, expr2, ...) or = ROW(...)
            // or = (SELECT ...)
            if self.current().kind == TokenKind::LParen {
                let lparen_span = self.advance().span; // consume '('
                let mut columns = Vec::new();
                loop {
                    let (col, _) = self.expect_identifier()?;
                    // Skip optional array subscripts and composite paths:
                    // e.g. (f1[2]) = ..., (f1[1].field) = ...
                    self.skip_array_subscripts();
                    while self.current().kind == TokenKind::Dot {
                        self.advance(); // consume '.'
                        let _ = self.expect_identifier();
                        self.skip_array_subscripts();
                    }
                    columns.push(col);
                    if !self.consume_kind(&TokenKind::Comma) {
                        break;
                    }
                }
                self.expect_token(&TokenKind::RParen)?;
                // Skip outer array subscripts: (f1[2])[1]
                self.skip_array_subscripts();
                self.expect_token(&TokenKind::Eq)?;

                // Consume optional ROW keyword
                let _ = self.consume_keyword(Keyword::Row);

                // Parse right-hand side as expression(s).
                // For (SELECT ...) or ROW(...) the parse_expr handles them.
                // For (expr1, expr2, ...) we manually parse comma-separated.
                let rhs_expr = self.parse_expr()?;
                let assignment_span = lparen_span.merge(self.previous_span());
                span = span.merge(assignment_span);

                // Assign the full RHS to every column; use into_iter to
                // avoid cloning the column strings.
                for col in columns {
                    assignments.push(UpdateAssignment {
                        column: col,
                        expr: rhs_expr.clone(),
                        span: assignment_span,
                    });
                }
            } else {
                let target = self.parse_assignment_target()?;
                self.expect_token(&TokenKind::Eq)?;
                let rhs = self.parse_expr()?;
                let assignment_span = target.span().merge(rhs.span());
                span = span.merge(assignment_span);
                self.push_update_assignment(&mut assignments, target, rhs);
            }

            if !self.consume_kind(&TokenKind::Comma) {
                break;
            }
        }
        // Parse optional FROM clause: UPDATE t SET ... FROM other_table [alias], ... WHERE ...
        let from_tables = self.parse_dml_from_tables(Keyword::From, &mut span)?;

        let (selection, where_span) =
            if let Some(where_token) = self.consume_keyword(Keyword::Where) {
                let selection = self.parse_expr()?;
                span = span.merge(where_token.span).merge(selection.span());
                (Some(selection), Some(where_token.span))
            } else {
                (None, None)
            };

        let returning = self.parse_returning_clause(&mut span)?;

        Ok(Statement::Update(UpdateStatement {
            table,
            table_alias,
            assignments,
            from_tables,
            selection,
            where_span,
            returning,
            ctes: Vec::new(),
            span,
        }))
    }

    fn parse_optional_dml_target_alias(&mut self, required_keyword: Keyword) -> Option<String> {
        if self.consume_keyword(Keyword::As).is_some() {
            return self.expect_identifier().ok().map(|(alias, _)| alias);
        }
        if let TokenKind::Identifier(_) = &self.current().kind {
            let reserved_follow = match required_keyword {
                Keyword::Set => vec![Keyword::Set],
                Keyword::Using => vec![Keyword::Where, Keyword::Returning, Keyword::Using],
                _ => vec![required_keyword],
            };
            if !matches!(
                self.current().kind,
                TokenKind::Keyword(keyword) if reserved_follow.contains(&keyword)
            ) && !matches!(self.current().kind, TokenKind::Semicolon | TokenKind::Eof)
            {
                return self.expect_identifier().ok().map(|(alias, _)| alias);
            }
        }
        None
    }

    /// Parse an optional `ON CONFLICT ...` clause for INSERT statements.
    fn parse_on_conflict_clause(
        &mut self,
        span: &mut crate::span::Span,
    ) -> DbResult<Option<OnConflict>> {
        // Look for ON CONFLICT
        let Some(on_token) = self.consume_keyword(Keyword::On) else {
            return Ok(None);
        };
        let conflict_token = self.expect_keyword(Keyword::Conflict)?;
        *span = span.merge(on_token.span).merge(conflict_token.span);

        // Optional conflict target:
        //   (col1, col2, ...) or (expr1, expr2, ...)
        //   ON CONSTRAINT constraint_name
        let columns = if self.consume_keyword(Keyword::On).is_some() {
            // ON CONSTRAINT constraint_name - the name is a constraint, not
            // a column, so we skip it (column validation would fail).
            self.expect_keyword(Keyword::Constraint)?;
            let (_name, _) = self.expect_identifier()?;
            *span = span.merge(self.previous_span());
            Vec::new()
        } else if self.consume_kind(&TokenKind::LParen) {
            let mut cols = Vec::new();
            // Use balanced-paren skipping since conflict targets can include
            // complex expressions like `lower(fruit) text_pattern_ops`
            let mut depth = 1u32;
            let mut current_col = String::new();
            // Track whether the current slot contains a non-column expression
            // (e.g., `lower(fruit)`) so we don't capture trailing identifiers
            // like `collate` or operator class names as column names.
            let mut is_expr_slot = false;
            while depth > 0 && !self.is_eof() {
                match &self.current().kind {
                    TokenKind::LParen => {
                        depth += 1;
                        if depth == 2 && current_col.is_empty() {
                            // Entered a sub-expression at depth 1 (e.g., the
                            // `(` in `lower(fruit)` or a bare `(expr)`)
                            is_expr_slot = true;
                        }
                        self.advance();
                    }
                    TokenKind::RParen => {
                        depth -= 1;
                        if depth == 0 {
                            if !current_col.is_empty() {
                                cols.push(std::mem::take(&mut current_col));
                            }
                            is_expr_slot = false;
                            let rparen = self.advance();
                            *span = span.merge(rparen.span);
                        } else {
                            self.advance();
                        }
                    }
                    TokenKind::Comma if depth == 1 => {
                        if !current_col.is_empty() {
                            cols.push(std::mem::take(&mut current_col));
                        }
                        is_expr_slot = false;
                        self.advance();
                    }
                    TokenKind::Identifier(s)
                        if depth == 1 && current_col.is_empty() && !is_expr_slot =>
                    {
                        // Check if this identifier is followed by '(' - if so,
                        // it's a function call expression (e.g. `lower(fruit)`)
                        // and should not be treated as a column name.
                        let next_is_lparen = self.index + 1 < self.tokens.len()
                            && matches!(self.tokens[self.index + 1].kind, TokenKind::LParen);
                        if next_is_lparen {
                            is_expr_slot = true;
                        } else {
                            current_col = s.clone();
                        }
                        self.advance();
                    }
                    // Skip trailing identifiers at depth 1 - these are
                    // operator class names (e.g. `text_pattern_ops`),
                    // COLLATE keywords, or other expression modifiers.
                    TokenKind::Identifier(_) | TokenKind::String(_)
                        if depth == 1 && (is_expr_slot || !current_col.is_empty()) =>
                    {
                        self.advance();
                    }
                    _ => {
                        self.advance();
                    }
                }
            }
            // Skip optional WHERE clause after conflict target: ON CONFLICT (col) WHERE expr
            if self.consume_keyword(Keyword::Where).is_some() {
                let _ = self.parse_expr()?;
            }
            cols
        } else {
            Vec::new()
        };

        // Parse DO NOTHING or DO UPDATE SET ...
        let do_token = self.expect_keyword(Keyword::Do)?;
        *span = span.merge(do_token.span);

        let action = if self.consume_keyword(Keyword::Nothing).is_some() {
            *span = span.merge(self.previous_span());
            OnConflictAction::DoNothing
        } else {
            self.expect_keyword(Keyword::Update)?;
            let set_token = self.expect_keyword(Keyword::Set)?;
            *span = span.merge(set_token.span);

            let mut assignments = Vec::new();
            loop {
                // Multi-column SET: (col1, col2) = (expr1, expr2) or = (SELECT ...)
                if self.current().kind == TokenKind::LParen {
                    let lparen_span = self.advance().span;
                    let mut columns = Vec::new();
                    loop {
                        let (col, _) = self.expect_identifier()?;
                        columns.push(col);
                        if !self.consume_kind(&TokenKind::Comma) {
                            break;
                        }
                    }
                    self.expect_token(&TokenKind::RParen)?;
                    self.expect_token(&TokenKind::Eq)?;
                    let _ = self.consume_keyword(Keyword::Row);
                    let rhs_expr = self.parse_expr()?;
                    let assignment_span = lparen_span.merge(self.previous_span());
                    *span = span.merge(assignment_span);
                    for col in columns {
                        assignments.push(UpdateAssignment {
                            column: col,
                            expr: rhs_expr.clone(),
                            span: assignment_span,
                        });
                    }
                } else {
                    let target = self.parse_assignment_target()?;
                    self.expect_token(&TokenKind::Eq)?;
                    let rhs = self.parse_expr()?;
                    let assignment_span = target.span().merge(rhs.span());
                    *span = span.merge(assignment_span);
                    self.push_update_assignment(&mut assignments, target, rhs);
                }
                if !self.consume_kind(&TokenKind::Comma) {
                    break;
                }
            }
            // Optional WHERE clause after DO UPDATE SET ...
            let where_clause = if self.consume_keyword(Keyword::Where).is_some() {
                let where_expr = self.parse_expr()?;
                *span = span.merge(where_expr.span());
                Some(where_expr)
            } else {
                None
            };
            OnConflictAction::DoUpdate {
                assignments,
                where_clause,
            }
        };

        Ok(Some(OnConflict { columns, action }))
    }

    pub(crate) fn parse_merge_statement(&mut self) -> DbResult<Statement> {
        let merge_token = self.expect_keyword(Keyword::Merge)?;
        self.expect_keyword(Keyword::Into)?;
        if let Some(only_token) = self.consume_keyword(Keyword::Only) {
            return self.unsupported_feature(
                only_token.span,
                "MERGE INTO ONLY is not supported".to_string(),
            );
        }
        let target_table = self.parse_object_name()?;
        let mut span = merge_token.span.merge(target_table.span);

        // Optional alias for target table (AS alias or bare identifier).
        let target_alias = self.parse_optional_table_alias();

        self.expect_keyword(Keyword::Using)?;

        // Source can be a table name or a parenthesized subquery.
        let (source, source_alias) = if self.consume_kind(&TokenKind::LParen) {
            // USING (SELECT ...) AS alias
            let Statement::Select(select_stmt) = self.parse_select_statement()? else {
                return self.unsupported_feature(
                    self.current().span,
                    "expected SELECT in MERGE USING subquery".to_string(),
                );
            };
            self.expect_token(&TokenKind::RParen)?;
            let source = MergeSource::Subquery(Box::new(select_stmt));
            let alias = self.parse_optional_table_alias();
            (source, alias)
        } else {
            if let Some(only_token) = self.consume_keyword(Keyword::Only) {
                return self.unsupported_feature(
                    only_token.span,
                    "MERGE USING ONLY is not supported".to_string(),
                );
            }
            let source_table = self.parse_object_name()?;
            span = span.merge(source_table.span);
            let source = MergeSource::Table(source_table);
            let alias = self.parse_optional_table_alias();
            (source, alias)
        };

        self.expect_keyword(Keyword::On)?;
        let on_condition = self.parse_expr()?;
        span = span.merge(on_condition.span());

        // Parse WHEN clauses.
        let mut when_clauses = Vec::new();
        while self.consume_keyword(Keyword::When).is_some() {
            let clause_start = self.previous_span();
            let matched = if self.consume_keyword(Keyword::Not).is_some() {
                self.expect_keyword(Keyword::Matched)?;
                false
            } else {
                self.expect_keyword(Keyword::Matched)?;
                true
            };

            // Optional AND condition.
            let condition = if self.consume_keyword(Keyword::And).is_some() {
                Some(self.parse_merge_when_condition()?)
            } else {
                None
            };

            self.expect_keyword(Keyword::Then)?;

            // Both WHEN MATCHED and WHEN NOT MATCHED can use DO NOTHING.
            let action = if self.consume_keyword(Keyword::Do).is_some() {
                self.expect_keyword(Keyword::Nothing)?;
                MergeAction::DoNothing
            } else if matched {
                // WHEN MATCHED THEN UPDATE or DELETE
                if self.consume_keyword(Keyword::Update).is_some() {
                    self.expect_keyword(Keyword::Set)?;
                    let assignments = self.parse_merge_update_assignments()?;
                    MergeAction::Update { assignments }
                } else if self.consume_keyword(Keyword::Delete).is_some() {
                    MergeAction::Delete
                } else {
                    return self.unsupported_feature(
                        self.current().span,
                        "MERGE WHEN MATCHED expects UPDATE, DELETE, or DO NOTHING".to_string(),
                    );
                }
            } else {
                // WHEN NOT MATCHED THEN INSERT
                self.expect_keyword(Keyword::Insert)?;

                // Check for INSERT DEFAULT VALUES
                if self.consume_keyword(Keyword::Default).is_some() {
                    self.expect_keyword(Keyword::Values)?;
                    MergeAction::InsertDefaultValues
                } else {
                    let mut columns = Vec::new();
                    if self.consume_kind(&TokenKind::LParen) {
                        loop {
                            let (col, _) = self.expect_identifier()?;
                            columns.push(col);
                            if !self.consume_kind(&TokenKind::Comma) {
                                break;
                            }
                        }
                        self.expect_token(&TokenKind::RParen)?;
                    }
                    self.expect_keyword(Keyword::Values)?;
                    self.expect_token(&TokenKind::LParen)?;
                    let mut values = Vec::new();
                    loop {
                        values.push(self.parse_expr()?);
                        if !self.consume_kind(&TokenKind::Comma) {
                            break;
                        }
                    }
                    let rparen = self.expect_token(&TokenKind::RParen)?;
                    span = span.merge(rparen.span);
                    MergeAction::Insert { columns, values }
                }
            };

            let clause_span = clause_start.merge(self.previous_span());
            span = span.merge(clause_span);
            when_clauses.push(MergeWhenClause {
                matched,
                condition,
                action,
                span: clause_span,
            });
        }

        Ok(Statement::Merge(MergeStatement {
            target_table,
            target_alias,
            source,
            source_alias,
            on_condition,
            when_clauses,
            span,
        }))
    }

    /// Parse a MERGE WHEN ... AND <condition>, stopping before THEN.
    /// We need special logic because THEN is not a binary operator, but
    fn parse_merge_when_condition(&mut self) -> DbResult<Expr> {
        self.parse_expr()
    }

    fn parse_merge_update_assignments(&mut self) -> DbResult<Vec<UpdateAssignment>> {
        let mut assignments = Vec::new();
        loop {
            let target = self.parse_assignment_target()?;
            self.expect_token(&TokenKind::Eq)?;
            let rhs = self.parse_expr()?;
            self.push_update_assignment(&mut assignments, target, rhs);
            if !self.consume_kind(&TokenKind::Comma) {
                break;
            }
        }
        Ok(assignments)
    }

    fn parse_assignment_target(&mut self) -> DbResult<AssignmentTarget> {
        let (column, column_span) = self.expect_identifier()?;
        let mut subscripts = Vec::new();
        let mut composite_field = None;
        let mut supported = true;
        let mut span = column_span;

        while self.current().kind == TokenKind::LBracket {
            let lbracket = self.advance();
            span = span.merge(lbracket.span);

            let lower = if self.current().kind == TokenKind::Colon {
                None
            } else {
                let index = self.parse_expr()?;
                span = span.merge(index.span());
                self.ensure_non_null_assignment_subscript(&index)?;
                Some(index)
            };

            let is_slice = if lower.is_none() {
                self.advance();
                true
            } else {
                self.consume_kind(&TokenKind::Colon)
            };

            if is_slice {
                let upper = if self.current().kind == TokenKind::RBracket {
                    None
                } else {
                    let upper = self.parse_expr()?;
                    span = span.merge(upper.span());
                    self.ensure_non_null_assignment_subscript(&upper)?;
                    Some(upper)
                };
                self.validate_assignment_slice_width(lower.as_ref(), upper.as_ref())?;
                if supported {
                    subscripts.push(AssignmentSubscript::Slice { lower, upper });
                }
            } else if supported {
                let Some(index) = lower else {
                    return Err(DbError::syntax_error("array assignment index is missing")
                        .with_position(lbracket.span.start + 1));
                };
                subscripts.push(AssignmentSubscript::Index(index));
            }
            let rbracket = self.expect_token(&TokenKind::RBracket)?;
            span = span.merge(rbracket.span);
        }

        let mut has_subfield = false;
        while self.current().kind == TokenKind::Dot {
            has_subfield = true;
            span = span.merge(self.advance().span);
            if matches!(
                self.current().kind,
                TokenKind::Identifier(_) | TokenKind::Keyword(_)
            ) {
                let (field, field_span) = self.expect_identifier()?;
                span = span.merge(field_span);
                if supported
                    && !subscripts.is_empty()
                    && composite_field.is_none()
                    && self.can_rewrite_composite_assignment_subscripts(&subscripts)
                {
                    composite_field = Some(field);
                } else {
                    supported = false;
                }
            } else {
                supported = false;
            }
            if self.current().kind == TokenKind::LBracket {
                supported = false;
            }
            self.skip_array_subscripts();
            span = span.merge(self.previous_span());
        }

        if supported
            && !subscripts.is_empty()
            && self.can_rewrite_assignment_subscripts(&subscripts)
        {
            Ok(AssignmentTarget::ArraySubscript {
                column,
                subscripts,
                composite_field,
                span,
            })
        } else {
            Ok(AssignmentTarget::Plain {
                column,
                span,
                is_subfield: has_subfield,
            })
        }
    }

    fn push_update_assignment(
        &self,
        assignments: &mut Vec<UpdateAssignment>,
        target: AssignmentTarget,
        rhs: Expr,
    ) {
        match target {
            AssignmentTarget::Plain { column, span, .. } => {
                let assignment_span = span.merge(rhs.span());
                if let Some(existing) = assignments
                    .iter_mut()
                    .rev()
                    .find(|item| item.column == column)
                {
                    existing.expr = rhs;
                    existing.span = existing.span.merge(assignment_span);
                } else {
                    assignments.push(UpdateAssignment {
                        column,
                        expr: rhs,
                        span: assignment_span,
                    });
                }
            }
            AssignmentTarget::ArraySubscript {
                column,
                subscripts,
                composite_field,
                span,
            } => {
                let assignment_span = span.merge(rhs.span());
                if let Some(existing) = assignments
                    .iter_mut()
                    .rev()
                    .find(|item| item.column == column)
                {
                    let prev_expr = std::mem::replace(
                        &mut existing.expr,
                        Expr::Literal(crate::ast::Literal::Null, assignment_span),
                    );
                    existing.expr = if let Some(field) = composite_field {
                        self.build_composite_array_assignment_expr(
                            prev_expr,
                            subscripts,
                            field,
                            rhs,
                            assignment_span,
                        )
                    } else {
                        self.build_array_assignment_expr(
                            prev_expr,
                            subscripts,
                            rhs,
                            assignment_span,
                        )
                    };
                    existing.span = existing.span.merge(assignment_span);
                } else {
                    let expr = if let Some(field) = composite_field {
                        self.rewrite_composite_array_assignment_expr(
                            &column,
                            subscripts,
                            &field,
                            rhs,
                            assignment_span,
                        )
                    } else {
                        self.rewrite_array_assignment_expr(
                            &column,
                            subscripts,
                            rhs,
                            assignment_span,
                        )
                    };
                    assignments.push(UpdateAssignment {
                        column,
                        expr,
                        span: assignment_span,
                    });
                }
            }
        }
    }

    /// Parse an optional table alias: `AS alias`, bare `identifier`,
    /// or nothing. Returns `None` if no alias is found.
    fn parse_optional_table_alias(&mut self) -> Option<String> {
        if self.consume_keyword(Keyword::As).is_some() {
            return self.expect_identifier().ok().map(|(name, _)| name);
        }
        // A bare identifier that is NOT a keyword that could start the next
        // clause (USING, ON, WHEN, SET, WHERE, etc.)
        if let TokenKind::Identifier(ref s) = self.current().kind {
            if !matches!(
                s.to_ascii_uppercase().as_str(),
                "USING" | "ON" | "WHEN" | "SET" | "WHERE"
            ) {
                return self.expect_identifier().ok().map(|(name, _)| name);
            }
        }
        None
    }

    /// Parse an optional `RETURNING col1, col2, ...` or `RETURNING *` clause.
    /// Returns an empty vec if the keyword is absent.
    /// Handles `RETURNING *`, `RETURNING *, expr AS alias`, and `RETURNING expr, ...`.
    /// Parse an optional FROM (for UPDATE) or USING (for DELETE) clause
    /// that lists additional table references. Returns a list of
    /// `(table_name, optional_alias)` pairs.
    ///
    /// Parenthesized sources (subqueries/joins) are currently unsupported and
    fn parse_dml_from_tables(
        &mut self,
        keyword: Keyword,
        span: &mut crate::span::Span,
    ) -> DbResult<Vec<(ObjectName, Option<String>)>> {
        if self.consume_keyword(keyword).is_none() {
            return Ok(Vec::new());
        }
        let mut tables = Vec::new();
        loop {
            if self.current().kind == TokenKind::LParen {
                return self.unsupported_feature(
                    self.current().span,
                    format!(
                        "{keyword_name} with parenthesized source is not supported",
                        keyword_name = keyword.name().to_uppercase()
                    ),
                );
            }
            // Skip optional ONLY
            let _ = self.consume_keyword(Keyword::Only);
            let table_name = self.parse_object_name()?;
            *span = span.merge(table_name.span);
            // Optional alias: AS alias or bare alias
            let alias = if self.consume_keyword(Keyword::As).is_some() {
                let (name, _) = self.expect_identifier()?;
                Some(name)
            } else if let TokenKind::Identifier(_) = &self.current().kind {
                if matches!(
                    self.current().kind,
                    TokenKind::Keyword(Keyword::Where | Keyword::Returning | Keyword::Set)
                ) {
                    None
                } else {
                    let (name, _) = self.expect_identifier()?;
                    Some(name)
                }
            } else {
                None
            };
            tables.push((table_name, alias));
            if !self.consume_kind(&TokenKind::Comma) {
                break;
            }
        }
        Ok(tables)
    }

    fn parse_returning_clause(
        &mut self,
        span: &mut crate::span::Span,
    ) -> DbResult<Vec<SelectItem>> {
        let Some(returning_token) = self.consume_keyword(Keyword::Returning) else {
            return Ok(Vec::new());
        };

        let mut items = Vec::new();

        loop {
            if self.current().kind == TokenKind::Star {
                let star_token = self.advance();
                *span = span.merge(star_token.span);
                items.push(SelectItem {
                    expr: Expr::Identifier(ObjectName {
                        parts: vec!["*".to_owned()],
                        span: star_token.span,
                    }),
                    alias: None,
                    span: returning_token.span.merge(star_token.span),
                });
            } else {
                let expr = self.parse_expr()?;
                let mut item_span = returning_token.span.merge(expr.span());
                let alias = if self.consume_keyword(Keyword::As).is_some() {
                    let (alias, alias_span) = self.expect_identifier()?;
                    item_span = item_span.merge(alias_span);
                    Some(alias)
                } else {
                    None
                };
                *span = span.merge(item_span);
                items.push(SelectItem {
                    expr,
                    alias,
                    span: item_span,
                });
            }
            if !self.consume_kind(&TokenKind::Comma) {
                break;
            }
        }

        Ok(items)
    }

    fn validate_assignment_slice_width(
        &self,
        lower: Option<&Expr>,
        upper: Option<&Expr>,
    ) -> DbResult<()> {
        let (Some(lower), Some(upper)) = (lower, upper) else {
            return Ok(());
        };
        let (Some(lower), Some(upper)) =
            (integer_literal_value(lower), integer_literal_value(upper))
        else {
            return Ok(());
        };
        if upper < lower {
            return Ok(());
        }
        let width = i128::from(upper) - i128::from(lower) + 1;
        if width > 100_000 {
            return Err(DbError::program_limit(
                "array assignment would create too many elements (max 100000)",
            ));
        }
        Ok(())
    }
}

pub(crate) fn integer_literal_value(expr: &Expr) -> Option<i64> {
    match expr {
        Expr::Literal(crate::ast::Literal::Integer(value), _) => Some(*value),
        Expr::Literal(crate::ast::Literal::NumericLit(value), _) => value.parse().ok(),
        Expr::UnaryOp {
            op: crate::ast::UnaryOperator::Minus,
            expr,
            ..
        } => match expr.as_ref() {
            Expr::Literal(crate::ast::Literal::Integer(value), _) => value.checked_neg(),
            Expr::Literal(crate::ast::Literal::NumericLit(value), _) => {
                value.parse::<i64>().ok().and_then(i64::checked_neg)
            }
            _ => None,
        },
        _ => None,
    }
}
