use super::*;

pub(super) fn eval_to_number(args: &[Value]) -> DbResult<Value> {
    expect_args(args, 2, "to_number")?;
    if args.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    let s = expect_text_arg(args, 0, "to_number()", "first")?.to_owned();
    let fmt = expect_text_arg(args, 1, "to_number()", "second")?.to_owned();

    // Check if the format uses PR (angle-bracket negative)
    let fmt_upper = fmt.to_ascii_uppercase();
    let has_pr = fmt_upper.contains("PR");

    let mut cleaned: String = s
        .chars()
        .filter(|c| {
            c.is_ascii_digit()
                || *c == '.'
                || *c == '-'
                || *c == '+'
                || (has_pr && (*c == '<' || *c == '>'))
        })
        .collect();

    // Handle PR format: <564646.654564> -> -564646.654564
    if has_pr && cleaned.starts_with('<') && cleaned.ends_with('>') {
        cleaned = format!("-{}", &cleaned[1..cleaned.len() - 1]);
    }

    // Handle trailing minus sign (S/MI format): "5.01-" -> "-5.01"
    if cleaned.ends_with('-') && !cleaned.starts_with('-') && !cleaned.starts_with('+') {
        cleaned = format!("-{}", &cleaned[..cleaned.len() - 1]);
    }

    // Parse as numeric to preserve scale/precision
    let n: NumericValue = cleaned
        .parse()
        .map_err(|_| DbError::internal(format!("to_number: invalid input \"{s}\"")))?;
    Ok(Value::Numeric(n))
}

pub(super) fn eval_row(args: &[Value]) -> Value {
    let mut result = String::new();
    result.push('(');
    for (i, arg) in args.iter().enumerate() {
        if i > 0 {
            result.push(',');
        }
        match arg {
            Value::Null => {}
            Value::Boolean(b) => push_composite_field_text(&mut result, if *b { "t" } else { "f" }),
            Value::Text(s) => push_composite_field_text(&mut result, s),
            other => {
                let rendered = other.to_string();
                push_composite_field_text(&mut result, &rendered);
            }
        }
    }
    result.push(')');
    Value::Text(result)
}

fn push_composite_field_text(result: &mut String, text: &str) {
    if text.is_empty()
        || text.contains(',')
        || text.contains('(')
        || text.contains(')')
        || text.contains('"')
        || text.contains('\\')
        || text.contains(' ')
    {
        result.push('"');
        for ch in text.chars() {
            if ch == '"' || ch == '\\' {
                result.push('\\');
            }
            result.push(ch);
        }
        result.push('"');
    } else {
        result.push_str(text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_quotes_array_field_with_comma() {
        let value = eval_row(&[
            Value::Int(4),
            Value::Array(vec![Value::Int(2), Value::Int(4)]),
        ]);
        assert_eq!(value, Value::Text("(4,\"{2,4}\")".to_owned()));
    }
}
