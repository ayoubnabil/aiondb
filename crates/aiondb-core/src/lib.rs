//! Core shared types for `AionDB`.
//!
//! Runtime values, identifiers, SQLSTATE-aware errors, and small
//! cross-cutting helpers used across parser/planner/executor/storage.
//!
//! # Example
//!
//! ```rust
//! use aiondb_core::{Row, SqlState, Value};
//!
//! let row = Row::new(vec![Value::Int(7), Value::Text("hello".to_owned())]);
//! assert_eq!(row.len(), 2);
//! assert_eq!(SqlState::UndefinedTable.code(), "42P01");
//! ```

pub mod bounded_io;
pub mod checksum;
pub mod convert;
pub mod data_type;
pub mod error;
pub mod fk;
pub mod identity;
pub mod ids;
pub mod network;
pub mod numeric;
pub mod pg_compat;
pub mod pg_lsn;
pub mod replication_fs;
pub mod row;
pub mod sql_trace;
pub mod temporal;
pub mod text_utils;
pub mod tid;
pub mod trace_context;
pub mod value;
pub mod vector_limits;
pub mod vector_storage;

pub use data_type::{DataType, TextTypeModifier, VectorElementType};
pub use error::{DbError, DbResult, ErrorReport, SqlState};
pub use fk::{FkAction, FkMatchType};
pub use identity::{IdentityGeneration, IdentityOptions, IdentitySpec};
pub use ids::{
    ColumnId, DatabaseId, IndexId, RelationId, SchemaId, SequenceId, TenantId, TupleId, TxnId,
};
pub use network::{MacAddr, MacAddr8};
pub use numeric::{IntervalValue, NumericValue};
pub use pg_compat::{
    compat_database_oid, compat_function_oid, compat_locale, compat_role_oid,
    compat_server_version_num_string, compat_setting_value, compat_timezone, compat_version_banner,
    is_compat_executor_intentional_noop_tag, AIONDB_VECTOR_TYPE_OID, COMPAT_BOOTSTRAP_ROLE_NAME,
    COMPAT_BOOTSTRAP_ROLE_OID, COMPAT_CLIENT_ENCODING, COMPAT_CLIENT_MIN_MESSAGES,
    COMPAT_DATE_STYLE, COMPAT_DEFAULT_DATABASE_NAME, COMPAT_DEFAULT_DATABASE_OID,
    COMPAT_DEFAULT_LOCALE, COMPAT_DEFAULT_SEARCH_PATH, COMPAT_DEFAULT_TIMEZONE,
    COMPAT_DEFAULT_TRANSACTION_DEFERRABLE, COMPAT_DEFAULT_TRANSACTION_ISOLATION,
    COMPAT_DEFAULT_TRANSACTION_READ_ONLY, COMPAT_EXECUTOR_INTENTIONAL_NOOP_TAGS,
    COMPAT_INFORMATION_SCHEMA_NAMESPACE_OID, COMPAT_INTEGER_DATETIMES, COMPAT_INTERVAL_STYLE,
    COMPAT_MAX_IDENTIFIER_LENGTH, COMPAT_PGVECTOR_HALFVEC_ARRAY_OID, COMPAT_PGVECTOR_HALFVEC_OID,
    COMPAT_PGVECTOR_HNSW_AM_OID, COMPAT_PGVECTOR_IVFFLAT_AM_OID,
    COMPAT_PGVECTOR_SPARSEVEC_ARRAY_OID, COMPAT_PGVECTOR_SPARSEVEC_OID,
    COMPAT_PGVECTOR_VECTOR_ARRAY_OID, COMPAT_PGVECTOR_VECTOR_OID, COMPAT_PG_BIT_ARRAY_OID,
    COMPAT_PG_BIT_OID, COMPAT_PG_CATALOG_NAMESPACE_OID, COMPAT_PG_DEFAULT_TABLESPACE_OID,
    COMPAT_PG_GLOBAL_TABLESPACE_OID, COMPAT_PG_VARBIT_ARRAY_OID, COMPAT_PG_VARBIT_OID,
    COMPAT_PUBLIC_NAMESPACE_OID, COMPAT_SERVER_ENCODING, COMPAT_SERVER_VERSION,
    COMPAT_SERVER_VERSION_NUM, COMPAT_STANDARD_CONFORMING_STRINGS, PG_TEMP_SCHEMA_NAME,
};
pub use pg_lsn::PgLsnValue;
pub use row::Row;
pub use temporal::{DateOrder, DateStyleFamily, DateStyleSetting, PgDate, TimeZoneSetting};
pub use text_utils::{escape_sql_literal, hex_encode, hex_encode_into, pg_array_unescape_quoted};
pub use tid::TidValue;
pub use value::{Value, VectorValue};
pub use vector_limits::{
    bounded_hnsw_ef_search, HNSW_BASELINE_EF_SEARCH, HNSW_MAX_EF_SEARCH, VECTOR_MAX_K,
};
