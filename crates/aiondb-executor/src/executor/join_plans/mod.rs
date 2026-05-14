use super::*;

mod set_ops;

/// FxHash variant inlined to avoid adding `rustc-hash` to the
/// workspace dep graph. Hash join indexes are keyed on integers and
/// short structural components from internal data — never untrusted
/// user-supplied bytes — so we can swap SipHash's anti-DoS guarantee
/// for FxHash's much faster mix without changing any threat model.
/// PG uses analogous "internal hash" entry points (`hash_uint32`,
/// `hash_combine_uint32`) instead of SipHash for the same reason.
#[derive(Default)]
pub(crate) struct JoinFxHasher {
    state: u64,
}

impl std::hash::Hasher for JoinFxHasher {
    #[inline]
    fn write(&mut self, mut bytes: &[u8]) {
        const SEED: u64 = 0x517c_c1b7_2722_0a95;
        while bytes.len() >= 8 {
            let mut chunk_bytes = [0u8; 8];
            chunk_bytes.copy_from_slice(&bytes[..8]);
            let chunk = u64::from_ne_bytes(chunk_bytes);
            self.state = (self.state.rotate_left(5) ^ chunk).wrapping_mul(SEED);
            bytes = &bytes[8..];
        }
        if !bytes.is_empty() {
            let mut last = [0u8; 8];
            last[..bytes.len()].copy_from_slice(bytes);
            let chunk = u64::from_ne_bytes(last);
            self.state = (self.state.rotate_left(5) ^ chunk).wrapping_mul(SEED);
        }
    }

    #[inline]
    fn write_u64(&mut self, value: u64) {
        const SEED: u64 = 0x517c_c1b7_2722_0a95;
        self.state = (self.state.rotate_left(5) ^ value).wrapping_mul(SEED);
    }

    #[inline]
    fn write_u32(&mut self, value: u32) {
        self.write_u64(u64::from(value));
    }

    #[inline]
    fn finish(&self) -> u64 {
        self.state
    }
}

pub(crate) type JoinFxBuildHasher = std::hash::BuildHasherDefault<JoinFxHasher>;

/// Decide once per join whether any expression flowing through the
/// pipeline references a PG-compat system column (`ctid`,
/// `tableoid`, `xmin`, etc.). When none of them do, the per-row
/// `compat_scan_row_consume` call can drop the 6+1 trailing
/// `Value::Null`/`Value::Tid` pushes for every input row — for a
/// 10k-row probe side that's 60k+ skipped value pushes per
/// statement. PG only emits these system columns when the planner
/// actually inserted a `Var::ctid` reference (`pg_class.attnum < 0`).
#[allow(dead_code)]
pub(crate) fn join_pipeline_needs_compat_columns(
    outputs: &[ProjectionExpr],
    condition: Option<&TypedExpr>,
    filter: Option<&TypedExpr>,
    order_by: &[SortExpr],
) -> bool {
    use super::projection_plans::expr_references_compat_system_column;
    if outputs
        .iter()
        .any(|o| expr_references_compat_system_column(&o.expr))
    {
        return true;
    }
    if condition.is_some_and(expr_references_compat_system_column) {
        return true;
    }
    if filter.is_some_and(expr_references_compat_system_column) {
        return true;
    }
    if order_by
        .iter()
        .any(|s| expr_references_compat_system_column(&s.expr))
    {
        return true;
    }
    false
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub(crate) enum JoinHashKeyComponent {
    /// Cross-type-compatible integer canonical form. `Int(37)`,
    /// `BigInt(37)`, and an integer-valued `Numeric(37)` all map to
    /// `ExactInteger(37_i128)` so they hash and compare the same in
    /// hash-join keys without paying for an intermediate `String`
    /// allocation per side.
    ExactInteger(i128),
    /// Canonical-text form for non-integer numerics (real / double /
    /// fractional decimal). Strings let us preserve exact equality
    /// across types whose binary representations differ but whose
    /// values are equal.
    ExactNumeric(String),
    Value(ValueHashKey),
}

pub(crate) type JoinHashKey = Vec<JoinHashKeyComponent>;

const JOIN_HASH_INDEX_CAPACITY_CAP: usize = 8_192;
const JOIN_HASH_INDEX_CAPACITY_PER_WORKER: usize = 2_048;
const JOIN_HASH_INDEX_BUILD_HARD_CAP_BYTES: u64 = 512 * 1024 * 1024;
const JOIN_HASH_INDEX_ENTRY_OVERHEAD_BYTES: u64 = 64;

/// Maximum rows to materialize for a single join child before
/// aborting with an error.  This prevents OOM when joining large
/// tables without proper filtering.
const JOIN_CHILD_MATERIALIZE_ROW_CAP: u64 = 10_000_000;

#[derive(Clone)]
struct IndexedVectorRerankSpec {
    output_mappings: Vec<IndexedVectorOutputMapping>,
    rebased_order_by: Vec<SortExpr>,
    right_vector_ordinal: usize,
    query_vector: aiondb_core::VectorValue,
    limit: Option<u64>,
}

#[derive(Clone)]
enum IndexedVectorOutputMapping {
    LeftColumn { ordinal: usize },
    RightColumn { ordinal: usize },
    Distance,
}

#[derive(Debug)]
struct InnerHashJoinSpec {
    left_ordinals: Vec<usize>,
    right_ordinals: Vec<usize>,
}

#[derive(Clone, Debug)]
struct FullHashJoinExprSpec {
    left_key_expr: TypedExpr,
    right_key_expr: TypedExpr,
}

fn extract_full_hash_join_expr_spec(
    condition: Option<&TypedExpr>,
    left_width: usize,
    right_width: usize,
) -> Option<FullHashJoinExprSpec> {
    let condition = condition?;
    if left_width == 0 || right_width == 0 {
        return None;
    }
    let total_width = left_width.checked_add(right_width)?;
    let mut conjuncts = Vec::new();
    flatten_full_hash_join_ands(condition, &mut conjuncts);
    for conjunct in conjuncts {
        let TypedExprKind::BinaryEq { left, right } = &conjunct.kind else {
            continue;
        };
        if !supports_hash_join_key_equality(left, right) {
            continue;
        }
        if hash_key_expr_references_only_join_side(left, left_width, total_width, true)
            && hash_key_expr_references_only_join_side(right, left_width, total_width, false)
        {
            return Some(FullHashJoinExprSpec {
                left_key_expr: left.as_ref().clone(),
                right_key_expr: right.as_ref().clone(),
            });
        }
        if hash_key_expr_references_only_join_side(right, left_width, total_width, true)
            && hash_key_expr_references_only_join_side(left, left_width, total_width, false)
        {
            return Some(FullHashJoinExprSpec {
                left_key_expr: right.as_ref().clone(),
                right_key_expr: left.as_ref().clone(),
            });
        }
    }
    None
}

fn flatten_full_hash_join_ands(expr: &TypedExpr, out: &mut Vec<TypedExpr>) {
    match &expr.kind {
        TypedExprKind::LogicalAnd { left, right } => {
            flatten_full_hash_join_ands(left, out);
            flatten_full_hash_join_ands(right, out);
        }
        _ => out.push(expr.clone()),
    }
}

fn values_match_quantified_join_order(value: &Value, filter_value: &Value) -> bool {
    if matches!(value, Value::Null) || matches!(filter_value, Value::Null) {
        return false;
    }
    match (value, filter_value) {
        (Value::Int(left), Value::BigInt(right)) => i64::from(*left) == *right,
        (Value::BigInt(left), Value::Int(right)) => *left == i64::from(*right),
        _ => value == filter_value,
    }
}

fn parse_quantified_array_literal_element(elem: &str) -> Value {
    if elem.eq_ignore_ascii_case("null") {
        Value::Null
    } else if let Ok(i) = elem.parse::<i32>() {
        Value::Int(i)
    } else if let Ok(i) = elem.parse::<i64>() {
        Value::BigInt(i)
    } else {
        Value::Text(elem.trim_matches('"').to_owned())
    }
}

fn parse_quantified_array_literal_body(s: &str) -> Option<Vec<Value>> {
    if !s.starts_with('{') || !s.ends_with('}') {
        return None;
    }
    let inner = &s[1..s.len() - 1];
    if inner.is_empty() {
        return Some(Vec::new());
    }
    Some(
        inner
            .split(',')
            .map(|elem| parse_quantified_array_literal_element(elem.trim()))
            .collect(),
    )
}

fn coerce_quantified_array_elements(value: &Value) -> Option<Vec<Value>> {
    match value {
        Value::Array(elements) => Some(elements.clone()),
        Value::Text(text) => {
            let text = text.trim();
            let body = if text.starts_with('[') {
                let (_, suffix) = text.split_once('=')?;
                suffix.trim()
            } else {
                text
            };
            parse_quantified_array_literal_body(body)
        }
        _ => None,
    }
}

fn row_contains_quantified_array_like_value(row: &Row) -> bool {
    row.values
        .iter()
        .any(|value| coerce_quantified_array_elements(value).is_some())
}

fn expr_side_usage(expr: &TypedExpr, left_width: usize, total_width: usize) -> (bool, bool) {
    fn visit(
        expr: &TypedExpr,
        left_width: usize,
        total_width: usize,
        has_left: &mut bool,
        has_right: &mut bool,
    ) {
        match &expr.kind {
            TypedExprKind::ColumnRef { ordinal, .. } => {
                if *ordinal < left_width {
                    *has_left = true;
                } else if (left_width..total_width).contains(ordinal) {
                    *has_right = true;
                }
            }
            TypedExprKind::OuterColumnRef { .. }
            | TypedExprKind::Literal(_)
            | TypedExprKind::NextValue { .. }
            | TypedExprKind::ScalarSubquery { .. }
            | TypedExprKind::ArraySubquery { .. }
            | TypedExprKind::ExistsSubquery { .. } => {}
            TypedExprKind::BinaryEq { left, right }
            | TypedExprKind::BinaryNe { left, right }
            | TypedExprKind::BinaryGe { left, right }
            | TypedExprKind::BinaryGt { left, right }
            | TypedExprKind::BinaryLe { left, right }
            | TypedExprKind::BinaryLt { left, right }
            | TypedExprKind::LogicalAnd { left, right }
            | TypedExprKind::LogicalOr { left, right }
            | TypedExprKind::ArithAdd { left, right }
            | TypedExprKind::ArithSub { left, right }
            | TypedExprKind::ArithMul { left, right }
            | TypedExprKind::ArithDiv { left, right }
            | TypedExprKind::ArithMod { left, right }
            | TypedExprKind::Concat { left, right }
            | TypedExprKind::JsonGet { left, right }
            | TypedExprKind::JsonGetText { left, right }
            | TypedExprKind::JsonPathGet { left, right }
            | TypedExprKind::JsonPathGetText { left, right }
            | TypedExprKind::JsonContains { left, right }
            | TypedExprKind::JsonContainedBy { left, right }
            | TypedExprKind::JsonKeyExists { left, right }
            | TypedExprKind::JsonAnyKeyExists { left, right }
            | TypedExprKind::JsonAllKeysExist { left, right }
            | TypedExprKind::ArrayConcat { left, right }
            | TypedExprKind::ArrayContains { left, right }
            | TypedExprKind::ArrayContainedBy { left, right }
            | TypedExprKind::ArrayOverlap { left, right }
            | TypedExprKind::IsDistinctFrom { left, right, .. }
            | TypedExprKind::Nullif { left, right } => {
                visit(left, left_width, total_width, has_left, has_right);
                visit(right, left_width, total_width, has_left, has_right);
            }
            TypedExprKind::LogicalNot { expr }
            | TypedExprKind::Negate { expr }
            | TypedExprKind::IsNull { expr, .. }
            | TypedExprKind::Cast { expr, .. }
            | TypedExprKind::InSubquery { expr, .. } => {
                visit(expr, left_width, total_width, has_left, has_right);
            }
            TypedExprKind::Like { expr, pattern, .. } => {
                visit(expr, left_width, total_width, has_left, has_right);
                visit(pattern, left_width, total_width, has_left, has_right);
            }
            TypedExprKind::InList { expr, list, .. } => {
                visit(expr, left_width, total_width, has_left, has_right);
                for item in list {
                    visit(item, left_width, total_width, has_left, has_right);
                }
            }
            TypedExprKind::Between {
                expr, low, high, ..
            } => {
                visit(expr, left_width, total_width, has_left, has_right);
                visit(low, left_width, total_width, has_left, has_right);
                visit(high, left_width, total_width, has_left, has_right);
            }
            TypedExprKind::CaseWhen {
                conditions,
                results,
                else_result,
            } => {
                for cond in conditions {
                    visit(cond, left_width, total_width, has_left, has_right);
                }
                for result in results {
                    visit(result, left_width, total_width, has_left, has_right);
                }
                if let Some(expr) = else_result {
                    visit(expr, left_width, total_width, has_left, has_right);
                }
            }
            TypedExprKind::Coalesce { args }
            | TypedExprKind::ScalarFunction { args, .. }
            | TypedExprKind::ArrayConstruct { elements: args }
            | TypedExprKind::UserFunction { args, .. } => {
                for arg in args {
                    visit(arg, left_width, total_width, has_left, has_right);
                }
            }
            TypedExprKind::AggCount { expr, filter, .. } => {
                if let Some(expr) = expr {
                    visit(expr, left_width, total_width, has_left, has_right);
                }
                if let Some(filter) = filter {
                    visit(filter, left_width, total_width, has_left, has_right);
                }
            }
            TypedExprKind::AggSum { expr, filter, .. }
            | TypedExprKind::AggAvg { expr, filter, .. }
            | TypedExprKind::AggAnyValue { expr, filter }
            | TypedExprKind::AggMin { expr, filter }
            | TypedExprKind::AggMax { expr, filter }
            | TypedExprKind::AggBoolAnd { expr, filter }
            | TypedExprKind::AggBoolOr { expr, filter }
            | TypedExprKind::AggStddevPop { expr, filter }
            | TypedExprKind::AggStddevSamp { expr, filter }
            | TypedExprKind::AggVarPop { expr, filter }
            | TypedExprKind::AggVarSamp { expr, filter }
            | TypedExprKind::AggArrayAgg { expr, filter, .. } => {
                visit(expr, left_width, total_width, has_left, has_right);
                if let Some(filter) = filter {
                    visit(filter, left_width, total_width, has_left, has_right);
                }
            }
            TypedExprKind::AggStringAgg {
                expr,
                delimiter,
                filter,
                ..
            } => {
                visit(expr, left_width, total_width, has_left, has_right);
                visit(delimiter, left_width, total_width, has_left, has_right);
                if let Some(filter) = filter {
                    visit(filter, left_width, total_width, has_left, has_right);
                }
            }
            TypedExprKind::WindowFunction {
                args,
                partition_by,
                order_by,
                ..
            } => {
                for arg in args {
                    visit(arg, left_width, total_width, has_left, has_right);
                }
                for expr in partition_by {
                    visit(expr, left_width, total_width, has_left, has_right);
                }
                for sort in order_by {
                    visit(&sort.expr, left_width, total_width, has_left, has_right);
                }
            }
        }
    }

    let mut has_left = false;
    let mut has_right = false;
    visit(expr, left_width, total_width, &mut has_left, &mut has_right);
    (has_left, has_right)
}

fn expr_contains_quantified_any(expr: Option<&TypedExpr>) -> bool {
    fn push_expr_children<'a>(expr: &'a TypedExpr, stack: &mut Vec<&'a TypedExpr>) {
        match &expr.kind {
            TypedExprKind::BinaryEq { left, right }
            | TypedExprKind::BinaryNe { left, right }
            | TypedExprKind::BinaryGe { left, right }
            | TypedExprKind::BinaryGt { left, right }
            | TypedExprKind::BinaryLe { left, right }
            | TypedExprKind::BinaryLt { left, right }
            | TypedExprKind::LogicalAnd { left, right }
            | TypedExprKind::LogicalOr { left, right }
            | TypedExprKind::ArithAdd { left, right }
            | TypedExprKind::ArithSub { left, right }
            | TypedExprKind::ArithMul { left, right }
            | TypedExprKind::ArithDiv { left, right }
            | TypedExprKind::ArithMod { left, right }
            | TypedExprKind::Concat { left, right }
            | TypedExprKind::JsonGet { left, right }
            | TypedExprKind::JsonGetText { left, right }
            | TypedExprKind::JsonPathGet { left, right }
            | TypedExprKind::JsonPathGetText { left, right }
            | TypedExprKind::JsonContains { left, right }
            | TypedExprKind::JsonContainedBy { left, right }
            | TypedExprKind::JsonKeyExists { left, right }
            | TypedExprKind::JsonAnyKeyExists { left, right }
            | TypedExprKind::JsonAllKeysExist { left, right }
            | TypedExprKind::ArrayConcat { left, right }
            | TypedExprKind::ArrayContains { left, right }
            | TypedExprKind::ArrayContainedBy { left, right }
            | TypedExprKind::ArrayOverlap { left, right }
            | TypedExprKind::IsDistinctFrom { left, right, .. }
            | TypedExprKind::Nullif { left, right } => {
                stack.push(right);
                stack.push(left);
            }
            TypedExprKind::LogicalNot { expr }
            | TypedExprKind::Negate { expr }
            | TypedExprKind::IsNull { expr, .. }
            | TypedExprKind::Cast { expr, .. }
            | TypedExprKind::InSubquery { expr, .. } => stack.push(expr),
            TypedExprKind::Like { expr, pattern, .. } => {
                stack.push(pattern);
                stack.push(expr);
            }
            TypedExprKind::InList { expr, list, .. } => {
                stack.extend(list);
                stack.push(expr);
            }
            TypedExprKind::Between {
                expr, low, high, ..
            } => {
                stack.push(high);
                stack.push(low);
                stack.push(expr);
            }
            TypedExprKind::CaseWhen {
                conditions,
                results,
                else_result,
            } => {
                if let Some(expr) = else_result {
                    stack.push(expr);
                }
                stack.extend(results);
                stack.extend(conditions);
            }
            TypedExprKind::Coalesce { args }
            | TypedExprKind::ScalarFunction { args, .. }
            | TypedExprKind::ArrayConstruct { elements: args }
            | TypedExprKind::UserFunction { args, .. } => stack.extend(args),
            TypedExprKind::AggCount { expr, filter, .. } => {
                if let Some(expr) = expr {
                    stack.push(expr);
                }
                if let Some(filter) = filter {
                    stack.push(filter);
                }
            }
            TypedExprKind::AggSum { expr, filter, .. }
            | TypedExprKind::AggAvg { expr, filter, .. }
            | TypedExprKind::AggAnyValue { expr, filter }
            | TypedExprKind::AggMin { expr, filter }
            | TypedExprKind::AggMax { expr, filter }
            | TypedExprKind::AggBoolAnd { expr, filter }
            | TypedExprKind::AggBoolOr { expr, filter }
            | TypedExprKind::AggStddevPop { expr, filter }
            | TypedExprKind::AggStddevSamp { expr, filter }
            | TypedExprKind::AggVarPop { expr, filter }
            | TypedExprKind::AggVarSamp { expr, filter }
            | TypedExprKind::AggArrayAgg { expr, filter, .. } => {
                stack.push(expr);
                if let Some(filter) = filter {
                    stack.push(filter);
                }
            }
            TypedExprKind::AggStringAgg {
                expr,
                delimiter,
                filter,
                ..
            } => {
                stack.push(delimiter);
                stack.push(expr);
                if let Some(filter) = filter {
                    stack.push(filter);
                }
            }
            TypedExprKind::WindowFunction {
                args,
                partition_by,
                order_by,
                ..
            } => {
                for sort in order_by {
                    stack.push(&sort.expr);
                }
                stack.extend(partition_by);
                stack.extend(args);
            }
            TypedExprKind::Literal(_)
            | TypedExprKind::ColumnRef { .. }
            | TypedExprKind::OuterColumnRef { .. }
            | TypedExprKind::NextValue { .. }
            | TypedExprKind::ScalarSubquery { .. }
            | TypedExprKind::ArraySubquery { .. }
            | TypedExprKind::ExistsSubquery { .. } => {}
        }
    }

    let Some(expr) = expr else {
        return false;
    };
    let mut stack = vec![expr];
    while let Some(expr) = stack.pop() {
        match &expr.kind {
            TypedExprKind::ScalarFunction { func, args }
                if matches!(
                    func,
                    ScalarFunction::Generic(name) if name.starts_with("__aiondb_quantified_any_")
                ) && args.len() == 2 =>
            {
                return true;
            }
            _ => push_expr_children(expr, &mut stack),
        }
    }
    false
}

fn hash_key_expr_references_only_join_side(
    expr: &TypedExpr,
    left_width: usize,
    total_width: usize,
    left_side: bool,
) -> bool {
    match &expr.kind {
        TypedExprKind::ColumnRef { ordinal, .. } => {
            if left_side {
                *ordinal < left_width
            } else {
                (left_width..total_width).contains(ordinal)
            }
        }
        TypedExprKind::Cast { expr, .. } | TypedExprKind::Negate { expr } => {
            hash_key_expr_references_only_join_side(expr, left_width, total_width, left_side)
        }
        TypedExprKind::ArithAdd { left, right }
        | TypedExprKind::ArithSub { left, right }
        | TypedExprKind::ArithMul { left, right }
        | TypedExprKind::ArithDiv { left, right }
        | TypedExprKind::ArithMod { left, right } => {
            hash_key_expr_references_only_join_side(left, left_width, total_width, left_side)
                && hash_key_expr_references_only_join_side(
                    right,
                    left_width,
                    total_width,
                    left_side,
                )
        }
        TypedExprKind::Literal(_) => true,
        _ => false,
    }
}

fn extract_inner_hash_join_spec(
    condition: Option<&TypedExpr>,
    left_width: usize,
    right_width: usize,
) -> Option<InnerHashJoinSpec> {
    let condition = condition?;
    let total_width = left_width.checked_add(right_width)?;
    let mut spec = InnerHashJoinSpec {
        left_ordinals: Vec::new(),
        right_ordinals: Vec::new(),
    };
    collect_inner_hash_join_clauses(condition, left_width, total_width, &mut spec)?;
    (!spec.left_ordinals.is_empty()).then_some(spec)
}

fn collect_inner_hash_join_clauses(
    expr: &TypedExpr,
    left_width: usize,
    total_width: usize,
    spec: &mut InnerHashJoinSpec,
) -> Option<()> {
    let mut stack = vec![expr];
    while let Some(expr) = stack.pop() {
        match &expr.kind {
            TypedExprKind::LogicalAnd { left, right } => {
                stack.push(right);
                stack.push(left);
            }
            TypedExprKind::BinaryEq { left, right } => {
                let (left_ordinal, right_ordinal) =
                    classify_inner_hash_join_columns(left, right, left_width, total_width)?;
                spec.left_ordinals.push(left_ordinal);
                spec.right_ordinals.push(right_ordinal);
            }
            _ => return None,
        }
    }
    Some(())
}

fn classify_inner_hash_join_columns(
    left: &TypedExpr,
    right: &TypedExpr,
    left_width: usize,
    total_width: usize,
) -> Option<(usize, usize)> {
    if !supports_hash_join_key_equality(left, right) {
        return None;
    }

    let left_ordinal = join_key_column_ordinal(left)?;
    let right_ordinal = join_key_column_ordinal(right)?;

    if left_ordinal < left_width && (left_width..total_width).contains(&right_ordinal) {
        return Some((left_ordinal, right_ordinal - left_width));
    }
    if right_ordinal < left_width && (left_width..total_width).contains(&left_ordinal) {
        return Some((right_ordinal, left_ordinal - left_width));
    }

    None
}

fn supports_hash_join_key_equality(left: &TypedExpr, right: &TypedExpr) -> bool {
    left.data_type == right.data_type
        || (is_exact_numeric_type(&left.data_type) && is_exact_numeric_type(&right.data_type))
}

fn is_exact_numeric_type(data_type: &DataType) -> bool {
    matches!(
        data_type,
        DataType::Int | DataType::BigInt | DataType::Numeric
    )
}

fn join_key_column_ordinal(expr: &TypedExpr) -> Option<usize> {
    match &expr.kind {
        TypedExprKind::ColumnRef { ordinal, .. } => Some(*ordinal),
        TypedExprKind::Cast { expr, target_type }
            if is_hash_safe_join_key_cast(&expr.data_type, target_type) =>
        {
            join_key_column_ordinal(expr)
        }
        _ => None,
    }
}

fn is_hash_safe_join_key_cast(source_type: &DataType, target_type: &DataType) -> bool {
    source_type == target_type
        || (is_exact_numeric_type(source_type) && is_exact_numeric_type(target_type))
}

fn canonical_exact_numeric_hash_text(value: &Value) -> Option<String> {
    let mut text = match value {
        Value::Int(value) => value.to_string(),
        Value::BigInt(value) => value.to_string(),
        Value::Numeric(value) => value.to_string(),
        _ => return None,
    };

    if matches!(text.as_str(), "NaN" | "Infinity" | "-Infinity") {
        return Some(text);
    }

    if let Some(dot_pos) = text.find('.') {
        let mut end = text.len();
        while end > dot_pos + 1 && text.as_bytes()[end - 1] == b'0' {
            end -= 1;
        }
        if end == dot_pos + 1 {
            end -= 1;
        }
        text.truncate(end);
    }

    if text == "-0" {
        text = "0".to_owned();
    }

    Some(text)
}

fn build_join_hash_key_component(value: &Value) -> DbResult<JoinHashKeyComponent> {
    // Integer fast path: covers the dominant `JOIN ON int_col = int_col`
    // pattern without the per-row `to_string` allocation that the text
    // canonical path takes. Cross-type integer equality (Int / BigInt /
    // integer-valued Numeric) is preserved because every variant maps
    // to the same `i128`.
    if let Some(integer) = canonical_exact_integer_hash(value) {
        return Ok(JoinHashKeyComponent::ExactInteger(integer));
    }
    if let Some(text) = canonical_exact_numeric_hash_text(value) {
        return Ok(JoinHashKeyComponent::ExactNumeric(text));
    }
    Ok(JoinHashKeyComponent::Value(build_hash_key(value)?))
}

fn canonical_exact_integer_hash(value: &Value) -> Option<i128> {
    match value {
        Value::Int(value) => Some(i128::from(*value)),
        Value::BigInt(value) => Some(i128::from(*value)),
        Value::Numeric(value) if !value.is_big() => {
            // Detect numerics that are mathematically integer-valued
            // (e.g. `1.00` with coefficient=100, scale=2 is the integer 1).
            // The pre-existing text canonicalization already collapsed
            // these to "1", so to preserve cross-type equality with the
            // `Int(1)` / `BigInt(1)` fast path we must map them to the
            // same `ExactInteger` variant.
            if value.scale == 0 {
                return Some(value.coefficient);
            }
            let scale = value.scale;
            let mut divisor: i128 = 1;
            for _ in 0..scale {
                divisor = divisor.checked_mul(10)?;
            }
            if value.coefficient % divisor == 0 {
                Some(value.coefficient / divisor)
            } else {
                None
            }
        }
        _ => None,
    }
}

pub(crate) fn build_hash_join_key(row: &Row, ordinals: &[usize]) -> DbResult<Option<JoinHashKey>> {
    let mut keys = Vec::with_capacity(ordinals.len());
    for ordinal in ordinals {
        let Some(value) = row.values.get(*ordinal) else {
            return Err(DbError::internal(
                "hash join column ordinal out of bounds for source row",
            ));
        };
        if matches!(value, Value::Null) {
            return Ok(None);
        }
        keys.push(build_join_hash_key_component(value)?);
    }
    Ok(Some(keys))
}

fn build_hash_join_key_into<'a>(
    row: &Row,
    ordinals: &[usize],
    keys: &'a mut JoinHashKey,
) -> DbResult<Option<&'a [JoinHashKeyComponent]>> {
    keys.clear();
    for ordinal in ordinals {
        let Some(value) = row.values.get(*ordinal) else {
            return Err(DbError::internal(
                "hash join column ordinal out of bounds for source row",
            ));
        };
        if matches!(value, Value::Null) {
            keys.clear();
            return Ok(None);
        }
        keys.push(build_join_hash_key_component(value)?);
    }
    Ok(Some(keys.as_slice()))
}

fn conservative_join_hash_index_capacity(row_count: usize, parallel_workers: usize) -> usize {
    if row_count == 0 {
        return 0;
    }

    let worker_scaled = parallel_workers
        .max(1)
        .saturating_mul(JOIN_HASH_INDEX_CAPACITY_PER_WORKER)
        .max(256);

    row_count
        .min(worker_scaled)
        .min(JOIN_HASH_INDEX_CAPACITY_CAP)
}

fn join_hash_index_guard_limit_bytes(context: &ExecutionContext) -> u64 {
    context
        .max_memory_bytes
        .min(JOIN_HASH_INDEX_BUILD_HARD_CAP_BYTES)
}

fn track_join_hash_index_memory(
    context: &ExecutionContext,
    tracked_bytes: &mut u64,
    additional_bytes: u64,
) -> DbResult<()> {
    if additional_bytes == 0 {
        return Ok(());
    }
    let next = tracked_bytes
        .checked_add(additional_bytes)
        .ok_or_else(|| DbError::program_limit("hash join memory accounting overflowed"))?;
    if next > join_hash_index_guard_limit_bytes(context) {
        return Err(DbError::program_limit(
            "hash join build side exceeded in-memory guard",
        ));
    }
    context.track_memory(additional_bytes)?;
    *tracked_bytes = next;
    Ok(())
}

fn estimate_join_hash_key_bytes(key: &JoinHashKey) -> u64 {
    let mut total = std::mem::size_of::<JoinHashKey>() as u64;
    for component in key {
        total = total.saturating_add(estimate_join_hash_key_component_bytes(component));
    }
    total
}

fn estimate_join_hash_key_component_bytes(component: &JoinHashKeyComponent) -> u64 {
    let base = std::mem::size_of::<JoinHashKeyComponent>() as u64;
    match component {
        JoinHashKeyComponent::ExactInteger(_) => base,
        JoinHashKeyComponent::ExactNumeric(text) => base.saturating_add(usize_to_u64(text.len())),
        JoinHashKeyComponent::Value(value) => {
            base.saturating_add(estimate_value_hash_key_bytes(value))
        }
    }
}

fn estimate_value_hash_key_bytes(value: &ValueHashKey) -> u64 {
    match value {
        ValueHashKey::Null => 1,
        ValueHashKey::Int(_) => 4,
        ValueHashKey::BigInt(_) => 8,
        ValueHashKey::Real(_) => 4,
        ValueHashKey::Double(_) => 8,
        ValueHashKey::Numeric(_) => 20,
        ValueHashKey::Money(_) => 8,
        ValueHashKey::Text(text) => usize_to_u64(text.len()),
        ValueHashKey::Boolean(_) => 1,
        ValueHashKey::Blob(bytes) => usize_to_u64(bytes.len()),
        ValueHashKey::Timestamp(_) => 16,
        ValueHashKey::Date(_) => 8,
        ValueHashKey::LargeDate(_) => 12,
        ValueHashKey::Time(_) => 8,
        ValueHashKey::TimeTz(_, _) => 12,
        ValueHashKey::Interval(_) => 16,
        ValueHashKey::Tid(_) => 8,
        ValueHashKey::PgLsn(_) => 8,
        ValueHashKey::MacAddr(_) => 6,
        ValueHashKey::MacAddr8(_) => 8,
        ValueHashKey::Uuid(_) => 16,
        ValueHashKey::TimestampTz(_) => 16,
        ValueHashKey::Jsonb(text) => usize_to_u64(text.len()),
        ValueHashKey::Array(elements) => {
            let mut total = 8u64;
            for element in elements {
                total = total.saturating_add(estimate_value_hash_key_bytes(element));
            }
            total
        }
    }
}

#[inline]
fn insert_join_hash_index_row(
    index: &mut std::collections::HashMap<JoinHashKey, Vec<usize>, JoinFxBuildHasher>,
    key: JoinHashKey,
    row_index: usize,
    context: &ExecutionContext,
    tracked_bytes: &mut u64,
) -> DbResult<()> {
    use std::collections::hash_map::Entry;

    const USIZE_BYTES: u64 = std::mem::size_of::<usize>() as u64;
    const VEC_BYTES: u64 = std::mem::size_of::<Vec<usize>>() as u64;

    match index.entry(key) {
        Entry::Vacant(vacant) => {
            let additional_bytes = estimate_join_hash_key_bytes(vacant.key())
                .saturating_add(JOIN_HASH_INDEX_ENTRY_OVERHEAD_BYTES)
                .saturating_add(VEC_BYTES)
                .saturating_add(USIZE_BYTES);
            track_join_hash_index_memory(context, tracked_bytes, additional_bytes)?;
            vacant.insert(vec![row_index]);
        }
        Entry::Occupied(mut occupied) => {
            let positions = occupied.get_mut();
            if positions.len() == positions.capacity() {
                let additional_slots = positions.capacity().max(1);
                let additional_bytes = usize_to_u64(additional_slots).saturating_mul(USIZE_BYTES);
                track_join_hash_index_memory(context, tracked_bytes, additional_bytes)?;
                positions.reserve(additional_slots);
            }
            positions.push(row_index);
        }
    }
    Ok(())
}

fn validate_equi_join_keys(
    left_keys: &[usize],
    right_keys: &[usize],
    left_width: usize,
    right_width: usize,
) -> DbResult<()> {
    if left_keys.len() != right_keys.len() {
        return Err(DbError::internal(
            "join plan has mismatched equi-join key counts",
        ));
    }
    if let Some(invalid) = left_keys
        .iter()
        .copied()
        .find(|ordinal| *ordinal >= left_width)
    {
        return Err(DbError::internal(format!(
            "join plan left key ordinal {invalid} exceeds child width {left_width}"
        )));
    }
    if let Some(invalid) = right_keys
        .iter()
        .copied()
        .find(|ordinal| *ordinal >= right_width)
    {
        return Err(DbError::internal(format!(
            "join plan right key ordinal {invalid} exceeds child width {right_width}"
        )));
    }
    Ok(())
}

impl Executor {
    pub(crate) fn join_child_width(
        &self,
        child: &PhysicalPlan,
        context: &ExecutionContext,
    ) -> DbResult<usize> {
        match child {
            PhysicalPlan::SeqScan { table_id } => {
                self.compat_row_width_for_table_id(context, *table_id)
            }
            // A ProjectTable with empty outputs is a filtered scan that
            // still produces full rows (including system columns).  This
            // can occur after predicate pushdown converts a SeqScan.
            PhysicalPlan::ProjectTable {
                table_id, outputs, ..
            } if outputs.is_empty() => self.compat_row_width_for_table_id(context, *table_id),
            // A DistributedScan whose outputs were empty is an identity
            // scan distributed across nodes: it produces full table rows
            // even though its own output_fields list is empty.
            PhysicalPlan::DistributedScan {
                table_id, outputs, ..
            } if outputs.is_empty() => self.compat_row_width_for_table_id(context, *table_id),
            PhysicalPlan::NestedLoopJoin {
                left,
                right,
                outputs,
                ..
            }
            | PhysicalPlan::HashJoin {
                left,
                right,
                outputs,
                ..
            }
            | PhysicalPlan::MergeJoin {
                left,
                right,
                outputs,
                ..
            } if outputs.is_empty() => Ok(self
                .join_child_width(left, context)?
                .saturating_add(self.join_child_width(right, context)?)),
            // Parameterized NLJ can also expose child rows directly when
            // `outputs` is empty; account for both sides in that case.
            PhysicalPlan::NestedLoopIndexJoin {
                left,
                right_width,
                outputs,
                ..
            } if outputs.is_empty() => Ok(self
                .join_child_width(left, context)?
                .saturating_add(*right_width)),
            _ => Ok(child.output_fields().len()),
        }
    }

    pub(crate) fn for_each_join_child_row(
        &self,
        child: &PhysicalPlan,
        context: &ExecutionContext,
        f: &mut dyn FnMut(Row) -> DbResult<bool>,
    ) -> DbResult<()> {
        match child {
            PhysicalPlan::SeqScan { table_id } => {
                match self.scan_table_locked(context, *table_id, None) {
                    Ok(mut stream) => {
                        let table = self
                            .catalog_reader
                            .get_table_by_id(context.txn_id, *table_id)?
                            .ok_or_else(|| {
                                DbError::internal(format!(
                                    "table {table_id:?} not found for join seqscan"
                                ))
                            })?;
                        let select_policies = self.compile_compat_rls_policies(
                            &table,
                            super::dml_plans::CompatRlsAction::Select,
                            context,
                        )?;
                        let include_oid_system_column =
                            self.compat_include_oid_system_column_for_table_id(context, *table_id)?;
                        let has_interrupts = context.has_execution_interrupts();
                        let mut row_counter: u32 = 0;
                        while let Some(record) = stream.next()? {
                            if has_interrupts {
                                row_counter = row_counter.wrapping_add(1);
                                if row_counter.trailing_zeros() >= 10 {
                                    context.check_deadline()?;
                                }
                            }
                            if !self.compat_rls_allows_existing_row(
                                select_policies.as_deref(),
                                &record.row,
                                context,
                            )? {
                                continue;
                            }
                            // Move the storage row into the compat-row
                            // builder rather than cloning it: every row
                            // produced here flows straight into the join
                            // pipeline, so the borrow-and-clone variant
                            // would discard the cloned `record.row`
                            // immediately after.
                            let compat_row = self.compat_scan_row_consume(
                                record,
                                include_oid_system_column,
                                Some(*table_id),
                            );
                            if !f(compat_row)? {
                                break;
                            }
                        }
                        Ok(())
                    }
                    Err(error) => {
                        if aiondb_planner::is_virtual_synthetic_relation(table_id.get()) {
                            Ok(())
                        } else {
                            Err(error)
                        }
                    }
                }
            }
            // A ProjectTable with empty outputs is a filtered scan that
            // still produces full rows (including system columns).  This
            // can occur after predicate pushdown converts a SeqScan.
            PhysicalPlan::ProjectTable {
                table_id,
                outputs,
                filter,
                access_path,
                ..
            } if outputs.is_empty() => {
                let table = self
                    .catalog_reader
                    .get_table_by_id(context.txn_id, *table_id)?
                    .ok_or_else(|| {
                        DbError::internal(format!(
                            "table {table_id:?} not found for join projecttable"
                        ))
                    })?;
                let select_policies = self.compile_compat_rls_policies(
                    &table,
                    super::dml_plans::CompatRlsAction::Select,
                    context,
                )?;
                let mut stream =
                    match self.resolve_scan_stream(context, *table_id, access_path, None) {
                        Ok(s) => s,
                        Err(error) => {
                            if aiondb_planner::is_virtual_synthetic_relation(table_id.get()) {
                                return Ok(());
                            }
                            return Err(error);
                        }
                    };
                let filter_requires_special_resolution = filter
                    .as_ref()
                    .is_some_and(super::projection_plans::expr_requires_special_resolution);
                let include_oid_system_column =
                    self.compat_include_oid_system_column_for_table_id(context, *table_id)?;
                let has_interrupts = context.has_execution_interrupts();
                let mut row_counter: u32 = 0;
                while let Some(record) = stream.next()? {
                    if has_interrupts {
                        row_counter = row_counter.wrapping_add(1);
                        if row_counter.trailing_zeros() >= 10 {
                            context.check_deadline()?;
                        }
                    }
                    if !self.compat_rls_allows_existing_row(
                        select_policies.as_deref(),
                        &record.row,
                        context,
                    )? {
                        continue;
                    }
                    let compat_row =
                        self.compat_scan_row(&record, include_oid_system_column, Some(*table_id));
                    if !predicate_matches(filter.as_ref().map(|predicate| {
                        self.evaluate_expr_with_row_prechecked(
                            predicate,
                            &compat_row,
                            context,
                            filter_requires_special_resolution,
                        )
                    }))? {
                        continue;
                    }
                    if !f(compat_row)? {
                        break;
                    }
                }
                Ok(())
            }
            PhysicalPlan::NestedLoopJoin {
                left,
                right,
                join_type,
                condition,
                outputs,
                filter,
                order_by,
                limit,
                offset,
                distinct,
                distinct_on,
            } if outputs.is_empty()
                && order_by.is_empty()
                && limit.is_none()
                && offset.is_none()
                && !distinct
                && distinct_on.is_empty() =>
            {
                let left_width = self.join_child_width(left, context)?;
                let right_width = self.join_child_width(right, context)?;
                self.for_each_join_combined_row(
                    left,
                    right,
                    join_type,
                    condition.as_ref(),
                    filter.as_ref(),
                    left_width,
                    right_width,
                    context,
                    f,
                )
            }
            PhysicalPlan::HashJoin {
                left,
                right,
                join_type,
                left_keys,
                right_keys,
                condition,
                outputs,
                filter,
                order_by,
                limit,
                offset,
                distinct,
                distinct_on,
            } if outputs.is_empty()
                && order_by.is_empty()
                && limit.is_none()
                && offset.is_none()
                && !distinct
                && distinct_on.is_empty() =>
            {
                let left_width = self.join_child_width(left, context)?;
                let right_width = self.join_child_width(right, context)?;
                validate_equi_join_keys(left_keys, right_keys, left_width, right_width)?;
                let build_side =
                    self.materialize_hash_join_build_side(right, right_keys, context)?;
                let right_rows = build_side.rows.as_slice();
                let right_index = &build_side.index;
                let hash_build_ok = build_side.hash_build_ok;
                if !hash_build_ok {
                    let full_condition = rebuild_equi_condition(
                        left_keys,
                        right_keys,
                        left_width,
                        condition.as_ref(),
                    );
                    return self.for_each_join_combined_row(
                        left,
                        right,
                        join_type,
                        full_condition.as_ref(),
                        filter.as_ref(),
                        left_width,
                        right_width,
                        context,
                        f,
                    );
                }
                self.hash_join_for_each_row(
                    left,
                    join_type,
                    left_keys,
                    right_keys,
                    condition.as_ref(),
                    filter.as_ref(),
                    right_rows,
                    right_index,
                    left_width,
                    right_width,
                    context,
                    f,
                )
            }
            PhysicalPlan::NestedLoopIndexJoin {
                left,
                right_table_id,
                right_index_id,
                right_width,
                outer_key_ordinal,
                join_type,
                right_filter,
                residual,
                outputs,
                filter,
                order_by,
                limit,
                offset,
                distinct,
                distinct_on,
            } if outputs.is_empty()
                && order_by.is_empty()
                && limit.is_none()
                && offset.is_none()
                && !distinct
                && distinct_on.is_empty() =>
            {
                self.for_each_nested_loop_index_join_combined_row(
                    left,
                    *right_table_id,
                    *right_index_id,
                    *right_width,
                    *outer_key_ordinal,
                    *join_type,
                    right_filter.as_ref(),
                    residual.as_ref(),
                    filter.as_ref(),
                    context,
                    f,
                )
            }
            _ => {
                let result = self.execute(child, context)?;
                match result {
                    ExecutionResult::Query { rows, .. } => {
                        for row in rows {
                            context.check_deadline()?;
                            if !f(row)? {
                                break;
                            }
                        }
                        Ok(())
                    }
                    _ => Err(DbError::internal(
                        "join child did not return a query result",
                    )),
                }
            }
        }
    }

    /// Materialize a join child plan into a `Vec<Row>`, returning the rows and
    /// the column width (needed to produce NULL padding for outer joins).
    pub(crate) fn materialize_join_child(
        &self,
        child: &PhysicalPlan,
        context: &ExecutionContext,
    ) -> DbResult<(Vec<Row>, usize)> {
        let width = self.join_child_width(child, context)?;
        // Adapt cap to available memory: assume ~100 bytes per row.
        let memory_row_cap = context.max_memory_bytes / 100;
        let row_cap = JOIN_CHILD_MATERIALIZE_ROW_CAP
            .min(context.max_result_rows)
            .min(memory_row_cap);
        // Skip the first ~10 reallocs (Vec doubles 0→1→2→4→…) by starting
        // at a 1024-row hint, capped by row_cap to avoid over-allocating
        // for small joins.
        let initial_cap = clamp_u64_to_usize(row_cap.min(1024), 1024);
        let mut rows = Vec::with_capacity(initial_cap);
        let mut collect_row = |row| {
            if usize_to_u64(rows.len()) >= row_cap {
                return Err(DbError::program_limit(
                    "join child materialization exceeded row cap; consider adding filters or indexes",
                ));
            }
            context.track_memory(estimate_row_bytes(&row))?;
            rows.push(row);
            Ok(true)
        };
        self.for_each_join_child_row(child, context, &mut collect_row)?;
        Ok((rows, width))
    }

    #[allow(clippy::too_many_arguments)]
    fn for_each_nested_loop_index_join_combined_row(
        &self,
        left: &PhysicalPlan,
        right_table_id: RelationId,
        right_index_id: IndexId,
        right_width: usize,
        outer_key_ordinal: usize,
        join_type: JoinType,
        right_filter: Option<&TypedExpr>,
        residual: Option<&TypedExpr>,
        filter: Option<&TypedExpr>,
        context: &ExecutionContext,
        on_row: &mut dyn FnMut(Row) -> DbResult<bool>,
    ) -> DbResult<()> {
        let (left_rows, _) = self.materialize_join_child(left, context)?;
        let null_right = Row::new(vec![Value::Null; right_width]);
        let filter_requires_special_resolution =
            filter.is_some_and(super::projection_plans::expr_requires_special_resolution);
        let include_right_oid_system_column =
            self.compat_include_oid_system_column_for_table_id(context, right_table_id)?;
        let memo_capacity = left_rows.len().min(JOIN_HASH_INDEX_CAPACITY_CAP);
        let mut memo_cache: HashMap<ValueHashKey, Vec<Row>, JoinFxBuildHasher> =
            HashMap::with_capacity_and_hasher(memo_capacity, JoinFxBuildHasher::default());

        for left_row in &left_rows {
            context.check_deadline()?;
            let Some(lookup_value) = left_row.values.get(outer_key_ordinal) else {
                if matches!(join_type, JoinType::Left | JoinType::Anti) {
                    let combined = combine_rows(left_row, &null_right);
                    if self.evaluate_optional_predicate_prechecked(
                        filter,
                        &combined,
                        context,
                        filter_requires_special_resolution,
                    )? && !on_row(combined)?
                    {
                        return Ok(());
                    }
                }
                continue;
            };
            if matches!(lookup_value, Value::Null) {
                if matches!(join_type, JoinType::Left | JoinType::Anti) {
                    let combined = combine_rows(left_row, &null_right);
                    if self.evaluate_optional_predicate_prechecked(
                        filter,
                        &combined,
                        context,
                        filter_requires_special_resolution,
                    )? && !on_row(combined)?
                    {
                        return Ok(());
                    }
                }
                continue;
            }

            let cache_key = build_hash_key(lookup_value)?;
            let right_rows_ref = match memo_cache.entry(cache_key) {
                std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::hash_map::Entry::Vacant(entry) => {
                    let raw_rows = self.fetch_join_index_lookup_rows_cached(
                        context,
                        right_table_id,
                        right_index_id,
                        lookup_value,
                        include_right_oid_system_column,
                    )?;
                    let mut fetched = Vec::with_capacity(raw_rows.len().min(4));
                    for right_row in raw_rows {
                        context.check_deadline()?;
                        if let Some(rf) = right_filter {
                            let val = self.evaluate_expr_with_row(rf, &right_row, context)?;
                            if !matches!(val, Value::Boolean(true)) {
                                continue;
                            }
                        }
                        fetched.push(right_row);
                    }
                    entry.insert(fetched)
                }
            };

            let mut found_match = false;
            for right_row in right_rows_ref.iter() {
                let combined = combine_rows(left_row, right_row);
                if let Some(res) = residual {
                    let val = self.evaluate_expr_with_row(res, &combined, context)?;
                    if !matches!(val, Value::Boolean(true)) {
                        continue;
                    }
                }
                found_match = true;
                match join_type {
                    JoinType::Anti => break,
                    JoinType::Semi => {
                        if self.evaluate_optional_predicate_prechecked(
                            filter,
                            &combined,
                            context,
                            filter_requires_special_resolution,
                        )? && !on_row(combined)?
                        {
                            return Ok(());
                        }
                        break;
                    }
                    _ => {
                        if self.evaluate_optional_predicate_prechecked(
                            filter,
                            &combined,
                            context,
                            filter_requires_special_resolution,
                        )? && !on_row(combined)?
                        {
                            return Ok(());
                        }
                    }
                }
            }

            if !found_match {
                match join_type {
                    JoinType::Left | JoinType::Anti => {
                        let combined = combine_rows(left_row, &null_right);
                        if self.evaluate_optional_predicate_prechecked(
                            filter,
                            &combined,
                            context,
                            filter_requires_special_resolution,
                        )? && !on_row(combined)?
                        {
                            return Ok(());
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn materialize_correlated_join_child(
        &self,
        child: &PhysicalPlan,
        outer_row: &Row,
        context: &ExecutionContext,
    ) -> DbResult<(Vec<Row>, usize)> {
        let substituted = substitute_outer_refs_in_physical_plan(child, outer_row);
        let width = self.join_child_width(&substituted, context)?;
        let row_cap = JOIN_CHILD_MATERIALIZE_ROW_CAP.min(context.max_result_rows);
        let initial_cap = clamp_u64_to_usize(row_cap.min(1024), 1024);
        let mut rows = Vec::with_capacity(initial_cap);
        let mut collect_row = |row| {
            if usize_to_u64(rows.len()) >= row_cap {
                return Err(DbError::program_limit(
                    "join child materialization exceeded row cap; consider adding filters or indexes",
                ));
            }
            context.track_memory(estimate_row_bytes(&row))?;
            rows.push(row);
            Ok(true)
        };
        self.for_each_join_child_row(&substituted, context, &mut collect_row)?;
        Ok((rows, width))
    }

    fn quantified_join_match_order(
        &self,
        condition: Option<&TypedExpr>,
        row: &Row,
        left_width: usize,
        right_width: usize,
        context: &ExecutionContext,
        requires_special_resolution: bool,
    ) -> DbResult<Option<usize>> {
        fn push_expr_children<'a>(expr: &'a TypedExpr, stack: &mut Vec<&'a TypedExpr>) {
            match &expr.kind {
                TypedExprKind::BinaryEq { left, right }
                | TypedExprKind::BinaryNe { left, right }
                | TypedExprKind::BinaryGe { left, right }
                | TypedExprKind::BinaryGt { left, right }
                | TypedExprKind::BinaryLe { left, right }
                | TypedExprKind::BinaryLt { left, right }
                | TypedExprKind::LogicalAnd { left, right }
                | TypedExprKind::LogicalOr { left, right }
                | TypedExprKind::ArithAdd { left, right }
                | TypedExprKind::ArithSub { left, right }
                | TypedExprKind::ArithMul { left, right }
                | TypedExprKind::ArithDiv { left, right }
                | TypedExprKind::ArithMod { left, right }
                | TypedExprKind::Concat { left, right }
                | TypedExprKind::JsonGet { left, right }
                | TypedExprKind::JsonGetText { left, right }
                | TypedExprKind::JsonPathGet { left, right }
                | TypedExprKind::JsonPathGetText { left, right }
                | TypedExprKind::JsonContains { left, right }
                | TypedExprKind::JsonContainedBy { left, right }
                | TypedExprKind::JsonKeyExists { left, right }
                | TypedExprKind::JsonAnyKeyExists { left, right }
                | TypedExprKind::JsonAllKeysExist { left, right }
                | TypedExprKind::ArrayConcat { left, right }
                | TypedExprKind::ArrayContains { left, right }
                | TypedExprKind::ArrayContainedBy { left, right }
                | TypedExprKind::ArrayOverlap { left, right }
                | TypedExprKind::IsDistinctFrom { left, right, .. }
                | TypedExprKind::Nullif { left, right } => {
                    stack.push(right);
                    stack.push(left);
                }
                TypedExprKind::LogicalNot { expr }
                | TypedExprKind::Negate { expr }
                | TypedExprKind::IsNull { expr, .. }
                | TypedExprKind::Cast { expr, .. }
                | TypedExprKind::InSubquery { expr, .. } => stack.push(expr),
                TypedExprKind::Like { expr, pattern, .. } => {
                    stack.push(pattern);
                    stack.push(expr);
                }
                TypedExprKind::InList { expr, list, .. } => {
                    stack.extend(list);
                    stack.push(expr);
                }
                TypedExprKind::Between {
                    expr, low, high, ..
                } => {
                    stack.push(high);
                    stack.push(low);
                    stack.push(expr);
                }
                TypedExprKind::CaseWhen {
                    conditions,
                    results,
                    else_result,
                } => {
                    if let Some(expr) = else_result {
                        stack.push(expr);
                    }
                    stack.extend(results);
                    stack.extend(conditions);
                }
                TypedExprKind::Coalesce { args }
                | TypedExprKind::ScalarFunction { args, .. }
                | TypedExprKind::ArrayConstruct { elements: args }
                | TypedExprKind::UserFunction { args, .. } => stack.extend(args),
                TypedExprKind::AggCount { expr, filter, .. } => {
                    if let Some(expr) = expr {
                        stack.push(expr);
                    }
                    if let Some(filter) = filter {
                        stack.push(filter);
                    }
                }
                TypedExprKind::AggSum { expr, filter, .. }
                | TypedExprKind::AggAvg { expr, filter, .. }
                | TypedExprKind::AggAnyValue { expr, filter }
                | TypedExprKind::AggMin { expr, filter }
                | TypedExprKind::AggMax { expr, filter }
                | TypedExprKind::AggBoolAnd { expr, filter }
                | TypedExprKind::AggBoolOr { expr, filter }
                | TypedExprKind::AggStddevPop { expr, filter }
                | TypedExprKind::AggStddevSamp { expr, filter }
                | TypedExprKind::AggVarPop { expr, filter }
                | TypedExprKind::AggVarSamp { expr, filter }
                | TypedExprKind::AggArrayAgg { expr, filter, .. } => {
                    stack.push(expr);
                    if let Some(filter) = filter {
                        stack.push(filter);
                    }
                }
                TypedExprKind::AggStringAgg {
                    expr,
                    delimiter,
                    filter,
                    ..
                } => {
                    stack.push(delimiter);
                    stack.push(expr);
                    if let Some(filter) = filter {
                        stack.push(filter);
                    }
                }
                TypedExprKind::WindowFunction {
                    args,
                    partition_by,
                    order_by,
                    ..
                } => {
                    for sort in order_by {
                        stack.push(&sort.expr);
                    }
                    stack.extend(partition_by);
                    stack.extend(args);
                }
                TypedExprKind::Literal(_)
                | TypedExprKind::ColumnRef { .. }
                | TypedExprKind::OuterColumnRef { .. }
                | TypedExprKind::NextValue { .. }
                | TypedExprKind::ScalarSubquery { .. }
                | TypedExprKind::ArraySubquery { .. }
                | TypedExprKind::ExistsSubquery { .. } => {}
            }
        }

        let Some(condition) = condition else {
            return Ok(None);
        };
        let total_width = left_width.saturating_add(right_width);
        let mut stack = vec![condition];
        while let Some(expr) = stack.pop() {
            match &expr.kind {
                TypedExprKind::ScalarFunction { func, args }
                    if matches!(
                        func,
                        ScalarFunction::Generic(name)
                            if name.starts_with("__aiondb_quantified_any_")
                    ) && args.len() == 2 =>
                {
                    let (scalar_has_left, scalar_has_right) =
                        expr_side_usage(&args[0], left_width, total_width);
                    let (array_has_left, array_has_right) =
                        expr_side_usage(&args[1], left_width, total_width);
                    let spans_join_sides = (scalar_has_left && array_has_right)
                        || (scalar_has_right && array_has_left);
                    if !spans_join_sides {
                        continue;
                    }
                    let scalar = self.evaluate_expr_with_row_prechecked(
                        &args[0],
                        row,
                        context,
                        requires_special_resolution,
                    )?;
                    let array = self.evaluate_expr_with_row_prechecked(
                        &args[1],
                        row,
                        context,
                        requires_special_resolution,
                    )?;
                    let Some(elements) = coerce_quantified_array_elements(&array) else {
                        continue;
                    };
                    if let Some(index) = elements
                        .iter()
                        .position(|element| values_match_quantified_join_order(&scalar, element))
                    {
                        return Ok(Some(index));
                    }
                }
                _ => push_expr_children(expr, &mut stack),
            }
        }
        Ok(None)
    }

    fn fallback_quantified_join_match_order_from_row(
        &self,
        row: &Row,
        left_width: usize,
        right_width: usize,
    ) -> Option<usize> {
        let right_start = left_width;
        let right_end = right_start
            .saturating_add(right_width)
            .min(row.values.len());
        let mut best: Option<usize> = None;
        for left_value in row.values.iter().take(left_width) {
            if matches!(left_value, Value::Null | Value::Array(_)) {
                continue;
            }
            for right_value in &row.values[right_start..right_end] {
                let Some(elements) = coerce_quantified_array_elements(right_value) else {
                    continue;
                };
                let Some(index) = elements
                    .iter()
                    .position(|element| values_match_quantified_join_order(left_value, element))
                else {
                    continue;
                };
                best = Some(best.map_or(index, |current| current.min(index)));
            }
        }
        for right_value in &row.values[right_start..right_end] {
            if matches!(right_value, Value::Null | Value::Array(_)) {
                continue;
            }
            for left_value in row.values.iter().take(left_width) {
                let Some(elements) = coerce_quantified_array_elements(left_value) else {
                    continue;
                };
                let Some(index) = elements
                    .iter()
                    .position(|element| values_match_quantified_join_order(right_value, element))
                else {
                    continue;
                };
                best = Some(best.map_or(index, |current| current.min(index)));
            }
        }
        best
    }

    /// Hash-probe variant for inner equi-joins where the right side is
    /// materialized and the left side is streamed once.
    fn try_hash_inner_join_matches_streaming_left<F>(
        &self,
        condition: Option<&TypedExpr>,
        left: &PhysicalPlan,
        right_rows: &[Row],
        left_width: usize,
        right_width: usize,
        context: &ExecutionContext,
        mut on_match: F,
    ) -> DbResult<Option<()>>
    where
        F: FnMut(&Row, &Row) -> DbResult<bool>,
    {
        let Some(spec) = extract_inner_hash_join_spec(condition, left_width, right_width) else {
            return Ok(None);
        };

        // Common hot path: single-column equi-join. Avoid building a
        // Vec<JoinHashKeyComponent> per row/probe and hash the scalar key directly.
        if spec.left_ordinals.len() == 1 && spec.right_ordinals.len() == 1 {
            let left_ord = spec.left_ordinals[0];
            let right_ord = spec.right_ordinals[0];
            let mut right_index =
                std::collections::HashMap::<JoinHashKeyComponent, Vec<usize>>::with_capacity(
                    conservative_join_hash_index_capacity(right_rows.len(), 1),
                );
            let mut tracked_hash_bytes = 0u64;
            for (ri, right_row) in right_rows.iter().enumerate() {
                context.check_deadline()?;
                let Some(value) = right_row.values.get(right_ord) else {
                    return Ok(None);
                };
                if matches!(value, Value::Null) {
                    continue;
                }
                let key = match build_join_hash_key_component(value) {
                    Ok(key) => key,
                    Err(_) => return Ok(None),
                };
                match right_index.entry(key) {
                    std::collections::hash_map::Entry::Occupied(mut occupied) => {
                        occupied.get_mut().push(ri);
                        track_join_hash_index_memory(
                            context,
                            &mut tracked_hash_bytes,
                            usize_to_u64(std::mem::size_of::<usize>()),
                        )?;
                    }
                    std::collections::hash_map::Entry::Vacant(vacant) => {
                        let additional_bytes = estimate_join_hash_key_component_bytes(vacant.key())
                            + JOIN_HASH_INDEX_ENTRY_OVERHEAD_BYTES
                            + usize_to_u64(std::mem::size_of::<usize>());
                        track_join_hash_index_memory(
                            context,
                            &mut tracked_hash_bytes,
                            additional_bytes,
                        )?;
                        vacant.insert(vec![ri]);
                    }
                }
            }

            let mut stream_left = |left_row: Row| {
                context.check_deadline()?;
                let Some(value) = left_row.values.get(left_ord) else {
                    return Ok(true);
                };
                if matches!(value, Value::Null) {
                    return Ok(true);
                }
                let key = match build_join_hash_key_component(value) {
                    Ok(key) => key,
                    Err(_) => return Ok(true),
                };
                let Some(candidate_indices) = right_index.get(&key) else {
                    return Ok(true);
                };
                for &ri in candidate_indices {
                    if !on_match(&left_row, &right_rows[ri])? {
                        return Ok(false);
                    }
                }
                Ok(true)
            };
            self.for_each_join_child_row(left, context, &mut stream_left)?;
            return Ok(Some(()));
        }

        let mut right_index = std::collections::HashMap::<JoinHashKey, Vec<usize>, JoinFxBuildHasher>::with_capacity_and_hasher(
            conservative_join_hash_index_capacity(right_rows.len(), 1),
            JoinFxBuildHasher::default(),
        );
        let mut tracked_hash_bytes = 0u64;
        for (ri, right_row) in right_rows.iter().enumerate() {
            context.check_deadline()?;
            let key = match build_hash_join_key(right_row, &spec.right_ordinals) {
                Ok(Some(key)) => key,
                Ok(None) => continue,
                Err(_) => return Ok(None),
            };
            if insert_join_hash_index_row(
                &mut right_index,
                key,
                ri,
                context,
                &mut tracked_hash_bytes,
            )
            .is_err()
            {
                return Ok(None);
            }
        }

        let mut left_key_scratch = JoinHashKey::with_capacity(spec.left_ordinals.len());
        let mut stream_left = |left_row| {
            context.check_deadline()?;
            let Some(left_key) = (match build_hash_join_key_into(
                &left_row,
                &spec.left_ordinals,
                &mut left_key_scratch,
            ) {
                Ok(key) => key,
                Err(err) => return Err(err),
            }) else {
                return Ok(true);
            };
            let Some(candidate_indices) = right_index.get(left_key) else {
                return Ok(true);
            };
            for &ri in candidate_indices {
                if !on_match(&left_row, &right_rows[ri])? {
                    return Ok(false);
                }
            }
            Ok(true)
        };
        self.for_each_join_child_row(left, context, &mut stream_left)?;

        Ok(Some(()))
    }

    fn try_build_full_join_expression_hash_index(
        &self,
        condition: Option<&TypedExpr>,
        left_width: usize,
        right_width: usize,
        right_rows: &[Row],
        context: &ExecutionContext,
    ) -> DbResult<
        Option<(
            FullHashJoinExprSpec,
            std::collections::HashMap<JoinHashKey, Vec<usize>, JoinFxBuildHasher>,
        )>,
    > {
        let Some(spec) = extract_full_hash_join_expr_spec(condition, left_width, right_width)
        else {
            return Ok(None);
        };

        let mut right_index = std::collections::HashMap::<JoinHashKey, Vec<usize>, JoinFxBuildHasher>::with_capacity_and_hasher(
            conservative_join_hash_index_capacity(right_rows.len(), 1),
            JoinFxBuildHasher::default(),
        );
        let mut tracked_hash_bytes = 0u64;
        let null_left = Row::new(vec![Value::Null; left_width]);

        for (ri, right_row) in right_rows.iter().enumerate() {
            context.check_deadline()?;
            let combined = combine_rows(&null_left, right_row);
            let key_value = self
                .evaluator
                .evaluate_with_row(&spec.right_key_expr, &combined)?;
            if matches!(key_value, Value::Null) {
                continue;
            }
            let key = vec![build_join_hash_key_component(&key_value)?];
            if insert_join_hash_index_row(
                &mut right_index,
                key,
                ri,
                context,
                &mut tracked_hash_bytes,
            )
            .is_err()
            {
                return Ok(None);
            }
        }

        Ok(Some((spec, right_index)))
    }

    pub(super) fn execute_join_plan(
        &self,
        plan: &PhysicalPlan,
        context: &ExecutionContext,
    ) -> DbResult<ExecutionResult> {
        match plan {
            PhysicalPlan::NestedLoopJoin {
                left,
                right,
                join_type,
                condition,
                outputs,
                filter,
                order_by,
                limit,
                offset,
                distinct,
                distinct_on,
            } => {
                let plan_limit = limit
                    .as_ref()
                    .map(|e| eval_limit_offset_expr(&self.evaluator, e, "LIMIT"))
                    .transpose()?;
                let effective_limit =
                    effective_collect_limit(plan_limit, context.collect_row_limit);
                context.check_deadline()?;
                if matches!(effective_limit, Some(0)) {
                    return Ok(ExecutionResult::Query {
                        columns: plan.output_fields(),
                        rows: Vec::new(),
                    });
                }

                let left_width = self.join_child_width(left, context)?;
                let right_width = self.join_child_width(right, context)?;

                let has_windows = window_eval::has_window_functions(outputs);
                let has_aggregates =
                    !has_windows && outputs.iter().any(|o| expr_contains_aggregate(&o.expr));

                if outputs.is_empty()
                    && order_by.is_empty()
                    && limit.is_none()
                    && offset.is_none()
                    && !distinct
                    && distinct_on.is_empty()
                {
                    let (rows, _) = self.materialize_join_child(plan, context)?;
                    return Ok(ExecutionResult::Query {
                        columns: Vec::new(),
                        rows,
                    });
                }

                // When aggregate expressions are present in the output
                // but no GROUP BY exists (the join plan never carries
                // GROUP BY - that would be an Aggregate plan), we treat
                // all joined rows as a single group, accumulate the
                // aggregates, finalize, and return one row.
                if has_aggregates {
                    let agg_templates: Vec<AggTemplate> = outputs
                        .iter()
                        .map(|proj| classify_agg_expr(&proj.expr))
                        .collect();
                    let aggregate_filter_requires_special_resolution: Vec<bool> = agg_templates
                        .iter()
                        .map(|template| {
                            template.filter.as_ref().is_some_and(|expr| {
                                super::projection_plans::expr_requires_special_resolution(expr)
                            })
                        })
                        .collect();

                    let mut accumulators: Vec<AggAccumulator> = agg_templates
                        .iter()
                        .map(AggAccumulator::from_template)
                        .collect();

                    self.for_each_join_combined_row(
                        left,
                        right,
                        join_type,
                        condition.as_ref(),
                        filter.as_ref(),
                        left_width,
                        right_width,
                        context,
                        &mut |row| {
                            context.check_deadline()?;
                            for (template_idx, (acc, template)) in accumulators
                                .iter_mut()
                                .zip(agg_templates.iter())
                                .enumerate()
                            {
                                if let Some(ref filter_expr) = template.filter {
                                    let fv = self.evaluate_expr_with_row_prechecked(
                                        filter_expr,
                                        &row,
                                        context,
                                        aggregate_filter_requires_special_resolution[template_idx],
                                    )?;
                                    if !matches!(fv, Value::Boolean(true)) {
                                        continue;
                                    }
                                }
                                self.accumulate_value(acc, template, &row, context)?;
                            }
                            Ok(true)
                        },
                    )?;

                    let mut finalized = Vec::with_capacity(agg_templates.len());
                    for (acc, template) in accumulators.iter().zip(agg_templates.iter()) {
                        finalized.push(finalize_accumulator(
                            acc,
                            template,
                            &self.evaluator,
                            context,
                        )?);
                    }
                    let row = Row::new(finalized);

                    return Ok(ExecutionResult::Query {
                        columns: plan.output_fields(),
                        rows: vec![row],
                    });
                }

                // When window functions are present, collect combined source
                // rows first, then project with window evaluation at the end.
                if has_windows {
                    let mut combined_rows = Vec::with_capacity(clamp_u64_to_usize(
                        context.max_result_rows.min(1024),
                        1024,
                    ));
                    self.for_each_join_combined_row(
                        left,
                        right,
                        join_type,
                        condition.as_ref(),
                        filter.as_ref(),
                        left_width,
                        right_width,
                        context,
                        &mut |row| {
                            if usize_to_u64(combined_rows.len()) >= context.max_result_rows {
                                return Err(DbError::program_limit(
                                    "maximum number of result rows reached",
                                ));
                            }
                            context.track_memory(estimate_row_bytes(&row))?;
                            combined_rows.push(row);
                            Ok(true)
                        },
                    )?;

                    let mut rows =
                        window_eval::evaluate_windows(self, outputs, &combined_rows, context)?;

                    if !order_by.is_empty() {
                        let rebased_order_by =
                            super::projection_plans::rebase_order_by_to_output_ordinals(
                                outputs, order_by,
                            );
                        let sort_col_indices: Vec<Option<usize>> = rebased_order_by
                            .iter()
                            .map(|sort| outputs.iter().position(|output| output.expr == sort.expr))
                            .collect();

                        let sort_error: std::cell::RefCell<Option<DbError>> =
                            std::cell::RefCell::new(None);
                        rows.sort_by(|a, b| {
                            if sort_error.borrow().is_some() {
                                return Ordering::Equal;
                            }
                            if let Err(e) = context.check_deadline() {
                                *sort_error.borrow_mut() = Some(e);
                                return Ordering::Equal;
                            }
                            for (si, sort) in rebased_order_by.iter().enumerate() {
                                let cmp = if let Some(col) = sort_col_indices[si] {
                                    match compare_runtime_values(&a.values[col], &b.values[col]) {
                                        Ok(Some(ord)) => ord,
                                        Ok(None) => Ordering::Equal,
                                        Err(e) => {
                                            *sort_error.borrow_mut() = Some(e);
                                            return Ordering::Equal;
                                        }
                                    }
                                } else {
                                    let la = self.evaluator.evaluate_with_row(&sort.expr, a);
                                    let ra = self.evaluator.evaluate_with_row(&sort.expr, b);
                                    match (la, ra) {
                                        (Ok(l), Ok(r)) => match compare_runtime_values(&l, &r) {
                                            Ok(Some(ord)) => ord,
                                            Ok(None) => Ordering::Equal,
                                            Err(e) => {
                                                *sort_error.borrow_mut() = Some(e);
                                                return Ordering::Equal;
                                            }
                                        },
                                        (Err(e), _) | (_, Err(e)) => {
                                            *sort_error.borrow_mut() = Some(e);
                                            return Ordering::Equal;
                                        }
                                    }
                                };
                                let cmp = if sort.descending { cmp.reverse() } else { cmp };
                                if cmp != Ordering::Equal {
                                    return cmp;
                                }
                            }
                            Ordering::Equal
                        });
                        if let Some(e) = sort_error.into_inner() {
                            return Err(e);
                        }
                    }

                    if *distinct {
                        hash_dedup_rows(&mut rows, context)?;
                    }

                    if !distinct_on.is_empty() {
                        let rebased_distinct_on =
                            super::projection_plans::rebase_distinct_on_to_output_ordinals(
                                outputs,
                                distinct_on,
                            );
                        super::projection_plans::apply_distinct_on(
                            self,
                            &mut rows,
                            &rebased_distinct_on,
                            context,
                        )?;
                    }

                    let offset_val = offset
                        .as_ref()
                        .map(|e| eval_limit_offset_expr(&self.evaluator, e, "OFFSET"))
                        .transpose()?
                        .unwrap_or(0);
                    if offset_val > 0 {
                        let skip = clamp_u64_to_usize(offset_val, rows.len());
                        rows.drain(..skip);
                    }

                    if let Some(limit) = effective_limit {
                        rows.truncate(clamp_u64_to_usize(limit, rows.len()));
                    }

                    return Ok(ExecutionResult::Query {
                        columns: plan.output_fields(),
                        rows,
                    });
                }

                let has_ordering = !order_by.is_empty();
                let offset_val = offset
                    .as_ref()
                    .map(|e| eval_limit_offset_expr(&self.evaluator, e, "OFFSET"))
                    .transpose()?
                    .unwrap_or(0);
                let has_offset = offset_val > 0;
                let correlated_right = physical_plan_contains_outer_refs(right);
                let condition_requires_special_resolution = condition
                    .as_ref()
                    .is_some_and(super::projection_plans::expr_requires_special_resolution);
                let filter_requires_special_resolution = filter
                    .as_ref()
                    .is_some_and(super::projection_plans::expr_requires_special_resolution);
                let quantified_join_requires_stable_match_order =
                    expr_contains_quantified_any(condition.as_ref())
                        || expr_contains_quantified_any(filter.as_ref());
                let output_direct_column_ordinals = Self::projection_column_ordinals(outputs);
                let output_all_direct_columns = output_direct_column_ordinals.is_some();
                let order_requires_special_resolution = order_by.iter().any(|sort| {
                    super::projection_plans::expr_requires_special_resolution(&sort.expr)
                });
                let mut result_rows: Vec<SortedQueryRow> = Vec::new();
                let mut result_bytes = 0u64;
                let empty_sort_keys = std::sync::Arc::new(Vec::new());
                let push_projected_row = |result_rows: &mut Vec<SortedQueryRow>,
                                          result_bytes: &mut u64,
                                          combined: &Row|
                 -> DbResult<bool> {
                    if !has_ordering
                        && !has_offset
                        && effective_limit
                            .is_some_and(|limit| usize_to_u64(result_rows.len()) >= limit)
                    {
                        return Ok(false);
                    }
                    if usize_to_u64(result_rows.len()) >= context.max_result_rows {
                        return Err(DbError::program_limit(
                            "maximum number of result rows reached",
                        ));
                    }

                    let projected_rows = self.project_outputs_expanding_srfs(
                        outputs,
                        output_direct_column_ordinals.as_deref(),
                        combined,
                        context,
                    )?;
                    if has_ordering {
                        let sort_keys = std::sync::Arc::new(self.evaluate_order_keys_prechecked(
                            order_by,
                            combined,
                            context,
                            order_requires_special_resolution,
                        )?);
                        for projected in projected_rows {
                            push_sorted_query_row(
                                result_rows,
                                context,
                                projected,
                                sort_keys.clone(),
                                result_bytes,
                            )?;
                        }
                    } else {
                        // Share the single empty sort-key Arc across
                        // every unordered match instead of cloning the
                        // inner Vec and re-wrapping in a fresh Arc.
                        for projected in projected_rows {
                            push_sorted_query_row(
                                result_rows,
                                context,
                                projected,
                                std::sync::Arc::clone(&empty_sort_keys),
                                result_bytes,
                            )?;
                        }
                    }
                    Ok(has_ordering
                        || has_offset
                        || effective_limit
                            .map_or(true, |limit| usize_to_u64(result_rows.len()) < limit))
                };
                let push_projected_direct_join_row = |result_rows: &mut Vec<SortedQueryRow>,
                                                      result_bytes: &mut u64,
                                                      left_row: &Row,
                                                      right_row: &Row|
                 -> DbResult<bool> {
                    if !has_ordering
                        && !has_offset
                        && effective_limit
                            .is_some_and(|limit| usize_to_u64(result_rows.len()) >= limit)
                    {
                        return Ok(false);
                    }
                    if usize_to_u64(result_rows.len()) >= context.max_result_rows {
                        return Err(DbError::program_limit(
                            "maximum number of result rows reached",
                        ));
                    }
                    let output_ordinals =
                        output_direct_column_ordinals.as_deref().ok_or_else(|| {
                            DbError::internal(
                                "join direct projection fast path missing output ordinals",
                            )
                        })?;
                    let mut projected_values = Vec::with_capacity(output_ordinals.len());
                    for ordinal in output_ordinals {
                        let value = if *ordinal < left_width {
                            left_row.values.get(*ordinal)
                        } else {
                            right_row.values.get(*ordinal - left_width)
                        }
                        .ok_or_else(|| {
                            DbError::internal("join direct projection ordinal out of bounds")
                        })?
                        .clone();
                        projected_values.push(value);
                    }
                    push_sorted_query_row(
                        result_rows,
                        context,
                        Row::new(projected_values),
                        std::sync::Arc::clone(&empty_sort_keys),
                        result_bytes,
                    )?;
                    Ok(has_ordering
                        || has_offset
                        || effective_limit
                            .map_or(true, |limit| usize_to_u64(result_rows.len()) < limit))
                };

                match join_type {
                    JoinType::Inner => {
                        if correlated_right {
                            self.for_each_join_child_row(left, context, &mut |left_row| {
                                if !has_ordering
                                    && !has_offset
                                    && effective_limit.is_some_and(|limit| {
                                        usize_to_u64(result_rows.len()) >= limit
                                    })
                                {
                                    return Ok(false);
                                }
                                let (right_rows, _) = self
                                    .materialize_correlated_join_child(right, &left_row, context)?;
                                for right_row in &right_rows {
                                    context.check_deadline()?;
                                    let combined = combine_rows(&left_row, right_row);
                                    if !self.evaluate_optional_predicate_prechecked(
                                        condition.as_ref(),
                                        &combined,
                                        context,
                                        condition_requires_special_resolution,
                                    )? {
                                        continue;
                                    }
                                    if !self.evaluate_optional_predicate_prechecked(
                                        filter.as_ref(),
                                        &combined,
                                        context,
                                        filter_requires_special_resolution,
                                    )? {
                                        continue;
                                    }

                                    if !push_projected_row(
                                        &mut result_rows,
                                        &mut result_bytes,
                                        &combined,
                                    )? {
                                        return Ok(false);
                                    }
                                }
                                Ok(true)
                            })?;
                        } else {
                            // For cross joins (no condition), iterate with
                            // the right table as the outer loop to match
                            // PostgreSQL's default row ordering.
                            if condition.is_none() && !quantified_join_requires_stable_match_order {
                                let (left_rows, _) = self.materialize_join_child(left, context)?;
                                self.for_each_join_child_row(right, context, &mut |right_row| {
                                    if !has_ordering
                                        && !has_offset
                                        && effective_limit.is_some_and(|limit| {
                                            usize_to_u64(result_rows.len()) >= limit
                                        })
                                    {
                                        return Ok(false);
                                    }
                                    for left_row in &left_rows {
                                        context.check_deadline()?;
                                        let combined = combine_rows(left_row, &right_row);
                                        if !self.evaluate_optional_predicate_prechecked(
                                            filter.as_ref(),
                                            &combined,
                                            context,
                                            filter_requires_special_resolution,
                                        )? {
                                            continue;
                                        }

                                        if !push_projected_row(
                                            &mut result_rows,
                                            &mut result_bytes,
                                            &combined,
                                        )? {
                                            return Ok(false);
                                        }
                                    }
                                    Ok(true)
                                })?;
                            } else {
                                let (right_rows, _) =
                                    self.materialize_join_child(right, context)?;
                                let prefer_right_outer_quantified_order = right_rows
                                    .iter()
                                    .any(row_contains_quantified_array_like_value);
                                if quantified_join_requires_stable_match_order {
                                    self.for_each_join_child_row(left, context, &mut |left_row| {
                                        if !has_ordering
                                            && !has_offset
                                            && effective_limit.is_some_and(|limit| {
                                                usize_to_u64(result_rows.len()) >= limit
                                            })
                                        {
                                            return Ok(false);
                                        }
                                        let mut deferred_matches: Vec<(usize, Row)> = Vec::new();
                                        for right_row in &right_rows {
                                            context.check_deadline()?;
                                            let combined = combine_rows(&left_row, right_row);
                                            if !self.evaluate_optional_predicate_prechecked(
                                                condition.as_ref(),
                                                &combined,
                                                context,
                                                condition_requires_special_resolution,
                                            )? {
                                                continue;
                                            }
                                            if !self.evaluate_optional_predicate_prechecked(
                                                filter.as_ref(),
                                                &combined,
                                                context,
                                                filter_requires_special_resolution,
                                            )? {
                                                continue;
                                            }
                                            let order_idx = self
                                                .quantified_join_match_order(
                                                    condition.as_ref(),
                                                    &combined,
                                                    left_width,
                                                    right_width,
                                                    context,
                                                    condition_requires_special_resolution,
                                                )?
                                                .or(self.quantified_join_match_order(
                                                    filter.as_ref(),
                                                    &combined,
                                                    left_width,
                                                    right_width,
                                                    context,
                                                    filter_requires_special_resolution,
                                                )?)
                                                .or_else(|| {
                                                    self.fallback_quantified_join_match_order_from_row(
                                                        &combined,
                                                        left_width,
                                                        right_width,
                                                    )
                                                });
                                            if let Some(order_idx) = order_idx {
                                                deferred_matches.push((order_idx, combined));
                                            } else {
                                                deferred_matches.push((usize::MAX, combined));
                                            }
                                        }
                                        if !deferred_matches.is_empty() {
                                            deferred_matches.sort_unstable_by(|left, right| {
                                                left.0.cmp(&right.0)
                                            });
                                            for (_, combined) in deferred_matches {
                                                if !push_projected_row(
                                                    &mut result_rows,
                                                    &mut result_bytes,
                                                    &combined,
                                                )? {
                                                    return Ok(false);
                                                }
                                            }
                                        }
                                        Ok(true)
                                    })?;
                                } else if prefer_right_outer_quantified_order {
                                    let (left_rows, _) =
                                        self.materialize_join_child(left, context)?;
                                    for right_row in &right_rows {
                                        if !has_ordering
                                            && !has_offset
                                            && effective_limit.is_some_and(|limit| {
                                                usize_to_u64(result_rows.len()) >= limit
                                            })
                                        {
                                            break;
                                        }
                                        let mut deferred_matches: Vec<(usize, Row)> = Vec::new();
                                        for left_row in &left_rows {
                                            context.check_deadline()?;
                                            let combined = combine_rows(left_row, right_row);
                                            if !self.evaluate_optional_predicate_prechecked(
                                                condition.as_ref(),
                                                &combined,
                                                context,
                                                condition_requires_special_resolution,
                                            )? {
                                                continue;
                                            }
                                            if !self.evaluate_optional_predicate_prechecked(
                                                filter.as_ref(),
                                                &combined,
                                                context,
                                                filter_requires_special_resolution,
                                            )? {
                                                continue;
                                            }
                                            let order_idx = self
                                                .quantified_join_match_order(
                                                    condition.as_ref(),
                                                    &combined,
                                                    left_width,
                                                    right_width,
                                                    context,
                                                    condition_requires_special_resolution,
                                                )?
                                                .or(self.quantified_join_match_order(
                                                    filter.as_ref(),
                                                    &combined,
                                                    left_width,
                                                    right_width,
                                                    context,
                                                    filter_requires_special_resolution,
                                                )?)
                                                .or_else(|| {
                                                    self.fallback_quantified_join_match_order_from_row(
                                                        &combined,
                                                        left_width,
                                                        right_width,
                                                    )
                                                });
                                            if let Some(order_idx) = order_idx {
                                                deferred_matches.push((order_idx, combined));
                                            } else if !push_projected_row(
                                                &mut result_rows,
                                                &mut result_bytes,
                                                &combined,
                                            )? {
                                                break;
                                            }
                                        }
                                        if !deferred_matches.is_empty() {
                                            deferred_matches.sort_unstable_by(|left, right| {
                                                left.0.cmp(&right.0)
                                            });
                                            for (_, combined) in deferred_matches {
                                                if !push_projected_row(
                                                    &mut result_rows,
                                                    &mut result_bytes,
                                                    &combined,
                                                )? {
                                                    break;
                                                }
                                            }
                                        }
                                    }
                                } else if self
                                    .try_hash_inner_join_matches_streaming_left(
                                        condition.as_ref(),
                                        left,
                                        &right_rows,
                                        left_width,
                                        right_width,
                                        context,
                                        |left_row, right_row| {
                                            if filter.is_none()
                                                && !has_ordering
                                                && !has_offset
                                                && output_all_direct_columns
                                            {
                                                return push_projected_direct_join_row(
                                                    &mut result_rows,
                                                    &mut result_bytes,
                                                    left_row,
                                                    right_row,
                                                );
                                            }
                                            let combined = combine_rows(left_row, right_row);
                                            if !self.evaluate_optional_predicate_prechecked(
                                                filter.as_ref(),
                                                &combined,
                                                context,
                                                filter_requires_special_resolution,
                                            )? {
                                                return Ok(true);
                                            }
                                            push_projected_row(
                                                &mut result_rows,
                                                &mut result_bytes,
                                                &combined,
                                            )
                                        },
                                    )?
                                    .is_none()
                                {
                                    self.for_each_join_child_row(left, context, &mut |left_row| {
                                        if !has_ordering
                                            && !has_offset
                                            && effective_limit.is_some_and(|limit| {
                                                usize_to_u64(result_rows.len()) >= limit
                                            })
                                        {
                                            return Ok(false);
                                        }
                                        let mut deferred_matches: Vec<(usize, Row)> = Vec::new();
                                        for right_row in &right_rows {
                                            context.check_deadline()?;
                                            let combined = combine_rows(&left_row, right_row);
                                            if !self.evaluate_optional_predicate_prechecked(
                                                condition.as_ref(),
                                                &combined,
                                                context,
                                                condition_requires_special_resolution,
                                            )? {
                                                continue;
                                            }
                                            if !self.evaluate_optional_predicate_prechecked(
                                                filter.as_ref(),
                                                &combined,
                                                context,
                                                filter_requires_special_resolution,
                                            )? {
                                                continue;
                                            }

                                            let order_idx = self
                                                .quantified_join_match_order(
                                                    condition.as_ref(),
                                                    &combined,
                                                    left_width,
                                                    right_width,
                                                    context,
                                                    condition_requires_special_resolution,
                                                )?
                                                .or(self.quantified_join_match_order(
                                                    filter.as_ref(),
                                                    &combined,
                                                    left_width,
                                                    right_width,
                                                    context,
                                                    filter_requires_special_resolution,
                                                )?)
                                                .or_else(|| {
                                                    self.fallback_quantified_join_match_order_from_row(
                                                        &combined,
                                                        left_width,
                                                        right_width,
                                                    )
                                                });
                                            if let Some(order_idx) = order_idx {
                                                deferred_matches.push((order_idx, combined));
                                            } else if !push_projected_row(
                                                &mut result_rows,
                                                &mut result_bytes,
                                                &combined,
                                            )? {
                                                return Ok(false);
                                            }
                                        }
                                        if !deferred_matches.is_empty() {
                                            deferred_matches
                                                .sort_unstable_by(|left, right| left.0.cmp(&right.0));
                                            for (_, combined) in deferred_matches {
                                                if !push_projected_row(
                                                    &mut result_rows,
                                                    &mut result_bytes,
                                                    &combined,
                                                )? {
                                                    return Ok(false);
                                                }
                                            }
                                        }
                                        Ok(true)
                                    })?;
                                }
                            }
                        }
                    }
                    JoinType::Left => {
                        let cached_right_rows = if correlated_right {
                            None
                        } else {
                            Some(self.materialize_join_child(right, context)?.0)
                        };
                        let null_right = Row::new(vec![Value::Null; right_width]);
                        self.for_each_join_child_row(left, context, &mut |left_row| {
                            if !has_ordering
                                && !has_offset
                                && effective_limit
                                    .is_some_and(|limit| usize_to_u64(result_rows.len()) >= limit)
                            {
                                return Ok(false);
                            }
                            let owned_right_rows;
                            let right_rows = if correlated_right {
                                owned_right_rows = self
                                    .materialize_correlated_join_child(right, &left_row, context)?
                                    .0;
                                &owned_right_rows
                            } else {
                                let Some(cached_rows) = cached_right_rows.as_ref() else {
                                    return Err(DbError::internal(
                                        "cached right rows missing for non-correlated LEFT JOIN",
                                    ));
                                };
                                cached_rows
                            };
                            let mut matched = false;
                            for right_row in right_rows {
                                context.check_deadline()?;
                                let combined = combine_rows(&left_row, right_row);
                                if !self.evaluate_optional_predicate_prechecked(
                                    condition.as_ref(),
                                    &combined,
                                    context,
                                    condition_requires_special_resolution,
                                )? {
                                    continue;
                                }
                                matched = true;

                                if !self.evaluate_optional_predicate_prechecked(
                                    filter.as_ref(),
                                    &combined,
                                    context,
                                    filter_requires_special_resolution,
                                )? {
                                    continue;
                                }

                                if !push_projected_row(
                                    &mut result_rows,
                                    &mut result_bytes,
                                    &combined,
                                )? {
                                    return Ok(false);
                                }
                            }
                            if !matched {
                                let combined = combine_rows(&left_row, &null_right);

                                if !self.evaluate_optional_predicate_prechecked(
                                    filter.as_ref(),
                                    &combined,
                                    context,
                                    filter_requires_special_resolution,
                                )? {
                                    return Ok(true);
                                }

                                if !push_projected_row(
                                    &mut result_rows,
                                    &mut result_bytes,
                                    &combined,
                                )? {
                                    return Ok(false);
                                }
                            }
                            Ok(true)
                        })?;
                    }
                    JoinType::Right => {
                        let (left_rows, _) = self.materialize_join_child(left, context)?;
                        let null_left = Row::new(vec![Value::Null; left_width]);
                        self.for_each_join_child_row(right, context, &mut |right_row| {
                            if !has_ordering
                                && !has_offset
                                && effective_limit
                                    .is_some_and(|limit| usize_to_u64(result_rows.len()) >= limit)
                            {
                                return Ok(false);
                            }
                            let mut matched = false;
                            for left_row in &left_rows {
                                context.check_deadline()?;
                                let combined = combine_rows(left_row, &right_row);
                                if !self.evaluate_optional_predicate_prechecked(
                                    condition.as_ref(),
                                    &combined,
                                    context,
                                    condition_requires_special_resolution,
                                )? {
                                    continue;
                                }
                                matched = true;

                                if !self.evaluate_optional_predicate_prechecked(
                                    filter.as_ref(),
                                    &combined,
                                    context,
                                    filter_requires_special_resolution,
                                )? {
                                    continue;
                                }

                                if !push_projected_row(
                                    &mut result_rows,
                                    &mut result_bytes,
                                    &combined,
                                )? {
                                    return Ok(false);
                                }
                            }
                            if !matched {
                                let combined = combine_rows(&null_left, &right_row);

                                if !self.evaluate_optional_predicate_prechecked(
                                    filter.as_ref(),
                                    &combined,
                                    context,
                                    filter_requires_special_resolution,
                                )? {
                                    return Ok(true);
                                }

                                if !push_projected_row(
                                    &mut result_rows,
                                    &mut result_bytes,
                                    &combined,
                                )? {
                                    return Ok(false);
                                }
                            }
                            Ok(true)
                        })?;
                    }
                    JoinType::Full => {
                        let (right_rows, _) = self.materialize_join_child(right, context)?;
                        let null_left = Row::new(vec![Value::Null; left_width]);
                        let null_right = Row::new(vec![Value::Null; right_width]);
                        if let Some((spec, right_index)) = self
                            .try_build_full_join_expression_hash_index(
                                condition.as_ref(),
                                left_width,
                                right_width,
                                &right_rows,
                                context,
                            )?
                        {
                            let mut right_matched = vec![false; right_rows.len()];
                            let mut stopped = false;
                            self.for_each_join_child_row(left, context, &mut |left_row| {
                                if !has_ordering
                                    && !has_offset
                                    && effective_limit.is_some_and(|limit| {
                                        usize_to_u64(result_rows.len()) >= limit
                                    })
                                {
                                    stopped = true;
                                    return Ok(false);
                                }
                                let left_key = {
                                    let combined = combine_rows(&left_row, &null_right);
                                    let key_value = self
                                        .evaluator
                                        .evaluate_with_row(&spec.left_key_expr, &combined)?;
                                    if matches!(key_value, Value::Null) {
                                        None
                                    } else {
                                        Some(vec![build_join_hash_key_component(&key_value)?])
                                    }
                                };
                                let mut matched = false;
                                if let Some(ref left_key) = left_key {
                                    if let Some(candidate_indices) = right_index.get(left_key) {
                                        for &ri in candidate_indices {
                                            context.check_deadline()?;
                                            let combined = combine_rows(&left_row, &right_rows[ri]);
                                            if !self.evaluate_optional_predicate_prechecked(
                                                condition.as_ref(),
                                                &combined,
                                                context,
                                                condition_requires_special_resolution,
                                            )? {
                                                continue;
                                            }
                                            matched = true;
                                            right_matched[ri] = true;

                                            if !self.evaluate_optional_predicate_prechecked(
                                                filter.as_ref(),
                                                &combined,
                                                context,
                                                filter_requires_special_resolution,
                                            )? {
                                                continue;
                                            }
                                            if !push_projected_row(
                                                &mut result_rows,
                                                &mut result_bytes,
                                                &combined,
                                            )? {
                                                stopped = true;
                                                return Ok(false);
                                            }
                                        }
                                    }
                                }
                                if !matched {
                                    let combined = combine_rows(&left_row, &null_right);
                                    if !self.evaluate_optional_predicate_prechecked(
                                        filter.as_ref(),
                                        &combined,
                                        context,
                                        filter_requires_special_resolution,
                                    )? {
                                        return Ok(true);
                                    }
                                    if !push_projected_row(
                                        &mut result_rows,
                                        &mut result_bytes,
                                        &combined,
                                    )? {
                                        stopped = true;
                                        return Ok(false);
                                    }
                                }
                                Ok(true)
                            })?;

                            if !stopped {
                                for (ri, right_row) in right_rows.iter().enumerate() {
                                    if right_matched[ri] {
                                        continue;
                                    }
                                    let combined = combine_rows(&null_left, right_row);
                                    if !self.evaluate_optional_predicate_prechecked(
                                        filter.as_ref(),
                                        &combined,
                                        context,
                                        filter_requires_special_resolution,
                                    )? {
                                        continue;
                                    }
                                    if !push_projected_row(
                                        &mut result_rows,
                                        &mut result_bytes,
                                        &combined,
                                    )? {
                                        break;
                                    }
                                }
                            }
                        } else {
                            let mut right_matched = vec![false; right_rows.len()];
                            let mut stopped = false;
                            self.for_each_join_child_row(left, context, &mut |left_row| {
                                if !has_ordering
                                    && !has_offset
                                    && effective_limit.is_some_and(|limit| {
                                        usize_to_u64(result_rows.len()) >= limit
                                    })
                                {
                                    stopped = true;
                                    return Ok(false);
                                }
                                let mut matched = false;
                                for (ri, right_row) in right_rows.iter().enumerate() {
                                    context.check_deadline()?;
                                    let combined = combine_rows(&left_row, right_row);
                                    if !self.evaluate_optional_predicate_prechecked(
                                        condition.as_ref(),
                                        &combined,
                                        context,
                                        condition_requires_special_resolution,
                                    )? {
                                        continue;
                                    }
                                    matched = true;
                                    right_matched[ri] = true;

                                    if !self.evaluate_optional_predicate_prechecked(
                                        filter.as_ref(),
                                        &combined,
                                        context,
                                        filter_requires_special_resolution,
                                    )? {
                                        continue;
                                    }

                                    if !push_projected_row(
                                        &mut result_rows,
                                        &mut result_bytes,
                                        &combined,
                                    )? {
                                        stopped = true;
                                        return Ok(false);
                                    }
                                }
                                if !matched {
                                    let combined = combine_rows(&left_row, &null_right);
                                    if !self.evaluate_optional_predicate_prechecked(
                                        filter.as_ref(),
                                        &combined,
                                        context,
                                        filter_requires_special_resolution,
                                    )? {
                                        return Ok(true);
                                    }
                                    if !push_projected_row(
                                        &mut result_rows,
                                        &mut result_bytes,
                                        &combined,
                                    )? {
                                        stopped = true;
                                        return Ok(false);
                                    }
                                }
                                Ok(true)
                            })?;

                            if !stopped {
                                for (ri, right_row) in right_rows.iter().enumerate() {
                                    if right_matched[ri] {
                                        continue;
                                    }
                                    let combined = combine_rows(&null_left, right_row);
                                    if !self.evaluate_optional_predicate_prechecked(
                                        filter.as_ref(),
                                        &combined,
                                        context,
                                        filter_requires_special_resolution,
                                    )? {
                                        continue;
                                    }
                                    if !push_projected_row(
                                        &mut result_rows,
                                        &mut result_bytes,
                                        &combined,
                                    )? {
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    JoinType::Semi => {
                        let (right_rows, _) = self.materialize_join_child(right, context)?;
                        self.for_each_join_child_row(left, context, &mut |left_row| {
                            if !has_ordering
                                && !has_offset
                                && effective_limit
                                    .is_some_and(|limit| usize_to_u64(result_rows.len()) >= limit)
                            {
                                return Ok(false);
                            }
                            for right_row in &right_rows {
                                context.check_deadline()?;
                                let combined = combine_rows(&left_row, right_row);
                                if !self.evaluate_optional_predicate_prechecked(
                                    condition.as_ref(),
                                    &combined,
                                    context,
                                    condition_requires_special_resolution,
                                )? {
                                    continue;
                                }
                                // Match found - emit left row only, move to next.
                                push_projected_row(&mut result_rows, &mut result_bytes, &left_row)?;
                                return Ok(true);
                            }
                            Ok(true)
                        })?;
                    }
                    JoinType::Anti => {
                        let (right_rows, _) = self.materialize_join_child(right, context)?;
                        self.for_each_join_child_row(left, context, &mut |left_row| {
                            if !has_ordering
                                && !has_offset
                                && effective_limit
                                    .is_some_and(|limit| usize_to_u64(result_rows.len()) >= limit)
                            {
                                return Ok(false);
                            }
                            for right_row in &right_rows {
                                context.check_deadline()?;
                                let combined = combine_rows(&left_row, right_row);
                                if self.evaluate_optional_predicate_prechecked(
                                    condition.as_ref(),
                                    &combined,
                                    context,
                                    condition_requires_special_resolution,
                                )? {
                                    // Match found - do NOT emit.
                                    return Ok(true);
                                }
                            }
                            // No match - emit left row.
                            push_projected_row(&mut result_rows, &mut result_bytes, &left_row)?;
                            Ok(true)
                        })?;
                    }
                }

                if has_ordering {
                    if !*distinct && distinct_on.is_empty() {
                        if let Some(bound) = effective_limit
                            .map(|limit| limit.saturating_add(offset_val))
                            .map(|limit| clamp_u64_to_usize(limit, usize::MAX))
                        {
                            sort_query_rows_bounded(&mut result_rows, order_by, bound, context)?;
                        } else {
                            sort_query_rows(&mut result_rows, order_by, context)?;
                        }
                    } else {
                        sort_query_rows(&mut result_rows, order_by, context)?;
                    }
                }

                let mut rows = result_rows
                    .into_iter()
                    .map(|entry| entry.row)
                    .collect::<Vec<_>>();

                if *distinct {
                    hash_dedup_rows(&mut rows, context)?;
                }

                if !distinct_on.is_empty() {
                    let rebased_distinct_on =
                        super::projection_plans::rebase_distinct_on_to_output_ordinals(
                            outputs,
                            distinct_on,
                        );
                    super::projection_plans::apply_distinct_on(
                        self,
                        &mut rows,
                        &rebased_distinct_on,
                        context,
                    )?;
                }

                if offset_val > 0 {
                    let skip = clamp_u64_to_usize(offset_val, rows.len());
                    rows.drain(..skip);
                }

                if let Some(limit) = effective_limit {
                    rows.truncate(clamp_u64_to_usize(limit, rows.len()));
                }

                Ok(ExecutionResult::Query {
                    columns: plan.output_fields(),
                    rows,
                })
            }
            PhysicalPlan::NestedLoopIndexJoin {
                left,
                right_table_id,
                right_index_id,
                right_width,
                outer_key_ordinal,
                join_type,
                right_filter,
                residual,
                outputs,
                filter,
                order_by,
                limit,
                offset,
                distinct,
                distinct_on,
            } => self.execute_nested_loop_index_join(
                left,
                *right_table_id,
                *right_index_id,
                *right_width,
                *outer_key_ordinal,
                *join_type,
                right_filter.as_ref(),
                residual.as_ref(),
                outputs,
                filter.as_ref(),
                order_by,
                limit.as_ref(),
                offset.as_ref(),
                *distinct,
                distinct_on,
                plan,
                context,
            ),
            PhysicalPlan::HashJoin { .. } => self.execute_hash_join_plan(plan, context),
            PhysicalPlan::MergeJoin { .. } => self.execute_merge_join_plan(plan, context),
            _ => Err(DbError::internal("non-join plan routed to join executor")),
        }
    }

    /// Execute a nested-loop join with parameterized inner index scan.
    ///
    /// For each row from the left (outer) child, performs an index lookup
    /// on the right table using the value at `outer_key_ordinal`, then
    /// combines matching rows.  This is O(N * log M) instead of O(N * M).
    fn execute_nested_loop_index_join(
        &self,
        left: &PhysicalPlan,
        right_table_id: RelationId,
        right_index_id: IndexId,
        right_width: usize,
        outer_key_ordinal: usize,
        join_type: JoinType,
        right_filter: Option<&TypedExpr>,
        residual: Option<&TypedExpr>,
        outputs: &[ProjectionExpr],
        filter: Option<&TypedExpr>,
        order_by: &[SortExpr],
        limit: Option<&TypedExpr>,
        offset: Option<&TypedExpr>,
        distinct: bool,
        distinct_on: &[TypedExpr],
        plan: &PhysicalPlan,
        context: &ExecutionContext,
    ) -> DbResult<ExecutionResult> {
        let plan_limit = limit
            .map(|e| eval_limit_offset_expr(&self.evaluator, e, "LIMIT"))
            .transpose()?;
        let effective_limit = effective_collect_limit(plan_limit, context.collect_row_limit);
        if matches!(effective_limit, Some(0)) {
            return Ok(ExecutionResult::Query {
                columns: plan.output_fields(),
                rows: Vec::new(),
            });
        }
        if let Some(result) = self.try_execute_indexed_vector_rerank_join(
            left,
            right_table_id,
            right_index_id,
            outer_key_ordinal,
            join_type,
            right_filter,
            residual,
            outputs,
            filter,
            order_by,
            offset,
            distinct,
            distinct_on,
            effective_limit,
            plan,
            context,
        )? {
            return Ok(result);
        }

        // Materialize the left (outer) side.
        let (left_rows, _) = self.materialize_join_child(left, context)?;
        let _left_width = left_rows.first().map_or(0, |r| r.values.len());

        // Null padding for anti/left joins when no match is found.
        let null_right = Row::new(vec![Value::Null; right_width]);

        let has_ordering = !order_by.is_empty();
        let order_requires_special_resolution = order_by
            .iter()
            .any(|sort| super::projection_plans::expr_requires_special_resolution(&sort.expr));
        let output_direct_column_ordinals = Self::projection_column_ordinals(outputs);
        let mut result_rows: Vec<SortedQueryRow> = Vec::new();
        let mut result_bytes = 0u64;
        let include_right_oid_system_column =
            self.compat_include_oid_system_column_for_table_id(context, right_table_id)?;

        // --- Memoize cache ---
        // Caches filtered right-side rows per lookup value to avoid
        // redundant index lookups when many left rows share the same key.
        // Only the right_filter is applied inside the cache; the residual
        // join condition (which may reference left columns) is evaluated
        // outside.
        let memo_capacity = left_rows.len().min(JOIN_HASH_INDEX_CAPACITY_CAP);
        // FxHash for the parameterized-NL memoize: keys come from
        // internal value hashes, so we can drop SipHash's anti-DoS
        // guarantee for the much faster mix. Mirrors the build-side
        // hash-join change.
        let mut memo_cache: HashMap<ValueHashKey, Vec<Row>, JoinFxBuildHasher> =
            HashMap::with_capacity_and_hasher(memo_capacity, JoinFxBuildHasher::default());

        for left_row in &left_rows {
            context.check_deadline()?;

            // Extract the lookup value from the left row.
            let Some(lookup_value) = left_row.values.get(outer_key_ordinal) else {
                if matches!(join_type, JoinType::Left | JoinType::Anti) {
                    let combined = combine_rows(left_row, &null_right);
                    self.emit_join_row(
                        combined,
                        outputs,
                        filter,
                        order_by,
                        order_requires_special_resolution,
                        output_direct_column_ordinals.as_deref(),
                        has_ordering,
                        &mut result_rows,
                        context,
                        &mut result_bytes,
                    )?;
                }
                continue;
            };

            // Skip NULL keys - they never match in equi-joins.
            if matches!(lookup_value, Value::Null) {
                if matches!(join_type, JoinType::Left | JoinType::Anti) {
                    let combined = combine_rows(left_row, &null_right);
                    self.emit_join_row(
                        combined,
                        outputs,
                        filter,
                        order_by,
                        order_requires_special_resolution,
                        output_direct_column_ordinals.as_deref(),
                        has_ordering,
                        &mut result_rows,
                        context,
                        &mut result_bytes,
                    )?;
                }
                continue;
            }

            // Check memoize cache first.
            let cache_key = build_hash_key(lookup_value)?;
            let right_rows_ref = match memo_cache.entry(cache_key) {
                std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::hash_map::Entry::Vacant(entry) => {
                    let raw_rows = self.fetch_join_index_lookup_rows_cached(
                        context,
                        right_table_id,
                        right_index_id,
                        lookup_value,
                        include_right_oid_system_column,
                    )?;
                    let mut fetched = Vec::with_capacity(raw_rows.len().min(4));
                    for right_row in raw_rows {
                        context.check_deadline()?;
                        // Apply right-side filter per query; it can contain bind parameters.
                        if let Some(rf) = right_filter {
                            let val = self.evaluate_expr_with_row(rf, &right_row, context)?;
                            if !matches!(val, Value::Boolean(true)) {
                                continue;
                            }
                        }
                        fetched.push(right_row);
                    }
                    entry.insert(fetched)
                }
            };

            let mut found_match = false;
            for right_row in right_rows_ref.iter() {
                let combined = combine_rows(left_row, right_row);

                // Apply residual join condition (may reference left columns).
                if let Some(res) = residual {
                    let val = self.evaluate_expr_with_row(res, &combined, context)?;
                    if !matches!(val, Value::Boolean(true)) {
                        continue;
                    }
                }

                found_match = true;

                match join_type {
                    JoinType::Anti => {
                        break;
                    }
                    JoinType::Semi => {
                        self.emit_join_row(
                            combined,
                            outputs,
                            filter,
                            order_by,
                            order_requires_special_resolution,
                            output_direct_column_ordinals.as_deref(),
                            has_ordering,
                            &mut result_rows,
                            context,
                            &mut result_bytes,
                        )?;
                        break;
                    }
                    _ => {
                        self.emit_join_row(
                            combined,
                            outputs,
                            filter,
                            order_by,
                            order_requires_special_resolution,
                            output_direct_column_ordinals.as_deref(),
                            has_ordering,
                            &mut result_rows,
                            context,
                            &mut result_bytes,
                        )?;
                    }
                }
            }

            // For LEFT/ANTI joins: emit null-padded row when no match found.
            if !found_match {
                match join_type {
                    JoinType::Left => {
                        let combined = combine_rows(left_row, &null_right);
                        self.emit_join_row(
                            combined,
                            outputs,
                            filter,
                            order_by,
                            order_requires_special_resolution,
                            output_direct_column_ordinals.as_deref(),
                            has_ordering,
                            &mut result_rows,
                            context,
                            &mut result_bytes,
                        )?;
                    }
                    JoinType::Anti => {
                        let combined = combine_rows(left_row, &null_right);
                        self.emit_join_row(
                            combined,
                            outputs,
                            filter,
                            order_by,
                            order_requires_special_resolution,
                            output_direct_column_ordinals.as_deref(),
                            has_ordering,
                            &mut result_rows,
                            context,
                            &mut result_bytes,
                        )?;
                    }
                    _ => {}
                }
            }
        }

        // Sort, offset, limit, distinct.
        if has_ordering {
            let offset_val = offset
                .map(|expr| eval_limit_offset_expr(&self.evaluator, expr, "OFFSET"))
                .transpose()?
                .unwrap_or(0);
            if !distinct && distinct_on.is_empty() {
                if let Some(bound) = effective_limit
                    .map(|limit| limit.saturating_add(offset_val))
                    .map(|limit| clamp_u64_to_usize(limit, usize::MAX))
                {
                    sort_query_rows_bounded(&mut result_rows, order_by, bound, context)?;
                } else {
                    sort_query_rows(&mut result_rows, order_by, context)?;
                }
            } else {
                sort_query_rows(&mut result_rows, order_by, context)?;
            }
        }
        let mut rows: Vec<Row> = result_rows.into_iter().map(|r| r.row).collect();
        if distinct {
            hash_dedup_rows(&mut rows, context)?;
        }
        if !distinct_on.is_empty() {
            let rebased_distinct_on =
                super::projection_plans::rebase_distinct_on_to_output_ordinals(
                    outputs,
                    distinct_on,
                );
            super::projection_plans::apply_distinct_on(
                self,
                &mut rows,
                &rebased_distinct_on,
                context,
            )?;
        }
        if let Some(off_expr) = offset {
            let off = clamp_u64_to_usize(
                eval_limit_offset_expr(&self.evaluator, off_expr, "OFFSET")?,
                rows.len(),
            );
            if off >= rows.len() {
                rows.clear();
            } else {
                rows = rows.split_off(off);
            }
        }
        if let Some(lim) = effective_limit {
            rows.truncate(clamp_u64_to_usize(lim, rows.len()));
        }

        Ok(ExecutionResult::Query {
            columns: plan.output_fields(),
            rows,
        })
    }

    pub(super) fn fetch_join_index_lookup_rows_cached(
        &self,
        context: &ExecutionContext,
        right_table_id: RelationId,
        right_index_id: IndexId,
        lookup_value: &Value,
        include_right_oid_system_column: bool,
    ) -> DbResult<Vec<Row>> {
        let generation = self.storage_dml.cache_generation();
        let cache_key = generation
            .and_then(|_| build_hash_key(lookup_value).ok())
            .map(|value_key| JoinIndexLookupRowsCacheKey {
                table_id: right_table_id,
                index_id: right_index_id,
                value_key,
                include_oid_system_column: include_right_oid_system_column,
            });
        if let (Some(cache_key), Some(generation)) = (&cache_key, generation) {
            if let Some((cached_generation, rows)) = self
                .join_index_lookup_row_cache
                .read()
                .map_err(|error| {
                    DbError::internal(format!("join index lookup cache poisoned: {error}"))
                })?
                .get(cache_key)
                .cloned()
            {
                if cached_generation == generation {
                    return Ok(rows);
                }
            }
        }

        let key_range = exact_lookup_key_range(lookup_value);
        let mut stream =
            self.scan_index_locked(context, right_table_id, right_index_id, key_range, None)?;
        let mut rows = Vec::with_capacity(4);
        while let Some(record) = stream.next()? {
            context.check_deadline()?;
            rows.push(self.compat_scan_row(
                &record,
                include_right_oid_system_column,
                Some(right_table_id),
            ));
        }

        if let (Some(cache_key), Some(generation)) = (cache_key, generation) {
            let mut cache = self.join_index_lookup_row_cache.write().map_err(|error| {
                DbError::internal(format!("join index lookup cache poisoned: {error}"))
            })?;
            if cache.len() >= 16384 {
                cache.clear();
            }
            cache.insert(cache_key, (generation, rows.clone()));
        }

        Ok(rows)
    }

    /// Helper: project, filter, and collect a single combined join row.
    /// Takes `combined` by value - the SELECT-* / no-projection path can
    /// move it into `result_rows` directly instead of cloning.
    fn emit_join_row(
        &self,
        combined: Row,
        outputs: &[ProjectionExpr],
        filter: Option<&TypedExpr>,
        order_by: &[SortExpr],
        order_requires_special_resolution: bool,
        output_direct_column_ordinals: Option<&[usize]>,
        has_ordering: bool,
        result_rows: &mut Vec<SortedQueryRow>,
        context: &ExecutionContext,
        result_bytes: &mut u64,
    ) -> DbResult<()> {
        // Apply output filter.
        if let Some(f) = filter {
            let val = self.evaluate_expr_with_row(f, &combined, context)?;
            if !matches!(val, Value::Boolean(true)) {
                return Ok(());
            }
        }

        // Compute sort keys against the borrowed row before we may
        // consume it for the no-projection fast path.
        let sort_keys = std::sync::Arc::new(if has_ordering {
            self.evaluate_order_keys_prechecked(
                order_by,
                &combined,
                context,
                order_requires_special_resolution,
            )?
        } else {
            Vec::new()
        });

        // Project output columns.
        let rows = if outputs.is_empty() {
            vec![combined]
        } else {
            self.project_outputs_expanding_srfs(
                outputs,
                output_direct_column_ordinals,
                &combined,
                context,
            )?
        };
        for row in rows {
            push_sorted_query_row(result_rows, context, row, sort_keys.clone(), result_bytes)?;
        }
        Ok(())
    }

    fn try_execute_indexed_vector_rerank_join(
        &self,
        left: &PhysicalPlan,
        right_table_id: RelationId,
        right_index_id: IndexId,
        outer_key_ordinal: usize,
        join_type: JoinType,
        right_filter: Option<&TypedExpr>,
        residual: Option<&TypedExpr>,
        outputs: &[ProjectionExpr],
        filter: Option<&TypedExpr>,
        order_by: &[SortExpr],
        offset: Option<&TypedExpr>,
        distinct: bool,
        distinct_on: &[TypedExpr],
        effective_limit: Option<u64>,
        plan: &PhysicalPlan,
        context: &ExecutionContext,
    ) -> DbResult<Option<ExecutionResult>> {
        let left_width = left.output_fields().len();
        let Some(spec) = self.indexed_vector_rerank_spec(
            outputs,
            order_by,
            left_width,
            join_type,
            right_filter,
            residual,
            filter,
            offset,
            distinct,
            distinct_on,
            effective_limit,
        )?
        else {
            return Ok(None);
        };

        let (left_rows, _) = self.materialize_join_child(left, context)?;
        let mut result_rows = Vec::new();
        let mut result_bytes = 0u64;
        let requested = spec
            .limit
            .map(|limit| clamp_u64_to_usize(limit, usize::MAX));
        let mut projected_ordinals = vec![spec.right_vector_ordinal];
        for mapping in &spec.output_mappings {
            if let IndexedVectorOutputMapping::RightColumn { ordinal } = mapping {
                projected_ordinals.push(*ordinal);
            }
        }
        projected_ordinals.sort_unstable();
        projected_ordinals.dedup();
        let projected_columns =
            self.table_column_ids_for_ordinals(context, right_table_id, &projected_ordinals)?;
        let Some(projected_columns) = projected_columns else {
            return Ok(None);
        };
        let Some(projected_vector_ordinal) = projected_ordinals
            .iter()
            .position(|ordinal| *ordinal == spec.right_vector_ordinal)
        else {
            return Ok(None);
        };

        for left_row in &left_rows {
            context.check_deadline()?;
            let lookup_value = left_row
                .values
                .get(outer_key_ordinal)
                .cloned()
                .unwrap_or(Value::Null);
            if matches!(lookup_value, Value::Null) {
                continue;
            }

            let key_range = exact_lookup_key_range(&lookup_value);
            let mut stream = self.scan_index_locked(
                context,
                right_table_id,
                right_index_id,
                key_range,
                Some(projected_columns.clone()),
            )?;
            while let Some(record) = stream.next()? {
                context.check_deadline()?;
                let values = &record.row.values;
                let Some(Value::Vector(candidate_vector)) = values.get(projected_vector_ordinal)
                else {
                    continue;
                };
                if candidate_vector.values.len() != spec.query_vector.values.len() {
                    return Err(DbError::internal(format!(
                        "l2_distance(): dimension mismatch ({} vs {})",
                        candidate_vector.values.len(),
                        spec.query_vector.values.len()
                    )));
                }
                let distance = aiondb_vector::distance::l2_distance_f64(
                    &candidate_vector.values,
                    &spec.query_vector.values,
                );
                let mut row_values = Vec::with_capacity(spec.output_mappings.len());
                for mapping in &spec.output_mappings {
                    let value = match mapping {
                        IndexedVectorOutputMapping::LeftColumn { ordinal } => left_row
                            .values
                            .get(*ordinal)
                            .cloned()
                            .unwrap_or(Value::Null),
                        IndexedVectorOutputMapping::RightColumn { ordinal } => {
                            let Some(projected_right_ordinal) =
                                projected_ordinals.iter().position(|value| value == ordinal)
                            else {
                                return Ok(None);
                            };
                            values
                                .get(projected_right_ordinal)
                                .cloned()
                                .unwrap_or(Value::Null)
                        }
                        IndexedVectorOutputMapping::Distance => Value::Double(distance),
                    };
                    row_values.push(value);
                }
                let row = Row::new(row_values);
                let sort_keys = std::sync::Arc::new(self.evaluate_order_keys_prechecked(
                    &spec.rebased_order_by,
                    &row,
                    context,
                    false,
                )?);
                push_sorted_query_row(
                    &mut result_rows,
                    context,
                    row,
                    sort_keys,
                    &mut result_bytes,
                )?;
            }
        }

        if let Some(bound) = requested {
            sort_query_rows_bounded(&mut result_rows, &spec.rebased_order_by, bound, context)?;
        } else {
            sort_query_rows(&mut result_rows, &spec.rebased_order_by, context)?;
        }

        Ok(Some(ExecutionResult::Query {
            columns: plan.output_fields(),
            rows: result_rows.into_iter().map(|entry| entry.row).collect(),
        }))
    }

    fn indexed_vector_rerank_spec(
        &self,
        outputs: &[ProjectionExpr],
        order_by: &[SortExpr],
        left_width: usize,
        join_type: JoinType,
        right_filter: Option<&TypedExpr>,
        residual: Option<&TypedExpr>,
        filter: Option<&TypedExpr>,
        offset: Option<&TypedExpr>,
        distinct: bool,
        distinct_on: &[TypedExpr],
        effective_limit: Option<u64>,
    ) -> DbResult<Option<IndexedVectorRerankSpec>> {
        if join_type != JoinType::Inner
            || right_filter.is_some()
            || residual.is_some()
            || filter.is_some()
            || offset.is_some()
            || distinct
            || !distinct_on.is_empty()
            || order_by.is_empty()
            || order_by[0].descending
            || effective_limit.is_none()
        {
            return Ok(None);
        }

        let Some((right_vector_ordinal, query_vector)) =
            l2_distance_right_vector_and_query(&order_by[0].expr, &self.evaluator, left_width)?
        else {
            return Ok(None);
        };

        let rebased_order_by =
            super::projection_plans::rebase_order_by_to_output_ordinals(outputs, order_by);
        if rebased_order_by.iter().any(|sort| {
            !matches!(
                sort.expr.kind,
                TypedExprKind::ColumnRef { .. } | TypedExprKind::Literal(_)
            )
        }) {
            return Ok(None);
        }

        let mut output_mappings = Vec::with_capacity(outputs.len());
        for projection in outputs {
            if projection.expr == order_by[0].expr {
                output_mappings.push(IndexedVectorOutputMapping::Distance);
            } else if let Some(left_ordinal) =
                left_table_ordinal_from_join_expr(&projection.expr, left_width)
            {
                output_mappings.push(IndexedVectorOutputMapping::LeftColumn {
                    ordinal: left_ordinal,
                });
            } else if let Some(right_ordinal) =
                right_table_ordinal_from_join_expr(&projection.expr, left_width)
            {
                output_mappings.push(IndexedVectorOutputMapping::RightColumn {
                    ordinal: right_ordinal,
                });
            } else {
                return Ok(None);
            }
        }

        Ok(Some(IndexedVectorRerankSpec {
            output_mappings,
            rebased_order_by,
            right_vector_ordinal,
            query_vector,
            limit: effective_limit,
        }))
    }
}

fn left_table_ordinal_from_join_expr(expr: &TypedExpr, left_width: usize) -> Option<usize> {
    match &expr.kind {
        TypedExprKind::ColumnRef { ordinal, .. } if *ordinal < left_width => Some(*ordinal),
        _ => None,
    }
}

fn right_table_ordinal_from_join_expr(expr: &TypedExpr, left_width: usize) -> Option<usize> {
    match &expr.kind {
        TypedExprKind::ColumnRef { ordinal, .. } if *ordinal >= left_width => {
            ordinal.checked_sub(left_width)
        }
        _ => None,
    }
}

fn l2_distance_right_vector_and_query(
    expr: &TypedExpr,
    evaluator: &aiondb_eval::ExpressionEvaluator,
    left_width: usize,
) -> DbResult<Option<(usize, aiondb_core::VectorValue)>> {
    let TypedExprKind::ScalarFunction { func, args } = &expr.kind else {
        return Ok(None);
    };
    if *func != ScalarFunction::L2Distance || args.len() != 2 {
        return Ok(None);
    }

    let left_right_ordinal = right_table_ordinal_from_join_expr(&args[0], left_width);
    let right_right_ordinal = right_table_ordinal_from_join_expr(&args[1], left_width);
    match (left_right_ordinal, right_right_ordinal) {
        (Some(vector_ordinal), None) => {
            let Value::Vector(query_vector) = evaluator.evaluate(&args[1])? else {
                return Ok(None);
            };
            Ok(Some((vector_ordinal, query_vector)))
        }
        (None, Some(vector_ordinal)) => {
            let Value::Vector(query_vector) = evaluator.evaluate(&args[0])? else {
                return Ok(None);
            };
            Ok(Some((vector_ordinal, query_vector)))
        }
        _ => Ok(None),
    }
}

/// Rebuild the full equi-join condition from key ordinals plus residual.
fn rebuild_equi_condition(
    left_keys: &[usize],
    right_keys: &[usize],
    left_width: usize,
    residual: Option<&TypedExpr>,
) -> Option<TypedExpr> {
    let mut conjuncts = Vec::new();
    for (lk, rk) in left_keys.iter().zip(right_keys.iter()) {
        conjuncts.push(TypedExpr::binary_eq(
            TypedExpr::column_ref("", *lk, DataType::Int, true),
            TypedExpr::column_ref("", rk.saturating_add(left_width), DataType::Int, true),
        ));
    }
    if let Some(r) = residual {
        conjuncts.push(r.clone());
    }
    let mut iter = conjuncts.into_iter();
    let first = iter.next()?;
    Some(iter.fold(first, TypedExpr::logical_and))
}

fn sort_query_rows_inline(
    executor: &Executor,
    rows: &mut [Row],
    order_by: &[SortExpr],
    context: &ExecutionContext,
) -> DbResult<()> {
    sort_rows_by_exprs(rows, order_by, &executor.evaluator, None, context)
}

fn hash_dedup_rows(rows: &mut Vec<Row>, context: &ExecutionContext) -> DbResult<()> {
    dedup_rows_by_value_hash(rows, context)
}

#[cfg(test)]
mod hash_join_spec_tests;
