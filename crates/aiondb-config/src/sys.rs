//! System resource detection for auto-tuning.
//!
//! Reads total physical memory from `/proc/meminfo` on Linux. Returns `None`
//! on unsupported platforms or when the file cannot be parsed.

/// Return total physical memory in bytes, or `None` when detection fails.
pub fn total_system_memory() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        total_memory_linux()
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

#[cfg(target_os = "linux")]
fn total_memory_linux() -> Option<u64> {
    let contents = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in contents.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            let rest = rest.trim();
            // Typical format: "16384000 kB"
            let kb_str = rest
                .strip_suffix("kB")
                .or_else(|| rest.strip_suffix("KB"))?;
            let kb: u64 = kb_str.trim().parse().ok()?;
            return kb.checked_mul(1024);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn total_system_memory_returns_reasonable_value() {
        if cfg!(target_os = "linux") {
            let mem = total_system_memory();
            assert!(mem.is_some(), "should detect memory on Linux");
            let bytes = mem.unwrap();
            // At least 256 MiB and no more than 128 TiB.
            assert!(bytes >= 256 * 1024 * 1024, "too little: {bytes}");
            assert!(
                bytes <= 128 * 1024 * 1024 * 1024 * 1024u64,
                "too much: {bytes}"
            );
        }
    }
}
