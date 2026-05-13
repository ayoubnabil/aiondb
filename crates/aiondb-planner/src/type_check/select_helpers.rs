use super::*;
use aiondb_core::convert::usize_to_i16_saturating;

pub(super) use aiondb_core::convert::usize_to_u32_saturating;
pub(super) use aiondb_core::convert::usize_to_u64_saturating;

pub(super) fn ensure_integer_limit_offset(expr: &TypedExpr, clause: &str) -> DbResult<()> {
    match &expr.data_type {
        DataType::Int
        | DataType::BigInt
        | DataType::Numeric
        | DataType::Double
        | DataType::Real
        | DataType::Text => Ok(()),
        other => Err(DbError::Bind(Box::new(ErrorReport::new(
            SqlState::SyntaxError,
            format!("{clause} requires an integer expression, got {other}"),
        )))),
    }
}

pub(super) fn supports_text_type_modifier(data_type: &DataType) -> bool {
    matches!(data_type, DataType::Text)
        || matches!(data_type, DataType::Int)
        || matches!(data_type, DataType::Array(inner) if matches!(inner.as_ref(), DataType::Text))
        || matches!(data_type, DataType::Array(inner) if matches!(inner.as_ref(), DataType::Int))
}

pub(super) fn result_text_type_modifier(
    source_expr: &Expr,
    expr: &TypedExpr,
    relation: Option<&TableDescriptor>,
) -> Option<TextTypeModifier> {
    if !supports_text_type_modifier(&expr.data_type) {
        return None;
    }
    if let Some(modifier) = cast_text_type_modifier(source_expr) {
        return Some(modifier);
    }
    let (_, ordinal) = expr.kind.as_column_ref()?;
    relation
        .and_then(|table| table.columns.get(ordinal))
        .and_then(|column| column.text_type_modifier)
}

pub(crate) fn cast_text_type_modifier(expr: &Expr) -> Option<TextTypeModifier> {
    if let Expr::FunctionCall { name, args, .. } = expr {
        if name
            .parts
            .last()
            .is_some_and(|part| part.eq_ignore_ascii_case("__aiondb_char_pad_length"))
        {
            let inner = args.first()?;
            let length = match args.get(1) {
                Some(Expr::Literal(Literal::Integer(length), _)) if *length > 0 => {
                    u32::try_from(*length).ok()?
                }
                _ => return None,
            };
            let hint = expr_fn_helpers::type_hint_name(inner)?;
            if hint.eq_ignore_ascii_case("character") {
                return Some(TextTypeModifier::Char { length });
            }
            return None;
        }
    }

    let hint = expr_fn_helpers::type_hint_name(expr)?;
    if hint.eq_ignore_ascii_case("character varying") {
        Some(TextTypeModifier::VarCharAny)
    } else if hint.eq_ignore_ascii_case("character") {
        Some(TextTypeModifier::BpChar)
    } else {
        None
    }
}

pub(crate) fn describe_select_output_origins(
    select: &BoundSelect,
    outputs: &[ProjectionExpr],
) -> Vec<Option<crate::ResultColumnOrigin>> {
    let origin_lookup = build_select_origin_lookup(select);
    outputs
        .iter()
        .map(|projection| {
            let (_, ordinal) = projection.expr.kind.as_column_ref()?;
            origin_lookup.get(ordinal).copied().flatten()
        })
        .collect()
}

fn build_select_origin_lookup(select: &BoundSelect) -> Vec<Option<crate::ResultColumnOrigin>> {
    let primary_relation = select.relation.as_ref().map(|relation| {
        if select.source.is_some() {
            relation.clone()
        } else {
            compat_relation_with_system_columns(relation)
        }
    });
    let Some(primary_relation) = primary_relation else {
        return Vec::new();
    };

    let join_relations = select
        .joins
        .iter()
        .map(|join| {
            if join.source.is_some() {
                join.relation.clone()
            } else {
                compat_relation_with_system_columns(&join.relation)
            }
        })
        .collect::<Vec<_>>();

    let mut combined_columns = Vec::new();
    let mut origin_lookup = Vec::new();
    let mut alias_entries: Vec<(String, usize)> = Vec::new();
    let mut using_alias_entries: Vec<(String, Vec<ColumnDescriptor>)> = Vec::new();

    let primary_alias = select
        .from_alias
        .clone()
        .unwrap_or_else(|| primary_relation.name.object_name().to_owned());
    let primary_name = primary_relation.name.object_name().to_owned();
    alias_entries.push((primary_name.clone(), 0));
    if !primary_alias.eq_ignore_ascii_case(&primary_name) {
        alias_entries.push((primary_alias.clone(), 0));
    }

    let left_nullable = select
        .joins
        .iter()
        .any(|join| matches!(join.join_type, AstJoinType::Right | AstJoinType::Full));
    let primary_origin_len = select
        .relation
        .as_ref()
        .map_or(0, |relation| relation.columns.len());
    for (index, column) in primary_relation.columns.iter().enumerate() {
        combined_columns.push(ColumnDescriptor {
            column_id: column.column_id,
            name: column.name.clone(),
            data_type: column.data_type.clone(),
            raw_type_name: None,
            text_type_modifier: column.text_type_modifier,
            nullable: column.nullable || left_nullable,
            ordinal_position: usize_to_u32_saturating(combined_columns.len()).saturating_add(1),
            default_value: column.default_value.clone(),
        });
        origin_lookup.push(if select.source.is_none() && index < primary_origin_len {
            Some(crate::ResultColumnOrigin {
                relation_id: primary_relation.table_id,
                column_attr: usize_to_i16_saturating(index.saturating_add(1)),
            })
        } else {
            None
        });
    }

    for (bound_join, join_relation) in select.joins.iter().zip(join_relations.iter()) {
        let join_start = combined_columns.len();
        if let Some(using_alias) = &bound_join.using_alias {
            let using_columns = bound_join
                .using_columns
                .iter()
                .filter_map(|column_name| {
                    combined_columns[..join_start]
                        .iter()
                        .find(|column| column.name.eq_ignore_ascii_case(column_name))
                        .cloned()
                })
                .collect::<Vec<_>>();
            if !using_columns.is_empty() {
                using_alias_entries.push((using_alias.clone(), using_columns));
            }
        }

        let join_alias = bound_join
            .alias
            .clone()
            .unwrap_or_else(|| join_relation.name.object_name().to_owned());
        let join_name = join_relation.name.object_name().to_owned();
        let join_has_explicit_alias = bound_join
            .alias
            .as_ref()
            .is_some_and(|alias| !alias.eq_ignore_ascii_case(&join_name));
        if !join_has_explicit_alias {
            let name_already_registered = alias_entries
                .iter()
                .any(|(alias, _)| alias.eq_ignore_ascii_case(&join_name));
            if !name_already_registered {
                alias_entries.push((join_name.clone(), combined_columns.len()));
            }
        }
        if !join_alias.eq_ignore_ascii_case(&join_name) {
            alias_entries.push((join_alias.clone(), combined_columns.len()));
        }

        let join_origin_len = bound_join.relation.columns.len();
        for (index, column) in join_relation.columns.iter().enumerate() {
            let is_using_column = bound_join
                .using_columns
                .iter()
                .any(|uc| uc.eq_ignore_ascii_case(&column.name));
            let column_name = if is_using_column {
                format!("{join_alias}\x00{}", column.name)
            } else {
                column.name.clone()
            };
            combined_columns.push(ColumnDescriptor {
                column_id: column.column_id,
                name: column_name,
                data_type: column.data_type.clone(),
                raw_type_name: None,
                text_type_modifier: column.text_type_modifier,
                nullable: column.nullable
                    || matches!(bound_join.join_type, AstJoinType::Left | AstJoinType::Full),
                ordinal_position: usize_to_u32_saturating(combined_columns.len()).saturating_add(1),
                default_value: column.default_value.clone(),
            });
            origin_lookup.push(if bound_join.source.is_none() && index < join_origin_len {
                Some(crate::ResultColumnOrigin {
                    relation_id: bound_join.relation.table_id,
                    column_attr: usize_to_i16_saturating(index.saturating_add(1)),
                })
            } else {
                None
            });
        }
    }

    let base_len = combined_columns.len();
    for (idx, (alias, start)) in alias_entries.iter().enumerate() {
        let end = alias_entries
            .iter()
            .skip(idx + 1)
            .find(|(_, next_start)| *next_start > *start)
            .map_or(base_len, |(_, next_start)| *next_start);
        for source_index in *start..end {
            let source_column = &combined_columns[source_index];
            let bare_name = source_column
                .name
                .rsplit('\0')
                .next()
                .unwrap_or(&source_column.name)
                .to_owned();
            let column_id = source_column.column_id;
            let data_type = source_column.data_type.clone();
            let text_type_modifier = source_column.text_type_modifier;
            let nullable = source_column.nullable;
            let ordinal_position = source_column.ordinal_position;
            let default_value = source_column.default_value.clone();
            let origin_index =
                usize::try_from(ordinal_position.saturating_sub(1)).unwrap_or(usize::MAX);
            combined_columns.push(ColumnDescriptor {
                column_id,
                name: format!("{alias}\x00{bare_name}"),
                data_type,
                raw_type_name: None,
                text_type_modifier,
                nullable,
                ordinal_position,
                default_value,
            });
            origin_lookup.push(origin_lookup.get(origin_index).copied().flatten());
        }
    }

    for (alias, columns) in using_alias_entries {
        for source_column in columns {
            let bare_name = source_column
                .name
                .rsplit('\0')
                .next()
                .unwrap_or(&source_column.name);
            combined_columns.push(ColumnDescriptor {
                column_id: source_column.column_id,
                name: format!("{alias}\x00{bare_name}"),
                data_type: source_column.data_type.clone(),
                raw_type_name: None,
                text_type_modifier: source_column.text_type_modifier,
                nullable: source_column.nullable,
                ordinal_position: source_column.ordinal_position,
                default_value: source_column.default_value.clone(),
            });
            let origin_index = usize::try_from(source_column.ordinal_position.saturating_sub(1))
                .unwrap_or(usize::MAX);
            origin_lookup.push(origin_lookup.get(origin_index).copied().flatten());
        }
    }

    origin_lookup
}

#[inline]
pub(super) fn ordinal_to_index(ordinal_position: u32) -> usize {
    usize::try_from(ordinal_position.saturating_sub(1)).unwrap_or(usize::MAX)
}

#[inline]
pub(super) fn order_by_position_to_index(position: i64, len: usize) -> Option<usize> {
    let one_based = usize::try_from(position).ok()?;
    if one_based == 0 || one_based > len {
        None
    } else {
        Some(one_based - 1)
    }
}
