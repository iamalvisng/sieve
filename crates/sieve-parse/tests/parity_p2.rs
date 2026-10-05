//! Parity tests for `build_graph` (P2), pinned against the golden
//! `wiring.json` recorded for `tests/fixtures/basic`.
//!
//! Only `kind:"file"` nodes and `meta` are in scope. Symbol nodes and
//! edges come with a later parity task.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use sieve_core::{write_graph, Confidence, Edge, Graph, Kind, Node};
use sieve_parse::{build_graph, build_graph_cached};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Reads the crate's manifest dir at run time, so the path stays valid
/// even when a build baked it in from a worktree that is now gone.
fn manifest_dir() -> PathBuf {
    PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
}

/// Builds a temp dir path that is unique per process, per call, and per
/// nanosecond, so two concurrent `cargo test` runs never collide.
fn unique_temp_dir(label: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .expect("system clock before epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("sieve-parity-p2-{label}-{pid}-{n}-{nanos}"))
}

fn fixture_root() -> PathBuf {
    fixture_path("basic")
}

fn fixture_path(name: &str) -> PathBuf {
    manifest_dir().join(format!("../../tests/fixtures/{name}"))
}

fn golden_graph() -> Graph {
    golden_graph_for("basic")
}

fn golden_graph_for(name: &str) -> Graph {
    let path = manifest_dir().join(format!("../../tests/fixtures/{name}.expected/wiring.json"));
    let text = fs::read_to_string(&path).expect("read golden wiring.json");
    serde_json::from_str(&text).expect("golden wiring.json deserializes as Graph")
}

fn golden_file_nodes() -> Vec<Node> {
    golden_graph()
        .nodes
        .into_iter()
        .filter(|node| node.kind == Kind::File)
        .collect()
}

fn built_file_nodes() -> Vec<Node> {
    build_graph(&fixture_root(), &fixture_root().join("sieve"))
        .expect("build_graph should not error")
        .nodes
        .into_iter()
        .filter(|node| node.kind == Kind::File)
        .collect()
}

/// Strips `body_text`, because no golden pins it (spec section 8).
fn strip_body_text(mut node: Node) -> Node {
    node.body_text = None;
    node
}

fn sorted_nodes(mut nodes: Vec<Node>) -> Vec<Node> {
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    nodes.into_iter().map(strip_body_text).collect()
}

/// Asserts the sorted built node list equals the sorted golden node list,
/// field for field. On a mismatch, prints the first differing id.
fn assert_nodes_match_golden(fixture: &str) {
    let built = sorted_nodes(
        build_graph(&fixture_path(fixture), &fixture_path(fixture).join("sieve"))
            .expect("build_graph should not error")
            .nodes,
    );
    let golden = sorted_nodes(golden_graph_for(fixture).nodes);
    if built != golden {
        for (b, g) in built.iter().zip(golden.iter()) {
            if b != g {
                println!("first differing id: sieve={} sieve={}", b.id, g.id);
                println!("sieve: {b:?}");
                println!("sieve: {g:?}");
                break;
            }
        }
    }
    assert_eq!(built, golden, "node list mismatch for fixture {fixture}");
}

fn find_node<'a>(nodes: &'a [Node], id: &str) -> &'a Node {
    nodes
        .iter()
        .find(|node| node.id == id)
        .unwrap_or_else(|| panic!("golden has no file node with id {id}"))
}

#[test]
fn test_p2_01_02_05_file_node_set_matches_golden() {
    let built: BTreeSet<String> = built_file_nodes().into_iter().map(|n| n.id).collect();
    let golden: BTreeSet<String> = golden_file_nodes().into_iter().map(|n| n.id).collect();
    assert_eq!(built, golden);
}

#[test]
fn test_p2_06_meta_languages_and_scopes_match_golden() {
    let built = build_graph(&fixture_root(), &fixture_root().join("sieve"))
        .expect("build_graph should not error");
    let golden = golden_graph();
    assert_eq!(built.meta.languages, golden.meta.languages);
    assert_eq!(built.meta.scopes, golden.meta.scopes);
}

#[test]
fn test_p2_06_scope_substance_guard_matches_golden() {
    let built = build_graph(
        &fixture_path("scopes"),
        &fixture_path("scopes").join("sieve"),
    )
    .expect("build_graph should not error");
    let golden = golden_graph_for("scopes");
    assert_eq!(built.meta.scopes, golden.meta.scopes);
}

#[test]
fn test_p2_07_file_node_id_is_the_bare_path() {
    for node in built_file_nodes() {
        assert_eq!(node.id, node.path);
    }
}

#[test]
fn test_p2_08_file_node_spans_match_golden() {
    let golden = golden_file_nodes();
    for node in built_file_nodes() {
        let expected = find_node(&golden, &node.id);
        assert_eq!(node.span, expected.span, "span for {}", node.id);
    }
}

#[test]
fn test_p2_09_file_node_chars_match_golden() {
    let golden = golden_file_nodes();
    for node in built_file_nodes() {
        let expected = find_node(&golden, &node.id);
        assert_eq!(node.chars, expected.chars, "chars for {}", node.id);
    }
}

#[test]
fn test_p2_11_file_node_body_hash_matches_golden() {
    let golden = golden_file_nodes();
    for node in built_file_nodes() {
        let expected = find_node(&golden, &node.id);
        assert_eq!(
            node.body_hash, expected.body_hash,
            "body_hash for {}",
            node.id
        );
    }
}

#[test]
fn test_p2_26_28_written_file_nodes_are_sorted_and_byte_stable() {
    let graph = build_graph(&fixture_root(), &fixture_root().join("sieve"))
        .expect("build_graph should not error");
    let dir = unique_temp_dir("write");
    let path = dir.join("wiring.json");

    write_graph(&graph, &path).expect("first write succeeds");
    let first = fs::read(&path).expect("read first write");
    write_graph(&graph, &path).expect("second write succeeds");
    let second = fs::read(&path).expect("read second write");
    assert_eq!(first, second, "write_graph should be byte-stable");

    let written: Graph =
        serde_json::from_str(&String::from_utf8(first).expect("written wiring.json is utf8"))
            .expect("written wiring.json deserializes as Graph");
    let written_file_ids: Vec<String> = written
        .nodes
        .into_iter()
        .filter(|n| n.kind == Kind::File)
        .map(|n| n.id)
        .collect();
    let mut golden_file_ids: Vec<String> = golden_file_nodes().into_iter().map(|n| n.id).collect();
    golden_file_ids.sort();
    assert_eq!(written_file_ids, golden_file_ids);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_p2_file_nodes_match_golden_field_for_field() {
    // No golden pins `body_text` (spec section 8, `write_graph` strips it).
    let golden = golden_file_nodes();
    for node in built_file_nodes() {
        let expected = strip_body_text(find_node(&golden, &node.id).clone());
        let actual = strip_body_text(node);
        assert_eq!(actual, expected, "file node mismatch for {}", actual.id);
    }
}

#[test]
fn test_p2_07_to_11_basic_nodes_match_golden() {
    assert_nodes_match_golden("basic");
}

#[test]
fn test_p2_07_to_11_symbols_nodes_match_golden() {
    assert_nodes_match_golden("symbols");
}

fn scopeord_scopes() -> Vec<sieve_core::Scope> {
    build_graph(
        &fixture_path("scopeord"),
        &fixture_path("scopeord").join("sieve"),
    )
    .expect("build_graph should not error")
    .meta
    .scopes
}

#[test]
fn test_p2_06_dv5_scope_order_is_locale_compare_like_golden() {
    // `apkg` and `Bpkg` tie on length. The `localeCompare` puts `apkg`
    // first; byte order would put `Bpkg` first.
    let built: Vec<String> = scopeord_scopes().into_iter().map(|s| s.prefix).collect();
    let golden: Vec<String> = golden_graph_for("scopeord")
        .meta
        .scopes
        .into_iter()
        .map(|s| s.prefix)
        .collect();
    assert_eq!(built, ["apkg", "Bpkg"]);
    assert_eq!(built, golden);
}

#[test]
fn test_p2_06_dv6_marker_order_is_the_golden_list_order() {
    // Markers are listed as `package.json`, `go.mod`, `Cargo.toml`, not
    // in alphabetical order.
    let golden = golden_graph_for("scopeord").meta.scopes;
    assert_eq!(scopeord_scopes(), golden);
    assert_eq!(golden[0].markers, ["package.json", "go.mod", "Cargo.toml"]);
}

#[test]
fn test_p2_06_dv7_an_ignored_marker_file_still_counts_like_golden() {
    // The check tests each visible dir with `existsSync`, so an ignored
    // `Cargo.toml` is a marker. Expected value: the golden on this tree.
    let dir = unique_temp_dir("dv7");
    fs::create_dir_all(dir.join("pkg")).expect("make dir");
    fs::write(dir.join("pkg/package.json"), "{}").expect("write file");
    fs::write(dir.join("pkg/Cargo.toml"), "").expect("write file");
    fs::write(dir.join("pkg/.gitignore"), "Cargo.toml\n").expect("write file");
    fs::write(dir.join("pkg/a.ts"), "export const a = 1;\n").expect("write file");
    let status = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&dir)
        .status()
        .expect("run git init");
    assert!(status.success());
    let scopes = build_graph(&dir, &dir.join("sieve"))
        .expect("build_graph should not error")
        .meta
        .scopes;
    fs::remove_dir_all(&dir).expect("remove temp dir");
    assert_eq!(scopes.len(), 1);
    assert_eq!(scopes[0].prefix, "pkg");
    assert_eq!(scopes[0].markers, ["package.json", "Cargo.toml"]);
}

#[test]
fn test_p2_07_to_11_scopes_nodes_match_golden() {
    assert_nodes_match_golden("scopes");
}

#[test]
fn test_p2_06_workspace_nodes_match_golden() {
    assert_nodes_match_golden("workspace");
}

#[test]
fn test_p2_06_pnpm_nodes_match_golden() {
    assert_nodes_match_golden("pnpm");
}

#[test]
fn test_p2_06_workspace_scopes_match_golden() {
    let built = build_graph(
        &fixture_path("workspace"),
        &fixture_path("workspace").join("sieve"),
    )
    .expect("build_graph should not error");
    let golden = golden_graph_for("workspace");
    assert_eq!(built.meta.scopes, golden.meta.scopes);
}

#[test]
fn test_p2_06_pnpm_scopes_match_golden() {
    let built = build_graph(&fixture_path("pnpm"), &fixture_path("pnpm").join("sieve"))
        .expect("build_graph should not error");
    let golden = golden_graph_for("pnpm");
    assert_eq!(built.meta.scopes, golden.meta.scopes);
}

/// Sorts an edge list by `(source, relation, target)` for a stable diff.
fn sorted_edges(mut edges: Vec<Edge>) -> Vec<Edge> {
    edges.sort_by(|a, b| {
        (&a.source, a.relation.as_str(), &a.target).cmp(&(
            &b.source,
            b.relation.as_str(),
            &b.target,
        ))
    });
    edges
}

/// P2-12, P2-13, P2-17 to P2-22: the resolved edges match the golden's,
/// for every fixture, edge for edge.
#[test]
fn test_p2_12_13_17_to_22_edges_match_golden() {
    for fixture in ["basic", "symbols", "scopes", "edges", "workspace", "pnpm"] {
        let built = sorted_edges(
            build_graph(&fixture_path(fixture), &fixture_path(fixture).join("sieve"))
                .expect("build_graph should not error")
                .edges,
        );
        let golden = sorted_edges(golden_graph_for(fixture).edges);
        if built != golden {
            let missing = golden.iter().find(|g| !built.contains(g));
            let extra = built.iter().find(|b| !golden.contains(b));
            println!("fixture {fixture}: first missing golden edge: {missing:?}");
            println!("fixture {fixture}: first extra sieve edge: {extra:?}");
        }
        assert_eq!(built, golden, "edge mismatch for fixture {fixture}");
    }
}

/// A temp dir that removes itself on drop, for a cache test that needs
/// its own writable copy of a fixture.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let path = unique_temp_dir(label);
        fs::create_dir_all(&path).expect("create temp dir");
        TempDir { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Recursively copies `src` into `dst`, which must already exist.
fn copy_dir(src: &std::path::Path, dst: &std::path::Path) {
    for entry in fs::read_dir(src).expect("read source dir") {
        let entry = entry.expect("read dir entry");
        let file_type = entry.file_type().expect("read file type");
        let target = dst.join(entry.file_name());
        if file_type.is_dir() {
            fs::create_dir_all(&target).expect("create sub dir");
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy file");
        }
    }
}

/// P2-35: a warm build replays every unchanged file from the extraction
/// cache, and gives the same graph a cold build gives.
#[test]
fn test_p2_35_a_warm_build_replays_the_cache_and_matches_the_cold_graph() {
    let dir = TempDir::new("warm-build");
    copy_dir(&fixture_root(), &dir.path);
    let context_dir = dir.path.join("sieve");

    let cold = build_graph_cached(&dir.path, &context_dir).expect("cold build succeeds");
    assert_eq!(cold.reused, 0);
    assert_eq!(cold.parsed, 7);
    assert_eq!(cold.claimed.len(), 7);
    assert_eq!(cold.prints.len(), 7);

    let warm = build_graph_cached(&dir.path, &context_dir).expect("warm build succeeds");
    assert_eq!(warm.reused, 7);
    assert_eq!(warm.parsed, 0);
    assert_eq!(warm.claimed.len(), 7);
    assert_eq!(warm.prints.len(), 7);

    let cold_claimed: BTreeSet<String> = cold.claimed.iter().cloned().collect();
    let warm_claimed: BTreeSet<String> = warm.claimed.iter().cloned().collect();
    let cold_print_keys: BTreeSet<String> = cold.prints.keys().cloned().collect();
    let warm_print_keys: BTreeSet<String> = warm.prints.keys().cloned().collect();
    assert_eq!(
        cold_print_keys, cold_claimed,
        "cold prints key set matches claimed"
    );
    assert_eq!(
        warm_print_keys, warm_claimed,
        "warm prints key set matches claimed"
    );
    assert_eq!(
        cold.prints, warm.prints,
        "warm prints should match cold prints"
    );

    let write_dir = TempDir::new("warm-build-write");
    let cold_path = write_dir.path.join("cold.json");
    let warm_path = write_dir.path.join("warm.json");
    write_graph(&cold.graph, &cold_path).expect("write cold graph");
    write_graph(&warm.graph, &warm_path).expect("write warm graph");
    let cold_bytes = fs::read(&cold_path).expect("read cold graph");
    let warm_bytes = fs::read(&warm_path).expect("read warm graph");
    assert_eq!(
        cold_bytes, warm_bytes,
        "warm build should match cold build byte for byte"
    );

    // Touching one file's content re-parses it, and only it.
    let touched = dir.path.join("src/util.ts");
    let original = fs::read_to_string(&touched).expect("read util.ts");
    fs::write(&touched, format!("{original}\n// touched\n")).expect("touch util.ts");

    let after_touch = build_graph_cached(&dir.path, &context_dir).expect("touched build succeeds");
    assert_eq!(after_touch.parsed, 1);
    assert_eq!(after_touch.reused, 6);
}

/// P2-35: a stamp change (an extractor upgrade) invalidates the whole
/// cache, so the next build re-parses every claimed file.
#[test]
fn test_p2_35_a_stamp_mismatch_invalidates_the_whole_cache() {
    let dir = TempDir::new("stamp-mismatch");
    copy_dir(&fixture_root(), &dir.path);
    let context_dir = dir.path.join("sieve");

    let first = build_graph_cached(&dir.path, &context_dir).expect("first build succeeds");
    assert_eq!(first.parsed, 7);

    let cache_path = context_dir
        .join(".cache")
        .join(format!("extract.{}.json", sieve_parse::extractor_stamp()));
    let text = fs::read_to_string(&cache_path).expect("read cache file");
    let mut cache: serde_json::Value = serde_json::from_str(&text).expect("parse cache file");
    cache["extractor"] = serde_json::Value::String("some-other-stamp".to_string());
    fs::write(&cache_path, serde_json::to_string(&cache).unwrap()).expect("rewrite cache file");

    let second = build_graph_cached(&dir.path, &context_dir).expect("second build succeeds");
    assert_eq!(second.parsed, 7);
    assert_eq!(second.reused, 0);
}

/// P3-01, P3-02: the `ask` sidecar built by Sieve matches the golden sidecar
/// byte for byte, on every fixture (`notes/ask-ranking.md` sections 1, 2, and
/// 10.10).
#[test]
fn test_p3_01_02_ask_sidecar_matches_golden_byte_for_byte() {
    for fixture in ["basic", "symbols", "scopes", "edges", "workspace", "pnpm"] {
        let graph = build_graph(&fixture_path(fixture), &fixture_path(fixture).join("sieve"))
            .unwrap_or_else(|e| panic!("build_graph should not error for {fixture}: {e}"));
        let index = sieve_core::askindex::build_ask_index(&graph);

        if fixture == "basic" {
            assert_eq!(
                index.avg_body_len, 7.333_333_333_333_333,
                "avg_body_len should match Sieve's sidecar on basic"
            );
        }

        let dir = TempDir::new(&format!("ask-index-{fixture}"));
        let out_path = dir.path.join("ask-index.json");
        sieve_core::askindex::write_ask_index(&out_path, &index)
            .expect("write_ask_index should not error");
        let built_bytes = fs::read_to_string(&out_path).expect("read built sidecar");

        let golden_path = manifest_dir().join(format!(
            "../../tests/fixtures/{fixture}.expected/ask-index.json"
        ));
        let golden_bytes = fs::read_to_string(&golden_path).expect("read golden sidecar");

        if built_bytes == golden_bytes {
            continue;
        }

        let built: serde_json::Value =
            serde_json::from_str(&built_bytes).expect("built sidecar parses as JSON");
        let golden: serde_json::Value =
            serde_json::from_str(&golden_bytes).expect("golden sidecar parses as JSON");

        if let (Some(built_obj), Some(golden_obj)) = (built.as_object(), golden.as_object()) {
            for key in golden_obj.keys() {
                if built_obj.get(key) != golden_obj.get(key) {
                    println!("fixture {fixture}: first differing top-level key: {key}");
                    break;
                }
            }
        }

        let built_docs = built.get("docs").and_then(|v| v.as_array());
        let golden_docs = golden.get("docs").and_then(|v| v.as_array());
        if let (Some(built_docs), Some(golden_docs)) = (built_docs, golden_docs) {
            for (b, g) in built_docs.iter().zip(golden_docs.iter()) {
                if b != g {
                    println!(
                        "fixture {fixture}: first differing doc id: {:?}",
                        g.get("id")
                    );
                    println!("fixture {fixture}: sieve name={:?}", b.get("name"));
                    println!("fixture {fixture}: sieve name={:?}", g.get("name"));
                    println!("fixture {fixture}: sieve path={:?}", b.get("path"));
                    println!("fixture {fixture}: sieve path={:?}", g.get("path"));
                    println!("fixture {fixture}: sieve body={:?}", b.get("body"));
                    println!("fixture {fixture}: sieve body={:?}", g.get("body"));
                    break;
                }
            }
        }

        let built_df = built.get("df").and_then(|v| v.as_array());
        let golden_df = golden.get("df").and_then(|v| v.as_array());
        if let (Some(built_df), Some(golden_df)) = (built_df, golden_df) {
            for (b, g) in built_df.iter().zip(golden_df.iter()) {
                if b != g {
                    println!("fixture {fixture}: df differs: sieve={b:?} sieve={g:?}");
                }
            }
        }

        assert_eq!(
            built_bytes, golden_bytes,
            "ask sidecar mismatch for fixture {fixture}"
        );
    }
}

/// P2-12: no resolved edge ever carries `Confidence::LspDispatch`.
#[test]
fn test_p2_12_no_edge_carries_lsp_dispatch() {
    for fixture in ["basic", "symbols", "scopes", "edges", "workspace", "pnpm"] {
        let built = build_graph(&fixture_path(fixture), &fixture_path(fixture).join("sieve"))
            .expect("build_graph should not error");
        assert!(
            !built
                .edges
                .iter()
                .any(|e| e.confidence == Confidence::LspDispatch),
            "fixture {fixture} should carry no lsp_dispatch edge"
        );
    }
}
