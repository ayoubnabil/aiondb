const MAX_AUTH_CYPHER_QUERY_DEPTH: usize = 512;
const MAX_AUTH_HYBRID_PLAN_DEPTH: usize = 512;
const MAX_AUTH_HYBRID_EXPR_DEPTH: usize = 1024;

thread_local! {
    static AUTH_CYPHER_QUERY_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static AUTH_HYBRID_PLAN_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static AUTH_HYBRID_EXPR_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

struct AuthDepthGuard {
    key: &'static std::thread::LocalKey<std::cell::Cell<usize>>,
}

impl Drop for AuthDepthGuard {
    fn drop(&mut self) {
        self.key
            .with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

fn enter_auth_depth(
    key: &'static std::thread::LocalKey<std::cell::Cell<usize>>,
    limit: usize,
    label: &str,
) -> DbResult<AuthDepthGuard> {
    key.with(|depth| {
        let current = depth.get();
        if current >= limit {
            return Err(DbError::program_limit(format!(
                "authorization {label} traversal depth exceeds limit {limit}"
            )));
        }
        depth.set(current + 1);
        Ok(AuthDepthGuard { key })
    })
}

fn enforce_cypher_schema_mutation_acl(
    catalog_reader: &dyn CatalogReader,
    identity: &AuthenticatedIdentity,
    plan: &PhysicalPlan,
) -> DbResult<()> {
    let PhysicalPlan::CypherQuery(query) = plan else {
        return Ok(());
    };
    if !catalog_has_any_roles(catalog_reader)? || is_superuser_checked(catalog_reader, identity)? {
        return Ok(());
    }
    deny_cypher_schema_mutation_for_query(catalog_reader, query)
}

fn deny_cypher_schema_mutation_for_query(
    catalog_reader: &dyn CatalogReader,
    query: &aiondb_plan::graph::CypherQueryPlan,
) -> DbResult<()> {
    let _guard = enter_auth_depth(
        &AUTH_CYPHER_QUERY_DEPTH,
        MAX_AUTH_CYPHER_QUERY_DEPTH,
        "cypher schema mutation",
    )?;
    for create_clause in &query.creates {
        for pattern in &create_clause.patterns {
            deny_cypher_schema_mutation_for_pattern(catalog_reader, pattern, "CREATE")?;
        }
    }
    for merge_clause in &query.merges {
        deny_cypher_schema_mutation_for_pattern(catalog_reader, &merge_clause.pattern, "MERGE")?;
    }
    if let Some(union) = &query.union {
        deny_cypher_schema_mutation_for_query(catalog_reader, &union.right)?;
    }
    Ok(())
}

fn deny_cypher_schema_mutation_for_pattern(
    catalog_reader: &dyn CatalogReader,
    pattern: &aiondb_plan::graph::CypherPattern,
    clause_name: &str,
) -> DbResult<()> {
    for node in &pattern.nodes {
        let table_ids = resolve_cypher_create_node_table_ids(catalog_reader, node)?;
        deny_cypher_missing_property_columns(
            catalog_reader,
            &table_ids,
            &node.properties,
            clause_name,
        )?;
    }
    for rel in &pattern.relationships {
        let table_ids = resolve_cypher_create_rel_table_ids(catalog_reader, rel)?;
        deny_cypher_missing_property_columns(
            catalog_reader,
            &table_ids,
            &rel.properties,
            clause_name,
        )?;
    }
    Ok(())
}

fn deny_cypher_missing_property_columns(
    catalog_reader: &dyn CatalogReader,
    table_ids: &[RelationId],
    properties: &[aiondb_plan::graph::CypherPropertyExpr],
    clause_name: &str,
) -> DbResult<()> {
    let txn = TxnId::default();
    for table_id in table_ids {
        let Some(table) = catalog_reader.get_table_by_id(txn, *table_id)? else {
            continue;
        };
        for property in properties {
            let column_exists = table
                .columns
                .iter()
                .any(|column| column.name.eq_ignore_ascii_case(&property.key));
            if !column_exists {
                return Err(DbError::insufficient_privilege(format!(
                    "Cypher {clause_name} cannot add missing property column '{}' on table {} when RBAC is active",
                    property.key, table.name
                )));
            }
        }
    }
    Ok(())
}

fn required_hybrid_scan_privileges(
    catalog_reader: &dyn CatalogReader,
    plan: &PhysicalPlan,
) -> DbResult<Vec<(CatalogPrivilege, RelationId)>> {
    let mut reqs = Vec::new();
    collect_required_hybrid_privileges_from_physical_plan(catalog_reader, plan, &mut reqs)?;
    Ok(reqs)
}

fn required_cypher_privileges(
    catalog_reader: &dyn CatalogReader,
    plan: &PhysicalPlan,
) -> DbResult<Vec<(CatalogPrivilege, RelationId)>> {
    let PhysicalPlan::CypherQuery(cypher) = plan else {
        return Ok(Vec::new());
    };
    let rbac_active = catalog_has_any_roles(catalog_reader)?;
    let mut reqs = Vec::new();
    collect_required_cypher_privileges_from_query(catalog_reader, cypher, rbac_active, &mut reqs)?;
    Ok(reqs)
}

fn collect_required_cypher_privileges_from_query(
    catalog_reader: &dyn CatalogReader,
    query: &aiondb_plan::graph::CypherQueryPlan,
    rbac_active: bool,
    reqs: &mut Vec<(CatalogPrivilege, RelationId)>,
) -> DbResult<()> {
    let _guard = enter_auth_depth(
        &AUTH_CYPHER_QUERY_DEPTH,
        MAX_AUTH_CYPHER_QUERY_DEPTH,
        "cypher privilege",
    )?;
    let mut variable_tables = BTreeMap::<String, Vec<RelationId>>::new();

    for pipeline_op in &query.pipeline {
        match pipeline_op {
            aiondb_plan::graph::CypherPipelineOp::Match(match_clause) => {
                collect_required_cypher_match_privileges(
                    catalog_reader,
                    match_clause,
                    reqs,
                    &mut variable_tables,
                )?;
            }
            aiondb_plan::graph::CypherPipelineOp::With(with_clause) => {
                collect_required_cypher_with_variable_propagation(
                    with_clause,
                    &mut variable_tables,
                );
            }
            aiondb_plan::graph::CypherPipelineOp::CallSubquery(subquery) => {
                collect_required_cypher_privileges_from_query(
                    catalog_reader,
                    subquery,
                    rbac_active,
                    reqs,
                )?;
            }
            aiondb_plan::graph::CypherPipelineOp::Unwind(_) => {}
        }
    }

    for match_clause in &query.matches {
        collect_required_cypher_match_privileges(
            catalog_reader,
            match_clause,
            reqs,
            &mut variable_tables,
        )?;
    }

    for create_clause in &query.creates {
        collect_required_cypher_create_privileges(
            catalog_reader,
            create_clause,
            rbac_active,
            reqs,
            &mut variable_tables,
        )?;
    }

    for merge_clause in &query.merges {
        collect_required_cypher_match_pattern_privileges(
            catalog_reader,
            &merge_clause.pattern,
            reqs,
            &mut variable_tables,
        )?;
        collect_required_cypher_create_pattern_privileges(
            catalog_reader,
            &merge_clause.pattern,
            rbac_active,
            reqs,
            &mut variable_tables,
            "MERGE",
        )?;
        for set_item in &merge_clause.on_create_set {
            collect_required_cypher_set_privileges(
                set_item,
                reqs,
                &variable_tables,
                rbac_active,
                "MERGE ON CREATE SET",
            )?;
        }
        for set_item in &merge_clause.on_match_set {
            collect_required_cypher_set_privileges(
                set_item,
                reqs,
                &variable_tables,
                rbac_active,
                "MERGE ON MATCH SET",
            )?;
        }
    }

    for set_item in &query.sets {
        collect_required_cypher_set_privileges(
            set_item,
            reqs,
            &variable_tables,
            rbac_active,
            "SET/REMOVE",
        )?;
    }

    for delete_clause in &query.deletes {
        collect_required_cypher_delete_privileges(
            catalog_reader,
            delete_clause,
            reqs,
            &variable_tables,
            rbac_active,
        )?;
    }

    if let Some(union) = &query.union {
        collect_required_cypher_privileges_from_query(
            catalog_reader,
            &union.right,
            rbac_active,
            reqs,
        )?;
    }

    Ok(())
}

fn collect_required_cypher_match_privileges(
    catalog_reader: &dyn CatalogReader,
    match_clause: &aiondb_plan::graph::CypherMatchClause,
    reqs: &mut Vec<(CatalogPrivilege, RelationId)>,
    variable_tables: &mut BTreeMap<String, Vec<RelationId>>,
) -> DbResult<()> {
    for pattern in &match_clause.patterns {
        collect_required_cypher_match_pattern_privileges(
            catalog_reader,
            pattern,
            reqs,
            variable_tables,
        )?;
    }
    Ok(())
}

fn collect_required_cypher_match_pattern_privileges(
    catalog_reader: &dyn CatalogReader,
    pattern: &aiondb_plan::graph::CypherPattern,
    reqs: &mut Vec<(CatalogPrivilege, RelationId)>,
    variable_tables: &mut BTreeMap<String, Vec<RelationId>>,
) -> DbResult<()> {
    for node in &pattern.nodes {
        let table_ids = resolve_cypher_match_node_table_ids(catalog_reader, node)?;
        for table_id in &table_ids {
            push_required_table_privilege(reqs, CatalogPrivilege::Select, *table_id);
        }
        if let Some(variable) = node.variable.as_deref() {
            push_variable_table_ids(variable_tables, variable, &table_ids);
        }
    }
    for rel in &pattern.relationships {
        if let Some(table_id) = rel.table_id {
            push_required_table_privilege(reqs, CatalogPrivilege::Select, table_id);
            if let Some(variable) = rel.variable.as_deref() {
                push_variable_table_ids(variable_tables, variable, &[table_id]);
            }
        }
    }
    Ok(())
}

fn collect_required_cypher_create_privileges(
    catalog_reader: &dyn CatalogReader,
    create_clause: &aiondb_plan::graph::CypherCreateClause,
    rbac_active: bool,
    reqs: &mut Vec<(CatalogPrivilege, RelationId)>,
    variable_tables: &mut BTreeMap<String, Vec<RelationId>>,
) -> DbResult<()> {
    for pattern in &create_clause.patterns {
        collect_required_cypher_create_pattern_privileges(
            catalog_reader,
            pattern,
            rbac_active,
            reqs,
            variable_tables,
            "CREATE",
        )?;
    }
    Ok(())
}

fn collect_required_cypher_create_pattern_privileges(
    catalog_reader: &dyn CatalogReader,
    pattern: &aiondb_plan::graph::CypherPattern,
    rbac_active: bool,
    reqs: &mut Vec<(CatalogPrivilege, RelationId)>,
    variable_tables: &mut BTreeMap<String, Vec<RelationId>>,
    clause_name: &str,
) -> DbResult<()> {
    for node in &pattern.nodes {
        let table_ids = resolve_cypher_create_node_table_ids(catalog_reader, node)?;
        if rbac_active && node.label.is_some() && table_ids.is_empty() {
            return Err(DbError::insufficient_privilege(format!(
                "cannot determine backing table for Cypher {clause_name} label '{}'",
                node.label.clone().unwrap_or_default()
            )));
        }
        for table_id in &table_ids {
            push_required_table_privilege(reqs, CatalogPrivilege::Insert, *table_id);
        }
        if let Some(variable) = node.variable.as_deref() {
            push_variable_table_ids(variable_tables, variable, &table_ids);
        }
    }
    for rel in &pattern.relationships {
        let table_ids = resolve_cypher_create_rel_table_ids(catalog_reader, rel)?;
        if rbac_active && rel.rel_type.is_some() && table_ids.is_empty() {
            return Err(DbError::insufficient_privilege(format!(
                "cannot determine backing table for Cypher {clause_name} relationship '{}'",
                rel.rel_type.clone().unwrap_or_default()
            )));
        }
        for table_id in &table_ids {
            push_required_table_privilege(reqs, CatalogPrivilege::Insert, *table_id);
        }
        if let Some(variable) = rel.variable.as_deref() {
            push_variable_table_ids(variable_tables, variable, &table_ids);
        }
    }
    Ok(())
}

fn collect_required_cypher_with_variable_propagation(
    with_clause: &aiondb_plan::graph::CypherWithClause,
    variable_tables: &mut BTreeMap<String, Vec<RelationId>>,
) {
    // WITH starts a new binding scope. Keep only aliases that are direct
    // variable projections (e.g. `WITH n AS m`).
    let incoming_variable_tables = variable_tables.clone();
    variable_tables.clear();

    for item in &with_clause.items {
        let TypedExprKind::ColumnRef { name, .. } = &item.expr.kind else {
            continue;
        };
        if name.contains('.') || name.contains('\0') {
            continue;
        }
        if let Some(table_ids) = incoming_variable_tables.get(name) {
            push_variable_table_ids(variable_tables, &item.field.name, table_ids);
        }
    }
}

fn collect_required_cypher_set_privileges(
    set_item: &aiondb_plan::graph::CypherSetItem,
    reqs: &mut Vec<(CatalogPrivilege, RelationId)>,
    variable_tables: &BTreeMap<String, Vec<RelationId>>,
    rbac_active: bool,
    clause_name: &str,
) -> DbResult<()> {
    if let Some(table_id) = set_item.table_id {
        push_required_table_privilege(reqs, CatalogPrivilege::Update, table_id);
        return Ok(());
    }
    match variable_tables.get(&set_item.variable) {
        Some(table_ids) if !table_ids.is_empty() => {
            for table_id in table_ids {
                push_required_table_privilege(reqs, CatalogPrivilege::Update, *table_id);
            }
            Ok(())
        }
        _ if rbac_active => Err(DbError::insufficient_privilege(format!(
            "cannot determine backing table for Cypher {clause_name} variable '{}'",
            set_item.variable
        ))),
        _ => Ok(()),
    }
}

fn collect_required_cypher_delete_privileges(
    catalog_reader: &dyn CatalogReader,
    delete_clause: &aiondb_plan::graph::CypherDeleteClause,
    reqs: &mut Vec<(CatalogPrivilege, RelationId)>,
    variable_tables: &BTreeMap<String, Vec<RelationId>>,
    rbac_active: bool,
) -> DbResult<()> {
    for target in &delete_clause.variables {
        match variable_tables.get(&target.variable) {
            Some(table_ids) if !table_ids.is_empty() => {
                for table_id in table_ids {
                    push_required_table_privilege(reqs, CatalogPrivilege::Delete, *table_id);
                }
            }
            _ if rbac_active => {
                return Err(DbError::insufficient_privilege(format!(
                    "cannot determine backing table for Cypher DELETE variable '{}'",
                    target.variable
                )));
            }
            _ => {}
        }

        if delete_clause.detach {
            for edge_table_id in resolve_required_detach_delete_edge_table_ids(
                catalog_reader,
                target,
                variable_tables,
            )? {
                push_required_table_privilege(reqs, CatalogPrivilege::Delete, edge_table_id);
            }
        }
    }
    Ok(())
}

fn resolve_required_detach_delete_edge_table_ids(
    catalog_reader: &dyn CatalogReader,
    target: &aiondb_plan::graph::CypherDeleteTarget,
    variable_tables: &BTreeMap<String, Vec<RelationId>>,
) -> DbResult<Vec<RelationId>> {
    if !target.connected_edge_table_ids.is_empty() {
        let mut table_ids = Vec::new();
        for table_id in &target.connected_edge_table_ids {
            if !table_ids.contains(table_id) {
                table_ids.push(*table_id);
            }
        }
        return Ok(table_ids);
    }

    let txn = TxnId::default();
    let edge_labels = catalog_reader.list_edge_labels(txn)?;
    let mut all_edge_table_ids = Vec::new();
    for edge_label in &edge_labels {
        if !all_edge_table_ids.contains(&edge_label.table_id) {
            all_edge_table_ids.push(edge_label.table_id);
        }
    }
    if all_edge_table_ids.is_empty() {
        return Ok(all_edge_table_ids);
    }

    let Some(target_table_ids) = variable_tables.get(&target.variable) else {
        return Ok(all_edge_table_ids);
    };
    if target_table_ids.is_empty() {
        return Ok(all_edge_table_ids);
    }

    let mut node_labels_by_table_id = BTreeMap::<RelationId, Vec<String>>::new();
    for node_label in catalog_reader.list_node_labels(txn)? {
        let entry = node_labels_by_table_id
            .entry(node_label.table_id)
            .or_default();
        if !entry
            .iter()
            .any(|label| label.eq_ignore_ascii_case(&node_label.label))
        {
            entry.push(node_label.label);
        }
    }

    let mut target_node_labels = BTreeSet::<String>::new();
    for table_id in target_table_ids {
        if let Some(labels) = node_labels_by_table_id.get(table_id) {
            for label in labels {
                target_node_labels.insert(label.to_ascii_lowercase());
            }
        }
    }

    // If the delete target resolves only to non-node tables (e.g. an edge
    // variable in `DETACH DELETE e`), no additional detach-edge privileges are
    // required beyond DELETE on the target itself.
    if target_node_labels.is_empty() {
        return Ok(Vec::new());
    }

    let mut connected_edge_table_ids = Vec::new();
    for edge_label in edge_labels {
        let source_label = edge_label.source_label.to_ascii_lowercase();
        let target_label = edge_label.target_label.to_ascii_lowercase();
        if (target_node_labels.contains(&source_label)
            || target_node_labels.contains(&target_label))
            && !connected_edge_table_ids.contains(&edge_label.table_id)
        {
            connected_edge_table_ids.push(edge_label.table_id);
        }
    }
    Ok(connected_edge_table_ids)
}

fn resolve_cypher_match_node_table_ids(
    catalog_reader: &dyn CatalogReader,
    node: &aiondb_plan::graph::CypherNodePattern,
) -> DbResult<Vec<RelationId>> {
    if let Some(table_id) = node.table_id {
        return Ok(vec![table_id]);
    }
    if node.variable.is_some() {
        return list_all_node_label_table_ids(catalog_reader);
    }
    Ok(Vec::new())
}

fn resolve_cypher_create_node_table_ids(
    catalog_reader: &dyn CatalogReader,
    node: &aiondb_plan::graph::CypherNodePattern,
) -> DbResult<Vec<RelationId>> {
    if let Some(table_id) = node.table_id {
        return Ok(vec![table_id]);
    }
    let Some(label) = node.label.as_deref() else {
        return Ok(Vec::new());
    };
    let txn = TxnId::default();
    Ok(catalog_reader
        .get_node_label(txn, label)?
        .map(|desc| vec![desc.table_id])
        .unwrap_or_default())
}

fn resolve_cypher_create_rel_table_ids(
    catalog_reader: &dyn CatalogReader,
    rel: &aiondb_plan::graph::CypherRelPattern,
) -> DbResult<Vec<RelationId>> {
    if let Some(table_id) = rel.table_id {
        return Ok(vec![table_id]);
    }
    let Some(rel_type) = rel.rel_type.as_deref() else {
        return Ok(Vec::new());
    };
    let txn = TxnId::default();
    Ok(catalog_reader
        .get_edge_label(txn, rel_type)?
        .map(|desc| vec![desc.table_id])
        .unwrap_or_default())
}

fn list_all_node_label_table_ids(catalog_reader: &dyn CatalogReader) -> DbResult<Vec<RelationId>> {
    let txn = TxnId::default();
    let mut table_ids = Vec::new();
    for label in catalog_reader.list_node_labels(txn)? {
        if !table_ids.contains(&label.table_id) {
            table_ids.push(label.table_id);
        }
    }
    Ok(table_ids)
}

fn push_required_table_privilege(
    reqs: &mut Vec<(CatalogPrivilege, RelationId)>,
    privilege: CatalogPrivilege,
    table_id: RelationId,
) {
    if !reqs.contains(&(privilege, table_id)) {
        reqs.push((privilege, table_id));
    }
}

fn push_variable_table_ids(
    variable_tables: &mut BTreeMap<String, Vec<RelationId>>,
    variable: &str,
    table_ids: &[RelationId],
) {
    if table_ids.is_empty() {
        return;
    }
    let entry = variable_tables.entry(variable.to_owned()).or_default();
    for table_id in table_ids {
        if !entry.contains(table_id) {
            entry.push(*table_id);
        }
    }
}

fn required_hybrid_function_privileges(
    catalog_reader: &dyn CatalogReader,
    function_name: &str,
    args: &[TypedExpr],
) -> DbResult<Vec<(CatalogPrivilege, RelationId)>> {
    let txn = TxnId::default();
    let catalog_has_roles = catalog_has_any_roles(catalog_reader)?;

    if hybrid_function_name_matches(function_name, "vector_top_k_ids")
        || hybrid_function_name_matches(function_name, "vector_top_k_hits")
        || hybrid_function_name_matches(function_name, "vector_prefetch_top_k_hits")
        || hybrid_function_name_matches(function_name, "vector_recommend_top_k_hits")
        || hybrid_function_name_matches(function_name, "full_text_top_k_hits")
        || hybrid_function_name_matches(function_name, "hybrid_search_top_k_hits")
    {
        let function_base_name = if hybrid_function_name_matches(function_name, "vector_top_k_hits")
        {
            "vector_top_k_hits"
        } else if hybrid_function_name_matches(function_name, "vector_prefetch_top_k_hits") {
            "vector_prefetch_top_k_hits"
        } else if hybrid_function_name_matches(function_name, "vector_recommend_top_k_hits") {
            "vector_recommend_top_k_hits"
        } else if hybrid_function_name_matches(function_name, "full_text_top_k_hits") {
            "full_text_top_k_hits"
        } else if hybrid_function_name_matches(function_name, "hybrid_search_top_k_hits") {
            "hybrid_search_top_k_hits"
        } else {
            "vector_top_k_ids"
        };
        let Some(table_name) = constant_text_arg(args.first()) else {
            if catalog_has_roles {
                return Err(DbError::insufficient_privilege(
                    format!(
                        "{function_base_name}() target relation must be a string literal when ACLs are active"
                    ),
                ));
            }
            return Ok(Vec::new());
        };
        let relation_name = QualifiedName::parse(table_name);
        if catalog_has_roles && relation_name.schema.as_ref().is_none() {
            return Err(DbError::insufficient_privilege(
                format!(
                    "{function_base_name}() requires a schema-qualified table name when ACLs are active"
                ),
            ));
        }
        let table = catalog_reader.get_table(txn, &relation_name)?;
        return Ok(table
            .map(|table| vec![(CatalogPrivilege::Select, table.table_id)])
            .unwrap_or_default());
    }

    if hybrid_function_name_matches(function_name, "graph_neighbors") {
        let Some(edge_label) = constant_text_arg(args.first()) else {
            if catalog_has_roles {
                return Err(DbError::insufficient_privilege(
                    "graph_neighbors() edge label must be a string literal when ACLs are active",
                ));
            }
            return Ok(Vec::new());
        };
        let edge = catalog_reader.get_edge_label(txn, edge_label)?;
        return Ok(edge
            .map(|edge| vec![(CatalogPrivilege::Select, edge.table_id)])
            .unwrap_or_default());
    }

    Ok(Vec::new())
}

fn hybrid_function_name_matches(function_name: &str, target: &str) -> bool {
    function_name.eq_ignore_ascii_case(target)
        || function_name
            .rsplit('.')
            .next()
            .is_some_and(|name| name.eq_ignore_ascii_case(target))
}

fn constant_text_arg(expr: Option<&TypedExpr>) -> Option<&str> {
    match expr?.kind.as_literal()? {
        aiondb_core::Value::Text(text) => Some(text.as_str()),
        _ => None,
    }
}

fn push_required_hybrid_privileges(
    reqs: &mut Vec<(CatalogPrivilege, RelationId)>,
    additions: Vec<(CatalogPrivilege, RelationId)>,
) {
    for addition in additions {
        if !reqs.contains(&addition) {
            reqs.push(addition);
        }
    }
}

fn collect_required_hybrid_privileges_from_physical_plan(
    catalog_reader: &dyn CatalogReader,
    plan: &PhysicalPlan,
    reqs: &mut Vec<(CatalogPrivilege, RelationId)>,
) -> DbResult<()> {
    let _guard = enter_auth_depth(
        &AUTH_HYBRID_PLAN_DEPTH,
        MAX_AUTH_HYBRID_PLAN_DEPTH,
        "hybrid physical plan",
    )?;
    match plan {
        PhysicalPlan::ProjectOnce {
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct_on,
            ..
        }
        | PhysicalPlan::ProjectTable {
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct_on,
            ..
        }
        | PhysicalPlan::ProjectSource {
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct_on,
            ..
        }
        | PhysicalPlan::NestedLoopJoin {
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct_on,
            ..
        }
        | PhysicalPlan::HashJoin {
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct_on,
            ..
        } => {
            if let PhysicalPlan::ProjectSource { source, .. } = plan {
                collect_required_hybrid_privileges_from_physical_plan(
                    catalog_reader,
                    source,
                    reqs,
                )?;
            }
            if let PhysicalPlan::NestedLoopJoin {
                left,
                right,
                condition,
                ..
            }
            | PhysicalPlan::HashJoin {
                left,
                right,
                condition,
                ..
            } = plan
            {
                collect_required_hybrid_privileges_from_physical_plan(catalog_reader, left, reqs)?;
                collect_required_hybrid_privileges_from_physical_plan(catalog_reader, right, reqs)?;
                collect_required_hybrid_privileges_from_optional_expr(
                    catalog_reader,
                    condition.as_ref(),
                    reqs,
                )?;
            }
            collect_required_hybrid_privileges_from_projection_exprs(
                catalog_reader,
                outputs,
                reqs,
            )?;
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                filter.as_ref(),
                reqs,
            )?;
            collect_required_hybrid_privileges_from_sort_exprs(catalog_reader, order_by, reqs)?;
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                limit.as_ref(),
                reqs,
            )?;
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                offset.as_ref(),
                reqs,
            )?;
            collect_required_hybrid_privileges_from_exprs(catalog_reader, distinct_on, reqs)?;
        }
        PhysicalPlan::MergeJoin {
            left,
            right,
            residual,
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct_on,
            ..
        } => {
            collect_required_hybrid_privileges_from_physical_plan(catalog_reader, left, reqs)?;
            collect_required_hybrid_privileges_from_physical_plan(catalog_reader, right, reqs)?;
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                residual.as_ref(),
                reqs,
            )?;
            collect_required_hybrid_privileges_from_projection_exprs(
                catalog_reader,
                outputs,
                reqs,
            )?;
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                filter.as_ref(),
                reqs,
            )?;
            collect_required_hybrid_privileges_from_sort_exprs(catalog_reader, order_by, reqs)?;
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                limit.as_ref(),
                reqs,
            )?;
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                offset.as_ref(),
                reqs,
            )?;
            collect_required_hybrid_privileges_from_exprs(catalog_reader, distinct_on, reqs)?;
        }
        PhysicalPlan::Aggregate {
            group_by,
            grouping_sets: _,
            aggregates,
            having,
            filter,
            order_by,
            limit,
            offset,
            distinct_on,
            ..
        }
        | PhysicalPlan::AggregateSource {
            group_by,
            grouping_sets: _,
            aggregates,
            having,
            filter,
            order_by,
            limit,
            offset,
            distinct_on,
            ..
        } => {
            if let PhysicalPlan::AggregateSource { source, .. } = plan {
                collect_required_hybrid_privileges_from_physical_plan(
                    catalog_reader,
                    source,
                    reqs,
                )?;
            }
            collect_required_hybrid_privileges_from_exprs(catalog_reader, group_by, reqs)?;
            collect_required_hybrid_privileges_from_projection_exprs(
                catalog_reader,
                aggregates,
                reqs,
            )?;
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                having.as_ref(),
                reqs,
            )?;
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                filter.as_ref(),
                reqs,
            )?;
            collect_required_hybrid_privileges_from_sort_exprs(catalog_reader, order_by, reqs)?;
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                limit.as_ref(),
                reqs,
            )?;
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                offset.as_ref(),
                reqs,
            )?;
            collect_required_hybrid_privileges_from_exprs(catalog_reader, distinct_on, reqs)?;
        }
        PhysicalPlan::InsertValues {
            rows,
            on_conflict,
            returning,
            ..
        } => {
            for row in rows {
                collect_required_hybrid_privileges_from_exprs(catalog_reader, row, reqs)?;
            }
            collect_required_hybrid_privileges_from_on_conflict(
                catalog_reader,
                on_conflict.as_ref(),
                reqs,
            )?;
            collect_required_hybrid_privileges_from_projection_exprs(
                catalog_reader,
                returning,
                reqs,
            )?;
        }
        PhysicalPlan::InsertSelect {
            assignments,
            source,
            on_conflict,
            returning,
            ..
        } => {
            collect_required_hybrid_privileges_from_exprs(catalog_reader, assignments, reqs)?;
            collect_required_hybrid_privileges_from_physical_plan(catalog_reader, source, reqs)?;
            collect_required_hybrid_privileges_from_on_conflict(
                catalog_reader,
                on_conflict.as_ref(),
                reqs,
            )?;
            collect_required_hybrid_privileges_from_projection_exprs(
                catalog_reader,
                returning,
                reqs,
            )?;
        }
        PhysicalPlan::DeleteFromTable {
            filter, returning, ..
        } => {
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                filter.as_ref(),
                reqs,
            )?;
            collect_required_hybrid_privileges_from_projection_exprs(
                catalog_reader,
                returning,
                reqs,
            )?;
        }
        PhysicalPlan::UpdateTable {
            assignments,
            filter,
            returning,
            ..
        } => {
            collect_required_hybrid_privileges_from_assignments(catalog_reader, assignments, reqs)?;
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                filter.as_ref(),
                reqs,
            )?;
            collect_required_hybrid_privileges_from_projection_exprs(
                catalog_reader,
                returning,
                reqs,
            )?;
        }
        PhysicalPlan::DistributedScan {
            outputs, filter, ..
        } => {
            collect_required_hybrid_privileges_from_projection_exprs(
                catalog_reader,
                outputs,
                reqs,
            )?;
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                filter.as_ref(),
                reqs,
            )?;
        }
        PhysicalPlan::PartialAggregate {
            source, group_by, ..
        } => {
            collect_required_hybrid_privileges_from_physical_plan(catalog_reader, source, reqs)?;
            collect_required_hybrid_privileges_from_exprs(catalog_reader, group_by, reqs)?;
        }
        PhysicalPlan::FinalAggregate {
            partials,
            group_by,
            having,
            order_by,
            limit,
            offset,
            ..
        } => {
            for partial in partials {
                collect_required_hybrid_privileges_from_physical_plan(
                    catalog_reader,
                    partial,
                    reqs,
                )?;
            }
            collect_required_hybrid_privileges_from_exprs(catalog_reader, group_by, reqs)?;
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                having.as_ref(),
                reqs,
            )?;
            collect_required_hybrid_privileges_from_sort_exprs(catalog_reader, order_by, reqs)?;
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                limit.as_ref(),
                reqs,
            )?;
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                offset.as_ref(),
                reqs,
            )?;
        }
        PhysicalPlan::BroadcastHashJoin {
            broadcast,
            local,
            left_keys,
            right_keys,
            condition,
            outputs,
            ..
        } => {
            collect_required_hybrid_privileges_from_physical_plan(catalog_reader, broadcast, reqs)?;
            collect_required_hybrid_privileges_from_physical_plan(catalog_reader, local, reqs)?;
            collect_required_hybrid_privileges_from_exprs(catalog_reader, left_keys, reqs)?;
            collect_required_hybrid_privileges_from_exprs(catalog_reader, right_keys, reqs)?;
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                condition.as_ref(),
                reqs,
            )?;
            collect_required_hybrid_privileges_from_projection_exprs(
                catalog_reader,
                outputs,
                reqs,
            )?;
        }
        PhysicalPlan::SetOperation {
            left,
            right,
            order_by,
            limit,
            offset,
            ..
        } => {
            collect_required_hybrid_privileges_from_physical_plan(catalog_reader, left, reqs)?;
            collect_required_hybrid_privileges_from_physical_plan(catalog_reader, right, reqs)?;
            collect_required_hybrid_privileges_from_sort_exprs(catalog_reader, order_by, reqs)?;
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                limit.as_ref(),
                reqs,
            )?;
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                offset.as_ref(),
                reqs,
            )?;
        }
        PhysicalPlan::ProjectValues {
            rows,
            order_by,
            limit,
            offset,
            ..
        } => {
            for row in rows {
                collect_required_hybrid_privileges_from_exprs(catalog_reader, row, reqs)?;
            }
            collect_required_hybrid_privileges_from_sort_exprs(catalog_reader, order_by, reqs)?;
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                limit.as_ref(),
                reqs,
            )?;
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                offset.as_ref(),
                reqs,
            )?;
        }
        PhysicalPlan::MergeTable(merge) => {
            collect_required_hybrid_privileges_from_merge_plan(catalog_reader, merge, reqs)?;
        }
        PhysicalPlan::RecursiveCte {
            base, recursive, ..
        } => {
            collect_required_hybrid_privileges_from_physical_plan(catalog_reader, base, reqs)?;
            collect_required_hybrid_privileges_from_physical_plan(catalog_reader, recursive, reqs)?;
        }
        PhysicalPlan::HybridFunctionScan {
            function_name,
            args,
            ..
        } => {
            push_required_hybrid_privileges(
                reqs,
                required_hybrid_function_privileges(catalog_reader, function_name, args)?,
            );
            collect_required_hybrid_privileges_from_exprs(catalog_reader, args, reqs)?;
        }
        PhysicalPlan::CreateTableAs { source, .. } => {
            collect_required_hybrid_privileges_from_physical_plan(catalog_reader, source, reqs)?;
        }
        PhysicalPlan::CypherQuery(query) => {
            collect_required_hybrid_privileges_from_cypher_query(catalog_reader, query, reqs)?;
        }
        _ => {}
    }
    Ok(())
}

include!("catalog_authorizer_hybrid_exprs.rs");
