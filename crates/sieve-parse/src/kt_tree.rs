//! The facts of one Kotlin file and the scope data of each Kotlin call.
//! The facts ride on an `imports` raw edge. The scope data rides on each
//! `calls` raw edge. The Kotlin call proof reads both in a later step. This
//! file only records data and changes no resolution result.
//!
//! A file is `broken` when its tree cannot be trusted: the grammar gave an
//! error node, a `context(` line sits in the file, or a declaration header
//! has no declaration node. A broken file also lists the names that its
//! text declares, because the tree can lose them.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};

use tree_sitter::Node as TsNode;

use crate::extract::{
    KtAlias, KtBody, KtBodyKind, KtCall, KtFileRec, KtFun, KtImport, KtKind, KtProp, KtRec,
    KtScope, KtShape, KtType, RawEdge,
};
use crate::go_tree::text;
use sieve_core::Relation;

/// The modifier words that can stand before a declaration keyword.
const DECL_MODIFIERS: &[&str] = &[
    "abstract",
    "actual",
    "annotation",
    "companion",
    "const",
    "data",
    "enum",
    "expect",
    "external",
    "final",
    "infix",
    "inline",
    "inner",
    "internal",
    "lateinit",
    "open",
    "operator",
    "override",
    "private",
    "protected",
    "public",
    "sealed",
    "suspend",
    "tailrec",
    "value",
];

/// A binder name that a scope node brings, with the reason word.
type Binder = (String, &'static str);

/// The facts and the scope data of one Kotlin file.
pub(crate) struct KtFile {
    rec: KtFileRec,
    /// The node id of each body scope, mapped to its place in `rec.bodies`.
    bodies: HashMap<usize, u32>,
    /// The node ids of the types that sit inside a body.
    local_ids: HashSet<usize>,
    /// The binders of a scope node, by node id.
    binders: RefCell<HashMap<usize, Vec<Binder>>>,
}

impl KtFile {
    /// Reads one Kotlin tree.
    pub fn new(root: TsNode, source: &str, path: &str) -> Self {
        let src = source.as_bytes();
        let mut f = KtFile {
            rec: KtFileRec {
                pkg: String::new(),
                broken: false,
                script: path.to_ascii_lowercase().ends_with(".kts"),
                gradle: false,
                imports: Vec::new(),
                types: Vec::new(),
                funs: Vec::new(),
                props: Vec::new(),
                aliases: Vec::new(),
                bodies: Vec::new(),
                lost: Vec::new(),
            },
            bodies: HashMap::new(),
            local_ids: HashSet::new(),
            binders: RefCell::new(HashMap::new()),
        };
        let mut c = root.walk();
        for ch in root.children(&mut c) {
            match ch.kind() {
                "package_header" => f.rec.pkg = qualified(ch, src),
                "import_list" => {
                    let mut ic = ch.walk();
                    for imp in ch.children(&mut ic) {
                        f.add_import(imp, src);
                    }
                }
                "import_header" => f.add_import(ch, src),
                "function_declaration" => f.rec.funs.extend(fun_of(ch, src)),
                "property_declaration" => f.rec.props.extend(props_of(ch, src)),
                "class_declaration" | "object_declaration" => {
                    f.add_type(ch, None, false, src);
                }
                "type_alias" => f.add_alias(ch, src),
                _ => {}
            }
        }
        f.rec.gradle = f
            .rec
            .imports
            .iter()
            .any(|i| i.path == "org.gradle" || i.path.starts_with("org.gradle."));
        let mut marks = Marks::default();
        f.scan(root, &mut marks, src);
        f.rec.broken = root.has_error() || marks.guard_broken(source);
        if f.rec.broken {
            f.rec.lost = lost_names(source);
        }
        f
    }

    /// The `imports` raw edge that carries the file record.
    pub fn file_edge(&self, path: &str) -> RawEdge {
        edge(
            path,
            Relation::Imports,
            KtRec::File(Box::new(self.rec.clone())),
        )
    }

    /// The record of one call node. `receiver` is the receiver text that the
    /// callee reader found: `this`, `super`, one identifier or nothing.
    pub fn call_rec(
        &self,
        node: TsNode,
        name: &str,
        receiver: Option<&str>,
        source: &str,
    ) -> Option<KtRec> {
        let name = name.trim_matches('`');
        let receiver = receiver.map(|r| r.trim_matches('`'));
        let src = source.as_bytes();
        let (arity, loose) = arguments(node);
        let shape = match receiver {
            None => KtShape::Bare,
            Some("this") => KtShape::This,
            Some("super") => KtShape::Super,
            Some(_) => KtShape::Name,
        };
        let recv = (shape == KtShape::Name).then(|| receiver.unwrap_or_default().to_string());
        let mut stop = label_stop(node, shape);
        let mut ctx = Vec::new();
        let mut up = node.parent();
        while let Some(a) = up {
            if stop.is_none() && shape == KtShape::Bare {
                stop = self.binder_stop(a, name, src);
            }
            if let Some(s) = self.scope_of(a, src) {
                ctx.push(s);
            }
            up = a.parent();
        }
        Some(KtRec::Cross(KtCall {
            shape,
            name: name.to_string(),
            arity,
            loose,
            ctx,
            recv,
            stop,
            line: row(node),
        }))
    }

    fn add_import(&mut self, n: TsNode, src: &[u8]) {
        if n.kind() != "import_header" {
            return;
        }
        let mut imp = KtImport {
            path: String::new(),
            star: false,
            alias: None,
        };
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            match ch.kind() {
                "identifier" => imp.path = compact(ch, src),
                "wildcard_import" => imp.star = true,
                "import_alias" => {
                    imp.alias = ch
                        .named_child(0)
                        .map(|a| ident(a, src))
                        .filter(|a| !a.is_empty());
                }
                _ => {}
            }
        }
        if !imp.path.is_empty() {
            self.rec.imports.push(imp);
        }
    }

    fn add_alias(&mut self, n: TsNode, src: &[u8]) {
        if let Some(name) = name_of(n, src) {
            self.rec.aliases.push(KtAlias { name, line: row(n) });
        }
    }

    /// Reads a class, an interface, an object or a companion with its
    /// members. Returns the type name.
    fn add_type(
        &mut self,
        n: TsNode,
        outer: Option<&str>,
        local: bool,
        src: &[u8],
    ) -> Option<String> {
        let kind = match n.kind() {
            "object_declaration" => KtKind::Object,
            "companion_object" => KtKind::Companion,
            _ if has_token(n, "interface") => KtKind::Interface,
            _ if has_token(n, "enum") => KtKind::Enum,
            _ => KtKind::Class,
        };
        let name = match (name_of(n, src), kind) {
            (Some(name), _) => name,
            (None, KtKind::Companion) => "Companion".to_string(),
            (None, _) => return None,
        };
        let (mut mods, annots) = modifiers(n, src);
        if kind == KtKind::Enum {
            mods.push("enum".to_string());
        }
        if has_token(n, "fun") {
            mods.push("fun".to_string());
        }
        let (bases, by_bases) = supertypes(n, src);
        let at = self.rec.types.len();
        self.rec.types.push(KtType {
            name: name.clone(),
            id: String::new(),
            line: row(n),
            kind,
            outer: outer.map(str::to_string),
            companion: None,
            mods,
            annots,
            bases,
            by_bases,
            tparams: type_params(n, src),
            props: Vec::new(),
            methods: Vec::new(),
            local,
        });
        if local {
            self.local_ids.insert(n.id());
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            match ch.kind() {
                "primary_constructor" => {
                    let props = ctor_props(ch, src);
                    self.rec.types[at].props.extend(props);
                }
                "class_body" | "enum_class_body" => self.add_members(at, ch, src),
                _ => {}
            }
        }
        Some(name)
    }

    fn add_members(&mut self, at: usize, body: TsNode, src: &[u8]) {
        let name = self.rec.types[at].name.clone();
        let local = self.rec.types[at].local;
        let mut c = body.walk();
        for m in body.children(&mut c) {
            match m.kind() {
                "function_declaration" => {
                    let f = fun_of(m, src);
                    self.rec.types[at].methods.extend(f);
                }
                "property_declaration" => {
                    let p = props_of(m, src);
                    self.rec.types[at].props.extend(p);
                }
                "class_declaration" | "object_declaration" => {
                    self.add_type(m, Some(&name), local, src);
                }
                "companion_object" => {
                    let comp = self.add_type(m, Some(&name), local, src);
                    self.rec.types[at].companion = comp;
                }
                _ => {}
            }
        }
    }

    /// Walks the whole tree once. It finds each body, each comment and
    /// string range, and each declaration keyword row.
    fn scan(&mut self, n: TsNode, marks: &mut Marks, src: &[u8]) {
        let kind = n.kind();
        if kind.contains("comment") || kind.contains("string_literal") {
            marks.skip.push((n.start_byte(), n.end_byte()));
            return;
        }
        match kind {
            "class_declaration" | "object_declaration" | "companion_object" => {
                marks.keyword(n, true);
                if self.local_ids.contains(&n.id()) {
                    // An outer local type already read this one.
                } else if kind != "companion_object" && self.is_local(n) {
                    self.add_local(n, src);
                } else if let Some(outer) = self.body_holder(n, src) {
                    self.add_type(n, outer.as_deref(), true, src);
                }
            }
            "object_literal" => {
                marks.keyword(
                    n,
                    n.parent().is_some_and(|p| p.kind() != "infix_expression"),
                );
                self.add_anon(n, src);
            }
            "enum_entry" => self.add_entry(n, src),
            _ => {}
        }
        if matches!(kind, "fun" | "val" | "var")
            && n.parent().is_some_and(|p| {
                matches!(
                    p.kind(),
                    "function_declaration"
                        | "anonymous_function"
                        | "binding_pattern_kind"
                        | "when_subject"
                        | "class_declaration"
                )
            })
        {
            marks.member_rows.insert(n.start_position().row);
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            self.scan(ch, marks, src);
        }
    }

    /// A class or an object that sits in a statement list of a function.
    fn is_local(&self, n: TsNode) -> bool {
        let Some(p) = n.parent().filter(|p| p.kind() == "statements") else {
            return false;
        };
        !p.parent()
            .is_some_and(|l| self.bodies.contains_key(&l.id()))
    }

    /// Whether a type declaration is a member of an anonymous object, a
    /// local class or an enum entry body. Gives the name of the holder, if
    /// the holder is a named class.
    fn body_holder(&self, n: TsNode, src: &[u8]) -> Option<Option<String>> {
        let p = n.parent()?;
        match p.kind() {
            "class_body" | "enum_class_body" => {
                let h = p.parent()?;
                if self.bodies.contains_key(&p.id()) {
                    Some(None)
                } else if self.bodies.contains_key(&h.id()) {
                    Some(name_of(h, src))
                } else {
                    None
                }
            }
            "statements" => self.bodies.contains_key(&p.parent()?.id()).then_some(None),
            _ => None,
        }
    }

    fn add_local(&mut self, n: TsNode, src: &[u8]) {
        let (bases, _) = supertypes(n, src);
        let body = child(n, "class_body");
        self.add_body(
            n.id(),
            KtBodyKind::Local,
            name_of(n, src).unwrap_or_default(),
            n,
            bases,
            body,
            src,
        );
    }

    fn add_anon(&mut self, n: TsNode, src: &[u8]) {
        let (bases, _) = supertypes(n, src);
        // The grammar can read the body of a one-line object as a lambda
        // that follows the object in the same call.
        let body = child(n, "class_body").or_else(|| {
            let call = n.parent().filter(|p| p.kind() == "call_expression")?;
            let suffix = child(call, "call_suffix")?;
            child(child(suffix, "annotated_lambda")?, "lambda_literal")
        });
        if let Some(body) = body {
            self.add_body(
                body.id(),
                KtBodyKind::Anon,
                String::new(),
                n,
                bases,
                Some(body),
                src,
            );
        }
    }

    fn add_entry(&mut self, n: TsNode, src: &[u8]) {
        let Some(body) = child(n, "class_body") else {
            return;
        };
        let owner = n
            .parent()
            .and_then(|p| p.parent())
            .and_then(|e| name_of(e, src));
        self.add_body(
            body.id(),
            KtBodyKind::Entry,
            name_of(n, src).unwrap_or_default(),
            n,
            owner.into_iter().collect(),
            Some(body),
            src,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn add_body(
        &mut self,
        id: usize,
        kind: KtBodyKind,
        name: String,
        at: TsNode,
        bases: Vec<String>,
        body: Option<TsNode>,
        src: &[u8],
    ) {
        let idx = self.rec.bodies.len() as u32;
        self.bodies.insert(id, idx);
        self.rec.bodies.push(KtBody {
            kind,
            name,
            line: row(at),
            bases,
            overrides: body.map(|b| override_names(b, src)).unwrap_or_default(),
        });
    }

    /// The scope that a node adds around the calls below it.
    fn scope_of(&self, a: TsNode, src: &[u8]) -> Option<KtScope> {
        if let Some(&idx) = self.bodies.get(&a.id()) {
            return Some(KtScope::Body { idx });
        }
        let ext = || child(a, "receiver_type").map(|r| written(r, src));
        let named = |default: &str| name_of(a, src).unwrap_or_else(|| default.to_string());
        match a.kind() {
            "function_declaration" => Some(KtScope::Fun {
                name: child(a, "simple_identifier")
                    .map(|n| ident(n, src))
                    .unwrap_or_default(),
                ext: ext(),
            }),
            "anonymous_function" => Some(KtScope::Fun {
                name: String::new(),
                ext: ext(),
            }),
            "secondary_constructor" => Some(KtScope::Fun {
                name: "<init>".to_string(),
                ext: None,
            }),
            "property_declaration" => Some(KtScope::Fun {
                name: props_of(a, src)?.name,
                ext: Some(written(child(a, "receiver_type")?, src)),
            }),
            "class_declaration" => Some(KtScope::Class {
                name: named(""),
                line: row(a),
            }),
            "object_declaration" => Some(KtScope::Object {
                name: named(""),
                line: row(a),
            }),
            "companion_object" => Some(KtScope::Companion {
                name: named("Companion"),
                line: row(a),
            }),
            "lambda_literal" => {
                let (call, recv) = lambda_call(a, src);
                let mut bound = false;
                let mut up = a.parent();
                while let Some(p) = up.filter(|_| !call.is_empty() && !recv && !bound) {
                    bound = self.binder_stop(p, &call, src).is_some();
                    up = p.parent();
                }
                Some(KtScope::Lambda { call, recv, bound })
            }
            _ => None,
        }
    }

    /// The reason word when the scope node `a` brings a binder `name`.
    fn binder_stop(&self, a: TsNode, name: &str, src: &[u8]) -> Option<String> {
        if !matches!(
            a.kind(),
            "function_declaration"
                | "anonymous_function"
                | "secondary_constructor"
                | "lambda_literal"
                | "setter"
                | "for_statement"
                | "catch_block"
                | "when_expression"
                | "statements"
                | "class_declaration"
        ) {
            return None;
        }
        let mut cache = self.binders.borrow_mut();
        let list = cache.entry(a.id()).or_insert_with(|| binders_of(a, src));
        list.iter()
            .find(|(n, _)| n == name)
            .map(|(_, reason)| (*reason).to_string())
    }
}

/// The names that a scope node binds, each with the reason word.
fn binders_of(a: TsNode, src: &[u8]) -> Vec<Binder> {
    let mut out: Vec<Binder> = Vec::new();
    match a.kind() {
        "function_declaration" | "anonymous_function" | "secondary_constructor" => {
            let reason = if a.kind() == "anonymous_function" {
                "anon_param"
            } else {
                "param"
            };
            if let Some(list) = child(a, "function_value_parameters") {
                let mut c = list.walk();
                for p in list.children(&mut c).filter(|p| p.kind() == "parameter") {
                    if let Some(n) = child(p, "simple_identifier") {
                        out.push((ident(n, src), reason));
                    }
                }
            }
        }
        "lambda_literal" => match child(a, "lambda_parameters") {
            Some(list) => {
                let mut c = list.walk();
                for d in list.children(&mut c) {
                    declare(&mut out, d, "lambda_param", src);
                }
            }
            None => out.push(("it".to_string(), "it")),
        },
        "for_statement" => {
            let mut c = a.walk();
            for d in a.children(&mut c) {
                declare(&mut out, d, "for", src);
            }
        }
        "setter" => {
            if let Some(n) =
                child(a, "parameter_with_optional_type").and_then(|p| child(p, "simple_identifier"))
            {
                out.push((ident(n, src), "setter_param"));
            }
        }
        "catch_block" => {
            if let Some(n) = child(a, "simple_identifier") {
                out.push((ident(n, src), "catch"));
            }
        }
        "when_expression" => {
            if let Some(d) = child(a, "when_subject").and_then(|s| child(s, "variable_declaration"))
            {
                declare(&mut out, d, "when", src);
            }
        }
        "statements" => {
            let mut c = a.walk();
            for s in a.children(&mut c) {
                match s.kind() {
                    "property_declaration" => {
                        let mut pc = s.walk();
                        for d in s.children(&mut pc) {
                            declare(&mut out, d, "local", src);
                        }
                    }
                    kind
                    @ ("function_declaration" | "class_declaration" | "object_declaration") => {
                        if let Some(n) = name_of(s, src)
                            .or_else(|| child(s, "simple_identifier").map(|n| ident(n, src)))
                        {
                            let reason = if kind == "function_declaration" {
                                "local_fun"
                            } else {
                                "local_class"
                            };
                            out.push((n, reason));
                        }
                    }
                    _ => {}
                }
            }
        }
        "class_declaration" => {
            if let Some(ctor) = child(a, "primary_constructor") {
                let mut c = ctor.walk();
                for p in ctor
                    .children(&mut c)
                    .filter(|p| p.kind() == "class_parameter")
                {
                    if let Some(n) = child(p, "simple_identifier") {
                        out.push((ident(n, src), "ctor_param"));
                    }
                }
            }
        }
        _ => {}
    }
    out
}

/// Adds the names that a variable declaration or a destructuring binds.
fn declare(out: &mut Vec<Binder>, d: TsNode, reason: &'static str, src: &[u8]) {
    match d.kind() {
        "variable_declaration" => {
            if let Some(n) = child(d, "simple_identifier") {
                out.push((ident(n, src), reason));
            }
        }
        "multi_variable_declaration" => {
            let mut c = d.walk();
            for v in d.children(&mut c) {
                if let Some(n) = child(v, "simple_identifier") {
                    out.push((ident(n, src), "destructure"));
                }
            }
        }
        _ => {}
    }
}

/// The name of the call that gets a lambda, and whether that call has a
/// receiver. A lambda that is no argument gives an empty name.
fn lambda_call(lambda: TsNode, src: &[u8]) -> (String, bool) {
    let mut up = lambda.parent();
    while let Some(p) = up {
        match p.kind() {
            "annotated_lambda" | "call_suffix" | "value_argument" | "value_arguments" => {
                up = p.parent();
            }
            "call_expression" => return callee_of(p, src),
            _ => break,
        }
    }
    (String::new(), false)
}

/// The called name of a `call_expression` and whether it has a receiver.
fn callee_of(call: TsNode, src: &[u8]) -> (String, bool) {
    match call.named_child(0) {
        Some(f) if f.kind() == "simple_identifier" => (ident(f, src), false),
        Some(f) if f.kind() == "navigation_expression" => {
            let mut c = f.walk();
            let name = f
                .children(&mut c)
                .filter(|k| k.kind() == "navigation_suffix")
                .last()
                .and_then(|s| child(s, "simple_identifier"))
                .map(|n| ident(n, src))
                .unwrap_or_default();
            (name, true)
        }
        _ => (String::new(), false),
    }
}

/// The count of value arguments (a trailing lambda adds 1) and whether a
/// named or a spread argument is present.
fn arguments(call: TsNode) -> (u32, bool) {
    let (mut n, mut loose) = (0, false);
    let Some(suffix) = child(call, "call_suffix") else {
        return (n, loose);
    };
    let mut c = suffix.walk();
    for part in suffix.children(&mut c) {
        match part.kind() {
            "annotated_lambda" => n += 1,
            "value_arguments" => {
                let mut ac = part.walk();
                for a in part
                    .children(&mut ac)
                    .filter(|a| a.kind() == "value_argument")
                {
                    n += 1;
                    loose |= has_token(a, "=") || child(a, "spread_expression").is_some();
                }
            }
            _ => {}
        }
    }
    (n, loose)
}

/// A label on `this`, or a type on `super`, stops the proof.
fn label_stop(call: TsNode, shape: KtShape) -> Option<String> {
    let reason = match shape {
        KtShape::This => "label",
        KtShape::Super => "super_typed",
        _ => return None,
    };
    let recv = call.named_child(0)?.named_child(0)?;
    (recv.named_child_count() > 0).then(|| reason.to_string())
}

/// What one walk over the tree learns for the `broken` test.
#[derive(Default)]
struct Marks {
    /// The byte range of each comment and string, in tree order.
    skip: Vec<(usize, usize)>,
    /// The line (from 0) of each type keyword in the tree.
    keyword_rows: HashSet<usize>,
    /// The line (from 0) of each `fun`, `val` and `var` of a declaration.
    member_rows: HashSet<usize>,
}

impl Marks {
    /// Notes the lines of the `class`, `interface` and `object` keywords of
    /// a declaration node, if `count` is true.
    fn keyword(&mut self, n: TsNode, count: bool) {
        if !count {
            return;
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            if matches!(ch.kind(), "class" | "interface" | "object") {
                self.keyword_rows.insert(ch.start_position().row);
            }
        }
    }

    /// Whether the byte `at` lies inside a comment or a string that starts
    /// before it.
    fn inside(&self, at: usize) -> bool {
        let i = self.skip.partition_point(|r| r.0 < at);
        i > 0 && self.skip[i - 1].1 > at
    }

    /// A `context(` line, or a line that starts a declaration while no
    /// declaration node has its keyword on that line. The guard checks one
    /// line at a time: two declarations on one line count as one.
    fn guard_broken(&self, source: &str) -> bool {
        let mut offset = 0;
        for (row, line) in source.split_inclusive('\n').enumerate() {
            let start = offset;
            offset += line.len();
            let mut trimmed = line.trim_start();
            if trimmed.is_empty() || self.inside(start + line.len() - trimmed.len()) {
                continue;
            }
            // Skip a block comment that opens and closes on this line.
            while let Some(after) = trimmed.strip_prefix("/*") {
                let Some(end) = after.find("*/") else {
                    break;
                };
                trimmed = after[end + 2..].trim_start();
            }
            let missing = match header_line(trimmed) {
                Some(Head::Type) => !self.keyword_rows.contains(&row),
                Some(Head::Member) => !self.member_rows.contains(&row),
                None => false,
            };
            if context_before_decl(trimmed) || missing {
                return true;
            }
        }
        false
    }
}

/// Whether a `context(` token stands before the declaration word, behind
/// annotations and modifiers only.
fn context_before_decl(line: &str) -> bool {
    let Some(at) = line.find("context(") else {
        return false;
    };
    line[..at]
        .split_whitespace()
        .all(|w| w.starts_with('@') || DECL_MODIFIERS.contains(&w))
}

/// The leading word of `s` and the rest after it.
fn word(s: &str) -> (&str, &str) {
    let end = s
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(s.len());
    s.split_at(end)
}

/// The kind of declaration that a line starts.
#[derive(Clone, Copy)]
enum Head {
    /// `class`, `object`, `interface`, `enum class` or `fun interface`.
    Type,
    /// `fun`, `val` or `var`.
    Member,
}

/// The declaration that the first token of a line starts, after modifiers
/// and annotations.
fn header_line(line: &str) -> Option<Head> {
    let mut rest = line;
    loop {
        rest = rest.trim_start();
        if let Some(after) = rest.strip_prefix('@') {
            rest = skip_annotation(after);
            continue;
        }
        let (w, tail) = word(rest);
        if DECL_MODIFIERS.contains(&w) {
            rest = tail;
            continue;
        }
        let next = word(tail.trim_start()).0;
        return match w {
            "class" | "object" | "interface" => Some(Head::Type),
            "fun" if next == "interface" => Some(Head::Type),
            "fun" | "val" | "var" => Some(Head::Member),
            _ => None,
        };
    }
}

/// The text after an annotation: its name, a use-site target and a
/// parenthesized argument list on the same line.
fn skip_annotation(s: &str) -> &str {
    let end = s
        .find(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | ':' | '.')))
        .unwrap_or(s.len());
    let rest = &s[end..];
    if !rest.starts_with('(') {
        return rest;
    }
    let mut depth = 0;
    for (i, c) in rest.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return &rest[i + 1..];
                }
            }
            _ => {}
        }
    }
    ""
}

/// One token of the declaration scan.
struct Tok<'a> {
    text: &'a str,
    ident: bool,
}

/// Splits source text into identifiers, backtick names and single marks.
/// `->` stays one token, so a function type does not close a `<..>` list.
fn tokens(src: &str) -> Vec<Tok<'_>> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let start = i;
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c == b'`' {
            let end = src[i + 1..].find('`').map_or(b.len(), |e| i + 1 + e);
            out.push(Tok {
                text: &src[i + 1..end],
                ident: true,
            });
            i = end + 1;
        } else if c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80 {
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] >= 0x80) {
                i += 1;
            }
            out.push(Tok {
                text: &src[start..i],
                ident: !c.is_ascii_digit(),
            });
        } else {
            i += if c == b'-' && b.get(i + 1) == Some(&b'>') {
                2
            } else {
                1
            };
            out.push(Tok {
                text: &src[start..i],
                ident: false,
            });
        }
    }
    out
}

/// The index after the `<..>` list that starts at `at`.
fn skip_angle(toks: &[Tok], at: usize) -> usize {
    let mut depth = 0;
    for (i, t) in toks.iter().enumerate().skip(at) {
        match t.text {
            "<" => depth += 1,
            ">" => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
    }
    toks.len()
}

/// The index after the `(..)` group that starts at `at`.
fn skip_paren(toks: &[Tok], at: usize) -> usize {
    let mut depth = 0;
    for (i, t) in toks.iter().enumerate().skip(at) {
        match t.text {
            "(" => depth += 1,
            ")" => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
    }
    toks.len()
}

/// The last name of a dotted chain that starts at `at`, after it skips a
/// `<..>` list, a `?` and a parenthesized receiver type. With `call` set,
/// the name must stand before a `(`.
fn chain_name(toks: &[Tok], mut at: usize, call: bool) -> Option<String> {
    let mut last = None;
    loop {
        let t = toks.get(at)?;
        if t.text == "(" && last.is_none() {
            at = skip_paren(toks, at);
        } else if t.ident {
            last = Some(t.text.to_string());
            at += 1;
        } else {
            return None;
        }
        if toks.get(at).is_some_and(|t| t.text == "<") {
            at = skip_angle(toks, at);
        }
        while toks.get(at).is_some_and(|t| t.text == "?") {
            at += 1;
        }
        if toks.get(at).is_some_and(|t| t.text == ".") {
            at += 1;
            last = None;
        } else {
            break;
        }
    }
    if call && toks.get(at).is_none_or(|t| t.text != "(") {
        return None;
    }
    last
}

/// The names in a destructuring list that starts at the `(` at `at`.
fn destructured(toks: &[Tok], at: usize) -> Vec<String> {
    let end = skip_paren(toks, at);
    let mut out = Vec::new();
    for i in at + 1..end {
        let after_mark = matches!(toks[i - 1].text, "(" | ",");
        if after_mark && toks[i].ident && toks[i].text != "_" {
            out.push(toks[i].text.to_string());
        }
    }
    out
}

/// Each name that the text declares after `fun`, `class`, `object`,
/// `interface`, `val` and `var`. The text gives them when the tree loses
/// them.
fn lost_names(src: &str) -> Vec<String> {
    let toks = tokens(src);
    let mut names = BTreeSet::new();
    for (i, t) in toks.iter().enumerate() {
        let name = match t.text {
            "class" | "object" | "interface" | "typealias" => toks
                .get(i + 1)
                .filter(|n| n.ident)
                .map(|n| n.text.to_string()),
            "val" | "var" if toks.get(i + 1).is_some_and(|n| n.text == "(") => {
                names.extend(destructured(&toks, i + 1));
                None
            }
            "val" | "var" | "fun" => {
                let mut at = i + 1;
                if toks.get(at).is_some_and(|n| n.text == "<") {
                    at = skip_angle(&toks, at);
                }
                chain_name(&toks, at, t.text == "fun")
            }
            _ => None,
        };
        names.extend(name);
    }
    names.into_iter().collect()
}

/// The text of an identifier node without the backticks that quote it.
fn ident(n: TsNode, src: &[u8]) -> String {
    text(n, src).trim_matches('`').to_string()
}

/// The line of a node, counted from 1.
fn row(n: TsNode) -> u32 {
    n.start_position().row as u32 + 1
}

/// The first child of a node with the given kind.
fn child<'a>(n: TsNode<'a>, kind: &str) -> Option<TsNode<'a>> {
    let mut c = n.walk();
    let found = n.children(&mut c).find(|k| k.kind() == kind);
    found
}

/// Whether a node has a direct child token of the given kind.
fn has_token(n: TsNode, kind: &str) -> bool {
    child(n, kind).is_some()
}

/// The text of a node without white space.
fn compact(n: TsNode, src: &[u8]) -> String {
    text(n, src).split_whitespace().collect()
}

/// The dotted name in the `identifier` child of a node.
fn qualified(n: TsNode, src: &[u8]) -> String {
    child(n, "identifier")
        .map(|i| compact(i, src))
        .unwrap_or_default()
}

/// The name of a declaration: its first direct identifier child.
fn name_of(n: TsNode, src: &[u8]) -> Option<String> {
    let mut c = n.walk();
    let found = n
        .children(&mut c)
        .find(|k| matches!(k.kind(), "type_identifier" | "simple_identifier"))
        .map(|k| ident(k, src));
    found
}

/// The last simple name of a written type. A nullable type gives the name
/// of its base. A function type gives `None`.
fn simple_type(n: TsNode, src: &[u8]) -> Option<String> {
    match n.kind() {
        "user_type" => {
            // A qualified type (`Outer.Inner`) has no simple name: the
            // proof cannot tell it from a type of the same last name.
            let mut c = n.walk();
            let mut ids = n.children(&mut c).filter(|k| k.kind() == "type_identifier");
            let first = ids.next()?;
            ids.next().is_none().then(|| ident(first, src))
        }
        "nullable_type" | "parenthesized_type" | "type_reference" | "not_nullable_type"
        | "receiver_type" => simple_type(n.named_child(0)?, src),
        _ => None,
    }
}

/// A written type by simple name, or `?` when it has none.
fn written(n: TsNode, src: &[u8]) -> String {
    simple_type(n, src).unwrap_or_else(|| "?".to_string())
}

/// The modifier words and the annotation names of a declaration.
fn modifiers(n: TsNode, src: &[u8]) -> (Vec<String>, Vec<String>) {
    let (mut words, mut annots) = (Vec::new(), Vec::new());
    let mut add = |m: TsNode| {
        if m.kind() == "annotation" {
            let name =
                first_of(m, "user_type").map_or_else(|| "?".to_string(), |u| written(u, src));
            annots.push(name);
        } else {
            words.extend(text(m, src).split_whitespace().map(str::to_string));
        }
    };
    let mut c = n.walk();
    for ch in n.children(&mut c) {
        match ch.kind() {
            "modifiers" => {
                let mut mc = ch.walk();
                ch.children(&mut mc).for_each(&mut add);
            }
            "annotation" => add(ch),
            _ => {}
        }
    }
    (words, annots)
}

/// The first node of a kind in a subtree, in tree order.
fn first_of<'a>(n: TsNode<'a>, kind: &str) -> Option<TsNode<'a>> {
    if n.kind() == kind {
        return Some(n);
    }
    let mut c = n.walk();
    let found = n.children(&mut c).find_map(|k| first_of(k, kind));
    found
}

/// The written supertypes of a type or an object, and the ones that use
/// `by`.
fn supertypes(n: TsNode, src: &[u8]) -> (Vec<String>, Vec<String>) {
    let (mut bases, mut by) = (Vec::new(), Vec::new());
    let mut c = n.walk();
    for spec in n
        .children(&mut c)
        .filter(|k| k.kind() == "delegation_specifier")
    {
        let Some(first) = spec.named_child(0) else {
            continue;
        };
        match first.kind() {
            "constructor_invocation" => {
                bases.push(
                    first
                        .named_child(0)
                        .map_or_else(|| "?".to_string(), |t| written(t, src)),
                );
            }
            "explicit_delegation" => {
                let t = first
                    .named_child(0)
                    .map_or_else(|| "?".to_string(), |t| written(t, src));
                by.push(t.clone());
                bases.push(t);
            }
            _ => bases.push(written(first, src)),
        }
    }
    (bases, by)
}

/// The names in the `type_parameters` list of a declaration.
fn type_params(n: TsNode, src: &[u8]) -> Vec<String> {
    let Some(list) = child(n, "type_parameters") else {
        return Vec::new();
    };
    let mut c = list.walk();
    list.children(&mut c)
        .filter(|k| k.kind() == "type_parameter")
        .filter_map(|k| name_of(k, src))
        .collect()
}

/// The count of parameters, the count with a default value, and whether
/// one parameter is `vararg`.
fn params(list: TsNode, src: &[u8]) -> (u32, u32, bool) {
    let (mut n, mut d, mut vararg) = (0, 0, false);
    let (mut pending, mut has_default) = (false, false);
    let mut c = list.walk();
    for p in list.children(&mut c) {
        match p.kind() {
            "parameter_modifiers" => {
                pending |= text(p, src).split_whitespace().any(|w| w == "vararg");
            }
            "parameter" => {
                n += 1;
                has_default = has_token(p, "=");
                d += u32::from(has_default);
                vararg |= pending;
                pending = false;
            }
            // The grammar can put the `=` of a default value beside the
            // parameter and not inside it.
            "=" if !has_default => {
                d += 1;
                has_default = true;
            }
            _ => {}
        }
    }
    (n, d, vararg)
}

/// One function or method.
fn fun_of(n: TsNode, src: &[u8]) -> Option<KtFun> {
    let name = child(n, "simple_identifier").map(|k| ident(k, src))?;
    let (count, defaults, vararg) = child(n, "function_value_parameters")
        .map(|l| params(l, src))
        .unwrap_or_default();
    Some(KtFun {
        name,
        id: String::new(),
        line: row(n),
        mods: modifiers(n, src).0,
        ext_recv: child(n, "receiver_type").map(|r| written(r, src)),
        n: count,
        d: defaults,
        vararg,
    })
}

/// The property that a `property_declaration` makes. A destructuring makes
/// none.
fn props_of(n: TsNode, src: &[u8]) -> Option<KtProp> {
    let decl = child(n, "variable_declaration")?;
    let name = child(decl, "simple_identifier").map(|k| ident(k, src))?;
    let mut c = decl.walk();
    let ty = decl.children(&mut c).find_map(|k| simple_type(k, src));
    Some(KtProp {
        name,
        line: row(n),
        mods: modifiers(n, src).0,
        ty,
        ext_recv: child(n, "receiver_type").map(|r| written(r, src)),
    })
}

/// The `val` and `var` parameters of a primary constructor.
fn ctor_props(ctor: TsNode, src: &[u8]) -> Vec<KtProp> {
    let mut c = ctor.walk();
    ctor.children(&mut c)
        .filter(|p| p.kind() == "class_parameter" && has_token(*p, "binding_pattern_kind"))
        .filter_map(|p| {
            let name = child(p, "simple_identifier").map(|k| ident(k, src))?;
            let mut pc = p.walk();
            let ty = p.children(&mut pc).find_map(|k| simple_type(k, src));
            Some(KtProp {
                name,
                line: row(p),
                mods: modifiers(p, src).0,
                ty,
                ext_recv: None,
            })
        })
        .collect()
}

/// The names that the direct members of a body declare with `override`.
fn override_names(body: TsNode, src: &[u8]) -> Vec<String> {
    let list = match body.kind() {
        "lambda_literal" => child(body, "statements"),
        _ => Some(body),
    };
    let Some(list) = list else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    let mut c = list.walk();
    for m in list.children(&mut c) {
        let name = match m.kind() {
            "function_declaration" => fun_of(m, src).map(|f| (f.mods, f.name)),
            "property_declaration" => props_of(m, src).map(|p| (p.mods, p.name)),
            _ => None,
        };
        if let Some((mods, name)) = name {
            if mods.iter().any(|w| w == "override") && !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out
}

fn edge(path: &str, relation: Relation, rec: KtRec) -> RawEdge {
    RawEdge {
        source: path.to_string(),
        relation,
        file: path.to_string(),
        target_id: None,
        specifier: None,
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
        rust: None,
        py: None,
        go: None,
        java: None,
        cs: None,
        kt: Some(rec),
    }
}

#[cfg(test)]
#[path = "kt_tree_tests.rs"]
mod tests;
