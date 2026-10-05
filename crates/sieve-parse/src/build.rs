//! Builds a `Graph` from a repo on disk (P2-01 to P2-11). This first pass
//! emits one node per file with a known language. Symbol nodes and edges
//! come later.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use sieve_core::collate::collate;
use sieve_core::fingerprint::{decode_source, stat_print, under_only_dirs, Print};
use sieve_core::lang::lang_for_path;
use sieve_core::node_error::node_io_error;
use sieve_core::walk::walk_repo;
use sieve_core::{
    span, Confidence, Edge, Graph, Kind, Meta, Node, Origin, Relation, Scope, SummaryState,
};

use crate::cache::{cache_path, read_cache, write_cache, CacheEntry, ExtractCache, CACHE_VERSION};
use crate::container::{container_lang_of, extract_container};
use crate::extract::{file_residual, Extractor, RawEdge};
use crate::generic::{generic_lang_of, GenericExtractor};
use crate::resolve::{resolve_edges_with_go_modules, GoModule};
use crate::stamp::extractor_stamp;

/// Marker file names that mark a monorepo scope root (P2-06).
const SCOPE_MARKERS: [&str; 9] = [
    "package.json",
    "go.mod",
    "pyproject.toml",
    "setup.py",
    "Cargo.toml",
    "composer.json",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
];

/// The `wiring.json` path a build reads its prior meaning layer from, and
/// writes its own graph to (`sieve_core::write::write_graph`,
/// `src/refresh.rs`). Kept here too, so a build can find its own prior
/// run with no dependency on the writer's call site.
fn wiring_path(context_dir: &Path) -> PathBuf {
    context_dir.join(".graph").join("wiring.json")
}

/// Reads the prior build's nodes, keyed by `id`, for the meaning-tier
/// carry (P1-34, P3-14). Returns an empty map on a missing file or a
/// parse error: a first build, or a corrupt prior file, never blocks a
/// fresh build — it only means every node starts `pending`.
fn read_prior_nodes(context_dir: &Path) -> HashMap<String, Node> {
    let Ok(body) = fs::read(wiring_path(context_dir)) else {
        return HashMap::new();
    };
    let Ok(prior) = serde_json::from_slice::<Graph>(&body) else {
        return HashMap::new();
    };
    prior.nodes.into_iter().map(|n| (n.id.clone(), n)).collect()
}

/// Carries the Tier-2 meaning layer (`summary`, `crux`, `summary_state`)
/// forward from a prior build's node of the same `id`, with the cache and
/// stale bookkeeping and no summarizer.
///
/// Every node arrives here `pending`, with `summary` and `crux` both
/// `None` (`build_graph_cached`, `read_and_extract`,
/// `generic_file_node`). This then applies, per node:
///
/// - No prior node with the same `id`, or a prior node with no
///   `summary`: the node stays `pending`.
/// - A prior node with a `summary`, whose `summary_state` was `ready`
///   and whose `body_hash` still matches: the node becomes `ready`,
///   with the prior `summary` and `crux` copied over unchanged.
/// - Any other prior node with a `summary` (a changed `body_hash`, or a
///   `summary_state` that was already `stale`): the node becomes
///   `stale`, with the prior `summary` and `crux` copied over as a
///   hint. A prior `stale` state never gets promoted back to `ready`
///   just because the hash happens to match again; the cache hit
///   only fires when the prior state was `ready`.
fn carry_meaning_layer(nodes: &mut [Node], prior: &HashMap<String, Node>) {
    for node in nodes.iter_mut() {
        let Some(was) = prior.get(&node.id) else {
            continue;
        };
        if was.summary.is_none() {
            continue;
        }
        if was.summary_state == SummaryState::Ready && was.body_hash == node.body_hash {
            node.summary = was.summary.clone();
            node.crux = was.crux.clone();
            node.summary_state = SummaryState::Ready;
        } else {
            node.summary = was.summary.clone();
            node.crux = was.crux.clone();
            node.summary_state = SummaryState::Stale;
        }
    }
}

/// Builds the symbol graph for the repo at `root`.
///
/// This reads every file `lang_for_path` claims and emits one file node
/// per file. A file that is not valid UTF-8 is skipped, because this pass
/// reads only text source. `meta.scopes` and `meta.languages` cover the
/// claimed files. This turns only the raw `contains` edges into real
/// edges; every other relation waits on resolution (a later task).
///
/// `context_dir` is Sieve's own output dir. A file under it, or under any
/// sibling dir that shares its name as a string prefix, is dropped from the
/// walk (see the `build-cards-freshness.md` note section 10), so a dir named
/// e.g. `sieve-tools/` is excluded on purpose when `context_dir` is `sieve`.
pub fn build_graph(root: &Path, context_dir: &Path) -> std::io::Result<Graph> {
    let files = collect_files(root, context_dir)?;
    let go_modules = discover_go_modules(root, &files)?;
    let (mut graph, raw_edges) = build_graph_with_raw(root, context_dir)?;
    graph.edges = resolve_edges_with_go_modules(&graph.nodes, &raw_edges, &go_modules);
    graph.meta.edge_count = graph.edges.len();
    Ok(graph)
}

/// Finds every `go.mod` under `files` and reads its `module` path, for Go
/// import resolution. `dir` is the posix directory `go.mod` lives in, relative
/// to `root`, `""` for the repo root. A `go.mod` this cannot read or parse is
/// skipped, not an error: a Go file's import then stays an external package
/// string, exactly as it does with no discovered modules at all.
fn discover_go_modules(root: &Path, files: &[PathBuf]) -> std::io::Result<Vec<GoModule>> {
    let mut modules = Vec::new();
    for rel in files {
        if rel.file_name().and_then(|n| n.to_str()) != Some("go.mod") {
            continue;
        }
        let Ok(bytes) = fs::read(root.join(rel)) else {
            continue;
        };
        // Read `go.mod` as lossy UTF-8: an invalid byte becomes U+FFFD, never a
        // skip.
        let text = String::from_utf8_lossy(&bytes);
        // The regex is `/^\s*module\s+(\S+)/m`: any whitespace after
        // `module`, and the path stops at the first whitespace. A prefix
        // match on `"module "` alone would miss a tab separator and would
        // wrongly accept `moduleX` (no split, so `strip_prefix("module")`
        // must land on whitespace, not `X`).
        let Some(module) = text.lines().find_map(|line| {
            let rest = line.trim().strip_prefix("module")?;
            if !rest.starts_with(char::is_whitespace) {
                return None;
            }
            rest.split_whitespace().next()
        }) else {
            continue;
        };
        let module = module.to_string();
        let dir = rel
            .parent()
            .map(to_slash)
            .filter(|d| !d.is_empty())
            .unwrap_or_default();
        modules.push(GoModule { module, dir });
    }
    Ok(modules)
}

/// Builds the symbol graph, and also returns every raw edge, in file
/// order, for a later resolution pass to turn into real edges. See
/// [`build_graph`] for what `context_dir` excludes and why. This function
/// never reads or writes the extraction cache; every file is re-parsed.
pub fn build_graph_with_raw(
    root: &Path,
    context_dir: &Path,
) -> std::io::Result<(Graph, Vec<RawEdge>)> {
    let files = collect_files(root, context_dir)?;
    let mut extractor = Extractor::new().map_err(std::io::Error::other)?;
    let mut generic_extractor = GenericExtractor::new().map_err(std::io::Error::other)?;
    let mut nodes = Vec::new();
    let mut raw_edges = Vec::new();
    let mut languages: BTreeSet<String> = BTreeSet::new();

    for rel in &files {
        let Some((file_node, symbols, edges, label)) =
            extract_one(root, rel, &mut extractor, &mut generic_extractor)?
        else {
            continue;
        };
        nodes.push(file_node);
        nodes.extend(symbols);
        raw_edges.extend(edges);
        languages.insert(label);
    }

    let mut graph = assemble(root, &files, nodes, &raw_edges, languages);
    carry_meaning_layer(&mut graph.nodes, &read_prior_nodes(context_dir));
    Ok((graph, raw_edges))
}

/// One file's extraction result, plus the language label it claimed
/// under.
type ExtractedWithLabel = (Node, Vec<Node>, Vec<RawEdge>, String);

/// Extracts one file by tier priority: native (depth) first, container second,
/// breadth third (see `languages-lsp.md` line 131). Returns `Ok(None)` for a
/// file no tier claims, or that is not valid UTF-8.
fn extract_one(
    root: &Path,
    rel: &Path,
    extractor: &mut Extractor,
    generic_extractor: &mut GenericExtractor,
) -> std::io::Result<Option<ExtractedWithLabel>> {
    let id = to_slash(rel);
    if claim_label(rel, &id).is_none() {
        return Ok(None);
    }
    let bytes = fs::read(root.join(rel))?;
    let Some(text) = decode_source(&bytes) else {
        return Ok(None);
    };
    Ok(extract_text(rel, &text, extractor, generic_extractor))
}

/// Extracts one already-decoded file by the same tier priority as
/// [`extract_one`]. Returns `None` for a file no tier claims.
fn extract_text(
    rel: &Path,
    text: &str,
    extractor: &mut Extractor,
    generic_extractor: &mut GenericExtractor,
) -> Option<ExtractedWithLabel> {
    let id = to_slash(rel);
    if let Some(lang) = lang_for_path(rel) {
        let (file_node, symbols, edges) = extract_native(rel, text, lang.grammar, extractor);
        return Some((file_node, symbols, edges, lang.label.to_string()));
    }
    if let Some(lang) = container_lang_of(&id) {
        let (file_node, symbols, edges) = extract_container(&id, text, extractor);
        return Some((file_node, symbols, edges, lang.name.to_string()));
    }
    if let Some(lang) = generic_lang_of(&id) {
        let (symbols, edges) = generic_extractor.extract_file(&id, text, lang.name);
        let file_node = generic_file_node(&id, text);
        return Some((file_node, symbols, edges, lang.name.to_string()));
    }
    None
}

/// The language label a file claims under, by the same tier priority as
/// [`extract_one`], with no read or parse. [`build_graph_cached`] needs
/// this on a cache hit, where the label never changes even though the
/// cached node and edge lists come from the old cache entry instead of a
/// fresh extraction.
pub(crate) fn claim_label(rel: &Path, id: &str) -> Option<String> {
    if let Some(lang) = lang_for_path(rel) {
        return Some(lang.label.to_string());
    }
    if let Some(lang) = container_lang_of(id) {
        return Some(lang.name.to_string());
    }
    if let Some(lang) = generic_lang_of(id) {
        return Some(lang.name.to_string());
    }
    None
}

/// Whether a build would fingerprint `rel`: true for every tier
/// [`claim_label`] claims (native, container, breadth).
///
/// [`sieve_core::fingerprint::probe_drift`] needs this predicate. A build
/// (`build_graph_cached`) fingerprints every tier, so a drift probe that
/// checks only the native tier misreads every breadth-tier file as
/// `removed` on every call (P1-46, P2-35): that file is in `fp.files` but
/// never reaches the probe's `seen` set.
pub(crate) fn is_claimed(rel: &Path) -> bool {
    claim_label(rel, &to_slash(rel)).is_some()
}

/// A breadth-tier file's own node: `origin: generic`, `chars` in UTF-8
/// bytes, and no residual `body_text` (`languages-lsp.md` line 139,
/// `generic.ts` `fileNode`, `:207-214`).
fn generic_file_node(id: &str, source: &str) -> Node {
    let name = id.rsplit('/').next().unwrap_or(id).to_string();
    let end_line = source.matches('\n').count() as u32 + 1;
    Node {
        id: id.to_string(),
        name,
        kind: Kind::File,
        owner: None,
        path: id.to_string(),
        span: span(1, end_line.max(1)),
        signature: None,
        exported: true,
        origin: Origin::Generic,
        body_hash: hex_sha256(source.as_bytes()),
        chars: Some(source.len() as u64),
        body_text: None,
        arity: None,
        variadic: None,
        summary_state: SummaryState::Pending,
        summary: None,
        crux: None,
    }
}

/// What one call to [`build_graph_cached`] produced, and how much work a
/// warm cache saved.
pub struct BuildReport {
    pub graph: Graph,
    pub files: usize,
    pub parsed: usize,
    pub reused: usize,
    /// The claimed repo-relative posix paths, in walk order, after the
    /// context-dir exclusion. A caller (for example `sieve-cli`) reports
    /// this list instead of walking and filtering the repo a second time.
    pub claimed: Vec<String>,
    /// One [`Print`] per claimed file, taken from that file's cache entry.
    /// The hash covers the same bytes the extractor read, so a caller
    /// never re-reads a file to fingerprint it.
    pub prints: BTreeMap<String, Print>,
    /// One line per file this build could not read, in walk order:
    /// `<rel>: <Node error text>` (P2-01).
    pub errors: Vec<String>,
}

/// Builds the symbol graph the way [`build_graph`] does, but replays a
/// file's nodes and raw edges from the extraction cache (P2-35) when the
/// hash of its decoded text still matches the cache entry. Every file is
/// read and hashed on every build. A file whose hash changed, or that the
/// cache has not seen, is parsed as usual, then written back to the cache.
/// A file this cannot read is reported in [`BuildReport::errors`] and
/// recorded with an empty hash, never a build failure (P2-01).
///
/// The cache lives under `context_dir/.cache/extract.<stamp>.json`, where
/// `<stamp>` is [`extractor_stamp`]. This writes the refreshed cache
/// before resolution.
///
/// Covers every tier [`extract_one`] does: native (depth), container
/// (`.vue`), and breadth.
pub fn build_graph_cached(root: &Path, context_dir: &Path) -> std::io::Result<BuildReport> {
    build_graph_cached_with(root, context_dir, &BuildOptions::default())
}

/// The build flags that shape one [`build_graph_cached_with`] call
/// (P1-70).
#[derive(Debug, Clone, Default)]
pub struct BuildOptions {
    /// The `--only-dir` whitelist: repo-relative posix prefixes. Empty
    /// means the whole repo.
    pub only_dirs: Vec<String>,
    /// `--no-reuse`: skip the extraction cache and parse every file.
    pub no_reuse: bool,
    /// Write no extract sidecar. `check` sets this: `check` never writes the
    /// extract cache, so a `sieve check` on a built tree leaves the cache dir
    /// as it found it.
    pub read_only: bool,
    /// Called after each claimed file with the count done, the total and the
    /// file path. The CLI uses it to draw its one progress line.
    pub progress: Option<fn(usize, usize, &str)>,
}

/// [`build_graph_cached`] with explicit [`BuildOptions`].
pub fn build_graph_cached_with(
    root: &Path,
    context_dir: &Path,
    opts: &BuildOptions,
) -> std::io::Result<BuildReport> {
    let stamp = extractor_stamp();
    let cache_file = cache_path(context_dir, &stamp);
    let old_cache = if opts.no_reuse {
        ExtractCache {
            version: CACHE_VERSION,
            extractor: stamp.clone(),
            files: BTreeMap::new(),
        }
    } else {
        read_cache(&cache_file, &stamp)
    };

    let mut files = collect_files(root, context_dir)?;
    files.retain(|rel| under_only_dirs(&to_slash(rel), &opts.only_dirs));
    let total = if opts.progress.is_some() {
        files
            .iter()
            .filter(|rel| claim_label(rel, &to_slash(rel)).is_some())
            .count()
    } else {
        0
    };
    let mut extractor = Extractor::new().map_err(std::io::Error::other)?;
    let mut generic_extractor = GenericExtractor::new().map_err(std::io::Error::other)?;
    let mut nodes = Vec::new();
    let mut raw_edges = Vec::new();
    let mut languages: BTreeSet<String> = BTreeSet::new();
    let mut new_files: BTreeMap<String, CacheEntry> = BTreeMap::new();
    let mut claimed_ids: Vec<String> = Vec::new();
    let mut parsed = 0usize;
    let mut reused = 0usize;

    let mut errors: Vec<String> = Vec::new();

    for rel in &files {
        let id = to_slash(rel);
        let Some(label) = claim_label(rel, &id) else {
            continue;
        };
        // A file gone between the walk and the stat leaves the set.
        let Ok((size, mtime_ms)) = stat_print(root, &id) else {
            continue;
        };
        claimed_ids.push(id.clone());
        if let Some(progress) = opts.progress {
            progress(claimed_ids.len(), total, &id);
        }

        // Every file is read and hashed, every build; only the parse is
        // replayed. A stat may decide whether a query rebuilds; it never
        // decides what the rebuild reads, or a same-size edit inside one mtime
        // tick stays stale forever (P2-35, P1-46).
        let abs = root.join(rel);
        let empty_entry = |error: Option<String>| CacheEntry {
            size,
            mtime_ms,
            hash: String::new(),
            nodes: Vec::new(),
            raw_edges: Vec::new(),
            error,
        };
        let bytes = match fs::read(&abs) {
            Ok(bytes) => bytes,
            Err(err) => {
                // Recorded with an empty hash, so the freshness probe never
                // reports the file as new on every query.
                let message = format!("{id}: {}", node_io_error(err, "open", &abs));
                errors.push(message.clone());
                new_files.insert(id, empty_entry(Some(message)));
                continue;
            }
        };
        let Some(text) = decode_source(&bytes) else {
            // UTF-16BE: a skip, never an error.
            new_files.insert(id, empty_entry(None));
            continue;
        };
        let hash = hex_sha256(text.as_bytes());

        if let Some(entry) = old_cache.files.get(&id).filter(|e| e.hash == hash) {
            reused += 1;
            let mut entry = entry.clone();
            entry.size = size;
            entry.mtime_ms = mtime_ms;
            match &entry.error {
                // This file failed last time too.
                Some(error) => errors.push(error.clone()),
                None => {
                    nodes.extend(entry.nodes.clone());
                    raw_edges.extend(entry.raw_edges.clone());
                    languages.insert(label);
                }
            }
            new_files.insert(id, entry);
            continue;
        }

        let Some((file_node, symbols, edges, label)) =
            extract_text(rel, &text, &mut extractor, &mut generic_extractor)
        else {
            continue;
        };
        parsed += 1;
        let mut entry_nodes = vec![file_node.clone()];
        entry_nodes.extend(symbols.clone());
        new_files.insert(
            id,
            CacheEntry {
                size,
                mtime_ms,
                hash,
                nodes: entry_nodes,
                raw_edges: edges.clone(),
                error: None,
            },
        );
        nodes.push(file_node);
        nodes.extend(symbols);
        raw_edges.extend(edges);
        languages.insert(label);
    }

    let new_cache = ExtractCache {
        version: CACHE_VERSION,
        extractor: stamp,
        files: new_files,
    };
    if !opts.read_only {
        write_cache(&cache_file, &new_cache)?;
    }

    let go_modules = discover_go_modules(root, &files)?;
    let mut graph = assemble(root, &files, nodes, &raw_edges, languages);
    graph.edges = resolve_edges_with_go_modules(&graph.nodes, &raw_edges, &go_modules);
    graph.meta.edge_count = graph.edges.len();
    carry_meaning_layer(&mut graph.nodes, &read_prior_nodes(context_dir));

    let prints: BTreeMap<String, Print> = new_cache
        .files
        .iter()
        .map(|(id, entry)| {
            (
                id.clone(),
                Print {
                    size: entry.size,
                    mtime_ms: entry.mtime_ms,
                    hash: entry.hash.clone(),
                },
            )
        })
        .collect();

    Ok(BuildReport {
        graph,
        files: claimed_ids.len(),
        parsed,
        reused,
        claimed: claimed_ids,
        prints,
        errors,
    })
}

/// Walks `root`, and drops every file under `context_dir` (or a sibling
/// dir whose name shares its string prefix). See [`build_graph`] for why.
fn collect_files(root: &Path, context_dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let context_prefix = resolved_slash_path(context_dir)?;
    let files: Vec<PathBuf> = walk_repo(root)?
        .into_iter()
        .filter(|rel| {
            let abs = format!("{}/{}", to_slash(root), to_slash(rel));
            !abs.starts_with(&context_prefix)
        })
        .collect();
    Ok(files)
}

/// One file's extraction result: its file node, its symbol nodes, and its
/// raw edges.
type Extracted = (Node, Vec<Node>, Vec<RawEdge>);

/// Extracts the file node and every symbol node and raw edge from the decoded
/// `text` of a native-tier file. The caller decodes the bytes through
/// [`decode_source`], so an invalid UTF-8 byte is already U+FFFD and a Latin-1
/// file still gets its symbols. A UTF-16BE file never reaches this: the caller
/// skips it.
fn extract_native(rel: &Path, text: &str, grammar: &str, extractor: &mut Extractor) -> Extracted {
    let id = to_slash(rel);
    let name = rel
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| id.clone());
    // A tree-sitter end row is the newline count, 0-indexed; Sieve
    // reports that row plus one (P2-08).
    let end_line = text.matches('\n').count() as u32 + 1;
    let chars = text.encode_utf16().count() as u64;
    let body_hash = hex_sha256(text.as_bytes());
    let (symbols, edges) = extractor.extract_file(&id, text, grammar);
    let residual = file_residual(text, &symbols);

    let file_node = Node {
        id: id.clone(),
        name,
        kind: Kind::File,
        owner: None,
        path: id,
        span: span(1, end_line),
        signature: None,
        exported: true,
        origin: Origin::Ast,
        body_hash,
        chars: Some(chars),
        body_text: Some(residual),
        arity: None,
        variadic: None,
        summary_state: SummaryState::Pending,
        summary: None,
        crux: None,
    };
    (file_node, symbols, edges)
}

/// Builds the `Graph`'s scopes, `meta`, and `contains` edges from a
/// finished node list. Both [`build_graph_with_raw`] and
/// [`build_graph_cached`] share this assembly step, so a warm build and a
/// cold build produce a byte-identical graph.
fn assemble(
    root: &Path,
    files: &[PathBuf],
    nodes: Vec<Node>,
    raw_edges: &[RawEdge],
    languages: BTreeSet<String>,
) -> Graph {
    let scopes = find_scopes(root, files, &nodes);
    let node_count = nodes.len();
    let contains_edges: Vec<Edge> = raw_edges
        .iter()
        .filter(|e| e.relation == Relation::Contains)
        .filter_map(|e| {
            e.target_id.clone().map(|target| Edge {
                source: e.source.clone(),
                target,
                relation: Relation::Contains,
                confidence: Confidence::Extracted,
            })
        })
        .collect();
    let edge_count = contains_edges.len();

    Graph {
        meta: Meta {
            version: 1,
            node_count,
            edge_count,
            languages: languages.into_iter().collect(),
            scopes,
        },
        nodes,
        edges: contains_edges,
    }
}

/// Returns the lowercase hex sha256 digest of `bytes`.
pub(crate) fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Joins path components with `/`, so a Windows path matches a repo id.
fn to_slash(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Resolves `path` against the current dir, then returns it as a forward-slash
/// string. `path` is returned as-is, slash-joined, when it is already absolute.
fn resolved_slash_path(path: &Path) -> std::io::Result<String> {
    if path.is_absolute() {
        return Ok(to_slash(path));
    }
    let cwd = std::env::current_dir()?;
    Ok(to_slash(&cwd.join(path)))
}

/// Minimum non-file node count a discovered sub-scope needs to stand on its
/// own. A scope below this count folds into the root scope.
const MIN_SCOPE_NODES: usize = 5;

/// Finds every monorepo scope and applies the minimum-substance guard.
///
/// This covers every scope-discovery rule: any marker-file dir is a
/// candidate (rule 1); a workspace-glob config is the only JS-family scope
/// intent honored, once one is present (rule 2); a
/// candidate deeper than 2 path segments is dropped unless it is a
/// workspace match (rule 3); a candidate nested
/// inside another collapses to the shallower one, except a workspace
/// match wins over a non-workspace parent (rule 4); a
/// sub-scope with fewer than [`MIN_SCOPE_NODES`] non-file nodes folds into
/// root (rule 5); root-only survival collapses to the canonical form (rule
/// 6); the kept scopes are ordered prefix-length descending, then
/// lexicographic, with `label` equal to `prefix` (rule 7).
fn find_scopes(root: &Path, files: &[PathBuf], nodes: &[Node]) -> Vec<Scope> {
    let candidates = candidate_scopes(root, files);
    apply_min_substance_guard(candidates, nodes)
}

/// One scope candidate before the nesting collapse: its markers, and
/// whether a workspace glob matched it (rule 2).
#[derive(Clone)]
struct Candidate {
    markers: Vec<String>,
    is_workspace: bool,
}

/// Finds every marker-file dir, resolves workspace-glob intent (rule 2),
/// then applies the depth guard (rule 3), the nesting collapse (rule 4),
/// and the canonical-root and ordering rules (rules 6 and 7).
/// Root-level counting (rule 5) needs node counts, so it runs separately
/// in [`apply_min_substance_guard`].
fn candidate_scopes(root: &Path, files: &[PathBuf]) -> Vec<Scope> {
    // A marker counts when the file exists on disk, even if the walk
    // dropped it (an ignored marker): the check runs per
    // visible dir, in `SCOPE_MARKERS` order.
    let dirs = collect_dirs(files);
    let mut markers_by_prefix: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for dir in &dirs {
        let found: Vec<String> = SCOPE_MARKERS
            .iter()
            .filter(|m| root.join(dir).join(m).exists())
            .map(|m| m.to_string())
            .collect();
        if !found.is_empty() {
            markers_by_prefix.insert(dir.clone(), found);
        }
    }

    // Rule 2: a workspace-config glob set, when present, is the only
    // JS-family scope intent honored. Every other dir's `package.json`
    // marker is stripped, the root included.
    let workspace_globs = read_workspace_globs(root);
    let workspace_matches: BTreeSet<String> = workspace_globs
        .as_ref()
        .map(|globs| {
            globs
                .iter()
                .flat_map(|glob| resolve_glob(&dirs, glob))
                .collect()
        })
        .unwrap_or_default();

    let mut candidates: BTreeMap<String, Candidate> = BTreeMap::new();
    for (dir, mut markers) in markers_by_prefix {
        let is_workspace = workspace_matches.contains(&dir);
        if workspace_globs.is_some() && !is_workspace && markers.iter().any(|m| m == "package.json")
        {
            markers.retain(|m| m != "package.json");
            if markers.is_empty() {
                continue;
            }
        }
        // Rule 3: depth guard (workspace matches are exempt).
        if segment_depth(&dir) > 2 && !is_workspace {
            continue;
        }
        candidates.insert(
            dir,
            Candidate {
                markers,
                is_workspace,
            },
        );
    }

    // A workspace-matched dir is a candidate even without a marker file
    // of its own.
    for dir in &workspace_matches {
        candidates
            .entry(dir.clone())
            .and_modify(|c| c.is_workspace = true)
            .or_insert_with(|| Candidate {
                markers: Vec::new(),
                is_workspace: true,
            });
    }

    // Rule 4: nesting collapse, in two ordered passes against frozen
    // snapshots, so the outcome is layout-determined, not iteration-order
    // determined.
    //
    // Pass A: a workspace-glob candidate's nearest ancestor candidate, if
    // that ancestor is not itself a workspace match, is dropped — a
    // workspace match wins over its parent.
    let frozen = candidates.clone();
    let pass_a_deletes: BTreeSet<String> = frozen
        .iter()
        .filter(|(_, entry)| entry.is_workspace)
        .filter_map(|(prefix, _)| nearest_ancestor_candidate(prefix, &frozen))
        .filter(|ancestor| !frozen[ancestor].is_workspace)
        .collect();
    for prefix in &pass_a_deletes {
        candidates.remove(prefix);
    }

    // Pass B: plain nesting collapse against the post-A set. A candidate
    // nested inside a surviving ancestor is dropped, unless it is a
    // workspace match whose ancestor is not itself a workspace match —
    // that pair already has Pass A's immunity.
    let post_a = candidates.clone();
    let pass_b_deletes: BTreeSet<String> = post_a
        .iter()
        .filter(|(prefix, _)| !prefix.is_empty())
        .filter_map(|(prefix, entry)| {
            let ancestor = nearest_ancestor_candidate(prefix, &post_a)?;
            let ancestor_is_workspace = post_a[&ancestor].is_workspace;
            (!entry.is_workspace || ancestor_is_workspace).then(|| prefix.clone())
        })
        .collect();
    for prefix in &pass_b_deletes {
        candidates.remove(prefix);
    }

    // Rule 6: canonical single-scope form when nothing, or only root,
    // survives.
    if candidates.is_empty() {
        return vec![canonical_root(Vec::new())];
    }
    if candidates.len() == 1 {
        if let Some(c) = candidates.remove("") {
            return vec![canonical_root(c.markers)];
        }
    }

    // Rule 7: label = prefix; order prefix-length desc, then lexicographic.
    let mut scopes: Vec<Scope> = candidates
        .into_iter()
        .map(|(prefix, c)| Scope {
            label: prefix.clone(),
            markers: c.markers,
            prefix,
        })
        .collect();
    scopes.sort_by(|a, b| {
        utf16_len(&b.prefix)
            .cmp(&utf16_len(&a.prefix))
            .then_with(|| collate(&a.prefix, &b.prefix))
    });
    scopes
}

/// JS `String.length`: the count of UTF-16 code units.
fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Collects every directory that a claimed file sits under, at every
/// depth, plus the root (`""`). A workspace glob resolves against the same
/// directory set the walk actually claims.
fn collect_dirs(files: &[PathBuf]) -> BTreeSet<String> {
    let mut dirs = BTreeSet::new();
    dirs.insert(String::new());
    for file in files {
        let rel = to_slash(file);
        let parts: Vec<&str> = rel.split('/').collect();
        for i in 1..parts.len() {
            dirs.insert(parts[..i].join("/"));
        }
    }
    dirs
}

/// Rule 2: resolves workspace-config-as-intent globs. Returns `None` when
/// the repo carries no workspace config. A `pnpm-workspace.yaml` wins over
/// `package.json#workspaces` when both exist.
fn read_workspace_globs(root: &Path) -> Option<Vec<String>> {
    let pnpm_path = root.join("pnpm-workspace.yaml");
    if let Ok(text) = fs::read_to_string(&pnpm_path) {
        if let Some(globs) = parse_pnpm_packages_list(&text) {
            return Some(globs);
        }
    }
    let pkg_path = root.join("package.json");
    let text = fs::read_to_string(&pkg_path).ok()?;
    let pkg: serde_json::Value = serde_json::from_str(&text).ok()?;
    let workspaces = pkg.get("workspaces")?;
    if let Some(globs) = workspaces.as_array() {
        return Some(
            globs
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
        );
    }
    let packages = workspaces.get("packages")?.as_array()?;
    Some(
        packages
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
    )
}

/// Reports whether `line`, once trimmed, opens a YAML `packages:` key
/// (`/^packages\s*:/`).
fn is_packages_key_line(line: &str) -> bool {
    let Some(rest) = line.trim().strip_prefix("packages") else {
        return false;
    };
    rest.trim_start().starts_with(':')
}

/// Minimal `packages:` list parser for `pnpm-workspace.yaml`: no YAML
/// dependency, handles the `packages:\n  - 'glob'\n  - "glob"` form pnpm
/// actually generates.
fn parse_pnpm_packages_list(text: &str) -> Option<Vec<String>> {
    let lines: Vec<&str> = text.lines().collect();
    let idx = lines.iter().position(|line| is_packages_key_line(line))?;
    let mut globs = Vec::new();
    for line in &lines[idx + 1..] {
        if line.trim().is_empty() {
            continue;
        }
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix('-') else {
            break; // dedented -- the list ended
        };
        // Strip one quote from each end (`/^['"]|['"]$/g`), not a run.
        let val = rest.trim();
        let is_quote = |c: char| c == '\'' || c == '"';
        let val = val.strip_prefix(is_quote).unwrap_or(val);
        let val = val.strip_suffix(is_quote).unwrap_or(val);
        if !val.is_empty() {
            globs.push(val.to_string());
        }
    }
    (!globs.is_empty()).then_some(globs)
}

/// Resolves one workspace glob against visible directories only. Supports
/// `dir/*` (immediate subdirs), `dir/**` (any nested depth), and literal
/// dirs.
fn resolve_glob(dirs: &BTreeSet<String>, pattern: &str) -> Vec<String> {
    let norm = pattern.replace('\\', "/");
    let norm = norm.strip_suffix('/').unwrap_or(&norm);
    let (base, recursive): (String, bool) = if norm == "**" {
        (String::new(), true)
    } else if norm == "*" {
        (String::new(), false)
    } else if let Some(base) = norm.strip_suffix("/**") {
        (base.to_string(), true)
    } else if let Some(base) = norm.strip_suffix("/*") {
        (base.to_string(), false)
    } else if !norm.contains('*') {
        return if dirs.contains(norm) {
            vec![norm.to_string()]
        } else {
            vec![]
        };
    } else {
        return vec![]; // unsupported glob form
    };

    dirs.iter()
        .filter(|dir| {
            if dir.is_empty() {
                return false;
            }
            if recursive {
                base.is_empty() || dir.starts_with(&format!("{base}/"))
            } else {
                let parent = match dir.rfind('/') {
                    Some(i) => &dir[..i],
                    None => "",
                };
                parent == base
            }
        })
        .cloned()
        .collect()
}

/// Finds the nearest ancestor of `prefix` that is itself a candidate in
/// `candidates`, checking the deepest possible ancestor first. Root
/// (`""`) is never returned, matching the loop bound.
fn nearest_ancestor_candidate(
    prefix: &str,
    candidates: &BTreeMap<String, Candidate>,
) -> Option<String> {
    let segs: Vec<&str> = prefix.split('/').collect();
    for i in (1..segs.len()).rev() {
        let ancestor = segs[..i].join("/");
        if candidates.contains_key(&ancestor) {
            return Some(ancestor);
        }
    }
    None
}

/// Rule 5: folds a sub-scope with fewer than [`MIN_SCOPE_NODES`] non-file
/// nodes into root, then re-applies the canonical single-scope form (rule
/// 6) if only root is left.
fn apply_min_substance_guard(scopes: Vec<Scope>, nodes: &[Node]) -> Vec<Scope> {
    if scopes.len() <= 1 {
        return scopes;
    }
    let mut counts: BTreeMap<String, usize> =
        scopes.iter().map(|s| (s.prefix.clone(), 0)).collect();
    for node in nodes {
        if node.kind == Kind::File {
            continue;
        }
        let owner = scope_of(&node.path, &scopes);
        if let Some(count) = counts.get_mut(&owner) {
            *count += 1;
        }
    }
    let kept: Vec<Scope> = scopes
        .into_iter()
        .filter(|s| {
            s.prefix.is_empty() || counts.get(&s.prefix).copied().unwrap_or(0) >= MIN_SCOPE_NODES
        })
        .collect();
    if kept.is_empty() {
        return vec![canonical_root(Vec::new())];
    }
    if kept.len() == 1 && kept[0].prefix.is_empty() {
        return vec![canonical_root(kept[0].markers.clone())];
    }
    kept
}

/// The canonical single-scope form emitted when no real sub-scope
/// survives: an empty prefix and label, with root's own markers (if any).
fn canonical_root(markers: Vec<String>) -> Scope {
    Scope {
        prefix: String::new(),
        label: String::new(),
        markers,
    }
}

/// Nearest-prefix owner of `path` among `scopes`. `scopes` must already be
/// ordered prefix-length descending, so the first prefix match is the
/// longest. Falls back to `""` when no scope prefix matches.
fn scope_of(path: &str, scopes: &[Scope]) -> String {
    for scope in scopes {
        if scope.prefix.is_empty() {
            continue; // root is the fallback, checked last
        }
        if path == scope.prefix || path.starts_with(&format!("{}/", scope.prefix)) {
            return scope.prefix.clone();
        }
    }
    scopes
        .iter()
        .find(|s| s.prefix.is_empty())
        .map(|s| s.prefix.clone())
        .unwrap_or_default()
}

/// Number of `/`-separated segments in `dir`; `""` (root) is depth 0.
fn segment_depth(dir: &str) -> usize {
    if dir.is_empty() {
        0
    } else {
        dir.split('/').count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sieve_core::Crux;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A minimal file node for the meaning-carry tests, all other fields
    /// held at their `pending` build-time defaults.
    fn plain_node(id: &str, body_hash: &str) -> Node {
        Node {
            id: id.to_string(),
            name: id.to_string(),
            kind: Kind::File,
            owner: None,
            path: id.to_string(),
            span: span(1, 1),
            signature: None,
            exported: true,
            origin: Origin::Ast,
            body_hash: body_hash.to_string(),
            chars: None,
            body_text: None,
            arity: None,
            variadic: None,
            summary_state: SummaryState::Pending,
            summary: None,
            crux: None,
        }
    }

    fn crux(code: &str) -> Crux {
        Crux {
            code: code.to_string(),
            span: "L1-L1".to_string(),
        }
    }

    #[test]
    fn carry_meaning_layer_p1_34_a_matching_hash_after_ready_stays_ready() {
        let mut prior = plain_node("a.ts", "hash-1");
        prior.summary_state = SummaryState::Ready;
        prior.summary = Some("summarized".to_string());
        prior.crux = Some(crux("const a = 1;"));
        let prior_map: HashMap<String, Node> = [(prior.id.clone(), prior)].into_iter().collect();

        let mut nodes = vec![plain_node("a.ts", "hash-1")];
        carry_meaning_layer(&mut nodes, &prior_map);

        assert_eq!(nodes[0].summary_state, SummaryState::Ready);
        assert_eq!(nodes[0].summary.as_deref(), Some("summarized"));
        assert_eq!(
            nodes[0].crux.as_ref().map(|c| c.code.as_str()),
            Some("const a = 1;")
        );
    }

    #[test]
    fn carry_meaning_layer_p1_34_a_changed_hash_after_ready_gives_stale() {
        let mut prior = plain_node("a.ts", "hash-1");
        prior.summary_state = SummaryState::Ready;
        prior.summary = Some("summarized".to_string());
        prior.crux = Some(crux("const a = 1;"));
        let prior_map: HashMap<String, Node> = [(prior.id.clone(), prior)].into_iter().collect();

        let mut nodes = vec![plain_node("a.ts", "hash-2")];
        carry_meaning_layer(&mut nodes, &prior_map);

        assert_eq!(nodes[0].summary_state, SummaryState::Stale);
        assert_eq!(nodes[0].summary.as_deref(), Some("summarized"));
        assert_eq!(
            nodes[0].crux.as_ref().map(|c| c.code.as_str()),
            Some("const a = 1;")
        );
    }

    #[test]
    fn carry_meaning_layer_p1_34_a_prior_stale_stays_stale_even_on_a_matching_hash() {
        let mut prior = plain_node("a.ts", "hash-1");
        prior.summary_state = SummaryState::Stale;
        prior.summary = Some("old hint".to_string());
        prior.crux = Some(crux("const a = 1;"));
        let prior_map: HashMap<String, Node> = [(prior.id.clone(), prior)].into_iter().collect();

        // The hash matches the prior node, but the prior state was already
        // "stale", not "ready" — the cache hit only fires on "ready".
        let mut nodes = vec![plain_node("a.ts", "hash-1")];
        carry_meaning_layer(&mut nodes, &prior_map);

        assert_eq!(nodes[0].summary_state, SummaryState::Stale);
        assert_eq!(nodes[0].summary.as_deref(), Some("old hint"));
    }

    #[test]
    fn carry_meaning_layer_p1_34_a_new_id_stays_pending() {
        let prior_map: HashMap<String, Node> = HashMap::new();

        let mut nodes = vec![plain_node("new.ts", "hash-1")];
        carry_meaning_layer(&mut nodes, &prior_map);

        assert_eq!(nodes[0].summary_state, SummaryState::Pending);
        assert_eq!(nodes[0].summary, None);
        assert_eq!(nodes[0].crux, None);
    }

    #[test]
    fn carry_meaning_layer_p1_34_a_prior_node_with_no_summary_stays_pending() {
        let prior = plain_node("a.ts", "hash-1"); // summary stays None
        let prior_map: HashMap<String, Node> = [(prior.id.clone(), prior)].into_iter().collect();

        let mut nodes = vec![plain_node("a.ts", "hash-2")];
        carry_meaning_layer(&mut nodes, &prior_map);

        assert_eq!(nodes[0].summary_state, SummaryState::Pending);
        assert_eq!(nodes[0].summary, None);
        assert_eq!(nodes[0].crux, None);
    }

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(label: &str) -> Self {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let pid = std::process::id();
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .expect("system clock before epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!("sieve-build-{label}-{pid}-{n}-{nanos}"));
            fs::create_dir_all(&path).expect("create temp dir");
            TempDir { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn write_file(dir: &Path, rel: &str, contents: &str) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dir");
        }
        fs::write(path, contents).expect("write file");
    }

    #[test]
    fn test_xfile_full_build_gives_an_extracted_cross_file_edge() {
        let dir = TempDir::new("xfile");
        write_file(
            &dir.path,
            "lib.ts",
            "export function helper() { return 1; }\n",
        );
        write_file(
            &dir.path,
            "main.ts",
            "import { helper } from \"./lib\";\nexport function run() { return helper(); }\n",
        );
        let ctx = dir.path.join("ctx");
        let confidence = build_graph(&dir.path, &ctx)
            .expect("build graph")
            .edges
            .into_iter()
            .find(|e| e.relation == Relation::Calls && e.target == "lib.ts#helper")
            .map(|e| e.confidence);
        assert_eq!(confidence, Some(Confidence::Extracted));
    }

    #[test]
    fn discover_go_modules_reads_a_trailing_comment_and_a_tab_separator() {
        let dir = TempDir::new("gomod");
        write_file(&dir.path, "a/go.mod", "module example.com/app // comment\n");
        write_file(&dir.path, "b/go.mod", "module\texample.com/tabbed\n");
        let files = vec![PathBuf::from("a/go.mod"), PathBuf::from("b/go.mod")];

        let modules = discover_go_modules(&dir.path, &files).expect("read go.mod files");

        let a = modules
            .iter()
            .find(|m| m.dir == "a")
            .expect("a/go.mod module");
        assert_eq!(a.module, "example.com/app");
        let b = modules
            .iter()
            .find(|m| m.dir == "b")
            .expect("b/go.mod module");
        assert_eq!(b.module, "example.com/tabbed");
    }

    #[test]
    fn discover_go_modules_skips_a_module_line_with_no_separating_whitespace() {
        let dir = TempDir::new("gomod-nospace");
        write_file(&dir.path, "go.mod", "moduleX example.com/app\n");
        let files = vec![PathBuf::from("go.mod")];

        let modules = discover_go_modules(&dir.path, &files).expect("read go.mod files");

        assert!(modules.is_empty());
    }

    #[test]
    fn build_graph_p2_05_skips_a_file_with_no_known_language() {
        let dir = TempDir::new("skip");
        write_file(&dir.path, "README.md", "no lang claims this file\n");

        let graph =
            build_graph(&dir.path, &dir.path.join("sieve")).expect("build_graph should not error");
        assert!(graph.nodes.is_empty());
        assert_eq!(graph.meta.node_count, 0);
    }

    #[test]
    fn build_graph_p2_08_09_11_file_node_span_chars_and_hash() {
        let dir = TempDir::new("fields");
        write_file(&dir.path, "a.ts", "const x = 1;\nconst y = 2;\n");

        let graph =
            build_graph(&dir.path, &dir.path.join("sieve")).expect("build_graph should not error");
        assert_eq!(graph.nodes.len(), 1);
        let node = &graph.nodes[0];
        assert_eq!(node.id, "a.ts");
        assert_eq!(node.span, "L1-L3");
        assert_eq!(node.chars, Some(26));
        assert_eq!(node.body_hash, hex_sha256(b"const x = 1;\nconst y = 2;\n"));
    }

    /// P3-26: a file that is not valid UTF-8 keeps its file node. Each
    /// invalid byte becomes one U+FFFD (lossy UTF-8 decoding), and the hash
    /// covers the decoded text.
    #[test]
    fn build_graph_p3_26_keeps_a_file_that_is_not_valid_utf8() {
        let dir = TempDir::new("badutf8");
        let path = dir.path.join("a.ts");
        fs::write(&path, [0x66, 0x6e, 0xff, 0xfe]).expect("write file");

        let graph =
            build_graph(&dir.path, &dir.path.join("sieve")).expect("build_graph should not error");
        assert_eq!(graph.nodes.len(), 1);
        let node = &graph.nodes[0];
        assert_eq!(node.id, "a.ts");
        assert_eq!(node.chars, Some(4));
        assert_eq!(node.body_hash, hex_sha256("fn\u{FFFD}\u{FFFD}".as_bytes()));
    }

    /// A UTF-16BE file is the one skip: the reader returns
    /// `null` for the `FE FF` BOM.
    #[test]
    fn build_graph_skips_a_utf16be_file() {
        let dir = TempDir::new("utf16be");
        fs::write(dir.path.join("a.ts"), [0xfe, 0xff, 0x00, 0x66]).expect("write file");

        let graph =
            build_graph(&dir.path, &dir.path.join("sieve")).expect("build_graph should not error");
        assert!(graph.nodes.is_empty());
    }

    #[test]
    fn build_graph_p2_06_nested_scope_collapses_to_the_shallower_one() {
        let dir = TempDir::new("nested");
        write_file(&dir.path, "pkg/package.json", "{}");
        write_file(&dir.path, "pkg/sub/package.json", "{}");
        write_file(&dir.path, "pkg/a.ts", "const x = 1;\n");

        let graph =
            build_graph(&dir.path, &dir.path.join("sieve")).expect("build_graph should not error");
        assert_eq!(graph.meta.scopes.len(), 1);
        assert_eq!(graph.meta.scopes[0].prefix, "pkg");
    }

    /// Writes a TS file that declares `count` top-level functions, each a
    /// distinct symbol node once extracted.
    fn write_functions(dir: &Path, rel: &str, count: u32) {
        let mut text = String::new();
        for i in 0..count {
            text.push_str(&format!("function f{i}(): number {{\n  return {i};\n}}\n"));
        }
        write_file(dir, rel, &text);
    }

    #[test]
    fn build_graph_p2_06_sub_scope_at_the_node_floor_keeps_both_scopes_sub_first() {
        let dir = TempDir::new("guard-keep");
        write_file(&dir.path, "package.json", "{}");
        write_file(&dir.path, "packages/big/package.json", "{}");
        write_functions(&dir.path, "packages/big/index.ts", 5);

        let graph =
            build_graph(&dir.path, &dir.path.join("sieve")).expect("build_graph should not error");
        assert_eq!(
            graph.meta.scopes,
            vec![
                Scope {
                    prefix: "packages/big".to_string(),
                    label: "packages/big".to_string(),
                    markers: vec!["package.json".to_string()],
                },
                Scope {
                    prefix: String::new(),
                    label: String::new(),
                    markers: vec!["package.json".to_string()],
                },
            ]
        );
    }

    #[test]
    fn build_graph_p2_06_sub_scope_below_the_node_floor_folds_into_root() {
        let dir = TempDir::new("guard-fold");
        write_file(&dir.path, "package.json", "{}");
        write_file(&dir.path, "packages/tiny/package.json", "{}");
        write_functions(&dir.path, "packages/tiny/index.ts", 4);

        let graph =
            build_graph(&dir.path, &dir.path.join("sieve")).expect("build_graph should not error");
        assert_eq!(
            graph.meta.scopes,
            vec![Scope {
                prefix: String::new(),
                label: String::new(),
                markers: vec!["package.json".to_string()],
            }]
        );
    }

    #[test]
    fn build_graph_p2_06_a_single_candidate_skips_the_substance_guard() {
        let dir = TempDir::new("guard-skip");
        write_file(&dir.path, "packages/tiny/package.json", "{}");
        write_functions(&dir.path, "packages/tiny/index.ts", 1);

        let graph =
            build_graph(&dir.path, &dir.path.join("sieve")).expect("build_graph should not error");
        assert_eq!(
            graph.meta.scopes,
            vec![Scope {
                prefix: "packages/tiny".to_string(),
                label: "packages/tiny".to_string(),
                markers: vec!["package.json".to_string()],
            }]
        );
    }

    #[test]
    fn build_graph_drops_a_file_under_the_context_dir() {
        let dir = TempDir::new("ctx-sieve");
        write_file(&dir.path, "sieve/x.ts", "const x = 1;\n");
        write_file(&dir.path, "a.ts", "const a = 1;\n");

        let graph =
            build_graph(&dir.path, &dir.path.join("sieve")).expect("build_graph should not error");
        let ids: Vec<&str> = graph.nodes.iter().map(|n| n.id.as_str()).collect();
        assert!(!ids.contains(&"sieve/x.ts"));
        assert!(ids.contains(&"a.ts"));
    }

    #[test]
    fn build_graph_drops_a_file_under_a_sibling_dir_that_shares_the_context_prefix() {
        // The prefix test is a plain string test, not a path-segment test. A
        // dir whose name starts with "sieve", like "sieve-tools", is excluded
        // on purpose.
        let dir = TempDir::new("ctx-siblings");
        write_file(&dir.path, "sieve-tools/y.ts", "const y = 1;\n");
        write_file(&dir.path, "a.ts", "const a = 1;\n");

        let graph =
            build_graph(&dir.path, &dir.path.join("sieve")).expect("build_graph should not error");
        let ids: Vec<&str> = graph.nodes.iter().map(|n| n.id.as_str()).collect();
        assert!(!ids.contains(&"sieve-tools/y.ts"));
        assert!(ids.contains(&"a.ts"));
    }

    #[test]
    fn build_graph_drops_sieveing_too_because_the_string_prefix_matches() {
        // "sieveing" starts with the "sieve" characters, so the plain
        // string-prefix test drops it as well. Pinned so a future switch to
        // a path-segment test is a visible, deliberate change.
        let dir = TempDir::new("ctx-sieveing");
        write_file(&dir.path, "sieveing/z.ts", "const z = 1;\n");
        write_file(&dir.path, "a.ts", "const a = 1;\n");

        let graph =
            build_graph(&dir.path, &dir.path.join("sieve")).expect("build_graph should not error");
        let ids: Vec<&str> = graph.nodes.iter().map(|n| n.id.as_str()).collect();
        assert!(!ids.contains(&"sieveing/z.ts"));
        assert!(ids.contains(&"a.ts"));
    }

    fn dirs(entries: &[&str]) -> BTreeSet<String> {
        entries.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn resolve_glob_star_matches_immediate_children_only() {
        let d = dirs(&["dir", "dir/a", "dir/a/b"]);
        assert_eq!(resolve_glob(&d, "dir/*"), vec!["dir/a".to_string()]);
    }

    #[test]
    fn resolve_glob_double_star_matches_any_depth() {
        let d = dirs(&["dir", "dir/a", "dir/a/b"]);
        let mut matched = resolve_glob(&d, "dir/**");
        matched.sort();
        assert_eq!(matched, vec!["dir/a".to_string(), "dir/a/b".to_string()]);
    }

    /// For `packages:\n - ''pkgs/a''\n - apps/web` the build keeps
    /// the scope `apps/web` only: one quote comes off each end, so the
    /// glob is the literal `'pkgs/a'`, which matches no directory.
    #[test]
    fn test_p2_06_pnpm_strips_one_quote_per_end_like_golden() {
        let text = "packages:\n  - ''pkgs/a''\n  - \"apps/web\"\n  - '\n";
        let globs = parse_pnpm_packages_list(text).expect("globs");
        assert_eq!(globs, ["'pkgs/a'", "apps/web"]);
        let d = dirs(&["pkgs", "pkgs/a", "apps", "apps/web"]);
        assert!(resolve_glob(&d, &globs[0]).is_empty());
        assert_eq!(resolve_glob(&d, &globs[1]), ["apps/web"]);
    }

    #[test]
    fn resolve_glob_literal_matches_only_itself() {
        let d = dirs(&["dir", "dir/a", "dir/a/b"]);
        assert_eq!(resolve_glob(&d, "dir/a"), vec!["dir/a".to_string()]);
    }

    #[test]
    fn build_graph_p2_06_pnpm_workspace_wins_over_package_json_workspaces() {
        let dir = TempDir::new("pnpm-wins");
        write_file(
            &dir.path,
            "pnpm-workspace.yaml",
            "packages:\n  - 'libs/core'\n",
        );
        write_file(&dir.path, "package.json", "{\"workspaces\": [\"apps/*\"]}");
        write_file(&dir.path, "libs/core/package.json", "{}");
        write_functions(&dir.path, "libs/core/index.ts", 5);
        write_file(&dir.path, "apps/web/package.json", "{}");
        write_functions(&dir.path, "apps/web/index.ts", 5);

        let graph =
            build_graph(&dir.path, &dir.path.join("sieve")).expect("build_graph should not error");
        let prefixes: BTreeSet<String> =
            graph.meta.scopes.iter().map(|s| s.prefix.clone()).collect();
        assert_eq!(prefixes, dirs(&["libs/core"]));
    }

    #[test]
    fn build_graph_p2_06_root_workspaces_marker_drops_the_root_entry() {
        let dir = TempDir::new("root-drops");
        write_file(
            &dir.path,
            "package.json",
            "{\"workspaces\": [\"packages/*\"]}",
        );
        write_file(&dir.path, "packages/a/package.json", "{}");
        write_functions(&dir.path, "packages/a/index.ts", 5);

        let graph =
            build_graph(&dir.path, &dir.path.join("sieve")).expect("build_graph should not error");
        assert_eq!(graph.meta.scopes.len(), 1);
        assert_eq!(graph.meta.scopes[0].prefix, "packages/a");
    }

    #[test]
    fn build_graph_p2_06_a_non_package_json_marker_survives_the_glob_strip() {
        // Rule 2 strips only the `package.json` marker from a non-matched
        // dir. A dir whose marker is `Cargo.toml` keeps its candidacy even
        // though a workspace config is present.
        let dir = TempDir::new("cargo-survives");
        write_file(
            &dir.path,
            "package.json",
            "{\"workspaces\": [\"packages/*\"]}",
        );
        write_file(&dir.path, "packages/a/package.json", "{}");
        write_functions(&dir.path, "packages/a/index.ts", 5);
        write_file(&dir.path, "tool/Cargo.toml", "[package]\nname = \"tool\"\n");
        write_functions(&dir.path, "tool/src/main.ts", 5);

        let graph =
            build_graph(&dir.path, &dir.path.join("sieve")).expect("build_graph should not error");
        let prefixes: BTreeSet<String> =
            graph.meta.scopes.iter().map(|s| s.prefix.clone()).collect();
        assert!(prefixes.contains("tool"), "prefixes: {prefixes:?}");
        assert!(prefixes.contains("packages/a"), "prefixes: {prefixes:?}");
    }

    /// P1-70: `only_dirs` keeps the files under the prefix and drops the
    /// rest; `no_reuse` parses every file again with a warm cache.
    #[test]
    fn test_p1_70_build_options_only_dirs_and_no_reuse() {
        let dir = TempDir::new("options");
        fs::create_dir_all(dir.path.join("src")).expect("mkdir src");
        fs::create_dir_all(dir.path.join("py")).expect("mkdir py");
        fs::write(dir.path.join("src/a.ts"), "export const a = 1;\n").expect("write a");
        fs::write(dir.path.join("py/b.py"), "b = 1\n").expect("write b");
        let context_dir = dir.path.join("sieve");

        let only = BuildOptions {
            only_dirs: vec!["src".to_string()],
            no_reuse: false,
            read_only: false,
            progress: None,
        };
        let report = build_graph_cached_with(&dir.path, &context_dir, &only).expect("build");
        assert_eq!(report.claimed, vec!["src/a.ts".to_string()]);
        assert_eq!((report.parsed, report.reused), (1, 0));

        let warm = build_graph_cached_with(&dir.path, &context_dir, &only).expect("build");
        assert_eq!((warm.parsed, warm.reused), (0, 1));

        let cold = BuildOptions {
            only_dirs: Vec::new(),
            no_reuse: true,
            read_only: false,
            progress: None,
        };
        let report = build_graph_cached_with(&dir.path, &context_dir, &cold).expect("build");
        assert_eq!(report.claimed.len(), 2);
        assert_eq!((report.parsed, report.reused), (2, 0));
    }
}
