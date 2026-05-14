use aiondb_core::{DataType, DbResult};

use crate::{
    ast::{
        FunctionGrantTarget, GrantStatement, GrantTarget, Privilege, RevokeStatement, Statement,
    },
    keywords::Keyword,
    tokens::TokenKind,
    Parser,
};

impl Parser {
    // GRANT <privileges> ON <target> TO <role>
    // GRANT <role> TO <role> [WITH ADMIN OPTION] [GRANTED BY ...]
    pub(crate) fn parse_grant_statement(&mut self) -> DbResult<Statement> {
        let grant = self.expect_keyword(Keyword::Grant)?;
        let saved = self.index;
        let Ok(privileges) = self.parse_privilege_list() else {
            // Not a privilege list - could be GRANT role TO role
            self.index = saved;
            // Try to parse as role membership: GRANT <role> TO <role>
            return self.parse_grant_role_membership(grant);
        };
        if self.consume_keyword(Keyword::On).is_none() {
            // No ON keyword - this is GRANT role TO role (role membership)
            self.index = saved;
            return self.parse_grant_role_membership(grant);
        }
        let target = self.parse_grant_target()?;
        self.consume_additional_acl_targets()?;
        if self.consume_keyword(Keyword::To).is_none() {
            return self.syntax_error_current("expected TO after GRANT ... ON ...");
        }
        self.consume_optional_group_keyword();
        let (role_name, role_span) = self.expect_identifier()?;
        // Skip optional WITH GRANT OPTION, GRANTED BY, and any trailing tokens
        let mut end_span = role_span;
        while !self.is_eof() && !matches!(self.current().kind, TokenKind::Semicolon) {
            end_span = self.advance().span;
        }

        Ok(Statement::Grant(GrantStatement {
            privileges,
            target,
            role_name,
            with_admin_option: false,
            granted_by: None,
            span: grant.span.merge(end_span),
        }))
    }

    /// Parse `GRANT <role> TO <role> [WITH ADMIN OPTION] [GRANTED BY ...]`
    /// as a role-target grant so compatibility helpers can observe membership.
    fn parse_grant_role_membership(&mut self, grant: crate::tokens::Token) -> DbResult<Statement> {
        let (granted_role, granted_span) = self.expect_identifier()?;
        self.expect_keyword(Keyword::To)?;
        self.consume_optional_group_keyword();
        let (member_role, member_span) = self.expect_identifier()?;
        let mut end_span = granted_span.merge(member_span);
        let (with_admin_option, granted_by, trailing_end) =
            self.consume_role_grant_trailing_options();
        if let Some(trailing_end) = trailing_end {
            end_span = end_span.merge(trailing_end);
        }
        Ok(Statement::Grant(GrantStatement {
            privileges: vec![Privilege::Usage],
            target: GrantTarget::Role(granted_role),
            role_name: member_role,
            with_admin_option,
            granted_by,
            span: grant.span.merge(end_span),
        }))
    }

    // REVOKE <privileges> ON <target> FROM <role>
    // REVOKE <role> FROM <role>
    pub(crate) fn parse_revoke_statement(&mut self) -> DbResult<Statement> {
        let revoke = self.expect_keyword(Keyword::Revoke)?;
        // Handle REVOKE ADMIN/GRANT/INHERIT OPTION FOR ... FROM ...
        let starts_role_option_revoke = matches!(
            self.current().kind,
            TokenKind::Keyword(Keyword::Grant | Keyword::Inherit | Keyword::Set)
        ) || matches!(
            &self.current().kind,
            TokenKind::Identifier(s)
                if s.eq_ignore_ascii_case("admin")
                    || s.eq_ignore_ascii_case("grant")
                    || s.eq_ignore_ascii_case("inherit")
                    || s.eq_ignore_ascii_case("set")
        );
        if starts_role_option_revoke {
            if let Some(stmt) = self.parse_revoke_role_option_for(&revoke)? {
                return Ok(stmt);
            }
        }
        // Object-privilege syntax:
        // REVOKE GRANT OPTION FOR <priv_list> ON <target> FROM <role> ...
        // Consume the optional "GRANT OPTION FOR" prefix and then parse the
        // normal privilege list.
        let saved_option_prefix = self.index;
        if self.consume_keyword(Keyword::Grant).is_some() {
            let saw_option = matches!(
                &self.current().kind,
                TokenKind::Identifier(s) if s.eq_ignore_ascii_case("option")
            );
            if saw_option {
                self.advance();
                if self.consume_keyword(Keyword::For).is_none() {
                    self.index = saved_option_prefix;
                }
            } else {
                self.index = saved_option_prefix;
            }
        }
        let saved = self.index;
        let Ok(privileges) = self.parse_privilege_list() else {
            self.index = saved;
            return self.parse_revoke_role_membership(revoke);
        };
        if self.consume_keyword(Keyword::On).is_none() {
            // No ON keyword - REVOKE role FROM role (role membership)
            self.index = saved;
            return self.parse_revoke_role_membership(revoke);
        }
        let target = self.parse_grant_target()?;
        self.consume_additional_acl_targets()?;
        if self.consume_keyword(Keyword::From).is_none() {
            return self.syntax_error_current("expected FROM after REVOKE ... ON ...");
        }
        self.consume_optional_group_keyword();
        let (role_name, role_span) = self.expect_identifier()?;
        // Skip optional CASCADE / RESTRICT and any trailing tokens
        let mut end_span = role_span;
        while !self.is_eof() && !matches!(self.current().kind, TokenKind::Semicolon) {
            end_span = self.advance().span;
        }

        Ok(Statement::Revoke(RevokeStatement {
            privileges,
            target,
            role_name,
            with_admin_option: false,
            granted_by: None,
            cascade: false,
            span: revoke.span.merge(end_span),
        }))
    }

    fn parse_revoke_role_membership(
        &mut self,
        revoke: crate::tokens::Token,
    ) -> DbResult<Statement> {
        let (granted_role, granted_span) = self.expect_identifier()?;
        self.expect_keyword(Keyword::From)?;
        self.consume_optional_group_keyword();
        let (member_role, member_span) = self.expect_identifier()?;
        let mut end_span = granted_span.merge(member_span);
        let (_admin_option, granted_by, cascade, trailing_end) =
            self.consume_role_revoke_trailing_options();
        if let Some(e) = trailing_end {
            end_span = end_span.merge(e);
        }
        Ok(Statement::Revoke(RevokeStatement {
            privileges: vec![Privilege::Usage],
            target: GrantTarget::Role(granted_role),
            role_name: member_role,
            with_admin_option: false,
            granted_by,
            cascade,
            span: revoke.span.merge(end_span),
        }))
    }

    fn parse_revoke_role_option_for(
        &mut self,
        revoke: &crate::tokens::Token,
    ) -> DbResult<Option<Statement>> {
        let saved = self.index;
        // Capture which OPTION family is being revoked: ADMIN | GRANT | INHERIT | SET.
        let _option_kind = match &self.current().kind {
            TokenKind::Identifier(s) if s.eq_ignore_ascii_case("admin") => "admin",
            TokenKind::Identifier(s) if s.eq_ignore_ascii_case("grant") => "grant",
            TokenKind::Identifier(s) if s.eq_ignore_ascii_case("inherit") => "inherit",
            TokenKind::Identifier(s) if s.eq_ignore_ascii_case("set") => "set",
            TokenKind::Keyword(Keyword::Grant) => "grant",
            TokenKind::Keyword(Keyword::Inherit) => "inherit",
            TokenKind::Keyword(Keyword::Set) => "set",
            _ => {
                self.index = saved;
                return Ok(None);
            }
        };
        self.advance();
        if !matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("option"))
        {
            self.index = saved;
            return Ok(None);
        }
        self.advance();
        if self.consume_keyword(Keyword::For).is_none() {
            self.index = saved;
            return Ok(None);
        }
        let Ok((granted_role, granted_span)) = self.expect_identifier() else {
            self.index = saved;
            return Ok(None);
        };
        if self.consume_keyword(Keyword::From).is_none() {
            // REVOKE GRANT OPTION FOR ... ON ... (object privileges): not a
            // role-membership option revoke; let normal REVOKE parsing handle it.
            self.index = saved;
            return Ok(None);
        }
        self.consume_optional_group_keyword();
        let Ok((member_role, member_span)) = self.expect_identifier() else {
            self.index = saved;
            return Ok(None);
        };
        let mut end_span = granted_span.merge(member_span);
        let (_admin_double, granted_by, cascade, trailing_end) =
            self.consume_role_revoke_trailing_options();
        if let Some(e) = trailing_end {
            end_span = end_span.merge(e);
        }

        Ok(Some(Statement::Revoke(RevokeStatement {
            privileges: vec![Privilege::Usage],
            target: GrantTarget::Role(granted_role),
            role_name: member_role,
            with_admin_option: true,
            granted_by,
            cascade,
            span: revoke.span.merge(end_span),
        })))
    }

    /// Consume trailing `WITH ADMIN OPTION` / `GRANTED BY <role>` on
    /// `GRANT <role> TO <role>` statements. Returns
    /// `(with_admin_option, granted_by, end_span)`.
    fn consume_role_grant_trailing_options(
        &mut self,
    ) -> (bool, Option<String>, Option<crate::span::Span>) {
        let mut with_admin = false;
        let mut granted_by = None;
        let mut end_span: Option<crate::span::Span> = None;
        loop {
            if self.consume_keyword(Keyword::With).is_some() {
                let family_owned = match &self.current().kind {
                    TokenKind::Identifier(s) if s.eq_ignore_ascii_case("admin") => {
                        Some("admin".to_owned())
                    }
                    TokenKind::Identifier(s) if s.eq_ignore_ascii_case("inherit") => {
                        Some("inherit".to_owned())
                    }
                    TokenKind::Identifier(s) if s.eq_ignore_ascii_case("set") => {
                        Some("set".to_owned())
                    }
                    TokenKind::Keyword(Keyword::Inherit) => Some("inherit".to_owned()),
                    TokenKind::Keyword(Keyword::Set) => Some("set".to_owned()),
                    _ => None,
                };
                if let Some(family) = family_owned {
                    end_span = Some(self.advance().span);
                    if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("option"))
                    {
                        end_span = Some(self.advance().span);
                    }
                    if family == "admin" {
                        with_admin = true;
                    }
                    continue;
                }
                // Unknown WITH clause - stop.
                break;
            }
            let ident_owned = match &self.current().kind {
                TokenKind::Identifier(s) => Some(s.to_owned()),
                _ => None,
            };
            let Some(ident) = ident_owned else {
                break;
            };
            if ident.eq_ignore_ascii_case("granted") {
                let _ = self.advance();
                if self.consume_keyword(Keyword::By).is_some() {
                    if let Ok((role, span)) = self.expect_identifier() {
                        granted_by = Some(role);
                        end_span = Some(span);
                        continue;
                    }
                }
                break;
            }
            break;
        }
        // Skip any remaining trailing tokens up to `;`.
        while !self.is_eof() && !matches!(self.current().kind, TokenKind::Semicolon) {
            end_span = Some(self.advance().span);
        }
        (with_admin, granted_by, end_span)
    }

    /// Consume trailing `GRANTED BY <role>` / `CASCADE` / `RESTRICT` on
    /// `REVOKE <role> FROM <role>` statements. Returns `(admin_option,
    /// granted_by, cascade, end_span)`.
    fn consume_role_revoke_trailing_options(
        &mut self,
    ) -> (bool, Option<String>, bool, Option<crate::span::Span>) {
        let mut granted_by = None;
        let mut cascade = false;
        let mut end_span: Option<crate::span::Span> = None;
        loop {
            if matches!(self.current().kind, TokenKind::Keyword(Keyword::Cascade)) {
                cascade = true;
                end_span = Some(self.advance().span);
                continue;
            }
            if matches!(self.current().kind, TokenKind::Keyword(Keyword::Restrict)) {
                end_span = Some(self.advance().span);
                continue;
            }
            let ident_owned = match &self.current().kind {
                TokenKind::Identifier(s) => Some(s.to_owned()),
                _ => None,
            };
            let Some(ident) = ident_owned else {
                break;
            };
            if ident.eq_ignore_ascii_case("granted") {
                let _ = self.advance();
                if self.consume_keyword(Keyword::By).is_some() {
                    if let Ok((role, span)) = self.expect_identifier() {
                        granted_by = Some(role);
                        end_span = Some(span);
                        continue;
                    }
                }
                break;
            }
            if ident.eq_ignore_ascii_case("cascade") {
                cascade = true;
                end_span = Some(self.advance().span);
                continue;
            }
            if ident.eq_ignore_ascii_case("restrict") {
                end_span = Some(self.advance().span);
                continue;
            }
            break;
        }
        // Skip any remaining trailing tokens up to `;`.
        while !self.is_eof() && !matches!(self.current().kind, TokenKind::Semicolon) {
            end_span = Some(self.advance().span);
        }
        (false, granted_by, cascade, end_span)
    }

    pub(crate) fn parse_privilege_list(&mut self) -> DbResult<Vec<Privilege>> {
        // ALL [PRIVILEGES]
        if self.consume_keyword(Keyword::All).is_some() {
            self.consume_keyword(Keyword::Privileges);
            self.consume_optional_privilege_column_list();
            return Ok(vec![Privilege::All]);
        }

        let mut privileges = Vec::new();
        loop {
            let privilege = if self.consume_keyword(Keyword::Select).is_some() {
                Privilege::Select
            } else if self.consume_keyword(Keyword::Insert).is_some() {
                Privilege::Insert
            } else if self.consume_keyword(Keyword::Update).is_some() {
                Privilege::Update
            } else if self.consume_keyword(Keyword::Delete).is_some() {
                Privilege::Delete
            } else if self.consume_keyword(Keyword::Create).is_some() {
                Privilege::Create
            } else if self.consume_keyword(Keyword::Execute).is_some() {
                Privilege::Execute
            } else if self.consume_keyword(Keyword::Truncate).is_some() {
                Privilege::Truncate
            } else if self.consume_keyword(Keyword::References).is_some() {
                Privilege::References
            } else if self.consume_keyword(Keyword::Trigger).is_some() {
                Privilege::Trigger
            } else if matches!(
                &self.current().kind,
                TokenKind::Identifier(s) if s.eq_ignore_ascii_case("usage")
            ) {
                self.advance();
                Privilege::Usage
            } else if matches!(
                &self.current().kind,
                TokenKind::Identifier(s) if s.eq_ignore_ascii_case("connect")
            ) {
                self.advance();
                Privilege::Connect
            } else if matches!(
                &self.current().kind,
                TokenKind::Identifier(s)
                    if s.eq_ignore_ascii_case("temporary")
                        || s.eq_ignore_ascii_case("temp")
            ) {
                self.advance();
                Privilege::Temporary
            } else {
                return self.syntax_error_current(
                    "expected SELECT, INSERT, UPDATE, DELETE, CREATE, EXECUTE, USAGE, CONNECT, TEMPORARY, TRUNCATE, REFERENCES, TRIGGER, or ALL",
                );
            };
            // Column-scoped grant `(col1, col2, ...)`. The catalog cannot
            // table-wide grant would be a privilege escalation. Reject the
            // statement instead - failing closed is the only safe option
            // until column-level ACLs are wired through.
            if self.current().kind == TokenKind::LParen {
                return self.syntax_error_current(
                    "column-level GRANT/REVOKE not supported; \
                     remove the column list and grant on the table instead",
                );
            }
            privileges.push(privilege);
            if !self.consume_kind(&TokenKind::Comma) {
                break;
            }
        }
        Ok(privileges)
    }

    pub(crate) fn parse_grant_target(&mut self) -> DbResult<GrantTarget> {
        // Skip ALL prefix: ON ALL TABLES/SEQUENCES/FUNCTIONS IN SCHEMA ...
        let saw_all = self.consume_keyword(Keyword::All).is_some();
        if self.consume_keyword(Keyword::Schema).is_some() {
            let (name, _) = self.expect_identifier()?;
            Ok(GrantTarget::Schema(name))
        } else if self.consume_keyword(Keyword::Table).is_some() {
            // Handle: ON [ALL] TABLE[S] [IN SCHEMA <schema>] <name>
            // Skip optional plural 'S' in TABLES
            // (the lexer returns "TABLES" as an identifier, not keyword)
            if self.consume_keyword(Keyword::In).is_some() {
                self.expect_keyword(Keyword::Schema)?;
                let (schema_name, _) = self.expect_identifier()?;
                // ON ALL TABLES IN SCHEMA public - treat as schema-level grant
                return Ok(GrantTarget::Schema(schema_name));
            }
            let name = self.parse_object_name()?;
            Ok(GrantTarget::Table(name))
        } else if matches!(
            &self.current().kind,
            TokenKind::Keyword(
                Keyword::Sequence
                    | Keyword::Type
                    | Keyword::Domain
                    | Keyword::Foreign
                    | Keyword::Language
            )
        ) {
            // Track which keyword started this branch so USAGE on
            // LANGUAGE/TYPE/DOMAIN can be routed to a USAGE-compatible target
            // instead of falling through to TABLE (which rejects USAGE).
            let target_kind = match &self.current().kind {
                TokenKind::Keyword(Keyword::Sequence) => "sequence",
                TokenKind::Keyword(Keyword::Type) => "type",
                TokenKind::Keyword(Keyword::Domain) => "domain",
                TokenKind::Keyword(Keyword::Foreign) => "foreign",
                TokenKind::Keyword(Keyword::Language) => "language",
                _ => unreachable!(),
            };
            self.advance();
            // GRANT ... ON FOREIGN { DATA WRAPPER | SERVER | TABLE } <name>
            // - consume the trailing kind word(s) so `parse_object_name`
            // sees the actual target identifier. DATA / SERVER / TABLE arrive
            // as either keywords or identifiers depending on the lexer's
            // reserved-word set.
            let mut word_lc = String::new();
            let mut foreign_target = false;
            match &self.current().kind {
                TokenKind::Identifier(s)
                    if s.eq_ignore_ascii_case("data")
                        || s.eq_ignore_ascii_case("server")
                        || s.eq_ignore_ascii_case("table") =>
                {
                    word_lc = s.to_ascii_lowercase();
                    foreign_target = true;
                }
                TokenKind::Keyword(kw) if matches!(kw, Keyword::Data | Keyword::Table) => {
                    word_lc = kw.name().to_ascii_lowercase();
                    foreign_target = true;
                }
                _ => {}
            }
            if !word_lc.is_empty() {
                self.advance();
                if word_lc == "data" {
                    let take_wrapper = matches!(
                        &self.current().kind,
                        TokenKind::Identifier(s) if s.eq_ignore_ascii_case("wrapper")
                    ) || matches!(
                        &self.current().kind,
                        TokenKind::Keyword(kw) if kw.name().eq_ignore_ascii_case("wrapper")
                    );
                    if take_wrapper {
                        self.advance();
                    }
                }
            }
            // For GRANT ON FOREIGN DATA WRAPPER / FOREIGN SERVER, route the
            // target through `GrantTarget::Schema` so the planner accepts
            // USAGE (Schema permits USAGE / CREATE). The grant itself is a
            if foreign_target && (word_lc == "data" || word_lc == "server") {
                let name = self.parse_object_name()?;
                let merged = name.parts.join(".");
                return Ok(GrantTarget::Schema(merged));
            }
            // Skip IN SCHEMA clause if present
            if self.consume_keyword(Keyword::In).is_some() {
                self.expect_keyword(Keyword::Schema)?;
                let (schema_name, _) = self.expect_identifier()?;
                return Ok(GrantTarget::Schema(schema_name));
            }
            let name = self.parse_object_name()?;
            if self.current().kind == TokenKind::LParen {
                let mut depth = 1u32;
                self.advance(); // consume '('
                while !self.is_eof() && depth > 0 {
                    match self.current().kind {
                        TokenKind::LParen => {
                            depth += 1;
                            self.advance();
                        }
                        TokenKind::RParen => {
                            depth -= 1;
                            self.advance();
                        }
                        _ => {
                            self.advance();
                        }
                    }
                }
            }
            // For LANGUAGE/TYPE/DOMAIN targets, route through Schema so the
            // planner accepts USAGE (Schema permits USAGE/CREATE). Stash the
            // without polluting the real schema ACL space.
            match target_kind {
                "language" | "type" | "domain" => {
                    let raw = name.parts.join(".");
                    let synthetic = format!("__pgcompat_{target_kind}__{raw}");
                    return Ok(GrantTarget::Schema(synthetic));
                }
                _ => {}
            }
            Ok(GrantTarget::Table(name))
        } else if self.consume_keyword(Keyword::Function).is_some()
            || self.consume_keyword(Keyword::Procedure).is_some()
        {
            if self.consume_keyword(Keyword::In).is_some() {
                self.expect_keyword(Keyword::Schema)?;
                let (schema_name, _) = self.expect_identifier()?;
                return Ok(GrantTarget::Schema(schema_name));
            }
            let name = self.parse_object_name()?;
            let arg_types = self.parse_optional_function_signature_arg_types()?;
            Ok(GrantTarget::Function(FunctionGrantTarget {
                name,
                arg_types,
            }))
        } else if self.consume_keyword(Keyword::Database).is_some() {
            let name = self.parse_object_name()?;
            let database_name = match name.parts.as_slice() {
                [database] => database.clone(),
                _ => name.parts.join("."),
            };
            Ok(GrantTarget::Database(database_name))
        } else if matches!(&self.current().kind, TokenKind::Identifier(s)
            if s.eq_ignore_ascii_case("large")
                || s.eq_ignore_ascii_case("tablespace"))
        {
            // GRANT ... ON LARGE OBJECT 42 TO ... or ON TABLESPACE ... TO ...
            let mut span = self.current().span;
            // Skip until TO keyword
            while !self.is_eof()
                && !matches!(
                    self.current().kind,
                    TokenKind::Keyword(Keyword::To | Keyword::From)
                )
                && !matches!(self.current().kind, TokenKind::Semicolon)
            {
                span = self.advance().span;
            }
            Ok(GrantTarget::Table(crate::ast::ObjectName {
                parts: vec!["__large_object__".to_owned()],
                span,
            }))
        } else if saw_all {
            // ON ALL <something> IN SCHEMA ...
            // Skip identifier like "TABLES", "SEQUENCES", "FUNCTIONS"
            if matches!(
                self.current().kind,
                TokenKind::Identifier(_) | TokenKind::Keyword(_)
            ) {
                self.advance();
            }
            if self.consume_keyword(Keyword::In).is_some() {
                self.expect_keyword(Keyword::Schema)?;
                let (schema_name, _) = self.expect_identifier()?;
                return Ok(GrantTarget::Schema(schema_name));
            }
            let name = self.parse_object_name()?;
            Ok(GrantTarget::Table(name))
        } else {
            // Default to TABLE if no keyword prefix
            let name = self.parse_object_name()?;
            Ok(GrantTarget::Table(name))
        }
    }

    fn parse_optional_function_signature_arg_types(&mut self) -> DbResult<Option<Vec<DataType>>> {
        if !self.consume_kind(&TokenKind::LParen) {
            return Ok(None);
        }

        let mut parsed_types = Vec::new();
        if self.consume_kind(&TokenKind::RParen) {
            return Ok(Some(parsed_types));
        }

        loop {
            loop {
                match &self.current().kind {
                    TokenKind::Keyword(Keyword::In) => {
                        self.advance();
                    }
                    TokenKind::Identifier(s) if s.eq_ignore_ascii_case("out") => {
                        self.advance();
                    }
                    TokenKind::Identifier(s) if s.eq_ignore_ascii_case("inout") => {
                        self.advance();
                    }
                    TokenKind::Identifier(s) if s.eq_ignore_ascii_case("variadic") => {
                        self.advance();
                    }
                    _ => break,
                }
            }

            let parsed = self.parse_function_signature_arg_type()?;
            parsed_types.push(parsed);

            if !self.consume_kind(&TokenKind::Comma) {
                self.expect_token(&TokenKind::RParen)?;
                break;
            }
        }

        Ok(Some(parsed_types))
    }

    fn consume_additional_acl_targets(&mut self) -> DbResult<()> {
        if self.current().kind == TokenKind::Comma {
            return self.unsupported_feature(
                self.current().span,
                "GRANT/REVOKE ... ON with multiple targets is not supported",
            );
        }
        Ok(())
    }

    fn consume_optional_group_keyword(&mut self) {
        if matches!(self.current().kind, TokenKind::Keyword(Keyword::Group))
            || matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("group"))
        {
            self.advance();
        }
    }

    fn consume_optional_privilege_column_list(&mut self) {
        if self.current().kind != TokenKind::LParen {
            return;
        }
        let mut depth = 1u32;
        self.advance(); // consume first '('
        while !self.is_eof() && depth > 0 {
            match self.current().kind {
                TokenKind::LParen => {
                    depth += 1;
                    self.advance();
                }
                TokenKind::RParen => {
                    depth -= 1;
                    self.advance();
                }
                _ => {
                    self.advance();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{ast::GrantTarget, parse_prepared_statement, Statement};

    #[test]
    fn revoke_admin_option_for_parses_as_role_revoke() {
        let stmt = parse_prepared_statement("REVOKE ADMIN OPTION FOR app_role FROM app_user")
            .expect("parse revoke admin option");
        let Statement::Revoke(revoke) = stmt else {
            panic!("expected REVOKE, got: {stmt:?}");
        };
        assert_eq!(revoke.role_name, "app_user");
        assert!(matches!(revoke.target, GrantTarget::Role(ref role) if role == "app_role"));
        assert!(
            revoke.with_admin_option,
            "WITH ADMIN OPTION should propagate to AST"
        );
    }

    #[test]
    fn grant_role_with_admin_option_populates_ast() {
        let stmt = parse_prepared_statement("GRANT admin TO developer WITH ADMIN OPTION")
            .expect("parse grant with admin option");
        let Statement::Grant(grant) = stmt else {
            panic!("expected GRANT, got: {stmt:?}");
        };
        assert!(grant.with_admin_option);
        assert!(grant.granted_by.is_none());
    }

    #[test]
    fn grant_role_with_granted_by_populates_ast() {
        let stmt = parse_prepared_statement("GRANT admin TO developer GRANTED BY root")
            .expect("parse grant granted by");
        let Statement::Grant(grant) = stmt else {
            panic!("expected GRANT, got: {stmt:?}");
        };
        assert_eq!(grant.granted_by.as_deref(), Some("root"));
    }

    #[test]
    fn revoke_role_with_cascade_populates_ast() {
        let stmt = parse_prepared_statement("REVOKE admin FROM developer CASCADE")
            .expect("parse revoke cascade");
        let Statement::Revoke(revoke) = stmt else {
            panic!("expected REVOKE, got: {stmt:?}");
        };
        assert!(revoke.cascade);
    }

    #[test]
    fn revoke_role_with_granted_by_populates_ast() {
        let stmt = parse_prepared_statement("REVOKE admin FROM developer GRANTED BY root")
            .expect("parse revoke granted by");
        let Statement::Revoke(revoke) = stmt else {
            panic!("expected REVOKE, got: {stmt:?}");
        };
        assert_eq!(revoke.granted_by.as_deref(), Some("root"));
    }

    #[test]
    fn revoke_inherit_option_for_parses_as_role_revoke() {
        let stmt = parse_prepared_statement("REVOKE INHERIT OPTION FOR app_role FROM app_user")
            .expect("parse revoke inherit option");
        let Statement::Revoke(revoke) = stmt else {
            panic!("expected REVOKE, got: {stmt:?}");
        };
        assert_eq!(revoke.role_name, "app_user");
        assert!(matches!(revoke.target, GrantTarget::Role(ref role) if role == "app_role"));
    }

    #[test]
    fn revoke_set_option_for_parses_as_role_revoke() {
        let stmt = parse_prepared_statement("REVOKE SET OPTION FOR app_role FROM app_user")
            .expect("parse revoke set option");
        let Statement::Revoke(revoke) = stmt else {
            panic!("expected REVOKE, got: {stmt:?}");
        };
        assert_eq!(revoke.role_name, "app_user");
        assert!(matches!(revoke.target, GrantTarget::Role(ref role) if role == "app_role"));
    }

    #[test]
    fn grant_to_group_parses_member_role_name() {
        let stmt = parse_prepared_statement("GRANT SELECT ON t TO GROUP regress_priv_group2")
            .expect("parse grant to group");
        let Statement::Grant(grant) = stmt else {
            panic!("expected GRANT statement");
        };
        assert_eq!(grant.role_name, "regress_priv_group2");
    }

    #[test]
    fn grant_all_with_column_list_parses() {
        let stmt = parse_prepared_statement("GRANT ALL (one) ON t TO u")
            .expect("parse grant all with column list");
        let Statement::Grant(grant) = stmt else {
            panic!("expected GRANT statement");
        };
        assert_eq!(grant.role_name, "u");
    }

    #[test]
    fn revoke_all_with_column_list_parses() {
        let stmt = parse_prepared_statement("REVOKE ALL (one) ON t FROM u")
            .expect("parse revoke all with column list");
        let Statement::Revoke(revoke) = stmt else {
            panic!("expected REVOKE statement");
        };
        assert_eq!(revoke.role_name, "u");
    }

    #[test]
    fn grant_rejects_multiple_targets() {
        let err = parse_prepared_statement("GRANT SELECT ON t1, t2 TO u")
            .expect_err("expected multiple GRANT targets to be rejected");
        assert!(
            err.to_string()
                .contains("GRANT/REVOKE ... ON with multiple targets is not supported"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn revoke_rejects_multiple_targets() {
        let err = parse_prepared_statement("REVOKE SELECT ON t1, t2 FROM u")
            .expect_err("expected multiple REVOKE targets to be rejected");
        assert!(
            err.to_string()
                .contains("GRANT/REVOKE ... ON with multiple targets is not supported"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn alter_group_add_user_maps_to_grant_role_membership() {
        let stmt = parse_prepared_statement("ALTER GROUP g ADD USER u")
            .expect("parse alter group add user");
        let Statement::Grant(grant) = stmt else {
            panic!("expected GRANT statement");
        };
        assert_eq!(grant.role_name, "u");
        let GrantTarget::Role(target_role) = grant.target else {
            panic!("expected role target");
        };
        assert_eq!(target_role, "g");
    }

    #[test]
    fn alter_group_drop_user_maps_to_revoke_role_membership() {
        let stmt = parse_prepared_statement("ALTER GROUP g DROP USER u")
            .expect("parse alter group drop user");
        let Statement::Revoke(revoke) = stmt else {
            panic!("expected REVOKE statement");
        };
        assert_eq!(revoke.role_name, "u");
        let GrantTarget::Role(target_role) = revoke.target else {
            panic!("expected role target");
        };
        assert_eq!(target_role, "g");
    }

    #[test]
    fn alter_group_add_user_rejects_trailing_clause() {
        let err = parse_prepared_statement("ALTER GROUP g ADD USER u WITH ADMIN OPTION")
            .expect_err("expected trailing clause to be rejected");
        assert!(
            err.to_string()
                .contains("ALTER GROUP ... ADD USER trailing clauses are not supported"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn alter_group_add_user_rejects_multiple_members() {
        let err = parse_prepared_statement("ALTER GROUP g ADD USER u1, u2")
            .expect_err("expected multiple ADD USER members to be rejected");
        assert!(
            err.to_string()
                .contains("ALTER GROUP ... ADD USER with multiple members is not supported"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn alter_group_drop_user_rejects_trailing_clause() {
        let err = parse_prepared_statement("ALTER GROUP g DROP USER u CASCADE")
            .expect_err("expected trailing clause to be rejected");
        assert!(
            err.to_string()
                .contains("ALTER GROUP ... DROP USER trailing clauses are not supported"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn alter_group_drop_user_rejects_multiple_members() {
        let err = parse_prepared_statement("ALTER GROUP g DROP USER u1, u2")
            .expect_err("expected multiple DROP USER members to be rejected");
        assert!(
            err.to_string()
                .contains("ALTER GROUP ... DROP USER with multiple members is not supported"),
            "unexpected error: {err}"
        );
    }
}
