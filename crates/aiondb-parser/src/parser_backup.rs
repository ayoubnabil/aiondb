use aiondb_core::DbResult;

use crate::{ast::Statement, keywords::Keyword, tokens::TokenKind, Parser};

impl Parser {
    /// Parse `BACKUP DATABASE TO '<path>'`
    pub(crate) fn parse_backup_statement(&mut self) -> DbResult<Statement> {
        let backup_token = self.expect_keyword(Keyword::Backup)?;
        self.expect_keyword(Keyword::Database)?;
        self.expect_keyword(Keyword::To)?;

        let TokenKind::String(path) = self.current().kind.clone() else {
            return self.syntax_error_current("expected string literal for backup path");
        };
        let path_token = self.advance();
        let span = backup_token.span.merge(path_token.span);

        Ok(Statement::Backup { path, span })
    }

    /// Parse `RESTORE DATABASE FROM '<path>'`
    pub(crate) fn parse_restore_statement(&mut self) -> DbResult<Statement> {
        let restore_token = self.expect_keyword(Keyword::Restore)?;
        self.expect_keyword(Keyword::Database)?;
        self.expect_keyword(Keyword::From)?;

        let TokenKind::String(path) = self.current().kind.clone() else {
            return self.syntax_error_current("expected string literal for restore path");
        };
        let path_token = self.advance();
        let span = restore_token.span.merge(path_token.span);

        Ok(Statement::Restore { path, span })
    }
}

#[cfg(test)]
mod tests {
    use crate::parse_sql;

    #[test]
    fn parse_backup_database() {
        let stmts = parse_sql("BACKUP DATABASE TO '/tmp/backup.sql'").unwrap();
        assert_eq!(stmts.len(), 1);
        match &stmts[0] {
            crate::Statement::Backup { path, .. } => {
                assert_eq!(path, "/tmp/backup.sql");
            }
            other => panic!("expected Backup, got {other:?}"),
        }
    }

    #[test]
    fn parse_restore_database() {
        let stmts = parse_sql("RESTORE DATABASE FROM '/tmp/backup.sql'").unwrap();
        assert_eq!(stmts.len(), 1);
        match &stmts[0] {
            crate::Statement::Restore { path, .. } => {
                assert_eq!(path, "/tmp/backup.sql");
            }
            other => panic!("expected Restore, got {other:?}"),
        }
    }

    #[test]
    fn parse_backup_missing_path() {
        let result = parse_sql("BACKUP DATABASE TO");
        assert!(result.is_err());
    }

    #[test]
    fn parse_restore_missing_path() {
        let result = parse_sql("RESTORE DATABASE FROM");
        assert!(result.is_err());
    }

    #[test]
    fn parse_backup_missing_database() {
        let result = parse_sql("BACKUP TO '/tmp/backup.sql'");
        assert!(result.is_err());
    }

    #[test]
    fn parse_restore_missing_database() {
        let result = parse_sql("RESTORE FROM '/tmp/backup.sql'");
        assert!(result.is_err());
    }

    #[test]
    fn parse_backup_case_insensitive() {
        let stmts = parse_sql("backup database to '/tmp/backup.sql'").unwrap();
        assert_eq!(stmts.len(), 1);
        match &stmts[0] {
            crate::Statement::Backup { path, .. } => {
                assert_eq!(path, "/tmp/backup.sql");
            }
            other => panic!("expected Backup, got {other:?}"),
        }
    }

    #[test]
    fn parse_restore_case_insensitive() {
        let stmts = parse_sql("restore database from '/tmp/backup.sql'").unwrap();
        assert_eq!(stmts.len(), 1);
        match &stmts[0] {
            crate::Statement::Restore { path, .. } => {
                assert_eq!(path, "/tmp/backup.sql");
            }
            other => panic!("expected Restore, got {other:?}"),
        }
    }

    #[test]
    fn parse_backup_span_is_valid() {
        let stmts = parse_sql("BACKUP DATABASE TO '/tmp/backup.sql'").unwrap();
        let span = stmts[0].span();
        assert_eq!(span.start, 0);
        assert!(span.end > span.start);
    }

    #[test]
    fn parse_restore_span_is_valid() {
        let stmts = parse_sql("RESTORE DATABASE FROM '/tmp/backup.sql'").unwrap();
        let span = stmts[0].span();
        assert_eq!(span.start, 0);
        assert!(span.end > span.start);
    }
}
