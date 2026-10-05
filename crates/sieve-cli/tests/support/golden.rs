//! The golden compare with a bless switch.
//!
//! A test compares the actual bytes of a run with a golden file. If the
//! environment variable `SIEVE_BLESS` is `1`, the compare writes the actual
//! bytes to the golden file and passes. Without the switch, a mismatch
//! fails the test.

#![allow(dead_code)]

use std::fs;
use std::path::Path;

/// True when `SIEVE_BLESS=1` asks the compare helpers to write goldens.
pub fn blessing() -> bool {
    std::env::var("SIEVE_BLESS").is_ok_and(|v| v == "1")
}

/// Writes `actual` to the golden file at `path`, and makes the parent dirs.
/// The write goes to a temp file first, so a parallel test never reads a
/// half-written golden.
pub fn bless(path: &Path, actual: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create golden dir");
    }
    let tmp = path.with_extension(format!("bless-{}-tmp", std::process::id()));
    fs::write(&tmp, actual).expect("write golden file");
    fs::rename(&tmp, path).expect("move golden file");
}

/// Compares `actual` with the golden file at `path`. With `SIEVE_BLESS=1`
/// it writes `actual` to the file and passes.
pub fn assert_golden(path: &Path, actual: &[u8], what: &str) {
    if blessing() {
        bless(path, actual);
        return;
    }
    let want = fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    assert!(
        actual == want.as_slice(),
        "{what}: differs from {}\n--- actual\n{}\n--- golden\n{}",
        path.display(),
        String::from_utf8_lossy(actual),
        String::from_utf8_lossy(&want)
    );
}

/// [`assert_golden`] for text.
pub fn assert_golden_text(path: &Path, actual: &str, what: &str) {
    assert_golden(path, actual.as_bytes(), what);
}

/// With `SIEVE_BLESS=1`, writes the golden triple of one case under `dir`:
/// `<id>.stdout.txt`, `<id>.stderr.txt` and `<id>.exit.txt`.
pub fn bless_triple(dir: &Path, id: &str, stdout: &[u8], stderr: &[u8], exit: i32) {
    if !blessing() {
        return;
    }
    bless(&dir.join(format!("{id}.stdout.txt")), stdout);
    bless(&dir.join(format!("{id}.stderr.txt")), stderr);
    bless(
        &dir.join(format!("{id}.exit.txt")),
        format!("{exit}\n").as_bytes(),
    );
}

/// With `SIEVE_BLESS=1`, writes `actual` to the golden file at `path`.
/// Without the switch, does nothing.
pub fn bless_text_if_blessing(path: &Path, actual: &str) {
    if blessing() {
        bless(path, actual.as_bytes());
    }
}

/// With `SIEVE_BLESS=1`, writes `actual` to the golden file at `path`, or
/// removes the file when `actual` is `None`. Without the switch, does
/// nothing.
pub fn bless_optional(path: &Path, actual: Option<&str>) {
    if !blessing() {
        return;
    }
    match actual {
        Some(text) => bless(path, text.as_bytes()),
        None => {
            let _ = fs::remove_file(path);
        }
    }
}

/// With `SIEVE_BLESS=1`, writes `actual` bytes to the golden file at
/// `path`. Without the switch, does nothing.
pub fn bless_if_blessing(path: &Path, actual: &[u8]) {
    if blessing() {
        bless(path, actual);
    }
}
