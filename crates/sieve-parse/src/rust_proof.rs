//! The same-file proof for a Rust bare call (a bare
//! call is Extracted only when one binding is proven). The proof holds only when the call reaches
//! exactly one free `fn` item of its own module. Any doubt gives `None`,
//! and the call then takes the capped name path in `resolve.rs`.

use tree_sitter::Node as TsNode;

use crate::extract::RustRec;

/// The record a Rust call site carries: the same-file proof when it holds,
/// else the module path for the resolver, else `None` (a shadowed name or a
/// shape with no static path).
pub fn call_rec(
    root: TsNode,
    source: &str,
    at: usize,
    name: &str,
    defs: &[(String, usize, usize)],
) -> Option<RustRec> {
    if let Some(target) = same_file_target(root, source, at, name, defs) {
        return Some(RustRec::SameFile { target });
    }
    let name_node = root.descendant_for_byte_range(at, at + name.len())?;
    if name_node.kind() == "field_identifier" {
        let target = self_method_target(name_node, source, name, defs)?;
        return Some(RustRec::SameFile { target });
    }
    let (head, mut path) = call_path(name_node, source)?;
    let mods = mods_of(head, source)?;
    if path.first().map(String::as_str) == Some("Self") {
        let ty = self_type(head, source)?;
        path.splice(0..1, ty);
    }
    let first = path.first()?;
    let keyword = matches!(first.as_str(), "crate" | "self" | "super");
    if !keyword && scope_blocks(head, module_of(head), source, first, path.len() == 1) {
        return None;
    }
    let generic = name_node
        .parent()
        .and_then(|p| p.child_by_field_name("path"))
        .and_then(|p| p.utf8_text(source.as_bytes()).ok())
        .is_some_and(|t| t.contains('<'));
    Some(RustRec::Call {
        mods,
        path,
        generic,
    })
}

/// The type path of the innermost enclosing `impl` of `node`, or `None`
/// when that block is a trait impl or a trait body.
fn self_type(node: TsNode, source: &str) -> Option<Vec<String>> {
    let mut cur = node.parent()?;
    while !matches!(cur.kind(), "impl_item" | "trait_item") {
        cur = cur.parent()?;
    }
    if cur.kind() == "trait_item" || cur.child_by_field_name("trait").is_some() {
        return None;
    }
    if !module_level_impl(cur) {
        return None;
    }
    crate::rust_items::type_segments(cur.child_by_field_name("type")?, source)
}

/// True when the parent of the `impl` is a file or a `mod` body. An `impl`
/// inside a block gives `self` a new type.
fn module_level_impl(imp: TsNode) -> bool {
    match imp.parent() {
        Some(p) if p.kind() == "source_file" => true,
        Some(p) => {
            p.kind() == "declaration_list" && p.parent().is_some_and(|m| m.kind() == "mod_item")
        }
        None => false,
    }
}

/// True when the name at byte `at` is the field name of a `x.f()` call.
pub fn is_method_name(root: TsNode, at: usize, len: usize) -> bool {
    root.descendant_for_byte_range(at, at + len)
        .is_some_and(|n| n.kind() == "field_identifier")
}

/// The receiver form of a `fn`: 0 for `self`, 1 for `&self`, 2 for `&mut self`.
/// `None` for no receiver or a typed `self`.
fn receiver_form(func: TsNode) -> Option<u8> {
    let first = func.child_by_field_name("parameters")?.named_child(0)?;
    if first.kind() != "self_parameter" {
        return None;
    }
    let mut walk = first.walk();
    let kinds: Vec<&str> = first.children(&mut walk).map(|c| c.kind()).collect();
    let by_ref = kinds.contains(&"&");
    Some(match (by_ref, kinds.contains(&"mutable_specifier")) {
        (false, _) => 0,
        (true, false) => 1,
        (true, true) => 2,
    })
}

/// True when `node` has a `where` clause or a type parameter bound.
fn has_bound(node: TsNode, source: &str) -> bool {
    node.child_by_field_name("type_parameters")
        .and_then(|tp| tp.utf8_text(source.as_bytes()).ok())
        .is_some_and(|t| t.contains(':'))
        || node
            .children(&mut node.walk())
            .any(|c| c.kind() == "where_clause")
}

/// The `fn` node id that `self.f()` reaches. The proof holds when the
/// caller sits in a module-level inherent `impl`, the same `impl` body holds
/// exactly one `fn f`, and both receivers have the same form. A bound on the
/// `impl` is no stop: the caller sits in the same block, so it holds there.
/// Any bound on `f`,
/// `cfg`, item macro or other doubt gives `None`.
fn self_method_target(
    name_node: TsNode,
    source: &str,
    name: &str,
    defs: &[(String, usize, usize)],
) -> Option<String> {
    let field = name_node.parent()?;
    let call = field.parent()?;
    let is_self_call = field.kind() == "field_expression"
        && field.child_by_field_name("field").map(|n| n.id()) == Some(name_node.id())
        && field.child_by_field_name("value").map(|v| v.kind()) == Some("self")
        && call.kind() == "call_expression"
        && call.child_by_field_name("function").map(|n| n.id()) == Some(field.id());
    if !is_self_call {
        return None;
    }
    let mut caller = call.parent()?;
    while caller.kind() != "function_item" {
        caller = caller.parent()?;
    }
    let body = caller.parent()?;
    let imp = body.parent()?;
    if body.kind() != "declaration_list"
        || imp.kind() != "impl_item"
        || imp.child_by_field_name("trait").is_some()
        || !module_level_impl(imp)
        || !plain_attrs(imp, source)
    {
        return None;
    }
    let mut target: Option<TsNode> = None;
    for item in module_items_of(body) {
        match item.kind() {
            "macro_invocation" => return None,
            "function_item" if item_name(item, source) == Some(name) => {
                if target.is_some() || !plain_attrs(item, source) || has_bound(item, source) {
                    return None;
                }
                target = Some(item);
            }
            _ => {}
        }
    }
    let target = target?;
    if receiver_form(target)? != receiver_form(caller)? {
        return None;
    }
    defs.iter()
        .find(|(_, s, e)| *s == target.start_byte() && *e == target.end_byte())
        .map(|(id, _, _)| id.clone())
}

/// True when every attribute before `item` is `inline`, `allow`, `doc` or
/// `must_use`. Any other attribute (`cfg`, `cfg_attr`, `async_trait`) may
/// change or remove the item, so it stops the proof.
fn plain_attrs(item: TsNode, source: &str) -> bool {
    let mut prev = item.prev_sibling();
    while let Some(p) = prev {
        match p.kind() {
            "attribute_item" => {
                let text = p.utf8_text(source.as_bytes()).unwrap_or("");
                let name: String = text
                    .trim_start_matches("#[")
                    .trim_start()
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if !matches!(name.as_str(), "inline" | "allow" | "doc" | "must_use") {
                    return false;
                }
            }
            "line_comment" | "block_comment" => {}
            _ => break,
        }
        prev = p.prev_sibling();
    }
    true
}

/// The named children of an `impl` body.
fn module_items_of(body: TsNode) -> Vec<TsNode> {
    let mut walk = body.walk();
    body.named_children(&mut walk).collect()
}

/// The call head and the path segments of `f()`, `a::f()` or `a::f::<T>()`,
/// or `None` for any other shape (a method, a type-qualified path).
fn call_path<'t>(name_node: TsNode<'t>, source: &str) -> Option<(TsNode<'t>, Vec<String>)> {
    if name_node.kind() != "identifier" {
        return None;
    }
    if is_macro_arg_call(name_node, source) {
        let text = name_node.utf8_text(source.as_bytes()).ok()?;
        return Some((name_node, vec![text.to_string()]));
    }
    let parent = name_node.parent()?;
    let scoped = parent.kind() == "scoped_identifier"
        && parent.child_by_field_name("name").map(|n| n.id()) == Some(name_node.id());
    let (mut head, path) = if scoped {
        (parent, path_segments(parent, source)?)
    } else {
        let text = name_node.utf8_text(source.as_bytes()).ok()?;
        (name_node, vec![text.to_string()])
    };
    if let Some(p) = head.parent() {
        if p.kind() == "generic_function" {
            head = p;
        }
    }
    let call = head.parent()?;
    let is_head = (call.kind() == "call_expression"
        && call.child_by_field_name("function").map(|n| n.id()) == Some(head.id()))
        || is_fn_value_arg(head);
    is_head.then_some((head, path))
}

/// The segments of a plain path node, or `None` for a generic, bracketed
/// or macro path. A leading `::` gives the segment `::`, which never resolves.
pub(crate) fn path_segments(node: TsNode, source: &str) -> Option<Vec<String>> {
    let text = node.utf8_text(source.as_bytes()).ok()?;
    match node.kind() {
        "identifier" | "crate" | "self" | "super" => Some(vec![text.to_string()]),
        "generic_type_with_turbofish" | "generic_type" => {
            path_segments(node.child_by_field_name("type")?, source)
        }
        "type_identifier" => Some(vec![text.to_string()]),
        "scoped_identifier" => {
            let mut segs = match node.child_by_field_name("path") {
                Some(p) => path_segments(p, source)?,
                None => vec!["::".to_string()],
            };
            let name = node.child_by_field_name("name")?;
            segs.push(name.utf8_text(source.as_bytes()).ok()?.to_string());
            Some(segs)
        }
        _ => None,
    }
}

/// The names of the inline `mod` items around `node`, outermost first, or
/// `None` when a `mod` sits inside a block (it has no file-level path).
fn mods_of(node: TsNode, source: &str) -> Option<Vec<String>> {
    let mut names = Vec::new();
    let mut cur = node;
    while let Some(parent) = cur.parent() {
        if parent.kind() == "mod_item" {
            let grand = parent.parent()?;
            if !matches!(grand.kind(), "source_file" | "declaration_list") {
                return None;
            }
            names.push(item_name(parent, source)?.to_string());
        }
        cur = parent;
    }
    names.reverse();
    Some(names)
}

/// True when something between the call and its module can shadow `name`:
/// a type parameter, an inner item or `use`, and for a bare call also any
/// pattern or parameter that mentions `name`.
fn scope_blocks(head: TsNode, module: TsNode, source: &str, name: &str, bare: bool) -> bool {
    let mut outer_fn: Option<TsNode> = None;
    let mut cur = head;
    while let Some(parent) = cur.parent() {
        if parent.id() == module.id() {
            break;
        }
        if matches!(parent.kind(), "function_item" | "impl_item" | "trait_item") {
            if parent.kind() == "function_item" {
                outer_fn = Some(parent);
            }
            if let Some(tp) = parent.child_by_field_name("type_parameters") {
                if mentions(tp, source, name, None) {
                    return true;
                }
            }
        }
        cur = parent;
    }
    let Some(f) = outer_fn else {
        return false;
    };
    let fn_name = f.child_by_field_name("name").map(|n| n.id());
    (bare && mentions(f, source, name, fn_name)) || inner_blocks(f, source, name)
}

/// True when a descendant item of `node` is named `name`, or an inner `use`
/// may bind `name` (a glob counts).
fn inner_blocks(node: TsNode, source: &str, name: &str) -> bool {
    let mut walk = node.walk();
    let found = node.children(&mut walk).any(|c| {
        let hit = match c.kind() {
            "use_declaration" => use_may_bind(c, source, name),
            "mod_item" | "struct_item" | "enum_item" | "union_item" | "type_item"
            | "trait_item" | "function_item" | "const_item" | "static_item" => {
                item_name(c, source) == Some(name)
            }
            _ => false,
        };
        hit || inner_blocks(c, source, name)
    });
    found
}

/// The id of the free `fn` node that the bare call named at byte `at`
/// reaches, or `None` when no static proof exists. `defs` holds each
/// node id with its start and end byte.
pub fn same_file_target(
    root: TsNode,
    source: &str,
    at: usize,
    name: &str,
    defs: &[(String, usize, usize)],
) -> Option<String> {
    let name_node = root.descendant_for_byte_range(at, at + name.len())?;
    let head = call_head(name_node, source)?;
    let module = module_of(head);
    if scope_blocks(head, module, source, name, true) {
        return None;
    }
    let mut target: Option<TsNode> = None;
    for item in module_items(module) {
        if item_blocks(item, source, name) {
            return None;
        }
        if item.kind() == "function_item" && item_name(item, source) == Some(name) {
            if target.is_some() || has_cfg(item, source) {
                return None;
            }
            target = Some(item);
        }
    }
    let target = target?;
    defs.iter()
        .find(|(_, s, e)| *s == target.start_byte() && *e == target.end_byte())
        .map(|(id, _, _)| id.clone())
}

/// Macros whose arguments are plain expressions. A call in their token
/// tree reads like a call outside a macro.
const EXPR_MACROS: &[&str] = &[
    "format",
    "print",
    "println",
    "eprint",
    "eprintln",
    "write",
    "writeln",
    "panic",
    "assert",
    "assert_eq",
    "assert_ne",
    "debug_assert",
    "debug_assert_eq",
    "debug_assert_ne",
    "vec",
    "format_args",
    "todo",
    "unimplemented",
    "unreachable",
    "dbg",
];

/// True when `name` is the last path segment of an `EXPR_MACROS` macro.
fn is_expr_macro(path_text: &str) -> bool {
    path_text
        .rsplit("::")
        .next()
        .is_some_and(|t| EXPR_MACROS.contains(&t))
}

/// True when the macro name before the `!` node `bang` is in `EXPR_MACROS`.
fn expr_macro_before(bang: TsNode, source: &str) -> bool {
    bang.prev_sibling()
        .filter(|n| matches!(n.kind(), "identifier" | "scoped_identifier"))
        .and_then(|n| n.utf8_text(source.as_bytes()).ok())
        .is_some_and(is_expr_macro)
}

/// True when every token tree around `tree` is an argument list of an
/// `EXPR_MACROS` macro, or a plain bracket group inside one.
fn trusted_tree(tree: TsNode, source: &str) -> bool {
    let Some(parent) = tree.parent() else {
        return false;
    };
    match parent.kind() {
        "macro_invocation" => parent
            .child_by_field_name("macro")
            .and_then(|m| m.utf8_text(source.as_bytes()).ok())
            .is_some_and(is_expr_macro),
        "token_tree" => match tree.prev_sibling() {
            Some(p) if p.kind() == "!" => {
                expr_macro_before(p, source) && trusted_tree(parent, source)
            }
            _ => trusted_tree(parent, source),
        },
        _ => false,
    }
}

/// True when `id` is the name of a bare call `f(..)` inside the arguments
/// of an `EXPR_MACROS` macro. A method, a path, a `fn` name and a macro
/// name do not count.
fn is_macro_arg_call(id: TsNode, source: &str) -> bool {
    let Some(tree) = id.parent().filter(|p| p.kind() == "token_tree") else {
        return false;
    };
    let args_next = id
        .next_sibling()
        .is_some_and(|n| n.kind() == "token_tree" && n.child(0).is_some_and(|c| c.kind() == "("));
    let prev_ok = id.prev_sibling().is_none_or(|p| {
        !matches!(p.kind(), "." | "::" | "!")
            && p.utf8_text(source.as_bytes()).is_ok_and(|t| t != "fn")
    });
    args_next && prev_ok && trusted_tree(tree, source)
}

/// Every bare call name in the arguments of an `EXPR_MACROS` macro under
/// `node`, with its start byte.
pub fn macro_arg_calls(node: TsNode, source: &str, out: &mut Vec<(String, usize)>) {
    if node.kind() == "identifier" && is_macro_arg_call(node, source) {
        if let Ok(t) = node.utf8_text(source.as_bytes()) {
            out.push((t.to_string(), node.start_byte()));
        }
    }
    let mut walk = node.walk();
    for c in node.named_children(&mut walk) {
        macro_arg_calls(c, source, out);
    }
}

/// True when `node` is a whole argument of a call or a method call, as in
/// `.map(f)` or `.map(m::f)`.
fn is_fn_value_arg(node: TsNode) -> bool {
    node.parent().is_some_and(|p| {
        p.kind() == "arguments" && p.parent().is_some_and(|c| c.kind() == "call_expression")
    })
}

/// Every name passed as a whole argument of a call or a method call under
/// `node` (`f`, `m::f`, `Self::f`), with the start byte of its last segment.
/// The resolver gives the link only if the same proof as for `f()` holds.
pub fn fn_value_args(node: TsNode, source: &str, out: &mut Vec<(String, usize)>) {
    let name = match node.kind() {
        "identifier" => Some(node),
        "scoped_identifier" => node.child_by_field_name("name"),
        _ => None,
    };
    if let Some(n) = name.filter(|_| is_fn_value_arg(node)) {
        if let Ok(t) = n.utf8_text(source.as_bytes()) {
            out.push((t.to_string(), n.start_byte()));
        }
    }
    let mut walk = node.walk();
    for c in node.named_children(&mut walk) {
        fn_value_args(c, source, out);
    }
}

/// The node that is the head of a bare `f()` or `f::<T>()` call, or
/// `None` for any other shape (a path, a macro, a method).
fn call_head<'t>(name_node: TsNode<'t>, source: &str) -> Option<TsNode<'t>> {
    if name_node.kind() != "identifier" {
        return None;
    }
    if is_macro_arg_call(name_node, source) {
        return Some(name_node);
    }
    let mut head = name_node;
    let parent = name_node.parent()?;
    if parent.kind() == "generic_function" {
        head = parent;
    }
    let call = head.parent()?;
    let is_head = (call.kind() == "call_expression"
        && call.child_by_field_name("function").map(|n| n.id()) == Some(head.id()))
        || is_fn_value_arg(head);
    is_head.then_some(head)
}

/// The nearest enclosing inline `mod` item, else the file root.
fn module_of(node: TsNode) -> TsNode {
    let mut cur = node;
    while let Some(parent) = cur.parent() {
        if parent.kind() == "mod_item" {
            return parent;
        }
        cur = parent;
    }
    cur
}

/// The direct item children of a module node.
pub(crate) fn module_items(module: TsNode) -> Vec<TsNode> {
    let body = if module.kind() == "mod_item" {
        module.child_by_field_name("body")
    } else {
        Some(module)
    };
    let Some(body) = body else {
        return Vec::new();
    };
    let mut walk = body.walk();
    body.named_children(&mut walk).collect()
}

pub(crate) fn item_name<'a>(item: TsNode, source: &'a str) -> Option<&'a str> {
    item.child_by_field_name("name")?
        .utf8_text(source.as_bytes())
        .ok()
}

/// True when a module item other than the free `fn` could bind `name`.
fn item_blocks(item: TsNode, source: &str, name: &str) -> bool {
    match item.kind() {
        "const_item" | "static_item" | "struct_item" | "mod_item" => {
            item_name(item, source) == Some(name)
        }
        // Any `use` that names `name` or holds a glob may bind it.
        "use_declaration" => use_may_bind(item, source, name),
        "foreign_mod_item" => mentions(item, source, name, None),
        _ => false,
    }
}

fn use_may_bind(node: TsNode, source: &str, name: &str) -> bool {
    if node.kind() == "use_wildcard" {
        return true;
    }
    if node.kind() == "identifier" && node.utf8_text(source.as_bytes()) == Ok(name) {
        return true;
    }
    let mut walk = node.walk();
    let found = node
        .children(&mut walk)
        .any(|c| use_may_bind(c, source, name));
    found
}

/// True when an attribute before `item` mentions `cfg`.
pub(crate) fn has_cfg(item: TsNode, source: &str) -> bool {
    let mut prev = item.prev_sibling();
    while let Some(p) = prev {
        match p.kind() {
            "attribute_item" => {
                if p.utf8_text(source.as_bytes())
                    .is_ok_and(|t| t.contains("cfg"))
                {
                    return true;
                }
            }
            "line_comment" | "block_comment" => {}
            _ => break,
        }
        prev = p.prev_sibling();
    }
    false
}

/// True when `node` holds an identifier or type name equal to `name`
/// that is not the head of a call. `skip` is a node id to ignore (the
/// name of the enclosing fn).
fn mentions(node: TsNode, source: &str, name: &str, skip: Option<usize>) -> bool {
    if Some(node.id()) == skip {
        return false;
    }
    if matches!(
        node.kind(),
        "identifier" | "type_identifier" | "shorthand_field_identifier"
    ) && node.utf8_text(source.as_bytes()) == Ok(name)
    {
        // A path segment (`f::g`) is not a binding either.
        let is_call_head = is_macro_arg_call(node, source)
            || is_fn_value_arg(node)
            || node.parent().is_some_and(|p| {
                p.kind() == "scoped_identifier"
                    || p.kind() == "generic_function"
                    || (p.kind() == "call_expression"
                        && p.child_by_field_name("function").map(|n| n.id()) == Some(node.id()))
            });
        return !is_call_head;
    }
    let mut walk = node.walk();
    let found = node
        .children(&mut walk)
        .any(|c| mentions(c, source, name, skip));
    found
}

#[cfg(test)]
mod tests {
    use crate::generic::GenericExtractor;
    use crate::resolve::resolve_edges;
    use sieve_core::{Confidence, Relation};

    /// The `calls` edges of one `.rs` source as `(caller, callee, confidence)`.
    fn calls(src: &str) -> Vec<(String, String, Confidence)> {
        let mut ext = GenericExtractor::new().expect("extractor builds");
        let (nodes, raw) = ext.extract_file("a.rs", src, "rust");
        resolve_edges(&nodes, &raw)
            .into_iter()
            .filter(|e| e.relation == Relation::Calls)
            .map(|e| (e.source, e.target, e.confidence))
            .collect()
    }

    fn none_extracted(src: &str) {
        let got = calls(src);
        assert!(
            got.iter().all(|(_, _, c)| *c != Confidence::Extracted),
            "{got:?}"
        );
    }

    fn one_extracted(src: &str) {
        let got = calls(src);
        assert!(
            got.iter()
                .any(|(s, t, c)| s == "a.rs#g" && t == "a.rs#f" && *c == Confidence::Extracted),
            "{got:?}"
        );
    }

    #[test]
    fn test_macro_arg_call_is_extracted() {
        one_extracted("fn f() -> i32 { 1 }\nfn g() { eprintln!(\"{}\", f()); }\n");
        one_extracted("fn f(x: i32) -> i32 { x }\nfn g() { let v = vec![f(1), 2]; assert_eq!(f(1), v[0]); }\n");
    }

    fn links_helper(src: &str) -> bool {
        calls(src)
            .iter()
            .any(|(s, t, c)| s == "a.rs#g" && t.ends_with("helper") && *c == Confidence::Extracted)
    }

    #[test]
    fn test_fn_value_arg_bare_is_extracted() {
        assert!(links_helper(
            "fn helper(x: i32) -> i32 { x }\nfn g(v: Vec<i32>) { let _ = v.into_iter().map(helper); }\n"
        ));
    }

    #[test]
    fn test_fn_value_arg_path_is_extracted() {
        let got = calls(
            "struct S;\nimpl S {\n fn helper(x: i32) -> i32 { x }\n fn g(v: Vec<i32>) { let _ = v.into_iter().map(Self::helper); }\n}\n",
        );
        assert!(got.iter().any(|(_, t, _)| t.ends_with("helper")), "{got:?}");
        let got = calls(
            "mod m { pub fn helper(x: i32) -> i32 { x } }\nfn g(v: Vec<i32>) { let _ = v.into_iter().map(m::helper); }\n",
        );
        assert!(got.iter().any(|(_, t, _)| t.ends_with("helper")), "{got:?}");
    }

    #[test]
    fn test_fn_value_arg_local_closure_is_no_edge() {
        assert!(!links_helper(
            "fn helper(x: i32) -> i32 { x }\nfn g(v: Vec<i32>) { let helper = |x| x; let _ = v.into_iter().map(helper); }\n"
        ));
    }

    #[test]
    fn test_fn_value_arg_param_is_no_edge() {
        assert!(!links_helper(
            "fn helper(x: i32) -> i32 { x }\nfn g(v: Vec<i32>, helper: fn(i32) -> i32) { let _ = v.into_iter().map(helper); }\n"
        ));
    }

    #[test]
    fn test_macro_arg_local_name_is_no_edge() {
        let src = "fn f() -> i32 { 1 }\nfn g() { let f = 3; println!(\"{}\", f); }\n";
        assert!(calls(src).is_empty(), "{:?}", calls(src));
        let src = "fn f() -> i32 { 1 }\nfn g() { let f = |x: i32| x; println!(\"{}\", f(1)); }\n";
        none_extracted(src);
        assert!(calls(src).is_empty(), "{:?}", calls(src));
    }

    #[test]
    fn test_macro_arg_method_and_unknown_macro_give_no_edge() {
        let src = "fn f() -> i32 { 1 }\nfn g(o: O) { println!(\"{}\", o.f()); mine!(f()); }\n";
        assert!(calls(src).is_empty(), "{:?}", calls(src));
    }

    #[test]
    fn test_rsx0_std_scoped_call_not_extracted() {
        none_extracted(
            "struct Foo;\nimpl Foo { fn new() -> Foo { Foo } }\nfn g() { let _v = Vec::new(); }\n",
        );
    }

    #[test]
    fn test_rsx0_macro_call_inferred() {
        let src = "macro_rules! m { () => {} }\nfn g() { m!(); }\n";
        let got = calls(src);
        assert!(
            got.iter()
                .any(|(_, t, c)| t == "a.rs#m" && *c == Confidence::Inferred),
            "{got:?}"
        );
        none_extracted(src);
    }

    #[test]
    fn test_rsx0_impl_method_target_not_extracted() {
        none_extracted("struct S;\nimpl S { fn f() {} }\nfn g() { f(); }\n");
    }

    #[test]
    fn test_rsx1_same_file_free_fn_extracted() {
        one_extracted("fn f() {}\nfn g() { f(); }\n");
    }

    #[test]
    fn test_rsx1_parent_item_not_in_scope() {
        none_extracted("fn f() {}\nmod x {\n    fn g() { f(); }\n}\n");
        // The items of the inline module itself are in scope.
        one_extracted("mod x {\n    fn f() {}\n    fn g() { f(); }\n}\n");
    }

    #[test]
    fn test_rsx1_param_shadow_no_edge() {
        none_extracted("fn f() {}\nfn g(f: fn()) { f(); }\n");
        none_extracted("fn f() {}\nfn g() { let f = || (); f(); }\n");
    }

    #[test]
    fn test_rsx1_type_param_shadow_no_edge() {
        none_extracted("fn f() {}\nfn g<f: Fn()>(x: f) { f(); }\n");
        none_extracted("fn f() {}\nstruct S;\nimpl<f> S { fn g() { f(); } }\n");
    }

    #[test]
    fn test_rsx1_use_binds_name_no_proof() {
        none_extracted("use other::f;\nfn f() {}\nfn g() { f(); }\n");
        none_extracted("use other::*;\nfn f() {}\nfn g() { f(); }\n");
        none_extracted("fn f() {}\nfn g() { use other::f; f(); }\n");
    }

    #[test]
    fn test_rsx1_cfg_twins_no_proof() {
        none_extracted("#[cfg(unix)]\nfn f() {}\n#[cfg(not(unix))]\nfn f() {}\nfn g() { f(); }\n");
        none_extracted("#[cfg(unix)]\nfn f() {}\nfn g() { f(); }\n");
    }

    #[test]
    fn test_rsx1_const_same_name_no_proof() {
        none_extracted("fn f() {}\nconst f: i32 = 1;\nfn g() { f(); }\n");
        none_extracted("fn f() {}\nstruct f(i32);\nfn g() { f(1); }\n");
    }

    #[test]
    fn test_rsx1_turbofish_stripped() {
        one_extracted("fn f<T>() {}\nfn g() { f::<u8>(); }\n");
    }

    #[test]
    fn test_rsx1_other_langs_raw_edges_unchanged() {
        let mut ext = GenericExtractor::new().expect("extractor builds");
        let (_, raw) = ext.extract_file("a.c", "void f(){}\nvoid g(){ f(); }\n", "c");
        assert!(!raw.is_empty());
        for edge in raw {
            assert!(edge.rust.is_none());
            let json = serde_json::to_string(&edge).expect("serializes");
            assert!(!json.contains("rust"), "{json}");
        }
    }

    #[test]
    fn test_rsx1_shorthand_field_shadow_no_edge() {
        none_extracted("fn f() {}\nstruct S { f: fn() }\nfn g(s: S) { let S { f } = s; f(); }\n");
    }

    #[test]
    fn test_rsx1_inner_fn_shadow_no_edge() {
        none_extracted("fn f() {}\nfn g() { fn f() {} f(); }\n");
    }

    #[test]
    fn test_rsx1_trait_signature_not_free_fn() {
        none_extracted("trait T { fn f(); }\nfn g() { f(); }\n");
    }

    #[test]
    fn test_rsx1_trait_default_method_not_free_fn() {
        none_extracted("trait T { fn f() {} }\nfn g() { f(); }\n");
    }

    #[test]
    fn test_rsx1_cfg_on_use_no_proof() {
        none_extracted("fn f() {}\n#[cfg(unix)]\nuse other::f;\nfn g() { f(); }\n");
    }

    #[test]
    fn test_rsx1_cfg_on_mod_no_proof() {
        none_extracted("fn f() {}\n#[cfg(unix)]\nmod f;\nfn g() { f(); }\n");
    }

    #[test]
    fn test_rmx1_self_method_proven() {
        for recv in ["&self", "&mut self", "self"] {
            let src = format!(
                "struct S;\nimpl S {{\n fn f({recv}) {{}}\n fn g({recv}) {{ self.f(); }}\n}}\n"
            );
            one_extracted(&src);
        }
        one_extracted(
            "mod m {\n struct S;\n impl S { fn f(&self) {} fn g(&self) { self.f(); } }\n}\n",
        );
        // The bounds of the impl hold in the same block.
        one_extracted(
            "struct S<T>(T);\nimpl<T: Copy> S<T> { fn f(&self) {} fn g(&self) { self.f(); } }\n",
        );
        one_extracted("struct S<T>(T);\nimpl<T> S<T> where T: Copy { fn f(&self) {} fn g(&self) { self.f(); } }\n");
        one_extracted(
            "struct S<T>(T);\nimpl<T> S<T> { fn f(&self) {} fn g(&self) { self.f(); } }\n",
        );
    }

    #[test]
    fn test_rmx1_self_method_stops() {
        // A bound on f, a where clause on f, a cfg on f.
        none_extracted("struct S;\nimpl S { fn f<T: Copy>(&self) {} fn g(&self) { self.f(); } }\n");
        none_extracted(
            "struct S;\nimpl S { fn f<T>(&self) where T: Copy {} fn g(&self) { self.f(); } }\n",
        );
        none_extracted(
            "struct S;\nimpl S { #[cfg(unix)] fn f(&self) {} fn g(&self) { self.f(); } }\n",
        );
        // Two fns named f.
        none_extracted("struct S;\nimpl S {\n #[cfg(unix)]\n fn f(&self) {}\n #[cfg(not(unix))]\n fn f(&self) {}\n fn g(&self) { self.f(); }\n}\n");
        // The receiver forms differ.
        none_extracted("struct S;\nimpl S { fn f(&mut self) {} fn g(&self) { self.f(); } }\n");
        none_extracted("struct S;\nimpl S { fn f(self) {} fn g(&self) { self.f(); } }\n");
        // f has no receiver; f sits in another impl block; an item macro.
        none_extracted("struct S;\nimpl S { fn f() {} fn g(&self) { self.f(); } }\n");
        none_extracted(
            "struct S;\nimpl S { fn f(&self) {} }\nimpl S { fn g(&self) { self.f(); } }\n",
        );
        none_extracted("struct S;\nimpl S { m!(); fn f(&self) {} fn g(&self) { self.f(); } }\n");
        // A trait impl caller; another receiver.
        none_extracted("struct S;\ntrait T { fn g(&self); }\nimpl S { fn f(&self) {} }\nimpl T for S { fn g(&self) { self.f(); } }\n");
        none_extracted("struct S;\nimpl S { fn f(&self) {} fn g(&self, x: S) { x.f(); } }\n");
        assert!(
            calls("struct S;\nimpl S { fn f(&self) {} fn g(&self, x: S) { x.f(); } }\n").is_empty()
        );
    }

    #[test]
    fn test_rmx1_attribute_allowlist() {
        none_extracted(
            "struct S;\n#[async_trait]\nimpl S { fn f(&self) {} fn g(&self) { self.f(); } }\n",
        );
        none_extracted(
            "struct S;\nimpl S { #[custom] fn f(&self) {} fn g(&self) { self.f(); } }\n",
        );
        none_extracted("struct S;\nimpl S { #[cfg_attr(unix, inline)] fn f(&self) {} fn g(&self) { self.f(); } }\n");
        one_extracted("struct S;\n#[allow(dead_code)]\nimpl S { #[inline]\n #[must_use]\n fn f(&self) {} fn g(&self) { self.f(); } }\n");
    }

    #[test]
    fn test_rmx1_closure_and_nested_fn() {
        one_extracted(
            "struct S;\nimpl S { fn f(&self) {} fn g(&self) { let c = || self.f(); c(); } }\n",
        );
        none_extracted(
            "struct S;\nimpl S { fn f(&self) {} fn g(&self) { fn h() { self.f(); } } }\n",
        );
    }

    #[test]
    fn test_rmx1_field_and_var_receivers_no_raw_edge() {
        let mut ext = GenericExtractor::new().expect("extractor builds");
        let src = "struct S { a: S }\nimpl S { fn f(&self) {} fn g(&self, x: S) { self.a.f(); x.f(); } }\n";
        let (_, raw) = ext.extract_file("a.rs", src, "rust");
        assert!(
            raw.iter().all(|e| e.name.as_deref() != Some("f")),
            "{raw:?}"
        );
    }

    #[test]
    fn test_rmx1_nested_impl_self_method_no_proof() {
        // The inner impl gives `self` a new type.
        none_extracted("struct L;\nimpl L { fn f(&self) {} }\nfn outer() {\n struct L;\n impl L { fn g(&self) { self.f(); } }\n}\n");
    }
}
