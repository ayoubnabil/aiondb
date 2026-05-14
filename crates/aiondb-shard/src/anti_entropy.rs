//! Anti-entropy synchroniser.
//!
//! Computes a Merkle tree hash of each range's key/value set so two
//! replicas can quickly determine whether they hold the same data
//! without exchanging every key. Used by the integrity verifier
//! (`range_scrubber`) and by the background reconciliation loop.

use std::collections::BTreeMap;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MerkleSummary {
    pub root_hash: u64,
    pub leaf_count: u64,
}

pub fn summarise(entries: &BTreeMap<Vec<u8>, Vec<u8>>) -> MerkleSummary {
    let mut leaves: Vec<u64> = entries.iter().map(|(k, v)| hash_pair(k, v)).collect();
    let leaf_count = leaves.len() as u64;
    while leaves.len() > 1 {
        let mut next = Vec::with_capacity(leaves.len().div_ceil(2));
        for pair in leaves.chunks(2) {
            let left = pair[0];
            let right = pair.get(1).copied().unwrap_or(left);
            next.push(hash_u64_pair(left, right));
        }
        leaves = next;
    }
    let root_hash = leaves.first().copied().unwrap_or(0);
    MerkleSummary {
        root_hash,
        leaf_count,
    }
}

fn hash_pair(key: &[u8], value: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in key.iter().chain(value.iter()) {
        h ^= u64::from(*byte);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

fn hash_u64_pair(left: u64, right: u64) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in left.to_le_bytes().iter().chain(right.to_le_bytes().iter()) {
        h ^= u64::from(*byte);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

pub fn divergent(left: &MerkleSummary, right: &MerkleSummary) -> bool {
    left.root_hash != right.root_hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> BTreeMap<Vec<u8>, Vec<u8>> {
        let mut m = BTreeMap::new();
        m.insert(b"a".to_vec(), b"1".to_vec());
        m.insert(b"b".to_vec(), b"2".to_vec());
        m.insert(b"c".to_vec(), b"3".to_vec());
        m
    }

    #[test]
    fn identical_inputs_produce_identical_summaries() {
        let a = summarise(&fixture());
        let b = summarise(&fixture());
        assert_eq!(a, b);
        assert!(!divergent(&a, &b));
    }

    #[test]
    fn divergent_inputs_produce_different_root_hashes() {
        let a = summarise(&fixture());
        let mut diff = fixture();
        diff.insert(b"d".to_vec(), b"4".to_vec());
        let b = summarise(&diff);
        assert!(divergent(&a, &b));
    }

    #[test]
    fn empty_map_summary_is_zero() {
        let empty = BTreeMap::new();
        let s = summarise(&empty);
        assert_eq!(s.leaf_count, 0);
        assert_eq!(s.root_hash, 0);
    }

    #[test]
    fn single_entry_summary_uses_self_pair() {
        let mut m = BTreeMap::new();
        m.insert(b"x".to_vec(), b"y".to_vec());
        let s = summarise(&m);
        assert_eq!(s.leaf_count, 1);
        assert_ne!(s.root_hash, 0);
    }

    #[test]
    fn leaf_count_matches_input_size() {
        let s = summarise(&fixture());
        assert_eq!(s.leaf_count, 3);
    }
}
