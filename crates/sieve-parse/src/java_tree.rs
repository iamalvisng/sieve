//! The type facts of one Java file (the Java call proof). The facts
//! feed the call proof in `java_proof.rs` and the index in `java_index.rs`.
//! A type that sits in the member chain of a top-level type is a "clean"
//! type and gets a `JavaType`. An anonymous class, an enum constant body and
//! a local class get a `JavaBody`: they never hold a proof, but their
//! method names can override a target.

use std::cell::RefCell;
use std::collections::HashMap;

use tree_sitter::Node as TsNode;

use crate::extract::{
    JavaBody, JavaFileRec, JavaImport, JavaKind, JavaMethod, JavaRec, JavaType, RawEdge,
};
use crate::go_tree::text;
use sieve_core::Relation;

/// The node kinds of a type declaration.
pub(crate) const TYPE_KINDS: &[&str] = &[
    "class_declaration",
    "interface_declaration",
    "enum_declaration",
    "record_declaration",
    "annotation_type_declaration",
];

/// The modifiers of a declaration.
#[derive(Default)]
struct Mods {
    public: bool,
    private: bool,
    protected: bool,
    stat: bool,
    abs: bool,
    fin: bool,
    default: bool,
}

/// Reads the modifier keywords of `n`.
fn modifiers(n: TsNode) -> Mods {
    let mut m = Mods::default();
    let mut c = n.walk();
    let Some(mods) = n.children(&mut c).find(|k| k.kind() == "modifiers") else {
        return m;
    };
    let mut mc = mods.walk();
    for k in mods.children(&mut mc) {
        match k.kind() {
            "public" => m.public = true,
            "private" => m.private = true,
            "protected" => m.protected = true,
            "static" => m.stat = true,
            "abstract" => m.abs = true,
            "final" => m.fin = true,
            "default" => m.default = true,
            _ => {}
        }
    }
    m
}

/// The simple type name that a type node writes: `T` or `T<..>`. Any other
/// form (an array, a primitive, a qualified or annotated type) gives `None`.
pub(crate) fn written_simple(n: TsNode, src: &[u8]) -> Option<String> {
    match n.kind() {
        "type_identifier" => Some(text(n, src).to_string()),
        "generic_type" => {
            let first = n.named_child(0)?;
            (first.kind() == "type_identifier").then(|| text(first, src).to_string())
        }
        _ => None,
    }
}

/// A written supertype with generics and spaces dropped. A qualified name
/// keeps its dots.
fn super_text(n: TsNode, src: &[u8]) -> String {
    let raw = text(n, src);
    if raw.contains('@') {
        return "?".to_string();
    }
    let mut depth = 0u32;
    let mut out = String::new();
    for ch in raw.chars() {
        match ch {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            c if depth == 0 && !c.is_whitespace() => out.push(c),
            _ => {}
        }
    }
    out
}

/// The last segment of a dotted name.
pub(crate) fn last_segment(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

/// The name of each method declared directly in a class body.
fn method_names(body: TsNode, src: &[u8]) -> Vec<String> {
    let mut c = body.walk();
    body.named_children(&mut c)
        .filter(|m| m.kind() == "method_declaration")
        .filter_map(|m| m.child_by_field_name("name"))
        .map(|n| text(n, src).to_string())
        .collect()
}

/// The extends type and the interface types that a declaration writes.
fn supers(n: TsNode, src: &[u8]) -> (Option<String>, Vec<String>) {
    let mut ext = None;
    let mut imp = Vec::new();
    let mut c = n.walk();
    for ch in n.named_children(&mut c) {
        match ch.kind() {
            "superclass" => ext = ch.named_child(0).map(|t| super_text(t, src)),
            "super_interfaces" | "extends_interfaces" => {
                let mut lc = ch.walk();
                for list in ch.named_children(&mut lc) {
                    let mut tc = list.walk();
                    imp.extend(list.named_children(&mut tc).map(|t| super_text(t, src)));
                }
            }
            _ => {}
        }
    }
    (ext, imp)
}

/// The written parameter types of a method. A generic method gives `?` for
/// each parameter, because a type parameter can share a name with a class.
fn param_types(method: TsNode, src: &[u8]) -> Vec<String> {
    let Some(params) = method.child_by_field_name("parameters") else {
        return Vec::new();
    };
    let generic = method.child_by_field_name("type_parameters").is_some();
    let mut out = Vec::new();
    let mut c = params.walk();
    for p in params.named_children(&mut c) {
        let ty = match p.kind() {
            "formal_parameter" => p.child_by_field_name("type").map(|t| {
                let dims = p
                    .child_by_field_name("dimensions")
                    .map_or(0, |d| text(d, src).matches('[').count());
                format!("{}{}", super_text(t, src), "[]".repeat(dims))
            }),
            "spread_parameter" => {
                let mut pc = p.walk();
                let t = p
                    .named_children(&mut pc)
                    .find(|k| k.kind() != "modifiers" && k.kind() != "variable_declarator");
                t.map(|t| format!("{}[]", super_text(t, src)))
            }
            _ => continue,
        };
        out.push(match ty {
            Some(t) if !generic && !t.contains('?') => t,
            _ => "?".to_string(),
        });
    }
    out
}

/// The type parameter names of a declaration.
pub(crate) fn type_params(n: TsNode, src: &[u8]) -> Vec<String> {
    let Some(tp) = n.child_by_field_name("type_parameters") else {
        return Vec::new();
    };
    let mut c = tp.walk();
    let mut out = Vec::new();
    for p in tp.named_children(&mut c) {
        let mut pc = p.walk();
        let id = p
            .named_children(&mut pc)
            .find(|k| k.kind() == "type_identifier");
        out.extend(id.map(|i| text(i, src).to_string()));
    }
    out
}

/// The type facts of one Java file.
pub struct JavaFile {
    pub(crate) rec: JavaFileRec,
    /// A clean type declaration (tree-sitter node id) to its index in `rec`.
    pub(crate) by_node: HashMap<usize, u32>,
    /// The facts of each scope root, by node id. The proof reads this
    /// cache once per call.
    pub(crate) scopes: RefCell<HashMap<usize, std::rc::Rc<crate::java_proof::Scope>>>,
}

impl JavaFile {
    /// Reads the type facts of the file under `root`.
    pub fn new(root: TsNode, source: &str) -> JavaFile {
        let src = source.as_bytes();
        let mut f = JavaFile {
            rec: JavaFileRec {
                pkg: String::new(),
                broken: root.has_error(),
                lombok: false,
                imports: Vec::new(),
                types: Vec::new(),
                bodies: Vec::new(),
            },
            by_node: HashMap::new(),
            scopes: RefCell::new(HashMap::new()),
        };
        let mut c = root.walk();
        for child in root.named_children(&mut c) {
            match child.kind() {
                "package_declaration" => f.rec.pkg = dotted_name(child, src),
                "import_declaration" => f.add_import(child, src),
                _ => f.visit(child, true, src),
            }
        }
        f
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
            rust: None,
            py: None,
            go: None,
            java: Some(JavaRec::File(Box::new(self.rec.clone()))),
            cs: None,
            kt: None,
        }
    }

    fn add_import(&mut self, n: TsNode, src: &[u8]) {
        let mut c = n.walk();
        let mut imp = JavaImport {
            path: String::new(),
            star: false,
            stat: false,
        };
        for k in n.children(&mut c) {
            match k.kind() {
                "static" => imp.stat = true,
                "asterisk" => imp.star = true,
                "identifier" | "scoped_identifier" => {
                    imp.path = text(k, src).split_whitespace().collect();
                }
                _ => {}
            }
        }
        if imp.path == "lombok" || imp.path.starts_with("lombok.") {
            self.rec.lombok = true;
        }
        self.rec.imports.push(imp);
    }

    /// Visits a node that is not a class member. A type declaration is
    /// clean only when `clean` is true. Every other node passes `false` to
    /// its children, so a type below a method body or an initializer is
    /// never clean.
    fn visit(&mut self, n: TsNode, clean: bool, src: &[u8]) {
        match n.kind() {
            k if TYPE_KINDS.contains(&k) => {
                self.add_type(n, None, clean, src);
                return;
            }
            "object_creation_expression" => {
                let mut c = n.walk();
                let body = n.named_children(&mut c).find(|k| k.kind() == "class_body");
                if let Some(body) = body {
                    let sup = n
                        .child_by_field_name("type")
                        .map(|t| last_segment(&super_text(t, src)).to_string());
                    self.rec.bodies.push(JavaBody {
                        name: None,
                        sups: sup.into_iter().collect(),
                        methods: method_names(body, src),
                    });
                }
            }
            _ => {}
        }
        let mut c = n.walk();
        for ch in n.named_children(&mut c) {
            self.visit(ch, false, src);
        }
    }

    /// Records a type declaration. A clean one gets a `JavaType` and its
    /// members. A dirty one gets a `JavaBody`.
    fn add_type(&mut self, n: TsNode, outer: Option<u32>, clean: bool, src: &[u8]) {
        let Some(name) = n.child_by_field_name("name").map(|x| text(x, src)) else {
            return;
        };
        let name = name.to_string();
        let (extends, implements) = supers(n, src);
        let body = n.child_by_field_name("body");
        if !clean {
            let sups = extends
                .iter()
                .chain(implements.iter())
                .map(|s| last_segment(s).to_string())
                .collect();
            self.rec.bodies.push(JavaBody {
                name: Some(name),
                sups,
                methods: body.map(|b| method_names(b, src)).unwrap_or_default(),
            });
            let mut c = n.walk();
            for ch in n.named_children(&mut c) {
                self.visit(ch, false, src);
            }
            return;
        }
        let kind = match n.kind() {
            "interface_declaration" => JavaKind::Interface,
            "enum_declaration" => JavaKind::Enum,
            "record_declaration" => JavaKind::Record,
            "annotation_type_declaration" => JavaKind::Annotation,
            _ => JavaKind::Class,
        };
        let mods = modifiers(n);
        let idx = self.rec.types.len() as u32;
        self.rec.types.push(JavaType {
            name: name.clone(),
            id: String::new(),
            line: n.start_position().row as u32 + 1,
            kind,
            outer,
            public: mods.public,
            vis: if mods.private {
                0
            } else if mods.public
                || outer.is_some_and(|o| {
                    matches!(
                        self.rec.types[o as usize].kind,
                        JavaKind::Interface | JavaKind::Annotation
                    )
                })
            {
                3
            } else if mods.protected {
                2
            } else {
                1
            },
            is_abstract: mods.abs || kind == JavaKind::Interface,
            is_final: mods.fin || kind == JavaKind::Record,
            extends,
            implements,
            tparams: type_params(n, src),
            fields: Vec::new(),
            components: Vec::new(),
            members: Vec::new(),
            methods: Vec::new(),
        });
        self.by_node.insert(n.id(), idx);
        if kind == JavaKind::Record {
            if let Some(params) = n.child_by_field_name("parameters") {
                self.add_components(params, idx, src);
            }
        }
        if let Some(body) = body {
            self.members(body, idx, kind, &name, src);
        }
    }

    fn add_components(&mut self, params: TsNode, idx: u32, src: &[u8]) {
        let mut c = params.walk();
        for p in params.named_children(&mut c) {
            let (name, ty) = match p.kind() {
                "formal_parameter" => {
                    let Some(name) = p.child_by_field_name("name") else {
                        continue;
                    };
                    let ty = p
                        .child_by_field_name("type")
                        .filter(|_| p.child_by_field_name("dimensions").is_none())
                        .and_then(|t| written_simple(t, src));
                    (text(name, src).to_string(), ty)
                }
                "spread_parameter" => {
                    let mut pc = p.walk();
                    let d = p
                        .named_children(&mut pc)
                        .find(|k| k.kind() == "variable_declarator");
                    let Some(name) = d.and_then(|d| d.child_by_field_name("name")) else {
                        continue;
                    };
                    (text(name, src).to_string(), None)
                }
                _ => continue,
            };
            let t = &mut self.rec.types[idx as usize];
            t.components.push(name.clone());
            t.fields.push((name, ty));
        }
    }

    /// Reads the members of a type body. `holder` is the enclosing clean
    /// type. A member type is clean. Every other member passes `false` to
    /// its children.
    fn members(&mut self, body: TsNode, idx: u32, kind: JavaKind, tname: &str, src: &[u8]) {
        let in_iface = matches!(kind, JavaKind::Interface | JavaKind::Annotation);
        let mut c = body.walk();
        for ch in body.named_children(&mut c) {
            match ch.kind() {
                k if TYPE_KINDS.contains(&k) => {
                    self.add_type(ch, Some(idx), true, src);
                    if let (Some(name), Some(&m)) =
                        (ch.child_by_field_name("name"), self.by_node.get(&ch.id()))
                    {
                        let name = text(name, src).to_string();
                        self.rec.types[idx as usize].members.push((name, m));
                    }
                    continue;
                }
                "enum_body_declarations" => {
                    self.members(ch, idx, kind, tname, src);
                    continue;
                }
                "method_declaration" => self.add_method(ch, idx, in_iface, src),
                "field_declaration" | "constant_declaration" => self.add_fields(ch, idx, src),
                "enum_constant" => self.add_constant(ch, idx, tname, src),
                _ => {}
            }
            let mut cc = ch.walk();
            for sub in ch.named_children(&mut cc) {
                self.visit(sub, false, src);
            }
        }
    }

    fn add_method(&mut self, n: TsNode, idx: u32, in_iface: bool, src: &[u8]) {
        let Some(name) = n.child_by_field_name("name") else {
            return;
        };
        let mods = modifiers(n);
        let vis = if mods.private {
            0
        } else if in_iface || mods.public {
            3
        } else if mods.protected {
            2
        } else {
            1
        };
        let no_body = n.child_by_field_name("body").is_none();
        let is_abstract = if in_iface {
            no_body && !mods.stat && !mods.default && !mods.private
        } else {
            mods.abs
        };
        self.rec.types[idx as usize].methods.push(JavaMethod {
            name: text(name, src).to_string(),
            id: String::new(),
            line: n.start_position().row as u32 + 1,
            vis,
            is_static: mods.stat,
            is_abstract,
            is_final: mods.fin,
            params: param_types(n, src),
        });
    }

    fn add_fields(&mut self, n: TsNode, idx: u32, src: &[u8]) {
        let ty = n.child_by_field_name("type");
        let mut c = n.walk();
        let decls: Vec<TsNode> = n.children_by_field_name("declarator", &mut c).collect();
        for d in decls {
            let Some(name) = d.child_by_field_name("name") else {
                continue;
            };
            let written = ty
                .filter(|_| d.child_by_field_name("dimensions").is_none())
                .and_then(|t| written_simple(t, src));
            self.rec.types[idx as usize]
                .fields
                .push((text(name, src).to_string(), written));
        }
    }

    fn add_constant(&mut self, n: TsNode, idx: u32, tname: &str, src: &[u8]) {
        if let Some(name) = n.child_by_field_name("name") {
            self.rec.types[idx as usize]
                .fields
                .push((text(name, src).to_string(), Some(tname.to_string())));
        }
        if let Some(body) = n.child_by_field_name("body") {
            self.rec.bodies.push(JavaBody {
                name: None,
                sups: vec![tname.to_string()],
                methods: method_names(body, src),
            });
        }
    }
}

/// The dotted name below a `package_declaration`.
fn dotted_name(n: TsNode, src: &[u8]) -> String {
    let mut c = n.walk();
    let id = n
        .named_children(&mut c)
        .find(|k| matches!(k.kind(), "scoped_identifier" | "identifier"));
    id.map(|i| text(i, src).split_whitespace().collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn file(source: &str) -> JavaFile {
        let mut p = Parser::new();
        p.set_language(&tree_sitter_java::LANGUAGE.into())
            .expect("java grammar");
        let tree = p.parse(source, None).expect("parse");
        JavaFile::new(tree.root_node(), source)
    }

    #[test]
    fn a_member_type_is_clean_and_a_local_or_anonymous_body_is_not() {
        let f = file(
            "package p;\nimport lombok.Data;\n\
             public class A extends B implements C<D> {\n\
               class In {}\n\
               void m() { class Loc extends E { void z() {} } new F() { void y() {} }; }\n\
               enum K { X { void w() {} }, Y }\n\
             }\n",
        );
        let r = &f.rec;
        assert_eq!(r.pkg, "p");
        assert!(r.lombok);
        let names: Vec<&str> = r.types.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["A", "In", "K"]);
        assert_eq!(r.types[0].extends.as_deref(), Some("B"));
        assert_eq!(r.types[0].implements, ["C"]);
        assert_eq!(r.types[1].outer, Some(0));
        assert_eq!(r.types[2].fields.len(), 2);
        let bodies: Vec<(Option<&str>, &[String], &[String])> = r
            .bodies
            .iter()
            .map(|b| (b.name.as_deref(), &b.sups[..], &b.methods[..]))
            .collect();
        assert!(bodies
            .iter()
            .any(|b| b.0 == Some("Loc") && b.1 == ["E"] && b.2 == ["z"]));
        assert!(bodies
            .iter()
            .any(|b| b.0.is_none() && b.1 == ["F"] && b.2 == ["y"]));
        assert!(bodies
            .iter()
            .any(|b| b.0.is_none() && b.1 == ["K"] && b.2 == ["w"]));
    }

    #[test]
    fn a_method_records_its_visibility_and_abstract_flag() {
        let f = file(
            "interface I { void a(); default void b() {} static void c() {} }\n\
             abstract class C { abstract void d(); protected final void e() {} private void f() {} }\n",
        );
        let m = |t: usize, n: &str| {
            f.rec.types[t]
                .methods
                .iter()
                .find(|x| x.name == n)
                .cloned()
                .expect("method")
        };
        assert!(m(0, "a").is_abstract && m(0, "a").vis == 3);
        assert!(!m(0, "b").is_abstract);
        assert!(m(0, "c").is_static);
        assert!(m(1, "d").is_abstract && m(1, "d").vis == 1);
        assert!(m(1, "e").is_final && m(1, "e").vis == 2);
        assert_eq!(m(1, "f").vis, 0);
    }

    #[test]
    fn a_record_has_components_as_fields() {
        let f = file("record R(int x, Foo y) implements Q {}");
        let t = &f.rec.types[0];
        assert_eq!(t.components, ["x", "y"]);
        assert_eq!(t.fields[1], ("y".to_string(), Some("Foo".to_string())));
        assert!(t.is_final);
    }
}
