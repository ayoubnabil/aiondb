#![allow(clippy::collapsible_if)]

use aiondb_core::DbResult;

use crate::{
    ast::{
        ResetVariableStatement, SetConstraintsStatement, SetVariableStatement,
        ShowVariableStatement, Statement,
    },
    keywords::Keyword,
    tokens::TokenKind,
    Parser,
};

impl Parser {
    /// Parse SET: either `SET TENANT <name>` or `SET <var> TO|= <value>`.
    pub(crate) fn parse_set_statement(&mut self) -> DbResult<Statement> {
        let set_token = self.expect_keyword(Keyword::Set)?;
        let is_local = self.consume_keyword(Keyword::Local).is_some();

        // SET TENANT <name> -- existing command, keep compatibility
        if self.consume_keyword(Keyword::Tenant).is_some() {
            let (name, name_span) = self.expect_identifier()?;
            return Ok(Statement::SetTenant {
                name,
                span: set_token.span.merge(name_span),
            });
        }

        // SET CONSTRAINTS { ALL | name, ... } { DEFERRED | IMMEDIATE }
        let matched_constraints = if self.consume_keyword(Keyword::Constraint).is_some() {
            true
        } else if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("constraints"))
        {
            self.advance();
            true
        } else {
            false
        };
        if matched_constraints {
            let mut span = set_token.span.merge(self.previous_span());
            let (all, names) = if self.consume_keyword(Keyword::All).is_some() {
                (true, Vec::new())
            } else {
                let mut names = Vec::new();
                loop {
                    let object = self.parse_object_name()?;
                    span = span.merge(object.span);
                    names.push(object.parts.join("."));
                    if matches!(self.current().kind, TokenKind::Comma) {
                        self.advance();
                        continue;
                    }
                    break;
                }
                (false, names)
            };
            let deferred = if let Some(tok) = self.consume_keyword(Keyword::Deferred) {
                span = span.merge(tok.span);
                true
            } else if let Some(tok) = self.consume_keyword(Keyword::Immediate) {
                span = span.merge(tok.span);
                false
            } else if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("deferred"))
            {
                span = span.merge(self.advance().span);
                true
            } else if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("immediate"))
            {
                span = span.merge(self.advance().span);
                false
            } else {
                return self
                    .syntax_error_current("expected DEFERRED or IMMEDIATE after SET CONSTRAINTS");
            };
            self.skip_to_semicolon(&mut span);
            return Ok(Statement::SetConstraints(SetConstraintsStatement {
                all,
                names,
                deferred,
                span,
            }));
        }

        // SESSION modifier (but reject unsupported SESSION commands explicitly)
        if self.consume_keyword(Keyword::Session).is_some() {
            if self.consume_keyword(Keyword::Authorization).is_some() {
                let (value, value_span) = self.parse_variable_value()?;
                return Ok(Statement::SetVariable(SetVariableStatement {
                    name: "session_authorization".to_owned(),
                    value,
                    is_local,
                    span: set_token.span.merge(value_span),
                }));
            }
            // SET SESSION CHARACTERISTICS AS TRANSACTION ...
            if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("characteristics"))
            {
                let _characteristics = self.advance();
                let _ = self.expect_keyword(Keyword::As)?;
                let transaction = self.expect_keyword(Keyword::Transaction)?;
                let control =
                    self.parse_transaction_options(set_token.span.merge(transaction.span))?;
                if control.isolation.is_none()
                    && control.read_only.is_none()
                    && control.deferrable.is_none()
                {
                    return self.syntax_error(
                        control.span,
                        "SET SESSION CHARACTERISTICS requires at least one transaction option",
                    );
                }
                return Ok(Statement::SetSessionCharacteristics(control));
            }
            // Otherwise SESSION is just a modifier before the variable name
        }

        // SET TIME ZONE <value> (special case: no TO/= required)
        if self.consume_keyword(Keyword::Time).is_some() {
            self.expect_keyword(Keyword::Zone)?;
            if self.consume_keyword(Keyword::Interval).is_some() {
                let (value, val_span) = self.parse_variable_value()?;
                let _ = self.parse_interval_field_spec();
                return Ok(Statement::SetVariable(SetVariableStatement {
                    name: "timezone".to_owned(),
                    value,
                    is_local,
                    span: set_token.span.merge(val_span),
                }));
            }
            let (value, val_span) = self.parse_variable_value()?;
            return Ok(Statement::SetVariable(SetVariableStatement {
                name: "timezone".to_owned(),
                value,
                is_local,
                span: set_token.span.merge(val_span),
            }));
        }

        if self.consume_keyword(Keyword::Role).is_some() {
            let (value, value_span) = self.parse_variable_value()?;
            return Ok(Statement::SetVariable(SetVariableStatement {
                name: "role".to_owned(),
                value,
                is_local,
                span: set_token.span.merge(value_span),
            }));
        }

        // SET NAMES <value> maps to client_encoding.
        if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("names"))
        {
            self.advance();
            let (value, value_span) = self.parse_variable_value()?;
            return Ok(Statement::SetVariable(SetVariableStatement {
                name: "client_encoding".to_owned(),
                value,
                is_local,
                span: set_token.span.merge(value_span),
            }));
        }

        // SET XML OPTION {DOCUMENT|CONTENT} (special case: no TO/= required)
        if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("xml"))
        {
            if self.index + 1 < self.tokens.len()
                && matches!(&self.tokens[self.index + 1].kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("option"))
            {
                self.advance(); // XML
                self.advance(); // OPTION
                let (value, val_span) = self.parse_variable_value()?;
                return Ok(Statement::SetVariable(SetVariableStatement {
                    name: "xmloption".to_owned(),
                    value,
                    is_local,
                    span: set_token.span.merge(val_span),
                }));
            }
        }

        // SET TRANSACTION ...
        if self.consume_keyword(Keyword::Transaction).is_some() {
            let control =
                self.parse_transaction_options(set_token.span.merge(self.previous_span()))?;
            if control.isolation.is_none()
                && control.read_only.is_none()
                && control.deferrable.is_none()
            {
                return self.syntax_error(
                    control.span,
                    "SET TRANSACTION requires at least one transaction option",
                );
            }
            return Ok(Statement::SetTransaction(control));
        }

        // SET <variable_name> TO|= <value>
        let name = self.parse_variable_name()?;

        // Expect TO or =
        if self.consume_keyword(Keyword::To).is_none() && !self.consume_kind(&TokenKind::Eq) {
            return self.syntax_error(
                self.current().span,
                format!("expected TO or = after SET variable '{name}'"),
            );
        }

        let (value, val_span) = self.parse_variable_value()?;
        // Consume additional comma-separated values (e.g., SET search_path TO pg_catalog, public)
        let mut last_span = val_span;
        let mut full_value = value;
        while self.consume_kind(&TokenKind::Comma) {
            let (next_val, next_span) = self.parse_variable_value()?;
            full_value.push_str(", ");
            full_value.push_str(&next_val);
            last_span = next_span;
        }
        Ok(Statement::SetVariable(SetVariableStatement {
            name,
            value: full_value,
            is_local,
            span: set_token.span.merge(last_span),
        }))
    }

    /// Parse SHOW: `SHOW <name>` or `SHOW ALL`.
    pub(crate) fn parse_show_statement(&mut self) -> DbResult<Statement> {
        let show_token = self.expect_keyword(Keyword::Show)?;

        if let Some(all_token) = self.consume_keyword(Keyword::All) {
            return Ok(Statement::ShowVariable(ShowVariableStatement {
                name: "ALL".to_owned(),
                span: show_token.span.merge(all_token.span),
            }));
        }

        if self.consume_keyword(Keyword::Transaction).is_some() {
            let name = if self.consume_keyword(Keyword::Isolation).is_some() {
                self.expect_keyword(Keyword::Level)?;
                "transaction_isolation"
            } else if self.consume_keyword(Keyword::Read).is_some() {
                self.expect_keyword(Keyword::Only)?;
                "transaction_read_only"
            } else if self.consume_keyword(Keyword::Deferrable).is_some() {
                "transaction_deferrable"
            } else {
                return self.syntax_error(
                    self.current().span,
                    "expected ISOLATION LEVEL, READ ONLY, or DEFERRABLE after SHOW TRANSACTION",
                );
            };
            let span = show_token.span.merge(self.previous_span());
            return Ok(Statement::ShowVariable(ShowVariableStatement {
                name: name.to_owned(),
                span,
            }));
        }

        let name = self.parse_variable_name()?;
        let span = show_token.span.merge(self.previous_span());
        Ok(Statement::ShowVariable(ShowVariableStatement {
            name,
            span,
        }))
    }

    /// Parse RESET: `RESET <name>` or `RESET ALL`.
    pub(crate) fn parse_reset_statement(&mut self) -> DbResult<Statement> {
        let reset_token = self.expect_keyword(Keyword::Reset)?;

        if let Some(all_token) = self.consume_keyword(Keyword::All) {
            return Ok(Statement::ResetVariable(ResetVariableStatement {
                name: "ALL".to_owned(),
                span: reset_token.span.merge(all_token.span),
            }));
        }

        if self.consume_keyword(Keyword::Session).is_some() {
            if self.consume_keyword(Keyword::Authorization).is_some() {
                return Ok(Statement::ResetVariable(ResetVariableStatement {
                    name: "session_authorization".to_owned(),
                    span: reset_token.span.merge(self.previous_span()),
                }));
            }
            return self.syntax_error(
                self.previous_span(),
                "expected AUTHORIZATION after RESET SESSION",
            );
        }

        if self.consume_keyword(Keyword::Role).is_some() {
            return Ok(Statement::ResetVariable(ResetVariableStatement {
                name: "role".to_owned(),
                span: reset_token.span.merge(self.previous_span()),
            }));
        }

        if self.consume_keyword(Keyword::Transaction).is_some() {
            let name = if self.consume_keyword(Keyword::Isolation).is_some() {
                self.expect_keyword(Keyword::Level)?;
                "transaction_isolation"
            } else if self.consume_keyword(Keyword::Read).is_some() {
                self.expect_keyword(Keyword::Only)?;
                "transaction_read_only"
            } else if self.consume_keyword(Keyword::Deferrable).is_some() {
                "transaction_deferrable"
            } else {
                return self.syntax_error(
                    self.current().span,
                    "expected ISOLATION LEVEL, READ ONLY, or DEFERRABLE after RESET TRANSACTION",
                );
            };
            let span = reset_token.span.merge(self.previous_span());
            return Ok(Statement::ResetVariable(ResetVariableStatement {
                name: name.to_owned(),
                span,
            }));
        }

        let name = self.parse_variable_name()?;
        let span = reset_token.span.merge(self.previous_span());
        Ok(Statement::ResetVariable(ResetVariableStatement {
            name,
            span,
        }))
    }

    /// Parse a variable name, which can be:
    /// - A simple identifier: `client_encoding`
    /// - A keyword used as identifier: `search_path`
    /// - A dotted name: `pg_catalog.setting`
    /// - Special: `TIME ZONE` -> `timezone`
    fn parse_variable_name(&mut self) -> DbResult<String> {
        // Special case: TIME ZONE -> timezone
        if self.consume_keyword(Keyword::Time).is_some() {
            self.expect_keyword(Keyword::Zone)?;
            return Ok("timezone".to_owned());
        }

        // Accept identifiers and unreserved/semi-reserved keywords as variable names.
        let first = self.parse_variable_name_segment()?;
        let mut name = first;

        // Allow dotted names like pg_catalog.setting
        while self.consume_kind(&TokenKind::Dot) {
            let next = self.parse_variable_name_segment()?;
            name.push('.');
            name.push_str(&next);
        }

        Ok(name)
    }

    /// Parse a single segment of a variable name (identifier or keyword-as-ident).
    /// Variable names are always lowercased for case-insensitive matching.
    fn parse_variable_name_segment(&mut self) -> DbResult<String> {
        match &self.current().kind {
            TokenKind::Identifier(s) => {
                let s = s.to_lowercase();
                self.advance();
                Ok(s)
            }
            TokenKind::Keyword(kw) => {
                // Allow any keyword as variable name segment
                let name = kw.name().to_lowercase();
                self.advance();
                Ok(name)
            }
            _ => Err(self.unexpected_token("variable name")),
        }
    }

    /// Parse a variable value: string literal, integer, float, identifier, or keyword.
    pub(crate) fn parse_variable_value(&mut self) -> DbResult<(String, crate::Span)> {
        // Handle negative numbers: -42
        if self.current().kind == TokenKind::Minus {
            let minus_span = self.current().span;
            self.advance();
            if let TokenKind::Integer(i) = &self.current().kind {
                let s = format!("-{i}");
                let token = self.advance();
                return Ok((s, minus_span.merge(token.span)));
            }
            return Ok(("-".to_owned(), minus_span));
        }
        match &self.current().kind {
            TokenKind::String(s) => {
                let s = s.clone();
                let token = self.advance();
                Ok((s, token.span))
            }
            TokenKind::Integer(i) => {
                let s = i.to_string();
                let token = self.advance();
                // Handle float-like: 1.0 might be tokenized as Integer(1) Dot Integer(0)
                if self.current().kind == TokenKind::Dot {
                    let dot_span = self.current().span;
                    self.advance();
                    if let TokenKind::Integer(frac) = &self.current().kind {
                        let frac_s = frac.to_string();
                        let frac_tok = self.advance();
                        return Ok((format!("{s}.{frac_s}"), token.span.merge(frac_tok.span)));
                    }
                    return Ok((format!("{s}."), token.span.merge(dot_span)));
                }
                Ok((s, token.span))
            }
            TokenKind::Identifier(s) => {
                let s = s.clone();
                let token = self.advance();
                Ok((s, token.span))
            }
            TokenKind::NumericLiteral(s) => {
                let s = s.clone();
                let token = self.advance();
                Ok((s, token.span))
            }
            TokenKind::Keyword(kw) => {
                let s = kw.name().to_lowercase();
                let token = self.advance();
                Ok((s, token.span))
            }
            _ => Err(self.unexpected_token("variable value")),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{parse_prepared_statement, parse_sql, Statement};

    #[test]
    fn set_variable_to_string() {
        let stmt = parse_prepared_statement("SET client_encoding TO 'UTF8'").expect("parse");
        let Statement::SetVariable(s) = stmt else {
            panic!("expected SetVariable");
        };
        assert_eq!(s.name, "client_encoding");
        assert_eq!(s.value, "UTF8");
    }

    #[test]
    fn set_variable_equals_string() {
        let stmt = parse_prepared_statement("SET client_encoding = 'UTF8'").expect("parse");
        let Statement::SetVariable(s) = stmt else {
            panic!("expected SetVariable");
        };
        assert_eq!(s.name, "client_encoding");
        assert_eq!(s.value, "UTF8");
    }

    #[test]
    fn set_names_maps_to_client_encoding() {
        let stmt = parse_prepared_statement("SET NAMES 'utf8'").expect("parse");
        let Statement::SetVariable(s) = stmt else {
            panic!("expected SetVariable");
        };
        assert_eq!(s.name, "client_encoding");
        assert_eq!(s.value, "utf8");
    }

    #[test]
    fn set_variable_to_identifier() {
        let stmt = parse_prepared_statement("SET search_path TO public").expect("parse");
        let Statement::SetVariable(s) = stmt else {
            panic!("expected SetVariable");
        };
        assert_eq!(s.name, "search_path");
        assert_eq!(s.value, "public");
        assert!(!s.is_local);
    }

    #[test]
    fn set_variable_to_integer() {
        let stmt = parse_prepared_statement("SET statement_timeout TO 5000").expect("parse");
        let Statement::SetVariable(s) = stmt else {
            panic!("expected SetVariable");
        };
        assert_eq!(s.name, "statement_timeout");
        assert_eq!(s.value, "5000");
        assert!(!s.is_local);
    }

    #[test]
    fn set_local_variable_marks_local_scope() {
        let stmt = parse_prepared_statement("SET LOCAL statement_timeout TO 5000").expect("parse");
        let Statement::SetVariable(s) = stmt else {
            panic!("expected SetVariable");
        };
        assert_eq!(s.name, "statement_timeout");
        assert_eq!(s.value, "5000");
        assert!(s.is_local);
    }

    #[test]
    fn set_time_zone_special() {
        let stmt = parse_prepared_statement("SET TIME ZONE 'UTC'").expect("parse");
        let Statement::SetVariable(s) = stmt else {
            panic!("expected SetVariable");
        };
        assert_eq!(s.name, "timezone");
        assert_eq!(s.value, "UTC");
    }

    #[test]
    fn set_tenant_still_works() {
        let stmt = parse_prepared_statement("SET TENANT mycompany").expect("parse");
        let Statement::SetTenant { name, .. } = stmt else {
            panic!("expected SetTenant");
        };
        assert_eq!(name, "mycompany");
    }

    #[test]
    fn set_constraints_parses_as_first_class_statement() {
        let stmt =
            parse_prepared_statement("SET CONSTRAINTS ALL IMMEDIATE").expect("SET CONSTRAINTS");
        let Statement::SetConstraints(s) = stmt else {
            panic!("expected SetConstraints");
        };
        assert!(s.all);
        assert!(!s.deferred);
    }

    #[test]
    fn set_session_characteristics_parses_transaction_options() {
        let stmt = parse_prepared_statement(
            "SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL SERIALIZABLE, READ ONLY",
        )
        .expect("parse");
        let Statement::SetSessionCharacteristics(control) = stmt else {
            panic!("expected SetSessionCharacteristics");
        };
        assert_eq!(
            control.isolation,
            Some(crate::ast::TransactionMode::Serializable)
        );
        assert_eq!(control.read_only, Some(true));
        assert_eq!(control.deferrable, None);
    }

    #[test]
    fn set_transaction_parses_transaction_options() {
        let stmt = parse_prepared_statement(
            "SET TRANSACTION ISOLATION LEVEL SNAPSHOT ISOLATION, NOT DEFERRABLE",
        )
        .expect("parse");
        let Statement::SetTransaction(control) = stmt else {
            panic!("expected SetTransaction");
        };
        assert_eq!(
            control.isolation,
            Some(crate::ast::TransactionMode::SnapshotIsolation)
        );
        assert_eq!(control.read_only, None);
        assert_eq!(control.deferrable, Some(false));
    }

    #[test]
    fn set_keyword_as_value() {
        let stmt = parse_prepared_statement("SET DateStyle TO default").expect("parse");
        let Statement::SetVariable(s) = stmt else {
            panic!("expected SetVariable");
        };
        assert_eq!(s.name, "datestyle");
        assert_eq!(s.value, "default");
    }

    #[test]
    fn show_variable() {
        let stmt = parse_prepared_statement("SHOW server_version").expect("parse");
        let Statement::ShowVariable(s) = stmt else {
            panic!("expected ShowVariable");
        };
        assert_eq!(s.name, "server_version");
    }

    #[test]
    fn show_all() {
        let stmt = parse_prepared_statement("SHOW ALL").expect("parse");
        let Statement::ShowVariable(s) = stmt else {
            panic!("expected ShowVariable");
        };
        assert_eq!(s.name, "ALL");
    }

    #[test]
    fn show_time_zone() {
        let stmt = parse_prepared_statement("SHOW TIME ZONE").expect("parse");
        let Statement::ShowVariable(s) = stmt else {
            panic!("expected ShowVariable");
        };
        assert_eq!(s.name, "timezone");
    }

    #[test]
    fn set_time_zone_interval_offset() {
        let stmt = parse_prepared_statement("SET TIME ZONE INTERVAL '+00:00' HOUR TO MINUTE")
            .expect("parse");
        let Statement::SetVariable(s) = stmt else {
            panic!("expected SetVariable");
        };
        assert_eq!(s.name, "timezone");
        assert_eq!(s.value, "+00:00");
    }

    #[test]
    fn show_transaction_isolation_level() {
        let stmt = parse_prepared_statement("SHOW TRANSACTION ISOLATION LEVEL").expect("parse");
        let Statement::ShowVariable(s) = stmt else {
            panic!("expected ShowVariable");
        };
        assert_eq!(s.name, "transaction_isolation");
    }

    #[test]
    fn reset_variable() {
        let stmt = parse_prepared_statement("RESET client_encoding").expect("parse");
        let Statement::ResetVariable(s) = stmt else {
            panic!("expected ResetVariable");
        };
        assert_eq!(s.name, "client_encoding");
    }

    #[test]
    fn reset_transaction_isolation_level_maps_to_variable_reset() {
        let stmt = parse_prepared_statement("RESET TRANSACTION ISOLATION LEVEL").expect("parse");
        let Statement::ResetVariable(s) = stmt else {
            panic!("expected ResetVariable");
        };
        assert_eq!(s.name, "transaction_isolation");
    }

    #[test]
    fn reset_transaction_read_only_maps_to_variable_reset() {
        let stmt = parse_prepared_statement("RESET TRANSACTION READ ONLY").expect("parse");
        let Statement::ResetVariable(s) = stmt else {
            panic!("expected ResetVariable");
        };
        assert_eq!(s.name, "transaction_read_only");
    }

    #[test]
    fn reset_transaction_deferrable_maps_to_variable_reset() {
        let stmt = parse_prepared_statement("RESET TRANSACTION DEFERRABLE").expect("parse");
        let Statement::ResetVariable(s) = stmt else {
            panic!("expected ResetVariable");
        };
        assert_eq!(s.name, "transaction_deferrable");
    }

    #[test]
    fn reset_all() {
        let stmt = parse_prepared_statement("RESET ALL").expect("parse");
        let Statement::ResetVariable(s) = stmt else {
            panic!("expected ResetVariable");
        };
        assert_eq!(s.name, "ALL");
    }

    #[test]
    fn multi_statement_with_set_show_reset() {
        let stmts = parse_sql("SET x TO 1; SHOW x; RESET x").expect("parse");
        assert_eq!(stmts.len(), 3);
        assert!(matches!(stmts[0], Statement::SetVariable(_)));
        assert!(matches!(stmts[1], Statement::ShowVariable(_)));
        assert!(matches!(stmts[2], Statement::ResetVariable(_)));
    }

    #[test]
    fn set_missing_value_error() {
        let result = parse_prepared_statement("SET x TO");
        assert!(result.is_err());
    }

    #[test]
    fn show_missing_name_error() {
        let result = parse_prepared_statement("SHOW");
        assert!(result.is_err());
    }

    #[test]
    fn set_standard_conforming_strings() {
        let stmt =
            parse_prepared_statement("SET standard_conforming_strings TO on").expect("parse");
        let Statement::SetVariable(s) = stmt else {
            panic!("expected SetVariable");
        };
        assert_eq!(s.name, "standard_conforming_strings");
        assert_eq!(s.value, "on");
    }

    #[test]
    fn set_role_maps_to_session_variable() {
        let stmt = parse_prepared_statement("SET ROLE app_user")
            .expect("SET ROLE should parse as a variable assignment");
        let Statement::SetVariable(s) = stmt else {
            panic!("expected SetVariable");
        };
        assert_eq!(s.name, "role");
        assert_eq!(s.value, "app_user");
    }

    #[test]
    fn set_session_authorization_maps_to_variable_assignment() {
        let stmt = parse_prepared_statement("SET SESSION AUTHORIZATION app_user")
            .expect("SET SESSION AUTHORIZATION should parse as a variable assignment");
        let Statement::SetVariable(s) = stmt else {
            panic!("expected SetVariable");
        };
        assert_eq!(s.name, "session_authorization");
        assert_eq!(s.value, "app_user");
    }

    #[test]
    fn reset_role_maps_to_variable_reset() {
        let stmt = parse_prepared_statement("RESET ROLE")
            .expect("RESET ROLE should parse as a variable reset");
        let Statement::ResetVariable(s) = stmt else {
            panic!("expected ResetVariable");
        };
        assert_eq!(s.name, "role");
    }

    #[test]
    fn reset_session_authorization_maps_to_variable_reset() {
        let stmt = parse_prepared_statement("RESET SESSION AUTHORIZATION")
            .expect("RESET SESSION AUTHORIZATION should parse as a variable reset");
        let Statement::ResetVariable(s) = stmt else {
            panic!("expected ResetVariable");
        };
        assert_eq!(s.name, "session_authorization");
    }

    #[test]
    fn malformed_set_without_to_or_equals_is_rejected() {
        let err = parse_prepared_statement("SET search_path public")
            .expect_err("malformed SET must fail");
        assert!(
            format!("{err}").contains("expected TO or = after SET variable 'search_path'"),
            "unexpected error: {err}"
        );
    }
}
