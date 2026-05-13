//! Recursive-descent parser for PL/pgSQL blocks.
//!
//! The parser consumes the token stream produced by [`crate::tokenizer`] and
//! emits a typed AST ([`crate::ast::Block`]). Expressions and embedded SQL
//! fragments are preserved as raw source slices - the engine's SQL planner
//! handles their actual evaluation, while the parser is responsible for
//! delimiting them correctly (balancing parens, respecting string literals,
//! stopping at the right punctuation).

use std::fmt;

use crate::ast::{
    Block, CaseArm, CaseStmt, CursorDecl, DiagnosticsKind, ExceptionCondition, ExceptionHandler,
    FetchDirection, ForLoopKind, GetDiagnosticsItem, IfBranch, IntoTarget, LValue, RaiseLevel,
    RaiseOption, ReturnKind, Stmt, TypeRef, VarDecl,
};
use crate::tokenizer::{tokenize, Keyword, Punct, Token, TokenKind, TokenizeError};

/// Parsing failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    pub line: usize,
    pub column: usize,
    pub offset: usize,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "parse error at line {} column {}: {}",
            self.line, self.column, self.message
        )
    }
}

impl std::error::Error for ParseError {}

impl From<TokenizeError> for ParseError {
    fn from(err: TokenizeError) -> Self {
        Self {
            message: err.message,
            line: err.line,
            column: err.column,
            offset: err.offset,
        }
    }
}

/// Parse the complete body of a `DO` block or function. Accepts either a
/// bare `BEGIN ... END` or a `DECLARE ... BEGIN ... END` pair.
pub fn parse_block(src: &str) -> Result<Block, ParseError> {
    let tokens = tokenize(src)?;
    let mut parser = Parser::new(src, tokens);
    let block = parser.parse_top_block()?;
    parser.expect_eof()?;
    Ok(block)
}

/// Parse a function body. Currently identical to [`parse_block`] but reserved
/// for possible future divergence (e.g. optional trailing `$$`).
pub fn parse_function_body(src: &str) -> Result<Block, ParseError> {
    parse_block(src)
}

/// Maximum nested PL/pgSQL block depth. Recursive descent over deeply
/// nested BEGIN/END would otherwise overflow the worker thread stack and
/// abort the engine process (audit plpgsql F-stack-overflow).
const MAX_BLOCK_NESTING_DEPTH: usize = 64;

struct Parser<'a> {
    src: &'a str,
    tokens: Vec<Token>,
    pos: usize,
    block_depth: usize,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str, tokens: Vec<Token>) -> Self {
        Self {
            src,
            tokens,
            pos: 0,
            block_depth: 0,
        }
    }

    fn peek(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn peek_kind(&self, offset: usize) -> &TokenKind {
        let idx = (self.pos + offset).min(self.tokens.len() - 1);
        &self.tokens[idx].kind
    }

    fn bump(&mut self) -> Token {
        let t = self.tokens[self.pos].clone();
        if !matches!(t.kind, TokenKind::Eof) {
            self.pos += 1;
        }
        t
    }

    fn check_keyword(&self, keyword: Keyword) -> bool {
        matches!(self.peek().kind, TokenKind::Keyword(k) if k == keyword)
    }

    fn eat_keyword(&mut self, keyword: Keyword) -> bool {
        if self.check_keyword(keyword) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn check_ident_ci(&self, expected: &str) -> bool {
        match &self.peek().kind {
            TokenKind::Ident | TokenKind::QuotedIdent => {
                self.peek().lexeme.eq_ignore_ascii_case(expected)
            }
            _ => false,
        }
    }

    fn eat_ident_ci(&mut self, expected: &str) -> bool {
        if self.check_ident_ci(expected) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect_keyword(&mut self, keyword: Keyword) -> Result<(), ParseError> {
        if self.eat_keyword(keyword) {
            Ok(())
        } else {
            Err(self.error(&format!("expected keyword `{}`", keyword.as_str())))
        }
    }

    fn check_punct(&self, punct: Punct) -> bool {
        matches!(self.peek().kind, TokenKind::Punct(p) if p == punct)
    }

    fn eat_punct(&mut self, punct: Punct) -> bool {
        if self.check_punct(punct) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect_punct(&mut self, punct: Punct) -> Result<(), ParseError> {
        if self.eat_punct(punct) {
            Ok(())
        } else {
            Err(self.error(&format!("expected `{punct}`")))
        }
    }

    fn expect_eof(&mut self) -> Result<(), ParseError> {
        if matches!(self.peek().kind, TokenKind::Eof) {
            Ok(())
        } else {
            Err(self.error("unexpected trailing input after END"))
        }
    }

    fn error(&self, message: &str) -> ParseError {
        let t = self.peek();
        ParseError {
            message: message.to_owned(),
            line: t.line,
            column: t.column,
            offset: t.offset,
        }
    }

    fn parse_identifier(&mut self) -> Result<String, ParseError> {
        match &self.peek().kind {
            TokenKind::Ident => {
                let t = self.bump();
                Ok(t.lexeme)
            }
            TokenKind::QuotedIdent => {
                let t = self.bump();
                Ok(t.lexeme)
            }
            TokenKind::Keyword(k) if keyword_can_be_identifier(*k) => {
                let t = self.bump();
                Ok(t.lexeme)
            }
            _ => Err(self.error("expected identifier")),
        }
    }

    fn parse_top_block(&mut self) -> Result<Block, ParseError> {
        let label = self.parse_optional_label()?;
        self.parse_block_after_label(label)
    }

    fn parse_optional_label(&mut self) -> Result<Option<String>, ParseError> {
        // `<< ident >>` - two consecutive `<` then an identifier then two `>`.
        if matches!(self.peek().kind, TokenKind::Punct(Punct::Lt))
            && matches!(self.peek_kind(1), TokenKind::Punct(Punct::Lt))
        {
            self.bump();
            self.bump();
            let name = self.parse_identifier()?;
            if !matches!(self.peek().kind, TokenKind::Punct(Punct::Gt)) {
                return Err(self.error("expected `>>` closing label"));
            }
            self.bump();
            if !matches!(self.peek().kind, TokenKind::Punct(Punct::Gt)) {
                return Err(self.error("expected `>>` closing label"));
            }
            self.bump();
            return Ok(Some(name));
        }
        Ok(None)
    }

    fn parse_block_after_label(&mut self, label: Option<String>) -> Result<Block, ParseError> {
        // Cap nested-block depth: recursive descent would otherwise
        // stack-overflow on adversarial CREATE FUNCTION bodies and abort
        // the entire engine process.
        self.block_depth = self.block_depth.saturating_add(1);
        if self.block_depth > MAX_BLOCK_NESTING_DEPTH {
            self.block_depth = self.block_depth.saturating_sub(1);
            return Err(self.error(&format!(
                "PL/pgSQL block nesting depth exceeds maximum {MAX_BLOCK_NESTING_DEPTH}"
            )));
        }
        let result = self.parse_block_after_label_inner(label);
        self.block_depth = self.block_depth.saturating_sub(1);
        result
    }

    fn parse_block_after_label_inner(
        &mut self,
        label: Option<String>,
    ) -> Result<Block, ParseError> {
        let declarations = if self.eat_keyword(Keyword::Declare) {
            self.parse_declarations()?
        } else {
            Vec::new()
        };
        self.expect_keyword(Keyword::Begin)?;
        let begin_offset = self.tokens[self.pos.saturating_sub(1)].offset;
        let body = self.parse_stmts_until_block_end()?;
        let exception_handlers = if self.eat_keyword(Keyword::Exception) {
            self.parse_exception_handlers()?
        } else {
            Vec::new()
        };
        self.expect_keyword(Keyword::End)?;
        // Optional trailing label repetition (we don't enforce match).
        if matches!(self.peek().kind, TokenKind::Ident | TokenKind::QuotedIdent) {
            self.bump();
        }
        self.eat_punct(Punct::Semicolon);
        Ok(Block {
            label,
            declarations,
            body,
            exception_handlers,
            begin_offset,
        })
    }

    fn parse_declarations(&mut self) -> Result<Vec<VarDecl>, ParseError> {
        let mut decls = Vec::new();
        loop {
            if self.check_keyword(Keyword::Begin) {
                break;
            }
            let decl = self.parse_single_declaration()?;
            decls.push(decl);
            self.expect_punct(Punct::Semicolon)?;
        }
        Ok(decls)
    }

    fn parse_single_declaration(&mut self) -> Result<VarDecl, ParseError> {
        let name = self.parse_identifier()?;
        if self.eat_keyword(Keyword::Alias) {
            self.expect_keyword(Keyword::For)?;
            let target = self.parse_identifier()?;
            return Ok(VarDecl::Alias { name, target });
        }
        let mut is_constant = false;
        if self.eat_keyword(Keyword::Constant) {
            is_constant = true;
        }
        // CURSOR declarations
        if self.eat_keyword(Keyword::Cursor)
            || (self.check_keyword(Keyword::No) && self.peek_is_cursor_after_no())
            || (self.check_ident_ci("scroll") && self.peek_is_cursor_after_scroll())
        {
            return self.parse_cursor_declaration(name, is_constant);
        }
        let type_ref = self.parse_type_ref()?;
        let mut not_null = false;
        if self.eat_keyword(Keyword::Not) {
            self.expect_keyword(Keyword::Null)?;
            not_null = true;
        }
        let default = if self.eat_keyword(Keyword::Default)
            || self.eat_punct(Punct::Assign)
            || self.eat_punct(Punct::Equal)
        {
            Some(self.consume_expression_until(&[TokenKind::Punct(Punct::Semicolon)])?)
        } else {
            None
        };
        Ok(VarDecl::Scalar {
            name,
            is_constant,
            not_null,
            type_ref,
            default,
        })
    }

    fn peek_is_cursor_after_no(&self) -> bool {
        matches!(self.peek_kind(1), TokenKind::Keyword(Keyword::Cursor))
            || (matches!(self.peek_kind(1), TokenKind::Ident | TokenKind::QuotedIdent)
                && matches!(self.peek_kind(2), TokenKind::Keyword(Keyword::Cursor)))
    }

    fn peek_is_cursor_after_scroll(&self) -> bool {
        matches!(self.peek_kind(1), TokenKind::Keyword(Keyword::Cursor))
    }

    fn parse_cursor_declaration(
        &mut self,
        name: String,
        is_constant: bool,
    ) -> Result<VarDecl, ParseError> {
        // `REFCURSOR` alone declares an unbound cursor variable.
        if self.eat_keyword(Keyword::Refcursor) {
            return Ok(VarDecl::Cursor(CursorDecl {
                name,
                is_constant,
                parameters: Vec::new(),
                query: None,
                scroll: None,
                no_scroll: false,
            }));
        }
        let mut parameters = Vec::new();
        if self.eat_punct(Punct::LParen) {
            loop {
                let pname = self.parse_identifier()?;
                let ptype = self.parse_type_ref()?;
                parameters.push((pname, ptype));
                if !self.eat_punct(Punct::Comma) {
                    break;
                }
            }
            self.expect_punct(Punct::RParen)?;
        }
        let mut scroll = None;
        let mut no_scroll = false;
        if self.eat_keyword(Keyword::No) {
            if self.eat_ident_ci("scroll") {
                no_scroll = true;
                let _ = self.eat_keyword(Keyword::Cursor);
            } else if self.eat_keyword(Keyword::Cursor) {
                no_scroll = true;
            }
        } else if self.eat_ident_ci("scroll") {
            scroll = Some(true);
            let _ = self.eat_keyword(Keyword::Cursor);
        } else if self.eat_keyword(Keyword::Cursor) {
            scroll = Some(true);
        }
        let query = if self.eat_keyword(Keyword::For) || self.eat_keyword(Keyword::Is) {
            Some(self.consume_expression_until(&[TokenKind::Punct(Punct::Semicolon)])?)
        } else {
            None
        };
        Ok(VarDecl::Cursor(CursorDecl {
            name,
            is_constant,
            parameters,
            query,
            scroll,
            no_scroll,
        }))
    }

    fn parse_type_ref(&mut self) -> Result<TypeRef, ParseError> {
        // Fast path: RECORD / REFCURSOR.
        if self.eat_keyword(Keyword::Record) {
            return Ok(TypeRef::Record);
        }
        if self.eat_keyword(Keyword::Refcursor) {
            return Ok(TypeRef::Refcursor);
        }
        let first = self.parse_identifier()?;
        if self.eat_punct(Punct::Dot) {
            let second = self.parse_identifier()?;
            if self.eat_punct(Punct::Percent) {
                if self.eat_keyword(Keyword::Type) {
                    return Ok(TypeRef::ColumnType {
                        relation: first,
                        column: second,
                    });
                }
                if self.eat_keyword(Keyword::Rowtype) {
                    return Ok(TypeRef::RowType(format!("{first}.{second}")));
                }
                return Err(self.error("expected %TYPE or %ROWTYPE"));
            }
            return Ok(TypeRef::Named(format!("{first}.{second}")));
        }
        if self.eat_punct(Punct::Percent) {
            if self.eat_keyword(Keyword::Type) {
                return Ok(TypeRef::VariableType(first));
            }
            if self.eat_keyword(Keyword::Rowtype) {
                return Ok(TypeRef::RowType(first));
            }
            return Err(self.error("expected %TYPE or %ROWTYPE"));
        }
        let mut name = first;
        // Parameterised types: numeric(10,2), varchar(32), timestamp(0) with
        // time zone, etc. We consume until we hit `;`, `:=`, `=`, `DEFAULT`,
        // `NOT`, or another declaration-terminating token.
        if self.eat_punct(Punct::LParen) {
            let inner = self.consume_expression_until(&[TokenKind::Punct(Punct::RParen)])?;
            self.expect_punct(Punct::RParen)?;
            name.push('(');
            name.push_str(&inner);
            name.push(')');
        }
        while self.eat_punct(Punct::LBracket) {
            let inner = if self.check_punct(Punct::RBracket) {
                String::new()
            } else {
                self.consume_expression_until(&[TokenKind::Punct(Punct::RBracket)])?
            };
            self.expect_punct(Punct::RBracket)?;
            name.push('[');
            name.push_str(&inner);
            name.push(']');
        }
        Ok(TypeRef::Named(name))
    }

    fn parse_exception_handlers(&mut self) -> Result<Vec<ExceptionHandler>, ParseError> {
        let mut handlers = Vec::new();
        while self.eat_keyword(Keyword::When) {
            let mut conditions = Vec::new();
            loop {
                conditions.push(self.parse_exception_condition()?);
                if !self.eat_keyword(Keyword::Or) {
                    break;
                }
            }
            self.expect_keyword(Keyword::Then)?;
            let body = self.parse_stmts_until_block_end()?;
            handlers.push(ExceptionHandler { conditions, body });
        }
        if handlers.is_empty() {
            return Err(self.error("EXCEPTION block requires at least one WHEN clause"));
        }
        Ok(handlers)
    }

    fn parse_exception_condition(&mut self) -> Result<ExceptionCondition, ParseError> {
        if self.eat_keyword(Keyword::Others) {
            return Ok(ExceptionCondition::Others);
        }
        if self.eat_keyword(Keyword::Sqlstate) {
            match &self.peek().kind {
                TokenKind::String { .. } => {
                    let t = self.bump();
                    return Ok(ExceptionCondition::SqlState(t.lexeme));
                }
                _ => return Err(self.error("SQLSTATE requires a string literal")),
            }
        }
        let name = self.parse_identifier()?;
        Ok(ExceptionCondition::Named(name))
    }

    fn parse_stmts_until_block_end(&mut self) -> Result<Vec<Stmt>, ParseError> {
        let mut stmts = Vec::new();
        loop {
            match &self.peek().kind {
                TokenKind::Eof => return Err(self.error("unexpected EOF inside block")),
                TokenKind::Keyword(Keyword::End | Keyword::Exception) => return Ok(stmts),
                TokenKind::Keyword(Keyword::Elsif | Keyword::Else | Keyword::When) => {
                    return Ok(stmts)
                }
                _ => {}
            }
            let stmt = self.parse_statement()?;
            stmts.push(stmt);
        }
    }

    fn parse_statement(&mut self) -> Result<Stmt, ParseError> {
        // Optional `<<label>>` prefix applies to the following block or loop.
        let label = self.parse_optional_label()?;
        // Label-less `BEGIN` / `DECLARE` introduce a nested block.
        if self.check_keyword(Keyword::Declare) || self.check_keyword(Keyword::Begin) {
            let block = self.parse_block_after_label(label)?;
            return Ok(Stmt::Block(block));
        }
        if label.is_some() {
            // Remaining statement types expect the optional label to attach
            // to the loop/while/for directly. For other statements we reject
            // - PL/pgSQL allows labels only on blocks and loops.
            if !matches!(
                self.peek().kind,
                TokenKind::Keyword(
                    Keyword::Loop | Keyword::While | Keyword::For | Keyword::Foreach,
                )
            ) {
                return Err(self.error("label only allowed on BEGIN / LOOP / WHILE / FOR"));
            }
        }
        if self.eat_keyword(Keyword::Null) {
            self.expect_punct(Punct::Semicolon)?;
            return Ok(Stmt::Null);
        }
        if self.eat_keyword(Keyword::If) {
            return self.parse_if_statement();
        }
        if self.eat_keyword(Keyword::Case) {
            return self.parse_case_statement();
        }
        if self.eat_keyword(Keyword::Loop) {
            let body = self.parse_stmts_until_block_end()?;
            self.expect_keyword(Keyword::End)?;
            self.expect_keyword(Keyword::Loop)?;
            self.consume_optional_label_after_end(label.as_deref())?;
            self.expect_punct(Punct::Semicolon)?;
            return Ok(Stmt::Loop { label, body });
        }
        if self.eat_keyword(Keyword::While) {
            let condition = self.consume_expression_until(&[TokenKind::Keyword(Keyword::Loop)])?;
            self.expect_keyword(Keyword::Loop)?;
            let body = self.parse_stmts_until_block_end()?;
            self.expect_keyword(Keyword::End)?;
            self.expect_keyword(Keyword::Loop)?;
            self.consume_optional_label_after_end(label.as_deref())?;
            self.expect_punct(Punct::Semicolon)?;
            return Ok(Stmt::While {
                label,
                condition,
                body,
            });
        }
        if self.eat_keyword(Keyword::For) {
            return self.parse_for_statement_with_label(label);
        }
        if self.eat_keyword(Keyword::Foreach) {
            return self.parse_foreach_statement_with_label(label);
        }
        if self.eat_keyword(Keyword::Exit) {
            return self.parse_exit_continue(true);
        }
        if self.eat_keyword(Keyword::Continue) {
            return self.parse_exit_continue(false);
        }
        // `return` can be used as an identifier in PL/pgSQL declarations.
        // Treat `return := ...;` as assignment; otherwise it's RETURN stmt.
        if self.check_keyword(Keyword::Return)
            && matches!(
                self.peek_kind(1),
                TokenKind::Punct(Punct::Assign | Punct::Equal)
            )
        {
            return self.parse_assignment_or_sql();
        }
        if self.eat_keyword(Keyword::Return) {
            return self.parse_return_statement();
        }
        if self.eat_keyword(Keyword::Perform) {
            let expr = self.consume_expression_until(&[TokenKind::Punct(Punct::Semicolon)])?;
            self.expect_punct(Punct::Semicolon)?;
            return Ok(Stmt::Perform(expr));
        }
        if self.eat_keyword(Keyword::Execute) {
            return self.parse_execute_statement();
        }
        if self.eat_keyword(Keyword::Raise) {
            return self.parse_raise_statement();
        }
        if self.eat_keyword(Keyword::Assert) {
            let condition = self.consume_expression_until(&[
                TokenKind::Punct(Punct::Semicolon),
                TokenKind::Punct(Punct::Comma),
            ])?;
            let message = if self.eat_punct(Punct::Comma) {
                Some(self.consume_expression_until(&[TokenKind::Punct(Punct::Semicolon)])?)
            } else {
                None
            };
            self.expect_punct(Punct::Semicolon)?;
            return Ok(Stmt::Assert { condition, message });
        }
        if self.eat_keyword(Keyword::Get) {
            return self.parse_get_diagnostics();
        }
        if self.eat_keyword(Keyword::Open) {
            return self.parse_open_cursor();
        }
        if self.eat_keyword(Keyword::Fetch) {
            return self.parse_fetch_or_move(false);
        }
        if self.eat_keyword(Keyword::Move) {
            return self.parse_fetch_or_move(true);
        }
        if self.eat_keyword(Keyword::Close) {
            let cursor = self.parse_identifier()?;
            self.expect_punct(Punct::Semicolon)?;
            return Ok(Stmt::Close { cursor });
        }

        // Otherwise: either an assignment or a plain SQL statement.
        self.parse_assignment_or_sql()
    }

    fn parse_if_statement(&mut self) -> Result<Stmt, ParseError> {
        let mut branches = Vec::new();
        let mut else_body = Vec::new();
        loop {
            let condition = self.consume_expression_until(&[TokenKind::Keyword(Keyword::Then)])?;
            self.expect_keyword(Keyword::Then)?;
            let body = self.parse_stmts_until_block_end()?;
            branches.push(IfBranch { condition, body });
            if self.eat_keyword(Keyword::Elsif) {
                continue;
            }
            if self.eat_keyword(Keyword::Else) {
                else_body = self.parse_stmts_until_block_end()?;
            }
            self.expect_keyword(Keyword::End)?;
            self.expect_keyword(Keyword::If)?;
            self.expect_punct(Punct::Semicolon)?;
            break;
        }
        Ok(Stmt::If {
            branches,
            else_body,
        })
    }

    fn parse_case_statement(&mut self) -> Result<Stmt, ParseError> {
        let selector = if self.check_keyword(Keyword::When) {
            None
        } else {
            Some(self.consume_expression_until(&[TokenKind::Keyword(Keyword::When)])?)
        };
        let mut arms = Vec::new();
        while self.eat_keyword(Keyword::When) {
            let mut matches = Vec::new();
            loop {
                let expr = self.consume_expression_until(&[
                    TokenKind::Punct(Punct::Comma),
                    TokenKind::Keyword(Keyword::Then),
                ])?;
                matches.push(expr);
                if !self.eat_punct(Punct::Comma) {
                    break;
                }
            }
            self.expect_keyword(Keyword::Then)?;
            let body = self.parse_stmts_until_block_end()?;
            arms.push(CaseArm { matches, body });
        }
        let else_body = if self.eat_keyword(Keyword::Else) {
            Some(self.parse_stmts_until_block_end()?)
        } else {
            None
        };
        self.expect_keyword(Keyword::End)?;
        self.expect_keyword(Keyword::Case)?;
        self.expect_punct(Punct::Semicolon)?;
        Ok(Stmt::Case(CaseStmt {
            selector,
            arms,
            else_body,
        }))
    }

    fn consume_optional_label_after_end(&mut self, _label: Option<&str>) -> Result<(), ParseError> {
        if matches!(self.peek().kind, TokenKind::Ident | TokenKind::QuotedIdent) {
            // PL/pgSQL allows `END LOOP <label>;` - match best-effort without
            // enforcing name equality here. The interpreter only cares about
            // the prefix label.
            self.bump();
        }
        Ok(())
    }

    fn parse_for_statement_with_label(
        &mut self,
        label: Option<String>,
    ) -> Result<Stmt, ParseError> {
        let mut stmt = self.parse_for_statement()?;
        if let Stmt::For {
            label: ref mut slot,
            ..
        } = stmt
        {
            *slot = label;
        }
        Ok(stmt)
    }

    fn parse_foreach_statement_with_label(
        &mut self,
        label: Option<String>,
    ) -> Result<Stmt, ParseError> {
        let mut stmt = self.parse_foreach_statement()?;
        if let Stmt::Foreach {
            label: ref mut slot,
            ..
        } = stmt
        {
            *slot = label;
        }
        Ok(stmt)
    }

    fn parse_for_statement(&mut self) -> Result<Stmt, ParseError> {
        // We need to distinguish integer FOR, cursor FOR, and query FOR.
        // Syntax:
        //   FOR var IN [REVERSE] lower..upper [BY step] LOOP
        //   FOR var [, var, ...] IN <query> LOOP
        //   FOR record IN cursor_name [(args)] LOOP
        let first_var = self.parse_identifier()?;
        let mut targets = vec![first_var];
        while self.eat_punct(Punct::Comma) {
            targets.push(self.parse_identifier()?);
        }
        self.expect_keyword(Keyword::In)?;
        let reverse = self.eat_keyword(Keyword::Reverse);
        // Snapshot to disambiguate integer-range vs cursor/query.
        let save = self.pos;
        if targets.len() == 1 && !reverse {
            // Try cursor form: identifier (optional args) LOOP.
            if matches!(self.peek().kind, TokenKind::Ident | TokenKind::QuotedIdent)
                && self.looks_like_cursor_for()
            {
                let cursor_name = self.parse_identifier()?;
                let mut arguments = Vec::new();
                if self.eat_punct(Punct::LParen) {
                    if !self.check_punct(Punct::RParen) {
                        loop {
                            let name_token_start = self.pos;
                            let maybe_name = if matches!(
                                self.peek().kind,
                                TokenKind::Ident | TokenKind::QuotedIdent
                            ) && matches!(
                                self.peek_kind(1),
                                TokenKind::Punct(Punct::Assign)
                            ) || (matches!(
                                self.peek().kind,
                                TokenKind::Keyword(k) if keyword_can_be_identifier(k)
                            ) && matches!(
                                self.peek_kind(1),
                                TokenKind::Punct(Punct::Assign)
                            )) {
                                let n = self.parse_identifier()?;
                                self.bump();
                                Some(n)
                            } else {
                                self.pos = name_token_start;
                                None
                            };
                            let expr = self.consume_expression_until(&[
                                TokenKind::Punct(Punct::Comma),
                                TokenKind::Punct(Punct::RParen),
                            ])?;
                            arguments.push((maybe_name, expr));
                            if !self.eat_punct(Punct::Comma) {
                                break;
                            }
                        }
                    }
                    self.expect_punct(Punct::RParen)?;
                }
                self.expect_keyword(Keyword::Loop)?;
                let body = self.parse_stmts_until_block_end()?;
                self.expect_keyword(Keyword::End)?;
                self.expect_keyword(Keyword::Loop)?;
                self.expect_punct(Punct::Semicolon)?;
                return Ok(Stmt::For {
                    label: None,
                    kind: ForLoopKind::Cursor {
                        target: targets.remove(0),
                        cursor: cursor_name,
                        arguments,
                    },
                    body,
                });
            }
        }
        // Integer-range FOR: <expr>..<expr> [BY <expr>] LOOP.
        if self.expression_contains_range_before_loop() {
            let variable = targets.remove(0);
            let lower = self.consume_expression_until(&[TokenKind::Punct(Punct::DotDot)])?;
            self.expect_punct(Punct::DotDot)?;
            let upper = self.consume_expression_until(&[
                TokenKind::Keyword(Keyword::Loop),
                TokenKind::Keyword(Keyword::By),
            ])?;
            let step = if self.eat_keyword(Keyword::By) {
                Some(self.consume_expression_until(&[TokenKind::Keyword(Keyword::Loop)])?)
            } else {
                None
            };
            self.expect_keyword(Keyword::Loop)?;
            let body = self.parse_stmts_until_block_end()?;
            self.expect_keyword(Keyword::End)?;
            self.expect_keyword(Keyword::Loop)?;
            self.expect_punct(Punct::Semicolon)?;
            return Ok(Stmt::For {
                label: None,
                kind: ForLoopKind::Integer {
                    variable,
                    lower,
                    upper,
                    step,
                    reverse,
                },
                body,
            });
        }
        // Otherwise assume it's a query form: FOR targets IN <query> LOOP.
        self.pos = save;
        let query = self.consume_expression_until(&[TokenKind::Keyword(Keyword::Loop)])?;
        self.expect_keyword(Keyword::Loop)?;
        let body = self.parse_stmts_until_block_end()?;
        self.expect_keyword(Keyword::End)?;
        self.expect_keyword(Keyword::Loop)?;
        self.expect_punct(Punct::Semicolon)?;
        Ok(Stmt::For {
            label: None,
            kind: ForLoopKind::Query { targets, query },
            body,
        })
    }

    fn looks_like_cursor_for(&self) -> bool {
        // After a single-identifier IN clause followed by `(` or `LOOP`, treat
        // as a cursor FOR loop.
        let mut i = self.pos;
        if !matches!(
            self.tokens[i].kind,
            TokenKind::Ident | TokenKind::QuotedIdent
        ) {
            return false;
        }
        i += 1;
        match self.tokens.get(i).map(|t| &t.kind) {
            Some(TokenKind::Punct(Punct::LParen)) => {
                // Distinguish cursor invocation syntax (`cursor_name(...) LOOP`)
                // from integer-range bounds that start with a function call
                // (`array_lower(...)..array_upper(...)`).
                let mut depth = 1i32;
                i += 1;
                while i < self.tokens.len() {
                    match self.tokens[i].kind {
                        TokenKind::Punct(Punct::LParen) => depth += 1,
                        TokenKind::Punct(Punct::RParen) => {
                            depth -= 1;
                            if depth == 0 {
                                i += 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                    i += 1;
                }
                depth == 0
                    && matches!(
                        self.tokens.get(i).map(|t| &t.kind),
                        Some(TokenKind::Keyword(Keyword::Loop))
                    )
            }
            Some(TokenKind::Keyword(Keyword::Loop)) => true,
            _ => false,
        }
    }

    fn expression_contains_range_before_loop(&self) -> bool {
        let mut depth = 0i32;
        for i in self.pos..self.tokens.len() {
            match &self.tokens[i].kind {
                TokenKind::Punct(Punct::LParen | Punct::LBracket) => depth += 1,
                TokenKind::Punct(Punct::RParen | Punct::RBracket) => depth -= 1,
                TokenKind::Punct(Punct::DotDot) if depth == 0 => return true,
                TokenKind::Keyword(Keyword::Loop) if depth == 0 => return false,
                TokenKind::Punct(Punct::Semicolon) if depth == 0 => return false,
                TokenKind::Eof => return false,
                _ => {}
            }
        }
        false
    }

    fn parse_foreach_statement(&mut self) -> Result<Stmt, ParseError> {
        let mut target = vec![self.parse_identifier()?];
        while self.eat_punct(Punct::Comma) {
            target.push(self.parse_identifier()?);
        }
        let slice = if self.eat_keyword(Keyword::Slice) {
            let lit = match &self.peek().kind {
                TokenKind::Number => self.bump().lexeme,
                _ => return Err(self.error("SLICE requires an integer literal")),
            };
            Some(
                lit.parse::<u32>()
                    .map_err(|_| self.error("SLICE literal must be an unsigned integer"))?,
            )
        } else {
            None
        };
        self.expect_keyword(Keyword::In)?;
        self.expect_keyword(Keyword::Array)?;
        let array_expr = self.consume_expression_until(&[TokenKind::Keyword(Keyword::Loop)])?;
        self.expect_keyword(Keyword::Loop)?;
        let body = self.parse_stmts_until_block_end()?;
        self.expect_keyword(Keyword::End)?;
        self.expect_keyword(Keyword::Loop)?;
        self.expect_punct(Punct::Semicolon)?;
        Ok(Stmt::Foreach {
            label: None,
            target,
            slice,
            array_expr,
            body,
        })
    }

    fn parse_exit_continue(&mut self, exit: bool) -> Result<Stmt, ParseError> {
        // `EXIT [label] [WHEN cond];`  - the label is any identifier
        // immediately following EXIT/CONTINUE. It stays optional so bare
        // `EXIT WHEN ...;` still parses.
        let label = if matches!(self.peek().kind, TokenKind::Ident | TokenKind::QuotedIdent) {
            Some(self.parse_identifier()?)
        } else {
            None
        };
        let when = if self.eat_keyword(Keyword::When) {
            Some(self.consume_expression_until(&[TokenKind::Punct(Punct::Semicolon)])?)
        } else {
            None
        };
        self.expect_punct(Punct::Semicolon)?;
        Ok(if exit {
            Stmt::Exit { label, when }
        } else {
            Stmt::Continue { label, when }
        })
    }

    fn parse_return_statement(&mut self) -> Result<Stmt, ParseError> {
        if self.eat_punct(Punct::Semicolon) {
            return Ok(Stmt::Return(ReturnKind::Void));
        }
        if self.eat_keyword(Keyword::Next) {
            if self.eat_punct(Punct::Semicolon) {
                return Ok(Stmt::Return(ReturnKind::Next(String::new())));
            }
            let expr = self.consume_expression_until(&[TokenKind::Punct(Punct::Semicolon)])?;
            self.expect_punct(Punct::Semicolon)?;
            return Ok(Stmt::Return(ReturnKind::Next(expr)));
        }
        if self.eat_keyword(Keyword::Query) {
            if self.eat_keyword(Keyword::Execute) {
                let (command, using) = self.parse_execute_tail()?;
                return Ok(Stmt::Return(ReturnKind::QueryExecute { command, using }));
            }
            let expr = self.consume_expression_until(&[TokenKind::Punct(Punct::Semicolon)])?;
            self.expect_punct(Punct::Semicolon)?;
            return Ok(Stmt::Return(ReturnKind::QueryExpr(expr)));
        }
        let expr = self.consume_expression_until(&[TokenKind::Punct(Punct::Semicolon)])?;
        self.expect_punct(Punct::Semicolon)?;
        Ok(Stmt::Return(ReturnKind::Expr(expr)))
    }

    fn parse_execute_statement(&mut self) -> Result<Stmt, ParseError> {
        let command = self.consume_expression_until(&[
            TokenKind::Keyword(Keyword::Into),
            TokenKind::Keyword(Keyword::Using),
            TokenKind::Punct(Punct::Semicolon),
        ])?;
        let mut into = None;
        let mut using = Vec::new();
        loop {
            if into.is_none() && self.eat_keyword(Keyword::Into) {
                into = Some(self.parse_into_target()?);
                continue;
            }
            if self.eat_keyword(Keyword::Using) {
                using.extend(self.parse_using_until_into_or_semicolon()?);
                continue;
            }
            break;
        }
        self.expect_punct(Punct::Semicolon)?;
        Ok(Stmt::Execute {
            command,
            into,
            using,
        })
    }

    fn parse_execute_tail(&mut self) -> Result<(String, Vec<String>), ParseError> {
        let command = self.consume_expression_until(&[
            TokenKind::Keyword(Keyword::Using),
            TokenKind::Punct(Punct::Semicolon),
        ])?;
        let using = self.maybe_parse_using()?;
        self.expect_punct(Punct::Semicolon)?;
        Ok((command, using))
    }

    fn parse_into_target(&mut self) -> Result<IntoTarget, ParseError> {
        let strict = self.eat_keyword(Keyword::Strict);
        let mut targets = vec![self.parse_identifier()?];
        while self.eat_punct(Punct::Comma) {
            targets.push(self.parse_identifier()?);
        }
        Ok(IntoTarget { strict, targets })
    }

    fn maybe_parse_using(&mut self) -> Result<Vec<String>, ParseError> {
        if !self.eat_keyword(Keyword::Using) {
            return Ok(Vec::new());
        }
        let mut args = Vec::new();
        loop {
            let expr = self.consume_expression_until(&[
                TokenKind::Punct(Punct::Comma),
                TokenKind::Punct(Punct::Semicolon),
            ])?;
            args.push(expr);
            if !self.eat_punct(Punct::Comma) {
                break;
            }
        }
        Ok(args)
    }

    fn parse_using_until_into_or_semicolon(&mut self) -> Result<Vec<String>, ParseError> {
        let mut args = Vec::new();
        loop {
            let expr = self.consume_expression_until(&[
                TokenKind::Punct(Punct::Comma),
                TokenKind::Keyword(Keyword::Into),
                TokenKind::Punct(Punct::Semicolon),
            ])?;
            args.push(expr);
            if self.eat_punct(Punct::Comma) {
                continue;
            }
            break;
        }
        Ok(args)
    }

    fn parse_raise_statement(&mut self) -> Result<Stmt, ParseError> {
        let level = self.parse_raise_level();
        let mut options = RaiseOption::default();
        if self.eat_punct(Punct::Semicolon) {
            options.reraise = true;
            return Ok(Stmt::Raise { level, options });
        }
        if self.eat_keyword(Keyword::Sqlstate) {
            match &self.peek().kind {
                TokenKind::String { .. } => {
                    let t = self.bump();
                    options.sqlstate = Some(t.lexeme);
                }
                _ => return Err(self.error("SQLSTATE requires a string literal")),
            }
        } else if matches!(&self.peek().kind, TokenKind::String { .. }) {
            let t = self.bump();
            options.format = Some(t.lexeme);
            while self.eat_punct(Punct::Comma) {
                let expr = self.consume_expression_until(&[
                    TokenKind::Punct(Punct::Comma),
                    TokenKind::Keyword(Keyword::Using),
                    TokenKind::Punct(Punct::Semicolon),
                ])?;
                options.arguments.push(expr);
            }
        } else if matches!(&self.peek().kind, TokenKind::Ident | TokenKind::QuotedIdent) {
            let t = self.bump();
            options.condition_name = Some(t.lexeme);
        }
        if self.eat_keyword(Keyword::Using) {
            loop {
                let name = self.parse_identifier()?;
                if !self.eat_punct(Punct::Equal) {
                    return Err(self.error("expected `=` after USING option name"));
                }
                let expr = self.consume_expression_until(&[
                    TokenKind::Punct(Punct::Comma),
                    TokenKind::Punct(Punct::Semicolon),
                ])?;
                options.using.push((name, expr));
                if !self.eat_punct(Punct::Comma) {
                    break;
                }
            }
        }
        self.expect_punct(Punct::Semicolon)?;
        Ok(Stmt::Raise { level, options })
    }

    fn parse_raise_level(&mut self) -> RaiseLevel {
        if self.eat_keyword(Keyword::Notice) {
            RaiseLevel::Notice
        } else if self.eat_keyword(Keyword::Warning) {
            RaiseLevel::Warning
        } else if self.eat_keyword(Keyword::Info) {
            RaiseLevel::Info
        } else if self.eat_keyword(Keyword::Log) {
            RaiseLevel::Log
        } else if self.eat_keyword(Keyword::Debug) {
            RaiseLevel::Debug
        } else {
            // `EXCEPTION` is optional - absence defaults to EXCEPTION per PG.
            let _ = self.eat_keyword(Keyword::Exception);
            RaiseLevel::Exception
        }
    }

    fn parse_get_diagnostics(&mut self) -> Result<Stmt, ParseError> {
        let stacked = self.eat_keyword(Keyword::Stacked);
        self.expect_keyword(Keyword::Diagnostics)?;
        let mut items = Vec::new();
        loop {
            let target = self.parse_identifier()?;
            if !self.eat_punct(Punct::Assign) && !self.eat_punct(Punct::Equal) {
                return Err(self.error("expected := or = in GET DIAGNOSTICS"));
            }
            let source = self.parse_identifier()?;
            let kind = match source.as_str() {
                "row_count" => DiagnosticsKind::RowCount,
                "result_oid" => DiagnosticsKind::ResultOid,
                "pg_routine_oid" => DiagnosticsKind::PgRoutineOid,
                "pg_context" => DiagnosticsKind::PgContext,
                "pg_exception_context" => DiagnosticsKind::PgExceptionContext,
                "pg_exception_detail" => DiagnosticsKind::PgExceptionDetail,
                "pg_exception_hint" => DiagnosticsKind::PgExceptionHint,
                "returned_sqlstate" => DiagnosticsKind::SqlState,
                "message_text" => DiagnosticsKind::MessageText,
                "schema_name" => DiagnosticsKind::SchemaName,
                "table_name" => DiagnosticsKind::TableName,
                "column_name" => DiagnosticsKind::ColumnName,
                "constraint_name" => DiagnosticsKind::ConstraintName,
                "datatype_name" | "pg_datatype_name" => DiagnosticsKind::DatatypeName,
                "returned" => DiagnosticsKind::Returned,
                other => {
                    return Err(self.error(&format!("unknown diagnostics source `{other}`")));
                }
            };
            items.push(GetDiagnosticsItem { target, kind });
            if !self.eat_punct(Punct::Comma) {
                break;
            }
        }
        self.expect_punct(Punct::Semicolon)?;
        Ok(Stmt::GetDiagnostics { stacked, items })
    }

    fn parse_open_cursor(&mut self) -> Result<Stmt, ParseError> {
        let cursor = self.parse_identifier()?;
        let mut scroll = None;
        if self.eat_keyword(Keyword::No) {
            if self.eat_ident_ci("scroll") {
                scroll = Some(false);
                let _ = self.eat_keyword(Keyword::Cursor);
            } else {
                self.expect_keyword(Keyword::Cursor)?;
                scroll = Some(false);
            }
        } else if self.eat_ident_ci("scroll") {
            scroll = Some(true);
            let _ = self.eat_keyword(Keyword::Cursor);
        } else if self.eat_keyword(Keyword::Cursor) {
            scroll = Some(true);
        }
        let mut arguments = Vec::new();
        if self.eat_punct(Punct::LParen) {
            if !self.check_punct(Punct::RParen) {
                loop {
                    let name_token_start = self.pos;
                    let maybe_name =
                        if matches!(self.peek().kind, TokenKind::Ident | TokenKind::QuotedIdent)
                            && matches!(self.peek_kind(1), TokenKind::Punct(Punct::Assign))
                            || (matches!(
                                self.peek().kind,
                                TokenKind::Keyword(k) if keyword_can_be_identifier(k)
                            ) && matches!(self.peek_kind(1), TokenKind::Punct(Punct::Assign)))
                        {
                            let n = self.parse_identifier()?;
                            self.bump();
                            Some(n)
                        } else {
                            self.pos = name_token_start;
                            None
                        };
                    let expr = self.consume_expression_until(&[
                        TokenKind::Punct(Punct::Comma),
                        TokenKind::Punct(Punct::RParen),
                    ])?;
                    arguments.push((maybe_name, expr));
                    if !self.eat_punct(Punct::Comma) {
                        break;
                    }
                }
            }
            self.expect_punct(Punct::RParen)?;
        }
        let query = if self.eat_keyword(Keyword::For) {
            Some(self.consume_expression_until(&[TokenKind::Punct(Punct::Semicolon)])?)
        } else {
            None
        };
        self.expect_punct(Punct::Semicolon)?;
        Ok(Stmt::Open {
            cursor,
            arguments,
            query,
            scroll,
        })
    }

    fn parse_fetch_or_move(&mut self, is_move: bool) -> Result<Stmt, ParseError> {
        let (direction, count) = self.parse_fetch_direction_and_count()?;
        let _ = self.eat_keyword(Keyword::From) || self.eat_keyword(Keyword::In);
        let cursor = self.parse_identifier()?;
        let targets = if !is_move && self.eat_keyword(Keyword::Into) {
            let mut list = vec![self.parse_identifier()?];
            while self.eat_punct(Punct::Comma) {
                list.push(self.parse_identifier()?);
            }
            list
        } else {
            Vec::new()
        };
        self.expect_punct(Punct::Semicolon)?;
        Ok(Stmt::Fetch {
            cursor,
            direction,
            count,
            targets,
            is_move,
        })
    }

    fn parse_fetch_direction_and_count(
        &mut self,
    ) -> Result<(FetchDirection, Option<String>), ParseError> {
        if self.eat_keyword(Keyword::Next) {
            return Ok((FetchDirection::Next, None));
        }
        if self.eat_ident_ci("prior") {
            return Ok((FetchDirection::Prior, None));
        }
        if self.eat_ident_ci("first") {
            return Ok((FetchDirection::First, None));
        }
        if self.eat_ident_ci("last") {
            return Ok((FetchDirection::Last, None));
        }
        if self.eat_ident_ci("absolute") {
            let count = self.consume_expression_until(&[
                TokenKind::Keyword(Keyword::From),
                TokenKind::Keyword(Keyword::In),
                TokenKind::Keyword(Keyword::Into),
                TokenKind::Punct(Punct::Semicolon),
            ])?;
            return Ok((FetchDirection::Absolute, Some(count)));
        }
        if self.eat_ident_ci("relative") {
            let count = if self.check_keyword(Keyword::From)
                || self.check_keyword(Keyword::In)
                || self.check_keyword(Keyword::Into)
                || self.check_punct(Punct::Semicolon)
            {
                None
            } else {
                Some(self.consume_expression_until(&[
                    TokenKind::Keyword(Keyword::From),
                    TokenKind::Keyword(Keyword::In),
                    TokenKind::Keyword(Keyword::Into),
                    TokenKind::Punct(Punct::Semicolon),
                ])?)
            };
            return Ok((FetchDirection::Relative, count));
        }
        if self.eat_keyword(Keyword::Forward) {
            let count = if self.check_keyword(Keyword::From)
                || self.check_keyword(Keyword::In)
                || self.check_keyword(Keyword::Into)
                || self.check_punct(Punct::Semicolon)
            {
                None
            } else {
                Some(self.consume_expression_until(&[
                    TokenKind::Keyword(Keyword::From),
                    TokenKind::Keyword(Keyword::In),
                    TokenKind::Keyword(Keyword::Into),
                    TokenKind::Punct(Punct::Semicolon),
                ])?)
            };
            return Ok((FetchDirection::Forward, count));
        }
        if self.eat_keyword(Keyword::Backward) {
            let count = if self.check_keyword(Keyword::From)
                || self.check_keyword(Keyword::In)
                || self.check_keyword(Keyword::Into)
                || self.check_punct(Punct::Semicolon)
            {
                None
            } else {
                Some(self.consume_expression_until(&[
                    TokenKind::Keyword(Keyword::From),
                    TokenKind::Keyword(Keyword::In),
                    TokenKind::Keyword(Keyword::Into),
                    TokenKind::Punct(Punct::Semicolon),
                ])?)
            };
            return Ok((FetchDirection::Backward, count));
        }
        Ok((FetchDirection::Next, None))
    }

    fn parse_assignment_or_sql(&mut self) -> Result<Stmt, ParseError> {
        // Lookahead: identifier (maybe dotted/indexed) followed by `:=` or `=`
        // indicates assignment. Anything else is passed through to the SQL
        // engine.
        let snapshot = self.pos;
        if self.try_parse_assignment_lvalue().is_some()
            && (matches!(self.peek().kind, TokenKind::Punct(Punct::Assign))
                || (matches!(self.peek().kind, TokenKind::Punct(Punct::Equal))
                    && Self::lvalue_without_dot_or_bracket(&self.tokens[snapshot..self.pos])))
        {
            self.pos = snapshot;
            let lvalue = self.parse_lvalue()?;
            self.bump();
            let expr = self.consume_expression_until(&[TokenKind::Punct(Punct::Semicolon)])?;
            self.expect_punct(Punct::Semicolon)?;
            return Ok(Stmt::Assign {
                target: lvalue,
                expr,
            });
        }
        self.pos = snapshot;
        self.parse_embedded_sql_statement()
    }

    fn try_parse_assignment_lvalue(&mut self) -> Option<()> {
        if !matches!(self.peek().kind, TokenKind::Ident | TokenKind::QuotedIdent)
            && !matches!(self.peek().kind, TokenKind::Keyword(k) if keyword_can_be_identifier(k))
        {
            return None;
        }
        self.bump();
        loop {
            match &self.peek().kind {
                TokenKind::Punct(Punct::Dot) => {
                    self.bump();
                    if !matches!(
                        self.peek().kind,
                        TokenKind::Ident | TokenKind::QuotedIdent | TokenKind::Punct(Punct::Star)
                    ) && !matches!(
                        self.peek().kind,
                        TokenKind::Keyword(k) if keyword_can_be_identifier(k)
                    ) {
                        return None;
                    }
                    self.bump();
                }
                TokenKind::Punct(Punct::LBracket) => {
                    self.bump();
                    let mut depth = 1i32;
                    while depth > 0 {
                        match &self.peek().kind {
                            TokenKind::Punct(Punct::LBracket) => depth += 1,
                            TokenKind::Punct(Punct::RBracket) => depth -= 1,
                            TokenKind::Eof => return None,
                            _ => {}
                        }
                        self.bump();
                    }
                }
                _ => break,
            }
        }
        Some(())
    }

    fn lvalue_without_dot_or_bracket(tokens: &[Token]) -> bool {
        tokens.iter().all(|t| {
            !matches!(
                t.kind,
                TokenKind::Punct(Punct::Dot | Punct::LBracket | Punct::RBracket)
            )
        })
    }

    fn parse_lvalue(&mut self) -> Result<LValue, ParseError> {
        let root = self.parse_identifier()?;
        let mut path: Vec<String> = Vec::new();
        let mut indices: Vec<String> = Vec::new();
        let mut is_array = false;
        loop {
            if self.eat_punct(Punct::Dot) {
                let field = self.parse_identifier()?;
                path.push(field);
            } else if self.eat_punct(Punct::LBracket) {
                is_array = true;
                let idx = self.consume_expression_until(&[TokenKind::Punct(Punct::RBracket)])?;
                self.expect_punct(Punct::RBracket)?;
                indices.push(idx);
            } else {
                break;
            }
        }
        if is_array {
            Ok(LValue::ArrayElement { root, indices })
        } else if path.is_empty() {
            Ok(LValue::Variable(root))
        } else {
            Ok(LValue::Field { root, path })
        }
    }

    fn parse_embedded_sql_statement(&mut self) -> Result<Stmt, ParseError> {
        // Collect the SQL verbatim up to the trailing `;`, recognising an
        // embedded `INTO [STRICT] target[, ...]` clause. PL/pgSQL's `INTO`
        // binding form appears in two shapes:
        //
        //  * `SELECT … INTO target … FROM …` - for SELECT statements.
        //  * `INSERT / UPDATE / DELETE … RETURNING … INTO target` - the
        //    INTO lives past a RETURNING clause in mutating statements.
        //
        // We capture whichever shape applies; INSERT/UPDATE/DELETE's leading
        // `INTO target` is never consumed because that form belongs to SQL
        // itself, not the PL/pgSQL binding grammar.
        let start = self.pos;
        let first_keyword = match self.tokens[self.pos].kind {
            TokenKind::Keyword(k) => Some(k),
            _ => None,
        };
        let allow_into_before_from = matches!(first_keyword, Some(Keyword::Select));
        let mut depth = 0i32;
        let mut seen_returning = false;
        let mut into_span: Option<(usize, usize)> = None;
        while self.pos < self.tokens.len() {
            let kind = self.tokens[self.pos].kind.clone();
            match kind {
                TokenKind::Punct(Punct::LParen | Punct::LBracket) => {
                    depth += 1;
                    self.pos += 1;
                }
                TokenKind::Punct(Punct::RParen | Punct::RBracket) => {
                    depth -= 1;
                    self.pos += 1;
                }
                TokenKind::Punct(Punct::Semicolon) if depth == 0 => {
                    break;
                }
                TokenKind::Eof => break,
                TokenKind::Ident if depth == 0 => {
                    if self.tokens[self.pos]
                        .lexeme
                        .eq_ignore_ascii_case("returning")
                    {
                        seen_returning = true;
                    }
                    self.pos += 1;
                }
                TokenKind::Keyword(Keyword::Into)
                    if depth == 0
                        && into_span.is_none()
                        && (allow_into_before_from || seen_returning) =>
                {
                    let into_start = self.pos;
                    self.pos += 1;
                    let _ = self.eat_keyword(Keyword::Strict);
                    while matches!(self.peek().kind, TokenKind::Ident | TokenKind::QuotedIdent) {
                        self.pos += 1;
                        if !self.eat_punct(Punct::Comma) {
                            break;
                        }
                    }
                    into_span = Some((into_start, self.pos));
                }
                _ => self.pos += 1,
            }
        }
        let end = self.pos;
        if start == end {
            return Err(self.error("expected a statement"));
        }
        let (command, into) = if let Some((into_start, into_end)) = into_span {
            let mut command = String::new();
            if let Some(s) = self.slice_from_tokens(start, into_start) {
                command.push_str(&s);
            }
            if let Some(s) = self.slice_from_tokens(into_end, end) {
                if !command.is_empty() && !command.ends_with(' ') {
                    command.push(' ');
                }
                command.push_str(&s);
            }
            let strict_offset = usize::from(matches!(
                self.tokens[into_start + 1].kind,
                TokenKind::Keyword(Keyword::Strict)
            ));
            let mut targets = Vec::new();
            let mut i = into_start + 1 + strict_offset;
            while i < into_end {
                match &self.tokens[i].kind {
                    TokenKind::Ident | TokenKind::QuotedIdent => {
                        targets.push(self.tokens[i].lexeme.clone());
                    }
                    _ => {}
                }
                i += 1;
            }
            (
                command,
                Some(IntoTarget {
                    strict: strict_offset == 1,
                    targets,
                }),
            )
        } else {
            (self.slice_from_tokens(start, end).unwrap_or_default(), None)
        };
        self.expect_punct(Punct::Semicolon)?;
        Ok(Stmt::Sql {
            command: command.trim().to_owned(),
            into,
        })
    }

    fn slice_from_tokens(&self, from: usize, to: usize) -> Option<String> {
        if from >= to {
            return None;
        }
        let start_offset = self.tokens[from].offset;
        let end_offset = if to >= self.tokens.len() {
            self.src.len()
        } else {
            self.tokens[to].offset
        };
        Some(self.src[start_offset..end_offset].trim().to_owned())
    }

    fn consume_expression_until(&mut self, stops: &[TokenKind]) -> Result<String, ParseError> {
        let start = self.pos;
        let mut depth = 0i32;
        while self.pos < self.tokens.len() {
            let kind = self.tokens[self.pos].kind.clone();
            if depth == 0 && stops.iter().any(|s| token_kinds_match(s, &kind)) {
                break;
            }
            match kind {
                TokenKind::Punct(Punct::LParen | Punct::LBracket) => depth += 1,
                TokenKind::Punct(Punct::RParen | Punct::RBracket) => {
                    if depth == 0
                        && stops
                            .iter()
                            .any(|s| token_kinds_match(s, &self.tokens[self.pos].kind))
                    {
                        break;
                    }
                    depth -= 1;
                }
                TokenKind::Eof => break,
                _ => {}
            }
            self.pos += 1;
        }
        let end = self.pos;
        if start == end {
            return Err(self.error("expected expression"));
        }
        Ok(self.slice_from_tokens(start, end).unwrap_or_default())
    }
}

fn keyword_can_be_identifier(keyword: Keyword) -> bool {
    matches!(
        keyword,
        Keyword::Debug | Keyword::Forward | Keyword::Return | Keyword::Row
    )
}

fn token_kinds_match(a: &TokenKind, b: &TokenKind) -> bool {
    match (a, b) {
        (TokenKind::Keyword(x), TokenKind::Keyword(y)) => x == y,
        (TokenKind::Punct(x), TokenKind::Punct(y)) => x == y,
        (TokenKind::Eof, TokenKind::Eof) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_if_else() {
        let block = parse_block(
            "DECLARE x int := 2; y int := 0; BEGIN IF x = 1 THEN y := 10; ELSIF x = 2 THEN y := 20; ELSE y := 30; END IF; END;",
        )
        .unwrap();
        assert_eq!(block.declarations.len(), 2);
        assert!(matches!(block.body[0], Stmt::If { .. }));
    }

    #[test]
    fn parses_for_integer_range() {
        let block =
            parse_block("BEGIN FOR i IN 1..10 LOOP RAISE NOTICE '%', i; END LOOP; END;").unwrap();
        let Stmt::For { kind, .. } = &block.body[0] else {
            panic!("expected FOR");
        };
        match kind {
            ForLoopKind::Integer { variable, .. } => assert_eq!(variable, "i"),
            _ => panic!("expected integer FOR"),
        }
    }

    #[test]
    fn parses_return_variants() {
        let block =
            parse_block("BEGIN RETURN NEXT 1; RETURN QUERY SELECT 1; RETURN; END;").unwrap();
        assert!(matches!(block.body[0], Stmt::Return(ReturnKind::Next(_))));
        assert!(matches!(
            block.body[1],
            Stmt::Return(ReturnKind::QueryExpr(_))
        ));
        assert!(matches!(block.body[2], Stmt::Return(ReturnKind::Void)));
    }

    #[test]
    fn parses_exception_handler() {
        let block = parse_block(
            "BEGIN RAISE EXCEPTION 'oops'; EXCEPTION WHEN OTHERS THEN RAISE NOTICE 'caught'; END;",
        )
        .unwrap();
        assert_eq!(block.exception_handlers.len(), 1);
    }

    #[test]
    fn parses_assignment_and_select_into() {
        let block = parse_block("DECLARE x int; BEGIN x := 5; SELECT count(*) INTO x FROM t; END;")
            .unwrap();
        assert!(matches!(block.body[0], Stmt::Assign { .. }));
        assert!(matches!(block.body[1], Stmt::Sql { into: Some(_), .. }));
    }

    #[test]
    fn parses_constant_cursor_declaration() {
        let block =
            parse_block("DECLARE c CONSTANT CURSOR FOR SELECT 1; BEGIN NULL; END;").unwrap();
        let VarDecl::Cursor(cursor) = &block.declarations[0] else {
            panic!("expected cursor declaration");
        };
        assert!(cursor.is_constant);
        assert_eq!(cursor.name, "c");
    }

    #[test]
    fn parses_empty_array_type_suffix_in_declaration() {
        let block = parse_block("DECLARE x int[]; BEGIN NULL; END;").unwrap();
        let VarDecl::Scalar { type_ref, .. } = &block.declarations[0] else {
            panic!("expected scalar declaration");
        };
        assert!(matches!(type_ref, TypeRef::Named(name) if name == "int[]"));
    }

    #[test]
    fn parses_assignment_to_return_named_variable() {
        let block =
            parse_block("DECLARE return int := 1; BEGIN return := return + 1; RETURN return; END;")
                .unwrap();
        assert!(matches!(block.body[0], Stmt::Assign { .. }));
        assert!(matches!(block.body[1], Stmt::Return(ReturnKind::Expr(_))));
    }

    #[test]
    fn parses_pg_datatype_name_in_stacked_diagnostics() {
        let block = parse_block(
            "DECLARE x text; BEGIN EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS x = pg_datatype_name; END;",
        )
        .unwrap();
        assert_eq!(block.exception_handlers.len(), 1);
    }

    #[test]
    fn parses_execute_using_before_into() {
        let block = parse_block(
            "DECLARE x record; BEGIN EXECUTE 'select 1' USING 10, 20 INTO STRICT x; END;",
        )
        .unwrap();
        let Stmt::Execute { into, using, .. } = &block.body[0] else {
            panic!("expected EXECUTE");
        };
        assert!(into.as_ref().is_some_and(|target| target.strict));
        assert_eq!(using.len(), 2);
    }

    #[test]
    fn parses_variadic_for_loop_body() {
        let block = parse_block(
            "BEGIN FOR i IN array_lower($1,1)..array_upper($1,1) LOOP RAISE NOTICE '%', $1[i]; END LOOP; END;",
        )
        .unwrap();
        assert!(matches!(block.body[0], Stmt::For { .. }));
    }

    #[test]
    fn parses_insert_returning_into_target() {
        let block = parse_block(
            "DECLARE curtid tid; BEGIN INSERT INTO t_probe VALUES (1) RETURNING ctid INTO curtid; END;",
        )
        .unwrap();
        let Stmt::Sql { command, into } = &block.body[0] else {
            panic!("expected SQL statement");
        };
        assert!(
            command.to_ascii_lowercase().contains("returning ctid"),
            "command should retain RETURNING expression: {command}"
        );
        assert!(
            !command.to_ascii_lowercase().contains("into curtid"),
            "command should not retain PL/pgSQL INTO target: {command}"
        );
        let Some(target) = into else {
            panic!("expected INTO target");
        };
        assert!(!target.strict);
        assert_eq!(target.targets, vec!["curtid".to_string()]);
    }

    #[test]
    fn parses_decl_with_equal_default_and_array_subscripts() {
        let block = parse_block(
            "DECLARE aux numeric = $1[array_lower($1,1)]; BEGIN FOR i IN array_lower($1,1)+1..array_upper($1,1) LOOP IF $1[i] < aux THEN aux := $1[i]; END IF; END LOOP; RETURN aux; END;",
        )
        .unwrap();
        assert!(matches!(block.body[0], Stmt::For { .. }));
        assert!(matches!(block.body[1], Stmt::Return(ReturnKind::Expr(_))));
    }
}
