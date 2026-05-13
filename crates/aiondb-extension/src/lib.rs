//! `AionDB` Extension Framework
//!
//! Provides a trait-based system for compiled-in extensions that can register
//! functions, types, and operators with the engine.  Each extension is a Rust
//! struct that implements [`Extension`].
//!
//! The [`ExtensionRegistry`] tracks which extensions are available and which
//! have been installed via `CREATE EXTENSION`.

mod builtin;
mod registry;

pub use builtin::{pgcrypto, uuid_ossp, vector};
pub use registry::{ExtensionDescriptor, ExtensionFunction, ExtensionRegistry, InstalledExtension};

use aiondb_core::{DbResult, Value};

// ---------------------------------------------------------------------------
// Extension trait
// ---------------------------------------------------------------------------

/// A compiled-in extension that can be installed into an `AionDB` database.
///
/// Extensions declare metadata (name, version, description, dependencies) and
/// provide an `install` callback that registers functions with the registry.
pub trait Extension: Send + Sync {
    /// Canonical extension name (e.g. `"uuid-ossp"`, `"pgcrypto"`).
    fn name(&self) -> &str;

    /// Semantic version string (e.g. `"1.1"`).
    fn version(&self) -> &str;

    /// Human-readable description.
    fn description(&self) -> &str;

    /// Names of extensions that must be installed before this one.
    fn dependencies(&self) -> &[&str] {
        &[]
    }

    /// Register functions (and future: types, operators) with the catalog.
    ///
    /// Called exactly once when the user executes `CREATE EXTENSION <name>`.
    #[allow(clippy::missing_errors_doc)]
    fn install(&self, registrar: &mut dyn ExtensionRegistrar) -> DbResult<()>;

    /// Upgrade from `from_version` to the current version.
    ///
    /// The default implementation returns an error indicating that in-place
    /// upgrade is not supported.
    #[allow(clippy::missing_errors_doc)]
    fn upgrade(
        &self,
        _from_version: &str,
        _registrar: &mut dyn ExtensionRegistrar,
    ) -> DbResult<()> {
        Err(aiondb_core::DbError::internal(format!(
            "extension \"{}\" does not support in-place upgrade",
            self.name(),
        )))
    }
}

/// Callback interface passed to [`Extension::install`] so the extension can
/// register its objects without knowing about the catalog internals.
pub trait ExtensionRegistrar {
    /// Register a scalar function.
    fn register_function(&mut self, func: ExtensionFunction);
}

/// Type alias for the native evaluation function pointer used by extensions.
///
/// The function receives a slice of evaluated argument [`Value`]s and returns
/// a single [`Value`] (or an error).
pub type ExtensionEvalFn = fn(&[Value]) -> DbResult<Value>;
