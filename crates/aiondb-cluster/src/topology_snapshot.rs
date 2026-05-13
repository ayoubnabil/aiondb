//! Durable cluster topology snapshot.
//!
//! Periodically captures membership + range assignments + lease
//! holdings to a single JSON blob. Used at node boot to recover
//! routing tables without replaying the full Raft log.

use std::path::{Path, PathBuf};

use aiondb_core::{DbError, DbResult};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TopologySnapshot {
    pub cluster_id: String,
    pub generation: u64,
    pub members: Vec<MemberRecord>,
    pub ranges: Vec<RangeRecord>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MemberRecord {
    pub node_id: u64,
    pub address: String,
    pub zone: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RangeRecord {
    pub range_id: u64,
    pub start_key: Vec<u8>,
    pub end_key: Vec<u8>,
    pub replicas: Vec<u64>,
    pub leaseholder: u64,
}

pub fn persist(snapshot: &TopologySnapshot, path: &Path) -> DbResult<()> {
    let json = serde_json::to_vec_pretty(snapshot)
        .map_err(|e| DbError::internal(format!("topology snapshot encode: {e}")))?;
    let tmp_path = path.with_extension("tmp");
    std::fs::write(&tmp_path, &json)
        .map_err(|e| DbError::internal(format!("topology snapshot write: {e}")))?;
    std::fs::rename(&tmp_path, path)
        .map_err(|e| DbError::internal(format!("topology snapshot rename: {e}")))?;
    Ok(())
}

pub fn load(path: &Path) -> DbResult<TopologySnapshot> {
    let bytes = std::fs::read(path)
        .map_err(|e| DbError::internal(format!("topology snapshot read: {e}")))?;
    serde_json::from_slice(&bytes)
        .map_err(|e| DbError::internal(format!("topology snapshot decode: {e}")))
}

pub fn ensure_parent_dir(path: &Path) -> DbResult<PathBuf> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| DbError::internal(format!("topology snapshot mkdir: {e}")))?;
    }
    Ok(path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_snapshot() -> TopologySnapshot {
        TopologySnapshot {
            cluster_id: "cluster-test".into(),
            generation: 1,
            members: vec![MemberRecord {
                node_id: 1,
                address: "host:1".into(),
                zone: "eu-west-1".into(),
            }],
            ranges: vec![RangeRecord {
                range_id: 1,
                start_key: b"a".to_vec(),
                end_key: b"m".to_vec(),
                replicas: vec![1, 2, 3],
                leaseholder: 1,
            }],
        }
    }

    #[test]
    fn persist_then_load_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("topology.json");
        let snap = sample_snapshot();
        persist(&snap, &path).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(snap, loaded);
    }

    #[test]
    fn persist_overwrites_existing_file_atomically() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("topology.json");
        let mut snap = sample_snapshot();
        persist(&snap, &path).unwrap();
        snap.generation = 99;
        persist(&snap, &path).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.generation, 99);
    }

    #[test]
    fn ensure_parent_dir_creates_missing_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a/b/c/topology.json");
        let resolved = ensure_parent_dir(&path).unwrap();
        assert_eq!(resolved, path);
        assert!(tmp.path().join("a/b/c").exists());
    }

    #[test]
    fn load_missing_file_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nope.json");
        assert!(load(&path).is_err());
    }
}
