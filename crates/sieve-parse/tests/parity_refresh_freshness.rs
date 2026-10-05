//! Pins the pre-query refresh against a repo whose claimed files include
//! the breadth tier (P1-46, P2-35). `tests/fixtures/langs` claims most of
//! its files there: `.rs`, `.c`, `.cpp`, `.rb`, `.cs`, `.scala`, `.ex`,
//! `.sol`, `.ml`, `.zig`, `.dart`, `.nix`, `.lua`, `.clj` all match only
//! `generic_lang_of`, never the native tier.
//!
//! Before the fix, [`sieve_core::fingerprint::probe_drift`] checked only
//! the native tier, so every breadth-tier file in the fingerprint read as
//! `removed` on every call. That forced a graph-only rebuild on every
//! single query, forever, on a clean tree.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use sieve_parse::{
    build_graph_cached, ensure_fresh_graph, rebuild_graph_only, RefreshOptions, RefreshOutcome,
};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("system clock before epoch")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("sieve-refresh-freshness-{label}-{pid}-{n}-{nanos}"));
        fs::create_dir_all(&path).expect("create temp dir");
        TempDir { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn manifest_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
}

fn fixture_root() -> PathBuf {
    manifest_dir().join("../../tests/fixtures/langs")
}

/// Copies every file under `src` into `dst`, recreating each subdirectory.
fn copy_tree(src: &Path, dst: &Path) {
    for entry in fs::read_dir(src).expect("read fixture dir") {
        let entry = entry.expect("read fixture entry");
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            fs::create_dir_all(&to).expect("create fixture subdir");
            copy_tree(&from, &to);
        } else {
            fs::copy(&from, &to).expect("copy fixture file");
        }
    }
}

/// Sets every file's modified time under `dir` to the same whole-second
/// value.
///
/// `fs::copy` on an APFS volume can clone a file, which carries over the
/// source's sub-millisecond mtime. A value that close to a whole
/// millisecond boundary can round to two different `f64` values a few
/// nanoseconds apart, on two separate stat calls, which is a pre-existing,
/// unrelated flake in the extraction cache's own stat comparison. Pinning
/// every mtime to a whole second, with no fractional part, keeps this
/// test about the fingerprint drift bug alone.
fn normalize_mtimes(dir: &Path) {
    let stamp = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    for entry in fs::read_dir(dir).expect("read dir to normalize") {
        let path = entry.expect("read dir entry to normalize").path();
        if path.is_dir() {
            normalize_mtimes(&path);
        } else {
            let file = fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .expect("open file to normalize mtime");
            file.set_modified(stamp).expect("set normalized mtime");
        }
    }
}

/// A build, then one query, on a fixture whose files are mostly claimed
/// only by the breadth tier: the query must not refresh the graph, and a
/// second build must replay every claimed file from the cache.
#[test]
fn test_p1_46_a_query_after_a_build_does_not_refresh_on_breadth_files() {
    let dir = TempDir::new("langs");
    copy_tree(&fixture_root(), &dir.path);
    normalize_mtimes(&dir.path);
    let context_dir = dir.path.join("sieve");

    // "sieve build": writes the graph and the fingerprint, the same as
    // the pre-query refresh does on a cold context dir.
    let first = rebuild_graph_only(&dir.path, &context_dir).expect("first build");
    assert!(first.files > 0, "the langs fixture should claim files");

    // "sieve grep ..." (the query prelude's pre-query refresh): a clean
    // tree must not rebuild, and must carry no refresh note.
    let outcome = ensure_fresh_graph(&dir.path, &context_dir, &RefreshOptions::default());
    assert_eq!(
        outcome,
        RefreshOutcome::default(),
        "a query right after a build must not refresh the graph"
    );

    // A second build must replay every claimed file from the extraction
    // cache: nothing changed on disk since the first build.
    let second = build_graph_cached(&dir.path, &context_dir).expect("second build");
    assert_eq!(second.parsed, 0, "a second build should parse nothing");
    assert_eq!(
        second.reused, second.files,
        "a second build should replay every claimed file from the cache"
    );
}
