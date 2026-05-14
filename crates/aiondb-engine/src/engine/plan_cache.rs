#![allow(clippy::unused_self, clippy::wildcard_imports)]

use super::*;
use std::collections::hash_map::DefaultHasher;
use std::fmt::{self, Write as _};
use std::hash::Hasher;
use std::sync::Arc;

use crate::session::{PlanCacheKey, StatementFingerprint};

pub(super) struct PlanCacheSessionContext {
    pub txn_id: TxnId,
    pub default_schema: Option<String>,
    pub search_path: Option<String>,
    pub current_user: String,
    pub session_user: String,
    pub catalog_revision: u64,
}

struct FingerprintWriter {
    first: DefaultHasher,
    second: DefaultHasher,
    bytes_seen: u64,
}

impl FingerprintWriter {
    fn new() -> Self {
        let mut first = DefaultHasher::new();
        let mut second = DefaultHasher::new();
        first.write_u64(0xA10D_BF01);
        second.write_u64(0xA10D_BF02);
        Self {
            first,
            second,
            bytes_seen: 0,
        }
    }

    fn finish(mut self) -> StatementFingerprint {
        self.first.write_u64(self.bytes_seen);
        self.second.write_u64(self.bytes_seen.rotate_left(17));
        StatementFingerprint {
            first: self.first.finish(),
            second: self.second.finish(),
        }
    }
}

impl fmt::Write for FingerprintWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.first.write(s.as_bytes());
        self.second.write(s.as_bytes());
        self.bytes_seen = self
            .bytes_seen
            .saturating_add(u64::try_from(s.len()).unwrap_or(u64::MAX));
        Ok(())
    }
}

impl Engine {
    pub(super) fn cacheable_plan_statement(statement: &Statement) -> bool {
        matches!(
            statement,
            Statement::Analyze { .. }
                | Statement::Vacuum { .. }
                | Statement::AlterTable(_)
                | Statement::Copy(_)
                | Statement::CreateEdgeLabel(_)
                | Statement::CreateIndex(_)
                | Statement::CreateNodeLabel(_)
                | Statement::CreateRole(_)
                | Statement::CreateSchema(_)
                | Statement::CreateSequence(_)
                | Statement::CreateTable(_)
                | Statement::CreateTableAs(_)
                | Statement::CreateView(_)
                | Statement::Delete(_)
                | Statement::DropEdgeLabel(_)
                | Statement::DropIndex(_)
                | Statement::DropNodeLabel(_)
                | Statement::DropRole(_)
                | Statement::DropSchema(_)
                | Statement::DropSequence(_)
                | Statement::DropTable(_)
                | Statement::DropView(_)
                | Statement::Grant(_)
                | Statement::Insert(_)
                | Statement::Merge(_)
                | Statement::Revoke(_)
                | Statement::Select(_)
                | Statement::SetOperation(_)
                | Statement::TruncateTable(_)
                | Statement::Update(_)
                | Statement::AlterRole(_)
        )
    }

    pub(super) fn plan_cache_session_context_for_record(
        &self,
        record: &SessionRecord,
    ) -> DbResult<PlanCacheSessionContext> {
        let current_txn_id = record
            .active_txn
            .as_ref()
            .map(|txn| txn.id)
            .unwrap_or_default();
        // Share plan-cache entries across explicit transactions that have no
        // pending catalog writes. This keeps transactional DDL semantics
        // correct (those still key on txn_id) while avoiding needless
        // re-planning for read-only OLTP loops.
        let plan_cache_txn_id = self.plan_cache_txn_id_for_record(record)?;
        Ok(PlanCacheSessionContext {
            txn_id: plan_cache_txn_id,
            default_schema: self::session_vars::primary_search_path_schema_for_record(
                self.catalog_reader.as_ref(),
                current_txn_id,
                record,
            )?,
            search_path: Some(self::session_vars::resolved_search_path_for_record(record)),
            current_user: self::session_vars::current_user_for_record(record),
            session_user: self::session_vars::session_user_for_record(record),
            catalog_revision: self.catalog_reader.catalog_revision(plan_cache_txn_id)?,
        })
    }

    pub(super) fn plan_cache_key_from_context_and_fingerprint(
        &self,
        statement_fingerprint: StatementFingerprint,
        context: &PlanCacheSessionContext,
    ) -> PlanCacheKey {
        PlanCacheKey {
            statement_fingerprint,
            txn_id: context.txn_id,
            default_schema: context.default_schema.clone(),
            search_path: context.search_path.clone(),
            current_user: context.current_user.clone(),
            session_user: context.session_user.clone(),
            catalog_revision: context.catalog_revision,
        }
    }

    pub(super) fn cached_physical_plan(
        &self,
        session: &SessionHandle,
        key: &PlanCacheKey,
    ) -> DbResult<Option<Arc<aiondb_plan::PhysicalPlan>>> {
        self.with_session_mut(session, |record| Ok(record.cached_plan(key)))
    }

    pub(super) fn remember_physical_plan(
        &self,
        session: &SessionHandle,
        key: PlanCacheKey,
        plan: Arc<aiondb_plan::PhysicalPlan>,
    ) -> DbResult<()> {
        self.with_session_mut(session, |record| {
            record.remember_plan(key, plan);
            Ok(())
        })
    }

    pub(super) fn invalidate_plan_cache(&self) -> DbResult<()> {
        let sessions = self.sessions()?;
        let mut poisoned_sessions = Vec::new();
        for (handle, session) in sessions.iter() {
            match Self::lock_session(session) {
                Ok(mut record) => record.clear_plan_cache(),
                Err(error) => {
                    warn!(
                        error = %error,
                        session = ?handle,
                        "dropping poisoned session during plan cache invalidation"
                    );
                    poisoned_sessions.push(handle.clone());
                }
            }
        }
        drop(sessions);

        if !poisoned_sessions.is_empty() {
            let mut sessions = self.sessions_mut()?;
            for handle in poisoned_sessions {
                sessions.remove(&handle);
            }
        }

        Ok(())
    }

    pub(super) fn statement_invalidates_plan_cache(statement: &Statement) -> bool {
        matches!(
            statement,
            Statement::Analyze { .. }
                | Statement::AlterTable(_)
                | Statement::Backup { .. }
                | Statement::CreateEdgeLabel(_)
                | Statement::CreateFunction(_)
                | Statement::CreateIndex(_)
                | Statement::CreateNodeLabel(_)
                | Statement::CreateRole(_)
                | Statement::CreateSchema(_)
                | Statement::CreateSequence(_)
                | Statement::CreateTable(_)
                | Statement::CreateTableAs(_)
                | Statement::CreateTenant { .. }
                | Statement::CreateTrigger(_)
                | Statement::AlterTriggerRename(_)
                | Statement::CreateExtension(_)
                | Statement::CreateView(_)
                | Statement::DropEdgeLabel(_)
                | Statement::DropExtension(_)
                | Statement::DropFunction(_)
                | Statement::DropIndex(_)
                | Statement::DropNodeLabel(_)
                | Statement::DropRole(_)
                | Statement::DropSchema(_)
                | Statement::DropSequence(_)
                | Statement::DropTable(_)
                | Statement::DropTenant { .. }
                | Statement::DropTrigger(_)
                | Statement::DropView(_)
                | Statement::Grant(_)
                | Statement::Revoke(_)
                | Statement::Restore { .. }
                | Statement::SetTenant { .. }
                | Statement::TruncateTable(_)
                | Statement::AlterRole(_)
        )
    }

    #[cfg(test)]
    pub(super) fn plan_cache_hits(&self) -> u64 {
        self.plan_cache_hits.load(Ordering::SeqCst)
    }

    #[cfg(test)]
    pub(super) fn session_plan_cache_len(&self, session: &SessionHandle) -> DbResult<usize> {
        self.with_session(session, |record| Ok(record.plan_cache.len()))
    }

    #[cfg(test)]
    pub(super) fn session_parsed_sql_cache_len(&self, session: &SessionHandle) -> DbResult<usize> {
        self.with_session(session, |record| Ok(record.parsed_sql_cache.len()))
    }

    #[cfg(test)]
    pub(super) fn session_parsed_sql_cache_sql_bytes(
        &self,
        session: &SessionHandle,
    ) -> DbResult<usize> {
        self.with_session(session, |record| Ok(record.parsed_sql_cache_sql_bytes))
    }

    #[cfg(test)]
    pub(super) fn session_has_cached_sql(
        &self,
        session: &SessionHandle,
        sql: &str,
    ) -> DbResult<bool> {
        self.with_session(session, |record| {
            if record.parsed_sql_cache.contains_key(sql) {
                return Ok(true);
            }
            Ok(super::query_api::literal_shape_sql(sql)
                .is_some_and(|shape| record.parsed_sql_cache.contains_key(&shape.sql)))
        })
    }

    #[cfg(test)]
    pub(super) fn session_has_cached_sql_plan_fingerprints(
        &self,
        session: &SessionHandle,
        sql: &str,
    ) -> DbResult<bool> {
        self.with_session(session, |record| {
            if record
                .parsed_sql_cache
                .get(sql)
                .is_some_and(|entry| entry.plan_fingerprints.is_some())
            {
                return Ok(true);
            }
            Ok(
                super::query_api::literal_shape_sql(sql).is_some_and(|shape| {
                    record
                        .parsed_sql_cache
                        .get(&shape.sql)
                        .is_some_and(|entry| entry.plan_fingerprints.is_some())
                }),
            )
        })
    }
}

pub(super) fn statement_fingerprint(statement: &Statement) -> StatementFingerprint {
    let mut writer = FingerprintWriter::new();
    let _ = write!(&mut writer, "{statement:?}");
    writer.finish()
}
