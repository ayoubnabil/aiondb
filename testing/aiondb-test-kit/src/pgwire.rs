use std::sync::Arc;

use aiondb_engine::{
    config::MAX_SQL_LENGTH, DbError, DbResult, EngineBuilder, ErrorReport, QueryEngine, SqlState,
};
use aiondb_pgwire::{
    codec::{read_cstring_from_buf, read_i16_from_buf, read_i32_from_buf, PROTOCOL_V3},
    connection::Connection,
    server::CancelRegistry,
};
use bytes::{Buf, BytesMut};
use tokio::io::{duplex, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream};

use crate::scenario::{
    scenario_error_from_db_error, ColumnOutcome, ExecutionBatchOutcome, PortalOutcome,
    PreparedOutcome, PreparedStatementOutcome, ScenarioOperation, ScenarioOutcome, ScenarioResult,
    SqlScenario, StatementOutcome,
};

fn message_len_u32(payload_len: usize) -> u32 {
    u32::try_from(payload_len.saturating_add(4)).expect("test-kit message length exceeds u32")
}

fn len_to_i16(len: usize, context: &str) -> i16 {
    i16::try_from(len).unwrap_or_else(|_| panic!("{context} length exceeds i16"))
}

fn len_to_i32(len: usize, context: &str) -> i32 {
    i32::try_from(len).unwrap_or_else(|_| panic!("{context} length exceeds i32"))
}

fn non_negative_i16_to_usize(value: i16, context: &str) -> DbResult<usize> {
    usize::try_from(value).map_err(|_| DbError::protocol(format!("{context} negative")))
}

fn non_negative_i32_to_usize(value: i32, context: &str) -> DbResult<usize> {
    usize::try_from(value).map_err(|_| DbError::protocol(format!("{context} negative")))
}

pub async fn run_pgwire(scenario: &SqlScenario) -> DbResult<ScenarioResult> {
    let engine = Arc::new(EngineBuilder::for_testing().build().unwrap());
    run_pgwire_with_engine(engine, scenario).await
}

pub(crate) async fn run_pgwire_with_engine<E>(
    engine: Arc<E>,
    scenario: &SqlScenario,
) -> DbResult<ScenarioResult>
where
    E: QueryEngine + 'static,
{
    let mut stream = spawn_connection(engine);

    if let Err(error) =
        write_message(&mut stream, &startup_message_bytes(scenario), "startup").await
    {
        return Ok(ScenarioResult::Error(scenario_error_from_db_error(&error)));
    }
    if let Err(error) = drain_until_ready(&mut stream, scenario).await {
        return Ok(ScenarioResult::Error(scenario_error_from_db_error(&error)));
    }

    let outcome = match run_pgwire_inner(&mut stream, scenario).await {
        Ok(outcome) => ScenarioResult::Success(outcome),
        Err(error) => ScenarioResult::Error(scenario_error_from_db_error(&error)),
    };

    let _ = write_message(&mut stream, &terminate_message_bytes(), "terminate").await;
    Ok(outcome)
}

async fn run_pgwire_inner<S>(stream: &mut S, scenario: &SqlScenario) -> DbResult<ScenarioOutcome>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if let Some(setup_sql) = &scenario.setup_sql {
        write_message(stream, &simple_query_bytes(setup_sql), "setup query").await?;
        let _ = read_simple_query_outcome(stream).await?;
    }

    let outcome = match &scenario.operation {
        ScenarioOperation::Simple { sql } => {
            if sql.len() > MAX_SQL_LENGTH {
                return Err(DbError::program_limit(
                    "SQL statement exceeds maximum allowed length",
                ));
            }
            write_message(stream, &simple_query_bytes(sql), "query").await?;
            ScenarioOutcome::Simple(read_simple_query_outcome(stream).await?)
        }
        ScenarioOperation::Prepared {
            sql,
            params,
            max_rows,
        } => {
            if sql.len() > MAX_SQL_LENGTH {
                return Err(DbError::program_limit(
                    "SQL statement exceeds maximum allowed length",
                ));
            }
            let statement_name = "stmt1";
            let portal_name = "portal1";
            let max_rows = i32::try_from(*max_rows).map_err(|_| {
                DbError::protocol(format!(
                    "scenario max_rows exceeds pgwire i32 limit: {max_rows}"
                ))
            })?;

            write_message(
                stream,
                &parse_message_bytes(statement_name, sql, &[]),
                "parse",
            )
            .await?;
            write_message(stream, &flush_message_bytes(), "flush").await?;
            write_message(
                stream,
                &describe_message_bytes(b'S', statement_name),
                "describe statement",
            )
            .await?;
            write_message(stream, &sync_message_bytes(), "sync").await?;
            let statement = read_prepared_statement_outcome(stream).await?;

            let bind_values: Vec<Option<Vec<u8>>> =
                params.iter().map(|value| value.to_text_bytes()).collect();
            write_message(
                stream,
                &bind_message_bytes(portal_name, statement_name, &[], &bind_values, &[]),
                "bind",
            )
            .await?;
            write_message(stream, &flush_message_bytes(), "flush").await?;
            write_message(
                stream,
                &describe_message_bytes(b'P', portal_name),
                "describe portal",
            )
            .await?;
            let portal = read_bind_portal_outcome(stream).await?;
            let mut executions = Vec::new();
            loop {
                write_message(
                    stream,
                    &execute_message_bytes(portal_name, max_rows),
                    "execute",
                )
                .await?;
                let batch =
                    read_execute_batch_outcome(stream, !portal.result_columns.is_empty()).await?;
                let is_complete = batch.is_complete();
                executions.push(batch);
                if is_complete {
                    break;
                }
            }
            write_message(
                stream,
                &close_message_bytes(b'P', portal_name),
                "close portal",
            )
            .await?;
            write_message(
                stream,
                &close_message_bytes(b'S', statement_name),
                "close statement",
            )
            .await?;
            write_message(stream, &sync_message_bytes(), "sync").await?;
            read_close_sequence(stream).await?;
            let verification = if let Some(verify_sql) = &scenario.verify_sql {
                write_message(stream, &simple_query_bytes(verify_sql), "verify query").await?;
                Some(read_simple_query_outcome(stream).await?)
            } else {
                None
            };
            ScenarioOutcome::Prepared(PreparedOutcome {
                statement,
                portal,
                executions,
                verification,
            })
        }
    };

    Ok(outcome)
}

fn spawn_connection<E>(engine: Arc<E>) -> DuplexStream
where
    E: QueryEngine + 'static,
{
    let (client_stream, server_stream) = duplex(16 * 1024);
    tokio::spawn(async move {
        let (reader, writer) = tokio::io::split(server_stream);
        let mut connection = Connection::new(engine, reader, writer, 1, 2, CancelRegistry::new());
        let _ = connection.run().await;
    });
    client_stream
}

fn startup_message_bytes(scenario: &SqlScenario) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&PROTOCOL_V3.to_be_bytes());
    push_cstring(&mut payload, "user");
    push_cstring(&mut payload, &scenario.user);
    push_cstring(&mut payload, "database");
    push_cstring(&mut payload, &scenario.database);
    push_cstring(&mut payload, "application_name");
    push_cstring(&mut payload, &format!("pgwire:{}", scenario.name));
    payload.push(0);

    let mut message = Vec::with_capacity(payload.len() + 4);
    message.extend_from_slice(&message_len_u32(payload.len()).to_be_bytes());
    message.extend_from_slice(&payload);
    message
}

fn simple_query_bytes(sql: &str) -> Vec<u8> {
    let mut payload = Vec::with_capacity(sql.len() + 1);
    push_cstring(&mut payload, sql);
    tagged_message_bytes(b'Q', &payload)
}

fn parse_message_bytes(statement_name: &str, sql: &str, param_type_oids: &[u32]) -> Vec<u8> {
    let mut payload = Vec::new();
    push_cstring(&mut payload, statement_name);
    push_cstring(&mut payload, sql);
    payload.extend_from_slice(&len_to_i16(param_type_oids.len(), "parameter OID").to_be_bytes());
    for oid in param_type_oids {
        payload.extend_from_slice(&oid.to_be_bytes());
    }
    tagged_message_bytes(b'P', &payload)
}

fn bind_message_bytes(
    portal_name: &str,
    statement_name: &str,
    param_formats: &[i16],
    param_values: &[Option<Vec<u8>>],
    result_formats: &[i16],
) -> Vec<u8> {
    let mut payload = Vec::new();
    push_cstring(&mut payload, portal_name);
    push_cstring(&mut payload, statement_name);
    payload.extend_from_slice(&len_to_i16(param_formats.len(), "parameter format").to_be_bytes());
    for format in param_formats {
        payload.extend_from_slice(&format.to_be_bytes());
    }
    payload.extend_from_slice(&len_to_i16(param_values.len(), "parameter value").to_be_bytes());
    for value in param_values {
        match value {
            None => payload.extend_from_slice(&(-1_i32).to_be_bytes()),
            Some(value) => {
                payload
                    .extend_from_slice(&len_to_i32(value.len(), "parameter payload").to_be_bytes());
                payload.extend_from_slice(value);
            }
        }
    }
    payload.extend_from_slice(&len_to_i16(result_formats.len(), "result format").to_be_bytes());
    for format in result_formats {
        payload.extend_from_slice(&format.to_be_bytes());
    }
    tagged_message_bytes(b'B', &payload)
}

fn describe_message_bytes(target: u8, name: &str) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.push(target);
    push_cstring(&mut payload, name);
    tagged_message_bytes(b'D', &payload)
}

fn execute_message_bytes(portal_name: &str, max_rows: i32) -> Vec<u8> {
    let mut payload = Vec::new();
    push_cstring(&mut payload, portal_name);
    payload.extend_from_slice(&max_rows.to_be_bytes());
    tagged_message_bytes(b'E', &payload)
}

fn close_message_bytes(target: u8, name: &str) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.push(target);
    push_cstring(&mut payload, name);
    tagged_message_bytes(b'C', &payload)
}

fn sync_message_bytes() -> Vec<u8> {
    tagged_message_bytes(b'S', &[])
}

fn flush_message_bytes() -> Vec<u8> {
    tagged_message_bytes(b'H', &[])
}

fn terminate_message_bytes() -> Vec<u8> {
    tagged_message_bytes(b'X', &[])
}

fn password_message_bytes(password: &str) -> Vec<u8> {
    let mut payload = Vec::with_capacity(password.len() + 1);
    push_cstring(&mut payload, password);
    tagged_message_bytes(b'p', &payload)
}

fn tagged_message_bytes(tag: u8, payload: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(payload.len() + 5);
    message.push(tag);
    message.extend_from_slice(&message_len_u32(payload.len()).to_be_bytes());
    message.extend_from_slice(payload);
    message
}

fn push_cstring(buffer: &mut Vec<u8>, value: &str) {
    buffer.extend_from_slice(value.as_bytes());
    buffer.push(0);
}

async fn write_message<S>(stream: &mut S, message: &[u8], label: &str) -> DbResult<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    stream
        .write_all(message)
        .await
        .map_err(|error| DbError::protocol(format!("write {label} message: {error}")))
}

async fn drain_until_ready<S>(stream: &mut S, scenario: &SqlScenario) -> DbResult<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    loop {
        let message = read_backend_message(stream).await?;
        match message.tag {
            b'R' => match parse_auth_request(message.payload)? {
                0 => {}
                3 => {
                    let password = scenario.password.as_deref().unwrap_or("");
                    write_message(stream, &password_message_bytes(password), "password").await?;
                }
                auth_type => {
                    return Err(DbError::protocol(format!(
                        "unsupported startup auth request type: {auth_type}"
                    )));
                }
            },
            b'S' | b'K' => {}
            b'Z' => return Ok(()),
            b'E' => return Err(parse_error_response(message.payload)),
            tag => {
                return Err(DbError::protocol(format!(
                    "unexpected startup backend message: {}",
                    tag as char
                )));
            }
        }
    }
}

async fn read_simple_query_outcome<S>(stream: &mut S) -> DbResult<Vec<StatementOutcome>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut outcome = Vec::new();
    let mut current_columns: Option<Vec<ColumnOutcome>> = None;
    let mut current_rows = Vec::new();

    loop {
        let message = read_backend_message(stream).await?;
        match message.tag {
            b'N' => {}
            b'T' => {
                current_columns = Some(parse_row_description(message.payload)?);
                current_rows.clear();
            }
            b'D' => {
                current_rows.push(parse_data_row(message.payload)?);
            }
            b'C' => {
                let completion_tag = parse_command_complete(message.payload)?;
                if let Some(columns) = current_columns.take() {
                    outcome.push(StatementOutcome::Query {
                        columns: columns.into_iter().map(|column| column.name).collect(),
                        rows: std::mem::take(&mut current_rows),
                        completion_tag,
                    });
                } else {
                    outcome.push(StatementOutcome::Command { completion_tag });
                }
            }
            b'I' => outcome.push(StatementOutcome::EmptyQuery),
            b'Z' => return Ok(outcome),
            b'E' => return Err(parse_error_response(message.payload)),
            tag => {
                return Err(DbError::protocol(format!(
                    "unexpected query backend message: {}",
                    tag as char
                )));
            }
        }
    }
}

async fn read_prepared_statement_outcome<S>(stream: &mut S) -> DbResult<PreparedStatementOutcome>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut saw_parse_complete = false;
    let mut param_type_oids = None;
    let mut result_columns = None;

    loop {
        let message = read_backend_message(stream).await?;
        match message.tag {
            b'1' => saw_parse_complete = true,
            b't' => {
                param_type_oids = Some(parse_parameter_description(message.payload)?);
            }
            b'T' => {
                result_columns = Some(parse_row_description(message.payload)?);
            }
            b'n' => {
                result_columns = Some(Vec::new());
            }
            b'Z' => {
                if !saw_parse_complete {
                    return Err(DbError::protocol(
                        "missing ParseComplete in prepared statement flow",
                    ));
                }
                return Ok(PreparedStatementOutcome {
                    param_type_oids: param_type_oids.unwrap_or_default(),
                    result_columns: result_columns.unwrap_or_default(),
                });
            }
            b'E' => return Err(parse_error_response(message.payload)),
            tag => {
                return Err(DbError::protocol(format!(
                    "unexpected prepared statement backend message: {}",
                    tag as char
                )));
            }
        }
    }
}

async fn read_bind_portal_outcome<S>(stream: &mut S) -> DbResult<PortalOutcome>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut saw_bind_complete = false;
    let mut portal_columns = None;

    loop {
        let message = read_backend_message(stream).await?;
        match message.tag {
            b'2' => saw_bind_complete = true,
            b'T' => {
                portal_columns = Some(parse_row_description(message.payload)?);
            }
            b'n' => {
                portal_columns = Some(Vec::new());
            }
            tag if saw_bind_complete && portal_columns.is_some() => {
                return Err(DbError::protocol(format!(
                    "unexpected extra bind/portal backend message: {}",
                    tag as char
                )));
            }
            b'E' => return Err(parse_error_response(message.payload)),
            tag => {
                return Err(DbError::protocol(format!(
                    "unexpected bind/portal backend message: {}",
                    tag as char
                )));
            }
        }

        if saw_bind_complete && portal_columns.is_some() {
            return Ok(PortalOutcome {
                result_columns: portal_columns.unwrap_or_default(),
            });
        }
    }
}

async fn read_execute_batch_outcome<S>(
    stream: &mut S,
    query_portal: bool,
) -> DbResult<ExecutionBatchOutcome>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut rows = Vec::new();

    loop {
        let message = read_backend_message(stream).await?;
        match message.tag {
            b'N' => {}
            b'D' => rows.push(parse_data_row(message.payload)?),
            b's' => {
                if !query_portal {
                    return Err(DbError::protocol(
                        "received PortalSuspended for a portal without result columns",
                    ));
                }
                return Ok(ExecutionBatchOutcome::QuerySuspended { rows });
            }
            b'C' => {
                let completion_tag = parse_command_complete(message.payload)?;
                return Ok(if query_portal {
                    ExecutionBatchOutcome::QueryComplete {
                        rows,
                        completion_tag,
                    }
                } else {
                    ExecutionBatchOutcome::CommandComplete { completion_tag }
                });
            }
            b'E' => return Err(parse_error_response(message.payload)),
            tag => {
                return Err(DbError::protocol(format!(
                    "unexpected execute backend message: {}",
                    tag as char
                )));
            }
        }
    }
}

async fn read_close_sequence<S>(stream: &mut S) -> DbResult<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut close_completes = 0_u8;
    loop {
        let message = read_backend_message(stream).await?;
        match message.tag {
            b'3' => {
                close_completes = close_completes.saturating_add(1);
            }
            b'Z' => {
                if close_completes != 2 {
                    return Err(DbError::protocol(format!(
                        "expected 2 CloseComplete messages, got {close_completes}"
                    )));
                }
                return Ok(());
            }
            b'E' => return Err(parse_error_response(message.payload)),
            tag => {
                return Err(DbError::protocol(format!(
                    "unexpected close/sync backend message: {}",
                    tag as char
                )));
            }
        }
    }
}

impl ExecutionBatchOutcome {
    fn is_complete(&self) -> bool {
        !matches!(self, Self::QuerySuspended { .. })
    }
}

struct BackendMessage {
    tag: u8,
    payload: BytesMut,
}

async fn read_backend_message<S>(stream: &mut S) -> DbResult<BackendMessage>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut tag = [0_u8; 1];
    stream
        .read_exact(&mut tag)
        .await
        .map_err(|error| DbError::protocol(format!("read backend tag: {error}")))?;

    let mut len = [0_u8; 4];
    stream
        .read_exact(&mut len)
        .await
        .map_err(|error| DbError::protocol(format!("read backend length: {error}")))?;
    let len = u32::from_be_bytes(len);
    if len < 4 {
        return Err(DbError::protocol("backend message length too short"));
    }

    let payload_len = (len - 4) as usize;
    let mut payload = BytesMut::zeroed(payload_len);
    if payload_len > 0 {
        stream
            .read_exact(&mut payload)
            .await
            .map_err(|error| DbError::protocol(format!("read backend payload: {error}")))?;
    }

    Ok(BackendMessage {
        tag: tag[0],
        payload,
    })
}

fn parse_auth_request(mut payload: BytesMut) -> DbResult<i32> {
    read_i32_from_buf(&mut payload)
}

fn parse_parameter_description(mut payload: BytesMut) -> DbResult<Vec<u32>> {
    let parameter_count = non_negative_i16_to_usize(
        read_i16_from_buf(&mut payload)?,
        "parameter description count",
    )?;
    let mut parameter_oids = Vec::with_capacity(parameter_count);
    for _ in 0..parameter_count {
        parameter_oids.push(read_i32_from_buf(&mut payload)?.cast_unsigned());
    }
    Ok(parameter_oids)
}

fn parse_row_description(mut payload: BytesMut) -> DbResult<Vec<ColumnOutcome>> {
    let column_count =
        non_negative_i16_to_usize(read_i16_from_buf(&mut payload)?, "row description count")?;
    let mut columns = Vec::with_capacity(column_count);
    for _ in 0..column_count {
        let name = read_cstring_from_buf(&mut payload)?;
        if payload.remaining() < 18 {
            return Err(DbError::protocol("row description truncated"));
        }
        payload.advance(6);
        let type_oid = payload.get_u32();
        let type_size = payload.get_i16();
        payload.advance(6);
        columns.push(ColumnOutcome {
            name,
            type_oid,
            type_size,
        });
    }
    Ok(columns)
}

fn parse_data_row(mut payload: BytesMut) -> DbResult<Vec<Option<String>>> {
    let column_count =
        non_negative_i16_to_usize(read_i16_from_buf(&mut payload)?, "data row column count")?;
    let mut row = Vec::with_capacity(column_count);
    for _ in 0..column_count {
        let len = read_i32_from_buf(&mut payload)?;
        if len == -1 {
            row.push(None);
            continue;
        }
        let len = non_negative_i32_to_usize(len, "data row value length")?;
        if payload.remaining() < len {
            return Err(DbError::protocol("data row truncated"));
        }
        let bytes = payload.split_to(len).to_vec();
        row.push(Some(String::from_utf8_lossy(&bytes).into_owned()));
    }
    Ok(row)
}

fn parse_command_complete(mut payload: BytesMut) -> DbResult<String> {
    read_cstring_from_buf(&mut payload)
}

fn parse_error_response(mut payload: BytesMut) -> DbError {
    let mut code = None;
    let mut message = None;
    let mut detail = None;
    let mut hint = None;
    let mut position = None;

    while payload.has_remaining() {
        let field_type = payload.get_u8();
        if field_type == 0 {
            break;
        }
        let value = read_cstring_from_buf(&mut payload)
            .unwrap_or_else(|_| "invalid error field".to_owned());
        match field_type {
            b'C' => code = Some(value),
            b'M' => message = Some(value),
            b'D' => detail = Some(value),
            b'H' => hint = Some(value),
            b'P' => position = value.parse::<usize>().ok(),
            _ => {}
        }
    }

    let sqlstate = code
        .as_deref()
        .and_then(SqlState::from_code)
        .unwrap_or(SqlState::InternalError);
    let mut report = ErrorReport::new(
        sqlstate,
        message.unwrap_or_else(|| "pgwire error response without message".to_owned()),
    );
    report.client_detail = detail;
    report.client_hint = hint;
    report.position = position;
    DbError::from_report(report)
}
