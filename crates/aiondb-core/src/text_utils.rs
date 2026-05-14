const HEX_ENCODE_CHUNK: usize = 64;

/// Escape a string for safe interpolation inside a SQL single-quoted
/// literal (`'...'`).
///
/// `AionDB` hardcodes `standard_conforming_strings = on`, which means
/// backslashes are always literal in regular string literals - only
/// single quotes need escaping.
///
/// Safety guarantees (each is a simple character match, O(n)):
/// - `'` → `''` : prevents breaking out of the string literal.
/// - `\0` → stripped : prevents null-byte truncation in downstream code.
/// - Backslashes are left as-is because scs=on means they are literal.
///   The engine rejects `SET standard_conforming_strings` to any value
///   other than `on`, so this invariant cannot be broken at runtime.
#[must_use]
pub fn escape_sql_literal(input: &str) -> String {
    // Bulk-copy chunks between trigger bytes (`'`, `\0`) via
    // `push_str` instead of dispatching per char. Both triggers are
    // single-byte ASCII; UTF-8 leading bytes (>= 0x80) cannot collide,
    // so slicing on raw byte indices stays at valid char boundaries.
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len() + 8);
    let mut last = 0usize;
    for (idx, &b) in bytes.iter().enumerate() {
        if b != b'\'' && b != b'\0' {
            continue;
        }
        if idx > last {
            out.push_str(&input[last..idx]);
        }
        if b == b'\'' {
            out.push_str("''");
        }
        last = idx + 1;
    }
    if last < bytes.len() {
        out.push_str(&input[last..]);
    }
    out
}

#[must_use]
pub fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    hex_encode_into(bytes, &mut s);
    s
}

/// Single-pass unescape of a PG-array-quoted element body: each
/// `\X` becomes `X` (so `\"` → `"`, `\\` → `\`). Walks the input
/// previous `text.replace("\\\"", "\"").replace("\\\\", "\\")`
/// chain that built two transient Strings per call.
#[must_use]
pub fn pg_array_unescape_quoted(inner: &str) -> String {
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Lookup table mapping each byte 0x00..=0xFF to its 2-character lower-case
/// hex pair. Used by `hex_encode_into` so each input byte produces a single
/// `[u8; 2]` table read + a 2-byte slice push into the destination, halving
/// the per-byte bounds-check cost relative to the previous two-`push` loop.
/// Mirrors the same trick used by `aiondb-pgwire::format::HEX_PAIRS` and
/// `aiondb-core::network::MAC_HEX_PAIRS`.
const HEX_PAIRS_TABLE: [[u8; 2]; 256] = {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut table = [[0u8; 2]; 256];
    let mut i = 0;
    while i < 256 {
        table[i] = [HEX[i >> 4], HEX[i & 0x0f]];
        i += 1;
    }
    table
};

/// Append the lowercase-hex representation of `bytes` to `out`,
/// reserving capacity in one shot. Lets callers prepend a prefix
/// (e.g. `\x` for bytea) into the same buffer.
pub fn hex_encode_into(bytes: &[u8], out: &mut String) {
    out.reserve(bytes.len() * 2);
    // Stage hex pairs into a 64-byte stack scratch and flush via
    // `push_str` once the buffer is full. The per-byte loop is now a
    // single table lookup + 2-byte memcpy into the stack scratch, with
    // amortised UTF-8 validation: validating one 64-byte ASCII chunk
    // is cheaper than 32 separate `String::push(char)` validity
    // checks.
    let mut scratch = [0u8; HEX_ENCODE_CHUNK];
    let mut pos = 0usize;
    for &b in bytes {
        let pair = HEX_PAIRS_TABLE[b as usize];
        scratch[pos] = pair[0];
        scratch[pos + 1] = pair[1];
        pos += 2;
        if pos == HEX_ENCODE_CHUNK {
            out.push_str(std::str::from_utf8(&scratch).unwrap_or(""));
            pos = 0;
        }
    }
    if pos > 0 {
        out.push_str(std::str::from_utf8(&scratch[..pos]).unwrap_or(""));
    }
}
