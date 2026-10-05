//! `callers`: who calls, references, implements or extends a symbol (or,
//! with `--direction out`, what it calls), walked over the graph
//! (the `query-commands.md` note section 2).
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

use crate::ask::structural::resolve_symbol as resolve_symbol_matches;
use crate::grep::escape_regex;
use crate::relations::WALK_RELATIONS;

/// Which way `callers` walks the graph: `In` = who points at the symbol,
/// `Out` = what the symbol points at (section 2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    In,
    Out,
}

/// The `--depth` a walk runs to. `None` is the unbounded `all` closure
/// (section 2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Depth(pub Option<usize>);

/// Every way `callers` can fail (section 2.2 to 2.4).
#[derive(Debug, Error)]
pub enum CallersError {
    #[error("--direction must be \"in\" or \"out\", got \"{raw}\"")]
    BadDirection { raw: String },
    #[error("--depth must be a positive number or \"all\", got \"{raw}\"")]
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
        "no symbol \"{query}\" in the graph — check spelling or run {} build",
        sieve_core::product().name
    )
}

/// Renders the `PrefixNotIndexed` message, matching `grep`'s wording
/// (section 0).
fn prefix_not_indexed_message(prefix: &str, scopes: &[String]) -> String {
    let clause = if scopes.len() > 1 {
        format!(" — scopes here: {}", scopes.join(" · "))
    } else {
        String::new()
    };
    format!("nothing indexed under \"{prefix}/\"{clause} (or any path prefix)")
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
/// below `1` (section 2.3).
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
/// (section 2.2).
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
/// (section 2.4).
pub fn resolve_symbol<'a>(
    graph: &'a Graph,
    query: &str,
    in_prefix: Option<&str>,
) -> Result<Vec<&'a Node>, CallersError> {
    let mut matches = resolve_symbol_matches(graph, query, None);

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
/// (section 2.5).
#[derive(Debug, Clone)]
pub struct Hit<'a> {
    pub id: String,
    pub relation: Relation,
    pub depth: u32,
    pub node: Option<&'a Node>,
}

/// Depth-1: a plain scan over `graph.edges` in file order, keeping every
/// walk-relation edge that touches `symbol`. No dedup: two edges between
/// the same pair give two hits (section 2.5, spec correction in section 6).
fn depth1_scan<'a>(graph: &'a Graph, symbol: &Node, direction: Direction) -> Vec<Hit<'a>> {
    let by_id: HashMap<&str, &Node> = graph.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
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
            node: by_id.get(other.as_str()).copied(),
        });
    }
    hits
}

/// The BFS over walk-relation edges from every seed at once, each node
/// deduped by id and reported once at the depth it was first reached.
/// Seeds are pre-marked visited, so a seed is never its own hit
/// (section 2.5, `impactOfMany`).
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
/// (section 2.5, `edgeWalk`/`impactOfFile`).
pub fn edge_walk<'a>(
    graph: &'a Graph,
    symbol: &'a Node,
    direction: Direction,
    depth: Depth,
) -> Vec<Hit<'a>> {
    if matches!(depth, Depth(Some(n)) if n <= 1) {
        return depth1_scan(graph, symbol, direction);
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
/// (section 2.6, `fileReader`).
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
/// (section 2.6). Reads `hit`'s file through `reader`, and returns the
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

/// `<name> · <kind> · <path>:<span>` (section 2.7, `headerOf`).
pub(crate) fn header_of(node: &Node) -> String {
    format!(
        "{} · {} · {}:{}",
        node.name,
        crate::ask::kind_word(node.kind),
        node.path,
        node.span
    )
}

/// The loud, never-silent note a zero-hit symbol carries, with the
/// ambiguity clause only when more than one symbol matched the query
/// (section 2.7, `looseNoteFor`).
pub(crate) fn loose_note_for(direction: Direction, name: &str, candidate_count: usize) -> String {
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
        or find its uses with {} grep \"{name}\". Fall back to raw grep -rn only for \
        unindexed files",
        sieve_core::product().name
    )
}

/// `  <relation> <arrow> <label><depthTag>`, with a six-space quote line
/// appended when `quote` is given (section 2.7, `hitLine`).
pub(crate) fn hit_line(
    direction: Direction,
    hit: &Hit,
    show_depth: bool,
    quote: Option<(u32, String)>,
) -> String {
    let arrow = if direction == Direction::In {
        "←"
    } else {
        "→"
    };
    let depth_tag = if show_depth {
        format!(" [depth {}]", hit.depth)
    } else {
        String::new()
    };
    let label = match hit.node {
        Some(node) => format!("{} ({}:{})", node.name, node.path, node.span),
        None => format!("{} (unresolved import)", hit.id),
    };
    let line = format!("  {} {arrow} {label}{depth_tag}", hit.relation.as_str());
    match quote {
        Some((n, text)) => format!("{line}\n      {n}: {}", js_trim(&text)),
        None => line,
    }
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

/// Collapses a trailing run of newlines to exactly one, matching
/// `.replace(/\n+$/, "\n")`.
fn collapse_trailing_newlines(s: &str) -> String {
    let trimmed = s.trim_end_matches('\n');
    format!("{trimmed}\n")
}

/// Renders the plain-text `callers` report, without the savings header —
/// the CLI prepends that. `reader` is the one cached file reader this
/// whole render shares, across every matched symbol's hits (section 2.7).
pub fn format_callers(
    _query: &str,
    matches: &[(&Node, Vec<Hit>)],
    direction: Direction,
    show_depth: bool,
    reader: &mut FileReader,
) -> String {
    let candidate_count = matches.len();
    let mut lines: Vec<String> = Vec::new();
    for (symbol, hits) in matches {
        lines.push(header_of(symbol));
        if hits.is_empty() {
            lines.push(loose_note_for(direction, &symbol.name, candidate_count));
        } else {
            for hit in hits {
                let quote = quote_for(reader, &symbol.name, hit);
                lines.push(hit_line(direction, hit, show_depth, quote));
            }
        }
        lines.push(String::new());
    }
    collapse_trailing_newlines(&lines.join("\n"))
}

/// The paths `callers`' tokens-saved baseline reads: each matched symbol's
/// path, plus every resolved hit's path, in that order (section 2.8,
/// `callersSavings`). `savings_for` dedupes.
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

/// A matched symbol's `--json` shape (section 2.9).
#[derive(Debug, Clone, Serialize)]
pub struct CallersSymbolJson {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub path: String,
    pub span: String,
}

/// One hit's `--json` shape: resolved fields are present only when the
/// hit's node resolved (section 2.9).
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
/// empty (section 2.9).
#[derive(Debug, Clone, Serialize)]
pub struct CallersMatchJson {
    pub symbol: CallersSymbolJson,
    pub hits: Vec<CallersHitJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// The full `--json` payload, minus `saved`, which the CLI attaches
/// (section 2.9).
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
                    Some(loose_note_for(direction, &symbol.name, candidate_count))
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
            "no symbol \"nope\" in the graph — check spelling or run sieve build"
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
            "no symbol \"nope\" in the graph — check spelling or run sieve build"
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
        assert!(rendered.starts_with("double · function · util.ts:L1-L3\n  no indexed callers"));
    }
}
