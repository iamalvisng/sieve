//! Parity tests for Go, Java, and PHP extraction and resolution (P2-14 to
//! P2-16), pinned against the recorded golden `wiring.json`
//! for `tests/fixtures/langs`.
//!
//! Every other language file in the fixture produces zero nodes today,
//! because its port has not landed yet. This test filters both the built
//! graph and the golden to `go/`, `java/`, and `php/` paths only, and
//! never asserts on the rest.

use std::fs;
use std::path::PathBuf;

use sieve_core::{Edge, Graph, Node};
use sieve_parse::build_graph;

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

/// `tests/fixtures/langs.expected/wiring.json` carries no `body_text` on
/// any of its 140 nodes, in every language, including the already-ported
/// TypeScript and Python ones. That is a capture-time omission in this
/// golden, not a per-language rule (the note says every depth-tier file
/// node carries a residual `body_text`; the golden disagrees, and the
/// golden wins). This strips `body_text` on both sides before the
/// struct-equality check, so this delta never masks a real difference.
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

#[test]
fn test_p2_14_to_16_go_java_php_match_golden() {
    let built = built_graph();
    let golden = golden_graph();

    for prefix in ["go/", "java/", "php/"] {
        run_prefix(&built, &golden, prefix);
    }
}

/// Swift and R against the same golden (edges note, sections 1.4 and 1.6).
#[test]
fn test_p2_14_to_16_swift_r_match_golden() {
    let built = built_graph();
    let golden = golden_graph();

    for prefix in ["swift/", "r/"] {
        run_prefix(&built, &golden, prefix);
    }
}

/// Kotlin against the same golden (edges note, section 1.3).
#[test]
fn test_p2_14_kotlin_matches_golden() {
    let built = built_graph();
    let golden = golden_graph();

    run_prefix(&built, &golden, "kotlin/");
}

/// P2-16: every language `resolve_edges` dispatches to its own resolver
/// (Go, Java, C, Rust, PHP) resolves an in-repo import edge to the right
/// file node, byte for byte with the golden. `run_prefix` above already
/// pins every node and edge under each language's directory; this test
/// names the specific import edges the fixture's cross-file cases add, so
/// a regression in one resolver fails on its own line instead of only on
/// a generic count mismatch.
#[test]
fn test_p2_16_imports_resolve_per_language_like_golden() {
    let built = built_graph();
    let want: &[(&str, &str, &str)] = &[
        ("go/main.go", "imports", "go/greet/greet.go"),
        ("java/App.java", "imports", "java/util/Helper.java"),
        ("c/app.c", "imports", "c/util.h"),
        ("rust/lib.rs", "imports", "rust/util.rs"),
        ("rust/lib.rs", "imports", "crate::missing::nope"),
        ("php/App.php", "imports", "php/Models/User.php"),
        ("swift/App.swift", "imports", "Foundation"),
    ];
    for (source, relation, target) in want {
        assert!(
            built.edges.iter().any(|e| {
                e.source == *source && e.relation.as_str() == *relation && e.target == *target
            }),
            "missing edge {source} -{relation}-> {target}\nedges: {:#?}",
            built.edges
        );
    }
}

/// P2-21: cross-file resolution is gated by language family. A same-family
/// call resolves (Java calling a Kotlin class, C calling a C++ function);
/// a cross-family call with the same kind and name available drops (Java
/// calling a same-named C struct, Python calling a same-named TypeScript
/// function). The full golden `wiring.json` still matches byte for byte,
/// so this test also proves the family gate adds no edge the golden drops and
/// drops no edge the golden keeps.
#[test]
fn test_p2_21_cross_file_resolution_is_gated_by_language_family() {
    let built = built_graph();
    let golden = golden_graph();

    let same_family_edges: &[(&str, &str)] = &[
        ("java/App.java#App.famACall", "kotlin/App.kt#FamAWidget"),
        ("c/app.c#main", "cpp/app.cpp#famB_run"),
    ];
    for (source, target) in same_family_edges {
        assert!(
            built.edges.iter().any(|e| e.source == *source
                && e.relation.as_str() == "calls"
                && e.target == *target),
            "missing same-family edge {source} -calls-> {target}"
        );
    }

    let cross_family_targets: &[(&str, &str)] = &[
        ("java/App.java#App.famCCall", "c/util.h#FamCThing"),
        ("python/app.py", "ts/app.ts#famD_run"),
    ];
    for (source, target) in cross_family_targets {
        assert!(
            !built.edges.iter().any(|e| e.source == *source
                && e.relation.as_str() == "calls"
                && e.target == *target),
            "cross-family edge {source} -calls-> {target} should not resolve"
        );
    }

    let mut built_nodes = built.nodes.clone();
    let mut golden_nodes = golden.nodes.clone();
    built_nodes.sort_by(|a, b| a.id.cmp(&b.id));
    golden_nodes.sort_by(|a, b| a.id.cmp(&b.id));
    assert_eq!(
        built_nodes
            .into_iter()
            .map(drop_body_text)
            .collect::<Vec<_>>(),
        golden_nodes
            .into_iter()
            .map(drop_body_text)
            .collect::<Vec<_>>(),
        "the langs golden's node set must still match byte for byte"
    );

    let mut built_edges = built.edges.clone();
    let mut golden_edges = golden.edges.clone();
    built_edges.sort_by(|a, b| {
        (&a.source, a.relation.as_str(), &a.target).cmp(&(
            &b.source,
            b.relation.as_str(),
            &b.target,
        ))
    });
    golden_edges.sort_by(|a, b| {
        (&a.source, a.relation.as_str(), &a.target).cmp(&(
            &b.source,
            b.relation.as_str(),
            &b.target,
        ))
    });
    assert_eq!(
        built_edges, golden_edges,
        "the langs golden's edge set must still match byte for byte"
    );
}

fn run_prefix(built: &Graph, golden: &Graph, prefix: &str) {
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

/// P2-14, P2-15 (DV18, DV19): PHP closures are function nodes that own
/// their calls, and a class-name or `static::`/`parent::` receiver
/// resolves a call edge. Ids and edges come from the golden.
#[test]
fn test_p2_14_php_closures_and_static_receivers_match_golden() {
    let built = built_graph();
    let f = "php/Closures.php";
    for id in [
        "build.fn",
        "build.g",
        "build.{closure}",
        "build.h",
        "build.h.{closure}",
    ] {
        let id = format!("{f}#{id}");
        assert!(built.nodes.iter().any(|n| n.id == id), "missing node {id}");
    }
    let want = [
        ("build.g", "helper"),
        ("build.{closure}", "helper"),
        ("build.h.{closure}", "helper"),
        ("build", "Point.origin"),
        ("Child.viaStatic", "Base.mk"),
        ("Child.viaParent", "Base.mk"),
    ];
    for (src, dst) in want {
        let (src, dst) = (format!("{f}#{src}"), format!("{f}#{dst}"));
        assert!(
            built
                .edges
                .iter()
                .any(|e| e.source == src && e.relation.as_str() == "calls" && e.target == dst),
            "missing edge {src} -calls-> {dst}"
        );
    }
    let build = format!("{f}#build");
    let stray = built
        .edges
        .iter()
        .any(|e| e.source == build && e.relation.as_str() == "calls" && e.target.ends_with(".m"));
    assert!(!stray, "`$obj->m()` must not resolve to a method");
}

/// P2-14 (DV17): a Kotlin enum signature ends at `enum_class_body`, for a
/// one-line enum, an enum with entries, and an enum whose entries hold a
/// body. Signatures come from the golden.
#[test]
fn test_p2_14_kotlin_enum_signatures_end_at_the_enum_body() {
    let built = built_graph();
    let want = [
        ("Color", "enum class Color"),
        ("Level", "enum class Level(val rank: Int)"),
        ("Op", "enum class Op"),
        ("Op.apply~3", "abstract fun apply(a: Int, b: Int): Int"),
    ];
    for (id, sig) in want {
        let id = format!("kotlin/Enums.kt#{id}");
        let node = built.nodes.iter().find(|n| n.id == id);
        let node = node.unwrap_or_else(|| panic!("missing node {id}"));
        assert_eq!(node.signature.as_deref(), Some(sig), "signature of {id}");
    }
}

/// P2-14, P2-15: a call through `this`, and a call inside a trailing
/// lambda, resolve as the golden does. The lambda call belongs to the
/// enclosing function. `each` has exactly one calls edge: the golden gives no
/// edge for `forEach`, `run`, `Box.make` or `"hi".shout`.
#[test]
fn test_p2_15_kotlin_this_and_trailing_lambda_calls_match_golden() {
    let built = built_graph();
    let f = "kotlin/Calls.kt";
    let calls_from = |src: &str| -> Vec<String> {
        let src = format!("{f}#{src}");
        let mut out: Vec<String> = built
            .edges
            .iter()
            .filter(|e| e.source == src && e.relation.as_str() == "calls")
            .map(|e| e.target.clone())
            .collect();
        out.sort();
        out
    };
    assert_eq!(calls_from("Box.reopen"), vec![format!("{f}#Box.open")]);
    assert_eq!(calls_from("each"), vec![format!("{f}#helper")]);
}

/// P2-05 (DV16): a Kotlin file gives the nodes and `contains` edges that
/// the golden gives for the same file: 9 nodes, 8 edges. The fwcd grammar
/// finds the class, object, interface and top-level function.
#[test]
fn test_p2_05_dv16_kotlin_file_nodes_match_golden() {
    let src = "package a\nclass Foo(val x: Int) {\n    fun bar(): Int = baz()\n    fun baz(): Int = 1\n}\nfun top() { Foo(1).bar() }\nobject O { fun f() {} }\ninterface I { fun g() }\n";
    let root = std::env::temp_dir().join(format!("sieve-p2-05-dv16-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("create temp repo");
    fs::write(root.join("A.kt"), src).expect("write A.kt");
    let built = build_graph(&root, &root.join("sieve")).expect("build_graph");
    let _ = fs::remove_dir_all(&root);

    let mut ids: Vec<&str> = built.nodes.iter().map(|n| n.id.as_str()).collect();
    ids.sort();
    let want = [
        "A.kt",
        "A.kt#Foo",
        "A.kt#Foo.bar",
        "A.kt#Foo.baz",
        "A.kt#I",
        "A.kt#I.g",
        "A.kt#O",
        "A.kt#O.f",
        "A.kt#top",
    ];
    assert_eq!(ids, want, "node ids");
    assert_eq!(built.edges.len(), 8, "edge count");
}
