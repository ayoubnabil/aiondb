//! COPY sub-protocol transport layer.
//!
//! This module implements the `PostgreSQL` COPY sub-protocol for both
//! `COPY FROM STDIN` (client-to-server) and `COPY TO STDOUT`
//! (server-to-client) directions.
//!
//! **Supported format**: text only (format code 0).
//! - Columns are tab-separated (`\t`).
//! - Rows are newline-terminated (`\n`).
//! - Data must be valid UTF-8.
//!
//! **Not supported**: binary format, CSV format, custom delimiters,
//! HEADER option, QUOTE/ESCAPE options. The parser rejects these
//! unsupported options before data reaches this layer.

use std::sync::OnceLock;
use std::time::Duration;

use super::*;

/// Default upper bound for buffered COPY FROM STDIN payload per connection.
/// Kept lower than the generic `PgWire` frame cap to reduce worst-case memory
/// amplification under many concurrent COPY sessions.
const DEFAULT_MAX_COPY_IN_BUFFER: usize = 8 * 1024 * 1024;
const MIN_COPY_IN_BUFFER: usize = 1024;
const MAX_COPY_IN_BUFFER: usize = 64 * 1024 * 1024;
const DEFAULT_COPY_IN_TOTAL_TIMEOUT: Duration = Duration::from_secs(60 * 15);
const COPY_IN_CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(100);

fn parse_max_copy_in_buffer_bytes(value: Option<&str>) -> usize {
    value
        .and_then(|raw| raw.parse::<usize>().ok())
        .filter(|value| (*value >= MIN_COPY_IN_BUFFER) && (*value <= MAX_COPY_IN_BUFFER))
        .unwrap_or(DEFAULT_MAX_COPY_IN_BUFFER)
}

fn max_copy_in_buffer_bytes() -> usize {
    static MAX_COPY_IN_BUFFER: OnceLock<usize> = OnceLock::new();
    *MAX_COPY_IN_BUFFER.get_or_init(|| {
        parse_max_copy_in_buffer_bytes(
            std::env::var("AIONDB_PGWIRE_COPY_IN_MAX_BUFFER")
                .ok()
                .as_deref(),
        )
    })
}

fn parse_copy_in_total_timeout_millis(value: Option<&str>) -> Duration {
    value
        .and_then(|raw| raw.parse::<u64>().ok())
        .map_or(DEFAULT_COPY_IN_TOTAL_TIMEOUT, Duration::from_millis)
}

fn copy_in_total_timeout() -> Duration {
    static COPY_IN_TOTAL_TIMEOUT: OnceLock<Duration> = OnceLock::new();
    *COPY_IN_TOTAL_TIMEOUT.get_or_init(|| {
        parse_copy_in_total_timeout_millis(
            std::env::var("AIONDB_PGWIRE_COPY_IN_TOTAL_TIMEOUT_MS")
                .ok()
                .as_deref(),
        )
    })
}

impl<E, R, W> Connection<E, R, W>
where
    E: PgWireEngine + 'static,
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    /// Handle the COPY IN sub-protocol: read `CopyData` messages from the
    /// client until `CopyDone` or `CopyFail`, then pass accumulated data to
    /// the engine for insertion.
    pub(super) async fn handle_copy_in_data(
        &mut self,
        table_id: aiondb_core::RelationId,
    ) -> Result<StatementResult, DbError> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| DbError::protocol("no active session"))?;

        let max_copy_in_buffer = max_copy_in_buffer_bytes();
        let mut data_buf = Vec::<u8>::with_capacity(max_copy_in_buffer.min(8192));
        let copy_in_total_timeout = copy_in_total_timeout();
        let copy_in_deadline =
            checked_deadline_after(copy_in_total_timeout, "COPY IN total timeout");
        let mut idle_deadline = checked_deadline_after(self.idle_timeout, "COPY IN idle timeout");

        loop {
            self.run_engine({
                let session = session.clone();
                move |engine| engine.as_ref().check_session_cancellation(&session)
            })
            .await?;

            let poll_timeout = if self.idle_timeout.is_zero() {
                COPY_IN_CANCEL_POLL_INTERVAL
            } else {
                self.idle_timeout.min(COPY_IN_CANCEL_POLL_INTERVAL)
            };
            let read_next_message = async {
                tokio::time::timeout(poll_timeout, codec::read_frontend_message(&mut self.reader))
                    .await
            };
            let raw = if let Some(deadline) = copy_in_deadline {
                match tokio::time::timeout_at(deadline, read_next_message).await {
                    Ok(Ok(result)) => result?,
                    Ok(Err(_)) => {
                        self.run_engine({
                            let session = session.clone();
                            move |engine| engine.as_ref().check_session_cancellation(&session)
                        })
                        .await?;
                        if let Some(idle_deadline) = idle_deadline {
                            if tokio::time::Instant::now() >= idle_deadline {
                                return Err(DbError::protocol(format!(
                                    "COPY IN idle timeout exceeded after {} ms",
                                    self.idle_timeout.as_millis()
                                )));
                            }
                        }
                        continue;
                    }
                    Err(_) => {
                        return Err(DbError::protocol(format!(
                            "COPY IN total timeout exceeded after {} ms",
                            copy_in_total_timeout.as_millis()
                        )));
                    }
                }
            } else {
                match read_next_message.await {
                    Ok(result) => result?,
                    Err(_) => {
                        self.run_engine({
                            let session = session.clone();
                            move |engine| engine.as_ref().check_session_cancellation(&session)
                        })
                        .await?;
                        if let Some(idle_deadline) = idle_deadline {
                            if tokio::time::Instant::now() >= idle_deadline {
                                return Err(DbError::protocol(format!(
                                    "COPY IN idle timeout exceeded after {} ms",
                                    self.idle_timeout.as_millis()
                                )));
                            }
                        }
                        continue;
                    }
                }
            };
            let msg = FrontendMessage::parse(raw.tag, raw.payload)?;
            idle_deadline = checked_deadline_after(self.idle_timeout, "COPY IN idle timeout");
            match msg {
                FrontendMessage::CopyData(chunk) => {
                    let projected_len =
                        data_buf.len().checked_add(chunk.len()).ok_or_else(|| {
                            DbError::protocol("COPY IN data length overflow while buffering")
                        })?;
                    if projected_len > max_copy_in_buffer {
                        return Err(DbError::protocol(format!(
                            "COPY IN data exceeds maximum buffer size ({max_copy_in_buffer} bytes)"
                        )));
                    }
                    if data_buf.try_reserve(chunk.len()).is_err() {
                        return Err(DbError::protocol(format!(
                            "COPY IN data buffer allocation refused at {projected_len} bytes"
                        )));
                    }
                    data_buf.extend_from_slice(&chunk);
                }
                FrontendMessage::CopyDone => {
                    break;
                }
                FrontendMessage::CopyFail(reason) => {
                    // Client-aborted COPY: per PG, abort the COPY but keep
                    // the connection alive. Surface as 57014 QueryCanceled
                    // so the outer extended-query loop emits ErrorResponse
                    // and resumes on Sync rather than poisoning the link.
                    return Err(DbError::query_canceled(format!(
                        "COPY FROM failed: {reason}"
                    )));
                }
                _ => {
                    return Err(DbError::protocol(
                        "unexpected message during COPY IN sub-protocol",
                    ));
                }
            }
        }

        let data_str = String::from_utf8(data_buf)
            .map_err(|_| DbError::protocol("COPY data is not valid UTF-8"))?;

        self.run_engine({
            let session = session.clone();
            move |engine| engine.execute_copy_from(&session, table_id, &data_str)
        })
        .await
    }
}

/// Write a `CopyOut` result using the `PostgreSQL` COPY sub-protocol messages.
///
/// Sends `CopyOutResponse`, then one `CopyData` per row, then `CopyDone`,
/// and finally `CommandComplete`.
pub(super) fn write_copy_out_result(
    w: &mut MessageWriter,
    data: &str,
    column_count: usize,
) -> Result<(), DbError> {
    messages::write_copy_out_response(w, column_count)?;
    let mut row_count: u64 = 0;
    for line in data.lines() {
        messages::write_copy_data_line(w, line)?;
        row_count = row_count.saturating_add(1);
    }
    messages::write_copy_done(w);
    messages::write_command_complete_with_count(w, "COPY", false, row_count);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_max_copy_in_buffer_defaults_when_missing_invalid_or_out_of_range() {
        assert_eq!(
            parse_max_copy_in_buffer_bytes(None),
            DEFAULT_MAX_COPY_IN_BUFFER
        );
        assert_eq!(
            parse_max_copy_in_buffer_bytes(Some("not-a-number")),
            DEFAULT_MAX_COPY_IN_BUFFER
        );
        assert_eq!(
            parse_max_copy_in_buffer_bytes(Some("1")),
            DEFAULT_MAX_COPY_IN_BUFFER
        );
        assert_eq!(
            parse_max_copy_in_buffer_bytes(Some(&(MAX_COPY_IN_BUFFER + 1).to_string())),
            DEFAULT_MAX_COPY_IN_BUFFER
        );
    }

    #[test]
    fn parse_max_copy_in_buffer_accepts_valid_values() {
        assert_eq!(parse_max_copy_in_buffer_bytes(Some("4096")), 4096);
        assert_eq!(
            parse_max_copy_in_buffer_bytes(Some(&MAX_COPY_IN_BUFFER.to_string())),
            MAX_COPY_IN_BUFFER
        );
    }

    #[test]
    fn parse_copy_in_total_timeout_defaults_when_missing_or_invalid() {
        assert_eq!(
            parse_copy_in_total_timeout_millis(None),
            DEFAULT_COPY_IN_TOTAL_TIMEOUT
        );
        assert_eq!(
            parse_copy_in_total_timeout_millis(Some("not-a-number")),
            DEFAULT_COPY_IN_TOTAL_TIMEOUT
        );
    }

    #[test]
    fn parse_copy_in_total_timeout_accepts_zero_and_positive_values() {
        assert_eq!(
            parse_copy_in_total_timeout_millis(Some("0")),
            Duration::ZERO
        );
        assert_eq!(
            parse_copy_in_total_timeout_millis(Some("1500")),
            Duration::from_millis(1500)
        );
    }

    #[test]
    fn write_copy_out_result_uses_explicit_column_count_for_empty_exports() {
        let mut w = MessageWriter::new();
        write_copy_out_result(&mut w, "", 2).unwrap();
        let bytes = w.finish_message();

        assert_eq!(bytes[0], b'H');
        let payload_len = u32::from_be_bytes(bytes[1..5].try_into().expect("length")) as usize;
        let payload = &bytes[5..5 + payload_len - 4];
        assert_eq!(payload[0], 0);
        assert_eq!(
            i16::from_be_bytes(payload[1..3].try_into().expect("column count")),
            2
        );
    }

    #[test]
    fn write_copy_out_result_rejects_excessive_column_count_without_partial_frames() {
        let mut w = MessageWriter::new();
        let error =
            write_copy_out_result(&mut w, "row\n", i16::MAX as usize + 1).expect_err("must fail");

        assert!(error
            .to_string()
            .contains("too many columns in COPY response"));
        assert!(w.is_empty(), "no partial COPY frames should be emitted");
    }
}
