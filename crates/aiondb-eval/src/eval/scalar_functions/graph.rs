//! Graph-specific scalar functions for Cypher/openCypher compatibility.
//!
//! These functions operate on graph nodes and relationships represented as
//! JSONB values.  A node is a JSON object with `"__graph_id"`, `"__graph_labels"`,
//! and arbitrary property keys.  A relationship is a JSON object with
//! `"__graph_id"`, `"__graph_type"`, `"__graph_start"`, `"__graph_end"`, and
//! properties.  A path is a JSON array alternating between nodes and relationships.

use aiondb_core::{DbError, DbResult, Value};

use super::expect_args;

/// `id(node_or_rel)` -- returns the internal graph ID of a node or relationship.
pub(super) fn eval_graph_id(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "id")?;
    if matches!(args[0], Value::Null) {
        return Ok(Value::Null);
    }
    match &args[0] {
        Value::Jsonb(obj) => match obj.get("__graph_id") {
            Some(serde_json::Value::Number(n)) => Ok(Value::BigInt(n.as_i64().unwrap_or_default())),
            Some(serde_json::Value::String(s)) => {
                Ok(Value::BigInt(s.parse::<i64>().unwrap_or_default()))
            }
            _ => Ok(Value::Null),
        },
        Value::Int(n) => Ok(Value::BigInt(i64::from(*n))),
        Value::BigInt(n) => Ok(Value::BigInt(*n)),
        _ => Err(DbError::internal(
            "id() argument must be a graph node or relationship",
        )),
    }
}

/// `labels(node)` -- returns the labels of a node as a text array.
pub(super) fn eval_graph_labels(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "labels")?;
    if matches!(args[0], Value::Null) {
        return Ok(Value::Null);
    }
    match &args[0] {
        Value::Jsonb(obj) => match obj.get("__graph_labels") {
            Some(serde_json::Value::Array(arr)) => {
                let labels: Vec<Value> = arr
                    .iter()
                    .map(|v| match v {
                        serde_json::Value::String(s) => Value::Text(s.clone()),
                        other => Value::Text(other.to_string()),
                    })
                    .collect();
                Ok(Value::Array(labels))
            }
            Some(serde_json::Value::String(s)) => Ok(Value::Array(vec![Value::Text(s.clone())])),
            _ => Ok(Value::Array(Vec::new())),
        },
        // Node literals from the Cypher graph executor render as
        // `(:Label {props})` or `(:L1:L2 ...)`. Recover the label list.
        Value::Text(text) => Ok(extract_node_labels(text).unwrap_or(Value::Array(Vec::new()))),
        _ => Err(DbError::internal("labels() argument must be a graph node")),
    }
}

fn extract_node_labels(text: &str) -> Option<Value> {
    let inner = text.strip_prefix('(')?.strip_suffix(')')?;
    let labels_part = inner.split_once(' ').map_or(inner, |(l, _)| l);
    if !labels_part.starts_with(':') {
        return Some(Value::Array(Vec::new()));
    }
    let labels: Vec<Value> = labels_part[1..]
        .split(':')
        .filter(|s| !s.is_empty())
        .map(|s| Value::Text(s.to_owned()))
        .collect();
    Some(Value::Array(labels))
}

/// `type(rel)` -- returns the type/label of a relationship as text.
pub(super) fn eval_graph_type(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "type")?;
    if matches!(args[0], Value::Null) {
        return Ok(Value::Null);
    }
    match &args[0] {
        Value::Jsonb(obj) => match obj.get("__graph_type") {
            Some(serde_json::Value::String(s)) => Ok(Value::Text(s.clone())),
            Some(other) => Ok(Value::Text(other.to_string())),
            None => Ok(Value::Null),
        },
        // Edge literals from the Cypher graph executor render as
        // `[:TYPE {props}]` or `[:TYPE]`; recover the type by stripping
        // the brackets and the leading colon.
        Value::Text(text) => Ok(extract_edge_type(text).unwrap_or(Value::Null)),
        _ => Err(DbError::internal(
            "type() argument must be a graph relationship",
        )),
    }
}

fn extract_edge_type(text: &str) -> Option<Value> {
    let inner = text.strip_prefix('[')?.strip_suffix(']')?;
    let after_colon = inner.strip_prefix(':')?;
    let type_part = after_colon.split_once(' ').map_or(after_colon, |(t, _)| t);
    if type_part.is_empty() {
        None
    } else {
        Some(Value::Text(type_part.to_owned()))
    }
}

/// `startNode(rel)` -- returns the start node ID of a relationship.
pub(super) fn eval_graph_start_node(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "startNode")?;
    if matches!(args[0], Value::Null) {
        return Ok(Value::Null);
    }
    match &args[0] {
        Value::Jsonb(obj) => match obj.get("__graph_start") {
            Some(serde_json::Value::Number(n)) => Ok(Value::BigInt(n.as_i64().unwrap_or_default())),
            Some(serde_json::Value::String(s)) => {
                Ok(Value::BigInt(s.parse::<i64>().unwrap_or_default()))
            }
            _ => Ok(Value::Null),
        },
        _ => Err(DbError::internal(
            "startNode() argument must be a graph relationship",
        )),
    }
}

/// `endNode(rel)` -- returns the end node ID of a relationship.
pub(super) fn eval_graph_end_node(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "endNode")?;
    if matches!(args[0], Value::Null) {
        return Ok(Value::Null);
    }
    match &args[0] {
        Value::Jsonb(obj) => match obj.get("__graph_end") {
            Some(serde_json::Value::Number(n)) => Ok(Value::BigInt(n.as_i64().unwrap_or_default())),
            Some(serde_json::Value::String(s)) => {
                Ok(Value::BigInt(s.parse::<i64>().unwrap_or_default()))
            }
            _ => Ok(Value::Null),
        },
        _ => Err(DbError::internal(
            "endNode() argument must be a graph relationship",
        )),
    }
}

/// `graph_path_length(path)` -- returns the number of relationships in a path.
///
/// A graph path is represented as a JSONB array alternating [node, rel, node, ...].
/// The length is the number of relationships, i.e. `(array_len - 1) / 2`.
pub(super) fn eval_graph_path_length(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "graph_path_length")?;
    if matches!(args[0], Value::Null) {
        return Ok(Value::Null);
    }
    match &args[0] {
        Value::Jsonb(serde_json::Value::Array(arr)) => {
            let len = if arr.is_empty() {
                0
            } else {
                (arr.len() - 1) / 2
            };
            let len_i64 = i64::try_from(len)
                .map_err(|_| DbError::internal("graph_path_length() result out of range"))?;
            Ok(Value::BigInt(len_i64))
        }
        Value::Array(arr) => {
            let len = if arr.is_empty() {
                0
            } else {
                (arr.len() - 1) / 2
            };
            let len_i64 = i64::try_from(len)
                .map_err(|_| DbError::internal("graph_path_length() result out of range"))?;
            Ok(Value::BigInt(len_i64))
        }
        _ => Err(DbError::internal(
            "graph_path_length() argument must be a graph path",
        )),
    }
}

/// `graph_nodes(path)` -- returns all nodes in a path (elements at even indices).
pub(super) fn eval_graph_nodes(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "graph_nodes")?;
    if matches!(args[0], Value::Null) {
        return Ok(Value::Null);
    }
    match &args[0] {
        Value::Jsonb(serde_json::Value::Array(arr)) => {
            let nodes: Vec<Value> = arr
                .iter()
                .step_by(2)
                .map(|v| Value::Jsonb(v.clone()))
                .collect();
            Ok(Value::Array(nodes))
        }
        Value::Array(arr) => {
            let nodes: Vec<Value> = arr.iter().step_by(2).cloned().collect();
            Ok(Value::Array(nodes))
        }
        _ => Err(DbError::internal(
            "graph_nodes() argument must be a graph path",
        )),
    }
}

/// `graph_relationships(path)` -- returns all relationships in a path (elements at odd indices).
pub(super) fn eval_graph_relationships(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "graph_relationships")?;
    if matches!(args[0], Value::Null) {
        return Ok(Value::Null);
    }
    match &args[0] {
        Value::Jsonb(serde_json::Value::Array(arr)) => {
            let rels: Vec<Value> = arr
                .iter()
                .skip(1)
                .step_by(2)
                .map(|v| Value::Jsonb(v.clone()))
                .collect();
            Ok(Value::Array(rels))
        }
        Value::Array(arr) => {
            let rels: Vec<Value> = arr.iter().skip(1).step_by(2).cloned().collect();
            Ok(Value::Array(rels))
        }
        _ => Err(DbError::internal(
            "graph_relationships() argument must be a graph path",
        )),
    }
}

/// `properties(node_or_rel)` -- returns all properties of a node or relationship as JSONB.
///
/// Internal keys (prefixed with `__graph_`) are excluded from the output.
pub(super) fn eval_graph_properties(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "properties")?;
    if matches!(args[0], Value::Null) {
        return Ok(Value::Null);
    }
    match &args[0] {
        Value::Jsonb(serde_json::Value::Object(map)) => {
            let props: serde_json::Map<String, serde_json::Value> = map
                .iter()
                .filter(|(k, _)| !k.starts_with("__graph_"))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            Ok(Value::Jsonb(serde_json::Value::Object(props)))
        }
        Value::Jsonb(other) => Ok(Value::Jsonb(other.clone())),
        _ => Err(DbError::internal(
            "properties() argument must be a graph node or relationship",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_node(id: i64, labels: &[&str], props: &[(&str, &str)]) -> Value {
        let mut map = serde_json::Map::new();
        map.insert(
            "__graph_id".to_owned(),
            serde_json::Value::Number(id.into()),
        );
        let labels_json: Vec<serde_json::Value> = labels
            .iter()
            .map(|l| serde_json::Value::String(l.to_string()))
            .collect();
        map.insert(
            "__graph_labels".to_owned(),
            serde_json::Value::Array(labels_json),
        );
        for (k, v) in props {
            map.insert(k.to_string(), serde_json::Value::String(v.to_string()));
        }
        Value::Jsonb(serde_json::Value::Object(map))
    }

    fn make_rel(id: i64, rel_type: &str, start: i64, end: i64) -> Value {
        let mut map = serde_json::Map::new();
        map.insert(
            "__graph_id".to_owned(),
            serde_json::Value::Number(id.into()),
        );
        map.insert(
            "__graph_type".to_owned(),
            serde_json::Value::String(rel_type.to_owned()),
        );
        map.insert(
            "__graph_start".to_owned(),
            serde_json::Value::Number(start.into()),
        );
        map.insert(
            "__graph_end".to_owned(),
            serde_json::Value::Number(end.into()),
        );
        Value::Jsonb(serde_json::Value::Object(map))
    }

    #[test]
    fn test_graph_id_node() {
        let node = make_node(42, &["Person"], &[]);
        let result = eval_graph_id(&[node]).unwrap();
        assert_eq!(result, Value::BigInt(42));
    }

    #[test]
    fn test_graph_id_null() {
        let result = eval_graph_id(&[Value::Null]).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn test_graph_labels() {
        let node = make_node(1, &["Person", "Employee"], &[]);
        let result = eval_graph_labels(&[node]).unwrap();
        assert_eq!(
            result,
            Value::Array(vec![
                Value::Text("Person".to_owned()),
                Value::Text("Employee".to_owned()),
            ])
        );
    }

    #[test]
    fn test_graph_type() {
        let rel = make_rel(10, "KNOWS", 1, 2);
        let result = eval_graph_type(&[rel]).unwrap();
        assert_eq!(result, Value::Text("KNOWS".to_owned()));
    }

    #[test]
    fn test_graph_start_node() {
        let rel = make_rel(10, "KNOWS", 1, 2);
        let result = eval_graph_start_node(&[rel]).unwrap();
        assert_eq!(result, Value::BigInt(1));
    }

    #[test]
    fn test_graph_end_node() {
        let rel = make_rel(10, "KNOWS", 1, 2);
        let result = eval_graph_end_node(&[rel]).unwrap();
        assert_eq!(result, Value::BigInt(2));
    }

    #[test]
    fn test_graph_path_length() {
        // Path: [node1, rel, node2] => length 1
        let path = Value::Jsonb(serde_json::Value::Array(vec![
            serde_json::Value::Object(serde_json::Map::new()),
            serde_json::Value::Object(serde_json::Map::new()),
            serde_json::Value::Object(serde_json::Map::new()),
        ]));
        let result = eval_graph_path_length(&[path]).unwrap();
        assert_eq!(result, Value::BigInt(1));
    }

    #[test]
    fn test_graph_nodes() {
        let n1 = serde_json::json!({"__graph_id": 1});
        let r1 = serde_json::json!({"__graph_id": 10});
        let n2 = serde_json::json!({"__graph_id": 2});
        let path = Value::Jsonb(serde_json::Value::Array(vec![n1.clone(), r1, n2.clone()]));
        let result = eval_graph_nodes(&[path]).unwrap();
        assert_eq!(
            result,
            Value::Array(vec![Value::Jsonb(n1), Value::Jsonb(n2)])
        );
    }

    #[test]
    fn test_graph_relationships() {
        let n1 = serde_json::json!({"__graph_id": 1});
        let r1 = serde_json::json!({"__graph_id": 10, "__graph_type": "KNOWS"});
        let n2 = serde_json::json!({"__graph_id": 2});
        let path = Value::Jsonb(serde_json::Value::Array(vec![n1, r1.clone(), n2]));
        let result = eval_graph_relationships(&[path]).unwrap();
        assert_eq!(result, Value::Array(vec![Value::Jsonb(r1)]));
    }

    #[test]
    fn test_graph_properties() {
        let node = make_node(1, &["Person"], &[("name", "Alice"), ("age", "30")]);
        let result = eval_graph_properties(&[node]).unwrap();
        match result {
            Value::Jsonb(serde_json::Value::Object(map)) => {
                assert!(map.contains_key("name"));
                assert!(map.contains_key("age"));
                assert!(!map.contains_key("__graph_id"));
                assert!(!map.contains_key("__graph_labels"));
            }
            other => panic!("expected JSONB object, got {other:?}"),
        }
    }
}
