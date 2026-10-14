//! Unit tests of the C# type index.

use super::*;
use crate::cs_tree::CsFile;
use sieve_core::Relation;
use tree_sitter::Parser;

/// The node kinds that get a fake definition id.
const DEF_KINDS: &[&str] = &[
    "class_declaration",
    "interface_declaration",
    "enum_declaration",
    "record_declaration",
    "method_declaration",
];

/// The fake definitions of a tree: `path#Name@line` for each type and
/// method.
fn fake_defs(root: tree_sitter::Node, path: &str, src: &str) -> Vec<(String, usize, usize)> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if DEF_KINDS.contains(&n.kind()) {
            if let Some(name) = n.child_by_field_name("name") {
                let id = format!(
                    "{path}#{}@{}",
                    &src[name.byte_range()],
                    n.start_position().row + 1
                );
                out.push((id, n.start_byte(), n.end_byte()));
            }
        }
        let mut c = n.walk();
        stack.extend(n.named_children(&mut c));
    }
    out
}

/// The result of every call of the files: `(file, name, target)`.
struct Run(Vec<(String, String, Option<String>)>);

impl Run {
    /// The target of the first call of `name` in `file`.
    fn get(&self, file: &str, name: &str) -> Option<String> {
        let hit = self.0.iter().find(|r| r.0 == file && r.1 == name);
        hit.unwrap_or_else(|| panic!("no call {name} in {file}"))
            .2
            .clone()
    }

    /// The targets of every call of `name`, in order.
    fn all(&self, name: &str) -> Vec<Option<String>> {
        self.0
            .iter()
            .filter(|r| r.1 == name)
            .map(|r| r.2.clone())
            .collect()
    }
}

fn run(files: &[(&str, &str)]) -> Run {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_c_sharp::LANGUAGE.into())
        .expect("c# grammar");
    let mut raws = Vec::new();
    let mut missing = Vec::new();
    for (path, src) in files {
        let tree = parser.parse(src, None).expect("parse");
        let defs = fake_defs(tree.root_node(), path, src);
        let f = CsFile::new(tree.root_node(), src, &defs);
        let template = f.file_edge(path);
        raws.push(template.clone());
        for site in f.sites(tree.root_node(), src) {
            let Some(rec) = site.rec else {
                missing.push((path.to_string(), site.name, None));
                continue;
            };
            let mut e = template.clone();
            e.source = format!("{path}#caller");
            e.relation = Relation::Calls;
            e.name = Some(site.name);
            e.cs = Some(rec);
            raws.push(e);
        }
    }
    let ix = CsIndex::build(&raws);
    let mut out: Vec<_> = raws
        .iter()
        .filter_map(|e| match &e.cs {
            Some(CsRec::Cross(c)) => {
                let name = e.name.as_deref()?;
                let got = ix.resolve(&e.file, &e.source, c, name);
                Some((e.file.clone(), name.to_string(), got))
            }
            _ => None,
        })
        .collect();
    out.extend(missing);
    Run(out)
}

fn one(src: &str, name: &str) -> Option<String> {
    run(&[("a.cs", src)]).get("a.cs", name)
}

fn id(file: &str, name: &str, line: u32) -> Option<String> {
    Some(format!("{file}#{name}@{line}"))
}

#[test]
fn a_bare_call_reaches_the_method_of_its_class() {
    let s = "class A {\n void M() {}\n void F() { M(); }\n}";
    assert_eq!(one(s, "M"), id("a.cs", "M", 2));
}

#[test]
fn a_bare_call_reaches_a_method_of_a_base_class_in_another_file() {
    let r = run(&[
        ("b.cs", "class Base {\n protected void B() {}\n}"),
        ("s.cs", "class Sub : Base { void F() { B(); } }"),
    ]);
    assert_eq!(r.get("s.cs", "B"), id("b.cs", "B", 2));
}

#[test]
fn a_bare_call_walks_out_to_the_enclosing_class() {
    let s = "class O {\n void M() {}\n class I { void F() { M(); } }\n}";
    assert_eq!(one(s, "M"), id("a.cs", "M", 2));
    let own = "class O { void M() {} class I { void M() {} void F() { M(); } } }";
    assert_eq!(one(own, "M"), id("a.cs", "M", 1));
}

#[test]
fn two_methods_of_one_name_stop() {
    let s = "class A { void M(int a) {} void M(string a) {} void F() { M(1); } }";
    assert_eq!(one(s, "M"), None);
}

#[test]
fn a_field_a_property_or_an_event_of_the_name_stops() {
    for member in ["int M;", "int M { get; set; }", "event Action M;"] {
        let s = format!("class A {{ void M() {{}} {member} void F() {{ M(); }} }}");
        assert_eq!(one(&s, "M"), None, "{member}");
    }
}

#[test]
fn a_frozen_member_name_stops() {
    let own = "class A { string ToString() => \"\"; void F() { ToString(); } }";
    assert_eq!(one(own, "ToString"), None);
    let exc = "class A : Exception { int Message; void F() { Foo(); } void Foo() {} }";
    assert!(one(exc, "Foo").is_some());
    let clash = "class A : Exception { void Message() {} void F() { Message(); } }";
    assert_eq!(one(clash, "Message"), None);
}

#[test]
fn a_wrong_argument_count_or_an_optional_parameter_stops() {
    assert_eq!(
        one("class A { void M(int a) {} void F() { M(); } }", "M"),
        None
    );
    assert_eq!(
        one("class A { void M(int a = 1) {} void F() { M(); } }", "M"),
        None
    );
    assert_eq!(
        one(
            "class A { void M(params int[] a) {} void F() { M(1); } }",
            "M"
        ),
        None
    );
    assert!(one(
        "class A { void M(int a, int b) {} void F() { M(1, 2); } }",
        "M"
    )
    .is_some());
}

#[test]
fn a_virtual_target_is_exact_only_without_an_override_in_a_subtype() {
    let alone = "class A {\n public virtual void M() {}\n void F() { M(); }\n}";
    assert_eq!(one(alone, "M"), id("a.cs", "M", 2));
    let over = "class A {\n public virtual void M() {}\n void F() { M(); }\n}\n\
                class B : A { public override void M() {} }";
    assert_eq!(one(over, "M"), None);
    let far = "class A { public virtual void M() {} void F() { M(); } }\n\
               class B : A {}\nclass C : B { public new void M() {} }";
    assert_eq!(one(far, "M"), None);
}

#[test]
fn an_abstract_target_is_never_exact() {
    let s = "abstract class A { public abstract void M(); void F() { M(); } }";
    assert_eq!(one(s, "M"), None);
}

#[test]
fn a_sealed_override_a_sealed_class_and_a_struct_need_no_subtype_walk() {
    let sealed_override = "class A { public virtual void M() {} }\n\
        class B : A {\n public sealed override void M() {}\n void F() { M(); }\n}\n\
        class C : B { void M2() {} }";
    assert_eq!(one(sealed_override, "M"), id("a.cs", "M", 3));
    let sealed_class = "sealed class A {\n public virtual void M() {}\n void F() { M(); }\n}";
    assert_eq!(one(sealed_class, "M"), id("a.cs", "M", 2));
    let st = "struct S {\n public void M() {}\n void F() { M(); }\n}";
    assert_eq!(one(st, "M"), id("a.cs", "M", 2));
}

#[test]
fn an_override_replaces_the_base_method_of_the_same_signature() {
    let s = "class B { public virtual void M(int a) {} }\n\
             class D : B {\n public override void M(int a) {}\n void F() { M(1); }\n}";
    assert_eq!(one(s, "M"), id("a.cs", "M", 3));
    let different = "class B { public virtual void M(int a) {} }\n\
                     class D : B { public void M(string a) {} void F() { M(1); } }";
    assert_eq!(one(different, "M"), None);
    let hiding = "class B { public void M(int a) {} }\n\
                  class D : B {\n public new void M(int a) {}\n void F() { M(1); }\n}";
    assert_eq!(one(hiding, "M"), id("a.cs", "M", 3));
    let named = "class B { public void M(Foo a) {} }\n\
                 class D : B { public new void M(Foo a) {} void F() { M(null); } }";
    assert_eq!(one(named, "M"), None);
}

#[test]
fn a_private_base_method_is_not_a_candidate() {
    let r = run(&[
        ("b.cs", "class Base { private void P() {} }"),
        ("s.cs", "class Sub : Base { void F() { P(); } }"),
    ]);
    assert_eq!(r.get("s.cs", "P"), None);
}

#[test]
fn this_m_reaches_the_method_and_an_extension_name_stops_it() {
    let s = "class A {\n void M() {}\n void F() { this.M(); }\n}";
    assert_eq!(one(s, "M"), id("a.cs", "M", 2));
    let ext_decl = "static class Ext { public static void M(this A a, int i) {} }";
    let ext = run(&[
        (
            "a.cs",
            "class A { void M(string s) {} void F() { this.M(\"\"); } }",
        ),
        ("e.cs", ext_decl),
    ]);
    assert_eq!(ext.get("a.cs", "M"), None);
    let bare = run(&[
        (
            "a.cs",
            "class A { void M(string s) {} void F() { M(\"\"); } }",
        ),
        ("e.cs", ext_decl),
    ]);
    assert!(bare.get("a.cs", "M").is_some());
}

#[test]
fn a_frozen_bcl_extension_name_stops_a_member_call() {
    let s = "class A { int Where(int a) => a; void F() { this.Where(1); } }";
    assert_eq!(one(s, "Where"), None);
}

#[test]
fn a_using_of_an_unknown_namespace_stops_a_member_call_but_not_a_bare_call() {
    let src = "using FluentAssertions;\nclass A {\n void M() {}\n void F() { this.M(); M(); }\n}";
    let r = run(&[("a.cs", src)]);
    assert_eq!(r.all("M"), [None, id("a.cs", "M", 3)]);
    let frozen =
        "using System;\nusing System.Linq;\nclass A { void M() {} void F() { this.M(); } }";
    assert!(one(frozen, "M").is_some());
}

#[test]
fn a_parse_error_stops_member_calls_only_when_its_text_has_an_extension_method() {
    let a = "class A {\n void M() {}\n void F() { this.M(); M(); }\n}";
    let plain = run(&[("a.cs", a), ("z.cs", "class Z { void m( }")]);
    assert_eq!(plain.all("M"), [id("a.cs", "M", 2), id("a.cs", "M", 2)]);
    let ext = "static class E { static void X(this A a) { } void m( }";
    let other = run(&[("a.cs", a), ("z.cs", ext)]);
    assert_eq!(other.all("M"), [id("a.cs", "M", 2), id("a.cs", "M", 2)]);
    let named = "static class E { static void M(this A a) { } void m( }";
    let lost = run(&[("a.cs", a), ("z.cs", named)]);
    assert_eq!(lost.all("M"), [None, id("a.cs", "M", 2)]);
    let block = run(&[
        ("a.cs", a),
        ("z.cs", "static class E { extension(A a) { } void m( }"),
    ]);
    assert_eq!(block.all("M"), [None, id("a.cs", "M", 2)]);
}

#[test]
fn an_override_in_the_text_of_a_broken_file_stops_virtual_dispatch() {
    let a = "class A {\n public virtual void V() {}\n void F() { V(); }\n}";
    let plain = run(&[("a.cs", a), ("z.cs", "class Z { void m( }")]);
    assert_eq!(plain.get("a.cs", "V"), id("a.cs", "V", 2));
    let lost = run(&[
        ("a.cs", a),
        (
            "z.cs",
            "class Z : A { public override void V() {} void m( }",
        ),
    ]);
    assert_eq!(lost.get("a.cs", "V"), None);
}

#[test]
fn a_type_name_in_the_text_of_a_broken_file_stops_its_lookup() {
    let r = run(&[
        ("w.cs", "namespace App { class W {} }"),
        (
            "u.cs",
            "namespace App.Inner { class U { void F() { new W(); } } }",
        ),
        ("z.cs", "namespace App.Inner { class W { void m( } }"),
    ]);
    assert_eq!(r.get("u.cs", "W"), None);
}

#[test]
fn a_parse_error_in_a_file_makes_its_types_unknown() {
    let r = run(&[
        ("b.cs", "class Base { public void B() {} int x = ; }"),
        ("s.cs", "class Sub : Base { void F() { B(); } }"),
    ]);
    assert_eq!(r.get("s.cs", "B"), None);
}

#[test]
fn a_local_variable_a_parameter_and_a_var_new_give_a_member_proof() {
    let r = run(&[
        ("w.cs", "class Widget {\n public void Draw() {}\n}"),
        (
            "u.cs",
            "class U {\n void L() { Widget w = new Widget(); w.Draw(); }\n \
             void P(Widget p) { p.Draw(); }\n void V() { var v = new Widget(); v.Draw(); }\n}",
        ),
    ]);
    let want = id("w.cs", "Draw", 2);
    assert_eq!(r.all("Draw"), [want.clone(), want.clone(), want]);
}

#[test]
fn a_private_member_of_another_type_is_not_a_candidate() {
    let s = "class A { void P() {} }\nclass U { void F(A a) { a.P(); } }";
    assert_eq!(one(s, "P"), None);
    let inside = "class A {\n void P() {}\n class N { void F(A a) { a.P(); } }\n}";
    assert_eq!(one(inside, "P"), id("a.cs", "P", 2));
}

#[test]
fn a_protected_member_is_a_candidate_from_a_subtype_only() {
    let s = "class A {\n protected void P() {}\n}\n\
             class S : A { void F(S s) { s.P(); } }\nclass U { void G(A a) { a.P(); } }";
    let r = run(&[("a.cs", s)]);
    assert_eq!(r.all("P"), [id("a.cs", "P", 2), None]);
}

#[test]
fn an_interface_receiver_or_a_static_method_gives_no_member_proof() {
    let iface = "interface I { void M(); }\nclass U { void F(I i) { i.M(); } }";
    assert_eq!(one(iface, "M"), None);
    let stat = "class A { static void M() {} }\nclass U { void F(A a) { a.M(); } }";
    assert_eq!(one(stat, "M"), None);
}

#[test]
fn new_reaches_the_class_node() {
    let r = run(&[
        ("w.cs", "class Widget {\n}\nrecord Rec(int A);"),
        ("u.cs", "class U { void F() { new Widget(); new Rec(1); } }"),
    ]);
    assert_eq!(r.get("u.cs", "Widget"), id("w.cs", "Widget", 1));
    assert_eq!(r.get("u.cs", "Rec"), id("w.cs", "Rec", 3));
}

#[test]
fn new_of_a_struct_an_abstract_class_a_partial_class_or_a_delegate_stops() {
    for decl in [
        "struct W {}",
        "abstract class W {}",
        "partial class W {}",
        "delegate void W();",
        "interface W {}",
        "static class W {}",
    ] {
        let r = run(&[
            ("w.cs", decl),
            ("u.cs", "class U { void F() { new W(); } }"),
        ]);
        assert_eq!(r.get("u.cs", "W"), None, "{decl}");
    }
}

#[test]
fn new_of_a_twin_name_stops() {
    let r = run(&[
        ("w1.cs", "class W {}"),
        ("w2.cs", "class W {}"),
        ("u.cs", "class U { void F() { new W(); } }"),
    ]);
    assert_eq!(r.get("u.cs", "W"), None);
    let generic = run(&[
        ("w1.cs", "class W {}"),
        ("w2.cs", "class W<T> {}"),
        ("u.cs", "class U { void F() { new W<int>(); } }"),
    ]);
    assert_eq!(generic.get("u.cs", "W"), None);
}

#[test]
fn a_type_name_resolves_through_the_enclosing_namespaces_then_the_usings() {
    let r = run(&[
        ("a.cs", "namespace App {\n class Foo {}\n}"),
        ("b.cs", "namespace Lib {\n class Foo {}\n class Bar {}\n}"),
        (
            "c.cs",
            "using Lib;\nnamespace App.Inner { class U { void F() { new Foo(); new Bar(); } } }",
        ),
    ]);
    assert_eq!(r.get("c.cs", "Foo"), id("a.cs", "Foo", 2));
    assert_eq!(r.get("c.cs", "Bar"), id("b.cs", "Bar", 3));
}

#[test]
fn a_using_inside_a_namespace_block_stops_every_type_lookup() {
    let r = run(&[
        ("a.cs", "namespace App { class Foo {} }"),
        ("b.cs", "namespace Lib { class Foo {} }"),
        (
            "c.cs",
            "namespace App.Inner {\n using Lib;\n class U { void F() { new Foo(); } }\n}",
        ),
    ]);
    assert_eq!(r.get("c.cs", "Foo"), None);
}

#[test]
fn two_usings_with_the_same_type_name_an_alias_or_an_extern_alias_stop() {
    let two = run(&[
        ("a.cs", "namespace A { class Foo {} }"),
        ("b.cs", "namespace B { class Foo {} }"),
        (
            "c.cs",
            "using A; using B;\nclass U { void F() { new Foo(); } }",
        ),
    ]);
    assert_eq!(two.get("c.cs", "Foo"), None);
    let alias = run(&[
        ("a.cs", "namespace A { class Foo {} }"),
        (
            "c.cs",
            "using A; using Foo = A.Foo;\nclass U { void F() { new Foo(); } }",
        ),
    ]);
    assert_eq!(alias.get("c.cs", "Foo"), None);
    let ext = run(&[
        ("a.cs", "namespace A { class Foo {} }"),
        (
            "c.cs",
            "extern alias X; using A;\nclass U { void F() { new Foo(); } }",
        ),
    ]);
    assert_eq!(ext.get("c.cs", "Foo"), None);
}

#[test]
fn a_type_found_only_through_a_global_using_of_another_file_stops() {
    let r = run(&[
        ("a.cs", "namespace A { class Foo {} }"),
        ("g.cs", "global using A;"),
        ("c.cs", "class U { void F() { new Foo(); } }"),
        ("d.cs", "using A;\nclass V { void F() { new Foo(); } }"),
    ]);
    assert_eq!(r.get("c.cs", "Foo"), None);
    assert!(r.get("d.cs", "Foo").is_some());
}

#[test]
fn a_type_parameter_hides_a_type_name() {
    let class_tp = "class W {}\nclass U<W> { void F() { new W(); } }";
    assert_eq!(one(class_tp, "W"), None);
}

#[test]
fn a_member_type_of_the_enclosing_class_or_a_base_class_is_found() {
    let own = "class U {\n class Item {}\n void F() { new Item(); }\n}";
    assert_eq!(one(own, "Item"), id("a.cs", "Item", 2));
    let inherited = run(&[
        (
            "b.cs",
            "class Base {\n protected class Item {}\n private class Hidden {}\n}",
        ),
        (
            "u.cs",
            "class U : Base { void F() { new Item(); new Hidden(); } }",
        ),
    ]);
    assert_eq!(inherited.get("u.cs", "Item"), id("b.cs", "Item", 2));
    assert_eq!(inherited.get("u.cs", "Hidden"), None);
}

#[test]
fn a_base_class_outside_the_repo_stops_but_an_interface_does_not() {
    let class = "class W {}\nclass U : Component { void F() { new W(); } }";
    assert_eq!(one(class, "W"), None);
    let iface = "class W {}\nclass U : IDisposable, IFoo<int> { void F() { new W(); } }";
    assert!(one(iface, "W").is_some());
    let exc = "class W {}\nclass U : Exception { void F() { new W(); } }";
    assert!(one(exc, "W").is_some());
}

#[test]
fn partial_parts_merge_and_a_partial_walk_without_a_hit_stops() {
    let r = run(&[
        ("p1.cs", "partial class P {\n void M() {}\n}"),
        ("p2.cs", "partial class P { void F() { M(); } }"),
    ]);
    assert_eq!(r.get("p2.cs", "M"), id("p1.cs", "M", 2));
    let outer = "class O {\n void M() {}\n partial class I { void F() { M(); } }\n}";
    assert_eq!(one(outer, "M"), None);
    let plain = "class O {\n void M() {}\n class I { void F() { M(); } }\n}";
    assert!(one(plain, "M").is_some());
}

#[test]
fn a_partial_method_stops() {
    let s = "partial class P { partial void M(); partial void M() {} void F() { M(); } }";
    assert_eq!(one(s, "M"), None);
}

#[test]
fn a_tainted_pair_is_never_extracted() {
    let s = "class B {\n public virtual void M() {}\n void F(B b, S s) { b.M(); s.M(); }\n}\n\
             sealed class S : B {}\nclass D : B { public override void M() {} }";
    let r = run(&[("a.cs", s)]);
    assert_eq!(r.all("M"), [None, None]);
}

#[test]
fn a_dynamic_file_stops_and_a_primary_constructor_parameter_is_read() {
    let dynamic = "class A { dynamic d; void M() {} void F() { M(); } }";
    assert_eq!(one(dynamic, "M"), None);
    let pc = run(&[
        ("w.cs", "class Widget {\n public void Draw() {}\n}"),
        ("u.cs", "class U(Widget w) { void F() { w.Draw(); } }"),
    ]);
    assert_eq!(pc.get("u.cs", "Draw"), id("w.cs", "Draw", 2));
}

#[test]
fn a_record_adds_implicit_members() {
    let s = "record R(int P) { void F() { Deconstruct(); } void Deconstruct() {} }";
    assert_eq!(one(s, "Deconstruct"), None);
}

#[test]
fn a_generic_base_class_resolves_by_its_simple_name() {
    let r = run(&[
        ("b.cs", "class Base<T> {\n protected void B() {}\n}"),
        ("s.cs", "class Sub : Base<int> { void F() { B(); } }"),
    ]);
    assert_eq!(r.get("s.cs", "B"), id("b.cs", "B", 2));
}

#[test]
fn a_conditional_twin_of_a_type_stops() {
    let s = "#if X\nclass W {}\n#else\nclass W {}\n#endif\nclass U { void F() { new W(); } }";
    assert_eq!(one(s, "W"), None);
}

#[test]
fn a_local_function_named_like_the_call_leaves_the_bare_call_without_a_proof() {
    let s = "class A {\n void M() {}\n void F() { int M(int a) => a; M(1); }\n}";
    let tree = {
        let mut p = Parser::new();
        p.set_language(&tree_sitter_c_sharp::LANGUAGE.into())
            .expect("c# grammar");
        p.parse(s, None).expect("parse")
    };
    let f = CsFile::new(tree.root_node(), s, &[]);
    let site = f
        .sites(tree.root_node(), s)
        .into_iter()
        .find(|x| x.name == "M")
        .expect("site");
    assert!(site.bare && site.rec.is_none());
}

#[test]
fn a_default_interface_method_is_never_extracted() {
    let s = "interface I { void M() {} void F() { M(); this.M(); } }\n\
             class C : I { public void M() {} }";
    let r = run(&[("a.cs", s)]);
    assert_eq!(r.all("M"), [None, None]);
}

#[test]
fn an_alias_of_a_base_type_does_not_hide_an_override() {
    let base = "namespace Lib { public class Base { public virtual void M() {} } }";
    let use_ = "namespace Lib { class Use { void F(Base b) { b.M(); } } }";
    let r = run(&[
        ("b.cs", base),
        (
            "s.cs",
            "using B = Lib.Base;\nnamespace App { class Sub : B { public override void M() {} } }",
        ),
        ("u.cs", use_),
    ]);
    assert_eq!(r.get("u.cs", "M"), None);
    let global = run(&[
        ("b.cs", base),
        ("g.cs", "global using B = Lib.Base;"),
        (
            "s.cs",
            "namespace App { class Sub : B { public override void M() {} } }",
        ),
        ("u.cs", use_),
    ]);
    assert_eq!(global.get("u.cs", "M"), None);
}

#[test]
fn an_alias_of_a_nested_type_in_a_generic_type_keeps_the_last_segment() {
    let lib = "namespace Lib {\n public class Outer<T> {\n  public class Inner { public virtual void M() {} }\n  void F(Inner i) { i.M(); }\n }\n}";
    let sub = "using B = Lib.Outer<int>.Inner;\nnamespace App { class Sub : B { public override void M() {} } }";
    let r = run(&[("l.cs", lib), ("s.cs", sub)]);
    assert_eq!(r.get("l.cs", "M"), None);
    assert_eq!(
        strip_generics("Lib.Outer<int, List<x>>.Inner"),
        "Lib.Outer.Inner"
    );
}

#[test]
fn an_internal_member_of_a_file_with_no_project_stops() {
    let r = run(&[
        (
            "a.cs",
            "public class Lib {\n internal void Imp() {}\n public void Pub() {}\n}",
        ),
        ("b.cs", "class U { void F(Lib l) { l.Imp(); l.Pub(); } }"),
    ]);
    assert_eq!(r.get("b.cs", "Imp"), None);
    assert_eq!(r.get("b.cs", "Pub"), id("a.cs", "Pub", 3));
}

#[test]
fn a_member_hides_a_primary_constructor_parameter_of_the_receiver_name() {
    let own = "class Foo { public void M() {} }\nclass Bar { public void M() {} }\n\
               class A(Foo p) { Bar p = new Bar(); void F() { p.M(); } }";
    assert_eq!(one(own, "M"), None);
    let inherited = "class Foo { public void M() {} }\nclass Bar { public void M() {} }\n\
                     class Base2 { protected Bar p = new Bar(); }\n\
                     class A2(Foo p) : Base2 { void G() { p.M(); } }";
    assert_eq!(one(inherited, "M"), None);
    let free = "class Foo {\n public void M() {}\n}\nclass A(Foo p) { void F() { p.M(); } }";
    assert_eq!(one(free, "M"), id("a.cs", "M", 2));
}

// ---- base.M(), Type.M() and field receivers ----------------------

#[test]
fn base_m_reaches_the_nearest_member_up_the_chain() {
    let src = "class R {\n public virtual void H() {}\n}\nclass M : R {\n public override void H() {}\n}\n\
               class L : M {\n void F() { base.H(); }\n}";
    let r = run(&[("a.cs", src)]);
    let got = r.all("H");
    assert_eq!(got, [id("a.cs", "H", 5)]);
    let root = "class R {\n public virtual void H() {}\n}\nclass M : R { }\n\
                class L : M {\n void F() { base.H(); }\n}";
    assert_eq!(one(root, "H"), id("a.cs", "H", 2));
}

#[test]
fn base_m_is_exact_even_when_a_subtype_overrides() {
    let src = "class R {\n public virtual void H() {}\n}\nclass L : R {\n public override void H() { base.H(); }\n}";
    assert_eq!(one(src, "H"), id("a.cs", "H", 2));
}

#[test]
fn base_m_stops_on_abstract_object_static_and_overload_targets() {
    let abs =
        "abstract class R { public abstract void H(); }\nclass L : R { void F() { base.H(); } }";
    assert_eq!(one(abs, "H"), None);
    let obj = "class L { string F() { return base.ToString(); } }";
    assert_eq!(one(obj, "ToString"), None);
    let outside = "class L : System.ComponentModel.Component { void F() { base.Dispose(); } }";
    assert_eq!(one(outside, "Dispose"), None);
    let over = "class R { public void H(int a) {} public void H(string a) {} }\nclass L : R { void F() { base.H(1); } }";
    assert_eq!(one(over, "H"), None);
    let hide = "class R { public void H() {} }\nclass M : R { public new void H() {} }\nclass L : M { void F() { base.H(); } }";
    // The `new` method of the intermediate class is the nearest member.
    assert_eq!(one(hide, "H"), id("a.cs", "H", 2));
    let private = "class R { private void H() {} }\nclass L : R { void F() { base.H(); } }";
    assert_eq!(one(private, "H"), None);
    let ext = "class R { public void H() {} }\nclass L : R { void F() { base.H(); } }\n\
               static class E { public static void H(this R r) {} }";
    assert_eq!(one(ext, "H"), None);
}

#[test]
fn type_m_reaches_the_one_static_method() {
    let src = "class E {\n public static void B() {}\n}\nclass U { void F() { E.B(); } }";
    assert_eq!(one(src, "B"), id("a.cs", "B", 2));
    let inherited =
        "class E {\n public static void B() {}\n}\nclass D : E { }\nclass U { void F() { D.B(); } }";
    assert_eq!(one(inherited, "B"), id("a.cs", "B", 2));
    let ext = "static class X {\n public static int T(this E e, int n) { return n; }\n}\n\
               class E { }\nclass U { int F(E e) { return X.T(e, 1); } }";
    assert_eq!(one(ext, "T"), id("a.cs", "T", 2));
}

#[test]
fn type_m_stops_on_instance_overload_and_two_extensions() {
    let inst = "class E { public void B() {} }\nclass U { void F() { E.B(); } }";
    assert_eq!(one(inst, "B"), None);
    let over = "class E { public static void B(int a) {} public static void B(string a) {} }\nclass U { void F() { E.B(1); } }";
    assert_eq!(one(over, "B"), None);
    let two = "static class X { public static int T(this E e) { return 1; } public static int T(this E e, int n) { return n; } }\n\
               class E { }\nclass U { int F(E e) { return X.T(e); } }";
    assert_eq!(one(two, "T"), None);
}

#[test]
fn type_m_stops_when_the_name_is_a_member_a_binder_a_namespace_or_a_using_static() {
    let base = "class E {\n public static void B() {}\n}\n";
    let cases = [
        "class U { int E; void F() { E.B(); } }",
        "class U { int E { get; } void F() { E.B(); } }",
        "class U { int E() => 1; void F() { E.B(); } }",
        "class P { protected int E; }\nclass U : P { void F() { E.B(); } }",
        "class U { void F() { var E = 1; E.B(); } }",
        "class U(int E) { void F() { E.B(); } }",
        "class U { void F<E>() { E.B(); } }",
        "namespace N.E { class Z { } }\nclass U { void F() { E.B(); } }",
        "class U { void F() { Unknown.B(); Console.B(); } }",
    ];
    for c in cases {
        assert_eq!(one(&format!("{base}{c}"), "B"), None, "{c}");
    }
    let stat = format!("using static System.Math;\n{base}class U {{ void F() {{ E.B(); }} }}");
    assert_eq!(one(&stat, "B"), None);
}

#[test]
fn type_m_stops_on_an_interface_or_a_twin_type() {
    let iface = "interface E { static void B() {} }\nclass U { void F() { E.B(); } }";
    assert_eq!(one(iface, "B"), None);
    let twin = "namespace A { class E { public static void B() {} } }\nnamespace C { class E { public static void B() {} } }\n\
                namespace D { using A; using C; class U { void F() { E.B(); } } }";
    assert_eq!(one(twin, "B"), None);
}

#[test]
fn a_field_receiver_reaches_the_method_of_its_type() {
    let src = "class W {\n public void D() {}\n}\nclass U {\n W f;\n void A() { f.D(); }\n void B() { this.f.D(); }\n}";
    let r = run(&[("a.cs", src)]);
    let want = id("a.cs", "D", 2);
    assert_eq!(r.all("D"), [want.clone(), want]);
}

#[test]
fn a_field_type_is_read_in_the_part_that_declares_it() {
    let w1 = "namespace N1 { class W {\n public void D() {}\n} }";
    let w2 = "namespace N2 { class W { public void D() {} } }";
    let a = "using N1;\npartial class U { W f; }";
    let b = "using N2;\npartial class U { void A() { f.D(); } }";
    let r = run(&[("w1.cs", w1), ("w2.cs", w2), ("a.cs", a), ("b.cs", b)]);
    assert_eq!(r.get("b.cs", "D"), id("w1.cs", "D", 2));
}

#[test]
fn a_field_receiver_stops_on_a_property_an_inherited_field_or_a_second_member() {
    let w = "class W {\n public void D() {}\n}\n";
    let cases = [
        "class U { W f { get; set; } void A() { f.D(); this.f.D(); } }",
        "class P { protected W f; }\nclass U : P { void A() { f.D(); this.f.D(); } }",
        "class U { W f; W f; void A() { f.D(); } }",
        "class U { W f; void f() { } void A() { f.D(); } }",
        "class U { W f; class In { void A(U u) { f.D(); } } }",
        "class P { public int f; }\nclass U : P { new W f; void A() { f.D(); } }",
    ];
    for c in cases {
        assert_eq!(one(&format!("{w}{c}"), "D"), None, "{c}");
    }
}

#[test]
fn a_field_receiver_stops_on_a_hiding_binder_or_a_primary_constructor_parameter() {
    let w = "class W {\n public void D() {}\n}\nclass V {\n public void D() {}\n}\n";
    // The local `f` of type V wins, so the call must not reach W.D.
    let local = format!("{w}class U {{ W f; void A() {{ V f = new V(); f.D(); }} }}");
    assert_eq!(one(&local, "D"), id("a.cs", "D", 5));
    let pat = format!("{w}class U {{ W f; void A(object o) {{ if (o is W f) {{ f.D(); }} }} }}");
    assert_eq!(one(&pat, "D"), None);
    let pctor = format!("{w}class U(V f) {{ W f; void A() {{ f.D(); }} }}");
    assert_eq!(one(&pctor, "D"), None);
}

#[test]
fn a_field_receiver_stops_on_an_interface_a_type_parameter_or_an_unreadable_type() {
    let w = "class W { public void D() {} }\ninterface I { void D(); }\n";
    for ty in ["I", "T", "N.W", "W?", "W[]", "int"] {
        let src = format!("{w}class U<T> {{ {ty} f; void A() {{ f.D(); }} }}");
        assert_eq!(one(&src, "D"), None, "{ty}");
    }
}

#[test]
fn a_field_receiver_takes_the_extension_and_dispatch_stops_of_a_variable() {
    let w = "class W {\n public void D() {}\n public virtual void V() {}\n}\nclass S : W { public override void V() {} }\n";
    let ext = format!(
        "{w}static class E {{ public static void D(this W w, int a) {{}} }}\nclass U {{ W f; void A() {{ f.D(); }} }}"
    );
    assert_eq!(one(&ext, "D"), None);
    let frozen = format!("{w}class U {{ W f; void A() {{ f.Where(); }} }}");
    assert_eq!(one(&frozen, "Where"), None);
    let virt = format!("{w}class U {{ W f; void A() {{ f.V(); }} }}");
    assert_eq!(one(&virt, "V"), None);
    let unknown = format!("using Pkg.Ext;\n{w}class U {{ W f; void A() {{ f.D(); }} }}");
    assert_eq!(one(&unknown, "D"), None);
}

#[test]
fn a_color_color_field_stops_static_and_instance_calls() {
    let w = "class W {\n public void D() {}\n public static void S() {}\n}\n";
    let stat = format!("{w}class U {{ W W; void A() {{ W.S(); }} }}");
    assert_eq!(one(&stat, "S"), None);
    let inst = format!("{w}class U {{ W W; void A() {{ W.D(); }} }}");
    assert_eq!(one(&inst, "D"), None);
}

#[test]
fn a_partial_class_stops_a_static_call() {
    let src = "class E { public static void B() {} }\npartial class U { void F() { E.B(); } }";
    assert_eq!(one(src, "B"), None);
}

#[test]
fn value_and_field_in_an_accessor_are_not_field_receivers() {
    let w = "class W { public void D() {} }\nclass V { public void D() {} }\n";
    let cases = [
        "class U { W value; V P { set { value.D(); } } }",
        "class U { W value; V P { init { value.D(); } } }",
        "class U { W value; V this[int i] { get { return null; } set { value.D(); } } }",
        "class U { V field; W P { get { field.D(); return null; } } }",
    ];
    for c in cases {
        assert_eq!(one(&format!("{w}{c}"), "D"), None, "{c}");
    }
    let plain = format!("{w}class U {{ W value; void A() {{ value.D(); }} }}");
    assert_eq!(one(&plain, "D"), id("a.cs", "D", 1));
}

#[test]
fn a_primary_constructor_parameter_named_value_or_field_is_hidden_in_an_accessor() {
    let w = "class W { public void D() {} }\nclass V { public void D() {} }\n";
    let cases = [
        "class U(W value) { V P { set { value.D(); } } }",
        "class U(W field) { V P { get { field.D(); return null; } } }",
        "class U { V field; W P => field.D(); }",
    ];
    for c in cases {
        assert_eq!(one(&format!("{w}{c}"), "D"), None, "{c}");
    }
}

#[test]
fn a_partial_base_class_stops_member_calls() {
    let e = "class E { public static void B() {} }\n";
    let bare = "partial class P { }\nclass U : P { void G() { } void F() { G(); } }";
    assert_eq!(one(bare, "G"), None);
    let stat = format!("{e}partial class P {{ }}\nclass U : P {{ void F() {{ E.B(); }} }}");
    assert_eq!(one(&stat, "B"), None);
    let this = "partial class P { }\nclass U : P { void G() { } void F() { this.G(); } }";
    assert_eq!(one(this, "G"), None);
}
