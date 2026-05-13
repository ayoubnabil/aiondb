use std::sync::Arc;
use std::sync::OnceLock;

use aiondb_config::EnginePoolConfig;
use aiondb_engine::{DbError, DbResult, PgWireEngine};
use tokio::sync::Semaphore;
use tracing::warn;

/// Bounded async bridge for dispatching blocking `PgWireEngine` work from the
/// pgwire tasks.
pub struct EnginePool<E: PgWireEngine + 'static> {
    engine: Arc<E>,
    worker_slots: Arc<Semaphore>,
    total_slots: Arc<Semaphore>,
    /// Lazily-discovered runtime flavor. When the ambient tokio runtime is
    /// multi-threaded, `run()` uses `block_in_place` to skip the
    /// spawn_blocking task-transfer overhead on the uncontended OLTP hot
    /// path. Detected on first `run()` because `new()` is sync and may be
    /// called before the runtime is attached.
    block_in_place_allowed: Arc<OnceLock<bool>>,
}

impl<E: PgWireEngine + 'static> Clone for EnginePool<E> {
    fn clone(&self) -> Self {
        Self {
            engine: Arc::clone(&self.engine),
            worker_slots: Arc::clone(&self.worker_slots),
            total_slots: Arc::clone(&self.total_slots),
            block_in_place_allowed: Arc::clone(&self.block_in_place_allowed),
        }
    }
}

impl<E: PgWireEngine + 'static> EnginePool<E> {
    #[must_use]
    pub fn new(engine: Arc<E>, config: EnginePoolConfig) -> Self {
        let worker_threads = config.worker_threads.max(1);
        let queue_depth = config.queue_depth.max(1);

        if worker_threads != config.worker_threads {
            warn!(
                configured = config.worker_threads,
                normalized = worker_threads,
                "pgwire engine pool worker_threads must be >= 1; normalizing invalid value"
            );
        }
        if queue_depth != config.queue_depth {
            warn!(
                configured = config.queue_depth,
                normalized = queue_depth,
                "pgwire engine pool queue_depth must be >= 1; normalizing invalid value"
            );
        }

        Self {
            engine,
            worker_slots: Arc::new(Semaphore::new(worker_threads)),
            total_slots: Arc::new(Semaphore::new(worker_threads.saturating_add(queue_depth))),
            block_in_place_allowed: Arc::new(OnceLock::new()),
        }
    }

    fn block_in_place_allowed(&self) -> bool {
        *self.block_in_place_allowed.get_or_init(|| {
            matches!(
                tokio::runtime::Handle::try_current().map(|h| h.runtime_flavor()),
                Ok(tokio::runtime::RuntimeFlavor::MultiThread)
            )
        })
    }

    pub async fn run<T, F>(&self, work: F) -> DbResult<T>
    where
        T: Send + 'static,
        F: FnOnce(Arc<E>) -> DbResult<T> + Send + 'static,
    {
        let total_permit = self
            .total_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| DbError::program_limit("pgwire engine pool queue is full"))?;

        // Fast path: on a multi-thread runtime with a worker slot immediately
        // available, dispatch via `block_in_place` to avoid the
        // spawn_blocking task-transfer overhead (~1-5µs per call) that
        // otherwise dominates short OLTP queries. The semaphores still bound
        // engine concurrency in the contended case via the slow path below.
        if self.block_in_place_allowed() {
            if let Ok(worker_permit) = self.worker_slots.clone().try_acquire_owned() {
                let engine = Arc::clone(&self.engine);
                let result = tokio::task::block_in_place(move || work(engine));
                drop(worker_permit);
                drop(total_permit);
                return result;
            }
        }

        let worker_permit = self
            .worker_slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| DbError::internal("pgwire engine pool is unavailable"))?;
        let engine = Arc::clone(&self.engine);

        tokio::task::spawn_blocking(move || {
            let _total_permit = total_permit;
            let _worker_permit = worker_permit;
            work(engine)
        })
        .await
        .map_err(|error| DbError::internal(format!("pgwire engine task failed: {error}")))?
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aiondb_core::DatabaseId;
    use aiondb_engine::{
        AuthenticatedIdentity, Credential, PortalBatch, PortalDescription, PreparedStatementDesc,
        QueryEngine, SessionHandle, SessionInfo, StartupAuthentication, StartupParams,
        StatementResult, Value,
    };

    use super::*;

    struct NoopEngine;

    impl NoopEngine {
        fn empty_prepared_statement(name: impl Into<String>) -> PreparedStatementDesc {
            PreparedStatementDesc {
                name: name.into(),
                param_types: vec![],
                result_columns: vec![],
                result_column_origins: vec![],
            }
        }

        fn empty_portal_description() -> PortalDescription {
            PortalDescription {
                result_columns: vec![],
                result_column_origins: vec![],
            }
        }

        fn empty_portal_batch() -> PortalBatch {
            PortalBatch {
                columns: vec![],
                rows: vec![],
                tag: "SELECT".to_string(),
                rows_affected: 0,
                exhausted: true,
            }
        }
    }

    impl QueryEngine for NoopEngine {
        fn startup_authentication(
            &self,
            _user: &str,
            _database: &str,
            _transport: &aiondb_engine::TransportInfo,
        ) -> DbResult<StartupAuthentication> {
            Ok(StartupAuthentication::Trust)
        }

        fn startup(&self, _params: StartupParams) -> DbResult<(SessionHandle, SessionInfo)> {
            Ok((
                SessionHandle::test_handle(),
                SessionInfo {
                    identity: AuthenticatedIdentity {
                        user: "test".to_owned(),
                        database_id: DatabaseId::new(0),
                        roles: Vec::new(),
                    },
                    is_superuser: false,
                    limits: Default::default(),
                    database_name: "db".to_owned(),
                    active_database: aiondb_cluster::DatabaseId::DEFAULT,
                },
            ))
        }

        fn has_active_transaction(&self, _session: &SessionHandle) -> DbResult<bool> {
            Ok(false)
        }
        fn begin_transaction(
            &self,
            _session: &SessionHandle,
            _isolation: aiondb_engine::IsolationLevel,
        ) -> DbResult<()> {
            Ok(())
        }
        fn commit_transaction(&self, _session: &SessionHandle) -> DbResult<()> {
            Ok(())
        }
        fn rollback_transaction(&self, _session: &SessionHandle) -> DbResult<()> {
            Ok(())
        }
        fn execute_sql(
            &self,
            _session: &SessionHandle,
            _sql: &str,
        ) -> DbResult<Vec<StatementResult>> {
            Ok(Vec::new())
        }
        fn prepare(
            &self,
            _session: &SessionHandle,
            statement_name: String,
            _sql: String,
        ) -> DbResult<PreparedStatementDesc> {
            Ok(Self::empty_prepared_statement(statement_name))
        }
        fn describe_statement(
            &self,
            _session: &SessionHandle,
            statement_name: &str,
        ) -> DbResult<PreparedStatementDesc> {
            Ok(Self::empty_prepared_statement(statement_name.to_owned()))
        }
        fn bind(
            &self,
            _session: &SessionHandle,
            _portal_name: String,
            _statement_name: String,
            _params: Vec<Value>,
        ) -> DbResult<()> {
            Ok(())
        }
        fn describe_portal(
            &self,
            _session: &SessionHandle,
            _portal_name: &str,
        ) -> DbResult<PortalDescription> {
            Ok(Self::empty_portal_description())
        }
        fn execute_portal(
            &self,
            _session: &SessionHandle,
            _portal_name: &str,
            _max_rows: usize,
        ) -> DbResult<PortalBatch> {
            Ok(Self::empty_portal_batch())
        }
        fn close_statement(&self, _session: &SessionHandle, _statement_name: &str) -> DbResult<()> {
            Ok(())
        }
        fn close_portal(&self, _session: &SessionHandle, _portal_name: &str) -> DbResult<()> {
            Ok(())
        }
        fn execute_copy_from(
            &self,
            _session: &SessionHandle,
            _table_id: aiondb_core::RelationId,
            _data: &str,
        ) -> DbResult<StatementResult> {
            Ok(StatementResult::Command {
                tag: "COPY".to_owned(),
                rows_affected: 0,
            })
        }
        fn cancel_session(&self, _session: &SessionHandle) -> DbResult<()> {
            Ok(())
        }
        fn terminate(&self, _session: SessionHandle) -> DbResult<()> {
            Ok(())
        }
        fn session_count(&self) -> DbResult<usize> {
            Ok(0)
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn engine_pool_executes_engine_calls() {
        let engine = Arc::new(NoopEngine);
        let pool = EnginePool::new(
            Arc::clone(&engine),
            EnginePoolConfig {
                worker_threads: 1,
                queue_depth: 1,
            },
        );

        let (session, info) = pool
            .run(|engine| {
                engine.startup(StartupParams {
                    database: "db".to_owned(),
                    application_name: None,
                    options: Default::default(),
                    credential: Credential::Anonymous {
                        user: "test".to_owned(),
                    },
                    transport: aiondb_engine::TransportInfo {
                        kind: aiondb_engine::TransportKind::InProcess,
                    },
                })
            })
            .await
            .expect("startup should succeed through engine pool");
        assert_eq!(session, SessionHandle::test_handle());
        assert_eq!(info.database_name, "db");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn engine_pool_rejects_requests_when_queue_is_full() {
        let pool = EnginePool::new(
            Arc::new(NoopEngine),
            EnginePoolConfig {
                worker_threads: 1,
                queue_depth: 1,
            },
        );

        let permit_a = pool
            .total_slots
            .clone()
            .try_acquire_owned()
            .expect("first slot");
        let permit_b = pool
            .total_slots
            .clone()
            .try_acquire_owned()
            .expect("second slot");

        let error = pool
            .run(|engine| {
                engine.startup(StartupParams {
                    database: "db".to_owned(),
                    application_name: None,
                    options: Default::default(),
                    credential: Credential::Anonymous {
                        user: "test".to_owned(),
                    },
                    transport: aiondb_engine::TransportInfo {
                        kind: aiondb_engine::TransportKind::InProcess,
                    },
                })
            })
            .await
            .expect_err("queue should be full");
        assert_eq!(
            error.sqlstate(),
            aiondb_core::SqlState::ProgramLimitExceeded
        );

        drop(permit_a);
        drop(permit_b);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn engine_pool_supports_extended_query_test_paths_without_panicking() {
        let pool = EnginePool::new(
            Arc::new(NoopEngine),
            EnginePoolConfig {
                worker_threads: 1,
                queue_depth: 1,
            },
        );
        let session = SessionHandle::test_handle();

        let prepared = pool
            .run({
                let session = session.clone();
                move |engine| engine.prepare(&session, "stmt".to_owned(), "SELECT 1".to_owned())
            })
            .await
            .expect("prepare should succeed through engine pool");
        assert_eq!(prepared.name, "stmt");
        assert!(prepared.result_columns.is_empty());

        let described = pool
            .run({
                let session = session.clone();
                move |engine| engine.describe_statement(&session, "stmt")
            })
            .await
            .expect("describe statement should succeed through engine pool");
        assert_eq!(described.name, "stmt");

        let portal = pool
            .run({
                let session = session.clone();
                move |engine| engine.describe_portal(&session, "portal")
            })
            .await
            .expect("describe portal should succeed through engine pool");
        assert!(portal.result_columns.is_empty());

        let batch = pool
            .run({
                let session = session.clone();
                move |engine| engine.execute_portal(&session, "portal", 32)
            })
            .await
            .expect("execute portal should succeed through engine pool");
        assert_eq!(batch.tag, "SELECT");
        assert!(batch.exhausted);
        assert!(batch.rows.is_empty());

        let copy = pool
            .run(move |engine| {
                engine.execute_copy_from(&session, aiondb_core::RelationId::new(7), "1\t2\n")
            })
            .await
            .expect("copy should succeed through engine pool");
        assert_eq!(
            copy,
            StatementResult::Command {
                tag: "COPY".to_owned(),
                rows_affected: 0,
            }
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn engine_pool_normalizes_zero_limits_instead_of_hanging() {
        let engine = Arc::new(NoopEngine);
        let pool = EnginePool::new(
            Arc::clone(&engine),
            EnginePoolConfig {
                worker_threads: 0,
                queue_depth: 0,
            },
        );

        assert_eq!(pool.worker_slots.available_permits(), 1);
        assert_eq!(pool.total_slots.available_permits(), 2);

        let (session, info) = pool
            .run(|engine| {
                engine.startup(StartupParams {
                    database: "db".to_owned(),
                    application_name: None,
                    options: Default::default(),
                    credential: Credential::Anonymous {
                        user: "test".to_owned(),
                    },
                    transport: aiondb_engine::TransportInfo {
                        kind: aiondb_engine::TransportKind::InProcess,
                    },
                })
            })
            .await
            .expect("startup should succeed through normalized engine pool");
        assert_eq!(session, SessionHandle::test_handle());
        assert_eq!(info.database_name, "db");
    }
}
