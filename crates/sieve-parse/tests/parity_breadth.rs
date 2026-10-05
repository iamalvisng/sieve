//! Parity tests for the container tier (`.vue`) and the breadth tier
//! (P2-05, P2-09, P2-10), pinned against the golden `wiring.json` of
//! `tests/fixtures/langs`.

use std::fs;
use std::path::PathBuf;

use sieve_core::{Edge, Graph, Kind, Node};
use sieve_parse::build_graph;

/// Every breadth or container language this test pins, by its fixture
/// directory prefix.
const LANGS: &[&str] = &[
    "vue/",
    "clojure/",
    "rust/",
    "c/",
    "cpp/",
    "ruby/",
    "csharp/",
    "scala/",
    "elixir/",
    "solidity/",
    "ocaml/",
    "zig/",
    "dart/",
    "nix/",
    "lua/",
];

fn manifest_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
}

fn fixture_root() -> PathBuf {
    manifest_dir().join("../../tests/fixtures/langs")
}

fn golden_graph() -> Graph {
    let path = manifest_dir().join("../../tests/fixtures/langs.expected/wiring.json");
    let text = fs::read_to_string(&path).expect("read golden wiring.json");
    serde_json::from_str(&text).expect("golden wiring.json deserializes as Graph")
}

fn built_graph() -> Graph {
    let root = fixture_root();
    build_graph(&root, &root.join("sieve")).expect("build_graph should not error")
}

/// The golden carries no `body_text` on any node; see the same
/// normalisation in `parity_langs.rs`.
fn drop_body_text(mut node: Node) -> Node {
    node.body_text = None;
    node
}

fn nodes_under(nodes: &[Node], prefix: &str) -> Vec<Node> {
    let mut filtered: Vec<Node> = nodes
        .iter()
        .filter(|n| n.path.starts_with(prefix))
        .cloned()
        .map(drop_body_text)
        .collect();
    filtered.sort_by(|a, b| a.id.cmp(&b.id));
    filtered
}

fn edges_under<'a>(edges: &'a [Edge], nodes: &[Node], prefix: &str) -> Vec<&'a Edge> {
    let owned: std::collections::HashSet<&str> = nodes
        .iter()
        .filter(|n| n.path.starts_with(prefix))
        .map(|n| n.id.as_str())
        .collect();
    let mut filtered: Vec<&Edge> = edges
        .iter()
        .filter(|e| owned.contains(e.source.as_str()))
        .collect();
    filtered.sort_by(|a, b| {
        (&a.source, a.relation.as_str(), &a.target).cmp(&(
            &b.source,
            b.relation.as_str(),
            &b.target,
        ))
    });
    filtered
}

/// Builds the `langs` graph once, and diffs it against the golden per
/// language directory, reporting the first mismatch for that language.
#[test]
fn test_p2_05_09_10_vue_and_breadth_match_golden() {
    let built = built_graph();
    let golden = golden_graph();

    for prefix in LANGS {
        let built_nodes = nodes_under(&built.nodes, prefix);
        let golden_nodes = nodes_under(&golden.nodes, prefix);
        assert_eq!(
            built_nodes.len(),
            golden_nodes.len(),
            "{prefix}: node count differs.\nbuilt: {built_nodes:#?}\ngolden: {golden_nodes:#?}"
        );
        for (b, g) in built_nodes.iter().zip(golden_nodes.iter()) {
            assert_eq!(b, g, "{prefix}: node mismatch at id {}", g.id);
        }

        let built_edges = edges_under(&built.edges, &built.nodes, prefix);
        let golden_edges = edges_under(&golden.edges, &golden.nodes, prefix);
        assert_eq!(
            built_edges.len(),
            golden_edges.len(),
            "{prefix}: edge count differs.\nbuilt: {built_edges:#?}\ngolden: {golden_edges:#?}"
        );
        for (b, g) in built_edges.iter().zip(golden_edges.iter()) {
            assert_eq!(b, g, "{prefix}: edge mismatch, source {}", g.source);
        }
    }
}

/// A breadth file node's `chars` is its UTF-8 byte length; a depth or
/// container file node's `chars` is its UTF-16 length
/// (`languages-lsp.md` section 7).
#[test]
fn test_p2_05_09_10_file_node_chars_match_the_tier_encoding() {
    let built = built_graph();
    let breadth_prefixes: Vec<&str> = LANGS.iter().filter(|p| **p != "vue/").copied().collect();

    for prefix in &breadth_prefixes {
        let Some(file_node) = built
            .nodes
            .iter()
            .find(|n| n.kind == Kind::File && n.path.starts_with(prefix))
        else {
            continue; // an unported language has no file node yet
        };
        let source_path = fixture_root().join(&file_node.path);
        let source = fs::read_to_string(&source_path).expect("read fixture source");
        assert_eq!(
            file_node.chars,
            Some(source.len() as u64),
            "{prefix}: breadth file node chars should be the UTF-8 byte length"
        );
    }

    let Some(vue_file) = built
        .nodes
        .iter()
        .find(|n| n.kind == Kind::File && n.path == "vue/App.vue")
    else {
        panic!("vue/App.vue file node missing from the built graph");
    };
    let source = fs::read_to_string(fixture_root().join("vue/App.vue")).expect("read vue fixture");
    assert_eq!(
        vue_file.chars,
        Some(source.encode_utf16().count() as u64),
        "vue/App.vue: container file node chars should be the UTF-16 length"
    );
}
