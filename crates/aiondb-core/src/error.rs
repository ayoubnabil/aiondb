use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum SqlState {
    SyntaxError,
    InvalidAuthorizationSpecification,
    InsufficientPrivilege,
    InvalidDatetimeFormat,
    InvalidCatalogName,
    ObjectNotInPrerequisiteState,
    UndefinedTable,
    UndefinedColumn,
    InvalidColumnReference,
    UndefinedFunction,
    UndefinedObject,
    UndefinedParameter,
    InvalidCursorName,
    InvalidCursorState,
    UniqueViolation,
    ForeignKeyViolation,
    NotNullViolation,
    CheckViolation,
    SerializationFailure,
    DeadlockDetected,
    LockNotAvailable,
    TooManyConnections,
    TooManyAuthenticationFailures,
    AdminShutdown,
    NoActiveSqlTransaction,
    IdleInTransactionSessionTimeout,
    InFailedSqlTransaction,
    QueryCanceled,
    IdleSessionTimeout,
    DependentObjectsStillExist,
    InvalidSavepointSpecification,
    ProgramLimitExceeded,
    InvalidSchemaName,
    DuplicateSchema,
    AmbiguousFunction,
    DatatypeMismatch,
    FeatureNotSupported,
    InvalidTextRepresentation,
    NumericValueOutOfRange,
    DivisionByZero,
    DatetimeFieldOverflow,
    StringDataRightTruncation,
    InvalidParameterValue,
    CaseNotFound,
    RaiseException,
    NoDataFound,
    TooManyRows,
    AssertFailure,
    DuplicateColumn,
    DuplicateObject,
    GroupingError,
    InvalidTableDefinition,
    WrongObjectType,
    InternalError,
}

impl SqlState {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::SyntaxError => "42601",
            Self::InvalidAuthorizationSpecification => "28000",
            Self::InsufficientPrivilege => "42501",
            Self::InvalidDatetimeFormat => "22007",
            Self::InvalidCatalogName => "3D000",
            Self::ObjectNotInPrerequisiteState => "55000",
            Self::UndefinedTable => "42P01",
            Self::UndefinedColumn => "42703",
            Self::InvalidColumnReference => "42P10",
            Self::UndefinedFunction => "42883",
            Self::UndefinedObject => "42704",
            Self::UndefinedParameter => "42P02",
            Self::InvalidCursorName => "34000",
            Self::InvalidCursorState => "24000",
            Self::UniqueViolation => "23505",
            Self::ForeignKeyViolation => "23503",
            Self::NotNullViolation => "23502",
            Self::CheckViolation => "23514",
            Self::SerializationFailure => "40001",
            Self::DeadlockDetected => "40P01",
            Self::LockNotAvailable => "55P03",
            Self::TooManyConnections => "53300",
            Self::TooManyAuthenticationFailures => "28P02",
            Self::AdminShutdown => "57P01",
            Self::NoActiveSqlTransaction => "25P01",
            Self::IdleInTransactionSessionTimeout => "25P03",
            Self::InFailedSqlTransaction => "25P02",
            Self::QueryCanceled => "57014",
            Self::IdleSessionTimeout => "57P05",
            Self::DependentObjectsStillExist => "2BP01",
            Self::InvalidSavepointSpecification => "3B001",
            Self::ProgramLimitExceeded => "54000",
            Self::InvalidSchemaName => "3F000",
            Self::DuplicateSchema => "42P06",
            Self::AmbiguousFunction => "42725",
            Self::DatatypeMismatch => "42804",
            Self::FeatureNotSupported => "0A000",
            Self::InvalidTextRepresentation => "22P02",
            Self::NumericValueOutOfRange => "22003",
            Self::DivisionByZero => "22012",
            Self::DatetimeFieldOverflow => "22008",
            Self::StringDataRightTruncation => "22001",
            Self::InvalidParameterValue => "22023",
            Self::CaseNotFound => "20000",
            Self::RaiseException => "P0001",
            Self::NoDataFound => "P0002",
            Self::TooManyRows => "P0003",
            Self::AssertFailure => "P0004",
            Self::DuplicateColumn => "42701",
            Self::DuplicateObject => "42710",
            Self::GroupingError => "42803",
            Self::InvalidTableDefinition => "42P16",
            Self::WrongObjectType => "42809",
            Self::InternalError => "XX000",
        }
    }

    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "42601" => Some(Self::SyntaxError),
            "28000" => Some(Self::InvalidAuthorizationSpecification),
            "42501" => Some(Self::InsufficientPrivilege),
            "22007" => Some(Self::InvalidDatetimeFormat),
            "3D000" => Some(Self::InvalidCatalogName),
            "55000" => Some(Self::ObjectNotInPrerequisiteState),
            "42P01" => Some(Self::UndefinedTable),
            "42703" => Some(Self::UndefinedColumn),
            "42P10" => Some(Self::InvalidColumnReference),
            "42883" => Some(Self::UndefinedFunction),
            "42704" => Some(Self::UndefinedObject),
            "42P02" => Some(Self::UndefinedParameter),
            "34000" => Some(Self::InvalidCursorName),
            "24000" => Some(Self::InvalidCursorState),
            "23505" => Some(Self::UniqueViolation),
            "23503" => Some(Self::ForeignKeyViolation),
            "23502" => Some(Self::NotNullViolation),
            "23514" => Some(Self::CheckViolation),
            "40001" => Some(Self::SerializationFailure),
            "40P01" => Some(Self::DeadlockDetected),
            "55P03" => Some(Self::LockNotAvailable),
            "53300" => Some(Self::TooManyConnections),
            "28P02" => Some(Self::TooManyAuthenticationFailures),
            "57P01" => Some(Self::AdminShutdown),
            "25P01" => Some(Self::NoActiveSqlTransaction),
            "25P03" => Some(Self::IdleInTransactionSessionTimeout),
            "25P02" => Some(Self::InFailedSqlTransaction),
            "57014" => Some(Self::QueryCanceled),
            "57P05" => Some(Self::IdleSessionTimeout),
            "2BP01" => Some(Self::DependentObjectsStillExist),
            "3B001" => Some(Self::InvalidSavepointSpecification),
            "54000" => Some(Self::ProgramLimitExceeded),
            "3F000" => Some(Self::InvalidSchemaName),
            "42P06" => Some(Self::DuplicateSchema),
            "42725" => Some(Self::AmbiguousFunction),
            "42804" => Some(Self::DatatypeMismatch),
            "0A000" => Some(Self::FeatureNotSupported),
            "22P02" => Some(Self::InvalidTextRepresentation),
            "22003" => Some(Self::NumericValueOutOfRange),
            "22012" => Some(Self::DivisionByZero),
            "22008" => Some(Self::DatetimeFieldOverflow),
            "22001" => Some(Self::StringDataRightTruncation),
            "22023" => Some(Self::InvalidParameterValue),
            "20000" => Some(Self::CaseNotFound),
            "P0001" => Some(Self::RaiseException),
            "P0002" => Some(Self::NoDataFound),
            "P0003" => Some(Self::TooManyRows),
            "P0004" => Some(Self::AssertFailure),
            "42701" => Some(Self::DuplicateColumn),
            "42710" => Some(Self::DuplicateObject),
            "42803" => Some(Self::GroupingError),
            "42P16" => Some(Self::InvalidTableDefinition),
            "42809" => Some(Self::WrongObjectType),
            "XX000" => Some(Self::InternalError),
            _ => None,
        }
    }
}

impl fmt::Display for SqlState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

#[derive(Clone, Debug)]
pub struct ErrorReport {
    pub sqlstate: SqlState,
    pub message: String,
    pub client_detail: Option<String>,
    pub client_hint: Option<String>,
    pub position: Option<usize>,
    pub internal_detail: Option<String>,
}

impl ErrorReport {
    #[must_use]
    pub fn new(sqlstate: SqlState, message: impl Into<String>) -> Self {
        Self {
            sqlstate,
            message: message.into(),
            client_detail: None,
            client_hint: None,
            position: None,
            internal_detail: None,
        }
    }

    #[must_use]
    pub fn with_position(mut self, position: usize) -> Self {
        self.position = Some(position);
        self
    }

    #[must_use]
    pub fn with_client_detail(mut self, detail: impl Into<String>) -> Self {
        self.client_detail = Some(detail.into());
        self
    }

    #[must_use]
    pub fn with_client_hint(mut self, hint: impl Into<String>) -> Self {
        self.client_hint = Some(hint.into());
        self
    }

    #[must_use]
    pub fn with_internal_detail(mut self, detail: impl Into<String>) -> Self {
        self.internal_detail = Some(detail.into());
        self
    }
}

#[derive(Clone, Debug)]
pub enum DbError {
    Parse(Box<ErrorReport>),
    Bind(Box<ErrorReport>),
    Authorization(Box<ErrorReport>),
    Constraint(Box<ErrorReport>),
    Transaction(Box<ErrorReport>),
    Storage(Box<ErrorReport>),
    Protocol(Box<ErrorReport>),
    Internal(Box<ErrorReport>),
}

impl DbError {
    #[must_use]
    pub fn from_report(report: ErrorReport) -> Self {
        Self::Internal(Box::new(report))
    }

    #[must_use]
    pub fn parse_error(sqlstate: SqlState, message: impl Into<String>) -> Self {
        Self::Parse(Box::new(ErrorReport::new(sqlstate, message)))
    }

    #[must_use]
    pub fn bind_error(sqlstate: SqlState, message: impl Into<String>) -> Self {
        Self::Bind(Box::new(ErrorReport::new(sqlstate, message)))
    }

    #[must_use]
    pub fn authorization_error(sqlstate: SqlState, message: impl Into<String>) -> Self {
        Self::Authorization(Box::new(ErrorReport::new(sqlstate, message)))
    }

    #[must_use]
    pub fn constraint_error(sqlstate: SqlState, message: impl Into<String>) -> Self {
        Self::Constraint(Box::new(ErrorReport::new(sqlstate, message)))
    }

    #[must_use]
    pub fn transaction_error(sqlstate: SqlState, message: impl Into<String>) -> Self {
        Self::Transaction(Box::new(ErrorReport::new(sqlstate, message)))
    }

    #[must_use]
    pub fn storage_error(sqlstate: SqlState, message: impl Into<String>) -> Self {
        Self::Storage(Box::new(ErrorReport::new(sqlstate, message)))
    }

    #[must_use]
    pub fn syntax_error(message: impl Into<String>) -> Self {
        Self::parse_error(SqlState::SyntaxError, message)
    }

    #[must_use]
    pub fn invalid_authorization(message: impl Into<String>) -> Self {
        Self::authorization_error(SqlState::InvalidAuthorizationSpecification, message)
    }

    #[must_use]
    pub fn insufficient_privilege(message: impl Into<String>) -> Self {
        Self::authorization_error(SqlState::InsufficientPrivilege, message)
    }

    #[must_use]
    pub fn query_canceled(message: impl Into<String>) -> Self {
        Self::transaction_error(SqlState::QueryCanceled, message)
    }

    #[must_use]
    pub fn program_limit(message: impl Into<String>) -> Self {
        Self::internal_error(SqlState::ProgramLimitExceeded, message)
    }

    #[must_use]
    pub fn protocol(message: impl Into<String>) -> Self {
        Self::Protocol(Box::new(ErrorReport::new(SqlState::InternalError, message)))
    }

    #[must_use]
    pub fn feature_not_supported(message: impl Into<String>) -> Self {
        Self::parse_error(SqlState::FeatureNotSupported, message)
    }

    #[must_use]
    pub fn internal(message: impl Into<String>) -> Self {
        Self::internal_error(SqlState::InternalError, message)
    }

    /// PG-compatible "invalid input syntax for type {`type_name}`: \"{value}\""
    /// SQLSTATE 22P02
    #[must_use]
    pub fn invalid_input_syntax(pg_type_name: &str, value: &str) -> Self {
        Self::internal_error(
            SqlState::InvalidTextRepresentation,
            format!("invalid input syntax for type {pg_type_name}: \"{value}\""),
        )
    }

    /// PG-compatible "invalid input syntax for type {`type_name}`: \"{value}\""
    /// SQLSTATE 22007 for date/time families.
    #[must_use]
    pub fn invalid_datetime_syntax(pg_type_name: &str, value: &str) -> Self {
        Self::internal_error(
            SqlState::InvalidDatetimeFormat,
            format!("invalid input syntax for type {pg_type_name}: \"{value}\""),
        )
    }

    /// PG-compatible 'value "{value}" is out of range for type {`type_name`}'
    /// SQLSTATE 22003. For `real` / `double precision` `PostgreSQL` drops the
    /// `value ` prefix and uses `"{value}" is out of range for type …`.
    #[must_use]
    pub fn out_of_range(pg_type_name: &str, value: &str) -> Self {
        let message = if matches!(pg_type_name, "real" | "double precision") {
            format!("\"{value}\" is out of range for type {pg_type_name}")
        } else {
            format!("value \"{value}\" is out of range for type {pg_type_name}")
        };
        Self::internal_error(SqlState::NumericValueOutOfRange, message)
    }

    /// PG-compatible "value too long for type {`type_name`}"
    /// SQLSTATE 22001
    #[must_use]
    pub fn value_too_long_for_type(pg_type_name: &str) -> Self {
        Self::internal_error(
            SqlState::StringDataRightTruncation,
            format!("value too long for type {pg_type_name}"),
        )
    }

    #[must_use]
    pub fn report(&self) -> &ErrorReport {
        match self {
            Self::Parse(report)
            | Self::Bind(report)
            | Self::Authorization(report)
            | Self::Constraint(report)
            | Self::Transaction(report)
            | Self::Storage(report)
            | Self::Protocol(report)
            | Self::Internal(report) => report,
        }
    }

    #[must_use]
    pub fn sqlstate(&self) -> SqlState {
        self.report().sqlstate
    }

    #[must_use]
    pub fn with_position(mut self, position: usize) -> Self {
        self.report_mut().position = Some(position);
        self
    }

    #[must_use]
    pub fn with_client_detail(mut self, detail: impl Into<String>) -> Self {
        self.report_mut().client_detail = Some(detail.into());
        self
    }

    #[must_use]
    pub fn with_client_hint(mut self, hint: impl Into<String>) -> Self {
        self.report_mut().client_hint = Some(hint.into());
        self
    }

    #[must_use]
    pub fn with_internal_detail(mut self, detail: impl Into<String>) -> Self {
        self.report_mut().internal_detail = Some(detail.into());
        self
    }

    /// Returns `true` when the error represents a concurrency conflict
    /// (serialization failure, deadlock, or lock timeout).  Use this instead
    /// of string-matching on the error message.
    #[must_use]
    pub fn is_concurrency_error(&self) -> bool {
        matches!(
            self.sqlstate(),
            SqlState::SerializationFailure
                | SqlState::DeadlockDetected
                | SqlState::LockNotAvailable
        )
    }

    fn internal_error(sqlstate: SqlState, message: impl Into<String>) -> Self {
        Self::Internal(Box::new(ErrorReport::new(sqlstate, message)))
    }

    fn report_mut(&mut self) -> &mut ErrorReport {
        match self {
            Self::Parse(report)
            | Self::Bind(report)
            | Self::Authorization(report)
            | Self::Constraint(report)
            | Self::Transaction(report)
            | Self::Storage(report)
            | Self::Protocol(report)
            | Self::Internal(report) => report.as_mut(),
        }
    }
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let report = self.report();
        write!(f, "{} ({})", report.message, report.sqlstate)
    }
}

impl std::error::Error for DbError {}

pub type DbResult<T> = Result<T, DbError>;

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, error::Error as _};

    use super::*;

    const SQLSTATE_CASES: &[(SqlState, &str)] = &[
        (SqlState::SyntaxError, "42601"),
        (SqlState::InvalidAuthorizationSpecification, "28000"),
        (SqlState::InsufficientPrivilege, "42501"),
        (SqlState::InvalidDatetimeFormat, "22007"),
        (SqlState::InvalidCatalogName, "3D000"),
        (SqlState::ObjectNotInPrerequisiteState, "55000"),
        (SqlState::UndefinedTable, "42P01"),
        (SqlState::UndefinedColumn, "42703"),
        (SqlState::UndefinedFunction, "42883"),
        (SqlState::UndefinedObject, "42704"),
        (SqlState::UndefinedParameter, "42P02"),
        (SqlState::InvalidCursorName, "34000"),
        (SqlState::InvalidCursorState, "24000"),
        (SqlState::UniqueViolation, "23505"),
        (SqlState::ForeignKeyViolation, "23503"),
        (SqlState::CheckViolation, "23514"),
        (SqlState::SerializationFailure, "40001"),
        (SqlState::DeadlockDetected, "40P01"),
        (SqlState::LockNotAvailable, "55P03"),
        (SqlState::TooManyConnections, "53300"),
        (SqlState::TooManyAuthenticationFailures, "28P02"),
        (SqlState::AdminShutdown, "57P01"),
        (SqlState::NoActiveSqlTransaction, "25P01"),
        (SqlState::IdleInTransactionSessionTimeout, "25P03"),
        (SqlState::InFailedSqlTransaction, "25P02"),
        (SqlState::QueryCanceled, "57014"),
        (SqlState::IdleSessionTimeout, "57P05"),
        (SqlState::DependentObjectsStillExist, "2BP01"),
        (SqlState::InvalidSavepointSpecification, "3B001"),
        (SqlState::ProgramLimitExceeded, "54000"),
        (SqlState::DuplicateSchema, "42P06"),
        (SqlState::AmbiguousFunction, "42725"),
        (SqlState::InvalidTextRepresentation, "22P02"),
        (SqlState::NumericValueOutOfRange, "22003"),
        (SqlState::InvalidParameterValue, "22023"),
        (SqlState::InternalError, "XX000"),
    ];

    fn sample_report(sqlstate: SqlState, message: &str) -> ErrorReport {
        ErrorReport::new(sqlstate, message)
            .with_position(42)
            .with_client_detail("detail")
            .with_client_hint("hint")
            .with_internal_detail("internal")
    }

    #[test]
    fn sqlstate_codes_are_unique_and_match_display() {
        let mut seen_codes = HashSet::new();

        for &(state, code) in SQLSTATE_CASES {
            assert_eq!(state.code(), code);
            assert_eq!(SqlState::from_code(code), Some(state));
            assert_eq!(state.to_string(), code);
            assert_eq!(code.len(), 5);
            assert!(seen_codes.insert(code), "duplicate SQLSTATE code {code}");
        }

        assert_eq!(SqlState::from_code("00000"), None);
    }

    #[test]
    fn error_report_new_starts_with_empty_optional_fields() {
        let report = ErrorReport::new(SqlState::SyntaxError, "bad syntax");

        assert_eq!(report.sqlstate, SqlState::SyntaxError);
        assert_eq!(report.message, "bad syntax");
        assert_eq!(report.position, None);
        assert_eq!(report.client_detail, None);
        assert_eq!(report.client_hint, None);
        assert_eq!(report.internal_detail, None);
    }

    #[test]
    fn error_report_builders_set_and_override_fields() {
        let report = ErrorReport::new(SqlState::UndefinedColumn, "missing column")
            .with_position(7)
            .with_client_detail("first detail")
            .with_client_hint("first hint")
            .with_internal_detail("first internal")
            .with_position(9)
            .with_client_detail("final detail")
            .with_client_hint("final hint")
            .with_internal_detail("final internal");

        assert_eq!(report.position, Some(9));
        assert_eq!(report.client_detail.as_deref(), Some("final detail"));
        assert_eq!(report.client_hint.as_deref(), Some("final hint"));
        assert_eq!(report.internal_detail.as_deref(), Some("final internal"));
    }

    #[test]
    fn dberror_constructors_assign_expected_sqlstates() {
        let cases = [
            (DbError::syntax_error("bad syntax"), SqlState::SyntaxError),
            (
                DbError::invalid_authorization("bad login"),
                SqlState::InvalidAuthorizationSpecification,
            ),
            (
                DbError::insufficient_privilege("denied"),
                SqlState::InsufficientPrivilege,
            ),
            (
                DbError::invalid_datetime_syntax("interval", "bad"),
                SqlState::InvalidDatetimeFormat,
            ),
            (DbError::query_canceled("cancel"), SqlState::QueryCanceled),
            (
                DbError::program_limit("too many"),
                SqlState::ProgramLimitExceeded,
            ),
            (DbError::protocol("wire error"), SqlState::InternalError),
            (DbError::internal("bug"), SqlState::InternalError),
        ];

        for (error, sqlstate) in cases {
            assert_eq!(error.sqlstate(), sqlstate);
        }
    }

    #[test]
    fn dberror_report_and_sqlstate_forward_to_inner_report() {
        let cases = [
            DbError::Parse(Box::new(sample_report(SqlState::SyntaxError, "parse"))),
            DbError::Bind(Box::new(sample_report(SqlState::UndefinedColumn, "bind"))),
            DbError::Authorization(Box::new(sample_report(
                SqlState::InvalidAuthorizationSpecification,
                "auth",
            ))),
            DbError::Constraint(Box::new(sample_report(
                SqlState::UniqueViolation,
                "constraint",
            ))),
            DbError::Transaction(Box::new(sample_report(
                SqlState::SerializationFailure,
                "transaction",
            ))),
            DbError::Storage(Box::new(sample_report(SqlState::InternalError, "storage"))),
            DbError::Protocol(Box::new(sample_report(SqlState::InternalError, "protocol"))),
            DbError::Internal(Box::new(sample_report(SqlState::InternalError, "internal"))),
        ];

        for error in cases {
            let report = error.report();
            assert_eq!(error.sqlstate(), report.sqlstate);
            assert_eq!(report.position, Some(42));
            assert_eq!(report.client_detail.as_deref(), Some("detail"));
            assert_eq!(report.client_hint.as_deref(), Some("hint"));
            assert_eq!(report.internal_detail.as_deref(), Some("internal"));
        }
    }

    #[test]
    fn dberror_display_renders_message_and_sqlstate_code() {
        let error = DbError::Constraint(Box::new(ErrorReport::new(
            SqlState::UniqueViolation,
            "duplicate key value violates unique constraint",
        )));

        assert_eq!(
            error.to_string(),
            "duplicate key value violates unique constraint (23505)"
        );
    }

    #[test]
    fn dberror_builders_update_inner_report() {
        let error = DbError::bind_error(SqlState::UndefinedColumn, "missing column")
            .with_position(17)
            .with_client_detail("detail")
            .with_client_hint("hint")
            .with_internal_detail("internal");

        let report = error.report();
        assert_eq!(report.sqlstate, SqlState::UndefinedColumn);
        assert_eq!(report.position, Some(17));
        assert_eq!(report.client_detail.as_deref(), Some("detail"));
        assert_eq!(report.client_hint.as_deref(), Some("hint"));
        assert_eq!(report.internal_detail.as_deref(), Some("internal"));
    }

    #[test]
    fn dberror_from_report_preserves_report_fields() {
        let report = sample_report(SqlState::UndefinedTable, "missing table");
        let error = DbError::from_report(report.clone());

        assert_eq!(error.report().sqlstate, report.sqlstate);
        assert_eq!(error.report().message, report.message);
        assert_eq!(error.report().position, report.position);
        assert_eq!(error.report().client_detail, report.client_detail);
        assert_eq!(error.report().client_hint, report.client_hint);
        assert_eq!(error.report().internal_detail, report.internal_detail);
    }

    #[test]
    fn dberror_implements_std_error_without_source() {
        let error = DbError::internal("boom");
        let boxed: Box<dyn std::error::Error> = Box::new(error.clone());

        assert_eq!(boxed.to_string(), "boom (XX000)");
        assert!(error.source().is_none());
    }
}
