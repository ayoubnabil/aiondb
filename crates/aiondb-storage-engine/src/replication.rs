use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use aiondb_core::replication_fs::{
    ensure_target_absent_or_empty, read_file_capped, relative_to_manifest_path, staging_dir_path,
};
use aiondb_core::{DbError, DbResult};
use serde::{Deserialize, Serialize};

use crate::backend::StorageBackendKind;

const STORAGE_REPLICATION_SEED_VERSION: u64 = 1;
const STORAGE_REPLICATION_STATE_DIRNAME: &str = "state";
const STORAGE_REPLICATION_MANIFEST_FILENAME: &str = "manifest.json";
const MAX_STORAGE_REPLICATION_MANIFEST_BYTES: u64 = 32 * 1024 * 1024;

#[cfg(test)]
struct ExportTestHook {
    seed_dir: PathBuf,
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
    seed_dir: PathBuf,
    entered: std::sync::mpsc::SyncSender<()>,
    resume: std::sync::mpsc::Receiver<()>,
) {
    let mut hook = export_test_hook_cell().lock().expect("export hook lock");
    *hook = Some(ExportTestHook {
        seed_dir,
        entered,
        resume,
    });
}

#[cfg(test)]
fn maybe_pause_storage_seed_export(seed_dir: &Path) {
    let hook = {
        let mut slot = export_test_hook_cell().lock().expect("export hook lock");
        match slot.take() {
            Some(hook) if hook.seed_dir == seed_dir => Some(hook),
            Some(hook) => {
                *slot = Some(hook);
                None
            }
            None => None,
        }
    };
    if let Some(hook) = hook {
        let _ = hook.entered.send(());
        let _ = hook.resume.recv();
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StorageReplicationSeedManifest {
    pub version: u64,
    pub backend: String,
    pub state_dir: String,
    pub files: Vec<String>,
}

impl StorageReplicationSeedManifest {
    pub(crate) fn new(kind: StorageBackendKind, files: Vec<String>) -> Self {
        Self {
            version: STORAGE_REPLICATION_SEED_VERSION,
            backend: kind.as_str().to_string(),
            state_dir: STORAGE_REPLICATION_STATE_DIRNAME.to_string(),
            files,
        }
    }
}

pub(crate) fn export_replication_seed_from_root(
    kind: StorageBackendKind,
    source_root: &Path,
    seed_dir: &Path,
) -> DbResult<StorageReplicationSeedManifest> {
    export_seed_atomically(seed_dir, "replication seed export", |staging_root| {
        let state_dir = staging_root.join(STORAGE_REPLICATION_STATE_DIRNAME);
        copy_tree(source_root, &state_dir)?;

        let files = collect_relative_files(&state_dir, &state_dir)?;
        let manifest = StorageReplicationSeedManifest::new(kind, files);
        write_manifest(staging_root, &manifest)?;
        Ok(manifest)
    })
}

pub fn install_replication_seed(
    seed_dir: impl AsRef<Path>,
    target_root: impl AsRef<Path>,
) -> DbResult<StorageReplicationSeedManifest> {
    let seed_dir = seed_dir.as_ref();
    let target_root = target_root.as_ref();
    let manifest = read_manifest(seed_dir)?;
    if manifest.version != STORAGE_REPLICATION_SEED_VERSION {
        return Err(DbError::internal(format!(
            "replication seed manifest version {} is not supported (expected {STORAGE_REPLICATION_SEED_VERSION})",
            manifest.version
        )));
    }
    validate_manifest_dirname(&manifest.state_dir, "state_dir")?;
    let source_state_dir = seed_dir.join(&manifest.state_dir);
    if !source_state_dir.is_dir() {
        return Err(DbError::internal(format!(
            "replication seed state directory is missing: {}",
            source_state_dir.display()
        )));
    }
    let actual_files = collect_relative_files(&source_state_dir, &source_state_dir)?;
    if actual_files != manifest.files {
        return Err(DbError::internal(
            "replication seed manifest file list does not match seed contents",
        ));
    }

    install_tree_atomically(
        &source_state_dir,
        target_root,
        "replication seed install",
        |staging_root| rewrite_backend_metadata(&manifest, staging_root, target_root),
    )?;
    Ok(manifest)
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
            "replication manifest {field} contains invalid path component: {name}"
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

    if seed_dir.exists() {
        fs::remove_dir(seed_dir).map_err(|error| {
            DbError::internal(format!(
                "failed to clear empty {context} target {} before staging: {error}",
                seed_dir.display()
            ))
        })?;
        sync_dir(parent_dir)?;
    }

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
        maybe_pause_storage_seed_export(seed_dir);
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

fn install_tree_atomically(
    source_root: &Path,
    target_root: &Path,
    context: &str,
    finalize: impl FnOnce(&Path) -> DbResult<()>,
) -> DbResult<()> {
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
        finalize(&staging_root)?;
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

const MAX_REPLICATION_DIR_DEPTH: usize = 256;

fn copy_tree(source: &Path, target: &Path) -> DbResult<()> {
    copy_tree_at_depth(source, target, 0)
}

fn copy_tree_at_depth(source: &Path, target: &Path, depth: usize) -> DbResult<()> {
    if depth >= MAX_REPLICATION_DIR_DEPTH {
        return Err(DbError::program_limit(format!(
            "storage replication directory depth exceeds limit {MAX_REPLICATION_DIR_DEPTH}"
        )));
    }
    fs::create_dir_all(target).map_err(|error| {
        DbError::internal(format!(
            "failed to create replication copy target {}: {error}",
            target.display()
        ))
    })?;

    for entry in fs::read_dir(source).map_err(|error| {
        DbError::internal(format!(
            "failed to enumerate replication source {}: {error}",
            source.display()
        ))
    })? {
        let entry = entry.map_err(|error| {
            DbError::internal(format!(
                "failed to enumerate replication source {}: {error}",
                source.display()
            ))
        })?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        let file_type = entry.file_type().map_err(|error| {
            DbError::internal(format!(
                "failed to stat replication source {}: {error}",
                source_path.display()
            ))
        })?;

        // Reject symlinks to prevent path traversal attacks.
        if file_type.is_symlink() {
            return Err(DbError::internal(format!(
                "refusing to follow symlink during storage replication: {}",
                source_path.display()
            )));
        }

        if file_type.is_dir() {
            copy_tree_at_depth(&source_path, &target_path, depth + 1)?;
        } else if file_type.is_file() {
            fs::copy(&source_path, &target_path).map_err(|error| {
                DbError::internal(format!(
                    "failed to copy replication file {} to {}: {error}",
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
    if depth >= MAX_REPLICATION_DIR_DEPTH {
        return Err(DbError::program_limit(format!(
            "storage replication manifest directory depth exceeds limit {MAX_REPLICATION_DIR_DEPTH}"
        )));
    }
    let mut files = Vec::new();

    for entry in fs::read_dir(current).map_err(|error| {
        DbError::internal(format!(
            "failed to enumerate replication seed state {}: {error}",
            current.display()
        ))
    })? {
        let entry = entry.map_err(|error| {
            DbError::internal(format!(
                "failed to enumerate replication seed state {}: {error}",
                current.display()
            ))
        })?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|error| {
            DbError::internal(format!(
                "failed to stat replication seed path {}: {error}",
                path.display()
            ))
        })?;

        if file_type.is_symlink() {
            return Err(DbError::internal(format!(
                "refusing to follow symlink during storage replication manifest: {}",
                path.display()
            )));
        }

        if file_type.is_dir() {
            files.extend(collect_relative_files_at_depth(root, &path, depth + 1)?);
        } else if file_type.is_file() {
            let relative = path.strip_prefix(root).map_err(|error| {
                DbError::internal(format!(
                    "failed to relativize replication seed path {} against {}: {error}",
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

fn atomic_temp_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    path.with_file_name(format!(".{file_name}.tmp"))
}

fn write_file_atomically(path: &Path, bytes: &[u8], context: &str) -> DbResult<()> {
    let temp_path = atomic_temp_path(path);
    let mut file = create_temp_file(&temp_path, context)?;
    file.write_all(bytes).map_err(|error| {
        DbError::internal(format!(
            "failed to write {context} temp file {}: {error}",
            temp_path.display()
        ))
    })?;
    file.flush().map_err(|error| {
        DbError::internal(format!(
            "failed to flush {context} temp file {}: {error}",
            temp_path.display()
        ))
    })?;
    file.sync_all().map_err(|error| {
        DbError::internal(format!(
            "failed to sync {context} temp file {}: {error}",
            temp_path.display()
        ))
    })?;
    drop(file);
    fs::rename(&temp_path, path).map_err(|error| {
        DbError::internal(format!(
            "failed to publish {context} {}: {error}",
            path.display()
        ))
    })?;
    sync_parent_dir(path, context)
}

fn create_temp_file(temp_path: &Path, context: &str) -> DbResult<File> {
    if temp_path.exists() {
        fs::remove_file(temp_path).map_err(|error| {
            DbError::internal(format!(
                "failed to clear stale {context} temp file {}: {error}",
                temp_path.display()
            ))
        })?;
        sync_parent_dir(temp_path, context)?;
    }

    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temp_path)
        .map_err(|error| {
            DbError::internal(format!(
                "failed to create {context} temp file {}: {error}",
                temp_path.display()
            ))
        })
}

fn write_manifest(seed_dir: &Path, manifest: &StorageReplicationSeedManifest) -> DbResult<()> {
    let manifest_path = seed_dir.join(STORAGE_REPLICATION_MANIFEST_FILENAME);
    let manifest_bytes = serde_json::to_vec_pretty(manifest).map_err(|error| {
        DbError::internal(format!(
            "failed to encode replication seed manifest {}: {error}",
            manifest_path.display()
        ))
    })?;
    write_file_atomically(&manifest_path, &manifest_bytes, "replication seed manifest")
}

fn sync_dir(path: &Path) -> DbResult<()> {
    aiondb_core::bounded_io::sync_dir(path).map_err(|error| {
        DbError::internal(format!(
            "failed to sync replication seed directory {}: {error}",
            path.display()
        ))
    })
}

fn sync_parent_dir(path: &Path, context: &str) -> DbResult<()> {
    aiondb_core::bounded_io::sync_parent_dir(path).map_err(|error| {
        let parent = path.parent().unwrap_or(path);
        DbError::internal(format!(
            "failed to sync {context} parent directory {}: {error}",
            parent.display()
        ))
    })
}

fn sync_file(path: &Path) -> DbResult<()> {
    fs::File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| {
            DbError::internal(format!(
                "failed to sync replication seed file {}: {error}",
                path.display()
            ))
        })
}

fn read_manifest(seed_dir: &Path) -> DbResult<StorageReplicationSeedManifest> {
    let manifest_path = seed_dir.join(STORAGE_REPLICATION_MANIFEST_FILENAME);
    let manifest_bytes = read_file_capped(
        &manifest_path,
        "replication seed manifest",
        MAX_STORAGE_REPLICATION_MANIFEST_BYTES,
    )?;
    serde_json::from_slice(&manifest_bytes).map_err(|error| {
        DbError::internal(format!(
            "failed to decode replication seed manifest {}: {error}",
            manifest_path.display()
        ))
    })
}

fn rewrite_backend_metadata(
    manifest: &StorageReplicationSeedManifest,
    manifest_root: &Path,
    installed_root: &Path,
) -> DbResult<()> {
    if manifest.backend != StorageBackendKind::Lsm.as_str() {
        return Ok(());
    }

    let manifest_path = manifest_root.join("manifest.json");
    let manifest_bytes = read_file_capped(
        &manifest_path,
        "backend manifest",
        MAX_STORAGE_REPLICATION_MANIFEST_BYTES,
    )?;
    let mut value: serde_json::Value =
        serde_json::from_slice(&manifest_bytes).map_err(|error| {
            DbError::internal(format!(
                "failed to decode backend manifest {}: {error}",
                manifest_path.display()
            ))
        })?;
    let object = value.as_object_mut().ok_or_else(|| {
        DbError::internal(format!(
            "backend manifest {} must be a json object",
            manifest_path.display()
        ))
    })?;
    object.insert(
        "wal_dir".to_string(),
        serde_json::Value::String(installed_root.join("wal").display().to_string()),
    );
    object.insert(
        "levels_dir".to_string(),
        serde_json::Value::String(installed_root.join("levels").display().to_string()),
    );
    let rewritten = serde_json::to_vec_pretty(&value).map_err(|error| {
        DbError::internal(format!(
            "failed to encode rewritten backend manifest {}: {error}",
            manifest_path.display()
        ))
    })?;
    write_file_atomically(&manifest_path, &rewritten, "rewritten backend manifest")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::unique_temp_path;
    use std::sync::mpsc;

    #[test]
    fn export_and_install_replication_seed_round_trip_files() {
        let source_root = unique_temp_path("storage-replication", "source");
        fs::create_dir_all(source_root.join("nested")).expect("source dir should be creatable");
        fs::write(source_root.join("root.txt"), b"root").expect("root file should be writable");
        fs::write(source_root.join("nested").join("leaf.txt"), b"leaf")
            .expect("leaf file should be writable");

        let seed_dir = unique_temp_path("storage-replication", "seed");
        let manifest =
            export_replication_seed_from_root(StorageBackendKind::Disk, &source_root, &seed_dir)
                .expect("seed export should succeed");
        assert_eq!(manifest.backend, "disk");
        assert_eq!(
            manifest.files,
            vec!["nested/leaf.txt".to_string(), "root.txt".to_string()]
        );

        let target_root = unique_temp_path("storage-replication", "target");
        let installed =
            install_replication_seed(&seed_dir, &target_root).expect("seed install should succeed");
        assert_eq!(installed, manifest);
        assert_eq!(
            fs::read(target_root.join("root.txt")).expect("target root file should be readable"),
            b"root"
        );
        assert_eq!(
            fs::read(target_root.join("nested").join("leaf.txt"))
                .expect("target nested file should be readable"),
            b"leaf"
        );

        let _ = fs::remove_dir_all(&source_root);
        let _ = fs::remove_dir_all(&seed_dir);
        let _ = fs::remove_dir_all(&target_root);
    }

    #[test]
    fn install_replication_seed_is_atomic_on_rewrite_failure() {
        let seed_dir = unique_temp_path("storage-replication", "seed-atomic-rewrite-failure");
        let state_dir = seed_dir.join(STORAGE_REPLICATION_STATE_DIRNAME);
        fs::create_dir_all(&state_dir).expect("seed state dir should be creatable");
        fs::write(
            state_dir.join("manifest.json"),
            b"{ definitely not valid json",
        )
        .expect("backend manifest should be writable");

        let manifest = StorageReplicationSeedManifest {
            version: STORAGE_REPLICATION_SEED_VERSION,
            backend: StorageBackendKind::Lsm.as_str().to_string(),
            state_dir: STORAGE_REPLICATION_STATE_DIRNAME.to_string(),
            files: vec!["manifest.json".to_string()],
        };
        write_manifest(&seed_dir, &manifest).expect("seed manifest should be writable");

        let target_root = unique_temp_path("storage-replication", "target-atomic-rewrite-failure");
        let err = install_replication_seed(&seed_dir, &target_root)
            .expect_err("install should fail when backend metadata rewrite fails");
        assert!(err.to_string().contains("backend manifest"));
        assert!(
            !target_root.exists(),
            "failed install must not publish a partial target"
        );

        let _ = fs::remove_dir_all(&seed_dir);
        let _ = fs::remove_dir_all(&target_root);
    }

    #[test]
    fn install_replication_seed_rejects_unknown_manifest_version() {
        let seed_dir = unique_temp_path("storage-replication", "seed-version");
        fs::create_dir_all(seed_dir.join(STORAGE_REPLICATION_STATE_DIRNAME))
            .expect("seed state dir should be creatable");
        let manifest = StorageReplicationSeedManifest {
            version: STORAGE_REPLICATION_SEED_VERSION + 1,
            backend: StorageBackendKind::Disk.as_str().to_string(),
            state_dir: STORAGE_REPLICATION_STATE_DIRNAME.to_string(),
            files: Vec::new(),
        };
        write_manifest(&seed_dir, &manifest).expect("seed manifest should be writable");

        let target_root = unique_temp_path("storage-replication", "target-version");
        let err = install_replication_seed(&seed_dir, &target_root)
            .expect_err("unknown storage seed versions must fail");
        assert!(err.to_string().contains("version"));

        let _ = fs::remove_dir_all(&seed_dir);
        let _ = fs::remove_dir_all(&target_root);
    }

    #[test]
    fn write_manifest_replaces_stale_temp_file() {
        let seed_dir = unique_temp_path("storage-replication", "seed-stale-temp");
        fs::create_dir_all(&seed_dir).expect("seed dir should be creatable");
        let manifest_path = seed_dir.join(STORAGE_REPLICATION_MANIFEST_FILENAME);
        fs::write(atomic_temp_path(&manifest_path), b"stale temp")
            .expect("stale manifest temp should be writable");

        let manifest = StorageReplicationSeedManifest {
            version: STORAGE_REPLICATION_SEED_VERSION,
            backend: StorageBackendKind::Disk.as_str().to_string(),
            state_dir: STORAGE_REPLICATION_STATE_DIRNAME.to_string(),
            files: Vec::new(),
        };
        write_manifest(&seed_dir, &manifest).expect("manifest write should replace stale temp");

        assert!(
            manifest_path.is_file(),
            "manifest should be published after stale temp replacement"
        );
        assert!(
            !atomic_temp_path(&manifest_path).exists(),
            "temp file should not remain after publish"
        );

        let _ = fs::remove_dir_all(&seed_dir);
    }

    #[test]
    fn install_replication_seed_rejects_manifest_file_mismatch() {
        let seed_dir = unique_temp_path("storage-replication", "seed-file-mismatch");
        let state_dir = seed_dir.join(STORAGE_REPLICATION_STATE_DIRNAME);
        fs::create_dir_all(&state_dir).expect("seed state dir should be creatable");
        fs::write(state_dir.join("extra.txt"), b"extra").expect("extra seed file");
        let manifest = StorageReplicationSeedManifest {
            version: STORAGE_REPLICATION_SEED_VERSION,
            backend: StorageBackendKind::Disk.as_str().to_string(),
            state_dir: STORAGE_REPLICATION_STATE_DIRNAME.to_string(),
            files: Vec::new(),
        };
        write_manifest(&seed_dir, &manifest).expect("seed manifest should be writable");

        let target_root = unique_temp_path("storage-replication", "target-file-mismatch");
        let err = install_replication_seed(&seed_dir, &target_root)
            .expect_err("manifest file mismatch must fail");
        assert!(err.to_string().contains("file list"));
        assert!(!target_root.exists());

        let _ = fs::remove_dir_all(&seed_dir);
        let _ = fs::remove_dir_all(&target_root);
    }

    #[test]
    fn export_replication_seed_does_not_publish_partial_root() {
        let source_root = unique_temp_path("storage-replication", "source-atomic-export");
        fs::create_dir_all(source_root.join("nested")).expect("source dir should be creatable");
        fs::write(source_root.join("root.txt"), b"root").expect("root file should be writable");
        fs::write(source_root.join("nested").join("leaf.txt"), b"leaf")
            .expect("leaf file should be writable");

        let seed_dir = unique_temp_path("storage-replication", "seed-atomic-export");
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (resume_tx, resume_rx) = mpsc::sync_channel(1);
        install_export_test_hook(seed_dir.clone(), entered_tx, resume_rx);

        let export_source = source_root.clone();
        let export_seed = seed_dir.clone();
        let export_thread = std::thread::spawn(move || {
            export_replication_seed_from_root(
                StorageBackendKind::Disk,
                &export_source,
                &export_seed,
            )
        });

        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("export should pause before publish");
        assert!(
            !seed_dir.exists(),
            "seed export must not expose the final root before the atomic rename"
        );

        resume_tx
            .send(())
            .expect("export thread should still be waiting");
        let manifest = export_thread
            .join()
            .expect("export thread should not panic")
            .expect("seed export should succeed");
        assert_eq!(manifest.backend, StorageBackendKind::Disk.as_str());
        assert!(
            seed_dir.is_dir(),
            "seed dir should become visible after publish"
        );

        let _ = fs::remove_dir_all(&source_root);
        let _ = fs::remove_dir_all(&seed_dir);
    }
}
