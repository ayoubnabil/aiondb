use aiondb_core::Row;
use aiondb_plan::ResultField;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ExecutionResult {
    Query {
        columns: Vec<ResultField>,
        rows: Vec<Row>,
    },
    Command {
        tag: String,
        rows_affected: u64,
    },
    CopyIn {
        table_id: aiondb_core::RelationId,
        columns: Vec<aiondb_plan::ColumnPlan>,
    },
    CopyOut {
        data: String,
        column_count: usize,
    },
}

impl ExecutionResult {
    /// Shorthand for a zero-rows-affected command result.
    pub fn command(tag: &str) -> Self {
        Self::Command {
            tag: tag.to_owned(),
            rows_affected: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ResultChunk {
    pub rows: Vec<Row>,
    pub exhausted: bool,
}
