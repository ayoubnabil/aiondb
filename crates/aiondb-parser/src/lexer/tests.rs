use aiondb_core::SqlState;

use super::*;
use crate::keywords::Keyword;

// ── Empty / whitespace ──────────────────────────────────────────

#[test]
fn empty_string_produces_only_eof() {
    let tokens = lex_sql("").unwrap();
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].kind, TokenKind::Eof);
    assert_eq!(tokens[0].span, Span::new(0, 0));
}

#[test]
fn whitespace_only_produces_only_eof() {
    let tokens = lex_sql("   \t\n\r  ").unwrap();
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].kind, TokenKind::Eof);
}

// ── Integers ────────────────────────────────────────────────────

#[test]
fn single_integer_42() {
    let tokens = lex_sql("42").unwrap();
    assert_eq!(tokens.len(), 2);
    assert_eq!(tokens[0].kind, TokenKind::Integer(42));
    assert_eq!(tokens[0].span, Span::new(0, 2));
    assert_eq!(tokens[1].kind, TokenKind::Eof);
}

#[test]
fn single_parameter_1() {
    let tokens = lex_sql("$1").unwrap();
    assert_eq!(tokens.len(), 2);
    assert_eq!(tokens[0].kind, TokenKind::Parameter(1));
    assert_eq!(tokens[0].span, Span::new(0, 2));
    assert_eq!(tokens[1].kind, TokenKind::Eof);
}

#[test]
fn parameter_zero_is_rejected() {
    let error = lex_sql("$0").expect_err("invalid parameter");
    assert_eq!(error.sqlstate(), SqlState::SyntaxError);
}

#[test]
fn integer_at_i64_max_boundary() {
    let tokens = lex_sql("9223372036854775807").unwrap();
    assert_eq!(tokens.len(), 2);
    assert_eq!(tokens[0].kind, TokenKind::Integer(i64::MAX));
}

#[test]
fn integer_overflow_larger_than_i64_max_becomes_numeric_literal() {
    let tokens = lex_sql("9223372036854775808").unwrap();
    assert_eq!(tokens.len(), 2);
    assert_eq!(
        tokens[0].kind,
        TokenKind::NumericLiteral("9223372036854775808".to_owned())
    );
}

// ── Identifiers ─────────────────────────────────────────────────

#[test]
fn single_identifier_foo() {
    let tokens = lex_sql("foo").unwrap();
    assert_eq!(tokens.len(), 2);
    assert_eq!(tokens[0].kind, TokenKind::Identifier("foo".to_owned()));
    assert_eq!(tokens[0].span, Span::new(0, 3));
}

#[test]
fn identifier_with_underscore_prefix() {
    let tokens = lex_sql("_foo").unwrap();
    assert_eq!(tokens.len(), 2);
    assert_eq!(tokens[0].kind, TokenKind::Identifier("_foo".to_owned()));
}

#[test]
fn identifier_with_digits() {
    let tokens = lex_sql("foo123").unwrap();
    assert_eq!(tokens.len(), 2);
    assert_eq!(tokens[0].kind, TokenKind::Identifier("foo123".to_owned()));
}

#[test]
fn identifier_starting_with_underscore_and_digits() {
    let tokens = lex_sql("_123abc").unwrap();
    assert_eq!(tokens.len(), 2);
    assert_eq!(tokens[0].kind, TokenKind::Identifier("_123abc".to_owned()));
}

// ── String literals ─────────────────────────────────────────────

#[test]
fn string_literal_hello() {
    let tokens = lex_sql("'hello'").unwrap();
    assert_eq!(tokens.len(), 2);
    assert_eq!(tokens[0].kind, TokenKind::String("hello".to_owned()));
    assert_eq!(tokens[0].span, Span::new(0, 7));
}

#[test]
fn string_with_escaped_quote() {
    let tokens = lex_sql("'it''s'").unwrap();
    assert_eq!(tokens.len(), 2);
    assert_eq!(tokens[0].kind, TokenKind::String("it's".to_owned()));
}

#[test]
fn empty_string_literal() {
    let tokens = lex_sql("''").unwrap();
    assert_eq!(tokens.len(), 2);
    assert_eq!(tokens[0].kind, TokenKind::String(String::new()));
}

#[test]
fn unicode_escape_string_literal_decodes_default_uescape() {
    let tokens = lex_sql(r#"U&'\0061\0308\24D1c'"#).unwrap();
    assert_eq!(tokens.len(), 2);
    assert_eq!(
        tokens[0].kind,
        TokenKind::String("a\u{0308}\u{24D1}c".to_owned())
    );
}

#[test]
fn unicode_escape_string_literal_decodes_custom_uescape() {
    let tokens = lex_sql(r#"U&'!00E4bc' UESCAPE '!'"#).unwrap();
    assert_eq!(tokens.len(), 2);
    assert_eq!(tokens[0].kind, TokenKind::String("äbc".to_owned()));
}

#[test]
fn unterminated_string_literal_returns_error() {
    let result = lex_sql("'hello");
    assert!(result.is_err());
}

// ── Single-char tokens ──────────────────────────────────────────

#[test]
fn all_single_char_tokens() {
    let tokens = lex_sql(", . = < > ( ) ;").unwrap();
    let kinds: Vec<_> = tokens.iter().map(|t| &t.kind).collect();
    assert_eq!(
        kinds,
        vec![
            &TokenKind::Comma,
            &TokenKind::Dot,
            &TokenKind::Eq,
            &TokenKind::Lt,
            &TokenKind::Gt,
            &TokenKind::LParen,
            &TokenKind::RParen,
            &TokenKind::Semicolon,
            &TokenKind::Eof,
        ]
    );
}

// ── Double-char tokens ──────────────────────────────────────────

#[test]
fn all_double_char_tokens() {
    let tokens = lex_sql("!= <= <> >=").unwrap();
    let kinds: Vec<_> = tokens.iter().map(|t| &t.kind).collect();
    assert_eq!(
        kinds,
        vec![
            &TokenKind::Ne,
            &TokenKind::Le,
            &TokenKind::Ne, // <> is also Ne
            &TokenKind::Ge,
            &TokenKind::Eof,
        ]
    );
}

#[test]
fn lt_followed_by_non_eq_non_gt_is_just_lt() {
    let tokens = lex_sql("<5").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::Lt);
    assert_eq!(tokens[0].span, Span::new(0, 1));
    assert_eq!(tokens[1].kind, TokenKind::Integer(5));
}

#[test]
fn gt_followed_by_non_eq_is_just_gt() {
    let tokens = lex_sql(">5").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::Gt);
    assert_eq!(tokens[0].span, Span::new(0, 1));
    assert_eq!(tokens[1].kind, TokenKind::Integer(5));
}

#[test]
fn geometric_amp_lt_pipe_has_full_span() {
    let tokens = lex_sql("&<|").unwrap();
    assert_eq!(tokens.len(), 2);
    assert_eq!(tokens[0].kind, TokenKind::CustomOp);
    assert_eq!(tokens[0].span, Span::new(0, 3));
}

// ── Comments ────────────────────────────────────────────────────

#[test]
fn line_comment_followed_by_token() {
    let tokens = lex_sql("-- this is a comment\nSELECT").unwrap();
    assert_eq!(tokens.len(), 2);
    assert_eq!(tokens[0].kind, TokenKind::Keyword(Keyword::Select));
}

#[test]
fn line_comment_at_end_of_input() {
    let tokens = lex_sql("SELECT -- comment").unwrap();
    assert_eq!(tokens.len(), 2);
    assert_eq!(tokens[0].kind, TokenKind::Keyword(Keyword::Select));
    assert_eq!(tokens[1].kind, TokenKind::Eof);
}

#[test]
fn multiple_consecutive_comments() {
    let tokens = lex_sql("-- comment1\n-- comment2\n-- comment3\nSELECT").unwrap();
    assert_eq!(tokens.len(), 2);
    assert_eq!(tokens[0].kind, TokenKind::Keyword(Keyword::Select));
}

// ── Keywords ────────────────────────────────────────────────────

#[test]
fn keyword_case_insensitive_lowercase() {
    let tokens = lex_sql("select").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::Keyword(Keyword::Select));
}

#[test]
fn keyword_case_insensitive_uppercase() {
    let tokens = lex_sql("SELECT").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::Keyword(Keyword::Select));
}

#[test]
fn keyword_case_insensitive_mixed() {
    let tokens = lex_sql("SeLeCt").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::Keyword(Keyword::Select));
}

#[test]
fn all_keywords_recognized() {
    let kw_pairs: Vec<(&str, Keyword)> = vec![
        ("AND", Keyword::And),
        ("AS", Keyword::As),
        ("BIGINT", Keyword::BigInt),
        ("BEGIN", Keyword::Begin),
        ("BLOB", Keyword::Blob),
        ("DELETE", Keyword::Delete),
        ("COMMITTED", Keyword::Committed),
        ("COMMIT", Keyword::Commit),
        ("CREATE", Keyword::Create),
        ("DATE", Keyword::Date),
        ("BOOLEAN", Keyword::Boolean),
        ("DOUBLE", Keyword::Double),
        ("DROP", Keyword::Drop),
        ("FALSE", Keyword::False),
        ("FROM", Keyword::From),
        ("INDEX", Keyword::Index),
        ("INT", Keyword::Int),
        ("INSERT", Keyword::Insert),
        ("ISOLATION", Keyword::Isolation),
        ("INTO", Keyword::Into),
        ("LEVEL", Keyword::Level),
        ("NUMERIC", Keyword::Numeric),
        ("NOT", Keyword::Not),
        ("NULL", Keyword::Null),
        ("ON", Keyword::On),
        ("READ", Keyword::Read),
        ("REAL", Keyword::Real),
        ("ROLLBACK", Keyword::Rollback),
        ("SEQUENCE", Keyword::Sequence),
        ("SELECT", Keyword::Select),
        ("SET", Keyword::Set),
        ("SNAPSHOT", Keyword::Snapshot),
        ("START", Keyword::Start),
        ("OR", Keyword::Or),
        ("TABLE", Keyword::Table),
        ("TEXT", Keyword::Text),
        ("TIME", Keyword::Time),
        ("TIMESTAMP", Keyword::Timestamp),
        ("TRANSACTION", Keyword::Transaction),
        ("TRUE", Keyword::True),
        ("UPDATE", Keyword::Update),
        ("VALUES", Keyword::Values),
        ("WHERE", Keyword::Where),
        ("INTERVAL", Keyword::Interval),
    ];
    for (text, expected) in kw_pairs {
        let tokens = lex_sql(text).unwrap();
        assert_eq!(
            tokens[0].kind,
            TokenKind::Keyword(expected),
            "failed for keyword text: {text}"
        );
    }
}

#[test]
fn non_keyword_identifier_foobar() {
    let tokens = lex_sql("foobar").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::Identifier("foobar".to_owned()));
}

#[test]
fn identifier_that_is_prefix_of_keyword() {
    // "SELECTING" starts with "SELECT" but is not a keyword
    let tokens = lex_sql("SELECTING").unwrap();
    assert_eq!(
        tokens[0].kind,
        TokenKind::Identifier("SELECTING".to_owned())
    );
}

// ── Unexpected characters ───────────────────────────────────────

#[test]
fn at_gt_token() {
    let tokens = lex_sql("@>").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::AtGt);
}

#[test]
fn hash_arrow_token() {
    let tokens = lex_sql("#>").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::HashArrow);
}

#[test]
fn hash_double_arrow_token() {
    let tokens = lex_sql("#>>").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::HashDoubleArrow);
}

#[test]
fn lt_at_token() {
    let tokens = lex_sql("<@").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::LtAt);
}

#[test]
fn question_token() {
    let tokens = lex_sql("?").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::Question);
}

#[test]
fn question_pipe_token() {
    let tokens = lex_sql("?|").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::QuestionPipe);
}

#[test]
fn question_ampersand_token() {
    let tokens = lex_sql("?&").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::QuestionAmpersand);
}

#[test]
fn unicode_identifier_start_fails() {
    // 'c' is ASCII, but 'é' is not - the lexer should fail at 'é'
    let result = lex_sql("café");
    // The lexer will tokenize "caf" as an identifier, then fail on 'é'
    assert!(result.is_err());
}

// ── Mixed tokens ────────────────────────────────────────────────

#[test]
fn mixed_tokens_full_query() {
    let tokens = lex_sql("SELECT 1, 'hello' FROM users WHERE id = 42").unwrap();
    let kinds: Vec<_> = tokens.iter().map(|t| &t.kind).collect();
    assert_eq!(kinds[0], &TokenKind::Keyword(Keyword::Select));
    assert_eq!(kinds[1], &TokenKind::Integer(1));
    assert_eq!(kinds[2], &TokenKind::Comma);
    assert_eq!(kinds[3], &TokenKind::String("hello".to_owned()));
    assert_eq!(kinds[4], &TokenKind::Keyword(Keyword::From));
    assert_eq!(kinds[5], &TokenKind::Identifier("users".to_owned()));
    assert_eq!(kinds[6], &TokenKind::Keyword(Keyword::Where));
    assert_eq!(kinds[7], &TokenKind::Identifier("id".to_owned()));
    assert_eq!(kinds[8], &TokenKind::Eq);
    assert_eq!(kinds[9], &TokenKind::Integer(42));
    assert_eq!(kinds[10], &TokenKind::Eof);
    assert_eq!(tokens.len(), 11);
}

// ── Whitespace types ────────────────────────────────────────────

#[test]
fn multiple_whitespace_types_tabs_newlines_cr() {
    let tokens = lex_sql("SELECT\t1\n,\r\n'a'").unwrap();
    let kinds: Vec<_> = tokens.iter().map(|t| &t.kind).collect();
    assert_eq!(kinds[0], &TokenKind::Keyword(Keyword::Select));
    assert_eq!(kinds[1], &TokenKind::Integer(1));
    assert_eq!(kinds[2], &TokenKind::Comma);
    assert_eq!(kinds[3], &TokenKind::String("a".to_owned()));
    assert_eq!(kinds[4], &TokenKind::Eof);
}

// ── Span tracking ───────────────────────────────────────────────

#[test]
fn token_spans_are_correct() {
    let tokens = lex_sql("SELECT 42").unwrap();
    // "SELECT" occupies bytes 0..6
    assert_eq!(tokens[0].span, Span::new(0, 6));
    // "42" occupies bytes 7..9
    assert_eq!(tokens[1].span, Span::new(7, 9));
    // EOF at position 9
    assert_eq!(tokens[2].span, Span::new(9, 9));
}

#[test]
fn span_for_double_char_token() {
    let tokens = lex_sql("!=").unwrap();
    assert_eq!(tokens[0].span, Span::new(0, 2));
}

#[test]
fn span_for_string_literal_includes_quotes() {
    let tokens = lex_sql("'abc'").unwrap();
    // The span covers the full 'abc' including quotes: 0..5
    assert_eq!(tokens[0].span, Span::new(0, 5));
}

#[test]
fn at_sign_is_valid_token() {
    let tokens = lex_sql("SELECT @").unwrap();
    assert_eq!(tokens[1].kind, TokenKind::At);
}

#[test]
fn error_position_for_unterminated_string() {
    let err = lex_sql("'unterminated").unwrap_err();
    let msg = format!("{err:?}");
    assert!(msg.contains("unterminated string literal"), "got: {msg}");
}
