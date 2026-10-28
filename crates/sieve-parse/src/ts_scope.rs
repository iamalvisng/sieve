//! A conservative scope scan for TypeScript and JavaScript files.
//!
//! The scan models module scope only. It does not resolve names the way the
//! TS checker does. Instead it answers three questions about one name:
//! where the name appears, whether any scope other than module scope
//! declares it, and which module-scope bindings it has. A name that a nested
//! scope declares gets the binder `Unknown`, never "shadowed". A construct
//! that the scan does not model also gives `Unknown`. The scan over-counts on
//! purpose. It may refuse where an exact resolver would prove a site. It must
//! never hide a site. The design is `docs/design/rename.md`, rule S4.
//!
//! The rename plan (RN2) uses these facts. Nothing in the graph build calls
//! this module yet.

use std::collections::{HashMap, HashSet};

use tree_sitter::{Node, Tree};

/// A half-open UTF-8 byte range in the source text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Span {
    /// The first byte of the range.
    pub start: usize,
    /// The byte after the last byte of the range.
    pub end: usize,
}

/// The syntax role of one token that equals the scanned name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SiteKind {
    /// The name of a binding: function, class, variable, parameter and so on.
    Declaration,
    /// A value use that is not a call.
    Reference,
    /// The callee of a call expression.
    Call,
    /// The tag name in a JSX opening or self-closing element.
    JsxOpen,
    /// The tag name in a JSX closing element.
    JsxClose,
    /// A name in an `import` statement.
    ImportSpecifier,
    /// A name in an `export { ... }` clause with no `from`.
    ExportClause,
    /// A name in `export { ... } from` or `export * as ns from`.
    ReExportSpecifier,
    /// The name in `export default name` or `export { name as default }`.
    ExportDefault,
    /// The name in `export as namespace name`.
    ExportAsNamespace,
    /// The property in `x.name`.
    MemberProperty,
    /// A key in an object literal, a class member or a property signature.
    ObjectKey,
    /// A shorthand property, such as `{ name }`.
    ShorthandProperty,
    /// A statement label.
    Label,
    /// A private class name, such as `#name`.
    PrivateName,
    /// A name in a type position.
    TypePosition,
    /// A word in a string literal.
    String,
    /// A word in a template string or in JSX text.
    Template,
    /// A word in a comment that is not a JSDoc site.
    Comment,
    /// A word in a JSDoc `@link`, `@linkcode`, `@linkplain` or `@see` tag.
    JsDoc,
    /// A word in the `{...}` type of a JSDoc tag.
    JsDocType,
}

/// What a token binds to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Binder {
    /// A module-scope declaration. The span is its first name token.
    Module(Span),
    /// The import binding with this index in `FileScopes::imports`.
    Import(usize),
    /// No declaration in the file. The name is a global or a missing name.
    Global,
    /// The token binds nothing: a key, a label, a string or a comment.
    NoBinding,
    /// The scan cannot say. Treat the token as unproven.
    Unknown,
}

/// One token that equals the scanned name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Site {
    /// The syntax role of the token.
    pub kind: SiteKind,
    /// The first byte of the name.
    pub start: usize,
    /// The byte after the last byte of the name.
    pub end: usize,
    /// The line of the first byte, counted from 1.
    pub line: usize,
    /// The UTF-8 byte column of the first byte, counted from 1.
    pub col: usize,
    /// What the token binds to.
    pub binder: Binder,
    /// True when the name is the whole content of a string or template.
    pub whole: bool,
}

/// The way an import statement binds a name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImportKind {
    /// `import { a }` or `import { a as b }`.
    Named,
    /// `import a from`.
    Default,
    /// `import * as a from`.
    Namespace,
    /// `import a = require("x")`.
    Require,
}

/// One name that an import statement binds in module scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportBinding {
    /// The local name.
    pub local: String,
    /// The exported name, for a named import.
    pub imported: Option<String>,
    /// The module specifier text.
    pub source: String,
    /// The form of the import.
    pub kind: ImportKind,
    /// True for `import type` and for an inline `type` modifier.
    pub type_only: bool,
}

/// The module-scope declarations of one name, counted per space.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ModuleBindings {
    /// Declarations in the value space.
    pub value: usize,
    /// Declarations in the type space.
    pub ty: usize,
    /// Declarations in the namespace space.
    pub namespace: usize,
    /// The index of the import that binds the name, if any.
    pub import: Option<usize>,
    /// The first declaring token that is not an import.
    pub span: Option<Span>,
    /// Every declaring token, in source order.
    pub decls: Vec<DeclRecord>,
}

/// One module-scope declaring token and the spaces it adds the name to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeclRecord {
    /// True when the declaration adds the name to the value space.
    pub value: bool,
    /// True when the declaration adds the name to the type space.
    pub ty: bool,
    /// The index of the import, when the declaration is an import.
    pub import: Option<usize>,
    /// The declaring token.
    pub span: Span,
}

/// The kind of a dynamic module access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DynamicKind {
    /// `import(...)`.
    ImportCall,
    /// `require(...)`.
    RequireCall,
    /// `import x = require(...)`.
    ImportRequire,
    /// `import.meta.glob(...)` and its variants.
    MetaGlob,
    /// `require.context(...)`.
    RequireContext,
}

/// One dynamic module access.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DynamicUse {
    /// The kind of access.
    pub kind: DynamicKind,
    /// The first byte of the call or of the `import x = require()` clause.
    pub start: usize,
    /// The first argument, when it is a plain string literal.
    pub literal: Option<String>,
    /// What the arguments of a glob or `require.context` call say.
    pub shape: DynShape,
}

/// The arguments of a glob or `require.context` call, as the scan read them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DynShape {
    /// Any other access: only `literal` matters.
    Plain,
    /// `import.meta.glob*(patterns, options)`.
    Glob {
        /// Every pattern, or `None` when one is not a literal or the
        /// patterns are not a string or a literal array.
        patterns: Option<Vec<String>>,
        /// False when the options hold a `base` key or are not an object
        /// literal of plain pairs.
        plain_options: bool,
    },
    /// `require.context(dir, deep, regex)`.
    Context {
        /// The directory, when it is a literal.
        dir: Option<String>,
        /// True when `deep` is the literal `false`.
        shallow: bool,
        /// True when the regex is a regex literal, or is absent.
        regex_literal: bool,
    },
}

/// File-level facts that the scan reports.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FileFlags {
    /// True when a top-level `import` or `export` exists.
    pub is_module: bool,
    /// True when the tree holds an ERROR or MISSING node.
    pub has_error: bool,
    /// True when the file holds a `with` statement.
    pub has_with: bool,
    /// True when the file calls `eval(...)` directly.
    pub has_direct_eval: bool,
    /// True when an identifier holds a `\u` escape.
    pub has_identifier_escape: bool,
    /// Every dynamic module access.
    pub dynamic: Vec<DynamicUse>,
    /// True for an `export as namespace name` statement.
    pub has_export_as_namespace: bool,
    /// True for an `export =` statement.
    pub has_export_assignment: bool,
    /// True for `module.exports`.
    pub has_module_exports: bool,
    /// True for an assignment to `exports.x`.
    pub has_exports_assign: bool,
    /// The sources of `import * as ns from`.
    pub namespace_imports: Vec<String>,
    /// The sources of `export * from`.
    pub export_star: Vec<String>,
    /// The sources of `export * as ns from`.
    pub export_namespace: Vec<String>,
}

/// The result of one scan: every site of a name, plus the file facts.
#[derive(Debug, Clone, Default)]
pub(crate) struct FileScopes {
    /// The scanned name.
    pub name: String,
    /// Every token that equals the name, in source order.
    pub sites: Vec<Site>,
    /// The imports that bind names in module scope.
    pub imports: Vec<ImportBinding>,
    /// The file-level facts.
    pub flags: FileFlags,
    nested: HashSet<String>,
    module: HashMap<String, ModuleBindings>,
}

impl FileScopes {
    /// True when any scope other than module scope declares `name`.
    pub(crate) fn declares(&self, name: &str) -> bool {
        self.nested.contains(name)
    }

    /// The module-scope declarations of `name`, counted per space.
    pub(crate) fn module_bindings(&self, name: &str) -> ModuleBindings {
        self.module.get(name).cloned().unwrap_or_default()
    }
}

/// How a token looks up its binder once the walk is done.
#[derive(Debug, Clone, Copy)]
enum Lookup {
    Value,
    Type,
    Either,
    NoBinding,
    Unknown,
    Fixed(Binder),
}

/// The spaces that a declaration adds a name to.
#[derive(Debug, Clone, Copy)]
struct Spaces {
    value: bool,
    ty: bool,
    ns: bool,
}

const VALUE: Spaces = Spaces {
    value: true,
    ty: false,
    ns: false,
};
const TYPE: Spaces = Spaces {
    value: false,
    ty: true,
    ns: false,
};
const VALUE_TYPE: Spaces = Spaces {
    value: true,
    ty: true,
    ns: false,
};
const NAMESPACE: Spaces = Spaces {
    value: true,
    ty: false,
    ns: true,
};

/// Parents under which an identifier is a plain value use.
const REFERENCE_PARENTS: &[&str] = &[
    "arguments",
    "array",
    "array_pattern",
    "arrow_function",
    "as_expression",
    "assignment_expression",
    "assignment_pattern",
    "augmented_assignment_expression",
    "await_expression",
    "binary_expression",
    "call_expression",
    "computed_property_name",
    "decorator",
    "enum_assignment",
    "export_statement",
    "expression_statement",
    "extends_clause",
    "for_in_statement",
    "for_statement",
    "instantiation_expression",
    "jsx_expression",
    "member_expression",
    "new_expression",
    "non_null_expression",
    "object_assignment_pattern",
    "object_pattern",
    "optional_parameter",
    "pair",
    "pair_pattern",
    "parenthesized_expression",
    "public_field_definition",
    "required_parameter",
    "rest_pattern",
    "return_statement",
    "satisfies_expression",
    "sequence_expression",
    "spread_element",
    "subscript_expression",
    "switch_case",
    "template_substitution",
    "ternary_expression",
    "throw_statement",
    "type_assertion",
    "type_query",
    "unary_expression",
    "update_expression",
    "variable_declarator",
    "yield_expression",
    "nested_identifier",
    "nested_type_identifier",
];

/// JSDoc tags whose `{...}` part holds a type.
const JSDOC_TYPE_TAGS: &[&str] = &[
    "type",
    "param",
    "returns",
    "return",
    "typedef",
    "property",
    "prop",
    "throws",
    "satisfies",
    "this",
    "enum",
    "extends",
    "augments",
    "implements",
    "callback",
    "template",
    "yields",
];

/// JSDoc tags that name a symbol.
const JSDOC_LINK_TAGS: &[&str] = &["see", "link", "linkcode", "linkplain"];

/// Scans a parsed TS or JS file for every token that equals `name`.
pub(crate) fn scope_file(tree: &Tree, source: &str, name: &str) -> FileScopes {
    let lines = line_starts(source);
    let mut walker = Walker {
        src: source,
        name,
        lines,
        scopes: FileScopes {
            name: name.to_string(),
            ..FileScopes::default()
        },
        pending: Vec::new(),
        decl_tokens: HashSet::new(),
        import_by_specifier: HashMap::new(),
        depth: 0,
        ns_level: None,
        unknown: 0,
        whole_ranges: Vec::new(),
    };
    let root = tree.root_node();
    if root.has_error() {
        walker.scopes.flags.has_error = true;
    }
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if matches!(child.kind(), "import_statement" | "export_statement") {
            walker.scopes.flags.is_module = true;
        }
    }
    walker.visit(root);
    walker.finish()
}

struct Walker<'s> {
    src: &'s str,
    name: &'s str,
    lines: Vec<usize>,
    scopes: FileScopes,
    pending: Vec<(Site, Lookup)>,
    decl_tokens: HashSet<usize>,
    import_by_specifier: HashMap<usize, usize>,
    depth: usize,
    ns_level: Option<usize>,
    unknown: usize,
    whole_ranges: Vec<(usize, usize)>,
}

impl Walker<'_> {
    fn text(&self, node: Node) -> &str {
        self.src.get(node.byte_range()).unwrap_or("")
    }

    fn finish(mut self) -> FileScopes {
        let pending = std::mem::take(&mut self.pending);
        for (mut site, lookup) in pending {
            site.binder = self.resolve(lookup);
            self.scopes.sites.push(site);
        }
        self.scopes.sites.sort_by_key(|s| s.start);
        self.scopes
    }

    fn resolve(&self, lookup: Lookup) -> Binder {
        let space = match lookup {
            Lookup::NoBinding => return Binder::NoBinding,
            Lookup::Unknown => return Binder::Unknown,
            Lookup::Fixed(binder) => return binder,
            other => other,
        };
        if self.scopes.nested.contains(self.name) {
            return Binder::Unknown;
        }
        let Some(b) = self.scopes.module.get(self.name) else {
            return Binder::Global;
        };
        // Bind only when exactly one declaring token matches the space. Two
        // imports, overloads or a merged declaration give Unknown.
        let mut matching = b.decls.iter().filter(|d| match space {
            Lookup::Value => d.value,
            Lookup::Type => d.ty,
            _ => true,
        });
        match (matching.next(), matching.next()) {
            (None, _) => Binder::Global,
            (Some(d), None) => d.import.map_or(Binder::Module(d.span), Binder::Import),
            _ => Binder::Unknown,
        }
    }

    // ---- declarations ----

    /// Records a declaration of the name in `node`. A nested scope or a
    /// namespace body also adds it to the nested set.
    fn declare(&mut self, node: Node, spaces: Spaces, is_import: Option<usize>) {
        let name = self.text(node).to_string();
        self.decl_tokens.insert(node.start_byte());
        let in_module_scope = self.depth == 0;
        let in_namespace_body = self.ns_level == Some(self.depth);
        if in_module_scope || in_namespace_body {
            let entry = self.scopes.module.entry(name.clone()).or_default();
            entry.value += usize::from(spaces.value);
            entry.ty += usize::from(spaces.ty);
            entry.namespace += usize::from(spaces.ns);
            let span = Span {
                start: node.start_byte(),
                end: node.end_byte(),
            };
            entry.decls.push(DeclRecord {
                value: spaces.value,
                ty: spaces.ty,
                import: is_import,
                span,
            });
            match is_import {
                Some(idx) => {
                    entry.import.get_or_insert(idx);
                }
                None => {
                    entry.span.get_or_insert(span);
                }
            }
        }
        if !in_module_scope {
            self.scopes.nested.insert(name);
        }
    }

    /// Records a declaration that always lives in a nested scope.
    fn declare_nested(&mut self, node: Node) {
        self.decl_tokens.insert(node.start_byte());
        let name = self.text(node).to_string();
        self.scopes.nested.insert(name);
    }

    fn declare_field(&mut self, node: Node, field: &str, spaces: Spaces) {
        if let Some(name) = node.child_by_field_name(field) {
            self.declare(name, spaces, None);
        }
    }

    fn nested_field(&mut self, node: Node, field: &str) {
        if let Some(name) = node.child_by_field_name(field) {
            self.declare_nested(name);
        }
    }

    /// Declares each name that a binding pattern introduces.
    fn declare_pattern(&mut self, node: Node) {
        match node.kind() {
            "identifier" | "shorthand_property_identifier_pattern" => {
                self.declare(node, VALUE, None);
            }
            "object_pattern" | "array_pattern" | "rest_pattern" => {
                let mut cursor = node.walk();
                let children: Vec<Node> = node.named_children(&mut cursor).collect();
                for child in children {
                    self.declare_pattern(child);
                }
            }
            "pair_pattern" => {
                if let Some(value) = node.child_by_field_name("value") {
                    self.declare_pattern(value);
                }
            }
            "assignment_pattern" | "object_assignment_pattern" => {
                if let Some(left) = node.child_by_field_name("left") {
                    self.declare_pattern(left);
                }
            }
            _ => {}
        }
    }

    // ---- the walk ----

    fn visit(&mut self, node: Node) {
        if node.is_missing() {
            self.scopes.flags.has_error = true;
        }
        let mut bump = false;
        let mut unknown = false;
        match node.kind() {
            "ERROR" => {
                self.scopes.flags.has_error = true;
                unknown = true;
            }
            "with_statement" => {
                self.scopes.flags.has_with = true;
                unknown = true;
            }
            "import_statement" => self.on_import(node),
            "export_statement" => self.on_export(node),
            "function_declaration" | "generator_function_declaration" | "function_signature" => {
                self.declare_field(node, "name", VALUE);
                bump = true;
            }
            "function_expression" | "function" | "generator_function" => {
                self.nested_field(node, "name");
                bump = true;
            }
            "arrow_function" => {
                if let Some(param) = node.child_by_field_name("parameter") {
                    self.declare_nested(param);
                }
                bump = true;
            }
            "method_definition"
            | "function_type"
            | "constructor_type"
            | "call_signature"
            | "construct_signature"
            | "method_signature"
            | "abstract_method_signature"
            | "index_signature"
            | "statement_block"
            | "switch_body"
            | "class_static_block"
            | "public_field_definition"
            | "for_statement" => bump = true,
            "for_in_statement" => {
                if node.child_by_field_name("kind").is_some() {
                    if let Some(left) = node.child_by_field_name("left") {
                        self.depth += 1;
                        self.declare_pattern(left);
                        self.depth -= 1;
                    }
                }
                bump = true;
            }
            "catch_clause" => {
                if let Some(param) = node.child_by_field_name("parameter") {
                    self.depth += 1;
                    self.declare_pattern(param);
                    self.depth -= 1;
                }
                bump = true;
            }
            "required_parameter" | "optional_parameter" => {
                if let Some(pattern) = node.child_by_field_name("pattern") {
                    self.declare_pattern(pattern);
                }
            }
            "formal_parameters" => {
                let mut cursor = node.walk();
                let ids: Vec<Node> = node
                    .named_children(&mut cursor)
                    .filter(|c| c.kind() == "identifier")
                    .collect();
                for id in ids {
                    self.declare_nested(id);
                }
            }
            "variable_declarator" => {
                if let Some(name) = node.child_by_field_name("name") {
                    self.declare_pattern(name);
                }
            }
            "class_declaration" | "abstract_class_declaration" => {
                self.declare_field(node, "name", VALUE_TYPE);
            }
            "class" => self.nested_field(node, "name"),
            "interface_declaration" | "type_alias_declaration" => {
                self.declare_field(node, "name", TYPE);
            }
            "enum_declaration" => {
                self.declare_field(node, "name", VALUE_TYPE);
                self.declare_enum_members(node);
            }
            "type_parameter" | "mapped_type_clause" => self.nested_field(node, "name"),
            "infer_type" => {
                let mut cursor = node.walk();
                let ids: Vec<Node> = node
                    .named_children(&mut cursor)
                    .filter(|c| c.kind() == "type_identifier")
                    .collect();
                for id in ids {
                    self.declare_nested(id);
                }
            }
            "import_alias" => {
                let mut cursor = node.walk();
                let first = node
                    .named_children(&mut cursor)
                    .find(|c| c.kind() == "identifier");
                if let Some(id) = first {
                    self.declare(
                        id,
                        Spaces {
                            value: true,
                            ty: true,
                            ns: true,
                        },
                        None,
                    );
                }
            }
            "internal_module" | "module" => return self.on_namespace(node),
            "ambient_declaration" if has_child_kind(node, "global") => {
                return self.on_global(node);
            }
            "string" | "template_string" => self.on_string(node),
            "call_expression" => self.on_call(node),
            "member_expression" => self.on_member(node),
            "assignment_expression" => self.on_assign(node),
            "identifier"
            | "type_identifier"
            | "property_identifier"
            | "shorthand_property_identifier"
            | "shorthand_property_identifier_pattern"
            | "private_property_identifier"
            | "statement_identifier"
            | "string_fragment"
            | "jsx_text"
            | "comment" => self.token(node),
            _ => {}
        }
        if bump {
            self.depth += 1;
        }
        if unknown {
            self.unknown += 1;
        }
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        for child in children {
            self.visit(child);
        }
        if bump {
            self.depth -= 1;
        }
        if unknown {
            self.unknown -= 1;
        }
    }

    fn declare_enum_members(&mut self, node: Node) {
        let Some(body) = node.child_by_field_name("body") else {
            return;
        };
        let mut cursor = body.walk();
        let members: Vec<Node> = body.named_children(&mut cursor).collect();
        for member in members {
            let name = match member.kind() {
                "property_identifier" => Some(member),
                "enum_assignment" => member.child_by_field_name("name"),
                _ => None,
            };
            if let Some(name) = name {
                self.declare_nested(name);
            }
        }
    }

    /// Walks a namespace or `declare module` node. The body counts as a
    /// module-scope space for `module_bindings`, and as a nested scope.
    fn on_namespace(&mut self, node: Node) {
        if let Some(name) = node.child_by_field_name("name") {
            let mut first = name;
            while first.kind() == "nested_identifier" {
                match first.child_by_field_name("object") {
                    Some(next) => first = next,
                    None => break,
                }
            }
            if first.kind() == "identifier" {
                self.declare(first, NAMESPACE, None);
            }
        }
        let body = node.child_by_field_name("body");
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        for child in children {
            if Some(child) == body {
                self.namespace_body(child);
            } else {
                self.visit(child);
            }
        }
    }

    /// Walks `declare global { ... }`.
    fn on_global(&mut self, node: Node) {
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        for child in children {
            if child.kind() == "statement_block" {
                self.namespace_body(child);
            } else {
                self.visit(child);
            }
        }
    }

    fn namespace_body(&mut self, body: Node) {
        self.depth += 1;
        let saved = self.ns_level.replace(self.depth);
        let mut cursor = body.walk();
        let children: Vec<Node> = body.children(&mut cursor).collect();
        for child in children {
            self.visit(child);
        }
        self.ns_level = saved;
        self.depth -= 1;
    }

    fn on_import(&mut self, node: Node) {
        let source = node
            .child_by_field_name("source")
            .and_then(|s| string_value(s, self.src))
            .unwrap_or_default();
        let statement_type_only = has_child_kind(node, "type");
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        for child in children {
            match child.kind() {
                "import_clause" => self.on_import_clause(child, &source, statement_type_only),
                "import_require_clause" => {
                    let literal = child
                        .child_by_field_name("source")
                        .and_then(|s| string_value(s, self.src));
                    let id = {
                        let mut c = child.walk();
                        let found = child
                            .named_children(&mut c)
                            .find(|n| n.kind() == "identifier");
                        found
                    };
                    if let Some(id) = id {
                        let local = self.text(id).to_string();
                        self.add_import(
                            id,
                            None,
                            ImportBinding {
                                local,
                                imported: None,
                                source: literal.clone().unwrap_or_default(),
                                kind: ImportKind::Require,
                                type_only: statement_type_only,
                            },
                        );
                    }
                    self.scopes.flags.dynamic.push(DynamicUse {
                        kind: DynamicKind::ImportRequire,
                        start: child.start_byte(),
                        literal,
                        shape: DynShape::Plain,
                    });
                }
                _ => {}
            }
        }
    }

    fn on_import_clause(&mut self, clause: Node, source: &str, type_only: bool) {
        let mut cursor = clause.walk();
        let children: Vec<Node> = clause.children(&mut cursor).collect();
        for child in children {
            match child.kind() {
                "identifier" => {
                    let local = self.text(child).to_string();
                    let binding = ImportBinding {
                        local,
                        imported: None,
                        source: source.to_string(),
                        kind: ImportKind::Default,
                        type_only,
                    };
                    self.add_import(child, None, binding);
                }
                "namespace_import" => {
                    self.scopes.flags.namespace_imports.push(source.to_string());
                    let mut c = child.walk();
                    let id = child
                        .named_children(&mut c)
                        .find(|n| n.kind() == "identifier");
                    if let Some(id) = id {
                        let local = self.text(id).to_string();
                        let binding = ImportBinding {
                            local,
                            imported: None,
                            source: source.to_string(),
                            kind: ImportKind::Namespace,
                            type_only,
                        };
                        self.add_import(id, None, binding);
                    }
                }
                "named_imports" => {
                    let mut c = child.walk();
                    let specs: Vec<Node> = child
                        .named_children(&mut c)
                        .filter(|n| n.kind() == "import_specifier")
                        .collect();
                    for spec in specs {
                        let Some(name) = spec.child_by_field_name("name") else {
                            continue;
                        };
                        let local_node = spec.child_by_field_name("alias").unwrap_or(name);
                        let binding = ImportBinding {
                            local: self.text(local_node).to_string(),
                            imported: Some(self.text(name).to_string()),
                            source: source.to_string(),
                            kind: ImportKind::Named,
                            type_only: type_only || has_child_kind(spec, "type"),
                        };
                        self.add_import(local_node, Some(spec), binding);
                    }
                }
                _ => {}
            }
        }
    }

    fn add_import(&mut self, local: Node, specifier: Option<Node>, binding: ImportBinding) {
        let idx = self.scopes.imports.len();
        self.scopes.imports.push(binding);
        if let Some(spec) = specifier {
            self.import_by_specifier.insert(spec.start_byte(), idx);
        }
        self.declare(
            local,
            Spaces {
                value: true,
                ty: true,
                ns: false,
            },
            Some(idx),
        );
    }

    fn on_export(&mut self, node: Node) {
        let source = node
            .child_by_field_name("source")
            .and_then(|s| string_value(s, self.src))
            .unwrap_or_default();
        if has_child_kind(node, "as") && has_child_kind(node, "namespace") {
            self.scopes.flags.has_export_as_namespace = true;
        }
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        for child in children {
            match child.kind() {
                "=" => self.scopes.flags.has_export_assignment = true,
                "*" => self.scopes.flags.export_star.push(source.clone()),
                "namespace_export" => self.scopes.flags.export_namespace.push(source.clone()),
                _ => {}
            }
        }
    }

    fn on_call(&mut self, node: Node) {
        let Some(func) = node.child_by_field_name("function") else {
            return;
        };
        let literal = node
            .child_by_field_name("arguments")
            .and_then(|args| args.named_child(0))
            .and_then(|first| string_value(first, self.src));
        // `(0, require)(x)` calls `require` like a bare call does.
        let func = unwrap_callee(func);
        let kind = match func.kind() {
            "import" => Some(DynamicKind::ImportCall),
            "identifier" => match self.text(func) {
                "require" => Some(DynamicKind::RequireCall),
                "eval" => {
                    self.scopes.flags.has_direct_eval = true;
                    None
                }
                _ => None,
            },
            "member_expression" => {
                let object = func.child_by_field_name("object");
                let property = func.child_by_field_name("property");
                match (object, property) {
                    (Some(o), Some(p))
                        if o.kind() == "meta_property" && self.text(p).starts_with("glob") =>
                    {
                        Some(DynamicKind::MetaGlob)
                    }
                    (Some(o), Some(p))
                        if o.kind() == "meta_property" && self.text(p) == "webpackContext" =>
                    {
                        Some(DynamicKind::RequireContext)
                    }
                    (Some(o), Some(p))
                        if o.kind() == "identifier"
                            && self.text(o) == "Object"
                            && self.text(p) == "defineProperty" =>
                    {
                        let target = node
                            .child_by_field_name("arguments")
                            .and_then(|args| args.named_child(0));
                        if target
                            .is_some_and(|t| matches!(self.text(t), "exports" | "module.exports"))
                        {
                            self.scopes.flags.has_exports_assign = true;
                        }
                        None
                    }
                    (Some(o), Some(p))
                        if o.kind() == "identifier"
                            && self.text(o) == "require"
                            && self.text(p) == "context" =>
                    {
                        Some(DynamicKind::RequireContext)
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        if let Some(kind) = kind {
            let args = node.child_by_field_name("arguments");
            let shape = match kind {
                DynamicKind::MetaGlob => self.glob_shape(args),
                DynamicKind::RequireContext if is_meta_call(func) => self.webpack_shape(args),
                DynamicKind::RequireContext => self.context_shape(args),
                _ => DynShape::Plain,
            };
            self.scopes.flags.dynamic.push(DynamicUse {
                kind,
                start: node.start_byte(),
                literal,
                shape,
            });
        }
    }

    /// Reads the arguments of `import.meta.glob*(patterns, options)`.
    fn glob_shape(&self, args: Option<Node>) -> DynShape {
        let Some(args) = args else {
            return DynShape::Glob {
                patterns: None,
                plain_options: false,
            };
        };
        let list = named_args(args);
        let patterns = list.first().and_then(|first| {
            if first.kind() == "array" {
                named_args(*first)
                    .into_iter()
                    .map(|e| literal_text(e, self.src))
                    .collect::<Option<Vec<String>>>()
            } else {
                literal_text(*first, self.src).map(|p| vec![p])
            }
        });
        let plain_options = match list.get(1) {
            None => true,
            Some(opts) => opts.kind() == "object" && self.plain_pairs(*opts),
        };
        DynShape::Glob {
            patterns,
            plain_options,
        }
    }

    /// True when an object literal holds only pairs, and none has the key `base`.
    fn plain_pairs(&self, object: Node) -> bool {
        named_args(object).into_iter().all(|pair| {
            if pair.kind() != "pair" {
                return false;
            }
            match pair.child_by_field_name("key") {
                Some(key) if key.kind() == "property_identifier" => self.text(key) != "base",
                Some(key) => string_value(key, self.src).is_some_and(|k| k != "base"),
                None => false,
            }
        })
    }

    /// Reads the arguments of `require.context(dir, deep, regex)`.
    fn context_shape(&self, args: Option<Node>) -> DynShape {
        let list = args.map(named_args).unwrap_or_default();
        DynShape::Context {
            dir: list.first().and_then(|n| literal_text(*n, self.src)),
            shallow: list.get(1).is_some_and(|n| self.text(*n) == "false"),
            regex_literal: list.get(2).is_none_or(|n| n.kind() == "regex"),
        }
    }

    /// Reads the arguments of `import.meta.webpackContext(dir, options)`.
    /// An option that the scan cannot read makes `dir` unknown.
    fn webpack_shape(&self, args: Option<Node>) -> DynShape {
        let list = args.map(named_args).unwrap_or_default();
        let mut dir = list.first().and_then(|n| literal_text(*n, self.src));
        let mut shallow = false;
        let mut regex_literal = true;
        if let Some(opts) = list.get(1) {
            if opts.kind() != "object" {
                dir = None;
            }
            for pair in named_args(*opts) {
                let key = pair.child_by_field_name("key");
                let value = pair.child_by_field_name("value");
                let name = key.map(|k| self.text(k).trim_matches(['"', '\'']).to_string());
                match (pair.kind(), name.as_deref(), value) {
                    ("pair", Some("recursive"), Some(v)) => match self.text(v) {
                        "false" => shallow = true,
                        "true" => {}
                        _ => dir = None,
                    },
                    ("pair", Some("regExp"), Some(v)) => regex_literal = v.kind() == "regex",
                    ("pair", Some(_), Some(_)) => {}
                    _ => dir = None,
                }
            }
        }
        DynShape::Context {
            dir,
            shallow,
            regex_literal,
        }
    }

    fn on_member(&mut self, node: Node) {
        let object = node.child_by_field_name("object");
        let property = node.child_by_field_name("property");
        if let (Some(o), Some(p)) = (object, property) {
            if o.kind() == "identifier" && self.text(o) == "module" && self.text(p) == "exports" {
                self.scopes.flags.has_module_exports = true;
            }
        }
    }

    fn on_assign(&mut self, node: Node) {
        let Some(left) = node.child_by_field_name("left") else {
            return;
        };
        // The grammar parses `using x = f()` as an assignment with a `using` token.
        if has_child_kind(node, "using") {
            self.declare_pattern(left);
            return;
        }
        if matches!(left.kind(), "member_expression" | "subscript_expression") {
            if let Some(o) = left.child_by_field_name("object") {
                if o.kind() == "identifier" && self.text(o) == "exports" {
                    self.scopes.flags.has_exports_assign = true;
                }
            }
        }
    }

    // ---- tokens ----

    fn push(&mut self, start: usize, end: usize, kind: SiteKind, mut lookup: Lookup, whole: bool) {
        if self.unknown > 0 && matches!(lookup, Lookup::Value | Lookup::Type | Lookup::Either) {
            lookup = Lookup::Unknown;
        }
        let line_idx = self
            .lines
            .partition_point(|&s| s <= start)
            .saturating_sub(1);
        let line_start = self.lines.get(line_idx).copied().unwrap_or(0);
        let site = Site {
            kind,
            start,
            end,
            line: line_idx + 1,
            col: start - line_start + 1,
            binder: Binder::Unknown,
            whole,
        };
        self.pending.push((site, lookup));
    }

    fn token(&mut self, node: Node) {
        let kind = node.kind();
        match kind {
            "string_fragment" | "jsx_text" => return self.text_token(node),
            "comment" => return self.comment_token(node),
            _ => {}
        }
        let text = self.text(node);
        if text.contains('\\') {
            self.scopes.flags.has_identifier_escape = true;
            return;
        }
        if kind == "private_property_identifier" {
            if text.strip_prefix('#') == Some(self.name) {
                let (start, end) = (node.start_byte() + 1, node.end_byte());
                self.push(start, end, SiteKind::PrivateName, Lookup::NoBinding, false);
            }
            return;
        }
        if text != self.name {
            return;
        }
        let (site_kind, lookup) = self.classify(node);
        self.push(node.start_byte(), node.end_byte(), site_kind, lookup, false);
    }

    fn classify(&self, node: Node) -> (SiteKind, Lookup) {
        let parent_kind = node.parent().map_or("", |p| p.kind());
        let is_decl = self.decl_tokens.contains(&node.start_byte());
        match node.kind() {
            "statement_identifier" => (SiteKind::Label, Lookup::NoBinding),
            "shorthand_property_identifier" | "shorthand_property_identifier_pattern" => {
                (SiteKind::ShorthandProperty, Lookup::Value)
            }
            "property_identifier" => {
                if is_decl {
                    return (SiteKind::Declaration, Lookup::NoBinding);
                }
                match parent_kind {
                    "member_expression" | "nested_identifier" => {
                        (SiteKind::MemberProperty, Lookup::NoBinding)
                    }
                    "pair"
                    | "pair_pattern"
                    | "method_definition"
                    | "public_field_definition"
                    | "property_signature"
                    | "method_signature"
                    | "abstract_method_signature"
                    | "jsx_attribute"
                    | "enum_body"
                    | "enum_assignment" => (SiteKind::ObjectKey, Lookup::NoBinding),
                    _ => (SiteKind::ObjectKey, Lookup::Unknown),
                }
            }
            "type_identifier" => {
                if is_decl {
                    let kind = if is_default_declaration(node) {
                        SiteKind::ExportDefault
                    } else {
                        SiteKind::Declaration
                    };
                    return (kind, Lookup::Either);
                }
                let is_qualified_name = parent_kind == "nested_type_identifier"
                    && node.parent().and_then(|p| p.child_by_field_name("name")) == Some(node);
                if is_qualified_name {
                    (SiteKind::MemberProperty, Lookup::NoBinding)
                } else {
                    (SiteKind::TypePosition, Lookup::Type)
                }
            }
            _ => self.classify_identifier(node, parent_kind, is_decl),
        }
    }

    fn classify_identifier(
        &self,
        node: Node,
        parent_kind: &str,
        is_decl: bool,
    ) -> (SiteKind, Lookup) {
        if is_decl {
            let kind = if matches!(
                parent_kind,
                "import_specifier"
                    | "import_clause"
                    | "namespace_import"
                    | "import_require_clause"
                    | "import_alias"
            ) {
                SiteKind::ImportSpecifier
            } else if is_default_declaration(node) {
                SiteKind::ExportDefault
            } else {
                SiteKind::Declaration
            };
            return (kind, Lookup::Either);
        }
        if let Some((kind, bare)) = jsx_role(node) {
            // A lowercase bare tag is an intrinsic element and binds nothing.
            let intrinsic = bare
                && self
                    .text(node)
                    .starts_with(|c: char| c.is_ascii_lowercase());
            let lookup = if intrinsic {
                Lookup::NoBinding
            } else {
                Lookup::Value
            };
            return (kind, lookup);
        }
        let parent = node.parent();
        match parent_kind {
            "import_specifier" => {
                let lookup = parent
                    .and_then(|p| self.import_by_specifier.get(&p.start_byte()))
                    .map_or(Lookup::Unknown, |&idx| Lookup::Fixed(Binder::Import(idx)));
                (SiteKind::ImportSpecifier, lookup)
            }
            "export_statement" if parent.is_some_and(|p| has_child_kind(p, "default")) => {
                (SiteKind::ExportDefault, Lookup::Either)
            }
            "export_statement"
                if parent
                    .is_some_and(|p| has_child_kind(p, "as") && has_child_kind(p, "namespace")) =>
            {
                (SiteKind::ExportAsNamespace, Lookup::Unknown)
            }
            "export_specifier" => {
                let specifier = parent;
                let has_source = specifier
                    .and_then(|s| s.parent())
                    .and_then(|clause| clause.parent())
                    .is_some_and(|stmt| stmt.child_by_field_name("source").is_some());
                let is_name = specifier.and_then(|s| s.child_by_field_name("name")) == Some(node);
                if has_source {
                    (SiteKind::ReExportSpecifier, Lookup::NoBinding)
                } else if is_name && alias_is_default(specifier, self.src) {
                    (SiteKind::ExportDefault, Lookup::Either)
                } else if is_name {
                    (SiteKind::ExportClause, Lookup::Either)
                } else {
                    (SiteKind::ExportClause, Lookup::NoBinding)
                }
            }
            "namespace_export" => (SiteKind::ReExportSpecifier, Lookup::NoBinding),
            "call_expression"
                if parent.and_then(|p| p.child_by_field_name("function")) == Some(node) =>
            {
                (SiteKind::Call, Lookup::Value)
            }
            kind if REFERENCE_PARENTS.contains(&kind) => (SiteKind::Reference, Lookup::Value),
            _ => (SiteKind::Reference, Lookup::Unknown),
        }
    }

    /// Words in a string literal, a template string or JSX text.
    fn text_token(&mut self, node: Node) {
        // A whole-string site already covers every fragment of its literal.
        let (from, to) = (node.start_byte(), node.end_byte());
        if self.whole_ranges.iter().any(|&(a, b)| a <= from && to <= b) {
            return;
        }
        let text = self.text(node).to_string();
        let parent = node.parent();
        let in_template =
            parent.is_some_and(|p| matches!(p.kind(), "template_string" | "template_literal_type"));
        let kind = if in_template || node.kind() == "jsx_text" {
            SiteKind::Template
        } else {
            SiteKind::String
        };
        let only_child = parent.is_some_and(|p| {
            let mut c = p.walk();
            let n = p
                .named_children(&mut c)
                .filter(|k| k.kind() != "comment")
                .count();
            n == 1
        });
        let whole = text == self.name && only_child && node.kind() == "string_fragment";
        for at in word_matches(&text, self.name) {
            let start = node.start_byte() + at;
            self.push(
                start,
                start + self.name.len(),
                kind,
                Lookup::NoBinding,
                whole,
            );
        }
    }

    /// A string or template whose decoded value equals the name, written with
    /// an escape such as `"\x4F"`. Such a literal is a whole-string site.
    fn on_string(&mut self, node: Node) {
        if !has_child_kind(node, "escape_sequence") {
            return;
        }
        if decode_literal(node, self.src).as_deref() != Some(self.name) {
            return;
        }
        let kind = if node.kind() == "string" {
            SiteKind::String
        } else {
            SiteKind::Template
        };
        let (start, end) = (node.start_byte() + 1, node.end_byte().saturating_sub(1));
        self.push(start, end, kind, Lookup::NoBinding, true);
        self.whole_ranges.push((node.start_byte(), node.end_byte()));
    }

    /// Words in a comment, with the JSDoc rule of design S11.
    fn comment_token(&mut self, node: Node) {
        let text = self.text(node).to_string();
        for at in word_matches(&text, self.name) {
            let (kind, lookup) = match jsdoc_role(&text, at, self.name) {
                (SiteKind::Comment, _) => (SiteKind::Comment, Lookup::NoBinding),
                (kind, true) => (kind, Lookup::Either),
                (kind, false) => (kind, Lookup::Unknown),
            };
            let start = node.start_byte() + at;
            self.push(start, start + self.name.len(), kind, lookup, false);
        }
    }
}

/// The byte offsets where a line starts. A line ends at `\n`, `\r\n`, a lone
/// `\r`, U+2028 or U+2029, as in the TS scanner.
fn line_starts(source: &str) -> Vec<usize> {
    let mut lines = vec![0usize];
    let mut chars = source.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            '\n' | '\u{2028}' | '\u{2029}' => lines.push(i + c.len_utf8()),
            '\r' if chars.peek().map(|&(_, n)| n) != Some('\n') => lines.push(i + 1),
            _ => {}
        }
    }
    lines
}

/// Strips parentheses and comma sequences from a callee: `(0, require)`.
fn unwrap_callee(mut node: Node) -> Node {
    loop {
        let next = match node.kind() {
            "parenthesized_expression" => node.named_child(0),
            "sequence_expression" => node
                .named_child_count()
                .checked_sub(1)
                .and_then(|last| node.named_child(last)),
            _ => None,
        };
        match next {
            Some(inner) => node = inner,
            None => return node,
        }
    }
}

/// True when `name` is the name of a declaration in `export default ...`.
fn is_default_declaration(name: Node) -> bool {
    let Some(decl) = name.parent() else {
        return false;
    };
    decl.child_by_field_name("name") == Some(name)
        && decl
            .parent()
            .is_some_and(|p| p.kind() == "export_statement" && has_child_kind(p, "default"))
}

/// True when an export specifier exports under the name `default`.
fn alias_is_default(specifier: Option<Node>, src: &str) -> bool {
    let alias = specifier.and_then(|s| s.child_by_field_name("alias"));
    alias
        .and_then(|a| src.get(a.byte_range()))
        .is_some_and(|t| t.trim_matches(['"', '\'']) == "default")
}

/// The value of a string or template node with its escapes decoded. A
/// template with a substitution, or an escape that cannot be decoded, gives
/// `None`.
fn decode_literal(node: Node, src: &str) -> Option<String> {
    let mut cursor = node.walk();
    let mut out = String::new();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "string_fragment" => out.push_str(src.get(child.byte_range())?),
            "escape_sequence" => out.push_str(&decode_escape(src.get(child.byte_range())?)?),
            "template_substitution" => return None,
            _ => {}
        }
    }
    Some(out)
}

fn decode_escape(text: &str) -> Option<String> {
    let rest = text.strip_prefix('\\')?;
    let first = rest.chars().next()?;
    let code = match first {
        'x' => u32::from_str_radix(rest.get(1..3)?, 16).ok()?,
        'u' => match rest.get(1..)?.strip_prefix('{') {
            Some(braced) => u32::from_str_radix(braced.strip_suffix('}')?, 16).ok()?,
            None => u32::from_str_radix(rest.get(1..5)?, 16).ok()?,
        },
        'n' => 10,
        't' => 9,
        'r' => 13,
        'b' => 8,
        'f' => 12,
        'v' => 11,
        '0'..='9' => return None,
        '\n' | '\r' | '\u{2028}' | '\u{2029}' => return Some(String::new()),
        other => other as u32,
    };
    char::from_u32(code).map(String::from)
}

fn has_child_kind(node: Node, kind: &str) -> bool {
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).any(|c| c.kind() == kind);
    found
}

/// True when the callee is a member of `import.meta`.
fn is_meta_call(func: Node) -> bool {
    func.child_by_field_name("object")
        .is_some_and(|o| o.kind() == "meta_property")
}

/// The named children of a node, without comments.
fn named_args(node: Node) -> Vec<Node> {
    let mut cursor = node.walk();
    let list: Vec<Node> = node
        .named_children(&mut cursor)
        .filter(|n| n.kind() != "comment")
        .collect();
    list
}

/// The text of a string literal, or of a template with no substitution.
fn literal_text(node: Node, src: &str) -> Option<String> {
    if node.kind() != "template_string" {
        return string_value(node, src);
    }
    let raw = src.get(node.byte_range())?;
    let inner = raw.strip_prefix('`')?.strip_suffix('`')?;
    let plain = named_args(node).is_empty() && !inner.contains(['\\', '$']);
    plain.then(|| inner.to_string())
}

/// The text of a plain string literal, or `None` for any other node.
fn string_value(node: Node, src: &str) -> Option<String> {
    if node.kind() != "string" {
        return None;
    }
    let mut cursor = node.walk();
    let mut out = String::new();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "string_fragment" => out.push_str(src.get(child.byte_range())?),
            "escape_sequence" => return None,
            _ => {}
        }
    }
    Some(out)
}

/// The JSX role of an identifier: the tag name, or the root of a member tag.
/// The flag is true when the identifier is the whole tag name.
fn jsx_role(node: Node) -> Option<(SiteKind, bool)> {
    let mut cur = node;
    let mut bare = true;
    while let Some(parent) = cur.parent() {
        let is_name = parent.child_by_field_name("name") == Some(cur);
        match parent.kind() {
            "member_expression" | "nested_identifier"
                if parent.child_by_field_name("object") == Some(cur) =>
            {
                cur = parent;
                bare = false;
            }
            "jsx_opening_element" | "jsx_self_closing_element" if is_name => {
                return Some((SiteKind::JsxOpen, bare));
            }
            "jsx_closing_element" if is_name => return Some((SiteKind::JsxClose, bare)),
            _ => return None,
        }
    }
    None
}

fn is_ident_part(c: char) -> bool {
    c == '$' || c == '_' || c.is_alphanumeric()
}

/// The byte offsets where `name` occurs as a whole word in `text`. The word
/// test uses the JS identifier-part class, not a regex word boundary.
pub(crate) fn word_matches<'a>(text: &'a str, name: &'a str) -> impl Iterator<Item = usize> + 'a {
    let found = if name.is_empty() {
        Vec::new()
    } else {
        text.match_indices(name).map(|(i, _)| i).collect()
    };
    found.into_iter().filter(move |&i| {
        let before = text[..i].chars().next_back();
        let after = text[i + name.len()..].chars().next();
        !before.is_some_and(is_ident_part) && !after.is_some_and(is_ident_part)
    })
}

/// The tag word that starts a JSDoc line, such as `param` for ` * @param x`.
fn line_tag(line: &str) -> Option<&str> {
    let rest = line
        .trim_start_matches(|c: char| c.is_whitespace() || c == '*' || c == '/')
        .strip_prefix('@')?;
    let end = rest
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(rest.len());
    Some(&rest[..end])
}

/// The text between the `{` at byte `open` and its matching `}`.
fn brace_content(text: &str, open: usize) -> Option<&str> {
    let mut depth = 0usize;
    for (i, b) in text.bytes().enumerate().skip(open) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return text.get(open + 1..i);
                }
            }
            _ => {}
        }
    }
    None
}

/// The JSDoc role of the word at byte `at` in comment `text`. The flag is
/// true only when the word is a proven symbol position: a `{@link}` tag, a
/// type brace that follows a known type tag, or the first word after `@see`.
/// Any other word in braces or in a link tag is in doubt, so the flag is false.
fn jsdoc_role(text: &str, at: usize, name: &str) -> (SiteKind, bool) {
    if !text.starts_with("/**") {
        return (SiteKind::Comment, false);
    }
    let bytes = text.as_bytes();
    // Stop at the first unmatched `{`: it is the brace that holds the word.
    let mut depth = 0usize;
    let mut outer = None;
    for i in (0..at).rev() {
        match bytes[i] {
            b'}' => depth += 1,
            b'{' if depth > 0 => depth -= 1,
            b'{' => {
                outer = Some(i);
                break;
            }
            _ => {}
        }
    }
    let line_start = |pos: usize| text[..pos].rfind('\n').map_or(0, |n| n + 1);
    if let Some(open) = outer {
        let inner = &text[open + 1..];
        for link in ["@link", "@linkcode", "@linkplain"] {
            let is_link = inner
                .strip_prefix(link)
                .is_some_and(|r| r.starts_with(char::is_whitespace));
            if is_link {
                let after = inner.get(link.len()..at - open - 1).unwrap_or("x");
                return (SiteKind::JsDoc, after.trim().is_empty());
            }
        }
        let before = &text[line_start(open)..open];
        return match line_tag(before) {
            Some(t) if JSDOC_TYPE_TAGS.contains(&t) => {
                // Proven only for a type brace that holds the bare name and
                // is the first thing after the tag. Any other type is in doubt.
                let after_tag = before.rsplit_once(t).map_or("", |(_, r)| r);
                // `{name}` and `{typeof name}` both name the symbol itself.
                let bare = brace_content(text, open).is_some_and(|c| {
                    let c = c.trim();
                    c == name
                        || c.strip_prefix("typeof")
                            .is_some_and(|r| r.starts_with(char::is_whitespace) && r.trim() == name)
                });
                (SiteKind::JsDocType, after_tag.trim().is_empty() && bare)
            }
            // In doubt: a word in other braces of a doc comment is unproven.
            _ => (SiteKind::JsDoc, false),
        };
    }
    // Find the nearest tag at or above the word, across continuation lines.
    let mut end = at;
    let mut same_line = true;
    loop {
        let ls = line_start(end);
        if let Some(t) = line_tag(&text[ls..end]) {
            if !JSDOC_LINK_TAGS.contains(&t) {
                return (SiteKind::Comment, false);
            }
            let after_tag = text[ls..end].rsplit_once(t).map_or("", |(_, r)| r);
            return (SiteKind::JsDoc, same_line && after_tag.trim().is_empty());
        }
        if ls == 0 {
            return (SiteKind::Comment, false);
        }
        end = ls - 1;
        same_line = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn scan(src: &str, tsx: bool, name: &str) -> FileScopes {
        let mut parser = Parser::new();
        let lang = if tsx {
            tree_sitter_typescript::LANGUAGE_TSX.into()
        } else {
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
        };
        parser.set_language(&lang).expect("grammar loads");
        let tree = parser.parse(src, None).expect("source parses");
        scope_file(&tree, src, name)
    }

    fn ts(src: &str) -> FileScopes {
        scan(src, false, "O")
    }

    fn kinds(s: &FileScopes) -> Vec<SiteKind> {
        s.sites.iter().map(|x| x.kind).collect()
    }

    #[test]
    fn test_rn1_nested_let_makes_name_unproven() {
        let s = ts("export function O() {}\nfunction f() { let O = 1; return O; }\n");
        assert!(s.declares("O"));
        let refs: Vec<_> = s
            .sites
            .iter()
            .filter(|x| x.kind == SiteKind::Reference)
            .collect();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].binder, Binder::Unknown);
    }

    #[test]
    fn test_rn1_param_named_like_target_is_nested_decl() {
        assert!(ts("function f(O: number) { return O; }").declares("O"));
        assert!(ts("const f = (O) => O;").declares("O"));
        assert!(ts("const f = O => O;").declares("O"));
        assert!(ts("function f({ a: [O] }, ...rest) {}").declares("O"));
    }

    #[test]
    fn test_rn1_top_level_function_alone_is_not_nested() {
        let s = ts("export function O() {}\nO();\n");
        assert!(!s.declares("O"));
        let b = s.module_bindings("O");
        assert_eq!(b.value, 1);
        assert_eq!(b.ty, 0);
        let call = s.sites.iter().find(|x| x.kind == SiteKind::Call);
        assert!(matches!(call.map(|c| c.binder), Some(Binder::Module(_))));
    }

    #[test]
    fn test_rn1_catch_param_is_nested_decl() {
        assert!(ts("try {} catch (O) {}").declares("O"));
    }

    #[test]
    fn test_rn1_type_param_is_nested_decl() {
        assert!(ts("function f<O>(a: O) {}").declares("O"));
        assert!(ts("type T<O> = O[];").declares("O"));
    }

    #[test]
    fn test_rn1_named_fn_expr_is_nested_decl() {
        assert!(ts("const g = function O() {};").declares("O"));
        assert!(ts("const g = class O {};").declares("O"));
    }

    #[test]
    fn test_rn1_switch_body_let_is_nested_decl() {
        assert!(ts("switch (1) { case 1: let O = 1; }").declares("O"));
    }

    #[test]
    fn test_rn1_static_block_decl_is_nested_decl() {
        assert!(ts("class C { static { let O = 1; } }").declares("O"));
    }

    #[test]
    fn test_rn1_enum_member_and_block_decl_are_nested() {
        assert!(ts("enum E { O, P = 2 }").declares("O"));
        assert!(ts("if (x) { const O = 1; }").declares("O"));
        assert!(ts("for (let O = 0;;) {}").declares("O"));
        assert!(ts("for (const O of xs) {}").declares("O"));
    }

    #[test]
    fn test_rn1_import_binding_in_module_scope() {
        let s = ts("import { O } from \"./a\";\nO();\n");
        assert!(!s.declares("O"));
        assert_eq!(s.imports.len(), 1);
        assert_eq!(s.imports[0].source, "./a");
        assert_eq!(s.imports[0].imported.as_deref(), Some("O"));
        assert_eq!(s.module_bindings("O").import, Some(0));
        assert!(s.sites.iter().all(|x| x.binder == Binder::Import(0)));
        assert_eq!(kinds(&s), vec![SiteKind::ImportSpecifier, SiteKind::Call]);
    }

    #[test]
    fn test_rn1_aliased_import_name_binds_the_import() {
        let s = ts("import { O as L } from \"./a\";\nL();\n");
        assert_eq!(s.sites.len(), 1);
        assert_eq!(s.sites[0].kind, SiteKind::ImportSpecifier);
        assert_eq!(s.sites[0].binder, Binder::Import(0));
        assert!(s.module_bindings("O").import.is_none());
        assert_eq!(s.module_bindings("L").import, Some(0));
    }

    #[test]
    fn test_rn1_inline_type_import_is_import_binding() {
        let s = ts("import { type O } from \"./a\";\nlet x: O;\n");
        assert!(s.imports[0].type_only);
        assert_eq!(s.sites.len(), 2);
        assert!(s.sites.iter().all(|x| x.binder == Binder::Import(0)));
        assert_eq!(s.sites[1].kind, SiteKind::TypePosition);
    }

    #[test]
    fn test_rn1_computed_key_is_value_reference() {
        let s = ts("function O() {}\nconst o = { [O]: 1 };\n");
        let site = s.sites.iter().find(|x| x.line == 2);
        let site = site.expect("site on line 2");
        assert_eq!(site.kind, SiteKind::Reference);
        assert!(matches!(site.binder, Binder::Module(_)));
    }

    #[test]
    fn test_rn1_decorator_is_value_reference() {
        let s = ts("function O() {}\n@O class C {}\n");
        let site = s.sites.iter().find(|x| x.line == 2).expect("site");
        assert_eq!(site.kind, SiteKind::Reference);
        assert!(matches!(site.binder, Binder::Module(_)));
    }

    #[test]
    fn test_rn1_declares_counts_all_spaces() {
        let s = ts("function O() {}\ninterface O { a: 1 }\nnamespace O { export const k = 1; }\n");
        let b = s.module_bindings("O");
        assert_eq!(b.value, 2);
        assert_eq!(b.ty, 1);
        assert_eq!(b.namespace, 1);
        let g = ts("function O() {}\ndeclare global { function O(): void; }\n");
        assert_eq!(g.module_bindings("O").value, 2);
        let m = ts("function O() {}\ndeclare module \"x\" { function O(): void; }\n");
        assert_eq!(m.module_bindings("O").value, 2);
        let c = ts("class O {}\ntype X = 1;\n");
        let cb = c.module_bindings("O");
        assert_eq!((cb.value, cb.ty), (1, 1));
    }

    #[test]
    fn test_rn1_label_is_no_binding() {
        let s = ts("O: for (;;) { break O; }\n");
        assert!(!s.declares("O"));
        assert_eq!(kinds(&s), vec![SiteKind::Label, SiteKind::Label]);
        assert!(s.sites.iter().all(|x| x.binder == Binder::NoBinding));
        assert_eq!(s.module_bindings("O"), ModuleBindings::default());
    }

    #[test]
    fn test_rn1_unknown_construct_gives_unknown() {
        let s = ts("function O() {}\nwith (x) { O(); }\n");
        assert!(s.flags.has_with);
        let call = s.sites.iter().find(|x| x.kind == SiteKind::Call);
        assert_eq!(call.map(|c| c.binder), Some(Binder::Unknown));
        let e = ts("function O( { O ( ;;\n");
        assert!(e.flags.has_error);
        assert!(e
            .sites
            .iter()
            .filter(|x| x.kind != SiteKind::Declaration)
            .all(|x| x.binder == Binder::Unknown));
    }

    #[test]
    fn test_rn1_jsx_closing_tag_binds() {
        let s = scan(
            "function O() { return 1; }\nconst a = <O>t</O>;\nconst b = <O />;\n",
            true,
            "O",
        );
        let k = kinds(&s);
        assert_eq!(
            k,
            vec![
                SiteKind::Declaration,
                SiteKind::JsxOpen,
                SiteKind::JsxClose,
                SiteKind::JsxOpen
            ]
        );
        assert!(s
            .sites
            .iter()
            .all(|x| matches!(x.binder, Binder::Module(_))));
    }

    #[test]
    fn test_rn1_shorthand_property_found() {
        let s = ts("function O() {}\nconst o = { O };\n");
        assert!(s
            .sites
            .iter()
            .any(|x| x.kind == SiteKind::ShorthandProperty));
    }

    #[test]
    fn test_rn1_object_key_member_and_string_are_no_binding() {
        let s = ts("const o = { O: 1 }; a.O; x = \"O\"; y = `p O q`; z = \"a O\";\n");
        let k = kinds(&s);
        assert_eq!(
            k,
            vec![
                SiteKind::ObjectKey,
                SiteKind::MemberProperty,
                SiteKind::String,
                SiteKind::Template,
                SiteKind::String
            ]
        );
        assert!(s.sites.iter().all(|x| x.binder == Binder::NoBinding));
        let wholes: Vec<bool> = s.sites.iter().map(|x| x.whole).collect();
        assert_eq!(wholes, vec![false, false, true, false, false]);
    }

    #[test]
    fn test_rn1_jsdoc_link_found() {
        let src = "function O() {}\n/** See {@link O} and O. */\nfunction g() {}\n// O here\n";
        let s = ts(src);
        let ks = kinds(&s);
        assert_eq!(
            ks,
            vec![
                SiteKind::Declaration,
                SiteKind::JsDoc,
                SiteKind::Comment,
                SiteKind::Comment
            ]
        );
        assert!(matches!(s.sites[1].binder, Binder::Module(_)));
        assert_eq!(s.sites[3].binder, Binder::NoBinding);
    }

    #[test]
    fn test_rn1_jsdoc_type_tag_found() {
        let src = "/**\n * @param {O} a\n * @see O\n */\nfunction f(a) {}\n";
        let s = ts(src);
        assert_eq!(kinds(&s), vec![SiteKind::JsDocType, SiteKind::JsDoc]);
    }

    #[test]
    fn test_rn1_import_meta_glob_flag() {
        let s =
            ts("const m = import.meta.glob(\"./x/*.ts\");\nconst n = import.meta.globEager(p);\n");
        let globs: Vec<_> = s
            .flags
            .dynamic
            .iter()
            .filter(|d| d.kind == DynamicKind::MetaGlob)
            .collect();
        assert_eq!(globs.len(), 2);
        assert_eq!(globs[0].literal.as_deref(), Some("./x/*.ts"));
        assert_eq!(globs[1].literal, None);
    }

    #[test]
    fn test_rn1_dynamic_access_flags() {
        let s = ts("import(\"./a\");\nrequire(p);\nrequire.context(\"./z\");\nimport q = require(\"./q\");\nmodule.exports = 1;\nexports.x = 2;\nexport = 3;\neval(\"1\");\n");
        let has = |k| s.flags.dynamic.iter().any(|d| d.kind == k);
        assert!(has(DynamicKind::ImportCall));
        assert!(has(DynamicKind::RequireCall));
        assert!(has(DynamicKind::RequireContext));
        assert!(has(DynamicKind::ImportRequire));
        assert!(s.flags.has_module_exports);
        assert!(s.flags.has_exports_assign);
        assert!(s.flags.has_export_assignment);
        assert!(s.flags.has_direct_eval);
    }

    #[test]
    fn test_rn1_export_star_and_namespace_flags() {
        let s = ts(
            "export * from \"./a\";\nexport * as ns from \"./b\";\nimport * as m from \"./c\";\n",
        );
        assert_eq!(s.flags.export_star, vec!["./a".to_string()]);
        assert_eq!(s.flags.export_namespace, vec!["./b".to_string()]);
        assert_eq!(s.flags.namespace_imports, vec!["./c".to_string()]);
        assert!(s.flags.is_module);
        assert!(!ts("function O() {}").flags.is_module);
    }

    #[test]
    fn test_rn1_export_clause_and_reexport_kinds() {
        let s =
            ts("function O() {}\nexport { O };\nexport { O as P };\nexport { O } from \"./k\";\n");
        assert_eq!(
            kinds(&s),
            vec![
                SiteKind::Declaration,
                SiteKind::ExportClause,
                SiteKind::ExportClause,
                SiteKind::ReExportSpecifier
            ]
        );
    }

    #[test]
    fn test_rn1_utf8_offsets_are_bytes() {
        let src = "const s = \"é€😀\";\nfunction O() {}\nconst t = \"ü\"; O();\n";
        let s = ts(src);
        assert_eq!(s.sites.len(), 2);
        for site in &s.sites {
            assert_eq!(&src[site.start..site.end], "O");
        }
        let call = &s.sites[1];
        assert_eq!(call.start, src.rfind("O()").expect("call"));
        assert_eq!(call.line, 3);
        // "const t = " is 10 bytes, the string "ü" is 4 bytes, then "; ".
        assert_eq!(call.col, 10 + 4 + 2 + 1);
    }

    #[test]
    fn test_rn1_word_test_uses_identifier_parts() {
        let s = ts("// Ox O_ $O O é\nconst Oé = 1; /* O */\n");
        let words: Vec<_> = s.sites.iter().map(|x| x.start).collect();
        assert_eq!(words.len(), 2);
    }

    #[test]
    fn test_rn1_identifier_escape_flag() {
        let s = ts("const \\u0061b = 1;\n");
        assert!(s.flags.has_identifier_escape);
    }

    #[test]
    fn test_rn1_jsdoc_stops_at_first_unmatched_brace() {
        let a = ts("/** if (x) { ... {@link O} */\n");
        assert_eq!(kinds(&a), vec![SiteKind::JsDoc]);
        let b = ts("/**\n * code {\n * @param {O} a */\n");
        assert_eq!(kinds(&b), vec![SiteKind::JsDocType]);
        let c = ts("/** code { O } */\n");
        assert_eq!(kinds(&c), vec![SiteKind::JsDoc]);
        assert_eq!(c.sites[0].binder, Binder::Unknown);
        let d = ts("/** plain O prose */\n");
        assert_eq!(kinds(&d), vec![SiteKind::Comment]);
    }

    #[test]
    fn test_rn1_function_and_type_of_one_name_do_not_cross_spaces() {
        let s = ts("export function O() {}\ntype O = { a: 1 };\nlet x: O;\nO();\n");
        // Two declaring tokens exist, so each declaration site is Unknown.
        assert_eq!(s.sites[0].binder, Binder::Unknown);
        let ty = s
            .sites
            .iter()
            .find(|x| x.kind == SiteKind::TypePosition)
            .expect("type site");
        let call = s
            .sites
            .iter()
            .find(|x| x.kind == SiteKind::Call)
            .expect("call site");
        let (Binder::Module(t), Binder::Module(c)) = (ty.binder, call.binder) else {
            panic!("both sites bind to one declaration in their own space");
        };
        assert_ne!(t, c);
        assert_eq!(
            &"export function O() {}\ntype O = { a: 1 };\n"[t.start..t.end],
            "O"
        );
        assert!(t.start > c.start, "the type site binds to the type alias");
    }

    #[test]
    fn test_rn1_two_imports_of_one_name_are_unknown() {
        let s = ts("import { O } from \"./a\";\nimport { O } from \"./b\";\nO();\n");
        let call = s.sites.iter().find(|x| x.kind == SiteKind::Call);
        assert_eq!(call.map(|c| c.binder), Some(Binder::Unknown));
    }

    #[test]
    fn test_rn1_overloads_are_unknown() {
        let s = ts("function O(a: string): void;\nfunction O(a: any) {}\nO(1);\n");
        let call = s.sites.iter().find(|x| x.kind == SiteKind::Call);
        assert_eq!(call.map(|c| c.binder), Some(Binder::Unknown));
        assert_eq!(s.module_bindings("O").value, 2);
    }

    #[test]
    fn test_rn1_unbound_name_is_global() {
        let s = ts("O();\n");
        assert_eq!(s.sites.len(), 1);
        assert_eq!(s.sites[0].binder, Binder::Global);
    }

    #[test]
    fn test_rn1_legacy_module_counts_as_namespace() {
        let s = ts("module O { export const k = 1; }\n");
        assert_eq!(s.module_bindings("O").namespace, 1);
    }

    #[test]
    fn test_rn1_escaped_string_with_decoded_value_is_whole() {
        let s = ts("a = \"\\x4F\"; b = \"\\u{4F}\"; c = \"\\u004F\"; d = `\\x4F`; e = \"O\";\n");
        assert_eq!(s.sites.len(), 5);
        assert!(s.sites.iter().all(|x| x.whole));
        assert_eq!(s.sites[3].kind, SiteKind::Template);
        let other = ts("a = \"\\x4F x\"; b = `${O}`;\n");
        assert!(other.sites.iter().all(|x| !x.whole));
    }

    #[test]
    fn test_rn1_export_default_is_marked() {
        let s = ts("function O() {}\nexport default O;\n");
        assert_eq!(s.sites[1].kind, SiteKind::ExportDefault);
        let t = ts("function O() {}\nexport { O as default };\n");
        assert_eq!(t.sites[1].kind, SiteKind::ExportDefault);
        let u = ts("function O() {}\nexport { O as \"default\" };\n");
        assert_eq!(u.sites[1].kind, SiteKind::ExportDefault);
        let v = ts("function O() {}\nexport { O as P };\n");
        assert_eq!(v.sites[1].kind, SiteKind::ExportClause);
    }

    #[test]
    fn test_rn1_export_as_namespace_is_marked() {
        let s = ts("export as namespace O;\n");
        assert!(s.flags.has_export_as_namespace);
        assert_eq!(kinds(&s), vec![SiteKind::ExportAsNamespace]);
    }

    #[test]
    fn test_rn1_using_declaration_declares() {
        assert!(ts("function g() { using O = f(); }").declares("O"));
        let top = ts("using O = f();\n");
        assert!(!top.declares("O"));
        assert_eq!(top.module_bindings("O").value, 1);
    }

    #[test]
    fn test_rn1_for_var_in_declares() {
        assert!(ts("for (var O in x) {}").declares("O"));
    }

    #[test]
    fn test_rn1_import_alias_binds_alias_and_references_root() {
        let s = ts("import X = O.y;\n");
        assert_eq!(kinds(&s), vec![SiteKind::Reference]);
        assert_eq!(s.module_bindings("X").value, 1);
        assert_eq!(s.sites[0].binder, Binder::Global);
    }

    #[test]
    fn test_rn1_infer_type_declares() {
        assert!(ts("type U = T extends infer O ? 1 : 2;").declares("O"));
    }

    #[test]
    fn test_rn1_template_literal_type_text_is_template() {
        let s = ts("type T = `x O y${O}`;\n");
        assert_eq!(kinds(&s), vec![SiteKind::Template, SiteKind::TypePosition]);
    }

    #[test]
    fn test_rn1_line_ends_match_ts() {
        let s = ts("O\r// O\u{2028}O\u{2029}O\r\nO\nO");
        let lines: Vec<usize> = s.sites.iter().map(|x| x.line).collect();
        assert_eq!(lines, vec![1, 2, 3, 4, 5, 6]);
        assert!(s.sites.iter().skip(1).all(|x| x.col <= 6));
    }

    #[test]
    fn test_rn1_indirect_require_and_define_property_are_dynamic() {
        let s = ts("(0, require)(p);\nObject.defineProperty(exports, \"x\", {});\n");
        assert!(s
            .flags
            .dynamic
            .iter()
            .any(|d| d.kind == DynamicKind::RequireCall));
        assert!(s.flags.has_exports_assign);
    }

    #[test]
    fn test_rn1_jsdoc_unknown_tag_brace_is_unknown() {
        let s = ts("function O() {}\n/** @example\n foo({ cb: O }) */\n");
        let site = s.sites.iter().find(|x| x.line == 3).expect("site");
        assert_eq!(site.kind, SiteKind::JsDoc);
        assert_eq!(site.binder, Binder::Unknown);
        let known = ts("function O() {}\n/** @param {O} a\n * {@link O} */\n");
        assert!(known.sites[1..]
            .iter()
            .all(|x| matches!(x.binder, Binder::Module(_))));
        let desc = ts("function O() {}\n/** @param {X} a {foo(O)} */\n");
        assert_eq!(desc.sites[1].binder, Binder::Unknown);
    }

    #[test]
    fn test_rn1_see_continuation_line_is_unknown() {
        let s = ts("function O() {}\n/**\n * @see foo\n *   O\n * @see O\n */\n");
        assert_eq!(s.sites[1].kind, SiteKind::JsDoc);
        assert_eq!(s.sites[1].binder, Binder::Unknown);
        assert_eq!(s.sites[2].kind, SiteKind::JsDoc);
        assert!(matches!(s.sites[2].binder, Binder::Module(_)));
    }

    #[test]
    fn test_rn1_export_default_declaration_is_marked() {
        for src in [
            "export default function O() {}",
            "export default async function O() {}",
            "export default function* O() {}",
            "export default class O {}",
            "export default abstract class O {}",
        ] {
            let s = ts(src);
            assert_eq!(kinds(&s), vec![SiteKind::ExportDefault], "{src}");
        }
        let named = ts("export function O() {}");
        assert_eq!(kinds(&named), vec![SiteKind::Declaration]);
    }

    #[test]
    fn test_rn1_export_as_namespace_binder_is_unknown() {
        let s = ts("export as namespace O;\n");
        assert_eq!(s.sites[0].binder, Binder::Unknown);
    }

    #[test]
    fn test_rn1_line_continuation_string_gives_one_site() {
        let s = ts("a = \"O\\\n\";\n");
        assert_eq!(s.sites.len(), 1);
        assert!(s.sites[0].whole);
    }

    #[test]
    fn test_rn1_jsdoc_type_brace_is_proven_only_for_the_bare_name() {
        let binder_of = |ty: &str| {
            let src = format!("function O() {{}}\n/** @param {ty} a */\n");
            let s = ts(&src);
            s.sites
                .iter()
                .find(|x| x.line == 2)
                .map(|x| x.binder)
                .expect("site on line 2")
        };
        for ty in [
            "{{O: string}}",
            "{Foo.O}",
            "{O.Bar}",
            "{import('./a').O}",
            "{O[]}",
            "{Array<O>}",
            "{?O}",
            "{O|null}",
        ] {
            assert_eq!(binder_of(ty), Binder::Unknown, "{ty}");
        }
        for ty in ["{O}", "{ O }"] {
            assert!(matches!(binder_of(ty), Binder::Module(_)), "{ty}");
        }
    }
}
