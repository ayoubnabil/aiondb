// Cypher-to-logical-plan binder. Reserved for future direct
// Cypher -> plan translation, bypassing the current Cypher -> SQL -> plan
// pipeline in engine/cypher_sql.rs.

use aiondb_core::{DbError, DbResult, TxnId};
use aiondb_parser::{CypherClause, CypherStatement};

use super::*;

impl Binder {
    pub(super) fn bind_cypher_query(
        &self,
        stmt: &CypherStatement,
        txn_id: TxnId,
    ) -> DbResult<BoundCypherQuery> {
        let mut unwinds = Vec::new();
        let mut withs = Vec::new();
        let mut matches = Vec::new();
        let mut creates = Vec::new();
        let mut merges = Vec::new();
        let mut sets = Vec::new();
        let mut deletes = Vec::new();
        let mut calls = Vec::new();
        let mut return_clause = None;
        let mut clause_order = Vec::new();

        for clause in &stmt.clauses {
            match clause {
                CypherClause::Match(m) => {
                    let bound = self.bind_cypher_match(m, txn_id)?;
                    clause_order.push(BoundCypherClauseRef::Match(matches.len()));
                    matches.push(bound);
                }
                CypherClause::Create(c) => {
                    let bound = self.bind_cypher_create(c, txn_id)?;
                    clause_order.push(BoundCypherClauseRef::Create(creates.len()));
                    creates.push(bound);
                }
                CypherClause::Merge(m) => {
                    let bound = self.bind_cypher_merge(m, txn_id)?;
                    clause_order.push(BoundCypherClauseRef::Merge(merges.len()));
                    merges.push(bound);
                }
                CypherClause::Set(s) => {
                    let set_start = sets.len();
                    for item in &s.items {
                        match item {
                            aiondb_parser::CypherSetItem::Property {
                                variable,
                                property,
                                expr,
                                ..
                            } => {
                                sets.push(BoundCypherSetItem {
                                    variable: variable.clone(),
                                    property: property.clone(),
                                    expr: expr.clone(),
                                });
                            }
                            aiondb_parser::CypherSetItem::Label {
                                variable, label, ..
                            } => {
                                return Err(DbError::feature_not_supported(format!(
                                    "SET {variable}:{label} — label assignment is not yet supported in the direct pipeline"
                                )));
                            }
                            aiondb_parser::CypherSetItem::ReplaceProperties {
                                variable,
                                entries,
                                ..
                            }
                            | aiondb_parser::CypherSetItem::MergeProperties {
                                variable,
                                entries,
                                ..
                            } => {
                                for (key, value_expr) in entries {
                                    sets.push(BoundCypherSetItem {
                                        variable: variable.clone(),
                                        property: key.clone(),
                                        expr: (**value_expr).clone(),
                                    });
                                }
                            }
                        }
                    }
                    // Record all SET items from this clause.
                    for i in set_start..sets.len() {
                        clause_order.push(BoundCypherClauseRef::Set(i));
                    }
                }
                CypherClause::Delete(d) => {
                    clause_order.push(BoundCypherClauseRef::Delete(deletes.len()));
                    deletes.push(BoundCypherDelete {
                        detach: d.detach,
                        variables: d.variables.clone(),
                    });
                }
                CypherClause::Return(r) => {
                    let items = r
                        .items
                        .iter()
                        .map(|item| BoundCypherReturnItem {
                            expr: item.expr.clone(),
                            alias: item.alias.clone(),
                        })
                        .collect();
                    let order_by = r
                        .order_by
                        .iter()
                        .map(|ob| BoundOrderBy {
                            expr: ob.expr.clone(),
                            descending: ob.descending,
                            nulls_first: ob.nulls_first,
                        })
                        .collect();
                    return_clause = Some(BoundCypherReturn {
                        distinct: r.distinct,
                        items,
                        order_by,
                        skip: r.skip.clone(),
                        limit: r.limit.clone(),
                    });
                }
                CypherClause::Unwind(u) => {
                    clause_order.push(BoundCypherClauseRef::Unwind(unwinds.len()));
                    unwinds.push(BoundCypherUnwind {
                        expr: u.expr.clone(),
                        variable: u.variable.clone(),
                    });
                }
                CypherClause::With(w) => {
                    let items = w
                        .items
                        .iter()
                        .map(|item| BoundCypherReturnItem {
                            expr: item.expr.clone(),
                            alias: item.alias.clone(),
                        })
                        .collect();
                    let order_by = w
                        .order_by
                        .iter()
                        .map(|ob| BoundOrderBy {
                            expr: ob.expr.clone(),
                            descending: ob.descending,
                            nulls_first: ob.nulls_first,
                        })
                        .collect();
                    clause_order.push(BoundCypherClauseRef::With(withs.len()));
                    withs.push(BoundCypherWith {
                        distinct: w.distinct,
                        items,
                        where_clause: w.where_clause.clone(),
                        order_by,
                        skip: w.skip.clone(),
                        limit: w.limit.clone(),
                    });
                }
                CypherClause::Remove(r) => {
                    let set_start = sets.len();
                    for item in &r.items {
                        match item {
                            aiondb_parser::CypherRemoveItem::Property {
                                variable,
                                property,
                                span,
                            } => {
                                // REMOVE n.prop is equivalent to SET n.prop = NULL
                                sets.push(BoundCypherSetItem {
                                    variable: variable.clone(),
                                    property: property.clone(),
                                    expr: aiondb_parser::Expr::Literal(
                                        aiondb_parser::Literal::Null,
                                        *span,
                                    ),
                                });
                            }
                            aiondb_parser::CypherRemoveItem::Label {
                                variable, label, ..
                            } => {
                                return Err(DbError::feature_not_supported(format!(
                                    "REMOVE {variable}:{label} — label removal is not yet supported in the direct pipeline"
                                )));
                            }
                        }
                    }
                    for i in set_start..sets.len() {
                        clause_order.push(BoundCypherClauseRef::Set(i));
                    }
                }
                CypherClause::Call(c) => {
                    if let Some(subquery) = c.subquery.as_deref() {
                        let bound = self.bind_cypher_query(subquery, txn_id)?;
                        clause_order.push(BoundCypherClauseRef::Call(calls.len()));
                        calls.push(BoundCypherCallSubquery {
                            query: Box::new(bound),
                        });
                    } else {
                        return Err(DbError::feature_not_supported(format!(
                            "CALL {} — procedure calls are not yet supported in the direct pipeline",
                            c.procedure
                        )));
                    }
                }
                CypherClause::Foreach(_) => {
                    return Err(DbError::feature_not_supported(
                        "FOREACH is not supported in native Cypher yet",
                    ));
                }
            }
        }

        // Bind UNION [ALL] if present.
        let union = if let Some(ref cypher_union) = stmt.union {
            let right = self.bind_cypher_query(&cypher_union.right, txn_id)?;
            Some(Box::new(BoundCypherUnion {
                all: cypher_union.all,
                right,
            }))
        } else {
            None
        };

        Ok(BoundCypherQuery {
            unwinds,
            withs,
            matches,
            creates,
            merges,
            sets,
            deletes,
            calls,
            return_clause,
            clause_order,
            union,
        })
    }

    fn bind_cypher_match(
        &self,
        m: &aiondb_parser::CypherMatchClause,
        txn_id: TxnId,
    ) -> DbResult<BoundCypherMatch> {
        let patterns = m
            .patterns
            .iter()
            .map(|pattern| {
                let nodes = pattern
                    .nodes
                    .iter()
                    .map(|node| self.bind_cypher_node(node, txn_id))
                    .collect::<DbResult<Vec<_>>>()?;
                let rels = pattern
                    .rels
                    .iter()
                    .map(|rel| self.bind_cypher_rel(rel, txn_id))
                    .collect::<DbResult<Vec<_>>>()?;
                Ok(BoundCypherPattern {
                    path_variable: pattern.path_variable.clone(),
                    nodes,
                    rels,
                })
            })
            .collect::<DbResult<Vec<_>>>()?;

        Ok(BoundCypherMatch {
            optional: m.optional,
            patterns,
            where_clause: m.where_clause.clone(),
        })
    }

    fn bind_cypher_create(
        &self,
        c: &aiondb_parser::CypherCreateClause,
        txn_id: TxnId,
    ) -> DbResult<BoundCypherCreate> {
        let patterns = c
            .patterns
            .iter()
            .map(|pattern| {
                let nodes = pattern
                    .nodes
                    .iter()
                    .map(|node| self.bind_cypher_node(node, txn_id))
                    .collect::<DbResult<Vec<_>>>()?;
                let rels = pattern
                    .rels
                    .iter()
                    .map(|rel| self.bind_cypher_rel(rel, txn_id))
                    .collect::<DbResult<Vec<_>>>()?;
                Ok(BoundCypherPattern {
                    path_variable: pattern.path_variable.clone(),
                    nodes,
                    rels,
                })
            })
            .collect::<DbResult<Vec<_>>>()?;

        Ok(BoundCypherCreate { patterns })
    }

    fn bind_cypher_merge(
        &self,
        m: &aiondb_parser::CypherMergeClause,
        txn_id: TxnId,
    ) -> DbResult<BoundCypherMerge> {
        let pattern = BoundCypherPattern {
            path_variable: m.pattern.path_variable.clone(),
            nodes: m
                .pattern
                .nodes
                .iter()
                .map(|node| self.bind_cypher_node(node, txn_id))
                .collect::<DbResult<Vec<_>>>()?,
            rels: m
                .pattern
                .rels
                .iter()
                .map(|rel| self.bind_cypher_rel(rel, txn_id))
                .collect::<DbResult<Vec<_>>>()?,
        };

        let mut on_create = Vec::new();
        let mut on_match = Vec::new();

        for action in &m.actions {
            let target = if action.on_create {
                &mut on_create
            } else {
                &mut on_match
            };
            for item in &action.items {
                match item {
                    aiondb_parser::CypherSetItem::Property {
                        variable,
                        property,
                        expr,
                        ..
                    } => {
                        target.push(BoundCypherSetItem {
                            variable: variable.clone(),
                            property: property.clone(),
                            expr: expr.clone(),
                        });
                    }
                    aiondb_parser::CypherSetItem::Label {
                        variable, label, ..
                    } => {
                        return Err(DbError::feature_not_supported(format!(
                            "SET {variable}:{label} — label assignment is not yet supported in the direct pipeline"
                        )));
                    }
                    aiondb_parser::CypherSetItem::ReplaceProperties {
                        variable, entries, ..
                    }
                    | aiondb_parser::CypherSetItem::MergeProperties {
                        variable, entries, ..
                    } => {
                        for (key, value_expr) in entries {
                            target.push(BoundCypherSetItem {
                                variable: variable.clone(),
                                property: key.clone(),
                                expr: (**value_expr).clone(),
                            });
                        }
                    }
                }
            }
        }

        Ok(BoundCypherMerge {
            pattern,
            on_create,
            on_match,
        })
    }

    fn bind_cypher_node(
        &self,
        node: &aiondb_parser::CypherNodePattern,
        txn_id: TxnId,
    ) -> DbResult<BoundCypherNode> {
        let (table_id, columns) = if let Some(label) = node.labels.first() {
            if let Some(descriptor) = self.catalog.get_node_label(txn_id, label)? {
                let table = self.catalog.get_table_by_id(txn_id, descriptor.table_id)?;
                let cols = table
                    .map(|t| {
                        t.columns
                            .iter()
                            .map(|c| (c.name.clone(), c.data_type.clone()))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                (Some(descriptor.table_id), cols)
            } else {
                // Label not found -- allow binding to proceed with no table_id
                // so that CREATE-only patterns can at least parse.
                (None, Vec::new())
            }
        } else {
            // No label: columns will be resolved at execution time from all
            // registered node labels.  Pass empty columns for now.
            (None, Vec::new())
        };

        Ok(BoundCypherNode {
            variable: node.variable.clone(),
            label: node.labels.first().cloned(),
            table_id,
            columns,
            property_filters: node.properties.clone(),
        })
    }

    fn bind_cypher_rel(
        &self,
        rel: &aiondb_parser::CypherRelPattern,
        txn_id: TxnId,
    ) -> DbResult<BoundCypherRel> {
        let (table_id, columns) = if let Some(ref rel_type) = rel.rel_type {
            if let Some(descriptor) = self.catalog.get_edge_label(txn_id, rel_type)? {
                let table = self.catalog.get_table_by_id(txn_id, descriptor.table_id)?;
                let cols = table
                    .map(|t| {
                        t.columns
                            .iter()
                            .map(|c| (c.name.clone(), c.data_type.clone()))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                (Some(descriptor.table_id), cols)
            } else {
                (None, Vec::new())
            }
        } else {
            (None, Vec::new())
        };

        Ok(BoundCypherRel {
            variable: rel.variable.clone(),
            rel_type: rel.rel_type.clone(),
            rel_type_alternatives: rel.rel_types_alt.clone(),
            table_id,
            columns,
            direction: rel.direction,
            property_filters: rel.properties.clone(),
            min_hops: rel.min_hops,
            max_hops: rel.max_hops,
        })
    }
}
