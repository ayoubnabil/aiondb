use super::*;

impl Parser {
    /// Reject `WITHIN GROUP` explicitly until ordered-set aggregates are modeled.
    pub(crate) fn reject_within_group(&mut self) -> DbResult<()> {
        if self.consume_keyword(Keyword::Within).is_some() {
            return self.unsupported_feature(
                self.previous_span(),
                "WITHIN GROUP ordered-set aggregates are not supported",
            );
        }
        Ok(())
    }

    /// Try to parse an optional table alias: `[AS] identifier`.
    /// Returns `None` if the next token cannot be an alias (e.g. a reserved keyword
    /// that starts a SQL clause).  Also skips optional column alias list after
    /// the alias: `AS alias(col1, col2, ...)`.
    pub(super) fn try_parse_table_alias(&mut self) -> (Option<String>, Option<Vec<String>>) {
        if self.consume_keyword(Keyword::As).is_some() {
            // After AS, an identifier is required
            if let Ok((name, _)) = self.expect_identifier() {
                // Parse optional column alias list: AS alias(col1, col2, ...)
                let col_aliases = self.try_parse_column_alias_list();
                return (Some(name), col_aliases);
            }
            return (None, None);
        }
        // Try bare alias: next token must be an identifier or unreserved keyword,
        // but NOT a reserved keyword that starts a clause, and NOT an identifier
        // that acts as a SQL clause keyword (WINDOW, TABLESAMPLE, etc.).
        match &self.current().kind {
            TokenKind::Identifier(s)
                if !matches!(
                    s.to_ascii_uppercase().as_str(),
                    "WINDOW" | "TABLESAMPLE" | "ROWS" | "ISNULL" | "NOTNULL"
                ) =>
            {
                let Some((name, _)) = self.expect_identifier().ok() else {
                    return (None, None);
                };
                // Parse optional column alias list: alias(col1, col2, ...)
                let col_aliases = self.try_parse_column_alias_list();
                (Some(name), col_aliases)
            }
            TokenKind::Keyword(kw) if kw.is_unreserved() => {
                // Most unreserved keywords can be aliases, but not these
                // that start clauses after a table reference in FROM/JOIN
                if matches!(
                    kw,
                    Keyword::From
                        | Keyword::Where
                        | Keyword::Group
                        | Keyword::Having
                        | Keyword::Order
                        | Keyword::Limit
                        | Keyword::Offset
                        | Keyword::Union
                        | Keyword::Intersect
                        | Keyword::Except
                        | Keyword::Join
                        | Keyword::Inner
                        | Keyword::Left
                        | Keyword::Right
                        | Keyword::Full
                        | Keyword::Cross
                        | Keyword::Natural
                        | Keyword::On
                        | Keyword::Using
                        | Keyword::Into
                        | Keyword::For
                        | Keyword::Fetch
                        | Keyword::Returning
                        | Keyword::With
                        | Keyword::Lateral
                        | Keyword::Set
                ) {
                    return (None, None);
                }
                let Some((name, _)) = self.expect_identifier().ok() else {
                    return (None, None);
                };
                // Parse optional column alias list: alias(col1, col2, ...)
                let col_aliases = self.try_parse_column_alias_list();
                (Some(name), col_aliases)
            }
            _ => (None, None),
        }
    }

    /// Parse an optional parenthesized column alias list: `(col1, col2, ...)`.
    /// Returns `Some(vec![...])` if a list was found, `None` otherwise.
    ///
    /// PostgreSQL also accepts a column-definition list here when the FROM-ref
    /// is a record-returning function: `f(...) AS x(a int, b text)`. We accept
    /// the type tokens too - only the alias names are surfaced - so the rest of
    /// the binder can route the call through the existing record helpers.
    fn try_parse_column_alias_list(&mut self) -> Option<Vec<String>> {
        if !self.consume_kind(&TokenKind::LParen) {
            return None;
        }
        let mut aliases = Vec::new();
        loop {
            if let Ok((alias, _)) = self.expect_identifier() {
                aliases.push(alias);
                if !matches!(self.current().kind, TokenKind::Comma | TokenKind::RParen) {
                    let save = self.index;
                    if self.parse_data_type().is_err() {
                        self.index = save;
                        let mut depth = 1u32;
                        while depth > 0 && !self.is_eof() {
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
                        return if aliases.is_empty() {
                            None
                        } else {
                            Some(aliases)
                        };
                    }
                }
            } else {
                let mut depth = 1u32;
                while depth > 0 && !self.is_eof() {
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
                return if aliases.is_empty() {
                    None
                } else {
                    Some(aliases)
                };
            }
            if !self.consume_kind(&TokenKind::Comma) {
                break;
            }
        }
        let _ = self.consume_kind(&TokenKind::RParen);
        if aliases.is_empty() {
            None
        } else {
            Some(aliases)
        }
    }

    /// Parse `DISTINCT`, `DISTINCT ON (expr, ...)`, or nothing after SELECT.
    pub(super) fn parse_distinct_kind(&mut self) -> DbResult<DistinctKind> {
        if self.consume_keyword(Keyword::Distinct).is_some() {
            // Check for DISTINCT ON (expr, ...)
            if self.consume_keyword(Keyword::On).is_some() {
                self.expect_token(&TokenKind::LParen)?;
                let mut exprs = Vec::new();
                loop {
                    exprs.push(self.parse_expr()?);
                    if !self.consume_kind(&TokenKind::Comma) {
                        break;
                    }
                }
                self.expect_token(&TokenKind::RParen)?;
                Ok(DistinctKind::DistinctOn(exprs))
            } else {
                Ok(DistinctKind::Distinct)
            }
        } else {
            // `ALL` is the default set quantifier but can be written explicitly:
            // `SELECT ALL ...`. Consume it so it does not get parsed as an item.
            let _ = self.consume_keyword(Keyword::All);
            Ok(DistinctKind::All)
        }
    }

    /// Check if the current token is one of the GROUP BY set constructors:
    /// ROLLUP, CUBE, or GROUPING (for GROUPING SETS).
    pub(super) fn is_group_by_set_constructor(&self) -> bool {
        match &self.current().kind {
            TokenKind::Identifier(s) => {
                let upper = s.to_ascii_uppercase();
                upper == "ROLLUP" || upper == "CUBE" || upper == "GROUPING"
            }
            _ => false,
        }
    }

    /// Parse a structured ROLLUP/CUBE/GROUPING SETS group-by item that
    /// preserves the semantic information needed for grouping set expansion.
    pub(super) fn parse_group_by_structured_item(&mut self) -> DbResult<GroupByItem> {
        let kw_token = self.advance(); // consume ROLLUP/CUBE/GROUPING
        let kw_upper = match &kw_token.kind {
            TokenKind::Identifier(s) => s.to_ascii_uppercase(),
            _ => {
                return self
                    .syntax_error(kw_token.span, "expected ROLLUP, CUBE, or GROUPING keyword");
            }
        };

        // GROUPING SETS(...) -- consume the extra SETS keyword
        if kw_upper == "GROUPING" {
            if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("sets"))
            {
                self.advance(); // consume SETS
            }
        }

        self.expect_token(&TokenKind::LParen)?;

        match kw_upper.as_str() {
            "ROLLUP" => {
                let col_sets = self.parse_grouping_col_sets()?;
                self.expect_token(&TokenKind::RParen)?;
                Ok(GroupByItem::Rollup(col_sets))
            }
            "CUBE" => {
                let col_sets = self.parse_grouping_col_sets()?;
                self.expect_token(&TokenKind::RParen)?;
                Ok(GroupByItem::Cube(col_sets))
            }
            "GROUPING" => {
                let sets = self.parse_grouping_sets_body()?;
                self.expect_token(&TokenKind::RParen)?;
                Ok(GroupByItem::GroupingSets(sets))
            }
            _ => self.syntax_error(kw_token.span, "expected ROLLUP, CUBE, or GROUPING SETS"),
        }
    }

    /// Parse column-set arguments inside ROLLUP(...) or CUBE(...).
    /// Each argument is either a single expression or a parenthesized
    /// list of expressions (composite key).
    fn parse_grouping_col_sets(&mut self) -> DbResult<Vec<Vec<Expr>>> {
        let mut result = Vec::new();
        if self.current().kind == TokenKind::RParen {
            return Ok(result);
        }
        loop {
            if self.current().kind == TokenKind::LParen {
                // Parenthesized composite key: (a, b)
                self.advance(); // consume '('
                let mut exprs = Vec::new();
                if !self.consume_kind(&TokenKind::RParen) {
                    loop {
                        exprs.push(self.parse_expr()?);
                        if !self.consume_kind(&TokenKind::Comma) {
                            break;
                        }
                    }
                    self.expect_token(&TokenKind::RParen)?;
                }
                result.push(exprs);
            } else {
                let expr = self.parse_expr()?;
                result.push(vec![expr]);
            }
            if !self.consume_kind(&TokenKind::Comma) {
                break;
            }
        }
        Ok(result)
    }

    /// Parse the body of GROUPING SETS(...): a comma-separated list of
    /// sets, each of which can be empty `()`, a parenthesized list `(a,b)`,
    /// a single expression, or a nested ROLLUP/CUBE/GROUPING SETS.
    fn parse_grouping_sets_body(&mut self) -> DbResult<Vec<GroupBySet>> {
        let mut sets = Vec::new();
        if self.current().kind == TokenKind::RParen {
            return Ok(sets);
        }
        loop {
            if self.is_group_by_set_constructor() {
                // Nested ROLLUP/CUBE/GROUPING SETS inside GROUPING SETS
                let nested_item = self.parse_group_by_structured_item()?;
                match nested_item {
                    GroupByItem::Rollup(cols) => sets.push(GroupBySet::Rollup(cols)),
                    GroupByItem::Cube(cols) => sets.push(GroupBySet::Cube(cols)),
                    GroupByItem::GroupingSets(inner) => sets.push(GroupBySet::Nested(inner)),
                    _ => {}
                }
            } else if self.current().kind == TokenKind::LParen {
                self.advance(); // consume '('
                if self.consume_kind(&TokenKind::RParen) {
                    // Empty set ()
                    sets.push(GroupBySet::Empty);
                } else {
                    // Check if this is ((expr, ...)) - nested parens for composite key
                    let mut exprs = Vec::new();
                    loop {
                        // Inner parenthesized group inside grouping sets:
                        // GROUPING SETS((a,b), (c)) - each (a,b) is a multi-column set
                        exprs.push(self.parse_expr()?);
                        if !self.consume_kind(&TokenKind::Comma) {
                            break;
                        }
                    }
                    self.expect_token(&TokenKind::RParen)?;
                    sets.push(GroupBySet::Exprs(exprs));
                }
            } else {
                let expr = self.parse_expr()?;
                sets.push(GroupBySet::Exprs(vec![expr]));
            }
            if !self.consume_kind(&TokenKind::Comma) {
                break;
            }
        }
        Ok(sets)
    }
}

/// Extract all unique expressions from structured `group_by_items` into
/// a flat list, for backward compatibility with existing code that
/// only uses `group_by: Vec<Expr>`.
pub(crate) fn flatten_group_by_items(items: &[GroupByItem]) -> Vec<Expr> {
    let mut result = Vec::new();
    for item in items {
        flatten_group_by_item(item, &mut result);
    }
    result
}

fn flatten_group_by_item(item: &GroupByItem, out: &mut Vec<Expr>) {
    match item {
        GroupByItem::Plain(expr) => {
            if !out.iter().any(|e| e == expr) {
                out.push(expr.clone());
            }
        }
        GroupByItem::Rollup(col_sets) | GroupByItem::Cube(col_sets) => {
            for col_set in col_sets {
                for expr in col_set {
                    if !out.iter().any(|e| e == expr) {
                        out.push(expr.clone());
                    }
                }
            }
        }
        GroupByItem::GroupingSets(sets) => {
            for set in sets {
                flatten_group_by_set(set, out);
            }
        }
        GroupByItem::Empty => {
            // Empty grouping set contributes no expressions.
        }
    }
}

fn flatten_group_by_set(set: &GroupBySet, out: &mut Vec<Expr>) {
    match set {
        GroupBySet::Exprs(exprs) => {
            for expr in exprs {
                if !out.iter().any(|e| e == expr) {
                    out.push(expr.clone());
                }
            }
        }
        GroupBySet::Empty => {}
        GroupBySet::Rollup(col_sets) | GroupBySet::Cube(col_sets) => {
            for col_set in col_sets {
                for expr in col_set {
                    if !out.iter().any(|e| e == expr) {
                        out.push(expr.clone());
                    }
                }
            }
        }
        GroupBySet::Nested(inner) => {
            for s in inner {
                flatten_group_by_set(s, out);
            }
        }
    }
}

impl Parser {
    /// Check if the current token can be a bare column alias (without AS)
    /// in a SELECT item list.  Returns `true` only for identifiers/unreserved
    /// keywords that are NOT SQL clause-starting keywords.
    pub(super) fn is_bare_select_alias(&self) -> bool {
        match &self.current().kind {
            TokenKind::Identifier(s) => {
                // Reject identifiers that look like SQL clause keywords
                !matches!(
                    s.to_ascii_uppercase().as_str(),
                    "WINDOW"
                        | "TABLESAMPLE"
                        | "ISNULL"
                        | "NOTNULL"
                        | "ROWS"
                        | "GROUPING"
                        | "ROLLUP"
                        | "CUBE"
                )
            }
            TokenKind::Keyword(kw) if kw.is_unreserved() => {
                // Most unreserved keywords can be aliases, but not these
                !matches!(
                    kw,
                    Keyword::From
                        | Keyword::Where
                        | Keyword::Group
                        | Keyword::Having
                        | Keyword::Order
                        | Keyword::Limit
                        | Keyword::Offset
                        | Keyword::Union
                        | Keyword::Intersect
                        | Keyword::Except
                        | Keyword::Join
                        | Keyword::Inner
                        | Keyword::Left
                        | Keyword::Right
                        | Keyword::Full
                        | Keyword::Cross
                        | Keyword::Natural
                        | Keyword::On
                        | Keyword::Using
                        | Keyword::Into
                        | Keyword::For
                        | Keyword::Fetch
                        | Keyword::Returning
                        | Keyword::With
                        | Keyword::Lateral
                )
            }
            _ => false,
        }
    }
}
