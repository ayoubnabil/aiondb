use aiondb_catalog::TableDescriptor;
use aiondb_core::{DataType, DbError, DbResult, RelationId, Row, SqlState, Value};
use aiondb_parser::{BinaryOperator, Expr, Literal, UnaryOperator};
use aiondb_plan::{ScalarFunction, TypedExpr};

use super::*;

impl Executor {
    /// Enforce CHECK constraints for INSERT operations.
    ///
    /// For each CHECK constraint on the table, parse the constraint expression,
    /// build a `TypedExpr`, evaluate it with the row values, and return an error
    /// if the result is FALSE.
    pub(super) fn enforce_check_on_insert(
        &self,
        table_id: RelationId,
        row_values: &[Value],
        context: &ExecutionContext,
    ) -> DbResult<()> {
        let table = self
            .catalog_reader
            .get_table_by_id(context.txn_id, table_id)?
            .ok_or_else(|| DbError::internal("table not found for CHECK constraint"))?;

        self.enforce_check_constraints(&table, row_values, context)
    }

    /// Enforce CHECK constraints for UPDATE operations.
    pub(super) fn enforce_check_on_update(
        &self,
        table_id: RelationId,
        new_values: &[Value],
        context: &ExecutionContext,
    ) -> DbResult<()> {
        self.enforce_check_on_insert(table_id, new_values, context)
    }

    /// Core CHECK constraint enforcement logic.
    ///
    /// For each CHECK constraint on the table:
    /// 1. Parse the SQL expression text
    /// 2. Build a `TypedExpr` by resolving column references against the table
    /// 3. Evaluate the expression with the given row values
    /// 4. If the result is FALSE, return a `CheckViolation` error
    ///
    /// Per SQL semantics, NULL results satisfy CHECK constraints (only FALSE
    /// violates them).
    fn enforce_check_constraints(
        &self,
        table: &TableDescriptor,
        row_values: &[Value],
        context: &ExecutionContext,
    ) -> DbResult<()> {
        if table.check_constraints.is_empty() {
            return Ok(());
        }

        let compiled = compile_check_constraints(table)?;
        self.enforce_compiled_check_constraints(&compiled, row_values, table, context)
    }

    /// Pre-compile every CHECK constraint of `table` into a `TypedExpr`,
    /// once per UPDATE/INSERT statement. Mirrors PostgreSQL's behaviour
    /// of holding the parsed `pg_constraint.conbin` (binary form) ready
    /// for execution rather than reparsing the SQL text per row. The
    /// per-row hot path then reuses the cached compiled form via
    /// `enforce_compiled_check_constraints`.
    pub(super) fn precompile_check_constraints(
        &self,
        table: &TableDescriptor,
    ) -> DbResult<Vec<CompiledCheckConstraint>> {
        compile_check_constraints(table)
    }

    /// Per-row CHECK enforcement using a pre-compiled list. The caller
    /// must have produced `compiled` via `precompile_check_constraints`
    /// and `table` must be the same descriptor it was compiled against.
    pub(super) fn enforce_compiled_check_constraints(
        &self,
        compiled: &[CompiledCheckConstraint],
        row_values: &[Value],
        table: &TableDescriptor,
        context: &ExecutionContext,
    ) -> DbResult<()> {
        if compiled.is_empty() {
            return Ok(());
        }

        let row = Row::new(row_values.to_vec());

        for check in compiled {
            let value = self.evaluate_expr_with_row(&check.typed, &row, context)?;

            match value {
                // SQL semantics: CHECK passes if TRUE or NULL; only FALSE is a violation.
                Value::Boolean(true) | Value::Null => {}
                Value::Boolean(false) => {
                    let detail = format!(
                        "Failing row contains ({}).",
                        row_values
                            .iter()
                            .map(|v| match v {
                                Value::Null => "null".to_owned(),
                                other => format!("{other}"),
                            })
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                    return Err(DbError::constraint_error(
                        SqlState::CheckViolation,
                        format!(
                            "new row for relation \"{}\" violates check constraint \"{}\"",
                            table.name.object_name(),
                            check.label,
                        ),
                    )
                    .with_client_detail(detail));
                }
                _ => {
                    return Err(DbError::internal(
                        "CHECK constraint expression did not evaluate to BOOLEAN",
                    ));
                }
            }
        }

        Ok(())
    }

    pub(super) fn validate_check_constraint_on_existing_rows(
        &self,
        table_id: RelationId,
        table: &TableDescriptor,
        check_expr: &str,
        constraint_name: Option<&str>,
        context: &ExecutionContext,
    ) -> DbResult<()> {
        let Ok(parsed) = aiondb_parser::parse_expression(check_expr) else {
            return Ok(());
        };
        let Ok(typed) = check_expr_to_typed(&parsed, table) else {
            return Ok(());
        };
        let mut stream = self.scan_table_locked(context, table_id, None)?;
        while let Some(record) = stream.next()? {
            let value = match self.evaluate_expr_with_row(&typed, &record.row, context) {
                Ok(value) => value,
                Err(error) if error.sqlstate() == SqlState::InternalError => return Ok(()),
                Err(error) => return Err(error),
            };
            if matches!(value, Value::Boolean(false)) {
                let label = constraint_name.unwrap_or(check_expr);
                return Err(DbError::constraint_error(
                    SqlState::CheckViolation,
                    format!(
                        "check constraint \"{label}\" of relation \"{}\" is violated by some row",
                        table.name.object_name()
                    ),
                ));
            }
        }
        Ok(())
    }
}

/// A single CHECK constraint in pre-compiled form, ready to be
/// evaluated against any row of the underlying relation. PostgreSQL
/// caches the same shape via `pg_constraint.conbin` plus the in-memory
/// `expression_planner` output; AionDB caches it for the duration of a
/// single UPDATE/INSERT statement so the per-row hot path skips the
/// SQL parse + type-check work.
#[derive(Clone)]
pub(crate) struct CompiledCheckConstraint {
    pub label: String,
    pub typed: TypedExpr,
}

/// Pre-compile every CHECK constraint on `table` once. The result is
/// reusable across all rows of a single UPDATE/INSERT statement.
fn compile_check_constraints(table: &TableDescriptor) -> DbResult<Vec<CompiledCheckConstraint>> {
    let mut compiled = Vec::with_capacity(table.check_constraints.len());
    for check in &table.check_constraints {
        let parsed = aiondb_parser::parse_expression(&check.expression).map_err(|_| {
            DbError::internal(format!(
                "failed to parse CHECK constraint expression: {}",
                check.expression
            ))
        })?;
        let typed = check_expr_to_typed(&parsed, table)?;
        let label = check
            .name
            .clone()
            .unwrap_or_else(|| check.expression.clone());
        compiled.push(CompiledCheckConstraint { label, typed });
    }
    Ok(compiled)
}

/// Convert a parser `Expr` into a `TypedExpr` for CHECK constraint evaluation.
///
/// This is a lightweight conversion that resolves column references against the
/// table descriptor and handles the expression types commonly found in CHECK
/// constraints (comparisons, logical operators, literals, IS NULL, etc.).
fn check_expr_to_typed(expr: &Expr, table: &TableDescriptor) -> DbResult<TypedExpr> {
    match expr {
        Expr::Identifier(name) => {
            let col_name = name.parts.last().map_or("", String::as_str);
            let (ordinal, column) = table
                .columns
                .iter()
                .enumerate()
                .find(|(_, c)| c.name.eq_ignore_ascii_case(col_name))
                .ok_or_else(|| {
                    DbError::internal(format!(
                        "CHECK constraint references unknown column: {col_name}"
                    ))
                })?;
            Ok(TypedExpr::column_ref(
                column.name.clone(),
                ordinal,
                column.data_type.clone(),
                column.nullable,
            ))
        }
        Expr::Literal(Literal::Integer(v), _) => {
            if let Ok(i) = i32::try_from(*v) {
                Ok(TypedExpr::literal(Value::Int(i), DataType::Int, false))
            } else {
                Ok(TypedExpr::literal(
                    Value::BigInt(*v),
                    DataType::BigInt,
                    false,
                ))
            }
        }
        Expr::Literal(Literal::String(v), _) => Ok(TypedExpr::literal(
            Value::Text(v.clone()),
            DataType::Text,
            false,
        )),
        Expr::Literal(Literal::Boolean(v), _) => Ok(TypedExpr::literal(
            Value::Boolean(*v),
            DataType::Boolean,
            false,
        )),
        Expr::Literal(Literal::Null, _) => Ok(TypedExpr::literal(Value::Null, DataType::Int, true)),
        Expr::BinaryOp {
            left, op, right, ..
        } => {
            let left_typed = check_expr_to_typed(left, table)?;
            let right_typed = check_expr_to_typed(right, table)?;
            match op {
                BinaryOperator::Gt => Ok(TypedExpr::binary_gt(left_typed, right_typed)),
                BinaryOperator::Ge => Ok(TypedExpr::binary_ge(left_typed, right_typed)),
                BinaryOperator::Lt => Ok(TypedExpr::binary_lt(left_typed, right_typed)),
                BinaryOperator::Le => Ok(TypedExpr::binary_le(left_typed, right_typed)),
                BinaryOperator::Eq => Ok(TypedExpr::binary_eq(left_typed, right_typed)),
                BinaryOperator::Ne => Ok(TypedExpr::binary_ne(left_typed, right_typed)),
                BinaryOperator::And => Ok(TypedExpr::logical_and(left_typed, right_typed)),
                BinaryOperator::Or => Ok(TypedExpr::logical_or(left_typed, right_typed)),
                BinaryOperator::Add => {
                    let dt = left_typed.data_type.clone();
                    let n = left_typed.nullable || right_typed.nullable;
                    Ok(TypedExpr::arith_add(left_typed, right_typed, dt, n))
                }
                BinaryOperator::Sub => {
                    let dt = left_typed.data_type.clone();
                    let n = left_typed.nullable || right_typed.nullable;
                    Ok(TypedExpr::arith_sub(left_typed, right_typed, dt, n))
                }
                BinaryOperator::Mul => {
                    let dt = left_typed.data_type.clone();
                    let n = left_typed.nullable || right_typed.nullable;
                    Ok(TypedExpr::arith_mul(left_typed, right_typed, dt, n))
                }
                BinaryOperator::Div => {
                    let dt = left_typed.data_type.clone();
                    let n = left_typed.nullable || right_typed.nullable;
                    Ok(TypedExpr::arith_div(left_typed, right_typed, dt, n))
                }
                BinaryOperator::Mod => {
                    let dt = left_typed.data_type.clone();
                    let n = left_typed.nullable || right_typed.nullable;
                    Ok(TypedExpr::arith_mod(left_typed, right_typed, dt, n))
                }
                BinaryOperator::Concat => Ok(TypedExpr::concat(left_typed, right_typed)),
                BinaryOperator::JsonGet => Ok(TypedExpr::json_get(left_typed, right_typed)),
                BinaryOperator::JsonGetText => {
                    Ok(TypedExpr::json_get_text(left_typed, right_typed))
                }
                BinaryOperator::JsonPathGet => {
                    Ok(TypedExpr::json_path_get(left_typed, right_typed))
                }
                BinaryOperator::JsonPathGetText => {
                    Ok(TypedExpr::json_path_get_text(left_typed, right_typed))
                }
                BinaryOperator::JsonContains => {
                    Ok(TypedExpr::json_contains(left_typed, right_typed))
                }
                BinaryOperator::JsonContainedBy => {
                    Ok(TypedExpr::json_contained_by(left_typed, right_typed))
                }
                BinaryOperator::JsonKeyExists => {
                    Ok(TypedExpr::json_key_exists(left_typed, right_typed))
                }
                BinaryOperator::JsonAnyKeyExists => {
                    Ok(TypedExpr::json_any_key_exists(left_typed, right_typed))
                }
                BinaryOperator::JsonAllKeysExist => {
                    Ok(TypedExpr::json_all_keys_exist(left_typed, right_typed))
                }
                BinaryOperator::ArrayOverlap => {
                    Ok(TypedExpr::array_overlap(left_typed, right_typed))
                }
                BinaryOperator::Exp
                | BinaryOperator::BitwiseAnd
                | BinaryOperator::BitwiseOr
                | BinaryOperator::BitwiseXor
                | BinaryOperator::ShiftLeft
                | BinaryOperator::ShiftRight
                | BinaryOperator::RegexMatch
                | BinaryOperator::RegexMatchInsensitive
                | BinaryOperator::NotRegexMatch
                | BinaryOperator::NotRegexMatchInsensitive
                | BinaryOperator::FullTextSearch
                | BinaryOperator::JsonPathExists
                | BinaryOperator::GeometricEq
                | BinaryOperator::VectorL2Distance
                | BinaryOperator::VectorCosineDistance
                | BinaryOperator::VectorNegativeInnerProduct
                | BinaryOperator::VectorL1Distance
                | BinaryOperator::VectorHammingDistance
                | BinaryOperator::VectorJaccardDistance => Err(DbError::internal(
                    "unsupported operator in CHECK constraint",
                )),
            }
        }
        Expr::UnaryOp {
            op: UnaryOperator::Not,
            expr: inner,
            ..
        } => {
            let inner_typed = check_expr_to_typed(inner, table)?;
            Ok(TypedExpr::logical_not(inner_typed))
        }
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr: inner,
            ..
        } => {
            let inner_typed = check_expr_to_typed(inner, table)?;
            let dt = inner_typed.data_type.clone();
            let n = inner_typed.nullable;
            Ok(TypedExpr::negate(inner_typed, dt, n))
        }
        Expr::UnaryOp {
            op:
                UnaryOperator::BitwiseNot
                | UnaryOperator::Abs
                | UnaryOperator::SquareRoot
                | UnaryOperator::CubeRoot,
            ..
        } => Err(DbError::internal(
            "unsupported operator in CHECK constraint",
        )),
        Expr::IsNull {
            expr: inner,
            negated,
            ..
        } => {
            let inner_typed = check_expr_to_typed(inner, table)?;
            Ok(TypedExpr::is_null(inner_typed, *negated))
        }
        Expr::IsDistinctFrom {
            left,
            right,
            negated,
            ..
        } => {
            let left_typed = check_expr_to_typed(left, table)?;
            let right_typed = check_expr_to_typed(right, table)?;
            Ok(TypedExpr::is_distinct_from(
                left_typed,
                right_typed,
                *negated,
            ))
        }
        Expr::InList {
            expr: inner,
            list,
            negated,
            ..
        } => {
            let inner_typed = check_expr_to_typed(inner, table)?;
            let list_typed = list
                .iter()
                .map(|e| check_expr_to_typed(e, table))
                .collect::<DbResult<Vec<_>>>()?;
            Ok(TypedExpr::in_list(inner_typed, list_typed, *negated))
        }
        Expr::Between {
            expr: inner,
            low,
            high,
            negated,
            ..
        } => {
            let inner_typed = check_expr_to_typed(inner, table)?;
            let low_typed = check_expr_to_typed(low, table)?;
            let high_typed = check_expr_to_typed(high, table)?;
            Ok(TypedExpr::between(
                inner_typed,
                low_typed,
                high_typed,
                *negated,
            ))
        }
        Expr::FunctionCall {
            name,
            args,
            distinct,
            filter,
            ..
        } => {
            if *distinct || filter.is_some() {
                return Err(DbError::internal(
                    "unsupported function form in CHECK constraint",
                ));
            }
            let function_name = name.parts.last().map_or("", String::as_str);
            if function_name.eq_ignore_ascii_case("cardinality") && args.len() == 1 {
                let arg = check_expr_to_typed(&args[0], table)?;
                return Ok(TypedExpr::scalar_function(
                    ScalarFunction::Cardinality,
                    vec![arg],
                    DataType::Int,
                    true,
                ));
            }
            if function_name.eq_ignore_ascii_case("__aiondb_compat_cast") && args.len() == 3 {
                let typed_args = args
                    .iter()
                    .map(|arg| check_expr_to_typed(arg, table))
                    .collect::<DbResult<Vec<_>>>()?;
                return Ok(TypedExpr::scalar_function(
                    ScalarFunction::Generic("__aiondb_compat_cast".to_owned()),
                    typed_args,
                    DataType::Text,
                    true,
                ));
            }
            let scalar_function = match function_name.to_ascii_lowercase().as_str() {
                "upper" => Some((ScalarFunction::Upper, DataType::Text)),
                "lower" => Some((ScalarFunction::Lower, DataType::Text)),
                "length" => Some((ScalarFunction::Length, DataType::Int)),
                "char_length" | "character_length" => {
                    Some((ScalarFunction::CharLength, DataType::Int))
                }
                "octet_length" => Some((ScalarFunction::OctetLength, DataType::Int)),
                "substring" | "substr" => Some((ScalarFunction::Substring, DataType::Text)),
                "trim" => Some((ScalarFunction::Trim, DataType::Text)),
                "ltrim" => Some((ScalarFunction::Ltrim, DataType::Text)),
                "rtrim" => Some((ScalarFunction::Rtrim, DataType::Text)),
                "replace" => Some((ScalarFunction::Replace, DataType::Text)),
                "strpos" => Some((ScalarFunction::Strpos, DataType::Int)),
                "left" => Some((ScalarFunction::Left, DataType::Text)),
                "right" => Some((ScalarFunction::Right, DataType::Text)),
                "repeat" => Some((ScalarFunction::Repeat, DataType::Text)),
                "reverse" => Some((ScalarFunction::Reverse, DataType::Text)),
                "starts_with" => Some((ScalarFunction::StartsWith, DataType::Boolean)),
                "concat" => Some((ScalarFunction::ConcatFunc, DataType::Text)),
                "lpad" => Some((ScalarFunction::Lpad, DataType::Text)),
                "rpad" => Some((ScalarFunction::Rpad, DataType::Text)),
                "position" => Some((ScalarFunction::Position, DataType::Int)),
                "initcap" => Some((ScalarFunction::Initcap, DataType::Text)),
                "split_part" => Some((ScalarFunction::SplitPart, DataType::Text)),
                _ => None,
            };
            if let Some((function, return_type)) = scalar_function {
                let typed_args = args
                    .iter()
                    .map(|arg| check_expr_to_typed(arg, table))
                    .collect::<DbResult<Vec<_>>>()?;
                return Ok(TypedExpr::scalar_function(
                    function,
                    typed_args,
                    return_type,
                    true,
                ));
            }
            Err(DbError::internal(format!(
                "unsupported function in CHECK constraint: {function_name}"
            )))
        }
        _ => Err(DbError::internal(format!(
            "unsupported expression type in CHECK constraint: {expr:?}"
        ))),
    }
}
