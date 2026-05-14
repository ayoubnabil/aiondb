//! Remote fragment execution transport for distributed `AionDB` queries.
//!
//! Provides client/server components for executing query plan fragments
//! on remote cluster nodes over authenticated, optionally TLS-encrypted
//! TCP connections.

pub mod auth;
pub mod client;
pub mod protocol;
pub mod server;
pub mod tls;

pub use auth::AuthToken;
pub use client::{ConnectionPool, FragmentClient};
pub use protocol::{FragmentRequest, FragmentResponse};
pub use server::{FragmentExecutor, FragmentServer};
