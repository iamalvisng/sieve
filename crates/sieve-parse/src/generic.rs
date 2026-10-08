//! The breadth tier: one language-agnostic extractor over any tree-sitter
//! grammar plus its `tags.scm`. This is how Sieve covers the long tail of
//! languages for about one registry row each, instead of a hand-written
//! extractor per language (the depth tier in `extract.rs`). See
//! the `languages-lsp.md` note section 3.
//!
//! What this emits (signature-only): one node per `@definition.<kind>`
//! capture, and bare-name `calls` raw edges from `@reference.call`,
//! attributed to the innermost enclosing definition. `resolve.rs` then
//! resolves those calls by name. A grammar with no vendored `.scm` falls
//! back to a node-kind walker (symbols only, no calls).

use std::collections::HashSet;

use tree_sitter::{Node as TsNode, Parser, Query, QueryCursor};

use sieve_core::{Kind, Node, Relation};

use crate::build::hex_sha256;
use crate::extract::RawEdge;

/// One breadth-tier language: its Sieve name and the file extensions it
/// claims. Extensions here must not collide with the depth tier's table.
pub struct GenericLang {
    pub name: &'static str,
    pub exts: &'static [&'static str],
}

/// The breadth registry.
pub const GENERIC_LANGS: &[GenericLang] = &[
    GenericLang {
        name: "clojure",
        exts: &[".clj", ".cljs", ".cljc", ".bb"],
    },
    GenericLang {
        name: "rust",
        exts: &[".rs"],
    },
    GenericLang {
        name: "c",
        exts: &[".c", ".h"],
    },
    GenericLang {
        name: "cpp",
        exts: &[".cpp", ".cc", ".cxx", ".hpp", ".hh"],
    },
    GenericLang {
        name: "ruby",
        exts: &[".rb"],
    },
    GenericLang {
        name: "c_sharp",
        exts: &[".cs"],
    },
    GenericLang {
        name: "scala",
        exts: &[".scala", ".sc"],
    },
    GenericLang {
        name: "elixir",
        exts: &[".ex", ".exs"],
    },
    GenericLang {
        name: "solidity",
        exts: &[".sol"],
    },
    GenericLang {
        name: "ocaml",
        exts: &[".ml", ".mli"],
    },
    GenericLang {
        name: "zig",
        exts: &[".zig"],
    },
    GenericLang {
        name: "dart",
        exts: &[".dart"],
    },
    GenericLang {
        name: "nix",
        exts: &[".nix"],
    },
    GenericLang {
        name: "lua",
        exts: &[".lua"],
    },
];

/// Finds the breadth-tier language claiming `path`, by longest matching
/// extension, or `None` when no row claims it.
pub fn generic_lang_of(path: &str) -> Option<&'static GenericLang> {
    let lower = path.to_lowercase();
    GENERIC_LANGS
        .iter()
        .flat_map(|l| l.exts.iter().map(move |e| (*e, l)))
        .filter(|(ext, _)| lower.ends_with(ext))
        .max_by_key(|(ext, _)| ext.len())
        .map(|(_, l)| l)
}

/// A `.scm` capture name maps to a [`Kind`] by its `@definition.<suffix>`
/// or `@reference.<suffix>` tail. An unmapped suffix falls back to
/// `function` (`generic.ts` `KIND`, `:79-84`).
fn kind_for_suffix(suffix: &str) -> Kind {
    match suffix {
        "method" => Kind::Method,
        "class" => Kind::Class,
        "interface" => Kind::Interface,
        "type" => Kind::Type,
        "struct" => Kind::Struct,
        "enum" => Kind::Enum,
        "module" => Kind::Module,
        "constant" => Kind::Constant,
        "variable" | "field" | "property" => Kind::Variable,
        "object" => Kind::Class,
        _ => Kind::Function,
    }
}

/// The tree-sitter query predicates an editor's own tags.scm may carry
/// that the compiled `Query` cannot express. Stripped before compiling.
const STRIPPED_PREDICATES: &[&str] = &[
    "#strip!",
    "#set!",
    "#set-adjacent!",
    "#select-adjacent!",
    "#make-range!",
    "#offset!",
    "#gsub!",
];

/// Removes every `(#<predicate> ...)` call the compiled `Query` cannot
/// express, leaving the surrounding query structure untouched. Assumes no
/// nested parens inside one predicate call, true of every vendored
/// `.scm` file here.
fn strip_predicates(scm: &str) -> String {
    let mut out = String::new();
    let mut rest = scm;
    loop {
        let hit = STRIPPED_PREDICATES
            .iter()
            .filter_map(|pred| {
                let needle = format!("(#{}", &pred[1..]);
                rest.find(&needle).map(|at| (at, needle.len()))
            })
            .min_by_key(|(at, _)| *at);
        let Some((at, _)) = hit else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..at]);
        match rest[at..].find(')') {
            Some(close) => rest = &rest[at + close + 1..],
            None => break, // malformed predicate call; stop rather than loop
        }
    }
    out
}

/// The tags query for each breadth language with one. Four come from the
/// grammar crate's `TAGS_QUERY` constant (`ruby`, `elixir`, `solidity`,
/// `lua`). The other eight use a file written for Sieve in
/// `crates/sieve-parse/queries/`. `ocaml` and
/// `zig` have none and take the fallback walker (`languages-lsp.md`
/// section 3).
fn scm_for(name: &str) -> Option<&'static str> {
    match name {
        "clojure" => Some(include_str!("../queries/clojure.scm")),
        "rust" => Some(include_str!("../queries/rust.scm")),
        "c" => Some(include_str!("../queries/c.scm")),
        "cpp" => Some(include_str!("../queries/cpp.scm")),
        "ruby" => Some(tree_sitter_ruby::TAGS_QUERY),
        "c_sharp" => Some(include_str!("../queries/c_sharp.scm")),
        "scala" => Some(include_str!("../queries/scala.scm")),
        "elixir" => Some(tree_sitter_elixir::TAGS_QUERY),
        "solidity" => Some(tree_sitter_solidity::TAGS_QUERY),
        "dart" => Some(include_str!("../queries/dart.scm")),
        "nix" => Some(include_str!("../queries/nix.scm")),
        "lua" => Some(tree_sitter_lua::TAGS_QUERY),
        _ => None,
    }
}

/// Builds a `tree_sitter::Language` for a breadth grammar by name.
pub(crate) fn language_for(name: &str) -> Option<tree_sitter::Language> {
    let language: tree_sitter::Language = match name {
        "clojure" => tree_sitter_clojure::LANGUAGE.into(),
        "rust" => tree_sitter_rust::LANGUAGE.into(),
        "c" => tree_sitter_c::LANGUAGE.into(),
        "cpp" => tree_sitter_cpp::LANGUAGE.into(),
        "ruby" => tree_sitter_ruby::LANGUAGE.into(),
        "c_sharp" => tree_sitter_c_sharp::LANGUAGE.into(),
        "scala" => tree_sitter_scala::LANGUAGE.into(),
        "elixir" => tree_sitter_elixir::LANGUAGE.into(),
        "solidity" => tree_sitter_solidity::LANGUAGE.into(),
        "ocaml" => tree_sitter_ocaml::LANGUAGE_OCAML.into(),
        "zig" => tree_sitter_zig::LANGUAGE.into(),
        "dart" => tree_sitter_dart::LANGUAGE.into(),
        "nix" => tree_sitter_nix::LANGUAGE.into(),
        "lua" => tree_sitter_lua::LANGUAGE.into(),
        _ => return None,
    };
    Some(language)
}

/// One breadth grammar loaded once: its parser and its compiled tags
/// query, when it has one.
struct Loaded {
    parser: Parser,
    query: Option<Query>,
}

/// Extracts breadth-tier files with one long-lived `Parser` per grammar,
/// same contract as [`crate::extract::Extractor`].
pub struct GenericExtractor {
    loaded: std::collections::HashMap<&'static str, Loaded>,
}

impl GenericExtractor {
    /// Builds one `Parser` per breadth grammar, and compiles its tags
    /// query when `scm_for` has one. A query that fails to compile (none
    /// do today) falls back to the walker for that language.
    pub fn new() -> Result<Self, tree_sitter::LanguageError> {
        let mut loaded = std::collections::HashMap::new();
        for lang in GENERIC_LANGS {
            let Some(language) = language_for(lang.name) else {
                continue;
            };
            let mut parser = Parser::new();
            parser.set_language(&language)?;
            let query = scm_for(lang.name).and_then(|scm| {
                let stripped = strip_predicates(scm);
                Query::new(&language, &stripped).ok()
            });
            loaded.insert(lang.name, Loaded { parser, query });
        }
        Ok(Self { loaded })
    }

    /// Extracts every symbol node and raw edge from one breadth-tier
    /// file. Returns an empty pair for a language this extractor has no
    /// grammar for.
    pub fn extract_file(
        &mut self,
        path: &str,
        source: &str,
        lang_name: &str,
    ) -> (Vec<Node>, Vec<RawEdge>) {
        let Some(entry) = self.loaded.get_mut(lang_name) else {
            return (Vec::new(), Vec::new());
        };
        let Some(tree) = entry.parser.parse(source, None) else {
            return (Vec::new(), Vec::new());
        };

        let mut sink = DefSink::new(path, source);
        let mut raw_edges = Vec::new();
        if let Some(query) = &entry.query {
            tags_extract(
                query,
                tree.root_node(),
                path,
                source,
                lang_name,
                &mut sink,
                &mut raw_edges,
            );
        } else {
            walk_extract(tree.root_node(), source, &mut sink);
        }

        if lang_name == "c" || lang_name == "cpp" {
            extract_includes(tree.root_node(), path, source, &mut raw_edges);
        } else if lang_name == "rust" {
            extract_rust_uses(tree.root_node(), path, source, &mut raw_edges);
        }

        (sink.nodes, raw_edges)
    }
}

/// The mutable state one file's definitions accumulate: minted ids,
/// symbol nodes, and every def's id and byte span (for the innermost-
/// enclosing-definition lookup a call or reference site needs).
struct DefSink<'a> {
    path: &'a str,
    source: &'a str,
    minted: HashSet<String>,
    span_seen: HashSet<usize>,
    nodes: Vec<Node>,
    defs: Vec<(String, usize, usize)>,
}

impl<'a> DefSink<'a> {
    fn new(path: &'a str, source: &'a str) -> Self {
        let mut minted = HashSet::new();
        minted.insert(path.to_string());
        Self {
            path,
            source,
            minted,
            span_seen: HashSet::new(),
            nodes: Vec::new(),
            defs: Vec::new(),
        }
    }

    /// Mints one definition node from a whole-definition tree node
    /// (`generic.ts` `mkDef`, `:253-271`). One node per `start_byte`; a
    /// second call at the same start is a no-op, so a grammar whose
    /// tags.scm captures the same node under two kinds, or a walker
    /// revisit, never mints a near-duplicate.
    fn mk_def(&mut self, name: &str, kind: Kind, whole: TsNode) {
        if self.span_seen.contains(&whole.start_byte()) {
            return;
        }
        self.span_seen.insert(whole.start_byte());
        let id = crate::extract::mint_id(format!("{}#{name}", self.path), &mut self.minted);
        let start_row = whole.start_position().row as u32 + 1;
        let end_row = whole.end_position().row as u32 + 1;
        let line = self
            .source
            .lines()
            .nth(start_row as usize - 1)
            .unwrap_or("");
        let signature = clean_signature(line);
        let body = self
            .source
            .get(whole.start_byte()..whole.end_byte())
            .unwrap_or("");
        self.nodes.push(Node {
            id: id.clone(),
            name: name.to_string(),
            kind,
            owner: None,
            path: self.path.to_string(),
            span: sieve_core::span(start_row, end_row),
            signature,
            exported: true,
            origin: sieve_core::Origin::Generic,
            body_hash: hex_sha256(body.as_bytes()),
            chars: None,
            body_text: Some(cap_body(body)),
            arity: None,
            variadic: None,
            summary_state: sieve_core::SummaryState::Pending,
            summary: None,
            crux: None,
        });
        self.defs.push((id, whole.start_byte(), whole.end_byte()));
    }

    /// The innermost enclosing definition of a byte offset: the
    /// surviving def whose span contains it, narrowest first.
    fn enclosing(&self, at: usize) -> Option<String> {
        self.defs
            .iter()
            .filter(|(_, start, end)| *start <= at && at < *end)
            .min_by_key(|(_, start, end)| end - start)
            .map(|(id, _, _)| id.clone())
    }
}

/// A definition node's `body_text`: the raw slice, whitespace collapsed,
/// capped at 5000 chars, not trimmed (`generic.ts` `mkDef`, `:267`).
fn cap_body(body: &str) -> String {
    let collapsed: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(5000).collect()
}

/// A definition's signature: its start line, trimmed, with one trailing
/// `{`, `:`, or nothing stripped. Empty after trimming becomes `None`.
fn clean_signature(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let trimmed = trimmed.trim_end();
    let stripped = trimmed
        .strip_suffix('{')
        .map(str::trim_end)
        .unwrap_or(trimmed);
    (!stripped.is_empty()).then(|| stripped.to_string())
}

/// The tags.scm path: `@definition.<kind>` mints a node; `@reference.call`
/// mints a bare-name `calls` edge attributed to the innermost enclosing
/// definition (`generic.ts` `tagsExtract`, `:378-432`).
fn tags_extract(
    query: &Query,
    root: TsNode,
    path: &str,
    source: &str,
    lang_name: &str,
    sink: &mut DefSink,
    raw_edges: &mut Vec<RawEdge>,
) {
    let names = query.capture_names().to_vec();
    let mut cursor = QueryCursor::new();
    let mut def_name_at: HashSet<usize> = HashSet::new();
    let mut calls: Vec<(String, usize)> = Vec::new();
    let mut refs: Vec<(String, usize)> = Vec::new();

    // tree-sitter 0.25's `QueryMatches` is a `StreamingIterator`, not a
    // plain `Iterator` (it reuses one match buffer per step), so this
    // walks it with `.next()`/`.get()` instead of a `for` loop.
    use tree_sitter::StreamingIterator;
    let mut matches = cursor.matches(query, root, source.as_bytes());
    while let Some(m) = matches.next() {
        let mut cap_by_name: std::collections::HashMap<&str, TsNode> =
            std::collections::HashMap::new();
        for c in m.captures {
            cap_by_name.insert(names[c.index as usize], c.node);
        }
        let def_key = names
            .iter()
            .find(|n| cap_by_name.contains_key(**n) && n.starts_with("definition."));
        if let (Some(def_key), Some(name_node)) = (def_key, cap_by_name.get("name")) {
            let raw = *cap_by_name.get(*def_key).unwrap_or(name_node);
            if !is_parse_error_artifact(raw) && !is_parse_error_artifact(*name_node) {
                def_name_at.insert(name_node.start_byte());
                let whole = def_scope(raw, lang_name);
                let kind = kind_for_suffix(def_key.trim_start_matches("definition."));
                let Ok(name_text) = name_node.utf8_text(source.as_bytes()) else {
                    continue;
                };
                sink.mk_def(name_text, kind, whole);
            }
        }
        if let Some(name_node) = cap_by_name.get("name") {
            if !is_parse_error_artifact(*name_node) {
                if let Ok(text) = name_node.utf8_text(source.as_bytes()) {
                    if cap_by_name.contains_key("reference.call")
                        || cap_by_name.contains_key("reference.send")
                    {
                        calls.push((text.to_string(), name_node.start_byte()));
                    }
                    if cap_by_name.contains_key("reference.class")
                        || cap_by_name.contains_key("reference.interface")
                        || cap_by_name.contains_key("reference.implementation")
                        || cap_by_name.contains_key("reference.module")
                    {
                        refs.push((text.to_string(), name_node.start_byte()));
                    }
                }
            }
        }
    }

    for (name, at) in calls {
        if def_name_at.contains(&at) {
            continue;
        }
        let source_id = sink.enclosing(at).unwrap_or_else(|| path.to_string());
        raw_edges.push(RawEdge {
            source: source_id,
            relation: Relation::Calls,
            file: path.to_string(),
            target_id: None,
            specifier: None,
            name: Some(name),
            via_member: false,
            recv_type: None,
            arg_count: None,
            // A generic-origin bare call resolves against both `function`
            // and `method` kinds (`languages-lsp.md` line 150), because the
            // breadth tier never types a call's receiver, so a member call
            // like `app.greet` reaches resolve.rs as a bare name.
            kinds: Some(vec![Kind::Function, Kind::Method]),
            implicit_self: false,
            direct_export: false,
            import_name: None,
            default_export: false,
            reexport: None,
            ns_export: false,
        });
    }
    for (name, at) in refs {
        if def_name_at.contains(&at) {
            continue;
        }
        let Some(source_id) = sink.enclosing(at) else {
            continue; // no enclosing def has no sound source
        };
        raw_edges.push(RawEdge {
            source: source_id,
            relation: Relation::References,
            file: path.to_string(),
            target_id: None,
            specifier: None,
            name: Some(name),
            via_member: false,
            recv_type: None,
            arg_count: None,
            // A generic-origin reference carries no specifier, so
            // resolve.rs's `resolve_reference` falls to this kind set
            // instead (`languages-lsp.md` line 151).
            kinds: Some(vec![
                Kind::Class,
                Kind::Interface,
                Kind::Struct,
                Kind::Enum,
                Kind::Type,
                Kind::Module,
            ]),
            implicit_self: false,
            direct_export: false,
            import_name: None,
            default_export: false,
            reexport: None,
            ns_export: false,
        });
    }
}

/// True when `node`'s immediately preceding sibling is a tree-sitter
/// `ERROR` node. A capture that sits right after a parse error is almost
/// always error-recovery debris, not a real definition, call, or
/// reference. This guards against a malformed snippet the grammar cannot
/// parse (an Objective-C `@property ... NS_SWIFT_NAME(...)` line inside
/// a header the breadth tier parses with the plain C grammar) minting a
/// bogus symbol or edge from the recovered fragments.
///
/// ponytail: only the immediately preceding sibling is checked, not
/// every ancestor. Widen the scan if a real repo shows recovery debris
/// this misses.
fn is_parse_error_artifact(node: TsNode) -> bool {
    node.prev_sibling().is_some_and(|s| s.kind() == "ERROR")
}

/// Widens a captured definition node up while its parent's type looks
/// like a definition or declaration container (`generic.ts` `defScope`,
/// `:520-542`).
///
/// ponytail: a fuller walker would widen a Dart `function_signature` to cover a
/// following `function_body` sibling, so a call inside the body
/// attributes to the method instead of the file. `TsNode` cannot be
/// synthesized with a combined span without `unsafe`, so that one Dart
/// case is skipped here; a Dart method's calls attribute to the file
/// instead of the method until a real need shows up.
fn def_scope<'a>(node: TsNode<'a>, _lang_name: &str) -> TsNode<'a> {
    let mut n = node;
    while let Some(parent) = n.parent() {
        let t = parent.kind();
        if t.ends_with("definition")
            || t.ends_with("declaration")
            || t.ends_with("specifier")
            || t.ends_with("_item")
        {
            n = parent;
        } else {
            break;
        }
    }
    n
}

/// Node-type-suffix walk for a grammar with no tags.scm (`ocaml`, `zig`):
/// symbols only, no call resolution (`generic.ts` `walkExtract`, `:471-486`).
fn walk_extract(root: TsNode, source: &str, sink: &mut DefSink) {
    walk_extract_node(root, source, sink);
}

fn walk_extract_node(node: TsNode, source: &str, sink: &mut DefSink) {
    if let Some(kind) = classify_kind(node.kind()) {
        if let Some(name) = node_name(node, source) {
            sink.mk_def(&name, kind, node);
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        walk_extract_node(child, source, sink);
    }
}

/// Maps a node-type suffix to a [`Kind`], most-specific first
/// (`generic.ts` `classifyKind`, `:439-454`).
fn classify_kind(node_type: &str) -> Option<Kind> {
    let t = node_type.to_lowercase();
    let looks_like_def = t.ends_with("declaration")
        || t.ends_with("definition")
        || t.ends_with("_item")
        || t.ends_with("_specifier")
        || t.ends_with("_decl")
        || t.ends_with("_def")
        || t.ends_with("_binding");
    let looks_like_type_kw = t.starts_with("class")
        || t.starts_with("struct")
        || t.starts_with("enum")
        || t.starts_with("interface")
        || t.starts_with("trait")
        || t.starts_with("module")
        || t.starts_with("namespace");
    if !looks_like_def && !looks_like_type_kw {
        return None;
    }
    if t.contains("method") || t.contains("constructor") {
        Some(Kind::Method)
    } else if t.contains("func") || t.contains("subroutine") || t.contains("procedure") {
        // Not a bare "def" substring check: "definition" and
        // "declaration" both end in a "def"-like run of letters, so a
        // "def" trigger would wrongly claim a language's own
        // `value_definition` or `module_definition` node before a later
        // branch (`module`, `val`/`let`) gets to classify it correctly.
        Some(Kind::Function)
    } else if t.contains("class") {
        Some(Kind::Class)
    } else if t.contains("struct") || t.contains("record") {
        Some(Kind::Struct)
    } else if t.contains("interface") || t.contains("trait") || t.contains("protocol") {
        Some(Kind::Interface)
    } else if t.contains("enum") {
        Some(Kind::Enum)
    } else if t.contains("module")
        || t.contains("namespace")
        || t.contains("package")
        || t.contains("mod_")
    {
        Some(Kind::Module)
    } else if t.contains("const") {
        Some(Kind::Constant)
    } else if t.contains("typedef")
        || t.contains("type_alias")
        || t.contains("type_def")
        || t.contains("alias")
        || t.contains("type")
    {
        Some(Kind::Type)
    } else if t.contains("val")
        || t.contains("var")
        || t.contains("let")
        || t.contains("field")
        || t.contains("property")
    {
        Some(Kind::Variable)
    } else {
        None
    }
}

/// A node's declared name: its `name` field, else the first
/// identifier-like descendant within a couple of levels
/// (`generic.ts` `nodeName`, `:456-470`).
fn node_name(node: TsNode, source: &str) -> Option<String> {
    let bytes = source.as_bytes();
    if let Some(field) = node.child_by_field_name("name") {
        if let Ok(text) = field.utf8_text(bytes) {
            if !text.is_empty() {
                return Some(text.to_string());
            }
        }
    }
    let mut queue: std::collections::VecDeque<(TsNode, u32)> = std::collections::VecDeque::new();
    queue.push_back((node, 0));
    while let Some((n, depth)) = queue.pop_front() {
        if depth > 0 && (n.kind().contains("identifier") || n.kind().contains("name")) {
            if let Ok(text) = n.utf8_text(bytes) {
                if !text.is_empty() && !text.contains('\n') {
                    return Some(text.to_string());
                }
            }
        }
        if depth < 3 {
            let mut cursor = n.walk();
            for child in n.named_children(&mut cursor) {
                queue.push_back((child, depth + 1));
            }
        }
    }
    None
}

/// C and C++ `#include "header.h"` → a file→file `imports` raw edge. Only
/// quoted (local) includes are captured (`generic.ts` `extractIncludes`,
/// `:359-374`).
fn extract_includes(root: TsNode, path: &str, source: &str, raw_edges: &mut Vec<RawEdge>) {
    visit_includes(root, path, source, raw_edges);
}

fn visit_includes(node: TsNode, path: &str, source: &str, raw_edges: &mut Vec<RawEdge>) {
    if node.kind() == "preproc_include" {
        if let Some(path_node) = node.child_by_field_name("path") {
            if let Ok(raw) = path_node.utf8_text(source.as_bytes()) {
                if let Some(spec) = raw.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                    if !spec.is_empty() {
                        raw_edges.push(RawEdge {
                            source: path.to_string(),
                            relation: Relation::Imports,
                            file: path.to_string(),
                            target_id: None,
                            specifier: Some(spec.to_string()),
                            name: None,
                            via_member: false,
                            recv_type: None,
                            arg_count: None,
                            kinds: None,
                            implicit_self: false,
                            direct_export: false,
                            import_name: None,
                            default_export: false,
                            reexport: None,
                            ns_export: false,
                        });
                    }
                }
            }
        }
    }
    let mut inner = node.walk();
    for child in node.named_children(&mut inner) {
        visit_includes(child, path, source, raw_edges);
    }
}

/// Rust `use crate::a::b::Item` → a file→module `imports` raw edge whose
/// specifier is the crate-relative module path (`generic.ts`
/// `extractUses`/`rustUseModule`, `:320-352`). `std::`, `super::`,
/// `self::`, external crates, and globs are skipped.
fn extract_rust_uses(root: TsNode, path: &str, source: &str, raw_edges: &mut Vec<RawEdge>) {
    visit_uses(root, path, source, raw_edges);
}

fn visit_uses(node: TsNode, path: &str, source: &str, raw_edges: &mut Vec<RawEdge>) {
    if node.kind() == "use_declaration" {
        if let Ok(text) = node.utf8_text(source.as_bytes()) {
            if let Some(spec) = rust_use_module(text) {
                let specifier = if spec.is_empty() {
                    "crate".to_string()
                } else {
                    format!("crate/{spec}")
                };
                raw_edges.push(RawEdge {
                    source: path.to_string(),
                    relation: Relation::Imports,
                    file: path.to_string(),
                    target_id: None,
                    specifier: Some(specifier),
                    name: None,
                    via_member: false,
                    recv_type: None,
                    arg_count: None,
                    kinds: None,
                    implicit_self: false,
                    direct_export: false,
                    import_name: None,
                    default_export: false,
                    reexport: None,
                    ns_export: false,
                });
            }
        }
    }
    let mut inner = node.walk();
    for child in node.named_children(&mut inner) {
        visit_uses(child, path, source, raw_edges);
    }
}

/// The crate-relative path a Rust `use crate::...` names (`::` → `/`), or
/// `None` when it is not an in-crate absolute import.
fn rust_use_module(text: &str) -> Option<String> {
    let mut s = text.trim().strip_prefix("use").unwrap_or(text).trim();
    s = s.strip_suffix(';').unwrap_or(s).trim();
    let s = if let Some(brace) = s.find('{') {
        s[..brace].trim_end_matches("::").trim()
    } else {
        let no_as = match s.find(" as ") {
            Some(i) => &s[..i],
            None => s,
        };
        no_as.trim()
    };
    if s != "crate" && !s.starts_with("crate::") {
        return None;
    }
    if s.contains('*') {
        return None;
    }
    let rest = s.strip_prefix("crate").unwrap_or(s);
    let rest = rest.strip_prefix("::").unwrap_or(rest);
    Some(rest.replace(' ', "").replace("::", "/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every one of the 12 languages with a tags query compiles it. A
    /// query that fails to compile falls back to the walker without a
    /// message, so this test is the only alarm.
    #[test]
    fn test_p2_05_every_tags_query_compiles() {
        const NAMES: [&str; 12] = [
            "c", "cpp", "c_sharp", "clojure", "dart", "elixir", "lua", "nix", "ruby", "rust",
            "scala", "solidity",
        ];
        for name in NAMES {
            let language = language_for(name).expect("grammar loads");
            let scm = scm_for(name).expect("query exists");
            let query = Query::new(&language, &strip_predicates(scm));
            assert!(query.is_ok(), "{name}: {:?}", query.err());
        }
    }

    /// P2-05: a C function with a multi-line body mints a span that
    /// covers the closing brace, not just the declarator line.
    #[test]
    fn test_p2_05_c_function_body_span_covers_closing_brace() {
        let src = "void clear() {\n  int x = 1;\n  x = x + 1;\n}\n";
        let mut ext = GenericExtractor::new().expect("extractor builds");
        let (nodes, _edges) = ext.extract_file("clear.c", src, "c");
        let clear = nodes
            .iter()
            .find(|n| n.name == "clear")
            .expect("clear def minted");
        assert_eq!(clear.span, "L1-L4");
    }

    /// P2-05: an Objective-C-style header line parsed with the plain C
    /// grammar recovers via `ERROR` nodes. That recovery must not mint a
    /// bogus `property` or `NS_SWIFT_NAME` definition, nor a `calls` raw
    /// edge from the recovered fragments.
    #[test]
    fn test_p2_05_objc_header_mints_no_bogus_nodes_or_calls() {
        let src = "@property (nonatomic) NSString *name NS_SWIFT_NAME(name);\n";
        let mut ext = GenericExtractor::new().expect("extractor builds");
        let (nodes, edges) = ext.extract_file("Header.h", src, "c");
        assert!(
            !nodes
                .iter()
                .any(|n| n.name == "property" || n.name == "NS_SWIFT_NAME"),
            "bogus node minted: {:?}",
            nodes.iter().map(|n| &n.name).collect::<Vec<_>>()
        );
        assert!(
            !edges.iter().any(|e| e.relation == Relation::Calls),
            "bogus calls edge minted: {edges:?}"
        );
    }
}
