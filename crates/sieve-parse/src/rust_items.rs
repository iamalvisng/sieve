//! The module-level items of one Rust file (the Rust
//! call proof). The resolver reads these records to build the
//! module tree and to look a name up. Anything this pass cannot read
//! becomes a record that stops a lookup.

use serde::{Deserialize, Serialize};
use sieve_core::Relation;
use tree_sitter::Node as TsNode;

use crate::extract::{RawEdge, RustRec};
use crate::rust_proof::{has_cfg, item_name, module_items, path_segments};

/// One module-level item: the inline `mod` chain it sits in, its flags, and
/// what it binds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RItem {
    /// The names of the inline modules around the item, outermost first.
    pub mods: Vec<String>,
    /// True for a plain `pub` item.
    pub public: bool,
    /// True when a `cfg` attribute sits on the item.
    pub cfg: bool,
    /// What the item binds.
    pub kind: RKind,
}

/// What a module-level item binds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RKind {
    /// A free `fn` and the id of its node.
    Fn { name: String, id: String },
    /// A name in the value namespace that is not a free `fn`.
    Value { name: String },
    /// A name in the type namespace.
    Type { name: String },
    /// A tuple or unit struct: one name in both namespaces.
    Both { name: String },
    /// A struct, enum or union. `both` is set for a tuple or unit struct.
    /// `variants` holds the variant names of an enum.
    Adt {
        name: String,
        both: bool,
        variants: Vec<String>,
    },
    /// An `impl` block with the `fn` items it holds. `ty` is the self type
    /// path without generic arguments (empty when it is not a plain path).
    /// `opaque` is set for an `impl` inside a block, which has no module.
    Impl {
        ty: Vec<String>,
        tr: bool,
        opaque: bool,
        fns: Vec<ImplFn>,
    },
    /// A `mod` item, inline or in its own file, with its `#[path]` value.
    Mod {
        name: String,
        inline: bool,
        path: Option<String>,
    },
    /// One name a `use` binds. `local` is `*` for a glob. `modonly` is set
    /// for `self` in a group, which binds a module only.
    Use {
        path: Vec<String>,
        local: String,
        modonly: bool,
    },
    /// An item-level macro call, which can define any item.
    Macro,
}

/// One `fn` item of an `impl` block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImplFn {
    /// The `fn` name.
    pub name: String,
    /// The id of the `fn` node.
    pub id: String,
    /// True when a `cfg` attribute sits on the `fn`.
    pub cfg: bool,
    /// True for a plain `pub` fn.
    pub public: bool,
}

/// Pushes one `imports` raw edge that carries all module-level items of a
/// Rust file. The resolver never turns it into a graph edge.
pub fn push_file_record(
    root: TsNode,
    path: &str,
    source: &str,
    defs: &[(String, usize, usize)],
    raw_edges: &mut Vec<RawEdge>,
) {
    let mut items = Vec::new();
    collect(root, source, defs, &mut Vec::new(), &mut items);
    raw_edges.push(RawEdge {
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
        rust: Some(RustRec::File { items }),
        py: None,
        go: None,
        java: None,
        cs: None,
    });
}

fn collect(
    module: TsNode,
    source: &str,
    defs: &[(String, usize, usize)],
    mods: &mut Vec<String>,
    out: &mut Vec<RItem>,
) {
    for item in module_items(module) {
        let public = is_pub(item, source);
        let cfg = has_cfg(item, source);
        for kind in kinds_of(item, source, defs) {
            out.push(RItem {
                mods: mods.clone(),
                public,
                cfg,
                kind,
            });
        }
        if item.kind() != "mod_item" {
            block_impls(item, source, defs, false, &mut |kind| {
                out.push(RItem {
                    mods: mods.clone(),
                    public: false,
                    cfg: false,
                    kind,
                });
            });
        }
        if item.kind() == "mod_item" && item.child_by_field_name("body").is_some() {
            if let Some(name) = item_name(item, source) {
                mods.push(name.to_string());
                collect(item, source, defs, mods, out);
                mods.pop();
            }
        }
    }
}

/// Reports each `impl` block that sits inside a block of `node`. Such a
/// block belongs to no module, so the record is opaque.
fn block_impls(
    node: TsNode,
    source: &str,
    defs: &[(String, usize, usize)],
    in_block: bool,
    emit: &mut dyn FnMut(RKind),
) {
    let mut walk = node.walk();
    for c in node.named_children(&mut walk) {
        if c.kind() == "impl_item" && in_block {
            if let Some(kind) = impl_kind(c, source, defs, true) {
                emit(kind);
            }
        }
        block_impls(c, source, defs, in_block || c.kind() == "block", emit);
    }
}

/// The record of one `impl` block, or `None` when it has no body.
fn impl_kind(
    item: TsNode,
    source: &str,
    defs: &[(String, usize, usize)],
    opaque: bool,
) -> Option<RKind> {
    let body = item.child_by_field_name("body")?;
    let ty = item
        .child_by_field_name("type")
        .and_then(|t| type_segments(t, source))
        .unwrap_or_default();
    let mut fns = Vec::new();
    let mut walk = body.walk();
    for f in body.named_children(&mut walk) {
        if f.kind() != "function_item" {
            continue;
        }
        let id = defs
            .iter()
            .find(|(_, s, e)| *s == f.start_byte() && *e == f.end_byte())
            .map(|(id, _, _)| id.clone());
        if let (Some(name), Some(id)) = (item_name(f, source), id) {
            fns.push(ImplFn {
                name: name.to_string(),
                id,
                cfg: has_cfg(f, source),
                public: is_pub(f, source),
            });
        }
    }
    Some(RKind::Impl {
        ty,
        tr: item.child_by_field_name("trait").is_some(),
        opaque,
        fns,
    })
}

/// The segments of a type path without generic arguments, or `None` for any
/// other type.
pub(crate) fn type_segments(node: TsNode, source: &str) -> Option<Vec<String>> {
    let text = |n: TsNode| n.utf8_text(source.as_bytes()).ok().map(str::to_string);
    match node.kind() {
        "type_identifier" => Some(vec![text(node)?]),
        "generic_type" => type_segments(node.child_by_field_name("type")?, source),
        "scoped_type_identifier" => {
            let mut segs = match node.child_by_field_name("path") {
                Some(p) => path_segments(p, source)?,
                None => vec!["::".to_string()],
            };
            segs.push(text(node.child_by_field_name("name")?)?);
            Some(segs)
        }
        _ => None,
    }
}

fn is_pub(item: TsNode, source: &str) -> bool {
    let mut walk = item.walk();
    let found = item.children(&mut walk).any(|c| {
        c.kind() == "visibility_modifier"
            && c.utf8_text(source.as_bytes()).map(str::trim) == Ok("pub")
    });
    found
}

fn kinds_of(item: TsNode, source: &str, defs: &[(String, usize, usize)]) -> Vec<RKind> {
    let name = item_name(item, source).map(str::to_string);
    let one = |make: fn(String) -> RKind| name.clone().map(make).into_iter().collect();
    match item.kind() {
        "function_item" => {
            let id = defs
                .iter()
                .find(|(_, s, e)| *s == item.start_byte() && *e == item.end_byte())
                .map(|(id, _, _)| id.clone());
            match (name, id) {
                (Some(name), Some(id)) => vec![RKind::Fn { name, id }],
                (Some(name), None) => vec![RKind::Value { name }],
                _ => Vec::new(),
            }
        }
        "const_item" | "static_item" => one(|name| RKind::Value { name }),
        "trait_item" | "type_item" => one(|name| RKind::Type { name }),
        "enum_item" | "union_item" => name
            .map(|name| RKind::Adt {
                name,
                both: false,
                variants: variant_names(item, source),
            })
            .into_iter()
            .collect(),
        "struct_item" => {
            let braced = item
                .child_by_field_name("body")
                .is_some_and(|b| b.kind() == "field_declaration_list");
            name.map(|name| RKind::Adt {
                name,
                both: !braced,
                variants: Vec::new(),
            })
            .into_iter()
            .collect()
        }
        "impl_item" => impl_kind(item, source, defs, false).into_iter().collect(),
        "mod_item" => name
            .map(|name| RKind::Mod {
                name,
                inline: item.child_by_field_name("body").is_some(),
                path: path_attr(item, source),
            })
            .into_iter()
            .collect(),
        "extern_crate_declaration" => {
            let bound = item
                .child_by_field_name("alias")
                .or_else(|| item.child_by_field_name("name"))
                .and_then(|n| n.utf8_text(source.as_bytes()).ok());
            bound
                .map(|name| RKind::Type {
                    name: name.to_string(),
                })
                .into_iter()
                .collect()
        }
        "foreign_mod_item" => {
            let mut names = Vec::new();
            foreign_names(item, source, &mut names);
            names.into_iter().map(|name| RKind::Both { name }).collect()
        }
        "use_declaration" => use_kinds(item, source),
        "macro_invocation" | "ERROR" => vec![RKind::Macro],
        "expression_statement" => {
            let mut walk = item.walk();
            let has_macro = item
                .named_children(&mut walk)
                .any(|c| c.kind() == "macro_invocation");
            if has_macro {
                vec![RKind::Macro]
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    }
}

fn variant_names(item: TsNode, source: &str) -> Vec<String> {
    let Some(body) = item.child_by_field_name("body") else {
        return Vec::new();
    };
    let mut walk = body.walk();
    let names = body
        .named_children(&mut walk)
        .filter(|c| c.kind() == "enum_variant")
        .filter_map(|c| item_name(c, source).map(str::to_string))
        .collect();
    names
}

fn foreign_names(node: TsNode, source: &str, out: &mut Vec<String>) {
    let mut walk = node.walk();
    for c in node.named_children(&mut walk) {
        if matches!(
            c.kind(),
            "function_signature_item" | "static_item" | "type_item"
        ) {
            if let Some(n) = item_name(c, source) {
                out.push(n.to_string());
            }
        } else {
            foreign_names(c, source, out);
        }
    }
}

/// The `#[path = "..."]` value on a `mod` item, if any.
fn path_attr(item: TsNode, source: &str) -> Option<String> {
    let mut prev = item.prev_sibling();
    while let Some(p) = prev {
        match p.kind() {
            "attribute_item" => {
                let text: String = p
                    .utf8_text(source.as_bytes())
                    .ok()?
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect();
                if let Some(rest) = text.strip_prefix("#[path=\"") {
                    if let Some(value) = rest.strip_suffix("\"]") {
                        return Some(value.to_string());
                    }
                }
            }
            "line_comment" | "block_comment" => {}
            _ => break,
        }
        prev = p.prev_sibling();
    }
    None
}

/// The bindings of one `use` declaration. A form this pass cannot read
/// becomes an unresolvable glob, which stops every lookup that reaches it.
fn use_kinds(item: TsNode, source: &str) -> Vec<RKind> {
    let mut out = Vec::new();
    let ok = item
        .child_by_field_name("argument")
        .is_some_and(|arg| use_leaves(arg, source, &[], &mut out));
    if ok {
        return out;
    }
    vec![RKind::Use {
        path: vec!["::".to_string()],
        local: "*".to_string(),
        modonly: false,
    }]
}

fn use_leaves(node: TsNode, source: &str, prefix: &[String], out: &mut Vec<RKind>) -> bool {
    match node.kind() {
        "identifier" | "scoped_identifier" | "crate" | "self" | "super" => {
            let Some(segs) = path_segments(node, source) else {
                return false;
            };
            push_leaf(prefix, segs, None, out)
        }
        "use_as_clause" => {
            let (Some(path), Some(alias)) = (
                node.child_by_field_name("path"),
                node.child_by_field_name("alias"),
            ) else {
                return false;
            };
            let (Some(segs), Ok(alias)) = (
                path_segments(path, source),
                alias.utf8_text(source.as_bytes()),
            ) else {
                return false;
            };
            if alias == "_" {
                return true;
            }
            push_leaf(prefix, segs, Some(alias.to_string()), out)
        }
        "use_wildcard" => {
            let mut walk = node.walk();
            let path = node.named_children(&mut walk).next();
            let mut full = prefix.to_vec();
            if let Some(p) = path {
                let Some(segs) = path_segments(p, source) else {
                    return false;
                };
                full.extend(segs);
            }
            out.push(RKind::Use {
                path: full,
                local: "*".to_string(),
                modonly: false,
            });
            true
        }
        "scoped_use_list" => {
            let mut full = prefix.to_vec();
            if let Some(p) = node.child_by_field_name("path") {
                let Some(segs) = path_segments(p, source) else {
                    return false;
                };
                full.extend(segs);
            }
            let Some(list) = node.child_by_field_name("list") else {
                return false;
            };
            use_leaves(list, source, &full, out)
        }
        "use_list" => {
            let mut walk = node.walk();
            let all = node
                .named_children(&mut walk)
                .all(|c| use_leaves(c, source, prefix, out));
            all
        }
        _ => false,
    }
}

/// Adds the binding of one leaf path. A leaf `self` in a group binds the
/// group prefix as a module.
fn push_leaf(
    prefix: &[String],
    segs: Vec<String>,
    alias: Option<String>,
    out: &mut Vec<RKind>,
) -> bool {
    let mut path = prefix.to_vec();
    path.extend(segs);
    let modonly = path.len() > 1 && path.last().map(String::as_str) == Some("self");
    if modonly {
        path.pop();
    }
    let Some(last) = path.last() else {
        return false;
    };
    let local = alias.unwrap_or_else(|| last.clone());
    out.push(RKind::Use {
        path,
        local,
        modonly,
    });
    true
}
