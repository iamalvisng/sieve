//! Pins the Sieve name set for every runtime-visible string this crate
//! builds.
//!
//! `sieve_core::product` reads a process-wide `OnceLock`. Cargo runs
//! every `#[test]` in one file's binary on its own thread, so two tests
//! that each call `set_for_tests` race: whichever thread's call wins
//! sets the product for the whole process. `pin_sieve` calls it and
//! asserts the win, so a lost race (or a stray `product()` read that ran
//! first) fails loudly at the first line, not as a confusing text
//! mismatch deep in a test body.

use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use sieve_parse::{check_graph, format_graph_check_report};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir {
    path: std::path::PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("system clock before epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("sieve-product-{label}-{pid}-{n}-{nanos}"));
        fs::create_dir_all(&path).expect("create temp dir");
        TempDir { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Pins `SIEVE` and asserts `product()` reads it back, so a lost
/// `OnceLock` race fails at this line instead of inside a test body.
fn pin_sieve() {
    assert_eq!(
        sieve_core::product().name,
        "sieve",
        "product() did not read back SIEVE after set_for_tests"
    );
}

#[test]
fn rename_table_parse_check_error_names_sieve_graph_path() {
    pin_sieve();

    let dir = TempDir::new("no-graph");
    let context_dir = dir.path.join("sieve");

    let check = check_graph(&dir.path, &context_dir).expect("check_graph should not error");
    let report = format_graph_check_report(&check);

    assert_eq!(report, "no index here yet \u{2014} run sieve build .");
}

#[test]
fn rename_table_parse_no_refresh_env_reads_sieve_prefix() {
    pin_sieve();

    assert_eq!(
        sieve_core::product().env_var("NO_REFRESH"),
        "SIEVE_NO_REFRESH"
    );

    // `set_var` here mutates the whole test process's environment. This
    // test is the only one in this file that reads an env var, so no
    // other test races this value.
    std::env::set_var("SIEVE_NO_REFRESH", "1");
    assert!(sieve_parse::env_truthy("SIEVE_NO_REFRESH"));
    std::env::remove_var("SIEVE_NO_REFRESH");
}
