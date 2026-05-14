//! Parsing helpers for CHECK constraints declared inline on CREATE TABLE
//! columns.
//! Pure: operates on `aiondb_parser` tokens/Expr and returns data only.

use aiondb_parser::tokens::{tokens_to_sql, Token, TokenKind};
use aiondb_parser::{keywords::Keyword, parse_expression, Expr};

pub fn parse_inline_check_constraints_from_column_sql(
    column_sql: &str,
) -> Vec<(Option<String>, Expr)> {
    let Ok(tokens) = aiondb_parser::lexer::lex_sql(column_sql) else {
        return Vec::new();
    };

    let mut constraints = Vec::new();
    let mut idx = 0usize;
    while idx < tokens.len() {
        match &tokens[idx].kind {
            TokenKind::Keyword(Keyword::Constraint) => {
                let Some(name_token) = tokens.get(idx + 1) else {
                    idx += 1;
                    continue;
                };
                let Some(constraint_name) = token_identifier_name(&name_token.kind) else {
                    idx += 1;
                    continue;
                };
                let check_idx = idx + 2;
                if !matches!(
                    tokens.get(check_idx).map(|token| &token.kind),
                    Some(TokenKind::Keyword(Keyword::Check))
                ) {
                    idx += 1;
                    continue;
                }
                if let Some((expr, next_idx)) = parse_check_expr_from_tokens(&tokens, check_idx) {
                    constraints.push((Some(constraint_name), expr));
                    idx = next_idx;
                    continue;
                }
                idx += 1;
            }
            TokenKind::Keyword(Keyword::Check) => {
                if let Some((expr, next_idx)) = parse_check_expr_from_tokens(&tokens, idx) {
                    constraints.push((None, expr));
                    idx = next_idx;
                    continue;
                }
                idx += 1;
            }
            _ => idx += 1,
        }
    }
    constraints
}

pub fn token_identifier_name(kind: &TokenKind) -> Option<String> {
    match kind {
        TokenKind::Identifier(name) => Some(name.clone()),
        TokenKind::Keyword(keyword) => Some(keyword.name().to_owned()),
        _ => None,
    }
}

pub fn parse_check_expr_from_tokens(tokens: &[Token], check_idx: usize) -> Option<(Expr, usize)> {
    let lparen_idx = check_idx + 1;
    if !matches!(
        tokens.get(lparen_idx).map(|token| &token.kind),
        Some(TokenKind::LParen)
    ) {
        return None;
    }

    let mut depth = 0usize;
    for idx in lparen_idx..tokens.len() {
        match tokens[idx].kind {
            TokenKind::LParen => depth = depth.saturating_add(1),
            TokenKind::RParen => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    let expr_tokens = &tokens[lparen_idx + 1..idx];
                    let expr_sql = tokens_to_sql(expr_tokens);
                    let expr = parse_expression(&expr_sql).ok()?;
                    return Some((expr, idx + 1));
                }
            }
            _ => {}
        }
    }

    None
}
