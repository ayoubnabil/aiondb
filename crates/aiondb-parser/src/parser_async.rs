//! Parsers for asynchronous notification statements: `LISTEN`, `UNLISTEN`,
//! and `NOTIFY`.
//!
//! Entry dispatch lives in `lib.rs`; these handlers are invoked once the
//! driver has identified the leading identifier token. They consume tokens
//! up to the next semicolon and produce dedicated [`Statement`] variants so
//! downstream layers (engine, pgwire) can implement pub/sub semantics

use aiondb_core::DbResult;

use crate::{ast::Statement, tokens::TokenKind, Parser};

/// PostgreSQL caps channel names at `NAMEDATALEN - 1` bytes (63).
const CHANNEL_NAME_MAX: usize = 63;

impl Parser {
    /// Parse `LISTEN <channel>`.
    pub(crate) fn parse_listen_statement(&mut self) -> DbResult<Statement> {
        let start = self.advance_span();
        let (channel, channel_span) = self.expect_identifier()?;
        if channel.len() > CHANNEL_NAME_MAX {
            return self.syntax_error(channel_span, "channel name too long");
        }
        let mut span = start.merge(channel_span);
        self.skip_to_semicolon(&mut span);
        Ok(Statement::Listen { channel, span })
    }

    /// Parse `UNLISTEN <channel>` or `UNLISTEN *`.
    pub(crate) fn parse_unlisten_statement(&mut self) -> DbResult<Statement> {
        let start = self.advance_span();
        let mut span = start;
        let channel = if matches!(self.current().kind, TokenKind::Star) {
            span = span.merge(self.advance_span());
            None
        } else {
            let (name, name_span) = self.expect_identifier()?;
            if name.len() > CHANNEL_NAME_MAX {
                return self.syntax_error(name_span, "channel name too long");
            }
            span = span.merge(name_span);
            Some(name)
        };
        self.skip_to_semicolon(&mut span);
        Ok(Statement::Unlisten { channel, span })
    }

    /// Parse `NOTIFY <channel>` or `NOTIFY <channel>, 'payload'`.
    pub(crate) fn parse_notify_statement(&mut self) -> DbResult<Statement> {
        let start = self.advance_span();
        let (channel, channel_span) = self.expect_identifier()?;
        if channel.len() > CHANNEL_NAME_MAX {
            return self.syntax_error(channel_span, "channel name too long");
        }
        let mut span = start.merge(channel_span);
        let payload = if matches!(self.current().kind, TokenKind::Comma) {
            self.advance();
            match self.current().kind.clone() {
                TokenKind::String(s) => {
                    span = span.merge(self.advance_span());
                    Some(s)
                }
                _ => {
                    return self.syntax_error_current("NOTIFY payload must be a string literal");
                }
            }
        } else {
            None
        };
        self.skip_to_semicolon(&mut span);
        Ok(Statement::Notify {
            channel,
            payload,
            span,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_sql;

    #[test]
    fn parses_listen_with_identifier() {
        let stmts = parse_sql("LISTEN events").expect("parse");
        assert_eq!(stmts.len(), 1);
        match &stmts[0] {
            Statement::Listen { channel, .. } => assert_eq!(channel, "events"),
            other => panic!("expected Listen, got {other:?}"),
        }
    }

    #[test]
    fn parses_unlisten_wildcard() {
        let stmts = parse_sql("UNLISTEN *;").expect("parse");
        assert_eq!(stmts.len(), 1);
        match &stmts[0] {
            Statement::Unlisten { channel, .. } => assert!(channel.is_none()),
            other => panic!("expected Unlisten *, got {other:?}"),
        }
    }

    #[test]
    fn parses_unlisten_named() {
        let stmts = parse_sql("UNLISTEN events;").expect("parse");
        assert_eq!(stmts.len(), 1);
        match &stmts[0] {
            Statement::Unlisten { channel, .. } => {
                assert_eq!(channel.as_deref(), Some("events"));
            }
            other => panic!("expected Unlisten, got {other:?}"),
        }
    }

    #[test]
    fn parses_notify_with_payload() {
        let stmts = parse_sql("NOTIFY events, 'hi'").expect("parse");
        assert_eq!(stmts.len(), 1);
        match &stmts[0] {
            Statement::Notify {
                channel, payload, ..
            } => {
                assert_eq!(channel, "events");
                assert_eq!(payload.as_deref(), Some("hi"));
            }
            other => panic!("expected Notify, got {other:?}"),
        }
    }

    #[test]
    fn parses_notify_without_payload() {
        let stmts = parse_sql("NOTIFY events").expect("parse");
        assert_eq!(stmts.len(), 1);
        match &stmts[0] {
            Statement::Notify {
                channel, payload, ..
            } => {
                assert_eq!(channel, "events");
                assert!(payload.is_none());
            }
            other => panic!("expected Notify, got {other:?}"),
        }
    }

    #[test]
    fn rejects_notify_with_non_string_payload() {
        let err = parse_sql("NOTIFY events, 42").expect_err("should fail");
        assert!(err
            .report()
            .message
            .to_ascii_lowercase()
            .contains("payload"));
    }
}
