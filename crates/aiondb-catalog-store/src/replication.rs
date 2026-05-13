use std::fs;
use std::io::Write;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

use aiondb_core::replication_fs::{
    ensure_target_absent_or_empty, read_file_capped, relative_to_manifest_path, staging_dir_path,
};
use aiondb_core::{DbError, DbResult};
use serde::{Deserialize, Serialize};

use crate::{snapshot::save_catalog_snapshot, CatalogState, CatalogStore};

const CATALOG_REPLICATION_SEED_VERSION: u64 = 1;
const CATALOG_REPLICATION_STATE_DIRNAME: &str = "state";
const CATALOG_REPLICATION_MANIFEST_FILENAME: &str = "manifest.json";
const MAX_CATALOG_REPLICATION_MANIFEST_BYTES: u64 = 8 * 1024 * 1024;

#[cfg(test)]
struct ExportTestHook {
    target: PathBuf,
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
    target: PathBuf,
    entered: std::sync::mpsc::SyncSender<()>,
    resume: std::sync::mpsc::Receiver<()>,
) {
    let mut hook = export_test_hook_cell().lock().expect("export hook lock");
    *hook = Some(ExportTestHook {
        target,
        entered,
        resume,
    });
}

#[cfg(test)]
fn maybe_pause_catalog_seed_export(seed_dir: &Path) {
    let hook = {
        let mut hook = export_test_hook_cell().lock().expect("export hook lock");
        if !hook
            .as_ref()
            .is_some_and(|hook| hook.target.as_path() == seed_dir)
        {
            return;
        }
        hook.take()
    };
    if let Some(hook) = hook {
        let _ = hook.entered.send(());
        let _ = hook.resume.recv();
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CatalogReplicationSeedManifest {
    pub version: u64,
    pub state_dir: String,
    pub files: Vec<String>,
}

impl CatalogReplicationSeedManifest {
    fn new(files: Vec<String>) -> Self {
        Self {
            version: CATALOG_REPLICATION_SEED_VERSION,
            state_dir: CATALOG_REPLICATION_STATE_DIRNAME.to_string(),
            files,
        }
    }
}

impl CatalogStore {
    pub fn export_replication_seed(
        &self,
        seed_dir: impl AsRef<Path>,
    ) -> DbResult<CatalogReplicationSeedManifest> {
        let _guard = self.export_barrier.write().map_err(|e| {
            DbError::internal(format!("catalog replication export barrier poisoned: {e}"))
        })?;
        self.export_replication_seed_locked(seed_dir.as_ref())
    }

    #[doc(hidden)]
    pub fn export_replication_seed_locked(
        &self,
        seed_dir: &Path,
    ) -> DbResult<CatalogReplicationSeedManifest> {
        if !self.read_active_txns()?.is_empty() {
            return Err(DbError::internal(
                "cannot export catalog replication seed while transactions are active",
            ));
        }

        let wal = self.wal.as_ref().ok_or_else(|| {
            DbError::feature_not_supported(
                "catalog replication seed export requires WAL-backed catalog storage",
            )
        })?;
        let checkpoint_lsn = wal.flush_and_last_lsn()?;
        let wal_dir = wal.wal_dir().ok_or_else(|| {
            DbError::internal(
                "catalog replication seed export requires a catalog WAL handle with a known directory",
            )
        })?;
        let state = self.read_state()?.clone();
        export_replication_seed_from_root(wal_dir, &state, checkpoint_lsn, seed_dir)
    }
}

pub fn install_replication_seed(
    seed_dir: impl AsRef<Path>,
    target_root: impl AsRef<Path>,
) -> DbResult<CatalogReplicationSeedManifest> {
    let seed_dir = seed_dir.as_ref();
    let target_root = target_root.as_ref();
    let manifest = read_manifest(seed_dir)?;
    if manifest.version != CATALOG_REPLICATION_SEED_VERSION {
        return Err(DbError::internal(format!(
            "catalog replication seed manifest version {} is not supported (expected {CATALOG_REPLICATION_SEED_VERSION})",
            manifest.version
        )));
    }
    validate_manifest_dirname(&manifest.state_dir, "state_dir")?;
    let source_state_dir = seed_dir.join(&manifest.state_dir);
    if !source_state_dir.is_dir() {
        return Err(DbError::internal(format!(
            "catalog replication seed state directory is missing: {}",
            source_state_dir.display()
        )));
    }
    let actual_files = collect_relative_files(&source_state_dir, &source_state_dir)?;
    if actual_files != manifest.files {
        return Err(DbError::internal(
            "catalog replication seed manifest file list does not match seed contents",
        ));
    }

    install_tree_atomically(
        &source_state_dir,
        target_root,
        "catalog replication seed install",
    )?;
    Ok(manifest)
}

fn export_replication_seed_from_root(
    source_root: &Path,
    state: &CatalogState,
    checkpoint_lsn: aiondb_wal::Lsn,
    seed_dir: &Path,
) -> DbResult<CatalogReplicationSeedManifest> {
    export_seed_atomically(
        seed_dir,
        "catalog replication seed export",
        |staging_root| {
            let state_dir = staging_root.join(CATALOG_REPLICATION_STATE_DIRNAME);
            copy_tree(source_root, &state_dir)?;
            save_catalog_snapshot(state, checkpoint_lsn, &state_dir)?;
            let files = collect_relative_files(&state_dir, &state_dir)?;
            let manifest = CatalogReplicationSeedManifest::new(files);
            write_manifest(staging_root, &manifest)?;
            Ok(manifest)
        },
    )
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
            "catalog replication manifest {field} contains invalid path component: {name}"
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
    if seed_dir.exists() {
        fs::remove_dir(seed_dir).map_err(|error| {
            DbError::internal(format!(
                "failed to clear empty {context} target {} before export: {error}",
                seed_dir.display()
            ))
        })?;
    }
    fs::create_dir_all(parent_dir).map_err(|error| {
        DbError::internal(format!(
            "failed to create {context} parent {}: {error}",
            parent_dir.display()
        ))
    })?;
    sync_dir(parent_dir)?;

    let staging_root = staging_dir_path(seed_dir);
    if staging_root.exists() {
        fs::remove_dir_all(&staging_root).map_err(|error| {
            DbError::internal(format!(
                "failed to clear stale {context} staging directory {}: {error}",
                staging_root.display()
            ))
        })?;
    }

    let export_result = (|| -> DbResult<T> {
        fs::create_dir_all(&staging_root).map_err(|error| {
            DbError::internal(format!(
                "failed to create {context} staging directory {}: {error}",
                staging_root.display()
            ))
        })?;
        sync_dir(&staging_root)?;
        let manifest = build(&staging_root)?;
        sync_dir(&staging_root)?;
        #[cfg(test)]
        maybe_pause_catalog_seed_export(seed_dir);
        if seed_dir.exists() {
            fs::remove_dir(seed_dir).map_err(|error| {
                DbError::internal(format!(
                    "failed to replace empty {context} target {}: {error}",
                    seed_dir.display()
                ))
            })?;
        }
        fs::rename(&staging_root, seed_dir).map_err(|error| {
            DbError::internal(format!(
                "failed to publish {context} target {}: {error}",
                seed_dir.display()
            ))
        })?;
        sync_dir(parent_dir)?;
        Ok(manifest)
    })();

    if export_result.is_err() && staging_root.exists() {
        let _ = fs::remove_dir_all(&staging_root);
    }

    export_result
}

fn install_tree_atomically(source_root: &Path, target_root: &Path, context: &str) -> DbResult<()> {
    ensure_target_absent_or_empty(target_root, context)?;
    let parent_dir = target_root.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent_dir).map_err(|error| {
        DbError::internal(format!(
            "failed to create {context} parent {}: {error}",
            parent_dir.display()
        ))
    })?;
    sync_dir(parent_dir)?;

    let staging_root = staging_dir_path(target_root);
    if staging_root.exists() {
        fs::remove_dir_all(&staging_root).map_err(|error| {
            DbError::internal(format!(
                "failed to clear stale {context} staging directory {}: {error}",
                staging_root.display()
            ))
        })?;
    }

    let install_result = (|| -> DbResult<()> {
        copy_tree(source_root, &staging_root)?;
        sync_dir(&staging_root)?;
        if target_root.exists() {
            fs::remove_dir(target_root).map_err(|error| {
                DbError::internal(format!(
                    "failed to replace empty {context} target {}: {error}",
                    target_root.display()
                ))
            })?;
        }
        fs::rename(&staging_root, target_root).map_err(|error| {
            DbError::internal(format!(
                "failed to publish {context} target {}: {error}",
                target_root.display()
            ))
        })?;
        sync_dir(parent_dir)
    })();

    if install_result.is_err() && staging_root.exists() {
        let _ = fs::remove_dir_all(&staging_root);
    }

    install_result
}

const MAX_CATALOG_REPLICATION_DIR_DEPTH: usize = 256;

fn copy_tree(source: &Path, target: &Path) -> DbResult<()> {
    copy_tree_at_depth(source, target, 0)
}

fn copy_tree_at_depth(source: &Path, target: &Path, depth: usize) -> DbResult<()> {
    if depth >= MAX_CATALOG_REPLICATION_DIR_DEPTH {
        return Err(DbError::program_limit(format!(
            "catalog replication directory depth exceeds limit {MAX_CATALOG_REPLICATION_DIR_DEPTH}"
        )));
    }
    fs::create_dir_all(target).map_err(|error| {
        DbError::internal(format!(
            "failed to create catalog replication copy target {}: {error}",
            target.display()
        ))
    })?;

    for entry in fs::read_dir(source).map_err(|error| {
        DbError::internal(format!(
            "failed to enumerate catalog replication source {}: {error}",
            source.display()
        ))
    })? {
        let entry = entry.map_err(|error| {
            DbError::internal(format!(
                "failed to enumerate catalog replication source {}: {error}",
                source.display()
            ))
        })?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        let file_type = entry.file_type().map_err(|error| {
            DbError::internal(format!(
                "failed to stat catalog replication source {}: {error}",
                source_path.display()
            ))
        })?;

        // Reject symlinks to prevent path traversal attacks.
        if file_type.is_symlink() {
            return Err(DbError::internal(format!(
                "refusing to follow symlink during catalog replication: {}",
                source_path.display()
            )));
        }

        if file_type.is_dir() {
            copy_tree_at_depth(&source_path, &target_path, depth + 1)?;
        } else if file_type.is_file() {
            fs::copy(&source_path, &target_path).map_err(|error| {
                DbError::internal(format!(
                    "failed to copy catalog replication file {} to {}: {error}",
                    source_path.display(),
                    target_path.display()
                ))
            })?;
            sync_file(&target_path)?;
        }
    }

    sync_dir(target)
}

fn collect_relative_files(root: &Path, current: &Path) -> DbResult<Vec<String>> {
    collect_relative_files_at_depth(root, current, 0)
}

fn collect_relative_files_at_depth(
    root: &Path,
    current: &Path,
    depth: usize,
) -> DbResult<Vec<String>> {
    if depth >= MAX_CATALOG_REPLICATION_DIR_DEPTH {
        return Err(DbError::program_limit(format!(
            "catalog replication manifest directory depth exceeds limit {MAX_CATALOG_REPLICATION_DIR_DEPTH}"
        )));
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(current).map_err(|error| {
        DbError::internal(format!(
            "failed to enumerate catalog replication seed state {}: {error}",
            current.display()
        ))
    })? {
        let entry = entry.map_err(|error| {
            DbError::internal(format!(
                "failed to enumerate catalog replication seed state {}: {error}",
                current.display()
            ))
        })?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|error| {
            DbError::internal(format!(
                "failed to stat catalog replication seed path {}: {error}",
                path.display()
            ))
        })?;

        if file_type.is_symlink() {
            return Err(DbError::internal(format!(
                "refusing to follow symlink during catalog replication manifest: {}",
                path.display()
            )));
        }

        if file_type.is_dir() {
            files.extend(collect_relative_files_at_depth(root, &path, depth + 1)?);
        } else if file_type.is_file() {
            let relative = path.strip_prefix(root).map_err(|error| {
                DbError::internal(format!(
                    "failed to relativize catalog replication seed path {} against {}: {error}",
                    path.display(),
                    root.display()
                ))
            })?;
            files.push(relative_to_manifest_path(relative));
        }
    }
    files.sort();
    Ok(files)
}

fn write_manifest(seed_dir: &Path, manifest: &CatalogReplicationSeedManifest) -> DbResult<()> {
    let manifest_path = seed_dir.join(CATALOG_REPLICATION_MANIFEST_FILENAME);
    let temp_path = seed_dir.join(format!("{CATALOG_REPLICATION_MANIFEST_FILENAME}.tmp"));
    let manifest_bytes = serde_json::to_vec_pretty(manifest).map_err(|error| {
        DbError::internal(format!(
            "failed to encode catalog replication seed manifest {}: {error}",
            manifest_path.display()
        ))
    })?;
    let mut file = create_tmp_file(&temp_path, "catalog replication seed manifest")?;
    file.write_all(&manifest_bytes).map_err(|error| {
        DbError::internal(format!(
            "failed to write catalog replication seed manifest temp file {}: {error}",
            temp_path.display()
        ))
    })?;
    file.flush().map_err(|error| {
        DbError::internal(format!(
            "failed to flush catalog replication seed manifest temp file {}: {error}",
            temp_path.display()
        ))
    })?;
    file.sync_all().map_err(|error| {
        DbError::internal(format!(
            "failed to sync catalog replication seed manifest temp file {}: {error}",
            temp_path.display()
        ))
    })?;
    drop(file);
    fs::rename(&temp_path, &manifest_path).map_err(|error| {
        DbError::internal(format!(
            "failed to publish catalog replication seed manifest {}: {error}",
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
            "failed to sync catalog replication seed directory {}: {error}",
            path.display()
        ))
    })
}

fn sync_file(path: &Path) -> DbResult<()> {
    fs::File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| {
            DbError::internal(format!(
                "failed to sync catalog replication seed file {}: {error}",
                path.display()
            ))
        })
}

fn read_manifest(seed_dir: &Path) -> DbResult<CatalogReplicationSeedManifest> {
    let manifest_path = seed_dir.join(CATALOG_REPLICATION_MANIFEST_FILENAME);
    let manifest_bytes = read_file_capped(
        &manifest_path,
        "catalog replication seed manifest",
        MAX_CATALOG_REPLICATION_MANIFEST_BYTES,
    )?;
    serde_json::from_slice(&manifest_bytes).map_err(|error| {
        DbError::internal(format!(
            "failed to decode catalog replication seed manifest {}: {error}",
            manifest_path.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_catalog::{CatalogWriter, RoleDescriptor};
    use aiondb_core::TxnId;
    use aiondb_wal::WalConfig;
    use std::sync::mpsc;
    use std::sync::Arc;

    #[test]
    fn catalog_replication_seed_round_trip_recovers_catalog_wal_state() {
        let wal_dir = crate::test_support::unique_temp_path("replication-test", "source");
        let seed_dir = crate::test_support::unique_temp_path("replication-test", "seed");
        let replica_dir = crate::test_support::unique_temp_path("replication-test", "replica");
        let wal = Arc::new(
            crate::CatalogWalHandle::open(WalConfig {
                dir: wal_dir.clone(),
                wal_lsn_mode: aiondb_wal::WalLsnMode::Logical,
                ..WalConfig::default()
            })
            .expect("catalog wal should open"),
        );
        let catalog = CatalogStore::new_with_wal(wal);
        catalog
            .create_role(
                TxnId::default(),
                RoleDescriptor {
                    name: "replica_user".to_string(),
                    login: true,
                    superuser: false,
                    password_hash: None,
                    ..RoleDescriptor::default()
                },
            )
            .expect("role should be created");

        let manifest = catalog
            .export_replication_seed(&seed_dir)
            .expect("catalog replication seed export should succeed");
        assert!(manifest.files.iter().any(|path| {
            std::path::Path::new(path)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("log"))
        }));
        assert!(manifest.files.iter().any(|path| path == "catalog.snapshot"));

        install_replication_seed(&seed_dir, &replica_dir)
            .expect("catalog replication seed install should succeed");
        let recovered = crate::recovery::recover_catalog_state(&replica_dir)
            .expect("catalog recovery should succeed");
        assert!(recovered.roles.contains_key("replica_user"));

        let _ = fs::remove_dir_all(&wal_dir);
        let _ = fs::remove_dir_all(&seed_dir);
        let _ = fs::remove_dir_all(&replica_dir);
    }

    #[test]
    fn catalog_replication_seed_export_does_not_publish_partial_root() {
        let wal_dir =
            crate::test_support::unique_temp_path("replication-test", "source-atomic-export");
        let seed_dir =
            crate::test_support::unique_temp_path("replication-test", "seed-atomic-export");
        let wal = Arc::new(
            crate::CatalogWalHandle::open(WalConfig {
                dir: wal_dir.clone(),
                wal_lsn_mode: aiondb_wal::WalLsnMode::Logical,
                ..WalConfig::default()
            })
            .expect("catalog wal should open"),
        );
        let catalog = Arc::new(CatalogStore::new_with_wal(wal));
        catalog
            .create_role(
                TxnId::default(),
                RoleDescriptor {
                    name: "paused_export".to_string(),
                    login: true,
                    superuser: false,
                    password_hash: None,
                    ..RoleDescriptor::default()
                },
            )
            .expect("role should be created");

        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (resume_tx, resume_rx) = mpsc::sync_channel(1);
        install_export_test_hook(seed_dir.clone(), entered_tx, resume_rx);

        let export_catalog = Arc::clone(&catalog);
        let export_seed = seed_dir.clone();
        let export_thread =
            std::thread::spawn(move || export_catalog.export_replication_seed(&export_seed));

        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("export should pause before publish");
        assert!(
            !seed_dir.exists(),
            "catalog seed export must not expose the final root before the atomic rename"
        );

        resume_tx
            .send(())
            .expect("export thread should still be waiting");
        let manifest = export_thread
            .join()
            .expect("export thread should not panic")
            .expect("catalog replication seed export should succeed");
        assert!(manifest.files.iter().any(|path| path == "catalog.snapshot"));
        assert!(
            seed_dir.is_dir(),
            "seed dir should become visible after publish"
        );

        let _ = fs::remove_dir_all(&wal_dir);
        let _ = fs::remove_dir_all(&seed_dir);
    }

    #[test]
    fn catalog_replication_seed_install_rejects_unknown_manifest_version() {
        let seed_dir = crate::test_support::unique_temp_path("replication-test", "seed-version");
        let target_dir =
            crate::test_support::unique_temp_path("replication-test", "target-version");
        fs::create_dir_all(seed_dir.join(CATALOG_REPLICATION_STATE_DIRNAME))
            .expect("create seed state dir");
        fs::write(
            seed_dir.join(CATALOG_REPLICATION_MANIFEST_FILENAME),
            serde_json::to_vec_pretty(&CatalogReplicationSeedManifest {
                version: CATALOG_REPLICATION_SEED_VERSION + 1,
                state_dir: CATALOG_REPLICATION_STATE_DIRNAME.to_owned(),
                files: Vec::new(),
            })
            .expect("encode manifest"),
        )
        .expect("write manifest");

        let err = install_replication_seed(&seed_dir, &target_dir)
            .expect_err("unknown catalog seed versions must fail");
        assert!(err.to_string().contains("version"));

        let _ = fs::remove_dir_all(&seed_dir);
        let _ = fs::remove_dir_all(&target_dir);
    }

    #[test]
    fn catalog_replication_seed_install_rejects_manifest_file_mismatch() {
        let seed_dir =
            crate::test_support::unique_temp_path("replication-test", "seed-file-mismatch");
        let target_dir =
            crate::test_support::unique_temp_path("replication-test", "target-file-mismatch");
        let state_dir = seed_dir.join(CATALOG_REPLICATION_STATE_DIRNAME);
        fs::create_dir_all(&state_dir).expect("create seed state dir");
        fs::write(state_dir.join("extra.wal"), b"extra").expect("write undeclared seed file");
        fs::write(
            seed_dir.join(CATALOG_REPLICATION_MANIFEST_FILENAME),
            serde_json::to_vec_pretty(&CatalogReplicationSeedManifest {
                version: CATALOG_REPLICATION_SEED_VERSION,
                state_dir: CATALOG_REPLICATION_STATE_DIRNAME.to_owned(),
                files: Vec::new(),
            })
            .expect("encode manifest"),
        )
        .expect("write manifest");

        let err = install_replication_seed(&seed_dir, &target_dir)
            .expect_err("manifest file mismatch must fail");
        assert!(err.to_string().contains("file list"));
        assert!(!target_dir.exists());

        let _ = fs::remove_dir_all(&seed_dir);
        let _ = fs::remove_dir_all(&target_dir);
    }
}
