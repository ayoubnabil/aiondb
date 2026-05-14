use std::sync::Arc;

use aiondb_engine::{DbError, DbResult, EngineBuilder, ErrorReport, SqlState};
use aiondb_pgwire::{
    codec::{read_cstring_from_buf, PROTOCOL_V3},
    connection::Connection,
    server::CancelRegistry,
};
use bytes::{Buf, BytesMut};
use tokio::io::{duplex, AsyncReadExt, AsyncWriteExt, DuplexStream};

use crate::scenario::ScenarioError;

fn message_len_u32(payload_len: usize) -> u32 {
    u32::try_from(payload_len.saturating_add(4)).expect("test-kit message length exceeds u32")
}

pub struct FaultInjectionHarness {
    stream: DuplexStream,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackendEvent {
    pub tag: u8,
    pub ready_status: Option<u8>,
    pub command_tag: Option<String>,
    pub error: Option<ScenarioError>,
}

impl FaultInjectionHarness {
    pub async fn new() -> DbResult<Self> {
        let mut stream = spawn_connection();
        write_message(&mut stream, &startup_message_bytes(), "startup").await?;
        drain_until_ready(&mut stream).await?;
        Ok(Self { stream })
    }

    pub async fn simple_query(&mut self, sql: &str) -> DbResult<Vec<BackendEvent>> {
        self.write(simple_query_bytes(sql), "simple query").await?;
        self.read_until_ready().await
    }

    pub async fn send_parse(&mut self, statement_name: &str, sql: &str) -> DbResult<()> {
        self.write(parse_message_bytes(statement_name, sql), "parse")
            .await
    }

    pub async fn send_bind(&mut self, portal_name: &str, statement_name: &str) -> DbResult<()> {
        self.write(bind_message_bytes(portal_name, statement_name), "bind")
            .await
    }

    pub async fn send_execute(&mut self, portal_name: &str, max_rows: i32) -> DbResult<()> {
        self.write(execute_message_bytes(portal_name, max_rows), "execute")
            .await
    }

    pub async fn send_sync(&mut self) -> DbResult<()> {
        self.write(sync_message_bytes(), "sync").await
    }

    pub async fn read_event(&mut self) -> DbResult<BackendEvent> {
        let message = read_backend_message(&mut self.stream).await?;
        backend_event_from_message(message)
    }

    pub async fn read_until_ready(&mut self) -> DbResult<Vec<BackendEvent>> {
        let mut events = Vec::new();
        loop {
            let event = self.read_event().await?;
            let is_ready = event.tag == b'Z';
            events.push(event);
            if is_ready {
                return Ok(events);
            }
        }
    }

    pub async fn terminate(&mut self) -> DbResult<()> {
        self.write(terminate_message_bytes(), "terminate").await
    }

    async fn write(&mut self, message: Vec<u8>, label: &str) -> DbResult<()> {
        write_message(&mut self.stream, &message, label).await
    }
}

fn spawn_connection() -> DuplexStream {
    let (client_stream, server_stream) = duplex(16 * 1024);
    let engine = Arc::new(EngineBuilder::for_testing().build().unwrap());
    tokio::spawn(async move {
        let (reader, writer) = tokio::io::split(server_stream);
        let mut connection = Connection::new(engine, reader, writer, 11, 22, CancelRegistry::new());
        let _ = connection.run().await;
    });
    client_stream
}

async fn write_message(stream: &mut DuplexStream, message: &[u8], label: &str) -> DbResult<()> {
    stream
        .write_all(message)
        .await
        .map_err(|error| DbError::protocol(format!("write {label} message: {error}")))
}

async fn drain_until_ready(stream: &mut DuplexStream) -> DbResult<()> {
    loop {
        let message = read_backend_message(stream).await?;
        match message.tag {
            b'R' => match parse_auth_request(message.payload)? {
                0 => {}
                3 => {
                    write_message(stream, &password_message_bytes(""), "password").await?;
                }
                auth_type => {
                    return Err(DbError::protocol(format!(
                        "unsupported startup auth request type: {auth_type}"
                    )));
                }
            },
            b'S' | b'K' => {}
            b'Z' => return Ok(()),
            b'E' => {
                return Err(DbError::from_report(error_report_from_response(
                    message.payload,
                )))
            }
            tag => {
                return Err(DbError::protocol(format!(
                    "unexpected startup backend message: {}",
                    tag as char
                )))
            }
        }
    }
}

fn startup_message_bytes() -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&PROTOCOL_V3.to_be_bytes());
    push_cstring(&mut payload, "user");
    push_cstring(&mut payload, "test-kit");
    push_cstring(&mut payload, "database");
    push_cstring(&mut payload, "default");
    push_cstring(&mut payload, "application_name");
    push_cstring(&mut payload, "fault-injection");
    payload.push(0);

    let mut message = Vec::with_capacity(payload.len() + 4);
    message.extend_from_slice(&message_len_u32(payload.len()).to_be_bytes());
    message.extend_from_slice(&payload);
    message
}

fn password_message_bytes(password: &str) -> Vec<u8> {
    let mut payload = Vec::with_capacity(password.len() + 1);
    push_cstring(&mut payload, password);
    tagged_message_bytes(b'p', &payload)
}

fn simple_query_bytes(sql: &str) -> Vec<u8> {
    let mut payload = Vec::new();
    push_cstring(&mut payload, sql);
    tagged_message_bytes(b'Q', &payload)
}

fn parse_message_bytes(statement_name: &str, sql: &str) -> Vec<u8> {
    let mut payload = Vec::new();
    push_cstring(&mut payload, statement_name);
    push_cstring(&mut payload, sql);
    payload.extend_from_slice(&0_i16.to_be_bytes());
    tagged_message_bytes(b'P', &payload)
}

fn bind_message_bytes(portal_name: &str, statement_name: &str) -> Vec<u8> {
    let mut payload = Vec::new();
    push_cstring(&mut payload, portal_name);
    push_cstring(&mut payload, statement_name);
    payload.extend_from_slice(&0_i16.to_be_bytes());
    payload.extend_from_slice(&0_i16.to_be_bytes());
    payload.extend_from_slice(&0_i16.to_be_bytes());
    tagged_message_bytes(b'B', &payload)
}

fn execute_message_bytes(portal_name: &str, max_rows: i32) -> Vec<u8> {
    let mut payload = Vec::new();
    push_cstring(&mut payload, portal_name);
    payload.extend_from_slice(&max_rows.to_be_bytes());
    tagged_message_bytes(b'E', &payload)
}

fn sync_message_bytes() -> Vec<u8> {
    tagged_message_bytes(b'S', &[])
}

fn terminate_message_bytes() -> Vec<u8> {
    tagged_message_bytes(b'X', &[])
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

struct BackendMessage {
    tag: u8,
    payload: BytesMut,
}

async fn read_backend_message(stream: &mut DuplexStream) -> DbResult<BackendMessage> {
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
    if payload.remaining() < 4 {
        return Err(DbError::protocol(
            "authentication request payload truncated",
        ));
    }
    Ok(payload.get_i32())
}

fn backend_event_from_message(message: BackendMessage) -> DbResult<BackendEvent> {
    match message.tag {
        b'Z' => {
            if message.payload.remaining() < 1 {
                return Err(DbError::protocol("ready for query payload truncated"));
            }
            let mut payload = message.payload;
            Ok(BackendEvent {
                tag: b'Z',
                ready_status: Some(payload.get_u8()),
                command_tag: None,
                error: None,
            })
        }
        b'C' => Ok(BackendEvent {
            tag: b'C',
            ready_status: None,
            command_tag: Some(read_cstring_from_buf(&mut message.payload.clone())?),
            error: None,
        }),
        b'E' => Ok(BackendEvent {
            tag: b'E',
            ready_status: None,
            command_tag: None,
            error: Some(parse_error_response(message.payload)),
        }),
        other => Ok(BackendEvent {
            tag: other,
            ready_status: None,
            command_tag: None,
            error: None,
        }),
    }
}

fn parse_error_response(payload: BytesMut) -> ScenarioError {
    let report = error_report_from_response(payload);
    ScenarioError {
        sqlstate: report.sqlstate.code().to_owned(),
        message: report.message,
        detail: report.client_detail,
        hint: report.client_hint,
        position: report.position,
    }
}

fn error_report_from_response(mut payload: BytesMut) -> ErrorReport {
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
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(events: &[BackendEvent]) -> Vec<u8> {
        events.iter().map(|event| event.tag).collect()
    }

    fn ready_statuses(events: &[BackendEvent]) -> Vec<u8> {
        events
            .iter()
            .filter_map(|event| event.ready_status)
            .collect()
    }

    #[tokio::test]
    async fn extended_error_skips_until_sync_and_recovers() -> DbResult<()> {
        let mut harness = FaultInjectionHarness::new().await?;

        harness.send_parse("s1", "SELECT FROM").await?;
        let parse_error = harness.read_event().await?;
        assert_eq!(parse_error.tag, b'E');
        assert_eq!(
            parse_error
                .error
                .as_ref()
                .map(|error| error.sqlstate.as_str()),
            Some(SqlState::SyntaxError.code())
        );

        harness.send_bind("p1", "s1").await?;
        harness.send_execute("p1", 0).await?;
        harness.send_sync().await?;
        let skipped_batch = harness.read_until_ready().await?;
        assert_eq!(tags(&skipped_batch), vec![b'Z']);
        assert_eq!(ready_statuses(&skipped_batch), vec![b'I']);

        harness.send_parse("s2", "SELECT 1").await?;
        harness.send_bind("p2", "s2").await?;
        harness.send_execute("p2", 0).await?;
        harness.send_sync().await?;
        let recovered_batch = harness.read_until_ready().await?;
        assert_eq!(tags(&recovered_batch), vec![b'1', b'2', b'D', b'C', b'Z']);
        assert_eq!(ready_statuses(&recovered_batch), vec![b'I']);

        harness.terminate().await?;
        Ok(())
    }

    #[tokio::test]
    async fn extended_error_in_transaction_requires_rollback() -> DbResult<()> {
        let mut harness = FaultInjectionHarness::new().await?;

        let begin = harness.simple_query("BEGIN").await?;
        assert_eq!(tags(&begin), vec![b'C', b'Z']);
        assert_eq!(ready_statuses(&begin), vec![b'T']);

        harness.send_parse("s1", "SELECT FROM").await?;
        let parse_error = harness.read_event().await?;
        assert_eq!(parse_error.tag, b'E');

        harness.send_bind("p1", "s1").await?;
        harness.send_sync().await?;
        let after_sync = harness.read_until_ready().await?;
        assert_eq!(tags(&after_sync), vec![b'Z']);
        assert_eq!(ready_statuses(&after_sync), vec![b'E']);

        let blocked = harness.simple_query("SELECT 1").await?;
        assert_eq!(tags(&blocked), vec![b'E', b'Z']);
        assert_eq!(
            blocked[0]
                .error
                .as_ref()
                .map(|error| error.sqlstate.as_str()),
            Some(SqlState::InFailedSqlTransaction.code())
        );
        assert_eq!(ready_statuses(&blocked), vec![b'E']);

        let rollback = harness.simple_query("ROLLBACK").await?;
        assert_eq!(tags(&rollback), vec![b'C', b'Z']);
        assert_eq!(ready_statuses(&rollback), vec![b'I']);

        harness.terminate().await?;
        Ok(())
    }
}
