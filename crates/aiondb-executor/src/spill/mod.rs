//! Spill-to-disk infrastructure for the executor.
//!
//! When an operation (sort, hash join, aggregate) exceeds its in-memory
//! budget, intermediate rows are serialised to temporary files on disk
//! and read back during the merge phase.  This prevents OOM kills on
//! large working sets while keeping small queries entirely in-memory.

pub(crate) mod sort_buffer;
pub(crate) mod spill_file;
