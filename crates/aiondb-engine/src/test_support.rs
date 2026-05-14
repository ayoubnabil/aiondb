use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn unique_temp_path(scope: &str, name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_nanos();
    let mut path = std::env::temp_dir();
    path.push(format!(
        "aiondb-{scope}-{name}-{}-{nanos}",
        std::process::id()
    ));
    path
}
