//! Build memory guard tests. These set `SIEVE_BUILD_CEILING_BYTES`, so
//! they live in their own test binary and hold one lock.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use sieve_parse::cache::cache_path;
use sieve_parse::guard::CEILING_ENV;
use sieve_parse::{build_graph_cached, extractor_stamp, rebuild_graph_only};

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Sets the ceiling env var and removes it on drop, so a failed assertion
/// does not leak it.
struct CeilingGuard;

impl CeilingGuard {
    fn set(value: &str) -> Self {
        std::env::set_var(CEILING_ENV, value);
        CeilingGuard
    }
}

impl Drop for CeilingGuard {
    fn drop(&mut self) {
        std::env::remove_var(CEILING_ENV);
    }
}

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

/// A ceiling of 1 byte stops the build. Only the marker file appears in the
/// context dir. A refresh then stops at once on the marker, and a manual
/// build under a higher ceiling clears it.
#[test]
fn test_s0_guard_stops_marks_and_clears() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let root = small_repo("stop");
    let context = root.join("sieve");
    let marker = context.join("build-stopped");

    let ceiling = CeilingGuard::set("1");
    let result = build_graph_cached(&root, &context);
    let err = result.err().expect("the build is stopped");
    assert!(err.to_string().contains("build stopped"), "{err}");
    assert!(err.to_string().contains("--only-dir"), "{err}");
    assert_eq!(
        list_files(&context),
        vec![marker.clone()],
        "only the marker"
    );

    // The refresh path stops on the marker and writes nothing more.
    let err = rebuild_graph_only(&root, &context).expect_err("the refresh is stopped");
    assert!(err.to_string().contains("build stopped"), "{err}");
    assert_eq!(
        list_files(&context),
        vec![marker.clone()],
        "only the marker"
    );

    // A raised ceiling lets the refresh run, and the manual build clears the marker.
    drop(ceiling);
    let _ceiling = CeilingGuard::set(&u64::MAX.to_string());
    build_graph_cached(&root, &context).expect("the build passes");
    assert!(!marker.exists(), "the marker stays");
    assert!(cache_path(&context, &extractor_stamp()).exists());
    let _ = fs::remove_dir_all(&root);
}
