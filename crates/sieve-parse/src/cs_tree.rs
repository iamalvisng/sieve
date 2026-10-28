//! The type facts of one C# file (the C# call proof). The facts
//! feed the call proof in `cs_proof.rs` and the index in `cs_index.rs`.
//! Every type declaration gets a `CsType`, nested types included. A type
//! below a conditional region is still read: the grammar keeps both
//! branches, so a twin declaration shows as a duplicate name.

use std::cell::RefCell;
use std::collections::HashMap;

use tree_sitter::Node as TsNode;

use crate::extract::{CsFileRec, CsKind, CsMethod, CsRec, CsType, CsUsing, RawEdge};
use crate::go_tree::text;
use sieve_core::Relation;

/// The node kinds of a type declaration.
pub(crate) const TYPE_KINDS: &[&str] = &[
    "class_declaration",
    "struct_declaration",
    "interface_declaration",
    "enum_declaration",
    "record_declaration",
    "delegate_declaration",
];

/// The modifiers of a declaration.
#[derive(Default)]
struct Mods {
    public: bool,
    private: bool,
    protected: bool,
    internal: bool,
    stat: bool,
    abs: bool,
    sealed: bool,
    virt: bool,
    ovr: bool,
    partial: bool,
    file: bool,
}

/// Reads the modifier keywords of `n`.
fn modifiers(n: TsNode, src: &[u8]) -> Mods {
    let mut m = Mods::default();
    let mut c = n.walk();
    for k in n.children(&mut c).filter(|k| k.kind() == "modifier") {
        match text(k, src) {
            "public" => m.public = true,
            "private" => m.private = true,
            "protected" => m.protected = true,
            "internal" => m.internal = true,
            "static" => m.stat = true,
            "abstract" => m.abs = true,
            "sealed" => m.sealed = true,
            "virtual" => m.virt = true,
            "override" => m.ovr = true,
            "partial" => m.partial = true,
            "file" => m.file = true,
            _ => {}
        }
    }
    m
}

impl Mods {
    /// The accessibility code, or `default` when no keyword names one.
    fn vis(&self, default: u8) -> u8 {
        match (self.public, self.protected, self.internal, self.private) {
            (true, ..) => 5,
            (_, true, true, _) => 4,
            (_, true, _, true) => 1,
            (_, true, ..) => 2,
            (_, _, true, _) => 3,
            (.., true) => 0,
            _ => default,
        }
    }
}

/// The simple type name that a type node writes: `T` or `T<..>`. A
/// qualified, nullable, array, predefined or pointer type gives `None`.
pub(crate) fn written_simple(n: TsNode, src: &[u8]) -> Option<String> {
    match n.kind() {
        "identifier" => Some(text(n, src).to_string()),
        "generic_name" => {
            let first = n.named_child(0)?;
            (first.kind() == "identifier").then(|| text(first, src).to_string())
        }
        _ => None,
    }
}

/// A written type with generics and spaces dropped. A qualified name keeps
/// its dots.
fn super_text(n: TsNode, src: &[u8]) -> String {
    let mut depth = 0u32;
    let mut out = String::new();
    for ch in text(n, src).chars() {
        match ch {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            c if depth == 0 && !c.is_whitespace() => out.push(c),
            _ => {}
        }
    }
    out
}

/// The text of `n` with no white space.
fn compact(n: TsNode, src: &[u8]) -> String {
    text(n, src).split_whitespace().collect()
}

/// The last segment of a dotted name.
pub(crate) fn last_segment(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

/// The names of the type parameters of a declaration.
pub(crate) fn type_params(n: TsNode, src: &[u8]) -> Vec<String> {
    let mut c = n.walk();
    let list = n
        .children(&mut c)
        .find(|k| k.kind() == "type_parameter_list")
        .or_else(|| n.child_by_field_name("type_parameters"));
    let Some(list) = list else {
        return Vec::new();
    };
    let mut lc = list.walk();
    list.named_children(&mut lc)
        .filter_map(|p| p.child_by_field_name("name"))
        .map(|i| text(i, src).to_string())
        .collect()
}

/// What the text before a `this` modifier shows.
#[derive(Debug, PartialEq, Eq)]
enum Sig<'a> {
    /// A method signature with this name.
    Name(&'a str),
    /// A parenthesis that no method name precedes, such as `(this is T)`.
    Other,
    /// The text has no shape the scan can read.
    Unknown,
}

/// The method whose parameter list holds the `this` modifier at `toks[at]`.
fn method_name_before<'a>(toks: &[&'a str], at: usize) -> Sig<'a> {
    let name_like = |w: &str| w.starts_with(|c: char| c.is_alphabetic() || c == '_' || c == '@');
    // Skip the attribute groups that sit before `this`.
    let mut i = at;
    loop {
        let Some(prev) = i.checked_sub(1) else {
            return Sig::Unknown;
        };
        i = prev;
        match toks[i] {
            "(" => break,
            "]" => {
                let mut depth = 0u32;
                loop {
                    match toks[i] {
                        "]" => depth += 1,
                        "[" => depth -= 1,
                        _ => {}
                    }
                    if depth == 0 {
                        break;
                    }
                    let Some(prev) = i.checked_sub(1) else {
                        return Sig::Unknown;
                    };
                    i = prev;
                }
            }
            _ => {}
        }
        if at - i > 60 {
            return Sig::Unknown;
        }
    }
    let Some(mut j) = i.checked_sub(1) else {
        return Sig::Other;
    };
    if toks[j] == ">" {
        // A generic method: skip the type parameter list.
        let mut depth = 0u32;
        loop {
            match toks[j] {
                ">" => depth += 1,
                "<" => depth -= 1,
                _ => {}
            }
            if depth == 0 {
                break;
            }
            let Some(prev) = j.checked_sub(1) else {
                return Sig::Unknown;
            };
            j = prev;
        }
        let Some(prev) = j.checked_sub(1) else {
            return Sig::Unknown;
        };
        j = prev;
    }
    if name_like(toks[j]) {
        Sig::Name(toks[j])
    } else {
        Sig::Other
    }
}

/// Reports whether the text has a C# 14 `extension` block.
fn has_extension_block(src: &str) -> bool {
    let toks = tokens(src);
    toks.windows(2)
        .any(|w| w[0] == "extension" && matches!(w[1], "(" | "<"))
}

/// The words and signs of `src`. Comments, strings and character literals
/// give no token. A word holds letters, digits, `_` and `@`.
fn tokens(src: &str) -> Vec<&str> {
    let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'@' || c >= 0x80;
    let b = src.as_bytes();
    let at = |i: usize| b.get(i).copied();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'/' && at(i + 1) == Some(b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if c == b'/' && at(i + 1) == Some(b'*') {
            i += 2;
            while i < b.len() && !(b[i] == b'*' && at(i + 1) == Some(b'/')) {
                i += 1;
            }
            i += 2;
        } else if c == b'@' && at(i + 1) == Some(b'"') {
            // A verbatim string: `""` is a quote inside it.
            i += 2;
            while i < b.len() {
                if b[i] == b'"' && at(i + 1) == Some(b'"') {
                    i += 2;
                } else if b[i] == b'"' {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
        } else if c == b'"' {
            let mut quotes = 0;
            while at(i + quotes) == Some(b'"') {
                quotes += 1;
            }
            if quotes >= 3 {
                // A raw string ends at the same count of quotes.
                i += quotes;
                while i < b.len() && !b[i..].starts_with(&b"\"".repeat(quotes)) {
                    i += 1;
                }
                i += quotes;
            } else {
                i += 1;
                while i < b.len() && b[i] != b'"' && b[i] != b'\n' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
            }
        } else if c == b'\'' {
            i += 1;
            while i < b.len() && b[i] != b'\'' && b[i] != b'\n' {
                i += if b[i] == b'\\' { 2 } else { 1 };
            }
            i += 1;
        } else if word(c) {
            let s = i;
            while i < b.len() && word(b[i]) {
                i += 1;
            }
            out.push(&src[s..i]);
        } else {
            if !c.is_ascii_whitespace() {
                out.push(&src[i..=i]);
            }
            i += 1;
        }
    }
    out
}

/// The members of a body, with the contents of conditional regions
/// flattened into the list.
fn flat_members<'a>(body: TsNode<'a>, out: &mut Vec<TsNode<'a>>) {
    let mut c = body.walk();
    for ch in body.named_children(&mut c) {
        if matches!(ch.kind(), "preproc_if" | "preproc_else" | "preproc_elif") {
            flat_members(ch, out);
        } else {
            out.push(ch);
        }
    }
}

/// The type facts of one C# file.
pub struct CsFile {
    pub(crate) rec: CsFileRec,
    /// A type declaration (tree-sitter node id) to its index in `rec`.
    pub(crate) by_node: HashMap<usize, u32>,
    /// The facts of each scope root, by node id. The proof reads this
    /// cache once per call.
    pub(crate) scopes: RefCell<HashMap<usize, std::rc::Rc<crate::cs_proof::Scope>>>,
    /// The node id of a declaration, by start and end byte.
    ids: HashMap<(usize, usize), String>,
    /// The count of `#if` regions around the node that the walk reads.
    if_depth: u32,
}

impl CsFile {
    /// Reads the type facts of the file under `root`. `defs` is the list
    /// of minted definitions as `(id, start byte, end byte)`.
    pub fn new(root: TsNode, source: &str, defs: &[(String, usize, usize)]) -> CsFile {
        let src = source.as_bytes();
        let mut f = CsFile {
            rec: CsFileRec {
                broken: root.has_error(),
                inner_using: false,
                extern_alias: false,
                dynamic: false,
                lost_ext: false,
                lost_ext_names: Vec::new(),
                lost_overrides: Vec::new(),
                lost_types: Vec::new(),
                usings: Vec::new(),
                types: Vec::new(),
                proj: None,
            },
            by_node: HashMap::new(),
            scopes: RefCell::new(HashMap::new()),
            ids: defs
                .iter()
                .map(|(id, s, e)| ((*s, *e), id.clone()))
                .collect(),
            if_depth: 0,
        };
        f.dynamic(root, source);
        f.unit(root, "", false, src);
        if f.rec.broken {
            f.read_text(source);
        } else if source.contains("extension") {
            // A C# 14 `extension` block can parse with no error, as a
            // constructor-like member that hides its methods.
            f.rec.lost_ext = has_extension_block(source);
        }
        f
    }

    /// The tree of a file with a parse error can lose a member or a
    /// whole declaration. The text still shows each extension method, each
    /// `override` and each type name, so the facts come from the text.
    fn read_text(&mut self, source: &str) {
        self.rec.lost_ext |= has_extension_block(source);
        let toks = tokens(source);
        for (i, t) in toks.iter().enumerate() {
            let prev = i.checked_sub(1).map(|p| toks[p]);
            let next = toks.get(i + 1).copied();
            let wordy = |w: Option<&str>| {
                w.is_some_and(|w| {
                    w.starts_with(|c: char| c.is_alphabetic() || c == '_' || c == '@')
                })
            };
            match *t {
                "this"
                    if matches!(prev, Some("(" | "," | "]" | "ref" | "in" | "scoped"))
                        && wordy(next) =>
                {
                    match method_name_before(&toks, i) {
                        Sig::Name(name) => self.rec.lost_ext_names.push(name.to_string()),
                        Sig::Other => {}
                        Sig::Unknown => self.rec.lost_ext = true,
                    }
                }
                "override" => {
                    // The return type can hold `<..>` and a tuple. The
                    // method name is the word before the first `(` that
                    // follows a word or a `>`.
                    let mut last: Option<&str> = None;
                    for w in toks[i + 1..].iter().take(60) {
                        if matches!(*w, "{" | ";" | "=") {
                            break;
                        }
                        if *w == "(" && (wordy(last) || last == Some(">")) {
                            break;
                        }
                        if wordy(Some(w)) {
                            self.rec.lost_overrides.push((*w).to_string());
                        }
                        last = Some(w);
                    }
                }
                // A `class` or `struct` after `:` or `,` is a type constraint.
                "class" | "struct" | "interface" | "record" | "enum"
                    if !matches!(prev, Some(":" | ",")) =>
                {
                    // The name follows, after the `class` of a `record class`.
                    let name = toks[i + 1..]
                        .iter()
                        .take(2)
                        .find(|w| wordy(Some(w)) && !matches!(**w, "class" | "struct"));
                    self.rec.lost_types.extend(name.map(|w| (*w).to_string()));
                }
                "delegate" => {
                    // The name is the last word before the parameter list.
                    let name = toks[i + 1..]
                        .iter()
                        .take(12)
                        .take_while(|w| !matches!(**w, "(" | "{" | ";"))
                        .filter(|w| wordy(Some(w)))
                        .last();
                    self.rec.lost_types.extend(name.map(|w| (*w).to_string()));
                }
                _ => {}
            }
        }
    }

    /// Sets the `dynamic` flag when an identifier reads `dynamic`.
    fn dynamic(&mut self, root: TsNode, source: &str) {
        let src = source.as_bytes();
        if !source.contains("dynamic") {
            return;
        }
        let mut stack = vec![root];
        while let Some(n) = stack.pop() {
            if n.kind() == "identifier" && text(n, src) == "dynamic" {
                self.rec.dynamic = true;
                return;
            }
            let mut c = n.walk();
            stack.extend(n.named_children(&mut c));
        }
    }

    /// The `imports` raw edge that carries the file record.
    pub fn file_edge(&self, path: &str) -> RawEdge {
        RawEdge {
            source: path.to_string(),
            relation: Relation::Imports,
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
            by_value: false,
            rust: None,
            py: None,
            go: None,
            java: None,
            cs: Some(CsRec::File(Box::new(self.rec.clone()))),
            kt: None,
        }
    }

    /// Reads the children of a compilation unit or a namespace body.
    /// `inner` is true inside a namespace block. A file-scoped namespace
    /// sets the namespace of the siblings that follow it.
    fn unit(&mut self, parent: TsNode, ns: &str, inner: bool, src: &[u8]) {
        let mut ns = ns.to_string();
        let mut c = parent.walk();
        for ch in parent.named_children(&mut c) {
            match ch.kind() {
                "using_directive" => self.add_using(ch, inner, src),
                "extern_alias_directive" => self.rec.extern_alias = true,
                "file_scoped_namespace_declaration" => {
                    ns = namespace_name(ch, "", src);
                }
                "namespace_declaration" => {
                    let inner_ns = namespace_name(ch, &ns, src);
                    if let Some(body) = ch.child_by_field_name("body") {
                        self.unit(body, &inner_ns, true, src);
                    }
                }
                "preproc_if" | "preproc_else" | "preproc_elif" => {
                    self.if_depth += 1;
                    self.unit(ch, &ns, inner, src);
                    self.if_depth -= 1;
                }
                k if TYPE_KINDS.contains(&k) => self.add_type(ch, None, &ns, src),
                _ => {}
            }
        }
    }

    fn add_using(&mut self, n: TsNode, inner: bool, src: &[u8]) {
        if inner {
            self.rec.inner_using = true;
        }
        let mut u = CsUsing {
            path: String::new(),
            alias: n
                .child_by_field_name("name")
                .map(|a| text(a, src).to_string()),
            stat: false,
            global: false,
            cond: self.if_depth > 0,
        };
        let mut c = n.walk();
        for k in n.children(&mut c) {
            match k.kind() {
                "static" => u.stat = true,
                "global" => u.global = true,
                _ => {}
            }
        }
        // The path is the target. For an alias it is the right-hand side.
        let name_id = n.child_by_field_name("name").map(|a| a.id());
        let mut c2 = n.walk();
        let path = n.named_children(&mut c2).find(|k| {
            Some(k.id()) != name_id
                && matches!(
                    k.kind(),
                    "identifier" | "qualified_name" | "generic_name" | "alias_qualified_name"
                )
        });
        u.path = path.map(|p| compact(p, src)).unwrap_or_default();
        self.rec.usings.push(u);
    }

    /// The node id of the definition that spans exactly `n`.
    fn id_of(&self, n: TsNode) -> String {
        self.ids
            .get(&(n.start_byte(), n.end_byte()))
            .cloned()
            .unwrap_or_default()
    }

    /// Records a type declaration and its members.
    fn add_type(&mut self, n: TsNode, outer: Option<u32>, ns: &str, src: &[u8]) {
        let Some(name) = n.child_by_field_name("name").map(|x| text(x, src)) else {
            return;
        };
        let name = name.to_string();
        let mods = modifiers(n, src);
        let is_struct_record = n.kind() == "record_declaration" && {
            let mut c = n.walk();
            let found = n.children(&mut c).any(|k| k.kind() == "struct");
            found
        };
        let kind = match n.kind() {
            "struct_declaration" => CsKind::Struct,
            "interface_declaration" => CsKind::Interface,
            "enum_declaration" => CsKind::Enum,
            "record_declaration" if is_struct_record => CsKind::Struct,
            "record_declaration" => CsKind::Record,
            "delegate_declaration" => CsKind::Delegate,
            _ => CsKind::Class,
        };
        let mut bases = Vec::new();
        let mut pctor = Vec::new();
        let mut c = n.walk();
        for ch in n.named_children(&mut c) {
            match ch.kind() {
                "base_list" => {
                    let mut bc = ch.walk();
                    for b in ch.named_children(&mut bc) {
                        match b.kind() {
                            "argument_list" => {}
                            "primary_constructor_base_type" => {
                                if let Some(t) = b.child_by_field_name("type") {
                                    bases.push(super_text(t, src));
                                }
                            }
                            _ => bases.push(super_text(b, src)),
                        }
                    }
                }
                "parameter_list" if n.kind() != "delegate_declaration" => {
                    if let Some(pn) = ch.child_by_field_name("name") {
                        pctor.push((text(pn, src).to_string(), None));
                    }
                    let mut pc = ch.walk();
                    for p in ch
                        .named_children(&mut pc)
                        .filter(|p| p.kind() == "parameter")
                    {
                        if let Some(pn) = p.child_by_field_name("name") {
                            let ty = p
                                .child_by_field_name("type")
                                .and_then(|t| written_simple(t, src));
                            pctor.push((text(pn, src).to_string(), ty));
                        }
                    }
                }
                _ => {}
            }
        }
        let default_vis = if outer.is_some() { 0 } else { 3 };
        let idx = self.rec.types.len() as u32;
        let id = if kind == CsKind::Struct && n.kind() == "struct_declaration" {
            String::new()
        } else {
            self.id_of(n)
        };
        let is_record = n.kind() == "record_declaration";
        let others = if is_record {
            pctor.iter().map(|(p, _)| p.clone()).collect()
        } else {
            Vec::new()
        };
        self.rec.types.push(CsType {
            name: name.clone(),
            id,
            kind,
            outer,
            ns: ns.to_string(),
            partial: mods.partial,
            sealed: mods.sealed,
            is_static: mods.stat,
            is_abstract: mods.abs,
            unknown: mods.file,
            vis: mods.vis(default_vis),
            bases,
            tparams: type_params(n, src),
            pctor,
            members: Vec::new(),
            methods: Vec::new(),
            others,
            fields: Vec::new(),
        });
        self.by_node.insert(n.id(), idx);
        if let Some(body) = n.child_by_field_name("body") {
            self.members(body, idx, ns, src);
        }
    }

    /// Reads the members of a type body.
    fn members(&mut self, body: TsNode, idx: u32, ns: &str, src: &[u8]) {
        let mut list = Vec::new();
        flat_members(body, &mut list);
        for m in list {
            match m.kind() {
                k if TYPE_KINDS.contains(&k) => {
                    self.add_type(m, Some(idx), ns, src);
                    if let (Some(name), Some(&mi)) =
                        (m.child_by_field_name("name"), self.by_node.get(&m.id()))
                    {
                        let name = text(name, src).to_string();
                        let t = &mut self.rec.types[idx as usize];
                        t.members.push((name.clone(), mi));
                        t.others.push(name);
                    }
                }
                "method_declaration" => self.add_method(m, idx, src),
                "field_declaration" | "event_field_declaration" => {
                    let mut c = m.walk();
                    let decl = m
                        .named_children(&mut c)
                        .find(|k| k.kind() == "variable_declaration");
                    if let Some(decl) = decl {
                        let mut dc = decl.walk();
                        for d in decl
                            .named_children(&mut dc)
                            .filter(|d| d.kind() == "variable_declarator")
                        {
                            if let Some(name) = d.child_by_field_name("name") {
                                let name = text(name, src).to_string();
                                let t = &mut self.rec.types[idx as usize];
                                if m.kind() == "field_declaration" {
                                    let ty = decl
                                        .child_by_field_name("type")
                                        .and_then(|t| written_simple(t, src));
                                    t.fields.push((name.clone(), ty));
                                }
                                t.others.push(name);
                            }
                        }
                    }
                }
                "property_declaration" | "event_declaration" => {
                    if let Some(name) = m.child_by_field_name("name") {
                        let name = text(name, src).to_string();
                        self.rec.types[idx as usize].others.push(name);
                    }
                }
                "enum_member_declaration" => {
                    if let Some(name) = m.child_by_field_name("name") {
                        let name = text(name, src).to_string();
                        self.rec.types[idx as usize].others.push(name);
                    }
                }
                _ => {}
            }
        }
    }

    fn add_method(&mut self, n: TsNode, idx: u32, src: &[u8]) {
        let mut c = n.walk();
        let explicit = n
            .children(&mut c)
            .any(|k| k.kind() == "explicit_interface_specifier");
        if explicit {
            return;
        }
        let Some(name) = n.child_by_field_name("name") else {
            return;
        };
        let mods = modifiers(n, src);
        let in_iface = self.rec.types[idx as usize].kind == CsKind::Interface;
        let generic = n.child_by_field_name("type_parameters").is_some();
        let (params, loose, is_ext) = n
            .child_by_field_name("parameters")
            .map(|p| parameters(p, generic, src))
            .unwrap_or_default();
        let no_body = n.child_by_field_name("body").is_none();
        let is_abstract = mods.abs || (in_iface && no_body && !mods.stat);
        let m = CsMethod {
            name: text(name, src).to_string(),
            id: self.id_of(n),
            vis: mods.vis(if in_iface { 5 } else { 0 }),
            is_static: mods.stat,
            is_virtual: mods.virt || mods.ovr || is_abstract,
            is_override: mods.ovr,
            is_abstract,
            is_sealed: mods.sealed,
            is_partial: mods.partial,
            is_ext,
            arity: params.len() as u32,
            loose,
            params,
        };
        self.rec.types[idx as usize].methods.push(m);
    }
}

/// The parameters of a method as `(types, loose, extension)`.
fn parameters(list: TsNode, generic: bool, src: &[u8]) -> (Vec<String>, bool, bool) {
    let mut types = Vec::new();
    // The grammar puts a `params` array in the list itself.
    let mut loose = list.child_by_field_name("name").is_some();
    let mut ext = false;
    let mut c = list.walk();
    for (i, p) in list
        .named_children(&mut c)
        .filter(|p| p.kind() == "parameter")
        .enumerate()
    {
        let mut pc = p.walk();
        let mut mode = String::new();
        for k in p.children(&mut pc).filter(|k| k.kind() == "modifier") {
            match text(k, src) {
                "this" if i == 0 => ext = true,
                "params" => loose = true,
                m @ ("ref" | "out" | "in") => mode = format!("{m} "),
                _ => {}
            }
        }
        let has_default = {
            let mut dc = p.walk();
            let found = p.named_children(&mut dc).any(|k| {
                !matches!(k.kind(), "attribute_list" | "modifier")
                    && k.id() != type_id(p)
                    && k.id() != name_id(p)
            });
            found
        };
        loose |= has_default;
        let ty = p.child_by_field_name("type").map(|t| compact(t, src));
        types.push(match ty {
            Some(t) if !generic => format!("{mode}{t}"),
            _ => "?".to_string(),
        });
    }
    if loose {
        // The array parameter is the last one, and its type is unknown.
        types.push("?".to_string());
    }
    (types, loose, ext)
}

/// The node id of the `type` field of a parameter, or 0.
fn type_id(p: TsNode) -> usize {
    p.child_by_field_name("type").map_or(0, |t| t.id())
}

/// The node id of the `name` field of a parameter, or 0.
fn name_id(p: TsNode) -> usize {
    p.child_by_field_name("name").map_or(0, |t| t.id())
}

/// The full dotted name of a namespace declaration below `outer`.
fn namespace_name(n: TsNode, outer: &str, src: &[u8]) -> String {
    let name = n
        .child_by_field_name("name")
        .map(|x| compact(x, src))
        .unwrap_or_default();
    if outer.is_empty() {
        name
    } else {
        format!("{outer}.{name}")
    }
}

/// Reports whether the text of a project file can add usings or extension
/// methods that the index cannot see.
fn risky_manifest(text: &str) -> bool {
    text.contains("<Using ") || text.contains("<Import") || odd_sdk_or_interceptors(text)
}

/// Reports whether the text names interceptors or an `Sdk` other than the
/// plain one.
fn odd_sdk_or_interceptors(text: &str) -> bool {
    if text.contains("InterceptorsNamespaces") {
        return true;
    }
    let mut rest = text;
    while let Some(at) = rest.find("Sdk=\"") {
        rest = &rest[at + 5..];
        let value = rest.split('"').next().unwrap_or("");
        if value != "Microsoft.NET.Sdk" {
            return true;
        }
    }
    text.contains("<Sdk ")
}

/// Reports whether the text of a manifest can change which files a
/// project compiles.
fn changes_file_set(text: &str) -> bool {
    [
        "EnableDefaultItems",
        "DefaultItemExcludes",
        "EnableDefaultCompileItems",
        "<Compile",
    ]
    .iter()
    .any(|k| text.contains(k))
}

/// Reports whether a `<Compile Include>` or `<Compile Remove>` item names
/// a file outside the directory of its manifest. An `Update` item does not
/// add or remove a file.
fn links_outside(text: &str) -> bool {
    let mut rest = text;
    while let Some(at) = rest.find("<Compile") {
        rest = &rest[at + 8..];
        let tag = rest.split('>').next().unwrap_or("");
        for attr in ["Include=", "Remove="] {
            let mut t = tag;
            while let Some(i) = t.find(attr) {
                t = &t[i + attr.len()..];
                let v = t.trim_start_matches(['"', '\'']);
                let v = v.split(['"', '\'']).next().unwrap_or("");
                let outside = ["..", "$(", "@(", "%(", ":"].iter().any(|k| v.contains(k));
                if outside || v.starts_with(['/', '\\']) {
                    return true;
                }
            }
        }
    }
    false
}

/// The path that an import value `v` names, from the directory `dir` of the
/// importing file. `None` when the value is not a plain local path.
fn resolve_import(dir: &str, v: &str) -> Option<String> {
    let v = v.strip_prefix("$(MSBuildThisFileDirectory)").unwrap_or(v);
    if v.contains("$(") || v.contains('*') || v.starts_with(['/', '\\']) {
        return None;
    }
    let mut parts: Vec<&str> = dir.split('/').filter(|d| !d.is_empty()).collect();
    let slashed = v.replace('\\', "/");
    for seg in slashed.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            _ => parts.push(seg),
        }
    }
    Some(parts.join("/"))
}

/// Reports whether a `<Using>` item of the text has an `Alias` or a
/// `Static` attribute.
fn has_alias_using(text: &str) -> bool {
    text.split("<Using").skip(1).any(|r| {
        let tag = r.split('>').next().unwrap_or("");
        tag.contains("Alias") || tag.contains("Static")
    })
}

/// Reports whether the manifest `path` and every local file that it imports
/// leave the file set of a project as the default. Any import that is not a
/// plain local path to a read file makes the answer no. A `<Using>` item
/// does not change the answer, unless it has an `Alias` or a `Static`
/// attribute: an alias hides a type from a using-namespace.
// ponytail: each project reads its imports again, so a repo with many
// projects repeats the reads. Memoize by path if such a repo shows the cost.
fn files_stay_default(
    path: &str,
    all: &HashMap<&str, &str>,
    depth: u32,
    seen: &mut Vec<String>,
) -> bool {
    let Some(text) = all.get(path) else {
        return false;
    };
    if depth > 4 || seen.iter().any(|s| s == path) {
        return false;
    }
    if changes_file_set(text) || odd_sdk_or_interceptors(text) || has_alias_using(text) {
        return false;
    }
    seen.push(path.to_string());
    let dir = path.rsplit_once('/').map_or("", |(d, _)| d);
    let mut rest = *text;
    let mut ok = true;
    while let Some(at) = rest.find("<Import") {
        rest = &rest[at + 7..];
        let tag = rest.split('>').next().unwrap_or("");
        let value = tag
            .split_once("Project=\"")
            .and_then(|(_, r)| r.split('"').next());
        ok = rest.starts_with(char::is_whitespace)
            && !tag.contains("Sdk=")
            && value
                .and_then(|v| resolve_import(dir, v))
                .is_some_and(|target| files_stay_default(&target, all, depth + 1, seen));
        if !ok {
            break;
        }
    }
    seen.pop();
    ok
}

/// Fills the project of each C# file record. `manifests`
/// holds `(path, text)` of each `.csproj`, `Directory.Build.props` and
/// `Directory.Build.targets` file. A file belongs to the project of the
/// nearest `.csproj` directory above it. A `Directory.Build` file above the
/// project that is risky makes the project risky. A directory with 2 or more
/// `.csproj` files is risky. A file with no `.csproj`
/// above it keeps no project. `hidden` is true when the repo has a project
/// file that the walk skipped: then every file is risky.
/// The `exact` flag of a project is true when the project holds exactly the
/// `.cs` files below its directory.
pub fn stamp_projects(manifests: &[(String, String)], hidden: bool, raw: &mut [RawEdge]) {
    let dir_of = |p: &str| p.rsplit_once('/').map_or("", |(d, _)| d).to_string();
    // The directory, the risk and the file-set flag of each manifest.
    let all: HashMap<&str, &str> = manifests
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let facts = |csproj: bool| -> Vec<(String, bool, bool)> {
        manifests
            .iter()
            .filter(|(p, _)| {
                let name = p.rsplit('/').next().unwrap_or("");
                if csproj {
                    name.ends_with(".csproj")
                } else {
                    name.starts_with("Directory.Build")
                }
            })
            .map(|(p, t)| {
                let stay = files_stay_default(p, &all, 0, &mut Vec::new());
                (dir_of(p), risky_manifest(t), stay)
            })
            .collect()
    };
    let projects = facts(true);
    let shared = facts(false);
    let linked = manifests.iter().any(|(_, t)| links_outside(t));
    let above = |dir: &str, file: &str| {
        dir.is_empty() || file.strip_prefix(dir).is_some_and(|r| r.starts_with('/'))
    };
    for e in raw.iter_mut() {
        let Some(CsRec::File(rec)) = &mut e.cs else {
            continue;
        };
        let nearest = projects
            .iter()
            .filter(|(d, _, _)| above(d, &e.file))
            .max_by_key(|(d, _, _)| d.len());
        rec.proj = match nearest {
            Some((dir, risky, stays)) => {
                let inherited = shared.iter().any(|(d, r, _)| *r && above(d, &e.file));
                let twins = projects.iter().filter(|(d, _, _)| d == dir).count() > 1;
                let nested = projects
                    .iter()
                    .any(|(d, _, _)| d != dir && (above(d, dir) || above(dir, d)));
                let shared_moves = shared.iter().any(|(d, _, s)| !*s && above(d, &e.file));
                let risky = *risky || inherited || hidden || twins;
                let exact = !hidden && !twins && *stays && !shared_moves;
                Some(crate::extract::CsProj {
                    dir: dir.clone(),
                    risky,
                    exact: exact && !linked && !nested,
                })
            }
            None => hidden.then(|| crate::extract::CsProj {
                dir: "<none>".to_string(),
                risky: true,
                exact: false,
            }),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn file(source: &str) -> CsFile {
        let mut p = Parser::new();
        p.set_language(&tree_sitter_c_sharp::LANGUAGE.into())
            .expect("c# grammar");
        let tree = p.parse(source, None).expect("parse");
        CsFile::new(tree.root_node(), source, &[])
    }

    #[test]
    fn a_namespace_block_and_a_file_scoped_namespace_set_the_namespace() {
        let f = file(
            "using System;\nglobal using static A.B;\nusing X = A.Foo;\n\
             namespace A.B { using Lib; class Foo {} namespace C { struct S {} } }\n\
             namespace Q;\nclass Top { class In {} }\n",
        );
        let r = &f.rec;
        assert!(r.inner_using);
        assert_eq!(r.usings.len(), 4);
        assert!(r.usings[1].global && r.usings[1].stat);
        assert_eq!(r.usings[2].alias.as_deref(), Some("X"));
        let names: Vec<(&str, &str)> = r
            .types
            .iter()
            .map(|t| (t.ns.as_str(), t.name.as_str()))
            .collect();
        assert_eq!(
            names,
            [("A.B", "Foo"), ("A.B.C", "S"), ("Q", "Top"), ("Q", "In")]
        );
        assert_eq!(r.types[3].outer, Some(2));
    }

    #[test]
    fn a_type_records_its_kind_bases_and_modifiers() {
        let f = file(
            "public sealed partial class A<T> : B<int>, I {}\n\
             struct S : I {}\nrecord R(int P, string Q) : Base(P);\n\
             record struct RS(int X);\ninterface I {}\nenum E { X, Y }\ndelegate void D(int z);\n\
             file class H {}\n",
        );
        let t = &f.rec.types;
        assert_eq!(t[0].bases, ["B", "I"]);
        assert!(t[0].sealed && t[0].partial && t[0].vis == 5);
        assert_eq!(t[0].tparams, ["T"]);
        assert_eq!(t[1].kind, CsKind::Struct);
        assert_eq!(t[2].kind, CsKind::Record);
        assert_eq!(t[2].bases, ["Base"]);
        assert_eq!(t[2].pctor.len(), 2);
        assert_eq!(t[2].others, ["P", "Q"]);
        assert_eq!(t[3].kind, CsKind::Struct);
        assert_eq!(t[4].kind, CsKind::Interface);
        assert_eq!(t[5].kind, CsKind::Enum);
        assert_eq!(t[5].others, ["X", "Y"]);
        assert_eq!(t[6].kind, CsKind::Delegate);
        assert!(t[7].unknown);
    }

    #[test]
    fn a_method_records_its_flags_and_parameters() {
        let f = file(
            "abstract class A {\n\
               public virtual void V(int a, ref string b) {}\n\
               protected abstract void Ab();\n\
               public sealed override void So(int a = 1) {}\n\
               static void St(params int[] xs) {}\n\
               public static void Ex(this A a, int b) {}\n\
               void I.Exp() {}\n\
               void G<T>(T t) {}\n\
               int Fld; public int Prop { get; set; } event System.Action Ev;\n\
             }\n",
        );
        let t = &f.rec.types[0];
        let m = |n: &str| t.methods.iter().find(|x| x.name == n).expect(n);
        assert!(m("V").is_virtual && m("V").vis == 5);
        assert_eq!(m("V").params, ["int", "ref string"]);
        assert!(m("Ab").is_abstract && m("Ab").vis == 2);
        assert!(m("So").is_override && m("So").is_sealed && m("So").loose);
        assert!(m("St").is_static && m("St").loose);
        assert!(m("Ex").is_ext && m("Ex").arity == 2);
        assert!(t.methods.iter().all(|x| x.name != "Exp"));
        assert_eq!(m("G").params, ["?"]);
        assert_eq!(t.others, ["Fld", "Prop", "Ev"]);
    }

    #[test]
    fn a_conditional_region_in_a_body_flattens_into_the_members() {
        let f = file("class A {\n#if X\n void M() {}\n#else\n void M2() {}\n#endif\n}\n");
        let names: Vec<&str> = f.rec.types[0]
            .methods
            .iter()
            .map(|m| m.name.as_str())
            .collect();
        assert_eq!(names, ["M", "M2"]);
    }

    #[test]
    fn a_broken_file_gives_the_facts_of_its_text() {
        let f = file(
            "static class E { public static void M(this Foo f) {} }\n\
             class B : A { public override string ToString() => \"\"; protected abstract override void Go<T>(T t); }\n\
             partial class P { void m( }\n delegate void D(int x);\n",
        );
        assert!(f.rec.broken && !f.rec.lost_ext);
        assert_eq!(f.rec.lost_ext_names, ["M"]);
        assert!(f.rec.lost_overrides.iter().any(|n| n == "ToString"));
        assert!(f.rec.lost_overrides.iter().any(|n| n == "Go"));
        for t in ["E", "B", "P", "D"] {
            assert!(f.rec.lost_types.iter().any(|n| n == t), "{t}");
        }
        let shapes = file(
            "class B : A { public override List<int> Lst() => null; \
             public override (int, int) Tup() => default; \
             static void R(ref this Foo f) {} static void I(in this Bar b) {} void m( }",
        );
        assert!(shapes.rec.lost_overrides.iter().any(|n| n == "Lst"));
        assert!(shapes.rec.lost_overrides.iter().any(|n| n == "Tup"));
        assert!(shapes.rec.lost_ext_names.iter().any(|n| n == "R"));
        assert!(shapes.rec.lost_ext_names.iter().any(|n| n == "I"));
        let clean = file("class A { void M(Foo f) { Go(this, f); } }");
        assert!(!clean.rec.lost_ext && clean.rec.lost_types.is_empty());
        let generic = file("static class E { static void G<T>([A(1)] this Foo<T> f) {} void m( }");
        assert_eq!(generic.rec.lost_ext_names, ["G"]);
        let block = file("static class E { extension(Foo f) { public void M() {} } }");
        assert!(block.rec.lost_ext);
    }

    #[test]
    fn a_project_file_marks_its_files_and_a_risky_one_marks_its_project() {
        let file_edge = |path: &str| {
            let f = file("class A {}");
            let mut e = f.file_edge(path);
            e.file = path.to_string();
            e
        };
        let sdk = "<Project Sdk=\"Microsoft.NET.Sdk\"/>";
        let using = "<Project Sdk=\"Microsoft.NET.Sdk\"><Using Include=\"X\"/></Project>";
        let mut raw = vec![
            file_edge("a/X.cs"),
            file_edge("b/sub/Y.cs"),
            file_edge("c/Z.cs"),
        ];
        let manifests = vec![
            ("a/A.csproj".to_string(), sdk.to_string()),
            ("b/B.csproj".to_string(), using.to_string()),
        ];
        stamp_projects(&manifests, false, &mut raw);
        let proj = |raw: &[RawEdge], i: usize| match &raw[i].cs {
            Some(CsRec::File(r)) => r.proj.clone().map(|p| (p.dir, p.risky)),
            _ => None,
        };
        assert_eq!(proj(&raw, 0), Some(("a".to_string(), false)));
        assert_eq!(proj(&raw, 1), Some(("b".to_string(), true)));
        assert_eq!(proj(&raw, 2), None);
        let web = vec![(
            "w/W.csproj".to_string(),
            "<Project Sdk=\"Microsoft.NET.Sdk.Web\"/>".to_string(),
        )];
        let mut raw2 = vec![file_edge("w/Q.cs")];
        stamp_projects(&web, false, &mut raw2);
        assert_eq!(proj(&raw2, 0), Some(("w".to_string(), true)));
        let shared = vec![
            ("a/A.csproj".to_string(), "<Project/>".to_string()),
            ("Directory.Build.props".to_string(), using.to_string()),
        ];
        let mut raw3 = vec![file_edge("a/X.cs")];
        stamp_projects(&shared, false, &mut raw3);
        let imp = vec![(
            "a/A.csproj".to_string(),
            "<Project><Import Project=\"x.props\"/></Project>".to_string(),
        )];
        let mut raw4 = vec![file_edge("a/X.cs"), file_edge("q/Y.cs")];
        stamp_projects(&imp, false, &mut raw4);
        assert_eq!(proj(&raw4, 0), Some(("a".to_string(), true)));
        assert_eq!(proj(&raw4, 1), None);
        let mut raw5 = vec![file_edge("a/X.cs"), file_edge("q/Y.cs")];
        stamp_projects(&manifests, true, &mut raw5);
        assert_eq!(proj(&raw5, 0), Some(("a".to_string(), true)));
        assert_eq!(proj(&raw5, 1), Some(("<none>".to_string(), true)));
        assert_eq!(proj(&raw3, 0), Some(("a".to_string(), true)));
    }

    #[test]
    fn the_scan_of_a_broken_file_skips_comments_strings_and_constraints() {
        let f = file(
            "// class InComment\nclass A<T> where T : class, IFoo { string s = \"class InString\"; \n\
             char c = 'x'; string v = @\"override \"\"Quoted\"\"\"; bool b = (this is A); void m( }\n",
        );
        assert_eq!(f.rec.lost_types, ["A"]);
        assert!(f.rec.lost_overrides.is_empty());
        assert!(!f.rec.lost_ext && f.rec.lost_ext_names.is_empty());
    }

    #[test]
    fn a_using_item_sets_risky_but_leaves_the_project_exact() {
        let f = file("class A {}");
        let mut e = f.file_edge("b/Y.cs");
        e.file = "b/Y.cs".to_string();
        let using = "<Project Sdk=\"Microsoft.NET.Sdk\"><Using Include=\"X\"/></Project>";
        let manifests = vec![("b/B.csproj".to_string(), using.to_string())];
        let mut raw = vec![e];
        stamp_projects(&manifests, false, &mut raw);
        let Some(CsRec::File(r)) = &raw[0].cs else {
            panic!("no file record");
        };
        let p = r.proj.as_ref().map(|p| (p.risky, p.exact));
        assert_eq!(p, Some((true, true)));
    }

    #[test]
    fn a_using_inside_an_if_region_has_the_cond_flag() {
        let f = file("global using A;\n#if X\nglobal using B;\n#else\nglobal using C;\n#endif\n");
        let flags: Vec<bool> = f.rec.usings.iter().map(|u| u.cond).collect();
        assert_eq!(flags, [false, true, true]);
    }

    #[test]
    fn the_flags_for_a_parse_error_and_for_dynamic() {
        assert!(file("class A { void M() { if ( } ").rec.broken);
        let f = file("class A { dynamic d; void M() { d.X(); } }");
        assert!(f.rec.dynamic && !f.rec.broken);
        assert!(!file("class A { /* dynamic */ }").rec.dynamic);
    }
}
