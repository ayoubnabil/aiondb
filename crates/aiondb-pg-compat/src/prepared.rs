//! PostgreSQL-compatible helpers for PREPARE/EXECUTE handling and the
//! type-name normalisation that callers expect in error messages and
//! catalog views. Pure: no engine coupling.

pub fn compat_prepared_type_is_numeric(type_sql: &str) -> bool {
    let normalized = normalize_type_head(type_sql);
    matches!(
        normalized.as_str(),
        "int"
            | "integer"
            | "int2"
            | "int4"
            | "int8"
            | "smallint"
            | "bigint"
            | "real"
            | "float"
            | "float4"
            | "float8"
            | "double precision"
            | "numeric"
            | "decimal"
            | "money"
    )
}

pub fn compat_prepared_type_display_name(type_sql: &str) -> String {
    let normalized = normalize_type_head(type_sql);
    match normalized.as_str() {
        "float" | "float8" => "double precision".to_owned(),
        "float4" => "real".to_owned(),
        "int" | "int4" => "integer".to_owned(),
        "int8" => "bigint".to_owned(),
        "int2" => "smallint".to_owned(),
        "decimal" => "numeric".to_owned(),
        _ => normalized,
    }
}

fn normalize_type_head(type_sql: &str) -> String {
    type_sql
        .trim()
        .to_ascii_lowercase()
        .split_once('(')
        .map_or_else(
            || type_sql.trim().to_ascii_lowercase(),
            |(head, _)| head.trim().to_owned(),
        )
}
