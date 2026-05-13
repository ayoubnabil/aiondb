//! Centralized checksum functions used across the storage stack.
//!
//! All WAL records, catalog snapshots, storage snapshots, and buffer
//! pool pages use CRC32C (Castagnoli) for integrity verification.

/// CRC32C polynomial in bit-reversed form (Castagnoli).
const CRC32C_REVERSED_POLY: u32 = 0x82f6_3b78;

/// Compute a CRC32C checksum over the given data.
#[must_use]
pub fn compute_crc32c(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xffff_ffff;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            if (crc & 1) != 0 {
                crc = (crc >> 1) ^ CRC32C_REVERSED_POLY;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

/// Legacy FNV-1a checksum for backward compatibility with older
/// snapshot and WAL formats.
#[must_use]
pub fn compute_legacy_fnv1a(data: &[u8]) -> u32 {
    const FNV_OFFSET: u32 = 0x811c_9dc5;
    const FNV_PRIME: u32 = 0x0100_0193;
    let mut hash = FNV_OFFSET;
    for &byte in data {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32c_empty() {
        assert_eq!(compute_crc32c(&[]), 0x0000_0000);
    }

    #[test]
    fn crc32c_hello() {
        // Known CRC32C value for "hello".
        assert_eq!(compute_crc32c(b"hello"), 0x9a71_bb4c);
    }

    #[test]
    fn crc32c_deterministic() {
        let data = b"test data for checksum";
        assert_eq!(compute_crc32c(data), compute_crc32c(data));
    }

    #[test]
    fn legacy_fnv1a_deterministic() {
        let data = b"test data";
        assert_eq!(compute_legacy_fnv1a(data), compute_legacy_fnv1a(data));
    }

    #[test]
    fn crc32c_differs_from_legacy() {
        let data = b"some data";
        assert_ne!(compute_crc32c(data), compute_legacy_fnv1a(data));
    }

    #[test]
    fn legacy_fnv1a_known_value() {
        // FNV-1a 32-bit of "" is the offset basis itself.
        assert_eq!(compute_legacy_fnv1a(&[]), 0x811c_9dc5);
    }

    #[test]
    fn crc32c_single_byte() {
        // Verify single-byte input produces a non-trivial value.
        let val = compute_crc32c(&[0x42]);
        assert_ne!(val, 0);
        assert_ne!(val, 0xffff_ffff);
    }
}
