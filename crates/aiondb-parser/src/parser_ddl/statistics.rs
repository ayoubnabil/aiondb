// CREATE / DROP / ALTER STATISTICS parsing.
//
// These parsers produce real AST nodes; the engine must provide real

use super::*;
use crate::ast::{
    AlterStatisticsAction, AlterStatisticsStatement, CommentOnStatisticsStatement,
    CreateStatisticsStatement, DropStatisticsStatement, StatisticsKeyItem, StatisticsKind,
};
use crate::tokens::Token;

impl Parser {
    // CREATE STATISTICS [IF NOT EXISTS] [name] [ ( kind [, ...] ) ] ON expr [, ...] FROM table_name
    pub(crate) fn parse_create_statistics_statement(
        &mut self,
        create: Token,
        statistics_tok: Token,
    ) -> DbResult<Statement> {
        // IF NOT EXISTS
        let if_not_exists = self.consume_if_not_exists_clause()?;

        // Optional name: either an identifier (possibly schema-qualified) or
        // straight to kinds `(...)` / ON for a nameless definition.
        let (name, name_span_opt) = if matches!(
            self.current().kind,
            TokenKind::Identifier(_) | TokenKind::Keyword(_)
        ) && !self.current_is_identifier_ci("on")
            && !matches!(self.current().kind, TokenKind::Keyword(Keyword::On))
        {
            let parsed = self.parse_object_name()?;
            let sp = parsed.span;
            (Some(parsed), Some(sp))
        } else {
            (None, None)
        };
        let mut span = name_span_opt.map_or(create.span, |sp| create.span.merge(sp));

        // Optional (kind [, ...])
        let kinds = if matches!(self.current().kind, TokenKind::LParen) {
            self.advance(); // (
            let mut kinds = Vec::new();
            loop {
                let (ident, ident_span) = self.expect_identifier()?;
                let kind = match ident.to_ascii_lowercase().as_str() {
                    "ndistinct" => StatisticsKind::Ndistinct,
                    "dependencies" => StatisticsKind::Dependencies,
                    "mcv" => StatisticsKind::Mcv,
                    other => {
                        // PG surfaces this as a plain ERROR without a LINE
                        // position pointer - use a bind-level error so our
                        // formatter does not append one.
                        let _ = ident_span;
                        return Err(aiondb_core::DbError::bind_error(
                            aiondb_core::SqlState::FeatureNotSupported,
                            format!("unrecognized statistics kind \"{other}\""),
                        ));
                    }
                };
                kinds.push(kind);
                if !self.consume_kind(&TokenKind::Comma) {
                    break;
                }
            }
            let rparen = self.expect_token(&TokenKind::RParen)?;
            span = span.merge(rparen.span);
            kinds
        } else {
            Vec::new()
        };

        // ON key [, ...]
        if !matches!(self.current().kind, TokenKind::Keyword(Keyword::On)) {
            return self.syntax_error_at_current_token();
        }
        self.advance(); // ON

        let mut keys: Vec<StatisticsKeyItem> = Vec::new();
        loop {
            let key = self.parse_statistics_key_item()?;
            span = span.merge(match &key {
                StatisticsKeyItem::Column { span, .. } => *span,
                StatisticsKeyItem::Expression { span, .. } => *span,
            });
            keys.push(key);
            if !self.consume_kind(&TokenKind::Comma) {
                break;
            }
        }

        // FROM table_name
        if !matches!(self.current().kind, TokenKind::Keyword(Keyword::From)) {
            return self.syntax_error_at_current_token();
        }
        self.advance(); // FROM
        let table = self.parse_object_name()?;
        span = span.merge(table.span);

        let _ = statistics_tok;
        // Synthesize a placeholder name when the user omitted one; the engine
        // will replace it with a deterministic auto-generated identifier.
        let final_name = match name {
            Some(n) => n,
            None => crate::ast::ObjectName {
                parts: vec!["__aiondb_stat_anon__".to_owned()],
                span: create.span,
            },
        };
        Ok(Statement::CreateStatistics(CreateStatisticsStatement {
            name: final_name,
            table,
            keys,
            kinds,
            if_not_exists,
            span,
        }))
    }

    fn parse_statistics_key_item(&mut self) -> DbResult<StatisticsKeyItem> {
        if matches!(self.current().kind, TokenKind::LParen) {
            // Parenthesized expression form: (expr).
            // Postgres rejects `(col, col)` (tuple) with "syntax error at or near ','".
            // A parenthesized single-column reference `(col)` is a legal expression
            // which the engine then collapses to a column and rejects with
            // "extended statistics require at least 2 columns" when it's the sole key.
            let lparen = self.advance();
            let expr_start_idx = self.index;
            let expr = self.parse_expr()?;
            // Disallow tuple form: `(a, b)`.
            if matches!(self.current().kind, TokenKind::Comma) {
                return self.syntax_error_at_current_token();
            }
            let rparen = self.expect_token(&TokenKind::RParen)?;
            // If the parenthesized form is just a bare column reference
            // (a single Identifier), demote it to a column item so the
            // engine applies the ≥2-column rule (PG does this via its
            // expression simplifier).
            if let crate::ast::Expr::Identifier(ref name) = expr {
                if name.parts.len() == 1 {
                    let column_name = name.parts[0].clone();
                    return Ok(StatisticsKeyItem::Column {
                        name: column_name,
                        span: lparen.span.merge(rparen.span),
                    });
                }
            }
            // Capture original SQL text for this expression.
            let start_byte = self.tokens[expr_start_idx].span.start;
            let end_byte = self.tokens[self.index - 2].span.end; // token before rparen
            let sql = self
                .source
                .get(start_byte..end_byte)
                .map_or_else(String::new, |s| s.trim().to_owned());
            Ok(StatisticsKeyItem::Expression {
                sql,
                expr,
                span: lparen.span.merge(rparen.span),
            })
        } else if matches!(
            self.current().kind,
            TokenKind::Identifier(_) | TokenKind::Keyword(_)
        ) {
            // Either a bare column reference or a function-call expression
            // such as `date_trunc('day', d)` (PG accepts function-call form
            // without requiring explicit parens around the whole expression).
            let ident_idx = self.index;
            let ident_span_start = self.current().span;
            let (name, name_span) = self.expect_identifier()?;
            if matches!(self.current().kind, TokenKind::LParen) {
                // Function call form - rewind and re-parse as full expression.
                self.index = ident_idx;
                let expr_start_idx = self.index;
                let expr = self.parse_expr()?;
                let start_byte = self.tokens[expr_start_idx].span.start;
                let end_byte = self.tokens[self.index - 1].span.end;
                let sql = self
                    .source
                    .get(start_byte..end_byte)
                    .map_or_else(String::new, |s| s.trim().to_owned());
                let span = ident_span_start.merge(self.tokens[self.index - 1].span);
                return Ok(StatisticsKeyItem::Expression { sql, expr, span });
            }
            match self.current().kind {
                TokenKind::Comma | TokenKind::Keyword(Keyword::From) | TokenKind::Semicolon => {}
                _ => {
                    return self.syntax_error_at_current_token();
                }
            }
            Ok(StatisticsKeyItem::Column {
                name,
                span: name_span,
            })
        } else {
            self.syntax_error_at_current_token()
        }
    }

    // DROP STATISTICS [IF EXISTS] name [, ...] [CASCADE | RESTRICT]
    pub(crate) fn parse_drop_statistics_statement(
        &mut self,
        drop: Token,
        statistics_tok: Token,
        if_exists_preconsumed: bool,
    ) -> DbResult<Statement> {
        let if_exists = self.consume_if_exists_clause_with_preconsumed(if_exists_preconsumed)?;
        let mut names = Vec::new();
        let first = self.parse_object_name()?;
        let mut span = drop.span.merge(first.span);
        names.push(first);
        while self.consume_kind(&TokenKind::Comma) {
            let next = self.parse_object_name()?;
            span = span.merge(next.span);
            names.push(next);
        }
        let mut cascade = false;
        if let Some(tok) = self.consume_drop_behavior_keyword() {
            span = span.merge(tok.span);
            cascade = matches!(tok.kind, TokenKind::Keyword(Keyword::Cascade));
        }
        let _ = statistics_tok;
        Ok(Statement::DropStatistics(DropStatisticsStatement {
            names,
            if_exists,
            cascade,
            span,
        }))
    }

    // ALTER STATISTICS [IF EXISTS] name
    //     { SET STATISTICS <integer> | RENAME TO new_name
    //     | SET SCHEMA new_schema | OWNER TO new_owner }
    pub(crate) fn parse_alter_statistics_statement(
        &mut self,
        alter: Token,
        statistics_tok: Token,
    ) -> DbResult<Statement> {
        let if_exists = self.consume_if_exists_clause()?;
        let name = self.parse_object_name()?;
        let mut span = alter.span.merge(name.span);
        let action = if self.consume_keyword(Keyword::Owner).is_some() {
            self.expect_keyword(Keyword::To)?;
            let (owner, sp) = self.expect_identifier()?;
            span = span.merge(sp);
            AlterStatisticsAction::OwnerTo(owner)
        } else if self.consume_keyword(Keyword::Rename).is_some() {
            self.expect_keyword(Keyword::To)?;
            let (new_name, sp) = self.expect_identifier()?;
            span = span.merge(sp);
            AlterStatisticsAction::Rename(new_name)
        } else if self.consume_keyword(Keyword::Set).is_some() {
            // SET SCHEMA or SET STATISTICS <int>
            if self.consume_keyword(Keyword::Schema).is_some() {
                let (schema, sp) = self.expect_identifier()?;
                span = span.merge(sp);
                AlterStatisticsAction::SetSchema(schema)
            } else if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("statistics"))
            {
                self.advance(); // STATISTICS
                let sign = if self.consume_kind(&TokenKind::Minus) {
                    -1i32
                } else {
                    let _ = self.consume_kind(&TokenKind::Plus);
                    1i32
                };
                let tok = self.advance();
                let value: i32 = match tok.kind {
                    TokenKind::Integer(v) => i32::try_from(v).map_err(|_| {
                        DbError::syntax_error("invalid SET STATISTICS target")
                            .with_position(tok.span.start + 1)
                    })?,
                    _ => {
                        return self
                            .syntax_error(tok.span, "expected integer SET STATISTICS target");
                    }
                };
                span = span.merge(tok.span);
                AlterStatisticsAction::SetStatisticsTarget(sign * value)
            } else {
                return self.syntax_error_at_current_token();
            }
        } else {
            return self.syntax_error_at_current_token();
        };
        let _ = statistics_tok;
        Ok(Statement::AlterStatistics(AlterStatisticsStatement {
            name,
            action,
            if_exists,
            span,
        }))
    }

    // COMMENT ON STATISTICS name IS { 'text' | NULL }
    pub(crate) fn parse_comment_on_statistics_statement(
        &mut self,
        start: Token,
    ) -> DbResult<Statement> {
        let name = self.parse_object_name()?;
        self.expect_keyword(Keyword::Is)?;
        let comment = match &self.current().kind {
            TokenKind::String(s) => {
                let owned = s.clone();
                self.advance();
                Some(owned)
            }
            TokenKind::Keyword(Keyword::Null) => {
                self.advance();
                None
            }
            _ => return self.syntax_error_at_current_token(),
        };
        let span = start.span.merge(self.previous_span());
        Ok(Statement::CommentOnStatistics(
            CommentOnStatisticsStatement {
                name,
                comment,
                span,
            },
        ))
    }
}
