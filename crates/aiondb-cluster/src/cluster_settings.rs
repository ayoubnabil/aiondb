//! Cluster setting registry.
//!
//! Typed runtime knobs (durations, ints, booleans, strings) that
//! propagate through the control plane and trigger
//! [`crate::pubsub`] notifications.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use aiondb_core::{DbError, DbResult};

#[derive(Clone, Debug, PartialEq)]
pub enum SettingValue {
    Bool(bool),
    Int(i64),
    Duration(Duration),
    Text(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct SettingEntry {
    pub key: String,
    pub value: SettingValue,
    pub version: u64,
}

#[derive(Clone, Debug, Default)]
pub struct ClusterSettings {
    inner: Arc<std::sync::RwLock<BTreeMap<String, SettingEntry>>>,
}

impl ClusterSettings {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, key: impl Into<String>, value: SettingValue) -> u64 {
        let key = key.into();
        let mut guard = self.inner.write().unwrap();
        let prev_version = guard.get(&key).map(|e| e.version).unwrap_or(0);
        let version = prev_version.saturating_add(1);
        guard.insert(
            key.clone(),
            SettingEntry {
                key,
                value,
                version,
            },
        );
        version
    }

    pub fn get(&self, key: &str) -> Option<SettingEntry> {
        self.inner.read().unwrap().get(key).cloned()
    }

    pub fn as_bool(&self, key: &str) -> DbResult<bool> {
        match self.get(key).map(|e| e.value) {
            Some(SettingValue::Bool(b)) => Ok(b),
            Some(other) => Err(DbError::internal(format!(
                "setting {key} is not a bool: {other:?}"
            ))),
            None => Err(DbError::internal(format!("setting {key} not found"))),
        }
    }

    pub fn as_duration(&self, key: &str) -> DbResult<Duration> {
        match self.get(key).map(|e| e.value) {
            Some(SettingValue::Duration(d)) => Ok(d),
            Some(other) => Err(DbError::internal(format!(
                "setting {key} is not a duration: {other:?}"
            ))),
            None => Err(DbError::internal(format!("setting {key} not found"))),
        }
    }

    pub fn snapshot(&self) -> Vec<SettingEntry> {
        self.inner.read().unwrap().values().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_then_get_returns_value() {
        let s = ClusterSettings::new();
        s.set("max_open_connections", SettingValue::Int(100));
        let e = s.get("max_open_connections").unwrap();
        assert_eq!(e.value, SettingValue::Int(100));
        assert_eq!(e.version, 1);
    }

    #[test]
    fn set_advances_version() {
        let s = ClusterSettings::new();
        let v1 = s.set("k", SettingValue::Bool(true));
        let v2 = s.set("k", SettingValue::Bool(false));
        assert!(v2 > v1);
    }

    #[test]
    fn typed_accessors_validate_type() {
        let s = ClusterSettings::new();
        s.set("flag", SettingValue::Bool(true));
        assert!(s.as_bool("flag").unwrap());
        assert!(s.as_duration("flag").is_err());
    }

    #[test]
    fn missing_key_errors() {
        let s = ClusterSettings::new();
        assert!(s.as_bool("nope").is_err());
    }

    #[test]
    fn snapshot_lists_every_entry() {
        let s = ClusterSettings::new();
        s.set("a", SettingValue::Int(1));
        s.set("b", SettingValue::Text("hi".into()));
        let snap = s.snapshot();
        assert_eq!(snap.len(), 2);
    }
}
