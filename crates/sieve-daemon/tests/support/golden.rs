//! The golden bless switch for the daemon tests.
//!
//! If the environment variable `SIEVE_BLESS` is `1`, a test writes the
//! actual bytes to the golden file. Without the switch, a test compares.

#![allow(dead_code)]

use std::fs;
use std::path::Path;

/// True when `SIEVE_BLESS=1` asks the tests to write goldens.
pub fn blessing() -> bool {
    std::env::var("SIEVE_BLESS").is_ok_and(|v| v == "1")
}

/// With `SIEVE_BLESS=1`, writes `actual` to the golden file at `path`.
/// Without the switch, does nothing. The write goes through a temp file,
/// so a parallel test never reads a half-written golden.
pub fn bless(path: &Path, actual: &[u8]) {
    if !blessing() {
        return;
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create golden dir");
    }
    let tmp = path.with_extension(format!("bless-{}-tmp", std::process::id()));
    fs::write(&tmp, actual).expect("write golden file");
    fs::rename(&tmp, path).expect("move golden file");
}
