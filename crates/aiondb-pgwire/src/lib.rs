//! `PostgreSQL` wire protocol (v3) implementation for `AionDB`.
//!
//! This crate provides:
//! - TCP listener accepting `PostgreSQL` client connections
//! - `StartupMessage` / `SSLRequest` handling
//! - Simple query protocol (`Query` -> `RowDescription` -> `DataRow` -> `CommandComplete`)
//! - Extended query protocol (`Parse`/`Bind`/`Describe`/`Execute`/`Sync`),
//!   including text and binary result formats where supported
//! - Error handling with `SQLSTATE` codes mapped from [`aiondb_core::DbError`]
//!
//! # Architecture
//! - [`codec`]: Low-level binary frame reading/writing
//! - [`messages`]: Typed frontend and backend message structs
//! - [`connection`]: Per-client connection handler state machine
//! - [`server`]: TCP listener that spawns connection tasks
//! - [`mod@format`]: `Value`-to-text serialization for `PostgreSQL` text format

pub mod binary_format;
mod bind;
pub mod codec;
pub mod connection;
pub mod engine_pool;
pub mod format;
pub mod messages;
pub mod replication;
pub mod server;
pub mod tls;

#[cfg(test)]
mod extended_query_e2e;
#[cfg(test)]
mod protocol_fuzz;
#[cfg(test)]
mod shutdown_tests;
