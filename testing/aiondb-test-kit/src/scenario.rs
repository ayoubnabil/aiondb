use aiondb_engine::{DbError, Value};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqlScenario {
    pub name: String,
    pub database: String,
    pub user: String,
    pub password: Option<String>,
    pub setup_sql: Option<String>,
    pub verify_sql: Option<String>,
    pub expectation: ScenarioExpectation,
    pub operation: ScenarioOperation,
}

impl SqlScenario {
    pub fn new(name: impl Into<String>, sql: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            database: "default".to_owned(),
            user: "test-kit".to_owned(),
            password: None,
            setup_sql: None,
            verify_sql: None,
            expectation: ScenarioExpectation::Success,
            operation: ScenarioOperation::Simple { sql: sql.into() },
        }
    }

    pub fn prepared(
        name: impl Into<String>,
        sql: impl Into<String>,
        params: Vec<ScenarioValue>,
    ) -> Self {
        Self {
            name: name.into(),
            database: "default".to_owned(),
            user: "test-kit".to_owned(),
            password: None,
            setup_sql: None,
            verify_sql: None,
            expectation: ScenarioExpectation::Success,
            operation: ScenarioOperation::Prepared {
                sql: sql.into(),
                params,
                max_rows: 0,
            },
        }
    }

    pub fn with_setup_sql(mut self, sql: impl Into<String>) -> Self {
        self.setup_sql = Some(sql.into());
        self
    }

    pub fn with_database(mut self, database: impl Into<String>) -> Self {
        self.database = database.into();
        self
    }

    pub fn with_user(mut self, user: impl Into<String>) -> Self {
        self.user = user.into();
        self
    }

    pub fn with_cleartext_password(mut self, password: impl Into<String>) -> Self {
        self.password = Some(password.into());
        self
    }

    pub fn with_verify_sql(mut self, sql: impl Into<String>) -> Self {
        self.verify_sql = Some(sql.into());
        self
    }

    pub fn expect_error(mut self) -> Self {
        self.expectation = ScenarioExpectation::Error;
        self
    }

    #[must_use]
    pub fn with_max_rows(mut self, max_rows: usize) -> Self {
        if let ScenarioOperation::Prepared {
            max_rows: current, ..
        } = &mut self.operation
        {
            *current = max_rows;
        }
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScenarioOperation {
    Simple {
        sql: String,
    },
    Prepared {
        sql: String,
        params: Vec<ScenarioValue>,
        max_rows: usize,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScenarioValue {
    Null,
    Int(i32),
    BigInt(i64),
    Text(String),
    Boolean(bool),
}

impl ScenarioValue {
    pub(crate) fn to_engine_value(&self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Int(value) => Value::Int(*value),
            Self::BigInt(value) => Value::BigInt(*value),
            Self::Text(value) => Value::Text(value.clone()),
            Self::Boolean(value) => Value::Boolean(*value),
        }
    }

    pub(crate) fn to_text_bytes(&self) -> Option<Vec<u8>> {
        match self {
            Self::Null => None,
            Self::Int(value) => Some(value.to_string().into_bytes()),
            Self::BigInt(value) => Some(value.to_string().into_bytes()),
            Self::Text(value) => Some(value.clone().into_bytes()),
            Self::Boolean(value) => Some(if *value { "true" } else { "false" }.as_bytes().to_vec()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StatementOutcome {
    Query {
        columns: Vec<String>,
        rows: Vec<Vec<Option<String>>>,
        completion_tag: String,
    },
    Command {
        completion_tag: String,
    },
    EmptyQuery,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScenarioOutcome {
    Simple(Vec<StatementOutcome>),
    Prepared(PreparedOutcome),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScenarioResult {
    Success(ScenarioOutcome),
    Error(ScenarioError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScenarioExpectation {
    Success,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScenarioError {
    pub sqlstate: String,
    pub message: String,
    pub detail: Option<String>,
    pub hint: Option<String>,
    pub position: Option<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedOutcome {
    pub statement: PreparedStatementOutcome,
    pub portal: PortalOutcome,
    pub executions: Vec<ExecutionBatchOutcome>,
    pub verification: Option<Vec<StatementOutcome>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedStatementOutcome {
    pub param_type_oids: Vec<u32>,
    pub result_columns: Vec<ColumnOutcome>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PortalOutcome {
    pub result_columns: Vec<ColumnOutcome>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExecutionBatchOutcome {
    QuerySuspended {
        rows: Vec<Vec<Option<String>>>,
    },
    QueryComplete {
        rows: Vec<Vec<Option<String>>>,
        completion_tag: String,
    },
    CommandComplete {
        completion_tag: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ColumnOutcome {
    pub name: String,
    pub type_oid: u32,
    pub type_size: i16,
}

pub(crate) fn command_completion_tag(tag: &str, rows_affected: u64) -> String {
    match tag {
        // PostgreSQL INSERT completion tag always includes the OID (always 0 since PG 12).
        "INSERT" => format!("INSERT 0 {rows_affected}"),
        _ if rows_affected > 0 => format!("{tag} {rows_affected}"),
        _ => tag.to_owned(),
    }
}

pub(crate) fn scenario_error_from_db_error(error: &DbError) -> ScenarioError {
    let report = error.report();
    ScenarioError {
        sqlstate: report.sqlstate.code().to_owned(),
        message: report.message.clone(),
        detail: report.client_detail.clone(),
        hint: report.client_hint.clone(),
        position: report.position,
    }
}
