use super::*;

pub(super) fn eval_pg_char_cast(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 1, "__aiondb_pg_char_cast")?;
    let Some(value) = args.first() else {
        return Err(DbError::internal(
            "__aiondb_pg_char_cast() requires exactly one argument",
        ));
    };
    if value.is_null() {
        return Ok(Value::Null);
    }

    let input = match value {
        Value::Text(text) => text.as_str(),
        other => {
            return Err(DbError::invalid_input_syntax(
                "\"char\"",
                &value_to_text(other),
            ));
        }
    };

    if let Some(byte) = parse_pg_char_octal_escape(input) {
        return Ok(Value::Text(render_pg_char_byte(byte)));
    }

    if input.is_empty() {
        return Ok(Value::Text(String::new()));
    }

    if input.is_ascii() && input.len() == 1 {
        return Ok(Value::Text(input.to_owned()));
    }

    Err(DbError::invalid_input_syntax("\"char\"", input))
}

fn parse_pg_char_octal_escape(input: &str) -> Option<u8> {
    let octal = input.strip_prefix('\\')?;
    if octal.len() != 3 || !octal.chars().all(|ch| matches!(ch, '0'..='7')) {
        return None;
    }
    u8::from_str_radix(octal, 8).ok()
}

fn render_pg_char_byte(byte: u8) -> String {
    match byte {
        0 => String::new(),
        0x20..=0x7e => char::from(byte).to_string(),
        _ => format!("\\{byte:03o}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pg_char_cast_decodes_ascii_octal_escape() {
        assert_eq!(
            eval_pg_char_cast(&[Value::Text("\\101".to_owned())]).unwrap(),
            Value::Text("A".to_owned())
        );
    }

    #[test]
    fn pg_char_cast_preserves_high_bit_escape_display() {
        assert_eq!(
            eval_pg_char_cast(&[Value::Text("\\377".to_owned())]).unwrap(),
            Value::Text("\\377".to_owned())
        );
    }

    #[test]
    fn pg_char_cast_maps_nul_escape_to_empty_text() {
        assert_eq!(
            eval_pg_char_cast(&[Value::Text("\\000".to_owned())]).unwrap(),
            Value::Text(String::new())
        );
    }
}
