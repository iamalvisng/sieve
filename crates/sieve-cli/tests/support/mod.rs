//! Shared support code for `sieve-cli`'s integration tests.
//!
//! Every test file that copies a fixture into a scratch dir uses
//! `TempDir` for that scratch dir, so the dir removes itself on drop,
//! pass or fail. Each test file adds `mod support;` and pulls in
//! `support::TempDir`.

pub mod golden;
pub mod ws;

use std::fs;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

/// Builds a `Command` for the `sieve` binary. `PATH` starts with the dir of
/// that binary, so `init` finds `sieve` on a machine with no installed copy.
#[allow(dead_code)]
pub fn sieve_command() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sieve"));
    cmd.env("SIEVE_TEST_NOW", TEST_NOW)
        .env("PATH", sieve_path());
    cmd
}

/// The `PATH` value that holds the dir of the test `sieve` binary first,
/// then the old `PATH`.
#[allow(dead_code)]
pub fn sieve_path() -> std::ffi::OsString {
    let bin = Path::new(env!("CARGO_BIN_EXE_sieve"));
    let mut dirs: Vec<PathBuf> = bin.parent().map(Path::to_path_buf).into_iter().collect();
    dirs.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    std::env::join_paths(dirs).expect("join PATH")
}

/// The fixed clock of every golden run: 2026-10-05T12:00:00Z. A golden
/// holds dates, so it must not depend on the day the test runs.
pub const TEST_NOW: &str = "1791201600";

/// The page's data block: the text from `<script>window.` up to the first
/// `</script>`. The viewer files are original Sieve code, so a page is
/// compared with its golden by this block alone.
#[allow(dead_code)]
pub fn data_block(page: &[u8]) -> String {
    let text = String::from_utf8_lossy(page);
    let start = text.find("<script>window.").expect("data block start");
    let end = start + text[start..].find("</script>").expect("data block end");
    text[start..end].to_string()
}

/// Replaces each `(N kB` page size with `(<KB> kB`. The size depends on the
/// viewer files.
#[allow(dead_code)]
pub fn mask_kb(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find(" kB") {
        let head = &rest[..i];
        let digits = head
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_digit())
            .count();
        out.push_str(&head[..head.len() - digits]);
        out.push_str("<KB>");
        out.push_str(" kB");
        rest = &rest[i + 3..];
    }
    out.push_str(rest);
    out
}

/// Reads the crate's manifest dir at run time.
///
/// Cargo sets `CARGO_MANIFEST_DIR` at run time for every test process.
/// A run-time read survives a worktree that moves after a build baked
/// the dir in at compile time through `env!`.
#[allow(dead_code)]
pub fn manifest_dir() -> PathBuf {
    PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A temp dir that removes itself on drop, even on a test panic.
///
/// `path` stays public so a caller can build a nested path with
/// `temp.path.join(...)`. `TempDir` also derefs to `Path`, so a caller
/// can pass `&temp` anywhere a `&Path` fits.
#[allow(dead_code)]
pub struct TempDir {
    pub path: PathBuf,
}

impl TempDir {
    /// Creates a fresh temp dir under the OS temp root, named
    /// `sieve-cli-<label>-<pid>-<n>` so parallel test runs never
    /// collide on the same path.
    #[allow(dead_code)]
    pub fn new(label: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("sieve-cli-{label}-{pid}-{n}"));
        fs::create_dir_all(&path).expect("create temp dir");
        TempDir { path }
    }
}

impl Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
