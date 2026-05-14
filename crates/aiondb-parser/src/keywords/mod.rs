#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Keyword {
    Add,
    Analyze,
    Alter,
    Backup,
    Array,
    And,
    Asc,
    As,
    Between,
    BigInt,
    Begin,
    Blob,
    By,
    Case,
    Cast,
    Column,
    Copy,
    Delete,
    Default,
    Distinct,
    Committed,
    Commit,
    Conflict,
    Create,
    Database,
    Date,
    Decimal,
    Double,
    Desc,
    Do,
    Drop,
    Else,
    End,
    False,
    Filter,
    From,
    Group,
    Having,
    If,
    Ilike,
    In,
    Index,
    Int,
    Insert,
    Is,
    Isolation,
    Into,
    Like,
    Limit,
    Level,
    Numeric,
    Not,
    Null,
    On,
    Order,
    Read,
    Real,
    Restore,
    Rollback,
    Sequence,
    Select,
    Set,
    Snapshot,
    Start,
    Stdin,
    Stdout,
    Or,
    Table,
    Text,
    Then,
    Time,
    Timestamp,
    Transaction,
    True,
    Update,
    Values,
    When,
    Where,
    Boolean,
    Interval,
    Offset,
    Coalesce,
    Cross,
    Full,
    Inner,
    Join,
    Jsonb,
    Left,
    Nullif,
    Outer,
    Right,
    Union,
    Intersect,
    Except,
    Exists,
    All,
    Check,
    Constraint,
    Explain,
    Foreign,
    Key,
    Primary,
    References,
    Release,
    Rename,
    Replace,
    Savepoint,
    Timestamptz,
    To,
    Unique,
    Uuid,
    Vacuum,
    Vector,
    View,
    Node,
    Edge,
    Label,
    Source,
    Target,
    Tenant,
    Grant,
    Revoke,
    Role,
    Login,
    Password,
    Superuser,
    Nosuperuser,
    Nologin,
    Nothing,
    Over,
    Partition,
    Privileges,
    Schema,
    Statistics,
    Using,
    With,
    Function,
    Language,
    Returns,
    Trigger,
    Before,
    After,
    Execute,
    Each,
    Row,
    Procedure,
    Truncate,
    Returning,
    Type,
    Data,
    Show,
    Reset,
    Zone,
    Cascade,
    Comment,
    Discard,
    Exclude,
    Fetch,
    First,
    Following,
    Groups,
    Last,
    Aggregate,
    Domain,
    Lock,
    Next,
    Nulls,
    Operator,
    Preceding,
    Range,
    Reindex,
    Rows,
    Unbounded,
    Within,
    // PG type aliases
    Bool,
    Bytea,
    Char,
    Character,
    Float,
    Float4,
    Float8,
    Int2,
    Int4,
    Int8,
    Integer,
    Serial,
    BigSerial,
    SmallInt,
    Varchar,
    Varying,
    Precision,
    // TEMP / TEMPORARY
    Temporary,
    Temp,
    Local,
    Global,
    // Extra SQL keywords
    Only,
    Lateral,
    Natural,
    Any,
    Some,
    For,
    No,
    Action,
    Restrict,
    Recursive,
    Materialized,
    Refresh,
    Concurrently,
    Without,
    Oids,
    Inherit,
    Inherits,
    Of,
    Owned,
    Owner,
    Unlogged,
    Logged,
    Work,
    Chain,
    Deferrable,
    Deferred,
    Immediate,
    Initially,
    Enable,
    Disable,
    Replica,
    Always,
    Identity,
    Generated,
    Stored,
    Increment,
    Minvalue,
    Maxvalue,
    Cycle,
    Cache,
    Overriding,
    System,
    User,
    Value,
    Current,
    Session,
    Authorization,
    None,
    Setof,
    Bit,
    Timetz,
    Merge,
    Matched,
    // Cypher keywords
    Match,
    Return,
    Optional,
    Detach,
    Unwind,
    Remove,
    Yield,
    Skip,
    Call,
    Foreach,
    Profile,
    FieldTerminator,
    Headers,
    Periodic,
    Load,
    Csv,
}

impl Keyword {
    /// Returns `true` if this keyword is unreserved and can be used as an
    /// identifier (table name, column name, etc.) without quoting.
    pub const fn is_unreserved(self) -> bool {
        matches!(
            self,
            Self::Backup
                | Self::Database
                | Self::Filter
                | Self::Node
                | Self::Edge
                | Self::Label
                | Self::Key
                | Self::Over
                | Self::Partition
                | Self::Rename
                | Self::Replace
                | Self::Restore
                | Self::Source
                | Self::Target
                | Self::Tenant
                | Self::Role
                | Self::Login
                | Self::Password
                | Self::Superuser
                | Self::Nosuperuser
                | Self::Nologin
                | Self::Privileges
                | Self::Schema
                | Self::Statistics
                | Self::Using
                | Self::Function
                | Self::If
                | Self::Language
                | Self::Returns
                | Self::Trigger
                | Self::Before
                | Self::After
                | Self::Execute
                | Self::Each
                | Self::Row
                | Self::Procedure
                | Self::Conflict
                | Self::Do
                | Self::Nothing
                | Self::Type
                | Self::Data
                | Self::Zone
                | Self::Cascade
                | Self::Only
                | Self::Recursive
                | Self::Materialized
                | Self::Refresh
                | Self::Concurrently
                | Self::Without
                | Self::Oids
                | Self::Inherit
                | Self::Inherits
                | Self::Of
                | Self::Owned
                | Self::Owner
                | Self::Unlogged
                | Self::Logged
                | Self::Work
                | Self::Chain
                | Self::Deferrable
                | Self::Deferred
                | Self::Immediate
                | Self::Initially
                | Self::Enable
                | Self::Disable
                | Self::Replica
                | Self::Always
                | Self::Identity
                | Self::Generated
                | Self::Stored
                | Self::Overriding
                | Self::System
                | Self::Value
                | Self::Current
                | Self::Session
                | Self::Authorization
                | Self::None
                | Self::Varying
                | Self::Precision
                | Self::Local
                | Self::Global
                | Self::No
                | Self::Action
                | Self::Restrict
                | Self::Temp
                | Self::Temporary
                | Self::Lateral
                | Self::Natural
                | Self::Any
                | Self::Some
                | Self::Float
                | Self::Float4
                | Self::Float8
                | Self::Int2
                | Self::Int4
                | Self::Int8
                | Self::Integer
                | Self::SmallInt
                | Self::Serial
                | Self::BigSerial
                | Self::Bool
                | Self::Bytea
                | Self::Char
                | Self::Character
                | Self::Varchar
                | Self::User
                | Self::Aggregate
                | Self::Comment
                | Self::Discard
                | Self::Domain
                | Self::Exclude
                | Self::Fetch
                | Self::First
                | Self::Following
                | Self::Groups
                | Self::Last
                | Self::Lock
                | Self::Next
                | Self::Nulls
                | Self::Operator
                | Self::Preceding
                | Self::Range
                | Self::Reindex
                | Self::Rows
                | Self::Unbounded
                | Self::Within
                // Type-name keywords - PostgreSQL allows these as identifiers
                | Self::Int
                | Self::BigInt
                | Self::Text
                | Self::Boolean
                | Self::Real
                | Self::Numeric
                | Self::Decimal
                | Self::Double
                | Self::Timestamp
                | Self::Timestamptz
                | Self::Time
                | Self::Date
                | Self::Interval
                | Self::Uuid
                | Self::Jsonb
                | Self::Blob
                | Self::Vector
                | Self::Setof
                | Self::Bit
                | Self::Timetz
                // Keywords that may appear as column names or function names
                // These don't start clauses in SELECT context
                | Self::Level
                | Self::Committed
                | Self::Snapshot
                | Self::Isolation
                | Self::Sequence
                | Self::Index
                | Self::Stdin
                | Self::Stdout
                | Self::Start
                | Self::Increment
                | Self::Savepoint
                | Self::Transaction
                | Self::Matched
                | Self::Match
                | Self::Optional
                | Self::Detach
                | Self::Unwind
                | Self::Remove
                | Self::Yield
                | Self::Skip
                | Self::Call
                | Self::Return
                | Self::Foreach
                | Self::Profile
                | Self::FieldTerminator
                | Self::Headers
                | Self::Periodic
                | Self::Load
                | Self::Csv
        )
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Add => "ADD",
            Self::Analyze => "ANALYZE",
            Self::Alter => "ALTER",
            Self::Array => "ARRAY",
            Self::And => "AND",
            Self::Asc => "ASC",
            Self::As => "AS",
            Self::Backup => "BACKUP",
            Self::Between => "BETWEEN",
            Self::BigInt => "BIGINT",
            Self::Begin => "BEGIN",
            Self::Blob => "BLOB",
            Self::By => "BY",
            Self::Case => "CASE",
            Self::Cast => "CAST",
            Self::Column => "COLUMN",
            Self::Copy => "COPY",
            Self::Delete => "DELETE",
            Self::Default => "DEFAULT",
            Self::Desc => "DESC",
            Self::Distinct => "DISTINCT",
            Self::Do => "DO",
            Self::Committed => "COMMITTED",
            Self::Commit => "COMMIT",
            Self::Conflict => "CONFLICT",
            Self::Create => "CREATE",
            Self::Database => "DATABASE",
            Self::Date => "DATE",
            Self::Decimal => "DECIMAL",
            Self::Boolean => "BOOLEAN",
            Self::Double => "DOUBLE",
            Self::Drop => "DROP",
            Self::Else => "ELSE",
            Self::End => "END",
            Self::False => "FALSE",
            Self::Filter => "FILTER",
            Self::From => "FROM",
            Self::Group => "GROUP",
            Self::Having => "HAVING",
            Self::If => "IF",
            Self::Ilike => "ILIKE",
            Self::In => "IN",
            Self::Index => "INDEX",
            Self::Int => "INT",
            Self::Interval => "INTERVAL",
            Self::Insert => "INSERT",
            Self::Is => "IS",
            Self::Isolation => "ISOLATION",
            Self::Into => "INTO",
            Self::Like => "LIKE",
            Self::Limit => "LIMIT",
            Self::Level => "LEVEL",
            Self::Numeric => "NUMERIC",
            Self::Not => "NOT",
            Self::Null => "NULL",
            Self::On => "ON",
            Self::Order => "ORDER",
            Self::Read => "READ",
            Self::Real => "REAL",
            Self::Restore => "RESTORE",
            Self::Rollback => "ROLLBACK",
            Self::Sequence => "SEQUENCE",
            Self::Select => "SELECT",
            Self::Set => "SET",
            Self::Snapshot => "SNAPSHOT",
            Self::Start => "START",
            Self::Stdin => "STDIN",
            Self::Stdout => "STDOUT",
            Self::Or => "OR",
            Self::Table => "TABLE",
            Self::Text => "TEXT",
            Self::Then => "THEN",
            Self::Time => "TIME",
            Self::Timestamp => "TIMESTAMP",
            Self::Transaction => "TRANSACTION",
            Self::True => "TRUE",
            Self::Update => "UPDATE",
            Self::Values => "VALUES",
            Self::When => "WHEN",
            Self::Where => "WHERE",
            Self::Offset => "OFFSET",
            Self::Coalesce => "COALESCE",
            Self::Cross => "CROSS",
            Self::Full => "FULL",
            Self::Inner => "INNER",
            Self::Join => "JOIN",
            Self::Jsonb => "JSONB",
            Self::Left => "LEFT",
            Self::Nullif => "NULLIF",
            Self::Outer => "OUTER",
            Self::Right => "RIGHT",
            Self::Union => "UNION",
            Self::Intersect => "INTERSECT",
            Self::Except => "EXCEPT",
            Self::Exists => "EXISTS",
            Self::All => "ALL",
            Self::Check => "CHECK",
            Self::Constraint => "CONSTRAINT",
            Self::Explain => "EXPLAIN",
            Self::Foreign => "FOREIGN",
            Self::Key => "KEY",
            Self::Primary => "PRIMARY",
            Self::References => "REFERENCES",
            Self::Release => "RELEASE",
            Self::Rename => "RENAME",
            Self::Replace => "REPLACE",
            Self::Savepoint => "SAVEPOINT",
            Self::Timestamptz => "TIMESTAMPTZ",
            Self::To => "TO",
            Self::Unique => "UNIQUE",
            Self::Uuid => "UUID",
            Self::Vacuum => "VACUUM",
            Self::Vector => "VECTOR",
            Self::View => "VIEW",
            Self::Node => "NODE",
            Self::Edge => "EDGE",
            Self::Label => "LABEL",
            Self::Source => "SOURCE",
            Self::Target => "TARGET",
            Self::Tenant => "TENANT",
            Self::Grant => "GRANT",
            Self::Revoke => "REVOKE",
            Self::Role => "ROLE",
            Self::Login => "LOGIN",
            Self::Password => "PASSWORD",
            Self::Superuser => "SUPERUSER",
            Self::Nosuperuser => "NOSUPERUSER",
            Self::Nologin => "NOLOGIN",
            Self::Nothing => "NOTHING",
            Self::Over => "OVER",
            Self::Partition => "PARTITION",
            Self::Privileges => "PRIVILEGES",
            Self::Schema => "SCHEMA",
            Self::Statistics => "STATISTICS",
            Self::Using => "USING",
            Self::With => "WITH",
            Self::Function => "FUNCTION",
            Self::Language => "LANGUAGE",
            Self::Returns => "RETURNS",
            Self::Trigger => "TRIGGER",
            Self::Before => "BEFORE",
            Self::After => "AFTER",
            Self::Execute => "EXECUTE",
            Self::Each => "EACH",
            Self::Row => "ROW",
            Self::Procedure => "PROCEDURE",
            Self::Truncate => "TRUNCATE",
            Self::Returning => "RETURNING",
            Self::Type => "TYPE",
            Self::Data => "DATA",
            Self::Show => "SHOW",
            Self::Reset => "RESET",
            Self::Zone => "ZONE",
            Self::Cascade => "CASCADE",
            Self::Comment => "COMMENT",
            Self::Discard => "DISCARD",
            Self::Exclude => "EXCLUDE",
            Self::Fetch => "FETCH",
            Self::First => "FIRST",
            Self::Following => "FOLLOWING",
            Self::Groups => "GROUPS",
            Self::Aggregate => "AGGREGATE",
            Self::Domain => "DOMAIN",
            Self::Last => "LAST",
            Self::Lock => "LOCK",
            Self::Next => "NEXT",
            Self::Operator => "OPERATOR",
            Self::Nulls => "NULLS",
            Self::Preceding => "PRECEDING",
            Self::Range => "RANGE",
            Self::Reindex => "REINDEX",
            Self::Rows => "ROWS",
            Self::Unbounded => "UNBOUNDED",
            Self::Within => "WITHIN",
            Self::Bool => "BOOL",
            Self::Bytea => "BYTEA",
            Self::Char => "CHAR",
            Self::Character => "CHARACTER",
            Self::Float => "FLOAT",
            Self::Float4 => "FLOAT4",
            Self::Float8 => "FLOAT8",
            Self::Int2 => "INT2",
            Self::Int4 => "INT4",
            Self::Int8 => "INT8",
            Self::Integer => "INTEGER",
            Self::Serial => "SERIAL",
            Self::BigSerial => "BIGSERIAL",
            Self::SmallInt => "SMALLINT",
            Self::Varchar => "VARCHAR",
            Self::Varying => "VARYING",
            Self::Precision => "PRECISION",
            Self::Temporary => "TEMPORARY",
            Self::Temp => "TEMP",
            Self::Local => "LOCAL",
            Self::Global => "GLOBAL",
            Self::Only => "ONLY",
            Self::Lateral => "LATERAL",
            Self::Natural => "NATURAL",
            Self::Any => "ANY",
            Self::Some => "SOME",
            Self::For => "FOR",
            Self::No => "NO",
            Self::Action => "ACTION",
            Self::Restrict => "RESTRICT",
            Self::Recursive => "RECURSIVE",
            Self::Materialized => "MATERIALIZED",
            Self::Refresh => "REFRESH",
            Self::Concurrently => "CONCURRENTLY",
            Self::Without => "WITHOUT",
            Self::Oids => "OIDS",
            Self::Inherit => "INHERIT",
            Self::Inherits => "INHERITS",
            Self::Of => "OF",
            Self::Owned => "OWNED",
            Self::Owner => "OWNER",
            Self::Unlogged => "UNLOGGED",
            Self::Logged => "LOGGED",
            Self::Work => "WORK",
            Self::Chain => "CHAIN",
            Self::Deferrable => "DEFERRABLE",
            Self::Deferred => "DEFERRED",
            Self::Immediate => "IMMEDIATE",
            Self::Initially => "INITIALLY",
            Self::Enable => "ENABLE",
            Self::Disable => "DISABLE",
            Self::Replica => "REPLICA",
            Self::Always => "ALWAYS",
            Self::Identity => "IDENTITY",
            Self::Generated => "GENERATED",
            Self::Stored => "STORED",
            Self::Increment => "INCREMENT",
            Self::Minvalue => "MINVALUE",
            Self::Maxvalue => "MAXVALUE",
            Self::Cycle => "CYCLE",
            Self::Cache => "CACHE",
            Self::Overriding => "OVERRIDING",
            Self::System => "SYSTEM",
            Self::User => "USER",
            Self::Value => "VALUE",
            Self::Current => "CURRENT",
            Self::Session => "SESSION",
            Self::Authorization => "AUTHORIZATION",
            Self::None => "NONE",
            Self::Setof => "SETOF",
            Self::Bit => "BIT",
            Self::Timetz => "TIMETZ",
            Self::Merge => "MERGE",
            Self::Matched => "MATCHED",
            Self::Match => "MATCH",
            Self::Return => "RETURN",
            Self::Optional => "OPTIONAL",
            Self::Detach => "DETACH",
            Self::Unwind => "UNWIND",
            Self::Remove => "REMOVE",
            Self::Yield => "YIELD",
            Self::Skip => "SKIP",
            Self::Call => "CALL",
            Self::Foreach => "FOREACH",
            Self::Profile => "PROFILE",
            Self::FieldTerminator => "FIELDTERMINATOR",
            Self::Headers => "HEADERS",
            Self::Periodic => "PERIODIC",
            Self::Load => "LOAD",
            Self::Csv => "CSV",
        }
    }
}

mod lookup;
pub use lookup::lookup_keyword;

#[cfg(test)]
mod tests;
