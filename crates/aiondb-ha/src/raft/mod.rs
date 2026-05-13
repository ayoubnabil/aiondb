//! Raft consensus layer for distributed cluster metadata.
//!
//! Provides term-indexed log replication, persistent state, and
//! leader-based consensus for shard assignments, node membership,
//! and cluster configuration changes.
//!
//! This module extends the existing HA election subsystem with:
//! - Persistent `voted_for` (Raft safety requirement)
//! - `AppendEntries` RPC for log replication
//! - A durable Raft log with term-indexed entries
//! - Commit index tracking for replicated state

pub mod log;
pub mod node;
pub mod rpc;
pub mod state;

pub use log::{RaftEntry, RaftLog};
pub use node::{RaftNode, RaftRole};
pub use rpc::{AppendEntriesRequest, AppendEntriesResponse};
pub use state::{PersistentState, RaftCommand};
