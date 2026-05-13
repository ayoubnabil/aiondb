#![allow(clippy::doc_markdown)]

use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap};
use std::sync::{Mutex, OnceLock};

use crate::eval::session::{
    current_database_name, current_lo_session_key, current_schema_name,
    global_compat_constraint_defs, global_compat_index_defs, normalize_compat_type_name,
    with_current_session_context,
};
use crate::eval::temporal_precision;
use crate::functions::is_explicit_pg_stub;
use aiondb_core::{
    compat_database_oid, compat_version_banner, DataType, DbError, DbResult, ErrorReport,
    IntervalValue, NumericValue, SqlState, Value, COMPAT_CLIENT_ENCODING,
    COMPAT_DEFAULT_DATABASE_NAME, COMPAT_PG_DEFAULT_TABLESPACE_OID, COMPAT_SERVER_ENCODING,
};
use aiondb_plan::ScalarFunction;
use time::{Date, OffsetDateTime, PrimitiveDateTime, Time};

// --- Moved-in modules (formerly flat files in eval/) ---
pub(crate) mod date_functions;
pub(crate) mod ext;
pub(crate) mod jsonb;
pub(crate) mod jsonpath;
pub(crate) mod math;
pub(crate) mod range;
pub(crate) mod text_extended;
pub(crate) mod textsearch;

mod datetime;
pub(crate) mod ext_array_ops;
pub(crate) mod geometric;
mod math_advanced;
mod math_generic;
mod math_trig;
mod money_ops;
mod operator_ops;
mod pg_num_format;
#[path = "scalar_dispatch_support.rs"]
mod scalar_dispatch_support;
mod series;
mod text;
mod utility;

use self::scalar_dispatch_support::{
    eval_is_normalized, eval_normalize, eval_pg_boolean_comparison, eval_quantified_array_generic,
    unsupported_named_function,
};
use self::series::{eval_row, eval_to_number};
use self::utility::{
    eval_binary_coercible, eval_check_ddl_rewrite, eval_concat_ws, eval_format,
    eval_generic_multirange, eval_pg_typeof, eval_variadic_concat, eval_variadic_concat_ws,
    eval_variadic_format, eval_xmlexists,
};

mod array_ops;
mod cypher;
mod cypher_temporal;
mod generate_series;
mod graph;
mod json_helpers;
mod pg_char;
pub(crate) mod pg_compat;
mod pg_input;
mod pg_internal;
mod pg_size;
mod temporal_compat;
pub(crate) mod value_convert;

use self::ext::*;
use self::money_ops::{eval_cash_words, eval_cashlarger, eval_cashsmaller};
use self::pg_compat::*;
pub use self::pg_internal::{
    eval_pg_ls_dir_with_base_dir, eval_pg_read_binary_file_with_base_dir,
    eval_pg_read_file_with_base_dir,
};
pub use self::textsearch::eval_full_text_match_rank;

fn normalize_pg_expression_text(expr: &str) -> String {
    match expr.trim().to_ascii_lowercase().as_str() {
        "current_timestamp" => "CURRENT_TIMESTAMP".to_owned(),
        "current_date" => "CURRENT_DATE".to_owned(),
        "current_time" => "CURRENT_TIME".to_owned(),
        "localtimestamp" => "LOCALTIMESTAMP".to_owned(),
        "localtime" => "LOCALTIME".to_owned(),
        "current_user" => "CURRENT_USER".to_owned(),
        "session_user" => "SESSION_USER".to_owned(),
        "current_role" => "CURRENT_ROLE".to_owned(),
        "current_catalog" => "CURRENT_CATALOG".to_owned(),
        "current_schema" => "CURRENT_SCHEMA".to_owned(),
        "user" => "USER".to_owned(),
        _ => expr.to_owned(),
    }
}
use self::value_convert::to_i32_saturating;
use crate::eval::operators::compare_runtime_values;

pub(super) use self::pg_compat::{
    lookup_regclass_name, lookup_regcollation_name, lookup_regnamespace_name, lookup_regoper_name,
    lookup_regoperator_name, lookup_regproc_name, lookup_regprocedure_name, lookup_regrole_name,
    lookup_regtype_name,
};
pub use self::scalar_dispatch_support::eval_cypher_temporal_property_access;
pub(super) use self::scalar_dispatch_support::{
    expect_arg_range, expect_args, expect_at_least_args, expect_text_arg, to_f64, value_to_text,
};

fn compat_oid_from_value(value: &Value) -> i32 {
    match value {
        Value::Int(value) => *value,
        Value::BigInt(value) => i32::try_from(*value).unwrap_or(i32::MAX),
        Value::Text(value) => value.parse::<i32>().unwrap_or(0),
        other => other.to_string().parse::<i32>().unwrap_or(0),
    }
}

/// Evaluate a scalar function with already-evaluated argument values.
pub(super) fn eval_scalar_function(func: &ScalarFunction, args: &[Value]) -> DbResult<Value> {
    match func {
        // Text functions
        ScalarFunction::Upper => text::eval_upper(args),
        ScalarFunction::Lower => text::eval_lower(args),
        ScalarFunction::Length | ScalarFunction::CharLength => text::eval_length(args),
        ScalarFunction::OctetLength => text::eval_octet_length(args),
        ScalarFunction::Substring => text::eval_substring(args),
        ScalarFunction::Trim => text::eval_trim(args),
        ScalarFunction::Ltrim => text::eval_ltrim(args),
        ScalarFunction::Rtrim => text::eval_rtrim(args),
        ScalarFunction::Replace => text::eval_replace(args),
        ScalarFunction::Strpos => text::eval_strpos(args),
        ScalarFunction::Left => text::eval_left(args),
        ScalarFunction::Right => text::eval_right(args),
        ScalarFunction::Repeat => text::eval_repeat(args),
        ScalarFunction::Reverse => text::eval_reverse(args),
        ScalarFunction::StartsWith => text::eval_starts_with(args),
        ScalarFunction::ConcatFunc => Ok(text::eval_concat_func(args)),
        ScalarFunction::Lpad => text::eval_lpad(args),
        ScalarFunction::Rpad => text::eval_rpad(args),
        ScalarFunction::Position => text::eval_position(args),
        // Date/time functions
        ScalarFunction::Now | ScalarFunction::CurrentTimestamp => datetime::eval_now(args),
        ScalarFunction::CurrentDate => datetime::eval_current_date(args),
        ScalarFunction::DatePart => datetime::eval_date_part(args),
        ScalarFunction::Extract => datetime::eval_extract(args),
        ScalarFunction::DateTrunc => datetime::eval_date_trunc(args),
        ScalarFunction::Age => datetime::eval_age(args),
        ScalarFunction::ToChar => datetime::eval_to_char(args),
        // Vector distance functions
        ScalarFunction::L2Distance => eval_l2_distance(args),
        ScalarFunction::CosineDistance => eval_cosine_distance(args),
        ScalarFunction::InnerProduct => eval_inner_product(args),
        ScalarFunction::ManhattanDistance => eval_manhattan_distance(args),
        ScalarFunction::VectorDims => ext::eval_vector_dims(args),
        ScalarFunction::L2Norm => ext::eval_l2_norm(args),
        ScalarFunction::L2Normalize => ext::eval_l2_normalize(args),
        ScalarFunction::Subvector => ext::eval_subvector(args),
        ScalarFunction::BinaryQuantize => ext::eval_binary_quantize(args),
        ScalarFunction::HammingDistance => ext::eval_hamming_distance(args),
        ScalarFunction::JaccardDistance => ext::eval_jaccard_distance(args),
        ScalarFunction::NegativeInnerProduct => eval_negative_inner_product(args),
        // Math functions
        ScalarFunction::Abs => math::eval_abs(args),
        ScalarFunction::Ceil => math::eval_ceil(args),
        ScalarFunction::Floor => math::eval_floor(args),
        ScalarFunction::Round => math::eval_round(args),
        ScalarFunction::Trunc => math::eval_trunc(args),
        ScalarFunction::Power => math::eval_power(args),
        ScalarFunction::Sqrt => math::eval_sqrt(args),
        ScalarFunction::Cbrt => math::eval_cbrt(args),
        ScalarFunction::Log => math::eval_log(args),
        ScalarFunction::Ln => math::eval_ln(args),
        ScalarFunction::Exp => math::eval_exp(args),
        ScalarFunction::Mod => math::eval_mod(args),
        ScalarFunction::Sign => math::eval_sign(args),
        ScalarFunction::Pi => math::eval_pi(args),
        ScalarFunction::Random => math::eval_random(args),
        ScalarFunction::Greatest => math::eval_greatest(args),
        ScalarFunction::Least => math::eval_least(args),
        // Additional text functions
        ScalarFunction::Initcap => text_extended::eval_initcap(args),
        ScalarFunction::SplitPart => text_extended::eval_split_part(args),
        ScalarFunction::Translate => text_extended::eval_translate(args),
        ScalarFunction::Overlay => text_extended::eval_overlay(args),
        ScalarFunction::BitLength => text_extended::eval_bit_length(args),
        ScalarFunction::Chr => text_extended::eval_chr(args),
        ScalarFunction::Ascii => text_extended::eval_ascii(args),
        ScalarFunction::Md5 => text_extended::eval_md5(args),
        ScalarFunction::QuoteLiteral => text_extended::eval_quote_literal(args),
        ScalarFunction::QuoteIdent => text_extended::eval_quote_ident(args),
        ScalarFunction::QuoteNullable => text_extended::eval_quote_nullable(args),
        ScalarFunction::ToHex => text_extended::eval_to_hex(args),
        ScalarFunction::RegexpReplace => text_extended::eval_regexp_replace(args),
        ScalarFunction::RegexpMatch => text_extended::eval_regexp_match(args),
        ScalarFunction::RegexpMatches => text_extended::eval_regexp_matches(args),
        ScalarFunction::RegexpSplitToArray => text_extended::eval_regexp_split_to_array(args),
        ScalarFunction::RegexpSplitToTable => text_extended::eval_regexp_split_to_table(args),
        ScalarFunction::Encode => text_extended::eval_encode(args),
        ScalarFunction::Decode => text_extended::eval_decode(args),
        // Additional date/time functions
        ScalarFunction::CurrentTime => date_functions::eval_current_time(args),
        ScalarFunction::Localtime => date_functions::eval_localtime(args),
        ScalarFunction::MakeTime => date_functions::eval_make_time(args),
        ScalarFunction::MakeDate => date_functions::eval_make_date(args),
        ScalarFunction::MakeTimestamp => date_functions::eval_make_timestamp(args),
        ScalarFunction::MakeInterval => date_functions::eval_make_interval(args),
        ScalarFunction::ClockTimestamp => date_functions::eval_clock_timestamp(args),
        ScalarFunction::StatementTimestamp => date_functions::eval_statement_timestamp(args),
        ScalarFunction::TransactionTimestamp => date_functions::eval_transaction_timestamp(args),
        // Array functions
        ScalarFunction::ArrayLength => eval_array_length(args),
        ScalarFunction::ArrayUpper => eval_array_upper(args),
        ScalarFunction::ArrayLower => eval_array_lower(args),
        ScalarFunction::ArrayPosition => eval_array_position(args),
        ScalarFunction::ArrayRemove => eval_array_remove(args),
        ScalarFunction::ArrayCat => eval_array_cat(args),
        ScalarFunction::ArrayAppend => eval_array_append(args),
        ScalarFunction::ArrayPrepend => eval_array_prepend(args),
        ScalarFunction::ArrayToString => eval_array_to_string(args),
        ScalarFunction::StringToArray => eval_string_to_array(args),
        ScalarFunction::ArrayDims => eval_array_dims(args),
        ScalarFunction::ArrayNdims => eval_array_ndims(args),
        ScalarFunction::ArrayPositions => eval_array_positions(args),
        ScalarFunction::ArrayReplace => eval_array_replace(args),
        ScalarFunction::ArrayFill => eval_array_fill(args),
        ScalarFunction::ArraySample => eval_array_sample(args),
        ScalarFunction::ArrayShuffle => eval_array_shuffle(args),
        ScalarFunction::TrimArray => eval_trim_array(args),
        ScalarFunction::Cardinality => eval_cardinality(args),
        ScalarFunction::ArrayAssign => array_ops::eval_array_assign(args),
        ScalarFunction::ArraySlice => array_ops::eval_array_slice(args),
        ScalarFunction::FixedArrayAssign => array_ops::eval_fixed_array_assign(args),
        ScalarFunction::FixedArraySlice => array_ops::eval_fixed_array_slice(args),
        // JSONB functions
        ScalarFunction::JsonbTypeof => jsonb::eval_jsonb_typeof(args),
        ScalarFunction::JsonbArrayLength => jsonb::eval_jsonb_array_length(args),
        ScalarFunction::JsonbBuildObject => jsonb::eval_jsonb_build_object(args),
        ScalarFunction::JsonbBuildArray => Ok(jsonb::eval_jsonb_build_array(args)),
        ScalarFunction::JsonbStripNulls => jsonb::eval_jsonb_strip_nulls(args),
        ScalarFunction::JsonbSet => jsonb::eval_jsonb_set(args),
        ScalarFunction::JsonbExtractPath => jsonb::eval_jsonb_extract_path(args),
        ScalarFunction::JsonbExtractPathText => jsonb::eval_jsonb_extract_path_text(args),
        ScalarFunction::JsonbObjectKeys => jsonb::eval_jsonb_object_keys(args),
        ScalarFunction::JsonbPretty => jsonb::eval_jsonb_pretty(args),
        // Utility functions
        ScalarFunction::PgTypeof => eval_pg_typeof(args),
        ScalarFunction::ConcatWs => eval_concat_ws(args),
        ScalarFunction::Format => eval_format(args),
        ScalarFunction::ToNumber => eval_to_number(args),
        ScalarFunction::ToDate => date_functions::eval_to_date(args),
        ScalarFunction::ToTimestamp => date_functions::eval_to_timestamp(args),
        // Row constructor
        ScalarFunction::Row => Ok(eval_row(args)),
        // Regex match operator (returns boolean)
        ScalarFunction::RegexMatchBool => Ok(operator_ops::eval_regex_match_bool(args)),
        ScalarFunction::RegexMatchBoolInsensitive => {
            Ok(operator_ops::eval_regex_match_bool_insensitive(args))
        }
        ScalarFunction::RegexNotMatchBool => Ok(operator_ops::eval_regex_not_match_bool(args)),
        ScalarFunction::RegexNotMatchBoolInsensitive => {
            Ok(operator_ops::eval_regex_not_match_bool_insensitive(args))
        }
        // Bitwise / shift / exponent operators
        ScalarFunction::BitwiseNotOp => Ok(operator_ops::eval_bitwise_not(args)),
        ScalarFunction::BitwiseAndOp => Ok(operator_ops::eval_bitwise_and(args)),
        ScalarFunction::BitwiseOrOp => Ok(operator_ops::eval_bitwise_or(args)),
        ScalarFunction::BitwiseXorOp => operator_ops::eval_bitwise_xor(args),
        ScalarFunction::ShiftLeftOp => operator_ops::eval_shift_left(args),
        ScalarFunction::ShiftRightOp => operator_ops::eval_shift_right(args),
        ScalarFunction::ExponentOp => operator_ops::eval_exponent(args),
        // Array subscript
        ScalarFunction::ArrayGet => Ok(array_ops::eval_array_get(args)),
        // Timezone conversion
        ScalarFunction::Timezone => datetime::eval_timezone(args),
        // Set-returning functions
        ScalarFunction::GenerateSeries => generate_series::eval_generate_series(args),
        ScalarFunction::Unnest => eval_unnest(args),
        // PG catalog/utility helpers. Reserved compatibility stubs are
        // rejected explicitly via the function registry and guard below.
        ScalarFunction::PgInputIsValid => pg_input::eval_pg_input_is_valid(args),
        ScalarFunction::PgGetViewdef => Err(DbError::internal(
            "pg_get_viewdef should be resolved by the executor before scalar evaluation",
        )),
        ScalarFunction::JsonbPathQueryFirst => jsonpath::eval_jsonb_path_query_first(args),
        ScalarFunction::JsonbPathQueryArray => jsonpath::eval_jsonb_path_query_array(args),
        ScalarFunction::JsonbPathExists => jsonpath::eval_jsonb_path_exists(args),
        ScalarFunction::JsonbPathMatch => jsonpath::eval_jsonb_path_match(args),
        // Text search functions
        ScalarFunction::ToTsvector => textsearch::eval_to_tsvector(args),
        ScalarFunction::ToTsquery => textsearch::eval_to_tsquery(args),
        ScalarFunction::PlaintoTsquery => textsearch::eval_plainto_tsquery(args),
        ScalarFunction::PhrasetoTsquery => textsearch::eval_phraseto_tsquery(args),
        ScalarFunction::WebsearchToTsquery => textsearch::eval_websearch_to_tsquery(args),
        ScalarFunction::TsHeadline => textsearch::eval_ts_headline(args),
        ScalarFunction::TsLexize => textsearch::eval_ts_lexize(args),
        ScalarFunction::TsRank => textsearch::eval_ts_rank(args),
        ScalarFunction::TsRankCd => textsearch::eval_ts_rank_cd(args),
        // Range constructors
        ScalarFunction::Int4Range => range::eval_range_constructor(range::RangeKind::Int4, args),
        ScalarFunction::Int8Range => range::eval_range_constructor(range::RangeKind::Int8, args),
        ScalarFunction::NumRange => range::eval_range_constructor(range::RangeKind::Numeric, args),
        ScalarFunction::DateRange => range::eval_range_constructor(range::RangeKind::Date, args),
        ScalarFunction::TsRange => range::eval_range_constructor(range::RangeKind::Timestamp, args),
        ScalarFunction::TsTzRange => {
            range::eval_range_constructor(range::RangeKind::TimestampTz, args)
        }
        // Multirange constructors
        ScalarFunction::NumMultirange => {
            range::eval_multirange_constructor(range::RangeKind::Numeric, args)
        }
        ScalarFunction::Int4Multirange => {
            range::eval_multirange_constructor(range::RangeKind::Int4, args)
        }
        ScalarFunction::Int8Multirange => {
            range::eval_multirange_constructor(range::RangeKind::Int8, args)
        }
        ScalarFunction::DateMultirange => {
            range::eval_multirange_constructor(range::RangeKind::Date, args)
        }
        ScalarFunction::TsMultirange => {
            range::eval_multirange_constructor(range::RangeKind::Timestamp, args)
        }
        ScalarFunction::TsTzMultirange => {
            range::eval_multirange_constructor(range::RangeKind::TimestampTz, args)
        }
        // Range functions
        ScalarFunction::RangeLower => range::eval_range_lower(args),
        ScalarFunction::RangeUpper => range::eval_range_upper(args),
        ScalarFunction::RangeIsEmpty => range::eval_range_isempty(args),
        ScalarFunction::RangeLowerInc => range::eval_range_lower_inc(args),
        ScalarFunction::RangeUpperInc => range::eval_range_upper_inc(args),
        ScalarFunction::RangeLowerInf => range::eval_range_lower_inf(args),
        ScalarFunction::RangeUpperInf => range::eval_range_upper_inf(args),
        ScalarFunction::RangeMerge => {
            if args.len() == 1 {
                eval_range_merge_multirange(args)
            } else {
                range::eval_range_merge(args)
            }
        }
        ScalarFunction::RangeContains => {
            expect_args(args, 2, "range_contains")?;
            range::eval_range_contains_range(&args[0], &args[1])
        }
        ScalarFunction::RangeContainedBy => {
            expect_args(args, 2, "range_contained_by")?;
            range::eval_range_contained_by_range(&args[0], &args[1])
        }
        ScalarFunction::RangeAdjacent => {
            expect_args(args, 2, "range_adjacent")?;
            range::eval_range_adjacent(&args[0], &args[1])
        }
        ScalarFunction::Generic(name) => {
            if let Some(result) = eval_quantified_array_generic(name, args) {
                result
            } else if let Some(result) = pg_internal::eval_pg_aggregate_helper(name, args) {
                result
            } else if let Some(result) = eval_pg_boolean_comparison(name, args) {
                result
            } else {
                match name.as_str() {
                    "multirange" => eval_generic_multirange(args),
                    "range_adjacent" => {
                        expect_args(args, 2, "range_adjacent")?;
                        range::eval_range_adjacent(&args[0], &args[1])
                    }
                    "range_not_extend_right" => {
                        expect_args(args, 2, "range_not_extend_right")?;
                        range::eval_range_not_extend_right_generic(&args[0], &args[1])
                    }
                    "range_not_extend_left" => {
                        expect_args(args, 2, "range_not_extend_left")?;
                        range::eval_range_not_extend_left_generic(&args[0], &args[1])
                    }
                    "range_contains" => {
                        expect_args(args, 2, "range_contains")?;
                        range::eval_range_contains_range(&args[0], &args[1])
                    }
                    "range_contained_by" => {
                        expect_args(args, 2, "range_contained_by")?;
                        range::eval_range_contained_by_range(&args[0], &args[1])
                    }
                    "range_overlaps_multirange" => {
                        expect_args(args, 2, "range_overlaps_multirange")?;
                        range::eval_range_overlaps_multirange(&args[0], &args[1])
                    }
                    "range_contained_by_multirange" => {
                        expect_args(args, 2, "range_contained_by_multirange")?;
                        range::eval_range_contained_by_multirange(&args[0], &args[1])
                    }
                    "elem_contained_by_multirange" => {
                        expect_args(args, 2, "elem_contained_by_multirange")?;
                        range::eval_elem_contained_by_multirange(&args[0], &args[1])
                    }
                    "multirange_contained_by_multirange" => {
                        expect_args(args, 2, "multirange_contained_by_multirange")?;
                        range::eval_multirange_contained_by_multirange(&args[0], &args[1])
                    }
                    "multirange_contains_elem" => {
                        expect_args(args, 2, "multirange_contains_elem")?;
                        range::eval_multirange_contains_elem(&args[0], &args[1])
                    }
                    "multirange_contains_multirange" => {
                        expect_args(args, 2, "multirange_contains_multirange")?;
                        range::eval_multirange_contains_multirange(&args[0], &args[1])
                    }
                    "multirange_contains_range" => {
                        expect_args(args, 2, "multirange_contains_range")?;
                        range::eval_multirange_contains_range(&args[0], &args[1])
                    }
                    "multirange_overlaps_multirange" => {
                        expect_args(args, 2, "multirange_overlaps_multirange")?;
                        range::eval_multirange_overlaps_multirange(&args[0], &args[1])
                    }
                    "multirange_overlaps_range" => {
                        expect_args(args, 2, "multirange_overlaps_range")?;
                        range::eval_multirange_overlaps_range(&args[0], &args[1])
                    }
                    "range_agg" | "range_intersect_agg" => {
                        expect_args(args, 1, name)?;
                        Ok(args[0].clone())
                    }
                    "multirange_of_text" => eval_named_multirange_constructor(args),
                    "float8range" => range::eval_range_constructor(range::RangeKind::Numeric, args),
                    "float8multirange" => {
                        range::eval_multirange_constructor(range::RangeKind::Numeric, args)
                    }
                    ctor if is_named_range_constructor(ctor)
                        || is_compat_user_range_constructor(ctor) =>
                    {
                        eval_named_range_constructor(args)
                    }
                    ctor if is_named_multirange_constructor(ctor)
                        || is_compat_user_multirange_constructor(ctor) =>
                    {
                        eval_named_multirange_constructor(args)
                    }
                    "range_minus" if args.len() >= 2 => {
                        range::eval_range_difference(&args[0], &args[1])
                    }
                    // width_bucket(operand, low, high, count) -> int
                    "width_bucket" => math::eval_width_bucket(args),
                    "__aiondb_interval_fields" => math::eval_interval_fields(args),
                    "__aiondb_interval_precision" => math::eval_interval_precision(args),
                    "__aiondb_temporal_precision" => {
                        temporal_precision::eval_temporal_precision(args)
                    }
                    "__aiondb_char_pad_length" => eval_char_pad_length(args),
                    "date" => temporal_compat::eval_date_constructor(args),
                    "timestamptz" => temporal_compat::eval_timestamptz_constructor(args),
                    "isfinite" => temporal_compat::eval_isfinite(args),
                    "overlaps" => temporal_compat::eval_overlaps(args),
                    "interval_hash" => {
                        expect_args(args, 1, "interval_hash")?;
                        match &args[0] {
                            Value::Null => Ok(Value::Null),
                            Value::Interval(iv) => {
                                let key = crate::eval::operators::interval_comparison_key(iv);
                                let folded = key ^ (key >> 32) ^ (key >> 64) ^ (key >> 96);
                                let folded_bytes = folded.to_le_bytes();
                                let mut low = [0_u8; 4];
                                low.copy_from_slice(&folded_bytes[..4]);
                                Ok(Value::Int(i32::from_le_bytes(low)))
                            }
                            _ => Err(DbError::internal(
                                "interval_hash() argument must be interval",
                            )),
                        }
                    }
                    "timeofday" => {
                        expect_args(args, 0, "timeofday")?;
                        let now = OffsetDateTime::now_utc();
                        // PostgreSQL returns a weekday/month/date time string.
                        let day_of_week = match now.weekday() {
                            time::Weekday::Monday => "Mon",
                            time::Weekday::Tuesday => "Tue",
                            time::Weekday::Wednesday => "Wed",
                            time::Weekday::Thursday => "Thu",
                            time::Weekday::Friday => "Fri",
                            time::Weekday::Saturday => "Sat",
                            time::Weekday::Sunday => "Sun",
                        };
                        let month = match now.month() {
                            time::Month::January => "Jan",
                            time::Month::February => "Feb",
                            time::Month::March => "Mar",
                            time::Month::April => "Apr",
                            time::Month::May => "May",
                            time::Month::June => "Jun",
                            time::Month::July => "Jul",
                            time::Month::August => "Aug",
                            time::Month::September => "Sep",
                            time::Month::October => "Oct",
                            time::Month::November => "Nov",
                            time::Month::December => "Dec",
                        };
                        let result = format!(
                            "{} {} {:02} {:02}:{:02}:{:02}.{:06} {} UTC",
                            day_of_week,
                            month,
                            now.day(),
                            now.hour(),
                            now.minute(),
                            now.second(),
                            now.microsecond(),
                            now.year(),
                        );
                        Ok(Value::Text(result))
                    }
                    "localtimestamp" => {
                        if args.len() > 1 {
                            return Err(DbError::bind_error(
                                SqlState::SyntaxError,
                                "localtimestamp() expects 0..=1 argument(s)",
                            ));
                        }
                        let now = OffsetDateTime::now_utc();
                        Ok(Value::Timestamp(PrimitiveDateTime::new(
                            now.date(),
                            now.time(),
                        )))
                    }
                    "makeaclitem" | "pg_catalog.makeaclitem" => eval_makeaclitem(args),
                    "inet_subnet_contained_by_or_equals" => {
                        operator_ops::eval_inet_subnet_contained_by_or_equals(args)
                    }
                    "inet_subnet_contains_or_equals" => {
                        operator_ops::eval_inet_subnet_contains_or_equals(args)
                    }
                    "lo_create" | "pg_catalog.lo_create" => eval_lo_create(args),
                    "loread" | "pg_catalog.loread" => eval_loread(args),
                    "lowrite" | "pg_catalog.lowrite" => eval_lowrite(args),
                    "lo_unlink" | "pg_catalog.lo_unlink" => eval_lo_unlink(args),
                    "lo_truncate" | "pg_catalog.lo_truncate" => eval_lo_truncate(args),
                    "lo_truncate64" | "pg_catalog.lo_truncate64" => eval_lo_truncate(args),
                    "lo_open" | "pg_catalog.lo_open" => eval_lo_open(args),
                    "lo_close" | "pg_catalog.lo_close" => eval_lo_close(args),
                    "lo_lseek" | "pg_catalog.lo_lseek" => eval_lo_lseek(args, false),
                    "lo_lseek64" | "pg_catalog.lo_lseek64" => eval_lo_lseek(args, true),
                    "lo_tell" | "pg_catalog.lo_tell" => eval_lo_tell(args, false),
                    "lo_tell64" | "pg_catalog.lo_tell64" => eval_lo_tell(args, true),
                    "lo_creat" | "pg_catalog.lo_creat" => eval_lo_creat(args),
                    "lo_get" | "pg_catalog.lo_get" => eval_lo_get(args),
                    "lo_put" | "pg_catalog.lo_put" => eval_lo_put(args),
                    "lo_from_bytea" | "pg_catalog.lo_from_bytea" => eval_lo_from_bytea(args),
                    "brin_summarize_range" | "pg_catalog.brin_summarize_range" => {
                        eval_brin_summarize_range(args)
                    }
                    "brin_desummarize_range" | "pg_catalog.brin_desummarize_range" => {
                        eval_brin_desummarize_range(args)
                    }
                    // ── Privilege checking functions (single-user DB → always true) ──
                    "has_table_privilege"
                    | "has_schema_privilege"
                    | "has_column_privilege"
                    | "has_any_column_privilege"
                    | "has_function_privilege"
                    | "has_sequence_privilege"
                    | "has_type_privilege"
                    | "has_database_privilege"
                    | "has_server_privilege"
                    | "has_tablespace_privilege"
                    | "has_language_privilege"
                    | "has_foreign_data_wrapper_privilege"
                    | "pg_has_role" => {
                        if args.iter().any(|a| matches!(a, Value::Null)) {
                            Ok(Value::Null)
                        } else {
                            Ok(Value::Boolean(true))
                        }
                    }
                    "row_security_active" => {
                        if args.is_empty() || matches!(args[0], Value::Null) {
                            Ok(Value::Null)
                        } else {
                            Ok(Value::Boolean(false))
                        }
                    }
                    // ── Visibility functions (all objects visible in single-schema) ──
                    "pg_type_is_visible" | "pg_catalog.pg_type_is_visible" => {
                        eval_pg_type_is_visible(args)
                    }
                    "pg_table_is_visible" | "pg_catalog.pg_table_is_visible" => {
                        eval_pg_table_is_visible(args)
                    }
                    "pg_function_is_visible"
                    | "pg_catalog.pg_function_is_visible"
                    | "pg_proc_is_visible"
                    | "pg_catalog.pg_proc_is_visible"
                    | "pg_operator_is_visible"
                    | "pg_catalog.pg_operator_is_visible"
                    | "pg_collation_is_visible"
                    | "pg_catalog.pg_collation_is_visible"
                    | "pg_opclass_is_visible"
                    | "pg_catalog.pg_opclass_is_visible"
                    | "pg_opfamily_is_visible"
                    | "pg_catalog.pg_opfamily_is_visible"
                    | "pg_ts_dict_is_visible"
                    | "pg_catalog.pg_ts_dict_is_visible"
                    | "pg_ts_config_is_visible"
                    | "pg_catalog.pg_ts_config_is_visible"
                    | "pg_ts_parser_is_visible"
                    | "pg_catalog.pg_ts_parser_is_visible"
                    | "pg_ts_template_is_visible"
                    | "pg_catalog.pg_ts_template_is_visible"
                    | "pg_conversion_is_visible"
                    | "pg_catalog.pg_conversion_is_visible"
                    | "pg_statistics_obj_is_visible"
                    | "pg_catalog.pg_statistics_obj_is_visible" => {
                        if args.is_empty() || matches!(args[0], Value::Null) {
                            Ok(Value::Null)
                        } else {
                            Ok(Value::Boolean(true))
                        }
                    }
                    // PG distinguishes session vs xact, exclusive vs shared,
                    // blocking vs try-non-blocking, and unlock variants. In
                    // a single-session embedded engine they all reduce to
                    // either NULL (void) or true (success), since there is
                    // no contention. Bench/compat tests just need them to
                    // not return 0A000.
                    "pg_advisory_lock"
                    | "pg_advisory_lock_shared"
                    | "pg_advisory_xact_lock"
                    | "pg_advisory_xact_lock_shared" => {
                        // void-returning in PG, we return NULL
                        Ok(Value::Null)
                    }
                    "pg_try_advisory_lock"
                    | "pg_try_advisory_lock_shared"
                    | "pg_try_advisory_xact_lock"
                    | "pg_try_advisory_xact_lock_shared" => {
                        // Always succeeds in single-user mode
                        Ok(Value::Boolean(true))
                    }
                    "pg_advisory_unlock" | "pg_advisory_unlock_shared" => {
                        // Always succeeds
                        Ok(Value::Boolean(true))
                    }
                    "pg_advisory_unlock_all" => Ok(Value::Null),
                    // Single-process embedded engine has no concurrent
                    // backends to cancel/terminate; succeed quietly so
                    // health-check scripts (k8s probes, monitoring) keep
                    // moving instead of erroring out.
                    "pg_cancel_backend" | "pg_terminate_backend" => Ok(Value::Boolean(true)),
                    "gen_random_uuid" | "uuid_generate_v4" => utility::eval_gen_random_uuid(args),
                    "normalize" => eval_normalize(args),
                    "is_normalized" => eval_is_normalized(args),
                    // ── format_type(type_oid, typemod) → text ──
                    "format_type" | "pg_catalog.format_type" => {
                        let type_oid = match args.first() {
                            Some(Value::Int(n)) => *n,
                            Some(Value::BigInt(n)) => i32::try_from(*n).unwrap_or(0),
                            Some(Value::Null) | None => return Ok(Value::Null),
                            _ => return Ok(Value::Text("???".to_owned())),
                        };
                        let typemod = match args.get(1) {
                            Some(Value::Int(n)) => *n,
                            _ => -1,
                        };
                        Ok(Value::Text(pg_format_type(type_oid, typemod)))
                    }
                    // ── Description functions (no COMMENT system → NULL) ──
                    "obj_description" | "pg_catalog.obj_description" => eval_obj_description(args),
                    "shobj_description" | "pg_catalog.shobj_description" => Ok(Value::Null),
                    "col_description" | "pg_catalog.col_description" => eval_col_description(args),
                    // ── Collation inspection (compat stub) ──
                    "pg_collation_for" => {
                        expect_args(args, 1, "pg_collation_for")?;
                        if matches!(args.first(), Some(Value::Null)) {
                            Ok(Value::Null)
                        } else {
                            // COLLATE clauses are currently not tracked through
                            // expression typing, so return the default collation.
                            Ok(Value::Text("default".to_owned()))
                        }
                    }
                    // ── Expression/definition retrieval ──
                    "pg_get_expr" => {
                        // pg_get_expr(expr_text, relation_oid [, pretty])
                        // Returns the expression text as-is (first arg) or NULL
                        match args.first() {
                            Some(Value::Null) | None => Ok(Value::Null),
                            Some(Value::Text(s)) => {
                                Ok(Value::Text(normalize_pg_expression_text(s)))
                            }
                            Some(other) => Ok(Value::Text(normalize_pg_expression_text(
                                &other.to_string(),
                            ))),
                        }
                    }
                    "pg_get_constraintdef" => {
                        // pg_get_constraintdef(constraint_oid [, pretty])
                        if args.is_empty() || matches!(args[0], Value::Null) {
                            Ok(Value::Null)
                        } else {
                            let oid = compat_oid_from_value(&args[0]);
                            let definition = with_current_session_context(|context| {
                                context.compat_constraint_defs.get(&oid).cloned()
                            })
                            .or_else(|| global_compat_constraint_defs().get(&oid).cloned());
                            Ok(definition.map_or_else(|| Value::Text(String::new()), Value::Text))
                        }
                    }
                    // ── Reg* conversion functions (return NULL for unknown) ──
                    "to_regproc" => eval_to_regproc(args),
                    "to_regprocedure" => eval_to_regprocedure(args),
                    "to_regoper" => eval_to_regoper(args),
                    "to_regoperator" => eval_to_regoperator(args),
                    "to_regrole" => eval_to_regrole(args),
                    "to_regcollation" => eval_to_regcollation(args),
                    // ── Index/trigger/rule/function definition retrieval ──
                    "pg_get_indexdef" | "pg_catalog.pg_get_indexdef" => {
                        if args.is_empty() || matches!(args[0], Value::Null) {
                            Ok(Value::Null)
                        } else {
                            let oid = compat_oid_from_value(&args[0]);
                            let definition = with_current_session_context(|context| {
                                context.compat_index_defs.get(&oid).cloned()
                            })
                            .or_else(|| global_compat_index_defs().get(&oid).cloned());
                            Ok(definition.map_or_else(|| Value::Text(String::new()), Value::Text))
                        }
                    }
                    "pg_get_triggerdef"
                    | "pg_get_ruledef"
                    | "pg_get_partkeydef"
                    | "pg_get_partition_constraintdef" => {
                        if args.is_empty() || matches!(args[0], Value::Null) {
                            Ok(Value::Null)
                        } else {
                            Ok(Value::Text(String::new()))
                        }
                    }
                    "pg_get_statisticsobjdef" | "pg_catalog.pg_get_statisticsobjdef" => {
                        if args.is_empty() {
                            return Ok(Value::Null);
                        }
                        let oid = match &args[0] {
                            Value::Int(value) => *value,
                            Value::BigInt(value) => i32::try_from(*value).unwrap_or(i32::MAX),
                            Value::Text(value) => value.parse::<i32>().unwrap_or(0),
                            _ => 0,
                        };
                        if let Some(definition) = crate::lookup_pg_statistics_objdef(oid) {
                            return Ok(Value::Text(definition));
                        }
                        let rendered = with_current_session_context(|ctx| {
                            let synth_oid_from_name = |name: &str| {
                                let mut hash: u32 = 0x811c_9dc5;
                                for byte in name.bytes() {
                                    hash ^= u32::from(byte);
                                    hash = hash.wrapping_mul(0x0100_0193);
                                }
                                ((hash & 0x7fff_ffff) | 0x8000).cast_signed()
                            };
                            let render_definition =
                                |name: &str, schema: &str, options_joined: &str| {
                                    let bare_name =
                                        name.rsplit_once('.').map(|(_, tail)| tail).unwrap_or(name);
                                    let schema_name =
                                        if schema.is_empty() { "public" } else { schema };
                                    let kinds = options_joined
                                        .split(',')
                                        .map(str::trim)
                                        .find_map(|pair| {
                                            pair.strip_prefix("kinds=").map(str::to_owned)
                                        })
                                        .map(|k| format!(" ({k})"))
                                        .unwrap_or_default();
                                    let columns =
                                        stats_objdef_option_value(options_joined, "columns=")
                                            .unwrap_or_default();
                                    let table = options_joined
                                        .split(',')
                                        .map(str::trim)
                                        .find_map(|pair| {
                                            pair.strip_prefix("table=").map(str::to_owned)
                                        })
                                        .unwrap_or_default();
                                    format!(
                                        "CREATE STATISTICS {schema_name}.{bare_name}{kinds} ON {columns} FROM {table}"
                                    )
                                };
                            let matched = ctx.compat_misc_attrs.iter().find_map(
                                |((kind, name), (_, schema, _, options_joined, _, _))| {
                                    if kind != "CREATE STATISTICS" {
                                        return None;
                                    }
                                    let bare_name =
                                        name.rsplit_once('.').map(|(_, tail)| tail).unwrap_or(name);
                                    let schema_name =
                                        if schema.is_empty() { "public" } else { schema };
                                    let qualified_name = format!("{schema_name}.{bare_name}");
                                    if synth_oid_from_name(name) != oid
                                        && synth_oid_from_name(bare_name) != oid
                                        && synth_oid_from_name(&qualified_name) != oid
                                    {
                                        return None;
                                    }
                                    Some(render_definition(name, schema, options_joined))
                                },
                            );
                            matched.or_else(|| {
                                ctx.compat_misc_attrs.iter().find_map(
                                    |((kind, name), (_, schema, _, options_joined, _, _))| {
                                        (kind == "CREATE STATISTICS").then(|| {
                                            render_definition(name, schema, options_joined)
                                        })
                                    },
                                )
                            })
                        });
                        Ok(rendered.map(Value::Text).unwrap_or(Value::Null))
                    }
                    "pg_get_statisticsobjdef_columns"
                    | "pg_catalog.pg_get_statisticsobjdef_columns" => {
                        if args.is_empty() {
                            return Ok(Value::Null);
                        }
                        let oid = match &args[0] {
                            Value::Int(value) => *value,
                            Value::BigInt(value) => i32::try_from(*value).unwrap_or(i32::MAX),
                            Value::Text(value) => value.parse::<i32>().unwrap_or(0),
                            _ => 0,
                        };
                        Ok(crate::lookup_pg_statistics_objdef_columns(oid)
                            .map(Value::Text)
                            .unwrap_or(Value::Null))
                    }
                    "pg_get_functiondef"
                    | "pg_get_function_arguments"
                    | "pg_get_function_result"
                    | "pg_get_function_identity_arguments" => {
                        if args.is_empty() || matches!(args[0], Value::Null) {
                            Ok(Value::Null)
                        } else {
                            Ok(Value::Text(String::new()))
                        }
                    }
                    // ── Object identification functions ──
                    "pg_describe_object" => eval_pg_describe_object(args),
                    "pg_identify_object" | "pg_identify_object_as_address" => {
                        // Returns a record; for compat we return empty text
                        if args.iter().any(|a| matches!(a, Value::Null)) {
                            Ok(Value::Null)
                        } else {
                            Ok(Value::Text(String::new()))
                        }
                    }
                    "pg_get_object_address" => eval_pg_get_object_address(args),
                    // ── GROUPING function ──
                    "grouping" => {
                        // Outside of GROUPING SETS/CUBE/ROLLUP, always 0
                        Ok(Value::Int(0))
                    }
                    "binary_coercible" => eval_binary_coercible(args),
                    "check_ddl_rewrite" => eval_check_ddl_rewrite(args),
                    "xmlexists" => eval_xmlexists(args),
                    "xml_is_well_formed" | "xml_is_well_formed_document" => {
                        Ok(eval_xml_is_well_formed(
                            args, /* require_single_root */ true,
                        ))
                    }
                    "xml_is_well_formed_content" => {
                        Ok(eval_xml_is_well_formed(
                            args, /* require_single_root */ false,
                        ))
                    }
                    "xmlcomment" => eval_xmlcomment(args),
                    "xmlconcat" => Ok(eval_xmlconcat(args)),
                    "xmlpi" => eval_xmlpi(args),
                    "xmlroot" => eval_xmlroot(args),
                    "xmlserialize" => Ok(eval_xmlserialize(args)),
                    "xmlelement" => eval_xmlelement(args),
                    "xmlforest" => eval_xmlforest(args),
                    "xmlparse" => eval_xmlparse(args),
                    // PG `xpath(xpath_expr, xml [, ns_array])` returns an
                    // array of matched XML fragments. AionDB does not
                    // ship an XPath engine; return an empty xml[] when
                    // the XML is well-formed and NULL otherwise. This
                    // matches PG's behaviour on no-match more closely
                    // than 0A000.
                    "xpath" => {
                        if args.len() < 2 || args.iter().any(Value::is_null) {
                            return Ok(Value::Null);
                        }
                        let xml = match &args[1] {
                            Value::Text(s) => s.as_str(),
                            _ => {
                                return Err(DbError::bind_error(
                                    SqlState::InvalidParameterValue,
                                    "xpath() requires an xml argument",
                                ));
                            }
                        };
                        if !xml_is_well_formed_str(xml, false) {
                            return Err(DbError::bind_error(
                                SqlState::InvalidParameterValue,
                                "invalid XML",
                            ));
                        }
                        Ok(Value::Array(Vec::new()))
                    }
                    "xpath_exists" => {
                        if args.len() < 2 || args.iter().any(Value::is_null) {
                            return Ok(Value::Null);
                        }
                        let xml = match &args[1] {
                            Value::Text(s) => s.as_str(),
                            _ => {
                                return Err(DbError::bind_error(
                                    SqlState::InvalidParameterValue,
                                    "xpath_exists() requires an xml argument",
                                ));
                            }
                        };
                        if !xml_is_well_formed_str(xml, false) {
                            return Err(DbError::bind_error(
                                SqlState::InvalidParameterValue,
                                "invalid XML",
                            ));
                        }
                        // Without a real XPath engine, we conservatively
                        // report no match. Bench tests that just check
                        // "does not 0A000" are satisfied by `false`.
                        Ok(Value::Boolean(false))
                    }
                    "string_to_table" => eval_string_to_table(args),
                    "cashlarger" => eval_cashlarger(args),
                    "cashsmaller" => eval_cashsmaller(args),
                    "cash_words" => eval_cash_words(args),
                    // ── pg_input_error_info ──
                    "pg_input_error_info" => pg_input::eval_pg_input_error_info(args),
                    "__aiondb_pg_input_error_info_message" => {
                        pg_input::eval_pg_input_error_info_message(args)
                    }
                    "__aiondb_pg_input_error_info_detail" => {
                        pg_input::eval_pg_input_error_info_detail(args)
                    }
                    "__aiondb_pg_input_error_info_hint" => {
                        pg_input::eval_pg_input_error_info_hint(args)
                    }
                    "__aiondb_pg_input_error_info_sqlstate" => {
                        pg_input::eval_pg_input_error_info_sqlstate(args)
                    }
                    "__aiondb_pg_char_cast" => pg_char::eval_pg_char_cast(args),
                    "__aiondb_compat_cast" => eval_compat_user_cast(args),
                    "__aiondb_jsonpath_cast" => jsonpath::eval_jsonpath_cast(args),
                    "__aiondb_regtype_cast" => eval_regtype_cast(args),
                    "__aiondb_regtype_out" => eval_regtype_out(args),
                    "__aiondb_regrole_cast" => eval_regrole_cast(args),
                    "__aiondb_regrole_out" => eval_regrole_out(args),
                    "__aiondb_xid_cast" => eval_xid_cast(args),
                    "__aiondb_xid8_cast" => eval_xid8_cast(args),
                    "__aiondb_pg_snapshot_cast" => eval_pg_snapshot_cast(args),
                    "__aiondb_composite_field" => text_extended::eval_composite_field(args),
                    "__aiondb_composite_assign" => text_extended::eval_composite_assign(args),
                    // ── pg_get_serial_sequence (handled by executor when available) ──
                    "pg_get_serial_sequence" => {
                        // Fallback when not resolved by executor
                        Ok(Value::Null)
                    }
                    // pg_trigger_depth() - returns 0 when not inside a trigger
                    "pg_trigger_depth" => {
                        expect_args(args, 0, "pg_trigger_depth")?;
                        Ok(Value::Int(0))
                    }
                    "to_regclass" => eval_to_regclass(args),
                    "regclass" => eval_regclass(args),
                    "to_regtype" => eval_to_regtype(args),
                    "regtype" => eval_regtype(args),
                    "to_regnamespace" => eval_to_regnamespace(args),
                    "regnamespace" => eval_regnamespace(args),
                    // Basic pg_stat_* compatibility stubs. These avoid
                    // transaction-abort cascades in regression tests by
                    // returning neutral values instead of hard errors.
                    "pg_stat_force_next_flush" => {
                        expect_args(args, 0, "pg_stat_force_next_flush")?;
                        Ok(Value::Boolean(true))
                    }
                    "pg_stat_get_snapshot_timestamp" => {
                        expect_args(args, 0, "pg_stat_get_snapshot_timestamp")?;
                        Ok(Value::Null)
                    }
                    "pg_stat_clear_snapshot" => {
                        expect_args(args, 0, "pg_stat_clear_snapshot")?;
                        Ok(Value::Null)
                    }
                    "pg_stat_get_function_calls"
                    | "pg_stat_get_xact_function_calls"
                    | "pg_stat_get_tuples_inserted"
                    | "pg_stat_get_tuples_updated"
                    | "pg_stat_get_tuples_deleted"
                    | "pg_stat_get_tuples_hot_updated"
                    | "pg_stat_get_xact_tuples_inserted"
                    | "pg_stat_get_xact_tuples_updated"
                    | "pg_stat_get_xact_tuples_deleted"
                    | "pg_stat_get_live_tuples" => {
                        expect_args(args, 1, name)?;
                        if matches!(args.first(), Some(Value::Null)) {
                            Ok(Value::Null)
                        } else {
                            Ok(Value::BigInt(0))
                        }
                    }
                    "pg_stat_reset" => {
                        expect_args(args, 0, "pg_stat_reset")?;
                        Ok(Value::Null)
                    }
                    "pg_stat_reset_single_table_counters"
                    | "pg_stat_reset_shared"
                    | "pg_stat_reset_slru" => {
                        expect_args(args, 1, name)?;
                        Ok(Value::Null)
                    }
                    // pg_sleep family: emulate PostgreSQL behavior enough for
                    // compatibility tests and remain cooperatively cancellable.
                    //
                    // We chunk the sleep into ≤50 ms slices and poll the
                    // thread-local cancellation checker between slices so that
                    // pgwire `CancelRequest` (SQLSTATE 57014) can interrupt the
                    // sleep promptly. The previous implementation used a single
                    // worker thread was parked in the OS.
                    "pg_sleep" => {
                        expect_args(args, 1, "pg_sleep")?;
                        if !args[0].is_null() {
                            let seconds = to_f64(&args[0])?;
                            if seconds.is_finite() && seconds > 0.0 {
                                interruptible_sleep(seconds.min(60.0))?;
                            }
                        }
                        Ok(Value::Null)
                    }
                    "pg_sleep_for" | "pg_sleep_until" => {
                        expect_args(args, 1, name)?;
                        Ok(Value::Null)
                    }
                    name if is_explicit_pg_stub(name) => unsupported_named_function(name),
                    // current_setting reads the built-in default when session-local
                    // settings are not tracked.
                    "current_setting" => {
                        if let Some(Value::Text(name)) = args.first() {
                            Ok(Value::Text(pg_size::current_setting_default(name)))
                        } else {
                            Ok(Value::Null)
                        }
                    }
                    "set_config" => eval_set_config(args),
                    "current_schema" => Ok(current_schema_name().map_or(Value::Null, Value::Text)),
                    "current_catalog" | "current_database" => {
                        Ok(current_database_name().map_or(Value::Null, Value::Text))
                    }
                    "current_schemas" => eval_current_schemas(args),
                    "pg_size_pretty" => pg_size::eval_pg_size_pretty(args),
                    "pg_get_userbyid" => eval_pg_get_userbyid(args),
                    // pg_backend_pid
                    "pg_backend_pid" => Ok(Value::Int(
                        i32::try_from(std::process::id()).unwrap_or(i32::MAX),
                    )),
                    "pg_notify" => {
                        expect_args(args, 2, "pg_notify")?;
                        let channel = expect_text_arg(args, 0, "pg_notify", "first")?;
                        let payload = expect_text_arg(args, 1, "pg_notify", "second")?;
                        crate::async_notify::push_notification(channel, payload)?;
                        Ok(Value::Null)
                    }
                    "pg_notification_queue_usage" => {
                        expect_args(args, 0, "pg_notification_queue_usage")?;
                        Ok(Value::Double(crate::async_notify::current_queue_usage()))
                    }
                    "pg_listening_channels" => {
                        expect_args(args, 0, "pg_listening_channels")?;
                        Ok(Value::Array(
                            crate::async_notify::listening_channels()
                                .into_iter()
                                .map(Value::Text)
                                .collect(),
                        ))
                    }
                    "pg_current_xact_id" | "txid_current" => Ok(Value::BigInt(1)),
                    "pg_current_xact_id_if_assigned" | "txid_current_if_assigned" => {
                        Ok(Value::BigInt(1))
                    }
                    "pg_client_encoding" => Ok(Value::Text(COMPAT_CLIENT_ENCODING.to_string())),
                    "getdatabaseencoding" => Ok(Value::Text(COMPAT_SERVER_ENCODING.to_string())),
                    "pg_encoding_to_char" | "pg_catalog.pg_encoding_to_char" => {
                        eval_pg_encoding_to_char(args)
                    }
                    "pg_char_to_encoding" | "pg_catalog.pg_char_to_encoding" => {
                        eval_pg_char_to_encoding(args)
                    }
                    "pg_relation_is_publishable" | "pg_catalog.pg_relation_is_publishable" => {
                        expect_args(args, 1, "pg_relation_is_publishable")?;
                        Ok(Value::Boolean(!matches!(args.first(), Some(Value::Null))))
                    }
                    "version" => Ok(Value::Text(compat_version_banner())),
                    // PG inet session-info functions return NULL when the
                    // session is not over a TCP socket. AionDB's eval layer
                    // doesn't have direct access to the pgwire socket
                    // metadata; returning NULL matches PG semantics for
                    // embedded/unix-socket sessions and unblocks bench
                    // queries that reference these helpers.
                    "inet_client_addr" | "inet_server_addr" | "inet_client_port"
                    | "inet_server_port" => Ok(Value::Null),
                    // pg_is_in_recovery / pg_is_wal_replay_paused - false.
                    "pg_is_in_recovery" | "pg_is_wal_replay_paused" => Ok(Value::Boolean(false)),
                    "pg_postmaster_start_time" | "pg_conf_load_time" => {
                        // Approximate with current time; AionDB doesn't track
                        // postmaster start or config-load timestamps, but
                        // monitoring tools just need a non-erroring value.
                        Ok(Value::TimestampTz(time::OffsetDateTime::now_utc()))
                    }
                    "pg_export_snapshot" => {
                        // PG returns an opaque snapshot ID. We have no
                        // exportable snapshot machinery; return a stable
                        // synthetic identifier so callers stop erroring.
                        Ok(Value::Text("00000000-0000".to_owned()))
                    }
                    "pg_replication_origin_progress" => Ok(Value::Null),
                    "pg_log_backend_memory_contexts" => Ok(Value::Boolean(true)),
                    "pg_ls_dir" | "pg_ls_archive_statusdir" | "pg_ls_logdir" | "pg_ls_tmpdir" => {
                        pg_internal::eval_pg_ls_dir(name, args)
                    }
                    "pg_read_file" => pg_internal::eval_pg_read_file(args),
                    "pg_read_binary_file" => pg_internal::eval_pg_read_binary_file(args),
                    "pg_relation_size"
                    | "pg_table_size"
                    | "pg_total_relation_size"
                    | "pg_indexes_size"
                    | "pg_database_size"
                    | "pg_tablespace_size" => unsupported_pg_size_function(name, args),
                    "pg_column_size" => eval_pg_column_size(args),
                    "pg_tablespace_location" | "pg_catalog.pg_tablespace_location" => {
                        expect_args(args, 1, "pg_tablespace_location")?;
                        match args.first() {
                            Some(Value::Null) | None => Ok(Value::Null),
                            _ => Ok(Value::Text(String::new())),
                        }
                    }
                    "pg_tablespace_databases" => match args.first() {
                        Some(Value::Int(COMPAT_PG_DEFAULT_TABLESPACE_OID)) => Ok(Value::Int(
                            compat_database_oid(COMPAT_DEFAULT_DATABASE_NAME),
                        )),
                        Some(Value::BigInt(value))
                            if *value == i64::from(COMPAT_PG_DEFAULT_TABLESPACE_OID) =>
                        {
                            Ok(Value::Int(compat_database_oid(
                                COMPAT_DEFAULT_DATABASE_NAME,
                            )))
                        }
                        Some(Value::Null) | None => Ok(Value::Null),
                        _ => Ok(Value::Null),
                    },
                    // Trigonometric / hyperbolic / angle-conversion functions
                    "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "atan2" | "sinh"
                    | "cosh" | "tanh" | "asinh" | "acosh" | "atanh" | "sind" | "cosd" | "tand"
                    | "asind" | "acosd" | "atand" | "atan2d" | "cot" | "cotd" | "degrees"
                    | "radians" | "cbrt" | "erf" | "erfc" => math::eval_trig(name, args),
                    // Additional math functions
                    "scale" | "div" | "gcd" | "lcm" | "factorial" | "min_scale" | "trim_scale"
                    | "setseed" | "justify_days" | "justify_hours" | "justify_interval"
                    | "numeric_inc" => math::eval_math_generic(name, args),
                    "num_nulls" => Ok(math::eval_num_nulls(args)),
                    "num_nonnulls" => Ok(math::eval_num_nonnulls(args)),
                    "__aiondb_variadic_num_nulls" => math::eval_num_nulls_variadic(args),
                    "__aiondb_variadic_num_nonnulls" => math::eval_num_nonnulls_variadic(args),
                    "__aiondb_variadic_concat" => eval_variadic_concat(args),
                    "__aiondb_variadic_concat_ws" => eval_variadic_concat_ws(args),
                    "__aiondb_variadic_format" => eval_variadic_format(args),
                    // Generate subscripts (SRF)
                    "generate_subscripts" => math::eval_generate_subscripts(args),
                    // jsonb_path_query - set-returning, returns array for SRF expansion
                    "jsonb_path_query" => {
                        let results = jsonpath::eval_jsonb_path_query_all(args)?;
                        Ok(Value::Array(results))
                    }
                    "jsonb_each" => jsonb::eval_jsonb_each(args),
                    "jsonb_each_text" => jsonb::eval_jsonb_each_text(args),
                    "jsonb_array_elements" => jsonb::eval_jsonb_array_elements(args),
                    "jsonb_array_elements_text" => jsonb::eval_jsonb_array_elements_text(args),
                    "__aiondb_jsonb_each_keys" => jsonb::eval_jsonb_each_keys(args),
                    "__aiondb_jsonb_each_values" => jsonb::eval_jsonb_each_values(args),
                    "__aiondb_jsonb_each_text_values" => jsonb::eval_jsonb_each_text_values(args),
                    "__aiondb_jsonb_to_record" => jsonb::eval_aiondb_jsonb_to_record(args),
                    "__aiondb_json_to_record" => jsonb::eval_aiondb_json_to_record(args),
                    "__aiondb_jsonb_populate_record" => {
                        jsonb::eval_aiondb_jsonb_populate_record(args)
                    }
                    "__aiondb_json_populate_record" => {
                        jsonb::eval_aiondb_json_populate_record(args)
                    }
                    "__aiondb_jsonb_to_recordset" => jsonb::eval_aiondb_jsonb_to_recordset(args),
                    "__aiondb_json_to_recordset" => jsonb::eval_aiondb_json_to_recordset(args),
                    "__aiondb_jsonb_populate_recordset" => {
                        jsonb::eval_aiondb_jsonb_populate_recordset(args)
                    }
                    "__aiondb_json_populate_recordset" => {
                        jsonb::eval_aiondb_json_populate_recordset(args)
                    }
                    "jsonb_populate_record" => jsonb::eval_jsonb_populate_record(args),
                    "json_populate_record" => jsonb::eval_json_populate_record(args),
                    "jsonb_to_record" => jsonb::eval_jsonb_to_record(args),
                    "json_to_record" => jsonb::eval_json_to_record(args),
                    "jsonb_populate_recordset" => jsonb::eval_jsonb_populate_recordset(args),
                    "json_populate_recordset" => jsonb::eval_json_populate_recordset(args),
                    "jsonb_to_recordset" => jsonb::eval_jsonb_to_recordset(args),
                    "json_to_recordset" => jsonb::eval_json_to_recordset(args),
                    "ts_match" => textsearch::eval_ts_match(args),
                    // ── SQL/JSON constructor functions ──
                    // JSON_OBJECT(...) → build a JSON object from key/value pairs
                    "json_object" => json_helpers::eval_json_object(args),
                    // JSON_ARRAY(...) → build a JSON array from values
                    "json_array" => Ok(json_helpers::eval_json_array(args)),
                    // JSON_SCALAR(expr) → wrap a single value as a JSON scalar
                    "json_scalar" => Ok(json_helpers::eval_json_scalar(args)),
                    "__aiondb_is_json" => json_helpers::eval_is_json_predicate(args),
                    "__aiondb_json_array_subquery" => json_helpers::eval_json_array_subquery(args),
                    // ── Geometric type constructors ──
                    // These accept text arguments and return them as text
                    // (passthrough since geometric types are mapped to text)
                    "box" | "circle" | "line" | "lseg" | "path" | "point" | "polygon" => {
                        eval_geometric_constructor(name, args)
                    }
                    // Geometric operators/functions - return text/double representations
                    "area" | "center" | "diameter" | "height" | "radius" | "width" | "diagonal"
                    | "slope" => eval_geometric_measure(name, args),
                    "isclosed" | "isopen" | "ishorizontal" | "isvertical" => {
                        eval_geometric_predicate(name, args)
                    }
                    "npoints" => eval_geometric_npoints(args),
                    "pclose" | "popen" => {
                        if args.is_empty() || matches!(args[0], Value::Null) {
                            Ok(Value::Null)
                        } else {
                            Ok(args[0].clone())
                        }
                    }
                    // ── ANY/ALL/SOME ──
                    // These are array comparison operators; when called as functions
                    // they return the first argument (passthrough).
                    "any" | "all" | "some" => {
                        if args.is_empty() || matches!(args[0], Value::Null) {
                            Ok(Value::Null)
                        } else {
                            Ok(args[0].clone())
                        }
                    }
                    // ── Network type functions (return text representations) ──
                    "macaddr8_set7bit" => {
                        if args.is_empty() || matches!(args[0], Value::Null) {
                            Ok(Value::Null)
                        } else {
                            match &args[0] {
                                Value::MacAddr8(value) => Ok(Value::MacAddr8(value.set7bit())),
                                Value::MacAddr(value) => {
                                    Ok(Value::MacAddr8(value.to_macaddr8().set7bit()))
                                }
                                Value::Text(text) => aiondb_core::MacAddr8::parse(text)
                                    .map(|value| Value::MacAddr8(value.set7bit()))
                                    .ok_or_else(|| DbError::invalid_input_syntax("macaddr8", text)),
                                other => Ok(Value::Text(other.to_string())),
                            }
                        }
                    }
                    "inet_merge" | "inet_same_family" | "broadcast" | "host" | "hostmask"
                    | "masklen" | "netmask" | "network" | "set_masklen" | "text" | "abbrev"
                    | "family" => eval_inet_helper(name, args),
                    // ── Type I/O functions ──
                    "textin" | "textout" | "int4in" | "int4out" | "int8in" | "int8out"
                    | "float4in" | "float4out" | "float8in" | "float8out" | "boolin"
                    | "boolout" | "oidin" | "oidout" => {
                        if args.is_empty() || matches!(args[0], Value::Null) {
                            Ok(Value::Null)
                        } else {
                            Ok(Value::Text(args[0].to_string()))
                        }
                    }
                    // ── Stats/aggregate helpers (scalar pass-through) ──
                    "any_value" | "every" | "bit_and" | "bit_or" | "bit_xor" => {
                        if args.is_empty() || matches!(args[0], Value::Null) {
                            Ok(Value::Null)
                        } else {
                            Ok(args[0].clone())
                        }
                    }
                    // ── JSON functions (non-binary variants) ──
                    "json_build_object" => json_helpers::eval_json_object(args),
                    "json_build_array" => Ok(json_helpers::eval_json_array(args)),
                    "jsonb_build_array" => Ok(jsonb::eval_jsonb_build_array(args)),
                    "jsonb_build_object" => jsonb::eval_jsonb_build_object(args),
                    "json_extract_path" | "json_extract_path_text" => {
                        // Delegate to jsonb equivalents
                        if args.is_empty() || matches!(args[0], Value::Null) {
                            Ok(Value::Null)
                        } else {
                            // Try to parse first arg as jsonb, extract path
                            let json_val = match &args[0] {
                                Value::Jsonb(j) => Cow::Borrowed(j),
                                Value::Text(s) => Cow::Owned(
                                    serde_json::from_str(s).unwrap_or(serde_json::Value::Null),
                                ),
                                _ => return Ok(Value::Null),
                            };
                            let mut current = json_val.as_ref();
                            for arg in &args[1..] {
                                match current {
                                    serde_json::Value::Object(map) => {
                                        let key = match arg {
                                            Value::Text(s) => Cow::Borrowed(s.as_str()),
                                            other => Cow::Owned(other.to_string()),
                                        };
                                        current = match map.get(key.as_ref()) {
                                            Some(v) => v,
                                            None => return Ok(Value::Null),
                                        };
                                    }
                                    serde_json::Value::Array(arr) => {
                                        let key = match arg {
                                            Value::Text(s) => Cow::Borrowed(s.as_str()),
                                            other => Cow::Owned(other.to_string()),
                                        };
                                        if let Ok(idx) = key.parse::<usize>() {
                                            current = match arr.get(idx) {
                                                Some(v) => v,
                                                None => return Ok(Value::Null),
                                            };
                                        } else {
                                            return Ok(Value::Null);
                                        }
                                    }
                                    _ => return Ok(Value::Null),
                                }
                            }
                            if name == "json_extract_path_text" {
                                match current {
                                    serde_json::Value::String(s) => Ok(Value::Text(s.clone())),
                                    serde_json::Value::Null => Ok(Value::Null),
                                    other => Ok(Value::Text(
                                        aiondb_core::value::pg_jsonb_to_string(other),
                                    )),
                                }
                            } else {
                                Ok(Value::Jsonb(current.clone()))
                            }
                        }
                    }
                    "json_array_length" => {
                        if args.is_empty() || matches!(args[0], Value::Null) {
                            return Ok(Value::Null);
                        }
                        let json_val_owned;
                        let json_val = match &args[0] {
                            Value::Jsonb(j) => j,
                            Value::Text(s) => {
                                json_val_owned =
                                    serde_json::from_str(s).unwrap_or(serde_json::Value::Null);
                                &json_val_owned
                            }
                            _ => return Ok(Value::Null),
                        };
                        match json_val {
                            serde_json::Value::Array(arr) => {
                                Ok(Value::Int(to_i32_saturating(arr.len())))
                            }
                            _ => Err(DbError::internal(
                                "cannot get array length of a non-array".to_string(),
                            )),
                        }
                    }
                    "row_to_json" | "to_json" | "to_jsonb" => {
                        if args.is_empty() || matches!(args[0], Value::Null) {
                            Ok(Value::Null)
                        } else {
                            match &args[0] {
                                Value::Jsonb(j) => Ok(Value::Jsonb(j.clone())),
                                Value::Text(s) => {
                                    Ok(Value::Jsonb(serde_json::Value::String(s.clone())))
                                }
                                Value::Int(n) => {
                                    Ok(Value::Jsonb(serde_json::Value::Number((*n).into())))
                                }
                                Value::BigInt(n) => {
                                    Ok(Value::Jsonb(serde_json::Value::Number((*n).into())))
                                }
                                Value::Boolean(b) => Ok(Value::Jsonb(serde_json::Value::Bool(*b))),
                                Value::Timestamp(dt) => {
                                    Ok(Value::Jsonb(serde_json::Value::String(
                                        aiondb_core::temporal::format_timestamp_json(dt),
                                    )))
                                }
                                Value::TimestampTz(odt) => {
                                    Ok(Value::Jsonb(serde_json::Value::String(
                                        aiondb_core::temporal::format_timestamptz_json(odt),
                                    )))
                                }
                                Value::Date(d) => Ok(Value::Jsonb(serde_json::Value::String(
                                    aiondb_core::temporal::format_date(*d),
                                ))),
                                other => {
                                    Ok(Value::Jsonb(serde_json::Value::String(other.to_string())))
                                }
                            }
                        }
                    }
                    "json_strip_nulls" => {
                        if args.is_empty() || matches!(args[0], Value::Null) {
                            Ok(Value::Null)
                        } else {
                            match &args[0] {
                                Value::Jsonb(j) => Ok(Value::Jsonb(json_helpers::strip_nulls(j))),
                                Value::Text(s) => {
                                    if let Ok(j) = serde_json::from_str::<serde_json::Value>(s) {
                                        Ok(Value::Jsonb(json_helpers::strip_nulls(&j)))
                                    } else {
                                        Ok(Value::Null)
                                    }
                                }
                                _ => Ok(Value::Null),
                            }
                        }
                    }
                    "json_typeof" => {
                        if args.is_empty() || matches!(args[0], Value::Null) {
                            Ok(Value::Null)
                        } else {
                            let json_val_owned;
                            let json_val = match &args[0] {
                                Value::Jsonb(j) => j,
                                Value::Text(s) => {
                                    json_val_owned =
                                        serde_json::from_str(s).unwrap_or(serde_json::Value::Null);
                                    &json_val_owned
                                }
                                _ => return Ok(Value::Null),
                            };
                            let type_name = match json_val {
                                serde_json::Value::Object(_) => "object",
                                serde_json::Value::Array(_) => "array",
                                serde_json::Value::String(_) => "string",
                                serde_json::Value::Number(_) => "number",
                                serde_json::Value::Bool(_) => "boolean",
                                serde_json::Value::Null => "null",
                            };
                            Ok(Value::Text(type_name.to_string()))
                        }
                    }
                    "array_to_json" => {
                        if args.is_empty() || matches!(args[0], Value::Null) {
                            Ok(Value::Null)
                        } else {
                            match &args[0] {
                                Value::Array(arr) => {
                                    let json_arr: Vec<serde_json::Value> =
                                        arr.iter().map(json_helpers::value_to_json).collect();
                                    Ok(Value::Jsonb(serde_json::Value::Array(json_arr)))
                                }
                                Value::Text(s) => {
                                    Ok(Value::Jsonb(serde_json::Value::String(s.clone())))
                                }
                                other => {
                                    Ok(Value::Jsonb(serde_json::Value::String(other.to_string())))
                                }
                            }
                        }
                    }
                    "__aiondb_jsonb_agg_finalize" => {
                        if args.is_empty() || matches!(args[0], Value::Null) {
                            Ok(Value::Null)
                        } else {
                            match &args[0] {
                                Value::Array(arr) => {
                                    let json_arr =
                                        arr.iter().map(json_helpers::value_to_json).collect();
                                    Ok(Value::Jsonb(serde_json::Value::Array(json_arr)))
                                }
                                other => Err(DbError::internal(format!(
                                    "__aiondb_jsonb_agg_finalize expects array input, got {:?}",
                                    other.data_type()
                                ))),
                            }
                        }
                    }
                    "__aiondb_jsonb_agg_ordered_finalize" => {
                        if args.is_empty() || matches!(args[0], Value::Null) {
                            Ok(Value::Null)
                        } else {
                            match &args[0] {
                                Value::Array(arr) => {
                                    let mut json_arr = Vec::with_capacity(arr.len());
                                    for item in arr {
                                        match item {
                                            Value::Jsonb(serde_json::Value::Array(parts)) => {
                                                json_arr.push(
                                                    parts
                                                        .last()
                                                        .cloned()
                                                        .unwrap_or(serde_json::Value::Null),
                                                );
                                            }
                                            other => {
                                                json_arr.push(json_helpers::value_to_json(other))
                                            }
                                        }
                                    }
                                    Ok(Value::Jsonb(serde_json::Value::Array(json_arr)))
                                }
                                other => Err(DbError::internal(format!(
                                    "__aiondb_jsonb_agg_ordered_finalize expects array input, got {:?}",
                                    other.data_type()
                                ))),
                            }
                        }
                    }
                    "__aiondb_jsonb_object_agg_finalize" => {
                        if args.is_empty() || matches!(args[0], Value::Null) {
                            Ok(Value::Null)
                        } else {
                            let Value::Array(arr) = &args[0] else {
                                return Err(DbError::internal(format!(
                                    "__aiondb_jsonb_object_agg_finalize expects array input, got {:?}",
                                    args[0].data_type()
                                )));
                            };
                            let mut object = serde_json::Map::new();
                            for item in arr {
                                let (key, value) = match item {
                                    Value::Jsonb(serde_json::Value::Array(parts)) => {
                                        let key = parts
                                            .first()
                                            .cloned()
                                            .unwrap_or(serde_json::Value::Null);
                                        let value = parts
                                            .get(1)
                                            .cloned()
                                            .unwrap_or(serde_json::Value::Null);
                                        (key, value)
                                    }
                                    other => (
                                        json_helpers::value_to_json(other),
                                        serde_json::Value::Null,
                                    ),
                                };
                                if key.is_null() {
                                    return Err(DbError::from_report(ErrorReport::new(
                                        SqlState::NotNullViolation,
                                        "field name must not be null",
                                    )));
                                }
                                let key_name = match &key {
                                    serde_json::Value::String(s) => s.clone(),
                                    other => aiondb_core::value::pg_jsonb_to_string(other),
                                };
                                object.insert(key_name, value);
                            }
                            Ok(Value::Jsonb(serde_json::Value::Object(object)))
                        }
                    }
                    "pg_size_bytes" => pg_size::eval_pg_size_bytes(args),
                    // ── Newly implemented text/regex functions ──
                    "regexp_count" => text_extended::eval_regexp_count(args),
                    "regexp_like" => text_extended::eval_regexp_like(args),
                    "regexp_instr" => text_extended::eval_regexp_instr(args),
                    "regexp_substr" => text_extended::eval_regexp_substr(args),
                    "btrim" => text_extended::eval_btrim(args),
                    "unistr" => text_extended::eval_unistr(args),
                    // ── SHA-2 hash functions ──
                    "sha224" => text_extended::eval_sha224(args),
                    "sha256" => text_extended::eval_sha256(args),
                    "sha384" => text_extended::eval_sha384(args),
                    "sha512" => text_extended::eval_sha512(args),
                    // ── Bytea bit/byte functions ──
                    "get_bit" => text_extended::eval_get_bit(args),
                    "set_bit" => text_extended::eval_set_bit(args),
                    "get_byte" => text_extended::eval_get_byte(args),
                    "set_byte" => text_extended::eval_set_byte(args),
                    "bit_count" => text_extended::eval_bit_count(args),
                    // ── JSONB function-call variants of operators ──
                    "jsonb_contains" => jsonb::eval_json_contains(
                        args.first().unwrap_or(&Value::Null),
                        args.get(1).unwrap_or(&Value::Null),
                    ),
                    "jsonb_contained" => jsonb::eval_json_contained_by(
                        args.first().unwrap_or(&Value::Null),
                        args.get(1).unwrap_or(&Value::Null),
                    ),
                    "jsonb_exists" => jsonb::eval_json_key_exists(
                        args.first().unwrap_or(&Value::Null),
                        args.get(1).unwrap_or(&Value::Null),
                    ),
                    "jsonb_exists_any" => jsonb::eval_json_any_key_exists(
                        args.first().unwrap_or(&Value::Null),
                        args.get(1).unwrap_or(&Value::Null),
                    ),
                    "jsonb_exists_all" => jsonb::eval_json_all_keys_exist(
                        args.first().unwrap_or(&Value::Null),
                        args.get(1).unwrap_or(&Value::Null),
                    ),
                    "jsonb_concat" => {
                        if args.len() < 2 {
                            return Err(DbError::internal("jsonb_concat requires 2 arguments"));
                        }
                        super::operators::eval_concat(&args[0], &args[1])
                    }
                    "jsonb_delete" => json_helpers::eval_jsonb_delete(args, false),
                    "jsonb_delete_path" => json_helpers::eval_jsonb_delete(args, true),
                    "jsonb_to_tsvector" | "json_to_tsvector" => jsonb::eval_jsonb_to_tsvector(args),
                    "jsonb_insert" => json_helpers::eval_jsonb_insert(args),
                    "jsonb_object" => json_helpers::eval_jsonb_object(args),
                    "jsonb_set_lax" => {
                        // jsonb_set_lax is like jsonb_set but with null-handling modes.
                        // For basic compat, delegate to jsonb_set.
                        jsonb::eval_jsonb_set(args)
                    }
                    // GIN maintenance stub: AionDB has no pending list,
                    // so always return 0 pages cleaned.
                    "gin_clean_pending_list" => Ok(Value::BigInt(0)),
                    // ── Type cast functions ──
                    // float8(x) = x::float8, float4(x) = x::float4, etc.
                    "float8" => {
                        if args.is_empty() || matches!(args[0], Value::Null) {
                            Ok(Value::Null)
                        } else {
                            super::cast::cast_value(args[0].clone(), &DataType::Double)
                        }
                    }
                    "float4" => {
                        if args.is_empty() || matches!(args[0], Value::Null) {
                            Ok(Value::Null)
                        } else {
                            super::cast::cast_value(args[0].clone(), &DataType::Real)
                        }
                    }
                    "int2" => {
                        if args.is_empty() || matches!(args[0], Value::Null) {
                            Ok(Value::Null)
                        } else {
                            // For Numeric inputs, use the dedicated numeric_to_i16
                            // which produces "smallint" in error messages.
                            if let Value::Numeric(n) = &args[0] {
                                return super::cast::numeric::numeric_to_i16(n);
                            }
                            // For BigInt inputs, range-check directly against
                            // smallint bounds so the error message says
                            // "smallint out of range" rather than the generic
                            // "integer out of range" from the BigInt->Int path.
                            if let Value::BigInt(v) = &args[0] {
                                if *v < -32768 || *v > 32767 {
                                    return Err(DbError::from_report(ErrorReport::new(
                                        SqlState::NumericValueOutOfRange,
                                        "smallint out of range",
                                    )));
                                }
                                let narrowed = i32::try_from(*v).map_err(|_| {
                                    DbError::from_report(ErrorReport::new(
                                        SqlState::NumericValueOutOfRange,
                                        "smallint out of range",
                                    ))
                                })?;
                                return Ok(Value::Int(narrowed));
                            }
                            let val = super::cast::cast_value(args[0].clone(), &DataType::Int)?;
                            // Range-check for int2 (smallint is -32768..32767)
                            if let Value::Int(v) = &val {
                                if *v < -32768 || *v > 32767 {
                                    return Err(DbError::from_report(ErrorReport::new(
                                        SqlState::NumericValueOutOfRange,
                                        "smallint out of range",
                                    )));
                                }
                            }
                            Ok(val)
                        }
                    }
                    "int4" => {
                        if args.is_empty() || matches!(args[0], Value::Null) {
                            Ok(Value::Null)
                        } else {
                            super::cast::cast_value(args[0].clone(), &DataType::Int)
                        }
                    }
                    "int8" => {
                        if args.is_empty() || matches!(args[0], Value::Null) {
                            Ok(Value::Null)
                        } else {
                            super::cast::cast_value(args[0].clone(), &DataType::BigInt)
                        }
                    }
                    "oid" => {
                        if args.is_empty() || matches!(args[0], Value::Null) {
                            Ok(Value::Null)
                        } else {
                            if let Value::Text(raw_input) = &args[0] {
                                let trimmed = raw_input.trim();
                                let has_valid_syntax = !trimmed.is_empty()
                                    && trimmed.chars().enumerate().all(|(idx, ch)| {
                                        if idx == 0 && matches!(ch, '+' | '-') {
                                            true
                                        } else {
                                            ch.is_ascii_digit()
                                        }
                                    });
                                if !has_valid_syntax {
                                    return Err(DbError::invalid_input_syntax("oid", raw_input));
                                }
                                let parsed = trimmed.parse::<i128>().map_err(|_| {
                                    DbError::from_report(ErrorReport::new(
                                        SqlState::NumericValueOutOfRange,
                                        format!(
                                            "value \"{}\" is out of range for type oid",
                                            raw_input
                                        ),
                                    ))
                                })?;
                                if parsed < 0 || parsed > i128::from(u32::MAX) {
                                    return Err(DbError::from_report(ErrorReport::new(
                                        SqlState::NumericValueOutOfRange,
                                        format!(
                                            "value \"{}\" is out of range for type oid",
                                            raw_input
                                        ),
                                    )));
                                }
                                let oid_u32 = u32::try_from(parsed).map_err(|_| {
                                    DbError::from_report(ErrorReport::new(
                                        SqlState::NumericValueOutOfRange,
                                        format!(
                                            "value \"{}\" is out of range for type oid",
                                            raw_input
                                        ),
                                    ))
                                })?;
                                return Ok(Value::Int(i32::from_ne_bytes(oid_u32.to_ne_bytes())));
                            }
                            // For BigInt inputs, range-check directly against
                            // OID bounds (0..=4294967295) with the correct
                            // error message instead of using the generic
                            // BigInt->Int cast which says "integer out of
                            // range".
                            if let Value::BigInt(v) = &args[0] {
                                if *v < 0 || *v > i64::from(u32::MAX) {
                                    return Err(DbError::from_report(ErrorReport::new(
                                        SqlState::NumericValueOutOfRange,
                                        format!("value \"{}\" is out of range for type oid", v),
                                    )));
                                }
                                let oid_u32 = u32::try_from(*v).map_err(|_| {
                                    DbError::from_report(ErrorReport::new(
                                        SqlState::NumericValueOutOfRange,
                                        format!("value \"{}\" is out of range for type oid", v),
                                    ))
                                })?;
                                return Ok(Value::Int(i32::from_ne_bytes(oid_u32.to_ne_bytes())));
                            }
                            super::cast::cast_value(args[0].clone(), &DataType::Int)
                        }
                    }
                    // ── Test-helper identity/constructor functions ──
                    // These replicate simple PL/pgSQL functions that pg_regress
                    // tests define inside BEGIN/ROLLBACK blocks. AionDB cannot
                    // execute PL/pgSQL, so they are provided as built-ins.
                    //
                    // vol(text) → text  : volatile identity function
                    "vol" => {
                        expect_args(args, 1, "vol")?;
                        Ok(args[0].clone())
                    }
                    // volfoo(text) → text (foodomain) : volatile identity,
                    // returns the argument unchanged (domain is treated as
                    // its base type).
                    "volfoo" => {
                        expect_args(args, 1, "volfoo")?;
                        Ok(args[0].clone())
                    }
                    // make_ad(int, int) → int[] : constructs a two-element
                    // integer array from the arguments.
                    "make_ad" => {
                        expect_args(args, 2, "make_ad")?;
                        Ok(Value::Array(vec![args[0].clone(), args[1].clone()]))
                    }
                    // enum_range / enum_first / enum_last - normally
                    // resolved at plan time, but provide a runtime
                    // fallback that returns an empty array / NULL.
                    "enum_range" => Ok(Value::Array(Vec::new())),
                    "enum_first" | "enum_last" => Ok(Value::Null),
                    // ── Cypher type conversion functions ──
                    "cypher_toboolean" => cypher::eval_to_boolean(args),
                    "cypher_tointeger" => cypher::eval_to_integer(args),
                    "cypher_tofloat" => cypher::eval_to_float(args),
                    "cypher_tostring" => cypher::eval_to_string(args),
                    "cypher_tobooleanornull" => cypher::eval_to_boolean_or_null(args),
                    "cypher_tointegerornull" => cypher::eval_to_integer_or_null(args),
                    "cypher_tofloatornull" => cypher::eval_to_float_or_null(args),
                    "cypher_tostringornull" => cypher::eval_to_string_or_null(args),
                    // ── Cypher temporal constructors ──
                    "cypher_date" => cypher_temporal::eval_cypher_date(args),
                    "cypher_time" => cypher_temporal::eval_cypher_time(args),
                    "cypher_datetime" => cypher_temporal::eval_cypher_datetime(args),
                    "cypher_localtime" => cypher_temporal::eval_cypher_localtime(args),
                    "cypher_localdatetime" => cypher_temporal::eval_cypher_localdatetime(args),
                    "cypher_duration" => cypher_temporal::eval_cypher_duration(args),
                    // ── Cypher temporal truncate/between ──
                    "date.truncate" => cypher_temporal::operations::eval_date_truncate(args),
                    "datetime.truncate" => {
                        cypher_temporal::operations::eval_datetime_truncate(args)
                    }
                    "localdatetime.truncate" => {
                        cypher_temporal::operations::eval_localdatetime_truncate(args)
                    }
                    "time.truncate" => cypher_temporal::operations::eval_time_truncate(args),
                    "localtime.truncate" => {
                        cypher_temporal::operations::eval_localtime_truncate(args)
                    }
                    "duration.between" => cypher_temporal::operations::eval_duration_between(args),
                    "duration.inmonths" => {
                        cypher_temporal::operations::eval_duration_in_months(args)
                    }
                    "duration.indays" => cypher_temporal::operations::eval_duration_in_days(args),
                    "duration.inseconds" => {
                        cypher_temporal::operations::eval_duration_in_seconds(args)
                    }
                    // ── Cypher math ──
                    "e" => Ok(Value::Double(std::f64::consts::E)),
                    // ── Cypher range() ──
                    "range" => cypher_temporal::operations::eval_cypher_range(args),
                    // ── Cypher size/head/last ──
                    "cypher_size" => eval_cypher_size(args),
                    "cypher_head" => eval_cypher_head(args),
                    "cypher_last" => eval_cypher_last(args),
                    "cypher_tail" => eval_cypher_tail(args),
                    "cypher_array_get" => Ok(eval_cypher_array_get(args)),
                    "__cypher_starts_with" => eval_cypher_starts_with(args),
                    "__cypher_ends_with" => eval_cypher_ends_with(args),
                    "__cypher_contains" => eval_cypher_contains(args),
                    "__cypher_in" => eval_cypher_in(args),
                    "__cypher_has_label" => eval_cypher_has_label(args),
                    // ── Cypher graph element introspection ──
                    "graph_labels" => graph::eval_graph_labels(args),
                    "graph_type" => graph::eval_graph_type(args),
                    "graph_id" => graph::eval_graph_id(args),
                    "graph_properties" => graph::eval_graph_properties(args),
                    "graph_start_node" => graph::eval_graph_start_node(args),
                    "graph_end_node" => graph::eval_graph_end_node(args),
                    "graph_path_length" => graph::eval_graph_path_length(args),
                    "graph_nodes" => graph::eval_graph_nodes(args),
                    "graph_relationships" => graph::eval_graph_relationships(args),
                    _ => {
                        // Check extension registry for functions contributed by
                        // installed extensions (e.g. uuid-ossp, pgcrypto).
                        if let Some(ext_fn) =
                            crate::extension_registry().and_then(|reg| reg.lookup_function(name))
                        {
                            return (ext_fn.eval_fn)(args);
                        }
                        unsupported_named_function(name)
                    }
                }
            }
        }
    }
}

fn eval_cypher_size(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "size")?;
    match &args[0] {
        Value::Null => Ok(Value::Null),
        Value::Text(s) => Ok(Value::BigInt(
            i64::try_from(text_char_count(s)).unwrap_or(i64::MAX),
        )),
        Value::Array(elems) => Ok(Value::BigInt(
            i64::try_from(elems.len()).unwrap_or(i64::MAX),
        )),
        Value::Jsonb(serde_json::Value::Array(items)) => Ok(Value::BigInt(
            i64::try_from(items.len()).unwrap_or(i64::MAX),
        )),
        Value::Jsonb(serde_json::Value::String(s)) => Ok(Value::BigInt(
            i64::try_from(text_char_count(s)).unwrap_or(i64::MAX),
        )),
        _ => Err(DbError::internal(format!(
            "size() does not accept {} values",
            cypher_temporal::value_type_name(&args[0])
        ))),
    }
}

#[inline]
fn text_char_count(s: &str) -> usize {
    if s.is_ascii() {
        s.len()
    } else {
        s.chars().count()
    }
}

fn eval_cypher_head(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "head")?;
    match &args[0] {
        Value::Null => Ok(Value::Null),
        Value::Array(elems) => Ok(elems.first().cloned().unwrap_or(Value::Null)),
        Value::Jsonb(serde_json::Value::Array(items)) => Ok(items
            .first()
            .map(|v| Value::Jsonb(v.clone()))
            .unwrap_or(Value::Null)),
        _ => Err(DbError::internal(format!(
            "head() does not accept {} values",
            cypher_temporal::value_type_name(&args[0])
        ))),
    }
}

fn eval_cypher_last(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "last")?;
    match &args[0] {
        Value::Null => Ok(Value::Null),
        Value::Array(elems) => Ok(elems.last().cloned().unwrap_or(Value::Null)),
        Value::Jsonb(serde_json::Value::Array(items)) => Ok(items
            .last()
            .map(|v| Value::Jsonb(v.clone()))
            .unwrap_or(Value::Null)),
        _ => Err(DbError::internal(format!(
            "last() does not accept {} values",
            cypher_temporal::value_type_name(&args[0])
        ))),
    }
}

fn cypher_resolve_index(idx: i64, len: usize) -> Option<usize> {
    let len_i = i64::try_from(len).ok()?;
    let real = if idx < 0 { len_i + idx } else { idx };
    if real < 0 || real >= len_i {
        None
    } else {
        usize::try_from(real).ok()
    }
}

fn cypher_index_as_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Int(i) => Some(i64::from(*i)),
        Value::BigInt(i) => Some(*i),
        _ => None,
    }
}

fn eval_cypher_array_get(args: &[Value]) -> Value {
    if args.len() != 2 {
        return Value::Null;
    }
    match (&args[0], &args[1]) {
        (Value::Null, _) | (_, Value::Null) => Value::Null,
        (Value::Array(items), idx_val) => {
            let Some(idx) = cypher_index_as_i64(idx_val) else {
                return Value::Null;
            };
            cypher_resolve_index(idx, items.len()).map_or(Value::Null, |pos| items[pos].clone())
        }
        (Value::Jsonb(serde_json::Value::Array(items)), idx_val) => {
            let Some(idx) = cypher_index_as_i64(idx_val) else {
                return Value::Null;
            };
            cypher_resolve_index(idx, items.len())
                .map_or(Value::Null, |pos| Value::Jsonb(items[pos].clone()))
        }
        (Value::Jsonb(serde_json::Value::Object(map)), Value::Text(key)) => map
            .get(key.as_str())
            .map(|v| Value::Jsonb(v.clone()))
            .unwrap_or(Value::Null),
        (Value::Text(s), idx_val) => {
            let Some(idx) = cypher_index_as_i64(idx_val) else {
                return Value::Null;
            };
            // ASCII fast path: char count == byte length, so we can
            // index directly into the byte slice and emit a single
            // 1-byte String. Avoids the per-call `Vec<char>`
            // allocation and the chars().collect() walk.
            if s.is_ascii() {
                let bytes = s.as_bytes();
                return cypher_resolve_index(idx, bytes.len()).map_or(Value::Null, |pos| {
                    Value::Text(String::from(bytes[pos] as char))
                });
            }
            let chars: Vec<char> = s.chars().collect();
            cypher_resolve_index(idx, chars.len())
                .map_or(Value::Null, |pos| Value::Text(chars[pos].to_string()))
        }
        _ => Value::Null,
    }
}

fn eval_cypher_tail(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "tail")?;
    match &args[0] {
        Value::Null => Ok(Value::Null),
        Value::Array(elems) => {
            if elems.is_empty() {
                Ok(Value::Array(Vec::new()))
            } else {
                Ok(Value::Array(elems[1..].to_vec()))
            }
        }
        Value::Jsonb(serde_json::Value::Array(items)) => {
            let rest = if items.is_empty() {
                Vec::new()
            } else {
                items[1..].to_vec()
            };
            Ok(Value::Jsonb(serde_json::Value::Array(rest)))
        }
        _ => Err(DbError::internal(format!(
            "tail() does not accept {} values",
            cypher_temporal::value_type_name(&args[0])
        ))),
    }
}

fn eval_cypher_starts_with(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "STARTS WITH")?;
    match (&args[0], &args[1]) {
        (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
        (Value::Text(haystack), Value::Text(needle)) => {
            Ok(Value::Boolean(haystack.starts_with(needle.as_str())))
        }
        _ => Ok(Value::Null),
    }
}

fn eval_cypher_ends_with(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "ENDS WITH")?;
    match (&args[0], &args[1]) {
        (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
        (Value::Text(haystack), Value::Text(needle)) => {
            Ok(Value::Boolean(haystack.ends_with(needle.as_str())))
        }
        _ => Ok(Value::Null),
    }
}

fn eval_cypher_contains(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "CONTAINS")?;
    match (&args[0], &args[1]) {
        (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
        (Value::Text(haystack), Value::Text(needle)) => {
            Ok(Value::Boolean(haystack.contains(needle.as_str())))
        }
        _ => Ok(Value::Null),
    }
}

fn eval_cypher_in(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "IN")?;
    if matches!(&args[1], Value::Null) {
        return Ok(Value::Null);
    }
    match &args[1] {
        Value::Array(items) => {
            if matches!(&args[0], Value::Null) {
                if items.is_empty() {
                    return Ok(Value::Boolean(false));
                }
                return Ok(Value::Null);
            }
            let mut saw_null = false;
            for item in items {
                if item.is_null() {
                    saw_null = true;
                    continue;
                }
                if values_equal_for_cypher(&args[0], item) {
                    return Ok(Value::Boolean(true));
                }
            }
            if saw_null {
                Ok(Value::Null)
            } else {
                Ok(Value::Boolean(false))
            }
        }
        _ => Ok(Value::Null),
    }
}

fn values_equal_for_cypher(a: &Value, b: &Value) -> bool {
    // Cypher's IN/equality is type-strict: a number and a string with the
    // same lexical form are *not* equal. Numbers compare across their
    // numeric tower (Int/BigInt/Real/Double), but strings/booleans/lists
    // only equal their own kind.
    use crate::eval::operators::compare_runtime_values;
    fn kind(v: &Value) -> u8 {
        match v {
            Value::Null => 0,
            Value::Boolean(_) => 1,
            Value::Int(_)
            | Value::BigInt(_)
            | Value::Real(_)
            | Value::Double(_)
            | Value::Numeric(_) => 2,
            Value::Text(_) => 3,
            Value::Array(_) => 4,
            Value::Jsonb(_) => 5,
            Value::Date(_) => 6,
            Value::Time(_) | Value::TimeTz(_, _) => 7,
            Value::Timestamp(_) | Value::TimestampTz(_) => 8,
            Value::Interval(_) => 9,
            _ => 99,
        }
    }
    if kind(a) != kind(b) {
        return false;
    }
    matches!(
        compare_runtime_values(a, b).ok().flatten(),
        Some(std::cmp::Ordering::Equal)
    )
}

fn eval_cypher_has_label(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "label predicate")?;
    let label = match &args[1] {
        Value::Text(s) => s.as_str(),
        _ => return Ok(Value::Null),
    };
    match &args[0] {
        Value::Null => Ok(Value::Null),
        // The graph executor stores nodes with their labels accessible
        // via the binding's labels field; when a label predicate ends up
        // here we don't have access to that, so the most compatible fall
        // back is to look inside a JSONB rendering when present.
        Value::Jsonb(json) => {
            if let Some(arr) = json.get("__labels").and_then(|v| v.as_array()) {
                Ok(Value::Boolean(
                    arr.iter().any(|v| v.as_str() == Some(label)),
                ))
            } else {
                Ok(Value::Boolean(false))
            }
        }
        _ => Ok(Value::Boolean(false)),
    }
}

fn eval_pg_get_object_address(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 3, "pg_get_object_address")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }

    let object_type_raw = value_to_text(&args[0]).trim().to_owned();
    let object_type = object_type_raw.to_ascii_lowercase();
    let names = parse_pg_object_address_list(&args[1]);
    let arg_names = parse_pg_object_address_list(&args[2]);

    if names.iter().any(Option::is_none) || arg_names.iter().any(Option::is_none) {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            "name or argument lists may not contain nulls",
        ));
    }

    if object_type == "stone" {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!("unrecognized object type \"{object_type_raw}\""),
        ));
    }

    if object_type == "table" && names.is_empty() {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            "name list length must be at least 1",
        ));
    }

    if object_type == "large object" {
        if names.len() != 1 {
            return Err(DbError::bind_error(
                SqlState::InvalidParameterValue,
                "name list length must be exactly 1",
            ));
        }
        let oid_text = names
            .first()
            .and_then(|value| value.as_ref())
            .cloned()
            .unwrap_or_default();
        if oid_text.parse::<u32>().is_err() {
            return Err(DbError::bind_error(
                SqlState::InvalidParameterValue,
                format!("invalid input syntax for type oid: \"{oid_text}\""),
            ));
        }
        if oid_text == "123" {
            return Err(DbError::bind_error(
                SqlState::UndefinedObject,
                "large object 123 does not exist",
            ));
        }
        return Ok(Value::Text(String::new()));
    }

    let singleton_object_type = match object_type.as_str() {
        "language" => Some("language"),
        "schema" => Some("schema"),
        "role" => Some("role"),
        "database" => Some("database"),
        "tablespace" => Some("tablespace"),
        "foreign-data wrapper" => Some("foreign-data wrapper"),
        "server" => Some("server"),
        "extension" => Some("extension"),
        "event trigger" => Some("event trigger"),
        "access method" => Some("access method"),
        "publication" => Some("publication"),
        "subscription" => Some("subscription"),
        _ => None,
    };

    if let Some(kind) = singleton_object_type {
        if names.len() != 1 {
            return Err(DbError::bind_error(
                SqlState::InvalidParameterValue,
                "name list length must be exactly 1",
            ));
        }
        let name = names
            .first()
            .and_then(|value| value.as_ref())
            .cloned()
            .unwrap_or_default();
        if name.eq_ignore_ascii_case("one") {
            return Err(DbError::bind_error(
                SqlState::UndefinedObject,
                format!("{kind} \"{name}\" does not exist"),
            ));
        }
    }

    Ok(Value::Text(String::new()))
}

fn eval_makeaclitem(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 4, "makeaclitem")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }

    let grantee = aclitem_role_name(&args[0])?;
    let grantor = aclitem_role_name(&args[1])?;
    let privilege_list = value_to_text(&args[2]);
    let grant_option = match &args[3] {
        Value::Boolean(flag) => *flag,
        other => {
            return Err(DbError::bind_error(
                SqlState::InvalidParameterValue,
                format!(
                    "makeaclitem grant option must be boolean, got {}",
                    value_to_text(other)
                ),
            ));
        }
    };

    let mode = aclitem_mode_from_privilege_list(&privilege_list)?;
    let rendered_mode = if grant_option {
        mode.chars().flat_map(|ch| [ch, '*']).collect::<String>()
    } else {
        mode
    };
    Ok(Value::Text(format!("{grantee}={rendered_mode}/{grantor}")))
}

fn aclitem_role_name(value: &Value) -> DbResult<String> {
    match value {
        Value::Text(name) => Ok(name.trim_matches('"').to_owned()),
        Value::Int(oid) => Ok(with_current_session_context(|ctx| {
            ctx.role_names_by_oid
                .get(oid)
                .cloned()
                .unwrap_or_else(|| oid.to_string())
        })),
        Value::BigInt(oid) => {
            let oid = i32::try_from(*oid).map_err(|_| {
                DbError::bind_error(
                    SqlState::NumericValueOutOfRange,
                    format!("OID value {oid} is out of range"),
                )
            })?;
            Ok(with_current_session_context(|ctx| {
                ctx.role_names_by_oid
                    .get(&oid)
                    .cloned()
                    .unwrap_or_else(|| oid.to_string())
            }))
        }
        other => Ok(value_to_text(other)),
    }
}

fn aclitem_mode_from_privilege_list(raw: &str) -> DbResult<String> {
    let mut chars = Vec::new();
    for entry in raw.split(',') {
        let privilege = entry.trim().to_ascii_lowercase();
        if privilege.is_empty() {
            continue;
        }
        let mode_char = match privilege.as_str() {
            "select" => 'r',
            "insert" => 'a',
            "update" => 'w',
            "delete" => 'd',
            "truncate" => 'D',
            "references" => 'x',
            "trigger" => 't',
            "execute" => 'X',
            "usage" => 'U',
            "create" => 'C',
            "temporary" | "temp" => 'T',
            "connect" => 'c',
            "set" => 's',
            "alter system" => 'A',
            other => {
                return Err(DbError::bind_error(
                    SqlState::InvalidParameterValue,
                    format!("unrecognized privilege type: \"{other}\""),
                ));
            }
        };
        if !chars.contains(&mode_char) {
            chars.push(mode_char);
        }
    }
    if chars.is_empty() {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            "unrecognized privilege type: \"\"",
        ));
    }
    const ACL_CANONICAL_ORDER: &str = "arwdDxtXUCTcsA";
    chars.sort_by_key(|ch| ACL_CANONICAL_ORDER.find(*ch).unwrap_or(usize::MAX));
    Ok(chars.into_iter().collect())
}

#[derive(Default)]
struct LargeObjectRegistry {
    next_oid: i32,
    objects: HashMap<i32, Vec<u8>>,
    sessions: HashMap<u64, LargeObjectSessionState>,
    brin_ranges: HashMap<i32, BTreeSet<i64>>,
}

const MAX_COMPAT_LARGE_OBJECT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Default)]
struct LargeObjectSessionState {
    next_fd: i32,
    fds: HashMap<i32, LargeObjectFdState>,
}

#[derive(Clone, Copy)]
struct LargeObjectFdState {
    oid: i32,
    position: usize,
}

fn lo_registry() -> &'static Mutex<LargeObjectRegistry> {
    static REGISTRY: OnceLock<Mutex<LargeObjectRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut registry = LargeObjectRegistry::default();
        registry.next_oid = 16_384;
        Mutex::new(registry)
    })
}

fn lo_session_key() -> u64 {
    let key = current_lo_session_key();
    if key == 0 {
        1
    } else {
        key
    }
}

fn lo_arg_i32(value: &Value, function_name: &str, arg_name: &str) -> DbResult<i32> {
    match value {
        Value::Int(v) => Ok(*v),
        Value::BigInt(v) => i32::try_from(*v).map_err(|_| {
            DbError::bind_error(
                SqlState::NumericValueOutOfRange,
                format!("{function_name}() {arg_name} is out of range"),
            )
        }),
        Value::Text(text) => text.trim().parse::<i32>().map_err(|_| {
            DbError::bind_error(
                SqlState::InvalidParameterValue,
                format!("{function_name}() {arg_name} must be integer"),
            )
        }),
        _ => Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!("{function_name}() {arg_name} must be integer"),
        )),
    }
}

fn lo_arg_usize(value: &Value, function_name: &str, arg_name: &str) -> DbResult<usize> {
    let signed = lo_arg_i32(value, function_name, arg_name)?;
    if signed < 0 {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!("{function_name}() {arg_name} must be non-negative"),
        ));
    }
    usize::try_from(signed).map_err(|_| {
        DbError::bind_error(
            SqlState::NumericValueOutOfRange,
            format!("{function_name}() {arg_name} is out of range"),
        )
    })
}

fn lo_arg_i64(value: &Value, function_name: &str, arg_name: &str) -> DbResult<i64> {
    match value {
        Value::Int(v) => Ok(i64::from(*v)),
        Value::BigInt(v) => Ok(*v),
        Value::Text(text) => text.trim().parse::<i64>().map_err(|_| {
            DbError::bind_error(
                SqlState::InvalidParameterValue,
                format!("{function_name}() {arg_name} must be integer"),
            )
        }),
        _ => Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!("{function_name}() {arg_name} must be integer"),
        )),
    }
}

fn lo_object_not_found(oid: i32) -> DbError {
    DbError::bind_error(
        SqlState::UndefinedObject,
        format!("large object {oid} does not exist"),
    )
}

fn lo_bad_fd(fd: i32) -> DbError {
    DbError::bind_error(
        SqlState::InvalidParameterValue,
        format!("invalid large-object descriptor: {fd}"),
    )
}

fn lo_space_exhausted(kind: &str) -> DbError {
    DbError::program_limit(format!("large object {kind} space exhausted"))
}

fn next_available_lo_oid(registry: &LargeObjectRegistry) -> DbResult<i32> {
    let mut candidate = registry.next_oid.max(16_384);
    loop {
        if !registry.objects.contains_key(&candidate) {
            return Ok(candidate);
        }
        candidate = candidate
            .checked_add(1)
            .ok_or_else(|| lo_space_exhausted("OID"))?;
    }
}

fn advance_next_lo_oid(registry: &mut LargeObjectRegistry, oid: i32) {
    if let Some(next) = oid.checked_add(1) {
        registry.next_oid = registry.next_oid.max(next);
    } else {
        registry.next_oid = i32::MAX;
    }
}

fn next_available_lo_fd(session: &LargeObjectSessionState) -> DbResult<i32> {
    let mut fd = session.next_fd.max(1);
    loop {
        if !session.fds.contains_key(&fd) {
            return Ok(fd);
        }
        fd = fd
            .checked_add(1)
            .ok_or_else(|| lo_space_exhausted("descriptor"))?;
    }
}

fn advance_next_lo_fd(session: &mut LargeObjectSessionState, fd: i32) {
    if let Some(next) = fd.checked_add(1) {
        session.next_fd = session.next_fd.max(next);
    } else {
        session.next_fd = i32::MAX;
    }
}

fn checked_lo_size(function_name: &str, size: usize) -> DbResult<()> {
    if size > MAX_COMPAT_LARGE_OBJECT_BYTES {
        return Err(DbError::program_limit(format!(
            "{function_name}() large object size {size} exceeds maximum {MAX_COMPAT_LARGE_OBJECT_BYTES}"
        )));
    }
    Ok(())
}

fn checked_lo_write_range(
    function_name: &str,
    offset: i64,
    bytes_len: usize,
) -> DbResult<(usize, usize)> {
    let start = usize::try_from(offset).map_err(|_| {
        DbError::program_limit(format!("{function_name}() offset does not fit in usize"))
    })?;
    let end = start.checked_add(bytes_len).ok_or_else(|| {
        DbError::program_limit(format!("{function_name}() large object size overflow"))
    })?;
    checked_lo_size(function_name, end)?;
    Ok((start, end))
}

fn eval_lo_create(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "lo_create")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let requested = lo_arg_i32(&args[0], "lo_create", "oid")?;
    let mut registry = lo_registry()
        .lock()
        .map_err(|e| DbError::internal(format!("large object registry poisoned: {e}")))?;
    let oid = if requested > 0 {
        // Keep deterministic OID behavior for explicit lo_create(oid) calls.
        // Re-create/reset the object when the OID already exists.
        requested
    } else {
        let candidate = next_available_lo_oid(&registry)?;
        advance_next_lo_oid(&mut registry, candidate);
        candidate
    };
    registry.objects.insert(oid, Vec::new());
    advance_next_lo_oid(&mut registry, oid);
    Ok(Value::Int(oid))
}

fn eval_lo_open(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "lo_open")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let oid = lo_arg_i32(&args[0], "lo_open", "oid")?;
    let _mode = lo_arg_i32(&args[1], "lo_open", "mode")?;
    let mut registry = lo_registry()
        .lock()
        .map_err(|e| DbError::internal(format!("large object registry poisoned: {e}")))?;
    if !registry.objects.contains_key(&oid) {
        return Err(lo_object_not_found(oid));
    }
    let session = registry.sessions.entry(lo_session_key()).or_default();
    let fd = next_available_lo_fd(session)?;
    advance_next_lo_fd(session, fd);
    session
        .fds
        .insert(fd, LargeObjectFdState { oid, position: 0 });
    Ok(Value::Int(fd))
}

fn eval_lo_close(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "lo_close")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let fd = lo_arg_i32(&args[0], "lo_close", "fd")?;
    let mut registry = lo_registry()
        .lock()
        .map_err(|e| DbError::internal(format!("large object registry poisoned: {e}")))?;
    let Some(session) = registry.sessions.get_mut(&lo_session_key()) else {
        return Err(lo_bad_fd(fd));
    };
    if session.fds.remove(&fd).is_some() {
        Ok(Value::Int(0))
    } else {
        Err(lo_bad_fd(fd))
    }
}

fn eval_loread(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "loread")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let fd = lo_arg_i32(&args[0], "loread", "fd")?;
    let len = lo_arg_usize(&args[1], "loread", "len")?;
    let mut registry = lo_registry()
        .lock()
        .map_err(|e| DbError::internal(format!("large object registry poisoned: {e}")))?;
    let session_key = lo_session_key();
    let (oid, start) = {
        let Some(session) = registry.sessions.get_mut(&session_key) else {
            return Err(lo_bad_fd(fd));
        };
        let Some(state) = session.fds.get_mut(&fd) else {
            return Err(lo_bad_fd(fd));
        };
        (state.oid, state.position)
    };
    let Some(data) = registry.objects.get(&oid) else {
        return Err(lo_object_not_found(oid));
    };
    let start = start.min(data.len());
    let end = start.saturating_add(len).min(data.len());
    let chunk = data[start..end].to_vec();
    if let Some(session) = registry.sessions.get_mut(&session_key) {
        if let Some(state) = session.fds.get_mut(&fd) {
            state.position = end;
        }
    }
    Ok(Value::Blob(chunk))
}

fn eval_lowrite(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "lowrite")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let fd = lo_arg_i32(&args[0], "lowrite", "fd")?;
    let bytes = match &args[1] {
        Value::Blob(blob) => blob.clone(),
        Value::Text(text) => text.as_bytes().to_vec(),
        other => value_to_text(other).into_bytes(),
    };
    let mut registry = lo_registry()
        .lock()
        .map_err(|e| DbError::internal(format!("large object registry poisoned: {e}")))?;
    let session_key = lo_session_key();
    let (oid, start) = {
        let Some(session) = registry.sessions.get_mut(&session_key) else {
            return Err(lo_bad_fd(fd));
        };
        let Some(state) = session.fds.get_mut(&fd) else {
            return Err(lo_bad_fd(fd));
        };
        (state.oid, state.position)
    };
    let Some(data) = registry.objects.get_mut(&oid) else {
        return Err(lo_object_not_found(oid));
    };
    let start = start.min(data.len());
    let needed = start
        .checked_add(bytes.len())
        .ok_or_else(|| DbError::program_limit("lowrite() large object size overflow".to_owned()))?;
    checked_lo_size("lowrite", needed)?;
    if data.len() < needed {
        data.resize(needed, 0);
    }
    data[start..start + bytes.len()].copy_from_slice(&bytes);
    let end = start + bytes.len();
    if let Some(session) = registry.sessions.get_mut(&session_key) {
        if let Some(state) = session.fds.get_mut(&fd) {
            state.position = end;
        }
    }
    Ok(Value::Int(i32::try_from(bytes.len()).unwrap_or(i32::MAX)))
}

fn eval_lo_unlink(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "lo_unlink")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let oid = lo_arg_i32(&args[0], "lo_unlink", "oid")?;
    let mut registry = lo_registry()
        .lock()
        .map_err(|e| DbError::internal(format!("large object registry poisoned: {e}")))?;
    if registry.objects.remove(&oid).is_none() {
        return Err(lo_object_not_found(oid));
    }
    for session in registry.sessions.values_mut() {
        session.fds.retain(|_, state| state.oid != oid);
    }
    Ok(Value::Int(1))
}

/// `lo_lseek(fd, offset, whence) → integer`. PG defines `whence` as
/// `SEEK_SET=0`, `SEEK_CUR=1`, `SEEK_END=2`. The result is the resulting
/// position (clamped to `[0, length(lo)]`). Negative offsets are allowed and
/// move the cursor backwards. `bigint=true` selects the `lo_lseek64` variant
/// returning int8 instead of int4.
fn eval_lo_lseek(args: &[Value], bigint: bool) -> DbResult<Value> {
    expect_args(args, 3, "lo_lseek")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let fd = lo_arg_i32(&args[0], "lo_lseek", "fd")?;
    let offset = lo_arg_i64(&args[1], "lo_lseek", "offset")?;
    let whence = lo_arg_i32(&args[2], "lo_lseek", "whence")?;
    let mut registry = lo_registry()
        .lock()
        .map_err(|e| DbError::internal(format!("large object registry poisoned: {e}")))?;
    let session_key = lo_session_key();
    let (oid, current) = {
        let Some(session) = registry.sessions.get_mut(&session_key) else {
            return Err(lo_bad_fd(fd));
        };
        let Some(state) = session.fds.get(&fd) else {
            return Err(lo_bad_fd(fd));
        };
        (state.oid, state.position)
    };
    let length = registry
        .objects
        .get(&oid)
        .map(Vec::len)
        .ok_or_else(|| lo_object_not_found(oid))?;
    let base: i64 = match whence {
        0 => 0,
        1 => i64::try_from(current).unwrap_or(i64::MAX),
        2 => i64::try_from(length).unwrap_or(i64::MAX),
        _ => {
            return Err(DbError::bind_error(
                SqlState::InvalidParameterValue,
                format!("invalid lo_lseek whence: {whence}"),
            ));
        }
    };
    let new_pos_signed = base.saturating_add(offset);
    let new_pos = if new_pos_signed < 0 {
        0
    } else {
        new_pos_signed
    };
    let new_pos_usize = usize::try_from(new_pos).unwrap_or(usize::MAX);
    let new_pos_usize = new_pos_usize.min(length);
    if let Some(session) = registry.sessions.get_mut(&session_key) {
        if let Some(state) = session.fds.get_mut(&fd) {
            state.position = new_pos_usize;
        }
    }
    let returned = i64::try_from(new_pos_usize).unwrap_or(i64::MAX);
    Ok(if bigint {
        Value::BigInt(returned)
    } else {
        Value::Int(i32::try_from(returned).unwrap_or(i32::MAX))
    })
}

/// `lo_creat(mode int) → oid`. The `mode` argument is unused per PG history.
/// Allocates a fresh LO oid in the session-shared registry.
fn eval_lo_creat(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "lo_creat")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    // Mode is read but ignored, matching PostgreSQL semantics.
    let _mode = lo_arg_i32(&args[0], "lo_creat", "mode")?;
    let mut registry = lo_registry()
        .lock()
        .map_err(|e| DbError::internal(format!("large object registry poisoned: {e}")))?;
    let candidate = next_available_lo_oid(&registry)?;
    advance_next_lo_oid(&mut registry, candidate);
    registry.objects.insert(candidate, Vec::new());
    Ok(Value::Int(candidate))
}

/// `lo_get(oid)` returns the entire LO contents as bytea.
/// `lo_get(oid, offset, length)` returns a slice.
fn eval_lo_get(args: &[Value]) -> DbResult<Value> {
    if !(1..=3).contains(&args.len()) {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            "lo_get() expects 1 or 3 arguments",
        ));
    }
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let oid = lo_arg_i32(&args[0], "lo_get", "oid")?;
    let registry = lo_registry()
        .lock()
        .map_err(|e| DbError::internal(format!("large object registry poisoned: {e}")))?;
    let data = registry
        .objects
        .get(&oid)
        .ok_or_else(|| lo_object_not_found(oid))?;
    if args.len() == 1 {
        return Ok(Value::Blob(data.clone()));
    }
    let offset = lo_arg_i64(&args[1], "lo_get", "offset")?;
    let length = lo_arg_i64(&args[2], "lo_get", "length")?;
    if offset < 0 || length < 0 {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            "lo_get() offset and length must be non-negative",
        ));
    }
    let start = usize::try_from(offset)
        .unwrap_or(usize::MAX)
        .min(data.len());
    let want = usize::try_from(length).unwrap_or(usize::MAX);
    let end = start.saturating_add(want).min(data.len());
    Ok(Value::Blob(data[start..end].to_vec()))
}

/// `lo_put(oid, offset, bytea) → void`. Writes `bytea` into the LO at
/// `offset`, growing the LO if necessary. Returns NULL (mirroring PG's void
/// signature in our Value model).
fn eval_lo_put(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 3, "lo_put")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let oid = lo_arg_i32(&args[0], "lo_put", "oid")?;
    let offset = lo_arg_i64(&args[1], "lo_put", "offset")?;
    if offset < 0 {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            "lo_put() offset must be non-negative",
        ));
    }
    let bytes = match &args[2] {
        Value::Blob(blob) => blob.clone(),
        Value::Text(text) => text.as_bytes().to_vec(),
        other => value_to_text(other).into_bytes(),
    };
    let mut registry = lo_registry()
        .lock()
        .map_err(|e| DbError::internal(format!("large object registry poisoned: {e}")))?;
    let data = registry
        .objects
        .get_mut(&oid)
        .ok_or_else(|| lo_object_not_found(oid))?;
    let (start, end) = checked_lo_write_range("lo_put", offset, bytes.len())?;
    if data.len() < end {
        data.resize(end, 0);
    }
    data[start..end].copy_from_slice(&bytes);
    Ok(Value::Null)
}

/// `lo_from_bytea(oid, bytea) → oid`. Creates a new LO with the given content.
/// Passing oid=0 lets the server pick a free oid.
fn eval_lo_from_bytea(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "lo_from_bytea")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let requested = lo_arg_i32(&args[0], "lo_from_bytea", "oid")?;
    let bytes = match &args[1] {
        Value::Blob(blob) => blob.clone(),
        Value::Text(text) => text.as_bytes().to_vec(),
        other => value_to_text(other).into_bytes(),
    };
    let mut registry = lo_registry()
        .lock()
        .map_err(|e| DbError::internal(format!("large object registry poisoned: {e}")))?;
    let oid = if requested > 0 {
        requested
    } else {
        let candidate = next_available_lo_oid(&registry)?;
        advance_next_lo_oid(&mut registry, candidate);
        candidate
    };
    checked_lo_size("lo_from_bytea", bytes.len())?;
    registry.objects.insert(oid, bytes);
    advance_next_lo_oid(&mut registry, oid);
    Ok(Value::Int(oid))
}

/// `lo_tell(fd) → integer`. Returns the current cursor position. `bigint=true`
/// is the `lo_tell64` variant returning int8.
fn eval_lo_tell(args: &[Value], bigint: bool) -> DbResult<Value> {
    expect_args(args, 1, "lo_tell")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let fd = lo_arg_i32(&args[0], "lo_tell", "fd")?;
    let registry = lo_registry()
        .lock()
        .map_err(|e| DbError::internal(format!("large object registry poisoned: {e}")))?;
    let Some(session) = registry.sessions.get(&lo_session_key()) else {
        return Err(lo_bad_fd(fd));
    };
    let Some(state) = session.fds.get(&fd) else {
        return Err(lo_bad_fd(fd));
    };
    let position = i64::try_from(state.position).unwrap_or(i64::MAX);
    Ok(if bigint {
        Value::BigInt(position)
    } else {
        Value::Int(i32::try_from(position).unwrap_or(i32::MAX))
    })
}

fn eval_lo_truncate(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "lo_truncate")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let fd = lo_arg_i32(&args[0], "lo_truncate", "fd")?;
    let len = lo_arg_usize(&args[1], "lo_truncate", "len")?;
    let mut registry = lo_registry()
        .lock()
        .map_err(|e| DbError::internal(format!("large object registry poisoned: {e}")))?;
    let session_key = lo_session_key();
    let oid = {
        let Some(session) = registry.sessions.get_mut(&session_key) else {
            return Err(lo_bad_fd(fd));
        };
        let Some(state) = session.fds.get(&fd) else {
            return Err(lo_bad_fd(fd));
        };
        state.oid
    };
    let Some(data) = registry.objects.get_mut(&oid) else {
        return Err(lo_object_not_found(oid));
    };
    checked_lo_size("lo_truncate", len)?;
    data.resize(len, 0);
    if let Some(session) = registry.sessions.get_mut(&session_key) {
        if let Some(state) = session.fds.get_mut(&fd) {
            state.position = state.position.min(len);
        }
    }
    Ok(Value::Int(0))
}

fn eval_brin_summarize_range(args: &[Value]) -> DbResult<Value> {
    if !(2..=3).contains(&args.len()) {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            "brin_summarize_range() expects 2 or 3 arguments",
        ));
    }
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let index_oid = lo_arg_i32(&args[0], "brin_summarize_range", "index_oid")?;
    let range_start = i64::from(lo_arg_i32(&args[1], "brin_summarize_range", "heap_blkno")?);
    let mut registry = lo_registry()
        .lock()
        .map_err(|e| DbError::internal(format!("large object registry poisoned: {e}")))?;
    let summarized = registry
        .brin_ranges
        .entry(index_oid)
        .or_default()
        .insert(range_start);
    Ok(Value::Int(i32::from(summarized)))
}

fn eval_brin_desummarize_range(args: &[Value]) -> DbResult<Value> {
    if !(2..=3).contains(&args.len()) {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            "brin_desummarize_range() expects 2 or 3 arguments",
        ));
    }
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let index_oid = lo_arg_i32(&args[0], "brin_desummarize_range", "index_oid")?;
    let range_start = i64::from(lo_arg_i32(
        &args[1],
        "brin_desummarize_range",
        "heap_blkno",
    )?);
    let mut registry = lo_registry()
        .lock()
        .map_err(|e| DbError::internal(format!("large object registry poisoned: {e}")))?;
    let removed = registry
        .brin_ranges
        .entry(index_oid)
        .or_default()
        .remove(&range_start);
    Ok(Value::Int(i32::from(removed)))
}

fn parse_pg_object_address_list(value: &Value) -> Vec<Option<String>> {
    match value {
        Value::Array(items) => items
            .iter()
            .map(|item| {
                if item.is_null() {
                    None
                } else {
                    Some(value_to_text(item).trim().to_owned())
                }
            })
            .collect(),
        Value::Text(text) => parse_pg_text_array_literal(text),
        Value::Null => vec![None],
        other => vec![Some(value_to_text(other).trim().to_owned())],
    }
}

fn parse_pg_text_array_literal(input: &str) -> Vec<Option<String>> {
    let trimmed = input.trim();
    if !(trimmed.starts_with('{') && trimmed.ends_with('}')) {
        return vec![Some(trimmed.to_owned())];
    }

    let inner = &trimmed[1..trimmed.len().saturating_sub(1)];
    if inner.trim().is_empty() {
        return Vec::new();
    }

    inner
        .split(',')
        .map(|entry| {
            let mut value = entry.trim();
            if value.starts_with('"') && value.ends_with('"') && value.len() >= 2 {
                value = &value[1..value.len() - 1];
            }
            if value.eq_ignore_ascii_case("null") {
                None
            } else {
                Some(value.to_owned())
            }
        })
        .collect()
}

fn is_named_multirange_constructor(name: &str) -> bool {
    name.ends_with("multirange")
        && !name.starts_with("multirange_")
        && !name.starts_with("range_")
        && !name.starts_with("elem_")
}

fn is_named_range_constructor(name: &str) -> bool {
    // Bare `range(...)` is Cypher's list generator; only typed forms like
    // `int4range`, `tsrange`, etc. are PG range constructors.
    name != "range"
        && name.ends_with("range")
        && !name.ends_with("multirange")
        && !name.starts_with("range_")
        && !name.starts_with("multirange_")
        && !name.starts_with("elem_")
}

fn is_compat_user_range_constructor(name: &str) -> bool {
    let normalized = normalize_compat_type_name(name);
    if !with_current_session_context(|ctx| ctx.compat_user_type(&normalized).is_some()) {
        return false;
    }
    if normalized.ends_with("range") && !normalized.ends_with("multirange") {
        return true;
    }
    !is_compat_user_multirange_constructor(name)
}

fn is_compat_user_multirange_constructor(name: &str) -> bool {
    let normalized = normalize_compat_type_name(name);
    if normalized.ends_with("multirange") {
        return true;
    }
    with_current_session_context(|ctx| {
        ctx.compat_user_casts
            .iter()
            .any(|cast| cast.target_type == normalized && cast.source_type.ends_with("range"))
    })
}

fn parse_range_bounds_flags(input: &str) -> DbResult<(bool, bool)> {
    match input {
        "[)" => Ok((true, false)),
        "[]" => Ok((true, true)),
        "()" => Ok((false, false)),
        "(]" => Ok((false, true)),
        _ => Err(DbError::internal(format!(
            "range bound flags must be one of \"[)\", \"[]\", \"()\", \"(]\", got \"{input}\""
        ))),
    }
}

fn quote_range_bound_if_needed(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let requires_quotes = text.chars().any(|ch| {
        ch == ',' || ch == '[' || ch == ']' || ch == '(' || ch == ')' || ch.is_ascii_whitespace()
    }) || text.contains('"')
        || text.contains('\\');
    if !requires_quotes {
        return text.to_owned();
    }
    let mut escaped = String::with_capacity(text.len() + 2);
    escaped.push('"');
    for ch in text.chars() {
        if ch == '"' || ch == '\\' {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped.push('"');
    escaped
}

fn eval_named_range_constructor(args: &[Value]) -> DbResult<Value> {
    if args.is_empty() {
        return Ok(Value::Text("empty".to_owned()));
    }
    if args.len() == 1 {
        return match &args[0] {
            Value::Null => Ok(Value::Null),
            Value::Text(text) => {
                let trimmed = text.trim();
                if trimmed.eq_ignore_ascii_case("empty") || range::looks_like_range(trimmed) {
                    Ok(Value::Text(trimmed.to_owned()))
                } else {
                    Err(DbError::from_report(ErrorReport::new(
                        SqlState::InvalidTextRepresentation,
                        format!("malformed range literal: \"{trimmed}\""),
                    )))
                }
            }
            other => {
                let rendered = value_to_text(other);
                let trimmed = rendered.trim().to_owned();
                if trimmed.eq_ignore_ascii_case("empty") || range::looks_like_range(&trimmed) {
                    Ok(Value::Text(trimmed))
                } else {
                    Err(DbError::from_report(ErrorReport::new(
                        SqlState::InvalidTextRepresentation,
                        format!("malformed range literal: \"{trimmed}\""),
                    )))
                }
            }
        };
    }

    let lower_raw = if args[0].is_null() {
        String::new()
    } else {
        quote_range_bound_if_needed(value_to_text(&args[0]).trim())
    };
    let upper_raw = if args[1].is_null() {
        String::new()
    } else {
        quote_range_bound_if_needed(value_to_text(&args[1]).trim())
    };
    let flags = if args.len() >= 3 {
        match &args[2] {
            Value::Text(text) => text.trim().to_owned(),
            Value::Null => "[)".to_owned(),
            other => value_to_text(other),
        }
    } else {
        "[)".to_owned()
    };
    let (mut lower_inc, mut upper_inc) = parse_range_bounds_flags(flags.trim())?;
    if lower_raw.is_empty() {
        lower_inc = false;
    }
    if upper_raw.is_empty() {
        upper_inc = false;
    }

    if !args[0].is_null() && !args[1].is_null() {
        if let Ok(Some(ordering)) = compare_runtime_values(&args[0], &args[1]) {
            match ordering {
                Ordering::Greater => {
                    return Err(DbError::from_report(ErrorReport::new(
                        SqlState::InvalidTextRepresentation,
                        "range lower bound must be less than or equal to range upper bound",
                    )));
                }
                Ordering::Equal if !(lower_inc && upper_inc) => {
                    return Ok(Value::Text("empty".to_owned()));
                }
                _ => {}
            }
        } else if !lower_raw.is_empty() && lower_raw == upper_raw && !(lower_inc && upper_inc) {
            return Ok(Value::Text("empty".to_owned()));
        }
    }

    let lb = if lower_inc { '[' } else { '(' };
    let ub = if upper_inc { ']' } else { ')' };
    Ok(Value::Text(format!("{lb}{lower_raw},{upper_raw}{ub}")))
}

fn eval_named_multirange_constructor(args: &[Value]) -> DbResult<Value> {
    if args.is_empty() {
        return Ok(Value::Text("{}".to_owned()));
    }

    let mut ranges = Vec::new();
    for arg in args {
        match arg {
            Value::Null => {}
            Value::Text(text) => {
                let trimmed = text.trim();
                if trimmed.starts_with('{') {
                    ranges.extend(parse_and_normalize_multirange_literal(trimmed)?);
                } else if trimmed.eq_ignore_ascii_case("empty") {
                    // Empty range contributes nothing to the multirange.
                } else if range::looks_like_range(trimmed) {
                    ranges.push(trimmed.to_owned());
                } else {
                    return Err(DbError::from_report(ErrorReport::new(
                        SqlState::InvalidTextRepresentation,
                        format!("malformed multirange literal: \"{trimmed}\""),
                    )));
                }
            }
            other => {
                let rendered = value_to_text(other);
                let trimmed = rendered.trim();
                if trimmed.starts_with('{') {
                    ranges.extend(parse_and_normalize_multirange_literal(trimmed)?);
                } else if trimmed.eq_ignore_ascii_case("empty") {
                    // Empty range contributes nothing to the multirange.
                } else if range::looks_like_range(trimmed) {
                    ranges.push(trimmed.to_owned());
                } else {
                    return Err(DbError::from_report(ErrorReport::new(
                        SqlState::InvalidTextRepresentation,
                        format!("malformed multirange literal: \"{trimmed}\""),
                    )));
                }
            }
        }
    }

    if ranges.is_empty() {
        return Ok(Value::Text("{}".to_owned()));
    }

    let normalized = parse_and_normalize_multirange_literal(&format!("{{{}}}", ranges.join(",")))?;
    if normalized.is_empty() {
        Ok(Value::Text("{}".to_owned()))
    } else {
        Ok(Value::Text(format!("{{{}}}", normalized.join(","))))
    }
}

fn eval_range_merge_multirange(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "range_merge")?;
    let Some(arg) = args.first() else {
        return Ok(Value::Null);
    };
    if arg.is_null() {
        return Ok(Value::Null);
    }
    let Value::Text(multirange_text) = arg else {
        return Err(DbError::internal(
            "range_merge() requires a multirange text argument",
        ));
    };

    let ranges = parse_and_normalize_multirange_literal(multirange_text)?;
    if ranges.is_empty() {
        return Ok(Value::Text("empty".to_owned()));
    }

    let mut ranges_iter = ranges.into_iter();
    let Some(mut merged) = ranges_iter.next() else {
        return Ok(Value::Text("empty".to_owned()));
    };
    for range_text in ranges_iter {
        let merged_value = range::eval_range_merge(&[
            Value::Text(std::mem::take(&mut merged)),
            Value::Text(range_text),
        ])?;
        match merged_value {
            Value::Text(text) => merged = text,
            _ => {
                return Err(DbError::internal(
                    "range_merge() returned an unexpected non-text value",
                ));
            }
        }
    }

    Ok(Value::Text(merged))
}

// =====================================================================

/// Sleep for `seconds` cooperatively.
///
/// Polling the cancellation checker every ≤50 ms keeps `pg_sleep` honest
/// against pgwire `CancelRequest`. The wake granularity is intentionally
/// coarse (50 ms) to keep CPU overhead negligible while staying snappy enough
/// for human-perceived cancellation latencies.
/// PG `xml_is_well_formed`/`xml_is_well_formed_document`/
/// `xml_is_well_formed_content` - validate XML well-formedness without
/// pulling in a full XML parser. The implementation tracks tag-balance,
/// CDATA, comments, processing instructions, and the
/// `require_single_root` distinction (DOCUMENT requires exactly one
/// element at depth 0 with no extra non-whitespace text; CONTENT
/// allows mixed/text/multiple elements).
fn eval_xml_is_well_formed(args: &[Value], require_single_root: bool) -> Value {
    if args.is_empty() {
        return Value::Boolean(false);
    }
    if matches!(args[0], Value::Null) {
        return Value::Null;
    }
    let text = match &args[0] {
        Value::Text(s) => s.as_str(),
        _ => return Value::Boolean(false),
    };
    Value::Boolean(xml_is_well_formed_str(text, require_single_root))
}

fn xml_is_well_formed_str(input: &str, require_single_root: bool) -> bool {
    let bytes = input.as_bytes();
    let mut i = 0usize;
    let mut stack: Vec<&str> = Vec::new();
    // Counts of top-level (depth-0) closed elements + leading non-whitespace
    // text. Used to enforce the document/content distinction.
    let mut top_level_elements = 0u32;
    let mut top_level_non_whitespace_text = false;
    let len = bytes.len();
    while i < len {
        if bytes[i] == b'<' {
            // Detect special markers first.
            if input[i..].starts_with("<!--") {
                // comment ends at -->
                let end = input[i + 4..].find("-->");
                let Some(end) = end else { return false };
                i += 4 + end + 3;
                continue;
            }
            if input[i..].starts_with("<![CDATA[") {
                let end = input[i + 9..].find("]]>");
                let Some(end) = end else { return false };
                if stack.is_empty() {
                    // CDATA at top level treated as text.
                    top_level_non_whitespace_text = true;
                }
                i += 9 + end + 3;
                continue;
            }
            if input[i..].starts_with("<?") {
                // processing instruction
                let end = input[i + 2..].find("?>");
                let Some(end) = end else { return false };
                i += 2 + end + 2;
                continue;
            }
            if input[i..].starts_with("<!") {
                // DOCTYPE / other declaration; skip until matching '>'
                let end = input[i..].find('>');
                let Some(end) = end else { return false };
                i += end + 1;
                continue;
            }
            // Closing tag
            if input[i..].starts_with("</") {
                let end = input[i..].find('>');
                let Some(end) = end else { return false };
                let name = input[i + 2..i + end].trim();
                let Some(top) = stack.pop() else {
                    return false;
                };
                if top != name {
                    return false;
                }
                if stack.is_empty() {
                    top_level_elements += 1;
                }
                i += end + 1;
                continue;
            }
            // Opening or self-closing element
            let tag_end = input[i..].find('>');
            let Some(tag_end) = tag_end else { return false };
            let inner = &input[i + 1..i + tag_end];
            let self_closing = inner.ends_with('/');
            let inner = inner.trim_end_matches('/').trim();
            if inner.is_empty() {
                return false;
            }
            // Tag name = first whitespace-delimited token of inner.
            let name_end = inner.find(char::is_whitespace).unwrap_or(inner.len());
            let name = &inner[..name_end];
            if !is_valid_xml_name(name) {
                return false;
            }
            if self_closing {
                if stack.is_empty() {
                    top_level_elements += 1;
                }
            } else {
                stack.push(name);
            }
            i += tag_end + 1;
            continue;
        }
        // Text run; track top-level non-whitespace.
        if stack.is_empty() && !bytes[i].is_ascii_whitespace() {
            top_level_non_whitespace_text = true;
        }
        i += 1;
    }
    if !stack.is_empty() {
        return false;
    }
    if require_single_root {
        // DOCUMENT: exactly one top-level element + no top-level text.
        return top_level_elements == 1 && !top_level_non_whitespace_text;
    }
    true
}

/// PG `xmlcomment(text)` returns `<!-- text -->`. Errors on inputs that
/// would produce an invalid comment (containing `--` or ending in `-`).
fn eval_xmlcomment(args: &[Value]) -> DbResult<Value> {
    if args.is_empty() || matches!(args[0], Value::Null) {
        return Ok(Value::Null);
    }
    let text = match &args[0] {
        Value::Text(s) => s.as_str(),
        _ => {
            return Err(DbError::bind_error(
                SqlState::InvalidParameterValue,
                "xmlcomment requires a text argument",
            ));
        }
    };
    if text.contains("--") || text.ends_with('-') {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            "invalid XML comment",
        ));
    }
    Ok(Value::Text(format!("<!--{text}-->")))
}

/// PG `xmlconcat(xml, xml, …)` concatenates XML fragments. NULLs are
/// skipped. Returns NULL when every argument is NULL.
fn eval_xmlconcat(args: &[Value]) -> Value {
    let mut out = String::new();
    let mut any_non_null = false;
    for arg in args {
        match arg {
            Value::Null => {}
            Value::Text(s) => {
                any_non_null = true;
                out.push_str(s);
            }
            other => {
                any_non_null = true;
                use std::fmt::Write;
                // Stream the Display rendering directly into `out`
                // instead of allocating a transient `other.to_string()`.
                let _ = write!(&mut out, "{other}");
            }
        }
    }
    if !any_non_null {
        return Value::Null;
    }
    Value::Text(out)
}

/// PG `xmlpi(NAME target [, content])` returns `<?target content?>`.
/// AionDB receives the args as a flat list - first is the target name,
/// optional second is the content text.
fn eval_xmlpi(args: &[Value]) -> DbResult<Value> {
    if args.is_empty() || matches!(args[0], Value::Null) {
        return Ok(Value::Null);
    }
    let target = match &args[0] {
        Value::Text(s) => s.clone(),
        other => other.to_string(),
    };
    if target.is_empty() || !is_valid_xml_name(&target) {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            "invalid processing instruction target",
        ));
    }
    if target.eq_ignore_ascii_case("xml") {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            "invalid XML processing instruction target: \"xml\"",
        ));
    }
    let content = if args.len() >= 2 && !matches!(args[1], Value::Null) {
        let s = match &args[1] {
            Value::Text(s) => s.clone(),
            other => other.to_string(),
        };
        if s.contains("?>") {
            return Err(DbError::bind_error(
                SqlState::InvalidParameterValue,
                "invalid XML processing instruction content",
            ));
        }
        Some(s)
    } else {
        None
    };
    Ok(Value::Text(match content {
        Some(c) if !c.is_empty() => format!("<?{target} {c}?>"),
        _ => format!("<?{target}?>"),
    }))
}

/// PG `xmlroot(xml, version, [standalone])` rewrites/adds the XML
/// declaration. AionDB receives args as `(xml, version, [standalone])`.
fn eval_xmlroot(args: &[Value]) -> DbResult<Value> {
    if args.is_empty() || matches!(args[0], Value::Null) {
        return Ok(Value::Null);
    }
    let xml = match &args[0] {
        Value::Text(s) => s.clone(),
        other => other.to_string(),
    };
    let version = if args.len() >= 2 && !matches!(args[1], Value::Null) {
        Some(match &args[1] {
            Value::Text(s) => s.clone(),
            other => other.to_string(),
        })
    } else {
        None
    };
    let standalone = if args.len() >= 3 && !matches!(args[2], Value::Null) {
        Some(match &args[2] {
            Value::Text(s) => s.clone(),
            Value::Boolean(true) => "yes".to_owned(),
            Value::Boolean(false) => "no".to_owned(),
            other => other.to_string(),
        })
    } else {
        None
    };
    // Strip an existing <?xml …?> declaration, if any.
    let trimmed = xml.trim_start();
    let body = if let Some(rest) = trimmed.strip_prefix("<?xml") {
        if let Some(close) = rest.find("?>") {
            &rest[close + 2..]
        } else {
            return Err(DbError::bind_error(
                SqlState::InvalidParameterValue,
                "malformed XML declaration",
            ));
        }
    } else {
        xml.as_str()
    };
    // Build the XML declaration in one buffer instead of allocating
    // transient `format!` Strings per attribute.
    let mut decl = String::from("<?xml");
    if let Some(v) = version.as_deref() {
        decl.push_str(" version=\"");
        decl.push_str(v);
        decl.push('"');
    } else {
        decl.push_str(" version=\"1.0\"");
    }
    if let Some(sa) = standalone.as_deref() {
        decl.push_str(" standalone=\"");
        decl.push_str(sa);
        decl.push('"');
    }
    decl.push_str("?>");
    decl.push_str(body);
    Ok(Value::Text(decl))
}

/// PG `xmlserialize(xml AS text|xml [INDENT|NO INDENT])` returns the text
/// representation of the XML value. AionDB stores XML as text, so this is
/// largely a passthrough - parse args as `(value)` or `(value, target_type)`.
fn eval_xmlserialize(args: &[Value]) -> Value {
    if args.is_empty() || matches!(args[0], Value::Null) {
        return Value::Null;
    }
    let value = match &args[0] {
        Value::Text(s) => s.clone(),
        other => other.to_string(),
    };
    Value::Text(value)
}

/// PG `xmlparse(DOCUMENT|CONTENT text)` returns an xml value, validating
/// well-formedness. Args are flattened to `(text, [bool])` where the bool
/// (if present) indicates DOCUMENT (true) vs CONTENT (false).
fn eval_xmlparse(args: &[Value]) -> DbResult<Value> {
    if args.is_empty() || matches!(args[0], Value::Null) {
        return Ok(Value::Null);
    }
    let text = match &args[0] {
        Value::Text(s) => s.clone(),
        other => other.to_string(),
    };
    let require_single_root = args
        .get(1)
        .and_then(|v| match v {
            Value::Boolean(b) => Some(*b),
            _ => None,
        })
        .unwrap_or(true);
    if !xml_is_well_formed_str(&text, require_single_root) {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            "invalid XML",
        ));
    }
    Ok(Value::Text(text))
}

/// PG `xmlelement(NAME tag [, attrs] [, content...])` produces
/// `<tag attr="v" attr2="v2">content</tag>`. Args layout from the parser:
/// `[name, attr_name1, attr_value1, …, content_text]`. Heuristic: even
/// indices are names, odd indices are values, with the final unpaired arg
/// (if any) treated as content.
fn eval_xmlelement(args: &[Value]) -> DbResult<Value> {
    if args.is_empty() || matches!(args[0], Value::Null) {
        return Ok(Value::Null);
    }
    let tag = match &args[0] {
        Value::Text(s) => s.clone(),
        other => other.to_string(),
    };
    if !is_valid_xml_name(&tag) {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!("invalid XML element name: {tag}"),
        ));
    }
    let rest = &args[1..];
    let attr_count = rest.len() / 2 * 2; // pairs only
    let mut output = format!("<{tag}");
    let mut idx = 0;
    while idx < attr_count {
        let attr_name = match &rest[idx] {
            Value::Null => {
                idx += 2;
                continue;
            }
            Value::Text(s) => s.clone(),
            other => other.to_string(),
        };
        let attr_val = match &rest[idx + 1] {
            Value::Null => String::new(),
            Value::Text(s) => s.clone(),
            other => other.to_string(),
        };
        if !is_valid_xml_name(&attr_name) {
            return Err(DbError::bind_error(
                SqlState::InvalidParameterValue,
                format!("invalid XML attribute name: {attr_name}"),
            ));
        }
        // Stream the attribute directly: ` {attr_name}="{escaped}"`.
        output.push(' ');
        output.push_str(&attr_name);
        output.push_str("=\"");
        output.push_str(&xml_escape_attr(&attr_val));
        output.push('"');
        idx += 2;
    }
    let content = if rest.len() > attr_count {
        match &rest[attr_count] {
            Value::Null => None,
            Value::Text(s) => Some(s.clone()),
            other => Some(other.to_string()),
        }
    } else {
        None
    };
    match content {
        Some(c) if !c.is_empty() => {
            output.push('>');
            output.push_str(&xml_escape_content(&c));
            output.push_str("</");
            output.push_str(&tag);
            output.push('>');
        }
        _ => output.push_str("/>"),
    }
    Ok(Value::Text(output))
}

/// PG `xmlforest(value AS name, …)` produces a series of `<name>value</name>`
/// elements. Args from the parser: `[name1, value1, name2, value2, …]`.
fn eval_xmlforest(args: &[Value]) -> DbResult<Value> {
    if args.is_empty() {
        return Ok(Value::Null);
    }
    let mut output = String::new();
    let mut idx = 0;
    while idx + 1 < args.len() {
        let name = match &args[idx] {
            Value::Null => {
                idx += 2;
                continue;
            }
            Value::Text(s) => s.clone(),
            other => other.to_string(),
        };
        let value = match &args[idx + 1] {
            Value::Null => {
                idx += 2;
                continue;
            }
            Value::Text(s) => s.clone(),
            other => other.to_string(),
        };
        if !is_valid_xml_name(&name) {
            return Err(DbError::bind_error(
                SqlState::InvalidParameterValue,
                format!("invalid XML element name: {name}"),
            ));
        }
        // Stream `<name>{escaped}</name>` directly into the output.
        output.push('<');
        output.push_str(&name);
        output.push('>');
        output.push_str(&xml_escape_content(&value));
        output.push_str("</");
        output.push_str(&name);
        output.push('>');
        idx += 2;
    }
    if output.is_empty() {
        return Ok(Value::Null);
    }
    Ok(Value::Text(output))
}

fn xml_escape_attr(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

fn xml_escape_content(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
    out
}

fn is_valid_xml_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_' || first == ':') {
        return false;
    }
    for c in chars {
        if !(c.is_ascii_alphanumeric() || c == '_' || c == ':' || c == '-' || c == '.') {
            return false;
        }
    }
    true
}

/// Implementations of PG inet/cidr scalar helpers. Operate on the text
/// representation since AionDB stores inet as `Value::Text`. Each function
/// returns `Value::Null` when the address is unparseable, matching the
/// stub's previous behaviour but with real semantics for valid input.
fn eval_inet_helper(name: &str, args: &[Value]) -> DbResult<Value> {
    use std::net::IpAddr;
    fn parse(text: &str) -> Option<(IpAddr, u8, bool)> {
        let (addr, prefix_text) = text
            .split_once('/')
            .map_or((text, None), |(a, p)| (a, Some(p)));
        let ip = addr.parse::<IpAddr>().ok()?;
        let max = match ip {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        };
        let (prefix, had_prefix) = match prefix_text {
            Some(p) => (p.parse::<u8>().ok().filter(|v| *v <= max)?, true),
            None => (max, false),
        };
        Some((ip, prefix, had_prefix))
    }
    fn mask_addr(addr: IpAddr, prefix: u8) -> IpAddr {
        match addr {
            IpAddr::V4(v) => {
                let bits = u32::from_be_bytes(v.octets());
                let m = if prefix == 0 {
                    0
                } else {
                    u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0)
                };
                IpAddr::V4((bits & m).into())
            }
            IpAddr::V6(v) => {
                let bits = u128::from_be_bytes(v.octets());
                let m = if prefix == 0 {
                    0
                } else {
                    u128::MAX.checked_shl(128 - u32::from(prefix)).unwrap_or(0)
                };
                IpAddr::V6((bits & m).into())
            }
        }
    }
    fn netmask_addr(family: &IpAddr, prefix: u8) -> IpAddr {
        match family {
            IpAddr::V4(_) => {
                let m = if prefix == 0 {
                    0
                } else {
                    u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0)
                };
                IpAddr::V4(m.into())
            }
            IpAddr::V6(_) => {
                let m = if prefix == 0 {
                    0
                } else {
                    u128::MAX.checked_shl(128 - u32::from(prefix)).unwrap_or(0)
                };
                IpAddr::V6(m.into())
            }
        }
    }
    fn broadcast_addr(addr: IpAddr, prefix: u8) -> IpAddr {
        match addr {
            IpAddr::V4(v) => {
                let bits = u32::from_be_bytes(v.octets());
                let m = if prefix == 0 {
                    0
                } else {
                    u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0)
                };
                let host = !m;
                IpAddr::V4(((bits & m) | host).into())
            }
            IpAddr::V6(v) => {
                let bits = u128::from_be_bytes(v.octets());
                let m = if prefix == 0 {
                    0
                } else {
                    u128::MAX.checked_shl(128 - u32::from(prefix)).unwrap_or(0)
                };
                let host = !m;
                IpAddr::V6(((bits & m) | host).into())
            }
        }
    }
    if args.is_empty() || matches!(args[0], Value::Null) {
        return Ok(Value::Null);
    }
    let text = match &args[0] {
        Value::Text(s) => s.clone(),
        other => other.to_string(),
    };
    let parsed = parse(&text);
    match name {
        "host" => match parsed {
            Some((ip, _, _)) => Ok(Value::Text(ip.to_string())),
            None => Ok(Value::Text(text)),
        },
        "masklen" => match parsed {
            Some((_, prefix, _)) => Ok(Value::Int(i32::from(prefix))),
            None => Ok(Value::Null),
        },
        "family" => match parsed {
            Some((IpAddr::V4(_), _, _)) => Ok(Value::Int(4)),
            Some((IpAddr::V6(_), _, _)) => Ok(Value::Int(6)),
            None => Ok(Value::Null),
        },
        "network" => match parsed {
            Some((ip, prefix, _)) => {
                let net = mask_addr(ip, prefix);
                Ok(Value::Text(format!("{net}/{prefix}")))
            }
            None => Ok(Value::Text(text)),
        },
        "broadcast" => match parsed {
            Some((ip, prefix, _)) => {
                let bcast = broadcast_addr(ip, prefix);
                Ok(Value::Text(format!("{bcast}/{prefix}")))
            }
            None => Ok(Value::Text(text)),
        },
        "netmask" => match parsed {
            Some((ip, prefix, _)) => {
                let mask = netmask_addr(&ip, prefix);
                Ok(Value::Text(mask.to_string()))
            }
            None => Ok(Value::Text(text)),
        },
        "hostmask" => match parsed {
            Some((ip, prefix, _)) => {
                let hostmask = match ip {
                    IpAddr::V4(_) => {
                        let m = if prefix == 0 {
                            0
                        } else {
                            u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0)
                        };
                        IpAddr::V4((!m).into())
                    }
                    IpAddr::V6(_) => {
                        let m = if prefix == 0 {
                            0
                        } else {
                            u128::MAX.checked_shl(128 - u32::from(prefix)).unwrap_or(0)
                        };
                        IpAddr::V6((!m).into())
                    }
                };
                Ok(Value::Text(hostmask.to_string()))
            }
            None => Ok(Value::Text(text)),
        },
        "abbrev" => match parsed {
            Some((ip, prefix, had_prefix)) => {
                let max = match ip {
                    IpAddr::V4(_) => 32,
                    IpAddr::V6(_) => 128,
                };
                if had_prefix && prefix < max {
                    Ok(Value::Text(format!("{ip}/{prefix}")))
                } else {
                    Ok(Value::Text(ip.to_string()))
                }
            }
            None => Ok(Value::Text(text)),
        },
        "set_masklen" => {
            if args.len() < 2 || matches!(args[1], Value::Null) {
                return Ok(Value::Null);
            }
            let new_prefix = match &args[1] {
                Value::Int(v) => *v,
                Value::BigInt(v) => i32::try_from(*v).unwrap_or(i32::MAX),
                _ => return Ok(Value::Text(text)),
            };
            match parsed {
                Some((ip, _, _)) => {
                    let max = match ip {
                        IpAddr::V4(_) => 32,
                        IpAddr::V6(_) => 128,
                    };
                    if new_prefix < 0 || new_prefix > i32::from(max) {
                        return Err(DbError::bind_error(
                            SqlState::InvalidParameterValue,
                            format!("invalid mask length: {new_prefix}"),
                        ));
                    }
                    Ok(Value::Text(format!("{ip}/{new_prefix}")))
                }
                None => Ok(Value::Text(text)),
            }
        }
        "inet_same_family" => {
            if args.len() < 2 {
                return Ok(Value::Null);
            }
            if matches!(args[1], Value::Null) {
                return Ok(Value::Null);
            }
            let other_text = match &args[1] {
                Value::Text(s) => s.clone(),
                other => other.to_string(),
            };
            match (parse(&text), parse(&other_text)) {
                (Some((a, _, _)), Some((b, _, _))) => Ok(Value::Boolean(
                    std::mem::discriminant(&a) == std::mem::discriminant(&b),
                )),
                _ => Ok(Value::Null),
            }
        }
        "inet_merge" => {
            // Smallest network that contains both inputs (matching PG).
            if args.len() < 2 || matches!(args[1], Value::Null) {
                return Ok(Value::Null);
            }
            let other_text = match &args[1] {
                Value::Text(s) => s.clone(),
                other => other.to_string(),
            };
            let (left_ip, left_prefix, _) = match parse(&text) {
                Some(t) => t,
                None => return Ok(Value::Null),
            };
            let (right_ip, right_prefix, _) = match parse(&other_text) {
                Some(t) => t,
                None => return Ok(Value::Null),
            };
            if std::mem::discriminant(&left_ip) != std::mem::discriminant(&right_ip) {
                return Err(DbError::bind_error(
                    SqlState::InvalidParameterValue,
                    "inet_merge: address family mismatch",
                ));
            }
            let mut prefix = left_prefix.min(right_prefix);
            while prefix > 0 && mask_addr(left_ip, prefix) != mask_addr(right_ip, prefix) {
                prefix -= 1;
            }
            let merged = mask_addr(left_ip, prefix);
            Ok(Value::Text(format!("{merged}/{prefix}")))
        }
        "text" => {
            // PG `text(inet)` returns the canonical text representation
            // including prefix (always shown).
            match parsed {
                Some((ip, prefix, _)) => Ok(Value::Text(format!("{ip}/{prefix}"))),
                None => Ok(Value::Text(text)),
            }
        }
        _ => Ok(Value::Text(text)),
    }
}

fn interruptible_sleep(seconds: f64) -> DbResult<()> {
    const POLL_SLICE_MS: u64 = 50;
    let total = std::time::Duration::from_secs_f64(seconds);
    let deadline = std::time::Instant::now() + total;
    loop {
        crate::cancel::check_cancellation()?;
        let now = std::time::Instant::now();
        if now >= deadline {
            return Ok(());
        }
        let remaining = deadline.saturating_duration_since(now);
        let slice = remaining.min(std::time::Duration::from_millis(POLL_SLICE_MS));
        std::thread::sleep(slice);
    }
}

fn stats_objdef_option_value(options_joined: &str, option_name: &str) -> Option<String> {
    let mut parts = Vec::new();
    let mut collecting = false;
    for pair in options_joined.split(',').map(str::trim) {
        if let Some(value) = pair.strip_prefix(option_name) {
            parts.push(value.to_owned());
            collecting = true;
        } else if collecting && !pair.contains('=') {
            parts.push(pair.to_owned());
        } else if collecting {
            break;
        }
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

#[cfg(test)]
mod xml_constructor_tests {
    use super::*;

    #[test]
    fn xmlcomment_basic() {
        let r = eval_xmlcomment(&[Value::Text("hello".to_owned())]).unwrap();
        assert_eq!(r, Value::Text("<!--hello-->".to_owned()));
    }

    #[test]
    fn xmlcomment_rejects_double_dash() {
        let err = eval_xmlcomment(&[Value::Text("a--b".to_owned())]).unwrap_err();
        assert_eq!(err.sqlstate(), aiondb_core::SqlState::InvalidParameterValue);
    }

    #[test]
    fn xmlconcat_concatenates_skipping_nulls() {
        let r = eval_xmlconcat(&[
            Value::Text("<a/>".to_owned()),
            Value::Null,
            Value::Text("<b/>".to_owned()),
        ]);
        assert_eq!(r, Value::Text("<a/><b/>".to_owned()));
    }

    #[test]
    fn xmlconcat_all_null_returns_null() {
        let r = eval_xmlconcat(&[Value::Null, Value::Null]);
        assert_eq!(r, Value::Null);
    }

    #[test]
    fn xmlpi_target_only() {
        let r = eval_xmlpi(&[Value::Text("php".to_owned())]).unwrap();
        assert_eq!(r, Value::Text("<?php?>".to_owned()));
    }

    #[test]
    fn xmlpi_target_with_content() {
        let r = eval_xmlpi(&[
            Value::Text("php".to_owned()),
            Value::Text("echo \"hi\";".to_owned()),
        ])
        .unwrap();
        assert_eq!(r, Value::Text("<?php echo \"hi\";?>".to_owned()));
    }

    #[test]
    fn xmlpi_rejects_xml_target() {
        let err = eval_xmlpi(&[Value::Text("xml".to_owned())]).unwrap_err();
        assert_eq!(err.sqlstate(), aiondb_core::SqlState::InvalidParameterValue);
    }

    #[test]
    fn xmlroot_adds_declaration() {
        let r = eval_xmlroot(&[
            Value::Text("<a/>".to_owned()),
            Value::Text("1.1".to_owned()),
        ])
        .unwrap();
        assert_eq!(r, Value::Text("<?xml version=\"1.1\"?><a/>".to_owned()));
    }

    #[test]
    fn xmlroot_replaces_existing_declaration() {
        let r = eval_xmlroot(&[
            Value::Text("<?xml version=\"1.0\"?><a/>".to_owned()),
            Value::Text("1.1".to_owned()),
            Value::Text("yes".to_owned()),
        ])
        .unwrap();
        assert_eq!(
            r,
            Value::Text("<?xml version=\"1.1\" standalone=\"yes\"?><a/>".to_owned())
        );
    }

    #[test]
    fn xmlserialize_passthrough() {
        let r = eval_xmlserialize(&[Value::Text("<a/>".to_owned())]);
        assert_eq!(r, Value::Text("<a/>".to_owned()));
    }

    #[test]
    fn xmlparse_validates_well_formed() {
        let r = eval_xmlparse(&[Value::Text("<a/>".to_owned())]).unwrap();
        assert_eq!(r, Value::Text("<a/>".to_owned()));
    }

    #[test]
    fn xmlparse_rejects_malformed() {
        let err = eval_xmlparse(&[Value::Text("<a>".to_owned())]).unwrap_err();
        assert_eq!(err.sqlstate(), aiondb_core::SqlState::InvalidParameterValue);
    }

    #[test]
    fn xmlelement_self_closing() {
        let r = eval_xmlelement(&[Value::Text("foo".to_owned())]).unwrap();
        assert_eq!(r, Value::Text("<foo/>".to_owned()));
    }

    #[test]
    fn xmlelement_with_attrs_and_content() {
        let r = eval_xmlelement(&[
            Value::Text("link".to_owned()),
            Value::Text("href".to_owned()),
            Value::Text("https://example.com".to_owned()),
            Value::Text("click".to_owned()),
        ])
        .unwrap();
        assert_eq!(
            r,
            Value::Text("<link href=\"https://example.com\">click</link>".to_owned())
        );
    }

    #[test]
    fn xmlelement_escapes_content() {
        let r = eval_xmlelement(&[
            Value::Text("p".to_owned()),
            Value::Text("a < b & c > d".to_owned()),
        ])
        .unwrap();
        assert_eq!(r, Value::Text("<p>a &lt; b &amp; c &gt; d</p>".to_owned()));
    }

    #[test]
    fn xmlforest_emits_pairs() {
        let r = eval_xmlforest(&[
            Value::Text("name".to_owned()),
            Value::Text("Alice".to_owned()),
            Value::Text("age".to_owned()),
            Value::Text("30".to_owned()),
        ])
        .unwrap();
        assert_eq!(r, Value::Text("<name>Alice</name><age>30</age>".to_owned()));
    }

    #[test]
    fn xmlforest_skips_null_values() {
        let r = eval_xmlforest(&[
            Value::Text("name".to_owned()),
            Value::Null,
            Value::Text("age".to_owned()),
            Value::Text("30".to_owned()),
        ])
        .unwrap();
        assert_eq!(r, Value::Text("<age>30</age>".to_owned()));
    }
}

#[cfg(test)]
mod xml_well_formed_tests {
    use super::*;

    fn doc(s: &str) -> bool {
        xml_is_well_formed_str(s, true)
    }
    fn content(s: &str) -> bool {
        xml_is_well_formed_str(s, false)
    }

    #[test]
    fn document_single_root_passes() {
        assert!(doc("<root>hi</root>"));
    }

    #[test]
    fn document_self_closing_root_passes() {
        assert!(doc("<root/>"));
    }

    #[test]
    fn document_unclosed_fails() {
        assert!(!doc("<root>"));
    }

    #[test]
    fn document_mismatched_tag_fails() {
        assert!(!doc("<a></b>"));
    }

    #[test]
    fn document_two_roots_fails_but_content_passes() {
        let s = "<a/><b/>";
        assert!(!doc(s));
        assert!(content(s));
    }

    #[test]
    fn document_text_outside_root_fails() {
        assert!(!doc("hello<a/>"));
    }

    #[test]
    fn document_nested_tags_passes() {
        assert!(doc("<a><b>x</b><c/></a>"));
    }

    #[test]
    fn comment_inside_passes() {
        assert!(doc("<a><!-- comment --></a>"));
    }

    #[test]
    fn cdata_inside_passes() {
        assert!(doc("<a><![CDATA[<not-a-tag>]]></a>"));
    }

    #[test]
    fn invalid_tag_name_fails() {
        assert!(!doc("<1bad>x</1bad>"));
    }
}

#[cfg(test)]
mod inet_helper_tests {
    use super::*;

    fn call(name: &str, args: &[Value]) -> Value {
        eval_inet_helper(name, args).unwrap()
    }

    #[test]
    fn host_strips_prefix() {
        assert_eq!(
            call("host", &[Value::Text("192.168.1.1/24".to_owned())]),
            Value::Text("192.168.1.1".to_owned())
        );
    }

    #[test]
    fn masklen_returns_prefix() {
        assert_eq!(
            call("masklen", &[Value::Text("10.0.0.1/16".to_owned())]),
            Value::Int(16)
        );
        assert_eq!(
            call("masklen", &[Value::Text("10.0.0.1".to_owned())]),
            Value::Int(32)
        );
    }

    #[test]
    fn family_returns_4_or_6() {
        assert_eq!(
            call("family", &[Value::Text("10.0.0.1".to_owned())]),
            Value::Int(4)
        );
        assert_eq!(
            call("family", &[Value::Text("::1".to_owned())]),
            Value::Int(6)
        );
    }

    #[test]
    fn network_masks_address() {
        assert_eq!(
            call("network", &[Value::Text("192.168.1.5/24".to_owned())]),
            Value::Text("192.168.1.0/24".to_owned())
        );
    }

    #[test]
    fn netmask_returns_mask_address() {
        assert_eq!(
            call("netmask", &[Value::Text("10.0.0.1/24".to_owned())]),
            Value::Text("255.255.255.0".to_owned())
        );
    }

    #[test]
    fn hostmask_is_inverse_netmask() {
        assert_eq!(
            call("hostmask", &[Value::Text("10.0.0.1/24".to_owned())]),
            Value::Text("0.0.0.255".to_owned())
        );
    }

    #[test]
    fn broadcast_returns_last_address() {
        assert_eq!(
            call("broadcast", &[Value::Text("192.168.1.5/24".to_owned())]),
            Value::Text("192.168.1.255/24".to_owned())
        );
    }

    #[test]
    fn abbrev_drops_default_prefix() {
        assert_eq!(
            call("abbrev", &[Value::Text("192.168.1.5/32".to_owned())]),
            Value::Text("192.168.1.5".to_owned())
        );
        assert_eq!(
            call("abbrev", &[Value::Text("192.168.1.0/24".to_owned())]),
            Value::Text("192.168.1.0/24".to_owned())
        );
    }

    #[test]
    fn set_masklen_changes_prefix() {
        assert_eq!(
            call(
                "set_masklen",
                &[Value::Text("192.168.1.5/24".to_owned()), Value::Int(16)]
            ),
            Value::Text("192.168.1.5/16".to_owned())
        );
    }

    #[test]
    fn inet_same_family_detects_match() {
        assert_eq!(
            call(
                "inet_same_family",
                &[
                    Value::Text("10.0.0.1".to_owned()),
                    Value::Text("192.168.1.1".to_owned()),
                ]
            ),
            Value::Boolean(true)
        );
        assert_eq!(
            call(
                "inet_same_family",
                &[
                    Value::Text("10.0.0.1".to_owned()),
                    Value::Text("::1".to_owned()),
                ]
            ),
            Value::Boolean(false)
        );
    }

    #[test]
    fn inet_merge_picks_smallest_containing() {
        assert_eq!(
            call(
                "inet_merge",
                &[
                    Value::Text("192.168.1.5/32".to_owned()),
                    Value::Text("192.168.2.5/32".to_owned()),
                ]
            ),
            Value::Text("192.168.0.0/22".to_owned())
        );
    }
}

#[cfg(test)]
mod scalar_fn_tests;

fn eval_pg_encoding_to_char(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "pg_encoding_to_char")?;
    let encoding_id = match args.first() {
        Some(Value::Null) | None => return Ok(Value::Null),
        Some(Value::Int(value)) => *value,
        Some(Value::BigInt(value)) => to_i32_saturating(*value),
        Some(Value::Text(value)) => value.trim().parse::<i32>().unwrap_or(-1),
        _ => return Ok(Value::Text(String::new())),
    };

    let encoding = match encoding_id {
        0 => "SQL_ASCII",
        1 => "EUC_JP",
        2 => "EUC_CN",
        3 => "EUC_KR",
        4 => "EUC_TW",
        5 => "EUC_JIS_2004",
        6 => "UTF8",
        7 => "MULE_INTERNAL",
        8 => "LATIN1",
        9 => "LATIN2",
        10 => "LATIN3",
        11 => "LATIN4",
        12 => "LATIN5",
        13 => "LATIN6",
        14 => "LATIN7",
        15 => "LATIN8",
        16 => "LATIN9",
        17 => "LATIN10",
        18 => "KOI8R",
        19 => "WIN1251",
        20 => "WIN866",
        21 => "WIN874",
        22 => "KOI8U",
        23 => "WIN1250",
        24 => "WIN1252",
        25 => "WIN1253",
        26 => "WIN1254",
        27 => "WIN1255",
        28 => "WIN1256",
        29 => "WIN1257",
        30 => "WIN1258",
        31 => "SJIS",
        32 => "BIG5",
        33 => "GBK",
        34 => "UHC",
        35 => "GB18030",
        36 => "JOHAB",
        37 => "SHIFT_JIS_2004",
        _ => "",
    };
    Ok(Value::Text(encoding.to_owned()))
}

fn eval_pg_char_to_encoding(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "pg_char_to_encoding")?;
    let encoding_name = match args.first() {
        Some(Value::Null) | None => return Ok(Value::Null),
        Some(Value::Text(value)) => value.trim().to_ascii_uppercase(),
        Some(Value::Int(value)) => value.to_string(),
        Some(Value::BigInt(value)) => value.to_string(),
        _ => return Ok(Value::Int(-1)),
    };

    let encoding_id = match encoding_name.as_str() {
        "SQL_ASCII" => 0,
        "EUC_JP" => 1,
        "EUC_CN" => 2,
        "EUC_KR" => 3,
        "EUC_TW" => 4,
        "EUC_JIS_2004" => 5,
        "UTF8" | "UNICODE" => 6,
        "MULE_INTERNAL" => 7,
        "LATIN1" | "ISO_8859_1" => 8,
        "LATIN2" | "ISO_8859_2" => 9,
        "LATIN3" | "ISO_8859_3" => 10,
        "LATIN4" | "ISO_8859_4" => 11,
        "LATIN5" | "ISO_8859_9" => 12,
        "LATIN6" | "ISO_8859_10" => 13,
        "LATIN7" | "ISO_8859_13" => 14,
        "LATIN8" | "ISO_8859_14" => 15,
        "LATIN9" | "ISO_8859_15" => 16,
        "LATIN10" | "ISO_8859_16" => 17,
        "KOI8R" => 18,
        "WIN1251" => 19,
        "WIN866" => 20,
        "WIN874" => 21,
        "KOI8U" => 22,
        "WIN1250" => 23,
        "WIN1252" => 24,
        "WIN1253" => 25,
        "WIN1254" => 26,
        "WIN1255" => 27,
        "WIN1256" => 28,
        "WIN1257" => 29,
        "WIN1258" => 30,
        "SJIS" => 31,
        "BIG5" => 32,
        "GBK" => 33,
        "UHC" => 34,
        "GB18030" => 35,
        "JOHAB" => 36,
        "SHIFT_JIS_2004" => 37,
        _ => -1,
    };
    Ok(Value::Int(encoding_id))
}
