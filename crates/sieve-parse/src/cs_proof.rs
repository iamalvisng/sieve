//! The call-site part of the C# proof (
//! the C# call part). The code reads one call, finds its scope root
//! and its binders, and gives the receiver shape. The index in
//! `cs_index.rs` proves the shape against the types of the repo. Any doubt
//! gives `None`. A bare call without a proof stays Inferred. A member call
//! or a `new` without a proof makes no edge.

use std::ops::Range;
use std::rc::Rc;

use tree_sitter::Node as TsNode;

use crate::cs_tree::{written_simple, CsFile, TYPE_KINDS};
use crate::extract::{CsCall, CsRec, CsShape};
use crate::go_tree::text;

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

/// The node kinds of a member of a type body.
const MEMBER_KINDS: &[&str] = &[
    "method_declaration",
    "constructor_declaration",
    "destructor_declaration",
    "operator_declaration",
    "conversion_operator_declaration",
    "property_declaration",
    "indexer_declaration",
    "event_declaration",
    "field_declaration",
    "event_field_declaration",
];

/// The node kinds that own a parameter list.
const OWNER_KINDS: &[&str] = &[
    "method_declaration",
    "constructor_declaration",
    "destructor_declaration",
    "operator_declaration",
    "conversion_operator_declaration",
    "indexer_declaration",
    "local_function_statement",
    "lambda_expression",
    "anonymous_method_expression",
    "class_declaration",
    "struct_declaration",
    "record_declaration",
    "delegate_declaration",
];

/// One call site that the pass found.
pub struct CsSite {
    /// The byte where the call starts.
    pub at: usize,
    /// The method name, or the type name of a `new` expression.
    pub name: String,
    /// The call is a bare `M()`: it keeps the old path when it has no proof.
    pub bare: bool,
    /// The proof record, when the call site has a shape.
    pub rec: Option<CsRec>,
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

/// The byte range of the declaration that owns a parameter.
fn owner_range(n: TsNode) -> Option<Range<usize>> {
    ancestor(n, OWNER_KINDS).map(|o| o.byte_range())
}

/// The name in the `name` field of `n`.
fn name_field(n: TsNode, src: &[u8]) -> Option<String> {
    let name = n.child_by_field_name("name")?;
    (name.kind() == "identifier").then(|| text(name, src).to_string())
}

/// A binder with no exact scope: any use of the name stops.
fn loose(name: String) -> Binder {
    Binder {
        name,
        scope: None,
        ty: None,
    }
}

/// The names in every `name` field of `n`, as binders with no exact scope.
fn name_fields(n: TsNode, src: &[u8], out: &mut Vec<Binder>) {
    let mut c = n.walk();
    for id in n.children_by_field_name("name", &mut c) {
        if id.kind() == "identifier" {
            out.push(loose(text(id, src).to_string()));
        }
    }
}

/// A parameter of a method, a lambda, a local function or a primary
/// constructor.
fn parameter(n: TsNode, src: &[u8], out: &mut Vec<Binder>) {
    let Some(name) = name_field(n, src) else {
        return;
    };
    let mut c = n.walk();
    let moded = n
        .children(&mut c)
        .filter(|k| k.kind() == "modifier")
        .any(|k| matches!(text(k, src), "ref" | "out" | "in" | "params"));
    let ty = n
        .child_by_field_name("type")
        .filter(|_| !moded)
        .and_then(|t| written_simple(t, src));
    out.push(Binder {
        name,
        scope: owner_range(n),
        ty,
    });
}

/// A `params` array parameter. The grammar puts its name and type in the
/// parameter list itself.
fn params_array(n: TsNode, src: &[u8], out: &mut Vec<Binder>) {
    if let Some(name) = name_field(n, src) {
        out.push(Binder {
            name,
            scope: owner_range(n),
            ty: None,
        });
    }
}

/// The name of a lambda with one bare parameter `a => ..`.
fn implicit_parameter(n: TsNode, src: &[u8], out: &mut Vec<Binder>) {
    out.push(Binder {
        name: text(n, src).to_string(),
        scope: n.parent().map(|p| p.byte_range()),
        ty: None,
    });
}

/// The type of `var x = new Foo(..)`: `Foo`, when the value is a `new`
/// expression with a plain type.
fn new_type(declarator: TsNode, src: &[u8]) -> Option<String> {
    let mut c = declarator.walk();
    let value = declarator
        .named_children(&mut c)
        .find(|k| k.kind() == "object_creation_expression")?;
    written_simple(value.child_by_field_name("type")?, src)
}

/// A variable declarator of a local declaration, a `for` header, a `using`
/// statement or a `fixed` statement. A field declarator gives nothing.
fn variable_declarator(n: TsNode, src: &[u8], out: &mut Vec<Binder>) {
    let Some(decl) = n.parent().filter(|p| p.kind() == "variable_declaration") else {
        return;
    };
    let Some(holder) = decl.parent() else {
        return;
    };
    let end = match holder.kind() {
        "local_declaration_statement" => holder.parent().map(|h| {
            if h.kind() == "switch_section" {
                h.parent().map_or(h.end_byte(), |s| s.end_byte())
            } else {
                h.end_byte()
            }
        }),
        "for_statement" | "using_statement" | "fixed_statement" => Some(holder.end_byte()),
        _ => return,
    };
    let Some(name) = name_field(n, src) else {
        return;
    };
    let ty = match decl.child_by_field_name("type") {
        Some(t) if t.kind() == "implicit_type" => new_type(n, src),
        Some(t) => written_simple(t, src),
        None => None,
    };
    out.push(Binder {
        name,
        scope: end.map(|e| n.end_byte()..e),
        ty,
    });
}

/// The variable of a `foreach` statement. A tuple on the left is a pattern
/// or a declaration expression, and those arms bind it.
fn foreach_statement(n: TsNode, src: &[u8], out: &mut Vec<Binder>) {
    let Some(left) = n.child_by_field_name("left") else {
        return;
    };
    if left.kind() != "identifier" {
        return;
    }
    let ty = n
        .child_by_field_name("type")
        .and_then(|t| written_simple(t, src));
    out.push(Binder {
        name: text(left, src).to_string(),
        scope: n.child_by_field_name("body").map(|b| b.byte_range()),
        ty,
    });
}

/// The variable of a `catch` clause.
fn catch_declaration(n: TsNode, src: &[u8], out: &mut Vec<Binder>) {
    if let Some(name) = name_field(n, src) {
        out.push(Binder {
            name,
            scope: n.parent().map(|p| p.byte_range()),
            ty: None,
        });
    }
}

/// The first identifier child of `n`, as a binder with no exact scope.
fn first_identifier(n: TsNode, src: &[u8], out: &mut Vec<Binder>) {
    let mut c = n.walk();
    let id = n.named_children(&mut c).find(|k| k.kind() == "identifier");
    if let Some(id) = id {
        out.push(loose(text(id, src).to_string()));
    }
}

/// Every name that the subtree `root` binds as a variable. Each
/// kind of binder has its own arm.
pub(crate) fn collect_binders(root: TsNode, src: &[u8]) -> Vec<Binder> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "parameter" => parameter(n, src, &mut out),
            "parameter_list" => params_array(n, src, &mut out),
            "implicit_parameter" => implicit_parameter(n, src, &mut out),
            "variable_declarator" => variable_declarator(n, src, &mut out),
            "foreach_statement" => foreach_statement(n, src, &mut out),
            "catch_declaration" => catch_declaration(n, src, &mut out),
            "declaration_pattern"
            | "recursive_pattern"
            | "var_pattern"
            | "tuple_pattern"
            | "parenthesized_variable_designation"
            | "declaration_expression"
            | "from_clause" => name_fields(n, src, &mut out),
            "let_clause" | "join_clause" | "join_into_clause" => {
                first_identifier(n, src, &mut out);
            }
            "query_expression" => {
                let mut c = n.walk();
                for id in n
                    .named_children(&mut c)
                    .filter(|k| k.kind() == "identifier")
                {
                    out.push(loose(text(id, src).to_string()));
                }
            }
            "local_function_statement" => {
                out.extend(name_field(n, src).map(loose));
            }
            _ => {}
        }
        let mut c = n.walk();
        stack.extend(n.named_children(&mut c));
    }
    out
}

/// The type parameter names that the subtree `root` declares: the type
/// parameters of its method and of its local functions.
fn local_type_params(root: TsNode, src: &[u8]) -> Vec<String> {
    let mut names = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "type_parameter" {
            names.extend(name_field(n, src));
        }
        let mut c = n.walk();
        stack.extend(n.named_children(&mut c));
    }
    names
}

/// The facts of one scope root, read once.
pub(crate) struct Scope {
    binders: Vec<Binder>,
    tparams: Vec<String>,
}

/// Where a call sits: its scope root and the innermost type.
struct Loc<'a> {
    root: TsNode<'a>,
    ty: u32,
}

/// The number of arguments in an argument list.
fn count_args(list: Option<TsNode>) -> u32 {
    list.map_or(0, |l| {
        let mut c = l.walk();
        l.named_children(&mut c)
            .filter(|k| k.kind() == "argument")
            .count() as u32
    })
}

impl CsFile {
    /// The binders and type parameter names of the scope root `root`.
    fn scope_of(&self, root: TsNode, src: &[u8]) -> Rc<Scope> {
        let mut cache = self.scopes.borrow_mut();
        Rc::clone(cache.entry(root.id()).or_insert_with(|| {
            Rc::new(Scope {
                binders: collect_binders(root, src),
                tparams: local_type_params(root, src),
            })
        }))
    }

    /// Finds the scope root and the innermost type of `call`. A call
    /// outside a member body gives `None`.
    fn locate<'a>(&self, call: TsNode<'a>) -> Option<Loc<'a>> {
        let mut root = None;
        let mut cur = call.parent();
        while let Some(p) = cur {
            let k = p.kind();
            if k == "global_statement" {
                return None;
            }
            if MEMBER_KINDS.contains(&k) {
                root = root.or(Some(p));
            } else if TYPE_KINDS.contains(&k) {
                let ty = *self.by_node.get(&p.id())?;
                return Some(Loc { root: root?, ty });
            }
            cur = p.parent();
        }
        None
    }

    /// Reads every call and `new` expression below `root`.
    pub fn sites(&self, root: TsNode, source: &str) -> Vec<CsSite> {
        let mut out = Vec::new();
        let mut stack = vec![root];
        while let Some(n) = stack.pop() {
            if let Some(site) = self.site(n, source) {
                out.push(site);
            }
            let mut c = n.walk();
            let kids: Vec<_> = n.named_children(&mut c).collect();
            stack.extend(kids.into_iter().rev());
        }
        out
    }

    /// The call site that the node `n` is, when it is a call.
    fn site(&self, n: TsNode, source: &str) -> Option<CsSite> {
        let src = source.as_bytes();
        let (name, bare) = match n.kind() {
            "invocation_expression" => {
                let f = n.child_by_field_name("function")?;
                match f.kind() {
                    "identifier" => (text(f, src).to_string(), true),
                    "member_access_expression" => {
                        let nm = f.child_by_field_name("name")?;
                        (nm.kind() == "identifier").then(|| (text(nm, src).to_string(), false))?
                    }
                    _ => return None,
                }
            }
            "object_creation_expression" => {
                let ty = written_simple(n.child_by_field_name("type")?, src)?;
                (ty, false)
            }
            _ => return None,
        };
        if n.prev_sibling().is_some_and(|s| s.kind() == "ERROR") {
            return None;
        }
        let arity = count_args(n.child_by_field_name("arguments"));
        let rec = self.call_rec(n, &name, arity, source);
        Some(CsSite {
            at: n.start_byte(),
            name,
            bare,
            rec,
        })
    }

    /// The proof record for the call node `call` of `name`, or `None` when
    /// the call has no shape the index can prove.
    pub fn call_rec(&self, call: TsNode, name: &str, arity: u32, source: &str) -> Option<CsRec> {
        if self.rec.broken || self.rec.dynamic {
            return None;
        }
        let src = source.as_bytes();
        let loc = self.locate(call)?;
        let scope = self.scope_of(loc.root, src);
        let mut recv = None;
        let shape = match call.kind() {
            "object_creation_expression" => {
                let ty = written_simple(call.child_by_field_name("type")?, src)?;
                (!scope.tparams.contains(&ty)).then_some(CsShape::New { ty })?
            }
            "invocation_expression" => {
                let f = call.child_by_field_name("function")?;
                match f.kind() {
                    "identifier" => self.bare_shape(name, &scope, loc.ty)?,
                    "member_access_expression" => {
                        let (shape, from) = self.receiver_shape(f, &scope, loc.ty, src)?;
                        recv = from;
                        shape
                    }
                    _ => return None,
                }
            }
            _ => return None,
        };
        Some(CsRec::Cross(CsCall {
            shape,
            name: name.to_string(),
            arity,
            ctx: loc.ty,
            recv,
        }))
    }

    /// A bare `M()`. A binder or a primary constructor
    /// parameter named `M` stops.
    fn bare_shape(&self, name: &str, scope: &Scope, ty: u32) -> Option<CsShape> {
        let bound = scope.binders.iter().any(|b| b.name == name);
        let pctor = self.rec.types[ty as usize]
            .pctor
            .iter()
            .any(|(p, _)| p == name);
        (!bound && !pctor).then_some(CsShape::Bare)
    }

    /// The shape of the member access `f` of a method call.
    fn receiver_shape(
        &self,
        f: TsNode,
        scope: &Scope,
        ty: u32,
        src: &[u8],
    ) -> Option<(CsShape, Option<String>)> {
        let Some(o) = f.child_by_field_name("expression") else {
            return (f.child(0)?.kind() == "this").then_some((CsShape::This, None));
        };
        match o.kind() {
            "this" => return Some((CsShape::This, None)),
            "base" => return Some((CsShape::Base, None)),
            "member_access_expression" => return this_field(o, src),
            "identifier" => {}
            _ => return None,
        }
        let x = text(o, src);
        // `value` is an implicit binder in an accessor. `field` is the
        // backing field in a property (C# 14). Both hide a primary
        // constructor parameter too.
        let kinds: &[&str] = match x {
            "value" => &["accessor_declaration"],
            "field" => &["accessor_declaration", "property_declaration"],
            _ => &[],
        };
        if !kinds.is_empty() && ancestor(f, kinds).is_some() {
            return None;
        }
        let at = o.start_byte();
        let mine: Vec<&Binder> = scope.binders.iter().filter(|b| b.name == x).collect();
        let pctor = self.rec.types[ty as usize]
            .pctor
            .iter()
            .find(|(p, _)| p == x);
        let written = if mine.is_empty() {
            // No local name and no primary constructor parameter: a
            // field, a property or a type. The index decides.
            let Some(p) = pctor else {
                let hidden = scope.tparams.iter().any(|t| t == x);
                let name = x.to_string();
                return (!hidden).then_some((CsShape::Name { name }, None));
            };
            p.1.clone()?
        } else {
            if pctor.is_some() || mine.iter().any(|b| b.scope.is_none()) {
                return None;
            }
            let live: Vec<&&Binder> = mine
                .iter()
                .filter(|b| b.scope.as_ref().is_some_and(|s| s.contains(&at)))
                .collect();
            match live.as_slice() {
                [b] => b.ty.clone()?,
                _ => return None,
            }
        };
        let from = (mine.is_empty()).then(|| x.to_string());
        (!scope.tparams.contains(&written)).then_some((CsShape::Var { ty: written }, from))
    }
}

/// The shape of `this.f.M()`: a field `f` of the class.
fn this_field(o: TsNode, src: &[u8]) -> Option<(CsShape, Option<String>)> {
    let base = o.child_by_field_name("expression")?;
    let name = o.child_by_field_name("name")?;
    if base.kind() != "this" || name.kind() != "identifier" {
        return None;
    }
    let name = text(name, src).to_string();
    Some((CsShape::Field { name }, None))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse(source: &str) -> tree_sitter::Tree {
        let mut p = Parser::new();
        p.set_language(&tree_sitter_c_sharp::LANGUAGE.into())
            .expect("c# grammar");
        p.parse(source, None).expect("parse")
    }

    /// The binders of the first method of `source` as `(name, scoped, type)`.
    fn binders(source: &str) -> Vec<(String, bool, Option<String>)> {
        let tree = parse(source);
        let mut stack = vec![tree.root_node()];
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

    fn body(stmts: &str) -> String {
        format!("class A {{ void m(Foo p, int q, ref Bar r) {{ {stmts} }} }}")
    }

    #[test]
    fn a_method_parameter_binds_with_its_type() {
        let s = body("");
        assert_eq!(one(&s, "p"), (true, Some("Foo".to_string())));
        assert_eq!(one(&s, "q").1, None);
        assert_eq!(one(&s, "r").1, None);
        let g = "class A { void m(Foo<X> p) {} }";
        assert_eq!(one(g, "p").1.as_deref(), Some("Foo"));
    }

    #[test]
    fn a_params_array_parameter_binds_without_a_type() {
        assert_eq!(
            one("class A { void m(params Foo[] xs) {} }", "xs"),
            (true, None)
        );
    }

    #[test]
    fn a_lambda_parameter_binds() {
        assert_eq!(
            one(&body("run((Foo f) => f.x());"), "f"),
            (true, Some("Foo".to_string()))
        );
        assert_eq!(one(&body("run(a => a.x());"), "a"), (true, None));
        assert_eq!(one(&body("run((a, b) => a.x());"), "a"), (true, None));
        assert_eq!(
            one(&body("run(delegate (Foo d) { return d; });"), "d"),
            (true, Some("Foo".to_string()))
        );
    }

    #[test]
    fn a_local_function_and_its_parameter_bind() {
        let s = body("int Loc(Foo w) => 1;");
        assert_eq!(one(&s, "Loc"), (false, None));
        assert_eq!(one(&s, "w"), (true, Some("Foo".to_string())));
    }

    #[test]
    fn a_catch_variable_binds() {
        assert_eq!(
            one(&body("try { } catch (Exception ex) { }"), "ex"),
            (true, None)
        );
    }

    #[test]
    fn a_foreach_variable_binds_with_its_type() {
        assert_eq!(
            one(&body("foreach (Foo f in xs) { }"), "f"),
            (true, Some("Foo".to_string()))
        );
        assert_eq!(one(&body("foreach (var f in xs) { }"), "f"), (true, None));
    }

    #[test]
    fn a_foreach_deconstruction_has_no_exact_scope() {
        assert_eq!(
            one(&body("foreach (var (k, v) in xs) { }"), "k"),
            (false, None)
        );
        assert_eq!(
            one(&body("foreach ((int k, int v) in xs) { }"), "v"),
            (false, None)
        );
    }

    #[test]
    fn a_using_local_and_a_using_statement_variable_bind() {
        assert_eq!(
            one(&body("using var r1 = new Foo();"), "r1"),
            (true, Some("Foo".to_string()))
        );
        assert_eq!(
            one(&body("using (Foo r2 = Open()) { }"), "r2"),
            (true, Some("Foo".to_string()))
        );
    }

    #[test]
    fn a_fixed_and_a_for_variable_bind() {
        assert_eq!(one(&body("fixed (int* pp = &q) { }"), "pp"), (true, None));
        assert_eq!(
            one(&body("for (Foo i = null; ; ) { }"), "i"),
            (true, Some("Foo".to_string()))
        );
    }

    #[test]
    fn a_declaration_pattern_variable_has_no_exact_scope() {
        let s = body("if (p is Foo f && q is not Bar b) { }");
        assert_eq!(one(&s, "f"), (false, None));
        assert_eq!(one(&s, "b"), (false, None));
    }

    #[test]
    fn a_switch_pattern_variable_has_no_exact_scope() {
        let s = body("switch (p) { case Foo f when f.x(): break; case { Len: var ln }: break; }");
        assert_eq!(one(&s, "f"), (false, None));
        assert_eq!(one(&s, "ln"), (false, None));
        let arm = body("var z = p switch { Foo f => 1, var v => 2, _ => 3 };");
        assert_eq!(one(&arm, "f"), (false, None));
        assert_eq!(one(&arm, "v"), (false, None));
    }

    #[test]
    fn a_property_and_a_list_pattern_variable_have_no_exact_scope() {
        let s = body("if (p is Foo { P: var pp } rr) { } if (q is [var h, ..]) { }");
        assert_eq!(one(&s, "pp"), (false, None));
        assert_eq!(one(&s, "rr"), (false, None));
        assert_eq!(one(&s, "h"), (false, None));
    }

    #[test]
    fn a_positional_pattern_and_a_designation_have_no_exact_scope() {
        let s = body("if (p is (int a3, var b3) g3) { }");
        assert_eq!(one(&s, "a3"), (false, None));
        assert_eq!(one(&s, "b3"), (false, None));
        assert_eq!(one(&s, "g3"), (false, None));
        let des = body("switch (p) { case var (c3, d3): break; }");
        assert_eq!(one(&des, "c3"), (false, None));
        assert_eq!(one(&des, "d3"), (false, None));
        let tup = body("var (k3, _) = t;");
        assert_eq!(one(&tup, "k3"), (false, None));
    }

    #[test]
    fn an_out_variable_and_a_deconstruction_have_no_exact_scope() {
        let s = body(
            "int.TryParse(s, out int n); Fn(out var o); (int a1, int b1) = t; var (c1, d1) = t;",
        );
        for n in ["n", "o", "a1", "b1", "c1", "d1"] {
            assert_eq!(one(&s, n), (false, None), "{n}");
        }
    }

    #[test]
    fn a_query_variable_has_no_exact_scope() {
        let s = body(
            "var l = from i in xs let j = i join k in ys on i equals k into g select g into h select h;",
        );
        for n in ["i", "j", "k", "g", "h"] {
            assert_eq!(one(&s, n), (false, None), "{n}");
        }
    }

    #[test]
    fn a_local_variable_binds_with_its_written_type() {
        let s = body("Foo<X> a = null, b = null; Foo[] c; int d; Foo? e = null;");
        assert_eq!(one(&s, "a"), (true, Some("Foo".to_string())));
        assert_eq!(one(&s, "b"), (true, Some("Foo".to_string())));
        assert_eq!(one(&s, "c").1, None);
        assert_eq!(one(&s, "d").1, None);
        assert_eq!(one(&s, "e").1, None);
    }

    #[test]
    fn a_var_local_takes_the_type_of_a_new_expression_only() {
        let s =
            body("var a = new Foo<int>(); var b = new Foo { P = 1 }; var c = mk(); var d = new();");
        assert_eq!(one(&s, "a").1.as_deref(), Some("Foo"));
        assert_eq!(one(&s, "b").1.as_deref(), Some("Foo"));
        assert_eq!(one(&s, "c").1, None);
        assert_eq!(one(&s, "d").1, None);
    }

    #[test]
    fn a_local_variable_is_live_until_the_end_of_its_block() {
        let s = body("{ Foo a = null; } a.x();");
        let tree = parse(&s);
        let b = collect_binders(tree.root_node(), s.as_bytes());
        let a = b.iter().find(|x| x.name == "a").expect("binder a");
        let use_at = s.rfind("a.x").expect("use");
        assert!(!a.scope.clone().expect("scope").contains(&use_at));
    }

    /// The first call record of `call_name` in `source`.
    fn rec_of(source: &str, call_name: &str) -> Option<CsRec> {
        let tree = parse(source);
        let f = CsFile::new(tree.root_node(), source, &[]);
        let sites = f.sites(tree.root_node(), source);
        // A broken file can lose the call node, which gives no record too.
        sites.into_iter().find(|s| s.name == call_name)?.rec
    }

    fn shape_of(source: &str, call_name: &str) -> Option<CsShape> {
        match rec_of(source, call_name) {
            Some(CsRec::Cross(c)) => Some(c.shape),
            _ => None,
        }
    }

    #[test]
    fn a_call_shape_follows_the_receiver() {
        let s = "class A { Foo f; void m(Foo p) { bare(); this.t(); p.v(); f.g(); new Foo(); } }";
        assert_eq!(shape_of(s, "bare"), Some(CsShape::Bare));
        assert_eq!(shape_of(s, "t"), Some(CsShape::This));
        assert_eq!(shape_of(s, "v"), Some(CsShape::Var { ty: "Foo".into() }));
        assert_eq!(shape_of(s, "g"), Some(CsShape::Name { name: "f".into() }));
        assert_eq!(shape_of(s, "Foo"), Some(CsShape::New { ty: "Foo".into() }));
    }

    #[test]
    fn a_primary_constructor_parameter_is_a_receiver() {
        let s = "class A(Foo p, int q) { void m() { p.v(); q.w(); } }";
        assert_eq!(shape_of(s, "v"), Some(CsShape::Var { ty: "Foo".into() }));
        assert_eq!(shape_of(s, "w"), None);
    }

    #[test]
    fn base_a_name_and_this_field_receivers_give_their_shapes() {
        let s = "class A { void m() { base.b(); Foo.s(); this.f.t(); this.g.h.u(); } }";
        assert_eq!(shape_of(s, "b"), Some(CsShape::Base));
        assert_eq!(shape_of(s, "s"), Some(CsShape::Name { name: "Foo".into() }));
        assert_eq!(shape_of(s, "t"), Some(CsShape::Field { name: "f".into() }));
        assert_eq!(shape_of(s, "u"), None);
    }

    #[test]
    fn a_type_parameter_or_a_binder_named_like_the_receiver_gives_no_name_shape() {
        let tp = "class A { void m<T>() { T.s(); } }";
        assert_eq!(shape_of(tp, "s"), None);
        let local = "class A { void m(int Foo) { Foo.s(); } }";
        assert_eq!(shape_of(local, "s"), None);
    }

    #[test]
    fn a_binder_named_like_the_bare_call_stops_it() {
        let local = "class A { void m() { int Go(int a) => a; Go(1); } }";
        assert_eq!(shape_of(local, "Go"), None);
        let var = "class A { void m() { Action Go = null; Go(); } }";
        assert_eq!(shape_of(var, "Go"), None);
        let pctor = "class A(Action Go) { void m() { Go(); } }";
        assert_eq!(shape_of(pctor, "Go"), None);
    }

    #[test]
    fn a_receiver_with_two_live_binders_or_a_pattern_binder_gives_no_shape() {
        let two = "class A { void m(Foo p) { foreach (Foo p in xs) { p.v(); } } }";
        assert_eq!(shape_of(two, "v"), None);
        let pat = "class A { void m(Foo p) { if (o is Foo p2 && p2 != null) { } p.v(); if (o is Bar p) { } } }";
        assert_eq!(shape_of(pat, "v"), None);
        let outside = "class A { void m() { { Foo p = null; } p.v(); } }";
        assert_eq!(shape_of(outside, "v"), None);
    }

    #[test]
    fn disjoint_scopes_of_one_name_each_give_a_shape() {
        let s = "class A { void m() { { Foo p = null; p.v(); } { Bar p = null; p.w(); } } }";
        assert_eq!(shape_of(s, "v"), Some(CsShape::Var { ty: "Foo".into() }));
        assert_eq!(shape_of(s, "w"), Some(CsShape::Var { ty: "Bar".into() }));
    }

    #[test]
    fn a_method_type_parameter_hides_a_type_name() {
        let tp = "class A { void m<Foo>(Foo p) { p.v(); new Foo(); } }";
        assert_eq!(shape_of(tp, "v"), None);
        assert_eq!(shape_of(tp, "Foo"), None);
        let local_fn = "class A { void m() { void L<Foo>() { new Foo(); } } }";
        assert_eq!(shape_of(local_fn, "Foo"), None);
    }

    #[test]
    fn a_chain_a_cast_a_null_check_and_a_static_receiver_give_no_shape() {
        let s = "class A<T> { void m(Foo x) { a().b(); a.b.c(); ((Foo)x).d(); x?.e(); x!.f(); T.h(); x.i<int>(); } }";
        for n in ["b", "c", "d", "f"] {
            assert_eq!(shape_of(s, n), None, "{n}");
        }
        let tree = parse(s);
        let f = CsFile::new(tree.root_node(), s, &[]);
        let names: Vec<String> = f
            .sites(tree.root_node(), s)
            .into_iter()
            .map(|x| x.name)
            .collect();
        assert!(!names.iter().any(|n| n == "e" || n == "i"));
    }

    #[test]
    fn a_call_outside_a_member_or_in_a_broken_or_dynamic_file_gives_no_shape() {
        let top = "Console.WriteLine(1); foo();";
        assert_eq!(shape_of(top, "foo"), None);
        let base_args = "record R(int P) : Base(Mk());";
        assert_eq!(shape_of(base_args, "Mk"), None);
        let dynamic = "class A { void m() { dynamic d = 1; go(); } }";
        assert_eq!(shape_of(dynamic, "go"), None);
        let broken = "class A { void m() { go( } }";
        assert_eq!(shape_of(broken, "go"), None);
    }

    #[test]
    fn a_qualified_or_target_typed_new_gives_no_site() {
        let s = "class A { void m() { new A.B(); new(); new int[3]; new global::X(); } }";
        let tree = parse(s);
        let f = CsFile::new(tree.root_node(), s, &[]);
        assert!(f.sites(tree.root_node(), s).is_empty());
    }
}
