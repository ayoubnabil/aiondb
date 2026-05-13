#![allow(clippy::collapsible_if, clippy::assigning_clones)]

use aiondb_core::{DataType, DbResult};

use crate::{
    ast::{
        CreateFunctionStatement, CreateTriggerStatement, DropFunctionStatement,
        DropTriggerStatement, FunctionParam, Statement, TriggerEvent, TriggerTiming,
    },
    keywords::Keyword,
    tokens::{tokens_to_sql, TokenKind},
    Parser,
};

impl Parser {
    pub(crate) fn parse_function_signature_arg_type(&mut self) -> DbResult<DataType> {
        let save_idx = self.index;
        if matches!(
            self.current().kind,
            TokenKind::Identifier(_) | TokenKind::Keyword(_)
        ) {
            // Try parsing "<argname> <type>" first.
            self.advance();
            loop {
                match &self.current().kind {
                    TokenKind::Keyword(Keyword::In) => {
                        self.advance();
                    }
                    TokenKind::Identifier(s) if s.eq_ignore_ascii_case("out") => {
                        self.advance();
                    }
                    TokenKind::Identifier(s) if s.eq_ignore_ascii_case("inout") => {
                        self.advance();
                    }
                    TokenKind::Identifier(s) if s.eq_ignore_ascii_case("variadic") => {
                        self.advance();
                    }
                    _ => break,
                }
            }
            if let Ok(data_type) = self.parse_data_type() {
                return Ok(data_type);
            }
            self.index = save_idx;
        }

        match self.parse_data_type() {
            Ok(data_type) => Ok(data_type),
            Err(_) => {
                let (ident, _) = self.expect_identifier()?;
                let mut data_type = self.name_to_data_type(&ident);
                while self.current().kind == TokenKind::LBracket {
                    self.advance(); // consume [
                    if matches!(self.current().kind, TokenKind::Integer(_)) {
                        self.advance();
                    }
                    let _ = self.consume_kind(&TokenKind::RBracket);
                    data_type = DataType::Array(Box::new(data_type));
                }
                Ok(data_type)
            }
        }
    }

    // CREATE [OR REPLACE] FUNCTION name(param type, ...) RETURNS type AS $$ body $$ LANGUAGE sql
    pub(crate) fn parse_create_function_statement(
        &mut self,
        create: crate::tokens::Token,
        or_replace: bool,
    ) -> DbResult<Statement> {
        self.expect_keyword(Keyword::Function)?;
        // Support schema-qualified names: schema.funcname
        let obj_name = self.parse_object_name()?;
        let name = obj_name.parts.join(".");

        // Parse parameter list
        self.expect_token(&TokenKind::LParen)?;
        let mut params = Vec::new();
        let mut out_params = Vec::new();
        if !self.consume_kind(&TokenKind::RParen) {
            loop {
                // Skip optional IN/OUT/INOUT/VARIADIC modifiers (can appear
                // before OR after param name in PG syntax). OUT-only parameters
                // are excluded from function call arity (PostgreSQL behavior).
                let mut saw_out_only = false;
                let mut saw_inout = false;
                let mut saw_variadic = false;
                loop {
                    match &self.current().kind {
                        TokenKind::Keyword(Keyword::In) => {
                            self.advance();
                        }
                        TokenKind::Identifier(s) if s.eq_ignore_ascii_case("out") => {
                            saw_out_only = true;
                            self.advance();
                        }
                        TokenKind::Identifier(s) if s.eq_ignore_ascii_case("inout") => {
                            saw_inout = true;
                            saw_out_only = false;
                            self.advance();
                        }
                        TokenKind::Identifier(s) if s.eq_ignore_ascii_case("variadic") => {
                            saw_variadic = true;
                            self.advance();
                        }
                        _ => break,
                    }
                }

                // Try to parse name + type, or just type
                let save_idx = self.index;
                let (param_name, _) = self.expect_identifier()?;
                let mut pushed_param_index: Option<usize> = None;

                // Skip mode modifiers that appear after the param name (e.g., "validlen OUT int")
                loop {
                    match &self.current().kind {
                        TokenKind::Keyword(Keyword::In) => {
                            self.advance();
                        }
                        TokenKind::Identifier(s) if s.eq_ignore_ascii_case("out") => {
                            saw_out_only = true;
                            self.advance();
                        }
                        TokenKind::Identifier(s) if s.eq_ignore_ascii_case("inout") => {
                            saw_inout = true;
                            saw_out_only = false;
                            self.advance();
                        }
                        TokenKind::Identifier(s) if s.eq_ignore_ascii_case("variadic") => {
                            saw_variadic = true;
                            self.advance();
                        }
                        _ => break,
                    }
                }
                let out_only_param = saw_out_only && !saw_inout;

                // Check if this was the type (no name) - also accept [] suffix for array types
                if matches!(self.current().kind, TokenKind::Comma | TokenKind::RParen) {
                    let raw_type_name =
                        raw_type_name_from_tokens(&self.tokens[save_idx..self.index])
                            .or_else(|| Some(param_name.to_ascii_lowercase()));
                    let data_type = self.name_to_data_type(&param_name);
                    let parsed_param = FunctionParam {
                        name: String::new(),
                        data_type,
                        raw_type_name,
                        variadic: saw_variadic,
                        has_default: false,
                    };
                    if out_only_param {
                        out_params.push(parsed_param);
                    } else {
                        if saw_inout {
                            out_params.push(parsed_param.clone());
                        }
                        params.push(parsed_param);
                        pushed_param_index = Some(params.len().saturating_sub(1));
                    }
                } else if matches!(self.current().kind, TokenKind::LBracket | TokenKind::Eq)
                    || (self.consume_keyword(Keyword::Default).is_some() && {
                        // Undo the consume if we took DEFAULT - we handle it below
                        self.index -= 1;
                        true
                    })
                {
                    // type[] or type = default or type DEFAULT default
                    let mut data_type = self.name_to_data_type(&param_name);
                    while self.current().kind == TokenKind::LBracket {
                        self.advance(); // consume [
                        if matches!(self.current().kind, TokenKind::Integer(_)) {
                            self.advance();
                        }
                        let _ = self.consume_kind(&TokenKind::RBracket);
                        data_type = DataType::Array(Box::new(data_type));
                    }
                    let raw_type_name =
                        raw_type_name_from_tokens(&self.tokens[save_idx..self.index])
                            .or_else(|| Some(param_name.to_ascii_lowercase()));
                    let parsed_param = FunctionParam {
                        name: String::new(),
                        data_type,
                        raw_type_name,
                        variadic: saw_variadic,
                        has_default: false,
                    };
                    if out_only_param {
                        out_params.push(parsed_param);
                    } else {
                        if saw_inout {
                            out_params.push(parsed_param.clone());
                        }
                        params.push(parsed_param);
                        pushed_param_index = Some(params.len().saturating_sub(1));
                    }
                } else {
                    // Try to parse a data type after the param name
                    self.index = save_idx;
                    let first_id = self.expect_identifier()?.0;
                    // Skip mode modifiers after name again
                    #[allow(unused_assignments)]
                    loop {
                        match &self.current().kind {
                            TokenKind::Keyword(Keyword::In) => {
                                self.advance();
                            }
                            TokenKind::Identifier(s) if s.eq_ignore_ascii_case("out") => {
                                saw_out_only = true;
                                self.advance();
                            }
                            TokenKind::Identifier(s) if s.eq_ignore_ascii_case("inout") => {
                                saw_inout = true;
                                saw_out_only = false;
                                self.advance();
                            }
                            TokenKind::Identifier(s) if s.eq_ignore_ascii_case("variadic") => {
                                saw_variadic = true;
                                self.advance();
                            }
                            _ => break,
                        }
                    }
                    let type_start = self.index;
                    match self.parse_data_type() {
                        Ok(data_type) => {
                            let parsed_param = FunctionParam {
                                name: first_id,
                                data_type,
                                raw_type_name: raw_type_name_from_tokens(
                                    &self.tokens[type_start..self.index],
                                ),
                                variadic: saw_variadic,
                                has_default: false,
                            };
                            if out_only_param {
                                out_params.push(parsed_param);
                            } else {
                                if saw_inout {
                                    out_params.push(parsed_param.clone());
                                }
                                params.push(parsed_param);
                                pushed_param_index = Some(params.len().saturating_sub(1));
                            }
                        }
                        Err(_) => {
                            // Failed to parse type - use the first_id as the type
                            let mut data_type = self.name_to_data_type(&first_id);
                            // Consume optional dotted suffix: schema.name or table.col%TYPE
                            while self.consume_kind(&TokenKind::Dot) {
                                if matches!(
                                    self.current().kind,
                                    TokenKind::Identifier(_) | TokenKind::Keyword(_)
                                ) {
                                    self.advance();
                                }
                            }
                            // Consume optional %TYPE suffix
                            if self.current().kind == TokenKind::Percent {
                                self.advance(); // consume %
                                let _ = self.consume_keyword(Keyword::Type);
                            }
                            // Consume trailing [] array brackets
                            while self.current().kind == TokenKind::LBracket {
                                self.advance();
                                if matches!(self.current().kind, TokenKind::Integer(_)) {
                                    self.advance();
                                }
                                let _ = self.consume_kind(&TokenKind::RBracket);
                                data_type = DataType::Array(Box::new(data_type));
                            }
                            let parsed_param = FunctionParam {
                                name: String::new(),
                                data_type,
                                raw_type_name: Some(first_id.to_ascii_lowercase()),
                                variadic: saw_variadic,
                                has_default: false,
                            };
                            if out_only_param {
                                out_params.push(parsed_param);
                            } else {
                                if saw_inout {
                                    out_params.push(parsed_param.clone());
                                }
                                params.push(parsed_param);
                                pushed_param_index = Some(params.len().saturating_sub(1));
                            }
                        }
                    }
                }
                // Skip optional DEFAULT value (PG also accepts = as synonym for DEFAULT)
                let has_default_value = self.consume_keyword(Keyword::Default).is_some()
                    || self.consume_kind(&TokenKind::Eq);
                if has_default_value {
                    if out_only_param {
                        return self.syntax_error(
                            self.current().span,
                            "only input parameters can have default values",
                        );
                    }
                    let _ = self.parse_expr();
                    if let Some(index) = pushed_param_index {
                        if let Some(param) = params.get_mut(index) {
                            param.has_default = true;
                        }
                    }
                }
                if !self.consume_kind(&TokenKind::Comma) {
                    break;
                }
            }
            self.expect_token(&TokenKind::RParen)?;
        }

        // Parse function attributes in any order
        let mut return_type = DataType::Text; // default if RETURNS not specified
        let mut raw_return_type_name = None;
        let mut body = String::new();
        let mut language = String::from("sql");
        let mut last_span = self.previous_span();

        loop {
            if self.consume_keyword(Keyword::Returns).is_some() {
                // RETURNS NULL ON NULL INPUT - function attribute, not return type.
                // Peek: if next is NULL followed by ON, this is the attribute form.
                if matches!(self.current().kind, TokenKind::Keyword(Keyword::Null)) {
                    if self.index + 1 < self.tokens.len()
                        && matches!(
                            self.tokens[self.index + 1].kind,
                            TokenKind::Keyword(Keyword::On)
                        )
                    {
                        // Consume: NULL ON NULL INPUT
                        self.advance(); // NULL
                        self.advance(); // ON
                        let _ = self.consume_keyword(Keyword::Null);
                        if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("input"))
                        {
                            self.advance();
                        }
                        last_span = self.previous_span();
                        continue;
                    }
                }
                // RETURNS [SETOF] type [TABLE (...)]
                if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("setof"))
                {
                    self.advance();
                }
                if matches!(&self.current().kind, TokenKind::Keyword(Keyword::Table)) {
                    self.advance();
                    // TABLE (col type, ...) exposes output columns similarly
                    // to OUT parameters; keep those names for PL/pgSQL RETURN NEXT.
                    if self.consume_kind(&TokenKind::LParen)
                        && !self.consume_kind(&TokenKind::RParen)
                    {
                        loop {
                            let (column_name, _) = self.expect_identifier()?;
                            let type_start = self.index;
                            let data_type = self.parse_data_type()?;
                            out_params.push(FunctionParam {
                                name: column_name,
                                data_type,
                                raw_type_name: raw_type_name_from_tokens(
                                    &self.tokens[type_start..self.index],
                                ),
                                variadic: false,
                                has_default: false,
                            });
                            if self.consume_kind(&TokenKind::RParen) {
                                break;
                            }
                            self.expect_token(&TokenKind::Comma)?;
                        }
                    }
                    raw_return_type_name = None;
                } else {
                    let type_start = self.index;
                    return_type = self.parse_data_type()?;
                    raw_return_type_name =
                        raw_type_name_from_tokens(&self.tokens[type_start..self.index]);
                }
                last_span = self.previous_span();
            } else if self.consume_keyword(Keyword::As).is_some() {
                // Skip optional psql colon prefix: AS :'varname'
                let _ = self.consume_kind(&TokenKind::Colon);
                body = match &self.current().kind {
                    TokenKind::String(s) => s.clone(),
                    _ => String::new(),
                };
                if !body.is_empty() || matches!(self.current().kind, TokenKind::String(_)) {
                    self.advance();
                }
                last_span = self.previous_span();
                // Some CREATE FUNCTION use: AS 'body', 'link_symbol'
                // Also handle AS :'lib', 'funcname'
                if self.consume_kind(&TokenKind::Comma) {
                    let _ = self.consume_kind(&TokenKind::Colon);
                    if matches!(self.current().kind, TokenKind::String(_)) {
                        self.advance();
                    }
                }
            } else if self.consume_keyword(Keyword::Language).is_some() {
                // LANGUAGE can be followed by identifier (sql) or string ('sql')
                if let TokenKind::String(s) = &self.current().kind {
                    language = s.clone();
                    self.advance();
                } else if let Ok((lang, _)) = self.expect_identifier() {
                    language = lang;
                }
                last_span = self.previous_span();
            } else if self.consume_keyword(Keyword::Begin).is_some() {
                // SQL-standard function body: BEGIN ATOMIC ... END
                // Skip optional ATOMIC keyword
                if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("atomic"))
                {
                    self.advance();
                }
                // Record start of body tokens
                let body_start = self.index;
                // Skip everything until END keyword at matching depth
                let mut depth = 1u32;
                while !self.is_eof() && depth > 0 {
                    if matches!(
                        self.current().kind,
                        TokenKind::Keyword(Keyword::Begin | Keyword::Case)
                    ) {
                        depth += 1;
                    } else if matches!(self.current().kind, TokenKind::Keyword(Keyword::End)) {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    self.advance();
                }
                // Capture the body text from tokens between ATOMIC and END
                let body_end = self.index;
                if body.is_empty() && body_start < body_end {
                    body = tokens_to_sql(&self.tokens[body_start..body_end]);
                }
                if !self.is_eof() {
                    self.advance(); // consume END
                }
                last_span = self.previous_span();
            } else if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("return"))
            {
                // SQL-standard function body: RETURN expr
                self.advance(); // consume RETURN
                                // Record start of body tokens (the expression after RETURN)
                let body_start = self.index;
                // Skip balanced tokens until we hit a function attribute keyword or ;/EOF
                let mut paren_depth = 0u32;
                while !self.is_eof() {
                    match self.current().kind {
                        TokenKind::Semicolon if paren_depth == 0 => break,
                        TokenKind::LParen => {
                            paren_depth += 1;
                            self.advance();
                        }
                        TokenKind::RParen => {
                            paren_depth = paren_depth.saturating_sub(1);
                            self.advance();
                        }
                        // Stop at function attribute keywords at top-level
                        TokenKind::Keyword(Keyword::Language | Keyword::As | Keyword::Set)
                            if paren_depth == 0 =>
                        {
                            break;
                        }
                        _ if paren_depth == 0
                            && matches!(&self.current().kind, TokenKind::Identifier(s) if
                                s.eq_ignore_ascii_case("immutable") || s.eq_ignore_ascii_case("stable")
                                || s.eq_ignore_ascii_case("volatile") || s.eq_ignore_ascii_case("strict")
                                || s.eq_ignore_ascii_case("called") || s.eq_ignore_ascii_case("security")
                                || s.eq_ignore_ascii_case("parallel") || s.eq_ignore_ascii_case("cost")
                                || s.eq_ignore_ascii_case("rows") || s.eq_ignore_ascii_case("support")
                                || s.eq_ignore_ascii_case("leakproof")
                            ) =>
                        {
                            break;
                        }
                        _ => {
                            self.advance();
                        }
                    }
                }
                // Capture body text from the RETURN expression tokens
                let body_end = self.index;
                if body.is_empty() && body_start < body_end {
                    body = format!(
                        "SELECT {}",
                        tokens_to_sql(&self.tokens[body_start..body_end])
                    );
                }
                last_span = self.previous_span();
            } else if matches!(&self.current().kind, TokenKind::Identifier(s) if
                s.eq_ignore_ascii_case("immutable") || s.eq_ignore_ascii_case("stable")
                || s.eq_ignore_ascii_case("volatile") || s.eq_ignore_ascii_case("strict")
                || s.eq_ignore_ascii_case("called") || s.eq_ignore_ascii_case("security")
                || s.eq_ignore_ascii_case("parallel") || s.eq_ignore_ascii_case("cost")
                || s.eq_ignore_ascii_case("rows") || s.eq_ignore_ascii_case("support")
                || s.eq_ignore_ascii_case("leakproof") || s.eq_ignore_ascii_case("transform")
                || s.eq_ignore_ascii_case("window") || s.eq_ignore_ascii_case("safe")
                || s.eq_ignore_ascii_case("unsafe") || s.eq_ignore_ascii_case("restricted")
            ) {
                self.advance();
                // Skip additional tokens for multi-word attributes
                // CALLED ON NULL INPUT, RETURNS NULL ON NULL INPUT, SECURITY DEFINER/INVOKER
                // PARALLEL SAFE/UNSAFE/RESTRICTED, COST n, ROWS n
                // Note: ON is a keyword, not identifier, so we must also check for it.
                while matches!(
                    &self.current().kind,
                    TokenKind::Identifier(_)
                        | TokenKind::Integer(_)
                        | TokenKind::NumericLiteral(_)
                        | TokenKind::Keyword(Keyword::On | Keyword::Null)
                ) {
                    if matches!(&self.current().kind, TokenKind::Identifier(s) if
                        s.eq_ignore_ascii_case("input") || s.eq_ignore_ascii_case("definer")
                        || s.eq_ignore_ascii_case("invoker") || s.eq_ignore_ascii_case("safe")
                        || s.eq_ignore_ascii_case("unsafe") || s.eq_ignore_ascii_case("restricted")
                        || s.eq_ignore_ascii_case("null") || s.eq_ignore_ascii_case("on")
                    ) || matches!(
                        self.current().kind,
                        TokenKind::Integer(_)
                            | TokenKind::NumericLiteral(_)
                            | TokenKind::Keyword(Keyword::On | Keyword::Null)
                    ) {
                        self.advance();
                    } else {
                        break;
                    }
                }
                last_span = self.previous_span();
            } else if self.consume_keyword(Keyword::Not).is_some() {
                // NOT LEAKPROOF
                if matches!(&self.current().kind, TokenKind::Identifier(_)) {
                    self.advance();
                }
                last_span = self.previous_span();
            } else if self.consume_keyword(Keyword::Set).is_some() {
                // SET parameter = value - skip to next function attribute boundary
                while !self.is_eof()
                    && !matches!(
                        self.current().kind,
                        TokenKind::Semicolon
                            | TokenKind::Keyword(
                                Keyword::As
                                    | Keyword::Language
                                    | Keyword::Returns
                                    | Keyword::Begin
                                    | Keyword::Set
                                    | Keyword::Not
                            )
                    )
                {
                    // Also stop at known function attribute identifiers
                    if matches!(&self.current().kind, TokenKind::Identifier(s) if
                        s.eq_ignore_ascii_case("immutable") || s.eq_ignore_ascii_case("stable")
                        || s.eq_ignore_ascii_case("volatile") || s.eq_ignore_ascii_case("strict")
                        || s.eq_ignore_ascii_case("called") || s.eq_ignore_ascii_case("security")
                        || s.eq_ignore_ascii_case("parallel") || s.eq_ignore_ascii_case("cost")
                        || s.eq_ignore_ascii_case("rows") || s.eq_ignore_ascii_case("support")
                        || s.eq_ignore_ascii_case("leakproof") || s.eq_ignore_ascii_case("return")
                    ) {
                        break;
                    }
                    self.advance();
                }
                last_span = self.previous_span();
            } else {
                break;
            }
        }

        Ok(Statement::CreateFunction(CreateFunctionStatement {
            name,
            params,
            out_params,
            return_type,
            raw_return_type_name,
            body,
            language,
            or_replace,
            span: create.span.merge(last_span),
        }))
    }

    // DROP FUNCTION [IF EXISTS] name [(arg_types)]
    // Also supports schema-qualified names: DROP FUNCTION schema.name(...)
    pub(crate) fn parse_drop_function_statement(
        &mut self,
        drop: crate::tokens::Token,
        if_exists: bool,
    ) -> DbResult<Statement> {
        self.expect_keyword(Keyword::Function)?;
        let if_exists = self.consume_if_exists_clause_with_preconsumed(if_exists)?;
        let obj_name = self.parse_object_name()?;
        let name = obj_name.parts.join(".");
        let mut span_end = obj_name.span;
        let mut arg_types: Option<Vec<DataType>> = None;
        // Parse optional argument type list: (int, text, ...)
        if self.consume_kind(&TokenKind::LParen) {
            let mut parsed_types = Vec::new();
            if self.consume_kind(&TokenKind::RParen) {
                span_end = self.previous_span();
            } else {
                loop {
                    // Skip optional mode modifiers in signatures.
                    loop {
                        match &self.current().kind {
                            TokenKind::Keyword(Keyword::In) => {
                                self.advance();
                            }
                            TokenKind::Identifier(s) if s.eq_ignore_ascii_case("out") => {
                                self.advance();
                            }
                            TokenKind::Identifier(s) if s.eq_ignore_ascii_case("inout") => {
                                self.advance();
                            }
                            TokenKind::Identifier(s) if s.eq_ignore_ascii_case("variadic") => {
                                self.advance();
                            }
                            _ => break,
                        }
                    }

                    let parsed = self.parse_function_signature_arg_type()?;
                    parsed_types.push(parsed);
                    if !self.consume_kind(&TokenKind::Comma) {
                        let rparen = self.expect_token(&TokenKind::RParen)?;
                        span_end = rparen.span;
                        break;
                    }
                }
            }
            arg_types = Some(parsed_types);
        }
        // Skip additional comma-separated function signatures:
        // DROP FUNCTION f1(), f2(int)
        while self.consume_kind(&TokenKind::Comma) {
            self.skip_to_semicolon(&mut span_end);
        }
        Ok(Statement::DropFunction(DropFunctionStatement {
            name,
            if_exists,
            arg_types,
            span: drop.span.merge(span_end),
        }))
    }

    // CREATE TRIGGER name BEFORE|AFTER INSERT|UPDATE|DELETE ON table
    //   [FOR EACH ROW] EXECUTE FUNCTION func_name()
    pub(crate) fn parse_create_trigger_statement(
        &mut self,
        create: crate::tokens::Token,
    ) -> DbResult<Statement> {
        self.parse_create_trigger_statement_with_replace(create, false)
    }

    pub(crate) fn parse_create_trigger_statement_with_replace(
        &mut self,
        create: crate::tokens::Token,
        or_replace: bool,
    ) -> DbResult<Statement> {
        // Support CREATE CONSTRAINT TRIGGER ... in addition to CREATE TRIGGER ...
        let _ = self.consume_keyword(Keyword::Constraint);
        self.expect_keyword(Keyword::Trigger)?;
        let (name, _name_span) = self.expect_identifier()?;

        // BEFORE | AFTER | INSTEAD OF
        let timing = if self.consume_keyword(Keyword::Before).is_some() {
            TriggerTiming::Before
        } else if self.consume_keyword(Keyword::After).is_some() {
            TriggerTiming::After
        } else if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("instead"))
        {
            self.advance(); // consume INSTEAD
            self.expect_keyword(Keyword::Of)?;
            TriggerTiming::InsteadOf
        } else {
            return self.syntax_error_current("expected BEFORE or AFTER");
        };

        // INSERT | UPDATE | DELETE [OR INSERT | UPDATE | DELETE | TRUNCATE ...]
        let mut has_column_list = false;
        let mut events: Vec<TriggerEvent> = Vec::new();
        let mut update_columns: Vec<String> = Vec::new();
        let parse_update_of_columns = |this: &mut Parser, cols: &mut Vec<String>| -> DbResult<()> {
            loop {
                let (col_name, _) = this.expect_identifier()?;
                let lower = col_name.to_ascii_lowercase();
                if cols.iter().any(|c| c == &lower) {
                    return this.syntax_error_current(format!(
                        "column \"{col_name}\" specified more than once"
                    ));
                }
                cols.push(lower);
                if !this.consume_kind(&TokenKind::Comma) {
                    break;
                }
            }
            Ok(())
        };
        let event = if self.consume_keyword(Keyword::Insert).is_some() {
            // INSERT cannot have OF column-list - PG raises "syntax error at or near OF"
            if matches!(&self.current().kind, TokenKind::Keyword(Keyword::Of)) {
                return self.syntax_error_current("syntax error at or near \"OF\"");
            }
            events.push(TriggerEvent::Insert);
            TriggerEvent::Insert
        } else if self.consume_keyword(Keyword::Update).is_some() {
            // Optional OF col1, col2 - PG validates column uniqueness
            if self.consume_keyword(Keyword::Of).is_some() {
                has_column_list = true;
                parse_update_of_columns(self, &mut update_columns)?;
            }
            events.push(TriggerEvent::Update);
            TriggerEvent::Update
        } else if self.consume_keyword(Keyword::Delete).is_some() {
            events.push(TriggerEvent::Delete);
            TriggerEvent::Delete
        } else if self.consume_keyword(Keyword::Truncate).is_some()
            || matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("truncate"))
                && {
                    self.advance();
                    true
                }
        {
            return self.syntax_error_current("TRUNCATE triggers are not supported");
        } else {
            return self.syntax_error_current("expected INSERT, UPDATE, or DELETE");
        };
        // Consume additional OR event clauses; PG rejects duplicates.
        while self.consume_keyword(Keyword::Or).is_some() {
            let next_event = if self.consume_keyword(Keyword::Insert).is_some() {
                if matches!(&self.current().kind, TokenKind::Keyword(Keyword::Of)) {
                    return self.syntax_error_current("syntax error at or near \"OF\"");
                }
                TriggerEvent::Insert
            } else if self.consume_keyword(Keyword::Delete).is_some() {
                TriggerEvent::Delete
            } else if self.consume_keyword(Keyword::Truncate).is_some()
                || matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("truncate"))
                    && {
                        self.advance();
                        true
                    }
            {
                return self.syntax_error_current("TRUNCATE triggers are not supported");
            } else if self.consume_keyword(Keyword::Update).is_some() {
                if self.consume_keyword(Keyword::Of).is_some() {
                    has_column_list = true;
                    parse_update_of_columns(self, &mut update_columns)?;
                }
                TriggerEvent::Update
            } else {
                break;
            };
            if events.contains(&next_event) {
                return self
                    .syntax_error_current("duplicate trigger events specified at or near \"ON\"");
            }
            events.push(next_event);
        }

        // ON table
        self.expect_keyword(Keyword::On)?;
        let table = self.parse_object_name()?;

        // Skip optional DEFERRABLE / NOT DEFERRABLE / INITIALLY DEFERRED/IMMEDIATE
        loop {
            if self.consume_keyword(Keyword::Not).is_some() {
                let _ = self.consume_keyword(Keyword::Deferrable);
            } else if self.consume_keyword(Keyword::Deferrable).is_some() {
                // ok
            } else if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("initially"))
            {
                self.advance();
                self.advance(); // DEFERRED or IMMEDIATE
            } else {
                break;
            }
        }

        // Optional: REFERENCING OLD TABLE AS name NEW TABLE AS name
        if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("referencing"))
        {
            self.advance();
            // Consume OLD/NEW TABLE AS name pairs
            loop {
                if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("old") || s.eq_ignore_ascii_case("new"))
                {
                    self.advance();
                    let _ = self.consume_keyword(Keyword::Table);
                    let _ = self.consume_keyword(Keyword::As);
                    let _ = self.expect_identifier();
                } else {
                    break;
                }
            }
        }

        // Optional: FOR EACH ROW | FOR EACH STATEMENT
        let for_each_row = if self.consume_keyword(Keyword::For).is_some() {
            let _ = self.consume_keyword(Keyword::Each);
            if self.current().kind == TokenKind::Keyword(Keyword::Row) {
                self.advance();
                true
            } else {
                // STATEMENT or other
                self.advance(); // consume STATEMENT or whatever follows
                false
            }
        } else {
            false
        };

        // Optional: WHEN (condition).  Inspect the token stream for references
        // to OLD./NEW. so we can enforce PG validity rules:
        //   * INSERT trigger WHEN cannot reference OLD
        //   * DELETE trigger WHEN cannot reference NEW
        //   * BEFORE trigger WHEN cannot reference NEW system columns
        let has_when_condition = if self.consume_keyword(Keyword::When).is_some() {
            let mut saw_old = false;
            let mut saw_new = false;
            let mut new_system_col: Option<String> = None;
            const SYSTEM_COLS: &[&str] = &["tableoid", "ctid", "xmin", "xmax", "cmin", "cmax"];
            if self.consume_kind(&TokenKind::LParen) {
                let mut depth = 1;
                while depth > 0 {
                    let kind_now = self.current().kind.clone();
                    match kind_now {
                        TokenKind::LParen => {
                            depth += 1;
                            self.advance();
                        }
                        TokenKind::RParen => {
                            depth -= 1;
                            self.advance();
                        }
                        TokenKind::Eof => break,
                        TokenKind::Identifier(ref ident) => {
                            let lower = ident.to_ascii_lowercase();
                            let next_dot = self
                                .tokens
                                .get(self.index + 1)
                                .map(|t| matches!(t.kind, TokenKind::Dot))
                                .unwrap_or(false);
                            if next_dot && lower == "old" {
                                saw_old = true;
                            }
                            if next_dot && lower == "new" {
                                saw_new = true;
                                if let Some(col_tok) = self.tokens.get(self.index + 2) {
                                    if let TokenKind::Identifier(col) = &col_tok.kind {
                                        let col_lower = col.to_ascii_lowercase();
                                        if SYSTEM_COLS.contains(&col_lower.as_str()) {
                                            new_system_col = Some(col_lower);
                                        }
                                    }
                                }
                            }
                            self.advance();
                        }
                        _ => {
                            self.advance();
                        }
                    }
                }
            }
            // Validate against trigger event/timing.  PG rejects WHEN clauses
            // that reference values not provided by every covered event:
            //   * INSERT events have no OLD row.
            //   * DELETE events have no NEW row.
            let has_insert = events.contains(&TriggerEvent::Insert);
            let has_delete = events.contains(&TriggerEvent::Delete);
            if saw_old && has_insert {
                return self.syntax_error_current(
                    "INSERT trigger's WHEN condition cannot reference OLD values",
                );
            }
            if saw_new && has_delete {
                return self.syntax_error_current(
                    "DELETE trigger's WHEN condition cannot reference NEW values",
                );
            }
            if timing == TriggerTiming::Before && new_system_col.is_some() {
                return self.syntax_error_current(
                    "BEFORE trigger's WHEN condition cannot reference NEW system columns",
                );
            }
            // Statement-level triggers see no row, so any OLD/NEW reference is
            // invalid. PG raises the same wording on every offending column.
            if !for_each_row && (saw_old || saw_new) {
                return self.syntax_error_current(
                    "statement trigger's WHEN condition cannot reference column values",
                );
            }
            true
        } else {
            false
        };

        // EXECUTE FUNCTION func_name() or EXECUTE PROCEDURE func_name()
        self.expect_keyword(Keyword::Execute)?;
        if self.consume_keyword(Keyword::Function).is_none() {
            let _ = self.consume_keyword(Keyword::Procedure);
        }
        // Support schema-qualified function names: schema.func_name
        let fn_obj = self.parse_object_name()?;
        let function_name = fn_obj.parts.join(".");

        // Consume the argument list () - may contain arguments
        self.expect_token(&TokenKind::LParen)?;
        let mut function_args: Vec<String> = Vec::new();
        if self.current().kind != TokenKind::RParen {
            // Parse arguments (string literals or identifiers)
            loop {
                match &self.current().kind {
                    TokenKind::RParen => break,
                    TokenKind::String(s) => {
                        function_args.push(s.clone());
                        self.advance();
                    }
                    TokenKind::Identifier(s) => {
                        function_args.push(s.clone());
                        self.advance();
                    }
                    TokenKind::Integer(n) => {
                        function_args.push(n.to_string());
                        self.advance();
                    }
                    TokenKind::NumericLiteral(n) => {
                        function_args.push(n.clone());
                        self.advance();
                    }
                    TokenKind::Eof => break,
                    _ => {
                        self.advance();
                    }
                }
                if !self.consume_kind(&TokenKind::Comma) {
                    break;
                }
            }
        }
        let rparen = self.expect_token(&TokenKind::RParen)?;

        Ok(Statement::CreateTrigger(CreateTriggerStatement {
            name,
            or_replace,
            timing,
            event,
            events,
            table,
            for_each_row,
            function_name,
            function_args,
            has_column_list,
            has_when_condition,
            update_columns,
            span: create.span.merge(rparen.span),
        }))
    }

    // DROP TRIGGER name ON table
    pub(crate) fn parse_drop_trigger_statement(
        &mut self,
        drop: crate::tokens::Token,
    ) -> DbResult<Statement> {
        self.expect_keyword(Keyword::Trigger)?;
        // Handle optional IF EXISTS (may also have been consumed by caller)
        let if_exists = if self.consume_keyword(Keyword::If).is_some() {
            let _ = self.consume_keyword(Keyword::Exists);
            true
        } else {
            false
        };
        let (name, _name_span) = self.expect_identifier()?;
        self.expect_keyword(Keyword::On)?;
        let table = self.parse_object_name()?;
        // Skip optional CASCADE/RESTRICT
        let _ = self.consume_keyword(Keyword::Cascade);
        let _ = self.consume_keyword(Keyword::Restrict);
        Ok(Statement::DropTrigger(DropTriggerStatement {
            name,
            table: table.clone(),
            if_exists,
            span: drop.span.merge(table.span),
        }))
    }

    // CREATE TENANT <name>
    pub(crate) fn parse_create_tenant_statement(
        &mut self,
        create: crate::tokens::Token,
    ) -> DbResult<Statement> {
        self.expect_keyword(Keyword::Tenant)?;
        let (name, name_span) = self.expect_identifier()?;
        Ok(Statement::CreateTenant {
            name,
            span: create.span.merge(name_span),
        })
    }

    // DROP TENANT <name>
    pub(crate) fn parse_drop_tenant_statement(
        &mut self,
        drop: crate::tokens::Token,
    ) -> DbResult<Statement> {
        self.expect_keyword(Keyword::Tenant)?;
        let (name, name_span) = self.expect_identifier()?;
        Ok(Statement::DropTenant {
            name,
            span: drop.span.merge(name_span),
        })
    }

    pub(crate) fn parse_analyze_statement(&mut self) -> DbResult<Statement> {
        let analyze = self.expect_keyword(Keyword::Analyze)?;

        // Skip optional VERBOSE keyword
        if matches!(&self.current().kind, TokenKind::Identifier(s) if s.eq_ignore_ascii_case("verbose"))
        {
            self.advance();
        }

        // Skip optional parenthesized options like (VERBOSE)
        if self.consume_kind(&TokenKind::LParen) {
            let mut depth = 1u32;
            while !self.is_eof() && depth > 0 {
                match self.current().kind {
                    TokenKind::LParen => depth += 1,
                    TokenKind::RParen => depth -= 1,
                    _ => {}
                }
                self.advance();
            }
        }

        if self.is_eof() || matches!(self.current().kind, TokenKind::Semicolon | TokenKind::Eof) {
            Ok(Statement::Analyze {
                table: None,
                span: analyze.span,
            })
        } else {
            let table = self.parse_object_name()?;
            let mut span = analyze.span.merge(table.span);
            // Skip trailing column list or other options
            while !self.is_eof() && !matches!(self.current().kind, TokenKind::Semicolon) {
                span = span.merge(self.advance().span);
                if matches!(self.tokens[self.index - 1].kind, TokenKind::LParen) {
                    let mut depth = 1u32;
                    while !self.is_eof() && depth > 0 {
                        match self.current().kind {
                            TokenKind::LParen => depth += 1,
                            TokenKind::RParen => depth -= 1,
                            _ => {}
                        }
                        span = span.merge(self.advance().span);
                    }
                }
            }
            Ok(Statement::Analyze {
                table: Some(table),
                span,
            })
        }
    }

    pub(crate) fn parse_vacuum_statement(&mut self) -> DbResult<Statement> {
        let vacuum = self.expect_keyword(Keyword::Vacuum)?;

        // Skip optional modifiers: FULL, FREEZE, VERBOSE, ANALYZE
        loop {
            if matches!(
                &self.current().kind,
                TokenKind::Identifier(s) if s.eq_ignore_ascii_case("verbose")
                    || s.eq_ignore_ascii_case("full")
                    || s.eq_ignore_ascii_case("freeze")
            ) {
                self.advance();
                continue;
            }
            if matches!(self.current().kind, TokenKind::Keyword(Keyword::Analyze)) {
                self.advance();
                continue;
            }
            break;
        }

        // Skip optional parenthesized options like (FULL, VERBOSE, ANALYZE)
        if self.consume_kind(&TokenKind::LParen) {
            let mut depth = 1u32;
            while !self.is_eof() && depth > 0 {
                match self.current().kind {
                    TokenKind::LParen => depth += 1,
                    TokenKind::RParen => depth -= 1,
                    _ => {}
                }
                self.advance();
            }
        }

        if self.is_eof() || matches!(self.current().kind, TokenKind::Semicolon | TokenKind::Eof) {
            Ok(Statement::Vacuum {
                table: None,
                span: vacuum.span,
            })
        } else {
            let table = self.parse_object_name()?;
            let mut span = vacuum.span.merge(table.span);
            // Skip trailing column list or other options
            while !self.is_eof() && !matches!(self.current().kind, TokenKind::Semicolon) {
                span = span.merge(self.advance().span);
                if matches!(self.tokens[self.index - 1].kind, TokenKind::LParen) {
                    let mut depth = 1u32;
                    while !self.is_eof() && depth > 0 {
                        match self.current().kind {
                            TokenKind::LParen => depth += 1,
                            TokenKind::RParen => depth -= 1,
                            _ => {}
                        }
                        span = span.merge(self.advance().span);
                    }
                }
            }
            Ok(Statement::Vacuum {
                table: Some(table),
                span,
            })
        }
    }
}

fn raw_type_name_from_tokens(tokens: &[crate::tokens::Token]) -> Option<String> {
    let mut index = 0usize;
    while matches!(
        tokens.get(index).map(|token| &token.kind),
        Some(TokenKind::Keyword(Keyword::Setof))
    ) {
        index += 1;
    }

    let mut name = match tokens.get(index).map(|token| &token.kind) {
        Some(TokenKind::Identifier(value)) => value.to_ascii_lowercase(),
        Some(TokenKind::Keyword(keyword)) => keyword.name().to_ascii_lowercase(),
        _ => return None,
    };
    index += 1;

    if matches!(
        tokens.get(index).map(|token| &token.kind),
        Some(TokenKind::Dot)
    ) {
        index += 1;
        match tokens.get(index).map(|token| &token.kind) {
            Some(TokenKind::Identifier(value)) => {
                name.push('.');
                name.push_str(&value.to_ascii_lowercase());
            }
            Some(TokenKind::Keyword(keyword)) => {
                name.push('.');
                name.push_str(&keyword.name().to_ascii_lowercase());
            }
            _ => {}
        }
        index += 1;
    }

    if matches!(
        tokens.get(index).map(|token| &token.kind),
        Some(TokenKind::Keyword(Keyword::Array))
    ) {
        name.push_str("[]");
        index += 1;
        if matches!(
            tokens.get(index).map(|token| &token.kind),
            Some(TokenKind::LBracket)
        ) {
            index += 1;
            if matches!(
                tokens.get(index).map(|token| &token.kind),
                Some(TokenKind::Integer(_))
            ) {
                index += 1;
            }
            if matches!(
                tokens.get(index).map(|token| &token.kind),
                Some(TokenKind::RBracket)
            ) {
                index += 1;
            }
        }
    }

    while matches!(
        tokens.get(index).map(|token| &token.kind),
        Some(TokenKind::LBracket)
    ) {
        index += 1;
        if matches!(
            tokens.get(index).map(|token| &token.kind),
            Some(TokenKind::Integer(_))
        ) {
            index += 1;
        }
        if matches!(
            tokens.get(index).map(|token| &token.kind),
            Some(TokenKind::RBracket)
        ) {
            index += 1;
        }
        name.push_str("[]");
    }

    Some(name)
}

#[cfg(test)]
mod tests {
    use crate::parse_prepared_statement;

    #[test]
    fn create_function_schema_qualified_name_is_preserved() {
        let stmt = parse_prepared_statement(
            "CREATE FUNCTION analytics.trg_fn(x INT) RETURNS INT AS 'x' LANGUAGE sql",
        )
        .expect("CREATE FUNCTION should parse");
        match stmt {
            crate::Statement::CreateFunction(ref s) => {
                assert_eq!(s.name, "analytics.trg_fn");
            }
            _ => panic!("expected CreateFunction, got {stmt:?}"),
        }
    }

    #[test]
    fn drop_function_schema_qualified_name_is_preserved() {
        let stmt = parse_prepared_statement("DROP FUNCTION analytics.trg_fn(INT)")
            .expect("DROP FUNCTION should parse");
        match stmt {
            crate::Statement::DropFunction(ref s) => {
                assert_eq!(s.name, "analytics.trg_fn");
            }
            _ => panic!("expected DropFunction, got {stmt:?}"),
        }
    }

    #[test]
    fn drop_function_signature_accepts_named_pseudo_type_args() {
        let stmt = parse_prepared_statement("DROP FUNCTION f1(x anyelement, y anyarray)")
            .expect("DROP FUNCTION with named pseudo-type args should parse");
        match stmt {
            crate::Statement::DropFunction(ref s) => {
                assert_eq!(s.name, "f1");
                assert_eq!(
                    s.arg_types,
                    Some(vec![
                        aiondb_core::DataType::Text,
                        aiondb_core::DataType::Text
                    ])
                );
            }
            _ => panic!("expected DropFunction, got {stmt:?}"),
        }
    }

    #[test]
    fn create_trigger_instead_of_parses_as_create_trigger() {
        let stmt = parse_prepared_statement(
            "CREATE TRIGGER t INSTEAD OF INSERT ON items FOR EACH ROW EXECUTE FUNCTION trg_fn()",
        )
        .expect("INSTEAD OF trigger should parse");
        match stmt {
            crate::Statement::CreateTrigger(ref s) => {
                assert_eq!(s.name, "t");
                assert_eq!(s.timing, crate::TriggerTiming::InsteadOf);
                assert_eq!(s.event, crate::TriggerEvent::Insert);
                assert!(s.for_each_row);
                assert!(!s.has_when_condition);
                assert!(!s.has_column_list);
            }
            _ => panic!("expected CreateTrigger, got {stmt:?}"),
        }
    }

    #[test]
    fn create_trigger_schema_qualified_function_name_is_preserved() {
        let stmt = parse_prepared_statement(
            "CREATE TRIGGER t BEFORE INSERT ON items FOR EACH ROW EXECUTE FUNCTION analytics.trg_fn()",
        )
        .expect("CREATE TRIGGER should parse");
        match stmt {
            crate::Statement::CreateTrigger(ref s) => {
                assert_eq!(s.function_name, "analytics.trg_fn");
            }
            _ => panic!("expected CreateTrigger, got {stmt:?}"),
        }
    }

    #[test]
    fn create_constraint_trigger_parses_as_create_trigger() {
        let stmt = parse_prepared_statement(
            "CREATE CONSTRAINT TRIGGER t AFTER INSERT ON items FOR EACH ROW EXECUTE FUNCTION trg_fn()",
        )
        .expect("CREATE CONSTRAINT TRIGGER should parse");
        match stmt {
            crate::Statement::CreateTrigger(ref s) => {
                assert_eq!(s.name, "t");
                assert_eq!(s.timing, crate::TriggerTiming::After);
                assert_eq!(s.event, crate::TriggerEvent::Insert);
                assert!(s.for_each_row);
            }
            _ => panic!("expected CreateTrigger, got {stmt:?}"),
        }
    }

    #[test]
    fn create_trigger_truncate_is_rejected() {
        let err = parse_prepared_statement(
            "CREATE TRIGGER t BEFORE TRUNCATE ON items FOR EACH ROW EXECUTE FUNCTION trg_fn()",
        )
        .expect_err("TRUNCATE trigger must fail");
        assert!(
            format!("{err}").contains("TRUNCATE triggers are not supported"),
            "unexpected error: {err}"
        );
    }
}
