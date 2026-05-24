use super::*;

use aiondb_plan::WindowFunctionKind;

use crate::executor::usize_to_i64 as usize_to_i64_saturating;

fn assign_value_to_rows(
    context: &ExecutionContext,
    values: &mut [Value],
    row_indices: &[usize],
    value: Value,
) -> DbResult<()> {
    let Some((last, head)) = row_indices.split_last() else {
        return Ok(());
    };
    for &row_idx in head {
        context.check_deadline()?;
        values[row_idx] = value.clone();
    }
    context.check_deadline()?;
    values[*last] = value;
    Ok(())
}

pub(super) fn compute_window_values_for_partition(
    func: &WindowFunctionKind,
    sorted_indices: &[usize],
    arg_values: &[Vec<Value>],
    sort_keys: &[Vec<Value>],
    order_by: &[aiondb_plan::SortExpr],
    context: &ExecutionContext,
    values: &mut [Value],
) -> DbResult<()> {
    let has_order_by = !order_by.is_empty();

    match func {
        WindowFunctionKind::RowNumber => {
            for (pos, &row_idx) in sorted_indices.iter().enumerate() {
                context.check_deadline()?;
                values[row_idx] = Value::BigInt(usize_to_i64_saturating(pos.saturating_add(1)));
            }
        }
        WindowFunctionKind::Rank => {
            let mut rank = 1i64;
            for (pos, &row_idx) in sorted_indices.iter().enumerate() {
                context.check_deadline()?;
                if pos > 0 {
                    let prev_idx = sorted_indices[pos - 1];
                    if !sort_keys_equal(&sort_keys[row_idx], &sort_keys[prev_idx]) {
                        rank = usize_to_i64_saturating(pos.saturating_add(1));
                    }
                }
                values[row_idx] = Value::BigInt(rank);
            }
        }
        WindowFunctionKind::DenseRank => {
            let mut rank = 1i64;
            for (pos, &row_idx) in sorted_indices.iter().enumerate() {
                context.check_deadline()?;
                if pos > 0 {
                    let prev_idx = sorted_indices[pos - 1];
                    if !sort_keys_equal(&sort_keys[row_idx], &sort_keys[prev_idx]) {
                        rank += 1;
                    }
                }
                values[row_idx] = Value::BigInt(rank);
            }
        }
        WindowFunctionKind::PercentRank => {
            let n = usize_to_f64(sorted_indices.len());
            if n <= 1.0 {
                for &row_idx in sorted_indices {
                    context.check_deadline()?;
                    values[row_idx] = Value::Double(0.0);
                }
            } else {
                let mut rank = 1usize;
                for (pos, &row_idx) in sorted_indices.iter().enumerate() {
                    context.check_deadline()?;
                    if pos > 0 {
                        let prev_idx = sorted_indices[pos - 1];
                        if !sort_keys_equal(&sort_keys[row_idx], &sort_keys[prev_idx]) {
                            rank = pos + 1;
                        }
                    }
                    values[row_idx] =
                        Value::Double(usize_to_f64(rank.saturating_sub(1)) / (n - 1.0));
                }
            }
        }
        WindowFunctionKind::CumeDist => {
            let n = usize_to_f64(sorted_indices.len());
            let peer_groups = compute_peer_groups(sorted_indices, sort_keys, true);
            for (start, end) in &peer_groups {
                context.check_deadline()?;
                let dist = usize_to_f64(*end) / n;
                for pos in *start..*end {
                    context.check_deadline()?;
                    values[sorted_indices[pos]] = Value::Double(dist);
                }
            }
        }
        WindowFunctionKind::Ntile => {
            let partition_len = sorted_indices.len();
            for (pos, &row_idx) in sorted_indices.iter().enumerate() {
                context.check_deadline()?;
                let arg = &arg_values[row_idx][0];
                if arg.is_null() {
                    values[row_idx] = Value::Null;
                    continue;
                }
                let bucket_count = positive_window_usize(arg, "ntile", "bucket count")?;
                let bucket = ntile_bucket_for_row(pos, partition_len, bucket_count);
                values[row_idx] = Value::BigInt(usize_to_i64_saturating(bucket));
            }
        }
        WindowFunctionKind::Lag => {
            for (pos, &row_idx) in sorted_indices.iter().enumerate() {
                context.check_deadline()?;
                let offset = window_offset(&arg_values[row_idx], "lag")?;
                values[row_idx] = offset_window_value(
                    sorted_indices,
                    arg_values,
                    pos.checked_sub(offset),
                    row_idx,
                );
            }
        }
        WindowFunctionKind::Lead => {
            for (pos, &row_idx) in sorted_indices.iter().enumerate() {
                context.check_deadline()?;
                let offset = window_offset(&arg_values[row_idx], "lead")?;
                values[row_idx] = offset_window_value(
                    sorted_indices,
                    arg_values,
                    pos.checked_add(offset),
                    row_idx,
                );
            }
        }
        WindowFunctionKind::FirstValue => {
            let result = sorted_indices
                .first()
                .map_or(Value::Null, |&row_idx| arg_values[row_idx][0].clone());
            assign_value_to_rows(context, values, sorted_indices, result)?;
        }
        WindowFunctionKind::LastValue => {
            if has_order_by {
                let peer_groups = compute_peer_groups(sorted_indices, sort_keys, true);
                for (start, end) in &peer_groups {
                    context.check_deadline()?;
                    let last_idx = sorted_indices[end - 1];
                    let result = arg_values[last_idx][0].clone();
                    assign_value_to_rows(context, values, &sorted_indices[*start..*end], result)?;
                }
            } else {
                let result = sorted_indices
                    .last()
                    .map_or(Value::Null, |&row_idx| arg_values[row_idx][0].clone());
                assign_value_to_rows(context, values, sorted_indices, result)?;
            }
        }
        WindowFunctionKind::Count => {
            let is_star = arg_values.is_empty() || arg_values.first().is_some_and(|v| v.is_empty());
            if has_order_by {
                let peer_groups = compute_peer_groups(sorted_indices, sort_keys, true);
                let mut running = 0i64;
                for (start, end) in &peer_groups {
                    context.check_deadline()?;
                    for &row_idx in &sorted_indices[*start..*end] {
                        context.check_deadline()?;
                        if is_star || !arg_values[row_idx][0].is_null() {
                            running = running.saturating_add(1);
                        }
                    }
                    let count_val = Value::BigInt(running);
                    assign_value_to_rows(
                        context,
                        values,
                        &sorted_indices[*start..*end],
                        count_val,
                    )?;
                }
            } else {
                let count = if is_star {
                    usize_to_i64_saturating(sorted_indices.len())
                } else {
                    sorted_indices
                        .iter()
                        .filter(|&&idx| !arg_values[idx][0].is_null())
                        .count()
                        .try_into()
                        .unwrap_or(i64::MAX)
                };
                let count_val = Value::BigInt(count);
                assign_value_to_rows(context, values, sorted_indices, count_val)?;
            }
        }
        WindowFunctionKind::Sum => {
            if has_order_by {
                let peer_groups = compute_peer_groups(sorted_indices, sort_keys, true);
                let mut running: Option<Value> = None;
                for (start, end) in &peer_groups {
                    context.check_deadline()?;
                    for &row_idx in &sorted_indices[*start..*end] {
                        context.check_deadline()?;
                        let val = &arg_values[row_idx][0];
                        if !val.is_null() {
                            running = Some(agg_add_value(running, val)?);
                        }
                    }
                    let result = running.clone().unwrap_or(Value::Null);
                    assign_value_to_rows(context, values, &sorted_indices[*start..*end], result)?;
                }
            } else {
                let mut sum: Option<Value> = None;
                for &row_idx in sorted_indices {
                    context.check_deadline()?;
                    let val = &arg_values[row_idx][0];
                    if !val.is_null() {
                        sum = Some(agg_add_value(sum, val)?);
                    }
                }
                let result = sum.unwrap_or(Value::Null);
                assign_value_to_rows(context, values, sorted_indices, result)?;
            }
        }
        WindowFunctionKind::Avg => {
            if has_order_by {
                let peer_groups = compute_peer_groups(sorted_indices, sort_keys, true);
                let mut running_sum: Option<Value> = None;
                let mut running_count = 0i64;
                for (start, end) in &peer_groups {
                    context.check_deadline()?;
                    for &row_idx in &sorted_indices[*start..*end] {
                        context.check_deadline()?;
                        let val = &arg_values[row_idx][0];
                        if !val.is_null() {
                            running_sum = Some(agg_add_value(running_sum, val)?);
                            running_count = running_count.saturating_add(1);
                        }
                    }
                    let result = if running_count == 0 {
                        Value::Null
                    } else {
                        let s = value_to_double(running_sum.as_ref().unwrap_or(&Value::Null))?;
                        Value::Double(s / i64_to_f64(running_count))
                    };
                    assign_value_to_rows(context, values, &sorted_indices[*start..*end], result)?;
                }
            } else {
                let mut sum: Option<Value> = None;
                let mut count = 0i64;
                for &row_idx in sorted_indices {
                    context.check_deadline()?;
                    let val = &arg_values[row_idx][0];
                    if !val.is_null() {
                        sum = Some(agg_add_value(sum, val)?);
                        count = count.saturating_add(1);
                    }
                }
                let result = if count == 0 {
                    Value::Null
                } else {
                    let s = value_to_double(&sum.unwrap_or(Value::Null))?;
                    Value::Double(s / i64_to_f64(count))
                };
                assign_value_to_rows(context, values, sorted_indices, result)?;
            }
        }
        WindowFunctionKind::Min => {
            if has_order_by {
                let peer_groups = compute_peer_groups(sorted_indices, sort_keys, true);
                let mut running_min: Option<Value> = None;
                for (start, end) in &peer_groups {
                    context.check_deadline()?;
                    for &row_idx in &sorted_indices[*start..*end] {
                        context.check_deadline()?;
                        let val = &arg_values[row_idx][0];
                        if !val.is_null() {
                            running_min = Some(match running_min {
                                None => val.clone(),
                                Some(cur) => {
                                    if compare_runtime_values(val, &cur)?.unwrap_or(Ordering::Equal)
                                        == Ordering::Less
                                    {
                                        val.clone()
                                    } else {
                                        cur
                                    }
                                }
                            });
                        }
                    }
                    let result = running_min.clone().unwrap_or(Value::Null);
                    assign_value_to_rows(context, values, &sorted_indices[*start..*end], result)?;
                }
            } else {
                let mut min_val: Option<Value> = None;
                for &row_idx in sorted_indices {
                    context.check_deadline()?;
                    let val = &arg_values[row_idx][0];
                    if !val.is_null() {
                        min_val = Some(match min_val {
                            None => val.clone(),
                            Some(cur) => {
                                if compare_runtime_values(val, &cur)?.unwrap_or(Ordering::Equal)
                                    == Ordering::Less
                                {
                                    val.clone()
                                } else {
                                    cur
                                }
                            }
                        });
                    }
                }
                let result = min_val.unwrap_or(Value::Null);
                assign_value_to_rows(context, values, sorted_indices, result)?;
            }
        }
        WindowFunctionKind::Max => {
            if has_order_by {
                let peer_groups = compute_peer_groups(sorted_indices, sort_keys, true);
                let mut running_max: Option<Value> = None;
                for (start, end) in &peer_groups {
                    context.check_deadline()?;
                    for &row_idx in &sorted_indices[*start..*end] {
                        context.check_deadline()?;
                        let val = &arg_values[row_idx][0];
                        if !val.is_null() {
                            running_max = Some(match running_max {
                                None => val.clone(),
                                Some(cur) => {
                                    if compare_runtime_values(val, &cur)?.unwrap_or(Ordering::Equal)
                                        == Ordering::Greater
                                    {
                                        val.clone()
                                    } else {
                                        cur
                                    }
                                }
                            });
                        }
                    }
                    let result = running_max.clone().unwrap_or(Value::Null);
                    assign_value_to_rows(context, values, &sorted_indices[*start..*end], result)?;
                }
            } else {
                let mut max_val: Option<Value> = None;
                for &row_idx in sorted_indices {
                    context.check_deadline()?;
                    let val = &arg_values[row_idx][0];
                    if !val.is_null() {
                        max_val = Some(match max_val {
                            None => val.clone(),
                            Some(cur) => {
                                if compare_runtime_values(val, &cur)?.unwrap_or(Ordering::Equal)
                                    == Ordering::Greater
                                {
                                    val.clone()
                                } else {
                                    cur
                                }
                            }
                        });
                    }
                }
                let result = max_val.unwrap_or(Value::Null);
                assign_value_to_rows(context, values, sorted_indices, result)?;
            }
        }
        WindowFunctionKind::NthValue => {
            if has_order_by {
                let peer_groups = compute_peer_groups(sorted_indices, sort_keys, true);
                for (start, end) in &peer_groups {
                    context.check_deadline()?;
                    let frame_end = *end;
                    for pos in *start..frame_end {
                        context.check_deadline()?;
                        let row_idx = sorted_indices[pos];
                        let n = positive_window_usize(&arg_values[row_idx][1], "nth_value", "n")?;
                        let result = if n <= frame_end {
                            let target_idx = sorted_indices[n - 1];
                            arg_values[target_idx][0].clone()
                        } else {
                            Value::Null
                        };
                        values[row_idx] = result;
                    }
                }
            } else {
                for &row_idx in sorted_indices {
                    context.check_deadline()?;
                    let n = positive_window_usize(&arg_values[row_idx][1], "nth_value", "n")?;
                    let result = sorted_indices
                        .get(n - 1)
                        .map_or(Value::Null, |&target_idx| arg_values[target_idx][0].clone());
                    values[row_idx] = result;
                }
            }
        }
        WindowFunctionKind::VarPop => {
            if has_order_by {
                let peer_groups = compute_peer_groups(sorted_indices, sort_keys, true);
                let mut sum = 0.0f64;
                let mut sum_sq = 0.0f64;
                let mut count = 0i64;
                for (start, end) in &peer_groups {
                    context.check_deadline()?;
                    for pos in *start..*end {
                        context.check_deadline()?;
                        let val = &arg_values[sorted_indices[pos]][0];
                        if !val.is_null() {
                            let v = value_to_double(val)?;
                            sum += v;
                            sum_sq += v * v;
                            count = count.saturating_add(1);
                        }
                    }
                    let result = compute_variance(sum, sum_sq, count, true)
                        .map_or(Value::Null, Value::Double);
                    assign_value_to_rows(context, values, &sorted_indices[*start..*end], result)?;
                }
            } else {
                let (sum, sum_sq, count) = window_sum_sq(sorted_indices, arg_values, context)?;
                let result =
                    compute_variance(sum, sum_sq, count, true).map_or(Value::Null, Value::Double);
                assign_value_to_rows(context, values, sorted_indices, result)?;
            }
        }
        WindowFunctionKind::VarSamp => {
            if has_order_by {
                let peer_groups = compute_peer_groups(sorted_indices, sort_keys, true);
                let mut sum = 0.0f64;
                let mut sum_sq = 0.0f64;
                let mut count = 0i64;
                for (start, end) in &peer_groups {
                    context.check_deadline()?;
                    for pos in *start..*end {
                        context.check_deadline()?;
                        let val = &arg_values[sorted_indices[pos]][0];
                        if !val.is_null() {
                            let v = value_to_double(val)?;
                            sum += v;
                            sum_sq += v * v;
                            count = count.saturating_add(1);
                        }
                    }
                    let result = compute_variance(sum, sum_sq, count, false)
                        .map_or(Value::Null, Value::Double);
                    assign_value_to_rows(context, values, &sorted_indices[*start..*end], result)?;
                }
            } else {
                let (sum, sum_sq, count) = window_sum_sq(sorted_indices, arg_values, context)?;
                let result =
                    compute_variance(sum, sum_sq, count, false).map_or(Value::Null, Value::Double);
                assign_value_to_rows(context, values, sorted_indices, result)?;
            }
        }
        WindowFunctionKind::StddevPop => {
            if has_order_by {
                let peer_groups = compute_peer_groups(sorted_indices, sort_keys, true);
                let mut sum = 0.0f64;
                let mut sum_sq = 0.0f64;
                let mut count = 0i64;
                for (start, end) in &peer_groups {
                    context.check_deadline()?;
                    for pos in *start..*end {
                        context.check_deadline()?;
                        let val = &arg_values[sorted_indices[pos]][0];
                        if !val.is_null() {
                            let v = value_to_double(val)?;
                            sum += v;
                            sum_sq += v * v;
                            count = count.saturating_add(1);
                        }
                    }
                    let result =
                        compute_stddev(sum, sum_sq, count, true).map_or(Value::Null, Value::Double);
                    assign_value_to_rows(context, values, &sorted_indices[*start..*end], result)?;
                }
            } else {
                let (sum, sum_sq, count) = window_sum_sq(sorted_indices, arg_values, context)?;
                let result =
                    compute_stddev(sum, sum_sq, count, true).map_or(Value::Null, Value::Double);
                assign_value_to_rows(context, values, sorted_indices, result)?;
            }
        }
        WindowFunctionKind::StddevSamp => {
            if has_order_by {
                let peer_groups = compute_peer_groups(sorted_indices, sort_keys, true);
                let mut sum = 0.0f64;
                let mut sum_sq = 0.0f64;
                let mut count = 0i64;
                for (start, end) in &peer_groups {
                    context.check_deadline()?;
                    for pos in *start..*end {
                        context.check_deadline()?;
                        let val = &arg_values[sorted_indices[pos]][0];
                        if !val.is_null() {
                            let v = value_to_double(val)?;
                            sum += v;
                            sum_sq += v * v;
                            count = count.saturating_add(1);
                        }
                    }
                    let result = compute_stddev(sum, sum_sq, count, false)
                        .map_or(Value::Null, Value::Double);
                    assign_value_to_rows(context, values, &sorted_indices[*start..*end], result)?;
                }
            } else {
                let (sum, sum_sq, count) = window_sum_sq(sorted_indices, arg_values, context)?;
                let result =
                    compute_stddev(sum, sum_sq, count, false).map_or(Value::Null, Value::Double);
                assign_value_to_rows(context, values, sorted_indices, result)?;
            }
        }
    }
    Ok(())
}

fn window_usize(value: &Value, func_name: &str, arg_name: &str) -> DbResult<usize> {
    let value = match value {
        Value::Int(value) => i64::from(*value),
        Value::BigInt(value) => *value,
        Value::Null => {
            return Err(DbError::internal(format!(
                "{func_name}() {arg_name} must not be NULL"
            )));
        }
        other => {
            return Err(DbError::internal(format!(
                "{func_name}() {arg_name} must be an integer, got {other:?}"
            )));
        }
    };
    usize::try_from(value).map_err(|_| {
        DbError::internal(format!("{func_name}() {arg_name} is too large to evaluate"))
    })
}

fn positive_window_usize(value: &Value, func_name: &str, arg_name: &str) -> DbResult<usize> {
    let value = window_usize(value, func_name, arg_name)?;
    if value == 0 {
        return Err(DbError::internal(format!(
            "{func_name}() {arg_name} must be greater than zero"
        )));
    }
    Ok(value)
}

fn window_offset(arg_values: &[Value], func_name: &str) -> DbResult<usize> {
    match arg_values.get(1) {
        Some(value) => window_usize(value, func_name, "offset"),
        None => Ok(1),
    }
}

fn ntile_bucket_for_row(position: usize, partition_len: usize, bucket_count: usize) -> usize {
    let base_size = partition_len / bucket_count;
    let remainder = partition_len % bucket_count;
    let larger_buckets_len = remainder.saturating_mul(base_size + 1);

    if position < larger_buckets_len {
        position / (base_size + 1) + 1
    } else {
        let bucket_offset = (position - larger_buckets_len)
            .checked_div(base_size)
            .unwrap_or(0);
        remainder + bucket_offset + 1
    }
}

fn offset_window_value(
    sorted_indices: &[usize],
    arg_values: &[Vec<Value>],
    target_pos: Option<usize>,
    current_row_idx: usize,
) -> Value {
    target_pos
        .and_then(|pos| sorted_indices.get(pos))
        .map_or_else(
            || {
                arg_values
                    .get(current_row_idx)
                    .and_then(|row| row.get(2))
                    .cloned()
                    .unwrap_or(Value::Null)
            },
            |&target_row_idx| arg_values[target_row_idx][0].clone(),
        )
}

fn sort_keys_equal(a: &[Value], b: &[Value]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    for (av, bv) in a.iter().zip(b.iter()) {
        match compare_runtime_values(av, bv) {
            Ok(Some(Ordering::Equal)) => {}
            _ => return false,
        }
    }
    true
}

fn compute_peer_groups(
    sorted_indices: &[usize],
    sort_keys: &[Vec<Value>],
    has_order_by: bool,
) -> Vec<(usize, usize)> {
    if !has_order_by || sorted_indices.is_empty() {
        return vec![(0, sorted_indices.len())];
    }
    let mut groups = Vec::new();
    let mut start = 0;
    for i in 1..sorted_indices.len() {
        if !sort_keys_equal(
            &sort_keys[sorted_indices[i]],
            &sort_keys[sorted_indices[start]],
        ) {
            groups.push((start, i));
            start = i;
        }
    }
    groups.push((start, sorted_indices.len()));
    groups
}

fn window_sum_sq(
    sorted_indices: &[usize],
    arg_values: &[Vec<Value>],
    context: &ExecutionContext,
) -> DbResult<(f64, f64, i64)> {
    let mut sum = 0.0f64;
    let mut sum_sq = 0.0f64;
    let mut count = 0i64;
    for &row_idx in sorted_indices {
        context.check_deadline()?;
        let val = &arg_values[row_idx][0];
        if !val.is_null() {
            let v = value_to_double(val)?;
            sum += v;
            sum_sq += v * v;
            count = count.saturating_add(1);
        }
    }
    Ok((sum, sum_sq, count))
}
