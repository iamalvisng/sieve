//! `ask`: the lexical, structural and graph-rank query engine
//!.
//!
//! This module covers the whole engine: single-scope and multi-scope
//! lexical ranking, the structural "who calls X" mode,
//! `--source` inlining, and the text and JSON renderers
//! (`format_ask`, `format_ask_json`).

mod concept;
mod def;
mod format;
mod fuse;
mod lexical;
mod pagerank;
mod saving;
mod select;
/// `pub(crate)` so `callers.rs` can reuse `resolve_symbol`, the same
///  resolution `callers` itself needs
///.
pub(crate) mod structural;

pub use def::{define, format_def, single_ident, Def, Ident};
pub use format::{format_ask, format_ask_json};
pub use fuse::scope_of;
pub use saving::crux_baseline_chars;
pub(crate) use select::round_robin_queues;

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};

use regex::Regex;
use serde::Serialize;
use sieve_core::{
    askindex::{AskDoc, AskIndex},
    Graph, Kind, Node,
};
use thiserror::Error;

/// The most source lines `--source` inlines per hit before it truncates
///.
pub const MAX_SPAN_LINES: usize = 80;

pub use lexical::query_terms;

/// The BM25 term-frequency saturation constant.
pub const K1: f64 = 1.2;
/// The BM25 length-normalization constant.
pub const B: f64 = 0.75;
/// The personalized PageRank restart probability.
pub const ALPHA: f64 = 0.25;
/// The fixed personalized PageRank iteration count.
pub const ITERS: u32 = 25;
/// The weight the graph score carries in a candidate's baseline
///.
pub const GRAPH_WEIGHT: f64 = 0.5;
/// The minimum normalized PageRank score a zero-lexical node needs to
/// join the candidate set.
pub const RESCUE_FLOOR: f64 = 0.15;
/// The score multiplier a test-path candidate carries when the query does
/// not itself ask about tests.
pub const TEST_RANK_PENALTY: f64 = 0.35;
/// The scope-fusion participation gate: a scope must reach this fraction
/// of the best scope's top score to federate.
pub const PARTICIPATION_RATIO: f64 = 0.25;
/// The default hit count `ask` returns.
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
    /// Inline each hit's source lines. `false` matches no
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
/// the JSON key order is `kind`, `title`, `pointer`,
/// `snippet`, `relation`, `score`, `scope`, `code`.
#[derive(Debug, Clone)]
pub struct AskHit {
    pub id: String,
    /// `"symbol"` for a lexical or empty hit; `"caller"`/`"callee"` for a
    /// structural hit.
    pub kind: String,
    pub title: String,
    pub pointer: String,
    pub snippet: String,
    /// A concept hit's `links[].to` roster, frontmatter order. Empty and
    /// unset on every other hit.
    pub related: Vec<String>,
    /// The edge relation a structural hit walked. Unset on every other hit.
    pub relation: Option<String>,
    pub score: f64,
    pub scope: Option<String>,
    /// The hit's inlined source, set only under `--source`.
    pub code: Option<String>,
    /// True when `code` holds less than the hit's span. Never serialized.
    pub cut: bool,
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
/// too weakly to join the pack (`alsoMatched`).
#[derive(Debug, Clone, Serialize)]
pub struct AlsoMatched {
    /// The scope's raw path prefix (`""` = root).
    pub scope: String,
    /// The id of that scope's best-scoring doc.
    #[serde(rename = "bestId")]
    pub best_id: String,
}

/// The scope footer of a multi-scope `ask` run. It matches the `scopeMeta` shape: raw scope
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

/// `"lexical"` when at least one hit scored, else `"empty"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AskMode {
    Lexical,
    Empty,
    /// A resolved structural query: a walk over caller or callee edges
    ///.
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
/// The field order is the `--json` key order, with `saved`
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
///.
fn prefix_not_indexed_message(prefix: &str, scopes: &[String]) -> String {
    let clause = if scopes.len() > 1 {
        format!(" (scopes here: {})", scopes.join(" \u{b7} "))
    } else {
        String::new()
    };
    format!(
        "nothing is indexed under {prefix}/{clause} \u{2014} try {} map",
        sieve_core::product().name
    )
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

/// The word `ask` prints for a node's kind in a hit title.
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
/// else `""`.
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
        Regex::new(r"(?i-u)(^|/)(tests?|__tests__|spec)/|(_test|\.test|\.spec)\.[a-z]+$|_tests\.rs$|(^|/)tests?\.rs$|(^|/)(test_(?u:[^/])+|conftest)\.py$")
            .ok()
    })
    .as_ref()
}

/// Whether the query itself asks about tests.
pub(crate) fn wants_tests(query: &str) -> bool {
    wants_tests_re().is_some_and(|re| re.is_match(query))
}

/// The query's lowercase words, split on every non-alphanumeric character.
pub(crate) fn split_words(query: &str) -> Vec<String> {
    query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Whether the query names the file at `path`. The file stem, split on `-`,
/// `_` and `.`, gives words. Each word of 3 or more letters must be a whole
/// query word. A stem with no such word is never named.
pub(crate) fn query_names_file(words: &[String], path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    let mut long = stem
        .split(['-', '_', '.'])
        .filter(|w| w.chars().filter(|c| c.is_alphabetic()).count() >= 3)
        .map(str::to_lowercase)
        .peekable();
    long.peek().is_some() && long.all(|w| words.contains(&w))
}

/// Whether `path` has a `scripts/` folder segment.
pub(crate) fn is_script_path(path: &str) -> bool {
    path.starts_with("scripts/") || path.contains("/scripts/")
}

/// Whether `path` looks like a test file.
pub(crate) fn is_test_path(path: &str) -> bool {
    is_test_path_re().is_some_and(|re| re.is_match(path))
}

/// Scripts rank like tests: a tool script is not the code a query looks for.
/// A script the query names, or the word "script", takes no penalty.
pub(crate) fn test_factor(query_wants_tests: bool, query_words: &[String], path: &str) -> f64 {
    if (!query_wants_tests && is_test_path(path))
        || (is_script_path(path)
            && !query_words.iter().any(|w| w == "script" || w == "scripts")
            && !query_names_file(query_words, path))
    {
        TEST_RANK_PENALTY
    } else {
        1.0
    }
}

/// Whether a Python class name marks a test class: `Test`, `Test` plus an
/// uppercase letter or digit, or a `Test` or `Tests` suffix.
pub(crate) fn is_test_class_name(name: &str) -> bool {
    name.strip_prefix("Test").is_some_and(|rest| {
        rest.chars()
            .next()
            .is_none_or(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    }) || name.ends_with("Test")
        || name.ends_with("Tests")
}

/// Whether a Rust module name marks a test module.
pub(crate) fn is_test_module_name(name: &str) -> bool {
    matches!(name, "tests" | "test") || name.ends_with("_tests")
}

/// Runs one `ask` query over `graph`, reading each hit's source under
/// `repo_root` when `opts.source` is set, using `index` when it is usable
///.
pub fn ask(
    graph: &Graph,
    index: Option<&AskIndex>,
    query: &str,
    opts: &AskOptions,
    repo_root: &Path,
) -> Result<AskResult, AskError> {
    ask_inner(graph, index, query, opts, repo_root, false, None).map(|(result, _)| result)
}

/// What a warm caller knows about the state behind one query. The cache keeps
/// the corpus and the PageRank shape only while the state stays the same.
pub struct WarmKey<'a> {
    /// The graph this call reads. A new graph is a new `Arc`, so a refresh,
    /// a reload or a heal drops the cache.
    pub graph: &'a Arc<Graph>,
    /// Reads the modification time (ns) and size of the ask index file, or
    /// `None` when the file is missing. A rewrite of the file changes it. The
    /// cache reads it again after `load_index`, because that call may write
    /// the file.
    pub index_stamp: &'a dyn Fn() -> Option<(u128, u64)>,
    /// Reads and parses the ask index. It runs only on a cache miss that
    /// `steps` cannot patch.
    pub load_index: &'a dyn Fn() -> Option<AskIndex>,
    /// The file changes between the graphs the caller held, oldest first. A
    /// cache built for an earlier graph of this chain is patched in place of
    /// a rebuild from the ask index file.
    pub steps: &'a [Arc<DocStep>],
}

/// The token bags a patch changed, between two graphs of a warm caller.
pub struct DocStep {
    from: Weak<Graph>,
    to: Weak<Graph>,
    /// Each changed file with the bags of its nodes in `to`. A deleted file
    /// has none.
    files: Vec<(String, Vec<AskDoc>)>,
}

impl DocStep {
    /// A step from `from` to `to`. `files` holds each changed file with the
    /// bags of all its nodes in `to`, as `sieve_core::askindex::doc_of` builds
    /// them from the node and its body text.
    pub fn new(from: &Arc<Graph>, to: &Arc<Graph>, files: Vec<(String, Vec<AskDoc>)>) -> Self {
        DocStep {
            from: Arc::downgrade(from),
            to: Arc::downgrade(to),
            files,
        }
    }

    /// True when this step ends at `graph`.
    pub fn ends_at(&self, graph: &Arc<Graph>) -> bool {
        Weak::ptr_eq(&self.to, &Arc::downgrade(graph))
    }

    fn starts_at(&self, graph: &Arc<Graph>) -> bool {
        Weak::ptr_eq(&self.from, &Arc::downgrade(graph))
    }
}

/// Runs one `ask` query like [`ask`] for a caller that keeps its graph between
/// calls. The corpus and the PageRank shape are built once per graph.
pub fn ask_warm(
    key: &WarmKey,
    query: &str,
    opts: &AskOptions,
    repo_root: &Path,
) -> Result<AskResult, AskError> {
    ask_inner(key.graph, None, query, opts, repo_root, false, Some(key)).map(|(result, _)| result)
}

/// Everything about a query that depends on the graph and not on the query
/// words: the token bags, the corpus and the PageRank shape of each node set.
pub(crate) struct Prepared {
    pub(crate) docs: lexical::Docs,
    pub(crate) corpus: lexical::Corpus,
    /// True when `docs` came from the ask index. Only such docs hold the body
    /// text, so only they take a patch.
    indexed: bool,
    topologies: Mutex<HashMap<String, Arc<pagerank::Topology>>>,
}

/// Where the bags of a cached `Prepared` came from.
enum Source {
    /// The ask index file, with the stamp it had when it was read.
    Disk(Option<(u128, u64)>),
    /// A patch. The file may change under it, because the caller writes the
    /// same bags there, and any other writer also changes the graph.
    Patched,
}

/// The one cached `Prepared`, with the state it was built from.
struct CacheEntry {
    graph: Arc<Graph>,
    source: Source,
    prefix: Option<String>,
    concept_bags: Vec<HashSet<String>>,
    prep: Arc<Prepared>,
}

static PREPARED: Mutex<Option<CacheEntry>> = Mutex::new(None);

impl Prepared {
    fn build(
        graph: &Graph,
        index: Option<&AskIndex>,
        active_nodes: &[&Node],
        concept_bags: &[HashSet<String>],
    ) -> Prepared {
        let (mut docs, indexed) = lexical::build_docs(graph, index);
        lexical::mark_inline_tests(&graph.nodes, &mut docs);
        Self::with_docs(docs, indexed, active_nodes, concept_bags)
    }

    /// The corpus over `docs`. The PageRank shapes start empty, because they
    /// depend on the whole graph.
    fn with_docs(
        docs: lexical::Docs,
        indexed: bool,
        active_nodes: &[&Node],
        concept_bags: &[HashSet<String>],
    ) -> Prepared {
        let corpus = lexical::Corpus::build_with_concepts(active_nodes, &docs, concept_bags);
        Prepared {
            docs,
            corpus,
            indexed,
            topologies: Mutex::new(HashMap::new()),
        }
    }

    /// Takes the cached value for an earlier graph of the `steps` chain and
    /// patches the bags of the changed files, so the bags of every other file
    /// stay. Gives `None`, and leaves the cache as it was, when the chain does
    /// not reach from the cached graph to the graph of `key`, or when the
    /// cached value has another prefix or other concept bags.
    fn patched(
        cache: &mut Option<CacheEntry>,
        key: &WarmKey,
        prefix: &Option<String>,
        active_nodes: &[&Node],
        concept_bags: &[HashSet<String>],
    ) -> Option<Arc<Prepared>> {
        let e = cache.as_mut()?;
        if !e.prep.indexed || e.prefix != *prefix || e.concept_bags != concept_bags {
            return None;
        }
        let from = key.steps.iter().position(|s| s.starts_at(&e.graph))?;
        let steps = &key.steps[from..];
        let chained = steps.windows(2).all(|w| w[0].to.ptr_eq(&w[1].from));
        if !chained || !steps.last()?.ends_at(key.graph) {
            return None;
        }
        // A later step replaces the bags of an earlier one for the same file.
        let changed: HashMap<&str, &[AskDoc]> = steps
            .iter()
            .flat_map(|s| s.files.iter())
            .map(|(path, docs)| (path.as_str(), docs.as_slice()))
            .collect();
        // A reader that still holds the value keeps it, so only a sole owner
        // patches it. A failed check leaves the cache as it was.
        let mut docs = std::mem::take(&mut Arc::get_mut(&mut e.prep)?.docs);
        lexical::patch_docs(&mut docs, &e.graph, key.graph, &changed);
        let prep = Arc::new(Self::with_docs(docs, true, active_nodes, concept_bags));
        e.graph = Arc::clone(key.graph);
        e.source = Source::Patched;
        e.prep = Arc::clone(&prep);
        Some(prep)
    }

    /// Returns the cached value when `warm` names the same graph, ask index,
    /// prefix and concept bags as the cached one. Else it patches the cached
    /// value from `warm.steps`, or builds a new value from the ask index. A
    /// call with no `warm` key never reads or writes the cache.
    fn get(
        graph: &Graph,
        index: Option<&AskIndex>,
        warm: Option<&WarmKey>,
        prefix: &Option<String>,
        active_nodes: &[&Node],
        concept_bags: Vec<HashSet<String>>,
    ) -> Arc<Prepared> {
        let Some(key) = warm else {
            return Arc::new(Self::build(graph, index, active_nodes, &concept_bags));
        };
        let mut cache = PREPARED.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(e) = cache.as_ref() {
            let same_source = match e.source {
                Source::Disk(stamp) => stamp == (key.index_stamp)(),
                Source::Patched => true,
            };
            if Arc::ptr_eq(&e.graph, key.graph)
                && same_source
                && e.prefix == *prefix
                && e.concept_bags == concept_bags
            {
                return Arc::clone(&e.prep);
            }
        }
        if let Some(prep) = Self::patched(&mut cache, key, prefix, active_nodes, &concept_bags) {
            return prep;
        }
        let loaded = (key.load_index)();
        let stamp = (key.index_stamp)();
        let prep = Arc::new(Self::build(
            graph,
            loaded.as_ref(),
            active_nodes,
            &concept_bags,
        ));
        *cache = Some(CacheEntry {
            graph: Arc::clone(key.graph),
            source: Source::Disk(stamp),
            prefix: prefix.clone(),
            concept_bags,
            prep: Arc::clone(&prep),
        });
        prep
    }

    /// The PageRank shape of `nodes`, built once per `scope` name.
    pub(crate) fn topology(
        &self,
        scope: &str,
        graph: &Graph,
        nodes: &[&Node],
    ) -> Arc<pagerank::Topology> {
        let mut topologies = self
            .topologies
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(t) = topologies.get(scope) {
            return Arc::clone(t);
        }
        let topology = Arc::new(pagerank::Topology::from_graph(nodes, graph));
        topologies.insert(scope.to_string(), Arc::clone(&topology));
        topology
    }
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
    ask_inner(graph, index, query, opts, repo_root, true, None)
}

fn ask_inner(
    graph: &Graph,
    index: Option<&AskIndex>,
    query: &str,
    opts: &AskOptions,
    repo_root: &Path,
    want_ranking: bool,
    warm: Option<&WarmKey>,
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
    // through to the lexical result below.
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
    let query_words = split_words(query);

    let active_nodes: Vec<&Node> = graph
        .nodes
        .iter()
        .filter(|n| match &prefix {
            Some(p) => path_under_prefix(&n.path, p),
            None => true,
        })
        .collect();

    // `loadCorpus` reads every top-level concept `.md` with no slug test
    //.
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

    let prep = Prepared::get(graph, index, warm, &prefix, &active_nodes, concept_bags);
    let (all_docs, corpus) = (&prep.docs, &prep.corpus);
    let lex = lexical::score_all(
        &active_nodes,
        all_docs,
        corpus,
        &terms,
        query_wants_tests,
        &query_words,
    );

    // The multi-scope branch keys on the graph's declared scope count, not on
    // how many scopes the active node set happens to touch
    // (`scopesOfGraph(graph).length > 1`). It walks each scope's own subgraph
    // separately (`preparePageRankPartitions`), so it computes its own PageRank
    // per scope rather than reusing one graph-wide walk. Both branches merge
    // the concept hits.
    let concept_scored = concept::score_concepts(&concept_docs, &terms, corpus);
    let concept_normalized = concept::normalize(&concept_scored);
    // A symbol with no doc bag reads `0` (`makeSymbolHit`);
    // a concept reads its name and body bags.
    let symbol_coverage = |node: &Node| match all_docs.get(node.id.as_str()) {
        Some(doc) => (
            lexical::matched_idf_share(&terms, &[&doc.name, &doc.path, &doc.body], corpus),
            lexical::strong_share(node.kind, &doc.name, &terms, corpus),
        ),
        None => (0.0, 0.0),
    };
    let item_coverage = |item: &select::QueueItem| match item {
        select::QueueItem::Symbol(node, _) => symbol_coverage(node),
        select::QueueItem::Concept(doc, _) => concept::coverage_of(doc, &terms, corpus),
    };
    let mut ranking_meta: Option<Ranking> = None;
    let (hits, scope_meta, top, scored, pagerank_for_test) = if graph.meta.scopes.len() > 1 {
        let by_scope = fuse::group_by_scope(&active_nodes, &graph.meta.scopes);
        let multi = fuse::run_multi_scope(
            &by_scope,
            &prep,
            &terms,
            query_wants_tests,
            &query_words,
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
            prep.topology("", graph, &active_nodes).run(&lex)
        } else {
            std::collections::HashMap::new()
        };
        let candidates = select::build_candidates(
            &active_nodes,
            &lex,
            &pr,
            query_wants_tests,
            &query_words,
            Some((all_docs, &terms)),
        );
        let baseline_syms: Vec<(&Node, f64)> = if want_ranking {
            candidates.iter().map(|c| (c.node, c.baseline)).collect()
        } else {
            Vec::new()
        };
        let groups = select::rank_files_bounded(candidates, &terms, corpus);
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
            let (broad, strong) = concept::coverage_of(doc, &terms, corpus);
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
    //.
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
/// unresolved structural target.
fn parse_span_pointer(pointer: &str) -> Option<(&str, u32, u32)> {
    let (path, span) = pointer.rsplit_once(':')?;
    let rest = span.strip_prefix('L')?;
    let (from, to) = rest.split_once("-L")?;
    Some((path, from.parse().ok()?, to.parse().ok()?))
}

/// Reads the source lines `[from, to]` (1-indexed, inclusive) of `path`
/// under `repo_root`, clamped to the file length, and capped at
/// [`MAX_SPAN_LINES`] with a truncation marker.
///
/// The flag is true when the text holds less than the span.
fn slice_span(repo_root: &Path, path: &str, from: u32, to: u32) -> Option<(String, bool)> {
    slice_span_marked(repo_root, path, from, to, MAX_SPAN_LINES, None)
}

/// Like [`slice_span`], with a line cap and an optional cap in UTF-16 units.
/// The char cap counts each line and its line break. The cut falls on a line
/// end, but the first line always prints. With no char cap the
/// marker names the whole slice, as `slice_span` always did. With a char cap
/// the marker names the span from the first line not printed.
pub(crate) fn slice_span_capped(
    repo_root: &Path,
    path: &str,
    from: u32,
    to: u32,
    max_lines: usize,
    max_chars: Option<usize>,
) -> Option<String> {
    slice_span_marked(repo_root, path, from, to, max_lines, max_chars).map(|(text, _)| text)
}

/// [`slice_span_capped`], and whether the text holds less than the span.
fn slice_span_marked(
    repo_root: &Path,
    path: &str,
    from: u32,
    to: u32,
    max_lines: usize,
    max_chars: Option<usize>,
) -> Option<(String, bool)> {
    let bytes = std::fs::read(repo_root.join(path)).ok()?;
    // The span read goes through the source decoder.
    let source = sieve_core::fingerprint::decode_source(&bytes)?;
    let lines: Vec<&str> = source.split('\n').collect();
    let start = from.max(1) as usize;
    let end = (to as usize).min(lines.len());
    if start > end {
        return Some((String::new(), false));
    }
    let slice = &lines[start - 1..end];
    let mut keep = slice.len().min(max_lines);
    // Set when the first line alone passes the char cap and prints cut.
    let mut first_cut: Option<String> = None;
    if let Some(cap) = max_chars {
        let mut used = 0;
        let mut fit = 0;
        for line in &slice[..keep] {
            let width = line.encode_utf16().count() + usize::from(fit > 0);
            if used + width > cap {
                break;
            }
            used += width;
            fit += 1;
        }
        if fit == 0 && keep > 0 {
            first_cut = Some(cut_to_units(slice[0], cap));
            keep = 1;
        } else {
            keep = fit;
        }
    }
    if keep == slice.len() && first_cut.is_none() {
        return Some((slice.join("\n"), false));
    }
    let mut head: Vec<String> = slice[..keep].iter().map(|l| l.to_string()).collect();
    let was_cut = first_cut.is_some();
    if let Some(cut) = first_cut {
        head[0] = cut;
    }
    let open_from = if max_chars.is_some() {
        start + keep
    } else {
        start
    };
    let note = if was_cut { " (line cut)" } else { "" };
    head.push(format!(
        "… (+{} more lines; open {path}:L{open_from}-L{end}){note}",
        slice.len() - keep
    ));
    Some((head.join("\n"), true))
}

/// The longest prefix of `line` that holds at most `cap` UTF-16 units, cut at
/// a char boundary.
fn cut_to_units(line: &str, cap: usize) -> String {
    let mut used = 0;
    let mut end = 0;
    for (idx, c) in line.char_indices() {
        used += c.len_utf16();
        if used > cap {
            break;
        }
        end = idx + c.len_utf8();
    }
    line[..end].to_string()
}

/// Attaches inlined source to every hit whose pointer is a real
/// `path:span`. Crux-first: a node with a graph-stored crux and `full` off
/// gets that short excerpt with an escalation marker; otherwise the whole
/// span is read from disk.
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
                hit.cut = true;
                continue;
            }
        }
        // Sieve sets the code only when it is truthy: a span past the end
        // of the file reads as an empty string and gives no code.
        if let Some((code, cut)) =
            slice_span(repo_root, path, from, to).filter(|(c, _)| !c.is_empty())
        {
            hit.code = Some(code);
            hit.cut = cut;
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
        cut: false,
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
        assert!(is_test_path("crates/x/src/kt_proof_tests.rs"));
        assert!(is_test_path("crates/x/src/kt_proof_test.rs"));
        assert!(is_test_path("pkg/test_a.py"));
        assert!(is_test_path("pkg/a_test.py"));
        assert!(!is_test_path("crates/x/src/contests.rs"));
        assert!(is_test_path("src/tests.rs"));
        assert!(is_test_path("test.rs"));
        assert!(!is_test_path("src/a.test.\u{212A}"));
    }
}

#[cfg(test)]
mod class_name_tests {
    use super::is_test_class_name;

    #[test]
    fn test_class_name_needs_a_word_boundary() {
        assert!(!is_test_class_name("Testimonial"));
        assert!(!is_test_class_name("Testable"));
        assert!(is_test_class_name("TestParser"));
        assert!(is_test_class_name("Test"));
        assert!(is_test_class_name("Test2"));
        assert!(is_test_class_name("FooTests"));
        assert!(is_test_class_name("FooTest"));
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
        let (code, cut) = slice_span(&dir, "a.txt", 2, 3).expect("file reads");
        assert_eq!(code, "two\nthree");
        assert!(!cut);
        // `to` past the file end clamps to the last line. The file's
        // trailing newline is a real empty final line, matching
        // JavaScript's `source.split("\n")`.
        let (code, _) = slice_span(&dir, "a.txt", 3, 100).expect("file reads");
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
        let (code, cut) = slice_span(&dir, "big.txt", 1, 100).expect("file reads");
        assert!(cut);
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

    /// A `stale` node still keeps its own `crux`, and `--source`
    /// still prints it verbatim — the crux is never re-sliced from the
    /// (now different) source.
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
            cut: false,
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
    /// one, and reads the span from disk instead.
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
            cut: false,
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
            cut: false,
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

    /// The zero-hit note names every scope, in stored order,
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

    /// The multi-scope branch merges the concept hits, and the top lock
    /// compares the concept with the best candidate's fused score. With graph
    /// rank on, the raw baseline of the best candidate is 1.5, but its fused
    /// score is 1.0. The concept ties at 1.0 and wins on title, so it comes
    /// first. The symbol groups follow by score, then title, and keep their
    /// `[scope/]` label.
    #[test]
    fn test_p3_08_multi_scope_ask_merges_concept_hits() {
        let g = two_scope_graph();
        let root = crate::test_support::TempDir::new("multi-scope");
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

    /// With `--in` on a two-scope graph only one scope has active
    /// nodes, and the hit keeps that scope label.
    #[test]
    fn test_p3_08_in_prefix_on_a_two_scope_graph_keeps_the_scope_label() {
        let g = two_scope_graph();
        let root = crate::test_support::TempDir::new("multi-scope-in");
        let opts = AskOptions {
            in_prefix: Some("packages/alpha".to_string()),
            ..AskOptions::default()
        };

        let result = ask(&g, None, "render", &opts, &root).expect("ask runs");

        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].scope.as_deref(), Some("packages/alpha"));
    }

    /// After the top lock, the footer puts the
    /// top hit's scope first. The top is `renderAardvark` (title wins the
    /// tie), which lives in `packages/beta`, although `packages/alpha`
    /// sorts first by name.
    #[test]
    fn test_p3_08_multi_scope_footer_puts_the_top_scope_first() {
        let g = two_scope_graph();
        let root = crate::test_support::TempDir::new("multi-scope-footer");

        let result = ask(&g, None, "render", &AskOptions::default(), &root).expect("ask runs");

        assert_eq!(result.hits[0].title, "renderAardvark \u{b7} function");
        let meta = result.scopes.expect("two scopes federated");
        assert_eq!(meta.federated, ["packages/beta", "packages/alpha"]);
    }

    /// `-n 0` on a scoring query keeps the `lexical` mode
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

    /// A warm query reuses the corpus for the same graph. A new graph, as
    /// after a refresh, gets a rebuilt corpus, so the new symbol shows.
    #[test]
    fn a_warm_query_rebuilds_the_corpus_when_the_graph_changes() {
        let _serial = WARM_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        let root = std::env::temp_dir().join("sieve-warm-fresh-no-such-dir");
        let opts = AskOptions::default();
        let no_index = || None;
        let ask_for = |graph: &Arc<Graph>| {
            let key = WarmKey {
                graph,
                index_stamp: &|| None,
                load_index: &no_index,
                steps: &[],
            };
            let result = ask_warm(&key, "quokkaz zebrafishz", &opts, &root).expect("ask");
            result.hits.into_iter().map(|h| h.title).collect::<Vec<_>>()
        };
        let zebra = node(
            "a.ts#zebrafishz",
            "zebrafishz",
            Kind::Function,
            "a.ts",
            "L1-L2",
        );
        let quokka = node("a.ts#quokkaz", "quokkaz", Kind::Function, "a.ts", "L3-L4");

        let before = Arc::new(graph(vec![zebra.clone()]));
        let first = ask_for(&before);
        assert!(first.iter().any(|t| t.contains("zebrafishz")));
        assert!(!first.iter().any(|t| t.contains("quokkaz")));
        assert_eq!(ask_for(&before), first);

        let after = Arc::new(graph(vec![zebra, quokka]));
        let second = ask_for(&after);
        assert!(second.iter().any(|t| t.contains("quokkaz")));
        assert!(second.iter().any(|t| t.contains("zebrafishz")));
    }

    /// Serializes the tests that use the one shared cache slot.
    static WARM_LOCK: Mutex<()> = Mutex::new(());

    /// A warm call gives the bytes of an uncached call, on a miss and on a hit.
    /// The ask index loader runs only on a miss, so its count shows each miss.
    /// A new prefix, index stamp or concept doc is a miss with the right answer.
    #[test]
    fn a_warm_query_matches_the_cold_query_and_hits_the_cache() {
        let _serial = WARM_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        let g = Arc::new(two_scope_graph());
        assert!(g.meta.scopes.len() > 1);
        let root = crate::test_support::TempDir::new("warm-equal");
        let loads = std::cell::Cell::new(0_u32);
        let load = || {
            loads.set(loads.get() + 1);
            None
        };
        let run = |stamp: Option<(u128, u64)>, query: &str, prefix: Option<&str>| {
            let opts = AskOptions {
                in_prefix: prefix.map(str::to_string),
                ..AskOptions::default()
            };
            let key = WarmKey {
                graph: &g,
                index_stamp: &|| stamp,
                load_index: &load,
                steps: &[],
            };
            let warm = ask_warm(&key, query, &opts, &root).expect("warm ask");
            let cold = ask(&g, None, query, &opts, &root).expect("cold ask");
            assert_eq!(format!("{warm:?}"), format!("{cold:?}"), "query {query}");
            format!("{warm:?}")
        };
        let queries = ["render", "renderZed", "aardvark render", "nothing here"];

        for prefix in [None, Some("packages/alpha"), Some("packages/beta")] {
            for query in queries {
                let before = loads.get();
                let first = run(None, query, prefix);
                let after_first = loads.get();
                let second = run(None, query, prefix);
                assert_eq!(first, second);
                assert_eq!(loads.get(), after_first, "second call must hit");
                // The first call of a query hits too, once its prefix is cached.
                assert!(after_first - before <= 1);
            }
        }

        // A new prefix misses, and the answer has only that scope.
        run(None, "render", None);
        let n = loads.get();
        let alpha = run(None, "render", Some("packages/alpha"));
        assert_eq!(loads.get(), n + 1, "a new prefix must miss");
        assert!(alpha.contains("renderZed") && !alpha.contains("renderAardvark"));

        // A new index stamp misses.
        let n = loads.get();
        run(Some((1, 2)), "render", Some("packages/alpha"));
        assert_eq!(loads.get(), n + 1, "a new stamp must miss");
        run(Some((1, 2)), "render", Some("packages/alpha"));
        assert_eq!(loads.get(), n + 1, "the same stamp must hit");

        // A new concept doc misses, and its hit shows in the answer.
        let before = run(Some((1, 2)), "render", None);
        assert!(!before.contains("Render Pipeline"));
        let n = loads.get();
        let context = root.join(sieve_core::product().context_dir_name());
        std::fs::create_dir_all(&context).expect("create the context dir");
        std::fs::write(
            context.join("render-pipeline.md"),
            "---\nname: Render Pipeline\nslug: render-pipeline\ntype: system\nsources:\n  - path: packages/beta/b.ts\n    hash: abc\n---\n## Summary\n\nThe render pipeline draws every render layout.\n",
        )
        .expect("write the concept doc");
        let with_concept = run(Some((1, 2)), "render", None);
        assert_eq!(loads.get(), n + 1, "new concept bags must miss");
        assert!(with_concept.contains("Render Pipeline"));
    }
}
