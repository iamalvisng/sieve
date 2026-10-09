//! The per-file lookup index `lookup.v1` (scale S3). A hook reads the nodes
//! and the incoming edges of one file without parsing the whole
//! `wiring.json`.
//!
//! Layout, one JSON value per line: the header, one record per file, the
//! `stale` id list, the table `[[path, offset, len], ...]`, then an 8-byte
//! little-endian `u64` with the byte offset of the table line.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::wiring::{
    start_line, Edge, Graph, Kind, Node, Relation, Scope, SummaryState, WALK_RELATIONS,
};
use crate::write::{edge_cmp, node_cmp, tmp_path_for};

/// The format version a reader accepts.
/// The default size cap, in bytes, on `wiring.json` for a hook load.
pub const DEFAULT_WIRING_CAP_BYTES: u64 = 64 * 1024 * 1024;

const VERSION: u32 = 1;
/// The most incoming edges a record keeps in `top`.
const TOP: usize = 8;
/// The size of the trailer in bytes.
const TRAILER: u64 = 8;

/// The first line of the lookup file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    /// The format version.
    pub v: u32,
    /// The `(size, mtime_ms)` stamp of the `wiring.json` this lookup matches.
    pub wiring: (u64, u64),
    /// The count of distinct node ids.
    pub nodes: usize,
    /// The count of File nodes.
    pub files: usize,
    /// The project scopes of the graph.
    pub scopes: Vec<Scope>,
}

/// One node of a file, as a record stores it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LookupNode {
    /// The node id.
    pub id: String,
    /// The node name.
    pub name: String,
    /// The node kind.
    pub kind: Kind,
    /// The `L<start>-L<end>` span.
    pub span: String,
    /// The signature, when the node has one.
    pub signature: Option<String>,
    /// The first line of the summary, trimmed, when it is not empty.
    pub summary: Option<String>,
    /// The count of walk-relation edges into the node.
    pub callers: usize,
    /// The body hash.
    pub body_hash: String,
}

/// The lookup record of one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// The file path.
    pub path: String,
    /// Every node of the path, in start-line order.
    pub nodes: Vec<LookupNode>,
    /// The count of incoming edges from another file.
    pub in_total: usize,
    /// The first incoming edges from another file, as `(relation, label)`.
    pub top: Vec<(Relation, String)>,
}

#[derive(Deserialize)]
struct RawNode(
    String,
    String,
    Kind,
    String,
    Option<String>,
    Option<String>,
    usize,
    String,
);

#[derive(Deserialize)]
struct RawRecord(String, Vec<RawNode>, usize, Vec<(Relation, String)>);

impl From<RawRecord> for Record {
    fn from(raw: RawRecord) -> Self {
        let nodes = raw
            .1
            .into_iter()
            .map(|n| LookupNode {
                id: n.0,
                name: n.1,
                kind: n.2,
                span: n.3,
                signature: n.4,
                summary: n.5,
                callers: n.6,
                body_hash: n.7,
            })
            .collect();
        Record {
            path: raw.0,
            nodes,
            in_total: raw.2,
            top: raw.3,
        }
    }
}

/// The `(size, mtime_ms)` stamp of a file's metadata.
pub fn stamp_of(meta: &fs::Metadata) -> (u64, u64) {
    let ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as u64);
    (meta.len(), ms)
}

/// The `(size, mtime_ms)` stamp of the `wiring.json` at `wiring_path`, or
/// `None` when the file is missing.
pub fn wiring_stamp(wiring_path: &Path) -> Option<(u64, u64)> {
    fs::metadata(wiring_path).ok().map(|m| stamp_of(&m))
}

/// The first line of `summary`, trimmed, or `None` when it is empty.
fn first_summary_line(summary: &str) -> Option<&str> {
    let first = summary.lines().next().unwrap_or("").trim();
    (!first.is_empty()).then_some(first)
}

/// The label of an incoming edge: `name (basename)`, or the source id when
/// the source node is missing.
fn label(edge: &Edge, by_id: &HashMap<&str, &Node>) -> String {
    match by_id.get(edge.source.as_str()) {
        Some(n) => {
            let base = Path::new(&n.path)
                .file_name()
                .map_or_else(|| n.path.clone(), |b| b.to_string_lossy().into_owned());
            format!("{} ({base})", n.name)
        }
        None => edge.source.clone(),
    }
}

/// Writes one JSON value and a newline, and returns the bytes written.
fn write_line<T: Serialize>(out: &mut impl Write, value: &T) -> io::Result<u64> {
    let mut buf = serde_json::to_vec(value).map_err(io::Error::other)?;
    buf.push(b'\n');
    out.write_all(&buf)?;
    Ok(buf.len() as u64)
}

/// Writes the lookup of `graph` to `path`, atomically, through a
/// `.<pid>.tmp` file and a rename.
///
/// `wiring_stamp` is the stamp of the `wiring.json` that matches `graph`.
/// An incoming edge counts as cross-file when its source node is in another
/// path or is missing.
pub fn write(graph: &Graph, path: &Path, wiring_stamp: (u64, u64)) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let tmp = tmp_path_for(path);
    let result = write_tmp(graph, &tmp, wiring_stamp).and_then(|()| fs::rename(&tmp, path));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Streams the whole lookup into `tmp`.
fn write_tmp(graph: &Graph, tmp: &Path, wiring_stamp: (u64, u64)) -> io::Result<()> {
    let by_id: HashMap<&str, &Node> = graph.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut by_path: BTreeMap<&str, Vec<&Node>> = BTreeMap::new();
    for node in &graph.nodes {
        by_path.entry(node.path.as_str()).or_default().push(node);
    }
    let mut callers: HashMap<&str, usize> = HashMap::new();
    let mut incoming: HashMap<&str, Vec<&Edge>> = HashMap::new();
    for edge in &graph.edges {
        if WALK_RELATIONS.contains(&edge.relation) {
            *callers.entry(edge.target.as_str()).or_default() += 1;
        }
        let Some(target) = by_id.get(edge.target.as_str()) else {
            continue;
        };
        let cross = match by_id.get(edge.source.as_str()) {
            Some(source) => source.path != target.path,
            None => true,
        };
        if cross {
            incoming.entry(target.path.as_str()).or_default().push(edge);
        }
    }
    let mut stale: Vec<&str> = graph
        .nodes
        .iter()
        .filter(|n| n.summary_state == SummaryState::Stale)
        .map(|n| n.id.as_str())
        .collect();
    stale.sort_unstable();
    stale.dedup();

    let header = Header {
        v: VERSION,
        wiring: wiring_stamp,
        nodes: by_id.len(),
        files: graph.nodes.iter().filter(|n| n.kind == Kind::File).count(),
        scopes: graph.meta.scopes.clone(),
    };
    let mut out = BufWriter::new(File::create(tmp)?);
    let mut pos = write_line(&mut out, &header)?;
    let mut table: Vec<(&str, u64, u64)> = Vec::with_capacity(by_path.len());
    for (file, mut nodes) in by_path {
        nodes.sort_by(|a, b| node_cmp(a, b));
        nodes.sort_by_key(|n| start_line(&n.span));
        let rows: Vec<_> = nodes
            .iter()
            .map(|n| {
                (
                    n.id.as_str(),
                    n.name.as_str(),
                    n.kind,
                    n.span.as_str(),
                    n.signature.as_deref(),
                    n.summary.as_deref().and_then(first_summary_line),
                    callers.get(n.id.as_str()).copied().unwrap_or(0),
                    n.body_hash.as_str(),
                )
            })
            .collect();
        let mut edges = incoming.remove(file).unwrap_or_default();
        edges.sort_by(|a, b| edge_cmp(a, b));
        let top: Vec<(Relation, String)> = edges
            .iter()
            .take(TOP)
            .map(|e| (e.relation, label(e, &by_id)))
            .collect();
        let len = write_line(&mut out, &(file, &rows, edges.len(), &top))?;
        table.push((file, pos, len));
        pos += len;
    }
    let table_at = pos + write_line(&mut out, &stale)?;
    write_line(&mut out, &table)?;
    out.write_all(&table_at.to_le_bytes())?;
    out.flush()
}

/// An open lookup file. It keeps one handle for its whole life.
#[derive(Debug)]
pub struct Lookup {
    file: File,
    header: Header,
    table: Vec<(String, u64, u64)>,
    stale_at: u64,
    table_at: u64,
}

impl Lookup {
    /// Opens the lookup at `path` and reads its header and table.
    ///
    /// Gives `None` for a missing file, an unknown version, a bad trailer
    /// or table, a table over 64 MB, or a `wiring_stamp` that differs from
    /// the header stamp.
    pub fn open(path: &Path, wiring_stamp: (u64, u64)) -> Option<Lookup> {
        let mut file = File::open(path).ok()?;
        let size = file.metadata().ok()?.len();
        if size < TRAILER {
            return None;
        }
        let mut line = String::new();
        let header_len = BufReader::new(&file).read_line(&mut line).ok()? as u64;
        let header: Header = serde_json::from_str(&line).ok()?;
        if header.v != VERSION || header.wiring != wiring_stamp {
            return None;
        }
        file.seek(SeekFrom::Start(size - TRAILER)).ok()?;
        let mut trailer = [0u8; TRAILER as usize];
        file.read_exact(&mut trailer).ok()?;
        let table_at = u64::from_le_bytes(trailer);
        if table_at < header_len || table_at > size - TRAILER {
            return None;
        }
        if size - TRAILER - table_at > 64 << 20 {
            return None;
        }
        file.seek(SeekFrom::Start(table_at)).ok()?;
        let mut buf = vec![0u8; (size - TRAILER - table_at) as usize];
        file.read_exact(&mut buf).ok()?;
        let table: Vec<(String, u64, u64)> = serde_json::from_slice(&buf).ok()?;
        let mut stale_at = header_len;
        for (_, offset, len) in &table {
            let end = offset.checked_add(*len)?;
            if *offset < header_len || end > table_at {
                return None;
            }
            stale_at = stale_at.max(end);
        }
        Some(Lookup {
            file,
            header,
            table,
            stale_at,
            table_at,
        })
    }

    /// The header of the lookup.
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// The paths in the table, in byte order.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.table.iter().map(|(p, _, _)| p.as_str())
    }

    /// Reads the record of `path`, or `None` when the table has no such
    /// path or the record at that offset belongs to another path.
    pub fn record(&mut self, path: &str) -> Option<Record> {
        let i = self
            .table
            .binary_search_by(|(p, _, _)| p.as_str().cmp(path))
            .ok()?;
        let (offset, len) = (self.table[i].1, self.table[i].2);
        self.file.seek(SeekFrom::Start(offset)).ok()?;
        let mut buf = vec![0u8; len as usize];
        self.file.read_exact(&mut buf).ok()?;
        let raw: RawRecord = serde_json::from_slice(&buf).ok()?;
        (raw.0 == path).then(|| raw.into())
    }

    /// Reads the sorted ids of the nodes with a stale summary.
    pub fn stale_ids(&mut self) -> Option<Vec<String>> {
        self.file.seek(SeekFrom::Start(self.stale_at)).ok()?;
        let mut buf = vec![0u8; (self.table_at - self.stale_at) as usize];
        self.file.read_exact(&mut buf).ok()?;
        serde_json::from_slice(&buf).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wiring::{Confidence, Meta, Origin};
    use std::path::PathBuf;

    const STAMP: (u64, u64) = (10, 20);

    fn node(id: &str, name: &str, path: &str, kind: Kind, span: &str) -> Node {
        Node {
            id: id.to_string(),
            name: name.to_string(),
            kind,
            owner: None,
            path: path.to_string(),
            span: span.to_string(),
            signature: None,
            exported: true,
            origin: Origin::Ast,
            body_hash: "0".repeat(64),
            chars: None,
            body_text: None,
            arity: None,
            variadic: None,
            summary_state: SummaryState::Pending,
            summary: None,
            crux: None,
        }
    }

    fn func(id: &str, name: &str, path: &str) -> Node {
        node(id, name, path, Kind::Function, "L1-L2")
    }

    fn edge(source: &str, relation: Relation, target: &str) -> Edge {
        Edge {
            source: source.to_string(),
            target: target.to_string(),
            relation,
            confidence: Confidence::Extracted,
        }
    }

    fn graph(nodes: Vec<Node>, edges: Vec<Edge>) -> Graph {
        Graph {
            meta: Meta {
                version: 1,
                node_count: nodes.len(),
                edge_count: edges.len(),
                languages: vec![],
                scopes: vec![],
            },
            nodes,
            edges,
        }
    }

    fn written(g: &Graph) -> (crate::test_support::TempDir, PathBuf) {
        let dir = crate::test_support::TempDir::new("lookup");
        let path = dir.join("lookup.v1");
        write(g, &path, STAMP).expect("write succeeds");
        (dir, path)
    }

    fn small_graph() -> Graph {
        graph(
            vec![
                node("a.rs", "a.rs", "a.rs", Kind::File, "L1-L9"),
                func("a.rs#f", "f", "a.rs"),
                node("b.rs", "b.rs", "b.rs", Kind::File, "L1-L9"),
                func("b.rs#g", "g", "b.rs"),
            ],
            vec![
                edge("b.rs#g", Relation::Calls, "a.rs#f"),
                edge("a.rs#f", Relation::Calls, "a.rs#f"),
                edge("a.rs", Relation::Contains, "a.rs#f"),
            ],
        )
    }

    #[test]
    fn round_trip_gives_header_counts_and_one_record_per_path() {
        let (_dir, path) = written(&small_graph());
        let mut lookup = Lookup::open(&path, STAMP).expect("opens");

        assert_eq!(lookup.header().nodes, 4);
        assert_eq!(lookup.header().files, 2);
        assert_eq!(lookup.paths().collect::<Vec<_>>(), vec!["a.rs", "b.rs"]);
        let a = lookup.record("a.rs").expect("record a");
        assert_eq!(a.nodes.len(), 2);
        assert_eq!(a.in_total, 1);
        assert_eq!(a.top, vec![(Relation::Calls, "g (b.rs)".to_string())]);
        let f = a.nodes.iter().find(|n| n.id == "a.rs#f").expect("f");
        assert_eq!(f.callers, 2);
        let b = lookup.record("b.rs").expect("record b");
        assert_eq!(b.in_total, 0);
        assert!(lookup.record("c.rs").is_none());
        assert_eq!(lookup.stale_ids(), Some(vec![]));
    }

    #[test]
    fn stale_ids_come_back_sorted() {
        let mut g = small_graph();
        g.nodes[3].summary_state = SummaryState::Stale;
        g.nodes[1].summary_state = SummaryState::Stale;
        let (_dir, path) = written(&g);
        let mut lookup = Lookup::open(&path, STAMP).expect("opens");

        assert_eq!(
            lookup.stale_ids(),
            Some(vec!["a.rs#f".to_string(), "b.rs#g".to_string()])
        );
    }

    #[test]
    fn a_stamp_mismatch_gives_none() {
        let (_dir, path) = written(&small_graph());

        assert!(Lookup::open(&path, (10, 21)).is_none());
        assert!(Lookup::open(&path.with_extension("none"), STAMP).is_none());
    }

    #[test]
    fn open_of_a_truncated_file_is_none() {
        let (_dir, path) = written(&small_graph());
        let len = fs::metadata(&path).expect("stat").len();
        File::options()
            .write(true)
            .open(&path)
            .expect("open")
            .set_len(len / 2)
            .expect("cut");

        assert!(Lookup::open(&path, STAMP).is_none());
    }

    #[test]
    fn open_of_a_seven_byte_file_is_none() {
        let (_dir, path) = written(&small_graph());
        fs::write(&path, [b'x'; 7]).expect("write");

        assert!(Lookup::open(&path, STAMP).is_none());
    }

    #[test]
    fn an_unknown_version_gives_none() {
        let (_dir, path) = written(&small_graph());
        let mut bytes = fs::read(&path).expect("read");
        let at = bytes
            .windows(5)
            .position(|w| w == b"\"v\":1")
            .expect("version key");
        bytes[at + 4] = b'2';
        fs::write(&path, bytes).expect("write back");

        assert!(Lookup::open(&path, STAMP).is_none());
    }

    #[test]
    fn a_bad_trailer_gives_none() {
        let (_dir, path) = written(&small_graph());
        let mut bytes = fs::read(&path).expect("read");
        let n = bytes.len();
        bytes[n - 8..].fill(0xFF);
        fs::write(&path, bytes).expect("write back");

        assert!(Lookup::open(&path, STAMP).is_none());
    }

    #[test]
    fn a_table_offset_that_points_at_another_record_gives_none() {
        let g = graph(
            vec![func("a.rs#f", "f", "a.rs"), func("c.rs#h", "h", "c.rs")],
            vec![],
        );
        let (_dir, path) = written(&g);
        let mut bytes = fs::read(&path).expect("read");
        let n = bytes.len();
        let mut trailer = [0u8; 8];
        trailer.copy_from_slice(&bytes[n - 8..]);
        let at = u64::from_le_bytes(trailer) as usize;
        let tail = String::from_utf8(bytes[at..n - 8].to_vec()).expect("table is text");
        let tail = tail.replacen("\"a.rs\"", "\"b.rs\"", 1);
        let _ = bytes.splice(at..n - 8, tail.into_bytes());
        fs::write(&path, bytes).expect("write back");
        let mut lookup = Lookup::open(&path, STAMP).expect("opens");

        assert!(lookup.record("b.rs").is_none());
        assert!(lookup.record("c.rs").is_some());
    }

    #[test]
    fn nodes_sort_by_collated_id_then_stable_by_start_line() {
        let g = graph(
            vec![
                node("a.rs#z", "z", "a.rs", Kind::Function, "L1-L2"),
                node("a.rs#a", "a", "a.rs", Kind::Function, "L5-L6"),
                node("a.rs#bad", "bad", "a.rs", Kind::Function, "oops"),
                node("a.rs", "a.rs", "a.rs", Kind::File, "L1-L9"),
            ],
            vec![],
        );
        let (_dir, path) = written(&g);
        let mut lookup = Lookup::open(&path, STAMP).expect("opens");

        let ids: Vec<String> = lookup
            .record("a.rs")
            .expect("record")
            .nodes
            .into_iter()
            .map(|n| n.id)
            .collect();
        assert_eq!(ids, vec!["a.rs#bad", "a.rs", "a.rs#z", "a.rs#a"]);
    }

    #[test]
    fn top_follows_edge_order_and_stops_at_eight() {
        let mut nodes = vec![func("a.rs#t", "t", "a.rs")];
        let mut edges = Vec::new();
        for i in (0..10).rev() {
            nodes.push(func(&format!("b.rs#s{i}"), &format!("s{i}"), "b.rs"));
            edges.push(edge(&format!("b.rs#s{i}"), Relation::Calls, "a.rs#t"));
        }
        let (_dir, path) = written(&graph(nodes, edges));
        let mut lookup = Lookup::open(&path, STAMP).expect("opens");

        let record = lookup.record("a.rs").expect("record");
        let labels: Vec<&str> = record.top.iter().map(|(_, l)| l.as_str()).collect();
        assert_eq!(record.in_total, 10);
        assert_eq!(labels.len(), 8);
        assert_eq!(labels[0], "s0 (b.rs)");
        assert_eq!(labels[7], "s7 (b.rs)");
    }

    #[test]
    fn a_dangling_source_counts_and_labels_with_the_source_id() {
        let g = graph(
            vec![func("a.rs#t", "t", "a.rs")],
            vec![edge("ghost#x", Relation::Calls, "a.rs#t")],
        );
        let (_dir, path) = written(&g);
        let mut lookup = Lookup::open(&path, STAMP).expect("opens");

        let record = lookup.record("a.rs").expect("record");
        assert_eq!(record.in_total, 1);
        assert_eq!(record.top, vec![(Relation::Calls, "ghost#x".to_string())]);
    }
}
