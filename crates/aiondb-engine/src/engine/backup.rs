#![allow(
    clippy::redundant_closure_for_method_calls,
    clippy::too_many_lines,
    clippy::unnecessary_wraps
)]

use std::fmt::Write;
use std::io::{Read, Write as IoWrite};
use std::path::{Component, Path, PathBuf};
use std::time::Instant;

use aiondb_catalog::{
    CatalogReader, ColumnDescriptor, IndexDescriptor, QualifiedName, SchemaDescriptor,
    TableDescriptor, ViewDescriptor,
};
use aiondb_config::V0_1_PRODUCT_CONSTRAINTS;
use aiondb_core::{
    escape_sql_literal as escape_sql_string, hex_encode, DataType, DbError, DbResult, SchemaId,
    TxnId, Value,
};
use aiondb_tx::Snapshot;
use sha2::{Digest, Sha256};
use tracing::{info, warn};

use super::{api::QueryEngine, Engine};
use crate::{prepared::StatementResult, session::SessionHandle};

/// Maximum backup file size (256 MiB) to prevent unbounded output.
const MAX_BACKUP_SIZE: usize = 256 * 1024 * 1024;
const BACKUP_HEADER_BANNER: &str = "-- AionDB Backup";
const BACKUP_HEADER_FORMAT_PREFIX: &str = "-- backup-format-version:";
const BACKUP_HEADER_ENGINE_PREFIX: &str = "-- engine-version:";
const BACKUP_HEADER_PAYLOAD_SHA256_PREFIX: &str = "-- backup-payload-sha256:";
const CURRENT_BACKUP_FORMAT_VERSION: u32 = 2;
const LEGACY_BACKUP_FORMAT_VERSION: u32 = 0;
const CHECKSUM_BACKUP_FORMAT_VERSION: u32 = 2;
const RESTORE_SAVEPOINT_NAME: &str = "__aiondb_restore__";
const BACKUP_ROOT_DIR_NAME: &str = "backups";
const ALLOW_LEGACY_BACKUP_RESTORE_ENV: &str = "AIONDB_ALLOW_LEGACY_BACKUP_RESTORE";

#[derive(Clone, Debug, Eq, PartialEq)]
struct BackupManifest {
    format_version: u32,
    engine_version: Option<String>,
    payload_sha256: Option<String>,
}

impl BackupManifest {
    fn legacy() -> Self {
        Self {
            format_version: LEGACY_BACKUP_FORMAT_VERSION,
            engine_version: None,
            payload_sha256: None,
        }
    }
}

/// Validate that a backup/restore path is lexically safe.
///
/// Rejects:
/// - Path traversal via `..` components
/// - Absolute paths (which could target arbitrary system files like /etc/shadow)
/// - Paths with a root or prefix component (e.g. C:\ on Windows)
///
/// Relative paths are resolved separately against a fixed base directory after
/// lexical validation so backup/restore stays inside the server working tree.
fn validate_path(path: &str) -> DbResult<()> {
    let p = Path::new(path);
    for component in p.components() {
        match component {
            Component::ParentDir => {
                return Err(DbError::internal(
                    "backup/restore path must not contain '..' components",
                ));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(DbError::internal(
                    "backup/restore path must be relative, not absolute",
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

fn resolve_backup_path(path: &str) -> DbResult<PathBuf> {
    let base_dir = resolve_backup_root_dir()?;
    resolve_backup_path_from_base(&base_dir, path)
}

fn resolve_backup_root_dir() -> DbResult<PathBuf> {
    let cwd = std::env::current_dir()
        .map_err(|e| DbError::internal(format!("failed to resolve backup base directory: {e}")))?;
    let backup_root = cwd.join(BACKUP_ROOT_DIR_NAME);
    match std::fs::symlink_metadata(&backup_root) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(DbError::internal(format!(
                "backup root '{}' must not be a symbolic link",
                backup_root.display()
            )));
        }
        Ok(metadata) if !metadata.file_type().is_dir() => {
            return Err(DbError::internal(format!(
                "backup root '{}' is not a directory",
                backup_root.display()
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(&backup_root).map_err(|create_error| {
                DbError::internal(format!(
                    "failed to create backup root directory '{}': {create_error}",
                    backup_root.display()
                ))
            })?;
        }
        Err(error) => {
            return Err(DbError::internal(format!(
                "failed to inspect backup root directory '{}': {error}",
                backup_root.display()
            )));
        }
    }
    backup_root
        .canonicalize()
        .map_err(|e| DbError::internal(format!("failed to resolve backup base directory: {e}")))
}

fn resolve_backup_path_from_base(base_dir: &Path, path: &str) -> DbResult<PathBuf> {
    validate_path(path)?;

    let base_dir = base_dir
        .canonicalize()
        .map_err(|e| DbError::internal(format!("failed to resolve backup base directory: {e}")))?;
    let mut resolved = base_dir.clone();

    for component in Path::new(path).components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => {
                resolved.push(part);
                reject_symlink_component(&resolved, path)?;
            }
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(DbError::internal(format!(
                    "invalid backup/restore path component after validation for '{path}'"
                )));
            }
        }
    }

    Ok(resolved)
}

fn reject_symlink_component(candidate: &Path, original_path: &str) -> DbResult<()> {
    match std::fs::symlink_metadata(candidate) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(DbError::internal(format!(
            "backup/restore path '{original_path}' must not traverse symlinks"
        ))),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(DbError::internal(format!(
            "failed to inspect backup/restore path '{original_path}': {error}"
        ))),
    }
}

fn write_new_backup_file(path: &Path, contents: &[u8]) -> DbResult<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let mut file = options.open(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            DbError::internal(format!(
                "refusing to overwrite existing backup file '{}'",
                path.display()
            ))
        } else {
            DbError::internal(format!("failed to write backup file: {error}"))
        }
    })?;
    set_private_backup_permissions(path, &file);
    file.write_all(contents)
        .map_err(|error| DbError::internal(format!("failed to write backup file: {error}")))?;
    file.sync_all()
        .map_err(|error| DbError::internal(format!("failed to sync backup file: {error}")))?;
    drop(file);
    sync_backup_parent_dir(path)
}

#[cfg(unix)]
fn set_private_backup_permissions(path: &Path, file: &std::fs::File) {
    use std::os::unix::fs::PermissionsExt;

    if let Err(error) = file.set_permissions(std::fs::Permissions::from_mode(0o600)) {
        warn!(
            path = %path.display(),
            %error,
            "failed to apply private permissions to backup file"
        );
    }
}

#[cfg(not(unix))]
fn set_private_backup_permissions(_path: &Path, _file: &std::fs::File) {}

#[cfg(unix)]
fn sync_backup_parent_dir(path: &Path) -> DbResult<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::File::open(parent)
        .and_then(|dir| dir.sync_all())
        .map_err(|error| {
            DbError::internal(format!(
                "failed to sync backup directory '{}': {error}",
                parent.display()
            ))
        })
}

#[cfg(not(unix))]
fn sync_backup_parent_dir(_path: &Path) -> DbResult<()> {
    Ok(())
}

/// Execute a BACKUP DATABASE TO '<path>' statement.
pub(crate) fn execute_backup(
    engine: &Engine,
    session: &SessionHandle,
    path: &str,
) -> DbResult<StatementResult> {
    // BACKUP requires superuser: it exports ALL tables unconditionally.
    let session_info = engine.session_info(session)?;
    if crate::catalog_authorizer::catalog_has_any_roles(engine.catalog_reader.as_ref())?
        && !crate::catalog_authorizer::is_superuser_checked(
            engine.catalog_reader.as_ref(),
            &session_info.identity,
        )?
    {
        return Err(DbError::insufficient_privilege(
            "must be superuser to BACKUP DATABASE",
        ));
    }

    warn!(
        release_line = V0_1_PRODUCT_CONSTRAINTS.release_line,
        "{}",
        V0_1_PRODUCT_CONSTRAINTS.backup_restore_summary()
    );
    let resolved_path = resolve_backup_path(path)?;
    let statement_deadline = backup_statement_deadline(engine, session)?;

    let (txn_id, snapshot) = engine.current_txn_context(session)?;

    let sql = generate_backup_sql(engine, session, statement_deadline, txn_id, &snapshot)?;

    write_new_backup_file(&resolved_path, sql.as_bytes())?;

    info!(path = %path, bytes = sql.len(), "database backup completed");

    Ok(super::support::command_ok("BACKUP"))
}

/// Execute a RESTORE DATABASE FROM '<path>' statement.
pub(crate) fn execute_restore(
    engine: &Engine,
    session: &SessionHandle,
    path: &str,
) -> DbResult<StatementResult> {
    // RESTORE requires superuser: it executes arbitrary DDL/DML.
    let session_info = engine.session_info(session)?;
    if crate::catalog_authorizer::catalog_has_any_roles(engine.catalog_reader.as_ref())?
        && !crate::catalog_authorizer::is_superuser_checked(
            engine.catalog_reader.as_ref(),
            &session_info.identity,
        )?
    {
        return Err(DbError::insufficient_privilege(
            "must be superuser to RESTORE DATABASE",
        ));
    }

    warn!(
        release_line = V0_1_PRODUCT_CONSTRAINTS.release_line,
        "{}",
        V0_1_PRODUCT_CONSTRAINTS.backup_restore_summary()
    );
    let resolved_path = resolve_backup_path(path)?;
    let statement_deadline = backup_statement_deadline(engine, session)?;

    let sql = read_restore_file(&resolved_path)?;
    let (manifest, payload) = parse_backup_document(&sql)?;
    enforce_restore_manifest_security(&manifest)?;
    if manifest.format_version < CHECKSUM_BACKUP_FORMAT_VERSION {
        warn!(
            format_version = manifest.format_version,
            env_var = ALLOW_LEGACY_BACKUP_RESTORE_ENV,
            "restoring legacy backup format without checksum integrity protection"
        );
    }
    validate_backup_payload_integrity(&manifest, payload)?;

    // Parse and execute the manifest payload atomically.
    let statements = aiondb_parser::parse_sql(payload)?;
    let mut count: u64 = 0;
    let had_active_txn = engine.current_txn_id(session)? != TxnId::default();
    let restore_savepoint_name = if had_active_txn {
        Some(engine.create_unique_savepoint(session, RESTORE_SAVEPOINT_NAME)?)
    } else {
        None
    };

    if !had_active_txn {
        engine.begin_transaction(session, aiondb_tx::IsolationLevel::ReadCommitted)?;
    }

    let restore_result: DbResult<()> = (|| {
        for statement in &statements {
            check_backup_interrupts(engine, session, statement_deadline)?;
            engine.execute_statement(session, statement)?;
            count += 1;
        }
        Ok(())
    })();

    match restore_result {
        Ok(()) => {
            if let Some(restore_savepoint_name) = restore_savepoint_name.as_deref() {
                engine.release_savepoint(session, restore_savepoint_name)?;
            } else {
                engine.commit_transaction(session)?;
            }
        }
        Err(error) => {
            let mut error = error;
            let mut restored_outer_transaction = false;
            if let Some(restore_savepoint_name) = restore_savepoint_name.as_deref() {
                if let Err(rollback_error) =
                    engine.rollback_to_savepoint(session, restore_savepoint_name)
                {
                    warn!(
                        error = %rollback_error,
                        "restore rollback-to-savepoint cleanup failed"
                    );
                    error = super::support::with_appended_internal_detail(
                        error,
                        format!("restore rollback-to-savepoint failed: {rollback_error}"),
                    );
                } else if let Err(release_error) =
                    engine.release_savepoint(session, restore_savepoint_name)
                {
                    warn!(
                        error = %release_error,
                        "restore savepoint release cleanup failed"
                    );
                    error = super::support::with_appended_internal_detail(
                        error,
                        format!("restore savepoint release failed: {release_error}"),
                    );
                } else {
                    restored_outer_transaction = true;
                }
            } else if let Err(rollback_error) = engine.rollback_transaction(session) {
                warn!(error = %rollback_error, "restore rollback cleanup failed");
                error = super::support::with_appended_internal_detail(
                    error,
                    format!("restore rollback failed: {rollback_error}"),
                );
            }
            if restored_outer_transaction {
                let _ = engine.with_session_mut(session, |record| {
                    record.suppress_next_transaction_failure_mark = true;
                    Ok(())
                });
            }
            return Err(error);
        }
    }

    info!(
        path = %path,
        statements = count,
        backup_format_version = manifest.format_version,
        backup_engine_version = manifest.engine_version.as_deref().unwrap_or("legacy"),
        "database restore completed"
    );

    Ok(StatementResult::Command {
        tag: "RESTORE".to_owned(),
        rows_affected: count,
    })
}

fn read_restore_file(path: &Path) -> DbResult<String> {
    let file = std::fs::File::open(path)
        .map_err(|e| DbError::internal(format!("failed to read restore file: {e}")))?;
    validate_restore_file_path_stability(path, &file)?;

    let metadata = file
        .metadata()
        .map_err(|e| DbError::internal(format!("failed to read restore file: {e}")))?;
    if metadata.len() > MAX_BACKUP_SIZE as u64 {
        return Err(DbError::program_limit("restore file exceeds maximum size"));
    }

    let mut bytes = Vec::new();
    let mut reader = file.take((MAX_BACKUP_SIZE + 1) as u64);
    reader
        .read_to_end(&mut bytes)
        .map_err(|e| DbError::internal(format!("failed to read restore file: {e}")))?;
    if bytes.len() > MAX_BACKUP_SIZE {
        return Err(DbError::program_limit("restore file exceeds maximum size"));
    }

    String::from_utf8(bytes)
        .map_err(|e| DbError::internal(format!("restore file is not valid UTF-8: {e}")))
}

#[cfg(unix)]
fn validate_restore_file_path_stability(path: &Path, file: &std::fs::File) -> DbResult<()> {
    use std::os::unix::fs::MetadataExt;

    let path_metadata = std::fs::symlink_metadata(path)
        .map_err(|e| DbError::internal(format!("failed to read restore file: {e}")))?;
    if path_metadata.file_type().is_symlink() {
        return Err(DbError::internal(
            "restore file path must not be a symbolic link",
        ));
    }

    let opened_metadata = file
        .metadata()
        .map_err(|e| DbError::internal(format!("failed to read restore file: {e}")))?;
    let current_path_metadata = std::fs::metadata(path)
        .map_err(|e| DbError::internal(format!("failed to read restore file: {e}")))?;

    if opened_metadata.dev() != current_path_metadata.dev()
        || opened_metadata.ino() != current_path_metadata.ino()
    {
        return Err(DbError::internal(
            "restore file changed during validation; aborting restore",
        ));
    }

    Ok(())
}

#[cfg(not(unix))]
fn validate_restore_file_path_stability(_path: &Path, _file: &std::fs::File) -> DbResult<()> {
    Ok(())
}

fn generate_backup_sql(
    engine: &Engine,
    session: &SessionHandle,
    statement_deadline: Option<Instant>,
    txn_id: TxnId,
    snapshot: &Snapshot,
) -> DbResult<String> {
    let catalog = engine.catalog_reader.as_ref();
    let mut body = String::new();
    let schemas = list_backup_schemas(catalog, txn_id)?;
    let mut tables = Vec::new();
    for schema in &schemas {
        check_backup_interrupts(engine, session, statement_deadline)?;
        if should_emit_schema(&schema.name) {
            write_create_schema(&mut body, &schema.name);
        }
        let schema_tables = catalog.list_tables(txn_id, schema.schema_id)?;
        for table in schema_tables {
            check_backup_interrupts(engine, session, statement_deadline)?;
            write_create_table(&mut body, &table)?;
            write_insert_data(
                &mut body,
                engine,
                session,
                statement_deadline,
                txn_id,
                snapshot,
                &table,
            )?;
            tables.push(table);

            if body.len() > MAX_BACKUP_SIZE {
                return Err(DbError::program_limit("backup exceeds maximum size"));
            }
        }
    }

    // 2. Emit CREATE INDEX for non-primary-key indexes
    for table in &tables {
        check_backup_interrupts(engine, session, statement_deadline)?;
        let indexes = catalog.list_indexes(txn_id, table.table_id)?;
        for index in &indexes {
            check_backup_interrupts(engine, session, statement_deadline)?;
            if is_primary_key_index(index, table) {
                continue;
            }
            write_create_index(&mut body, index, table)?;
        }
    }

    // 3. Emit CREATE VIEW statements
    for schema in &schemas {
        let views = catalog.list_views(txn_id, schema.schema_id)?;
        for view in &views {
            check_backup_interrupts(engine, session, statement_deadline)?;
            write_create_view(&mut body, view);
        }
    }

    let mut output = String::new();
    let payload_sha256 = payload_sha256(&body);
    write_backup_header(&mut output, &payload_sha256);
    output.push_str(&body);

    if output.len() > MAX_BACKUP_SIZE {
        return Err(DbError::program_limit("backup exceeds maximum size"));
    }

    Ok(output)
}

fn write_backup_header(output: &mut String, payload_sha256: &str) {
    let _ = writeln!(output, "{BACKUP_HEADER_BANNER}");
    let _ = writeln!(
        output,
        "{BACKUP_HEADER_FORMAT_PREFIX} {CURRENT_BACKUP_FORMAT_VERSION}"
    );
    let _ = writeln!(
        output,
        "{BACKUP_HEADER_ENGINE_PREFIX} {}",
        env!("CARGO_PKG_VERSION")
    );
    let _ = writeln!(
        output,
        "{BACKUP_HEADER_PAYLOAD_SHA256_PREFIX} {payload_sha256}"
    );
    output.push('\n');
}

#[cfg(test)]
fn parse_backup_manifest(sql: &str) -> DbResult<BackupManifest> {
    parse_backup_document(sql).map(|(manifest, _)| manifest)
}

fn parse_backup_document(sql: &str) -> DbResult<(BackupManifest, &str)> {
    let mut saw_banner = false;
    let mut format_version = None;
    let mut engine_version = None;
    let mut payload_sha256 = None;
    let mut offset = 0usize;
    let mut payload_offset = 0usize;

    for chunk in sql.split_inclusive('\n') {
        let line = chunk.strip_suffix('\n').unwrap_or(chunk);
        let trimmed = line.trim();
        if trimmed.is_empty() {
            offset += chunk.len();
            if saw_banner {
                payload_offset = offset;
            }
            continue;
        }

        if !trimmed.starts_with("--") {
            if saw_banner {
                payload_offset = offset;
            }
            break;
        }

        if trimmed == BACKUP_HEADER_BANNER {
            saw_banner = true;
            offset += chunk.len();
            payload_offset = offset;
            continue;
        }

        if !saw_banner {
            break;
        }

        if let Some(raw_version) = trimmed.strip_prefix(BACKUP_HEADER_FORMAT_PREFIX) {
            let parsed = raw_version.trim().parse::<u32>().map_err(|error| {
                DbError::internal(format!(
                    "invalid backup format version in header: {} ({error})",
                    raw_version.trim()
                ))
            })?;
            format_version = Some(parsed);
            offset += chunk.len();
            payload_offset = offset;
            continue;
        }

        if let Some(raw_engine_version) = trimmed.strip_prefix(BACKUP_HEADER_ENGINE_PREFIX) {
            let raw_engine_version = raw_engine_version.trim();
            if !raw_engine_version.is_empty() {
                engine_version = Some(raw_engine_version.to_owned());
            }
            offset += chunk.len();
            payload_offset = offset;
            continue;
        }

        if let Some(raw_payload_sha256) = trimmed.strip_prefix(BACKUP_HEADER_PAYLOAD_SHA256_PREFIX)
        {
            let raw_payload_sha256 = raw_payload_sha256.trim();
            if !raw_payload_sha256.is_empty() {
                payload_sha256 = Some(raw_payload_sha256.to_owned());
            }
            offset += chunk.len();
            payload_offset = offset;
            continue;
        }

        offset += chunk.len();
        payload_offset = offset;
    }

    let manifest = if saw_banner {
        BackupManifest {
            format_version: format_version.unwrap_or(LEGACY_BACKUP_FORMAT_VERSION),
            engine_version,
            payload_sha256,
        }
    } else {
        BackupManifest::legacy()
    };

    match manifest.format_version {
        LEGACY_BACKUP_FORMAT_VERSION..=CURRENT_BACKUP_FORMAT_VERSION => Ok((
            manifest,
            if saw_banner {
                &sql[payload_offset..]
            } else {
                sql
            },
        )),
        other => Err(DbError::internal(format!(
            "unsupported backup format version {other}; supported versions: 0..={CURRENT_BACKUP_FORMAT_VERSION}"
        ))),
    }
}

fn validate_backup_payload_integrity(manifest: &BackupManifest, payload: &str) -> DbResult<()> {
    if manifest.format_version < CHECKSUM_BACKUP_FORMAT_VERSION {
        return Ok(());
    }

    let expected = manifest.payload_sha256.as_deref().ok_or_else(|| {
        DbError::internal(format!(
            "backup format version {} requires a payload checksum",
            manifest.format_version
        ))
    })?;
    let actual = payload_sha256(payload);
    // Use constant-time comparison to prevent timing oracle attacks on the
    // backup checksum. An attacker who can observe restore timing could
    // otherwise forge checksums byte-by-byte.
    // Constant-time comparison that does not leak length information.
    // Both sides are hex strings from SHA-256, so lengths should always
    // match (64 hex chars).  We still avoid short-circuiting on length
    // to prevent any timing oracle.
    let expected_bytes = expected.to_ascii_lowercase();
    let actual_bytes = actual.to_ascii_lowercase();
    let len_diff = u8::from(expected_bytes.len() != actual_bytes.len());
    let content_diff = expected_bytes
        .as_bytes()
        .iter()
        .zip(actual_bytes.as_bytes())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b));
    let checksums_match = (len_diff | content_diff) == 0;
    if !checksums_match {
        return Err(DbError::internal(format!(
            "backup payload checksum mismatch: expected {expected}, got {actual}"
        )));
    }

    Ok(())
}

fn allow_legacy_backup_restore() -> bool {
    std::env::var(ALLOW_LEGACY_BACKUP_RESTORE_ENV).is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

fn enforce_restore_manifest_security(manifest: &BackupManifest) -> DbResult<()> {
    if manifest.format_version < CHECKSUM_BACKUP_FORMAT_VERSION && !allow_legacy_backup_restore() {
        return Err(DbError::internal(format!(
            "backup format version {} is legacy and not checksum-protected; re-export using format version {CHECKSUM_BACKUP_FORMAT_VERSION} or set {ALLOW_LEGACY_BACKUP_RESTORE_ENV}=1 to allow insecure restore",
            manifest.format_version
        )));
    }
    Ok(())
}

fn list_backup_schemas(
    catalog: &dyn CatalogReader,
    txn_id: TxnId,
) -> DbResult<Vec<SchemaDescriptor>> {
    let mut schemas = catalog.list_schemas(txn_id)?;
    if schemas.is_empty() {
        let schema = catalog.get_schema(txn_id, &QualifiedName::unqualified("public"))?;
        if let Some(schema) = schema {
            schemas.push(schema);
        } else {
            schemas.push(SchemaDescriptor {
                schema_id: SchemaId::new(1),
                name: "public".to_owned(),
            });
        }
    }
    schemas.retain(|schema| !is_builtin_schema(&schema.name));
    schemas.sort_by(|left, right| {
        (
            left.name != "public",
            left.name.as_str(),
            left.schema_id.get(),
        )
            .cmp(&(
                right.name != "public",
                right.name.as_str(),
                right.schema_id.get(),
            ))
    });
    Ok(schemas)
}

fn is_builtin_schema(name: &str) -> bool {
    matches!(name, "pg_catalog" | "information_schema")
}

fn should_emit_schema(name: &str) -> bool {
    !matches!(name, "public" | "pg_catalog" | "information_schema")
}

fn is_primary_key_index(index: &IndexDescriptor, table: &TableDescriptor) -> bool {
    if let Some(pk_cols) = &table.primary_key {
        let index_col_ids: Vec<_> = index.key_columns.iter().map(|c| c.column_id).collect();
        index.unique && index_col_ids == *pk_cols
    } else {
        false
    }
}

fn write_create_table(output: &mut String, table: &TableDescriptor) -> DbResult<()> {
    let table_name = format_object_name(&table.name);
    let _ = writeln!(output, "CREATE TABLE {table_name} (");

    let mut parts: Vec<String> = Vec::new();
    for col in &table.columns {
        parts.push(format_column_def(col));
    }

    // Primary key constraint
    if let Some(pk_cols) = &table.primary_key {
        let pk_col_names: Vec<String> = pk_cols
            .iter()
            .filter_map(|col_id| {
                table
                    .columns
                    .iter()
                    .find(|c| c.column_id == *col_id)
                    .map(|c| format_identifier(&c.name))
            })
            .collect();
        if !pk_col_names.is_empty() {
            parts.push(format!("  PRIMARY KEY ({})", pk_col_names.join(", ")));
        }
    }

    // Foreign key constraints
    for fk in &table.foreign_keys {
        let cols: Vec<String> = fk.columns.iter().map(|c| format_identifier(c)).collect();
        let ref_cols: Vec<String> = fk
            .referenced_columns
            .iter()
            .map(|c| format_identifier(c))
            .collect();
        parts.push(format!(
            "  FOREIGN KEY ({}) REFERENCES {} ({})",
            cols.join(", "),
            format_object_name(&QualifiedName::parse(&fk.referenced_table)),
            ref_cols.join(", ")
        ));
    }

    // Check constraints
    for chk in &table.check_constraints {
        let name_part = chk
            .name
            .as_ref()
            .map(|n| format!("CONSTRAINT {} ", format_identifier(n)))
            .unwrap_or_default();
        parts.push(format!("  {name_part}CHECK ({})", chk.expression));
    }

    output.push_str(&parts.join(",\n"));
    output.push_str("\n);\n\n");
    Ok(())
}

fn format_column_def(col: &ColumnDescriptor) -> String {
    let mut def = format!(
        "  {} {}",
        format_identifier(&col.name),
        format_data_type(&col.data_type)
    );

    if !col.nullable {
        def.push_str(" NOT NULL");
    }

    if let Some(default) = &col.default_value {
        let _ = write!(def, " DEFAULT {default}");
    }

    def
}

fn format_data_type(dt: &DataType) -> String {
    match dt {
        DataType::Int => "INT".to_owned(),
        DataType::BigInt => "BIGINT".to_owned(),
        DataType::Real => "REAL".to_owned(),
        DataType::Double => "DOUBLE".to_owned(),
        DataType::Numeric => "NUMERIC".to_owned(),
        DataType::Money => "MONEY".to_owned(),
        DataType::Text => "TEXT".to_owned(),
        DataType::Boolean => "BOOLEAN".to_owned(),
        DataType::Blob => "BLOB".to_owned(),
        DataType::Timestamp => "TIMESTAMP".to_owned(),
        DataType::Date => "DATE".to_owned(),
        DataType::Time => "TIME".to_owned(),
        DataType::TimeTz => "TIMETZ".to_owned(),
        DataType::Interval => "INTERVAL".to_owned(),
        DataType::Tid => "TID".to_owned(),
        DataType::MacAddr => "MACADDR".to_owned(),
        DataType::MacAddr8 => "MACADDR8".to_owned(),
        DataType::PgLsn => "PG_LSN".to_owned(),
        DataType::Uuid => "UUID".to_owned(),
        DataType::TimestampTz => "TIMESTAMPTZ".to_owned(),
        DataType::Jsonb => "JSONB".to_owned(),
        DataType::Vector { dims, .. } => format!("VECTOR({dims})"),
        DataType::Array(inner) => format!("{}[]", format_data_type(inner)),
    }
}

fn write_insert_data(
    output: &mut String,
    engine: &Engine,
    session: &SessionHandle,
    statement_deadline: Option<Instant>,
    txn_id: TxnId,
    snapshot: &Snapshot,
    table: &TableDescriptor,
) -> DbResult<()> {
    let mut stream = engine
        .storage_dml
        .scan_table(txn_id, snapshot, table.table_id, None)?;

    let table_name = format_object_name(&table.name);
    let col_names: Vec<String> = table
        .columns
        .iter()
        .map(|c| format_identifier(&c.name))
        .collect();
    let col_list = col_names.join(", ");

    while let Some(record) = stream.next()? {
        check_backup_interrupts(engine, session, statement_deadline)?;
        let values: Vec<String> = record
            .row
            .values
            .iter()
            .map(value_to_sql_literal)
            .collect::<DbResult<_>>()?;
        let _ = writeln!(
            output,
            "INSERT INTO {table_name} ({col_list}) VALUES ({});",
            values.join(", ")
        );
    }

    Ok(())
}

fn backup_statement_deadline(
    engine: &Engine,
    session: &SessionHandle,
) -> DbResult<Option<Instant>> {
    let timeout = engine.session_info(session)?.limits.statement_timeout;
    Ok(Instant::now().checked_add(timeout))
}

fn check_backup_interrupts(
    engine: &Engine,
    session: &SessionHandle,
    statement_deadline: Option<Instant>,
) -> DbResult<()> {
    if let Some(deadline) = statement_deadline {
        if Instant::now() >= deadline {
            return Err(DbError::query_canceled("statement timeout exceeded"));
        }
    }
    engine.take_cancellation_if_needed(session)
}

fn write_create_index(
    output: &mut String,
    index: &IndexDescriptor,
    table: &TableDescriptor,
) -> DbResult<()> {
    let index_name = format_object_name(&index.name);
    let table_name = format_object_name(&table.name);

    let col_names: Vec<String> = index
        .key_columns
        .iter()
        .filter_map(|key_col| {
            table
                .columns
                .iter()
                .find(|c| c.column_id == key_col.column_id)
                .map(|c| format_identifier(&c.name))
        })
        .collect();

    let unique = if index.unique { "UNIQUE " } else { "" };
    let _ = writeln!(
        output,
        "CREATE {unique}INDEX {index_name} ON {table_name} ({});",
        col_names.join(", ")
    );

    Ok(())
}

fn write_create_schema(output: &mut String, schema_name: &str) {
    let _ = writeln!(output, "CREATE SCHEMA {};", format_identifier(schema_name));
}

fn write_create_view(output: &mut String, view: &ViewDescriptor) {
    let view_name = format_object_name(&view.name);
    if !view.creation_search_path_schemas.is_empty() {
        let search_path = view
            .creation_search_path_schemas
            .iter()
            .map(|schema| format_identifier(schema))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(output, "SET search_path TO {search_path};");
    }
    let _ = writeln!(output, "CREATE VIEW {view_name} AS {};", view.query_sql);
    if !view.creation_search_path_schemas.is_empty() {
        let _ = writeln!(output, "RESET search_path;");
    }
}

fn format_object_name(name: &QualifiedName) -> String {
    match &name.schema {
        Some(schema) => format!(
            "{}.{}",
            quote_backup_identifier(schema),
            quote_backup_identifier(&name.name)
        ),
        None => quote_backup_identifier(&name.name),
    }
}

fn format_identifier(name: &str) -> String {
    quote_backup_identifier(name)
}

/// Always quote identifiers in backup output to prevent injection when
/// table/column names contain special characters, keywords, or double quotes.
///
/// NUL bytes are stripped: PG identifiers cannot legally contain `\0`, and a
/// stray NUL inside a backup identifier would truncate the name in any C-string
/// consumer of the dump (audit catalog F3).
fn quote_backup_identifier(name: &str) -> String {
    let mut sanitized = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch == '\0' {
            continue;
        }
        if ch == '"' {
            sanitized.push_str("\"\"");
        } else {
            sanitized.push(ch);
        }
    }
    format!("\"{sanitized}\"")
}

const MAX_BACKUP_VALUE_LITERAL_DEPTH: usize = 256;

fn value_to_sql_literal(v: &Value) -> DbResult<String> {
    value_to_sql_literal_at_depth(v, 0)
}

fn value_to_sql_literal_at_depth(v: &Value, depth: usize) -> DbResult<String> {
    if depth >= MAX_BACKUP_VALUE_LITERAL_DEPTH {
        return Err(DbError::program_limit(format!(
            "backup value nesting depth exceeds limit {MAX_BACKUP_VALUE_LITERAL_DEPTH}"
        )));
    }
    match v {
        Value::Null => Ok("NULL".to_owned()),
        Value::Int(i) => Ok(i.to_string()),
        Value::BigInt(i) => Ok(i.to_string()),
        Value::Real(f) => Ok(format_float(f64::from(*f))),
        Value::Double(f) => Ok(format_float(*f)),
        Value::Numeric(n) => Ok(n.to_string()),
        Value::Money(value) => Ok(format!(
            "CAST('{}' AS MONEY)",
            escape_sql_string(&Value::Money(*value).to_string())
        )),
        Value::Text(s) => Ok(format!("'{}'", escape_sql_string(s))),
        Value::Boolean(b) => Ok(if *b { "TRUE" } else { "FALSE" }.to_owned()),
        Value::Blob(bytes) => Ok(format!("'\\x{}'", hex_encode(bytes))),
        Value::Timestamp(ts) => Ok(format!("CAST('{ts}' AS TIMESTAMP)")),
        Value::Date(d) => Ok(format!("CAST('{d}' AS DATE)")),
        Value::LargeDate(d) => Ok(format!("CAST('{d}' AS DATE)")),
        Value::Interval(iv) => Ok(format!(
            "CAST('{} months {} days {} microseconds' AS INTERVAL)",
            iv.months, iv.days, iv.micros
        )),
        Value::Tid(value) => Ok(format!("CAST('{value}' AS TID)")),
        Value::MacAddr(value) => Ok(format!("CAST('{value}' AS MACADDR)")),
        Value::MacAddr8(value) => Ok(format!("CAST('{value}' AS MACADDR8)")),
        Value::PgLsn(value) => Ok(format!("CAST('{value}' AS PG_LSN)")),
        Value::Uuid(bytes) => {
            let s = format_uuid(bytes);
            Ok(format!("CAST('{s}' AS UUID)"))
        }
        Value::TimestampTz(ts) => Ok(format!("CAST('{ts}' AS TIMESTAMPTZ)")),
        Value::Jsonb(val) => {
            let json_str = val.to_string();
            Ok(format!("CAST('{}' AS JSONB)", escape_sql_string(&json_str)))
        }
        Value::Vector(vec) => {
            let elems: Vec<String> = vec.values.iter().map(|f| f.to_string()).collect();
            Ok(format!(
                "CAST('[{}]' AS VECTOR({}))",
                elems.join(","),
                vec.dims
            ))
        }
        Value::Array(elements) => {
            let inner: Vec<String> = elements
                .iter()
                .map(|value| value_to_sql_literal_at_depth(value, depth + 1))
                .collect::<DbResult<_>>()?;
            Ok(format!("ARRAY[{}]", inner.join(", ")))
        }
        Value::Time(t) => Ok(format!("CAST('{t}' AS TIME)")),
        Value::TimeTz(t, offset) => Ok(format!("CAST('{t}{offset}' AS TIMETZ)")),
    }
}

fn format_float(f: f64) -> String {
    if f.is_nan() {
        "CAST('NaN' AS DOUBLE)".to_owned()
    } else if f.is_infinite() {
        if f.is_sign_positive() {
            "CAST('Infinity' AS DOUBLE)".to_owned()
        } else {
            "CAST('-Infinity' AS DOUBLE)".to_owned()
        }
    } else {
        let s = f.to_string();
        // Ensure it looks like a float literal
        if s.contains('.') || s.contains('e') || s.contains('E') {
            s
        } else {
            format!("{s}.0")
        }
    }
}

fn payload_sha256(payload: &str) -> String {
    let digest = Sha256::digest(payload.as_bytes());
    hex_encode(digest.as_ref())
}

fn format_uuid(bytes: &[u8; 16]) -> String {
    let mut out = String::with_capacity(36);
    for (i, b) in bytes.iter().enumerate() {
        if i == 4 || i == 6 || i == 8 || i == 10 {
            out.push('-');
        }
        let _ = write!(out, "{b:02x}");
    }
    out
}

#[cfg(test)]
#[path = "backup_tests.rs"]
mod tests;
