//! The package-level facts of one Go file (GOX1). The same-file proof in
//! `go_proof.rs` reads these facts. The file holds the names it binds at
//! package level, its funcs, its types and its methods. A dot import, a
//! `//go:build ignore` line or a parse error stops every proof of the file.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use tree_sitter::Node as TsNode;

use crate::extract::{GoFileRec, GoTy, GoTypeRec, RawEdge};
use sieve_core::Relation;

/// The text of `n`, or an empty string when the bytes are not UTF-8.
pub(crate) fn text<'a>(n: TsNode, src: &'a [u8]) -> &'a str {
    src.get(n.byte_range())
        .and_then(|b| std::str::from_utf8(b).ok())
        .unwrap_or("")
}

/// Reports whether the go tool skips the file `path` (GO1): a dir named
/// `testdata`, a dir or file name that starts with `_` or `.`.
pub(crate) fn go_skipped(path: &str) -> bool {
    path.split('/')
        .any(|seg| seg == "testdata" || seg.starts_with(['_', '.']))
}

/// The type name a type node writes: `T` or `*T`. Any other form gives
/// `None`, so a generic, qualified or double pointer type stops the proof.
pub(crate) fn written_type(n: TsNode, src: &[u8]) -> Option<String> {
    match n.kind() {
        "type_identifier" => Some(text(n, src).to_string()),
        "pointer_type" => {
            let inner = n.named_child(0)?;
            (inner.kind() == "type_identifier").then(|| text(inner, src).to_string())
        }
        _ => None,
    }
}

/// The type a node writes: `T`, `*T`, `p.T` or `*p.T`. Any other form
/// (a generic, a double pointer, a map) gives `None`.
pub(crate) fn written_ty(n: TsNode, src: &[u8]) -> Option<GoTy> {
    match n.kind() {
        "type_identifier" => Some(GoTy {
            qual: None,
            name: text(n, src).to_string(),
        }),
        "qualified_type" => Some(GoTy {
            qual: Some(text(n.child_by_field_name("package")?, src).to_string()),
            name: text(n.child_by_field_name("name")?, src).to_string(),
        }),
        "pointer_type" => {
            let inner = n.named_child(0)?;
            (inner.kind() != "pointer_type")
                .then(|| written_ty(inner, src))
                .flatten()
        }
        _ => None,
    }
}

/// One package-level func of the file.
pub(crate) struct FuncInfo {
    /// The 1-based start line of the declaration.
    pub line: u32,
    /// The one written result type, when the func has exactly one.
    pub result: Option<String>,
}

/// One package-level type of the file.
pub(crate) struct TypeInfo {
    /// True for a defined, non-generic, non-interface type.
    pub plain: bool,
    /// The field names of a struct, embedded types included.
    pub fields: HashSet<String>,
}

/// The package-level facts of one Go file.
#[derive(Default)]
pub struct GoFile {
    /// Each package-level name with its count of bindings in this file.
    pub(crate) names: HashMap<String, u32>,
    pub(crate) funcs: HashMap<String, FuncInfo>,
    pub(crate) types: HashMap<String, TypeInfo>,
    /// The start lines of the methods of `(type, method)` in this file.
    pub(crate) methods: HashMap<(String, String), Vec<u32>>,
    /// A dot import, a build `ignore` line or a parse error.
    pub(crate) stop: bool,
    /// The import aliases of the file. They bind in the file scope.
    pub(crate) aliases: HashSet<String>,
    /// The name in the package clause.
    pkg: String,
    /// The file has a dot import.
    dot: bool,
    /// The file has a parse error.
    broken: bool,
    /// The file starts with the build line `ignore` and nothing else.
    ignore: bool,
    /// Each import as `(alias, path)`.
    imports: Vec<(Option<String>, String)>,
    /// The binder table of each top-level declaration, by start byte.
    pub(crate) binder_cache: RefCell<HashMap<usize, crate::go_proof::BinderTable>>,
}

impl GoFile {
    /// Reads the package-level facts of the file under `root`.
    pub fn new(root: TsNode, source: &str) -> GoFile {
        let src = source.as_bytes();
        let mut f = GoFile {
            stop: root.has_error(),
            broken: root.has_error(),
            ..GoFile::default()
        };
        let mut build_rows: Vec<usize> = Vec::new();
        let mut cursor = root.walk();
        for child in root.named_children(&mut cursor) {
            match child.kind() {
                "comment" => {
                    let t = text(child, src);
                    let build = t.starts_with("//go:build") || t.starts_with("// +build");
                    if build && t.split_whitespace().any(|w| w == "ignore") {
                        f.stop = true;
                    }
                    if matches!(t.trim(), "//go:build ignore" | "// +build ignore") {
                        build_rows.push(child.end_position().row);
                    }
                }
                "package_clause" => {
                    // The go tool reads a build line only when a blank line
                    // separates it from the package clause.
                    let pkg_row = child.start_position().row;
                    let lines: Vec<&str> = source.lines().collect();
                    f.ignore = build_rows.iter().any(|r| {
                        lines
                            .get(r + 1..pkg_row)
                            .is_some_and(|between| between.iter().any(|l| l.trim().is_empty()))
                    });
                    build_rows.clear();
                    let mut pc = child.walk();
                    let id = child
                        .named_children(&mut pc)
                        .find(|c| c.kind() == "package_identifier");
                    f.pkg = id.map(|i| text(i, src).to_string()).unwrap_or_default();
                }
                "function_declaration" => f.add_func(child, src),
                "method_declaration" => f.add_method(child, src),
                "var_declaration" | "const_declaration" => f.add_specs(child, src),
                "type_declaration" => f.add_types(child, src),
                "import_declaration" => f.add_imports(child, src),
                _ => {}
            }
        }
        f
    }

    /// Reports whether the file binds `name` exactly once at package level
    /// and no import alias hides it.
    pub(crate) fn bound_once(&self, name: &str) -> bool {
        self.names.get(name) == Some(&1) && !self.aliases.contains(name)
    }

    /// The raw edge that carries the package-level facts of this file. The
    /// resolver reads it and never turns it into a graph edge.
    pub fn file_edge(&self, path: &str) -> RawEdge {
        let mut names: Vec<(String, u32)> =
            self.names.iter().map(|(k, v)| (k.clone(), *v)).collect();
        names.sort();
        let mut funcs: Vec<(String, Option<String>)> = self
            .funcs
            .iter()
            .map(|(k, v)| (k.clone(), v.result.clone()))
            .collect();
        funcs.sort();
        let mut types: Vec<GoTypeRec> = self
            .types
            .iter()
            .map(|(k, v)| {
                let mut fields: Vec<String> = v.fields.iter().cloned().collect();
                fields.sort();
                GoTypeRec {
                    name: k.clone(),
                    plain: v.plain,
                    fields,
                }
            })
            .collect();
        types.sort_by(|a, b| a.name.cmp(&b.name));
        let mut methods: Vec<(String, String, u32)> = self
            .methods
            .iter()
            .map(|((t, m), lines)| (t.clone(), m.clone(), lines.len() as u32))
            .collect();
        methods.sort();
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
            rust: None,
            py: None,
            go: Some(crate::extract::GoRec::File(Box::new(GoFileRec {
                pkg: self.pkg.clone(),
                ignore: self.ignore,
                broken: self.broken,
                dot: self.dot,
                imports: self.imports.clone(),
                names,
                funcs,
                types,
                methods,
            }))),
            java: None,
        }
    }

    fn bind(&mut self, name: &str) {
        *self.names.entry(name.to_string()).or_default() += 1;
    }

    fn add_func(&mut self, n: TsNode, src: &[u8]) {
        let Some(name) = n.child_by_field_name("name") else {
            return;
        };
        let name = text(name, src).to_string();
        self.bind(&name);
        let info = FuncInfo {
            line: line_of(n),
            result: one_result(n, src),
        };
        self.funcs.insert(name, info);
    }

    fn add_method(&mut self, n: TsNode, src: &[u8]) {
        let (Some(name), Some(recv)) = (
            n.child_by_field_name("name"),
            n.child_by_field_name("receiver"),
        ) else {
            return;
        };
        let mut c = recv.walk();
        let ty = recv
            .named_children(&mut c)
            .find(|p| p.kind() == "parameter_declaration")
            .and_then(|p| p.child_by_field_name("type"))
            .and_then(|t| written_type(t, src));
        if let Some(ty) = ty {
            self.methods
                .entry((ty, text(name, src).to_string()))
                .or_default()
                .push(line_of(n));
        }
    }

    /// Binds the names of each `var_spec` and `const_spec` under `n`.
    fn add_specs(&mut self, n: TsNode, src: &[u8]) {
        let mut stack = vec![n];
        while let Some(x) = stack.pop() {
            if matches!(x.kind(), "var_spec" | "const_spec") {
                let mut c = x.walk();
                let names: Vec<String> = x
                    .children_by_field_name("name", &mut c)
                    .filter(|i| i.kind() == "identifier")
                    .map(|i| text(i, src).to_string())
                    .collect();
                for name in names {
                    self.bind(&name);
                }
            } else {
                let mut c = x.walk();
                stack.extend(x.named_children(&mut c));
            }
        }
    }

    fn add_types(&mut self, n: TsNode, src: &[u8]) {
        let mut c = n.walk();
        for spec in n.named_children(&mut c) {
            let Some(name) = spec.child_by_field_name("name") else {
                continue;
            };
            let name = text(name, src).to_string();
            self.bind(&name);
            let ty = spec.child_by_field_name("type");
            let mut sc = spec.walk();
            let alias =
                spec.kind() == "type_alias" || spec.children(&mut sc).any(|k| k.kind() == "=");
            let plain = spec.kind() == "type_spec"
                && !alias
                && spec.child_by_field_name("type_parameters").is_none()
                && ty.is_some_and(|t| t.kind() != "interface_type");
            let mut fields = HashSet::new();
            if let Some(t) = ty.filter(|t| t.kind() == "struct_type") {
                collect_fields(t, src, &mut fields);
            }
            self.types.insert(name, TypeInfo { plain, fields });
        }
    }

    fn add_imports(&mut self, n: TsNode, src: &[u8]) {
        let mut stack = vec![n];
        while let Some(x) = stack.pop() {
            if x.kind() == "import_spec" {
                let path = x
                    .child_by_field_name("path")
                    .map(|p| text(p, src).trim_matches(['"', '`']).to_string());
                let alias = x.child_by_field_name("name");
                match alias {
                    Some(a) if a.kind() == "dot" => {
                        self.stop = true;
                        self.dot = true;
                    }
                    Some(a) if a.kind() == "package_identifier" => {
                        let name = text(a, src).to_string();
                        self.aliases.insert(name);
                    }
                    _ => {}
                }
                if let Some(path) = path {
                    self.imports
                        .push((alias.map(|a| text(a, src).to_string()), path));
                }
            } else {
                let mut c = x.walk();
                stack.extend(x.named_children(&mut c));
            }
        }
    }
}

/// The 1-based start line of `n`.
pub(crate) fn line_of(n: TsNode) -> u32 {
    n.start_position().row as u32 + 1
}

/// Every identifier below a `struct_type`, as a field name. Embedded type
/// names count too, so a name clash with a method stops the proof.
fn collect_fields(t: TsNode, src: &[u8], out: &mut HashSet<String>) {
    let mut stack = vec![t];
    while let Some(x) = stack.pop() {
        if matches!(x.kind(), "field_identifier" | "type_identifier") {
            out.insert(text(x, src).to_string());
        }
        let mut c = x.walk();
        stack.extend(x.named_children(&mut c));
    }
}

/// The one written result type of a func, or `None`.
fn one_result(f: TsNode, src: &[u8]) -> Option<String> {
    if f.child_by_field_name("type_parameters").is_some() {
        return None;
    }
    let r = f.child_by_field_name("result")?;
    if r.kind() != "parameter_list" {
        return written_type(r, src);
    }
    let mut c = r.walk();
    let mut it = r.named_children(&mut c);
    let p = it.next()?;
    if it.next().is_some() || p.kind() != "parameter_declaration" {
        return None;
    }
    let mut pc = p.walk();
    if p.children_by_field_name("name", &mut pc).count() > 1 {
        return None;
    }
    written_type(p.child_by_field_name("type")?, src)
}
