#![allow(
    clippy::assigning_clones,
    clippy::doc_markdown,
    clippy::items_after_statements,
    clippy::manual_let_else,
    clippy::match_same_arms,
    clippy::match_wildcard_for_single_variants,
    clippy::map_unwrap_or,
    clippy::missing_errors_doc,
    clippy::must_use_candidate,
    clippy::needless_pass_by_value,
    clippy::no_effect_underscore_binding,
    clippy::redundant_closure_for_method_calls,
    clippy::return_self_not_must_use,
    clippy::self_only_used_in_recursion,
    clippy::single_match_else,
    clippy::struct_excessive_bools,
    clippy::too_many_lines,
    clippy::unnecessary_wraps,
    clippy::uninlined_format_args,
    clippy::unused_self,
    clippy::wildcard_imports
)]

pub mod ast;
pub mod cypher_ast;
pub mod identifier;
pub mod keywords;
#[path = "lexer.rs"]
pub mod lexer;
pub mod parser_acl;
pub mod parser_async;
pub mod parser_backup;
pub mod parser_comment;
pub mod parser_copy;
pub mod parser_cypher;
pub mod parser_ddl;
pub mod parser_ddl_ext;
pub mod parser_dml;
mod parser_dml_rewrite;
pub mod parser_expr;
pub mod parser_func;
pub mod parser_lock;
pub mod parser_owned;
pub mod parser_security_label;
pub mod parser_select;
pub mod parser_session;
pub mod parser_tx;
pub mod parser_types;
pub mod span;
pub mod tokens;

pub use ast::{
    AlterRoleRenameStatement, AlterRoleStatement, AlterSystemAction, AlterSystemStatement,
    AlterTableAction, AlterTableStatement, AlterTriggerRenameStatement, BinaryOperator, ColumnDef,
    CommentStatement, CommentSubject, CopyDirection, CopyStatement, CreateEdgeLabelStatement,
    CreateExtensionStatement, CreateFunctionStatement, CreateIndexStatement,
    CreateNodeLabelStatement, CreateRoleStatement, CreateSchemaStatement, CreateSequenceStatement,
    CreateTableAsStatement, CreateTableStatement, CreateTriggerStatement, CreateViewStatement,
    CteDefinition, CypherCallClause, CypherClause, CypherCreateClause, CypherDeleteClause,
    CypherDirection, CypherForeachClause, CypherMatchClause, CypherMergeAction, CypherMergeClause,
    CypherNodePattern, CypherPathPattern, CypherRelPattern, CypherRemoveClause, CypherRemoveItem,
    CypherReturnClause, CypherReturnItem, CypherSetClause, CypherSetItem, CypherStatement,
    CypherUnion, CypherUnwindClause, CypherWithClause, DeleteStatement, DiscardStatement,
    DiscardTarget, DistinctKind, DropEdgeLabelStatement, DropExtensionStatement,
    DropFunctionStatement, DropIndexStatement, DropNodeLabelStatement, DropOwnedStatement,
    DropRoleStatement, DropSchemaStatement, DropSequenceStatement, DropTableStatement,
    DropTriggerStatement, DropViewStatement, Expr, FunctionParam, GrantStatement, GrantTarget,
    GroupByItem, GroupBySet, IndexMethod, InlineColumnReference, InsertStatement, JoinClause,
    JoinType as AstJoinType, Literal, LockStatement, MergeAction, MergeSource, MergeStatement,
    MergeWhenClause, ObjectName, OnConflict, OnConflictAction, OrderByItem, PartitionStrategy,
    PgLockMode, Privilege, ReassignOwnedStatement, ResetVariableStatement, RevokeStatement,
    RoleOption, SecurityLabelStatement, SecurityLabelSubject, SelectItem, SelectStatement,
    SetConstraintsStatement, SetOperationStatement, SetOperationType, SetVariableStatement,
    ShowVariableStatement, Statement, TableConstraint, TransactionControlStatement,
    TransactionMode, TriggerEvent, TriggerTiming, TruncateTableStatement, UnaryOperator,
    UpdateAssignment, UpdateStatement,
};
pub use span::Span;

use aiondb_core::{DbError, DbResult};
use std::sync::OnceLock;

use crate::{
    keywords::Keyword,
    tokens::{Token, TokenKind},
};

pub fn parse_sql(sql: &str) -> DbResult<Vec<Statement>> {
    // Fast path for the most common trivial case.
    if sql.is_empty() {
        return Ok(Vec::new());
    }
    let tokens = lex_and_validate(sql)?;
    Parser::with_source(tokens, sql.to_owned()).parse_statements()
}

pub fn parse_prepared_statement(sql: &str) -> DbResult<Statement> {
    let tokens = lex_and_validate(sql)?;
    Parser::with_source(tokens, sql.to_owned()).parse_single_statement()
}

pub fn parse_expression(sql: &str) -> DbResult<Expr> {
    let tokens = lex_and_validate(sql)?;
    Parser::new(tokens).parse_single_expression()
}

/// Maximum parenthesis / bracket nesting depth allowed in a token stream.
/// Checked in a single O(n) pass **before** any recursive parsing begins.
/// The default balances stack safety with real sqllogictest workloads.
/// Debug builds keep a tighter ceiling because recursive descent uses many
/// stack frames per nesting level.
const DEFAULT_MAX_NESTING_DEPTH: usize = if cfg!(debug_assertions) { 32 } else { 48 };
const MIN_MAX_NESTING_DEPTH: usize = 8;
/// Prevent debug builds from opting into stack-unsafe nesting levels.
const HARD_MAX_NESTING_DEPTH: usize = 128;

/// Maximum number of tokens allowed in a single SQL statement.
/// Prevents combinatorial blowup in the planner/optimizer for
/// pathologically wide queries.
const MAX_TOKEN_COUNT: usize = 500_000;

/// Maximum SQL input size accepted by parser entrypoints.
/// This avoids lexing unbounded inputs into large transient allocations.
const MAX_SQL_INPUT_BYTES: usize = 64 * 1024 * 1024;

/// Maximum length of any single identifier token.
const MAX_IDENTIFIER_TOKEN_LEN: usize = 1024;

/// Hard limits for `VALUES (...)` parsing to prevent unbounded AST growth.
pub(crate) const MAX_VALUES_ROWS: usize = 100_000;
pub(crate) const MAX_VALUES_COLUMNS: usize = 4_096;
pub(crate) const MAX_VALUES_CELLS: usize = 2_000_000;

/// Per-array element ceiling for `ARRAY[...]` literals.
pub(crate) const MAX_ARRAY_ELEMENTS: usize = 100_000;

fn max_nesting_depth() -> usize {
    static MAX_DEPTH: OnceLock<usize> = OnceLock::new();
    *MAX_DEPTH.get_or_init(|| {
        std::env::var("AIONDB_PARSER_MAX_NESTING_DEPTH")
            .ok()
            .and_then(|raw| raw.parse::<usize>().ok())
            .map_or(DEFAULT_MAX_NESTING_DEPTH, |value| {
                value.clamp(MIN_MAX_NESTING_DEPTH, HARD_MAX_NESTING_DEPTH)
            })
    })
}

/// Lex SQL text into tokens, then validate the token stream with absolute
/// guarantees before any recursive parsing can begin.
///
/// Runs in O(n) time and O(1) extra space.  Rejects
/// inputs that would later cause stack overflows, unbounded memory use,
/// or other resource exhaustion during recursive descent parsing.
///
/// The checks are intentionally simple `if` conditions on integer
/// counters - they cannot be circumvented by any input pattern.
fn lex_and_validate(sql: &str) -> DbResult<Vec<Token>> {
    if sql.len() > MAX_SQL_INPUT_BYTES {
        return Err(DbError::program_limit(
            "SQL input exceeds maximum allowed length",
        ));
    }
    let tokens = lexer::lex_sql(sql)?;
    validate_token_stream(&tokens)?;
    Ok(tokens)
}

/// Single-pass O(n) validation of the token stream.
///
/// Guarantees enforced (each is a simple integer comparison):
/// 1. Total token count ≤ `MAX_TOKEN_COUNT`
/// 2. Parenthesis nesting depth ≤ configured max nesting depth
/// 3. Bracket nesting depth ≤ configured max nesting depth
/// 4. Every identifier length ≤ `MAX_IDENTIFIER_TOKEN_LEN`
/// 5. Parentheses and brackets are balanced (no underflow)
fn validate_token_stream(tokens: &[Token]) -> DbResult<()> {
    if tokens.len() > MAX_TOKEN_COUNT {
        return Err(DbError::program_limit(
            "SQL statement exceeds maximum token count",
        ));
    }

    let mut paren_depth: u32 = 0;
    let mut bracket_depth: u32 = 0;

    let max_depth = u32::try_from(max_nesting_depth()).map_err(|_| {
        DbError::program_limit("configured maximum nesting depth exceeds u32 range")
    })?;

    for token in tokens {
        match &token.kind {
            TokenKind::LParen => {
                paren_depth += 1;
                if paren_depth > max_depth {
                    return Err(DbError::program_limit(
                        "parenthesis nesting depth exceeds maximum",
                    ));
                }
            }
            TokenKind::RParen => {
                if paren_depth == 0 {
                    return Err(DbError::syntax_error("unmatched closing parenthesis"));
                }
                paren_depth -= 1;
            }
            TokenKind::LBracket => {
                bracket_depth += 1;
                if bracket_depth > max_depth {
                    return Err(DbError::program_limit(
                        "bracket nesting depth exceeds maximum",
                    ));
                }
            }
            TokenKind::RBracket => {
                if bracket_depth == 0 {
                    return Err(DbError::syntax_error("unmatched closing bracket"));
                }
                bracket_depth -= 1;
            }
            TokenKind::Identifier(name) if name.len() > MAX_IDENTIFIER_TOKEN_LEN => {
                return Err(DbError::program_limit("identifier exceeds maximum length"));
            }
            TokenKind::String(s) if s.len() > 16 * 1024 * 1024 => {
                return Err(DbError::program_limit(
                    "string literal exceeds maximum length",
                ));
            }
            _ => {}
        }
    }

    let eof_pos = tokens.last().map_or(1, |token| token.span.start + 1);
    if paren_depth != 0 {
        return Err(DbError::syntax_error("unmatched opening parenthesis").with_position(eof_pos));
    }
    if bracket_depth != 0 {
        return Err(DbError::syntax_error("unmatched opening bracket").with_position(eof_pos));
    }

    Ok(())
}

const MAX_EXPR_DEPTH: usize = 128;
/// Maximum subquery nesting depth.  Subquery parsing traverses many
/// stack frames per level; keeping this bounded prevents stack overflow
/// in debug builds with the default 8 MiB thread stack.
const MAX_SUBQUERY_DEPTH: usize = 15;
// Runtime paren depth guard is redundant with pre-parse validation,
// but kept as defense-in-depth.

pub(crate) struct Parser {
    pub(crate) tokens: Vec<Token>,
    pub(crate) index: usize,
    expr_depth: usize,
    /// Counter for generating unique synthetic CTE names for FROM-subqueries.
    subquery_cte_counter: usize,
    /// Counter for generating unique synthetic CTE names for FROM-clause SRFs.
    from_function_cte_counter: usize,
    /// Guard against infinite recursion in set operations.
    set_op_depth: usize,
    /// Guard against infinite recursion in subqueries.
    subquery_depth: usize,
    /// Guard against stack overflow from deeply nested parenthesized
    /// sub-expressions (`(((...)))`) which each traverse the full
    /// precedence chain of stack frames.
    paren_depth: usize,
    /// True when parsing inside a Cypher statement (enables Cypher-specific syntax).
    pub(crate) cypher_mode: bool,
    /// Guard against stack overflow from deeply nested Cypher UNION chains.
    pub(crate) cypher_depth: usize,
    /// Original source text for span-based lookups.
    pub(crate) source: String,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Self::with_source(tokens, String::new())
    }

    fn with_source(mut tokens: Vec<Token>, source: String) -> Self {
        // Defensive invariant: parser helpers assume an EOF sentinel exists.
        if !matches!(tokens.last().map(|token| &token.kind), Some(TokenKind::Eof)) {
            let eof_pos = tokens.last().map_or(0, |token| token.span.end);
            tokens.push(Token::eof(eof_pos));
        }
        Self {
            tokens,
            index: 0,
            expr_depth: 0,
            subquery_cte_counter: 0,
            from_function_cte_counter: 0,
            set_op_depth: 0,
            subquery_depth: 0,
            paren_depth: 0,
            cypher_mode: false,
            cypher_depth: 0,
            source,
        }
    }

    pub(crate) fn pg_compat_utility_statement(
        &self,
        tag: impl Into<String>,
        notice: Option<String>,
        raw_sql: String,
        span: Span,
    ) -> Statement {
        Statement::PgCompatUtility(crate::ast::CompatTaggedNoticeStatement {
            tag: tag.into(),
            notice,
            raw_sql,
            span,
        })
    }

    pub(crate) fn compat_tagged_statement(
        &self,
        tag: impl Into<String>,
        raw_sql: String,
        span: Span,
    ) -> Statement {
        Statement::CompatTagged(crate::ast::CompatTaggedStatement {
            tag: tag.into(),
            raw_sql,
            span,
        })
    }

    /// Heuristic: does `WITH <next_token>` look like a Cypher WITH or a SQL CTE?
    pub(crate) fn looks_like_cypher_with(&self) -> bool {
        let after_with = self.index + 1;
        if after_with >= self.tokens.len() {
            return false;
        }
        let next = &self.tokens[after_with].kind;
        match next {
            TokenKind::Keyword(Keyword::Distinct) | TokenKind::Star => return true,
            TokenKind::Keyword(Keyword::Recursive) => return false,
            TokenKind::Integer(_)
            | TokenKind::NumericLiteral(_)
            | TokenKind::String(_)
            | TokenKind::Keyword(Keyword::True | Keyword::False | Keyword::Null) => return true,
            TokenKind::LBracket | TokenKind::LBrace => return true,
            TokenKind::LParen => return false,
            // Temporal keywords (date, time, etc.) followed by ( or . are Cypher
            TokenKind::Keyword(
                Keyword::Date | Keyword::Time | Keyword::Timestamp | Keyword::Interval,
            ) => {
                let after_kw = after_with + 1;
                if after_kw < self.tokens.len()
                    && matches!(
                        self.tokens[after_kw].kind,
                        TokenKind::LParen | TokenKind::Dot
                    )
                {
                    return true;
                }
                return false;
            }
            TokenKind::Identifier(_) => {}
            TokenKind::Keyword(kw) if kw.is_unreserved() => {}
            _ => return false,
        }
        // Check: identifier followed by AS (not followed by `(`) -> Cypher
        let after_ident = after_with + 1;
        if after_ident >= self.tokens.len() {
            return false;
        }
        // `WITH ident(...)` could be either a Cypher function-call projection
        // (`WITH localdatetime({...}) AS x`) or a SQL CTE column list
        // (`WITH cte (a, b) AS (SELECT ...)`). Distinguish by looking past
        // the matching `)` for `AS (` (SQL) vs `AS ident` (Cypher).
        if matches!(self.tokens[after_ident].kind, TokenKind::LParen) {
            let mut depth = 1usize;
            let mut i = after_ident + 1;
            while i < self.tokens.len() && depth > 0 {
                match self.tokens[i].kind {
                    TokenKind::LParen => depth += 1,
                    TokenKind::RParen => depth -= 1,
                    TokenKind::Eof => break,
                    _ => {}
                }
                i += 1;
            }
            if i < self.tokens.len() {
                match &self.tokens[i].kind {
                    TokenKind::Keyword(Keyword::As) if i + 1 < self.tokens.len() => {
                        if matches!(self.tokens[i + 1].kind, TokenKind::LParen) {
                            return false;
                        }
                        return true;
                    }
                    TokenKind::Comma
                    | TokenKind::Keyword(
                        Keyword::Where
                        | Keyword::Order
                        | Keyword::Skip
                        | Keyword::Limit
                        | Keyword::Return
                        | Keyword::Match
                        | Keyword::Optional
                        | Keyword::Unwind
                        | Keyword::With
                        | Keyword::Create
                        | Keyword::Merge
                        | Keyword::Delete
                        | Keyword::Detach
                        | Keyword::Set
                        | Keyword::Remove
                        | Keyword::Call,
                    ) => {
                        return true;
                    }
                    _ => {}
                }
            }
        }
        match &self.tokens[after_ident].kind {
            TokenKind::Keyword(Keyword::As) => {
                let after_as = after_ident + 1;
                if after_as >= self.tokens.len() {
                    return true;
                }
                // SQL CTEs may use:
                //   WITH cte AS (SELECT ...)
                //   WITH cte AS MATERIALIZED (SELECT ...)
                //   WITH cte AS NOT MATERIALIZED (SELECT ...)
                // Do not route these forms to the Cypher parser.
                if matches!(self.tokens[after_as].kind, TokenKind::LParen) {
                    return false;
                }
                if matches!(
                    self.tokens[after_as].kind,
                    TokenKind::Keyword(Keyword::Materialized)
                ) {
                    let after_materialized = after_as + 1;
                    return !(after_materialized < self.tokens.len()
                        && matches!(self.tokens[after_materialized].kind, TokenKind::LParen));
                }
                if matches!(self.tokens[after_as].kind, TokenKind::Keyword(Keyword::Not)) {
                    let after_not = after_as + 1;
                    if after_not < self.tokens.len()
                        && matches!(
                            self.tokens[after_not].kind,
                            TokenKind::Keyword(Keyword::Materialized)
                        )
                    {
                        let after_materialized = after_not + 1;
                        return !(after_materialized < self.tokens.len()
                            && matches!(self.tokens[after_materialized].kind, TokenKind::LParen));
                    }
                }
                true
            }
            TokenKind::LParen => false,
            _ => true,
        }
    }

    /// Generate a unique synthetic CTE name for an inline subquery in FROM.
    pub(crate) fn next_subquery_cte_name(&mut self) -> String {
        self.subquery_cte_counter += 1;
        format!("__subquery_{}", self.subquery_cte_counter)
    }

    /// Generate a unique synthetic CTE name for a FROM-clause SRF.
    pub(crate) fn next_from_function_cte_name(&mut self) -> String {
        self.from_function_cte_counter += 1;
        format!("__from_function_{}", self.from_function_cte_counter)
    }

    pub(crate) fn enter_expr_recursion(&mut self) -> DbResult<()> {
        self.expr_depth += 1;
        if self.expr_depth > MAX_EXPR_DEPTH {
            Err(
                DbError::syntax_error("expression nesting depth exceeded limit")
                    .with_client_detail(format!("maximum depth is {MAX_EXPR_DEPTH}")),
            )
        } else {
            Ok(())
        }
    }

    pub(crate) fn leave_expr_recursion(&mut self) {
        self.expr_depth = self.expr_depth.saturating_sub(1);
    }

    /// Guard against stack overflow from deeply nested parenthesized
    /// sub-expressions.  Must be called when consuming `(` in
    /// `parse_primary_expr` before recursing into `parse_expr`.
    pub(crate) fn enter_paren_recursion(&mut self) -> DbResult<()> {
        self.paren_depth += 1;
        let max_paren_depth = max_nesting_depth();
        if self.paren_depth > max_paren_depth {
            Err(
                DbError::syntax_error("parenthesized expression nesting depth exceeded limit")
                    .with_client_detail(format!("maximum depth is {max_paren_depth}")),
            )
        } else {
            Ok(())
        }
    }

    pub(crate) fn leave_paren_recursion(&mut self) {
        self.paren_depth = self.paren_depth.saturating_sub(1);
    }

    pub(crate) fn enter_subquery_recursion(&mut self) -> DbResult<()> {
        self.subquery_depth += 1;
        if self.subquery_depth > MAX_SUBQUERY_DEPTH {
            Err(
                DbError::syntax_error("subquery nesting depth exceeded limit")
                    .with_client_detail(format!("maximum depth is {MAX_SUBQUERY_DEPTH}")),
            )
        } else {
            Ok(())
        }
    }

    pub(crate) fn leave_subquery_recursion(&mut self) {
        self.subquery_depth = self.subquery_depth.saturating_sub(1);
    }

    fn parse_statements(mut self) -> DbResult<Vec<Statement>> {
        let mut statements = Vec::new();

        while !self.is_eof() {
            while self.consume_kind(&TokenKind::Semicolon) {}
            if self.is_eof() {
                break;
            }

            let statement = self.parse_statement()?;
            statements.push(statement);

            if self.consume_kind(&TokenKind::Semicolon) {
                while self.consume_kind(&TokenKind::Semicolon) {}
                continue;
            }

            if !self.is_eof() {
                return Err(self.unexpected_token("';' or end of input"));
            }
        }

        Ok(statements)
    }

    fn parse_single_statement(self) -> DbResult<Statement> {
        let mut statements = self.parse_statements()?;
        match statements.len() {
            0 => Err(DbError::syntax_error("expected a statement").with_position(1)),
            1 => Ok(statements.remove(0)),
            _ => Err(DbError::syntax_error(
                "prepared statements must contain exactly one statement",
            )),
        }
    }

    fn parse_single_expression(mut self) -> DbResult<Expr> {
        let expr = self.parse_expr()?;
        if !self.is_eof() {
            if self.current().kind == TokenKind::Keyword(Keyword::Within) {
                return self.unsupported_feature(
                    self.current().span,
                    "WITHIN GROUP ordered-set aggregates are not supported",
                );
            }
            return Err(self.unexpected_token("end of input"));
        }
        Ok(expr)
    }

    fn parse_prepare_statement_stub(&mut self) -> DbResult<Statement> {
        let start = self.advance();
        let mut span = start.span;
        self.skip_to_semicolon(&mut span);
        Ok(Statement::PrepareStmt { span })
    }

    fn parse_execute_statement_stub(&mut self) -> DbResult<Statement> {
        let start = self.advance();
        let mut span = start.span;
        self.skip_to_semicolon(&mut span);
        Ok(Statement::ExecuteStmt { span })
    }

    fn parse_deallocate_statement_stub(&mut self) -> DbResult<Statement> {
        let start = self.advance();
        let mut span = start.span;
        self.skip_to_semicolon(&mut span);
        Ok(Statement::DeallocateStmt { span })
    }

    fn parse_do_statement_stub(&mut self) -> DbResult<Statement> {
        let start = self.advance();
        let mut span = start.span;
        self.skip_to_semicolon(&mut span);
        Ok(Statement::DoStmt { span })
    }

    fn parse_declare_statement_stub(&mut self) -> DbResult<Statement> {
        let start = self.advance();
        let mut span = start.span;
        self.skip_to_semicolon(&mut span);
        Ok(Statement::DeclareStmt { span })
    }

    fn parse_fetch_statement_stub(&mut self) -> DbResult<Statement> {
        let start = self.advance();
        let mut span = start.span;
        self.skip_to_semicolon(&mut span);
        Ok(Statement::FetchStmt { span })
    }

    fn parse_move_statement_stub(&mut self) -> DbResult<Statement> {
        let start = self.advance();
        let mut span = start.span;
        self.skip_to_semicolon(&mut span);
        Ok(Statement::MoveStmt { span })
    }

    fn parse_close_statement_stub(&mut self) -> DbResult<Statement> {
        let start = self.advance();
        let mut span = start.span;
        self.skip_to_semicolon(&mut span);
        Ok(Statement::CloseStmt { span })
    }

    fn parse_statement(&mut self) -> DbResult<Statement> {
        if let Some(explain_token) = self.consume_keyword(Keyword::Explain) {
            let mut analyze = self.consume_keyword(Keyword::Analyze).is_some();
            // Handle EXPLAIN (options) form: skip parenthesized options
            if !analyze && self.consume_kind(&TokenKind::LParen) {
                // Parse options like (ANALYZE, VERBOSE, COSTS OFF, BUFFERS, FORMAT TEXT)
                loop {
                    if let TokenKind::Keyword(kw) = &self.current().kind {
                        if *kw == Keyword::Analyze {
                            analyze = true;
                        }
                    }
                    self.advance();
                    // Skip optional value: ON/OFF/TRUE/FALSE, keyword values (TEXT, JSON, YAML),
                    // integer values, or identifiers
                    if matches!(
                        self.current().kind,
                        TokenKind::Keyword(
                            Keyword::On
                                | Keyword::True
                                | Keyword::False
                                | Keyword::Text
                                | Keyword::Jsonb
                        ) | TokenKind::Integer(_)
                    ) || matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("off") || s.eq_ignore_ascii_case("true") || s.eq_ignore_ascii_case("false") || s.eq_ignore_ascii_case("json") || s.eq_ignore_ascii_case("yaml") || s.eq_ignore_ascii_case("xml"))
                        || matches!(&self.current().kind, TokenKind::Keyword(kw) if kw.is_unreserved())
                    {
                        self.advance();
                    }
                    if !self.consume_kind(&TokenKind::Comma) {
                        break;
                    }
                }
                self.expect_token(&TokenKind::RParen)?;
            }
            let inner = self.parse_statement()?;
            let span = explain_token.span.merge(inner.span());
            return Ok(Statement::Explain {
                analyze,
                statement: Box::new(inner),
                span,
            });
        }

        // Cypher: keywords that unambiguously start a Cypher statement
        if matches!(
            self.current().kind,
            TokenKind::Keyword(
                Keyword::Match
                    | Keyword::Optional
                    | Keyword::Return
                    | Keyword::Unwind
                    | Keyword::Remove
                    | Keyword::Detach
                    | Keyword::Foreach
                    | Keyword::Call
            )
        ) {
            return Ok(Statement::Cypher(self.parse_cypher_statement()?));
        }
        // Cypher CREATE: CREATE followed by `(` (not TABLE, INDEX, etc.)
        if matches!(self.current().kind, TokenKind::Keyword(Keyword::Create))
            && self.index + 1 < self.tokens.len()
            && matches!(self.tokens[self.index + 1].kind, TokenKind::LParen)
        {
            return Ok(Statement::Cypher(self.parse_cypher_statement()?));
        }
        // Cypher WITH: distinguish from SQL CTE
        if matches!(self.current().kind, TokenKind::Keyword(Keyword::With))
            && self.looks_like_cypher_with()
        {
            return Ok(Statement::Cypher(self.parse_cypher_statement()?));
        }
        // Cypher MERGE: MERGE followed by `(` (not INTO)
        if matches!(self.current().kind, TokenKind::Keyword(Keyword::Merge))
            && self.index + 1 < self.tokens.len()
            && matches!(self.tokens[self.index + 1].kind, TokenKind::LParen)
        {
            return Ok(Statement::Cypher(self.parse_cypher_statement()?));
        }

        let left = match &self.current().kind {
            TokenKind::Keyword(
                Keyword::Begin
                | Keyword::Commit
                | Keyword::Rollback
                | Keyword::Start
                | Keyword::Savepoint
                | Keyword::Release,
            ) => return self.parse_transaction_statement(),
            TokenKind::Keyword(Keyword::Analyze) => return self.parse_analyze_statement(),
            TokenKind::Keyword(Keyword::Backup) => return self.parse_backup_statement(),
            TokenKind::Keyword(Keyword::Vacuum) => return self.parse_vacuum_statement(),
            TokenKind::Keyword(Keyword::Alter) => return self.parse_alter_statement(),
            TokenKind::Keyword(Keyword::Copy) => return self.parse_copy_statement(),
            TokenKind::Keyword(Keyword::Create) => return self.parse_create_statement(),
            TokenKind::Keyword(Keyword::Truncate) => return self.parse_truncate_statement(),
            TokenKind::Keyword(Keyword::Drop) => return self.parse_drop_statement(),
            TokenKind::Keyword(Keyword::Grant) => return self.parse_grant_statement(),
            TokenKind::Keyword(Keyword::Revoke) => return self.parse_revoke_statement(),
            TokenKind::Keyword(Keyword::Restore) => return self.parse_restore_statement(),
            TokenKind::Keyword(Keyword::Comment) => {
                return self.parse_comment_on_statement();
            }
            TokenKind::Keyword(Keyword::Reindex) => {
                let mut span = self.current().span;
                self.advance();
                let reindex_notice = self.detect_reindex_verbose_notice();
                self.skip_to_semicolon(&mut span);
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                return Ok(Statement::Reindex(crate::ast::ReindexStatement {
                    notice: reindex_notice,
                    raw_sql,
                    span,
                }));
            }
            TokenKind::Keyword(Keyword::Lock) => {
                return self.parse_lock_statement();
            }
            TokenKind::Keyword(Keyword::Discard) => {
                let start = self.advance();
                let mut span = start.span;
                let target = match &self.current().kind {
                    TokenKind::Keyword(Keyword::All) => {
                        span = span.merge(self.advance().span);
                        crate::ast::DiscardTarget::All
                    }
                    TokenKind::Identifier(s) if s.eq_ignore_ascii_case("plans") => {
                        span = span.merge(self.advance().span);
                        crate::ast::DiscardTarget::Plans
                    }
                    TokenKind::Identifier(s) if s.eq_ignore_ascii_case("sequences") => {
                        span = span.merge(self.advance().span);
                        crate::ast::DiscardTarget::Sequences
                    }
                    TokenKind::Identifier(s)
                        if s.eq_ignore_ascii_case("temp")
                            || s.eq_ignore_ascii_case("temporary") =>
                    {
                        span = span.merge(self.advance().span);
                        crate::ast::DiscardTarget::Temporary
                    }
                    TokenKind::Keyword(Keyword::Temp | Keyword::Temporary) => {
                        span = span.merge(self.advance().span);
                        crate::ast::DiscardTarget::Temporary
                    }
                    _ => {
                        return self.syntax_error_current(
                            "expected ALL, PLANS, SEQUENCES, or TEMP after DISCARD",
                        );
                    }
                };
                self.skip_to_semicolon(&mut span);
                return Ok(Statement::Discard(crate::ast::DiscardStatement {
                    target,
                    span,
                }));
            }
            TokenKind::Keyword(Keyword::Refresh | Keyword::Concurrently) => {
                let name = if let TokenKind::Keyword(kw) = &self.current().kind {
                    kw.name().to_uppercase()
                } else {
                    return self.syntax_error_current("unexpected statement");
                };
                let mut span = self.current().span;
                self.advance();
                self.skip_to_semicolon(&mut span);
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                return Ok(self.pg_compat_utility_statement(name, None, raw_sql, span));
            }
            TokenKind::Keyword(Keyword::Execute) => {
                return self.parse_execute_statement_stub();
            }
            TokenKind::Keyword(Keyword::Do) => {
                return self.parse_do_statement_stub();
            }
            TokenKind::Keyword(Keyword::Fetch) => {
                return self.parse_fetch_statement_stub();
            }
            TokenKind::Keyword(Keyword::End) => {
                let token = self.advance();
                // END is a synonym for COMMIT; skip optional WORK/TRANSACTION
                let _ = self.consume_keyword(Keyword::Work);
                let _ = self.consume_keyword(Keyword::Transaction);
                if self.consume_keyword(Keyword::And).is_some() {
                    let _ = self.consume_keyword(Keyword::No);
                    let _ = self.consume_keyword(Keyword::Chain);
                }
                return Ok(Statement::Commit { span: token.span });
            }
            // Identifiers that are PG synonyms for transaction commands
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("END") => {
                let token = self.advance();
                // END is a synonym for COMMIT; skip optional WORK/TRANSACTION
                let _ = self.consume_keyword(Keyword::Work);
                let _ = self.consume_keyword(Keyword::Transaction);
                if self.consume_keyword(Keyword::And).is_some() {
                    let _ = self.consume_keyword(Keyword::No);
                    let _ = self.consume_keyword(Keyword::Chain);
                }
                return Ok(Statement::Commit { span: token.span });
            }
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("ABORT") => {
                let token = self.advance();
                // ABORT is a synonym for ROLLBACK; skip optional WORK/TRANSACTION
                let _ = self.consume_keyword(Keyword::Work);
                let _ = self.consume_keyword(Keyword::Transaction);
                return Ok(Statement::Rollback { span: token.span });
            }
            // CHECKPOINT - a real admin command that flushes the WAL.
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("CHECKPOINT") => {
                let mut span = self.current().span;
                self.advance();
                self.skip_to_semicolon(&mut span);
                return Ok(Statement::Checkpoint { span });
            }
            // REASSIGN OWNED BY src[, ...] TO target - first-class AST node.
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("REASSIGN") => {
                return self.parse_reassign_owned_statement();
            }
            // SECURITY LABEL ... IS {label|NULL} - first-class AST node.
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("SECURITY") => {
                return self.parse_security_label_statement();
            }
            // PREPARE TRANSACTION 'gid' - first-class AST node.
            TokenKind::Identifier(ref s)
                if s.eq_ignore_ascii_case("PREPARE")
                    && self.index + 1 < self.tokens.len()
                    && (matches!(
                        &self.tokens[self.index + 1].kind,
                        TokenKind::Keyword(Keyword::Transaction)
                    ) || matches!(
                        &self.tokens[self.index + 1].kind,
                        TokenKind::Identifier(n) if n.eq_ignore_ascii_case("transaction")
                    )) =>
            {
                return self.parse_prepare_transaction_statement();
            }
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("PREPARE") => {
                return self.parse_prepare_statement_stub();
            }
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("DEALLOCATE") => {
                return self.parse_deallocate_statement_stub();
            }
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("DECLARE") => {
                return self.parse_declare_statement_stub();
            }
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("MOVE") => {
                return self.parse_move_statement_stub();
            }
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("CLOSE") => {
                return self.parse_close_statement_stub();
            }
            // Unsupported statements that start with an identifier
            TokenKind::Identifier(ref s)
                if s.eq_ignore_ascii_case("CLUSTER")
                    || s.eq_ignore_ascii_case("IMPORT")
                    || s.eq_ignore_ascii_case("SAVEPOINT")
                    || s.eq_ignore_ascii_case("RELEASE") =>
            {
                let name = if let TokenKind::Identifier(s) = &self.current().kind {
                    s.to_ascii_uppercase()
                } else {
                    return self.syntax_error_current("unexpected statement");
                };
                let mut span = self.current().span;
                self.advance();
                self.skip_to_semicolon(&mut span);
                let raw_sql = self
                    .source
                    .get(span.start..span.end)
                    .unwrap_or_default()
                    .to_owned();
                return Ok(self.pg_compat_utility_statement(name, None, raw_sql, span));
            }
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("LISTEN") => {
                return self.parse_listen_statement();
            }
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("UNLISTEN") => {
                return self.parse_unlisten_statement();
            }
            TokenKind::Identifier(ref s) if s.eq_ignore_ascii_case("NOTIFY") => {
                return self.parse_notify_statement();
            }
            TokenKind::Keyword(Keyword::Set) => return self.parse_set_statement(),
            TokenKind::Keyword(Keyword::Load) => {
                let token = self.advance();
                let mut span = token.span;
                let file = if let TokenKind::String(s) = &self.current().kind {
                    let value = s.clone();
                    span = span.merge(self.advance().span);
                    Some(value)
                } else {
                    None
                };
                self.skip_to_semicolon(&mut span);
                return Ok(Statement::Load { file, span });
            }
            TokenKind::Keyword(Keyword::Show) => return self.parse_show_statement(),
            TokenKind::Keyword(Keyword::Reset) => return self.parse_reset_statement(),
            TokenKind::Keyword(Keyword::Delete) => return self.parse_delete_statement(),
            // Cypher: MATCH starts a Cypher query
            TokenKind::Keyword(Keyword::Match) => {
                return Ok(Statement::Cypher(self.parse_cypher_statement()?));
            }
            // Cypher: OPTIONAL MATCH starts a Cypher query
            TokenKind::Keyword(Keyword::Optional) => {
                // Lookahead for OPTIONAL MATCH
                if self.index + 1 < self.tokens.len()
                    && matches!(
                        self.tokens[self.index + 1].kind,
                        TokenKind::Keyword(Keyword::Match)
                    )
                {
                    return Ok(Statement::Cypher(self.parse_cypher_statement()?));
                }
                return self.syntax_error_current("unexpected statement");
            }
            TokenKind::Keyword(Keyword::Insert) => return self.parse_insert_statement(),
            // Cypher: CALL starts a Cypher procedure call
            TokenKind::Keyword(Keyword::Call) => {
                return Ok(Statement::Cypher(self.parse_cypher_statement()?));
            }
            TokenKind::Keyword(Keyword::Merge) => {
                // Check if this is a Cypher MERGE (followed by '(' for node pattern)
                // vs SQL MERGE INTO.
                if self.index + 1 < self.tokens.len()
                    && matches!(self.tokens[self.index + 1].kind, TokenKind::LParen)
                {
                    return Ok(Statement::Cypher(self.parse_cypher_statement()?));
                }
                return self.parse_merge_statement();
            }
            TokenKind::Keyword(Keyword::Select) => self.parse_select_statement()?,
            TokenKind::Keyword(Keyword::Values) => {
                let values_stmt = self.parse_values_as_statement()?;
                // Allow trailing UNION/INTERSECT/EXCEPT after VALUES
                self.maybe_parse_set_operation(values_stmt)?
            }
            TokenKind::Keyword(Keyword::Table) => return self.parse_table_shorthand(),
            TokenKind::Keyword(Keyword::With) => self.parse_with_statement()?,
            TokenKind::Keyword(Keyword::Update) => return self.parse_update_statement(),
            // Parenthesized subquery for set operations: (SELECT ...) UNION ALL (SELECT ...)
            TokenKind::LParen => {
                self.advance(); // consume '('
                                // Handle nested parens: ((SELECT ...)), (((SELECT ...)))
                let mut extra_parens = 0u32;
                while matches!(self.current().kind, TokenKind::LParen) {
                    self.advance();
                    extra_parens += 1;
                }
                let inner = if matches!(self.current().kind, TokenKind::Keyword(Keyword::Select)) {
                    let sel = self.parse_select_statement()?;
                    // Handle set operations inside parens: (SELECT ... UNION SELECT ...)
                    self.maybe_parse_set_operation(sel)?
                } else if matches!(self.current().kind, TokenKind::Keyword(Keyword::Values)) {
                    let vs = self.parse_values_as_statement()?;
                    self.maybe_parse_set_operation(vs)?
                } else if matches!(self.current().kind, TokenKind::Keyword(Keyword::With)) {
                    self.parse_with_statement()?
                } else {
                    self.parse_select_bare()?
                };
                // Consume closing parens for any extra nesting
                for _ in 0..extra_parens {
                    let _ = self.consume_kind(&TokenKind::RParen);
                }
                let _ = self.consume_kind(&TokenKind::RParen);
                inner
            }
            TokenKind::Eof => return self.syntax_error_current("expected a statement"),
            // Remaining keywords that start PG-compatible statements we
            // do not implement.
            TokenKind::Keyword(
                Keyword::Sequence
                | Keyword::Aggregate
                | Keyword::Domain
                | Keyword::Operator
                | Keyword::For,
            ) => {
                let name = if let TokenKind::Keyword(kw) = &self.current().kind {
                    kw.name().to_uppercase()
                } else {
                    return self.syntax_error_current("unexpected statement");
                };
                let span = self.current().span;
                return self.unsupported_feature(span, format!("{name} is not supported"));
            }
            TokenKind::Identifier(ref identifier) => {
                return self
                    .syntax_error_current(format!("syntax error at or near \"{identifier}\""));
            }
            TokenKind::Keyword(ref keyword) => {
                return self.syntax_error_current(format!(
                    "syntax error at or near \"{}\"",
                    keyword.name().to_lowercase()
                ));
            }
            _ => return self.syntax_error_current("unexpected statement"),
        };
        self.maybe_parse_set_operation(left)
    }

    fn detect_reindex_verbose_notice(&self) -> Option<String> {
        let token_word_is = |tok: &Token, word: &str| match &tok.kind {
            TokenKind::Identifier(value) => value.eq_ignore_ascii_case(word),
            TokenKind::Keyword(keyword) => keyword.name().eq_ignore_ascii_case(word),
            _ => false,
        };

        let mut saw_verbose = false;
        let mut saw_table = false;
        let mut table_name: Option<String> = None;

        for tok in self.tokens.iter().skip(self.index) {
            if matches!(tok.kind, TokenKind::Semicolon | TokenKind::Eof) {
                break;
            }
            if token_word_is(tok, "verbose") {
                saw_verbose = true;
                continue;
            }
            if token_word_is(tok, "table") {
                saw_table = true;
                continue;
            }
            if saw_table && table_name.is_none() {
                match &tok.kind {
                    TokenKind::Identifier(value) => {
                        table_name = Some(value.clone());
                    }
                    TokenKind::Keyword(keyword) => {
                        table_name = Some(keyword.name().to_ascii_lowercase());
                    }
                    _ => {}
                }
            }
        }

        if saw_verbose && saw_table {
            table_name.map(|name| format!("INFO:  index \"{name}_pkey\" was reindexed"))
        } else {
            None
        }
    }

    fn maybe_parse_set_operation(&mut self, left: Statement) -> DbResult<Statement> {
        self.set_op_depth += 1;
        let result = self.maybe_parse_set_operation_inner(left);
        self.set_op_depth -= 1;
        result
    }

    fn maybe_parse_set_operation_inner(&mut self, left: Statement) -> DbResult<Statement> {
        use ast::{SetOperationStatement, SetOperationType};

        if self.set_op_depth > 64 {
            return Err(DbError::syntax_error(
                "set operation nesting too deep (maximum is 64)",
            ));
        }

        // Accept set-ops where each arm is parenthesized, e.g.
        // `(SELECT ...) UNION ALL (SELECT ...)`.
        // In these forms, after parsing the left arm we can be positioned on
        // a closing ')' right before the set-op keyword.
        if matches!(self.current().kind, TokenKind::RParen)
            && matches!(
                self.tokens.get(self.index + 1).map(|token| &token.kind),
                Some(TokenKind::Keyword(
                    Keyword::Union | Keyword::Intersect | Keyword::Except
                ))
            )
        {
            self.advance();
        }

        let op = if self.consume_keyword(Keyword::Union).is_some() {
            SetOperationType::Union
        } else if self.consume_keyword(Keyword::Intersect).is_some() {
            SetOperationType::Intersect
        } else if self.consume_keyword(Keyword::Except).is_some() {
            SetOperationType::Except
        } else {
            return Ok(left);
        };

        let all = self.consume_keyword(Keyword::All).is_some();
        // DISTINCT is the default for set operations; consume it if present.
        if !all {
            self.consume_keyword(Keyword::Distinct);
        }

        // Parse the right-hand SELECT (bare, without ORDER BY/LIMIT/OFFSET)
        // Handle parenthesized SELECT: (SELECT ...) or (WITH ... SELECT ...)
        let right_is_parenthesized;
        let right = if self.current().kind == TokenKind::LParen {
            right_is_parenthesized = true;
            self.advance(); // consume '('
                            // Handle nested parens
            let mut extra_parens = 0u32;
            while matches!(self.current().kind, TokenKind::LParen) {
                self.advance();
                extra_parens += 1;
            }
            let inner = if matches!(self.current().kind, TokenKind::Keyword(Keyword::Select)) {
                let sel = self.parse_select_statement()?;
                // Handle set operations inside parens: (SELECT ... EXCEPT SELECT ...)
                self.maybe_parse_set_operation(sel)?
            } else if matches!(self.current().kind, TokenKind::Keyword(Keyword::Values)) {
                self.parse_values_as_statement()?
            } else if matches!(self.current().kind, TokenKind::Keyword(Keyword::With)) {
                self.parse_with_statement()?
            } else {
                self.parse_select_bare()?
            };
            for _ in 0..extra_parens {
                let _ = self.consume_kind(&TokenKind::RParen);
            }
            let _ = self.consume_kind(&TokenKind::RParen);
            inner
        } else if matches!(self.current().kind, TokenKind::Keyword(Keyword::Values)) {
            right_is_parenthesized = false;
            self.parse_values_as_statement()?
        } else {
            right_is_parenthesized = false;
            self.parse_select_statement()?
        };

        // If the right-hand SELECT consumed ORDER BY / LIMIT / OFFSET that
        // should belong to the set operation, extract them.
        // When the right side is parenthesized, ORDER BY / LIMIT / OFFSET
        // belong to the inner subquery, not the set operation.
        let (mut stolen_order_by, mut stolen_order_by_span) = (Vec::new(), None);
        let (mut stolen_limit, mut stolen_limit_span) = (None, None);
        let (mut stolen_offset, mut stolen_offset_span) = (None, None);
        let mut right = right;
        if !right_is_parenthesized {
            if let Statement::Select(ref mut sel) = right {
                if !sel.order_by.is_empty() || sel.limit.is_some() || sel.offset.is_some() {
                    stolen_order_by = std::mem::take(&mut sel.order_by);
                    stolen_order_by_span = sel.order_by_span.take();
                    stolen_limit = sel.limit.take();
                    stolen_limit_span = sel.limit_span.take();
                    stolen_offset = sel.offset.take();
                    stolen_offset_span = sel.offset_span.take();
                }
            }
        }

        // Check for chaining: if another set operation keyword follows,
        // build the current pair and recurse.
        let is_chained = matches!(
            self.current().kind,
            TokenKind::Keyword(Keyword::Union | Keyword::Intersect | Keyword::Except)
        );

        if is_chained {
            let left_span = left.span();
            let span = left_span.merge(right.span());
            let intermediate = Statement::SetOperation(SetOperationStatement {
                op,
                all,
                left: Box::new(left),
                right: Box::new(right),
                order_by: Vec::new(),
                order_by_span: None,
                limit: None,
                limit_span: None,
                offset: None,
                offset_span: None,
                span,
            });
            return self.maybe_parse_set_operation(intermediate);
        }

        let left_span = left.span();
        let mut span = left_span.merge(right.span());

        // Parse trailing ORDER BY / LIMIT / OFFSET (applies to the combined result)
        let (order_by, order_by_span) =
            if let Some(order_token) = self.consume_keyword(Keyword::Order) {
                let by_token = self.expect_keyword(Keyword::By)?;
                let mut items = Vec::new();
                let mut order_span = order_token.span.merge(by_token.span);
                loop {
                    let expr = self.parse_expr()?;
                    let mut item_span = expr.span();
                    let descending = if self.consume_keyword(Keyword::Desc).is_some() {
                        item_span = item_span.merge(self.previous_span());
                        true
                    } else if self.consume_keyword(Keyword::Asc).is_some() {
                        item_span = item_span.merge(self.previous_span());
                        false
                    } else {
                        false
                    };
                    let nulls_first = if self.consume_keyword(Keyword::Nulls).is_some() {
                        item_span = item_span.merge(self.previous_span());
                        if self.consume_keyword(Keyword::First).is_some() {
                            item_span = item_span.merge(self.previous_span());
                            Some(true)
                        } else {
                            self.expect_keyword(Keyword::Last)?;
                            item_span = item_span.merge(self.previous_span());
                            Some(false)
                        }
                    } else {
                        None
                    };
                    order_span = order_span.merge(item_span);
                    items.push(ast::OrderByItem {
                        expr,
                        descending,
                        nulls_first,
                        span: item_span,
                    });
                    if !self.consume_kind(&TokenKind::Comma) {
                        break;
                    }
                }
                span = span.merge(order_span);
                (items, Some(order_span))
            } else {
                (Vec::new(), None)
            };

        let (limit, limit_span) = if let Some(limit_token) = self.consume_keyword(Keyword::Limit) {
            let expr = self.parse_expr()?;
            let span_with_value = limit_token.span.merge(expr.span());
            span = span.merge(span_with_value);
            (Some(expr), Some(span_with_value))
        } else {
            (None, None)
        };

        let (offset, offset_span) =
            if let Some(offset_token) = self.consume_keyword(Keyword::Offset) {
                let expr = self.parse_expr()?;
                let span_with_value = offset_token.span.merge(expr.span());
                span = span.merge(span_with_value);
                (Some(expr), Some(span_with_value))
            } else {
                (None, None)
            };

        // Use stolen ORDER BY/LIMIT/OFFSET from the right SELECT if no
        // trailing clauses were parsed at the set-operation level.
        let final_order_by = if order_by.is_empty() && !stolen_order_by.is_empty() {
            stolen_order_by
        } else {
            order_by
        };
        let final_order_by_span = if final_order_by.is_empty() {
            order_by_span
        } else {
            order_by_span.or(stolen_order_by_span)
        };
        let final_limit = limit.or(stolen_limit);
        let final_limit_span = limit_span.or(stolen_limit_span);
        let final_offset = offset.or(stolen_offset);
        let final_offset_span = offset_span.or(stolen_offset_span);

        Ok(Statement::SetOperation(SetOperationStatement {
            op,
            all,
            left: Box::new(left),
            right: Box::new(right),
            order_by: final_order_by,
            order_by_span: final_order_by_span,
            limit: final_limit,
            limit_span: final_limit_span,
            offset: final_offset,
            offset_span: final_offset_span,
            span,
        }))
    }

    /// Parse `TABLE tablename` as shorthand for `SELECT * FROM tablename`.
    fn parse_table_shorthand(&mut self) -> DbResult<Statement> {
        let table_kw = self.expect_keyword(Keyword::Table)?;
        let name = self.parse_object_name()?;
        let mut span = table_kw.span.merge(name.span);
        let from_span = name.span;
        let star = ast::SelectItem {
            expr: Expr::Identifier(ast::ObjectName {
                parts: vec!["*".to_owned()],
                span,
            }),
            alias: None,
            span,
        };

        let (order_by, order_by_span) =
            if let Some(order_token) = self.consume_keyword(Keyword::Order) {
                let by_token = self.expect_keyword(Keyword::By)?;
                let mut items = Vec::new();
                let mut order_span = order_token.span.merge(by_token.span);
                loop {
                    let expr = self.parse_expr()?;
                    let mut item_span = expr.span();
                    let descending = if self.consume_keyword(Keyword::Desc).is_some() {
                        item_span = item_span.merge(self.previous_span());
                        true
                    } else if self.consume_keyword(Keyword::Asc).is_some() {
                        item_span = item_span.merge(self.previous_span());
                        false
                    } else {
                        false
                    };
                    let nulls_first = if self.consume_keyword(Keyword::Nulls).is_some() {
                        item_span = item_span.merge(self.previous_span());
                        if self.consume_keyword(Keyword::First).is_some() {
                            item_span = item_span.merge(self.previous_span());
                            Some(true)
                        } else {
                            self.expect_keyword(Keyword::Last)?;
                            item_span = item_span.merge(self.previous_span());
                            Some(false)
                        }
                    } else {
                        None
                    };
                    order_span = order_span.merge(item_span);
                    items.push(ast::OrderByItem {
                        expr,
                        descending,
                        nulls_first,
                        span: item_span,
                    });
                    if !self.consume_kind(&TokenKind::Comma) {
                        break;
                    }
                }
                span = span.merge(order_span);
                (items, Some(order_span))
            } else {
                (Vec::new(), None)
            };

        let (limit, limit_span) = if let Some(limit_token) = self.consume_keyword(Keyword::Limit) {
            let expr = self.parse_expr()?;
            let span_with_value = limit_token.span.merge(expr.span());
            span = span.merge(span_with_value);
            (Some(expr), Some(span_with_value))
        } else {
            (None, None)
        };

        let (offset, offset_span) =
            if let Some(offset_token) = self.consume_keyword(Keyword::Offset) {
                let expr = self.parse_expr()?;
                let span_with_value = offset_token.span.merge(expr.span());
                span = span.merge(span_with_value);
                (Some(expr), Some(span_with_value))
            } else {
                (None, None)
            };

        Ok(Statement::Select(ast::SelectStatement {
            row_lock: None,
            ctes: Vec::new(),
            distinct: ast::DistinctKind::All,
            items: vec![star],
            from: Some(name),
            from_alias: None,
            from_span: Some(from_span),
            joins: Vec::new(),
            selection: None,
            where_span: None,
            group_by: Vec::new(),
            group_by_items: Vec::new(),
            group_by_span: None,
            having: None,
            having_span: None,
            window_definitions: Vec::new(),
            order_by,
            order_by_span,
            limit,
            limit_span,
            offset,
            offset_span,
            span,
        }))
    }

    /// Peek at the current token and return its text if it looks like an
    /// object name (identifier or keyword used as identifier).  Does NOT
    /// for NOTICE messages.
    pub(crate) fn peek_noop_object_name(&self) -> String {
        match &self.current().kind {
            TokenKind::Identifier(s) => s.clone(),
            TokenKind::Keyword(kw) => kw.name().to_lowercase(),
            _ => String::new(),
        }
    }

    /// Skip all tokens until `;` or EOF, properly handling balanced
    /// parentheses `()`, brackets `[]`, and braces `{}`.
    /// statement without failing on nested constructs.
    pub(crate) fn skip_to_semicolon(&mut self, span: &mut Span) {
        let mut paren_depth: u32 = 0;
        let mut bracket_depth: u32 = 0;
        let mut brace_depth: u32 = 0;
        while !self.is_eof() {
            match self.current().kind {
                TokenKind::Semicolon
                    if paren_depth == 0 && bracket_depth == 0 && brace_depth == 0 =>
                {
                    break;
                }
                TokenKind::LParen => {
                    paren_depth += 1;
                    *span = span.merge(self.advance().span);
                }
                TokenKind::RParen => {
                    paren_depth = paren_depth.saturating_sub(1);
                    *span = span.merge(self.advance().span);
                }
                TokenKind::LBracket => {
                    bracket_depth += 1;
                    *span = span.merge(self.advance().span);
                }
                TokenKind::RBracket => {
                    bracket_depth = bracket_depth.saturating_sub(1);
                    *span = span.merge(self.advance().span);
                }
                TokenKind::LBrace => {
                    brace_depth += 1;
                    *span = span.merge(self.advance().span);
                }
                TokenKind::RBrace => {
                    brace_depth = brace_depth.saturating_sub(1);
                    *span = span.merge(self.advance().span);
                }
                _ => {
                    *span = span.merge(self.advance().span);
                }
            }
        }
    }

    /// Skip zero or more array subscript brackets: `[expr]`, `[lo:hi]`,
    /// `col[1][2]`, etc.  Used in UPDATE SET and INSERT column lists where
    /// `PostgreSQL` allows `col[n] = expr`.
    pub(crate) fn skip_array_subscripts(&mut self) {
        while self.current().kind == TokenKind::LBracket {
            self.advance(); // consume '['
            let mut depth: u32 = 1;
            while !self.is_eof() && depth > 0 {
                match self.current().kind {
                    TokenKind::LBracket => {
                        depth += 1;
                        self.advance();
                    }
                    TokenKind::RBracket => {
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

    pub(crate) fn current(&self) -> &Token {
        &self.tokens[self.index]
    }

    pub(crate) fn is_eof(&self) -> bool {
        matches!(self.current().kind, TokenKind::Eof)
    }

    pub(crate) fn advance(&mut self) -> Token {
        let token = self.tokens[self.index].clone();
        if !matches!(token.kind, TokenKind::Eof) {
            self.index += 1;
        }
        token
    }

    /// Advance the cursor and return only the [`Span`] (which is `Copy`),
    /// avoiding the clone of the full `Token`.
    pub(crate) fn advance_span(&mut self) -> Span {
        let span = self.tokens[self.index].span;
        if !matches!(self.tokens[self.index].kind, TokenKind::Eof) {
            self.index += 1;
        }
        span
    }

    pub(crate) fn previous_span(&self) -> Span {
        if self.index == 0 {
            self.current().span
        } else {
            self.tokens[self.index - 1].span
        }
    }

    pub(crate) fn consume_kind(&mut self, kind: &TokenKind) -> bool {
        if &self.current().kind == kind {
            // Skip the full Token clone: we only need to advance the cursor.
            let _ = self.advance_span();
            true
        } else {
            false
        }
    }

    pub(crate) fn consume_keyword(&mut self, keyword: Keyword) -> Option<Token> {
        match self.current().kind {
            TokenKind::Keyword(current) if current == keyword => Some(self.advance()),
            _ => None,
        }
    }

    pub(crate) fn expect_keyword(&mut self, keyword: Keyword) -> DbResult<Token> {
        self.consume_keyword(keyword)
            .ok_or_else(|| self.unexpected_token(keyword.name()))
    }

    pub(crate) fn consume_if_exists_clause(&mut self) -> DbResult<bool> {
        if self.consume_keyword(Keyword::If).is_some() {
            self.expect_keyword(Keyword::Exists)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub(crate) fn consume_if_exists_clause_with_preconsumed(
        &mut self,
        preconsumed: bool,
    ) -> DbResult<bool> {
        if preconsumed {
            Ok(true)
        } else {
            self.consume_if_exists_clause()
        }
    }

    /// Consume optional `IF [EXISTS]` where `EXISTS` is tolerated as optional.
    ///
    /// permissiveness is intentional.
    pub(crate) fn consume_if_exists_lenient_with_preconsumed(&mut self, preconsumed: bool) -> bool {
        preconsumed
            || (self.consume_keyword(Keyword::If).is_some() && {
                let _ = self.consume_keyword(Keyword::Exists);
                true
            })
    }

    pub(crate) fn consume_if_not_exists_clause(&mut self) -> DbResult<bool> {
        if self.consume_keyword(Keyword::If).is_some() {
            self.expect_keyword(Keyword::Not)?;
            self.expect_keyword(Keyword::Exists)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub(crate) fn consume_drop_behavior_keyword(&mut self) -> Option<Token> {
        self.consume_keyword(Keyword::Cascade)
            .or_else(|| self.consume_keyword(Keyword::Restrict))
    }

    pub(crate) fn expect_identifier(&mut self) -> DbResult<(String, Span)> {
        if matches!(
            self.current().kind,
            TokenKind::Identifier(_) | TokenKind::Keyword(_)
        ) {
            let token = self.advance();
            let name = match token.kind {
                TokenKind::Identifier(value) => value,
                TokenKind::Keyword(kw) => kw.name().to_lowercase(),
                _ => unreachable!(),
            };
            // Cypher is case-sensitive for identifiers, but the SQL lexer
            // folds unquoted identifiers to lowercase. When in Cypher mode,
            // recover the original case from the source span so identifiers
            // like `inputList` or `myVar` survive. Backtick-delimited names
            // are already preserved by the lexer; we only patch unquoted
            // ones with mixed-case letters.
            let name = if self.cypher_mode {
                let raw = self.source.get(token.span.start..token.span.end);
                match raw {
                    Some(r) if r.starts_with('`') => name,
                    Some(r) if r.starts_with('"') => name,
                    Some(r) if !r.is_empty() && r.bytes().any(|b| b.is_ascii_uppercase()) => {
                        r.to_owned()
                    }
                    _ => name,
                }
            } else {
                name
            };
            Ok((name, token.span))
        } else {
            Err(self.unexpected_token("identifier"))
        }
    }

    pub(crate) fn expect_token(&mut self, kind: &TokenKind) -> DbResult<Token> {
        if &self.current().kind == kind {
            Ok(self.advance())
        } else {
            Err(self.unexpected_token(kind.describe()))
        }
    }

    /// Returns `true` if the current token is an identifier matching `name`
    /// (case-insensitive comparison).
    pub(crate) fn current_is_identifier_ci(&self, name: &str) -> bool {
        matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case(name))
    }

    pub(crate) fn syntax_error<T>(&self, span: Span, message: impl Into<String>) -> DbResult<T> {
        Err(DbError::syntax_error(message).with_position(span.start + 1))
    }

    pub(crate) fn unsupported_feature<T>(
        &self,
        span: Span,
        message: impl Into<String>,
    ) -> DbResult<T> {
        Err(DbError::feature_not_supported(message).with_position(span.start + 1))
    }

    pub(crate) fn syntax_error_current<T>(&self, message: impl Into<String>) -> DbResult<T> {
        self.syntax_error(self.current().span, message)
    }

    pub(crate) fn syntax_error_at_current_token<T>(&self) -> DbResult<T> {
        let token = self.current();
        match &token.kind {
            TokenKind::Eof => self.syntax_error(token.span, "syntax error at end of input"),
            TokenKind::Keyword(kw) => self.syntax_error(
                token.span,
                format!("syntax error at or near \"{}\"", kw.name().to_lowercase()),
            ),
            TokenKind::Identifier(name) => {
                self.syntax_error(token.span, format!("syntax error at or near \"{name}\""))
            }
            TokenKind::Integer(value) => {
                self.syntax_error(token.span, format!("syntax error at or near \"{value}\""))
            }
            TokenKind::NumericLiteral(value) => {
                self.syntax_error(token.span, format!("syntax error at or near \"{value}\""))
            }
            TokenKind::String(value) => {
                self.syntax_error(token.span, format!("syntax error at or near \"{value}\""))
            }
            TokenKind::Semicolon => self.syntax_error(token.span, "syntax error at or near \";\""),
            TokenKind::Comma => self.syntax_error(token.span, "syntax error at or near \",\""),
            TokenKind::LParen => self.syntax_error(token.span, "syntax error at or near \"(\""),
            TokenKind::RParen => self.syntax_error(token.span, "syntax error at or near \")\""),
            TokenKind::Dot => self.syntax_error(token.span, "syntax error at or near \".\""),
            TokenKind::Plus => self.syntax_error(token.span, "syntax error at or near \"+\""),
            TokenKind::Minus => self.syntax_error(token.span, "syntax error at or near \"-\""),
            TokenKind::Star => self.syntax_error(token.span, "syntax error at or near \"*\""),
            TokenKind::Slash => self.syntax_error(token.span, "syntax error at or near \"/\""),
            TokenKind::Eq => self.syntax_error(token.span, "syntax error at or near \"=\""),
            TokenKind::Lt => self.syntax_error(token.span, "syntax error at or near \"<\""),
            TokenKind::Gt => self.syntax_error(token.span, "syntax error at or near \">\""),
            TokenKind::Ne => self.syntax_error(token.span, "syntax error at or near \"!=\""),
            TokenKind::Le => self.syntax_error(token.span, "syntax error at or near \"<=\""),
            TokenKind::Ge => self.syntax_error(token.span, "syntax error at or near \">=\""),
            TokenKind::Percent => self.syntax_error(token.span, "syntax error at or near \"%\""),
            TokenKind::PipePipe => self.syntax_error(token.span, "syntax error at or near \"||\""),
            TokenKind::Arrow => self.syntax_error(token.span, "syntax error at or near \"->\""),
            TokenKind::DoubleArrow => {
                self.syntax_error(token.span, "syntax error at or near \"->>\"")
            }
            TokenKind::HashArrow => self.syntax_error(token.span, "syntax error at or near \"#>\""),
            TokenKind::HashDoubleArrow => {
                self.syntax_error(token.span, "syntax error at or near \"#>>\"")
            }
            TokenKind::AtGt => self.syntax_error(token.span, "syntax error at or near \"@>\""),
            TokenKind::LtAt => self.syntax_error(token.span, "syntax error at or near \"<@\""),
            TokenKind::VecL2 => self.syntax_error(token.span, "syntax error at or near \"<->\""),
            TokenKind::VecCosine => {
                self.syntax_error(token.span, "syntax error at or near \"<=>\"")
            }
            TokenKind::VecNegInner => {
                self.syntax_error(token.span, "syntax error at or near \"<#>\"")
            }
            TokenKind::VecL1 => self.syntax_error(token.span, "syntax error at or near \"<+>\""),
            TokenKind::VecHamming => {
                self.syntax_error(token.span, "syntax error at or near \"<~>\"")
            }
            TokenKind::VecJaccard => {
                self.syntax_error(token.span, "syntax error at or near \"<%>\"")
            }
            TokenKind::Question => self.syntax_error(token.span, "syntax error at or near \"?\""),
            TokenKind::QuestionPipe => {
                self.syntax_error(token.span, "syntax error at or near \"?|\"")
            }
            TokenKind::QuestionAmpersand => {
                self.syntax_error(token.span, "syntax error at or near \"?&\"")
            }
            TokenKind::AmpAmp => self.syntax_error(token.span, "syntax error at or near \"&&\""),
            TokenKind::AtAt => self.syntax_error(token.span, "syntax error at or near \"@@\""),
            TokenKind::AtQuestion => {
                self.syntax_error(token.span, "syntax error at or near \"@?\"")
            }
            TokenKind::DoubleColon => {
                self.syntax_error(token.span, "syntax error at or near \"::\"")
            }
            TokenKind::Tilde => self.syntax_error(token.span, "syntax error at or near \"~\""),
            TokenKind::TildeEq => self.syntax_error(token.span, "syntax error at or near \"~=\""),
            TokenKind::TildeStar => self.syntax_error(token.span, "syntax error at or near \"~*\""),
            TokenKind::ExclTilde => self.syntax_error(token.span, "syntax error at or near \"!~\""),
            TokenKind::ExclTildeStar => {
                self.syntax_error(token.span, "syntax error at or near \"!~*\"")
            }
            TokenKind::Ampersand => self.syntax_error(token.span, "syntax error at or near \"&\""),
            TokenKind::Pipe => self.syntax_error(token.span, "syntax error at or near \"|\""),
            TokenKind::Hash => self.syntax_error(token.span, "syntax error at or near \"#\""),
            TokenKind::HashMinus => self.syntax_error(token.span, "syntax error at or near \"#-\""),
            TokenKind::Caret => self.syntax_error(token.span, "syntax error at or near \"^\""),
            TokenKind::ShiftLeft => self.syntax_error(token.span, "syntax error at or near \"<<\""),
            TokenKind::ShiftRight => {
                self.syntax_error(token.span, "syntax error at or near \">>\"")
            }
            TokenKind::At => self.syntax_error(token.span, "syntax error at or near \"@\""),
            TokenKind::CustomOp => {
                self.syntax_error(token.span, "syntax error at or near \"operator\"")
            }
            TokenKind::Colon => self.syntax_error(token.span, "syntax error at or near \":\""),
            TokenKind::LBrace => self.syntax_error(token.span, "syntax error at or near \"{\""),
            TokenKind::RBrace => self.syntax_error(token.span, "syntax error at or near \"}\""),
            TokenKind::LBracket => self.syntax_error(token.span, "syntax error at or near \"[\""),
            TokenKind::RBracket => self.syntax_error(token.span, "syntax error at or near \"]\""),
            TokenKind::Excl => self.syntax_error(token.span, "syntax error at or near \"!\""),
            TokenKind::FatArrow => self.syntax_error(token.span, "syntax error at or near \"=>\""),
            TokenKind::Parameter(index) => {
                self.syntax_error(token.span, format!("syntax error at or near \"${index}\""))
            }
        }
    }

    pub(crate) fn unexpected_token(&self, expected: impl AsRef<str>) -> DbError {
        let token = self.current();
        DbError::syntax_error(format!(
            "expected {}, found {}",
            expected.as_ref(),
            token.kind.describe()
        ))
        .with_position(token.span.start + 1)
    }
}

#[cfg(test)]
mod tests;

// =============================================================================
// Fuzz / smoke tests -- robustness against malformed, random, and adversarial
// inputs.  The parser must NEVER panic; it must always return Ok or a
// well-structured DbError.
// =============================================================================
#[cfg(test)]
mod fuzz_smoke;

// ---- Cypher graph query support ----

pub use cypher_ast::*;

pub fn parse_cypher(sql: &str) -> aiondb_core::DbResult<cypher_ast::CypherStatement> {
    let tokens = lex_and_validate(sql)?;
    Parser::new(tokens).parse_cypher_statement()
}
