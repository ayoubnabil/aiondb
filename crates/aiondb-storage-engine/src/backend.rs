use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
};

use aiondb_core::{ColumnId, DbError, DbResult, IndexId, RelationId, Row, TupleId, TxnId, Value};
use aiondb_storage_api::{
    CheckpointInfo, IndexStorageDescriptor, KeyRange, StorageCapabilities, StorageDDL, StorageDML,
    StorageTxnParticipant, TableStorageDescriptor, TupleStream,
};
use aiondb_tx::Snapshot;
use aiondb_wal::replication::{ReplicaRegistry, WalNotifier};
use aiondb_wal::{Lsn, WalLsnMode};

use crate::{
    engine::{
        install_snapshot_file_for_recovery, recover_disk_checkpoint_snapshot_bytes,
        snapshot_file_state,
    },
    layout::{latest_snapshot_bytes, prepare_lsm_layout, record_lsm_checkpoint, LsmLayout},
    replication::{export_replication_seed_from_root, StorageReplicationSeedManifest},
    InMemoryStorage, StorageBufferPoolConfig, StorageOptions, WalCommitPolicy,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StorageBackendKind {
    InMemory,
    Durable,
    Disk,
    PageEngine,
    Lsm,
}

impl StorageBackendKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InMemory => "in_memory",
            Self::Durable => "durable",
            Self::Disk => "disk",
            Self::PageEngine => "page_engine",
            Self::Lsm => "lsm",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiskSyncPolicy {
    Always,
    Every(u32),
    Never,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiskBackendConfig {
    pub path: PathBuf,
    pub sync_policy: DiskSyncPolicy,
    pub wal_group_commit_delay_micros: u64,
    pub index_shards: usize,
    pub commit_stripes: usize,
    pub buffer_pool: StorageBufferPoolConfig,
    pub max_open_files: usize,
}

impl DiskBackendConfig {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            sync_policy: DiskSyncPolicy::Always,
            wal_group_commit_delay_micros: 0,
            index_shards: 256,
            commit_stripes: 2048,
            buffer_pool: StorageBufferPoolConfig::default(),
            max_open_files: usize::MAX,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PageSyncPolicy {
    Always,
    Every(u32),
    Never,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PageEngineBackendConfig {
    pub base_path: PathBuf,
    pub page_size: usize,
    pub buffer_pool_pages: usize,
    pub sync_policy: PageSyncPolicy,
    pub wal_group_commit_delay_micros: u64,
}

impl PageEngineBackendConfig {
    #[must_use]
    pub fn new(base_path: impl Into<PathBuf>) -> Self {
        Self {
            base_path: base_path.into(),
            page_size: aiondb_buffer_pool::PAGE_SIZE,
            buffer_pool_pages: 320,
            sync_policy: PageSyncPolicy::Always,
            wal_group_commit_delay_micros: 0,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LsmBackendConfig {
    pub base_dir: PathBuf,
    pub memtable_flush_bytes: usize,
    pub block_size_bytes: usize,
    pub wal_group_commit_delay_micros: u64,
}

impl LsmBackendConfig {
    #[must_use]
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
            memtable_flush_bytes: 4 * 1024 * 1024,
            block_size_bytes: aiondb_buffer_pool::PAGE_SIZE,
            wal_group_commit_delay_micros: 0,
        }
    }
}

#[derive(Clone, Debug)]
pub enum StorageBackendSpec {
    InMemory { memory_limit_bytes: Option<u64> },
    Durable { options: StorageOptions },
    Disk { config: DiskBackendConfig },
    PageEngine { config: PageEngineBackendConfig },
    Lsm { config: LsmBackendConfig },
}

impl StorageBackendSpec {
    #[must_use]
    pub const fn kind(&self) -> StorageBackendKind {
        match self {
            Self::InMemory { .. } => StorageBackendKind::InMemory,
            Self::Durable { .. } => StorageBackendKind::Durable,
            Self::Disk { .. } => StorageBackendKind::Disk,
            Self::PageEngine { .. } => StorageBackendKind::PageEngine,
            Self::Lsm { .. } => StorageBackendKind::Lsm,
        }
    }

    #[must_use]
    pub const fn in_memory() -> Self {
        Self::InMemory {
            memory_limit_bytes: None,
        }
    }

    #[must_use]
    pub fn durable(options: StorageOptions) -> Self {
        Self::Durable { options }
    }
}

#[derive(Debug)]
pub enum StorageBackendHandle {
    InMemory(InMemoryStorage),
    Durable(InMemoryStorage),
    Disk(DiskBackend),
    PageEngine(InMemoryStorage),
    Lsm(LsmBackend),
}

#[doc(hidden)]
#[derive(Debug)]
pub struct LsmBackend {
    storage: InMemoryStorage,
    checkpoint_lock: Mutex<LsmLayout>,
}

#[doc(hidden)]
#[derive(Debug)]
pub struct DiskBackend {
    storage: InMemoryStorage,
    layout: DiskLayout,
}

#[derive(Clone, Debug)]
#[allow(clippy::struct_field_names)]
struct DiskLayout {
    base_dir: PathBuf,
    wal_dir: PathBuf,
    checkpoints_dir: PathBuf,
}

const LEGACY_MIGRATION_MARKER_FILENAME: &str = ".legacy_migrated";

impl StorageBackendHandle {
    #[must_use]
    pub fn open_in_memory(memory_limit_bytes: Option<u64>) -> Self {
        Self::InMemory(InMemoryStorage::new_without_wal_with_memory_limit(
            memory_limit_bytes,
        ))
    }

    pub fn open(spec: StorageBackendSpec) -> DbResult<Self> {
        match spec {
            StorageBackendSpec::InMemory { memory_limit_bytes } => {
                Ok(Self::open_in_memory(memory_limit_bytes))
            }
            StorageBackendSpec::Durable { options } => {
                Ok(Self::Durable(InMemoryStorage::new(options)?))
            }
            StorageBackendSpec::Disk { config } => Ok(Self::Disk(DiskBackend::open(&config)?)),
            StorageBackendSpec::PageEngine { config } => Ok(Self::PageEngine(
                InMemoryStorage::new(page_engine_options(&config)?)?,
            )),
            StorageBackendSpec::Lsm { config } => Ok(Self::Lsm(LsmBackend::open(&config)?)),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> StorageBackendKind {
        match self {
            Self::InMemory(_) => StorageBackendKind::InMemory,
            Self::Durable(_) => StorageBackendKind::Durable,
            Self::Disk(_) => StorageBackendKind::Disk,
            Self::PageEngine(_) => StorageBackendKind::PageEngine,
            Self::Lsm(_) => StorageBackendKind::Lsm,
        }
    }

    fn inner(&self) -> &InMemoryStorage {
        match self {
            Self::InMemory(storage) | Self::Durable(storage) | Self::PageEngine(storage) => storage,
            Self::Disk(storage) => &storage.storage,
            Self::Lsm(storage) => &storage.storage,
        }
    }

    #[doc(hidden)]
    pub fn set_replication_export_barrier(&mut self, barrier: Arc<RwLock<()>>) {
        match self {
            Self::InMemory(storage) | Self::Durable(storage) | Self::PageEngine(storage) => {
                storage.set_replication_export_barrier(barrier);
            }
            Self::Disk(storage) => storage.storage.set_replication_export_barrier(barrier),
            Self::Lsm(storage) => storage.storage.set_replication_export_barrier(barrier),
        }
    }

    #[doc(hidden)]
    pub fn set_replica_registry(&mut self, registry: Arc<ReplicaRegistry>) {
        match self {
            Self::InMemory(storage) | Self::Durable(storage) | Self::PageEngine(storage) => {
                storage.set_replica_registry(Arc::clone(&registry));
            }
            Self::Disk(storage) => storage.storage.set_replica_registry(Arc::clone(&registry)),
            Self::Lsm(storage) => storage.storage.set_replica_registry(registry),
        }
    }

    #[doc(hidden)]
    pub fn set_min_wal_keep_segments(&mut self, min_wal_keep_segments: u32) {
        match self {
            Self::InMemory(storage) | Self::Durable(storage) | Self::PageEngine(storage) => {
                storage.set_min_wal_keep_segments(min_wal_keep_segments);
            }
            Self::Disk(storage) => storage
                .storage
                .set_min_wal_keep_segments(min_wal_keep_segments),
            Self::Lsm(storage) => storage
                .storage
                .set_min_wal_keep_segments(min_wal_keep_segments),
        }
    }

    #[doc(hidden)]
    pub fn set_write_concern(&self, concern_level: u32, timeout: std::time::Duration) {
        match self {
            Self::InMemory(storage) | Self::Durable(storage) | Self::PageEngine(storage) => {
                storage.set_write_concern(concern_level, timeout);
            }
            Self::Disk(storage) => storage.storage.set_write_concern(concern_level, timeout),
            Self::Lsm(storage) => storage.storage.set_write_concern(concern_level, timeout),
        }
    }

    #[doc(hidden)]
    pub fn set_gpu_distance_computer(
        &self,
        computer: std::sync::Arc<dyn aiondb_gpu::BatchDistanceComputer>,
    ) {
        match self {
            Self::InMemory(storage) | Self::Durable(storage) | Self::PageEngine(storage) => {
                storage.set_gpu_distance_computer(computer);
            }
            Self::Disk(storage) => storage.storage.set_gpu_distance_computer(computer),
            Self::Lsm(storage) => storage.storage.set_gpu_distance_computer(computer),
        }
    }

    #[doc(hidden)]
    pub fn set_wal_notifier(&mut self, notifier: Arc<WalNotifier>) -> DbResult<()> {
        match self {
            Self::InMemory(storage) | Self::Durable(storage) | Self::PageEngine(storage) => {
                storage.set_wal_notifier(Arc::clone(&notifier))
            }
            Self::Disk(storage) => storage.storage.set_wal_notifier(Arc::clone(&notifier)),
            Self::Lsm(storage) => storage.storage.set_wal_notifier(notifier),
        }
    }

    #[doc(hidden)]
    pub fn current_wal_end_lsn(&self) -> DbResult<Option<Lsn>> {
        match self {
            Self::InMemory(storage) | Self::Durable(storage) | Self::PageEngine(storage) => {
                storage.current_wal_end_lsn()
            }
            Self::Disk(storage) => storage.storage.current_wal_end_lsn(),
            Self::Lsm(storage) => storage.storage.current_wal_end_lsn(),
        }
    }

    pub fn export_replication_seed(
        &self,
        seed_dir: impl AsRef<Path>,
    ) -> DbResult<StorageReplicationSeedManifest> {
        let seed_dir = seed_dir.as_ref();
        match self {
            Self::InMemory(_) => self.export_replication_seed_locked(seed_dir),
            _ => self
                .inner()
                .with_replication_export_lock(|| self.export_replication_seed_locked(seed_dir)),
        }
    }

    #[doc(hidden)]
    pub fn export_replication_seed_locked(
        &self,
        seed_dir: &Path,
    ) -> DbResult<StorageReplicationSeedManifest> {
        match self {
            Self::InMemory(_) => Err(DbError::feature_not_supported(
                "replication seed export is not supported for in-memory storage",
            )),
            Self::Durable(storage) => export_replication_seed_from_root(
                StorageBackendKind::Durable,
                &storage.prepare_replication_seed_export_locked()?,
                seed_dir,
            ),
            Self::Disk(storage) => storage.export_replication_seed_locked(seed_dir),
            Self::PageEngine(storage) => export_replication_seed_from_root(
                StorageBackendKind::PageEngine,
                &storage.prepare_replication_seed_export_locked()?,
                seed_dir,
            ),
            Self::Lsm(storage) => storage.export_replication_seed_locked(seed_dir),
        }
    }
}

impl DiskLayout {
    fn new(base_dir: &Path) -> Self {
        Self {
            base_dir: base_dir.to_path_buf(),
            wal_dir: base_dir.join("wal"),
            checkpoints_dir: base_dir.join("checkpoints"),
        }
    }

    fn wal_pages_dir(&self) -> PathBuf {
        self.wal_dir.join("pages")
    }

    fn wal_table_pages_dir(&self) -> PathBuf {
        self.wal_dir.join("table_pages")
    }

    fn checkpoint_pages_dir(&self) -> PathBuf {
        self.checkpoints_dir.join("pages")
    }

    fn checkpoint_table_pages_dir(&self) -> PathBuf {
        self.checkpoints_dir.join("table_pages")
    }
}

impl DiskBackend {
    fn open(config: &DiskBackendConfig) -> DbResult<Self> {
        validate_disk_config(config)?;
        let layout = prepare_disk_layout(&config.path)?;
        migrate_legacy_disk_paged_state(&layout)?;
        restore_snapshot_for_disk_recovery(&layout, config)?;
        Ok(Self {
            storage: InMemoryStorage::new(disk_options(config, &layout)?)?,
            layout,
        })
    }

    fn checkpoint(&self) -> DbResult<CheckpointInfo> {
        self.storage.checkpoint()
    }

    fn export_replication_seed_locked(
        &self,
        seed_dir: &Path,
    ) -> DbResult<StorageReplicationSeedManifest> {
        self.storage.prepare_replication_seed_export_locked()?;
        export_replication_seed_from_root(StorageBackendKind::Disk, &self.layout.base_dir, seed_dir)
    }
}

impl LsmBackend {
    fn open(config: &LsmBackendConfig) -> DbResult<Self> {
        validate_lsm_config(config)?;
        let layout = prepare_lsm_layout(
            &config.base_dir,
            config.memtable_flush_bytes,
            config.block_size_bytes,
        )?;
        restore_snapshot_for_lsm_recovery(&layout)?;
        let mut options = StorageOptions::durable(aiondb_wal::WalConfig {
            dir: layout.wal_dir.clone(),
            group_commit_delay_micros: config.wal_group_commit_delay_micros,
            ..aiondb_wal::WalConfig::default()
        });
        options.buffer_pool = StorageBufferPoolConfig::default();
        Ok(Self {
            storage: InMemoryStorage::new(options)?,
            checkpoint_lock: Mutex::new(layout),
        })
    }

    fn checkpoint(&self) -> DbResult<CheckpointInfo> {
        let (checkpoint, snapshot_bytes) = self.storage.checkpoint_with_snapshot_bytes()?;
        let layout = self.checkpoint_lock.lock().map_err(|e| {
            DbError::internal(format!(
                "lsm backend checkpoint metadata lock poisoned: {e}"
            ))
        })?;
        record_lsm_checkpoint(&layout, &checkpoint, &snapshot_bytes)?;
        Ok(checkpoint)
    }

    fn export_replication_seed_locked(
        &self,
        seed_dir: &Path,
    ) -> DbResult<StorageReplicationSeedManifest> {
        self.storage.prepare_replication_seed_export_locked()?;
        let base_dir = self
            .checkpoint_lock
            .lock()
            .map_err(|e| {
                DbError::internal(format!(
                    "lsm backend checkpoint metadata lock poisoned: {e}"
                ))
            })?
            .base_dir
            .clone();
        export_replication_seed_from_root(StorageBackendKind::Lsm, &base_dir, seed_dir)
    }
}

fn restore_snapshot_for_lsm_recovery(layout: &LsmLayout) -> DbResult<()> {
    if snapshot_file_state(&layout.wal_dir).is_ok_and(|state| state.is_some()) {
        return Ok(());
    }

    let Some(snapshot_bytes) = latest_snapshot_bytes(layout)? else {
        return Ok(());
    };
    install_snapshot_file_for_recovery(&layout.wal_dir, &snapshot_bytes)
}

fn prepare_disk_layout(base_dir: &Path) -> DbResult<DiskLayout> {
    let layout = DiskLayout::new(base_dir);
    ensure_durable_dir(&layout.base_dir, "disk backend base directory")?;
    ensure_durable_dir(
        &layout.checkpoints_dir,
        "disk backend checkpoints directory",
    )?;
    Ok(layout)
}

fn restore_snapshot_for_disk_recovery(
    layout: &DiskLayout,
    config: &DiskBackendConfig,
) -> DbResult<()> {
    if snapshot_file_state(&layout.wal_dir).is_ok_and(|state| state.is_some()) {
        return Ok(());
    }

    let snapshot_bytes = recover_disk_checkpoint_snapshot_bytes(
        &layout.checkpoints_dir,
        config.buffer_pool.snapshot_frames,
        config.max_open_files,
    )?;
    let Some(snapshot_bytes) = snapshot_bytes else {
        return Ok(());
    };
    install_snapshot_file_for_recovery(&layout.wal_dir, &snapshot_bytes)
}

fn migrate_legacy_disk_paged_state(layout: &DiskLayout) -> DbResult<()> {
    migrate_legacy_disk_dir(
        &layout.wal_pages_dir(),
        &layout.checkpoint_pages_dir(),
        "paged snapshot",
    )?;
    migrate_legacy_disk_dir(
        &layout.wal_table_pages_dir(),
        &layout.checkpoint_table_pages_dir(),
        "paged tables",
    )?;
    Ok(())
}

fn migrate_legacy_disk_dir(source: &Path, target: &Path, label: &str) -> DbResult<()> {
    if !source.is_dir() {
        return Ok(());
    }
    if target.exists() && !target.is_dir() {
        return Err(DbError::internal(format!(
            "disk backend legacy migration target {} exists and is not a directory",
            target.display()
        )));
    }

    let context = format!("disk backend legacy migration for {label}");
    let marker_path = legacy_migration_marker_path(target);
    if target.is_dir() && marker_path.is_file() {
        return Ok(());
    }

    let staging_dir = legacy_migration_staging_dir(target);
    if staging_dir.exists() {
        fs::remove_dir_all(&staging_dir).map_err(|error| {
            DbError::internal(format!(
                "failed to clean stale {context} staging directory {}: {error}",
                staging_dir.display()
            ))
        })?;
        sync_parent_dir(&staging_dir)?;
    }
    if target.is_dir() {
        fs::remove_dir_all(target).map_err(|error| {
            DbError::internal(format!(
                "failed to replace incomplete {context} target {}: {error}",
                target.display()
            ))
        })?;
        sync_parent_dir(target)?;
    }

    copy_dir_tree_durable(source, &staging_dir, &context)?;
    write_legacy_migration_marker(&staging_dir, &context)?;
    fs::rename(&staging_dir, target).map_err(|error| {
        DbError::internal(format!(
            "failed to finalize {context} into {}: {error}",
            target.display()
        ))
    })?;
    sync_parent_dir(target)?;
    Ok(())
}

fn copy_dir_tree_durable(source: &Path, target: &Path, context: &str) -> DbResult<()> {
    ensure_durable_dir(target, context)?;

    for entry in fs::read_dir(source).map_err(|error| {
        DbError::internal(format!(
            "failed to enumerate {context} source {}: {error}",
            source.display()
        ))
    })? {
        let entry = entry.map_err(|error| {
            DbError::internal(format!(
                "failed to enumerate {context} source {}: {error}",
                source.display()
            ))
        })?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        let file_type = entry.file_type().map_err(|error| {
            DbError::internal(format!(
                "failed to stat {context} source {}: {error}",
                source_path.display()
            ))
        })?;

        // Reject symlinks to prevent path traversal attacks.
        if file_type.is_symlink() {
            return Err(DbError::internal(format!(
                "refusing to follow symlink during {context}: {}",
                source_path.display()
            )));
        }

        if file_type.is_dir() {
            copy_dir_tree_durable(&source_path, &target_path, context)?;
        } else if file_type.is_file() {
            fs::copy(&source_path, &target_path).map_err(|error| {
                DbError::internal(format!(
                    "failed to copy {context} file {} to {}: {error}",
                    source_path.display(),
                    target_path.display()
                ))
            })?;
            sync_file(&target_path, context)?;
        }
    }

    sync_dir(target, context)?;
    Ok(())
}

fn write_legacy_migration_marker(target: &Path, context: &str) -> DbResult<()> {
    let marker_path = legacy_migration_marker_path(target);
    let file = File::create(&marker_path).map_err(|error| {
        DbError::internal(format!(
            "failed to create {context} marker {}: {error}",
            marker_path.display()
        ))
    })?;
    file.sync_all().map_err(|error| {
        DbError::internal(format!(
            "failed to sync {context} marker {}: {error}",
            marker_path.display()
        ))
    })?;
    sync_dir(target, context)
}

fn legacy_migration_marker_path(target: &Path) -> PathBuf {
    target.join(LEGACY_MIGRATION_MARKER_FILENAME)
}

fn legacy_migration_staging_dir(target: &Path) -> PathBuf {
    let file_name = target.file_name().map_or_else(
        || "legacy_migration".to_string(),
        |name| format!("{}.migration.tmp", name.to_string_lossy()),
    );
    target.with_file_name(file_name)
}

fn ensure_durable_dir(path: &Path, context: &str) -> DbResult<()> {
    fs::create_dir_all(path).map_err(|error| {
        DbError::internal(format!(
            "failed to create {context} {}: {error}",
            path.display()
        ))
    })?;
    sync_dir(path, context)?;
    sync_parent_dir(path)?;
    Ok(())
}

fn sync_file(path: &Path, context: &str) -> DbResult<()> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| {
            DbError::internal(format!(
                "failed to sync {context} file {}: {error}",
                path.display()
            ))
        })
}

fn sync_dir(dir: &Path, context: &str) -> DbResult<()> {
    aiondb_core::bounded_io::sync_dir(dir).map_err(|error| {
        DbError::internal(format!(
            "failed to sync {context} directory {}: {error}",
            dir.display()
        ))
    })
}

fn sync_parent_dir(path: &Path) -> DbResult<()> {
    aiondb_core::bounded_io::sync_parent_dir(path).map_err(|error| {
        let parent = path.parent().unwrap_or(path);
        DbError::internal(format!(
            "failed to sync disk backend parent directory {}: {error}",
            parent.display()
        ))
    })
}

fn disk_options(config: &DiskBackendConfig, layout: &DiskLayout) -> DbResult<StorageOptions> {
    validate_disk_config(config)?;

    if config.index_shards != 256 {
        return Err(DbError::feature_not_supported(format!(
            "disk backend index_shards is not configurable yet (expected 256, got {})",
            config.index_shards
        )));
    }
    if config.commit_stripes != 2048 {
        return Err(DbError::feature_not_supported(format!(
            "disk backend commit_stripes is not configurable yet (expected 2048, got {})",
            config.commit_stripes
        )));
    }

    let wal_lsn_mode = existing_wal_lsn_mode(&layout.wal_dir)?.unwrap_or_default();
    let mut options = StorageOptions::durable(aiondb_wal::WalConfig {
        dir: layout.wal_dir.clone(),
        sync_on_flush: disk_sync_on_flush(config.sync_policy),
        group_commit_delay_micros: config.wal_group_commit_delay_micros,
        wal_lsn_mode,
        ..aiondb_wal::WalConfig::default()
    });
    options.wal_commit_policy = disk_commit_policy(config.sync_policy)?;
    options.buffer_pool = config.buffer_pool.clone();
    options.max_open_files = config.max_open_files;
    options.paged_root_dir = Some(layout.checkpoints_dir.clone());
    options.file_snapshot_mirror_dir = Some(layout.checkpoints_dir.clone());
    options.checkpoint_manifest_dir = Some(layout.checkpoints_dir.clone());
    Ok(options)
}

fn existing_wal_lsn_mode(wal_dir: &Path) -> DbResult<Option<WalLsnMode>> {
    if !wal_dir.is_dir() {
        return Ok(None);
    }
    let Some(first_segment) = aiondb_wal::segment::list_segments(wal_dir)?
        .into_iter()
        .next()
    else {
        return Ok(None);
    };
    let bytes = aiondb_wal::segment::read_segment_bytes_bounded(
        wal_dir,
        first_segment,
        aiondb_wal::WalConfig::default().segment_max_bytes,
        "detecting existing WAL LSN mode",
    )?;
    Ok(aiondb_wal::segment::inspect_segment_header(&bytes)?.lsn_mode)
}

fn validate_disk_config(config: &DiskBackendConfig) -> DbResult<()> {
    if matches!(config.sync_policy, DiskSyncPolicy::Every(0)) {
        return Err(DbError::internal(
            "disk backend sync policy Every(0) requires interval >= 1",
        ));
    }
    if config.max_open_files == 0 {
        return Err(DbError::internal(
            "disk backend max_open_files must be >= 1",
        ));
    }
    if config.buffer_pool.table_frames == 0 {
        return Err(DbError::internal(
            "disk backend table buffer pool must be >= 1 frame",
        ));
    }
    if config.buffer_pool.snapshot_frames == 0 {
        return Err(DbError::internal(
            "disk backend snapshot buffer pool must be >= 1 frame",
        ));
    }
    Ok(())
}

fn page_engine_options(config: &PageEngineBackendConfig) -> DbResult<StorageOptions> {
    if config.page_size != aiondb_buffer_pool::PAGE_SIZE {
        return Err(DbError::feature_not_supported(format!(
            "page_engine currently requires page_size={} bytes, got {}",
            aiondb_buffer_pool::PAGE_SIZE,
            config.page_size
        )));
    }
    if config.buffer_pool_pages == 0 {
        return Err(DbError::internal(
            "page_engine buffer_pool_pages must be >= 1",
        ));
    }

    let mut options = StorageOptions::durable(aiondb_wal::WalConfig {
        dir: config.base_path.clone(),
        sync_on_flush: page_sync_requires_fsync(config.sync_policy),
        group_commit_delay_micros: config.wal_group_commit_delay_micros,
        ..aiondb_wal::WalConfig::default()
    });
    options.wal_commit_policy = page_commit_policy(config.sync_policy);
    options.buffer_pool = split_page_engine_buffer_pool(config.buffer_pool_pages);
    Ok(options)
}

fn validate_lsm_config(config: &LsmBackendConfig) -> DbResult<()> {
    if config.memtable_flush_bytes == 0 {
        return Err(DbError::internal("lsm memtable_flush_bytes must be >= 1"));
    }
    if config.memtable_flush_bytes != 4 * 1024 * 1024 {
        return Err(DbError::feature_not_supported(format!(
            "lsm memtable_flush_bytes is not configurable yet (expected 4194304, got {})",
            config.memtable_flush_bytes
        )));
    }
    if config.block_size_bytes != aiondb_buffer_pool::PAGE_SIZE {
        return Err(DbError::feature_not_supported(format!(
            "lsm currently requires block_size_bytes={} bytes, got {}",
            aiondb_buffer_pool::PAGE_SIZE,
            config.block_size_bytes
        )));
    }
    Ok(())
}

fn split_page_engine_buffer_pool(buffer_pool_pages: usize) -> StorageBufferPoolConfig {
    let snapshot_frames = (buffer_pool_pages / 4).max(1);
    let table_frames = buffer_pool_pages.saturating_sub(snapshot_frames).max(1);

    StorageBufferPoolConfig {
        table_frames,
        snapshot_frames,
        index_frames: table_frames,
    }
}

fn disk_sync_on_flush(policy: DiskSyncPolicy) -> bool {
    matches!(policy, DiskSyncPolicy::Always)
}

fn disk_commit_policy(policy: DiskSyncPolicy) -> DbResult<WalCommitPolicy> {
    match policy {
        DiskSyncPolicy::Always => Ok(WalCommitPolicy::Always),
        DiskSyncPolicy::Never => Ok(WalCommitPolicy::Never),
        DiskSyncPolicy::Every(value) => {
            if value == 0 {
                return Err(DbError::internal(
                    "disk backend sync policy Every(0) requires interval >= 1",
                ));
            }
            Ok(WalCommitPolicy::Every(value))
        }
    }
}

fn page_sync_requires_fsync(policy: PageSyncPolicy) -> bool {
    match policy {
        PageSyncPolicy::Always | PageSyncPolicy::Every(_) => true,
        PageSyncPolicy::Never => false,
    }
}

fn page_commit_policy(policy: PageSyncPolicy) -> WalCommitPolicy {
    match policy {
        PageSyncPolicy::Always | PageSyncPolicy::Every(_) => WalCommitPolicy::Always,
        PageSyncPolicy::Never => WalCommitPolicy::Never,
    }
}

impl StorageCapabilities for StorageBackendHandle {
    fn supports_vector_search(&self) -> bool {
        self.inner().supports_vector_search()
    }

    fn supports_gin_search(&self) -> bool {
        self.inner().supports_gin_search()
    }

    fn supports_savepoints(&self) -> bool {
        self.inner().supports_savepoints()
    }

    fn supports_durability(&self) -> bool {
        self.inner().supports_durability()
    }

    fn supports_persistent_ordered_indexes(&self) -> bool {
        self.inner().supports_persistent_ordered_indexes()
    }

    fn supports_vacuum(&self) -> bool {
        self.inner().supports_vacuum()
    }

    fn supports_statistics_logging(&self) -> bool {
        self.inner().supports_statistics_logging()
    }
}

impl StorageDDL for StorageBackendHandle {
    fn create_table_storage(&self, txn: TxnId, table: &TableStorageDescriptor) -> DbResult<()> {
        self.inner().create_table_storage(txn, table)
    }

    fn create_index_storage(&self, txn: TxnId, index: &IndexStorageDescriptor) -> DbResult<()> {
        self.inner().create_index_storage(txn, index)
    }

    fn alter_table_storage(&self, txn: TxnId, table: &TableStorageDescriptor) -> DbResult<()> {
        self.inner().alter_table_storage(txn, table)
    }

    fn drop_table_storage(&self, txn: TxnId, table_id: RelationId) -> DbResult<()> {
        self.inner().drop_table_storage(txn, table_id)
    }

    fn drop_index_storage(&self, txn: TxnId, index_id: IndexId) -> DbResult<()> {
        self.inner().drop_index_storage(txn, index_id)
    }
}

impl StorageDML for StorageBackendHandle {
    fn cache_generation(&self) -> Option<u64> {
        self.inner().cache_generation()
    }

    fn graph_projection_cache_get(
        &self,
        namespace: &str,
        cache_key: &str,
        generation: u64,
    ) -> DbResult<Option<Vec<u8>>> {
        self.inner()
            .graph_projection_cache_get(namespace, cache_key, generation)
    }

    fn graph_projection_cache_put(
        &self,
        namespace: &str,
        cache_key: &str,
        generation: u64,
        payload: &[u8],
    ) -> DbResult<()> {
        self.inner()
            .graph_projection_cache_put(namespace, cache_key, generation, payload)
    }

    fn apply_replicated_wal_entry(&self, record_bytes: &[u8]) -> DbResult<()> {
        let (entry, _consumed) = aiondb_wal::codec::decode_entry(record_bytes)?;
        self.inner().apply_replicated_wal_entry(&entry)
    }

    fn scan_table(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        table_id: RelationId,
        projected_columns: Option<Vec<ColumnId>>,
    ) -> DbResult<Box<dyn TupleStream>> {
        self.inner()
            .scan_table(txn, snapshot, table_id, projected_columns)
    }

    fn scan_table_eq_filter(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        table_id: RelationId,
        filter_column: ColumnId,
        filter_value: &Value,
        projected_columns: Option<Vec<ColumnId>>,
    ) -> DbResult<Box<dyn TupleStream>> {
        self.inner().scan_table_eq_filter(
            txn,
            snapshot,
            table_id,
            filter_column,
            filter_value,
            projected_columns,
        )
    }

    fn scan_table_eq_filter_limited(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        table_id: RelationId,
        filter_column: ColumnId,
        filter_value: &Value,
        projected_columns: Option<Vec<ColumnId>>,
        max_matches: Option<u64>,
    ) -> DbResult<Box<dyn TupleStream>> {
        self.inner().scan_table_eq_filter_limited(
            txn,
            snapshot,
            table_id,
            filter_column,
            filter_value,
            projected_columns,
            max_matches,
        )
    }

    fn scan_table_in_filter(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        table_id: RelationId,
        filter_column: ColumnId,
        filter_values: &[Value],
        projected_columns: Option<Vec<ColumnId>>,
    ) -> DbResult<Box<dyn TupleStream>> {
        self.inner().scan_table_in_filter(
            txn,
            snapshot,
            table_id,
            filter_column,
            filter_values,
            projected_columns,
        )
    }

    fn scan_table_range_filter(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        table_id: RelationId,
        filter_column: ColumnId,
        lower: std::ops::Bound<Value>,
        upper: std::ops::Bound<Value>,
        projected_columns: Option<Vec<ColumnId>>,
    ) -> DbResult<Box<dyn TupleStream>> {
        self.inner().scan_table_range_filter(
            txn,
            snapshot,
            table_id,
            filter_column,
            lower,
            upper,
            projected_columns,
        )
    }

    fn scan_table_null_filter(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        table_id: RelationId,
        filter_column: ColumnId,
        is_not_null: bool,
        projected_columns: Option<Vec<ColumnId>>,
    ) -> DbResult<Box<dyn TupleStream>> {
        self.inner().scan_table_null_filter(
            txn,
            snapshot,
            table_id,
            filter_column,
            is_not_null,
            projected_columns,
        )
    }

    fn scan_table_multi_range_filter(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        table_id: RelationId,
        filters: &[(ColumnId, std::ops::Bound<Value>, std::ops::Bound<Value>)],
        projected_columns: Option<Vec<ColumnId>>,
    ) -> DbResult<Box<dyn TupleStream>> {
        self.inner().scan_table_multi_range_filter(
            txn,
            snapshot,
            table_id,
            filters,
            projected_columns,
        )
    }

    fn visible_row_count(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        table_id: RelationId,
    ) -> DbResult<u64> {
        self.inner().visible_row_count(txn, snapshot, table_id)
    }

    fn try_prove_filter_empty(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        table_id: RelationId,
        column_predicates: &[(ColumnId, std::ops::Bound<Value>, std::ops::Bound<Value>)],
    ) -> DbResult<Option<bool>> {
        self.inner()
            .try_prove_filter_empty(txn, snapshot, table_id, column_predicates)
    }

    fn visible_eq_row_count(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        table_id: RelationId,
        filter_column: ColumnId,
        filter_value: &Value,
    ) -> DbResult<u64> {
        self.inner()
            .visible_eq_row_count(txn, snapshot, table_id, filter_column, filter_value)
    }

    fn visible_index_row_count(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        index_id: IndexId,
        key_range: KeyRange,
    ) -> DbResult<u64> {
        self.inner()
            .visible_index_row_count(txn, snapshot, index_id, key_range)
    }

    fn index_candidate_tuple_ids(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        index_id: IndexId,
        key_range: KeyRange,
    ) -> DbResult<Vec<TupleId>> {
        self.inner()
            .index_candidate_tuple_ids(txn, snapshot, index_id, key_range)
    }

    fn visible_index_group_counts(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        index_id: IndexId,
        key_range: KeyRange,
    ) -> DbResult<Vec<(Value, u64)>> {
        self.inner()
            .visible_index_group_counts(txn, snapshot, index_id, key_range)
    }

    fn visible_index_group_count_rows(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        index_id: IndexId,
        key_range: KeyRange,
    ) -> DbResult<Vec<Row>> {
        self.inner()
            .visible_index_group_count_rows(txn, snapshot, index_id, key_range)
    }

    fn index_min_single_column_value(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        index_id: IndexId,
    ) -> DbResult<Option<Value>> {
        self.inner()
            .index_min_single_column_value(txn, snapshot, index_id)
    }

    fn index_max_single_column_value(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        index_id: IndexId,
    ) -> DbResult<Option<Value>> {
        self.inner()
            .index_max_single_column_value(txn, snapshot, index_id)
    }

    fn scan_index(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        index_id: IndexId,
        key_range: KeyRange,
        projected_columns: Option<Vec<ColumnId>>,
    ) -> DbResult<Box<dyn TupleStream>> {
        self.inner()
            .scan_index(txn, snapshot, index_id, key_range, projected_columns)
    }

    fn scan_index_limited(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        index_id: IndexId,
        key_range: KeyRange,
        projected_columns: Option<Vec<ColumnId>>,
        limit: usize,
    ) -> DbResult<Box<dyn TupleStream>> {
        self.inner().scan_index_limited(
            txn,
            snapshot,
            index_id,
            key_range,
            projected_columns,
            limit,
        )
    }

    fn scan_index_ordered(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        index_id: IndexId,
        key_range: KeyRange,
        projected_columns: Option<Vec<ColumnId>>,
        descending: bool,
    ) -> DbResult<Box<dyn TupleStream>> {
        self.inner().scan_index_ordered(
            txn,
            snapshot,
            index_id,
            key_range,
            projected_columns,
            descending,
        )
    }

    fn scan_index_ordered_limited(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        index_id: IndexId,
        key_range: KeyRange,
        projected_columns: Option<Vec<ColumnId>>,
        descending: bool,
        limit: usize,
    ) -> DbResult<Box<dyn TupleStream>> {
        self.inner().scan_index_ordered_limited(
            txn,
            snapshot,
            index_id,
            key_range,
            projected_columns,
            descending,
            limit,
        )
    }

    fn fetch(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        table_id: RelationId,
        tuple_id: TupleId,
        projected_columns: Option<Vec<ColumnId>>,
    ) -> DbResult<Option<Row>> {
        self.inner()
            .fetch(txn, snapshot, table_id, tuple_id, projected_columns)
    }

    fn fetch_ref(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        table_id: RelationId,
        tuple_id: TupleId,
        projected_columns: Option<&[ColumnId]>,
    ) -> DbResult<Option<Row>> {
        self.inner()
            .fetch_ref(txn, snapshot, table_id, tuple_id, projected_columns)
    }

    fn insert(&self, txn: TxnId, table_id: RelationId, row: Row) -> DbResult<TupleId> {
        self.inner().insert(txn, table_id, row)
    }

    fn insert_batch(
        &self,
        txn: TxnId,
        table_id: RelationId,
        rows: Vec<Row>,
    ) -> DbResult<Vec<TupleId>> {
        self.inner().insert_batch(txn, table_id, rows)
    }

    fn update(
        &self,
        txn: TxnId,
        table_id: RelationId,
        tuple_id: TupleId,
        row: Row,
    ) -> DbResult<TupleId> {
        self.inner().update(txn, table_id, tuple_id, row)
    }

    fn delete(&self, txn: TxnId, table_id: RelationId, tuple_id: TupleId) -> DbResult<()> {
        self.inner().delete(txn, table_id, tuple_id)
    }

    fn vacuum_table(&self, table_id: RelationId) -> DbResult<u64> {
        self.inner().vacuum_table(table_id)
    }

    fn vector_search(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        index_id: IndexId,
        query: &[f32],
        k: usize,
        ef: usize,
        tuple_id_filter: Option<&(dyn Fn(TupleId) -> bool + Send + Sync)>,
        max_search_duration: Option<std::time::Duration>,
        interrupt_checker: Option<&(dyn Fn() -> DbResult<()> + Send + Sync)>,
    ) -> DbResult<Box<dyn TupleStream>> {
        self.inner().vector_search(
            txn,
            snapshot,
            index_id,
            query,
            k,
            ef,
            tuple_id_filter,
            max_search_duration,
            interrupt_checker,
        )
    }

    fn gin_containment_search(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        index_id: IndexId,
        pattern: &serde_json::Value,
    ) -> DbResult<Box<dyn TupleStream>> {
        self.inner()
            .gin_containment_search(txn, snapshot, index_id, pattern)
    }

    fn gin_containment_search_limited(
        &self,
        txn: TxnId,
        snapshot: &Snapshot,
        index_id: IndexId,
        pattern: &serde_json::Value,
        visible_limit: usize,
    ) -> DbResult<Box<dyn TupleStream>> {
        self.inner()
            .gin_containment_search_limited(txn, snapshot, index_id, pattern, visible_limit)
    }

    fn log_analyze_stats(
        &self,
        table_id: RelationId,
        row_count: u64,
        total_bytes: u64,
        dead_row_count: u64,
        column_stats: Vec<(ColumnId, f64, f64, u32)>,
    ) -> DbResult<()> {
        self.inner().log_analyze_stats(
            table_id,
            row_count,
            total_bytes,
            dead_row_count,
            column_stats,
        )
    }

    fn register_edge_table(
        &self,
        table_id: RelationId,
        source_col_idx: usize,
        target_col_idx: usize,
    ) {
        StorageDML::register_edge_table(self.inner(), table_id, source_col_idx, target_col_idx);
    }

    fn unregister_edge_table(&self, table_id: RelationId) {
        StorageDML::unregister_edge_table(self.inner(), table_id);
    }

    fn adjacency_index_available(&self, txn: TxnId, edge_table_id: RelationId) -> bool {
        StorageDML::adjacency_index_available(self.inner(), txn, edge_table_id)
    }

    fn adjacency_index_stats(
        &self,
        txn: TxnId,
        edge_table_id: RelationId,
    ) -> Option<aiondb_graph_api::GraphStats> {
        StorageDML::adjacency_index_stats(self.inner(), txn, edge_table_id)
    }

    fn adjacency_index_has_edges(&self, txn: TxnId, edge_table_id: RelationId) -> bool {
        StorageDML::adjacency_index_has_edges(self.inner(), txn, edge_table_id)
    }

    fn adjacency_lookup(
        &self,
        txn: TxnId,
        snapshot: &aiondb_tx::Snapshot,
        edge_table_id: RelationId,
        node_id: &aiondb_core::Value,
        outgoing: bool,
    ) -> DbResult<Vec<TupleId>> {
        StorageDML::adjacency_lookup(
            self.inner(),
            txn,
            snapshot,
            edge_table_id,
            node_id,
            outgoing,
        )
    }

    fn adjacency_edge_cursor(
        &self,
        txn: TxnId,
        snapshot: &aiondb_tx::Snapshot,
        edge_table_id: RelationId,
        node_id: &Value,
        outgoing: bool,
    ) -> DbResult<Box<dyn aiondb_graph_api::NeighborCursor<TupleId> + '_>> {
        StorageDML::adjacency_edge_cursor(
            self.inner(),
            txn,
            snapshot,
            edge_table_id,
            node_id,
            outgoing,
        )
    }

    fn adjacency_neighbors(
        &self,
        txn: TxnId,
        snapshot: &aiondb_tx::Snapshot,
        edge_table_id: RelationId,
        node_id: &aiondb_core::Value,
        outgoing: bool,
    ) -> DbResult<Vec<aiondb_core::Value>> {
        StorageDML::adjacency_neighbors(
            self.inner(),
            txn,
            snapshot,
            edge_table_id,
            node_id,
            outgoing,
        )
    }

    fn adjacency_neighbor_cursor(
        &self,
        txn: TxnId,
        snapshot: &aiondb_tx::Snapshot,
        edge_table_id: RelationId,
        node_id: &Value,
        outgoing: bool,
    ) -> DbResult<Box<dyn aiondb_graph_api::NeighborCursor<Value> + '_>> {
        StorageDML::adjacency_neighbor_cursor(
            self.inner(),
            txn,
            snapshot,
            edge_table_id,
            node_id,
            outgoing,
        )
    }

    fn adjacency_edges(
        &self,
        txn: TxnId,
        snapshot: &aiondb_tx::Snapshot,
        edge_table_id: RelationId,
    ) -> DbResult<Vec<(TupleId, Value, Value)>> {
        StorageDML::adjacency_edges(self.inner(), txn, snapshot, edge_table_id)
    }

    fn adjacency_weighted_edges(
        &self,
        txn: TxnId,
        snapshot: &aiondb_tx::Snapshot,
        edge_table_id: RelationId,
        weight_column: ColumnId,
    ) -> DbResult<Vec<(TupleId, Value, Value, Value)>> {
        StorageDML::adjacency_weighted_edges(
            self.inner(),
            txn,
            snapshot,
            edge_table_id,
            weight_column,
        )
    }

    fn adjacency_edge_endpoints(
        &self,
        txn: TxnId,
        snapshot: &aiondb_tx::Snapshot,
        edge_table_id: RelationId,
        edge_tuple_id: TupleId,
    ) -> DbResult<Option<(Value, Value)>> {
        StorageDML::adjacency_edge_endpoints(
            self.inner(),
            txn,
            snapshot,
            edge_table_id,
            edge_tuple_id,
        )
    }
}

impl StorageTxnParticipant for StorageBackendHandle {
    fn begin_txn(&self, txn: TxnId, isolation: aiondb_tx::IsolationLevel) -> DbResult<()> {
        self.inner().begin_txn(txn, isolation)
    }

    fn validate_commit_txn(&self, txn: TxnId) -> DbResult<()> {
        self.inner().validate_commit_txn(txn)
    }

    fn commit_txn(&self, txn: TxnId, commit_ts: u64) -> DbResult<()> {
        self.inner().commit_txn(txn, commit_ts)
    }

    fn rollback_txn(&self, txn: TxnId) -> DbResult<()> {
        self.inner().rollback_txn(txn)
    }

    fn checkpoint(&self) -> DbResult<CheckpointInfo> {
        match self {
            Self::Disk(storage) => storage.checkpoint(),
            Self::Lsm(storage) => storage.checkpoint(),
            _ => self.inner().checkpoint(),
        }
    }

    fn create_savepoint(&self, txn: TxnId) -> DbResult<u64> {
        self.inner().create_savepoint(txn)
    }

    fn rollback_to_savepoint(&self, txn: TxnId, savepoint_id: u64) -> DbResult<()> {
        self.inner().rollback_to_savepoint(txn, savepoint_id)
    }

    fn release_savepoint(&self, txn: TxnId, savepoint_id: u64) -> DbResult<()> {
        self.inner().release_savepoint(txn, savepoint_id)
    }
}

#[cfg(test)]
#[path = "backend_tests.rs"]
mod tests;
