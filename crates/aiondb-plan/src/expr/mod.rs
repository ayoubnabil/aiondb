mod constructors;

#[cfg(test)]
mod tests;

use aiondb_core::{DataType, Value};

use crate::LogicalPlan;

/// Names of built-in scalar functions.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ScalarFunction {
    // Text functions
    Upper,
    Lower,
    Length,
    CharLength,
    OctetLength,
    Substring,
    Trim,
    Ltrim,
    Rtrim,
    Replace,
    Strpos,
    Left,
    Right,
    Repeat,
    Reverse,
    StartsWith,
    ConcatFunc,
    Lpad,
    Rpad,
    Position,
    // Date/time functions
    Now,
    CurrentTimestamp,
    CurrentDate,
    DatePart,
    Extract,
    DateTrunc,
    Age,
    ToChar,
    // Vector distance functions
    L2Distance,
    CosineDistance,
    InnerProduct,
    ManhattanDistance,
    VectorDims,
    L2Norm,
    L2Normalize,
    Subvector,
    BinaryQuantize,
    HammingDistance,
    JaccardDistance,
    /// Negated inner product: `-dot(a, b)`. Smaller = closer, so this can
    /// be used with `ORDER BY ... ASC LIMIT k` to retrieve the
    /// max-inner-product neighbours (pgvector `<#>`).
    NegativeInnerProduct,
    // Math functions
    Abs,
    Ceil,
    Floor,
    Round,
    Trunc,
    Power,
    Sqrt,
    Cbrt,
    Log,
    Ln,
    Exp,
    Mod,
    Sign,
    Pi,
    Random,
    Greatest,
    Least,
    // Additional text functions
    Initcap,
    SplitPart,
    Translate,
    Overlay,
    BitLength,
    Chr,
    Ascii,
    Md5,
    QuoteLiteral,
    QuoteIdent,
    QuoteNullable,
    ToHex,
    RegexpReplace,
    RegexpMatch,
    RegexpMatches,
    RegexpSplitToArray,
    RegexpSplitToTable,
    Encode,
    Decode,
    // Additional date/time functions
    MakeDate,
    MakeTimestamp,
    MakeInterval,
    MakeTime,
    CurrentTime,
    Localtime,
    ClockTimestamp,
    StatementTimestamp,
    TransactionTimestamp,
    // Array functions
    ArrayLength,
    ArrayUpper,
    ArrayLower,
    ArrayPosition,
    ArrayRemove,
    ArrayCat,
    ArrayAppend,
    ArrayPrepend,
    ArrayToString,
    StringToArray,
    ArrayDims,
    ArrayNdims,
    ArrayPositions,
    ArrayReplace,
    ArrayFill,
    ArraySample,
    ArrayShuffle,
    TrimArray,
    Cardinality,
    ArrayAssign,
    ArraySlice,
    FixedArrayAssign,
    FixedArraySlice,
    // JSONB functions
    JsonbTypeof,
    JsonbArrayLength,
    JsonbBuildObject,
    JsonbBuildArray,
    JsonbStripNulls,
    // New JSONB functions
    JsonbSet,
    JsonbExtractPath,
    JsonbExtractPathText,
    JsonbObjectKeys,
    JsonbPretty,
    // Utility functions
    PgTypeof,
    ConcatWs,
    Format,
    ToNumber,
    ToDate,
    ToTimestamp,
    // Row constructor
    Row,
    // Regex match operators (returns boolean)
    RegexMatchBool,
    RegexMatchBoolInsensitive,
    RegexNotMatchBool,
    RegexNotMatchBoolInsensitive,
    // Bitwise / shift / exponent operators
    BitwiseNotOp,
    BitwiseAndOp,
    BitwiseOrOp,
    BitwiseXorOp,
    ShiftLeftOp,
    ShiftRightOp,
    ExponentOp,
    // Array subscript
    ArrayGet,
    // Timezone conversion
    Timezone,
    // Set-returning functions
    GenerateSeries,
    Unnest,
    // PG catalog/utility functions
    PgInputIsValid,
    PgGetViewdef,
    // JSONB path query functions
    JsonbPathQueryFirst,
    JsonbPathQueryArray,
    JsonbPathExists,
    JsonbPathMatch,
    // Range constructors
    Int4Range,
    Int8Range,
    NumRange,
    DateRange,
    TsRange,
    TsTzRange,
    // Range functions
    RangeLower,
    RangeUpper,
    RangeIsEmpty,
    RangeLowerInc,
    RangeUpperInc,
    RangeLowerInf,
    RangeUpperInf,
    RangeMerge,
    RangeContains,
    RangeContainedBy,
    RangeAdjacent,
    // Multirange constructors
    NumMultirange,
    Int4Multirange,
    Int8Multirange,
    DateMultirange,
    TsMultirange,
    TsTzMultirange,
    // Text search
    TsLexize,
    ToTsvector,
    ToTsquery,
    PlaintoTsquery,
    PhrasetoTsquery,
    WebsearchToTsquery,
    TsHeadline,
    TsRank,
    TsRankCd,
    // Generic hook for named compatibility functions. Unsupported ones must
    // fail explicitly during planning or evaluation.
    Generic(String),
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum TypedExprKind {
    Literal(Value),
    ColumnRef {
        name: String,
        ordinal: usize,
    },
    /// Reference to a column from an outer (enclosing) query scope.
    /// Used in correlated subqueries (e.g. `EXISTS (SELECT 1 FROM t2 WHERE t2.x = t1.y)`).
    /// The ordinal is the column position in the outer row.
    OuterColumnRef {
        name: String,
        ordinal: usize,
    },
    NextValue {
        sequence_name: String,
    },
    BinaryEq {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    BinaryNe {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    BinaryGe {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    BinaryGt {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    BinaryLe {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    BinaryLt {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    LogicalAnd {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    LogicalOr {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    LogicalNot {
        expr: Box<TypedExpr>,
    },
    ArithAdd {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    ArithSub {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    ArithMul {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    ArithDiv {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    ArithMod {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    Concat {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    JsonGet {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    JsonGetText {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    JsonPathGet {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    JsonPathGetText {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    JsonContains {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    JsonContainedBy {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    JsonKeyExists {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    JsonAnyKeyExists {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    JsonAllKeysExist {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    ArrayConcat {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    ArrayContains {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    ArrayContainedBy {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    ArrayOverlap {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    Negate {
        expr: Box<TypedExpr>,
    },
    IsNull {
        expr: Box<TypedExpr>,
        negated: bool,
    },
    IsDistinctFrom {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
        negated: bool,
    },
    Like {
        expr: Box<TypedExpr>,
        pattern: Box<TypedExpr>,
        negated: bool,
        case_insensitive: bool,
    },
    InList {
        expr: Box<TypedExpr>,
        list: Vec<TypedExpr>,
        negated: bool,
    },
    Between {
        expr: Box<TypedExpr>,
        low: Box<TypedExpr>,
        high: Box<TypedExpr>,
        negated: bool,
    },
    Cast {
        expr: Box<TypedExpr>,
        target_type: DataType,
    },
    CaseWhen {
        conditions: Vec<TypedExpr>,
        results: Vec<TypedExpr>,
        else_result: Option<Box<TypedExpr>>,
    },
    Coalesce {
        args: Vec<TypedExpr>,
    },
    Nullif {
        left: Box<TypedExpr>,
        right: Box<TypedExpr>,
    },
    AggCount {
        expr: Option<Box<TypedExpr>>,
        distinct: bool,
        filter: Option<Box<TypedExpr>>,
    },
    AggSum {
        expr: Box<TypedExpr>,
        distinct: bool,
        filter: Option<Box<TypedExpr>>,
    },
    AggAvg {
        expr: Box<TypedExpr>,
        distinct: bool,
        filter: Option<Box<TypedExpr>>,
    },
    AggAnyValue {
        expr: Box<TypedExpr>,
        filter: Option<Box<TypedExpr>>,
    },
    AggMin {
        expr: Box<TypedExpr>,
        filter: Option<Box<TypedExpr>>,
    },
    AggMax {
        expr: Box<TypedExpr>,
        filter: Option<Box<TypedExpr>>,
    },
    AggStringAgg {
        expr: Box<TypedExpr>,
        delimiter: Box<TypedExpr>,
        distinct: bool,
        filter: Option<Box<TypedExpr>>,
    },
    AggArrayAgg {
        expr: Box<TypedExpr>,
        distinct: bool,
        filter: Option<Box<TypedExpr>>,
        order_descending: Option<bool>,
    },
    AggBoolAnd {
        expr: Box<TypedExpr>,
        filter: Option<Box<TypedExpr>>,
    },
    AggBoolOr {
        expr: Box<TypedExpr>,
        filter: Option<Box<TypedExpr>>,
    },
    AggStddevPop {
        expr: Box<TypedExpr>,
        filter: Option<Box<TypedExpr>>,
    },
    AggStddevSamp {
        expr: Box<TypedExpr>,
        filter: Option<Box<TypedExpr>>,
    },
    AggVarPop {
        expr: Box<TypedExpr>,
        filter: Option<Box<TypedExpr>>,
    },
    AggVarSamp {
        expr: Box<TypedExpr>,
        filter: Option<Box<TypedExpr>>,
    },
    ScalarFunction {
        func: ScalarFunction,
        args: Vec<TypedExpr>,
    },
    ArrayConstruct {
        elements: Vec<TypedExpr>,
    },
    UserFunction {
        name: String,
        args: Vec<TypedExpr>,
        body: String,
        params: Vec<(String, DataType)>,
        language: String,
    },
    ScalarSubquery {
        plan: Box<LogicalPlan>,
    },
    ArraySubquery {
        plan: Box<LogicalPlan>,
    },
    InSubquery {
        expr: Box<TypedExpr>,
        plan: Box<LogicalPlan>,
        negated: bool,
    },
    ExistsSubquery {
        plan: Box<LogicalPlan>,
        negated: bool,
    },
    WindowFunction {
        func: WindowFunctionKind,
        args: Vec<TypedExpr>,
        partition_by: Vec<TypedExpr>,
        order_by: Vec<crate::SortExpr>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum WindowFunctionKind {
    RowNumber,
    Rank,
    DenseRank,
    PercentRank,
    CumeDist,
    Ntile,
    Lag,
    Lead,
    FirstValue,
    LastValue,
    NthValue,
    Sum,
    Count,
    Avg,
    Min,
    Max,
    VarPop,
    VarSamp,
    StddevPop,
    StddevSamp,
}

macro_rules! binary_accessor {
    ($name:ident, $variant:ident) => {
        pub fn $name(&self) -> Option<(&TypedExpr, &TypedExpr)> {
            match self {
                Self::$variant { left, right } => Some((left, right)),
                _ => None,
            }
        }
    };
}

macro_rules! unary_accessor {
    ($name:ident, $variant:ident) => {
        pub fn $name(&self) -> Option<&TypedExpr> {
            match self {
                Self::$variant { expr } => Some(expr),
                _ => None,
            }
        }
    };
}

impl TypedExprKind {
    binary_accessor!(as_binary_eq, BinaryEq);
    binary_accessor!(as_binary_ne, BinaryNe);
    binary_accessor!(as_binary_ge, BinaryGe);
    binary_accessor!(as_binary_gt, BinaryGt);
    binary_accessor!(as_binary_le, BinaryLe);
    binary_accessor!(as_binary_lt, BinaryLt);
    binary_accessor!(as_logical_and, LogicalAnd);
    binary_accessor!(as_logical_or, LogicalOr);
    binary_accessor!(as_arith_add, ArithAdd);
    binary_accessor!(as_arith_sub, ArithSub);
    binary_accessor!(as_arith_mul, ArithMul);
    binary_accessor!(as_arith_div, ArithDiv);
    binary_accessor!(as_arith_mod, ArithMod);
    binary_accessor!(as_concat, Concat);
    binary_accessor!(as_json_get, JsonGet);
    binary_accessor!(as_json_get_text, JsonGetText);
    binary_accessor!(as_json_path_get, JsonPathGet);
    binary_accessor!(as_json_path_get_text, JsonPathGetText);
    binary_accessor!(as_json_contains, JsonContains);
    binary_accessor!(as_json_contained_by, JsonContainedBy);
    binary_accessor!(as_json_key_exists, JsonKeyExists);
    binary_accessor!(as_json_any_key_exists, JsonAnyKeyExists);
    binary_accessor!(as_json_all_keys_exist, JsonAllKeysExist);
    binary_accessor!(as_array_concat, ArrayConcat);
    binary_accessor!(as_array_contains, ArrayContains);
    binary_accessor!(as_array_contained_by, ArrayContainedBy);
    binary_accessor!(as_array_overlap, ArrayOverlap);
    binary_accessor!(as_nullif, Nullif);

    unary_accessor!(as_logical_not, LogicalNot);
    unary_accessor!(as_negate, Negate);

    pub fn as_literal(&self) -> Option<&Value> {
        match self {
            Self::Literal(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_next_value(&self) -> Option<&str> {
        match self {
            Self::NextValue { sequence_name } => Some(sequence_name),
            _ => None,
        }
    }

    pub fn as_column_ref(&self) -> Option<(&str, usize)> {
        match self {
            Self::ColumnRef { name, ordinal } => Some((name, *ordinal)),
            _ => None,
        }
    }

    pub fn as_outer_column_ref(&self) -> Option<(&str, usize)> {
        match self {
            Self::OuterColumnRef { name, ordinal } => Some((name, *ordinal)),
            _ => None,
        }
    }

    pub fn as_is_null(&self) -> Option<(&TypedExpr, bool)> {
        match self {
            Self::IsNull { expr, negated } => Some((expr, *negated)),
            _ => None,
        }
    }

    pub fn as_like(&self) -> Option<(&TypedExpr, &TypedExpr, bool, bool)> {
        match self {
            Self::Like {
                expr,
                pattern,
                negated,
                case_insensitive,
            } => Some((expr, pattern, *negated, *case_insensitive)),
            _ => None,
        }
    }

    pub fn as_in_list(&self) -> Option<(&TypedExpr, &[TypedExpr], bool)> {
        match self {
            Self::InList {
                expr,
                list,
                negated,
            } => Some((expr, list, *negated)),
            _ => None,
        }
    }

    pub fn as_between(&self) -> Option<(&TypedExpr, &TypedExpr, &TypedExpr, bool)> {
        match self {
            Self::Between {
                expr,
                low,
                high,
                negated,
            } => Some((expr, low, high, *negated)),
            _ => None,
        }
    }

    pub fn as_cast(&self) -> Option<(&TypedExpr, &DataType)> {
        match self {
            Self::Cast { expr, target_type } => Some((expr, target_type)),
            _ => None,
        }
    }

    pub fn as_case_when(&self) -> Option<(&[TypedExpr], &[TypedExpr], Option<&TypedExpr>)> {
        match self {
            Self::CaseWhen {
                conditions,
                results,
                else_result,
            } => Some((conditions, results, else_result.as_deref())),
            _ => None,
        }
    }

    pub fn as_coalesce(&self) -> Option<&[TypedExpr]> {
        match self {
            Self::Coalesce { args } => Some(args),
            _ => None,
        }
    }

    pub fn as_agg_count(&self) -> Option<Option<&TypedExpr>> {
        match self {
            Self::AggCount { expr, .. } => Some(expr.as_deref()),
            _ => None,
        }
    }

    pub fn as_agg_sum(&self) -> Option<&TypedExpr> {
        match self {
            Self::AggSum { expr, .. } => Some(expr),
            _ => None,
        }
    }

    pub fn as_agg_avg(&self) -> Option<&TypedExpr> {
        match self {
            Self::AggAvg { expr, .. } => Some(expr),
            _ => None,
        }
    }

    pub fn as_agg_any_value(&self) -> Option<&TypedExpr> {
        match self {
            Self::AggAnyValue { expr, .. } => Some(expr),
            _ => None,
        }
    }

    pub fn as_agg_min(&self) -> Option<&TypedExpr> {
        match self {
            Self::AggMin { expr, .. } => Some(expr),
            _ => None,
        }
    }

    pub fn as_agg_max(&self) -> Option<&TypedExpr> {
        match self {
            Self::AggMax { expr, .. } => Some(expr),
            _ => None,
        }
    }

    pub fn as_scalar_function(&self) -> Option<(&ScalarFunction, &[TypedExpr])> {
        match self {
            Self::ScalarFunction { func, args } => Some((func, args)),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TypedExpr {
    pub kind: TypedExprKind,
    pub data_type: DataType,
    pub nullable: bool,
}

// ---------------------------------------------------------------------------
// PostgreSQL-style expression rendering for EXPLAIN output
// ---------------------------------------------------------------------------

impl TypedExpr {
    /// Render the expression in a PostgreSQL-compatible style suitable for
    /// EXPLAIN output (e.g. `(ctid = '(0,1)'::tid)`).
    pub fn pg_display(&self) -> String {
        pg_display_expr(self)
    }

    /// Return `true` when this expression (or part of it) references `ctid`.
    pub fn references_ctid(&self) -> bool {
        expr_references_ctid(self)
    }
}

const PG_DISPLAY_MAX_DEPTH: u32 = 256;

thread_local! {
    static PG_DISPLAY_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

struct PgDisplayDepthGuard {
    too_deep: bool,
}

impl PgDisplayDepthGuard {
    fn enter() -> Self {
        let too_deep = PG_DISPLAY_DEPTH.with(|c| {
            let next = c.get().saturating_add(1);
            if next > PG_DISPLAY_MAX_DEPTH {
                true
            } else {
                c.set(next);
                false
            }
        });
        Self { too_deep }
    }
}

impl Drop for PgDisplayDepthGuard {
    fn drop(&mut self) {
        if !self.too_deep {
            PG_DISPLAY_DEPTH.with(|c| c.set(c.get().saturating_sub(1)));
        }
    }
}

fn pg_display_expr(expr: &TypedExpr) -> String {
    let guard = PgDisplayDepthGuard::enter();
    if guard.too_deep {
        return "<expr too deep>".to_string();
    }
    pg_display_expr_inner(expr)
}

fn pg_display_expr_inner(expr: &TypedExpr) -> String {
    match &expr.kind {
        TypedExprKind::Literal(v) => pg_display_literal(v, &expr.data_type),
        TypedExprKind::ColumnRef { name, .. } => name.replace('\0', "."),
        TypedExprKind::OuterColumnRef { name, .. } => name.replace('\0', "."),
        TypedExprKind::BinaryEq { left, right } => {
            format!("({} = {})", pg_display_expr(left), pg_display_expr(right))
        }
        TypedExprKind::BinaryNe { left, right } => {
            format!("({} <> {})", pg_display_expr(left), pg_display_expr(right))
        }
        TypedExprKind::BinaryGe { left, right } => {
            format!("({} >= {})", pg_display_expr(left), pg_display_expr(right))
        }
        TypedExprKind::BinaryGt { left, right } => {
            format!("({} > {})", pg_display_expr(left), pg_display_expr(right))
        }
        TypedExprKind::BinaryLe { left, right } => {
            format!("({} <= {})", pg_display_expr(left), pg_display_expr(right))
        }
        TypedExprKind::BinaryLt { left, right } => {
            format!("({} < {})", pg_display_expr(left), pg_display_expr(right))
        }
        TypedExprKind::LogicalAnd { left, right } => {
            format!("({} AND {})", pg_display_expr(left), pg_display_expr(right))
        }
        TypedExprKind::LogicalOr { left, right } => {
            format!("({} OR {})", pg_display_expr(left), pg_display_expr(right))
        }
        TypedExprKind::LogicalNot { expr } => {
            format!("(NOT {})", pg_display_expr(expr))
        }
        TypedExprKind::ArithAdd { left, right } => {
            format!("({} + {})", pg_display_expr(left), pg_display_expr(right))
        }
        TypedExprKind::ArithSub { left, right } => {
            format!("({} - {})", pg_display_expr(left), pg_display_expr(right))
        }
        TypedExprKind::ArithMul { left, right } => {
            format!("({} * {})", pg_display_expr(left), pg_display_expr(right))
        }
        TypedExprKind::ArithDiv { left, right } => {
            format!("({} / {})", pg_display_expr(left), pg_display_expr(right))
        }
        TypedExprKind::ArithMod { left, right } => {
            format!("({} %% {})", pg_display_expr(left), pg_display_expr(right))
        }
        TypedExprKind::Negate { expr } => {
            format!("(-{})", pg_display_expr(expr))
        }
        TypedExprKind::IsNull { expr, negated } => {
            if *negated {
                format!("({} IS NOT NULL)", pg_display_expr(expr))
            } else {
                format!("({} IS NULL)", pg_display_expr(expr))
            }
        }
        TypedExprKind::IsDistinctFrom {
            left,
            right,
            negated,
        } => {
            if *negated {
                format!(
                    "({} IS NOT DISTINCT FROM {})",
                    pg_display_expr(left),
                    pg_display_expr(right)
                )
            } else {
                format!(
                    "({} IS DISTINCT FROM {})",
                    pg_display_expr(left),
                    pg_display_expr(right)
                )
            }
        }
        TypedExprKind::Like {
            expr,
            pattern,
            negated,
            case_insensitive,
        } => {
            let op = match (*negated, *case_insensitive) {
                (false, false) => "~~",
                (true, false) => "!~~",
                (false, true) => "~~*",
                (true, true) => "!~~*",
            };
            format!(
                "({} {} {})",
                pg_display_expr(expr),
                op,
                pg_display_expr(pattern)
            )
        }
        TypedExprKind::InList {
            expr: inner_expr,
            list,
            negated,
        } => {
            // PostgreSQL renders IN-lists as = ANY('{...}'::type[]) in EXPLAIN.
            // Attempt to produce that format when elements share a common type.
            if !list.is_empty() && !*negated {
                let element_type = &list[0].data_type;
                let all_same_type = list.iter().all(|e| &e.data_type == element_type);
                if all_same_type {
                    let inner_strs: Vec<String> = list
                        .iter()
                        .map(|e| {
                            let raw = pg_display_expr(e);
                            // Strip trailing ::type cast suffix for the array literal
                            let stripped = raw
                                .strip_suffix(&format!("::{}", element_type.pg_type_name()))
                                .unwrap_or(&raw);
                            // Wrap in double quotes for PG array literal style
                            if stripped.starts_with('\'') && stripped.ends_with('\'') {
                                format!("\"{}\"", &stripped[1..stripped.len() - 1])
                            } else {
                                format!("\"{stripped}\"")
                            }
                        })
                        .collect();
                    let arr_literal = format!("{{{}}}", inner_strs.join(","));
                    let type_suffix = format!("{}[]", element_type.pg_type_name());
                    return format!(
                        "({} = ANY ('{arr_literal}'::{type_suffix}))",
                        pg_display_expr(inner_expr)
                    );
                }
            }
            let values: Vec<String> = list.iter().map(pg_display_expr).collect();
            let op = if *negated { "<> ALL" } else { "= ANY" };
            format!(
                "({} {} ({}))",
                pg_display_expr(inner_expr),
                op,
                values.join(", ")
            )
        }
        TypedExprKind::Between {
            expr,
            low,
            high,
            negated,
        } => {
            let op = if *negated { "NOT BETWEEN" } else { "BETWEEN" };
            format!(
                "({} {} {} AND {})",
                pg_display_expr(expr),
                op,
                pg_display_expr(low),
                pg_display_expr(high)
            )
        }
        TypedExprKind::Cast { expr, target_type } => {
            // When casting an array literal to another array type, render the
            // literal directly with the target element type (e.g.
            // '{"(0,1)"}'::tid[] instead of '{"(0,1)"}'::text[]::tid[]).
            if let DataType::Array(ref target_inner) = target_type {
                if let TypedExprKind::Literal(Value::Array(elements)) = &expr.kind {
                    let inner: Vec<String> = elements
                        .iter()
                        .map(|el| match el {
                            Value::Tid(t) => format!("\"{t}\""),
                            Value::Text(s) => format!("\"{s}\""),
                            other => format!("{other:?}"),
                        })
                        .collect();
                    let arr_str = format!("{{{}}}", inner.join(","));
                    return format!("'{arr_str}'::{}[]", target_inner.pg_type_name());
                }
            }
            let type_name = match target_type {
                DataType::Array(inner) => format!("{}[]", inner.pg_type_name()),
                _ => target_type.pg_type_name().to_owned(),
            };
            format!("{}::{}", pg_display_expr(expr), type_name)
        }
        TypedExprKind::ScalarFunction { func, args } => {
            // Detect quantified array comparison functions:
            // __aiondb_quantified_any_eq(scalar, array) -> (scalar = ANY (array))
            if let ScalarFunction::Generic(fname) = func {
                if let Some(rendered) = try_render_quantified_comparison(fname, args) {
                    return rendered;
                }
                if let Some(rendered) = try_render_is_json_predicate(fname, args) {
                    return rendered;
                }
            }
            let name = pg_scalar_function_name(func);
            let arg_strs: Vec<String> = args.iter().map(pg_display_expr).collect();
            format!("{}({})", name, arg_strs.join(", "))
        }
        TypedExprKind::AggCount { expr, distinct, .. } => match expr {
            Some(e) => {
                if *distinct {
                    format!("count(DISTINCT {})", pg_display_expr(e))
                } else {
                    format!("count({})", pg_display_expr(e))
                }
            }
            None => "count(*)".to_owned(),
        },
        TypedExprKind::AggSum { expr, distinct, .. } => {
            if *distinct {
                format!("sum(DISTINCT {})", pg_display_expr(expr))
            } else {
                format!("sum({})", pg_display_expr(expr))
            }
        }
        TypedExprKind::AggAvg { expr, distinct, .. } => {
            if *distinct {
                format!("avg(DISTINCT {})", pg_display_expr(expr))
            } else {
                format!("avg({})", pg_display_expr(expr))
            }
        }
        TypedExprKind::AggMin { expr, .. } => format!("min({})", pg_display_expr(expr)),
        TypedExprKind::AggMax { expr, .. } => format!("max({})", pg_display_expr(expr)),
        TypedExprKind::Coalesce { args } => {
            let arg_strs: Vec<String> = args.iter().map(pg_display_expr).collect();
            format!("COALESCE({})", arg_strs.join(", "))
        }
        TypedExprKind::Nullif { left, right } => {
            format!(
                "NULLIF({}, {})",
                pg_display_expr(left),
                pg_display_expr(right)
            )
        }
        TypedExprKind::CaseWhen {
            conditions,
            results,
            else_result,
        } => {
            // Stream each WHEN/THEN pair and the ELSE branch directly
            // into `s` instead of allocating a transient format!() per
            // arm.
            use std::fmt::Write;
            let mut s = "CASE".to_owned();
            for (cond, res) in conditions.iter().zip(results.iter()) {
                let _ = write!(
                    s,
                    " WHEN {} THEN {}",
                    pg_display_expr(cond),
                    pg_display_expr(res)
                );
            }
            if let Some(e) = else_result {
                let _ = write!(s, " ELSE {}", pg_display_expr(e));
            }
            s.push_str(" END");
            s
        }
        TypedExprKind::Concat { left, right } => {
            format!("({} || {})", pg_display_expr(left), pg_display_expr(right))
        }
        TypedExprKind::ArrayConstruct { elements } => {
            let el_strs: Vec<String> = elements.iter().map(pg_display_expr).collect();
            format!("ARRAY[{}]", el_strs.join(", "))
        }
        TypedExprKind::NextValue { sequence_name } => {
            format!("nextval('{sequence_name}')")
        }
        _ => format!("{:?}", expr.kind),
    }
}

fn pg_display_literal(v: &Value, data_type: &DataType) -> String {
    match v {
        Value::Null => "NULL".to_owned(),
        Value::Boolean(b) => if *b { "true" } else { "false" }.to_owned(),
        Value::Int(i) => i.to_string(),
        Value::BigInt(i) => i.to_string(),
        Value::Real(f) => format!("{f}"),
        Value::Double(f) => format!("{f}"),
        Value::Text(s) => {
            let escaped = s.replace('\'', "''");
            match data_type {
                DataType::Tid => format!("'{escaped}'::tid"),
                _ => format!("'{escaped}'"),
            }
        }
        Value::Tid(tid) => {
            let escaped = tid.to_string().replace('\'', "''");
            format!("'{escaped}'::tid")
        }
        Value::Array(elements) => {
            // Render as PG-style array literal, e.g. '{"(0,1)","(0,2)"}'::tid[]
            let inner: Vec<String> = elements
                .iter()
                .map(|el| match el {
                    Value::Tid(t) => format!("\"{t}\""),
                    Value::Text(s) => format!("\"{s}\""),
                    other => format!("{other:?}"),
                })
                .collect();
            let arr_str = format!("{{{}}}", inner.join(","));
            match data_type {
                DataType::Array(inner_type) => {
                    format!("'{arr_str}'::{}", pg_array_type_name(inner_type))
                }
                _ => format!("'{arr_str}'"),
            }
        }
        _ => format!("{v:?}"),
    }
}

fn pg_array_type_name(element_type: &DataType) -> String {
    format!("{}[]", element_type.pg_type_name())
}

fn pg_scalar_function_name(func: &ScalarFunction) -> &'static str {
    match func {
        ScalarFunction::Upper => "upper",
        ScalarFunction::Lower => "lower",
        ScalarFunction::Length => "length",
        ScalarFunction::CharLength => "char_length",
        ScalarFunction::OctetLength => "octet_length",
        ScalarFunction::Substring => "substring",
        ScalarFunction::Trim => "trim",
        ScalarFunction::Ltrim => "ltrim",
        ScalarFunction::Rtrim => "rtrim",
        ScalarFunction::Replace => "replace",
        ScalarFunction::Strpos => "strpos",
        ScalarFunction::Left => "left",
        ScalarFunction::Right => "right",
        ScalarFunction::Repeat => "repeat",
        ScalarFunction::Reverse => "reverse",
        ScalarFunction::Abs => "abs",
        ScalarFunction::Ceil => "ceil",
        ScalarFunction::Floor => "floor",
        ScalarFunction::Round => "round",
        ScalarFunction::Trunc => "trunc",
        ScalarFunction::Sqrt => "sqrt",
        ScalarFunction::Power => "power",
        ScalarFunction::Log => "log",
        ScalarFunction::Ln => "ln",
        ScalarFunction::Exp => "exp",
        ScalarFunction::Sign => "sign",
        ScalarFunction::Mod => "mod",
        ScalarFunction::BinaryQuantize => "binary_quantize",
        ScalarFunction::HammingDistance => "hamming_distance",
        ScalarFunction::JaccardDistance => "jaccard_distance",
        _ => "fn",
    }
}

/// Try to render a quantified array comparison function in PG style.
/// e.g. `__aiondb_quantified_any_eq(ctid, array)` -> `(ctid = ANY (array))`
fn try_render_quantified_comparison(fname: &str, args: &[TypedExpr]) -> Option<String> {
    if args.len() != 2 {
        return None;
    }
    let stripped = fname.strip_prefix("__aiondb_quantified_")?;
    let (quantifier, op) = if let Some(rest) = stripped.strip_prefix("any_") {
        ("ANY", rest)
    } else if let Some(rest) = stripped.strip_prefix("all_") {
        ("ALL", rest)
    } else {
        return None;
    };
    let op_symbol = match op {
        "eq" => "=",
        "ne" => "<>",
        "ge" => ">=",
        "gt" => ">",
        "le" => "<=",
        "lt" => "<",
        _ => return None,
    };
    let scalar = pg_display_expr(&args[0]);
    let array = pg_display_expr(&args[1]);
    Some(format!("({scalar} {op_symbol} {quantifier} ({array}))"))
}

fn typed_literal_text(expr: &TypedExpr) -> Option<&str> {
    match &expr.kind {
        TypedExprKind::Literal(Value::Text(text)) => Some(text.as_str()),
        _ => None,
    }
}

fn try_render_is_json_predicate(fname: &str, args: &[TypedExpr]) -> Option<String> {
    if fname != "__aiondb_is_json" || args.len() != 3 {
        return None;
    }
    let input = pg_display_expr(&args[0]);
    let kind = typed_literal_text(&args[1])?.to_ascii_uppercase();
    let unique_mode = typed_literal_text(&args[2])?.to_ascii_uppercase();
    let mut rendered = format!("{input} IS JSON");
    match kind.as_str() {
        "JSON" | "VALUE" => {}
        "OBJECT" => rendered.push_str(" OBJECT"),
        "ARRAY" => rendered.push_str(" ARRAY"),
        "SCALAR" => rendered.push_str(" SCALAR"),
        _ => return None,
    }
    match unique_mode.as_str() {
        "DEFAULT" => {}
        "WITH" => rendered.push_str(" WITH UNIQUE KEYS"),
        "WITHOUT" => rendered.push_str(" WITHOUT UNIQUE KEYS"),
        _ => return None,
    }
    Some(format!("({rendered})"))
}

fn expr_references_ctid(expr: &TypedExpr) -> bool {
    match &expr.kind {
        TypedExprKind::ColumnRef { name, .. } => name.eq_ignore_ascii_case("ctid"),
        TypedExprKind::BinaryEq { left, right }
        | TypedExprKind::BinaryNe { left, right }
        | TypedExprKind::BinaryGe { left, right }
        | TypedExprKind::BinaryGt { left, right }
        | TypedExprKind::BinaryLe { left, right }
        | TypedExprKind::BinaryLt { left, right }
        | TypedExprKind::LogicalAnd { left, right }
        | TypedExprKind::LogicalOr { left, right }
        | TypedExprKind::Concat { left, right }
        | TypedExprKind::ArithAdd { left, right }
        | TypedExprKind::ArithSub { left, right }
        | TypedExprKind::ArithMul { left, right }
        | TypedExprKind::ArithDiv { left, right }
        | TypedExprKind::ArithMod { left, right } => {
            expr_references_ctid(left) || expr_references_ctid(right)
        }
        TypedExprKind::LogicalNot { expr } | TypedExprKind::Negate { expr } => {
            expr_references_ctid(expr)
        }
        TypedExprKind::IsNull { expr, .. } => expr_references_ctid(expr),
        TypedExprKind::Cast { expr, .. } => expr_references_ctid(expr),
        TypedExprKind::InList { expr, list, .. } => {
            expr_references_ctid(expr) || list.iter().any(expr_references_ctid)
        }
        TypedExprKind::ScalarFunction { args, .. } => args.iter().any(expr_references_ctid),
        _ => false,
    }
}
