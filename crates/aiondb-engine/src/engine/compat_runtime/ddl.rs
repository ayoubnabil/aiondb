#![allow(
    clippy::assigning_clones,
    clippy::doc_markdown,
    clippy::items_after_statements,
    clippy::map_unwrap_or,
    clippy::redundant_closure_for_method_calls,
    clippy::semicolon_if_nothing_returned,
    clippy::similar_names,
    clippy::single_match_else,
    clippy::too_many_lines,
    clippy::unused_self
)]

//! `compat/ddl.rs`: DROP IF EXISTS flavors, ALTER * targets, DDL validations
//! PG-compat. Extrait de `compat/hooks.rs` (voir ADR-0004).

use super::*;
use aiondb_parser::{
    keywords::Keyword,
    lexer,
    tokens::{Token, TokenKind},
};

/// Render a tag suffix as the PG-style object kind label used in error
/// messages. Mostly the lowercased tag suffix, with a few hyphenated forms
/// matching PostgreSQL phrasing.
pub(super) fn pg_object_kind_label_from_tag(tag: &str, prefix: &str) -> String {
    let raw = tag.strip_prefix(prefix).unwrap_or(tag).to_ascii_lowercase();
    match raw.as_str() {
        "foreign data wrapper" => "foreign-data wrapper".to_owned(),
        "statistics" => "statistics object".to_owned(),
        _ => raw,
    }
}

fn lex_compat_ddl(statement_sql: &str) -> DbResult<Vec<Token>> {
    lexer::lex_sql(statement_sql)
}

fn token_word(token: &Token) -> Option<&str> {
    match &token.kind {
        TokenKind::Keyword(keyword) => Some(keyword.name()),
        TokenKind::Identifier(value) => Some(value.as_str()),
        _ => None,
    }
}

fn token_is_word_ci(token: &Token, expected: &str) -> bool {
    token_word(token).is_some_and(|word| word.eq_ignore_ascii_case(expected))
}

fn token_is_keyword(token: &Token, expected: Keyword) -> bool {
    matches!(&token.kind, TokenKind::Keyword(keyword) if *keyword == expected)
}

fn find_balanced_paren_body(tokens: &[Token], lparen_index: usize) -> Option<&[Token]> {
    let mut depth: i32 = 0;
    for idx in lparen_index..tokens.len() {
        match tokens[idx].kind {
            TokenKind::LParen => depth += 1,
            TokenKind::RParen => {
                depth -= 1;
                if depth == 0 {
                    return tokens.get(lparen_index + 1..idx);
                }
            }
            TokenKind::Eof => return None,
            _ => {}
        }
    }
    None
}

/// Locate the first balanced `OPTIONS ( ... )` clause using the official SQL
/// lexer rather than string scans. This keeps quote/comment handling aligned
/// with the parser while the remaining compat AST is still raw-SQL based.
fn find_options_clause_body_tokens(tokens: &[Token]) -> Option<&[Token]> {
    for (idx, token) in tokens.iter().enumerate() {
        if !token_is_word_ci(token, "options") {
            continue;
        }
        if tokens
            .get(idx + 1)
            .is_some_and(|next| matches!(next.kind, TokenKind::LParen))
        {
            return find_balanced_paren_body(tokens, idx + 1);
        }
    }
    None
}

/// Walk tokenized `OPTIONS ( ... )` entries and emit each option key as
/// (lowercased name, optional ADD/SET/DROP prefix). Used to detect duplicate
/// keys that PostgreSQL rejects with `option "<name>" provided more than once`.
fn iter_options_clause_token_keys(body: &[Token]) -> Vec<(String, Option<String>)> {
    let mut keys = Vec::new();
    let mut idx = 0usize;
    let mut depth: i32 = 0;
    let mut entry_start = 0usize;

    while idx <= body.len() {
        let at_entry_end =
            idx == body.len() || (matches!(body[idx].kind, TokenKind::Comma) && depth == 0);
        if at_entry_end {
            if let Some((name, prefix)) = option_entry_key(&body[entry_start..idx]) {
                keys.push((name, prefix));
            }
            entry_start = idx.saturating_add(1);
        } else {
            match body[idx].kind {
                TokenKind::LParen | TokenKind::LBracket => depth += 1,
                TokenKind::RParen | TokenKind::RBracket => depth -= 1,
                _ => {}
            }
        }
        idx += 1;
    }

    keys
}

fn option_entry_key(entry: &[Token]) -> Option<(String, Option<String>)> {
    let first_index = entry
        .iter()
        .position(|token| !matches!(token.kind, TokenKind::Eof))?;
    let first = &entry[first_index];
    let (name_token, prefix) = if token_is_keyword(first, Keyword::Add)
        || token_is_keyword(first, Keyword::Set)
        || token_is_keyword(first, Keyword::Drop)
    {
        let prefix = token_word(first)?.to_ascii_uppercase();
        (entry.get(first_index + 1)?, Some(prefix))
    } else {
        (first, None)
    };
    let name = token_word(name_token)?.to_ascii_lowercase();
    (!name.is_empty()).then_some((name, prefix))
}

fn split_top_level_commas(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth: i32 = 0;
    let mut buf = String::new();
    let mut chars = body.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '(' | '[' => {
                depth += 1;
                buf.push(ch);
            }
            ')' | ']' => {
                depth -= 1;
                buf.push(ch);
            }
            '\'' => {
                buf.push(ch);
                for inner in chars.by_ref() {
                    buf.push(inner);
                    if inner == '\'' {
                        break;
                    }
                }
            }
            ',' if depth == 0 => {
                out.push(std::mem::take(&mut buf));
            }
            _ => buf.push(ch),
        }
    }
    if !buf.is_empty() {
        out.push(buf);
    }
    out
}

fn split_statistics_key_items(raw: &str) -> Vec<String> {
    split_top_level_commas(raw)
        .into_iter()
        .map(|item| item.trim().to_owned())
        .filter(|item| !item.is_empty())
        .collect()
}

fn simple_stats_column_key(item: &str) -> Option<String> {
    let trimmed = item.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(inner) = trimmed.strip_prefix('(').and_then(|s| s.strip_suffix(')')) {
        let inner = inner.trim();
        if inner.is_empty() {
            return None;
        }
        // `(col)` is treated as a plain column key by PostgreSQL.
        if inner
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        {
            return Some(inner.to_ascii_lowercase());
        }
        return None;
    }
    if trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        Some(trimmed.to_ascii_lowercase())
    } else {
        None
    }
}

fn normalize_stats_expr_key(item: &str) -> Option<String> {
    let trimmed = item.trim();
    if !(trimmed.starts_with('(') && trimmed.ends_with(')')) {
        return None;
    }
    let mut normalized = String::with_capacity(trimmed.len());
    let mut in_string = false;
    let mut chars = trimmed.chars().peekable();
    while let Some(ch) = chars.next() {
        if in_string {
            normalized.push(ch);
            if ch == '\'' {
                if chars.peek().is_some_and(|next| *next == '\'') {
                    normalized.push('\'');
                    let _ = chars.next();
                } else {
                    in_string = false;
                }
            }
            continue;
        }
        match ch {
            '\'' => {
                in_string = true;
                normalized.push(ch);
            }
            c if c.is_whitespace() => {}
            _ => normalized.push(ch.to_ascii_lowercase()),
        }
    }
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

fn parse_compat_signed_int_literal(sql: &str, cursor: &mut usize) -> Option<i64> {
    skip_sql_whitespace(sql, cursor);
    let mut negative = false;
    if consume_punctuation(sql, cursor, '-') {
        negative = true;
    } else {
        let _ = consume_punctuation(sql, cursor, '+');
    }
    skip_sql_whitespace(sql, cursor);
    let tail = sql.get(*cursor..)?;
    let digit_count = tail.chars().take_while(|ch| ch.is_ascii_digit()).count();
    if digit_count == 0 {
        return None;
    }
    let raw_digits = tail.get(..digit_count)?;
    *cursor = cursor.saturating_add(digit_count);
    let value = raw_digits.parse::<i64>().ok()?;
    Some(if negative { -value } else { value })
}

fn compat_tail_is_empty_or_semicolon(sql: &str, cursor: &mut usize) -> bool {
    skip_sql_whitespace(sql, cursor);
    let tail = sql.get(*cursor..).map(str::trim).unwrap_or_default();
    tail.is_empty() || tail == ";"
}

fn validate_supported_alter_statistics_action(sql: &str, cursor: usize) -> DbResult<()> {
    let mut probe = cursor;
    if consume_word_ci(sql, &mut probe, "rename").is_some() {
        if consume_word_ci(sql, &mut probe, "to").is_some()
            && parse_identifier_part(sql, &mut probe).is_some()
            && compat_tail_is_empty_or_semicolon(sql, &mut probe)
        {
            return Ok(());
        }
        return Err(DbError::bind_error(
            aiondb_core::SqlState::SyntaxError,
            "syntax error in ALTER STATISTICS",
        ));
    }

    probe = cursor;
    if consume_word_ci(sql, &mut probe, "owner").is_some() {
        if consume_word_ci(sql, &mut probe, "to").is_some()
            && parse_identifier_part(sql, &mut probe).is_some()
            && compat_tail_is_empty_or_semicolon(sql, &mut probe)
        {
            return Ok(());
        }
        return Err(DbError::bind_error(
            aiondb_core::SqlState::SyntaxError,
            "syntax error in ALTER STATISTICS",
        ));
    }

    probe = cursor;
    if consume_word_ci(sql, &mut probe, "set").is_some() {
        if consume_word_ci(sql, &mut probe, "schema").is_some() {
            if parse_identifier_part(sql, &mut probe).is_some()
                && compat_tail_is_empty_or_semicolon(sql, &mut probe)
            {
                return Ok(());
            }
            return Err(DbError::bind_error(
                aiondb_core::SqlState::SyntaxError,
                "syntax error in ALTER STATISTICS",
            ));
        }
        if consume_word_ci(sql, &mut probe, "statistics").is_some() {
            if parse_compat_signed_int_literal(sql, &mut probe).is_some()
                && compat_tail_is_empty_or_semicolon(sql, &mut probe)
            {
                return Ok(());
            }
            return Err(DbError::bind_error(
                aiondb_core::SqlState::SyntaxError,
                "syntax error in ALTER STATISTICS",
            ));
        }
        return Err(DbError::feature_not_supported(
            "unsupported compatibility command: ALTER STATISTICS",
        ));
    }

    Err(DbError::feature_not_supported(
        "unsupported compatibility command: ALTER STATISTICS",
    ))
}

/// Reject `OPTIONS (key 'a', key 'b')` and similar where the same option
/// name appears twice. Matches PostgreSQL's `option "<name>" provided more
/// than once` (errcode 42710 / `duplicate_object`).
pub(super) fn check_compat_options_clause_duplicates(statement_sql: &str) -> DbResult<()> {
    let tokens = lex_compat_ddl(statement_sql)?;
    let Some(body) = find_options_clause_body_tokens(&tokens) else {
        return Ok(());
    };
    // PostgreSQL allows the special sequence `ADD <opt> '...', DROP <opt>` in a
    // single ALTER ... OPTIONS clause: the option is added then immediately
    // `ADD x, ADD x`, `SET x, SET x`, or two unprefixed entries) is rejected
    // with `option "<name>" provided more than once`.
    let mut seen: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for (name, prefix) in iter_options_clause_token_keys(body) {
        let prefix_str = prefix.unwrap_or_else(|| "NONE".to_owned());
        match seen.get(&name) {
            None => {
                seen.insert(name, prefix_str);
            }
            Some(prev) => {
                let combo = (prev.as_str(), prefix_str.as_str());
                if matches!(combo, ("ADD", "DROP") | ("DROP", "ADD")) {
                    seen.insert(name, prefix_str);
                    continue;
                }
                return Err(DbError::bind_error(
                    aiondb_core::SqlState::DuplicateObject,
                    format!("option \"{name}\" provided more than once"),
                ));
            }
        }
    }
    Ok(())
}

/// Validate the column list / table-level constraint section of
/// `CREATE FOREIGN TABLE`. PostgreSQL rejects PRIMARY KEY, REFERENCES, and
/// UNIQUE constraints on foreign tables, and rejects an empty `()` column
/// list as a syntax error. Runs before the row is recorded so the failed
/// statement does not leave a phantom entry that blocks a subsequent valid
/// `CREATE FOREIGN TABLE` for the same name.
pub(super) fn check_compat_create_foreign_table_body(statement_sql: &str) -> DbResult<()> {
    let tokens = lex_compat_ddl(trim_compat_statement(statement_sql))?;
    for (idx, token) in tokens.iter().enumerate() {
        if !matches!(token.kind, TokenKind::LParen) {
            continue;
        }
        let Some(body) = find_balanced_paren_body(&tokens, idx) else {
            return Ok(());
        };
        if body.is_empty() {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::SyntaxError,
                "syntax error at or near \")\"",
            ));
        }
        if body.windows(2).any(|pair| {
            token_is_keyword(&pair[0], Keyword::Primary) && token_is_keyword(&pair[1], Keyword::Key)
        }) {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::FeatureNotSupported,
                "primary key constraints are not supported on foreign tables",
            ));
        }
        if body
            .iter()
            .any(|token| token_is_keyword(token, Keyword::References))
        {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::FeatureNotSupported,
                "foreign key constraints are not supported on foreign tables",
            ));
        }
        if body
            .iter()
            .any(|token| token_is_keyword(token, Keyword::Unique))
        {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::FeatureNotSupported,
                "unique constraints are not supported on foreign tables",
            ));
        }
        return Ok(());
    }
    Ok(())
}

/// Reject `CREATE FOREIGN DATA WRAPPER … HANDLER … HANDLER …` (or duplicate
/// VALIDATOR / NO VALIDATOR clauses) that PostgreSQL flags as
/// "conflicting or redundant options".
pub(super) fn check_compat_create_fdw_redundant_options(statement_sql: &str) -> DbResult<()> {
    let mut handler_seen = false;
    let mut validator_seen = false;
    let mut no_validator_seen = false;
    let tokens = lex_compat_ddl(statement_sql)?;
    let mut idx = 0usize;
    while idx < tokens.len() {
        if token_is_word_ci(&tokens[idx], "handler") {
            if handler_seen {
                return Err(DbError::bind_error(
                    aiondb_core::SqlState::SyntaxError,
                    "conflicting or redundant options",
                ));
            }
            handler_seen = true;
        } else if token_is_word_ci(&tokens[idx], "validator") {
            if validator_seen || no_validator_seen {
                return Err(DbError::bind_error(
                    aiondb_core::SqlState::SyntaxError,
                    "conflicting or redundant options",
                ));
            }
            validator_seen = true;
        } else if token_is_keyword(&tokens[idx], Keyword::No)
            && tokens
                .get(idx + 1)
                .is_some_and(|next| token_is_word_ci(next, "validator"))
        {
            if validator_seen || no_validator_seen {
                return Err(DbError::bind_error(
                    aiondb_core::SqlState::SyntaxError,
                    "conflicting or redundant options",
                ));
            }
            no_validator_seen = true;
            idx += 1;
        }
        idx += 1;
    }
    Ok(())
}

#[derive(Clone)]
enum ViewCheckOptionAction {
    Reset,
    Set(String),
}

enum AlterTableRlsAction {
    SetRls(&'static str),
    SetForce(&'static str),
    Noop,
}

fn parse_create_view_check_option(statement_sql: &str) -> Option<String> {
    let lower = trim_compat_statement(statement_sql).to_ascii_lowercase();
    if !lower.contains("check option") {
        return None;
    }
    if lower.contains("local check option") {
        return Some("local".to_owned());
    }
    if lower.contains("cascaded check option") || lower.contains("check option") {
        return Some("cascaded".to_owned());
    }
    None
}

fn parse_alter_view_check_option_action(
    sql: &str,
    cursor: usize,
) -> DbResult<Option<ViewCheckOptionAction>> {
    let mut probe = cursor;
    if consume_word_ci(sql, &mut probe, "reset").is_some() {
        if consume_punctuation(sql, &mut probe, '(')
            && consume_word_ci(sql, &mut probe, "check_option").is_some()
            && consume_punctuation(sql, &mut probe, ')')
            && compat_tail_is_empty_or_semicolon(sql, &mut probe)
        {
            return Ok(Some(ViewCheckOptionAction::Reset));
        }
        return Err(DbError::bind_error(
            aiondb_core::SqlState::SyntaxError,
            "syntax error in ALTER VIEW",
        ));
    }

    probe = cursor;
    if consume_word_ci(sql, &mut probe, "set").is_some() {
        if !(consume_punctuation(sql, &mut probe, '(')
            && consume_word_ci(sql, &mut probe, "check_option").is_some()
            && consume_punctuation(sql, &mut probe, '='))
        {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::SyntaxError,
                "syntax error in ALTER VIEW",
            ));
        }
        let Some(value) = parse_identifier_part(sql, &mut probe) else {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::SyntaxError,
                "syntax error in ALTER VIEW",
            ));
        };
        let value = value.to_ascii_lowercase();
        if !matches!(value.as_str(), "local" | "cascaded") {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::SyntaxError,
                "syntax error in ALTER VIEW",
            ));
        }
        if !(consume_punctuation(sql, &mut probe, ')')
            && compat_tail_is_empty_or_semicolon(sql, &mut probe))
        {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::SyntaxError,
                "syntax error in ALTER VIEW",
            ));
        }
        return Ok(Some(ViewCheckOptionAction::Set(value)));
    }

    Ok(None)
}

/// Parsed shape for `ALTER POLICY <name> ON <table> [TO …] [USING (…)]
/// [WITH CHECK (…)]`. Each field is `Some` only when the corresponding
/// clause appears, so the runtime can update the persisted attrs in
/// place without erasing untouched fields (matching PG semantics).
#[derive(Default)]
struct ParsedAlterPolicyModifiers {
    roles: Option<String>,
    using_expr: Option<String>,
    with_check_expr: Option<String>,
}

impl ParsedAlterPolicyModifiers {
    fn has_any_modifier(&self) -> bool {
        self.roles.is_some() || self.using_expr.is_some() || self.with_check_expr.is_some()
    }
}

fn parse_alter_policy_modifiers(sql: &str, cursor: usize) -> ParsedAlterPolicyModifiers {
    let mut parsed = ParsedAlterPolicyModifiers::default();
    let mut cursor = cursor;
    loop {
        skip_sql_whitespace(sql, &mut cursor);
        if compat_tail_is_empty_or_semicolon(sql, &mut cursor) {
            break;
        }
        let mut probe = cursor;
        if consume_word_ci(sql, &mut probe, "to").is_some() {
            let mut roles: Vec<String> = Vec::new();
            loop {
                skip_sql_whitespace(sql, &mut probe);
                let lookahead = probe;
                let mut keyword_probe = probe;
                if consume_word_ci(sql, &mut keyword_probe, "using").is_some()
                    || consume_word_ci(sql, &mut keyword_probe, "with").is_some()
                {
                    probe = lookahead;
                    break;
                }
                let Some(role) = parse_identifier_part(sql, &mut probe) else {
                    break;
                };
                roles.push(role);
                skip_sql_whitespace(sql, &mut probe);
                if !sql.get(probe..).is_some_and(|tail| tail.starts_with(',')) {
                    break;
                }
                probe += 1;
            }
            if !roles.is_empty() {
                // SECURITY: see compat/mod.rs CREATE POLICY handler — the
                // role list must be joined with `|` so the executor's
                // top-level-comma split in `split_compat_options_paren_aware`
                // does not strip every role after the first.
                parsed.roles = Some(roles.join("|"));
                cursor = probe;
                continue;
            }
            return parsed;
        }
        let mut probe_using = cursor;
        if consume_word_ci(sql, &mut probe_using, "using").is_some() {
            if let Some(expr) = parse_parenthesized_expression(sql, &mut probe_using) {
                parsed.using_expr = Some(expr);
                cursor = probe_using;
                continue;
            }
            return parsed;
        }
        let mut probe_check = cursor;
        if consume_word_ci(sql, &mut probe_check, "with").is_some()
            && consume_word_ci(sql, &mut probe_check, "check").is_some()
        {
            if let Some(expr) = parse_parenthesized_expression(sql, &mut probe_check) {
                parsed.with_check_expr = Some(expr);
                cursor = probe_check;
                continue;
            }
            return parsed;
        }
        break;
    }
    parsed
}

fn parse_alter_table_rls_action(sql: &str, cursor: usize) -> DbResult<AlterTableRlsAction> {
    let mut probe = cursor;
    if consume_word_ci(sql, &mut probe, "enable").is_some()
        && consume_word_ci(sql, &mut probe, "row").is_some()
        && consume_word_ci(sql, &mut probe, "level").is_some()
        && consume_word_ci(sql, &mut probe, "security").is_some()
    {
        if compat_tail_is_empty_or_semicolon(sql, &mut probe) {
            return Ok(AlterTableRlsAction::SetRls("enabled"));
        }
        return Err(DbError::bind_error(
            aiondb_core::SqlState::SyntaxError,
            "syntax error in ALTER TABLE",
        ));
    }

    probe = cursor;
    if consume_word_ci(sql, &mut probe, "disable").is_some()
        && consume_word_ci(sql, &mut probe, "row").is_some()
        && consume_word_ci(sql, &mut probe, "level").is_some()
        && consume_word_ci(sql, &mut probe, "security").is_some()
    {
        if compat_tail_is_empty_or_semicolon(sql, &mut probe) {
            return Ok(AlterTableRlsAction::SetRls("disabled"));
        }
        return Err(DbError::bind_error(
            aiondb_core::SqlState::SyntaxError,
            "syntax error in ALTER TABLE",
        ));
    }

    probe = cursor;
    if consume_word_ci(sql, &mut probe, "force").is_some()
        && consume_word_ci(sql, &mut probe, "row").is_some()
        && consume_word_ci(sql, &mut probe, "level").is_some()
        && consume_word_ci(sql, &mut probe, "security").is_some()
    {
        if compat_tail_is_empty_or_semicolon(sql, &mut probe) {
            return Ok(AlterTableRlsAction::SetForce("force"));
        }
        return Err(DbError::bind_error(
            aiondb_core::SqlState::SyntaxError,
            "syntax error in ALTER TABLE",
        ));
    }

    probe = cursor;
    if consume_word_ci(sql, &mut probe, "no").is_some()
        && consume_word_ci(sql, &mut probe, "force").is_some()
        && consume_word_ci(sql, &mut probe, "row").is_some()
        && consume_word_ci(sql, &mut probe, "level").is_some()
        && consume_word_ci(sql, &mut probe, "security").is_some()
    {
        if compat_tail_is_empty_or_semicolon(sql, &mut probe) {
            return Ok(AlterTableRlsAction::SetForce("no_force"));
        }
        return Err(DbError::bind_error(
            aiondb_core::SqlState::SyntaxError,
            "syntax error in ALTER TABLE",
        ));
    }

    // ALTER TABLE ... OWNER TO <role>: table ownership transfer is not
    // implemented, and silently accepting it would imply an authorization
    // change that did not happen.
    let mut probe = cursor;
    if consume_word_ci(sql, &mut probe, "owner").is_some()
        && consume_word_ci(sql, &mut probe, "to").is_some()
    {
        return Err(DbError::feature_not_supported(
            "unsupported compatibility command: ALTER TABLE",
        ));
    }
    // ALTER TABLE … SET TABLESPACE / SET (option=...) and similar
    // pg_dump-style metadata changes are accepted as no-ops too: they
    // don't change query semantics in an embedded engine and refusing
    // them blocks every restore.
    let mut probe = cursor;
    if consume_word_ci(sql, &mut probe, "set").is_some() {
        return Ok(AlterTableRlsAction::Noop);
    }
    let mut probe = cursor;
    if consume_word_ci(sql, &mut probe, "reset").is_some() {
        return Ok(AlterTableRlsAction::Noop);
    }
    Err(DbError::feature_not_supported(
        "unsupported compatibility command: ALTER TABLE",
    ))
}

impl Engine {
    /// Parse `<name>` or `IF EXISTS <name>` from `sql` at `cursor`.
    ///
    /// Returns `(if_exists, rendered_name, bare_name)` where:
    /// - `rendered_name` preserves optional qualification (`schema.obj`);
    /// - `bare_name` is the unqualified tail (`obj`) used by session keys.
    ///
    /// Compatibility fallback: if the first parsed name is exactly `if`,
    /// retry once as `IF EXISTS <qualified_name>` so guarded forms don't bind
    /// to an object named `if`.
    fn parse_if_exists_object_name(
        sql: &str,
        cursor: &mut usize,
    ) -> (bool, Option<String>, Option<String>) {
        let mut if_exists = consume_if_exists(sql, cursor);
        let name_start = *cursor;
        let mut parsed_name = Self::parse_compat_misc_qualified_name(sql, cursor);

        if !if_exists
            && parsed_name
                .as_deref()
                .is_some_and(|name| name.eq_ignore_ascii_case("if"))
        {
            let mut probe = name_start;
            if consume_word_ci(sql, &mut probe, "if").is_some()
                && consume_word_ci(sql, &mut probe, "exists").is_some()
            {
                if let Some(real_name) = Self::parse_compat_misc_qualified_name(sql, &mut probe) {
                    if_exists = true;
                    parsed_name = Some(real_name);
                    *cursor = probe;
                }
            }
        }

        let Some(rendered_name) = parsed_name else {
            return (if_exists, None, None);
        };
        let bare_name = rendered_name
            .rsplit_once('.')
            .map(|(_, tail)| tail.to_owned())
            .unwrap_or_else(|| rendered_name.clone());
        (if_exists, Some(rendered_name), Some(bare_name))
    }

    pub(super) fn compatibility_drop_command_tag(statement: &Statement) -> Option<String> {
        match statement {
            Statement::DropTable(_) => Some("DROP TABLE".to_owned()),
            Statement::DropView(_) => Some("DROP VIEW".to_owned()),
            Statement::DropIndex(_) => Some("DROP INDEX".to_owned()),
            Statement::DropSequence(_) => Some("DROP SEQUENCE".to_owned()),
            Statement::DropFunction(_) => Some("DROP FUNCTION".to_owned()),
            statement if statement_compat_tag(statement).is_some() => {
                let tag = statement_compat_tag(statement).unwrap_or_default();
                Some(match tag {
                    "DROP AGGREGATE" => "DROP AGGREGATE".to_owned(),
                    "DROP CAST" => "DROP CAST".to_owned(),
                    "DROP OPERATOR" => "DROP OPERATOR".to_owned(),
                    "DROP COLLATION" => "DROP COLLATION".to_owned(),
                    "DROP CONVERSION" => "DROP CONVERSION".to_owned(),
                    "DROP DOMAIN" => "DROP DOMAIN".to_owned(),
                    "DROP FOREIGN TABLE" => "DROP FOREIGN TABLE".to_owned(),
                    "DROP TEXT" => "DROP TEXT".to_owned(),
                    "DROP TYPE" => "DROP TYPE".to_owned(),
                    "DROP PROCEDURE" => "DROP PROCEDURE".to_owned(),
                    "DROP ROUTINE" => "DROP ROUTINE".to_owned(),
                    _ => tag.to_owned(),
                })
            }
            _ => None,
        }
    }

    pub(super) fn schema_missing_notice_result(
        &self,
        tag: &str,
        schema_name: &str,
    ) -> Vec<StatementResult> {
        vec![
            StatementResult::Notice {
                message: format!("schema \"{schema_name}\" does not exist, skipping"),
            },
            super::support::command_ok(tag),
        ]
    }

    pub(super) fn collect_missing_schema_in_drop_if_exists(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
    ) -> DbResult<Option<String>> {
        let Some(rest) = parse_drop_if_exists_sql_tail(statement_sql) else {
            return Ok(None);
        };
        let mut cursor = 0usize;
        while cursor < rest.len() {
            let start = cursor;
            if let Some(schema_candidate) = parse_identifier_part(rest, &mut cursor) {
                let mut dot_cursor = cursor;
                skip_sql_whitespace(rest, &mut dot_cursor);
                if rest
                    .get(dot_cursor..)
                    .is_some_and(|tail| tail.starts_with('.'))
                    && !self.compat_schema_exists(session, &schema_candidate)?
                {
                    return Ok(Some(schema_candidate));
                }
                if cursor == start {
                    let ch = rest[cursor..].chars().next().unwrap_or('\0');
                    cursor = cursor.saturating_add(ch.len_utf8().max(1));
                }
            } else {
                let ch = rest[cursor..].chars().next().unwrap_or('\0');
                cursor = cursor.saturating_add(ch.len_utf8().max(1));
            }
        }
        Ok(None)
    }

    pub(in crate::engine) fn compat_direct_command_results(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<Option<Vec<StatementResult>>> {
        let Some(tag) = statement_compat_tag(statement) else {
            return Ok(None);
        };
        let notice = statement_compat_notice(statement).map(str::to_owned);

        let command_with_optional_notice = |tag_name: &str| {
            let mut results = Vec::new();
            if let Some(message) = notice.clone() {
                results.push(StatementResult::Notice { message });
            }
            if let Ok(pending) = self.drain_pending_notices(session) {
                for message in pending {
                    results.push(StatementResult::Notice { message });
                }
            }
            results.push(super::support::command_ok(tag_name));
            results
        };

        match tag {
            // ALTER TYPE / ALTER DOMAIN dispatched via
            // `execute_compat_type_command` / `execute_compat_domain_command`.
            // CREATE OPERATOR via `execute_compat_operator_command`.
            // CREATE RULE via `execute_compat_rule_command`.
            "REINDEX" => {
                // REINDEX has no physical rewrite implementation in AionDB.
                // Reject explicitly instead of returning a fake success.
                return Err(DbError::feature_not_supported(
                    "unsupported compatibility command: REINDEX",
                ));
            }
            "CLUSTER" => {
                // CLUSTER has no physical rewrite implementation in AionDB.
                // Reject explicitly instead of returning a fake success.
                return Err(DbError::feature_not_supported(
                    "unsupported compatibility command: CLUSTER",
                ));
            }
            "REFRESH" => {
                self.refresh_materialized_view(session, statement_sql)?;
                return Ok(Some(command_with_optional_notice(
                    "REFRESH MATERIALIZED VIEW",
                )));
            }
            "ALTER TABLE" => {
                self.validate_compat_alter_table_target(session, statement_sql)?;
                self.apply_compat_alter_table_trigger_state(session, statement_sql)?;
                return Ok(Some(command_with_optional_notice("ALTER TABLE")));
            }
            // ALTER TRIGGER dispatched via `execute_compat_trigger_command`.
            "ALTER FUNCTION" => {
                return Err(DbError::feature_not_supported(
                    "unsupported compatibility command: ALTER FUNCTION",
                ));
            }
            "ALTER SCHEMA" => {
                return Err(DbError::feature_not_supported(
                    "unsupported compatibility command: ALTER SCHEMA",
                ));
            }
            "ALTER DEFAULT" => {
                return Err(DbError::feature_not_supported(
                    "unsupported compatibility command: ALTER DEFAULT",
                ));
            }
            "ALTER COLLATION" => {
                return Err(DbError::feature_not_supported(
                    "unsupported compatibility command: ALTER COLLATION",
                ));
            }
            "ALTER EXTENSION" => {
                return Err(DbError::feature_not_supported(
                    "unsupported compatibility command: ALTER EXTENSION",
                ));
            }
            "ALTER OPERATOR" => {
                return Err(DbError::feature_not_supported(
                    "unsupported compatibility command: ALTER OPERATOR",
                ));
            }
            "ALTER TABLESPACE" => {
                return Err(DbError::feature_not_supported(
                    "unsupported compatibility command: ALTER TABLESPACE",
                ));
            }
            "ALTER TEXT SEARCH" => {
                return Err(DbError::feature_not_supported(
                    "unsupported compatibility command: ALTER TEXT SEARCH",
                ));
            }
            "ALTER INDEX" => {
                self.validate_compat_alter_index_target(session, statement_sql)?;
                return Ok(Some(command_with_optional_notice("ALTER INDEX")));
            }
            "ALTER SEQUENCE" => {
                return Err(DbError::feature_not_supported(
                    "unsupported compatibility command: ALTER SEQUENCE",
                ));
            }
            "ALTER VIEW" => {
                self.validate_compat_alter_view_target(session, statement_sql, tag)?;
                return Ok(Some(command_with_optional_notice(tag)));
            }
            "ALTER MATERIALIZED" => {
                return Err(DbError::feature_not_supported(
                    "unsupported compatibility command: ALTER MATERIALIZED",
                ));
            }
            "ALTER AGGREGATE" => {
                return Err(DbError::feature_not_supported(
                    "unsupported compatibility command: ALTER AGGREGATE",
                ));
            }
            "ALTER PROCEDURE" => {
                return Err(DbError::feature_not_supported(
                    "unsupported compatibility command: ALTER PROCEDURE",
                ));
            }
            "ALTER PUBLICATION" => {
                return Err(DbError::feature_not_supported(
                    "unsupported compatibility command: ALTER PUBLICATION",
                ));
            }
            "ALTER SUBSCRIPTION" => {
                return Err(DbError::feature_not_supported(
                    "unsupported compatibility command: ALTER SUBSCRIPTION",
                ));
            }
            "ALTER SERVER" => {
                return Err(DbError::feature_not_supported(
                    "unsupported compatibility command: ALTER SERVER",
                ));
            }
            "ALTER USER MAPPING" => {
                return Err(DbError::feature_not_supported(
                    "unsupported compatibility command: ALTER USER MAPPING",
                ));
            }
            "ALTER FOREIGN DATA WRAPPER" => {
                return Err(DbError::feature_not_supported(
                    "unsupported compatibility command: ALTER FOREIGN DATA WRAPPER",
                ));
            }
            "ALTER FOREIGN TABLE" => {
                return Err(DbError::feature_not_supported(
                    "unsupported compatibility command: ALTER FOREIGN TABLE",
                ));
            }
            "CREATE TEXT SEARCH" => {
                // Minimal compat support for CREATE TEXT SEARCH {DICTIONARY |
                // CONFIGURATION | PARSER | TEMPLATE}: accept the DDL and let
                // compat misc storage persist object metadata.
                self.record_compat_misc_create(session, tag, statement_sql)?;
                return Ok(Some(command_with_optional_notice(tag)));
            }
            "CREATE PUBLICATION"
            | "CREATE SUBSCRIPTION"
            | "CREATE USER MAPPING"
            | "CREATE FOREIGN TABLE" => {
                let parsed_name = parse_compat_create_object_name(tag, statement_sql);
                let resolved_name = if tag == "CREATE USER MAPPING" {
                    match parsed_name.as_deref() {
                        Some(name) => self.resolve_compat_user_mapping_name(session, name)?,
                        None => String::new(),
                    }
                } else {
                    parsed_name.unwrap_or_default()
                };
                let attrs = extract_compat_create_attrs(tag, statement_sql);
                self.validate_compat_create_misc_references(
                    session,
                    tag,
                    &resolved_name,
                    &attrs,
                    statement_sql,
                )?;
                return Err(DbError::feature_not_supported(format!(
                    "unsupported compatibility command: {tag}"
                )));
            }
            "DROP TABLESPACE" => {
                return Err(DbError::feature_not_supported(
                    "unsupported compatibility command: DROP TABLESPACE",
                ));
            }
            "DROP COLLATION" => {
                return Err(DbError::feature_not_supported(
                    "unsupported compatibility command: DROP COLLATION",
                ));
            }
            "DROP STATISTICS" => {
                return Err(DbError::feature_not_supported(
                    "unsupported compatibility command: DROP STATISTICS",
                ));
            }
            "DROP USER MAPPING" => {
                return Err(DbError::feature_not_supported(
                    "unsupported compatibility command: DROP USER MAPPING",
                ));
            }
            "DROP SERVER" => {
                return Err(DbError::feature_not_supported(
                    "unsupported compatibility command: DROP SERVER",
                ));
            }
            "DROP PUBLICATION" => {
                return Err(DbError::feature_not_supported(
                    "unsupported compatibility command: DROP PUBLICATION",
                ));
            }
            "DROP SUBSCRIPTION" => {
                return Err(DbError::feature_not_supported(
                    "unsupported compatibility command: DROP SUBSCRIPTION",
                ));
            }
            "DROP FOREIGN DATA WRAPPER" => {
                return Err(DbError::feature_not_supported(
                    "unsupported compatibility command: DROP FOREIGN DATA WRAPPER",
                ));
            }
            "DROP FOREIGN TABLE" => {
                return Err(DbError::feature_not_supported(
                    "unsupported compatibility command: DROP FOREIGN TABLE",
                ));
            }
            // DROP OPERATOR is dispatched via
            // `execute_compat_operator_command` (ADR-0004 typed dispatcher).
            // DROP RULE is dispatched via `execute_compat_rule_command`.
            _ => {}
        }

        // Misc-object dispatch: intrinsic lists
        // replace the per-tag matrix entries that used to carry
        // `CompatDispatch::{Alter,Drop}MiscObject`. The helpers in
        // `aiondb_pg_compat::compat_tag_matrix` encode the full set
        // of ALTER/DROP tags that share the two uniform validators.
        use aiondb_pg_compat::compat_tag_matrix::{
            is_alter_misc_object_tag, is_drop_misc_object_tag,
        };
        if is_alter_misc_object_tag(tag) {
            self.validate_compat_alter_misc_object(session, tag, statement_sql)?;
            return Ok(Some(command_with_optional_notice(tag)));
        }
        if is_drop_misc_object_tag(tag) {
            let notice_opt = self.validate_compat_drop_misc_object(session, tag, statement_sql)?;
            let mut results = Vec::new();
            if let Some(notice) = notice_opt {
                results.push(StatementResult::Notice { message: notice });
            }
            results.push(super::support::command_ok(tag));
            return Ok(Some(results));
        }

        Ok(None)
    }

    pub(super) fn validate_compat_drop_misc_object(
        &self,
        session: &SessionHandle,
        tag: &str,
        statement_sql: &str,
    ) -> DbResult<Option<String>> {
        if tag != "DROP POLICY" {
            return Ok(None);
        }
        let sql = trim_compat_statement(statement_sql);
        let mut cursor = 0usize;
        consume_word_ci(sql, &mut cursor, "drop")
            .ok_or_else(|| DbError::internal("DROP compat validator invoked on non-DROP SQL"))?;
        if consume_word_phrase_ci(sql, &mut cursor, tag.strip_prefix("DROP ").unwrap_or(tag))
            .is_none()
        {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::SyntaxError,
                "syntax error in DROP POLICY",
            ));
        }
        // `DROP POLICY name ON table` keeps the policy keyed by both
        // policy name and target table so identical names on different
        // tables don't collide.
        let if_exists = consume_if_exists(sql, &mut cursor);
        let Some(policy_name) = parse_identifier_part(sql, &mut cursor) else {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::SyntaxError,
                "syntax error in DROP POLICY",
            ));
        };
        if consume_word_ci(sql, &mut cursor, "on").is_none() {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::SyntaxError,
                "syntax error in DROP POLICY",
            ));
        }
        let Some(table_name) = Self::parse_compat_misc_qualified_name(sql, &mut cursor) else {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::SyntaxError,
                "syntax error in DROP POLICY",
            ));
        };
        let bare_table = table_name
            .rsplit_once('.')
            .map(|(_, tail)| tail.to_owned())
            .unwrap_or_else(|| table_name.clone());
        if !compat_tail_is_empty_or_semicolon(sql, &mut cursor) {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::SyntaxError,
                "syntax error in DROP POLICY",
            ));
        }
        let (object_kind, canonical_name) = (
            format!("policy \"{policy_name}\" for table \"{bare_table}\""),
            format!(
                "{}@@{}",
                policy_name.to_ascii_lowercase(),
                table_name.to_ascii_lowercase()
            ),
        );
        let key = ("CREATE POLICY".to_owned(), canonical_name);
        let known = self.with_session(session, |record| {
            Ok(record.compat_misc_objects.contains_key(&key))
        })?;
        if known {
            if let Some((_, table_name)) = key.1.split_once("@@") {
                self.validate_compat_policy_table_owner(session, table_name, "relation")?;
            }
            self.with_session_mut(session, |record| {
                record.compat_misc_objects.remove(&key);
                record.compat_misc_attrs.remove(&key);
                Ok(())
            })?;
            return Ok(None);
        }
        if if_exists {
            Ok(Some(format!("{object_kind} does not exist, skipping")))
        } else {
            Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedObject,
                format!("{object_kind} does not exist"),
            ))
        }
    }

    pub(super) fn compat_current_user_lower(&self, session: &SessionHandle) -> DbResult<String> {
        self.with_session(session, |record| {
            Ok(super::session_vars::current_user_for_record(record).to_ascii_lowercase())
        })
    }

    pub(super) fn compat_misc_owner(
        &self,
        session: &SessionHandle,
        key: &(String, String),
    ) -> DbResult<String> {
        self.with_session(session, |record| {
            Ok(record
                .compat_misc_attrs
                .get(key)
                .and_then(|attrs| attrs.owner.as_ref())
                .map(|owner| owner.to_ascii_lowercase())
                .unwrap_or_default())
        })
    }

    pub(super) fn compat_current_user_has_role(
        &self,
        session: &SessionHandle,
        target_role: &str,
    ) -> DbResult<bool> {
        if target_role.is_empty() {
            return Ok(false);
        }
        let txn_id = self.current_txn_id(session)?;
        let current_user = self.compat_current_user_lower(session)?;
        let mut visited = std::collections::BTreeSet::new();
        let mut frontier = vec![current_user];
        while let Some(role_name) = frontier.pop() {
            let normalized = role_name.to_ascii_lowercase();
            if !visited.insert(normalized.clone()) {
                continue;
            }
            if normalized.eq_ignore_ascii_case(target_role) {
                return Ok(true);
            }
            for privilege in self.catalog_reader.get_privileges(txn_id, &role_name)? {
                if let PrivilegeTarget::Role(member_of) = privilege.target {
                    let member_normalized = member_of.to_ascii_lowercase();
                    if member_normalized.eq_ignore_ascii_case(target_role) {
                        return Ok(true);
                    }
                    if !visited.contains(&member_normalized) {
                        frontier.push(member_of);
                    }
                }
            }
        }
        Ok(false)
    }

    fn validate_compat_policy_table_owner(
        &self,
        session: &SessionHandle,
        table_name: &str,
        relation_kind: &str,
    ) -> DbResult<()> {
        if self.compat_session_is_superuser(session)? {
            return Ok(());
        }
        let bare_table = table_name
            .rsplit_once('.')
            .map(|(_, tail)| tail)
            .unwrap_or(table_name)
            .to_owned();
        let current = self.compat_current_user_lower(session)?;
        let txn_id = self.current_txn_id(session)?;
        let owner = self
            .resolve_compat_table_name(session, txn_id, table_name)?
            .and_then(|table| table.owner)
            .map(|owner| owner.to_ascii_lowercase())
            .unwrap_or_default();
        if owner.is_empty()
            || owner == current
            || self.compat_current_user_has_role(session, &owner)?
        {
            return Ok(());
        }
        Err(DbError::bind_error(
            aiondb_core::SqlState::InsufficientPrivilege,
            format!("must be owner of {relation_kind} {bare_table}"),
        ))
    }

    pub(super) fn render_drop_function_notice_signature(
        function_name: &str,
        args: &[ParsedCompatTypeRef],
    ) -> String {
        let rendered = args
            .iter()
            .map(|arg| {
                if let Some(schema_name) = &arg.schema_name {
                    format!("{schema_name}.{}", arg.type_name)
                } else {
                    Self::render_drop_notice_pg_type_name(&arg.type_name)
                }
            })
            .collect::<Vec<_>>()
            .join(",");
        format!("{function_name}({rendered})")
    }

    pub(in crate::engine) fn compat_drop_if_exists_notice_results(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<Option<Vec<StatementResult>>> {
        if !trim_compat_statement(statement_sql)
            .get(..4)
            .is_some_and(|head| head.eq_ignore_ascii_case("drop"))
        {
            return Ok(None);
        }

        if find_ascii_case_insensitive(statement_sql, "if exists").is_none() {
            return Ok(None);
        }

        if let Some(schema_name) =
            self.collect_missing_schema_in_drop_if_exists(session, statement_sql)?
        {
            if let Some(tag) = Self::compatibility_drop_command_tag(statement) {
                return Ok(Some(self.schema_missing_notice_result(&tag, &schema_name)));
            }
        }

        if let Statement::DropFunction(drop_function) = statement {
            if drop_function.if_exists {
                if let Some((function_name, parsed_args)) =
                    parse_drop_function_if_exists_signature(statement_sql)
                {
                    if let Some(missing_type) = parsed_args
                        .iter()
                        .find(|arg| arg.schema_name.is_none())
                        .map(|arg| arg.type_name.clone())
                        .filter(|type_name| {
                            !self
                                .compat_type_exists_for_drop_notice(session, type_name)
                                .unwrap_or(true)
                        })
                    {
                        return Ok(Some(vec![
                            StatementResult::Notice {
                                message: format!(
                                    "type \"{missing_type}\" does not exist, skipping"
                                ),
                            },
                            super::support::command_ok("DROP FUNCTION"),
                        ]));
                    }

                    let txn_id = self.current_txn_id(session)?;
                    let resolved_name = drop_function.name.clone();
                    let overloads: Vec<FunctionDescriptor> = self
                        .catalog_reader
                        .list_functions(txn_id)?
                        .into_iter()
                        .filter(|func| func.name.eq_ignore_ascii_case(&resolved_name))
                        .collect();
                    if !(drop_function.arg_types.is_none() && overloads.len() > 1) {
                        let signature = drop_function.arg_types.as_deref().unwrap_or(&[]);
                        let matched = if drop_function.arg_types.is_some() {
                            overloads.iter().any(|func| {
                                func.params.len() == signature.len()
                                    && func
                                        .params
                                        .iter()
                                        .zip(signature.iter())
                                        .all(|(param, expected)| param.data_type == *expected)
                            })
                        } else {
                            !overloads.is_empty()
                        };
                        if !matched {
                            let rendered = Self::render_drop_function_notice_signature(
                                &function_name,
                                &parsed_args,
                            );
                            return Ok(Some(vec![
                                StatementResult::Notice {
                                    message: format!(
                                        "function {rendered} does not exist, skipping"
                                    ),
                                },
                                super::support::command_ok("DROP FUNCTION"),
                            ]));
                        }
                    }
                }
            }
        }

        if let Some(tag) = statement_compat_tag(statement) {
            if tag == "DROP CAST" {
                if let Some((source, target)) = parse_drop_cast_if_exists_types(statement_sql) {
                    for type_ref in [&source, &target] {
                        if type_ref.schema_name.is_none()
                            && !self
                                .compat_type_exists_for_drop_notice(session, &type_ref.type_name)?
                        {
                            return Ok(Some(vec![
                                StatementResult::Notice {
                                    message: format!(
                                        "type \"{}\" does not exist, skipping",
                                        type_ref.type_name
                                    ),
                                },
                                super::support::command_ok("DROP CAST"),
                            ]));
                        }
                    }
                    let source_name = source.schema_name.as_ref().map_or_else(
                        || source.type_name.clone(),
                        |schema| format!("{schema}.{}", source.type_name),
                    );
                    let target_name = target.schema_name.as_ref().map_or_else(
                        || target.type_name.clone(),
                        |schema| format!("{schema}.{}", target.type_name),
                    );
                    return Ok(Some(vec![
                        StatementResult::Notice {
                            message: format!(
                                "cast from type {source_name} to type {target_name} does not exist, skipping"
                            ),
                        },
                        super::support::command_ok("DROP CAST"),
                    ]));
                }
            }

            if tag == "DROP AGGREGATE" {
                if let Some((aggregate_name, arg_types)) =
                    parse_drop_aggregate_if_exists_signature(statement_sql)
                {
                    if let Some(missing_type) = arg_types
                        .iter()
                        .find(|arg| arg.schema_name.is_none())
                        .map(|arg| arg.type_name.clone())
                        .filter(|type_name| {
                            !self
                                .compat_type_exists_for_drop_notice(session, type_name)
                                .unwrap_or(true)
                        })
                    {
                        return Ok(Some(vec![
                            StatementResult::Notice {
                                message: format!(
                                    "type \"{missing_type}\" does not exist, skipping"
                                ),
                            },
                            super::support::command_ok("DROP AGGREGATE"),
                        ]));
                    }
                    let rendered_args = arg_types
                        .iter()
                        .map(|arg| {
                            if arg.schema_name.is_some() {
                                arg.type_name.clone()
                            } else {
                                Self::render_drop_notice_pg_type_name(&arg.type_name)
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(",");
                    return Ok(Some(vec![
                        StatementResult::Notice {
                            message: format!(
                                "aggregate {aggregate_name}({rendered_args}) does not exist, skipping"
                            ),
                        },
                        super::support::command_ok("DROP AGGREGATE"),
                    ]));
                }
            }
        }

        Ok(None)
    }

    pub(in crate::engine) fn validate_compat_alter_table_target(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
    ) -> DbResult<()> {
        let sql = trim_compat_statement(statement_sql);
        let mut cursor = 0usize;
        consume_word_ci(sql, &mut cursor, "alter").ok_or_else(|| {
            DbError::internal("ALTER TABLE compat validator invoked on non-ALTER SQL")
        })?;
        consume_word_ci(sql, &mut cursor, "table").ok_or_else(|| {
            DbError::internal("ALTER TABLE compat validator expected TABLE keyword")
        })?;
        let if_exists = consume_if_exists(sql, &mut cursor);
        let _ = consume_word_ci(sql, &mut cursor, "only");
        let Some(name) = Self::parse_compat_misc_qualified_name(sql, &mut cursor) else {
            let _ = if_exists;
            return Err(DbError::bind_error(
                aiondb_core::SqlState::SyntaxError,
                "syntax error in ALTER TABLE",
            ));
        };
        let txn_id = self.current_txn_id(session)?;
        let resolved_table = self.resolve_compat_table_name(session, txn_id, &name)?;
        if resolved_table.is_none() && !if_exists {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedTable,
                format!("relation \"{name}\" does not exist"),
            ));
        }
        Ok(())
    }

    pub(in crate::engine) fn apply_compat_alter_table_trigger_state(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
    ) -> DbResult<()> {
        let sql = trim_compat_statement(statement_sql);
        let mut cursor = 0usize;
        if consume_word_ci(sql, &mut cursor, "alter").is_none()
            || consume_word_ci(sql, &mut cursor, "table").is_none()
        {
            return Ok(());
        }
        let if_exists = consume_if_exists(sql, &mut cursor);
        let _ = consume_word_ci(sql, &mut cursor, "only");
        let Some(table_name) = Self::parse_compat_misc_qualified_name(sql, &mut cursor) else {
            return Ok(());
        };
        let action = parse_alter_table_rls_action(sql, cursor)?;
        let table_lc = table_name
            .rsplit_once('.')
            .map(|(_, tail)| tail.to_ascii_lowercase())
            .unwrap_or_else(|| table_name.to_ascii_lowercase());

        let txn_id = self.current_txn_id(session)?;
        let resolved_table = self.resolve_compat_table_name(session, txn_id, &table_name)?;
        if resolved_table.is_none() {
            if if_exists {
                self.with_session_mut(session, |record| {
                    record.push_notice(format!(
                        "relation \"{table_name}\" does not exist, skipping"
                    ));
                    Ok(())
                })?;
                return Ok(());
            }
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedTable,
                format!("relation \"{table_name}\" does not exist"),
            ));
        }

        // Ownership gate: non-superusers must own the table to ALTER it.
        let identity = self.session_info(session)?.identity.clone();
        if crate::catalog_authorizer::catalog_has_any_roles(self.catalog_reader.as_ref())?
            && !crate::catalog_authorizer::is_superuser(self.catalog_reader.as_ref(), &identity)
        {
            let qualified = aiondb_catalog::QualifiedName::parse(&table_name);
            if let Some(desc) = self.catalog_reader.get_table(txn_id, &qualified)? {
                if let Some(owner) = desc.owner.as_deref() {
                    let owner_match = identity.roles.iter().any(|r| r.eq_ignore_ascii_case(owner));
                    if !owner_match {
                        return Err(aiondb_core::DbError::insufficient_privilege(
                            "must be owner of table",
                        ));
                    }
                }
            }
        }

        // Supported compat ALTER TABLE subforms:
        // - {ENABLE|DISABLE} ROW LEVEL SECURITY
        // - {FORCE|NO FORCE} ROW LEVEL SECURITY
        let upsert_rls_flags = |rls: Option<&str>, force: Option<&str>| -> DbResult<()> {
            self.with_session_mut(session, |record| {
                let key = ("CREATE TABLE".to_owned(), table_lc.clone());
                let attrs = record.compat_misc_attrs.entry(key).or_default();
                if let Some(value) = rls {
                    upsert_option(&mut attrs.options, "rls", value);
                }
                if let Some(value) = force {
                    upsert_option(&mut attrs.options, "rls_force", value);
                }
                Ok(())
            })
        };

        match action {
            AlterTableRlsAction::SetRls(value) => upsert_rls_flags(Some(value), None),
            AlterTableRlsAction::SetForce(value) => upsert_rls_flags(None, Some(value)),
            AlterTableRlsAction::Noop => Ok(()),
        }
    }

    #[allow(unused_assignments)]
    pub(super) fn validate_compat_alter_misc_object(
        &self,
        session: &SessionHandle,
        tag: &str,
        statement_sql: &str,
    ) -> DbResult<()> {
        let sql = trim_compat_statement(statement_sql);
        let mut cursor = 0usize;
        consume_word_ci(sql, &mut cursor, "alter")
            .ok_or_else(|| DbError::internal("ALTER compat validator invoked on non-ALTER SQL"))?;
        if consume_word_phrase_ci(sql, &mut cursor, tag.strip_prefix("ALTER ").unwrap_or(tag))
            .is_none()
        {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::SyntaxError,
                format!("syntax error in {tag}"),
            ));
        }
        let create_tag = tag.replacen("ALTER", "CREATE", 1);
        let object_kind = pg_object_kind_label_from_tag(tag, "ALTER ");

        let if_exists;
        let mut rendered_name_for_message: Option<String> = None;
        let key = if tag == "ALTER USER MAPPING" {
            if_exists = consume_if_exists(sql, &mut cursor);
            let _ = consume_word_ci(sql, &mut cursor, "for");
            let Some(role) = parse_identifier_part(sql, &mut cursor) else {
                return Ok(());
            };
            if consume_word_ci(sql, &mut cursor, "server").is_none() {
                return Ok(());
            }
            let Some(server) = parse_identifier_part(sql, &mut cursor) else {
                return Ok(());
            };
            let raw_name = format!(
                "{}@{}",
                role.to_ascii_lowercase(),
                server.to_ascii_lowercase()
            );
            (
                create_tag,
                self.resolve_compat_user_mapping_name(session, &raw_name)?,
            )
        } else {
            let (parsed_if_exists, parsed_rendered_name, parsed_bare_name) =
                Self::parse_if_exists_object_name(sql, &mut cursor);
            if_exists = parsed_if_exists;
            let Some(rendered_name) = parsed_rendered_name else {
                return Err(DbError::bind_error(
                    aiondb_core::SqlState::SyntaxError,
                    format!("syntax error in {tag}"),
                ));
            };
            let Some(name) = parsed_bare_name else {
                return Err(DbError::bind_error(
                    aiondb_core::SqlState::SyntaxError,
                    format!("syntax error in {tag}"),
                ));
            };
            rendered_name_for_message = Some(rendered_name);
            // ALTER POLICY name ON table: the real identity is the policy
            // name combined with the table.
            if tag == "ALTER POLICY" {
                if if_exists {
                    return Err(DbError::bind_error(
                        aiondb_core::SqlState::SyntaxError,
                        "syntax error in ALTER POLICY",
                    ));
                }
                if consume_word_ci(sql, &mut cursor, "on").is_none() {
                    return Err(DbError::bind_error(
                        aiondb_core::SqlState::SyntaxError,
                        "syntax error in ALTER POLICY",
                    ));
                }
                let Some(table) = Self::parse_compat_misc_qualified_name(sql, &mut cursor) else {
                    return Err(DbError::bind_error(
                        aiondb_core::SqlState::SyntaxError,
                        "syntax error in ALTER POLICY",
                    ));
                };
                let bare_table = table
                    .rsplit_once('.')
                    .map(|(_, tail)| tail.to_owned())
                    .unwrap_or_else(|| table.clone());
                let composite = format!(
                    "{}@@{}",
                    name.to_ascii_lowercase(),
                    table.to_ascii_lowercase()
                );
                let key = (create_tag.clone(), composite);
                let exists = self.with_session(session, |record| {
                    Ok(record.compat_misc_objects.contains_key(&key))
                })?;
                if !exists {
                    return Err(DbError::bind_error(
                        aiondb_core::SqlState::UndefinedObject,
                        format!("policy \"{name}\" for table \"{bare_table}\" does not exist"),
                    ));
                }
                self.validate_compat_policy_table_owner(session, &table, "table")?;
                let mut rename_probe = cursor;
                if consume_word_ci(sql, &mut rename_probe, "rename").is_some()
                    && consume_word_ci(sql, &mut rename_probe, "to").is_some()
                {
                    let Some(new_name) = parse_identifier_part(sql, &mut rename_probe) else {
                        return Err(DbError::bind_error(
                            aiondb_core::SqlState::SyntaxError,
                            "syntax error in ALTER POLICY",
                        ));
                    };
                    if new_name.eq_ignore_ascii_case(&name) {
                        return Err(DbError::bind_error(
                            aiondb_core::SqlState::DuplicateObject,
                            format!("policy \"{name}\" for table \"{bare_table}\" already exists"),
                        ));
                    }
                    if !compat_tail_is_empty_or_semicolon(sql, &mut rename_probe) {
                        return Err(DbError::bind_error(
                            aiondb_core::SqlState::SyntaxError,
                            "syntax error in ALTER POLICY",
                        ));
                    }
                    let new_key = (
                        create_tag.clone(),
                        format!(
                            "{}@@{}",
                            new_name.to_ascii_lowercase(),
                            table.to_ascii_lowercase()
                        ),
                    );
                    return self.with_session_mut(session, |record| {
                        if record.compat_misc_objects.contains_key(&new_key) {
                            return Err(DbError::bind_error(
                                aiondb_core::SqlState::DuplicateObject,
                                format!(
                                    "policy \"{new_name}\" for table \"{bare_table}\" already exists"
                                ),
                            ));
                        }
                        if let Some(sql_text) = record.compat_misc_objects.remove(&key) {
                            record.compat_misc_objects.insert(new_key.clone(), sql_text);
                        }
                        if let Some(attrs) = record.compat_misc_attrs.remove(&key) {
                            record.compat_misc_attrs.insert(new_key, attrs);
                        }
                        Ok(())
                    });
                }
                // PG `ALTER POLICY <name> ON <table> [TO …] [USING (…)]
                // [WITH CHECK (…)]`: at least one clause required.
                let parsed = parse_alter_policy_modifiers(sql, cursor);
                if !parsed.has_any_modifier() {
                    return Err(DbError::feature_not_supported(
                        "unsupported compatibility command: ALTER POLICY",
                    ));
                }
                self.with_session_mut(session, |record| {
                    let attrs = record.compat_misc_attrs.entry(key.clone()).or_default();
                    if let Some(roles_value) = parsed.roles {
                        upsert_option(&mut attrs.options, "to", &roles_value);
                    }
                    if let Some(using_value) = parsed.using_expr {
                        upsert_option(&mut attrs.options, "using", &using_value);
                    }
                    if let Some(check_value) = parsed.with_check_expr {
                        upsert_option(&mut attrs.options, "with_check", &check_value);
                    }
                    Ok(())
                })?;
                return Ok(());
            }
            (create_tag, name.to_ascii_lowercase())
        };

        let exists = self.with_session(session, |record| {
            Ok(record.compat_misc_objects.contains_key(&key))
        })?;
        if tag == "ALTER STATISTICS" {
            validate_supported_alter_statistics_action(sql, cursor)?;
        }
        if !exists {
            let rendered = rendered_name_for_message.unwrap_or_else(|| key.1.clone());
            if tag == "ALTER USER MAPPING" {
                // key.1 is "role@server"
                let Some((role, server)) = key.1.split_once('@') else {
                    return Err(DbError::bind_error(
                        aiondb_core::SqlState::UndefinedObject,
                        "user mapping does not exist",
                    ));
                };
                if if_exists {
                    self.with_session_mut(session, |record| {
                        record.push_notice(format!(
                            "user mapping for \"{role}\" does not exist for server \"{server}\", skipping"
                        ));
                        Ok(())
                    })?;
                    return Ok(());
                }
                return Err(DbError::bind_error(
                    aiondb_core::SqlState::UndefinedObject,
                    format!("user mapping for \"{role}\" does not exist for server \"{server}\""),
                ));
            }
            if if_exists {
                let notice_message = if tag == "ALTER FOREIGN TABLE" {
                    format!("relation \"{rendered}\" does not exist, skipping")
                } else {
                    format!("{object_kind} \"{rendered}\" does not exist, skipping")
                };
                self.with_session_mut(session, |record| {
                    record.push_notice(notice_message);
                    Ok(())
                })?;
                return Ok(());
            }
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedObject,
                format!("{object_kind} \"{rendered}\" does not exist"),
            ));
        }

        if tag == "ALTER STATISTICS" {
            let owner = self.with_session(session, |record| {
                Ok(record
                    .compat_misc_attrs
                    .get(&key)
                    .and_then(|attrs| attrs.owner.clone())
                    .unwrap_or_default())
            })?;
            let current_user = self.with_session(session, |record| {
                Ok(super::session_vars::current_user_for_record(record).to_ascii_lowercase())
            })?;
            if !owner.is_empty() && !owner.eq_ignore_ascii_case(&current_user) {
                let rendered = rendered_name_for_message.unwrap_or_else(|| key.1.clone());
                return Err(DbError::bind_error(
                    aiondb_core::SqlState::InsufficientPrivilege,
                    format!("must be owner of statistics object {rendered}"),
                ));
            }
        }

        // ALTER EXTENSION name {ADD|DROP} <object_kind> <object_name>
        // Tracks extension membership in the stored `members` option list.
        if tag == "ALTER EXTENSION" {
            let extension_probe = cursor;
            let action_kw = if consume_word_ci(sql, &mut cursor, "add").is_some() {
                Some("add")
            } else if consume_word_ci(sql, &mut cursor, "drop").is_some() {
                Some("drop")
            } else {
                None
            };
            if let Some(action) = action_kw {
                let mut kind_words: Vec<String> = Vec::new();
                while let Some(word) = parse_identifier_part(sql, &mut cursor) {
                    kind_words.push(word);
                    if kind_words.len() >= 4 {
                        break;
                    }
                    skip_sql_whitespace(sql, &mut cursor);
                    if sql[cursor..].starts_with('(') {
                        break;
                    }
                }
                if kind_words.len() >= 2 {
                    let name = kind_words.pop().unwrap_or_default();
                    let kind = kind_words.join(" ").to_ascii_lowercase();
                    let _ = parse_parenthesized_expression(sql, &mut cursor);
                    self.mutate_extension_membership(session, &key, action, &kind, &name)?;
                    return Ok(());
                }
            }
            cursor = extension_probe;
        }

        // ALTER SUBSCRIPTION name {ADD|DROP|SET} PUBLICATION p1, p2
        // ALTER SUBSCRIPTION name CONNECTION '...'
        // ALTER SUBSCRIPTION name REFRESH PUBLICATION
        // Mutate the stored `publication`/`connection` option entries.
        if tag == "ALTER SUBSCRIPTION" {
            let subscription_publication_probe = cursor;
            if consume_word_ci(sql, &mut cursor, "connection").is_some() {
                if let Some(conn) = parse_string_literal(sql, &mut cursor) {
                    self.with_session_mut(session, |record| {
                        let attrs = record.compat_misc_attrs.entry(key.clone()).or_default();
                        if let Some(entry) =
                            attrs.options.iter_mut().find(|(k, _)| k == "connection")
                        {
                            entry.1 = conn;
                        } else {
                            attrs.options.push(("connection".to_owned(), conn));
                        }
                        Ok(())
                    })?;
                    return Ok(());
                }
                cursor = subscription_publication_probe;
            }
            let refresh_probe = cursor;
            if consume_word_ci(sql, &mut cursor, "refresh").is_some()
                && consume_word_ci(sql, &mut cursor, "publication").is_some()
            {
                self.with_session_mut(session, |record| {
                    record
                        .compat_misc_attrs
                        .entry(key.clone())
                        .or_default()
                        .state = Some("refreshing".to_owned());
                    Ok(())
                })?;
                return Ok(());
            }
            cursor = refresh_probe;
            let probe = cursor;
            let action_kw = if consume_word_ci(sql, &mut cursor, "add").is_some() {
                Some("add")
            } else if consume_word_ci(sql, &mut cursor, "drop").is_some() {
                Some("drop")
            } else if consume_word_ci(sql, &mut cursor, "set").is_some() {
                Some("set")
            } else {
                None
            };
            if let Some(action) = action_kw {
                if consume_word_ci(sql, &mut cursor, "publication").is_some() {
                    let pubs = parse_identifier_list(sql, &mut cursor);
                    if !pubs.is_empty() {
                        self.mutate_publication_membership(
                            session,
                            &key,
                            "publication",
                            action,
                            &pubs,
                        )?;
                        return Ok(());
                    }
                }
                cursor = probe;
            }
        }

        // ALTER PUBLICATION {ADD|DROP|SET} {TABLE | TABLES IN SCHEMA} ...
        // Mutate the stored `tables`/`schemas` option list directly so the
        // publication's membership stays in sync with the DDL.
        if tag == "ALTER PUBLICATION" {
            let probe = cursor;
            let action_kw = if consume_word_ci(sql, &mut cursor, "add").is_some() {
                Some("add")
            } else if consume_word_ci(sql, &mut cursor, "drop").is_some() {
                Some("drop")
            } else if consume_word_ci(sql, &mut cursor, "set").is_some() {
                Some("set")
            } else {
                None
            };
            if let Some(action) = action_kw {
                if consume_word_ci(sql, &mut cursor, "table").is_some() {
                    let _ = consume_word_ci(sql, &mut cursor, "only");
                    let names = parse_identifier_list(sql, &mut cursor);
                    if !names.is_empty() {
                        self.mutate_publication_membership(
                            session, &key, "tables", action, &names,
                        )?;
                        return Ok(());
                    }
                }
                if consume_word_ci(sql, &mut cursor, "tables").is_some()
                    && consume_word_ci(sql, &mut cursor, "in").is_some()
                    && consume_word_ci(sql, &mut cursor, "schema").is_some()
                {
                    let schemas = parse_identifier_list(sql, &mut cursor);
                    if !schemas.is_empty() {
                        self.mutate_publication_membership(
                            session, &key, "schemas", action, &schemas,
                        )?;
                        return Ok(());
                    }
                }
                cursor = probe;
            }
        }

        if tag == "ALTER FOREIGN DATA WRAPPER" {
            check_compat_options_clause_duplicates(statement_sql)?;
            check_compat_create_fdw_redundant_options(statement_sql)?;
            // Empty `ALTER FOREIGN DATA WRAPPER foo;` is a parse error in
            // PostgreSQL; the grammar requires at least one action keyword.
            let probe = cursor;
            skip_sql_whitespace(sql, &mut cursor);
            let tail = sql.get(cursor..).map(str::trim).unwrap_or_default();
            if tail.is_empty() || tail == ";" {
                return Err(DbError::bind_error(
                    aiondb_core::SqlState::SyntaxError,
                    "syntax error at or near \";\"",
                ));
            }
            cursor = probe;
            if !self.compat_session_is_superuser(session)? {
                let rendered = rendered_name_for_message
                    .clone()
                    .unwrap_or_else(|| key.1.clone());
                let probe2 = cursor;
                skip_sql_whitespace(sql, &mut cursor);
                let owner_to_clause = consume_word_ci(sql, &mut cursor, "owner").is_some()
                    && consume_word_ci(sql, &mut cursor, "to").is_some();
                cursor = probe2;
                if owner_to_clause {
                    return Err(DbError::bind_error(
                        aiondb_core::SqlState::InsufficientPrivilege,
                        format!(
                            "permission denied to change owner of foreign-data wrapper \"{rendered}\""
                        ),
                    )
                    .with_client_hint(
                        "The owner of a foreign-data wrapper must be a superuser.",
                    ));
                }
                return Err(DbError::bind_error(
                    aiondb_core::SqlState::InsufficientPrivilege,
                    format!("permission denied to alter foreign-data wrapper \"{rendered}\""),
                )
                .with_client_hint("Must be superuser to alter a foreign-data wrapper."));
            }
        }
        self.apply_compat_alter_action(session, &key, sql, &mut cursor, &object_kind)?;
        Ok(())
    }

    fn parse_compat_misc_qualified_name(sql: &str, cursor: &mut usize) -> Option<String> {
        let mut name = parse_identifier_part(sql, cursor)?;
        let mut dot_cursor = *cursor;
        skip_sql_whitespace(sql, &mut dot_cursor);
        if sql
            .get(dot_cursor..)
            .is_some_and(|tail| tail.starts_with('.'))
        {
            dot_cursor += 1;
            let part = parse_identifier_part(sql, &mut dot_cursor)?;
            *cursor = dot_cursor;
            name.push('.');
            name.push_str(&part);
        }
        Some(name)
    }

    /// Merge an `ALTER EXTENSION name {ADD|DROP} kind object` mutation into
    /// the stored extension membership list. Each entry is formatted as
    /// `kind:object` and lives under the `members` option key so it can be
    /// inspected via `pg_compat_object_attrs`.
    fn mutate_extension_membership(
        &self,
        session: &SessionHandle,
        key: &(String, String),
        action: &str,
        kind: &str,
        name: &str,
    ) -> DbResult<()> {
        let entry_value = format!("{kind}:{}", name.to_ascii_lowercase());
        self.with_session_mut(session, |record| {
            let attrs = record.compat_misc_attrs.entry(key.clone()).or_default();
            let current: Vec<String> = attrs
                .options
                .iter()
                .find(|(k, _)| k == "members")
                .map(|(_, v)| {
                    v.split(',')
                        .map(|s| s.trim().to_owned())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            let mut new_list = current;
            match action {
                "add" => {
                    if !new_list.contains(&entry_value) {
                        new_list.push(entry_value);
                    }
                }
                "drop" => {
                    new_list.retain(|e| e != &entry_value);
                }
                _ => {}
            }
            let joined = new_list.join(", ");
            if let Some(entry) = attrs.options.iter_mut().find(|(k, _)| k == "members") {
                entry.1 = joined;
            } else {
                attrs.options.push(("members".to_owned(), joined));
            }
            Ok(())
        })
    }

    /// Merge a publication membership mutation into the stored option list.
    /// `field` is `"tables"` or `"schemas"`. `action` is `"add"`, `"drop"`,
    /// or `"set"`. The option value is a comma-separated canonical list.
    fn mutate_publication_membership(
        &self,
        session: &SessionHandle,
        key: &(String, String),
        field: &str,
        action: &str,
        names: &[String],
    ) -> DbResult<()> {
        self.with_session_mut(session, |record| {
            let attrs = record.compat_misc_attrs.entry(key.clone()).or_default();
            let current: Vec<String> = attrs
                .options
                .iter()
                .find(|(k, _)| k == field)
                .map(|(_, v)| {
                    v.split(',')
                        .map(|s| s.trim().to_owned())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            let new_list: Vec<String> = match action {
                "set" => names.iter().map(|n| n.to_ascii_lowercase()).collect(),
                "add" => {
                    let mut out = current.clone();
                    for n in names {
                        let lc = n.to_ascii_lowercase();
                        if !out.iter().any(|e| e.eq_ignore_ascii_case(&lc)) {
                            out.push(lc);
                        }
                    }
                    out
                }
                "drop" => current
                    .iter()
                    .filter(|e| !names.iter().any(|n| n.eq_ignore_ascii_case(e.as_str())))
                    .cloned()
                    .collect(),
                _ => current,
            };
            let joined = new_list.join(", ");
            if let Some(entry) = attrs.options.iter_mut().find(|(k, _)| k == field) {
                entry.1 = joined;
            } else {
                attrs.options.push((field.to_owned(), joined));
            }
            Ok(())
        })
    }

    pub(super) fn validate_compat_alter_index_target(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
    ) -> DbResult<()> {
        let sql = trim_compat_statement(statement_sql);
        let mut cursor = 0usize;
        consume_word_ci(sql, &mut cursor, "alter").ok_or_else(|| {
            DbError::internal("ALTER INDEX compat validator invoked on non-ALTER SQL")
        })?;
        consume_word_ci(sql, &mut cursor, "index").ok_or_else(|| {
            DbError::internal("ALTER INDEX compat validator expected INDEX keyword")
        })?;
        let if_exists = consume_if_exists(sql, &mut cursor);
        let Some(name) = parse_identifier_part(sql, &mut cursor) else {
            let _ = if_exists;
            return Err(DbError::bind_error(
                aiondb_core::SqlState::SyntaxError,
                "syntax error in ALTER INDEX",
            ));
        };
        let mut parsed_rename_cursor = cursor;
        if !(consume_word_ci(sql, &mut parsed_rename_cursor, "rename").is_some()
            && consume_word_ci(sql, &mut parsed_rename_cursor, "to").is_some())
        {
            return Err(DbError::feature_not_supported(
                "unsupported compatibility command: ALTER INDEX",
            ));
        }
        let Some(new_name) = parse_identifier_part(sql, &mut parsed_rename_cursor) else {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::SyntaxError,
                "syntax error in ALTER INDEX",
            ));
        };
        if !compat_tail_is_empty_or_semicolon(sql, &mut parsed_rename_cursor) {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::SyntaxError,
                "syntax error in ALTER INDEX",
            ));
        }
        let txn_id = self.current_txn_id(session)?;
        let mut found_index: Option<(aiondb_core::IndexId, Option<String>)> = None;
        let mut matching_table_schema: Option<Option<String>> = None;
        'outer: for schema in self.catalog_reader.list_schemas(txn_id)? {
            for table in self.catalog_reader.list_tables(txn_id, schema.schema_id)? {
                if table.name.object_name().eq_ignore_ascii_case(&name) {
                    matching_table_schema = Some(table.name.schema_name().map(|s| s.to_owned()));
                }
                for idx in self.catalog_reader.list_indexes(txn_id, table.table_id)? {
                    if idx.name.object_name().eq_ignore_ascii_case(&name) {
                        found_index =
                            Some((idx.index_id, idx.name.schema_name().map(|s| s.to_owned())));
                        break 'outer;
                    }
                }
            }
        }
        if found_index.is_none() {
            if matching_table_schema.is_some() {
                return Err(DbError::bind_error(
                    aiondb_core::SqlState::WrongObjectType,
                    format!("\"{name}\" is not an index"),
                ));
            }
            if if_exists {
                self.with_session_mut(session, |record| {
                    record.push_notice(format!("index \"{name}\" does not exist, skipping"));
                    Ok(())
                })?;
                return Ok(());
            }
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedObject,
                format!("index \"{name}\" does not exist"),
            ));
        }

        // ALTER INDEX X RENAME TO Y: actually rename in catalog so the new
        // name is resolvable by subsequent DDL/DML.
        let Some((index_id, schema_name)) = found_index else {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedObject,
                format!("index \"{name}\" does not exist"),
            ));
        };
        let new_qname = match schema_name {
            Some(schema) => {
                aiondb_catalog::QualifiedName::qualified(schema.as_str(), new_name.as_str())
            }
            None => aiondb_catalog::QualifiedName::unqualified(new_name.as_str()),
        };
        self.catalog_writer.alter_index(
            txn_id,
            index_id,
            aiondb_catalog::IndexAlteration::Rename {
                new_name: new_qname,
            },
        )?;
        Ok(())
    }

    pub(super) fn validate_compat_alter_view_target(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        tag: &str,
    ) -> DbResult<()> {
        let sql = trim_compat_statement(statement_sql);
        let mut cursor = 0usize;
        consume_word_ci(sql, &mut cursor, "alter").ok_or_else(|| {
            DbError::internal("ALTER VIEW compat validator invoked on non-ALTER SQL")
        })?;
        if tag == "ALTER MATERIALIZED" {
            consume_word_ci(sql, &mut cursor, "materialized").ok_or_else(|| {
                DbError::internal(
                    "ALTER MATERIALIZED compat validator expected MATERIALIZED keyword",
                )
            })?;
        }
        let _ = consume_word_ci(sql, &mut cursor, "view");
        let if_exists = consume_if_exists(sql, &mut cursor);
        let Some(name) = parse_identifier_part(sql, &mut cursor) else {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::SyntaxError,
                "syntax error in ALTER VIEW",
            ));
        };
        let Some(alter_view_action) = parse_alter_view_check_option_action(sql, cursor)? else {
            return Err(DbError::feature_not_supported(format!(
                "unsupported compatibility command: {tag}"
            )));
        };
        let normalized = name.to_ascii_lowercase();
        let kind_key = if tag == "ALTER MATERIALIZED" {
            "CREATE MATERIALIZED VIEW".to_owned()
        } else {
            "CREATE VIEW".to_owned()
        };
        let compat_key = (kind_key, normalized.clone());

        // Materialized views live in the session compat registry; regular
        // views resolve via the catalog.  Check both sources before
        // rejecting.
        let found_in_session = self.with_session(session, |record| {
            Ok(record.compat_misc_objects.contains_key(&compat_key))
        })?;
        let found_in_catalog = if found_in_session {
            true
        } else {
            let txn_id = self.current_txn_id(session)?;
            if self
                .resolve_compat_table_name(session, txn_id, &name)?
                .is_some()
            {
                true
            } else {
                self.resolve_compat_view_name(session, txn_id, &name)?
                    .is_some()
            }
        };
        if !found_in_session && !found_in_catalog {
            let kind = if tag == "ALTER MATERIALIZED" {
                "materialized view"
            } else {
                "view"
            };
            if if_exists {
                self.with_session_mut(session, |record| {
                    record.push_notice(format!("{kind} \"{name}\" does not exist, skipping"));
                    Ok(())
                })?;
                return Ok(());
            }
            let kind = if tag == "ALTER MATERIALIZED" {
                "materialized view"
            } else {
                "view"
            };
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedTable,
                format!("{kind} \"{name}\" does not exist"),
            ));
        }

        if self.apply_compat_alter_view_check_option(
            session,
            compat_key,
            Some(alter_view_action),
        )? {
            return Ok(());
        }

        Err(DbError::feature_not_supported(format!(
            "unsupported compatibility command: {tag}"
        )))
    }

    fn apply_compat_alter_view_check_option(
        &self,
        session: &SessionHandle,
        compat_key: (String, String),
        action: Option<ViewCheckOptionAction>,
    ) -> DbResult<bool> {
        let Some(action) = action else {
            return Ok(false);
        };
        self.with_session_mut(session, |record| {
            let attrs = record.compat_misc_attrs.entry(compat_key).or_default();
            match action {
                ViewCheckOptionAction::Reset => {
                    upsert_option(&mut attrs.options, "check_option", "none");
                }
                ViewCheckOptionAction::Set(option) => {
                    upsert_option(&mut attrs.options, "check_option", option.as_str());
                }
            }
            Ok(())
        })?;
        Ok(true)
    }

    fn compat_create_misc_object_exists(
        &self,
        session: &SessionHandle,
        create_tag: &str,
        name: &str,
    ) -> DbResult<bool> {
        let key = (create_tag.to_owned(), name.to_ascii_lowercase());
        self.with_session(session, |record| {
            Ok(record.compat_misc_objects.contains_key(&key))
        })
    }

    fn compat_create_attr<'a>(
        attrs: &'a crate::session::CompatMiscObjectAttrs,
        key: &str,
    ) -> Option<&'a str> {
        attrs
            .options
            .iter()
            .find(|(candidate, _)| candidate == key)
            .map(|(_, value)| value.as_str())
    }

    /// True when the effective role of `session` is a superuser. Falls back to
    /// `true` when no roles are registered yet so single-tenant tests boot
    /// without role rows aren't blocked from running superuser-only DDL.
    #[allow(dead_code)]
    pub(super) fn compat_session_is_superuser(&self, session: &SessionHandle) -> DbResult<bool> {
        let identity = self.with_session(session, |record| {
            Ok(super::session_vars::session_identity_for_record(record))
        })?;
        if !crate::catalog_authorizer::catalog_has_any_roles(self.catalog_reader.as_ref())? {
            return Ok(true);
        }
        Ok(crate::catalog_authorizer::is_superuser(
            self.catalog_reader.as_ref(),
            &identity,
        ))
    }

    /// PostgreSQL built-in FDW validator functions that we accept without a
    /// catalog entry. `postgresql_fdw_validator` is the canonical example shipped
    /// with the postgresql_fdw template.
    fn is_builtin_fdw_validator(name: &str) -> bool {
        matches!(
            name.to_ascii_lowercase().as_str(),
            "postgresql_fdw_validator" | "file_fdw_validator"
        )
    }

    /// Validate an FDW VALIDATOR clause. PostgreSQL rejects validators that
    /// don't resolve to a function with signature (text[], oid). We accept the
    /// built-in `postgresql_fdw_validator` plus any user function whose name
    /// is registered in the catalog.
    pub(super) fn validate_compat_fdw_validator_function(
        &self,
        session: &SessionHandle,
        validator: &str,
    ) -> DbResult<()> {
        if Self::is_builtin_fdw_validator(validator) {
            return Ok(());
        }
        let txn_id = self.current_txn_id(session)?;
        if self
            .catalog_reader
            .get_function(txn_id, validator)?
            .is_none()
        {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedFunction,
                format!("function {validator}(text[], oid) does not exist"),
            ));
        }
        Ok(())
    }

    /// Validate an FDW HANDLER clause. PostgreSQL requires the handler function
    /// to return type `fdw_handler`. We approximate by checking the catalog
    /// FunctionDescriptor's `raw_return_type_name`.
    pub(super) fn validate_compat_fdw_handler_function(
        &self,
        session: &SessionHandle,
        handler: &str,
    ) -> DbResult<()> {
        let txn_id = self.current_txn_id(session)?;
        let Some(func) = self.catalog_reader.get_function(txn_id, handler)? else {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::UndefinedFunction,
                format!("function {handler}() does not exist"),
            ));
        };
        let returns_fdw_handler = func
            .raw_return_type_name
            .as_deref()
            .map(|name| name.eq_ignore_ascii_case("fdw_handler"))
            .unwrap_or(false);
        if !returns_fdw_handler {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::WrongObjectType,
                format!("function {handler} must return type fdw_handler"),
            ));
        }
        Ok(())
    }

    fn validate_compat_create_misc_references(
        &self,
        session: &SessionHandle,
        tag: &str,
        object_name: &str,
        attrs: &crate::session::CompatMiscObjectAttrs,
        statement_sql: &str,
    ) -> DbResult<()> {
        if matches!(
            tag,
            "CREATE FOREIGN DATA WRAPPER"
                | "CREATE SERVER"
                | "CREATE USER MAPPING"
                | "CREATE FOREIGN TABLE"
        ) {
            check_compat_options_clause_duplicates(statement_sql)?;
            if tag == "CREATE FOREIGN DATA WRAPPER" {
                check_compat_create_fdw_redundant_options(statement_sql)?;
                if !self.compat_session_is_superuser(session)? {
                    return Err(DbError::bind_error(
                        aiondb_core::SqlState::InsufficientPrivilege,
                        format!(
                            "permission denied to create foreign-data wrapper \"{object_name}\""
                        ),
                    )
                    .with_client_hint("Must be superuser to create a foreign-data wrapper."));
                }
                if let Some(validator) = Self::compat_create_attr(attrs, "validator") {
                    self.validate_compat_fdw_validator_function(session, validator)?;
                }
                if let Some(handler) = Self::compat_create_attr(attrs, "handler") {
                    self.validate_compat_fdw_handler_function(session, handler)?;
                }
            }
        }
        match tag {
            "CREATE POLICY" => {
                if let Some(table_name) = Self::compat_create_attr(attrs, "table") {
                    let txn_id = self.current_txn_id(session)?;
                    if self
                        .resolve_compat_table_name(session, txn_id, table_name)?
                        .is_none()
                    {
                        return Err(DbError::bind_error(
                            SqlState::UndefinedTable,
                            format!("relation \"{table_name}\" does not exist"),
                        ));
                    }
                    self.validate_compat_policy_table_owner(session, table_name, "relation")?;
                }
                if let Some(kind) = Self::compat_create_attr(attrs, "permissive") {
                    if !matches!(kind, "permissive" | "restrictive") {
                        return Err(DbError::bind_error(
                            SqlState::SyntaxError,
                            format!("unrecognized row security option \"{kind}\""),
                        )
                        .with_client_hint(
                            "Only PERMISSIVE or RESTRICTIVE policies are supported currently.",
                        ));
                    }
                }
                if let Some(cmd) = Self::compat_create_attr(attrs, "for") {
                    if !matches!(cmd, "select" | "insert" | "update" | "delete" | "all") {
                        return Err(DbError::bind_error(
                            SqlState::SyntaxError,
                            format!("unrecognized policy command \"{cmd}\""),
                        ));
                    }
                }
            }
            "CREATE PUBLICATION" => {
                if let Some(tables) = Self::compat_create_attr(attrs, "tables") {
                    let txn_id = self.current_txn_id(session)?;
                    for table_name in tables.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                        if self
                            .resolve_compat_table_name(session, txn_id, table_name)?
                            .is_none()
                        {
                            return Err(DbError::bind_error(
                                SqlState::UndefinedTable,
                                format!("relation \"{table_name}\" does not exist"),
                            ));
                        }
                    }
                }
            }
            "CREATE SUBSCRIPTION" => {
                if let Some(publications) = Self::compat_create_attr(attrs, "publication") {
                    for publication in publications
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                    {
                        if !self.compat_create_misc_object_exists(
                            session,
                            "CREATE PUBLICATION",
                            publication,
                        )? {
                            return Err(DbError::bind_error(
                                SqlState::UndefinedObject,
                                format!("publication \"{publication}\" does not exist"),
                            ));
                        }
                    }
                }
            }
            "CREATE STATISTICS" => {
                let Some(table_name) = Self::compat_create_attr(attrs, "table") else {
                    return Ok(());
                };
                let relation_name = table_name
                    .rsplit_once('.')
                    .map(|(_, tail)| tail)
                    .unwrap_or(table_name);
                let txn_id = self.current_txn_id(session)?;
                let table = self.resolve_compat_table_name(session, txn_id, table_name)?;
                if table.is_none() {
                    let view_exists = self
                        .resolve_compat_view_name(session, txn_id, table_name)?
                        .is_some();
                    if view_exists {
                        return Err(DbError::bind_error(
                            SqlState::WrongObjectType,
                            format!("cannot define statistics for relation \"{relation_name}\""),
                        )
                        .with_client_detail("This operation is not supported for views."));
                    }
                    let mut sequence_exists = false;
                    let unqualified = aiondb_catalog::QualifiedName::unqualified(table_name);
                    if self
                        .catalog_reader
                        .get_sequence(txn_id, &unqualified)?
                        .is_some()
                    {
                        sequence_exists = true;
                    } else {
                        let search_path = self.with_session(session, |record| {
                            self::session_vars::effective_search_path_schemas_for_record(
                                self.catalog_reader.as_ref(),
                                txn_id,
                                record,
                            )
                        })?;
                        for schema_name in search_path {
                            let qualified =
                                aiondb_catalog::QualifiedName::qualified(schema_name, table_name);
                            if self
                                .catalog_reader
                                .get_sequence(txn_id, &qualified)?
                                .is_some()
                            {
                                sequence_exists = true;
                                break;
                            }
                        }
                    }
                    if sequence_exists {
                        return Err(DbError::bind_error(
                            SqlState::WrongObjectType,
                            format!("cannot define statistics for relation \"{relation_name}\""),
                        )
                        .with_client_detail("This operation is not supported for sequences."));
                    }
                    let mut index_exists = false;
                    'schemas: for schema in self.catalog_reader.list_schemas(txn_id)? {
                        for listed in self.catalog_reader.list_tables(txn_id, schema.schema_id)? {
                            for index in
                                self.catalog_reader.list_indexes(txn_id, listed.table_id)?
                            {
                                if index.name.object_name().eq_ignore_ascii_case(table_name)
                                    || index.name.object_name().eq_ignore_ascii_case(relation_name)
                                {
                                    index_exists = true;
                                    break 'schemas;
                                }
                            }
                        }
                    }
                    if index_exists {
                        return Err(DbError::bind_error(
                            SqlState::WrongObjectType,
                            format!("cannot define statistics for relation \"{relation_name}\""),
                        )
                        .with_client_detail("This operation is not supported for indexes."));
                    }
                    let composite_type_exists = self.with_session(session, |record| {
                        let rel_lc = relation_name.to_ascii_lowercase();
                        Ok(record
                            .compat_user_types
                            .iter()
                            .any(|ty| ty.name.eq_ignore_ascii_case(&rel_lc))
                            || record
                                .domain_defs
                                .iter()
                                .any(|ty| ty.name.eq_ignore_ascii_case(&rel_lc)))
                    })?;
                    if composite_type_exists {
                        return Err(DbError::bind_error(
                            SqlState::WrongObjectType,
                            format!("cannot define statistics for relation \"{relation_name}\""),
                        )
                        .with_client_detail(
                            "This operation is not supported for composite types.",
                        ));
                    }
                    return Err(DbError::bind_error(
                        SqlState::UndefinedTable,
                        format!("relation \"{table_name}\" does not exist"),
                    ));
                }
                let Some(table) = table else {
                    return Err(DbError::internal(
                        "statistics target table disappeared after existence check",
                    ));
                };
                let key_items = Self::compat_create_attr(attrs, "columns")
                    .map(split_statistics_key_items)
                    .unwrap_or_default();
                let single_expression_stat = key_items.len() == 1
                    && simple_stats_column_key(&key_items[0]).is_none()
                    && normalize_stats_expr_key(&key_items[0]).is_some();
                if key_items.len() < 2 && !single_expression_stat {
                    return Err(DbError::bind_error(
                        SqlState::InvalidTableDefinition,
                        "extended statistics require at least 2 columns",
                    ));
                }
                if key_items.len() > 8 {
                    return Err(DbError::bind_error(
                        SqlState::InvalidTableDefinition,
                        "cannot have more than 8 columns in statistics",
                    ));
                }
                let mut seen_columns = std::collections::HashSet::new();
                let mut seen_exprs = std::collections::HashSet::new();
                for item in &key_items {
                    if let Some(column_key) = simple_stats_column_key(item) {
                        if !seen_columns.insert(column_key.clone()) {
                            return Err(DbError::bind_error(
                                SqlState::DuplicateColumn,
                                "duplicate column name in statistics definition",
                            ));
                        }
                        let exists = table
                            .columns
                            .iter()
                            .any(|column| column.name.eq_ignore_ascii_case(&column_key));
                        if !exists {
                            return Err(DbError::bind_error(
                                SqlState::UndefinedColumn,
                                format!("column \"{column_key}\" does not exist"),
                            ));
                        }
                    } else if let Some(expr_key) = normalize_stats_expr_key(item) {
                        if !seen_exprs.insert(expr_key) {
                            return Err(DbError::bind_error(
                                SqlState::DuplicateColumn,
                                "duplicate expression in statistics definition",
                            ));
                        }
                    }
                }
            }
            "CREATE SERVER" => {
                if let Some(fdw) = Self::compat_create_attr(attrs, "fdw") {
                    if !self.compat_create_misc_object_exists(
                        session,
                        "CREATE FOREIGN DATA WRAPPER",
                        fdw,
                    )? {
                        return Err(DbError::bind_error(
                            SqlState::UndefinedObject,
                            format!("foreign-data wrapper \"{fdw}\" does not exist"),
                        ));
                    }
                    if !self.compat_session_is_superuser(session)?
                        && !self.compat_role_has_usage_on_compat_fdw(
                            session,
                            &self.compat_current_user_lower(session)?,
                            fdw,
                        )?
                    {
                        return Err(DbError::bind_error(
                            SqlState::InsufficientPrivilege,
                            format!("permission denied for foreign-data wrapper {fdw}"),
                        ));
                    }
                    self.validate_postgresql_fdw_server_options(fdw, &attrs.options)?;
                }
            }
            "CREATE USER MAPPING" => {
                if let Some((role, server)) = object_name.rsplit_once('@') {
                    let role_lc = role.to_ascii_lowercase();
                    let pseudo_role = matches!(
                        role_lc.as_str(),
                        "current_user" | "user" | "session_user" | "public"
                    );
                    if !pseudo_role {
                        let txn_id = self.current_txn_id(session)?;
                        if self.catalog_reader.get_role(txn_id, role)?.is_none() {
                            return Err(DbError::bind_error(
                                SqlState::UndefinedObject,
                                format!("role \"{role}\" does not exist"),
                            ));
                        }
                    }
                    if !self.compat_create_misc_object_exists(session, "CREATE SERVER", server)? {
                        return Err(DbError::bind_error(
                            SqlState::UndefinedObject,
                            format!("server \"{server}\" does not exist"),
                        ));
                    }
                    self.validate_postgresql_fdw_user_mapping_options(
                        session,
                        server,
                        &attrs.options,
                    )?;
                }
            }
            "CREATE FOREIGN TABLE" => {
                if let Some(server) = Self::compat_create_attr(attrs, "server") {
                    if !self.compat_create_misc_object_exists(session, "CREATE SERVER", server)? {
                        return Err(DbError::bind_error(
                            SqlState::UndefinedObject,
                            format!("server \"{server}\" does not exist"),
                        ));
                    }
                }
                check_compat_create_foreign_table_body(statement_sql)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn is_postgresql_fdw_name(name: &str) -> bool {
        name.eq_ignore_ascii_case("postgresql")
    }

    fn validate_postgresql_fdw_server_options(
        &self,
        fdw_name: &str,
        options: &[(String, String)],
    ) -> DbResult<()> {
        if !Self::is_postgresql_fdw_name(fdw_name) {
            return Ok(());
        }
        for (name, _) in options {
            if matches!(name.as_str(), "fdw" | "type" | "version") {
                continue;
            }
            let valid = matches!(
                name.as_str(),
                "host"
                    | "hostaddr"
                    | "port"
                    | "dbname"
                    | "connect_timeout"
                    | "application_name"
                    | "keepalives"
                    | "keepalives_idle"
                    | "keepalives_interval"
                    | "keepalives_count"
                    | "tcp_user_timeout"
                    | "sslmode"
                    | "sslcompression"
                    | "sslcert"
                    | "sslkey"
                    | "sslrootcert"
                    | "sslcrl"
                    | "sslsni"
                    | "requirepeer"
                    | "krbsrvname"
                    | "gsslib"
                    | "use_remote_estimate"
                    | "fdw_startup_cost"
                    | "fdw_tuple_cost"
                    | "extensions"
                    | "updatable"
                    | "fetch_size"
                    | "batch_size"
                    | "async_capable"
                    | "parallel_commit"
                    | "parallel_abort"
            );
            if !valid {
                return Err(DbError::bind_error(
                    SqlState::InvalidParameterValue,
                    format!("invalid option \"{name}\""),
                ));
            }
        }
        Ok(())
    }

    fn validate_postgresql_fdw_user_mapping_options(
        &self,
        session: &SessionHandle,
        server_name: &str,
        options: &[(String, String)],
    ) -> DbResult<()> {
        let server_key = ("CREATE SERVER".to_owned(), server_name.to_ascii_lowercase());
        let server_fdw = self.with_session(session, |record| {
            Ok(record
                .compat_misc_attrs
                .get(&server_key)
                .and_then(|attrs| attrs.options.iter().find(|(k, _)| k == "fdw"))
                .map(|(_, v)| v.clone()))
        })?;
        let Some(server_fdw) = server_fdw else {
            return Ok(());
        };
        if !Self::is_postgresql_fdw_name(&server_fdw) {
            return Ok(());
        }
        for (name, _) in options {
            if matches!(name.as_str(), "server" | "user_name") {
                continue;
            }
            let valid = matches!(
                name.as_str(),
                "user"
                    | "password"
                    | "sslpassword"
                    | "password_required"
                    | "clientcert"
                    | "gssdelegation"
                    | "use_scram_passthrough"
            );
            if !valid {
                let err = DbError::bind_error(
                    SqlState::InvalidParameterValue,
                    format!("invalid option \"{name}\""),
                );
                if name.eq_ignore_ascii_case("username") {
                    return Err(err.with_client_hint("Perhaps you meant the option \"user\"."));
                }
                return Err(err);
            }
        }
        Ok(())
    }

    pub(super) fn validate_compat_alter_server_owner_target_role(
        &self,
        session: &SessionHandle,
        server_name: &str,
        new_owner: &str,
    ) -> DbResult<()> {
        let txn_id = self.current_txn_id(session)?;
        if self.catalog_reader.get_role(txn_id, new_owner)?.is_none() {
            return Err(DbError::bind_error(
                SqlState::UndefinedObject,
                format!("role \"{new_owner}\" does not exist"),
            ));
        }
        if !self.compat_session_is_superuser(session)?
            && !self.compat_current_user_has_role(session, &new_owner.to_ascii_lowercase())?
        {
            return Err(DbError::bind_error(
                SqlState::InsufficientPrivilege,
                format!("must be able to SET ROLE \"{new_owner}\""),
            ));
        }
        let server_key = ("CREATE SERVER".to_owned(), server_name.to_ascii_lowercase());
        let fdw = self.with_session(session, |record| {
            Ok(record
                .compat_misc_attrs
                .get(&server_key)
                .and_then(|attrs| attrs.options.iter().find(|(k, _)| k == "fdw"))
                .map(|(_, v)| v.clone()))
        })?;
        if let Some(fdw_name) = fdw {
            if !self.compat_session_is_superuser(session)?
                && !self.compat_role_has_usage_on_compat_fdw(session, new_owner, &fdw_name)?
            {
                return Err(DbError::bind_error(
                    SqlState::InsufficientPrivilege,
                    format!("permission denied for foreign-data wrapper {fdw_name}"),
                ));
            }
        }
        Ok(())
    }

    pub(super) fn validate_compat_alter_options_for_key(
        &self,
        session: &SessionHandle,
        key: &(String, String),
        pairs: &[(String, String, String)],
    ) -> DbResult<()> {
        if key.0 == "CREATE SERVER" {
            let fdw = self.with_session(session, |record| {
                Ok(record
                    .compat_misc_attrs
                    .get(key)
                    .and_then(|attrs| attrs.options.iter().find(|(k, _)| k == "fdw"))
                    .map(|(_, v)| v.clone()))
            })?;
            if let Some(fdw_name) = fdw {
                let options: Vec<(String, String)> = pairs
                    .iter()
                    .map(|(_, n, v)| (n.to_ascii_lowercase(), v.clone()))
                    .collect();
                self.validate_postgresql_fdw_server_options(&fdw_name, &options)?;
            }
        } else if key.0 == "CREATE USER MAPPING" {
            if let Some((_, server)) = key.1.split_once('@') {
                let options: Vec<(String, String)> = pairs
                    .iter()
                    .map(|(_, n, v)| (n.to_ascii_lowercase(), v.clone()))
                    .collect();
                self.validate_postgresql_fdw_user_mapping_options(session, server, &options)?;
            }
        } else if key.0 == "CREATE FOREIGN DATA WRAPPER" {
            for (_, name, _) in pairs {
                if !name.eq_ignore_ascii_case("handler")
                    && !name.eq_ignore_ascii_case("validator")
                    && !name.eq_ignore_ascii_case("testing")
                    && !name.eq_ignore_ascii_case("another")
                {
                    return Err(DbError::bind_error(
                        SqlState::InvalidParameterValue,
                        format!("invalid option \"{}\"", name.to_ascii_lowercase()),
                    )
                    .with_client_hint("There are no valid options in this context."));
                }
            }
        }
        Ok(())
    }

    pub(super) fn compat_role_has_usage_on_compat_fdw(
        &self,
        session: &SessionHandle,
        role_name: &str,
        fdw_name: &str,
    ) -> DbResult<bool> {
        let normalized_role = role_name.to_ascii_lowercase();
        let txn_id = self.current_txn_id(session)?;
        let privileges = self.catalog_reader.get_privileges(txn_id, role_name)?;
        let has_direct = privileges.iter().any(|privilege| {
            matches!(
                privilege.privilege,
                CatalogPrivilege::Usage | CatalogPrivilege::All
            ) && matches!(
                &privilege.target,
                PrivilegeTarget::Schema(schema_name)
                    if schema_name.eq_ignore_ascii_case(fdw_name)
            )
        });
        if has_direct {
            return Ok(true);
        }
        let owner_key = (
            "CREATE FOREIGN DATA WRAPPER".to_owned(),
            fdw_name.to_ascii_lowercase(),
        );
        let owner = self.compat_misc_owner(session, &owner_key)?;
        if owner.is_empty() {
            return Ok(false);
        }
        Ok(owner.eq_ignore_ascii_case(&normalized_role))
    }

    fn record_compat_misc_create(
        &self,
        session: &SessionHandle,
        tag: &str,
        statement_sql: &str,
    ) -> DbResult<()> {
        let Some(parsed_name) = parse_compat_create_object_name(tag, statement_sql) else {
            return Ok(());
        };
        let name = if tag == "CREATE USER MAPPING" {
            self.resolve_compat_user_mapping_name(session, &parsed_name)?
        } else {
            parsed_name
        };
        let key = (tag.to_owned(), name.to_ascii_lowercase());
        let sql_owned = statement_sql.to_owned();
        let mut initial_attrs = extract_compat_create_attrs(tag, statement_sql);
        if initial_attrs.owner.is_none() {
            let owner = self.with_session(session, |record| {
                Ok(super::session_vars::current_user_for_record(record))
            })?;
            initial_attrs.owner = Some(owner);
        }
        self.validate_compat_create_misc_references(
            session,
            tag,
            &name,
            &initial_attrs,
            statement_sql,
        )?;
        if tag == "CREATE STATISTICS" {
            if let Some(table_name) = Self::compat_create_attr(&initial_attrs, "table") {
                let txn_id = self.current_txn_id(session)?;
                if let Some(table) = self.resolve_compat_table_name(session, txn_id, table_name)? {
                    upsert_option(
                        &mut initial_attrs.options,
                        "table_id",
                        &table.table_id.get().to_string(),
                    );
                }
            }
        }
        let global_key = key.clone();
        let global_sql = sql_owned.clone();
        let global_attrs = initial_attrs.clone();

        let if_not_exists = sql_has_if_not_exists(statement_sql);
        let notice = self.with_session_mut(session, |record| {
            if record.compat_misc_objects.contains_key(&key) {
                let object_kind = pg_object_kind_label_from_tag(tag, "CREATE ");
                let already_msg = if tag == "CREATE USER MAPPING" {
                    let (role, server) = name.split_once('@').unwrap_or((name.as_str(), ""));
                    format!("user mapping for \"{role}\" already exists for server \"{server}\"")
                } else if tag == "CREATE POLICY" {
                    let (policy, table) = name.split_once("@@").unwrap_or((name.as_str(), ""));
                    let bare_table = table
                        .rsplit_once('.')
                        .map(|(_, tail)| tail)
                        .unwrap_or(table);
                    format!("policy \"{policy}\" for table \"{bare_table}\" already exists")
                } else {
                    format!("{object_kind} \"{name}\" already exists")
                };
                if if_not_exists {
                    return Ok(Some(format!("{already_msg}, skipping")));
                }
                return Err(DbError::bind_error(SqlState::DuplicateObject, already_msg));
            }
            record.compat_misc_objects.insert(key.clone(), sql_owned);
            record.compat_misc_attrs.insert(key, initial_attrs);
            Ok(None)
        })?;
        if let Some(msg) = notice {
            let _ = self.with_session_mut(session, |record| {
                record.push_notice(msg);
                Ok(())
            });
        }
        if tag == "CREATE TEXT SEARCH" {
            if let Ok(mut shared_objects) = self.compat_misc_global_objects.lock() {
                shared_objects.insert(global_key.clone(), global_sql);
            }
            if let Ok(mut shared_attrs) = self.compat_misc_global_attrs.lock() {
                shared_attrs.insert(global_key, global_attrs);
            }
        }
        Ok(())
    }

    /// Resolve a USER MAPPING `<role>@<server>` key, replacing the role token
    /// when it is `current_user`, `user`, or `session_user`. PostgreSQL stores
    /// the resolved role name in the catalog, so a later mapping for the same
    /// concrete role surfaces as a duplicate.
    pub(super) fn resolve_compat_user_mapping_name(
        &self,
        session: &SessionHandle,
        name: &str,
    ) -> DbResult<String> {
        let Some((role, server)) = name.split_once('@') else {
            return Ok(name.to_owned());
        };
        let resolved_role = self.with_session(session, |record| {
            let lower = role.to_ascii_lowercase();
            Ok(match lower.as_str() {
                "current_user" | "user" => {
                    super::session_vars::current_user_for_record(record).to_ascii_lowercase()
                }
                "session_user" => {
                    super::session_vars::session_user_for_record(record).to_ascii_lowercase()
                }
                _ => lower,
            })
        })?;
        Ok(format!("{resolved_role}@{}", server.to_ascii_lowercase()))
    }

    fn cleanup_compat_misc_drop(
        &self,
        session: &SessionHandle,
        tag: &str,
        statement_sql: &str,
    ) -> DbResult<()> {
        let create_tag = tag.replacen("DROP", "CREATE", 1);
        if let Some(parsed_name) = parse_compat_drop_object_name(tag, statement_sql) {
            let name = if tag == "DROP USER MAPPING" {
                self.resolve_compat_user_mapping_name(session, &parsed_name)?
            } else {
                parsed_name
            };
            let key = (create_tag, name.to_ascii_lowercase());
            self.with_session_mut(session, |record| {
                record.compat_misc_objects.remove(&key);
                record.compat_misc_attrs.remove(&key);
                Ok(())
            })?;
            if tag == "DROP TEXT SEARCH" {
                if let Ok(mut shared_objects) = self.compat_misc_global_objects.lock() {
                    shared_objects.remove(&key);
                }
                if let Ok(mut shared_attrs) = self.compat_misc_global_attrs.lock() {
                    shared_attrs.remove(&key);
                }
            }
        }
        Ok(())
    }

    fn cleanup_drop_table_compat_policies(
        &self,
        session: &SessionHandle,
        drop_table: &aiondb_parser::ast::DropTableStatement,
    ) -> DbResult<()> {
        let mut table_names = Vec::with_capacity(1 + drop_table.extra_names.len());
        table_names.push(drop_table.name.parts.join(".").to_ascii_lowercase());
        table_names.extend(
            drop_table
                .extra_names
                .iter()
                .map(|name| name.parts.join(".").to_ascii_lowercase()),
        );
        self.with_session_mut(session, |record| {
            let policy_keys: Vec<_> = record
                .compat_misc_objects
                .keys()
                .filter(|(tag, name)| {
                    tag == "CREATE POLICY"
                        && name.split_once("@@").is_some_and(|(_, table)| {
                            table_names.iter().any(|dropped| {
                                table == dropped
                                    || table
                                        .rsplit_once('.')
                                        .is_some_and(|(_, bare)| bare == dropped)
                                    || dropped
                                        .rsplit_once('.')
                                        .is_some_and(|(_, bare)| table == bare)
                            })
                        })
                })
                .cloned()
                .collect();
            for key in policy_keys {
                record.compat_misc_objects.remove(&key);
                record.compat_misc_attrs.remove(&key);
            }
            Ok(())
        })
    }

    /// Mirror CREATE/ALTER/DROP POLICY into the durable catalog so RLS
    /// policies survive engine restart. The session-level
    /// `compat_misc_objects` / `compat_misc_attrs` registry has already
    /// been mutated by `record_compat_misc_create` /
    /// `validate_compat_alter_misc_object` / `validate_compat_drop_misc_object`
    /// when this is invoked, so we read the post-mutation state and
    /// project it onto the catalog API. Conflicts (re-issue with same
    /// name) are coerced into ALTER so the durable row is overwritten
    /// in lock-step with the cache.
    pub(in crate::engine) fn persist_compat_policy_ddl(
        &self,
        session: &SessionHandle,
        tag: &str,
        statement_sql: &str,
    ) -> DbResult<()> {
        let txn_id = self.current_txn_id(session)?;
        match tag {
            "CREATE POLICY" | "ALTER POLICY" => {
                // Reissue the canonical name parser so the lookup matches
                // exactly the key the misc-object handler stored under.
                let canonical_name = match parse_compat_create_object_name(tag, statement_sql) {
                    Some(name) => name,
                    None => return Ok(()),
                };
                let registry_key = ("CREATE POLICY".to_owned(), canonical_name);
                let entry = self.with_session(session, |record| {
                    Ok(record
                        .compat_misc_attrs
                        .get(&registry_key)
                        .cloned()
                        .map(|attrs| (registry_key.1.clone(), attrs)))
                })?;
                let Some((canonical, attrs)) = entry else {
                    return Ok(());
                };
                let Some(descriptor) = compat_misc_attrs_to_policy_descriptor(&canonical, &attrs)
                else {
                    return Ok(());
                };
                if tag == "CREATE POLICY" {
                    match self
                        .catalog_writer
                        .create_policy(txn_id, descriptor.clone())
                    {
                        Ok(()) => {}
                        Err(error)
                            if error.sqlstate() == aiondb_core::SqlState::UniqueViolation =>
                        {
                            self.catalog_writer.alter_policy(txn_id, descriptor)?;
                        }
                        Err(error) => return Err(error),
                    }
                } else {
                    self.catalog_writer.alter_policy(txn_id, descriptor)?;
                }
            }
            "DROP POLICY" => {
                if let Some(canonical) = parse_compat_drop_object_name(tag, statement_sql) {
                    if let Some((policy_name, table_name)) =
                        split_compat_policy_canonical(&canonical)
                    {
                        if let Err(error) =
                            self.catalog_writer
                                .drop_policy(txn_id, &policy_name, &table_name)
                        {
                            if error.sqlstate() != aiondb_core::SqlState::UndefinedObject {
                                return Err(error);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub(in crate::engine) fn apply_tagged_post_statement_compat_effects(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<()> {
        if matches!(statement, Statement::DropFunction(_)) {
            self.cleanup_drop_function_compat_casts(session, statement_sql, statement)?;
            return Ok(());
        }
        if let Statement::DropTable(drop_table) = statement {
            self.cleanup_drop_table_compat_policies(session, drop_table)?;
            return Ok(());
        }
        if matches!(statement, Statement::CreateTable(_)) {
            self.emit_create_table_inherits_notices(session, statement)?;
        }

        let Some(tag) = statement_compat_tag(statement) else {
            return Ok(());
        };
        match tag {
            "CREATE VIEW" => {
                if let Statement::CreateView(create_view) = statement {
                    let view_name = create_view
                        .name
                        .parts
                        .last()
                        .map(|part| part.to_ascii_lowercase())
                        .unwrap_or_default();
                    if !view_name.is_empty() {
                        self.with_session_mut(session, |record| {
                            let key = ("CREATE VIEW".to_owned(), view_name.clone());
                            let attrs = record.compat_misc_attrs.entry(key).or_default();
                            if let Some(option) = parse_create_view_check_option(statement_sql) {
                                upsert_option(&mut attrs.options, "check_option", option.as_str());
                            } else {
                                attrs.options.retain(|(name, _)| name != "check_option");
                            }
                            Ok(())
                        })?;
                    }
                }
            }
            // CREATE/DROP PROCEDURE was a session-only counter; there
            // is no SQL CALL execution path in AionDB, so accepting the
            // DDL would lie to clients that expect to invoke the body.
            // The `_ => {}` catch-all below is intentionally not reached:
            // these tags are guarded earlier in the dispatch tree by the
            // typed-family handler that emits feature_not_supported.
            "CREATE AGGREGATE" => {
                if let Some((aggregate_name, rewrite)) =
                    classify_compat_aggregate_rewrite(statement_sql)
                {
                    self.with_session_mut(session, |record| {
                        record
                            .compat_aggregate_rewrites
                            .insert(aggregate_name, rewrite);
                        Ok(())
                    })?;
                }
            }
            "DROP AGGREGATE" => {
                if let Some(aggregate_name) = parse_drop_aggregate_name(statement_sql) {
                    self.with_session_mut(session, |record| {
                        record.compat_aggregate_rewrites.remove(&aggregate_name);
                        Ok(())
                    })?;
                }
            }
            "CREATE CAST" => {
                if let Some(parsed) = parse_create_cast_statement(statement_sql) {
                    let txn_id = self.current_txn_id(session)?;
                    let owner = self.with_session(session, |record| {
                        Ok(super::session_vars::current_user_for_record(record))
                    })?;
                    let registered = self.with_session_mut(session, |record| {
                        let _ = ensure_compat_user_type(record, &parsed.source_type);
                        let _ = ensure_compat_user_type(record, &parsed.target_type);
                        Arc::make_mut(&mut record.compat_user_casts).retain(|entry| {
                            !(entry.source_type == parsed.source_type
                                && entry.target_type == parsed.target_type)
                        });
                        let method = match parsed.method.clone() {
                            ParsedCompatCastMethod::Binary => CompatCastMethod::Binary,
                            ParsedCompatCastMethod::InOut => CompatCastMethod::InOut,
                            ParsedCompatCastMethod::Function(function_name) => {
                                let function_oid = record.next_compat_function_oid;
                                record.next_compat_function_oid =
                                    record.next_compat_function_oid.saturating_add(1);
                                CompatCastMethod::Function {
                                    function_name,
                                    function_oid,
                                }
                            }
                        };
                        let cast_oid = record.next_compat_cast_oid;
                        let cast = CompatUserCast {
                            oid: cast_oid,
                            source_type: parsed.source_type.clone(),
                            target_type: parsed.target_type.clone(),
                            context: parsed.context,
                            method,
                        };
                        Arc::make_mut(&mut record.compat_user_casts).push(cast.clone());
                        record.next_compat_cast_oid = record.next_compat_cast_oid.saturating_add(1);
                        Ok(cast)
                    })?;
                    // Persist the cast to the durable catalog so it
                    // survives engine restart. UniqueViolation is
                    // tolerated (re-issue / replace): overwrite by
                    // dropping the prior catalog row first.
                    let descriptor = compat_user_cast_to_descriptor(&registered, Some(owner));
                    if let Err(error) = self.catalog_writer.create_cast(txn_id, descriptor.clone())
                    {
                        if error.sqlstate() == aiondb_core::SqlState::UniqueViolation {
                            self.catalog_writer.drop_cast(
                                txn_id,
                                &registered.source_type,
                                &registered.target_type,
                            )?;
                            self.catalog_writer.create_cast(txn_id, descriptor)?;
                        } else {
                            return Err(error);
                        }
                    }
                }
            }
            "DROP CAST" => {
                if let Some(parsed) = parse_drop_cast_statement(statement_sql) {
                    let txn_id = self.current_txn_id(session)?;
                    self.with_session_mut(session, |record| {
                        Arc::make_mut(&mut record.compat_user_casts).retain(|entry| {
                            !(entry.source_type == parsed.source_type
                                && entry.target_type == parsed.target_type)
                        });
                        Ok(())
                    })?;
                    if let Err(error) = self.catalog_writer.drop_cast(
                        txn_id,
                        &parsed.source_type,
                        &parsed.target_type,
                    ) {
                        if error.sqlstate() != aiondb_core::SqlState::UndefinedObject {
                            return Err(error);
                        }
                    }
                }
            }
            "CREATE POLICY" => {
                // POLICY has real RLS qual injection via the planner +
                // catalog persistence, so the misc-attrs path is the
                // genuine implementation, not a stub.
                self.record_compat_misc_create(session, tag, statement_sql)?;
                self.persist_compat_policy_ddl(session, tag, statement_sql)?;
            }
            "CREATE STATISTICS" => {
                // STATISTICS rows are consumed by the optimizer's
                // `pg_statistic_ext` views and by extended-statistics
                // OID lookups (`register_pg_statistics_objdef`). Keep
                // the misc-attrs registration so those queries see the
                // shape they expect.
                self.record_compat_misc_create(session, tag, statement_sql)?;
            }
            // AionDB has no logical-replication, FDW, full-text search,
            // procedural-language, or character-set conversion stack
            // tooling that issues the DDL expecting real semantics. PG
            // returns `feature_not_supported` rather than half-storing
            // metadata that the engine then can't honour.
            //
            // Reference checks run first so a `CREATE PUBLICATION FOR
            // TABLE missing` surfaces `undefined_table` (matching PG's
            // ordering: input validation precedes the
            // feature-not-supported terminal error).
            "CREATE CONVERSION"
            | "CREATE TRANSFORM"
            | "CREATE LANGUAGE"
            | "CREATE PUBLICATION"
            | "CREATE SUBSCRIPTION"
            | "CREATE SERVER"
            | "CREATE USER MAPPING"
            | "CREATE FOREIGN TABLE"
            | "CREATE FOREIGN DATA WRAPPER" => {
                if let Some(parsed_name) = parse_compat_create_object_name(tag, statement_sql) {
                    let resolved_name = if tag == "CREATE USER MAPPING" {
                        self.resolve_compat_user_mapping_name(session, &parsed_name)?
                    } else {
                        parsed_name
                    };
                    let attrs = extract_compat_create_attrs(tag, statement_sql);
                    self.validate_compat_create_misc_references(
                        session,
                        tag,
                        &resolved_name,
                        &attrs,
                        statement_sql,
                    )?;
                }
                return Err(DbError::feature_not_supported(format!(
                    "unsupported compatibility command: {tag}"
                )));
            }
            "ALTER POLICY" => {
                self.validate_compat_alter_misc_object(session, tag, statement_sql)?;
                self.persist_compat_policy_ddl(session, tag, statement_sql)?;
            }
            "DROP POLICY" => {
                let _ = self.validate_compat_drop_misc_object(session, tag, statement_sql)?;
                self.persist_compat_policy_ddl(session, tag, statement_sql)?;
            }
            "DROP EVENT TRIGGER" | "DROP ACCESS METHOD" | "DROP CONVERSION" | "DROP TRANSFORM"
            | "DROP TEXT SEARCH" | "DROP LANGUAGE" => {
                self.cleanup_compat_misc_drop(session, tag, statement_sql)?;
            }
            _ => {}
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub(in crate::engine) fn record_compat_grant_dependencies(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<()> {
        let Statement::Grant(grant) = statement else {
            return Ok(());
        };
        if matches!(grant.target, aiondb_parser::GrantTarget::Role(_)) {
            if role_membership_has_admin_option(statement_sql) {
                if let aiondb_parser::GrantTarget::Role(granted_role) = &grant.target {
                    let txn_id = self.current_txn_id(session)?;
                    self.catalog_writer.grant_privilege(
                        txn_id,
                        PrivilegeDescriptor {
                            role_name: grant.role_name.clone(),
                            privilege: CatalogPrivilege::All,
                            target: PrivilegeTarget::Role(granted_role.clone()),
                        },
                    )?;
                }
            }
            if let Some(dependency) = parse_role_membership_granted_by_dependency(statement_sql) {
                self.with_compat_role_membership_dependency_registry_mut(|registry| {
                    if !registry.dependencies.iter().any(|entry| {
                        entry.grantor.eq_ignore_ascii_case(&dependency.grantor)
                            && entry.grantee.eq_ignore_ascii_case(&dependency.grantee)
                            && entry
                                .granted_role
                                .eq_ignore_ascii_case(&dependency.granted_role)
                    }) {
                        registry.dependencies.push(dependency);
                    }
                    Ok(())
                })?;
            }
        }

        let current_user = self.with_session(session, |record| {
            Ok(super::session_vars::current_user_for_record(record).clone())
        })?;
        let requested_privileges: Vec<CatalogPrivilege> = grant
            .privileges
            .iter()
            .copied()
            .map(parser_privilege_to_catalog)
            .collect();
        let txn_id = self.current_txn_id(session)?;
        let grantee_privileges = self
            .catalog_reader
            .get_privileges(txn_id, &grant.role_name)?;

        self.with_compat_granted_privilege_dependency_registry_mut(|registry| {
            for privilege in grantee_privileges {
                if !parser_grant_target_matches_privilege_target(
                    &grant.target,
                    &privilege.target,
                    None,
                ) {
                    continue;
                }
                if !requested_privileges.contains(&CatalogPrivilege::All)
                    && !requested_privileges.contains(&privilege.privilege)
                    && privilege.privilege != CatalogPrivilege::All
                {
                    continue;
                }
                if !registry.dependencies.iter().any(|entry| {
                    entry.grantor.eq_ignore_ascii_case(&current_user)
                        && entry.privilege == privilege
                }) {
                    registry
                        .dependencies
                        .push(CompatGrantedPrivilegeDependency {
                            grantor: current_user.clone(),
                            privilege,
                        });
                }
            }
            Ok(())
        })
    }

    #[allow(dead_code)]
    pub(in crate::engine) fn cleanup_compat_revoke_dependencies(
        &self,
        statement: &Statement,
    ) -> DbResult<()> {
        let Statement::Revoke(revoke) = statement else {
            return Ok(());
        };
        if let aiondb_parser::GrantTarget::Role(granted_role) = &revoke.target {
            let grantee = revoke.role_name.clone();
            self.with_compat_role_membership_dependency_registry_mut(|registry| {
                registry.dependencies.retain(|dependency| {
                    !(dependency.grantee.eq_ignore_ascii_case(&grantee)
                        && dependency.granted_role.eq_ignore_ascii_case(granted_role))
                });
                Ok(())
            })?;
        }

        let requested_privileges: Vec<CatalogPrivilege> = revoke
            .privileges
            .iter()
            .copied()
            .map(parser_privilege_to_catalog)
            .collect();
        self.with_compat_granted_privilege_dependency_registry_mut(|registry| {
            registry.dependencies.retain(|dependency| {
                if !dependency
                    .privilege
                    .role_name
                    .eq_ignore_ascii_case(&revoke.role_name)
                {
                    return true;
                }
                if !parser_grant_target_matches_privilege_target(
                    &revoke.target,
                    &dependency.privilege.target,
                    None,
                ) {
                    return true;
                }
                if requested_privileges.contains(&CatalogPrivilege::All)
                    || dependency.privilege.privilege == CatalogPrivilege::All
                    || requested_privileges.contains(&dependency.privilege.privilege)
                {
                    return false;
                }
                true
            });
            Ok(())
        })
    }

    #[allow(dead_code)]
    pub(in crate::engine) fn cleanup_compat_drop_role_dependencies(
        &self,
        statement: &Statement,
    ) -> DbResult<()> {
        let Statement::DropRole(drop_role) = statement else {
            return Ok(());
        };
        self.with_compat_role_membership_dependency_registry_mut(|registry| {
            registry.dependencies.retain(|dependency| {
                !dependency.grantor.eq_ignore_ascii_case(&drop_role.name)
                    && !dependency.grantee.eq_ignore_ascii_case(&drop_role.name)
                    && !dependency
                        .granted_role
                        .eq_ignore_ascii_case(&drop_role.name)
            });
            Ok(())
        })?;
        self.with_compat_granted_privilege_dependency_registry_mut(|registry| {
            registry.dependencies.retain(|dependency| {
                !dependency.grantor.eq_ignore_ascii_case(&drop_role.name)
                    && !dependency
                        .privilege
                        .role_name
                        .eq_ignore_ascii_case(&drop_role.name)
                    && !matches!(
                        &dependency.privilege.target,
                        PrivilegeTarget::Role(granted_role)
                            if granted_role.eq_ignore_ascii_case(&drop_role.name)
                    )
            });
            Ok(())
        })
    }

    #[allow(dead_code)]
    pub(in crate::engine) fn apply_tracked_pg_object_compat_effects(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<()> {
        self.validate_compat_create_type_shell_collision(session, statement_sql, statement)?;
        self.validate_compat_create_range_type_collision(session, statement_sql, statement)?;
        if statement_tracks_compat_types(statement) {
            self.with_session_mut(session, |record| {
                track_compat_types(record, statement_sql, statement);
                Ok(())
            })?;
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub(in crate::engine) fn emit_create_table_inherits_notices(
        &self,
        session: &SessionHandle,
        statement: &Statement,
    ) -> DbResult<()> {
        let Statement::CreateTable(ct) = statement else {
            return Ok(());
        };
        if ct.inherits.is_empty() {
            return Ok(());
        }

        let txn_id = self.plan_cache_txn_id(session)?;
        let child_col_names: Vec<String> =
            ct.columns.iter().map(|c| c.name.to_lowercase()).collect();
        for parent_obj in &ct.inherits {
            let parent_table_name = parent_obj.parts.join(".");
            if let Ok(Some(parent_table)) =
                self.resolve_inherits_parent_table(session, txn_id, parent_obj)
            {
                if let Err(error) = self.with_session_mut(session, |record| {
                    for col in &parent_table.columns {
                        if child_col_names
                            .iter()
                            .any(|c| c.eq_ignore_ascii_case(&col.name))
                        {
                            record.push_notice(format!(
                                "merging column \"{}\" with inherited definition",
                                col.name
                            ));
                        }
                    }
                    Ok(())
                }) {
                    warn!(
                        error = %error,
                        parent_table = %parent_table_name,
                        "failed to persist CREATE TABLE INHERITS notice in session"
                    );
                }
            }
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub(in crate::engine) fn cleanup_drop_function_compat_casts(
        &self,
        session: &SessionHandle,
        statement_sql: &str,
        statement: &Statement,
    ) -> DbResult<()> {
        let Statement::DropFunction(_) = statement else {
            return Ok(());
        };
        let Some(function_name) = parse_drop_function_cascade_name(statement_sql) else {
            return Ok(());
        };

        self.with_session_mut(session, |record| {
            let mut dropped = Vec::new();
            Arc::make_mut(&mut record.compat_user_casts).retain(|entry| {
                let keep = entry
                    .method
                    .function_name()
                    .map_or(true, |name| !name.eq_ignore_ascii_case(&function_name));
                if !keep {
                    dropped.push((entry.source_type.clone(), entry.target_type.clone()));
                }
                keep
            });
            for (source_type, target_type) in dropped {
                if is_builtin_compat_type(&source_type) {
                    record.push_notice(format!(
                        "drop cascades to cast from {} to {}",
                        aiondb_eval::compat_display_type_name(&source_type),
                        target_type,
                    ));
                }
            }
            Ok(())
        })
    }
}

/// Detect `IF NOT EXISTS` as a complete token sequence in the SQL. Case
/// insensitive and ignores intra-token whitespace noise. False positives
/// are bounded: we only look for `if` immediately followed by `not`
/// immediately followed by `exists`.
fn sql_has_if_not_exists(sql: &str) -> bool {
    let needles: &[&str] = &["if", "not", "exists"];
    let mut pos = 0usize;
    for needle in needles {
        let rest = sql.get(pos..).unwrap_or("");
        let idx = rest.char_indices().find(|&(i, _)| {
            let tail = &rest[i..];
            tail.len() >= needle.len()
                && tail[..needle.len()].eq_ignore_ascii_case(needle)
                && tail[needle.len()..]
                    .chars()
                    .next()
                    .map_or(true, |c| !c.is_ascii_alphanumeric() && c != '_')
                && (i == 0
                    || !rest[..i]
                        .chars()
                        .next_back()
                        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_'))
        });
        let Some((offset, _)) = idx else {
            return false;
        };
        pos += offset + needle.len();
    }
    true
}

fn parse_create_cast_statement(statement_sql: &str) -> Option<ParsedCompatCast> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "create")?;
    consume_word_ci(sql, &mut cursor, "cast")?;
    skip_sql_whitespace(sql, &mut cursor);
    if !sql.get(cursor..)?.starts_with('(') {
        return None;
    }
    cursor += 1;
    let source_type = parse_type_reference(sql, &mut cursor)?;
    consume_word_ci(sql, &mut cursor, "as")?;
    let target_type = parse_type_reference(sql, &mut cursor)?;
    skip_sql_whitespace(sql, &mut cursor);
    if !sql.get(cursor..)?.starts_with(')') {
        return None;
    }
    cursor += 1;

    let method = if consume_word_ci(sql, &mut cursor, "without").is_some() {
        consume_word_ci(sql, &mut cursor, "function")?;
        ParsedCompatCastMethod::Binary
    } else if consume_word_ci(sql, &mut cursor, "with").is_some() {
        if consume_word_ci(sql, &mut cursor, "inout").is_some() {
            ParsedCompatCastMethod::InOut
        } else {
            consume_word_ci(sql, &mut cursor, "function")?;
            let function_name = parse_type_reference(sql, &mut cursor)?;
            skip_sql_whitespace(sql, &mut cursor);
            if sql.get(cursor..).is_some_and(|tail| tail.starts_with('(')) {
                let _ = extract_parenthesized(sql, &mut cursor)?;
            }
            ParsedCompatCastMethod::Function(function_name)
        }
    } else {
        return None;
    };

    let context = if consume_word_ci(sql, &mut cursor, "as").is_some() {
        if consume_word_ci(sql, &mut cursor, "implicit").is_some() {
            CompatCastContext::Implicit
        } else if consume_word_ci(sql, &mut cursor, "assignment").is_some() {
            CompatCastContext::Assignment
        } else {
            return None;
        }
    } else {
        CompatCastContext::Explicit
    };

    Some(ParsedCompatCast {
        source_type: normalize_compat_type_name(&source_type),
        target_type: normalize_compat_type_name(&target_type),
        context,
        method,
    })
}

fn parse_drop_cast_statement(statement_sql: &str) -> Option<ParsedCompatDropCast> {
    let sql = trim_compat_statement(statement_sql);
    let mut cursor = 0usize;
    consume_word_ci(sql, &mut cursor, "drop")?;
    consume_word_ci(sql, &mut cursor, "cast")?;
    let saved = cursor;
    if consume_word_ci(sql, &mut cursor, "if").is_some()
        && consume_word_ci(sql, &mut cursor, "exists").is_none()
    {
        cursor = saved;
    }
    skip_sql_whitespace(sql, &mut cursor);
    if !sql.get(cursor..)?.starts_with('(') {
        return None;
    }
    cursor += 1;
    let source_type = parse_type_reference(sql, &mut cursor)?;
    consume_word_ci(sql, &mut cursor, "as")?;
    let target_type = parse_type_reference(sql, &mut cursor)?;
    Some(ParsedCompatDropCast {
        source_type: normalize_compat_type_name(&source_type),
        target_type: normalize_compat_type_name(&target_type),
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedCompatCast {
    source_type: String,
    target_type: String,
    context: CompatCastContext,
    method: ParsedCompatCastMethod,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ParsedCompatCastMethod {
    Binary,
    InOut,
    Function(String),
}

/// Pull the `(policy_name, table_name)` tuple out of the compat misc-object
/// canonical key (`<policy>@@<table>`).
fn split_compat_policy_canonical(canonical: &str) -> Option<(String, String)> {
    let (policy, table) = canonical.split_once("@@")?;
    if policy.is_empty() || table.is_empty() {
        return None;
    }
    Some((policy.to_owned(), table.to_owned()))
}

/// Project a `compat_misc_attrs` row created by `record_compat_misc_create`
/// (or mutated by ALTER POLICY) onto a persistable `PolicyDescriptor`. The
/// options bag mirrors PG's policy semantics so the projection is a
/// straightforward field walk.
fn compat_misc_attrs_to_policy_descriptor(
    canonical: &str,
    attrs: &crate::session::CompatMiscObjectAttrs,
) -> Option<aiondb_catalog::PolicyDescriptor> {
    let (policy_name, default_table) = split_compat_policy_canonical(canonical)?;
    let mut table_name = default_table.clone();
    let mut command = aiondb_catalog::PolicyCommandDescriptor::All;
    let mut kind = aiondb_catalog::PolicyKindDescriptor::Permissive;
    let mut roles: Vec<String> = Vec::new();
    let mut using_expr: Option<String> = None;
    let mut with_check_expr: Option<String> = None;
    for (key, value) in &attrs.options {
        match key.as_str() {
            "table" => table_name = value.clone(),
            "for" => {
                command = match value.to_ascii_lowercase().as_str() {
                    "select" => aiondb_catalog::PolicyCommandDescriptor::Select,
                    "insert" => aiondb_catalog::PolicyCommandDescriptor::Insert,
                    "update" => aiondb_catalog::PolicyCommandDescriptor::Update,
                    "delete" => aiondb_catalog::PolicyCommandDescriptor::Delete,
                    _ => aiondb_catalog::PolicyCommandDescriptor::All,
                };
            }
            "permissive" => {
                kind = if value.eq_ignore_ascii_case("restrictive") {
                    aiondb_catalog::PolicyKindDescriptor::Restrictive
                } else {
                    aiondb_catalog::PolicyKindDescriptor::Permissive
                };
            }
            "to" | "roles" => {
                // SECURITY: match the `|` separator used everywhere the
                // role list is written (see compat/mod.rs CREATE POLICY,
                // ALTER POLICY parser, and the catalog hydration path in
                // query_api.rs). Older snapshots may still carry `,` so we
                // tolerate either separator on read for backwards compat,
                // but new writes use `|` to survive the top-level comma
                // split that flattens compat_misc_attrs into pairs.
                roles = value
                    .split(['|', ','])
                    .map(|role| role.trim().to_owned())
                    .filter(|role| !role.is_empty())
                    .collect();
            }
            "using" => {
                using_expr = Some(value.clone());
            }
            "with_check" => {
                with_check_expr = Some(value.clone());
            }
            _ => {}
        }
    }
    Some(aiondb_catalog::PolicyDescriptor {
        name: policy_name,
        table_name,
        command,
        kind,
        roles,
        using_expr,
        with_check_expr,
        owner: attrs.owner.clone(),
    })
}

/// Project a session-level `CompatUserCast` onto a persistable
/// `CastDescriptor`. The cast's stable OID is reused so `pg_cast` lookups
/// remain stable across restarts.
fn compat_user_cast_to_descriptor(
    cast: &CompatUserCast,
    owner: Option<String>,
) -> aiondb_catalog::CastDescriptor {
    aiondb_catalog::CastDescriptor {
        oid: cast.oid,
        source_type: cast.source_type.clone(),
        target_type: cast.target_type.clone(),
        context: match cast.context {
            CompatCastContext::Explicit => aiondb_catalog::CastContextDescriptor::Explicit,
            CompatCastContext::Assignment => aiondb_catalog::CastContextDescriptor::Assignment,
            CompatCastContext::Implicit => aiondb_catalog::CastContextDescriptor::Implicit,
        },
        method: match &cast.method {
            CompatCastMethod::Binary => aiondb_catalog::CastMethodDescriptor::Binary,
            CompatCastMethod::InOut => aiondb_catalog::CastMethodDescriptor::InOut,
            CompatCastMethod::Function {
                function_name,
                function_oid,
            } => aiondb_catalog::CastMethodDescriptor::Function {
                function_name: function_name.clone(),
                function_oid: *function_oid,
            },
        },
        owner,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedCompatDropCast {
    source_type: String,
    target_type: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_core::SqlState;

    #[test]
    fn if_not_exists_matches_standard_forms() {
        assert!(sql_has_if_not_exists(
            "CREATE SERVER IF NOT EXISTS foo FOREIGN DATA WRAPPER x"
        ));
        assert!(sql_has_if_not_exists("create SERVER If nOt eXiStS foo"));
        assert!(!sql_has_if_not_exists("CREATE SERVER foo"));
        assert!(!sql_has_if_not_exists(
            "CREATE TABLE noif_notexists (x INT)"
        ));
    }

    #[test]
    fn parse_if_exists_object_name_prefers_guard_over_identifier_if() {
        let sql = "ALTER FOREIGN TABLE IF EXISTS doesnt_exist_ft1 OWNER TO role_x";
        let mut cursor = 0usize;
        consume_word_ci(sql, &mut cursor, "alter").expect("ALTER");
        consume_word_ci(sql, &mut cursor, "foreign").expect("FOREIGN");
        consume_word_ci(sql, &mut cursor, "table").expect("TABLE");

        let (if_exists, rendered, bare) = Engine::parse_if_exists_object_name(sql, &mut cursor);
        assert!(if_exists);
        assert_eq!(rendered.as_deref(), Some("doesnt_exist_ft1"));
        assert_eq!(bare.as_deref(), Some("doesnt_exist_ft1"));
    }

    #[test]
    fn parse_if_exists_object_name_keeps_qualified_name_and_bare_tail() {
        let sql = "DROP FOREIGN TABLE IF EXISTS foreign_schema.foreign_table_1";
        let mut cursor = 0usize;
        consume_word_ci(sql, &mut cursor, "drop").expect("DROP");
        consume_word_ci(sql, &mut cursor, "foreign").expect("FOREIGN");
        consume_word_ci(sql, &mut cursor, "table").expect("TABLE");

        let (if_exists, rendered, bare) = Engine::parse_if_exists_object_name(sql, &mut cursor);
        assert!(if_exists);
        assert_eq!(rendered.as_deref(), Some("foreign_schema.foreign_table_1"));
        assert_eq!(bare.as_deref(), Some("foreign_table_1"));
    }

    #[test]
    fn parse_if_exists_object_name_does_not_force_guard_for_plain_if_identifier() {
        let sql = "ALTER FOREIGN TABLE if OWNER TO role_x";
        let mut cursor = 0usize;
        consume_word_ci(sql, &mut cursor, "alter").expect("ALTER");
        consume_word_ci(sql, &mut cursor, "foreign").expect("FOREIGN");
        consume_word_ci(sql, &mut cursor, "table").expect("TABLE");

        let (if_exists, rendered, bare) = Engine::parse_if_exists_object_name(sql, &mut cursor);
        assert!(!if_exists);
        assert_eq!(rendered.as_deref(), Some("if"));
        assert_eq!(bare.as_deref(), Some("if"));
    }

    /// `"if"` quoted is a user-chosen relation name; the cursor must
    /// advance past the closing quote and never re-probe IF EXISTS.
    #[test]
    fn parse_if_exists_object_name_keeps_quoted_if_identifier() {
        let sql = "ALTER FOREIGN TABLE \"if\" OWNER TO role_x";
        let mut cursor = 0usize;
        consume_word_ci(sql, &mut cursor, "alter").expect("ALTER");
        consume_word_ci(sql, &mut cursor, "foreign").expect("FOREIGN");
        consume_word_ci(sql, &mut cursor, "table").expect("TABLE");

        let (if_exists, rendered, bare) = Engine::parse_if_exists_object_name(sql, &mut cursor);
        assert!(!if_exists);
        assert_eq!(rendered.as_deref(), Some("if"));
        assert_eq!(bare.as_deref(), Some("if"));
        let tail = sql[cursor..].trim_start();
        assert!(
            tail.starts_with("OWNER"),
            "cursor should advance past quoted identifier, tail={tail:?}"
        );
    }

    #[test]
    fn parse_if_exists_object_name_lowercase_guard_with_qualified_name() {
        let sql = "alter foreign table if exists foreign_schema.doesnt_exist OWNER TO r";
        let mut cursor = 0usize;
        consume_word_ci(sql, &mut cursor, "alter").expect("ALTER");
        consume_word_ci(sql, &mut cursor, "foreign").expect("FOREIGN");
        consume_word_ci(sql, &mut cursor, "table").expect("TABLE");

        let (if_exists, rendered, bare) = Engine::parse_if_exists_object_name(sql, &mut cursor);
        assert!(if_exists);
        assert_eq!(rendered.as_deref(), Some("foreign_schema.doesnt_exist"));
        assert_eq!(bare.as_deref(), Some("doesnt_exist"));
    }

    #[test]
    fn parse_if_exists_object_name_guard_then_quoted_if_identifier() {
        let sql = "DROP FOREIGN TABLE IF EXISTS \"if\"";
        let mut cursor = 0usize;
        consume_word_ci(sql, &mut cursor, "drop").expect("DROP");
        consume_word_ci(sql, &mut cursor, "foreign").expect("FOREIGN");
        consume_word_ci(sql, &mut cursor, "table").expect("TABLE");

        let (if_exists, rendered, bare) = Engine::parse_if_exists_object_name(sql, &mut cursor);
        assert!(if_exists);
        assert_eq!(rendered.as_deref(), Some("if"));
        assert_eq!(bare.as_deref(), Some("if"));
    }

    #[test]
    fn options_duplicate_check_uses_lexer_not_comments() {
        let sql = "\
CREATE SERVER s FOREIGN DATA WRAPPER fdw
-- OPTIONS (host 'ignored')
OPTIONS (host 'a', host 'b')";

        let err = check_compat_options_clause_duplicates(sql).expect_err("duplicate option");
        assert_eq!(err.sqlstate(), SqlState::DuplicateObject);
        assert_eq!(
            err.report().message,
            "option \"host\" provided more than once"
        );
    }

    #[test]
    fn fdw_redundant_option_check_ignores_comments() {
        let sql = "\
CREATE FOREIGN DATA WRAPPER fdw
-- HANDLER mentioned in a comment must not count
HANDLER fdw_handler";

        check_compat_create_fdw_redundant_options(sql).expect("single real handler is valid");
    }

    #[test]
    fn foreign_table_constraint_check_ignores_comments() {
        let sql = "\
CREATE FOREIGN TABLE ft (
    id int -- PRIMARY KEY mentioned in a comment must not count
) SERVER srv";

        check_compat_create_foreign_table_body(sql).expect("commented constraint is not real DDL");
    }
}
