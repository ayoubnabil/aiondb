use aiondb_core::{DataType, DbError, DbResult, SqlState};

use crate::{keywords::Keyword, tokens::TokenKind, Parser};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IntervalField {
    Year,
    Month,
    Day,
    Hour,
    Minute,
    Second,
}

impl IntervalField {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Year => "year",
            Self::Month => "month",
            Self::Day => "day",
            Self::Hour => "hour",
            Self::Minute => "minute",
            Self::Second => "second",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct IntervalFieldSpec {
    pub start: IntervalField,
    pub end: IntervalField,
    pub second_precision: Option<u32>,
}

impl Parser {
    pub(crate) fn parse_data_type_with_pg_type_name_hint(
        &mut self,
    ) -> DbResult<(
        DataType,
        Option<String>,
        Option<u32>,
        Option<IntervalFieldSpec>,
        Option<u32>,
        Option<u32>,
    )> {
        let type_start = self.index;
        let data_type = self.parse_data_type()?;
        let tokens = &self.tokens[type_start..self.index];
        let hint = infer_pg_type_name_hint(tokens, &data_type);
        let interval_precision = infer_interval_precision_hint(tokens, &data_type);
        let interval_fields = infer_interval_field_spec_hint(tokens, &data_type);
        let temporal_precision = infer_temporal_precision_hint(tokens, &data_type);
        let char_length = infer_char_length_hint(tokens);
        Ok((
            data_type,
            hint,
            interval_precision,
            interval_fields,
            temporal_precision,
            char_length,
        ))
    }

    pub(crate) fn parse_data_type(&mut self) -> DbResult<DataType> {
        // Handle SETOF prefix: strip it and parse the inner type
        if self.consume_keyword(Keyword::Setof).is_some() {
            return self.parse_data_type();
        }

        let mut base_type = match self.current().kind {
            TokenKind::Keyword(Keyword::Int | Keyword::Int4 | Keyword::Integer) => {
                self.advance();
                DataType::Int
            }
            TokenKind::Keyword(Keyword::BigInt | Keyword::Int8) => {
                self.advance();
                DataType::BigInt
            }
            TokenKind::Keyword(Keyword::SmallInt | Keyword::Int2) => {
                self.advance();
                // AionDB maps SMALLINT to Int (no separate SmallInt type)
                DataType::Int
            }
            TokenKind::Keyword(Keyword::Serial) => {
                self.advance();
                // SERIAL is syntactic sugar - treated as INT in type parsing.
                // The DDL layer handles auto-sequence creation separately.
                DataType::Int
            }
            TokenKind::Keyword(Keyword::BigSerial) => {
                self.advance();
                DataType::BigInt
            }
            TokenKind::Keyword(Keyword::Real | Keyword::Float4) => {
                self.advance();
                DataType::Real
            }
            TokenKind::Keyword(Keyword::Float8) => {
                self.advance();
                DataType::Double
            }
            TokenKind::Keyword(Keyword::Float) => {
                let float_token_span = self.current().span;
                self.advance();
                // FLOAT without precision → DOUBLE, FLOAT(n<=24) → REAL,
                // FLOAT(25..=53) → DOUBLE. PG rejects n<1 and n>53.
                if self.consume_kind(&TokenKind::LParen) {
                    let n = match self.current().kind {
                        TokenKind::Integer(v) => {
                            self.advance();
                            v
                        }
                        _ => 53,
                    };
                    self.expect_token(&TokenKind::RParen)?;
                    if !(1..=53).contains(&n) {
                        return self.syntax_error(
                            float_token_span,
                            "precision for type float must be between 1 and 53 bits",
                        );
                    }
                    if n <= 24 {
                        DataType::Real
                    } else {
                        DataType::Double
                    }
                } else {
                    DataType::Double
                }
            }
            TokenKind::Keyword(Keyword::Double) => {
                self.advance();
                // Consume optional PRECISION
                self.consume_keyword(Keyword::Precision);
                DataType::Double
            }
            TokenKind::Keyword(Keyword::Numeric | Keyword::Decimal) => {
                self.advance();
                // Optionally consume (precision) or (precision, scale)
                if self.consume_kind(&TokenKind::LParen) {
                    // consume precision
                    if matches!(self.current().kind, TokenKind::Integer(_)) {
                        self.advance();
                    }
                    // consume optional scale (may be negative, e.g. numeric(3, -6))
                    if self.consume_kind(&TokenKind::Comma) {
                        let _ = self.consume_kind(&TokenKind::Minus); // optional negative sign
                        if matches!(self.current().kind, TokenKind::Integer(_)) {
                            self.advance();
                        }
                    }
                    self.expect_token(&TokenKind::RParen)?;
                }
                DataType::Numeric
            }
            TokenKind::Keyword(Keyword::Text) => {
                self.advance();
                DataType::Text
            }
            TokenKind::Keyword(Keyword::Varchar) => {
                self.advance();
                // VARCHAR or VARCHAR(n) → Text
                if self.consume_kind(&TokenKind::LParen) {
                    if matches!(self.current().kind, TokenKind::Integer(_)) {
                        self.advance();
                    }
                    self.expect_token(&TokenKind::RParen)?;
                }
                DataType::Text
            }
            TokenKind::Keyword(Keyword::Char) => {
                self.advance();
                // CHAR or CHAR(n) → Text
                if self.consume_kind(&TokenKind::LParen) {
                    if matches!(self.current().kind, TokenKind::Integer(_)) {
                        self.advance();
                    }
                    self.expect_token(&TokenKind::RParen)?;
                }
                DataType::Text
            }
            TokenKind::Keyword(Keyword::Character) => {
                self.advance();
                // CHARACTER VARYING(n) = VARCHAR(n), CHARACTER(n) = CHAR(n)
                if self.consume_keyword(Keyword::Varying).is_some() {
                    // CHARACTER VARYING or CHARACTER VARYING(n)
                    if self.consume_kind(&TokenKind::LParen) {
                        if matches!(self.current().kind, TokenKind::Integer(_)) {
                            self.advance();
                        }
                        self.expect_token(&TokenKind::RParen)?;
                    }
                } else if self.consume_kind(&TokenKind::LParen) {
                    if matches!(self.current().kind, TokenKind::Integer(_)) {
                        self.advance();
                    }
                    self.expect_token(&TokenKind::RParen)?;
                }
                DataType::Text
            }
            TokenKind::Keyword(Keyword::Boolean | Keyword::Bool) => {
                self.advance();
                DataType::Boolean
            }
            TokenKind::Keyword(Keyword::Blob | Keyword::Bytea) => {
                self.advance();
                DataType::Blob
            }
            TokenKind::Keyword(Keyword::Timestamp) => {
                self.advance();
                // Optional precision: TIMESTAMP(n)
                self.skip_type_precision();
                // TIMESTAMP WITH TIME ZONE → TimestampTz
                // TIMESTAMP WITHOUT TIME ZONE → Timestamp
                // TIMESTAMP → Timestamp
                self.parse_optional_timezone(DataType::Timestamp, DataType::TimestampTz)
            }
            TokenKind::Keyword(Keyword::Timestamptz) => {
                self.advance();
                // Optional precision: TIMESTAMPTZ(n)
                self.skip_type_precision();
                DataType::TimestampTz
            }
            TokenKind::Keyword(Keyword::Time) => {
                self.advance();
                // Optional precision: TIME(n)
                self.skip_type_precision();
                // TIME WITH TIME ZONE → TimeTz
                // TIME WITHOUT TIME ZONE → Time
                // TIME → Time
                self.parse_optional_timezone(DataType::Time, DataType::TimeTz)
            }
            TokenKind::Keyword(Keyword::Timetz) => {
                self.advance();
                // Optional precision: TIMETZ(n)
                self.skip_type_precision();
                DataType::TimeTz
            }
            TokenKind::Keyword(Keyword::Date) => {
                self.advance();
                DataType::Date
            }
            TokenKind::Keyword(Keyword::Interval) => {
                self.advance();
                // Optional precision: INTERVAL(n)
                self.skip_type_precision();
                // Skip optional interval fields like YEAR, MONTH, DAY, HOUR,
                // MINUTE, SECOND, YEAR TO MONTH, DAY TO SECOND, etc.
                let _ = self.parse_interval_field_spec();
                DataType::Interval
            }
            TokenKind::Keyword(Keyword::Uuid) => {
                self.advance();
                DataType::Uuid
            }
            TokenKind::Keyword(Keyword::Jsonb) => {
                self.advance();
                DataType::Jsonb
            }
            TokenKind::Keyword(Keyword::Vector) => {
                self.advance();
                if !self.consume_kind(&TokenKind::LParen) {
                    return Ok(DataType::Vector {
                        dims: 0,
                        element_type: aiondb_core::VectorElementType::Float32,
                    });
                }
                let dims = match self.current().kind {
                    TokenKind::Integer(n) if n > 0 => {
                        self.advance();
                        match u32::try_from(n) {
                            Ok(dims) => dims,
                            Err(_) => {
                                return self
                                    .syntax_error_current("VECTOR dimension is out of range");
                            }
                        }
                    }
                    _ => {
                        return self.syntax_error_current(
                            "expected positive integer for VECTOR dimension",
                        );
                    }
                };
                // Optional element type: VECTOR(dims, float16)
                let element_type = if self.consume_kind(&TokenKind::Comma) {
                    match &self.current().kind {
                        TokenKind::Identifier(s) => {
                            let et = aiondb_core::VectorElementType::parse(s).ok_or_else(|| {
                                DbError::parse_error(
                                    SqlState::SyntaxError,
                                    format!("unknown vector element type: '{s}' (expected float32, float16, or uint8)"),
                                )
                            })?;
                            self.advance();
                            et
                        }
                        _ => {
                            return self.syntax_error_current(
                                "expected element type (float32, float16, uint8)",
                            );
                        }
                    }
                } else {
                    aiondb_core::VectorElementType::Float32
                };
                self.expect_token(&TokenKind::RParen)?;
                DataType::Vector { dims, element_type }
            }
            TokenKind::Keyword(Keyword::Bit) => {
                self.advance();
                // BIT VARYING / BIT are represented with textual storage in
                // the current engine compatibility layer.
                let _ = self.consume_keyword(Keyword::Varying);
                if self.consume_kind(&TokenKind::LParen) {
                    if matches!(self.current().kind, TokenKind::Integer(_)) {
                        self.advance();
                    }
                    self.expect_token(&TokenKind::RParen)?;
                }
                DataType::Text
            }
            // Handle PG-specific types that aren't keywords - matched as
            // case-insensitive identifiers and mapped to the closest internal type.
            TokenKind::Identifier(_) => self.parse_identifier_type()?,
            // Unreserved keywords used as type names (user-defined types, domain
            // types, PG-specific types that collide with our keyword list).
            // Treat them like unknown identifiers → Text fallback.
            TokenKind::Keyword(kw) if kw.is_unreserved() => {
                self.advance();
                // Consume optional schema-qualified suffix: kw.name
                if self.consume_kind(&TokenKind::Dot) {
                    // Skip the qualified name (identifier or keyword)
                    match &self.current().kind {
                        TokenKind::Identifier(_) | TokenKind::Keyword(_) => {
                            self.advance();
                        }
                        _ => {}
                    }
                }
                // Consume optional type modifiers like (10) or (10,2)
                self.skip_type_modifiers();
                DataType::Text
            }
            _ => return self.syntax_error_current("expected data type"),
        };
        // Skip optional %TYPE suffix (PG column reference type)
        // Only consume % if it is actually followed by the TYPE keyword;
        if self.current().kind == TokenKind::Percent
            && self.index + 1 < self.tokens.len()
            && matches!(
                self.tokens[self.index + 1].kind,
                TokenKind::Keyword(Keyword::Type)
            )
        {
            self.advance(); // consume %
            self.advance(); // consume TYPE
        }
        // Check for ARRAY keyword suffix: integer ARRAY[4] → Array(Int)
        if self.current().kind == TokenKind::Keyword(Keyword::Array) {
            self.advance(); // consume ARRAY
                            // Optional [n] dimension
            if self.current().kind == TokenKind::LBracket {
                self.advance(); // consume [
                if matches!(self.current().kind, TokenKind::Integer(_)) {
                    self.advance();
                }
                if self.current().kind == TokenKind::RBracket {
                    self.advance(); // consume ]
                }
            }
            base_type = DataType::Array(Box::new(base_type));
        }
        // Check for trailing []...[] or [n]...[] to make it an array type (multi-dimensional)
        while self.current().kind == TokenKind::LBracket {
            self.advance(); // consume [
                            // Skip optional dimension number: [3], [10], etc.
            if matches!(self.current().kind, TokenKind::Integer(_)) {
                self.advance();
            }
            if self.current().kind == TokenKind::RBracket {
                self.advance(); // consume ]
            }
            base_type = DataType::Array(Box::new(base_type));
        }
        Ok(base_type)
    }

    /// Skip optional type precision specifier `(n)` after TIME/TIMESTAMP types.
    fn skip_type_precision(&mut self) {
        if self.consume_kind(&TokenKind::LParen) {
            if matches!(self.current().kind, TokenKind::Integer(_)) {
                self.advance();
            }
            let _ = self.consume_kind(&TokenKind::RParen);
        }
    }

    /// Parse optional `WITH TIME ZONE` / `WITHOUT TIME ZONE` suffix after
    /// TIMESTAMP or TIME.  Returns `without_tz` if no suffix or WITHOUT,
    /// `with_tz` if WITH TIME ZONE.
    fn parse_optional_timezone(&mut self, without_tz: DataType, with_tz: DataType) -> DataType {
        if self.consume_keyword(Keyword::With).is_some() {
            // WITH TIME ZONE
            let _ = self.consume_keyword(Keyword::Time);
            let _ = self.consume_keyword(Keyword::Zone);
            with_tz
        } else if self.consume_keyword(Keyword::Without).is_some() {
            // WITHOUT TIME ZONE
            let _ = self.consume_keyword(Keyword::Time);
            let _ = self.consume_keyword(Keyword::Zone);
            without_tz
        } else {
            without_tz
        }
    }

    /// Parse optional interval qualifier fields like YEAR, MONTH, DAY TO
    /// SECOND, etc. These can appear after INTERVAL in type declarations or
    /// after an interval string literal in `PostgreSQL`'s typed-literal syntax.
    pub(crate) fn parse_interval_field_spec(&mut self) -> Option<IntervalFieldSpec> {
        let start = interval_field_from_token_kind(&self.current().kind)?;
        self.advance();
        let mut end = start;
        let mut second_precision = if start == IntervalField::Second {
            self.parse_optional_precision_u32()
        } else {
            None
        };
        if self.consume_keyword(Keyword::To).is_some() {
            let parsed_end = interval_field_from_token_kind(&self.current().kind)?;
            self.advance();
            end = parsed_end;
            if end == IntervalField::Second {
                second_precision = self.parse_optional_precision_u32();
            }
        }
        Some(IntervalFieldSpec {
            start,
            end,
            second_precision,
        })
    }

    fn parse_optional_precision_u32(&mut self) -> Option<u32> {
        let current_index = self.index;
        if !self.consume_kind(&TokenKind::LParen) {
            return None;
        }
        let precision = match self.current().kind {
            TokenKind::Integer(value) if value >= 0 => match u32::try_from(value) {
                Ok(precision) => precision,
                Err(_) => {
                    self.index = current_index;
                    return None;
                }
            },
            _ => {
                self.index = current_index;
                return None;
            }
        };
        self.advance();
        if !self.consume_kind(&TokenKind::RParen) {
            self.index = current_index;
            return None;
        }
        Some(precision)
    }

    /// Consume optional parenthesized type modifiers like `(10)` or `(10,2)`.
    /// Used after consuming an unknown / user-defined type name so the rest
    /// of the SQL can parse cleanly.
    fn skip_type_modifiers(&mut self) {
        if self.consume_kind(&TokenKind::LParen) {
            let mut depth = 1u32;
            while depth > 0 && !self.is_eof() {
                match self.current().kind {
                    TokenKind::LParen => {
                        depth += 1;
                        self.advance();
                    }
                    TokenKind::RParen => {
                        depth -= 1;
                        self.advance();
                    }
                    _ => {
                        self.advance();
                    }
                }
            }
        }
    }

    /// Parse a PG-specific data type name that appears as a plain identifier
    /// (not a keyword).  Each recognised name is mapped to the closest
    /// internal `DataType` variant.
    fn parse_identifier_type(&mut self) -> DbResult<DataType> {
        // Clone the identifier text so we can freely mutate `self` afterwards.
        let ident = match &self.current().kind {
            TokenKind::Identifier(s) => s.to_ascii_uppercase(),
            _ => return self.syntax_error_current("expected data type"),
        };

        // Determine the mapped type, or error out for unknown identifiers.
        // BIT and VARBIT need special handling for optional (n).
        let dt = match ident.as_str() {
            // OID - 32-bit object identifier
            "OID" => {
                self.advance_span();
                DataType::Int
            }
            // NAME - 64-byte fixed-length string
            "NAME" => {
                self.advance_span();
                DataType::Text
            }
            // MONEY - currency type
            "MONEY" => {
                self.advance_span();
                DataType::Money
            }
            // Network address types
            "INET" | "CIDR" => {
                self.advance_span();
                DataType::Text
            }
            "MACADDR" => {
                self.advance_span();
                DataType::MacAddr
            }
            "MACADDR8" => {
                self.advance_span();
                DataType::MacAddr8
            }
            // Geometric types
            "POINT" | "BOX" | "LINE" | "LSEG" | "POLYGON" | "CIRCLE" | "PATH" => {
                self.advance_span();
                DataType::Text
            }
            // JSON (non-binary) - mapped to our single JSON type
            "JSON" => {
                self.advance_span();
                DataType::Jsonb
            }
            // XML
            "XML" => {
                self.advance_span();
                DataType::Text
            }
            // BIT with optional (n) length (identifier fallback for unrecognised case)
            "BIT" => {
                self.advance();
                // Check for BIT VARYING
                if self.consume_keyword(Keyword::Varying).is_some() {
                    if self.consume_kind(&TokenKind::LParen) {
                        if matches!(self.current().kind, TokenKind::Integer(_)) {
                            self.advance();
                        }
                        self.expect_token(&TokenKind::RParen)?;
                    }
                    DataType::Text
                } else {
                    if self.consume_kind(&TokenKind::LParen) {
                        if matches!(self.current().kind, TokenKind::Integer(_)) {
                            self.advance();
                        }
                        self.expect_token(&TokenKind::RParen)?;
                    }
                    DataType::Text
                }
            }
            // VARBIT with optional (n) length
            "VARBIT" => {
                self.advance();
                if self.consume_kind(&TokenKind::LParen) {
                    if matches!(self.current().kind, TokenKind::Integer(_)) {
                        self.advance();
                    }
                    self.expect_token(&TokenKind::RParen)?;
                }
                DataType::Text
            }
            // Registry / OID-alias types
            "REGCLASS" | "REGTYPE" | "REGPROC" | "REGPROCEDURE" | "REGOPER" | "REGOPERATOR"
            | "REGNAMESPACE" | "REGROLE" | "REGCONFIG" | "REGDICTIONARY" => {
                self.advance();
                DataType::Int
            }
            // Full-text search types
            "HALFVEC" => {
                self.parse_pgvector_named_type(aiondb_core::VectorElementType::Float16, "HALFVEC")?
            }
            "SPARSEVEC" => self
                .parse_pgvector_named_type(aiondb_core::VectorElementType::Float32, "SPARSEVEC")?,
            "TSVECTOR" | "TSQUERY" => {
                self.advance_span();
                DataType::Text
            }
            // Pseudo-types
            "RECORD"
            | "VOID"
            | "ANYELEMENT"
            | "ANYARRAY"
            | "ANYNONARRAY"
            | "ANYENUM"
            | "ANYRANGE"
            | "ANYMULTIRANGE"
            | "ANYCOMPATIBLE"
            | "ANYCOMPATIBLEARRAY"
            | "ANYCOMPATIBLERANGE"
            | "ANYCOMPATIBLEMULTIRANGE"
            | "ANYCOMPATIBLENONARRAY"
            | "CSTRING"
            | "INTERNAL"
            | "EVENT_TRIGGER"
            | "LANGUAGE_HANDLER"
            | "FDW_HANDLER" => {
                self.advance();
                DataType::Text
            }
            // SMALLSERIAL - auto-increment small int
            "SMALLSERIAL" => {
                self.advance();
                DataType::Int
            }
            // Range types
            "INT4RANGE" | "INT8RANGE" | "NUMRANGE" | "TSRANGE" | "TSTZRANGE" | "DATERANGE"
            | "INT4MULTIRANGE" | "INT8MULTIRANGE" | "NUMMULTIRANGE" | "TSMULTIRANGE"
            | "TSTZMULTIRANGE" | "DATEMULTIRANGE" => {
                self.advance();
                DataType::Text
            }
            // System column types
            "XID" | "CID" => {
                self.advance();
                DataType::Int
            }
            "TID" => {
                self.advance();
                DataType::Tid
            }
            // pg_lsn (WAL position)
            "PG_LSN" => {
                self.advance();
                DataType::PgLsn
            }
            // SERIAL8 - alias for BIGSERIAL
            "SERIAL8" => {
                self.advance();
                DataType::BigInt
            }
            // SERIAL4 - alias for SERIAL
            "SERIAL4" => {
                self.advance();
                DataType::Int
            }
            // SERIAL2 - alias for SMALLSERIAL
            "SERIAL2" => {
                self.advance();
                DataType::Int
            }
            // INT2VECTOR, OIDVECTOR - array-like types
            "OIDVECTOR" | "INT2VECTOR" => {
                self.advance();
                DataType::Text
            }
            // REFCURSOR - cursor reference type
            "REFCURSOR" => {
                self.advance();
                DataType::Text
            }
            // PG internal array type names: _int2, _int4, _int8, _float4,
            // _float8, _text, _name, _bool, _numeric, _varchar, _char, _oid
            "_INT2" | "_INT4" => {
                self.advance();
                DataType::Array(Box::new(DataType::Int))
            }
            "_INT8" => {
                self.advance();
                DataType::Array(Box::new(DataType::BigInt))
            }
            "_FLOAT4" => {
                self.advance();
                DataType::Array(Box::new(DataType::Real))
            }
            "_FLOAT8" => {
                self.advance();
                DataType::Array(Box::new(DataType::Double))
            }
            "_TEXT" | "_NAME" | "_VARCHAR" | "_CHAR" | "_BPCHAR" => {
                self.advance();
                DataType::Array(Box::new(DataType::Text))
            }
            "_TID" => {
                self.advance();
                DataType::Array(Box::new(DataType::Tid))
            }
            "_BOOL" => {
                self.advance();
                DataType::Array(Box::new(DataType::Boolean))
            }
            "_NUMERIC" => {
                self.advance();
                DataType::Array(Box::new(DataType::Numeric))
            }
            "_OID" | "_REGCLASS" | "_REGTYPE" => {
                self.advance();
                DataType::Array(Box::new(DataType::Int))
            }
            // Fallback: treat any unrecognised identifier as a user-defined
            // type and map it to Text.  This allows custom / domain types,
            // composite types, and PG types we haven't explicitly listed.
            _ => {
                let mut identifier_name = match &self.current().kind {
                    TokenKind::Identifier(name) => Some(name.clone()),
                    TokenKind::Keyword(keyword) => Some(keyword.name().to_owned()),
                    _ => None,
                };
                self.advance();
                // Consume optional schema-qualified suffix: ident.name
                if self.consume_kind(&TokenKind::Dot) {
                    match &self.current().kind {
                        TokenKind::Identifier(name) => {
                            identifier_name = Some(name.clone());
                            self.advance();
                        }
                        TokenKind::Keyword(keyword) => {
                            identifier_name = Some(keyword.name().to_owned());
                            self.advance();
                        }
                        _ => {}
                    }
                }
                // Consume optional type modifiers like (10) or (10,2)
                self.skip_type_modifiers();
                identifier_name
                    .as_deref()
                    .map_or(DataType::Text, |name| self.name_to_data_type(name))
            }
        };
        Ok(dt)
    }

    /// Map a type name string to a `DataType` without consuming tokens.
    /// Used when we've already consumed the identifier and need to interpret it as a type.
    pub(crate) fn name_to_data_type(&self, name: &str) -> DataType {
        // Use the same stack-buffer uppercase trick from keyword lookup to
        // avoid a heap allocation for the common ASCII case.
        let upper: String;
        let key = if name.len() <= 32 && name.is_ascii() {
            let mut buf = [0u8; 32];
            for (i, &b) in name.as_bytes().iter().enumerate() {
                buf[i] = b.to_ascii_uppercase();
            }
            // Safe: input is ASCII so uppercased result is valid UTF-8.
            let Ok(s) = std::str::from_utf8(&buf[..name.len()]) else {
                return DataType::Text;
            };
            // We need a &str that lives long enough; copy into `upper`.
            upper = s.to_owned();
            upper.as_str()
        } else {
            upper = name.to_ascii_uppercase();
            upper.as_str()
        };
        match key {
            "INT" | "INT4" | "INTEGER" | "SERIAL" | "OID" | "SMALLINT" | "INT2" | "SMALLSERIAL"
            | "SERIAL2" | "SERIAL4" | "REGCLASS" | "REGTYPE" | "REGPROC" | "REGPROCEDURE"
            | "REGOPER" | "REGOPERATOR" | "REGNAMESPACE" | "REGROLE" | "REGCONFIG"
            | "REGDICTIONARY" | "XID" | "CID" => DataType::Int,
            "TID" => DataType::Tid,
            "BIGINT" | "INT8" | "BIGSERIAL" | "SERIAL8" => DataType::BigInt,
            "REAL" | "FLOAT4" => DataType::Real,
            "DOUBLE" | "FLOAT" | "FLOAT8" => DataType::Double,
            "NUMERIC" | "DECIMAL" => DataType::Numeric,
            "MONEY" => DataType::Money,
            "TEXT" | "VARCHAR" | "NAME" | "CHAR" | "CHARACTER" => DataType::Text,
            "BOOLEAN" | "BOOL" => DataType::Boolean,
            "BYTEA" | "BLOB" => DataType::Blob,
            "TIMESTAMP" => DataType::Timestamp,
            "TIMESTAMPTZ" => DataType::TimestampTz,
            "DATE" => DataType::Date,
            "TIME" => DataType::Time,
            "TIMETZ" => DataType::TimeTz,
            "INTERVAL" => DataType::Interval,
            "UUID" => DataType::Uuid,
            "JSONB" | "JSON" => DataType::Jsonb,
            "MACADDR" => DataType::MacAddr,
            "MACADDR8" => DataType::MacAddr8,
            "VECTOR" => DataType::Vector {
                dims: 0,
                element_type: aiondb_core::VectorElementType::Float32,
            },
            "HALFVEC" => DataType::Vector {
                dims: 0,
                element_type: aiondb_core::VectorElementType::Float16,
            },
            "SPARSEVEC" => DataType::Vector {
                dims: 0,
                element_type: aiondb_core::VectorElementType::Float32,
            },
            _ => DataType::Text,
        }
    }

    fn parse_pgvector_named_type(
        &mut self,
        element_type: aiondb_core::VectorElementType,
        type_name: &str,
    ) -> DbResult<DataType> {
        self.advance();
        if !self.consume_kind(&TokenKind::LParen) {
            return Ok(DataType::Vector {
                dims: 0,
                element_type,
            });
        }
        let dims = match self.current().kind {
            TokenKind::Integer(n) if n > 0 => {
                self.advance();
                u32::try_from(n).map_err(|_| {
                    DbError::parse_error(
                        SqlState::SyntaxError,
                        format!("{type_name} dimension is out of range"),
                    )
                })?
            }
            _ => {
                return self.syntax_error_current(format!(
                    "expected positive integer for {type_name} dimension"
                ));
            }
        };
        self.expect_token(&TokenKind::RParen)?;
        Ok(DataType::Vector { dims, element_type })
    }
}

fn infer_pg_type_name_hint(
    tokens: &[crate::tokens::Token],
    data_type: &DataType,
) -> Option<String> {
    if let Some(numeric_hint) = infer_numeric_type_name_hint(tokens) {
        return Some(numeric_hint);
    }

    let qualified_identifier_suffix = match (
        tokens.first().map(|token| &token.kind),
        tokens.get(1).map(|token| &token.kind),
        tokens.get(2).map(|token| &token.kind),
    ) {
        (
            Some(TokenKind::Identifier(_) | TokenKind::Keyword(_)),
            Some(TokenKind::Dot),
            Some(TokenKind::Identifier(name)),
        ) => Some(name.as_str()),
        (
            Some(TokenKind::Identifier(_) | TokenKind::Keyword(_)),
            Some(TokenKind::Dot),
            Some(TokenKind::Keyword(keyword)),
        ) => Some(keyword.name()),
        _ => None,
    };

    let base_name = match qualified_identifier_suffix {
        Some(name) if name.eq_ignore_ascii_case("regclass") => Some("regclass"),
        Some(name) if name.eq_ignore_ascii_case("regtype") => Some("regtype"),
        Some(name) if name.eq_ignore_ascii_case("regproc") => Some("regproc"),
        Some(name) if name.eq_ignore_ascii_case("regprocedure") => Some("regprocedure"),
        Some(name) if name.eq_ignore_ascii_case("regoper") => Some("regoper"),
        Some(name) if name.eq_ignore_ascii_case("regoperator") => Some("regoperator"),
        Some(name) if name.eq_ignore_ascii_case("regnamespace") => Some("regnamespace"),
        Some(name) if name.eq_ignore_ascii_case("regrole") => Some("regrole"),
        Some(name) if name.eq_ignore_ascii_case("regconfig") => Some("regconfig"),
        Some(name) if name.eq_ignore_ascii_case("regdictionary") => Some("regdictionary"),
        Some(name) if name.eq_ignore_ascii_case("int2") => Some("int2"),
        Some(name) if name.eq_ignore_ascii_case("money") => Some("money"),
        Some(name) if name.eq_ignore_ascii_case("oid") => Some("oid"),
        Some(name) if name.eq_ignore_ascii_case("xid") => Some("xid"),
        Some(name) if matches!(data_type, DataType::Text) => Some(name),
        _ => match tokens.first().map(|token| &token.kind) {
            Some(TokenKind::Keyword(Keyword::Varchar)) => Some("character varying"),
            Some(TokenKind::Keyword(Keyword::Char)) => Some("character"),
            Some(TokenKind::Keyword(Keyword::Character)) => {
                if matches!(
                    tokens.get(1).map(|token| &token.kind),
                    Some(TokenKind::Keyword(Keyword::Varying))
                ) {
                    Some("character varying")
                } else {
                    Some("character")
                }
            }
            Some(TokenKind::Identifier(ident)) if ident == "char" => Some("char"),
            Some(TokenKind::Identifier(ident)) if ident.eq_ignore_ascii_case("regclass") => {
                Some("regclass")
            }
            Some(TokenKind::Identifier(ident)) if ident.eq_ignore_ascii_case("regtype") => {
                Some("regtype")
            }
            Some(TokenKind::Identifier(ident)) if ident.eq_ignore_ascii_case("regproc") => {
                Some("regproc")
            }
            Some(TokenKind::Identifier(ident)) if ident.eq_ignore_ascii_case("regprocedure") => {
                Some("regprocedure")
            }
            Some(TokenKind::Identifier(ident)) if ident.eq_ignore_ascii_case("regoper") => {
                Some("regoper")
            }
            Some(TokenKind::Identifier(ident)) if ident.eq_ignore_ascii_case("regoperator") => {
                Some("regoperator")
            }
            Some(TokenKind::Identifier(ident)) if ident.eq_ignore_ascii_case("regnamespace") => {
                Some("regnamespace")
            }
            Some(TokenKind::Identifier(ident)) if ident.eq_ignore_ascii_case("regrole") => {
                Some("regrole")
            }
            Some(TokenKind::Identifier(ident)) if ident.eq_ignore_ascii_case("regconfig") => {
                Some("regconfig")
            }
            Some(TokenKind::Identifier(ident)) if ident.eq_ignore_ascii_case("regdictionary") => {
                Some("regdictionary")
            }
            Some(TokenKind::Keyword(Keyword::SmallInt)) => Some("smallint"),
            Some(TokenKind::Keyword(Keyword::Int2)) => Some("int2"),
            Some(TokenKind::Identifier(ident)) if ident.eq_ignore_ascii_case("int2") => {
                Some("int2")
            }
            Some(TokenKind::Identifier(ident)) if ident.eq_ignore_ascii_case("money") => {
                Some("money")
            }
            Some(TokenKind::Identifier(ident)) if ident.eq_ignore_ascii_case("oid") => Some("oid"),
            Some(TokenKind::Identifier(ident)) if ident.eq_ignore_ascii_case("xid") => Some("xid"),
            Some(TokenKind::Identifier(ident)) if ident.eq_ignore_ascii_case("halfvec") => {
                Some("halfvec")
            }
            Some(TokenKind::Identifier(ident)) if ident.eq_ignore_ascii_case("sparsevec") => {
                Some("sparsevec")
            }
            Some(TokenKind::Keyword(keyword)) if keyword.name().eq_ignore_ascii_case("money") => {
                Some("money")
            }
            Some(TokenKind::Identifier(ident)) if matches!(data_type, DataType::Text) => {
                Some(ident.as_str())
            }
            _ => None,
        },
    }?;

    let array_depth = data_type_array_depth(data_type);
    let mut name = base_name.to_owned();
    for _ in 0..array_depth {
        name.push_str("[]");
    }
    Some(name)
}

pub(crate) fn infer_numeric_type_name_hint(tokens: &[crate::tokens::Token]) -> Option<String> {
    let is_numeric = match tokens.first().map(|token| &token.kind) {
        Some(TokenKind::Keyword(Keyword::Numeric | Keyword::Decimal)) => true,
        Some(TokenKind::Identifier(name)) => {
            name.eq_ignore_ascii_case("numeric") || name.eq_ignore_ascii_case("decimal")
        }
        _ => false,
    };
    if !is_numeric {
        return None;
    }

    let mut hint = "numeric".to_owned();
    if tokens.len() == 4 {
        if let (
            Some(TokenKind::LParen),
            Some(TokenKind::Integer(precision)),
            Some(TokenKind::RParen),
        ) = (
            tokens.get(1).map(|token| &token.kind),
            tokens.get(2).map(|token| &token.kind),
            tokens.get(3).map(|token| &token.kind),
        ) {
            hint.push('(');
            hint.push_str(&precision.to_string());
            hint.push(')');
        }
    } else if tokens.len() == 6 {
        if let (
            Some(TokenKind::LParen),
            Some(TokenKind::Integer(precision)),
            Some(TokenKind::Comma),
            Some(TokenKind::Integer(scale)),
            Some(TokenKind::RParen),
        ) = (
            tokens.get(1).map(|token| &token.kind),
            tokens.get(2).map(|token| &token.kind),
            tokens.get(3).map(|token| &token.kind),
            tokens.get(4).map(|token| &token.kind),
            tokens.get(5).map(|token| &token.kind),
        ) {
            hint.push('(');
            hint.push_str(&precision.to_string());
            hint.push(',');
            hint.push_str(&scale.to_string());
            hint.push(')');
        }
    }
    Some(hint)
}

fn infer_interval_precision_hint(
    tokens: &[crate::tokens::Token],
    data_type: &DataType,
) -> Option<u32> {
    if !matches!(data_type, DataType::Interval) {
        return None;
    }
    if !matches!(
        tokens.first().map(|token| &token.kind),
        Some(TokenKind::Keyword(Keyword::Interval))
    ) {
        return None;
    }

    for window in tokens.windows(3) {
        let [crate::tokens::Token {
            kind: TokenKind::LParen,
            ..
        }, crate::tokens::Token {
            kind: TokenKind::Integer(precision),
            ..
        }, crate::tokens::Token {
            kind: TokenKind::RParen,
            ..
        }] = window
        else {
            continue;
        };
        if *precision >= 0 {
            if let Ok(precision) = u32::try_from(*precision) {
                return Some(precision);
            }
        }
    }

    None
}

fn infer_temporal_precision_hint(
    tokens: &[crate::tokens::Token],
    data_type: &DataType,
) -> Option<u32> {
    if !matches!(
        data_type,
        DataType::Time | DataType::TimeTz | DataType::Timestamp | DataType::TimestampTz
    ) {
        return None;
    }

    let starts_with_temporal_type = matches!(
        tokens.first().map(|token| &token.kind),
        Some(TokenKind::Keyword(
            Keyword::Time | Keyword::Timetz | Keyword::Timestamp | Keyword::Timestamptz
        ))
    );
    if !starts_with_temporal_type {
        return None;
    }

    for window in tokens.windows(3) {
        let [crate::tokens::Token {
            kind: TokenKind::LParen,
            ..
        }, crate::tokens::Token {
            kind: TokenKind::Integer(precision),
            ..
        }, crate::tokens::Token {
            kind: TokenKind::RParen,
            ..
        }] = window
        else {
            continue;
        };
        if *precision >= 0 {
            if let Ok(precision) = u32::try_from(*precision) {
                return Some(precision);
            }
        }
    }

    None
}

fn infer_interval_field_spec_hint(
    tokens: &[crate::tokens::Token],
    data_type: &DataType,
) -> Option<IntervalFieldSpec> {
    if !matches!(data_type, DataType::Interval) {
        return None;
    }
    if !matches!(
        tokens.first().map(|token| &token.kind),
        Some(TokenKind::Keyword(Keyword::Interval))
    ) {
        return None;
    }

    let mut index = 1usize;
    if matches!(
        tokens.get(index).map(|token| &token.kind),
        Some(TokenKind::LParen)
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
            Some(TokenKind::RParen)
        ) {
            index += 1;
        }
    }

    let start = interval_field_from_token_kind(&tokens.get(index)?.kind)?;
    index += 1;
    let mut end = start;
    let mut second_precision = None;
    if start == IntervalField::Second {
        second_precision = infer_interval_field_precision(tokens, index);
        if second_precision.is_some() {
            index += 3;
        }
    }
    if matches!(
        tokens.get(index).map(|token| &token.kind),
        Some(TokenKind::Keyword(Keyword::To))
    ) {
        index += 1;
        end = interval_field_from_token_kind(&tokens.get(index)?.kind)?;
        index += 1;
        if end == IntervalField::Second {
            second_precision = infer_interval_field_precision(tokens, index);
        }
    }

    Some(IntervalFieldSpec {
        start,
        end,
        second_precision,
    })
}

fn infer_interval_field_precision(tokens: &[crate::tokens::Token], index: usize) -> Option<u32> {
    let [crate::tokens::Token {
        kind: TokenKind::LParen,
        ..
    }, crate::tokens::Token {
        kind: TokenKind::Integer(precision),
        ..
    }, crate::tokens::Token {
        kind: TokenKind::RParen,
        ..
    }] = tokens.get(index..index + 3)?
    else {
        return None;
    };
    (*precision >= 0)
        .then_some(*precision)
        .and_then(|precision| u32::try_from(precision).ok())
}

/// Extract the declared length from a CHAR(n) or CHARACTER(n) type specification.
/// Returns `None` for VARCHAR, CHARACTER VARYING, or types without a length.
fn infer_char_length_hint(tokens: &[crate::tokens::Token]) -> Option<u32> {
    let is_char = match tokens.first().map(|token| &token.kind) {
        Some(TokenKind::Keyword(Keyword::Char)) => true,
        Some(TokenKind::Keyword(Keyword::Character)) => {
            // CHARACTER VARYING is VARCHAR, not CHAR
            !matches!(
                tokens.get(1).map(|token| &token.kind),
                Some(TokenKind::Keyword(Keyword::Varying))
            )
        }
        _ => false,
    };
    if !is_char {
        return None;
    }

    for window in tokens.windows(3) {
        let [crate::tokens::Token {
            kind: TokenKind::LParen,
            ..
        }, crate::tokens::Token {
            kind: TokenKind::Integer(length),
            ..
        }, crate::tokens::Token {
            kind: TokenKind::RParen,
            ..
        }] = window
        else {
            continue;
        };
        if *length > 0 {
            if let Ok(length) = u32::try_from(*length) {
                return Some(length);
            }
        }
    }

    None
}

fn interval_field_from_token_kind(kind: &TokenKind) -> Option<IntervalField> {
    let TokenKind::Identifier(ident) = kind else {
        return None;
    };

    if ident.eq_ignore_ascii_case("YEAR") {
        Some(IntervalField::Year)
    } else if ident.eq_ignore_ascii_case("MONTH") {
        Some(IntervalField::Month)
    } else if ident.eq_ignore_ascii_case("DAY") {
        Some(IntervalField::Day)
    } else if ident.eq_ignore_ascii_case("HOUR") {
        Some(IntervalField::Hour)
    } else if ident.eq_ignore_ascii_case("MINUTE") {
        Some(IntervalField::Minute)
    } else if ident.eq_ignore_ascii_case("SECOND") {
        Some(IntervalField::Second)
    } else {
        None
    }
}

fn data_type_array_depth(data_type: &DataType) -> usize {
    match data_type {
        DataType::Array(inner) => 1 + data_type_array_depth(inner),
        _ => 0,
    }
}
