//! `map`: a deterministic, token-budgeted repo orientation — directory
//! clusters, per-directory hubs, and global hotspots — computed purely from the
//! wiring graph (the `query-commands.md` note section 4).

use std::collections::HashMap;
use std::path::Path;

use serde::Serialize;
use sieve_core::collate::collate;
use sieve_core::wiring::Scope;
use sieve_core::{Graph, Kind, Node};

use crate::relations::compute_in_degree;

/// A single group must not exceed this share of all file nodes, or it is
/// refined one path segment deeper.
const SPLIT_THRESHOLD: f64 = 0.6;

/// The width `formatDirLine` pads a directory label to, in UTF-16 code
/// units.
const DIR_COL_WIDTH: usize = 20;

/// The flags one `map` run carries: how many directories, hubs per
/// directory, and global hotspots it keeps.
#[derive(Debug, Clone, Copy)]
pub struct MapOptions {
    pub max_dirs: usize,
    pub hubs_per_dir: usize,
    pub hotspots: usize,
}

impl Default for MapOptions {
    fn default() -> Self {
        MapOptions {
            max_dirs: 16,
            hubs_per_dir: 3,
            hotspots: 12,
        }
    }
}

/// One directory's or the whole repo's top symbol by incoming
/// `WALK_RELATIONS` edges.
#[derive(Debug, Clone, Serialize)]
pub struct Hub {
    pub name: String,
    pub kind: Kind,
    pub path: String,
    pub span: String,
    #[serde(rename = "inDegree")]
    pub in_degree: usize,
}

/// One directory's own file, symbol, language, and hub breakdown.
#[derive(Debug, Clone, Serialize)]
pub struct DirEntry {
    pub path: String,
    pub files: usize,
    pub symbols: usize,
    pub languages: Vec<String>,
    pub hubs: Vec<Hub>,
    #[serde(rename = "isFile")]
    pub is_file: bool,
}

/// One scope's own directory breakdown, on a multi-scope repo.
#[derive(Debug, Clone, Serialize)]
pub struct ScopeGroup {
    pub scope: String,
    pub dirs: Vec<DirEntry>,
    pub dropped: usize,
}

/// The whole-repo counts a [`RepoMap`] heads its report with.
#[derive(Debug, Clone, Serialize)]
pub struct Totals {
    pub files: usize,
    pub symbols: usize,
    pub edges: usize,
    pub languages: Vec<String>,
}

/// The full result of one `map` run.
#[derive(Debug, Clone, Serialize)]
pub struct RepoMap {
    pub totals: Totals,
    pub dirs: Vec<DirEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scopes: Option<Vec<ScopeGroup>>,
    pub hotspots: Vec<Hub>,
    pub dropped: usize,
}

/// The nearest-prefix scope owner of `path` among `scopes`, with the
/// empty-prefix root as the fallback.
///
/// Copied from `ask::fuse::scope_of`, which is private to the `ask`
/// module, so this crate adds no new cross-module coupling for one small
/// helper.
fn scope_of(path: &str, scopes: &[Scope]) -> String {
    let mut best: Option<&str> = None;
    for s in scopes {
        if s.prefix.is_empty() {
            continue;
        }
        let under_prefix = path == s.prefix || path.starts_with(&format!("{}/", s.prefix));
        if under_prefix && best.is_none_or(|b| s.prefix.len() > b.len()) {
            best = Some(s.prefix.as_str());
        }
    }
    best.map(str::to_string).unwrap_or_default()
}

/// Renders a scope prefix the way the `scopeLabel` does.
fn scope_label(prefix: &str) -> String {
    if prefix.is_empty() {
        "(root)".to_string()
    } else {
        format!("{prefix}/")
    }
}

/// Renders a [`Kind`] the way it serializes to JSON: a lowercase word.
fn kind_str(kind: Kind) -> &'static str {
    match kind {
        Kind::File => "file",
        Kind::Class => "class",
        Kind::Function => "function",
        Kind::Method => "method",
        Kind::Interface => "interface",
        Kind::Type => "type",
        Kind::Enum => "enum",
        Kind::Struct => "struct",
        Kind::Trait => "trait",
        Kind::Module => "module",
        Kind::Constant => "constant",
        Kind::Variable => "variable",
    }
}

/// The first `min(depth, segments.len())` path segments of `path`, joined
/// back with `/`. A path with fewer segments than `depth` (a root-level
/// file at depth 2, say) just uses every segment it has.
fn dir_key(path: &str, depth: usize) -> String {
    let segments: Vec<&str> = path.split('/').collect();
    let take = depth.min(segments.len());
    segments[..take].join("/")
}

/// Top `cap` nodes by in-degree, dropping a node with zero inbound edges,
/// ties broken by `collate(name)` then `collate(path)`.
fn top_hubs(nodes: &[&Node], in_degree: &HashMap<String, usize>, cap: usize) -> Vec<Hub> {
    let mut hubs: Vec<Hub> = nodes
        .iter()
        .map(|n| Hub {
            name: n.name.clone(),
            kind: n.kind,
            path: n.path.clone(),
            span: n.span.clone(),
            in_degree: *in_degree.get(&n.id).unwrap_or(&0),
        })
        .filter(|h| h.in_degree > 0)
        .collect();
    hubs.sort_by(|a, b| {
        b.in_degree
            .cmp(&a.in_degree)
            .then_with(|| collate(&a.name, &b.name))
            .then_with(|| collate(&a.path, &b.path))
    });
    hubs.truncate(cap);
    hubs
}

/// Display labels for every distinct language among `paths`, plain byte
/// order (never `collate` — the language sort uses the default
/// `Array.sort()`).
fn sorted_languages(paths: &[&str]) -> Vec<String> {
    let mut set: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for p in paths {
        if let Some(lang) = sieve_core::lang::lang_for_path(Path::new(p)) {
            set.insert(lang.label);
        }
    }
    let mut out: Vec<String> = set.into_iter().map(str::to_string).collect();
    out.sort();
    out
}

/// The directory-clustering core, run once over the whole graph
/// (single-scope) or once per scope's own node subset (multi-scope).
///
/// `strip_prefix` is the scope prefix to strip before computing
/// depth/split ("" for single-scope / the root scope). Reported
/// `DirEntry.path`s are always repo-rooted: the prefix is reattached.
fn compute_dir_entries(
    nodes: &[&Node],
    in_degree: &HashMap<String, usize>,
    max_dirs: usize,
    hubs_per_dir: usize,
    strip_prefix: &str,
) -> (Vec<DirEntry>, usize) {
    let rel_path = |path: &str| -> String {
        if strip_prefix.is_empty() {
            path.to_string()
        } else if path == strip_prefix {
            String::new()
        } else {
            path[strip_prefix.len() + 1..].to_string()
        }
    };

    let file_nodes: Vec<&&Node> = nodes.iter().filter(|n| n.kind == Kind::File).collect();
    let total_files = file_nodes.len();

    // Pass 1: depth-1 file counts, to find the (at most one) group over
    // the split threshold.
    let mut depth1_counts: HashMap<String, usize> = HashMap::new();
    for n in &file_nodes {
        let key = dir_key(&rel_path(&n.path), 1);
        *depth1_counts.entry(key).or_insert(0) += 1;
    }
    let mut split_segment: Option<String> = None;
    if total_files > 0 {
        for (seg, count) in &depth1_counts {
            if (*count as f64) / (total_files as f64) > SPLIT_THRESHOLD {
                split_segment = Some(seg.clone());
                break;
            }
        }
    }

    let depth_for = |rp: &str| -> usize {
        match &split_segment {
            Some(seg) if dir_key(rp, 1) == *seg => 2,
            _ => 1,
        }
    };

    // Pass 2: assign every node, file and symbol alike, to its group, in
    // node order.
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, (Vec<&Node>, Vec<&Node>)> = HashMap::new();
    for &n in nodes {
        let rp = rel_path(&n.path);
        let key = dir_key(&rp, depth_for(&rp));
        let entry = groups.entry(key.clone()).or_insert_with(|| {
            order.push(key.clone());
            (Vec::new(), Vec::new())
        });
        if n.kind == Kind::File {
            entry.0.push(n);
        } else {
            entry.1.push(n);
        }
    }

    let full_path = |rel_key: &str| -> String {
        if strip_prefix.is_empty() {
            rel_key.to_string()
        } else if rel_key.is_empty() {
            strip_prefix.to_string()
        } else {
            format!("{strip_prefix}/{rel_key}")
        }
    };

    let file_rel_paths: std::collections::HashSet<String> =
        file_nodes.iter().map(|n| rel_path(&n.path)).collect();

    let mut dir_entries: Vec<DirEntry> = order
        .into_iter()
        .map(|key| {
            let (files, symbols) = groups.remove(&key).expect("key came from the same loop");
            let file_paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
            DirEntry {
                path: full_path(&key),
                files: files.len(),
                symbols: symbols.len(),
                languages: sorted_languages(&file_paths),
                hubs: top_hubs(&symbols, in_degree, hubs_per_dir),
                is_file: file_rel_paths.contains(&key),
            }
        })
        .collect();

    dir_entries.sort_by(|a, b| {
        b.symbols
            .cmp(&a.symbols)
            .then_with(|| collate(&a.path, &b.path))
    });
    let dropped = dir_entries.len().saturating_sub(max_dirs);
    dir_entries.truncate(max_dirs);
    (dir_entries, dropped)
}

/// Builds the repo map from an already-loaded [`Graph`].
///
/// Deterministic: the same graph gives the same [`RepoMap`], modulo
/// insertion order, which is never observed — everything meaningful is
/// sorted.
pub fn build_repo_map(graph: &Graph, opts: &MapOptions) -> RepoMap {
    let file_nodes: Vec<&Node> = graph
        .nodes
        .iter()
        .filter(|n| n.kind == Kind::File)
        .collect();
    let total_files = file_nodes.len();
    let in_degree = compute_in_degree(graph);

    let scopes = &graph.meta.scopes;
    let (dirs, dropped, scope_groups) = if scopes.len() <= 1 {
        let all_nodes: Vec<&Node> = graph.nodes.iter().collect();
        let (dirs, dropped) =
            compute_dir_entries(&all_nodes, &in_degree, opts.max_dirs, opts.hubs_per_dir, "");
        (dirs, dropped, None)
    } else {
        let scope_groups: Vec<ScopeGroup> = scopes
            .iter()
            .map(|s| {
                let nodes_in_scope: Vec<&Node> = graph
                    .nodes
                    .iter()
                    .filter(|n| scope_of(&n.path, scopes) == s.prefix)
                    .collect();
                let (dirs, dropped) = compute_dir_entries(
                    &nodes_in_scope,
                    &in_degree,
                    opts.max_dirs,
                    opts.hubs_per_dir,
                    &s.prefix,
                );
                ScopeGroup {
                    scope: scope_label(&s.prefix),
                    dirs,
                    dropped,
                }
            })
            .collect();
        (Vec::new(), 0, Some(scope_groups))
    };

    let all_symbols: Vec<&Node> = graph
        .nodes
        .iter()
        .filter(|n| n.kind != Kind::File)
        .collect();
    let hotspots = top_hubs(&all_symbols, &in_degree, opts.hotspots);

    let file_paths: Vec<&str> = file_nodes.iter().map(|n| n.path.as_str()).collect();
    RepoMap {
        totals: Totals {
            files: total_files,
            symbols: all_symbols.len(),
            edges: graph.edges.len(),
            languages: sorted_languages(&file_paths),
        },
        dirs,
        scopes: scope_groups,
        hotspots,
        dropped,
    }
}

/// Every indexed file path — the savings baseline a `map` run reads
/// whole, for the CLI to attach as `saved`.
pub fn map_saved_paths(graph: &Graph) -> Vec<String> {
    graph
        .nodes
        .iter()
        .filter(|n| n.kind == Kind::File)
        .map(|n| n.path.clone())
        .collect()
}

fn basename_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[i + 1..],
        None => path,
    }
}

/// Pads `s` to `width` UTF-16 code units with trailing spaces. Leaves `s`
/// unchanged when it is already at or past `width`, the same quirk
/// JavaScript's `String.prototype.padEnd` has.
fn pad_end_utf16(s: &str, width: usize) -> String {
    let len = s.encode_utf16().count();
    if len >= width {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(width - len))
    }
}

fn format_dir_hub(h: &Hub) -> String {
    format!("{} ({}, {}←)", h.name, basename_of(&h.path), h.in_degree)
}

fn format_dir_line(d: &DirEntry) -> String {
    let label = if d.is_file {
        d.path.clone()
    } else {
        format!("{}/", d.path)
    };
    let label = pad_end_utf16(&label, DIR_COL_WIDTH);
    let counts = format!(
        "{} \u{b7} {}",
        sieve_core::voice::count(d.files, "file"),
        sieve_core::voice::count(d.symbols, "symbol")
    );
    let hubs = if d.hubs.is_empty() {
        String::new()
    } else {
        let joined: Vec<String> = d.hubs.iter().map(format_dir_hub).collect();
        format!("   hubs: {}", joined.join(", "))
    };
    format!("{label}{counts}{hubs}")
}

fn format_hotspot(h: &Hub) -> String {
    let links = if h.in_degree == 1 { "link" } else { "links" };
    format!(
        "{}  \u{b7} {} {links} in",
        sieve_core::voice::row(&h.name, kind_str(h.kind), &h.path, Some(&h.span)),
        h.in_degree
    )
}

/// The "+N more directories not shown" note, or `None` when nothing was
/// dropped.
fn dropped_note(dropped: usize) -> Option<String> {
    if dropped == 0 {
        return None;
    }
    let word = if dropped == 1 { "y" } else { "ies" };
    Some(format!(
        "… +{dropped} more director{word} not shown (raise max-dirs to see more)"
    ))
}

/// Renders a [`RepoMap`], without the savings header (the CLI prepends that).
pub fn format_repo_map(map: &RepoMap) -> String {
    let totals = &map.totals;
    let header = format!(
        "map \u{b7} {} \u{b7} {} \u{b7} {} \u{b7} {}",
        sieve_core::voice::count(totals.files, "file"),
        sieve_core::voice::count(totals.symbols, "symbol"),
        sieve_core::voice::count(totals.edges, "link"),
        totals.languages.join(", ")
    );

    let mut lines: Vec<String> = vec![header, String::new()];
    if let Some(scopes) = &map.scopes {
        for sg in scopes {
            lines.push(format!("## {}", sg.scope));
            for d in &sg.dirs {
                lines.push(format_dir_line(d));
            }
            if let Some(note) = dropped_note(sg.dropped) {
                lines.push(note);
            }
            lines.push(String::new());
        }
    } else {
        for d in &map.dirs {
            lines.push(format_dir_line(d));
        }
        if let Some(note) = dropped_note(map.dropped) {
            lines.push(note);
        }
        lines.push(String::new());
    }
    if map.hotspots.is_empty() {
        lines.push("hotspots: none".to_string());
    } else {
        lines.push("hotspots".to_string());
        lines.extend(map.hotspots.iter().map(format_hotspot));
    }

    format!("{}\n", lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sieve_core::{Confidence, Edge, Meta, Origin, SummaryState};

    fn file_node(path: &str) -> Node {
        Node {
            id: path.to_string(),
            name: path.to_string(),
            kind: Kind::File,
            owner: None,
            path: path.to_string(),
            span: "L1-L1".to_string(),
            signature: None,
            exported: false,
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

    fn symbol_node(id: &str, name: &str, kind: Kind, path: &str, start: u32, end: u32) -> Node {
        Node {
            id: id.to_string(),
            name: name.to_string(),
            kind,
            owner: None,
            path: path.to_string(),
            span: format!("L{start}-L{end}"),
            signature: None,
            exported: false,
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

    fn empty_graph(nodes: Vec<Node>, edges: Vec<Edge>) -> Graph {
        Graph {
            meta: Meta {
                version: 1,
                node_count: nodes.len(),
                edge_count: edges.len(),
                languages: Vec::new(),
                scopes: Vec::new(),
            },
            nodes,
            edges,
        }
    }

    #[test]
    fn pad_end_gives_no_space_before_the_count_at_or_past_twenty_columns() {
        let label21 = "a".repeat(21);
        assert_eq!(pad_end_utf16(&label21, DIR_COL_WIDTH), label21);

        let label19 = "a".repeat(19);
        let padded = pad_end_utf16(&label19, DIR_COL_WIDTH);
        assert_eq!(padded.len(), DIR_COL_WIDTH);
        assert!(padded.ends_with(' '));
    }

    /// P1-29: the map pads the directory column with JavaScript's
    /// `padEnd`, so the width counts UTF-16 units, not chars.
    #[test]
    fn test_p1_29_pad_end_counts_utf16_units_not_chars() {
        // 9 astral chars are 18 units; with "/" that is 19, so one space.
        let nine_astral = format!("{}/", "🚀".repeat(9));
        assert_eq!(
            pad_end_utf16(&nine_astral, DIR_COL_WIDTH),
            format!("{nine_astral} ")
        );

        // 10 astral chars are 20 units; with "/" that is 21, so no space.
        let ten_astral = format!("{}/", "🚀".repeat(10));
        assert_eq!(pad_end_utf16(&ten_astral, DIR_COL_WIDTH), ten_astral);

        // 18 "é" are 18 units; with "/" that is 19, so one space.
        let bmp = format!("{}/", "é".repeat(18));
        assert_eq!(pad_end_utf16(&bmp, DIR_COL_WIDTH), format!("{bmp} "));
    }

    #[test]
    fn the_split_rule_breaks_at_exactly_sixty_percent_but_not_at_the_boundary() {
        // 3 of 5 files under "a": 3/5 == 0.6, no split, so "a/x1.ts" stays
        // at depth 1.
        let at_boundary = [
            file_node("a/x1.ts"),
            file_node("a/x2.ts"),
            file_node("a/x3.ts"),
            file_node("b/y1.ts"),
            file_node("b/y2.ts"),
        ];
        let nodes: Vec<&Node> = at_boundary.iter().collect();
        let in_degree = HashMap::new();
        let (dirs, _) = compute_dir_entries(&nodes, &in_degree, 16, 3, "");
        assert!(dirs.iter().any(|d| d.path == "a" && d.files == 3));

        // 4 of 6 files under "a": 4/6 > 0.6, splits one level deeper, so
        // each root-level file under "a" becomes its own group.
        let above_boundary = [
            file_node("a/x1.ts"),
            file_node("a/x2.ts"),
            file_node("a/x3.ts"),
            file_node("a/x4.ts"),
            file_node("b/y1.ts"),
            file_node("b/y2.ts"),
        ];
        let nodes: Vec<&Node> = above_boundary.iter().collect();
        let (dirs, _) = compute_dir_entries(&nodes, &in_degree, 16, 3, "");
        assert!(dirs.iter().all(|d| d.path != "a"));
        assert!(dirs.iter().any(|d| d.path == "a/x1.ts" && d.is_file));
    }

    #[test]
    fn the_counts_singularize_at_one() {
        let d = DirEntry {
            path: "src".to_string(),
            files: 1,
            symbols: 1,
            languages: Vec::new(),
            hubs: Vec::new(),
            is_file: false,
        };
        assert!(format_dir_line(&d).contains("1 file · 1 symbol"));
    }

    #[test]
    fn an_empty_hotspot_list_still_prints_the_hotspots_label() {
        let map = RepoMap {
            totals: Totals {
                files: 0,
                symbols: 0,
                edges: 0,
                languages: Vec::new(),
            },
            dirs: Vec::new(),
            scopes: None,
            hotspots: Vec::new(),
            dropped: 0,
        };
        let rendered = format_repo_map(&map);
        assert!(rendered.contains("hotspots: none\n"));
    }

    #[test]
    fn in_degree_zero_hubs_are_dropped_and_ties_break_by_name_then_path() {
        let a = symbol_node("a.ts#b", "b", Kind::Function, "a.ts", 1, 1);
        let c = symbol_node("a.ts#a", "a", Kind::Function, "a.ts", 2, 2);
        let z = symbol_node("a.ts#z", "z", Kind::Function, "a.ts", 3, 3);
        let nodes = vec![&a, &c, &z];
        let mut in_degree = HashMap::new();
        in_degree.insert("a.ts#b".to_string(), 1);
        in_degree.insert("a.ts#a".to_string(), 1);
        let hubs = top_hubs(&nodes, &in_degree, 16);
        assert_eq!(hubs.len(), 2);
        assert_eq!(hubs[0].name, "a");
        assert_eq!(hubs[1].name, "b");
    }

    #[test]
    fn map_saved_paths_lists_every_file_node_path() {
        let graph = empty_graph(vec![file_node("a.ts"), file_node("b.ts")], Vec::new());
        assert_eq!(map_saved_paths(&graph), vec!["a.ts", "b.ts"]);
    }

    #[test]
    fn build_repo_map_counts_hubs_from_walk_relation_in_degree() {
        let f = file_node("a.ts");
        let sym = symbol_node("a.ts#run", "run", Kind::Function, "a.ts", 1, 2);
        let edge = Edge {
            source: "a.ts#caller".to_string(),
            target: "a.ts#run".to_string(),
            relation: sieve_core::Relation::Calls,
            confidence: Confidence::Extracted,
        };
        let graph = empty_graph(vec![f, sym], vec![edge]);
        let map = build_repo_map(&graph, &MapOptions::default());
        assert_eq!(map.totals.files, 1);
        assert_eq!(map.totals.symbols, 1);
        assert_eq!(map.dirs.len(), 1);
        assert_eq!(map.dirs[0].hubs.len(), 1);
        assert_eq!(map.dirs[0].hubs[0].name, "run");
    }
}
