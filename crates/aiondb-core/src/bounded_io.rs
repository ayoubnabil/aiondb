//! Bounded file reading helpers shared by callers that must refuse to load
//! oversized inputs into memory.
//!
//! Both `aiondb-pgwire` and `aiondb-fragment-transport` need to read PEM
//! certificate / key files at startup. Each path is operator-supplied and
//! could point at an arbitrarily large file by accident, so the reader
//! enforces a hard byte cap *during* the read (not just from the metadata
//! length) to defend against TOCTOU growth and against operators who hand
//! the server a 4 GiB file by mistake.
//!
//! The previous copy-pasted version lived in
//! `aiondb-pgwire::tls::read_tls_pem_file` and
//! `aiondb-fragment-transport::tls::read_tls_pem_file`. They had identical
//! bodies and identical bug surface; consolidating here removes the
//! duplication.

use std::fs::File;
use std::io::{self, Read as _};
use std::path::Path;

/// Read an entire file into memory, refusing to accept inputs larger than
/// `max_bytes`. The `kind` label is interpolated into error messages so the
/// caller can distinguish "cert" vs "key" vs "client CA cert" failures
/// without writing four near-identical wrappers.
///
/// Returns an [`io::ErrorKind::InvalidData`] error if either the file
/// metadata or the actual bytes read exceed `max_bytes`. The dual check
/// closes a TOCTOU window where the file is rewritten between metadata
/// inspection and read.
///
/// # Errors
///
/// Propagates underlying filesystem errors and emits `InvalidData` for any
/// length violation.
pub fn read_file_capped(path: &str, kind: &str, max_bytes: u64) -> io::Result<Vec<u8>> {
    let file = File::open(path).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("cannot read {kind} '{path}': {e}"),
        )
    })?;
    let metadata = file.metadata().map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("cannot inspect {kind} '{path}': {e}"),
        )
    })?;
    if metadata.len() > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{kind} '{path}' exceeds maximum {max_bytes} bytes"),
        ));
    }

    let mut buf = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    let mut reader = file.take(max_bytes.saturating_add(1));
    reader.read_to_end(&mut buf).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("cannot read {kind} '{path}': {e}"),
        )
    })?;
    if u64::try_from(buf.len()).unwrap_or(u64::MAX) > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{kind} '{path}' grew beyond maximum {max_bytes} bytes"),
        ));
    }

    Ok(buf)
}

/// fsync a directory so any rename or unlink that landed inside it is durable
/// across crashes. On non-Unix platforms this is a no-op because directory
/// fsync is not exposed through the standard filesystem API and the platform
/// usually persists metadata transactionally on a different schedule.
///
/// Returns the underlying `io::Error` so callers can wrap it in whatever
/// error type they use. Adding the path to the message at the call site keeps
/// this helper free of any error-domain dependency.
///
/// # Errors
///
/// Propagates errors from opening or syncing the directory.
#[cfg(unix)]
pub fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

#[cfg(not(unix))]
#[allow(clippy::needless_pass_by_value)]
pub fn sync_dir(_dir: &Path) -> io::Result<()> {
    Ok(())
}

/// fsync the parent directory of `path`. Convenience wrapper used after a
/// `rename` or `remove_file` so the directory entry change becomes durable.
/// Treats an empty parent (i.e. `path` is just a filename) as the current
/// directory.
///
/// # Errors
///
/// Propagates errors from opening or syncing the parent directory.
pub fn sync_parent_dir(path: &Path) -> io::Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    sync_dir(parent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::fs::OpenOptions;
    use std::path::PathBuf;

    fn unique_tmp(prefix: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let pid = std::process::id();
        let path = env::temp_dir().join(format!("aiondb-bounded-io-{prefix}-{pid}-{nanos}"));
        std::fs::create_dir_all(&path).expect("temp dir creation");
        path
    }

    #[test]
    fn rejects_oversized_file() {
        let dir = unique_tmp("oversized");
        let file_path = dir.join("big.bin");
        let f = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&file_path)
            .expect("create");
        f.set_len(1024 + 1).expect("set_len");
        drop(f);

        let err = read_file_capped(file_path.to_str().unwrap(), "test file", 1024).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("exceeds maximum"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reads_small_file() {
        let dir = unique_tmp("small");
        let file_path = dir.join("ok.bin");
        std::fs::write(&file_path, b"hello").expect("write");

        let bytes = read_file_capped(file_path.to_str().unwrap(), "test file", 1024).expect("read");
        assert_eq!(bytes, b"hello");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reports_missing_file_path() {
        let err = read_file_capped("/does/not/exist/foo.pem", "cert", 1024).unwrap_err();
        assert!(err.to_string().contains("/does/not/exist/foo.pem"));
    }
}
