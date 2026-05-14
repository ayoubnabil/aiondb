use super::*;

impl Parser {
    fn parse_create_table_as_query(
        &mut self,
        create_span: crate::Span,
    ) -> DbResult<SelectStatement> {
        // PostgreSQL accepts an optional pair of parentheses around the
        // SELECT body in `CREATE TABLE ... AS (SELECT ...)`. Strip them
        // here so the rest of the code path behaves identically to the
        // unparenthesized form.
        let mut parens_depth = 0_usize;
        while matches!(self.current().kind, TokenKind::LParen) {
            self.advance();
            parens_depth += 1;
        }
        let statement = if matches!(self.current().kind, TokenKind::Keyword(Keyword::Values)) {
            self.parse_values_as_statement()?
        } else if matches!(self.current().kind, TokenKind::Keyword(Keyword::With)) {
            self.parse_with_statement()?
        } else {
            self.parse_select_statement()?
        };

        let statement = match self.maybe_parse_set_operation(statement) {
            Ok(Statement::Select(query)) => query,
            Ok(_) | Err(_) => {
                return self.unsupported_feature(
                    create_span,
                    "CREATE TABLE AS with this query form is not supported",
                );
            }
        };

        for _ in 0..parens_depth {
            self.expect_token(&TokenKind::RParen)?;
        }
        Ok(statement)
    }

    /// Parse optional trailing `WITH NO DATA` / `WITH DATA` for CTAS.
    /// Returns `true` when `WITH NO DATA` was present.
    fn parse_create_table_as_with_no_data_clause(&mut self) -> DbResult<bool> {
        let saved = self.index;
        if self.consume_keyword(Keyword::With).is_none() {
            return Ok(false);
        }
        if self.consume_keyword(Keyword::No).is_some() {
            self.expect_keyword(Keyword::Data)?;
            return Ok(true);
        }
        if self.consume_keyword(Keyword::Data).is_some() {
            return Ok(false);
        }
        self.index = saved;
        Ok(false)
    }

    /// Try to parse a PARTITION BY clause, returning the strategy and column names.
    /// Returns Err for unrecognized strategies, Ok(None) if not a PARTITION BY.
    fn try_parse_partition_by(&mut self) -> DbResult<Option<PartitionByClause>> {
        if self.current().kind != TokenKind::Keyword(Keyword::Partition) {
            return Ok(None);
        }
        let saved = self.index;
        let part_span = self.advance().span; // consume PARTITION
        if self.consume_keyword(Keyword::By).is_none() {
            self.index = saved;
            return Ok(None);
        }

        // Parse strategy: LIST, RANGE, or HASH
        let strategy = if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("list"))
        {
            self.advance();
            PartitionStrategy::List
        } else if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("range"))
            || matches!(&self.current().kind, TokenKind::Keyword(Keyword::Range))
        {
            self.advance();
            PartitionStrategy::Range
        } else if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("hash"))
        {
            self.advance();
            PartitionStrategy::Hash
        } else {
            // Unrecognized partitioning strategy
            let strategy_name = match &self.current().kind {
                TokenKind::Identifier(s) => s.clone(),
                TokenKind::Keyword(kw) => kw.name().to_lowercase(),
                _ => "unknown".to_string(),
            };
            return Err(DbError::bind_error(
                SqlState::InvalidParameterValue,
                format!("unrecognized partitioning strategy \"{strategy_name}\""),
            ));
        };

        // Parse column list in parentheses
        let mut columns = Vec::new();
        let mut end_span = part_span;
        if self.consume_kind(&TokenKind::LParen) {
            loop {
                if matches!(self.current().kind, TokenKind::RParen) {
                    break;
                }
                // Column name or expression.
                // If an identifier/keyword is immediately followed by '(',
                // it is a function call (e.g. `lower(a)`) - treat it as an
                // expression, not a column reference.
                match &self.current().kind {
                    TokenKind::Identifier(s) => {
                        let name = s.clone();
                        end_span = self.advance().span;
                        if !self.is_eof() && self.current().kind == TokenKind::LParen {
                            // Function call - not a plain column reference.
                        } else {
                            columns.push(name);
                        }
                    }
                    TokenKind::Keyword(kw) => {
                        let name = kw.name().to_lowercase();
                        end_span = self.advance().span;
                        if !self.is_eof() && self.current().kind == TokenKind::LParen {
                            // Function call - not a plain column reference.
                        } else {
                            columns.push(name);
                        }
                    }
                    _ => {
                        // Expression-based partition key - skip
                        end_span = self.advance().span;
                    }
                }
                // Skip optional operator class name, collation, etc.
                while !self.is_eof()
                    && !matches!(self.current().kind, TokenKind::Comma | TokenKind::RParen)
                {
                    if self.current().kind == TokenKind::LParen {
                        self.advance();
                        let mut depth = 1u32;
                        while !self.is_eof() && depth > 0 {
                            match self.current().kind {
                                TokenKind::LParen => depth += 1,
                                TokenKind::RParen => depth -= 1,
                                _ => {}
                            }
                            self.advance();
                        }
                    } else {
                        self.advance();
                    }
                }
                if !self.consume_kind(&TokenKind::Comma) {
                    break;
                }
            }
            if self.consume_kind(&TokenKind::RParen) {
                end_span = self.previous_span();
            }
        }

        Ok(Some(PartitionByClause {
            strategy,
            columns,
            span: part_span.merge(end_span),
        }))
    }

    /// Parse `CREATE TABLE child PARTITION OF parent [(...)] FOR VALUES ...`
    ///
    /// Produces a `CreateTable` statement with:
    /// - An empty column list (columns will be inherited from parent at bind time)
    /// - The parent table in `inherits`, so the binder copies its columns
    /// - Any inline column definitions or constraints from the optional `(...)` block
    fn parse_partition_of_clause(
        &mut self,
        create: crate::tokens::Token,
        table: crate::tokens::Token,
        name: ObjectName,
        temporary: bool,
        _if_not_exists: bool,
    ) -> DbResult<Statement> {
        self.expect_keyword(Keyword::Partition)?;
        self.expect_keyword(Keyword::Of)?;
        let parent_name = self.parse_object_name()?;

        let mut columns = Vec::new();
        let mut constraints = Vec::new();

        // Optional column definitions/constraints in parentheses:
        // CREATE TABLE child PARTITION OF parent (col1 WITH OPTIONS ..., CONSTRAINT ...)
        if self.current().kind == TokenKind::LParen {
            self.advance(); // consume '('
            if self.current().kind != TokenKind::RParen {
                loop {
                    if self.consume_keyword(Keyword::Like).is_some() {
                        let _ = self.parse_object_name()?;
                        while matches!(
                            &self.current().kind,
                            TokenKind::Identifier(s) if s.eq_ignore_ascii_case("including") || s.eq_ignore_ascii_case("excluding")
                        ) {
                            self.advance();
                            if !self.is_eof()
                                && !matches!(
                                    self.current().kind,
                                    TokenKind::Comma | TokenKind::RParen
                                )
                            {
                                self.advance();
                            }
                        }
                    } else if self.is_table_constraint_start() {
                        constraints.push(self.parse_table_constraint()?);
                    } else {
                        // In PARTITION OF, column entries may omit the data
                        // type and specify only constraints/options, e.g.:
                        //   b NOT NULL DEFAULT 1
                        //   b WITH OPTIONS NOT NULL
                        // Detect this by checking the token after the column
                        // name - if it is a constraint keyword or WITH, skip
                        // the data type and parse only constraints.
                        let is_constraint_only = if matches!(
                            self.current().kind,
                            TokenKind::Identifier(_) | TokenKind::Keyword(_)
                        ) {
                            // Peek at what follows the column name
                            let next_idx = self.index + 1;
                            if next_idx < self.tokens.len() {
                                matches!(
                                    self.tokens[next_idx].kind,
                                    TokenKind::Keyword(
                                        Keyword::Not
                                            | Keyword::Null
                                            | Keyword::Default
                                            | Keyword::Check
                                            | Keyword::Unique
                                            | Keyword::Primary
                                            | Keyword::References
                                            | Keyword::With
                                            | Keyword::Constraint
                                    )
                                ) || matches!(
                                    &self.tokens[next_idx].kind,
                                    TokenKind::Identifier(s) if s.eq_ignore_ascii_case("collate") || s.eq_ignore_ascii_case("storage")
                                ) || matches!(
                                    self.tokens[next_idx].kind,
                                    TokenKind::Comma | TokenKind::RParen
                                )
                            } else {
                                false
                            }
                        } else {
                            false
                        };

                        if is_constraint_only {
                            // Parse just the column name, skip data type,
                            // then skip all remaining constraint tokens
                            // until comma or rparen.
                            let (_col_name, _col_span) = self.expect_identifier()?;
                            // Skip optional WITH OPTIONS
                            if self.current().kind == TokenKind::Keyword(Keyword::With) {
                                let next_idx = self.index + 1;
                                if next_idx < self.tokens.len()
                                    && matches!(
                                        &self.tokens[next_idx].kind,
                                        TokenKind::Identifier(s) if s.eq_ignore_ascii_case("options")
                                    )
                                {
                                    self.advance(); // WITH
                                    self.advance(); // OPTIONS
                                }
                            }
                            // Skip all constraint tokens until comma or rparen
                            let mut depth = 0u32;
                            while !self.is_eof() {
                                match self.current().kind {
                                    TokenKind::LParen => {
                                        depth += 1;
                                        self.advance();
                                    }
                                    TokenKind::RParen if depth > 0 => {
                                        depth -= 1;
                                        self.advance();
                                    }
                                    TokenKind::Comma | TokenKind::RParen if depth == 0 => break,
                                    _ => {
                                        self.advance();
                                    }
                                }
                            }
                            // We don't add to columns since the parent's
                            // columns will be inherited; the constraints are
                            // handled at the binder level.
                        } else {
                            columns.push(self.parse_column_def()?);
                        }
                    }

                    if self.consume_kind(&TokenKind::Comma) {
                        continue;
                    }
                    break;
                }
            }
            let _ = self.consume_kind(&TokenKind::RParen);
        }

        // Scan remaining tokens to determine the partition bound type and
        // extract a PARTITION BY sub-clause, without actually parsing expressions
        // (which would be fragile for PARTITION OF's column-modifier syntax).

        let mut end_span = parent_name.span;

        // Scan tokens to extract bound info before skip_to_semicolon
        let _scan_start = self.index;
        // Collect remaining tokens until semicolon for analysis
        let mut remaining_tokens: Vec<(TokenKind, crate::Span)> = Vec::new();
        {
            let saved = self.index;
            while !self.is_eof() && !matches!(self.current().kind, TokenKind::Semicolon) {
                remaining_tokens.push((self.current().kind.clone(), self.current().span));
                self.advance();
            }
            self.index = saved;
        }

        // Determine partition bound type from token patterns
        let partition_bound: Option<PartitionBound> = Self::scan_partition_bound(&remaining_tokens);

        // Look for PARTITION BY in the remaining tokens
        let partition_by: Option<PartitionByClause> = Self::scan_partition_by(&remaining_tokens);

        // Skip everything until semicolon
        self.skip_to_semicolon(&mut end_span);

        Ok(Statement::CreateTable(CreateTableStatement {
            name: name.clone(),
            columns,
            constraints,
            typed_table_of: None,
            typed_table_options: None,
            temporary,
            unlogged: false,
            if_not_exists: true,
            inherits: vec![parent_name],
            partition_of: true,
            partition_bound,
            partition_by,
            has_storage_params: false,
            storage_params: Vec::new(),
            has_exclusion_constraint: false,
            span: create
                .span
                .merge(table.span)
                .merge(name.span)
                .merge(end_span),
        }))
    }

    /// Scan a sequence of tokens to determine the partition bound type
    /// without actually parsing expressions.
    fn scan_partition_bound(tokens: &[(TokenKind, crate::Span)]) -> Option<PartitionBound> {
        if tokens.is_empty() {
            return None;
        }

        // Check for DEFAULT
        if matches!(
            tokens.first(),
            Some((TokenKind::Keyword(Keyword::Default), _))
        ) {
            let span = tokens[0].1;
            return Some(PartitionBound::Default { span });
        }

        // Check for FOR VALUES ...
        let mut i = 0;
        if !matches!(tokens.get(i), Some((TokenKind::Keyword(Keyword::For), _))) {
            return None;
        }
        let for_span = tokens[i].1;
        i += 1;
        if !matches!(
            tokens.get(i),
            Some((TokenKind::Keyword(Keyword::Values), _))
        ) {
            return None;
        }
        i += 1;

        // FOR VALUES IN (...)
        if matches!(tokens.get(i), Some((TokenKind::Keyword(Keyword::In), _))) {
            i += 1;
            // Check for empty IN list: FOR VALUES IN ()
            if matches!(tokens.get(i), Some((TokenKind::LParen, _))) {
                if matches!(tokens.get(i + 1), Some((TokenKind::RParen, _))) {
                    let end_span = tokens[i + 1].1;
                    return Some(PartitionBound::List {
                        values: Vec::new(),
                        span: for_span.merge(end_span),
                    });
                }
            }
            // Non-empty IN list - just mark it as a List bound
            let end_span = tokens.last().map_or(for_span, |t| t.1);
            return Some(PartitionBound::List {
                values: Vec::new(), // We don't parse individual values here
                span: for_span.merge(end_span),
            });
        }

        // FOR VALUES FROM (...) TO (...)
        if matches!(tokens.get(i), Some((TokenKind::Keyword(Keyword::From), _))) {
            let end_span = tokens.last().map_or(for_span, |t| t.1);
            return Some(PartitionBound::Range {
                from: Vec::new(),
                to: Vec::new(),
                span: for_span.merge(end_span),
            });
        }

        // FOR VALUES WITH (MODULUS n, REMAINDER r)
        if matches!(tokens.get(i), Some((TokenKind::Keyword(Keyword::With), _))) {
            i += 1;
            let mut modulus = 0i64;
            let mut remainder = 0i64;
            // Scan for MODULUS and REMAINDER values
            while i < tokens.len() {
                if matches!(&tokens[i].0, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("modulus"))
                {
                    i += 1;
                    if let Some((TokenKind::Integer(n), _)) = tokens.get(i) {
                        modulus = *n;
                        i += 1;
                    }
                } else if matches!(&tokens[i].0, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("remainder"))
                {
                    i += 1;
                    if let Some((TokenKind::Integer(n), _)) = tokens.get(i) {
                        remainder = *n;
                        i += 1;
                    }
                } else {
                    i += 1;
                }
            }
            let end_span = tokens.last().map_or(for_span, |t| t.1);
            return Some(PartitionBound::Hash {
                modulus,
                remainder,
                span: for_span.merge(end_span),
            });
        }

        None
    }

    /// Scan a sequence of tokens to find a PARTITION BY clause.
    fn scan_partition_by(tokens: &[(TokenKind, crate::Span)]) -> Option<PartitionByClause> {
        for i in 0..tokens.len() {
            if !matches!(tokens[i].0, TokenKind::Keyword(Keyword::Partition)) {
                continue;
            }
            if !matches!(
                tokens.get(i + 1),
                Some((TokenKind::Keyword(Keyword::By), _))
            ) {
                continue;
            }
            let part_span = tokens[i].1;
            let j = i + 2;

            let strategy = match tokens.get(j) {
                Some((TokenKind::Identifier(s), _)) if s.eq_ignore_ascii_case("list") => {
                    PartitionStrategy::List
                }
                Some((TokenKind::Identifier(s), _)) if s.eq_ignore_ascii_case("range") => {
                    PartitionStrategy::Range
                }
                // RANGE is a keyword in the lexer, so also match Keyword::Range.
                Some((TokenKind::Keyword(Keyword::Range), _)) => PartitionStrategy::Range,
                Some((TokenKind::Identifier(s), _)) if s.eq_ignore_ascii_case("hash") => {
                    PartitionStrategy::Hash
                }
                _ => continue,
            };

            // Extract column names from the parenthesized list
            let mut columns = Vec::new();
            let mut k = j + 1;
            if matches!(tokens.get(k), Some((TokenKind::LParen, _))) {
                k += 1;
                while k < tokens.len() {
                    match &tokens[k].0 {
                        TokenKind::RParen => break,
                        TokenKind::Identifier(s) => {
                            let name = s.clone();
                            k += 1;
                            // If followed by '(' it is a function call, not
                            // a column reference - skip the parenthesized args.
                            if matches!(tokens.get(k), Some((TokenKind::LParen, _))) {
                                k += 1; // skip '('
                                let mut depth = 1u32;
                                while k < tokens.len() && depth > 0 {
                                    match &tokens[k].0 {
                                        TokenKind::LParen => depth += 1,
                                        TokenKind::RParen => depth -= 1,
                                        _ => {}
                                    }
                                    k += 1;
                                }
                            } else {
                                columns.push(name);
                            }
                        }
                        TokenKind::Keyword(kw) => {
                            let name = kw.name().to_lowercase();
                            k += 1;
                            if matches!(tokens.get(k), Some((TokenKind::LParen, _))) {
                                k += 1;
                                let mut depth = 1u32;
                                while k < tokens.len() && depth > 0 {
                                    match &tokens[k].0 {
                                        TokenKind::LParen => depth += 1,
                                        TokenKind::RParen => depth -= 1,
                                        _ => {}
                                    }
                                    k += 1;
                                }
                            } else {
                                columns.push(name);
                            }
                        }
                        TokenKind::Comma => {
                            k += 1;
                        }
                        TokenKind::LParen => {
                            // Parenthesized expression like (b+0) - skip
                            // the entire group so inner identifiers are not
                            // mistaken for column names.
                            k += 1;
                            let mut depth = 1u32;
                            while k < tokens.len() && depth > 0 {
                                match &tokens[k].0 {
                                    TokenKind::LParen => depth += 1,
                                    TokenKind::RParen => depth -= 1,
                                    _ => {}
                                }
                                k += 1;
                            }
                        }
                        _ => {
                            k += 1;
                        }
                    }
                }
            }

            let end_span = if k < tokens.len() {
                tokens[k].1
            } else {
                tokens.last().map_or(part_span, |t| t.1)
            };

            return Some(PartitionByClause {
                strategy,
                columns,
                span: part_span.merge(end_span),
            });
        }
        None
    }

    pub(super) fn parse_create_table_statement(
        &mut self,
        create: crate::tokens::Token,
        temporary: bool,
        unlogged: bool,
    ) -> DbResult<Statement> {
        let table = self.expect_keyword(Keyword::Table)?;
        let if_not_exists = self.consume_if_not_exists_clause()?;
        if matches!(self.current().kind, TokenKind::Eof) {
            return self.syntax_error_current("syntax error at end of input");
        }
        if matches!(self.current().kind, TokenKind::Semicolon) {
            return self.syntax_error_current("syntax error at or near \";\"");
        }
        let name = self.parse_object_name()?;

        // CREATE TABLE name (col_aliases) AS SELECT ... (CTAS with column aliases)
        // Detect by checking if '(' is followed by identifiers and ')' then 'AS'
        if self.current().kind == TokenKind::LParen {
            // Lookahead: check if this is (ident, ident, ...) followed by AS
            let saved_ctas = self.index;
            let mut is_ctas_aliases = false;
            let mut column_aliases = Vec::new();
            self.advance(); // skip '('
                            // Scan for identifiers separated by commas until ')'
            let mut ok = true;
            loop {
                match &self.current().kind {
                    TokenKind::Identifier(name) => {
                        column_aliases.push(name.clone());
                        self.advance();
                    }
                    TokenKind::Keyword(kw) => {
                        column_aliases.push(kw.name().to_lowercase());
                        self.advance();
                    }
                    _ => {
                        ok = false;
                        break;
                    }
                }
                if self.consume_kind(&TokenKind::RParen) {
                    // Check if AS follows (possibly after ON COMMIT ... clause)
                    if matches!(self.current().kind, TokenKind::Keyword(Keyword::As)) {
                        is_ctas_aliases = true;
                    } else if matches!(self.current().kind, TokenKind::Keyword(Keyword::On)) {
                        // ON COMMIT {PRESERVE ROWS | DELETE ROWS | DROP} ... AS
                        let saved_on = self.index;
                        self.advance(); // ON
                        if self.consume_keyword(Keyword::Commit).is_some() {
                            // Skip DELETE/PRESERVE/DROP ROWS
                            while !self.is_eof()
                                && !matches!(
                                    self.current().kind,
                                    TokenKind::Keyword(Keyword::As)
                                        | TokenKind::Semicolon
                                        | TokenKind::Eof
                                )
                            {
                                self.advance();
                            }
                            if matches!(self.current().kind, TokenKind::Keyword(Keyword::As)) {
                                is_ctas_aliases = true;
                            } else {
                                self.index = saved_on;
                            }
                        } else {
                            self.index = saved_on;
                        }
                    }
                    // Also handle TABLESPACE before AS
                    if !is_ctas_aliases
                        && matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("tablespace"))
                    {
                        let saved_ts = self.index;
                        self.advance(); // TABLESPACE
                        if matches!(
                            self.current().kind,
                            TokenKind::Identifier(_) | TokenKind::Keyword(_)
                        ) {
                            self.advance(); // tablespace name
                        }
                        if matches!(self.current().kind, TokenKind::Keyword(Keyword::As)) {
                            is_ctas_aliases = true;
                        } else {
                            self.index = saved_ts;
                        }
                    }
                    break;
                }
                if !self.consume_kind(&TokenKind::Comma) {
                    ok = false;
                    break;
                }
            }
            if is_ctas_aliases && ok {
                // This is CTAS with column aliases - skip to AS
                self.consume_keyword(Keyword::As);
                let query = self.parse_create_table_as_query(create.span)?;
                let with_no_data = self.parse_create_table_as_with_no_data_clause()?;
                let mut span = create
                    .span
                    .merge(table.span)
                    .merge(name.span)
                    .merge(query.span);
                self.skip_to_semicolon(&mut span);
                return Ok(Statement::CreateTableAs(
                    crate::ast::CreateTableAsStatement {
                        name,
                        query,
                        temporary,
                        if_not_exists,
                        with_no_data,
                        column_aliases,
                        span,
                    },
                ));
            }
            // Not CTAS - restore position and continue normal parsing
            self.index = saved_ctas;
        }

        // CREATE TABLE name AS SELECT ... (CTAS)
        if self.consume_keyword(Keyword::As).is_some() {
            let query = self.parse_create_table_as_query(create.span)?;
            let with_no_data = self.parse_create_table_as_with_no_data_clause()?;
            let mut span = create
                .span
                .merge(table.span)
                .merge(name.span)
                .merge(query.span);
            self.skip_to_semicolon(&mut span);
            return Ok(Statement::CreateTableAs(
                crate::ast::CreateTableAsStatement {
                    name,
                    query,
                    temporary,
                    if_not_exists,
                    with_no_data,
                    column_aliases: Vec::new(),
                    span,
                },
            ));
        }

        // CREATE TABLE child PARTITION OF parent [(...)] FOR VALUES ...
        if self.current().kind == TokenKind::Keyword(Keyword::Partition) {
            return self.parse_partition_of_clause(create, table, name, temporary, if_not_exists);
        }

        // CREATE TABLE name OF type_name [(...)]
        if self.current().kind == TokenKind::Keyword(Keyword::Of) {
            self.advance(); // OF
            let typed_table_of = self.parse_object_name()?;
            let mut typed_table_options = None;
            let mut end_span = typed_table_of.span;

            if self.current().kind == TokenKind::LParen {
                let lparen = self.advance();
                let inner_start = lparen.span.end;
                let mut depth = 1u32;
                let mut closing_rparen_span = None;
                while !self.is_eof() {
                    match self.current().kind {
                        TokenKind::LParen => depth += 1,
                        TokenKind::RParen => {
                            depth = depth.saturating_sub(1);
                            if depth == 0 {
                                closing_rparen_span = Some(self.current().span);
                                break;
                            }
                        }
                        _ => {}
                    }
                    self.advance();
                }
                let rparen_span = closing_rparen_span.ok_or_else(|| {
                    DbError::parse_error(SqlState::SyntaxError, "unterminated typed-table clause")
                })?;
                typed_table_options = Some(
                    self.source[inner_start..rparen_span.start]
                        .trim()
                        .to_owned(),
                );
                let _ = self.expect_token(&TokenKind::RParen)?;
                end_span = rparen_span;
            }

            self.skip_to_semicolon(&mut end_span);
            let name_span = name.span;
            return Ok(Statement::CreateTable(CreateTableStatement {
                name,
                columns: Vec::new(),
                constraints: Vec::new(),
                typed_table_of: Some(typed_table_of),
                typed_table_options,
                temporary,
                unlogged,
                if_not_exists,
                inherits: Vec::new(),
                partition_of: false,
                partition_bound: None,
                partition_by: None,
                has_storage_params: false,
                storage_params: Vec::new(),
                has_exclusion_constraint: false,
                span: create
                    .span
                    .merge(table.span)
                    .merge(name_span)
                    .merge(end_span),
            }));
        }

        // Skip optional USING <access_method> (e.g., CREATE TABLE t USING heap2 (...))
        if self.consume_keyword(Keyword::Using).is_some() {
            // Consume the access method name (identifier or keyword)
            let _ = self.advance();
        }

        // Skip optional WITH (storage_parameters) before the column list
        if self.current().kind == TokenKind::Keyword(Keyword::With) {
            let saved_with = self.index;
            self.advance(); // consume WITH

            // WITH OIDS: removed in PG12+, reject with syntax error.
            if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("oids"))
                || matches!(self.current().kind, TokenKind::Keyword(Keyword::Oids))
            {
                let oids_span = self.current().span;
                return Err(DbError::parse_error(
                    SqlState::SyntaxError,
                    "syntax error at or near \"OIDS\"",
                )
                .with_position(oids_span.start + 1));
            }

            if self.consume_kind(&TokenKind::LParen) {
                let mut depth = 1u32;
                while depth > 0 && !self.is_eof() {
                    if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("oids"))
                        || matches!(self.current().kind, TokenKind::Keyword(Keyword::Oids))
                    {
                        return Err(DbError::bind_error(
                            SqlState::FeatureNotSupported,
                            "tables declared WITH OIDS are not supported",
                        ));
                    }
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
            } else {
                // WITH not followed by '(' - restore position
                self.index = saved_with;
            }
        }

        // Skip optional TABLESPACE <name>
        if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("tablespace"))
        {
            self.advance(); // TABLESPACE
            if matches!(
                self.current().kind,
                TokenKind::Identifier(_) | TokenKind::Keyword(_)
            ) {
                self.advance(); // tablespace name
            }
        }

        // After consuming USING/WITH/TABLESPACE, check again for CTAS: AS SELECT ...
        if self.consume_keyword(Keyword::As).is_some() {
            let query = self.parse_create_table_as_query(create.span)?;
            let with_no_data = self.parse_create_table_as_with_no_data_clause()?;
            let mut span = create
                .span
                .merge(table.span)
                .merge(name.span)
                .merge(query.span);
            self.skip_to_semicolon(&mut span);
            return Ok(Statement::CreateTableAs(
                crate::ast::CreateTableAsStatement {
                    name,
                    query,
                    temporary,
                    if_not_exists,
                    with_no_data,
                    column_aliases: Vec::new(),
                    span,
                },
            ));
        }

        self.expect_token(&TokenKind::LParen)?;

        let mut columns = Vec::new();
        let mut constraints = Vec::new();
        let mut like_parents = Vec::new();
        let mut has_exclusion_constraint = false;

        // Handle empty column list: CREATE TABLE name ()
        if self.current().kind == TokenKind::RParen {
            // empty - fall through to RParen below
        } else {
            loop {
                // Handle LIKE clause: LIKE other_table [INCLUDING/EXCLUDING ...]
                if self.consume_keyword(Keyword::Like).is_some() {
                    // Record LIKE source table so binder can clone its columns.
                    // This reuses the existing INHERITS column-copy path while
                    // preserving lightweight parser behavior.
                    let like_table = self.parse_object_name()?;
                    like_parents.push(like_table);
                    while matches!(
                        &self.current().kind,
                        TokenKind::Identifier(s) if s.eq_ignore_ascii_case("including") || s.eq_ignore_ascii_case("excluding")
                    ) {
                        self.advance(); // consume INCLUDING/EXCLUDING
                                        // Skip the option keyword (DEFAULTS, CONSTRAINTS, INDEXES, etc.)
                        if !self.is_eof()
                            && !matches!(self.current().kind, TokenKind::Comma | TokenKind::RParen)
                        {
                            self.advance();
                        }
                    }
                } else if self.is_table_constraint_start() {
                    // Detect EXCLUDE constraint for partitioned table validation
                    if matches!(self.current().kind, TokenKind::Keyword(Keyword::Exclude)) {
                        has_exclusion_constraint = true;
                    }
                    // Check for CONSTRAINT <name> EXCLUDE
                    if matches!(self.current().kind, TokenKind::Keyword(Keyword::Constraint)) {
                        let saved_peek = self.index;
                        self.advance(); // CONSTRAINT
                        if matches!(
                            self.current().kind,
                            TokenKind::Identifier(_) | TokenKind::Keyword(_)
                        ) {
                            self.advance(); // name
                            if matches!(self.current().kind, TokenKind::Keyword(Keyword::Exclude)) {
                                has_exclusion_constraint = true;
                            }
                        }
                        self.index = saved_peek;
                    }
                    constraints.push(self.parse_table_constraint()?);
                } else {
                    columns.push(self.parse_column_def()?);
                }

                if self.consume_kind(&TokenKind::Comma) {
                    continue;
                }
                break;
            }
        } // end of non-empty column list else block

        let rparen = self.expect_token(&TokenKind::RParen)?;
        let mut end_span = rparen.span;
        let mut inherits = Vec::new();
        let mut partition_by = None;
        let mut has_storage_params = false;
        let mut storage_params: Vec<(String, String)> = Vec::new();

        // Parse trailing clauses: INHERITS(...), PARTITION BY ..., TABLESPACE ..., WITH (...), etc.
        while !self.is_eof()
            && !matches!(self.current().kind, TokenKind::Semicolon)
            && !matches!(self.current().kind, TokenKind::Keyword(Keyword::Create))
        {
            // Parse INHERITS(parent1, parent2, ...) to extract parent table names.
            if self.consume_keyword(Keyword::Inherits).is_some() {
                end_span = end_span.merge(self.previous_span());
                if self.consume_kind(&TokenKind::LParen) {
                    loop {
                        if matches!(self.current().kind, TokenKind::RParen) {
                            break;
                        }
                        inherits.push(self.parse_object_name()?);
                        if !self.consume_kind(&TokenKind::Comma) {
                            break;
                        }
                    }
                    if self.consume_kind(&TokenKind::RParen) {
                        end_span = end_span.merge(self.previous_span());
                    }
                }
                continue;
            }

            // Parse PARTITION BY clause
            if self.current().kind == TokenKind::Keyword(Keyword::Partition) {
                if let Some(pb) = self.try_parse_partition_by()? {
                    end_span = end_span.merge(pb.span);
                    partition_by = Some(pb);
                    continue;
                }
            }

            // WITH (storage_params) after column list
            if self.current().kind == TokenKind::Keyword(Keyword::With) {
                let saved_with = self.index;
                let _with_span = self.advance().span;

                // WITH OIDS: reject
                if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("oids"))
                    || matches!(self.current().kind, TokenKind::Keyword(Keyword::Oids))
                {
                    let oids_span = self.current().span;
                    return Err(DbError::parse_error(
                        SqlState::SyntaxError,
                        "syntax error at or near \"OIDS\"",
                    )
                    .with_position(oids_span.start + 1));
                }

                if self.consume_kind(&TokenKind::LParen) {
                    has_storage_params = true;
                    // Check for WITH (oids) or WITH (oids = true) - reject
                    // But WITH (oids = false) is allowed
                    if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("oids"))
                        || matches!(self.current().kind, TokenKind::Keyword(Keyword::Oids))
                    {
                        let saved_oids = self.index;
                        self.advance(); // skip 'oids'
                        if self.consume_kind(&TokenKind::Eq) {
                            if !matches!(&self.current().kind, TokenKind::Keyword(Keyword::False)) {
                                return Err(DbError::bind_error(
                                    SqlState::FeatureNotSupported,
                                    "tables declared WITH OIDS are not supported",
                                ));
                            }
                            // oids = false - allowed, just continue parsing
                            self.advance(); // skip 'false'
                        } else {
                            self.index = saved_oids;
                            return Err(DbError::bind_error(
                                SqlState::FeatureNotSupported,
                                "tables declared WITH OIDS are not supported",
                            ));
                        }
                    }
                    // Parse remaining key = value pairs (may follow oids = false).
                    // Skip a leading comma if we already consumed oids = false.
                    let _ = self.consume_kind(&TokenKind::Comma);
                    while !self.is_eof() && !matches!(self.current().kind, TokenKind::RParen) {
                        let key = match &self.current().kind {
                            TokenKind::Identifier(s) => s.clone(),
                            TokenKind::Keyword(kw) => format!("{kw:?}").to_ascii_lowercase(),
                            _ => break,
                        };
                        self.advance();
                        let value = if self.consume_kind(&TokenKind::Eq) {
                            let v = match &self.current().kind {
                                TokenKind::Identifier(s) => s.clone(),
                                TokenKind::Keyword(kw) => format!("{kw:?}").to_ascii_lowercase(),
                                TokenKind::Integer(n) => n.to_string(),
                                TokenKind::String(s) => s.clone(),
                                _ => String::new(),
                            };
                            self.advance();
                            v
                        } else {
                            String::new()
                        };
                        storage_params.push((key, value));
                        if !self.consume_kind(&TokenKind::Comma) {
                            break;
                        }
                    }
                    if self.consume_kind(&TokenKind::RParen) {
                        end_span = end_span.merge(self.previous_span());
                    }
                    continue;
                }
                self.index = saved_with;
            }

            end_span = end_span.merge(self.advance().span);
            // Skip balanced parentheses
            if matches!(self.tokens[self.index - 1].kind, TokenKind::LParen) {
                let mut depth = 1u32;
                while !self.is_eof() && depth > 0 {
                    match self.current().kind {
                        TokenKind::LParen => depth += 1,
                        TokenKind::RParen => depth -= 1,
                        _ => {}
                    }
                    end_span = end_span.merge(self.advance().span);
                }
            }
        }
        for like_parent in like_parents {
            let already_present = inherits.iter().any(|parent| {
                parent.parts.len() == like_parent.parts.len()
                    && parent
                        .parts
                        .iter()
                        .zip(like_parent.parts.iter())
                        .all(|(left, right)| left.eq_ignore_ascii_case(right))
            });
            if !already_present {
                inherits.push(like_parent);
            }
        }
        let name_span = name.span;
        Ok(Statement::CreateTable(CreateTableStatement {
            name,
            columns,
            constraints,
            typed_table_of: None,
            typed_table_options: None,
            temporary,
            unlogged,
            if_not_exists,
            inherits,
            partition_of: false,
            partition_bound: None,
            partition_by,
            has_storage_params,
            storage_params,
            has_exclusion_constraint,
            span: create
                .span
                .merge(table.span)
                .merge(name_span)
                .merge(end_span),
        }))
    }
}
