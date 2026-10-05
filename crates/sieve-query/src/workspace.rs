//! Workspace federation for the query commands (P1-54, P1-57, P1-58,
//! P1-60): the per-child graph load, the coverage note, the merged
//! `grep`, the per-child `map`, and the per-child `callers`

use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use sieve_core::askindex::{ask_index_path, read_ask_index, AskIndex};
use sieve_core::collate::collate;
use sieve_core::workspace;
use sieve_core::{Graph, Node};

use thiserror::Error;

use crate::ask::{
    ask_ranked, round_robin_queues, AlsoMatched, AskError, AskHit, AskMode, AskOptions, AskResult,
    Ranking, ScopeMeta, PARTICIPATION_RATIO,
};
use crate::callers::{
    edge_walk, render_block, resolve_symbol, CallersError, Depth, Direction, Hit,
};
use crate::grep::{grep_graph, GrepError, GrepGroup, GrepOptions, GrepResult, GrepTruncated};
use crate::map::{build_repo_map, format_repo_map, MapOptions};

/// One built child: its dir name, its repo root, and its graph.
#[derive(Debug)]
pub struct ChildGraph {
    /// The child dir name, relative to the parent.
    pub child: String,
    /// The child's repo root, `<parent>/<child>`.
    pub root: PathBuf,
    /// The child's own `wiring.json`.
    pub graph: Graph,
}

/// Every child of a workspace: the built ones with a graph, and the names
/// of the unbuilt ones.
#[derive(Debug, Default)]
pub struct WorkspaceGraphs {
    /// The children with a graph, in sorted name order.
    pub loaded: Vec<ChildGraph>,
    /// The children with no graph, in sorted name order.
    pub missing: Vec<String>,
}

/// Loads each child's graph from its own context dir, `<child>/sieve/`.
/// The children come from the parent's index at `context_dir`, or from
/// live discovery when the index is absent. A child with no readable
/// `wiring.json` goes to `missing`, not dropped.
pub fn load_children(root: &Path, context_dir: &Path) -> WorkspaceGraphs {
    let mut children = match workspace::read(context_dir) {
        Some(ws) => ws.children,
        None => workspace::discover_children(root),
    };
    children.sort();
    let name = sieve_core::product().context_dir_name();
    let mut out = WorkspaceGraphs::default();
    for child in children {
        let child_root = root.join(&child);
        let wiring = child_root.join(name).join(".graph").join("wiring.json");
        let graph = std::fs::read(&wiring)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Graph>(&bytes).ok());
        match graph {
            Some(graph) => out.loaded.push(ChildGraph {
                child,
                root: child_root,
                graph,
            }),
            None => out.missing.push(child),
        }
    }
    out
}

/// The coverage line a federated command adds when a child is unbuilt:
/// `1 of 2 workspace repos have graphs; run sieve build to cover beta`.
/// Empty when every child has a graph.
pub fn coverage_note(g: &WorkspaceGraphs) -> String {
    if g.missing.is_empty() {
        return String::new();
    }
    format!(
        "{} of {} workspace repos have graphs; run {} build to cover {}",
        g.loaded.len(),
        g.loaded.len() + g.missing.len(),
        sieve_core::product().name,
        g.missing.join(", ")
    )
}

/// The merged `grep` over every built child, plus the summed savings
/// baseline. `saved_chars` is zero when no child had a baseline.
#[derive(Debug)]
pub struct FederatedGrep {
    /// The merged result, every path prefixed with `<child>/`.
    pub result: GrepResult,
    /// The summed baseline file count.
    pub saved_files: usize,
    /// The summed baseline chars.
    pub saved_chars: u64,
}

/// Runs `grep` in each built child and merges the results: paths get a
/// `<child>/` prefix, the counts sum, and the groups re-sort by in-degree desc
/// then path, as `localeCompare` orders them. `saved_for` gives one child's
/// `(files, chars)` baseline for its hit paths; the caller passes
/// `sieve_savings::savings_for`, which this crate cannot see `opts.in_prefix`
/// is ignored, drops `--in` at a workspace.
pub fn federate_grep(
    graphs: &WorkspaceGraphs,
    pattern: &str,
    opts: &GrepOptions,
    saved_for: impl Fn(&Graph, &[String]) -> Option<(usize, u64)>,
) -> Result<FederatedGrep, GrepError> {
    let child_opts = GrepOptions {
        in_prefix: None,
        ..opts.clone()
    };
    let mut groups: Vec<GrepGroup> = Vec::new();
    let mut files_searched = 0;
    let mut total_hits = 0;
    let mut truncated = GrepTruncated { files: 0, hits: 0 };
    let mut saved_files = 0;
    let mut saved_chars = 0;
    for ChildGraph { child, root, graph } in &graphs.loaded {
        let r = grep_graph(graph, root, pattern, &child_opts)?;
        files_searched += r.files_searched;
        total_hits += r.total_hits;
        truncated.files += r.truncated.files;
        truncated.hits += r.truncated.hits;
        let paths: Vec<String> = r.groups.iter().map(|g| g.path.clone()).collect();
        if let Some((files, chars)) = saved_for(graph, &paths) {
            saved_files += files;
            saved_chars += chars;
        }
        for mut g in r.groups {
            g.path = format!("{child}/{}", g.path);
            if let Some(symbol) = g.symbol.as_mut() {
                symbol.path = format!("{child}/{}", symbol.path);
            }
            groups.push(g);
        }
    }
    groups.sort_by(|a, b| {
        b.in_degree
            .cmp(&a.in_degree)
            .then_with(|| collate(&a.path, &b.path))
    });
    Ok(FederatedGrep {
        result: GrepResult {
            pattern: pattern.to_string(),
            files_searched,
            total_hits,
            groups,
            truncated,
        },
        saved_files,
        saved_chars,
    })
}

/// The workspace `map` text (P1-54): the `workspace map — N repo(s)` head, then
/// one `## <child>/` section per built child, then the coverage note.
/// `max_dirs` splits evenly across the built children, at least 1 each; `None`
/// keeps the default. `render` wraps one child's map body in its savings
/// header; the caller passes `sieve_savings::with_savings`, which this crate
/// cannot see.
pub fn federate_map(
    graphs: &WorkspaceGraphs,
    max_dirs: Option<usize>,
    render: impl Fn(&Graph, &str) -> String,
) -> String {
    let per_child = match max_dirs {
        Some(n) if !graphs.loaded.is_empty() => Some((n / graphs.loaded.len()).max(1)),
        _ => None,
    };
    let sections: Vec<String> = graphs
        .loaded
        .iter()
        .map(|ChildGraph { child, graph, .. }| {
            let opts = MapOptions {
                max_dirs: per_child.unwrap_or(MapOptions::default().max_dirs),
                ..MapOptions::default()
            };
            let rendered = format_repo_map(&build_repo_map(graph, &opts));
            // Sieve measures the savings pack on the body with no
            // trailing newline (see `map.rs` in the CLI), then `trimEnd`s
            // the whole section.
            let body = rendered.strip_suffix('\n').unwrap_or(&rendered);
            format!("## {child}/\n{}", render(graph, body).trim_end())
        })
        .collect();
    let mut parts = vec![
        format!("workspace map — {} repo(s)", graphs.loaded.len()),
        String::new(),
        sections.join("\n\n"),
    ];
    let cov = coverage_note(graphs);
    if !cov.is_empty() {
        parts.push(String::new());
        parts.push(cov);
    }
    parts.join("\n") + "\n"
}

/// The workspace `callers` text (P1-58).
#[derive(Debug)]
pub struct FederatedCallers {
    /// The blocks joined, plus the coverage note; or the miss line, plus
    /// the coverage note.
    pub text: String,
    /// False when no child resolved the symbol.
    pub found: bool,
}

/// Runs `callers` in each built child: one `## <child>/` block per child
/// that resolves `symbol`, with child-relative paths and no call-site
/// quote, each block wrapped by `render` in its own savings header. A
/// child that resolves nothing prints no block. `render` takes the
/// child's graph, the block body, and the savings paths; the caller
/// passes `sieve_savings::with_savings`. A bad `--in` prefix errors:
/// it throws; a plain miss is `found: false`.
pub fn federate_callers(
    graphs: &WorkspaceGraphs,
    symbol: &str,
    in_prefix: Option<&str>,
    direction: Direction,
    depth: Depth,
    render: impl Fn(&Graph, &str, &[String]) -> String,
) -> Result<FederatedCallers, CallersError> {
    let mut blocks = Vec::new();
    for ChildGraph { child, graph, .. } in &graphs.loaded {
        let matches = match resolve_symbol(graph, symbol, in_prefix) {
            Ok(m) => m,
            Err(CallersError::NoSymbol { .. }) => continue,
            Err(e) => return Err(e),
        };
        let results: Vec<(&Node, Vec<Hit>)> = matches
            .iter()
            .map(|m| (*m, edge_walk(graph, m, direction, depth)))
            .collect();
        let mut lines = vec![format!("## {child}/")];
        for (sym, hits) in &results {
            lines.push(render_block(sym, hits, direction, matches.len(), None));
        }
        let paths = crate::callers::callers_saved_paths(&results);
        blocks.push(render(graph, &lines.join("\n"), &paths));
    }
    let cov = coverage_note(graphs);
    if blocks.is_empty() {
        let base = format!(
            "no symbol named {symbol} in any of the {} workspace repos — try {} grep {symbol}",
            graphs.loaded.len(),
            sieve_core::product().name
        );
        let text = if cov.is_empty() {
            base
        } else {
            format!("{base}\n{cov}")
        };
        return Ok(FederatedCallers { text, found: false });
    }
    let mut text = blocks.join("\n\n");
    if !cov.is_empty() {
        text.push_str("\n\n");
        text.push_str(&cov);
    }
    Ok(FederatedCallers { text, found: true })
}

/// The flags a federated `ask` runs with (: limit,
/// source, full, in; the LLM and rank flags never reach a child).
#[derive(Debug, Clone)]
pub struct FederateAskOptions {
    /// The most hits the merged result holds.
    pub limit: usize,
    /// Inline each hit's source.
    pub source: bool,
    /// With `source`, inline whole spans.
    pub full: bool,
    /// `<child>[/<sub-prefix>]`; an empty string means no scope.
    pub in_prefix: Option<String>,
}

/// Every way a federated `ask` can fail.
#[derive(Debug, Error)]
pub enum FederateAskError {
    /// The first `--in` segment names no workspace child.
    #[error("no workspace repo named {name} \u{2014} use one of: {repos}")]
    UnknownChild {
        /// The first `--in` segment.
        name: String,
        /// Every child, sorted, joined by `, `.
        repos: String,
    },
}

/// One doc a scope fusion ranks (`{ id, scope, score }`).
#[derive(Debug, Clone, PartialEq)]
pub struct ScopedDoc {
    /// The doc id.
    pub id: String,
    /// The scope the doc ranks in.
    pub scope: String,
    /// The doc score.
    pub score: f64,
}

/// The outcome of [`fuse_scopes`].
#[derive(Debug, Clone, Default)]
pub struct Fusion {
    /// The fused docs, best first.
    pub ranked: Vec<ScopedDoc>,
    /// The scopes that joined, best first.
    pub federated: Vec<String>,
    /// The scopes gated out, with their best doc id.
    pub also_matched: Vec<AlsoMatched>,
}

/// The reciprocal-rank smoothing constant.
const RRF_K: usize = 60;

/// Score desc, then id.
fn doc_order(a: &ScopedDoc, b: &ScopedDoc) -> Ordering {
    b.score
        .partial_cmp(&a.score)
        .unwrap_or(Ordering::Equal)
        .then_with(|| collate(&a.id, &b.id))
}

/// Ports `fuseScopes`: drops score 0 or less, ranks
/// each scope, passes one scope through with its raw scores, else gates on
/// a quarter of the best scope and fuses by reciprocal rank. Each doc adds
/// `1.0 / (K + i + 1)`, and the sum divides by the max, as the two IEEE
/// operations read in the source.
pub fn fuse_scopes(docs: Vec<ScopedDoc>) -> Fusion {
    let mut by_scope: Vec<(String, Vec<ScopedDoc>)> = Vec::new();
    for d in docs {
        if d.score <= 0.0 {
            continue;
        }
        match by_scope.iter_mut().find(|(s, _)| *s == d.scope) {
            Some((_, list)) => list.push(d),
            None => by_scope.push((d.scope.clone(), vec![d])),
        }
    }
    for (_, list) in &mut by_scope {
        list.sort_by(doc_order);
    }
    if by_scope.is_empty() {
        return Fusion::default();
    }
    if by_scope.len() == 1 {
        let (scope, list) = by_scope.remove(0);
        return Fusion {
            ranked: list,
            federated: vec![scope],
            also_matched: Vec::new(),
        };
    }

    by_scope.sort_by(|a, b| {
        b.1[0]
            .score
            .partial_cmp(&a.1[0].score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| collate(&a.0, &b.0))
    });
    let gate = PARTICIPATION_RATIO * by_scope[0].1[0].score;
    let mut federated: Vec<&(String, Vec<ScopedDoc>)> = Vec::new();
    let mut also_matched = Vec::new();
    for entry in &by_scope {
        if entry.1[0].score >= gate {
            federated.push(entry);
        } else {
            also_matched.push(AlsoMatched {
                scope: entry.0.clone(),
                best_id: entry.1[0].id.clone(),
            });
        }
    }

    // id, scope, summed score, best rank
    let mut acc: Vec<(String, String, f64, usize)> = Vec::new();
    let mut slot: HashMap<String, usize> = HashMap::new();
    for (scope, list) in federated.iter().map(|e| (&e.0, &e.1)) {
        for (i, d) in list.iter().enumerate() {
            let contribution = 1.0 / ((RRF_K + i + 1) as f64);
            match slot.get(&d.id) {
                None => {
                    slot.insert(d.id.clone(), acc.len());
                    acc.push((d.id.clone(), scope.clone(), contribution, i));
                }
                Some(&at) => {
                    let prev = &mut acc[at];
                    prev.2 += contribution;
                    if i < prev.3 {
                        prev.1 = scope.clone();
                        prev.3 = i;
                    }
                }
            }
        }
    }
    let mut max = 0.0_f64;
    for e in &acc {
        if e.2 > max {
            max = e.2;
        }
    }
    let mut ranked: Vec<ScopedDoc> = acc
        .into_iter()
        .map(|(id, scope, score, _)| ScopedDoc {
            id,
            scope,
            score: score / max,
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| collate(&a.scope, &b.scope))
            .then_with(|| collate(&a.id, &b.id))
    });
    Fusion {
        ranked,
        federated: federated.iter().map(|e| e.0.clone()).collect(),
        also_matched,
    }
}

/// Prefixes each comma part of a pointer with its child dir, so a
/// `path:span`, or a concept's path list, opens from the parent. A part
/// that is empty or holds a space is free text and keeps no prefix.
pub fn prefix_pointer(child: &str, pointer: &str) -> String {
    /// The JavaScript `trim` set: Rust's white space, less U+0085, plus
    /// U+FEFF.
    fn is_js_space(c: char) -> bool {
        (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}'
    }
    fn js_trim(s: &str) -> &str {
        s.trim_matches(is_js_space)
    }
    pointer
        .split(',')
        .map(|part| {
            let t = js_trim(part);
            if t.is_empty() || t.contains(' ') {
                part.to_string()
            } else {
                format!("{child}/{t}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// A child's fusion scope: `<child>/<scope>`, or the bare child for the
/// root scope, which is the empty string.
fn fusion_scope(child: &str, hit: &AskHit) -> String {
    match hit.scope.as_deref() {
        Some(s) if !s.is_empty() => format!("{child}/{s}"),
        _ => child.to_string(),
    }
}

/// `qualifyHit`: the hit with a new score and scope, and its pointer prefixed.
/// The JSON key `scope` stays where it was, or lands after `code` when the hit
/// had none.
fn qualify_hit(child: &str, hit: &AskHit, score: f64, scope: String) -> AskHit {
    AskHit {
        score,
        scope_last: hit.scope.is_none(),
        scope: Some(scope),
        pointer: prefix_pointer(child, &hit.pointer),
        ..hit.clone()
    }
}

/// `sameHit`: kind, pointer and title.
fn same_hit(a: &AskHit, b: &AskHit) -> bool {
    a.kind == b.kind && a.pointer == b.pointer && a.title == b.title
}

/// One child's ask run, kept for the fusion.
struct ChildRun<'a> {
    child: &'a str,
    hits: Vec<AskHit>,
    ranking: Option<Ranking>,
    coverage: f64,
    coverage_strong: f64,
    baseline_coverage: f64,
    baseline_coverage_strong: f64,
}

/// One file or concept group after the file-stream fusion.
struct RankedGroup {
    key: String,
    child: String,
    hits: Vec<AskHit>,
    baseline_hits: Vec<AskHit>,
}

/// A child survives the gate when its top hit matched a name or path term
/// (`STRONG_FLOOR`), or its coverage is broad (`HIGH_FLOOR`).
const STRONG_FLOOR: f64 = 0.1;
const HIGH_FLOOR: f64 = 0.5;

/// The lock: the baseline top's group moves to the front. Its first hit is the
/// top at the baseline fused score, and its tail takes the file-stream score of
/// the group leader. A group absent from `projected` locks as a singleton, with
/// no tail.
fn lock_top(
    mut projected: Vec<RankedGroup>,
    top: AskHit,
    key: String,
    child: &str,
) -> Vec<RankedGroup> {
    let locked = match projected.iter().position(|g| g.key == key) {
        Some(at) => {
            let existing = projected.remove(at);
            let projected_score = existing.hits.first().map_or(top.score, |h| h.score);
            let mut hits = vec![top.clone()];
            hits.extend(
                existing
                    .baseline_hits
                    .iter()
                    .filter(|h| !same_hit(h, &top))
                    .map(|h| AskHit {
                        score: projected_score,
                        ..h.clone()
                    }),
            );
            RankedGroup { hits, ..existing }
        }
        None => RankedGroup {
            key,
            child: child.to_string(),
            hits: vec![top.clone()],
            baseline_hits: vec![top],
        },
    };
    projected.insert(0, locked);
    projected
}

/// The scope footer: the locked scope first, then
/// the fused scopes, with no repeat. A scope that is federated leaves the
/// also-matched list.
fn merge_scopes(
    locked_scope: Option<String>,
    fused_federated: Vec<String>,
    fused_also_matched: Vec<AlsoMatched>,
    gated_out: Vec<AlsoMatched>,
) -> ScopeMeta {
    let mut federated: Vec<String> = Vec::new();
    for scope in locked_scope.into_iter().chain(fused_federated) {
        if !federated.contains(&scope) {
            federated.push(scope);
        }
    }
    let also_matched = fused_also_matched
        .into_iter()
        .chain(gated_out)
        .filter(|m| !federated.contains(&m.scope))
        .collect();
    ScopeMeta {
        federated,
        also_matched,
    }
}

/// The federated `ask` across a workspace (P1-56, the `fileTopLock` branch).
/// Each built child runs its own ask with ranking metadata. The merged result
/// holds the baseline top first, then the files by reciprocal-rank fusion,
/// round robin over the files.
pub fn federate_ask(
    graphs: &WorkspaceGraphs,
    query: &str,
    opts: &FederateAskOptions,
) -> Result<AskResult, FederateAskError> {
    federate_ask_with(graphs, query, opts, ask_ranked)
}

/// One child's ask, as `federate_ask_with` runs it.
type ChildAsk = fn(
    &Graph,
    Option<&AskIndex>,
    &str,
    &AskOptions,
    &Path,
) -> Result<(AskResult, Option<Ranking>), AskError>;

/// [`federate_ask`] with the child runner injected, so a test can force a
/// panic in one child.
fn federate_ask_with(
    graphs: &WorkspaceGraphs,
    query: &str,
    opts: &FederateAskOptions,
    child_ask: ChildAsk,
) -> Result<AskResult, FederateAskError> {
    let limit = opts.limit;

    // `--in` scopes to one child, and past the first segment to a sub-scope
    // of it. An unknown first segment is a caller mistake.
    let mut only_child: Option<String> = None;
    let mut child_in: Option<String> = None;
    if let Some(raw) = opts.in_prefix.as_deref().filter(|p| !p.is_empty()) {
        let prefix = raw.trim_end_matches('/');
        let mut parts = prefix.split('/');
        let name = parts.next().unwrap_or_default();
        let rest: Vec<&str> = parts.collect();
        let mut all: Vec<&str> = graphs
            .loaded
            .iter()
            .map(|l| l.child.as_str())
            .chain(graphs.missing.iter().map(String::as_str))
            .collect();
        // JavaScript's default sort compares UTF-16 code units.
        all.sort_by_key(|s| s.encode_utf16().collect::<Vec<u16>>());
        if !all.contains(&name) {
            return Err(FederateAskError::UnknownChild {
                name: name.to_string(),
                repos: all.join(", "),
            });
        }
        only_child = Some(name.to_string());
        if !rest.is_empty() {
            child_in = Some(rest.join("/"));
        }
    }

    // Pass 1: each child's own ask, over-fetched so the fusion has
    // candidates to rank before the final cut.
    let child_limit = limit.saturating_mul(4).max(20);
    let context_name = sieve_core::product().context_dir_name();
    let mut runs: Vec<ChildRun> = Vec::new();
    for ChildGraph { child, root, graph } in &graphs.loaded {
        if only_child.as_ref().is_some_and(|only| only != child) {
            continue;
        }
        let index = read_ask_index(&ask_index_path(&root.join(context_name)));
        let child_opts = AskOptions {
            limit: child_limit,
            in_prefix: child_in.clone(),
            graph_rank: true,
            source: opts.source,
            full: opts.full,
        };
        // Sieve skips a child whose ask throws, for any reason (a bad
        // sub-prefix, a corrupt child). A panic is the
        // Rust throw.
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            child_ask(graph, index.as_ref(), query, &child_opts, root)
        }));
        let Ok(Ok((r, ranking))) = ran else {
            continue;
        };
        if r.hits.is_empty() {
            continue;
        }
        let top_group = ranking.as_ref().and_then(|g| g.groups.first());
        runs.push(ChildRun {
            child,
            coverage: top_group.map(|g| g.coverage).or(r.coverage).unwrap_or(0.0),
            coverage_strong: top_group
                .map(|g| g.coverage_strong)
                .or(r.coverage_strong)
                .unwrap_or(0.0),
            baseline_coverage: ranking
                .as_ref()
                .map(|g| g.baseline_coverage)
                .or(r.coverage)
                .unwrap_or(0.0),
            baseline_coverage_strong: ranking
                .as_ref()
                .map(|g| g.baseline_coverage_strong)
                .or(r.coverage_strong)
                .unwrap_or(0.0),
            hits: r.hits,
            ranking,
        });
    }

    // The participation gate, on match strength. `--in <child>` skips it.
    let mut gated_out: Vec<AlsoMatched> = Vec::new();
    let mut survivors: Vec<&ChildRun> = Vec::new();
    let mut baseline_survivors: Vec<&ChildRun> = Vec::new();
    for run in &runs {
        let baseline_eligible = only_child.is_some()
            || run.baseline_coverage_strong >= STRONG_FLOOR
            || run.baseline_coverage >= HIGH_FLOOR;
        let file_eligible = only_child.is_some()
            || run.coverage_strong >= STRONG_FLOOR
            || run.coverage >= HIGH_FLOOR;
        if baseline_eligible {
            baseline_survivors.push(run);
        }
        if baseline_eligible || file_eligible {
            survivors.push(run);
        } else {
            let best = run
                .ranking
                .as_ref()
                .and_then(|g| g.groups.first())
                .and_then(|g| g.hits.first())
                .or(run.hits.first());
            if let Some(best) = best {
                gated_out.push(AlsoMatched {
                    scope: run.child.to_string(),
                    best_id: prefix_pointer(run.child, &best.pointer),
                });
            }
        }
    }

    // The baseline stream: the all-span fusion, whose top is the lock. The
    // id keeps the unpadded index, so equal-score ties order as JS
    // `localeCompare` does.
    let mut baseline_docs: Vec<ScopedDoc> = Vec::new();
    let mut baseline_back: HashMap<String, (&ChildRun, String, &AskHit)> = HashMap::new();
    for run in &baseline_survivors {
        let entries: Vec<(String, &AskHit)> = match &run.ranking {
            Some(r) => r.baseline.iter().map(|(g, h)| (g.clone(), h)).collect(),
            None => run
                .hits
                .iter()
                .enumerate()
                .map(|(i, h)| (format!("singleton:{i}"), h))
                .collect(),
        };
        for (index, (group, hit)) in entries.into_iter().enumerate() {
            let id = format!("{} {index}", run.child);
            baseline_docs.push(ScopedDoc {
                id: id.clone(),
                scope: fusion_scope(run.child, hit),
                score: hit.score,
            });
            baseline_back.insert(id, (run, format!("{}\0{group}", run.child), hit));
        }
    }
    let baseline_fused = fuse_scopes(baseline_docs);
    let baseline_top = baseline_fused.ranked.first().and_then(|top| {
        let (run, group, hit) = baseline_back.get(&top.id)?;
        Some((
            qualify_hit(run.child, hit, top.score, top.scope.clone()),
            group.clone(),
            run.child,
        ))
    });

    // The file stream: one leader per child file; concepts stay singleton
    // groups. Span queues ride in a side map and take no rank position.
    let mut file_docs: Vec<ScopedDoc> = Vec::new();
    let mut file_back: HashMap<String, RankedGroup> = HashMap::new();
    for run in &survivors {
        let groups: Vec<(String, Vec<&AskHit>, Vec<&AskHit>)> = match &run.ranking {
            Some(r) => r
                .groups
                .iter()
                .map(|g| {
                    (
                        g.key.clone(),
                        g.hits.iter().collect(),
                        g.baseline_hits.iter().collect(),
                    )
                })
                .collect(),
            None => run
                .hits
                .iter()
                .enumerate()
                .map(|(i, h)| (format!("singleton:{i}"), vec![h], vec![h]))
                .collect(),
        };
        for (index, (key, hits, baseline_hits)) in groups.into_iter().enumerate() {
            let Some(&leader) = hits.first() else {
                continue;
            };
            let id = format!("{} file {index:08}", run.child);
            file_docs.push(ScopedDoc {
                id: id.clone(),
                scope: fusion_scope(run.child, leader),
                score: leader.score,
            });
            let baseline_hits = if baseline_hits.is_empty() {
                vec![leader]
            } else {
                baseline_hits
            };
            file_back.insert(
                id,
                RankedGroup {
                    key: format!("{}\0{key}", run.child),
                    child: run.child.to_string(),
                    hits: hits.into_iter().cloned().collect(),
                    baseline_hits: baseline_hits.into_iter().cloned().collect(),
                },
            );
        }
    }
    let file_fused = fuse_scopes(file_docs);
    let mut projected: Vec<RankedGroup> = file_fused
        .ranked
        .iter()
        .filter_map(|ranked| {
            let group = file_back.remove(&ranked.id)?;
            // The spans project one cross-child file document, so they
            // carry its reciprocal-rank score.
            let hits = group
                .hits
                .iter()
                .map(|h| qualify_hit(&group.child, h, ranked.score, fusion_scope(&group.child, h)))
                .collect();
            let baseline_hits = group
                .baseline_hits
                .iter()
                .map(|h| qualify_hit(&group.child, h, h.score, fusion_scope(&group.child, h)))
                .collect();
            Some(RankedGroup {
                hits,
                baseline_hits,
                ..group
            })
        })
        .collect();

    // The lock: the baseline top's group moves to the front, its first hit
    // at the baseline fused score and its tail at the file-stream score.
    let locked_scope = baseline_top
        .as_ref()
        .and_then(|(top, _, _)| top.scope.clone());
    if let Some((top, key, child)) = baseline_top {
        projected = lock_top(projected, top, key, child);
    }
    let queues: Vec<Vec<AskHit>> = projected.into_iter().map(|g| g.hits).collect();
    let hits = if limit == 0 {
        Vec::new()
    } else {
        round_robin_queues(&queues, limit)
    };

    let coverage = coverage_note(graphs);
    let mut note = None;
    let mut scopes = None;
    if hits.is_empty() {
        note = Some(format!(
            "no matching nodes across {} workspace repo(s) — try different words, or `{} build` at a child",
            graphs.loaded.len(),
            sieve_core::product().name
        ));
    } else {
        scopes = Some(merge_scopes(
            locked_scope,
            file_fused.federated,
            file_fused.also_matched,
            gated_out,
        ));
    }
    if !coverage.is_empty() {
        note = Some(match note {
            Some(n) => format!("{n}\n{coverage}"),
            None => coverage,
        });
    }
    Ok(AskResult {
        query: query.to_string(),
        mode: if hits.is_empty() {
            AskMode::Empty
        } else {
            AskMode::Lexical
        },
        subject: None,
        hits,
        scopes,
        coverage: None,
        coverage_strong: None,
        ranking: None,
        note,
        saved: None,
        pagerank_for_test: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grep::DEFAULT_MAX_HITS;
    use crate::test_support::TempDir;
    use sieve_core::{Kind, Meta, Node, Origin, SummaryState};

    fn file_node(path: &str, chars: u64) -> Node {
        Node {
            id: path.to_string(),
            name: path.to_string(),
            kind: Kind::File,
            path: path.to_string(),
            span: "L1-L1".to_string(),
            signature: None,
            exported: false,
            origin: Origin::Ast,
            body_hash: "0".repeat(64),
            chars: Some(chars),
            body_text: None,
            arity: None,
            variadic: None,
            summary_state: SummaryState::Pending,
            summary: None,
            crux: None,
            owner: None,
        }
    }

    fn graph_with_file(path: &str, chars: u64) -> Graph {
        Graph {
            meta: Meta {
                version: 1,
                node_count: 1,
                edge_count: 0,
                languages: Vec::new(),
                scopes: Vec::new(),
            },
            nodes: vec![file_node(path, chars)],
            edges: Vec::new(),
        }
    }

    /// Writes a child with one source file and a built `wiring.json`.
    fn write_child(root: &Path, child: &str, body: &str) {
        let child_root = root.join(child);
        std::fs::create_dir_all(child_root.join("sieve/.graph")).expect("mkdir");
        std::fs::write(child_root.join("a.ts"), body).expect("write source");
        let graph = graph_with_file("a.ts", body.len() as u64);
        std::fs::write(
            child_root.join("sieve/.graph/wiring.json"),
            serde_json::to_vec(&graph).expect("serialize"),
        )
        .expect("write wiring");
    }

    fn opts() -> GrepOptions {
        GrepOptions {
            ignore_case: false,
            fixed: false,
            in_prefix: Some("alpha".to_string()),
            max_hits: DEFAULT_MAX_HITS,
        }
    }

    /// P1-57, P1-60: an unbuilt or unparseable child goes to `missing`;
    /// the note counts it; the merged grep prefixes each path, sums the
    /// counts and the baseline, and ignores `--in`.
    #[test]
    fn test_p1_57_federate_grep_prefixes_paths_and_sums_counts() {
        let dir = TempDir::new("ws-grep");
        write_child(&dir, "beta", "hit here\n");
        write_child(&dir, "alpha", "hit one\nhit two\n");
        std::fs::create_dir_all(dir.join("gamma/sieve/.graph")).expect("mkdir");
        std::fs::write(dir.join("gamma/sieve/.graph/wiring.json"), "nope").expect("write");
        let context_dir = dir.join("sieve");
        workspace::write(
            &context_dir,
            &[
                "beta".into(),
                "alpha".into(),
                "gamma".into(),
                "delta".into(),
            ],
        )
        .expect("write index");

        let graphs = load_children(&dir, &context_dir);
        let loaded: Vec<&str> = graphs.loaded.iter().map(|c| c.child.as_str()).collect();
        assert_eq!(loaded, ["alpha", "beta"]);
        assert_eq!(graphs.missing, ["delta", "gamma"]);
        assert_eq!(
            coverage_note(&graphs),
            "2 of 4 workspace repos have graphs; run sieve build to cover delta, gamma"
        );

        let fed = federate_grep(&graphs, "hit", &opts(), |graph, paths| {
            assert_eq!(paths, ["a.ts"]);
            graph.nodes[0].chars.map(|c| (1, c))
        })
        .expect("grep runs");
        assert_eq!(fed.result.pattern, "hit");
        assert_eq!(fed.result.files_searched, 2);
        assert_eq!(fed.result.total_hits, 3);
        let paths: Vec<&str> = fed.result.groups.iter().map(|g| g.path.as_str()).collect();
        assert_eq!(paths, ["alpha/a.ts", "beta/a.ts"]);
        assert_eq!(fed.saved_files, 2);
        assert_eq!(fed.saved_chars, 9 + 16);
    }

    fn symbol_node(id: &str, path: &str, span: &str) -> Node {
        Node {
            id: id.to_string(),
            name: id.to_string(),
            kind: Kind::Function,
            span: span.to_string(),
            exported: true,
            ..file_node(path, 0)
        }
    }

    /// A child graph with `a.ts`, `shared` and `useShared` calling it.
    fn callers_graph(path: &str) -> Graph {
        let mut g = graph_with_file(path, 50);
        g.nodes.push(symbol_node("shared", path, "L1-L3"));
        g.nodes.push(symbol_node("useShared", path, "L5-L7"));
        g.edges.push(sieve_core::Edge {
            source: "useShared".to_string(),
            target: "shared".to_string(),
            relation: sieve_core::Relation::Calls,
            confidence: sieve_core::Confidence::Extracted,
        });
        g
    }

    fn in_memory(loaded: Vec<(&str, Graph)>, missing: &[&str]) -> WorkspaceGraphs {
        WorkspaceGraphs {
            loaded: loaded
                .into_iter()
                .map(|(child, graph)| ChildGraph {
                    child: child.to_string(),
                    root: PathBuf::from(child),
                    graph,
                })
                .collect(),
            missing: missing.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// P1-54: the head, one section per built child with the render
    /// applied to the body without its trailing newline, `--max-dirs`
    /// split evenly with a floor of 1, then the coverage note.
    #[test]
    fn test_p1_54_federate_map_sections_each_child_and_splits_max_dirs() {
        let graphs = in_memory(
            vec![
                ("alpha", graph_with_file("a.ts", 10)),
                ("beta", graph_with_file("b.ts", 10)),
            ],
            &["gamma"],
        );
        let text = federate_map(&graphs, Some(3), |_, body| {
            assert!(!body.ends_with('\n'), "body keeps no trailing newline");
            format!("[saved]\n\n{body}\n")
        });
        let alpha = format_repo_map(&build_repo_map(
            &graphs.loaded[0].graph,
            &MapOptions {
                max_dirs: 1,
                ..MapOptions::default()
            },
        ));
        let beta = format_repo_map(&build_repo_map(
            &graphs.loaded[1].graph,
            &MapOptions {
                max_dirs: 1,
                ..MapOptions::default()
            },
        ));
        assert_eq!(
            text,
            format!(
                "workspace map — 2 repo(s)\n\n## alpha/\n[saved]\n\n{}\n\n## beta/\n[saved]\n\n{}\n\n\
                 2 of 3 workspace repos have graphs; run sieve build to cover gamma\n",
                alpha.trim_end(),
                beta.trim_end()
            )
        );
        // The split floors at 1 and ignores a split when nothing is built.
        assert!(
            federate_map(&in_memory(vec![], &[]), Some(1), |_, b| b.to_string())
                .starts_with("workspace map — 0 repo(s)\n")
        );
    }

    /// P1-58: one block per child with a match, child-relative paths, no
    /// quote line, the render given the saved paths; a miss in every
    /// child is `found: false` with the workspace miss line.
    #[test]
    fn test_p1_58_federate_callers_blocks_each_child_with_a_hit() {
        let graphs = in_memory(
            vec![
                ("alpha", callers_graph("a.ts")),
                ("beta", graph_with_file("b.ts", 10)),
            ],
            &["gamma"],
        );
        let fed = federate_callers(
            &graphs,
            "shared",
            None,
            Direction::In,
            Depth(Some(2)),
            |_, body, paths| {
                assert_eq!(paths, ["a.ts", "a.ts"]);
                format!("{body}\n[saved]")
            },
        )
        .expect("resolves");
        assert!(fed.found);
        assert_eq!(
            fed.text,
            "## alpha/\nshared  fn  a.ts:1-3\n1 caller\n\u{2514}\u{2500} useShared  fn  a.ts:5-7\n[saved]\n\n\
             2 of 3 workspace repos have graphs; run sieve build to cover gamma"
        );

        let miss = federate_callers(
            &graphs,
            "zzz",
            None,
            Direction::In,
            Depth(Some(1)),
            |_, body, _| body.to_string(),
        )
        .expect("a miss is not an error");
        assert!(!miss.found);
        assert_eq!(
            miss.text,
            "no symbol named zzz in any of the 2 workspace repos — try sieve grep zzz\n\
             2 of 3 workspace repos have graphs; run sieve build to cover gamma"
        );
    }

    /// P1-60: every child built gives an empty note, and discovery
    /// stands in for a missing index.
    #[test]
    fn test_p1_60_coverage_note_is_empty_when_every_child_is_built() {
        let dir = TempDir::new("ws-cov");
        write_child(&dir, "alpha", "x\n");
        std::fs::create_dir_all(dir.join("alpha/.git")).expect("mkdir");
        let graphs = load_children(&dir, &dir.join("sieve"));
        assert_eq!(graphs.loaded.len(), 1);
        assert!(graphs.missing.is_empty());
        assert_eq!(coverage_note(&graphs), "");
    }

    fn scoped(id: &str, scope: &str, score: f64) -> ScopedDoc {
        ScopedDoc {
            id: id.to_string(),
            scope: scope.to_string(),
            score,
        }
    }

    /// P1-56: the reciprocal-rank sums and the divide by the max keep the exact
    /// bits. A tie sorts by score, then scope, then id, and the id order is
    /// plain `localeCompare`, so `alpha 10` comes before `alpha 9`. A score of
    /// 0 or less drops out. A scope below a quarter of the best scope is gated.
    /// The expected bits come from a recorded `fuseScopes` run on the same
    /// docs.
    #[test]
    fn test_p1_56_fuse_scopes_rrf_bits_and_ties() {
        let fused = fuse_scopes(vec![
            scoped("alpha 0", "alpha", 1.0),
            scoped("alpha 1", "alpha", 0.9),
            scoped("alpha 10", "alpha", 0.9),
            scoped("alpha 9", "alpha", 0.9),
            scoped("alpha 2", "alpha", 0.0),
            scoped("beta 0", "beta/x", 1.5),
            scoped("beta 1", "beta/x", 0.5),
            scoped("beta 2", "beta/x", 0.5),
            scoped("gamma 0", "gamma", 0.3),
            scoped("delta 0", "delta", 1.0),
            scoped("delta 1", "delta", 0.4),
        ]);
        let got: Vec<(&str, &str, u64)> = fused
            .ranked
            .iter()
            .map(|d| (d.id.as_str(), d.scope.as_str(), d.score.to_bits()))
            .collect();
        assert_eq!(
            got,
            [
                ("alpha 0", "alpha", 0x3ff0000000000000),
                ("beta 0", "beta/x", 0x3ff0000000000000),
                ("delta 0", "delta", 0x3ff0000000000000),
                ("alpha 1", "alpha", 0x3fef7bdef7bdef7b),
                ("beta 1", "beta/x", 0x3fef7bdef7bdef7b),
                ("delta 1", "delta", 0x3fef7bdef7bdef7b),
                ("alpha 10", "alpha", 0x3feefbefbefbefbe),
                ("beta 2", "beta/x", 0x3feefbefbefbefbe),
                ("alpha 9", "alpha", 0x3fee800000000000),
            ]
        );
        assert_eq!(fused.federated, ["beta/x", "alpha", "delta"]);
        assert_eq!(fused.also_matched.len(), 1);
        assert_eq!(fused.also_matched[0].scope, "gamma");
        assert_eq!(fused.also_matched[0].best_id, "gamma 0");

        // One scope passes its raw scores, in score then id order.
        let one = fuse_scopes(vec![
            scoped("a 1", "s", 0.5),
            scoped("a 0", "s", 0.5),
            scoped("a 2", "s", -1.0),
        ]);
        let ids: Vec<&str> = one.ranked.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["a 0", "a 1"]);
        assert_eq!(one.ranked[0].score, 0.5);
        assert_eq!(one.federated, ["s"]);
    }

    /// P1-56: each comma part of a concept pointer gets the child prefix; a
    /// part with a space is free text and keeps its own text, leading space
    /// included.
    #[test]
    fn test_p1_56_prefix_pointer_skips_free_text() {
        assert_eq!(
            prefix_pointer("alpha", "src/a.ts:L1-L3"),
            "alpha/src/a.ts:L1-L3"
        );
        assert_eq!(
            prefix_pointer("alpha", "src/a.ts, src/b.ts"),
            "alpha/src/a.ts, alpha/src/b.ts"
        );
        assert_eq!(
            prefix_pointer("alpha", "src/a.ts, see the notes"),
            "alpha/src/a.ts,  see the notes"
        );
        assert_eq!(prefix_pointer("alpha", "slug"), "alpha/slug");
        // JavaScript `trim` keeps U+0085 and strips U+FEFF.
        assert_eq!(prefix_pointer("alpha", "\u{85}"), "alpha/\u{85}");
        assert_eq!(prefix_pointer("alpha", " \u{feff}"), " \u{feff}");
    }

    fn hit_of(title: &str, score: f64) -> AskHit {
        AskHit {
            id: title.to_string(),
            kind: "symbol".to_string(),
            title: title.to_string(),
            pointer: format!("{title}.ts:L1-L2"),
            snippet: String::new(),
            related: Vec::new(),
            relation: None,
            score,
            scope: Some("alpha".to_string()),
            code: None,
            scope_last: false,
            path: String::new(),
            span: None,
        }
    }

    /// P1-56: the lock puts the baseline top first at its own score, and the
    /// tail takes the group leader's score, not the top's. A group absent from
    /// the ranked list locks as a singleton.
    #[test]
    fn test_p1_56_lock_tail_score_and_the_singleton_arm() {
        let group = |key: &str, lead: f64| RankedGroup {
            key: key.to_string(),
            child: "alpha".to_string(),
            hits: vec![hit_of("lead", lead)],
            baseline_hits: vec![hit_of("top", 0.9), hit_of("tail", 0.5)],
        };
        let projected = vec![group("g1", 0.8), group("g2", 0.7)];
        let locked = lock_top(projected, hit_of("top", 1.0), "g2".to_string(), "alpha");
        let keys: Vec<&str> = locked.iter().map(|g| g.key.as_str()).collect();
        assert_eq!(keys, ["g2", "g1"]);
        let scores: Vec<(&str, f64)> = locked[0]
            .hits
            .iter()
            .map(|h| (h.title.as_str(), h.score))
            .collect();
        assert_eq!(scores, [("top", 1.0), ("tail", 0.7)]);

        let absent = lock_top(
            vec![group("g1", 0.8)],
            hit_of("top", 1.0),
            "gX".to_string(),
            "alpha",
        );
        assert_eq!(absent[0].key, "gX");
        assert_eq!(absent[0].hits.len(), 1);
        assert_eq!(absent[0].hits[0].title, "top");
        assert_eq!(absent[1].key, "g1");
    }

    /// P1-56: the locked scope leads the footer once,
    /// and a federated scope leaves the also-matched list.
    #[test]
    fn test_p1_56_scope_footer_drops_a_federated_scope_from_also_matched() {
        let also = |scope: &str| AlsoMatched {
            scope: scope.to_string(),
            best_id: "x".to_string(),
        };
        let meta = merge_scopes(
            Some("b".to_string()),
            vec!["a".to_string(), "b".to_string()],
            vec![also("b"), also("c")],
            vec![also("a"), also("d")],
        );
        assert_eq!(meta.federated, ["b", "a"]);
        let left: Vec<&str> = meta.also_matched.iter().map(|m| m.scope.as_str()).collect();
        assert_eq!(left, ["c", "d"]);
    }

    /// P1-56: a scope at exactly a quarter of the best federates (`>=`), and a
    /// doc scoring exactly 0 drops (`<= 0`).
    #[test]
    fn test_p1_56_fuse_scopes_gate_is_inclusive_and_a_zero_score_drops() {
        let fused = fuse_scopes(vec![
            scoped("a 0", "a", 1.0),
            scoped("b 0", "b", 0.25),
            scoped("c 0", "c", 0.0),
            scoped("c 1", "c", 0.2),
        ]);
        assert_eq!(fused.federated, ["a", "b"]);
        assert_eq!(fused.also_matched.len(), 1);
        assert_eq!(fused.also_matched[0].scope, "c");
        assert_eq!(fused.also_matched[0].best_id, "c 1");
        // The 0 doc left the scope c before the gate: no `c 0` anywhere.
        assert!(fused.ranked.iter().all(|d| d.id != "c 0"));
        // Scope c holds only a zero doc: it drops, not gated.
        let only_zero = fuse_scopes(vec![scoped("a 0", "a", 1.0), scoped("z 0", "z", 0.0)]);
        assert_eq!(only_zero.federated, ["a"]);
        assert!(only_zero.also_matched.is_empty());
    }

    /// P1-56: a child whose ask panics is skipped, as
    /// a child whose ask throws is skipped.
    #[test]
    fn test_p1_56_a_panicking_child_is_skipped() {
        fn boom(
            _: &Graph,
            _: Option<&AskIndex>,
            _: &str,
            _: &AskOptions,
            _: &Path,
        ) -> Result<(AskResult, Option<Ranking>), AskError> {
            panic!("corrupt child");
        }
        let graphs = WorkspaceGraphs {
            loaded: vec![ChildGraph {
                child: "alpha".to_string(),
                root: PathBuf::from("alpha"),
                graph: graph_with_file("a.ts", 1),
            }],
            missing: Vec::new(),
        };
        let opts = FederateAskOptions {
            limit: 8,
            source: false,
            full: false,
            in_prefix: None,
        };
        let result = federate_ask_with(&graphs, "q", &opts, boom).expect("no error");
        assert_eq!(result.mode, AskMode::Empty);
        assert!(result.hits.is_empty());
    }

    /// P1-56: an unknown first `--in` segment is an
    /// error naming every child, sorted; an empty `--in` is no scope.
    #[test]
    fn test_p1_56_unknown_in_child_names_the_sorted_repos() {
        let graphs = WorkspaceGraphs {
            loaded: Vec::new(),
            missing: vec!["beta".to_string(), "alpha".to_string()],
        };
        let opts = |in_prefix: &str| FederateAskOptions {
            limit: 8,
            source: false,
            full: false,
            in_prefix: Some(in_prefix.to_string()),
        };
        let err = federate_ask(&graphs, "q", &opts("zzz/sub//")).expect_err("unknown child");
        assert_eq!(
            err.to_string(),
            "no workspace repo named zzz \u{2014} use one of: alpha, beta"
        );
        // A missing child passes the name check and gives the empty note.
        let result = federate_ask(&graphs, "q", &opts("alpha/")).expect("known child");
        assert_eq!(result.mode, AskMode::Empty);
        // An empty `--in` is no scope.
        assert!(federate_ask(&graphs, "q", &opts("")).is_ok());
    }
}
