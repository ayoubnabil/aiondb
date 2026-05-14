use aiondb_engine::{DbError, DbResult};

use super::algorithm::AlgorithmRegistration;
use super::raft;

const HA_ALGORITHM_ENV: &str = "AIONDB_HA_ALGORITHM";
const DEFAULT_HA_ALGORITHM: &str = "raft";

const ALGORITHMS: &[AlgorithmRegistration] = &[AlgorithmRegistration {
    name: "raft",
    build: raft::build,
}];

pub(crate) fn selected_algorithm_name() -> String {
    normalize_algorithm_name(std::env::var(HA_ALGORITHM_ENV).ok().as_deref())
}

pub(crate) fn resolve_algorithm(raw_name: &str) -> DbResult<&'static AlgorithmRegistration> {
    let name = normalize_algorithm_name(Some(raw_name));
    for registration in ALGORITHMS {
        if registration.name == name {
            return Ok(registration);
        }
    }

    let supported = ALGORITHMS
        .iter()
        .map(|registration| registration.name)
        .collect::<Vec<_>>()
        .join(", ");
    Err(DbError::feature_not_supported(format!(
        "unsupported HA algorithm: {name} (supported: {supported})"
    )))
}

fn normalize_algorithm_name(raw: Option<&str>) -> String {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_else(|| DEFAULT_HA_ALGORITHM.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_algorithm_defaults_to_raft() {
        assert_eq!(normalize_algorithm_name(None), "raft");
        assert_eq!(normalize_algorithm_name(Some("")), "raft");
        assert_eq!(normalize_algorithm_name(Some("   ")), "raft");
    }

    #[test]
    fn resolve_algorithm_is_case_insensitive() {
        let registration = resolve_algorithm("  RaFt  ").expect("algorithm should resolve");
        assert_eq!(registration.name, "raft");
    }

    #[test]
    fn resolve_algorithm_unknown_lists_supported_values() {
        let error = resolve_algorithm("paxos").expect_err("unknown algorithm should fail");
        let message = error.to_string();
        assert!(
            message.contains("unsupported HA algorithm: paxos"),
            "unexpected message: {message}"
        );
        assert!(
            message.contains("supported: raft"),
            "unexpected message: {message}"
        );
    }
}
