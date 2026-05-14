use crate::{keywords::Keyword, span::Span};

#[derive(Clone, Debug, PartialEq)]
pub enum TokenKind {
    Keyword(Keyword),
    Identifier(String),
    Integer(i64),
    NumericLiteral(String),
    Parameter(usize),
    String(String),
    Comma,
    Dot,
    Eq,
    Ge,
    Gt,
    Le,
    LParen,
    Lt,
    Ne,
    RParen,
    LBracket,
    RBracket,
    Semicolon,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    PipePipe,
    Arrow,
    DoubleArrow,
    HashArrow,
    HashDoubleArrow,
    HashMinus,
    AtGt,
    LtAt,
    /// pgvector L2 distance operator `<->`.
    VecL2,
    /// pgvector cosine distance operator `<=>`.
    VecCosine,
    /// pgvector negative inner-product operator `<#>`.
    VecNegInner,
    /// pgvector L1/taxicab distance operator `<+>`.
    VecL1,
    /// pgvector Hamming distance operator `<~>`.
    VecHamming,
    /// pgvector Jaccard distance operator `<%>`.
    VecJaccard,
    Question,
    QuestionPipe,
    QuestionAmpersand,
    AmpAmp,
    AtAt,
    AtQuestion,
    DoubleColon,
    Tilde,
    TildeEq,
    TildeStar,
    ExclTilde,
    ExclTildeStar,
    Ampersand,
    Pipe,
    Hash,
    Caret,
    ShiftLeft,
    ShiftRight,
    At,
    /// Generic custom operator (geometric, etc.) - treated as binary op
    CustomOp,
    /// Bare colon - used in array slicing (e.g., `a[1:5]`)
    Colon,
    /// Left curly brace
    LBrace,
    /// Right curly brace
    RBrace,
    /// Bare exclamation mark (e.g., `!!` factorial)
    Excl,
    /// Fat arrow `=>` - used for named function arguments (e.g., `f(name => value)`)
    FatArrow,
    Eof,
}

impl TokenKind {
    pub fn describe(&self) -> String {
        match self {
            Self::Keyword(keyword) => format!("keyword {}", keyword.name()),
            Self::Identifier(value) => format!("identifier {value}"),
            Self::Integer(value) => format!("integer {value}"),
            Self::NumericLiteral(value) => format!("numeric literal {value}"),
            Self::Parameter(index) => format!("parameter ${index}"),
            Self::String(_) => "string literal".to_owned(),
            Self::Comma => "','".to_owned(),
            Self::Dot => "'.'".to_owned(),
            Self::Eq => "'='".to_owned(),
            Self::Ge => "'>='".to_owned(),
            Self::Gt => "'>'".to_owned(),
            Self::Le => "'<='".to_owned(),
            Self::LParen => "'('".to_owned(),
            Self::Lt => "'<'".to_owned(),
            Self::Ne => "'!='".to_owned(),
            Self::RParen => "')'".to_owned(),
            Self::LBracket => "'['".to_owned(),
            Self::RBracket => "']'".to_owned(),
            Self::Semicolon => "';'".to_owned(),
            Self::Plus => "'+'".to_owned(),
            Self::Minus => "'-'".to_owned(),
            Self::Star => "'*'".to_owned(),
            Self::Slash => "'/'".to_owned(),
            Self::Percent => "'%'".to_owned(),
            Self::PipePipe => "'||'".to_owned(),
            Self::Arrow => "'->'".to_owned(),
            Self::DoubleArrow => "'->>'".to_owned(),
            Self::HashArrow => "'#>'".to_owned(),
            Self::HashDoubleArrow => "'#>>'".to_owned(),
            Self::HashMinus => "'#-'".to_owned(),
            Self::AtGt => "'@>'".to_owned(),
            Self::LtAt => "'<@'".to_owned(),
            Self::VecL2 => "'<->'".to_owned(),
            Self::VecCosine => "'<=>'".to_owned(),
            Self::VecNegInner => "'<#>'".to_owned(),
            Self::VecL1 => "'<+>'".to_owned(),
            Self::VecHamming => "'<~>'".to_owned(),
            Self::VecJaccard => "'<%>'".to_owned(),
            Self::Question => "'?'".to_owned(),
            Self::QuestionPipe => "'?|'".to_owned(),
            Self::QuestionAmpersand => "'?&'".to_owned(),
            Self::AmpAmp => "'&&'".to_owned(),
            Self::AtAt => "'@@'".to_owned(),
            Self::AtQuestion => "'@?'".to_owned(),
            Self::DoubleColon => "'::'".to_owned(),
            Self::Tilde => "'~'".to_owned(),
            Self::TildeEq => "'~='".to_owned(),
            Self::TildeStar => "'~*'".to_owned(),
            Self::ExclTilde => "'!~'".to_owned(),
            Self::ExclTildeStar => "'!~*'".to_owned(),
            Self::Ampersand => "'&'".to_owned(),
            Self::Pipe => "'|'".to_owned(),
            Self::Hash => "'#'".to_owned(),
            Self::Caret => "'^'".to_owned(),
            Self::ShiftLeft => "'<<'".to_owned(),
            Self::ShiftRight => "'>>'".to_owned(),
            Self::At => "'@'".to_owned(),
            Self::CustomOp => "custom operator".to_owned(),
            Self::Colon => "':'".to_owned(),
            Self::LBrace => "'{'".to_owned(),
            Self::RBrace => "'}'".to_owned(),
            Self::Excl => "'!'".to_owned(),
            Self::FatArrow => "'=>'".to_owned(),
            Self::Eof => "end of input".to_owned(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl Token {
    pub fn eof(position: usize) -> Self {
        Self {
            kind: TokenKind::Eof,
            span: Span::new(position, position),
        }
    }
}

/// Reconstruct approximate SQL text from a slice of tokens.
///
/// The output may differ cosmetically from the original source (extra
/// spaces, lost comments) but it preserves the semantic content.  This
/// is used to capture function bodies from inline SQL-standard syntax
/// (`BEGIN ATOMIC ... END`, `RETURN expr`).
pub fn tokens_to_sql(tokens: &[Token]) -> String {
    let mut out = String::new();
    for tok in tokens {
        if !out.is_empty() {
            out.push(' ');
        }
        match &tok.kind {
            TokenKind::Keyword(kw) => out.push_str(kw.name()),
            TokenKind::Identifier(s) => out.push_str(s),
            TokenKind::Integer(n) => out.push_str(&n.to_string()),
            TokenKind::NumericLiteral(s) => out.push_str(s),
            TokenKind::Parameter(idx) => {
                out.push('$');
                out.push_str(&idx.to_string());
            }
            TokenKind::String(s) => {
                out.push('\'');
                out.push_str(&s.replace('\'', "''"));
                out.push('\'');
            }
            TokenKind::Comma => out.push(','),
            TokenKind::Dot => out.push('.'),
            TokenKind::Eq => out.push('='),
            TokenKind::Ge => out.push_str(">="),
            TokenKind::Gt => out.push('>'),
            TokenKind::Le => out.push_str("<="),
            TokenKind::LParen => out.push('('),
            TokenKind::Lt => out.push('<'),
            TokenKind::Ne => out.push_str("!="),
            TokenKind::RParen => out.push(')'),
            TokenKind::LBracket => out.push('['),
            TokenKind::RBracket => out.push(']'),
            TokenKind::Semicolon => out.push(';'),
            TokenKind::Plus => out.push('+'),
            TokenKind::Minus => out.push('-'),
            TokenKind::Star => out.push('*'),
            TokenKind::Slash => out.push('/'),
            TokenKind::Percent => out.push('%'),
            TokenKind::PipePipe => out.push_str("||"),
            TokenKind::Arrow => out.push_str("->"),
            TokenKind::DoubleArrow => out.push_str("->>"),
            TokenKind::HashArrow => out.push_str("#>"),
            TokenKind::HashDoubleArrow => out.push_str("#>>"),
            TokenKind::HashMinus => out.push_str("#-"),
            TokenKind::AtGt => out.push_str("@>"),
            TokenKind::LtAt => out.push_str("<@"),
            TokenKind::VecL2 => out.push_str("<->"),
            TokenKind::VecCosine => out.push_str("<=>"),
            TokenKind::VecNegInner => out.push_str("<#>"),
            TokenKind::VecL1 => out.push_str("<+>"),
            TokenKind::VecHamming => out.push_str("<~>"),
            TokenKind::VecJaccard => out.push_str("<%>"),
            TokenKind::Question => out.push('?'),
            TokenKind::QuestionPipe => out.push_str("?|"),
            TokenKind::QuestionAmpersand => out.push_str("?&"),
            TokenKind::AmpAmp => out.push_str("&&"),
            TokenKind::AtAt => out.push_str("@@"),
            TokenKind::AtQuestion => out.push_str("@?"),
            TokenKind::DoubleColon => out.push_str("::"),
            TokenKind::Tilde => out.push('~'),
            TokenKind::TildeEq => out.push_str("~="),
            TokenKind::TildeStar => out.push_str("~*"),
            TokenKind::ExclTilde => out.push_str("!~"),
            TokenKind::ExclTildeStar => out.push_str("!~*"),
            TokenKind::Ampersand => out.push('&'),
            TokenKind::Pipe => out.push('|'),
            TokenKind::Hash => out.push('#'),
            TokenKind::Caret => out.push('^'),
            TokenKind::ShiftLeft => out.push_str("<<"),
            TokenKind::ShiftRight => out.push_str(">>"),
            TokenKind::At => out.push('@'),
            TokenKind::CustomOp => out.push_str("??"),
            TokenKind::Colon => out.push(':'),
            TokenKind::LBrace => out.push('{'),
            TokenKind::RBrace => out.push('}'),
            TokenKind::Excl => out.push('!'),
            TokenKind::FatArrow => out.push_str("=>"),
            TokenKind::Eof => {}
        }
    }
    out
}

#[cfg(test)]
mod tests;
