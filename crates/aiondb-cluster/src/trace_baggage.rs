//! W3C trace context baggage.
//!
//! Implements the `baggage` HTTP header per
//! <https://www.w3.org/TR/baggage/>. Each key=value pair is comma-
//! separated; values are percent-encoded. The propagator carries
//! the baggage across RPC boundaries so observability tools can
//! correlate across services.

use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TraceBaggage {
    items: BTreeMap<String, String>,
}

impl TraceBaggage {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&mut self, key: &str, value: &str) -> &mut Self {
        self.items.insert(key.to_string(), value.to_string());
        self
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.items.get(key).map(|s| s.as_str())
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &String)> {
        self.items.iter()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn to_header(&self) -> String {
        let mut parts: Vec<String> = self
            .items
            .iter()
            .map(|(k, v)| format!("{k}={}", percent_encode(v)))
            .collect();
        parts.sort();
        parts.join(",")
    }

    pub fn from_header(header: &str) -> Self {
        let mut out = Self::new();
        for piece in header.split(',') {
            let piece = piece.trim();
            if piece.is_empty() {
                continue;
            }
            if let Some(eq) = piece.find('=') {
                let k = piece[..eq].trim();
                let v = piece[eq + 1..].trim();
                if !k.is_empty() {
                    out.items.insert(k.to_string(), percent_decode(v));
                }
            }
        }
        out
    }
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(c) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(c as char);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_get() {
        let mut b = TraceBaggage::new();
        b.set("user_id", "42").set("tenant", "acme");
        assert_eq!(b.get("user_id"), Some("42"));
        assert_eq!(b.get("tenant"), Some("acme"));
    }

    #[test]
    fn to_header_is_alphabetical() {
        let mut b = TraceBaggage::new();
        b.set("z", "1").set("a", "2");
        let h = b.to_header();
        assert!(h.starts_with("a="));
    }

    #[test]
    fn round_trip_preserves_values() {
        let mut b = TraceBaggage::new();
        b.set("k", "hello world").set("k2", "v=v");
        let h = b.to_header();
        let parsed = TraceBaggage::from_header(&h);
        assert_eq!(parsed.get("k"), Some("hello world"));
        assert_eq!(parsed.get("k2"), Some("v=v"));
    }

    #[test]
    fn from_header_ignores_empty_pieces() {
        let b = TraceBaggage::from_header(",,k=v,,");
        assert_eq!(b.len(), 1);
        assert_eq!(b.get("k"), Some("v"));
    }

    #[test]
    fn percent_encoding_handles_special_chars() {
        let mut b = TraceBaggage::new();
        b.set("k", "@#$%");
        let h = b.to_header();
        assert!(h.contains("%40"));
        let parsed = TraceBaggage::from_header(&h);
        assert_eq!(parsed.get("k"), Some("@#$%"));
    }

    #[test]
    fn empty_baggage_is_empty_header() {
        let b = TraceBaggage::new();
        assert_eq!(b.to_header(), "");
        assert!(b.is_empty());
    }
}
