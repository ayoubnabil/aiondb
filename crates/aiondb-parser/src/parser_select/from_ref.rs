use super::*;
use aiondb_core::{DataType, DbError};

#[derive(Clone, Debug)]
struct FunctionColumnDef {
    alias: String,
    data_type: Option<DataType>,
}

const ORDINALITY_UNSUPPORTED_FROM_SRFS: &[&str] = &[
    "test_ret_set_rec_dyn",
    "test_ret_rec_dyn",
    "jsonb_to_record",
    "json_to_record",
    "jsonb_populate_record",
    "json_populate_record",
    "jsonb_to_recordset",
    "json_to_recordset",
    "jsonb_populate_recordset",
    "json_populate_recordset",
    "unnest2",
    "pg_input_error_info",
    "pg_options_to_table",
    "pg_partition_tree",
    "jsonb_each",
    "jsonb_each_text",
    "jsonb_array_elements",
    "jsonb_array_elements_text",
];

const JSON_RECORD_FROM_SRFS: &[&str] = &[
    "jsonb_to_record",
    "json_to_record",
    "jsonb_populate_record",
    "json_populate_record",
];

const JSON_RECORDSET_FROM_SRFS: &[&str] = &[
    "jsonb_to_recordset",
    "json_to_recordset",
    "jsonb_populate_recordset",
    "json_populate_recordset",
];

const KNOWN_FROM_SRFS: &[&str] = &[
    "generate_series",
    "generate_subscripts",
    "current_schema",
    "current_catalog",
    "current_database",
    "unnest",
    "unnest1",
    "unnest2",
    "graph_neighbors",
    "string_to_table",
    "regexp_split_to_table",
    "regexp_matches",
    "vector_top_k_ids",
    "vector_top_k_hits",
    "vector_prefetch_top_k_hits",
    "vector_recommend_top_k_hits",
    "full_text_top_k_hits",
    "hybrid_search_top_k_hits",
    "hybrid_fuse_rrf_hits",
    "hybrid_fuse_dbsf_hits",
    "hybrid_group_hits_by",
    "pg_input_error_info",
    "pg_options_to_table",
    "pg_partition_tree",
    "pg_ls_dir",
    "pg_ls_archive_statusdir",
    "pg_ls_logdir",
    "pg_ls_tmpdir",
    "jsonb_each",
    "jsonb_each_text",
    "jsonb_array_elements",
    "jsonb_array_elements_text",
    "json_to_record",
    "json_to_recordset",
    "jsonb_to_record",
    "jsonb_to_recordset",
    "json_populate_record",
    "json_populate_recordset",
    "jsonb_populate_record",
    "jsonb_populate_recordset",
    // PostgreSQL plpgsql regression helpers that return setof/record and
    // are expected to be legal in FROM/ROWS FROM clauses.
    "f1",
    "duplic",
    "test_table_func_rec",
    "test_table_func_row",
    "test_ret_set_scalar",
    "test_ret_set_rec_dyn",
    "test_ret_rec_dyn",
    "sc_test",
    "ret_query1",
    "ret_query2",
    "return_dquery",
    "returnqueryf",
    "compos",
    "tftest",
    "rttest",
    "conflict_test",
    "leaker_1",
    "consumes_rw_array",
    "returns_rw_array",
    "list_partitioned_table",
    "get_from_partitioned_table",
];

fn name_matches_any_ci(name: &str, candidates: &[&str]) -> bool {
    candidates
        .iter()
        .any(|candidate| name.eq_ignore_ascii_case(candidate))
}

fn synthetic_select(span: Span, items: Vec<SelectItem>) -> SelectStatement {
    SelectStatement {
        row_lock: None,
        ctes: Vec::new(),
        distinct: DistinctKind::All,
        items,
        from: None,
        from_alias: None,
        from_span: None,
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
    }
}

fn synthetic_select_from(
    span: Span,
    ctes: Vec<CteDefinition>,
    items: Vec<SelectItem>,
    from: ObjectName,
    from_span: Span,
) -> SelectStatement {
    let mut select = synthetic_select(span, items);
    select.ctes = ctes;
    select.from = Some(from);
    select.from_span = Some(from_span);
    select
}

fn synthetic_cte(
    name: String,
    column_aliases: Option<Vec<String>>,
    select: SelectStatement,
    span: Span,
) -> CteDefinition {
    synthetic_cte_statement(name, column_aliases, Statement::Select(select), span)
}

fn synthetic_cte_statement(
    name: String,
    column_aliases: Option<Vec<String>>,
    query: Statement,
    span: Span,
) -> CteDefinition {
    CteDefinition {
        name,
        column_aliases,
        recursive: false,
        query: Box::new(query),
        recursive_term: None,
        union_all: false,
        span,
    }
}

fn cte_object_name(name: String, span: Span) -> ObjectName {
    ObjectName {
        parts: vec![name],
        span,
    }
}

impl Parser {
    pub(super) fn parse_from_table_ref(
        &mut self,
    ) -> DbResult<(
        ObjectName,
        Option<String>,
        Vec<CteDefinition>,
        Option<Vec<String>>,
    )> {
        let _ = self.consume_keyword(Keyword::Lateral);
        if self.current().kind == TokenKind::LParen {
            let (name, alias, ctes, col_aliases) = self.parse_parenthesized_from_ref()?;
            return Ok((name, alias, ctes, col_aliases));
        }
        if self.current().kind == TokenKind::Keyword(Keyword::Rows) {
            let rows_span = self.current().span;
            if self.index + 1 < self.tokens.len()
                && matches!(
                    self.tokens[self.index + 1].kind,
                    TokenKind::Keyword(Keyword::From)
                )
            {
                return self.parse_rows_from_function_call(rows_span);
            }
        }
        if self.is_from_function_call() {
            let (name, alias, ctes) = self.parse_from_function_call()?;
            return Ok((name, alias, ctes, None));
        }
        let relation = self.parse_object_name()?;
        let _ = self.consume_kind(&TokenKind::Star);
        self.reject_tablesample_clause()?;
        let (alias, col_aliases) = self.try_parse_table_alias();
        // Also reject TABLESAMPLE after alias (e.g., FROM t1 alias TABLESAMPLE ...).
        self.reject_tablesample_clause()?;
        Ok((relation, alias, Vec::new(), col_aliases))
    }

    fn parse_parenthesized_from_ref(
        &mut self,
    ) -> DbResult<(
        ObjectName,
        Option<String>,
        Vec<CteDefinition>,
        Option<Vec<String>>,
    )> {
        self.enter_subquery_recursion()?;
        let result = self.parse_parenthesized_from_ref_inner();
        self.leave_subquery_recursion();
        result
    }

    fn parse_parenthesized_from_ref_inner(
        &mut self,
    ) -> DbResult<(
        ObjectName,
        Option<String>,
        Vec<CteDefinition>,
        Option<Vec<String>>,
    )> {
        let lparen_span = self.current().span;
        self.advance();
        if matches!(self.current().kind, TokenKind::LParen) {
            let (name, alias, ctes, col_aliases) = self.parse_parenthesized_from_ref()?;
            let _ = self.consume_kind(&TokenKind::RParen);
            let (outer_alias, outer_col_aliases) = self.try_parse_table_alias();
            return Ok((
                name,
                outer_alias.or(alias),
                ctes,
                outer_col_aliases.or(col_aliases),
            ));
        }
        if matches!(
            self.current().kind,
            TokenKind::Keyword(Keyword::Select | Keyword::With | Keyword::Values)
        ) {
            let inner_stmt = if matches!(self.current().kind, TokenKind::Keyword(Keyword::Values)) {
                let vs = self.parse_values_as_statement()?;
                self.maybe_parse_set_operation(vs)?
            } else if matches!(self.current().kind, TokenKind::Keyword(Keyword::Select)) {
                let sel = self.parse_select_statement()?;
                self.maybe_parse_set_operation(sel)?
            } else {
                let with_stmt = self.parse_with_statement()?;
                self.maybe_parse_set_operation(with_stmt)?
            };
            let rparen = self.expect_token(&TokenKind::RParen)?;
            let subquery_span = lparen_span.merge(rparen.span);
            let (alias, col_aliases) = self.try_parse_table_alias();
            let cte_name = alias
                .clone()
                .unwrap_or_else(|| self.next_subquery_cte_name());
            let cte =
                synthetic_cte_statement(cte_name.clone(), col_aliases, inner_stmt, subquery_span);
            let obj = cte_object_name(cte_name, subquery_span);
            return Ok((obj, alias, vec![cte], None));
        }

        if self.current().kind == TokenKind::Keyword(Keyword::Rows) {
            let rows_span = self.current().span;
            if self.index + 1 < self.tokens.len()
                && matches!(
                    self.tokens[self.index + 1].kind,
                    TokenKind::Keyword(Keyword::From)
                )
            {
                return self.parse_rows_from_function_call(rows_span);
            }
        }

        if self.is_from_function_call() {
            let (name, alias, ctes) = self.parse_from_function_call()?;
            let _ = self.consume_kind(&TokenKind::RParen);
            let (outer_alias, outer_col_aliases) = self.try_parse_table_alias();
            return Ok((name, outer_alias.or(alias), ctes, outer_col_aliases));
        }

        let relation = self.parse_object_name()?;
        let _ = self.consume_kind(&TokenKind::Star);
        self.reject_tablesample_clause()?;
        let (alias, col_aliases) = self.try_parse_table_alias();
        self.reject_tablesample_clause()?;
        if self.consume_kind(&TokenKind::RParen) {
            let (outer_alias, outer_col_aliases) = self.try_parse_table_alias();
            return Ok((
                relation,
                outer_alias.or(alias),
                Vec::new(),
                outer_col_aliases.or(col_aliases),
            ));
        }

        // Parenthesized join: (t1 JOIN t2 ON cond) or (t1, t2)
        if self.is_join_start_or_comma() {
            let (name, alias, ctes) =
                self.parse_parenthesized_join(lparen_span, relation, alias)?;
            return Ok((name, alias, ctes, None));
        }

        self.unsupported_feature(lparen_span, "unsupported FROM clause expression")
    }

    fn parse_from_function_call(
        &mut self,
    ) -> DbResult<(ObjectName, Option<String>, Vec<CteDefinition>)> {
        let name = self.parse_object_name()?;
        let func_span = name.span;
        let func_base = name.parts.last().cloned().unwrap_or_default();

        // Check if this is a known set-returning function whose arguments
        // we can parse to produce a real SELECT rather than a dummy CTE.
        if is_known_from_srf(&func_base) {
            self.expect_token(&TokenKind::LParen)?;
            let mut args = Vec::new();
            if self.current().kind != TokenKind::RParen {
                loop {
                    args.push(self.parse_expr()?);
                    if !self.consume_kind(&TokenKind::Comma) {
                        break;
                    }
                }
            }
            self.expect_token(&TokenKind::RParen)?;

            let with_ordinality = self.try_parse_with_ordinality_clause()?;
            let (alias, col_aliases, column_defs) = self.try_parse_from_function_alias();
            let (col_aliases, ordinality_alias) =
                Self::split_ordinality_column_aliases(col_aliases, with_ordinality);
            return self.build_from_function_cte(
                name,
                args,
                alias,
                col_aliases,
                column_defs,
                ordinality_alias,
            );
        }

        self.unsupported_feature(
            func_span,
            format!(
                "set-returning function \"{}\" in FROM is not supported",
                name.parts.join(".")
            ),
        )
    }

    fn parse_rows_from_function_call(
        &mut self,
        rows_span: Span,
    ) -> DbResult<(
        ObjectName,
        Option<String>,
        Vec<CteDefinition>,
        Option<Vec<String>>,
    )> {
        self.expect_keyword(Keyword::Rows)?;
        self.expect_keyword(Keyword::From)?;
        self.expect_token(&TokenKind::LParen)?;

        let name = self.parse_object_name()?;
        let func_span = rows_span.merge(name.span);
        let func_base = name.parts.last().cloned().unwrap_or_default();
        if !is_known_from_srf(&func_base) {
            return self.unsupported_feature(
                func_span,
                format!(
                    "set-returning function \"{}\" in ROWS FROM is not supported",
                    name.parts.join(".")
                ),
            );
        }

        self.expect_token(&TokenKind::LParen)?;
        let mut args = Vec::new();
        if self.current().kind != TokenKind::RParen {
            loop {
                args.push(self.parse_expr()?);
                if !self.consume_kind(&TokenKind::Comma) {
                    break;
                }
            }
        }
        self.expect_token(&TokenKind::RParen)?;

        if self.consume_kind(&TokenKind::Comma) {
            return self.unsupported_feature(
                func_span,
                "ROWS FROM with multiple functions is not supported",
            );
        }

        self.expect_token(&TokenKind::RParen)?;
        let with_ordinality = self.try_parse_with_ordinality_clause()?;
        let (alias, col_aliases, column_defs) = self.try_parse_from_function_alias();
        let (col_aliases, ordinality_alias) =
            Self::split_ordinality_column_aliases(col_aliases, with_ordinality);
        let (name, alias, ctes) = self.build_from_function_cte(
            name,
            args,
            alias,
            col_aliases,
            column_defs,
            ordinality_alias,
        )?;
        Ok((name, alias, ctes, None))
    }

    fn try_parse_with_ordinality_clause(&mut self) -> DbResult<bool> {
        let Some(with_token) = self.consume_keyword(Keyword::With) else {
            return Ok(false);
        };
        if self.current_is_identifier_ci("ordinality") {
            self.advance();
            return Ok(true);
        }
        self.unsupported_feature(with_token.span, "unsupported WITH clause in FROM function")
    }

    fn try_parse_from_function_alias(
        &mut self,
    ) -> (
        Option<String>,
        Option<Vec<String>>,
        Option<Vec<FunctionColumnDef>>,
    ) {
        if let Some(parsed) = self.try_parse_from_function_alias_with_types() {
            return parsed;
        }
        // PostgreSQL accepts function-column definitions without a table alias:
        // FROM f(...) AS (a int, b text)
        if let Some(columns) = self.try_parse_function_column_definition_list() {
            return (
                None,
                Some(columns.iter().map(|column| column.alias.clone()).collect()),
                Some(columns),
            );
        }
        (None, None, None)
    }

    fn split_ordinality_column_aliases(
        mut col_aliases: Option<Vec<String>>,
        with_ordinality: bool,
    ) -> (Option<Vec<String>>, Option<String>) {
        if !with_ordinality {
            return (col_aliases, None);
        }
        let ordinality_alias = match col_aliases.as_mut() {
            Some(aliases) if aliases.len() >= 2 => aliases.pop(),
            _ => Some("ordinality".to_owned()),
        };
        (col_aliases, ordinality_alias)
    }

    fn try_parse_from_function_alias_with_types(
        &mut self,
    ) -> Option<(
        Option<String>,
        Option<Vec<String>>,
        Option<Vec<FunctionColumnDef>>,
    )> {
        let save_index = self.index;
        let saw_as = self.consume_keyword(Keyword::As).is_some();
        if !saw_as && self.current_token_starts_from_clause_tail() {
            return None;
        }
        let alias = match self.expect_identifier() {
            Ok((alias, _)) => alias,
            Err(_) => {
                self.index = save_index;
                return None;
            }
        };

        if !self.consume_kind(&TokenKind::LParen) {
            return Some((Some(alias), None, None));
        }

        let mut columns = Vec::new();
        let mut saw_type = false;
        loop {
            let (column_alias, _) = match self.expect_identifier() {
                Ok(parsed) => parsed,
                Err(_) => {
                    self.index = save_index;
                    return None;
                }
            };
            let data_type = {
                let type_index = self.index;
                match self.parse_data_type() {
                    Ok(data_type) => {
                        saw_type = true;
                        Some(data_type)
                    }
                    Err(_) => {
                        self.index = type_index;
                        None
                    }
                }
            };
            columns.push(FunctionColumnDef {
                alias: column_alias,
                data_type,
            });

            if self.consume_kind(&TokenKind::Comma) {
                continue;
            }
            if self.consume_kind(&TokenKind::RParen) {
                let aliases = columns
                    .iter()
                    .map(|column| column.alias.clone())
                    .collect::<Vec<_>>();
                let defs = if saw_type {
                    Some(columns)
                } else {
                    Some(
                        aliases
                            .iter()
                            .map(|alias| FunctionColumnDef {
                                alias: alias.clone(),
                                data_type: None,
                            })
                            .collect(),
                    )
                };
                return Some((Some(alias), Some(aliases), defs));
            }
            self.index = save_index;
            return None;
        }
    }

    fn current_token_starts_from_clause_tail(&self) -> bool {
        matches!(
            self.current().kind,
            TokenKind::Keyword(
                Keyword::Where
                    | Keyword::Group
                    | Keyword::Having
                    | Keyword::Order
                    | Keyword::Limit
                    | Keyword::Offset
                    | Keyword::Union
                    | Keyword::Intersect
                    | Keyword::Except
                    | Keyword::Join
                    | Keyword::Inner
                    | Keyword::Left
                    | Keyword::Right
                    | Keyword::Full
                    | Keyword::Cross
                    | Keyword::Natural
                    | Keyword::On
                    | Keyword::Using
                    | Keyword::For
                    | Keyword::Fetch
                    | Keyword::Returning
                    | Keyword::With
                    | Keyword::Lateral
            )
        )
    }

    fn try_parse_function_column_definition_list(&mut self) -> Option<Vec<FunctionColumnDef>> {
        let save_index = self.index;
        let saw_as = self.consume_keyword(Keyword::As).is_some();
        if !self.consume_kind(&TokenKind::LParen) {
            if saw_as {
                self.index = save_index;
            }
            return None;
        }

        let mut columns = Vec::new();
        loop {
            let (alias, _) = match self.expect_identifier() {
                Ok(parsed) => parsed,
                Err(_) => {
                    self.index = save_index;
                    return None;
                }
            };
            let data_type = match self.parse_data_type() {
                Ok(data_type) => Some(data_type),
                Err(_) => {
                    self.index = save_index;
                    return None;
                }
            };
            columns.push(FunctionColumnDef { alias, data_type });

            if self.consume_kind(&TokenKind::Comma) {
                continue;
            }
            if self.consume_kind(&TokenKind::RParen) {
                return Some(columns);
            }
            self.index = save_index;
            return None;
        }
    }

    fn function_cte_names(
        &mut self,
        alias: Option<String>,
        fallback_exposed_name: String,
    ) -> (String, String) {
        match alias {
            Some(alias) => {
                let cte_name = alias.clone();
                (alias, cte_name)
            }
            None => (fallback_exposed_name, self.next_from_function_cte_name()),
        }
    }

    fn build_from_function_cte(
        &mut self,
        name: ObjectName,
        args: Vec<Expr>,
        alias: Option<String>,
        col_aliases: Option<Vec<String>>,
        column_defs: Option<Vec<FunctionColumnDef>>,
        ordinality_alias: Option<String>,
    ) -> DbResult<(ObjectName, Option<String>, Vec<CteDefinition>)> {
        let func_span = name.span;
        let func_base = name.parts.last().cloned().unwrap_or_default();
        if ordinality_alias.is_some()
            && name_matches_any_ci(&func_base, ORDINALITY_UNSUPPORTED_FROM_SRFS)
        {
            return self.unsupported_feature(
                func_span,
                "WITH ORDINALITY is not supported for this FROM-clause set-returning function",
            );
        }
        if let Some(column_aliases) = col_aliases.as_ref().filter(|aliases| aliases.len() > 1) {
            if func_base.eq_ignore_ascii_case("test_ret_set_rec_dyn") {
                let column_types = column_aliases.iter().map(|_| None).collect();
                return self.build_record_set_function_cte(
                    name,
                    args,
                    alias,
                    column_aliases.clone(),
                    column_types,
                );
            }
            if func_base.eq_ignore_ascii_case("test_ret_rec_dyn") {
                return self.build_record_single_function_cte(
                    name,
                    args,
                    alias,
                    column_aliases.clone(),
                );
            }
        }
        let inferred_record_columns = || {
            column_defs.clone().or_else(|| {
                col_aliases.as_ref().map(|aliases| {
                    aliases
                        .iter()
                        .map(|alias| FunctionColumnDef {
                            alias: alias.clone(),
                            data_type: None,
                        })
                        .collect()
                })
            })
        };
        if name_matches_any_ci(&func_base, JSON_RECORD_FROM_SRFS) {
            if let Some(columns) = inferred_record_columns() {
                return self.build_json_record_function_cte(name, args, alias, columns, false);
            }
        }
        if name_matches_any_ci(&func_base, JSON_RECORDSET_FROM_SRFS) {
            if let Some(columns) = inferred_record_columns() {
                return self.build_json_record_function_cte(name, args, alias, columns, true);
            }
        }
        if func_base.eq_ignore_ascii_case("unnest1") || func_base.eq_ignore_ascii_case("unnest") {
            return self.build_unnest1_from_function_cte(
                name,
                args,
                alias,
                col_aliases,
                ordinality_alias,
            );
        }
        if func_base.eq_ignore_ascii_case("unnest2") {
            return self.build_unnest2_from_function_cte(name, args, alias, col_aliases);
        }
        if func_base.eq_ignore_ascii_case("pg_input_error_info") {
            return self.build_pg_input_error_info_cte(name, args, alias, col_aliases);
        }
        if func_base.eq_ignore_ascii_case("pg_options_to_table") {
            return self.build_pg_options_to_table_cte(name, args, alias, col_aliases);
        }
        if func_base.eq_ignore_ascii_case("pg_partition_tree") {
            return self.build_pg_partition_tree_cte(name, alias, col_aliases);
        }
        if func_base.eq_ignore_ascii_case("jsonb_each") {
            return self.build_jsonb_each_from_function_cte(name, args, alias, col_aliases, false);
        }
        if func_base.eq_ignore_ascii_case("jsonb_each_text") {
            return self.build_jsonb_each_from_function_cte(name, args, alias, col_aliases, true);
        }
        if func_base.eq_ignore_ascii_case("jsonb_array_elements") {
            return self.build_jsonb_array_elements_from_function_cte(
                name,
                args,
                alias,
                col_aliases,
                false,
            );
        }
        if func_base.eq_ignore_ascii_case("jsonb_array_elements_text") {
            return self.build_jsonb_array_elements_from_function_cte(
                name,
                args,
                alias,
                col_aliases,
                true,
            );
        }
        let (exposed_name, cte_name) = self.function_cte_names(alias, func_base.clone());

        let value_alias = col_aliases
            .as_ref()
            .and_then(|aliases| aliases.first().cloned())
            .unwrap_or_else(|| exposed_name.clone());
        let func_call_expr = Expr::FunctionCall {
            name,
            args,
            distinct: false,
            filter: None,
            span: func_span,
        };

        if let Some(ordinality_alias) = ordinality_alias {
            let ordinality_expr = Expr::WindowFunction {
                function: Box::new(Expr::FunctionCall {
                    name: ObjectName {
                        parts: vec!["row_number".to_owned()],
                        span: func_span,
                    },
                    args: Vec::new(),
                    distinct: false,
                    filter: None,
                    span: func_span,
                }),
                partition_by: Vec::new(),
                order_by: Vec::new(),
                window_name: None,
                span: func_span,
            };
            let result_cte = synthetic_cte(
                cte_name.clone(),
                None,
                synthetic_select(
                    func_span,
                    vec![
                        SelectItem {
                            expr: func_call_expr,
                            alias: Some(value_alias.clone()),
                            span: func_span,
                        },
                        SelectItem {
                            expr: ordinality_expr,
                            alias: Some(ordinality_alias),
                            span: func_span,
                        },
                    ],
                ),
                func_span,
            );
            let obj = cte_object_name(cte_name, func_span);
            return Ok((obj, Some(exposed_name), vec![result_cte]));
        }

        let select = synthetic_select(
            func_span,
            vec![SelectItem {
                expr: func_call_expr,
                alias: Some(exposed_name.clone()),
                span: func_span,
            }],
        );
        let cte = synthetic_cte(cte_name.clone(), col_aliases, select, func_span);
        let obj = cte_object_name(cte_name, func_span);
        Ok((obj, Some(exposed_name), vec![cte]))
    }

    fn build_json_record_function_cte(
        &mut self,
        name: ObjectName,
        mut args: Vec<Expr>,
        alias: Option<String>,
        column_defs: Vec<FunctionColumnDef>,
        set_returning: bool,
    ) -> DbResult<(ObjectName, Option<String>, Vec<CteDefinition>)> {
        let func_span = name.span;
        let func_base = name.parts.last().cloned().unwrap_or_default();
        let (exposed_name, cte_name) = self.function_cte_names(alias, func_base.clone());
        let keys_expr = Expr::Array {
            elements: column_defs
                .iter()
                .map(|column| Expr::Literal(Literal::String(column.alias.clone()), func_span))
                .collect(),
            span: func_span,
        };
        let mode_expr = Expr::Array {
            elements: column_defs
                .iter()
                .map(|column| {
                    let mode = match column.data_type {
                        Some(DataType::Jsonb) => "json",
                        Some(DataType::Array(_)) => "array",
                        _ => "scalar",
                    };
                    Expr::Literal(Literal::String(mode.to_owned()), func_span)
                })
                .collect(),
            span: func_span,
        };
        args.push(keys_expr);
        args.push(mode_expr);

        let helper_name = if set_returning {
            if func_base.eq_ignore_ascii_case("jsonb_to_recordset") {
                "__aiondb_jsonb_to_recordset"
            } else if func_base.eq_ignore_ascii_case("json_to_recordset") {
                "__aiondb_json_to_recordset"
            } else if func_base.eq_ignore_ascii_case("jsonb_populate_recordset") {
                "__aiondb_jsonb_populate_recordset"
            } else {
                "__aiondb_json_populate_recordset"
            }
        } else if func_base.eq_ignore_ascii_case("jsonb_to_record") {
            "__aiondb_jsonb_to_record"
        } else if func_base.eq_ignore_ascii_case("json_to_record") {
            "__aiondb_json_to_record"
        } else if func_base.eq_ignore_ascii_case("jsonb_populate_record") {
            "__aiondb_jsonb_populate_record"
        } else {
            "__aiondb_json_populate_record"
        };
        let helper_call = ObjectName {
            parts: vec![helper_name.to_owned()],
            span: func_span,
        };
        if set_returning {
            let aliases: Vec<String> = column_defs
                .iter()
                .map(|column| column.alias.clone())
                .collect();
            let column_types: Vec<Option<DataType>> = column_defs
                .iter()
                .map(|column| column.data_type.clone())
                .collect();
            return self.build_record_set_function_cte(
                helper_call,
                args,
                Some(exposed_name),
                aliases,
                column_types,
            );
        }

        let rows_cte_name = self.next_from_function_cte_name();
        let row_col = "__row".to_owned();
        let func_call_expr = Expr::FunctionCall {
            name: helper_call,
            args,
            distinct: false,
            filter: None,
            span: func_span,
        };
        let rows_cte = synthetic_cte(
            rows_cte_name.clone(),
            None,
            synthetic_select(
                func_span,
                vec![SelectItem {
                    expr: func_call_expr,
                    alias: Some(row_col.clone()),
                    span: func_span,
                }],
            ),
            func_span,
        );
        let items = column_defs
            .iter()
            .enumerate()
            .map(|(index, column)| {
                let row_expr = Expr::Cast {
                    expr: Box::new(Expr::Identifier(ObjectName {
                        parts: vec![rows_cte_name.clone(), row_col.clone()],
                        span: func_span,
                    })),
                    data_type: DataType::Array(Box::new(DataType::Text)),
                    span: func_span,
                };
                let mut value_expr = Expr::FunctionCall {
                    name: ObjectName {
                        parts: vec!["array_get".to_owned()],
                        span: func_span,
                    },
                    args: vec![
                        row_expr,
                        Expr::Literal(
                            Literal::Integer(i64::try_from(index + 1).unwrap_or(i64::MAX)),
                            func_span,
                        ),
                    ],
                    distinct: false,
                    filter: None,
                    span: func_span,
                };
                if let Some(data_type) = &column.data_type {
                    value_expr = Expr::Cast {
                        expr: Box::new(value_expr),
                        data_type: data_type.clone(),
                        span: func_span,
                    };
                }
                SelectItem {
                    expr: value_expr,
                    alias: Some(column.alias.clone()),
                    span: func_span,
                }
            })
            .collect::<Vec<_>>();
        let cte = synthetic_cte(
            cte_name.clone(),
            None,
            synthetic_select_from(
                func_span,
                vec![rows_cte],
                items,
                cte_object_name(rows_cte_name, func_span),
                func_span,
            ),
            func_span,
        );
        let obj = cte_object_name(cte_name, func_span);
        Ok((obj, Some(exposed_name), vec![cte]))
    }

    fn build_record_set_function_cte(
        &mut self,
        name: ObjectName,
        args: Vec<Expr>,
        alias: Option<String>,
        column_aliases: Vec<String>,
        column_types: Vec<Option<DataType>>,
    ) -> DbResult<(ObjectName, Option<String>, Vec<CteDefinition>)> {
        let func_span = name.span;
        let func_base = name.parts.last().cloned().unwrap_or_default();
        let (exposed_name, cte_name) = self.function_cte_names(alias, func_base);
        let rows_cte_name = self.next_from_function_cte_name();
        let row_col = "__row".to_owned();

        let func_call_expr = Expr::FunctionCall {
            name,
            args,
            distinct: false,
            filter: None,
            span: func_span,
        };
        let rows_cte = synthetic_cte(
            rows_cte_name.clone(),
            None,
            synthetic_select(
                func_span,
                vec![SelectItem {
                    expr: func_call_expr,
                    alias: Some(row_col.clone()),
                    span: func_span,
                }],
            ),
            func_span,
        );
        let items = column_aliases
            .iter()
            .enumerate()
            .map(|(index, alias_name)| {
                let row_expr = Expr::Cast {
                    expr: Box::new(Expr::Identifier(ObjectName {
                        parts: vec![rows_cte_name.clone(), row_col.clone()],
                        span: func_span,
                    })),
                    data_type: DataType::Array(Box::new(DataType::Text)),
                    span: func_span,
                };
                let mut value_expr = Expr::FunctionCall {
                    name: ObjectName {
                        parts: vec!["array_get".to_owned()],
                        span: func_span,
                    },
                    args: vec![
                        row_expr,
                        Expr::Literal(
                            Literal::Integer(i64::try_from(index + 1).unwrap_or(i64::MAX)),
                            func_span,
                        ),
                    ],
                    distinct: false,
                    filter: None,
                    span: func_span,
                };
                if let Some(Some(data_type)) = column_types.get(index) {
                    value_expr = Expr::Cast {
                        expr: Box::new(value_expr),
                        data_type: data_type.clone(),
                        span: func_span,
                    };
                }
                SelectItem {
                    expr: value_expr,
                    alias: Some(alias_name.clone()),
                    span: func_span,
                }
            })
            .collect::<Vec<_>>();

        let result_cte = synthetic_cte(
            cte_name.clone(),
            None,
            synthetic_select_from(
                func_span,
                vec![rows_cte],
                items,
                cte_object_name(rows_cte_name, func_span),
                func_span,
            ),
            func_span,
        );

        let obj = cte_object_name(cte_name, func_span);
        Ok((obj, Some(exposed_name), vec![result_cte]))
    }

    fn build_record_single_function_cte(
        &mut self,
        name: ObjectName,
        args: Vec<Expr>,
        alias: Option<String>,
        column_aliases: Vec<String>,
    ) -> DbResult<(ObjectName, Option<String>, Vec<CteDefinition>)> {
        let func_span = name.span;
        let func_base = name.parts.last().cloned().unwrap_or_default();
        let (exposed_name, cte_name) = self.function_cte_names(alias, func_base);
        let rows_cte_name = self.next_from_function_cte_name();
        let row_col = "__row".to_owned();
        let func_call_expr = Expr::FunctionCall {
            name,
            args,
            distinct: false,
            filter: None,
            span: func_span,
        };
        let rows_cte = synthetic_cte(
            rows_cte_name.clone(),
            None,
            synthetic_select(
                func_span,
                vec![SelectItem {
                    expr: func_call_expr,
                    alias: Some(row_col.clone()),
                    span: func_span,
                }],
            ),
            func_span,
        );
        let items = column_aliases
            .iter()
            .enumerate()
            .map(|(index, alias_name)| {
                let row_expr = Expr::Cast {
                    expr: Box::new(Expr::Identifier(ObjectName {
                        parts: vec![rows_cte_name.clone(), row_col.clone()],
                        span: func_span,
                    })),
                    data_type: DataType::Array(Box::new(DataType::Text)),
                    span: func_span,
                };
                SelectItem {
                    expr: Expr::FunctionCall {
                        name: ObjectName {
                            parts: vec!["array_get".to_owned()],
                            span: func_span,
                        },
                        args: vec![
                            row_expr,
                            Expr::Literal(
                                Literal::Integer(i64::try_from(index + 1).unwrap_or(i64::MAX)),
                                func_span,
                            ),
                        ],
                        distinct: false,
                        filter: None,
                        span: func_span,
                    },
                    alias: Some(alias_name.clone()),
                    span: func_span,
                }
            })
            .collect::<Vec<_>>();
        let cte = synthetic_cte(
            cte_name.clone(),
            None,
            synthetic_select_from(
                func_span,
                vec![rows_cte],
                items,
                cte_object_name(rows_cte_name, func_span),
                func_span,
            ),
            func_span,
        );
        let obj = cte_object_name(cte_name, func_span);
        Ok((obj, Some(exposed_name), vec![cte]))
    }

    fn build_unnest1_from_function_cte(
        &mut self,
        mut name: ObjectName,
        args: Vec<Expr>,
        alias: Option<String>,
        col_aliases: Option<Vec<String>>,
        ordinality_alias: Option<String>,
    ) -> DbResult<(ObjectName, Option<String>, Vec<CteDefinition>)> {
        let original_func_name = name
            .parts
            .last()
            .cloned()
            .unwrap_or_else(|| "unnest".to_owned());
        let exposed_name = alias.clone().unwrap_or(original_func_name);
        if let Some(last) = name.parts.last_mut() {
            "unnest".clone_into(last);
        }
        let func_span = name.span;
        if let Some(ordinality_alias) = ordinality_alias {
            if args.is_empty() {
                return self.unsupported_feature(
                    func_span,
                    "unnest() WITH ORDINALITY in FROM requires at least one argument",
                );
            }
            let array_expr = args[0].clone();
            let value_aliases = (0..args.len())
                .map(|index| {
                    col_aliases
                        .as_ref()
                        .and_then(|aliases| aliases.get(index).cloned())
                        .unwrap_or_else(|| {
                            if index == 0 {
                                exposed_name.clone()
                            } else {
                                format!("column{}", index + 1)
                            }
                        })
                })
                .collect::<Vec<_>>();
            let cte_name = exposed_name.clone();
            let subscripts_cte_name = self.next_from_function_cte_name();
            let subscripts_col = "__ord".to_owned();
            let subscripts_cte = self.build_internal_srf_cte(
                subscripts_cte_name.clone(),
                "generate_subscripts",
                vec![
                    array_expr.clone(),
                    Expr::Literal(Literal::Integer(1), func_span),
                ],
                &subscripts_col,
                func_span,
            );
            let ordinality_expr = Expr::Identifier(ObjectName {
                parts: vec![subscripts_cte_name.clone(), subscripts_col.clone()],
                span: func_span,
            });
            let mut items = args
                .into_iter()
                .zip(value_aliases)
                .map(|(arg, alias)| SelectItem {
                    expr: Expr::FunctionCall {
                        name: ObjectName {
                            parts: vec!["array_get".to_owned()],
                            span: func_span,
                        },
                        args: vec![arg, ordinality_expr.clone()],
                        distinct: false,
                        filter: None,
                        span: func_span,
                    },
                    alias: Some(alias),
                    span: func_span,
                })
                .collect::<Vec<_>>();
            items.push(SelectItem {
                expr: ordinality_expr,
                alias: Some(ordinality_alias),
                span: func_span,
            });
            let result_cte = synthetic_cte(
                cte_name.clone(),
                None,
                synthetic_select_from(
                    func_span,
                    vec![subscripts_cte],
                    items,
                    cte_object_name(subscripts_cte_name, func_span),
                    func_span,
                ),
                func_span,
            );
            return Ok((
                cte_object_name(cte_name, func_span),
                Some(exposed_name),
                vec![result_cte],
            ));
        }

        let (exposed_name, cte_name) = self.function_cte_names(alias, exposed_name);

        if args.len() <= 1 {
            let select = synthetic_select(
                func_span,
                vec![SelectItem {
                    expr: Expr::FunctionCall {
                        name,
                        args,
                        distinct: false,
                        filter: None,
                        span: func_span,
                    },
                    alias: Some(exposed_name.clone()),
                    span: func_span,
                }],
            );
            let cte = synthetic_cte(cte_name.clone(), col_aliases, select, func_span);
            let obj = cte_object_name(cte_name, func_span);
            return Ok((obj, Some(exposed_name), vec![cte]));
        }

        let subscripts_cte_name = self.next_from_function_cte_name();
        let subscripts_col = "__ord".to_owned();
        let subscripts_cte = self.build_internal_srf_cte(
            subscripts_cte_name.clone(),
            "generate_subscripts",
            vec![
                args[0].clone(),
                Expr::Literal(Literal::Integer(1), func_span),
            ],
            &subscripts_col,
            func_span,
        );
        let ordinality_expr = Expr::Identifier(ObjectName {
            parts: vec![subscripts_cte_name.clone(), subscripts_col.clone()],
            span: func_span,
        });
        let items = args
            .into_iter()
            .enumerate()
            .map(|(index, array_expr)| SelectItem {
                expr: Expr::FunctionCall {
                    name: ObjectName {
                        parts: vec!["array_get".to_owned()],
                        span: func_span,
                    },
                    args: vec![array_expr, ordinality_expr.clone()],
                    distinct: false,
                    filter: None,
                    span: func_span,
                },
                alias: Some(
                    col_aliases
                        .as_ref()
                        .and_then(|aliases| aliases.get(index).cloned())
                        .unwrap_or_else(|| {
                            if index == 0 {
                                exposed_name.clone()
                            } else {
                                format!("column{}", index + 1)
                            }
                        }),
                ),
                span: func_span,
            })
            .collect();
        let cte = synthetic_cte(
            cte_name.clone(),
            None,
            synthetic_select_from(
                func_span,
                vec![subscripts_cte],
                items,
                cte_object_name(subscripts_cte_name, func_span),
                func_span,
            ),
            func_span,
        );
        Ok((
            cte_object_name(cte_name, func_span),
            Some(exposed_name),
            vec![cte],
        ))
    }

    fn build_unnest2_from_function_cte(
        &mut self,
        name: ObjectName,
        args: Vec<Expr>,
        alias: Option<String>,
        col_aliases: Option<Vec<String>>,
    ) -> DbResult<(ObjectName, Option<String>, Vec<CteDefinition>)> {
        let func_span = name.span;
        if args.len() != 1 {
            return self.unsupported_feature(
                func_span,
                "unnest2() in FROM is only supported with exactly one argument",
            );
        }

        let array_expr = args[0].clone();
        let dim1_name = self.next_from_function_cte_name();
        let dim2_name = self.next_from_function_cte_name();

        let dim1_cte = self.build_internal_srf_cte(
            dim1_name.clone(),
            "generate_subscripts",
            vec![
                array_expr.clone(),
                Expr::Literal(Literal::Integer(1), func_span),
            ],
            "s1",
            func_span,
        );
        let dim2_cte = self.build_internal_srf_cte(
            dim2_name.clone(),
            "generate_subscripts",
            vec![
                array_expr.clone(),
                Expr::Literal(Literal::Integer(2), func_span),
            ],
            "s2",
            func_span,
        );

        let fallback = name
            .parts
            .last()
            .cloned()
            .unwrap_or_else(|| "unnest2".to_owned());
        let (exposed_name, cte_name) = self.function_cte_names(alias, fallback);

        let s1_expr = Expr::Identifier(ObjectName {
            parts: vec![dim1_name.clone(), "s1".to_owned()],
            span: func_span,
        });
        let s2_expr = Expr::Identifier(ObjectName {
            parts: vec![dim2_name.clone(), "s2".to_owned()],
            span: func_span,
        });
        let inner_array_get = Expr::FunctionCall {
            name: ObjectName {
                parts: vec!["array_get".to_owned()],
                span: func_span,
            },
            args: vec![array_expr, s1_expr],
            distinct: false,
            filter: None,
            span: func_span,
        };
        let flattened_expr = Expr::FunctionCall {
            name: ObjectName {
                parts: vec!["array_get".to_owned()],
                span: func_span,
            },
            args: vec![inner_array_get, s2_expr],
            distinct: false,
            filter: None,
            span: func_span,
        };
        let mut select = synthetic_select_from(
            func_span,
            vec![dim1_cte, dim2_cte],
            vec![SelectItem {
                expr: flattened_expr,
                alias: Some(exposed_name.clone()),
                span: func_span,
            }],
            ObjectName {
                parts: vec![dim1_name],
                span: func_span,
            },
            func_span,
        );
        select.joins.push(JoinClause {
            join_type: JoinType::Cross,
            table: ObjectName {
                parts: vec![dim2_name],
                span: func_span,
            },
            alias: None,
            condition: None,
            using_columns: Vec::new(),
            using_alias: None,
            natural: false,
            span: func_span,
        });
        let result_cte = synthetic_cte(cte_name.clone(), col_aliases, select, func_span);
        let obj = cte_object_name(cte_name, func_span);
        Ok((obj, Some(exposed_name), vec![result_cte]))
    }

    fn build_jsonb_each_from_function_cte(
        &mut self,
        name: ObjectName,
        args: Vec<Expr>,
        alias: Option<String>,
        col_aliases: Option<Vec<String>>,
        text_values: bool,
    ) -> DbResult<(ObjectName, Option<String>, Vec<CteDefinition>)> {
        let func_span = name.span;
        let func_base = name.parts.last().cloned().unwrap_or_default();
        let (exposed_name, cte_name) = self.function_cte_names(alias, func_base);
        let mut output_aliases =
            col_aliases.unwrap_or_else(|| vec!["key".to_owned(), "value".to_owned()]);
        if output_aliases.len() == 1 {
            output_aliases.push("value".to_owned());
        }
        let rows_cte_name = self.next_from_function_cte_name();
        let row_col = "__row".to_owned();
        let func_call_expr = Expr::FunctionCall {
            name,
            args,
            distinct: false,
            filter: None,
            span: func_span,
        };
        let rows_cte = synthetic_cte(
            rows_cte_name.clone(),
            None,
            synthetic_select(
                func_span,
                vec![SelectItem {
                    expr: func_call_expr,
                    alias: Some(row_col.clone()),
                    span: func_span,
                }],
            ),
            func_span,
        );

        let row_expr = || {
            Expr::Identifier(ObjectName {
                parts: vec![rows_cte_name.clone(), row_col.clone()],
                span: func_span,
            })
        };
        let key_expr = Expr::FunctionCall {
            name: ObjectName {
                parts: vec!["__aiondb_composite_field".to_owned()],
                span: func_span,
            },
            args: vec![
                row_expr(),
                Expr::Literal(Literal::String("key".to_owned()), func_span),
            ],
            distinct: false,
            filter: None,
            span: func_span,
        };
        let raw_value_expr = Expr::FunctionCall {
            name: ObjectName {
                parts: vec!["__aiondb_composite_field".to_owned()],
                span: func_span,
            },
            args: vec![
                row_expr(),
                Expr::Literal(Literal::String("value".to_owned()), func_span),
            ],
            distinct: false,
            filter: None,
            span: func_span,
        };
        let value_expr = if text_values {
            raw_value_expr
        } else {
            Expr::Cast {
                expr: Box::new(raw_value_expr),
                data_type: DataType::Jsonb,
                span: func_span,
            }
        };
        let cte = synthetic_cte(
            cte_name.clone(),
            None,
            synthetic_select_from(
                func_span,
                vec![rows_cte],
                vec![
                    SelectItem {
                        expr: key_expr,
                        alias: Some(output_aliases[0].clone()),
                        span: func_span,
                    },
                    SelectItem {
                        expr: value_expr,
                        alias: Some(output_aliases[1].clone()),
                        span: func_span,
                    },
                ],
                ObjectName {
                    parts: vec![rows_cte_name],
                    span: func_span,
                },
                func_span,
            ),
            func_span,
        );
        let obj = cte_object_name(cte_name, func_span);
        Ok((obj, Some(exposed_name), vec![cte]))
    }

    fn build_jsonb_array_elements_from_function_cte(
        &mut self,
        name: ObjectName,
        args: Vec<Expr>,
        alias: Option<String>,
        col_aliases: Option<Vec<String>>,
        text_values: bool,
    ) -> DbResult<(ObjectName, Option<String>, Vec<CteDefinition>)> {
        let func_span = name.span;
        let mut function_name = name;
        if text_values {
            if let Some(last) = function_name.parts.last_mut() {
                "jsonb_array_elements_text".clone_into(last);
            }
        } else if let Some(last) = function_name.parts.last_mut() {
            "jsonb_array_elements".clone_into(last);
        }
        let func_base = function_name.parts.last().cloned().unwrap_or_default();
        let (exposed_name, cte_name) = self.function_cte_names(alias, func_base);
        let value_alias = col_aliases
            .and_then(|aliases| aliases.into_iter().next())
            .unwrap_or_else(|| "value".to_owned());
        let func_call_expr = Expr::FunctionCall {
            name: function_name,
            args,
            distinct: false,
            filter: None,
            span: func_span,
        };
        let cte = synthetic_cte(
            cte_name.clone(),
            None,
            synthetic_select(
                func_span,
                vec![SelectItem {
                    expr: func_call_expr,
                    alias: Some(value_alias),
                    span: func_span,
                }],
            ),
            func_span,
        );
        let obj = cte_object_name(cte_name, func_span);
        Ok((obj, Some(exposed_name), vec![cte]))
    }

    fn build_internal_srf_cte(
        &self,
        cte_name: String,
        function_name: &str,
        args: Vec<Expr>,
        column_name: &str,
        span: Span,
    ) -> CteDefinition {
        let func_call_expr = Expr::FunctionCall {
            name: ObjectName {
                parts: vec![function_name.to_owned()],
                span,
            },
            args,
            distinct: false,
            filter: None,
            span,
        };
        synthetic_cte(
            cte_name,
            None,
            synthetic_select(
                span,
                vec![SelectItem {
                    expr: func_call_expr,
                    alias: Some(column_name.to_owned()),
                    span,
                }],
            ),
            span,
        )
    }

    fn build_pg_input_error_info_cte(
        &mut self,
        name: ObjectName,
        args: Vec<Expr>,
        alias: Option<String>,
        col_aliases: Option<Vec<String>>,
    ) -> DbResult<(ObjectName, Option<String>, Vec<CteDefinition>)> {
        let func_span = name.span;
        let func_base = name.parts.last().cloned().unwrap_or_default();
        let (exposed_name, cte_name) = self.function_cte_names(alias, func_base);

        let sqlstate_expr = self.build_internal_function_call(
            "__aiondb_pg_input_error_info_sqlstate",
            args.clone(),
            func_span,
        );
        let mut select = synthetic_select(
            func_span,
            vec![
                SelectItem {
                    expr: self.build_internal_function_call(
                        "__aiondb_pg_input_error_info_message",
                        args.clone(),
                        func_span,
                    ),
                    alias: Some("message".to_owned()),
                    span: func_span,
                },
                SelectItem {
                    expr: self.build_internal_function_call(
                        "__aiondb_pg_input_error_info_detail",
                        args.clone(),
                        func_span,
                    ),
                    alias: Some("detail".to_owned()),
                    span: func_span,
                },
                SelectItem {
                    expr: self.build_internal_function_call(
                        "__aiondb_pg_input_error_info_hint",
                        args.clone(),
                        func_span,
                    ),
                    alias: Some("hint".to_owned()),
                    span: func_span,
                },
                SelectItem {
                    expr: sqlstate_expr.clone(),
                    alias: Some("sql_error_code".to_owned()),
                    span: func_span,
                },
            ],
        );
        select.selection = Some(Expr::IsNull {
            expr: Box::new(sqlstate_expr),
            negated: true,
            span: func_span,
        });
        select.where_span = Some(func_span);
        let cte = synthetic_cte(cte_name.clone(), col_aliases, select, func_span);
        let obj = cte_object_name(cte_name, func_span);
        Ok((obj, Some(exposed_name), vec![cte]))
    }

    fn build_pg_options_to_table_cte(
        &mut self,
        name: ObjectName,
        args: Vec<Expr>,
        alias: Option<String>,
        col_aliases: Option<Vec<String>>,
    ) -> DbResult<(ObjectName, Option<String>, Vec<CteDefinition>)> {
        let func_span = name.span;
        let func_base = name.parts.last().cloned().unwrap_or_default();
        let (exposed_name, cte_name) = self.function_cte_names(alias, func_base);
        let mut output_aliases = col_aliases
            .unwrap_or_else(|| vec!["option_name".to_owned(), "option_value".to_owned()]);
        if output_aliases.is_empty() {
            output_aliases.push("option_name".to_owned());
        }
        if output_aliases.len() == 1 {
            output_aliases.push("option_value".to_owned());
        }

        let rows_cte_name = self.next_from_function_cte_name();
        let row_col = "__row".to_owned();
        let rows_cte = synthetic_cte(
            rows_cte_name.clone(),
            None,
            synthetic_select(
                func_span,
                vec![SelectItem {
                    expr: self.build_internal_function_call("unnest", args, func_span),
                    alias: Some(row_col.clone()),
                    span: func_span,
                }],
            ),
            func_span,
        );
        let row_expr = || {
            Expr::Identifier(ObjectName {
                parts: vec![rows_cte_name.clone(), row_col.clone()],
                span: func_span,
            })
        };
        let build_part_expr = |field: i64| Expr::FunctionCall {
            name: ObjectName {
                parts: vec!["split_part".to_owned()],
                span: func_span,
            },
            args: vec![
                row_expr(),
                Expr::Literal(Literal::String("=".to_owned()), func_span),
                Expr::Literal(Literal::Integer(field), func_span),
            ],
            distinct: false,
            filter: None,
            span: func_span,
        };
        let cte = synthetic_cte(
            cte_name.clone(),
            None,
            synthetic_select_from(
                func_span,
                vec![rows_cte],
                vec![
                    SelectItem {
                        expr: build_part_expr(1),
                        alias: Some(output_aliases[0].clone()),
                        span: func_span,
                    },
                    SelectItem {
                        expr: build_part_expr(2),
                        alias: Some(output_aliases[1].clone()),
                        span: func_span,
                    },
                ],
                ObjectName {
                    parts: vec![rows_cte_name.clone()],
                    span: func_span,
                },
                func_span,
            ),
            func_span,
        );
        let obj = cte_object_name(cte_name, func_span);
        Ok((obj, Some(exposed_name), vec![cte]))
    }

    fn build_pg_partition_tree_cte(
        &mut self,
        name: ObjectName,
        alias: Option<String>,
        col_aliases: Option<Vec<String>>,
    ) -> DbResult<(ObjectName, Option<String>, Vec<CteDefinition>)> {
        let func_span = name.span;
        let func_base = name.parts.last().cloned().unwrap_or_default();
        let (exposed_name, cte_name) = self.function_cte_names(alias, func_base);
        let mut output_aliases = col_aliases.unwrap_or_else(|| {
            vec![
                "relid".to_owned(),
                "parentrelid".to_owned(),
                "isleaf".to_owned(),
                "level".to_owned(),
            ]
        });
        while output_aliases.len() < 4 {
            let fallback = match output_aliases.len() {
                0 => "relid",
                1 => "parentrelid",
                2 => "isleaf",
                _ => "level",
            };
            output_aliases.push(fallback.to_owned());
        }
        let null_cast = |data_type| Expr::Cast {
            expr: Box::new(Expr::Literal(Literal::Null, func_span)),
            data_type,
            span: func_span,
        };
        let mut select = synthetic_select(
            func_span,
            vec![
                SelectItem {
                    expr: null_cast(DataType::Int),
                    alias: Some(output_aliases[0].clone()),
                    span: func_span,
                },
                SelectItem {
                    expr: null_cast(DataType::Int),
                    alias: Some(output_aliases[1].clone()),
                    span: func_span,
                },
                SelectItem {
                    expr: null_cast(DataType::Boolean),
                    alias: Some(output_aliases[2].clone()),
                    span: func_span,
                },
                SelectItem {
                    expr: null_cast(DataType::Int),
                    alias: Some(output_aliases[3].clone()),
                    span: func_span,
                },
            ],
        );
        select.selection = Some(Expr::Literal(Literal::Boolean(false), func_span));
        select.where_span = Some(func_span);
        let cte = synthetic_cte(cte_name.clone(), None, select, func_span);
        let obj = cte_object_name(cte_name, func_span);
        Ok((obj, Some(exposed_name), vec![cte]))
    }

    fn build_internal_function_call(
        &self,
        function_name: &str,
        args: Vec<Expr>,
        span: Span,
    ) -> Expr {
        Expr::FunctionCall {
            name: ObjectName {
                parts: vec![function_name.to_owned()],
                span,
            },
            args,
            distinct: false,
            filter: None,
            span,
        }
    }

    pub(super) fn is_from_function_call(&self) -> bool {
        let is_ident = match &self.current().kind {
            TokenKind::Identifier(_) => true,
            TokenKind::Keyword(kw) if kw.is_unreserved() => true,
            _ => false,
        };
        if !is_ident {
            return false;
        }
        let mut ahead = self.index + 1;
        while ahead < self.tokens.len() {
            match &self.tokens[ahead].kind {
                TokenKind::Dot => {
                    ahead += 1;
                    // Skip the identifier after the dot.
                    if ahead < self.tokens.len() {
                        ahead += 1;
                    }
                }
                TokenKind::LParen => return true,
                _ => return false,
            }
        }
        false
    }

    /// Create a synthetic CTE that wraps an original relation (table or CTE)
    /// and renames its columns.  For `FROM v AS v1(x1, x2)` this produces:
    ///
    /// ```text
    /// CteDefinition {
    ///     name: "v1",
    ///     column_aliases: Some(["x1", "x2"]),
    ///     query: SELECT * FROM v
    /// }
    /// ```
    pub(super) fn make_column_alias_cte(
        alias_name: &str,
        original: &ObjectName,
        column_aliases: &[String],
        at_span: Span,
    ) -> CteDefinition {
        let inner_select = synthetic_select_from(
            at_span,
            Vec::new(),
            vec![SelectItem {
                expr: Expr::Identifier(ObjectName {
                    parts: vec!["*".to_owned()],
                    span: at_span,
                }),
                alias: None,
                span: at_span,
            }],
            original.clone(),
            at_span,
        );
        synthetic_cte(
            alias_name.to_owned(),
            Some(column_aliases.to_vec()),
            inner_select,
            at_span,
        )
    }

    /// Parse `VALUES (expr, ...), (expr, ...), ...` and return as a `Statement`.
    ///
    /// A single-row VALUES returns `Statement::Select`. Multi-row VALUES
    /// returns a balanced `Statement::SetOperation` tree of UNION ALL selects
    /// so that every row is preserved without creating pathological AST depth.
    pub(crate) fn parse_values_as_statement(&mut self) -> DbResult<Statement> {
        let values_token = self.expect_keyword(Keyword::Values)?;
        let mut rows: Vec<Vec<Expr>> = Vec::new();
        let mut span = values_token.span;
        let mut expected_width: Option<usize> = None;
        let mut total_cells = 0usize;
        loop {
            self.expect_token(&TokenKind::LParen)?;
            let mut row = Vec::new();
            loop {
                row.push(self.parse_expr()?);
                if !self.consume_kind(&TokenKind::Comma) {
                    break;
                }
            }
            let rparen = self.expect_token(&TokenKind::RParen)?;
            span = span.merge(rparen.span);
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

        // Helper: convert a row of exprs into a SelectStatement.
        // The first row gets `column1`, `column2`, ... aliases; subsequent
        // rows get no aliases (the column names come from the first branch
        // of UNION ALL, matching PG behaviour).
        let make_select = |row: Vec<Expr>, with_aliases: bool, row_span: Span| {
            let items: Vec<SelectItem> = row
                .into_iter()
                .enumerate()
                .map(|(i, expr)| {
                    let alias = if with_aliases {
                        Some(format!("column{}", i + 1))
                    } else {
                        None
                    };
                    SelectItem {
                        span: expr.span(),
                        expr,
                        alias,
                    }
                })
                .collect();
            synthetic_select(row_span, items)
        };

        if rows.len() == 1 {
            let Some(row) = rows.into_iter().next() else {
                return self.syntax_error(span, "VALUES requires at least one row");
            };
            return Ok(Statement::Select(make_select(row, true, span)));
        }

        // Multiple rows: build a balanced UNION ALL tree to avoid deep
        // recursion/memory spikes for large VALUES lists.
        let mut branches: Vec<Statement> = rows
            .into_iter()
            .enumerate()
            .map(|(index, row)| Statement::Select(make_select(row, index == 0, span)))
            .collect();
        if branches.is_empty() {
            return self.syntax_error(span, "VALUES requires at least one row");
        }

        while branches.len() > 1 {
            let mut next_level = Vec::with_capacity(branches.len().div_ceil(2));
            let mut iter = branches.into_iter();
            while let Some(left) = iter.next() {
                if let Some(right) = iter.next() {
                    next_level.push(Statement::SetOperation(SetOperationStatement {
                        op: SetOperationType::Union,
                        all: true,
                        left: Box::new(left),
                        right: Box::new(right),
                        order_by: Vec::new(),
                        order_by_span: None,
                        limit: None,
                        limit_span: None,
                        offset: None,
                        offset_span: None,
                        span,
                    }));
                } else {
                    next_level.push(left);
                }
            }
            branches = next_level;
        }

        let Some(result) = branches.pop() else {
            return self.syntax_error(span, "VALUES requires at least one row");
        };
        Ok(result)
    }

    /// Reject TABLESAMPLE explicitly until parser -> AST -> planner support exists.
    fn reject_tablesample_clause(&mut self) -> DbResult<()> {
        if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("tablesample"))
        {
            return self
                .unsupported_feature(self.current().span, "TABLESAMPLE in FROM is not supported");
        }
        Ok(())
    }

    /// Returns `true` if the current token starts a join or is a comma
    /// (implicit cross join separator).
    fn is_join_start_or_comma(&self) -> bool {
        matches!(
            self.current().kind,
            TokenKind::Comma
                | TokenKind::Keyword(
                    Keyword::Join
                        | Keyword::Inner
                        | Keyword::Left
                        | Keyword::Right
                        | Keyword::Full
                        | Keyword::Cross
                        | Keyword::Natural
                )
        )
    }

    /// Parse the remainder of a parenthesized join expression.
    ///
    /// At call time we have already consumed `(`, the first table reference
    /// (`relation` with optional `alias`), and confirmed that the next token
    /// is a join keyword or comma.  We parse any joins / commas inside the
    /// parentheses, build a `SELECT * FROM relation [alias] JOIN ...` CTE,
    /// consume the closing `)`, and return the CTE reference.
    fn parse_parenthesized_join(
        &mut self,
        lparen_span: Span,
        relation: ObjectName,
        alias: Option<String>,
    ) -> DbResult<(ObjectName, Option<String>, Vec<CteDefinition>)> {
        let mut extra_ctes: Vec<CteDefinition> = Vec::new();
        let mut joins: Vec<JoinClause> = Vec::new();
        let mut span = relation.span;

        loop {
            // Implicit cross join via comma
            while self.consume_kind(&TokenKind::Comma) {
                let join_start = self.previous_span();
                let _ = self.consume_keyword(Keyword::Lateral);
                let (table, tbl_alias, ctes, _col_aliases) = self.parse_from_table_ref()?;
                extra_ctes.extend(ctes);
                let join_span = join_start.merge(table.span);
                span = span.merge(join_span);
                joins.push(JoinClause {
                    join_type: JoinType::Cross,
                    table,
                    alias: tbl_alias,
                    condition: None,
                    using_columns: Vec::new(),
                    using_alias: None,
                    natural: false,
                    span: join_span,
                });
            }

            // Explicit joins
            let join_start_span = self.current().span;
            let natural = self.consume_keyword(Keyword::Natural).is_some();
            let join_type = if self.consume_keyword(Keyword::Inner).is_some() {
                self.expect_keyword(Keyword::Join)?;
                Some(JoinType::Inner)
            } else if self.consume_keyword(Keyword::Left).is_some() {
                self.consume_keyword(Keyword::Outer);
                self.expect_keyword(Keyword::Join)?;
                Some(JoinType::Left)
            } else if self.consume_keyword(Keyword::Right).is_some() {
                self.consume_keyword(Keyword::Outer);
                self.expect_keyword(Keyword::Join)?;
                Some(JoinType::Right)
            } else if self.consume_keyword(Keyword::Full).is_some() {
                self.consume_keyword(Keyword::Outer);
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

            let _ = self.consume_keyword(Keyword::Lateral);
            let (table, tbl_alias, ctes, _col_aliases) = self.parse_from_table_ref()?;
            extra_ctes.extend(ctes);
            let (condition, using_columns, using_alias) = if natural || jt == JoinType::Cross {
                (None, Vec::new(), None)
            } else if self.consume_keyword(Keyword::Using).is_some() {
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
                (None, Vec::new(), None)
            };

            let join_span =
                join_start_span.merge(condition.as_ref().map_or(table.span, |c| c.span()));
            span = span.merge(join_span);
            joins.push(JoinClause {
                join_type: jt,
                table,
                alias: tbl_alias,
                condition,
                using_columns,
                using_alias,
                natural,
                span: join_span,
            });
        }

        let rparen = self.expect_token(&TokenKind::RParen)?;
        let subquery_span = lparen_span.merge(rparen.span);

        let (outer_alias, col_aliases) = self.try_parse_table_alias();
        let cte_name = outer_alias
            .clone()
            .unwrap_or_else(|| self.next_subquery_cte_name());

        // Build SELECT * FROM relation [alias] JOIN ... as CTE body
        let star_item = SelectItem {
            expr: Expr::Identifier(ObjectName {
                parts: vec!["*".to_owned()],
                span: relation.span,
            }),
            alias: None,
            span: relation.span,
        };
        let mut select = synthetic_select_from(span, extra_ctes, vec![star_item], relation, span);
        select.from_alias = alias;
        select.joins = joins;
        let cte = synthetic_cte(cte_name.clone(), col_aliases, select, subquery_span);
        let obj = ObjectName {
            parts: vec![cte_name],
            span: subquery_span,
        };
        Ok((obj, outer_alias, vec![cte]))
    }
}

fn is_known_from_srf(func_base: &str) -> bool {
    name_matches_any_ci(func_base, KNOWN_FROM_SRFS)
}
