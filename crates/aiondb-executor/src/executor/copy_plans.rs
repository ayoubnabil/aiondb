use super::*;

impl Executor {
    /// Parse tab-delimited COPY text data and insert rows into the given table.
    ///
    /// Format rules (`PostgreSQL` text mode):
    /// - Rows separated by `\n`
    /// - Columns separated by `\t`
    /// - `\N` represents NULL
    /// - Empty trailing lines are skipped
    pub fn execute_copy_from_data(
        &self,
        table_id: RelationId,
        columns: &[aiondb_plan::ColumnPlan],
        data: &str,
        context: &ExecutionContext,
    ) -> DbResult<ExecutionResult> {
        fn copy_insert_batch_rows() -> usize {
            std::env::var("AIONDB_COPY_INSERT_BATCH_ROWS")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .filter(|v| *v > 0)
                .unwrap_or(4096)
        }

        context.check_deadline()?;
        let table = self
            .catalog_reader
            .get_table_by_id(context.txn_id, table_id)?
            .ok_or_else(|| DbError::internal("COPY FROM target table descriptor is missing"))?;
        let table_width = table.columns.len();
        let projected_indexes: Option<Vec<usize>> = if columns.len() == table_width {
            None
        } else {
            Some(
                columns
                    .iter()
                    .map(|column| {
                        table
                            .columns
                            .iter()
                            .position(|table_col| table_col.name.eq_ignore_ascii_case(&column.name))
                            .ok_or_else(|| {
                                DbError::internal(format!(
                                    "COPY FROM column '{}' not found in table",
                                    column.name
                                ))
                            })
                    })
                    .collect::<DbResult<Vec<_>>>()?,
            )
        };
        let mut inserted = 0u64;
        // COPY inserts many rows into a single target table. Acquire the
        // write lock once and reuse it for the full stream to avoid
        // per-row lock bookkeeping overhead.
        context.record_relation_write(table_id)?;
        self.lock_table(context, table_id, LockMode::RowExclusive)?;
        let batch_rows_target = copy_insert_batch_rows();
        let mut pending_rows: Vec<Row> = Vec::with_capacity(batch_rows_target);

        for (line_index, line) in data.lines().enumerate() {
            // `\.` on a line by itself is the PostgreSQL text-format
            // end-of-copy marker. psql and pgbench emit it at the end of the
            // data stream. Anything after it is ignored.
            if line == "\\." {
                break;
            }
            context.check_deadline()?;

            let fields = split_copy_line_fields_exact(line, columns, line_index + 1)?;
            let mut values = Vec::with_capacity(columns.len());
            for (field, column) in fields.into_iter().zip(columns.iter()) {
                let value = parse_copy_text_value(field, &column.data_type)?;
                // COPY's `parse_copy_text_value` already returned a value
                // typed for the column, so the full coercion chain
                // (cast + text-modifier + range canonicalisation) is
                // a series of no-ops on the dominant numeric / Boolean /
                // date / timestamp shapes COPY ingests. Skip it when we
                // can prove it's irrelevant — same shape as the
                // INSERT-VALUES fast path.
                let needs_coerce = column.text_type_modifier.is_some()
                    || matches!(column.data_type, aiondb_core::DataType::Text)
                    || !super::dml_plans::value_matches_column_type_exactly(
                        &value,
                        &column.data_type,
                    );
                let coerced = if needs_coerce {
                    coerce_assigned_value(
                        value,
                        &column.data_type,
                        column.nullable,
                        column.text_type_modifier,
                    )?
                } else {
                    value
                };
                values.push(coerced);
            }

            let row_values = if let Some(indexes) = projected_indexes.as_ref() {
                let mut expanded = vec![Value::Null; table_width];
                for (value, column_index) in values.into_iter().zip(indexes.iter().copied()) {
                    expanded[column_index] = value;
                }
                expanded
            } else {
                values
            };

            pending_rows.push(Row::new(row_values));
            if pending_rows.len() >= batch_rows_target {
                let batch = std::mem::take(&mut pending_rows);
                let tuple_ids = self
                    .storage_dml
                    .insert_batch(context.txn_id, table_id, batch)?;
                inserted += tuple_ids.len() as u64;
                context.record_tuple_writes(table_id, &tuple_ids)?;
            }
        }

        if !pending_rows.is_empty() {
            let tuple_ids =
                self.storage_dml
                    .insert_batch(context.txn_id, table_id, pending_rows)?;
            inserted += tuple_ids.len() as u64;
            context.record_tuple_writes(table_id, &tuple_ids)?;
        }

        Ok(ExecutionResult::Command {
            tag: "COPY".to_owned(),
            rows_affected: inserted,
        })
    }
}

/// Parse a single text field from COPY data into a `Value`.
///
/// `\N` is interpreted as NULL. All other values are parsed according
/// to `data_type`.
pub fn parse_copy_text_value(field: &str, data_type: &DataType) -> DbResult<Value> {
    if field == "\\N" {
        return Ok(Value::Null);
    }

    match data_type {
        DataType::Int => field
            .parse::<i32>()
            .map(Value::Int)
            .map_err(|_| copy_parse_error(field, "INT")),
        DataType::BigInt => field
            .parse::<i64>()
            .map(Value::BigInt)
            .map_err(|_| copy_parse_error(field, "BIGINT")),
        DataType::Real => field
            .parse::<f32>()
            .map(Value::Real)
            .map_err(|_| copy_parse_error(field, "REAL")),
        DataType::Double => field
            .parse::<f64>()
            .map(Value::Double)
            .map_err(|_| copy_parse_error(field, "DOUBLE")),
        DataType::Numeric => {
            // Simple numeric parsing: integer or decimal string.
            parse_numeric_value(field)
        }
        DataType::Money => aiondb_eval::coercions::coerce_value(
            Value::Text(unescape_copy_text(field)),
            &DataType::Money,
        )
        .map_err(|_| copy_parse_error(field, "MONEY")),
        DataType::Text => Ok(Value::Text(unescape_copy_text(field))),
        DataType::Boolean => match field {
            "t" | "true" | "TRUE" | "True" | "1" => Ok(Value::Boolean(true)),
            "f" | "false" | "FALSE" | "False" | "0" => Ok(Value::Boolean(false)),
            _ => Err(copy_parse_error(field, "BOOLEAN")),
        },
        DataType::Blob => {
            // Expect hex-encoded bytes prefixed with \x (PostgreSQL bytea format).
            if let Some(hex) = field.strip_prefix("\\x") {
                parse_hex_bytes(hex).map(Value::Blob)
            } else {
                Err(copy_parse_error(field, "BLOB"))
            }
        }
        DataType::Timestamp => parse_timestamp(field),
        DataType::Date => parse_date(field),
        DataType::Time => parse_time_copy(field),
        DataType::TimeTz => parse_time_tz_copy(field),
        DataType::Interval => parse_interval(field),
        DataType::Tid => aiondb_core::TidValue::parse(field)
            .map(Value::Tid)
            .ok_or_else(|| copy_parse_error(field, "TID")),
        DataType::MacAddr => aiondb_core::MacAddr::parse(field)
            .map(Value::MacAddr)
            .ok_or_else(|| copy_parse_error(field, "MACADDR")),
        DataType::MacAddr8 => aiondb_core::MacAddr8::parse(field)
            .map(Value::MacAddr8)
            .ok_or_else(|| copy_parse_error(field, "MACADDR8")),
        DataType::PgLsn => aiondb_core::PgLsnValue::parse(field)
            .map(Value::PgLsn)
            .ok_or_else(|| copy_parse_error(field, "PG_LSN")),
        DataType::Uuid => {
            Value::uuid_from_str(field).ok_or_else(|| copy_parse_error(field, "UUID"))
        }
        DataType::TimestampTz => parse_timestamp_tz(field),
        DataType::Vector { dims, .. } => {
            let v = aiondb_core::VectorValue::parse(field)
                .ok_or_else(|| copy_parse_error(field, "VECTOR"))?;
            if v.dims != *dims {
                let got_dims = v.dims;
                return Err(DbError::Bind(Box::new(ErrorReport::new(
                    SqlState::InternalError,
                    format!("COPY: vector dimension mismatch, expected {dims} but got {got_dims}"),
                ))));
            }
            Ok(Value::Vector(v))
        }
        DataType::Jsonb => {
            let v: serde_json::Value =
                serde_json::from_str(field).map_err(|_| copy_parse_error(field, "JSONB"))?;
            Ok(Value::Jsonb(v))
        }
        DataType::Array(inner) => parse_copy_array_value(field, inner),
    }
}

/// Parse a `PostgreSQL` array literal `{val1,val2,...}` from COPY text format.
fn parse_copy_array_value(field: &str, elem_type: &DataType) -> DbResult<Value> {
    let trimmed = field.trim();
    let inner = trimmed
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .ok_or_else(|| copy_parse_error(field, "ARRAY"))?;
    if inner.trim().is_empty() {
        return Ok(Value::Array(Vec::new()));
    }
    let elements: Vec<Value> = split_array_elements(inner)
        .iter()
        .map(|elem| {
            let elem = elem.trim();
            if elem.eq_ignore_ascii_case("NULL") {
                Ok(Value::Null)
            } else if let Some(quoted) = elem.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                let unescaped = aiondb_core::pg_array_unescape_quoted(quoted);
                parse_copy_text_value(&unescaped, elem_type)
            } else {
                parse_copy_text_value(elem, elem_type)
            }
        })
        .collect::<DbResult<Vec<_>>>()?;
    Ok(Value::Array(elements))
}

/// Split array elements respecting quoted strings and nested arrays.
fn split_array_elements(s: &str) -> Vec<String> {
    let mut elements = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut depth = 0i32;
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '"' if !in_quotes => {
                in_quotes = true;
                current.push(ch);
            }
            '"' if in_quotes => {
                in_quotes = false;
                current.push(ch);
            }
            '\\' if in_quotes => {
                current.push(ch);
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            '{' if !in_quotes => {
                depth += 1;
                current.push(ch);
            }
            '}' if !in_quotes => {
                depth -= 1;
                current.push(ch);
            }
            ',' if !in_quotes && depth == 0 => {
                elements.push(std::mem::take(&mut current));
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        elements.push(current);
    }
    elements
}

/// Format a Value for COPY TO text output (`PostgreSQL` text format).
pub fn format_copy_text_value(value: &Value) -> String {
    match value {
        Value::Null => "\\N".to_owned(),
        Value::Text(s) => escape_copy_text(s),
        Value::Blob(bytes) => {
            // Stream the hex-pair bytes via the shared helper instead
            // of allocating a 2-char String per byte through `format!`.
            let mut hex =
                String::with_capacity(2usize.saturating_add(bytes.len().saturating_mul(2)));
            hex.push_str("\\x");
            aiondb_core::hex_encode_into(bytes, &mut hex);
            hex
        }
        Value::Numeric(n) => {
            if n.scale == 0 {
                n.coefficient.to_string()
            } else {
                let s = n.coefficient.to_string();
                let is_negative = n.coefficient < 0;
                let digits = if is_negative { &s[1..] } else { &s[..] };
                let scale = usize::try_from(n.scale).unwrap_or(0);
                if digits.len() <= scale {
                    let padded = format!("{:0>width$}", digits, width = scale.saturating_add(1));
                    let (int_part, frac_part) = padded.split_at(padded.len() - scale);
                    if is_negative {
                        format!("-{int_part}.{frac_part}")
                    } else {
                        format!("{int_part}.{frac_part}")
                    }
                } else {
                    let (int_part, frac_part) = digits.split_at(digits.len() - scale);
                    if is_negative {
                        format!("-{int_part}.{frac_part}")
                    } else {
                        format!("{int_part}.{frac_part}")
                    }
                }
            }
        }
        other => format!("{other}"),
    }
}

/// Escape a string for `PostgreSQL` COPY text format output.
fn escape_copy_text(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\t' => result.push_str("\\t"),
            '\r' => result.push_str("\\r"),
            _ => result.push(ch),
        }
    }
    result
}

fn copy_parse_error(field: &str, type_name: &str) -> DbError {
    DbError::Bind(Box::new(ErrorReport::new(
        SqlState::InternalError,
        format!("COPY: cannot parse \"{field}\" as {type_name}"),
    )))
}

/// Unescape `PostgreSQL` COPY text-mode escape sequences.
/// Also follows PostgreSQL's fallback rule: unknown escaped characters
/// resolve to the escaped character itself.
fn unescape_copy_text(input: &str) -> String {
    // Most COPY text fields contain no backslash escapes - reads from
    // pgbench, JOB, or any pipeline that already produced canonical
    // PG text format hit this path on every row of the load. Detect
    // that up front and use the bulk `String::from` (memcpy) instead
    // of pushing char-by-char.
    if !input.as_bytes().contains(&b'\\') {
        return input.to_owned();
    }
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('\\') => result.push('\\'),
                Some('b') => result.push('\x08'),
                Some('f') => result.push('\x0c'),
                Some('n') => result.push('\n'),
                Some('t') => result.push('\t'),
                Some('r') => result.push('\r'),
                Some('v') => result.push('\x0b'),
                Some(other) => result.push(other),
                None => result.push('\\'),
            }
        } else {
            result.push(ch);
        }
    }
    result
}

fn parse_numeric_value(field: &str) -> DbResult<Value> {
    field
        .parse::<aiondb_core::NumericValue>()
        .map(Value::Numeric)
        .map_err(|_| copy_parse_error(field, "NUMERIC"))
}

fn split_copy_line_fields_exact<'a>(
    line: &'a str,
    columns: &[aiondb_plan::ColumnPlan],
    line_number: usize,
) -> DbResult<Vec<&'a str>> {
    let expected_columns = columns.len();
    if expected_columns == 0 {
        let got = usize::from(!line.is_empty());
        if got != 0 {
            return Err(copy_column_count_error(line_number, columns, got, line));
        }
        return Ok(Vec::new());
    }

    if line.is_empty() {
        return Ok(vec![""; expected_columns]);
    }

    let bytes = line.as_bytes();
    let mut fields = Vec::with_capacity(expected_columns);
    let mut start = 0usize;
    let mut column_count = 1usize;

    for (idx, byte) in bytes.iter().enumerate() {
        if *byte != b'\t' {
            continue;
        }
        if column_count <= expected_columns {
            fields.push(&line[start..idx]);
        }
        column_count += 1;
        start = idx + 1;
    }

    if column_count <= expected_columns {
        fields.push(&line[start..]);
    }

    if column_count != expected_columns {
        return Err(copy_column_count_error(
            line_number,
            columns,
            column_count,
            line,
        ));
    }

    Ok(fields)
}

fn copy_column_count_error(
    line_number: usize,
    columns: &[aiondb_plan::ColumnPlan],
    got: usize,
    line: &str,
) -> DbError {
    let expected = columns.len();
    if got < expected {
        let column_name = columns
            .get(got)
            .map(|column| format!("column \"{}\"", column.name))
            .unwrap_or_else(|| format!("column {expected}"));
        return DbError::Bind(Box::new(
            ErrorReport::new(
                SqlState::InvalidTextRepresentation,
                format!("missing data for {column_name}"),
            )
            .with_client_detail(format!("COPY line {line_number}: \"{line}\"")),
        ));
    }
    if got > expected {
        return DbError::Bind(Box::new(
            ErrorReport::new(
                SqlState::InvalidTextRepresentation,
                "extra data after last expected column".to_owned(),
            )
            .with_client_detail(format!("COPY line {line_number}: \"{line}\"")),
        ));
    }
    DbError::Bind(Box::new(ErrorReport::new(
        SqlState::SyntaxError,
        format!("COPY line {line_number}: expected {expected} columns, got {got}"),
    )))
}

fn parse_hex_bytes(hex: &str) -> DbResult<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return Err(copy_parse_error(hex, "BLOB"));
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|_| copy_parse_error(hex, "BLOB")))
        .collect()
}

fn parse_timestamp(field: &str) -> DbResult<Value> {
    let field = field
        .strip_prefix('\'')
        .and_then(|inner| inner.strip_suffix('\''))
        .unwrap_or(field);
    // PostgreSQL accepts bare dates for timestamp input and normalizes them
    // to midnight.
    let (date_str, time_str) = match field.split_once(' ') {
        Some((date_str, time_str)) => (date_str, time_str),
        None => (field, "00:00:00"),
    };
    let date =
        parse_date_components(date_str).map_err(|()| copy_parse_error(field, "TIMESTAMP"))?;
    let time =
        parse_time_components(time_str).map_err(|()| copy_parse_error(field, "TIMESTAMP"))?;
    Ok(Value::Timestamp(time::PrimitiveDateTime::new(date, time)))
}

fn parse_date(field: &str) -> DbResult<Value> {
    let field = field
        .strip_prefix('\'')
        .and_then(|inner| inner.strip_suffix('\''))
        .unwrap_or(field);
    parse_date_components(field)
        .map(Value::Date)
        .map_err(|()| copy_parse_error(field, "DATE"))
}

fn parse_date_components(s: &str) -> Result<time::Date, ()> {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 3 {
        return Err(());
    }
    let year: i32 = parts[0].parse().map_err(|_| ())?;
    let month: u8 = parts[1].parse().map_err(|_| ())?;
    let day: u8 = parts[2].parse().map_err(|_| ())?;
    let month = time::Month::try_from(month).map_err(|_| ())?;
    time::Date::from_calendar_date(year, month, day).map_err(|_| ())
}

fn parse_time_copy(field: &str) -> DbResult<Value> {
    parse_time_components(field)
        .map(Value::Time)
        .map_err(|()| copy_parse_error(field, "TIME"))
}

fn parse_time_tz_copy(field: &str) -> DbResult<Value> {
    let err = || copy_parse_error(field, "TIMETZ");
    let offset_start = field
        .rfind('+')
        .or_else(|| field.rfind('-'))
        .ok_or_else(err)?;
    let time_str = field[..offset_start].trim_end();
    let offset_str = &field[offset_start..];
    let time = parse_time_components(time_str).map_err(|()| err())?;
    let offset = parse_utc_offset_copy(offset_str).map_err(|()| err())?;
    Ok(Value::TimeTz(time, offset))
}

fn parse_time_components(s: &str) -> Result<time::Time, ()> {
    // HH:MM:SS or HH:MM:SS.fraction
    let (main, subsec) = match s.split_once('.') {
        Some((m, f)) => (m, Some(f)),
        None => (s, None),
    };
    let parts: Vec<&str> = main.split(':').collect();
    if parts.len() != 3 {
        return Err(());
    }
    let hour: u8 = parts[0].parse().map_err(|_| ())?;
    let minute: u8 = parts[1].parse().map_err(|_| ())?;
    let second: u8 = parts[2].parse().map_err(|_| ())?;
    let micro = if let Some(frac) = subsec {
        // Pad or truncate to 6 digits for microseconds
        let padded = format!("{frac:0<6}");
        padded[..6].parse::<u32>().map_err(|_| ())?
    } else {
        0
    };
    time::Time::from_hms_micro(hour, minute, second, micro).map_err(|_| ())
}

fn parse_interval(field: &str) -> DbResult<Value> {
    // Parse the PostgreSQL-style Display format:
    //   "1 year 2 mons 3 days 04:05:06.123456"
    // Also supports the internal format: "Nm Nd Nus"
    let mut months = 0i32;
    let mut days = 0i32;
    let mut micros = 0i64;

    let tokens: Vec<&str> = field.split_whitespace().collect();
    let mut i = 0;
    while i < tokens.len() {
        let token = tokens[i];

        // Check for time component: [+/-]HH:MM:SS[.frac]
        let time_part = token.strip_prefix('+').unwrap_or(token);
        if time_part.contains(':') {
            let neg = token.starts_with('-');
            let unsigned = token.trim_start_matches(['+', '-']);
            let val = parse_interval_time_micros(unsigned)
                .map_err(|()| copy_parse_error(field, "INTERVAL"))?;
            micros = if neg {
                micros
                    .checked_sub(val)
                    .ok_or_else(|| copy_parse_error(field, "INTERVAL"))?
            } else {
                micros
                    .checked_add(val)
                    .ok_or_else(|| copy_parse_error(field, "INTERVAL"))?
            };
            i += 1;
            continue;
        }

        // Check for "Nm", "Nd", "Nus" tokens
        if let Some(val) = token.strip_suffix("us") {
            micros = val
                .parse()
                .map_err(|_| copy_parse_error(field, "INTERVAL"))?;
            i += 1;
            continue;
        }
        if token.ends_with('d')
            && !token.ends_with("nd")
            && token[..token.len() - 1]
                .chars()
                .all(|c| c.is_ascii_digit() || c == '-')
        {
            let val = &token[..token.len() - 1];
            if val.parse::<i32>().is_ok() {
                days = val
                    .parse()
                    .map_err(|_| copy_parse_error(field, "INTERVAL"))?;
                i += 1;
                continue;
            }
        }
        if token.ends_with('m')
            && !token.ends_with("nm")
            && token[..token.len() - 1]
                .chars()
                .all(|c| c.is_ascii_digit() || c == '-')
        {
            let val = &token[..token.len() - 1];
            if val.parse::<i32>().is_ok() {
                months = val
                    .parse()
                    .map_err(|_| copy_parse_error(field, "INTERVAL"))?;
                i += 1;
                continue;
            }
        }

        // PG-style: number followed by unit word
        if i + 1 < tokens.len() {
            if let Ok(num) = token.parse::<i64>() {
                let unit = tokens[i + 1].to_ascii_lowercase();
                match unit.as_str() {
                    "year" | "years" => {
                        let num_i32 =
                            i32::try_from(num).map_err(|_| copy_parse_error(field, "INTERVAL"))?;
                        let year_months = num_i32
                            .checked_mul(12)
                            .ok_or_else(|| copy_parse_error(field, "INTERVAL"))?;
                        months = months
                            .checked_add(year_months)
                            .ok_or_else(|| copy_parse_error(field, "INTERVAL"))?;
                        i += 2;
                        continue;
                    }
                    "mon" | "mons" | "month" | "months" => {
                        let num_i32 =
                            i32::try_from(num).map_err(|_| copy_parse_error(field, "INTERVAL"))?;
                        months = months
                            .checked_add(num_i32)
                            .ok_or_else(|| copy_parse_error(field, "INTERVAL"))?;
                        i += 2;
                        continue;
                    }
                    "day" | "days" => {
                        let num_i32 =
                            i32::try_from(num).map_err(|_| copy_parse_error(field, "INTERVAL"))?;
                        days = days
                            .checked_add(num_i32)
                            .ok_or_else(|| copy_parse_error(field, "INTERVAL"))?;
                        i += 2;
                        continue;
                    }
                    _ => {}
                }
            }
        }

        return Err(copy_parse_error(field, "INTERVAL"));
    }
    Ok(Value::Interval(aiondb_core::IntervalValue::new(
        months, days, micros,
    )))
}

/// Parse HH:MM:SS[.frac] into microseconds.
fn parse_interval_time_micros(s: &str) -> Result<i64, ()> {
    let (main, subsec) = match s.split_once('.') {
        Some((m, f)) => (m, Some(f)),
        None => (s, None),
    };
    let parts: Vec<&str> = main.split(':').collect();
    if parts.len() < 2 || parts.len() > 3 {
        return Err(());
    }
    let hours: i64 = parts[0].parse().map_err(|_| ())?;
    let minutes: i64 = parts[1].parse().map_err(|_| ())?;
    let seconds: i64 = if parts.len() == 3 {
        parts[2].parse().map_err(|_| ())?
    } else {
        0
    };
    let frac_micros: i64 = if let Some(frac) = subsec {
        let padded = format!("{frac:0<6}");
        padded[..6].parse::<i64>().map_err(|_| ())?
    } else {
        0
    };
    Ok(hours * 3_600_000_000 + minutes * 60_000_000 + seconds * 1_000_000 + frac_micros)
}

fn parse_timestamp_tz(field: &str) -> DbResult<Value> {
    let field = field
        .strip_prefix('\'')
        .and_then(|inner| inner.strip_suffix('\''))
        .unwrap_or(field);
    // Expect: YYYY-MM-DD HH:MM:SS[.frac] +HH:MM or YYYY-MM-DD HH:MM:SS[.frac]+HH:MM
    // Find the last '+' or '-' that starts the offset (skip the date part).
    let err = || copy_parse_error(field, "TIMESTAMPTZ");
    // Minimum valid input: "YYYY-MM-DD " (at least 11 chars for date + space)
    if field.len() < 10 {
        return Err(err());
    }
    // Find offset separator: look for +/- after the time part
    let offset_start = field
        .rfind('+')
        .or_else(|| {
            // Find last '-' that's after position 10 (past the date YYYY-MM-DD)
            field[10..].rfind('-').map(|pos| pos + 10)
        })
        .ok_or_else(err)?;

    let datetime_str = field[..offset_start].trim_end();
    let offset_str = &field[offset_start..];

    let (date_str, time_str) = datetime_str.split_once(' ').ok_or_else(err)?;
    let date = parse_date_components(date_str).map_err(|()| err())?;
    let time = parse_time_components(time_str).map_err(|()| err())?;

    let offset = parse_utc_offset_copy(offset_str).map_err(|()| err())?;

    let pdt = time::PrimitiveDateTime::new(date, time);
    Ok(Value::TimestampTz(pdt.assume_offset(offset)))
}

fn parse_utc_offset_copy(s: &str) -> Result<time::UtcOffset, ()> {
    if s.is_empty() {
        return Err(());
    }
    let sign: i8 = if s.starts_with('-') { -1 } else { 1 };
    let parts: Vec<&str> = s[1..].split(':').collect();
    if parts.len() != 2 {
        return Err(());
    }
    let hours: i8 = parts[0].parse().map_err(|_| ())?;
    let minutes: i8 = parts[1].parse().map_err(|_| ())?;
    time::UtcOffset::from_hms(sign * hours, sign * minutes, 0).map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_columns(count: usize) -> Vec<aiondb_plan::ColumnPlan> {
        (0..count)
            .map(|index| aiondb_plan::ColumnPlan {
                name: format!("col{}", index + 1),
                data_type: DataType::Text,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: true,
                has_default: false,
            })
            .collect()
    }

    #[test]
    fn format_null_value() {
        assert_eq!(format_copy_text_value(&Value::Null), "\\N");
    }

    #[test]
    fn format_text_escapes_special_chars() {
        assert_eq!(
            format_copy_text_value(&Value::Text("a\tb".to_owned())),
            "a\\tb"
        );
        assert_eq!(
            format_copy_text_value(&Value::Text("a\nb".to_owned())),
            "a\\nb"
        );
        assert_eq!(
            format_copy_text_value(&Value::Text("a\\b".to_owned())),
            "a\\\\b"
        );
        assert_eq!(
            format_copy_text_value(&Value::Text("a\rb".to_owned())),
            "a\\rb"
        );
    }

    #[test]
    fn format_plain_text_unchanged() {
        assert_eq!(
            format_copy_text_value(&Value::Text("hello world".to_owned())),
            "hello world"
        );
    }

    #[test]
    fn format_blob_as_hex() {
        assert_eq!(
            format_copy_text_value(&Value::Blob(vec![0xde, 0xad, 0xbe, 0xef])),
            "\\xdeadbeef"
        );
        assert_eq!(format_copy_text_value(&Value::Blob(vec![])), "\\x");
    }

    #[test]
    fn format_numeric_with_scale() {
        let n = Value::Numeric(aiondb_core::NumericValue::new(12345, 2));
        assert_eq!(format_copy_text_value(&n), "123.45");
    }

    #[test]
    fn format_numeric_zero_scale() {
        let n = Value::Numeric(aiondb_core::NumericValue::new(42, 0));
        assert_eq!(format_copy_text_value(&n), "42");
    }

    #[test]
    fn unescape_roundtrip() {
        let original = "hello\tworld\nnew\\line";
        let escaped = escape_copy_text(original);
        let unescaped = unescape_copy_text(&escaped);
        assert_eq!(unescaped, original);
    }

    #[test]
    fn unescape_postgres_text_escape_sequences() {
        assert_eq!(unescape_copy_text("\\b\\f\\v\\q"), "\x08\x0c\x0bq");
    }

    #[test]
    fn format_int_and_bool() {
        assert_eq!(format_copy_text_value(&Value::Int(42)), "42");
        assert_eq!(format_copy_text_value(&Value::Boolean(true)), "true");
        assert_eq!(format_copy_text_value(&Value::Boolean(false)), "false");
    }

    #[test]
    fn split_copy_line_fields_exact_handles_trailing_empty_column() {
        let columns = test_columns(3);
        let fields = split_copy_line_fields_exact("a\tb\t", &columns, 1).expect("fields");
        assert_eq!(fields, vec!["a", "b", ""]);
    }

    #[test]
    fn split_copy_line_fields_exact_rejects_extra_columns() {
        let columns = test_columns(2);
        let err =
            split_copy_line_fields_exact("a\tb\tc", &columns, 9).expect_err("expected mismatch");
        let msg = err.to_string();
        assert!(
            msg.contains("extra data after last expected column")
                || msg.contains("COPY line 9: expected 2 columns, got 3"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn split_copy_line_fields_exact_rejects_missing_columns() {
        let columns = test_columns(3);
        let err = split_copy_line_fields_exact("a\tb", &columns, 2).expect_err("expected mismatch");
        let msg = err.to_string();
        assert!(
            msg.contains("missing data for column")
                || msg.contains("COPY line 2: expected 3 columns, got 2"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn parse_numeric_value_supports_large_coefficients() {
        let parsed = parse_numeric_value("123456789012345678901234567890123456789")
            .expect("numeric parse must succeed");
        let Value::Numeric(numeric) = parsed else {
            panic!("expected numeric value");
        };
        assert_eq!(
            numeric.to_string(),
            "123456789012345678901234567890123456789"
        );
    }
}
