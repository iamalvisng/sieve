//! S2 (one copy of each node): the warm build keeps the cold build's bytes
//! and order after the nodes move out of the cache instead of cloning.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sieve_core::{write_graph, Kind};
use sieve_parse::{build_graph_cached, BuildReport};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A temp dir that removes itself on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("sieve-s2-{label}-{}-{n}", std::process::id()));
        fs::create_dir_all(&path).expect("create temp dir");
        TempDir(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Writes a small fixture. The walk visits `src/a/x.ts` before `src/a.ts`; a byte sort does not.
fn write_fixture(root: &Path) {
    fs::create_dir_all(root.join("src/a")).expect("mkdir");
    fs::write(
        root.join("src/a/x.ts"),
        "export function big(): number { return 1; }\n",
    )
    .expect("write x.ts");
    fs::write(
        root.join("src/a.ts"),
        "export function small(): number { return 2; }\n",
    )
    .expect("write a.ts");
    fs::write(
        root.join("src/m.ts"),
        "import { big } from './a/x';\nexport const m = big();\n",
    )
    .expect("write m.ts");
}

/// Builds, writes `wiring.json` the way the refresh does, returns its bytes.
fn build_and_write(root: &Path, context: &Path) -> (BuildReport, Vec<u8>) {
    let report = build_graph_cached(root, context).expect("build succeeds");
    let wiring = context.join(".graph/wiring.json");
    fs::create_dir_all(wiring.parent().expect("parent")).expect("mkdir .graph");
    write_graph(&report.graph, &wiring).expect("write wiring");
    let bytes = fs::read(&wiring).expect("read wiring");
    (report, bytes)
}

#[test]
fn test_s2_warm_wiring_equals_cold_wiring() {
    let dir = TempDir::new("warm");
    write_fixture(&dir.0);
    let context = dir.0.join("sieve");

    let (cold, cold_bytes) = build_and_write(&dir.0, &context);
    assert_eq!(cold.reused, 0);
    let (warm, warm_bytes) = build_and_write(&dir.0, &context);
    assert_eq!(warm.parsed, 0);
    assert_eq!(warm.reused, cold.parsed);
    assert_eq!(
        cold_bytes, warm_bytes,
        "an untouched warm build changes no byte"
    );

    let touched = dir.0.join("src/a.ts");
    let original = fs::read_to_string(&touched).expect("read a.ts");
    fs::write(&touched, format!("{original}// touched\n")).expect("touch a.ts");
    let (after, after_bytes) = build_and_write(&dir.0, &context);
    assert_eq!(after.parsed, 1);

    let fresh = TempDir::new("fresh");
    write_fixture(&fresh.0);
    fs::write(fresh.0.join("src/a.ts"), format!("{original}// touched\n")).expect("write a.ts");
    let (_, fresh_bytes) = build_and_write(&fresh.0, &fresh.0.join("sieve"));
    assert_eq!(
        after_bytes, fresh_bytes,
        "a one-file warm build equals a cold build"
    );
}

#[test]
fn test_s2_cards_keep_claimed_order() {
    let dir = TempDir::new("order");
    write_fixture(&dir.0);
    let context = dir.0.join("sieve");

    let cold = build_graph_cached(&dir.0, &context).expect("cold build");
    let warm = build_graph_cached(&dir.0, &context).expect("warm build");
    let mut sorted = cold.claimed.clone();
    sorted.sort();
    assert_ne!(
        cold.claimed, sorted,
        "the fixture walk order differs from map order"
    );

    assert_eq!(cold.claimed, warm.claimed);
    assert_eq!(cold.errors, warm.errors);
    let ids =
        |r: &BuildReport| -> Vec<String> { r.graph.nodes.iter().map(|n| n.id.clone()).collect() };
    assert_eq!(ids(&cold), ids(&warm), "the node list keeps its order");

    let file_ids: Vec<String> = cold
        .graph
        .nodes
        .iter()
        .filter(|n| n.kind == Kind::File)
        .map(|n| n.id.clone())
        .collect();
    let expected: Vec<String> = cold
        .claimed
        .iter()
        .filter(|id| file_ids.contains(id))
        .cloned()
        .collect();
    assert!(!expected.is_empty());
    assert_eq!(file_ids, expected, "the file nodes follow the walk order");
}

/// A claimed file the build cannot read gives an error entry and no node.
#[cfg(unix)]
#[test]
fn test_s2_unreadable_file_keeps_errors_and_adds_no_node() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new("broken");
    write_fixture(&dir.0);
    let broken = dir.0.join("src/broken.ts");
    fs::write(&broken, "export const b = 1;\n").expect("write broken.ts");
    fs::set_permissions(&broken, fs::Permissions::from_mode(0o000)).expect("chmod");
    let context = dir.0.join("sieve");

    let cold = build_graph_cached(&dir.0, &context).expect("cold build");
    let warm = build_graph_cached(&dir.0, &context).expect("warm build");
    assert!(!cold.errors.is_empty());
    assert_eq!(cold.errors, warm.errors);
    assert!(cold.claimed.iter().any(|id| id == "src/broken.ts"));
    assert!(
        cold.graph.nodes.iter().all(|n| !n.id.contains("broken.ts")),
        "an error entry adds no node"
    );
}
