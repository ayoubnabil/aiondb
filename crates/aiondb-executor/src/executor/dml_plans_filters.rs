use super::*;

/// Find the longest btree-index leading-prefix that the caller's
/// equality clauses can fully bind. `eq_clauses` maps a column id to
/// the literal it must equal; for every index we walk its key columns
/// in order and accumulate values until we hit a column that is not in
/// the map. Returns the chosen index plus the prefix-aligned literals
/// in the order the index expects (ready to feed
/// `composite_lookup_key_range`). Mirrors PostgreSQL's
/// `match_clause_to_indexable_clause` for the multi-column point
/// lookup shape: equality on a leading prefix -> btree point access.
pub(in super::super) fn best_composite_eq_lookup_index(
    indexes: &[IndexDescriptor],
    eq_clauses: &HashMap<ColumnId, Value>,
) -> Option<(IndexId, Vec<Value>)> {
    if eq_clauses.is_empty() {
        return None;
    }
    let mut best: Option<(IndexId, bool, Vec<Value>)> = None;
    for index in indexes {
        if index.key_columns.is_empty() {
            continue;
        }
        let mut prefix_values: Vec<Value> = Vec::new();
        for idx_col in &index.key_columns {
            if let Some(value) = eq_clauses.get(&idx_col.column_id) {
                prefix_values.push(value.clone());
            } else {
                break;
            }
        }
        if prefix_values.is_empty() {
            continue;
        }
        let candidate = (index.index_id, index.unique, prefix_values);
        match best.as_ref() {
            None => best = Some(candidate),
            Some((_, best_unique, best_prefix)) => {
                let prefer = match candidate.2.len().cmp(&best_prefix.len()) {
                    Ordering::Greater => true,
                    Ordering::Less => false,
                    Ordering::Equal => candidate.1 && !*best_unique,
                };
                if prefer {
                    best = Some(candidate);
                }
            }
        }
    }
    best.map(|(idx, _, vals)| (idx, vals))
}

pub(in super::super) fn best_eq_lookup_index(
    indexes: &[IndexDescriptor],
    column_id: ColumnId,
) -> Option<IndexId> {
    let mut best: Option<(IndexId, bool, usize)> = None;
    for index in indexes {
        let Some(first_key_column) = index.key_columns.first() else {
            continue;
        };
        if first_key_column.column_id != column_id {
            continue;
        }
        let candidate = (index.index_id, index.unique, index.key_columns.len());
        match best {
            None => best = Some(candidate),
            Some((_, best_unique, best_key_len))
                if (candidate.1 && !best_unique)
                    || (candidate.1 == best_unique && candidate.2 < best_key_len) =>
            {
                best = Some(candidate);
            }
            _ => {}
        }
    }
    best.map(|(index_id, _, _)| index_id)
}

pub(in super::super) struct DmlSimpleEqLiteralFilter {
    pub column_ordinal: usize,
    pub literal: Value,
}

pub(in super::super) struct DmlCaseLookupAssignment {
    values_by_case_key: HashMap<ValueHashKey, Value>,
}

impl DmlCaseLookupAssignment {
    pub(in super::super) fn value_for_key(&self, case_key: &ValueHashKey) -> Option<Value> {
        self.values_by_case_key.get(case_key).cloned()
    }
}

fn strip_dml_cast_wrappers(expr: &TypedExpr) -> &TypedExpr {
    let mut current = expr;
    while let TypedExprKind::Cast { expr, .. } = &current.kind {
        current = expr;
    }
    current
}

fn dml_column_ref_ordinal(expr: &TypedExpr) -> Option<usize> {
    let expr = strip_dml_cast_wrappers(expr);
    let TypedExprKind::ColumnRef { ordinal, .. } = &expr.kind else {
        return None;
    };
    Some(*ordinal)
}

fn dml_literal_value(expr: &TypedExpr) -> Option<Value> {
    let expr = strip_dml_cast_wrappers(expr);
    let TypedExprKind::Literal(value) = &expr.kind else {
        return None;
    };
    Some(value.clone())
}

fn dml_binary_eq_literal_for_column(condition: &TypedExpr, column_ordinal: usize) -> Option<Value> {
    let condition = strip_dml_cast_wrappers(condition);
    let TypedExprKind::BinaryEq { left, right } = &condition.kind else {
        return None;
    };
    let left_column = dml_column_ref_ordinal(left);
    let right_column = dml_column_ref_ordinal(right);
    if left_column == Some(column_ordinal) && right_column.is_none() {
        return dml_literal_value(right);
    }
    if right_column == Some(column_ordinal) && left_column.is_none() {
        return dml_literal_value(left);
    }
    None
}

pub(in super::super) fn extract_dml_case_lookup_assignment(
    expr: &TypedExpr,
    case_column_ordinal: usize,
    target_column_ordinal: usize,
) -> Option<DmlCaseLookupAssignment> {
    let expr = strip_dml_cast_wrappers(expr);
    let TypedExprKind::CaseWhen {
        conditions,
        results,
        else_result,
    } = &expr.kind
    else {
        return None;
    };
    if conditions.is_empty() || conditions.len() != results.len() {
        return None;
    }
    let else_result = else_result.as_deref()?;
    if dml_column_ref_ordinal(else_result) != Some(target_column_ordinal) {
        return None;
    }

    let mut values_by_case_key = HashMap::with_capacity(conditions.len());
    for (condition, result) in conditions.iter().zip(results.iter()) {
        let case_value = dml_binary_eq_literal_for_column(condition, case_column_ordinal)?;
        let result_value = dml_literal_value(result)?;
        let Ok(case_key) = build_hash_key(&case_value) else {
            return None;
        };
        values_by_case_key.entry(case_key).or_insert(result_value);
    }
    Some(DmlCaseLookupAssignment { values_by_case_key })
}

pub(in super::super) fn extract_dml_simple_eq_literal_filter(
    filter: &TypedExpr,
) -> Option<DmlSimpleEqLiteralFilter> {
    let filter = strip_dml_cast_wrappers(filter);
    let TypedExprKind::BinaryEq { left, right } = &filter.kind else {
        return None;
    };
    let left = strip_dml_cast_wrappers(left);
    let right = strip_dml_cast_wrappers(right);
    match (&left.kind, &right.kind) {
        (TypedExprKind::ColumnRef { ordinal, .. }, TypedExprKind::Literal(literal))
        | (TypedExprKind::Literal(literal), TypedExprKind::ColumnRef { ordinal, .. }) => {
            Some(DmlSimpleEqLiteralFilter {
                column_ordinal: *ordinal,
                literal: literal.clone(),
            })
        }
        _ => None,
    }
}

fn collect_dml_or_eq_clauses(
    filter: &TypedExpr,
    column: &mut Option<usize>,
    out: &mut Vec<Value>,
) -> bool {
    let filter = strip_dml_cast_wrappers(filter);
    if let TypedExprKind::LogicalOr { left, right } = &filter.kind {
        return collect_dml_or_eq_clauses(left, column, out)
            && collect_dml_or_eq_clauses(right, column, out);
    }
    let Some(eq) = extract_dml_simple_eq_literal_filter(filter) else {
        return false;
    };
    if matches!(eq.literal, Value::Null) {
        return false;
    }
    match column {
        None => *column = Some(eq.column_ordinal),
        Some(existing) if *existing != eq.column_ordinal => return false,
        _ => {}
    }
    out.push(eq.literal);
    true
}

pub(in super::super) fn extract_dml_or_eq_literal_filter(
    filter: &TypedExpr,
) -> Option<(usize, Vec<Value>)> {
    if extract_dml_simple_eq_literal_filter(filter).is_some() {
        return None;
    }
    let mut column: Option<usize> = None;
    let mut literals: Vec<Value> = Vec::new();
    if !collect_dml_or_eq_clauses(filter, &mut column, &mut literals) {
        return None;
    }
    if literals.len() < 2 {
        return None;
    }
    column.map(|ord| (ord, literals))
}

#[allow(dead_code)]
pub(in super::super) struct UpdateFromHashJoinPlan {
    pub target_ordinal: usize,
    pub from_ordinal: usize,
    pub residual: Option<TypedExpr>,
}

fn collect_and_conjuncts<'a>(filter: &'a TypedExpr, out: &mut Vec<&'a TypedExpr>) {
    let stripped = strip_dml_cast_wrappers(filter);
    if let TypedExprKind::LogicalAnd { left, right } = &stripped.kind {
        collect_and_conjuncts(left, out);
        collect_and_conjuncts(right, out);
    } else {
        out.push(filter);
    }
}

fn rebuild_and_conjuncts(parts: Vec<TypedExpr>) -> Option<TypedExpr> {
    let mut iter = parts.into_iter();
    let mut acc = iter.next()?;
    for next in iter {
        acc = TypedExpr {
            kind: TypedExprKind::LogicalAnd {
                left: Box::new(acc),
                right: Box::new(next),
            },
            data_type: aiondb_core::DataType::Boolean,
            nullable: true,
        };
    }
    Some(acc)
}

pub(in super::super) fn extract_update_from_hash_join_plan(
    filter: &TypedExpr,
    target_col_count: usize,
    from_col_count: usize,
) -> Option<UpdateFromHashJoinPlan> {
    let mut conjuncts: Vec<&TypedExpr> = Vec::new();
    collect_and_conjuncts(filter, &mut conjuncts);

    let mut target_ordinal: Option<usize> = None;
    let mut from_ordinal: Option<usize> = None;
    let mut residual: Vec<TypedExpr> = Vec::new();

    for clause in conjuncts {
        if target_ordinal.is_some() {
            residual.push(clause.clone());
            continue;
        }
        let stripped = strip_dml_cast_wrappers(clause);
        let TypedExprKind::BinaryEq { left, right } = &stripped.kind else {
            residual.push(clause.clone());
            continue;
        };
        let left_stripped = strip_dml_cast_wrappers(left);
        let right_stripped = strip_dml_cast_wrappers(right);
        let key_match = match (&left_stripped.kind, &right_stripped.kind) {
            (
                TypedExprKind::ColumnRef { ordinal: lo, .. },
                TypedExprKind::ColumnRef { ordinal: ro, .. },
            ) => {
                if left_stripped.data_type == right_stripped.data_type {
                    let l_target = *lo < target_col_count;
                    let r_target = *ro < target_col_count;
                    if l_target && !r_target {
                        let from_local = ro.checked_sub(target_col_count)?;
                        if from_local < from_col_count {
                            Some((*lo, from_local))
                        } else {
                            None
                        }
                    } else if r_target && !l_target {
                        let from_local = lo.checked_sub(target_col_count)?;
                        if from_local < from_col_count {
                            Some((*ro, from_local))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            _ => None,
        };
        if let Some((t_ord, f_ord)) = key_match {
            target_ordinal = Some(t_ord);
            from_ordinal = Some(f_ord);
        } else {
            residual.push(clause.clone());
        }
    }

    let target_ordinal = target_ordinal?;
    let from_ordinal = from_ordinal?;
    Some(UpdateFromHashJoinPlan {
        target_ordinal,
        from_ordinal,
        residual: rebuild_and_conjuncts(residual),
    })
}

pub(in super::super) fn extract_dml_in_literal_filter(
    filter: &TypedExpr,
) -> Option<(usize, Vec<Value>)> {
    let filter = strip_dml_cast_wrappers(filter);
    let TypedExprKind::InList {
        expr,
        list,
        negated: false,
    } = &filter.kind
    else {
        return None;
    };
    let expr = strip_dml_cast_wrappers(expr);
    let TypedExprKind::ColumnRef { ordinal, .. } = &expr.kind else {
        return None;
    };
    let mut literals = Vec::with_capacity(list.len());
    for element in list {
        let element = strip_dml_cast_wrappers(element);
        let TypedExprKind::Literal(literal) = &element.kind else {
            return None;
        };
        literals.push(literal.clone());
    }
    if literals.is_empty() {
        return None;
    }
    Some((*ordinal, literals))
}

fn collect_dml_composite_eq_clauses(filter: &TypedExpr, out: &mut Vec<(usize, Value)>) -> bool {
    let filter = strip_dml_cast_wrappers(filter);
    if let TypedExprKind::LogicalAnd { left, right } = &filter.kind {
        return collect_dml_composite_eq_clauses(left, out)
            && collect_dml_composite_eq_clauses(right, out);
    }
    if let Some(eq) = extract_dml_simple_eq_literal_filter(filter) {
        if matches!(eq.literal, Value::Null) {
            return false;
        }
        out.push((eq.column_ordinal, eq.literal));
        return true;
    }
    false
}

pub(in super::super) fn extract_dml_composite_eq_literal_filter(
    filter: &TypedExpr,
) -> Option<Vec<(usize, Value)>> {
    let mut clauses: Vec<(usize, Value)> = Vec::new();
    if !collect_dml_composite_eq_clauses(filter, &mut clauses) {
        return None;
    }
    if clauses.len() < 2 {
        return None;
    }
    let mut seen_columns = std::collections::HashSet::new();
    for (ordinal, _) in &clauses {
        if !seen_columns.insert(*ordinal) {
            return None;
        }
    }
    Some(clauses)
}

pub(in super::super) struct DmlRangeBound {
    pub column_ordinal: usize,
    pub lower: aiondb_storage_api::Bound<Value>,
    pub upper: aiondb_storage_api::Bound<Value>,
}

impl DmlRangeBound {
    fn is_definitely_empty(&self) -> bool {
        use aiondb_storage_api::Bound as B;
        let (lower_value, lower_inclusive) = match &self.lower {
            B::Unbounded => return false,
            B::Included(v) => (v, true),
            B::Excluded(v) => (v, false),
        };
        let (upper_value, upper_inclusive) = match &self.upper {
            B::Unbounded => return false,
            B::Included(v) => (v, true),
            B::Excluded(v) => (v, false),
        };
        match compare_runtime_values(lower_value, upper_value) {
            Ok(Some(Ordering::Greater)) => true,
            Ok(Some(Ordering::Equal)) => !(lower_inclusive && upper_inclusive),
            _ => false,
        }
    }

    pub(in super::super) fn to_key_range(&self) -> KeyRange {
        KeyRange {
            lower: lift_value_bound(&self.lower),
            upper: lift_value_bound(&self.upper),
        }
    }
}

fn lift_value_bound(b: &aiondb_storage_api::Bound<Value>) -> aiondb_storage_api::Bound<Vec<Value>> {
    use aiondb_storage_api::Bound as B;
    match b {
        B::Unbounded => B::Unbounded,
        B::Included(v) => B::Included(vec![v.clone()]),
        B::Excluded(v) => B::Excluded(vec![v.clone()]),
    }
}

fn merge_range_lower(
    a: aiondb_storage_api::Bound<Value>,
    b: aiondb_storage_api::Bound<Value>,
) -> aiondb_storage_api::Bound<Value> {
    use aiondb_storage_api::Bound as B;
    match (a, b) {
        (B::Unbounded, other) | (other, B::Unbounded) => other,
        (B::Included(av), B::Included(bv)) => match compare_runtime_values(&av, &bv) {
            Ok(Some(Ordering::Less)) => B::Included(bv),
            _ => B::Included(av),
        },
        (B::Excluded(av), B::Excluded(bv)) => match compare_runtime_values(&av, &bv) {
            Ok(Some(Ordering::Less)) => B::Excluded(bv),
            _ => B::Excluded(av),
        },
        (B::Included(iv), B::Excluded(ev)) | (B::Excluded(ev), B::Included(iv)) => {
            match compare_runtime_values(&iv, &ev) {
                Ok(Some(Ordering::Greater)) => B::Included(iv),
                _ => B::Excluded(ev),
            }
        }
    }
}

fn merge_range_upper(
    a: aiondb_storage_api::Bound<Value>,
    b: aiondb_storage_api::Bound<Value>,
) -> aiondb_storage_api::Bound<Value> {
    use aiondb_storage_api::Bound as B;
    match (a, b) {
        (B::Unbounded, other) | (other, B::Unbounded) => other,
        (B::Included(av), B::Included(bv)) => match compare_runtime_values(&av, &bv) {
            Ok(Some(Ordering::Greater)) => B::Included(bv),
            _ => B::Included(av),
        },
        (B::Excluded(av), B::Excluded(bv)) => match compare_runtime_values(&av, &bv) {
            Ok(Some(Ordering::Greater)) => B::Excluded(bv),
            _ => B::Excluded(av),
        },
        (B::Included(iv), B::Excluded(ev)) | (B::Excluded(ev), B::Included(iv)) => {
            match compare_runtime_values(&iv, &ev) {
                Ok(Some(Ordering::Less)) => B::Included(iv),
                _ => B::Excluded(ev),
            }
        }
    }
}

fn extract_single_range_clause(
    filter: &TypedExpr,
) -> Option<(
    usize,
    aiondb_storage_api::Bound<Value>,
    aiondb_storage_api::Bound<Value>,
)> {
    use aiondb_storage_api::Bound as B;
    let filter = strip_dml_cast_wrappers(filter);
    let (left, right, op_kind) = match &filter.kind {
        TypedExprKind::BinaryGe { left, right } => (left.as_ref(), right.as_ref(), 'g'),
        TypedExprKind::BinaryGt { left, right } => (left.as_ref(), right.as_ref(), 'G'),
        TypedExprKind::BinaryLe { left, right } => (left.as_ref(), right.as_ref(), 'l'),
        TypedExprKind::BinaryLt { left, right } => (left.as_ref(), right.as_ref(), 'L'),
        _ => return None,
    };
    let left_stripped = strip_dml_cast_wrappers(left);
    let right_stripped = strip_dml_cast_wrappers(right);
    let column_ordinal;
    let literal;
    let column_on_left;
    match (&left_stripped.kind, &right_stripped.kind) {
        (TypedExprKind::ColumnRef { ordinal, .. }, TypedExprKind::Literal(value)) => {
            column_ordinal = *ordinal;
            literal = value.clone();
            column_on_left = true;
        }
        (TypedExprKind::Literal(value), TypedExprKind::ColumnRef { ordinal, .. }) => {
            column_ordinal = *ordinal;
            literal = value.clone();
            column_on_left = false;
        }
        _ => return None,
    }
    if matches!(literal, Value::Null) {
        return None;
    }
    let (lower, upper) = match (op_kind, column_on_left) {
        ('g', true) => (B::Included(literal), B::Unbounded),
        ('G', true) => (B::Excluded(literal), B::Unbounded),
        ('l', true) => (B::Unbounded, B::Included(literal)),
        ('L', true) => (B::Unbounded, B::Excluded(literal)),
        ('g', false) => (B::Unbounded, B::Included(literal)),
        ('G', false) => (B::Unbounded, B::Excluded(literal)),
        ('l', false) => (B::Included(literal), B::Unbounded),
        ('L', false) => (B::Excluded(literal), B::Unbounded),
        _ => return None,
    };
    Some((column_ordinal, lower, upper))
}

pub(in super::super) fn extract_dml_range_literal_filter(
    filter: &TypedExpr,
) -> Option<DmlRangeBound> {
    use aiondb_storage_api::Bound as B;
    let filter = strip_dml_cast_wrappers(filter);

    if let TypedExprKind::Between {
        expr,
        low,
        high,
        negated: false,
    } = &filter.kind
    {
        let expr = strip_dml_cast_wrappers(expr);
        let low = strip_dml_cast_wrappers(low);
        let high = strip_dml_cast_wrappers(high);
        if let (
            TypedExprKind::ColumnRef { ordinal, .. },
            TypedExprKind::Literal(low_v),
            TypedExprKind::Literal(high_v),
        ) = (&expr.kind, &low.kind, &high.kind)
        {
            if matches!(low_v, Value::Null) || matches!(high_v, Value::Null) {
                return None;
            }
            let bound = DmlRangeBound {
                column_ordinal: *ordinal,
                lower: B::Included(low_v.clone()),
                upper: B::Included(high_v.clone()),
            };
            return (!bound.is_definitely_empty()).then_some(bound);
        }
        return None;
    }

    if let TypedExprKind::LogicalAnd { left, right } = &filter.kind {
        let lhs = extract_dml_range_literal_filter(left)?;
        let rhs = extract_dml_range_literal_filter(right)?;
        if lhs.column_ordinal != rhs.column_ordinal {
            return None;
        }
        let merged = DmlRangeBound {
            column_ordinal: lhs.column_ordinal,
            lower: merge_range_lower(lhs.lower, rhs.lower),
            upper: merge_range_upper(lhs.upper, rhs.upper),
        };
        return (!merged.is_definitely_empty()).then_some(merged);
    }

    let (column_ordinal, lower, upper) = extract_single_range_clause(filter)?;
    let bound = DmlRangeBound {
        column_ordinal,
        lower,
        upper,
    };
    (!bound.is_definitely_empty()).then_some(bound)
}

pub(in super::super) fn row_matches_dml_range_bound(
    row: &Row,
    bound: &DmlRangeBound,
) -> DbResult<bool> {
    use aiondb_storage_api::Bound as B;
    let Some(value) = row.values.get(bound.column_ordinal) else {
        return Ok(false);
    };
    if matches!(value, Value::Null) {
        return Ok(false);
    }
    let lower_ok = match &bound.lower {
        B::Unbounded => true,
        B::Included(v) => matches!(
            compare_runtime_values(value, v)?,
            Some(Ordering::Greater | Ordering::Equal)
        ),
        B::Excluded(v) => matches!(compare_runtime_values(value, v)?, Some(Ordering::Greater)),
    };
    if !lower_ok {
        return Ok(false);
    }
    let upper_ok = match &bound.upper {
        B::Unbounded => true,
        B::Included(v) => matches!(
            compare_runtime_values(value, v)?,
            Some(Ordering::Less | Ordering::Equal)
        ),
        B::Excluded(v) => matches!(compare_runtime_values(value, v)?, Some(Ordering::Less)),
    };
    Ok(upper_ok)
}

pub(in super::super) fn row_matches_dml_simple_eq_literal_filter(
    row: &Row,
    projected_filter_ordinal: usize,
    literal: &Value,
) -> DbResult<bool> {
    if matches!(literal, Value::Null) {
        return Ok(false);
    }
    let Some(value) = row.values.get(projected_filter_ordinal) else {
        return Ok(false);
    };
    Ok(compare_runtime_values(value, literal)? == Some(Ordering::Equal))
}

pub(in super::super) fn build_dml_literal_key_set(
    literals: &[Value],
) -> Option<std::collections::HashSet<ValueHashKey>> {
    let mut keys = std::collections::HashSet::with_capacity(literals.len());
    for literal in literals {
        if matches!(literal, Value::Null) {
            return None;
        }
        keys.insert(build_hash_key(literal).ok()?);
    }
    Some(keys)
}

pub(in super::super) fn row_matches_dml_literal_key_set(
    row: &Row,
    projected_filter_ordinal: usize,
    literal_keys: &std::collections::HashSet<ValueHashKey>,
) -> bool {
    let Some(value) = row.values.get(projected_filter_ordinal) else {
        return false;
    };
    if matches!(value, Value::Null) {
        return false;
    }
    match build_hash_key(value) {
        Ok(key) => literal_keys.contains(&key),
        Err(_) => false,
    }
}

pub(in super::super) fn value_matches_column_type_exactly(
    value: &Value,
    data_type: &aiondb_core::DataType,
) -> bool {
    use aiondb_core::DataType as D;
    matches!(
        (value, data_type),
        (Value::Null, _)
            | (Value::Int(_), D::Int)
            | (Value::BigInt(_), D::BigInt)
            | (Value::Real(_), D::Real)
            | (Value::Double(_), D::Double)
            | (Value::Boolean(_), D::Boolean)
            | (Value::Date(_), D::Date)
            | (Value::Timestamp(_), D::Timestamp)
            | (Value::TimestampTz(_), D::TimestampTz)
            | (Value::Time(_), D::Time)
            | (Value::Uuid(_), D::Uuid)
            | (Value::Numeric(_), D::Numeric)
    )
}

pub(in super::super) fn enforce_not_null_constraints(
    values: &[Value],
    columns: &[aiondb_plan::ColumnPlan],
    table_name: &str,
) -> DbResult<()> {
    for (value, column) in values.iter().zip(columns.iter()) {
        if !column.nullable && *value == Value::Null {
            let detail = format!(
                "Failing row contains ({}).",
                values
                    .iter()
                    .map(|v| match v {
                        Value::Null => "null".to_owned(),
                        other => format!("{other}"),
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            return Err(DbError::constraint_error(
                SqlState::NotNullViolation,
                format!(
                    "null value in column \"{}\" of relation \"{}\" violates not-null constraint",
                    column.name, table_name
                ),
            )
            .with_client_detail(detail));
        }
    }
    Ok(())
}

pub(in super::super) fn enforce_not_null_constraints_for_table(
    values: &[Value],
    table: &TableDescriptor,
) -> DbResult<()> {
    for (value, column) in values.iter().zip(table.columns.iter()) {
        if !column.nullable && *value == Value::Null {
            let detail = format!(
                "Failing row contains ({}).",
                values
                    .iter()
                    .map(|v| match v {
                        Value::Null => "null".to_owned(),
                        other => format!("{other}"),
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            return Err(DbError::constraint_error(
                SqlState::NotNullViolation,
                format!(
                    "null value in column \"{}\" of relation \"{}\" violates not-null constraint",
                    column.name,
                    table.name.object_name()
                ),
            )
            .with_client_detail(detail));
        }
    }
    Ok(())
}

pub(in super::super) fn updated_not_null_check_ordinals(
    updated_ordinals: &std::collections::HashSet<usize>,
    table: &TableDescriptor,
) -> Vec<usize> {
    updated_ordinals
        .iter()
        .copied()
        .filter(|&ord| {
            table
                .columns
                .get(ord)
                .is_some_and(|column| !column.nullable)
        })
        .collect()
}

pub(in super::super) fn enforce_not_null_constraints_on_updated_columns(
    values: &[Value],
    updated_not_null_ordinals: &[usize],
    table: &TableDescriptor,
) -> DbResult<()> {
    for &ord in updated_not_null_ordinals {
        let Some(value) = values.get(ord) else {
            continue;
        };
        if *value == Value::Null {
            let column_name = table
                .columns
                .get(ord)
                .map_or("?", |column| column.name.as_str());
            let detail = format!(
                "Failing row contains ({}).",
                values
                    .iter()
                    .map(|v| match v {
                        Value::Null => "null".to_owned(),
                        other => format!("{other}"),
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            return Err(DbError::constraint_error(
                SqlState::NotNullViolation,
                format!(
                    "null value in column \"{}\" of relation \"{}\" violates not-null constraint",
                    column_name,
                    table.name.object_name()
                ),
            )
            .with_client_detail(detail));
        }
    }
    Ok(())
}
