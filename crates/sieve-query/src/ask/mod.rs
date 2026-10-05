//! `ask`: the lexical, structural and graph-rank query engine
//! (the `ask-ranking.md` note).
//!
//! This module covers the whole engine: single-scope and multi-scope
//! lexical ranking, the structural "who calls X" mode (section 6),
//! `--source` inlining (section 7.8), and the text and JSON renderers
//! (`format_ask`, `format_ask_json`).

mod concept;
mod format;
mod fuse;
mod lexical;
mod pagerank;
mod select;
/// `pub(crate)` so `callers.rs` can reuse `resolve_symbol`, the same
///  resolution `callers` itself needs
/// (the `query-commands.md` note section 2.4).
pub(crate) mod structural;

pub use format::{format_ask, format_ask_json};
pub use fuse::scope_of;
pub(crate) use select::round_robin_queues;

use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;
use serde::Serialize;
use sieve_core::{askindex::AskIndex, Graph, Kind, Node};
use thiserror::Error;

/// The most source lines `--source` inlines per hit before it truncates
/// (section 7.8).
pub const MAX_SPAN_LINES: usize = 80;

pub use lexical::query_terms;

/// The BM25 term-frequency saturation constant (section 3.3).
pub const K1: f64 = 1.2;
/// The BM25 length-normalization constant (section 3.3).
pub const B: f64 = 0.75;
/// The personalized PageRank restart probability (section 4.3).
pub const ALPHA: f64 = 0.25;
/// The fixed personalized PageRank iteration count (section 4.3).
pub const ITERS: u32 = 25;
/// The weight the graph score carries in a candidate's baseline
/// (section 5.2).
pub const GRAPH_WEIGHT: f64 = 0.5;
/// The minimum normalized PageRank score a zero-lexical node needs to
/// join the candidate set (section 5.2).
pub const RESCUE_FLOOR: f64 = 0.15;
/// The score multiplier a test-path candidate carries when the query does
/// not itself ask about tests (section 3.5).
pub const TEST_RANK_PENALTY: f64 = 0.35;
/// The scope-fusion participation gate: a scope must reach this fraction
/// of the best scope's top score to federate (section 8.5).
pub const PARTICIPATION_RATIO: f64 = 0.25;
/// The default hit count `ask` returns (section 5.9).
pub const DEFAULT_LIMIT: usize = 8;

/// The flags one `ask` call runs with.
#[derive(Debug, Clone)]
pub struct AskOptions {
    /// The most hits `ask` returns.
    pub limit: usize,
    /// Search only nodes under this path prefix, when set.
    pub in_prefix: Option<String>,
    /// Run the personalized PageRank pass. `false` matches
    /// `--no-graph-rank`.
    pub graph_rank: bool,
    /// Inline each hit's source lines (section 7.8). `false` matches no
    /// `--source` flag.
    pub source: bool,
    /// On Tier-1 (no crux), `--full` changes nothing: every hit already
    /// reads the whole span from disk. Kept so the flag stays valid.
    pub full: bool,
}

impl Default for AskOptions {
    fn default() -> Self {
        AskOptions {
            limit: DEFAULT_LIMIT,
            in_prefix: None,
            graph_rank: true,
            source: false,
            full: false,
        }
    }
}

/// One returned hit: a graph node, with its rendered fields and its
/// ranking score.
///
/// `id`, `path` and `span` are internal bookkeeping and never serialize;
/// the JSON key order (section 7.10) is `kind`, `title`, `pointer`,
/// `snippet`, `relation`, `score`, `scope`, `code`.
#[derive(Debug, Clone)]
pub struct AskHit {
    pub id: String,
    /// `"symbol"` for a lexical or empty hit; `"caller"`/`"callee"` for a
    /// structural hit (section 7.5, section 6.3).
    pub kind: String,
    pub title: String,
    pub pointer: String,
    pub snippet: String,
    /// A concept hit's `links[].to` roster, frontmatter order. Empty and
    /// unset on every other hit (the `deep-tier.md` note
    /// section 2.8).
    pub related: Vec<String>,
    /// The edge relation a structural hit walked. Unset on every other hit.
    pub relation: Option<String>,
    pub score: f64,
    pub scope: Option<String>,
    /// The hit's inlined source, set only under `--source` (section 7.8).
    pub code: Option<String>,
    /// Set on a federated hit whose `scope` key is new: the
    /// `qualifyHit` step appends it after `code`.
    pub scope_last: bool,
    pub path: String,
    pub span: Option<String>,
}

/// Writes the keys in `--json` order. A concept hit always carries `related`,
/// even when empty. A symbol or structural hit never does. `relation`, `scope`
/// and `code` show only when set.
impl Serialize for AskHit {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("kind", &self.kind)?;
        map.serialize_entry("title", &self.title)?;
        map.serialize_entry("pointer", &self.pointer)?;
        map.serialize_entry("snippet", &self.snippet)?;
        if self.kind == "concept" || !self.related.is_empty() {
            map.serialize_entry("related", &self.related)?;
        }
        if let Some(relation) = &self.relation {
            map.serialize_entry("relation", relation)?;
        }
        map.serialize_entry("score", &self.score)?;
        if let Some(scope) = self.scope.as_ref().filter(|_| !self.scope_last) {
            map.serialize_entry("scope", scope)?;
        }
        if let Some(code) = &self.code {
            map.serialize_entry("code", code)?;
        }
        if let Some(scope) = self.scope.as_ref().filter(|_| self.scope_last) {
            map.serialize_entry("scope", scope)?;
        }
        map.end()
    }
}

/// One scope gated out of a federated multi-scope result: it matched, but
/// too weakly to join the pack (section 8.5, `fuse.ts`'s `alsoMatched`).
#[derive(Debug, Clone, Serialize)]
pub struct AlsoMatched {
    /// The scope's raw path prefix (`""` = root).
    pub scope: String,
    /// The id of that scope's best-scoring doc.
    #[serde(rename = "bestId")]
    pub best_id: String,
}

/// The scope footer of a multi-scope `ask` run (section 8, `fuse.ts`'s
/// `FusionResult`). Matches the `scopeMeta` shape: raw scope
/// prefixes, not rendered labels — `scope_label` renders them at display
/// time (`format.rs`, `empty_note`).
#[derive(Debug, Clone, Default, Serialize)]
pub struct ScopeMeta {
    /// The scopes that joined the fused result, best-score first.
    pub federated: Vec<String>,
    /// The scopes gated out, in the order Sieve reports them.
    #[serde(rename = "alsoMatched")]
    pub also_matched: Vec<AlsoMatched>,
}

/// `"lexical"` when at least one hit scored, else `"empty"` (section 5.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AskMode {
    Lexical,
    Empty,
    /// A resolved structural query: a walk over caller or callee edges
    /// (section 6).
    Structural,
}

/// The "read it whole" baseline `ask --source` reports as `saved`
/// (`ask.ts`'s `baselineFor`): the file count and the summed chars.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AskSaved {
    /// The count of distinct files the hits point at.
    pub files: usize,
    /// The summed char count of those files, read whole.
    #[serde(rename = "baselineChars")]
    pub baseline_chars: u64,
}

/// The top-ranked item, the one `coverage` reads from (:
/// `matchedOf.get(top)`): a symbol node, or a concept doc.
pub(crate) enum Top<'a> {
    Symbol(&'a Node),
    Concept(&'a concept::ConceptDoc),
}

/// The full result of one `ask` run, ready for a renderer.
///
/// The field order is the `--json` key order (section 7.10, and `saved`
/// last, as `ask()` assigns it after the result exists).
#[derive(Debug, Clone, Serialize)]
pub struct AskResult {
    pub query: String,
    pub mode: AskMode,
    /// The resolved subject name of a structural result. Unset in every
    /// other mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    pub hits: Vec<AskHit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scopes: Option<ScopeMeta>,
    /// The top hit's matched idf share. `None` stands for Sieve's
    /// `undefined`: a structural result, no scored hit, or no query term.
    /// The JSON drops a `None` field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage: Option<f64>,
    /// The top hit's strong share, `None` on the same rule as `coverage`.
    #[serde(rename = "coverageStrong", skip_serializing_if = "Option::is_none")]
    pub coverage_strong: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ranking: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// The `--source` baseline; the CLI fills it after `ask()` returns
    /// and never in any other mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saved: Option<AskSaved>,
    /// The raw personalized PageRank map, for the `test_p3_05` parity
    /// assertion against the `pr(...)` values. Not part of the
    /// rendered output.
    #[serde(skip)]
    pub pagerank_for_test: Option<std::collections::HashMap<String, f64>>,
}

/// One group of a child's ranking metadata: a concept, or one file with
/// its queue of spans (`ranking.groups`).
#[derive(Debug, Clone)]
pub struct RankGroup {
    /// `file:<path>` or `concept:<slug>:<index>`.
    pub key: String,
    /// The group's queue; the first hit is the leader.
    pub hits: Vec<AskHit>,
    /// The group's hits at their baseline scores.
    pub baseline_hits: Vec<AskHit>,
    /// The idf share of every matched term (`unionCoverage`).
    pub coverage: f64,
    /// The idf share of the NAME-only matched terms (`unionStrongCoverage`).
    pub coverage_strong: f64,
}

/// The ranking metadata a workspace parent reads from one child
/// (`ranking`). Every list is cut to the child's limit.
#[derive(Debug, Clone)]
pub struct Ranking {
    /// The concept and file groups, best leader first.
    pub groups: Vec<RankGroup>,
    /// Every baseline hit with its group key, best first.
    pub baseline: Vec<(String, AskHit)>,
    /// The top baseline hit's matched idf share.
    pub baseline_coverage: f64,
    /// The top baseline hit's strong share.
    pub baseline_coverage_strong: f64,
}

/// ` — scopes here: a/ · b/ · (root)` over the rendered scope labels, in graph
/// order, or `""` with one scope or none (`scopesHereClause`). Shared by the
/// `--in` error and the zero-hit note.
fn scopes_here_clause(labels: &[String]) -> String {
    if labels.len() > 1 {
        format!(" — scopes here: {}", labels.join(" · "))
    } else {
        String::new()
    }
}

/// Renders the `PrefixNotIndexed` message, with the scope list only when
/// there is more than one scope to name. Matches `grep`'s wording
/// (`ask-ranking.md` section 8.8).
fn prefix_not_indexed_message(prefix: &str, scopes: &[String]) -> String {
    let clause = scopes_here_clause(scopes);
    format!("nothing indexed under \"{prefix}/\"{clause} (or any path prefix)")
}

/// Every way `ask` can fail.
#[derive(Debug, Error)]
pub enum AskError {
    /// The `--in` prefix matches no indexed node.
    #[error("{}", prefix_not_indexed_message(prefix, scopes))]
    PrefixNotIndexed { prefix: String, scopes: Vec<String> },
}

/// Renders a scope prefix the way the `scopeLabel` does.
pub(crate) fn scope_label(prefix: &str) -> String {
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

/// The word `ask` prints for a node's kind in a hit title (section 7.5).
pub(crate) fn kind_word(kind: Kind) -> &'static str {
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

fn title_of(node: &Node) -> String {
    format!("{} \u{b7} {}", node.name, kind_word(node.kind))
}

fn pointer_of(node: &Node) -> String {
    if node.kind == Kind::File {
        node.path.clone()
    } else {
        format!("{}:{}", node.path, node.span)
    }
}

fn span_of(node: &Node) -> Option<String> {
    if node.kind == Kind::File {
        None
    } else {
        Some(node.span.clone())
    }
}

/// The snippet: the first line of `summary`, trimmed, else `signature`,
/// else `""` (section 7.4).
fn snippet_of(node: &Node) -> String {
    if let Some(summary) = &node.summary {
        if let Some(first) = summary.lines().next() {
            let trimmed = first.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    node.signature.clone().unwrap_or_default()
}

/// Ports. A JavaScript regex without the `u` flag has an
/// ASCII `\b` and an ASCII case fold, so the port pins `(?i-u)`: `étests`
/// still matches, and a Kelvin sign never folds to
/// `k`.
fn wants_tests_re() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i-u)\b(tests?|specs?|coverage|assert(?:ion)?s?|fixtures?|mocks?)\b").ok()
    })
    .as_ref()
}

/// Ports. `(?i-u)` keeps the `i` fold ASCII, as a JavaScript
/// `/[a-z]+/i` without the `u` flag is. `(?u:[^/])` keeps the negated
/// class on whole chars, so the pattern never matches invalid UTF-8.
fn is_test_path_re() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i-u)(^|/)(tests?|__tests__|spec)/|(_test|\.test|\.spec)\.[a-z]+$|(^|/)(test_(?u:[^/])+|conftest)\.py$")
            .ok()
    })
    .as_ref()
}

/// Whether the query itself asks about tests (section 3.5).
pub(crate) fn wants_tests(query: &str) -> bool {
    wants_tests_re().is_some_and(|re| re.is_match(query))
}

/// Whether `path` looks like a test file (section 3.5).
pub(crate) fn is_test_path(path: &str) -> bool {
    is_test_path_re().is_some_and(|re| re.is_match(path))
}

/// A candidate's test-path score multiplier (section 3.5).
pub(crate) fn test_factor(query_wants_tests: bool, path: &str) -> f64 {
    if !query_wants_tests && is_test_path(path) {
        TEST_RANK_PENALTY
    } else {
        1.0
    }
}

/// Runs one `ask` query over `graph`, reading each hit's source under
/// `repo_root` when `opts.source` is set, using `index` when it is usable
/// (`ask-ranking.md` section 1.9).
pub fn ask(
    graph: &Graph,
    index: Option<&AskIndex>,
    query: &str,
    opts: &AskOptions,
    repo_root: &Path,
) -> Result<AskResult, AskError> {
    ask_inner(graph, index, query, opts, repo_root, false).map(|(result, _)| result)
}

/// Runs one `ask` query like [`ask`], and returns the ranking metadata a
/// workspace parent fuses. The metadata is `None` on a structural result. Every
/// ranking hit carries its inlined source under `opts.source`.
pub fn ask_ranked(
    graph: &Graph,
    index: Option<&AskIndex>,
    query: &str,
    opts: &AskOptions,
    repo_root: &Path,
) -> Result<(AskResult, Option<Ranking>), AskError> {
    ask_inner(graph, index, query, opts, repo_root, true)
}

fn ask_inner(
    graph: &Graph,
    index: Option<&AskIndex>,
    query: &str,
    opts: &AskOptions,
    repo_root: &Path,
    want_ranking: bool,
) -> Result<(AskResult, Option<Ranking>), AskError> {
    let prefix = match &opts.in_prefix {
        Some(raw) => {
            let normalized = normalize_path_prefix(raw);
            // An empty prefix (from a bare `/`) means "match everything",
            // the same as no `--in` at all — never an indexed check, never
            // an error, even on an empty graph (`assertPrefixIndexed`'s
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
                    return Err(AskError::PrefixNotIndexed {
                        prefix: normalized,
                        scopes,
                    });
                }
            }
            Some(normalized)
        }
        None => None,
    };

    // Structural mode runs first, before any lexical scoring. A genuine
    // structural result returns immediately; a fallthrough note carries
    // through to the lexical result below (section 6.4).
    let structural_outcome = structural::structural(query, graph, opts.limit, prefix.as_deref());
    if let structural::StructuralOutcome::Result {
        subject,
        hits,
        note,
    } = structural_outcome
    {
        let mut result = AskResult {
            query: query.to_string(),
            mode: AskMode::Structural,
            subject: Some(subject),
            hits,
            scopes: None,
            coverage: None,
            coverage_strong: None,
            ranking: None,
            note: Some(note),
            saved: None,
            pagerank_for_test: None,
        };
        if opts.source {
            inline_source(repo_root, graph, &mut result.hits, opts.full);
        }
        return Ok((result, None));
    }
    let fallthrough_note = match structural_outcome {
        structural::StructuralOutcome::Fallthrough(note) => Some(note),
        _ => None,
    };

    let terms = lexical::query_terms(query);
    let query_wants_tests = wants_tests(query);

    let all_docs = lexical::build_docs(graph, index);
    let active_nodes: Vec<&Node> = graph
        .nodes
        .iter()
        .filter(|n| match &prefix {
            Some(p) => path_under_prefix(&n.path, p),
            None => true,
        })
        .collect();

    // `loadCorpus` reads every top-level concept `.md` with no slug test
    // (the `deep-tier.md` note section 2.8).
    //
    // ponytail: the context dir name comes from the product table
    // (`context_dir_name`), so Sieve reads `sieve/`
    // under `repo_root`. A `--dir`-overridden context dir never
    // reaches this function today; add a `context_dir` parameter when a
    // fixture needs one.
    let context_dir = repo_root.join(sieve_core::product().context_dir_name());
    let mut concept_docs = concept::load_corpus(&context_dir);
    if let Some(p) = &prefix {
        concept_docs.retain(|d| d.sources.iter().any(|s| path_under_prefix(s, p)));
    }
    let concept_bags: Vec<std::collections::HashSet<String>> =
        concept_docs.iter().map(concept::idf_fold_terms).collect();

    let corpus = lexical::Corpus::build_with_concepts(&active_nodes, &all_docs, &concept_bags);
    let lex = lexical::score_all(&active_nodes, &all_docs, &corpus, &terms, query_wants_tests);

    // The multi-scope branch keys on the graph's declared scope count, not on
    // how many scopes the active node set happens to touch (section 8.1:
    // `scopesOfGraph(graph).length > 1`). It walks each scope's own subgraph
    // separately (`preparePageRankPartitions`), so it computes its own PageRank
    // per scope rather than reusing one graph-wide walk. Both branches merge
    // the concept hits.
    let concept_scored = concept::score_concepts(&concept_docs, &terms, &corpus);
    let concept_normalized = concept::normalize(&concept_scored);
    // A symbol with no doc bag reads `0` (`makeSymbolHit`);
    // a concept reads its name and body bags.
    let symbol_coverage = |node: &Node| match all_docs.get(node.id.as_str()) {
        Some(doc) => (
            lexical::matched_idf_share(&terms, &[&doc.name, &doc.path, &doc.body], &corpus),
            lexical::strong_share(node.kind, &doc.name, &terms, &corpus),
        ),
        None => (0.0, 0.0),
    };
    let item_coverage = |item: &select::QueueItem| match item {
        select::QueueItem::Symbol(node, _) => symbol_coverage(node),
        select::QueueItem::Concept(doc, _) => concept::coverage_of(doc, &terms, &corpus),
    };
    let mut ranking_meta: Option<Ranking> = None;
    let (hits, scope_meta, top, scored, pagerank_for_test) = if graph.meta.scopes.len() > 1 {
        let by_scope = fuse::group_by_scope(&active_nodes, &graph.meta.scopes);
        let multi = fuse::run_multi_scope(
            &by_scope,
            &all_docs,
            &corpus,
            &terms,
            query_wants_tests,
            &lex,
            graph,
            opts.graph_rank,
        );
        if want_ranking {
            ranking_meta = Some(select::build_ranking(
                &multi.groups,
                &concept_normalized,
                &multi.baseline_rows,
                opts.limit,
                &|path| multi.file_scope.get(path).cloned(),
                &item_coverage,
            ));
        }
        let (mut hits, top, scored) = select::select_hits_with_concepts(
            multi.groups,
            &concept_normalized,
            opts.limit,
            multi.fused.as_ref(),
        );
        for hit in hits.iter_mut().filter(|h| h.kind == "symbol") {
            hit.scope = multi.file_scope.get(&hit.path).cloned();
        }
        // Every scope walks its own subgraph (see above), so there is no single
        // graph-wide map left to expose here; no parity test pins one for the
        // multi-scope path. After the top lock, the top's scope moves to the
        // front of the footer.
        let mut scope_meta = multi.scope_meta;
        if let (Some(Top::Symbol(n)), Some(meta)) = (&top, scope_meta.as_mut()) {
            if let Some(top_scope) = multi.file_scope.get(&n.path) {
                let mut federated = vec![top_scope.clone()];
                for s in &meta.federated {
                    if !federated.contains(s) {
                        federated.push(s.clone());
                    }
                }
                meta.federated = federated;
                // Drop the top's scope from `alsoMatched` too.
                meta.also_matched.retain(|m| m.scope != *top_scope);
            }
        }
        (hits, scope_meta, top, scored, None)
    } else {
        let pr = if opts.graph_rank && !lex.is_empty() {
            let active_ids: std::collections::HashSet<&str> =
                active_nodes.iter().map(|n| n.id.as_str()).collect();
            let adjacency = pagerank::build_adjacency(graph, &active_ids);
            pagerank::run(&active_nodes, &lex, &adjacency)
        } else {
            std::collections::HashMap::new()
        };
        let candidates = select::build_candidates(
            &active_nodes,
            &lex,
            &pr,
            query_wants_tests,
            Some((&all_docs, &terms)),
        );
        let baseline_syms: Vec<(&Node, f64)> = if want_ranking {
            candidates.iter().map(|c| (c.node, c.baseline)).collect()
        } else {
            Vec::new()
        };
        let groups = select::rank_files_bounded(candidates, &terms, &corpus);
        if want_ranking {
            ranking_meta = Some(select::build_ranking(
                &groups,
                &concept_normalized,
                &baseline_syms,
                opts.limit,
                &|_| None,
                &item_coverage,
            ));
        }
        let (hits, top, scored) =
            select::select_hits_with_concepts(groups, &concept_normalized, opts.limit, None);
        (hits, None, top, scored, Some(pr))
    };

    // `coverage`/`coverageStrong` come from ONLY the top hit's matched share (:
    // `matchedOf.get(top)`, `matchedStrongOf.get(top)`), never an aggregate
    // over every active node. Both stay `undefined` with no `top` or no query
    // term; `None` is that `undefined` here, and the JSON drops it. A symbol
    // with no doc bag reads `0` (`makeSymbolHit`); a concept reads its name and
    // body bags.
    let (coverage, coverage_strong) = match top.filter(|_| !terms.is_empty()) {
        None => (None, None),
        Some(Top::Symbol(node)) => {
            let (broad, strong) = symbol_coverage(node);
            (Some(broad), Some(strong))
        }
        Some(Top::Concept(doc)) => {
            let (broad, strong) = concept::coverage_of(doc, &terms, &corpus);
            (Some(broad), Some(strong))
        }
    };

    // The mode reads the scored set, not the cut hit list
    // (: `scored.length ? "lexical": "empty"`), so `-n 0` on
    // a scoring query stays `lexical` with no hits.
    let mode = if scored {
        AskMode::Lexical
    } else {
        AskMode::Empty
    };
    let lexical_note = if mode == AskMode::Empty {
        Some(empty_note(graph))
    } else {
        None
    };
    // A structural-intent query that fell through still gets a prominent
    // note on the lexical result, joined ahead of any lexical note
    // (section 6.4).
    let note = match (fallthrough_note, lexical_note) {
        (Some(fallthrough), Some(lexical)) => Some(format!("{fallthrough}\n{lexical}")),
        (Some(fallthrough), None) => Some(fallthrough),
        (None, lexical) => lexical,
    };

    let mut hits = hits;
    if opts.source {
        inline_source(repo_root, graph, &mut hits, opts.full);
        if let Some(r) = ranking_meta.as_mut() {
            for g in r.groups.iter_mut() {
                inline_source(repo_root, graph, &mut g.hits, opts.full);
                inline_source(repo_root, graph, &mut g.baseline_hits, opts.full);
            }
            inline_source(
                repo_root,
                graph,
                r.baseline.iter_mut().map(|(_, hit)| hit),
                opts.full,
            );
        }
    }

    let result = AskResult {
        query: query.to_string(),
        mode,
        subject: None,
        hits,
        scopes: scope_meta,
        coverage,
        coverage_strong,
        ranking: None,
        note,
        saved: None,
        pagerank_for_test,
    };
    Ok((result, ranking_meta))
}

/// Parses a `path:L<from>-L<to>` pointer into its parts. Returns `None` for
/// a pointer with no span, such as a file node's bare path, or an
/// unresolved structural target (section 7.8).
fn parse_span_pointer(pointer: &str) -> Option<(&str, u32, u32)> {
    let (path, span) = pointer.rsplit_once(':')?;
    let rest = span.strip_prefix('L')?;
    let (from, to) = rest.split_once("-L")?;
    Some((path, from.parse().ok()?, to.parse().ok()?))
}

/// Reads the source lines `[from, to]` (1-indexed, inclusive) of `path`
/// under `repo_root`, clamped to the file length, and capped at
/// [`MAX_SPAN_LINES`] with a truncation marker (section 7.8).
fn slice_span(repo_root: &Path, path: &str, from: u32, to: u32) -> Option<String> {
    let bytes = std::fs::read(repo_root.join(path)).ok()?;
    // The span read goes through the source decoder.
    let source = sieve_core::fingerprint::decode_source(&bytes)?;
    let lines: Vec<&str> = source.split('\n').collect();
    let start = from.max(1) as usize;
    let end = (to as usize).min(lines.len());
    if start > end {
        return Some(String::new());
    }
    let slice = &lines[start - 1..end];
    if slice.len() > MAX_SPAN_LINES {
        let mut head: Vec<String> = slice[..MAX_SPAN_LINES]
            .iter()
            .map(|l| l.to_string())
            .collect();
        head.push(format!(
            "… (+{} more lines; open {path}:L{start}-L{end})",
            slice.len() - MAX_SPAN_LINES
        ));
        Some(head.join("\n"))
    } else {
        Some(slice.join("\n"))
    }
}

/// Attaches inlined source to every hit whose pointer is a real
/// `path:span`. Crux-first: a node with a graph-stored crux and `full` off
/// gets that short excerpt with an escalation marker; otherwise the whole
/// span is read from disk (section 7.8).
fn inline_source<'h>(
    repo_root: &Path,
    graph: &Graph,
    hits: impl IntoIterator<Item = &'h mut AskHit>,
    full: bool,
) {
    for hit in hits {
        let Some((path, from, to)) = parse_span_pointer(&hit.pointer) else {
            continue;
        };
        let span_str = sieve_core::span(from, to);
        if !full {
            let crux = graph
                .nodes
                .iter()
                .find(|n| n.path == path && n.span == span_str)
                .and_then(|n| n.crux.as_ref());
            if let Some(crux) = crux {
                hit.code = Some(format!(
                    "{}\n… (crux — full definition at {}; rerun with --full)",
                    crux.code, hit.pointer
                ));
                continue;
            }
        }
        // Sieve sets the code only when it is truthy: a span past the end
        // of the file reads as an empty string and gives no code.
        if let Some(code) = slice_span(repo_root, path, from, to).filter(|c| !c.is_empty()) {
            hit.code = Some(code);
        }
    }
}

/// The zero-hit note, with the `scopes here:` clause on a multi-scope
/// graph (`scopesHereClause(scopes)` over `graph.meta.scopes`
/// in stored order).
fn empty_note(graph: &Graph) -> String {
    let product = sieve_core::product();
    let labels: Vec<String> = graph
        .meta
        .scopes
        .iter()
        .map(|s| scope_label(&s.prefix))
        .collect();
    format!(
        "no matching nodes — try different words, or `{} build` if {}/ is empty{}",
        product.name,
        product.context_dir_name(),
        scopes_here_clause(&labels)
    )
}

pub(crate) fn to_hit(node: &Node, score: f64, scope: Option<String>) -> AskHit {
    AskHit {
        id: node.id.clone(),
        kind: "symbol".to_string(),
        title: title_of(node),
        pointer: pointer_of(node),
        snippet: snippet_of(node),
        related: Vec::new(),
        relation: None,
        score,
        scope,
        code: None,
        scope_last: false,
        path: node.path.clone(),
        span: span_of(node),
    }
}

#[cfg(test)]
mod regex_tests {
    use super::*;

    /// A JavaScript regex without the `u` flag folds case in ASCII only,
    /// so a Kelvin sign (U+212A) never matches `k`.
    #[test]
    fn wants_tests_and_is_test_path_fold_case_in_ascii_only() {
        assert!(wants_tests("MOCKS"));
        assert!(!wants_tests("moc\u{212A}s"));
        assert!(is_test_path("src/a.TEST.ts"));
        assert!(!is_test_path("src/a.test.\u{212A}"));
    }
}

#[cfg(test)]
mod source_tests {
    use super::*;

    fn tempdir(label: &str) -> crate::test_support::TempDir {
        crate::test_support::TempDir::new(label)
    }

    #[test]
    fn parse_span_pointer_reads_path_and_span_but_not_a_bare_path() {
        assert_eq!(
            parse_span_pointer("src/app.ts:L11-L13"),
            Some(("src/app.ts", 11, 13))
        );
        assert_eq!(parse_span_pointer("src/app.ts"), None);
    }

    #[test]
    fn slice_span_reads_the_clamped_inclusive_lines() {
        let dir = tempdir("slice-clamped");
        std::fs::write(dir.join("a.txt"), "one\ntwo\nthree\nfour\n").expect("write");
        let code = slice_span(&dir, "a.txt", 2, 3).expect("file reads");
        assert_eq!(code, "two\nthree");
        // `to` past the file end clamps to the last line. The file's
        // trailing newline is a real empty final line, matching
        // JavaScript's `source.split("\n")`.
        let code = slice_span(&dir, "a.txt", 3, 100).expect("file reads");
        assert_eq!(code, "three\nfour\n");
    }

    #[test]
    fn slice_span_caps_at_80_lines_with_a_truncation_marker() {
        let dir = tempdir("slice-cap");
        let text = (1..=100)
            .map(|n| format!("line{n}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(dir.join("big.txt"), &text).expect("write");
        let code = slice_span(&dir, "big.txt", 1, 100).expect("file reads");
        let lines: Vec<&str> = code.lines().collect();
        assert_eq!(lines.len(), MAX_SPAN_LINES + 1);
        assert_eq!(lines[0], "line1");
        assert_eq!(lines[MAX_SPAN_LINES - 1], "line80");
        assert_eq!(
            lines[MAX_SPAN_LINES],
            "… (+20 more lines; open big.txt:L1-L100)"
        );
    }

    fn node_with_crux(id: &str, path: &str, span: &str, crux: Option<sieve_core::Crux>) -> Node {
        Node {
            id: id.to_string(),
            name: "run".to_string(),
            kind: Kind::Function,
            owner: None,
            path: path.to_string(),
            span: span.to_string(),
            signature: None,
            exported: true,
            origin: sieve_core::Origin::Ast,
            body_hash: "0".repeat(64),
            chars: None,
            body_text: None,
            arity: None,
            variadic: None,
            summary_state: sieve_core::SummaryState::Stale,
            summary: None,
            crux,
        }
    }

    /// P3-14: a `stale` node still keeps its own `crux`, and `--source`
    /// still prints it verbatim — the crux is never re-sliced from the
    /// (now different) source (note section 2.9).
    #[test]
    fn inline_source_prints_a_stale_nodes_crux_verbatim() {
        let node = node_with_crux(
            "a.ts#run",
            "a.ts",
            "L1-L3",
            Some(sieve_core::Crux {
                code: "function run() {\n  return 1;\n}".to_string(),
                span: "L1-L3".to_string(),
            }),
        );
        let graph = Graph {
            meta: sieve_core::Meta {
                version: 1,
                node_count: 1,
                edge_count: 0,
                languages: Vec::new(),
                scopes: Vec::new(),
            },
            nodes: vec![node],
            edges: Vec::new(),
        };
        let mut hits = vec![AskHit {
            id: "a.ts#run".to_string(),
            kind: "symbol".to_string(),
            title: "run \u{b7} function".to_string(),
            pointer: "a.ts:L1-L3".to_string(),
            snippet: String::new(),
            related: Vec::new(),
            relation: None,
            score: 1.0,
            scope: None,
            code: None,
            scope_last: false,
            path: "a.ts".to_string(),
            span: Some("L1-L3".to_string()),
        }];
        // The file on disk no longer matches the stored crux; the crux
        // still wins, and the file is never read.
        inline_source(&std::env::temp_dir(), &graph, &mut hits, false);
        assert_eq!(
            hits[0].code.as_deref(),
            Some(
                "function run() {\n  return 1;\n}\n\u{2026} (crux \u{2014} full definition at a.ts:L1-L3; rerun with --full)"
            )
        );
    }

    /// `--full` skips the crux map entirely, even for a node that carries
    /// one, and reads the span from disk instead (note section 2.9).
    #[test]
    fn inline_source_full_skips_the_crux_map() {
        let dir = tempdir("inline-full");
        std::fs::write(dir.join("a.ts"), "one\ntwo\nthree\n").expect("write");
        let node = node_with_crux(
            "a.ts#run",
            "a.ts",
            "L1-L2",
            Some(sieve_core::Crux {
                code: "stale crux text".to_string(),
                span: "L1-L2".to_string(),
            }),
        );
        let graph = Graph {
            meta: sieve_core::Meta {
                version: 1,
                node_count: 1,
                edge_count: 0,
                languages: Vec::new(),
                scopes: Vec::new(),
            },
            nodes: vec![node],
            edges: Vec::new(),
        };
        let mut hits = vec![AskHit {
            id: "a.ts#run".to_string(),
            kind: "symbol".to_string(),
            title: "run \u{b7} function".to_string(),
            pointer: "a.ts:L1-L2".to_string(),
            snippet: String::new(),
            related: Vec::new(),
            relation: None,
            score: 1.0,
            scope: None,
            code: None,
            scope_last: false,
            path: "a.ts".to_string(),
            span: Some("L1-L2".to_string()),
        }];
        inline_source(&dir, &graph, &mut hits, true);
        assert_eq!(hits[0].code.as_deref(), Some("one\ntwo"));
    }

    #[test]
    fn inline_source_skips_a_hit_with_no_span() {
        let graph = Graph {
            meta: sieve_core::Meta {
                version: 1,
                node_count: 0,
                edge_count: 0,
                languages: Vec::new(),
                scopes: Vec::new(),
            },
            nodes: Vec::new(),
            edges: Vec::new(),
        };
        let mut hits = vec![AskHit {
            id: "a.ts".to_string(),
            kind: "symbol".to_string(),
            title: "a.ts \u{b7} file".to_string(),
            pointer: "a.ts".to_string(),
            snippet: String::new(),
            related: Vec::new(),
            relation: None,
            score: 1.0,
            scope: None,
            code: None,
            scope_last: false,
            path: "a.ts".to_string(),
            span: None,
        }];
        inline_source(&std::env::temp_dir(), &graph, &mut hits, false);
        assert!(hits[0].code.is_none());
    }
}

#[cfg(test)]
mod scope_prefix_tests {
    use super::*;
    use sieve_core::{Meta, Origin, SummaryState};

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

    fn graph(nodes: Vec<Node>) -> Graph {
        Graph {
            meta: Meta {
                version: 1,
                node_count: nodes.len(),
                edge_count: 0,
                languages: Vec::new(),
                scopes: Vec::new(),
            },
            nodes,
            edges: Vec::new(),
        }
    }

    #[test]
    fn test_p1_41_normalize_path_prefix_matches_golden() {
        // Pinned from recorded runs of the path prefix rule.
        assert_eq!(normalize_path_prefix("./src/app/"), "src/app");
        assert_eq!(normalize_path_prefix("src/ap"), "src/ap");
        assert_eq!(normalize_path_prefix("/"), "");
        assert_eq!(normalize_path_prefix(""), "");
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
    fn test_p1_42_in_on_a_bare_slash_never_errors_on_an_empty_graph() {
        let g = graph(Vec::new());
        let opts = AskOptions {
            in_prefix: Some("/".to_string()),
            ..AskOptions::default()
        };
        // A bare "/" normalizes to "" ("match everything"); it never
        // triggers the not-indexed check, even with zero nodes.
        let result = ask(&g, None, "run", &opts, &std::env::temp_dir());
        assert!(result.is_ok());
    }

    #[test]
    fn test_p1_42_in_filters_active_nodes_before_scoring() {
        // Two nodes named "run": one under "src/app", one under "lib". The
        // "lib" one carries a query-matching summary that would outscore
        // the "src/app" one on lexical terms alone — `--in src/app` must
        // still never surface it (`--in` filters BEFORE scoring).
        let mut outside = node(
            "lib/run.ts#run",
            "run",
            Kind::Function,
            "lib/run.ts",
            "L1-L2",
        );
        outside.summary = Some("run run run run run run".to_string());
        let inside = node(
            "src/app/run.ts#run",
            "run",
            Kind::Function,
            "src/app/run.ts",
            "L1-L2",
        );
        let g = graph(vec![outside, inside]);
        let opts = AskOptions {
            in_prefix: Some("src/app".to_string()),
            graph_rank: false,
            ..AskOptions::default()
        };
        let result = ask(&g, None, "run", &opts, &std::env::temp_dir()).expect("ask runs");
        assert!(
            result.hits.iter().all(|h| h.path.starts_with("src/app")),
            "a hit outside --in src/app leaked through: {:?}",
            result.hits.iter().map(|h| &h.path).collect::<Vec<_>>()
        );
    }

    /// P1-05 (DV6): the zero-hit note names every scope, in stored order,
    /// root as `(root)`; a one-scope graph gets no clause.
    #[test]
    fn test_p1_05_empty_note_lists_the_scopes_in_graph_order() {
        let mut g = graph(Vec::new());
        assert!(!empty_note(&g).contains("scopes here"));
        g.meta.scopes = ["pkg/b", "pkg/a", ""]
            .iter()
            .map(|p| sieve_core::wiring::Scope {
                prefix: p.to_string(),
                label: p.to_string(),
                markers: Vec::new(),
            })
            .collect();
        assert!(empty_note(&g).ends_with(" — scopes here: pkg/b/ · pkg/a/ · (root)"));
    }

    /// Two scopes, one `render` function each. The names tie the lexical
    /// score, so the fused order and the top lock fall to the title.
    fn two_scope_graph() -> Graph {
        let mut g = graph(vec![
            node(
                "packages/alpha/a.ts#renderZed",
                "renderZed",
                Kind::Function,
                "packages/alpha/a.ts",
                "L1-L2",
            ),
            node(
                "packages/beta/b.ts#renderAardvark",
                "renderAardvark",
                Kind::Function,
                "packages/beta/b.ts",
                "L1-L2",
            ),
        ]);
        g.meta.scopes = ["packages/alpha", "packages/beta"]
            .iter()
            .map(|p| sieve_core::wiring::Scope {
                prefix: p.to_string(),
                label: p.to_string(),
                markers: Vec::new(),
            })
            .collect();
        g
    }

    /// P3-08: the multi-scope branch merges the concept hits, and the top lock
    /// compares the concept with the best candidate's fused score. With graph
    /// rank on, the raw baseline of the best candidate is 1.5, but its fused
    /// score is 1.0. The concept ties at 1.0 and wins on title, so it comes
    /// first. The symbol groups follow by score, then title, and keep their
    /// `[scope/]` label.
    #[test]
    fn test_p3_08_multi_scope_ask_merges_concept_hits() {
        let g = two_scope_graph();
        let root = crate::test_support::TempDir::new("p3-08");
        let context = root.join(sieve_core::product().context_dir_name());
        std::fs::create_dir_all(&context).expect("create the context dir");
        std::fs::write(
            context.join("render-pipeline.md"),
            "---\nname: Render Pipeline\nslug: render-pipeline\ntype: system\nsources:\n  - path: packages/beta/b.ts\n    hash: abc\n---\n## Summary\n\nThe render pipeline draws every render layout.\n",
        )
        .expect("write the concept doc");

        let result = ask(&g, None, "render", &AskOptions::default(), &root).expect("ask runs");

        let hits: Vec<(&str, Option<&str>)> = result
            .hits
            .iter()
            .map(|h| (h.title.as_str(), h.scope.as_deref()))
            .collect();
        assert_eq!(
            hits,
            [
                ("Render Pipeline", None),
                ("renderAardvark \u{b7} function", Some("packages/beta")),
                ("renderZed \u{b7} function", Some("packages/alpha")),
            ]
        );
    }

    /// P3-08: with `--in` on a two-scope graph only one scope has active
    /// nodes, and the hit keeps that scope label.
    #[test]
    fn test_p3_08_in_prefix_on_a_two_scope_graph_keeps_the_scope_label() {
        let g = two_scope_graph();
        let root = crate::test_support::TempDir::new("p3-08-in");
        let opts = AskOptions {
            in_prefix: Some("packages/alpha".to_string()),
            ..AskOptions::default()
        };

        let result = ask(&g, None, "render", &opts, &root).expect("ask runs");

        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].scope.as_deref(), Some("packages/alpha"));
    }

    /// P3-08: after the top lock, the footer puts the
    /// top hit's scope first. The top is `renderAardvark` (title wins the
    /// tie), which lives in `packages/beta`, although `packages/alpha`
    /// sorts first by name.
    #[test]
    fn test_p3_08_multi_scope_footer_puts_the_top_scope_first() {
        let g = two_scope_graph();
        let root = crate::test_support::TempDir::new("p3-08-footer");

        let result = ask(&g, None, "render", &AskOptions::default(), &root).expect("ask runs");

        assert_eq!(result.hits[0].title, "renderAardvark \u{b7} function");
        let meta = result.scopes.expect("two scopes federated");
        assert_eq!(meta.federated, ["packages/beta", "packages/alpha"]);
    }

    /// P1-02 (DV3): `-n 0` on a scoring query keeps the `lexical` mode
    /// with no hits; the mode reads the scored set, not the cut list.
    #[test]
    fn test_p1_02_limit_zero_keeps_the_lexical_mode() {
        let g = graph(vec![node(
            "a.ts#run",
            "run",
            Kind::Function,
            "a.ts",
            "L1-L2",
        )]);
        let opts = AskOptions {
            limit: 0,
            graph_rank: false,
            ..AskOptions::default()
        };
        let result = ask(&g, None, "run", &opts, &std::env::temp_dir()).expect("ask runs");
        assert!(result.hits.is_empty());
        assert_eq!(result.mode, AskMode::Lexical);
        assert!(result.note.is_none());
    }

    #[test]
    fn test_p1_44_scope_label_renders_root_and_prefix() {
        assert_eq!(scope_label(""), "(root)");
        assert_eq!(scope_label("apps/api"), "apps/api/");
    }
}
