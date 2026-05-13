//! Parser for `SECURITY LABEL [FOR provider] ON object_type name IS
//! { 'label' | NULL }` statements.

use aiondb_core::DbResult;

use crate::{
    ast::{SecurityLabelStatement, SecurityLabelSubject, Statement},
    keywords::Keyword,
    tokens::TokenKind,
    Parser,
};

impl Parser {
    pub(crate) fn parse_security_label_statement(&mut self) -> DbResult<Statement> {
        let security_token = self.advance(); // consume SECURITY
        let mut span = security_token.span;

        // Expect LABEL (keyword or identifier).
        let label_kw_matched = matches!(self.current().kind, TokenKind::Keyword(Keyword::Label))
            || matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("label"));
        if !label_kw_matched {
            return self.syntax_error_current("expected LABEL after SECURITY");
        }
        span = span.merge(self.advance().span);

        // Optional FOR provider
        let provider = if self.consume_keyword(Keyword::For).is_some() {
            let (name, name_span) = self.expect_identifier()?;
            span = span.merge(name_span);
            Some(name)
        } else {
            None
        };

        if self.consume_keyword(Keyword::On).is_none() {
            return self.syntax_error_current("expected ON in SECURITY LABEL");
        }

        // object_type is one or two identifier tokens (e.g. TABLE, COLUMN, ROLE).
        let (type_word, type_span) = match &self.current().kind {
            TokenKind::Identifier(s) => (s.to_ascii_uppercase(), self.advance().span),
            TokenKind::Keyword(kw) => (kw.name().to_ascii_uppercase(), self.advance().span),
            _ => return self.syntax_error_current("expected object type after ON"),
        };
        span = span.merge(type_span);

        // Handle two-word object types: FOREIGN TABLE, MATERIALIZED VIEW.
        let mut object_type = type_word.clone();
        if matches!(type_word.as_str(), "FOREIGN" | "MATERIALIZED")
            && matches!(
                &self.current().kind,
                TokenKind::Identifier(_) | TokenKind::Keyword(_)
            )
        {
            let second = match &self.current().kind {
                TokenKind::Identifier(s) => s.to_ascii_uppercase(),
                TokenKind::Keyword(kw) => kw.name().to_ascii_uppercase(),
                _ => String::new(),
            };
            if !second.is_empty() {
                span = span.merge(self.advance().span);
                object_type = format!("{type_word} {second}");
            }
        }

        // Parse object name. Roles and databases are simple identifiers;
        // tables/columns use qualified names.
        let (subject, sub_span) = if matches!(
            object_type.as_str(),
            "ROLE" | "DATABASE" | "TABLESPACE" | "EVENT TRIGGER" | "LANGUAGE" | "SCHEMA"
        ) {
            let (name, name_span) = self.expect_identifier()?;
            (SecurityLabelSubject::Simple(name), name_span)
        } else {
            let obj = self.parse_object_name()?;
            let obj_span = obj.span;
            (SecurityLabelSubject::Qualified(obj), obj_span)
        };
        span = span.merge(sub_span);

        // IS { 'label' | NULL }
        if self.consume_keyword(Keyword::Is).is_none() {
            return self.syntax_error_current("expected IS after SECURITY LABEL subject");
        }

        let label = if self.consume_keyword(Keyword::Null).is_some() {
            None
        } else {
            match &self.current().kind {
                TokenKind::String(s) => {
                    let value = s.clone();
                    span = span.merge(self.advance().span);
                    Some(value)
                }
                _ => {
                    return self.syntax_error_current("expected quoted label or NULL after IS");
                }
            }
        };

        self.skip_to_semicolon(&mut span);

        Ok(Statement::SecurityLabel(SecurityLabelStatement {
            provider,
            object_type,
            subject,
            label,
            span,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_sql;

    #[test]
    fn parses_security_label_on_role() {
        let stmts = parse_sql("SECURITY LABEL ON ROLE alice IS 'staff';").expect("parse");
        match &stmts[0] {
            Statement::SecurityLabel(s) => {
                assert_eq!(s.object_type, "ROLE");
                assert_eq!(s.label.as_deref(), Some("staff"));
                assert!(matches!(&s.subject, SecurityLabelSubject::Simple(n) if n == "alice"));
            }
            other => panic!("expected SecurityLabel, got {other:?}"),
        }
    }

    #[test]
    fn parses_security_label_null_removes() {
        let stmts = parse_sql("SECURITY LABEL ON TABLE users IS NULL;").expect("parse");
        match &stmts[0] {
            Statement::SecurityLabel(s) => {
                assert!(s.label.is_none());
                assert_eq!(s.object_type, "TABLE");
            }
            other => panic!("expected SecurityLabel, got {other:?}"),
        }
    }

    #[test]
    fn parses_security_label_with_provider() {
        let stmts = parse_sql(
            "SECURITY LABEL FOR selinux ON TABLE t IS 'system_u:object_r:sepgsql_table_t:s0';",
        )
        .expect("parse");
        match &stmts[0] {
            Statement::SecurityLabel(s) => {
                assert_eq!(s.provider.as_deref(), Some("selinux"));
            }
            other => panic!("expected SecurityLabel, got {other:?}"),
        }
    }
}
