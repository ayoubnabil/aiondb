use std::{fmt, mem};

use subtle::ConstantTimeEq;
use zeroize::Zeroize;

fn fmt_redacted(f: &mut fmt::Formatter<'_>, type_name: &str) -> fmt::Result {
    write!(f, "{type_name}(**redacted**)")
}

fn ct_eq_bytes(left: &[u8], right: &[u8]) -> bool {
    // `<[u8] as ConstantTimeEq>::ct_eq` short-circuits on length mismatch,
    // which leaks the stored secret's length via timing. Walk to max length
    // with zero-padded compare so the length difference is folded into the
    // accumulator without an early exit.
    let max_len = left.len().max(right.len());
    let len_eq = (left.len() as u64).ct_eq(&(right.len() as u64));
    let mut byte_diff: u8 = 0;
    for i in 0..max_len {
        let l = *left.get(i).unwrap_or(&0);
        let r = *right.get(i).unwrap_or(&0);
        byte_diff |= l ^ r;
    }
    bool::from(byte_diff.ct_eq(&0) & len_eq)
}

macro_rules! secret_type {
    (
        $name:ident,
        $inner:ty,
        $type_name:literal,
        $accessor:ident($self_ident:ident) -> $access_ty:ty => $access_body:expr,
        eq($left_ident:ident, $right_ident:ident) => $eq_body:expr,
        $from:ty
    ) => {
        #[derive(Default)]
        pub struct $name {
            inner: $inner,
        }

        impl $name {
            pub fn new(inner: impl Into<$inner>) -> Self {
                Self {
                    inner: inner.into(),
                }
            }

            pub fn $accessor(&$self_ident) -> $access_ty {
                $access_body
            }

            pub fn into_inner(mut self) -> $inner {
                mem::take(&mut self.inner)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt_redacted(f, $type_name)
            }
        }

        impl PartialEq for $name {
            fn eq(&$left_ident, $right_ident: &Self) -> bool {
                $eq_body
            }
        }

        impl Eq for $name {}

        impl Drop for $name {
            fn drop(&mut self) {
                self.inner.zeroize();
            }
        }

        impl From<$from> for $name {
            fn from(value: $from) -> Self {
                Self::new(value)
            }
        }
    };
}

secret_type!(
    SecretString,
    String,
    "SecretString",
    as_str(self) -> &str => self.inner.as_str(),
    eq(self, other) => ct_eq_bytes(self.inner.as_bytes(), other.inner.as_bytes()),
    String
);

impl From<&str> for SecretString {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

secret_type!(
    SecretBytes,
    Vec<u8>,
    "SecretBytes",
    as_bytes(self) -> &[u8] => self.inner.as_slice(),
    eq(self, other) => ct_eq_bytes(self.inner.as_slice(), other.inner.as_slice()),
    Vec<u8>
);

impl From<&[u8]> for SecretBytes {
    fn from(value: &[u8]) -> Self {
        Self::new(value.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- SecretString::new and as_str() ---
    #[test]
    fn secret_string_new_and_as_str() {
        let s = SecretString::new("hello".to_string());
        assert_eq!(s.as_str(), "hello");
    }

    // --- SecretString from &str ---
    #[test]
    fn secret_string_from_str_ref() {
        let s: SecretString = "world".into();
        assert_eq!(s.as_str(), "world");
    }

    // --- SecretString from String ---
    #[test]
    fn secret_string_from_string() {
        let s: SecretString = String::from("owned").into();
        assert_eq!(s.as_str(), "owned");
    }

    // --- SecretString Debug does NOT reveal content ---
    #[test]
    fn secret_string_debug_redacted() {
        let s = SecretString::new("supersecret".to_string());
        let debug_output = format!("{s:?}");
        assert!(
            debug_output.contains("**redacted**"),
            "Debug output should contain **redacted**, got: {debug_output}"
        );
        assert!(
            !debug_output.contains("supersecret"),
            "Debug output must not contain the secret"
        );
    }

    // --- SecretString equality: same content -> equal ---
    #[test]
    fn secret_string_eq_same_content() {
        let a = SecretString::new("abc".to_string());
        let b = SecretString::new("abc".to_string());
        assert_eq!(a, b);
    }

    // --- SecretString equality: different content -> not equal ---
    #[test]
    fn secret_string_ne_different_content() {
        let a = SecretString::new("abc".to_string());
        let b = SecretString::new("xyz".to_string());
        assert_ne!(a, b);
    }

    // --- SecretString equality: different lengths -> not equal ---
    #[test]
    fn secret_string_ne_different_lengths() {
        let a = SecretString::new("short".to_string());
        let b = SecretString::new("a much longer string".to_string());
        assert_ne!(a, b);
    }

    // --- SecretString::into_inner returns the string ---
    #[test]
    fn secret_string_into_inner() {
        let s = SecretString::new("recover_me".to_string());
        let inner = s.into_inner();
        assert_eq!(inner, "recover_me");
    }

    // --- SecretString::default is empty string ---
    #[test]
    fn secret_string_default_is_empty() {
        let s = SecretString::default();
        assert_eq!(s.as_str(), "");
    }

    // --- SecretBytes::new and as_bytes() ---
    #[test]
    fn secret_bytes_new_and_as_bytes() {
        let b = SecretBytes::new(vec![1, 2, 3]);
        assert_eq!(b.as_bytes(), &[1, 2, 3]);
    }

    // --- SecretBytes from Vec<u8> ---
    #[test]
    fn secret_bytes_from_vec() {
        let b: SecretBytes = vec![10, 20, 30].into();
        assert_eq!(b.as_bytes(), &[10, 20, 30]);
    }

    // --- SecretBytes from &[u8] ---
    #[test]
    fn secret_bytes_from_slice() {
        let data: &[u8] = &[0xDE, 0xAD, 0xBE, 0xEF];
        let b: SecretBytes = data.into();
        assert_eq!(b.as_bytes(), &[0xDE, 0xAD, 0xBE, 0xEF]);
    }

    // --- SecretBytes Debug does NOT reveal content ---
    #[test]
    fn secret_bytes_debug_redacted() {
        let b = SecretBytes::new(vec![1, 2, 3, 4, 5]);
        let debug_output = format!("{b:?}");
        assert!(
            debug_output.contains("**redacted**"),
            "Debug output should contain **redacted**, got: {debug_output}"
        );
        // Ensure raw bytes not present in debug
        assert!(!debug_output.contains("[1, 2, 3, 4, 5]"));
    }

    // --- SecretBytes equality ---
    #[test]
    fn secret_bytes_equality() {
        let a = SecretBytes::new(vec![1, 2, 3]);
        let b = SecretBytes::new(vec![1, 2, 3]);
        let c = SecretBytes::new(vec![4, 5, 6]);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    // --- SecretBytes::default is empty vec ---
    #[test]
    fn secret_bytes_default_is_empty() {
        let b = SecretBytes::default();
        assert!(b.as_bytes().is_empty());
    }

    // ===================================================================
    // NEW EDGE CASE TESTS
    // ===================================================================

    // --- SecretString: empty string equality ---

    #[test]
    fn secret_string_empty_eq_empty() {
        let a = SecretString::new(String::new());
        let b = SecretString::new(String::new());
        assert_eq!(a, b);
    }

    #[test]
    fn secret_string_empty_ne_nonempty() {
        let a = SecretString::new(String::new());
        let b = SecretString::new("x".to_string());
        assert_ne!(a, b);
    }

    // --- SecretString: very long secret ---

    #[test]
    fn secret_string_very_long() {
        let long = "a".repeat(100_000);
        let s = SecretString::new(long.clone());
        assert_eq!(s.as_str(), long.as_str());
    }

    #[test]
    fn secret_string_very_long_equality() {
        let long = "b".repeat(100_000);
        let a = SecretString::new(long.clone());
        let b = SecretString::new(long);
        assert_eq!(a, b);
    }

    #[test]
    fn secret_string_very_long_inequality() {
        let a_str = "c".repeat(100_000);
        let mut b_str = a_str.clone();
        // Change the very last character
        b_str.pop();
        b_str.push('d');
        let a = SecretString::new(a_str);
        let b = SecretString::new(b_str);
        assert_ne!(a, b);
    }

    // --- SecretString: unicode content ---

    #[test]
    fn secret_string_unicode_content() {
        let s = SecretString::new("Héllo Wörld 日本語 🔑".to_string());
        assert_eq!(s.as_str(), "Héllo Wörld 日本語 🔑");
    }

    #[test]
    fn secret_string_unicode_equality() {
        let a = SecretString::new("café".to_string());
        let b = SecretString::new("café".to_string());
        assert_eq!(a, b);
    }

    #[test]
    fn secret_string_unicode_inequality() {
        let a = SecretString::new("café".to_string());
        let b = SecretString::new("cafe".to_string());
        assert_ne!(a, b);
    }

    // --- SecretString: special characters ---

    #[test]
    fn secret_string_null_bytes_in_content() {
        let s = SecretString::new("before\0after".to_string());
        assert_eq!(s.as_str(), "before\0after");
    }

    #[test]
    fn secret_string_newlines_and_tabs() {
        let s = SecretString::new("line1\nline2\ttab".to_string());
        assert_eq!(s.as_str(), "line1\nline2\ttab");
    }

    // --- SecretString: Debug never reveals content for various inputs ---

    #[test]
    fn secret_string_debug_does_not_reveal_unicode() {
        let s = SecretString::new("日本語パスワード".to_string());
        let dbg = format!("{s:?}");
        assert!(!dbg.contains("日本語"));
        assert!(dbg.contains("SecretString"));
    }

    #[test]
    fn secret_string_debug_does_not_reveal_empty() {
        let s = SecretString::new(String::new());
        let dbg = format!("{s:?}");
        assert!(dbg.contains("**redacted**"));
        assert!(dbg.contains("SecretString"));
    }

    // --- SecretString: into_inner on empty string ---

    #[test]
    fn secret_string_into_inner_empty() {
        let s = SecretString::new(String::new());
        assert_eq!(s.into_inner(), "");
    }

    // --- SecretString: constant-time comparison - same prefix different suffix ---

    #[test]
    fn secret_string_ct_same_prefix_different_suffix() {
        let a = SecretString::new("password123".to_string());
        let b = SecretString::new("password124".to_string());
        assert_ne!(a, b);
    }

    // --- SecretString: same length but completely different ---

    #[test]
    fn secret_string_same_length_all_different() {
        let a = SecretString::new("aaaa".to_string());
        let b = SecretString::new("zzzz".to_string());
        assert_ne!(a, b);
    }

    // --- SecretBytes: empty bytes equality ---

    #[test]
    fn secret_bytes_empty_eq_empty() {
        let a = SecretBytes::new(vec![]);
        let b = SecretBytes::new(vec![]);
        assert_eq!(a, b);
    }

    #[test]
    fn secret_bytes_empty_ne_nonempty() {
        let a = SecretBytes::new(vec![]);
        let b = SecretBytes::new(vec![0]);
        assert_ne!(a, b);
    }

    // --- SecretBytes: very long binary data ---

    #[test]
    fn secret_bytes_very_long() {
        let data = vec![0xABu8; 100_000];
        let b = SecretBytes::new(data.clone());
        assert_eq!(b.as_bytes().len(), 100_000);
        assert_eq!(b.as_bytes(), data.as_slice());
    }

    #[test]
    fn secret_bytes_very_long_equality() {
        let data = vec![0xCDu8; 50_000];
        let a = SecretBytes::new(data.clone());
        let b = SecretBytes::new(data);
        assert_eq!(a, b);
    }

    #[test]
    fn secret_bytes_very_long_inequality_last_byte() {
        let mut data_a = vec![0xFFu8; 50_000];
        let data_b = data_a.clone();
        data_a[49_999] = 0xFE;
        let a = SecretBytes::new(data_a);
        let b = SecretBytes::new(data_b);
        assert_ne!(a, b);
    }

    // --- SecretBytes: all zero bytes ---

    #[test]
    fn secret_bytes_all_zeros() {
        let a = SecretBytes::new(vec![0u8; 256]);
        let b = SecretBytes::new(vec![0u8; 256]);
        assert_eq!(a, b);
    }

    // --- SecretBytes: all 0xFF bytes ---

    #[test]
    fn secret_bytes_all_ones() {
        let a = SecretBytes::new(vec![0xFFu8; 256]);
        let b = SecretBytes::new(vec![0xFFu8; 256]);
        assert_eq!(a, b);
    }

    // --- SecretBytes: single byte ---

    #[test]
    fn secret_bytes_single_byte() {
        let a = SecretBytes::new(vec![42]);
        assert_eq!(a.as_bytes(), &[42]);
    }

    // --- SecretBytes: binary data with all byte values 0x00..0xFF ---

    #[test]
    fn secret_bytes_all_byte_values() {
        let data: Vec<u8> = (0..=255).collect();
        let a = SecretBytes::new(data.clone());
        let b = SecretBytes::new(data);
        assert_eq!(a, b);
        assert_eq!(a.as_bytes().len(), 256);
    }

    // --- SecretBytes: into_inner on non-empty ---

    #[test]
    fn secret_bytes_into_inner_preserves_data() {
        let data = vec![10, 20, 30, 40, 50];
        let b = SecretBytes::new(data.clone());
        let inner = b.into_inner();
        assert_eq!(inner, data);
    }

    // --- SecretBytes: into_inner on empty ---

    #[test]
    fn secret_bytes_into_inner_empty() {
        let b = SecretBytes::new(vec![]);
        let inner = b.into_inner();
        assert!(inner.is_empty());
    }

    // --- SecretBytes: Debug format includes type name ---

    #[test]
    fn secret_bytes_debug_includes_type_name() {
        let b = SecretBytes::new(vec![1, 2, 3]);
        let dbg = format!("{b:?}");
        assert!(dbg.contains("SecretBytes"));
    }

    // --- SecretBytes: from slice of all zeros ---

    #[test]
    fn secret_bytes_from_zero_slice() {
        let data: &[u8] = &[0, 0, 0, 0, 0];
        let b: SecretBytes = data.into();
        assert_eq!(b.as_bytes(), &[0, 0, 0, 0, 0]);
    }

    // --- Constant-time comparison: different length always not equal ---

    #[test]
    fn secret_bytes_different_lengths_not_equal() {
        let a = SecretBytes::new(vec![1, 2, 3]);
        let b = SecretBytes::new(vec![1, 2, 3, 4]);
        assert_ne!(a, b);
    }

    #[test]
    fn secret_string_different_lengths_not_equal_single_char() {
        let a = SecretString::new("a".to_string());
        let b = SecretString::new("aa".to_string());
        assert_ne!(a, b);
    }

    // --- SecretString: from &str with special characters ---

    #[test]
    fn secret_string_from_str_special_chars() {
        let s: SecretString = "p@$$w0rd!#%^&*()".into();
        assert_eq!(s.as_str(), "p@$$w0rd!#%^&*()");
    }
}
