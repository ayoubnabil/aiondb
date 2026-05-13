use super::*;

impl Binder {
    pub(super) fn bind_create_node_label(
        &self,
        stmt: &CreateNodeLabelStatement,
        txn_id: TxnId,
        default_schema: Option<&str>,
    ) -> DbResult<BoundCreateNodeLabel> {
        let relation_error_name = relation_error_name(&stmt.table, default_schema)?;
        let (_, table) = resolve_table_in_search_path(
            self.catalog.as_ref(),
            txn_id,
            &stmt.table,
            default_schema,
        )?
        .ok_or_else(|| undefined_table(&stmt.table, &relation_error_name))?;
        Ok(BoundCreateNodeLabel {
            label: stmt.label.clone(),
            table,
        })
    }

    pub(super) fn bind_create_edge_label(
        &self,
        stmt: &CreateEdgeLabelStatement,
        txn_id: TxnId,
        default_schema: Option<&str>,
    ) -> DbResult<BoundCreateEdgeLabel> {
        let relation_error_name = relation_error_name(&stmt.table, default_schema)?;
        let (_, table) = resolve_table_in_search_path(
            self.catalog.as_ref(),
            txn_id,
            &stmt.table,
            default_schema,
        )?
        .ok_or_else(|| undefined_table(&stmt.table, &relation_error_name))?;
        // Validate that source and target node labels exist
        if self
            .catalog
            .get_node_label(txn_id, &stmt.source_label)?
            .is_none()
        {
            return Err(DbError::bind_error(
                SqlState::UndefinedObject,
                format!("source node label \"{}\" does not exist", stmt.source_label),
            ));
        }
        if self
            .catalog
            .get_node_label(txn_id, &stmt.target_label)?
            .is_none()
        {
            return Err(DbError::bind_error(
                SqlState::UndefinedObject,
                format!("target node label \"{}\" does not exist", stmt.target_label),
            ));
        }
        Ok(BoundCreateEdgeLabel {
            label: stmt.label.clone(),
            table,
            source_label: stmt.source_label.clone(),
            target_label: stmt.target_label.clone(),
            endpoints: stmt.endpoints.clone(),
        })
    }

    pub(super) fn bind_drop_node_label(
        &self,
        stmt: &DropNodeLabelStatement,
        txn_id: TxnId,
    ) -> DbResult<BoundDropNodeLabel> {
        // Verify the label exists
        if self.catalog.get_node_label(txn_id, &stmt.label)?.is_none() {
            return Err(DbError::bind_error(
                SqlState::UndefinedObject,
                format!("node label \"{}\" does not exist", stmt.label),
            ));
        }
        Ok(BoundDropNodeLabel {
            label: stmt.label.clone(),
        })
    }

    pub(super) fn bind_drop_edge_label(
        &self,
        stmt: &DropEdgeLabelStatement,
        txn_id: TxnId,
    ) -> DbResult<BoundDropEdgeLabel> {
        // Verify the label exists
        if self.catalog.get_edge_label(txn_id, &stmt.label)?.is_none() {
            return Err(DbError::bind_error(
                SqlState::UndefinedObject,
                format!("edge label \"{}\" does not exist", stmt.label),
            ));
        }
        Ok(BoundDropEdgeLabel {
            label: stmt.label.clone(),
        })
    }
}
