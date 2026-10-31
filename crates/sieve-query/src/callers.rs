//! `callers`: who calls, references, implements or extends a symbol (or,
//! with `--direction out`, what it calls), walked over the graph
//!.
//!
//! Symbol resolution reuses [`crate::ask::structural::resolve_symbol`], the
//! same three-pass resolution `ask`'s structural mode
//! already needs. The edge walk and the render below are this module's own,
//! mirroring and.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;
use serde::Serialize;
use sieve_core::{Graph, Kind, Node, Relation};
use thiserror::Error;

use crate::ask::structural::{candidate_id, resolve_symbol_with as resolve_symbol_matches_with};
#[cfg(test)]
use crate::grep::grep_graph;
use crate::grep::{compile_pattern, decode_file_bytes, escape_regex, GrepOptions, MAX_HIT_TEXT};
use crate::relations::WALK_RELATIONS;

/// Which way `callers` walks the graph: `In` = who points at the symbol,
/// `Out` = what the symbol points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    In,
    Out,
}

/// The `--depth` a walk runs to. `None` is the unbounded `all` closure
///.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Depth(pub Option<usize>);

/// Every way `callers` can fail.
#[derive(Debug, Error)]
pub enum CallersError {
    #[error("--direction must be in or out, got {raw} \u{2014} try --direction in")]
    BadDirection { raw: String },
    #[error("--depth must be a positive number or all, got {raw} \u{2014} try --depth 2")]
    BadDepth { raw: String },
    #[error("{}", prefix_not_indexed_message(prefix, scopes))]
    PrefixNotIndexed { prefix: String, scopes: Vec<String> },
    #[error("{}", no_symbol_message(query))]
    NoSymbol { query: String },
}

/// Renders the `NoSymbol` message, naming the active product's build
/// command.
fn no_symbol_message(query: &str) -> String {
    format!(
        "no symbol named {query} — try {} find {query}",
        sieve_core::product().name
    )
}

/// Renders the `PrefixNotIndexed` message, matching `grep`'s wording
///.
fn prefix_not_indexed_message(prefix: &str, scopes: &[String]) -> String {
    let clause = if scopes.len() > 1 {
        format!(" (scopes here: {})", scopes.join(" · "))
    } else {
        String::new()
    };
    format!(
        "nothing is indexed under {prefix}/{clause} — try {} map",
        sieve_core::product().name
    )
}

/// Renders a scope prefix the way the `scopeLabel` does.
fn scope_label(prefix: &str) -> String {
    if prefix.is_empty() {
        "(root)".to_string()
    } else {
        format!("{prefix}/")
    }
}

/// Strips a leading `./` (repeatedly), maps a platform separator to `/`,
/// and strips every trailing `/` from a `--in` prefix. Matches Sieve's
/// `normalizePathPrefix`: a leading slash is not
/// special on its own — only a wholly-slash string (`"/"`, `"///"`) ends
/// up empty, through the trailing-slash strip.
fn normalize_path_prefix(prefix: &str) -> String {
    let mut out = if cfg!(windows) {
        prefix.replace('\\', "/")
    } else {
        prefix.to_string()
    };
    while let Some(rest) = out.strip_prefix("./") {
        out = rest.to_string();
    }
    out.trim_end_matches('/').to_string()
}

/// Segment-aware prefix match, mirroring `pathUnderPrefix`
///: an empty prefix matches every path.
fn path_under_prefix(path: &str, prefix: &str) -> bool {
    prefix.is_empty() || path == prefix || path.starts_with(&format!("{prefix}/"))
}

fn all_depth_re() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i-u)^(all|full|max)$").ok())
        .as_ref()
}

/// Parses a raw `--depth` value with JavaScript `Number()` semantics: `all`
/// (or `full`/`max`, case-insensitively) is unbounded; otherwise a trimmed
/// decimal or exponent form, floored, rejecting anything non-finite or
/// below `1`.
pub fn parse_depth(raw: &str) -> Result<Depth, CallersError> {
    if all_depth_re().is_some_and(|re| re.is_match(raw)) {
        return Ok(Depth(None));
    }
    let trimmed = raw.trim();
    let value: f64 = if trimmed.is_empty() {
        0.0
    } else {
        trimmed.parse().unwrap_or(f64::NAN)
    };
    if !value.is_finite() || value < 1.0 {
        return Err(CallersError::BadDepth {
            raw: raw.to_string(),
        });
    }
    Ok(Depth(Some(value.floor() as usize)))
}

/// Parses a raw `--direction` value: only `in` and `out`, case-sensitively
///.
pub fn parse_direction(raw: &str) -> Result<Direction, CallersError> {
    match raw {
        "in" => Ok(Direction::In),
        "out" => Ok(Direction::Out),
        _ => Err(CallersError::BadDirection {
            raw: raw.to_string(),
        }),
    }
}

/// Resolves `query` to every matching node, then narrows to `in_prefix`
/// when given, erroring the way `grep`'s `--in` does when the prefix is
/// not indexed at all, and when `--in` narrows the result to nothing
///.
pub fn resolve_symbol<'a>(
    graph: &'a Graph,
    query: &str,
    in_prefix: Option<&str>,
) -> Result<Vec<&'a Node>, CallersError> {
    resolve_symbol_with(graph, query, in_prefix, None)
}

/// The per-graph lookups that `callers` rebuilds on every call: each node's
/// lowercase name and candidate id, and an id map. A warm process builds one
/// per graph version and passes it to [`resolve_symbol_with`] and
/// [`edge_walk_with`]. The answers equal the ones without an index.
#[derive(Debug)]
pub struct GraphIndex {
    pub(crate) lower_name: Vec<Box<str>>,
    pub(crate) candidate: Vec<Box<str>>,
    by_id: HashMap<Box<str>, u32>,
}

impl GraphIndex {
    /// Builds the lookups for `graph`.
    pub fn build(graph: &Graph) -> GraphIndex {
        let lower_name = graph
            .nodes
            .iter()
            .map(|n| n.name.to_lowercase().into())
            .collect();
        let candidate = graph
            .nodes
            .iter()
            .map(|n| {
                if n.kind == Kind::File {
                    Box::from("")
                } else {
                    candidate_id(&n.id).into()
                }
            })
            .collect();
        let mut by_id = HashMap::with_capacity(graph.nodes.len());
        for (i, n) in graph.nodes.iter().enumerate() {
            // The last node of an id wins, as `collect` into a `HashMap` does.
            by_id.insert(Box::from(n.id.as_str()), i as u32);
        }
        GraphIndex {
            lower_name,
            candidate,
            by_id,
        }
    }

    /// The node of `graph` with `id`, in one lookup. `graph` must be the
    /// graph this index was built for.
    pub(crate) fn node<'g>(&self, graph: &'g Graph, id: &str) -> Option<&'g Node> {
        self.by_id
            .get(id)
            .and_then(|&i| graph.nodes.get(i as usize))
    }
}

/// [`resolve_symbol`] with an index built for the same graph.
pub fn resolve_symbol_with<'a>(
    graph: &'a Graph,
    query: &str,
    in_prefix: Option<&str>,
    index: Option<&GraphIndex>,
) -> Result<Vec<&'a Node>, CallersError> {
    let mut matches = resolve_symbol_matches_with(graph, query, None, index);

    if let Some(raw) = in_prefix {
        let normalized = normalize_path_prefix(raw);
        // An empty prefix (from a bare `/`) means "match everything", the
        // same as no `--in` at all — it never needs an indexed check, and
        // never errors even on an empty graph (`assertPrefixIndexed`'s
        // `!prefix` short-circuit).
        if !normalized.is_empty() {
            let indexed = graph
                .nodes
                .iter()
                .any(|n| path_under_prefix(&n.path, &normalized));
            if !indexed {
                let scopes: Vec<String> = graph
                    .meta
                    .scopes
                    .iter()
                    .map(|s| scope_label(&s.prefix))
                    .collect();
                return Err(CallersError::PrefixNotIndexed {
                    prefix: normalized,
                    scopes,
                });
            }
        }
        matches.retain(|n| path_under_prefix(&n.path, &normalized));
    }

    if matches.is_empty() {
        return Err(CallersError::NoSymbol {
            query: query.to_string(),
        });
    }
    Ok(matches)
}

/// One traversed edge. `node` is `None` when the other endpoint is not a
/// real node in the graph, such as an unresolved import module string
///.
#[derive(Debug, Clone)]
pub struct Hit<'a> {
    pub id: String,
    pub relation: Relation,
    pub depth: u32,
    pub node: Option<&'a Node>,
    /// The id of the hit this one was reached through. `None` at depth 1.
    pub parent: Option<String>,
}

/// Depth-1: a plain scan over `graph.edges` in file order, keeping every
/// walk-relation edge that touches `symbol`. No dedup: two edges between
/// the same pair give two hits (a recorded correction).
fn depth1_scan<'a>(
    graph: &'a Graph,
    symbol: &Node,
    direction: Direction,
    index: Option<&GraphIndex>,
) -> Vec<Hit<'a>> {
    let owned: HashMap<&str, &Node> = match index {
        Some(_) => HashMap::new(),
        None => graph.nodes.iter().map(|n| (n.id.as_str(), n)).collect(),
    };
    let find = |id: &str| -> Option<&'a Node> {
        match index {
            Some(i) => i.by_id.get(id).map(|at| &graph.nodes[*at as usize]),
            None => owned.get(id).copied(),
        }
    };
    let mut hits = Vec::new();
    for edge in &graph.edges {
        if !WALK_RELATIONS.contains(&edge.relation) {
            continue;
        }
        let other = match direction {
            Direction::In if edge.target == symbol.id => &edge.source,
            Direction::Out if edge.source == symbol.id => &edge.target,
            _ => continue,
        };
        hits.push(Hit {
            id: other.clone(),
            relation: edge.relation,
            depth: 1,
            node: find(other.as_str()),
            parent: None,
        });
    }
    hits
}

/// The BFS over walk-relation edges from every seed at once, each node
/// deduped by id and reported once at the depth it was first reached.
/// Seeds are pre-marked visited, so a seed is never its own hit
///.
fn bfs<'a>(
    graph: &'a Graph,
    seeds: &[&Node],
    direction: Direction,
    max_depth: usize,
) -> Vec<Hit<'a>> {
    let by_id: HashMap<&str, &Node> = graph.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut adj: HashMap<&str, Vec<(&str, Relation)>> = HashMap::new();
    for edge in &graph.edges {
        if !WALK_RELATIONS.contains(&edge.relation) {
            continue;
        }
        let (key, other) = match direction {
            Direction::In => (edge.target.as_str(), edge.source.as_str()),
            Direction::Out => (edge.source.as_str(), edge.target.as_str()),
        };
        adj.entry(key).or_default().push((other, edge.relation));
    }

    let mut visited: HashSet<&str> = HashSet::new();
    let mut frontier: Vec<&str> = Vec::new();
    for seed in seeds {
        if visited.insert(seed.id.as_str()) {
            frontier.push(seed.id.as_str());
        }
    }

    let mut hits: Vec<Hit<'a>> = Vec::new();
    let mut depth = 1usize;
    while !frontier.is_empty() && depth <= max_depth {
        let mut next = Vec::new();
        for current in &frontier {
            for (other, relation) in adj.get(*current).into_iter().flatten() {
                if !visited.insert(other) {
                    continue;
                }
                hits.push(Hit {
                    id: other.to_string(),
                    relation: *relation,
                    depth: depth as u32,
                    node: by_id.get(*other).copied(),
                    parent: Some(current.to_string()),
                });
                next.push(*other);
            }
        }
        frontier = next;
        depth += 1;
    }
    hits
}

/// Walks the edges of `symbol`: the depth-1 scan for `depth <= 1`, else a
/// BFS, seeded with every same-path symbol too when `symbol` is a file
///.
pub fn edge_walk<'a>(
    graph: &'a Graph,
    symbol: &'a Node,
    direction: Direction,
    depth: Depth,
) -> Vec<Hit<'a>> {
    edge_walk_with(graph, symbol, direction, depth, None)
}

/// [`edge_walk`] with an index built for the same graph.
pub fn edge_walk_with<'a>(
    graph: &'a Graph,
    symbol: &'a Node,
    direction: Direction,
    depth: Depth,
    index: Option<&GraphIndex>,
) -> Vec<Hit<'a>> {
    if matches!(depth, Depth(Some(n)) if n <= 1) {
        return depth1_scan(graph, symbol, direction, index);
    }
    let max_depth = match depth {
        Depth(Some(n)) => n,
        Depth(None) => usize::MAX,
    };
    if symbol.kind == Kind::File {
        let mut seeds: Vec<&Node> = vec![symbol];
        seeds.extend(
            graph
                .nodes
                .iter()
                .filter(|n| n.kind != Kind::File && n.path == symbol.path),
        );
        bfs(graph, &seeds, direction, max_depth)
    } else {
        bfs(graph, &[symbol], direction, max_depth)
    }
}

/// Parses a node's `L<start>-L<end>` span into a pair of line numbers.
fn parse_span(span: &str) -> Option<(u32, u32)> {
    let rest = span.strip_prefix('L')?;
    let (start, rest) = rest.split_once("-L")?;
    Some((start.parse().ok()?, rest.parse().ok()?))
}

/// A cached reader of the repo's files, rooted at `repo_root`: a `callers`
/// run reads each file at most once, no matter how many hits quote it
///.
pub struct FileReader<'a> {
    root: &'a Path,
    cache: HashMap<String, Option<Vec<String>>>,
}

impl<'a> FileReader<'a> {
    /// Builds an empty reader rooted at `repo_root`.
    pub fn new(repo_root: &'a Path) -> Self {
        FileReader {
            root: repo_root,
            cache: HashMap::new(),
        }
    }

    fn lines(&mut self, path: &str) -> Option<&[String]> {
        let root = self.root;
        self.cache
            .entry(path.to_string())
            .or_insert_with(|| {
                // The reader reads with `readFileSync(path, "utf8")`: an
                // invalid byte becomes U+FFFD, and the quote still prints.
                std::fs::read(root.join(path))
                    .ok()
                    .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                    .map(|text| text.split('\n').map(str::to_string).collect())
            })
            .as_deref()
    }
}

/// The call-site line for one hit: only for a resolved hit at depth 1
///. Reads `hit`'s file through `reader`, and returns the
/// first line in its span mentioning `symbol_name` as a whole word — the
/// queried symbol's name, not the hit's. An unreadable file, or no
/// matching line, gives `None` rather than an error.
pub fn quote_for(reader: &mut FileReader, symbol_name: &str, hit: &Hit) -> Option<(u32, String)> {
    let node = hit.node?;
    if hit.depth > 1 {
        return None;
    }
    let (start, end) = parse_span(&node.span)?;
    let re = word_re(symbol_name)?;
    let lines = reader.lines(&node.path)?;
    let from = (start as usize).saturating_sub(1);
    let to = (end as usize).min(lines.len());
    if from >= to {
        return None;
    }
    lines[from..to]
        .iter()
        .enumerate()
        .find(|(_, line)| re.is_match(line))
        .map(|(i, line)| (start + i as u32, line.clone()))
}

/// `\b<name>\b` with ASCII word boundaries (`wordRe`): JavaScript `\b` has no
/// `u` flag there, so `café(2)` has no boundary after the `é` and never quotes.
fn word_re(symbol_name: &str) -> Option<Regex> {
    Regex::new(&format!(r"(?-u:\b){}(?-u:\b)", escape_regex(symbol_name))).ok()
}

/// The row of a symbol: `name  kind  path:start-end`.
pub(crate) fn header_of(node: &Node) -> String {
    sieve_core::voice::row(
        &node.name,
        crate::ask::kind_word(node.kind),
        &node.path,
        Some(&node.span),
    )
}

/// The note a zero-hit symbol carries, with the ambiguity clause only when
/// more than one symbol matched the query.
pub(crate) fn loose_note_for(direction: Direction, name: &str, candidate_count: usize) -> String {
    let (label, dir_word) = if direction == Direction::Out {
        ("callees", "outgoing")
    } else {
        ("callers", "incoming")
    };
    let ambiguity = if candidate_count > 1 {
        format!(
            " {candidate_count} symbols share the name {name}. A caller in another file is \
            dropped, not guessed, so this may undercount."
        )
    } else {
        String::new()
    };
    format!(
        "no {label} found — the index has no {dir_word} links for this symbol.{ambiguity} \
        Check the name, or run {} find {name}",
        sieve_core::product().name
    )
}

/// The note the `--json` report holds for a zero-hit symbol. The text is the
/// same as it was before the new output voice, so `--json` stays byte-identical.
/// The text report uses [`loose_note_for`]. The note has the
/// ambiguity clause only when more than one symbol matched the query
///.
fn loose_note_json(direction: Direction, name: &str, candidate_count: usize) -> String {
    let label = if direction == Direction::Out {
        "callees"
    } else {
        "callers"
    };
    let dir_word = if direction == Direction::Out {
        "outgoing"
    } else {
        "incoming"
    };
    let ambiguity = if candidate_count > 1 {
        format!(
            " {candidate_count} definitions share the name \"{name}\"; a cross-file caller of \
            an ambiguous name is dropped rather than guessed, so this may undercount."
        )
    } else {
        String::new()
    };
    format!(
        "  no indexed {label} — the graph has no {dir_word} call/reference edges for this \
        symbol as written.{ambiguity} Check the name (try the bare symbol, or \"Type.method\"), \
        or find its uses with {} find \"{name}\". Fall back to raw grep -rn only for \
        unindexed files",
        sieve_core::product().name
    )
}

/// The count line under a symbol row, such as `2 callers · 5 within 2 hops`.
fn summary_line(direction: Direction, hits: &[Hit]) -> String {
    let direct = hits.iter().filter(|h| h.depth <= 1).count();
    let deepest = hits.iter().map(|h| h.depth).max().unwrap_or(1);
    // `in` counts callers. `out` counts what the symbol calls.
    let head = match (direction, direct) {
        (Direction::In, 1) => "1 caller".to_string(),
        (Direction::In, n) => format!("{n} callers"),
        (Direction::Out, n) => format!("calls {n}"),
    };
    if deepest <= 1 {
        return head;
    }
    format!("{head} \u{b7} {} within {deepest} hops", hits.len())
}

/// The row of one hit, with its relation when it is not a call.
fn hit_row(hit: &Hit) -> String {
    let row = match hit.node {
        Some(node) => header_of(node),
        None => format!("{}  import", hit.id),
    };
    match hit.relation {
        Relation::Calls => row,
        other => format!("{row}  \u{b7} {}", other.as_str()),
    }
}

/// Renders one matched symbol and its hits as a tree by hop, with `\u{251c}\u{2500}`,
/// `\u{2514}\u{2500}` and `\u{2502}` lines. A hit at depth 1 gets its call line
/// when `reader` is given. The text has no final newline.
pub fn render_block(
    symbol: &Node,
    hits: &[Hit],
    direction: Direction,
    candidate_count: usize,
    mut reader: Option<&mut FileReader>,
) -> String {
    let mut lines = vec![header_of(symbol)];
    if hits.is_empty() {
        lines.push(loose_note_for(direction, &symbol.name, candidate_count));
        return lines.join("\n");
    }
    lines.push(summary_line(direction, hits));

    let items: Vec<sieve_core::voice::TreeItem> = hits
        .iter()
        .map(|hit| {
            let note = reader
                .as_deref_mut()
                .and_then(|reader| quote_for(reader, &symbol.name, hit))
                .map(|(n, text)| format!("{n}: {}", js_trim(&text)));
            sieve_core::voice::TreeItem {
                id: hit.id.clone(),
                parent: hit.parent.clone(),
                text: hit_row(hit),
                note,
            }
        })
        .collect();
    lines.extend(sieve_core::voice::tree_lines(&items));
    lines.join("\n")
}

/// True for a char JavaScript `String.prototype.trim` strips: WhiteSpace
/// (Zs, tab, VT, FF, NBSP, U+FEFF) and LineTerminator. Rust's
/// `char::is_whitespace` differs on two chars: it strips U+0085 (NEL) and
/// keeps U+FEFF (the BOM).
pub(crate) fn is_js_whitespace(c: char) -> bool {
    c == '\u{FEFF}' || (c.is_whitespace() && c != '\u{85}')
}

/// JavaScript `trim`: strips a leading BOM, which
/// Rust `trim()` keeps.
pub(crate) fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_whitespace)
}

/// Renders the plain-text `callers` report, without the savings line; the
/// CLI adds that last. `reader` is the one cached file reader this whole
/// render shares, across every matched symbol's hits.
pub fn format_callers(
    _query: &str,
    matches: &[(&Node, Vec<Hit>)],
    direction: Direction,
    _show_depth: bool,
    reader: &mut FileReader,
) -> String {
    let candidate_count = matches.len();
    let blocks: Vec<String> = matches
        .iter()
        .map(|(symbol, hits)| {
            render_block(symbol, hits, direction, candidate_count, Some(&mut *reader))
        })
        .collect();
    format!("{}\n", blocks.join("\n\n"))
}

/// The paths `callers`' tokens-saved baseline reads: each matched symbol's
/// path, plus every resolved hit's path, in that order. `savings_for` dedupes.
pub fn callers_saved_paths(matches: &[(&Node, Vec<Hit>)]) -> Vec<String> {
    let mut paths = Vec::new();
    for (symbol, hits) in matches {
        paths.push(symbol.path.clone());
        for hit in hits {
            if let Some(node) = hit.node {
                paths.push(node.path.clone());
            }
        }
    }
    paths
}

/// The count of hits over every matched symbol: the `<n>` of the answer line.
pub fn callers_hit_count(matches: &[(&Node, Vec<Hit>)]) -> usize {
    matches.iter().map(|(_, hits)| hits.len()).sum()
}

/// The size, in chars, of the output of `grep -rnw <name>` over the indexed
/// files under `root`: one `path:line:text` row per whole-word match. This is
/// the baseline the `callers` answer line measures against. A hit's text is
/// capped at 160 code units, so the baseline errs low. A word with a line
/// break gives 0: `grep` splits on `\n`, so such a word never matches a line.
pub fn word_grep_chars(graph: &Graph, root: &Path, name: &str) -> u64 {
    if name.contains(['\n', '\r']) {
        return 0;
    }
    let opts = GrepOptions {
        ignore_case: false,
        fixed: false,
        in_prefix: None,
        max_hits: 1_000_000,
    };
    let pattern = format!(r"\b{}\b", escape_regex(name));
    let Ok(regex) = compile_pattern(&pattern, &opts) else {
        return 0;
    };
    let paths: Vec<&str> = graph
        .nodes
        .iter()
        .filter(|n| n.kind == Kind::File)
        .map(|n| n.path.as_str())
        .collect();
    if paths.is_empty() {
        return 0;
    }
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get().min(8));
    word_grep_chars_in(&regex, root, &paths, threads, opts.max_hits as u64)
}

/// The most hits the baseline counts before the cap applies. A name past it
/// uses the disk scan, which applies the cap in file order.
const WORD_CAP: u64 = 1_000_000;

/// The rows of one word in one file: the count, and the chars of each row
/// without its path (`:`, the line digits, `:`, the text, `\n`).
type Rows = (u32, u64);

/// One file's indexed words.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct FileWords {
    /// The UTF-16 length of the path.
    path_units: u64,
    words: HashMap<Box<str>, Rows>,
}

/// The savings word index. It holds, per
/// file, the row size of each line that holds a word, for the words that are
/// node names made of ASCII letters, digits and `_`. [`WordIndex::chars`] then
/// gives the figure of [`word_grep_chars`] with no disk read.
#[derive(Debug, Default)]
pub struct WordIndex {
    vocab: HashSet<Box<str>>,
    files: HashMap<String, FileWords>,
}

/// True for a name that the `\b` regex reads as one token of word chars.
fn is_pure_word(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

impl WordIndex {
    /// Indexes every file node of `graph` under `root`, on scoped threads.
    pub fn build(graph: &Graph, root: &Path) -> WordIndex {
        let vocab: HashSet<Box<str>> = graph
            .nodes
            .iter()
            .filter(|n| is_pure_word(&n.name))
            .map(|n| n.name.as_str().into())
            .collect();
        let paths: Vec<&str> = graph
            .nodes
            .iter()
            .filter(|n| n.kind == Kind::File)
            .map(|n| n.path.as_str())
            .collect();
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get().min(8));
        let size = paths.len().div_ceil(threads).max(1);
        let vocab_ref = &vocab;
        let files: HashMap<String, FileWords> = std::thread::scope(|scope| {
            let handles: Vec<_> = paths
                .chunks(size)
                .map(|chunk| {
                    scope.spawn(move || {
                        let mut buf = Vec::new();
                        chunk
                            .iter()
                            .map(|path| {
                                (
                                    (*path).to_string(),
                                    file_words(vocab_ref, root, path, &mut buf),
                                )
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|h| h.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
                .collect()
        });
        WordIndex { vocab, files }
    }

    /// Reads one file again and replaces its entry.
    pub fn refresh_file(&mut self, root: &Path, path: &str) {
        let words = file_words(&self.vocab, root, path, &mut Vec::new());
        self.files.insert(path.to_string(), words);
    }

    /// Removes the entry of a file that left the graph.
    pub fn drop_file(&mut self, path: &str) {
        self.files.remove(path);
    }

    /// The figure of [`word_grep_chars`] for `name`, or `None` when the index
    /// cannot give it: a name that is not an indexed word, or a name past the
    /// hit cap.
    pub fn chars(&self, name: &str) -> Option<u64> {
        if !self.vocab.contains(name) {
            return None;
        }
        let (mut hits, mut chars) = (0u64, 0u64);
        for file in self.files.values() {
            if let Some(&(n, rest)) = file.words.get(name) {
                hits += u64::from(n);
                chars += u64::from(n) * file.path_units + rest;
            }
        }
        (hits <= WORD_CAP).then_some(chars)
    }

    /// A rough size in bytes, for the memory gate.
    pub fn approx_bytes(&self) -> usize {
        let words: usize = self
            .files
            .iter()
            .map(|(p, f)| p.len() + 48 + f.words.keys().map(|w| w.len() + 40).sum::<usize>())
            .sum();
        words + self.vocab.iter().map(|w| w.len() + 24).sum::<usize>()
    }
}

/// Reads and indexes one file. An unreadable file gives an empty entry, the
/// same as `grep` gives no rows.
fn file_words(vocab: &HashSet<Box<str>>, root: &Path, path: &str, buf: &mut Vec<u8>) -> FileWords {
    let mut out = FileWords {
        path_units: path.encode_utf16().count() as u64,
        words: HashMap::new(),
    };
    buf.clear();
    let read = std::fs::File::open(root.join(path))
        .and_then(|mut f| std::io::Read::read_to_end(&mut f, buf));
    if read.is_err() {
        return out;
    }
    let Some(text) = decode_file_bytes(buf) else {
        return out;
    };
    let mut seen: Vec<&str> = Vec::new();
    for (i, line) in text.split('\n').enumerate() {
        seen.clear();
        let bytes = line.as_bytes();
        let mut at = 0;
        while at < bytes.len() {
            if !(bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_') {
                at += 1;
                continue;
            }
            let start = at;
            while at < bytes.len() && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_') {
                at += 1;
            }
            let token = &line[start..at];
            if vocab.contains(token) && !seen.contains(&token) {
                seen.push(token);
            }
        }
        if seen.is_empty() {
            continue;
        }
        let shown = line.trim().encode_utf16().count().min(MAX_HIT_TEXT) as u64;
        let digits = (i as u64 + 1).ilog10() as u64 + 1;
        // `:` + line + `:` + text + `\n`
        let rest = 2 + digits + shown + 1;
        for token in &seen {
            let row = out.words.entry((*token).into()).or_insert((0, 0));
            row.0 += 1;
            row.1 += rest;
        }
    }
    out
}

/// [`word_grep_chars`] that reads `index` first and scans the disk when the
/// index cannot answer.
pub fn word_grep_chars_indexed(
    index: Option<&WordIndex>,
    graph: &Graph,
    root: &Path,
    name: &str,
) -> u64 {
    index
        .and_then(|i| i.chars(name))
        .unwrap_or_else(|| word_grep_chars(graph, root, name))
}

/// Reads one file into `buf` and counts its rows, at most `limit` of them.
/// An unreadable file gives `(0, 0)`, the same as in `grep`.
fn file_rows(
    regex: &regress::Regex,
    root: &Path,
    path: &str,
    buf: &mut Vec<u8>,
    limit: u64,
) -> (u64, u64) {
    buf.clear();
    let read = std::fs::File::open(root.join(path))
        .and_then(|mut f| std::io::Read::read_to_end(&mut f, buf));
    if read.is_err() {
        return (0, 0);
    }
    decode_file_bytes(buf).map_or((0, 0), |text| file_row_chars(regex, path, &text, limit))
}

/// Sums the row sizes of `paths` on `threads` scoped threads. Threads count
/// each file; one ordered pass then applies the `max_hits` cap, so the
/// result equals the sequential `grep` for any thread count.
fn word_grep_chars_in(
    regex: &regress::Regex,
    root: &Path,
    paths: &[&str],
    threads: usize,
    max_hits: u64,
) -> u64 {
    let size = paths.len().div_ceil(threads.max(1));
    let per_file: Vec<(u64, u64)> = std::thread::scope(|scope| {
        let handles: Vec<_> = paths
            .chunks(size)
            .map(|chunk| {
                scope.spawn(move || {
                    let mut buf = Vec::new();
                    chunk
                        .iter()
                        .map(|path| file_rows(regex, root, path, &mut buf, u64::MAX))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
            .collect()
    });
    let (mut hits, mut chars) = (0u64, 0u64);
    for (path, (file_hits, file_chars)) in paths.iter().zip(per_file) {
        if hits + file_hits <= max_hits {
            hits += file_hits;
            chars += file_chars;
            continue;
        }
        // This file crosses the cap: count only its first rows.
        chars += file_rows(regex, root, path, &mut Vec::new(), max_hits - hits).1;
        break;
    }
    chars
}

/// The count and UTF-16 size of the `path:line:text\n` rows of one file's
/// matching lines, at most `limit` rows.
fn file_row_chars(regex: &regress::Regex, path: &str, text: &str, limit: u64) -> (u64, u64) {
    let bytes = text.as_bytes();
    let path_units = path.encode_utf16().count() as u64;
    let (mut sum, mut line, mut counted, mut line_end) = (0u64, 1u64, 0usize, 0usize);
    let mut hits = 0u64;
    for m in regex.find_iter(text) {
        if hits >= limit {
            break;
        }
        let start = m.start();
        if start < line_end {
            continue;
        }
        let line_start = bytes[..start]
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |i| i + 1);
        line += bytes[counted..line_start]
            .iter()
            .filter(|&&b| b == b'\n')
            .count() as u64;
        counted = line_start;
        let end = bytes[start..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(bytes.len(), |i| start + i);
        line_end = end + 1;
        let shown = text[line_start..end]
            .trim()
            .encode_utf16()
            .count()
            .min(MAX_HIT_TEXT) as u64;
        // `path` + `:` + line + `:` + text + `\n`
        sum += path_units + 2 + (line.ilog10() as u64 + 1) + shown + 1;
        hits += 1;
    }
    (hits, sum)
}

/// The slow reference for `word_grep_chars`, kept to pin its figure.
#[cfg(test)]
fn word_grep_chars_reference(graph: &Graph, root: &Path, name: &str) -> u64 {
    let opts = GrepOptions {
        ignore_case: false,
        fixed: false,
        in_prefix: None,
        max_hits: 1_000_000,
    };
    let pattern = format!(r"\b{}\b", escape_regex(name));
    let Ok(result) = grep_graph(graph, root, &pattern, &opts) else {
        return 0;
    };
    let mut chars = 0u64;
    for group in &result.groups {
        for hit in &group.hits {
            let row = format!("{}:{}:{}\n", group.path, hit.line, hit.text);
            chars += row.encode_utf16().count() as u64;
        }
    }
    chars
}

/// A matched symbol's `--json` shape.
#[derive(Debug, Clone, Serialize)]
pub struct CallersSymbolJson {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub path: String,
    pub span: String,
}

/// One hit's `--json` shape: resolved fields are present only when the
/// hit's node resolved.
#[derive(Debug, Clone, Serialize)]
pub struct CallersHitJson {
    pub id: String,
    pub relation: Relation,
    pub depth: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<Kind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span: Option<String>,
}

/// One matched symbol's `--json` shape: `note` appears only when `hits` is
/// empty.
#[derive(Debug, Clone, Serialize)]
pub struct CallersMatchJson {
    pub symbol: CallersSymbolJson,
    pub hits: Vec<CallersHitJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// The full `--json` payload, minus `saved`, which the CLI attaches
///.
#[derive(Debug, Clone, Serialize)]
pub struct CallersJson {
    pub query: String,
    pub matches: Vec<CallersMatchJson>,
}

/// Builds the `--json` payload for one `callers` run.
pub fn to_callers_json(
    query: &str,
    matches: &[(&Node, Vec<Hit>)],
    direction: Direction,
) -> CallersJson {
    let candidate_count = matches.len();
    CallersJson {
        query: query.to_string(),
        matches: matches
            .iter()
            .map(|(symbol, hits)| CallersMatchJson {
                symbol: CallersSymbolJson {
                    id: symbol.id.clone(),
                    name: symbol.name.clone(),
                    kind: symbol.kind,
                    path: symbol.path.clone(),
                    span: symbol.span.clone(),
                },
                hits: hits
                    .iter()
                    .map(|h| CallersHitJson {
                        id: h.id.clone(),
                        relation: h.relation,
                        depth: h.depth,
                        name: h.node.map(|n| n.name.clone()),
                        kind: h.node.map(|n| n.kind),
                        path: h.node.map(|n| n.path.clone()),
                        span: h.node.map(|n| n.span.clone()),
                    })
                    .collect(),
                note: if hits.is_empty() {
                    Some(loose_note_json(direction, &symbol.name, candidate_count))
                } else {
                    None
                },
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sieve_core::{Confidence, Edge, Meta, Origin, SummaryState};

    fn node(id: &str, name: &str, kind: Kind, path: &str, span: &str) -> Node {
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

    fn edge(source: &str, target: &str, relation: Relation) -> Edge {
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
                languages: Vec::new(),
                scopes: Vec::new(),
            },
            nodes,
            edges,
        }
    }

    #[test]
    fn word_grep_chars_counts_whole_word_rows_only() {
        let dir = crate::test_support::TempDir::new("word-grep-chars");
        std::fs::write(dir.join("a.ts"), "run();\nrunner();\nx = run;\n").expect("write");
        let g = graph(
            vec![node("a", "a.ts", Kind::File, "a.ts", "L1-L3")],
            Vec::new(),
        );
        // Two rows match the whole word: `a.ts:1:run();` and `a.ts:3:x = run;`.
        let want = ("a.ts:1:run();\n".len() + "a.ts:3:x = run;\n".len()) as u64;
        assert_eq!(word_grep_chars(&g, &dir, "run"), want);
        assert_eq!(word_grep_chars(&g, &dir, "absent"), 0);
    }

    /// Writes the differential fixture and returns its graph.
    fn word_fixture(dir: &Path) -> Graph {
        let long: String = format!("run {}\u{1D4BB}{}", "a".repeat(155), "b".repeat(30));
        let cut_bmp = format!("run {}{}", "x".repeat(150), "\u{00E9}".repeat(40));
        let files: Vec<(&str, Vec<u8>)> = vec![
            (
                "crlf.ts",
                b"run();\r\nrunner();\r\n  x = run;  \r\n".to_vec(),
            ),
            ("bom.ts", b"\xEF\xBB\xBFrun();\nrun;\n".to_vec()),
            ("u16.ts", {
                let mut v = vec![0xFF, 0xFE];
                for u in "run();\nx = run\n".encode_utf16() {
                    v.extend_from_slice(&u.to_le_bytes());
                }
                v
            }),
            ("u16be.ts", vec![0xFE, 0xFF, 0, b'r', 0, b'u', 0, b'n']),
            ("nonl.ts", b"a\nrun".to_vec()),
            ("two.ts", b"run(run) + run\nrun\n\nrun\n".to_vec()),
            ("longer.ts", b"runner running _run run_ $run\n".to_vec()),
            ("pair.ts", long.into_bytes()),
            ("bmp.ts", cut_bmp.into_bytes()),
            ("empty.ts", Vec::new()),
            ("none.ts", b"nothing here\n".to_vec()),
            ("bad.ts", b"run \xFF\xFE bad\nrun\n".to_vec()),
            ("cr.ts", b"run\rrun\rx".to_vec()),
            ("many.ts", {
                let mut v = String::new();
                for i in 1..=120 {
                    match i {
                        9 | 10 | 99 | 100 | 101 => v.push_str("run\n"),
                        20 => v.push_str("run x\n"),
                        _ => v.push('\n'),
                    }
                }
                v.push_str(&"\n".repeat(300));
                v.push_str("  run  \n");
                v.into_bytes()
            }),
            ("café run.ts", "run caf\u{e9}\n".as_bytes().to_vec()),
        ];
        let mut nodes = Vec::new();
        for (name, bytes) in &files {
            std::fs::write(dir.join(name), bytes).expect("write");
            nodes.push(node(name, name, Kind::File, name, "L1-L1"));
        }
        std::fs::create_dir(dir.join("adir")).expect("dir");
        nodes.push(node("adir", "adir", Kind::File, "adir", "L1-L1"));
        nodes.push(node("gone", "gone.ts", Kind::File, "gone.ts", "L1-L1"));
        graph(nodes, Vec::new())
    }

    #[test]
    fn word_grep_chars_matches_the_reference() {
        let dir = crate::test_support::TempDir::new("word-grep-diff");
        let g = word_fixture(&dir);
        for word in [
            "run",
            "runner",
            "absent",
            "a",
            "x",
            "caf\u{e9}",
            "\u{1D4BB}",
        ] {
            assert_eq!(
                word_grep_chars(&g, &dir, word),
                word_grep_chars_reference(&g, &dir, word),
                "word {word}"
            );
        }
        assert!(word_grep_chars(&g, &dir, "run") > 0);
    }

    #[test]
    fn grep_row_chars_matches_the_reference() {
        let dir = crate::test_support::TempDir::new("grep-row-chars");
        let g = word_fixture(&dir);
        let opts = GrepOptions {
            ignore_case: false,
            fixed: false,
            in_prefix: None,
            max_hits: 1_000_000,
        };
        for word in ["run", "runner", "a", "caf\u{e9}", "\u{1D4BB}", "absent"] {
            let pattern = format!(r"\b{}\b", escape_regex(word));
            let result = grep_graph(&g, &dir, &pattern, &opts).expect("grep");
            assert_eq!(
                result.row_chars(),
                word_grep_chars_reference(&g, &dir, word),
                "word {word}"
            );
        }
    }

    #[test]
    fn the_word_index_matches_the_disk_scan() {
        let dir = crate::test_support::TempDir::new("word-index-diff");
        let mut g = word_fixture(&dir);
        // Names: a pure word, a longer word, a non-ASCII name, a dotted name.
        for (i, name) in ["run", "runner", "caf\u{e9}", "a.b", "x", "gone"]
            .iter()
            .enumerate()
        {
            let mut n = node(&format!("s{i}"), "a.ts", Kind::Function, name, "L1-L1");
            n.name = (*name).to_string();
            g.nodes.push(n);
        }
        std::fs::write(
            dir.join("adj.ts"),
            "\u{e9}run run\u{e9} \u{e9}run\u{e9} a.b a.b.c\n",
        )
        .expect("write");
        g.nodes
            .push(node("adj", "adj.ts", Kind::File, "adj.ts", "L1-L1"));
        let index = WordIndex::build(&g, &dir);
        for word in ["run", "runner", "x", "caf\u{e9}", "a.b", "absent", "gone"] {
            assert_eq!(
                word_grep_chars_indexed(Some(&index), &g, &dir, word),
                word_grep_chars(&g, &dir, word),
                "word {word}"
            );
        }
        // A pure word is answered by the index; the others use the disk.
        assert!(index.chars("run").is_some_and(|c| c > 0));
        assert_eq!(index.chars("caf\u{e9}"), None);
        assert_eq!(index.chars("a.b"), None);
        assert_eq!(index.chars("absent"), None);
        // The UTF-16LE file counts in the figure.
        assert!(index.files["u16.ts"].words.contains_key("run"));
        // An edit replaces the entry of one file.
        std::fs::write(dir.join("two.ts"), "run\nrun run\n").expect("write");
        let mut index = index;
        index.refresh_file(&dir, "two.ts");
        assert_eq!(index.chars("run"), Some(word_grep_chars(&g, &dir, "run")));
        index.drop_file("two.ts");
        assert_ne!(index.chars("run"), Some(word_grep_chars(&g, &dir, "run")));
    }

    #[test]
    fn an_index_changes_no_answer_of_resolve_or_walk() {
        let nodes = vec![
            node("a.ts", "a.ts", Kind::File, "a.ts", "L1-L9"),
            node("a.ts#Run", "Run", Kind::Function, "a.ts", "L1-L2"),
            node("a.ts#Run~2", "Run", Kind::Function, "a.ts", "L3-L4"),
            node("b.ts#Cls.go", "go", Kind::Method, "b.ts", "L1-L2"),
            node("b.ts#caller", "caller", Kind::Function, "b.ts", "L3-L4"),
        ];
        let edges = vec![
            edge("b.ts#caller", "a.ts#Run", Relation::Calls),
            edge("b.ts#caller", "b.ts#Cls.go", Relation::Calls),
            edge("b.ts#caller", "ghost.ts#x", Relation::Calls),
        ];
        let g = graph(nodes, edges);
        let index = GraphIndex::build(&g);
        for query in [
            "run", "RUN", "go", "Cls.go", "a.ts", "caller", "nope", "x.go",
        ] {
            let plain = resolve_symbol(&g, query, None);
            let fast = resolve_symbol_with(&g, query, None, Some(&index));
            let ids = |r: &Result<Vec<&Node>, CallersError>| {
                r.as_ref()
                    .map(|v| v.iter().map(|n| n.id.clone()).collect::<Vec<_>>())
                    .ok()
            };
            assert_eq!(ids(&plain), ids(&fast), "query {query}");
            for symbol in plain.iter().flatten() {
                for dir in [Direction::In, Direction::Out] {
                    for depth in [Depth(Some(1)), Depth(None)] {
                        let a = edge_walk(&g, symbol, dir, depth);
                        let b = edge_walk_with(&g, symbol, dir, depth, Some(&index));
                        let key = |h: &Vec<Hit>| {
                            h.iter()
                                .map(|x| (x.id.clone(), x.node.map(|n| n.id.clone()), x.depth))
                                .collect::<Vec<_>>()
                        };
                        assert_eq!(key(&a), key(&b), "query {query}");
                    }
                }
            }
        }
    }

    #[test]
    fn word_grep_chars_is_the_same_on_1_and_8_threads() {
        let dir = crate::test_support::TempDir::new("word-grep-threads");
        let g = word_fixture(&dir);
        let opts = GrepOptions {
            ignore_case: false,
            fixed: false,
            in_prefix: None,
            max_hits: 1_000_000,
        };
        let regex = compile_pattern(r"\brun\b", &opts).expect("regex");
        let paths: Vec<&str> = g.nodes.iter().map(|n| n.path.as_str()).collect();
        let one = word_grep_chars_in(&regex, &dir, &paths, 1, 1_000_000);
        assert_eq!(one, word_grep_chars_in(&regex, &dir, &paths, 8, 1_000_000));
        assert_eq!(one, word_grep_chars_reference(&g, &dir, "run"));
    }

    #[test]
    fn word_grep_chars_cap_matches_the_reference() {
        let dir = crate::test_support::TempDir::new("word-grep-cap");
        let g = word_fixture(&dir);
        let opts = GrepOptions {
            ignore_case: false,
            fixed: false,
            in_prefix: None,
            max_hits: 0,
        };
        let regex = compile_pattern(r"\brun\b", &opts).expect("regex");
        let paths: Vec<&str> = g
            .nodes
            .iter()
            .filter(|n| n.kind == Kind::File)
            .map(|n| n.path.as_str())
            .collect();
        for cap in [0u64, 1, 2, 5, 9, 14, 100] {
            let want = {
                let o = GrepOptions {
                    max_hits: cap as usize,
                    ..opts.clone()
                };
                let r = grep_graph(&g, &dir, r"\brun\b", &o).expect("grep");
                r.groups
                    .iter()
                    .flat_map(|gr| gr.hits.iter().map(move |h| (gr, h)))
                    .map(|(gr, h)| {
                        format!("{}:{}:{}\n", gr.path, h.line, h.text)
                            .encode_utf16()
                            .count() as u64
                    })
                    .sum::<u64>()
            };
            for threads in [1, 3, 8] {
                assert_eq!(
                    word_grep_chars_in(&regex, &dir, &paths, threads, cap),
                    want,
                    "cap {cap}"
                );
            }
        }
        assert_eq!(word_grep_chars(&g, &dir, "a\nb"), 0);
    }

    #[test]
    fn parse_depth_accepts_all_case_insensitively() {
        assert_eq!(parse_depth("all").unwrap().0, None);
        assert_eq!(parse_depth("FULL").unwrap().0, None);
        assert_eq!(parse_depth("Max").unwrap().0, None);
    }

    #[test]
    fn parse_depth_accepts_decimal_and_exponent_js_number_forms() {
        assert_eq!(parse_depth("2.7").unwrap().0, Some(2));
        assert_eq!(parse_depth("1e3").unwrap().0, Some(1000));
        assert_eq!(parse_depth(" 2 ").unwrap().0, Some(2));
    }

    #[test]
    fn parse_depth_rejects_zero_and_garbage() {
        assert!(parse_depth("0").is_err());
        assert!(parse_depth("x").is_err());
    }

    #[test]
    fn test_p1_21_quote_word_boundary_is_ascii() {
        let hit = |name: &str, line: &str| word_re(name).expect("regex").is_match(line);
        assert!(hit("add", "return add(a, b)"));
        assert!(!hit("add", "return added(a, b)"));
        assert!(!hit("café", "return café(2);"));
        assert!(!hit("𝒻oo", "return 𝒻oo(3);"));
    }

    #[test]
    fn test_p1_21_js_trim_strips_the_bom_and_keeps_nel() {
        assert_eq!(js_trim("\u{FEFF}def go(): pass \n"), "def go(): pass");
        assert_eq!(js_trim("\u{85}x\u{85}"), "\u{85}x\u{85}");
        assert_eq!(js_trim("\u{A0}\u{3000}x\u{2028}"), "x");
    }

    #[test]
    fn parse_direction_is_case_sensitive() {
        assert_eq!(parse_direction("in").unwrap(), Direction::In);
        assert_eq!(parse_direction("out").unwrap(), Direction::Out);
        assert!(parse_direction("In").is_err());
    }

    #[test]
    fn resolve_symbol_strips_ordinals_on_a_qualified_query() {
        let g = graph(
            vec![node(
                "src/x.ts#C~2.m",
                "m",
                Kind::Method,
                "src/x.ts",
                "L1-L2",
            )],
            Vec::new(),
        );
        let found = resolve_symbol(&g, "C.m", None).expect("resolves");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "src/x.ts#C~2.m");
    }

    #[test]
    fn resolve_symbol_errors_with_the_no_symbol_text_on_zero_matches() {
        let g = graph(Vec::new(), Vec::new());
        let err = resolve_symbol(&g, "nope", None).expect_err("no match");
        assert_eq!(
            err.to_string(),
            "no symbol named nope — try sieve find nope"
        );
    }

    #[test]
    fn test_p1_41_normalize_path_prefix_matches_golden() {
        // Pinned from recorded runs of the path prefix rule.
        assert_eq!(normalize_path_prefix("./src/app/"), "src/app");
        assert_eq!(normalize_path_prefix("src/ap"), "src/ap");
        assert_eq!(normalize_path_prefix("/"), "");
        assert_eq!(normalize_path_prefix(""), "");
        // A leading slash is not special on its own.
        assert_eq!(normalize_path_prefix("/src/app"), "/src/app");
        // "." never matches "./" and is left untouched (verified live: it
        // does NOT normalize to "").
        assert_eq!(normalize_path_prefix("."), ".");
    }

    #[test]
    fn test_p1_41_normalize_path_prefix_backslash_is_platform_dependent() {
        let out = normalize_path_prefix("src\\app");
        if cfg!(windows) {
            assert_eq!(out, "src/app");
        } else {
            assert_eq!(out, "src\\app");
        }
    }

    #[test]
    fn test_p1_41_path_under_prefix_is_segment_aware() {
        assert!(path_under_prefix("src/app/x.ts", "src/app"));
        assert!(!path_under_prefix("src/app/x.ts", "src/ap"));
    }

    #[test]
    fn test_p1_42_empty_prefix_matches_every_path() {
        assert!(path_under_prefix("src/app/x.ts", ""));
    }

    #[test]
    fn test_p1_42_resolve_symbol_filters_by_in_before_returning() {
        // Two nodes named "run": one under "src/app", one under "lib". A
        // resolve_symbol call with `--in src/app` never returns the "lib"
        // one, even though both share a name.
        let a = node("a", "run", Kind::Function, "src/app/a.ts", "L1-L2");
        let b = node("b", "run", Kind::Function, "lib/b.ts", "L1-L2");
        let g = graph(vec![a, b], Vec::new());
        let found = resolve_symbol(&g, "run", Some("src/app")).expect("resolves");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, "src/app/a.ts");
    }

    #[test]
    fn test_p1_42_in_on_a_bare_slash_never_errors_on_an_empty_graph() {
        let g = graph(Vec::new(), Vec::new());
        // A bare "/" normalizes to "" ("match everything"); it never
        // triggers the not-indexed check, even with zero nodes.
        let err = resolve_symbol(&g, "nope", Some("/")).expect_err("no symbol, not bad prefix");
        assert_eq!(
            err.to_string(),
            "no symbol named nope — try sieve find nope"
        );
    }

    #[test]
    fn test_p1_44_scope_label_renders_root_and_prefix() {
        assert_eq!(scope_label(""), "(root)");
        assert_eq!(scope_label("apps/api"), "apps/api/");
    }

    #[test]
    fn depth1_scan_keeps_duplicate_edges_with_no_dedup() {
        let a = node("a", "a", Kind::Function, "a.ts", "L1-L2");
        let b = node("b", "b", Kind::Function, "b.ts", "L1-L2");
        let g = graph(
            vec![a, b],
            vec![
                edge("a", "b", Relation::Calls),
                edge("a", "b", Relation::Calls),
            ],
        );
        let hits = edge_walk(&g, &g.nodes[1], Direction::In, Depth(Some(1)));
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn bfs_never_reports_a_seed() {
        let a = node("a", "a", Kind::Function, "a.ts", "L1-L2");
        let b = node("b", "b", Kind::Function, "b.ts", "L1-L2");
        let g = graph(
            vec![a, b],
            vec![
                edge("b", "a", Relation::Calls),
                edge("a", "b", Relation::Calls),
            ],
        );
        let hits = edge_walk(&g, &g.nodes[0], Direction::In, Depth(Some(2)));
        assert!(hits.iter().all(|h| h.id != "a"));
    }

    #[test]
    fn bfs_reports_each_node_once_at_its_shallowest_depth() {
        let a = node("a", "a", Kind::Function, "a.ts", "L1-L2");
        let b = node("b", "b", Kind::Function, "b.ts", "L1-L2");
        let c = node("c", "c", Kind::Function, "c.ts", "L1-L2");
        let g = graph(
            vec![a, b, c],
            vec![
                edge("b", "a", Relation::Calls),
                edge("c", "a", Relation::Calls),
                edge("c", "b", Relation::Calls),
            ],
        );
        let hits = edge_walk(&g, &g.nodes[0], Direction::In, Depth(None));
        assert_eq!(hits.len(), 2);
        let c_hit = hits.iter().find(|h| h.id == "c").expect("c reached");
        assert_eq!(c_hit.depth, 1);
    }

    #[test]
    fn format_callers_renders_the_arrow_and_the_loose_note() {
        let a = node("a", "double", Kind::Function, "util.ts", "L1-L3");
        let matches: Vec<(&Node, Vec<Hit>)> = vec![(&a, Vec::new())];
        let dir = std::env::temp_dir();
        let mut reader = FileReader::new(&dir);
        let rendered = format_callers("double", &matches, Direction::In, false, &mut reader);
        assert!(rendered.starts_with("double  fn  util.ts:1-3\nno callers found"));
    }

    #[test]
    fn a_symbol_row_keeps_the_span_end_and_a_file_keeps_its_range() {
        let f = node(
            "f",
            "handleHMRUpdate",
            Kind::Function,
            "hmr.ts",
            "L411-L679",
        );
        assert_eq!(header_of(&f), "handleHMRUpdate  fn  hmr.ts:411-679");
        let m = node("m", "hmr.spec.ts", Kind::File, "hmr.spec.ts", "L1-L56");
        assert_eq!(header_of(&m), "hmr.spec.ts  file  hmr.spec.ts:1-56");
    }

    #[test]
    fn the_direction_out_header_counts_calls() {
        let a = node("a", "a", Kind::Function, "a.ts", "L1-L2");
        let b = node("b", "b", Kind::Function, "b.ts", "L1-L2");
        let g = graph(vec![a, b], vec![edge("a", "b", Relation::Calls)]);
        let hits = edge_walk(&g, &g.nodes[0], Direction::Out, Depth(Some(1)));
        let text = render_block(&g.nodes[0], &hits, Direction::Out, 1, None);
        assert!(text.contains("\ncalls 1\n"), "{text}");
    }

    #[test]
    fn render_block_nests_a_deeper_hit_under_its_parent() {
        let a = node("a", "a", Kind::Function, "a.ts", "L1-L2");
        let b = node("b", "b", Kind::Function, "b.ts", "L1-L2");
        let c = node("c", "c", Kind::Function, "c.ts", "L1-L2");
        let g = graph(
            vec![a, b, c],
            vec![
                edge("b", "a", Relation::Calls),
                edge("c", "b", Relation::Calls),
            ],
        );
        let hits = edge_walk(&g, &g.nodes[0], Direction::In, Depth(Some(2)));
        let text = render_block(&g.nodes[0], &hits, Direction::In, 1, None);
        assert_eq!(
            text,
            "a  fn  a.ts:1-2\n1 caller \u{b7} 2 within 2 hops\n\
             \u{2514}\u{2500} b  fn  b.ts:1-2\n   \u{2514}\u{2500} c  fn  c.ts:1-2"
        );
    }
}
