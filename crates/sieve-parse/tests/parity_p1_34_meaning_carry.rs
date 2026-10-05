//! Pins the Tier-1 meaning-tier carry (P1-34 `stale` half, prerequisite
//! for P3-14): a rebuild must carry `summary`, `crux`, and
//! `summary_state` forward from the prior `wiring.json`, never drop them
//! (`crates/sieve-parse/src/build.rs`, `carry_meaning_layer`).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use sieve_core::{Crux, SummaryState};
use sieve_parse::{ensure_fresh_graph, rebuild_graph_only, RefreshOptions};

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
            std::env::temp_dir().join(format!("sieve-meaning-carry-{label}-{pid}-{n}-{nanos}"));
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
    manifest_dir().join("../../tests/fixtures/basic")
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

fn wiring_path(context_dir: &Path) -> PathBuf {
    context_dir.join(".graph").join("wiring.json")
}

fn read_wiring(context_dir: &Path) -> sieve_core::Graph {
    let text = fs::read_to_string(wiring_path(context_dir)).expect("read wiring.json");
    serde_json::from_str(&text).expect("parse wiring.json")
}

fn write_wiring(context_dir: &Path, graph: &sieve_core::Graph) {
    let path = wiring_path(context_dir);
    let text = serde_json::to_string_pretty(graph).expect("serialize wiring.json");
    fs::write(path, text).expect("write wiring.json");
}

/// Sets one node's meaning-tier fields in place, matching it by `id`.
fn set_meaning(
    graph: &mut sieve_core::Graph,
    id: &str,
    state: SummaryState,
    summary: &str,
    crux: Crux,
) {
    let node = graph
        .nodes
        .iter_mut()
        .find(|n| n.id == id)
        .unwrap_or_else(|| panic!("node {id} not found in wiring.json"));
    node.summary_state = state;
    node.summary = Some(summary.to_string());
    node.crux = Some(crux);
}

const DOUBLE_ID: &str = "src/util.ts#double";

#[test]
fn test_p1_34_a_rebuild_carries_the_summary_and_marks_a_changed_body_stale() {
    let dir = TempDir::new("carry");
    copy_tree(&fixture_root(), &dir.path);
    let context_dir = dir.path.join("sieve");

    // A first build: every node starts pending, with no summary.
    rebuild_graph_only(&dir.path, &context_dir).expect("first build");
    let mut graph = read_wiring(&context_dir);
    let old_hash = graph
        .nodes
        .iter()
        .find(|n| n.id == DOUBLE_ID)
        .expect("double node exists")
        .body_hash
        .clone();

    // Inject a prior "ready" summary and crux, matching the current hash.
    set_meaning(
        &mut graph,
        DOUBLE_ID,
        SummaryState::Ready,
        "doubles a number",
        Crux {
            code: "return n * 2;".to_string(),
            span: "L2-L2".to_string(),
        },
    );
    write_wiring(&context_dir, &graph);

    // A rebuild through the public build entry, with no source change: the
    // summary and crux must survive, and the state must stay "ready".
    rebuild_graph_only(&dir.path, &context_dir).expect("second build");
    let after = read_wiring(&context_dir);
    let node = after
        .nodes
        .iter()
        .find(|n| n.id == DOUBLE_ID)
        .expect("double node still exists");
    assert_eq!(node.body_hash, old_hash, "the source did not change");
    assert_eq!(node.summary_state, SummaryState::Ready);
    assert_eq!(node.summary.as_deref(), Some("doubles a number"));
    assert_eq!(
        node.crux.as_ref().map(|c| c.code.as_str()),
        Some("return n * 2;")
    );

    // Now edit the source file the node lives in, and refresh through the
    // query-path refresh: the changed body must mark the node "stale",
    // keeping the old summary and crux as a hint, never dropping them.
    let util_path = dir.path.join("src/util.ts");
    let original = fs::read_to_string(&util_path).expect("read util.ts");
    let edited = original.replace("return n * 2;", "return n * 20;");
    assert_ne!(
        original, edited,
        "the replacement must actually change the file"
    );
    fs::write(&util_path, edited).expect("edit util.ts");

    let outcome = ensure_fresh_graph(&dir.path, &context_dir, &RefreshOptions::default());
    assert!(
        outcome.refreshed,
        "the refresh must rebuild on a changed file"
    );

    let stale = read_wiring(&context_dir);
    let stale_node = stale
        .nodes
        .iter()
        .find(|n| n.id == DOUBLE_ID)
        .expect("double node exists after the edit");
    assert_ne!(
        stale_node.body_hash, old_hash,
        "the edited function must hash differently"
    );
    assert_eq!(stale_node.summary_state, SummaryState::Stale);
    assert_eq!(stale_node.summary.as_deref(), Some("doubles a number"));
    assert_eq!(
        stale_node.crux.as_ref().map(|c| c.code.as_str()),
        Some("return n * 2;"),
        "the stale node keeps its old crux, not a fresh one"
    );
}

#[test]
fn test_p1_34_a_new_node_with_no_prior_stays_pending() {
    let dir = TempDir::new("pending");
    copy_tree(&fixture_root(), &dir.path);
    let context_dir = dir.path.join("sieve");

    rebuild_graph_only(&dir.path, &context_dir).expect("first build");
    let graph = read_wiring(&context_dir);
    let node = graph
        .nodes
        .iter()
        .find(|n| n.id == DOUBLE_ID)
        .expect("double node exists");
    assert_eq!(node.summary_state, SummaryState::Pending);
    assert_eq!(node.summary, None);
    assert_eq!(node.crux, None);
}
