use diesel::connection::SimpleConnection;
use diesel::pg::PgConnection;
use diesel::prelude::*;
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("DATABASE_URL")?;
    let mut conn = PgConnection::establish(&url)?;

    conn.batch_execute(
        "DROP TABLE IF EXISTS xtask_diesel_users; \
         CREATE TABLE xtask_diesel_users (id INT NOT NULL, name TEXT NOT NULL); \
         INSERT INTO xtask_diesel_users VALUES (1, 'alice'), (2, 'bob');",
    )?;

    let lookup: String = diesel::sql_query("SELECT name FROM xtask_diesel_users WHERE id = $1")
        .bind::<diesel::sql_types::Integer, _>(2)
        .get_result::<NameRow>(&mut conn)?
        .name;

    conn.batch_execute("BEGIN; INSERT INTO xtask_diesel_users VALUES (3, 'rollback'); ROLLBACK;")?;
    let count_after_rollback: i64 = diesel::sql_query("SELECT COUNT(*) AS count FROM xtask_diesel_users")
        .get_result::<CountRow>(&mut conn)?
        .count;

    let columns = diesel::sql_query(
        "SELECT column_name AS name FROM information_schema.columns \
         WHERE table_name = 'xtask_diesel_users' ORDER BY ordinal_position",
    )
    .load::<ColumnRow>(&mut conn)?
    .into_iter()
    .map(|row| row.name)
    .collect::<Vec<_>>();

    let error_class = match diesel::sql_query("SELECT * FROM xtask_diesel_missing").execute(&mut conn) {
        Ok(_) => "00000".to_owned(),
        Err(diesel::result::Error::DatabaseError(kind, _)) => format!("{kind:?}"),
        Err(error) => format!("client:{error}"),
    };

    conn.batch_execute("DROP TABLE IF EXISTS xtask_diesel_users")?;

    println!(
        "{}",
        json!({
            "details": "Diesel executed bound parameters, rollback, information_schema and SQLSTATE checks",
            "checks": ["connect", "parameter_binding", "transaction_rollback", "information_schema", "sqlstate"],
            "observed": {
                "lookup_name": lookup,
                "columns": columns,
                "count_after_rollback": count_after_rollback,
                "error_class": error_class,
            }
        })
    );
    Ok(())
}

#[derive(QueryableByName)]
struct NameRow {
    #[diesel(sql_type = diesel::sql_types::Text)]
    name: String,
}

#[derive(QueryableByName)]
struct CountRow {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    count: i64,
}

#[derive(QueryableByName)]
struct ColumnRow {
    #[diesel(sql_type = diesel::sql_types::Text)]
    name: String,
}
