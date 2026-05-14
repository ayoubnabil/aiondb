//! Parsers for `DROP OWNED BY ...` and `REASSIGN OWNED BY ... TO ...`.
//!
//! Both statements used to be emitted as generic compatibility stubs with the
//! raw SQL re-parsed later by ad-hoc text scanners in the engine.  They are
//! now first-class AST nodes so binder/executor work on structured data.

use aiondb_core::DbResult;

use crate::{
    ast::{DropOwnedStatement, ReassignOwnedStatement, Statement},
    keywords::Keyword,
    tokens::TokenKind,
    Parser,
};

impl Parser {
    /// Parse `REASSIGN OWNED BY src[, ...] TO target` starting at the
    /// `REASSIGN` identifier.
    pub(crate) fn parse_reassign_owned_statement(&mut self) -> DbResult<Statement> {
        let start = self.advance(); // consume REASSIGN
        let mut span = start.span;

        if self.consume_keyword(Keyword::Owned).is_none()
            && !self.consume_identifier_eq_ignore_ascii_case("owned")
        {
            return self.syntax_error_current("expected OWNED after REASSIGN");
        }

        if self.consume_keyword(Keyword::By).is_none() {
            return self.syntax_error_current("expected BY after REASSIGN OWNED");
        }

        let mut sources = Vec::new();
        loop {
            let (name, name_span) = self.expect_identifier()?;
            span = span.merge(name_span);
            sources.push(name);
            if matches!(self.current().kind, TokenKind::Comma) {
                self.advance();
                continue;
            }
            break;
        }

        if self.consume_keyword(Keyword::To).is_none() {
            return self.syntax_error_current("expected TO after REASSIGN OWNED BY");
        }

        let (target, target_span) = self.expect_identifier()?;
        span = span.merge(target_span);
        self.skip_to_semicolon(&mut span);

        Ok(Statement::ReassignOwned(ReassignOwnedStatement {
            sources,
            target,
            span,
        }))
    }

    /// Parse the tail of `DROP OWNED BY role[, ...] [CASCADE | RESTRICT]`
    /// after the `DROP` keyword has already been consumed by the caller and
    /// the current token is the `OWNED` identifier.
    pub(crate) fn parse_drop_owned_tail(
        &mut self,
        drop_span: crate::span::Span,
    ) -> DbResult<Statement> {
        let mut span = drop_span.merge(self.current().span);
        self.advance(); // consume OWNED

        if self.consume_keyword(Keyword::By).is_none() {
            return self.syntax_error_current("expected BY after DROP OWNED");
        }

        let mut roles = Vec::new();
        loop {
            let (name, name_span) = self.expect_identifier()?;
            span = span.merge(name_span);
            roles.push(name);
            if matches!(self.current().kind, TokenKind::Comma) {
                self.advance();
                continue;
            }
            break;
        }

        let cascade = if matches!(&self.current().kind, TokenKind::Identifier(s)
            if s.eq_ignore_ascii_case("cascade"))
        {
            span = span.merge(self.advance().span);
            true
        } else if matches!(&self.current().kind, TokenKind::Identifier(s)
            if s.eq_ignore_ascii_case("restrict"))
        {
            span = span.merge(self.advance().span);
            false
        } else {
            false
        };

        self.skip_to_semicolon(&mut span);

        Ok(Statement::DropOwned(DropOwnedStatement {
            roles,
            cascade,
            span,
        }))
    }

    fn consume_identifier_eq_ignore_ascii_case(&mut self, expected: &str) -> bool {
        let matched = matches!(&self.current().kind, TokenKind::Identifier(s)
            if s.eq_ignore_ascii_case(expected));
        if matched {
            self.advance();
        }
        matched
    }
}
