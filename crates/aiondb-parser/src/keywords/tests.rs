use super::*;

// ── Exhaustive keyword mapping ──────────────────────────────────

#[test]
fn lookup_and() {
    assert_eq!(lookup_keyword("AND"), Some(Keyword::And));
}

#[test]
fn lookup_as() {
    assert_eq!(lookup_keyword("AS"), Some(Keyword::As));
}

#[test]
fn lookup_bigint() {
    assert_eq!(lookup_keyword("BIGINT"), Some(Keyword::BigInt));
}

#[test]
fn lookup_begin() {
    assert_eq!(lookup_keyword("BEGIN"), Some(Keyword::Begin));
}

#[test]
fn lookup_blob() {
    assert_eq!(lookup_keyword("BLOB"), Some(Keyword::Blob));
}

#[test]
fn lookup_delete() {
    assert_eq!(lookup_keyword("DELETE"), Some(Keyword::Delete));
}

#[test]
fn lookup_default() {
    assert_eq!(lookup_keyword("DEFAULT"), Some(Keyword::Default));
}

#[test]
fn lookup_committed() {
    assert_eq!(lookup_keyword("COMMITTED"), Some(Keyword::Committed));
}

#[test]
fn lookup_commit() {
    assert_eq!(lookup_keyword("COMMIT"), Some(Keyword::Commit));
}

#[test]
fn lookup_create() {
    assert_eq!(lookup_keyword("CREATE"), Some(Keyword::Create));
}

#[test]
fn lookup_date() {
    assert_eq!(lookup_keyword("DATE"), Some(Keyword::Date));
}

#[test]
fn lookup_boolean() {
    assert_eq!(lookup_keyword("BOOLEAN"), Some(Keyword::Boolean));
}

#[test]
fn lookup_double() {
    assert_eq!(lookup_keyword("DOUBLE"), Some(Keyword::Double));
}

#[test]
fn lookup_drop() {
    assert_eq!(lookup_keyword("DROP"), Some(Keyword::Drop));
}

#[test]
fn lookup_false() {
    assert_eq!(lookup_keyword("FALSE"), Some(Keyword::False));
}

#[test]
fn lookup_from() {
    assert_eq!(lookup_keyword("FROM"), Some(Keyword::From));
}

#[test]
fn lookup_index() {
    assert_eq!(lookup_keyword("INDEX"), Some(Keyword::Index));
}

#[test]
fn lookup_int() {
    assert_eq!(lookup_keyword("INT"), Some(Keyword::Int));
}

#[test]
fn lookup_interval() {
    assert_eq!(lookup_keyword("INTERVAL"), Some(Keyword::Interval));
}

#[test]
fn lookup_insert() {
    assert_eq!(lookup_keyword("INSERT"), Some(Keyword::Insert));
}

#[test]
fn lookup_isolation() {
    assert_eq!(lookup_keyword("ISOLATION"), Some(Keyword::Isolation));
}

#[test]
fn lookup_into() {
    assert_eq!(lookup_keyword("INTO"), Some(Keyword::Into));
}

#[test]
fn lookup_limit() {
    assert_eq!(lookup_keyword("LIMIT"), Some(Keyword::Limit));
}

#[test]
fn lookup_level() {
    assert_eq!(lookup_keyword("LEVEL"), Some(Keyword::Level));
}

#[test]
fn lookup_numeric() {
    assert_eq!(lookup_keyword("NUMERIC"), Some(Keyword::Numeric));
}

#[test]
fn lookup_not() {
    assert_eq!(lookup_keyword("NOT"), Some(Keyword::Not));
}

#[test]
fn lookup_null() {
    assert_eq!(lookup_keyword("NULL"), Some(Keyword::Null));
}

#[test]
fn lookup_on() {
    assert_eq!(lookup_keyword("ON"), Some(Keyword::On));
}

#[test]
fn lookup_read() {
    assert_eq!(lookup_keyword("READ"), Some(Keyword::Read));
}

#[test]
fn lookup_real() {
    assert_eq!(lookup_keyword("REAL"), Some(Keyword::Real));
}

#[test]
fn lookup_rollback() {
    assert_eq!(lookup_keyword("ROLLBACK"), Some(Keyword::Rollback));
}

#[test]
fn lookup_sequence() {
    assert_eq!(lookup_keyword("SEQUENCE"), Some(Keyword::Sequence));
}

#[test]
fn lookup_select() {
    assert_eq!(lookup_keyword("SELECT"), Some(Keyword::Select));
}

#[test]
fn lookup_set() {
    assert_eq!(lookup_keyword("SET"), Some(Keyword::Set));
}

#[test]
fn lookup_snapshot() {
    assert_eq!(lookup_keyword("SNAPSHOT"), Some(Keyword::Snapshot));
}

#[test]
fn lookup_start() {
    assert_eq!(lookup_keyword("START"), Some(Keyword::Start));
}

#[test]
fn lookup_or() {
    assert_eq!(lookup_keyword("OR"), Some(Keyword::Or));
}

#[test]
fn lookup_table() {
    assert_eq!(lookup_keyword("TABLE"), Some(Keyword::Table));
}

#[test]
fn lookup_text() {
    assert_eq!(lookup_keyword("TEXT"), Some(Keyword::Text));
}

#[test]
fn lookup_timestamp() {
    assert_eq!(lookup_keyword("TIMESTAMP"), Some(Keyword::Timestamp));
}

#[test]
fn lookup_transaction() {
    assert_eq!(lookup_keyword("TRANSACTION"), Some(Keyword::Transaction));
}

#[test]
fn lookup_true() {
    assert_eq!(lookup_keyword("TRUE"), Some(Keyword::True));
}

#[test]
fn lookup_update() {
    assert_eq!(lookup_keyword("UPDATE"), Some(Keyword::Update));
}

#[test]
fn lookup_values() {
    assert_eq!(lookup_keyword("VALUES"), Some(Keyword::Values));
}

#[test]
fn lookup_where() {
    assert_eq!(lookup_keyword("WHERE"), Some(Keyword::Where));
}

#[test]
fn lookup_check() {
    assert_eq!(lookup_keyword("CHECK"), Some(Keyword::Check));
}

#[test]
fn lookup_constraint() {
    assert_eq!(lookup_keyword("CONSTRAINT"), Some(Keyword::Constraint));
}

#[test]
fn lookup_foreign() {
    assert_eq!(lookup_keyword("FOREIGN"), Some(Keyword::Foreign));
}

#[test]
fn lookup_key() {
    assert_eq!(lookup_keyword("KEY"), Some(Keyword::Key));
}

#[test]
fn lookup_primary() {
    assert_eq!(lookup_keyword("PRIMARY"), Some(Keyword::Primary));
}

#[test]
fn lookup_references() {
    assert_eq!(lookup_keyword("REFERENCES"), Some(Keyword::References));
}

#[test]
fn lookup_release() {
    assert_eq!(lookup_keyword("RELEASE"), Some(Keyword::Release));
}

#[test]
fn lookup_savepoint() {
    assert_eq!(lookup_keyword("SAVEPOINT"), Some(Keyword::Savepoint));
}

#[test]
fn lookup_to() {
    assert_eq!(lookup_keyword("TO"), Some(Keyword::To));
}

#[test]
fn lookup_unique() {
    assert_eq!(lookup_keyword("UNIQUE"), Some(Keyword::Unique));
}

#[test]
fn lookup_uuid() {
    assert_eq!(lookup_keyword("UUID"), Some(Keyword::Uuid));
}

#[test]
fn lookup_uuid_lowercase() {
    assert_eq!(lookup_keyword("uuid"), Some(Keyword::Uuid));
}

#[test]
fn lookup_view() {
    assert_eq!(lookup_keyword("VIEW"), Some(Keyword::View));
}

#[test]
fn lookup_view_lowercase() {
    assert_eq!(lookup_keyword("view"), Some(Keyword::View));
}

#[test]
fn lookup_timestamptz() {
    assert_eq!(lookup_keyword("TIMESTAMPTZ"), Some(Keyword::Timestamptz));
}

#[test]
fn lookup_timestamptz_lowercase() {
    assert_eq!(lookup_keyword("timestamptz"), Some(Keyword::Timestamptz));
}

#[test]
fn lookup_ilike() {
    assert_eq!(lookup_keyword("ILIKE"), Some(Keyword::Ilike));
}

#[test]
fn lookup_ilike_lowercase() {
    assert_eq!(lookup_keyword("ilike"), Some(Keyword::Ilike));
}

// ── Case insensitivity ──────────────────────────────────────────

#[test]
fn case_insensitive_lowercase_select() {
    assert_eq!(lookup_keyword("select"), Some(Keyword::Select));
}

#[test]
fn case_insensitive_uppercase_select() {
    assert_eq!(lookup_keyword("SELECT"), Some(Keyword::Select));
}

#[test]
fn case_insensitive_mixed_case_select() {
    assert_eq!(lookup_keyword("SeLeCt"), Some(Keyword::Select));
}

// ── Non-keywords ────────────────────────────────────────────────

#[test]
fn non_keyword_foobar() {
    assert_eq!(lookup_keyword("foobar"), None);
}

#[test]
fn non_keyword_selectx() {
    assert_eq!(lookup_keyword("SELECTX"), None);
}

#[test]
fn empty_string_returns_none() {
    assert_eq!(lookup_keyword(""), None);
}

// ── name() method ───────────────────────────────────────────────

#[test]
fn keyword_name_matches_uppercase() {
    let all_keywords = vec![
        (Keyword::Add, "ADD"),
        (Keyword::Alter, "ALTER"),
        (Keyword::And, "AND"),
        (Keyword::As, "AS"),
        (Keyword::BigInt, "BIGINT"),
        (Keyword::Begin, "BEGIN"),
        (Keyword::Blob, "BLOB"),
        (Keyword::Case, "CASE"),
        (Keyword::Cast, "CAST"),
        (Keyword::Column, "COLUMN"),
        (Keyword::Copy, "COPY"),
        (Keyword::Delete, "DELETE"),
        (Keyword::Committed, "COMMITTED"),
        (Keyword::Commit, "COMMIT"),
        (Keyword::Create, "CREATE"),
        (Keyword::Date, "DATE"),
        (Keyword::Boolean, "BOOLEAN"),
        (Keyword::Double, "DOUBLE"),
        (Keyword::Drop, "DROP"),
        (Keyword::Else, "ELSE"),
        (Keyword::End, "END"),
        (Keyword::False, "FALSE"),
        (Keyword::From, "FROM"),
        (Keyword::Group, "GROUP"),
        (Keyword::Having, "HAVING"),
        (Keyword::Ilike, "ILIKE"),
        (Keyword::Index, "INDEX"),
        (Keyword::Int, "INT"),
        (Keyword::Interval, "INTERVAL"),
        (Keyword::Insert, "INSERT"),
        (Keyword::Isolation, "ISOLATION"),
        (Keyword::Into, "INTO"),
        (Keyword::Limit, "LIMIT"),
        (Keyword::Level, "LEVEL"),
        (Keyword::Numeric, "NUMERIC"),
        (Keyword::Not, "NOT"),
        (Keyword::Null, "NULL"),
        (Keyword::On, "ON"),
        (Keyword::Read, "READ"),
        (Keyword::Real, "REAL"),
        (Keyword::Rollback, "ROLLBACK"),
        (Keyword::Sequence, "SEQUENCE"),
        (Keyword::Select, "SELECT"),
        (Keyword::Set, "SET"),
        (Keyword::Snapshot, "SNAPSHOT"),
        (Keyword::Start, "START"),
        (Keyword::Stdin, "STDIN"),
        (Keyword::Stdout, "STDOUT"),
        (Keyword::Or, "OR"),
        (Keyword::Table, "TABLE"),
        (Keyword::Text, "TEXT"),
        (Keyword::Then, "THEN"),
        (Keyword::Timestamp, "TIMESTAMP"),
        (Keyword::Transaction, "TRANSACTION"),
        (Keyword::True, "TRUE"),
        (Keyword::Update, "UPDATE"),
        (Keyword::Values, "VALUES"),
        (Keyword::When, "WHEN"),
        (Keyword::Where, "WHERE"),
        (Keyword::Offset, "OFFSET"),
        (Keyword::Coalesce, "COALESCE"),
        (Keyword::Nullif, "NULLIF"),
        (Keyword::Check, "CHECK"),
        (Keyword::Constraint, "CONSTRAINT"),
        (Keyword::Explain, "EXPLAIN"),
        (Keyword::Foreign, "FOREIGN"),
        (Keyword::Key, "KEY"),
        (Keyword::Primary, "PRIMARY"),
        (Keyword::References, "REFERENCES"),
        (Keyword::Release, "RELEASE"),
        (Keyword::Savepoint, "SAVEPOINT"),
        (Keyword::Timestamptz, "TIMESTAMPTZ"),
        (Keyword::To, "TO"),
        (Keyword::Unique, "UNIQUE"),
        (Keyword::Uuid, "UUID"),
        (Keyword::Vector, "VECTOR"),
        (Keyword::View, "VIEW"),
        (Keyword::Node, "NODE"),
        (Keyword::Edge, "EDGE"),
        (Keyword::Label, "LABEL"),
        (Keyword::Source, "SOURCE"),
        (Keyword::Target, "TARGET"),
        (Keyword::Grant, "GRANT"),
        (Keyword::Revoke, "REVOKE"),
        (Keyword::Role, "ROLE"),
        (Keyword::Login, "LOGIN"),
        (Keyword::Password, "PASSWORD"),
        (Keyword::Superuser, "SUPERUSER"),
        (Keyword::Nosuperuser, "NOSUPERUSER"),
        (Keyword::Nologin, "NOLOGIN"),
        (Keyword::Privileges, "PRIVILEGES"),
        (Keyword::Schema, "SCHEMA"),
        (Keyword::With, "WITH"),
    ];
    for (kw, expected_name) in all_keywords {
        assert_eq!(kw.name(), expected_name, "failed for {kw:?}");
    }
}
