use std::{
    cell::RefCell,
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
};

use aiondb_core::{
    compat_timezone, DataType, DateOrder, DateStyleFamily, DateStyleSetting, TimeZoneSetting,
    COMPAT_DATE_STYLE, COMPAT_DEFAULT_DATABASE_NAME,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompatUserTypeField {
    pub name: String,
    pub data_type: DataType,
    pub raw_type_name: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompatUserType {
    pub name: String,
    pub schema_name: Option<String>,
    pub oid: i32,
    /// For enum types, the ordered list of labels.
    /// Empty for non-enum user types (composite, shell, etc.).
    pub enum_labels: Vec<String>,
    /// Composite fields for `CREATE TYPE ... AS (...)`.
    /// Empty for enum/shell/base-type stubs.
    pub composite_fields: Vec<CompatUserTypeField>,
}

/// A single CHECK constraint attached to a domain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DomainConstraint {
    pub name: String,
    /// The CHECK expression source text (using VALUE as the placeholder).
    pub check_expr: String,
}

/// Metadata for a domain type created via `CREATE DOMAIN`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DomainDef {
    /// The domain name (lowercase-normalised).
    pub name: String,
    /// Owning schema when created as `schema.domain`.
    pub schema_name: Option<String>,
    /// The base type name (e.g. "int4", "text", "float8").
    pub base_type: String,
    /// Whether the domain has a NOT NULL constraint.
    pub not_null: bool,
    /// Optional default expression (source text).
    pub default_expr: Option<String>,
    /// CHECK constraints in definition order.
    pub constraints: Vec<DomainConstraint>,
    /// Optional varchar/char length limit inherited from the base type.
    pub char_length: Option<u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EvalIntervalStyle {
    Postgres,
    PostgresVerbose,
    SqlStandard,
    Iso8601,
}

impl EvalIntervalStyle {
    #[must_use]
    pub fn parse(value: Option<&str>) -> Self {
        let raw = value
            .unwrap_or("postgres")
            .trim()
            .trim_matches('\'')
            .trim_matches('"');

        if raw.eq_ignore_ascii_case("postgres_verbose") {
            Self::PostgresVerbose
        } else if raw.eq_ignore_ascii_case("sql_standard") {
            Self::SqlStandard
        } else if raw.eq_ignore_ascii_case("iso_8601") {
            Self::Iso8601
        } else {
            Self::Postgres
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompatCastContext {
    Explicit,
    Assignment,
    Implicit,
}

impl CompatCastContext {
    #[must_use]
    pub const fn as_pg_code(self) -> &'static str {
        match self {
            Self::Explicit => "e",
            Self::Assignment => "a",
            Self::Implicit => "i",
        }
    }

    #[must_use]
    pub const fn allows_implicit(self) -> bool {
        matches!(self, Self::Implicit)
    }

    #[must_use]
    pub const fn allows_assignment(self) -> bool {
        matches!(self, Self::Implicit | Self::Assignment)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompatCastMethod {
    Binary,
    InOut,
    Function {
        function_name: String,
        function_oid: i32,
    },
}

impl CompatCastMethod {
    #[must_use]
    pub const fn as_pg_code(&self) -> &'static str {
        match self {
            Self::Binary => "b",
            Self::InOut => "i",
            Self::Function { .. } => "f",
        }
    }

    #[must_use]
    pub const fn function_oid(&self) -> i32 {
        match self {
            Self::Function { function_oid, .. } => *function_oid,
            Self::Binary | Self::InOut => 0,
        }
    }

    #[must_use]
    pub fn function_name(&self) -> Option<&str> {
        match self {
            Self::Function { function_name, .. } => Some(function_name.as_str()),
            Self::Binary | Self::InOut => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompatUserCast {
    pub oid: i32,
    pub source_type: String,
    pub target_type: String,
    pub context: CompatCastContext,
    pub method: CompatCastMethod,
}

#[derive(Clone)]
struct CachedTemporalSessionContext {
    datestyle: Option<String>,
    timezone: Option<String>,
    intervalstyle: Option<String>,
    context: EvalSessionContext,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvalTemporalSessionContext {
    pub date_order: DateOrder,
    pub date_style: DateStyleFamily,
    pub timezone: TimeZoneSetting,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvalSessionContext {
    pub date_order: DateOrder,
    pub date_style: DateStyleFamily,
    pub timezone: TimeZoneSetting,
    pub interval_style: EvalIntervalStyle,
    pub current_user: Option<String>,
    pub session_user: Option<String>,
    pub current_schema: Option<String>,
    pub current_database: Option<String>,
    pub lo_session_key: u64,
    pub search_path_schemas: Arc<Vec<String>>,
    pub compat_relation_schemas_by_oid: Arc<HashMap<i32, String>>,
    pub compat_relation_names_by_oid: Arc<HashMap<i32, String>>,
    pub role_names_by_oid: Arc<HashMap<i32, String>>,
    pub compat_user_types: Arc<Vec<CompatUserType>>,
    pub compat_user_casts: Arc<Vec<CompatUserCast>>,
    pub domain_defs: Arc<Vec<DomainDef>>,
    /// `COMMENT ON` entries keyed by `(object_type, canonical_subject)` with
    /// the stored comment text. Populated by the engine per session and read
    /// by pg_catalog.pg_description to expose stored comments.
    pub compat_comments: Arc<HashMap<(String, String), String>>,
    /// `SECURITY LABEL` entries keyed by `(object_type, canonical_subject)`
    /// with `(provider, label)`. Populated by the engine per session and read
    /// by pg_catalog.pg_seclabel.
    pub compat_security_labels: Arc<HashMap<(String, String), (Option<String>, String)>>,
    /// Per-object attributes recorded from ALTER statements. Values encode
    /// `(owner, schema, state, options_joined, tablespace, version)`; empty
    /// strings stand in for `None`. `options_joined` is a `k=v` list joined
    /// with `', '`. pg_catalog views consume this to surface ALTER effects.
    pub compat_misc_attrs:
        Arc<HashMap<(String, String), (String, String, String, String, String, String)>>,
    /// Canonical compat objects currently alive in session state. Keys use
    /// `(compat_tag, canonical_name)` and values keep the original CREATE SQL.
    /// This lets planner-level virtual views distinguish dropped objects from
    /// stale ALTER metadata that may still linger in compat_misc_attrs.
    pub compat_misc_objects: Arc<HashMap<(String, String), String>>,
    /// Per-trigger state set by `ALTER TABLE … {ENABLE|DISABLE} TRIGGER …` /
    /// `ALTER TRIGGER … {ENABLE|DISABLE|DEPENDS ON EXTENSION}`. Key:
    /// `(table_lowercase, trigger_lowercase)`; value canonical state string
    /// or `depends:<ext>`. `trigger_lowercase == "*"` denotes ALL/USER.
    pub compat_trigger_state: Arc<HashMap<(String, String), String>>,
    /// Session rewrite rules recorded by `CREATE RULE` / `CREATE OR REPLACE
    /// RULE`. Keyed `(relation_lowercase, event_uppercase)` where `event`
    /// is `SELECT`/`INSERT`/`UPDATE`/`DELETE`. Value is the replacement
    /// action SQL text - what PG would store as `pg_rewrite.ev_action`.
    pub compat_rules: Arc<HashMap<(String, String), String>>,
    /// Pre-rendered `pg_get_indexdef(index_oid, ...)` definitions keyed by
    /// synthetic PostgreSQL-compatible index OID.
    pub compat_index_defs: Arc<HashMap<i32, String>>,
    /// Pre-rendered `pg_get_constraintdef(constraint_oid, ...)` definitions
    /// keyed by synthetic `pg_constraint.oid`.
    pub compat_constraint_defs: Arc<HashMap<i32, String>>,
    /// Pre-rendered `pg_get_viewdef(view_oid, ...)` definitions keyed by
    /// synthetic relation OID.
    pub compat_view_defs: Arc<HashMap<i32, String>>,
    /// Snapshot of the databases known to the cluster catalog
    /// (ADR-0014). Consumed by `pg_catalog.pg_database` to emit one
    /// row per database. Each entry is a lightweight tuple
    /// `(id, name, owner, encoding, collate, ctype, tablespace_oid,
    /// connection_limit, is_template, allow_connections)`.
    pub cluster_databases: Arc<Vec<ClusterDatabaseSummary>>,
    /// Role-membership grantors tracked by compat GRANT ROLE handling.
    /// Each tuple is `(granted_role, grantee, grantor)` using catalog role
    /// names. Used by `pg_catalog.pg_auth_members` to expose non-bootstrap
    /// grantors.
    pub role_membership_grantors: Arc<Vec<(String, String, String)>>,
}

/// Minimal summary of a cluster database for pg_catalog introspection.
/// Avoids a circular dependency: aiondb-eval does not depend on
/// aiondb-cluster; callers provide flat structures.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClusterDatabaseSummary {
    pub id: u32,
    pub name: String,
    pub owner: String,
    pub encoding: String,
    pub collate: String,
    pub ctype: String,
    pub tablespace_oid: Option<u32>,
    pub connection_limit: Option<i32>,
    pub is_template: bool,
    pub allow_connections: bool,
}

impl Default for EvalSessionContext {
    fn default() -> Self {
        Self::from_settings(Some(COMPAT_DATE_STYLE), Some(&compat_timezone()))
    }
}

impl EvalSessionContext {
    #[must_use]
    pub fn from_settings(datestyle: Option<&str>, timezone: Option<&str>) -> Self {
        Self::from_settings_with_interval_style(datestyle, timezone, None)
    }

    #[must_use]
    pub fn from_settings_with_interval_style(
        datestyle: Option<&str>,
        timezone: Option<&str>,
        intervalstyle: Option<&str>,
    ) -> Self {
        let datestyle = datestyle.map(str::trim).filter(|value| !value.is_empty());
        let timezone = timezone.map(str::trim).filter(|value| !value.is_empty());
        let intervalstyle = intervalstyle
            .map(str::trim)
            .filter(|value| !value.is_empty());

        if datestyle.is_none() && timezone.is_none() && intervalstyle.is_none() {
            static DEFAULT_CONTEXT: OnceLock<EvalSessionContext> = OnceLock::new();
            return DEFAULT_CONTEXT
                .get_or_init(|| Self::build_from_settings_with_interval_style(None, None, None))
                .clone();
        }

        static CACHE: OnceLock<Mutex<Vec<CachedTemporalSessionContext>>> = OnceLock::new();
        let cache = CACHE.get_or_init(|| Mutex::new(Vec::new()));
        if let Ok(cache) = cache.lock() {
            if let Some(entry) = cache.iter().find(|entry| {
                entry.datestyle.as_deref() == datestyle
                    && entry.timezone.as_deref() == timezone
                    && entry.intervalstyle.as_deref() == intervalstyle
            }) {
                return entry.context.clone();
            }
        }

        let context =
            Self::build_from_settings_with_interval_style(datestyle, timezone, intervalstyle);

        if let Ok(mut cache) = cache.lock() {
            if cache.len() >= 32 {
                cache.remove(0);
            }
            cache.push(CachedTemporalSessionContext {
                datestyle: datestyle.map(str::to_owned),
                timezone: timezone.map(str::to_owned),
                intervalstyle: intervalstyle.map(str::to_owned),
                context: context.clone(),
            });
        }

        context
    }

    fn build_from_settings_with_interval_style(
        datestyle: Option<&str>,
        timezone: Option<&str>,
        intervalstyle: Option<&str>,
    ) -> Self {
        let date_style = DateStyleSetting::parse(datestyle.unwrap_or(COMPAT_DATE_STYLE));
        let timezone = timezone.map_or_else(compat_timezone, str::to_owned);

        Self {
            date_order: date_style.order(),
            date_style: date_style.family(),
            timezone: TimeZoneSetting::parse(&timezone),
            interval_style: EvalIntervalStyle::parse(intervalstyle),
            current_user: None,
            session_user: None,
            current_schema: Some("public".to_owned()),
            current_database: Some(COMPAT_DEFAULT_DATABASE_NAME.to_owned()),
            lo_session_key: 0,
            search_path_schemas: Arc::new(vec!["public".to_owned()]),
            compat_relation_schemas_by_oid: Arc::new(HashMap::new()),
            compat_relation_names_by_oid: Arc::new(HashMap::new()),
            role_names_by_oid: Arc::new(HashMap::new()),
            compat_user_types: Arc::new(Vec::new()),
            compat_user_casts: Arc::new(Vec::new()),
            domain_defs: Arc::new(Vec::new()),
            compat_comments: Arc::new(HashMap::new()),
            compat_security_labels: Arc::new(HashMap::new()),
            compat_misc_attrs: Arc::new(HashMap::new()),
            compat_misc_objects: Arc::new(HashMap::new()),
            compat_trigger_state: Arc::new(HashMap::new()),
            compat_rules: Arc::new(HashMap::new()),
            compat_index_defs: Arc::new(HashMap::new()),
            compat_constraint_defs: Arc::new(HashMap::new()),
            compat_view_defs: Arc::new(HashMap::new()),
            cluster_databases: Arc::new(Vec::new()),
            role_membership_grantors: Arc::new(Vec::new()),
        }
    }

    /// Renseigne le snapshot des bases cluster (ADR-0014).
    #[must_use]
    pub fn with_cluster_databases(mut self, databases: Arc<Vec<ClusterDatabaseSummary>>) -> Self {
        self.cluster_databases = databases;
        self
    }

    #[must_use]
    pub fn with_current_schema(mut self, current_schema: Option<String>) -> Self {
        self.current_schema = current_schema;
        self
    }

    #[must_use]
    pub fn with_current_user(mut self, current_user: Option<String>) -> Self {
        self.current_user = current_user;
        self
    }

    #[must_use]
    pub fn with_session_user(mut self, session_user: Option<String>) -> Self {
        self.session_user = session_user;
        self
    }

    #[must_use]
    pub fn with_current_database(mut self, current_database: Option<String>) -> Self {
        self.current_database = current_database;
        self
    }

    #[must_use]
    pub fn with_lo_session_key(mut self, lo_session_key: u64) -> Self {
        self.lo_session_key = lo_session_key;
        self
    }

    #[must_use]
    pub fn with_search_path_schemas(mut self, search_path_schemas: Vec<String>) -> Self {
        self.search_path_schemas = Arc::new(search_path_schemas);
        self
    }

    #[must_use]
    pub fn with_compat_relation_schemas_by_oid(
        mut self,
        compat_relation_schemas_by_oid: HashMap<i32, String>,
    ) -> Self {
        self.compat_relation_schemas_by_oid = Arc::new(compat_relation_schemas_by_oid);
        self
    }

    #[must_use]
    pub fn with_compat_relation_names_by_oid(
        mut self,
        compat_relation_names_by_oid: HashMap<i32, String>,
    ) -> Self {
        self.compat_relation_names_by_oid = Arc::new(compat_relation_names_by_oid);
        self
    }

    #[must_use]
    pub fn with_role_names_by_oid(mut self, role_names_by_oid: HashMap<i32, String>) -> Self {
        self.role_names_by_oid = Arc::new(role_names_by_oid);
        self
    }

    #[must_use]
    pub fn with_compat_user_types(mut self, compat_user_types: Vec<CompatUserType>) -> Self {
        self.compat_user_types = Arc::new(compat_user_types);
        self
    }

    #[must_use]
    pub fn with_compat_user_casts(mut self, compat_user_casts: Vec<CompatUserCast>) -> Self {
        self.compat_user_casts = Arc::new(compat_user_casts);
        self
    }

    #[must_use]
    pub fn with_domain_defs(mut self, domain_defs: Vec<DomainDef>) -> Self {
        self.domain_defs = Arc::new(domain_defs);
        self
    }

    #[must_use]
    pub fn compat_user_type(&self, type_name: &str) -> Option<&CompatUserType> {
        let normalized = normalize_compat_type_name(type_name);
        self.compat_user_types
            .iter()
            .find(|entry| entry.name == normalized)
    }

    #[must_use]
    pub fn domain_def(&self, domain_name: &str) -> Option<&DomainDef> {
        let normalized = normalize_compat_type_name(domain_name);
        self.domain_defs
            .iter()
            .find(|entry| entry.name == normalized)
    }

    #[must_use]
    pub fn compat_cast(&self, source_type: &str, target_type: &str) -> Option<&CompatUserCast> {
        let source = normalize_compat_type_name(source_type);
        let target = normalize_compat_type_name(target_type);
        self.compat_user_casts
            .iter()
            .find(|entry| entry.source_type == source && entry.target_type == target)
    }
}

#[must_use]
pub fn normalize_compat_type_name(type_name: &str) -> String {
    let trimmed = type_name.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let array_suffix = trimmed.ends_with("[]");
    let base = if array_suffix {
        &trimmed[..trimmed.len().saturating_sub(2)]
    } else {
        trimmed
    };
    let base = base
        .rsplit('.')
        .next()
        .unwrap_or(base)
        .trim()
        .trim_matches('"')
        .to_ascii_lowercase();

    let canonical = match base.as_str() {
        "bool" | "boolean" => "bool",
        "bytea" | "blob" => "bytea",
        "int" | "int4" | "integer" => "int4",
        "int8" | "bigint" => "int8",
        "real" | "float4" => "float4",
        "double" | "double precision" | "float8" => "float8",
        "numeric" | "decimal" => "numeric",
        "money" => "money",
        "text" | "varchar" | "character varying" | "char" | "character" | "name" => "text",
        "date" => "date",
        "time" | "time without time zone" => "time",
        "timetz" | "time with time zone" => "timetz",
        "timestamp" | "timestamp without time zone" => "timestamp",
        "timestamptz" | "timestamp with time zone" => "timestamptz",
        "interval" => "interval",
        "uuid" => "uuid",
        "vector" => "vector",
        value if value.starts_with("vector(") => "vector",
        "halfvec" => "halfvec",
        value if value.starts_with("halfvec(") => "halfvec",
        "sparsevec" => "sparsevec",
        value if value.starts_with("sparsevec(") => "sparsevec",
        "bit" => "bit",
        value if value.starts_with("bit(") => "bit",
        "bit varying" | "varbit" => "varbit",
        value if value.starts_with("bit varying(") || value.starts_with("varbit(") => "varbit",
        "json" | "jsonb" => "jsonb",
        "tid" => "tid",
        "pg_lsn" => "pg_lsn",
        "regclass" => "regclass",
        "regtype" => "regtype",
        "regproc" => "regproc",
        "regprocedure" => "regprocedure",
        "regoper" => "regoper",
        "regoperator" => "regoperator",
        "regnamespace" => "regnamespace",
        "regrole" => "regrole",
        "cstring" => "cstring",
        _ => base.as_str(),
    };

    if array_suffix {
        format!("{canonical}[]")
    } else {
        canonical.to_owned()
    }
}

#[must_use]
pub fn compat_display_type_name(type_name: &str) -> String {
    match normalize_compat_type_name(type_name).as_str() {
        "bool" => "boolean".to_owned(),
        "bytea" => "bytea".to_owned(),
        "int4" => "integer".to_owned(),
        "int8" => "bigint".to_owned(),
        "float4" => "real".to_owned(),
        "float8" => "double precision".to_owned(),
        "numeric" => "numeric".to_owned(),
        "money" => "money".to_owned(),
        "text" => "text".to_owned(),
        "date" => "date".to_owned(),
        "time" => "time without time zone".to_owned(),
        "timetz" => "time with time zone".to_owned(),
        "timestamp" => "timestamp without time zone".to_owned(),
        "timestamptz" => "timestamp with time zone".to_owned(),
        "interval" => "interval".to_owned(),
        "uuid" => "uuid".to_owned(),
        "jsonb" => "jsonb".to_owned(),
        "bit" => "bit".to_owned(),
        "varbit" => "bit varying".to_owned(),
        "tid" => "tid".to_owned(),
        "pg_lsn" => "pg_lsn".to_owned(),
        "vector" => "vector".to_owned(),
        "halfvec" => "halfvec".to_owned(),
        "sparsevec" => "sparsevec".to_owned(),
        "regclass" => "regclass".to_owned(),
        "regtype" => "regtype".to_owned(),
        "regproc" => "regproc".to_owned(),
        "regprocedure" => "regprocedure".to_owned(),
        "regoper" => "regoper".to_owned(),
        "regoperator" => "regoperator".to_owned(),
        "regnamespace" => "regnamespace".to_owned(),
        "regrole" => "regrole".to_owned(),
        "cstring" => "cstring".to_owned(),
        other => other.to_owned(),
    }
}

#[must_use]
pub fn compat_type_name_for_data_type(data_type: &DataType) -> String {
    match data_type {
        DataType::Int => "int4".to_owned(),
        DataType::BigInt => "int8".to_owned(),
        DataType::Real => "float4".to_owned(),
        DataType::Double => "float8".to_owned(),
        DataType::Numeric => "numeric".to_owned(),
        DataType::Money => "money".to_owned(),
        DataType::Text => "text".to_owned(),
        DataType::Boolean => "bool".to_owned(),
        DataType::Blob => "bytea".to_owned(),
        DataType::Timestamp => "timestamp".to_owned(),
        DataType::TimestampTz => "timestamptz".to_owned(),
        DataType::Date => "date".to_owned(),
        DataType::Time => "time".to_owned(),
        DataType::TimeTz => "timetz".to_owned(),
        DataType::Interval => "interval".to_owned(),
        DataType::Uuid => "uuid".to_owned(),
        DataType::Tid => "tid".to_owned(),
        DataType::PgLsn => "pg_lsn".to_owned(),
        DataType::Jsonb => "jsonb".to_owned(),
        DataType::MacAddr => "macaddr".to_owned(),
        DataType::MacAddr8 => "macaddr8".to_owned(),
        DataType::Vector {
            dims: 0,
            element_type: aiondb_core::VectorElementType::Float16,
        } => "halfvec".to_owned(),
        DataType::Vector {
            dims,
            element_type: aiondb_core::VectorElementType::Float16,
        } => format!("halfvec({dims})"),
        DataType::Vector {
            dims: 0,
            element_type: aiondb_core::VectorElementType::Float32,
        } => "vector".to_owned(),
        DataType::Vector {
            dims,
            element_type: aiondb_core::VectorElementType::Float32,
        } => format!("vector({dims})"),
        DataType::Vector { dims, element_type } => format!("vector({dims}, {element_type})"),
        DataType::Array(inner) => format!("{}[]", compat_type_name_for_data_type(inner)),
    }
}

#[must_use]
pub fn is_builtin_compat_type(type_name: &str) -> bool {
    let normalized = normalize_compat_type_name(type_name);
    let base = normalized.strip_suffix("[]").unwrap_or(&normalized);
    matches!(
        base,
        // Core scalar types
        "bool"
            | "bytea"
            | "int2"
            | "int4"
            | "int8"
            | "float4"
            | "float8"
            | "numeric"
            | "money"
            | "text"
            | "date"
            | "time"
            | "timetz"
            | "timestamp"
            | "timestamptz"
            | "interval"
            | "uuid"
            | "json"
            | "jsonb"
            | "xml"
            | "tid"
            | "oid"
            | "pg_lsn"
            // Name/char types
            | "name"
            | "char"
            | "bpchar"
            | "varchar"
            // Registration types
            | "regclass"
            | "regtype"
            | "regproc"
            | "regprocedure"
            | "regoper"
            | "regoperator"
            | "regnamespace"
            | "regrole"
            | "cstring"
            // Pseudo-types used as function return/parameter types
            | "void"
            | "trigger"
            | "event_trigger"
            | "internal"
            | "language_handler"
            | "fdw_handler"
            | "tsm_handler"
            | "table_am_handler"
            | "index_am_handler"
            | "record"
            | "pg_ddl_command"
            // Polymorphic types
            | "anyelement"
            | "anyarray"
            | "anynonarray"
            | "anyenum"
            | "anyrange"
            | "anymultirange"
            | "anycompatible"
            | "anycompatiblearray"
            | "anycompatiblerange"
            | "anycompatiblemultirange"
            // Vector/array helper types
            | "vector"
            | "halfvec"
            | "sparsevec"
            | "int2vector"
            | "oidvector"
            // Network types
            | "inet"
            | "cidr"
            | "macaddr"
            | "macaddr8"
            // Bit string types
            | "bit"
            | "varbit"
            // Geometric types
            | "point"
            | "line"
            | "lseg"
            | "box"
            | "path"
            | "polygon"
            | "circle"
            // Full-text search types
            | "tsvector"
            | "tsquery"
            // Cursor type
            | "refcursor"
    )
}

thread_local! {
    static SESSION_CONTEXT_STACK: RefCell<Vec<EvalSessionContext>> = const { RefCell::new(Vec::new()) };
}

static GLOBAL_COMPAT_INDEX_DEFS: OnceLock<Mutex<Arc<HashMap<i32, String>>>> = OnceLock::new();
static GLOBAL_COMPAT_CONSTRAINT_DEFS: OnceLock<Mutex<Arc<HashMap<i32, String>>>> = OnceLock::new();

struct SessionContextGuard;

impl Drop for SessionContextGuard {
    fn drop(&mut self) {
        SESSION_CONTEXT_STACK.with(|stack| {
            let _ = stack.borrow_mut().pop();
        });
    }
}

pub fn with_session_context<T>(context: EvalSessionContext, f: impl FnOnce() -> T) -> T {
    SESSION_CONTEXT_STACK.with(|stack| {
        stack.borrow_mut().push(context);
    });
    let _guard = SessionContextGuard;
    f()
}

fn with_current_session_context_ref<T>(f: impl FnOnce(&EvalSessionContext) -> T) -> T {
    SESSION_CONTEXT_STACK.with(|stack| {
        let stack = stack.borrow();
        if let Some(context) = stack.last() {
            return f(context);
        }

        static DEFAULT_CONTEXT: OnceLock<EvalSessionContext> = OnceLock::new();
        f(DEFAULT_CONTEXT.get_or_init(EvalSessionContext::default))
    })
}

pub fn with_current_session_context<T>(f: impl FnOnce(&EvalSessionContext) -> T) -> T {
    with_current_session_context_ref(f)
}

#[allow(clippy::implicit_hasher)]
pub fn set_global_compat_definition_caches(
    index_defs: Arc<HashMap<i32, String>>,
    constraint_defs: Arc<HashMap<i32, String>>,
) {
    let global_index_defs =
        GLOBAL_COMPAT_INDEX_DEFS.get_or_init(|| Mutex::new(Arc::new(HashMap::new())));
    if let Ok(mut guard) = global_index_defs.lock() {
        *guard = index_defs;
    }
    let global_constraint_defs =
        GLOBAL_COMPAT_CONSTRAINT_DEFS.get_or_init(|| Mutex::new(Arc::new(HashMap::new())));
    if let Ok(mut guard) = global_constraint_defs.lock() {
        *guard = constraint_defs;
    }
}

pub fn global_compat_index_defs() -> Arc<HashMap<i32, String>> {
    GLOBAL_COMPAT_INDEX_DEFS
        .get_or_init(|| Mutex::new(Arc::new(HashMap::new())))
        .lock()
        .map(|guard| Arc::clone(&guard))
        .unwrap_or_else(|_| Arc::new(HashMap::new()))
}

pub fn global_compat_constraint_defs() -> Arc<HashMap<i32, String>> {
    GLOBAL_COMPAT_CONSTRAINT_DEFS
        .get_or_init(|| Mutex::new(Arc::new(HashMap::new())))
        .lock()
        .map(|guard| Arc::clone(&guard))
        .unwrap_or_else(|_| Arc::new(HashMap::new()))
}

pub fn current_session_context() -> EvalSessionContext {
    with_current_session_context_ref(Clone::clone)
}

pub fn current_temporal_session_context() -> EvalTemporalSessionContext {
    with_current_session_context_ref(|context| EvalTemporalSessionContext {
        date_order: context.date_order,
        date_style: context.date_style,
        timezone: context.timezone.clone(),
    })
}

pub fn current_search_path_schemas() -> Arc<Vec<String>> {
    with_current_session_context_ref(|context| Arc::clone(&context.search_path_schemas))
}

pub fn current_time_zone() -> TimeZoneSetting {
    with_current_session_context_ref(|context| context.timezone.clone())
}

pub fn current_date_order() -> DateOrder {
    with_current_session_context_ref(|context| context.date_order)
}

pub fn current_interval_style() -> EvalIntervalStyle {
    with_current_session_context_ref(|context| context.interval_style)
}

pub fn current_schema_name() -> Option<String> {
    with_current_session_context_ref(|context| {
        context
            .current_schema
            .as_deref()
            .map(visible_session_schema_name)
    })
}

pub fn current_database_name() -> Option<String> {
    with_current_session_context_ref(|context| context.current_database.clone())
}

pub fn visible_session_schema_name(schema: &str) -> String {
    if schema.to_ascii_lowercase().starts_with("db_") {
        "public".to_owned()
    } else {
        schema.to_owned()
    }
}

pub fn current_lo_session_key() -> u64 {
    with_current_session_context_ref(|context| context.lo_session_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_european_postgres_datestyle() {
        let context = EvalSessionContext::from_settings(Some("European,Postgres"), Some("UTC"));
        assert_eq!(context.date_style, DateStyleFamily::Postgres);
        assert_eq!(context.date_order, DateOrder::Dmy);
        assert_eq!(context.timezone.show_value(), "UTC");
    }

    #[test]
    fn german_implies_dmy_order() {
        let context = EvalSessionContext::from_settings(Some("German"), Some("UTC"));
        assert_eq!(context.date_style, DateStyleFamily::German);
        assert_eq!(context.date_order, DateOrder::Dmy);
    }

    #[test]
    fn defaults_to_compat_settings() {
        let context = EvalSessionContext::default();
        assert_eq!(context.date_style, DateStyleFamily::Iso);
        assert_eq!(context.date_order, DateOrder::Mdy);
        assert!(!context.timezone.show_value().is_empty());
        assert_eq!(context.interval_style, EvalIntervalStyle::Postgres);
    }

    #[test]
    fn parses_sql_standard_intervalstyle() {
        let context = EvalSessionContext::from_settings_with_interval_style(
            Some("ISO, MDY"),
            Some("UTC"),
            Some("sql_standard"),
        );
        assert_eq!(context.interval_style, EvalIntervalStyle::SqlStandard);
    }
}
