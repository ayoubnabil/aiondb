//! Synthetic workload generator.
//!
//! Used by stress tests and benchmarks. Produces a deterministic
//! sequence of mixed read/write operations across a configured key
//! space. Three workload profiles : Uniform, Skewed (Zipf-like),
//! ReadHeavy.

use crate::dist_sender::BatchRequest;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkloadProfile {
    Uniform,
    Skewed,
    ReadHeavy,
}

#[derive(Clone, Debug)]
pub struct WorkloadConfig {
    pub profile: WorkloadProfile,
    pub keyspace_size: u64,
    pub ops: usize,
    /// Probability (0..100) that a given op is a write.
    pub write_pct: u8,
    pub seed: u64,
}

#[derive(Debug)]
pub struct WorkloadGenerator {
    config: WorkloadConfig,
    state: u64,
}

impl WorkloadGenerator {
    pub fn new(config: WorkloadConfig) -> Self {
        let state = config.seed.max(1);
        Self { config, state }
    }

    pub fn generate(&mut self) -> Vec<BatchRequest> {
        let mut out = Vec::with_capacity(self.config.ops);
        for _ in 0..self.config.ops {
            let key = self.next_key();
            let is_write = (self.next_random() % 100) < u64::from(self.config.write_pct);
            let op = if is_write {
                BatchRequest::Put {
                    key: format_key(key),
                    value: Some(format_value(key)),
                }
            } else {
                BatchRequest::Get {
                    key: format_key(key),
                }
            };
            out.push(op);
        }
        out
    }

    fn next_key(&mut self) -> u64 {
        let raw = self.next_random();
        match self.config.profile {
            WorkloadProfile::Uniform => raw % self.config.keyspace_size,
            WorkloadProfile::Skewed => skewed(raw, self.config.keyspace_size),
            WorkloadProfile::ReadHeavy => raw % self.config.keyspace_size,
        }
    }

    fn next_random(&mut self) -> u64 {
        // Splitmix-style PRNG.
        self.state = self
            .state
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(0x53C5_8D5C_18B7_8D6F);
        let mut x = self.state;
        x ^= x >> 30;
        x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x ^= x >> 27;
        x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
        x ^ (x >> 31)
    }
}

fn skewed(value: u64, keyspace: u64) -> u64 {
    // Crude Zipf-ish : square the random number, then modulo. Heavy
    // weight on small keys -> hot spots.
    let scaled = value.wrapping_mul(value);
    scaled % keyspace
}

fn format_key(n: u64) -> Vec<u8> {
    format!("k{n:020}").into_bytes()
}

fn format_value(n: u64) -> Vec<u8> {
    format!("v{n}").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn produces_requested_op_count() {
        let mut gen = WorkloadGenerator::new(WorkloadConfig {
            profile: WorkloadProfile::Uniform,
            keyspace_size: 1000,
            ops: 100,
            write_pct: 50,
            seed: 42,
        });
        let ops = gen.generate();
        assert_eq!(ops.len(), 100);
    }

    #[test]
    fn write_pct_roughly_matches_target() {
        let mut gen = WorkloadGenerator::new(WorkloadConfig {
            profile: WorkloadProfile::Uniform,
            keyspace_size: 1000,
            ops: 10_000,
            write_pct: 30,
            seed: 7,
        });
        let ops = gen.generate();
        let writes = ops
            .iter()
            .filter(|o| matches!(o, BatchRequest::Put { .. }))
            .count();
        // Tolerate +/-3% deviation.
        let pct = writes as f64 / 10_000.0;
        assert!(pct > 0.27 && pct < 0.33, "writes={writes} pct={pct:.3}");
    }

    #[test]
    fn deterministic_under_same_seed() {
        let mut g1 = WorkloadGenerator::new(WorkloadConfig {
            profile: WorkloadProfile::Uniform,
            keyspace_size: 1000,
            ops: 50,
            write_pct: 50,
            seed: 1,
        });
        let mut g2 = WorkloadGenerator::new(WorkloadConfig {
            profile: WorkloadProfile::Uniform,
            keyspace_size: 1000,
            ops: 50,
            write_pct: 50,
            seed: 1,
        });
        assert_eq!(g1.generate(), g2.generate());
    }

    #[test]
    fn skewed_profile_concentrates_keys() {
        let mut gen = WorkloadGenerator::new(WorkloadConfig {
            profile: WorkloadProfile::Skewed,
            keyspace_size: 100,
            ops: 10_000,
            write_pct: 0,
            seed: 42,
        });
        let ops = gen.generate();
        let mut counts: HashMap<Vec<u8>, usize> = HashMap::new();
        for op in ops {
            if let BatchRequest::Get { key } = op {
                *counts.entry(key).or_default() += 1;
            }
        }
        // At least one hot key should exceed 2% of total.
        let max = counts.values().copied().max().unwrap();
        assert!(
            max as f64 / 10_000.0 > 0.01,
            "skewed profile should produce hot keys, max={max}"
        );
    }

    #[test]
    fn empty_workload_returns_empty_ops() {
        let mut gen = WorkloadGenerator::new(WorkloadConfig {
            profile: WorkloadProfile::Uniform,
            keyspace_size: 100,
            ops: 0,
            write_pct: 0,
            seed: 1,
        });
        assert!(gen.generate().is_empty());
    }
}
