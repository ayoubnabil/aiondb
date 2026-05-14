use super::*;

// ===================================================================
// Token::eof() constructor
// ===================================================================

#[test]
fn token_eof_at_position_zero() {
    let tok = Token::eof(0);
    assert_eq!(tok.kind, TokenKind::Eof);
    assert_eq!(tok.span, Span::new(0, 0));
}

#[test]
fn token_eof_at_position_42() {
    let tok = Token::eof(42);
    assert_eq!(tok.kind, TokenKind::Eof);
    assert_eq!(tok.span.start, 42);
    assert_eq!(tok.span.end, 42);
}

#[test]
fn token_eof_at_large_position() {
    let tok = Token::eof(999_999);
    assert_eq!(tok.span, Span::new(999_999, 999_999));
}

#[test]
fn token_eof_at_usize_max() {
    let tok = Token::eof(usize::MAX);
    assert_eq!(tok.span.start, usize::MAX);
    assert_eq!(tok.span.end, usize::MAX);
}

#[test]
fn token_eof_span_is_zero_width() {
    let tok = Token::eof(10);
    assert_eq!(tok.span.start, tok.span.end);
}

// ===================================================================
// Token struct field access
// ===================================================================

#[test]
fn token_kind_field_keyword() {
    let tok = Token {
        kind: TokenKind::Keyword(Keyword::Select),
        span: Span::new(0, 6),
    };
    assert_eq!(tok.kind, TokenKind::Keyword(Keyword::Select));
}

#[test]
fn token_kind_field_identifier() {
    let tok = Token {
        kind: TokenKind::Identifier("foo".to_string()),
        span: Span::new(0, 3),
    };
    if let TokenKind::Identifier(ref v) = tok.kind {
        assert_eq!(v, "foo");
    } else {
        panic!("expected Identifier");
    }
}

#[test]
fn token_kind_field_integer() {
    let tok = Token {
        kind: TokenKind::Integer(123),
        span: Span::new(0, 3),
    };
    assert_eq!(tok.kind, TokenKind::Integer(123));
}

#[test]
fn token_kind_field_string() {
    let tok = Token {
        kind: TokenKind::String("hello".to_string()),
        span: Span::new(0, 7),
    };
    if let TokenKind::String(ref v) = tok.kind {
        assert_eq!(v, "hello");
    } else {
        panic!("expected String");
    }
}

#[test]
fn token_span_field() {
    let tok = Token {
        kind: TokenKind::Comma,
        span: Span::new(5, 6),
    };
    assert_eq!(tok.span.start, 5);
    assert_eq!(tok.span.end, 6);
}

#[test]
fn token_punctuation_kinds() {
    let kinds = [
        TokenKind::Comma,
        TokenKind::Dot,
        TokenKind::Eq,
        TokenKind::Ge,
        TokenKind::Gt,
        TokenKind::Le,
        TokenKind::LParen,
        TokenKind::Lt,
        TokenKind::Ne,
        TokenKind::RParen,
        TokenKind::Semicolon,
        TokenKind::Eof,
    ];
    for kind in &kinds {
        let tok = Token {
            kind: kind.clone(),
            span: Span::new(0, 1),
        };
        assert_eq!(&tok.kind, kind);
    }
}

// ===================================================================
// Token and TokenKind: Clone
// ===================================================================

#[test]
fn token_kind_clone_keyword() {
    let tk = TokenKind::Keyword(Keyword::Select);
    assert_eq!(tk, tk.clone());
}

#[test]
fn token_kind_clone_identifier() {
    let tk = TokenKind::Identifier("abc".to_string());
    assert_eq!(tk, tk.clone());
}

#[test]
fn token_kind_clone_integer() {
    let tk = TokenKind::Integer(99);
    assert_eq!(tk, tk.clone());
}

#[test]
fn token_kind_clone_string() {
    let tk = TokenKind::String("data".to_string());
    assert_eq!(tk, tk.clone());
}

#[test]
fn token_kind_clone_all_punctuation() {
    let variants = vec![
        TokenKind::Comma,
        TokenKind::Dot,
        TokenKind::Eq,
        TokenKind::Ge,
        TokenKind::Gt,
        TokenKind::Le,
        TokenKind::LParen,
        TokenKind::Lt,
        TokenKind::Ne,
        TokenKind::RParen,
        TokenKind::Semicolon,
        TokenKind::Eof,
    ];
    for tk in &variants {
        assert_eq!(tk, &tk.clone());
    }
}

#[test]
fn token_clone_full_struct() {
    let tok = Token {
        kind: TokenKind::Identifier("test".to_string()),
        span: Span::new(10, 14),
    };
    let cloned = tok.clone();
    assert_eq!(tok, cloned);
}

#[test]
fn token_clone_eof() {
    let tok = Token::eof(5);
    let cloned = tok.clone();
    assert_eq!(tok, cloned);
}

// ===================================================================
// Token and TokenKind: PartialEq
// ===================================================================

#[test]
fn token_kind_eq_same_keyword() {
    assert_eq!(
        TokenKind::Keyword(Keyword::Select),
        TokenKind::Keyword(Keyword::Select)
    );
}

#[test]
fn token_kind_ne_different_keyword() {
    assert_ne!(
        TokenKind::Keyword(Keyword::Select),
        TokenKind::Keyword(Keyword::Insert)
    );
}

#[test]
fn token_kind_ne_keyword_vs_identifier() {
    assert_ne!(
        TokenKind::Keyword(Keyword::Select),
        TokenKind::Identifier("SELECT".to_string())
    );
}

#[test]
fn token_kind_eq_same_identifier() {
    assert_eq!(
        TokenKind::Identifier("foo".into()),
        TokenKind::Identifier("foo".into())
    );
}

#[test]
fn token_kind_ne_different_identifier() {
    assert_ne!(
        TokenKind::Identifier("foo".into()),
        TokenKind::Identifier("bar".into())
    );
}

#[test]
fn token_kind_eq_same_integer() {
    assert_eq!(TokenKind::Integer(0), TokenKind::Integer(0));
}

#[test]
fn token_kind_ne_different_integer() {
    assert_ne!(TokenKind::Integer(1), TokenKind::Integer(2));
}

#[test]
fn token_kind_ne_integer_vs_identifier() {
    assert_ne!(
        TokenKind::Integer(1),
        TokenKind::Identifier("1".to_string())
    );
}

#[test]
fn token_kind_eq_same_string() {
    assert_eq!(TokenKind::String("a".into()), TokenKind::String("a".into()));
}

#[test]
fn token_kind_ne_different_string() {
    assert_ne!(TokenKind::String("a".into()), TokenKind::String("b".into()));
}

#[test]
fn token_kind_ne_string_vs_identifier() {
    assert_ne!(
        TokenKind::String("x".to_string()),
        TokenKind::Identifier("x".to_string())
    );
}

#[test]
fn token_kind_punctuation_same() {
    assert_eq!(TokenKind::Comma, TokenKind::Comma);
    assert_eq!(TokenKind::Dot, TokenKind::Dot);
    assert_eq!(TokenKind::Eq, TokenKind::Eq);
    assert_eq!(TokenKind::Ge, TokenKind::Ge);
    assert_eq!(TokenKind::Gt, TokenKind::Gt);
    assert_eq!(TokenKind::Le, TokenKind::Le);
    assert_eq!(TokenKind::LParen, TokenKind::LParen);
    assert_eq!(TokenKind::Lt, TokenKind::Lt);
    assert_eq!(TokenKind::Ne, TokenKind::Ne);
    assert_eq!(TokenKind::RParen, TokenKind::RParen);
    assert_eq!(TokenKind::Semicolon, TokenKind::Semicolon);
    assert_eq!(TokenKind::Eof, TokenKind::Eof);
}

#[test]
fn token_kind_punctuation_all_distinct() {
    let variants: Vec<TokenKind> = vec![
        TokenKind::Comma,
        TokenKind::Dot,
        TokenKind::Eq,
        TokenKind::Ge,
        TokenKind::Gt,
        TokenKind::Le,
        TokenKind::LParen,
        TokenKind::Lt,
        TokenKind::Ne,
        TokenKind::RParen,
        TokenKind::Semicolon,
        TokenKind::Eof,
    ];
    for (i, a) in variants.iter().enumerate() {
        for (j, b) in variants.iter().enumerate() {
            if i == j {
                assert_eq!(a, b);
            } else {
                assert_ne!(a, b, "{a:?} should differ from {b:?}");
            }
        }
    }
}

#[test]
fn token_eq_same() {
    let a = Token {
        kind: TokenKind::Integer(5),
        span: Span::new(0, 1),
    };
    let b = Token {
        kind: TokenKind::Integer(5),
        span: Span::new(0, 1),
    };
    assert_eq!(a, b);
}

#[test]
fn token_ne_different_kind() {
    let a = Token {
        kind: TokenKind::Integer(5),
        span: Span::new(0, 1),
    };
    let b = Token {
        kind: TokenKind::Integer(6),
        span: Span::new(0, 1),
    };
    assert_ne!(a, b);
}

#[test]
fn token_ne_different_span() {
    let a = Token {
        kind: TokenKind::Comma,
        span: Span::new(0, 1),
    };
    let b = Token {
        kind: TokenKind::Comma,
        span: Span::new(5, 6),
    };
    assert_ne!(a, b);
}

// ===================================================================
// Token and TokenKind: Debug
// ===================================================================

#[test]
fn token_kind_debug_keyword() {
    let dbg = format!("{:?}", TokenKind::Keyword(Keyword::Select));
    assert!(dbg.contains("Keyword"));
    assert!(dbg.contains("Select"));
}

#[test]
fn token_kind_debug_identifier() {
    let dbg = format!("{:?}", TokenKind::Identifier("foo".into()));
    assert!(dbg.contains("Identifier"));
    assert!(dbg.contains("foo"));
}

#[test]
fn token_kind_debug_integer() {
    let dbg = format!("{:?}", TokenKind::Integer(42));
    assert!(dbg.contains("Integer"));
    assert!(dbg.contains("42"));
}

#[test]
fn token_kind_debug_string() {
    let dbg = format!("{:?}", TokenKind::String("hi".into()));
    assert!(dbg.contains("String"));
    assert!(dbg.contains("hi"));
}

#[test]
fn token_kind_debug_comma() {
    let dbg = format!("{:?}", TokenKind::Comma);
    assert!(dbg.contains("Comma"));
}

#[test]
fn token_kind_debug_dot() {
    let dbg = format!("{:?}", TokenKind::Dot);
    assert!(dbg.contains("Dot"));
}

#[test]
fn token_kind_debug_eq() {
    let dbg = format!("{:?}", TokenKind::Eq);
    assert!(dbg.contains("Eq"));
}

#[test]
fn token_kind_debug_ge() {
    let dbg = format!("{:?}", TokenKind::Ge);
    assert!(dbg.contains("Ge"));
}

#[test]
fn token_kind_debug_gt() {
    let dbg = format!("{:?}", TokenKind::Gt);
    assert!(dbg.contains("Gt"));
}

#[test]
fn token_kind_debug_le() {
    let dbg = format!("{:?}", TokenKind::Le);
    assert!(dbg.contains("Le"));
}

#[test]
fn token_kind_debug_lparen() {
    let dbg = format!("{:?}", TokenKind::LParen);
    assert!(dbg.contains("LParen"));
}

#[test]
fn token_kind_debug_lt() {
    let dbg = format!("{:?}", TokenKind::Lt);
    assert!(dbg.contains("Lt"));
}

#[test]
fn token_kind_debug_ne() {
    let dbg = format!("{:?}", TokenKind::Ne);
    assert!(dbg.contains("Ne"));
}

#[test]
fn token_kind_debug_rparen() {
    let dbg = format!("{:?}", TokenKind::RParen);
    assert!(dbg.contains("RParen"));
}

#[test]
fn token_kind_debug_semicolon() {
    let dbg = format!("{:?}", TokenKind::Semicolon);
    assert!(dbg.contains("Semicolon"));
}

#[test]
fn token_kind_debug_eof() {
    let dbg = format!("{:?}", TokenKind::Eof);
    assert!(dbg.contains("Eof"));
}

#[test]
fn token_debug_includes_kind_and_span() {
    let tok = Token {
        kind: TokenKind::Integer(7),
        span: Span::new(3, 4),
    };
    let dbg = format!("{tok:?}");
    assert!(dbg.contains("Token"));
    assert!(dbg.contains("Integer"));
    assert!(dbg.contains('7'));
}

#[test]
fn token_debug_eof_constructor() {
    let tok = Token::eof(0);
    let dbg = format!("{tok:?}");
    assert!(dbg.contains("Eof"));
}
