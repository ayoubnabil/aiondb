use std::{collections::HashMap, sync::Arc};

use aiondb_catalog::{CatalogPrivilege, CatalogReader, PrivilegeTarget, QualifiedName};
use aiondb_core::{
    convert::u32_to_i32_saturating, convert::usize_to_i32_saturating, DbResult, TxnId,
    COMPAT_PG_DEFAULT_TABLESPACE_OID, COMPAT_PG_GLOBAL_TABLESPACE_OID,
};
use aiondb_plan::{LogicalPlan, ResultField, TypedExpr};

use super::extra_tables::typed_array_literal;
use super::matview::parse_matview_sidecar;
use super::*;

/// Compress a `CatalogPrivilege` to its canonical PG ACL character. See
/// the PG docs for the full mapping; `Self::All` expands to every relation
/// privilege combined into a single bag (`arwdDxt`).
fn catalog_privilege_acl_chars(privilege: CatalogPrivilege) -> &'static str {
    match privilege {
        CatalogPrivilege::Select => "r",
        CatalogPrivilege::Insert => "a",
        CatalogPrivilege::Update => "w",
        CatalogPrivilege::Delete => "d",
        CatalogPrivilege::Truncate => "D",
        CatalogPrivilege::References => "x",
        CatalogPrivilege::Trigger => "t",
        CatalogPrivilege::Execute => "X",
        CatalogPrivilege::Usage => "U",
        CatalogPrivilege::Connect => "c",
        CatalogPrivilege::Create => "C",
        CatalogPrivilege::Temporary => "T",
        // ALL on a table expands to the 7 relation privileges.
        CatalogPrivilege::All => "arwdDxt",
    }
}

/// Stable hash from tablespace name → synthetic OID (matches the
/// convention used by `pg_catalog_extra2::synth_oid_from_name`, kept
/// local to avoid crossing module visibility).
fn synth_tablespace_oid(name: &str) -> i32 {
    let lower = name.to_ascii_lowercase();
    if lower == "pg_default" {
        return COMPAT_PG_DEFAULT_TABLESPACE_OID;
    }
    if lower == "pg_global" {
        return COMPAT_PG_GLOBAL_TABLESPACE_OID;
    }
    let mut hash: u32 = 0x811c_9dc5;
    for byte in lower.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    ((hash & 0x7fff_ffff) | 0x8000).cast_signed()
}

fn pg_vector_literal(lower_bound: i64, elements: Vec<Value>) -> Value {
    if elements.is_empty() {
        return Value::Array(elements);
    }
    let upper_bound = lower_bound + i64::try_from(elements.len()).unwrap_or(0) - 1;
    Value::Text(format!(
        "[{lower_bound}:{upper_bound}]={}",
        Value::Array(elements)
    ))
}

// Well-known PostgreSQL system catalog tables with their OIDs.
// These are emitted as rows in pg_class so that queries like
// `SELECT oid FROM pg_class WHERE relname = 'pg_class'` return
// the expected results.
//
// Exposed publicly because the executor needs the same canonical mapping
// to resolve `to_regclass('pg_namespace')`-style lookups; without it the
// executor falls back on AionDB's synthetic 60_000+ IDs and the OID a
// client sees from `pg_class` does not match the one from `to_regclass`.
pub const SYSTEM_CATALOG_TABLES: &[(i32, &str)] = &[
    (1247, "pg_type"),
    (1249, "pg_attribute"),
    (1255, "pg_proc"),
    (1259, "pg_class"),
    (2604, "pg_attrdef"),
    (2606, "pg_constraint"),
    (2608, "pg_depend"),
    (2610, "pg_index"),
    (2611, "pg_inherits"),
    (2615, "pg_namespace"),
    (2617, "pg_operator"),
    (3592, "pg_range"),
    (2612, "pg_language"),
    (1260, "pg_authid"),
    (2964, "pg_database"),
    (2396, "pg_shdepend"),
    (1213, "pg_tablespace"),
    (2600, "pg_aggregate"),
    (2601, "pg_am"),
    (2602, "pg_amop"),
    (2603, "pg_amproc"),
    (2616, "pg_opclass"),
    (2753, "pg_opfamily"),
    (1136, "pg_description"),
    (3596, "pg_seclabel"),
    (2995, "pg_shdescription"),
    (3602, "pg_ts_config"),
    (3600, "pg_ts_dict"),
    (3601, "pg_ts_parser"),
    (3764, "pg_ts_template"),
    (2328, "pg_foreign_data_wrapper"),
    (1417, "pg_foreign_server"),
    (2830, "pg_foreign_table"),
    (2224, "pg_sequence"),
    (6104, "pg_publication"),
    (6106, "pg_subscription"),
    (3456, "pg_collation"),
    (6243, "pg_parameter_acl"),
    (826, "pg_default_acl"),
    (3394, "pg_init_privs"),
    (6000, "pg_replication_origin"),
    (3256, "pg_policy"),
    (3350, "pg_partitioned_table"),
    (3596, "pg_transform"),
    (6100, "pg_publication_rel"),
    (6237, "pg_publication_namespace"),
    (3079, "pg_extension"),
    (3429, "pg_event_trigger"),
    (2609, "pg_cast"),
    (1262, "pg_statistic"),
    (3381, "pg_statistic_ext"),
    (3379, "pg_statistic_ext_data"),
    (2618, "pg_rewrite"),
    (2620, "pg_trigger"),
];

// ---------------------------------------------------------------
// pg_catalog.pg_class
// ---------------------------------------------------------------

pub(super) fn pg_class_fields() -> Vec<ResultField> {
    vec![
        oid_field("oid"),
        name_field("relname"),
        oid_field("relnamespace"),
        internal_char_field("relkind"),
        oid_field("relowner"),
        double_field("reltuples"),
        bool_field("relhasindex"),
        oid_field("reltablespace"),
        oid_field("relam"),
        oid_field("relfilenode"),
        int_field("relpages"),
        internal_char_field("relpersistence"),
        bool_field("relisshared"),
        bool_field("relhasrules"),
        bool_field("relhastriggers"),
        bool_field("relhassubclass"),
        bool_field("relrowsecurity"),
        bool_field("relforcerowsecurity"),
        bool_field("relispartition"),
        int_field("relnatts"),
        int_field("relchecks"),
        bool_field("relispopulated"),
        // `reltoastrelid` is read by many PG client apps (psql \d+, ORMs).
        // AionDB does not maintain TOAST sidecars so we always report 0
        // (no toast table), matching PG's behavior for small relations.
        oid_field("reltoastrelid"),
        ResultField {
            name: "reloptions".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Text)),
            text_type_modifier: None,
            nullable: true,
        },
        oid_field("reltype"),
        oid_field("reloftype"),
        int_field("relallvisible"),
        internal_char_field("relreplident"),
        oid_field("relrewrite"),
        int_field("relfrozenxid"),
        int_field("relminmxid"),
        ResultField {
            name: "relacl".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Text)),
            text_type_modifier: None,
            nullable: true,
        },
        nullable_text_field("relpartbound"),
    ]
}

pub(super) fn build_pg_class_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
    owner_oid: i32,
) -> DbResult<LogicalPlan> {
    let output_fields = pg_class_fields();
    let tables = list_user_tables(catalog, txn_id, default_schema)?;
    let sequences = list_user_sequences(catalog, txn_id, default_schema)?;
    let mut rows: Vec<Vec<TypedExpr>> = Vec::with_capacity(tables.len());
    let views = list_user_views(catalog, txn_id, default_schema)?;
    let mut ordinary_views = Vec::new();
    let mut matview_sidecars = HashMap::new();

    for view in views {
        if let Some(metadata) = parse_matview_sidecar(&view) {
            matview_sidecars.insert(metadata.relation_name.clone(), metadata);
        } else {
            ordinary_views.push(view);
        }
    }

    // Pre-compute ACL entries per table: walk every role's privileges and
    // group by table name. `pg_class.relacl` is filled from catalog
    // privileges so GRANTs remain visible in catalog introspection.
    let acl_by_relation: HashMap<QualifiedName, Vec<String>> = {
        let mut per_role: HashMap<(QualifiedName, String), String> = HashMap::new();
        for role in catalog.list_roles(txn_id)? {
            for privilege in catalog.get_privileges(txn_id, &role.name)? {
                let PrivilegeTarget::Table(relation) = privilege.target else {
                    continue;
                };
                let chars = catalog_privilege_acl_chars(privilege.privilege);
                per_role
                    .entry((relation, role.name.clone()))
                    .or_default()
                    .push_str(chars);
            }
        }
        let mut out: HashMap<QualifiedName, Vec<String>> = HashMap::new();
        for ((relation, role), mut chars) in per_role {
            // Deduplicate chars while preserving PG's canonical order:
            // arwdDxt (+XUCcTRt for other kinds).
            let mut seen: Vec<char> = Vec::new();
            for ch in chars.chars() {
                if !seen.contains(&ch) {
                    seen.push(ch);
                }
            }
            chars = seen.into_iter().collect();
            // PG format: "role=perms/grantor". We don't track grantor in
            // the catalog yet; leave empty (clients accept that).
            out.entry(relation)
                .or_default()
                .push(format!("{role}={chars}/"));
        }
        out
    };

    // Resolve per-table RLS flags + reloptions + tablespace from the
    // session compat registry once, keyed by lowercase relation name.
    // `CREATE TABLE` attrs carry the currently-supported compat
    // ALTER TABLE metadata. Keep `ALTER TABLE` as a fallback for
    // pre-migration sessions that still stored those keys there.
    let (rls_flags, rules_by_relation, reloptions_by_relation, tablespace_by_relation) =
        aiondb_eval::with_current_session_context(|context| {
            let mut rls_map: std::collections::HashMap<String, (bool, bool)> =
                std::collections::HashMap::new();
            let mut reloptions_map: std::collections::HashMap<String, Vec<String>> =
                std::collections::HashMap::new();
            let mut tablespace_map: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            for ((kind, name), (_, _, _, options_joined, tablespace, _)) in
                context.compat_misc_attrs.iter()
            {
                if kind != "CREATE TABLE" && kind != "ALTER TABLE" {
                    continue;
                }
                if !tablespace.is_empty() {
                    tablespace_map.insert(name.clone(), tablespace.clone());
                }
                let mut rls_enabled = false;
                let mut rls_force = false;
                let mut reloptions: Vec<String> = Vec::new();
                for pair in options_joined.split(',').map(str::trim) {
                    if pair.is_empty() {
                        continue;
                    }
                    if let Some(value) = pair.strip_prefix("rls=") {
                        rls_enabled = !matches!(value, "disabled" | "");
                        continue;
                    }
                    if let Some(value) = pair.strip_prefix("rls_force=") {
                        rls_force = matches!(value, "force");
                        continue;
                    }
                    // Exclude state-flavoured keys that are not
                    // reloptions. Everything else (fillfactor=…,
                    // autovacuum_vacuum_scale_factor=…, etc.) round-trips
                    // through pg_class.reloptions.
                    let is_state = pair.starts_with("rule:")
                        || pair.starts_with("inherits=")
                        || pair.starts_with("cluster=")
                        || pair.starts_with("replica_identity=")
                        || pair.starts_with("access_method=")
                        || pair.starts_with("of=")
                        || pair.starts_with("logged=")
                        || pair.starts_with("partition=");
                    if !is_state {
                        reloptions.push(pair.to_owned());
                    }
                }
                if rls_enabled || rls_force {
                    rls_map.insert(name.clone(), (rls_enabled, rls_force));
                }
                if !reloptions.is_empty() {
                    reloptions_map.insert(name.clone(), reloptions);
                }
            }
            // Each real rewrite-rule entry has a key whose first component
            // is the relation name (non-empty action). Rule-name registry
            // entries prefix the relation with a sentinel; skip them.
            let mut rules: std::collections::HashSet<String> = std::collections::HashSet::new();
            for ((relation, _event), action_sql) in context.compat_rules.iter() {
                if relation.starts_with("__aiondb_rule_name_registry__.") || action_sql.is_empty() {
                    continue;
                }
                rules.insert(relation.clone());
            }
            (rls_map, rules, reloptions_map, tablespace_map)
        });

    for table in &tables {
        let table_oid = relation_id_to_oid(table);
        let ns_oid = schema_id_to_namespace_oid(table);
        let has_index = !catalog.list_indexes(txn_id, table.table_id)?.is_empty();
        let num_cols = usize_to_i32_saturating(table.columns.len());
        let num_checks = usize_to_i32_saturating(table.check_constraints.len());
        let matview = matview_sidecars.get(&table.name);
        let lc_name = table.name.object_name().to_ascii_lowercase();
        let (rls, rls_force) = rls_flags.get(&lc_name).copied().unwrap_or((false, false));
        let has_rules = rules_by_relation.contains(&lc_name);
        let has_triggers = !catalog
            .list_triggers(txn_id, &table.name.to_string())?
            .is_empty();
        let reloptions = reloptions_by_relation.get(&lc_name).cloned();
        let reltablespace_oid = tablespace_by_relation
            .get(&lc_name)
            .map(|ts| synth_tablespace_oid(ts))
            .unwrap_or(0);
        let relacl = acl_by_relation.get(&table.name).cloned();
        rows.push(class_table_row(
            table_oid,
            table.name.object_name(),
            ns_oid,
            owner_oid,
            has_index,
            num_cols,
            num_checks,
            if matview.is_some() { "m" } else { "r" },
            matview.map_or(true, |metadata| metadata.relispopulated),
            rls,
            rls_force,
            has_rules,
            has_triggers,
            reloptions,
            reltablespace_oid,
            relacl,
        ));

        // Emit an 'i' row for each index on this table.
        let indexes = catalog.list_indexes(txn_id, table.table_id)?;
        for idx in &indexes {
            let idx_oid = index_id_to_oid(idx);
            rows.push(class_index_row(
                idx_oid,
                idx.name.object_name(),
                ns_oid,
                owner_oid,
            ));
        }
    }

    // Emit a 'v' row for each view
    for view in &ordinary_views {
        let view_oid = view_id_to_oid(view);
        let ns_oid = view_schema_id_to_namespace_oid(view);
        let num_view_cols = usize_to_i32_saturating(view.columns.len());
        let view_acl = acl_by_relation.get(&view.name).cloned();
        rows.push(class_view_row(
            view_oid,
            view.name.object_name(),
            ns_oid,
            owner_oid,
            num_view_cols,
            view_acl,
        ));
    }

    for sequence in &sequences {
        let sequence_oid = sequence_id_to_oid(sequence);
        let ns_oid = if sequence.schema_id.get() == 1 {
            PUBLIC_NAMESPACE_OID
        } else {
            u64_to_i32_saturating(sequence.schema_id.get()).saturating_add(16384)
        };
        let relowner = sequence
            .owner
            .as_deref()
            .map(aiondb_core::compat_role_oid)
            .unwrap_or(owner_oid);
        rows.push(class_sequence_row(
            sequence_oid,
            sequence.name.object_name(),
            ns_oid,
            relowner,
        ));
    }

    // Emit rows for well-known pg_catalog system tables so that queries
    // like `SELECT oid FROM pg_class WHERE relname = 'pg_class'` return
    // the expected OIDs, matching real PostgreSQL behaviour.
    let pg_ns = PG_CATALOG_NAMESPACE_OID;
    for &(sys_oid, sys_name) in SYSTEM_CATALOG_TABLES {
        rows.push(class_table_row(
            sys_oid, sys_name, pg_ns, owner_oid, true,  // relhasindex
            0,     // relnatts (not critical)
            0,     // relchecks
            "r",   // relkind = ordinary table
            true,  // relispopulated
            false, // relrowsecurity
            false, // relforcerowsecurity
            false, // relhasrules
            false, // relhastriggers
            None,  // reloptions
            0,     // reltablespace
            None,  // relacl
        ));
    }

    Ok(project_values(output_fields, rows))
}

#[allow(clippy::fn_params_excessive_bools)]
fn class_table_row(
    oid: i32,
    name: &str,
    ns: i32,
    owner_oid: i32,
    has_idx: bool,
    ncols: i32,
    nchecks: i32,
    relkind: &str,
    relispopulated: bool,
    relrowsecurity: bool,
    relforcerowsecurity: bool,
    relhasrules: bool,
    relhastriggers: bool,
    reloptions: Option<Vec<String>>,
    reltablespace: i32,
    relacl: Option<Vec<String>>,
) -> Vec<TypedExpr> {
    let reloptions_literal = match reloptions {
        Some(options) if !options.is_empty() => {
            let values: Vec<aiondb_core::Value> =
                options.into_iter().map(aiondb_core::Value::Text).collect();
            typed_array_literal(values, DataType::Text)
        }
        _ => null_literal(DataType::Array(Box::new(DataType::Text))),
    };
    let relacl_literal = match relacl {
        Some(entries) if !entries.is_empty() => {
            let values: Vec<aiondb_core::Value> =
                entries.into_iter().map(aiondb_core::Value::Text).collect();
            typed_array_literal(values, DataType::Text)
        }
        _ => null_literal(DataType::Array(Box::new(DataType::Text))),
    };
    vec![
        int_literal(oid),
        text_literal(name),
        int_literal(ns),
        text_literal(relkind),
        int_literal(owner_oid),
        double_literal(-1.0),
        bool_literal(has_idx),
        int_literal(reltablespace),
        int_literal(0),
        int_literal(oid),
        int_literal(0),
        text_literal("p"),
        bool_literal(false),
        bool_literal(relhasrules),
        bool_literal(relhastriggers),
        bool_literal(false),
        bool_literal(relrowsecurity),
        bool_literal(relforcerowsecurity),
        bool_literal(false),
        int_literal(ncols),
        int_literal(nchecks),
        bool_literal(relispopulated),
        // reltoastrelid: AionDB never emits sidecar TOAST tables, so 0.
        int_literal(0),
        reloptions_literal,
        int_literal(oid),
        int_literal(0),
        int_literal(0),
        text_literal("d"),
        int_literal(0),
        int_literal(0),
        int_literal(0),
        relacl_literal,
        null_literal(DataType::Text),
    ]
}

fn class_index_row(oid: i32, name: &str, ns: i32, owner_oid: i32) -> Vec<TypedExpr> {
    vec![
        int_literal(oid),
        text_literal(name),
        int_literal(ns),
        text_literal("i"),
        int_literal(owner_oid),
        double_literal(0.0),
        bool_literal(false),
        int_literal(0),
        int_literal(403),
        int_literal(oid),
        int_literal(1),
        text_literal("p"),
        bool_literal(false),
        bool_literal(false),
        bool_literal(false),
        bool_literal(false),
        bool_literal(false),
        bool_literal(false),
        bool_literal(false),
        int_literal(0),
        int_literal(0),
        bool_literal(true),
        // reltoastrelid
        int_literal(0),
        null_literal(DataType::Array(Box::new(DataType::Text))),
        int_literal(0),
        int_literal(0),
        int_literal(0),
        text_literal("d"),
        int_literal(0),
        int_literal(0),
        int_literal(0),
        null_literal(DataType::Array(Box::new(DataType::Text))),
        null_literal(DataType::Text),
    ]
}

fn class_view_row(
    oid: i32,
    name: &str,
    ns: i32,
    owner_oid: i32,
    ncols: i32,
    relacl: Option<Vec<String>>,
) -> Vec<TypedExpr> {
    let relacl_literal = match relacl {
        Some(entries) if !entries.is_empty() => {
            let values: Vec<aiondb_core::Value> =
                entries.into_iter().map(aiondb_core::Value::Text).collect();
            typed_array_literal(values, DataType::Text)
        }
        _ => null_literal(DataType::Array(Box::new(DataType::Text))),
    };
    vec![
        int_literal(oid),
        text_literal(name),
        int_literal(ns),
        text_literal("v"),
        int_literal(owner_oid),
        double_literal(0.0),
        bool_literal(false),
        int_literal(0),
        int_literal(0),
        int_literal(oid),
        int_literal(0),
        text_literal("p"),
        bool_literal(false),
        bool_literal(true),
        bool_literal(false),
        bool_literal(false),
        bool_literal(false),
        bool_literal(false),
        bool_literal(false),
        int_literal(ncols),
        int_literal(0),
        bool_literal(true),
        // reltoastrelid
        int_literal(0),
        null_literal(DataType::Array(Box::new(DataType::Text))),
        int_literal(oid),
        int_literal(0),
        int_literal(0),
        text_literal("d"),
        int_literal(0),
        int_literal(0),
        int_literal(0),
        relacl_literal,
        null_literal(DataType::Text),
    ]
}

fn class_sequence_row(oid: i32, name: &str, ns: i32, owner_oid: i32) -> Vec<TypedExpr> {
    vec![
        int_literal(oid),
        text_literal(name),
        int_literal(ns),
        text_literal("S"),
        int_literal(owner_oid),
        double_literal(0.0),
        bool_literal(false),
        int_literal(0),
        int_literal(0),
        int_literal(oid),
        int_literal(0),
        text_literal("p"),
        bool_literal(false),
        bool_literal(false),
        bool_literal(false),
        bool_literal(false),
        bool_literal(false),
        bool_literal(false),
        bool_literal(false),
        int_literal(0),
        int_literal(0),
        bool_literal(true),
        int_literal(0),
        null_literal(DataType::Array(Box::new(DataType::Text))),
        int_literal(oid),
        int_literal(0),
        int_literal(0),
        text_literal("d"),
        int_literal(0),
        int_literal(0),
        int_literal(0),
        null_literal(DataType::Array(Box::new(DataType::Text))),
        null_literal(DataType::Text),
    ]
}

fn double_literal(n: f64) -> TypedExpr {
    TypedExpr::literal(
        aiondb_core::Value::Double(n),
        aiondb_core::DataType::Double,
        false,
    )
}

/// List user views, filtered by the active tenant schema when set.
fn list_user_views(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
) -> DbResult<Vec<aiondb_catalog::ViewDescriptor>> {
    let tenant_filter = tenant_schema_filter(default_schema);
    let mut views = Vec::new();
    for schema in catalog.list_schemas(txn_id)? {
        if !super::schema_visible_with_tenant_filter(&schema.name, tenant_filter.as_deref()) {
            continue;
        }
        views.extend(catalog.list_views(txn_id, schema.schema_id)?);
    }
    Ok(views)
}

/// Deterministic OID derived from a view's `RelationId`.
fn view_id_to_oid(view: &aiondb_catalog::ViewDescriptor) -> i32 {
    u64_to_i32_saturating(view.view_id.get()).saturating_add(16384)
}

/// Map a view's `SchemaId` to the corresponding namespace OID.
fn view_schema_id_to_namespace_oid(view: &aiondb_catalog::ViewDescriptor) -> i32 {
    let sid = view.schema_id.get();
    if sid == 1 {
        PUBLIC_NAMESPACE_OID
    } else {
        u64_to_i32_saturating(sid).saturating_add(16384)
    }
}

// ---------------------------------------------------------------
// pg_catalog.pg_attribute
// ---------------------------------------------------------------

pub(super) fn pg_attribute_fields() -> Vec<ResultField> {
    vec![
        oid_field("attrelid"),
        name_field("attname"),
        oid_field("atttypid"),
        int_field("attnum"),
        bool_field("attnotnull"),
        bool_field("attisdropped"),
        int_field("atttypmod"),
        bool_field("atthasdef"),
        internal_char_field("attgenerated"),
        internal_char_field("attidentity"),
        oid_field("attcollation"),
        int_field("attlen"),
        internal_char_field("attalign"),
        internal_char_field("attstorage"),
        internal_char_field("attcompression"),
        int_field("attinhcount"),
        int_field("attstattarget"),
        bool_field("attislocal"),
        ResultField {
            name: "attacl".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Text)),
            text_type_modifier: None,
            nullable: true,
        },
    ]
}

pub(super) fn build_pg_attribute_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
) -> DbResult<LogicalPlan> {
    let output_fields = pg_attribute_fields();
    let tables = list_user_tables(catalog, txn_id, default_schema)?;
    let mut rows: Vec<Vec<TypedExpr>> = Vec::new();

    for table in &tables {
        let table_oid = relation_id_to_oid(table);
        for col in &table.columns {
            let type_oid = pg_attribute_type_oid(table, col);
            let has_default = col.default_value.is_some();
            let attlen = pg_attlen(&col.data_type);
            rows.push(vec![
                int_literal(table_oid),
                text_literal(&col.name),
                int_literal(type_oid),
                int_literal(u32_to_i32_saturating(col.ordinal_position)),
                bool_literal(!col.nullable),
                bool_literal(false), // attisdropped
                int_literal(pg_attribute_typmod(col)),
                bool_literal(has_default), // atthasdef
                text_literal(""),          // attgenerated (empty = regular)
                text_literal(""),          // attidentity (empty = regular)
                int_literal(0),            // attcollation (0 = default)
                int_literal(attlen),       // attlen
                text_literal("i"),         // attalign (int alignment)
                text_literal("p"),         // attstorage (plain)
                text_literal(""),          // attcompression (empty = none)
                int_literal(0),            // attinhcount
                int_literal(-1),           // attstattarget
                bool_literal(true),        // attislocal
                null_literal(DataType::Array(Box::new(DataType::Text))), // attacl
            ]);
        }
    }

    // Emit attribute rows for the well-known pg_catalog system tables.
    // psql `\d pg_class`, ORM probes joining pg_attribute against pg_class
    // for system tables (sqlx, Diesel, SQLAlchemy reflection) all expect
    // a non-empty result here.
    for &(sys_oid, sys_name) in SYSTEM_CATALOG_TABLES {
        let Some(fields) = super::output_fields_for(sys_name) else {
            continue;
        };
        for (idx, field) in fields.iter().enumerate() {
            let type_oid = result_field_pg_type_oid(field);
            let attlen = pg_attlen(&field.data_type);
            rows.push(vec![
                int_literal(sys_oid),
                text_literal(&field.name),
                int_literal(type_oid),
                int_literal(u32_to_i32_saturating(
                    u32::try_from(idx + 1).unwrap_or(u32::MAX),
                )),
                bool_literal(!field.nullable),
                bool_literal(false),
                int_literal(
                    field
                        .text_type_modifier
                        .map_or(-1, aiondb_core::TextTypeModifier::atttypmod),
                ),
                bool_literal(false),
                text_literal(""),
                text_literal(""),
                int_literal(0),
                int_literal(attlen),
                text_literal("i"),
                text_literal("p"),
                text_literal(""),
                int_literal(0),
                int_literal(-1),
                bool_literal(true),
                null_literal(DataType::Array(Box::new(DataType::Text))),
            ]);
        }
    }

    Ok(project_values(output_fields, rows))
}

fn result_field_pg_type_oid(field: &ResultField) -> i32 {
    if let Some(modifier) = field.text_type_modifier {
        if matches!(field.data_type, DataType::Array(ref inner) if matches!(inner.as_ref(), DataType::Text))
        {
            return u32_to_i32_saturating(modifier.array_type_oid());
        }
        return u32_to_i32_saturating(modifier.scalar_type_oid());
    }
    field.data_type.pg_oid().map_or(0, u32_to_i32_saturating)
}

fn pg_attribute_type_oid(
    table: &aiondb_catalog::TableDescriptor,
    column: &aiondb_catalog::ColumnDescriptor,
) -> i32 {
    if let Some(domain_oid) = compat_domain_oid_for_column(table, column) {
        return domain_oid;
    }
    if let Some(compat_type_oid) = compat_user_type_oid_for_column(table, column) {
        return compat_type_oid;
    }
    if let Some(type_oid) = raw_text_type_oid(column.raw_type_name.as_deref()) {
        return type_oid;
    }
    match (&column.data_type, column.text_type_modifier) {
        (DataType::Text, Some(text_type_modifier)) => {
            u32_to_i32_saturating(text_type_modifier.scalar_type_oid())
        }
        (DataType::Array(inner), Some(text_type_modifier))
            if matches!(inner.as_ref(), DataType::Text) =>
        {
            u32_to_i32_saturating(text_type_modifier.array_type_oid())
        }
        _ => column.data_type.pg_oid().map_or(0, u32_to_i32_saturating),
    }
}

fn raw_text_type_oid(raw_type_name: Option<&str>) -> Option<i32> {
    let raw = raw_type_name?.trim().to_ascii_lowercase();
    if raw == "vector" || raw.starts_with("vector(") {
        return Some(COMPAT_PGVECTOR_VECTOR_OID);
    }
    if raw == "halfvec" || raw.starts_with("halfvec(") {
        return Some(COMPAT_PGVECTOR_HALFVEC_OID);
    }
    if raw == "sparsevec" || raw.starts_with("sparsevec(") {
        return Some(COMPAT_PGVECTOR_SPARSEVEC_OID);
    }
    match raw.as_str() {
        "smallint" | "int2" => Some(21),
        "smallint[]" | "int2[]" => Some(1005),
        "integer[]" | "int4[]" => Some(1007),
        "bigint[]" | "int8[]" => Some(1016),
        "boolean[]" | "bool[]" => Some(1000),
        "text[]" => Some(1009),
        "uuid[]" => Some(2951),
        "jsonb[]" => Some(3807),
        "varchar" | "character varying" => Some(1043),
        "varchar[]" | "character varying[]" => Some(1015),
        "\"char\"" | "char" | "character" | "bpchar" => Some(1042),
        "\"char\"[]" | "char[]" | "character[]" | "bpchar[]" => Some(1014),
        "bit" => Some(COMPAT_PG_BIT_OID),
        "bit[]" => Some(COMPAT_PG_BIT_ARRAY_OID),
        "varbit" | "bit varying" => Some(COMPAT_PG_VARBIT_OID),
        "varbit[]" | "bit varying[]" => Some(COMPAT_PG_VARBIT_ARRAY_OID),
        "vector[]" => Some(COMPAT_PGVECTOR_VECTOR_ARRAY_OID),
        "halfvec[]" => Some(COMPAT_PGVECTOR_HALFVEC_ARRAY_OID),
        "sparsevec[]" => Some(COMPAT_PGVECTOR_SPARSEVEC_ARRAY_OID),
        _ => None,
    }
}

fn compat_domain_identity(schema_name: Option<&str>, name: &str) -> String {
    match schema_name {
        Some(schema_name) if !schema_name.is_empty() => {
            format!(
                "{}.{}",
                schema_name.to_ascii_lowercase(),
                name.to_ascii_lowercase()
            )
        }
        _ => name.to_ascii_lowercase(),
    }
}

fn compat_domain_oid(schema_name: Option<&str>, name: &str) -> i32 {
    aiondb_core::compat_function_oid(&format!(
        "domain:{}",
        compat_domain_identity(schema_name, name)
    ))
}

fn compat_domain_oid_for_column(
    table: &aiondb_catalog::TableDescriptor,
    column: &aiondb_catalog::ColumnDescriptor,
) -> Option<i32> {
    let quoted_column = aiondb_parser::identifier::quote_identifier(&column.name);
    let pattern = format!("__aiondb_compat_cast({quoted_column}, '");
    let type_name = table.check_constraints.iter().find_map(|constraint| {
        let expr = constraint.expression.as_str();
        let start = expr.find(&pattern)?;
        let target = &expr[start + pattern.len()..];
        let target_start = target.find("', '")?;
        let domain = &target[target_start + 4..];
        let end = domain.find('\'')?;
        Some(aiondb_eval::normalize_compat_type_name(&domain[..end]))
    })?;
    aiondb_eval::with_current_session_context(|ctx| {
        ctx.domain_defs
            .iter()
            .find(|entry| {
                let bare_name = type_name
                    .rsplit_once('.')
                    .map_or(type_name.as_str(), |(_, tail)| tail);
                entry.name.eq_ignore_ascii_case(bare_name)
            })
            .map(|entry| compat_domain_oid(entry.schema_name.as_deref(), &entry.name))
    })
}

fn compat_user_type_oid_for_column(
    table: &aiondb_catalog::TableDescriptor,
    column: &aiondb_catalog::ColumnDescriptor,
) -> Option<i32> {
    let quoted_column = aiondb_parser::identifier::quote_identifier(&column.name);
    let pattern = format!("__aiondb_compat_cast({quoted_column}, 'text', '");
    let type_name = table.check_constraints.iter().find_map(|constraint| {
        let expr = constraint.expression.as_str();
        let start = expr.find(&pattern)?;
        let target = &expr[start + pattern.len()..];
        let end = target.find('\'')?;
        Some(aiondb_eval::normalize_compat_type_name(&target[..end]))
    })?;
    aiondb_eval::with_current_session_context(|ctx| {
        ctx.compat_user_types
            .iter()
            .find(|entry| entry.name.eq_ignore_ascii_case(&type_name))
            .map(|entry| entry.oid)
    })
}

fn pg_attribute_typmod(column: &aiondb_catalog::ColumnDescriptor) -> i32 {
    if let Some((precision, scale)) =
        numeric_raw_type_precision_scale(column.raw_type_name.as_deref())
    {
        return ((precision << 16) | scale) + 4;
    }
    if let Some(dims) = pgvector_raw_type_dims(column.raw_type_name.as_deref()) {
        return dims + 4;
    }
    column
        .text_type_modifier
        .map_or(-1, aiondb_core::TextTypeModifier::atttypmod)
}

fn pgvector_raw_type_dims(raw_type_name: Option<&str>) -> Option<i32> {
    let raw = raw_type_name?.trim().to_ascii_lowercase();
    for prefix in ["vector(", "halfvec(", "sparsevec("] {
        if let Some(suffix) = raw
            .strip_prefix(prefix)
            .and_then(|value| value.strip_suffix(')'))
        {
            let dims = suffix.trim().parse::<i32>().ok()?;
            if dims > 0 {
                return Some(dims);
            }
        }
    }
    None
}

fn numeric_raw_type_precision_scale(raw_type_name: Option<&str>) -> Option<(i32, i32)> {
    let raw = raw_type_name?.trim().to_ascii_lowercase();
    let suffix = raw.strip_prefix("numeric(")?.strip_suffix(')')?;
    if let Some((precision, scale)) = suffix.split_once(',') {
        let precision = precision.trim().parse::<i32>().ok()?;
        let scale = scale.trim().parse::<i32>().ok()?;
        return Some((precision, scale));
    }
    let precision = suffix.trim().parse::<i32>().ok()?;
    Some((precision, 0))
}

#[cfg(test)]
mod tests {
    use super::pgvector_raw_type_dims;

    #[test]
    fn pgvector_raw_type_dims_parses_vector_typmods() {
        assert_eq!(pgvector_raw_type_dims(Some("VECTOR(1536)")), Some(1536));
        assert_eq!(pgvector_raw_type_dims(Some("halfvec(4)")), Some(4));
        assert_eq!(pgvector_raw_type_dims(Some(" sparsevec(5) ")), Some(5));
        assert_eq!(pgvector_raw_type_dims(Some("vector")), None);
    }
}

/// Return the pg_attribute.attlen value for a data type.
fn pg_attlen(dt: &DataType) -> i32 {
    match dt {
        DataType::Boolean => 1,
        DataType::Int => 4,
        DataType::BigInt => 8,
        DataType::Real => 4,
        DataType::Double => 8,
        DataType::Date => 4,
        DataType::Time => 8,
        DataType::TimeTz => 12,
        DataType::Timestamp | DataType::TimestampTz => 8,
        DataType::Interval => 16,
        DataType::Uuid => 16,
        DataType::MacAddr => 6,
        DataType::MacAddr8 => 8,
        // Variable-length types
        _ => -1,
    }
}

// ---------------------------------------------------------------
// pg_catalog.pg_attrdef  (column defaults)
// ---------------------------------------------------------------

pub(super) fn pg_attrdef_fields() -> Vec<ResultField> {
    vec![
        oid_field("oid"),
        oid_field("adrelid"),
        int_field("adnum"),
        text_field("adbin"),
    ]
}

pub(super) fn build_pg_attrdef_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
) -> DbResult<LogicalPlan> {
    let output_fields = pg_attrdef_fields();
    let tables = list_user_tables(catalog, txn_id, default_schema)?;
    let mut rows: Vec<Vec<TypedExpr>> = Vec::new();
    let mut oid_counter = 1_i32;

    for table in &tables {
        let table_oid = relation_id_to_oid(table);
        for col in &table.columns {
            if let Some(default_expr) = &col.default_value {
                rows.push(vec![
                    int_literal(oid_counter),
                    int_literal(table_oid),
                    int_literal(u32_to_i32_saturating(col.ordinal_position)),
                    text_literal(default_expr),
                ]);
                oid_counter += 1;
            }
        }
    }

    Ok(project_values(output_fields, rows))
}

// ---------------------------------------------------------------
// pg_catalog.pg_index
// ---------------------------------------------------------------

pub(super) fn pg_index_fields() -> Vec<ResultField> {
    vec![
        oid_field("indexrelid"),
        oid_field("indrelid"),
        bool_field("indisunique"),
        bool_field("indisprimary"),
        ResultField {
            name: "indkey".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Int)),
            text_type_modifier: Some(TextTypeModifier::Int2Vector),
            nullable: false,
        },
        bool_field("indisexclusion"),
        bool_field("indimmediate"),
        bool_field("indisclustered"),
        bool_field("indisvalid"),
        bool_field("indcheckxmin"),
        bool_field("indisready"),
        bool_field("indislive"),
        bool_field("indisreplident"),
        bool_field("indnullsnotdistinct"),
        int_field("indnatts"),
        int_field("indnkeyatts"),
        ResultField {
            name: "indcollation".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Int)),
            text_type_modifier: Some(TextTypeModifier::OidVector),
            nullable: false,
        },
        ResultField {
            name: "indclass".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Int)),
            text_type_modifier: Some(TextTypeModifier::OidVector),
            nullable: false,
        },
        ResultField {
            name: "indoption".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Int)),
            text_type_modifier: Some(TextTypeModifier::Int2Vector),
            nullable: false,
        },
        nullable_text_field("indexprs"),
        nullable_text_field("indpred"),
    ]
}

pub(super) fn build_pg_index_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
) -> DbResult<LogicalPlan> {
    let output_fields = pg_index_fields();
    let tables = list_user_tables(catalog, txn_id, default_schema)?;
    let mut rows: Vec<Vec<TypedExpr>> = Vec::new();

    for table in &tables {
        let table_oid = relation_id_to_oid(table);
        let col_by_id: std::collections::HashMap<ColumnId, &ColumnDescriptor> =
            table.columns.iter().map(|c| (c.column_id, c)).collect();
        let indexes = catalog.list_indexes(txn_id, table.table_id)?;
        for idx in &indexes {
            let idx_oid = index_id_to_oid(idx);
            let indkey_values: Vec<Value> = idx
                .key_columns
                .iter()
                .filter_map(|kc| {
                    col_by_id
                        .get(&kc.column_id)
                        .map(|c| Value::Int(u32_to_i32_saturating(c.ordinal_position)))
                })
                .collect();
            let is_primary = is_primary_key_index(table, idx);
            let key_arity = idx.key_columns.len();
            let zero_vector_values: Vec<Value> =
                std::iter::repeat_n(Value::Int(0), key_arity).collect();
            rows.push(vec![
                int_literal(idx_oid),
                int_literal(table_oid),
                bool_literal(idx.unique),
                bool_literal(is_primary),
                TypedExpr::literal(
                    pg_vector_literal(0, indkey_values),
                    DataType::Array(Box::new(DataType::Int)),
                    false,
                ),
                bool_literal(false),
                bool_literal(true),
                bool_literal(false),
                bool_literal(true),
                bool_literal(false),
                bool_literal(true),
                bool_literal(true),
                bool_literal(false),
                bool_literal(false),
                int_literal(i32::try_from(idx.key_columns.len()).unwrap_or(i32::MAX)),
                int_literal(i32::try_from(idx.key_columns.len()).unwrap_or(i32::MAX)),
                TypedExpr::literal(
                    pg_vector_literal(0, zero_vector_values.clone()),
                    DataType::Array(Box::new(DataType::Int)),
                    false,
                ),
                TypedExpr::literal(
                    pg_vector_literal(0, zero_vector_values.clone()),
                    DataType::Array(Box::new(DataType::Int)),
                    false,
                ),
                TypedExpr::literal(
                    pg_vector_literal(0, zero_vector_values),
                    DataType::Array(Box::new(DataType::Int)),
                    false,
                ),
                null_literal(DataType::Text),
                null_literal(DataType::Text),
            ]);
        }
    }

    Ok(project_values(output_fields, rows))
}

// ---------------------------------------------------------------
// pg_catalog.pg_constraint
// ---------------------------------------------------------------

pub(super) fn pg_constraint_fields() -> Vec<ResultField> {
    vec![
        oid_field("oid"),
        name_field("conname"),
        oid_field("connamespace"),
        internal_char_field("contype"),
        oid_field("conrelid"),
        oid_field("contypid"),
        oid_field("conindid"),
        ResultField {
            name: "conkey".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Int)),
            text_type_modifier: None,
            nullable: true,
        },
        oid_field("confrelid"),
        ResultField {
            name: "confkey".to_owned(),
            data_type: DataType::Array(Box::new(DataType::Int)),
            text_type_modifier: None,
            nullable: true,
        },
        bool_field("condeferrable"),
        bool_field("condeferred"),
        bool_field("convalidated"),
        nullable_text_field("conbin"),
        nullable_text_field("consrc"),
        bool_field("conislocal"),
        int_field("coninhcount"),
        internal_char_field("confupdtype"),
        internal_char_field("confdeltype"),
        internal_char_field("confmatchtype"),
    ]
}

pub(super) fn build_pg_constraint_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
) -> DbResult<LogicalPlan> {
    let output_fields = pg_constraint_fields();
    let tables = list_user_tables(catalog, txn_id, default_schema)?;
    let schema_namespace_oids = catalog
        .list_schemas(txn_id)?
        .into_iter()
        .map(|schema| {
            let namespace_oid = if schema.schema_id.get() == 1 {
                PUBLIC_NAMESPACE_OID
            } else {
                u64_to_i32_saturating(schema.schema_id.get()).saturating_add(16384)
            };
            (schema.name.to_ascii_lowercase(), namespace_oid)
        })
        .collect::<std::collections::HashMap<_, _>>();
    let mut rows: Vec<Vec<TypedExpr>> = Vec::new();
    let mut constraint_oid: i32 = 100_000;

    for table in &tables {
        let table_oid = relation_id_to_oid(table);
        let ns_oid = schema_id_to_namespace_oid(table);
        let col_by_id: std::collections::HashMap<ColumnId, &ColumnDescriptor> =
            table.columns.iter().map(|c| (c.column_id, c)).collect();
        let indexes = catalog.list_indexes(txn_id, table.table_id)?;
        let primary_index_oid = indexes
            .iter()
            .find(|idx| is_primary_key_index(table, idx))
            .map(index_id_to_oid)
            .unwrap_or(0);

        // Primary key constraint
        if let Some(pk_cols) = &table.primary_key {
            let conname = format!("{}_pkey", table.name.object_name());
            let conkey: Vec<i32> = pk_cols
                .iter()
                .filter_map(|col_id| {
                    col_by_id
                        .get(col_id)
                        .map(|c| u32_to_i32_saturating(c.ordinal_position))
                })
                .collect();
            rows.push(make_constraint_row(
                constraint_oid,
                &conname,
                ns_oid,
                "p",
                table_oid,
                0,
                primary_index_oid,
                &conkey,
                0,
                &[],
                "",
                "",
                "",
            ));
            constraint_oid += 1;
        }

        // Unique constraints (non-PK unique indexes)
        for idx in &indexes {
            if idx.unique && !is_primary_key_index(table, idx) {
                let Some(conname) = idx.constraint_name.as_ref() else {
                    continue;
                };
                let conkey: Vec<i32> = idx
                    .key_columns
                    .iter()
                    .filter_map(|kc| {
                        col_by_id
                            .get(&kc.column_id)
                            .map(|c| u32_to_i32_saturating(c.ordinal_position))
                    })
                    .collect();
                rows.push(make_constraint_row(
                    constraint_oid,
                    conname,
                    ns_oid,
                    "u",
                    table_oid,
                    0,
                    index_id_to_oid(idx),
                    &conkey,
                    0,
                    &[],
                    "",
                    "",
                    "",
                ));
                constraint_oid += 1;
            }
        }

        // Foreign key constraints
        for fk in &table.foreign_keys {
            let conname = fk.effective_name(table.name.object_name());
            let conkey: Vec<i32> = fk
                .columns
                .iter()
                .filter_map(|col_name| {
                    table
                        .columns
                        .iter()
                        .find(|c| c.name.eq_ignore_ascii_case(col_name))
                        .map(|c| u32_to_i32_saturating(c.ordinal_position))
                })
                .collect();
            // Resolve referenced table OID and confkey
            let (confrelid, confkey) = tables
                .iter()
                .find(|t| {
                    let referenced = fk.referenced_table.to_ascii_lowercase();
                    t.name.object_name().eq_ignore_ascii_case(&referenced)
                        || t.name.name.eq_ignore_ascii_case(&referenced)
                        || t.name
                            .schema_name()
                            .map(|schema| format!("{schema}.{}", t.name.object_name()))
                            .is_some_and(|qualified| qualified.eq_ignore_ascii_case(&referenced))
                        || format!("public.{}", t.name.object_name())
                            .eq_ignore_ascii_case(&referenced)
                })
                .map_or((0, Vec::new()), |ref_table| {
                    let oid = relation_id_to_oid(ref_table);
                    let positions: Vec<i32> = fk
                        .referenced_columns
                        .iter()
                        .filter_map(|col_name| {
                            ref_table
                                .columns
                                .iter()
                                .find(|c| c.name.eq_ignore_ascii_case(col_name))
                                .map(|c| u32_to_i32_saturating(c.ordinal_position))
                        })
                        .collect();
                    (oid, positions)
                });
            rows.push(make_constraint_row(
                constraint_oid,
                &conname,
                ns_oid,
                "f",
                table_oid,
                0,
                0,
                &conkey,
                confrelid,
                &confkey,
                fk.on_update.as_pg_code(),
                fk.on_delete.as_pg_code(),
                "s",
            ));
            constraint_oid += 1;
        }

        // CHECK constraints
        for (i, check) in table.check_constraints.iter().enumerate() {
            let conname = check
                .name
                .clone()
                .unwrap_or_else(|| format!("{}_check{}", table.name.object_name(), i + 1));
            rows.push(make_constraint_row(
                constraint_oid,
                &conname,
                ns_oid,
                "c",
                table_oid,
                0,
                0,
                &[],
                0,
                &[],
                "",
                "",
                "",
            ));
            constraint_oid += 1;
        }
    }

    for domain in catalog.list_domains(txn_id)? {
        let ns_oid = domain
            .schema_name
            .as_deref()
            .and_then(|schema_name| schema_namespace_oids.get(&schema_name.to_ascii_lowercase()))
            .copied()
            .unwrap_or(PUBLIC_NAMESPACE_OID);
        let domain_oid = compat_domain_oid(domain.schema_name.as_deref(), &domain.name);
        for (index, check) in domain.constraints.iter().enumerate() {
            let conname = if check.name.is_empty() {
                format!("{}_check{}", domain.name, index + 1)
            } else {
                check.name.clone()
            };
            rows.push(make_constraint_row(
                constraint_oid,
                &conname,
                ns_oid,
                "c",
                0,
                domain_oid,
                0,
                &[],
                0,
                &[],
                "",
                "",
                "",
            ));
            constraint_oid += 1;
        }
    }

    Ok(project_values(output_fields, rows))
}

/// Build a single pg_constraint row with proper array-typed conkey/confkey.
fn make_constraint_row(
    oid: i32,
    conname: &str,
    ns_oid: i32,
    contype: &str,
    conrelid: i32,
    contypid: i32,
    conindid: i32,
    conkey: &[i32],
    confrelid: i32,
    confkey: &[i32],
    confupdtype: &str,
    confdeltype: &str,
    confmatchtype: &str,
) -> Vec<TypedExpr> {
    let array_type = DataType::Array(Box::new(DataType::Int));
    let conkey_expr = if conkey.is_empty() {
        TypedExpr::literal(Value::Null, array_type.clone(), true)
    } else {
        TypedExpr::literal(
            Value::Array(conkey.iter().map(|&v| Value::Int(v)).collect()),
            array_type.clone(),
            false,
        )
    };
    let confkey_expr = if confkey.is_empty() {
        TypedExpr::literal(Value::Null, array_type, true)
    } else {
        TypedExpr::literal(
            Value::Array(confkey.iter().map(|&v| Value::Int(v)).collect()),
            array_type,
            false,
        )
    };
    vec![
        int_literal(oid),
        text_literal(conname),
        int_literal(ns_oid),
        text_literal(contype),
        int_literal(conrelid),
        int_literal(contypid),
        int_literal(conindid),
        conkey_expr,
        int_literal(confrelid),
        confkey_expr,
        bool_literal(false),          // condeferrable
        bool_literal(false),          // condeferred
        bool_literal(true),           // convalidated
        null_literal(DataType::Text), // conbin
        null_literal(DataType::Text), // consrc
        bool_literal(true),           // conislocal
        int_literal(0),               // coninhcount
        text_literal(confupdtype),
        text_literal(confdeltype),
        text_literal(confmatchtype),
    ]
}
