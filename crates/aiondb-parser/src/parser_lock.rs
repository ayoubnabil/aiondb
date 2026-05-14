//! Parser for PostgreSQL `LOCK [TABLE] ...` statement.

use aiondb_core::DbResult;

use crate::{
    ast::{LockStatement, PgLockMode, Statement},
    keywords::Keyword,
    tokens::TokenKind,
    Parser,
};

impl Parser {
    /// Parse `LOCK [TABLE] [ONLY] name [*] [, ...] [IN lockmode MODE] [NOWAIT]`.
    pub(crate) fn parse_lock_statement(&mut self) -> DbResult<Statement> {
        let start = self.expect_keyword(Keyword::Lock)?;
        let mut span = start.span;

        if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("table"))
            || matches!(self.current().kind, TokenKind::Keyword(Keyword::Table))
        {
            span = span.merge(self.advance().span);
        }

        let only = if matches!(self.current().kind, TokenKind::Keyword(Keyword::Only)) {
            span = span.merge(self.advance().span);
            true
        } else {
            false
        };

        let mut tables = Vec::new();
        loop {
            let name = self.parse_object_name()?;
            span = span.merge(name.span);
            tables.push(name);
            if matches!(self.current().kind, TokenKind::Star) {
                span = span.merge(self.advance().span);
            }
            if matches!(self.current().kind, TokenKind::Comma) {
                self.advance();
                continue;
            }
            break;
        }

        let mode = if matches!(self.current().kind, TokenKind::Keyword(Keyword::In)) {
            span = span.merge(self.advance().span);
            let (m, m_span) = self.parse_lock_mode()?;
            span = span.merge(m_span);
            if !matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("mode"))
            {
                return self.syntax_error_current("expected MODE after lock mode name");
            }
            span = span.merge(self.advance().span);
            m
        } else {
            PgLockMode::AccessExclusive
        };

        let nowait = if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("nowait"))
        {
            span = span.merge(self.advance().span);
            true
        } else {
            false
        };

        self.skip_to_semicolon(&mut span);

        Ok(Statement::Lock(LockStatement {
            tables,
            only,
            mode,
            nowait,
            span,
        }))
    }

    fn parse_lock_mode(&mut self) -> DbResult<(PgLockMode, crate::span::Span)> {
        let mut span = self.current().span;
        let ident = self.expect_identifier_or_keyword_name()?;
        match ident.to_ascii_uppercase().as_str() {
            "ACCESS" => {
                let next = self.expect_identifier_or_keyword_name()?;
                span = span.merge(self.tokens[self.index - 1].span);
                if next.eq_ignore_ascii_case("share") {
                    Ok((PgLockMode::AccessShare, span))
                } else if next.eq_ignore_ascii_case("exclusive") {
                    Ok((PgLockMode::AccessExclusive, span))
                } else {
                    self.syntax_error_current("expected SHARE or EXCLUSIVE after ACCESS")
                }
            }
            "ROW" => {
                let next = self.expect_identifier_or_keyword_name()?;
                span = span.merge(self.tokens[self.index - 1].span);
                if next.eq_ignore_ascii_case("share") {
                    Ok((PgLockMode::RowShare, span))
                } else if next.eq_ignore_ascii_case("exclusive") {
                    Ok((PgLockMode::RowExclusive, span))
                } else {
                    self.syntax_error_current("expected SHARE or EXCLUSIVE after ROW")
                }
            }
            "SHARE" => {
                let is_update = matches!(self.current().kind, TokenKind::Keyword(Keyword::Update))
                    || matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("update"));
                let is_row = matches!(self.current().kind, TokenKind::Keyword(Keyword::Row))
                    || matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("row"));
                if is_update {
                    span = span.merge(self.advance().span);
                    let next = self.expect_identifier_or_keyword_name()?;
                    span = span.merge(self.tokens[self.index - 1].span);
                    if !next.eq_ignore_ascii_case("exclusive") {
                        return self.syntax_error_current("expected EXCLUSIVE after SHARE UPDATE");
                    }
                    Ok((PgLockMode::ShareUpdateExclusive, span))
                } else if is_row {
                    span = span.merge(self.advance().span);
                    let next = self.expect_identifier_or_keyword_name()?;
                    span = span.merge(self.tokens[self.index - 1].span);
                    if !next.eq_ignore_ascii_case("exclusive") {
                        return self.syntax_error_current("expected EXCLUSIVE after SHARE ROW");
                    }
                    Ok((PgLockMode::ShareRowExclusive, span))
                } else {
                    Ok((PgLockMode::Share, span))
                }
            }
            "EXCLUSIVE" => Ok((PgLockMode::Exclusive, span)),
            other => self.syntax_error_current(format!("unknown lock mode \"{other}\"")),
        }
    }

    fn expect_identifier_or_keyword_name(&mut self) -> DbResult<String> {
        let name = match &self.current().kind {
            TokenKind::Identifier(s) => s.clone(),
            TokenKind::Keyword(kw) => kw.name().to_owned(),
            _ => return self.syntax_error_current("expected identifier"),
        };
        self.advance();
        Ok(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_sql;

    #[test]
    fn parses_lock_default_mode() {
        let stmts = parse_sql("LOCK TABLE users;").expect("parse");
        match &stmts[0] {
            Statement::Lock(lock) => {
                assert_eq!(lock.tables.len(), 1);
                assert_eq!(lock.tables[0].parts, vec!["users".to_owned()]);
                assert_eq!(lock.mode, PgLockMode::AccessExclusive);
                assert!(!lock.nowait);
                assert!(!lock.only);
            }
            other => panic!("expected Lock, got {other:?}"),
        }
    }

    #[test]
    fn parses_lock_with_explicit_mode() {
        let stmts = parse_sql("LOCK users IN ACCESS SHARE MODE;").expect("parse");
        match &stmts[0] {
            Statement::Lock(lock) => {
                assert_eq!(lock.mode, PgLockMode::AccessShare);
            }
            other => panic!("expected Lock, got {other:?}"),
        }
    }

    #[test]
    fn parses_lock_nowait() {
        let stmts = parse_sql("LOCK TABLE users IN EXCLUSIVE MODE NOWAIT;").expect("parse");
        match &stmts[0] {
            Statement::Lock(lock) => {
                assert_eq!(lock.mode, PgLockMode::Exclusive);
                assert!(lock.nowait);
            }
            other => panic!("expected Lock, got {other:?}"),
        }
    }

    #[test]
    fn parses_lock_multiple_tables() {
        let stmts = parse_sql("LOCK TABLE a, b, c IN SHARE MODE;").expect("parse");
        match &stmts[0] {
            Statement::Lock(lock) => {
                assert_eq!(lock.tables.len(), 3);
                assert_eq!(lock.mode, PgLockMode::Share);
            }
            other => panic!("expected Lock, got {other:?}"),
        }
    }

    #[test]
    fn parses_share_row_exclusive() {
        let stmts = parse_sql("LOCK TABLE a IN SHARE ROW EXCLUSIVE MODE;").expect("parse");
        match &stmts[0] {
            Statement::Lock(lock) => {
                assert_eq!(lock.mode, PgLockMode::ShareRowExclusive);
            }
            other => panic!("expected Lock, got {other:?}"),
        }
    }

    #[test]
    fn parses_share_update_exclusive() {
        let stmts = parse_sql("LOCK TABLE a IN SHARE UPDATE EXCLUSIVE MODE;").expect("parse");
        match &stmts[0] {
            Statement::Lock(lock) => {
                assert_eq!(lock.mode, PgLockMode::ShareUpdateExclusive);
            }
            other => panic!("expected Lock, got {other:?}"),
        }
    }

    #[test]
    fn parses_row_share() {
        let stmts = parse_sql("LOCK TABLE a IN ROW SHARE MODE;").expect("parse");
        match &stmts[0] {
            Statement::Lock(lock) => {
                assert_eq!(lock.mode, PgLockMode::RowShare);
            }
            other => panic!("expected Lock, got {other:?}"),
        }
    }
}
