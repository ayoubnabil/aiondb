use super::*;

// ===================================================================
// initcap
// ===================================================================

#[test]
fn initcap_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT initcap('hello world')")
        .expect("initcap");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Text("Hello World".into()));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn initcap_mixed_case() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT initcap('hELLO wORLD')")
        .expect("initcap");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Text("Hello World".into()));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// split_part
// ===================================================================

#[test]
fn split_part_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT split_part('a.b.c', '.', 2)")
        .expect("split_part");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Text("b".into()));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn split_part_out_of_range() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT split_part('a.b.c', '.', 5)")
        .expect("split_part");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Text(String::new()));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// translate
// ===================================================================

#[test]
fn translate_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT translate('12345', '143', 'ax')")
        .expect("translate");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Text("a2x5".into()));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// chr
// ===================================================================

#[test]
fn chr_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine.execute_sql(&session, "SELECT chr(65)").expect("chr");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Text("A".into()));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// ascii
// ===================================================================

#[test]
fn ascii_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT ascii('A')")
        .expect("ascii");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Int(65));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// md5
// ===================================================================

#[test]
fn md5_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT md5('hello')")
        .expect("md5");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(
                rows[0].values[0],
                Value::Text("5d41402abc4b2a76b9719d911017c592".into())
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn fipshash_compat_alias_matches_md5_length() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT length(fipshash('hello'))")
        .expect("fipshash");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Int(32));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn pg_size_pretty_formats_expected_units() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT pg_size_pretty(10::bigint), pg_size_pretty(10240::bigint), pg_size_pretty(10.5::numeric)",
        )
        .expect("pg_size_pretty");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Text("10 bytes".into()));
            assert_eq!(rows[0].values[1], Value::Text("10 kB".into()));
            assert_eq!(rows[0].values[2], Value::Text("10.5 bytes".into()));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn pg_size_bytes_parses_common_units() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT pg_size_bytes('1.5 GB'), pg_size_bytes('-.1kb'), pg_size_bytes('99 PB')",
        )
        .expect("pg_size_bytes");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::BigInt(1_610_612_736));
            assert_eq!(rows[0].values[1], Value::BigInt(-102));
            assert_eq!(rows[0].values[2], Value::BigInt(111_464_090_777_419_776));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// quote_literal
// ===================================================================

#[test]
fn quote_literal_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT quote_literal('hello')")
        .expect("quote_literal");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Text("'hello'".into()));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// quote_ident
// ===================================================================

#[test]
fn quote_ident_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT quote_ident('my_col')")
        .expect("quote_ident");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            // PG: quote_ident only adds quotes when the identifier needs quoting.
            // 'my_col' is a valid simple identifier, so no quotes needed.
            assert_eq!(rows[0].values[0], Value::Text("my_col".into()));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// quote_nullable
// ===================================================================

#[test]
fn quote_nullable_text() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT quote_nullable('hello')")
        .expect("quote_nullable");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Text("'hello'".into()));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// to_hex
// ===================================================================

#[test]
fn to_hex_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT to_hex(255)")
        .expect("to_hex");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Text("ff".into()));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// pg_input_is_valid
// ===================================================================

#[test]
fn pg_input_is_valid_common_scalar_types() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT \
                pg_input_is_valid('34', 'int4'), \
                pg_input_is_valid('asdf', 'int4'), \
                pg_input_is_valid('2024-03-15', 'date'), \
                pg_input_is_valid('garbage', 'date'), \
                pg_input_is_valid('{1,2,3}', 'integer[]'), \
                pg_input_is_valid('{1,zed}', 'integer[]')",
        )
        .expect("pg_input_is_valid");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].values,
                vec![
                    Value::Boolean(true),
                    Value::Boolean(false),
                    Value::Boolean(true),
                    Value::Boolean(false),
                    Value::Boolean(true),
                    Value::Boolean(false),
                ]
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn pg_input_is_valid_ranges_and_network_types() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT \
                pg_input_is_valid('[1,4)', 'int4range'), \
                pg_input_is_valid('[1,zed)', 'int4range'), \
                pg_input_is_valid('{[1,4),[10,20)}', 'int4multirange'), \
                pg_input_is_valid('{[1,zed)}', 'int4multirange'), \
                pg_input_is_valid('192.168.1.10/24', 'inet'), \
                pg_input_is_valid('192.168.1.10/99', 'inet'), \
                pg_input_is_valid('08:00:2b:01:02:03', 'macaddr'), \
                pg_input_is_valid('08:00:2b:01:02:ZZ', 'macaddr')",
        )
        .expect("pg_input_is_valid");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].values,
                vec![
                    Value::Boolean(true),
                    Value::Boolean(false),
                    Value::Boolean(true),
                    Value::Boolean(false),
                    Value::Boolean(true),
                    Value::Boolean(false),
                    Value::Boolean(true),
                    Value::Boolean(false),
                ]
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn generic_multirange_wraps_single_range() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT multirange(int4range(1, 4))")
        .expect("multirange");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Text("{[1,4)}".into()));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn generic_multirange_merges_supported_ranges() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT multirange(int4range(1, 4), int4range(4, 8), int4range(10, 12))",
        )
        .expect("multirange");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Text("{[1,8),[10,12)}".into()));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// regexp_replace
// ===================================================================

#[test]
fn regexp_replace_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT regexp_replace('foobarbaz', 'b..', 'X')")
        .expect("regexp_replace");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Text("fooXbaz".into()));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn regexp_replace_global() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT regexp_replace('foobarbaz', 'b..', 'X', 'g')",
        )
        .expect("regexp_replace global");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Text("fooXX".into()));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// regexp_match
// ===================================================================

#[test]
fn regexp_match_full() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT regexp_match('foobarbaz', 'bar')")
        .expect("regexp_match");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            // PG regexp_match returns text[] (an array)
            assert_eq!(
                rows[0].values[0],
                Value::Array(vec![Value::Text("bar".into())])
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn regexp_match_capture_group() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT regexp_match('foobarbaz', 'foo(b..)baz')")
        .expect("regexp_match capture");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            // With capture groups, regexp_match returns the captured groups
            assert_eq!(
                rows[0].values[0],
                Value::Array(vec![Value::Text("bar".into())])
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn regexp_match_no_match() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT regexp_match('hello', 'xyz')")
        .expect("regexp_match no match");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Null);
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn regexp_match_case_insensitive() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT regexp_match('Hello World', 'hello', 'i')")
        .expect("regexp_match case insensitive");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            // PG regexp_match returns text[]
            assert_eq!(
                rows[0].values[0],
                Value::Array(vec![Value::Text("Hello".into())])
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// regex operators
// ===================================================================

#[test]
fn regex_operators_work() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT 'Hello' ~ 'ell', 'Hello' ~* 'hEL', 'Hello' !~ 'xyz', 'Hello' !~* 'hEL'",
        )
        .expect("regex operators");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].values,
                vec![
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Boolean(false),
                ]
            );
        }
        other => panic!("expected query, got {other:?}"),
    }
}

// ===================================================================
// encode / decode
// ===================================================================

#[test]
fn encode_hex() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT encode('hello', 'hex')")
        .expect("encode hex");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Text("68656c6c6f".into()));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn encode_base64() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT encode('hello', 'base64')")
        .expect("encode base64");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Text("aGVsbG8=".into()));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn decode_hex() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT decode('68656c6c6f', 'hex')")
        .expect("decode hex");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Blob(b"hello".to_vec()));
        }
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn decode_base64() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT decode('aGVsbG8=', 'base64')")
        .expect("decode base64");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], Value::Blob(b"hello".to_vec()));
        }
        other => panic!("expected query, got {other:?}"),
    }
}
