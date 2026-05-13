//! Bloom-filter-based replica diff.
//!
//! Each replica computes a Bloom filter over its keys and ships it
//! to the peer. The peer queries its own keys against the filter:
//! keys that miss the filter are definitely absent on the sender
//! and become repair candidates. Net effect: replicas swap O(N)
//! bits instead of O(N) keys to detect divergence.

#[derive(Clone, Debug)]
pub struct BloomFilter {
    bits: Vec<u64>,
    num_hashes: u32,
    num_bits: usize,
}

impl BloomFilter {
    pub fn new(num_bits: usize, num_hashes: u32) -> Self {
        let words = num_bits.div_ceil(64);
        Self {
            bits: vec![0; words],
            num_hashes,
            num_bits: words * 64,
        }
    }

    pub fn insert(&mut self, key: &[u8]) {
        let hs: Vec<u64> = self.hashes(key).collect();
        for h in hs {
            let bit_idx = (h as usize) % self.num_bits;
            self.bits[bit_idx / 64] |= 1u64 << (bit_idx % 64);
        }
    }

    pub fn contains(&self, key: &[u8]) -> bool {
        for h in self.hashes(key) {
            let bit_idx = (h as usize) % self.num_bits;
            if self.bits[bit_idx / 64] & (1u64 << (bit_idx % 64)) == 0 {
                return false;
            }
        }
        true
    }

    pub fn merge(&mut self, other: &Self) {
        debug_assert_eq!(self.bits.len(), other.bits.len());
        for (a, b) in self.bits.iter_mut().zip(other.bits.iter()) {
            *a |= *b;
        }
    }

    fn hashes(&self, key: &[u8]) -> impl Iterator<Item = u64> + '_ {
        let h1 = fnv64(key);
        let h2 = fnv64a(key);
        (0..self.num_hashes).map(move |i| h1.wrapping_add(h2.wrapping_mul(i as u64)))
    }
}

fn fnv64(s: &[u8]) -> u64 {
    let mut h = 0xCBF2_9CE4_8422_2325u64;
    for b in s {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

fn fnv64a(s: &[u8]) -> u64 {
    let mut h = 0x100u64;
    for b in s {
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
        h ^= *b as u64;
    }
    h
}

#[derive(Clone, Debug)]
pub struct DiffReport {
    pub missing_on_remote: Vec<Vec<u8>>,
}

pub fn diff_against(local_keys: &[Vec<u8>], remote_filter: &BloomFilter) -> DiffReport {
    let missing = local_keys
        .iter()
        .filter(|k| !remote_filter.contains(k))
        .cloned()
        .collect();
    DiffReport {
        missing_on_remote: missing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_contains() {
        let mut b = BloomFilter::new(1024, 3);
        b.insert(b"alpha");
        assert!(b.contains(b"alpha"));
    }

    #[test]
    fn absent_key_definitely_missing() {
        let mut b = BloomFilter::new(8192, 5);
        for i in 0..100u32 {
            b.insert(&i.to_be_bytes());
        }
        // A value we never inserted shouldn't usually be present.
        // With 8192 bits / 5 hashes / 100 items, fpr is very low.
        assert!(!b.contains(b"definitely-absent-string"));
    }

    #[test]
    fn merge_takes_union() {
        let mut a = BloomFilter::new(1024, 3);
        let mut b = BloomFilter::new(1024, 3);
        a.insert(b"x");
        b.insert(b"y");
        a.merge(&b);
        assert!(a.contains(b"x"));
        assert!(a.contains(b"y"));
    }

    #[test]
    fn diff_against_reports_missing() {
        let mut remote = BloomFilter::new(8192, 5);
        remote.insert(b"a");
        remote.insert(b"b");
        let local = vec![b"a".to_vec(), b"c".to_vec()];
        let report = diff_against(&local, &remote);
        assert!(report.missing_on_remote.contains(&b"c".to_vec()));
        assert!(!report.missing_on_remote.contains(&b"a".to_vec()));
    }

    #[test]
    fn empty_filter_reports_all() {
        let remote = BloomFilter::new(1024, 3);
        let local = vec![b"a".to_vec(), b"b".to_vec()];
        let report = diff_against(&local, &remote);
        assert_eq!(report.missing_on_remote.len(), 2);
    }

    #[test]
    fn deterministic_for_same_input() {
        let mut a = BloomFilter::new(1024, 4);
        let mut b = BloomFilter::new(1024, 4);
        for i in 0..50u32 {
            a.insert(&i.to_le_bytes());
            b.insert(&i.to_le_bytes());
        }
        assert_eq!(a.bits, b.bits);
    }
}
