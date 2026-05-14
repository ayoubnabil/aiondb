use super::*;

pub(super) fn lookup(name: &str) -> Option<FunctionInfo> {
    match name {
        // ── Implemented date/time functions ──
        "now" => Some(FunctionInfo {
            func: ScalarFunction::Now,
            return_type: DataType::TimestampTz,
            min_args: 0,
            max_args: Some(0),
        }),
        "current_timestamp" => Some(FunctionInfo {
            func: ScalarFunction::CurrentTimestamp,
            return_type: DataType::TimestampTz,
            min_args: 0,
            max_args: Some(1),
        }),
        "current_date" => Some(FunctionInfo {
            func: ScalarFunction::CurrentDate,
            return_type: DataType::Date,
            min_args: 0,
            max_args: Some(0),
        }),
        "date_part" => Some(FunctionInfo {
            func: ScalarFunction::DatePart,
            return_type: DataType::Double,
            min_args: 2,
            max_args: Some(2),
        }),
        "extract" => Some(FunctionInfo {
            func: ScalarFunction::Extract,
            return_type: DataType::Numeric,
            min_args: 2,
            max_args: Some(2),
        }),
        "date_trunc" => Some(FunctionInfo {
            func: ScalarFunction::DateTrunc,
            return_type: DataType::Timestamp,
            min_args: 2,
            max_args: Some(3),
        }),
        "age" => Some(FunctionInfo {
            func: ScalarFunction::Age,
            return_type: DataType::Interval,
            min_args: 2,
            max_args: Some(2),
        }),
        "to_char" => Some(FunctionInfo {
            func: ScalarFunction::ToChar,
            return_type: DataType::Text,
            min_args: 2,
            max_args: Some(2),
        }),
        "make_date" => Some(FunctionInfo {
            func: ScalarFunction::MakeDate,
            return_type: DataType::Date,
            min_args: 3,
            max_args: Some(3),
        }),
        "current_time" => Some(FunctionInfo {
            func: ScalarFunction::CurrentTime,
            return_type: DataType::TimeTz,
            min_args: 0,
            max_args: Some(1),
        }),
        "localtime" => Some(FunctionInfo {
            func: ScalarFunction::Localtime,
            return_type: DataType::Time,
            min_args: 0,
            max_args: Some(1),
        }),
        "localtimestamp" => Some(FunctionInfo {
            func: ScalarFunction::Generic("localtimestamp".into()),
            return_type: DataType::Timestamp,
            min_args: 0,
            max_args: Some(1),
        }),
        "make_time" => Some(FunctionInfo {
            func: ScalarFunction::MakeTime,
            return_type: DataType::Time,
            min_args: 3,
            max_args: Some(3),
        }),
        "make_timestamp" => Some(FunctionInfo {
            func: ScalarFunction::MakeTimestamp,
            return_type: DataType::Timestamp,
            min_args: 6,
            max_args: Some(6),
        }),
        "make_interval" => Some(FunctionInfo {
            func: ScalarFunction::MakeInterval,
            return_type: DataType::Interval,
            min_args: 0,
            max_args: Some(7),
        }),
        "clock_timestamp" => Some(FunctionInfo {
            func: ScalarFunction::ClockTimestamp,
            return_type: DataType::TimestampTz,
            min_args: 0,
            max_args: Some(0),
        }),
        "statement_timestamp" => Some(FunctionInfo {
            func: ScalarFunction::StatementTimestamp,
            return_type: DataType::TimestampTz,
            min_args: 0,
            max_args: Some(0),
        }),
        "transaction_timestamp" => Some(FunctionInfo {
            func: ScalarFunction::TransactionTimestamp,
            return_type: DataType::TimestampTz,
            min_args: 0,
            max_args: Some(0),
        }),
        "to_date" => Some(FunctionInfo {
            func: ScalarFunction::ToDate,
            return_type: DataType::Date,
            min_args: 2,
            max_args: Some(2),
        }),
        "to_timestamp" => Some(FunctionInfo {
            func: ScalarFunction::ToTimestamp,
            return_type: DataType::TimestampTz,
            min_args: 1,
            max_args: Some(2),
        }),
        // ── Implemented date/time utility functions ──
        "timezone" => Some(FunctionInfo {
            func: ScalarFunction::Timezone,
            return_type: DataType::Timestamp,
            min_args: 2,
            max_args: Some(2),
        }),
        "make_timestamptz" => Some(FunctionInfo {
            func: ScalarFunction::Generic("make_timestamptz".into()),
            return_type: DataType::Timestamp,
            min_args: 1,
            max_args: Some(7),
        }),
        "date_bin" => Some(FunctionInfo {
            func: ScalarFunction::Generic("date_bin".into()),
            return_type: DataType::Timestamp,
            min_args: 2,
            max_args: Some(3),
        }),
        "date_add" | "date_subtract" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Timestamp,
            min_args: 2,
            max_args: Some(2),
        }),
        "justify_days" | "justify_hours" | "justify_interval" => Some(FunctionInfo {
            func: ScalarFunction::Generic(name.into()),
            return_type: DataType::Interval,
            min_args: 1,
            max_args: Some(1),
        }),
        "__aiondb_interval_precision" => Some(FunctionInfo {
            func: ScalarFunction::Generic("__aiondb_interval_precision".into()),
            return_type: DataType::Interval,
            min_args: 2,
            max_args: Some(2),
        }),
        "__aiondb_interval_fields" => Some(FunctionInfo {
            func: ScalarFunction::Generic("__aiondb_interval_fields".into()),
            return_type: DataType::Interval,
            min_args: 4,
            max_args: Some(4),
        }),
        "isfinite" => Some(FunctionInfo {
            func: ScalarFunction::Generic("isfinite".into()),
            return_type: DataType::Boolean,
            min_args: 1,
            max_args: Some(1),
        }),
        "timeofday" => Some(FunctionInfo {
            func: ScalarFunction::Generic("timeofday".into()),
            return_type: DataType::Text,
            min_args: 0,
            max_args: Some(0),
        }),
        "overlaps" => Some(FunctionInfo {
            func: ScalarFunction::Generic("overlaps".into()),
            return_type: DataType::Boolean,
            min_args: 2,
            max_args: Some(4),
        }),
        _ => None,
    }
}
