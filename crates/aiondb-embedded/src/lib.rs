//! Embedded Rust API for AionDB.
//!
//! This crate provides a synchronous in-process facade around `aiondb-engine`.
//! It is intended for applications that want to embed the database without
//! speaking PostgreSQL wire protocol.
//!
//! # Quick example
//!
//! ```rust,no_run
//! use aiondb_embedded::Database;
//!
//! fn main() -> aiondb_embedded::DbResult<()> {
//!     let db = Database::in_memory()?;
//!     let conn = db.connect_anonymous("default", "app")?;
//!     conn.execute("CREATE TABLE t (id INT PRIMARY KEY, v TEXT);")?;
//!     conn.execute("INSERT INTO t VALUES (1, 'hello');")?;
//!     let _ = conn.execute("SELECT * FROM t;")?;
//!     Ok(())
//! }
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracing::warn;

pub use aiondb_config::RuntimeConfig;
pub use aiondb_engine::{
    Credential, DbResult, Engine, IsolationLevel, PortalBatch, PreparedStatementDesc, QueryEngine,
    ResultColumn, SessionHandle, SessionInfo, StatementResult, Value,
};

use aiondb_engine::{EngineBuilder, StartupParams, TransportInfo, TransportKind};
use aiondb_security::AllowAllAuthorizer;

/// Connection startup options for embedded sessions.
#[derive(Debug)]
pub struct ConnectOptions {
    /// Target database name.
    pub database: String,
    /// Authentication credential used for startup.
    pub credential: Credential,
    /// Optional `application_name` session setting.
    pub application_name: Option<String>,
}

impl ConnectOptions {
    /// Build anonymous options for in-process local usage.
    #[must_use]
    pub fn anonymous(database: impl Into<String>, user: impl Into<String>) -> Self {
        Self {
            database: database.into(),
            credential: Credential::Anonymous { user: user.into() },
            application_name: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OpenProfile {
    /// Pure in-memory profile (no persistence).
    InMemory,
    /// Durable profile rooted at `data_dir`.
    Durable { data_dir: PathBuf },
    /// Durable profile with explicit runtime configuration.
    DurableWithConfig {
        data_dir: PathBuf,
        runtime_config: Box<RuntimeConfig>,
    },
}

pub struct Database<E: QueryEngine> {
    engine: Arc<E>,
}

impl Database<Engine> {
    fn build_trusted_engine(builder: EngineBuilder) -> DbResult<Self> {
        let engine = builder
            .with_authorizer(Arc::new(AllowAllAuthorizer))
            .with_allow_ephemeral_users(true)
            .build()?;
        Ok(Self {
            engine: Arc::new(engine),
        })
    }

    /// Open a database with durable WAL-backed storage at `data_dir`.
    ///
    /// WAL segment files are stored under `<data_dir>/wal/`. Data committed to
    /// this database survives process restarts. The embedded profile trusts
    /// in-process callers and allows anonymous local users by default.
    ///
    /// # Errors
    ///
    /// Returns an error if the WAL directory cannot be created or opened.
    pub fn open(data_dir: impl AsRef<Path>) -> DbResult<Self> {
        Self::open_with_profile(OpenProfile::Durable {
            data_dir: data_dir.as_ref().to_path_buf(),
        })
    }

    /// Create an in-memory database with no persistence.
    ///
    /// **For test and development use only.** Data is lost when the
    /// `Database` is dropped or the process exits. The embedded profile trusts
    /// in-process callers and allows anonymous local users by default.
    pub fn in_memory() -> DbResult<Self> {
        Self::open_with_profile(OpenProfile::InMemory)
    }

    /// Open a durable database with an explicit runtime config.
    pub fn open_with_config(
        data_dir: impl AsRef<Path>,
        runtime_config: RuntimeConfig,
    ) -> DbResult<Self> {
        Self::open_with_profile(OpenProfile::DurableWithConfig {
            data_dir: data_dir.as_ref().to_path_buf(),
            runtime_config: Box::new(runtime_config),
        })
    }

    /// Open a database from a stable embedded profile.
    pub fn open_with_profile(profile: OpenProfile) -> DbResult<Self> {
        match profile {
            OpenProfile::InMemory => Self::build_trusted_engine(EngineBuilder::new_in_memory()),
            OpenProfile::Durable { data_dir } => {
                Self::build_trusted_engine(EngineBuilder::new_durable(data_dir)?)
            }
            OpenProfile::DurableWithConfig {
                data_dir,
                runtime_config,
            } => {
                // `AllowAllAuthorizer` and forces `allow_ephemeral_users=true`
                // (`build_trusted_engine` above). That is acceptable for the
                // documented in-process trust model on Development profiles
                // but turns a caller-supplied Production config into an
                // unauthenticated trust-everyone surface (audit
                // embedded F2). Refuse to launch on a non-Development
                // profile so the caller's intent is honoured.
                if !matches!(
                    runtime_config.security.profile,
                    aiondb_config::SecurityProfile::Development
                ) {
                    return Err(aiondb_engine::DbError::invalid_authorization(
                        "embedded Database does not honour Production/Staging \
                         security profiles; use a separate authenticated \
                         deployment instead",
                    ));
                }
                Self::build_trusted_engine(EngineBuilder::new_durable_with_config(
                    data_dir,
                    *runtime_config,
                )?)
            }
        }
    }
}

impl<E: QueryEngine> Database<E> {
    /// Wrap an existing `QueryEngine` implementation.
    pub fn new(engine: Arc<E>) -> Self {
        Self { engine }
    }

    /// Start an anonymous embedded session.
    pub fn connect_anonymous(
        &self,
        database: impl Into<String>,
        user: impl Into<String>,
    ) -> DbResult<Connection<E>> {
        self.connect(ConnectOptions::anonymous(database, user))
    }

    pub fn connect(&self, options: ConnectOptions) -> DbResult<Connection<E>> {
        let (session, info) = self.engine.startup(StartupParams {
            database: options.database,
            application_name: options
                .application_name
                .or_else(|| Some("aiondb-embedded".to_owned())),
            options: Default::default(),
            credential: options.credential,
            transport: TransportInfo {
                kind: TransportKind::InProcess,
            },
        })?;

        Ok(Connection {
            engine: Arc::clone(&self.engine),
            session,
            info,
        })
    }
}

pub struct Connection<E: QueryEngine> {
    engine: Arc<E>,
    session: SessionHandle,
    info: SessionInfo,
}

impl<E: QueryEngine> Connection<E> {
    /// Return session identity and startup metadata.
    pub fn identity(&self) -> &SessionInfo {
        &self.info
    }

    /// Execute one SQL string (possibly multiple statements).
    pub fn execute(&self, sql: &str) -> DbResult<Vec<StatementResult>> {
        self.engine.execute_sql(&self.session, sql)
    }

    /// Prepare a named statement for extended-protocol execution.
    pub fn prepare(
        &self,
        name: impl Into<String>,
        sql: impl Into<String>,
    ) -> DbResult<PreparedStatement<E>> {
        let name = name.into();
        let desc = self
            .engine
            .prepare(&self.session, name.clone(), sql.into())?;
        Ok(PreparedStatement {
            engine: Arc::clone(&self.engine),
            session: self.session.clone(),
            name,
            desc,
        })
    }

    /// Run a closure inside an explicit transaction.
    ///
    /// On `Ok`, commits. On `Err`, rolls back and returns the original error.
    pub fn transaction<F, T>(&self, isolation: IsolationLevel, f: F) -> DbResult<T>
    where
        F: FnOnce(&Connection<E>) -> DbResult<T>,
    {
        self.engine.begin_transaction(&self.session, isolation)?;

        match f(self) {
            Ok(value) => {
                self.engine.commit_transaction(&self.session)?;
                Ok(value)
            }
            Err(error) => {
                if let Err(rb_err) = self.engine.rollback_transaction(&self.session) {
                    warn!(error = %rb_err, "rollback failed after transaction error");
                }
                Err(error)
            }
        }
    }
}

impl<E: QueryEngine> Drop for Connection<E> {
    fn drop(&mut self) {
        let _ = self.engine.terminate(self.session.clone());
    }
}

pub struct PreparedStatement<E: QueryEngine> {
    engine: Arc<E>,
    session: SessionHandle,
    name: String,
    desc: PreparedStatementDesc,
}

impl<E: QueryEngine> PreparedStatement<E> {
    /// Return statement metadata (columns and parameter types).
    pub fn descriptor(&self) -> &PreparedStatementDesc {
        &self.desc
    }

    /// Bind and execute a portal from this prepared statement.
    pub fn execute(
        &self,
        portal: impl Into<String>,
        params: Vec<Value>,
        max_rows: usize,
    ) -> DbResult<PortalBatch> {
        let portal = portal.into();
        self.engine
            .bind(&self.session, portal.clone(), self.name.clone(), params)?;

        match self.engine.execute_portal(&self.session, &portal, max_rows) {
            Ok(batch) => {
                if batch.exhausted {
                    self.engine.close_portal(&self.session, &portal)?;
                }
                Ok(batch)
            }
            Err(error) => {
                let _ = self.engine.close_portal(&self.session, &portal);
                Err(error)
            }
        }
    }

    /// Continue consuming an existing portal.
    pub fn resume(&self, portal: &str, max_rows: usize) -> DbResult<PortalBatch> {
        match self.engine.execute_portal(&self.session, portal, max_rows) {
            Ok(batch) => {
                if batch.exhausted {
                    self.engine.close_portal(&self.session, portal)?;
                }
                Ok(batch)
            }
            Err(error) => {
                let _ = self.engine.close_portal(&self.session, portal);
                Err(error)
            }
        }
    }
}

impl<E: QueryEngine> Drop for PreparedStatement<E> {
    fn drop(&mut self) {
        let _ = self.engine.close_statement(&self.session, &self.name);
    }
}

#[cfg(test)]
mod tests;
