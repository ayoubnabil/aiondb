use super::*;
use crate::binary_format::resolve_result_format_code;
use aiondb_core::AIONDB_VECTOR_TYPE_OID;
use aiondb_parser::{Expr, Statement};
use std::convert::TryFrom;
use std::sync::Arc;

/// Convert a `ResultColumn` to a `FieldDescription`, respecting the client's
/// requested result format codes from the Bind message.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn result_column_to_field_fmt(
    col: &aiondb_engine::ResultColumn,
    result_formats: &[i16],
    col_index: usize,
) -> FieldDescription {
    result_column_to_field_with_origin_fmt(col, None, result_formats, col_index)
}

#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn result_columns_to_fields(
    columns: &[aiondb_engine::ResultColumn],
    origins: &[Option<aiondb_engine::ResultColumnOrigin>],
    result_formats: &[i16],
) -> Vec<FieldDescription> {
    columns
        .iter()
        .enumerate()
        .map(|(i, col)| {
            result_column_to_field_with_origin_fmt(
                col,
                origins.get(i).and_then(|origin| origin.as_ref()),
                result_formats,
                i,
            )
        })
        .collect()
}

pub(super) fn write_row_description_from_result_columns(
    w: &mut MessageWriter,
    columns: &[aiondb_engine::ResultColumn],
    origins: &[Option<aiondb_engine::ResultColumnOrigin>],
    result_formats: &[i16],
) -> Result<(), DbError> {
    let field_count = i16::try_from(columns.len()).map_err(|_| {
        DbError::internal(format!(
            "too many columns in result set ({}, maximum is {})",
            columns.len(),
            i16::MAX
        ))
    })?;
    let pos = w.begin(b'T');
    w.put_i16(field_count);
    for (index, column) in columns.iter().enumerate() {
        let origin = origins.get(index).and_then(|origin| origin.as_ref());
        let (type_oid, type_size, type_modifier) = result_column_type_info(column);
        w.put_cstring(&column.name);
        w.put_u32(origin.map_or(0, |origin| relation_id_to_pg_oid(origin.relation_id)));
        w.put_i16(origin.map_or(0, |origin| origin.column_attr));
        w.put_u32(type_oid);
        w.put_i16(type_size);
        w.put_i32(type_modifier);
        w.put_i16(resolve_result_format_code(
            &column.data_type,
            result_formats,
            index,
        ));
    }
    w.try_finish(pos)?;
    Ok(())
}

fn result_column_to_field_with_origin_fmt(
    col: &aiondb_engine::ResultColumn,
    origin: Option<&aiondb_engine::ResultColumnOrigin>,
    result_formats: &[i16],
    col_index: usize,
) -> FieldDescription {
    let (type_oid, type_size, type_modifier) = result_column_type_info(col);
    FieldDescription {
        name: col.name.clone(),
        table_oid: origin.map_or(0, |origin| relation_id_to_pg_oid(origin.relation_id)),
        column_attr: origin.map_or(0, |origin| origin.column_attr),
        type_oid,
        type_size,
        type_modifier,
        format_code: resolve_result_format_code(&col.data_type, result_formats, col_index),
    }
}

fn relation_id_to_pg_oid(relation_id: aiondb_core::RelationId) -> u32 {
    let raw = relation_id.get();
    raw.checked_add(16_384)
        .and_then(|oid| u32::try_from(oid).ok())
        .unwrap_or(0)
}

fn result_column_type_info(col: &aiondb_engine::ResultColumn) -> (u32, i16, i32) {
    let (type_oid, type_size) = data_type_to_pg(&col.data_type);
    if let Some(text_type_modifier) = col.text_type_modifier {
        match (&col.data_type, text_type_modifier) {
            (DataType::Text, _) => {
                return (
                    text_type_modifier.scalar_type_oid(),
                    type_size,
                    text_type_modifier.atttypmod(),
                );
            }
            (DataType::Array(inner), _) if matches!(inner.as_ref(), DataType::Text) => {
                return (
                    text_type_modifier.array_type_oid(),
                    type_size,
                    text_type_modifier.atttypmod(),
                );
            }
            (DataType::Int, modifier) if is_int_alias_modifier(modifier) => {
                return (
                    text_type_modifier.scalar_type_oid(),
                    type_size,
                    text_type_modifier.atttypmod(),
                );
            }
            (DataType::Array(inner), modifier)
                if is_int_alias_modifier(modifier) && matches!(inner.as_ref(), DataType::Int) =>
            {
                return (
                    text_type_modifier.array_type_oid(),
                    type_size,
                    text_type_modifier.atttypmod(),
                );
            }
            (DataType::Array(inner), modifier)
                if is_int_vector_alias_modifier(modifier)
                    && matches!(inner.as_ref(), DataType::Int) =>
            {
                return (
                    text_type_modifier.scalar_type_oid(),
                    type_size,
                    text_type_modifier.atttypmod(),
                );
            }
            _ => {}
        }
    }
    (type_oid, type_size, -1)
}

fn is_int_alias_modifier(modifier: aiondb_core::TextTypeModifier) -> bool {
    matches!(
        modifier,
        aiondb_core::TextTypeModifier::Oid
            | aiondb_core::TextTypeModifier::RegProc
            | aiondb_core::TextTypeModifier::RegProcedure
            | aiondb_core::TextTypeModifier::RegOper
            | aiondb_core::TextTypeModifier::RegOperator
            | aiondb_core::TextTypeModifier::RegClass
            | aiondb_core::TextTypeModifier::RegType
            | aiondb_core::TextTypeModifier::RegConfig
            | aiondb_core::TextTypeModifier::RegDictionary
            | aiondb_core::TextTypeModifier::RegNamespace
            | aiondb_core::TextTypeModifier::RegRole
            | aiondb_core::TextTypeModifier::RegCollation
    )
}

fn is_int_vector_alias_modifier(modifier: aiondb_core::TextTypeModifier) -> bool {
    matches!(
        modifier,
        aiondb_core::TextTypeModifier::Int2Vector | aiondb_core::TextTypeModifier::OidVector
    )
}

pub(super) fn direct_param_result_alias_slots(
    statement: &Statement,
) -> Option<Arc<[Option<usize>]>> {
    let Statement::Select(select) = statement else {
        return None;
    };
    Some(
        select
            .items
            .iter()
            .map(|item| direct_param_projection_index(&item.expr))
            .collect::<Vec<_>>()
            .into(),
    )
}

pub(super) fn apply_direct_param_result_aliases(
    alias_slots: Option<&[Option<usize>]>,
    param_oids: &[u32],
    result_columns: &[aiondb_engine::ResultColumn],
) -> Vec<aiondb_engine::ResultColumn> {
    let Some(alias_slots) = alias_slots else {
        return result_columns.to_vec();
    };

    result_columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            let Some(param_index) = alias_slots.get(index).copied().flatten() else {
                return column.clone();
            };
            let Some(param_oid) = param_oids.get(param_index.saturating_sub(1)).copied() else {
                return column.clone();
            };
            let Some(text_type_modifier) =
                result_alias_text_type_modifier(param_oid, &column.data_type)
            else {
                return column.clone();
            };

            let mut column = column.clone();
            column.text_type_modifier = Some(text_type_modifier);
            column
        })
        .collect()
}

pub(super) fn preserve_direct_param_oids(
    _query: &str,
    param_oids: &[u32],
    param_count: usize,
) -> Vec<u32> {
    let mut preserved = vec![0; param_count];
    for (slot, oid) in preserved.iter_mut().zip(param_oids.iter().copied()) {
        *slot = oid;
    }

    preserved
}

fn direct_param_projection_index(expr: &Expr) -> Option<usize> {
    match expr {
        Expr::Parameter { index, .. } => Some(*index),
        _ => None,
    }
}

fn result_alias_text_type_modifier(
    oid: u32,
    data_type: &DataType,
) -> Option<aiondb_core::TextTypeModifier> {
    match (oid, data_type) {
        (18, DataType::Text) => Some(aiondb_core::TextTypeModifier::InternalChar),
        (1002, DataType::Array(inner)) if matches!(inner.as_ref(), DataType::Text) => {
            Some(aiondb_core::TextTypeModifier::InternalChar)
        }
        (19, DataType::Text) => Some(aiondb_core::TextTypeModifier::Name),
        (1003, DataType::Array(inner)) if matches!(inner.as_ref(), DataType::Text) => {
            Some(aiondb_core::TextTypeModifier::Name)
        }
        (26, DataType::Int) => Some(aiondb_core::TextTypeModifier::Oid),
        (1028, DataType::Array(inner)) if matches!(inner.as_ref(), DataType::Int) => {
            Some(aiondb_core::TextTypeModifier::Oid)
        }
        (24, DataType::Int) => Some(aiondb_core::TextTypeModifier::RegProc),
        (1008, DataType::Array(inner)) if matches!(inner.as_ref(), DataType::Int) => {
            Some(aiondb_core::TextTypeModifier::RegProc)
        }
        (2206, DataType::Int) => Some(aiondb_core::TextTypeModifier::RegType),
        (2211, DataType::Array(inner)) if matches!(inner.as_ref(), DataType::Int) => {
            Some(aiondb_core::TextTypeModifier::RegType)
        }
        (2202, DataType::Int) => Some(aiondb_core::TextTypeModifier::RegProcedure),
        (2207, DataType::Array(inner)) if matches!(inner.as_ref(), DataType::Int) => {
            Some(aiondb_core::TextTypeModifier::RegProcedure)
        }
        (2203, DataType::Int) => Some(aiondb_core::TextTypeModifier::RegOper),
        (2208, DataType::Array(inner)) if matches!(inner.as_ref(), DataType::Int) => {
            Some(aiondb_core::TextTypeModifier::RegOper)
        }
        (2204, DataType::Int) => Some(aiondb_core::TextTypeModifier::RegOperator),
        (2209, DataType::Array(inner)) if matches!(inner.as_ref(), DataType::Int) => {
            Some(aiondb_core::TextTypeModifier::RegOperator)
        }
        (2205, DataType::Int) => Some(aiondb_core::TextTypeModifier::RegClass),
        (2210, DataType::Array(inner)) if matches!(inner.as_ref(), DataType::Int) => {
            Some(aiondb_core::TextTypeModifier::RegClass)
        }
        (3734, DataType::Int) => Some(aiondb_core::TextTypeModifier::RegConfig),
        (3735, DataType::Array(inner)) if matches!(inner.as_ref(), DataType::Int) => {
            Some(aiondb_core::TextTypeModifier::RegConfig)
        }
        (3769, DataType::Int) => Some(aiondb_core::TextTypeModifier::RegDictionary),
        (3770, DataType::Array(inner)) if matches!(inner.as_ref(), DataType::Int) => {
            Some(aiondb_core::TextTypeModifier::RegDictionary)
        }
        (4089, DataType::Int) => Some(aiondb_core::TextTypeModifier::RegNamespace),
        (4090, DataType::Array(inner)) if matches!(inner.as_ref(), DataType::Int) => {
            Some(aiondb_core::TextTypeModifier::RegNamespace)
        }
        (4096, DataType::Int) => Some(aiondb_core::TextTypeModifier::RegRole),
        (4097, DataType::Array(inner)) if matches!(inner.as_ref(), DataType::Int) => {
            Some(aiondb_core::TextTypeModifier::RegRole)
        }
        (4191, DataType::Int) => Some(aiondb_core::TextTypeModifier::RegCollation),
        (4192, DataType::Array(inner)) if matches!(inner.as_ref(), DataType::Int) => {
            Some(aiondb_core::TextTypeModifier::RegCollation)
        }
        (1042, DataType::Text) => Some(aiondb_core::TextTypeModifier::BpChar),
        (1014, DataType::Array(inner)) if matches!(inner.as_ref(), DataType::Text) => {
            Some(aiondb_core::TextTypeModifier::BpChar)
        }
        (1043, DataType::Text) => Some(aiondb_core::TextTypeModifier::VarCharAny),
        (1015, DataType::Array(inner)) if matches!(inner.as_ref(), DataType::Text) => {
            Some(aiondb_core::TextTypeModifier::VarCharAny)
        }
        _ => None,
    }
}

/// Map a `DataType` to its `PostgreSQL` OID and type size.
pub(super) fn data_type_to_pg(dt: &DataType) -> (u32, i16) {
    let oid = match dt {
        DataType::Vector { .. } => AIONDB_VECTOR_TYPE_OID,
        _ => dt.pg_oid().unwrap_or(25),
    };
    let size = match dt {
        DataType::Int | DataType::Real => 4,
        DataType::BigInt | DataType::Double => 8,
        DataType::Tid => 6,
        DataType::TimeTz => 12,
        DataType::Boolean => 1,
        _ => -1, // Variable length.
    };
    (oid, size)
}

/// Map a `PostgreSQL` parameter type OID from `Parse` to an internal `DataType`.
///
/// OID 0 is handled by callers as "unspecified" and should not be passed here.
pub(super) fn pg_oid_to_data_type(oid: u32) -> Option<DataType> {
    match oid {
        16 => Some(DataType::Boolean),
        17 => Some(DataType::Blob),
        20 => Some(DataType::BigInt),
        21 | 23 | 24 | 26 | 2202 | 2203 | 2204 | 2205 | 2206 | 3734 | 3769 | 4089 | 4096 | 4191 => {
            Some(DataType::Int)
        }
        18 | 19 | 25 | 705 | 1042 | 1043 | 2275 => Some(DataType::Text),
        27 => Some(DataType::Tid),
        700 => Some(DataType::Real),
        701 => Some(DataType::Double),
        774 => Some(DataType::MacAddr8),
        790 => Some(DataType::Money),
        829 => Some(DataType::MacAddr),
        1082 => Some(DataType::Date),
        1083 => Some(DataType::Time),
        1114 => Some(DataType::Timestamp),
        1184 => Some(DataType::TimestampTz),
        1186 => Some(DataType::Interval),
        1266 => Some(DataType::TimeTz),
        1700 => Some(DataType::Numeric),
        2950 => Some(DataType::Uuid),
        3220 => Some(DataType::PgLsn),
        114 | 3802 => Some(DataType::Jsonb),
        62_000 => Some(DataType::Vector {
            dims: 0,
            element_type: aiondb_core::VectorElementType::Float32,
        }),
        // Common array OIDs.
        1000 => Some(DataType::Array(Box::new(DataType::Boolean))),
        1001 => Some(DataType::Array(Box::new(DataType::Blob))),
        1002 | 1003 => Some(DataType::Array(Box::new(DataType::Text))),
        1005 | 1007 | 1008 | 1028 | 2207 | 2208 | 2209 | 2210 | 2211 | 3735 | 3770 | 4090
        | 4097 | 4192 => Some(DataType::Array(Box::new(DataType::Int))),
        1010 => Some(DataType::Array(Box::new(DataType::Tid))),
        1009 | 1014 | 1015 => Some(DataType::Array(Box::new(DataType::Text))),
        1016 => Some(DataType::Array(Box::new(DataType::BigInt))),
        1021 => Some(DataType::Array(Box::new(DataType::Real))),
        1022 => Some(DataType::Array(Box::new(DataType::Double))),
        1040 => Some(DataType::Array(Box::new(DataType::MacAddr))),
        1115 => Some(DataType::Array(Box::new(DataType::Timestamp))),
        1183 => Some(DataType::Array(Box::new(DataType::Time))),
        1182 => Some(DataType::Array(Box::new(DataType::Date))),
        1185 => Some(DataType::Array(Box::new(DataType::TimestampTz))),
        1187 => Some(DataType::Array(Box::new(DataType::Interval))),
        1231 => Some(DataType::Array(Box::new(DataType::Numeric))),
        1270 => Some(DataType::Array(Box::new(DataType::TimeTz))),
        2951 => Some(DataType::Array(Box::new(DataType::Uuid))),
        3221 => Some(DataType::Array(Box::new(DataType::PgLsn))),
        3807 => Some(DataType::Array(Box::new(DataType::Jsonb))),
        791 => Some(DataType::Array(Box::new(DataType::Money))),
        _ => None,
    }
}

/// Write `ParameterDescription` + `RowDescription` (or `NoData`) for a prepared statement.
pub(super) fn write_stmt_description(
    w: &mut MessageWriter,
    desc: &PreparedStatementDesc,
    explicit_param_oids: Option<&[u32]>,
) -> Result<(), aiondb_core::DbError> {
    // ParameterDescription
    let param_oids: Vec<u32> = match explicit_param_oids {
        Some(oids) if oids.len() <= desc.param_types.len() => desc
            .param_types
            .iter()
            .enumerate()
            .map(|(index, dt)| match oids.get(index).copied() {
                Some(0) | None => data_type_to_pg(dt).0,
                Some(oid) => oid,
            })
            .collect(),
        _ => desc
            .param_types
            .iter()
            .map(|dt| data_type_to_pg(dt).0)
            .collect(),
    };
    messages::write_parameter_description(w, &param_oids)?;

    // RowDescription or NoData
    if desc.result_columns.is_empty() {
        messages::write_no_data(w);
    } else {
        write_row_description_from_result_columns(
            w,
            &desc.result_columns,
            &desc.result_column_origins,
            &[],
        )?;
    }
    Ok(())
}

pub(super) fn validate_bind_formats(
    param_formats: &[i16],
    param_count: usize,
) -> Result<(), DbError> {
    validate_formats(
        param_formats,
        param_count,
        "bind parameter",
        "bind parameter format count mismatch",
    )
}

pub(super) fn validate_result_formats(
    result_formats: &[i16],
    result_column_count: usize,
) -> Result<(), DbError> {
    validate_formats(
        result_formats,
        result_column_count,
        "result column",
        "result format count mismatch",
    )
}

fn validate_formats(
    formats: &[i16],
    expected_count: usize,
    context: &str,
    mismatch_context: &str,
) -> Result<(), DbError> {
    match formats.len() {
        0 => Ok(()),
        1 => validate_format_code(formats[0], context),
        len if len == expected_count => formats
            .iter()
            .try_for_each(|format| validate_format_code(*format, context)),
        len => Err(DbError::protocol(format!(
            "{mismatch_context}: expected 0, 1, or {expected_count}, got {len}"
        ))),
    }
}

pub(super) fn validate_format_code(format_code: i16, context: &str) -> Result<(), DbError> {
    match format_code {
        0 | 1 => Ok(()),
        _ => Err(DbError::protocol(format!(
            "unknown {context} format code: {format_code}"
        ))),
    }
}
