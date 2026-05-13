//! Startup-parameter handling for PostgreSQL-compatible connections:
//! whitelist of known GUCs, `options=...` tokenizer, and per-assignment
//! applier. Pure: applies to a caller-owned `HashMap<String, String>`.

use std::collections::HashMap;

use tracing::warn;

/// Names of `SET`/`options=` parameters the compat layer forwards into the
/// session state. Anything outside this list is ignored (with a warning).
pub const KNOWN_STARTUP_PARAMS: &[&str] = &[
    "application_name",
    "bytea_output",
    "client_encoding",
    "client_min_messages",
    "datestyle",
    "default_transaction_isolation",
    "default_transaction_read_only",
    "default_transaction_deferrable",
    "extra_float_digits",
    "intervalstyle",
    "search_path",
    "standard_conforming_strings",
    "statement_timeout",
    "lock_timeout",
    "max_parallel_workers_per_query",
    "distributed_loopback_nodes",
    "idle_in_transaction_session_timeout",
    "timezone",
    "work_mem",
    "geqo",
    "jit",
    "row_security",
    "temp_buffers",
    "lc_collate",
    "lc_ctype",
];

/// Apply a libpq-style `options=-c name=value -c other=value` string to the
#[allow(clippy::implicit_hasher)]
pub fn apply_startup_option_string(session_variables: &mut HashMap<String, String>, raw: &str) {
    let tokens = tokenize_startup_options(raw);
    let mut index = 0usize;
    while let Some(token) = tokens.get(index) {
        if token == "-c" {
            if let Some(argument) = tokens.get(index + 1) {
                apply_startup_option_assignment(session_variables, argument);
            }
            index += 2;
            continue;
        }
        if let Some(argument) = token.strip_prefix("-c") {
            if !argument.is_empty() {
                apply_startup_option_assignment(session_variables, argument);
            }
            index += 1;
            continue;
        }
        if let Some(argument) = token.strip_prefix("--") {
            apply_startup_option_assignment(session_variables, argument);
        }
        index += 1;
    }
}

#[allow(clippy::implicit_hasher)]
pub fn apply_startup_option_assignment(
    session_variables: &mut HashMap<String, String>,
    assignment: &str,
) {
    let trimmed = assignment.trim();
    let Some((name, value)) = trimmed.split_once('=') else {
        return;
    };
    let normalized = name.to_ascii_lowercase();
    if normalized == "role" || normalized == "session_authorization" {
        warn!(param = %normalized, "ignoring unsafe startup option parameter");
        return;
    }
    if KNOWN_STARTUP_PARAMS.contains(&normalized.as_str()) {
        session_variables.insert(normalized, value.to_owned());
    } else {
        warn!(param = %normalized, "ignoring unknown startup option parameter");
    }
}

pub fn tokenize_startup_options(raw: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = raw.chars();
    let mut quote: Option<char> = None;

    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(escaped) = chars.next() {
                current.push(escaped);
            }
            continue;
        }

        if let Some(active_quote) = quote {
            if ch == active_quote {
                quote = None;
            } else {
                current.push(ch);
            }
            continue;
        }

        if ch == '\'' || ch == '"' {
            quote = Some(ch);
            continue;
        }

        if ch.is_whitespace() {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            continue;
        }

        current.push(ch);
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    tokens
}
