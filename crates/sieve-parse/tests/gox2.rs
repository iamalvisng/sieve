//! The cross-file Go proof on `tests/fixtures/gox`. A proof gives
//! Extracted. Each stop case gives no Extracted edge.

use std::path::PathBuf;

use sieve_core::{Confidence, Relation};
use sieve_parse::build_graph;

fn extracted_calls() -> Vec<(String, String)> {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let root = PathBuf::from(manifest).join("../../tests/fixtures/gox");
    let graph = build_graph(&root, &root.join("sieve")).expect("build_graph");
    graph
        .edges
        .iter()
        .filter(|e| e.relation == Relation::Calls && e.confidence == Confidence::Extracted)
        .map(|e| (e.source.clone(), e.target.clone()))
        .collect()
}

#[test]
fn test_gox2_fixture_pins_cross_file_proofs() {
    let got = extracted_calls();
    let has = |s: &str, t: &str| got.iter().any(|(a, b)| a == s && b == t);
    let cross = "cross.go#Cross";
    // A func of another file of the same package.
    assert!(has(cross, "local.go#sib"));
    // The local name is the package clause, not the last path segment.
    assert!(has(cross, "util/util.go#Help"));
    // An alias is the local name.
    assert!(has(cross, "other/other.go#Run"));
    // A go.mod `replace => ./path` links a second module.
    assert!(has(cross, "ext/ext.go#Ext"));
    // A constructor and a typed var of an imported package.
    assert!(has(cross, "util/util.go#Box.Open"));
    // `new(T)` and `&T{}` with the type in another file.
    assert!(has(cross, "local.go#Local.Do"));
    // An external test package sees the test files of its own package.
    assert!(has("util/ext_test.go#TestExt", "util/util.go#Help"));
    assert!(has(
        "util/ext_test.go#TestExt",
        "util/util_test.go#TestOnly"
    ));
}

#[test]
fn test_gox2_fixture_pins_stops() {
    let got = extracted_calls();
    let has = |s: &str, t: &str| got.iter().any(|(a, b)| a == s && b == t);
    // A build-tag twin gives 2 bindings.
    assert!(!got.iter().any(|(_, t)| t.ends_with("#Twin")));
    // A module with no replace or go.work entry.
    assert!(!has("cross.go#Cross", "lone/lone.go#Lone"));
    // A test file def is invisible to a non-test caller.
    assert!(!has("util/util.go#UseTest", "util/util_test.go#TestOnly"));
    // A dot import stops the file.
    assert!(!has("dotdir/a.go#A", "dotdir/b.go#B"));
    // A dir with 2 non-test package clauses stops.
    assert!(!has("mixed/a.go#A", "mixed/b.go#B"));
    // A package-level `new` in another file hides the builtin.
    assert!(!has("shadow/use.go#Use", "shadow/use.go#T.M"));
}
