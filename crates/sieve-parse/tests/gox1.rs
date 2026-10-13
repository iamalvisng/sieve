//! GOX1: the same-file Go proof on `tests/fixtures/gox`. A proof gives
//! Extracted. Each stop case gives no Extracted edge.

use std::path::PathBuf;

use sieve_core::{Confidence, Relation};
use sieve_parse::build_graph;

#[test]
fn test_gox1_fixture_pins_proofs_and_stops() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let root = PathBuf::from(manifest).join("../../tests/fixtures/gox");
    let graph = build_graph(&root, &root.join("sieve")).expect("build_graph");
    let got: Vec<(String, String, Confidence)> = graph
        .edges
        .iter()
        .filter(|e| e.relation == Relation::Calls)
        .map(|e| (e.source.clone(), e.target.clone(), e.confidence))
        .collect();
    let conf = |s: &str, t: &str| {
        got.iter()
            .find(|(a, b, _)| a == s && b == t)
            .map(|(_, _, c)| *c)
    };
    let ext = Some(Confidence::Extracted);
    assert_eq!(conf("proofs.go#Proofs", "proofs.go#helper"), ext);
    assert_eq!(conf("proofs.go#Proofs", "proofs.go#T.M"), ext);
    assert_eq!(conf("proofs.go#Proofs", "proofs.go#NewT"), ext);
    assert_eq!(conf("proofs.go#T.Self", "proofs.go#T.M"), ext);
    // Each stop case gives no Extracted edge. The old path caps it.
    for (s, t, c) in &got {
        assert!(
            !(s.starts_with("stops.go#") && *c == Confidence::Extracted),
            "stop case gave Extracted: {s} -> {t}"
        );
    }
    assert_eq!(
        conf("stops.go#ShadowParam", "stops.go#fn"),
        Some(Confidence::Inferred)
    );
}
