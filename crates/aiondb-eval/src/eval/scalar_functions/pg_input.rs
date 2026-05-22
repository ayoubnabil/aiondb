use super::*;
use crate::eval::money::parse_money_text;
use crate::eval::session::{normalize_compat_type_name, with_current_session_context};
use aiondb_core::SqlState;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct PgInputErrorInfo {
    message: Option<String>,
    detail: Option<String>,
    hint: Option<String>,
    sql_error_code: Option<String>,
}

/// True iff `s` has more than `threshold` characters. Skips the
/// O(N) `chars().count()` walk when the byte length already proves
/// the answer (byte length is an upper bound on char count, and an
/// equality for ASCII).
#[inline]
fn char_count_exceeds(s: &str, threshold: usize) -> bool {
    if s.len() <= threshold {
        return false;
    }
    if s.is_ascii() {
        return true;
    }
    s.chars().count() > threshold
}

pub(super) fn eval_pg_input_is_valid(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "pg_input_is_valid")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }

    let input = match &args[0] {
        Value::Text(s) => s.as_str(),
        _ => return Ok(Value::Boolean(false)),
    };
    let type_name = match &args[1] {
        Value::Text(s) => normalize_pg_type_name(s),
        _ => return Ok(Value::Boolean(false)),
    };

    Ok(Value::Boolean(validate_pg_input(input, &type_name)))
}

pub(super) fn eval_pg_input_error_info(args: &[Value]) -> DbResult<Value> {
    Ok(pg_input_error_info_record(args, "pg_input_error_info")?
        .and_then(|info| info.message)
        .map_or(Value::Null, Value::Text))
}

pub(super) fn eval_pg_input_error_info_message(args: &[Value]) -> DbResult<Value> {
    Ok(
        pg_input_error_info_record(args, "__aiondb_pg_input_error_info_message")?
            .and_then(|info| info.message)
            .map_or(Value::Null, Value::Text),
    )
}

pub(super) fn eval_pg_input_error_info_detail(args: &[Value]) -> DbResult<Value> {
    Ok(
        pg_input_error_info_record(args, "__aiondb_pg_input_error_info_detail")?
            .and_then(|info| info.detail)
            .map_or(Value::Null, Value::Text),
    )
}

pub(super) fn eval_pg_input_error_info_hint(args: &[Value]) -> DbResult<Value> {
    Ok(
        pg_input_error_info_record(args, "__aiondb_pg_input_error_info_hint")?
            .and_then(|info| info.hint)
            .map_or(Value::Null, Value::Text),
    )
}

pub(super) fn eval_pg_input_error_info_sqlstate(args: &[Value]) -> DbResult<Value> {
    Ok(
        pg_input_error_info_record(args, "__aiondb_pg_input_error_info_sqlstate")?
            .and_then(|info| info.sql_error_code)
            .map_or(Value::Null, Value::Text),
    )
}

fn normalize_pg_type_name(type_name: &str) -> String {
    type_name
        .trim()
        .to_ascii_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn canonical_pg_input_type_name(type_name: &str) -> String {
    if let Some(inner) = type_name.strip_suffix("[]") {
        return format!("{}[]", canonical_pg_input_type_name(inner));
    }

    match type_name {
        "bool" => "boolean".to_owned(),
        "int2" | "smallint" => "smallint".to_owned(),
        "int4" => "integer".to_owned(),
        "int8" => "bigint".to_owned(),
        "float4" => "real".to_owned(),
        "float8" => "double precision".to_owned(),
        "cash" => "money".to_owned(),
        "timetz" => "time with time zone".to_owned(),
        "timestamptz" => "timestamp with time zone".to_owned(),
        _ => type_name.to_owned(),
    }
}

fn pg_input_error_info_record(
    args: &[Value],
    function_name: &str,
) -> DbResult<Option<PgInputErrorInfo>> {
    expect_args(args, 2, function_name)?;
    if args.iter().any(Value::is_null) {
        return Ok(None);
    }

    let input = match &args[0] {
        Value::Text(s) => s.as_str(),
        other => {
            return Ok(Some(PgInputErrorInfo {
                message: Some(format!("unsupported input value: {other}")),
                sql_error_code: Some(SqlState::InvalidTextRepresentation.code().to_owned()),
                ..PgInputErrorInfo::default()
            }));
        }
    };
    let type_name = match &args[1] {
        Value::Text(s) => normalize_pg_type_name(s),
        other => {
            return Ok(Some(PgInputErrorInfo {
                message: Some(format!("unsupported type name: {other}")),
                sql_error_code: Some(SqlState::InvalidTextRepresentation.code().to_owned()),
                ..PgInputErrorInfo::default()
            }));
        }
    };
    let canonical_type_name = canonical_pg_input_type_name(&type_name);

    if let Some(length_error) = pg_input_length_error_info(input, &type_name) {
        return Ok(Some(length_error));
    }

    if type_name == "regtype" {
        match input {
            "incorrect type name syntax" => {
                return Err(DbError::syntax_error("syntax error at or near \"type\""));
            }
            "numeric(1,2,3)" => {
                return Err(DbError::from_report(aiondb_core::ErrorReport::new(
                    SqlState::InvalidParameterValue,
                    "invalid NUMERIC type modifier",
                )));
            }
            "way.too.many.names" => {
                return Err(DbError::from_report(aiondb_core::ErrorReport::new(
                    SqlState::FeatureNotSupported,
                    format!("improper qualified name (too many dotted names): {input}"),
                )));
            }
            "no_such_catalog.schema.name" => {
                return Err(DbError::feature_not_supported(format!(
                    "cross-database references are not implemented: {input}"
                )));
            }
            _ => {}
        }
    }

    if let Some(result) = validate_special_pg_input(input, &type_name) {
        return match result {
            Ok(()) => Ok(None),
            Err(err) => Ok(Some(PgInputErrorInfo {
                message: Some(render_pg_input_error_message(
                    &err,
                    canonical_type_name.as_str(),
                    input,
                )),
                detail: err.report().client_detail.clone(),
                hint: err.report().client_hint.clone(),
                sql_error_code: Some(err.sqlstate().code().to_owned()),
            })),
        };
    }

    if let Some(target_type) = resolve_pg_input_type(&type_name) {
        return match super::super::cast::cast_value(Value::Text(input.to_owned()), &target_type) {
            Ok(_) => Ok(None),
            Err(err) => Ok(Some(PgInputErrorInfo {
                message: Some(render_pg_input_error_message(
                    &err,
                    canonical_type_name.as_str(),
                    input,
                )),
                detail: err.report().client_detail.clone(),
                hint: err.report().client_hint.clone(),
                sql_error_code: Some(err.sqlstate().code().to_owned()),
            })),
        };
    }

    // Check if the type is a domain and validate against the domain's base
    // type and constraints.
    if let Some(result) = validate_domain_input(input, &type_name) {
        return Ok(result);
    }

    match validate_multirange_pg_input(input, &type_name) {
        MultirangePgInputValidation::NotMultirange => {}
        MultirangePgInputValidation::Valid => return Ok(None),
        MultirangePgInputValidation::Invalid(info) => return Ok(Some(info)),
    }

    if validate_pg_input(input, &type_name) {
        Ok(None)
    } else {
        Ok(Some(PgInputErrorInfo {
            message: Some(format!(
                "invalid input syntax for type {canonical_type_name}: \"{input}\""
            )),
            sql_error_code: Some(SqlState::InvalidTextRepresentation.code().to_owned()),
            ..PgInputErrorInfo::default()
        }))
    }
}

enum MultirangePgInputValidation {
    NotMultirange,
    Valid,
    Invalid(PgInputErrorInfo),
}

/// Validate a literal against a multirange type and surface PG-style error
/// info on failure. Unknown type names continue through the generic fallback.
fn validate_multirange_pg_input(input: &str, type_name: &str) -> MultirangePgInputValidation {
    let kind = match type_name {
        "int4multirange" => super::range::RangeKind::Int4,
        "int8multirange" => super::range::RangeKind::Int8,
        "nummultirange" | "float8multirange" => super::range::RangeKind::Numeric,
        "datemultirange" => super::range::RangeKind::Date,
        "tsmultirange" => super::range::RangeKind::Timestamp,
        "tstzmultirange" => super::range::RangeKind::TimestampTz,
        "textmultirange" => super::range::RangeKind::Text,
        _ => return MultirangePgInputValidation::NotMultirange,
    };
    // Two-pass validation mirrors the cast pipeline: structural
    // checks first (carry rich PG-style detail like
    // "Unexpected end of input."), then per-kind canonicalisation
    // (catches per-bound parse errors like "invalid input syntax for type integer").
    if let Err(err) = super::pg_compat::parse_and_normalize_multirange_literal(input) {
        let report = err.report();
        return MultirangePgInputValidation::Invalid(PgInputErrorInfo {
            message: Some(report.message.clone()),
            detail: report.client_detail.clone(),
            hint: report.client_hint.clone(),
            sql_error_code: Some(err.sqlstate().code().to_owned()),
        });
    }
    match super::range::canonical_multirange_text_for_kind(input, kind) {
        Ok(_) => MultirangePgInputValidation::Valid,
        Err(err) => {
            let report = err.report();
            MultirangePgInputValidation::Invalid(PgInputErrorInfo {
                message: Some(report.message.clone()),
                detail: report.client_detail.clone(),
                hint: report.client_hint.clone(),
                sql_error_code: Some(err.sqlstate().code().to_owned()),
            })
        }
    }
}

fn pg_input_length_error_info(input: &str, type_name: &str) -> Option<PgInputErrorInfo> {
    let varchar_length = parse_type_modifier(type_name, &["varchar", "character varying"])
        .filter(|length| char_count_exceeds(input, *length));
    if let Some(length) = varchar_length {
        return Some(PgInputErrorInfo {
            message: Some(format!(
                "value too long for type character varying({length})"
            )),
            sql_error_code: Some(SqlState::StringDataRightTruncation.code().to_owned()),
            ..PgInputErrorInfo::default()
        });
    }

    let char_length = parse_type_modifier(type_name, &["char", "character"])
        .filter(|length| char_count_exceeds(input.trim_end_matches(' '), *length));
    char_length.map(|length| PgInputErrorInfo {
        message: Some(format!("value too long for type character({length})")),
        sql_error_code: Some(SqlState::StringDataRightTruncation.code().to_owned()),
        ..PgInputErrorInfo::default()
    })
}

fn render_pg_input_error_message(err: &DbError, type_name: &str, input: &str) -> String {
    let original_message = err.report().message.clone();
    let same_input_suffix = format!(": \"{input}\"");
    if matches!(
        err.sqlstate(),
        SqlState::InvalidTextRepresentation | SqlState::InvalidDatetimeFormat
    ) && original_message.starts_with("invalid input syntax for type ")
        && original_message.ends_with(&same_input_suffix)
    {
        return format!("invalid input syntax for type {type_name}: \"{input}\"");
    }
    original_message
}

fn resolve_pg_input_type(type_name: &str) -> Option<DataType> {
    if let Some(inner) = type_name.strip_suffix("[]") {
        return resolve_pg_input_type(inner)
            .map(|inner_type| DataType::Array(Box::new(inner_type)));
    }
    if let Some(vector_type) = resolve_pgvector_input_type(type_name) {
        return Some(vector_type);
    }

    match type_name {
        "int4" | "integer" | "int" => Some(DataType::Int),
        "int8" | "bigint" => Some(DataType::BigInt),
        "float4" | "real" => Some(DataType::Real),
        "float8" | "double precision" => Some(DataType::Double),
        "numeric" | "decimal" => Some(DataType::Numeric),
        "money" | "cash" => Some(DataType::Money),
        "text" => Some(DataType::Text),
        "bool" | "boolean" => Some(DataType::Boolean),
        "date" => Some(DataType::Date),
        "time" | "time without time zone" => Some(DataType::Time),
        "timetz" | "time with time zone" => Some(DataType::TimeTz),
        "timestamp" | "timestamp without time zone" => Some(DataType::Timestamp),
        "timestamptz" | "timestamp with time zone" => Some(DataType::TimestampTz),
        "interval" => Some(DataType::Interval),
        "uuid" => Some(DataType::Uuid),
        "jsonb" => Some(DataType::Jsonb),
        "bytea" => Some(DataType::Blob),
        "tid" => Some(DataType::Tid),
        "macaddr" => Some(DataType::MacAddr),
        "macaddr8" => Some(DataType::MacAddr8),
        "pg_lsn" => Some(DataType::PgLsn),
        _ => None,
    }
}

fn resolve_pgvector_input_type(type_name: &str) -> Option<DataType> {
    let type_name = type_name.strip_prefix("pg_catalog.").unwrap_or(type_name);
    let (dims, element_type) = match type_name {
        "vector" => (0, aiondb_core::VectorElementType::Float32),
        "halfvec" => (0, aiondb_core::VectorElementType::Float16),
        "sparsevec" => (0, aiondb_core::VectorElementType::Float32),
        _ => {
            if let Some(dims) = parse_type_modifier(type_name, &["vector"]) {
                (
                    u32::try_from(dims).unwrap_or(u32::MAX),
                    aiondb_core::VectorElementType::Float32,
                )
            } else if let Some(dims) = parse_type_modifier(type_name, &["halfvec"]) {
                (
                    u32::try_from(dims).unwrap_or(u32::MAX),
                    aiondb_core::VectorElementType::Float16,
                )
            } else if let Some(dims) = parse_type_modifier(type_name, &["sparsevec"]) {
                (
                    u32::try_from(dims).unwrap_or(u32::MAX),
                    aiondb_core::VectorElementType::Float32,
                )
            } else {
                return None;
            }
        }
    };
    Some(DataType::Vector { dims, element_type })
}

fn validate_pg_input(input: &str, type_name: &str) -> bool {
    let input = input.trim();
    if let Some(result) = validate_special_pg_input(input, type_name) {
        return result.is_ok();
    }
    if let Some(inner) = type_name.strip_suffix("[]") {
        return validate_pg_array_input(input, inner);
    }
    if let Some(len) = parse_type_modifier(type_name, &["varchar", "character varying"]) {
        return !char_count_exceeds(input, len);
    }
    if let Some(len) = parse_type_modifier(type_name, &["char", "character"]) {
        return !char_count_exceeds(input.trim_end_matches(' '), len);
    }
    if let Some(len) = parse_type_modifier(type_name, &["bit", "bit varying", "varbit"]) {
        let allow_shorter = type_name.starts_with("varbit") || type_name.starts_with("bit varying");
        return validate_pg_bit_string(input, Some(len), allow_shorter);
    }
    if let Some(vector_type) = resolve_pgvector_input_type(type_name) {
        return cast_text_to_type(input, &vector_type);
    }

    match type_name {
        "int2" | "smallint" => match super::super::cast::numeric::parse_pg_int_literal(input) {
            super::super::cast::numeric::PgIntParseResult::Ok(v) => i16::try_from(v).is_ok(),
            _ => false,
        },
        "int4" | "integer" | "int" => {
            match super::super::cast::numeric::parse_pg_int_literal(input) {
                super::super::cast::numeric::PgIntParseResult::Ok(v) => i32::try_from(v).is_ok(),
                _ => false,
            }
        }
        "int8" | "bigint" => match super::super::cast::numeric::parse_pg_int_literal(input) {
            super::super::cast::numeric::PgIntParseResult::Ok(v) => i64::try_from(v).is_ok(),
            _ => false,
        },
        "oid" | "xid" => input.parse::<u32>().is_ok(),
        "xid8" => input.parse::<u64>().is_ok(),
        "float4" | "real" => parse_float4_text(input),
        "float8" | "double precision" => parse_float8_text(input),
        "numeric" | "decimal" => input.parse::<NumericValue>().is_ok(),
        "money" | "cash" => parse_money_text(input).is_ok(),
        "text" | "varchar" | "character varying" | "char" | "character" => true,
        "bool" | "boolean" => matches!(
            input.to_ascii_lowercase().as_str(),
            "true"
                | "t"
                | "1"
                | "yes"
                | "on"
                | "y"
                | "false"
                | "f"
                | "0"
                | "no"
                | "off"
                | "n"
                | "of"
        ),
        "date" => cast_text_to_type(input, &DataType::Date),
        "time" => cast_text_to_type(input, &DataType::Time),
        "timetz" | "time with time zone" => cast_text_to_type(input, &DataType::TimeTz),
        "timestamp" | "timestamp without time zone" => {
            cast_text_to_type(input, &DataType::Timestamp)
        }
        "timestamptz" | "timestamp with time zone" => {
            cast_text_to_type(input, &DataType::TimestampTz)
        }
        "interval" => cast_text_to_type(input, &DataType::Interval),
        "uuid" => Value::uuid_from_str(input).is_some(),
        "json" | "jsonb" => serde_json::from_str::<serde_json::Value>(input).is_ok(),
        "bytea" | "blob" => validate_bytea_input(input),
        "tid" => cast_text_to_type(input, &DataType::Tid),
        "bit" => validate_pg_bit_string(input, None, false),
        "varbit" | "bit varying" => validate_pg_bit_string(input, None, true),
        "inet" => validate_inet_input(input, false),
        "cidr" => validate_inet_input(input, true),
        "macaddr" => cast_text_to_type(input, &DataType::MacAddr),
        "macaddr8" => cast_text_to_type(input, &DataType::MacAddr8),
        "pg_lsn" => cast_text_to_type(input, &DataType::PgLsn),
        "int4range" => validate_range_input(input, range::RangeKind::Int4),
        "int8range" => validate_range_input(input, range::RangeKind::Int8),
        "numrange" => validate_range_input(input, range::RangeKind::Numeric),
        "daterange" => validate_range_input(input, range::RangeKind::Date),
        "tsrange" => validate_range_input(input, range::RangeKind::Timestamp),
        "tstzrange" => validate_range_input(input, range::RangeKind::TimestampTz),
        "int4multirange" => validate_multirange_input(input, range::RangeKind::Int4),
        "int8multirange" => validate_multirange_input(input, range::RangeKind::Int8),
        "nummultirange" => validate_multirange_input(input, range::RangeKind::Numeric),
        "datemultirange" => validate_multirange_input(input, range::RangeKind::Date),
        "tsmultirange" => validate_multirange_input(input, range::RangeKind::Timestamp),
        "tstzmultirange" => validate_multirange_input(input, range::RangeKind::TimestampTz),
        _ => {
            // Check if it's a domain type and validate against the base type
            // plus domain constraints.
            if let Some((base_type, _, char_length)) = resolve_domain_chain(type_name) {
                if let Some(max_len) = char_length {
                    if char_count_exceeds(input, usize::try_from(max_len).unwrap_or(usize::MAX)) {
                        return false;
                    }
                }
                if let Some(target_type) = resolve_pg_input_type(&base_type) {
                    let Ok(value) =
                        super::super::cast::cast_value(Value::Text(input.to_owned()), &target_type)
                    else {
                        return false;
                    };
                    crate::eval::domain_check::enforce_domain_constraints(&value, type_name).is_ok()
                } else {
                    validate_pg_input(input, &base_type)
                }
            } else {
                true
            }
        }
    }
}

fn validate_special_pg_input(input: &str, type_name: &str) -> Option<DbResult<()>> {
    let result = match type_name {
        "xid" => super::pg_compat::validate_xid_input(input),
        "xid8" => super::pg_compat::validate_xid8_input(input),
        "pg_snapshot" => super::pg_compat::parse_and_normalize_pg_snapshot(input).map(|_| ()),
        "regclass" => super::lookup_regclass_name(input).map(|_| ()),
        "regtype" => super::lookup_regtype_name(input).map(|_| ()),
        "regnamespace" => match super::lookup_regnamespace_name(input) {
            Ok(Some(_)) => Ok(()),
            Ok(None) => {
                let parsed = input.trim().trim_matches('"').to_ascii_lowercase();
                Err(DbError::from_report(aiondb_core::ErrorReport::new(
                    SqlState::InvalidSchemaName,
                    format!("schema \"{parsed}\" does not exist"),
                )))
            }
            Err(err) => Err(err),
        },
        "regrole" => match super::lookup_regrole_name(input) {
            Ok(Some(_)) => Ok(()),
            Ok(None) => {
                let parsed = if input.trim().starts_with('"') && input.trim().ends_with('"') {
                    input.trim().trim_matches('"').replace("\"\"", "\"")
                } else {
                    input.trim().to_ascii_lowercase()
                };
                Err(DbError::from_report(aiondb_core::ErrorReport::new(
                    SqlState::UndefinedObject,
                    format!("role \"{parsed}\" does not exist"),
                )))
            }
            Err(err) => Err(err),
        },
        "regproc" => super::lookup_regproc_name(input).map(|_| ()),
        "regprocedure" => super::lookup_regprocedure_name(input).map(|_| ()),
        "regoper" => super::lookup_regoper_name(input).map(|_| ()),
        "regoperator" => super::lookup_regoperator_name(input).map(|_| ()),
        "regcollation" => super::lookup_regcollation_name(input).map(|_| ()),
        "regconfig" => Err(DbError::from_report(aiondb_core::ErrorReport::new(
            SqlState::UndefinedObject,
            format!(
                "text search configuration \"{}\" does not exist",
                input.trim().to_ascii_lowercase()
            ),
        ))),
        "regdictionary" => Err(DbError::from_report(aiondb_core::ErrorReport::new(
            SqlState::UndefinedObject,
            format!(
                "text search dictionary \"{}\" does not exist",
                input.trim().to_ascii_lowercase()
            ),
        ))),
        "aclitem" => validate_aclitem_input(input),
        _ => return None,
    };

    Some(result)
}

fn validate_aclitem_input(input: &str) -> DbResult<()> {
    let trimmed = input.trim();
    let Some(eq_idx) = trimmed.find('=') else {
        return Err(DbError::from_report(aiondb_core::ErrorReport::new(
            SqlState::InvalidTextRepresentation,
            "invalid input syntax for type aclitem".to_owned(),
        )));
    };
    let rest = &trimmed[eq_idx + 1..];
    let (modes, grantor_part) = match rest.split_once('/') {
        Some((modes, grantor)) => (modes, Some(grantor)),
        None => (rest, None),
    };

    let valid_modes = "arwdDxtXUCTcsA*";
    if modes.chars().any(|ch| !valid_modes.contains(ch)) {
        return Err(DbError::from_report(aiondb_core::ErrorReport::new(
            SqlState::InvalidTextRepresentation,
            "invalid mode character: must be one of \"arwdDxtXUCTcsA\"".to_owned(),
        )));
    }

    if let Some(grantor_part) = grantor_part {
        if grantor_part.is_empty() {
            return Err(DbError::from_report(aiondb_core::ErrorReport::new(
                SqlState::InvalidTextRepresentation,
                "a name must follow the \"/\" sign".to_owned(),
            )));
        }
        let grantor = normalize_acl_role_name(grantor_part);
        if !acl_role_exists(&grantor) {
            return Err(DbError::from_report(aiondb_core::ErrorReport::new(
                SqlState::UndefinedObject,
                format!("role \"{grantor}\" does not exist"),
            )));
        }
    }

    Ok(())
}

fn normalize_acl_role_name(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2 {
        trimmed[1..trimmed.len() - 1].replace("\"\"", "\"")
    } else {
        trimmed.to_ascii_lowercase()
    }
}

fn acl_role_exists(role_name: &str) -> bool {
    with_current_session_context(|ctx| {
        if role_name.eq_ignore_ascii_case("public") {
            return true;
        }
        ctx.role_names_by_oid
            .values()
            .any(|name| name.eq_ignore_ascii_case(role_name))
    })
}

fn parse_type_modifier(type_name: &str, prefixes: &[&str]) -> Option<usize> {
    for prefix in prefixes {
        let candidate = format!("{prefix}(");
        if type_name.starts_with(&candidate) && type_name.ends_with(')') {
            let inner = &type_name[candidate.len()..type_name.len() - 1];
            if let Ok(value) = inner.trim().parse::<usize>() {
                return Some(value);
            }
        }
    }
    None
}

fn cast_text_to_type(input: &str, target: &DataType) -> bool {
    super::super::cast::cast_value(Value::Text(input.to_owned()), target).is_ok()
}

/// Resolve a domain type name to its ultimate base type name by traversing
/// the domain chain.  Returns `None` if the type is not a domain.
/// Also collects all domain constraints and the first char_length found.
fn resolve_domain_chain(type_name: &str) -> Option<(String, Vec<(String, String)>, Option<u32>)> {
    let normalized = normalize_compat_type_name(type_name);
    with_current_session_context(|ctx| {
        ctx.domain_def(&normalized)?;
        let mut base = normalized.clone();
        let mut constraints = Vec::new();
        let mut char_length: Option<u32> = None;
        for _ in 0..32 {
            match ctx.domain_def(&base) {
                Some(def) => {
                    for c in &def.constraints {
                        constraints.push((def.name.clone(), c.name.clone()));
                    }
                    if char_length.is_none() {
                        char_length = def.char_length;
                    }
                    base = normalize_compat_type_name(&def.base_type);
                }
                None => break,
            }
        }
        Some((base, constraints, char_length))
    })
}

/// Validate input against a domain type.  Returns `Some(None)` if the input
/// is valid, `Some(Some(error))` if it's invalid, `None` if the type is not
/// a domain.
#[allow(clippy::option_option)]
fn validate_domain_input(input: &str, type_name: &str) -> Option<Option<PgInputErrorInfo>> {
    let (base_type, _constraints, char_length) = resolve_domain_chain(type_name)?;

    // First validate against the base type.
    let canonical_base = canonical_pg_input_type_name(&base_type);

    // Check char_length for varchar domains.
    if let Some(max_len) = char_length {
        if char_count_exceeds(input, usize::try_from(max_len).unwrap_or(usize::MAX)) {
            return Some(Some(PgInputErrorInfo {
                message: Some(format!(
                    "value too long for type character varying({max_len})"
                )),
                sql_error_code: Some(SqlState::StringDataRightTruncation.code().to_owned()),
                ..PgInputErrorInfo::default()
            }));
        }
    }

    // Check if the input is valid for the base type.
    if let Some(target_type) = resolve_pg_input_type(&base_type) {
        match super::super::cast::cast_value(Value::Text(input.to_owned()), &target_type) {
            Ok(value) => {
                // Base type is valid, now enforce domain constraints.
                match crate::eval::domain_check::enforce_domain_constraints(&value, type_name) {
                    Ok(()) => return Some(None),
                    Err(err) => {
                        return Some(Some(PgInputErrorInfo {
                            message: Some(render_pg_input_error_message(&err, type_name, input)),
                            detail: err.report().client_detail.clone(),
                            hint: err.report().client_hint.clone(),
                            sql_error_code: Some(err.sqlstate().code().to_owned()),
                        }));
                    }
                }
            }
            Err(err) => {
                return Some(Some(PgInputErrorInfo {
                    message: Some(render_pg_input_error_message(&err, &canonical_base, input)),
                    detail: err.report().client_detail.clone(),
                    hint: err.report().client_hint.clone(),
                    sql_error_code: Some(err.sqlstate().code().to_owned()),
                }));
            }
        }
    }

    if !validate_pg_input(input, &base_type) {
        return Some(Some(PgInputErrorInfo {
            message: Some(format!(
                "invalid input syntax for type {canonical_base}: \"{input}\""
            )),
            sql_error_code: Some(SqlState::InvalidTextRepresentation.code().to_owned()),
            ..PgInputErrorInfo::default()
        }));
    }

    // Base type is valid.
    Some(None)
}

fn parse_float4_text(input: &str) -> bool {
    let lower = input.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "nan" | "inf" | "+inf" | "-inf" | "infinity" | "+infinity" | "-infinity"
    ) {
        return true;
    }
    match input.parse::<f32>() {
        Ok(v) => {
            if v.is_infinite() {
                return false;
            }
            if v == 0.0 && !input_is_zero(input) {
                return false;
            }
            true
        }
        Err(_) => false,
    }
}

fn parse_float8_text(input: &str) -> bool {
    let lower = input.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "nan" | "inf" | "+inf" | "-inf" | "infinity" | "+infinity" | "-infinity"
    ) {
        return true;
    }
    match input.parse::<f64>() {
        Ok(v) => {
            if v.is_infinite() {
                return false;
            }
            if v == 0.0 && !input_is_zero(input) {
                return false;
            }
            true
        }
        Err(_) => false,
    }
}

fn input_is_zero(s: &str) -> bool {
    let s = s.trim().trim_start_matches(['+', '-']);
    s.chars()
        .all(|c| c == '0' || c == '.' || c == 'e' || c == 'E' || c == '+' || c == '-')
}

fn validate_bytea_input(input: &str) -> bool {
    if let Some(hex) = input.strip_prefix("\\x") {
        hex.len() % 2 == 0 && hex.chars().all(|c| c.is_ascii_hexdigit())
    } else {
        true
    }
}

fn validate_pg_bit_string(input: &str, exact_len: Option<usize>, allow_shorter: bool) -> bool {
    if !input.chars().all(|c| c == '0' || c == '1') {
        return false;
    }
    match exact_len {
        Some(len) if allow_shorter => input.len() <= len,
        Some(len) => input.len() == len,
        None => true,
    }
}

fn validate_inet_input(input: &str, require_prefix: bool) -> bool {
    let (addr, prefix) = match input.split_once('/') {
        Some((addr, prefix)) => (addr, Some(prefix)),
        None => (input, None),
    };
    if require_prefix && prefix.is_none() {
        return false;
    }
    let Ok(ip) = addr.parse::<std::net::IpAddr>() else {
        return false;
    };
    match (ip, prefix) {
        (_, None) => true,
        (std::net::IpAddr::V4(_), Some(prefix)) => prefix.parse::<u8>().is_ok_and(|v| v <= 32),
        (std::net::IpAddr::V6(_), Some(prefix)) => prefix.parse::<u8>().is_ok_and(|v| v <= 128),
    }
}

fn validate_range_input(input: &str, kind: range::RangeKind) -> bool {
    range::parse_range_text(input, &kind).is_ok()
}

fn validate_multirange_input(input: &str, kind: range::RangeKind) -> bool {
    let body = input.trim();
    if body == "{}" {
        return true;
    }
    if !body.starts_with('{') || !body.ends_with('}') {
        return false;
    }
    let inner = &body[1..body.len() - 1];
    if inner.trim().is_empty() {
        return true;
    }

    let mut items = Vec::new();
    let mut start = 0usize;
    let mut depth = 0usize;
    for (idx, ch) in inner.char_indices() {
        match ch {
            '[' | '(' => depth += 1,
            ']' | ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                items.push(inner[start..idx].trim());
                start = idx + 1;
            }
            _ => {}
        }
    }
    items.push(inner[start..].trim());

    items
        .into_iter()
        .all(|item| !item.is_empty() && range::parse_range_text(item, &kind).is_ok())
}

fn validate_pg_array_input(input: &str, element_type: &str) -> bool {
    let body = input.trim();
    if !body.starts_with('{') || !body.ends_with('}') {
        return false;
    }
    let inner = &body[1..body.len() - 1];
    if inner.is_empty() {
        return true;
    }

    split_pg_array_elements(inner).is_some_and(|elements| {
        elements.into_iter().all(|(value, quoted)| {
            if !quoted && value.eq_ignore_ascii_case("null") {
                return true;
            }
            validate_pg_input(&value, element_type)
        })
    })
}

fn split_pg_array_elements(inner: &str) -> Option<Vec<(String, bool)>> {
    // Pre-size by counting unquoted-region commas (upper-bound). For
    // a typical array literal like `{a,b,c,…}` this skips the
    // first 2-3 doublings on `out`.
    let comma_count = inner.bytes().filter(|&b| b == b',').count();
    let mut out = Vec::with_capacity(comma_count.saturating_add(1));
    let mut current = String::new();
    let mut in_quotes = false;
    let mut escaped = false;
    let mut quoted = false;

    for ch in inner.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_quotes => escaped = true,
            '"' => {
                in_quotes = !in_quotes;
                quoted = true;
            }
            ',' if !in_quotes => {
                out.push((current.trim().to_owned(), quoted));
                current.clear();
                quoted = false;
            }
            _ => current.push(ch),
        }
    }

    if in_quotes || escaped {
        return None;
    }
    out.push((current.trim().to_owned(), quoted));
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::{with_session_context, DomainConstraint, DomainDef, EvalSessionContext};

    #[test]
    fn canonical_pg_input_type_name_normalizes_bool_aliases() {
        assert_eq!(canonical_pg_input_type_name("bool"), "boolean");
        assert_eq!(canonical_pg_input_type_name("bool[]"), "boolean[]");
        assert_eq!(canonical_pg_input_type_name("text"), "text");
    }

    #[test]
    fn domain_check_constraints_are_applied_for_pg_input_is_valid() {
        let domain = DomainDef {
            name: "positive_int".to_owned(),
            schema_name: None,
            base_type: "int4".to_owned(),
            not_null: false,
            default_expr: None,
            constraints: vec![DomainConstraint {
                name: "positive_int_check".to_owned(),
                check_expr: "VALUE > 0".to_owned(),
            }],
            char_length: None,
        };
        let ctx = EvalSessionContext::default().with_domain_defs(vec![domain]);
        with_session_context(ctx, || {
            assert!(validate_pg_input("1", "positive_int"));
            assert!(!validate_pg_input("0", "positive_int"));
        });
    }

    #[test]
    fn pg_input_error_info_reports_domain_check_violation() {
        let domain = DomainDef {
            name: "positive_int".to_owned(),
            schema_name: None,
            base_type: "int4".to_owned(),
            not_null: false,
            default_expr: None,
            constraints: vec![DomainConstraint {
                name: "positive_int_check".to_owned(),
                check_expr: "VALUE > 0".to_owned(),
            }],
            char_length: None,
        };
        let ctx = EvalSessionContext::default().with_domain_defs(vec![domain]);
        with_session_context(ctx, || {
            let args = vec![
                Value::Text("0".to_owned()),
                Value::Text("positive_int".to_owned()),
            ];
            let info =
                pg_input_error_info_record(&args, "pg_input_error_info").expect("error info call");
            let info = info.expect("must report violation");
            assert_eq!(
                info.sql_error_code.as_deref(),
                Some(SqlState::CheckViolation.code())
            );
            assert!(info
                .message
                .as_deref()
                .is_some_and(|m| m.contains("violates check constraint")));
        });
    }
}
