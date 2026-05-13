#![allow(clippy::if_same_then_else)]

use aiondb_core::{DbError, DbResult, SqlState};

use crate::{
    ast::{
        AlterRoleStatement, CreateEdgeLabelStatement, CreateNodeLabelStatement,
        CreateRoleStatement, CreateSchemaStatement, DropEdgeLabelStatement, DropNodeLabelStatement,
        DropRoleStatement, DropSchemaStatement, GrantStatement, GrantTarget, Privilege,
        RevokeStatement, RoleOption, Statement,
    },
    keywords::Keyword,
    tokens::TokenKind,
    Parser,
};

impl Parser {
    // CREATE NODE LABEL <label> ON <table>;
    pub(crate) fn parse_create_node_label_statement(
        &mut self,
        create: crate::tokens::Token,
    ) -> DbResult<Statement> {
        self.expect_keyword(Keyword::Node)?;
        self.expect_keyword(Keyword::Label)?;
        let (label, _label_span) = self.expect_identifier()?;
        self.expect_keyword(Keyword::On)?;
        let table = self.parse_object_name()?;
        let span = create.span.merge(table.span);
        Ok(Statement::CreateNodeLabel(CreateNodeLabelStatement {
            label,
            table,
            span,
        }))
    }

    // CREATE EDGE LABEL <label> ON <table>
    //   SOURCE <src_label> [KEY (<source_column>)]
    //   TARGET <tgt_label> [KEY (<target_column>)];
    pub(crate) fn parse_create_edge_label_statement(
        &mut self,
        create: crate::tokens::Token,
    ) -> DbResult<Statement> {
        self.expect_keyword(Keyword::Edge)?;
        self.expect_keyword(Keyword::Label)?;
        let (label, _label_span) = self.expect_identifier()?;
        self.expect_keyword(Keyword::On)?;
        let table = self.parse_object_name()?;
        self.expect_keyword(Keyword::Source)?;
        let (source_label, _src_span) = self.expect_identifier()?;
        let source_endpoint = self.parse_optional_edge_endpoint_column()?;
        self.expect_keyword(Keyword::Target)?;
        let (target_label, tgt_span) = self.expect_identifier()?;
        let target_endpoint = self.parse_optional_edge_endpoint_column()?;
        let endpoints = match (source_endpoint, target_endpoint) {
            (Some(source), Some(target)) => Some((source, target)),
            (None, None) => None,
            _ => {
                return Err(DbError::parse_error(
                    SqlState::SyntaxError,
                    "CREATE EDGE LABEL must specify KEY (...) for both SOURCE and TARGET, or neither",
                ));
            }
        };
        let span = create.span.merge(tgt_span);
        Ok(Statement::CreateEdgeLabel(CreateEdgeLabelStatement {
            label,
            table,
            source_label,
            target_label,
            endpoints,
            span,
        }))
    }

    fn parse_optional_edge_endpoint_column(&mut self) -> DbResult<Option<String>> {
        if self.consume_keyword(Keyword::Key).is_none() {
            return Ok(None);
        }
        self.expect_token(&TokenKind::LParen)?;
        let (column, _) = self.expect_identifier()?;
        self.expect_token(&TokenKind::RParen)?;
        Ok(Some(column))
    }

    // DROP NODE LABEL <label>;
    pub(crate) fn parse_drop_node_label_statement(
        &mut self,
        drop: crate::tokens::Token,
    ) -> DbResult<Statement> {
        self.expect_keyword(Keyword::Node)?;
        self.expect_keyword(Keyword::Label)?;
        let (label, label_span) = self.expect_identifier()?;
        let span = drop.span.merge(label_span);
        Ok(Statement::DropNodeLabel(DropNodeLabelStatement {
            label,
            span,
        }))
    }

    // DROP EDGE LABEL <label>;
    pub(crate) fn parse_drop_edge_label_statement(
        &mut self,
        drop: crate::tokens::Token,
    ) -> DbResult<Statement> {
        self.expect_keyword(Keyword::Edge)?;
        self.expect_keyword(Keyword::Label)?;
        let (label, label_span) = self.expect_identifier()?;
        let span = drop.span.merge(label_span);
        Ok(Statement::DropEdgeLabel(DropEdgeLabelStatement {
            label,
            span,
        }))
    }

    // CREATE ROLE/USER <name> [WITH] [LOGIN | NOLOGIN | PASSWORD '<pw>' | SUPERUSER | NOSUPERUSER]...
    pub(crate) fn parse_create_role_statement(
        &mut self,
        create: crate::tokens::Token,
    ) -> DbResult<Statement> {
        // Accept ROLE, USER, and GROUP.
        if self.consume_keyword(Keyword::Role).is_none()
            && self.consume_keyword(Keyword::User).is_none()
        {
            self.expect_keyword(Keyword::Group)?;
        }
        let (name, name_span) = self.expect_identifier()?;
        self.consume_keyword(Keyword::With);

        let mut options = Vec::new();
        let mut in_roles: Vec<String> = Vec::new();
        let mut role_members: Vec<String> = Vec::new();
        let mut admin_members: Vec<String> = Vec::new();
        loop {
            if self.consume_keyword(Keyword::Login).is_some() {
                options.push(RoleOption::Login);
            } else if self.consume_keyword(Keyword::Nologin).is_some() {
                options.push(RoleOption::Nologin);
            } else if self.consume_keyword(Keyword::Superuser).is_some() {
                options.push(RoleOption::Superuser);
            } else if self.consume_keyword(Keyword::Nosuperuser).is_some() {
                options.push(RoleOption::Nosuperuser);
            } else if self.consume_keyword(Keyword::Password).is_some() {
                if self.consume_keyword(Keyword::Null).is_some() {
                    options.push(RoleOption::PasswordNull);
                } else {
                    let password = match &self.current().kind {
                        TokenKind::String(s) => s.clone(),
                        _ => {
                            return self
                                .syntax_error_current("expected string literal after PASSWORD");
                        }
                    };
                    self.advance();
                    options.push(RoleOption::Password(password));
                }
            } else if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("encrypted")
                || s.eq_ignore_ascii_case("unencrypted"))
            {
                // ENCRYPTED/UNENCRYPTED PASSWORD - skip qualifier, parse password
                self.advance(); // skip ENCRYPTED/UNENCRYPTED
                self.expect_keyword(Keyword::Password)?;
                if self.consume_keyword(Keyword::Null).is_some() {
                    options.push(RoleOption::PasswordNull);
                } else {
                    let password = match &self.current().kind {
                        TokenKind::String(s) => s.clone(),
                        _ => {
                            return self
                                .syntax_error_current("expected string literal after PASSWORD");
                        }
                    };
                    self.advance();
                    options.push(RoleOption::Password(password));
                }
            } else if self.consume_keyword(Keyword::In).is_some() {
                // IN ROLE/GROUP <name>, ... - record the list.
                if self.consume_keyword(Keyword::Role).is_none()
                    && self.consume_keyword(Keyword::Group).is_none()
                    && matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("group"))
                {
                    self.advance();
                }
                loop {
                    let (role_name, _) = self.expect_identifier()?;
                    in_roles.push(role_name);
                    if !self.consume_kind(&TokenKind::Comma) {
                        break;
                    }
                }
            } else if self.consume_keyword(Keyword::Role).is_some() {
                // ROLE <name>, ... - role membership list.
                loop {
                    let (role_name, _) = self.expect_identifier()?;
                    role_members.push(role_name);
                    if !self.consume_kind(&TokenKind::Comma) {
                        break;
                    }
                }
            } else if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("admin"))
            {
                // ADMIN <name>, ... - record as admin members.
                self.advance(); // skip ADMIN
                loop {
                    let (role_name, _) = self.expect_identifier()?;
                    admin_members.push(role_name);
                    if !self.consume_kind(&TokenKind::Comma) {
                        break;
                    }
                }
            } else if self.consume_keyword(Keyword::User).is_some() {
                // USER <name>, ... - role membership spelling.
                loop {
                    let (role_name, _) = self.expect_identifier()?;
                    role_members.push(role_name);
                    if !self.consume_kind(&TokenKind::Comma) {
                        break;
                    }
                }
            } else if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("user"))
            {
                // USER <name>, ... - role membership spelling.
                self.advance(); // skip USER identifier form
                loop {
                    let (role_name, _) = self.expect_identifier()?;
                    role_members.push(role_name);
                    if !self.consume_kind(&TokenKind::Comma) {
                        break;
                    }
                }
            } else if let Some(opt) = match &self.current().kind {
                TokenKind::Identifier(s) if s.eq_ignore_ascii_case("createdb") => {
                    Some(RoleOption::Createdb)
                }
                TokenKind::Identifier(s) if s.eq_ignore_ascii_case("nocreatedb") => {
                    Some(RoleOption::Nocreatedb)
                }
                TokenKind::Identifier(s) if s.eq_ignore_ascii_case("createrole") => {
                    Some(RoleOption::Createrole)
                }
                TokenKind::Identifier(s) if s.eq_ignore_ascii_case("nocreaterole") => {
                    Some(RoleOption::Nocreaterole)
                }
                TokenKind::Identifier(s) if s.eq_ignore_ascii_case("replication") => {
                    Some(RoleOption::Replication)
                }
                TokenKind::Identifier(s) if s.eq_ignore_ascii_case("noreplication") => {
                    Some(RoleOption::Noreplication)
                }
                TokenKind::Identifier(s) if s.eq_ignore_ascii_case("bypassrls") => {
                    Some(RoleOption::Bypassrls)
                }
                TokenKind::Identifier(s) if s.eq_ignore_ascii_case("nobypassrls") => {
                    Some(RoleOption::Nobypassrls)
                }
                _ => None,
            } {
                self.advance();
                options.push(opt);
            } else if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("sysid"))
            {
                let span = self.current().span;
                return self.unsupported_feature(
                    span,
                    "CREATE ROLE ... SYSID is not supported".to_string(),
                );
            } else if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("connection"))
            {
                self.advance(); // CONNECTION
                if self.consume_keyword(Keyword::Limit).is_none() {
                    return self.syntax_error_current("expected LIMIT after CONNECTION");
                }
                let mut negative = false;
                if self.consume_kind(&TokenKind::Minus) {
                    negative = true;
                }
                let TokenKind::Integer(i) = &self.current().kind else {
                    return self.syntax_error_current("expected integer after CONNECTION LIMIT");
                };
                let v = *i;
                let v = if negative { -v } else { v };
                options.push(RoleOption::ConnectionLimit(v));
                self.advance();
            } else if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("valid"))
            {
                self.advance(); // VALID
                if !matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("until"))
                {
                    return self.syntax_error_current("expected UNTIL after VALID");
                }
                self.advance(); // UNTIL
                let TokenKind::String(s) = &self.current().kind else {
                    return self.syntax_error_current("expected string literal after VALID UNTIL");
                };
                let s = s.clone();
                options.push(RoleOption::ValidUntil(s));
                self.advance();
            } else if self.consume_keyword(Keyword::Inherit).is_some() {
                options.push(RoleOption::Inherit);
            } else if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("noinherit"))
            {
                self.advance();
                options.push(RoleOption::Noinherit);
            } else {
                break;
            }
        }

        if !matches!(self.current().kind, TokenKind::Semicolon | TokenKind::Eof) {
            return self.unsupported_feature(
                self.current().span,
                "CREATE ROLE clause is not supported".to_string(),
            );
        }

        let span_end = if options.is_empty() {
            name_span
        } else {
            self.previous_span()
        };

        Ok(Statement::CreateRole(CreateRoleStatement {
            name,
            options,
            in_roles,
            role_members,
            admin_members,
            span: create.span.merge(span_end),
        }))
    }

    // DROP ROLE/USER <name>
    pub(crate) fn parse_drop_role_statement(
        &mut self,
        drop: crate::tokens::Token,
    ) -> DbResult<Statement> {
        // Accept ROLE, USER, and GROUP.
        if self.consume_keyword(Keyword::Role).is_none()
            && self.consume_keyword(Keyword::User).is_none()
        {
            self.expect_keyword(Keyword::Group)?;
        }
        // Optional IF EXISTS
        let if_exists = if self.consume_keyword(Keyword::If).is_some() {
            self.expect_keyword(Keyword::Exists)?;
            true
        } else {
            false
        };
        let (name, name_span) = self.expect_identifier()?;
        Ok(Statement::DropRole(DropRoleStatement {
            name,
            if_exists,
            span: drop.span.merge(name_span),
        }))
    }

    // ALTER ROLE/USER <name> [WITH] (LOGIN | NOLOGIN | PASSWORD '<pw>' | SUPERUSER | NOSUPERUSER | ...)
    pub(crate) fn parse_alter_role_statement(
        &mut self,
        alter: crate::tokens::Token,
    ) -> DbResult<Statement> {
        // Accept ROLE, USER, and GROUP.
        let is_group_target = if self.consume_keyword(Keyword::Role).is_some() {
            false
        } else if self.consume_keyword(Keyword::User).is_some() {
            false
        } else {
            self.expect_keyword(Keyword::Group)?;
            true
        };
        let (name, name_span) = self.expect_identifier()?;

        if is_group_target {
            if self.consume_keyword(Keyword::Add).is_some() {
                if self.consume_keyword(Keyword::User).is_none()
                    && !matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("user"))
                {
                    return self.syntax_error_current("expected USER after ALTER GROUP ... ADD");
                }
                if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("user"))
                {
                    self.advance();
                }
                let (role_name, role_span) = self.expect_identifier()?;
                let end_span = role_span;
                if self.consume_kind(&TokenKind::Comma) {
                    let span = self.previous_span();
                    return self.unsupported_feature(
                        span,
                        "ALTER GROUP ... ADD USER with multiple members is not supported"
                            .to_string(),
                    );
                }
                if !matches!(self.current().kind, TokenKind::Semicolon | TokenKind::Eof) {
                    return self.unsupported_feature(
                        self.current().span,
                        "ALTER GROUP ... ADD USER trailing clauses are not supported".to_string(),
                    );
                }
                return Ok(Statement::Grant(GrantStatement {
                    privileges: vec![Privilege::Usage],
                    target: GrantTarget::Role(name),
                    role_name,
                    with_admin_option: false,
                    granted_by: None,
                    span: alter.span.merge(end_span),
                }));
            }
            if self.consume_keyword(Keyword::Drop).is_some() {
                if self.consume_keyword(Keyword::User).is_none()
                    && !matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("user"))
                {
                    return self.syntax_error_current("expected USER after ALTER GROUP ... DROP");
                }
                if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("user"))
                {
                    self.advance();
                }
                let (role_name, role_span) = self.expect_identifier()?;
                let end_span = role_span;
                if self.consume_kind(&TokenKind::Comma) {
                    let span = self.previous_span();
                    return self.unsupported_feature(
                        span,
                        "ALTER GROUP ... DROP USER with multiple members is not supported"
                            .to_string(),
                    );
                }
                if !matches!(self.current().kind, TokenKind::Semicolon | TokenKind::Eof) {
                    return self.unsupported_feature(
                        self.current().span,
                        "ALTER GROUP ... DROP USER trailing clauses are not supported".to_string(),
                    );
                }
                return Ok(Statement::Revoke(RevokeStatement {
                    privileges: vec![Privilege::Usage],
                    target: GrantTarget::Role(name),
                    role_name,
                    with_admin_option: false,
                    granted_by: None,
                    cascade: false,
                    span: alter.span.merge(end_span),
                }));
            }
        }

        // ALTER ROLE ... SET/RESET role-level configuration: AionDB has no
        // per-role GUC store yet, but ORM migration tools (Diesel,
        // SQLAlchemy, sqlx) routinely emit these to fix search_path or
        // statement_timeout. Accept and discard the rest of the statement
        // so the migration script keeps moving instead of erroring out.
        if matches!(
            self.current().kind,
            TokenKind::Keyword(Keyword::Set | Keyword::Reset)
        ) {
            let mut span = alter.span;
            self.skip_to_semicolon(&mut span);
            return Ok(Statement::AlterRole(AlterRoleStatement {
                name: name.clone(),
                options: Vec::new(),
                span,
            }));
        }
        // ALTER ROLE ... IN DATABASE ... SET ... is not supported yet.
        if self.consume_keyword(Keyword::In).is_some() {
            if self.consume_keyword(Keyword::Database).is_none() {
                let span = self.previous_span();
                return self.unsupported_feature(
                    span,
                    "ALTER ROLE ... IN ROLE/IN GROUP is not supported".to_string(),
                );
            }
            let span = self.previous_span();
            return self.unsupported_feature(
                span,
                "ALTER ROLE ... IN DATABASE ... SET is not supported".to_string(),
            );
        }
        // ALTER ROLE name RENAME TO new_name → first-class AST node.
        if self.consume_keyword(Keyword::Rename).is_some() {
            self.expect_keyword(Keyword::To)?;
            let (target, target_span) = self.expect_identifier()?;
            let mut span = alter.span.merge(target_span);
            self.skip_to_semicolon(&mut span);
            return Ok(Statement::AlterRoleRename(
                crate::ast::AlterRoleRenameStatement {
                    source: name.clone(),
                    target,
                    span,
                },
            ));
        }

        self.consume_keyword(Keyword::With);

        let mut options = Vec::new();
        let mut consumed_anything = false;
        loop {
            if self.consume_keyword(Keyword::Login).is_some() {
                options.push(RoleOption::Login);
                consumed_anything = true;
            } else if self.consume_keyword(Keyword::Nologin).is_some() {
                options.push(RoleOption::Nologin);
                consumed_anything = true;
            } else if self.consume_keyword(Keyword::Superuser).is_some() {
                options.push(RoleOption::Superuser);
                consumed_anything = true;
            } else if self.consume_keyword(Keyword::Nosuperuser).is_some() {
                options.push(RoleOption::Nosuperuser);
                consumed_anything = true;
            } else if self.consume_keyword(Keyword::Password).is_some() {
                consumed_anything = true;
                if self.consume_keyword(Keyword::Null).is_some() {
                    options.push(RoleOption::PasswordNull);
                } else {
                    let password = match &self.current().kind {
                        TokenKind::String(s) => s.clone(),
                        _ => {
                            return self
                                .syntax_error_current("expected string literal after PASSWORD");
                        }
                    };
                    self.advance();
                    options.push(RoleOption::Password(password));
                }
            } else if self.consume_keyword(Keyword::Inherit).is_some() {
                consumed_anything = true;
                options.push(RoleOption::Inherit);
            } else if self.consume_keyword(Keyword::In).is_some() {
                let span = self.previous_span();
                return self.unsupported_feature(
                    span,
                    "ALTER ROLE ... IN ROLE/IN GROUP is not supported".to_string(),
                );
            } else if self.consume_keyword(Keyword::Role).is_some() {
                let span = self.previous_span();
                return self.unsupported_feature(
                    span,
                    "ALTER ROLE ... ROLE <member_list> is not supported".to_string(),
                );
            } else if matches!(&self.current().kind, TokenKind::Identifier(s)
                if s.eq_ignore_ascii_case("admin") || s.eq_ignore_ascii_case("user"))
                || matches!(self.current().kind, TokenKind::Keyword(Keyword::User))
            {
                let span = self.current().span;
                return self.unsupported_feature(
                    span,
                    "ALTER ROLE ... ADMIN/USER <member_list> is not supported".to_string(),
                );
            } else if let Some(opt) = match &self.current().kind {
                TokenKind::Identifier(s) if s.eq_ignore_ascii_case("noinherit") => {
                    Some(RoleOption::Noinherit)
                }
                TokenKind::Identifier(s) if s.eq_ignore_ascii_case("createdb") => {
                    Some(RoleOption::Createdb)
                }
                TokenKind::Identifier(s) if s.eq_ignore_ascii_case("nocreatedb") => {
                    Some(RoleOption::Nocreatedb)
                }
                TokenKind::Identifier(s) if s.eq_ignore_ascii_case("createrole") => {
                    Some(RoleOption::Createrole)
                }
                TokenKind::Identifier(s) if s.eq_ignore_ascii_case("nocreaterole") => {
                    Some(RoleOption::Nocreaterole)
                }
                TokenKind::Identifier(s) if s.eq_ignore_ascii_case("replication") => {
                    Some(RoleOption::Replication)
                }
                TokenKind::Identifier(s) if s.eq_ignore_ascii_case("noreplication") => {
                    Some(RoleOption::Noreplication)
                }
                TokenKind::Identifier(s) if s.eq_ignore_ascii_case("bypassrls") => {
                    Some(RoleOption::Bypassrls)
                }
                TokenKind::Identifier(s) if s.eq_ignore_ascii_case("nobypassrls") => {
                    Some(RoleOption::Nobypassrls)
                }
                _ => None,
            } {
                self.advance();
                options.push(opt);
                consumed_anything = true;
            } else if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("connection"))
            {
                self.advance(); // CONNECTION
                consumed_anything = true;
                if self.consume_keyword(Keyword::Limit).is_none() {
                    return self.syntax_error_current("expected LIMIT after CONNECTION");
                }
                let mut negative = false;
                if self.consume_kind(&TokenKind::Minus) {
                    negative = true;
                }
                let TokenKind::Integer(i) = &self.current().kind else {
                    return self.syntax_error_current("expected integer after CONNECTION LIMIT");
                };
                let v = *i;
                let v = if negative { -v } else { v };
                options.push(RoleOption::ConnectionLimit(v));
                self.advance();
            } else if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("valid"))
            {
                self.advance(); // VALID
                consumed_anything = true;
                if !matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("until"))
                {
                    return self.syntax_error_current("expected UNTIL after VALID");
                }
                self.advance(); // UNTIL
                let TokenKind::String(s) = &self.current().kind else {
                    return self.syntax_error_current("expected string literal after VALID UNTIL");
                };
                let s = s.clone();
                options.push(RoleOption::ValidUntil(s));
                self.advance();
            } else if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("encrypted") || s.eq_ignore_ascii_case("unencrypted"))
            {
                self.advance(); // skip ENCRYPTED/UNENCRYPTED
                consumed_anything = true;
                self.expect_keyword(Keyword::Password)?;
                if self.consume_keyword(Keyword::Null).is_some() {
                    options.push(RoleOption::PasswordNull);
                } else {
                    let password = match &self.current().kind {
                        TokenKind::String(s) => s.clone(),
                        _ => {
                            return self
                                .syntax_error_current("expected string literal after PASSWORD");
                        }
                    };
                    self.advance();
                    options.push(RoleOption::Password(password));
                }
            } else {
                break;
            }
        }

        if !consumed_anything {
            let span = if matches!(self.current().kind, TokenKind::Semicolon | TokenKind::Eof) {
                name_span
            } else {
                self.current().span
            };
            let message = if matches!(self.current().kind, TokenKind::Semicolon | TokenKind::Eof) {
                "ALTER ROLE requires at least one supported option"
            } else {
                "ALTER ROLE clause is not supported"
            };
            return self.unsupported_feature(span, message.to_string());
        }

        Ok(Statement::AlterRole(AlterRoleStatement {
            name,
            options,
            span: alter.span.merge(self.previous_span().merge(name_span)),
        }))
    }

    // CREATE SCHEMA [IF NOT EXISTS] <name> | CREATE SCHEMA AUTHORIZATION <role>
    pub(crate) fn parse_create_schema_statement(
        &mut self,
        create: crate::tokens::Token,
    ) -> DbResult<Statement> {
        self.expect_keyword(Keyword::Schema)?;
        let if_not_exists = self.consume_if_not_exists_clause()?;
        let mut body = Vec::new();
        if self.consume_keyword(Keyword::Authorization).is_some() {
            let (name, name_span) = self.expect_identifier()?;
            if !self.is_eof() && self.current().kind != TokenKind::Semicolon {
                body = self.parse_create_schema_body(&name, create.span)?;
            }
            return Ok(Statement::CreateSchema(CreateSchemaStatement {
                name,
                if_not_exists,
                body,
                span: create.span.merge(name_span),
            }));
        }
        let (name, name_span) = self.expect_identifier()?;
        let span = if self.consume_keyword(Keyword::Authorization).is_some() {
            let (_, auth_span) = self.expect_identifier()?;
            create.span.merge(auth_span)
        } else {
            create.span.merge(name_span)
        };
        if !self.is_eof() && self.current().kind != TokenKind::Semicolon {
            body = self.parse_create_schema_body(&name, create.span)?;
        }
        Ok(Statement::CreateSchema(CreateSchemaStatement {
            name,
            if_not_exists,
            body,
            span,
        }))
    }

    fn parse_create_schema_body(
        &mut self,
        schema_name: &str,
        create_span: crate::Span,
    ) -> DbResult<Vec<Statement>> {
        let mut body = Vec::new();
        while !self.is_eof() && self.current().kind != TokenKind::Semicolon {
            let start_index = self.index;
            let statement = self.parse_statement()?;
            if self.index == start_index {
                return self.syntax_error_current("failed to parse CREATE SCHEMA inline statement");
            }
            body.push(statement);
        }
        for statement in &body {
            self.validate_create_schema_body_statement(schema_name, statement, create_span)?;
        }
        Ok(body)
    }

    fn validate_create_schema_body_statement(
        &self,
        schema_name: &str,
        statement: &Statement,
        create_span: crate::Span,
    ) -> DbResult<()> {
        fn schema_component(name: &crate::ast::ObjectName) -> Option<&str> {
            (name.parts.len() > 1).then_some(name.parts[0].as_str())
        }

        let offending_schema = match statement {
            Statement::CreateTable(stmt) => schema_component(&stmt.name),
            Statement::CreateTableAs(stmt) => schema_component(&stmt.name),
            Statement::CreateSequence(stmt) => schema_component(&stmt.name),
            Statement::CreateView(stmt) => schema_component(&stmt.name),
            Statement::CreateIndex(stmt) => {
                schema_component(&stmt.table).or_else(|| schema_component(&stmt.name))
            }
            Statement::CreateTrigger(stmt) => schema_component(&stmt.table),
            _ => None,
        };

        if let Some(offending_schema) = offending_schema {
            if !offending_schema.eq_ignore_ascii_case(schema_name) {
                return Err(DbError::bind_error(
                    SqlState::InvalidSchemaName,
                    format!(
                        "CREATE specifies a schema ({offending_schema}) different from the one being created ({schema_name})"
                    ),
                )
                .with_position(create_span.start));
            }
        }

        Ok(())
    }

    // DROP SCHEMA [IF EXISTS] <name> [CASCADE]
    pub(crate) fn parse_drop_schema_statement(
        &mut self,
        drop: crate::tokens::Token,
    ) -> DbResult<Statement> {
        self.expect_keyword(Keyword::Schema)?;
        let if_exists = self.consume_if_exists_clause()?;
        let (name, name_span) = self.expect_identifier()?;
        let cascade = self.consume_keyword(Keyword::Cascade).is_some();
        let span_end = if cascade {
            self.previous_span()
        } else {
            name_span
        };
        Ok(Statement::DropSchema(DropSchemaStatement {
            name,
            if_exists,
            cascade,
            span: drop.span.merge(span_end),
        }))
    }
}
