use std::fmt;

use aiondb_core::{
    ColumnId, DataType, FkAction, FkMatchType, IdentityGeneration, IndexId, RelationId, SchemaId,
    TenantId, TextTypeModifier,
};
use serde::{Deserialize, Serialize};

/// A single CHECK constraint attached to a domain. Stored as the raw
/// CHECK expression text using `VALUE` as the placeholder for the value
/// being validated, mirroring PostgreSQL's `pg_constraint.consrc` form.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DomainConstraintDescriptor {
    pub name: String,
    /// The CHECK expression source text (using `VALUE` as the placeholder).
    pub check_expr: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommentDescriptor {
    pub object_type: String,
    pub object_identity: String,
    pub comment: String,
}

/// PG rewrite-rule event, mirroring `pg_rewrite.ev_type`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum RuleEventDescriptor {
    Select,
    Insert,
    Update,
    Delete,
}

/// A persisted rewrite rule (`CREATE RULE`). Mirrors `pg_rewrite`. The
/// raw action SQL is kept verbatim - the engine re-parses it whenever it
/// rewrites a DML targeting the rule's relation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuleDescriptor {
    /// Lowercase-normalised rule name.
    pub name: String,
    /// Lowercase-normalised relation name (the `ON <table>` target).
    pub table_name: String,
    pub event: RuleEventDescriptor,
    /// Whether this is `INSTEAD` (`true`) or `ALSO` (`false`).
    pub is_instead: bool,
    /// Raw rule action SQL text (`DO <action>` body, possibly empty for
    /// `DO INSTEAD NOTHING`).
    pub action_sql: String,
    /// Number of expressions in the rule action's RETURNING list, used
    /// by the rewrite engine to validate compatibility with caller-side
    /// RETURNING.
    pub returning_count: u32,
    #[serde(default)]
    pub owner: Option<String>,
}

/// PG row-level security policy command target. Mirrors PG's
/// `pg_policy.polcmd`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum PolicyCommandDescriptor {
    All,
    Select,
    Insert,
    Update,
    Delete,
}

/// Whether a policy is permissive (combined with OR among policies of the
/// same command) or restrictive (combined with AND).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum PolicyKindDescriptor {
    Permissive,
    Restrictive,
}

/// A row-level security policy, mirroring `pg_policy`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PolicyDescriptor {
    /// Lowercase-normalised policy name.
    pub name: String,
    /// Schema-qualified target table name as written by the user
    /// (lowercased). The catalog uses this string verbatim - schema
    /// resolution is the engine's job, mirroring how PG stores
    /// `pg_policy.polrelid`.
    pub table_name: String,
    pub command: PolicyCommandDescriptor,
    pub kind: PolicyKindDescriptor,
    /// Roles this policy applies to. `["public"]` for the catch-all.
    /// Empty when no `TO <role>` clause was supplied (matches PG's
    /// behaviour of defaulting to `public`).
    pub roles: Vec<String>,
    /// USING expression source text (the row visibility predicate).
    pub using_expr: Option<String>,
    /// WITH CHECK expression source text (write-side predicate).
    pub with_check_expr: Option<String>,
    #[serde(default)]
    pub owner: Option<String>,
}

/// Cast invocation context, mirroring PG's `pg_cast.castcontext`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum CastContextDescriptor {
    Explicit,
    Assignment,
    Implicit,
}

/// Cast realisation method, mirroring PG's `pg_cast.castmethod`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum CastMethodDescriptor {
    /// Binary-coercible - no conversion code, source bytes are reused.
    Binary,
    /// Round-trip via the source/target type's I/O functions.
    InOut,
    /// Invoke a SQL function. The function is identified by its
    /// schema-qualified name; AionDB doesn't yet persist a stable cast
    /// function OID, so the textual name is the source of truth.
    Function {
        function_name: String,
        function_oid: i32,
    },
}

/// A user-registered `CREATE CAST` entry. Mirrors PG's `pg_cast` row.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CastDescriptor {
    /// Stable cast OID, mirroring `pg_cast.oid`. Allocated by the
    /// engine session (`next_compat_cast_oid`).
    pub oid: i32,
    pub source_type: String,
    pub target_type: String,
    pub context: CastContextDescriptor,
    pub method: CastMethodDescriptor,
    #[serde(default)]
    pub owner: Option<String>,
}

/// One field inside a composite (`CREATE TYPE … AS (…)`) type.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UserTypeFieldDescriptor {
    pub name: String,
    pub data_type: DataType,
    #[serde(default)]
    pub raw_type_name: Option<String>,
}

/// A user-defined type registered via `CREATE TYPE`. Covers the three
/// kinds the surface supports today: shell types (no body), enum types
/// (`AS ENUM (…)`), and composite types (`AS (…)`). The optional fields
/// stay empty for kinds that don't use them so a single descriptor
/// shape can round-trip through the WAL without per-kind variants.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UserTypeDescriptor {
    /// Lowercase-normalised type name.
    pub name: String,
    /// Owning schema, when the type was created as `schema.type`.
    #[serde(default)]
    pub schema_name: Option<String>,
    /// Stable numeric identifier mirroring `pg_type.oid`. Allocated by
    /// the engine session (`next_compat_type_oid`) and persisted so
    /// `pg_type` lookups remain stable across restarts.
    pub oid: i32,
    /// For ENUM kinds: ordered label list. Empty otherwise.
    #[serde(default)]
    pub enum_labels: Vec<String>,
    /// For composite kinds: declaration-order field list. Empty otherwise.
    #[serde(default)]
    pub composite_fields: Vec<UserTypeFieldDescriptor>,
    /// Owning role at create-time.
    #[serde(default)]
    pub owner: Option<String>,
}

/// A user-defined domain type - `CREATE DOMAIN <name> AS <base_type>
/// [NOT NULL] [DEFAULT …] [CHECK …]`. Mirrors the core fields of
/// PostgreSQL's `pg_type` / `pg_constraint` for domains.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DomainDescriptor {
    /// Lowercase-normalised domain name (no schema qualifier - schemas
    /// are not yet a separate axis for domains in AionDB).
    pub name: String,
    /// Owning schema when the domain was created as `schema.domain`.
    #[serde(default)]
    pub schema_name: Option<String>,
    /// Base type as written in the CREATE DOMAIN statement (already
    /// canonicalised, e.g. `int4`, `text`, `varchar(40)`).
    pub base_type: String,
    /// `NOT NULL` declared on the domain itself.
    pub not_null: bool,
    /// Optional `DEFAULT` expression text.
    pub default_expr: Option<String>,
    /// CHECK constraints in declaration order.
    pub constraints: Vec<DomainConstraintDescriptor>,
    /// `varchar(N)` / `char(N)` length cap inherited from the base type.
    pub char_length: Option<u32>,
    /// Owning role at create-time. `None` for domains created before
    /// owner tracking shipped.
    #[serde(default)]
    pub owner: Option<String>,
}

/// A SQL-language stored function.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FunctionDescriptor {
    pub name: String,
    pub params: Vec<FunctionParamDescriptor>,
    #[serde(default)]
    pub out_params: Vec<FunctionParamDescriptor>,
    pub return_type: DataType,
    #[serde(default)]
    pub raw_return_type_name: Option<String>,
    pub body: String,
    pub language: String,
    #[serde(default)]
    pub owner: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FunctionParamDescriptor {
    pub name: String,
    pub data_type: DataType,
    #[serde(default)]
    pub raw_type_name: Option<String>,
    #[serde(default)]
    pub variadic: bool,
    #[serde(default)]
    pub has_default: bool,
}

/// A row-level trigger on a table.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TriggerDescriptor {
    pub name: String,
    pub table_name: String,
    pub timing: TriggerTimingDescriptor,
    pub event: TriggerEventDescriptor,
    /// Additional events for multi-event triggers (e.g., INSERT OR UPDATE).
    /// When non-empty, the trigger fires for `event` plus all `extra_events`.
    #[serde(default)]
    pub extra_events: Vec<TriggerEventDescriptor>,
    pub function_name: String,
    pub for_each_row: bool,
    /// Arguments passed to the trigger function (e.g., string literals).
    #[serde(default)]
    pub function_args: Vec<String>,
    /// Columns from `UPDATE OF (a, b)` clauses, lowercased. Empty when none.
    /// `ALTER TABLE … DROP COLUMN` consults this list to refuse drops that
    /// would invalidate the trigger.
    #[serde(default)]
    pub update_columns: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TriggerTimingDescriptor {
    Before,
    After,
    InsteadOf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TriggerEventDescriptor {
    Insert,
    Update,
    Delete,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct QualifiedName {
    pub schema: Option<String>,
    pub name: String,
}

impl QualifiedName {
    pub fn new(schema: Option<impl Into<String>>, name: impl Into<String>) -> Self {
        Self {
            schema: schema.map(|value| value.into()),
            name: name.into(),
        }
    }

    pub fn qualified(schema: impl Into<String>, name: impl Into<String>) -> Self {
        Self::new(Some(schema), name)
    }

    pub fn unqualified(name: impl Into<String>) -> Self {
        Self::new(None::<String>, name)
    }

    pub fn parse(input: &str) -> Self {
        match input.split_once('.') {
            Some((schema, name)) => Self::qualified(schema, name),
            None => Self::unqualified(input),
        }
    }

    #[must_use]
    pub fn schema_name(&self) -> Option<&str> {
        self.schema.as_deref()
    }

    #[must_use]
    pub fn object_name(&self) -> &str {
        &self.name
    }
}

impl fmt::Display for QualifiedName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.schema {
            Some(schema) => write!(f, "{schema}.{}", self.name),
            None => f.write_str(&self.name),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TenantDescriptor {
    pub tenant_id: TenantId,
    pub name: String,
    pub schema_id: SchemaId,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SchemaDescriptor {
    pub schema_id: SchemaId,
    pub name: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ColumnDescriptor {
    pub column_id: ColumnId,
    pub name: String,
    pub data_type: DataType,
    #[serde(default)]
    pub raw_type_name: Option<String>,
    #[serde(default)]
    pub text_type_modifier: Option<TextTypeModifier>,
    pub nullable: bool,
    pub ordinal_position: u32,
    pub default_value: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IdentityColumnDescriptor {
    pub ordinal_position: u32,
    pub generation: IdentityGeneration,
    #[serde(default)]
    pub implicit_serial: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ForeignKeyConstraint {
    pub columns: Vec<String>,
    pub referenced_table: String,
    pub referenced_columns: Vec<String>,
    #[serde(default)]
    pub on_delete: FkAction,
    #[serde(default)]
    pub on_update: FkAction,
    /// Optional subset targeted by `ON DELETE SET NULL/DEFAULT (col, ...)`.
    #[serde(default)]
    pub on_delete_set_columns: Vec<String>,
    /// Optional subset targeted by `ON UPDATE SET NULL/DEFAULT (col, ...)`.
    #[serde(default)]
    pub on_update_set_columns: Vec<String>,
    #[serde(default)]
    pub match_type: FkMatchType,
    /// Optional explicit constraint name from `CONSTRAINT <name>`. When None,
    /// callers should derive the implicit name as `{table}_{cols...}_fkey`.
    #[serde(default)]
    pub name: Option<String>,
}

impl ForeignKeyConstraint {
    /// Compute the effective constraint name: explicit `CONSTRAINT <name>`
    /// when present, otherwise the PostgreSQL-style implicit
    /// `{table}_{col1}_{col2}_..._fkey`.
    pub fn effective_name(&self, table_name: &str) -> String {
        if let Some(name) = &self.name {
            return name.clone();
        }
        format!("{table_name}_{}_fkey", self.columns.join("_"))
    }
}

/// Catalog-level sharding configuration persisted with the table.
///
/// Must stay aligned with `aiondb_storage_api::MAX_STORAGE_SHARD_COUNT`.
pub const MAX_CATALOG_SHARD_COUNT: u32 = 1 << 16;

/// Must stay aligned with `aiondb_storage_api::MAX_STORAGE_VIRTUAL_NODES_PER_SHARD`.
pub const MAX_CATALOG_VIRTUAL_NODES_PER_SHARD: u32 = 4096;

/// Must stay aligned with `aiondb_storage_api::MAX_STORAGE_HASH_RING_VIRTUAL_NODES`.
pub const MAX_CATALOG_HASH_RING_VIRTUAL_NODES: u64 = 1 << 20;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CatalogShardConfig {
    /// Column names that form the shard key.
    pub shard_key_columns: Vec<String>,
    /// Number of shards.
    pub shard_count: u32,
    /// Number of virtual nodes per physical shard (consistent hash ring).
    pub virtual_nodes_per_shard: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TableDescriptor {
    pub table_id: RelationId,
    pub schema_id: SchemaId,
    pub name: QualifiedName,
    pub columns: Vec<ColumnDescriptor>,
    #[serde(default)]
    pub identity_columns: Vec<IdentityColumnDescriptor>,
    pub primary_key: Option<Vec<ColumnId>>,
    pub foreign_keys: Vec<ForeignKeyConstraint>,
    pub check_constraints: Vec<CheckConstraint>,
    /// Sharding configuration, when the table is distributed across
    /// multiple shards via consistent hashing.
    #[serde(default)]
    pub shard_config: Option<CatalogShardConfig>,
    /// The role that owns this table. Owners have all privileges
    /// implicitly, matching `PostgreSQL` behavior.
    #[serde(default)]
    pub owner: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CheckConstraint {
    pub name: Option<String>,
    pub expression: String,
}

impl TableDescriptor {
    #[must_use]
    pub fn column_by_name(&self, name: &str) -> Option<&ColumnDescriptor> {
        self.columns
            .iter()
            .find(|column| column.name.eq_ignore_ascii_case(name))
    }

    #[must_use]
    pub fn identity_column(&self, ordinal_position: u32) -> Option<&IdentityColumnDescriptor> {
        self.identity_columns
            .iter()
            .find(|identity| identity.ordinal_position == ordinal_position)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewCheckOption {
    Local,
    Cascaded,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ViewDescriptor {
    pub view_id: RelationId,
    pub schema_id: SchemaId,
    pub name: QualifiedName,
    pub query_sql: String,
    #[serde(default)]
    pub creation_search_path_schemas: Vec<String>,
    pub columns: Vec<ColumnDescriptor>,
    #[serde(default)]
    pub check_option: Option<ViewCheckOption>,
    /// V2-04 : role that created the view. `CREATE OR REPLACE VIEW`
    /// must match the current identity against this value (or be
    /// superuser) before replacing the descriptor. `#[serde(default)]`
    /// is wired so on-disk catalogs from earlier versions deserialize
    /// as `owner=""`, which the replace path treats as
    /// "owner unknown — require superuser".
    #[serde(default)]
    pub owner: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum IndexKind {
    BTree,
    Hash,
    GiST,
    Gin,
    Brin,
    Hnsw,
}

/// Distance metric for an HNSW vector index.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub enum VectorDistanceMetric {
    #[default]
    L2,
    Cosine,
    InnerProduct,
    Manhattan,
}

impl VectorDistanceMetric {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::L2 => "l2",
            Self::Cosine => "cosine",
            Self::InnerProduct => "inner_product",
            Self::Manhattan => "manhattan",
        }
    }
}

/// Quantization codec used for HNSW vector storage.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub enum VectorQuantizationKind {
    #[default]
    None,
    Scalar,
    Binary,
    Product,
}

impl VectorQuantizationKind {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Scalar => "sq",
            Self::Binary => "bq",
            Self::Product => "pq",
        }
    }
}

/// Parameters for an HNSW index.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HnswParams {
    /// Maximum number of connections per layer (default 16).
    pub m: u32,
    /// Search width during construction (default 200).
    pub ef_construction: u32,
    /// Distance metric for the HNSW graph.
    #[serde(default)]
    pub distance_metric: VectorDistanceMetric,
    /// Storage quantization codec.
    #[serde(default)]
    pub quantization: VectorQuantizationKind,
    /// When `true`, vectors are guaranteed L2-normalised; cosine searches
    /// can use the `1 - dot` fast path.
    #[serde(default)]
    pub prenormalised: bool,
}

impl Default for HnswParams {
    fn default() -> Self {
        Self {
            m: 16,
            ef_construction: 200,
            distance_metric: VectorDistanceMetric::L2,
            quantization: VectorQuantizationKind::None,
            prenormalised: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum SortOrder {
    Ascending,
    Descending,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IndexKeyColumn {
    pub column_id: ColumnId,
    pub sort_order: SortOrder,
    pub nulls_first: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IndexDescriptor {
    pub index_id: IndexId,
    pub schema_id: SchemaId,
    pub table_id: RelationId,
    pub name: QualifiedName,
    pub unique: bool,
    #[serde(default)]
    pub nulls_not_distinct: bool,
    pub kind: IndexKind,
    pub key_columns: Vec<IndexKeyColumn>,
    pub include_columns: Vec<ColumnId>,
    #[serde(default)]
    pub constraint_name: Option<String>,
    pub hnsw_params: Option<HnswParams>,
}

impl IndexDescriptor {
    /// Resolve the effective distance metric for an HNSW index.
    ///
    /// Legacy descriptors may not persist `hnsw_params`; those are treated as
    /// L2 indexes for compatibility.
    #[must_use]
    pub fn hnsw_distance_metric(&self) -> Option<VectorDistanceMetric> {
        (self.kind == IndexKind::Hnsw).then(|| {
            self.hnsw_params
                .as_ref()
                .map_or(VectorDistanceMetric::L2, |params| params.distance_metric)
        })
    }
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RoleDescriptor {
    pub name: String,
    pub login: bool,
    pub superuser: bool,
    pub password_hash: Option<String>,
    #[serde(default = "default_true")]
    pub inherit: bool,
    #[serde(default)]
    pub createdb: bool,
    #[serde(default)]
    pub createrole: bool,
    #[serde(default)]
    pub replication: bool,
    #[serde(default)]
    pub bypassrls: bool,
    #[serde(default = "default_connection_limit")]
    pub connection_limit: i64,
    #[serde(default)]
    pub valid_until: Option<String>,
}

impl std::fmt::Debug for RoleDescriptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoleDescriptor")
            .field("name", &self.name)
            .field("login", &self.login)
            .field("superuser", &self.superuser)
            .field(
                "password_hash",
                &self.password_hash.as_ref().map(|_| "<redacted>"),
            )
            .field("inherit", &self.inherit)
            .field("createdb", &self.createdb)
            .field("createrole", &self.createrole)
            .field("replication", &self.replication)
            .field("bypassrls", &self.bypassrls)
            .field("connection_limit", &self.connection_limit)
            .field("valid_until", &self.valid_until)
            .finish()
    }
}

fn default_true() -> bool {
    true
}

fn default_connection_limit() -> i64 {
    -1
}

impl Default for RoleDescriptor {
    fn default() -> Self {
        Self {
            name: String::new(),
            login: false,
            superuser: false,
            password_hash: None,
            inherit: true,
            createdb: false,
            createrole: false,
            replication: false,
            bypassrls: false,
            connection_limit: -1,
            valid_until: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum PrivilegeTarget {
    Table(QualifiedName),
    Function(FunctionPrivilegeTarget),
    Schema(String),
    Database(String),
    Role(String),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FunctionPrivilegeTarget {
    pub name: QualifiedName,
    #[serde(default)]
    pub arg_types: Option<Vec<DataType>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum CatalogPrivilege {
    Select,
    Insert,
    Update,
    Delete,
    Create,
    Usage,
    All,
    Execute,
    Trigger,
    References,
    Connect,
    Temporary,
    Truncate,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PrivilegeDescriptor {
    pub role_name: String,
    pub privilege: CatalogPrivilege,
    pub target: PrivilegeTarget,
}

#[cfg(test)]
mod tests;
