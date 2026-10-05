//! The byte-stable sorted write of a `Graph` to `wiring.json` (P2-26 to
//! P2-28).

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process;

use crate::collate::collate;
use crate::wiring::Graph;

/// Writes `graph` to `path` as sorted, pretty JSON, atomically.
///
/// This function sorts nodes by `id` and edges by `(source, relation,
/// target)`, using ICU root collation for each field, strips `body_text`
/// from the written copy, and never changes `graph` itself. The write goes
/// through a `.<pid>.tmp` file and a rename, so a reader never sees a
/// partial file.
pub fn write_graph(graph: &Graph, path: &Path) -> io::Result<()> {
    let mut out = graph.clone();
    out.nodes.sort_by(|a, b| collate(&a.id, &b.id));
    out.edges.sort_by(|a, b| {
        collate(&a.source, &b.source)
            .then_with(|| collate(a.relation.as_str(), b.relation.as_str()))
            .then_with(|| collate(&a.target, &b.target))
    });
    for node in &mut out.nodes {
        node.body_text = None;
    }
    out.meta.version = 1;
    out.meta.node_count = out.nodes.len();
    out.meta.edge_count = out.edges.len();

    let mut json = serde_json::to_string_pretty(&out).map_err(io::Error::other)?;
    json.push('\n');

    write_atomic(path, json.as_bytes())
}

/// Writes `bytes` to `path` atomically, through a `.<pid>.tmp` sibling file
/// and a rename, so a reader never sees a partial file.
///
/// Creates the parent directory when it does not exist yet.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }

    let tmp_path = tmp_path_for(path);
    let result = fs::write(&tmp_path, bytes).and_then(|()| fs::rename(&tmp_path, path));
    if result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    result
}

/// Builds the `<path>.<pid>.tmp` sibling path used for the atomic write.
fn tmp_path_for(path: &Path) -> PathBuf {
    let pid = process::id();
    let mut file_name: OsString = path.file_name().unwrap_or_default().to_os_string();
    file_name.push(format!(".{pid}.tmp"));
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(file_name),
        _ => PathBuf::from(file_name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wiring::{Confidence, Edge, Kind, Meta, Node, Origin, Relation, SummaryState};
    use std::fs;

    fn node(id: &str, body_text: Option<&str>) -> Node {
        Node {
            id: id.to_string(),
            name: id.to_string(),
            kind: Kind::Function,
            owner: None,
            path: "a.rs".to_string(),
            span: "L1-L2".to_string(),
            signature: None,
            exported: true,
            origin: Origin::Ast,
            body_hash: "0".repeat(64),
            chars: None,
            body_text: body_text.map(|s| s.to_string()),
            arity: None,
            variadic: None,
            summary_state: SummaryState::Pending,
            summary: None,
            crux: None,
        }
    }

    /// A temp dir unique per call, that removes itself on drop, even if
    /// the test panics before it reaches its own cleanup line.
    fn unique_temp_dir(label: &str) -> crate::test_support::TempDir {
        crate::test_support::TempDir::new(label)
    }

    fn edge(source: &str, relation: Relation, target: &str) -> Edge {
        Edge {
            source: source.to_string(),
            target: target.to_string(),
            relation,
            confidence: Confidence::Extracted,
        }
    }

    fn sample_graph() -> Graph {
        Graph {
            meta: Meta {
                version: 1,
                node_count: 0,
                edge_count: 0,
                languages: vec!["rust".to_string()],
                scopes: vec![],
            },
            nodes: vec![
                node("b.rs#foo", Some("fn foo() {}")),
                node("a.rs#foo", Some("fn foo() {}")),
            ],
            edges: vec![
                edge("b.rs#foo", Relation::Calls, "a.rs#foo"),
                edge("a.rs#foo", Relation::Calls, "a.rs#foo"),
                edge("a.rs#foo", Relation::Contains, "a.rs#foo"),
            ],
        }
    }

    #[test]
    fn write_graph_sorts_nodes_by_id_and_edges_by_source_relation_target() {
        let dir = unique_temp_dir("sort");
        let path = dir.join("wiring.json");
        let graph = sample_graph();

        write_graph(&graph, &path).expect("write succeeds");
        let text = fs::read_to_string(&path).expect("read written file");
        let written: Graph = serde_json::from_str(&text).expect("parse written file");

        assert_eq!(
            written
                .nodes
                .iter()
                .map(|n| n.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a.rs#foo", "b.rs#foo"]
        );
        assert_eq!(
            written
                .edges
                .iter()
                .map(|e| (e.source.as_str(), e.relation.as_str(), e.target.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("a.rs#foo", "calls", "a.rs#foo"),
                ("a.rs#foo", "contains", "a.rs#foo"),
                ("b.rs#foo", "calls", "a.rs#foo"),
            ]
        );
    }

    #[test]
    fn write_graph_strips_body_text_but_leaves_the_input_graph_untouched() {
        let dir = unique_temp_dir("body");
        let path = dir.join("wiring.json");
        let graph = sample_graph();

        write_graph(&graph, &path).expect("write succeeds");
        let text = fs::read_to_string(&path).expect("read written file");

        assert!(!text.contains("body_text"));
        assert_eq!(graph.nodes[0].body_text.as_deref(), Some("fn foo() {}"));
    }

    #[test]
    fn write_graph_is_byte_identical_across_two_writes() {
        let dir = unique_temp_dir("stable");
        let path = dir.join("wiring.json");
        let graph = sample_graph();

        write_graph(&graph, &path).expect("first write succeeds");
        let first = fs::read(&path).expect("read first write");
        write_graph(&graph, &path).expect("second write succeeds");
        let second = fs::read(&path).expect("read second write");

        assert_eq!(first, second);
    }

    #[test]
    fn write_graph_leaves_no_temp_file_after_success() {
        let dir = unique_temp_dir("temp");
        let path = dir.join("wiring.json");
        let graph = sample_graph();

        write_graph(&graph, &path).expect("write succeeds");
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .expect("read dir")
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }
}
