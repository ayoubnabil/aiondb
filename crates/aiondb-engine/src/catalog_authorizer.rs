#![allow(clippy::pedantic)]
#![allow(
    clippy::doc_markdown,
    clippy::match_same_arms,
    clippy::needless_pass_by_value,
    clippy::semicolon_if_nothing_returned,
    clippy::too_many_lines,
    clippy::unnecessary_wraps
)]

use std::collections::{BTreeMap, BTreeSet};

use aiondb_catalog::{
    CatalogPrivilege, CatalogReader, FunctionPrivilegeTarget, PrivilegeTarget, QualifiedName,
};
use aiondb_core::{DataType, DbError, DbResult, RelationId, SqlState, TxnId};
use aiondb_plan::{
    dml::{InsertOnConflict, MergeActionPlan, MergePlan, OnConflictActionPlan, UpdateAssignment},
    LogicalPlan, PhysicalPlan, ProjectionExpr, ScalarFunction, SortExpr, TypedExpr, TypedExprKind,
};
use aiondb_security::AuthenticatedIdentity;

/// Extracts the (Action, `RelationId`) pairs that a physical plan requires.
/// Returns a list of `(CatalogPrivilege, RelationId)` tuples.
pub(crate) fn required_privileges(plan: &PhysicalPlan) -> Vec<(CatalogPrivilege, RelationId)> {
    match plan {
        PhysicalPlan::ProjectTable { table_id, .. }
        | PhysicalPlan::Aggregate { table_id, .. }
        | PhysicalPlan::DistributedScan { table_id, .. }
        | PhysicalPlan::HnswScan { table_id, .. } => {
            vec![(CatalogPrivilege::Select, *table_id)]
        }
        PhysicalPlan::InsertValues {
            table_id,
            returning,
            on_conflict,
            ..
        } => {
            let mut reqs = vec![(CatalogPrivilege::Insert, *table_id)];
            if !returning.is_empty() {
                reqs.push((CatalogPrivilege::Select, *table_id));
            }
            if let Some(InsertOnConflict {
                action: OnConflictActionPlan::DoUpdate { .. },
                ..
            }) = on_conflict
            {
                reqs.push((CatalogPrivilege::Update, *table_id));
                reqs.push((CatalogPrivilege::Select, *table_id));
            }
            reqs
        }
        PhysicalPlan::InsertSelect {
            table_id,
            source,
            returning,
            on_conflict,
            ..
        } => {
            let mut reqs = vec![(CatalogPrivilege::Insert, *table_id)];
            reqs.extend(required_privileges(source));
            if !returning.is_empty() {
                reqs.push((CatalogPrivilege::Select, *table_id));
            }
            if let Some(InsertOnConflict {
                action: OnConflictActionPlan::DoUpdate { .. },
                ..
            }) = on_conflict
            {
                reqs.push((CatalogPrivilege::Update, *table_id));
                reqs.push((CatalogPrivilege::Select, *table_id));
            }
            reqs
        }
        PhysicalPlan::DeleteFromTable {
            table_id,
            using_table_ids,
            returning,
            filter,
            ..
        } => {
            let mut reqs = vec![(CatalogPrivilege::Delete, *table_id)];
            for using_table_id in using_table_ids {
                if *using_table_id != *table_id {
                    reqs.push((CatalogPrivilege::Select, *using_table_id));
                }
            }
            // PG: a non-trivial WHERE filter or any RETURNING clause that
            // reads target-table columns requires SELECT on those columns.
            // Without column-level ACLs, conservatively require table-level
            // SELECT whenever such a read happens.
            if !returning.is_empty() || filter.is_some() {
                reqs.push((CatalogPrivilege::Select, *table_id));
            }
            reqs
        }
        PhysicalPlan::TruncateTable { table_id, .. } => {
            vec![(CatalogPrivilege::Delete, *table_id)]
        }
        PhysicalPlan::UpdateTable {
            table_id,
            from_table_ids,
            returning,
            filter,
            ..
        } => {
            let mut reqs = vec![(CatalogPrivilege::Update, *table_id)];
            for from_table_id in from_table_ids {
                if *from_table_id != *table_id {
                    reqs.push((CatalogPrivilege::Select, *from_table_id));
                }
            }
            if !returning.is_empty() || filter.is_some() {
                reqs.push((CatalogPrivilege::Select, *table_id));
            }
            reqs
        }
        PhysicalPlan::NestedLoopJoin { left, right, .. }
        | PhysicalPlan::HashJoin { left, right, .. } => {
            let mut reqs = required_privileges(left);
            reqs.extend(required_privileges(right));
            reqs
        }
        PhysicalPlan::BroadcastHashJoin {
            broadcast, local, ..
        } => {
            let mut reqs = required_privileges(broadcast);
            reqs.extend(required_privileges(local));
            reqs
        }
        PhysicalPlan::PartialAggregate { source, .. } => required_privileges(source),
        PhysicalPlan::FinalAggregate { partials, .. } => {
            let mut reqs = Vec::new();
            for partial in partials {
                reqs.extend(required_privileges(partial));
            }
            reqs
        }
        PhysicalPlan::SeqScan { table_id } => {
            vec![(CatalogPrivilege::Select, *table_id)]
        }
        PhysicalPlan::SetOperation { left, right, .. } => {
            let mut reqs = required_privileges(left);
            reqs.extend(required_privileges(right));
            reqs
        }
        PhysicalPlan::ProjectSource { source, .. }
        | PhysicalPlan::AggregateSource { source, .. } => required_privileges(source),
        PhysicalPlan::CopyFrom { table_id, .. } => {
            vec![(CatalogPrivilege::Insert, *table_id)]
        }
        PhysicalPlan::CopyTo { table_id, .. } => {
            vec![(CatalogPrivilege::Select, *table_id)]
        }
        PhysicalPlan::CreateTableAs { source, .. } => required_privileges(source),
        PhysicalPlan::RecursiveCte {
            base, recursive, ..
        } => {
            let mut reqs = required_privileges(base);
            reqs.extend(required_privileges(recursive));
            reqs
        }
        PhysicalPlan::MergeTable(merge) => {
            let mut reqs = vec![
                (CatalogPrivilege::Select, merge.source_table_id),
                (CatalogPrivilege::Select, merge.target_table_id),
            ];
            if let Some(source_subquery_plan) = merge.source_subquery_plan.as_deref() {
                reqs.extend(required_privileges(source_subquery_plan));
            }
            for clause in &merge.when_clauses {
                match &clause.action {
                    aiondb_plan::MergeActionPlan::Update { .. } => {
                        reqs.push((CatalogPrivilege::Update, merge.target_table_id));
                    }
                    aiondb_plan::MergeActionPlan::Delete => {
                        reqs.push((CatalogPrivilege::Delete, merge.target_table_id));
                    }
                    aiondb_plan::MergeActionPlan::Insert { .. } => {
                        reqs.push((CatalogPrivilege::Insert, merge.target_table_id));
                    }
                    aiondb_plan::MergeActionPlan::InsertDefaultValues => {
                        reqs.push((CatalogPrivilege::Insert, merge.target_table_id));
                    }
                    aiondb_plan::MergeActionPlan::DoNothing => {}
                }
            }
            reqs
        }
        // `LOCK TABLE` privilege requirements follow PostgreSQL-style behavior:
        // - ACCESS SHARE: requires any table privilege
        // - ROW EXCLUSIVE: requires INSERT/UPDATE/DELETE/TRUNCATE
        // - ACCESS EXCLUSIVE: requires UPDATE/DELETE/TRUNCATE
        // We enforce this in `enforce_plan_acl` to preserve OR semantics.
        PhysicalPlan::Lock { .. } => Vec::new(),
        // DDL, GRANT/REVOKE, role management, etc. -- not checked here.
        _ => Vec::new(),
    }
}

/// Check whether the given identity has the required privileges on a table.
///
/// The check enforces a **conditional deny** policy:
/// - If the catalog has *no roles at all*, the role system is not in use and
///   access is allowed (backward compatibility).
/// - If the catalog *has* roles but none match the identity, access is denied.
/// - When a matching role is found, superuser status bypasses all further checks.
pub(crate) fn check_privilege(
    catalog_reader: &dyn CatalogReader,
    identity: &AuthenticatedIdentity,
    privilege: CatalogPrivilege,
    table_id: RelationId,
) -> DbResult<()> {
    let txn = TxnId::default(); // read committed state

    // First pass: determine if the role system applies to this identity.
    let mut any_role_exists = false;
    for role_name in &identity.roles {
        if let Some(role_desc) = catalog_reader.get_role(txn, role_name)? {
            any_role_exists = true;
            if role_desc.superuser {
                return Ok(());
            }
        }
    }

    if !any_role_exists {
        // Check whether the role system is in use at all.  If the catalog
        // contains no roles, the RBAC subsystem is inactive → allow access
        // for backward compatibility.  If roles DO exist but none match
        // this identity, deny access (possible misconfiguration or
        // unauthorized user). `catalog_has_any_roles` filters out
        // predefined PG roles so a fresh-engine bootstrap doesn't trip
        // the deny path.
        if catalog_has_any_roles(catalog_reader)? {
            return Err(DbError::insufficient_privilege(
                "no valid roles found for user",
            ));
        }
        return Ok(());
    }

    // Resolve table name from catalog.
    let table_desc = catalog_reader.get_table_by_id(txn, table_id)?;
    let table_name = match &table_desc {
        Some(desc) => &desc.name,
        None => {
            // Table not found -- let the executor handle this later.
            return Ok(());
        }
    };

    // PG: any access to schema-qualified relations requires USAGE on the
    // schema, *before* table-level ACLs are consulted. Built-in schemas
    // (public, pg_catalog, information_schema, pg_temp, pg_toast) carry an
    // implicit PUBLIC USAGE grant; user schemas require an explicit
    // GRANT USAGE. Without this gate a `GRANT SELECT ON sch.t TO r` alone
    // bypasses schema-level isolation.
    if let Some(schema_name) = table_name.schema.as_deref() {
        if !is_builtin_schema(schema_name)
            && !has_schema_privilege(
                catalog_reader,
                identity,
                schema_name,
                CatalogPrivilege::Usage,
            )?
        {
            return Err(DbError::insufficient_privilege(format!(
                "permission denied for schema {schema_name}"
            )));
        }
    }

    // Owner of the table has all privileges implicitly (PostgreSQL behavior).
    if let Some(owner) = table_desc.as_ref().and_then(|d| d.owner.as_deref()) {
        if identity_matches_owner(identity, owner) {
            return Ok(());
        }
    }

    // Check privileges for direct and inherited roles plus PUBLIC grants.
    let effective_roles = collect_effective_role_names(catalog_reader, identity)?;
    for role_name in &effective_roles {
        // Predefined PostgreSQL roles `pg_read_all_data` / `pg_write_all_data`
        // bypass per-relation ACLs for SELECT and INSERT/UPDATE/DELETE
        // respectively. PG seeds these roles empty and recognises membership
        // implicitly during privilege checks.
        if role_name.eq_ignore_ascii_case("pg_read_all_data")
            && matches!(privilege, CatalogPrivilege::Select)
        {
            return Ok(());
        }
        if role_name.eq_ignore_ascii_case("pg_write_all_data")
            && matches!(
                privilege,
                CatalogPrivilege::Insert | CatalogPrivilege::Update | CatalogPrivilege::Delete
            )
        {
            return Ok(());
        }
        let privileges = catalog_reader.get_privileges(txn, role_name)?;
        for priv_desc in &privileges {
            if matches_privilege(&priv_desc.privilege, privilege)
                && matches_target(&priv_desc.target, table_name)
            {
                return Ok(());
            }
        }
    }

    Err(DbError::insufficient_privilege(format!(
        "permission denied for table {}",
        table_name.name
    )))
}

fn collect_effective_role_names(
    _catalog_reader: &dyn CatalogReader,
    identity: &AuthenticatedIdentity,
) -> DbResult<Vec<String>> {
    // The engine intentionally implements PostgreSQL `NOINHERIT` semantics
    // by default: a role's privileges become effective only after `SET ROLE`,
    // not via mere membership. Audit engine_authorizer F1 proposed walking
    // `PrivilegeTarget::Role` grants here to mirror the `INHERIT` default,
    // but doing so flips the engine-wide policy and breaks the existing
    // `set_role_applies_acl_as_effective_role` contract. Defer until
    // `INHERIT` / `NOINHERIT` is plumbed through `CREATE ROLE`.
    let mut effective = identity.roles.clone();
    if !effective
        .iter()
        .any(|role| role.eq_ignore_ascii_case("public"))
    {
        effective.push("public".to_owned());
    }

    Ok(effective)
}

/// Check if a granted privilege covers the required privilege.
fn matches_privilege(granted: &CatalogPrivilege, required: CatalogPrivilege) -> bool {
    *granted == CatalogPrivilege::All || *granted == required
}

/// Check if the privilege target matches the table.
///
/// Schema matching: when both the granted target and the actual table have a
/// schema qualifier, they must match (case-insensitive).  When neither has a
/// schema, the names match.  When the grant target is unqualified (no schema),
/// it matches any table with the same name -- this mirrors `PostgreSQL`'s behavior
/// where an unqualified name in `GRANT` resolves via the `search_path` at grant time
/// and the catalog stores tables with a schema qualifier.
///
/// The reverse direction (qualified grant, unqualified runtime descriptor)
/// must NOT match: a grant scoped to `secret.t` must never authorize
/// access to a `t` resolved in another schema. Allowing that would let a
/// privilege bound to one schema spread to other schemas via descriptors
/// whose schema field happens to be `None` (which several internal
/// catalog-write paths do produce). See SECURITY_FINDINGS_0DAY_A1.md F1.
fn matches_target(target: &PrivilegeTarget, table_name: &QualifiedName) -> bool {
    match target {
        PrivilegeTarget::Table(name) => {
            name.name.eq_ignore_ascii_case(&table_name.name)
                && match (&name.schema, &table_name.schema) {
                    (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
                    // PG: unqualified grant resolves via search_path at
                    // GRANT time and matches the qualified runtime table.
                    (None, _) => true,
                    // A grant scoped to a specific schema must NOT match a
                    // runtime descriptor that lacks a schema; otherwise a
                    // grant on `secret.t` would silently authorize any
                    // schema's `t` whose descriptor was created without
                    // an explicit schema qualifier.
                    (Some(_), None) => false,
                }
        }
        PrivilegeTarget::Role(_) => false,
        _ => false,
    }
}

/// Check whether the identity is a superuser in the catalog.
/// Returns `false` if none of the identity's roles exist (role system not in use).
pub(crate) fn is_superuser_checked(
    catalog_reader: &dyn CatalogReader,
    identity: &AuthenticatedIdentity,
) -> DbResult<bool> {
    let txn = TxnId::default();
    for role_name in &identity.roles {
        if let Some(role_desc) = catalog_reader.get_role(txn, role_name)? {
            if role_desc.superuser {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Backward-compatible helper used outside strict ACL paths.
/// Returns `false` on catalog read errors.
pub(crate) fn is_superuser(
    catalog_reader: &dyn CatalogReader,
    identity: &AuthenticatedIdentity,
) -> bool {
    is_superuser_checked(catalog_reader, identity).unwrap_or(false)
}

/// Check whether the role system is in use for this identity (at least one role exists).
pub(crate) fn role_system_active(
    catalog_reader: &dyn CatalogReader,
    identity: &AuthenticatedIdentity,
) -> DbResult<bool> {
    let txn = TxnId::default();
    for role_name in &identity.roles {
        if catalog_reader.get_role(txn, role_name)?.is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Process-wide forcing of RBAC-active mode. Set at engine startup when the
/// security profile is non-Development so a fresh-bootstrap-without-roles
/// catalog cannot fail-open into "anyone can DDL" mode (audit
/// engine_functions / engine_backup F1 root cause).
static FORCE_RBAC_ACTIVE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Force the RBAC-active gate on regardless of catalog state. Should be called
/// once at engine boot when the security profile is not `Development`.
pub fn force_rbac_active() {
    let _ = FORCE_RBAC_ACTIVE.set(true);
}

/// Check whether the catalog contains any roles at all (globally, not
/// identity-scoped).  When the catalog has roles the RBAC subsystem is
/// considered active even if the current session identity does not map to
/// any of them.
pub(crate) fn catalog_has_any_roles(catalog_reader: &dyn CatalogReader) -> DbResult<bool> {
    if FORCE_RBAC_ACTIVE.get().copied().unwrap_or(false) {
        return Ok(true);
    }
    let txn = TxnId::default();
    // Predefined PG roles (`pg_monitor`, `pg_read_all_data`, …) are
    // bootstrapped on every engine startup and have `login=false`,
    // `superuser=false`. They never represent a real user, so they
    // should not flip the role system into "active" mode just by
    // existing - that would force every fresh-engine DDL through the
    // superuser gate.
    Ok(catalog_reader
        .list_roles(txn)?
        .iter()
        .any(|role| role.login || role.superuser))
}

/// Require superuser for role-management and privilege-management operations.
/// When the role system is not active (no roles in catalog), these operations
/// are allowed unconditionally for backward compatibility.
///
/// The check is **catalog-scoped**: if the catalog contains *any* roles then
/// the RBAC subsystem is considered active, even when the current identity
/// does not map to a known role.  This prevents an identity without a
/// matching role from bypassing superuser enforcement for DDL and GRANT.
fn enforce_role_management_acl(
    catalog_reader: &dyn CatalogReader,
    identity: &AuthenticatedIdentity,
    plan: &PhysicalPlan,
) -> DbResult<()> {
    match plan {
        PhysicalPlan::CreateRole { .. } => {
            if catalog_has_any_roles(catalog_reader)?
                && !is_superuser_checked(catalog_reader, identity)?
            {
                return Err(DbError::insufficient_privilege(
                    "must be superuser to create roles",
                ));
            }
        }
        PhysicalPlan::AlterRole {
            superuser,
            name,
            login,
            createdb,
            createrole,
            replication,
            bypassrls,
            ..
        } => {
            if catalog_has_any_roles(catalog_reader)?
                && !is_superuser_checked(catalog_reader, identity)?
            {
                // Non-superusers may only alter their own non-privilege-escalating attributes.
                let is_own_role = identity.roles.iter().any(|r| r == name);
                if *superuser || !is_own_role {
                    return Err(DbError::insufficient_privilege(
                        "must be superuser to alter another role or grant SUPERUSER",
                    ));
                }
                // SECURITY: prevent non-superusers from granting themselves
                // privilege-escalating attributes by altering their own
                // role. CREATEDB / CREATEROLE / REPLICATION / BYPASSRLS
                // each unlock a distinct escalation path (DDL on system
                // schemas, role/grant management, physical replication
                // streaming, RLS bypass). PG itself only allows these
                // changes from a role with CREATEROLE on a non-superuser
                // target, or from a superuser. The earlier guard only
                // covered SUPERUSER, leaving the other four attributes as
                // a self-elevation escape hatch. See
                // SECURITY_FINDINGS_0DAY_A1.md F8.
                if *createdb || *createrole || *replication || *bypassrls {
                    let txn = TxnId::default();
                    if let Some(current_role) = catalog_reader.get_role(txn, name)? {
                        if (*createdb && !current_role.createdb)
                            || (*createrole && !current_role.createrole)
                            || (*replication && !current_role.replication)
                            || (*bypassrls && !current_role.bypassrls)
                        {
                            return Err(DbError::insufficient_privilege(
                                "must be superuser to grant CREATEDB / CREATEROLE / REPLICATION / BYPASSRLS to a role",
                            ));
                        }
                    }
                }
                // Prevent non-superusers from re-enabling LOGIN on their own role.
                if *login {
                    let txn = TxnId::default();
                    if let Some(current_role) = catalog_reader.get_role(txn, name)? {
                        if !current_role.login {
                            return Err(DbError::insufficient_privilege(
                                "must be superuser to re-enable LOGIN on a role",
                            ));
                        }
                    }
                }
            }
        }
        PhysicalPlan::DropRole { .. } => {
            if catalog_has_any_roles(catalog_reader)?
                && !is_superuser_checked(catalog_reader, identity)?
            {
                return Err(DbError::insufficient_privilege(
                    "must be superuser to drop roles",
                ));
            }
        }
        PhysicalPlan::Grant { target, .. } | PhysicalPlan::Revoke { target, .. } => {
            if catalog_has_any_roles(catalog_reader)?
                && !is_superuser_checked(catalog_reader, identity)?
            {
                match target {
                    PrivilegeTarget::Table(table_name) => {
                        // Preserve undefined-table behavior from statement
                        // execution: if the relation is missing, defer instead
                        // of masking it with an ACL error.
                        if let Some(table) =
                            catalog_reader.get_table(TxnId::default(), table_name)?
                        {
                            if !table_owner_matches_identity(&table, identity) {
                                return Err(DbError::insufficient_privilege(
                                    "must be owner of table or superuser to grant/revoke privileges",
                                ));
                            }
                        }
                    }
                    PrivilegeTarget::Function(function_target) => {
                        if !function_target_owner_matches_identity(
                            catalog_reader,
                            identity,
                            function_target,
                        )? {
                            return Err(DbError::insufficient_privilege(
                                "must be owner of function or superuser to grant/revoke privileges",
                            ));
                        }
                    }
                    _ => {
                        return Err(DbError::insufficient_privilege(
                            "must be superuser to grant/revoke non-table privileges",
                        ));
                    }
                }
            }
        }
        PhysicalPlan::CreateTable { .. }
        | PhysicalPlan::CreateTableAs { .. }
        | PhysicalPlan::CreateSequence { .. }
        | PhysicalPlan::CreateView { .. } => {
            if catalog_has_any_roles(catalog_reader)?
                && !is_superuser_checked(catalog_reader, identity)?
                && !role_system_active(catalog_reader, identity)?
            {
                return Err(DbError::insufficient_privilege(
                    "must map to an existing role to create objects when role management is active",
                ));
            }
        }
        PhysicalPlan::DropTable { table_id, .. }
        | PhysicalPlan::AlterTableAddColumn { table_id, .. }
        | PhysicalPlan::AlterTableDropColumn { table_id, .. }
        | PhysicalPlan::AlterTableRename { table_id, .. }
        | PhysicalPlan::AlterTableRenameColumn { table_id, .. }
        | PhysicalPlan::AlterTableSetDefault { table_id, .. }
        | PhysicalPlan::AlterTableDropDefault { table_id, .. }
        | PhysicalPlan::AlterTableSetNotNull { table_id, .. }
        | PhysicalPlan::AlterTableDropNotNull { table_id, .. }
        | PhysicalPlan::AlterTableAddConstraint { table_id, .. }
        | PhysicalPlan::AlterTableDropConstraint { table_id, .. }
        | PhysicalPlan::AlterTableAlterColumnType { table_id, .. }
        | PhysicalPlan::TruncateTable { table_id } => {
            if catalog_has_any_roles(catalog_reader)?
                && !is_superuser_checked(catalog_reader, identity)?
                && !owns_table_id(catalog_reader, identity, *table_id)?
            {
                return Err(DbError::insufficient_privilege("must be owner of table"));
            }
        }
        PhysicalPlan::CreateIndex { table_id, .. } => {
            if catalog_has_any_roles(catalog_reader)?
                && !is_superuser_checked(catalog_reader, identity)?
                && !owns_table_id(catalog_reader, identity, *table_id)?
            {
                return Err(DbError::insufficient_privilege("must be owner of table"));
            }
        }
        PhysicalPlan::CreateSchema { .. } => {
            if catalog_has_any_roles(catalog_reader)?
                && !is_superuser_checked(catalog_reader, identity)?
                && !role_system_active(catalog_reader, identity)?
            {
                return Err(DbError::insufficient_privilege(
                    "must map to an existing role to create schemas when role management is active",
                ));
            }
        }
        PhysicalPlan::DropSchema { .. }
        | PhysicalPlan::DropIndex { .. }
        | PhysicalPlan::DropView { .. }
        | PhysicalPlan::DropSequence { .. }
        | PhysicalPlan::CreateNodeLabel { .. }
        | PhysicalPlan::CreateEdgeLabel { .. }
        | PhysicalPlan::DropNodeLabel { .. }
        | PhysicalPlan::DropEdgeLabel { .. }
        | PhysicalPlan::Analyze { .. }
        | PhysicalPlan::Vacuum { .. } => {
            if catalog_has_any_roles(catalog_reader)?
                && !is_superuser_checked(catalog_reader, identity)?
            {
                return Err(DbError::insufficient_privilege(
                    "must be superuser to perform DDL operations",
                ));
            }
        }
        _ => {}
    }
    Ok(())
}

fn owns_table_id(
    catalog_reader: &dyn CatalogReader,
    identity: &AuthenticatedIdentity,
    table_id: RelationId,
) -> DbResult<bool> {
    Ok(catalog_reader
        .get_table_by_id(TxnId::default(), table_id)?
        .as_ref()
        .is_some_and(|table| table_owner_matches_identity(table, identity)))
}

/// True if any role on `identity` matches `owner` (case-insensitive).
///
/// This is the canonical "is the caller the object owner?" check, used by
/// table/function/trigger ACL gating. Centralised so the comparison rule
/// (ASCII case-insensitive) stays consistent across the engine.
pub(crate) fn identity_matches_owner(identity: &AuthenticatedIdentity, owner: &str) -> bool {
    identity
        .roles
        .iter()
        .any(|role| role.eq_ignore_ascii_case(owner))
}

fn table_owner_matches_identity(
    table: &aiondb_catalog::TableDescriptor,
    identity: &AuthenticatedIdentity,
) -> bool {
    table
        .owner
        .as_deref()
        .is_some_and(|owner| identity_matches_owner(identity, owner))
}

fn function_target_owner_matches_identity(
    catalog_reader: &dyn CatalogReader,
    identity: &AuthenticatedIdentity,
    target: &FunctionPrivilegeTarget,
) -> DbResult<bool> {
    let functions = catalog_reader.list_functions(TxnId::default())?;
    if let Some(owned) = function_target_owner_check_for_candidate(&functions, identity, target) {
        return Ok(owned);
    }

    if target.name.schema.is_some() {
        // GRANT/REVOKE function targets are currently schema-qualified using
        // the session default schema. If this schema-specific lookup finds no
        // function, retry without schema to avoid bypassing owner checks when
        // the routine lives on another visible schema.
        let mut relaxed_target = target.clone();
        relaxed_target.name.schema = None;
        if let Some(owned) =
            function_target_owner_check_for_candidate(&functions, identity, &relaxed_target)
        {
            return Ok(owned);
        }
    }

    // Preserve undefined-object behavior for unknown targets: if no function
    // matched this ACL target, defer to later execution/binding stages.
    Ok(true)
}

fn function_target_owner_check_for_candidate(
    functions: &[aiondb_catalog::FunctionDescriptor],
    identity: &AuthenticatedIdentity,
    target: &FunctionPrivilegeTarget,
) -> Option<bool> {
    let mut matched = false;
    for function in functions {
        let descriptor_target = FunctionPrivilegeTarget {
            name: QualifiedName::parse(&function.name),
            arg_types: Some(
                function
                    .params
                    .iter()
                    .map(|param| param.data_type.clone())
                    .collect(),
            ),
        };
        if !function_acl_target_matches(target, &descriptor_target) {
            continue;
        }
        matched = true;
        let Some(owner) = function.owner.as_deref() else {
            return Some(false);
        };
        if !identity_matches_owner(identity, owner) {
            return Some(false);
        }
    }
    if matched {
        Some(true)
    } else {
        None
    }
}

fn is_builtin_schema(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "public" | "pg_catalog" | "information_schema" | "pg_toast"
    ) || name.to_ascii_lowercase().starts_with("pg_temp")
        || name.to_ascii_lowercase().starts_with("pg_toast_temp")
}

fn has_schema_privilege(
    catalog_reader: &dyn CatalogReader,
    identity: &AuthenticatedIdentity,
    schema_name: &str,
    required: CatalogPrivilege,
) -> DbResult<bool> {
    let txn = TxnId::default();
    for role_name in collect_effective_role_names(catalog_reader, identity)? {
        for privilege in catalog_reader.get_privileges(txn, &role_name)? {
            if !matches_privilege(&privilege.privilege, required) {
                continue;
            }
            if let PrivilegeTarget::Schema(granted_schema) = &privilege.target {
                if granted_schema.eq_ignore_ascii_case(schema_name) {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

fn function_acl_target_matches(
    granted: &FunctionPrivilegeTarget,
    requested: &FunctionPrivilegeTarget,
) -> bool {
    granted.name.name.eq_ignore_ascii_case(&requested.name.name)
        && match (&granted.name.schema, &requested.name.schema) {
            (Some(left), Some(right)) => left.eq_ignore_ascii_case(right),
            (None, Some(_)) => true,
            (Some(_), None) => false,
            (None, None) => true,
        }
        && match (&granted.arg_types, &requested.arg_types) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(left), Some(right)) => left == right,
        }
}

/// Enforce ACL for a physical plan.  Iterates over the required privileges
/// and checks each one against the catalog.  Also enforces superuser
/// requirements for role management operations.
pub(crate) fn enforce_plan_acl(
    catalog_reader: &dyn CatalogReader,
    identity: &AuthenticatedIdentity,
    plan: &PhysicalPlan,
) -> DbResult<()> {
    if is_superuser_checked(catalog_reader, identity)? {
        return Ok(());
    }
    enforce_role_management_acl(catalog_reader, identity, plan)?;
    enforce_cypher_schema_mutation_acl(catalog_reader, identity, plan)?;
    let mut reqs = required_privileges(plan);
    reqs.extend(required_cypher_privileges(catalog_reader, plan)?);
    // PG: every table referenced from a subquery (EXISTS/IN/scalar/array)
    // requires SELECT. `required_privileges` only looks at the top-level
    // physical plan; without the subquery walk a non-priv user could use
    // a denied table as an oracle inside a WHERE/HAVING clause.
    let mut subquery_table_ids = std::collections::BTreeSet::new();
    collect_subquery_table_ids(plan, &mut subquery_table_ids);
    for table_id in subquery_table_ids {
        reqs.push((CatalogPrivilege::Select, table_id));
    }
    for (privilege, table_id) in reqs {
        check_privilege(catalog_reader, identity, privilege, table_id)?;
    }
    for (privilege, table_id) in required_hybrid_scan_privileges(catalog_reader, plan)? {
        check_privilege(catalog_reader, identity, privilege, table_id)?;
    }
    for function_target in required_function_execs(plan) {
        check_execute_privilege(catalog_reader, identity, &function_target)?;
    }
    if let PhysicalPlan::Lock {
        table_ids, mode, ..
    } = plan
    {
        for table_id in table_ids {
            enforce_lock_table_privilege(catalog_reader, identity, *table_id, *mode)?;
        }
    }
    Ok(())
}

fn has_table_privilege(
    catalog_reader: &dyn CatalogReader,
    identity: &AuthenticatedIdentity,
    required: CatalogPrivilege,
    table_id: RelationId,
) -> DbResult<bool> {
    check_privilege(catalog_reader, identity, required, table_id)
        .map(|()| true)
        .or_else(|err| {
            if err.sqlstate() == SqlState::InsufficientPrivilege {
                Ok(false)
            } else {
                Err(err)
            }
        })
}

fn enforce_lock_table_privilege(
    catalog_reader: &dyn CatalogReader,
    identity: &AuthenticatedIdentity,
    table_id: RelationId,
    mode: aiondb_plan::PgLockMode,
) -> DbResult<()> {
    let required_any: &[CatalogPrivilege] = match mode {
        aiondb_plan::PgLockMode::AccessShare => &[
            CatalogPrivilege::Select,
            CatalogPrivilege::Insert,
            CatalogPrivilege::Update,
            CatalogPrivilege::Delete,
            CatalogPrivilege::Truncate,
        ],
        aiondb_plan::PgLockMode::RowExclusive => &[
            CatalogPrivilege::Insert,
            CatalogPrivilege::Update,
            CatalogPrivilege::Delete,
            CatalogPrivilege::Truncate,
        ],
        aiondb_plan::PgLockMode::AccessExclusive => &[
            CatalogPrivilege::Update,
            CatalogPrivilege::Delete,
            CatalogPrivilege::Truncate,
        ],
        _ => &[
            CatalogPrivilege::Select,
            CatalogPrivilege::Insert,
            CatalogPrivilege::Update,
            CatalogPrivilege::Delete,
            CatalogPrivilege::Truncate,
        ],
    };

    for required in required_any {
        if has_table_privilege(catalog_reader, identity, *required, table_id)? {
            return Ok(());
        }
    }

    check_privilege(catalog_reader, identity, required_any[0], table_id)
}

include!("catalog_authorizer_cypher_hybrid.rs");

include!("catalog_authorizer_functions.rs");

/// Walk a physical plan and collect every RelationId that appears inside a
/// subquery TypedExpr (EXISTS / IN / scalar / array). The caller treats
/// each as requiring SELECT - matches PG semantics where the subquery
/// runs with the caller's privileges and must therefore see every base
/// table named there.
fn collect_subquery_table_ids(
    plan: &PhysicalPlan,
    out: &mut std::collections::BTreeSet<RelationId>,
) {
    walk_physical_plan_exprs(plan, &mut |expr| {
        collect_table_ids_from_typed_expr(expr, out)
    });
}

fn walk_physical_plan_exprs<F: FnMut(&TypedExpr)>(plan: &PhysicalPlan, visit: &mut F) {
    let mut stack = vec![plan];
    while let Some(plan) = stack.pop() {
        match plan {
            PhysicalPlan::ProjectTable {
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
            } => {
                for proj in outputs {
                    visit(&proj.expr);
                }
                if let Some(f) = filter {
                    visit(f);
                }
                for s in order_by {
                    visit(&s.expr);
                }
                if let Some(l) = limit {
                    visit(l);
                }
                if let Some(o) = offset {
                    visit(o);
                }
                for d in distinct_on {
                    visit(d);
                }
                if let PhysicalPlan::ProjectSource { source, .. } = plan {
                    stack.push(source);
                }
            }
            PhysicalPlan::DistributedScan {
                outputs, filter, ..
            } => {
                for proj in outputs {
                    visit(&proj.expr);
                }
                if let Some(f) = filter {
                    visit(f);
                }
            }
            PhysicalPlan::Aggregate {
                group_by,
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
                aggregates,
                having,
                filter,
                order_by,
                limit,
                offset,
                distinct_on,
                ..
            } => {
                for e in group_by {
                    visit(e);
                }
                for a in aggregates {
                    visit(&a.expr);
                }
                if let Some(h) = having {
                    visit(h);
                }
                if let Some(f) = filter {
                    visit(f);
                }
                for s in order_by {
                    visit(&s.expr);
                }
                if let Some(l) = limit {
                    visit(l);
                }
                if let Some(o) = offset {
                    visit(o);
                }
                for d in distinct_on {
                    visit(d);
                }
                if let PhysicalPlan::AggregateSource { source, .. } = plan {
                    stack.push(source);
                }
            }
            PhysicalPlan::PartialAggregate {
                source, group_by, ..
            } => {
                stack.push(source);
                for e in group_by {
                    visit(e);
                }
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
                    stack.push(partial);
                }
                for e in group_by {
                    visit(e);
                }
                if let Some(h) = having {
                    visit(h);
                }
                for s in order_by {
                    visit(&s.expr);
                }
                if let Some(l) = limit {
                    visit(l);
                }
                if let Some(o) = offset {
                    visit(o);
                }
            }
            PhysicalPlan::NestedLoopJoin {
                left,
                right,
                condition,
                outputs,
                filter,
                order_by,
                limit,
                offset,
                distinct_on,
                ..
            }
            | PhysicalPlan::HashJoin {
                left,
                right,
                condition,
                outputs,
                filter,
                order_by,
                limit,
                offset,
                distinct_on,
                ..
            } => {
                stack.push(right);
                stack.push(left);
                if let Some(c) = condition {
                    visit(c);
                }
                for proj in outputs {
                    visit(&proj.expr);
                }
                if let Some(f) = filter {
                    visit(f);
                }
                for s in order_by {
                    visit(&s.expr);
                }
                if let Some(l) = limit {
                    visit(l);
                }
                if let Some(o) = offset {
                    visit(o);
                }
                for d in distinct_on {
                    visit(d);
                }
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
                stack.push(local);
                stack.push(broadcast);
                for key in left_keys {
                    visit(key);
                }
                for key in right_keys {
                    visit(key);
                }
                if let Some(c) = condition {
                    visit(c);
                }
                for proj in outputs {
                    visit(&proj.expr);
                }
            }
            PhysicalPlan::SetOperation { left, right, .. } => {
                stack.push(right);
                stack.push(left);
            }
            PhysicalPlan::DeleteFromTable {
                filter, returning, ..
            } => {
                if let Some(f) = filter {
                    visit(f);
                }
                for proj in returning {
                    visit(&proj.expr);
                }
            }
            PhysicalPlan::UpdateTable {
                filter,
                assignments,
                returning,
                ..
            } => {
                if let Some(f) = filter {
                    visit(f);
                }
                for a in assignments {
                    visit(&a.expr);
                }
                for proj in returning {
                    visit(&proj.expr);
                }
            }
            PhysicalPlan::InsertValues {
                rows, returning, ..
            } => {
                for row in rows {
                    for v in row {
                        visit(v);
                    }
                }
                for proj in returning {
                    visit(&proj.expr);
                }
            }
            PhysicalPlan::InsertSelect {
                source, returning, ..
            } => {
                stack.push(source);
                for proj in returning {
                    visit(&proj.expr);
                }
            }
            PhysicalPlan::CreateTableAs { source, .. } => {
                stack.push(source);
            }
            PhysicalPlan::RecursiveCte {
                base, recursive, ..
            } => {
                stack.push(recursive);
                stack.push(base);
            }
            // Other variants don't carry user-author'd expressions reachable
            // through subqueries; safe to skip.
            _ => {}
        }
    }
}

fn collect_table_ids_from_typed_expr(
    expr: &TypedExpr,
    out: &mut std::collections::BTreeSet<RelationId>,
) {
    collect_table_ids_from_work(vec![CatalogTableIdWork::Expr(expr)], out);
}

enum CatalogTableIdWork<'a> {
    Expr(&'a TypedExpr),
    Logical(&'a LogicalPlan),
}

fn collect_table_ids_from_work(
    mut stack: Vec<CatalogTableIdWork<'_>>,
    out: &mut std::collections::BTreeSet<RelationId>,
) {
    use aiondb_plan::LogicalPlan as LP;
    while let Some(work) = stack.pop() {
        match work {
            CatalogTableIdWork::Expr(expr) => match &expr.kind {
                TypedExprKind::ScalarSubquery { plan }
                | TypedExprKind::ArraySubquery { plan }
                | TypedExprKind::ExistsSubquery { plan, .. } => {
                    stack.push(CatalogTableIdWork::Logical(plan));
                }
                TypedExprKind::InSubquery {
                    expr: inner, plan, ..
                } => {
                    stack.push(CatalogTableIdWork::Logical(plan));
                    stack.push(CatalogTableIdWork::Expr(inner));
                }
                TypedExprKind::BinaryEq { left, right }
                | TypedExprKind::BinaryNe { left, right }
                | TypedExprKind::BinaryGe { left, right }
                | TypedExprKind::BinaryGt { left, right }
                | TypedExprKind::BinaryLe { left, right }
                | TypedExprKind::BinaryLt { left, right }
                | TypedExprKind::LogicalAnd { left, right }
                | TypedExprKind::LogicalOr { left, right }
                | TypedExprKind::ArithAdd { left, right }
                | TypedExprKind::ArithSub { left, right }
                | TypedExprKind::ArithMul { left, right }
                | TypedExprKind::ArithDiv { left, right }
                | TypedExprKind::ArithMod { left, right }
                | TypedExprKind::Concat { left, right }
                | TypedExprKind::ArrayConcat { left, right }
                | TypedExprKind::ArrayContains { left, right }
                | TypedExprKind::ArrayContainedBy { left, right }
                | TypedExprKind::ArrayOverlap { left, right }
                | TypedExprKind::JsonGet { left, right }
                | TypedExprKind::JsonGetText { left, right }
                | TypedExprKind::JsonPathGet { left, right }
                | TypedExprKind::JsonPathGetText { left, right }
                | TypedExprKind::JsonContains { left, right }
                | TypedExprKind::JsonContainedBy { left, right }
                | TypedExprKind::JsonKeyExists { left, right }
                | TypedExprKind::JsonAnyKeyExists { left, right }
                | TypedExprKind::JsonAllKeysExist { left, right }
                | TypedExprKind::Nullif { left, right }
                | TypedExprKind::IsDistinctFrom { left, right, .. } => {
                    stack.push(CatalogTableIdWork::Expr(right));
                    stack.push(CatalogTableIdWork::Expr(left));
                }
                TypedExprKind::LogicalNot { expr }
                | TypedExprKind::Negate { expr }
                | TypedExprKind::Cast { expr, .. }
                | TypedExprKind::IsNull { expr, .. } => {
                    stack.push(CatalogTableIdWork::Expr(expr));
                }
                TypedExprKind::Like { expr, pattern, .. } => {
                    stack.push(CatalogTableIdWork::Expr(pattern));
                    stack.push(CatalogTableIdWork::Expr(expr));
                }
                TypedExprKind::InList { expr, list, .. } => {
                    for item in list.iter().rev() {
                        stack.push(CatalogTableIdWork::Expr(item));
                    }
                    stack.push(CatalogTableIdWork::Expr(expr));
                }
                TypedExprKind::Between {
                    expr, low, high, ..
                } => {
                    stack.push(CatalogTableIdWork::Expr(high));
                    stack.push(CatalogTableIdWork::Expr(low));
                    stack.push(CatalogTableIdWork::Expr(expr));
                }
                TypedExprKind::CaseWhen {
                    conditions,
                    results,
                    else_result,
                } => {
                    if let Some(expr) = else_result {
                        stack.push(CatalogTableIdWork::Expr(expr));
                    }
                    for expr in results.iter().rev() {
                        stack.push(CatalogTableIdWork::Expr(expr));
                    }
                    for expr in conditions.iter().rev() {
                        stack.push(CatalogTableIdWork::Expr(expr));
                    }
                }
                TypedExprKind::ScalarFunction { args, .. }
                | TypedExprKind::Coalesce { args }
                | TypedExprKind::ArrayConstruct { elements: args }
                | TypedExprKind::UserFunction { args, .. } => {
                    for arg in args.iter().rev() {
                        stack.push(CatalogTableIdWork::Expr(arg));
                    }
                }
                TypedExprKind::WindowFunction {
                    args,
                    partition_by,
                    order_by,
                    ..
                } => {
                    for sort in order_by.iter().rev() {
                        stack.push(CatalogTableIdWork::Expr(&sort.expr));
                    }
                    for expr in partition_by.iter().rev() {
                        stack.push(CatalogTableIdWork::Expr(expr));
                    }
                    for arg in args.iter().rev() {
                        stack.push(CatalogTableIdWork::Expr(arg));
                    }
                }
                _ => {}
            },
            CatalogTableIdWork::Logical(plan) => match plan {
                LP::SeqScan { table_id, .. }
                | LP::ProjectTable { table_id, .. }
                | LP::Aggregate { table_id, .. } => {
                    out.insert(*table_id);
                }
                LP::ProjectSource {
                    source,
                    outputs,
                    filter,
                    ..
                } => {
                    if let Some(f) = filter {
                        stack.push(CatalogTableIdWork::Expr(f));
                    }
                    for proj in outputs.iter().rev() {
                        stack.push(CatalogTableIdWork::Expr(&proj.expr));
                    }
                    stack.push(CatalogTableIdWork::Logical(source));
                }
                LP::AggregateSource { source, .. } => {
                    stack.push(CatalogTableIdWork::Logical(source));
                }
                LP::NestedLoopJoin { left, right, .. } | LP::SetOperation { left, right, .. } => {
                    stack.push(CatalogTableIdWork::Logical(right));
                    stack.push(CatalogTableIdWork::Logical(left));
                }
                LP::RecursiveCte {
                    base, recursive, ..
                } => {
                    stack.push(CatalogTableIdWork::Logical(recursive));
                    stack.push(CatalogTableIdWork::Logical(base));
                }
                _ => {}
            },
        }
    }
}
