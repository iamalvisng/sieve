//! Symbol extraction for TypeScript, TSX, JavaScript-labelled and Python
//! files. Every rule follows the extraction notes; see
//! the `extract-ts-py.md` note.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tree_sitter::{Node as TsNode, Parser};

use sieve_core::{span, Kind, Node, Origin, Relation, SummaryState};

use crate::bindings::{collect_bindings, FileBindings};

/// The char cap for a symbol's collapsed `body_text`.
pub const MAX_BODY_CHARS: usize = 5000;

/// The char cap for a file's collapsed residual text
/// (`MAX_FILE_BODY_CHARS`).
pub const MAX_FILE_BODY_CHARS: usize = 16000;

/// TS value node kinds that make a `variable_declarator` a function symbol.
const FUNCTION_VALUE_TYPES: [&str; 4] = [
    "arrow_function",
    "function",
    "function_expression",
    "generator_function",
];

/// One raw edge, emitted in walk order, before resolution. Resolution (a
/// later task) turns this into a real [`sieve_core::Edge`], or drops it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawEdge {
    pub source: String,
    pub relation: Relation,
    pub file: String,
    pub target_id: Option<String>,
    pub specifier: Option<String>,
    pub name: Option<String>,
    pub via_member: bool,
    pub recv_type: Option<String>,
    /// A call site's argument count (Java overload disambiguation, Swift
    /// `argCount`). `None` for TS and Python, which never set it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arg_count: Option<u32>,
    /// An explicit kind set a bare call resolves against (R only, where a
    /// callee's own shape names its candidate kinds). `None` for TS and
    /// Python, which never set it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kinds: Option<Vec<Kind>>,
    /// Whether a bare call inside a type body stands for `self.<call>`
    /// (Swift `implicitSelf`). Always `false` for TS and Python.
    #[serde(default, skip_serializing_if = "is_false")]
    pub implicit_self: bool,
    /// A `contains` edge only: the definition is a named `export` of its
    /// own file (TS and JS), not nested, not a default export. The
    /// resolver reads it for exact import resolution.
    #[serde(default, skip_serializing_if = "is_false")]
    pub direct_export: bool,
    /// The export name a TS or JS call reaches in its specifier's module:
    /// set for an aliased, a default or a namespace binding, `None` for a
    /// plain named import.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_name: Option<String>,
    /// A `contains` edge only: the definition is the file's default
    /// export.
    #[serde(default, skip_serializing_if = "is_false")]
    pub default_export: bool,
    /// An `imports` record of a TS or JS export (`export ... from`, or a
    /// local export name). Resolution reads it to follow a barrel and
    /// never turns it into a graph edge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reexport: Option<ReExport>,
    /// A `calls` edge only: a member call `r.fn()` whose receiver `r` is a
    /// relative named import, so `r` may be an `export * as r` namespace.
    /// `import_name` holds the receiver's export name.
    #[serde(default, skip_serializing_if = "is_false")]
    pub ns_export: bool,
}

/// One export record of a TS or JS file, for barrel resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReExport {
    /// `export { imported as export } from`: the module's `imported` name
    /// is exported as `export`.
    Named { export: String, imported: String },
    /// `export * from`: every name except `default`.
    Star,
    /// `export * as export from`: the whole module as a namespace.
    StarAs { export: String },
    /// A local export name that mints no function node (a `const`, a
    /// source-less `export { }` name, an `export default` expression).
    Local { export: String },
}

/// Reports whether `value` is `false`, for `RawEdge::implicit_self`'s
/// `skip_serializing_if`, so a `false` value never grows the cache file.
fn is_false(value: &bool) -> bool {
    !*value
}

/// What an import binding names in its module.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ImportedName {
    /// A named import: the exported name (`default` for `{ default as x }`).
    Named(String),
    /// A default import clause.
    Default,
    /// A `* as ns` namespace import.
    Namespace,
}

/// One import binding: what it imports, and from which specifier.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ImportBinding {
    imported: ImportedName,
    specifier: String,
}

/// The walk context: the enclosing scope names, kind, class name, the
/// innermost enclosing definition's id, the file's type bindings, and the
/// file's imported symbol map.
#[derive(Clone)]
struct Ctx<'a> {
    /// The grammar name (`typescript`, `tsx`, `python`), for per-language
    /// dispatch (`exported_for`, `heritage_gate`).
    lang: &'static str,
    scope: Vec<String>,
    enclosing_kind: Option<Kind>,
    enclosing_class: Option<String>,
    parent_id: String,
    bindings: &'a FileBindings,
    imported_symbols: &'a HashMap<String, ImportBinding>,
    /// TS and JS: the local name the file exports as its default, from
    /// `export default <name>;` or `export { <name> as default }`.
    default_local: Option<&'a str>,
    /// Import names a parameter or local declaration shadows in the
    /// current function or method. A shadowed name emits no reference.
    shadowed: HashSet<String>,
    /// TS and JS: the names the parameters and locals of every enclosing
    /// anonymous callback bind. Only the call specifier reads this set, so
    /// reference edges stay unchanged.
    callback_shadowed: HashSet<String>,
    /// Go: the receiver variable name of the enclosing method, for
    /// example `w` in `func (w *Worker) Run()`.
    go_receiver_var: Option<String>,
    /// R and Swift: the immediate parent class's name, kept live through a
    /// class's own methods, so `super$method()` (R) or `super.ping()`
    /// (Swift) resolves to the parent instead of the current override.
    r_super_class: Option<String>,
    /// R: which R6 access section (`public`, `private`, `active`) the walk
    /// is inside while inside an `R6Class(...)` call's own argument list.
    r_r6_access: Option<String>,
    /// R: the S3 generics registered in this file by a local `UseMethod()`
    /// call.
    r_generics: Option<HashSet<String>>,
}

/// Extracts symbols with one long-lived `Parser` per grammar. Building a
/// `Parser` is not free, so `Extractor` builds each once and reuses it
/// for every file `extract_file` is called on.
pub struct Extractor {
    typescript: Parser,
    tsx: Parser,
    python: Parser,
    go: Parser,
    java: Parser,
    php: Parser,
    swift: Parser,
    r: Parser,
    kotlin: Parser,
}

impl Extractor {
    /// Builds one `Parser` per known grammar (typescript, tsx, python, go,
    /// java, php, swift, r, kotlin).
    pub fn new() -> Result<Self, tree_sitter::LanguageError> {
        let mut typescript = Parser::new();
        typescript.set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())?;
        let mut tsx = Parser::new();
        tsx.set_language(&tree_sitter_typescript::LANGUAGE_TSX.into())?;
        let mut python = Parser::new();
        python.set_language(&tree_sitter_python::LANGUAGE.into())?;
        let mut go = Parser::new();
        go.set_language(&tree_sitter_go::LANGUAGE.into())?;
        let mut java = Parser::new();
        java.set_language(&tree_sitter_java::LANGUAGE.into())?;
        let mut php = Parser::new();
        php.set_language(&tree_sitter_php::LANGUAGE_PHP.into())?;
        let mut swift = Parser::new();
        swift.set_language(&tree_sitter_swift::LANGUAGE.into())?;
        let mut r = Parser::new();
        r.set_language(&tree_sitter_r::LANGUAGE.into())?;
        let mut kotlin = Parser::new();
        kotlin.set_language(&tree_sitter_kotlin_sg::LANGUAGE.into())?;
        Ok(Self {
            typescript,
            tsx,
            python,
            go,
            java,
            php,
            swift,
            r,
            kotlin,
        })
    }

    /// Extracts every symbol node and every raw edge from one file's
    /// source, in pre-order document order. Returns an empty pair for a
    /// grammar this extractor does not know.
    pub fn extract_file(
        &mut self,
        path: &str,
        source: &str,
        grammar: &str,
    ) -> (Vec<Node>, Vec<RawEdge>) {
        let (parser, lang): (&mut Parser, &'static str) = match grammar {
            "typescript" => (&mut self.typescript, "typescript"),
            "tsx" => (&mut self.tsx, "tsx"),
            "python" => (&mut self.python, "python"),
            "go" => (&mut self.go, "go"),
            "java" => (&mut self.java, "java"),
            "php" => (&mut self.php, "php"),
            "swift" => (&mut self.swift, "swift"),
            "r" => (&mut self.r, "r"),
            "kotlin" => (&mut self.kotlin, "kotlin"),
            _ => return (Vec::new(), Vec::new()),
        };
        let is_python = lang == "python";
        let Some(tree) = parser.parse(source, None) else {
            return (Vec::new(), Vec::new());
        };

        let mut minted: HashSet<String> = HashSet::new();
        minted.insert(path.to_string());

        let bindings = collect_bindings(tree.root_node(), source, grammar);
        let imported_symbols = collect_imported_symbols(tree.root_node(), source, is_python);
        let default_local = (!is_python).then(|| collect_default_local(tree.root_node(), source));
        let default_local = default_local.flatten();
        let r_generics = (lang == "r").then(|| collect_r_generics(tree.root_node(), source));

        let root_ctx = Ctx {
            lang,
            scope: Vec::new(),
            enclosing_kind: None,
            enclosing_class: None,
            parent_id: path.to_string(),
            bindings: &bindings,
            imported_symbols: &imported_symbols,
            default_local: default_local.as_deref(),
            shadowed: HashSet::new(),
            callback_shadowed: HashSet::new(),
            go_receiver_var: None,
            r_super_class: None,
            r_r6_access: None,
            r_generics,
        };

        let mut out = Vec::new();
        let mut edges = Vec::new();
        let file = FileCtx {
            path,
            source,
            is_python,
        };
        let mut sink = Sink {
            minted: &mut minted,
            out: &mut out,
            edges: &mut edges,
        };
        let mut cursor = tree.root_node().walk();
        for child in tree.root_node().named_children(&mut cursor) {
            if matches!(lang, "typescript" | "tsx") {
                let callback_shadowed = root_statement_shadowed_names(child, source);
                let stmt_ctx = Ctx {
                    callback_shadowed,
                    ..root_ctx.clone()
                };
                walk(child, &stmt_ctx, &file, &mut sink);
            } else {
                walk(child, &root_ctx, &file, &mut sink);
            }
        }
        if matches!(lang, "typescript" | "tsx") {
            edges.extend(collect_reexports(tree.root_node(), source, path));
        }
        (out, edges)
    }
}

/// The per-file constants a walk needs at every node: the file id, its
/// source text, and whether it is a Python file.
struct FileCtx<'a> {
    path: &'a str,
    source: &'a str,
    is_python: bool,
}

/// The mutable collectors a walk writes into: minted ids, symbol nodes,
/// and raw edges.
struct Sink<'a> {
    minted: &'a mut HashSet<String>,
    out: &'a mut Vec<Node>,
    edges: &'a mut Vec<RawEdge>,
}

/// Walks one named node in pre-order. `describe` runs first: a matched
/// definition mints its node, emits its `contains` and heritage edges, then
/// descends into its own children with a narrowed context, and returns. An
/// unmatched node runs the `imports`, `calls`, and `references` checks, then
/// descends with the same context. A definition node never also emits a `calls`
/// edge for itself.
fn walk(node: TsNode, ctx: &Ctx, file: &FileCtx, sink: &mut Sink) {
    let (path, source) = (file.path, file.source);
    let lang = ctx.lang;

    let desc = if lang == "r" {
        describe_r(node, ctx, source)
    } else {
        describe(
            node,
            ctx.enclosing_kind,
            ctx.enclosing_class.as_deref(),
            source,
            lang,
        )
    };
    if let Some(desc) = desc {
        emit_definition(node, desc, ctx, file, sink);
        return;
    }

    // R6: a `public =`/`private =`/`active =` argument inside an
    // R6-class-defining call's own arguments is R's version of a class body.
    // Each entry walks with `r_r6_access` set, so a nested `argument` node can
    // match `describe_r`'s R6-method shape.
    if let Some(access) = r_r6_access_section_name(node, ctx, source) {
        if let Some(value) = node.child_by_field_name("value") {
            for entry in r_call_args(value) {
                let child_ctx = Ctx {
                    r_r6_access: Some(access.clone()),
                    ..ctx.clone()
                };
                walk(entry, &child_ctx, file, sink);
            }
        }
        return;
    }

    if is_import(node, source, lang) {
        if let Some(specifier) = import_specifier(node, source, lang) {
            sink.edges.push(RawEdge {
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
        return;
    }

    if is_call_node(node.kind(), lang) {
        let info = if lang == "r" && r_is_consumed_class_call(node, source) {
            None
        } else {
            callee_info(node, source, lang)
        };
        if let Some((name, via_member, receiver, arg_count, kinds)) = info {
            // Swift `implicitSelf` (edges note, section 1.4): a bare
            // lowercase call inside a type body, with no explicit kinds,
            // stands for `self.<call>`.
            let swift_implicit_self = lang == "swift"
                && !via_member
                && kinds.is_none()
                && ctx.enclosing_class.is_some()
                && !name.chars().next().is_some_and(char::is_uppercase);
            if swift_implicit_self {
                sink.edges.push(RawEdge {
                    source: ctx.parent_id.clone(),
                    relation: Relation::Calls,
                    file: path.to_string(),
                    target_id: None,
                    specifier: None,
                    name: Some(name),
                    via_member: true,
                    recv_type: ctx.enclosing_class.clone(),
                    arg_count,
                    kinds: None,
                    implicit_self: true,
                    direct_export: false,
                    import_name: None,
                    default_export: false,
                    reexport: None,
                    ns_export: false,
                });
            } else {
                let (specifier, import_name, ns_export) =
                    match call_import_specifier(&name, via_member, receiver.as_deref(), ctx) {
                        Some((specifier, import_name, ns)) => (Some(specifier), import_name, ns),
                        None => (None, None, false),
                    };
                let recv_type = if via_member && lang == "go" && receiver == ctx.go_receiver_var {
                    ctx.enclosing_class.clone()
                } else if lang == "r" && receiver.as_deref() == Some("super") {
                    ctx.r_super_class.clone()
                } else if lang == "php"
                    && receiver
                        .as_deref()
                        .is_some_and(|r| !matches!(r, "self" | "this") && !r.starts_with('$'))
                {
                    // A PHP receiver without `$` is the class name itself.
                    receiver
                } else {
                    receiver.and_then(|r| {
                        ctx.bindings.resolve_recv_type(
                            &r,
                            &ctx.scope,
                            ctx.enclosing_class.as_deref(),
                        )
                    })
                };
                sink.edges.push(RawEdge {
                    source: ctx.parent_id.clone(),
                    relation: Relation::Calls,
                    file: path.to_string(),
                    target_id: None,
                    specifier,
                    name: Some(name),
                    via_member,
                    recv_type,
                    arg_count,
                    kinds,
                    implicit_self: false,
                    direct_export: false,
                    import_name,
                    default_export: false,
                    reexport: None,
                    ns_export,
                });
            }
        }
    }

    if node.kind() == "identifier" {
        maybe_emit_reference(node, ctx, path, source, sink.edges);
    }

    let callback_ctx;
    let child_ctx = if matches!(node.kind(), "arrow_function" | "function_expression") {
        let mut callback_shadowed = ctx.callback_shadowed.clone();
        callback_shadowed.extend(callback_bound_names(node, source));
        callback_shadowed.extend(loop_catch_bound_names(node, source));
        callback_ctx = Ctx {
            callback_shadowed,
            ..ctx.clone()
        };
        &callback_ctx
    } else {
        ctx
    };
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        walk(child, child_ctx, file, sink);
    }
}

/// The `(specifier, import_name, ns_export)` a TS or JS call carries. A bare
/// call of a binding gives the imported name (`None` when local and imported
/// names match). A `ns.fn()` call on a namespace binding gives `fn`. An
/// `r.fn()` call on a relative named binding gives the receiver's export
/// name and sets `ns_export`. A name in a shadow set gives nothing.
fn call_import_specifier(
    name: &str,
    via_member: bool,
    receiver: Option<&str>,
    ctx: &Ctx,
) -> Option<(String, Option<String>, bool)> {
    if !matches!(ctx.lang, "typescript" | "tsx") {
        return None;
    }
    let local = if via_member { receiver? } else { name };
    if ctx.shadowed.contains(local) || ctx.callback_shadowed.contains(local) {
        return None;
    }
    let binding = ctx.imported_symbols.get(local)?;
    let specifier = binding.specifier.clone();
    match (&binding.imported, via_member) {
        (ImportedName::Namespace, true) => Some((specifier, Some(name.to_string()), false)),
        (ImportedName::Named(imported), true) if specifier.starts_with('.') => {
            Some((specifier, Some(imported.clone()), true))
        }
        (ImportedName::Named(imported), false) if imported == name => {
            Some((specifier, None, false))
        }
        (ImportedName::Named(imported), false) => Some((specifier, Some(imported.clone()), false)),
        (ImportedName::Default, false) => Some((specifier, Some("default".to_string()), false)),
        _ => None,
    }
}

/// Inserts every identifier a binding pattern binds under `pattern` (a name,
/// or a destructuring pattern). The walk over-counts a default value, which
/// is safe: a larger shadow set only removes specifiers.
fn insert_bound_names(pattern: TsNode, source: &str, names: &mut HashSet<String>) {
    let mut stack = vec![pattern];
    while let Some(n) = stack.pop() {
        if matches!(
            n.kind(),
            "identifier" | "shorthand_property_identifier_pattern"
        ) {
            if let Ok(text) = n.utf8_text(source.as_bytes()) {
                names.insert(text.to_string());
            }
        }
        let mut cursor = n.walk();
        stack.extend(n.named_children(&mut cursor));
    }
}

/// The names one root statement binds outside any function: its loop and
/// catch bindings, and its locals. A function statement binds nothing here,
/// because its own context scans its body. The set stays inside this
/// statement, so a root loop variable never shadows a name in a function.
fn root_statement_shadowed_names(stmt: TsNode, source: &str) -> HashSet<String> {
    let mut names = HashSet::new();
    if !is_function_boundary(stmt.kind()) {
        names = scan_loop_catch_names(stmt, source, true);
        collect_local_declarations(stmt, stmt, None, source, &mut names);
    }
    names
}

/// The local name a file exports as its default, read from the direct
/// children of the root: `export default <name>;` or `export { <name> as
/// default }`. A re-export (`export { x as default } from './m'`) binds no
/// local name, so it is skipped.
fn collect_default_local(root: TsNode, source: &str) -> Option<String> {
    let text = |n: TsNode| n.utf8_text(source.as_bytes()).ok().map(str::to_string);
    let mut found = None;
    let mut cursor = root.walk();
    for stmt in root.named_children(&mut cursor) {
        if stmt.kind() != "export_statement" || stmt.child_by_field_name("source").is_some() {
            continue;
        }
        if let Some(value) = stmt.child_by_field_name("value") {
            if value.kind() == "identifier" {
                found = text(value).or(found);
            }
        }
        let mut sc = stmt.walk();
        for clause in stmt.named_children(&mut sc) {
            if clause.kind() != "export_clause" {
                continue;
            }
            let mut ec = clause.walk();
            for spec in clause.named_children(&mut ec) {
                let alias = spec.child_by_field_name("alias").and_then(text);
                if alias.as_deref() == Some("default") {
                    found = spec.child_by_field_name("name").and_then(text).or(found);
                }
            }
        }
    }
    found
}

/// Builds one `imports` record that carries an export, with no call data.
fn export_record(path: &str, specifier: Option<String>, reexport: ReExport) -> RawEdge {
    RawEdge {
        source: path.to_string(),
        relation: Relation::Imports,
        file: path.to_string(),
        target_id: None,
        specifier,
        name: None,
        via_member: false,
        recv_type: None,
        arg_count: None,
        kinds: None,
        implicit_self: false,
        direct_export: false,
        import_name: None,
        default_export: false,
        reexport: Some(reexport),
        ns_export: false,
    }
}

/// The text of an export name node, or `None` for a string name
/// (`export { x as "a-b" }`), which no identifier can import.
fn export_name_text(node: TsNode, source: &str) -> Option<String> {
    if node.kind() == "string" {
        return None;
    }
    node.utf8_text(source.as_bytes()).ok().map(str::to_string)
}

/// Reports whether `node` has an unnamed child token of kind `kind`.
fn has_token(node: TsNode, kind: &str) -> bool {
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).any(|c| c.kind() == kind);
    found
}

/// Reads the export records of a TS or JS file from the direct children of
/// the root: each `export ... from` form, and each local export name that
/// mints no function node (see [`ReExport::Local`]). A type-only form
/// (`export type { }`, `export type * from`, an inline `type` specifier)
/// and a string name give no record. JS files give no re-export records in
/// this slice, because only the TS and TSX grammars run this scan.
fn collect_reexports(root: TsNode, source: &str, path: &str) -> Vec<RawEdge> {
    let mut records = Vec::new();
    let local = |export: String| export_record(path, None, ReExport::Local { export });
    let mut cursor = root.walk();
    for stmt in root.named_children(&mut cursor) {
        if stmt.kind() != "export_statement" || has_token(stmt, "type") || stmt.has_error() {
            continue;
        }
        let from = import_specifier(stmt, source, "typescript");
        let mut plain_star = from.is_some() && has_token(stmt, "*");
        let mut sc = stmt.walk();
        for child in stmt.named_children(&mut sc) {
            match child.kind() {
                "namespace_export" => {
                    plain_star = false;
                    let mut nc = child.walk();
                    let export = child
                        .named_children(&mut nc)
                        .find_map(|n| export_name_text(n, source));
                    if let (Some(export), Some(from)) = (export, from.clone()) {
                        records.push(export_record(path, Some(from), ReExport::StarAs { export }));
                    }
                }
                "export_clause" => {
                    plain_star = false;
                    let mut ec = child.walk();
                    for spec in child.named_children(&mut ec) {
                        if spec.kind() != "export_specifier" || has_token(spec, "type") {
                            continue;
                        }
                        let name = spec
                            .child_by_field_name("name")
                            .and_then(|n| export_name_text(n, source));
                        let export = match spec.child_by_field_name("alias") {
                            Some(a) => export_name_text(a, source),
                            None => name.clone(),
                        };
                        let (Some(imported), Some(export)) = (name, export) else {
                            continue;
                        };
                        records.push(match from.clone() {
                            Some(from) => export_record(
                                path,
                                Some(from),
                                ReExport::Named { export, imported },
                            ),
                            None => local(export),
                        });
                    }
                }
                _ => {}
            }
        }
        if let (Some(from), true) = (from, plain_star) {
            records.push(export_record(path, Some(from), ReExport::Star));
        }
        records.extend(local_export_names(stmt, source).into_iter().map(local));
    }
    records
}

/// The export names a declaring `export` statement binds without a function
/// node: a `const`, `let` or `var` whose value is no function, and an `export
/// default` of anything but a named function declaration.
fn local_export_names(stmt: TsNode, source: &str) -> Vec<String> {
    if has_token(stmt, "default") {
        let named_fn = stmt.child_by_field_name("declaration").is_some_and(|d| {
            matches!(
                d.kind(),
                "function_declaration" | "generator_function_declaration"
            ) && d.child_by_field_name("name").is_some()
        });
        return if named_fn {
            Vec::new()
        } else {
            vec!["default".to_string()]
        };
    }
    let Some(decl) = stmt.child_by_field_name("declaration") else {
        return Vec::new();
    };
    if !matches!(decl.kind(), "lexical_declaration" | "variable_declaration") {
        return Vec::new();
    }
    let mut names = HashSet::new();
    let mut cursor = decl.walk();
    for declarator in decl.named_children(&mut cursor) {
        let is_function = declarator
            .child_by_field_name("value")
            .is_some_and(|v| FUNCTION_VALUE_TYPES.contains(&v.kind()));
        if declarator.kind() == "variable_declarator" && !is_function {
            if let Some(name) = declarator.child_by_field_name("name") {
                insert_bound_names(name, source, &mut names);
            }
        }
    }
    let mut names: Vec<String> = names.into_iter().collect();
    names.sort();
    names
}

/// Reports whether `node` is the declaration of an `export default`
/// statement.
fn is_default_declaration(node: TsNode) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    if parent.kind() != "export_statement"
        || parent.child_by_field_name("declaration") != Some(node)
    {
        return false;
    }
    let mut cursor = parent.walk();
    let has_default = parent.children(&mut cursor).any(|c| c.kind() == "default");
    has_default
}

/// Every identifier a `for...in`/`for...of` head or a `catch` parameter
/// binds anywhere under `node`, nested functions included. The scan
/// over-counts, which is safe: a larger set only removes specifiers.
fn loop_catch_bound_names(node: TsNode, source: &str) -> HashSet<String> {
    scan_loop_catch_names(node, source, false)
}

/// Runs the `loop_catch_bound_names` scan. With `stop_at_functions`, it skips
/// the body of each nested function, because that function's own context
/// scans it.
fn scan_loop_catch_names(node: TsNode, source: &str, stop_at_functions: bool) -> HashSet<String> {
    let mut names = HashSet::new();
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if stop_at_functions && n != node && is_function_boundary(n.kind()) {
            continue;
        }
        let field = match n.kind() {
            "for_in_statement" => Some("left"),
            "catch_clause" => Some("parameter"),
            _ => None,
        };
        if let Some(head) = field.and_then(|f| n.child_by_field_name(f)) {
            insert_bound_names(head, source, &mut names);
        }
        let mut cursor = n.walk();
        stack.extend(n.named_children(&mut cursor));
    }
    names
}

/// Every name an anonymous callback binds: each identifier in its
/// parameter list (this over-counts a default value, which is safe), and
/// its local declarations.
fn callback_bound_names(node: TsNode, source: &str) -> HashSet<String> {
    let mut names = HashSet::new();
    for field in ["parameters", "parameter"] {
        if let Some(params) = node.child_by_field_name(field) {
            insert_bound_names(params, source, &mut names);
        }
    }
    collect_local_declarations(node, node, None, source, &mut names);
    names
}

/// Mints one matched definition's node, emits its `contains` edge and (when
/// `heritage_gate` fires) its heritage edges, then walks its children with
/// a context narrowed to this definition.
fn emit_definition(node: TsNode, desc: Desc, ctx: &Ctx, file: &FileCtx, sink: &mut Sink) {
    let (path, source, is_python) = (file.path, file.source, file.is_python);
    let Desc {
        name,
        id_name,
        kind,
        header_end,
        hash_node,
        owner,
        arity,
        variadic,
    } = desc;
    // The id-scope segment: `id_name` when a language sets one (Go's
    // receiver-qualified method), else the bare `name`.
    let id_part = id_name.unwrap_or_else(|| name.clone());

    let signature = clean(&source_slice(source, hash_node.start_byte(), header_end));
    let start_line = hash_node.start_position().row as u32 + 1;
    let end_line = hash_node.end_position().row as u32 + 1;
    let raw_text = source_slice(source, hash_node.start_byte(), hash_node.end_byte());
    let body_hash = hex_sha256(raw_text.as_bytes());
    let body_text = Some(cap_chars(collapse_whitespace(&raw_text), MAX_BODY_CHARS));
    let exported = exported_for(ctx.lang, node, &name, source, ctx);
    let owner = if kind == Kind::Method {
        owner.or_else(|| ctx.enclosing_class.clone())
    } else {
        None
    };

    let mut id_parts = ctx.scope.clone();
    id_parts.push(id_part.clone());
    let base = format!("{path}#{}", id_parts.join("."));
    let id = mint_id(base, sink.minted);

    let is_ts = matches!(ctx.lang, "typescript" | "tsx");
    sink.edges.push(RawEdge {
        source: ctx.parent_id.clone(),
        relation: Relation::Contains,
        file: path.to_string(),
        target_id: Some(id.clone()),
        specifier: None,
        name: None,
        via_member: false,
        recv_type: None,
        arg_count: None,
        kinds: None,
        implicit_self: false,
        direct_export: is_ts && is_direct_named_export(node),
        import_name: None,
        default_export: is_ts
            && (is_default_declaration(node)
                || (ctx.scope.is_empty() && ctx.default_local == Some(name.as_str()))),
        reexport: None,
        ns_export: false,
    });
    if heritage_gate(ctx.lang, kind) {
        emit_heritage_edges(node, &id, path, source, ctx.lang, sink.edges);
    }

    sink.out.push(Node {
        id: id.clone(),
        name: name.clone(),
        kind,
        path: path.to_string(),
        span: span(start_line, end_line),
        signature,
        exported,
        origin: Origin::Ast,
        body_hash,
        chars: None,
        body_text,
        summary_state: SummaryState::Pending,
        summary: None,
        crux: None,
        owner: owner.clone(),
        arity,
        variadic,
    });

    let mut child_scope = ctx.scope.clone();
    child_scope.push(id_part);
    let mut child_shadowed = ctx.shadowed.clone();
    if matches!(kind, Kind::Function | Kind::Method) {
        child_shadowed.extend(collect_shadowed_names(node, source, is_python));
    }
    let child_ctx = Ctx {
        lang: ctx.lang,
        scope: child_scope,
        enclosing_kind: Some(kind),
        // A class always hosts its own methods' `owner` fallback. Java
        // widens this to interface too (an abstract method still gets an
        // owner); PHP does not (an interface method's `owner` stays
        // unset, per the golden). A trait never hosts one either: a
        // trait method's `owner` stays unset, so it resolves only
        // through the class that pulls the trait in
        // (`resolve_trait_member`, edges note section 4).
        enclosing_class: if kind == Kind::Class
            || (kind == Kind::Interface && ctx.lang == "java")
            || (ctx.lang == "swift" && SWIFT_TYPE_KINDS.contains(&kind))
            || (ctx.lang == "kotlin" && KOTLIN_TYPE_KINDS.contains(&kind))
        {
            Some(name)
        } else if ctx.lang == "go" && kind == Kind::Method {
            // A Go method is not lexically nested inside its receiver
            // type, so its own `owner` (the receiver type) stands in for
            // `enclosing_class` inside its body, letting a receiver
            // variable call resolve.
            owner.clone()
        } else {
            ctx.enclosing_class.clone()
        },
        parent_id: id,
        bindings: ctx.bindings,
        imported_symbols: ctx.imported_symbols,
        default_local: ctx.default_local,
        shadowed: child_shadowed,
        callback_shadowed: if matches!(kind, Kind::Function | Kind::Method) && !is_python {
            let mut set = ctx.callback_shadowed.clone();
            set.extend(loop_catch_bound_names(node, source));
            set
        } else {
            ctx.callback_shadowed.clone()
        },
        go_receiver_var: if ctx.lang == "go" && kind == Kind::Method {
            go_receiver_var_name(node, source)
        } else {
            ctx.go_receiver_var.clone()
        },
        // Swift reads its own `:` clause off the declaration node; R reads
        // an R6 `inherit =` off whatever `describe_r` matched. Both are
        // recomputed only when entering a genuinely new class, so a
        // `super`/`super$` call inside a method resolves against the
        // PARENT type, not the overriding current one (edges note, section
        // 1.4 and 1.6).
        r_super_class: if kind == Kind::Class {
            match ctx.lang {
                "r" => r_r6_parent_class(node, source),
                "swift" => swift_super_class_name(node, source),
                _ => None,
            }
        } else {
            ctx.r_super_class.clone()
        },
        // Reset on every new definition: a purely local marker for "still
        // inside THIS class-defining call's own public=/private=/active=
        // argument chain," not something that leaks into a nested
        // definition (edges note, section 1.6).
        r_r6_access: None,
        r_generics: ctx.r_generics.clone(),
    };

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        walk(child, &child_ctx, file, sink);
    }
}

/// The names a function or method binds inside its own scope: its own
/// parameters, plus (TS only) every local declaration in its body, except a
/// declaration inside a nested function or method.
/// A name in this set shadows an import of the same name for the whole
/// function.
fn collect_shadowed_names(node: TsNode, source: &str, is_python: bool) -> HashSet<String> {
    let mut names = collect_param_names(node, source, is_python);
    if !is_python {
        let value = if node.kind() == "variable_declarator" {
            node.child_by_field_name("value")
        } else {
            None
        };
        collect_local_declarations(node, node, value, source, &mut names);
    }
    names
}

/// TS node kinds Sieve treats as a nested function boundary. The scan for
/// `collect_local_declarations` records a boundary's own name, then stops,
/// because a nested function is a separate scope that filters its own imports.
fn is_function_boundary(kind: &str) -> bool {
    matches!(
        kind,
        "function_declaration"
            | "generator_function_declaration"
            | "method_definition"
            | "arrow_function"
            | "function_expression"
            | "function"
    )
}

/// Walks `node`'s subtree for the local declarations that shadow an import
/// inside `definition`. `definition` and its `variable_declarator` value
/// (`definition_value`) are exempt from the boundary check, so the scan enters
/// the function's own body. Any other function boundary records its own name,
/// then the scan does not recurse into it.
fn collect_local_declarations(
    node: TsNode,
    definition: TsNode,
    definition_value: Option<TsNode>,
    source: &str,
    out: &mut HashSet<String>,
) {
    let is_definition = node == definition || Some(node) == definition_value;
    if !is_definition && is_function_boundary(node.kind()) {
        // A function expression's name binds only inside its own body.
        let is_expression = matches!(
            node.kind(),
            "function_expression" | "function" | "generator_function"
        );
        if let Some(name) = node.child_by_field_name("name").filter(|_| !is_expression) {
            if name.kind() == "identifier" {
                if let Ok(text) = name.utf8_text(source.as_bytes()) {
                    out.insert(text.to_string());
                }
            }
        }
        return;
    }
    match node.kind() {
        "variable_declarator" => {
            if let Some(name) = node.child_by_field_name("name") {
                insert_bound_names(name, source, out);
            }
        }
        "required_parameter" | "optional_parameter" => {
            if let Some(pattern) = node.child_by_field_name("pattern") {
                insert_bound_names(pattern, source, out);
            }
        }
        // A named function expression binds its own name inside its body.
        "function_expression" | "function" | "generator_function" => {
            if let Some(name) = node.child_by_field_name("name") {
                insert_bound_names(name, source, out);
            }
        }
        "identifier" if node.parent().map(|p| p.kind()) == Some("formal_parameters") => {
            if let Ok(text) = node.utf8_text(source.as_bytes()) {
                out.insert(text.to_string());
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_local_declarations(child, definition, definition_value, source, out);
    }
}

/// The parameter names a function or method's own parameter list binds.
fn collect_param_names(node: TsNode, source: &str, is_python: bool) -> HashSet<String> {
    let mut names = HashSet::new();
    let params = if is_python {
        node.child_by_field_name("parameters")
    } else {
        let target = if node.kind() == "variable_declarator" {
            node.child_by_field_name("value")
        } else {
            Some(node)
        };
        target.and_then(|t| t.child_by_field_name("parameters"))
    };
    let Some(params) = params else {
        return names;
    };
    let mut cursor = params.walk();
    for child in params.named_children(&mut cursor) {
        let ident = match child.kind() {
            "identifier" => Some(child),
            "required_parameter" | "optional_parameter" => child.child_by_field_name("pattern"),
            "typed_parameter" | "default_parameter" | "typed_default_parameter" => {
                child.named_child(0)
            }
            _ => None,
        };
        if let Some(id_node) = ident {
            insert_bound_names(id_node, source, &mut names);
        }
    }
    names
}

/// Reports whether `node` is an import node for `lang`. Go and PHP match
/// at the leaf (`import_spec`, `namespace_use_clause`), so a grouped
/// statement gives one edge per leaf, because the grouping node itself
/// never matches and the walk keeps descending into it. R has no import
/// grammar construct: `library()`, `require()`, and `source()` are
/// ordinary `call` nodes, so `is_import` must check the node's own callee
/// text, not just its kind (edges note, section 1.6).
fn is_import(node: TsNode, source: &str, lang: &str) -> bool {
    let kind = node.kind();
    match lang {
        "python" => matches!(kind, "import_statement" | "import_from_statement"),
        "go" => kind == "import_spec",
        "java" => kind == "import_declaration",
        "php" => kind == "namespace_use_clause",
        "r" => {
            kind == "call"
                && matches!(
                    r_callee_name(node, source).as_deref(),
                    Some("library" | "require" | "source")
                )
        }
        "swift" => kind == "import_declaration",
        "kotlin" => kind == "import_header",
        _ => kind == "import_statement",
    }
}

/// Reports whether `kind` is a call node for `lang`.
fn is_call_node(kind: &str, lang: &str) -> bool {
    match lang {
        "python" => kind == "call",
        "go" => kind == "call_expression",
        "java" => matches!(kind, "method_invocation" | "object_creation_expression"),
        "php" => matches!(
            kind,
            "function_call_expression"
                | "member_call_expression"
                | "scoped_call_expression"
                | "nullsafe_member_call_expression"
        ),
        "r" => kind == "call",
        _ => kind == "call_expression",
    }
}

/// Reads one import statement's specifier text, per the source-shape
/// table in the edge note, section 3, and per section 1 for Go, Java, and
/// PHP.
fn import_specifier(node: TsNode, source: &str, lang: &str) -> Option<String> {
    match lang {
        "python" => {
            if node.kind() == "import_from_statement" {
                let module = node.child_by_field_name("module_name")?;
                return module.utf8_text(source.as_bytes()).ok().map(str::to_string);
            }
            let name = node.child_by_field_name("name")?;
            if name.kind() != "dotted_name" {
                return None;
            }
            name.utf8_text(source.as_bytes()).ok().map(str::to_string)
        }
        "go" => {
            let path_node = node.child_by_field_name("path")?;
            let text = path_node.utf8_text(source.as_bytes()).ok()?;
            Some(text.trim_matches(|c| c == '"' || c == '`').to_string())
        }
        "java" => {
            let mut cursor = node.walk();
            let name_node = node
                .named_children(&mut cursor)
                .find(|c| matches!(c.kind(), "scoped_identifier" | "identifier"))?;
            name_node
                .utf8_text(source.as_bytes())
                .ok()
                .map(str::to_string)
        }
        "php" => {
            let mut cursor = node.walk();
            let name_node = node
                .named_children(&mut cursor)
                .find(|c| matches!(c.kind(), "qualified_name" | "name"))?;
            let text = name_node.utf8_text(source.as_bytes()).ok()?;
            Some(text.trim_start_matches('\\').to_string())
        }
        "kotlin" => {
            // `import_header`: the specifier is the first `identifier`
            // child's text (edges note, section 1.3).
            let mut cursor = node.walk();
            let found = node
                .named_children(&mut cursor)
                .find(|c| c.kind() == "identifier")
                .and_then(|c| c.utf8_text(source.as_bytes()).ok())
                .map(str::to_string);
            found
        }
        "swift" => {
            // `import Foundation` / `import struct Foundation.Date`: the
            // dotted module path is the `identifier` child; an
            // import-kind keyword (`struct`, `class`, ...) is a separate
            // token, so it never matches here.
            let mut cursor = node.walk();
            let found = node
                .named_children(&mut cursor)
                .find(|c| c.kind() == "identifier")
                .and_then(|c| c.utf8_text(source.as_bytes()).ok())
                .map(str::to_string);
            found
        }
        "r" => {
            // `library(pkg)` / `library("pkg")` / `require(pkg)` /
            // `source("f.R")`: the target is the first positional
            // argument, bare symbol or string (edges note, section 1.6).
            let value = r_call_args(node)
                .into_iter()
                .next()
                .and_then(|a| a.child_by_field_name("value"));
            if let Some(v) = value {
                if v.kind() == "identifier" {
                    return v.utf8_text(source.as_bytes()).ok().map(str::to_string);
                }
            }
            r_string_content(value, source)
        }
        _ => {
            let source_node = node.child_by_field_name("source")?;
            let mut cursor = source_node.walk();
            let found = source_node
                .named_children(&mut cursor)
                .find(|c| c.kind() == "string_fragment")
                .and_then(|c| c.utf8_text(source.as_bytes()).ok())
                .map(str::to_string);
            found
        }
    }
}

/// Emits an `extends` or `implements` raw edge for each base named in a
/// class node's heritage, per the edge note, section 6.
fn emit_heritage_edges(
    node: TsNode,
    class_id: &str,
    path: &str,
    source: &str,
    lang: &str,
    edges: &mut Vec<RawEdge>,
) {
    let mut push_name = |relation: Relation, name: &str| {
        edges.push(RawEdge {
            source: class_id.to_string(),
            relation,
            file: path.to_string(),
            target_id: None,
            specifier: None,
            name: Some(name.to_string()),
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
    };
    let mut push = |relation: Relation, base: TsNode| {
        if let Ok(name) = base.utf8_text(source.as_bytes()) {
            push_name(relation, name);
        }
    };

    match lang {
        "python" => {
            let Some(superclasses) = node.child_by_field_name("superclasses") else {
                return;
            };
            let mut cursor = superclasses.walk();
            for base in superclasses.named_children(&mut cursor) {
                if base.kind() == "identifier" {
                    push(Relation::Extends, base);
                }
            }
        }
        "java" => {
            if let Some(superclass) = node.child_by_field_name("superclass") {
                if let Some(name) = java_heritage_name(superclass, source) {
                    push_name(Relation::Extends, &name);
                }
            }
            let mut cursor = node.walk();
            for clause in node.named_children(&mut cursor) {
                if !matches!(clause.kind(), "super_interfaces" | "extends_interfaces") {
                    continue;
                }
                let relation = if clause.kind() == "extends_interfaces" {
                    Relation::Extends
                } else {
                    Relation::Implements
                };
                for base in java_heritage_entries(clause) {
                    if let Some(name) = java_heritage_name(base, source) {
                        push_name(relation, &name);
                    }
                }
            }
        }
        "php" => {
            let mut cursor = node.walk();
            for clause in node.named_children(&mut cursor) {
                match clause.kind() {
                    "base_clause" => {
                        let mut cc = clause.walk();
                        for base in clause.named_children(&mut cc) {
                            if let Some(name) = php_dequalified_name(base, source) {
                                push_name(Relation::Extends, &name);
                            }
                        }
                    }
                    "class_interface_clause" => {
                        let mut cc = clause.walk();
                        for base in clause.named_children(&mut cc) {
                            if let Some(name) = php_dequalified_name(base, source) {
                                push_name(Relation::Implements, &name);
                            }
                        }
                    }
                    _ => {}
                }
            }
            // `use SomeTrait;` in a class body is an `implements` edge
            // (edges note, section 1.5).
            if let Some(body) = node.child_by_field_name("body") {
                let mut bc = body.walk();
                for member in body.named_children(&mut bc) {
                    if member.kind() != "use_declaration" {
                        continue;
                    }
                    let mut uc = member.walk();
                    for base in member.named_children(&mut uc) {
                        if let Some(name) = php_dequalified_name(base, source) {
                            push_name(Relation::Implements, &name);
                        }
                    }
                }
            }
        }
        "swift" => {
            // `class A: B, C` — every `inheritance_specifier` (a direct
            // child of the declaration) wraps a `user_type` whose LAST
            // direct `type_identifier` is the bare supertype name. Swift
            // cannot say syntactically whether a specifier is the
            // superclass or a protocol conformance, so every edge is
            // `extends` (edges note, section 1.4).
            let mut cursor = node.walk();
            for spec in node
                .named_children(&mut cursor)
                .filter(|c| c.kind() == "inheritance_specifier")
            {
                if let Some(name) = swift_user_type_last_identifier(spec, source) {
                    push_name(Relation::Extends, &name);
                }
            }
        }
        "kotlin" => {
            // Every direct `delegation_specifier` child is an `extends`
            // edge. The name is the first `type_identifier` under its
            // `user_type` (Kotlin).
            let mut cursor = node.walk();
            for spec in node
                .named_children(&mut cursor)
                .filter(|c| c.kind() == "delegation_specifier")
            {
                let mut uc = spec.walk();
                let user_type = spec
                    .named_children(&mut uc)
                    .find(|c| c.kind() == "user_type");
                let Some(user_type) = user_type else {
                    continue;
                };
                let mut ic = user_type.walk();
                let ident = user_type
                    .named_children(&mut ic)
                    .find(|c| c.kind() == "type_identifier");
                if let Some(ident) = ident {
                    push(Relation::Extends, ident);
                }
            }
        }
        "r" => {
            // `node` is whatever `describe_r` matched: a `binary_operator`
            // for R6, or the call itself for S4 (edges note, section 1.6).
            let call = if node.kind() == "binary_operator" {
                node.child_by_field_name("rhs")
            } else {
                Some(node)
            };
            let Some(call) = call.filter(|c| c.kind() == "call") else {
                return;
            };
            match r_callee_name(call, source).as_deref() {
                Some("R6Class") => {
                    if let Some(value) = r_named_arg(call, "inherit", source) {
                        if value.kind() == "identifier" {
                            if let Ok(name) = value.utf8_text(source.as_bytes()) {
                                push_name(Relation::Extends, name);
                            }
                        }
                    }
                }
                Some("setClass") => {
                    for name in r_string_or_c_vector(r_named_arg(call, "contains", source), source)
                    {
                        push_name(Relation::Extends, &name);
                    }
                }
                _ => {}
            }
        }
        _ => {
            let mut cursor = node.walk();
            let Some(heritage) = node
                .named_children(&mut cursor)
                .find(|c| c.kind() == "class_heritage")
            else {
                return;
            };
            let mut hc = heritage.walk();
            for clause in heritage.named_children(&mut hc) {
                let relation = match clause.kind() {
                    "extends_clause" => Relation::Extends,
                    "implements_clause" => Relation::Implements,
                    _ => continue,
                };
                let mut cc = clause.walk();
                for base in clause.named_children(&mut cc) {
                    if matches!(base.kind(), "identifier" | "type_identifier") {
                        push(relation, base);
                    }
                }
            }
        }
    }
}

/// Returns a Java heritage clause's base type nodes: the `type_list`'s
/// children when present, else the clause's own named children, minus the
/// keyword token (`extends_clause`/`implements`) itself.
fn java_heritage_entries(clause: TsNode) -> Vec<TsNode> {
    let mut cursor = clause.walk();
    if let Some(list) = clause
        .named_children(&mut cursor)
        .find(|c| c.kind() == "type_list")
    {
        let mut lc = list.walk();
        return list.named_children(&mut lc).collect();
    }
    let mut cursor = clause.walk();
    clause
        .named_children(&mut cursor)
        .filter(|c| c.kind() != "type_list")
        .collect()
}

/// Erases a Java type's type arguments: a `generic_type` reduces to its
/// first child; a qualified name gives `None`.
fn java_heritage_name(node: TsNode, source: &str) -> Option<String> {
    let node = if node.kind() == "generic_type" {
        node.named_child(0)?
    } else {
        node
    };
    if node.kind() != "type_identifier" {
        return None;
    }
    node.utf8_text(source.as_bytes()).ok().map(str::to_string)
}

/// De-qualifies a PHP heritage or `use` name: the last segment after a
/// `\`, for a `name` or `qualified_name` node.
fn php_dequalified_name(node: TsNode, source: &str) -> Option<String> {
    if !matches!(node.kind(), "name" | "qualified_name") {
        return None;
    }
    let text = node.utf8_text(source.as_bytes()).ok()?;
    Some(text.rsplit('\\').next().unwrap_or(text).to_string())
}

/// A call site's callee name, whether it is a member call, its receiver
/// text, its argument count (Java, PHP, Swift), and an explicit kind set
/// to resolve a bare call against (R only).
type CalleeInfo = (String, bool, Option<String>, Option<u32>, Option<Vec<Kind>>);

/// Returns a call site's callee name, whether it is a member call, its
/// receiver text, and its argument count (Java only), per the edge note,
/// section 4, and section 1 for Go, Java, and PHP.
fn callee_info(node: TsNode, source: &str, lang: &str) -> Option<CalleeInfo> {
    match lang {
        "python" => {
            let func = node.child_by_field_name("function")?;
            if func.kind() == "attribute" {
                let name = func
                    .child_by_field_name("attribute")?
                    .utf8_text(source.as_bytes())
                    .ok()?
                    .to_string();
                let object = func.child_by_field_name("object")?;
                return Some((name, true, py_receiver(object, source), None, None));
            }
            if func.kind() == "identifier" {
                return Some((
                    func.utf8_text(source.as_bytes()).ok()?.to_string(),
                    false,
                    None,
                    None,
                    None,
                ));
            }
            None
        }
        "go" => {
            let func = node.child_by_field_name("function")?;
            match func.kind() {
                "identifier" => Some((
                    func.utf8_text(source.as_bytes()).ok()?.to_string(),
                    false,
                    None,
                    None,
                    None,
                )),
                "selector_expression" => {
                    let field = func.child_by_field_name("field")?;
                    let name = field.utf8_text(source.as_bytes()).ok()?.to_string();
                    let operand = func.child_by_field_name("operand")?;
                    let receiver = if operand.kind() == "identifier" {
                        operand
                            .utf8_text(source.as_bytes())
                            .ok()
                            .map(str::to_string)
                    } else {
                        None
                    };
                    Some((name, true, receiver, None, None))
                }
                _ => None,
            }
        }
        "java" => match node.kind() {
            "method_invocation" => {
                let name_node = node.child_by_field_name("name")?;
                let name = name_node.utf8_text(source.as_bytes()).ok()?.to_string();
                let arg_count = node
                    .child_by_field_name("arguments")
                    .map(|a| a.named_child_count() as u32);
                match node.child_by_field_name("object") {
                    None => Some((name, true, Some("this".to_string()), arg_count, None)),
                    Some(object) => {
                        Some((name, true, java_receiver(object, source), arg_count, None))
                    }
                }
            }
            "object_creation_expression" => {
                let type_node = node.child_by_field_name("type")?;
                let name = java_constructed_type_name(type_node, source)?;
                let arg_count = node
                    .child_by_field_name("arguments")
                    .map(|a| a.named_child_count() as u32);
                Some((name, false, None, arg_count, None))
            }
            _ => None,
        },
        "php" => php_callee_info(node, source).map(|(n, v, r, a)| (n, v, r, a, None)),
        "r" => r_callee_info(node, source),
        "swift" => swift_callee_info(node, source),
        "kotlin" => kotlin_callee_info(node, source).map(|(n, v, r)| (n, v, r, None, None)),
        _ => {
            let func = node.child_by_field_name("function")?;
            match func.kind() {
                "identifier" => Some((
                    func.utf8_text(source.as_bytes()).ok()?.to_string(),
                    false,
                    None,
                    None,
                    None,
                )),
                "member_expression" => {
                    let prop = func.child_by_field_name("property")?;
                    let name = prop.utf8_text(source.as_bytes()).ok()?.to_string();
                    let object = func.child_by_field_name("object")?;
                    Some((name, true, ts_receiver(object, source), None, None))
                }
                _ => None,
            }
        }
    }
}

/// R's callee: `self$m()`/`private$m()` resolves through `self`; `super$m()`
/// through `super`; any other `obj$m()` is a plain name match over both
/// `function` and `method` kinds; `pkg::fn()` is a bare call (edges note,
/// section 1.6). Returns `None` for the class-defining `R6Class(...)`/mixin
/// `list(...)` call itself, already consumed by `describe_r`.
fn r_callee_info(node: TsNode, source: &str) -> Option<CalleeInfo> {
    if r_is_consumed_class_call(node, source) {
        return None;
    }
    let func = node.child_by_field_name("function")?;
    match func.kind() {
        "extract_operator" => {
            let rhs = func.child_by_field_name("rhs")?;
            if rhs.kind() != "identifier" {
                return None;
            }
            let name = rhs.utf8_text(source.as_bytes()).ok()?.to_string();
            if let Some(lhs) = func.child_by_field_name("lhs") {
                if lhs.kind() == "identifier" {
                    if let Ok(lhs_text) = lhs.utf8_text(source.as_bytes()) {
                        if lhs_text == "self" || lhs_text == "private" {
                            return Some((name, true, Some("self".to_string()), None, None));
                        }
                        if lhs_text == "super" {
                            return Some((name, true, Some("super".to_string()), None, None));
                        }
                    }
                }
            }
            Some((
                name,
                false,
                None,
                None,
                Some(vec![Kind::Function, Kind::Method]),
            ))
        }
        "namespace_operator" => {
            let rhs = func.child_by_field_name("rhs")?;
            if rhs.kind() != "identifier" {
                return None;
            }
            Some((
                rhs.utf8_text(source.as_bytes()).ok()?.to_string(),
                false,
                None,
                None,
                None,
            ))
        }
        "identifier" => Some((
            func.utf8_text(source.as_bytes()).ok()?.to_string(),
            false,
            None,
            None,
            None,
        )),
        _ => None,
    }
}

/// Swift's callee: the Kotlin shape (a `call_expression` pairs a callee
/// expression with a `call_suffix`), plus a one-hop `self.<field>`
/// receiver (edges note, section 1.4).
fn swift_callee_info(node: TsNode, source: &str) -> Option<CalleeInfo> {
    let mut cursor = node.walk();
    let target = node.named_children(&mut cursor).next()?;
    let arg_count = swift_arg_count(node);
    match target.kind() {
        "simple_identifier" => Some((
            target.utf8_text(source.as_bytes()).ok()?.to_string(),
            false,
            None,
            arg_count,
            None,
        )),
        "navigation_expression" => {
            let mut tc = target.walk();
            let suffix = target
                .named_children(&mut tc)
                .find(|c| c.kind() == "navigation_suffix")?;
            let mut sc = suffix.walk();
            let name_node = suffix
                .named_children(&mut sc)
                .find(|c| c.kind() == "simple_identifier")?;
            let name = name_node.utf8_text(source.as_bytes()).ok()?.to_string();
            let mut rc = target.walk();
            let receiver_node = target.named_children(&mut rc).next();
            let receiver = if let Some(receiver_node) = receiver_node {
                match receiver_node.kind() {
                    "simple_identifier" => receiver_node
                        .utf8_text(source.as_bytes())
                        .ok()
                        .map(str::to_string),
                    "self_expression" => Some("self".to_string()),
                    "super_expression" => Some("super".to_string()),
                    "navigation_expression" => swift_self_field_receiver(receiver_node, source),
                    _ => None,
                }
            } else {
                None
            };
            Some((name, true, receiver, arg_count, None))
        }
        _ => None,
    }
}

/// Swift's one-hop `self.<field>` receiver: `self.repo.save()` binds like
/// TS's `this.x` (edges note, section 1.4).
fn swift_self_field_receiver(node: TsNode, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    let head = node.named_children(&mut cursor).next()?;
    if head.kind() != "self_expression" {
        return None;
    }
    let mut sc = node.walk();
    let suffix = node
        .named_children(&mut sc)
        .find(|c| c.kind() == "navigation_suffix")?;
    let mut fc = suffix.walk();
    let field = suffix
        .named_children(&mut fc)
        .find(|c| c.kind() == "simple_identifier")?;
    field
        .utf8_text(source.as_bytes())
        .ok()
        .map(|f| format!("self.{f}"))
}

/// Kotlin's callee: a `call_expression`'s first named child is either a bare
/// `simple_identifier` callee, or a `navigation_expression` whose
/// `navigation_suffix` holds the member name. The receiver is the first named
/// child of the `navigation_expression`. It must be a `simple_identifier`,
/// `this_expression` or `super_expression`. Any other receiver (a chained call
/// or a literal) gives no call edge at all. Kotlin sets no `argCount`.
fn kotlin_callee_info(node: TsNode, source: &str) -> Option<(String, bool, Option<String>)> {
    let text = |n: TsNode| n.utf8_text(source.as_bytes()).ok().map(str::to_string);
    let mut cursor = node.walk();
    let target = node.named_children(&mut cursor).next()?;
    match target.kind() {
        "simple_identifier" => Some((text(target)?, false, None)),
        "navigation_expression" => {
            let mut tc = target.walk();
            let suffix = target
                .named_children(&mut tc)
                .find(|c| c.kind() == "navigation_suffix")?;
            let mut sc = suffix.walk();
            let name = suffix
                .named_children(&mut sc)
                .find(|c| c.kind() == "simple_identifier")?;
            let mut rc = target.walk();
            let receiver = target.named_children(&mut rc).next()?;
            let receiver = match receiver.kind() {
                "simple_identifier" => text(receiver)?,
                "this_expression" => "this".to_string(),
                "super_expression" => "super".to_string(),
                _ => return None,
            };
            Some((text(name)?, true, Some(receiver)))
        }
        _ => None,
    }
}

/// Java `javaReceiver`: an identifier, `this`, or `this.<field>`.
fn java_receiver(object: TsNode, source: &str) -> Option<String> {
    match object.kind() {
        "identifier" => object.utf8_text(source.as_bytes()).ok().map(str::to_string),
        "this" => Some("this".to_string()),
        "field_access" => {
            let inner = object.child_by_field_name("object")?;
            if inner.kind() != "this" {
                return None;
            }
            let field = object.child_by_field_name("field")?;
            let field_text = field.utf8_text(source.as_bytes()).ok()?;
            Some(format!("this.{field_text}"))
        }
        _ => None,
    }
}

/// Java `javaConstructedTypeName`: a `generic_type` reduces to its first
/// child; a `type_identifier` passes; a qualified name gives no name, so
/// `new a.b.C` emits no calls edge.
fn java_constructed_type_name(node: TsNode, source: &str) -> Option<String> {
    let node = if node.kind() == "generic_type" {
        node.named_child(0)?
    } else {
        node
    };
    if node.kind() != "type_identifier" {
        return None;
    }
    node.utf8_text(source.as_bytes()).ok().map(str::to_string)
}

/// PHP `phpCallee` across the four call types: a plain function call, a
/// member (nullsafe or not) call, and a scoped (static) call.
fn php_callee_info(
    node: TsNode,
    source: &str,
) -> Option<(String, bool, Option<String>, Option<u32>)> {
    let arg_count = |args_field: Option<TsNode>| args_field.map(|a| a.named_child_count() as u32);
    match node.kind() {
        "function_call_expression" => {
            let func = node.child_by_field_name("function")?;
            if func.kind() != "name" {
                // `$fn` gives no callee.
                return None;
            }
            let name = func.utf8_text(source.as_bytes()).ok()?.to_string();
            Some((
                name,
                false,
                None,
                arg_count(node.child_by_field_name("arguments")),
            ))
        }
        "member_call_expression" | "nullsafe_member_call_expression" => {
            let name_node = node.child_by_field_name("name")?;
            let name = name_node.utf8_text(source.as_bytes()).ok()?.to_string();
            let object = node.child_by_field_name("object")?;
            let receiver = php_receiver(object, source);
            Some((
                name,
                true,
                receiver,
                arg_count(node.child_by_field_name("arguments")),
            ))
        }
        "scoped_call_expression" => {
            let name_node = node.child_by_field_name("name")?;
            let name = name_node.utf8_text(source.as_bytes()).ok()?.to_string();
            let scope = node.child_by_field_name("scope")?;
            let receiver = php_scope_receiver(scope, source);
            Some((
                name,
                true,
                receiver,
                arg_count(node.child_by_field_name("arguments")),
            ))
        }
        _ => None,
    }
}

/// PHP receiver for a member call: a `$this` operand becomes `this`; any
/// other `$var` operand keeps its `$` (edges note, section 1.5, "keys
/// keep the `$`").
fn php_receiver(object: TsNode, source: &str) -> Option<String> {
    if object.kind() != "variable_name" {
        return None;
    }
    let text = object.utf8_text(source.as_bytes()).ok()?;
    Some(if text == "$this" {
        "this".to_string()
    } else {
        text.to_string()
    })
}

/// PHP receiver for a scoped (static) call: `self`, `static`, and
/// `parent` all map to `self`; a class name operand is kept de-qualified
/// (`phpScopeReceiver`).
fn php_scope_receiver(scope: TsNode, source: &str) -> Option<String> {
    match scope.kind() {
        "relative_scope" => Some("self".to_string()),
        "name" | "qualified_name" => {
            let text = scope.utf8_text(source.as_bytes()).ok()?;
            if matches!(text, "self" | "static" | "parent") {
                return Some("self".to_string());
            }
            php_dequalified_name(scope, source)
        }
        _ => None,
    }
}

/// TS `tsReceiver`: `this`, a bare identifier, or `this.<prop>`.
fn ts_receiver(object: TsNode, source: &str) -> Option<String> {
    match object.kind() {
        "this" => Some("this".to_string()),
        "identifier" => object.utf8_text(source.as_bytes()).ok().map(str::to_string),
        "member_expression" => {
            let inner = object.child_by_field_name("object")?;
            if inner.kind() != "this" {
                return None;
            }
            let prop = object.child_by_field_name("property")?;
            let prop_text = prop.utf8_text(source.as_bytes()).ok()?;
            Some(format!("this.{prop_text}"))
        }
        _ => None,
    }
}

/// Python `pyReceiver`: a bare identifier, or `self.<attr>`.
fn py_receiver(object: TsNode, source: &str) -> Option<String> {
    match object.kind() {
        "identifier" => object.utf8_text(source.as_bytes()).ok().map(str::to_string),
        "attribute" => {
            let inner = object.child_by_field_name("object")?;
            if inner.kind() != "identifier" || inner.utf8_text(source.as_bytes()).ok()? != "self" {
                return None;
            }
            let attr = object.child_by_field_name("attribute")?;
            let attr_text = attr.utf8_text(source.as_bytes()).ok()?;
            Some(format!("self.{attr_text}"))
        }
        _ => None,
    }
}

/// Collects every TS import binding by its local name: each
/// `import_specifier` (named or aliased), each default clause identifier and
/// each `* as ns` namespace import. Empty for Python (edge note, section 5).
fn collect_imported_symbols(
    root: TsNode,
    source: &str,
    is_python: bool,
) -> HashMap<String, ImportBinding> {
    let mut map = HashMap::new();
    if is_python {
        return map;
    }
    let text = |n: TsNode| n.utf8_text(source.as_bytes()).ok().map(str::to_string);
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "import_specifier" => {
                if let Some((local, imported, specifier)) = imported_symbol_entry(node, source) {
                    let imported = ImportedName::Named(imported);
                    map.insert(
                        local,
                        ImportBinding {
                            imported,
                            specifier,
                        },
                    );
                }
            }
            "import_clause" => {
                let specifier = find_import_statement(node)
                    .and_then(|stmt| import_specifier(stmt, source, "typescript"));
                if let Some(specifier) = specifier {
                    let mut cursor = node.walk();
                    for child in node.named_children(&mut cursor) {
                        let (local, imported) = match child.kind() {
                            "identifier" => (text(child), ImportedName::Default),
                            "namespace_import" => {
                                let mut ic = child.walk();
                                let id = child
                                    .named_children(&mut ic)
                                    .find(|c| c.kind() == "identifier");
                                (id.and_then(text), ImportedName::Namespace)
                            }
                            _ => continue,
                        };
                        if let Some(local) = local {
                            let specifier = specifier.clone();
                            map.insert(
                                local,
                                ImportBinding {
                                    imported,
                                    specifier,
                                },
                            );
                        }
                    }
                }
            }
            _ => {}
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            stack.push(child);
        }
    }
    map
}

/// Reads one `import_specifier`'s `(local name, imported name, specifier)`.
fn imported_symbol_entry(node: TsNode, source: &str) -> Option<(String, String, String)> {
    let stmt = find_import_statement(node)?;
    let specifier = import_specifier(stmt, source, "typescript")?;
    let name_node = node.child_by_field_name("name")?;
    let imported = name_node.utf8_text(source.as_bytes()).ok()?.to_string();
    let local_node = node.child_by_field_name("alias").unwrap_or(name_node);
    let local = local_node.utf8_text(source.as_bytes()).ok()?.to_string();
    Some((local, imported, specifier))
}

/// Walks up from `node` to the nearest `import_statement` ancestor.
fn find_import_statement(node: TsNode) -> Option<TsNode> {
    let mut current = node.parent();
    while let Some(n) = current {
        if n.kind() == "import_statement" {
            return Some(n);
        }
        current = n.parent();
    }
    None
}

/// Reports whether `node` is a direct callee: the `function` field of a
/// `call_expression` or `call` node.
fn is_direct_callee(node: TsNode) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    matches!(parent.kind(), "call_expression" | "call")
        && parent.child_by_field_name("function") == Some(node)
}

/// Reports whether `node` is the `name` field of its parent, such as a
/// declaration's own name.
fn is_declaration_name(node: TsNode) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    parent.child_by_field_name("name") == Some(node)
}

/// Emits a `references` raw edge for an identifier that names an imported
/// symbol, is not a direct callee, and is not a declaration name, per the
/// edge note, section 5.
fn maybe_emit_reference(
    node: TsNode,
    ctx: &Ctx,
    path: &str,
    source: &str,
    edges: &mut Vec<RawEdge>,
) {
    let Ok(text) = node.utf8_text(source.as_bytes()) else {
        return;
    };
    if ctx.shadowed.contains(text) {
        return;
    }
    let Some(ImportBinding {
        imported: ImportedName::Named(imported_name),
        specifier,
    }) = ctx.imported_symbols.get(text)
    else {
        return;
    };
    if is_direct_callee(node) || is_declaration_name(node) {
        return;
    }
    edges.push(RawEdge {
        source: ctx.parent_id.clone(),
        relation: Relation::References,
        file: path.to_string(),
        target_id: None,
        specifier: Some(specifier.clone()),
        name: Some(imported_name.clone()),
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

/// One matched definition, normalized across every language Sieve
/// extracts.
pub(crate) struct Desc<'a> {
    /// The bare symbol name: used for the node's `name`, and for call
    /// resolution.
    pub name: String,
    /// The id-scope segment when it differs from `name` (Go: a method's
    /// `Recv.method`). `None` for TS and Python.
    pub id_name: Option<String>,
    pub kind: Kind,
    /// The byte offset where the signature ends and the body starts.
    pub header_end: usize,
    /// The node whose text forms `body_hash`, `body_text`, and the span.
    /// TS and Python always set this to the matched node itself; R (later)
    /// sets it to a different node (the call, or the nested function).
    pub hash_node: TsNode<'a>,
    /// An explicit owner, for a method that does not lexically nest inside
    /// its class (Go's receiver methods, R's S3/S4 methods). `None` for TS
    /// and Python; the enclosing-class fallback applies instead.
    pub owner: Option<String>,
    /// The declared parameter count, for overload disambiguation (Java).
    /// `None` for TS and Python.
    pub arity: Option<u32>,
    /// Whether the last parameter is a vararg, so `arity` is a minimum.
    /// `None` for TS and Python.
    pub variadic: Option<bool>,
}

/// Decides whether `node` is a definition, and returns its descriptor.
/// Returns `None` for a node that is not a definition, has no `name`
/// field, or whose name field's text is not valid UTF-8 (see the per-language
/// `describe*` functions).
pub(crate) fn describe<'a>(
    node: TsNode<'a>,
    enclosing_kind: Option<Kind>,
    enclosing_class: Option<&str>,
    source: &str,
    lang: &str,
) -> Option<Desc<'a>> {
    match lang {
        "python" => describe_python(node, enclosing_kind, source),
        "go" => describe_go(node, source),
        "java" => describe_java(node, source),
        "php" => describe_php(node, source),
        "swift" => describe_swift(node, enclosing_kind, enclosing_class, source),
        "kotlin" => describe_kotlin(node, enclosing_kind, enclosing_class, source),
        _ => describe_ts(node, source),
    }
}

/// `describe` for Python: a class or a function, promoted to method inside
/// a class body.
fn describe_python<'a>(
    node: TsNode<'a>,
    enclosing_kind: Option<Kind>,
    source: &str,
) -> Option<Desc<'a>> {
    let kind = match node.kind() {
        "class_definition" => Kind::Class,
        "function_definition" => {
            if enclosing_kind == Some(Kind::Class) {
                Kind::Method
            } else {
                Kind::Function
            }
        }
        _ => return None,
    };
    let name_node = node.child_by_field_name("name")?;
    let name = name_node.utf8_text(source.as_bytes()).ok()?.to_string();
    Some(Desc {
        name,
        id_name: None,
        kind,
        header_end: header_end(node, false),
        hash_node: node,
        owner: None,
        arity: None,
        variadic: None,
    })
}

/// `describe` for TypeScript, TSX and the JavaScript label.
fn describe_ts<'a>(node: TsNode<'a>, source: &str) -> Option<Desc<'a>> {
    let kind = match node.kind() {
        "class_declaration" | "abstract_class_declaration" => Kind::Class,
        "function_declaration" | "generator_function_declaration" => Kind::Function,
        "method_definition" => Kind::Method,
        "interface_declaration" => Kind::Interface,
        "type_alias_declaration" => Kind::Type,
        "enum_declaration" => Kind::Enum,
        "variable_declarator" => {
            let value = node.child_by_field_name("value")?;
            if FUNCTION_VALUE_TYPES.contains(&value.kind()) {
                Kind::Function
            } else {
                return None;
            }
        }
        _ => return None,
    };
    let name_node = node.child_by_field_name("name")?;
    let name = name_node.utf8_text(source.as_bytes()).ok()?.to_string();
    let is_declarator = node.kind() == "variable_declarator";
    Some(Desc {
        name,
        id_name: None,
        kind,
        header_end: header_end(node, is_declarator),
        hash_node: node,
        owner: None,
        arity: None,
        variadic: None,
    })
}

/// `describe` for Go: a function, a receiver method, or a `type_spec`
/// widened to struct, interface, or a plain type alias (`describeGo`).
fn describe_go<'a>(node: TsNode<'a>, source: &str) -> Option<Desc<'a>> {
    match node.kind() {
        "function_declaration" => {
            let name_node = node.child_by_field_name("name")?;
            let name = name_node.utf8_text(source.as_bytes()).ok()?.to_string();
            Some(Desc {
                name,
                id_name: None,
                kind: Kind::Function,
                header_end: header_end(node, false),
                hash_node: node,
                owner: None,
                arity: None,
                variadic: None,
            })
        }
        "method_declaration" => {
            let name_node = node.child_by_field_name("name")?;
            let name = name_node.utf8_text(source.as_bytes()).ok()?.to_string();
            let owner = go_receiver_type(node, source);
            let id_name = owner.as_ref().map(|o| format!("{o}.{name}"));
            Some(Desc {
                name,
                id_name,
                kind: Kind::Method,
                header_end: header_end(node, false),
                hash_node: node,
                owner,
                arity: None,
                variadic: None,
            })
        }
        "type_spec" => {
            let name_node = node.child_by_field_name("name")?;
            let name = name_node.utf8_text(source.as_bytes()).ok()?.to_string();
            let type_field = node.child_by_field_name("type");
            let kind = match type_field.map(|t| t.kind()) {
                Some("struct_type") => Kind::Struct,
                Some("interface_type") => Kind::Interface,
                _ => Kind::Type,
            };
            let header_end = match kind {
                Kind::Struct | Kind::Interface => type_field
                    .map(|t| t.start_byte())
                    .unwrap_or_else(|| node.end_byte()),
                _ => node.end_byte(),
            };
            Some(Desc {
                name,
                id_name: None,
                kind,
                header_end,
                hash_node: node,
                owner: None,
                arity: None,
                variadic: None,
            })
        }
        _ => None,
    }
}

/// Returns a Go method's receiver type name: the `type_identifier` of its
/// receiver parameter, one pointer level unwrapped (`goReceiverType`).
fn go_receiver_type(node: TsNode, source: &str) -> Option<String> {
    let receiver = node.child_by_field_name("receiver")?;
    let mut cursor = receiver.walk();
    let param = receiver
        .named_children(&mut cursor)
        .find(|c| c.kind() == "parameter_declaration")?;
    let mut type_node = param.child_by_field_name("type")?;
    if type_node.kind() == "pointer_type" {
        type_node = type_node.named_child(0)?;
    }
    if type_node.kind() != "type_identifier" {
        return None;
    }
    type_node
        .utf8_text(source.as_bytes())
        .ok()
        .map(str::to_string)
}

/// Returns a Go method's receiver variable name, for example `g` in
/// `func (g Greeter) Greet()` (edges note, section 1.1: the receiver
/// variable maps to the enclosing class inside the method body).
fn go_receiver_var_name(node: TsNode, source: &str) -> Option<String> {
    let receiver = node.child_by_field_name("receiver")?;
    let mut cursor = receiver.walk();
    let param = receiver
        .named_children(&mut cursor)
        .find(|c| c.kind() == "parameter_declaration")?;
    let name_node = param.child_by_field_name("name")?;
    name_node
        .utf8_text(source.as_bytes())
        .ok()
        .map(str::to_string)
}

/// `describe` for Java: interfaces, classes, enums, a record widened to
/// struct, and every method-shaped declaration (`describeJava`).
fn describe_java<'a>(node: TsNode<'a>, source: &str) -> Option<Desc<'a>> {
    let kind = match node.kind() {
        "interface_declaration" | "annotation_type_declaration" => Kind::Interface,
        "class_declaration" => Kind::Class,
        "enum_declaration" => Kind::Enum,
        "record_declaration" => Kind::Struct,
        "method_declaration"
        | "constructor_declaration"
        | "annotation_type_element_declaration" => Kind::Method,
        _ => return None,
    };
    let name_node = node.child_by_field_name("name")?;
    let name = name_node.utf8_text(source.as_bytes()).ok()?.to_string();
    // Never set `arity` on a record's own `parameters` node, even
    // though the shape matches a method's.
    let (arity, variadic) = if matches!(
        node.kind(),
        "method_declaration" | "constructor_declaration"
    ) {
        java_arity(node)
    } else {
        (None, None)
    };
    Some(Desc {
        name,
        id_name: None,
        kind,
        header_end: header_end(node, false),
        hash_node: node,
        owner: None,
        arity,
        variadic,
    })
}

/// Counts a Java callable's `formal_parameter` and `spread_parameter`
/// children, and reports whether a `spread_parameter` is present.
fn java_arity(node: TsNode) -> (Option<u32>, Option<bool>) {
    let Some(params) = node.child_by_field_name("parameters") else {
        return (None, None);
    };
    let mut count = 0u32;
    let mut variadic = false;
    let mut cursor = params.walk();
    for child in params.named_children(&mut cursor) {
        match child.kind() {
            "formal_parameter" => count += 1,
            "spread_parameter" => {
                count += 1;
                variadic = true;
            }
            _ => {}
        }
    }
    // Never write `variadic: false`; the field is present only
    // when a spread parameter makes it true.
    (Some(count), variadic.then_some(true))
}

/// Closure name: the `$` stripped variable an assignment binds the closure
/// to, else `{closure}` (`phpClosureName`).
fn php_closure_name(node: TsNode, source: &str) -> String {
    node.parent()
        .filter(|p| p.kind() == "assignment_expression")
        .filter(|p| p.child_by_field_name("right").map(|r| r.id()) == Some(node.id()))
        .and_then(|p| p.child_by_field_name("left"))
        .filter(|l| l.kind() == "variable_name")
        .and_then(|l| l.utf8_text(source.as_bytes()).ok())
        .map(|t| t.strip_prefix('$').unwrap_or(t).to_string())
        .unwrap_or_else(|| "{closure}".to_string())
}

/// `describe` for PHP: an interface, a trait, a class, an enum, a
/// top-level function, and a method (section 1.5).
fn describe_php<'a>(node: TsNode<'a>, source: &str) -> Option<Desc<'a>> {
    // A closure is a function node.
    if matches!(node.kind(), "anonymous_function" | "arrow_function") {
        return Some(Desc {
            name: php_closure_name(node, source),
            id_name: None,
            kind: Kind::Function,
            header_end: header_end(node, false),
            hash_node: node,
            owner: None,
            arity: None,
            variadic: None,
        });
    }
    let kind = match node.kind() {
        "interface_declaration" => Kind::Interface,
        "trait_declaration" => Kind::Trait,
        "class_declaration" => Kind::Class,
        "enum_declaration" => Kind::Enum,
        "function_definition" => Kind::Function,
        "method_declaration" => Kind::Method,
        _ => return None,
    };
    let name_node = node.child_by_field_name("name")?;
    let name = name_node.utf8_text(source.as_bytes()).ok()?.to_string();
    Some(Desc {
        name,
        id_name: None,
        kind,
        header_end: header_end(node, false),
        hash_node: node,
        owner: None,
        arity: None,
        variadic: None,
    })
}

/// `describe` for Kotlin: a class-shaped declaration widened to class, enum, or
/// interface; an object; a function promoted to method inside a type body; a
/// secondary constructor named after the enclosing class; a type alias; and a
/// top-level property. `tree-sitter-kotlin-sg` (the fwcd grammar) has no `name`
/// or `body` fields, so every rule reads a child by kind: the first direct
/// `type_identifier` or `simple_identifier` is the name.
fn describe_kotlin<'a>(
    node: TsNode<'a>,
    enclosing_kind: Option<Kind>,
    enclosing_class: Option<&str>,
    source: &str,
) -> Option<Desc<'a>> {
    let name_of = |kind: &str| {
        kotlin_child(node, kind)
            .and_then(|n| n.utf8_text(source.as_bytes()).ok())
            .map(str::to_string)
    };
    let desc = |name: String, kind: Kind, header_end: usize| Desc {
        name,
        id_name: None,
        kind,
        header_end,
        hash_node: node,
        owner: None,
        arity: None,
        variadic: None,
    };
    match node.kind() {
        "class_declaration" => {
            let name = name_of("type_identifier")?;
            let kind = if kotlin_child(node, "enum_class_body").is_some() {
                Kind::Enum
            } else if kotlin_has_anon_child(node, "interface")
                || kotlin_is_annotation_class(node, source)
            {
                Kind::Interface
            } else {
                Kind::Class
            };
            // Both body kinds end the header (DV17: `enum_class_body`).
            let end = kotlin_header_end(node, &["class_body", "enum_class_body"]);
            Some(desc(name, kind, end))
        }
        "object_declaration" => {
            let name = name_of("type_identifier")?;
            Some(desc(
                name,
                Kind::Class,
                kotlin_header_end(node, &["class_body"]),
            ))
        }
        "function_declaration" => {
            let name = name_of("simple_identifier")?;
            let kind = if enclosing_kind.is_some_and(|k| KOTLIN_TYPE_KINDS.contains(&k)) {
                Kind::Method
            } else {
                Kind::Function
            };
            Some(desc(
                name,
                kind,
                kotlin_header_end(node, &["function_body"]),
            ))
        }
        "secondary_constructor" => {
            let name = enclosing_class?.to_string();
            Some(desc(
                name,
                Kind::Method,
                kotlin_header_end(node, &["statements"]),
            ))
        }
        "type_alias" => {
            let name = name_of("type_identifier")?;
            Some(desc(name, Kind::Type, node.end_byte()))
        }
        "property_declaration" => {
            // Only a top-level `val`/`var`; a stored property inside a
            // type body is a field, not a definition node.
            if enclosing_kind.is_some() {
                return None;
            }
            let name = kotlin_property_name(node, source)?;
            Some(desc(name, Kind::Variable, node.end_byte()))
        }
        // No node for a `companion_object`: `KOTLIN_KINDS`
        // lists `object_declaration` only. Its members land at the
        // enclosing class's scope, so `walk` descends with the same `Ctx`.
        "companion_object" => None,
        _ => None,
    }
}

/// Returns `node`'s first direct NAMED child of kind `kind`.
fn kotlin_child<'a>(node: TsNode<'a>, kind: &str) -> Option<TsNode<'a>> {
    let mut cursor = node.walk();
    let found = node.named_children(&mut cursor).find(|c| c.kind() == kind);
    found
}

/// Reports whether `node` has a direct, unnamed (anonymous token) child
/// whose kind text equals `token`, for example the `interface` keyword on
/// a `class_declaration`-shaped node.
fn kotlin_has_anon_child(node: TsNode, token: &str) -> bool {
    let mut cursor = node.walk();
    let found = node
        .children(&mut cursor)
        .any(|c| !c.is_named() && c.kind() == token);
    found
}

/// Reports whether `node`'s `modifiers` child holds a `class_modifier`
/// whose text reads `annotation` (an annotation class is interface-shaped,
/// edges note section 1.3).
fn kotlin_is_annotation_class(node: TsNode, source: &str) -> bool {
    let Some(modifiers) = kotlin_child(node, "modifiers") else {
        return false;
    };
    let mut cursor = modifiers.walk();
    let found = modifiers
        .named_children(&mut cursor)
        .filter(|c| c.kind() == "class_modifier")
        .any(|c| c.utf8_text(source.as_bytes()) == Ok("annotation"));
    found
}

/// Returns the byte offset where a Kotlin declaration's header ends: the
/// start of its first direct child of one of `body_kinds`, else the
/// node's own end (the grammar sets no `body` field).
fn kotlin_header_end(node: TsNode, body_kinds: &[&str]) -> usize {
    let mut cursor = node.walk();
    let found = node
        .named_children(&mut cursor)
        .find(|c| body_kinds.contains(&c.kind()))
        .map(|b| b.start_byte());
    found.unwrap_or_else(|| node.end_byte())
}

/// A top-level `property_declaration`'s bound name: the `variable_declaration`
/// child's first `simple_identifier`.
fn kotlin_property_name(node: TsNode, source: &str) -> Option<String> {
    let decl = kotlin_child(node, "variable_declaration")?;
    let mut cursor = decl.walk();
    let ident = decl
        .named_children(&mut cursor)
        .find(|c| c.kind() == "simple_identifier")?;
    ident.utf8_text(source.as_bytes()).ok().map(str::to_string)
}

/// Kotlin `kotlinExported`: no `modifiers` child means true; else the
/// `visibility_modifier` inside `modifiers` is absent (true) or reads
/// `public` (edges note, section 1.3).
fn kotlin_exported(node: TsNode, source: &str) -> bool {
    let Some(modifiers) = kotlin_child(node, "modifiers") else {
        return true;
    };
    let mut cursor = modifiers.walk();
    let Some(vis) = modifiers
        .named_children(&mut cursor)
        .find(|c| c.kind() == "visibility_modifier")
    else {
        return true;
    };
    vis.utf8_text(source.as_bytes()) == Ok("public")
}

/// Swift kinds that host their own members: class, struct, enum, and
/// interface (a protocol), plus module (an extension body) — a
/// member-contributing scope named after the extended type, deliberately
/// not a real type kind for name resolution.
const SWIFT_TYPE_KINDS: [Kind; 5] = [
    Kind::Class,
    Kind::Struct,
    Kind::Enum,
    Kind::Interface,
    Kind::Module,
];

/// Kotlin kinds that host their own members, and promote a nested
/// function to a method: class, interface, and enum (edges note, section
/// 1.3, `KOTLIN_TYPE_KINDS`).
const KOTLIN_TYPE_KINDS: [Kind; 3] = [Kind::Class, Kind::Interface, Kind::Enum];

/// `describe` for Swift: one `class_declaration` node covers class,
/// struct, enum, actor, and extension; a protocol; a function promoted to
/// method inside a type body; an initializer named after its enclosing
/// class; a top-level typealias; and a top-level property (edges note,
/// section 1.4, `describeSwift`).
fn describe_swift<'a>(
    node: TsNode<'a>,
    enclosing_kind: Option<Kind>,
    enclosing_class: Option<&str>,
    source: &str,
) -> Option<Desc<'a>> {
    match node.kind() {
        "class_declaration" => describe_swift_class(node, source),
        "protocol_declaration" => {
            let name = swift_type_identifier(node, source)?;
            Some(Desc {
                name,
                id_name: None,
                kind: Kind::Interface,
                header_end: header_end(node, false),
                hash_node: node,
                owner: None,
                arity: None,
                variadic: None,
            })
        }
        "function_declaration" | "protocol_function_declaration" => {
            let name = swift_simple_identifier(node, source)?;
            let kind = if node.kind() == "protocol_function_declaration"
                || enclosing_kind.is_some_and(|k| SWIFT_TYPE_KINDS.contains(&k))
            {
                Kind::Method
            } else {
                Kind::Function
            };
            let (arity, variadic) = swift_arity(node);
            Some(Desc {
                name,
                id_name: None,
                kind,
                header_end: header_end(node, false),
                hash_node: node,
                owner: None,
                arity: Some(arity),
                variadic,
            })
        }
        "init_declaration" => {
            let name = enclosing_class?.to_string();
            let (arity, variadic) = swift_arity(node);
            Some(Desc {
                name,
                id_name: None,
                kind: Kind::Method,
                header_end: header_end(node, false),
                hash_node: node,
                owner: None,
                arity: Some(arity),
                variadic,
            })
        }
        "typealias_declaration" => {
            let name = swift_type_identifier(node, source)?;
            Some(Desc {
                name,
                id_name: None,
                kind: Kind::Type,
                header_end: header_end(node, false),
                hash_node: node,
                owner: None,
                arity: None,
                variadic: None,
            })
        }
        "property_declaration" => {
            // Top-level `let`/`var` only — a stored property inside a type
            // is a field, not a definition node.
            if enclosing_kind.is_some() {
                return None;
            }
            let name = swift_pattern_identifier(node, source)?;
            Some(Desc {
                name,
                id_name: None,
                kind: Kind::Variable,
                header_end: header_end(node, false),
                hash_node: node,
                owner: None,
                arity: None,
                variadic: None,
            })
        }
        _ => None,
    }
}

/// A Swift class-declaration-shaped node's kind and name: the keyword
/// token decides class/struct/enum/actor/extension; an extension names
/// itself after the extended type's last `type_identifier`.
fn describe_swift_class<'a>(node: TsNode<'a>, source: &str) -> Option<Desc<'a>> {
    let keyword = swift_declaration_keyword(node)?;
    let name = if keyword == "extension" {
        swift_user_type_last_identifier(node, source)?
    } else {
        swift_type_identifier(node, source)?
    };
    let kind = match keyword {
        "extension" => Kind::Module,
        "struct" => Kind::Struct,
        "enum" => Kind::Enum,
        // An actor is class-like (reference semantics, methods) and there
        // is no dedicated actor kind, so it takes "class".
        _ => Kind::Class,
    };
    Some(Desc {
        name,
        id_name: None,
        kind,
        header_end: header_end(node, false),
        hash_node: node,
        owner: None,
        arity: None,
        variadic: None,
    })
}

/// Returns the `class`/`struct`/`enum`/`actor`/`extension` keyword token
/// among `node`'s direct children.
fn swift_declaration_keyword(node: TsNode) -> Option<&'static str> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "class" => return Some("class"),
            "struct" => return Some("struct"),
            "enum" => return Some("enum"),
            "actor" => return Some("actor"),
            "extension" => return Some("extension"),
            _ => {}
        }
    }
    None
}

/// The first direct `type_identifier` named child: the declared name of a
/// class-shaped, protocol, or typealias declaration (generic parameters
/// and inheritance specifiers nest under other child nodes).
fn swift_type_identifier(node: TsNode, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    let found = node
        .named_children(&mut cursor)
        .find(|c| c.kind() == "type_identifier")?;
    found.utf8_text(source.as_bytes()).ok().map(str::to_string)
}

/// The first direct `simple_identifier` named child: the declared name of
/// a function-shaped declaration (parameters and generic parameters nest
/// under other child nodes).
fn swift_simple_identifier(node: TsNode, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    let found = node
        .named_children(&mut cursor)
        .find(|c| c.kind() == "simple_identifier")?;
    found.utf8_text(source.as_bytes()).ok().map(str::to_string)
}

/// A top-level `property_declaration`'s bound name: the `pattern` child's
/// own `simple_identifier`.
fn swift_pattern_identifier(node: TsNode, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    let pattern = node
        .named_children(&mut cursor)
        .find(|c| c.kind() == "pattern")?;
    let mut pc = pattern.walk();
    let ident = pattern
        .named_children(&mut pc)
        .find(|c| c.kind() == "simple_identifier")?;
    ident.utf8_text(source.as_bytes()).ok().map(str::to_string)
}

/// Finds the (only) `user_type` among `node`'s direct named children, and
/// returns the LAST `type_identifier` inside it: a module-qualified
/// `Foundation.NSObject` reduces to `NSObject`.
pub(crate) fn swift_user_type_last_identifier(node: TsNode, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    let user_type = node
        .named_children(&mut cursor)
        .find(|c| c.kind() == "user_type")?;
    let mut ic = user_type.walk();
    let ids: Vec<TsNode> = user_type
        .named_children(&mut ic)
        .filter(|c| c.kind() == "type_identifier")
        .collect();
    ids.last()
        .and_then(|n| n.utf8_text(source.as_bytes()).ok())
        .map(str::to_string)
}

/// Declared parameter count for a Swift callable (`swiftArity`).
/// `parameter` nodes are direct children of the
/// declaration; a default value's `=` sits as a sibling token after its
/// parameter, and a variadic `...` sits inside its own parameter. `arity`
/// is the required minimum (parameters minus defaults); `variadic` marks
/// any default or variadic parameter.
fn swift_arity(node: TsNode) -> (u32, Option<bool>) {
    let mut cursor = node.walk();
    let params: Vec<TsNode> = node
        .children(&mut cursor)
        .filter(|c| c.kind() == "parameter")
        .collect();
    let mut dc = node.walk();
    let defaults = node.children(&mut dc).filter(|c| c.kind() == "=").count();
    let has_variadic = params.iter().any(|p| {
        let mut pc = p.walk();
        let found = p.children(&mut pc).any(|c| c.kind() == "...");
        found
    });
    let arity = params.len().saturating_sub(defaults) as u32;
    let variadic = (has_variadic || defaults > 0).then_some(true);
    (arity, variadic)
}

/// Argument count at a Swift call site: the `value_argument`s plus one for
/// a trailing closure.
fn swift_arg_count(node: TsNode) -> Option<u32> {
    let mut cursor = node.walk();
    let suffix = node
        .named_children(&mut cursor)
        .find(|c| c.kind() == "call_suffix")?;
    let mut sc = suffix.walk();
    let args = suffix
        .named_children(&mut sc)
        .find(|c| c.kind() == "value_arguments")
        .map(|value_arguments| {
            let mut vc = value_arguments.walk();
            value_arguments
                .named_children(&mut vc)
                .filter(|c| c.kind() == "value_argument")
                .count()
        })
        .unwrap_or(0);
    let mut lc = suffix.walk();
    let trailing = suffix
        .named_children(&mut lc)
        .any(|c| c.kind() == "lambda_literal");
    Some((args + usize::from(trailing)) as u32)
}

/// Swift `swiftSuperClassName`: the first `inheritance_specifier`'s last
/// `type_identifier`, set only when the matched node's own kind is class.
fn swift_super_class_name(node: TsNode, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    let spec = node
        .named_children(&mut cursor)
        .find(|c| c.kind() == "inheritance_specifier")?;
    swift_user_type_last_identifier(spec, source)
}

/// Dispatches the `exported` rule by grammar. Kotlin comes later. R runs
/// `r_exported` here too, since it needs `ctx.r_r6_access` and the roxygen
/// comment scan off the matched node.
fn exported_for(lang: &str, node: TsNode, name: &str, source: &str, ctx: &Ctx) -> bool {
    match lang {
        "python" => !name.starts_with('_'),
        "go" => go_exported(name),
        "java" => java_exported(node),
        "php" => php_exported(node),
        "swift" => swift_exported(node, source),
        "kotlin" => kotlin_exported(node, source),
        "r" => r_exported(name, ctx, node, source),
        _ => exported_ts(node),
    }
}

/// Swift `swiftExported`: the default (`internal`) is API surface, so
/// only an explicit `private`/`fileprivate` visibility modifier hides a
/// definition.
fn swift_exported(node: TsNode, source: &str) -> bool {
    let mut cursor = node.walk();
    let Some(modifiers) = node
        .named_children(&mut cursor)
        .find(|c| c.kind() == "modifiers")
    else {
        return true;
    };
    let mut mc = modifiers.walk();
    let Some(vis) = modifiers
        .named_children(&mut mc)
        .find(|c| c.kind() == "visibility_modifier")
    else {
        return true;
    };
    let Ok(text) = vis.utf8_text(source.as_bytes()) else {
        return true;
    };
    text != "private" && text != "fileprivate"
}

/// Go `goExported`: the part of `name` after the last `.` starts with an
/// uppercase letter. A bare name has no `.`, so this
/// checks the whole name.
fn go_exported(name: &str) -> bool {
    let tail = name.rsplit('.').next().unwrap_or(name);
    tail.chars().next().is_some_and(char::is_uppercase)
}

/// Java `javaExported`: the `modifiers` child holds a `public` or
/// `protected` token; no `modifiers` child means false.
fn java_exported(node: TsNode) -> bool {
    let Some(modifiers) = find_child(node, "modifiers") else {
        return false;
    };
    let mut cursor = modifiers.walk();
    let found = modifiers
        .children(&mut cursor)
        .any(|c| matches!(c.kind(), "public" | "protected"));
    found
}

/// PHP `phpExported`: a `visibility_modifier` child must read `public`;
/// no `visibility_modifier` child means true.
fn php_exported(node: TsNode) -> bool {
    let Some(modifier) = find_child(node, "visibility_modifier") else {
        return true;
    };
    let mut cursor = modifier.walk();
    let found = modifier.children(&mut cursor).any(|c| c.kind() == "public");
    found
}

/// Returns `node`'s first direct child of kind `kind`, named or not.
fn find_child<'a>(node: TsNode<'a>, kind: &str) -> Option<TsNode<'a>> {
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).find(|c| c.kind() == kind);
    found
}

/// Dispatches whether `kind` is a heritage site, a definition that emits
/// `extends`/`implements` edges, by grammar (the dispatch
/// and the per-language type-kind sets it reads from). TS and Python widen
/// no kind beyond `class`. Java widens to class, interface, enum, and
/// struct. PHP widens to class only, plus the `use
/// SomeTrait;` implements rule handled inside `emit_heritage_edges`.
fn heritage_gate(lang: &str, kind: Kind) -> bool {
    match lang {
        "java" => matches!(
            kind,
            Kind::Class | Kind::Interface | Kind::Enum | Kind::Struct
        ),
        "php" => kind == Kind::Class,
        "swift" => SWIFT_TYPE_KINDS.contains(&kind),
        "kotlin" => KOTLIN_TYPE_KINDS.contains(&kind),
        _ => kind == Kind::Class,
    }
}

/// Returns the byte offset where a node's header text ends: the start of
/// its `body` field, or the node's own end. A `variable_declarator` looks
/// at its `value` child's `body` field instead.
fn header_end(node: TsNode, is_declarator: bool) -> usize {
    let target = if is_declarator {
        node.child_by_field_name("value")
    } else {
        Some(node)
    };
    target
        .and_then(|n| n.child_by_field_name("body"))
        .map(|b| b.start_byte())
        .unwrap_or_else(|| node.end_byte())
}

/// Reports whether any ancestor of `node` is an `export_statement`.
fn exported_ts(node: TsNode) -> bool {
    let mut current = node.parent();
    while let Some(n) = current {
        if n.kind() == "export_statement" {
            return true;
        }
        current = n.parent();
    }
    false
}

/// Reports whether `node` is a named top-level `export`: its parent, or
/// its declaration's parent for `export const f = ...`, is an
/// `export_statement` with no `default` keyword.
fn is_direct_named_export(node: TsNode) -> bool {
    let Some(mut parent) = node.parent() else {
        return false;
    };
    if matches!(
        parent.kind(),
        "lexical_declaration" | "variable_declaration"
    ) {
        match parent.parent() {
            Some(grand) => parent = grand,
            None => return false,
        }
    }
    if parent.kind() != "export_statement" {
        return false;
    }
    let mut cursor = parent.walk();
    let has_default = parent.children(&mut cursor).any(|c| c.kind() == "default");
    !has_default
}

/// Mints a collision-free id: `base`, or `base~2`, `base~3`, and so on.
pub(crate) fn mint_id(base: String, minted: &mut HashSet<String>) -> String {
    if !minted.contains(&base) {
        minted.insert(base.clone());
        return base;
    }
    let mut k = 2;
    loop {
        let candidate = format!("{base}~{k}");
        if !minted.contains(&candidate) {
            minted.insert(candidate.clone());
            return candidate;
        }
        k += 1;
    }
}

/// Collapses every whitespace run to one space, then trims both ends,
/// then strips one trailing `=>`, `{`, `:` or `=`. Returns `None` when the
/// result is empty.
fn clean(text: &str) -> Option<String> {
    let collapsed = collapse_whitespace(text);
    let stripped = collapsed.strip_suffix("=>").unwrap_or_else(|| {
        collapsed
            .strip_suffix('{')
            .or_else(|| collapsed.strip_suffix(':'))
            .or_else(|| collapsed.strip_suffix('='))
            .unwrap_or(&collapsed)
    });
    let result = stripped.trim_end().to_string();
    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

/// Collapses every whitespace run to one space, and trims both ends.
/// Collapses with JavaScript `\s`, which also matches U+FEFF (zero
/// width no-break space). Rust's `char::is_whitespace` does not, so this
/// treats U+FEFF as whitespace too.
fn collapse_whitespace(text: &str) -> String {
    let mut out = String::new();
    let mut last_space = false;
    for c in text.chars() {
        if c.is_whitespace() || c == '\u{FEFF}' {
            if !last_space && !out.is_empty() {
                out.push(' ');
                last_space = true;
            }
        } else {
            out.push(c);
            last_space = false;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

/// Slices `max` chars from `text` when it holds more than `max` chars.
fn cap_chars(text: String, max: usize) -> String {
    if text.chars().count() > max {
        text.chars().take(max).collect()
    } else {
        text
    }
}

/// Returns the lowercase hex sha256 digest of `bytes`.
fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Returns the UTF-8 slice of `source` between two byte offsets, or an
/// empty string when the offsets fall outside a char boundary.
fn source_slice(source: &str, start: usize, end: usize) -> String {
    source.get(start..end).unwrap_or("").to_string()
}

/// Parses a `span` string of the form `L<start>-L<end>` into its two
/// 1-indexed line numbers.
fn parse_span(text: &str) -> Option<(u32, u32)> {
    let rest = text.strip_prefix('L')?;
    let (start_str, end_str) = rest.split_once("-L")?;
    let start = start_str.parse().ok()?;
    let end = end_str.parse().ok()?;
    Some((start, end))
}

/// Builds a file node's `body_text`: every source line not covered by a
/// symbol's span, joined by a space and collapsed.
pub fn file_residual(source: &str, symbols: &[Node]) -> String {
    let lines: Vec<&str> = source.split('\n').collect();
    let mut covered = vec![false; lines.len() + 2];
    for symbol in symbols {
        if let Some((start, end)) = parse_span(&symbol.span) {
            let mut r = start as usize;
            while r <= end as usize && r < covered.len() {
                covered[r] = true;
                r += 1;
            }
        }
    }
    let kept: Vec<&str> = lines
        .iter()
        .enumerate()
        .filter(|(i, _)| !covered[i + 1])
        .map(|(_, line)| *line)
        .collect();
    cap_chars(collapse_whitespace(&kept.join(" ")), MAX_FILE_BODY_CHARS)
}

/// Common base-R S3 generics worth assuming even without local evidence:
/// `print.Foo`/`format.Foo` and the like are the single most common
/// real-world S3 pattern, and a local `UseMethod()` call never exists for
/// them (they ship in base/methods/stats, not the user's own repo).
const R_BASE_GENERICS: [&str; 29] = [
    "print",
    "format",
    "summary",
    "plot",
    "str",
    "toString",
    "as.character",
    "as.list",
    "as.data.frame",
    "as.vector",
    "as.numeric",
    "as.matrix",
    "length",
    "dim",
    "names",
    "rev",
    "sort",
    "unique",
    "predict",
    "coef",
    "residuals",
    "fitted",
    "update",
    "merge",
    "all.equal",
    "anova",
    "confint",
    "vcov",
    "logLik",
];

/// `describe` for R: pattern-matched, not node-type-mapped (`R_KINDS` is
/// empty). Covers a plain or right-assigned function (promoted to
/// an S3 method when its name splits against a known generic), an
/// `R6Class(...)`/mixin `list(...)` class assignment, a top-level
/// `setClass`/`setMethod` call, and an R6 method reached through the
/// `r_r6_access` section interception in `walk` (edges note, section 1.6,
/// `describeR`).
fn describe_r<'a>(node: TsNode<'a>, ctx: &Ctx, source: &str) -> Option<Desc<'a>> {
    match node.kind() {
        "binary_operator" => {
            let op = node.child_by_field_name("operator")?;
            let op_text = op.utf8_text(source.as_bytes()).ok()?;
            if !matches!(op_text, "<-" | "<<-" | "=") {
                return None;
            }
            let lhs = node.child_by_field_name("lhs")?;
            if lhs.kind() != "identifier" {
                return None;
            }
            let lhs_name = lhs.utf8_text(source.as_bytes()).ok()?.to_string();
            let rhs = node.child_by_field_name("rhs")?;
            if rhs.kind() == "function_definition" {
                let body = rhs.child_by_field_name("body");
                return Some(r_function_descriptor(lhs_name, rhs, body, ctx));
            }
            if rhs.kind() == "call" {
                let callee = r_callee_name(rhs, source);
                let is_class = callee.as_deref() == Some("R6Class")
                    || (callee.as_deref() == Some("list") && r_is_mixin_container(rhs, source));
                if is_class {
                    return Some(Desc {
                        name: lhs_name,
                        id_name: None,
                        kind: Kind::Class,
                        header_end: rhs.end_byte(),
                        hash_node: rhs,
                        owner: None,
                        arity: None,
                        variadic: None,
                    });
                }
            }
            None
        }
        "function_definition" => {
            // A right-assigned function (`function() {} -> foo`): the
            // body's own trailing statement carries the name, so this
            // cannot mirror the `binary_operator` branch above.
            let body = node.child_by_field_name("body")?;
            if body.kind() != "binary_operator" {
                return None;
            }
            let op = body.child_by_field_name("operator")?;
            let op_text = op.utf8_text(source.as_bytes()).ok()?;
            if !matches!(op_text, "->" | "->>") {
                return None;
            }
            let rhs = body.child_by_field_name("rhs")?;
            if rhs.kind() != "identifier" {
                return None;
            }
            let name = rhs.utf8_text(source.as_bytes()).ok()?.to_string();
            let lhs = body.child_by_field_name("lhs");
            Some(r_function_descriptor(name, node, lhs, ctx))
        }
        "call" => describe_r_top_level_call(node, source),
        "argument" if ctx.r_r6_access.is_some() => {
            let arg_name = node.child_by_field_name("name")?;
            if arg_name.kind() != "identifier" {
                return None;
            }
            let name = arg_name.utf8_text(source.as_bytes()).ok()?.to_string();
            let value = node.child_by_field_name("value")?;
            if value.kind() != "function_definition" {
                return None;
            }
            let body = value.child_by_field_name("body");
            let header_end = body
                .map(|b| b.start_byte())
                .unwrap_or_else(|| value.end_byte());
            Some(Desc {
                name,
                id_name: None,
                kind: Kind::Method,
                header_end,
                hash_node: value,
                // Deliberately unset: an R6 method lexically nests inside
                // the class-defining call, so `ctx.enclosing_class`
                // already carries it.
                owner: None,
                arity: None,
                variadic: None,
            })
        }
        _ => None,
    }
}

/// A plain function assignment (left- or right-assign), or — if `name`
/// matches a known S3 generic's `generic.Class` pattern — an S3 method
/// instead (`rFunctionDescriptor`).
fn r_function_descriptor<'a>(
    name: String,
    hash_node: TsNode<'a>,
    body: Option<TsNode<'a>>,
    ctx: &Ctx,
) -> Desc<'a> {
    let header_end = body
        .map(|b| b.start_byte())
        .unwrap_or_else(|| hash_node.end_byte());
    if let Some(generics) = ctx.r_generics.as_ref() {
        if let Some((generic, class_name)) = r_s3_split(&name, generics) {
            return Desc {
                name: generic.clone(),
                id_name: Some(format!("{class_name}.{generic}")),
                kind: Kind::Method,
                header_end,
                hash_node,
                owner: Some(class_name),
                arity: None,
                variadic: None,
            };
        }
    }
    Desc {
        name,
        id_name: None,
        kind: Kind::Function,
        header_end,
        hash_node,
        owner: None,
        arity: None,
        variadic: None,
    }
}

/// S4's top-level registration calls: `setClass("Foo", ...)` and
/// `setMethod("gen", "Cls", function() {...})`. Both are ordinary
/// top-level `call` nodes, essentially never assigned to a variable, so
/// they need their own describe branch (`describeRTopLevelCall`).
fn describe_r_top_level_call<'a>(node: TsNode<'a>, source: &str) -> Option<Desc<'a>> {
    let callee = r_callee_name(node, source)?;
    if callee == "setClass" {
        let args = r_call_args(node);
        let name = r_string_content(
            args.first().and_then(|a| a.child_by_field_name("value")),
            source,
        )?;
        return Some(Desc {
            name,
            id_name: None,
            kind: Kind::Class,
            header_end: node.end_byte(),
            hash_node: node,
            owner: None,
            arity: None,
            variadic: None,
        });
    }
    if callee == "setMethod" {
        let args = r_call_args(node);
        let generic = r_string_content(
            args.first().and_then(|a| a.child_by_field_name("value")),
            source,
        )?;
        let class_name = r_string_content(
            args.get(1).and_then(|a| a.child_by_field_name("value")),
            source,
        )?;
        let def_arg = args
            .iter()
            .find(|a| {
                a.child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                    == Some("definition")
            })
            .or_else(|| args.get(2))?;
        let fn_def = def_arg.child_by_field_name("value")?;
        if fn_def.kind() != "function_definition" {
            return None;
        }
        let body = fn_def.child_by_field_name("body");
        let header_end = body
            .map(|b| b.start_byte())
            .unwrap_or_else(|| fn_def.end_byte());
        return Some(Desc {
            name: generic.clone(),
            id_name: Some(format!("{class_name}.{generic}")),
            kind: Kind::Method,
            header_end,
            hash_node: fn_def,
            owner: Some(class_name),
            arity: None,
            variadic: None,
        });
    }
    None
}

/// S3 dispatch detection: does `name` split as `generic.Class` for some
/// known generic? Tries the longest possible generic prefix first, and
/// only matches a generic registered locally via `UseMethod()` (`generics`)
/// or in the curated `R_BASE_GENERICS` set (`rS3Split`).
fn r_s3_split(name: &str, generics: &HashSet<String>) -> Option<(String, String)> {
    let parts: Vec<&str> = name.split('.').collect();
    if parts.len() < 2 {
        return None;
    }
    for i in (1..parts.len()).rev() {
        let generic = parts[..i].join(".");
        if generics.contains(&generic) || R_BASE_GENERICS.contains(&generic.as_str()) {
            let class_name = parts[i..].join(".");
            return Some((generic, class_name));
        }
    }
    None
}

/// Every S3 generic this file registers via a local `UseMethod()` call,
/// so `r_s3_split` can recognize `generic.Class` methods for a repo's own
/// generics, not just the base-R ones. Runs once per file, ahead of the
/// main walk (`collectRGenerics`).
fn collect_r_generics(root: TsNode, source: &str) -> HashSet<String> {
    let mut generics = HashSet::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let mut fn_def_and_name: Option<(TsNode, String)> = None;
        if node.kind() == "binary_operator" {
            if let (Some(op), Some(lhs), Some(rhs)) = (
                node.child_by_field_name("operator")
                    .and_then(|o| o.utf8_text(source.as_bytes()).ok()),
                node.child_by_field_name("lhs"),
                node.child_by_field_name("rhs"),
            ) {
                if matches!(op, "<-" | "<<-" | "=")
                    && lhs.kind() == "identifier"
                    && rhs.kind() == "function_definition"
                {
                    if let Ok(name) = lhs.utf8_text(source.as_bytes()) {
                        fn_def_and_name = Some((rhs, name.to_string()));
                    }
                }
            }
        } else if node.kind() == "function_definition" {
            if let Some(body) = node.child_by_field_name("body") {
                if body.kind() == "binary_operator" {
                    if let (Some(op), Some(rhs)) = (
                        body.child_by_field_name("operator")
                            .and_then(|o| o.utf8_text(source.as_bytes()).ok()),
                        body.child_by_field_name("rhs"),
                    ) {
                        if matches!(op, "->" | "->>") && rhs.kind() == "identifier" {
                            if let Ok(name) = rhs.utf8_text(source.as_bytes()) {
                                fn_def_and_name = Some((node, name.to_string()));
                            }
                        }
                    }
                }
            }
        }
        if let Some((fn_def, own_name)) = fn_def_and_name {
            if let Some(body) = fn_def.child_by_field_name("body") {
                if let Some(arg) = find_use_method_arg(body, source) {
                    generics.insert(if arg.is_empty() { own_name } else { arg });
                }
            }
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            stack.push(child);
        }
    }
    generics
}

/// Searches a function body for a `UseMethod(...)` call and returns its
/// string-literal generic-name argument, `""` if called with no arguments
/// (defaults to the enclosing function's own name), or `None` if no
/// `UseMethod` call exists at all (`findUseMethodArg`).
fn find_use_method_arg(node: TsNode, source: &str) -> Option<String> {
    if node.kind() == "call" && r_callee_name(node, source).as_deref() == Some("UseMethod") {
        let first = r_call_args(node)
            .into_iter()
            .next()
            .and_then(|a| a.child_by_field_name("value"));
        return Some(r_string_content(first, source).unwrap_or_default());
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if let Some(found) = find_use_method_arg(child, source) {
            return Some(found);
        }
    }
    None
}

/// A call's callee name, whether bare (`R6Class(...)`) or
/// namespace-qualified (`R6::R6Class(...)`). `None` if the callee isn't a
/// simple name (`rCalleeName`).
fn r_callee_name(node: TsNode, source: &str) -> Option<String> {
    let func = node.child_by_field_name("function")?;
    match func.kind() {
        "identifier" => func.utf8_text(source.as_bytes()).ok().map(str::to_string),
        "namespace_operator" => {
            let rhs = func.child_by_field_name("rhs")?;
            if rhs.kind() != "identifier" {
                return None;
            }
            rhs.utf8_text(source.as_bytes()).ok().map(str::to_string)
        }
        _ => None,
    }
}

/// A call's positional/named `argument` children (`rCallArgs`).
fn r_call_args(node: TsNode) -> Vec<TsNode> {
    let Some(args) = node.child_by_field_name("arguments") else {
        return Vec::new();
    };
    let mut cursor = args.walk();
    args.named_children(&mut cursor)
        .filter(|c| c.kind() == "argument")
        .collect()
}

/// An R `string` node's unquoted text, or `None` if `node` isn't a string
/// (`rStringContent`).
fn r_string_content(node: Option<TsNode>, source: &str) -> Option<String> {
    let node = node?;
    if node.kind() != "string" {
        return None;
    }
    let mut cursor = node.walk();
    let content = node
        .named_children(&mut cursor)
        .find(|c| c.kind() == "string_content")?;
    content
        .utf8_text(source.as_bytes())
        .ok()
        .map(str::to_string)
}

/// The value of a call's named argument (`rNamedArg`).
fn r_named_arg<'a>(node: TsNode<'a>, arg_name: &str, source: &str) -> Option<TsNode<'a>> {
    r_call_args(node)
        .into_iter()
        .find(|a| {
            a.child_by_field_name("name")
                .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                == Some(arg_name)
        })
        .and_then(|a| a.child_by_field_name("value"))
}

/// Does this `list(...)` call look like a mixin/extension bundle: a
/// `public =` or `private =` entry whose own value is itself a `list(...)`
/// call (`rIsMixinContainer`)?
fn r_is_mixin_container(node: TsNode, source: &str) -> bool {
    r_call_args(node).iter().any(|a| {
        let name = a
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(source.as_bytes()).ok());
        if name != Some("public") && name != Some("private") {
            return false;
        }
        let Some(value) = a.child_by_field_name("value") else {
            return false;
        };
        value.kind() == "call" && r_callee_name(value, source).as_deref() == Some("list")
    })
}

/// A base-class name list from either a bare string (`contains = "Base"`)
/// or a `c(...)` call of strings (`rStringOrCVector`).
fn r_string_or_c_vector(value: Option<TsNode>, source: &str) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    if let Some(single) = r_string_content(Some(value), source) {
        return vec![single];
    }
    if value.kind() == "call" && r_callee_name(value, source).as_deref() == Some("c") {
        return r_call_args(value)
            .iter()
            .filter_map(|a| r_string_content(a.child_by_field_name("value"), source))
            .collect();
    }
    Vec::new()
}

/// An R6 class-defining node's `inherit =` parent class name (a bare
/// identifier, not a string), for `super$` call resolution. `node` is
/// whatever `describe_r` matched: a `binary_operator` for R6, or the call
/// itself for any other class kind, which always gives `None` since its
/// callee is never `R6Class` (`rR6ParentClass`).
fn r_r6_parent_class(node: TsNode, source: &str) -> Option<String> {
    let call = if node.kind() == "binary_operator" {
        node.child_by_field_name("rhs")?
    } else {
        node
    };
    if call.kind() != "call" || r_callee_name(call, source).as_deref() != Some("R6Class") {
        return None;
    }
    let value = r_named_arg(call, "inherit", source)?;
    if value.kind() != "identifier" {
        return None;
    }
    value.utf8_text(source.as_bytes()).ok().map(str::to_string)
}

/// Reports whether `node` is the class-defining `R6Class(...)` or mixin
/// `list(...)` call, already consumed by `describe_r` as its enclosing
/// `binary_operator`'s definition. The walk still reaches this same call
/// node again while descending generically to find its
/// `public =`/`private =`/`active =` arguments, and must not also read it
/// as an ordinary call to a function literally named `R6Class`/`list`
/// (edges note, section 1.6).
fn r_is_consumed_class_call(node: TsNode, source: &str) -> bool {
    if node.kind() != "call" {
        return false;
    }
    match r_callee_name(node, source) {
        Some(name) if name == "R6Class" => true,
        Some(name) if name == "list" => r_is_mixin_container(node, source),
        _ => false,
    }
}

/// Reports whether `node` (a `public =`/`private =`/`active =` argument
/// inside an R6-class-defining call's own arguments) opens an R6 access
/// section, and returns which one. Scoped to `ctx.enclosing_kind ==
/// Class` and `ctx.r_r6_access.is_none()`, so an unrelated nested
/// `list(public = list(...))` elsewhere is never misread as another class
/// body.
fn r_r6_access_section_name(node: TsNode, ctx: &Ctx, source: &str) -> Option<String> {
    if ctx.lang != "r" || ctx.enclosing_kind != Some(Kind::Class) || ctx.r_r6_access.is_some() {
        return None;
    }
    if node.kind() != "argument" {
        return None;
    }
    let arg_name = node.child_by_field_name("name")?;
    if arg_name.kind() != "identifier" {
        return None;
    }
    let text = arg_name.utf8_text(source.as_bytes()).ok()?;
    if !matches!(text, "public" | "private" | "active") {
        return None;
    }
    let value = node.child_by_field_name("value")?;
    if value.kind() != "call" || r_callee_name(value, source).as_deref() != Some("list") {
        return None;
    }
    Some(text.to_string())
}

/// R6 visibility follows the `public =`/`private =`/`active =` section a
/// method was declared in; a plain function or an S3/S4 method checks for
/// a roxygen `@export` tag next, falling back to the leading-dot naming
/// convention only when there's no roxygen evidence at all (`rExported`).
fn r_exported(name: &str, ctx: &Ctx, node: TsNode, source: &str) -> bool {
    if let Some(access) = ctx.r_r6_access.as_deref() {
        return access != "private";
    }
    if let Some(roxygen) = r_roxygen_exported(node, source) {
        return roxygen;
    }
    !name.starts_with('.')
}

/// Roxygen `@export` detection: does `node` have a contiguous run of `#'`
/// comment siblings immediately before it, and if so, is one tagged
/// `@export`? `None` means no roxygen block at all, so the caller falls
/// back to the leading-dot convention (`rRoxygenExported`).
fn r_roxygen_exported(node: TsNode, source: &str) -> Option<bool> {
    let mut sib = node.prev_named_sibling();
    let mut saw_roxygen = false;
    let mut exported = false;
    while let Some(s) = sib {
        if s.kind() != "comment" {
            break;
        }
        let Ok(text) = s.utf8_text(source.as_bytes()) else {
            break;
        };
        let trimmed = text.trim();
        if !trimmed.starts_with("#'") {
            break;
        }
        saw_roxygen = true;
        if trimmed
            .trim_start_matches("#'")
            .trim_start()
            .starts_with("@export")
        {
            exported = true;
        }
        sib = s.prev_named_sibling();
    }
    saw_roxygen.then_some(exported)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_kotlin_promotes_a_class_body_function_to_method_with_an_owner() {
        let mut extractor = Extractor::new().expect("build extractor");
        let source = "class App {\n    fun greet(): String {\n        return \"hi\"\n    }\n}\n";
        let (nodes, _edges) = extractor.extract_file("a.kt", source, "kotlin");
        assert_eq!(nodes.len(), 2);
        let greet = nodes.iter().find(|n| n.name == "greet").expect("greet");
        assert_eq!(greet.kind, Kind::Method);
        assert_eq!(greet.owner.as_deref(), Some("App"));
        assert_eq!(greet.signature.as_deref(), Some("fun greet(): String"));
    }

    #[test]
    fn describe_kotlin_reads_a_type_alias_name_off_the_type_identifier() {
        // The fwcd grammar has no `name` field: the first direct
        // `type_identifier` of a `type_alias` is its name.
        let mut extractor = Extractor::new().expect("build extractor");
        let source = "typealias GreeterFn = () -> String\n";
        let (nodes, _edges) = extractor.extract_file("a.kt", source, "kotlin");
        let alias = nodes
            .iter()
            .find(|n| n.kind == Kind::Type)
            .expect("type_alias node");
        assert_eq!(alias.name, "GreeterFn");
    }

    #[test]
    fn describe_kotlin_owns_a_companion_object_method_by_the_enclosing_class() {
        // A companion object mints no node of its own: its members land at
        // the enclosing class's own scope and owner.
        let mut extractor = Extractor::new().expect("build extractor");
        let source =
            "class App {\n    companion object {\n        fun create(): App = App()\n    }\n}\n";
        let (nodes, _edges) = extractor.extract_file("a.kt", source, "kotlin");
        assert_eq!(nodes.len(), 2);
        let create = nodes.iter().find(|n| n.name == "create").expect("create");
        assert_eq!(create.kind, Kind::Method);
        assert_eq!(create.owner.as_deref(), Some("App"));
        assert_eq!(create.id, "a.kt#App.create");
    }

    #[test]
    fn test_p2_14_kotlin_enum_header_ends_at_the_enum_class_body() {
        // DV17: a one-line enum, and an enum with entries, end the
        // signature at `enum_class_body`.
        let mut extractor = Extractor::new().expect("build extractor");
        let source =
            "enum class Color { RED, GREEN }\nenum class Op(val n: Int) {\n    ADD(1);\n}\n";
        let (nodes, _edges) = extractor.extract_file("a.kt", source, "kotlin");
        let sig = |name: &str| {
            nodes
                .iter()
                .find(|n| n.name == name)
                .and_then(|n| n.signature.clone())
        };
        assert_eq!(sig("Color").as_deref(), Some("enum class Color"));
        assert_eq!(sig("Op").as_deref(), Some("enum class Op(val n: Int)"));
    }

    #[test]
    fn test_p2_15_kotlin_call_receiver_must_be_a_name_this_or_super() {
        // A chained receiver gives no call edge.
        let mut extractor = Extractor::new().expect("build extractor");
        let source = "class A {\n    fun a() { this.b() }\n    fun b() { c().b() }\n    fun c(): A = this\n}\n";
        let (_nodes, edges) = extractor.extract_file("a.kt", source, "kotlin");
        let from = |src: &str| edges.iter().filter(|e| e.source == src).count();
        assert_eq!(from("a.kt#A.a"), 1);
        assert_eq!(from("a.kt#A.b"), 1, "only `c()` resolves, not `c().b()`");
    }

    #[test]
    fn clean_strips_only_one_trailing_arrow_brace_colon_or_equals() {
        assert_eq!(
            clean("run(n: number): number =>"),
            Some("run(n: number): number".to_string())
        );
        assert_eq!(clean("class App {"), Some("class App".to_string()));
        assert_eq!(clean("def add(a, b):"), Some("def add(a, b)".to_string()));
        assert_eq!(clean("const x ="), Some("const x".to_string()));
        assert_eq!(clean("  {  "), None);
    }

    #[test]
    fn describe_promotes_a_nested_python_def_to_function_with_no_owner() {
        let mut extractor = Extractor::new().expect("build extractor");
        let (nodes, _edges) = extractor.extract_file(
            "a.py",
            "def outer():\n    def inner():\n        pass\n    return inner\n",
            "python",
        );
        assert_eq!(nodes.len(), 2);
        let inner = nodes
            .iter()
            .find(|n| n.name == "inner")
            .expect("inner node");
        assert_eq!(inner.kind, Kind::Function);
        assert!(inner.owner.is_none());
    }

    #[test]
    fn second_node_with_the_same_name_gets_a_tilde_2_suffix() {
        let source = "class Box {\n  get value() {\n    return 1;\n  }\n  set value(v) {\n  }\n}\n";
        let mut extractor = Extractor::new().expect("build extractor");
        let (nodes, _edges) = extractor.extract_file("a.ts", source, "typescript");
        let ids: Vec<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
        assert!(ids.contains(&"a.ts#Box.value"));
        assert!(ids.contains(&"a.ts#Box.value~2"));
    }

    #[test]
    fn file_residual_keeps_only_uncovered_lines_joined_by_a_space() {
        let source = "one\ntwo\nthree\n";
        let symbols = vec![sample_node("a#two", "L2-L2")];
        assert_eq!(file_residual(source, &symbols), "one three");
    }

    #[test]
    fn symbol_body_text_caps_at_5000_chars_by_char_count() {
        let padding = "a".repeat(6000);
        let source = format!("def f():\n    x = \"{padding}\"\n");
        let mut extractor = Extractor::new().expect("build extractor");
        let (nodes, _edges) = extractor.extract_file("a.py", &source, "python");
        let body_text = nodes[0].body_text.as_ref().expect("body_text is set");
        assert_eq!(body_text.chars().count(), MAX_BODY_CHARS);
    }

    #[test]
    fn collapse_whitespace_treats_u_feff_as_a_space() {
        // Collapse with JavaScript `\s`, which matches U+FEFF as
        // well as every Unicode `White_Space` code point.
        assert_eq!(collapse_whitespace("a\u{FEFF}b"), "a b");
        assert_eq!(collapse_whitespace("a \u{FEFF} b"), "a b");
    }

    fn sample_node(id: &str, span: &str) -> Node {
        Node {
            id: id.to_string(),
            name: "n".to_string(),
            kind: Kind::Function,
            path: "a".to_string(),
            span: span.to_string(),
            signature: None,
            exported: true,
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

    #[test]
    fn a_module_level_call_has_the_file_id_as_source() {
        let mut extractor = Extractor::new().expect("build extractor");
        let (_nodes, edges) = extractor.extract_file("a.ts", "run();\n", "typescript");
        let call = edges
            .iter()
            .find(|e| e.relation == Relation::Calls)
            .expect("a calls edge");
        assert_eq!(call.source, "a.ts");
        assert_eq!(call.name.as_deref(), Some("run"));
    }

    #[test]
    fn a_call_in_an_argument_gives_two_raw_edges() {
        let mut extractor = Extractor::new().expect("build extractor");
        let (_nodes, edges) = extractor.extract_file("a.ts", "outer(inner());\n", "typescript");
        let calls: Vec<&RawEdge> = edges
            .iter()
            .filter(|e| e.relation == Relation::Calls)
            .collect();
        assert_eq!(calls.len(), 2);
        let names: Vec<&str> = calls.iter().filter_map(|e| e.name.as_deref()).collect();
        assert!(names.contains(&"outer"));
        assert!(names.contains(&"inner"));
    }

    #[test]
    fn a_method_node_gets_its_enclosing_class_as_owner() {
        let source = "class Box {\n  run(): void {\n  }\n}\n";
        let mut extractor = Extractor::new().expect("build extractor");
        let (nodes, _edges) = extractor.extract_file("a.ts", source, "typescript");
        let run = nodes.iter().find(|n| n.name == "run").expect("run node");
        assert_eq!(run.owner.as_deref(), Some("Box"));
    }

    #[test]
    fn describe_first_walk_order_gives_no_self_calls_edge_for_a_definition() {
        // `walk` now runs `describe` before the `calls` check. A top-level
        // function definition must still emit no `calls` edge naming itself.
        let mut extractor = Extractor::new().expect("build extractor");
        let (_nodes, edges) = extractor.extract_file("a.ts", "function run() {}\n", "typescript");
        assert!(!edges
            .iter()
            .any(|e| e.relation == Relation::Calls && e.name.as_deref() == Some("run")));
    }

    #[test]
    fn super_dot_call_has_via_member_and_no_recv_type() {
        let mut extractor = Extractor::new().expect("build extractor");
        let source = "class C extends Base {\n  run(): void {\n    super.run();\n  }\n}\n";
        let (_nodes, edges) = extractor.extract_file("a.ts", source, "typescript");
        let call = edges
            .iter()
            .find(|e| e.relation == Relation::Calls && e.name.as_deref() == Some("run"))
            .expect("a calls edge for run");
        assert!(call.via_member);
        assert_eq!(call.recv_type, None);
    }

    #[test]
    fn import_a_dot_b_as_c_gives_no_import_edge() {
        let mut extractor = Extractor::new().expect("build extractor");
        let (_nodes, edges) = extractor.extract_file("a.py", "import a.b as c\n", "python");
        assert!(!edges.iter().any(|e| e.relation == Relation::Imports));
    }

    #[test]
    fn from_dot_import_x_gives_specifier_dot() {
        let mut extractor = Extractor::new().expect("build extractor");
        let (_nodes, edges) = extractor.extract_file("a.py", "from . import x\n", "python");
        let import = edges
            .iter()
            .find(|e| e.relation == Relation::Imports)
            .expect("an imports edge");
        assert_eq!(import.specifier.as_deref(), Some("."));
    }

    #[test]
    fn a_python_file_gives_no_references_edge() {
        let mut extractor = Extractor::new().expect("build extractor");
        let source = "from a import b\n\n\ndef f():\n    return b()\n";
        let (_nodes, edges) = extractor.extract_file("a.py", source, "python");
        assert!(!edges.iter().any(|e| e.relation == Relation::References));
    }

    #[test]
    fn a_local_declaration_that_shadows_an_import_gives_no_references_edge() {
        let mut extractor = Extractor::new().expect("build extractor");
        let source = "import { helper } from \"./h\";\n\nexport function run(): number {\n  const helper = 1;\n  return helper + 1;\n}\n";
        let (_nodes, edges) = extractor.extract_file("a.ts", source, "typescript");
        assert!(!edges.iter().any(|e| e.relation == Relation::References));
    }

    #[test]
    fn an_unshadowed_import_gives_one_references_edge() {
        let mut extractor = Extractor::new().expect("build extractor");
        let source = "import { helper } from \"./h\";\n\nexport function run(): number {\n  return helper + 1;\n}\n";
        let (_nodes, edges) = extractor.extract_file("a.ts", source, "typescript");
        let refs: Vec<&RawEdge> = edges
            .iter()
            .filter(|e| e.relation == Relation::References)
            .collect();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name.as_deref(), Some("helper"));
    }

    #[test]
    fn a_java_record_never_sets_arity() {
        let mut extractor = Extractor::new().expect("build extractor");
        let source = "record Point(int x, int y) {\n}\n";
        let (nodes, _edges) = extractor.extract_file("a.java", source, "java");
        let point = nodes
            .iter()
            .find(|n| n.name == "Point")
            .expect("Point node");
        assert_eq!(point.kind, Kind::Struct);
        assert_eq!(point.arity, None);
    }

    #[test]
    fn a_qualified_java_new_expression_gives_no_calls_edge() {
        let mut extractor = Extractor::new().expect("build extractor");
        let source = "class App {\n  void run() {\n    new a.b.C();\n  }\n}\n";
        let (_nodes, edges) = extractor.extract_file("a.java", source, "java");
        assert!(!edges.iter().any(|e| e.relation == Relation::Calls));
    }

    #[test]
    fn a_php_dollar_call_gives_no_callee() {
        let mut extractor = Extractor::new().expect("build extractor");
        let source = "<?php\nfunction run() {\n  $fn();\n}\n";
        let (_nodes, edges) = extractor.extract_file("a.php", source, "php");
        assert!(!edges.iter().any(|e| e.relation == Relation::Calls));
    }

    #[test]
    fn a_swift_import_gives_one_imports_edge_with_the_bare_module_name() {
        let mut extractor = Extractor::new().expect("build extractor");
        let source = "import Foundation\n\nfunc run() {}\n";
        let (_nodes, edges) = extractor.extract_file("a.swift", source, "swift");
        let specifiers: Vec<&str> = edges
            .iter()
            .filter(|e| e.relation == Relation::Imports)
            .filter_map(|e| e.specifier.as_deref())
            .collect();
        assert_eq!(specifiers, vec!["Foundation"]);
    }

    #[test]
    fn a_go_grouped_import_gives_one_edge_per_line() {
        let mut extractor = Extractor::new().expect("build extractor");
        let source = "package main\n\nimport (\n\t\"fmt\"\n\t\"os\"\n)\n\nfunc main() {}\n";
        let (_nodes, edges) = extractor.extract_file("a.go", source, "go");
        let specifiers: Vec<&str> = edges
            .iter()
            .filter(|e| e.relation == Relation::Imports)
            .filter_map(|e| e.specifier.as_deref())
            .collect();
        assert_eq!(specifiers, vec!["fmt", "os"]);
    }

    #[test]
    fn a_go_method_s_receiver_variable_resolves_to_the_enclosing_class() {
        let mut extractor = Extractor::new().expect("build extractor");
        let source = "package main\n\ntype T struct{}\n\nfunc (t T) A() {\n\tt.B()\n}\n\nfunc (t T) B() {}\n";
        let (_nodes, edges) = extractor.extract_file("a.go", source, "go");
        let call = edges
            .iter()
            .find(|e| e.relation == Relation::Calls && e.name.as_deref() == Some("B"))
            .expect("a calls edge for B");
        assert_eq!(call.recv_type.as_deref(), Some("T"));
    }

    #[test]
    fn a_swift_extension_takes_kind_module() {
        let mut extractor = Extractor::new().expect("build extractor");
        let source =
            "struct Point {\n}\n\nextension Point {\n  func desc() -> String {\n    return \"\"\n  }\n}\n";
        let (nodes, _edges) = extractor.extract_file("a.swift", source, "swift");
        let ext = nodes
            .iter()
            .find(|n| n.name == "Point" && n.kind == Kind::Module)
            .expect("an extension node with kind module");
        let method = nodes
            .iter()
            .find(|n| n.name == "desc")
            .expect("desc method node");
        assert_eq!(method.owner.as_deref(), Some("Point"));
        assert_eq!(ext.kind, Kind::Module);
    }

    #[test]
    fn swift_arity_counts_parameters_minus_defaults() {
        let mut extractor = Extractor::new().expect("build extractor");
        let source = "func f(a: Int, b: Int = 1) {\n}\n";
        let (nodes, _edges) = extractor.extract_file("a.swift", source, "swift");
        let f = nodes.iter().find(|n| n.name == "f").expect("f node");
        assert_eq!(f.arity, Some(1));
        assert_eq!(f.variadic, Some(true));
    }

    #[test]
    fn an_implicit_self_call_inside_a_struct_sets_recv_type_and_the_flag() {
        let mut extractor = Extractor::new().expect("build extractor");
        let source = "struct Box {\n  func helper() {\n  }\n  func run() {\n    helper()\n  }\n}\n";
        let (_nodes, edges) = extractor.extract_file("a.swift", source, "swift");
        let call = edges
            .iter()
            .find(|e| e.relation == Relation::Calls && e.name.as_deref() == Some("helper"))
            .expect("a calls edge for helper");
        assert!(call.via_member);
        assert!(call.implicit_self);
        assert_eq!(call.recv_type.as_deref(), Some("Box"));
    }

    #[test]
    fn an_r_right_assign_function_is_described_as_a_plain_function() {
        let mut extractor = Extractor::new().expect("build extractor");
        let source = "function(x) {\n  x\n} -> square\n";
        let (nodes, _edges) = extractor.extract_file("a.R", source, "r");
        let square = nodes
            .iter()
            .find(|n| n.name == "square")
            .expect("square node");
        assert_eq!(square.kind, Kind::Function);
    }

    #[test]
    fn an_s3_split_fires_on_a_base_generic_with_no_local_use_method() {
        let mut extractor = Extractor::new().expect("build extractor");
        let source = "print.Foo <- function(x) {\n  x\n}\n";
        let (nodes, _edges) = extractor.extract_file("a.R", source, "r");
        let method = nodes
            .iter()
            .find(|n| n.name == "print")
            .expect("print node");
        assert_eq!(method.kind, Kind::Method);
        assert_eq!(method.owner.as_deref(), Some("Foo"));
        assert_eq!(method.id, "a.R#Foo.print");
    }

    #[test]
    fn an_r_dot_hidden_name_is_not_exported() {
        let mut extractor = Extractor::new().expect("build extractor");
        let source = ".hidden <- function() {\n}\n";
        let (nodes, _edges) = extractor.extract_file("a.R", source, "r");
        let hidden = nodes
            .iter()
            .find(|n| n.name == ".hidden")
            .expect(".hidden node");
        assert!(!hidden.exported);
    }

    /// The raw edges of one TS source.
    fn ts_edges(source: &str) -> Vec<RawEdge> {
        let mut extractor = Extractor::new().expect("build extractor");
        extractor.extract_file("app.ts", source, "typescript").1
    }

    /// The `(specifier, import_name)` of the one `calls` edge named `name`.
    fn call_of(edges: &[RawEdge], name: &str) -> (Option<String>, Option<String>) {
        let edge = edges
            .iter()
            .find(|e| e.relation == Relation::Calls && e.name.as_deref() == Some(name))
            .expect("call edge");
        (edge.specifier.clone(), edge.import_name.clone())
    }

    fn some(a: &str, b: &str) -> (Option<String>, Option<String>) {
        (Some(a.to_string()), Some(b.to_string()))
    }

    #[test]
    fn test_xfile2_alias_call_carries_import_name() {
        let edges =
            ts_edges("import { area as a } from \"./m\";\nexport function run() { a(); }\n");
        assert_eq!(call_of(&edges, "a"), some("./m", "area"));
    }

    #[test]
    fn test_xfile2_default_import_carries_default() {
        let src = "import b from \"./m\";\nimport c, { x as y } from \"./n\";\nimport { default as d } from \"./o\";\nexport function run() { b(); c(); y(); d(); }\n";
        let edges = ts_edges(src);
        assert_eq!(call_of(&edges, "b"), some("./m", "default"));
        assert_eq!(call_of(&edges, "c"), some("./n", "default"));
        assert_eq!(call_of(&edges, "y"), some("./n", "x"));
        assert_eq!(call_of(&edges, "d"), some("./o", "default"));
    }

    #[test]
    fn test_xfile2_namespace_member_call_carries_specifier() {
        let src = "import * as ns from \"./m\";\nimport d, * as ns2 from \"./n\";\nexport function run() { ns.fn(); ns2.g(); ns.a.deep(); ns[\"k\"](); ns(); }\n";
        let edges = ts_edges(src);
        assert_eq!(call_of(&edges, "fn"), some("./m", "fn"));
        assert_eq!(call_of(&edges, "g"), some("./n", "g"));
        assert_eq!(call_of(&edges, "deep"), (None, None));
        assert_eq!(call_of(&edges, "ns"), (None, None));
    }

    #[test]
    fn test_xfile2_shadowed_namespace_receiver_gets_no_specifier() {
        let src = "import * as shapes from \"./m\";\nexport function local(shapes: any) { shapes.perimeter(); }\n";
        assert_eq!(call_of(&ts_edges(src), "perimeter"), (None, None));
    }

    #[test]
    fn test_xfile2_default_export_flag_three_forms() {
        let forms = [
            (
                "export default function build() {}\nexport function plain() {}\n",
                "build",
            ),
            (
                "function make() {}\nexport default make;\nfunction other() {}\n",
                "make",
            ),
            (
                "const create = () => 1;\nexport { create as default };\nconst z = () => 2;\n",
                "create",
            ),
        ];
        for (src, want) in forms {
            let edges = ts_edges(src);
            let flagged: Vec<&str> = edges
                .iter()
                .filter(|e| e.relation == Relation::Contains && e.default_export)
                .filter_map(|e| e.target_id.as_deref())
                .collect();
            assert_eq!(flagged, [format!("app.ts#{want}")], "{src}");
        }
    }

    #[test]
    fn test_xfile2_named_import_raw_edge_unchanged() {
        let edges =
            ts_edges("import { helper } from \"./m\";\nexport function run() { helper(); }\n");
        let call = edges
            .iter()
            .find(|e| e.relation == Relation::Calls)
            .expect("call edge");
        assert_eq!(call.specifier.as_deref(), Some("./m"));
        assert_eq!(call.import_name, None);
        let json = String::from_utf8(serde_json::to_vec(call).expect("serialize")).expect("utf8");
        assert!(!json.contains("import_name"), "{json}");
        assert!(!json.contains("default_export"), "{json}");
    }

    #[test]
    fn test_xfile2_reexport_default_does_not_flag_local() {
        let src =
            "export { make as default } from \"./impl\";\nexport function make() { return 2; }\n";
        assert!(ts_edges(src).iter().all(|e| !e.default_export));
    }

    #[test]
    fn test_xfile2_destructured_param_shadows_namespace() {
        let src = "import * as shapes from \"./m\";\nexport function local({ shapes }: Props) { shapes.perimeter(); }\n";
        assert_eq!(call_of(&ts_edges(src), "perimeter"), (None, None));
    }

    #[test]
    fn test_xfile2_destructured_local_shadows_alias() {
        let src = "import { area as a } from \"./m\";\nexport function run(o: any) { const { a } = o; a(); }\n";
        assert_eq!(call_of(&ts_edges(src), "a"), (None, None));
    }

    #[test]
    fn test_xfile2_top_level_for_of_shadows_alias() {
        let src = "import { area as a } from \"./m\";\nconst xs: Array<() => void> = [];\nfor (const a of xs) { a(); }\n";
        assert_eq!(call_of(&ts_edges(src), "a"), (None, None));
    }

    #[test]
    fn test_xfile2_named_function_expression_shadows_alias() {
        let src = "import { area as a } from \"./m\";\nexport const f = function a() { return a(); };\nexport function g(xs: number[]) { return xs.map(function a() { return a(); }); }\n";
        let edges = ts_edges(src);
        assert!(edges
            .iter()
            .filter(|e| e.relation == Relation::Calls && e.name.as_deref() == Some("a"))
            .all(|e| e.specifier.is_none() && e.import_name.is_none()));
    }

    /// The `(specifier, record)` of every export record in one TS source.
    fn records(source: &str) -> Vec<(Option<String>, ReExport)> {
        ts_edges(source)
            .into_iter()
            .filter_map(|e| e.reexport.map(|r| (e.specifier, r)))
            .collect()
    }

    fn from(spec: &str, record: ReExport) -> (Option<String>, ReExport) {
        (Some(spec.to_string()), record)
    }

    fn named(export: &str, imported: &str) -> ReExport {
        ReExport::Named {
            export: export.to_string(),
            imported: imported.to_string(),
        }
    }

    fn local(export: &str) -> (Option<String>, ReExport) {
        (
            None,
            ReExport::Local {
                export: export.to_string(),
            },
        )
    }

    #[test]
    fn test_xfile3_named_reexport_records() {
        let src = "export { a } from './x';\nexport { b as c, d } from './y';\n";
        assert_eq!(
            records(src),
            [
                from("./x", named("a", "a")),
                from("./y", named("c", "b")),
                from("./y", named("d", "d")),
            ]
        );
        let edge = &ts_edges(src)[0];
        assert_eq!(edge.relation, Relation::Imports);
        assert_eq!(
            (edge.source.as_str(), edge.file.as_str()),
            ("app.ts", "app.ts")
        );
    }

    #[test]
    fn test_xfile3_star_reexport_record() {
        assert_eq!(
            records("export * from './x';\n"),
            [from("./x", ReExport::Star)]
        );
    }

    #[test]
    fn test_xfile3_default_reexport_three_forms() {
        let src = "export { default } from './a';\nexport { default as x } from './b';\nexport { y as default } from './c';\n";
        assert_eq!(
            records(src),
            [
                from("./a", named("default", "default")),
                from("./b", named("x", "default")),
                from("./c", named("default", "y")),
            ]
        );
    }

    #[test]
    fn test_xfile3_star_as_namespace_record() {
        assert_eq!(
            records("export * as ns from './x';\n"),
            [from(
                "./x",
                ReExport::StarAs {
                    export: "ns".to_string()
                }
            )]
        );
    }

    #[test]
    fn test_xfile3_local_records_in_every_file() {
        // The attack edit E1 widened this rule: a Local record now exists in
        // every file, for each export name that mints no function node.
        let src = "export * from './x';\nexport function f() {}\nexport const g = () => 1;\nexport const v = 1, { w } = o;\nconst h = 2;\nexport { h, h as k };\nexport default 5;\n";
        assert_eq!(
            records(src),
            [
                from("./x", ReExport::Star),
                local("v"),
                local("w"),
                local("h"),
                local("k"),
                local("default"),
            ]
        );
        assert_eq!(records("export default function build() {}\n"), []);
    }

    #[test]
    fn test_xfile3_reexport_next_to_error_still_records() {
        let src = "export type * from './t';\nexport * from './x';\nexport { a } from './y';\n";
        assert_eq!(
            records(src),
            [from("./x", ReExport::Star), from("./y", named("a", "a"))]
        );
    }

    #[test]
    fn test_xfile3_type_only_reexport_gives_no_record() {
        let src = "export type { A } from './x';\nexport { type B } from './x';\nexport type * from './x';\nexport { a as \"s-t\" } from './x';\nexport type T = number;\nexport interface I {}\n";
        assert_eq!(records(src), []);
    }

    #[test]
    fn test_xfile3_named_import_member_call_sets_ns_export() {
        let src = "import { tools, a as t } from \"./b\";\nimport * as ns from \"./c\";\nimport { pk } from \"pkg\";\nexport function run() { tools.fn(); t.g(); ns.h(); pk.i(); }\nexport function local(tools: any) { tools.j(); }\n";
        let edges = ts_edges(src);
        let call = |name: &str| {
            edges
                .iter()
                .find(|e| e.relation == Relation::Calls && e.name.as_deref() == Some(name))
                .map(|e| (e.specifier.clone(), e.import_name.clone(), e.ns_export))
                .expect("call edge")
        };
        let some = |s: &str, i: &str, ns: bool| (Some(s.to_string()), Some(i.to_string()), ns);
        assert_eq!(call("fn"), some("./b", "tools", true));
        assert_eq!(call("g"), some("./b", "a", true));
        assert_eq!(call("h"), some("./c", "h", false));
        assert_eq!(call("i"), (None, None, false));
        assert_eq!(call("j"), (None, None, false));
    }

    #[test]
    fn test_xfile3_plain_file_raw_edges_unchanged() {
        let src = "import { helper } from \"./m\";\nexport function run() { return helper(); }\nexport class C { go() { return 1; } }\n";
        let edges = ts_edges(src);
        assert!(edges.iter().all(|e| e.reexport.is_none() && !e.ns_export));
        let json = String::from_utf8(serde_json::to_vec(&edges).expect("serialize")).expect("utf8");
        assert!(!json.contains("reexport"), "{json}");
        assert!(!json.contains("ns_export"), "{json}");
    }
}
