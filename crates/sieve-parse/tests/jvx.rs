//! JVX: the exact Java call proof on `tests/fixtures/jvx`. Each proof gives
//! Extracted. Each stop gives no Extracted edge, and JV0 caps the edge that
//! the old name path still finds.

use std::path::PathBuf;

use sieve_core::{Confidence, Relation};
use sieve_parse::build_graph;

fn extracted_calls() -> Vec<(String, String)> {
    extracted_calls_of("jvx")
}

fn extracted_calls_of(fixture: &str) -> Vec<(String, String)> {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let root = PathBuf::from(manifest)
        .join("../../tests/fixtures")
        .join(fixture);
    let graph = build_graph(&root, &root.join("sieve")).expect("build_graph");
    graph
        .edges
        .iter()
        .filter(|e| e.relation == Relation::Calls && e.confidence == Confidence::Extracted)
        .map(|e| (e.source.clone(), e.target.clone()))
        .collect()
}

#[test]
fn test_jvx_fixture_pins_proofs() {
    let got = extracted_calls();
    let has = |s: &str, t: &str| got.iter().any(|(a, b)| a == s && b == t);
    let draw = "q/Widget.java#Widget.draw";
    let widget = "q/Widget.java#Widget";
    let helper = "p/Use.java#Use.helper";
    // JV5: a bare call, and a call in a catch block.
    assert!(has("p/Use.java#Use.bare", helper));
    assert!(has("p/Use.java#Use.viaTry", helper));
    // JV5: a bare call of a method that another file declares.
    assert!(has(
        "p/Subs.java#Sub.inherited",
        "p/Base.java#Base.baseOnly"
    ));
    // JV5: `this.m()`.
    assert!(has("p/Use.java#Use.viaThis", helper));
    // JV4: an own method overrides an interface method of the same signature.
    assert!(has("p/Subs.java#Impl.twice", "p/Subs.java#Impl.greet"));
    // JV6: `super.m()`.
    assert!(has(
        "p/Subs.java#SuperCaller.viaSuper",
        "p/Base.java#Base.sup"
    ));
    // JV7: `Type.m()` with a single-type import.
    assert!(has("p/Use.java#Use.stat", "q/Util.java#Util.twice"));
    // JV8: a local, a `var`, a field, `this.field`, a param, a for-each
    // variable, a typed lambda parameter and a resource block local.
    for src in [
        "local",
        "viaVar",
        "viaField",
        "viaThisField",
        "viaParam",
        "viaLoop",
        "viaLambda",
        "viaTry",
    ] {
        assert!(has(&format!("p/Use.java#Use.{src}"), draw), "{src}");
    }
    // JV9: `new Foo()` reaches the class node, also from a field initializer
    // and through an on-demand import.
    assert!(has("p/Use.java#Use.local", widget));
    assert!(has("p/Use.java#Use", widget));
    assert!(has("p/Use.java#Use.onDemand", "r/Gadget.java#Gadget"));
    // JV2: a member type of the enclosing class.
    assert!(has("p/Use.java#Use.member", "p/Use.java#Use.Item"));
    assert!(has("p/Use.java#Use.member", "p/Use.java#Use.Item.ping"));
    // B3: an enum without a constant body gives a proof for its own method.
    assert!(has(
        "p/Use.java#Use.enumReceiver",
        "p/Use.java#Use.Color.paint"
    ));
}

#[test]
fn test_jvx_fixture_pins_the_b_fix_stops() {
    let got = extracted_calls();
    let has = |s: &str, t: &str| got.iter().any(|(a, b)| a == s && b == t);
    let st = "p/Stops.java#";
    // B1: the union over the chain has 2 methods named m.
    assert!(!has(&format!("{st}B1Sub.f"), &format!("{st}B1Base.m")));
    assert!(!has(&format!("{st}B1Sub.f"), &format!("{st}B1Sub.m")));
    // B2: a private member is not inherited. The call goes to the outer class.
    assert!(!has(
        &format!("{st}B2Outer.Inner.f"),
        &format!("{st}B2Base.m")
    ));
    assert!(has(
        &format!("{st}B2Outer.Inner.f"),
        &format!("{st}B2Outer.m")
    ));
    // B2: a package-private member of another package is not inherited.
    assert!(!has(
        &format!("{st}PkgOuter.In.f"),
        "q/PkgBase.java#PkgBase.hidden"
    ));
    assert!(has(
        &format!("{st}PkgOuter.In.f"),
        &format!("{st}PkgOuter.hidden")
    ));
    // B2: an interface static method is not inherited.
    assert!(!has(&format!("{st}IOuter.In.f"), &format!("{st}IStat.s")));
    assert!(has(&format!("{st}IOuter.In.f"), &format!("{st}IOuter.s")));
    // B3: a record accessor is an implicit member.
    assert!(!has(&format!("{st}B3Outer.R.f"), &format!("{st}B3Outer.x")));
    // B4: an anonymous class, a local class and an enum constant body override.
    assert!(!has(&format!("{st}B4Use.f"), &format!("{st}B4Foo.m")));
    assert!(!has(&format!("{st}B4Local.f"), &format!("{st}B4Foo2.m")));
    assert!(!has(
        &format!("{st}B4EnumUse.f"),
        &format!("{st}B4Enum.run")
    ));
    // B5: an inherited field hides the field of the outer class.
    assert!(!has(
        &format!("{st}B5Outer.Inner.f"),
        &format!("{st}B5Foo.m")
    ));
    assert!(!has(
        &format!("{st}B5Outer.Inner.f"),
        &format!("{st}B5Bar.m")
    ));
    // B5: a static import of a field hides the type of the same name.
    assert!(!has(
        "p/StaticImp.java#StaticImp.f",
        "p/Widget.java#Widget.twice"
    ));
}

#[test]
fn test_jvx_fixture_pins_the_other_stops() {
    let got = extracted_calls();
    let has = |s: &str, t: &str| got.iter().any(|(a, b)| a == s && b == t);
    let st = "p/Stops.java#";
    let draw = "q/Widget.java#Widget.draw";
    // JV10: a subtype overrides the target, the target is abstract, or the
    // receiver is an interface.
    assert!(!has(&format!("{st}Zoo.f"), &format!("{st}Animal.speak")));
    assert!(!has(&format!("{st}Shape.g"), &format!("{st}Shape.area")));
    assert!(!has(&format!("{st}NamedUse.f"), &format!("{st}Named.name")));
    // JV1: 2 binders of one name, a pattern variable, a type parameter.
    assert!(!has(&format!("{st}Binders.two"), draw));
    assert!(!has(&format!("{st}Binders.pattern"), draw));
    assert!(!has(&format!("{st}Binders.typeParam"), draw));
    // JV3: a supertype outside the repo.
    assert!(!has(
        &format!("{st}ChainJdk.g"),
        &format!("{st}ChainJdk.helper")
    ));
    // JV9: a qualified type; 2 files with one full name.
    assert!(!has(&format!("{st}Qualified.f"), "q/Widget.java#Widget"));
    assert!(!has("dup/User.java#User.f", "dup/A.java#A"));
    assert!(!has("dup/User.java#User.f", "dup2/A.java#A"));
}

#[test]
fn test_jvx_inherited_member_type_follows_access() {
    let got = extracted_calls();
    let has = |s: &str, t: &str| got.iter().any(|(a, b)| a == s && b == t);
    // B-a: a private member type of the super class is not inherited, so
    // `Node` is the top-level type of the package.
    assert!(!has("p/Subs.java#Sub.viaNode", "p/Base.java#Base.Node.go"));
    assert!(has("p/Subs.java#Sub.viaNode", "p/Node.java#Node.go"));
}

#[test]
fn test_jvx_review_findings_f1_f2_f4() {
    let got = extracted_calls();
    let has = |s: &str, t: &str| got.iter().any(|(a, b)| a == s && b == t);
    let r = "p/Review.java#";
    // F1: a sub-interface of a subtype declares the default method.
    assert!(!has(&format!("{r}F1Use.f"), &format!("{r}F1I.m")));
    // F2: a class type parameter named `String` is no `java.lang.String`.
    assert!(!has(&format!("{r}F2Sub.f"), &format!("{r}F2Sub.m")));
    assert!(!has(&format!("{r}F2Sub.f"), &format!("{r}F2Base.m")));
    // F4: a covariant return overrides, so the own method wins.
    assert!(has(&format!("{r}F4Sub.f"), &format!("{r}F4Sub.get")));
    // F4: a generic parameter override stops, and the base receiver stops.
    assert!(!has(&format!("{r}F4GenSub.g"), &format!("{r}F4GenSub.put")));
    assert!(!has(&format!("{r}F4GenSub.g"), &format!("{r}F4Gen.put")));
    assert!(!has(&format!("{r}F4Use.h"), &format!("{r}F4Gen.put")));
}

#[test]
fn test_jvx_lombok_or_parse_error_stops_virtual_dispatch_proofs() {
    for fixture in ["jvx_lombok", "jvx_broken"] {
        let got = extracted_calls_of(fixture);
        let has = |s: &str, t: &str| got.iter().any(|(a, b)| a == s && b == t);
        // B-b: a hidden subtype may override `m`.
        assert!(!has("Use.java#Use.virt", "Base.java#Base.m"), "{fixture}");
        // A final target has no override.
        assert!(has("Use.java#Use.fixed", "Base.java#Base.fin"), "{fixture}");
    }
    let got = extracted_calls_of("jvx_lombok");
    assert!(!got.iter().any(|(a, _)| a.starts_with("Lom.java")));
}
