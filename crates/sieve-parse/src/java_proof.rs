//! The call-site part of the Java proof (the Java
//! call proof). The code reads one call, finds its scope root and its
//! binders, and gives the receiver shape. The index in `java_index.rs`
//! proves the shape against the types of the repo. Any doubt gives `None`,
//! and the call stays Inferred.
//!
//! A call that sits in an anonymous class, an enum constant body, a local
//! class or a compact constructor gives `None`.

use std::ops::Range;
use std::rc::Rc;

use tree_sitter::Node as TsNode;

use crate::extract::{JavaCall, JavaRec, JavaShape};
use crate::go_tree::text;
use crate::java_tree::{type_params, written_simple, JavaFile, TYPE_KINDS};

/// One name that a declaration binds, with the byte range where it is live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Binder {
    /// The bound name.
    pub name: String,
    /// The bytes where the name is live. `None` when the scope is not
    /// exact (a pattern variable), so any use of the name stops.
    pub scope: Option<Range<usize>>,
    /// The written simple type, when the binder has one.
    pub ty: Option<String>,
}

/// Where a call sits: its scope root and the innermost clean type.
struct Loc<'a> {
    root: TsNode<'a>,
    ty: u32,
}

/// The nearest ancestor of `n` with one of `kinds`.
fn ancestor<'a>(n: TsNode<'a>, kinds: &[&str]) -> Option<TsNode<'a>> {
    let mut cur = n.parent();
    while let Some(p) = cur {
        if kinds.contains(&p.kind()) {
            return Some(p);
        }
        cur = p.parent();
    }
    None
}

/// The byte range of the method, constructor or lambda that owns a parameter.
fn owner_range(n: TsNode) -> Option<Range<usize>> {
    ancestor(
        n,
        &[
            "method_declaration",
            "constructor_declaration",
            "lambda_expression",
        ],
    )
    .map(|o| o.byte_range())
}

/// The name that `n` binds in its `name` field, unless it is `_`.
fn name_field(n: TsNode, src: &[u8]) -> Option<String> {
    let name = n.child_by_field_name("name")?;
    (name.kind() == "identifier").then(|| text(name, src).to_string())
}

/// The written type of a declaration. `var` and an array give `None`.
fn declared_type(ty: Option<TsNode>, has_dims: bool, src: &[u8]) -> Option<String> {
    if has_dims {
        return None;
    }
    let name = written_simple(ty?, src)?;
    (name != "var").then_some(name)
}

/// The type of `var x = new Foo(..)`: `Foo`, when the value is a plain
/// `new` expression with no class body.
fn new_type(value: TsNode, src: &[u8]) -> Option<String> {
    if value.kind() != "object_creation_expression" || value.child(0)?.kind() != "new" {
        return None;
    }
    let mut c = value.walk();
    let has_body = value
        .named_children(&mut c)
        .any(|k| k.kind() == "class_body");
    if has_body {
        return None;
    }
    written_simple(value.child_by_field_name("type")?, src)
}

/// A typed parameter of a method, a constructor or a lambda.
fn formal_parameter(n: TsNode, src: &[u8], out: &mut Vec<Binder>) {
    let Some(name) = name_field(n, src) else {
        return;
    };
    let dims = n.child_by_field_name("dimensions").is_some();
    out.push(Binder {
        name,
        scope: owner_range(n),
        ty: declared_type(n.child_by_field_name("type"), dims, src),
    });
}

/// A varargs parameter. Its type is an array, so it has no type.
fn spread_parameter(n: TsNode, src: &[u8], out: &mut Vec<Binder>) {
    let mut c = n.walk();
    let decl = n
        .named_children(&mut c)
        .find(|k| k.kind() == "variable_declarator");
    if let Some(name) = decl.and_then(|d| name_field(d, src)) {
        out.push(Binder {
            name,
            scope: owner_range(n),
            ty: None,
        });
    }
}

/// The names of a lambda with an untyped list `(a, b) -> ..`.
fn inferred_parameters(n: TsNode, src: &[u8], out: &mut Vec<Binder>) {
    let mut c = n.walk();
    for id in n
        .named_children(&mut c)
        .filter(|k| k.kind() == "identifier")
    {
        out.push(Binder {
            name: text(id, src).to_string(),
            scope: owner_range(n),
            ty: None,
        });
    }
}

/// The name of a lambda with one bare parameter `a -> ..`.
fn lambda_expression(n: TsNode, src: &[u8], out: &mut Vec<Binder>) {
    if let Some(p) = n
        .child_by_field_name("parameters")
        .filter(|p| p.kind() == "identifier")
    {
        out.push(Binder {
            name: text(p, src).to_string(),
            scope: Some(n.byte_range()),
            ty: None,
        });
    }
}

/// The parameter of a `catch` clause.
fn catch_formal_parameter(n: TsNode, src: &[u8], out: &mut Vec<Binder>) {
    if let Some(name) = name_field(n, src) {
        out.push(Binder {
            name,
            scope: n.parent().map(|p| p.byte_range()),
            ty: None,
        });
    }
}

/// The variable of a for-each statement.
fn enhanced_for_statement(n: TsNode, src: &[u8], out: &mut Vec<Binder>) {
    if let Some(name) = name_field(n, src) {
        let dims = n.child_by_field_name("dimensions").is_some();
        out.push(Binder {
            name,
            scope: n.child_by_field_name("body").map(|b| b.byte_range()),
            ty: declared_type(n.child_by_field_name("type"), dims, src),
        });
    }
}

/// A variable of a try-with-resources header. It is live from its own
/// declaration to the end of the try body.
fn resource(n: TsNode, src: &[u8], out: &mut Vec<Binder>) {
    let Some(name) = name_field(n, src) else {
        return;
    };
    let end = ancestor(n, &["try_with_resources_statement"])
        .and_then(|t| t.child_by_field_name("body"))
        .map(|b| b.end_byte());
    let dims = n.child_by_field_name("dimensions").is_some();
    out.push(Binder {
        name,
        scope: end.map(|e| n.end_byte()..e),
        ty: declared_type(n.child_by_field_name("type"), dims, src),
    });
}

/// The pattern variable of `x instanceof Foo f`.
fn instanceof_expression(n: TsNode, src: &[u8], out: &mut Vec<Binder>) {
    if let Some(name) = name_field(n, src) {
        out.push(Binder {
            name,
            scope: None,
            ty: None,
        });
    }
}

/// The identifier child of a type pattern or a record pattern component.
fn pattern_variable(n: TsNode, src: &[u8], out: &mut Vec<Binder>) {
    let mut c = n.walk();
    let id = n
        .named_children(&mut c)
        .filter(|k| k.kind() == "identifier")
        .last();
    if let Some(id) = id {
        out.push(Binder {
            name: text(id, src).to_string(),
            scope: None,
            ty: None,
        });
    }
}

/// A local variable. It is live from its declarator to the end of the
/// block, the switch block or the `for` statement that holds it.
fn local_variable(n: TsNode, src: &[u8], out: &mut Vec<Binder>) {
    let Some(decl) = n
        .parent()
        .filter(|p| p.kind() == "local_variable_declaration")
    else {
        return;
    };
    let Some(name) = name_field(n, src) else {
        return;
    };
    let end = decl.parent().map(|h| match h.kind() {
        "switch_block_statement_group" => h.parent().map_or(h.end_byte(), |s| s.end_byte()),
        _ => h.end_byte(),
    });
    let dims = n.child_by_field_name("dimensions").is_some();
    let mut ty = declared_type(decl.child_by_field_name("type"), dims, src);
    let is_var = decl
        .child_by_field_name("type")
        .is_some_and(|t| text(t, src) == "var");
    if is_var && !dims {
        ty = n
            .child_by_field_name("value")
            .and_then(|v| new_type(v, src));
    }
    out.push(Binder {
        name,
        scope: end.map(|e| n.end_byte()..e),
        ty,
    });
}

/// Every name that the subtree `root` binds as a variable. Each kind
/// of binder has its own arm.
pub(crate) fn collect_binders(root: TsNode, src: &[u8]) -> Vec<Binder> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "formal_parameter" => formal_parameter(n, src, &mut out),
            "spread_parameter" => spread_parameter(n, src, &mut out),
            "inferred_parameters" => inferred_parameters(n, src, &mut out),
            "lambda_expression" => lambda_expression(n, src, &mut out),
            "catch_formal_parameter" => catch_formal_parameter(n, src, &mut out),
            "enhanced_for_statement" => enhanced_for_statement(n, src, &mut out),
            "resource" => resource(n, src, &mut out),
            "instanceof_expression" => instanceof_expression(n, src, &mut out),
            "type_pattern" | "record_pattern_component" => pattern_variable(n, src, &mut out),
            "variable_declarator" => local_variable(n, src, &mut out),
            _ => {}
        }
        let mut c = n.walk();
        stack.extend(n.named_children(&mut c));
    }
    out
}

/// The type names that the scope root `root` hides from the package: the
/// type parameters of its method and the classes declared inside it.
fn local_types(root: TsNode, src: &[u8]) -> Vec<String> {
    let mut names = type_params(root, src);
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if TYPE_KINDS.contains(&n.kind()) {
            names.extend(
                n.child_by_field_name("name")
                    .map(|x| text(x, src).to_string()),
            );
        }
        let mut c = n.walk();
        stack.extend(n.named_children(&mut c));
    }
    names
}

/// The facts of one scope root, read once.
pub(crate) struct Scope {
    binders: Vec<Binder>,
    types: Vec<String>,
}

impl JavaFile {
    /// The binders and hidden type names of the scope root `root`.
    fn scope_of(&self, root: TsNode, src: &[u8]) -> Rc<Scope> {
        let mut cache = self.scopes.borrow_mut();
        Rc::clone(cache.entry(root.id()).or_insert_with(|| {
            Rc::new(Scope {
                binders: collect_binders(root, src),
                types: local_types(root, src),
            })
        }))
    }

    /// Finds the scope root and the innermost clean type of `call`. A call
    /// in an anonymous or local class body gives `None`.
    fn locate<'a>(&self, call: TsNode<'a>) -> Option<Loc<'a>> {
        let mut root = None;
        let mut cur = call.parent();
        while let Some(p) = cur {
            match p.kind() {
                "class_body" => {
                    let owner = p.parent()?;
                    if matches!(owner.kind(), "object_creation_expression" | "enum_constant") {
                        return None;
                    }
                }
                "compact_constructor_declaration" => return None,
                "method_declaration"
                | "constructor_declaration"
                | "static_initializer"
                | "field_declaration"
                | "constant_declaration"
                | "enum_constant" => {
                    root = root.or(Some(p));
                }
                "block" => {
                    let holder = p.parent()?;
                    if matches!(holder.kind(), "class_body" | "enum_body_declarations") {
                        root = root.or(Some(p));
                    }
                }
                k if TYPE_KINDS.contains(&k) => {
                    let ty = *self.by_node.get(&p.id())?;
                    return Some(Loc { root: root?, ty });
                }
                _ => {}
            }
            cur = p.parent();
        }
        None
    }

    /// The proof record for the call node `call` of `name`, or `None` when
    /// the call has no shape the index can prove.
    pub fn call_rec(
        &self,
        call: TsNode,
        name: &str,
        arity: Option<u32>,
        source: &str,
    ) -> Option<JavaRec> {
        if self.rec.broken || self.rec.lombok {
            return None;
        }
        let src = source.as_bytes();
        let loc = self.locate(call)?;
        let shape = match call.kind() {
            "object_creation_expression" => self.new_shape(call, loc.root, src)?,
            "method_invocation" => match call.child_by_field_name("object") {
                None => JavaShape::Bare,
                Some(o) => self.receiver_shape(o, loc.root, src)?,
            },
            _ => return None,
        };
        Some(JavaRec::Cross(JavaCall {
            shape,
            name: name.to_string(),
            arity: arity.unwrap_or(0),
            ctx: loc.ty,
        }))
    }

    /// `new Foo(..)` with a plain type, no class body and no outer
    /// instance.
    fn new_shape(&self, call: TsNode, root: TsNode, src: &[u8]) -> Option<JavaShape> {
        if call.child(0)?.kind() != "new" {
            return None;
        }
        let mut c = call.walk();
        let has_body = call
            .named_children(&mut c)
            .any(|k| k.kind() == "class_body");
        let ty = written_simple(call.child_by_field_name("type")?, src)?;
        let hidden = self.scope_of(root, src).types.contains(&ty);
        (!has_body && !hidden).then_some(JavaShape::New { ty })
    }

    /// The shape of the receiver `o` of a method call.
    fn receiver_shape(&self, o: TsNode, root: TsNode, src: &[u8]) -> Option<JavaShape> {
        match o.kind() {
            "this" => Some(JavaShape::This),
            "super" => Some(JavaShape::Super),
            "field_access" => {
                let inner = o.child_by_field_name("object")?;
                let field = o.child_by_field_name("field")?;
                (inner.kind() == "this" && field.kind() == "identifier").then(|| JavaShape::Field {
                    name: text(field, src).to_string(),
                })
            }
            "identifier" => {
                let x = text(o, src);
                let scope = self.scope_of(root, src);
                let mut mine = scope.binders.iter().filter(|b| b.name == x);
                let Some(b) = mine.next() else {
                    // No local name: a field or a type. A local class of
                    // the same name stops.
                    return (!scope.types.iter().any(|t| t == x)).then(|| JavaShape::Name {
                        name: x.to_string(),
                    });
                };
                // A second binder of the name stops.
                if mine.next().is_some() || !b.scope.as_ref()?.contains(&o.start_byte()) {
                    return None;
                }
                let ty = b.ty.clone()?;
                (!scope.types.contains(&ty)).then_some(JavaShape::Var { ty })
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse(source: &str) -> tree_sitter::Tree {
        let mut p = Parser::new();
        p.set_language(&tree_sitter_java::LANGUAGE.into())
            .expect("java grammar");
        p.parse(source, None).expect("parse")
    }

    /// The binders of the first method of `source` as `(name, scoped, type)`.
    fn binders(source: &str) -> Vec<(String, bool, Option<String>)> {
        let tree = parse(source);
        let root = tree.root_node();
        let mut stack = vec![root];
        let mut method = None;
        while let Some(n) = stack.pop() {
            if n.kind() == "method_declaration" {
                method = Some(n);
            }
            let mut c = n.walk();
            let kids: Vec<_> = n.named_children(&mut c).collect();
            stack.extend(kids.into_iter().rev());
        }
        let method = method.expect("a method");
        collect_binders(method, source.as_bytes())
            .into_iter()
            .map(|b| (b.name, b.scope.is_some(), b.ty))
            .collect()
    }

    fn one(source: &str, name: &str) -> (bool, Option<String>) {
        let all = binders(source);
        let mut hits = all.iter().filter(|b| b.0 == name);
        let b = hits.next().expect("binder");
        assert!(hits.next().is_none(), "one binder of {name}");
        (b.1, b.2.clone())
    }

    #[test]
    fn a_method_parameter_binds_with_its_type() {
        let b = one("class A { void m(Foo<X> p, int q) {} }", "p");
        assert_eq!(b, (true, Some("Foo".to_string())));
        assert_eq!(one("class A { void m(Foo<X> p, int q) {} }", "q").1, None);
    }

    #[test]
    fn a_varargs_parameter_has_no_type() {
        assert_eq!(one("class A { void m(Foo... p) {} }", "p"), (true, None));
    }

    #[test]
    fn a_typed_lambda_parameter_binds() {
        let b = one("class A { void m() { run((Foo f) -> f.x()); } }", "f");
        assert_eq!(b, (true, Some("Foo".to_string())));
    }

    #[test]
    fn an_untyped_lambda_list_binds_without_a_type() {
        assert_eq!(
            one("class A { void m() { run((a, b) -> a.x()); } }", "a"),
            (true, None)
        );
    }

    #[test]
    fn a_bare_lambda_parameter_binds_without_a_type() {
        assert_eq!(
            one("class A { void m() { run(a -> a.x()); } }", "a"),
            (true, None)
        );
    }

    #[test]
    fn a_catch_parameter_binds() {
        assert_eq!(
            one("class A { void m() { try { } catch (E | F e) { } } }", "e"),
            (true, None)
        );
    }

    #[test]
    fn a_for_each_variable_binds_with_its_type() {
        let b = one("class A { void m() { for (Foo f : xs) { } } }", "f");
        assert_eq!(b, (true, Some("Foo".to_string())));
        assert_eq!(
            one("class A { void m() { for (var f : xs) { } } }", "f").1,
            None
        );
    }

    #[test]
    fn a_try_resource_binds_with_its_type() {
        let b = one("class A { void m() { try (Foo r = open()) { } } }", "r");
        assert_eq!(b, (true, Some("Foo".to_string())));
    }

    #[test]
    fn an_instanceof_pattern_variable_has_no_exact_scope() {
        assert_eq!(
            one(
                "class A { void m(Object o) { if (o instanceof Foo f) { } } }",
                "f"
            ),
            (false, None)
        );
    }

    #[test]
    fn a_switch_type_pattern_variable_has_no_exact_scope() {
        let s = "class A { void m(Object o) { switch (o) { case Foo f -> { } default -> { } } } }";
        assert_eq!(one(s, "f"), (false, None));
    }

    #[test]
    fn a_record_pattern_component_has_no_exact_scope() {
        let s = "class A { void m(Object o) { if (o instanceof P(int x, Foo y)) { } } }";
        assert_eq!(one(s, "x"), (false, None));
        assert_eq!(one(s, "y"), (false, None));
    }

    #[test]
    fn a_local_variable_binds_with_its_written_type() {
        let s = "class A { void m() { Foo<X> a = null, b = null; Foo[] c; int d; } }";
        assert_eq!(one(s, "a"), (true, Some("Foo".to_string())));
        assert_eq!(one(s, "b"), (true, Some("Foo".to_string())));
        assert_eq!(one(s, "c").1, None);
        assert_eq!(one(s, "d").1, None);
    }

    #[test]
    fn a_var_local_takes_the_type_of_a_plain_new_only() {
        let s =
            "class A { void m() { var a = new Foo<>(); var b = new Foo() { }; var c = mk(); } }";
        assert_eq!(one(s, "a").1.as_deref(), Some("Foo"));
        assert_eq!(one(s, "b").1, None);
        assert_eq!(one(s, "c").1, None);
    }

    #[test]
    fn a_local_variable_is_live_until_the_end_of_its_block() {
        let s = "class A { void m() { { Foo a = null; } a.x(); } }";
        let tree = parse(s);
        let b = collect_binders(tree.root_node(), s.as_bytes());
        let scope = b[0].scope.clone().expect("scope");
        let use_at = s.rfind("a.x").expect("use");
        assert!(!scope.contains(&use_at));
    }

    fn rec_of(source: &str, call_name: &str) -> Option<JavaRec> {
        let tree = parse(source);
        let f = JavaFile::new(tree.root_node(), source);
        let mut stack = vec![tree.root_node()];
        while let Some(n) = stack.pop() {
            let named = n
                .child_by_field_name("name")
                .is_some_and(|x| text(x, source.as_bytes()) == call_name);
            if n.kind() == "method_invocation" && named {
                return f.call_rec(n, call_name, Some(0), source);
            }
            let mut c = n.walk();
            stack.extend(n.named_children(&mut c));
        }
        panic!("no call {call_name}");
    }

    fn shape_of(source: &str, call_name: &str) -> Option<JavaShape> {
        match rec_of(source, call_name) {
            Some(JavaRec::Cross(c)) => Some(c.shape),
            _ => None,
        }
    }

    #[test]
    fn a_call_shape_follows_the_receiver() {
        let s = "class A { Foo f; void m(Foo p) { bare(); this.t(); super.s(); p.v(); f.g(); this.f.h(); T.st(); } }";
        assert_eq!(shape_of(s, "bare"), Some(JavaShape::Bare));
        assert_eq!(shape_of(s, "t"), Some(JavaShape::This));
        assert_eq!(shape_of(s, "s"), Some(JavaShape::Super));
        assert_eq!(shape_of(s, "v"), Some(JavaShape::Var { ty: "Foo".into() }));
        assert_eq!(shape_of(s, "g"), Some(JavaShape::Name { name: "f".into() }));
        assert_eq!(
            shape_of(s, "h"),
            Some(JavaShape::Field { name: "f".into() })
        );
        assert_eq!(
            shape_of(s, "st"),
            Some(JavaShape::Name { name: "T".into() })
        );
    }

    #[test]
    fn a_call_in_an_anonymous_or_local_class_gives_no_shape() {
        let anon = "class A { void m() { new B() { void z() { go(); } }; } }";
        assert_eq!(shape_of(anon, "go"), None);
        let local = "class A { void m() { class L { void z() { go(); } } } }";
        assert_eq!(shape_of(local, "go"), None);
        let constant = "enum E { X { void z() { go(); } } }";
        assert_eq!(shape_of(constant, "go"), None);
    }

    #[test]
    fn a_receiver_with_two_binders_or_a_type_parameter_gives_no_shape() {
        let two = "class A { void m(Foo p) { for (Foo p : xs) { } p.v(); } }";
        assert_eq!(shape_of(two, "v"), None);
        let tp = "class A { <T> void m(T p) { p.v(); } }";
        assert_eq!(shape_of(tp, "v"), None);
        let outside = "class A { Foo p; void m() { { Foo p = null; } p.v(); } }";
        assert_eq!(shape_of(outside, "v"), None);
        let local_class = "class A { void m() { class L { } L.st(); } }";
        assert_eq!(shape_of(local_class, "st"), None);
    }

    #[test]
    fn a_chain_receiver_gives_no_shape() {
        let s = "class A { void m() { a().b(); x.y.z(); } }";
        assert_eq!(shape_of(s, "b"), None);
        assert_eq!(shape_of(s, "z"), None);
    }
}
