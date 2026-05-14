#![allow(
    clippy::match_same_arms,
    clippy::too_many_lines,
    clippy::wildcard_imports
)]

use std::time::Duration;

use super::*;
use aiondb_catalog::CatalogReader;
use aiondb_catalog::PrivilegeTarget;
use aiondb_catalog::QualifiedName;
use aiondb_core::{DataType, DateStyleSetting, Row, SqlState, TimeZoneSetting, TxnId, Value};
use aiondb_executor::SessionSettings;
use aiondb_parser::{
    ResetVariableStatement, SetVariableStatement, ShowVariableStatement,
    TransactionControlStatement,
};
use aiondb_security::AuthenticatedIdentity;

use crate::prepared::{ResultColumn, StatementResult};
use crate::SessionLimits;

const SET_LOCAL_OUTSIDE_TRANSACTION_NOTICE: &str =
    "SET LOCAL can only be used in transaction blocks";
const MAX_PARALLEL_WORKERS_PER_QUERY_SETTING: &str = "max_parallel_workers_per_query";
const DISTRIBUTED_LOOPBACK_NODES_SETTING: &str = "distributed_loopback_nodes";
pub(super) const HNSW_EF_SEARCH_SETTING: &str = "hnsw.ef_search";
const HNSW_ITERATIVE_SCAN_SETTING: &str = "hnsw.iterative_scan";
const HNSW_MAX_SCAN_TUPLES_SETTING: &str = "hnsw.max_scan_tuples";
const HNSW_SCAN_MEM_MULTIPLIER_SETTING: &str = "hnsw.scan_mem_multiplier";
const IVFFLAT_PROBES_SETTING: &str = "ivfflat.probes";
const IVFFLAT_ITERATIVE_SCAN_SETTING: &str = "ivfflat.iterative_scan";
const IVFFLAT_MAX_PROBES_SETTING: &str = "ivfflat.max_probes";
const MAX_SESSION_VARIABLES: usize = 256;
const MAX_SESSION_VARIABLE_NAME_BYTES: usize = 128;
const MAX_SESSION_VARIABLE_VALUE_BYTES: usize = 16 * 1024;
const MAX_SESSION_VARIABLE_TOTAL_BYTES: usize = 256 * 1024;

#[derive(Clone)]
struct CachedSearchPathSchemas {
    catalog_key: usize,
    catalog_revision: u64,
    current_user: String,
    search_path: String,
    schemas: Vec<String>,
}

/// Normalize variable name to lowercase for case-insensitive comparison.
fn normalize_name(name: &str) -> String {
    name.to_lowercase()
}

fn is_simple_guc_identifier_segment(segment: &str) -> bool {
    let mut chars = segment.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return false;
    }
    chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn is_custom_guc_name(name: &str) -> bool {
    let mut parts = name.split('.');
    let first = parts.next();
    let second = parts.next();
    if first.is_none() || second.is_none() {
        return false;
    }
    name.split('.').all(is_simple_guc_identifier_segment)
}

fn show_column_name(name: &str) -> String {
    match name {
        "timezone" => "TimeZone".to_owned(),
        "datestyle" => "DateStyle".to_owned(),
        _ => name.to_owned(),
    }
}

fn session_variables_total_bytes(variables: &HashMap<String, String>) -> usize {
    variables.iter().fold(0usize, |acc, (name, value)| {
        acc.saturating_add(name.len() + value.len())
    })
}

fn transaction_isolation_to_setting(value: IsolationLevel) -> &'static str {
    match value {
        IsolationLevel::ReadCommitted => "read committed",
        IsolationLevel::SnapshotIsolation => "snapshot isolation",
        IsolationLevel::Serializable => "serializable",
        _ => "read committed",
    }
}

fn parse_transaction_isolation_setting(value: &str) -> DbResult<IsolationLevel> {
    // Avoid the per-call `to_ascii_lowercase()` allocation via
    // `eq_ignore_ascii_case` against the small accept-list. Each
    // pairwise compare short-circuits on length/byte mismatch so a
    // canonical lowercase value (the dominant shape) finds its match
    // in O(name.len()) bytes.
    let normalized = value.trim().trim_matches('\'');
    if normalized.eq_ignore_ascii_case("read committed")
        || normalized.eq_ignore_ascii_case("read_committed")
    {
        return Ok(IsolationLevel::ReadCommitted);
    }
    if normalized.eq_ignore_ascii_case("snapshot isolation")
        || normalized.eq_ignore_ascii_case("snapshot_isolation")
        || normalized.eq_ignore_ascii_case("repeatable read")
        || normalized.eq_ignore_ascii_case("repeatable_read")
    {
        return Ok(IsolationLevel::SnapshotIsolation);
    }
    if normalized.eq_ignore_ascii_case("serializable") {
        return Ok(IsolationLevel::Serializable);
    }
    Err(DbError::parse_error(
        SqlState::InvalidParameterValue,
        format!("invalid value for transaction_isolation: \"{value}\""),
    ))
}

fn parse_bool_setting(value: &str, name: &str) -> DbResult<bool> {
    let normalized = value.trim().trim_matches('\'');
    if normalized.eq_ignore_ascii_case("on")
        || normalized.eq_ignore_ascii_case("true")
        || normalized == "1"
    {
        return Ok(true);
    }
    if normalized.eq_ignore_ascii_case("off")
        || normalized.eq_ignore_ascii_case("false")
        || normalized == "0"
    {
        return Ok(false);
    }
    Err(DbError::parse_error(
        SqlState::InvalidParameterValue,
        format!("invalid value for {name}: \"{value}\""),
    ))
}

fn parse_datestyle_setting(value: &str) -> DbResult<()> {
    let normalized = value.trim().trim_matches('\'').trim_matches('"');
    if DateStyleSetting::try_parse(normalized).is_some() {
        Ok(())
    } else {
        Err(DbError::parse_error(
            SqlState::InvalidParameterValue,
            format!("invalid value for datestyle: \"{value}\""),
        ))
    }
}

fn parse_timezone_setting(value: &str) -> DbResult<()> {
    let normalized = value.trim().trim_matches('\'').trim_matches('"');
    if TimeZoneSetting::try_parse(normalized).is_some() {
        Ok(())
    } else {
        Err(DbError::parse_error(
            SqlState::InvalidParameterValue,
            format!("invalid value for timezone: \"{value}\""),
        ))
    }
}

fn parse_intervalstyle_setting(value: &str) -> DbResult<()> {
    let normalized = value.trim().trim_matches('\'').trim_matches('"');
    if normalized.eq_ignore_ascii_case("postgres")
        || normalized.eq_ignore_ascii_case("postgres_verbose")
        || normalized.eq_ignore_ascii_case("sql_standard")
        || normalized.eq_ignore_ascii_case("iso_8601")
    {
        Ok(())
    } else {
        Err(DbError::parse_error(
            SqlState::InvalidParameterValue,
            format!("invalid value for intervalstyle: \"{value}\""),
        ))
    }
}

fn bool_setting_to_string(value: bool) -> &'static str {
    if value {
        "on"
    } else {
        "off"
    }
}

pub(super) fn default_transaction_isolation_for_record(record: &SessionRecord) -> IsolationLevel {
    effective_session_variable_ref_for_record(record, "default_transaction_isolation")
        .and_then(|value| parse_transaction_isolation_setting(value).ok())
        .unwrap_or(IsolationLevel::ReadCommitted)
}

pub(super) fn default_transaction_read_only_for_record(record: &SessionRecord) -> bool {
    effective_session_variable_ref_for_record(record, "default_transaction_read_only")
        .and_then(|value| parse_bool_setting(value, "default_transaction_read_only").ok())
        .unwrap_or(false)
}

pub(super) fn default_transaction_deferrable_for_record(record: &SessionRecord) -> bool {
    effective_session_variable_ref_for_record(record, "default_transaction_deferrable")
        .and_then(|value| parse_bool_setting(value, "default_transaction_deferrable").ok())
        .unwrap_or(false)
}

pub(super) fn transaction_read_only_for_record(record: &SessionRecord) -> bool {
    effective_session_variable_ref_for_record(record, "transaction_read_only")
        .and_then(|value| parse_bool_setting(value, "transaction_read_only").ok())
        .unwrap_or(false)
}

pub(super) fn set_transaction_characteristics_in_record(
    record: &mut SessionRecord,
    isolation: Option<IsolationLevel>,
    read_only: Option<bool>,
    deferrable: Option<bool>,
    is_local: bool,
) -> DbResult<()> {
    if let Some(isolation) = isolation {
        {
            let target = if is_local {
                &mut record.local_session_variables
            } else {
                &mut record.session_variables
            };
            insert_session_variable_checked(
                target,
                if is_local {
                    "transaction_isolation".to_owned()
                } else {
                    "default_transaction_isolation".to_owned()
                },
                transaction_isolation_to_setting(isolation).to_owned(),
                is_local,
            )?;
        }
        if is_local {
            if let Some(txn) = record.active_txn.as_mut() {
                txn.isolation = isolation;
            }
        }
    }

    if let Some(read_only) = read_only {
        let target = if is_local {
            &mut record.local_session_variables
        } else {
            &mut record.session_variables
        };
        insert_session_variable_checked(
            target,
            if is_local {
                "transaction_read_only".to_owned()
            } else {
                "default_transaction_read_only".to_owned()
            },
            bool_setting_to_string(read_only).to_owned(),
            is_local,
        )?;
    }

    if let Some(deferrable) = deferrable {
        let target = if is_local {
            &mut record.local_session_variables
        } else {
            &mut record.session_variables
        };
        insert_session_variable_checked(
            target,
            if is_local {
                "transaction_deferrable".to_owned()
            } else {
                "default_transaction_deferrable".to_owned()
            },
            bool_setting_to_string(deferrable).to_owned(),
            is_local,
        )?;
    }

    Ok(())
}

fn ensure_session_variable_write_allowed(
    variables: &HashMap<String, String>,
    name: &str,
    value: &str,
    is_local: bool,
) -> DbResult<()> {
    if name.len() > MAX_SESSION_VARIABLE_NAME_BYTES {
        return Err(DbError::program_limit(format!(
            "SET {}variable name exceeds maximum of {MAX_SESSION_VARIABLE_NAME_BYTES} bytes",
            if is_local { "LOCAL " } else { "" }
        )));
    }
    if value.len() > MAX_SESSION_VARIABLE_VALUE_BYTES {
        return Err(DbError::program_limit(format!(
            "SET {}value for \"{name}\" exceeds maximum of {MAX_SESSION_VARIABLE_VALUE_BYTES} bytes",
            if is_local { "LOCAL " } else { "" }
        )));
    }
    if !variables.contains_key(name) && variables.len() >= MAX_SESSION_VARIABLES {
        return Err(DbError::program_limit(format!(
            "too many {}session variables (maximum {MAX_SESSION_VARIABLES})",
            if is_local { "transaction-local " } else { "" }
        )));
    }

    let mut projected_total = session_variables_total_bytes(variables);
    match variables.get(name) {
        Some(previous_value) => {
            projected_total = projected_total.saturating_sub(previous_value.len());
        }
        None => {
            projected_total = projected_total.saturating_add(name.len());
        }
    }
    projected_total = projected_total.saturating_add(value.len());
    if projected_total > MAX_SESSION_VARIABLE_TOTAL_BYTES {
        return Err(DbError::program_limit(format!(
            "SET {}session variables exceed maximum total size of {MAX_SESSION_VARIABLE_TOTAL_BYTES} bytes",
            if is_local { "LOCAL " } else { "" }
        )));
    }

    Ok(())
}

fn insert_session_variable_checked(
    variables: &mut HashMap<String, String>,
    name: String,
    value: String,
    is_local: bool,
) -> DbResult<()> {
    ensure_session_variable_write_allowed(variables, &name, &value, is_local)?;
    variables.insert(name, value);
    Ok(())
}

pub(super) fn session_settings_for_record(
    catalog_reader: &dyn CatalogReader,
    txn_id: TxnId,
    record: &SessionRecord,
) -> DbResult<SessionSettings> {
    let is_superuser = session_is_superuser(catalog_reader, txn_id, record)?;
    Ok(SessionSettings::new(
        effective_session_variables_for_record(record),
        record.tenant_schema_name.clone(),
        Some(current_user_for_record(record)),
        is_superuser,
    ))
}

pub(super) fn resolve_hnsw_ef_search_setting(
    settings: &SessionSettings,
) -> DbResult<Option<usize>> {
    settings
        .resolve_value(HNSW_EF_SEARCH_SETTING)
        .map(|value| parse_positive_integer_setting_value(HNSW_EF_SEARCH_SETTING, &value))
        .transpose()
}

pub(super) fn effective_session_variables_for_record(
    record: &SessionRecord,
) -> HashMap<String, String> {
    let mut values = record.session_variables.clone();
    if record.active_txn.is_some() {
        values.extend(record.local_session_variables.clone());
    }
    values
}

pub(super) fn effective_session_variable_for_record(
    record: &SessionRecord,
    name: &str,
) -> Option<String> {
    if record.active_txn.is_some() {
        if let Some(value) = record.local_session_variables.get(name) {
            return Some(value.clone());
        }
    }
    record.session_variables.get(name).cloned()
}

pub(super) fn effective_session_variable_ref_for_record<'a>(
    record: &'a SessionRecord,
    name: &str,
) -> Option<&'a str> {
    if record.active_txn.is_some() {
        if let Some(value) = record.local_session_variables.get(name) {
            return Some(value.as_str());
        }
    }
    record.session_variables.get(name).map(String::as_str)
}

pub(super) fn resolved_search_path_for_record_ref(record: &SessionRecord) -> &str {
    record
        .tenant_schema_name
        .as_deref()
        .or_else(|| effective_session_variable_ref_for_record(record, "search_path"))
        .unwrap_or("public")
}

pub(super) fn resolved_search_path_for_record(record: &SessionRecord) -> String {
    resolved_search_path_for_record_ref(record).to_owned()
}

pub(super) fn effective_search_path_schemas_for_record(
    catalog_reader: &dyn CatalogReader,
    txn_id: TxnId,
    record: &SessionRecord,
) -> DbResult<Vec<String>> {
    // Per-thread cache. The bench harness drives all queries on a
    // single tokio worker, so a thread-local cache has the same hit
    // rate as a global RwLock'd cache but skips the lock and the
    // String-equality linear scan that fired on every Execute.
    thread_local! {
        static CACHE: std::cell::RefCell<Vec<CachedSearchPathSchemas>>
            = const { std::cell::RefCell::new(Vec::new()) };
    }

    let search_path = resolved_search_path_for_record(record);
    let current_user = current_user_for_record(record);
    let catalog_revision = catalog_reader.catalog_revision(txn_id)?;
    let catalog_key = std::ptr::from_ref::<dyn CatalogReader>(catalog_reader).cast::<()>() as usize;

    if let Some(schemas) = CACHE.with(|cell| {
        let cache = cell.borrow();
        cache
            .iter()
            .find(|entry| {
                entry.catalog_key == catalog_key
                    && entry.catalog_revision == catalog_revision
                    && entry.current_user == current_user
                    && entry.search_path == search_path
            })
            .map(|entry| entry.schemas.clone())
    }) {
        return Ok(schemas);
    }

    let mut schemas = Vec::new();
    for component in parse_search_path_components(&search_path) {
        let schema_name = if component == "$user" {
            current_user.clone()
        } else {
            component
        };
        if schema_name.is_empty()
            || schemas
                .iter()
                .any(|existing: &String| existing.eq_ignore_ascii_case(&schema_name))
        {
            continue;
        }
        if catalog_reader
            .get_schema(txn_id, &QualifiedName::unqualified(&schema_name))?
            .is_some()
        {
            schemas.push(schema_name);
        }
    }

    CACHE.with(|cell| {
        let mut cache = cell.borrow_mut();
        if cache.len() >= 64 {
            cache.remove(0);
        }
        cache.push(CachedSearchPathSchemas {
            catalog_key,
            catalog_revision,
            current_user,
            search_path,
            schemas: schemas.clone(),
        });
    });

    Ok(schemas)
}

pub(super) fn primary_search_path_schema_for_record(
    catalog_reader: &dyn CatalogReader,
    txn_id: TxnId,
    record: &SessionRecord,
) -> DbResult<Option<String>> {
    Ok(
        effective_search_path_schemas_for_record(catalog_reader, txn_id, record)?
            .into_iter()
            .next(),
    )
}

pub(super) fn effective_limits_for_record(record: &SessionRecord) -> DbResult<SessionLimits> {
    let mut limits = record.info.limits.clone();
    if record.active_txn.is_some() {
        if let Some(value) = record.local_session_variables.get("lock_timeout") {
            limits.lock_timeout = parse_timeout_value(value)?;
        }
        if let Some(value) = record.local_session_variables.get("statement_timeout") {
            limits.statement_timeout = parse_timeout_value(value)?;
        }
        if let Some(value) = record
            .local_session_variables
            .get(MAX_PARALLEL_WORKERS_PER_QUERY_SETTING)
        {
            limits.max_parallel_workers_per_query = parse_parallel_workers_per_query_value(value)?;
        }
    }
    Ok(limits)
}

pub(super) fn resolve_distributed_loopback_nodes(
    settings: &SessionSettings,
    runtime_defaults: &[String],
) -> DbResult<Vec<String>> {
    match settings.resolve_value(DISTRIBUTED_LOOPBACK_NODES_SETTING) {
        Some(value) => parse_distributed_loopback_nodes_value(&value),
        None => Ok(runtime_defaults.to_vec()),
    }
}

pub(super) fn resolve_distributed_fragment_target_nodes(
    settings: &SessionSettings,
    runtime_loopback_defaults: &[String],
    runtime_remote_nodes: &[aiondb_config::RemoteNodeConfig],
) -> DbResult<Vec<String>> {
    let nodes = resolve_distributed_loopback_nodes(settings, runtime_loopback_defaults)?;
    let explicit_loopback_setting = settings
        .resolve_value(DISTRIBUTED_LOOPBACK_NODES_SETTING)
        .is_some_and(|_| nodes != runtime_loopback_defaults);
    if explicit_loopback_setting {
        return Ok(nodes);
    }

    if !runtime_remote_nodes.is_empty() {
        let mut nodes = Vec::new();
        for remote in runtime_remote_nodes {
            if !nodes
                .iter()
                .any(|node: &String| node.eq_ignore_ascii_case(&remote.node_id))
            {
                nodes.push(remote.node_id.clone());
            }
        }
        return Ok(nodes);
    }

    Ok(nodes)
}

pub(super) fn apply_session_setting_to_record(
    record: &mut SessionRecord,
    name: &str,
    value: &str,
    is_local: bool,
) -> DbResult<()> {
    let name = normalize_name(name);

    if name == "session_authorization" || name == "role" {
        return Err(DbError::feature_not_supported(format!(
            "set_config cannot change {name}"
        )));
    }

    if name == "standard_conforming_strings" {
        let v = value.trim().to_lowercase();
        if v != "on" && v != "'on'" && v != "1" && v != "true" {
            return Err(DbError::feature_not_supported(
                "standard_conforming_strings cannot be changed from 'on'",
            ));
        }
    }

    if name == "client_encoding" || name == "server_encoding" {
        let v = value.trim().to_lowercase();
        if v != "utf8" && v != "'utf8'" && v != "utf-8" && v != "'utf-8'" {
            return Err(DbError::feature_not_supported(
                "only UTF8 encoding is supported",
            ));
        }
    }

    if name == "lock_timeout" || name == "statement_timeout" {
        parse_timeout_value(value)?;
    }
    if name == MAX_PARALLEL_WORKERS_PER_QUERY_SETTING {
        parse_parallel_workers_per_query_value(value)?;
    }
    if name == DISTRIBUTED_LOOPBACK_NODES_SETTING {
        parse_distributed_loopback_nodes_value(value)?;
    }
    if matches!(
        name.as_str(),
        HNSW_EF_SEARCH_SETTING
            | HNSW_MAX_SCAN_TUPLES_SETTING
            | IVFFLAT_PROBES_SETTING
            | IVFFLAT_MAX_PROBES_SETTING
    ) {
        parse_positive_integer_setting_value(&name, value)?;
    }
    if matches!(
        name.as_str(),
        HNSW_ITERATIVE_SCAN_SETTING | IVFFLAT_ITERATIVE_SCAN_SETTING
    ) {
        parse_pgvector_iterative_scan_setting_value(&name, value)?;
    }
    if name == HNSW_SCAN_MEM_MULTIPLIER_SETTING {
        parse_positive_float_setting_value(&name, value)?;
    }

    if is_local {
        if record.active_txn.is_none() {
            record
                .pending_notices
                .push(SET_LOCAL_OUTSIDE_TRANSACTION_NOTICE.to_owned());
            return Ok(());
        }
        insert_session_variable_checked(
            &mut record.local_session_variables,
            name,
            value.to_owned(),
            true,
        )?;
        return Ok(());
    }

    record.local_session_variables.remove(&name);
    insert_session_variable_checked(
        &mut record.session_variables,
        name.clone(),
        value.to_owned(),
        false,
    )?;
    if name == "lock_timeout" {
        record.info.limits.lock_timeout = parse_timeout_value(value)?;
    } else if name == "statement_timeout" {
        record.info.limits.statement_timeout = parse_timeout_value(value)?;
    } else if name == MAX_PARALLEL_WORKERS_PER_QUERY_SETTING {
        record.info.limits.max_parallel_workers_per_query =
            parse_parallel_workers_per_query_value(value)?;
    }
    Ok(())
}

fn session_is_superuser(
    catalog_reader: &dyn CatalogReader,
    txn_id: TxnId,
    record: &SessionRecord,
) -> DbResult<bool> {
    // Track the *current* session user, not the original LOGIN identity. After
    // `SET SESSION AUTHORIZATION nonsuper`, `is_superuser` must follow the
    // overlay; otherwise a previously-superuser session caches `true` and
    // `SET ROLE` skips the membership check (see audit F1 in
    // engine_session_audit.md).
    let role_name = session_user_for_record_ref(record).to_owned();
    Ok(catalog_reader
        .get_role(txn_id, &role_name)?
        .is_some_and(|role| role.superuser))
}

fn parse_search_path_components(value: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut chars = value.trim().chars().peekable();
    let mut in_quotes = false;
    let mut quoted = false;

    while let Some(ch) = chars.next() {
        match ch {
            '"' => {
                if in_quotes {
                    if matches!(chars.peek(), Some('"')) {
                        chars.next();
                        current.push('"');
                    } else {
                        in_quotes = false;
                        quoted = true;
                    }
                } else if current.trim().is_empty() {
                    in_quotes = true;
                } else {
                    current.push(ch);
                }
            }
            ',' if !in_quotes => {
                parts.push(normalize_search_path_component(&current, quoted));
                current.clear();
                quoted = false;
            }
            _ => current.push(ch),
        }
    }

    parts.push(normalize_search_path_component(&current, quoted));
    parts
}

fn normalize_search_path_component(component: &str, quoted: bool) -> String {
    let trimmed = component.trim();
    if quoted {
        trimmed.to_owned()
    } else {
        trimmed.to_ascii_lowercase()
    }
}

impl Engine {
    pub(super) fn validate_set_variable(
        &self,
        session: &SessionHandle,
        stmt: &SetVariableStatement,
    ) -> DbResult<()> {
        let name = normalize_name(&stmt.name);
        if name == "session_authorization" {
            self.validate_set_session_authorization_target(session, stmt.value.as_str())?;
            return Ok(());
        }
        if name == "role" {
            self.validate_set_role_target(session, stmt.value.as_str())?;
            return Ok(());
        }

        if name.contains('.') && !is_custom_guc_name(&name) {
            return Err(DbError::parse_error(
                SqlState::InvalidParameterValue,
                format!("invalid configuration parameter name \"{name}\""),
            )
            .with_client_detail(
                "Custom parameter names must be two or more simple identifiers separated by dots.",
            ));
        }

        if name == "standard_conforming_strings" {
            let v = stmt.value.trim().to_lowercase();
            if v != "on" && v != "'on'" && v != "1" && v != "true" {
                return Err(DbError::feature_not_supported(
                    "standard_conforming_strings cannot be changed from 'on'",
                ));
            }
            return Ok(());
        }

        if name == "client_encoding" || name == "server_encoding" {
            let v = stmt.value.trim().to_lowercase();
            if v != "utf8" && v != "'utf8'" && v != "utf-8" && v != "'utf-8'" {
                return Err(DbError::feature_not_supported(
                    "only UTF8 encoding is supported",
                ));
            }
            return Ok(());
        }

        if name == "lock_timeout" || name == "statement_timeout" {
            parse_timeout_value(&stmt.value)?;
        }
        if name == "datestyle" {
            parse_datestyle_setting(&stmt.value)?;
        }
        if name == "timezone" {
            parse_timezone_setting(&stmt.value)?;
        }
        if name == "intervalstyle" {
            parse_intervalstyle_setting(&stmt.value)?;
        }
        if name == MAX_PARALLEL_WORKERS_PER_QUERY_SETTING {
            parse_parallel_workers_per_query_value(&stmt.value)?;
        }
        if name == DISTRIBUTED_LOOPBACK_NODES_SETTING {
            parse_distributed_loopback_nodes_value(&stmt.value)?;
        }
        if matches!(
            name.as_str(),
            HNSW_EF_SEARCH_SETTING
                | HNSW_MAX_SCAN_TUPLES_SETTING
                | IVFFLAT_PROBES_SETTING
                | IVFFLAT_MAX_PROBES_SETTING
        ) {
            parse_positive_integer_setting_value(&name, &stmt.value)?;
        }
        if matches!(
            name.as_str(),
            HNSW_ITERATIVE_SCAN_SETTING | IVFFLAT_ITERATIVE_SCAN_SETTING
        ) {
            parse_pgvector_iterative_scan_setting_value(&name, &stmt.value)?;
        }
        if name == HNSW_SCAN_MEM_MULTIPLIER_SETTING {
            parse_positive_float_setting_value(&name, &stmt.value)?;
        }

        Ok(())
    }

    pub(super) fn show_variable_value(
        &self,
        session: &SessionHandle,
        name: &str,
    ) -> DbResult<String> {
        let name = normalize_name(name);
        let txn_id = self.current_txn_id(session)?;

        self.with_session(session, |record| {
            session_settings_for_record(self.catalog_reader.as_ref(), txn_id, record)?
                .current_setting(&name, false)?
                .ok_or_else(|| {
                    DbError::parse_error(
                        SqlState::UndefinedObject,
                        format!("unrecognized configuration parameter \"{name}\""),
                    )
                })
        })
    }

    pub(super) fn execute_set_variable(
        &self,
        session: &SessionHandle,
        stmt: &SetVariableStatement,
    ) -> DbResult<StatementResult> {
        self.validate_set_variable(session, stmt)?;

        let name = normalize_name(&stmt.name);
        let value = stmt.value.clone();

        if name.starts_with("plpgsql.") {
            let reserved =
                self.with_session(session, |record| Ok(record.plpgsql_prefix_reserved))?;
            if reserved {
                return Err(DbError::parse_error(
                    SqlState::InvalidParameterValue,
                    format!("invalid configuration parameter name \"{name}\""),
                )
                .with_client_detail("\"plpgsql\" is a reserved prefix."));
            }
        }

        if stmt.is_local {
            if name == "session_authorization" {
                let target_role =
                    self.validate_set_session_authorization_target(session, value.as_str())?;
                self.with_session_mut(session, |record| {
                    if record.active_txn.is_none() {
                        record
                            .pending_notices
                            .push(SET_LOCAL_OUTSIDE_TRANSACTION_NOTICE.to_owned());
                        return Ok(());
                    }
                    insert_session_variable_checked(
                        &mut record.local_session_variables,
                        "session_authorization".to_owned(),
                        target_role.clone(),
                        true,
                    )?;
                    insert_session_variable_checked(
                        &mut record.local_session_variables,
                        "role".to_owned(),
                        target_role.clone(),
                        true,
                    )?;
                    Ok(())
                })?;
                debug!(variable = %name, "transaction-local session variable set");
                return Ok(super::support::command_ok("SET"));
            }

            if name == "role" {
                self.validate_set_role_target(session, value.as_str())?;
                let target_role = self.with_session(session, |record| {
                    Ok(Self::resolve_set_role_target(record, value.as_str()))
                })?;
                self.with_session_mut(session, |record| {
                    if record.active_txn.is_none() {
                        record
                            .pending_notices
                            .push(SET_LOCAL_OUTSIDE_TRANSACTION_NOTICE.to_owned());
                        return Ok(());
                    }
                    match &target_role {
                        Some(target_role) => {
                            insert_session_variable_checked(
                                &mut record.local_session_variables,
                                "role".to_owned(),
                                target_role.clone(),
                                true,
                            )?;
                        }
                        None => {
                            record.local_session_variables.remove("role");
                        }
                    }
                    Ok(())
                })?;
                debug!(variable = %name, "transaction-local session variable set");
                return Ok(super::support::command_ok("SET"));
            }

            self.with_session_mut(session, |record| {
                if record.active_txn.is_none() {
                    record
                        .pending_notices
                        .push(SET_LOCAL_OUTSIDE_TRANSACTION_NOTICE.to_owned());
                    return Ok(());
                }
                insert_session_variable_checked(
                    &mut record.local_session_variables,
                    name.clone(),
                    value.clone(),
                    true,
                )?;
                Ok(())
            })?;
            debug!(variable = %name, "transaction-local session variable set");
            return Ok(super::support::command_ok("SET"));
        }

        if name == "session_authorization" {
            return self.execute_set_session_authorization(session, stmt.value.as_str());
        }
        if name == "role" {
            return self.execute_set_role(session, stmt.value.as_str());
        }

        // Handle limit variables that need to update session limits.
        if name == "lock_timeout"
            || name == "statement_timeout"
            || name == MAX_PARALLEL_WORKERS_PER_QUERY_SETTING
        {
            let duration = if name == "lock_timeout" || name == "statement_timeout" {
                Some(parse_timeout_value(&value)?)
            } else {
                None
            };
            let parallel_workers = if name == MAX_PARALLEL_WORKERS_PER_QUERY_SETTING {
                Some(parse_parallel_workers_per_query_value(&value)?)
            } else {
                None
            };
            self.with_session_mut(session, |record| {
                record.local_session_variables.remove(&name);
                insert_session_variable_checked(
                    &mut record.session_variables,
                    name.clone(),
                    value.clone(),
                    false,
                )?;
                match name.as_str() {
                    "lock_timeout" => {
                        if let Some(duration) = duration {
                            record.info.limits.lock_timeout = duration;
                        }
                    }
                    "statement_timeout" => {
                        if let Some(duration) = duration {
                            record.info.limits.statement_timeout = duration;
                        }
                    }
                    MAX_PARALLEL_WORKERS_PER_QUERY_SETTING => {
                        if let Some(parallel_workers) = parallel_workers {
                            record.info.limits.max_parallel_workers_per_query = parallel_workers;
                        }
                    }
                    _ => {}
                }
                Ok(())
            })?;
            debug!(variable = %name, "session variable set");
            return Ok(super::support::command_ok("SET"));
        }

        self.with_session_mut(session, |record| {
            record.local_session_variables.remove(&name);
            insert_session_variable_checked(
                &mut record.session_variables,
                name.clone(),
                value.clone(),
                false,
            )?;
            Ok(())
        })?;
        debug!(variable = %name, "session variable set");
        Ok(super::support::command_ok("SET"))
    }

    pub(super) fn execute_show_variable(
        &self,
        session: &SessionHandle,
        stmt: &ShowVariableStatement,
    ) -> DbResult<StatementResult> {
        let name = normalize_name(&stmt.name);

        if name == "all" {
            let txn_id = self.current_txn_id(session)?;
            return self.execute_show_all(session, txn_id);
        }

        let value = self.show_variable_value(session, &stmt.name)?;

        let columns = vec![ResultColumn {
            name: show_column_name(&name),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        }];
        let rows = vec![Row::new(vec![Value::Text(value)])];
        Ok(StatementResult::Query { columns, rows })
    }

    fn execute_show_all(
        &self,
        session: &SessionHandle,
        txn_id: TxnId,
    ) -> DbResult<StatementResult> {
        let columns = vec![
            ResultColumn {
                name: "name".to_owned(),
                data_type: DataType::Text,
                text_type_modifier: None,
                nullable: false,
            },
            ResultColumn {
                name: "setting".to_owned(),
                data_type: DataType::Text,
                text_type_modifier: None,
                nullable: false,
            },
        ];

        let well_known = [
            "application_name",
            "bytea_output",
            "client_encoding",
            "client_min_messages",
            "datestyle",
            "default_transaction_isolation",
            "default_transaction_read_only",
            "default_transaction_deferrable",
            "in_hot_standby",
            "integer_datetimes",
            "intervalstyle",
            "is_superuser",
            "lc_collate",
            "lc_ctype",
            "max_identifier_length",
            "max_parallel_workers_per_query",
            "role",
            "search_path",
            "server_encoding",
            "server_version",
            "standard_conforming_strings",
            "distributed_loopback_nodes",
            "timezone",
            "transaction_isolation",
            "transaction_read_only",
            "transaction_deferrable",
        ];

        let rows = self.with_session(session, |record| {
            let mut result: Vec<Row> = Vec::new();
            let settings =
                session_settings_for_record(self.catalog_reader.as_ref(), txn_id, record)?;
            for var_name in &well_known {
                let value = settings.resolve_value(var_name).unwrap_or_default();
                result.push(Row::new(vec![
                    Value::Text(var_name.to_string()),
                    Value::Text(value),
                ]));
            }
            // Add any custom session variables not in well_known
            for (k, v) in &effective_session_variables_for_record(record) {
                if !well_known.contains(&k.as_str()) {
                    result.push(Row::new(vec![
                        Value::Text(k.clone()),
                        Value::Text(v.clone()),
                    ]));
                }
            }
            Ok(result)
        })?;

        Ok(StatementResult::Query { columns, rows })
    }

    pub(super) fn execute_reset_variable(
        &self,
        session: &SessionHandle,
        stmt: &ResetVariableStatement,
    ) -> DbResult<StatementResult> {
        let name = normalize_name(&stmt.name);
        if name == "session_authorization" {
            return self.execute_reset_session_authorization(session);
        }
        if name == "role" {
            return self.execute_reset_role(session);
        }
        let default_limits = self.config.default_limits.clone();
        let default_distributed_loopback_nodes = self
            .runtime_config
            .distributed
            .loopback_remote_nodes
            .join(",");
        self.with_session_mut(session, |record| {
            if name == "all" {
                record.session_variables.clear();
                record.local_session_variables.clear();
                record.info.limits.lock_timeout = default_limits.lock_timeout;
                record.info.limits.statement_timeout = default_limits.statement_timeout;
                record.info.limits.max_parallel_workers_per_query =
                    default_limits.max_parallel_workers_per_query;
                insert_session_variable_checked(
                    &mut record.session_variables,
                    MAX_PARALLEL_WORKERS_PER_QUERY_SETTING.to_owned(),
                    default_limits.max_parallel_workers_per_query.to_string(),
                    false,
                )?;
                insert_session_variable_checked(
                    &mut record.session_variables,
                    DISTRIBUTED_LOOPBACK_NODES_SETTING.to_owned(),
                    default_distributed_loopback_nodes.clone(),
                    false,
                )?;
            } else if name == "transaction_isolation" {
                if record.active_txn.is_some() {
                    let default_isolation = default_transaction_isolation_for_record(record);
                    set_transaction_characteristics_in_record(
                        record,
                        Some(default_isolation),
                        None,
                        None,
                        true,
                    )?;
                } else {
                    record
                        .local_session_variables
                        .remove("transaction_isolation");
                    record.session_variables.remove("transaction_isolation");
                    record
                        .session_variables
                        .remove("default_transaction_isolation");
                }
            } else if name == "transaction_read_only" {
                if record.active_txn.is_some() {
                    let default_read_only = default_transaction_read_only_for_record(record);
                    set_transaction_characteristics_in_record(
                        record,
                        None,
                        Some(default_read_only),
                        None,
                        true,
                    )?;
                } else {
                    record
                        .local_session_variables
                        .remove("transaction_read_only");
                    record.session_variables.remove("transaction_read_only");
                    record
                        .session_variables
                        .remove("default_transaction_read_only");
                }
            } else if name == "transaction_deferrable" {
                if record.active_txn.is_some() {
                    let default_deferrable = default_transaction_deferrable_for_record(record);
                    set_transaction_characteristics_in_record(
                        record,
                        None,
                        None,
                        Some(default_deferrable),
                        true,
                    )?;
                } else {
                    record
                        .local_session_variables
                        .remove("transaction_deferrable");
                    record.session_variables.remove("transaction_deferrable");
                    record
                        .session_variables
                        .remove("default_transaction_deferrable");
                }
            } else {
                let keep_as_known_custom = is_custom_guc_name(&name)
                    && (record.session_variables.contains_key(&name)
                        || record.local_session_variables.contains_key(&name));
                record.session_variables.remove(&name);
                record.local_session_variables.remove(&name);
                if keep_as_known_custom {
                    insert_session_variable_checked(
                        &mut record.session_variables,
                        name.clone(),
                        String::new(),
                        false,
                    )?;
                }
                match name.as_str() {
                    "lock_timeout" => {
                        record.info.limits.lock_timeout = default_limits.lock_timeout;
                    }
                    "statement_timeout" => {
                        record.info.limits.statement_timeout = default_limits.statement_timeout;
                    }
                    MAX_PARALLEL_WORKERS_PER_QUERY_SETTING => {
                        record.info.limits.max_parallel_workers_per_query =
                            default_limits.max_parallel_workers_per_query;
                        insert_session_variable_checked(
                            &mut record.session_variables,
                            MAX_PARALLEL_WORKERS_PER_QUERY_SETTING.to_owned(),
                            default_limits.max_parallel_workers_per_query.to_string(),
                            false,
                        )?;
                    }
                    DISTRIBUTED_LOOPBACK_NODES_SETTING => {
                        insert_session_variable_checked(
                            &mut record.session_variables,
                            DISTRIBUTED_LOOPBACK_NODES_SETTING.to_owned(),
                            default_distributed_loopback_nodes.clone(),
                            false,
                        )?;
                    }
                    _ => {}
                }
            }
            Ok(())
        })?;
        debug!(variable = %name, "session variable reset");
        Ok(super::support::command_ok("RESET"))
    }

    pub(super) fn execute_set_transaction(
        &self,
        session: &SessionHandle,
        stmt: &TransactionControlStatement,
    ) -> DbResult<StatementResult> {
        if stmt.isolation.is_none() && stmt.read_only.is_none() && stmt.deferrable.is_none() {
            return Err(DbError::syntax_error(
                "SET TRANSACTION requires at least one transaction option",
            ));
        }

        self.with_session_mut(session, |record| {
            if record.active_txn.is_none() {
                return Err(DbError::transaction_error(
                    SqlState::NoActiveSqlTransaction,
                    "SET TRANSACTION can only be used in transaction blocks",
                ));
            }
            set_transaction_characteristics_in_record(
                record,
                stmt.isolation
                    .map(|mode| super::support::map_transaction_mode(Some(mode))),
                stmt.read_only,
                stmt.deferrable,
                true,
            )
        })?;

        Ok(super::support::command_ok("SET"))
    }

    pub(super) fn execute_set_constraints(
        &self,
        session: &SessionHandle,
        stmt: &aiondb_parser::SetConstraintsStatement,
    ) -> DbResult<StatementResult> {
        let txn_id = self.with_session(session, |record| {
            Ok(record
                .active_txn
                .as_ref()
                .map(|txn| txn.id)
                .unwrap_or_default())
        })?;
        let scope_id = std::ptr::from_ref(self).cast::<()>() as u64;
        if stmt.all {
            aiondb_executor::executor::deferred_fk::set_all(scope_id, txn_id, stmt.deferred);
        } else {
            aiondb_executor::executor::deferred_fk::set_named(
                scope_id,
                txn_id,
                &stmt.names,
                stmt.deferred,
            );
        }
        Ok(super::support::command_ok("SET CONSTRAINTS"))
    }

    pub(super) fn execute_set_session_characteristics(
        &self,
        session: &SessionHandle,
        stmt: &TransactionControlStatement,
    ) -> DbResult<StatementResult> {
        if stmt.isolation.is_none() && stmt.read_only.is_none() && stmt.deferrable.is_none() {
            return Err(DbError::syntax_error(
                "SET SESSION CHARACTERISTICS requires at least one transaction option",
            ));
        }

        self.with_session_mut(session, |record| {
            set_transaction_characteristics_in_record(
                record,
                stmt.isolation
                    .map(|mode| super::support::map_transaction_mode(Some(mode))),
                stmt.read_only,
                stmt.deferrable,
                false,
            )
        })?;

        Ok(super::support::command_ok("SET"))
    }
}

impl Engine {
    fn normalize_role_value(target: &str) -> String {
        target
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .to_owned()
    }

    fn normalize_identifier(target: &str) -> String {
        target.to_ascii_lowercase()
    }

    fn session_user_for_record(record: &SessionRecord) -> String {
        effective_session_variable_for_record(record, "session_authorization")
            .unwrap_or_else(|| record.info.identity.user.clone())
    }

    fn effective_role_for_record(record: &SessionRecord) -> String {
        effective_session_variable_for_record(record, "role")
            .filter(|role| !role.eq_ignore_ascii_case("none"))
            .unwrap_or_else(|| Self::session_user_for_record(record))
    }

    fn resolve_set_role_target(record: &SessionRecord, raw_target: &str) -> Option<String> {
        let target = Self::normalize_role_value(raw_target);
        if target.eq_ignore_ascii_case("none") {
            return None;
        }
        if target.eq_ignore_ascii_case("session_user") {
            return Some(Self::session_user_for_record(record));
        }
        if target.eq_ignore_ascii_case("current_user")
            || target.eq_ignore_ascii_case("current_role")
        {
            return Some(Self::effective_role_for_record(record));
        }
        Some(Self::normalize_identifier(&target))
    }

    pub(super) fn validate_set_role_target(
        &self,
        session: &SessionHandle,
        target: &str,
    ) -> DbResult<()> {
        let txn_id = self.current_txn_id(session)?;
        let catalog_role_system_active =
            crate::catalog_authorizer::catalog_has_any_roles(self.catalog_reader.as_ref())?;
        let (session_user, current_role, is_superuser, identity_role_system_active, target_role) =
            self.with_session(session, |record| {
                let session_user = Self::session_user_for_record(record);
                let current_role = Self::effective_role_for_record(record);
                let resolved = Self::resolve_set_role_target(record, target);
                Ok((
                    session_user,
                    current_role,
                    { session_is_superuser(self.catalog_reader.as_ref(), txn_id, record)? },
                    crate::catalog_authorizer::role_system_active(
                        self.catalog_reader.as_ref(),
                        &session_identity_for_record(record),
                    )?,
                    resolved,
                ))
            })?;
        let role_system_active = catalog_role_system_active || identity_role_system_active;

        let Some(target_role) = target_role else {
            return Ok(());
        };

        if target_role.is_empty() {
            return Err(DbError::syntax_error(
                "SET ROLE requires a role name or NONE",
            ));
        }

        if role_system_active
            && self
                .catalog_reader
                .get_role(txn_id, &target_role)?
                .is_none()
        {
            return Err(DbError::parse_error(
                SqlState::UndefinedObject,
                format!("role \"{target_role}\" does not exist"),
            ));
        }

        if role_system_active
            && !is_superuser
            && !target_role.eq_ignore_ascii_case(&session_user)
            && !target_role.eq_ignore_ascii_case(&current_role)
            && !role_is_granted_to_user(
                self.catalog_reader.as_ref(),
                txn_id,
                &session_user,
                &target_role,
            )?
        {
            return Err(DbError::insufficient_privilege(format!(
                "permission denied to set role \"{target_role}\""
            )));
        }

        Ok(())
    }

    fn execute_set_role(&self, session: &SessionHandle, target: &str) -> DbResult<StatementResult> {
        self.validate_set_role_target(session, target)?;
        let target_role = self.with_session(session, |record| {
            Ok(Self::resolve_set_role_target(record, target))
        })?;
        let Some(target_role) = target_role else {
            return self.execute_reset_role(session);
        };

        self.with_session_mut(session, |record| {
            record.local_session_variables.remove("role");
            insert_session_variable_checked(
                &mut record.session_variables,
                "role".to_owned(),
                target_role.clone(),
                false,
            )?;
            Ok(())
        })?;

        Ok(super::support::command_ok("SET"))
    }

    pub(super) fn validate_set_session_authorization_target(
        &self,
        session: &SessionHandle,
        target: &str,
    ) -> DbResult<String> {
        let txn_id = self.current_txn_id(session)?;
        let catalog_role_system_active =
            crate::catalog_authorizer::catalog_has_any_roles(self.catalog_reader.as_ref())?;
        let target_role = Self::normalize_identifier(&Self::normalize_role_value(target));
        if target_role.is_empty() {
            return Err(DbError::syntax_error(
                "SET SESSION AUTHORIZATION requires a role name",
            ));
        }

        let (session_user, is_superuser, identity_role_system_active) =
            self.with_session(session, |record| {
                Ok((
                    record.info.identity.user.clone(),
                    { session_is_superuser(self.catalog_reader.as_ref(), txn_id, record)? },
                    crate::catalog_authorizer::role_system_active(
                        self.catalog_reader.as_ref(),
                        &session_identity_for_record(record),
                    )?,
                ))
            })?;
        let role_system_active = catalog_role_system_active || identity_role_system_active;

        if role_system_active && !is_superuser && !target_role.eq_ignore_ascii_case(&session_user) {
            return Err(DbError::insufficient_privilege(
                "permission denied to set session authorization",
            ));
        }

        if role_system_active
            && self
                .catalog_reader
                .get_role(txn_id, &target_role)?
                .is_none()
        {
            return Err(DbError::parse_error(
                SqlState::UndefinedObject,
                format!("role \"{target_role}\" does not exist"),
            ));
        }

        Ok(target_role)
    }

    fn execute_set_session_authorization(
        &self,
        session: &SessionHandle,
        target: &str,
    ) -> DbResult<StatementResult> {
        let target_role = self.validate_set_session_authorization_target(session, target)?;

        self.with_session_mut(session, |record| {
            record
                .local_session_variables
                .remove("session_authorization");
            record.local_session_variables.remove("role");
            insert_session_variable_checked(
                &mut record.session_variables,
                "session_authorization".to_owned(),
                target_role.clone(),
                false,
            )?;
            insert_session_variable_checked(
                &mut record.session_variables,
                "role".to_owned(),
                target_role.clone(),
                false,
            )?;
            Ok(())
        })?;

        Ok(super::support::command_ok("SET"))
    }

    fn execute_reset_session_authorization(
        &self,
        session: &SessionHandle,
    ) -> DbResult<StatementResult> {
        self.with_session_mut(session, |record| {
            record
                .local_session_variables
                .remove("session_authorization");
            record.local_session_variables.remove("role");
            record.session_variables.remove("session_authorization");
            record.session_variables.remove("role");
            Ok(())
        })?;

        Ok(super::support::command_ok("RESET"))
    }

    fn execute_reset_role(&self, session: &SessionHandle) -> DbResult<StatementResult> {
        self.with_session_mut(session, |record| {
            record.local_session_variables.remove("role");
            record.session_variables.remove("role");
            Ok(())
        })?;

        Ok(super::support::command_ok("RESET"))
    }
}

fn role_is_granted_to_user(
    catalog_reader: &dyn CatalogReader,
    txn_id: TxnId,
    session_user: &str,
    target_role: &str,
) -> DbResult<bool> {
    use std::collections::BTreeSet;

    let mut visited = BTreeSet::new();
    let mut frontier = vec![session_user.to_owned()];

    while let Some(role_name) = frontier.pop() {
        let normalized = role_name.to_ascii_lowercase();
        if !visited.insert(normalized.clone()) {
            continue;
        }
        if normalized.eq_ignore_ascii_case(target_role) {
            return Ok(true);
        }
        for privilege in catalog_reader.get_privileges(txn_id, &role_name)? {
            if let PrivilegeTarget::Role(member_of) = privilege.target {
                let member_normalized = member_of.to_ascii_lowercase();
                if member_normalized.eq_ignore_ascii_case(target_role) {
                    return Ok(true);
                }
                if !visited.contains(&member_normalized) {
                    frontier.push(member_of);
                }
            }
        }
    }
    Ok(false)
}

pub(super) fn session_user_for_record(record: &SessionRecord) -> String {
    session_user_for_record_ref(record).to_owned()
}

pub(super) fn session_user_for_record_ref(record: &SessionRecord) -> &str {
    effective_session_variable_ref_for_record(record, "session_authorization")
        .unwrap_or(record.info.identity.user.as_str())
}

pub(super) fn current_user_for_record(record: &SessionRecord) -> String {
    current_user_for_record_ref(record).to_owned()
}

pub(super) fn current_user_for_record_ref(record: &SessionRecord) -> &str {
    effective_session_variable_ref_for_record(record, "role")
        .filter(|role| !role.eq_ignore_ascii_case("none"))
        .unwrap_or_else(|| session_user_for_record_ref(record))
}

pub(super) fn session_identity_for_record(record: &SessionRecord) -> AuthenticatedIdentity {
    let current_user = current_user_for_record(record);
    AuthenticatedIdentity {
        user: current_user.clone(),
        database_id: record.info.identity.database_id,
        roles: vec![current_user],
    }
}

/// Parse a timeout value from a SET statement.
///
/// Accepts PostgreSQL-compatible formats:
///   - Integer milliseconds: `'5000'`, `5000`
///   - Duration with unit suffix: `'5s'`, `'500ms'`, `'2min'`
///   - Zero or `'0'` means no timeout.
///   - `'default'` is not handled here (handled by RESET).
pub(super) fn parse_timeout_value(value: &str) -> DbResult<Duration> {
    let trimmed = value.trim().trim_matches('\'');

    // Try plain integer (milliseconds, matching PostgreSQL convention).
    if let Ok(ms) = trimmed.parse::<u64>() {
        return Ok(Duration::from_millis(ms));
    }

    // Try suffixed durations. Lazy-lowercase via `Cow` so the dominant
    // shape (`"5s"`, `"500ms"`, `"2min"` already in lowercase) skips the
    // String allocation.
    let lower: std::borrow::Cow<'_, str> = if trimmed.bytes().any(|b| b.is_ascii_uppercase()) {
        std::borrow::Cow::Owned(trimmed.to_ascii_lowercase())
    } else {
        std::borrow::Cow::Borrowed(trimmed)
    };
    let lower = lower.as_ref();
    if let Some(rest) = lower.strip_suffix("ms") {
        if let Ok(ms) = rest.trim().parse::<u64>() {
            return Ok(Duration::from_millis(ms));
        }
    }
    if let Some(rest) = lower.strip_suffix("min") {
        if let Ok(mins) = rest.trim().parse::<u64>() {
            let secs = mins.checked_mul(60).ok_or_else(|| {
                DbError::parse_error(
                    SqlState::InvalidParameterValue,
                    format!("invalid value for timeout parameter: \"{value}\""),
                )
            })?;
            return Ok(Duration::from_secs(secs));
        }
    }
    if let Some(rest) = lower.strip_suffix('s') {
        if let Ok(secs) = rest.trim().parse::<u64>() {
            return Ok(Duration::from_secs(secs));
        }
    }
    if let Some(rest) = lower.strip_suffix('h') {
        if let Ok(hours) = rest.trim().parse::<u64>() {
            let secs = hours.checked_mul(3600).ok_or_else(|| {
                DbError::parse_error(
                    SqlState::InvalidParameterValue,
                    format!("invalid value for timeout parameter: \"{value}\""),
                )
            })?;
            return Ok(Duration::from_secs(secs));
        }
    }

    Err(DbError::parse_error(
        SqlState::InvalidParameterValue,
        format!("invalid value for timeout parameter: \"{value}\""),
    ))
}

pub(super) fn parse_parallel_workers_per_query_value(value: &str) -> DbResult<usize> {
    let trimmed = value.trim().trim_matches('"').trim_matches('\'');
    let parsed = trimmed.parse::<usize>().map_err(|_| {
        DbError::parse_error(
            SqlState::InvalidParameterValue,
            format!(
                "invalid value for parameter \"{MAX_PARALLEL_WORKERS_PER_QUERY_SETTING}\": \"{value}\""
            ),
        )
    })?;
    if parsed == 0 {
        return Err(DbError::parse_error(
            SqlState::InvalidParameterValue,
            format!("{MAX_PARALLEL_WORKERS_PER_QUERY_SETTING} must be greater than zero"),
        ));
    }
    Ok(parsed)
}

pub(super) fn parse_positive_integer_setting_value(name: &str, value: &str) -> DbResult<usize> {
    let trimmed = value.trim().trim_matches('"').trim_matches('\'');
    let parsed = trimmed.parse::<usize>().map_err(|_| {
        DbError::parse_error(
            SqlState::InvalidParameterValue,
            format!("invalid value for parameter \"{name}\": \"{value}\""),
        )
    })?;
    if parsed == 0 {
        return Err(DbError::parse_error(
            SqlState::InvalidParameterValue,
            format!("{name} must be greater than zero"),
        ));
    }
    Ok(parsed)
}

fn parse_positive_float_setting_value(name: &str, value: &str) -> DbResult<f64> {
    let trimmed = value.trim().trim_matches('"').trim_matches('\'');
    let parsed = trimmed.parse::<f64>().map_err(|_| {
        DbError::parse_error(
            SqlState::InvalidParameterValue,
            format!("invalid value for parameter \"{name}\": \"{value}\""),
        )
    })?;
    if !parsed.is_finite() || parsed <= 0.0 {
        return Err(DbError::parse_error(
            SqlState::InvalidParameterValue,
            format!("{name} must be greater than zero"),
        ));
    }
    Ok(parsed)
}

fn parse_pgvector_iterative_scan_setting_value(name: &str, value: &str) -> DbResult<&'static str> {
    let trimmed = value.trim().trim_matches('"').trim_matches('\'');
    if trimmed.eq_ignore_ascii_case("off") {
        return Ok("off");
    }
    if trimmed.eq_ignore_ascii_case("strict_order") {
        return Ok("strict_order");
    }
    if trimmed.eq_ignore_ascii_case("relaxed_order") {
        return Ok("relaxed_order");
    }
    Err(DbError::parse_error(
        SqlState::InvalidParameterValue,
        format!("invalid value for parameter \"{name}\": \"{value}\""),
    ))
}

pub(super) fn parse_distributed_loopback_nodes_value(value: &str) -> DbResult<Vec<String>> {
    let trimmed = value.trim().trim_matches('"').trim_matches('\'');
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    let mut nodes = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for part in trimmed.split(',') {
        let node = part.trim();
        if node.is_empty() {
            return Err(DbError::parse_error(
                SqlState::InvalidParameterValue,
                format!(
                    "invalid value for parameter \"{DISTRIBUTED_LOOPBACK_NODES_SETTING}\": \"{value}\""
                ),
            ));
        }
        if seen.insert(node.to_ascii_lowercase()) {
            nodes.push(node.to_owned());
        }
    }
    Ok(nodes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_config::RemoteNodeConfig;
    use std::collections::HashMap;

    fn remote_node(node_id: &str) -> RemoteNodeConfig {
        RemoteNodeConfig {
            node_id: node_id.to_owned(),
            addr: "127.0.0.1:7543".to_owned(),
        }
    }

    fn settings(values: HashMap<String, String>) -> SessionSettings {
        SessionSettings::new(values, None, None, false)
    }

    #[test]
    fn fragment_targets_prefer_runtime_remote_nodes_when_session_uses_defaults() {
        let targets = resolve_distributed_fragment_target_nodes(
            &settings(HashMap::new()),
            &["loop-a".to_owned()],
            &[remote_node("node-a"), remote_node("node-b")],
        )
        .expect("resolve targets");

        assert_eq!(targets, vec!["node-a", "node-b"]);
    }

    #[test]
    fn fragment_targets_prefer_runtime_remote_nodes_when_default_loopback_setting_is_present() {
        let mut values = HashMap::new();
        values.insert(DISTRIBUTED_LOOPBACK_NODES_SETTING.to_owned(), String::new());

        let targets = resolve_distributed_fragment_target_nodes(
            &settings(values),
            &[],
            &[remote_node("node-a")],
        )
        .expect("resolve targets");

        assert_eq!(targets, vec!["node-a"]);
    }

    #[test]
    fn fragment_targets_do_not_duplicate_remote_nodes_case_insensitively() {
        let targets = resolve_distributed_fragment_target_nodes(
            &settings(HashMap::new()),
            &[],
            &[
                remote_node("NODE-A"),
                remote_node("node-a"),
                remote_node("node-b"),
            ],
        )
        .expect("resolve targets");

        assert_eq!(targets, vec!["NODE-A", "node-b"]);
    }

    #[test]
    fn fragment_targets_use_runtime_loopback_nodes_when_no_remote_nodes_exist() {
        let targets = resolve_distributed_fragment_target_nodes(
            &settings(HashMap::new()),
            &["loop-a".to_owned(), "loop-b".to_owned()],
            &[],
        )
        .expect("resolve targets");

        assert_eq!(targets, vec!["loop-a", "loop-b"]);
    }

    #[test]
    fn explicit_session_loopback_targets_override_runtime_remote_nodes() {
        let mut values = HashMap::new();
        values.insert(
            DISTRIBUTED_LOOPBACK_NODES_SETTING.to_owned(),
            "session-node".to_owned(),
        );

        let targets = resolve_distributed_fragment_target_nodes(
            &settings(values),
            &["loop-a".to_owned()],
            &[remote_node("node-a")],
        )
        .expect("resolve targets");

        assert_eq!(targets, vec!["session-node"]);
    }
}
