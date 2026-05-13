use super::*;

// ===================================================================
// TokenKind::describe() -- exhaustive for every variant
// ===================================================================

#[test]
fn describe_keyword_select() {
    let tk = TokenKind::Keyword(Keyword::Select);
    assert_eq!(tk.describe(), "keyword SELECT");
}

#[test]
fn describe_keyword_insert() {
    let tk = TokenKind::Keyword(Keyword::Insert);
    assert_eq!(tk.describe(), "keyword INSERT");
}

#[test]
fn describe_keyword_update() {
    let tk = TokenKind::Keyword(Keyword::Update);
    assert_eq!(tk.describe(), "keyword UPDATE");
}

#[test]
fn describe_keyword_delete() {
    let tk = TokenKind::Keyword(Keyword::Delete);
    assert_eq!(tk.describe(), "keyword DELETE");
}

#[test]
fn describe_keyword_create() {
    let tk = TokenKind::Keyword(Keyword::Create);
    assert_eq!(tk.describe(), "keyword CREATE");
}

#[test]
fn describe_keyword_begin() {
    let tk = TokenKind::Keyword(Keyword::Begin);
    assert_eq!(tk.describe(), "keyword BEGIN");
}

#[test]
fn describe_keyword_commit() {
    let tk = TokenKind::Keyword(Keyword::Commit);
    assert_eq!(tk.describe(), "keyword COMMIT");
}

#[test]
fn describe_keyword_rollback() {
    let tk = TokenKind::Keyword(Keyword::Rollback);
    assert_eq!(tk.describe(), "keyword ROLLBACK");
}

#[test]
fn describe_keyword_and() {
    assert_eq!(TokenKind::Keyword(Keyword::And).describe(), "keyword AND");
}

#[test]
fn describe_keyword_or() {
    assert_eq!(TokenKind::Keyword(Keyword::Or).describe(), "keyword OR");
}

#[test]
fn describe_keyword_not() {
    assert_eq!(TokenKind::Keyword(Keyword::Not).describe(), "keyword NOT");
}

#[test]
fn describe_keyword_from() {
    assert_eq!(TokenKind::Keyword(Keyword::From).describe(), "keyword FROM");
}

#[test]
fn describe_keyword_where() {
    assert_eq!(
        TokenKind::Keyword(Keyword::Where).describe(),
        "keyword WHERE"
    );
}

#[test]
fn describe_keyword_as() {
    assert_eq!(TokenKind::Keyword(Keyword::As).describe(), "keyword AS");
}

#[test]
fn describe_keyword_asc() {
    assert_eq!(TokenKind::Keyword(Keyword::Asc).describe(), "keyword ASC");
}

#[test]
fn describe_keyword_desc() {
    assert_eq!(TokenKind::Keyword(Keyword::Desc).describe(), "keyword DESC");
}

#[test]
fn describe_keyword_order() {
    assert_eq!(
        TokenKind::Keyword(Keyword::Order).describe(),
        "keyword ORDER"
    );
}

#[test]
fn describe_keyword_by() {
    assert_eq!(TokenKind::Keyword(Keyword::By).describe(), "keyword BY");
}

#[test]
fn describe_parameter() {
    assert_eq!(TokenKind::Parameter(3).describe(), "parameter $3");
}

#[test]
fn describe_keyword_into() {
    assert_eq!(TokenKind::Keyword(Keyword::Into).describe(), "keyword INTO");
}

#[test]
fn describe_keyword_values() {
    assert_eq!(
        TokenKind::Keyword(Keyword::Values).describe(),
        "keyword VALUES"
    );
}

#[test]
fn describe_keyword_set() {
    assert_eq!(TokenKind::Keyword(Keyword::Set).describe(), "keyword SET");
}

#[test]
fn describe_keyword_table() {
    assert_eq!(
        TokenKind::Keyword(Keyword::Table).describe(),
        "keyword TABLE"
    );
}

#[test]
fn describe_keyword_index() {
    assert_eq!(
        TokenKind::Keyword(Keyword::Index).describe(),
        "keyword INDEX"
    );
}

#[test]
fn describe_keyword_on() {
    assert_eq!(TokenKind::Keyword(Keyword::On).describe(), "keyword ON");
}

#[test]
fn describe_keyword_true() {
    assert_eq!(TokenKind::Keyword(Keyword::True).describe(), "keyword TRUE");
}

#[test]
fn describe_keyword_false() {
    assert_eq!(
        TokenKind::Keyword(Keyword::False).describe(),
        "keyword FALSE"
    );
}

#[test]
fn describe_keyword_null() {
    assert_eq!(TokenKind::Keyword(Keyword::Null).describe(), "keyword NULL");
}

#[test]
fn describe_keyword_int() {
    assert_eq!(TokenKind::Keyword(Keyword::Int).describe(), "keyword INT");
}

#[test]
fn describe_keyword_bigint() {
    assert_eq!(
        TokenKind::Keyword(Keyword::BigInt).describe(),
        "keyword BIGINT"
    );
}

#[test]
fn describe_keyword_real() {
    assert_eq!(TokenKind::Keyword(Keyword::Real).describe(), "keyword REAL");
}

#[test]
fn describe_keyword_double() {
    assert_eq!(
        TokenKind::Keyword(Keyword::Double).describe(),
        "keyword DOUBLE"
    );
}

#[test]
fn describe_keyword_numeric() {
    assert_eq!(
        TokenKind::Keyword(Keyword::Numeric).describe(),
        "keyword NUMERIC"
    );
}

#[test]
fn describe_keyword_text() {
    assert_eq!(TokenKind::Keyword(Keyword::Text).describe(), "keyword TEXT");
}

#[test]
fn describe_keyword_boolean() {
    assert_eq!(
        TokenKind::Keyword(Keyword::Boolean).describe(),
        "keyword BOOLEAN"
    );
}

#[test]
fn describe_keyword_blob() {
    assert_eq!(TokenKind::Keyword(Keyword::Blob).describe(), "keyword BLOB");
}

#[test]
fn describe_keyword_timestamp() {
    assert_eq!(
        TokenKind::Keyword(Keyword::Timestamp).describe(),
        "keyword TIMESTAMP"
    );
}

#[test]
fn describe_keyword_date() {
    assert_eq!(TokenKind::Keyword(Keyword::Date).describe(), "keyword DATE");
}

#[test]
fn describe_keyword_interval() {
    assert_eq!(
        TokenKind::Keyword(Keyword::Interval).describe(),
        "keyword INTERVAL"
    );
}

#[test]
fn describe_keyword_isolation() {
    assert_eq!(
        TokenKind::Keyword(Keyword::Isolation).describe(),
        "keyword ISOLATION"
    );
}

#[test]
fn describe_keyword_level() {
    assert_eq!(
        TokenKind::Keyword(Keyword::Level).describe(),
        "keyword LEVEL"
    );
}

#[test]
fn describe_keyword_read() {
    assert_eq!(TokenKind::Keyword(Keyword::Read).describe(), "keyword READ");
}

#[test]
fn describe_keyword_committed() {
    assert_eq!(
        TokenKind::Keyword(Keyword::Committed).describe(),
        "keyword COMMITTED"
    );
}

#[test]
fn describe_keyword_snapshot() {
    assert_eq!(
        TokenKind::Keyword(Keyword::Snapshot).describe(),
        "keyword SNAPSHOT"
    );
}

#[test]
fn describe_keyword_start() {
    assert_eq!(
        TokenKind::Keyword(Keyword::Start).describe(),
        "keyword START"
    );
}

#[test]
fn describe_keyword_transaction() {
    assert_eq!(
        TokenKind::Keyword(Keyword::Transaction).describe(),
        "keyword TRANSACTION"
    );
}

#[test]
fn describe_identifier_simple() {
    let tk = TokenKind::Identifier("users".to_string());
    assert_eq!(tk.describe(), "identifier users");
}

#[test]
fn describe_identifier_single_char() {
    let tk = TokenKind::Identifier("x".to_string());
    assert_eq!(tk.describe(), "identifier x");
}

#[test]
fn describe_identifier_with_underscore() {
    let tk = TokenKind::Identifier("my_table".to_string());
    assert_eq!(tk.describe(), "identifier my_table");
}

#[test]
fn describe_identifier_empty_string() {
    let tk = TokenKind::Identifier(String::new());
    assert_eq!(tk.describe(), "identifier ");
}

#[test]
fn describe_integer_zero() {
    let tk = TokenKind::Integer(0);
    assert_eq!(tk.describe(), "integer 0");
}

#[test]
fn describe_integer_positive() {
    let tk = TokenKind::Integer(42);
    assert_eq!(tk.describe(), "integer 42");
}

#[test]
fn describe_integer_negative() {
    let tk = TokenKind::Integer(-1);
    assert_eq!(tk.describe(), "integer -1");
}

#[test]
fn describe_integer_max() {
    let tk = TokenKind::Integer(i64::MAX);
    assert_eq!(tk.describe(), format!("integer {}", i64::MAX));
}

#[test]
fn describe_integer_min() {
    let tk = TokenKind::Integer(i64::MIN);
    assert_eq!(tk.describe(), format!("integer {}", i64::MIN));
}

#[test]
fn describe_string_empty() {
    let tk = TokenKind::String(String::new());
    assert_eq!(tk.describe(), "string literal");
}

#[test]
fn describe_string_nonempty() {
    let tk = TokenKind::String("hello world".to_string());
    assert_eq!(tk.describe(), "string literal");
}

#[test]
fn describe_string_does_not_leak_content() {
    let tk = TokenKind::String("secret".to_string());
    assert!(!tk.describe().contains("secret"));
}

#[test]
fn describe_comma() {
    assert_eq!(TokenKind::Comma.describe(), "','");
}

#[test]
fn describe_dot() {
    assert_eq!(TokenKind::Dot.describe(), "'.'");
}

#[test]
fn describe_eq() {
    assert_eq!(TokenKind::Eq.describe(), "'='");
}

#[test]
fn describe_ge() {
    assert_eq!(TokenKind::Ge.describe(), "'>='");
}

#[test]
fn describe_gt() {
    assert_eq!(TokenKind::Gt.describe(), "'>'");
}

#[test]
fn describe_le() {
    assert_eq!(TokenKind::Le.describe(), "'<='");
}

#[test]
fn describe_lparen() {
    assert_eq!(TokenKind::LParen.describe(), "'('");
}

#[test]
fn describe_lt() {
    assert_eq!(TokenKind::Lt.describe(), "'<'");
}

#[test]
fn describe_ne() {
    assert_eq!(TokenKind::Ne.describe(), "'!='");
}

#[test]
fn describe_rparen() {
    assert_eq!(TokenKind::RParen.describe(), "')'");
}

#[test]
fn describe_semicolon() {
    assert_eq!(TokenKind::Semicolon.describe(), "';'");
}

#[test]
fn describe_eof() {
    assert_eq!(TokenKind::Eof.describe(), "end of input");
}

// ===================================================================
// TokenKind::describe() -- format verification for keyword pattern
// ===================================================================

#[test]
fn describe_keyword_always_starts_with_keyword_prefix() {
    let keywords = [
        Keyword::And,
        Keyword::Asc,
        Keyword::As,
        Keyword::BigInt,
        Keyword::Begin,
        Keyword::Blob,
        Keyword::By,
        Keyword::Case,
        Keyword::Cast,
        Keyword::Delete,
        Keyword::Committed,
        Keyword::Commit,
        Keyword::Create,
        Keyword::Date,
        Keyword::Double,
        Keyword::Desc,
        Keyword::Else,
        Keyword::End,
        Keyword::False,
        Keyword::From,
        Keyword::Index,
        Keyword::Int,
        Keyword::Insert,
        Keyword::Isolation,
        Keyword::Into,
        Keyword::Level,
        Keyword::Numeric,
        Keyword::Not,
        Keyword::Null,
        Keyword::On,
        Keyword::Order,
        Keyword::Read,
        Keyword::Real,
        Keyword::Rollback,
        Keyword::Select,
        Keyword::Set,
        Keyword::Snapshot,
        Keyword::Start,
        Keyword::Or,
        Keyword::Table,
        Keyword::Text,
        Keyword::Then,
        Keyword::Timestamp,
        Keyword::Transaction,
        Keyword::True,
        Keyword::Update,
        Keyword::Values,
        Keyword::When,
        Keyword::Where,
        Keyword::Boolean,
        Keyword::Interval,
    ];
    for kw in &keywords {
        let desc = TokenKind::Keyword(*kw).describe();
        assert!(
            desc.starts_with("keyword "),
            "describe() for {kw:?} = {desc:?} does not start with 'keyword '"
        );
    }
}
