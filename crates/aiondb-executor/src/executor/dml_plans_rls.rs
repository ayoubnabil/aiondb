//! DML pre-execution helpers: compat RLS policy compile/enforce,
//! filter pushdown extraction, and sequence-value prefetch.
//!
//! Split out of `dml_plans.rs` (pre-`execute_dml_plan` methods).
//! Continuation of `impl Executor`; parent scope via `use super::*`.
#![allow(clippy::too_many_lines, clippy::type_complexity)]

use super::*;

impl Executor {
    pub(in crate::executor) fn compat_row_security_setting_enabled(&self, context: &ExecutionContext) -> DbResult<bool> {
        let Some(value) = context.resolve_session_setting("row_security") else {
            // PostgreSQL defaults to ON.
            return Ok(true);
        };
        match value
            .trim()
            .trim_matches('\'')
            .to_ascii_lowercase()
            .as_str()
        {
            "on" | "true" | "1" => Ok(true),
            "off" | "false" | "0" => Ok(false),
            other => Err(DbError::bind_error(
                SqlState::InvalidParameterValue,
                format!("invalid value for row_security: \"{other}\""),
            )),
        }
    }

    pub(in crate::executor) fn compat_query_affected_by_rls_error(&self, table: &TableDescriptor) -> DbError {
        DbError::insufficient_privilege(format!(
            "query would be affected by row-level security policy for table \"{}\"",
            table.name.object_name()
        ))
    }

    pub(in crate::executor) fn parse_compat_rls_expression(
        &self,
        expression_sql: &str,
        context: &str,
    ) -> DbResult<aiondb_parser::Expr> {
        aiondb_parser::parse_expression(expression_sql).map_err(|error| {
            DbError::bind_error(SqlState::SyntaxError, format!("invalid {context}: {error}"))
        })
    }

    pub(in crate::executor) fn type_check_compat_rls_expression(
        &self,
        expr: &aiondb_parser::Expr,
        relation: &TableDescriptor,
        context: &ExecutionContext,
    ) -> DbResult<TypedExpr> {
        let search_path_schemas = super::session_search_path_schemas(context);
        let current_schema = search_path_schemas.first().cloned();
        let current_user = context.current_user_name();
        aiondb_planner::type_check_expression_with_relation_and_session_context(
            expr,
            relation,
            current_user.clone(),
            current_user,
            current_schema,
            None,
        )
    }

    /// Detect a filter shape `col IN (lit1, lit2, …, litN) AND <residual>`
    /// where the IN-list is the only push-eligible clause and the
    /// rest (sub-queries, function calls, …) becomes a residual to
    /// re-evaluate per matching tuple. Returns `Some((column_id,
    /// literals, residual))` when the pattern matches with a
    /// non-empty IN-list, else `None`. Mirrors PG's
    /// `match_or_clause_to_indexscan_clauses` for the IN-as-OR-of-eqs
    /// shape — except instead of an index we use the heap's
    /// `scan_table_in_filter` storage primitive which iterates only
    /// rows whose column value matches one of the listed literals.
    pub(in crate::executor) fn dml_extract_in_list_with_residual(
        &self,
        filter: &TypedExpr,
        table: &TableDescriptor,
    ) -> Option<(ColumnId, Vec<Value>, Option<TypedExpr>)> {
        fn collect<'a>(expr: &'a TypedExpr, out: &mut Vec<&'a TypedExpr>) {
            match &expr.kind {
                TypedExprKind::LogicalAnd { left, right } => {
                    collect(left, out);
                    collect(right, out);
                }
                _ => out.push(expr),
            }
        }
        let mut conjuncts = Vec::new();
        collect(filter, &mut conjuncts);

        let mut in_clause: Option<(usize, Vec<Value>)> = None;
        let mut residuals: Vec<TypedExpr> = Vec::new();
        for c in &conjuncts {
            // Detect an `IN (literal_list)` clause via the same
            // extractor the existing fast path uses.
            if let Some((ord, lits)) = extract_dml_in_literal_filter(c) {
                if in_clause.is_some() {
                    // Multiple IN clauses — bail. The user could
                    // be hand-rolling a multi-column IN-of-tuples
                    // shape, but our pushdown can only feed one
                    // column to `scan_table_in_filter`.
                    return None;
                }
                in_clause = Some((ord, lits));
                continue;
            }
            // Anything else becomes residual.
            residuals.push((*c).clone());
        }
        let (ord, literals) = in_clause?;
        if literals.is_empty() {
            return None;
        }
        let column = table.columns.get(ord)?;
        let residual = if residuals.is_empty() {
            None
        } else {
            let mut iter = residuals.into_iter();
            let first = iter.next()?;
            Some(iter.fold(first, |acc, next| TypedExpr {
                kind: TypedExprKind::LogicalAnd {
                    left: Box::new(acc),
                    right: Box::new(next),
                },
                data_type: aiondb_core::DataType::Boolean,
                nullable: true,
            }))
        };
        Some((column.column_id, literals, residual))
    }

    /// Decompose `filter` as an AND of per-column bounds suitable
    /// for `scan_table_multi_range_filter` pushdown. Returns
    /// `Some((predicates, residual))` where `predicates` is the
    /// pushable bound list and `residual` is the AND of conjuncts
    /// the storage layer cannot push down (sub-queries, function
    /// calls, etc.). Returns `None` when no conjunct is pushable.
    ///
    /// Splitting pushable from residual is what lets shapes like
    /// `WHERE EXISTS(...) AND cat IN ('c0','c1')` benefit: the
    /// `cat IN (...)` clause becomes a storage-side filter that
    /// shrinks the candidate set by ~5x BEFORE the EXISTS sub-plan
    /// is evaluated, instead of every one of the 20k tuples paying
    /// the full per-row evaluator dispatch.
    pub(in crate::executor) fn dml_extract_pushdown_bounds(
        &self,
        filter: &TypedExpr,
        table: &TableDescriptor,
    ) -> Option<(
        Vec<(ColumnId, std::ops::Bound<Value>, std::ops::Bound<Value>)>,
        Option<TypedExpr>,
    )> {
        fn collect<'a>(expr: &'a TypedExpr, out: &mut Vec<&'a TypedExpr>) {
            match &expr.kind {
                TypedExprKind::LogicalAnd { left, right } => {
                    collect(left, out);
                    collect(right, out);
                }
                _ => out.push(expr),
            }
        }
        let mut conjuncts = Vec::new();
        collect(filter, &mut conjuncts);

        let mut bounds: Vec<(usize, std::ops::Bound<Value>, std::ops::Bound<Value>)> = Vec::new();
        let mut residuals: Vec<TypedExpr> = Vec::new();
        for c in &conjuncts {
            if let Some(eq) = extract_dml_simple_eq_literal_filter(c) {
                bounds.push((
                    eq.column_ordinal,
                    std::ops::Bound::Included(eq.literal.clone()),
                    std::ops::Bound::Included(eq.literal),
                ));
                continue;
            }
            if let Some(rb) = extract_dml_range_literal_filter(c) {
                let lower = match rb.lower {
                    aiondb_storage_api::Bound::Unbounded => std::ops::Bound::Unbounded,
                    aiondb_storage_api::Bound::Included(v) => std::ops::Bound::Included(v),
                    aiondb_storage_api::Bound::Excluded(v) => std::ops::Bound::Excluded(v),
                };
                let upper = match rb.upper {
                    aiondb_storage_api::Bound::Unbounded => std::ops::Bound::Unbounded,
                    aiondb_storage_api::Bound::Included(v) => std::ops::Bound::Included(v),
                    aiondb_storage_api::Bound::Excluded(v) => std::ops::Bound::Excluded(v),
                };
                bounds.push((rb.column_ordinal, lower, upper));
                continue;
            }
            if let Some(eq_clauses) = extract_dml_composite_eq_literal_filter(c) {
                for (ord, lit) in eq_clauses {
                    bounds.push((
                        ord,
                        std::ops::Bound::Included(lit.clone()),
                        std::ops::Bound::Included(lit),
                    ));
                }
                continue;
            }
            // Not pushable on its own — keep it as residual so the
            // executor can re-evaluate it per candidate row.
            residuals.push((*c).clone());
        }
        if bounds.is_empty() {
            return None;
        }
        let mut column_predicates: Vec<(ColumnId, std::ops::Bound<Value>, std::ops::Bound<Value>)> =
            Vec::with_capacity(bounds.len());
        for (ord, lo, hi) in bounds {
            let column = table.columns.get(ord)?;
            column_predicates.push((column.column_id, lo, hi));
        }
        let residual = if residuals.is_empty() {
            None
        } else {
            // Reduce `[a, b, c]` to `(a AND b AND c)` left-associatively.
            let mut iter = residuals.into_iter();
            let first = iter.next()?;
            Some(iter.fold(first, |acc, next| TypedExpr {
                kind: TypedExprKind::LogicalAnd {
                    left: Box::new(acc),
                    right: Box::new(next),
                },
                data_type: aiondb_core::DataType::Boolean,
                nullable: true,
            }))
        };
        Some((column_predicates, residual))
    }

    /// Decompose `filter` as an AND of equality-literal conjuncts plus an
    /// optional residual predicate that must still be evaluated row-by-row.
    ///
    /// This is narrower than `dml_extract_pushdown_bounds`: it keeps only
    /// exact equality predicates (`col = lit`, or ANDs that reduce to a list of
    /// equalities) so DML paths can build multi-index bitmap intersections
    /// without mistaking a wider range predicate for an exact lookup.
    pub(in crate::executor) fn dml_extract_eq_conjuncts_with_residual(
        &self,
        filter: &TypedExpr,
        _table: &TableDescriptor,
    ) -> Option<(Vec<(usize, Value)>, Option<TypedExpr>)> {
        fn collect<'a>(expr: &'a TypedExpr, out: &mut Vec<&'a TypedExpr>) {
            match &expr.kind {
                TypedExprKind::LogicalAnd { left, right } => {
                    collect(left, out);
                    collect(right, out);
                }
                _ => out.push(expr),
            }
        }

        let mut conjuncts = Vec::new();
        collect(filter, &mut conjuncts);

        let mut eqs = Vec::new();
        let mut residuals = Vec::new();
        for conjunct in conjuncts {
            if let Some(eq) = extract_dml_simple_eq_literal_filter(conjunct) {
                eqs.push((eq.column_ordinal, eq.literal));
                continue;
            }
            if let Some(eq_clauses) = extract_dml_composite_eq_literal_filter(conjunct) {
                eqs.extend(eq_clauses);
                continue;
            }
            residuals.push(conjunct.clone());
        }

        if eqs.is_empty() {
            return None;
        }

        let residual = if residuals.is_empty() {
            None
        } else {
            let mut iter = residuals.into_iter();
            let first = iter.next()?;
            Some(iter.fold(first, TypedExpr::logical_and))
        };

        Some((eqs, residual))
    }

    /// Try to prove that `filter` matches NO visible row of
    /// `table` without scanning the heap. Walks the filter as an
    /// AND-conjunction, extracting `(column_ordinal, lower_bound,
    /// upper_bound)` triples for every clause we can recognise
    /// (`col = lit`, `col CMP lit`, `col BETWEEN lo AND hi`,
    /// `col IN (lits)` collapsed to per-literal eq), then asks the
    /// storage backend to consult its per-`(ordinal, value)` count
    /// map. Returns `true` only when the backend can authoritatively
    /// answer "no row matches"; on any uncertainty (unknown clause
    /// kind, untrackable column type, concurrent active txns,
    /// overlay present, …) returns `false` so the caller falls
    /// through to the regular execution path.
    pub(in crate::executor) fn dml_filter_proves_empty(
        &self,
        filter: &TypedExpr,
        table: &TableDescriptor,
        table_id: aiondb_core::RelationId,
        context: &ExecutionContext,
    ) -> DbResult<bool> {
        // Walk AND-conjuncts. Any clause we can't decompose into a
        // simple `(ordinal, lower, upper)` predicate disables the
        // pre-flight (we can't prove emptiness on opaque shapes).
        fn collect<'a>(expr: &'a TypedExpr, out: &mut Vec<&'a TypedExpr>) {
            match &expr.kind {
                TypedExprKind::LogicalAnd { left, right } => {
                    collect(left, out);
                    collect(right, out);
                }
                _ => out.push(expr),
            }
        }
        let mut conjuncts = Vec::new();
        collect(filter, &mut conjuncts);

        let mut bounds: Vec<(usize, std::ops::Bound<Value>, std::ops::Bound<Value>)> = Vec::new();
        for c in &conjuncts {
            // Detect a single equality literal first — most
            // common shape, exactly bounded.
            if let Some(eq) = extract_dml_simple_eq_literal_filter(c) {
                bounds.push((
                    eq.column_ordinal,
                    std::ops::Bound::Included(eq.literal.clone()),
                    std::ops::Bound::Included(eq.literal),
                ));
                continue;
            }
            // Comparison / BETWEEN: same DmlRangeBound the index
            // path uses.
            if let Some(rb) = extract_dml_range_literal_filter(c) {
                let lower = match rb.lower {
                    aiondb_storage_api::Bound::Unbounded => std::ops::Bound::Unbounded,
                    aiondb_storage_api::Bound::Included(v) => std::ops::Bound::Included(v),
                    aiondb_storage_api::Bound::Excluded(v) => std::ops::Bound::Excluded(v),
                };
                let upper = match rb.upper {
                    aiondb_storage_api::Bound::Unbounded => std::ops::Bound::Unbounded,
                    aiondb_storage_api::Bound::Included(v) => std::ops::Bound::Included(v),
                    aiondb_storage_api::Bound::Excluded(v) => std::ops::Bound::Excluded(v),
                };
                bounds.push((rb.column_ordinal, lower, upper));
                continue;
            }
            // Composite eq via `extract_dml_composite_eq_literal_filter`
            // returns the AND-of-eqs as a list; expand each into a
            // bound entry.
            if let Some(eq_clauses) = extract_dml_composite_eq_literal_filter(c) {
                for (ord, lit) in eq_clauses {
                    bounds.push((
                        ord,
                        std::ops::Bound::Included(lit.clone()),
                        std::ops::Bound::Included(lit),
                    ));
                }
                continue;
            }
            // Unknown clause — abort pre-flight so we don't
            // false-positive an emptiness conclusion.
            return Ok(false);
        }
        if bounds.is_empty() {
            return Ok(false);
        }
        // Map ordinals → ColumnIds for the storage call.
        let mut column_predicates: Vec<(
            aiondb_core::ColumnId,
            std::ops::Bound<Value>,
            std::ops::Bound<Value>,
        )> = Vec::with_capacity(bounds.len());
        for (ord, lo, hi) in bounds {
            let Some(column) = table.columns.get(ord) else {
                return Ok(false);
            };
            column_predicates.push((column.column_id, lo, hi));
        }
        let proven = self.storage_dml.try_prove_filter_empty(
            context.txn_id,
            &context.snapshot,
            table_id,
            &column_predicates,
        )?;
        Ok(matches!(proven, Some(true)))
    }

    pub(in crate::executor) fn compile_compat_rls_policies(
        &self,
        table: &TableDescriptor,
        action: CompatRlsAction,
        context: &ExecutionContext,
    ) -> DbResult<Option<Vec<CompatRlsPolicy>>> {
        // Fast path: when the session has no compat_misc_attrs at all,
        // no RLS configuration can possibly apply to this query, so we
        // can skip the (relatively expensive) role-lookup catalog reads
        // entirely. The catalog reads only matter when there's
        // potentially RLS to bypass.
        let session_has_compat_misc =
            aiondb_eval::with_current_session_context(|ctx| !ctx.compat_misc_attrs.is_empty());
        if !session_has_compat_misc {
            return Ok(None);
        }
        let current_user = context.current_user_name().unwrap_or_default();
        if current_user.is_empty() {
            return Ok(None);
        }
        if self.role_is_superuser(&current_user, context)?
            || self.role_has_bypassrls(&current_user, context)?
        {
            return Ok(None);
        }

        let table_name = table.name.object_name().to_ascii_lowercase();
        let qualified_name = table.name.to_string().to_ascii_lowercase();
        let Some((rls_enabled, rls_force)) =
            aiondb_eval::with_current_session_context(|session_context| {
                Ok(session_context.compat_misc_attrs.iter().find_map(
                    |((kind, name), (_, _, _, options_joined, _, _))| {
                        // SECURITY: only allow either an exact qualified-name
                        // match, or an unqualified compat entry (no dot)
                        // matching the runtime basename. The previous third
                        // arm — strip the compat entry's schema and match its
                        // tail against the runtime basename — let a
                        // `secret.users` compat row attach its rls=enabled /
                        // rls_force flag to a `public.users` runtime query
                        // (and vice versa). Combined with the non-deterministic
                        // HashMap iteration order of compat_misc_attrs, this
                        // produces an intermittent cross-schema RLS-policy
                        // confusion. See SECURITY_FINDINGS_0DAY_A1.md F3.
                        let entry_is_unqualified = !name.contains('.');
                        let matches_relation = name.eq_ignore_ascii_case(&qualified_name)
                            || (entry_is_unqualified && name.eq_ignore_ascii_case(&table_name));
                        if kind != "CREATE TABLE" || !matches_relation {
                            return None;
                        }
                        let mut enabled = false;
                        let mut force = false;
                        for pair in options_joined.split(',').map(str::trim) {
                            if let Some(value) = pair.strip_prefix("rls=") {
                                enabled = !matches!(value, "disabled" | "");
                            }
                            if let Some(value) = pair.strip_prefix("rls_force=") {
                                force = matches!(value, "force");
                            }
                        }
                        Some((enabled, force))
                    },
                ))
            })?
        else {
            return Ok(None);
        };

        if !rls_enabled && !rls_force {
            return Ok(None);
        }
        if table_descriptor_owner_matches(table, &current_user) && !rls_force {
            return Ok(None);
        }
        if !self.compat_row_security_setting_enabled(context)? {
            return Err(self.compat_query_affected_by_rls_error(table));
        }

        let effective_roles = self.effective_role_names(&current_user, context)?;
        let mut policy_specs: Vec<(Option<String>, bool, Option<String>, Option<String>)> =
            aiondb_eval::with_current_session_context(|session_context| {
                let mut policies = Vec::new();
                for ((kind, object_name), (_, _, _, options_joined, _, _)) in
                    session_context.compat_misc_attrs.iter()
                {
                    if kind != "CREATE POLICY" {
                        continue;
                    }
                    let pairs = split_compat_options_paren_aware(options_joined);
                    let policy_table_id = pairs
                        .iter()
                        .find_map(|p| p.strip_prefix("table_id="))
                        .and_then(|value| value.parse::<u64>().ok());
                    let policy_table_name = pairs
                        .iter()
                        .find_map(|p| p.strip_prefix("table=").map(str::to_owned))
                        .unwrap_or_default()
                        .to_ascii_lowercase();
                    // SECURITY: prefer table_id (unambiguous). Fall back to a
                    // qualified-name match. An unqualified policy entry (no
                    // dot in the recorded `table=` value) is allowed to match
                    // the runtime basename, but a *qualified* policy entry
                    // must NOT silently match a different schema's same-named
                    // table. See SECURITY_FINDINGS_0DAY_A1.md F3.
                    let entry_is_unqualified = !policy_table_name.contains('.');
                    let matches_table = policy_table_id
                        .is_some_and(|id| id == table.table_id.get())
                        || policy_table_name == qualified_name
                        || (entry_is_unqualified && policy_table_name == table_name);
                    if !matches_table {
                        continue;
                    }

                    let cmd = pairs
                        .iter()
                        .find_map(|p| p.strip_prefix("for=").map(str::to_ascii_lowercase))
                        .unwrap_or_else(|| "all".to_owned());
                    if cmd != "all" && cmd != action.policy_keyword() {
                        continue;
                    }

                    let roles_csv = pairs
                        .iter()
                        .find_map(|p| p.strip_prefix("to=").map(str::to_owned))
                        .unwrap_or_else(|| "public".to_owned());
                    let role_applies = roles_csv
                        .split('|')
                        .map(str::trim)
                        .filter(|role| !role.is_empty())
                        .any(|role| {
                            effective_roles
                                .iter()
                                .any(|candidate| candidate.eq_ignore_ascii_case(role))
                        });
                    if !role_applies {
                        continue;
                    }

                    let permissive = pairs
                        .iter()
                        .find_map(|p| p.strip_prefix("permissive=").map(str::to_ascii_lowercase))
                        .map_or(true, |value| value != "restrictive");
                    let using_sql = pairs
                        .iter()
                        .find_map(|p| p.strip_prefix("using=").map(str::to_owned));
                    let with_check_sql = pairs
                        .iter()
                        .find_map(|p| p.strip_prefix("with_check=").map(str::to_owned));
                    let policy_name = object_name
                        .split_once("@@")
                        .map(|(name, _)| name.to_owned())
                        .or_else(|| Some(object_name.clone()));
                    policies.push((policy_name, permissive, using_sql, with_check_sql));
                }
                Ok(policies)
            })?;
        policy_specs.sort_by(|left, right| {
            left.0
                .as_deref()
                .unwrap_or("")
                .to_ascii_lowercase()
                .cmp(&right.0.as_deref().unwrap_or("").to_ascii_lowercase())
        });

        let mut compiled = Vec::with_capacity(policy_specs.len());
        for (policy_name, permissive, using_sql, with_check_sql) in policy_specs {
            let using_expr = match using_sql {
                Some(sql) if !sql.trim().is_empty() => {
                    Some(self.type_check_compat_rls_expression(
                        &self.parse_compat_rls_expression(&sql, "row security USING expression")?,
                        table,
                        context,
                    )?)
                }
                _ => None,
            };
            let with_check_expr = match with_check_sql {
                Some(sql) if !sql.trim().is_empty() => {
                    Some(self.type_check_compat_rls_expression(
                        &self.parse_compat_rls_expression(
                            &sql,
                            "row security WITH CHECK expression",
                        )?,
                        table,
                        context,
                    )?)
                }
                _ => None,
            };
            let using_requires_special_resolution = using_expr
                .as_ref()
                .is_some_and(super::projection_plans::expr_requires_special_resolution);
            let with_check_requires_special_resolution = with_check_expr
                .as_ref()
                .is_some_and(super::projection_plans::expr_requires_special_resolution);
            compiled.push(CompatRlsPolicy {
                policy_name,
                permissive,
                using_expr,
                with_check_expr,
                using_requires_special_resolution,
                with_check_requires_special_resolution,
            });
        }
        Ok(Some(compiled))
    }

    pub(in crate::executor) fn evaluate_compat_rls_expr(
        &self,
        expr: &TypedExpr,
        row: &Row,
        context: &ExecutionContext,
        requires_special_resolution: bool,
    ) -> DbResult<bool> {
        self.evaluate_optional_predicate_prechecked(
            Some(expr),
            row,
            context,
            requires_special_resolution,
        )
    }

    /// Hot per-row scan-time predicate. The dominant case is
    /// `policies = None` (no RLS on the table), so make the inline
    /// fast-out explicit and let the codegen elide the function-call
    /// overhead in scan inner loops.
    #[inline]
    pub(in crate::executor) fn compat_rls_allows_existing_row(
        &self,
        policies: Option<&[CompatRlsPolicy]>,
        row: &Row,
        context: &ExecutionContext,
    ) -> DbResult<bool> {
        let Some(policies) = policies else {
            return Ok(true);
        };
        if policies.is_empty() {
            return Ok(false);
        }

        let mut permissive_allowed = false;
        let mut saw_permissive = false;
        for policy in policies {
            let decision = match &policy.using_expr {
                Some(expr) => self.evaluate_compat_rls_expr(
                    expr,
                    row,
                    context,
                    policy.using_requires_special_resolution,
                )?,
                None => true,
            };
            if policy.permissive {
                saw_permissive = true;
                permissive_allowed |= decision;
            } else if !decision {
                return Ok(false);
            }
        }
        Ok(saw_permissive && permissive_allowed)
    }

    pub(in crate::executor) fn compat_rls_enforce_new_row(
        &self,
        table: &TableDescriptor,
        policies: Option<&[CompatRlsPolicy]>,
        row: &Row,
        context: &ExecutionContext,
    ) -> DbResult<()> {
        let Some(policies) = policies else {
            return Ok(());
        };
        if policies.is_empty() {
            return Err(DbError::insufficient_privilege(format!(
                "new row violates row-level security policy for table \"{}\"",
                table.name.object_name()
            )));
        }

        let mut permissive_allowed = false;
        let mut saw_permissive = false;
        for policy in policies {
            // Prefer WITH CHECK; fall back to USING. The cached
            // requires_special_resolution flag matches whichever expression
            // we picked.
            let (expr_opt, requires_special_resolution) =
                if let Some(expr) = policy.with_check_expr.as_ref() {
                    (Some(expr), policy.with_check_requires_special_resolution)
                } else if let Some(expr) = policy.using_expr.as_ref() {
                    (Some(expr), policy.using_requires_special_resolution)
                } else {
                    (None, false)
                };
            let decision = match expr_opt {
                Some(expr) => {
                    self.evaluate_compat_rls_expr(expr, row, context, requires_special_resolution)?
                }
                None => true,
            };
            if policy.permissive {
                saw_permissive = true;
                permissive_allowed |= decision;
            } else if !decision {
                let message = if let Some(policy_name) = policy.policy_name.as_deref() {
                    format!(
                        "new row violates row-level security policy \"{}\" for table \"{}\"",
                        policy_name,
                        table.name.object_name()
                    )
                } else {
                    format!(
                        "new row violates row-level security policy for table \"{}\"",
                        table.name.object_name()
                    )
                };
                return Err(DbError::insufficient_privilege(message));
            }
        }
        if !saw_permissive || !permissive_allowed {
            return Err(DbError::insufficient_privilege(format!(
                "new row violates row-level security policy for table \"{}\"",
                table.name.object_name()
            )));
        }
        Ok(())
    }

    /// Probe whether the direct-insert fast path applies for `table_id`.
    ///
    /// On a positive answer, returns the resolved `TableDescriptor` so the
    /// caller can reuse it instead of issuing a second `get_table_by_id`
    /// catalog read - historically the InsertValues arm did exactly that.
    pub(in crate::executor) fn try_direct_insert_path(
        &self,
        table_id: RelationId,
        on_conflict: Option<&InsertOnConflict>,
        has_returning: bool,
        row_count: usize,
        context: &ExecutionContext,
    ) -> DbResult<Option<TableDescriptor>> {
        let on_conflict_compatible = match on_conflict.map(|conflict| &conflict.action) {
            None => true,
            Some(OnConflictActionPlan::DoNothing) => row_count == 1,
            Some(OnConflictActionPlan::DoUpdate { .. }) => false,
        };
        if !on_conflict_compatible || has_returning {
            return Ok(None);
        }

        let session_has_compat_misc =
            aiondb_eval::with_current_session_context(|ctx| !ctx.compat_misc_attrs.is_empty());
        let cache_revision = if session_has_compat_misc {
            None
        } else {
            Some(self.catalog_reader.catalog_revision(context.txn_id)?)
        };
        if let Some(revision) = cache_revision {
            if let Some(cached) = DIRECT_INSERT_PATH_CACHE
                .with(|cache| cache.borrow().get(&(revision, table_id)).cloned())
            {
                return Ok(cached);
            }
        }

        let table = self
            .catalog_reader
            .get_table_by_id(context.txn_id, table_id)?
            .ok_or_else(|| DbError::internal(format!("table {table_id:?} not found for INSERT")))?;

        if session_has_compat_misc
            && self
                .compile_compat_rls_policies(&table, CompatRlsAction::Insert, context)?
                .is_some()
        {
            return Ok(None);
        }

        // Direct insert path bypasses full per-row constraint evaluation.
        // Keep it disabled whenever FK/CHECK constraints are present so
        // INSERT semantics stay PostgreSQL-compatible (including geometric
        // input validation checks injected at CREATE TABLE time). A primary
        // key by itself is still eligible: NOT NULL is enforced below, and
        // the storage/index layer preflights the backing unique index.
        if !table.foreign_keys.is_empty() || !table.check_constraints.is_empty() {
            if let Some(revision) = cache_revision {
                DIRECT_INSERT_PATH_CACHE.with(|cache| {
                    let mut cache = cache.borrow_mut();
                    if cache.len() >= 256 {
                        cache.clear();
                    }
                    cache.insert((revision, table_id), None);
                });
            }
            return Ok(None);
        }

        let has_insert_trigger = self
            .list_triggers_cached(table_id, &table.name.to_string(), context)?
            .into_iter()
            .any(|trigger| trigger.event == TriggerEventDescriptor::Insert);
        if has_insert_trigger {
            if let Some(revision) = cache_revision {
                DIRECT_INSERT_PATH_CACHE.with(|cache| {
                    let mut cache = cache.borrow_mut();
                    if cache.len() >= 256 {
                        cache.clear();
                    }
                    cache.insert((revision, table_id), None);
                });
            }
            return Ok(None);
        }

        let unique_non_primary_index = self
            .catalog_reader
            .list_indexes(context.txn_id, table_id)?
            .into_iter()
            .any(|index| {
                if !index.unique {
                    return false;
                }
                !table.primary_key.as_ref().is_some_and(|primary_key| {
                    primary_key.len() == index.key_columns.len()
                        && primary_key
                            .iter()
                            .zip(index.key_columns.iter())
                            .all(|(left, right)| *left == right.column_id)
                })
            });
        if unique_non_primary_index {
            if let Some(revision) = cache_revision {
                DIRECT_INSERT_PATH_CACHE.with(|cache| {
                    let mut cache = cache.borrow_mut();
                    if cache.len() >= 256 {
                        cache.clear();
                    }
                    cache.insert((revision, table_id), None);
                });
            }
            return Ok(None);
        }

        if let Some(revision) = cache_revision {
            DIRECT_INSERT_PATH_CACHE.with(|cache| {
                let mut cache = cache.borrow_mut();
                if cache.len() >= 256 {
                    cache.clear();
                }
                cache.insert((revision, table_id), Some(table.clone()));
            });
        }
        Ok(Some(table))
    }

    pub(in crate::executor) fn prefetch_sequence_next_values(
        &self,
        sequence_name: &str,
        row_count: usize,
        context: &ExecutionContext,
    ) -> DbResult<Vec<i64>> {
        let descriptor = self.find_sequence_descriptor(sequence_name, context)?;
        let values =
            self.sequence_manager
                .next_values(context.txn_id, descriptor.sequence_id, row_count)?;
        if let Some(last_value) = values.last().copied() {
            context.record_sequence_next_value(descriptor.sequence_id, last_value)?;
        }
        Ok(values)
    }

    pub(in crate::executor) fn same_next_value_sequence_for_column(
        rows: &[Vec<TypedExpr>],
        column_idx: usize,
    ) -> Option<&str> {
        let mut sequence_name = None;
        for row in rows {
            let TypedExprKind::NextValue {
                sequence_name: current,
            } = &row.get(column_idx)?.kind
            else {
                return None;
            };

            match sequence_name {
                Some(existing) if existing != current => return None,
                Some(_) => {}
                None => sequence_name = Some(current.as_str()),
            }
        }
        sequence_name
    }

    pub(in crate::executor) fn prefetch_insert_values_next_values(
        &self,
        rows: &[Vec<TypedExpr>],
        column_count: usize,
        context: &ExecutionContext,
    ) -> DbResult<Vec<Option<Vec<i64>>>> {
        if rows.len() <= 1 {
            return Ok(Vec::new());
        }

        let mut prefetched = vec![None; column_count];
        for (column_idx, slot) in prefetched.iter_mut().enumerate() {
            let Some(sequence_name) = Self::same_next_value_sequence_for_column(rows, column_idx)
            else {
                continue;
            };
            *slot = Some(self.prefetch_sequence_next_values(sequence_name, rows.len(), context)?);
        }
        Ok(prefetched)
    }
}
