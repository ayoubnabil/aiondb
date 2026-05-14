use aiondb_core::{DbError, DbResult, SqlState};
use std::collections::HashSet;

use crate::{
    ast::{CopyDirection, CopyStatement, ObjectName, Statement},
    keywords::Keyword,
    span::Span,
    tokens::TokenKind,
    Parser,
};

impl Parser {
    /// Parses `COPY table [(col, ...)] FROM STDIN`, `COPY table [(col, ...)] TO STDOUT`
    /// or `COPY (query) TO STDOUT`.
    ///
    /// File endpoints and advanced COPY options are validated later by the
    /// compatibility layer using the original SQL text.
    pub(crate) fn parse_copy_statement(&mut self) -> DbResult<Statement> {
        let copy_token = self.expect_keyword(Keyword::Copy)?;

        // Reject old-style BINARY keyword (COPY BINARY table ...)
        if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("binary"))
        {
            return self.unsupported_feature(
                copy_token.span,
                "COPY BINARY is not supported; only text format is supported",
            );
        }

        let mut query = None;
        let (table, mut span, columns) = if self.current().kind == TokenKind::LParen {
            let lparen = self.expect_token(&TokenKind::LParen)?;
            let inner_start = lparen.span.end;
            let mut depth = 1u32;
            let mut closing_rparen = None;
            while !self.is_eof() {
                match self.current().kind {
                    TokenKind::LParen => {
                        depth += 1;
                    }
                    TokenKind::RParen => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            closing_rparen = Some(self.current().clone());
                            break;
                        }
                    }
                    _ => {}
                }
                self.advance();
            }
            let rparen = closing_rparen.ok_or_else(|| {
                DbError::parse_error(SqlState::SyntaxError, "unterminated COPY query")
            })?;
            let inner_end = rparen.span.start;
            let inner_sql = self.source[inner_start..inner_end].trim();
            if inner_sql.is_empty() {
                return self.syntax_error_current("COPY query must not be empty");
            }
            let inner_statement = crate::parse_prepared_statement(inner_sql)?;
            if matches!(&inner_statement, Statement::Merge(_)) {
                return self.unsupported_feature(copy_token.span, "MERGE not supported in COPY");
            }
            if matches!(
                &inner_statement,
                Statement::Insert(insert) if insert.returning.is_empty()
            ) || matches!(
                &inner_statement,
                Statement::Update(update) if update.returning.is_empty()
            ) || matches!(
                &inner_statement,
                Statement::Delete(delete) if delete.returning.is_empty()
            ) {
                return self.unsupported_feature(
                    copy_token.span,
                    "COPY query must have a RETURNING clause",
                );
            }
            query = Some(Box::new(inner_statement));
            let _ = self.expect_token(&TokenKind::RParen)?;
            // `CopyStatement` reuses the `table` slot to carry the qualified
            // target name. For `COPY (query) TO STDOUT` there is no table, so
            // we plant a sentinel name that downstream layers can detect (or
            // ignore once they see `query.is_some()`); double-underscore makes
            // accidental collision with a real identifier impossible.
            let table = ObjectName {
                parts: vec!["__copy_query__".to_owned()],
                span: copy_token.span,
            };
            (table, copy_token.span.merge(rparen.span), Vec::new())
        } else {
            let table = self.parse_object_name()?;
            let mut span = copy_token.span.merge(table.span);

            // Optional column list: (col1, col2, ...)
            let mut columns = Vec::new();
            if self.consume_kind(&TokenKind::LParen) {
                let mut seen = HashSet::new();
                loop {
                    let (name, _col_span) = self.expect_identifier()?;
                    if !seen.insert(name.to_ascii_lowercase()) {
                        return Err(DbError::parse_error(
                            SqlState::DuplicateColumn,
                            format!("column \"{name}\" specified more than once"),
                        ));
                    }
                    columns.push(name);
                    if !self.consume_kind(&TokenKind::Comma) {
                        break;
                    }
                }
                let rparen = self.expect_token(&TokenKind::RParen)?;
                span = span.merge(rparen.span);
            }
            (table, span, columns)
        };

        // Direction: FROM or TO
        let direction = if let Some(from_token) = self.consume_keyword(Keyword::From) {
            if query.is_some() {
                return copy_syntax_error_at_token(from_token.span, &from_token.kind);
            }
            if self.consume_keyword(Keyword::Stdin).is_some() {
                span = span.merge(self.previous_span());
                CopyDirection::From
            } else {
                // COPY ... FROM 'filename' - file-based, unsupported.
                // STDOUT after FROM is a syntax error (PG rejects it).
                return self.unsupported_feature(
                    copy_token.span,
                    "COPY FROM file is not supported; use COPY FROM STDIN",
                );
            }
        } else if self.consume_keyword(Keyword::To).is_some() {
            if self.consume_keyword(Keyword::Stdout).is_some() {
                span = span.merge(self.previous_span());
                CopyDirection::To
            } else {
                // COPY ... TO 'filename' - file-based, unsupported
                return self.unsupported_feature(
                    copy_token.span,
                    "COPY TO file is not supported; use COPY TO STDOUT",
                );
            }
        } else {
            if query.is_some() {
                return copy_syntax_error_at_token(self.current().span, &self.current().kind);
            }
            return self.syntax_error_current("expected FROM or TO after COPY source");
        };

        if query.is_some() && direction != CopyDirection::To {
            return self
                .unsupported_feature(copy_token.span, "COPY (query) TO STDOUT is not supported");
        }

        while !self.is_eof() && self.current().kind != TokenKind::Semicolon {
            span = span.merge(self.advance().span);
        }

        Ok(Statement::Copy(CopyStatement {
            table,
            columns,
            query,
            direction,
            span,
        }))
    }
}

fn copy_syntax_error_at_token<T>(span: Span, kind: &TokenKind) -> DbResult<T> {
    Err(DbError::syntax_error(format!(
        "syntax error at or near \"{}\"",
        copy_error_token_text(kind)
    ))
    .with_position(span.start + 1))
}

fn copy_error_token_text(kind: &TokenKind) -> String {
    match kind {
        TokenKind::Keyword(keyword) => keyword.name().to_ascii_lowercase(),
        TokenKind::Identifier(value) => value.clone(),
        TokenKind::Integer(value) => value.to_string(),
        TokenKind::NumericLiteral(value) => value.clone(),
        TokenKind::String(_) => "string".to_owned(),
        TokenKind::LParen => "(".to_owned(),
        TokenKind::RParen => ")".to_owned(),
        TokenKind::Comma => ",".to_owned(),
        TokenKind::Semicolon => ";".to_owned(),
        TokenKind::Eof => "end of input".to_owned(),
        _ => kind.describe().trim_matches('\'').to_owned(),
    }
}
