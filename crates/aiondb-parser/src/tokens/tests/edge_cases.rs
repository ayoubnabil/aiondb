use super::*;

// ===================================================================
// Edge cases
// ===================================================================

#[test]
fn describe_returns_owned_string_each_call() {
    let tk = TokenKind::Comma;
    let d1 = tk.describe();
    let d2 = tk.describe();
    assert_eq!(d1, d2);
    // They are separate heap allocations (owned Strings)
    assert_eq!(d1.as_str(), d2.as_str());
}

#[test]
fn describe_identifier_preserves_case() {
    let tk = TokenKind::Identifier("MyTable".to_string());
    assert_eq!(tk.describe(), "identifier MyTable");
}

#[test]
fn describe_integer_includes_sign_for_negative() {
    let tk = TokenKind::Integer(-999);
    let desc = tk.describe();
    assert!(desc.contains("-999"));
}

#[test]
fn token_eof_kind_is_eof() {
    let tok = Token::eof(100);
    assert!(matches!(tok.kind, TokenKind::Eof));
}

#[test]
fn token_with_zero_width_span() {
    let tok = Token {
        kind: TokenKind::Semicolon,
        span: Span::new(10, 10),
    };
    assert_eq!(tok.span.start, tok.span.end);
}

#[test]
fn token_kind_string_with_unicode() {
    let tk = TokenKind::String("\u{1F600}\u{4E16}\u{754C}".to_string());
    assert_eq!(tk.describe(), "string literal");
}

#[test]
fn token_kind_identifier_with_digits() {
    let tk = TokenKind::Identifier("col123".to_string());
    assert_eq!(tk.describe(), "identifier col123");
}
