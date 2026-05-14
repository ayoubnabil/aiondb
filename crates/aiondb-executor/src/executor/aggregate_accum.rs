use super::*;

impl Executor {
    pub(super) fn accumulate_value(
        &self,
        acc: &mut AggAccumulator,
        template: &AggTemplate,
        row: &Row,
        context: &ExecutionContext,
    ) -> DbResult<()> {
        match &template.kind {
            AggKind::CountStar => {
                acc.count = acc.count.saturating_add(1);
            }
            AggKind::CountExpr(inner) => {
                let val = self.evaluate_expr_with_row(inner, row, context)?;
                if !val.is_null() {
                    if !self.check_distinct(acc, &val, context)? {
                        return Ok(());
                    }
                    acc.count = acc.count.saturating_add(1);
                }
            }
            AggKind::Sum(inner) | AggKind::Avg(inner) => {
                let val = self.evaluate_expr_with_row(inner, row, context)?;
                if !val.is_null() {
                    if !self.check_distinct(acc, &val, context)? {
                        return Ok(());
                    }
                    acc.count = acc.count.saturating_add(1);
                    acc.sum = Some(agg_add_value(acc.sum.take(), &val)?);
                }
            }
            AggKind::AnyValue(inner) => {
                if acc.passthrough.is_none() {
                    let val = self.evaluate_expr_with_row(inner, row, context)?;
                    if !val.is_null() {
                        acc.passthrough = Some(val);
                    }
                }
            }
            AggKind::Min(inner) => {
                let val = self.evaluate_expr_with_row(inner, row, context)?;
                if !val.is_null() {
                    match acc.extremum.as_ref() {
                        Some(cur)
                            if compare_runtime_values(&val, cur)?.unwrap_or(Ordering::Equal)
                                != Ordering::Less => {}
                        _ => acc.extremum = Some(val),
                    }
                }
            }
            AggKind::Max(inner) => {
                let val = self.evaluate_expr_with_row(inner, row, context)?;
                if !val.is_null() {
                    match acc.extremum.as_ref() {
                        Some(cur)
                            if compare_runtime_values(&val, cur)?.unwrap_or(Ordering::Equal)
                                != Ordering::Greater => {}
                        _ => acc.extremum = Some(val),
                    }
                }
            }
            AggKind::StringAgg(inner, _) => {
                let val = self.evaluate_expr_with_row(inner, row, context)?;
                if !val.is_null() {
                    if !self.check_distinct(acc, &val, context)? {
                        return Ok(());
                    }
                    let s = if let Value::Text(s) = val {
                        s
                    } else {
                        val.to_string()
                    };
                    let string_memory = u64::try_from(s.len())
                        .unwrap_or(u64::MAX)
                        .saturating_add(64);
                    context.track_memory(string_memory)?;
                    acc.string_parts.push(s);
                }
            }
            AggKind::ArrayAgg(inner, _) => {
                let val = self.evaluate_expr_with_row(inner, row, context)?;
                if !self.check_distinct(acc, &val, context)? {
                    return Ok(());
                }
                acc.validate_array_agg_input(inner, &val)?;
                context
                    .track_memory(super::helpers::estimate_value_bytes(&val).saturating_add(64))?;
                acc.array_parts.push(val);
            }
            AggKind::BoolAnd(inner) => {
                let val = self.evaluate_expr_with_row(inner, row, context)?;
                if let Value::Boolean(b) = val {
                    acc.bool_acc = Some(acc.bool_acc.unwrap_or(true) && b);
                }
            }
            AggKind::BoolOr(inner) => {
                let val = self.evaluate_expr_with_row(inner, row, context)?;
                if let Value::Boolean(b) = val {
                    acc.bool_acc = Some(acc.bool_acc.unwrap_or(false) || b);
                }
            }
            AggKind::StddevPop(inner)
            | AggKind::StddevSamp(inner)
            | AggKind::VarPop(inner)
            | AggKind::VarSamp(inner) => {
                let val = self.evaluate_expr_with_row(inner, row, context)?;
                if !val.is_null() {
                    let d = value_to_double(&val)?;
                    if !d.is_finite() {
                        acc.var_saw_non_finite = true;
                    }
                    let prev_count = acc.count;
                    acc.count = acc.count.saturating_add(1);
                    if prev_count == 0 {
                        acc.var_mean = d;
                        acc.var_m2 = 0.0;
                    } else {
                        let n = i64_to_f64(acc.count);
                        let delta = d - acc.var_mean;
                        acc.var_mean += delta / n;
                        let delta2 = d - acc.var_mean;
                        acc.var_m2 += delta * delta2;
                    }
                    let sq = Value::Double(d * d);
                    acc.sum = Some(agg_add_value(acc.sum.take(), &val)?);
                    acc.sum_sq = Some(agg_add_value(acc.sum_sq.take(), &sq)?);
                }
            }
            AggKind::PassThrough(inner) => {
                if acc.passthrough.is_none() {
                    // Skip window function expressions during accumulation;
                    // they will be evaluated post-aggregation.
                    if !matches!(inner.kind, TypedExprKind::WindowFunction { .. }) {
                        let val = self.evaluate_expr_with_row(inner, row, context)?;
                        acc.passthrough = Some(val);
                    }
                }
            }
            AggKind::CompositeAgg { sub_aggs, .. } => {
                // Accumulate each sub-aggregate independently.
                for (i, (_, sub_template)) in sub_aggs.iter().enumerate() {
                    if let Some(sub_acc) = acc.sub_accumulators.get_mut(i) {
                        // Apply per-sub-aggregate filter if present
                        if let Some(ref filter_expr) = sub_template.filter {
                            let filter_val =
                                self.evaluate_expr_with_row(filter_expr, row, context)?;
                            if !matches!(filter_val, Value::Boolean(true)) {
                                continue;
                            }
                        }
                        self.accumulate_value(sub_acc, sub_template, row, context)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn check_distinct(
        &self,
        acc: &mut AggAccumulator,
        val: &Value,
        context: &ExecutionContext,
    ) -> DbResult<bool> {
        if let Some(ref mut seen) = acc.distinct_seen {
            let key = build_hash_key(val)?;
            let is_new = seen.insert(key);
            if is_new {
                context
                    .track_memory(super::helpers::estimate_value_bytes(val).saturating_add(64))?;
            }
            Ok(is_new)
        } else {
            Ok(true)
        }
    }
}
