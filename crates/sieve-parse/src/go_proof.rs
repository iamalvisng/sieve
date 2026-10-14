//! The same-file proof for a Go call (a call is Extracted only when
//! exactly one binding is proven). A proof means exactly
//! 1 binding. Any doubt gives `None`, and the call then takes the name path
//! in `resolve.rs`, capped at Inferred.
//!
//! The proof collects every binder of the enclosing top-level declaration.
//! A bare call `f()` holds when the declaration binds no `f` and the file
//! binds `f` once, as a func. A call `x.M()` holds when the declaration
//! binds `x` once, the call sits in the scope of that binding, the binding
//! writes a type `T` or `*T`, and `T` is a plain same-file type with one
//! method `M` and no field `M`.

use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use tree_sitter::Node as TsNode;

use crate::extract::{GoCall, GoRec, GoTy, GoTyFrom};
use crate::go_tree::{text, written_ty, GoFile};

/// The node kinds that end the scope of a local declaration.
const SCOPE_ENDS: &[&str] = &[
    "block",
    "if_statement",
    "for_statement",
    "expression_switch_statement",
    "type_switch_statement",
    "expression_case",
    "default_case",
    "type_case",
    "communication_case",
    "func_literal",
    "function_declaration",
    "method_declaration",
    "source_file",
];

/// The builtin funcs. A bare call to one never needs the package index.
const BUILTINS: &[&str] = &[
    "append", "cap", "clear", "close", "complex", "copy", "delete", "imag", "len", "make", "max",
    "min", "panic", "print", "println", "real", "recover",
];

/// The written type of one binding.
enum Ty {
    /// A written type `T`, `p.T`, from `T`, `*T`, `T{}` or `&T{}`.
    Named(GoTy),
    /// The result of a call to the func `f` or `p.f`.
    Ctor(GoTy),
    /// The type `T` of `new(T)`.
    New(GoTy),
    /// No single written type (a type switch, a range, a receive).
    Unknown,
}

/// The binders of one declaration, grouped by name.
pub(crate) type BinderTable = Rc<HashMap<String, Vec<Binder>>>;

/// One name that a declaration binds, with the byte range where it is live.
pub(crate) struct Binder {
    name: String,
    scope: Range<usize>,
    ty: Ty,
}

impl GoFile {
    /// The binders of the declaration `top`, grouped by name. The first call
    /// for a declaration collects them. Later calls in it reuse the table,
    /// so the work stays linear in the file size.
    fn binders_of(&self, top: TsNode, src: &[u8]) -> BinderTable {
        let mut cache = self.binder_cache.borrow_mut();
        cache
            .entry(top.start_byte())
            .or_insert_with(|| {
                let mut by_name: HashMap<String, Vec<Binder>> = HashMap::new();
                for b in collect(top, src) {
                    by_name.entry(b.name.clone()).or_default().push(b);
                }
                Rc::new(by_name)
            })
            .clone()
    }

    /// The proof for the call `call` of `name`, or `None` when the file
    /// does not prove one same-file target. The record names the target by
    /// start line, and `extract_file` turns it into a node id. A call that
    /// needs other files gets a `Cross` record for the package index.
    pub fn call_rec(&self, call: TsNode, name: &str, source: &str) -> Option<GoRec> {
        if self.stop {
            return None;
        }
        let src = source.as_bytes();
        let func = call.child_by_field_name("function")?;
        let top = top_level(call)?;
        let binders = self.binders_of(top, src);
        let count = |n: &str| binders.get(n).map_or(0, Vec::len);
        let def = |line: u32, method: bool| GoRec::Def {
            name: name.to_string(),
            line,
            method,
        };
        match func.kind() {
            // A bare call.
            "identifier" => {
                if text(func, src) != name || count(name) > 0 || self.aliases.contains(name) {
                    return None;
                }
                match self.names.get(name) {
                    Some(1) => self.funcs.get(name).map(|f| def(f.line, false)),
                    Some(_) => None,
                    None => (!BUILTINS.contains(&name)).then_some(GoRec::Cross(GoCall::Bare)),
                }
            }
            // A call on a name.
            "selector_expression" => {
                let field = func.child_by_field_name("field")?;
                let x = func.child_by_field_name("operand")?;
                if text(field, src) != name || x.kind() != "identifier" {
                    return None;
                }
                let x = text(x, src);
                let mut mine = binders.get(x).into_iter().flatten();
                let Some(b) = mine.next() else {
                    // `x` is no local name, so it may be an import.
                    return Some(GoRec::Cross(GoCall::Qual { pkg: x.to_string() }));
                };
                if !matches!(top.kind(), "function_declaration" | "method_declaration") {
                    return None;
                }
                if mine.next().is_some() || !b.scope.contains(&call.start_byte()) {
                    return None;
                }
                let (ty, from) = match &b.ty {
                    Ty::Named(t) => (t, GoTyFrom::Written),
                    Ty::Ctor(t) => (t, GoTyFrom::Ctor),
                    Ty::New(t) => (t, GoTyFrom::New),
                    Ty::Unknown => return None,
                };
                // A local binding of a type name or qualifier hides it.
                // The same holds for `new` and for a constructor name.
                let hidden = count(&ty.name) > 0
                    || ty.qual.as_deref().is_some_and(|q| count(q) > 0)
                    || (from == GoTyFrom::New && count("new") > 0);
                if hidden {
                    return None;
                }
                if ty.qual.is_none() && from != GoTyFrom::New {
                    if let Some(line) = self.same_file_method(ty, from, name, &count) {
                        return Some(def(line, true));
                    }
                }
                Some(GoRec::Cross(GoCall::Method {
                    ty: ty.clone(),
                    from,
                }))
            }
            _ => None,
        }
    }

    /// The start line of the one method `name` on the type that `ty` gives,
    /// when this file alone proves it.
    fn same_file_method(
        &self,
        ty: &GoTy,
        from: GoTyFrom,
        name: &str,
        count: &dyn Fn(&str) -> usize,
    ) -> Option<u32> {
        let ty = match from {
            GoTyFrom::Ctor => {
                if !self.bound_once(&ty.name) {
                    return None;
                }
                self.funcs.get(&ty.name)?.result.clone()?
            }
            _ => ty.name.clone(),
        };
        if count(&ty) > 0 || !self.bound_once(&ty) {
            return None;
        }
        // An interface, an alias or a generic type never proves.
        let info = self.types.get(&ty).filter(|t| t.plain)?;
        if info.fields.contains(name) {
            return None;
        }
        match self.methods.get(&(ty, name.to_string()))?.as_slice() {
            [line] => Some(*line),
            _ => None,
        }
    }
}

/// The child of `source_file` that holds `n`.
fn top_level(n: TsNode) -> Option<TsNode> {
    let mut cur = n;
    loop {
        let p = cur.parent()?;
        if p.kind() == "source_file" {
            return Some(cur);
        }
        cur = p;
    }
}

/// The end byte of the nearest scope that holds `n`.
fn scope_end(n: TsNode) -> usize {
    let mut cur = n;
    while let Some(p) = cur.parent() {
        if SCOPE_ENDS.contains(&p.kind()) {
            return p.end_byte();
        }
        cur = p;
    }
    n.end_byte()
}

/// The identifiers of an `expression_list` node.
fn idents<'a>(list: TsNode<'a>, src: &[u8]) -> Vec<(TsNode<'a>, String)> {
    let mut c = list.walk();
    list.named_children(&mut c)
        .filter(|i| i.kind() != "comment")
        .map(|i| (i, text(i, src).to_string()))
        .collect()
}

/// The written type of a value expression.
fn value_ty(e: TsNode, src: &[u8]) -> Ty {
    match e.kind() {
        "composite_literal" => match e
            .child_by_field_name("type")
            .and_then(|t| written_ty(t, src))
        {
            Some(t) => Ty::Named(t),
            None => Ty::Unknown,
        },
        "unary_expression" => {
            let amp = e
                .child_by_field_name("operator")
                .is_some_and(|o| o.kind() == "&");
            match e.child_by_field_name("operand") {
                Some(o) if amp && o.kind() == "composite_literal" => value_ty(o, src),
                _ => Ty::Unknown,
            }
        }
        "call_expression" => match e.child_by_field_name("function") {
            Some(f) if f.kind() == "identifier" && text(f, src) == "new" => new_ty(e, src),
            Some(f) if f.kind() == "identifier" => Ty::Ctor(GoTy {
                qual: None,
                name: text(f, src).to_string(),
            }),
            Some(f) if f.kind() == "selector_expression" => {
                match (
                    f.child_by_field_name("operand"),
                    f.child_by_field_name("field"),
                ) {
                    (Some(o), Some(n)) if o.kind() == "identifier" => Ty::Ctor(GoTy {
                        qual: Some(text(o, src).to_string()),
                        name: text(n, src).to_string(),
                    }),
                    _ => Ty::Unknown,
                }
            }
            _ => Ty::Unknown,
        },
        _ => Ty::Unknown,
    }
}

/// The type of `new(T)` or `new(p.T)`: the one argument must be a type name.
fn new_ty(call: TsNode, src: &[u8]) -> Ty {
    let Some(args) = call.child_by_field_name("arguments") else {
        return Ty::Unknown;
    };
    let mut c = args.walk();
    let mut it = args
        .named_children(&mut c)
        .filter(|a| a.kind() != "comment");
    let (Some(a), None) = (it.next(), it.next()) else {
        return Ty::Unknown;
    };
    let ty = match a.kind() {
        "identifier" | "type_identifier" => Some(GoTy {
            qual: None,
            name: text(a, src).to_string(),
        }),
        "selector_expression" | "qualified_type" => {
            let (q, n) = if a.kind() == "qualified_type" {
                ("package", "name")
            } else {
                ("operand", "field")
            };
            match (a.child_by_field_name(q), a.child_by_field_name(n)) {
                (Some(q), Some(n)) if q.kind() != "call_expression" => Some(GoTy {
                    qual: Some(text(q, src).to_string()),
                    name: text(n, src).to_string(),
                }),
                _ => None,
            }
        }
        _ => None,
    };
    ty.map_or(Ty::Unknown, Ty::New)
}

/// The type of the `i`-th name, given the spec's written type and values.
fn spec_ty(ty: Option<TsNode>, vals: &[TsNode], i: usize, n: usize, src: &[u8]) -> Ty {
    if let Some(t) = ty {
        return written_ty(t, src).map_or(Ty::Unknown, Ty::Named);
    }
    if vals.len() == n {
        return value_ty(vals[i], src);
    }
    Ty::Unknown
}

/// Every name that the declaration `top` binds. Each binder node kind of
/// tree-sitter-go has a named arm. A label is a `label_name`, not an
/// `identifier`, so it binds nothing.
fn collect(top: TsNode, src: &[u8]) -> Vec<Binder> {
    let mut out: Vec<Binder> = Vec::new();
    let mut stack = vec![top];
    while let Some(n) = stack.pop() {
        let mut c = n.walk();
        stack.extend(n.named_children(&mut c));
        match n.kind() {
            // Params, named results, the receiver and func literal params.
            "parameter_declaration" | "variadic_parameter_declaration" => {
                let scope = param_scope(n);
                let ty = n.child_by_field_name("type");
                let mut nc = n.walk();
                let names: Vec<TsNode> = n.children_by_field_name("name", &mut nc).collect();
                for id in names {
                    let t = match ty {
                        Some(t) if n.kind() == "parameter_declaration" => {
                            written_ty(t, src).map_or(Ty::Unknown, Ty::Named)
                        }
                        _ => Ty::Unknown,
                    };
                    out.push(Binder {
                        name: text(id, src).to_string(),
                        scope: scope.clone(),
                        ty: t,
                    });
                }
            }
            "type_parameter_declaration" => {
                let mut nc = n.walk();
                let names: Vec<TsNode> = n.children_by_field_name("name", &mut nc).collect();
                for id in names {
                    out.push(Binder {
                        name: text(id, src).to_string(),
                        scope: top.byte_range(),
                        ty: Ty::Unknown,
                    });
                }
            }
            // The type parameters of a generic receiver, `func (l *L[T])`.
            "method_declaration" => {
                if let Some(r) = n.child_by_field_name("receiver") {
                    let mut rs = vec![r];
                    while let Some(x) = rs.pop() {
                        if x.kind() == "type_arguments" {
                            let mut tc = x.walk();
                            let mut ts = vec![x];
                            while let Some(y) = ts.pop() {
                                if y.kind() == "type_identifier" {
                                    out.push(Binder {
                                        name: text(y, src).to_string(),
                                        scope: top.byte_range(),
                                        ty: Ty::Unknown,
                                    });
                                }
                                ts.extend(y.named_children(&mut tc));
                            }
                        } else {
                            let mut xc = x.walk();
                            rs.extend(x.named_children(&mut xc));
                        }
                    }
                }
            }
            "short_var_declaration" => {
                let (Some(l), Some(r)) = (
                    n.child_by_field_name("left"),
                    n.child_by_field_name("right"),
                ) else {
                    continue;
                };
                let mut rc = r.walk();
                let vals: Vec<TsNode> = r
                    .named_children(&mut rc)
                    .filter(|v| v.kind() != "comment")
                    .collect();
                let left = idents(l, src);
                let end = scope_end(n);
                for (i, (id, name)) in left.iter().enumerate() {
                    let ty = if id.kind() == "identifier" {
                        spec_ty(None, &vals, i, left.len(), src)
                    } else {
                        Ty::Unknown
                    };
                    out.push(Binder {
                        name: name.clone(),
                        scope: n.end_byte()..end,
                        ty,
                    });
                }
            }
            // `case x := <-ch:` is a receive_statement.
            "receive_statement" => {
                if let Some(l) = n.child_by_field_name("left") {
                    for (_, name) in idents(l, src) {
                        out.push(Binder {
                            name,
                            scope: n.start_byte()..scope_end(n),
                            ty: Ty::Unknown,
                        });
                    }
                }
            }
            "range_clause" => {
                if let Some(l) = n.child_by_field_name("left") {
                    for (_, name) in idents(l, src) {
                        out.push(Binder {
                            name,
                            scope: n.start_byte()..scope_end(n),
                            ty: Ty::Unknown,
                        });
                    }
                }
            }
            // A type switch var has no single written type.
            "type_switch_statement" => {
                if let Some(l) = n.child_by_field_name("alias") {
                    for (_, name) in idents(l, src) {
                        out.push(Binder {
                            name,
                            scope: n.byte_range(),
                            ty: Ty::Unknown,
                        });
                    }
                }
            }
            "var_spec" | "const_spec" => {
                let mut nc = n.walk();
                let names: Vec<TsNode> = n
                    .children_by_field_name("name", &mut nc)
                    .filter(|i| i.kind() == "identifier")
                    .collect();
                let vals: Vec<TsNode> = match n.child_by_field_name("value") {
                    Some(v) => {
                        let mut vc = v.walk();
                        v.named_children(&mut vc)
                            .filter(|v| v.kind() != "comment")
                            .collect()
                    }
                    None => Vec::new(),
                };
                let typed = n.kind() == "var_spec";
                let ty = n.child_by_field_name("type").filter(|_| typed);
                let end = scope_end(n);
                for (i, id) in names.iter().enumerate() {
                    let t = if typed {
                        spec_ty(ty, &vals, i, names.len(), src)
                    } else {
                        Ty::Unknown
                    };
                    out.push(Binder {
                        name: text(*id, src).to_string(),
                        scope: n.end_byte()..end,
                        ty: t,
                    });
                }
            }
            "type_spec" | "type_alias" => {
                if let Some(id) = n.child_by_field_name("name") {
                    out.push(Binder {
                        name: text(id, src).to_string(),
                        scope: n.start_byte()..scope_end(n),
                        ty: Ty::Unknown,
                    });
                }
            }
            _ => {}
        }
    }
    out
}

/// The byte range where a param is live: the func that owns its list. A
/// param of a func type or an interface method has an empty range.
fn param_scope(param: TsNode) -> Range<usize> {
    let owner = param.parent().and_then(|list| list.parent());
    match owner {
        Some(f)
            if matches!(
                f.kind(),
                "function_declaration" | "method_declaration" | "func_literal"
            ) =>
        {
            f.byte_range()
        }
        _ => 0..0,
    }
}

#[cfg(test)]
mod tests {
    use crate::extract::Extractor;
    use crate::resolve::resolve_edges;
    use sieve_core::{Confidence, Relation};

    /// The `calls` edges of one `a.go` source as `(caller, callee, confidence)`.
    fn calls(src: &str) -> Vec<(String, String, Confidence)> {
        let mut ext = Extractor::new().expect("extractor builds");
        let (nodes, raw) = ext.extract_file("a.go", src, "go");
        resolve_edges(&nodes, &raw)
            .into_iter()
            .filter(|e| e.relation == Relation::Calls)
            .map(|e| (e.source, e.target, e.confidence))
            .collect()
    }

    fn extracted(src: &str, from: &str, to: &str) -> bool {
        calls(src).iter().any(|(s, t, c)| {
            *s == format!("a.go#{from}")
                && *t == format!("a.go#{to}")
                && *c == Confidence::Extracted
        })
    }

    fn none_extracted(src: &str) {
        let got = calls(src);
        assert!(
            got.iter().all(|(_, _, c)| *c != Confidence::Extracted),
            "{got:?}"
        );
    }

    #[test]
    fn test_gox1_bare_call_is_extracted() {
        let src = "package p\nfunc f() {}\nfunc g() { f() }\n";
        assert!(extracted(src, "g", "f"));
    }

    #[test]
    fn test_gox1_receiver_call_is_extracted() {
        let src = "package p\ntype T struct{}\nfunc (t *T) M() {}\nfunc (t *T) N() { t.M() }\n";
        assert!(extracted(src, "T.N", "T.M"));
    }

    #[test]
    fn test_gox1_typed_param_is_extracted() {
        let src = "package p\ntype T struct{}\nfunc (T) M() {}\nfunc g(x *T) { x.M() }\n";
        assert!(extracted(src, "g", "T.M"));
    }

    #[test]
    fn test_gox1_named_result_is_extracted() {
        let src = "package p\ntype T struct{}\nfunc (T) M() {}\nfunc g() (x T) { x.M(); return }\n";
        assert!(extracted(src, "g", "T.M"));
    }

    #[test]
    fn test_gox1_var_literal_and_address_are_extracted() {
        let a = "package p\ntype T struct{}\nfunc (T) M() {}\nfunc g() { var x T; x.M() }\n";
        let b = "package p\ntype T struct{}\nfunc (T) M() {}\nfunc g() { x := T{}; x.M() }\n";
        let c = "package p\ntype T struct{}\nfunc (T) M() {}\nfunc g() { x := &T{}; x.M() }\n";
        for src in [a, b, c] {
            assert!(extracted(src, "g", "T.M"), "{src}");
        }
    }

    #[test]
    fn test_gox1_constructor_is_extracted() {
        let src = "package p\ntype T struct{}\nfunc (T) M() {}\nfunc NewT() *T { return &T{} }\nfunc g() { x := NewT(); x.M() }\n";
        assert!(extracted(src, "g", "T.M"));
        let two = "package p\ntype T struct{}\nfunc (T) M() {}\nfunc NewT() (*T, error) { return nil, nil }\nfunc g() { x, _ := NewT(); x.M() }\n";
        assert!(!extracted(two, "g", "T.M"));
    }

    #[test]
    fn test_gox1_label_binds_nothing() {
        let src = "package p\nfunc f() {}\nfunc g() {\nf:\n\tfor { f(); break f }\n}\n";
        assert!(extracted(src, "g", "f"));
    }

    #[test]
    fn test_gox1_stop_param() {
        none_extracted("package p\nfunc f() {}\nfunc g(f func()) { f() }\n");
        none_extracted("package p\nfunc f() {}\nfunc g(f ...func()) { f() }\n");
    }

    #[test]
    fn test_gox1_stop_named_result() {
        none_extracted("package p\nfunc f() {}\nfunc g() (f func()) { f() ; return }\n");
    }

    #[test]
    fn test_gox1_stop_receiver() {
        none_extracted("package p\nfunc f() {}\ntype T struct{}\nfunc (f T) g() { f() }\n");
    }

    #[test]
    fn test_gox1_stop_type_param() {
        none_extracted("package p\nfunc f() {}\nfunc g[f any]() { f() }\n");
    }

    #[test]
    fn test_gox1_stop_short_var() {
        none_extracted("package p\nfunc f() {}\nfunc g() { f := func() {}; f() }\n");
    }

    #[test]
    fn test_gox1_stop_var_const_type() {
        none_extracted("package p\nfunc f() {}\nfunc g() { var f func(); f() }\n");
        none_extracted("package p\nfunc f() {}\nfunc g() { const f = 1; f() }\n");
        none_extracted("package p\nfunc f() {}\nfunc g() { type f int; f() }\n");
        none_extracted("package p\nfunc f() {}\nfunc g() { type f = int; f() }\n");
    }

    #[test]
    fn test_gox1_stop_range() {
        none_extracted(
            "package p\nfunc f() {}\nfunc g(fs []func()) { for _, f := range fs { f() } }\n",
        );
    }

    #[test]
    fn test_gox1_stop_type_switch_var() {
        none_extracted(
            "package p\nfunc f() {}\nfunc g(v any) { switch f := v.(type) { case func(): f() } }\n",
        );
        // One var, two types, no proof.
        let src = "package p\ntype T struct{}\nfunc (T) M() {}\ntype U struct{}\nfunc (U) M() {}\nfunc g(v any) { switch x := v.(type) { case T: x.M(); case U: x.M() } }\n";
        none_extracted(src);
    }

    #[test]
    fn test_gox1_stop_func_literal_param() {
        none_extracted(
            "package p\nfunc f() {}\nfunc g() { h := func(f func()) { f() }; h(nil) }\n",
        );
    }

    #[test]
    fn test_gox1_stop_receive_statement() {
        // `case f := <-ch` binds f.
        none_extracted(
            "package p\nfunc f() {}\nfunc g(ch chan func()) { select { case f := <-ch: f() } }\n",
        );
    }

    #[test]
    fn test_gox1_stop_dot_import_and_ignore() {
        none_extracted("package p\nimport . \"fmt\"\nfunc f() {}\nfunc g() { f() }\n");
        none_extracted("//go:build ignore\n\npackage p\nfunc f() {}\nfunc g() { f() }\n");
    }

    #[test]
    fn test_gox1_stop_two_bindings() {
        none_extracted("package p\nvar f = func() {}\nfunc g() { f() }\n");
        none_extracted("package p\nfunc f() {}\nfunc f() {}\nfunc g() { f() }\n");
    }

    #[test]
    fn test_gox1_b1_type_param_hides_type() {
        let src = "package p\ntype T struct{}\nfunc (T) M() {}\ntype I interface{ M() }\nfunc g[T I](x T) { x.M() }\n";
        none_extracted(src);
        let recv = "package p\ntype T struct{}\nfunc (T) M() {}\ntype L[T any] struct{}\nfunc (l *L[T]) h(x T) { x.M() }\n";
        none_extracted(recv);
    }

    #[test]
    fn test_gox1_b1_local_type_hides_type() {
        let src = "package p\ntype T struct{}\nfunc (T) M() {}\nfunc h() { type T struct{ M func() }; var x T; x.M() }\n";
        none_extracted(src);
    }

    #[test]
    fn test_gox1_interface_receiver_is_not_extracted() {
        let src = "package p\ntype I interface{ M() }\nfunc g(x I) { x.M() }\n";
        none_extracted(src);
    }

    #[test]
    fn test_gox1_stop_alias_field_and_scope() {
        none_extracted(
            "package p\ntype U struct{}\ntype T = U\nfunc (U) M() {}\nfunc g(x T) { x.M() }\n",
        );
        none_extracted(
            "package p\ntype T struct{ M func() }\nfunc (T) N() {}\nfunc g(x T) { x.M() }\n",
        );
        // The call sits outside the scope of the local `x`.
        let src = "package p\ntype T struct{}\nfunc (T) M() {}\nvar x *T\nfunc g() { if true { x := T{}; _ = x }; x.M() }\n";
        none_extracted(src);
    }

    #[test]
    fn test_gox1_promoted_method_is_not_extracted() {
        let src = "package p\ntype B struct{}\nfunc (B) M() {}\ntype T struct{ B }\nfunc g(x T) { x.M() }\n";
        none_extracted(src);
    }

    const TM: &str =
        "package p\ntype T struct{}\nfunc (T) M() {}\ntype U struct{}\nfunc (U) M() {}\n";

    #[test]
    fn test_gox1_f1_comments_do_not_shift_names() {
        // The comments must not count as names or values.
        let src = format!("{TM}type V struct{{}}\nfunc (V) M() {{}}\nfunc g() {{ x, y, /*c*/ z := /*d*/ T{{}}, U{{}}, V{{}}; y.M(); _, _, _ = x, y, z }}\n");
        assert!(extracted(&src, "g", "U.M"));
        assert!(!extracted(&src, "g", "T.M"));
    }

    #[test]
    fn test_gox1_f2_func_and_method_on_one_line() {
        let src = "package p\ntype T struct{}\nfunc M() {}; func (T) M() {}\nfunc g(x T) { M(); x.M() }\n";
        assert!(extracted(src, "g", "M"));
        assert!(extracted(src, "g", "T.M"));
    }

    #[test]
    fn test_gox1_f4_index_mapping() {
        let src = format!("{TM}func g() {{ x, y := T{{}}, U{{}}; x.M(); y.M() }}\n");
        assert!(extracted(&src, "g", "T.M"));
        assert!(extracted(&src, "g", "U.M"));
        let uneven = format!("{TM}func h() (T, U) {{ return T{{}}, U{{}} }}\nfunc g() {{ x, y := h(); x.M(); y.M() }}\n");
        assert!(!extracted(&uneven, "g", "T.M") && !extracted(&uneven, "g", "U.M"));
    }

    #[test]
    fn test_gox1_f4_init_statement_scope() {
        for init in [
            "if x := mk(); true { x.M() }",
            "for x := mk(); ; { x.M() }",
            "switch x := mk(); true { default: x.M() }",
        ] {
            let inside = format!("{TM}func mk() T {{ return T{{}} }}\nfunc g() {{ {init} }}\n");
            assert!(extracted(&inside, "g", "T.M"), "{inside}");
        }
        // The call sits after the statement, outside the scope of `x`.
        let outside =
            format!("{TM}var x U\nfunc g() {{ if x := mk(); true {{ _ = x }}; x.M() }}\n");
        assert!(!extracted(&outside, "g", "T.M"));
        let outside =
            format!("{TM}var x U\nfunc g() {{ for x := mk(); ; {{ _ = x; break }}; x.M() }}\n");
        assert!(!extracted(&outside, "g", "T.M"));
    }

    #[test]
    fn test_gox1_f4_case_and_block_scope() {
        let inside = format!("{TM}func g(n int) {{ switch n {{ case 1: x := T{{}}; x.M() }} }}\n");
        assert!(extracted(&inside, "g", "T.M"));
        let leak = format!("{TM}var x U\nfunc g(n int) {{ switch n {{ case 1: x := T{{}}; _ = x; case 2: x.M() }} }}\n");
        assert!(!extracted(&leak, "g", "T.M"));
        let var_leak = format!("{TM}var x U\nfunc g() {{ {{ var x T; _ = x }}; x.M() }}\n");
        assert!(!extracted(&var_leak, "g", "T.M"));
    }

    #[test]
    fn test_gox1_f4_closure_rebinds_receiver() {
        let src = format!("{TM}func (t T) N() {{ f := func(t U) {{ t.M() }}; f(U{{}}); t.M() }}\n");
        assert!(!extracted(&src, "T.N", "T.M"));
        assert!(!extracted(&src, "T.N", "U.M"));
    }

    #[test]
    fn test_gox1_f4_embedded_field_named_m() {
        let src = "package p\ntype M struct{}\ntype T struct{ M }\nfunc (T) M() {}\nfunc g(x T) { x.M() }\n";
        assert!(!extracted(src, "g", "T.M"));
    }

    #[test]
    fn test_gox1_f4_local_constructor_shadows() {
        let src = format!("{TM}func NewT() *T {{ return &T{{}} }}\nfunc g() {{ NewT := func() *U {{ return &U{{}} }}; x := NewT(); x.M() }}\n");
        none_extracted(&src);
    }

    #[test]
    fn test_gox1_f4_method_on_value_and_pointer() {
        let src = "package p\ntype T struct{}\nfunc (T) M() {}\nfunc (*T) M() {}\nfunc g(x T) { x.M() }\n";
        assert!(!extracted(src, "g", "T.M"));
    }

    #[test]
    fn test_gox1_f4_plus_build_ignore() {
        none_extracted("// +build ignore\n\npackage p\nfunc f() {}\nfunc g() { f() }\n");
    }
}
