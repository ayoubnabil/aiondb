//! Extension registry: tracks available and installed extensions.

use std::collections::HashMap;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use aiondb_core::{DataType, DbError, DbResult};

use crate::{Extension, ExtensionEvalFn, ExtensionRegistrar};

// ---------------------------------------------------------------------------
// Function metadata exposed by extensions
// ---------------------------------------------------------------------------

/// Describes a single scalar function contributed by an extension.
#[derive(Clone)]
pub struct ExtensionFunction {
    /// SQL-level function name (e.g. `"uuid_generate_v4"`).
    pub name: String,
    /// Return type.
    pub return_type: DataType,
    /// Minimum number of arguments.
    pub min_args: usize,
    /// Maximum number of arguments (`None` = variadic).
    pub max_args: Option<usize>,
    /// Native evaluation function.
    pub eval_fn: ExtensionEvalFn,
}

// ---------------------------------------------------------------------------
// Installed extension record
// ---------------------------------------------------------------------------

/// Metadata for an extension that has been installed via `CREATE EXTENSION`.
#[derive(Clone, Debug)]
pub struct InstalledExtension {
    /// Synthetic OID for `pg_extension` compatibility.
    pub oid: i32,
    /// Extension name.
    pub name: String,
    /// Installed version string.
    pub version: String,
    /// Whether the extension can be relocated to a different schema.
    pub relocatable: bool,
}

/// Metadata for an extension that is available but not necessarily installed.
#[derive(Clone, Debug)]
pub struct ExtensionDescriptor {
    pub name: String,
    pub default_version: String,
    pub description: String,
    pub dependencies: Vec<String>,
}

// ---------------------------------------------------------------------------
// Registrar implementation used during install
// ---------------------------------------------------------------------------

struct InstallRegistrar {
    functions: Vec<ExtensionFunction>,
}

impl ExtensionRegistrar for InstallRegistrar {
    fn register_function(&mut self, func: ExtensionFunction) {
        self.functions.push(func);
    }
}

// ---------------------------------------------------------------------------
// ExtensionRegistry
// ---------------------------------------------------------------------------

/// Central registry that knows about all compiled-in extensions and which ones
/// have been installed.
pub struct ExtensionRegistry {
    /// All compiled-in extensions, keyed by canonical name.
    available: HashMap<String, Arc<dyn Extension>>,
    /// Installed extensions and their contributed functions.
    inner: RwLock<RegistryState>,
}

struct RegistryState {
    /// Installed extension metadata, keyed by name.
    installed: HashMap<String, InstalledExtension>,
    /// Functions contributed by installed extensions, keyed by lowercase name.
    functions: HashMap<String, ExtensionFunction>,
    /// Tracks which extension contributed each function.
    function_owner: HashMap<String, String>,
    /// Next synthetic OID for new extensions.
    next_oid: i32,
}

impl ExtensionRegistry {
    /// Create a new registry and register the default set of compiled-in
    /// extensions.
    #[must_use]
    pub fn new() -> Self {
        let mut available: HashMap<String, Arc<dyn Extension>> = HashMap::new();

        // Register built-in extensions
        let uuid_ossp = Arc::new(crate::builtin::uuid_ossp::UuidOsspExtension);
        available.insert(uuid_ossp.name().to_owned(), uuid_ossp);

        let pgcrypto = Arc::new(crate::builtin::pgcrypto::PgcryptoExtension);
        available.insert(pgcrypto.name().to_owned(), pgcrypto);

        let vector = Arc::new(crate::builtin::vector::VectorExtension);
        available.insert(vector.name().to_owned(), vector);

        Self {
            available,
            inner: RwLock::new(RegistryState {
                installed: HashMap::new(),
                functions: HashMap::new(),
                function_owner: HashMap::new(),
                // Start synthetic OIDs above the PostgreSQL built-in range.
                next_oid: 90000,
            }),
        }
    }

    fn read_state(&self) -> RwLockReadGuard<'_, RegistryState> {
        self.inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn write_state(&self) -> RwLockWriteGuard<'_, RegistryState> {
        self.inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    // -- query APIs --------------------------------------------------------

    /// List all available (compiled-in) extensions with their metadata.
    pub fn list_available(&self) -> Vec<ExtensionDescriptor> {
        let state = self.read_state();
        self.available
            .values()
            .map(|ext| {
                let installed_version = state.installed.get(ext.name()).map(|i| i.version.clone());
                let _ = installed_version; // used by pg_available_extensions
                ExtensionDescriptor {
                    name: ext.name().to_owned(),
                    default_version: ext.version().to_owned(),
                    description: ext.description().to_owned(),
                    dependencies: ext.dependencies().iter().map(|s| (*s).to_owned()).collect(),
                }
            })
            .collect()
    }

    /// List currently installed extensions.
    pub fn list_installed(&self) -> Vec<InstalledExtension> {
        let state = self.read_state();
        state.installed.values().cloned().collect()
    }

    /// Check if an extension is installed.
    pub fn is_installed(&self, name: &str) -> bool {
        let state = self.read_state();
        state.installed.contains_key(name)
    }

    /// Look up a function contributed by any installed extension.
    pub fn lookup_function(&self, name: &str) -> Option<ExtensionFunction> {
        let state = self.read_state();
        state.functions.get(&name.to_ascii_lowercase()).cloned()
    }

    /// Get the installed version of an extension, if installed.
    pub fn installed_version(&self, name: &str) -> Option<String> {
        let state = self.read_state();
        state.installed.get(name).map(|i| i.version.clone())
    }

    // -- mutation APIs -----------------------------------------------------

    /// Install an extension.  Validates that:
    /// - the extension is known (compiled-in)
    /// - it is not already installed
    /// - all dependencies are satisfied
    #[allow(clippy::missing_errors_doc)]
    pub fn install_extension(&self, name: &str, if_not_exists: bool) -> DbResult<()> {
        let ext = self
            .available
            .get(name)
            .ok_or_else(|| {
                DbError::from_report(aiondb_core::ErrorReport::new(
                    aiondb_core::SqlState::UndefinedObject,
                    format!("extension \"{name}\" is not available"),
                ))
            })?
            .clone();

        // Hold the write lock for the entire check + install + commit
        // sequence. The previous read-then-write split race let two
        // concurrent installs of the same name both pass the duplicate
        // check, then collide on the second `installed.insert` and lose
        // the first OID. Hold one lock end-to-end.
        let mut state = self.write_state();
        if state.installed.contains_key(name) {
            if if_not_exists {
                return Ok(());
            }
            return Err(DbError::from_report(aiondb_core::ErrorReport::new(
                aiondb_core::SqlState::DuplicateObject,
                format!("extension \"{name}\" already exists"),
            )));
        }
        for dep in ext.dependencies() {
            if !state.installed.contains_key(*dep) {
                return Err(DbError::from_report(aiondb_core::ErrorReport::new(
                    aiondb_core::SqlState::UndefinedObject,
                    format!(
                        "required extension \"{dep}\" is not installed (required by \"{name}\")"
                    ),
                )));
            }
        }

        let mut registrar = InstallRegistrar {
            functions: Vec::new(),
        };
        ext.install(&mut registrar)?;

        // Reject collisions on function names: two extensions exporting
        // each other. Surface the conflict before committing OID.
        for func in &registrar.functions {
            let fn_name = func.name.to_ascii_lowercase();
            if let Some(existing_owner) = state.function_owner.get(&fn_name) {
                return Err(DbError::from_report(aiondb_core::ErrorReport::new(
                    aiondb_core::SqlState::DuplicateObject,
                    format!(
                        "function \"{fn_name}\" is already provided by extension \"{existing_owner}\""
                    ),
                )));
            }
        }

        let oid = state.next_oid;
        state.next_oid += 1;
        state.installed.insert(
            name.to_owned(),
            InstalledExtension {
                oid,
                name: name.to_owned(),
                version: ext.version().to_owned(),
                relocatable: false,
            },
        );
        for func in registrar.functions {
            let fn_name = func.name.to_ascii_lowercase();
            state
                .function_owner
                .insert(fn_name.clone(), name.to_owned());
            state.functions.insert(fn_name, func);
        }

        Ok(())
    }

    /// Drop an installed extension and remove its contributed functions.
    #[allow(clippy::missing_errors_doc)]
    pub fn drop_extension(&self, name: &str, if_exists: bool) -> DbResult<Option<String>> {
        let mut state = self.write_state();

        if !state.installed.contains_key(name) {
            if if_exists {
                return Ok(Some(format!(
                    "extension \"{name}\" does not exist, skipping"
                )));
            }
            return Err(DbError::from_report(aiondb_core::ErrorReport::new(
                aiondb_core::SqlState::UndefinedObject,
                format!("extension \"{name}\" does not exist"),
            )));
        }

        // Check if other installed extensions depend on this one
        for other_name in state.installed.keys() {
            if other_name == name {
                continue;
            }
            if let Some(ext) = self.available.get(other_name) {
                if ext.dependencies().contains(&name) {
                    return Err(DbError::from_report(aiondb_core::ErrorReport::new(
                        aiondb_core::SqlState::DependentObjectsStillExist,
                        format!(
                            "cannot drop extension \"{name}\" because extension \"{other_name}\" depends on it"
                        ),
                    )));
                }
            }
        }

        // Remove functions owned by this extension
        let owned_functions: Vec<String> = state
            .function_owner
            .iter()
            .filter(|(_, owner)| *owner == name)
            .map(|(fn_name, _)| fn_name.clone())
            .collect();
        for fn_name in &owned_functions {
            state.functions.remove(fn_name);
            state.function_owner.remove(fn_name);
        }

        state.installed.remove(name);
        Ok(None)
    }

    /// Upgrade an installed extension to its current compiled-in version.
    #[allow(clippy::missing_errors_doc)]
    pub fn alter_extension_update(&self, name: &str) -> DbResult<()> {
        let ext = self
            .available
            .get(name)
            .ok_or_else(|| {
                DbError::from_report(aiondb_core::ErrorReport::new(
                    aiondb_core::SqlState::UndefinedObject,
                    format!("extension \"{name}\" is not available"),
                ))
            })?
            .clone();

        let from_version = {
            let state = self.read_state();
            match state.installed.get(name) {
                Some(inst) => inst.version.clone(),
                None => {
                    return Err(DbError::from_report(aiondb_core::ErrorReport::new(
                        aiondb_core::SqlState::UndefinedObject,
                        format!("extension \"{name}\" is not installed"),
                    )));
                }
            }
        };

        if from_version == ext.version() {
            return Ok(());
        }

        let mut registrar = InstallRegistrar {
            functions: Vec::new(),
        };
        ext.upgrade(&from_version, &mut registrar)?;

        // Update state
        let mut state = self.write_state();
        let inst = state.installed.get_mut(name).ok_or_else(|| {
            DbError::internal(format!(
                "extension \"{name}\" disappeared from installed state during upgrade"
            ))
        })?;
        ext.version().clone_into(&mut inst.version);
        for func in registrar.functions {
            let fn_name = func.name.to_ascii_lowercase();
            state
                .function_owner
                .insert(fn_name.clone(), name.to_owned());
            state.functions.insert(fn_name, func);
        }

        Ok(())
    }
}

impl Default for ExtensionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for ExtensionRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.read_state();
        f.debug_struct("ExtensionRegistry")
            .field("available", &self.available.keys().collect::<Vec<_>>())
            .field("installed", &state.installed.keys().collect::<Vec<_>>())
            .field("functions", &state.functions.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{mpsc, Arc, Mutex};

    use super::*;
    use crate::ExtensionRegistrar;

    const BLOCKING_UPGRADE_NAME: &str = "blocking-upgrade";
    const BLOCKING_UPGRADE_VERSION: &str = "2.0";
    const BLOCKING_UPGRADE_DESCRIPTION: &str = "test extension";

    struct BlockingUpgradeExtension {
        entered: mpsc::Sender<()>,
        proceed: Mutex<mpsc::Receiver<()>>,
    }

    impl crate::Extension for BlockingUpgradeExtension {
        fn name(&self) -> &str {
            BLOCKING_UPGRADE_NAME
        }

        fn version(&self) -> &str {
            BLOCKING_UPGRADE_VERSION
        }

        fn description(&self) -> &str {
            BLOCKING_UPGRADE_DESCRIPTION
        }

        fn install(&self, _registrar: &mut dyn ExtensionRegistrar) -> DbResult<()> {
            Ok(())
        }

        fn upgrade(
            &self,
            _from_version: &str,
            _registrar: &mut dyn ExtensionRegistrar,
        ) -> DbResult<()> {
            self.entered.send(()).expect("signal upgrade start");
            self.proceed
                .lock()
                .expect("lock receiver")
                .recv()
                .expect("wait for test release");
            Ok(())
        }
    }

    #[test]
    fn new_registry_has_builtin_extensions_available() {
        let reg = ExtensionRegistry::new();
        let available = reg.list_available();
        let names: Vec<&str> = available.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"pgcrypto"), "pgcrypto should be available");
        assert!(
            names.contains(&"uuid-ossp"),
            "uuid-ossp should be available"
        );
        assert!(names.contains(&"vector"), "vector should be available");
    }

    #[test]
    fn no_extensions_installed_initially() {
        let reg = ExtensionRegistry::new();
        assert!(reg.list_installed().is_empty());
        assert!(!reg.is_installed("pgcrypto"));
        assert!(!reg.is_installed("uuid-ossp"));
        assert!(!reg.is_installed("vector"));
    }

    #[test]
    fn install_extension_succeeds() {
        let reg = ExtensionRegistry::new();
        reg.install_extension("pgcrypto", false).unwrap();
        assert!(reg.is_installed("pgcrypto"));
        assert_eq!(reg.installed_version("pgcrypto"), Some("1.3".to_owned()));

        let installed = reg.list_installed();
        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].name, "pgcrypto");
    }

    #[test]
    fn install_vector_extension_succeeds_without_function_registration() {
        let reg = ExtensionRegistry::new();
        reg.install_extension("vector", false).unwrap();
        assert!(reg.is_installed("vector"));
        assert_eq!(reg.installed_version("vector"), Some("0.8.2".to_owned()));
        assert!(reg.lookup_function("l2_distance").is_none());
    }

    #[test]
    fn install_registers_functions() {
        let reg = ExtensionRegistry::new();
        // Before install, functions should not be available.
        assert!(reg.lookup_function("digest").is_none());

        reg.install_extension("pgcrypto", false).unwrap();

        // After install, functions are available.
        let digest_fn = reg.lookup_function("digest");
        assert!(digest_fn.is_some());
        let digest_fn = digest_fn.unwrap();
        assert_eq!(digest_fn.name, "digest");
        assert_eq!(digest_fn.min_args, 2);
        assert_eq!(digest_fn.max_args, Some(2));
        assert_eq!(digest_fn.return_type, DataType::Blob);
    }

    #[test]
    fn install_duplicate_errors() {
        let reg = ExtensionRegistry::new();
        reg.install_extension("pgcrypto", false).unwrap();
        let err = reg.install_extension("pgcrypto", false);
        assert!(err.is_err());
    }

    #[test]
    fn install_duplicate_with_if_not_exists_ok() {
        let reg = ExtensionRegistry::new();
        reg.install_extension("pgcrypto", false).unwrap();
        reg.install_extension("pgcrypto", true).unwrap();
        assert!(reg.is_installed("pgcrypto"));
    }

    #[test]
    fn install_unknown_extension_errors() {
        let reg = ExtensionRegistry::new();
        let err = reg.install_extension("nonexistent", false);
        assert!(err.is_err());
    }

    #[test]
    fn drop_extension_removes_it() {
        let reg = ExtensionRegistry::new();
        reg.install_extension("uuid-ossp", false).unwrap();
        assert!(reg.is_installed("uuid-ossp"));

        reg.drop_extension("uuid-ossp", false).unwrap();
        assert!(!reg.is_installed("uuid-ossp"));
        assert!(reg.list_installed().is_empty());
    }

    #[test]
    fn drop_extension_removes_functions() {
        let reg = ExtensionRegistry::new();
        reg.install_extension("uuid-ossp", false).unwrap();
        assert!(reg.lookup_function("uuid_generate_v4").is_some());

        reg.drop_extension("uuid-ossp", false).unwrap();
        assert!(reg.lookup_function("uuid_generate_v4").is_none());
    }

    #[test]
    fn drop_nonexistent_errors() {
        let reg = ExtensionRegistry::new();
        let err = reg.drop_extension("pgcrypto", false);
        assert!(err.is_err());
    }

    #[test]
    fn drop_nonexistent_with_if_exists_ok() {
        let reg = ExtensionRegistry::new();
        let result = reg.drop_extension("pgcrypto", true).unwrap();
        // Returns a notice message when using IF EXISTS on missing extension.
        assert!(result.is_some());
    }

    #[test]
    fn upgrade_at_same_version_is_noop() {
        let reg = ExtensionRegistry::new();
        reg.install_extension("pgcrypto", false).unwrap();
        reg.alter_extension_update("pgcrypto").unwrap();
        assert_eq!(reg.installed_version("pgcrypto"), Some("1.3".to_owned()));
    }

    #[test]
    fn upgrade_not_installed_errors() {
        let reg = ExtensionRegistry::new();
        let err = reg.alter_extension_update("pgcrypto");
        assert!(err.is_err());
    }

    #[test]
    fn upgrade_unknown_extension_errors() {
        let reg = ExtensionRegistry::new();
        let err = reg.alter_extension_update("nonexistent");
        assert!(err.is_err());
    }

    #[test]
    fn upgrade_fails_if_installed_entry_disappears_mid_upgrade() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (proceed_tx, proceed_rx) = mpsc::channel();
        let reg = Arc::new(ExtensionRegistry {
            available: HashMap::from([(
                "blocking-upgrade".to_owned(),
                Arc::new(BlockingUpgradeExtension {
                    entered: entered_tx,
                    proceed: Mutex::new(proceed_rx),
                }) as Arc<dyn crate::Extension>,
            )]),
            inner: RwLock::new(RegistryState {
                installed: HashMap::from([(
                    "blocking-upgrade".to_owned(),
                    InstalledExtension {
                        oid: 90000,
                        name: "blocking-upgrade".to_owned(),
                        version: "1.0".to_owned(),
                        relocatable: false,
                    },
                )]),
                functions: HashMap::new(),
                function_owner: HashMap::new(),
                next_oid: 90001,
            }),
        });

        let worker_reg = Arc::clone(&reg);
        let handle =
            std::thread::spawn(move || worker_reg.alter_extension_update("blocking-upgrade"));

        entered_rx.recv().expect("upgrade started");
        reg.write_state().installed.remove("blocking-upgrade");
        proceed_tx.send(()).expect("release upgrade");

        let err = handle.join().expect("upgrade thread joins").unwrap_err();
        assert!(
            err.to_string().contains("disappeared from installed state"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn multiple_extensions_can_coexist() {
        let reg = ExtensionRegistry::new();
        reg.install_extension("pgcrypto", false).unwrap();
        reg.install_extension("uuid-ossp", false).unwrap();

        assert!(reg.is_installed("pgcrypto"));
        assert!(reg.is_installed("uuid-ossp"));
        assert_eq!(reg.list_installed().len(), 2);

        // Functions from both should be available.
        assert!(reg.lookup_function("digest").is_some());
        assert!(reg.lookup_function("uuid_generate_v4").is_some());
        assert!(reg.lookup_function("uuid_nil").is_some());
    }

    #[test]
    fn function_lookup_is_case_insensitive() {
        let reg = ExtensionRegistry::new();
        reg.install_extension("pgcrypto", false).unwrap();
        assert!(reg.lookup_function("DIGEST").is_some());
        assert!(reg.lookup_function("Digest").is_some());
        assert!(reg.lookup_function("digest").is_some());
    }

    #[test]
    fn installed_extensions_have_ascending_oids() {
        let reg = ExtensionRegistry::new();
        reg.install_extension("pgcrypto", false).unwrap();
        reg.install_extension("uuid-ossp", false).unwrap();

        let installed = reg.list_installed();
        let oids: Vec<i32> = installed.iter().map(|e| e.oid).collect();
        // OIDs should be distinct.
        assert_ne!(oids[0], oids[1]);
        // Both should be >= 90000 (the starting OID).
        assert!(oids.iter().all(|&o| o >= 90000));
    }

    #[test]
    fn default_trait_creates_same_as_new() {
        let reg = ExtensionRegistry::default();
        let available = reg.list_available();
        assert!(available.len() >= 2);
    }

    #[test]
    fn registry_recovers_from_poisoned_lock() {
        let registry = Arc::new(ExtensionRegistry::new());
        let registry_for_poison = Arc::clone(&registry);
        let _ = std::thread::spawn(move || {
            let _guard = registry_for_poison.inner.write().expect("lock");
            panic!("poison extension registry lock");
        })
        .join();

        let available = registry.list_available();
        assert!(!available.is_empty());
        registry.install_extension("pgcrypto", false).unwrap();
        assert!(registry.lookup_function("digest").is_some());
    }
}
