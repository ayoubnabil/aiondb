use super::*;

impl TypedExpr {
    pub fn literal(value: Value, data_type: DataType, nullable: bool) -> Self {
        Self {
            kind: TypedExprKind::Literal(value),
            data_type,
            nullable,
        }
    }

    pub fn column_ref(
        name: impl Into<String>,
        ordinal: usize,
        data_type: DataType,
        nullable: bool,
    ) -> Self {
        Self {
            kind: TypedExprKind::ColumnRef {
                name: name.into(),
                ordinal,
            },
            data_type,
            nullable,
        }
    }

    /// Create an outer column reference (for correlated subqueries).
    pub fn outer_column_ref(
        name: impl Into<String>,
        ordinal: usize,
        data_type: DataType,
        nullable: bool,
    ) -> Self {
        Self {
            kind: TypedExprKind::OuterColumnRef {
                name: name.into(),
                ordinal,
            },
            data_type,
            nullable,
        }
    }

    pub fn next_value(sequence_name: impl Into<String>) -> Self {
        Self {
            kind: TypedExprKind::NextValue {
                sequence_name: sequence_name.into(),
            },
            data_type: DataType::BigInt,
            nullable: false,
        }
    }

    pub fn binary_eq(left: TypedExpr, right: TypedExpr) -> Self {
        let nullable = left.nullable || right.nullable;
        Self {
            kind: TypedExprKind::BinaryEq {
                left: Box::new(left),
                right: Box::new(right),
            },
            data_type: DataType::Boolean,
            nullable,
        }
    }

    pub fn binary_ne(left: TypedExpr, right: TypedExpr) -> Self {
        let nullable = left.nullable || right.nullable;
        Self {
            kind: TypedExprKind::BinaryNe {
                left: Box::new(left),
                right: Box::new(right),
            },
            data_type: DataType::Boolean,
            nullable,
        }
    }

    pub fn binary_gt(left: TypedExpr, right: TypedExpr) -> Self {
        let nullable = left.nullable || right.nullable;
        Self {
            kind: TypedExprKind::BinaryGt {
                left: Box::new(left),
                right: Box::new(right),
            },
            data_type: DataType::Boolean,
            nullable,
        }
    }

    pub fn binary_ge(left: TypedExpr, right: TypedExpr) -> Self {
        let nullable = left.nullable || right.nullable;
        Self {
            kind: TypedExprKind::BinaryGe {
                left: Box::new(left),
                right: Box::new(right),
            },
            data_type: DataType::Boolean,
            nullable,
        }
    }

    pub fn binary_lt(left: TypedExpr, right: TypedExpr) -> Self {
        let nullable = left.nullable || right.nullable;
        Self {
            kind: TypedExprKind::BinaryLt {
                left: Box::new(left),
                right: Box::new(right),
            },
            data_type: DataType::Boolean,
            nullable,
        }
    }

    pub fn binary_le(left: TypedExpr, right: TypedExpr) -> Self {
        let nullable = left.nullable || right.nullable;
        Self {
            kind: TypedExprKind::BinaryLe {
                left: Box::new(left),
                right: Box::new(right),
            },
            data_type: DataType::Boolean,
            nullable,
        }
    }

    pub fn logical_and(left: TypedExpr, right: TypedExpr) -> Self {
        let nullable = left.nullable || right.nullable;
        Self {
            kind: TypedExprKind::LogicalAnd {
                left: Box::new(left),
                right: Box::new(right),
            },
            data_type: DataType::Boolean,
            nullable,
        }
    }

    pub fn logical_or(left: TypedExpr, right: TypedExpr) -> Self {
        let nullable = left.nullable || right.nullable;
        Self {
            kind: TypedExprKind::LogicalOr {
                left: Box::new(left),
                right: Box::new(right),
            },
            data_type: DataType::Boolean,
            nullable,
        }
    }

    pub fn logical_not(expr: TypedExpr) -> Self {
        let nullable = expr.nullable;
        Self {
            kind: TypedExprKind::LogicalNot {
                expr: Box::new(expr),
            },
            data_type: DataType::Boolean,
            nullable,
        }
    }

    pub fn arith_add(
        left: TypedExpr,
        right: TypedExpr,
        data_type: DataType,
        nullable: bool,
    ) -> Self {
        Self {
            kind: TypedExprKind::ArithAdd {
                left: Box::new(left),
                right: Box::new(right),
            },
            data_type,
            nullable,
        }
    }

    pub fn arith_sub(
        left: TypedExpr,
        right: TypedExpr,
        data_type: DataType,
        nullable: bool,
    ) -> Self {
        Self {
            kind: TypedExprKind::ArithSub {
                left: Box::new(left),
                right: Box::new(right),
            },
            data_type,
            nullable,
        }
    }

    pub fn arith_mul(
        left: TypedExpr,
        right: TypedExpr,
        data_type: DataType,
        nullable: bool,
    ) -> Self {
        Self {
            kind: TypedExprKind::ArithMul {
                left: Box::new(left),
                right: Box::new(right),
            },
            data_type,
            nullable,
        }
    }

    pub fn arith_div(
        left: TypedExpr,
        right: TypedExpr,
        data_type: DataType,
        nullable: bool,
    ) -> Self {
        Self {
            kind: TypedExprKind::ArithDiv {
                left: Box::new(left),
                right: Box::new(right),
            },
            data_type,
            nullable,
        }
    }

    pub fn arith_mod(
        left: TypedExpr,
        right: TypedExpr,
        data_type: DataType,
        nullable: bool,
    ) -> Self {
        Self {
            kind: TypedExprKind::ArithMod {
                left: Box::new(left),
                right: Box::new(right),
            },
            data_type,
            nullable,
        }
    }

    pub fn json_get(left: TypedExpr, right: TypedExpr) -> Self {
        Self {
            kind: TypedExprKind::JsonGet {
                left: Box::new(left),
                right: Box::new(right),
            },
            data_type: DataType::Jsonb,
            nullable: true,
        }
    }

    pub fn json_get_text(left: TypedExpr, right: TypedExpr) -> Self {
        Self {
            kind: TypedExprKind::JsonGetText {
                left: Box::new(left),
                right: Box::new(right),
            },
            data_type: DataType::Text,
            nullable: true,
        }
    }

    pub fn json_path_get(left: TypedExpr, right: TypedExpr) -> Self {
        Self {
            kind: TypedExprKind::JsonPathGet {
                left: Box::new(left),
                right: Box::new(right),
            },
            data_type: DataType::Jsonb,
            nullable: true,
        }
    }

    pub fn json_path_get_text(left: TypedExpr, right: TypedExpr) -> Self {
        Self {
            kind: TypedExprKind::JsonPathGetText {
                left: Box::new(left),
                right: Box::new(right),
            },
            data_type: DataType::Text,
            nullable: true,
        }
    }

    pub fn json_contains(left: TypedExpr, right: TypedExpr) -> Self {
        Self {
            kind: TypedExprKind::JsonContains {
                left: Box::new(left),
                right: Box::new(right),
            },
            data_type: DataType::Boolean,
            nullable: false,
        }
    }

    pub fn json_contained_by(left: TypedExpr, right: TypedExpr) -> Self {
        Self {
            kind: TypedExprKind::JsonContainedBy {
                left: Box::new(left),
                right: Box::new(right),
            },
            data_type: DataType::Boolean,
            nullable: false,
        }
    }

    pub fn json_key_exists(left: TypedExpr, right: TypedExpr) -> Self {
        Self {
            kind: TypedExprKind::JsonKeyExists {
                left: Box::new(left),
                right: Box::new(right),
            },
            data_type: DataType::Boolean,
            nullable: false,
        }
    }

    pub fn json_any_key_exists(left: TypedExpr, right: TypedExpr) -> Self {
        Self {
            kind: TypedExprKind::JsonAnyKeyExists {
                left: Box::new(left),
                right: Box::new(right),
            },
            data_type: DataType::Boolean,
            nullable: false,
        }
    }

    pub fn json_all_keys_exist(left: TypedExpr, right: TypedExpr) -> Self {
        Self {
            kind: TypedExprKind::JsonAllKeysExist {
                left: Box::new(left),
                right: Box::new(right),
            },
            data_type: DataType::Boolean,
            nullable: false,
        }
    }

    pub fn array_concat(left: TypedExpr, right: TypedExpr, data_type: DataType) -> Self {
        let nullable = left.nullable || right.nullable;
        Self {
            kind: TypedExprKind::ArrayConcat {
                left: Box::new(left),
                right: Box::new(right),
            },
            data_type,
            nullable,
        }
    }

    pub fn array_contains(left: TypedExpr, right: TypedExpr) -> Self {
        Self {
            kind: TypedExprKind::ArrayContains {
                left: Box::new(left),
                right: Box::new(right),
            },
            data_type: DataType::Boolean,
            nullable: false,
        }
    }

    pub fn array_contained_by(left: TypedExpr, right: TypedExpr) -> Self {
        Self {
            kind: TypedExprKind::ArrayContainedBy {
                left: Box::new(left),
                right: Box::new(right),
            },
            data_type: DataType::Boolean,
            nullable: false,
        }
    }

    pub fn array_overlap(left: TypedExpr, right: TypedExpr) -> Self {
        Self {
            kind: TypedExprKind::ArrayOverlap {
                left: Box::new(left),
                right: Box::new(right),
            },
            data_type: DataType::Boolean,
            nullable: false,
        }
    }

    pub fn concat(left: TypedExpr, right: TypedExpr) -> Self {
        let nullable = left.nullable || right.nullable;
        Self {
            kind: TypedExprKind::Concat {
                left: Box::new(left),
                right: Box::new(right),
            },
            data_type: DataType::Text,
            nullable,
        }
    }

    pub fn concat_typed(left: TypedExpr, right: TypedExpr, data_type: DataType) -> Self {
        let nullable = left.nullable || right.nullable;
        Self {
            kind: TypedExprKind::Concat {
                left: Box::new(left),
                right: Box::new(right),
            },
            data_type,
            nullable,
        }
    }

    pub fn negate(expr: TypedExpr, data_type: DataType, nullable: bool) -> Self {
        Self {
            kind: TypedExprKind::Negate {
                expr: Box::new(expr),
            },
            data_type,
            nullable,
        }
    }

    pub fn is_null(expr: TypedExpr, negated: bool) -> Self {
        Self {
            kind: TypedExprKind::IsNull {
                expr: Box::new(expr),
                negated,
            },
            data_type: DataType::Boolean,
            nullable: false,
        }
    }

    pub fn is_distinct_from(left: TypedExpr, right: TypedExpr, negated: bool) -> Self {
        Self {
            kind: TypedExprKind::IsDistinctFrom {
                left: Box::new(left),
                right: Box::new(right),
                negated,
            },
            data_type: DataType::Boolean,
            nullable: false,
        }
    }

    pub fn like(
        expr: TypedExpr,
        pattern: TypedExpr,
        negated: bool,
        case_insensitive: bool,
    ) -> Self {
        let nullable = expr.nullable || pattern.nullable;
        Self {
            kind: TypedExprKind::Like {
                expr: Box::new(expr),
                pattern: Box::new(pattern),
                negated,
                case_insensitive,
            },
            data_type: DataType::Boolean,
            nullable,
        }
    }

    pub fn in_list(expr: TypedExpr, list: Vec<TypedExpr>, negated: bool) -> Self {
        let nullable = expr.nullable || list.iter().any(|item| item.nullable);
        Self {
            kind: TypedExprKind::InList {
                expr: Box::new(expr),
                list,
                negated,
            },
            data_type: DataType::Boolean,
            nullable,
        }
    }

    pub fn between(expr: TypedExpr, low: TypedExpr, high: TypedExpr, negated: bool) -> Self {
        let nullable = expr.nullable || low.nullable || high.nullable;
        Self {
            kind: TypedExprKind::Between {
                expr: Box::new(expr),
                low: Box::new(low),
                high: Box::new(high),
                negated,
            },
            data_type: DataType::Boolean,
            nullable,
        }
    }

    pub fn cast(expr: TypedExpr, target_type: DataType) -> Self {
        let nullable = expr.nullable;
        Self {
            kind: TypedExprKind::Cast {
                expr: Box::new(expr),
                target_type: target_type.clone(),
            },
            data_type: target_type,
            nullable,
        }
    }

    pub fn case_when(
        conditions: Vec<TypedExpr>,
        results: Vec<TypedExpr>,
        else_result: Option<TypedExpr>,
        data_type: DataType,
        nullable: bool,
    ) -> Self {
        Self {
            kind: TypedExprKind::CaseWhen {
                conditions,
                results,
                else_result: else_result.map(Box::new),
            },
            data_type,
            nullable,
        }
    }

    pub fn coalesce(args: Vec<TypedExpr>, data_type: DataType) -> Self {
        Self {
            kind: TypedExprKind::Coalesce { args },
            data_type,
            nullable: true,
        }
    }

    pub fn nullif(left: TypedExpr, right: TypedExpr, data_type: DataType) -> Self {
        Self {
            kind: TypedExprKind::Nullif {
                left: Box::new(left),
                right: Box::new(right),
            },
            data_type,
            nullable: true,
        }
    }

    pub fn agg_count(expr: Option<TypedExpr>) -> Self {
        Self {
            kind: TypedExprKind::AggCount {
                expr: expr.map(Box::new),
                distinct: false,
                filter: None,
            },
            data_type: DataType::BigInt,
            nullable: false,
        }
    }

    pub fn agg_count_ext(
        expr: Option<TypedExpr>,
        distinct: bool,
        filter: Option<TypedExpr>,
    ) -> Self {
        Self {
            kind: TypedExprKind::AggCount {
                expr: expr.map(Box::new),
                distinct,
                filter: filter.map(Box::new),
            },
            data_type: DataType::BigInt,
            nullable: false,
        }
    }

    pub fn agg_sum(expr: TypedExpr) -> Self {
        let data_type = expr.data_type.clone();
        Self {
            kind: TypedExprKind::AggSum {
                expr: Box::new(expr),
                distinct: false,
                filter: None,
            },
            data_type,
            nullable: true,
        }
    }

    pub fn agg_sum_ext(expr: TypedExpr, distinct: bool, filter: Option<TypedExpr>) -> Self {
        let data_type = expr.data_type.clone();
        Self {
            kind: TypedExprKind::AggSum {
                expr: Box::new(expr),
                distinct,
                filter: filter.map(Box::new),
            },
            data_type,
            nullable: true,
        }
    }

    pub fn agg_avg(expr: TypedExpr) -> Self {
        let data_type = if matches!(expr.data_type, DataType::Vector { .. }) {
            expr.data_type.clone()
        } else {
            DataType::Double
        };
        Self {
            kind: TypedExprKind::AggAvg {
                expr: Box::new(expr),
                distinct: false,
                filter: None,
            },
            data_type,
            nullable: true,
        }
    }

    pub fn agg_avg_ext(expr: TypedExpr, distinct: bool, filter: Option<TypedExpr>) -> Self {
        let data_type = if matches!(expr.data_type, DataType::Vector { .. }) {
            expr.data_type.clone()
        } else {
            DataType::Double
        };
        Self {
            kind: TypedExprKind::AggAvg {
                expr: Box::new(expr),
                distinct,
                filter: filter.map(Box::new),
            },
            data_type,
            nullable: true,
        }
    }

    pub fn agg_any_value_ext(expr: TypedExpr, filter: Option<TypedExpr>) -> Self {
        let data_type = expr.data_type.clone();
        Self {
            kind: TypedExprKind::AggAnyValue {
                expr: Box::new(expr),
                filter: filter.map(Box::new),
            },
            data_type,
            nullable: true,
        }
    }

    pub fn agg_min(expr: TypedExpr) -> Self {
        let data_type = expr.data_type.clone();
        Self {
            kind: TypedExprKind::AggMin {
                expr: Box::new(expr),
                filter: None,
            },
            data_type,
            nullable: true,
        }
    }

    pub fn agg_min_ext(expr: TypedExpr, filter: Option<TypedExpr>) -> Self {
        let data_type = expr.data_type.clone();
        Self {
            kind: TypedExprKind::AggMin {
                expr: Box::new(expr),
                filter: filter.map(Box::new),
            },
            data_type,
            nullable: true,
        }
    }

    pub fn agg_max(expr: TypedExpr) -> Self {
        let data_type = expr.data_type.clone();
        Self {
            kind: TypedExprKind::AggMax {
                expr: Box::new(expr),
                filter: None,
            },
            data_type,
            nullable: true,
        }
    }

    pub fn agg_max_ext(expr: TypedExpr, filter: Option<TypedExpr>) -> Self {
        let data_type = expr.data_type.clone();
        Self {
            kind: TypedExprKind::AggMax {
                expr: Box::new(expr),
                filter: filter.map(Box::new),
            },
            data_type,
            nullable: true,
        }
    }

    pub fn agg_string_agg(expr: TypedExpr, delimiter: TypedExpr) -> Self {
        Self {
            kind: TypedExprKind::AggStringAgg {
                expr: Box::new(expr),
                delimiter: Box::new(delimiter),
                distinct: false,
                filter: None,
            },
            data_type: DataType::Text,
            nullable: true,
        }
    }

    pub fn agg_string_agg_ext(
        expr: TypedExpr,
        delimiter: TypedExpr,
        distinct: bool,
        filter: Option<TypedExpr>,
    ) -> Self {
        Self {
            kind: TypedExprKind::AggStringAgg {
                expr: Box::new(expr),
                delimiter: Box::new(delimiter),
                distinct,
                filter: filter.map(Box::new),
            },
            data_type: DataType::Text,
            nullable: true,
        }
    }

    pub fn agg_array_agg(expr: TypedExpr) -> Self {
        let elem_type = expr.data_type.clone();
        Self {
            kind: TypedExprKind::AggArrayAgg {
                expr: Box::new(expr),
                distinct: false,
                filter: None,
                order_descending: None,
            },
            data_type: DataType::Array(Box::new(elem_type)),
            nullable: true,
        }
    }

    pub fn agg_array_agg_ext(
        expr: TypedExpr,
        distinct: bool,
        filter: Option<TypedExpr>,
        order_descending: Option<bool>,
    ) -> Self {
        let elem_type = expr.data_type.clone();
        Self {
            kind: TypedExprKind::AggArrayAgg {
                expr: Box::new(expr),
                distinct,
                filter: filter.map(Box::new),
                order_descending,
            },
            data_type: DataType::Array(Box::new(elem_type)),
            nullable: true,
        }
    }

    pub fn agg_bool_and(expr: TypedExpr) -> Self {
        Self {
            kind: TypedExprKind::AggBoolAnd {
                expr: Box::new(expr),
                filter: None,
            },
            data_type: DataType::Boolean,
            nullable: true,
        }
    }

    pub fn agg_bool_and_ext(expr: TypedExpr, filter: Option<TypedExpr>) -> Self {
        Self {
            kind: TypedExprKind::AggBoolAnd {
                expr: Box::new(expr),
                filter: filter.map(Box::new),
            },
            data_type: DataType::Boolean,
            nullable: true,
        }
    }

    pub fn agg_bool_or(expr: TypedExpr) -> Self {
        Self {
            kind: TypedExprKind::AggBoolOr {
                expr: Box::new(expr),
                filter: None,
            },
            data_type: DataType::Boolean,
            nullable: true,
        }
    }

    pub fn agg_bool_or_ext(expr: TypedExpr, filter: Option<TypedExpr>) -> Self {
        Self {
            kind: TypedExprKind::AggBoolOr {
                expr: Box::new(expr),
                filter: filter.map(Box::new),
            },
            data_type: DataType::Boolean,
            nullable: true,
        }
    }

    pub fn agg_stddev_pop_ext(expr: TypedExpr, filter: Option<TypedExpr>) -> Self {
        Self {
            kind: TypedExprKind::AggStddevPop {
                expr: Box::new(expr),
                filter: filter.map(Box::new),
            },
            data_type: DataType::Double,
            nullable: true,
        }
    }

    pub fn agg_stddev_samp_ext(expr: TypedExpr, filter: Option<TypedExpr>) -> Self {
        Self {
            kind: TypedExprKind::AggStddevSamp {
                expr: Box::new(expr),
                filter: filter.map(Box::new),
            },
            data_type: DataType::Double,
            nullable: true,
        }
    }

    pub fn agg_var_pop_ext(expr: TypedExpr, filter: Option<TypedExpr>) -> Self {
        Self {
            kind: TypedExprKind::AggVarPop {
                expr: Box::new(expr),
                filter: filter.map(Box::new),
            },
            data_type: DataType::Double,
            nullable: true,
        }
    }

    pub fn agg_var_samp_ext(expr: TypedExpr, filter: Option<TypedExpr>) -> Self {
        Self {
            kind: TypedExprKind::AggVarSamp {
                expr: Box::new(expr),
                filter: filter.map(Box::new),
            },
            data_type: DataType::Double,
            nullable: true,
        }
    }

    pub fn scalar_function(
        func: ScalarFunction,
        args: Vec<TypedExpr>,
        data_type: DataType,
        nullable: bool,
    ) -> Self {
        Self {
            kind: TypedExprKind::ScalarFunction { func, args },
            data_type,
            nullable,
        }
    }

    pub fn array_construct(
        elements: Vec<TypedExpr>,
        element_type: DataType,
        nullable: bool,
    ) -> Self {
        Self {
            kind: TypedExprKind::ArrayConstruct { elements },
            data_type: DataType::Array(Box::new(element_type)),
            nullable,
        }
    }

    pub fn user_function(
        name: String,
        args: Vec<TypedExpr>,
        body: String,
        params: Vec<(String, DataType)>,
        return_type: DataType,
        language: String,
    ) -> Self {
        Self {
            kind: TypedExprKind::UserFunction {
                name,
                args,
                body,
                params,
                language,
            },
            data_type: return_type,
            nullable: true,
        }
    }

    pub fn scalar_subquery(plan: LogicalPlan, data_type: DataType, nullable: bool) -> Self {
        Self {
            kind: TypedExprKind::ScalarSubquery {
                plan: Box::new(plan),
            },
            data_type,
            nullable,
        }
    }

    pub fn array_subquery(plan: LogicalPlan, data_type: DataType) -> Self {
        Self {
            kind: TypedExprKind::ArraySubquery {
                plan: Box::new(plan),
            },
            data_type,
            nullable: false,
        }
    }

    pub fn in_subquery(expr: TypedExpr, plan: LogicalPlan, negated: bool) -> Self {
        Self {
            kind: TypedExprKind::InSubquery {
                expr: Box::new(expr),
                plan: Box::new(plan),
                negated,
            },
            data_type: DataType::Boolean,
            nullable: true,
        }
    }

    pub fn exists_subquery(plan: LogicalPlan, negated: bool) -> Self {
        Self {
            kind: TypedExprKind::ExistsSubquery {
                plan: Box::new(plan),
                negated,
            },
            data_type: DataType::Boolean,
            nullable: false,
        }
    }
}
