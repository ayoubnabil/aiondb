use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub struct MacAddr([u8; 6]);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub struct MacAddr8([u8; 8]);

#[allow(clippy::should_implement_trait)]
impl MacAddr {
    #[must_use]
    pub const fn new(bytes: [u8; 6]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 6] {
        &self.0
    }

    #[must_use]
    pub fn parse(input: &str) -> Option<Self> {
        parse_macaddr_like(input, false)
            .and_then(|v| <[u8; 6]>::try_from(v).ok())
            .map(Self)
    }

    #[must_use]
    pub const fn trunc(self) -> Self {
        let [a, b, c, ..] = self.0;
        Self([a, b, c, 0, 0, 0])
    }

    #[must_use]
    pub fn bitand(self, other: Self) -> Self {
        Self(std::array::from_fn(|i| self.0[i] & other.0[i]))
    }

    #[must_use]
    pub fn bitor(self, other: Self) -> Self {
        Self(std::array::from_fn(|i| self.0[i] | other.0[i]))
    }

    #[must_use]
    pub fn bitxor(self, other: Self) -> Self {
        Self(std::array::from_fn(|i| self.0[i] ^ other.0[i]))
    }

    #[must_use]
    pub fn bitnot(self) -> Self {
        Self(std::array::from_fn(|i| !self.0[i]))
    }

    #[must_use]
    #[allow(clippy::many_single_char_names)]
    pub const fn to_macaddr8(self) -> MacAddr8 {
        let [a, b, c, d, e, f] = self.0;
        MacAddr8::new([a, b, c, 0xff, 0xfe, d, e, f])
    }
}

#[allow(clippy::should_implement_trait)]
impl MacAddr8 {
    #[must_use]
    pub const fn new(bytes: [u8; 8]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 8] {
        &self.0
    }

    #[must_use]
    pub fn parse(input: &str) -> Option<Self> {
        let bytes = parse_macaddr_like(input, true)?;
        Some(match bytes.len() {
            6 => Self([
                bytes[0], bytes[1], bytes[2], 0xff, 0xfe, bytes[3], bytes[4], bytes[5],
            ]),
            8 => Self(bytes.try_into().ok()?),
            _ => return None,
        })
    }

    #[must_use]
    pub const fn trunc(self) -> Self {
        let [a, b, c, ..] = self.0;
        Self([a, b, c, 0, 0, 0, 0, 0])
    }

    #[must_use]
    pub fn bitand(self, other: Self) -> Self {
        Self(std::array::from_fn(|i| self.0[i] & other.0[i]))
    }

    #[must_use]
    pub fn bitor(self, other: Self) -> Self {
        Self(std::array::from_fn(|i| self.0[i] | other.0[i]))
    }

    #[must_use]
    pub fn bitxor(self, other: Self) -> Self {
        Self(std::array::from_fn(|i| self.0[i] ^ other.0[i]))
    }

    #[must_use]
    pub fn bitnot(self) -> Self {
        Self(std::array::from_fn(|i| !self.0[i]))
    }

    #[must_use]
    #[allow(clippy::many_single_char_names)]
    pub const fn set7bit(self) -> Self {
        let [a, b, c, d, e, f, g, h] = self.0;
        Self([a | 0x02, b, c, d, e, f, g, h])
    }

    /// PG `macaddr8::macaddr` cast: only valid when bytes 3..=4 are `ff:fe`
    /// (a EUI-48-encoded EUI-64). Returns `None` otherwise so callers can
    #[must_use]
    #[allow(clippy::many_single_char_names)]
    pub const fn to_macaddr(self) -> Option<MacAddr> {
        let [a, b, c, d, e, f, g, h] = self.0;
        if d == 0xff && e == 0xfe {
            Some(MacAddr::new([a, b, c, f, g, h]))
        } else {
            None
        }
    }
}

impl fmt::Display for MacAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_mac_bytes(f, &self.0)
    }
}

impl fmt::Display for MacAddr8 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_mac_bytes(f, &self.0)
    }
}

/// Lookup table mapping each byte 0x00..=0xFF to its 2-character lower-case
/// hex pair. Avoids the per-byte `write!("{byte:02x}")` round-trip through
/// the formatting machinery; one byte → one `[u8; 2]` table read + one
/// 2-byte memcpy into a stack buffer that is finally emitted in a single
/// `f.write_str` call.
const MAC_HEX_PAIRS: [[u8; 2]; 256] = {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut table = [[0u8; 2]; 256];
    let mut i = 0;
    while i < 256 {
        table[i] = [HEX[i >> 4], HEX[i & 0x0f]];
        i += 1;
    }
    table
};

fn write_mac_bytes(f: &mut fmt::Formatter<'_>, bytes: &[u8]) -> fmt::Result {
    // MacAddr (6 bytes) renders as 17 chars and MacAddr8 (8 bytes) as
    // 23 chars (`xx:xx:...:xx`). Build the whole rendering in a stack
    // buffer and emit it with one `f.write_str` so the per-byte path
    // is a table lookup + a couple of indexed writes - no formatting
    // machinery, no separator-only `write_str` call per byte.
    let mut buf = [0u8; 23];
    let mut pos = 0;
    for (index, &b) in bytes.iter().enumerate() {
        if index > 0 {
            buf[pos] = b':';
            pos += 1;
        }
        let pair = MAC_HEX_PAIRS[b as usize];
        buf[pos] = pair[0];
        buf[pos + 1] = pair[1];
        pos += 2;
    }
    let rendered = std::str::from_utf8(&buf[..pos]).unwrap_or("");
    f.write_str(rendered)
}

fn parse_macaddr_like(input: &str, allow_eui64: bool) -> Option<Vec<u8>> {
    let trimmed = input.trim();
    if let Some(bytes) = parse_variable_byte_groups(trimmed, ':', allow_eui64) {
        return Some(bytes);
    }
    if let Some(bytes) = parse_variable_byte_groups(trimmed, '-', allow_eui64) {
        return Some(bytes);
    }
    let patterns: &[(&str, &[usize])] = if allow_eui64 {
        &[
            (":", &[2, 2, 2, 2, 2, 2, 2, 2]),
            ("-", &[2, 2, 2, 2, 2, 2, 2, 2]),
            (":", &[6, 10]),
            ("-", &[6, 10]),
            (".", &[4, 4, 4, 4]),
            (":", &[8, 8]),
            (":", &[2, 2, 2, 2, 2, 2]),
            ("-", &[2, 2, 2, 2, 2, 2]),
            (":", &[6, 6]),
            ("-", &[6, 6]),
            (".", &[4, 4, 4]),
            ("-", &[4, 4, 4]),
            (":", &[4, 4, 4]),
        ]
    } else {
        &[
            (":", &[2, 2, 2, 2, 2, 2]),
            ("-", &[2, 2, 2, 2, 2, 2]),
            (":", &[6, 6]),
            ("-", &[6, 6]),
            (".", &[4, 4, 4]),
            ("-", &[4, 4, 4]),
        ]
    };

    for (separator, group_lengths) in patterns {
        if let Some(bytes) = parse_grouped_hex(trimmed, separator, group_lengths) {
            return Some(bytes);
        }
    }

    let plain_len = if allow_eui64 { [12, 16] } else { [12, 12] };
    if plain_len.contains(&trimmed.len()) && trimmed.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return hex_pairs_to_bytes(trimmed);
    }

    None
}

fn parse_variable_byte_groups(input: &str, sep: char, allow_eui64: bool) -> Option<Vec<u8>> {
    if !input.contains(sep) {
        return None;
    }
    let parts: Vec<_> = input.split(sep).collect();
    let valid_group_count = if allow_eui64 {
        matches!(parts.len(), 6 | 8)
    } else {
        parts.len() == 6
    };
    if !valid_group_count {
        return None;
    }

    let mut out = Vec::with_capacity(parts.len());
    for part in parts {
        if part.is_empty() || part.len() > 2 || !part.chars().all(|ch| ch.is_ascii_hexdigit()) {
            return None;
        }
        let parsed = u8::from_str_radix(part, 16).ok()?;
        out.push(parsed);
    }
    Some(out)
}

fn parse_grouped_hex(input: &str, separator: &str, group_lengths: &[usize]) -> Option<Vec<u8>> {
    let parts: Vec<_> = input.split(separator).collect();
    if parts.len() != group_lengths.len() {
        return None;
    }

    let mut out = Vec::new();
    for (part, expected_len) in parts.iter().zip(group_lengths.iter()) {
        if part.len() != *expected_len || !part.chars().all(|ch| ch.is_ascii_hexdigit()) {
            return None;
        }
        out.extend(hex_pairs_to_bytes(part)?);
    }
    Some(out)
}

fn hex_pairs_to_bytes(input: &str) -> Option<Vec<u8>> {
    if !input.len().is_multiple_of(2) {
        return None;
    }

    input
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = char::from(pair[0]).to_digit(16)?;
            let low = char::from(pair[1]).to_digit(16)?;
            u8::try_from((high << 4) | low).ok()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{MacAddr, MacAddr8};

    #[test]
    fn macaddr_parses_supported_formats() {
        let parsed = [
            "08:00:2b:01:02:03",
            "08-00-2b-01-02-03",
            "08002b:010203",
            "08002b-010203",
            "0800.2b01.0203",
            "0800-2b01-0203",
            "08002b010203",
        ]
        .into_iter()
        .map(MacAddr::parse)
        .collect::<Vec<_>>();

        assert!(parsed.iter().all(Option::is_some));
        assert!(MacAddr::parse("0800:2b01:0203").is_none());
    }

    #[test]
    fn macaddr8_parses_supported_formats() {
        let parsed = [
            "08:00:2b:01:02:03",
            "0800:2b01:0203",
            "08:00:2b:01:02:03:04:05",
            "08002b:0102030405",
            "0800.2b01.0203.0405",
            "08002b01:02030405",
            "08002b0102030405",
        ]
        .into_iter()
        .map(MacAddr8::parse)
        .collect::<Vec<_>>();

        assert!(parsed.iter().all(Option::is_some));
        assert_eq!(
            MacAddr8::parse("08:00:2b:01:02:03").unwrap().to_string(),
            "08:00:2b:ff:fe:01:02:03"
        );
    }

    #[test]
    fn macaddr8_set7bit_sets_local_bit() {
        assert_eq!(
            MacAddr8::parse("00:08:2b:01:02:03")
                .unwrap()
                .set7bit()
                .to_string(),
            "02:08:2b:ff:fe:01:02:03"
        );
    }

    /// POC: macaddr8 → macaddr cast must REJECT inputs whose bytes 3-4
    /// are not `ff:fe` (they are not EUI-48-encoded). Pre-fix the
    /// looks-valid macaddr; PG `macaddr8::macaddr` errors instead.
    #[test]
    fn macaddr8_to_macaddr_rejects_non_eui48_encoding() {
        // Bytes 3-4 = `01:02` are not `ff:fe` → cast invalid.
        let mac8 = MacAddr8::parse("00:11:22:01:02:33:44:55").unwrap();
        assert!(
            mac8.to_macaddr().is_none(),
            "macaddr8 with non-ff:fe middle bytes must not convert"
        );

        // Valid EUI-48-encoded form: bytes 3-4 = `ff:fe`.
        let mac8_valid = MacAddr8::parse("00:11:22:ff:fe:33:44:55").unwrap();
        let mac = mac8_valid
            .to_macaddr()
            .expect("valid encoding must convert");
        assert_eq!(mac.to_string(), "00:11:22:33:44:55");
    }
}
