use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn unique_temp_path(scope: &str, name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let mut path = std::env::temp_dir();
    path.push(format!(
        "aiondb-catalog-{scope}-{name}-{}-{nanos}",
        std::process::id()
    ));
    path
}
