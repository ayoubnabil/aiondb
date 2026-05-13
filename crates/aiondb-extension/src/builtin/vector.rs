//! `vector` extension compatibility.
//!
//! AionDB exposes vector types, distance functions, and ANN index support as
//! built-ins. This extension marker lets pgvector migrations run
//! `CREATE EXTENSION vector` and see a `pg_extension` row.

use aiondb_core::DbResult;

use crate::{Extension, ExtensionRegistrar};

/// The pgvector-compatible `vector` extension marker.
pub struct VectorExtension;

impl Extension for VectorExtension {
    fn name(&self) -> &'static str {
        "vector"
    }

    fn version(&self) -> &'static str {
        "0.8.2"
    }

    fn description(&self) -> &'static str {
        "vector data type and similarity search"
    }

    fn install(&self, _registrar: &mut dyn ExtensionRegistrar) -> DbResult<()> {
        Ok(())
    }
}
