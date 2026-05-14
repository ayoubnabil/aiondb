//! AST-level rewrites for INSERT/UPDATE assignments that target nested array
//! or composite slots (e.g. `SET col[1] = ...`).
//!
//! The rewrites lower these into calls to the runtime helpers
//! `__aiondb_array_assign` and `__aiondb_composite_assign`. The argument
//! layout consumed by `__aiondb_array_assign` is:
//!
//! ```text
//! base, ("index", index, NULL | "slice", lower_or_null, upper_or_null)*, value
//! ```
//!
//! The marker literal in each triplet is what the runtime uses to dispatch
//! between element and slice updates; keeping the encoding here in lockstep
//! with the executor is a hard cross-module contract.

use super::parser_dml::{AssignmentSubscript, AssignmentTarget};
use super::*;

impl Parser {
    pub(crate) fn rewrite_array_assignment_expr(
        &self,
        column: &str,
        subscripts: Vec<AssignmentSubscript>,
        value: Expr,
        span: crate::span::Span,
    ) -> Expr {
        self.build_array_assignment_expr(
            Expr::Identifier(ObjectName {
                parts: vec![column.to_owned()],
                span,
            }),
            subscripts,
            value,
            span,
        )
    }

    pub(crate) fn build_array_assignment_expr(
        &self,
        base: Expr,
        subscripts: Vec<AssignmentSubscript>,
        value: Expr,
        span: crate::span::Span,
    ) -> Expr {
        let mut args = Vec::with_capacity(2 + subscripts.len() * 3);
        args.push(base);
        for subscript in subscripts {
            match subscript {
                AssignmentSubscript::Index(index) => {
                    let index_span = index.span();
                    args.push(Expr::Literal(
                        crate::ast::Literal::String("index".to_owned()),
                        index_span,
                    ));
                    args.push(index);
                    args.push(Expr::Literal(crate::ast::Literal::Null, index_span));
                }
                AssignmentSubscript::Slice { lower, upper } => {
                    let lower_span = lower
                        .as_ref()
                        .map(Expr::span)
                        .or_else(|| upper.as_ref().map(Expr::span))
                        .unwrap_or(span);
                    args.push(Expr::Literal(
                        crate::ast::Literal::String("slice".to_owned()),
                        lower_span,
                    ));
                    args.push(
                        lower.unwrap_or(Expr::Literal(crate::ast::Literal::Null, lower_span)),
                    );
                    args.push(
                        upper.unwrap_or(Expr::Literal(crate::ast::Literal::Null, lower_span)),
                    );
                }
            }
        }
        args.push(value);

        Expr::FunctionCall {
            name: ObjectName {
                parts: vec!["__aiondb_array_assign".to_owned()],
                span,
            },
            args,
            distinct: false,
            filter: None,
            span,
        }
    }

    pub(crate) fn rewrite_composite_array_assignment_expr(
        &self,
        column: &str,
        subscripts: Vec<AssignmentSubscript>,
        field: &str,
        value: Expr,
        span: crate::span::Span,
    ) -> Expr {
        self.build_composite_array_assignment_expr(
            Expr::Identifier(ObjectName {
                parts: vec![column.to_owned()],
                span,
            }),
            subscripts,
            field.to_owned(),
            value,
            span,
        )
    }

    pub(crate) fn build_composite_array_assignment_expr(
        &self,
        base: Expr,
        subscripts: Vec<AssignmentSubscript>,
        field: String,
        value: Expr,
        span: crate::span::Span,
    ) -> Expr {
        let replacement = self.build_composite_assignment_value_expr(
            base.clone(),
            &subscripts,
            &field,
            value,
            span,
        );
        self.build_array_assignment_expr(base, subscripts, replacement, span)
    }

    pub(crate) fn build_composite_assignment_value_expr(
        &self,
        base: Expr,
        subscripts: &[AssignmentSubscript],
        field: &str,
        value: Expr,
        span: crate::span::Span,
    ) -> Expr {
        let current_value = self.build_array_element_expr(base, subscripts);
        Expr::FunctionCall {
            name: ObjectName {
                parts: vec!["__aiondb_composite_assign".to_owned()],
                span,
            },
            args: vec![
                current_value,
                Expr::Literal(crate::ast::Literal::String(field.to_owned()), span),
                value,
            ],
            distinct: false,
            filter: None,
            span,
        }
    }

    pub(crate) fn build_array_element_expr(
        &self,
        mut base: Expr,
        subscripts: &[AssignmentSubscript],
    ) -> Expr {
        for subscript in subscripts {
            let AssignmentSubscript::Index(index) = subscript else {
                return base;
            };
            let index = index.clone();
            let span = base.span().merge(index.span());
            base = Expr::FunctionCall {
                name: ObjectName {
                    parts: vec!["array_get".to_owned()],
                    span,
                },
                args: vec![base, index],
                distinct: false,
                filter: None,
                span,
            };
        }
        base
    }

    pub(crate) fn insert_targets_to_columns(
        &self,
        targets: &[AssignmentTarget],
    ) -> Vec<ObjectName> {
        targets
            .iter()
            .map(|target| match target {
                AssignmentTarget::Plain { column, span, .. }
                | AssignmentTarget::ArraySubscript { column, span, .. } => ObjectName {
                    parts: vec![column.clone()],
                    span: *span,
                },
            })
            .collect()
    }

    pub(crate) fn rewrite_insert_values_targets(
        &self,
        targets: &[AssignmentTarget],
        rows: Vec<Vec<Expr>>,
    ) -> (Vec<ObjectName>, Vec<Vec<Expr>>) {
        if targets.is_empty()
            || !targets
                .iter()
                .any(|target| matches!(target, AssignmentTarget::ArraySubscript { .. }))
            || rows.iter().any(|row| row.len() != targets.len())
        {
            return (self.insert_targets_to_columns(targets), rows);
        }

        let mut columns = Vec::new();
        let mut column_ordinals = std::collections::BTreeMap::new();
        for target in targets {
            let (column, span) = match target {
                AssignmentTarget::Plain { column, span, .. }
                | AssignmentTarget::ArraySubscript { column, span, .. } => (column, span),
            };
            if !column_ordinals.contains_key(column) {
                let ordinal = columns.len();
                column_ordinals.insert(column.clone(), ordinal);
                columns.push(ObjectName {
                    parts: vec![column.clone()],
                    span: *span,
                });
            }
        }

        let rewritten_rows = rows
            .into_iter()
            .map(|row| {
                let mut values = vec![None; columns.len()];
                for (target, value) in targets.iter().zip(row) {
                    let (column, target_span) = match target {
                        AssignmentTarget::Plain { column, span, .. }
                        | AssignmentTarget::ArraySubscript { column, span, .. } => (column, span),
                    };
                    let ordinal = column_ordinals[column];
                    match target {
                        AssignmentTarget::Plain { .. } => {
                            values[ordinal] = Some(value);
                        }
                        AssignmentTarget::ArraySubscript { .. } => {
                            let base = values[ordinal].take().unwrap_or_else(|| {
                                Expr::Literal(crate::ast::Literal::Null, value.span())
                            });
                            let expr = match target {
                                AssignmentTarget::ArraySubscript {
                                    subscripts,
                                    composite_field,
                                    ..
                                } => {
                                    if let Some(field) = composite_field {
                                        self.build_composite_array_assignment_expr(
                                            base,
                                            subscripts.clone(),
                                            field.clone(),
                                            value,
                                            target_span.merge(columns[ordinal].span),
                                        )
                                    } else {
                                        self.build_array_assignment_expr(
                                            base,
                                            subscripts.clone(),
                                            value,
                                            target_span.merge(columns[ordinal].span),
                                        )
                                    }
                                }
                                AssignmentTarget::Plain { .. } => base,
                            };
                            values[ordinal] = Some(expr);
                        }
                    }
                }

                values
                    .into_iter()
                    .map(|expr| {
                        expr.unwrap_or(Expr::Default {
                            span: crate::span::Span::new(0, 0),
                        })
                    })
                    .collect()
            })
            .collect();

        (columns, rewritten_rows)
    }

    /// Rewrite INSERT ... SELECT when the column list contains array subscript
    /// targets (e.g. `INSERT INTO t (f2[1], f2[2]) SELECT 7, 8`).
    ///
    /// The inner SELECT is wrapped in a CTE, and a new outer SELECT is built
    /// that folds the individual column values into `__aiondb_array_assign`
    /// expressions, mirroring what `rewrite_insert_values_targets` does for
    /// the VALUES path.
    pub(crate) fn rewrite_insert_select_targets(
        &self,
        targets: &[AssignmentTarget],
        original_columns: Vec<ObjectName>,
        query: SelectStatement,
    ) -> (Vec<ObjectName>, SelectStatement) {
        // No rewriting needed if there are no array subscript targets.
        if targets.is_empty()
            || !targets
                .iter()
                .any(|t| matches!(t, AssignmentTarget::ArraySubscript { .. }))
        {
            return (original_columns, query);
        }

        let span = query.span;

        // Generate synthetic column names for each target in the inner SELECT.
        let synthetic_cols: Vec<String> = (0..targets.len())
            .map(|i| format!("__ins_col_{i}"))
            .collect();

        // Build deduplicated column list and fold array assignments.
        let mut column_ordinals = std::collections::BTreeMap::<String, usize>::new();
        let mut deduped_columns = Vec::new();
        let mut select_exprs: Vec<Option<Expr>> = Vec::new();

        for (i, target) in targets.iter().enumerate() {
            let (column, target_span) = match target {
                AssignmentTarget::Plain { column, span, .. }
                | AssignmentTarget::ArraySubscript { column, span, .. } => (column, span),
            };
            let col_key = column.to_ascii_lowercase();
            let ref_expr = Expr::Identifier(ObjectName {
                parts: vec![synthetic_cols[i].clone()],
                span: *target_span,
            });

            if let Some(&ordinal) = column_ordinals.get(&col_key) {
                // Accumulate into existing array assignment expression.
                match target {
                    AssignmentTarget::ArraySubscript {
                        subscripts,
                        composite_field,
                        ..
                    } => {
                        let base = select_exprs[ordinal]
                            .take()
                            .unwrap_or(Expr::Literal(crate::ast::Literal::Null, *target_span));
                        let expr = if let Some(field) = composite_field {
                            self.build_composite_array_assignment_expr(
                                base,
                                subscripts.clone(),
                                field.clone(),
                                ref_expr,
                                *target_span,
                            )
                        } else {
                            self.build_array_assignment_expr(
                                base,
                                subscripts.clone(),
                                ref_expr,
                                *target_span,
                            )
                        };
                        select_exprs[ordinal] = Some(expr);
                    }
                    AssignmentTarget::Plain { .. } => {
                        // Later plain target for the same column overwrites.
                        select_exprs[ordinal] = Some(ref_expr);
                    }
                }
            } else {
                let ordinal = deduped_columns.len();
                column_ordinals.insert(col_key, ordinal);
                deduped_columns.push(ObjectName {
                    parts: vec![column.clone()],
                    span: *target_span,
                });
                let expr = match target {
                    AssignmentTarget::ArraySubscript {
                        subscripts,
                        composite_field,
                        column,
                        ..
                    } => {
                        let base_null = Expr::Literal(crate::ast::Literal::Null, *target_span);
                        if let Some(field) = composite_field {
                            self.rewrite_composite_array_assignment_expr(
                                column,
                                subscripts.clone(),
                                field,
                                ref_expr,
                                *target_span,
                            )
                        } else {
                            self.build_array_assignment_expr(
                                base_null,
                                subscripts.clone(),
                                ref_expr,
                                *target_span,
                            )
                        }
                    }
                    AssignmentTarget::Plain { .. } => ref_expr,
                };
                select_exprs.push(Some(expr));
            }
        }

        // Build the outer SELECT items from the folded expressions.
        let outer_items: Vec<SelectItem> = select_exprs
            .into_iter()
            .map(|expr| {
                let expr = expr.unwrap_or(Expr::Default { span });
                SelectItem {
                    span: expr.span(),
                    expr,
                    alias: None,
                }
            })
            .collect();

        // Wrap the original query as a CTE and build the outer SELECT.
        let cte_name = "__insert_array_rewrite".to_owned();
        let from_name = ObjectName {
            parts: vec![cte_name.clone()],
            span,
        };
        let cte = CteDefinition {
            name: cte_name,
            column_aliases: Some(synthetic_cols),
            recursive: false,
            query: Box::new(Statement::Select(query)),
            recursive_term: None,
            union_all: false,
            span,
        };

        let outer_query = SelectStatement {
            row_lock: None,
            ctes: vec![cte],
            distinct: DistinctKind::All,
            items: outer_items,
            from: Some(from_name),
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
        };

        (deduped_columns, outer_query)
    }

    pub(crate) fn ensure_non_null_assignment_subscript(&self, expr: &Expr) -> DbResult<()> {
        if matches!(expr, Expr::Literal(crate::ast::Literal::Null, _)) {
            return Err(DbError::syntax_error(
                "array subscript in assignment must not be null",
            ));
        }
        Ok(())
    }

    pub(crate) fn can_rewrite_assignment_subscripts(
        &self,
        subscripts: &[AssignmentSubscript],
    ) -> bool {
        for subscript in subscripts {
            match subscript {
                AssignmentSubscript::Index(_) => {}
                AssignmentSubscript::Slice { lower, upper } => {
                    if lower
                        .as_ref()
                        .is_some_and(|expr| !self.is_integer_literal(expr))
                    {
                        return false;
                    }
                    if upper
                        .as_ref()
                        .is_some_and(|expr| !self.is_integer_literal(expr))
                    {
                        return false;
                    }
                }
            }
        }
        true
    }

    pub(crate) fn can_rewrite_composite_assignment_subscripts(
        &self,
        subscripts: &[AssignmentSubscript],
    ) -> bool {
        self.can_rewrite_assignment_subscripts(subscripts)
            && subscripts
                .iter()
                .all(|subscript| matches!(subscript, AssignmentSubscript::Index(_)))
    }

    pub(crate) fn is_integer_literal(&self, expr: &Expr) -> bool {
        super::parser_dml::integer_literal_value(expr).is_some()
    }
}
