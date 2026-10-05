//! Edge resolution: turns raw edges into real graph edges. Every rule
//! cites the `edges-ts-py.md` note, section 7.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use sieve_core::lang::lang_for_path;
use sieve_core::{Confidence, Edge, Kind, Node, Relation};

use crate::extract::RawEdge;
use crate::generic::generic_lang_of;

/// Known source extensions `resolveImport` probes, in probe order (edge
/// note, section 3).
const EXTS: [&str; 9] = [
    ".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs", ".py",
];

/// One name-indexed node, kept by reference into `nodes` so the indexes
/// below never copy a `Node`.
type NodeRef<'a> = &'a Node;

/// A Go module discovered in the repo: its `module` path from `go.mod`,
/// and the repo directory (posix, `""` for the repo root) that `go.mod`
/// lives in.
#[derive(Debug, Clone)]
pub struct GoModule {
    pub module: String,
    pub dir: String,
}

/// The per-node-name indexes `resolve_edges` builds once and every
/// relation's resolution reads from.
struct Indexes<'a> {
    node_ids: HashSet<&'a str>,
    global_name: HashMap<&'a str, Vec<NodeRef<'a>>>,
    per_file_name: HashMap<&'a str, HashMap<&'a str, Vec<NodeRef<'a>>>>,
    owner_method: HashMap<String, Vec<NodeRef<'a>>>,
    class_parents: HashMap<String, Vec<String>>,
    /// A PHP class name to the trait names it pulls in with `use`, built
    /// only from `implements` raw edges in `.php` files (edges note,
    /// section 4, `resolveTraitMember`).
    class_traits: HashMap<String, Vec<String>>,
    /// A `.go` file's directory (posix) to the ids of every `.go` file
    /// node in it, for `resolveGoImport`.
    go_files_by_dir: HashMap<String, Vec<String>>,
    /// Every directory-boundary path suffix of a `.java` file
    /// (`com/acme/Foo.java`, `acme/Foo.java`, ...) to its file node ids,
    /// for `resolveJavaImport`.
    java_files_by_suffix: HashMap<String, Vec<String>>,
    /// The same suffix index, for C/C++ header/source files, for
    /// `resolveCInclude`.
    c_files_by_suffix: HashMap<String, Vec<String>>,
    /// The same suffix index, for `.php` files, for `resolvePhpUse`.
    php_files_by_suffix: HashMap<String, Vec<String>>,
    /// Every directory (posix, `""` for the repo root) holding a `lib.rs`
    /// or `main.rs`, for `resolveRustUse`.
    rust_crate_roots: Vec<String>,
    /// Ids of the definitions a `contains` raw edge marks as a direct
    /// named export (TS and JS).
    direct_exports: HashSet<&'a str>,
}

/// Resolves every raw edge into a real graph edge, per the edge note,
/// section 7. Drops a raw edge that resolution cannot place. Equivalent
/// to [`resolve_edges_with_go_modules`] with no discovered Go modules, so
/// a `.go` file's import stays an external package string.
pub fn resolve_edges(nodes: &[Node], raw: &[RawEdge]) -> Vec<Edge> {
    resolve_edges_with_go_modules(nodes, raw, &[])
}

/// Resolves every raw edge into a real graph edge, per the edge note,
/// section 7, with `go_modules` enabling Go module imports for `.go`
/// files. A TS or JS call to a named import resolves through its
/// specifier, and a call bound to a Node built-in module loses its
/// name-matched edge. Drops a raw edge that resolution cannot place.
pub fn resolve_edges_with_go_modules(
    nodes: &[Node],
    raw: &[RawEdge],
    go_modules: &[GoModule],
) -> Vec<Edge> {
    let idx = build_indexes(nodes, raw);

    let mut out: Vec<Edge> = Vec::new();
    let mut seen: HashSet<(String, &'static str, String)> = HashSet::new();
    for raw_edge in raw {
        let Some(edge) = resolve_one(raw_edge, &idx, go_modules) else {
            continue;
        };
        let key = (
            edge.source.clone(),
            edge.relation.as_str(),
            edge.target.clone(),
        );
        if seen.insert(key) {
            out.push(edge);
        }
    }
    out
}

/// Builds the three name indexes and the `classParents` map, per section 7.
fn build_indexes<'a>(nodes: &'a [Node], raw: &'a [RawEdge]) -> Indexes<'a> {
    let node_ids: HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();

    let mut global_name: HashMap<&str, Vec<NodeRef<'a>>> = HashMap::new();
    let mut per_file_name: HashMap<&str, HashMap<&str, Vec<NodeRef<'a>>>> = HashMap::new();
    let mut owner_method: HashMap<String, Vec<NodeRef<'a>>> = HashMap::new();
    let mut id_to_name: HashMap<&str, &str> = HashMap::new();
    let mut go_files_by_dir: HashMap<String, Vec<String>> = HashMap::new();
    let mut java_files_by_suffix: HashMap<String, Vec<String>> = HashMap::new();
    let mut c_files_by_suffix: HashMap<String, Vec<String>> = HashMap::new();
    let mut php_files_by_suffix: HashMap<String, Vec<String>> = HashMap::new();
    let mut rust_crate_roots: Vec<String> = Vec::new();

    for node in nodes {
        if node.kind == Kind::File {
            if node.path.ends_with(".go") {
                let dir = parent_dir(&node.path);
                go_files_by_dir
                    .entry(dir)
                    .or_default()
                    .push(node.id.clone());
            }
            if node.path.ends_with(".java") {
                for suffix in path_suffixes(&node.path) {
                    java_files_by_suffix
                        .entry(suffix)
                        .or_default()
                        .push(node.id.clone());
                }
            }
            if is_c_ext(&node.path) {
                for suffix in path_suffixes(&node.path) {
                    c_files_by_suffix
                        .entry(suffix)
                        .or_default()
                        .push(node.id.clone());
                }
            }
            if node.path.ends_with(".php") {
                for suffix in path_suffixes(&node.path) {
                    php_files_by_suffix
                        .entry(suffix)
                        .or_default()
                        .push(node.id.clone());
                }
            }
            if node.path == "lib.rs" || node.path == "main.rs" {
                rust_crate_roots.push(String::new());
            } else if node.path.ends_with("/lib.rs") || node.path.ends_with("/main.rs") {
                rust_crate_roots.push(parent_dir(&node.path));
            }
            continue;
        }
        id_to_name.insert(node.id.as_str(), node.name.as_str());
        global_name
            .entry(node.name.as_str())
            .or_default()
            .push(node);
        per_file_name
            .entry(node.path.as_str())
            .or_default()
            .entry(node.name.as_str())
            .or_default()
            .push(node);
        // The `ownerMethod` index covers method nodes only. A class, a
        // function, or any other kind never sits in this index, even when it
        // carries an `owner`-shaped id segment.
        if node.kind == Kind::Method {
            if let Some(owner) = owner_of(node) {
                owner_method
                    .entry(format!("{owner}.{}", node.name))
                    .or_default()
                    .push(node);
            }
        }
    }

    let mut class_parents: HashMap<String, Vec<String>> = HashMap::new();
    for edge in raw.iter().filter(|e| e.relation == Relation::Extends) {
        let (Some(class_name), Some(parent_name)) =
            (id_to_name.get(edge.source.as_str()), edge.name.as_deref())
        else {
            continue;
        };
        class_parents
            .entry((*class_name).to_string())
            .or_default()
            .push(parent_name.to_string());
    }

    let trait_names: HashSet<&str> = nodes
        .iter()
        .filter(|n| n.kind == Kind::Trait)
        .map(|n| n.name.as_str())
        .collect();
    let mut class_traits: HashMap<String, Vec<String>> = HashMap::new();
    for edge in raw.iter().filter(|e| e.relation == Relation::Implements) {
        let (Some(class_name), Some(trait_name)) =
            (id_to_name.get(edge.source.as_str()), edge.name.as_deref())
        else {
            continue;
        };
        if !is_php_file(&edge.file) || !trait_names.contains(trait_name) {
            continue;
        }
        class_traits
            .entry((*class_name).to_string())
            .or_default()
            .push(trait_name.to_string());
    }

    let direct_exports: HashSet<&str> = raw
        .iter()
        .filter(|e| e.relation == Relation::Contains && e.direct_export)
        .filter_map(|e| e.target_id.as_deref())
        .collect();

    Indexes {
        direct_exports,
        node_ids,
        global_name,
        per_file_name,
        owner_method,
        class_parents,
        class_traits,
        go_files_by_dir,
        java_files_by_suffix,
        c_files_by_suffix,
        php_files_by_suffix,
        rust_crate_roots,
    }
}

/// The posix directory holding `path`'s last segment, or `""` when `path`
/// has no `/` (the repo root).
fn parent_dir(path: &str) -> String {
    match path.rsplit_once('/') {
        Some((dir, _)) => dir.to_string(),
        None => String::new(),
    }
}

/// Every directory-boundary suffix of `path`'s posix segments, longest
/// first: `com/acme/Foo.java` gives `com/acme/Foo.java`, `acme/Foo.java`,
/// `Foo.java`.
fn path_suffixes(path: &str) -> Vec<String> {
    let parts: Vec<&str> = path.split('/').collect();
    (0..parts.len()).map(|i| parts[i..].join("/")).collect()
}

/// C/C++ source and header extensions `resolveCInclude` matches, case
/// insensitively.
const C_EXTS: [&str; 12] = [
    "c", "h", "cc", "cpp", "cxx", "hpp", "hh", "hxx", "inl", "ipp", "c++", "h++",
];

/// Reports whether `path`'s extension is a C/C++ source or header
/// extension.
fn is_c_ext(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    C_EXTS.iter().any(|ext| lower.ends_with(&format!(".{ext}")))
}

/// Reports whether `path` is a Rust source file, by extension (Rust is a
/// breadth-tier grammar, so `lang_for_path` never claims it).
fn is_rust_file(path: &str) -> bool {
    path.ends_with(".rs")
}

/// Returns a node's owner name: its own `owner` field, else the
/// second-last dotted segment of its id after `#`.
fn owner_of(node: &Node) -> Option<String> {
    if let Some(owner) = &node.owner {
        return Some(owner.clone());
    }
    let after_hash = node.id.split_once('#')?.1;
    let segs: Vec<&str> = after_hash.split('.').collect();
    if segs.len() < 2 {
        return None;
    }
    Some(segs[segs.len() - 2].to_string())
}

/// Resolves one raw edge by its relation.
fn resolve_one(raw: &RawEdge, idx: &Indexes, go_modules: &[GoModule]) -> Option<Edge> {
    match raw.relation {
        Relation::Contains => {
            let target = raw.target_id.clone()?;
            Some(Edge {
                source: raw.source.clone(),
                target,
                relation: Relation::Contains,
                confidence: Confidence::Extracted,
            })
        }
        Relation::Imports => {
            let specifier = raw.specifier.as_deref()?;
            let target = resolve_import_dispatch(idx, go_modules, &raw.file, specifier);
            Some(Edge {
                source: raw.source.clone(),
                target,
                relation: Relation::Imports,
                confidence: Confidence::Extracted,
            })
        }
        Relation::Calls => resolve_calls(raw, idx),
        Relation::Extends => {
            resolve_heritage(raw, Relation::Extends, &[Kind::Class, Kind::Interface], idx)
        }
        Relation::Implements => resolve_heritage(
            raw,
            Relation::Implements,
            &[Kind::Interface, Kind::Trait],
            idx,
        ),
        Relation::References => resolve_reference(raw, idx, go_modules),
    }
}

/// Dispatches a specifier to the resolver its importing file's grammar needs:
/// Go (only once a `go.mod` module was discovered), Java, C/C++, Rust, PHP,
/// else the dot-relative resolver every other grammar shares.
fn resolve_import_dispatch(
    idx: &Indexes,
    go_modules: &[GoModule],
    file: &str,
    specifier: &str,
) -> String {
    if !go_modules.is_empty() && file.ends_with(".go") {
        resolve_go_import(specifier, go_modules, &idx.go_files_by_dir)
    } else if is_java_file(file) {
        resolve_java_import(specifier, &idx.java_files_by_suffix)
    } else if is_c_ext(file) {
        resolve_c_include(specifier, file, &idx.node_ids, &idx.c_files_by_suffix)
    } else if is_rust_file(file) {
        resolve_rust_use(specifier, file, &idx.node_ids, &idx.rust_crate_roots)
    } else if is_php_file(file) {
        resolve_php_use(specifier, &idx.php_files_by_suffix)
    } else {
        resolve_import(&idx.node_ids, file, specifier)
    }
}

/// Resolves a `calls` raw edge: a typed member call via `resolve_typed_member`,
/// else a bare call via `resolve_name`, with a Python `Class` retry on miss.
fn resolve_calls(raw: &RawEdge, idx: &Indexes) -> Option<Edge> {
    let name = raw.name.as_deref()?;
    if !raw.via_member {
        if let Some(specifier) = raw.specifier.as_deref() {
            if is_node_builtin(specifier) {
                return None;
            }
            if let Some(target) = exact_import_target(raw, name, specifier, idx) {
                return Some(Edge {
                    source: raw.source.clone(),
                    target,
                    relation: Relation::Calls,
                    confidence: Confidence::Extracted,
                });
            }
        }
    }
    if raw.via_member {
        let recv_type = raw.recv_type.as_deref()?;
        let (target, confidence) =
            resolve_typed_member(&raw.file, recv_type, name, raw.arg_count, idx)?;
        return Some(Edge {
            source: raw.source.clone(),
            target,
            relation: Relation::Calls,
            confidence,
        });
    }
    let kinds = raw.kinds.clone().unwrap_or_else(|| bare_kinds(&raw.file));
    if let Some((target, confidence)) = resolve_name(&raw.file, name, &kinds, idx) {
        return Some(Edge {
            source: raw.source.clone(),
            target,
            relation: Relation::Calls,
            confidence,
        });
    }
    if is_python_file(&raw.file) {
        if let Some((target, confidence)) = resolve_name(&raw.file, name, &[Kind::Class], idx) {
            return Some(Edge {
                source: raw.source.clone(),
                target,
                relation: Relation::Calls,
                confidence,
            });
        }
    }
    if is_swift_file(&raw.file) {
        let fallback = [Kind::Class, Kind::Struct, Kind::Enum];
        if let Some((target, confidence)) = resolve_name(&raw.file, name, &fallback, idx) {
            return Some(Edge {
                source: raw.source.clone(),
                target,
                relation: Relation::Calls,
                confidence,
            });
        }
    }
    None
}

/// The one exported function `name` that a relative `specifier` names. Gives
/// `None` when the specifier does not resolve to a repo file, or the file
/// holds no exported function of that name, or holds two or more: the
/// caller then keeps the name-matched path.
fn exact_import_target(
    raw: &RawEdge,
    name: &str,
    specifier: &str,
    idx: &Indexes,
) -> Option<String> {
    if !specifier.starts_with('.') {
        return None;
    }
    let file_id = resolve_import(&idx.node_ids, &raw.file, specifier);
    if !idx.node_ids.contains(file_id.as_str()) {
        return None;
    }
    let mut candidates = idx
        .per_file_name
        .get(file_id.as_str())?
        .get(name)?
        .iter()
        .filter(|n| n.kind == Kind::Function && idx.direct_exports.contains(n.id.as_str()));
    let first = candidates.next()?;
    candidates.next().is_none().then(|| first.id.clone())
}

/// Node's built-in modules, bare names. A `node:` specifier also counts.
const NODE_BUILTINS: &[&str] = &[
    "assert",
    "assert/strict",
    "async_hooks",
    "buffer",
    "child_process",
    "cluster",
    "console",
    "constants",
    "crypto",
    "dgram",
    "diagnostics_channel",
    "dns",
    "dns/promises",
    "domain",
    "events",
    "fs",
    "fs/promises",
    "http",
    "http2",
    "https",
    "inspector",
    "inspector/promises",
    "module",
    "net",
    "os",
    "path",
    "path/posix",
    "path/win32",
    "perf_hooks",
    "process",
    "punycode",
    "querystring",
    "readline",
    "readline/promises",
    "repl",
    "stream",
    "stream/consumers",
    "stream/promises",
    "stream/web",
    "string_decoder",
    "sys",
    "timers",
    "timers/promises",
    "tls",
    "trace_events",
    "tty",
    "url",
    "util",
    "util/types",
    "v8",
    "vm",
    "wasi",
    "worker_threads",
    "zlib",
];

/// Reports whether `specifier` names a Node built-in module.
fn is_node_builtin(specifier: &str) -> bool {
    specifier.starts_with("node:") || NODE_BUILTINS.contains(&specifier)
}

/// The kind set a bare call resolves against, by the calling file's
/// grammar: a `.java` file widens to every type-shaped kind, because a
/// bare Java call is a same-name constructor, never a free function
/// (edges note, section 4). Every other known grammar keeps `function`.
/// An explicit `raw.kinds` (R only) overrides this in `resolve_calls`.
fn bare_kinds(file: &str) -> Vec<Kind> {
    if is_java_file(file) {
        vec![Kind::Class, Kind::Struct, Kind::Enum, Kind::Interface]
    } else {
        vec![Kind::Function]
    }
}

/// Resolves an `extends` or `implements` raw edge. An unresolved base
/// keeps the raw name as target, with `Inferred` confidence.
fn resolve_heritage(
    raw: &RawEdge,
    relation: Relation,
    kinds: &[Kind],
    idx: &Indexes,
) -> Option<Edge> {
    let name = raw.name.as_deref()?;
    match resolve_name(&raw.file, name, kinds, idx) {
        Some((target, confidence)) => Some(Edge {
            source: raw.source.clone(),
            target,
            relation,
            confidence,
        }),
        None => Some(Edge {
            source: raw.source.clone(),
            target: name.to_string(),
            relation,
            confidence: Confidence::Inferred,
        }),
    }
}

/// Resolves a `references` raw edge. With a specifier (an import-bound
/// name), the specifier resolves to a file id, then the candidates of
/// that name in that file with no kind filter give the target. With no
/// specifier (generic origin, `languages-lsp.md` line 151), `raw.kinds`
/// names the candidate kind set and [`resolve_name`] finds the target,
/// same-file first, else repo-wide unique. Either path drops a self-loop.
fn resolve_reference(raw: &RawEdge, idx: &Indexes, go_modules: &[GoModule]) -> Option<Edge> {
    let name = raw.name.as_deref()?;
    let (target, confidence) = if let Some(specifier) = raw.specifier.as_deref() {
        let file_id = resolve_import_dispatch(idx, go_modules, &raw.file, specifier);
        if !idx.node_ids.contains(file_id.as_str()) {
            return None;
        }
        let candidates = idx.per_file_name.get(file_id.as_str())?.get(name)?;
        if candidates.len() != 1 {
            return None;
        }
        (candidates[0].id.clone(), Confidence::Extracted)
    } else {
        let kinds = raw.kinds.as_deref()?;
        resolve_name(&raw.file, name, kinds, idx)?
    };
    if raw.source == target {
        return None;
    }
    Some(Edge {
        source: raw.source.clone(),
        target,
        relation: Relation::References,
        confidence,
    })
}

/// `resolveName`: a same-file unique match by kind is `Extracted`; a
/// repo-wide unique reachable match by kind is `Inferred`; else `None`.
fn resolve_name(
    file: &str,
    name: &str,
    kinds: &[Kind],
    idx: &Indexes,
) -> Option<(String, Confidence)> {
    let same_file: Vec<&NodeRef> = idx
        .per_file_name
        .get(file)
        .and_then(|m| m.get(name))
        .map(|v| v.iter().filter(|n| kinds.contains(&n.kind)).collect())
        .unwrap_or_default();
    if same_file.len() == 1 {
        return Some((same_file[0].id.clone(), Confidence::Extracted));
    }
    if !same_file.is_empty() {
        return None;
    }

    let repo_wide: Vec<&NodeRef> = idx
        .global_name
        .get(name)
        .map(|v| {
            v.iter()
                .filter(|n| kinds.contains(&n.kind) && reachable(file, &n.path))
                .collect()
        })
        .unwrap_or_default();
    if repo_wide.len() == 1 {
        return Some((repo_wide[0].id.clone(), Confidence::Inferred));
    }
    None
}

/// `resolveTypedMember`: a breadth-first climb up `classParents`, depth 0
/// to 3 inclusive, cycle-guarded by a visited set.
fn resolve_typed_member(
    file: &str,
    recv_type: &str,
    name: &str,
    arg_count: Option<u32>,
    idx: &Indexes,
) -> Option<(String, Confidence)> {
    let mut frontier = vec![recv_type.to_string()];
    let mut visited: HashSet<String> = HashSet::new();
    for _depth in 0..=3 {
        let mut next_frontier = Vec::new();
        for ty in &frontier {
            if !visited.insert(ty.clone()) {
                continue;
            }
            let key = format!("{ty}.{name}");
            let candidates: Vec<&NodeRef> = idx
                .owner_method
                .get(&key)
                .map(|v| v.iter().filter(|n| reachable(file, &n.path)).collect())
                .unwrap_or_default();
            let candidates = narrow_by_arity(candidates, arg_count);
            if candidates.len() == 1 {
                let confidence = if candidates[0].path == file {
                    Confidence::Extracted
                } else {
                    Confidence::Inferred
                };
                return Some((candidates[0].id.clone(), confidence));
            }
            // Swift: 2+ candidates surviving arity narrowing are always
            // ambiguous, with no same-file fallback.
            if is_swift_file(file) && candidates.len() >= 2 {
                return None;
            }
            if !candidates.is_empty() {
                if let Some(same_file) = candidates.iter().find(|n| n.path == file) {
                    return Some((same_file.id.clone(), Confidence::Extracted));
                }
                return None;
            }
            if let Some(hit) = resolve_trait_member(file, ty, name, idx) {
                return Some(hit);
            }
            if let Some(parents) = idx.class_parents.get(ty) {
                next_frontier.extend(parents.iter().cloned());
            }
        }
        if next_frontier.is_empty() {
            break;
        }
        frontier = next_frontier;
    }
    None
}

/// `narrowByArity`: a no-op when `arg_count` is absent or fewer than two
/// candidates remain. A candidate with no `arity` is always kept. A
/// variadic candidate fits when `arg_count >= arity - 1`; a fixed-arity
/// candidate fits only an exact match. An empty result returns the
/// original set unchanged.
fn narrow_by_arity<'a>(
    candidates: Vec<&'a NodeRef<'a>>,
    arg_count: Option<u32>,
) -> Vec<&'a NodeRef<'a>> {
    let Some(arg_count) = arg_count else {
        return candidates;
    };
    if candidates.len() < 2 {
        return candidates;
    }
    let narrowed: Vec<&NodeRef> = candidates
        .iter()
        .copied()
        .filter(|c| match c.arity {
            None => true,
            Some(arity) => {
                if c.variadic == Some(true) {
                    arg_count >= arity.saturating_sub(1)
                } else {
                    arity == arg_count
                }
            }
        })
        .collect();
    if narrowed.is_empty() {
        candidates
    } else {
        narrowed
    }
}

/// `resolveTraitMember`: a PHP class's own traits (`classTraits`, built
/// from `implements` raw edges) each get one direct `owner_method`
/// lookup, no further climb, before `resolve_typed_member` climbs to a
/// parent class (edges note, section 4).
fn resolve_trait_member(
    file: &str,
    class_name: &str,
    name: &str,
    idx: &Indexes,
) -> Option<(String, Confidence)> {
    let traits = idx.class_traits.get(class_name)?;
    for trait_name in traits {
        let key = format!("{trait_name}.{name}");
        let candidates: Vec<&NodeRef> = idx
            .owner_method
            .get(&key)
            .map(|v| v.iter().filter(|n| reachable(file, &n.path)).collect())
            .unwrap_or_default();
        if candidates.len() == 1 {
            let confidence = if candidates[0].path == file {
                Confidence::Extracted
            } else {
                Confidence::Inferred
            };
            return Some((candidates[0].id.clone(), confidence));
        }
    }
    None
}

/// `reachable`: two grammars filter each other only inside a known
/// family; `[typescript, tsx]` is one family, `[java, kotlin, scala,
/// clojure]` is one family, `[c, cpp]` is one family. Every other
/// recognized grammar is its own singleton family: a Go file cannot
/// reach a Swift name, and a Swift file cannot reach an R name, even
/// though nothing groups Go, Swift, or R with anything else. An
/// unrecognized path (`lang_for_path` finds no grammar at all) never
/// filters: absence of data is not evidence of a mismatch.
fn reachable(from_path: &str, to_path: &str) -> bool {
    match (family(from_path), family(to_path)) {
        (Some(a), Some(b)) => a == b,
        _ => true,
    }
}

/// The resolution family of a file path's grammar: the grouped family
/// name for `[typescript, tsx]`, `[java, kotlin, scala, clojure]`, and
/// `[c, cpp]`; the grammar's own name otherwise; `None` only when no
/// grammar recognizes the path at all (edges note, section 4, "Families").
/// A breadth-tier path (`generic_lang_of`) is its own singleton family
/// too, so a Ruby name and a Lua name never cross-reach, even though
/// neither is a depth-tier grammar.
fn family(path: &str) -> Option<&'static str> {
    let grammar = lang_for_path(Path::new(path))
        .map(|lang| lang.grammar)
        .or_else(|| generic_lang_of(path).map(|lang| lang.name))?;
    Some(match grammar {
        "typescript" | "tsx" => "typescript",
        "java" | "kotlin" | "scala" | "clojure" => "java",
        "c" | "cpp" => "c",
        other => other,
    })
}

/// Reports whether `path`'s grammar is Python.
fn is_python_file(path: &str) -> bool {
    lang_for_path(Path::new(path)).is_some_and(|l| l.grammar == "python")
}

/// Reports whether `path`'s grammar is Java.
fn is_java_file(path: &str) -> bool {
    lang_for_path(Path::new(path)).is_some_and(|l| l.grammar == "java")
}

/// Reports whether `path`'s grammar is PHP.
fn is_php_file(path: &str) -> bool {
    lang_for_path(Path::new(path)).is_some_and(|l| l.grammar == "php")
}

/// Reports whether `path`'s grammar is Swift.
fn is_swift_file(path: &str) -> bool {
    lang_for_path(Path::new(path)).is_some_and(|l| l.grammar == "swift")
}

/// `resolveImport`: a bare specifier stays raw; a dot-leading specifier
/// joins against the importing file's directory, strips one known
/// extension, then probes the path, `+ext`, and `/index+ext`. The first
/// existing node id wins; else the raw specifier stays.
fn resolve_import(node_ids: &HashSet<&str>, from_path: &str, specifier: &str) -> String {
    if !specifier.starts_with('.') {
        return specifier.to_string();
    }
    let joined = join_normalized(from_path, specifier);
    let stripped = strip_known_ext(&joined);

    if node_ids.contains(stripped.as_str()) {
        return stripped;
    }
    for ext in EXTS {
        let candidate = format!("{stripped}{ext}");
        if node_ids.contains(candidate.as_str()) {
            return candidate;
        }
    }
    for ext in EXTS {
        let candidate = format!("{stripped}/index{ext}");
        if node_ids.contains(candidate.as_str()) {
            return candidate;
        }
    }
    specifier.to_string()
}

/// Joins `from_path`'s directory with `specifier`, then normalizes `.`
/// and `..` segments. A segment such as `.rel` is not special; it joins
/// literally.
fn join_normalized(from_path: &str, specifier: &str) -> String {
    let dir = Path::new(from_path)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let joined = if dir.is_empty() {
        specifier.to_string()
    } else {
        format!("{dir}/{specifier}")
    };
    let mut parts: Vec<&str> = Vec::new();
    for seg in joined.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// Strips one known extension from `path`, if it ends with one.
fn strip_known_ext(path: &str) -> String {
    for ext in EXTS {
        if let Some(stripped) = path.strip_suffix(ext) {
            return stripped.to_string();
        }
    }
    path.to_string()
}

/// Joins two posix directory strings (either may be empty for the repo
/// root), then normalizes `.` and `..` segments.
fn join_dirs(a: &str, b: &str) -> String {
    let joined = match (a.is_empty(), b.is_empty()) {
        (true, true) => String::new(),
        (true, false) => b.to_string(),
        (false, true) => a.to_string(),
        (false, false) => format!("{a}/{b}"),
    };
    let mut parts: Vec<&str> = Vec::new();
    for seg in joined.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// `resolveGoImport`: a Go import package path resolves to the in-repo
/// directory of the module whose path is its longest-matching prefix,
/// then to a deterministic (lowest-sorted) `.go` file node in that
/// directory. Stdlib, third-party, or a directory with no claimed `.go`
/// file stays the raw package path.
fn resolve_go_import(
    spec: &str,
    modules: &[GoModule],
    files_by_dir: &HashMap<String, Vec<String>>,
) -> String {
    let mut best: Option<(&GoModule, String)> = None;
    for module in modules {
        let subpath = if spec == module.module {
            Some(String::new())
        } else {
            spec.strip_prefix(&format!("{}/", module.module))
                .map(|rest| rest.to_string())
        };
        let Some(subpath) = subpath else {
            continue;
        };
        if best
            .as_ref()
            .is_none_or(|(m, _)| module.module.len() > m.module.len())
        {
            best = Some((module, subpath));
        }
    }
    let Some((module, subpath)) = best else {
        return spec.to_string();
    };
    let dir = join_dirs(&module.dir, &subpath);
    let Some(files) = files_by_dir.get(&dir) else {
        return spec.to_string();
    };
    if files.is_empty() {
        return spec.to_string();
    }
    let mut sorted = files.clone();
    sorted.sort();
    sorted.remove(0)
}

/// `resolveJavaImport`: a fully-qualified Java type name resolves to the
/// in-repo file whose path-suffix uniquely matches it; `import static
/// a.b.C.member` retries as the enclosing type `a.b.C` on a miss. A
/// wildcard, an ambiguous suffix, or a JDK/third-party type stays the raw
/// specifier.
fn resolve_java_import(spec: &str, files_by_suffix: &HashMap<String, Vec<String>>) -> String {
    let hit = |fqn: &str| -> Option<String> {
        let suffix = format!("{}.java", fqn.replace('.', "/"));
        let files = files_by_suffix.get(&suffix)?;
        (files.len() == 1).then(|| files[0].clone())
    };
    if let Some(direct) = hit(spec) {
        return direct;
    }
    if let Some(dot) = spec.rfind('.') {
        if dot > 0 {
            if let Some(enclosing) = hit(&spec[..dot]) {
                return enclosing;
            }
        }
    }
    spec.to_string()
}

/// `resolveCInclude`: a `#include "path"` resolves relative to the
/// including file first (certain), else by a unique path-suffix match,
/// which covers a header reached through an `-I` include directory. A
/// system header, an ambiguous suffix, or a miss stays the raw path.
fn resolve_c_include(
    spec: &str,
    file: &str,
    node_ids: &HashSet<&str>,
    files_by_suffix: &HashMap<String, Vec<String>>,
) -> String {
    let rel_join = join_normalized(file, spec);
    if node_ids.contains(rel_join.as_str()) {
        return rel_join;
    }
    let trimmed = spec
        .strip_prefix("./")
        .or_else(|| spec.strip_prefix('/'))
        .unwrap_or(spec);
    if let Some(hits) = files_by_suffix.get(trimmed) {
        if hits.len() == 1 {
            return hits[0].clone();
        }
    }
    spec.to_string()
}

/// `resolvePhpUse`: a PHP `use App\Models\User` resolves to the in-repo
/// class file whose path-suffix is the longest namespace tail that
/// uniquely names one file (`App/Models/User.php`, then
/// `Models/User.php`, then `User.php`). An ambiguous tail at any level, or
/// a vendor/out-of-repo class, stays the raw name.
fn resolve_php_use(fqn: &str, files_by_suffix: &HashMap<String, Vec<String>>) -> String {
    let parts: Vec<&str> = fqn.split('\\').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return fqn.to_string();
    }
    for i in 0..parts.len() {
        let suffix = format!("{}.php", parts[i..].join("/"));
        let Some(hits) = files_by_suffix.get(&suffix) else {
            continue;
        };
        if hits.len() == 1 {
            return hits[0].clone();
        }
        if hits.len() > 1 {
            break; // ambiguous at the most specific level — do not guess
        }
    }
    fqn.to_string()
}

/// `resolveRustUse`: a `crate`-relative module path (`crate/a/b`, from
/// `use crate::a::b::Item`) resolves to `<crate root>/a/b.rs` or
/// `.../a/b/mod.rs`, where the crate root is the longest `lib.rs`/`main.rs`
/// directory that contains the importing file. No owning crate root, no
/// module match, or an ambiguous level keeps the `crate::...` string.
fn resolve_rust_use(
    spec: &str,
    file: &str,
    node_ids: &HashSet<&str>,
    crate_roots: &[String],
) -> String {
    let full = spec
        .strip_prefix("crate")
        .map_or(spec, |rest| rest.strip_prefix('/').unwrap_or(rest));
    let as_crate_string = |full: &str| -> String {
        if full.is_empty() {
            "crate".to_string()
        } else {
            format!("crate::{}", full.replace('/', "::"))
        }
    };

    let mut owning: Vec<&String> = crate_roots
        .iter()
        .filter(|root| {
            root.is_empty() || file == root.as_str() || file.starts_with(&format!("{root}/"))
        })
        .collect();
    owning.sort_by_key(|a| std::cmp::Reverse(a.len()));
    let Some(root) = owning.first() else {
        return as_crate_string(full);
    };

    let segs: Vec<&str> = if full.is_empty() {
        Vec::new()
    } else {
        full.split('/').collect()
    };
    let hits_for = |rels: &[String]| -> HashSet<String> {
        rels.iter()
            .map(|rel| join_dirs(root, rel))
            .filter(|cand| node_ids.contains(cand.as_str()))
            .collect()
    };
    for k in (1..=segs.len()).rev() {
        let mod_path = segs[..k].join("/");
        let hits = hits_for(&[format!("{mod_path}.rs"), format!("{mod_path}/mod.rs")]);
        if hits.len() == 1 {
            return hits.into_iter().next().unwrap();
        }
        if hits.len() > 1 {
            break; // ambiguous at this level — do not guess
        }
    }
    if segs.len() <= 1 {
        let hits = hits_for(&["lib.rs".to_string(), "main.rs".to_string()]);
        if hits.len() == 1 {
            return hits.into_iter().next().unwrap();
        }
    }
    as_crate_string(full)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sieve_core::{span, Origin, SummaryState};

    fn node(id: &str, name: &str, kind: Kind, path: &str, owner: Option<&str>) -> Node {
        Node {
            id: id.to_string(),
            name: name.to_string(),
            kind,
            path: path.to_string(),
            span: span(1, 2),
            signature: None,
            exported: true,
            origin: Origin::Ast,
            body_hash: "0".repeat(64),
            chars: None,
            body_text: None,
            summary_state: SummaryState::Pending,
            summary: None,
            crux: None,
            owner: owner.map(str::to_string),
            arity: None,
            variadic: None,
        }
    }

    fn call_edge(source: &str, file: &str, name: &str) -> RawEdge {
        RawEdge {
            source: source.to_string(),
            relation: Relation::Calls,
            file: file.to_string(),
            target_id: None,
            specifier: None,
            name: Some(name.to_string()),
            via_member: false,
            recv_type: None,
            arg_count: None,
            kinds: None,
            implicit_self: false,
            direct_export: false,
        }
    }

    #[test]
    fn two_same_file_candidates_drop_the_call_edge() {
        let nodes = vec![
            node("a.ts#f", "f", Kind::Function, "a.ts", None),
            node("a.ts#f~2", "f", Kind::Function, "a.ts", None),
        ];
        let raw = vec![call_edge("a.ts#caller", "a.ts", "f")];
        assert!(resolve_edges(&nodes, &raw).is_empty());
    }

    #[test]
    fn repo_wide_unique_gives_inferred() {
        let nodes = vec![node("b.ts#f", "f", Kind::Function, "b.ts", None)];
        let raw = vec![call_edge("a.ts#caller", "a.ts", "f")];
        let edges = resolve_edges(&nodes, &raw);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].confidence, Confidence::Inferred);
        assert_eq!(edges[0].target, "b.ts#f");
    }

    #[test]
    fn python_class_retry_resolves_a_class_call() {
        let nodes = vec![node("a.py#Box", "Box", Kind::Class, "a.py", None)];
        let raw = vec![call_edge("a.py#caller", "a.py", "Box")];
        let edges = resolve_edges(&nodes, &raw);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target, "a.py#Box");
        assert_eq!(edges[0].confidence, Confidence::Extracted);
    }

    #[test]
    fn owner_method_index_ignores_a_non_method_node_with_a_dotted_id() {
        // A nested function's id looks like `Box.helper`, the same shape
        // `owner_of`'s id fallback reads for a method. `owner_method` must
        // still skip it, because it is not a method.
        let nodes = vec![node(
            "a.ts#Box.helper",
            "helper",
            Kind::Function,
            "a.ts",
            None,
        )];
        let raw = vec![RawEdge {
            source: "a.ts#caller".to_string(),
            relation: Relation::Calls,
            file: "a.ts".to_string(),
            target_id: None,
            specifier: None,
            name: Some("helper".to_string()),
            via_member: true,
            recv_type: Some("Box".to_string()),
            arg_count: None,
            kinds: None,
            implicit_self: false,
            direct_export: false,
        }];
        assert!(resolve_edges(&nodes, &raw).is_empty());
    }

    #[test]
    fn a_member_call_with_no_recv_type_drops() {
        let nodes = vec![node(
            "a.ts#Box.run",
            "run",
            Kind::Method,
            "a.ts",
            Some("Box"),
        )];
        let raw = vec![RawEdge {
            source: "a.ts#caller".to_string(),
            relation: Relation::Calls,
            file: "a.ts".to_string(),
            target_id: None,
            specifier: None,
            name: Some("run".to_string()),
            via_member: true,
            recv_type: None,
            arg_count: None,
            kinds: None,
            implicit_self: false,
            direct_export: false,
        }];
        assert!(resolve_edges(&nodes, &raw).is_empty());
    }

    #[test]
    fn depth_three_climb_finds_a_method_and_depth_four_does_not() {
        let mut nodes = vec![node("a.ts#T0.m", "m", Kind::Method, "a.ts", Some("T0"))];
        let mut raw = vec![RawEdge {
            source: "a.ts#caller".to_string(),
            relation: Relation::Calls,
            file: "a.ts".to_string(),
            target_id: None,
            specifier: None,
            name: Some("m".to_string()),
            via_member: true,
            recv_type: Some("T3".to_string()),
            arg_count: None,
            kinds: None,
            implicit_self: false,
            direct_export: false,
        }];
        for i in (0..3).rev() {
            raw.push(RawEdge {
                source: format!("a.ts#T{}", i + 1),
                relation: Relation::Extends,
                file: "a.ts".to_string(),
                target_id: None,
                specifier: None,
                name: Some(format!("T{i}")),
                via_member: false,
                recv_type: None,
                arg_count: None,
                kinds: None,
                implicit_self: false,
                direct_export: false,
            });
        }
        for i in 1..=3 {
            nodes.push(node(
                &format!("a.ts#T{i}"),
                &format!("T{i}"),
                Kind::Class,
                "a.ts",
                None,
            ));
        }
        let edges = resolve_edges(&nodes, &raw);
        let call = edges
            .iter()
            .find(|e| e.relation == Relation::Calls)
            .expect("depth-3 climb should find the method");
        assert_eq!(call.target, "a.ts#T0.m");

        // Depth 4: T4 -> T3 -> T2 -> T1 -> T0, the method sits one level too far.
        raw.push(RawEdge {
            source: "a.ts#T4".to_string(),
            relation: Relation::Extends,
            file: "a.ts".to_string(),
            target_id: None,
            specifier: None,
            name: Some("T3".to_string()),
            via_member: false,
            recv_type: None,
            arg_count: None,
            kinds: None,
            implicit_self: false,
            direct_export: false,
        });
        nodes.push(node("a.ts#T4", "T4", Kind::Class, "a.ts", None));
        raw[0].recv_type = Some("T4".to_string());
        let edges = resolve_edges(&nodes, &raw);
        assert!(!edges.iter().any(|e| e.relation == Relation::Calls));
    }

    #[test]
    fn two_candidates_at_one_level_with_none_same_file_stop_the_climb() {
        let nodes = vec![
            node("a.ts#T0.m", "m", Kind::Method, "a.ts", Some("T0")),
            node("b.ts#T0.m", "m", Kind::Method, "b.ts", Some("T0")),
        ];
        let raw = vec![RawEdge {
            source: "c.ts#caller".to_string(),
            relation: Relation::Calls,
            file: "c.ts".to_string(),
            target_id: None,
            specifier: None,
            name: Some("m".to_string()),
            via_member: true,
            recv_type: Some("T0".to_string()),
            arg_count: None,
            kinds: None,
            implicit_self: false,
            direct_export: false,
        }];
        assert!(resolve_edges(&nodes, &raw).is_empty());
    }

    #[test]
    fn dedupe_keeps_the_first_confidence() {
        let nodes = vec![
            node("a.ts#f", "f", Kind::Function, "a.ts", None),
            node("b.ts#f", "f", Kind::Function, "b.ts", None),
        ];
        // Same-file first: extracted. A later raw edge with the same key
        // would resolve differently, but dedupe keeps the first.
        let raw = vec![
            call_edge("a.ts#caller", "a.ts", "f"),
            call_edge("a.ts#caller", "a.ts", "f"),
        ];
        let edges = resolve_edges(&nodes, &raw);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].confidence, Confidence::Extracted);
        assert_eq!(edges[0].target, "a.ts#f");
    }

    #[test]
    fn relative_import_probes_dot_ts_then_index_dot_ts() {
        let nodes = [node(
            "src/util.ts",
            "util.ts",
            Kind::File,
            "src/util.ts",
            None,
        )];
        let node_ids: HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(
            resolve_import(&node_ids, "src/app.ts", "./util"),
            "src/util.ts"
        );

        let nodes = [node(
            "src/util/index.ts",
            "index.ts",
            Kind::File,
            "src/util/index.ts",
            None,
        )];
        let node_ids: HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(
            resolve_import(&node_ids, "src/app.ts", "./util"),
            "src/util/index.ts"
        );
    }

    fn java_method_node(id: &str, name: &str, owner: &str, arity: u32, variadic: bool) -> Node {
        let mut n = node(id, name, Kind::Method, "a.java", Some(owner));
        n.arity = Some(arity);
        n.variadic = variadic.then_some(true);
        n
    }

    #[test]
    fn java_bare_new_resolves_to_the_class_node() {
        let nodes = vec![node("a.java#App", "App", Kind::Class, "a.java", None)];
        let raw = vec![RawEdge {
            source: "a.java#caller".to_string(),
            relation: Relation::Calls,
            file: "a.java".to_string(),
            target_id: None,
            specifier: None,
            name: Some("App".to_string()),
            via_member: false,
            recv_type: None,
            arg_count: Some(1),
            kinds: None,
            implicit_self: false,
            direct_export: false,
        }];
        let edges = resolve_edges(&nodes, &raw);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target, "a.java#App");
    }

    #[test]
    fn narrow_by_arity_keeps_only_the_matching_fixed_arity_overload() {
        let one = java_method_node("a.java#T.m1", "m", "T", 1, false);
        let two = java_method_node("a.java#T.m2", "m", "T", 2, false);
        let (one_ref, two_ref): (&Node, &Node) = (&one, &two);
        let narrowed = narrow_by_arity(vec![&one_ref, &two_ref], Some(2));
        assert_eq!(narrowed.len(), 1);
        assert_eq!(narrowed[0].id, "a.java#T.m2");
    }

    #[test]
    fn narrow_by_arity_keeps_a_variadic_overload_at_or_above_arity_minus_one() {
        let fixed = java_method_node("a.java#T.m1", "m", "T", 3, false);
        let variadic = java_method_node("a.java#T.m2", "m", "T", 1, true);
        let (fixed_ref, variadic_ref): (&Node, &Node) = (&fixed, &variadic);
        let narrowed = narrow_by_arity(vec![&fixed_ref, &variadic_ref], Some(1));
        assert_eq!(narrowed.len(), 1);
        assert_eq!(narrowed[0].id, "a.java#T.m2");
    }

    #[test]
    fn narrow_by_arity_is_a_no_op_below_two_candidates_or_with_no_arg_count() {
        let one = java_method_node("a.java#T.m1", "m", "T", 1, false);
        let two = java_method_node("a.java#T.m2", "m", "T", 2, false);
        let (one_ref, two_ref): (&Node, &Node) = (&one, &two);
        assert_eq!(narrow_by_arity(vec![&one_ref], Some(9)).len(), 1);
        assert_eq!(narrow_by_arity(vec![&one_ref, &two_ref], None).len(), 2);
    }

    #[test]
    fn resolve_trait_member_finds_a_method_the_class_pulls_in_with_use() {
        let nodes = vec![
            node("a.php#App", "App", Kind::Class, "a.php", None),
            node("a.php#Named", "Named", Kind::Trait, "a.php", None),
            node(
                "a.php#Named.label",
                "label",
                Kind::Method,
                "a.php",
                Some("Named"),
            ),
        ];
        let raw = vec![RawEdge {
            source: "a.php#App".to_string(),
            relation: Relation::Implements,
            file: "a.php".to_string(),
            target_id: None,
            specifier: None,
            name: Some("Named".to_string()),
            via_member: false,
            recv_type: None,
            arg_count: None,
            kinds: None,
            implicit_self: false,
            direct_export: false,
        }];
        let idx = build_indexes(&nodes, &raw);
        let hit = resolve_trait_member("a.php", "App", "label", &idx);
        assert_eq!(hit.map(|(id, _)| id), Some("a.php#Named.label".to_string()));
    }

    /// A file→file `imports` raw edge, the shape both the native and the
    /// generic extractors emit (`source == file`).
    fn import_edge(file: &str, specifier: &str) -> RawEdge {
        RawEdge {
            source: file.to_string(),
            relation: Relation::Imports,
            file: file.to_string(),
            target_id: None,
            specifier: Some(specifier.to_string()),
            name: None,
            via_member: false,
            recv_type: None,
            arg_count: None,
            kinds: None,
            implicit_self: false,
            direct_export: false,
        }
    }

    fn file_node(path: &str) -> Node {
        node(path, path, Kind::File, path, None)
    }

    #[test]
    fn go_import_resolves_to_the_module_relative_package_dir() {
        let nodes = vec![file_node("go/util/util.go")];
        let modules = vec![GoModule {
            module: "example.com/app".to_string(),
            dir: "go".to_string(),
        }];
        let raw = vec![import_edge("go/main.go", "example.com/app/util")];
        let edges = resolve_edges_with_go_modules(&nodes, &raw, &modules);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target, "go/util/util.go");
    }

    #[test]
    fn go_import_with_no_matching_module_stays_the_raw_specifier() {
        let modules = vec![GoModule {
            module: "example.com/app".to_string(),
            dir: "go".to_string(),
        }];
        let raw = vec![import_edge("go/main.go", "fmt")];
        let edges = resolve_edges_with_go_modules(&[], &raw, &modules);
        assert_eq!(edges[0].target, "fmt");
    }

    #[test]
    fn java_import_resolves_by_the_longest_matching_path_suffix() {
        let nodes = vec![file_node("com/acme/Util.java")];
        let raw = vec![import_edge("java/App.java", "com.acme.Util")];
        let edges = resolve_edges(&nodes, &raw);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target, "com/acme/Util.java");
    }

    #[test]
    fn java_static_import_retries_the_enclosing_type() {
        let nodes = vec![file_node("com/acme/Util.java")];
        let raw = vec![import_edge("java/App.java", "com.acme.Util.CONST")];
        let edges = resolve_edges(&nodes, &raw);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target, "com/acme/Util.java");
    }

    #[test]
    fn java_wildcard_import_stays_the_raw_package() {
        let nodes = vec![file_node("com/acme/Util.java")];
        let raw = vec![import_edge("java/App.java", "com.acme.*")];
        let edges = resolve_edges(&nodes, &raw);
        assert_eq!(edges[0].target, "com.acme.*");
    }

    #[test]
    fn c_include_resolves_relative_to_the_including_file_first() {
        let nodes = vec![file_node("c/inc/b.h")];
        let raw = vec![import_edge("c/a.c", "inc/b.h")];
        let edges = resolve_edges(&nodes, &raw);
        assert_eq!(edges[0].target, "c/inc/b.h");
    }

    #[test]
    fn c_include_falls_back_to_a_unique_path_suffix() {
        let nodes = vec![file_node("c/inc/b.h")];
        let raw = vec![import_edge("c/a.c", "b.h")];
        let edges = resolve_edges(&nodes, &raw);
        assert_eq!(edges[0].target, "c/inc/b.h");
    }

    #[test]
    fn c_include_of_a_system_header_stays_the_raw_path() {
        let raw = vec![import_edge("c/a.c", "stdio.h")];
        let edges = resolve_edges(&[], &raw);
        assert_eq!(edges[0].target, "stdio.h");
    }

    #[test]
    fn php_use_resolves_by_the_longest_namespace_tail() {
        let nodes = vec![file_node("App/Models/User.php")];
        let raw = vec![import_edge("php/App.php", "App\\Models\\User")];
        let edges = resolve_edges(&nodes, &raw);
        assert_eq!(edges[0].target, "App/Models/User.php");
    }

    #[test]
    fn php_use_of_a_vendor_class_stays_the_raw_name() {
        let nodes = vec![file_node("App/Models/User.php")];
        let raw = vec![import_edge("php/App.php", "Foo\\Bar")];
        let edges = resolve_edges(&nodes, &raw);
        assert_eq!(edges[0].target, "Foo\\Bar");
    }

    #[test]
    fn rust_use_crate_resolves_relative_to_the_crate_root() {
        let nodes = vec![file_node("lib.rs"), file_node("a/b.rs")];
        let raw = vec![import_edge("lib.rs", "crate/a/b")];
        let edges = resolve_edges(&nodes, &raw);
        assert_eq!(edges[0].target, "a/b.rs");
    }

    #[test]
    fn rust_use_with_no_module_match_stays_the_crate_string() {
        let nodes = vec![file_node("lib.rs"), file_node("a/b.rs")];
        let raw = vec![import_edge("lib.rs", "crate/a/missing")];
        let edges = resolve_edges(&nodes, &raw);
        assert_eq!(edges[0].target, "crate::a::missing");
    }
}

#[cfg(test)]
mod xfile_tests {
    use super::*;
    use crate::extract::Extractor;
    use sieve_core::{span, Origin, SummaryState};

    fn file_node(path: &str) -> Node {
        Node {
            id: path.to_string(),
            name: path.to_string(),
            kind: Kind::File,
            path: path.to_string(),
            span: span(1, 1),
            signature: None,
            exported: false,
            origin: Origin::Ast,
            body_hash: "0".repeat(64),
            chars: None,
            body_text: None,
            summary_state: SummaryState::Pending,
            summary: None,
            crux: None,
            owner: None,
            arity: None,
            variadic: None,
        }
    }

    type Hit = Option<(String, Confidence)>;

    /// Extracts every `(path, source)` file, resolves the edges, and returns the one `calls` edge that `main.ts` makes.
    fn main_call(files: &[(&str, &str)]) -> Hit {
        let mut extractor = Extractor::new().expect("build extractor");
        let mut nodes = Vec::new();
        let mut raw = Vec::new();
        for (path, source) in files {
            nodes.push(file_node(path));
            let (n, r) = extractor.extract_file(path, source, "typescript");
            nodes.extend(n);
            raw.extend(r);
        }
        resolve_edges(&nodes, &raw)
            .into_iter()
            .find(|e| e.relation == Relation::Calls && e.source.starts_with("main.ts#"))
            .map(|e| (e.target, e.confidence))
    }

    /// How many Function nodes named `helper` the files define.
    fn helper_count(files: &[(&str, &str)]) -> usize {
        let mut extractor = Extractor::new().expect("build extractor");
        files
            .iter()
            .flat_map(|(path, source)| extractor.extract_file(path, source, "typescript").0)
            .filter(|n| n.kind == Kind::Function && n.name == "helper")
            .count()
    }

    fn hit(target: &str, confidence: Confidence) -> Hit {
        Some((target.to_string(), confidence))
    }

    const LIB: (&str, &str) = ("lib.ts", "export function helper(): number { return 1; }\n");
    const OTHER: (&str, &str) = (
        "other.ts",
        "export function helper(): number { return 2; }\n",
    );

    /// A `main.ts` whose `run` calls `helper()` imported from `spec`.
    fn main_src(spec: &str) -> String {
        format!(
            "import {{ helper }} from \"{spec}\";\nexport function run() {{ return helper(); }}\n"
        )
    }

    #[test]
    fn test_xfile_named_relative_import_is_extracted_under_sieve_only() {
        let src = main_src("./lib");
        let files = [LIB, ("main.ts", src.as_str())];
        assert_eq!(
            main_call(&files),
            hit("lib.ts#helper", Confidence::Extracted)
        );
    }

    #[test]
    fn test_xfile_two_same_name_files_pick_the_imported_file() {
        let src = main_src("./lib");
        let files = [LIB, OTHER, ("main.ts", src.as_str())];
        assert_eq!(
            main_call(&files),
            hit("lib.ts#helper", Confidence::Extracted)
        );
    }

    #[test]
    fn test_xfile_aliased_import_is_unchanged() {
        let src = "import { helper as h } from \"./lib\";\nexport function run() { return h(); }\n";
        let files = [LIB, ("main.ts", src)];
        assert_eq!(main_call(&files), None);
    }

    #[test]
    fn test_xfile_barrel_with_zero_candidates_keeps_the_name_matched_result() {
        let barrel = ("barrel.ts", "export { helper } from \"./lib\";\n");
        let src = main_src("./barrel");
        let files = [LIB, barrel, ("main.ts", src.as_str())];
        assert_eq!(
            main_call(&files),
            hit("lib.ts#helper", Confidence::Inferred)
        );
    }

    #[test]
    fn test_xfile_non_exported_function_is_not_promoted() {
        let hidden = (
            "hidden.ts",
            "function helper(): number { return 3; }\nexport const x = 1;\n",
        );
        let src = main_src("./hidden");
        let files = [hidden, ("main.ts", src.as_str())];
        assert_eq!(
            main_call(&files),
            hit("hidden.ts#helper", Confidence::Inferred)
        );
    }

    #[test]
    fn test_xfile_two_candidates_in_the_file_keep_the_name_matched_path() {
        let twice = (
            "twice.ts",
            "export function helper() { return 1; }\nexport const helper = () => 2;\n",
        );
        let src = main_src("./twice");
        let files = [twice, ("main.ts", src.as_str())];
        assert_eq!(helper_count(&files), 2);
        assert_ne!(
            main_call(&files).map(|(_, c)| c),
            Some(Confidence::Extracted)
        );
    }

    #[test]
    fn test_xfile_arrow_const_function_is_promoted() {
        let arrow = ("arrow.ts", "export const helper = () => 1;\n");
        let src = main_src("./arrow");
        let files = [arrow, ("main.ts", src.as_str())];
        assert_eq!(
            main_call(&files),
            hit("arrow.ts#helper", Confidence::Extracted)
        );
    }

    #[test]
    fn test_xfile_node_builtin_call_loses_its_repo_edge_under_sieve_only() {
        let local = (
            "util.ts",
            "export function helper(): string { return \"a\"; }\n",
        );
        for spec in [
            "node:path",
            "path",
            "timers/promises",
            "fs/promises",
            "child_process",
            "worker_threads",
        ] {
            let src = main_src(spec);
            let files = [local, ("main.ts", src.as_str())];
            assert_eq!(main_call(&files), None, "{spec}");
        }
    }

    #[test]
    fn test_xfile_package_specifiers_keep_the_name_matched_path() {
        for spec in ["react", "@/x", "@scope/pkg"] {
            let src = main_src(spec);
            let files = [LIB, ("main.ts", src.as_str())];
            assert_eq!(
                main_call(&files),
                hit("lib.ts#helper", Confidence::Inferred),
                "{spec}"
            );
        }
    }

    #[test]
    fn test_xfile_callback_parameter_shadow_gives_no_extracted_edge() {
        let shadowed = "import { helper } from \"./lib\";\nexport function run(xs: Array<() => number>) { return xs.map((helper) => helper()); }\n";
        let files = [LIB, ("main.ts", shadowed)];
        assert_ne!(
            main_call(&files).map(|(_, c)| c),
            Some(Confidence::Extracted)
        );
        let plain = "import { helper } from \"./lib\";\nexport function run(xs: number[]) { return xs.map((x) => helper()); }\n";
        let files = [LIB, ("main.ts", plain)];
        assert_eq!(
            main_call(&files),
            hit("lib.ts#helper", Confidence::Extracted)
        );
    }

    #[test]
    fn test_xfile_nested_function_in_an_exported_function_is_not_promoted() {
        let outer = (
            "outer.ts",
            "export function outer() {\n  function helper() { return 1; }\n  return helper();\n}\n",
        );
        let src = main_src("./outer");
        let files = [outer, ("main.ts", src.as_str())];
        assert_eq!(helper_count(&files), 1);
        assert_ne!(
            main_call(&files).map(|(_, c)| c),
            Some(Confidence::Extracted)
        );
    }

    #[test]
    fn test_xfile_default_export_imported_by_name_is_not_promoted() {
        let def = (
            "def.ts",
            "export default function helper(): number { return 1; }\n",
        );
        let src = main_src("./def");
        let files = [def, ("main.ts", src.as_str())];
        assert_eq!(helper_count(&files), 1);
        assert_ne!(
            main_call(&files).map(|(_, c)| c),
            Some(Confidence::Extracted)
        );
    }

    #[test]
    fn test_xfile_javascript_files_resolve_like_typescript() {
        let lib = ("lib.js", "export function helper() { return 1; }\n");
        let src = main_src("./lib.js");
        let files = [lib, ("main.js", src.as_str())];
        let call = || {
            let mut extractor = Extractor::new().expect("build extractor");
            let mut nodes = Vec::new();
            let mut raw = Vec::new();
            for (path, source) in files {
                nodes.push(file_node(path));
                let (n, r) = extractor.extract_file(path, source, "typescript");
                nodes.extend(n);
                raw.extend(r);
            }
            resolve_edges(&nodes, &raw)
                .into_iter()
                .find(|e| e.relation == Relation::Calls)
                .map(|e| e.confidence)
        };
        assert_eq!(call(), Some(Confidence::Extracted));
    }

    #[test]
    fn test_xfile_for_of_and_catch_bindings_shadow_the_import() {
        let for_of = "import { helper } from \"./lib\";\nexport function run(xs: Array<() => number>) { for (const helper of xs) { helper(); } }\n";
        let catch = "import { helper } from \"./lib\";\nexport function run() { try { return 1; } catch (helper) { helper(); } }\n";
        for src in [for_of, catch] {
            let files = [LIB, ("main.ts", src)];
            assert_ne!(
                main_call(&files).map(|(_, c)| c),
                Some(Confidence::Extracted),
                "{src}"
            );
        }
    }
}
