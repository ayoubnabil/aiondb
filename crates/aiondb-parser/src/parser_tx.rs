use aiondb_core::DbResult;

use crate::{
    ast::{Statement, TransactionControlStatement, TransactionMode},
    keywords::Keyword,
    tokens::TokenKind,
    Parser,
};

impl Parser {
    pub(crate) fn parse_prepare_transaction_statement(&mut self) -> DbResult<Statement> {
        let prepare = self.advance();
        let mut span = prepare.span;
        if self.consume_keyword(Keyword::Transaction).is_some() {
            span = span.merge(self.previous_span());
        } else if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("transaction"))
        {
            span = span.merge(self.advance().span);
        } else {
            return self.syntax_error_current("expected TRANSACTION after PREPARE");
        }

        let (gid, gid_span) = self.parse_prepared_transaction_gid()?;
        Ok(Statement::PrepareTransaction {
            gid,
            span: span.merge(gid_span),
        })
    }

    pub(crate) fn parse_transaction_statement(&mut self) -> DbResult<Statement> {
        match self.current().kind {
            crate::tokens::TokenKind::Keyword(Keyword::Begin) => self.parse_begin_statement(),
            crate::tokens::TokenKind::Keyword(Keyword::Start) => self.parse_start_transaction(),
            crate::tokens::TokenKind::Keyword(Keyword::Commit) => {
                let token = self.advance();
                if matches!(&self.current().kind, crate::tokens::TokenKind::Identifier(s) if s.eq_ignore_ascii_case("prepared"))
                {
                    let prepared = self.advance();
                    let (gid, gid_span) = self.parse_prepared_transaction_gid()?;
                    return Ok(Statement::CommitPrepared {
                        gid,
                        span: token.span.merge(prepared.span).merge(gid_span),
                    });
                }
                // Skip optional WORK or TRANSACTION keyword after COMMIT
                let _ = self.consume_keyword(Keyword::Work);
                let _ = self.consume_keyword(Keyword::Transaction);
                // Skip optional AND [NO] CHAIN
                if self.consume_keyword(Keyword::And).is_some() {
                    let _ = self.consume_keyword(Keyword::No);
                    let _ = self.consume_keyword(Keyword::Chain);
                }
                Ok(Statement::Commit { span: token.span })
            }
            crate::tokens::TokenKind::Keyword(Keyword::Rollback) => self.parse_rollback_statement(),
            crate::tokens::TokenKind::Keyword(Keyword::Savepoint) => {
                self.parse_savepoint_statement()
            }
            crate::tokens::TokenKind::Keyword(Keyword::Release) => {
                self.parse_release_savepoint_statement()
            }
            _ => self.syntax_error_current("expected transaction statement"),
        }
    }

    fn parse_begin_statement(&mut self) -> DbResult<Statement> {
        let begin = self.expect_keyword(Keyword::Begin)?;
        let control = self.parse_transaction_options(begin.span)?;
        Ok(Statement::Begin {
            mode: control.isolation,
            read_only: control.read_only,
            deferrable: control.deferrable,
            span: control.span,
        })
    }

    fn parse_start_transaction(&mut self) -> DbResult<Statement> {
        let start = self.expect_keyword(Keyword::Start)?;
        let transaction = self.expect_keyword(Keyword::Transaction)?;
        let control = self.parse_transaction_options(start.span.merge(transaction.span))?;
        Ok(Statement::Begin {
            mode: control.isolation,
            read_only: control.read_only,
            deferrable: control.deferrable,
            span: control.span,
        })
    }

    fn parse_rollback_statement(&mut self) -> DbResult<Statement> {
        let rollback_token = self.expect_keyword(Keyword::Rollback)?;
        if matches!(&self.current().kind, crate::tokens::TokenKind::Identifier(s) if s.eq_ignore_ascii_case("prepared"))
        {
            let prepared = self.advance();
            let (gid, gid_span) = self.parse_prepared_transaction_gid()?;
            return Ok(Statement::RollbackPrepared {
                gid,
                span: rollback_token.span.merge(prepared.span).merge(gid_span),
            });
        }
        // Skip optional WORK or TRANSACTION keyword after ROLLBACK
        let _ = self.consume_keyword(Keyword::Work);
        let _ = self.consume_keyword(Keyword::Transaction);
        if self.consume_keyword(Keyword::To).is_none() {
            // Skip optional AND [NO] CHAIN
            if self.consume_keyword(Keyword::And).is_some() {
                let _ = self.consume_keyword(Keyword::No);
                let _ = self.consume_keyword(Keyword::Chain);
            }
            return Ok(Statement::Rollback {
                span: rollback_token.span,
            });
        }
        // ROLLBACK TO [SAVEPOINT] name
        self.consume_keyword(Keyword::Savepoint);
        let (name, name_span) = self.expect_identifier()?;
        Ok(Statement::RollbackToSavepoint {
            name,
            span: rollback_token.span.merge(name_span),
        })
    }

    fn parse_prepared_transaction_gid(&mut self) -> DbResult<(String, crate::Span)> {
        match self.current().kind.clone() {
            TokenKind::String(gid) => {
                let span = self.advance().span;
                Ok((gid, span))
            }
            _ => self.syntax_error_current("expected transaction identifier after PREPARED"),
        }
    }

    fn parse_savepoint_statement(&mut self) -> DbResult<Statement> {
        let savepoint_token = self.expect_keyword(Keyword::Savepoint)?;
        let (name, name_span) = self.expect_identifier()?;
        Ok(Statement::Savepoint {
            name,
            span: savepoint_token.span.merge(name_span),
        })
    }

    fn parse_release_savepoint_statement(&mut self) -> DbResult<Statement> {
        let release_token = self.expect_keyword(Keyword::Release)?;
        self.consume_keyword(Keyword::Savepoint);
        let (name, name_span) = self.expect_identifier()?;
        Ok(Statement::ReleaseSavepoint {
            name,
            span: release_token.span.merge(name_span),
        })
    }

    pub(crate) fn parse_transaction_options(
        &mut self,
        base_span: crate::Span,
    ) -> DbResult<TransactionControlStatement> {
        // Consume optional TRANSACTION or WORK keyword after BEGIN/START.
        let _ = self.consume_keyword(Keyword::Transaction);
        let _ = self.consume_keyword(Keyword::Work);

        let mut isolation = None;
        let mut read_only = None;
        let mut deferrable = None;
        let mut span = base_span;

        while !self.is_eof() && !matches!(self.current().kind, TokenKind::Semicolon) {
            if matches!(self.current().kind, TokenKind::Comma) {
                self.advance();
                continue;
            }
            if self.consume_keyword(Keyword::And).is_some() {
                let _ = self.consume_keyword(Keyword::No);
                let _ = self.consume_keyword(Keyword::Chain);
                span = span.merge(self.previous_span());
                continue;
            }

            if self.consume_keyword(Keyword::Isolation).is_some() {
                let level = self.expect_keyword(Keyword::Level)?;
                let mode = self.parse_isolation_level()?;
                isolation = Some(mode);
                span = span.merge(level.span).merge(self.previous_span());
                continue;
            }

            if self.consume_keyword(Keyword::Read).is_some() {
                if self.consume_keyword(Keyword::Only).is_some() {
                    read_only = Some(true);
                    span = span.merge(self.previous_span());
                    continue;
                }
                if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("write"))
                {
                    span = span.merge(self.advance().span);
                    read_only = Some(false);
                    continue;
                }
                let read_span = self.previous_span();
                return self.syntax_error(
                    read_span,
                    "expected ONLY or WRITE after READ in transaction options",
                );
            }

            if self.consume_keyword(Keyword::Deferrable).is_some() {
                deferrable = Some(true);
                span = span.merge(self.previous_span());
                continue;
            }

            if self.consume_keyword(Keyword::Not).is_some() {
                self.expect_keyword(Keyword::Deferrable)?;
                deferrable = Some(false);
                span = span.merge(self.previous_span());
                continue;
            }

            return self.syntax_error_current("expected transaction option");
        }

        Ok(TransactionControlStatement {
            isolation,
            read_only,
            deferrable,
            span,
        })
    }

    pub(crate) fn parse_isolation_level(&mut self) -> DbResult<TransactionMode> {
        let start_span = self.current().span;
        if self.consume_keyword(Keyword::Read).is_some() {
            self.expect_keyword(Keyword::Committed)?;
            Ok(TransactionMode::ReadCommitted)
        } else if self.consume_keyword(Keyword::Snapshot).is_some() {
            self.expect_keyword(Keyword::Isolation)?;
            Ok(TransactionMode::SnapshotIsolation)
        } else if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("serializable"))
        {
            self.advance();
            Ok(TransactionMode::Serializable)
        } else if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("repeatable"))
        {
            self.advance();
            let _ = self.consume_keyword(Keyword::Read);
            Ok(TransactionMode::SnapshotIsolation)
        } else {
            self.syntax_error(
                start_span,
                "expected READ COMMITTED, SNAPSHOT ISOLATION, or SERIALIZABLE",
            )
        }
    }
}
