//! The same-file proof for a Python bare call (a bare
//! call is Extracted only when one binding is proven). The proof holds only when the
//! scope chain of the call binds the name exactly once, and that binding is
//! a `def` or a `class`. Any doubt gives `None`, and the call then takes the
//! name path in `resolve.rs`, capped at Inferred.

use std::collections::{HashMap, HashSet};

use tree_sitter::Node as TsNode;

use sieve_core::Relation;

use crate::extract::{PyBind, PyRec, RawEdge};

/// Decorators that keep a def call target exact, as `(name, module)`. An
/// empty module means a builtin.
const DEF_DECORATORS: &[(&str, &str)] = &[
    ("staticmethod", ""),
    ("classmethod", ""),
    ("cache", "functools"),
    ("lru_cache", "functools"),
    ("contextmanager", "contextlib"),
    ("final", "typing"),
    ("override", "typing"),
];

/// Class decorators that keep a constructor call exact.
const CLASS_DECORATORS: &[(&str, &str)] = &[
    ("dataclass", "dataclasses"),
    ("total_ordering", "functools"),
    ("unique", "enum"),
    ("final", "typing"),
    ("runtime_checkable", "typing"),
];

/// Decorators that may carry call arguments and stay exact.
const CALL_FORMS: &[&str] = &["lru_cache", "dataclass"];

const COMPREHENSIONS: &[&str] = &[
    "list_comprehension",
    "set_comprehension",
    "dictionary_comprehension",
    "generator_expression",
];

/// The def or class behind a single binding.
#[derive(Clone)]
struct DefInfo {
    name: String,
    line: u32,
    ok: bool,
}

/// What one scope knows about one name.
#[derive(Default)]
struct Binding {
    count: u32,
    def: Option<DefInfo>,
}

/// All bindings of one scope.
#[derive(Default)]
struct Table {
    names: HashMap<String, Binding>,
    nonlocal: HashSet<String>,
    globals: HashSet<String>,
    /// A star import: any name may be bound.
    wild: bool,
    /// Module only: `globals()`, `exec(` or a module `__getattr__`.
    stop_all: bool,
    /// A throwaway table that skips the decorator check.
    shallow: bool,
    /// Local name to `(module, member)` for each `from module import member`.
    from_imports: HashMap<String, (String, String)>,
    /// Local name to module for each `import module [as name]`.
    module_imports: HashMap<String, String>,
    /// The full name of each plain `import a.b` without `as`.
    dotted: Vec<String>,
    /// Plain-import locals bound once at the top level. A second plain
    /// import of the same first name is the same binding.
    plain_top: HashSet<String>,
}

impl Table {
    fn add(&mut self, name: &str, weight: u32, def: Option<DefInfo>) {
        let b = self.names.entry(name.to_string()).or_default();
        b.count += weight;
        b.def = if b.count == 1 { def } else { None };
    }
}

/// The scope tables of one Python file, built on first use.
#[derive(Default)]
pub struct PyScopes {
    tables: HashMap<usize, Table>,
}

impl PyScopes {
    fn table(&mut self, scope: TsNode, src: &[u8]) -> &Table {
        self.tables
            .entry(scope.id())
            .or_insert_with(|| build_table(scope, src))
    }

    /// The proof for the call `call` of `name`, or `None` when the scope
    /// chain does not prove one target. A bare call or a dotted call whose
    /// first name is a module-level binding gives `PyRec::Call`, for the
    /// module map to prove.
    pub fn call_rec(&mut self, call: TsNode, name: &str, source: &str) -> Option<PyRec> {
        let src = source.as_bytes();
        let func = call.child_by_field_name("function")?;
        match func.kind() {
            "identifier" => match self.locate(call, name, src) {
                Loc::Def(rec) => Some(rec),
                Loc::Module => Some(PyRec::Call { chain: Vec::new() }),
                Loc::Stop => None,
            },
            "attribute" => {
                if text(func.child_by_field_name("attribute")?, src) != name {
                    return None;
                }
                let chain = dotted_chain(func.child_by_field_name("object")?, src)?;
                match self.locate(call, &chain[0], src) {
                    Loc::Module => Some(PyRec::Call { chain }),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// The `imports` raw edge that carries the module-level facts of the
    /// file under `root`.
    pub fn file_edge(&mut self, root: TsNode, path: &str, source: &str) -> RawEdge {
        let src = source.as_bytes();
        let t = self.table(root, src);
        let mut binds: Vec<(String, PyBind)> = t
            .names
            .iter()
            .map(|(name, b)| (name.clone(), bind_of(name, b, t)))
            .collect();
        binds.sort_by(|a, b| a.0.cmp(&b.0));
        let (patches, sys_path) = scan_file(root, src);
        let rec = PyRec::File {
            binds,
            dotted: t.dotted.clone(),
            patches,
            sys_path,
            broken: root.has_error(),
            opaque: t.stop_all || t.wild,
        };
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
            py: Some(rec),
            go: None,
            java: None,
            cs: None,
        }
    }

    /// Finds the binding of `name` along the scope chain of `call`.
    fn locate(&mut self, call: TsNode, name: &str, src: &[u8]) -> Loc {
        let mut class_visible = true;
        let mut prev = call;
        let mut node = call;
        while let Some(parent) = node.parent() {
            let kind = parent.kind();
            if matches!(
                kind,
                "function_definition" | "class_definition" | "type_alias_statement"
            ) && parent
                .child_by_field_name("type_parameters")
                .is_some_and(|tp| has_ident(tp, name, src))
            {
                return Loc::Stop;
            }
            let inside = match kind {
                "function_definition" | "lambda" | "class_definition" => parent
                    .child_by_field_name("body")
                    .is_some_and(|b| b.id() == node.id()),
                "module" => true,
                k if COMPREHENSIONS.contains(&k) => !is_first_iterable(parent, node, prev),
                _ => false,
            };
            if inside && (kind != "class_definition" || class_visible) {
                let table = self.table(parent, src);
                if table.stop_all || table.wild || table.nonlocal.contains(name) {
                    return Loc::Stop;
                }
                if table.globals.contains(name) && kind != "module" {
                    return self.module_lookup(parent, name, src);
                }
                if let Some(b) = table.names.get(name) {
                    return loc_of(b, kind == "module");
                }
            }
            if inside && kind != "module" {
                class_visible = false;
            }
            prev = node;
            node = parent;
        }
        Loc::Stop
    }

    /// The binding from the module scope above `scope`.
    fn module_lookup(&mut self, scope: TsNode, name: &str, src: &[u8]) -> Loc {
        let mut root = scope;
        while let Some(p) = root.parent() {
            root = p;
        }
        let table = self.table(root, src);
        if table.stop_all || table.wild {
            return Loc::Stop;
        }
        table.names.get(name).map_or(Loc::Stop, |b| loc_of(b, true))
    }
}

/// Where the scope chain of a call found its name.
enum Loc {
    /// One exact same-file def or class.
    Def(PyRec),
    /// Exactly one other module-level binding, for the module map to read.
    Module,
    Stop,
}

fn loc_of(b: &Binding, module_level: bool) -> Loc {
    match proof(b) {
        Some(rec) => Loc::Def(rec),
        None if module_level && b.count == 1 => Loc::Module,
        None => Loc::Stop,
    }
}

/// The identifier chain `a.b.c` of an expression, or `None` for any other
/// expression.
fn dotted_chain(n: TsNode, src: &[u8]) -> Option<Vec<String>> {
    match n.kind() {
        "identifier" => Some(vec![text(n, src).to_string()]),
        "attribute" => {
            let mut chain = dotted_chain(n.child_by_field_name("object")?, src)?;
            chain.push(text(n.child_by_field_name("attribute")?, src).to_string());
            Some(chain)
        }
        _ => None,
    }
}

/// The module-level binding record of one name.
fn bind_of(name: &str, b: &Binding, t: &Table) -> PyBind {
    if b.count != 1 {
        return PyBind::Other;
    }
    if let Some(def) = &b.def {
        return if def.ok {
            PyBind::Def {
                name: def.name.clone(),
                line: def.line,
            }
        } else {
            PyBind::Other
        };
    }
    if let Some((module, member)) = t.from_imports.get(name) {
        return PyBind::From {
            module: module.clone(),
            member: member.clone(),
        };
    }
    if t.dotted.iter().any(|d| d.split('.').next() == Some(name)) {
        return PyBind::Module {
            module: name.to_string(),
            plain: true,
        };
    }
    match t.module_imports.get(name) {
        Some(module) => PyBind::Module {
            module: module.clone(),
            plain: false,
        },
        None => PyBind::Other,
    }
}

/// One pass over a file: every `X.f = ...` target, and whether the file
/// touches `sys.path`.
fn scan_file(root: TsNode, src: &[u8]) -> (Vec<(Vec<String>, String)>, bool) {
    let mut patches = Vec::new();
    let mut sys_path = false;
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "assignment" | "augmented_assignment" => {
                if let Some(left) = n.child_by_field_name("left") {
                    let mut targets = vec![left];
                    while let Some(l) = targets.pop() {
                        if l.kind() == "identifier" && text(l, src) == "__path__" {
                            // handled by the `identifier` arm below
                        } else if l.kind() == "attribute" {
                            let chain = l
                                .child_by_field_name("object")
                                .and_then(|o| dotted_chain(o, src));
                            let attr = l.child_by_field_name("attribute");
                            if let (Some(chain), Some(attr)) = (chain, attr) {
                                patches.push((chain, text(attr, src).to_string()));
                            }
                        } else {
                            let mut cursor = l.walk();
                            targets.extend(l.named_children(&mut cursor));
                        }
                    }
                }
            }
            "attribute" if matches!(text(n, src), "sys.path" | "site.addsitedir") => {
                sys_path = true;
            }
            "identifier" if text(n, src) == "__path__" => {
                // A write to `__path__` changes where submodules come from.
                sys_path = true;
                patches.push((Vec::new(), "*".to_string()));
            }
            "import_from_statement" => {
                let module = n.child_by_field_name("module_name").map(|m| text(m, src));
                let mut cursor = n.walk();
                let names: Vec<&str> = n
                    .children_by_field_name("name", &mut cursor)
                    .filter_map(|c| text(c, src).split_whitespace().next())
                    .collect();
                if (module == Some("sys") && names.contains(&"path"))
                    || (module == Some("site") && names.contains(&"addsitedir"))
                {
                    sys_path = true;
                }
            }
            "delete_statement" => {
                let mut cursor = n.walk();
                let mut items: Vec<TsNode> = n.named_children(&mut cursor).collect();
                while let Some(x) = items.pop() {
                    if x.kind() == "attribute" {
                        let chain = x
                            .child_by_field_name("object")
                            .and_then(|o| dotted_chain(o, src));
                        if let (Some(chain), Some(attr)) =
                            (chain, x.child_by_field_name("attribute"))
                        {
                            patches.push((chain, text(attr, src).to_string()));
                        }
                    } else if x.kind() == "expression_list" {
                        let mut c = x.walk();
                        items.extend(x.named_children(&mut c));
                    }
                }
            }
            "call" => scan_call(n, src, &mut patches),
            _ => {}
        }
        let mut cursor = n.walk();
        stack.extend(n.named_children(&mut cursor));
    }
    (patches, sys_path)
}

/// Records the attribute writes of `setattr`, `delattr`, `patch` and
/// `patch.object` calls. A chain `["*"]` names a patched attribute
/// of any module, and the attribute `"*"` means any attribute.
fn scan_call(call: TsNode, src: &[u8], patches: &mut Vec<(Vec<String>, String)>) {
    let (Some(func), Some(args)) = (
        call.child_by_field_name("function"),
        call.child_by_field_name("arguments"),
    ) else {
        return;
    };
    let f = text(func, src);
    let mut cursor = args.walk();
    let args: Vec<TsNode> = args.named_children(&mut cursor).collect();
    let Some(first) = args.first() else {
        return;
    };
    let string_arg = |a: Option<&TsNode>| {
        a.filter(|a| a.kind() == "string")
            .map(|a| text(*a, src).trim_matches(['"', '\'']).to_string())
    };
    let is_patch = |s: &str, tail: &str| s == tail || s.ends_with(&format!(".{tail}"));
    if matches!(f, "setattr" | "delattr") {
        if let Some(chain) = dotted_chain(*first, src) {
            let attr = string_arg(args.get(1)).unwrap_or_else(|| "*".to_string());
            patches.push((chain, attr));
        }
    } else if is_patch(f, "patch.object") {
        if let Some(chain) = dotted_chain(*first, src) {
            let attr = string_arg(args.get(1)).unwrap_or_else(|| "*".to_string());
            patches.push((chain, attr));
        }
    } else if is_patch(f, "patch") {
        if let Some(target) = string_arg(Some(first)) {
            let last = target.rsplit('.').next().unwrap_or("").to_string();
            patches.push((vec!["*".to_string()], last));
        }
    }
}

/// The record for a binding that is exactly one allowed def or class.
fn proof(b: &Binding) -> Option<PyRec> {
    let def = b.def.as_ref()?;
    (b.count == 1 && def.ok).then(|| PyRec::Def {
        name: def.name.clone(),
        line: def.line,
    })
}

/// Reports whether any identifier under `n` has the text `name`.
fn has_ident(n: TsNode, name: &str, src: &[u8]) -> bool {
    let mut stack = vec![n];
    while let Some(x) = stack.pop() {
        if x.kind() == "identifier" && text(x, src) == name {
            return true;
        }
        let mut cursor = x.walk();
        stack.extend(x.named_children(&mut cursor));
    }
    false
}

/// Reports whether `node` is the first `for` clause of the comprehension
/// `comp` and `prev` is its iterable. Python evaluates that iterable in the
/// enclosing scope.
fn is_first_iterable(comp: TsNode, node: TsNode, prev: TsNode) -> bool {
    if node.kind() != "for_in_clause" {
        return false;
    }
    let mut cursor = comp.walk();
    let first = comp
        .named_children(&mut cursor)
        .find(|c| c.kind() == "for_in_clause");
    first.is_some_and(|f| f.id() == node.id())
        && node
            .child_by_field_name("right")
            .is_some_and(|r| r.id() == prev.id())
}

fn text<'s>(n: TsNode, src: &'s [u8]) -> &'s str {
    n.utf8_text(src).unwrap_or("")
}

/// Builds the table of one scope node: a module, def, lambda, class or
/// comprehension.
fn build_table(scope: TsNode, src: &[u8]) -> Table {
    let mut t = Table::default();
    match scope.kind() {
        "module" => {
            visit_children(scope, src, &mut t, false, false);
            scan_module(scope, src, &mut t);
        }
        "function_definition" | "lambda" => {
            if let Some(params) = scope.child_by_field_name("parameters") {
                bind_params(params, src, &mut t);
            }
            if let Some(body) = scope.child_by_field_name("body") {
                visit(body, src, &mut t, false, false);
                scan_nonlocal(body, src, &mut t);
            }
        }
        "class_definition" => {
            if let Some(body) = scope.child_by_field_name("body") {
                visit_children(body, src, &mut t, false, false);
            }
        }
        _ => {
            let mut cursor = scope.walk();
            for c in scope.named_children(&mut cursor) {
                if c.kind() == "for_in_clause" {
                    if let Some(left) = c.child_by_field_name("left") {
                        bind_targets(left, src, &mut t, 1);
                    }
                }
            }
        }
    }
    t
}

/// An inner `nonlocal f` rebinds `f` here, so it counts as a second binding.
fn scan_nonlocal(body: TsNode, src: &[u8], t: &mut Table) {
    let mut stack = vec![body];
    while let Some(n) = stack.pop() {
        if n.kind() == "nonlocal_statement" {
            let mut cursor = n.walk();
            for c in n.named_children(&mut cursor) {
                t.add(text(c, src), 2, None);
            }
        }
        let mut cursor = n.walk();
        stack.extend(n.named_children(&mut cursor));
    }
}

/// One pass over the whole module: `global` names count as module bindings,
/// and `globals()`, `exec(` or a module `__getattr__` stop every lookup.
fn scan_module(root: TsNode, src: &[u8], t: &mut Table) {
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "global_statement" => {
                let mut cursor = n.walk();
                for c in n.named_children(&mut cursor) {
                    t.add(text(c, src), 2, None);
                }
            }
            "call" => {
                if let Some(f) = n.child_by_field_name("function") {
                    if f.kind() == "identifier"
                        && matches!(text(f, src), "globals" | "exec" | "vars" | "locals")
                    {
                        t.stop_all = true;
                    }
                }
            }
            "subscript"
                if n.child_by_field_name("value")
                    .is_some_and(|v| text(v, src) == "sys.modules") =>
            {
                t.stop_all = true;
            }
            _ => {}
        }
        let mut cursor = n.walk();
        stack.extend(n.named_children(&mut cursor));
    }
    if t.names.contains_key("__getattr__") {
        t.stop_all = true;
    }
}

fn visit_children(n: TsNode, src: &[u8], t: &mut Table, block: bool, walrus_only: bool) {
    let mut cursor = n.walk();
    for c in n.named_children(&mut cursor) {
        visit(c, src, t, block, walrus_only);
    }
}

fn is_block(kind: &str) -> bool {
    matches!(
        kind,
        "if_statement"
            | "try_statement"
            | "for_statement"
            | "while_statement"
            | "with_statement"
            | "match_statement"
    )
}

/// Adds the bindings of `n` and of its children that belong to the scope
/// being built. A nested def, class, lambda or comprehension binds its own
/// names, so only its header and any walrus reach this scope.
fn visit(n: TsNode, src: &[u8], t: &mut Table, block: bool, walrus_only: bool) {
    let kind = n.kind();
    if matches!(kind, "function_definition" | "class_definition") {
        if !walrus_only {
            if let Some(name) = n.child_by_field_name("name") {
                let weight = if block { 2 } else { 1 };
                t.add(
                    text(name, src),
                    weight,
                    Some(def_info(n, name, src, t.shallow)),
                );
            }
        }
        let body = n.child_by_field_name("body").map(|b| b.id());
        let mut cursor = n.walk();
        for c in n.named_children(&mut cursor) {
            if Some(c.id()) != body {
                visit(c, src, t, block, true);
            }
        }
        return;
    }
    if kind == "lambda" || COMPREHENSIONS.contains(&kind) {
        visit_children(n, src, t, block, true);
        return;
    }
    let inner = block || is_block(kind);
    let weight = if inner { 2 } else { 1 };
    if kind == "named_expression" {
        if let Some(name) = n.child_by_field_name("name") {
            t.add(text(name, src), weight, None);
        }
    }
    if !walrus_only {
        match kind {
            "assignment" | "augmented_assignment" | "for_statement" => {
                if let Some(left) = n.child_by_field_name("left") {
                    bind_targets(left, src, t, weight);
                }
            }
            "as_pattern" | "except_clause" | "except_group_clause" => {
                if let Some(alias) = n.child_by_field_name("alias") {
                    bind_targets(alias, src, t, weight);
                }
            }
            "delete_statement" => bind_targets(n, src, t, 2),
            "case_pattern" => bind_all_idents(n, src, t),
            "import_statement" | "import_from_statement" => bind_imports(n, src, t, weight),
            "global_statement" | "nonlocal_statement" => {
                let mut cursor = n.walk();
                for c in n.named_children(&mut cursor) {
                    let set = if kind == "global_statement" {
                        &mut t.globals
                    } else {
                        &mut t.nonlocal
                    };
                    set.insert(text(c, src).to_string());
                }
            }
            "type_alias_statement" => {
                if let Some(first) = n.named_child(0) {
                    bind_targets(first, src, t, weight);
                }
            }
            _ => {}
        }
    }
    visit_children(n, src, t, inner, walrus_only);
}

/// The definition record of a def or class node.
fn def_info(def: TsNode, name: TsNode, src: &[u8], shallow: bool) -> DefInfo {
    let allowed = if def.kind() == "class_definition" {
        CLASS_DECORATORS
    } else {
        DEF_DECORATORS
    };
    let mut ok = true;
    if let Some(parent) = def.parent().filter(|p| p.kind() == "decorated_definition") {
        let mut prov: Option<Table> = None;
        let mut cursor = parent.walk();
        for d in parent.named_children(&mut cursor) {
            if d.kind() == "decorator" {
                ok &= shallow
                    || d.named_child(0)
                        .is_some_and(|e| decorator_ok(e, src, allowed, parent, &mut prov));
            }
        }
    }
    DefInfo {
        name: text(name, src).to_string(),
        line: def.start_position().row as u32 + 1,
        ok,
    }
}

/// Reports whether one decorator expression is on the allowlist and comes
/// from the builtin or the stdlib module that this file imports.
fn decorator_ok(
    expr: TsNode,
    src: &[u8],
    allowed: &[(&str, &str)],
    at: TsNode,
    prov: &mut Option<Table>,
) -> bool {
    let target = if expr.kind() == "call" {
        let Some(f) = expr.child_by_field_name("function") else {
            return false;
        };
        let member = text(f, src).rsplit('.').next().unwrap_or("");
        if !CALL_FORMS.contains(&member) {
            return false;
        }
        f
    } else {
        expr
    };
    let full = text(target, src);
    let (prefix, member) = match full.rsplit_once('.') {
        Some((p, m)) => (Some(p), m),
        None => (None, full),
    };
    let Some(&(_, module)) = allowed.iter().find(|(m, _)| *m == member) else {
        return false;
    };
    let mut root = at;
    while let Some(p) = root.parent() {
        root = p;
    }
    let t = prov.get_or_insert_with(|| {
        let mut t = Table {
            shallow: true,
            ..Table::default()
        };
        visit_children(root, src, &mut t, false, false);
        t
    });
    let once = |name: &str| t.names.get(name).is_some_and(|b| b.count == 1);
    match prefix {
        Some(p) => {
            once(p) && !module.is_empty() && t.module_imports.get(p).is_some_and(|m| m == module)
        }
        None if module.is_empty() => !t.names.contains_key(member),
        None => {
            once(member)
                && t.from_imports
                    .get(member)
                    .is_some_and(|(m, n)| m == module && n == member)
        }
    }
}

fn bind_params(params: TsNode, src: &[u8], t: &mut Table) {
    let mut cursor = params.walk();
    for p in params.named_children(&mut cursor) {
        match p.kind() {
            "default_parameter" | "typed_default_parameter" => {
                if let Some(name) = p.child_by_field_name("name") {
                    bind_targets(name, src, t, 1);
                }
            }
            "typed_parameter" => {
                if let Some(first) = p.named_child(0) {
                    bind_targets(first, src, t, 1);
                }
            }
            _ => bind_targets(p, src, t, 1),
        }
    }
}

/// Adds every name a target pattern binds. An attribute or a subscript
/// binds nothing.
fn bind_targets(n: TsNode, src: &[u8], t: &mut Table, weight: u32) {
    match n.kind() {
        "identifier" => t.add(text(n, src), weight, None),
        "attribute" | "subscript" => {}
        _ => {
            let mut cursor = n.walk();
            for c in n.named_children(&mut cursor) {
                bind_targets(c, src, t, weight);
            }
        }
    }
}

/// Adds every identifier under `n` as a binding, for a match pattern.
fn bind_all_idents(n: TsNode, src: &[u8], t: &mut Table) {
    let mut stack = vec![n];
    while let Some(x) = stack.pop() {
        if x.kind() == "identifier" {
            t.add(text(x, src), 2, None);
        }
        let mut cursor = x.walk();
        stack.extend(x.named_children(&mut cursor));
    }
}

fn bind_imports(n: TsNode, src: &[u8], t: &mut Table, weight: u32) {
    let module_node = n.child_by_field_name("module_name");
    let from = (n.kind() == "import_from_statement")
        .then(|| module_node.map(|m| text(m, src).to_string()))
        .flatten();
    let module = module_node.map(|m| m.id());
    let mut cursor = n.walk();
    for c in n.named_children(&mut cursor) {
        if Some(c.id()) == module {
            continue;
        }
        let (local, real) = match c.kind() {
            "wildcard_import" => {
                t.wild = true;
                continue;
            }
            "aliased_import" => {
                let (Some(a), Some(r)) = (
                    c.child_by_field_name("alias"),
                    c.child_by_field_name("name"),
                ) else {
                    continue;
                };
                (text(a, src), text(r, src))
            }
            "dotted_name" => {
                let Some(first) = c.named_child(0) else {
                    continue;
                };
                (text(first, src), text(c, src))
            }
            _ => continue,
        };
        let plain = from.is_none() && c.kind() == "dotted_name";
        if plain {
            t.dotted.push(real.to_string());
        }
        // A second plain `import a.x` at the top level is the same binding.
        let again = plain && weight == 1 && t.plain_top.contains(local);
        if !again {
            t.add(local, weight, None);
        }
        if plain && weight == 1 {
            t.plain_top.insert(local.to_string());
        }
        match &from {
            Some(m) => {
                t.from_imports
                    .insert(local.to_string(), (m.clone(), real.to_string()));
            }
            None => {
                t.module_imports.insert(local.to_string(), real.to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::extract::Extractor;
    use crate::resolve::resolve_edges;
    use sieve_core::{Confidence, Relation};

    /// The `calls` edges of one `a.py` source as `(caller, callee, confidence)`.
    fn calls(src: &str) -> Vec<(String, String, Confidence)> {
        let mut ext = Extractor::new().expect("extractor builds");
        let (nodes, raw) = ext.extract_file("a.py", src, "python");
        resolve_edges(&nodes, &raw)
            .into_iter()
            .filter(|e| e.relation == Relation::Calls)
            .map(|e| (e.source, e.target, e.confidence))
            .collect()
    }

    fn extracted(src: &str, from: &str, to: &str) -> bool {
        calls(src)
            .iter()
            .any(|(s, t, c)| s == from && t == to && *c == Confidence::Extracted)
    }

    fn none_extracted(src: &str) {
        let got = calls(src);
        assert!(
            got.iter().all(|(_, _, c)| *c != Confidence::Extracted),
            "{got:?}"
        );
    }

    #[test]
    fn test_pyx0_bare_name_path_is_inferred() {
        // A conditional def is no proof. The name path still finds the one
        // same-file def, capped at Inferred.
        let got = calls("if c:\n    def f():\n        pass\ndef g():\n    f()\n");
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].2, Confidence::Inferred);
    }

    #[test]
    fn test_pyx0_class_retry_is_inferred() {
        let got = calls("if c:\n    class Box:\n        pass\ndef g():\n    Box()\n");
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].2, Confidence::Inferred);
    }

    #[test]
    fn test_pyx0_typed_member_is_inferred() {
        let src = "class A:\n    def m(self):\n        pass\ndef g():\n    a = A()\n    a.m()\n";
        let got = calls(src);
        let member = got.iter().find(|(_, t, _)| t == "a.py#A.m");
        assert_eq!(member.map(|m| m.2), Some(Confidence::Inferred), "{got:?}");
    }

    #[test]
    fn test_pyx1_module_def_is_extracted() {
        assert!(extracted(
            "def f():\n    pass\ndef g():\n    f()\n",
            "a.py#g",
            "a.py#f"
        ));
    }

    #[test]
    fn test_pyx1_param_shadow_stops() {
        none_extracted("def f():\n    pass\ndef g(f):\n    f()\n");
        none_extracted("def f():\n    pass\ndef g(*, f=1):\n    f()\n");
    }

    #[test]
    fn test_pyx1_late_assignment_shadows() {
        none_extracted("def f():\n    pass\ndef g():\n    f()\n    f = 1\n");
        none_extracted("def f():\n    pass\ndef g():\n    for f in x:\n        pass\n    f()\n");
        none_extracted("def f():\n    pass\ndef g():\n    with a as f:\n        f()\n");
        none_extracted("def f():\n    pass\ndef g():\n    if (f := h()):\n        f()\n");
        none_extracted("def f():\n    pass\ndef g():\n    import f\n    f()\n");
    }

    #[test]
    fn test_pyx1_comprehension_and_lambda_shadow() {
        none_extracted("def f():\n    pass\ndef g():\n    return [f() for f in xs]\n");
        none_extracted("def f():\n    pass\ng = lambda f: f()\n");
        // The first iterable runs in the enclosing scope.
        assert!(extracted(
            "def f():\n    pass\ndef g():\n    return [x for x in f()]\n",
            "a.py#g",
            "a.py#f"
        ));
    }

    #[test]
    fn test_pyx1_global_and_nonlocal() {
        none_extracted(
            "def f():\n    pass\ndef g():\n    f = 1\n    def h():\n        nonlocal f\n        f()\n",
        );
        // A `global f` in any function makes the module name Many.
        none_extracted(
            "def f():\n    pass\ndef g():\n    global f\n    f = 1\ndef h():\n    f()\n",
        );
    }

    #[test]
    fn test_pyx1_nested_def_from_other_fn_stops() {
        none_extracted("def a():\n    def f():\n        pass\ndef b():\n    f()\n");
    }

    #[test]
    fn test_pyx1_local_def_is_extracted() {
        assert!(extracted(
            "def g():\n    def f():\n        pass\n    f()\n",
            "a.py#g",
            "a.py#g.f"
        ));
        // A closure def reached from an inner function.
        assert!(extracted(
            "def g():\n    def f():\n        pass\n    def h():\n        f()\n",
            "a.py#g.h",
            "a.py#g.f"
        ));
    }

    #[test]
    fn test_pyx1_conditional_def_stops() {
        none_extracted("if c:\n    def f():\n        pass\ndef g():\n    f()\n");
        none_extracted(
            "def f():\n    pass\nif c:\n    def f():\n        pass\ndef g():\n    f()\n",
        );
    }

    #[test]
    fn test_pyx1_try_import_fallback_stops() {
        none_extracted(
            "try:\n    from c import f\nexcept ImportError:\n    def f():\n        pass\ndef g():\n    f()\n",
        );
    }

    #[test]
    fn test_pyx1_class_call_is_extracted() {
        assert!(extracted(
            "class Box:\n    pass\ndef g():\n    Box()\n",
            "a.py#g",
            "a.py#Box"
        ));
    }

    #[test]
    fn test_pyx1_class_body_shadow() {
        // A class-body call sees the class binding first.
        none_extracted("def f():\n    pass\nclass C:\n    f = 1\n    x = f()\n");
        // A method skips the class scope.
        assert!(extracted(
            "def f():\n    pass\nclass C:\n    f = 1\n    def m(self):\n        f()\n",
            "a.py#C.m",
            "a.py#f"
        ));
    }

    #[test]
    fn test_pyx1_class_body_comprehension_skips_class() {
        let src = "def f():\n    pass\nclass C:\n    f = 1\n    xs = [f() for _ in y]\n";
        assert!(extracted(src, "a.py#C", "a.py#f"));
    }

    #[test]
    fn test_pyx1_decorator_not_allowed_is_inferred() {
        let got = calls("import d\n@d.wrap\ndef f():\n    pass\ndef g():\n    f()\n");
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].2, Confidence::Inferred);
    }

    #[test]
    fn test_pyx1_allowed_decorator_is_extracted() {
        let src = "import functools\n@functools.lru_cache(maxsize=2)\ndef f():\n    pass\ndef g():\n    f()\n";
        assert!(extracted(src, "a.py#g", "a.py#f"));
        let src = "@staticmethod\ndef f():\n    pass\ndef g():\n    f()\n";
        assert!(extracted(src, "a.py#g", "a.py#f"));
    }

    #[test]
    fn test_pyx1_exec_stops_module() {
        none_extracted("def f():\n    pass\ndef g():\n    f()\n    exec(s)\n");
        none_extracted("def f():\n    pass\ndef __getattr__(n):\n    pass\ndef g():\n    f()\n");
    }

    #[test]
    fn test_pyx1_del_makes_many() {
        none_extracted("def f():\n    pass\ndel f\ndef g():\n    f()\n");
    }

    #[test]
    fn test_pyx1_other_langs_raw_edges_unchanged() {
        let mut ext = Extractor::new().expect("extractor builds");
        let (_, raw) = ext.extract_file("a.ts", "function f() {}\nf();\n", "typescript");
        assert!(raw.iter().all(|e| e.py.is_none()));
        let json = serde_json::to_string(&raw).expect("serializes");
        assert!(!json.contains("\"py\""));
    }

    #[test]
    fn test_pyx1_except_as_binds() {
        none_extracted(
            "def f():\n    pass\ndef g():\n    try:\n        pass\n    except E as f:\n        f()\n",
        );
    }

    #[test]
    fn test_pyx1_except_group_as_binds() {
        none_extracted(
            "def f():\n    pass\ndef g():\n    try:\n        pass\n    except* E as f:\n        f()\n",
        );
    }

    #[test]
    fn test_pyx1_type_param_binds() {
        none_extracted("def f():\n    pass\ndef g[f]():\n    f()\n");
        none_extracted("def f():\n    pass\nclass A[f]:\n    x = f()\n");
        none_extracted("def f():\n    pass\ntype X[f] = f()\n");
    }

    #[test]
    fn test_pyx1_nonlocal_rebind_from_inner_makes_many() {
        none_extracted(
            "def g():\n    def f():\n        pass\n    def h():\n        nonlocal f\n        f = 1\n    f()\n",
        );
    }

    #[test]
    fn test_pyx1_local_cache_decorator_blocks() {
        let own = "def cache(fn):\n    return fn\n@cache\ndef f():\n    pass\ndef g():\n    f()\n";
        none_extracted(own);
        let other = "from mylib import cache\n@cache\ndef f():\n    pass\ndef g():\n    f()\n";
        none_extracted(other);
        let good = "from functools import cache\n@cache\ndef f():\n    pass\ndef g():\n    f()\n";
        assert!(extracted(good, "a.py#g", "a.py#f"));
        let prefixed =
            "import functools\n@functools.cache\ndef f():\n    pass\ndef g():\n    f()\n";
        assert!(extracted(prefixed, "a.py#g", "a.py#f"));
    }

    #[test]
    fn test_pyx1_sys_modules_write_stops() {
        none_extracted(
            "import sys\ndef f():\n    pass\nsys.modules[__name__].f = 1\ndef g():\n    f()\n",
        );
        none_extracted("def f():\n    pass\nvars()\ndef g():\n    f()\n");
    }
}
