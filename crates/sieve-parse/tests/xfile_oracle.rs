//! Cross-file gate: the Extracted cross-file call edges Sieve builds on
//! `tests/fixtures/xfile2` and `xfile3` match the frozen compiler oracle.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use sieve_core::{Confidence, Relation};
use sieve_parse::build_graph;

/// The `(source file, target file, name)` key of one cross-file call.
type Key = (String, String, String);

/// One oracle row: its key, with `None` for a row whose target has no name,
/// and its source line.
type Row = ((String, String, Option<String>), u64);

fn manifest_dir() -> PathBuf {
    PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
}

/// Parses a `L<start>-L<end>` span into its inclusive line bounds.
fn parse_span(span: &str) -> (u64, u64) {
    let (a, b) = span.split_once('-').expect("span has a dash");
    let num = |t: &str| {
        t.trim_start_matches('L')
            .parse::<u64>()
            .expect("span number")
    };
    (num(a), num(b))
}

/// Checks the Sieve graph of `tests/fixtures/<fixture>` against the frozen
/// oracle. Every Sieve Extracted cross-file call edge must match a row by
/// target file, name and span. Every row must be covered by an edge, except
/// the `file:line` rows in `expected_uncovered`, which must stay uncovered.
/// A row with no target name
/// counts as uncovered, never as wrong.
fn check_frozen(fixture: &str, expected_uncovered: &[&str]) {
    let root = manifest_dir().join("../../tests/fixtures").join(fixture);
    let expected = manifest_dir()
        .join("../../tests/fixtures")
        .join(format!("{fixture}.expected/oracle.jsonl"));
    let graph = build_graph(&root, &root.join("sieve")).expect("build_graph");
    let text = fs::read_to_string(&expected).expect("read oracle.jsonl");

    let mut rows: Vec<Row> = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let row: serde_json::Value = serde_json::from_str(line).expect("oracle row");
        let field = |k: &str| row[k].as_str().map(str::to_string);
        let key = (
            field("src").expect("src field"),
            field("dst").unwrap_or_default(),
            field("name"),
        );
        rows.push((key, row["line"].as_u64().expect("line")));
    }

    let nodes: HashMap<&str, &sieve_core::Node> =
        graph.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    // Each Sieve Extracted cross-file call edge: its key and source span.
    let mut edges: Vec<(Key, (u64, u64), String)> = Vec::new();
    for edge in &graph.edges {
        if edge.relation != Relation::Calls || edge.confidence != Confidence::Extracted {
            continue;
        }
        let (Some(src), Some(dst)) = (
            nodes.get(edge.source.as_str()),
            nodes.get(edge.target.as_str()),
        ) else {
            continue;
        };
        if src.path == dst.path {
            continue;
        }
        let key = (src.path.clone(), dst.path.clone(), dst.name.clone());
        let label = format!("{} -> {}", edge.source, edge.target);
        edges.push((key, parse_span(&src.span), label));
    }

    assert!(!rows.is_empty(), "empty oracle");
    assert!(!edges.is_empty(), "no Extracted cross-file edge");
    let matches = |row: &Row, edge: &(Key, (u64, u64), String)| {
        let ((src, dst, name), line) = row;
        let (key, (start, end), _) = edge;
        name.as_deref() == Some(key.2.as_str())
            && *src == key.0
            && *dst == key.1
            && (*start..=*end).contains(line)
    };
    // For a call at file level the source node is the file node, so the span
    // check is weak there.
    let wrong: Vec<&str> = edges
        .iter()
        .filter(|edge| !rows.iter().any(|row| matches(row, edge)))
        .map(|(_, _, label)| label.as_str())
        .collect();
    let mut uncovered: Vec<String> = rows
        .iter()
        .filter(|row| !edges.iter().any(|edge| matches(row, edge)))
        .map(|((src, _, _), line)| format!("{src}:{line}"))
        .collect();
    uncovered.sort();
    let unexpected: Vec<&String> = uncovered
        .iter()
        .filter(|row| !expected_uncovered.contains(&row.as_str()))
        .collect();
    let covered: Vec<&&str> = expected_uncovered
        .iter()
        .filter(|row| !uncovered.iter().any(|u| u == *row))
        .collect();
    assert!(wrong.is_empty(), "wrong edges: {wrong:?}");
    assert!(unexpected.is_empty(), "uncovered rows: {unexpected:?}");
    assert!(
        covered.is_empty(),
        "rows expected uncovered but covered: {covered:?}"
    );
}

#[test]
fn test_xfile2_matches_frozen_oracle() {
    check_frozen("xfile2", &[]);
}

/// The `hc` row (line 28) is a star conflict, the `h3` row (line 32) is a
/// local `const` alias, and the `hm` row (line 36) is a non-function export
/// behind a star. Sieve gives no edge for each, so each stays uncovered.
#[test]
fn test_xfile3_matches_frozen_oracle() {
    check_frozen("xfile3", &["app.ts:28", "app.ts:32", "app.ts:36"]);
}
