use std::sync::Arc;

use aiondb_core::{DbError, DbResult, Value, VectorValue};
use aiondb_graph::algorithms::procedures::AlgorithmResult;

use super::{graph_prealloc_capacity, BoundValue, SharedBoundValue};

fn selected_procedure_value_indexes(
    procedure: &str,
    yields: &[String],
    column_names: &[&str],
) -> DbResult<Vec<usize>> {
    yields
        .iter()
        .map(|requested| {
            column_names
                .iter()
                .position(|name| name.eq_ignore_ascii_case(requested))
                .ok_or_else(|| {
                    DbError::internal(format!(
                        "procedure {procedure} did not produce yielded column {requested}"
                    ))
                })
        })
        .collect()
}

fn selected_procedure_values_from_indexes(
    indexes: &[usize],
    values: &[SharedBoundValue],
) -> Vec<SharedBoundValue> {
    indexes.iter().map(|&index| values[index].clone()).collect()
}

struct SharedNodeIds<'a> {
    node_ids: &'a [Value],
    values: Vec<Option<SharedBoundValue>>,
}

impl<'a> SharedNodeIds<'a> {
    fn new(node_ids: &'a [Value]) -> Self {
        Self {
            node_ids,
            values: vec![None; node_ids.len()],
        }
    }

    fn get(&mut self, node_index: usize) -> Option<SharedBoundValue> {
        let slot = self.values.get_mut(node_index)?;
        if slot.is_none() {
            *slot = Some(Arc::new(BoundValue::Scalar(
                self.node_ids.get(node_index)?.clone(),
            )));
        }
        slot.clone()
    }
}

fn algorithm_result_row_count(result: &AlgorithmResult) -> usize {
    match result {
        AlgorithmResult::NodeScores { scores, .. } => scores.len(),
        AlgorithmResult::NodeDualScores { scores, .. } => scores.len(),
        AlgorithmResult::NodeLabels { labels, .. } => labels.len(),
        AlgorithmResult::NodeCounts { counts, .. } => counts.len(),
        AlgorithmResult::NodeIds { nodes, .. } => nodes.len(),
        AlgorithmResult::NodePairs { pairs, .. } => pairs.len(),
        AlgorithmResult::DegreeDistribution { distribution, .. } => distribution.len(),
        AlgorithmResult::NodePairScores { scores, .. } => scores.len(),
        AlgorithmResult::NodePaths { paths, .. } => paths.len(),
        AlgorithmResult::NodeWalks { walks, .. } => walks.len(),
        AlgorithmResult::NodeEmbeddings { embeddings, .. } => embeddings.len(),
        AlgorithmResult::Scalar { .. } | AlgorithmResult::ScalarU64 { .. } => 1,
    }
}

pub(super) fn procedure_result_bindings(
    procedure: &str,
    yields: &[String],
    results: &[AlgorithmResult],
    node_ids: &[Value],
) -> DbResult<Vec<Vec<SharedBoundValue>>> {
    let estimated_rows = results
        .iter()
        .map(algorithm_result_row_count)
        .fold(0usize, usize::saturating_add);
    let mut shared_node_ids = SharedNodeIds::new(node_ids);
    let mut rows = Vec::with_capacity(graph_prealloc_capacity(estimated_rows));
    for result in results {
        match result {
            AlgorithmResult::NodeScores { column, scores } => {
                let selected = selected_procedure_value_indexes(
                    procedure,
                    yields,
                    &["nodeId", column.as_str()],
                )?;
                for (node_index, score) in scores.iter().enumerate() {
                    let node_id = shared_node_ids.get(node_index).ok_or_else(|| {
                        DbError::internal(format!(
                            "procedure {procedure} produced result for unknown node index {node_index}"
                        ))
                    })?;
                    let score = Arc::new(BoundValue::Scalar(Value::Double(*score)));
                    match selected.as_slice() {
                        [0, 1] => rows.push(vec![node_id, score]),
                        [1, 0] => rows.push(vec![score, node_id]),
                        _ => {
                            let values = [node_id, score];
                            rows.push(selected_procedure_values_from_indexes(&selected, &values));
                        }
                    }
                }
            }
            AlgorithmResult::NodeDualScores {
                first_column,
                second_column,
                scores,
            } => {
                let selected = selected_procedure_value_indexes(
                    procedure,
                    yields,
                    &["nodeId", first_column.as_str(), second_column.as_str()],
                )?;
                for (node_index, (first, second)) in scores.iter().enumerate() {
                    let node_id = shared_node_ids.get(node_index).ok_or_else(|| {
                        DbError::internal(format!(
                            "procedure {procedure} produced result for unknown node index {node_index}"
                        ))
                    })?;
                    let values = [
                        node_id,
                        Arc::new(BoundValue::Scalar(Value::Double(*first))),
                        Arc::new(BoundValue::Scalar(Value::Double(*second))),
                    ];
                    rows.push(selected_procedure_values_from_indexes(&selected, &values));
                }
            }
            AlgorithmResult::NodeLabels { column, labels } => {
                let selected = selected_procedure_value_indexes(
                    procedure,
                    yields,
                    &["nodeId", column.as_str()],
                )?;
                for (node_index, label) in labels.iter().enumerate() {
                    let node_id = shared_node_ids.get(node_index).ok_or_else(|| {
                        DbError::internal(format!(
                            "procedure {procedure} produced result for unknown node index {node_index}"
                        ))
                    })?;
                    let values = [
                        node_id,
                        Arc::new(BoundValue::Scalar(Value::BigInt(i64::from(*label)))),
                    ];
                    rows.push(selected_procedure_values_from_indexes(&selected, &values));
                }
            }
            AlgorithmResult::NodeCounts { column, counts } => {
                let selected = selected_procedure_value_indexes(
                    procedure,
                    yields,
                    &["nodeId", column.as_str()],
                )?;
                for (node_index, count) in counts.iter().enumerate() {
                    let node_id = shared_node_ids.get(node_index).ok_or_else(|| {
                        DbError::internal(format!(
                            "procedure {procedure} produced result for unknown node index {node_index}"
                        ))
                    })?;
                    let values = [
                        node_id,
                        Arc::new(BoundValue::Scalar(Value::BigInt(i64::from(*count)))),
                    ];
                    rows.push(selected_procedure_values_from_indexes(&selected, &values));
                }
            }
            AlgorithmResult::NodeIds { column, nodes } => {
                let selected =
                    selected_procedure_value_indexes(procedure, yields, &[column.as_str()])?;
                for node_index in nodes {
                    let node_id = shared_node_ids
                        .get(usize::try_from(*node_index).map_err(|_| {
                            DbError::program_limit(
                                "Cypher graph procedure node index exceeds usize capacity",
                            )
                        })?)
                        .ok_or_else(|| {
                            DbError::internal(format!(
                                "procedure {procedure} produced unknown node index {node_index}"
                            ))
                        })?;
                    let values = [node_id];
                    rows.push(selected_procedure_values_from_indexes(&selected, &values));
                }
            }
            AlgorithmResult::NodePairs {
                source_column,
                target_column,
                pairs,
            } => {
                let selected = selected_procedure_value_indexes(
                    procedure,
                    yields,
                    &[source_column.as_str(), target_column.as_str()],
                )?;
                for (source_index, target_index) in pairs {
                    let source_id = shared_node_ids
                        .get(usize::try_from(*source_index).map_err(|_| {
                            DbError::program_limit(
                                "Cypher graph procedure source index exceeds usize capacity",
                            )
                        })?)
                        .ok_or_else(|| {
                            DbError::internal(format!(
                                "procedure {procedure} produced result for unknown source node index {source_index}"
                            ))
                        })?;
                    let target_id = shared_node_ids
                        .get(usize::try_from(*target_index).map_err(|_| {
                            DbError::program_limit(
                                "Cypher graph procedure target index exceeds usize capacity",
                            )
                        })?)
                        .ok_or_else(|| {
                            DbError::internal(format!(
                                "procedure {procedure} produced result for unknown target node index {target_index}"
                            ))
                        })?;
                    let values = [source_id, target_id];
                    rows.push(selected_procedure_values_from_indexes(&selected, &values));
                }
            }
            AlgorithmResult::DegreeDistribution {
                degree_column,
                count_column,
                distribution,
            } => {
                let selected = selected_procedure_value_indexes(
                    procedure,
                    yields,
                    &[degree_column.as_str(), count_column.as_str()],
                )?;
                for (degree, count) in distribution {
                    let values = [
                        Arc::new(BoundValue::Scalar(Value::BigInt(i64::from(*degree)))),
                        Arc::new(BoundValue::Scalar(Value::BigInt(i64::from(*count)))),
                    ];
                    rows.push(selected_procedure_values_from_indexes(&selected, &values));
                }
            }
            AlgorithmResult::NodePairScores {
                source_column,
                target_column,
                score_column,
                scores,
            } => {
                let selected = selected_procedure_value_indexes(
                    procedure,
                    yields,
                    &[
                        source_column.as_str(),
                        target_column.as_str(),
                        score_column.as_str(),
                    ],
                )?;
                for (source_index, target_index, score) in scores {
                    let source_id = shared_node_ids
                        .get(usize::try_from(*source_index).map_err(|_| {
                            DbError::program_limit(
                                "Cypher graph procedure source index exceeds usize capacity",
                            )
                        })?)
                        .ok_or_else(|| {
                            DbError::internal(format!(
                                "procedure {procedure} produced result for unknown source node index {source_index}"
                            ))
                        })?;
                    let target_id = shared_node_ids
                        .get(usize::try_from(*target_index).map_err(|_| {
                            DbError::program_limit(
                                "Cypher graph procedure target index exceeds usize capacity",
                            )
                        })?)
                        .ok_or_else(|| {
                            DbError::internal(format!(
                                "procedure {procedure} produced result for unknown target node index {target_index}"
                            ))
                        })?;
                    let values = [
                        source_id,
                        target_id,
                        Arc::new(BoundValue::Scalar(Value::Double(*score))),
                    ];
                    rows.push(selected_procedure_values_from_indexes(&selected, &values));
                }
            }
            AlgorithmResult::NodePaths {
                source_column,
                target_column,
                cost_column,
                path_column,
                paths,
            } => {
                let selected = selected_procedure_value_indexes(
                    procedure,
                    yields,
                    &[
                        source_column.as_str(),
                        target_column.as_str(),
                        cost_column.as_str(),
                        path_column.as_str(),
                    ],
                )?;
                for (source_index, target_index, cost, path) in paths {
                    let source_id = shared_node_ids
                        .get(usize::try_from(*source_index).map_err(|_| {
                            DbError::program_limit(
                                "Cypher graph procedure source index exceeds usize capacity",
                            )
                        })?)
                        .ok_or_else(|| {
                            DbError::internal(format!(
                                "procedure {procedure} produced path for unknown source node index {source_index}"
                            ))
                        })?;
                    let target_id = shared_node_ids
                        .get(usize::try_from(*target_index).map_err(|_| {
                            DbError::program_limit(
                                "Cypher graph procedure target index exceeds usize capacity",
                            )
                        })?)
                        .ok_or_else(|| {
                            DbError::internal(format!(
                                "procedure {procedure} produced path for unknown target node index {target_index}"
                            ))
                        })?;
                    let path_values = path
                        .iter()
                        .map(|node_index| {
                            node_ids
                                .get(usize::try_from(*node_index).map_err(|_| {
                                    DbError::program_limit(
                                        "Cypher graph procedure path index exceeds usize capacity",
                                    )
                                })?)
                                .cloned()
                                .ok_or_else(|| {
                                    DbError::internal(format!(
                                        "procedure {procedure} produced path with unknown node index {node_index}"
                                    ))
                                })
                        })
                        .collect::<DbResult<Vec<_>>>()?;
                    let values = [
                        source_id,
                        target_id,
                        Arc::new(BoundValue::Scalar(Value::Double(*cost))),
                        Arc::new(BoundValue::Scalar(Value::Array(path_values))),
                    ];
                    rows.push(selected_procedure_values_from_indexes(&selected, &values));
                }
            }
            AlgorithmResult::NodeWalks {
                node_column,
                path_column,
                walks,
            } => {
                let selected = selected_procedure_value_indexes(
                    procedure,
                    yields,
                    &[node_column.as_str(), path_column.as_str()],
                )?;
                for (node_index, path) in walks {
                    let node_id = shared_node_ids
                        .get(usize::try_from(*node_index).map_err(|_| {
                            DbError::program_limit(
                                "Cypher graph procedure walk node index exceeds usize capacity",
                            )
                        })?)
                        .ok_or_else(|| {
                            DbError::internal(format!(
                                "procedure {procedure} produced walk for unknown node index {node_index}"
                            ))
                        })?;
                    let path_values = path
                        .iter()
                        .map(|path_node_index| {
                            node_ids
                                .get(usize::try_from(*path_node_index).map_err(|_| {
                                    DbError::program_limit(
                                        "Cypher graph procedure walk path index exceeds usize capacity",
                                    )
                                })?)
                                .cloned()
                                .ok_or_else(|| {
                                    DbError::internal(format!(
                                        "procedure {procedure} produced walk with unknown node index {path_node_index}"
                                    ))
                                })
                        })
                        .collect::<DbResult<Vec<_>>>()?;
                    let values = [
                        node_id,
                        Arc::new(BoundValue::Scalar(Value::Array(path_values))),
                    ];
                    rows.push(selected_procedure_values_from_indexes(&selected, &values));
                }
            }
            AlgorithmResult::NodeEmbeddings {
                node_column,
                embedding_column,
                embeddings,
            } => {
                let selected = selected_procedure_value_indexes(
                    procedure,
                    yields,
                    &[node_column.as_str(), embedding_column.as_str()],
                )?;
                for (node_index, embedding) in embeddings.iter().enumerate() {
                    let node_id = shared_node_ids.get(node_index).ok_or_else(|| {
                        DbError::internal(format!(
                            "procedure {procedure} produced result for unknown node index {node_index}"
                        ))
                    })?;
                    let vector = VectorValue::new(
                        u32::try_from(embedding.len()).unwrap_or(u32::MAX),
                        embedding.iter().map(|value| *value as f32).collect(),
                    );
                    let values = [node_id, Arc::new(BoundValue::Scalar(Value::Vector(vector)))];
                    rows.push(selected_procedure_values_from_indexes(&selected, &values));
                }
            }
            AlgorithmResult::Scalar { column, value } => {
                let selected =
                    selected_procedure_value_indexes(procedure, yields, &[column.as_str()])?;
                let values = [Arc::new(BoundValue::Scalar(Value::Double(*value)))];
                rows.push(selected_procedure_values_from_indexes(&selected, &values));
            }
            AlgorithmResult::ScalarU64 { column, value } => {
                let selected =
                    selected_procedure_value_indexes(procedure, yields, &[column.as_str()])?;
                let scalar = i64::try_from(*value).map(Value::BigInt).map_err(|_| {
                    DbError::program_limit("Cypher graph procedure scalar exceeds i64 capacity")
                })?;
                let values = [Arc::new(BoundValue::Scalar(scalar))];
                rows.push(selected_procedure_values_from_indexes(&selected, &values));
            }
        }
    }
    Ok(rows)
}
