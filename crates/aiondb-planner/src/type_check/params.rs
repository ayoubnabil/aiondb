use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use aiondb_catalog::ColumnDescriptor;
use aiondb_core::{DataType, DbError, DbResult, ErrorReport, SqlState};

use super::support::merge_parameter_types;

#[derive(Clone, Debug, Default)]
pub(super) struct SessionVariableContext {
    pub(super) current_user: Option<String>,
    pub(super) session_user: Option<String>,
    pub(super) current_schema: Option<String>,
    pub(super) current_database: Option<String>,
    pub(super) search_path_schemas: Arc<Vec<String>>,
}

#[derive(Default)]
pub(super) struct ParameterTypes {
    seen: BTreeSet<usize>,
    inferred: BTreeMap<usize, DataType>,
    session_context: SessionVariableContext,
    /// Columns from an outer (enclosing) query scope, used for correlated subqueries.
    /// When set, unresolved column names are checked against these columns as a fallback.
    outer_columns: Vec<ColumnDescriptor>,
}

impl ParameterTypes {
    pub(super) fn with_session_context(session_context: SessionVariableContext) -> Self {
        Self {
            seen: BTreeSet::new(),
            inferred: BTreeMap::new(),
            session_context,
            outer_columns: Vec::new(),
        }
    }

    pub(super) fn infer(
        &mut self,
        index: usize,
        position: usize,
        data_type: &DataType,
    ) -> DbResult<DataType> {
        self.seen.insert(index);
        let merged = match self.inferred.get(&index) {
            Some(existing) => merge_parameter_types(index, position, existing, data_type)?,
            None => data_type.clone(),
        };
        let result = merged.clone();
        self.inferred.insert(index, merged);
        Ok(result)
    }

    pub(super) fn mark_seen(&mut self, index: usize) {
        self.seen.insert(index);
    }

    pub(super) fn known(&self, index: usize) -> Option<&DataType> {
        self.inferred.get(&index)
    }

    pub(super) fn seed_hints(&mut self, hints: &[Option<DataType>]) {
        for (offset, maybe_type) in hints.iter().enumerate() {
            if let Some(data_type) = maybe_type {
                let index = offset + 1;
                self.inferred.insert(index, data_type.clone());
            }
        }
    }

    pub(super) fn session_context(&self) -> &SessionVariableContext {
        &self.session_context
    }

    /// Set outer scope columns for correlated subquery resolution.
    pub(super) fn set_outer_columns(&mut self, columns: Vec<ColumnDescriptor>) {
        self.outer_columns = columns;
    }

    /// Look up a column name in the outer scope columns.
    /// Returns the column descriptor if found.
    pub(super) fn find_outer_column(&self, column_name: &str) -> Option<&ColumnDescriptor> {
        let matched = self
            .outer_columns
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(column_name))?;
        if !matched.name.contains('\0') {
            return Some(matched);
        }

        let bare_name = matched.name.rsplit('\0').next().unwrap_or(&matched.name);
        self.outer_columns
            .iter()
            .find(|candidate| {
                candidate.column_id == matched.column_id
                    && !candidate.name.contains('\0')
                    && candidate.name.eq_ignore_ascii_case(bare_name)
            })
            .or(Some(matched))
    }

    pub(super) fn merge_inferred(&mut self, other: &[DataType]) -> DbResult<()> {
        for (index, data_type) in other.iter().enumerate() {
            let parameter_index = index + 1;
            let _ = self.infer(parameter_index, 1, data_type)?;
        }
        Ok(())
    }

    pub(super) fn finalize(mut self) -> DbResult<Vec<DataType>> {
        let Some(max_index) = self.seen.iter().copied().max() else {
            return Ok(Vec::new());
        };

        let mut result = Vec::with_capacity(max_index);
        for index in 1..=max_index {
            let data_type = if self.seen.contains(&index) {
                let Some(data_type) = self.inferred.remove(&index) else {
                    return Err(DbError::Bind(Box::new(ErrorReport::new(
                        SqlState::SyntaxError,
                        format!("could not infer data type of parameter ${index}"),
                    ))));
                };
                data_type
            } else {
                // Some PostgreSQL clients/ORMs reuse named bind parameters
                // through protocol adapters that can leave gaps in the final
                // `$n` numbering. PostgreSQL accepts those sparse parameter
                // slots as long as the bind message supplies values up to the
                // highest index, so mirror that behaviour here.
                DataType::Text
            };
            result.push(data_type);
        }
        Ok(result)
    }
}
