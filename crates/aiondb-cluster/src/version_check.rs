//! Cluster identity + protocol version compatibility check.
//!
//! Two nodes can only join the same cluster if :
//!
//! - They share the same `cluster_id`.
//! - Their protocol-major versions match.
//! - Their protocol-minor versions differ by at most `tolerance`.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProtocolVersion {
    pub major: u16,
    pub minor: u16,
}

impl ProtocolVersion {
    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ClusterIdentity {
    pub cluster_id: String,
    pub version: ProtocolVersion,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompatibilityVerdict {
    Compatible,
    DifferentCluster,
    MajorMismatch {
        local: u16,
        remote: u16,
    },
    MinorTooFar {
        local: u16,
        remote: u16,
        max_skew: u16,
    },
}

pub fn check_compatibility(
    local: &ClusterIdentity,
    remote: &ClusterIdentity,
    minor_skew: u16,
) -> CompatibilityVerdict {
    if local.cluster_id != remote.cluster_id {
        return CompatibilityVerdict::DifferentCluster;
    }
    if local.version.major != remote.version.major {
        return CompatibilityVerdict::MajorMismatch {
            local: local.version.major,
            remote: remote.version.major,
        };
    }
    let local_minor = local.version.minor;
    let remote_minor = remote.version.minor;
    let skew = local_minor.abs_diff(remote_minor);
    if skew > minor_skew {
        return CompatibilityVerdict::MinorTooFar {
            local: local_minor,
            remote: remote_minor,
            max_skew: minor_skew,
        };
    }
    CompatibilityVerdict::Compatible
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(cluster: &str, major: u16, minor: u16) -> ClusterIdentity {
        ClusterIdentity {
            cluster_id: cluster.into(),
            version: ProtocolVersion::new(major, minor),
        }
    }

    #[test]
    fn same_cluster_and_version_is_compatible() {
        assert_eq!(
            check_compatibility(&id("c1", 1, 0), &id("c1", 1, 0), 0),
            CompatibilityVerdict::Compatible
        );
    }

    #[test]
    fn different_cluster_rejected() {
        assert_eq!(
            check_compatibility(&id("c1", 1, 0), &id("c2", 1, 0), 0),
            CompatibilityVerdict::DifferentCluster
        );
    }

    #[test]
    fn major_mismatch_rejected() {
        match check_compatibility(&id("c1", 1, 0), &id("c1", 2, 0), 0) {
            CompatibilityVerdict::MajorMismatch {
                local: 1,
                remote: 2,
            } => {}
            other => panic!("expected MajorMismatch, got {other:?}"),
        }
    }

    #[test]
    fn minor_within_skew_is_compatible() {
        assert_eq!(
            check_compatibility(&id("c1", 1, 5), &id("c1", 1, 3), 5),
            CompatibilityVerdict::Compatible
        );
    }

    #[test]
    fn minor_too_far_rejected() {
        match check_compatibility(&id("c1", 1, 0), &id("c1", 1, 10), 2) {
            CompatibilityVerdict::MinorTooFar { max_skew: 2, .. } => {}
            other => panic!("expected MinorTooFar, got {other:?}"),
        }
    }
}
