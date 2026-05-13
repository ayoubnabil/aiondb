// -------------------------------------------------------------------------
// Helpers
// -------------------------------------------------------------------------

fn read_fixed_bytes<const N: usize>(payload: &[u8], offset: usize, context: &str) -> [u8; N] {
    let end = offset.saturating_add(N);
    let Some(bytes) = payload.get(offset..end) else {
        eprintln!("{context}");
        return [0; N];
    };
    let Ok(array) = bytes.try_into() else {
        eprintln!("{context}");
        return [0; N];
    };
    array
}

fn read_i16_be(payload: &[u8], offset: usize, context: &str) -> i16 {
    i16::from_be_bytes(read_fixed_bytes(payload, offset, context))
}

fn read_i32_be(payload: &[u8], offset: usize, context: &str) -> i32 {
    i32::from_be_bytes(read_fixed_bytes(payload, offset, context))
}

fn read_u32_be(payload: &[u8], offset: usize, context: &str) -> u32 {
    u32::from_be_bytes(read_fixed_bytes(payload, offset, context))
}

fn find_cstring_end(payload: &[u8], offset: usize, context: &str) -> usize {
    let Some(rest) = payload.get(offset..) else {
        eprintln!("{context}");
        return payload.len();
    };
    let Some(relative_end) = rest.iter().position(|byte| *byte == 0) else {
        eprintln!("{context}");
        return payload.len();
    };
    offset + relative_end
}

fn utf8_owned(bytes: &[u8], context: &str) -> String {
    match std::str::from_utf8(bytes) {
        Ok(value) => value.to_owned(),
        Err(_) => {
            eprintln!("{context}");
            String::from_utf8_lossy(bytes).into_owned()
        }
    }
}

fn utf8_str_or_empty<'a>(bytes: &'a [u8], context: &str) -> &'a str {
    match std::str::from_utf8(bytes) {
        Ok(value) => value,
        Err(_) => {
            eprintln!("{context}");
            ""
        }
    }
}

/// Build a valid v3 startup message.
fn build_startup_bytes() -> Vec<u8> {
    build_startup_bytes_with_user("test")
}

/// Build a valid v3 startup message for a specific user.
fn build_startup_bytes_with_user(user: &str) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&codec::PROTOCOL_V3.to_be_bytes());
    payload.extend_from_slice(b"user\0");
    payload.extend_from_slice(user.as_bytes());
    payload.extend_from_slice(b"\0\0");
    let len = (payload.len() as u32) + 4;
    let mut data = Vec::new();
    data.extend_from_slice(&len.to_be_bytes());
    data.extend_from_slice(&payload);
    data
}

/// Build a Terminate message (`X` + length 4).
fn build_terminate_bytes() -> Vec<u8> {
    let mut data = Vec::new();
    data.push(b'X');
    data.extend_from_slice(&4u32.to_be_bytes());
    data
}

/// Build a raw frontend message with the given tag and payload.
fn build_raw_message(tag: u8, payload: &[u8]) -> Vec<u8> {
    let mut data = Vec::new();
    data.push(tag);
    let msg_len = (payload.len() as u32) + 4;
    data.extend_from_slice(&msg_len.to_be_bytes());
    data.extend_from_slice(payload);
    data
}

/// Build a Parse message (`P`).
fn build_parse_bytes(stmt_name: &str, query: &str, param_oids: &[u32]) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(stmt_name.as_bytes());
    payload.push(0);
    payload.extend_from_slice(query.as_bytes());
    payload.push(0);
    payload.extend_from_slice(&(param_oids.len() as i16).to_be_bytes());
    for &oid in param_oids {
        payload.extend_from_slice(&oid.to_be_bytes());
    }
    build_raw_message(b'P', &payload)
}

/// Build a Bind message (`B`).
fn build_bind_bytes(
    portal: &str,
    statement: &str,
    param_formats: &[i16],
    param_values: &[Option<&[u8]>],
    result_formats: &[i16],
) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(portal.as_bytes());
    payload.push(0);
    payload.extend_from_slice(statement.as_bytes());
    payload.push(0);
    payload.extend_from_slice(&(param_formats.len() as i16).to_be_bytes());
    for &f in param_formats {
        payload.extend_from_slice(&f.to_be_bytes());
    }
    payload.extend_from_slice(&(param_values.len() as i16).to_be_bytes());
    for v in param_values {
        match v {
            None => payload.extend_from_slice(&(-1i32).to_be_bytes()),
            Some(data) => {
                payload.extend_from_slice(&(data.len() as i32).to_be_bytes());
                payload.extend_from_slice(data);
            }
        }
    }
    payload.extend_from_slice(&(result_formats.len() as i16).to_be_bytes());
    for &f in result_formats {
        payload.extend_from_slice(&f.to_be_bytes());
    }
    build_raw_message(b'B', &payload)
}

fn build_text_array_binary_bytes(element_oid: u32, elements: &[&str]) -> Vec<u8> {
    build_text_array_binary_bytes_with_lbound(element_oid, elements, 1)
}

fn build_text_array_binary_bytes_with_lbound(
    element_oid: u32,
    elements: &[&str],
    lbound: i32,
) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&1_i32.to_be_bytes()); // ndim
    payload.extend_from_slice(&0_i32.to_be_bytes()); // flags
    payload.extend_from_slice(&element_oid.to_be_bytes());
    payload.extend_from_slice(&(elements.len() as i32).to_be_bytes());
    payload.extend_from_slice(&lbound.to_be_bytes());
    for element in elements {
        payload.extend_from_slice(&(element.len() as i32).to_be_bytes());
        payload.extend_from_slice(element.as_bytes());
    }
    payload
}

/// Build a Describe message (`D`).
fn build_describe_bytes(target: u8, name: &str) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.push(target);
    payload.extend_from_slice(name.as_bytes());
    payload.push(0);
    build_raw_message(b'D', &payload)
}

/// Build an Execute message (`E`).
fn build_execute_bytes(portal: &str, max_rows: i32) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(portal.as_bytes());
    payload.push(0);
    payload.extend_from_slice(&max_rows.to_be_bytes());
    build_raw_message(b'E', &payload)
}

/// Build a Close message (`C`).
fn build_close_bytes(target: u8, name: &str) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.push(target);
    payload.extend_from_slice(name.as_bytes());
    payload.push(0);
    build_raw_message(b'C', &payload)
}

/// Build a Sync message (`S` + length 4).
fn build_sync_bytes() -> Vec<u8> {
    build_raw_message(b'S', &[])
}

/// Build a simple Query message (`Q`).
fn build_query_bytes(sql: &str) -> Vec<u8> {
    let mut payload = sql.as_bytes().to_vec();
    payload.push(0);
    build_raw_message(b'Q', &payload)
}

fn build_copy_data_bytes(data: &[u8]) -> Vec<u8> {
    build_raw_message(b'd', data)
}

fn build_copy_done_bytes() -> Vec<u8> {
    build_raw_message(b'c', &[])
}

fn backend_messages(bytes: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let mut messages = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let tag = bytes[offset];
        let len = read_u32_be(bytes, offset + 1, "backend message length") as usize;
        messages.push((tag, bytes[offset + 5..offset + 1 + len].to_vec()));
        offset += 1 + len;
    }
    messages
}

fn count_tag_occurrences(tags: &[u8], target: u8) -> usize {
    let mut count = 0usize;
    for &tag in tags {
        if tag == target {
            count += 1;
        }
    }
    count
}

fn parse_row_description_field_names(payload: &[u8]) -> Vec<String> {
    let mut offset = 0;
    let count = read_i16_be(payload, offset, "row description count") as usize;
    offset += 2;

    let mut fields = Vec::with_capacity(count);
    for _ in 0..count {
        let name_end = find_cstring_end(payload, offset, "row description field name terminator");
        let name = utf8_owned(
            &payload[offset..name_end],
            "row description field name utf8",
        );
        offset = name_end + 1;
        offset += 4; // table oid
        offset += 2; // column attr
        offset += 4; // type oid
        offset += 2; // type size
        offset += 4; // type modifier
        offset += 2; // format code
        fields.push(name);
    }
    fields
}

fn parse_row_description_format_codes(payload: &[u8]) -> Vec<i16> {
    let mut offset = 0;
    let count = read_i16_be(payload, offset, "row description count") as usize;
    offset += 2;

    let mut formats = Vec::with_capacity(count);
    for _ in 0..count {
        let name_end = find_cstring_end(payload, offset, "row description field name terminator");
        offset = name_end + 1;
        offset += 4; // table oid
        offset += 2; // column attr
        offset += 4; // type oid
        offset += 2; // type size
        offset += 4; // type modifier
        let format_code = read_i16_be(payload, offset, "row description field format code");
        offset += 2;
        formats.push(format_code);
    }
    formats
}

fn parse_row_description_type_info(payload: &[u8]) -> Vec<(u32, i32)> {
    let mut offset = 0;
    let count = read_i16_be(payload, offset, "row description count") as usize;
    offset += 2;

    let mut fields = Vec::with_capacity(count);
    for _ in 0..count {
        let name_end = find_cstring_end(payload, offset, "row description field name terminator");
        offset = name_end + 1;
        offset += 4; // table oid
        offset += 2; // column attr
        let type_oid = read_u32_be(payload, offset, "row description type oid");
        offset += 4;
        offset += 2; // type size
        let type_modifier = read_i32_be(payload, offset, "row description type modifier");
        offset += 4;
        offset += 2; // format code
        fields.push((type_oid, type_modifier));
    }
    fields
}

fn parse_row_description_origin_info(payload: &[u8]) -> Vec<(u32, i16)> {
    let mut offset = 0;
    let count = read_i16_be(payload, offset, "row description count") as usize;
    offset += 2;

    let mut fields = Vec::with_capacity(count);
    for _ in 0..count {
        let name_end = find_cstring_end(payload, offset, "row description field name terminator");
        offset = name_end + 1;
        let table_oid = read_u32_be(payload, offset, "row description table oid");
        offset += 4;
        let column_attr = read_i16_be(payload, offset, "row description column attr");
        offset += 2;
        offset += 4; // type oid
        offset += 2; // type size
        offset += 4; // type modifier
        offset += 2; // format code
        fields.push((table_oid, column_attr));
    }
    fields
}

fn lookup_relation_oid(engine: &Engine, session: &SessionHandle, relname: &str) -> u32 {
    let results = match engine.execute_sql(
        session,
        &format!("SELECT oid FROM pg_class WHERE relname = '{relname}'"),
    ) {
        Ok(results) => results,
        Err(error) => {
            eprintln!("lookup pg_class oid failed: {error}");
            return 0;
        }
    };
    let Some(result) = results.into_iter().next() else {
        eprintln!("lookup pg_class oid returned no result row");
        return 0;
    };
    let StatementResult::Query { rows, .. } = result else {
        eprintln!("expected oid query result");
        return 0;
    };
    let Some(Value::Int(oid)) = rows.first().and_then(|row| row.values.first()) else {
        eprintln!("expected oid value");
        return 0;
    };
    u32::try_from(*oid).unwrap_or(0)
}

fn parse_error_response_message(payload: &[u8]) -> Option<String> {
    let mut offset = 0;
    while offset < payload.len() {
        let field_type = *payload.get(offset)?;
        offset += 1;
        if field_type == 0 {
            break;
        }
        let end = payload[offset..]
            .iter()
            .position(|byte| *byte == 0)
            .map(|relative| offset + relative)?;
        let value = String::from_utf8_lossy(&payload[offset..end]).into_owned();
        offset = end + 1;
        if field_type == b'M' {
            return Some(value);
        }
    }
    None
}

fn parse_copy_response_column_count(payload: &[u8]) -> i16 {
    assert!(!payload.is_empty(), "copy response payload");
    read_i16_be(payload, 1, "copy response column count")
}

fn parse_parameter_description_oids(payload: &[u8]) -> Vec<u32> {
    let mut offset = 0;
    let count = read_i16_be(payload, offset, "parameter description count") as usize;
    offset += 2;

    let mut oids = Vec::with_capacity(count);
    for _ in 0..count {
        let oid = read_u32_be(payload, offset, "parameter description oid");
        offset += 4;
        oids.push(oid);
    }
    oids
}

fn parse_data_row_columns(payload: &[u8]) -> Vec<Option<Vec<u8>>> {
    let mut offset = 0;
    let count = read_i16_be(payload, offset, "data row column count") as usize;
    offset += 2;

    let mut columns = Vec::with_capacity(count);
    for _ in 0..count {
        let len = read_i32_be(payload, offset, "data row column length");
        offset += 4;
        if len < 0 {
            columns.push(None);
            continue;
        }
        let len = len as usize;
        columns.push(Some(payload[offset..offset + len].to_vec()));
        offset += len;
    }
    columns
}

fn parse_cstring_payload(payload: &[u8]) -> &str {
    let nul = find_cstring_end(payload, 0, "cstring terminator");
    utf8_str_or_empty(&payload[..nul], "cstring utf8 payload")
}

// =========================================================================
// MISS-W7: Extended query protocol end-to-end tests
// =========================================================================
