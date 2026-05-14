/// Ultra-fast xorshift64 PRNG for fuzzing. Not cryptographic. Deterministic.
pub struct FastRng {
    state: u64,
}

impl FastRng {
    #[must_use]
    pub fn seeded(seed: u64) -> Self {
        Self {
            state: if seed == 0 { 1 } else { seed },
        }
    }

    /// Next raw u64
    pub fn next_u64(&mut self) -> u64 {
        self.state ^= self.state << 13;
        self.state ^= self.state >> 7;
        self.state ^= self.state << 17;
        self.state
    }

    /// Random u64 in [lo, hi)
    pub fn next_range(&mut self, lo: u64, hi: u64) -> u64 {
        if lo >= hi {
            return lo;
        }
        lo + (self.next_u64() % (hi - lo))
    }

    /// Random i64 in [lo, hi)
    pub fn next_i64(&mut self, lo: i64, hi: i64) -> i64 {
        if lo >= hi {
            return lo;
        }
        let range = (hi - lo) as u64;
        lo + (self.next_u64() % range) as i64
    }

    /// Random bool with `p` probability of true (0..100)
    pub fn chance(&mut self, p: u64) -> bool {
        self.next_range(0, 100) < p
    }

    /// Pick a random element from a slice.
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        let idx = self.next_range(0, items.len() as u64) as usize;
        &items[idx]
    }

    /// Random alphanumeric string
    pub fn random_alnum(&mut self, len: usize) -> String {
        const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
        (0..len)
            .map(|_| {
                let idx = self.next_range(0, CHARS.len() as u64) as usize;
                CHARS[idx] as char
            })
            .collect()
    }
}
