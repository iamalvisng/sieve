//! Shared test-only helpers for `sieve-core`'s unit tests.

#![cfg(test)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Reads the crate's manifest dir at run time.
///
/// Cargo sets `CARGO_MANIFEST_DIR` at run time for every test process.
/// A run-time read survives a worktree that moves after a build baked
/// the dir in at compile time through `env!`.
pub(crate) fn manifest_dir() -> PathBuf {
    PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
}

/// A unique temp dir under the system temp dir. It creates the dir on
/// `new`, and removes it on drop, even if the test panics first.
pub(crate) struct TempDir(PathBuf);

impl TempDir {
    /// Creates a fresh, empty temp dir tagged with `label`.
    pub(crate) fn new(label: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .expect("system clock before epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("sieve-core-{label}-{pid}-{n}-{nanos}"));
        std::fs::create_dir_all(&path).expect("create temp dir");
        TempDir(path)
    }
}

impl std::ops::Deref for TempDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
