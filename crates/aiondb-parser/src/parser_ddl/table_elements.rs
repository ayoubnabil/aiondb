#![allow(clippy::collapsible_if, clippy::doc_markdown)]

use super::*;
use crate::Span;
use aiondb_core::{IdentityGeneration, IdentityOptions, IdentitySpec, TextTypeModifier};

/// Extract the raw type text for a column type as written in SQL,
/// lowercased and schema-qualified when present. This preserves
/// built-in spellings like `varchar` / `character varying` in addition
/// to user-defined identifiers so downstream catalog reflection can
/// distinguish `text` from unbounded varchar columns.
pub(super) fn extract_raw_type_name(tokens: &[crate::tokens::Token]) -> Option<String> {
    if let Some(hint) = super::super::parser_types::infer_numeric_type_name_hint(tokens) {
        return Some(hint);
    }
    let mut idx = 0usize;
    let mut array_depth = 0usize;
    let first = tokens.first().map(|t| &t.kind)?;
    let mut name = match first {
        TokenKind::Identifier(s) => s.to_ascii_lowercase(),
        TokenKind::Keyword(keyword) => keyword.name().to_ascii_lowercase(),
        _ => return None,
    };
    idx += 1;
    if matches!(tokens.get(idx).map(|t| &t.kind), Some(TokenKind::Dot)) {
        idx += 1;
        match tokens.get(idx).map(|t| &t.kind) {
            Some(TokenKind::Identifier(s)) => {
                name.push('.');
                name.push_str(&s.to_ascii_lowercase());
                idx += 1;
            }
            Some(TokenKind::Keyword(keyword)) => {
                name.push('.');
                name.push_str(&keyword.name().to_ascii_lowercase());
                idx += 1;
            }
            _ => return None,
        }
    }
    while let Some(TokenKind::Keyword(keyword)) = tokens.get(idx).map(|t| &t.kind) {
        match keyword {
            Keyword::Varying
            | Keyword::Precision
            | Keyword::Without
            | Keyword::With
            | Keyword::Time
            | Keyword::Zone => {
                name.push(' ');
                name.push_str(&keyword.name().to_ascii_lowercase());
                idx += 1;
            }
            _ => break,
        }
    }
    while idx < tokens.len() {
        match tokens.get(idx).map(|t| &t.kind) {
            Some(TokenKind::LParen) => {
                let mut depth = 1;
                idx += 1;
                while idx < tokens.len() && depth > 0 {
                    match tokens.get(idx).map(|t| &t.kind) {
                        Some(TokenKind::LParen) => depth += 1,
                        Some(TokenKind::RParen) => depth -= 1,
                        _ => {}
                    }
                    idx += 1;
                }
            }
            Some(TokenKind::LBracket) => {
                if !matches!(
                    tokens.get(idx.wrapping_sub(1)).map(|t| &t.kind),
                    Some(TokenKind::Keyword(Keyword::Array))
                ) {
                    array_depth += 1;
                }
                let mut depth = 1;
                idx += 1;
                while idx < tokens.len() && depth > 0 {
                    match tokens.get(idx).map(|t| &t.kind) {
                        Some(TokenKind::LBracket) => depth += 1,
                        Some(TokenKind::RBracket) => depth -= 1,
                        _ => {}
                    }
                    idx += 1;
                }
            }
            Some(TokenKind::Keyword(Keyword::Array)) => {
                array_depth += 1;
                idx += 1;
            }
            _ => return None,
        }
    }
    for _ in 0..array_depth {
        name.push_str("[]");
    }
    Some(name)
}

pub(super) fn infer_text_type_modifier(
    tokens: &[crate::tokens::Token],
) -> Option<TextTypeModifier> {
    fn parse_length(tokens: &[crate::tokens::Token], index: usize) -> Option<u32> {
        if !matches!(
            tokens.get(index).map(|token| &token.kind),
            Some(TokenKind::LParen)
        ) {
            return None;
        }
        match tokens.get(index + 1).map(|token| &token.kind) {
            Some(TokenKind::Integer(length)) if *length > 0 => {
                Some(u32::try_from(*length).unwrap_or(u32::MAX))
            }
            Some(TokenKind::Integer(_)) => Some(1),
            _ => Some(1),
        }
    }

    match tokens.first().map(|token| &token.kind) {
        Some(TokenKind::Keyword(Keyword::Char)) => Some(TextTypeModifier::Char {
            length: parse_length(tokens, 1).unwrap_or(1),
        }),
        Some(TokenKind::Keyword(Keyword::Varchar)) => {
            parse_length(tokens, 1).map(|length| TextTypeModifier::VarChar { length })
        }
        Some(TokenKind::Keyword(Keyword::Character)) => {
            if matches!(
                tokens.get(1).map(|token| &token.kind),
                Some(TokenKind::Keyword(Keyword::Varying))
            ) {
                parse_length(tokens, 2).map(|length| TextTypeModifier::VarChar { length })
            } else {
                Some(TextTypeModifier::Char {
                    length: parse_length(tokens, 1).unwrap_or(1),
                })
            }
        }
        _ => None,
    }
}

impl Parser {
    /// Parse a `RESTRICT | NO ACTION | CASCADE | SET NULL [(cols)] | SET DEFAULT [(cols)]`
    /// referential action clause.
    ///
    /// Caller has already consumed `ON DELETE` / `ON UPDATE`.
    pub(super) fn parse_inline_fk_action(
        &mut self,
    ) -> DbResult<(aiondb_core::FkAction, Vec<String>)> {
        if self.consume_keyword(Keyword::Restrict).is_some() {
            return Ok((aiondb_core::FkAction::Restrict, Vec::new()));
        }
        if self.consume_keyword(Keyword::Cascade).is_some() {
            return Ok((aiondb_core::FkAction::Cascade, Vec::new()));
        }
        if self.consume_keyword(Keyword::No).is_some() {
            self.expect_keyword(Keyword::Action)?;
            return Ok((aiondb_core::FkAction::NoAction, Vec::new()));
        }
        if self.consume_keyword(Keyword::Set).is_some() {
            let action = if self.consume_keyword(Keyword::Null).is_some() {
                aiondb_core::FkAction::SetNull
            } else if self.consume_keyword(Keyword::Default).is_some() {
                aiondb_core::FkAction::SetDefault
            } else {
                return self.syntax_error_current("expected NULL or DEFAULT after SET");
            };
            let action_columns = if self.current().kind == TokenKind::LParen {
                self.parse_column_name_list()?
            } else {
                Vec::new()
            };
            return Ok((action, action_columns));
        }
        self.syntax_error_current("expected RESTRICT, NO ACTION, CASCADE, SET NULL, or SET DEFAULT")
    }

    pub(super) fn parse_column_def(&mut self) -> DbResult<ColumnDef> {
        if matches!(self.current().kind, TokenKind::Keyword(Keyword::With)) {
            return self.syntax_error_current("syntax error at or near \"with\"");
        }
        let (column_name, column_span) = self.expect_identifier()?;
        let type_start = self.index;
        let data_type = self.parse_data_type()?;
        let text_type_modifier = infer_text_type_modifier(&self.tokens[type_start..self.index]);
        let raw_type_name = extract_raw_type_name(&self.tokens[type_start..self.index]);

        // Detect SERIAL/BIGSERIAL/SMALLSERIAL type keywords.  These are
        // syntactic sugar for an integer column with an auto-incrementing
        // sequence default.  Mark the column with an implicit identity so
        // the binder synthesizes the nextval() default.
        let is_serial = self.tokens[type_start..self.index].iter().any(|token| {
            matches!(
                token.kind,
                TokenKind::Keyword(Keyword::Serial | Keyword::BigSerial)
            ) || matches!(
                &token.kind,
                TokenKind::Identifier(s)
                    if s.eq_ignore_ascii_case("smallserial")
                        || s.eq_ignore_ascii_case("serial2")
                        || s.eq_ignore_ascii_case("serial4")
                        || s.eq_ignore_ascii_case("serial8")
            )
        });

        let mut nullable = true;
        let mut default = None;
        let mut nullability_seen = false;
        let mut primary_key = false;
        let mut unique = false;
        let mut identity = if is_serial {
            Some(IdentitySpec {
                generation: IdentityGeneration::ByDefault,
                options: IdentityOptions::default(),
            })
        } else {
            None
        };
        let mut inline_references: Vec<crate::ast::InlineColumnReference> = Vec::new();
        let mut inline_checks: Vec<crate::ast::Expr> = Vec::new();
        let mut span_end = self.tokens[self.index - 1].span;

        loop {
            if self.current().kind == TokenKind::Keyword(Keyword::Default) {
                if default.is_some() {
                    return self.syntax_error_current("DEFAULT specified more than once");
                }
                self.advance();
                let expr = self.parse_expr()?;
                span_end = expr.span();
                default = Some(expr);
                continue;
            }

            if matches!(self.current().kind, TokenKind::Keyword(Keyword::Not))
                && matches!(
                    self.tokens.get(self.index + 1).map(|token| &token.kind),
                    Some(TokenKind::Identifier(s)) if s.eq_ignore_ascii_case("nul")
                )
            {
                let nul_span = self.tokens[self.index + 1].span;
                return self.syntax_error(nul_span, "syntax error at or near \"NUL\"");
            }

            if matches!(self.current().kind, TokenKind::Keyword(Keyword::Not))
                && matches!(
                    self.tokens.get(self.index + 1).map(|token| &token.kind),
                    Some(TokenKind::Keyword(Keyword::Deferrable))
                )
            {
                let not = self.advance();
                let deferrable = self.expect_keyword(Keyword::Deferrable)?;
                span_end = not.span.merge(deferrable.span);
                if self.consume_keyword(Keyword::Initially).is_some() {
                    let _ = self.consume_keyword(Keyword::Deferred);
                    let _ = self.consume_keyword(Keyword::Immediate);
                    span_end = self.previous_span();
                }
                continue;
            }

            if matches!(self.current().kind, TokenKind::Keyword(Keyword::Not))
                && matches!(
                    self.tokens.get(self.index + 1).map(|token| &token.kind),
                    Some(TokenKind::Keyword(Keyword::Null))
                )
            {
                let not = self.advance();
                if nullability_seen {
                    return self.syntax_error(not.span, "NULL/NOT NULL specified more than once");
                }
                let null = self.expect_keyword(Keyword::Null)?;
                nullable = false;
                nullability_seen = true;
                span_end = not.span.merge(null.span);
                continue;
            }

            if let Some(null) = self.consume_keyword(Keyword::Null) {
                if nullability_seen {
                    return self.syntax_error(null.span, "NULL/NOT NULL specified more than once");
                }
                nullable = true;
                nullability_seen = true;
                span_end = null.span;
                continue;
            }

            if let Some(pk) = self.consume_keyword(Keyword::Primary) {
                let key = self.expect_keyword(Keyword::Key)?;
                primary_key = true;
                nullable = false;
                span_end = pk.span.merge(key.span);
                continue;
            }

            if let Some(u) = self.consume_keyword(Keyword::Unique) {
                unique = true;
                span_end = u.span;
                // Skip optional NULLS [NOT] DISTINCT after UNIQUE
                if matches!(self.current().kind, TokenKind::Keyword(Keyword::Nulls)) {
                    self.advance(); // NULLS
                    let _ = self.consume_keyword(Keyword::Not); // optional NOT
                    if matches!(self.current().kind, TokenKind::Keyword(Keyword::Distinct)) {
                        self.advance(); // DISTINCT
                    }
                    span_end = self.previous_span();
                }
                continue;
            }

            if self.consume_keyword(Keyword::Check).is_some() {
                // Inline CHECK: capture and lift to table-level CHECK during binding.
                self.expect_token(&TokenKind::LParen)?;
                let expr = self.parse_expr()?;
                let rparen = self.expect_token(&TokenKind::RParen)?;
                span_end = rparen.span;
                inline_checks.push(expr);
                // Skip optional NO INHERIT
                if self.consume_keyword(Keyword::No).is_some() {
                    let _ = self.consume_keyword(Keyword::Inherit);
                    span_end = self.previous_span();
                }
                continue;
            }

            // Inline REFERENCES: REFERENCES reftable [(col, ...)] [MATCH ...] [ON DELETE ...] [ON UPDATE ...]
            if let Some(refs_kw) = self.consume_keyword(Keyword::References) {
                let ref_table = self.parse_object_name()?;
                let mut ref_columns: Vec<String> = Vec::new();
                span_end = self.previous_span();
                // Optional column list - capture identifiers, allow dotted/quoted forms.
                if self.consume_kind(&TokenKind::LParen) {
                    loop {
                        let (col_name, _name_span) = self.expect_identifier()?;
                        ref_columns.push(col_name);
                        if self.consume_kind(&TokenKind::Comma) {
                            continue;
                        }
                        let rparen = self.expect_token(&TokenKind::RParen)?;
                        span_end = rparen.span;
                        break;
                    }
                }
                let mut inline_on_delete = aiondb_core::FkAction::NoAction;
                let mut inline_on_update = aiondb_core::FkAction::NoAction;
                let mut inline_on_delete_set_columns = Vec::new();
                let mut inline_on_update_set_columns = Vec::new();
                let mut inline_match = aiondb_core::FkMatchType::Simple;
                // Optional MATCH FULL/PARTIAL/SIMPLE.
                if self.consume_keyword(Keyword::Match).is_some()
                    || matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("match"))
                        && {
                            self.advance();
                            true
                        }
                {
                    span_end = self.previous_span();
                    if self.consume_keyword(Keyword::Full).is_some() {
                        inline_match = aiondb_core::FkMatchType::Full;
                        span_end = self.previous_span();
                    } else if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("partial"))
                    {
                        inline_match = aiondb_core::FkMatchType::Partial;
                        span_end = self.advance().span;
                    } else if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("simple"))
                    {
                        span_end = self.advance().span;
                    }
                }
                // ON DELETE / ON UPDATE clauses, in any order.
                while self.consume_keyword(Keyword::On).is_some() {
                    let target_is_delete = self.consume_keyword(Keyword::Delete).is_some();
                    let target_is_update =
                        !target_is_delete && self.consume_keyword(Keyword::Update).is_some();
                    if !target_is_delete && !target_is_update {
                        return self.syntax_error_current("expected DELETE or UPDATE after ON");
                    }
                    let (action, action_columns) = self.parse_inline_fk_action()?;
                    if target_is_delete {
                        inline_on_delete = action;
                        inline_on_delete_set_columns = action_columns;
                    } else {
                        inline_on_update = action;
                        inline_on_update_set_columns = action_columns;
                    }
                    span_end = self.previous_span();
                }
                // (NOT) DEFERRABLE / INITIALLY DEFERRED|IMMEDIATE - accepted
                // syntactically.
                if self.consume_keyword(Keyword::Not).is_some() {
                    if self.consume_keyword(Keyword::Deferrable).is_some() {
                        span_end = self.previous_span();
                    }
                } else if self.consume_keyword(Keyword::Deferrable).is_some() {
                    span_end = self.previous_span();
                }
                if self.consume_keyword(Keyword::Initially).is_some() {
                    let _ = self.consume_keyword(Keyword::Deferred);
                    let _ = self.consume_keyword(Keyword::Immediate);
                    span_end = self.previous_span();
                }
                inline_references.push(crate::ast::InlineColumnReference {
                    ref_table,
                    ref_columns,
                    on_delete: inline_on_delete,
                    on_update: inline_on_update,
                    on_delete_set_columns: inline_on_delete_set_columns,
                    on_update_set_columns: inline_on_update_set_columns,
                    match_type: inline_match,
                    span: refs_kw.span.merge(span_end),
                });
                continue;
            }

            // GENERATED ALWAYS AS (...) STORED | GENERATED ALWAYS/BY DEFAULT AS IDENTITY
            if self.consume_keyword(Keyword::Generated).is_some() {
                let generation = if self.consume_keyword(Keyword::Always).is_some() {
                    span_end = self.previous_span();
                    IdentityGeneration::Always
                } else {
                    let by = self.expect_keyword(Keyword::By)?;
                    let default_kw = self.expect_keyword(Keyword::Default)?;
                    span_end = by.span.merge(default_kw.span);
                    IdentityGeneration::ByDefault
                };
                if self.consume_keyword(Keyword::As).is_some() {
                    span_end = self.previous_span();
                    if self.consume_keyword(Keyword::Identity).is_some() {
                        span_end = self.previous_span();
                        let options = if self.consume_kind(&TokenKind::LParen) {
                            let (options, end_span) = self.parse_identity_options()?;
                            span_end = end_span;
                            options
                        } else {
                            IdentityOptions::default()
                        };
                        identity = Some(IdentitySpec {
                            generation,
                            options,
                        });
                        nullable = false;
                    } else if self.consume_kind(&TokenKind::LParen) {
                        // GENERATED ALWAYS AS (expr) STORED
                        let _expr = self.parse_expr()?;
                        let rparen = self.expect_token(&TokenKind::RParen)?;
                        span_end = rparen.span;
                        let _ = self.consume_keyword(Keyword::Stored);
                        if self.tokens[self.index - 1].kind == TokenKind::Keyword(Keyword::Stored) {
                            span_end = self.previous_span();
                        }
                    }
                }
                continue;
            }

            // COLLATE "xxx" - skip
            if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("collate"))
            {
                self.advance(); // COLLATE
                                // Skip collation name (can be dotted)
                let _ = self.parse_object_name()?;
                span_end = self.previous_span();
                continue;
            }

            // COMPRESSION method - skip (PG 14+ column compression option)
            if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("compression"))
            {
                self.advance(); // COMPRESSION
                let _ = self.advance(); // method name (pglz, lz4, etc.)
                span_end = self.previous_span();
                continue;
            }

            // STORAGE option - skip (PLAIN, EXTERNAL, EXTENDED, MAIN)
            if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("storage"))
            {
                self.advance(); // STORAGE
                let _ = self.advance(); // storage option
                span_end = self.previous_span();
                continue;
            }

            // CONSTRAINT name - inline constraint name, skip name
            if self.consume_keyword(Keyword::Constraint).is_some() {
                let _ = self.expect_identifier()?;
                span_end = self.previous_span();
                continue;
            }

            // DEFERRABLE / NOT DEFERRABLE / INITIALLY DEFERRED/IMMEDIATE (standalone)
            if self.consume_keyword(Keyword::Deferrable).is_some() {
                span_end = self.previous_span();
                if self.consume_keyword(Keyword::Initially).is_some() {
                    let _ = self.consume_keyword(Keyword::Deferred);
                    let _ = self.consume_keyword(Keyword::Immediate);
                    span_end = self.previous_span();
                }
                continue;
            }

            // USING INDEX [TABLESPACE name] - skip inline index specification
            if self.consume_keyword(Keyword::Using).is_some() {
                span_end = self.previous_span();
                // Skip until column boundary: comma, RParen, semicolon, or known keyword
                while !self.is_eof()
                    && !matches!(
                        self.current().kind,
                        TokenKind::Comma | TokenKind::RParen | TokenKind::Semicolon
                    )
                {
                    span_end = self.advance().span;
                }
                continue;
            }

            break;
        }

        let span = column_span.merge(span_end);
        Ok(ColumnDef {
            name: column_name,
            data_type,
            text_type_modifier,
            nullable,
            default,
            primary_key,
            unique,
            identity,
            raw_type_name,
            inline_references,
            inline_checks,
            span,
        })
    }

    fn parse_identity_options(&mut self) -> DbResult<(IdentityOptions, Span)> {
        let mut options = IdentityOptions::default();
        let mut span_end = self.previous_span();

        while !self.is_eof() {
            if matches!(self.current().kind, TokenKind::RParen) {
                span_end = self.advance().span;
                break;
            }

            if self.consume_word("increment") {
                let _ = self.consume_word("by");
                let value = self.parse_signed_integer_option_value()?;
                span_end = self.previous_span();
                options.increment_by = Some(value);
                continue;
            }

            if self.consume_word("start") {
                let _ = self.consume_word("with");
                let value = self.parse_signed_integer_option_value()?;
                span_end = self.previous_span();
                options.start_value = Some(value);
                continue;
            }

            if self.consume_word("minvalue") {
                let value = self.parse_signed_integer_option_value()?;
                span_end = self.previous_span();
                options.min_value = Some(value);
                continue;
            }

            if self.consume_word("maxvalue") {
                let value = self.parse_signed_integer_option_value()?;
                span_end = self.previous_span();
                options.max_value = Some(value);
                continue;
            }

            if self.consume_word("no") {
                let no_span = self.previous_span();
                if self.consume_word("cycle") {
                    span_end = no_span.merge(self.previous_span());
                    options.cycle = Some(false);
                    continue;
                }
                if self.consume_word("minvalue") {
                    span_end = no_span.merge(self.previous_span());
                    options.min_value = None;
                    continue;
                }
                if self.consume_word("maxvalue") {
                    span_end = no_span.merge(self.previous_span());
                    options.max_value = None;
                    continue;
                }
                span_end = no_span;
                continue;
            }

            if self.consume_word("cycle") {
                span_end = self.previous_span();
                options.cycle = Some(true);
                continue;
            }

            if self.consume_word("cache") {
                let _ = self.parse_signed_integer_option_value()?;
                span_end = self.previous_span();
                continue;
            }

            match &self.current().kind {
                TokenKind::RParen => {
                    span_end = self.advance().span;
                    break;
                }
                _ => span_end = self.advance().span,
            }
        }

        Ok((options, span_end))
    }

    fn parse_signed_integer_option_value(&mut self) -> DbResult<i64> {
        let negative = self.consume_kind(&TokenKind::Minus);
        match self.current().kind {
            TokenKind::Integer(value) => {
                self.advance();
                Ok(if negative { -value } else { value })
            }
            _ => self.syntax_error_current("expected integer option value"),
        }
    }

    fn consume_word(&mut self, word: &str) -> bool {
        if self.current_word_is(word) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn current_word_is(&self, word: &str) -> bool {
        match &self.current().kind {
            TokenKind::Identifier(value) => value.eq_ignore_ascii_case(word),
            TokenKind::Keyword(keyword) => keyword.name().eq_ignore_ascii_case(word),
            _ => false,
        }
    }

    pub(super) fn is_table_constraint_start(&self) -> bool {
        matches!(
            self.current().kind,
            TokenKind::Keyword(
                Keyword::Primary
                    | Keyword::Unique
                    | Keyword::Check
                    | Keyword::Foreign
                    | Keyword::Constraint
                    | Keyword::Exclude,
            )
        )
    }

    pub(super) fn parse_table_constraint(&mut self) -> DbResult<TableConstraint> {
        let constraint_name = if self.consume_keyword(Keyword::Constraint).is_some() {
            let (name, _) = self.expect_identifier()?;
            Some(name)
        } else {
            None
        };

        if let Some(pk) = self.consume_keyword(Keyword::Primary) {
            self.expect_keyword(Keyword::Key)?;
            // PRIMARY KEY USING INDEX idx_name - uses an existing index
            if self.consume_keyword(Keyword::Using).is_some() {
                let mut end_span = self.previous_span();
                let mut using_index_name: Option<String> = None;
                // Consume INDEX keyword if present
                if self.consume_keyword(Keyword::Index).is_some() {
                    end_span = self.previous_span();
                }
                if self.can_consume_constraint_index_name() {
                    let (idx_name, name_span) = self.expect_identifier()?;
                    using_index_name = Some(idx_name);
                    end_span = name_span;
                }
                self.skip_optional_tablespace(&mut end_span)?;
                self.skip_constraint_suffix(&mut end_span);
                let columns = using_index_name
                    .map(|name| vec![format!("__using_index__:{name}")])
                    .unwrap_or_default();
                return Ok(TableConstraint::PrimaryKey {
                    name: constraint_name,
                    columns,
                    span: pk.span.merge(end_span),
                });
            }
            let columns = self.parse_column_name_list()?;
            let mut end_span = self.tokens[self.index - 1].span;
            self.skip_constraint_suffix(&mut end_span);
            return Ok(TableConstraint::PrimaryKey {
                name: constraint_name,
                columns,
                span: pk.span.merge(end_span),
            });
        }

        if let Some(u) = self.consume_keyword(Keyword::Unique) {
            // UNIQUE USING INDEX idx_name - uses an existing index
            if self.consume_keyword(Keyword::Using).is_some() {
                let mut end_span = self.previous_span();
                let mut using_index_name: Option<String> = None;
                if self.consume_keyword(Keyword::Index).is_some() {
                    end_span = self.previous_span();
                }
                if self.can_consume_constraint_index_name() {
                    let (idx_name, name_span) = self.expect_identifier()?;
                    using_index_name = Some(idx_name);
                    end_span = name_span;
                }
                self.skip_optional_tablespace(&mut end_span)?;
                self.skip_constraint_suffix(&mut end_span);
                let columns = using_index_name
                    .map(|name| vec![format!("__using_index__:{name}")])
                    .unwrap_or_default();
                return Ok(TableConstraint::Unique {
                    name: constraint_name,
                    columns,
                    span: u.span.merge(end_span),
                });
            }
            // Skip optional NULLS [NOT] DISTINCT before column list
            if matches!(self.current().kind, TokenKind::Keyword(Keyword::Nulls)) {
                self.advance(); // NULLS
                let _ = self.consume_keyword(Keyword::Not); // optional NOT
                if matches!(self.current().kind, TokenKind::Keyword(Keyword::Distinct)) {
                    self.advance(); // DISTINCT
                }
            }
            let columns = self.parse_column_name_list()?;
            let mut end_span = self.tokens[self.index - 1].span;
            self.skip_constraint_suffix(&mut end_span);
            return Ok(TableConstraint::Unique {
                name: constraint_name,
                columns,
                span: u.span.merge(end_span),
            });
        }

        if let Some(c) = self.consume_keyword(Keyword::Check) {
            self.expect_token(&TokenKind::LParen)?;
            let expr = self.parse_expr()?;
            let rparen = self.expect_token(&TokenKind::RParen)?;
            let mut end_span = rparen.span;
            // Skip optional NO INHERIT
            if self.consume_keyword(Keyword::No).is_some() {
                let _ = self.consume_keyword(Keyword::Inherit);
                end_span = self.previous_span();
            }
            // Skip optional NOT VALID
            if self.consume_keyword(Keyword::Not).is_some() {
                if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("valid"))
                {
                    self.advance();
                }
                end_span = self.previous_span();
            }
            self.skip_constraint_suffix(&mut end_span);
            return Ok(TableConstraint::Check {
                name: constraint_name,
                expr,
                span: c.span.merge(end_span),
            });
        }

        if let Some(f) = self.consume_keyword(Keyword::Foreign) {
            self.expect_keyword(Keyword::Key)?;
            let columns = self.parse_column_name_list()?;
            self.expect_keyword(Keyword::References)?;
            let ref_table = self.parse_object_name()?;
            // Column list is optional in PG (defaults to referenced table's PK)
            let ref_columns = if self.current().kind == TokenKind::LParen {
                self.parse_column_name_list()?
            } else {
                Vec::new()
            };
            let mut end_span = self.tokens[self.index - 1].span;
            let mut match_type = aiondb_core::FkMatchType::Simple;
            // Optional MATCH FULL / MATCH PARTIAL / MATCH SIMPLE
            if self.consume_keyword(Keyword::Match).is_some()
                || matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("match"))
                    && {
                        self.advance();
                        true
                    }
            {
                end_span = self.previous_span();
                if self.consume_keyword(Keyword::Full).is_some() {
                    match_type = aiondb_core::FkMatchType::Full;
                    end_span = self.previous_span();
                } else if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("partial"))
                {
                    match_type = aiondb_core::FkMatchType::Partial;
                    end_span = self.advance().span;
                } else if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("simple"))
                {
                    end_span = self.advance().span;
                }
            }
            let mut on_delete = aiondb_core::FkAction::NoAction;
            let mut on_update = aiondb_core::FkAction::NoAction;
            let mut on_delete_set_columns = Vec::new();
            let mut on_update_set_columns = Vec::new();
            while self.consume_keyword(Keyword::On).is_some() {
                let target_is_delete = self.consume_keyword(Keyword::Delete).is_some();
                let target_is_update =
                    !target_is_delete && self.consume_keyword(Keyword::Update).is_some();
                if !target_is_delete && !target_is_update {
                    return self.syntax_error_current("expected DELETE or UPDATE after ON");
                }
                let (action, action_columns) = self.parse_inline_fk_action()?;
                if target_is_delete {
                    on_delete = action;
                    on_delete_set_columns = action_columns;
                } else {
                    on_update = action;
                    on_update_set_columns = action_columns;
                }
                end_span = self.previous_span();
            }
            // Skip DEFERRABLE / NOT DEFERRABLE / INITIALLY DEFERRED/IMMEDIATE
            if self.consume_keyword(Keyword::Not).is_some() {
                let _ = self.consume_keyword(Keyword::Deferrable);
                end_span = self.previous_span();
            } else if self.consume_keyword(Keyword::Deferrable).is_some() {
                end_span = self.previous_span();
            }
            if self.consume_keyword(Keyword::Initially).is_some() {
                let _ = self.consume_keyword(Keyword::Deferred);
                let _ = self.consume_keyword(Keyword::Immediate);
                end_span = self.previous_span();
            }
            return Ok(TableConstraint::ForeignKey {
                name: constraint_name,
                columns,
                ref_table,
                ref_columns,
                on_delete,
                on_update,
                on_delete_set_columns,
                on_update_set_columns,
                match_type,
                span: f.span.merge(end_span),
            });
        }

        // EXCLUDE USING method (col WITH operator, ...) [INCLUDE (...)]
        // Skip the entire EXCLUDE constraint, treat as CHECK true
        if let Some(ex) = self.consume_keyword(Keyword::Exclude) {
            let mut end_span = ex.span;
            // Skip USING method
            if self.consume_keyword(Keyword::Using).is_some() {
                if !self.is_eof() {
                    end_span = self.advance().span; // method name
                }
            }
            // Skip balanced parens (the element list)
            if self.consume_kind(&TokenKind::LParen) {
                let mut depth = 1u32;
                while !self.is_eof() && depth > 0 {
                    match self.current().kind {
                        TokenKind::LParen => depth += 1,
                        TokenKind::RParen => depth -= 1,
                        _ => {}
                    }
                    end_span = self.advance().span;
                }
            }
            // Skip optional INCLUDE (...)
            if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("include"))
            {
                end_span = self.advance().span;
                if self.consume_kind(&TokenKind::LParen) {
                    let mut depth = 1u32;
                    while !self.is_eof() && depth > 0 {
                        match self.current().kind {
                            TokenKind::LParen => depth += 1,
                            TokenKind::RParen => depth -= 1,
                            _ => {}
                        }
                        end_span = self.advance().span;
                    }
                }
            }
            // Skip optional WHERE clause
            if self.consume_keyword(Keyword::Where).is_some() {
                let _ = self.parse_expr()?;
                end_span = self.previous_span();
            }
            // Skip DEFERRABLE / INITIALLY DEFERRED/IMMEDIATE
            if self.consume_keyword(Keyword::Not).is_some() {
                let _ = self.consume_keyword(Keyword::Deferrable);
                end_span = self.previous_span();
            } else if self.consume_keyword(Keyword::Deferrable).is_some() {
                end_span = self.previous_span();
            }
            if self.consume_keyword(Keyword::Initially).is_some() {
                let _ = self.consume_keyword(Keyword::Deferred);
                let _ = self.consume_keyword(Keyword::Immediate);
                end_span = self.previous_span();
            }
            return Ok(TableConstraint::Check {
                name: constraint_name,
                expr: Expr::Literal(Literal::Boolean(true), ex.span),
                span: ex.span.merge(end_span),
            });
        }

        self.syntax_error_current("expected PRIMARY KEY, UNIQUE, CHECK, or FOREIGN KEY")
    }

    /// Skip optional constraint suffix tokens:
    /// INCLUDE (col_list), USING INDEX TABLESPACE name,
    /// DEFERRABLE / NOT DEFERRABLE, INITIALLY DEFERRED/IMMEDIATE.
    fn skip_constraint_suffix(&mut self, end_span: &mut crate::Span) {
        // INCLUDE (col1, col2, ...)
        if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("include"))
        {
            *end_span = self.advance().span;
            if self.consume_kind(&TokenKind::LParen) {
                let mut depth = 1u32;
                while !self.is_eof() && depth > 0 {
                    match self.current().kind {
                        TokenKind::LParen => depth += 1,
                        TokenKind::RParen => depth -= 1,
                        _ => {}
                    }
                    *end_span = self.advance().span;
                }
            }
        }
        // USING INDEX TABLESPACE name
        if self.consume_keyword(Keyword::Using).is_some() {
            *end_span = self.previous_span();
            // Skip everything until a boundary: comma, RParen, semicolon, or a known keyword
            while !self.is_eof()
                && !matches!(
                    self.current().kind,
                    TokenKind::Comma | TokenKind::RParen | TokenKind::Semicolon
                )
            {
                *end_span = self.advance().span;
            }
        }
        // DEFERRABLE / NOT DEFERRABLE
        if self.consume_keyword(Keyword::Not).is_some() {
            let _ = self.consume_keyword(Keyword::Deferrable);
            *end_span = self.previous_span();
        } else if self.consume_keyword(Keyword::Deferrable).is_some() {
            *end_span = self.previous_span();
        }
        // INITIALLY DEFERRED / INITIALLY IMMEDIATE
        if self.consume_keyword(Keyword::Initially).is_some() {
            let _ = self.consume_keyword(Keyword::Deferred);
            let _ = self.consume_keyword(Keyword::Immediate);
            *end_span = self.previous_span();
        }
    }

    fn parse_column_name_list(&mut self) -> DbResult<Vec<String>> {
        self.expect_token(&TokenKind::LParen)?;
        let mut names = Vec::new();
        loop {
            let (name, _) = self.expect_identifier()?;
            names.push(name);
            if !self.consume_kind(&TokenKind::Comma) {
                break;
            }
        }
        self.expect_token(&TokenKind::RParen)?;
        Ok(names)
    }

    fn can_consume_constraint_index_name(&self) -> bool {
        if matches!(
            self.current().kind,
            TokenKind::Comma | TokenKind::RParen | TokenKind::Semicolon
        ) {
            return false;
        }
        if matches!(
            self.current().kind,
            TokenKind::Keyword(Keyword::Deferrable | Keyword::Initially)
        ) {
            return false;
        }
        if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("tablespace"))
        {
            return false;
        }
        true
    }

    fn skip_optional_tablespace(&mut self, end_span: &mut Span) -> DbResult<()> {
        if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("tablespace"))
        {
            *end_span = self.advance().span;
            let (_, name_span) = self.expect_identifier()?;
            *end_span = name_span;
        }
        Ok(())
    }
}
