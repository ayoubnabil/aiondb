use postgres::NoTls as SyncNoTls;
use serde_json::json;
use tokio_postgres::{types::Type as AsyncType, NoTls as AsyncNoTls};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let database_url = std::env::var("DATABASE_URL")?;
    let tokio_observed = run_tokio_postgres(&database_url).await?;
    let sync_database_url = database_url.clone();
    let postgres_observed = tokio::task::spawn_blocking(move || run_postgres(&sync_database_url))
        .await
        .map_err(|error| format!("postgres thread join failed: {error}"))??;

    println!(
        "{}",
        json!({
            "details": "tokio-postgres and postgres executed typed binary parameter roundtrips",
            "checks": [
                "tokio_postgres_binary_parameters",
                "postgres_binary_parameters",
                "int_bool_float_text_roundtrips",
                "prepared_statement_metadata",
                "tokio_postgres_array_type_metadata",
                "postgres_array_type_metadata"
            ],
            "observed": {
                "tokio_postgres": tokio_observed,
                "postgres": postgres_observed
            }
        })
    );
    Ok(())
}

async fn run_tokio_postgres(
    database_url: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let (client, connection) = tokio_postgres::connect(database_url, AsyncNoTls).await?;
    let connection_task = tokio::spawn(async move {
        if let Err(error) = connection.await {
            eprintln!("tokio-postgres connection error: {error}");
        }
    });

    let row = client
        .query_one(
            "SELECT $1::INT4, $2::INT8, $3::BOOL, $4::FLOAT8, $5::TEXT",
            &[&42_i32, &9_000_000_000_i64, &true, &3.5_f64, &"tokio"],
        )
        .await?;
    let int4: i32 = row.get(0);
    let int8: i64 = row.get(1);
    let bool_value: bool = row.get(2);
    let float8: f64 = row.get(3);
    let text: String = row.get(4);
    if (int4, int8, bool_value, float8, text.as_str()) != (42, 9_000_000_000, true, 3.5, "tokio") {
        return Err(format!(
            "tokio-postgres binary roundtrip mismatch: {int4}, {int8}, {bool_value}, {float8}, {text}"
        )
        .into());
    }

    let statement = client
        .prepare_typed(
            "SELECT $1::INT4 AS id, $2::TEXT AS name",
            &[AsyncType::INT4, AsyncType::TEXT],
        )
        .await?;
    let param_types = statement
        .params()
        .iter()
        .map(|ty| ty.name().to_owned())
        .collect::<Vec<_>>();
    let columns = statement
        .columns()
        .iter()
        .map(|column| (column.name().to_owned(), column.type_().name().to_owned()))
        .collect::<Vec<_>>();
    if param_types != ["int4", "text"] {
        return Err(format!("tokio-postgres parameter metadata mismatch: {param_types:?}").into());
    }
    if columns
        != [
            ("id".to_owned(), "int4".to_owned()),
            ("name".to_owned(), "text".to_owned()),
        ]
    {
        return Err(format!("tokio-postgres column metadata mismatch: {columns:?}").into());
    }

    let array_statement = client
        .prepare_typed(
            "SELECT $1::INT4[] AS ints, $2::TEXT[] AS labels",
            &[AsyncType::INT4_ARRAY, AsyncType::TEXT_ARRAY],
        )
        .await?;
    let array_param_types = array_statement
        .params()
        .iter()
        .map(|ty| ty.name().to_owned())
        .collect::<Vec<_>>();
    let array_columns = array_statement
        .columns()
        .iter()
        .map(|column| (column.name().to_owned(), column.type_().name().to_owned()))
        .collect::<Vec<_>>();
    if array_param_types != ["_int4", "_text"] {
        return Err(
            format!("tokio-postgres array parameter metadata mismatch: {array_param_types:?}")
                .into(),
        );
    }
    if array_columns
        != [
            ("ints".to_owned(), "_int4".to_owned()),
            ("labels".to_owned(), "_text".to_owned()),
        ]
    {
        return Err(format!("tokio-postgres array column metadata mismatch: {array_columns:?}").into());
    }
    let int_values = vec![1_i32, 2, 3];
    let text_values = vec!["north".to_owned(), "south".to_owned()];
    let array_row = client
        .query_one(&array_statement, &[&int_values, &text_values])
        .await?;
    let ints: Vec<i32> = array_row.get(0);
    let labels: Vec<String> = array_row.get(1);
    if ints != int_values || labels != text_values {
        return Err(format!("tokio-postgres array roundtrip mismatch: {ints:?}, {labels:?}").into());
    }

    drop(client);
    connection_task.await?;
    Ok(json!({
        "int4": int4,
        "int8": int8,
        "bool": bool_value,
        "float8": float8,
        "text": text,
        "params": param_types,
        "columns": columns,
        "array_params": array_param_types,
        "array_columns": array_columns,
        "arrays": {
            "ints": ints,
            "labels": labels
        }
    }))
}

fn run_postgres(
    database_url: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let mut client = postgres::Client::connect(database_url, SyncNoTls)?;
    let row = client.query_one(
        "SELECT $1::INT4, $2::INT8, $3::BOOL, $4::FLOAT8, $5::TEXT",
        &[&7_i32, &8_000_000_000_i64, &false, &2.25_f64, &"postgres"],
    )?;
    let int4: i32 = row.get(0);
    let int8: i64 = row.get(1);
    let bool_value: bool = row.get(2);
    let float8: f64 = row.get(3);
    let text: String = row.get(4);
    if (int4, int8, bool_value, float8, text.as_str())
        != (7, 8_000_000_000, false, 2.25, "postgres")
    {
        return Err(format!(
            "postgres binary roundtrip mismatch: {int4}, {int8}, {bool_value}, {float8}, {text}"
        )
        .into());
    }

    let statement = client.prepare_typed(
        "SELECT $1::INT4 AS id, $2::TEXT AS name",
        &[postgres::types::Type::INT4, postgres::types::Type::TEXT],
    )?;
    let param_types = statement
        .params()
        .iter()
        .map(|ty| ty.name().to_owned())
        .collect::<Vec<_>>();
    let columns = statement
        .columns()
        .iter()
        .map(|column| (column.name().to_owned(), column.type_().name().to_owned()))
        .collect::<Vec<_>>();
    if param_types != ["int4", "text"] {
        return Err(format!("postgres parameter metadata mismatch: {param_types:?}").into());
    }
    if columns
        != [
            ("id".to_owned(), "int4".to_owned()),
            ("name".to_owned(), "text".to_owned()),
        ]
    {
        return Err(format!("postgres column metadata mismatch: {columns:?}").into());
    }

    let array_statement = client.prepare_typed(
        "SELECT $1::INT4[] AS ints, $2::TEXT[] AS labels",
        &[
            postgres::types::Type::INT4_ARRAY,
            postgres::types::Type::TEXT_ARRAY,
        ],
    )?;
    let array_param_types = array_statement
        .params()
        .iter()
        .map(|ty| ty.name().to_owned())
        .collect::<Vec<_>>();
    let array_columns = array_statement
        .columns()
        .iter()
        .map(|column| (column.name().to_owned(), column.type_().name().to_owned()))
        .collect::<Vec<_>>();
    if array_param_types != ["_int4", "_text"] {
        return Err(format!("postgres array parameter metadata mismatch: {array_param_types:?}").into());
    }
    if array_columns
        != [
            ("ints".to_owned(), "_int4".to_owned()),
            ("labels".to_owned(), "_text".to_owned()),
        ]
    {
        return Err(format!("postgres array column metadata mismatch: {array_columns:?}").into());
    }
    let int_values = vec![4_i32, 5, 6];
    let text_values = vec!["east".to_owned(), "west".to_owned()];
    let array_row = client.query_one(&array_statement, &[&int_values, &text_values])?;
    let ints: Vec<i32> = array_row.get(0);
    let labels: Vec<String> = array_row.get(1);
    if ints != int_values || labels != text_values {
        return Err(format!("postgres array roundtrip mismatch: {ints:?}, {labels:?}").into());
    }

    Ok(json!({
        "int4": int4,
        "int8": int8,
        "bool": bool_value,
        "float8": float8,
        "text": text,
        "params": param_types,
        "columns": columns,
        "array_params": array_param_types,
        "array_columns": array_columns,
        "arrays": {
            "ints": ints,
            "labels": labels
        }
    }))
}
