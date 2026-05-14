#![allow(
    clippy::match_same_arms,
    clippy::too_many_lines,
    clippy::wildcard_imports
)]

use super::*;

// Function name dispatch: maps Cypher function names to their SQL equivalents.
// This runs at translation time (not execution time), so string matching is acceptable.
pub(super) fn translate_function_call(
    name: &aiondb_parser::ast::ObjectName,
    args: &[Expr],
    distinct: bool,
    filter: Option<&Expr>,
) -> String {
    let fn_name_lower = name.parts.join(".").to_lowercase();
    match fn_name_lower.as_str() {
        "date" | "time" | "localtime" | "datetime" | "localdatetime" | "duration" => {
            let a: Vec<String> = args.iter().map(expr_to_sql_plain).collect();
            format!("\"cypher_{}\"({})", fn_name_lower, a.join(", "))
        }
        "date.truncate"
        | "datetime.truncate"
        | "localdatetime.truncate"
        | "time.truncate"
        | "localtime.truncate" => {
            let a: Vec<String> = args.iter().map(expr_to_sql_plain).collect();
            format!("\"{}\"({})", fn_name_lower, a.join(", "))
        }
        "duration.between" | "duration.inmonths" | "duration.indays" | "duration.inseconds" => {
            let a: Vec<String> = args.iter().map(expr_to_sql_plain).collect();
            format!("\"{}\"({})", fn_name_lower, a.join(", "))
        }
        "datetime.fromepoch" => {
            let a: Vec<String> = args.iter().map(expr_to_sql_plain).collect();
            format!("\"datetime.fromepoch\"({})", a.join(", "))
        }
        "datetime.fromepochmillis" => {
            let a: Vec<String> = args.iter().map(expr_to_sql_plain).collect();
            format!("\"datetime.fromepochmillis\"({})", a.join(", "))
        }
        "datetime.transaction"
        | "datetime.statement"
        | "datetime.realtime"
        | "date.transaction"
        | "date.statement"
        | "date.realtime"
        | "time.transaction"
        | "time.statement"
        | "time.realtime"
        | "localtime.transaction"
        | "localtime.statement"
        | "localtime.realtime"
        | "localdatetime.transaction"
        | "localdatetime.statement"
        | "localdatetime.realtime" => {
            format!("\"{fn_name_lower}\"()")
        }
        "__aiondb_composite_field" if args.len() == 2 => {
            let base = expr_to_sql_plain(&args[0]);
            if let Expr::Literal(Literal::String(field), _) = &args[1] {
                if let Some(es) = temporal_property_to_extract(field, &base) {
                    es
                } else {
                    format!("(({base})->>'{}')", escape_sq(field))
                }
            } else {
                let field = expr_to_sql_plain(&args[1]);
                format!("(({base})->>{field})")
            }
        }
        "array_get" if args.len() == 2 => {
            let base = expr_to_sql_plain(&args[0]);
            let index = expr_to_sql_plain(&args[1]);
            format!("(CASE WHEN ({index}) >= 0 THEN ({base})[({index}) + 1] ELSE ({base})[coalesce(array_length({base}, 1), 0) + ({index}) + 1] END)")
        }
        "__aiondb_array_slice" if args.len() == 5 => {
            let base = expr_to_sql_plain(&args[0]);
            let lower = expr_to_sql_plain(&args[1]);
            let upper = expr_to_sql_plain(&args[2]);
            let lo_omit = matches!(&args[3], Expr::Literal(Literal::Boolean(true), _));
            let up_omit = matches!(&args[4], Expr::Literal(Literal::Boolean(true), _));
            let lo_sql = if lo_omit {
                "1".to_string()
            } else {
                format!("(CASE WHEN ({lower}) >= 0 THEN ({lower}) + 1 ELSE GREATEST(coalesce(array_length({base}, 1), 0) + ({lower}) + 1, 1) END)")
            };
            let hi_sql = if up_omit {
                format!("coalesce(array_length({base}, 1), 0)")
            } else {
                format!("(CASE WHEN ({upper}) >= 0 THEN ({upper}) ELSE coalesce(array_length({base}, 1), 0) + ({upper}) END)")
            };
            if lo_omit && up_omit {
                base
            } else if (!lo_omit && matches!(&args[1], Expr::Literal(Literal::Null, _)))
                || (!up_omit && matches!(&args[2], Expr::Literal(Literal::Null, _)))
            {
                "NULL".into()
            } else {
                format!("({base})[{lo_sql}:{hi_sql}]")
            }
        }
        "__cypher_starts_with" if args.len() == 2 => {
            let l = expr_to_sql_plain(&args[0]);
            let r = expr_to_sql_plain(&args[1]);
            format!("(CASE WHEN ({l}) IS NULL OR ({r}) IS NULL THEN NULL::boolean ELSE ({l} LIKE ({r}) || '%') END)")
        }
        "__cypher_ends_with" if args.len() == 2 => {
            let l = expr_to_sql_plain(&args[0]);
            let r = expr_to_sql_plain(&args[1]);
            format!("(CASE WHEN ({l}) IS NULL OR ({r}) IS NULL THEN NULL::boolean ELSE ({l} LIKE '%' || ({r})) END)")
        }
        "__cypher_contains" if args.len() == 2 => {
            let l = expr_to_sql_plain(&args[0]);
            let r = expr_to_sql_plain(&args[1]);
            format!("(CASE WHEN ({l}) IS NULL OR ({r}) IS NULL THEN NULL::boolean ELSE ({l} LIKE '%' || ({r}) || '%') END)")
        }
        "__cypher_in" if args.len() == 2 => {
            let l = expr_to_sql_plain(&args[0]);
            let r = expr_to_sql_plain(&args[1]);
            format!("({l} = ANY({r}))")
        }
        "__cypher_has_label" if args.len() == 2 => {
            let var = expr_to_sql_plain(&args[0]);
            if let Expr::Literal(Literal::String(ref label), _) = args[1] {
                let safe = escape_sq(label);
                format!("('{safe}' = ANY({var}.\"__labels\"))")
            } else {
                "true".to_string()
            }
        }
        // openCypher quantifiers use three-valued logic over null
        // predicates (`bool_and`/`bool_or` ignore NULL, but Cypher must
        // surface it). Priority for each: deterministic answer first,
        // then NULL when any predicate is unknown, otherwise the
        // universal / existential identity for the remaining
        // all-true / all-false population.
        "__cypher_any" if args.len() == 3 => {
            let var = cypher_quantifier_var(&args[0]);
            let list = expr_to_sql_plain(&args[1]);
            let pred = expr_to_sql_plain(&args[2]);
            if let Some(simple) = try_simple_any_all(&args[0], &args[1], &args[2], "any") {
                simple
            } else {
                format!(
                    "(SELECT CASE \
                     WHEN count(*) = 0 THEN false \
                     WHEN bool_or(({pred}) IS TRUE) THEN true \
                     WHEN bool_or(({pred}) IS NULL) THEN NULL::boolean \
                     ELSE false END FROM unnest({list}) AS {var})"
                )
            }
        }
        "__cypher_all" if args.len() == 3 => {
            let var = cypher_quantifier_var(&args[0]);
            let list = expr_to_sql_plain(&args[1]);
            let pred = expr_to_sql_plain(&args[2]);
            if let Some(simple) = try_simple_any_all(&args[0], &args[1], &args[2], "all") {
                simple
            } else {
                format!(
                    "(SELECT CASE \
                     WHEN count(*) = 0 THEN true \
                     WHEN bool_or(({pred}) IS FALSE) THEN false \
                     WHEN bool_or(({pred}) IS NULL) THEN NULL::boolean \
                     ELSE true END FROM unnest({list}) AS {var})"
                )
            }
        }
        "__cypher_none" if args.len() == 3 => {
            let var = cypher_quantifier_var(&args[0]);
            let list = expr_to_sql_plain(&args[1]);
            let pred = expr_to_sql_plain(&args[2]);
            if let Some(simple) = try_simple_any_all(&args[0], &args[1], &args[2], "none") {
                simple
            } else {
                format!(
                    "(SELECT CASE \
                     WHEN count(*) = 0 THEN true \
                     WHEN bool_or(({pred}) IS TRUE) THEN false \
                     WHEN bool_or(({pred}) IS NULL) THEN NULL::boolean \
                     ELSE true END FROM unnest({list}) AS {var})"
                )
            }
        }
        "__cypher_single" if args.len() == 3 => {
            let var = cypher_quantifier_var(&args[0]);
            let list = expr_to_sql_plain(&args[1]);
            let pred = expr_to_sql_plain(&args[2]);
            if let Some(simple) = try_simple_any_all(&args[0], &args[1], &args[2], "single") {
                simple
            } else {
                format!(
                    "(SELECT CASE \
                     WHEN count(*) FILTER (WHERE ({pred}) IS TRUE) = 1 \
                          AND count(*) FILTER (WHERE ({pred}) IS NULL) = 0 THEN true \
                     WHEN count(*) FILTER (WHERE ({pred}) IS TRUE) > 1 THEN false \
                     WHEN count(*) FILTER (WHERE ({pred}) IS NULL) > 0 THEN NULL::boolean \
                     ELSE false END FROM unnest({list}) AS {var})"
                )
            }
        }
        "__cypher_list_comprehension" if args.len() == 4 => {
            let var = cypher_quantifier_var(&args[0]);
            let list = expr_to_sql_plain(&args[1]);
            let pred = expr_to_sql_plain(&args[2]);
            let map_e = expr_to_sql_plain(&args[3]);
            format!("COALESCE((SELECT array_agg({map_e}) FROM unnest({list}) AS {var} WHERE {pred}), ARRAY[]::bigint[])")
        }
        "toboolean" | "tobooleanornull" | "tointeger" | "tointegerornull" | "tofloat"
        | "tofloatornull" | "tostring" | "tostringornull"
            if args.len() == 1 =>
        {
            format!("{}({})", fn_name_lower, expr_to_sql_plain(&args[0]))
        }
        // Cypher camelCase string-case aliases that map to PG's
        // bare names. Without these the SQL fallback emitted
        // `toupper(...)` / `tolower(...)` literally and the
        // executor returned UndefinedFunction.
        "toupper" if args.len() == 1 => {
            format!("upper({})", expr_to_sql_plain(&args[0]))
        }
        "tolower" if args.len() == 1 => {
            format!("lower({})", expr_to_sql_plain(&args[0]))
        }
        // openCypher `exists(prop)` → `prop IS NOT NULL`. PG has
        // no scalar `exists` function — we'd otherwise return
        // UndefinedFunction at runtime.
        "exists" if args.len() == 1 => {
            format!("(({}) IS NOT NULL)", expr_to_sql_plain(&args[0]))
        }
        "substring" if !args.is_empty() && args.len() <= 3 => {
            let s = expr_to_sql_plain(&args[0]);
            if args.len() == 1 {
                s
            } else {
                let start = expr_to_sql_plain(&args[1]);
                if args.len() == 3 {
                    format!(
                        "substr({s}, ({start}) + 1, {})",
                        expr_to_sql_plain(&args[2])
                    )
                } else {
                    format!("substr({s}, ({start}) + 1)")
                }
            }
        }
        "split" if args.len() == 2 => {
            format!(
                "string_to_array({}, {})",
                expr_to_sql_plain(&args[0]),
                expr_to_sql_plain(&args[1])
            )
        }
        "collect" if args.len() == 1 => {
            let inner = expr_to_sql_plain(&args[0]);
            let d = if distinct { "DISTINCT " } else { "" };
            format!("coalesce(array_agg({d}{inner}) FILTER (WHERE ({inner}) IS NOT NULL), ARRAY[]::bigint[])")
        }
        "size" if args.len() == 1 => {
            let inner = expr_to_sql_plain(&args[0]);
            if is_string_expr(&args[0]) {
                format!("char_length({inner})")
            } else {
                format!("(CASE WHEN pg_typeof({inner})::text = 'text' OR pg_typeof({inner})::text = 'character varying' THEN char_length({inner}::text) ELSE coalesce(array_length({inner}, 1), 0) END)")
            }
        }
        "head" if args.len() == 1 => {
            let i = expr_to_sql_plain(&args[0]);
            format!("({i})[1]")
        }
        "last" if args.len() == 1 => {
            let i = expr_to_sql_plain(&args[0]);
            format!("({i})[array_length({i}, 1)]")
        }
        "tail" if args.len() == 1 => {
            format!("({})[2:]", expr_to_sql_plain(&args[0]))
        }
        "reverse" if args.len() == 1 => {
            format!("reverse({})", expr_to_sql_plain(&args[0]))
        }
        "keys" if args.len() == 1 => {
            let inner = expr_to_sql_plain(&args[0]);
            format!("ARRAY(SELECT jsonb_object_keys({inner}))")
        }
        "properties" if args.len() == 1 => expr_to_sql_plain(&args[0]),
        "range" if args.len() >= 2 && args.len() <= 3 => {
            let start = expr_to_sql_plain(&args[0]);
            let end = expr_to_sql_plain(&args[1]);
            if args.len() == 3 {
                let step = expr_to_sql_plain(&args[2]);
                format!(
                    "COALESCE(ARRAY(SELECT generate_series({start}, {end}, {step})), ARRAY[]::bigint[])"
                )
            } else {
                format!(
                    "COALESCE(ARRAY(SELECT generate_series({start}, {end}, 1)), ARRAY[]::bigint[])"
                )
            }
        }
        _ => {
            // Cypher `count(*)` parses with a star argument; emit bare `count(*)`
            // rather than `count("*")` (which references a column literally
            // named `*`).
            let star_only = args.len() == 1
                && matches!(
                    &args[0],
                    Expr::Identifier(name)
                        if name.parts.len() == 1 && name.parts[0] == "*"
                );
            if star_only {
                let mut s = format!("{}(*)", name.parts.join("."));
                if let Some(f) = filter {
                    use std::fmt::Write;
                    let _ = write!(s, " FILTER (WHERE {})", expr_to_sql_plain(f));
                }
                return s;
            }
            let a: Vec<String> = args.iter().map(expr_to_sql_plain).collect();
            let d = if distinct { "DISTINCT " } else { "" };
            let mut s = format!("{}({d}{})", name.parts.join("."), a.join(", "));
            if let Some(f) = filter {
                use std::fmt::Write;
                let _ = write!(s, " FILTER (WHERE {})", expr_to_sql_plain(f));
            }
            s
        }
    }
}
