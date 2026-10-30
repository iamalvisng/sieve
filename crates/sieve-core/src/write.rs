//! The byte-stable sorted write of a `Graph` to `wiring.json`.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process;

use crate::collate::collate;
use crate::wiring::{Edge, Graph, Node};

/// Orders two nodes by `id`, using ICU root collation.
pub fn node_cmp(a: &Node, b: &Node) -> Ordering {
    collate(&a.id, &b.id)
}

/// Orders two edges by `(source, relation, target)`, using ICU root
/// collation for each field. This is the wiring edge order.
pub fn edge_cmp(a: &Edge, b: &Edge) -> Ordering {
    collate(&a.source, &b.source)
        .then_with(|| collate(a.relation.as_str(), b.relation.as_str()))
        .then_with(|| collate(&a.target, &b.target))
}

/// The item count from which [`par_sort_by`] uses threads.
const PAR_SORT_MIN: usize = 4096;

/// Sorts like the stable `slice::sort_by`, and gives the same order. Threads
/// sort equal chunks first. The final stable sort then only merges the runs,
/// and an equal pair keeps its first-chunk-first order.
fn par_sort_by<T: Send>(items: &mut [T], cmp: impl Fn(&T, &T) -> Ordering + Sync) {
    if items.len() >= PAR_SORT_MIN {
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get().min(8));
        let size = items.len().div_ceil(threads);
        std::thread::scope(|scope| {
            for chunk in items.chunks_mut(size) {
                let cmp = &cmp;
                scope.spawn(move || chunk.sort_by(|a, b| cmp(a, b)));
            }
        });
    }
    items.sort_by(|a, b| cmp(a, b));
}

/// Turns a built graph into the graph a reader decodes from `wiring.json`:
/// sorted, with no `body_text`, and with the counts of `meta` set.
pub fn into_written(graph: Graph) -> Graph {
    into_written_keeping(graph).0
}

/// Like [`into_written`], and also gives back what the strip took out. The
/// second value holds each written node's `body_text`, in written order. The
/// third value maps the index of each node in the input to its written index.
pub fn into_written_keeping(graph: Graph) -> (Graph, Vec<Option<String>>, Vec<u32>) {
    keep_sorted(graph, None)
}

/// Like [`into_written_keeping`], with the graph a reader decoded before as a
/// hint. A patch that keeps the node ids and the edge keys keeps their order,
/// so the hint gives the order with one check in place of a sort. The check
/// proves the order is sorted; a hint that fails it falls back to the sort. So
/// the result always equals [`into_written_keeping`].
pub fn into_written_keeping_hinted(
    graph: Graph,
    prior: &Graph,
) -> (Graph, Vec<Option<String>>, Vec<u32>) {
    keep_sorted(graph, Some(prior))
}

/// The node order of the prior graph applied to `nodes`, if it is the sorted
/// order. The index breaks ties, as in the sort.
fn hinted_node_order(nodes: &[Node], prior: &[Node]) -> Option<Vec<u32>> {
    if nodes.len() != prior.len() {
        return None;
    }
    let at: HashMap<&str, u32> = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id.as_str(), i as u32))
        .collect();
    if at.len() != nodes.len() {
        return None;
    }
    let order: Vec<u32> = prior
        .iter()
        .map(|n| at.get(n.id.as_str()).copied())
        .collect::<Option<_>>()?;
    let sorted = order.windows(2).all(|w| {
        node_cmp(&nodes[w[0] as usize], &nodes[w[1] as usize]).then(w[0].cmp(&w[1]))
            == Ordering::Less
    });
    sorted.then_some(order)
}

/// The edge order of the prior graph applied to `edges`, if it is the sorted
/// order. An edge key that repeats gives `None`.
fn hinted_edge_order(edges: &[Edge], prior: &[Edge]) -> Option<Vec<u32>> {
    if edges.len() != prior.len() {
        return None;
    }
    fn key(e: &Edge) -> (&str, &str, &str) {
        (e.source.as_str(), e.relation.as_str(), e.target.as_str())
    }
    let at: HashMap<(&str, &str, &str), u32> = edges
        .iter()
        .enumerate()
        .map(|(i, e)| (key(e), i as u32))
        .collect();
    if at.len() != edges.len() {
        return None;
    }
    let order: Vec<u32> = prior
        .iter()
        .map(|e| at.get(&key(e)).copied())
        .collect::<Option<_>>()?;
    let sorted = order.windows(2).all(|w| {
        edge_cmp(&edges[w[0] as usize], &edges[w[1] as usize]).then(w[0].cmp(&w[1]))
            == Ordering::Less
    });
    sorted.then_some(order)
}

/// Puts `items` in the order `order` lists (each index once).
fn permute<T>(items: Vec<T>, order: &[u32]) -> Vec<T> {
    let mut slots: Vec<Option<T>> = items.into_iter().map(Some).collect();
    order
        .iter()
        .filter_map(|at| slots[*at as usize].take())
        .collect()
}

fn keep_sorted(mut graph: Graph, prior: Option<&Graph>) -> (Graph, Vec<Option<String>>, Vec<u32>) {
    let hinted = prior.and_then(|p| hinted_node_order(&graph.nodes, &p.nodes));
    let order: Vec<u32> = hinted.unwrap_or_else(|| {
        let mut order: Vec<u32> = (0..graph.nodes.len() as u32).collect();
        let nodes = &graph.nodes;
        // The index breaks ties, so the order equals a stable sort of the nodes.
        par_sort_by(&mut order, |a, b| {
            node_cmp(&nodes[*a as usize], &nodes[*b as usize]).then(a.cmp(b))
        });
        order
    });
    let mut pos = vec![0u32; order.len()];
    for (written, old) in order.iter().enumerate() {
        pos[*old as usize] = written as u32;
    }
    let mut old: Vec<Option<Node>> = graph.nodes.drain(..).map(Some).collect();
    let mut bodies = Vec::with_capacity(order.len());
    for at in &order {
        // `order` holds each index once, so the slot is always full.
        let Some(mut node) = old[*at as usize].take() else {
            continue;
        };
        bodies.push(node.body_text.take());
        graph.nodes.push(node);
    }
    match prior.and_then(|p| hinted_edge_order(&graph.edges, &p.edges)) {
        Some(order) => graph.edges = permute(std::mem::take(&mut graph.edges), &order),
        None => par_sort_by(&mut graph.edges, edge_cmp),
    }
    graph.meta.version = 1;
    graph.meta.node_count = graph.nodes.len();
    graph.meta.edge_count = graph.edges.len();
    (graph, bodies, pos)
}

/// Writes `graph` to `path` as sorted, pretty JSON, atomically.
///
/// This function sorts nodes by `id` and edges by `(source, relation,
/// target)`, using ICU root collation for each field, strips `body_text`
/// from the written copy, and never changes `graph` itself. The write goes
/// through a `.<pid>.tmp` file and a rename, so a reader never sees a
/// partial file.
pub fn write_graph(graph: &Graph, path: &Path) -> io::Result<()> {
    write_graph_stamped(graph, path).map(|_| ())
}

/// Writes `graph` as `write_graph` does and returns the `(size, mtime_ms)`
/// of the temp file, read before the rename.
pub fn write_graph_stamped(graph: &Graph, path: &Path) -> io::Result<(u64, u64)> {
    write_graph_kept(graph, path).map(|(stamp, _)| stamp)
}

/// Writes like [`write_graph_stamped`] and also returns the graph as written:
/// sorted, with no `body_text`. A reader that decodes the file gets this graph.
pub fn write_graph_kept(graph: &Graph, path: &Path) -> io::Result<((u64, u64), Graph)> {
    let out = into_written(graph.clone());

    let mut json = serde_json::to_string_pretty(&out).map_err(io::Error::other)?;
    json.push('\n');

    let stamp = write_atomic_stamped(path, json.as_bytes())?;
    Ok((stamp, out))
}

/// Writes `bytes` to `path` atomically, through a `.<pid>.tmp` sibling file
/// and a rename, so a reader never sees a partial file.
///
/// Creates the parent directory when it does not exist yet.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_atomic_stamped(path, bytes).map(|_| ())
}

/// Writes like `write_atomic` and returns the `(size, mtime_ms)` of the
/// temp file before the rename.
fn write_atomic_stamped(path: &Path, bytes: &[u8]) -> io::Result<(u64, u64)> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }

    let tmp_path = tmp_path_for(path);
    let result = fs::write(&tmp_path, bytes)
        .and_then(|()| fs::metadata(&tmp_path))
        .and_then(|meta| {
            let stamp = crate::lookup::stamp_of(&meta);
            fs::rename(&tmp_path, path).map(|()| stamp)
        });
    if result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    result
}

/// Builds the `<path>.<pid>.tmp` sibling path used for the atomic write.
pub(crate) fn tmp_path_for(path: &Path) -> PathBuf {
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

    /// A hint from the written graph gives the bytes of the plain sort, for a
    /// graph that keeps its keys, one that changes them, and a wrong hint.
    #[test]
    fn a_hinted_sort_equals_the_plain_sort() {
        let g = sample_graph();
        let written = into_written(g.clone());
        let plain = into_written_keeping(g.clone());
        assert_eq!(into_written_keeping_hinted(g.clone(), &written), plain);
        // Reverse the input order: the hint still gives the sorted order.
        let mut rev = g.clone();
        rev.nodes.reverse();
        rev.edges.reverse();
        assert_eq!(
            into_written_keeping_hinted(rev.clone(), &written),
            into_written_keeping(rev.clone())
        );
        // A new node and an edge fewer: the hint fails and the sort runs.
        let mut grown = g.clone();
        let mut extra = grown.nodes[0].clone();
        extra.id = "zz/new.ts#fresh".to_string();
        grown.nodes.push(extra);
        grown.edges.pop();
        assert_eq!(
            into_written_keeping_hinted(grown.clone(), &written),
            into_written_keeping(grown.clone())
        );
        // An unsorted prior graph is a wrong hint.
        assert_eq!(
            into_written_keeping_hinted(g.clone(), &g),
            into_written_keeping(g.clone())
        );
    }

    #[test]
    fn into_written_keeping_maps_each_node_and_its_body() {
        let g = sample_graph();
        let (kept, bodies, pos) = into_written_keeping(g.clone());
        assert_eq!(kept, into_written(g.clone()));
        for (old, node) in g.nodes.iter().enumerate() {
            let at = pos[old] as usize;
            assert_eq!(kept.nodes[at].id, node.id);
            assert_eq!(bodies[at], node.body_text);
            assert!(kept.nodes[at].body_text.is_none());
        }
    }

    #[test]
    fn write_graph_stamped_returns_the_size_of_the_written_file() {
        let dir = unique_temp_dir("stamped");
        let path = dir.join("wiring.json");

        let (size, mtime) = write_graph_stamped(&sample_graph(), &path).expect("write succeeds");

        assert_eq!(size, fs::metadata(&path).expect("stat").len());
        assert_eq!(Some((size, mtime)), crate::lookup::wiring_stamp(&path));
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

    /// The threaded sort gives the order of the plain stable sort, ties included.
    #[test]
    fn par_sort_by_matches_the_stable_sort() {
        let mut items: Vec<(u32, u32)> = (0..10_000u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) % 97, i))
            .collect();
        let mut plain = items.clone();
        plain.sort_by_key(|a| a.0);
        par_sort_by(&mut items, |a, b| a.0.cmp(&b.0));
        assert_eq!(items, plain);
    }
}
