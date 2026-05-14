//! Bridge between per-shard counters and a flat metric registry.
//!
//! Each shard records its own counters (writes, reads, raft proposals).
//! The bridge folds them into a global registry with per-label
//! cardinality control. The Prometheus scrape endpoint then sees a
//! single flat namespace instead of needing to walk every shard.

use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct MetricSnapshot {
    pub name: String,
    pub labels: BTreeMap<String, String>,
    pub value: f64,
}

#[derive(Clone, Debug, Default)]
pub struct DistMetricBridge {
    inner: Arc<std::sync::Mutex<BridgeState>>,
}

#[derive(Default, Debug)]
struct BridgeState {
    series: BTreeMap<MetricKey, f64>,
    max_cardinality: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct MetricKey {
    name: String,
    labels: Vec<(String, String)>,
}

impl DistMetricBridge {
    pub fn new(max_cardinality: usize) -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(BridgeState {
                series: BTreeMap::new(),
                max_cardinality: max_cardinality.max(1),
            })),
        }
    }

    pub fn set(&self, name: &str, labels: BTreeMap<String, String>, value: f64) -> bool {
        let mut g = self.inner.lock().unwrap();
        let key = MetricKey {
            name: name.to_string(),
            labels: labels.into_iter().collect(),
        };
        if !g.series.contains_key(&key) && g.series.len() >= g.max_cardinality {
            return false;
        }
        g.series.insert(key, value);
        true
    }

    pub fn add(&self, name: &str, labels: BTreeMap<String, String>, delta: f64) -> bool {
        let mut g = self.inner.lock().unwrap();
        let key = MetricKey {
            name: name.to_string(),
            labels: labels.into_iter().collect(),
        };
        if !g.series.contains_key(&key) && g.series.len() >= g.max_cardinality {
            return false;
        }
        *g.series.entry(key).or_insert(0.0) += delta;
        true
    }

    pub fn snapshot(&self) -> Vec<MetricSnapshot> {
        let g = self.inner.lock().unwrap();
        g.series
            .iter()
            .map(|(k, v)| MetricSnapshot {
                name: k.name.clone(),
                labels: k.labels.iter().cloned().collect(),
                value: *v,
            })
            .collect()
    }

    pub fn series_count(&self) -> usize {
        self.inner.lock().unwrap().series.len()
    }

    pub fn render_prometheus(&self) -> String {
        let snap = self.snapshot();
        let mut out = String::new();
        for m in snap {
            let labels = if m.labels.is_empty() {
                String::new()
            } else {
                let parts: Vec<String> = m
                    .labels
                    .into_iter()
                    .map(|(k, v)| format!("{k}=\"{v}\""))
                    .collect();
                format!("{{{}}}", parts.join(","))
            };
            out.push_str(&format!("{}{labels} {}\n", m.name, m.value));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lbls(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn set_overwrites_value() {
        let b = DistMetricBridge::new(100);
        b.set("raft_proposals", lbls(&[("shard", "1")]), 10.0);
        b.set("raft_proposals", lbls(&[("shard", "1")]), 20.0);
        assert_eq!(b.series_count(), 1);
        assert_eq!(b.snapshot()[0].value, 20.0);
    }

    #[test]
    fn add_accumulates() {
        let b = DistMetricBridge::new(100);
        b.add("writes", lbls(&[("table", "t1")]), 5.0);
        b.add("writes", lbls(&[("table", "t1")]), 3.0);
        assert_eq!(b.snapshot()[0].value, 8.0);
    }

    #[test]
    fn distinct_labels_make_distinct_series() {
        let b = DistMetricBridge::new(100);
        b.set("reads", lbls(&[("shard", "1")]), 1.0);
        b.set("reads", lbls(&[("shard", "2")]), 1.0);
        assert_eq!(b.series_count(), 2);
    }

    #[test]
    fn cardinality_cap_rejects_new_series() {
        let b = DistMetricBridge::new(2);
        assert!(b.set("a", lbls(&[("k", "1")]), 1.0));
        assert!(b.set("a", lbls(&[("k", "2")]), 1.0));
        assert!(!b.set("a", lbls(&[("k", "3")]), 1.0));
        // But existing series can still be updated.
        assert!(b.set("a", lbls(&[("k", "1")]), 99.0));
    }

    #[test]
    fn render_prometheus_includes_labels() {
        let b = DistMetricBridge::new(100);
        b.set("metric_a", lbls(&[("k", "v")]), 42.0);
        let s = b.render_prometheus();
        assert!(s.contains("metric_a"));
        assert!(s.contains("k=\"v\""));
        assert!(s.contains("42"));
    }

    #[test]
    fn render_empty_metric_has_no_label_block() {
        let b = DistMetricBridge::new(100);
        b.set("metric_a", BTreeMap::new(), 1.0);
        let s = b.render_prometheus();
        assert!(s.starts_with("metric_a 1"));
    }
}
