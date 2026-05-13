use aiondb_core::{DbError, DbResult};

use crate::{
    keywords::lookup_keyword,
    span::Span,
    tokens::{Token, TokenKind},
};

pub fn lex_sql(sql: &str) -> DbResult<Vec<Token>> {
    let mut lexer = Lexer::new(sql);
    lexer.lex()
}

struct Lexer<'a> {
    sql: &'a str,
    offset: usize,
    tokens: Vec<Token>,
}

impl<'a> Lexer<'a> {
    fn new(sql: &'a str) -> Self {
        // Most tokens are short and ASCII-heavy; this reduces reallocations
        // in the main lexing loop while keeping memory overhead modest.
        let estimated_tokens = (sql.len() / 4).max(16);
        Self {
            sql,
            offset: 0,
            tokens: Vec::with_capacity(estimated_tokens),
        }
    }

    fn lex(&mut self) -> DbResult<Vec<Token>> {
        while let Some(ch) = self.peek() {
            match ch {
                ch if ch.is_whitespace() => {
                    self.bump();
                }
                '-' if self.peek_next() == Some('-') => {
                    self.skip_line_comment();
                }
                '/' if self.peek_next() == Some('*') => {
                    self.skip_block_comment()?;
                }
                // Cypher line comment `// ...`. We accept it everywhere
                // (including SQL-mode parsing) since SQL-92 has no `//`
                // operator and recognising it doesn't affect any
                // PG-compat tokens.
                '/' if self.peek_next() == Some('/') => {
                    self.skip_line_comment();
                }
                '-' if self.peek_next() == Some('>') && self.peek_at(2) == Some('>') => {
                    self.push_triple(TokenKind::DoubleArrow);
                }
                '-' if self.peek_next() == Some('>') => self.push_double(TokenKind::Arrow),
                '-' if self.peek_next() == Some('|') && self.peek_at(2) == Some('-') => {
                    self.push_triple(TokenKind::CustomOp); // -|- adjacent range operator
                }
                '|' if self.peek_next() == Some('|') && self.peek_at(2) == Some('/') => {
                    self.push_triple(TokenKind::CustomOp); // ||/ cube root
                }
                '|' if self.peek_next() == Some('|') => self.push_double(TokenKind::PipePipe),
                '|' if self.peek_next() == Some('/') => {
                    self.push_double(TokenKind::CustomOp); // |/ square root
                }
                '|' if self.peek_next() == Some('>') && self.peek_at(2) == Some('>') => {
                    self.push_triple(TokenKind::CustomOp); // |>>
                }
                '|' if self.peek_next() == Some('&') && self.peek_at(2) == Some('>') => {
                    self.push_triple(TokenKind::CustomOp); // |&>
                }
                '|' => self.push_simple(TokenKind::Pipe),
                '#' if self.peek_next() == Some('>') && self.peek_at(2) == Some('>') => {
                    self.push_triple(TokenKind::HashDoubleArrow);
                }
                '#' if self.peek_next() == Some('>') => self.push_double(TokenKind::HashArrow),
                '#' if self.peek_next() == Some('-') => self.push_double(TokenKind::HashMinus),
                '#' if self.peek_next() == Some('#') => self.push_double(TokenKind::CustomOp), // ## closest point
                '#' => self.push_simple(TokenKind::Hash),
                '@' if self.peek_next() == Some('@') => self.push_double(TokenKind::AtAt),
                '@' if self.peek_next() == Some('?') => self.push_double(TokenKind::AtQuestion),
                '@' if self.peek_next() == Some('>') => self.push_double(TokenKind::AtGt),
                '@' => self.push_simple(TokenKind::At),
                '?' if self.peek_next() == Some('|') && self.peek_at(2) == Some('|') => {
                    self.push_triple(TokenKind::CustomOp); // ?|| geometric
                }
                '?' if self.peek_next() == Some('-') && self.peek_at(2) == Some('|') => {
                    self.push_triple(TokenKind::CustomOp); // ?-| geometric
                }
                '?' if self.peek_next() == Some('-') => self.push_double(TokenKind::CustomOp), // ?- geometric
                '?' if self.peek_next() == Some('#') => self.push_double(TokenKind::CustomOp), // ?# geometric
                '?' if self.peek_next() == Some('|') => self.push_double(TokenKind::QuestionPipe),
                '?' if self.peek_next() == Some('&') => {
                    self.push_double(TokenKind::QuestionAmpersand);
                }
                '?' => self.push_simple(TokenKind::Question),
                ':' if self.peek_next() == Some(':') => self.push_double(TokenKind::DoubleColon),
                ':' if matches!(self.peek_next(), Some(ch) if ch == '_' || ch.is_ascii_alphabetic()) =>
                {
                    self.lex_colon_variable()?;
                }
                ':' => self.push_simple(TokenKind::Colon),
                '&' if self.peek_next() == Some('&') => self.push_double(TokenKind::AmpAmp),
                '&' if self.peek_next() == Some('<') => {
                    // &< and &<| geometric operators - skip as custom operator
                    let start = self.offset;
                    self.bump();
                    self.bump();
                    if self.peek() == Some('|') {
                        self.bump();
                    }
                    let span = Span::new(start, self.offset);
                    self.tokens.push(Token {
                        kind: TokenKind::CustomOp,
                        span,
                    });
                }
                '&' if self.peek_next() == Some('>') => {
                    self.push_double(TokenKind::CustomOp);
                }
                '&' => self.push_simple(TokenKind::Ampersand),
                '!' if self.peek_next() == Some('~') && self.peek_at(2) == Some('*') => {
                    self.push_triple(TokenKind::ExclTildeStar);
                }
                '!' if self.peek_next() == Some('~') => self.push_double(TokenKind::ExclTilde),
                '!' if self.peek_next() == Some('=') && self.peek_at(2) == Some('-') => {
                    self.push_triple(TokenKind::CustomOp); // !=- user-defined operator
                }
                '!' if self.peek_next() == Some('=') => self.push_double(TokenKind::Ne),
                '!' if self.peek_next() == Some('!') => self.push_double(TokenKind::CustomOp), // !! factorial
                '!' => self.push_simple(TokenKind::Excl),
                '~' if self.peek_next() == Some('<') && self.peek_at(2) == Some('~') => {
                    self.push_triple(TokenKind::CustomOp); // ~<~ locale-aware less-than
                }
                '~' if self.peek_next() == Some('<') => {
                    // ~<=~ (4 chars)
                    if self.peek_at(2) == Some('=') && self.peek_at(3) == Some('~') {
                        let start = self.offset;
                        self.bump();
                        self.bump();
                        self.bump();
                        self.bump();
                        self.tokens.push(Token {
                            kind: TokenKind::CustomOp,
                            span: Span::new(start, self.offset),
                        });
                    } else {
                        self.push_simple(TokenKind::Tilde); // fallback
                    }
                }
                '~' if self.peek_next() == Some('>') && self.peek_at(2) == Some('~') => {
                    self.push_triple(TokenKind::CustomOp); // ~>~ locale-aware greater-than
                }
                '~' if self.peek_next() == Some('>') => {
                    // ~>=~ (4 chars)
                    if self.peek_at(2) == Some('=') && self.peek_at(3) == Some('~') {
                        let start = self.offset;
                        self.bump();
                        self.bump();
                        self.bump();
                        self.bump();
                        self.tokens.push(Token {
                            kind: TokenKind::CustomOp,
                            span: Span::new(start, self.offset),
                        });
                    } else {
                        self.push_simple(TokenKind::Tilde); // fallback
                    }
                }
                '~' if self.peek_next() == Some('=') => self.push_double(TokenKind::TildeEq),
                '~' if self.peek_next() == Some('*') => self.push_double(TokenKind::TildeStar),
                '~' => self.push_simple(TokenKind::Tilde),
                '^' => self.push_simple(TokenKind::Caret),
                '<' if self.peek_next() == Some('-') && self.peek_at(2) == Some('>') => {
                    self.push_triple(TokenKind::VecL2); // <-> pgvector L2 distance
                }
                '<' if self.peek_next() == Some('=') && self.peek_at(2) == Some('>') => {
                    self.push_triple(TokenKind::VecCosine); // <=> pgvector cosine distance
                }
                '<' if self.peek_next() == Some('#') && self.peek_at(2) == Some('>') => {
                    self.push_triple(TokenKind::VecNegInner); // <#> pgvector negative inner product
                }
                '<' if self.peek_next() == Some('+') && self.peek_at(2) == Some('>') => {
                    self.push_triple(TokenKind::VecL1); // <+> pgvector L1/taxicab distance
                }
                '<' if self.peek_next() == Some('~') && self.peek_at(2) == Some('>') => {
                    self.push_triple(TokenKind::VecHamming); // <~> pgvector Hamming distance
                }
                '<' if self.peek_next() == Some('%') && self.peek_at(2) == Some('>') => {
                    self.push_triple(TokenKind::VecJaccard); // <%> pgvector Jaccard distance
                }
                '<' if self.peek_next() == Some('<') && self.peek_at(2) == Some('<') => {
                    self.push_triple(TokenKind::CustomOp); // <<< user-defined operator
                }
                '<' if self.peek_next() == Some('<') && self.peek_at(2) == Some('|') => {
                    self.push_triple(TokenKind::CustomOp); // <<| strictly below
                }
                '<' if self.peek_next() == Some('%') => self.push_double(TokenKind::CustomOp), // <% contained-by (custom user-defined)
                '<' if self.peek_next() == Some('^') => self.push_double(TokenKind::CustomOp), // <^ below-left
                '<' if self.peek_next() == Some('<') && self.peek_at(2) == Some('=') => {
                    self.push_triple(TokenKind::CustomOp); // <<= contained-by-or-equal (inet/cidr)
                }
                '<' if self.peek_next() == Some('<') => self.push_double(TokenKind::ShiftLeft),
                '<' if self.peek_next() == Some('@') => self.push_double(TokenKind::LtAt),
                '<' if self.peek_next() == Some('=') => self.push_double(TokenKind::Le),
                '<' if self.peek_next() == Some('>') => self.push_double(TokenKind::Ne),
                '>' if self.peek_next() == Some('^') => self.push_double(TokenKind::CustomOp), // >^ above-right
                '>' if self.peek_next() == Some('>') && self.peek_at(2) == Some('>') => {
                    self.push_triple(TokenKind::CustomOp); // >>> user-defined operator
                }
                '>' if self.peek_next() == Some('>') && self.peek_at(2) == Some('=') => {
                    self.push_triple(TokenKind::CustomOp); // >>= contains-or-equal (inet/cidr)
                }
                '>' if self.peek_next() == Some('>') => self.push_double(TokenKind::ShiftRight),
                '>' if self.peek_next() == Some('=') => self.push_double(TokenKind::Ge),
                '+' => self.push_simple(TokenKind::Plus),
                '-' => self.push_simple(TokenKind::Minus),
                '*' if self.peek_next() == Some('<') && self.peek_at(2) == Some('=') => {
                    self.push_triple(TokenKind::CustomOp); // *<= row type comparison
                }
                '*' if self.peek_next() == Some('<') && self.peek_at(2) == Some('>') => {
                    self.push_triple(TokenKind::CustomOp); // *<> row type comparison
                }
                '*' if self.peek_next() == Some('>') && self.peek_at(2) == Some('=') => {
                    self.push_triple(TokenKind::CustomOp); // *>= row type comparison
                }
                '*' if matches!(self.peek_next(), Some('<' | '>' | '=')) => {
                    self.push_double(TokenKind::CustomOp); // *< *> *= row type comparison
                }
                '*' => self.push_simple(TokenKind::Star),
                '/' => self.push_simple(TokenKind::Slash),
                '%' => self.push_simple(TokenKind::Percent),
                ',' => self.push_simple(TokenKind::Comma),
                '.' if matches!(self.peek_next(), Some(ch) if ch.is_ascii_digit()) => {
                    // Dot-prefixed float literal: .123 => 0.123
                    // But after an identifier-ish token (`a.1`) the dot is a
                    // separator, not a numeric literal prefix.
                    if self.can_start_dot_number() {
                        self.lex_dot_number()?;
                    } else {
                        self.push_simple(TokenKind::Dot);
                    }
                }
                '.' => self.push_simple(TokenKind::Dot),
                '$' if self.peek_next() == Some('$') => {
                    self.lex_dollar_string()?;
                }
                '$' if matches!(self.peek_next(), Some(ch) if ch.is_ascii_digit()) => {
                    self.lex_parameter()?;
                }
                '$' if matches!(self.peek_next(), Some(ch) if ch == '_' || ch.is_ascii_alphabetic()) =>
                {
                    self.lex_tagged_dollar_string()?;
                }
                '=' if self.peek_next() == Some('>') => self.push_double(TokenKind::FatArrow),
                '=' if self.peek_next() == Some('=') && self.peek_at(2) == Some('=') => {
                    self.push_triple(TokenKind::CustomOp); // === user-defined operator
                }
                '=' => self.push_simple(TokenKind::Eq),
                '<' => self.push_simple(TokenKind::Lt),
                '(' => self.push_simple(TokenKind::LParen),
                ')' => self.push_simple(TokenKind::RParen),
                '[' => self.push_simple(TokenKind::LBracket),
                ']' => self.push_simple(TokenKind::RBracket),
                ';' => self.push_simple(TokenKind::Semicolon),
                '>' => self.push_simple(TokenKind::Gt),
                '"' => self.lex_quoted_identifier()?,
                '`' => self.lex_backtick_identifier()?,
                '\'' => self.lex_string()?,
                ch if ch.is_ascii_digit() => self.lex_integer()?,
                ch if is_identifier_start(ch) => {
                    // Handle E'...' and B'...' and X'...' escape/bit/hex strings
                    if matches!(ch, 'E' | 'e') && self.peek_next() == Some('\'') {
                        self.bump(); // skip the prefix letter
                        self.lex_escape_string()?;
                    } else if matches!(ch, 'B' | 'b' | 'X' | 'x') && self.peek_next() == Some('\'')
                    {
                        self.bump(); // skip the prefix letter
                        self.lex_string()?;
                    }
                    // Handle U&'...' and U&"..." Unicode escape strings/identifiers
                    else if matches!(ch, 'U' | 'u')
                        && self.peek_next() == Some('&')
                        && matches!(self.peek_at(2), Some('\'' | '"'))
                    {
                        let start = self.offset;
                        self.bump(); // skip U
                        self.bump(); // skip &
                        if self.peek() == Some('"') {
                            self.lex_quoted_identifier()?;
                            self.skip_uescape();
                        } else {
                            self.lex_unicode_string(start)?;
                        }
                    } else {
                        self.lex_identifier_or_keyword();
                    }
                }
                '{' => self.push_simple(TokenKind::LBrace),
                '}' => self.push_simple(TokenKind::RBrace),
                // psql metacommands (\gset, \pset, etc.) - skip to end of line
                '\\' if matches!(self.peek_next(), Some(ch) if ch.is_ascii_alphabetic()) => {
                    self.skip_line_comment();
                }
                // Bare backslash (psql \; separator, etc.) - skip gracefully
                '\\' => {
                    self.bump();
                }
                other => return Err(self.syntax_error(format!("unexpected character: '{other}'"))),
            }
        }

        self.tokens.push(Token::eof(self.sql.len()));
        Ok(std::mem::take(&mut self.tokens))
    }

    #[inline]
    fn peek(&self) -> Option<char> {
        let bytes = self.sql.as_bytes();
        let byte = *bytes.get(self.offset)?;
        if byte.is_ascii() {
            Some(char::from(byte))
        } else {
            self.sql[self.offset..].chars().next()
        }
    }

    #[inline]
    fn peek_next(&self) -> Option<char> {
        self.peek_at(1)
    }

    #[inline]
    fn peek_at(&self, n: usize) -> Option<char> {
        let bytes = self.sql.as_bytes();
        let mut idx = self.offset;

        // Fast path for ASCII-heavy SQL: avoid rebuilding UTF-8 iterators
        // on every lookahead.
        for step in 0..=n {
            let byte = *bytes.get(idx)?;
            if byte.is_ascii() {
                if step == n {
                    return Some(char::from(byte));
                }
                idx += 1;
            } else {
                // Non-ASCII is uncommon in SQL keywords/operators; defer to
                // the UTF-8-aware path to preserve correctness.
                return self.sql[self.offset..].chars().nth(n);
            }
        }
        None
    }

    #[inline]
    fn bump(&mut self) -> Option<char> {
        let bytes = self.sql.as_bytes();
        let byte = *bytes.get(self.offset)?;
        if byte.is_ascii() {
            self.offset += 1;
            Some(char::from(byte))
        } else {
            let ch = self.sql[self.offset..].chars().next()?;
            self.offset += ch.len_utf8();
            Some(ch)
        }
    }

    fn can_start_dot_number(&self) -> bool {
        if self.offset == 0 {
            return true;
        }
        let prev = self
            .sql
            .as_bytes()
            .get(self.offset.saturating_sub(1))
            .copied()
            .map(char::from);
        // Reject after `.` so `1..3` lexes as `1` `.` `.` `3`, not `1.` `.3`.
        !prev.is_some_and(|ch| {
            ch.is_ascii_alphanumeric() || ch == '_' || ch == ')' || ch == ']' || ch == '.'
        })
    }

    fn push_simple(&mut self, kind: TokenKind) {
        let start = self.offset;
        self.bump();
        self.tokens.push(Token {
            kind,
            span: Span::new(start, self.offset),
        });
    }

    fn push_double(&mut self, kind: TokenKind) {
        let start = self.offset;
        self.bump();
        self.bump();
        self.tokens.push(Token {
            kind,
            span: Span::new(start, self.offset),
        });
    }

    fn push_triple(&mut self, kind: TokenKind) {
        let start = self.offset;
        self.bump();
        self.bump();
        self.bump();
        self.tokens.push(Token {
            kind,
            span: Span::new(start, self.offset),
        });
    }

    fn skip_line_comment(&mut self) {
        while let Some(ch) = self.bump() {
            if ch == '\n' {
                break;
            }
        }
    }

    fn skip_block_comment(&mut self) -> DbResult<()> {
        self.bump(); // consume '/'
        self.bump(); // consume '*'
        let mut depth: u32 = 1;
        while let Some(ch) = self.bump() {
            if ch == '/' && self.peek() == Some('*') {
                self.bump();
                depth += 1;
                if depth > 256 {
                    return Err(self.syntax_error("block comment nesting too deep"));
                }
            } else if ch == '*' && self.peek() == Some('/') {
                self.bump();
                depth -= 1;
                if depth == 0 {
                    return Ok(());
                }
            }
        }
        Err(self.syntax_error("unterminated block comment"))
    }

    fn lex_integer(&mut self) -> DbResult<()> {
        let start = self.offset;

        // Check for non-decimal prefixes: 0b, 0o, 0x
        if self.peek() == Some('0') {
            if let Some(prefix) = self.peek_next() {
                match prefix {
                    'b' | 'B' => {
                        self.bump(); // consume '0'
                        self.bump(); // consume 'b'/'B'
                        while matches!(self.peek(), Some(ch) if ch.is_ascii_digit() || ch == '_') {
                            self.bump();
                        }
                        let raw_digits = &self.sql[start + 2..self.offset];
                        let Some(cleaned) = normalize_based_literal_digits(raw_digits, 2) else {
                            let snippet = &self.sql[start..self.offset];
                            return Err(DbError::syntax_error(format!(
                                "invalid binary integer at or near \"{snippet}\""
                            ))
                            .with_position(start + 1));
                        };
                        if let Some(ch) = self.peek() {
                            if is_identifier_continue(ch) {
                                let snippet_end = self.offset + ch.len_utf8();
                                let snippet = &self.sql[start..snippet_end];
                                return Err(DbError::syntax_error(format!(
                                    "trailing junk after numeric literal at or near \"{snippet}\""
                                ))
                                .with_position(start + 1));
                            }
                        }
                        let decimal = radix_to_decimal_string(&cleaned, 2).ok_or_else(|| {
                            DbError::syntax_error("invalid binary integer").with_position(start + 1)
                        })?;
                        let kind = match decimal.parse::<i64>() {
                            Ok(value) => TokenKind::Integer(value),
                            Err(_) => TokenKind::NumericLiteral(decimal),
                        };
                        self.tokens.push(Token {
                            kind,
                            span: Span::new(start, self.offset),
                        });
                        return Ok(());
                    }
                    'o' | 'O' => {
                        self.bump(); // consume '0'
                        self.bump(); // consume 'o'/'O'
                        while matches!(self.peek(), Some(ch) if ch.is_ascii_digit() || ch == '_') {
                            self.bump();
                        }
                        let raw_digits = &self.sql[start + 2..self.offset];
                        let Some(cleaned) = normalize_based_literal_digits(raw_digits, 8) else {
                            let snippet = &self.sql[start..self.offset];
                            return Err(DbError::syntax_error(format!(
                                "invalid octal integer at or near \"{snippet}\""
                            ))
                            .with_position(start + 1));
                        };
                        if let Some(ch) = self.peek() {
                            if is_identifier_continue(ch) {
                                let snippet_end = self.offset + ch.len_utf8();
                                let snippet = &self.sql[start..snippet_end];
                                return Err(DbError::syntax_error(format!(
                                    "trailing junk after numeric literal at or near \"{snippet}\""
                                ))
                                .with_position(start + 1));
                            }
                        }
                        let decimal = radix_to_decimal_string(&cleaned, 8).ok_or_else(|| {
                            DbError::syntax_error("invalid octal integer").with_position(start + 1)
                        })?;
                        let kind = match decimal.parse::<i64>() {
                            Ok(value) => TokenKind::Integer(value),
                            Err(_) => TokenKind::NumericLiteral(decimal),
                        };
                        self.tokens.push(Token {
                            kind,
                            span: Span::new(start, self.offset),
                        });
                        return Ok(());
                    }
                    'x' | 'X' => {
                        self.bump(); // consume '0'
                        self.bump(); // consume 'x'/'X'
                        while matches!(self.peek(), Some(ch) if ch.is_ascii_hexdigit() || ch == '_')
                        {
                            self.bump();
                        }
                        let raw_digits = &self.sql[start + 2..self.offset];
                        let Some(cleaned) = normalize_based_literal_digits(raw_digits, 16) else {
                            let snippet = &self.sql[start..self.offset];
                            return Err(DbError::syntax_error(format!(
                                "invalid hexadecimal integer at or near \"{snippet}\""
                            ))
                            .with_position(start + 1));
                        };
                        if let Some(ch) = self.peek() {
                            if is_identifier_continue(ch) {
                                let snippet_end = self.offset + ch.len_utf8();
                                let snippet = &self.sql[start..snippet_end];
                                return Err(DbError::syntax_error(format!(
                                    "trailing junk after numeric literal at or near \"{snippet}\""
                                ))
                                .with_position(start + 1));
                            }
                        }
                        let decimal = radix_to_decimal_string(&cleaned, 16).ok_or_else(|| {
                            DbError::syntax_error("invalid hexadecimal integer")
                                .with_position(start + 1)
                        })?;
                        let kind = match decimal.parse::<i64>() {
                            Ok(value) => TokenKind::Integer(value),
                            Err(_) => TokenKind::NumericLiteral(decimal),
                        };
                        self.tokens.push(Token {
                            kind,
                            span: Span::new(start, self.offset),
                        });
                        return Ok(());
                    }
                    _ => {}
                }
            }
        }

        // Decimal integer: allow digit separators (underscores)
        while matches!(self.peek(), Some(ch) if ch.is_ascii_digit() || ch == '_') {
            self.bump();
        }

        // Optional decimal-point part.
        if self.peek() == Some('.') {
            match self.peek_next() {
                Some(ch) if ch.is_ascii_alphabetic() || ch == '_' => {
                    let snippet_end = self.offset + 2;
                    let snippet = &self.sql[start..snippet_end.min(self.sql.len())];
                    return Err(DbError::syntax_error(format!(
                        "trailing junk after numeric literal at or near \"{snippet}\""
                    ))
                    .with_position(start + 1));
                }
                // Don't fold `..` into the integer: `1..3` must lex as
                // `1` `.` `.` `3` so the slice/range parser can handle it.
                Some('.') => {}
                _ => {
                    self.bump(); // consume '.'
                    while matches!(self.peek(), Some(ch) if ch.is_ascii_digit() || ch == '_') {
                        self.bump();
                    }
                }
            }
        }

        // Optional exponent part.
        if matches!(self.peek(), Some('e' | 'E')) {
            self.bump(); // consume e/E
            if matches!(self.peek(), Some('+' | '-')) {
                self.bump();
            }
            while matches!(self.peek(), Some(ch) if ch.is_ascii_digit() || ch == '_') {
                self.bump();
            }
        }

        let raw = &self.sql[start..self.offset];
        let (normalized, is_decimal_form) =
            normalize_decimal_literal(raw).map_err(|snippet_end| {
                let snippet = &raw[..snippet_end.min(raw.len())];
                DbError::syntax_error(format!(
                    "trailing junk after numeric literal at or near \"{snippet}\""
                ))
                .with_position(start + 1)
            })?;

        if let Some(ch) = self.peek() {
            if is_identifier_continue(ch) {
                let snippet_end = self.offset + ch.len_utf8();
                let snippet = &self.sql[start..snippet_end];
                return Err(DbError::syntax_error(format!(
                    "trailing junk after numeric literal at or near \"{snippet}\""
                ))
                .with_position(start + 1));
            }
        }

        if is_decimal_form {
            self.tokens.push(Token {
                kind: TokenKind::NumericLiteral(normalized),
                span: Span::new(start, self.offset),
            });
            return Ok(());
        }

        match normalized.parse::<i64>() {
            Ok(value) => {
                self.tokens.push(Token {
                    kind: TokenKind::Integer(value),
                    span: Span::new(start, self.offset),
                });
            }
            Err(_) => {
                // Integer overflows i64 - emit as NumericLiteral instead
                self.tokens.push(Token {
                    kind: TokenKind::NumericLiteral(normalized),
                    span: Span::new(start, self.offset),
                });
            }
        }
        Ok(())
    }

    /// Lex a numeric literal that starts with a dot: `.123`, `.5e10`, etc.
    /// The caller has verified that the current char is '.' and the next is a digit.
    fn lex_dot_number(&mut self) -> DbResult<()> {
        let start = self.offset;
        self.bump(); // consume '.'
        while matches!(self.peek(), Some(ch) if ch.is_ascii_digit() || ch == '_') {
            self.bump();
        }
        // Check for exponent: e.g. .5e10
        if matches!(self.peek(), Some('e' | 'E')) {
            self.bump();
            if matches!(self.peek(), Some('+' | '-')) {
                self.bump();
            }
            while matches!(self.peek(), Some(ch) if ch.is_ascii_digit() || ch == '_') {
                self.bump();
            }
        }
        let raw = &self.sql[start..self.offset];
        let (cleaned, _) = normalize_decimal_literal(raw).map_err(|snippet_end| {
            let snippet = &raw[..snippet_end.min(raw.len())];
            DbError::syntax_error(format!(
                "trailing junk after numeric literal at or near \"{snippet}\""
            ))
            .with_position(start + 1)
        })?;
        if let Some(ch) = self.peek() {
            if is_identifier_continue(ch) {
                let snippet_end = self.offset + ch.len_utf8();
                let snippet = &self.sql[start..snippet_end];
                return Err(DbError::syntax_error(format!(
                    "trailing junk after numeric literal at or near \"{snippet}\""
                ))
                .with_position(start + 1));
            }
        }
        self.tokens.push(Token {
            kind: TokenKind::NumericLiteral(cleaned),
            span: Span::new(start, self.offset),
        });
        Ok(())
    }

    fn lex_parameter(&mut self) -> DbResult<()> {
        let start = self.offset;
        self.bump();
        while matches!(self.peek(), Some(ch) if ch.is_ascii_digit()) {
            self.bump();
        }

        if let Some(ch) = self.peek() {
            if is_identifier_continue(ch) {
                let snippet_end = self.offset + ch.len_utf8();
                let snippet = &self.sql[start..snippet_end];
                return Err(DbError::syntax_error(format!(
                    "trailing junk after parameter at or near \"{snippet}\""
                ))
                .with_position(start + 1));
            }
        }

        let raw = &self.sql[start + 1..self.offset];
        let index = raw.parse::<usize>().map_err(|error| {
            DbError::syntax_error(format!("invalid parameter placeholder ${raw}: {error}"))
                .with_position(start + 1)
        })?;
        if index == 0 {
            return Err(DbError::syntax_error("parameter placeholders start at $1")
                .with_position(start + 1));
        }

        self.tokens.push(Token {
            kind: TokenKind::Parameter(index),
            span: Span::new(start, self.offset),
        });
        Ok(())
    }

    fn lex_identifier_or_keyword(&mut self) {
        let start = self.offset;
        while matches!(self.peek(), Some(ch) if is_identifier_continue(ch)) {
            self.bump();
        }

        let raw = &self.sql[start..self.offset];
        let kind = if let Some(keyword) = lookup_keyword(raw) {
            TokenKind::Keyword(keyword)
        } else {
            // PostgreSQL folds unquoted identifiers to lowercase.
            let identifier = if raw.bytes().any(|byte| byte.is_ascii_uppercase()) {
                raw.to_ascii_lowercase()
            } else {
                raw.to_owned()
            };
            TokenKind::Identifier(identifier)
        };

        self.tokens.push(Token {
            kind,
            span: Span::new(start, self.offset),
        });
    }

    /// Lex Cypher's backtick-delimited identifier (preserves case, allows
    /// arbitrary characters and escaped backticks via doubling).
    fn lex_backtick_identifier(&mut self) -> DbResult<()> {
        const MAX_IDENTIFIER_LEN: usize = 256;
        let start = self.offset;
        self.bump(); // consume opening backtick
        let mut value = String::new();
        loop {
            match self.bump() {
                Some('`') => {
                    if self.peek() == Some('`') {
                        self.bump();
                        value.push('`');
                        continue;
                    }
                    break;
                }
                Some(ch) => {
                    if value.len() >= MAX_IDENTIFIER_LEN {
                        return Err(self.syntax_error(format!(
                            "identifier too long (maximum is {MAX_IDENTIFIER_LEN} bytes)"
                        )));
                    }
                    value.push(ch);
                }
                None => return Err(self.syntax_error("unterminated backtick identifier")),
            }
        }
        self.tokens.push(Token {
            kind: TokenKind::Identifier(value),
            span: Span::new(start, self.offset),
        });
        Ok(())
    }

    fn lex_quoted_identifier(&mut self) -> DbResult<()> {
        /// Maximum quoted identifier length (bytes). Matches `PostgreSQL` NAMEDATALEN - 1.
        const MAX_IDENTIFIER_LEN: usize = 63;

        let start = self.offset;
        self.bump(); // consume opening "

        let mut value = String::new();
        loop {
            match self.bump() {
                Some('"') => {
                    if self.peek() == Some('"') {
                        self.bump();
                        if value.len() < MAX_IDENTIFIER_LEN {
                            value.push('"');
                        }
                        continue;
                    }
                    break;
                }
                Some(ch) => {
                    // PG behaviour: identifiers exceeding NAMEDATALEN-1
                    // (63 bytes) are TRUNCATED, not rejected. Skip
                    // overflow chars instead of erroring so SQL ported
                    // from PG keeps planning.
                    if value.len() < MAX_IDENTIFIER_LEN {
                        value.push(ch);
                    }
                }
                None => return Err(self.syntax_error("unterminated quoted identifier")),
            }
        }

        self.tokens.push(Token {
            kind: TokenKind::Identifier(value),
            span: Span::new(start, self.offset),
        });
        Ok(())
    }

    fn lex_string(&mut self) -> DbResult<()> {
        const MAX_STRING_LITERAL_SIZE: usize = 10 * 1024 * 1024; // 10 MiB

        let start = self.offset;
        let mut value = self.lex_string_contents()?;

        // Handle string continuation: 'first' 'second' -> 'firstsecond'
        // Adjacent string literals (possibly separated by whitespace/comments) are concatenated.
        loop {
            let saved = self.offset;
            // Skip whitespace and comments
            while let Some(ch) = self.peek() {
                if ch.is_whitespace() {
                    self.bump();
                } else if ch == '-' && self.peek_next() == Some('-') {
                    self.skip_line_comment();
                } else if ch == '/' && self.peek_next() == Some('*') {
                    self.skip_block_comment()?;
                } else {
                    break;
                }
            }
            if self.peek() == Some('\'') {
                // Another string literal follows - concatenate
                let continuation = self.lex_string_contents()?;
                if value.len() + continuation.len() > MAX_STRING_LITERAL_SIZE {
                    return Err(self.syntax_error("string literal too long (maximum is 10 MiB)"));
                }
                value.push_str(&continuation);
            } else {
                // No continuation - restore position and stop
                self.offset = saved;
                break;
            }
        }

        self.tokens.push(Token {
            kind: TokenKind::String(value),
            span: Span::new(start, self.offset),
        });
        Ok(())
    }

    fn lex_string_contents(&mut self) -> DbResult<String> {
        self.bump();

        let mut value = String::new();
        loop {
            match self.bump() {
                Some('\'') => {
                    if self.peek() == Some('\'') {
                        self.bump();
                        value.push('\'');
                        continue;
                    }
                    break;
                }
                Some(ch) => value.push(ch),
                None => return Err(self.syntax_error("unterminated string literal")),
            }
        }

        Ok(value)
    }

    fn lex_unicode_string(&mut self, start: usize) -> DbResult<()> {
        let raw = self.lex_string_contents()?;
        let escape = self.parse_uescape_char().unwrap_or('\\');
        let decoded = decode_unicode_escape_literal(&raw, escape)
            .map_err(|message| self.syntax_error(message))?;
        self.tokens.push(Token {
            kind: TokenKind::String(decoded),
            span: Span::new(start, self.offset),
        });
        Ok(())
    }

    fn lex_escape_string(&mut self) -> DbResult<()> {
        let start = self.offset.saturating_sub(1);
        self.bump();

        let mut value = String::new();
        loop {
            match self.bump() {
                Some('\'') => {
                    if self.peek() == Some('\'') {
                        self.bump();
                        value.push('\'');
                        continue;
                    }
                    break;
                }
                Some('\\') => match self.bump() {
                    Some('\\') => value.push('\\'),
                    Some('\'') => value.push('\''),
                    Some('n') => value.push('\n'),
                    Some('r') => value.push('\r'),
                    Some('t') => value.push('\t'),
                    Some('b') => value.push('\u{0008}'),
                    Some('f') => value.push('\u{000C}'),
                    Some('v') => value.push('\u{000B}'),
                    Some('0') => value.push('\0'),
                    // \xNN - 1 or 2 hex digits (PG §4.1.2.2)
                    Some('x') => {
                        let mut digits = String::new();
                        for _ in 0..2 {
                            match self.peek() {
                                Some(c) if c.is_ascii_hexdigit() => {
                                    digits.push(c);
                                    self.bump();
                                }
                                _ => break,
                            }
                        }
                        if digits.is_empty() {
                            return Err(
                                self.syntax_error("invalid hexadecimal escape: expected digit")
                            );
                        }
                        let code = u32::from_str_radix(&digits, 16)
                            .map_err(|_| self.syntax_error("invalid hexadecimal escape value"))?;
                        if let Some(ch) = char::from_u32(code) {
                            value.push(ch);
                        } else {
                            return Err(self.syntax_error("invalid hexadecimal escape value"));
                        }
                    }
                    // \uXXXX - exactly 4 hex digits
                    Some('u') => {
                        let mut digits = String::new();
                        for _ in 0..4 {
                            match self.bump() {
                                Some(c) if c.is_ascii_hexdigit() => digits.push(c),
                                _ => {
                                    return Err(self.syntax_error(
                                        "invalid Unicode escape: expected 4 hex digits",
                                    ))
                                }
                            }
                        }
                        let code = u32::from_str_radix(&digits, 16)
                            .map_err(|_| self.syntax_error("invalid Unicode escape value"))?;
                        if let Some(ch) = char::from_u32(code) {
                            value.push(ch);
                        } else {
                            return Err(self.syntax_error("invalid Unicode escape value"));
                        }
                    }
                    // \UXXXXXXXX - exactly 8 hex digits
                    Some('U') => {
                        let mut digits = String::new();
                        for _ in 0..8 {
                            match self.bump() {
                                Some(c) if c.is_ascii_hexdigit() => digits.push(c),
                                _ => {
                                    return Err(self.syntax_error(
                                        "invalid Unicode escape: expected 8 hex digits",
                                    ))
                                }
                            }
                        }
                        let code = u32::from_str_radix(&digits, 16)
                            .map_err(|_| self.syntax_error("invalid Unicode escape value"))?;
                        if let Some(ch) = char::from_u32(code) {
                            value.push(ch);
                        } else {
                            return Err(self.syntax_error("invalid Unicode escape value"));
                        }
                    }
                    // \NNN - octal escape, 1 to 3 digits
                    Some(d @ '1'..='7') => {
                        let mut digits = String::from(d);
                        for _ in 0..2 {
                            match self.peek() {
                                Some(c) if ('0'..='7').contains(&c) => {
                                    digits.push(c);
                                    self.bump();
                                }
                                _ => break,
                            }
                        }
                        let code = u32::from_str_radix(&digits, 8)
                            .map_err(|_| self.syntax_error("invalid octal escape value"))?;
                        if let Some(ch) = char::from_u32(code) {
                            value.push(ch);
                        } else {
                            return Err(self.syntax_error("invalid octal escape value"));
                        }
                    }
                    Some(other) => value.push(other),
                    None => return Err(self.syntax_error("unterminated string literal")),
                },
                Some(ch) => value.push(ch),
                None => return Err(self.syntax_error("unterminated string literal")),
            }
        }

        self.tokens.push(Token {
            kind: TokenKind::String(value),
            span: Span::new(start, self.offset),
        });
        Ok(())
    }

    /// Skip an optional UESCAPE 'x' clause that may follow U&'...' or U&"..." literals.
    fn skip_uescape(&mut self) {
        let _ = self.parse_uescape_char();
    }

    fn parse_uescape_char(&mut self) -> Option<char> {
        // Save position in case this isn't actually a UESCAPE clause
        let saved_offset = self.offset;
        let saved_tokens_len = self.tokens.len();
        // Skip whitespace
        while matches!(self.peek(), Some(ch) if ch.is_whitespace()) {
            self.bump();
        }
        // Check for UESCAPE keyword
        let remaining = &self.sql[self.offset..];
        if remaining.len() >= 7 && remaining[..7].eq_ignore_ascii_case("uescape") {
            // Check that the 8th char is not alphanumeric (to avoid matching "uescapex")
            if remaining.len() == 7 || !remaining.as_bytes()[7].is_ascii_alphanumeric() {
                // Consume UESCAPE
                for _ in 0..7 {
                    self.bump();
                }
                // Skip whitespace
                while matches!(self.peek(), Some(ch) if ch.is_whitespace()) {
                    self.bump();
                }
                // Consume the escape character (a string literal like '!')
                if self.peek() == Some('\'') {
                    let escape = self.lex_string_contents().ok().and_then(|value| {
                        let mut chars = value.chars();
                        match (chars.next(), chars.next()) {
                            (Some(ch), None) => Some(ch),
                            _ => None,
                        }
                    });
                    self.tokens.truncate(saved_tokens_len);
                    return escape;
                }
                // Not a valid UESCAPE clause - consume the next token anyway
                // to avoid leaving unconsumed tokens (e.g. UESCAPE +)
                if let Some(ch) = self.peek() {
                    if !ch.is_whitespace() && ch != ';' {
                        self.bump();
                    }
                }
                return None;
            }
        }
        // Not UESCAPE - restore position
        self.offset = saved_offset;
        self.tokens.truncate(saved_tokens_len);
        None
    }

    /// Lex a tagged dollar-quoted string: `$tag$body$tag$`
    /// where tag is `[A-Za-z_][A-Za-z0-9_]*`.
    fn lex_tagged_dollar_string(&mut self) -> DbResult<()> {
        let start = self.offset;

        // Build the opening tag: $tag$
        self.bump(); // consume opening '$'
        let mut tag = String::from("$");
        // PG identifier rules: tag starts with letter or underscore, then
        // letter/digit/underscore. Digit-start tags like `$1$body$1$` are
        // rejected (only positional parameters use that shape).
        match self.peek() {
            Some(ch) if ch == '_' || ch.is_ascii_alphabetic() => {
                let Some(ch) = self.bump() else {
                    return Err(self.syntax_error("invalid dollar-quoted string tag"));
                };
                tag.push(ch);
            }
            Some(_) | None => {
                return Err(
                    self.syntax_error("dollar-quoted tag must start with a letter or underscore")
                );
            }
        }
        while matches!(self.peek(), Some(ch) if ch == '_' || ch.is_ascii_alphanumeric()) {
            let Some(ch) = self.bump() else {
                return Err(self.syntax_error("invalid dollar-quoted string tag"));
            };
            tag.push(ch);
        }
        // Expect closing '$' of the opening tag
        match self.peek() {
            Some('$') => {
                self.bump();
                tag.push('$');
            }
            _ => {
                // Not a valid dollar-tag, treat as error
                return Err(self.syntax_error("invalid dollar-quoted string tag"));
            }
        }

        // Now read the body until we find $tag$
        let mut value = String::new();
        loop {
            match self.peek() {
                Some('$') => {
                    // Check if the tag follows
                    let remaining = &self.sql[self.offset..];
                    if remaining.starts_with(&tag) {
                        // consume the closing tag
                        for _ in 0..tag.len() {
                            self.bump();
                        }
                        break;
                    }
                    let Some(ch) = self.bump() else {
                        return Err(self.syntax_error("unterminated dollar-quoted string"));
                    };
                    value.push(ch);
                }
                Some(ch) => {
                    self.bump();
                    value.push(ch);
                }
                None => return Err(self.syntax_error("unterminated dollar-quoted string")),
            }
        }

        self.tokens.push(Token {
            kind: TokenKind::String(value),
            span: Span::new(start, self.offset),
        });
        Ok(())
    }

    /// Lex a psql colon-variable reference: `:varname`.
    /// Treated as a named parameter placeholder.
    fn lex_colon_variable(&mut self) -> DbResult<()> {
        let start = self.offset;
        self.bump(); // consume ':'
        let ident_start = self.offset;
        while matches!(self.peek(), Some(ch) if ch == '_' || ch.is_ascii_alphanumeric()) {
            self.bump();
        }
        let name = &self.sql[ident_start..self.offset];
        // If the identifier after ':' is a SQL keyword (e.g., :NULL in array
        // slice syntax `a[1:NULL]`), emit a Colon token followed by the keyword
        // rather than treating the whole thing as a psql variable.
        // Always emit Colon + Identifier/Keyword instead of a combined token.
        // This ensures both SQL (`:NULL` in array slice) and Cypher (`:Person` label)
        // work correctly.
        self.tokens.push(Token {
            kind: TokenKind::Colon,
            span: Span::new(start, ident_start),
        });
        if let Some(kw) = lookup_keyword(name) {
            self.tokens.push(Token {
                kind: TokenKind::Keyword(kw),
                span: Span::new(ident_start, self.offset),
            });
        } else {
            self.tokens.push(Token {
                kind: TokenKind::Identifier(name.to_owned()),
                span: Span::new(ident_start, self.offset),
            });
        }
        Ok(())
    }

    fn lex_dollar_string(&mut self) -> DbResult<()> {
        let start = self.offset;
        // consume opening $$
        self.bump();
        self.bump();

        let mut value = String::new();
        loop {
            match self.peek() {
                Some('$') if self.peek_next() == Some('$') => {
                    // consume closing $$
                    self.bump();
                    self.bump();
                    break;
                }
                Some(ch) => {
                    self.bump();
                    value.push(ch);
                }
                None => return Err(self.syntax_error("unterminated dollar-quoted string")),
            }
        }

        self.tokens.push(Token {
            kind: TokenKind::String(value),
            span: Span::new(start, self.offset),
        });
        Ok(())
    }

    fn syntax_error(&self, message: impl Into<String>) -> DbError {
        DbError::syntax_error(message).with_position(self.offset + 1)
    }
}

fn decode_unicode_escape_literal(input: &str, escape: char) -> Result<String, String> {
    let mut decoded = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let mut index = 0;

    while index < chars.len() {
        if chars[index] != escape {
            decoded.push(chars[index]);
            index += 1;
            continue;
        }

        if index + 1 >= chars.len() {
            return Err("invalid Unicode escape in string literal".to_owned());
        }

        if chars[index + 1] == escape {
            decoded.push(escape);
            index += 2;
            continue;
        }

        let (codepoint, consumed) = if chars[index + 1] == '+' {
            if index + 8 > chars.len() {
                return Err("invalid Unicode escape in string literal".to_owned());
            }
            let hex: String = chars[index + 2..index + 8].iter().collect();
            (
                u32::from_str_radix(&hex, 16)
                    .map_err(|_| "invalid Unicode escape in string literal".to_owned())?,
                8,
            )
        } else {
            if index + 5 > chars.len() {
                return Err("invalid Unicode escape in string literal".to_owned());
            }
            let hex: String = chars[index + 1..index + 5].iter().collect();
            if !hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
                return Err("invalid Unicode escape in string literal".to_owned());
            }
            (
                u32::from_str_radix(&hex, 16)
                    .map_err(|_| "invalid Unicode escape in string literal".to_owned())?,
                5,
            )
        };

        let ch = char::from_u32(codepoint)
            .ok_or_else(|| "invalid Unicode escape in string literal".to_owned())?;
        if (0xD800..=0xDFFF).contains(&codepoint) {
            return Err("invalid Unicode escape in string literal".to_owned());
        }
        decoded.push(ch);
        index += consumed;
    }

    Ok(decoded)
}

fn is_identifier_start(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphabetic()
}

fn is_identifier_continue(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphanumeric()
}

fn normalize_based_literal_digits(raw: &str, radix: u32) -> Option<String> {
    let mut cleaned = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    let mut saw_digit = false;
    let mut previous_was_digit = false;

    // PostgreSQL allows a single leading underscore in non-decimal prefixed
    // literals (e.g. 0b_10_0101).
    if matches!(chars.peek(), Some('_')) {
        chars.next();
    }

    while let Some(ch) = chars.next() {
        if ch == '_' {
            let next = chars.peek().copied()?;
            if !previous_was_digit || next.to_digit(radix).is_none() {
                return None;
            }
            previous_was_digit = false;
            continue;
        }

        ch.to_digit(radix)?;
        cleaned.push(ch.to_ascii_lowercase());
        previous_was_digit = true;
        saw_digit = true;
    }

    if !saw_digit {
        return None;
    }

    Some(cleaned)
}

fn radix_to_decimal_string(cleaned_digits: &str, radix: u32) -> Option<String> {
    if cleaned_digits.is_empty() {
        return None;
    }

    if let Ok(value) = u128::from_str_radix(cleaned_digits, radix) {
        return Some(value.to_string());
    }

    let mut decimal: Vec<u8> = vec![0];
    for ch in cleaned_digits.chars() {
        let digit = ch.to_digit(radix)?;
        let mut carry = digit;
        for value in decimal.iter_mut().rev() {
            let current = u32::from(*value) * radix + carry;
            *value = (current % 10) as u8;
            carry = current / 10;
        }
        while carry > 0 {
            decimal.insert(0, (carry % 10) as u8);
            carry /= 10;
        }
    }

    let mut out = String::with_capacity(decimal.len());
    for d in decimal {
        out.push(char::from(b'0' + d));
    }
    Some(out)
}

/// Normalize/validate a decimal literal with optional underscores, decimal
/// point, and exponent.
///
/// Returns `(normalized, is_decimal_form)` where `is_decimal_form` is true when
/// the source contained a decimal point or exponent.
/// On validation failure, returns the end index (within `raw`) of the snippet
/// that should be reported in "at or near".
fn normalize_decimal_literal(raw: &str) -> Result<(String, bool), usize> {
    let bytes = raw.as_bytes();
    let len = bytes.len();
    let mut idx = 0usize;
    let mut integer = String::new();
    let mut fraction = String::new();
    let mut exponent = String::new();
    let mut exp_sign: Option<char> = None;
    let mut has_dot = false;
    let mut has_exp = false;
    let starts_with_dot = bytes.first().copied() == Some(b'.');

    if !starts_with_dot {
        let int_start = idx;
        let int_had_digits = consume_decimal_digits_with_underscores(
            bytes,
            &mut idx,
            &mut integer,
            int_start,
            false,
        )?;
        if !int_had_digits {
            return Err(int_start);
        }
    }

    if idx < len && bytes[idx] == b'.' {
        has_dot = true;
        idx += 1;
        let frac_start = idx;
        let frac_had_digits = consume_decimal_digits_with_underscores(
            bytes,
            &mut idx,
            &mut fraction,
            frac_start,
            true,
        )?;
        if starts_with_dot && !frac_had_digits {
            return Err(frac_start);
        }
    }

    if idx < len && (bytes[idx] == b'e' || bytes[idx] == b'E') {
        has_exp = true;
        idx += 1;
        if idx < len && (bytes[idx] == b'+' || bytes[idx] == b'-') {
            exp_sign = Some(bytes[idx] as char);
            idx += 1;
        }
        let exp_start = idx;
        if idx >= len || !bytes[idx].is_ascii_digit() {
            // Include `e` and optional sign, but not the invalid first char.
            return Err(exp_start);
        }
        let exp_had_digits = consume_decimal_digits_with_underscores(
            bytes,
            &mut idx,
            &mut exponent,
            exp_start,
            false,
        )?;
        if !exp_had_digits {
            return Err(exp_start);
        }
    }

    if idx != len {
        return Err(idx + 1);
    }

    let mut normalized = String::with_capacity(raw.len());
    normalized.push_str(&integer);
    if !fraction.is_empty() {
        normalized.push('.');
        normalized.push_str(&fraction);
    }
    if has_exp {
        normalized.push('e');
        if let Some(sign) = exp_sign {
            normalized.push(sign);
        }
        normalized.push_str(&exponent);
    }

    // `1_000.` should normalize to `1000`, not `1000.`.
    if has_dot && fraction.is_empty() && !has_exp {
        return Ok((integer, false));
    }

    Ok((normalized, has_dot || has_exp))
}

fn consume_decimal_digits_with_underscores(
    bytes: &[u8],
    idx: &mut usize,
    out: &mut String,
    section_start: usize,
    allow_empty: bool,
) -> Result<bool, usize> {
    let mut had_digit = false;
    let mut previous_was_digit = false;

    while *idx < bytes.len() {
        let byte = bytes[*idx];
        if byte.is_ascii_digit() {
            out.push(char::from(byte));
            *idx += 1;
            had_digit = true;
            previous_was_digit = true;
            continue;
        }
        if byte == b'_' {
            let next = bytes.get(*idx + 1).copied();
            if !previous_was_digit || !next.is_some_and(|b| b.is_ascii_digit()) {
                return Err(*idx + 1);
            }
            previous_was_digit = false;
            *idx += 1;
            continue;
        }
        break;
    }

    if !allow_empty && !had_digit {
        return Err(section_start);
    }

    Ok(had_digit)
}

#[cfg(test)]
mod tests;
