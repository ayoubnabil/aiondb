use std::path::PathBuf;

pub const DEFAULT_STORAGE_DATA_DIR: &str = "./data";
pub const DEFAULT_SERVER_STORAGE_DATA_DIR: &str = "./data/aiondb";
pub const DEFAULT_STORAGE_PAGE_SIZE: usize = 8192;
pub const DEFAULT_STORAGE_MAX_OPEN_FILES: usize = 256;
pub const DEFAULT_STORAGE_TABLE_POOL_FRAMES: usize = 256;
pub const DEFAULT_STORAGE_SNAPSHOT_POOL_FRAMES: usize = 64;
pub const DEFAULT_STORAGE_BACKEND: StorageBackend = StorageBackend::Durable;
pub const DEFAULT_STORAGE_EVICTION_THRESHOLD_PERCENT: u8 = 70;
pub const DEFAULT_STORAGE_DURABLE_WAL_COMMIT_POLICY: DurableWalCommitPolicy =
    DurableWalCommitPolicy::Always;
pub const DEFAULT_STORAGE_WAL_GROUP_COMMIT_DELAY_MICROS: u64 = 0;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum StorageBackend {
    InMemory,
    #[default]
    Durable,
    Disk,
    PageEngine,
    Lsm,
}

impl StorageBackend {
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

    #[must_use]
    pub const fn is_persistent(self) -> bool {
        !matches!(self, Self::InMemory)
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "in_memory" | "in-memory" | "inmemory" | "memory" => Some(Self::InMemory),
            "durable" | "wal" => Some(Self::Durable),
            "disk" => Some(Self::Disk),
            "page_engine" | "page-engine" | "pageengine" => Some(Self::PageEngine),
            "lsm" => Some(Self::Lsm),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DurableWalCommitPolicy {
    Always,
    Every(u32),
    Never,
}

impl DurableWalCommitPolicy {
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let normalized = value.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "always" | "sync" | "fsync" => Some(Self::Always),
            "never" | "async" | "off" | "none" => Some(Self::Never),
            _ => {
                let interval = normalized
                    .strip_prefix("every:")
                    .or_else(|| normalized.strip_prefix("every="))
                    .unwrap_or(normalized.as_str());
                let interval = interval.parse::<u32>().ok()?;
                (interval > 0).then_some(Self::Every(interval))
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageConfig {
    pub backend: StorageBackend,
    pub data_dir: PathBuf,
    pub page_size: usize,
    pub max_open_files: usize,
    pub table_pool_frames: usize,
    pub snapshot_pool_frames: usize,
    /// Percentage of `memory_limit_bytes` at which proactive eviction of cold
    /// table data to the paged store begins. Valid range: 1..=99. Default: 70.
    pub eviction_threshold_percent: u8,
    /// WAL durability policy for the default durable backend. `Always` fsyncs
    /// every commit; `Every(N)` fsyncs one commit in N; `Never` flushes without
    /// fsync. Non-`Always` policies are intended for benchmarks/dev only.
    pub durable_wal_commit_policy: DurableWalCommitPolicy,
    /// Delay in microseconds used to batch concurrent WAL commits.
    /// `0` disables group-commit batching.
    pub wal_group_commit_delay_micros: u64,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            backend: DEFAULT_STORAGE_BACKEND,
            data_dir: PathBuf::from(DEFAULT_STORAGE_DATA_DIR),
            page_size: DEFAULT_STORAGE_PAGE_SIZE,
            max_open_files: DEFAULT_STORAGE_MAX_OPEN_FILES,
            table_pool_frames: DEFAULT_STORAGE_TABLE_POOL_FRAMES,
            snapshot_pool_frames: DEFAULT_STORAGE_SNAPSHOT_POOL_FRAMES,
            eviction_threshold_percent: DEFAULT_STORAGE_EVICTION_THRESHOLD_PERCENT,
            durable_wal_commit_policy: DEFAULT_STORAGE_DURABLE_WAL_COMMIT_POLICY,
            wal_group_commit_delay_micros: DEFAULT_STORAGE_WAL_GROUP_COMMIT_DELAY_MICROS,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_backend_is_durable() {
        let cfg = StorageConfig::default();
        assert_eq!(cfg.backend, DEFAULT_STORAGE_BACKEND);
    }

    #[test]
    fn default_data_dir_is_dot_data() {
        let cfg = StorageConfig::default();
        assert_eq!(cfg.data_dir, PathBuf::from(DEFAULT_STORAGE_DATA_DIR));
    }

    #[test]
    fn default_server_data_dir_is_aiondb_subdir() {
        assert_eq!(
            PathBuf::from(DEFAULT_SERVER_STORAGE_DATA_DIR),
            PathBuf::from("./data/aiondb")
        );
    }

    #[test]
    fn default_page_size_is_8192() {
        let cfg = StorageConfig::default();
        assert_eq!(cfg.page_size, DEFAULT_STORAGE_PAGE_SIZE);
    }

    #[test]
    fn default_max_open_files_is_256() {
        let cfg = StorageConfig::default();
        assert_eq!(cfg.max_open_files, DEFAULT_STORAGE_MAX_OPEN_FILES);
    }

    #[test]
    fn default_table_pool_frames_is_256() {
        let cfg = StorageConfig::default();
        assert_eq!(cfg.table_pool_frames, DEFAULT_STORAGE_TABLE_POOL_FRAMES);
    }

    #[test]
    fn default_snapshot_pool_frames_is_64() {
        let cfg = StorageConfig::default();
        assert_eq!(
            cfg.snapshot_pool_frames,
            DEFAULT_STORAGE_SNAPSHOT_POOL_FRAMES
        );
    }

    #[test]
    fn default_wal_group_commit_delay_is_zero_micros() {
        let cfg = StorageConfig::default();
        assert_eq!(
            cfg.wal_group_commit_delay_micros,
            DEFAULT_STORAGE_WAL_GROUP_COMMIT_DELAY_MICROS
        );
    }

    #[test]
    fn clone_produces_equal_config() {
        let a = StorageConfig::default();
        let b = a.clone();
        assert_eq!(a, b);
    }

    #[test]
    fn eq_works_for_identical_configs() {
        let a = StorageConfig {
            data_dir: PathBuf::from("/tmp/db"),
            page_size: 4096,
            max_open_files: 512,
            ..StorageConfig::default()
        };
        let b = StorageConfig {
            data_dir: PathBuf::from("/tmp/db"),
            page_size: 4096,
            max_open_files: 512,
            ..StorageConfig::default()
        };
        assert_eq!(a, b);
    }

    #[test]
    fn ne_when_data_dir_differs() {
        let mut a = StorageConfig::default();
        let b = StorageConfig::default();
        a.data_dir = PathBuf::from("/other");
        assert_ne!(a, b);
    }

    #[test]
    fn ne_when_page_size_differs() {
        let mut a = StorageConfig::default();
        let b = StorageConfig::default();
        a.page_size = 16384;
        assert_ne!(a, b);
    }

    #[test]
    fn ne_when_max_open_files_differs() {
        let mut a = StorageConfig::default();
        let b = StorageConfig::default();
        a.max_open_files = 1;
        assert_ne!(a, b);
    }

    #[test]
    fn debug_format_contains_fields() {
        let cfg = StorageConfig::default();
        let dbg = format!("{cfg:?}");
        assert!(dbg.contains("backend"));
        assert!(dbg.contains("data_dir"));
        assert!(dbg.contains("page_size"));
        assert!(dbg.contains("max_open_files"));
        assert!(dbg.contains("table_pool_frames"));
        assert!(dbg.contains("snapshot_pool_frames"));
        assert!(dbg.contains("durable_wal_commit_policy"));
        assert!(dbg.contains("wal_group_commit_delay_micros"));
    }

    #[test]
    fn parse_storage_backend_aliases() {
        assert_eq!(
            StorageBackend::parse("in-memory"),
            Some(StorageBackend::InMemory)
        );
        assert_eq!(
            StorageBackend::parse("PAGE_ENGINE"),
            Some(StorageBackend::PageEngine)
        );
        assert_eq!(StorageBackend::parse("wal"), Some(StorageBackend::Durable));
    }

    #[test]
    fn parse_storage_backend_rejects_unknown_values() {
        assert_eq!(StorageBackend::parse("banana"), None);
    }

    #[test]
    fn parse_durable_wal_commit_policy_aliases() {
        assert_eq!(
            DurableWalCommitPolicy::parse("always"),
            Some(DurableWalCommitPolicy::Always)
        );
        assert_eq!(
            DurableWalCommitPolicy::parse("every:64"),
            Some(DurableWalCommitPolicy::Every(64))
        );
        assert_eq!(
            DurableWalCommitPolicy::parse("128"),
            Some(DurableWalCommitPolicy::Every(128))
        );
        assert_eq!(
            DurableWalCommitPolicy::parse("async"),
            Some(DurableWalCommitPolicy::Never)
        );
        assert_eq!(DurableWalCommitPolicy::parse("every:0"), None);
    }

    // ===================================================================
    // NEW EDGE CASE TESTS
    // ===================================================================

    // --- zero page_size ---

    #[test]
    fn storage_zero_page_size() {
        let cfg = StorageConfig {
            data_dir: PathBuf::from(DEFAULT_STORAGE_DATA_DIR),
            page_size: 0,
            max_open_files: DEFAULT_STORAGE_MAX_OPEN_FILES,
            ..StorageConfig::default()
        };
        assert_eq!(cfg.page_size, 0);
    }

    // --- max usize page_size ---

    #[test]
    fn storage_max_page_size() {
        let cfg = StorageConfig {
            data_dir: PathBuf::from(DEFAULT_STORAGE_DATA_DIR),
            page_size: usize::MAX,
            max_open_files: DEFAULT_STORAGE_MAX_OPEN_FILES,
            ..StorageConfig::default()
        };
        assert_eq!(cfg.page_size, usize::MAX);
    }

    // --- zero max_open_files ---

    #[test]
    fn storage_zero_max_open_files() {
        let cfg = StorageConfig {
            data_dir: PathBuf::from(DEFAULT_STORAGE_DATA_DIR),
            page_size: DEFAULT_STORAGE_PAGE_SIZE,
            max_open_files: 0,
            ..StorageConfig::default()
        };
        assert_eq!(cfg.max_open_files, 0);
    }

    // --- max usize max_open_files ---

    #[test]
    fn storage_max_open_files_max_usize() {
        let cfg = StorageConfig {
            data_dir: PathBuf::from(DEFAULT_STORAGE_DATA_DIR),
            page_size: DEFAULT_STORAGE_PAGE_SIZE,
            max_open_files: usize::MAX,
            ..StorageConfig::default()
        };
        assert_eq!(cfg.max_open_files, usize::MAX);
    }

    // --- unusual paths ---

    #[test]
    fn storage_empty_data_dir() {
        let cfg = StorageConfig {
            data_dir: PathBuf::from(""),
            page_size: DEFAULT_STORAGE_PAGE_SIZE,
            max_open_files: DEFAULT_STORAGE_MAX_OPEN_FILES,
            ..StorageConfig::default()
        };
        assert_eq!(cfg.data_dir, PathBuf::from(""));
    }

    #[test]
    fn storage_root_data_dir() {
        let cfg = StorageConfig {
            data_dir: PathBuf::from("/"),
            page_size: DEFAULT_STORAGE_PAGE_SIZE,
            max_open_files: DEFAULT_STORAGE_MAX_OPEN_FILES,
            ..StorageConfig::default()
        };
        assert_eq!(cfg.data_dir, PathBuf::from("/"));
    }

    #[test]
    fn storage_unicode_path() {
        let cfg = StorageConfig {
            data_dir: PathBuf::from("/données/données"),
            page_size: DEFAULT_STORAGE_PAGE_SIZE,
            max_open_files: DEFAULT_STORAGE_MAX_OPEN_FILES,
            ..StorageConfig::default()
        };
        assert_eq!(cfg.data_dir, PathBuf::from("/données/données"));
    }

    #[test]
    fn storage_path_with_spaces() {
        let cfg = StorageConfig {
            data_dir: PathBuf::from("/path with spaces/data"),
            page_size: DEFAULT_STORAGE_PAGE_SIZE,
            max_open_files: DEFAULT_STORAGE_MAX_OPEN_FILES,
            ..StorageConfig::default()
        };
        assert_eq!(cfg.data_dir, PathBuf::from("/path with spaces/data"));
    }

    #[test]
    fn storage_path_with_dots() {
        let cfg = StorageConfig {
            data_dir: PathBuf::from("/a/../b/./c"),
            page_size: DEFAULT_STORAGE_PAGE_SIZE,
            max_open_files: DEFAULT_STORAGE_MAX_OPEN_FILES,
            ..StorageConfig::default()
        };
        // PathBuf stores as-is without resolving
        assert_eq!(cfg.data_dir, PathBuf::from("/a/../b/./c"));
    }

    #[test]
    fn storage_very_long_path() {
        let long = "/".to_owned() + &"x".repeat(4096);
        let cfg = StorageConfig {
            data_dir: PathBuf::from(&long),
            page_size: DEFAULT_STORAGE_PAGE_SIZE,
            max_open_files: DEFAULT_STORAGE_MAX_OPEN_FILES,
            ..StorageConfig::default()
        };
        assert_eq!(cfg.data_dir.to_str().unwrap().len(), 4097);
    }

    // --- all-zero config ---

    #[test]
    fn storage_all_zero_config() {
        let cfg = StorageConfig {
            backend: StorageBackend::InMemory,
            data_dir: PathBuf::new(),
            page_size: 0,
            max_open_files: 0,
            table_pool_frames: 0,
            snapshot_pool_frames: 0,
            eviction_threshold_percent: 0,
            durable_wal_commit_policy: DurableWalCommitPolicy::Never,
            wal_group_commit_delay_micros: 0,
        };
        assert_eq!(cfg.page_size, 0);
        assert_eq!(cfg.max_open_files, 0);
        assert_eq!(cfg.data_dir, PathBuf::new());
        assert_eq!(cfg.table_pool_frames, 0);
        assert_eq!(cfg.snapshot_pool_frames, 0);
    }

    // --- power-of-two page sizes ---

    #[test]
    fn storage_power_of_two_page_sizes() {
        for shift in 0..20 {
            let ps = 1usize << shift;
            let cfg = StorageConfig {
                data_dir: PathBuf::from(DEFAULT_STORAGE_DATA_DIR),
                page_size: ps,
                max_open_files: DEFAULT_STORAGE_MAX_OPEN_FILES,
                ..StorageConfig::default()
            };
            assert_eq!(cfg.page_size, ps);
        }
    }

    // --- debug output includes specific default values ---

    #[test]
    fn debug_includes_default_page_size_value() {
        let cfg = StorageConfig::default();
        let dbg = format!("{cfg:?}");
        assert!(dbg.contains(&DEFAULT_STORAGE_PAGE_SIZE.to_string()));
        assert!(dbg.contains(&DEFAULT_STORAGE_MAX_OPEN_FILES.to_string()));
        assert!(dbg.contains(&DEFAULT_STORAGE_TABLE_POOL_FRAMES.to_string()));
        assert!(dbg.contains(&DEFAULT_STORAGE_SNAPSHOT_POOL_FRAMES.to_string()));
    }
}
