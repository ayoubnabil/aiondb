//! Parser for `COMMENT ON object_type name IS { 'text' | NULL };`.

use aiondb_core::DbResult;

use crate::{
    ast::{CommentStatement, CommentSubject, ObjectName, Statement},
    keywords::Keyword,
    tokens::TokenKind,
    Parser,
};

impl Parser {
    pub(crate) fn parse_comment_on_statement(&mut self) -> DbResult<Statement> {
        let comment_token = self.advance(); // consume COMMENT
        let mut span = comment_token.span;

        // Expect ON
        if self.consume_keyword(Keyword::On).is_none() {
            return self.syntax_error_current("expected ON after COMMENT");
        }

        // object_type is one or two identifier tokens (e.g. TABLE, COLUMN,
        // MATERIALIZED VIEW). COMMENT ON OPERATOR <op> (...) is handled as a
        let (type_word, type_span) = match &self.current().kind {
            TokenKind::Identifier(s) => (s.to_ascii_uppercase(), self.advance().span),
            TokenKind::Keyword(kw) => (kw.name().to_ascii_uppercase(), self.advance().span),
            _ => return self.syntax_error_current("expected object type after COMMENT ON"),
        };
        span = span.merge(type_span);

        // Handle multi-word object types: FOREIGN TABLE, MATERIALIZED VIEW,
        // FOREIGN DATA WRAPPER, USER MAPPING [FOR ...], TEXT SEARCH ...,
        // EVENT TRIGGER, ACCESS METHOD.
        let mut object_type = type_word.clone();
        let multi_starts: &[&str] = &["FOREIGN", "MATERIALIZED", "USER", "TEXT", "EVENT", "ACCESS"];
        if multi_starts.contains(&type_word.as_str())
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
                // Three-word object types: FOREIGN DATA WRAPPER, TEXT SEARCH
                // (CONFIGURATION|DICTIONARY|PARSER|TEMPLATE).
                let three_word = matches!(object_type.as_str(), "FOREIGN DATA" | "TEXT SEARCH");
                if three_word
                    && matches!(
                        &self.current().kind,
                        TokenKind::Identifier(_) | TokenKind::Keyword(_)
                    )
                {
                    let third = match &self.current().kind {
                        TokenKind::Identifier(s) => s.to_ascii_uppercase(),
                        TokenKind::Keyword(kw) => kw.name().to_ascii_uppercase(),
                        _ => String::new(),
                    };
                    if !third.is_empty() {
                        span = span.merge(self.advance().span);
                        object_type.push(' ');
                        object_type.push_str(&third);
                    }
                }
            }
        }

        let (subject, sub_span) = if object_type == "USER MAPPING" {
            // COMMENT ON USER MAPPING FOR role SERVER srv IS '...'
            // Consume the role/server tokens and synthesize a name like
            // "<role>@<srv>" so downstream display has something to work
            // with. We tolerate missing pieces to keep the validator
            // tolerant.
            let _ = self.consume_keyword(Keyword::For);
            let mut role: String = String::new();
            let mut role_span = span;
            if matches!(
                &self.current().kind,
                TokenKind::Identifier(_) | TokenKind::Keyword(_)
            ) {
                let tok = self.advance();
                role_span = tok.span;
                role = match tok.kind {
                    TokenKind::Identifier(s) => s,
                    TokenKind::Keyword(kw) => kw.name().to_owned(),
                    _ => String::new(),
                };
            }
            let mut server = String::new();
            if matches!(
                &self.current().kind,
                TokenKind::Identifier(s) if s.eq_ignore_ascii_case("server")
            ) || matches!(
                &self.current().kind,
                TokenKind::Keyword(kw) if kw.name().eq_ignore_ascii_case("server")
            ) {
                let _ = self.advance();
                if matches!(
                    &self.current().kind,
                    TokenKind::Identifier(_) | TokenKind::Keyword(_)
                ) {
                    let tok = self.advance();
                    role_span = role_span.merge(tok.span);
                    server = match tok.kind {
                        TokenKind::Identifier(s) => s,
                        TokenKind::Keyword(kw) => kw.name().to_owned(),
                        _ => String::new(),
                    };
                }
            }
            let synth = format!("{role}@{server}");
            (CommentSubject::Simple(synth), role_span)
        } else if matches!(
            object_type.as_str(),
            "CONSTRAINT" | "TRIGGER" | "RULE" | "POLICY"
        ) {
            let (sub_name, sub_name_span) = self.expect_identifier()?;
            if self.consume_keyword(Keyword::On).is_none() {
                return self.syntax_error_current("expected ON after object name");
            }
            let relation = self.parse_object_name()?;
            let mut parts = relation.parts.clone();
            parts.push(sub_name);
            (
                CommentSubject::Qualified(ObjectName {
                    span: relation.span.merge(sub_name_span),
                    parts,
                }),
                relation.span.merge(sub_name_span),
            )
        } else if matches!(
            object_type.as_str(),
            "ROLE" | "DATABASE" | "TABLESPACE" | "LANGUAGE" | "SCHEMA" | "EXTENSION"
        ) {
            let (name, name_span) = self.expect_identifier()?;
            (CommentSubject::Simple(name), name_span)
        } else {
            let obj = self.parse_object_name()?;
            let mut obj_span = obj.span;
            // COMMENT ON FUNCTION name(args...) accepts a function signature.
            // We keep the base object name in the AST and consume the
            // signature tokens to reach `IS`.
            if object_type == "FUNCTION" && matches!(self.current().kind, TokenKind::LParen) {
                let mut depth = 0usize;
                while !self.is_eof() {
                    let tok = self.advance();
                    obj_span = obj_span.merge(tok.span);
                    match tok.kind {
                        TokenKind::LParen => depth += 1,
                        TokenKind::RParen => {
                            if depth == 0 {
                                break;
                            }
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
            }
            (CommentSubject::Qualified(obj), obj_span)
        };
        span = span.merge(sub_span);

        if self.consume_keyword(Keyword::Is).is_none() {
            return self.syntax_error_current("expected IS after COMMENT ON subject");
        }

        let text = if self.consume_keyword(Keyword::Null).is_some() {
            None
        } else {
            match &self.current().kind {
                TokenKind::String(s) => {
                    let value = s.clone();
                    span = span.merge(self.advance().span);
                    Some(value)
                }
                _ => {
                    return self.syntax_error_current("expected quoted string or NULL after IS");
                }
            }
        };

        self.skip_to_semicolon(&mut span);

        Ok(Statement::Comment(CommentStatement {
            object_type,
            subject,
            text,
            span,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_sql;

    #[test]
    fn parses_comment_on_table() {
        let stmts = parse_sql("COMMENT ON TABLE users IS 'user accounts';").expect("parse");
        match &stmts[0] {
            Statement::Comment(c) => {
                assert_eq!(c.object_type, "TABLE");
                assert_eq!(c.text.as_deref(), Some("user accounts"));
            }
            other => panic!("expected Comment, got {other:?}"),
        }
    }

    #[test]
    fn parses_comment_on_column() {
        let stmts =
            parse_sql("COMMENT ON COLUMN users.email IS 'primary contact email';").expect("parse");
        match &stmts[0] {
            Statement::Comment(c) => {
                assert_eq!(c.object_type, "COLUMN");
                assert!(
                    matches!(&c.subject, CommentSubject::Qualified(n) if n.parts == vec!["users".to_owned(), "email".to_owned()])
                );
            }
            other => panic!("expected Comment, got {other:?}"),
        }
    }

    #[test]
    fn parses_comment_on_role_null_removes() {
        let stmts = parse_sql("COMMENT ON ROLE analyst IS NULL;").expect("parse");
        match &stmts[0] {
            Statement::Comment(c) => {
                assert_eq!(c.object_type, "ROLE");
                assert!(c.text.is_none());
                assert!(matches!(&c.subject, CommentSubject::Simple(n) if n == "analyst"));
            }
            other => panic!("expected Comment, got {other:?}"),
        }
    }

    #[test]
    fn parses_comment_on_function_signature() {
        let stmts = parse_sql("COMMENT ON FUNCTION myfunc(int, text) IS 'doc';").expect("parse");
        match &stmts[0] {
            Statement::Comment(c) => {
                assert_eq!(c.object_type, "FUNCTION");
                assert_eq!(c.text.as_deref(), Some("doc"));
                assert!(
                    matches!(&c.subject, CommentSubject::Qualified(n) if n.parts == vec!["myfunc".to_owned()])
                );
            }
            other => panic!("expected Comment, got {other:?}"),
        }
    }

    #[test]
    fn parses_comment_on_constraint() {
        let stmts =
            parse_sql("COMMENT ON CONSTRAINT users_email_key ON users IS 'email uniqueness';")
                .expect("parse");
        match &stmts[0] {
            Statement::Comment(c) => {
                assert_eq!(c.object_type, "CONSTRAINT");
                assert_eq!(c.text.as_deref(), Some("email uniqueness"));
                assert!(matches!(
                    &c.subject,
                    CommentSubject::Qualified(n)
                    if n.parts
                        == vec!["users".to_owned(), "users_email_key".to_owned()]
                ));
            }
            other => panic!("expected Comment, got {other:?}"),
        }
    }
}
