//! Row-level write conflict resolver.
//!
//! When two writes target the same key with overlapping logical
//! timestamps, the resolver decides which one wins. Policies :
//!
//! - `HighestTimestamp` : the larger HLC wins.
//! - `LowestId`         : the writer with the smaller txn id wins.
//! - `Custom`           : caller-provided closure.
//!
//! Returning a deterministic verdict is critical to keep replicas
//! converged across the cluster.

use std::cmp::Ordering;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WriteCandidate {
    pub txn_id: u64,
    pub timestamp_ns: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConflictPolicy {
    HighestTimestamp,
    LowestId,
}

pub fn resolve(a: WriteCandidate, b: WriteCandidate, policy: ConflictPolicy) -> WriteCandidate {
    match policy {
        ConflictPolicy::HighestTimestamp => match a.timestamp_ns.cmp(&b.timestamp_ns) {
            Ordering::Greater => a,
            Ordering::Less => b,
            Ordering::Equal => {
                if a.txn_id <= b.txn_id {
                    a
                } else {
                    b
                }
            }
        },
        ConflictPolicy::LowestId => {
            if a.txn_id <= b.txn_id {
                a
            } else {
                b
            }
        }
    }
}

pub fn resolve_with<F>(a: WriteCandidate, b: WriteCandidate, custom: F) -> WriteCandidate
where
    F: FnOnce(WriteCandidate, WriteCandidate) -> WriteCandidate,
{
    custom(a, b)
}

pub fn resolve_many(
    mut candidates: Vec<WriteCandidate>,
    policy: ConflictPolicy,
) -> Option<WriteCandidate> {
    if candidates.is_empty() {
        return None;
    }
    let mut winner = candidates.swap_remove(0);
    for c in candidates {
        winner = resolve(winner, c, policy);
    }
    Some(winner)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(id: u64, ts: u64) -> WriteCandidate {
        WriteCandidate {
            txn_id: id,
            timestamp_ns: ts,
        }
    }

    #[test]
    fn highest_timestamp_wins() {
        let r = resolve(w(1, 100), w(2, 200), ConflictPolicy::HighestTimestamp);
        assert_eq!(r, w(2, 200));
    }

    #[test]
    fn tie_breaks_by_lowest_id() {
        let r = resolve(w(5, 100), w(2, 100), ConflictPolicy::HighestTimestamp);
        assert_eq!(r, w(2, 100));
    }

    #[test]
    fn lowest_id_policy_ignores_timestamp() {
        let r = resolve(w(10, 1000), w(2, 1), ConflictPolicy::LowestId);
        assert_eq!(r, w(2, 1));
    }

    #[test]
    fn custom_resolver_runs() {
        let r = resolve_with(w(1, 0), w(2, 0), |a, b| if a.txn_id == 2 { a } else { b });
        assert_eq!(r, w(2, 0));
    }

    #[test]
    fn resolve_many_reduces_to_winner() {
        let v = vec![w(1, 10), w(2, 50), w(3, 30)];
        let r = resolve_many(v, ConflictPolicy::HighestTimestamp).unwrap();
        assert_eq!(r, w(2, 50));
    }

    #[test]
    fn resolve_many_empty_returns_none() {
        let r = resolve_many(vec![], ConflictPolicy::HighestTimestamp);
        assert!(r.is_none());
    }

    #[test]
    fn resolve_is_commutative_under_highest_ts() {
        let a = w(1, 10);
        let b = w(2, 20);
        assert_eq!(
            resolve(a, b, ConflictPolicy::HighestTimestamp),
            resolve(b, a, ConflictPolicy::HighestTimestamp)
        );
    }
}
