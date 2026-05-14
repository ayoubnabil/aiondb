#![allow(clippy::doc_markdown)]

//! `PostgreSQL`-compatible `JSONPath` evaluator for `jsonb_path_query_first`,
//! `jsonb_path_query_array`, `jsonb_path_exists`, and `jsonb_path_match`.
//!
//! Supports the SQL/JSON path language subset used by `PostgreSQL` 16:
//! - Root `$`, member access `.key`, wildcard `.*`, recursive `.**`
//! - Array access `[n]`, `[*]`, `[n to m]`, `[last]`, multiple subscripts
//! - Filters `? (predicate)` with comparisons, logical operators, `exists()`
//! - Arithmetic `+`, `-`, `*`, `/`, `%` in expressions
//! - Methods `.type()`, `.size()`, `.double()`, `.ceiling()`, `.floor()`,
//!   `.abs()`, `.keyvalue()`
//! - Lax (default) vs strict mode
//! - Variable references `$varname`

use std::borrow::Cow;

use aiondb_core::{DbError, DbResult, Value};
use serde_json::Value as JV;

use self::jsonpath_support::{collect_recursive, collect_recursive_owned, eval_filter};
use super::json_helpers::{floor_clamped_f64_to_i64, json_to_f64, whole_f64_to_i64};

#[path = "jsonpath_support.rs"]
mod jsonpath_support;

// ── Public evaluation entry points ──────────────────────────────────

pub(super) fn eval_jsonb_path_query_first(args: &[Value]) -> DbResult<Value> {
    match eval_jsonb_path_query_all(args)?.into_iter().next() {
        Some(value) => Ok(value),
        None => Ok(Value::Null),
    }
}

pub(super) fn eval_jsonb_path_query_array(args: &[Value]) -> DbResult<Value> {
    let results = eval_jsonb_path_query_all(args)?
        .into_iter()
        .map(|value| match value {
            Value::Jsonb(json) => json,
            Value::Null => JV::Null,
            other => JV::String(other.to_string()),
        })
        .collect();
    Ok(Value::Jsonb(JV::Array(results)))
}

pub(super) fn eval_jsonb_path_exists(args: &[Value]) -> DbResult<Value> {
    Ok(Value::Boolean(!eval_jsonb_path_query_all(args)?.is_empty()))
}

pub(super) fn eval_jsonb_path_match(args: &[Value]) -> DbResult<Value> {
    match eval_jsonb_path_query_all(args)?.first() {
        Some(Value::Jsonb(JV::Bool(b))) => Ok(Value::Boolean(*b)),
        Some(_) => Ok(Value::Null),
        None => Ok(Value::Null),
    }
}

/// `__aiondb_jsonpath_cast(text_or_value)` is the internal cast hook used by
/// the planner to validate and canonicalize `::jsonpath`.
pub(super) fn eval_jsonpath_cast(args: &[Value]) -> DbResult<Value> {
    let Some(value) = args.first() else {
        return Err(DbError::internal(
            "__aiondb_jsonpath_cast() requires exactly one argument",
        ));
    };
    if matches!(value, Value::Null) {
        return Ok(Value::Null);
    }
    let input = value.to_string();
    Ok(Value::Text(normalize_jsonpath_text(&input)?))
}

/// Evaluate a jsonpath and return ALL matches (for jsonb_path_query SRF).
pub(super) fn eval_jsonb_path_query_all(args: &[Value]) -> DbResult<Vec<Value>> {
    let (target, path_str, vars, _silent) = extract_path_args(args)?;
    let path = parse_jsonpath_cached(path_str.as_ref())?;
    let results = eval_path(target.as_ref(), path.as_ref(), vars.as_ref());
    Ok(results.into_iter().map(Value::Jsonb).collect())
}

// ── Per-thread parsed-jsonpath cache ────────────────────────────────
//
// `eval_jsonb_path_query_*` functions run per row, and the second
// argument (the path string) is almost always a constant literal.
// Without a cache we re-parse the path through the recursive descent
// parser on every row of the scan. Mirror the regex_cache approach:
// thread-local + bounded size + Arc so hits are zero-alloc.

const JSONPATH_CACHE_CAP: usize = 256;

thread_local! {
    static JSONPATH_CACHE: std::cell::RefCell<
        std::collections::HashMap<String, std::sync::Arc<JsonPath>>,
    > = std::cell::RefCell::new(std::collections::HashMap::with_capacity(16));
}

fn parse_jsonpath_cached(input: &str) -> DbResult<std::sync::Arc<JsonPath>> {
    JSONPATH_CACHE.with(|cell| {
        if let Some(hit) = cell.borrow().get(input) {
            return Ok(std::sync::Arc::clone(hit));
        }
        let parsed = parse_jsonpath(input)?;
        let arc = std::sync::Arc::new(parsed);
        let mut map = cell.borrow_mut();
        if map.len() >= JSONPATH_CACHE_CAP {
            map.clear();
        }
        map.insert(input.to_owned(), std::sync::Arc::clone(&arc));
        Ok(arc)
    })
}

// ── Argument extraction ─────────────────────────────────────────────

fn extract_path_args(args: &[Value]) -> DbResult<(Cow<'_, JV>, Cow<'_, str>, Cow<'_, JV>, bool)> {
    if args.len() < 2 {
        return Err(DbError::internal(
            "jsonb_path functions require at least 2 arguments",
        ));
    }
    // First arg: JSONB target
    let target = match &args[0] {
        Value::Null => {
            return Ok((
                Cow::Owned(JV::Null),
                Cow::Borrowed(""),
                Cow::Owned(JV::Null),
                false,
            ));
        }
        Value::Jsonb(j) => Cow::Borrowed(j),
        Value::Text(s) => Cow::Owned(
            serde_json::from_str(s).map_err(|e| DbError::internal(format!("invalid JSON: {e}")))?,
        ),
        _ => return Err(DbError::internal("first argument must be jsonb")),
    };
    // Second arg: jsonpath string
    let path_str = match &args[1] {
        Value::Null => return Ok((target, Cow::Borrowed(""), Cow::Owned(JV::Null), false)),
        Value::Text(s) => Cow::Borrowed(s.as_str()),
        _ => return Err(DbError::internal("second argument must be text")),
    };
    // Third arg (optional): variables jsonb object
    let vars = if args.len() >= 3 {
        match &args[2] {
            Value::Null => Cow::Owned(JV::Object(serde_json::Map::new())),
            Value::Jsonb(j) => Cow::Borrowed(j),
            Value::Text(s) => Cow::Owned(serde_json::from_str(s).unwrap_or(JV::Null)),
            _ => Cow::Owned(JV::Object(serde_json::Map::new())),
        }
    } else {
        Cow::Owned(JV::Object(serde_json::Map::new()))
    };
    let silent = if args.len() >= 4 {
        matches!(&args[3], Value::Boolean(true))
    } else {
        false
    };
    Ok((target, path_str, vars, silent))
}

// ── JSONPath AST ────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct JsonPath {
    mode: PathMode,
    expr: PathExpr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathMode {
    Lax,
    Strict,
}

#[derive(Debug, Clone)]
enum PathExpr {
    Root,
    Current,
    Member(Box<PathExpr>, String),
    WildcardMember(Box<PathExpr>),
    ArrayIndex(Box<PathExpr>, Vec<ArraySubscript>),
    WildcardArray(Box<PathExpr>),
    RecursiveDescend(Box<PathExpr>, Option<(u32, u32)>),
    Filter(Box<PathExpr>, Box<FilterExpr>),
    Method(Box<PathExpr>, String, Vec<PathExpr>),
    Literal(JV),
    Variable(String),
    BinaryArith(Box<PathExpr>, ArithOp, Box<PathExpr>),
    UnaryMinus(Box<PathExpr>),
    UnaryPlus(Box<PathExpr>),
    Predicate(Box<FilterExpr>),
}

#[derive(Debug, Clone)]
enum ArraySubscript {
    Index(Box<PathExpr>),
    Range(Box<PathExpr>, Box<PathExpr>),
}

#[derive(Debug, Clone, Copy)]
enum ArithOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

#[derive(Debug, Clone)]
enum FilterExpr {
    Comparison(CmpOp, Box<PathExpr>, Box<PathExpr>),
    And(Box<FilterExpr>, Box<FilterExpr>),
    Or(Box<FilterExpr>, Box<FilterExpr>),
    Not(Box<FilterExpr>),
    Exists(Box<PathExpr>),
    IsUnknown(Box<FilterExpr>),
    PathPredicate(Box<PathExpr>),
}

#[derive(Debug, Clone, Copy)]
enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

// ── JSONPath Parser ─────────────────────────────────────────────────

/// Maximum nesting depth for jsonpath expressions to prevent stack overflow.
const MAX_JSONPATH_DEPTH: usize = 64;

struct PathParser<'a> {
    input: &'a str,
    pos: usize,
    depth: usize,
}

impl<'a> PathParser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input,
            pos: 0,
            depth: 0,
        }
    }

    fn skip_ws(&mut self) {
        while self.pos < self.input.len() && self.input.as_bytes()[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    fn peek(&mut self) -> Option<char> {
        self.skip_ws();
        self.input[self.pos..].chars().next()
    }

    fn peek_no_ws(&self) -> Option<char> {
        self.input[self.pos..].chars().next()
    }

    fn advance(&mut self, n: usize) {
        self.pos += n;
    }

    fn starts_with(&mut self, s: &str) -> bool {
        self.skip_ws();
        self.input[self.pos..].starts_with(s)
    }

    fn starts_with_ci(&mut self, s: &str) -> bool {
        self.skip_ws();
        let remaining = &self.input[self.pos..];
        if remaining.len() < s.len() {
            return false;
        }
        remaining[..s.len()].eq_ignore_ascii_case(s)
            && remaining
                .as_bytes()
                .get(s.len())
                .map_or(true, |b| !b.is_ascii_alphanumeric())
    }

    fn consume(&mut self, s: &str) -> bool {
        self.skip_ws();
        if self.input[self.pos..].starts_with(s) {
            self.pos += s.len();
            true
        } else {
            false
        }
    }

    fn consume_ci(&mut self, s: &str) -> bool {
        self.skip_ws();
        let remaining = &self.input[self.pos..];
        if remaining.len() < s.len() {
            return false;
        }
        if remaining[..s.len()].eq_ignore_ascii_case(s) {
            let after = remaining.as_bytes().get(s.len());
            if !after.is_some_and(|byte| byte.is_ascii_alphanumeric()) {
                self.pos += s.len();
                return true;
            }
        }
        false
    }

    fn parse_path(&mut self) -> DbResult<JsonPath> {
        self.skip_ws();
        let mode = if self.consume_ci("strict") {
            PathMode::Strict
        } else {
            self.consume_ci("lax");
            PathMode::Lax
        };
        let expr_start = self.pos;
        let mut expr = self.parse_expr()?;
        self.skip_ws();
        if self.pos != self.input.len() {
            self.pos = expr_start;
            let filter = self.parse_filter_expr()?;
            self.skip_ws();
            if self.pos == self.input.len() {
                expr = PathExpr::Predicate(Box::new(filter));
            } else {
                return Err(DbError::Internal(Box::new(aiondb_core::ErrorReport::new(
                    aiondb_core::SqlState::InvalidTextRepresentation,
                    format!(
                        "trailing junk after jsonpath input at position {}",
                        self.pos
                    ),
                ))));
            }
        }
        Ok(JsonPath { mode, expr })
    }

    fn parse_expr(&mut self) -> DbResult<PathExpr> {
        self.depth += 1;
        if self.depth > MAX_JSONPATH_DEPTH {
            return Err(DbError::internal(
                "jsonpath expression exceeds maximum nesting depth",
            ));
        }
        let result = self.parse_additive();
        self.depth -= 1;
        result
    }

    fn parse_expr_or_predicate_in_parens(&mut self) -> DbResult<PathExpr> {
        let checkpoint = self.pos;
        let expr = self.parse_expr()?;
        self.skip_ws();
        if self.peek_no_ws() == Some(')') {
            return Ok(expr);
        }
        self.pos = checkpoint;
        let filter = self.parse_filter_expr()?;
        Ok(PathExpr::Predicate(Box::new(filter)))
    }

    fn parse_additive(&mut self) -> DbResult<PathExpr> {
        let mut left = self.parse_multiplicative()?;
        loop {
            self.skip_ws();
            if self.consume("+") {
                let right = self.parse_multiplicative()?;
                left = PathExpr::BinaryArith(Box::new(left), ArithOp::Add, Box::new(right));
            } else if self.consume("-") {
                let right = self.parse_multiplicative()?;
                left = PathExpr::BinaryArith(Box::new(left), ArithOp::Sub, Box::new(right));
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn parse_multiplicative(&mut self) -> DbResult<PathExpr> {
        let mut left = self.parse_unary()?;
        loop {
            self.skip_ws();
            if self.consume("*") {
                let right = self.parse_unary()?;
                left = PathExpr::BinaryArith(Box::new(left), ArithOp::Mul, Box::new(right));
            } else if self.consume("/") {
                let right = self.parse_unary()?;
                left = PathExpr::BinaryArith(Box::new(left), ArithOp::Div, Box::new(right));
            } else if self.consume("%") {
                let right = self.parse_unary()?;
                left = PathExpr::BinaryArith(Box::new(left), ArithOp::Mod, Box::new(right));
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> DbResult<PathExpr> {
        // Collect leading unary operators iteratively rather than recursing.
        // The previous implementation recursed once per `-`/`+`, so an
        // attacker-controlled jsonpath expression like `$ + ---------1`
        // could blow the thread stack (confirmed remote DoS via
        // `jsonb_path_query`). A simple stack of operators keeps the
        // parser strictly O(n) and bounds stack depth to a constant.
        let mut ops: Vec<bool> = Vec::new();
        loop {
            self.skip_ws();
            if self.consume("-") {
                ops.push(false);
            } else if self.consume("+") {
                ops.push(true);
            } else {
                break;
            }
            if ops.len() > MAX_JSONPATH_DEPTH {
                return Err(DbError::internal(
                    "jsonpath unary operator chain exceeds maximum nesting depth",
                ));
            }
        }
        let mut expr = self.parse_primary()?;
        while let Some(is_plus) = ops.pop() {
            expr = if is_plus {
                PathExpr::UnaryPlus(Box::new(expr))
            } else {
                PathExpr::UnaryMinus(Box::new(expr))
            };
        }
        Ok(expr)
    }

    fn parse_primary(&mut self) -> DbResult<PathExpr> {
        self.skip_ws();
        let mut expr = match self.peek() {
            Some('$') => {
                self.advance(1);
                // Check if it's a variable reference like $varname
                if let Some(c) = self.peek_no_ws() {
                    if c.is_ascii_alphabetic() || c == '_' {
                        let name = self.parse_identifier();
                        PathExpr::Variable(name)
                    } else {
                        PathExpr::Root
                    }
                } else {
                    PathExpr::Root
                }
            }
            Some('@') => {
                self.advance(1);
                PathExpr::Current
            }
            Some('"') => {
                let s = self.parse_quoted_string()?;
                PathExpr::Literal(JV::String(s))
            }
            Some(c) if c.is_ascii_digit() => {
                let n = self.parse_number()?;
                PathExpr::Literal(n)
            }
            Some('.')
                if self
                    .input
                    .as_bytes()
                    .get(self.pos + 1)
                    .is_some_and(|b| b.is_ascii_digit()) =>
            {
                let n = self.parse_number()?;
                PathExpr::Literal(n)
            }
            Some('(') => {
                self.advance(1);
                let e = self.parse_expr_or_predicate_in_parens()?;
                if !self.consume(")") {
                    return Err(DbError::internal("expected ')' in jsonpath"));
                }
                e
            }
            Some('t' | 'T') if self.starts_with_ci("true") => {
                self.consume_ci("true");
                PathExpr::Literal(JV::Bool(true))
            }
            Some('f' | 'F') if self.starts_with_ci("false") => {
                self.consume_ci("false");
                PathExpr::Literal(JV::Bool(false))
            }
            Some('n' | 'N') if self.starts_with_ci("null") => {
                self.consume_ci("null");
                PathExpr::Literal(JV::Null)
            }
            Some('l' | 'L') if self.starts_with_ci("last") => {
                self.consume_ci("last");
                PathExpr::Method(Box::new(PathExpr::Current), "last".to_owned(), Vec::new())
            }
            _ => {
                // Try to parse as identifier (e.g., in bare key context)
                let ident = self.parse_identifier();
                if ident.is_empty() {
                    return Err(DbError::internal(format!(
                        "unexpected character in jsonpath at position {}",
                        self.pos
                    )));
                }
                if ident.as_bytes().first().is_some_and(|byte| *byte == b'_')
                    && ident
                        .as_bytes()
                        .get(1)
                        .is_some_and(|byte| byte.is_ascii_digit())
                {
                    return Err(DbError::internal("syntax error at end of jsonpath input"));
                }
                PathExpr::Literal(JV::String(ident))
            }
        };
        // Parse postfix accessors: .key, .*, .**, [n], [*], ?(filter), .method()
        expr = self.parse_postfix(expr)?;
        Ok(expr)
    }

    fn parse_postfix(&mut self, mut expr: PathExpr) -> DbResult<PathExpr> {
        loop {
            self.skip_ws();
            if self.consume(".") {
                self.skip_ws();
                if self.consume("*") {
                    // Check for ** (recursive descent)
                    if self.consume("*") {
                        let depth = self.parse_recursive_depth()?;
                        expr = PathExpr::RecursiveDescend(Box::new(expr), depth);
                    } else {
                        expr = PathExpr::WildcardMember(Box::new(expr));
                    }
                } else {
                    // Member access
                    let key = if self.peek() == Some('"') {
                        self.parse_quoted_string()?
                    } else {
                        self.parse_identifier()
                    };
                    if key.is_empty() {
                        return Err(DbError::internal("expected key name in jsonpath"));
                    }
                    // Check if it's a method call (key followed by ())
                    self.skip_ws();
                    if self.consume("(") {
                        let mut args = Vec::new();
                        self.skip_ws();
                        if !self.consume(")") {
                            loop {
                                args.push(self.parse_expr()?);
                                self.skip_ws();
                                if self.consume(")") {
                                    break;
                                }
                                if !self.consume(",") {
                                    return Err(DbError::internal("expected ')' in jsonpath"));
                                }
                            }
                        }
                        expr = PathExpr::Method(Box::new(expr), key, args);
                    } else {
                        expr = PathExpr::Member(Box::new(expr), key);
                    }
                }
            } else if self.starts_with("[") {
                self.advance(1);
                self.skip_ws();
                if self.consume("*") {
                    if !self.consume("]") {
                        return Err(DbError::internal("expected ']' after '[*'"));
                    }
                    expr = PathExpr::WildcardArray(Box::new(expr));
                } else {
                    let subscripts = self.parse_array_subscripts()?;
                    if !self.consume("]") {
                        return Err(DbError::internal("expected ']' in jsonpath"));
                    }
                    expr = PathExpr::ArrayIndex(Box::new(expr), subscripts);
                }
            } else if self.starts_with("?") && !self.starts_with("?|") && !self.starts_with("?&") {
                self.advance(1);
                if !self.consume("(") {
                    return Err(DbError::internal("expected '(' after '?'"));
                }
                let filter = self.parse_filter_expr()?;
                if !self.consume(")") {
                    return Err(DbError::internal("expected ')' after filter"));
                }
                expr = PathExpr::Filter(Box::new(expr), Box::new(filter));
            } else if self.starts_with("**") {
                self.advance(2);
                let depth = self.parse_recursive_depth()?;
                expr = PathExpr::RecursiveDescend(Box::new(expr), depth);
            } else {
                break;
            }
        }
        Ok(expr)
    }

    fn parse_recursive_depth(&mut self) -> DbResult<Option<(u32, u32)>> {
        self.skip_ws();
        if self.consume("{") {
            self.skip_ws();
            let lo = if self.starts_with_ci("last") {
                self.consume_ci("last");
                u32::MAX
            } else {
                self.parse_uint()?
            };
            self.skip_ws();
            let hi = if self.consume_ci("to") {
                self.skip_ws();
                if self.starts_with_ci("last") {
                    self.consume_ci("last");
                    u32::MAX
                } else {
                    self.parse_uint()?
                }
            } else {
                lo
            };
            if !self.consume("}") {
                return Err(DbError::internal("expected '}' in recursive depth"));
            }
            Ok(Some((lo, hi)))
        } else {
            Ok(None)
        }
    }

    fn parse_uint(&mut self) -> DbResult<u32> {
        self.skip_ws();
        let start = self.pos;
        while self.pos < self.input.len() && self.input.as_bytes()[self.pos].is_ascii_digit() {
            self.pos += 1;
        }
        if self.pos == start {
            return Err(DbError::internal("expected integer in jsonpath"));
        }
        self.input[start..self.pos]
            .parse::<u32>()
            .map_err(|_| DbError::internal("integer overflow in jsonpath"))
    }

    fn parse_array_subscripts(&mut self) -> DbResult<Vec<ArraySubscript>> {
        let mut subs = Vec::new();
        loop {
            let e = self.parse_expr()?;
            self.skip_ws();
            if self.consume_ci("to") {
                let e2 = self.parse_expr()?;
                subs.push(ArraySubscript::Range(Box::new(e), Box::new(e2)));
            } else {
                subs.push(ArraySubscript::Index(Box::new(e)));
            }
            self.skip_ws();
            if !self.consume(",") {
                break;
            }
        }
        Ok(subs)
    }

    fn parse_filter_expr(&mut self) -> DbResult<FilterExpr> {
        // Filter expressions recurse through parenthesised sub-expressions
        // (`parse_filter_primary` → `parse_filter_expr`). Without a guard,
        // a pattern like `$?( ((((… @==1 …)))) )` blows the thread stack
        // (confirmed remote DoS). Budget the depth here since every
        // parenthesised recursion funnels through this entry point.
        self.depth += 1;
        if self.depth > MAX_JSONPATH_DEPTH {
            self.depth -= 1;
            return Err(DbError::internal(
                "jsonpath filter expression exceeds maximum nesting depth",
            ));
        }
        let result = self.parse_filter_or();
        self.depth -= 1;
        result
    }

    fn parse_filter_or(&mut self) -> DbResult<FilterExpr> {
        let mut left = self.parse_filter_and()?;
        while self.consume_ci("||") || self.consume_ci("or") {
            let right = self.parse_filter_and()?;
            left = FilterExpr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_filter_and(&mut self) -> DbResult<FilterExpr> {
        let mut left = self.parse_filter_not()?;
        while self.consume_ci("&&") || self.consume_ci("and") {
            let right = self.parse_filter_not()?;
            left = FilterExpr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_filter_not(&mut self) -> DbResult<FilterExpr> {
        self.skip_ws();
        if self.consume("!") || self.consume_ci("not") {
            let e = self.parse_filter_primary()?;
            Ok(FilterExpr::Not(Box::new(e)))
        } else {
            self.parse_filter_primary()
        }
    }

    fn parse_filter_primary(&mut self) -> DbResult<FilterExpr> {
        self.skip_ws();
        if self.consume("(") {
            let e = self.parse_filter_expr()?;
            self.skip_ws();
            if !self.consume(")") {
                return Err(DbError::internal("expected ')' after filter"));
            }
            self.skip_ws();
            if self.consume_ci("is") {
                self.skip_ws();
                if !self.consume_ci("unknown") {
                    return Err(DbError::internal("expected UNKNOWN after IS"));
                }
                return Ok(FilterExpr::IsUnknown(Box::new(e)));
            }
            return Ok(e);
        }
        if self.consume_ci("exists") {
            self.consume("(");
            let e = self.parse_expr()?;
            self.consume(")");
            return Ok(FilterExpr::Exists(Box::new(e)));
        }
        // Parse comparison: expr op expr
        let left = self.parse_expr()?;
        self.skip_ws();
        // Check for `is unknown`
        if self.starts_with_ci("is") {
            self.consume_ci("is");
            self.skip_ws();
            self.consume_ci("unknown");
            return Ok(FilterExpr::IsUnknown(Box::new(FilterExpr::PathPredicate(
                Box::new(left),
            ))));
        }
        // Handle `starts with` operator: @ starts with "prefix"
        if self.starts_with_ci("starts") {
            self.consume_ci("starts");
            self.skip_ws();
            self.consume_ci("with");
            let right = self.parse_expr()?;
            return Ok(FilterExpr::Comparison(
                CmpOp::Eq,
                Box::new(left),
                Box::new(right),
            ));
        }
        // Handle `like_regex` operator: @ like_regex "pattern" [flag "flags"]
        if self.starts_with_ci("like_regex") {
            self.consume_ci("like_regex");
            let pattern_expr = self.parse_expr()?;
            if let Some(pattern) = jsonpath_string_literal(&pattern_expr) {
                regex::Regex::new(pattern).map_err(|err| {
                    DbError::internal(format!("invalid regular expression: {err}"))
                })?;
            }
            self.skip_ws();
            // Optional `flag "flags"` clause
            if self.starts_with_ci("flag") {
                self.consume_ci("flag");
                let flags_expr = self.parse_expr()?;
                if let Some(flags) = jsonpath_string_literal(&flags_expr) {
                    let has_q = flags.chars().any(|ch| ch == 'q' || ch == 'Q');
                    for flag in flags.chars() {
                        if (flag == 'x' || flag == 'X') && !has_q {
                            return Err(DbError::feature_not_supported(
                                "XQuery \"x\" flag (expanded regular expressions) is not implemented",
                            ));
                        }
                        if !matches!(
                            flag,
                            'i' | 'I' | 'm' | 'M' | 's' | 'S' | 'q' | 'Q' | 'x' | 'X'
                        ) {
                            return Err(DbError::Internal(Box::new(
                                aiondb_core::ErrorReport::new(
                                    aiondb_core::SqlState::InvalidTextRepresentation,
                                    "invalid input syntax for type jsonpath",
                                )
                                .with_client_detail(format!(
                                "Unrecognized flag character \"{flag}\" in LIKE_REGEX predicate."
                            )),
                            )));
                        }
                    }
                }
            }
            return Ok(FilterExpr::PathPredicate(Box::new(left)));
        }
        let op = if self.consume("==") {
            Some(CmpOp::Eq)
        } else if self.consume("!=") || self.consume("<>") {
            Some(CmpOp::Ne)
        } else if self.consume("<=") {
            Some(CmpOp::Le)
        } else if self.consume(">=") {
            Some(CmpOp::Ge)
        } else if self.consume("<") {
            Some(CmpOp::Lt)
        } else if self.consume(">") {
            Some(CmpOp::Gt)
        } else {
            None
        };
        match op {
            Some(op) => {
                let right = self.parse_expr()?;
                Ok(FilterExpr::Comparison(op, Box::new(left), Box::new(right)))
            }
            None => Ok(FilterExpr::PathPredicate(Box::new(left))),
        }
    }

    fn parse_identifier(&mut self) -> String {
        let mut out = String::new();
        while self.pos < self.input.len() {
            let c = self.input.as_bytes()[self.pos];
            if c.is_ascii_alphanumeric() || c == b'_' {
                out.push(c as char);
                self.pos += 1;
                continue;
            }
            if c == b'\\' && self.pos + 1 < self.input.len() {
                self.pos += 1;
                let esc = self.input.as_bytes()[self.pos];
                match esc {
                    b'"' | b'\\' | b'/' => {
                        out.push(esc as char);
                        self.pos += 1;
                    }
                    b't' => {
                        out.push('\t');
                        self.pos += 1;
                    }
                    b'n' => {
                        out.push('\n');
                        self.pos += 1;
                    }
                    b'r' => {
                        out.push('\r');
                        self.pos += 1;
                    }
                    b'x' => {
                        if self.pos + 2 < self.input.len() {
                            let hex = &self.input[self.pos + 1..self.pos + 3];
                            if let Ok(code) = u8::from_str_radix(hex, 16) {
                                out.push(code as char);
                                self.pos += 3;
                                continue;
                            }
                        }
                        out.push('x');
                        self.pos += 1;
                    }
                    b'u' => {
                        if self.pos + 1 < self.input.len()
                            && self.input.as_bytes()[self.pos + 1] == b'{'
                        {
                            let mut cursor = self.pos + 2;
                            while cursor < self.input.len() && self.input.as_bytes()[cursor] != b'}'
                            {
                                cursor += 1;
                            }
                            if cursor < self.input.len() {
                                let hex = &self.input[self.pos + 2..cursor];
                                if let Ok(code) = u32::from_str_radix(hex, 16) {
                                    if let Some(ch) = char::from_u32(code) {
                                        out.push(ch);
                                        self.pos = cursor + 1;
                                        continue;
                                    }
                                }
                            }
                            out.push('u');
                            self.pos += 1;
                        } else if self.pos + 4 < self.input.len() {
                            let hex = &self.input[self.pos + 1..self.pos + 5];
                            if let Ok(code) = u32::from_str_radix(hex, 16) {
                                if let Some(ch) = char::from_u32(code) {
                                    out.push(ch);
                                    self.pos += 5;
                                    continue;
                                }
                            }
                            out.push('u');
                            self.pos += 1;
                        } else {
                            out.push('u');
                            self.pos += 1;
                        }
                    }
                    _ => {
                        out.push(esc as char);
                        self.pos += 1;
                    }
                }
                continue;
            }
            break;
        }
        out
    }

    fn parse_quoted_string(&mut self) -> DbResult<String> {
        if !self.consume("\"") {
            return Err(DbError::internal("expected '\"' in jsonpath"));
        }
        let mut s = String::new();
        while self.pos < self.input.len() {
            let c = self.input.as_bytes()[self.pos];
            if c == b'"' {
                self.pos += 1;
                return Ok(s);
            }
            if c == b'\\' && self.pos + 1 < self.input.len() {
                self.pos += 1;
                let esc = self.input.as_bytes()[self.pos];
                match esc {
                    b'"' | b'\\' | b'/' => s.push(esc as char),
                    b'n' => s.push('\n'),
                    b'r' => s.push('\r'),
                    b't' => s.push('\t'),
                    _ => {
                        s.push('\\');
                        s.push(esc as char);
                    }
                }
                self.pos += 1;
            } else {
                s.push(c as char);
                self.pos += 1;
            }
        }
        Err(DbError::internal("unterminated string in jsonpath"))
    }

    fn parse_number(&mut self) -> DbResult<JV> {
        let bytes = self.input.as_bytes();
        let len = bytes.len();

        // Base-prefixed integers: 0b..., 0o..., 0x... (underscores allowed).
        if self.pos + 1 < len && bytes[self.pos] == b'0' {
            let base = match bytes[self.pos + 1] {
                b'b' | b'B' => Some(2),
                b'o' | b'O' => Some(8),
                b'x' | b'X' => Some(16),
                _ => None,
            };
            if let Some(base) = base {
                self.pos += 2;
                let digits_start = self.pos;
                while self.pos < len {
                    let ch = bytes[self.pos];
                    if ch == b'_' || ch.is_ascii_hexdigit() {
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
                let raw = &self.input[digits_start..self.pos];
                if !is_valid_jsonpath_digit_segment(raw, false, base) {
                    return Err(DbError::internal("invalid number in jsonpath"));
                }
                let cleaned: String = raw.chars().filter(|ch| *ch != '_').collect();
                if cleaned.is_empty() {
                    return Err(DbError::internal("invalid number in jsonpath"));
                }
                let value = i64::from_str_radix(&cleaned, base)
                    .map_err(|_| DbError::internal("invalid number in jsonpath"))?;
                return Ok(JV::Number(value.into()));
            }
        }

        let mut int_part = String::new();
        while self.pos < len && (bytes[self.pos].is_ascii_digit() || bytes[self.pos] == b'_') {
            int_part.push(bytes[self.pos] as char);
            self.pos += 1;
        }

        let mut frac_part = String::new();
        let mut has_dot = false;
        if self.pos < len
            && bytes[self.pos] == b'.'
            && (self.pos + 1 >= len
                || bytes[self.pos + 1] == b'e'
                || bytes[self.pos + 1] == b'E'
                || !bytes[self.pos + 1].is_ascii_alphabetic())
        {
            has_dot = true;
            self.pos += 1;
            while self.pos < len && (bytes[self.pos].is_ascii_digit() || bytes[self.pos] == b'_') {
                frac_part.push(bytes[self.pos] as char);
                self.pos += 1;
            }
        }

        let mut exp_part = String::new();
        let mut exp_sign = None;
        if self.pos < len && (bytes[self.pos] == b'e' || bytes[self.pos] == b'E') {
            self.pos += 1;
            if self.pos < len && (bytes[self.pos] == b'+' || bytes[self.pos] == b'-') {
                exp_sign = Some(bytes[self.pos] as char);
                self.pos += 1;
            }
            while self.pos < len && (bytes[self.pos].is_ascii_digit() || bytes[self.pos] == b'_') {
                exp_part.push(bytes[self.pos] as char);
                self.pos += 1;
            }
            if exp_part.is_empty() {
                return Err(DbError::internal("invalid number in jsonpath"));
            }
        }

        if int_part.is_empty() && !(has_dot && !frac_part.is_empty()) {
            return Err(DbError::internal("invalid number in jsonpath"));
        }
        if !int_part.is_empty() && !is_valid_jsonpath_digit_segment(&int_part, false, 10) {
            return Err(DbError::internal("invalid number in jsonpath"));
        }
        if has_dot && !is_valid_jsonpath_digit_segment(&frac_part, true, 10) {
            return Err(DbError::internal("invalid number in jsonpath"));
        }
        if !exp_part.is_empty() && !is_valid_jsonpath_digit_segment(&exp_part, false, 10) {
            return Err(DbError::internal("invalid number in jsonpath"));
        }

        let int_clean: String = int_part.chars().filter(|ch| *ch != '_').collect();
        let frac_clean: String = frac_part.chars().filter(|ch| *ch != '_').collect();
        let exp_clean: String = exp_part.chars().filter(|ch| *ch != '_').collect();

        // PG jsonpath rejects plain decimal integers with leading zeroes.
        if !has_dot
            && exp_part.is_empty()
            && int_clean.len() > 1
            && int_clean.starts_with('0')
            && int_clean.chars().all(|ch| ch.is_ascii_digit())
        {
            return Err(DbError::internal("trailing junk after numeric literal"));
        }

        let mut normalized = String::new();
        normalized.push_str(&int_clean);
        if has_dot {
            normalized.push('.');
            normalized.push_str(&frac_clean);
        }
        if !exp_part.is_empty() {
            normalized.push('e');
            if let Some(sign) = exp_sign {
                normalized.push(sign);
            }
            normalized.push_str(&exp_clean);
        }

        if has_dot || !exp_part.is_empty() {
            let f: f64 = normalized.parse().map_err(|_| {
                DbError::internal(format!("invalid number in jsonpath: {normalized}"))
            })?;
            Ok(serde_json::Number::from_f64(f).map_or(JV::Null, JV::Number))
        } else {
            let i: i64 = normalized.parse().map_err(|_| {
                DbError::internal(format!("invalid number in jsonpath: {normalized}"))
            })?;
            Ok(JV::Number(i.into()))
        }
    }
}

fn parse_jsonpath(input: &str) -> DbResult<JsonPath> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(DbError::Internal(Box::new(aiondb_core::ErrorReport::new(
            aiondb_core::SqlState::InvalidTextRepresentation,
            "invalid input syntax for type jsonpath: \"\"",
        ))));
    }
    let mut parser = PathParser::new(trimmed);
    parser.parse_path()
}

fn normalize_jsonpath_text(input: &str) -> DbResult<String> {
    let path = parse_jsonpath(input)?;
    validate_jsonpath(&path, input)?;
    Ok(render_jsonpath(&path))
}

fn validate_jsonpath(path: &JsonPath, input: &str) -> DbResult<()> {
    validate_jsonpath_expr(&path.expr, false, false, input.trim_start(), true)
}

fn validate_jsonpath_expr(
    expr: &PathExpr,
    allow_current: bool,
    allow_last: bool,
    input: &str,
    is_root: bool,
) -> DbResult<()> {
    match expr {
        PathExpr::Root | PathExpr::Literal(_) | PathExpr::Variable(_) => Ok(()),
        PathExpr::Current => {
            if allow_current {
                Ok(())
            } else {
                Err(DbError::Bind(Box::new(aiondb_core::ErrorReport::new(
                    aiondb_core::SqlState::SyntaxError,
                    "@ is not allowed in root expressions",
                ))))
            }
        }
        PathExpr::Member(base, _)
        | PathExpr::WildcardMember(base)
        | PathExpr::WildcardArray(base)
        | PathExpr::RecursiveDescend(base, _) => {
            validate_jsonpath_expr(base, allow_current, allow_last, input, false)
        }
        PathExpr::ArrayIndex(base, subscripts) => {
            validate_jsonpath_expr(base, allow_current, allow_last, input, false)?;
            for subscript in subscripts {
                match subscript {
                    ArraySubscript::Index(expr) => {
                        validate_jsonpath_expr(expr, true, true, input, false)?;
                    }
                    ArraySubscript::Range(lo, hi) => {
                        validate_jsonpath_expr(lo, true, true, input, false)?;
                        validate_jsonpath_expr(hi, true, true, input, false)?;
                    }
                }
            }
            Ok(())
        }
        PathExpr::Filter(base, filter) => {
            validate_jsonpath_expr(base, allow_current, allow_last, input, false)?;
            validate_json_filter(filter, input, allow_last)
        }
        PathExpr::Method(base, method, args) => {
            if method.eq_ignore_ascii_case("last") && !allow_last {
                return Err(DbError::Bind(Box::new(aiondb_core::ErrorReport::new(
                    aiondb_core::SqlState::SyntaxError,
                    "LAST is allowed only in array subscripts",
                ))));
            }
            validate_jsonpath_expr(base, allow_current, allow_last, input, false)?;
            if is_root
                && matches!(base.as_ref(), PathExpr::Literal(JV::Number(n)) if n.as_i64().is_some())
                && !input.trim_start().starts_with('(')
            {
                let snippet = input.trim_start().chars().take(3).collect::<String>();
                return Err(DbError::Internal(Box::new(aiondb_core::ErrorReport::new(
                    aiondb_core::SqlState::InvalidTextRepresentation,
                    format!(
                        "trailing junk after numeric literal at or near \"{snippet}\" of jsonpath input"
                    ),
                ))));
            }
            for arg in args {
                validate_jsonpath_expr(arg, true, allow_last, input, false)?;
            }
            Ok(())
        }
        PathExpr::Predicate(filter) => validate_json_filter(filter, input, allow_last),
        PathExpr::BinaryArith(left, _, right) => {
            validate_jsonpath_expr(left, allow_current, allow_last, input, false)?;
            validate_jsonpath_expr(right, allow_current, allow_last, input, false)
        }
        PathExpr::UnaryMinus(expr) | PathExpr::UnaryPlus(expr) => {
            validate_jsonpath_expr(expr, allow_current, allow_last, input, false)
        }
    }
}

fn validate_json_filter(filter: &FilterExpr, input: &str, allow_last: bool) -> DbResult<()> {
    match filter {
        FilterExpr::Comparison(_, left, right) => {
            validate_jsonpath_expr(left, true, allow_last, input, false)?;
            validate_jsonpath_expr(right, true, allow_last, input, false)
        }
        FilterExpr::And(left, right) | FilterExpr::Or(left, right) => {
            validate_json_filter(left, input, allow_last)?;
            validate_json_filter(right, input, allow_last)
        }
        FilterExpr::Not(inner) | FilterExpr::IsUnknown(inner) => {
            validate_json_filter(inner, input, allow_last)
        }
        FilterExpr::Exists(expr) | FilterExpr::PathPredicate(expr) => {
            validate_jsonpath_expr(expr, true, allow_last, input, false)
        }
    }
}

fn jsonpath_string_literal(expr: &PathExpr) -> Option<&str> {
    match expr {
        PathExpr::Literal(JV::String(value)) => Some(value.as_str()),
        _ => None,
    }
}

fn is_valid_jsonpath_digit_segment(segment: &str, allow_empty: bool, base: u32) -> bool {
    if segment.is_empty() {
        return allow_empty;
    }
    if segment.starts_with('_') || segment.ends_with('_') || segment.contains("__") {
        return false;
    }
    segment
        .chars()
        .all(|ch| if ch == '_' { true } else { ch.is_digit(base) })
}

fn render_jsonpath(path: &JsonPath) -> String {
    let rendered = render_jsonpath_expr(&path.expr, 0);
    match path.mode {
        PathMode::Lax => rendered,
        PathMode::Strict => format!("strict {rendered}"),
    }
}

fn render_jsonpath_expr(expr: &PathExpr, parent_prec: u8) -> String {
    let rendered = match expr {
        PathExpr::Root => "$".to_owned(),
        PathExpr::Current => "@".to_owned(),
        PathExpr::Literal(v) => render_jsonpath_literal(v),
        PathExpr::Variable(name) => format!("${}", render_jsonpath_quoted(name)),
        PathExpr::Member(base, key) => format!(
            "{}.{}",
            render_jsonpath_postfix_base(base),
            render_jsonpath_quoted(key)
        ),
        PathExpr::WildcardMember(base) => format!("{}.*", render_jsonpath_postfix_base(base)),
        PathExpr::ArrayIndex(base, subscripts) => {
            let subs = subscripts
                .iter()
                .map(render_array_subscript)
                .collect::<Vec<_>>()
                .join(",");
            format!("{}[{subs}]", render_jsonpath_postfix_base(base))
        }
        PathExpr::WildcardArray(base) => format!("{}[*]", render_jsonpath_postfix_base(base)),
        PathExpr::RecursiveDescend(base, depth) => {
            let suffix = match depth {
                None => String::new(),
                Some((lo, hi)) if *lo == u32::MAX && *hi == u32::MAX => "{last}".to_owned(),
                Some((lo, hi)) if lo == hi => format!("{{{lo}}}"),
                Some((lo, hi)) if *hi == u32::MAX => format!("{{{lo} to last}}"),
                Some((lo, hi)) => format!("{{{lo} to {hi}}}"),
            };
            format!("{}.**{suffix}", render_jsonpath_postfix_base(base))
        }
        PathExpr::Filter(base, filter) => {
            format!(
                "{}?({})",
                render_jsonpath_postfix_base(base),
                render_filter(filter)
            )
        }
        PathExpr::Method(base, method, args) => {
            if method.eq_ignore_ascii_case("last") && args.is_empty() {
                "last".to_owned()
            } else {
                let rendered_args = args
                    .iter()
                    .map(|arg| render_jsonpath_expr(arg, 0))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "{}.{}({rendered_args})",
                    render_jsonpath_postfix_base(base),
                    method
                )
            }
        }
        PathExpr::BinaryArith(left, op, right) => {
            let prec = match op {
                ArithOp::Add | ArithOp::Sub => 1,
                ArithOp::Mul | ArithOp::Div | ArithOp::Mod => 2,
            };
            let l = render_jsonpath_expr(left, prec);
            let r = render_jsonpath_expr(right, prec + 1);
            let op = match op {
                ArithOp::Add => "+",
                ArithOp::Sub => "-",
                ArithOp::Mul => "*",
                ArithOp::Div => "/",
                ArithOp::Mod => "%",
            };
            format!("{l} {op} {r}")
        }
        PathExpr::UnaryMinus(expr) => format!("-{}", render_jsonpath_expr(expr, 3)),
        PathExpr::UnaryPlus(expr) => format!("+{}", render_jsonpath_expr(expr, 3)),
        PathExpr::Predicate(filter) => format!("({})", render_filter(filter)),
    };
    if needs_jsonpath_parens(expr, parent_prec) {
        format!("({rendered})")
    } else {
        rendered
    }
}

fn render_jsonpath_postfix_base(expr: &PathExpr) -> String {
    match expr {
        PathExpr::Literal(JV::Number(_)) => format!("({})", render_jsonpath_expr(expr, 0)),
        PathExpr::BinaryArith(..)
        | PathExpr::UnaryMinus(..)
        | PathExpr::UnaryPlus(..)
        | PathExpr::Filter(..)
        | PathExpr::Predicate(..) => format!("({})", render_jsonpath_expr(expr, 0)),
        _ => render_jsonpath_expr(expr, 4),
    }
}

fn needs_jsonpath_parens(expr: &PathExpr, parent_prec: u8) -> bool {
    matches!(expr, PathExpr::BinaryArith(_, _, _)) && parent_prec <= 2
}

fn render_array_subscript(subscript: &ArraySubscript) -> String {
    match subscript {
        ArraySubscript::Index(expr) => render_jsonpath_expr(expr, 0),
        ArraySubscript::Range(lo, hi) => {
            format!(
                "{} to {}",
                render_jsonpath_expr(lo, 0),
                render_jsonpath_expr(hi, 0)
            )
        }
    }
}

fn render_filter(filter: &FilterExpr) -> String {
    match filter {
        FilterExpr::Comparison(op, left, right) => format!(
            "{} {} {}",
            render_jsonpath_expr(left, 0),
            match op {
                CmpOp::Eq => "==",
                CmpOp::Ne => "!=",
                CmpOp::Lt => "<",
                CmpOp::Le => "<=",
                CmpOp::Gt => ">",
                CmpOp::Ge => ">=",
            },
            render_jsonpath_expr(right, 0)
        ),
        FilterExpr::And(left, right) => {
            format!("{} && {}", render_filter(left), render_filter(right))
        }
        FilterExpr::Or(left, right) => {
            format!("{} || {}", render_filter(left), render_filter(right))
        }
        FilterExpr::Not(inner) => format!("!{}", render_filter(inner)),
        FilterExpr::Exists(expr) => format!("exists ({})", render_jsonpath_expr(expr, 0)),
        FilterExpr::IsUnknown(inner) => format!("({} is unknown)", render_filter(inner)),
        FilterExpr::PathPredicate(expr) => render_jsonpath_expr(expr, 0),
    }
}

fn render_jsonpath_literal(value: &JV) -> String {
    match value {
        JV::Null => "null".to_owned(),
        JV::Bool(true) => "true".to_owned(),
        JV::Bool(false) => "false".to_owned(),
        JV::Number(n) => {
            if let Some(int) = n.as_i64() {
                int.to_string()
            } else if let Some(uint) = n.as_u64() {
                uint.to_string()
            } else if let Some(float) = n.as_f64() {
                if float.is_finite() && float.fract() == 0.0 {
                    format!("{float:.0}")
                } else {
                    n.to_string()
                }
            } else {
                n.to_string()
            }
        }
        JV::String(s) => render_jsonpath_quoted(s),
        other => other.to_string(),
    }
}

fn render_jsonpath_quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    // Bulk-copy the chunks between escape triggers via `push_str`
    // instead of per-`char` dispatch. The five trigger bytes
    // (`\\`, `"`, `\n`, `\r`, `\t`) are all single-byte ASCII and
    // never collide with UTF-8 leading bytes (>= 0x80), so slicing
    // on raw byte indices remains at valid char boundaries.
    let bytes = value.as_bytes();
    let mut last = 0usize;
    for (idx, &b) in bytes.iter().enumerate() {
        let escape = match b {
            b'\\' => "\\\\",
            b'"' => "\\\"",
            b'\n' => "\\n",
            b'\r' => "\\r",
            b'\t' => "\\t",
            _ => continue,
        };
        if idx > last {
            out.push_str(&value[last..idx]);
        }
        out.push_str(escape);
        last = idx + 1;
    }
    if last < bytes.len() {
        out.push_str(&value[last..]);
    }
    out.push('"');
    out
}

// ── JSONPath Evaluation ─────────────────────────────────────────────

struct EvalCtx<'a> {
    root: &'a JV,
    vars: &'a JV,
    mode: PathMode,
}

type JsonPathValue<'a> = Cow<'a, JV>;

fn clone_json_path_value<'a>(value: &JsonPathValue<'a>) -> JsonPathValue<'a> {
    match value {
        Cow::Borrowed(v) => Cow::Borrowed(v),
        Cow::Owned(v) => Cow::Owned(v.clone()),
    }
}

fn eval_path(target: &JV, path: &JsonPath, vars: &JV) -> Vec<JV> {
    let ctx = EvalCtx {
        root: target,
        vars,
        mode: path.mode,
    };
    eval_expr(&path.expr, target, &ctx)
        .into_iter()
        .map(Cow::into_owned)
        .collect()
}

fn eval_expr<'a>(expr: &PathExpr, current: &'a JV, ctx: &EvalCtx<'a>) -> Vec<JsonPathValue<'a>> {
    match expr {
        PathExpr::Root => vec![Cow::Borrowed(ctx.root)],
        PathExpr::Current => vec![Cow::Borrowed(current)],
        PathExpr::Literal(v) => vec![Cow::Owned(v.clone())],
        PathExpr::Variable(name) => {
            if let JV::Object(map) = ctx.vars {
                match map.get(name) {
                    Some(v) => vec![Cow::Borrowed(v)],
                    None => vec![],
                }
            } else {
                vec![]
            }
        }
        PathExpr::Member(base, key) => {
            let base_vals = eval_expr(base, current, ctx);
            let mut results = Vec::new();
            for bv in &base_vals {
                match bv {
                    Cow::Borrowed(JV::Object(map)) => {
                        if let Some(v) = map.get(key) {
                            results.push(Cow::Borrowed(v));
                        }
                    }
                    Cow::Owned(JV::Object(map)) => {
                        if let Some(v) = map.get(key) {
                            results.push(Cow::Owned(v.clone()));
                        }
                    }
                    Cow::Borrowed(JV::Array(arr)) if ctx.mode == PathMode::Lax => {
                        for elem in arr {
                            if let JV::Object(map) = elem {
                                if let Some(v) = map.get(key) {
                                    results.push(Cow::Borrowed(v));
                                }
                            }
                        }
                    }
                    Cow::Owned(JV::Array(arr)) if ctx.mode == PathMode::Lax => {
                        for elem in arr {
                            if let JV::Object(map) = elem {
                                if let Some(v) = map.get(key) {
                                    results.push(Cow::Owned(v.clone()));
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            results
        }
        PathExpr::WildcardMember(base) => {
            let base_vals = eval_expr(base, current, ctx);
            let mut results = Vec::new();
            for bv in &base_vals {
                match bv {
                    Cow::Borrowed(JV::Object(map)) => {
                        results.extend(map.values().map(Cow::Borrowed));
                    }
                    Cow::Owned(JV::Object(map)) => {
                        results.extend(map.values().cloned().map(Cow::Owned));
                    }
                    Cow::Borrowed(JV::Array(arr)) if ctx.mode == PathMode::Lax => {
                        for elem in arr {
                            if let JV::Object(map) = elem {
                                results.extend(map.values().map(Cow::Borrowed));
                            }
                        }
                    }
                    Cow::Owned(JV::Array(arr)) if ctx.mode == PathMode::Lax => {
                        for elem in arr {
                            if let JV::Object(map) = elem {
                                results.extend(map.values().cloned().map(Cow::Owned));
                            }
                        }
                    }
                    _ => {}
                }
            }
            results
        }
        PathExpr::WildcardArray(base) => {
            let base_vals = eval_expr(base, current, ctx);
            let mut results = Vec::new();
            for bv in &base_vals {
                match bv {
                    Cow::Borrowed(JV::Array(arr)) => {
                        results.extend(arr.iter().map(Cow::Borrowed));
                    }
                    Cow::Owned(JV::Array(arr)) => {
                        results.extend(arr.iter().cloned().map(Cow::Owned));
                    }
                    _ if ctx.mode == PathMode::Lax => {
                        results.push(clone_json_path_value(bv));
                    }
                    _ => {}
                }
            }
            results
        }
        PathExpr::ArrayIndex(base, subscripts) => {
            let base_vals = eval_expr(base, current, ctx);
            let mut results = Vec::new();
            for bv in &base_vals {
                match bv {
                    Cow::Borrowed(JV::Array(arr)) => {
                        let arr_len = arr.len();
                        for sub in subscripts {
                            match sub {
                                ArraySubscript::Index(idx_expr) => {
                                    let idx = eval_index_expr(idx_expr, current, ctx, arr_len);
                                    if let Some(i) = idx {
                                        if i < arr_len {
                                            results.push(Cow::Borrowed(&arr[i]));
                                        }
                                    }
                                }
                                ArraySubscript::Range(lo_expr, hi_expr) => {
                                    let lo = eval_index_expr(lo_expr, current, ctx, arr_len);
                                    let hi = eval_index_expr(hi_expr, current, ctx, arr_len);
                                    if let (Some(lo), Some(hi)) = (lo, hi) {
                                        let lo = lo.min(arr_len);
                                        let hi = (hi + 1).min(arr_len);
                                        for item in arr.iter().take(hi).skip(lo) {
                                            results.push(Cow::Borrowed(item));
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Cow::Owned(JV::Array(arr)) => {
                        let arr_len = arr.len();
                        for sub in subscripts {
                            match sub {
                                ArraySubscript::Index(idx_expr) => {
                                    let idx = eval_index_expr(idx_expr, current, ctx, arr_len);
                                    if let Some(i) = idx {
                                        if i < arr_len {
                                            results.push(Cow::Owned(arr[i].clone()));
                                        }
                                    }
                                }
                                ArraySubscript::Range(lo_expr, hi_expr) => {
                                    let lo = eval_index_expr(lo_expr, current, ctx, arr_len);
                                    let hi = eval_index_expr(hi_expr, current, ctx, arr_len);
                                    if let (Some(lo), Some(hi)) = (lo, hi) {
                                        let lo = lo.min(arr_len);
                                        let hi = (hi + 1).min(arr_len);
                                        for item in arr.iter().take(hi).skip(lo) {
                                            results.push(Cow::Owned(item.clone()));
                                        }
                                    }
                                }
                            }
                        }
                    }
                    _ if ctx.mode == PathMode::Lax => results.push(clone_json_path_value(bv)),
                    _ => {}
                }
            }
            results
        }
        PathExpr::RecursiveDescend(base, depth) => {
            let base_vals = eval_expr(base, current, ctx);
            let (min_d, max_d) = depth.unwrap_or((0, u32::MAX));
            let mut results = Vec::new();
            for bv in &base_vals {
                match bv {
                    Cow::Borrowed(v) => collect_recursive(v, 0, min_d, max_d, &mut results),
                    Cow::Owned(v) => collect_recursive_owned(v, 0, min_d, max_d, &mut results),
                }
            }
            results
        }
        PathExpr::Filter(base, filter) => {
            let base_vals = eval_expr(base, current, ctx);
            let mut results = Vec::new();
            for bv in &base_vals {
                if eval_filter(filter, bv.as_ref(), ctx) == Some(true) {
                    results.push(clone_json_path_value(bv));
                }
            }
            results
        }
        PathExpr::Method(base, method, _args) => {
            let base_vals = eval_expr(base, current, ctx);
            let mut results = Vec::new();
            for bv in &base_vals {
                match method.as_str() {
                    "type" => {
                        let t = match bv.as_ref() {
                            JV::Null => "null",
                            JV::Bool(_) => "boolean",
                            JV::Number(_) => "number",
                            JV::String(_) => "string",
                            JV::Array(_) => "array",
                            JV::Object(_) => "object",
                        };
                        results.push(Cow::Owned(JV::String(t.to_owned())));
                    }
                    "size" => match bv.as_ref() {
                        JV::Array(arr) => {
                            let len = i64::try_from(arr.len()).unwrap_or(i64::MAX);
                            results.push(Cow::Owned(JV::Number(len.into())));
                        }
                        _ if ctx.mode == PathMode::Lax => {
                            results.push(Cow::Owned(JV::Number(1.into())));
                        }
                        _ => {}
                    },
                    "double" => {
                        if let Some(n) = json_to_f64(bv) {
                            if let Some(jn) = serde_json::Number::from_f64(n) {
                                results.push(Cow::Owned(JV::Number(jn)));
                            }
                        }
                    }
                    "ceiling" => {
                        if let Some(n) = json_to_f64(bv) {
                            if let Some(jn) = serde_json::Number::from_f64(n.ceil()) {
                                results.push(Cow::Owned(JV::Number(jn)));
                            }
                        }
                    }
                    "floor" => {
                        if let Some(n) = json_to_f64(bv) {
                            if let Some(jn) = serde_json::Number::from_f64(n.floor()) {
                                results.push(Cow::Owned(JV::Number(jn)));
                            }
                        }
                    }
                    "abs" => {
                        if let Some(n) = json_to_f64(bv) {
                            if let Some(jn) = serde_json::Number::from_f64(n.abs()) {
                                results.push(Cow::Owned(JV::Number(jn)));
                            }
                        }
                    }
                    "keyvalue" => {
                        if let JV::Object(map) = bv.as_ref() {
                            for (k, v) in map {
                                let mut obj = serde_json::Map::new();
                                obj.insert("key".to_owned(), JV::String(k.clone()));
                                obj.insert("value".to_owned(), v.clone());
                                obj.insert("id".to_owned(), JV::Number(0.into()));
                                results.push(Cow::Owned(JV::Object(obj)));
                            }
                        }
                    }
                    "last" => {
                        // `last` in array subscript context evaluates to
                        // array length - 1 of the current array.  But
                        // here it's used as method; just return the value.
                        results.push(clone_json_path_value(bv));
                    }
                    _ => results.push(clone_json_path_value(bv)),
                }
            }
            results
        }
        PathExpr::BinaryArith(left, op, right) => {
            let left_vals = eval_expr(left, current, ctx);
            let right_vals = eval_expr(right, current, ctx);
            let mut results = Vec::new();
            for lv in &left_vals {
                for rv in &right_vals {
                    if let (Some(l), Some(r)) = (json_to_f64(lv), json_to_f64(rv)) {
                        let res = match op {
                            ArithOp::Add => l + r,
                            ArithOp::Sub => l - r,
                            ArithOp::Mul => l * r,
                            ArithOp::Div => {
                                if r == 0.0 {
                                    continue;
                                }
                                l / r
                            }
                            ArithOp::Mod => {
                                if r == 0.0 {
                                    continue;
                                }
                                l % r
                            }
                        };
                        // Use integer if result is whole number
                        if let Some(as_i64) = whole_f64_to_i64(res) {
                            results.push(Cow::Owned(JV::Number(as_i64.into())));
                        } else if let Some(n) = serde_json::Number::from_f64(res) {
                            results.push(Cow::Owned(JV::Number(n)));
                        }
                    }
                }
            }
            results
        }
        PathExpr::UnaryMinus(e) => {
            let vals = eval_expr(e, current, ctx);
            vals.into_iter()
                .filter_map(|v| {
                    json_to_f64(v.as_ref()).and_then(|n| {
                        let neg = -n;
                        if let Some(as_i64) = whole_f64_to_i64(neg) {
                            Some(Cow::Owned(JV::Number(as_i64.into())))
                        } else {
                            serde_json::Number::from_f64(neg)
                                .map(JV::Number)
                                .map(Cow::Owned)
                        }
                    })
                })
                .collect()
        }
        PathExpr::UnaryPlus(e) => eval_expr(e, current, ctx),
        PathExpr::Predicate(filter) => {
            let value = eval_filter(filter, current, ctx).unwrap_or(false);
            vec![Cow::Owned(JV::Bool(value))]
        }
    }
}

fn eval_index_expr(expr: &PathExpr, current: &JV, ctx: &EvalCtx, arr_len: usize) -> Option<usize> {
    // Special handling for `last` keyword
    if let PathExpr::Method(inner, method, args) = expr {
        if method == "last" && args.is_empty() {
            if let PathExpr::Current = inner.as_ref() {
                return if arr_len == 0 {
                    None
                } else {
                    Some(arr_len - 1)
                };
            }
        }
    }
    // Also handle `last` in arithmetic expressions
    let vals = eval_expr_with_last(expr, current, ctx, arr_len);
    vals.first().and_then(|v| {
        json_to_f64(v).map(|f| {
            let i = floor_clamped_f64_to_i64(f);
            if i < 0 {
                0usize
            } else {
                usize::try_from(i).unwrap_or(usize::MAX)
            }
        })
    })
}

/// Like `eval_expr` but substitutes `last` with the array length - 1.
fn eval_expr_with_last<'a>(
    expr: &PathExpr,
    current: &'a JV,
    ctx: &EvalCtx<'a>,
    arr_len: usize,
) -> Vec<JsonPathValue<'a>> {
    match expr {
        PathExpr::Method(inner, method, args) if method == "last" && args.is_empty() => {
            if let PathExpr::Current = inner.as_ref() {
                if arr_len == 0 {
                    return vec![];
                }
                // arr_len >= 1 here; arr_len - 1 fits in usize. JSON arrays
                // cannot exceed physical memory, so the cast to i64 is safe
                // in practice, but we use try_from to be explicit.
                let last_idx = i64::try_from(arr_len - 1).unwrap_or(i64::MAX);
                return vec![Cow::Owned(JV::Number(last_idx.into()))];
            }
            // If inner is not @, evaluate the inner expression and then
            // the `last` expression refers to array subscript context.
            let inner_vals = eval_expr_with_last(inner, current, ctx, arr_len);
            // `last` applied as method: evaluate as arr_len - 1
            if arr_len == 0 {
                return vec![];
            }
            // Actually for `last` as a standalone, just return arr_len-1
            if matches!(inner.as_ref(), PathExpr::Current) {
                let last_idx = i64::try_from(arr_len - 1).unwrap_or(i64::MAX);
                return vec![Cow::Owned(JV::Number(last_idx.into()))];
            }
            inner_vals
        }
        PathExpr::Predicate(filter) => {
            let value = eval_filter(filter, current, ctx).unwrap_or(false);
            vec![Cow::Owned(JV::Bool(value))]
        }
        PathExpr::BinaryArith(left, op, right) => {
            let left_vals = eval_expr_with_last(left, current, ctx, arr_len);
            let right_vals = eval_expr_with_last(right, current, ctx, arr_len);
            let mut results = Vec::new();
            for lv in &left_vals {
                for rv in &right_vals {
                    if let (Some(l), Some(r)) = (json_to_f64(lv), json_to_f64(rv)) {
                        let res = match op {
                            ArithOp::Add => l + r,
                            ArithOp::Sub => l - r,
                            ArithOp::Mul => l * r,
                            ArithOp::Div => {
                                if r == 0.0 {
                                    continue;
                                }
                                l / r
                            }
                            ArithOp::Mod => {
                                if r == 0.0 {
                                    continue;
                                }
                                l % r
                            }
                        };
                        if let Some(as_i64) = whole_f64_to_i64(res) {
                            results.push(Cow::Owned(JV::Number(as_i64.into())));
                        } else if let Some(n) = serde_json::Number::from_f64(res) {
                            results.push(Cow::Owned(JV::Number(n)));
                        }
                    }
                }
            }
            results
        }
        // For `last` used as a standalone identifier (not method on @)
        PathExpr::Literal(JV::String(s)) if s == "last" => {
            if arr_len == 0 {
                vec![]
            } else {
                let last_idx = i64::try_from(arr_len - 1).unwrap_or(i64::MAX);
                vec![Cow::Owned(JV::Number(last_idx.into()))]
            }
        }
        _ => eval_expr(expr, current, ctx),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonpath_cast_canonicalizes_basic_paths() {
        assert_eq!(
            normalize_jsonpath_text("$.a").expect("normalize"),
            "$.\"a\""
        );
        assert_eq!(normalize_jsonpath_text("1e3").expect("normalize"), "1000");
        assert_eq!(
            normalize_jsonpath_text("1.2.type()").expect("normalize"),
            "(1.2).type()"
        );
    }

    #[test]
    fn jsonpath_cast_rejects_invalid_root_current() {
        let err = normalize_jsonpath_text("@ + 1").expect_err("should fail");
        assert_eq!(err.sqlstate(), aiondb_core::SqlState::SyntaxError);
    }

    #[test]
    fn jsonpath_cast_rejects_invalid_integer_method_root() {
        let err = normalize_jsonpath_text("1.type()").expect_err("should fail");
        assert_eq!(
            err.sqlstate(),
            aiondb_core::SqlState::InvalidTextRepresentation
        );
    }

    #[test]
    fn jsonpath_cast_function_returns_canonical_text() {
        let value = eval_jsonpath_cast(&[Value::Text("$.a".to_owned())]).expect("cast");
        assert_eq!(value, Value::Text("$.\"a\"".to_owned()));
    }

    /// jsonpath parser. `parse_unary` recurses on each leading `-`/`+` without
    /// going through the `parse_expr` depth guard, so an attacker-controlled
    /// pattern like `------...$` could previously walk the parser into
    /// unbounded recursion.
    #[test]
    fn audit_jsonpath_unary_recursion_is_bounded() {
        let mut pattern = String::from("lax ");
        // 20_000 leading minus signs; with a stack frame of a few hundred
        // bytes this blows past the default 8 MiB test thread stack.
        pattern.extend(std::iter::repeat('-').take(20_000));
        pattern.push('$');
        let outcome = std::thread::Builder::new()
            // Use the default test-thread stack (2 MiB) to make sure we do
            // not accidentally mask a real stack overflow with a larger one.
            .stack_size(2 * 1024 * 1024)
            .spawn(move || {
                let _ = parse_jsonpath(&pattern);
            })
            .expect("spawn")
            .join();
        assert!(
            outcome.is_ok(),
            "jsonpath parse_unary recursed until stack overflow: {outcome:?}",
        );
    }

    /// Security audit: deeply nested parenthesised filter expressions inside
    /// a `?(...)` clause also recurse through `parse_filter_primary` without
    /// hitting the `parse_expr` depth guard.
    #[test]
    fn audit_jsonpath_filter_parens_recursion_is_bounded() {
        let mut pattern = String::from("$ ?(");
        let n = 20_000usize;
        pattern.extend(std::iter::repeat('(').take(n));
        pattern.push_str("@ == 1");
        pattern.extend(std::iter::repeat(')').take(n));
        pattern.push(')');
        let outcome = std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(move || {
                let _ = parse_jsonpath(&pattern);
            })
            .expect("spawn")
            .join();
        assert!(
            outcome.is_ok(),
            "jsonpath filter-paren parser overflowed the stack: {outcome:?}",
        );
    }
}
