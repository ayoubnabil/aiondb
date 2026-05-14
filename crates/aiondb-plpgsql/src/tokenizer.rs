//! PL/pgSQL tokenizer.
//!
//! Produces a stream of tokens preserving positional information so the parser
//! can emit SQL-state-aware errors. The tokenizer recognises PL/pgSQL-specific
//! constructs (dollar-quoted string bodies, labels, `%TYPE`, `%ROWTYPE`,
//! `:=`, `..`, etc.) as well as the generic SQL lexical elements it shares
//! with the rest of the planner.

use std::fmt;

/// A single PL/pgSQL token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub lexeme: String,
    /// 0-based byte offset into the original source.
    pub offset: usize,
    /// 1-based line (\n separated) for error reporting.
    pub line: usize,
    /// 1-based column within the line, in characters.
    pub column: usize,
}

/// Lexical category of a token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    /// An unquoted identifier (always stored lower-cased in `lexeme`).
    Ident,
    /// A double-quoted identifier (stored with quotes stripped, case preserved).
    QuotedIdent,
    /// A string literal (already unescaped). `tag` carries the dollar tag when
    /// the literal was dollar-quoted; otherwise it is empty.
    String { tag: String },
    /// A numeric literal kept as the original lexeme.
    Number,
    /// A colon-prefixed bind-like placeholder, e.g. `:foo`.
    Placeholder,
    /// A reserved PL/pgSQL keyword. The variant encodes the keyword.
    Keyword(Keyword),
    /// Punctuation/operator. The variant encodes the exact symbol.
    Punct(Punct),
    /// End of input sentinel.
    Eof,
}

/// Recognised PL/pgSQL keywords.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::upper_case_acronyms)]
pub enum Keyword {
    Alias,
    All,
    And,
    Array,
    As,
    Assert,
    Backward,
    Begin,
    By,
    Call,
    Case,
    Close,
    Constant,
    Continue,
    Cursor,
    Debug,
    Declare,
    Default,
    Diagnostics,
    Do,
    Elsif,
    Else,
    End,
    Exception,
    Execute,
    Exit,
    False,
    Fetch,
    For,
    Foreach,
    Forward,
    From,
    Get,
    If,
    In,
    Info,
    Insert,
    Into,
    Is,
    Language,
    Log,
    Loop,
    Move,
    Next,
    No,
    Not,
    Notice,
    Null,
    Of,
    Open,
    Or,
    Others,
    Perform,
    Query,
    Raise,
    Record,
    Refcursor,
    Return,
    Reverse,
    Row,
    Rowtype,
    Select,
    Slice,
    Sqlstate,
    Stacked,
    Strict,
    Then,
    To,
    True,
    Type,
    Use,
    Using,
    Warning,
    When,
    While,
}

impl Keyword {
    /// Returns the canonical lower-case spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Alias => "alias",
            Self::All => "all",
            Self::And => "and",
            Self::Array => "array",
            Self::As => "as",
            Self::Assert => "assert",
            Self::Backward => "backward",
            Self::Begin => "begin",
            Self::By => "by",
            Self::Call => "call",
            Self::Case => "case",
            Self::Close => "close",
            Self::Constant => "constant",
            Self::Continue => "continue",
            Self::Cursor => "cursor",
            Self::Debug => "debug",
            Self::Declare => "declare",
            Self::Default => "default",
            Self::Diagnostics => "diagnostics",
            Self::Do => "do",
            Self::Else => "else",
            Self::Elsif => "elsif",
            Self::End => "end",
            Self::Exception => "exception",
            Self::Execute => "execute",
            Self::Exit => "exit",
            Self::False => "false",
            Self::Fetch => "fetch",
            Self::For => "for",
            Self::Foreach => "foreach",
            Self::Forward => "forward",
            Self::From => "from",
            Self::Get => "get",
            Self::If => "if",
            Self::In => "in",
            Self::Info => "info",
            Self::Insert => "insert",
            Self::Into => "into",
            Self::Is => "is",
            Self::Language => "language",
            Self::Log => "log",
            Self::Loop => "loop",
            Self::Move => "move",
            Self::Next => "next",
            Self::No => "no",
            Self::Not => "not",
            Self::Notice => "notice",
            Self::Null => "null",
            Self::Of => "of",
            Self::Open => "open",
            Self::Or => "or",
            Self::Others => "others",
            Self::Perform => "perform",
            Self::Query => "query",
            Self::Raise => "raise",
            Self::Record => "record",
            Self::Refcursor => "refcursor",
            Self::Return => "return",
            Self::Reverse => "reverse",
            Self::Row => "row",
            Self::Rowtype => "rowtype",
            Self::Select => "select",
            Self::Slice => "slice",
            Self::Sqlstate => "sqlstate",
            Self::Stacked => "stacked",
            Self::Strict => "strict",
            Self::Then => "then",
            Self::To => "to",
            Self::True => "true",
            Self::Type => "type",
            Self::Use => "use",
            Self::Using => "using",
            Self::Warning => "warning",
            Self::When => "when",
            Self::While => "while",
        }
    }

    fn lookup(word: &str) -> Option<Self> {
        match word {
            "alias" => Some(Self::Alias),
            "all" => Some(Self::All),
            "and" => Some(Self::And),
            "array" => Some(Self::Array),
            "as" => Some(Self::As),
            "assert" => Some(Self::Assert),
            "backward" => Some(Self::Backward),
            "begin" => Some(Self::Begin),
            "by" => Some(Self::By),
            "call" => Some(Self::Call),
            "case" => Some(Self::Case),
            "close" => Some(Self::Close),
            "constant" => Some(Self::Constant),
            "continue" => Some(Self::Continue),
            "cursor" => Some(Self::Cursor),
            "debug" => Some(Self::Debug),
            "declare" => Some(Self::Declare),
            "default" => Some(Self::Default),
            "diagnostics" => Some(Self::Diagnostics),
            "do" => Some(Self::Do),
            "else" => Some(Self::Else),
            "elsif" | "elseif" => Some(Self::Elsif),
            "end" => Some(Self::End),
            "exception" => Some(Self::Exception),
            "execute" => Some(Self::Execute),
            "exit" => Some(Self::Exit),
            "false" => Some(Self::False),
            "fetch" => Some(Self::Fetch),
            "for" => Some(Self::For),
            "foreach" => Some(Self::Foreach),
            "forward" => Some(Self::Forward),
            "from" => Some(Self::From),
            "get" => Some(Self::Get),
            "if" => Some(Self::If),
            "in" => Some(Self::In),
            "info" => Some(Self::Info),
            "insert" => Some(Self::Insert),
            "into" => Some(Self::Into),
            "is" => Some(Self::Is),
            "language" => Some(Self::Language),
            "log" => Some(Self::Log),
            "loop" => Some(Self::Loop),
            "move" => Some(Self::Move),
            "next" => Some(Self::Next),
            "no" => Some(Self::No),
            "not" => Some(Self::Not),
            "notice" => Some(Self::Notice),
            "null" => Some(Self::Null),
            "of" => Some(Self::Of),
            "open" => Some(Self::Open),
            "or" => Some(Self::Or),
            "others" => Some(Self::Others),
            "perform" => Some(Self::Perform),
            "query" => Some(Self::Query),
            "raise" => Some(Self::Raise),
            "record" => Some(Self::Record),
            "refcursor" => Some(Self::Refcursor),
            "return" => Some(Self::Return),
            "reverse" => Some(Self::Reverse),
            "row" => Some(Self::Row),
            "rowtype" => Some(Self::Rowtype),
            "select" => Some(Self::Select),
            "slice" => Some(Self::Slice),
            "sqlstate" => Some(Self::Sqlstate),
            "stacked" => Some(Self::Stacked),
            "strict" => Some(Self::Strict),
            "then" => Some(Self::Then),
            "to" => Some(Self::To),
            "true" => Some(Self::True),
            "type" => Some(Self::Type),
            "use" => Some(Self::Use),
            "using" => Some(Self::Using),
            "warning" => Some(Self::Warning),
            "when" => Some(Self::When),
            "while" => Some(Self::While),
            _ => None,
        }
    }
}

/// Recognised punctuation / operator tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Punct {
    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
    Semicolon,
    Colon,
    ColonColon,
    Dot,
    DotDot,
    Percent,
    Assign,
    Equal,
    Star,
    Plus,
    Minus,
    Slash,
    Caret,
    Lt,
    Gt,
    LtEq,
    GtEq,
    NotEq,
    Bang,
    Question,
    Amp,
    Pipe,
    Tilde,
    At,
    Hash,
    BackSlash,
}

impl fmt::Display for Punct {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::LParen => "(",
            Self::RParen => ")",
            Self::LBracket => "[",
            Self::RBracket => "]",
            Self::Comma => ",",
            Self::Semicolon => ";",
            Self::Colon => ":",
            Self::ColonColon => "::",
            Self::Dot => ".",
            Self::DotDot => "..",
            Self::Percent => "%",
            Self::Assign => ":=",
            Self::Equal => "=",
            Self::Star => "*",
            Self::Plus => "+",
            Self::Minus => "-",
            Self::Slash => "/",
            Self::Caret => "^",
            Self::Lt => "<",
            Self::Gt => ">",
            Self::LtEq => "<=",
            Self::GtEq => ">=",
            Self::NotEq => "<>",
            Self::Bang => "!",
            Self::Question => "?",
            Self::Amp => "&",
            Self::Pipe => "|",
            Self::Tilde => "~",
            Self::At => "@",
            Self::Hash => "#",
            Self::BackSlash => "\\",
        };
        f.write_str(s)
    }
}

/// A tokenisation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenizeError {
    pub message: String,
    pub offset: usize,
    pub line: usize,
    pub column: usize,
}

impl fmt::Display for TokenizeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "tokenize error at line {} column {}: {}",
            self.line, self.column, self.message
        )
    }
}

impl std::error::Error for TokenizeError {}

struct Scanner<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    line: usize,
    col: usize,
}

impl<'a> Scanner<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            src,
            bytes: src.as_bytes(),
            pos: 0,
            line: 1,
            col: 1,
        }
    }

    fn peek(&self, n: usize) -> Option<u8> {
        self.bytes.get(self.pos + n).copied()
    }

    fn advance(&mut self) -> Option<u8> {
        let b = self.bytes.get(self.pos).copied()?;
        self.pos += 1;
        if b == b'\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(b)
    }

    fn skip_whitespace_and_comments(&mut self) -> Result<(), TokenizeError> {
        loop {
            match self.peek(0) {
                Some(b) if b.is_ascii_whitespace() => {
                    self.advance();
                }
                Some(b'-') if self.peek(1) == Some(b'-') => {
                    while let Some(b) = self.peek(0) {
                        if b == b'\n' {
                            break;
                        }
                        self.advance();
                    }
                }
                Some(b'/') if self.peek(1) == Some(b'*') => {
                    let start_line = self.line;
                    let start_col = self.col;
                    self.advance();
                    self.advance();
                    let mut depth: usize = 1;
                    while depth > 0 {
                        match self.peek(0) {
                            Some(b'/') if self.peek(1) == Some(b'*') => {
                                self.advance();
                                self.advance();
                                depth += 1;
                            }
                            Some(b'*') if self.peek(1) == Some(b'/') => {
                                self.advance();
                                self.advance();
                                depth -= 1;
                            }
                            Some(_) => {
                                self.advance();
                            }
                            None => {
                                return Err(TokenizeError {
                                    message: "unterminated /* block comment".to_owned(),
                                    offset: self.pos,
                                    line: start_line,
                                    column: start_col,
                                });
                            }
                        }
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    fn read_identifier(&mut self) -> Token {
        let start = self.pos;
        let line = self.line;
        let col = self.col;
        while let Some(b) = self.peek(0) {
            if b.is_ascii_alphanumeric() || b == b'_' || b == b'$' {
                self.advance();
            } else {
                break;
            }
        }
        let lexeme = &self.src[start..self.pos];
        let lower = lexeme.to_ascii_lowercase();
        let kind = if let Some(keyword) = Keyword::lookup(&lower) {
            TokenKind::Keyword(keyword)
        } else {
            TokenKind::Ident
        };
        Token {
            kind,
            lexeme: lower,
            offset: start,
            line,
            column: col,
        }
    }

    fn read_quoted_identifier(&mut self) -> Result<Token, TokenizeError> {
        let start = self.pos;
        let line = self.line;
        let col = self.col;
        self.advance();
        let mut buf = String::new();
        loop {
            match self.peek(0) {
                Some(b'"') if self.peek(1) == Some(b'"') => {
                    buf.push('"');
                    self.advance();
                    self.advance();
                }
                Some(b'"') => {
                    self.advance();
                    return Ok(Token {
                        kind: TokenKind::QuotedIdent,
                        lexeme: buf,
                        offset: start,
                        line,
                        column: col,
                    });
                }
                Some(_) => {
                    let Some(b) = self.advance() else {
                        return Err(TokenizeError {
                            message: "unterminated quoted identifier".to_owned(),
                            offset: start,
                            line,
                            column: col,
                        });
                    };
                    buf.push(b as char);
                }
                None => {
                    return Err(TokenizeError {
                        message: "unterminated quoted identifier".to_owned(),
                        offset: start,
                        line,
                        column: col,
                    });
                }
            }
        }
    }

    fn read_string(&mut self) -> Result<Token, TokenizeError> {
        let start = self.pos;
        let line = self.line;
        let col = self.col;
        self.advance();
        let mut buf = String::new();
        loop {
            match self.peek(0) {
                Some(b'\'') if self.peek(1) == Some(b'\'') => {
                    buf.push('\'');
                    self.advance();
                    self.advance();
                }
                Some(b'\'') => {
                    self.advance();
                    return Ok(Token {
                        kind: TokenKind::String { tag: String::new() },
                        lexeme: buf,
                        offset: start,
                        line,
                        column: col,
                    });
                }
                Some(_) => {
                    let Some(b) = self.advance() else {
                        return Err(TokenizeError {
                            message: "unterminated string literal".to_owned(),
                            offset: start,
                            line,
                            column: col,
                        });
                    };
                    buf.push(b as char);
                }
                None => {
                    return Err(TokenizeError {
                        message: "unterminated string literal".to_owned(),
                        offset: start,
                        line,
                        column: col,
                    });
                }
            }
        }
    }

    /// Attempt to read a dollar-quoted string. Returns `Ok(Some(_))` if a
    /// dollar literal was recognised, `Ok(None)` if the `$` starts something
    /// else (placeholder, identifier continuation).
    fn read_dollar_string(&mut self) -> Result<Option<Token>, TokenizeError> {
        let start = self.pos;
        let start_line = self.line;
        let start_col = self.col;
        let mut end_tag_start = start + 1;
        while let Some(b) = self.bytes.get(end_tag_start).copied() {
            if b == b'$' {
                break;
            }
            if !(b.is_ascii_alphanumeric() || b == b'_') {
                return Ok(None);
            }
            end_tag_start += 1;
        }
        if end_tag_start >= self.bytes.len() {
            return Ok(None);
        }
        let tag = &self.src[start + 1..end_tag_start];
        let close = format!("${tag}$");
        let body_start = end_tag_start + 1;
        let Some(end_pos) = self.src[body_start..].find(&close).map(|i| body_start + i) else {
            return Err(TokenizeError {
                message: format!("unterminated dollar-quoted string $${tag}$$"),
                offset: start,
                line: start_line,
                column: start_col,
            });
        };
        let body = self.src[body_start..end_pos].to_owned();
        let new_end = end_pos + close.len();
        while self.pos < new_end {
            self.advance();
        }
        Ok(Some(Token {
            kind: TokenKind::String {
                tag: tag.to_owned(),
            },
            lexeme: body,
            offset: start,
            line: start_line,
            column: start_col,
        }))
    }

    fn read_number(&mut self) -> Token {
        let start = self.pos;
        let line = self.line;
        let col = self.col;
        while let Some(b) = self.peek(0) {
            if b.is_ascii_digit() {
                self.advance();
            } else {
                break;
            }
        }
        if self.peek(0) == Some(b'.') && self.peek(1) != Some(b'.') {
            self.advance();
            while let Some(b) = self.peek(0) {
                if b.is_ascii_digit() {
                    self.advance();
                } else {
                    break;
                }
            }
        }
        if matches!(self.peek(0), Some(b'e' | b'E')) {
            self.advance();
            if matches!(self.peek(0), Some(b'+' | b'-')) {
                self.advance();
            }
            while let Some(b) = self.peek(0) {
                if b.is_ascii_digit() {
                    self.advance();
                } else {
                    break;
                }
            }
        }
        Token {
            kind: TokenKind::Number,
            lexeme: self.src[start..self.pos].to_owned(),
            offset: start,
            line,
            column: col,
        }
    }
}

/// Tokenise PL/pgSQL source into a vector terminated by [`TokenKind::Eof`].
pub fn tokenize(src: &str) -> Result<Vec<Token>, TokenizeError> {
    let mut scanner = Scanner::new(src);
    let mut tokens = Vec::new();
    loop {
        scanner.skip_whitespace_and_comments()?;
        let line = scanner.line;
        let col = scanner.col;
        let offset = scanner.pos;
        let Some(b) = scanner.peek(0) else {
            tokens.push(Token {
                kind: TokenKind::Eof,
                lexeme: String::new(),
                offset,
                line,
                column: col,
            });
            return Ok(tokens);
        };
        if b.is_ascii_alphabetic() || b == b'_' {
            tokens.push(scanner.read_identifier());
            continue;
        }
        if b == b'"' {
            tokens.push(scanner.read_quoted_identifier()?);
            continue;
        }
        if b == b'\'' {
            tokens.push(scanner.read_string()?);
            continue;
        }
        if b == b'$' {
            if let Some(token) = scanner.read_dollar_string()? {
                tokens.push(token);
                continue;
            }
            // `$1`, `$2`, ... are SQL positional parameter references that
            // PL/pgSQL uses inside function bodies that predate the named
            // parameter style. Tokenise them as identifiers whose name is
            // the exact `$N` lexeme so downstream substitution can resolve
            // them against the interpreter's parameter bindings.
            if scanner.peek(1).is_some_and(|c| c.is_ascii_digit()) {
                scanner.advance(); // consume `$`
                let digits_start = scanner.pos;
                while let Some(c) = scanner.peek(0) {
                    if c.is_ascii_digit() {
                        scanner.advance();
                    } else {
                        break;
                    }
                }
                let lexeme = format!("${}", &scanner.src[digits_start..scanner.pos]);
                tokens.push(Token {
                    kind: TokenKind::Ident,
                    lexeme,
                    offset,
                    line,
                    column: col,
                });
                continue;
            }
            return Err(TokenizeError {
                message: "unexpected `$` outside of dollar-quoted literal".to_owned(),
                offset,
                line,
                column: col,
            });
        }
        if b.is_ascii_digit() {
            tokens.push(scanner.read_number());
            continue;
        }
        // Punctuation
        let punct = match b {
            b'(' => Some(Punct::LParen),
            b')' => Some(Punct::RParen),
            b'[' => Some(Punct::LBracket),
            b']' => Some(Punct::RBracket),
            b',' => Some(Punct::Comma),
            b';' => Some(Punct::Semicolon),
            b'%' => Some(Punct::Percent),
            b'=' => Some(Punct::Equal),
            b'*' => Some(Punct::Star),
            b'+' => Some(Punct::Plus),
            b'-' => Some(Punct::Minus),
            b'/' => Some(Punct::Slash),
            b'^' => Some(Punct::Caret),
            b'!' => Some(Punct::Bang),
            b'?' => Some(Punct::Question),
            b'&' => Some(Punct::Amp),
            b'|' => Some(Punct::Pipe),
            b'~' => Some(Punct::Tilde),
            b'@' => Some(Punct::At),
            b'#' => Some(Punct::Hash),
            b'\\' => Some(Punct::BackSlash),
            _ => None,
        };
        if let Some(p) = punct {
            // Two-character comparison operators: <=, >=, <>.
            if matches!(
                p,
                Punct::Percent
                    | Punct::Equal
                    | Punct::Star
                    | Punct::Plus
                    | Punct::Minus
                    | Punct::Slash
                    | Punct::Caret
                    | Punct::Bang
                    | Punct::Question
                    | Punct::Amp
                    | Punct::Pipe
                    | Punct::Tilde
                    | Punct::At
                    | Punct::Hash
                    | Punct::BackSlash
                    | Punct::LParen
                    | Punct::RParen
                    | Punct::LBracket
                    | Punct::RBracket
                    | Punct::Comma
                    | Punct::Semicolon
            ) {
                scanner.advance();
                tokens.push(Token {
                    kind: TokenKind::Punct(p),
                    lexeme: p.to_string(),
                    offset,
                    line,
                    column: col,
                });
                continue;
            }
            scanner.advance();
            tokens.push(Token {
                kind: TokenKind::Punct(p),
                lexeme: p.to_string(),
                offset,
                line,
                column: col,
            });
            continue;
        }
        if b == b'<' {
            scanner.advance();
            let punct = match scanner.peek(0) {
                Some(b'=') => {
                    scanner.advance();
                    Punct::LtEq
                }
                Some(b'>') => {
                    scanner.advance();
                    Punct::NotEq
                }
                _ => Punct::Lt,
            };
            tokens.push(Token {
                kind: TokenKind::Punct(punct),
                lexeme: punct.to_string(),
                offset,
                line,
                column: col,
            });
            continue;
        }
        if b == b'>' {
            scanner.advance();
            let punct = if scanner.peek(0) == Some(b'=') {
                scanner.advance();
                Punct::GtEq
            } else {
                Punct::Gt
            };
            tokens.push(Token {
                kind: TokenKind::Punct(punct),
                lexeme: punct.to_string(),
                offset,
                line,
                column: col,
            });
            continue;
        }
        if b == b':' {
            scanner.advance();
            if scanner.peek(0) == Some(b'=') {
                scanner.advance();
                tokens.push(Token {
                    kind: TokenKind::Punct(Punct::Assign),
                    lexeme: ":=".to_owned(),
                    offset,
                    line,
                    column: col,
                });
                continue;
            }
            if scanner.peek(0) == Some(b':') {
                scanner.advance();
                tokens.push(Token {
                    kind: TokenKind::Punct(Punct::ColonColon),
                    lexeme: "::".to_owned(),
                    offset,
                    line,
                    column: col,
                });
                continue;
            }
            // Bare `:` becomes either placeholder when followed by an ident, or
            // a plain colon punctuation used in a few grammar rules.
            if matches!(scanner.peek(0), Some(b) if b.is_ascii_alphabetic() || b == b'_') {
                let ident_start = scanner.pos;
                while let Some(b) = scanner.peek(0) {
                    if b.is_ascii_alphanumeric() || b == b'_' {
                        scanner.advance();
                    } else {
                        break;
                    }
                }
                let name = &scanner.src[ident_start..scanner.pos];
                tokens.push(Token {
                    kind: TokenKind::Placeholder,
                    lexeme: name.to_ascii_lowercase(),
                    offset,
                    line,
                    column: col,
                });
                continue;
            }
            tokens.push(Token {
                kind: TokenKind::Punct(Punct::Colon),
                lexeme: ":".to_owned(),
                offset,
                line,
                column: col,
            });
            continue;
        }
        if b == b'.' {
            scanner.advance();
            if scanner.peek(0) == Some(b'.') {
                scanner.advance();
                tokens.push(Token {
                    kind: TokenKind::Punct(Punct::DotDot),
                    lexeme: "..".to_owned(),
                    offset,
                    line,
                    column: col,
                });
                continue;
            }
            tokens.push(Token {
                kind: TokenKind::Punct(Punct::Dot),
                lexeme: ".".to_owned(),
                offset,
                line,
                column: col,
            });
            continue;
        }
        return Err(TokenizeError {
            message: format!("unexpected character `{}`", b as char),
            offset,
            line,
            column: col,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<TokenKind> {
        tokenize(src).unwrap().into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn tokenises_keywords_and_operators() {
        let kinds = kinds("DECLARE x INT := 1; BEGIN END");
        assert_eq!(
            kinds,
            vec![
                TokenKind::Keyword(Keyword::Declare),
                TokenKind::Ident,
                TokenKind::Ident,
                TokenKind::Punct(Punct::Assign),
                TokenKind::Number,
                TokenKind::Punct(Punct::Semicolon),
                TokenKind::Keyword(Keyword::Begin),
                TokenKind::Keyword(Keyword::End),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn dollar_quoted_string_captures_tag_and_body() {
        let toks = tokenize("$body$ hello world $body$").unwrap();
        assert_eq!(toks.len(), 2);
        match &toks[0].kind {
            TokenKind::String { tag } => assert_eq!(tag, "body"),
            other => panic!("expected string, got {other:?}"),
        }
        assert_eq!(toks[0].lexeme, " hello world ");
    }

    #[test]
    fn single_quoted_string_unescapes_double_apostrophes() {
        let toks = tokenize("'it''s ok'").unwrap();
        assert_eq!(toks[0].kind, TokenKind::String { tag: String::new() });
        assert_eq!(toks[0].lexeme, "it's ok");
    }

    #[test]
    fn line_comments_and_block_comments_are_ignored() {
        let kinds = kinds("-- comment\n/* outer /* inner */ */ x");
        assert_eq!(kinds, vec![TokenKind::Ident, TokenKind::Eof]);
    }

    #[test]
    fn range_and_cast_operators_distinct_from_dot() {
        let toks = tokenize("a..b::c.d").unwrap();
        let kinds: Vec<_> = toks.into_iter().map(|t| t.kind).collect();
        assert_eq!(
            kinds,
            vec![
                TokenKind::Ident,
                TokenKind::Punct(Punct::DotDot),
                TokenKind::Ident,
                TokenKind::Punct(Punct::ColonColon),
                TokenKind::Ident,
                TokenKind::Punct(Punct::Dot),
                TokenKind::Ident,
                TokenKind::Eof,
            ]
        );
    }
}
