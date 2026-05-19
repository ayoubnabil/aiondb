//! Cypher value/literal formatting helpers.
//!
//! Split out of `graph_plans/mod.rs` (see the module docs there). These
//! functions render graph elements (bound edges, paths, property bags) and
//! scalar `Value`s into Cypher's textual literal syntax. They depend on the
//! shared `BindingRow` / `BoundValue` types and the node/edge literal
//! formatters, which stay in the parent module and are reached via
//! `use super::*`.

use aiondb_core::{Row, Value};
use aiondb_plan::graph::CypherRelDirection;

use super::*;

pub(super) fn format_cypher_bound_edge_literal(
    binding: &BindingRow,
    variable: &str,
) -> Option<String> {
    match binding.get(variable) {
        Some(BoundValue::Edge {
            row,
            column_names,
            rel_type,
            ..
        }) => Some(format_cypher_edge_literal(column_names, row, rel_type)),
        _ => None,
    }
}

pub(super) fn format_cypher_path_literal(
    binding: &BindingRow,
    node_vars: &[String],
    relationship_vars: &[String],
    directions: &[CypherRelDirection],
) -> String {
    let nodes = node_vars
        .iter()
        .map(|var| {
            format_cypher_bound_node_literal(binding, var).unwrap_or_else(|| "()".to_owned())
        })
        .collect::<Vec<_>>();
    let relationships = relationship_vars
        .iter()
        .map(|var| {
            format_cypher_bound_edge_literal(binding, var).unwrap_or_else(|| "[]".to_owned())
        })
        .collect::<Vec<_>>();
    format_cypher_path_value_literal(&nodes, &relationships, directions)
}

pub(super) fn format_cypher_path_value_literal(
    node_literals: &[String],
    relationship_literals: &[String],
    directions: &[CypherRelDirection],
) -> String {
    if node_literals.is_empty() {
        return String::from("()");
    }

    let mut out = String::new();
    for (idx, node_literal) in node_literals.iter().enumerate() {
        if idx > 0 {
            let rel_idx = idx - 1;
            let rel_literal = relationship_literals
                .get(rel_idx)
                .map(String::as_str)
                .unwrap_or("[]");
            match directions
                .get(rel_idx)
                .copied()
                .unwrap_or(CypherRelDirection::Outgoing)
            {
                CypherRelDirection::Outgoing => {
                    out.push('-');
                    out.push_str(rel_literal);
                    out.push_str("->");
                }
                CypherRelDirection::Incoming => {
                    out.push_str("<-");
                    out.push_str(rel_literal);
                    out.push('-');
                }
                CypherRelDirection::Both => {
                    out.push('-');
                    out.push_str(rel_literal);
                    out.push('-');
                }
            }
        }
        out.push_str(node_literal);
    }
    out
}

/// Render the user-visible properties of a graph element as Cypher's
/// `{key: value, ...}` map literal. System columns (prefixed with `__` or
/// the conventional `id`/`tid`/`source`/`target` identity columns) and
/// NULL-valued columns are skipped.
pub(super) fn format_cypher_property_bag(column_names: &[String], row: &Row) -> String {
    use std::fmt::Write as _;

    // Walk once to count visible properties; if there are none we don't need
    // to emit braces (matches the empty-entries -> empty string behaviour)
    // and we avoid even allocating the output buffer.
    let mut visible = 0usize;
    for (idx, name) in column_names.iter().enumerate() {
        if is_cypher_system_column(name) {
            continue;
        }
        if row.values.get(idx).is_some_and(|value| !value.is_null()) {
            visible += 1;
        }
    }
    if visible == 0 {
        return String::new();
    }

    // Reserve a rough capacity (`{name: value, ` ~ 20 chars per entry) so
    // the buffer typically doesn't need to grow during writes. Replaces
    // the previous Vec<String>-of-entries-then-join pattern, which paid
    // one alloc per visible column plus one for the joined output.
    let mut out = String::with_capacity(2 + visible * 20);
    out.push('{');
    let mut first = true;
    for (idx, name) in column_names.iter().enumerate() {
        if is_cypher_system_column(name) {
            continue;
        }
        let Some(value) = row.values.get(idx) else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        if !first {
            out.push_str(", ");
        }
        first = false;
        let _ = write!(out, "{name}: {}", format_cypher_property_value(value));
    }
    out.push('}');
    out
}

pub(super) fn is_cypher_system_column(name: &str) -> bool {
    if name.starts_with("__") {
        return true;
    }
    matches!(
        name,
        "id" | "tid"
            | "source"
            | "target"
            | "source_id"
            | "target_id"
            | "__id"
            | "__tid"
            | "__source"
            | "__target"
    )
}

pub(in crate::executor) fn format_cypher_property_value(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Boolean(b) => b.to_string(),
        Value::Int(n) => n.to_string(),
        Value::BigInt(n) => n.to_string(),
        Value::Real(f) => f.to_string(),
        Value::Double(f) => f.to_string(),
        Value::Numeric(v) => v.to_string(),
        Value::Text(s) => format!("'{}'", s.replace('\'', "\\'")),
        // Cypher renders temporal values in single quotes inside node
        // property bags (e.g. `{date: '1910-05-06'}`). Cypher format differs
        // from PG: timestamps use `T` separator, time/datetime drop seconds
        // when zero with no sub-second, and tz offsets always include minutes.
        Value::Date(_) | Value::Interval(_) => format!("'{value}'"),
        Value::Time(t) => format!("'{}'", format_cypher_time(t)),
        Value::TimeTz(t, off) => {
            format!("'{}{}'", format_cypher_time(t), format_cypher_offset(off))
        }
        Value::Timestamp(ts) => format!("'{}'", format_cypher_primitive_datetime(ts)),
        Value::TimestampTz(odt) => {
            let pdt = time::PrimitiveDateTime::new(odt.date(), odt.time());
            format!(
                "'{}{}'",
                format_cypher_primitive_datetime(&pdt),
                format_cypher_offset(&odt.offset())
            )
        }
        Value::Array(elems) => {
            // Cypher renders arrays as `[1, 2, 3]`, not PG's `{1,2,3}`.
            // Build the output in one buffer with a capacity hint
            // instead of paying N+2 heap allocations
            // (Vec<String> + per-element String + join String).
            let mut out = String::with_capacity(2 + elems.len() * 8);
            out.push('[');
            for (i, elem) in elems.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&format_cypher_property_value(elem));
            }
            out.push(']');
            out
        }
        Value::Jsonb(json) => format_cypher_jsonb_value(json),
        other => format!("{other}"),
    }
}

fn format_cypher_time(t: &time::Time) -> String {
    use std::fmt::Write as _;
    let nano = t.nanosecond();
    let sec = t.second();
    // Reserve enough room for `HH:MM:SS.fffffffff` so the buffer never grows.
    let mut out = String::with_capacity(18);
    let _ = write!(out, "{:02}:{:02}", t.hour(), t.minute());
    if sec != 0 || nano != 0 {
        let _ = write!(out, ":{sec:02}");
    }
    push_trimmed_nanos(&mut out, nano);
    out
}

fn format_cypher_offset(offset: &time::UtcOffset) -> String {
    let (oh, om, _) = offset.as_hms();
    let abs_om = om.unsigned_abs();
    format!("{oh:+03}:{abs_om:02}")
}

fn format_cypher_primitive_datetime(dt: &time::PrimitiveDateTime) -> String {
    use std::fmt::Write as _;
    let nano = dt.nanosecond();
    let sec = dt.second();
    let mut out = String::with_capacity(32);
    let _ = write!(
        out,
        "{:04}-{:02}-{:02}T{:02}:{:02}",
        dt.year(),
        u8::from(dt.month()),
        dt.day(),
        dt.hour(),
        dt.minute()
    );
    if sec != 0 || nano != 0 {
        let _ = write!(out, ":{sec:02}");
    }
    push_trimmed_nanos(&mut out, nano);
    out
}

/// Append `.<digits>` to `out` for a nanosecond fraction, trimming trailing
/// zeros. Skips emission entirely when `nano == 0` or when the trimmed digit
/// string is empty. Replaces a `format!`-then-`push_str` pattern with a
/// stack-buffer write - no intermediate `String` allocation.
fn push_trimmed_nanos(out: &mut String, nano: u32) {
    if nano == 0 {
        return;
    }
    // 9 ASCII digits cover 0..=999_999_999.
    let mut buf = [0u8; 9];
    let mut n = nano;
    for slot in buf.iter_mut().rev() {
        *slot = b'0' + (n % 10) as u8;
        n /= 10;
    }
    let mut end = buf.len();
    while end > 0 && buf[end - 1] == b'0' {
        end -= 1;
    }
    if end == 0 {
        return;
    }
    out.push('.');
    out.push_str(std::str::from_utf8(&buf[..end]).unwrap_or(""));
}

fn format_cypher_jsonb_value(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "null".to_owned(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => format!("'{}'", s.replace('\'', "\\'")),
        // Build the array / object layout into a single output String
        // instead of paying N+2 heap allocations per recursion level
        // (Vec<String> of formatted parts, the join() output, and the
        // outer `format!("[{}]", ...)`). The recursive call still
        // allocates one String per element, but the per-level
        // surface allocations collapse to one.
        serde_json::Value::Array(items) => {
            let mut out = String::with_capacity(2 + items.len() * 8);
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&format_cypher_jsonb_value(item));
            }
            out.push(']');
            out
        }
        serde_json::Value::Object(map) => {
            let mut out = String::with_capacity(2 + map.len() * 16);
            out.push('{');
            let mut first = true;
            for (k, v) in map {
                if !first {
                    out.push_str(", ");
                }
                first = false;
                out.push_str(k);
                out.push_str(": ");
                out.push_str(&format_cypher_jsonb_value(v));
            }
            out.push('}');
            out
        }
    }
}
