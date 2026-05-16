use std::collections::HashMap;

use aiondb_core::{DbError, DbResult, Value};
use aiondb_eval::{build_hash_key, ValueHashKey};
use aiondb_graph::algorithms::procedures::{
    procedure_info, AlgorithmConfig, AlgorithmConfigField, ProcedureArgumentType,
};
use aiondb_plan::{TypedExpr, TypedExprKind};

pub(crate) fn literal_arg_value<'a>(procedure: &str, arg: &'a TypedExpr) -> DbResult<&'a Value> {
    let TypedExprKind::Literal(value) = &arg.kind else {
        return Err(DbError::feature_not_supported(format!(
            "CALL {procedure} arguments must be literals in native Cypher for now"
        )));
    };
    Ok(value)
}

pub(crate) fn value_to_usize_arg(
    procedure: &str,
    value: &Value,
    arg_name: &str,
) -> DbResult<usize> {
    let parsed = match value {
        Value::Int(value) => i64::from(*value),
        Value::BigInt(value) => *value,
        other => {
            return Err(DbError::syntax_error(format!(
                "CALL {procedure} argument {arg_name} requires an integer, got {other:?}"
            )));
        }
    };
    usize::try_from(parsed).map_err(|_| {
        DbError::syntax_error(format!(
            "CALL {procedure} argument {arg_name} must be non-negative"
        ))
    })
}

pub(crate) fn value_to_f64_arg(procedure: &str, value: &Value, arg_name: &str) -> DbResult<f64> {
    match value {
        Value::Int(value) => value.to_string().parse::<f64>().map_err(|error| {
            DbError::syntax_error(format!(
                "CALL {procedure} argument {arg_name} is not a valid number: {error}"
            ))
        }),
        Value::BigInt(value) => value.to_string().parse::<f64>().map_err(|error| {
            DbError::syntax_error(format!(
                "CALL {procedure} argument {arg_name} is not a valid number: {error}"
            ))
        }),
        Value::Real(value) => Ok(f64::from(*value)),
        Value::Double(value) => Ok(*value),
        Value::Numeric(value) => value.to_string().parse::<f64>().map_err(|error| {
            DbError::syntax_error(format!(
                "CALL {procedure} argument {arg_name} is not a valid number: {error}"
            ))
        }),
        other => Err(DbError::syntax_error(format!(
            "CALL {procedure} argument {arg_name} requires a number, got {other:?}"
        ))),
    }
}

pub(crate) fn value_to_string_arg(
    procedure: &str,
    value: &Value,
    arg_name: &str,
) -> DbResult<String> {
    match value {
        Value::Text(value) => Ok(value.clone()),
        other => Err(DbError::syntax_error(format!(
            "CALL {procedure} argument {arg_name} requires a string, got {other:?}"
        ))),
    }
}

pub(crate) fn value_to_u32_array_arg(
    procedure: &str,
    value: &Value,
    arg_name: &str,
) -> DbResult<Vec<u32>> {
    let Value::Array(values) = value else {
        return Err(DbError::syntax_error(format!(
            "CALL {procedure} argument {arg_name} requires an integer array, got {value:?}"
        )));
    };
    values
        .iter()
        .map(|value| {
            let parsed = match value {
                Value::Int(value) => i64::from(*value),
                Value::BigInt(value) => *value,
                other => {
                    return Err(DbError::syntax_error(format!(
                        "CALL {procedure} argument {arg_name} requires an integer array, got element {other:?}"
                    )));
                }
            };
            u32::try_from(parsed).map_err(|_| {
                DbError::syntax_error(format!(
                    "CALL {procedure} argument {arg_name} elements must fit u32"
                ))
            })
        })
        .collect()
}

fn exact_integer_value(value: &Value) -> Option<i128> {
    match value {
        Value::Int(value) => Some(i128::from(*value)),
        Value::BigInt(value) => Some(i128::from(*value)),
        Value::Numeric(value) if !value.is_big() => {
            if value.scale == 0 {
                return Some(value.coefficient);
            }
            let mut divisor: i128 = 1;
            for _ in 0..value.scale {
                divisor = divisor.checked_mul(10)?;
            }
            (value.coefficient % divisor == 0).then_some(value.coefficient / divisor)
        }
        _ => None,
    }
}

fn graph_node_id_values_match(expected: &Value, actual: &Value) -> bool {
    expected == actual
        || matches!(
            (exact_integer_value(expected), exact_integer_value(actual)),
            (Some(left), Some(right)) if left == right
        )
}

pub(crate) fn value_to_node_index_arg(
    procedure: &str,
    value: &Value,
    arg_name: &str,
    node_value_indexes: &HashMap<ValueHashKey, Vec<u32>>,
    node_ids: &[Value],
) -> DbResult<u32> {
    let value_key = build_hash_key(value)?;
    let mut candidate_indexes = node_value_indexes
        .get(&value_key)
        .cloned()
        .unwrap_or_else(|| {
            node_ids
                .iter()
                .enumerate()
                .filter_map(|(index, candidate)| {
                    graph_node_id_values_match(value, candidate)
                        .then(|| u32::try_from(index).ok())
                        .flatten()
                })
                .collect()
        })
        .into_iter();
    let mut matches = candidate_indexes.by_ref().filter(|&index| {
        usize::try_from(index)
            .ok()
            .and_then(|index| node_ids.get(index))
            .is_some_and(|candidate| graph_node_id_values_match(value, candidate))
    });
    let Some(first) = matches.next() else {
        return Err(DbError::syntax_error(format!(
            "CALL {procedure} argument {arg_name} references unknown node id {value:?}"
        )));
    };
    if matches.next().is_some() {
        return Err(DbError::syntax_error(format!(
            "CALL {procedure} argument {arg_name} references ambiguous node id {value:?}"
        )));
    }
    u32::try_from(first).map_err(|_| {
        DbError::program_limit("Cypher graph procedure node index exceeds u32 capacity")
    })
}

pub(crate) fn algorithm_config_from_args(
    procedure: &str,
    args: &[TypedExpr],
    node_value_indexes: &HashMap<ValueHashKey, Vec<u32>>,
    node_ids: &[Value],
) -> DbResult<AlgorithmConfig> {
    let info = procedure_info(procedure).ok_or_else(|| {
        DbError::internal(format!(
            "CALL {procedure} reached executor without registry metadata"
        ))
    })?;
    if args.len() > info.args.len() {
        if info.args.is_empty() {
            return Err(DbError::syntax_error(format!(
                "CALL {procedure} does not accept algorithm config arguments"
            )));
        }
        return Err(DbError::syntax_error(format!(
            "CALL {procedure} accepts at most {} algorithm config arguments",
            info.args.len()
        )));
    }

    let mut config = AlgorithmConfig::default();
    for (arg, arg_info) in args.iter().zip(info.args.iter()) {
        let value = literal_arg_value(procedure, arg)?;
        match (arg_info.config_field, arg_info.value_type) {
            (AlgorithmConfigField::MaxIterations, ProcedureArgumentType::NonNegativeInteger) => {
                config.max_iterations = Some(value_to_usize_arg(procedure, value, &arg_info.name)?);
            }
            (AlgorithmConfigField::Damping, ProcedureArgumentType::Float) => {
                config.damping = Some(value_to_f64_arg(procedure, value, &arg_info.name)?);
            }
            (AlgorithmConfigField::Tolerance, ProcedureArgumentType::Float) => {
                config.tolerance = Some(value_to_f64_arg(procedure, value, &arg_info.name)?);
            }
            (AlgorithmConfigField::Resolution, ProcedureArgumentType::Float) => {
                config.resolution = Some(value_to_f64_arg(procedure, value, &arg_info.name)?);
            }
            (AlgorithmConfigField::MinModularityGain, ProcedureArgumentType::Float) => {
                config.min_modularity_gain =
                    Some(value_to_f64_arg(procedure, value, &arg_info.name)?);
            }
            (AlgorithmConfigField::ReturnParam, ProcedureArgumentType::Float) => {
                config.return_param = Some(value_to_f64_arg(procedure, value, &arg_info.name)?);
            }
            (AlgorithmConfigField::InOutParam, ProcedureArgumentType::Float) => {
                config.in_out_param = Some(value_to_f64_arg(procedure, value, &arg_info.name)?);
            }
            (AlgorithmConfigField::WeightColumn, ProcedureArgumentType::String) => {
                config.weight_column = Some(value_to_string_arg(procedure, value, &arg_info.name)?);
            }
            (AlgorithmConfigField::Metric, ProcedureArgumentType::String) => {
                config.metric = Some(value_to_string_arg(procedure, value, &arg_info.name)?);
            }
            (AlgorithmConfigField::TopK, ProcedureArgumentType::NonNegativeInteger) => {
                config.top_k = Some(value_to_usize_arg(procedure, value, &arg_info.name)?);
            }
            (
                AlgorithmConfigField::EmbeddingDimension,
                ProcedureArgumentType::NonNegativeInteger,
            ) => {
                config.embedding_dimension =
                    Some(value_to_usize_arg(procedure, value, &arg_info.name)?);
            }
            (AlgorithmConfigField::SourceNode, ProcedureArgumentType::NodeId) => {
                config.source_node = Some(value_to_node_index_arg(
                    procedure,
                    value,
                    &arg_info.name,
                    node_value_indexes,
                    node_ids,
                )?);
            }
            (AlgorithmConfigField::TargetNode, ProcedureArgumentType::NodeId) => {
                config.target_node = Some(value_to_node_index_arg(
                    procedure,
                    value,
                    &arg_info.name,
                    node_value_indexes,
                    node_ids,
                )?);
            }
            (AlgorithmConfigField::MaxDepth, ProcedureArgumentType::NonNegativeInteger) => {
                config.max_depth = Some(value_to_usize_arg(procedure, value, &arg_info.name)?);
            }
            (AlgorithmConfigField::WalkLength, ProcedureArgumentType::NonNegativeInteger) => {
                config.walk_length = Some(value_to_usize_arg(procedure, value, &arg_info.name)?);
            }
            (AlgorithmConfigField::WalksPerNode, ProcedureArgumentType::NonNegativeInteger) => {
                config.walks_per_node = Some(value_to_usize_arg(procedure, value, &arg_info.name)?);
            }
            (AlgorithmConfigField::RandomSeed, ProcedureArgumentType::NonNegativeInteger) => {
                let seed = value_to_usize_arg(procedure, value, &arg_info.name)?;
                config.random_seed = Some(u64::try_from(seed).map_err(|_| {
                    DbError::program_limit(
                        "Cypher graph procedure random seed exceeds u64 capacity",
                    )
                })?);
            }
            (AlgorithmConfigField::Communities, ProcedureArgumentType::NonNegativeIntegerArray) => {
                config.communities =
                    Some(value_to_u32_array_arg(procedure, value, &arg_info.name)?);
            }
            _ => {
                return Err(DbError::internal(format!(
                    "CALL {procedure} argument {} has incompatible registry metadata",
                    arg_info.name
                )));
            }
        }
    }
    Ok(config)
}

pub(crate) fn weight_column_arg_from_args(
    procedure: &str,
    args: &[TypedExpr],
) -> DbResult<Option<String>> {
    let Some(info) = procedure_info(procedure) else {
        return Ok(None);
    };
    for (arg, arg_info) in args.iter().zip(info.args.iter()) {
        if arg_info.config_field != AlgorithmConfigField::WeightColumn {
            continue;
        }
        let value = literal_arg_value(procedure, arg)?;
        return value_to_string_arg(procedure, value, &arg_info.name).map(Some);
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_core::numeric::NumericValue;

    #[test]
    fn value_to_node_index_arg_accepts_cross_type_integer_node_ids() {
        let node_ids = vec![Value::Int(1), Value::BigInt(2)];
        let mut node_value_indexes = HashMap::new();
        node_value_indexes.insert(build_hash_key(&Value::Int(1)).unwrap(), vec![0]);
        node_value_indexes.insert(build_hash_key(&Value::BigInt(2)).unwrap(), vec![1]);

        assert_eq!(
            value_to_node_index_arg(
                "graph.dijkstra",
                &Value::BigInt(1),
                "sourceNodeId",
                &node_value_indexes,
                &node_ids,
            )
            .unwrap(),
            0
        );
        assert_eq!(
            value_to_node_index_arg(
                "graph.dijkstra",
                &Value::Numeric(NumericValue::new(2, 0)),
                "targetNodeId",
                &node_value_indexes,
                &node_ids,
            )
            .unwrap(),
            1
        );
    }
}
