//! S0 build memory guard tests. These set `SIEVE_BUILD_CEILING_BYTES`, so
//! they live in their own test binary and hold one lock.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use sieve_parse::cache::cache_path;
use sieve_parse::guard::{projected_peak_bytes, CEILING_ENV};
use sieve_parse::{build_graph_cached, extractor_stamp, rebuild_graph_only};

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Makes a one-file repo in a unique temp dir.
fn small_repo(label: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("sieve-s0-{label}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&dir).expect("create temp dir");
    fs::write(
        dir.join("a.ts"),
        "export function a(): number { return 1; }\n",
    )
    .expect("write a.ts");
    dir
}

/// Lists every file under `dir`, recursively.
fn list_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(list_files(&p));
            } else {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

#[test]
fn test_s0_guard_refuses_at_one_megabyte_ceiling() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let root = small_repo("refuse");
    let context = root.join("sieve");
    std::env::set_var(CEILING_ENV, "1000000");
    let result = build_graph_cached(&root, &context);
    std::env::remove_var(CEILING_ENV);
    let err = result.err().expect("the build is refused");
    assert!(err.to_string().contains("build refused"), "{err}");
    assert!(err.to_string().contains("--only-dir"), "{err}");
    assert!(err.to_string().contains(" — "), "{err}");
    assert!(list_files(&context).is_empty(), "sieve/ holds a new file");
    let _ = fs::remove_dir_all(&root);
}

/// Proves a refused warm rebuild returns the refusal error and leaves the
/// cache file as it was. It does not prove the check order: `read_cache`
/// returns an empty cache on garbage, so a late guard would pass this too.
#[test]
fn test_s0_rebuild_refuses_and_writes_nothing() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let root = small_repo("rebuild");
    let context = root.join("sieve");
    std::env::remove_var(CEILING_ENV);
    rebuild_graph_only(&root, &context).expect("first rebuild writes the fingerprint");
    let cache = cache_path(&context, &extractor_stamp());
    fs::write(&cache, "not json {{{").expect("write garbage cache");
    let before = fs::read(&cache).expect("read garbage");
    std::env::set_var(CEILING_ENV, "1000000");
    let result = rebuild_graph_only(&root, &context);
    std::env::remove_var(CEILING_ENV);
    let err = result.expect_err("the rebuild is refused");
    assert!(err.to_string().contains("build refused"), "{err}");
    assert_eq!(fs::read(&cache).expect("read cache"), before);
    let _ = fs::remove_dir_all(&root);
}

/// Proves the build path projects its peak from claimed files only. Ten
/// `.json` files are not claimed, so they add nothing to the count.
#[test]
fn test_s0_guard_build_path_counts_only_claimed_files() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let root = small_repo("claimed");
    for i in 0..10 {
        fs::write(
            root.join(format!("f{i}.ts")),
            format!("export function f{i}(): number {{ return {i}; }}\n"),
        )
        .expect("write ts file");
        fs::write(root.join(format!("d{i}.json")), "{}\n").expect("write json file");
    }
    let context = root.join("sieve");
    let ten = projected_peak_bytes(10, false);
    let twenty = projected_peak_bytes(20, false);
    std::env::set_var(CEILING_ENV, ((ten + twenty) / 2).to_string());
    let ok = build_graph_cached(&root, &context);
    std::env::remove_var(CEILING_ENV);
    ok.expect("the build passes under the midpoint ceiling");
    assert!(
        cache_path(&context, &extractor_stamp()).exists(),
        "the extract cache is missing"
    );
    fs::remove_dir_all(&context).expect("delete sieve/");
    std::env::set_var(CEILING_ENV, (ten - 1).to_string());
    let refused = build_graph_cached(&root, &context);
    std::env::remove_var(CEILING_ENV);
    let err = refused.err().expect("the build is refused");
    assert!(err.to_string().contains("build refused"), "{err}");
    let _ = fs::remove_dir_all(&root);
}
