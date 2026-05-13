#![allow(clippy::missing_errors_doc)]

use std::fs;
use std::io::{Read as _, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use aiondb_catalog_store::{
    replication::{
        install_replication_seed as install_catalog_replication_seed,
        CatalogReplicationSeedManifest,
    },
    CatalogStore,
};
use aiondb_config::V0_1_PRODUCT_CONSTRAINTS;
use aiondb_core::{DbError, DbResult};
use aiondb_storage_engine::{
    ensure_storage_contract_for_open, install_replication_seed as install_storage_replication_seed,
    StorageBackendHandle, StorageBackendKind, StorageReplicationSeedManifest,
};
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::Engine;

#[cfg(test)]
use super::{api::StartupParams, QueryEngine};

#[cfg(test)]
use crate::{prepared::StatementResult, session::SessionHandle, EngineBuilder};

#[cfg(test)]
use aiondb_config::{RuntimeConfig, SecurityProfile, StorageBackend};

#[cfg(test)]
use aiondb_core::{Row, Value};

#[cfg(test)]
use aiondb_security::{AllowAllAuthorizer, Credential, TransportInfo};

#[cfg(test)]
struct ExportTestHook {
    entered: std::sync::mpsc::SyncSender<()>,
    resume: std::sync::mpsc::Receiver<()>,
}

#[cfg(test)]
fn export_test_hook_cell() -> &'static std::sync::Mutex<Option<ExportTestHook>> {
    static HOOK: std::sync::OnceLock<std::sync::Mutex<Option<ExportTestHook>>> =
        std::sync::OnceLock::new();
    HOOK.get_or_init(|| std::sync::Mutex::new(None))
}

#[cfg(test)]
fn install_export_test_hook(
    entered: std::sync::mpsc::SyncSender<()>,
    resume: std::sync::mpsc::Receiver<()>,
) {
    let mut hook = export_test_hook_cell().lock().expect("export hook lock");
    *hook = Some(ExportTestHook { entered, resume });
}

#[cfg(test)]
fn maybe_pause_engine_seed_export() {
    let hook = export_test_hook_cell()
        .lock()
        .expect("export hook lock")
        .take();
    if let Some(hook) = hook {
        let _ = hook.entered.send(());
        let _ = hook.resume.recv();
    }
}

const ENGINE_REPLICATION_SEED_VERSION: u64 = 1;
const ENGINE_REPLICATION_MANIFEST_FILENAME: &str = "manifest.json";
const ENGINE_REPLICATION_STORAGE_DIRNAME: &str = "storage";
const ENGINE_REPLICATION_CATALOG_DIRNAME: &str = "catalog";
const MAX_ENGINE_REPLICATION_MANIFEST_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EngineReplicationSeedManifest {
    pub version: u64,
    pub storage_dir: String,
    pub catalog_dir: String,
    pub storage: StorageReplicationSeedManifest,
    pub catalog: CatalogReplicationSeedManifest,
}

impl EngineReplicationSeedManifest {
    fn new(
        storage: StorageReplicationSeedManifest,
        catalog: CatalogReplicationSeedManifest,
    ) -> Self {
        Self {
            version: ENGINE_REPLICATION_SEED_VERSION,
            storage_dir: ENGINE_REPLICATION_STORAGE_DIRNAME.to_string(),
            catalog_dir: ENGINE_REPLICATION_CATALOG_DIRNAME.to_string(),
            storage,
            catalog,
        }
    }
}

#[derive(Clone)]
pub(crate) struct EngineReplicationHandle {
    storage_backend: Arc<StorageBackendHandle>,
    catalog_store: Arc<CatalogStore>,
    export_barrier: Arc<RwLock<()>>,
}

impl std::fmt::Debug for EngineReplicationHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineReplicationHandle")
            .field("storage_backend", &self.storage_backend.kind())
            .finish_non_exhaustive()
    }
}

impl EngineReplicationHandle {
    pub(crate) fn new(
        storage_backend: Arc<StorageBackendHandle>,
        catalog_store: Arc<CatalogStore>,
        export_barrier: Arc<RwLock<()>>,
    ) -> Self {
        Self {
            storage_backend,
            catalog_store,
            export_barrier,
        }
    }

    fn export(&self, seed_dir: &Path) -> DbResult<EngineReplicationSeedManifest> {
        export_seed_atomically(seed_dir, "engine replication seed export", |staging_root| {
            let _guard = self.export_barrier.write().map_err(|e| {
                DbError::internal(format!("engine replication export barrier poisoned: {e}"))
            })?;

            let storage = self.storage_backend.export_replication_seed_locked(
                &staging_root.join(ENGINE_REPLICATION_STORAGE_DIRNAME),
            )?;
            let catalog = self.catalog_store.export_replication_seed_locked(
                &staging_root.join(ENGINE_REPLICATION_CATALOG_DIRNAME),
            )?;
            let manifest = EngineReplicationSeedManifest::new(storage, catalog);
            write_manifest(staging_root, &manifest)?;
            #[cfg(test)]
            maybe_pause_engine_seed_export();
            Ok(manifest)
        })
    }
}

impl Engine {
    pub fn export_replication_seed(
        &self,
        seed_dir: impl AsRef<Path>,
    ) -> DbResult<EngineReplicationSeedManifest> {
        warn!(
            release_line = V0_1_PRODUCT_CONSTRAINTS.release_line,
            "{}",
            V0_1_PRODUCT_CONSTRAINTS.clustering_summary()
        );
        let seed_dir = seed_dir.as_ref();
        let handle = self.replication_handle.as_ref().ok_or_else(|| {
            DbError::feature_not_supported(
                "replication seed export is unavailable for this engine configuration",
            )
        })?;
        handle.export(seed_dir)
    }
}

pub fn install_replication_seed(
    seed_dir: impl AsRef<Path>,
    data_dir: impl AsRef<Path>,
) -> DbResult<EngineReplicationSeedManifest> {
    warn!(
        release_line = V0_1_PRODUCT_CONSTRAINTS.release_line,
        "{}",
        V0_1_PRODUCT_CONSTRAINTS.clustering_summary()
    );
    let seed_dir = seed_dir.as_ref();
    let data_dir = data_dir.as_ref();
    let manifest = read_manifest(seed_dir)?;
    if manifest.version != ENGINE_REPLICATION_SEED_VERSION {
        return Err(DbError::internal(format!(
            "engine replication seed manifest version {} is not supported (expected {ENGINE_REPLICATION_SEED_VERSION})",
            manifest.version
        )));
    }
    validate_manifest_dirname(&manifest.storage_dir, "storage_dir")?;
    validate_manifest_dirname(&manifest.catalog_dir, "catalog_dir")?;

    let storage_seed_dir = seed_dir.join(&manifest.storage_dir);
    let catalog_seed_dir = seed_dir.join(&manifest.catalog_dir);
    install_engine_seed_atomically(&storage_seed_dir, &catalog_seed_dir, data_dir, &manifest)?;
    Ok(manifest)
}

fn install_engine_seed_atomically(
    storage_seed_dir: &Path,
    catalog_seed_dir: &Path,
    data_dir: &Path,
    manifest: &EngineReplicationSeedManifest,
) -> DbResult<()> {
    ensure_target_absent_or_empty(data_dir, "engine replication seed install")?;
    let parent_dir = data_dir.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent_dir).map_err(|error| {
        DbError::internal(format!(
            "failed to create engine replication seed install parent {}: {error}",
            parent_dir.display()
        ))
    })?;
    sync_dir(parent_dir)?;

    let staging_dir = staging_dir_path(data_dir);
    if staging_dir.exists() {
        fs::remove_dir_all(&staging_dir).map_err(|error| {
            DbError::internal(format!(
                "failed to clear stale engine replication seed staging directory {}: {error}",
                staging_dir.display()
            ))
        })?;
    }

    let install_result = (|| -> DbResult<()> {
        fs::create_dir_all(&staging_dir).map_err(|error| {
            DbError::internal(format!(
                "failed to create engine replication seed staging directory {}: {error}",
                staging_dir.display()
            ))
        })?;
        sync_dir(&staging_dir)?;
        ensure_storage_contract_for_open(
            &staging_dir,
            storage_backend_kind_from_manifest(manifest.storage.backend.as_str())?,
        )?;
        install_storage_replication_seed(
            storage_seed_dir,
            storage_target_root(&staging_dir, manifest.storage.backend.as_str())?,
        )?;
        install_catalog_replication_seed(catalog_seed_dir, staging_dir.join("catalog_wal"))?;
        sync_dir(&staging_dir)?;
        if data_dir.exists() {
            fs::remove_dir(data_dir).map_err(|error| {
                DbError::internal(format!(
                    "failed to replace empty engine replication target {}: {error}",
                    data_dir.display()
                ))
            })?;
        }
        fs::rename(&staging_dir, data_dir).map_err(|error| {
            DbError::internal(format!(
                "failed to publish engine replication target {}: {error}",
                data_dir.display()
            ))
        })?;
        sync_dir(parent_dir)
    })();

    if install_result.is_err() && staging_dir.exists() {
        let _ = fs::remove_dir_all(&staging_dir);
    }

    install_result
}

fn storage_target_root(data_dir: &Path, backend: &str) -> DbResult<PathBuf> {
    match backend {
        "durable" => Ok(data_dir.join("wal")),
        "disk" => Ok(data_dir.join("disk")),
        "page_engine" => Ok(data_dir.join("page_engine")),
        "lsm" => Ok(data_dir.join("lsm")),
        "in_memory" => Err(DbError::feature_not_supported(
            "in-memory storage does not support replication seed install",
        )),
        other => Err(DbError::internal(format!(
            "unknown storage backend in engine replication manifest: {other}"
        ))),
    }
}

fn storage_backend_kind_from_manifest(backend: &str) -> DbResult<StorageBackendKind> {
    match backend {
        "durable" => Ok(StorageBackendKind::Durable),
        "disk" => Ok(StorageBackendKind::Disk),
        "page_engine" => Ok(StorageBackendKind::PageEngine),
        "lsm" => Ok(StorageBackendKind::Lsm),
        "in_memory" => Ok(StorageBackendKind::InMemory),
        other => Err(DbError::internal(format!(
            "unknown storage backend in engine replication manifest: {other}"
        ))),
    }
}

/// Validate that a manifest directory name is a simple name without path traversal.
fn validate_manifest_dirname(name: &str, field: &str) -> DbResult<()> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name == ".."
        || name == "."
        || name.contains("../")
        || name.contains("..\\")
    {
        return Err(DbError::internal(format!(
            "engine replication manifest {field} contains invalid path component: {name}"
        )));
    }
    Ok(())
}

fn export_seed_atomically<T>(
    seed_dir: &Path,
    context: &str,
    build: impl FnOnce(&Path) -> DbResult<T>,
) -> DbResult<T> {
    ensure_target_absent_or_empty(seed_dir, context)?;
    let parent_dir = seed_dir.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent_dir).map_err(|error| {
        DbError::internal(format!(
            "failed to create {context} parent {}: {error}",
            parent_dir.display()
        ))
    })?;
    sync_dir(parent_dir)?;

    let staging_dir = staging_dir_path(seed_dir);
    if staging_dir.exists() {
        fs::remove_dir_all(&staging_dir).map_err(|error| {
            DbError::internal(format!(
                "failed to clear stale {context} staging directory {}: {error}",
                staging_dir.display()
            ))
        })?;
    }

    let export_result = (|| -> DbResult<T> {
        fs::create_dir_all(&staging_dir).map_err(|error| {
            DbError::internal(format!(
                "failed to create {context} staging directory {}: {error}",
                staging_dir.display()
            ))
        })?;
        sync_dir(&staging_dir)?;
        let manifest = build(&staging_dir)?;
        sync_dir(&staging_dir)?;
        if seed_dir.exists() {
            fs::remove_dir(seed_dir).map_err(|error| {
                DbError::internal(format!(
                    "failed to replace empty {context} target {}: {error}",
                    seed_dir.display()
                ))
            })?;
        }
        fs::rename(&staging_dir, seed_dir).map_err(|error| {
            DbError::internal(format!(
                "failed to publish {context} target {}: {error}",
                seed_dir.display()
            ))
        })?;
        sync_dir(parent_dir)?;
        Ok(manifest)
    })();

    if export_result.is_err() && staging_dir.exists() {
        let _ = fs::remove_dir_all(&staging_dir);
    }

    export_result
}

fn ensure_target_absent_or_empty(path: &Path, context: &str) -> DbResult<()> {
    if !path.exists() {
        return Ok(());
    }

    let mut entries = fs::read_dir(path).map_err(|error| {
        DbError::internal(format!(
            "failed to inspect {context} target {}: {error}",
            path.display()
        ))
    })?;
    if entries
        .next()
        .transpose()
        .map_err(|error| {
            DbError::internal(format!(
                "failed to inspect {context} target {}: {error}",
                path.display()
            ))
        })?
        .is_some()
    {
        return Err(DbError::internal(format!(
            "{context} target {} must be empty",
            path.display()
        )));
    }
    Ok(())
}

fn staging_dir_path(target_root: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let file_name = target_root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("target");
    let staging_name = format!(".{file_name}.seed-install-{}-{nanos}", std::process::id());
    target_root
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(staging_name)
}

fn write_manifest(seed_dir: &Path, manifest: &EngineReplicationSeedManifest) -> DbResult<()> {
    let manifest_path = seed_dir.join(ENGINE_REPLICATION_MANIFEST_FILENAME);
    let temp_path = seed_dir.join(format!("{ENGINE_REPLICATION_MANIFEST_FILENAME}.tmp"));
    let manifest_bytes = serde_json::to_vec_pretty(manifest).map_err(|error| {
        DbError::internal(format!(
            "failed to encode engine replication seed manifest {}: {error}",
            manifest_path.display()
        ))
    })?;
    let mut file = create_tmp_file(&temp_path, "engine replication seed manifest")?;
    file.write_all(&manifest_bytes).map_err(|error| {
        DbError::internal(format!(
            "failed to write engine replication seed manifest temp file {}: {error}",
            temp_path.display()
        ))
    })?;
    file.flush().map_err(|error| {
        DbError::internal(format!(
            "failed to flush engine replication seed manifest temp file {}: {error}",
            temp_path.display()
        ))
    })?;
    file.sync_all().map_err(|error| {
        DbError::internal(format!(
            "failed to sync engine replication seed manifest temp file {}: {error}",
            temp_path.display()
        ))
    })?;
    drop(file);
    fs::rename(&temp_path, &manifest_path).map_err(|error| {
        DbError::internal(format!(
            "failed to publish engine replication seed manifest {}: {error}",
            manifest_path.display()
        ))
    })?;
    sync_dir(seed_dir)
}

fn create_tmp_file(path: &Path, context: &str) -> DbResult<fs::File> {
    for attempt in 0..2 {
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(file) => return Ok(file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && attempt == 0 => {
                fs::remove_file(path).map_err(|remove_error| {
                    DbError::internal(format!(
                        "failed to cleanup stale {context} temp file {}: {remove_error}",
                        path.display()
                    ))
                })?;
            }
            Err(error) => {
                return Err(DbError::internal(format!(
                    "failed to create {context} temp file {}: {error}",
                    path.display()
                )));
            }
        }
    }

    Err(DbError::internal(format!(
        "failed to create {context} temp file"
    )))
}

fn sync_dir(path: &Path) -> DbResult<()> {
    aiondb_core::bounded_io::sync_dir(path).map_err(|error| {
        DbError::internal(format!(
            "failed to sync engine replication seed directory {}: {error}",
            path.display()
        ))
    })
}

fn read_file_capped(path: &Path, context: &str, max_bytes: u64) -> DbResult<Vec<u8>> {
    let file = fs::File::open(path).map_err(|error| {
        DbError::internal(format!(
            "failed to open {context} {}: {error}",
            path.display()
        ))
    })?;
    let file_len = file
        .metadata()
        .map_err(|error| {
            DbError::internal(format!(
                "failed to read {context} metadata {}: {error}",
                path.display()
            ))
        })?
        .len();
    if file_len > max_bytes {
        return Err(DbError::internal(format!(
            "{context} {} is {file_len} bytes, exceeding maximum {max_bytes} bytes",
            path.display()
        )));
    }
    let capacity = usize::try_from(file_len).map_err(|_| {
        DbError::internal(format!(
            "{context} {} size {file_len} does not fit in usize",
            path.display()
        ))
    })?;
    let mut bytes = Vec::with_capacity(capacity);
    let mut limited = file.take(max_bytes.saturating_add(1));
    limited.read_to_end(&mut bytes).map_err(|error| {
        DbError::internal(format!(
            "failed to read {context} {}: {error}",
            path.display()
        ))
    })?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_bytes {
        return Err(DbError::internal(format!(
            "{context} {} grew while reading, exceeding maximum {max_bytes} bytes",
            path.display()
        )));
    }
    Ok(bytes)
}

fn read_manifest(seed_dir: &Path) -> DbResult<EngineReplicationSeedManifest> {
    let manifest_path = seed_dir.join(ENGINE_REPLICATION_MANIFEST_FILENAME);
    let manifest_bytes = read_file_capped(
        &manifest_path,
        "engine replication seed manifest",
        MAX_ENGINE_REPLICATION_MANIFEST_BYTES,
    )?;
    serde_json::from_slice(&manifest_bytes).map_err(|error| {
        DbError::internal(format!(
            "failed to decode engine replication seed manifest {}: {error}",
            manifest_path.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::unique_temp_path;
    use std::collections::BTreeMap;
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::time::Duration;

    fn startup_params() -> StartupParams {
        StartupParams {
            database: "default".to_owned(),
            application_name: Some("test".to_owned()),
            options: BTreeMap::new(),
            credential: Credential::Anonymous {
                user: "alice".to_owned(),
            },
            transport: TransportInfo::in_process(),
        }
    }

    fn query_rows(engine: &Engine, session: &SessionHandle, sql: &str) -> Vec<Row> {
        let results = engine
            .execute_sql(session, sql)
            .expect("query should succeed");
        match results.last().expect("query should return one result") {
            StatementResult::Query { rows, .. } => rows.clone(),
            other => panic!("expected query result, got {other:?}"),
        }
    }

    #[test]
    fn engine_replication_seed_round_trip_restores_catalog_and_storage() {
        let data_dir = unique_temp_path("engine-replication", "source");
        let seed_dir = unique_temp_path("engine-replication", "seed");
        let replica_dir = unique_temp_path("engine-replication", "replica");
        let mut runtime = RuntimeConfig::default();
        runtime.storage.backend = StorageBackend::Disk;
        runtime.security =
            aiondb_config::SecurityConfig::from_profile(SecurityProfile::Development);
        runtime.security.allow_ephemeral_users = true;

        let engine = EngineBuilder::new_with_config(data_dir.clone(), runtime.clone())
            .expect("engine should build")
            .with_authorizer(Arc::new(AllowAllAuthorizer))
            .build()
            .unwrap();
        let (session, _) = engine.startup(startup_params()).expect("startup");
        engine
            .execute_sql(
                &session,
                "CREATE ROLE alice SUPERUSER LOGIN; \
                 CREATE SCHEMA repl_schema; \
                 CREATE TABLE repl_schema.repl_items (id INT NOT NULL, val TEXT); \
                 INSERT INTO repl_schema.repl_items VALUES (1, 'seed'); \
                 CREATE ROLE repl_user LOGIN",
            )
            .expect("seed workload should succeed");

        let manifest = engine
            .export_replication_seed(&seed_dir)
            .expect("engine replication seed export should succeed");
        assert_eq!(manifest.storage.backend, StorageBackendKind::Disk.as_str());
        assert!(seed_dir
            .join(ENGINE_REPLICATION_CATALOG_DIRNAME)
            .join("state")
            .is_dir());

        install_replication_seed(&seed_dir, &replica_dir)
            .expect("engine replication seed install should succeed");

        let replica = EngineBuilder::new_with_config(replica_dir.clone(), runtime)
            .expect("replica engine should build")
            .with_authorizer(Arc::new(AllowAllAuthorizer))
            .build()
            .unwrap();
        let (replica_session, _) = replica.startup(startup_params()).expect("replica startup");
        let rows = query_rows(
            &replica,
            &replica_session,
            "SELECT id, val FROM repl_schema.repl_items",
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].values[0], Value::Int(1));
        assert_eq!(rows[0].values[1], Value::Text("seed".to_string()));

        let role = replica
            .catalog_reader
            .get_role(aiondb_core::TxnId::default(), "repl_user")
            .expect("role lookup should succeed")
            .expect("replicated role should exist");
        assert_eq!(role.name, "repl_user");

        let _ = fs::remove_dir_all(&data_dir);
        let _ = fs::remove_dir_all(&seed_dir);
        let _ = fs::remove_dir_all(&replica_dir);
    }

    #[test]
    fn engine_replication_seed_export_quiesces_concurrent_writes() {
        let data_dir = unique_temp_path("engine-replication", "source-quiesce");
        let seed_dir = unique_temp_path("engine-replication", "seed-quiesce");
        let replica_dir = unique_temp_path("engine-replication", "replica-quiesce");
        let mut runtime = RuntimeConfig::default();
        runtime.storage.backend = StorageBackend::Disk;
        runtime.security =
            aiondb_config::SecurityConfig::from_profile(SecurityProfile::Development);
        runtime.security.allow_ephemeral_users = true;

        let engine = Arc::new(
            EngineBuilder::new_with_config(data_dir.clone(), runtime.clone())
                .expect("engine should build")
                .with_authorizer(Arc::new(AllowAllAuthorizer))
                .build()
                .unwrap(),
        );
        let (session, _) = engine.startup(startup_params()).expect("startup");
        engine
            .execute_sql(
                &session,
                "CREATE TABLE repl_before_export (id INT NOT NULL, val TEXT); \
                 INSERT INTO repl_before_export VALUES (1, 'seed')",
            )
            .expect("seed workload should succeed");

        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (resume_tx, resume_rx) = mpsc::sync_channel(1);
        install_export_test_hook(entered_tx, resume_rx);

        let export_engine = Arc::clone(&engine);
        let export_seed_dir = seed_dir.clone();
        let export_thread =
            std::thread::spawn(move || export_engine.export_replication_seed(&export_seed_dir));

        entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("export should pause while holding the seed barrier");
        assert!(
            !seed_dir.exists(),
            "engine seed export must not expose the final root before the atomic rename"
        );

        let writer_engine = Arc::clone(&engine);
        let (writer_done_tx, writer_done_rx) = mpsc::sync_channel(1);
        let writer_thread = std::thread::spawn(move || {
            let (writer_session, _) = writer_engine
                .startup(startup_params())
                .expect("writer startup");
            let result = writer_engine.execute_sql(
                &writer_session,
                "CREATE TABLE repl_after_export (id INT NOT NULL)",
            );
            writer_done_tx
                .send(result)
                .expect("writer result should be observable");
        });

        assert!(
            writer_done_rx
                .recv_timeout(Duration::from_millis(100))
                .is_err(),
            "concurrent DDL should block while export holds the seed barrier"
        );

        resume_tx
            .send(())
            .expect("export thread should still be waiting on the test hook");
        export_thread
            .join()
            .expect("export thread should not panic")
            .expect("engine replication seed export should succeed");
        writer_done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("writer should complete once export finishes")
            .expect("concurrent DDL should succeed after export");
        writer_thread
            .join()
            .expect("writer thread should not panic");

        install_replication_seed(&seed_dir, &replica_dir)
            .expect("engine replication seed install should succeed");

        let replica = EngineBuilder::new_with_config(replica_dir.clone(), runtime)
            .expect("replica engine should build")
            .with_authorizer(Arc::new(AllowAllAuthorizer))
            .build()
            .unwrap();
        let (replica_session, _) = replica.startup(startup_params()).expect("replica startup");
        let rows = query_rows(
            &replica,
            &replica_session,
            "SELECT id, val FROM repl_before_export",
        );
        assert_eq!(rows.len(), 1);
        replica
            .execute_sql(&replica_session, "SELECT * FROM repl_after_export")
            .expect_err("writes that complete after export must be absent from the seed");

        let _ = fs::remove_dir_all(&data_dir);
        let _ = fs::remove_dir_all(&seed_dir);
        let _ = fs::remove_dir_all(&replica_dir);
    }

    #[test]
    fn engine_replication_seed_install_is_atomic_when_catalog_install_fails() {
        let seed_dir = unique_temp_path("engine-replication", "seed-install-atomic");
        let storage_seed_dir = seed_dir.join(ENGINE_REPLICATION_STORAGE_DIRNAME);
        let storage_state_dir = storage_seed_dir.join("state");
        fs::create_dir_all(&storage_state_dir).expect("storage state dir should be creatable");
        fs::write(storage_state_dir.join("root.txt"), b"seed")
            .expect("storage seed file should be writable");
        fs::write(
            storage_seed_dir.join("manifest.json"),
            serde_json::to_vec_pretty(&StorageReplicationSeedManifest {
                version: 1,
                backend: StorageBackendKind::Disk.as_str().to_string(),
                state_dir: "state".to_string(),
                files: vec!["root.txt".to_string()],
            })
            .expect("storage manifest should encode"),
        )
        .expect("storage manifest should be writable");

        let catalog_seed_dir = seed_dir.join(ENGINE_REPLICATION_CATALOG_DIRNAME);
        fs::create_dir_all(&catalog_seed_dir).expect("catalog seed dir should be creatable");
        fs::write(
            catalog_seed_dir.join("manifest.json"),
            serde_json::to_vec_pretty(&CatalogReplicationSeedManifest {
                version: 1,
                state_dir: "state".to_string(),
                files: Vec::new(),
            })
            .expect("catalog manifest should encode"),
        )
        .expect("catalog manifest should be writable");

        let engine_manifest = EngineReplicationSeedManifest {
            version: ENGINE_REPLICATION_SEED_VERSION,
            storage_dir: ENGINE_REPLICATION_STORAGE_DIRNAME.to_string(),
            catalog_dir: ENGINE_REPLICATION_CATALOG_DIRNAME.to_string(),
            storage: StorageReplicationSeedManifest {
                version: 1,
                backend: StorageBackendKind::Disk.as_str().to_string(),
                state_dir: "state".to_string(),
                files: vec!["root.txt".to_string()],
            },
            catalog: CatalogReplicationSeedManifest {
                version: 1,
                state_dir: "state".to_string(),
                files: Vec::new(),
            },
        };
        write_manifest(&seed_dir, &engine_manifest).expect("engine manifest should be writable");

        let replica_dir = unique_temp_path("engine-replication", "replica-install-atomic");
        let err = install_replication_seed(&seed_dir, &replica_dir)
            .expect_err("engine install should fail when catalog seed is incomplete");
        assert!(err
            .to_string()
            .contains("catalog replication seed state directory is missing"));
        assert!(
            !replica_dir.exists(),
            "failed engine install must not publish a partial data directory"
        );

        let _ = fs::remove_dir_all(&seed_dir);
        let _ = fs::remove_dir_all(&replica_dir);
    }

    #[test]
    fn engine_replication_seed_install_rejects_unknown_manifest_version() {
        let seed_dir = unique_temp_path("engine-replication", "seed-version");
        let engine_manifest = EngineReplicationSeedManifest {
            version: ENGINE_REPLICATION_SEED_VERSION + 1,
            storage_dir: ENGINE_REPLICATION_STORAGE_DIRNAME.to_string(),
            catalog_dir: ENGINE_REPLICATION_CATALOG_DIRNAME.to_string(),
            storage: StorageReplicationSeedManifest {
                version: 1,
                backend: StorageBackendKind::Disk.as_str().to_string(),
                state_dir: "state".to_string(),
                files: Vec::new(),
            },
            catalog: CatalogReplicationSeedManifest {
                version: 1,
                state_dir: "state".to_string(),
                files: Vec::new(),
            },
        };
        fs::create_dir_all(&seed_dir).expect("seed dir should be creatable");
        write_manifest(&seed_dir, &engine_manifest).expect("engine manifest should be writable");

        let replica_dir = unique_temp_path("engine-replication", "replica-version");
        let err = install_replication_seed(&seed_dir, &replica_dir)
            .expect_err("unknown engine seed versions must fail");
        assert!(err.to_string().contains("version"));

        let _ = fs::remove_dir_all(&seed_dir);
        let _ = fs::remove_dir_all(&replica_dir);
    }
}
