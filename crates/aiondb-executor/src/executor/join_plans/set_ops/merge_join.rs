use super::*;

impl Executor {
    pub(super) fn merge_compare_keys(
        left: &Row,
        left_keys: &[usize],
        right: &Row,
        right_keys: &[usize],
    ) -> DbResult<Ordering> {
        for (lk, rk) in left_keys.iter().zip(right_keys.iter()) {
            let lv = left.values.get(*lk).unwrap_or(&Value::Null);
            let rv = right.values.get(*rk).unwrap_or(&Value::Null);
            match (lv, rv) {
                (Value::Null, Value::Null) => continue,
                (Value::Null, _) => return Ok(Ordering::Greater),
                (_, Value::Null) => return Ok(Ordering::Less),
                _ => {}
            }
            let cmp = compare_runtime_values(lv, rv)?.unwrap_or(Ordering::Equal);
            if cmp != Ordering::Equal {
                return Ok(cmp);
            }
        }
        Ok(Ordering::Equal)
    }

    pub(super) fn merge_key_has_null(row: &Row, keys: &[usize]) -> bool {
        keys.iter().any(|k| {
            row.values
                .get(*k)
                .map_or(true, |v| matches!(v, Value::Null))
        })
    }

    pub(super) fn merge_join_for_each_combined_row(
        &self,
        left_rows: &[Row],
        right_rows: &[Row],
        join_type: &JoinType,
        left_keys: &[usize],
        right_keys: &[usize],
        residual: Option<&TypedExpr>,
        filter: Option<&TypedExpr>,
        left_width: usize,
        right_width: usize,
        context: &ExecutionContext,
        on_row: &mut dyn FnMut(Row) -> DbResult<bool>,
    ) -> DbResult<()> {
        let on_row = &mut |row: Row| -> DbResult<bool> {
            context.check_join_row_limit()?;
            on_row(row)
        };
        let residual_requires_special_resolution =
            residual.is_some_and(super::projection_plans::expr_requires_special_resolution);
        let filter_requires_special_resolution =
            filter.is_some_and(super::projection_plans::expr_requires_special_resolution);

        let null_left = Row::new(vec![Value::Null; left_width]);
        let null_right = Row::new(vec![Value::Null; right_width]);

        let needs_right_unmatched = matches!(join_type, JoinType::Right | JoinType::Full);
        let mut right_matched = if needs_right_unmatched {
            vec![false; right_rows.len()]
        } else {
            Vec::new()
        };

        let mut li = 0usize;
        let mut ri = 0usize;
        let mut any_left_matched_scratch: Vec<bool> = Vec::new();

        while li < left_rows.len() && ri < right_rows.len() {
            context.check_deadline()?;
            let left_row = &left_rows[li];
            let right_row = &right_rows[ri];

            if Self::merge_key_has_null(left_row, left_keys) {
                if matches!(join_type, JoinType::Left | JoinType::Full) {
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
                li += 1;
                continue;
            }
            if Self::merge_key_has_null(right_row, right_keys) {
                ri += 1;
                continue;
            }

            let cmp = Self::merge_compare_keys(left_row, left_keys, right_row, right_keys)?;
            match cmp {
                Ordering::Less => {
                    if matches!(join_type, JoinType::Left | JoinType::Full) {
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
                    li += 1;
                }
                Ordering::Greater => {
                    if needs_right_unmatched {
                        let combined = combine_rows(&null_left, right_row);
                        if self.evaluate_optional_predicate_prechecked(
                            filter,
                            &combined,
                            context,
                            filter_requires_special_resolution,
                        )? {
                            right_matched[ri] = true;
                            if !on_row(combined)? {
                                return Ok(());
                            }
                        }
                    }
                    ri += 1;
                }
                Ordering::Equal => {
                    let li_start = li;
                    while li < left_rows.len()
                        && Self::merge_compare_keys(
                            &left_rows[li],
                            left_keys,
                            right_row,
                            right_keys,
                        )? == Ordering::Equal
                    {
                        li += 1;
                    }
                    let ri_start = ri;
                    while ri < right_rows.len()
                        && Self::merge_compare_keys(
                            left_row,
                            left_keys,
                            &right_rows[ri],
                            right_keys,
                        )? == Ordering::Equal
                    {
                        ri += 1;
                    }

                    any_left_matched_scratch.clear();
                    any_left_matched_scratch.resize(li - li_start, false);
                    for rj in ri_start..ri {
                        context.check_deadline()?;
                        let rr = &right_rows[rj];
                        for (loff, lj) in (li_start..li).enumerate() {
                            let lr = &left_rows[lj];
                            let combined = combine_rows(lr, rr);
                            if !self.evaluate_optional_predicate_prechecked(
                                residual,
                                &combined,
                                context,
                                residual_requires_special_resolution,
                            )? {
                                continue;
                            }
                            any_left_matched_scratch[loff] = true;
                            if needs_right_unmatched {
                                right_matched[rj] = true;
                            }
                            if !self.evaluate_optional_predicate_prechecked(
                                filter,
                                &combined,
                                context,
                                filter_requires_special_resolution,
                            )? {
                                continue;
                            }
                            if !on_row(combined)? {
                                return Ok(());
                            }
                        }
                    }

                    if matches!(join_type, JoinType::Left | JoinType::Full) {
                        for (loff, lj) in (li_start..li).enumerate() {
                            if any_left_matched_scratch[loff] {
                                continue;
                            }
                            let combined = combine_rows(&left_rows[lj], &null_right);
                            if !self.evaluate_optional_predicate_prechecked(
                                filter,
                                &combined,
                                context,
                                filter_requires_special_resolution,
                            )? {
                                continue;
                            }
                            if !on_row(combined)? {
                                return Ok(());
                            }
                        }
                    }
                }
            }
        }

        if matches!(join_type, JoinType::Left | JoinType::Full) {
            while li < left_rows.len() {
                context.check_deadline()?;
                let combined = combine_rows(&left_rows[li], &null_right);
                if self.evaluate_optional_predicate_prechecked(
                    filter,
                    &combined,
                    context,
                    filter_requires_special_resolution,
                )? && !on_row(combined)?
                {
                    return Ok(());
                }
                li += 1;
            }
        }

        if needs_right_unmatched {
            for (rj, right_row) in right_rows.iter().enumerate() {
                if right_matched[rj] {
                    continue;
                }
                context.check_deadline()?;
                let combined = combine_rows(&null_left, right_row);
                if !self.evaluate_optional_predicate_prechecked(
                    filter,
                    &combined,
                    context,
                    filter_requires_special_resolution,
                )? {
                    continue;
                }
                if !on_row(combined)? {
                    return Ok(());
                }
            }
        }

        Ok(())
    }
}
