//! The exact C# call proof on `tests/fixtures/csx`. Each proof gives
//! Extracted. Each stop gives no Extracted edge, and the cap keeps the edge
//! that the old name path still finds at Inferred.

use std::path::PathBuf;

use sieve_core::{Confidence, Relation};
use sieve_parse::build_graph;

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

/// Reports whether `got` holds an Extracted edge from `s` to `t`.
fn has(got: &[(String, String)], s: &str, t: &str) -> bool {
    got.iter().any(|(a, b)| a == s && b == t)
}

/// Reports whether `got` holds an Extracted edge from `s`.
fn from(got: &[(String, String)], s: &str) -> bool {
    got.iter().any(|(a, _)| a == s)
}

#[test]
fn test_csx_fixture_pins_proofs() {
    let got = extracted_calls_of("csx");
    let p = "Proofs.cs#";
    let draw = "Proofs.cs#Draw";
    let widget = "Proofs.cs#Widget";
    // A bare call reaches a method of the class, an inherited one, an
    // outer class, a struct, a generic base and a record.
    assert!(has(&got, "Proofs.cs#ViaCatch", "Proofs.cs#Local"));
    assert!(has(&got, "Proofs.cs#CallsBare", "Proofs.cs#BaseHelper"));
    assert!(has(&got, "Proofs.cs#CallsBare", "Proofs.cs#ProtHelper"));
    assert!(has(&got, "Proofs.cs#InnerCaller", "Proofs.cs#OuterMethod"));
    assert!(has(&got, "Proofs.cs#StructCaller", "Proofs.cs#Move"));
    assert!(has(
        &got,
        "Proofs.cs#GenericCaller",
        "Proofs.cs#GenericBaseMethod"
    ));
    assert!(has(&got, "Proofs.cs#RecFirst", "Proofs.cs#RecSecond"));
    // An interface outside the repo adds no class member.
    assert!(has(&got, "Proofs.cs#IfaceCaller", "Proofs.cs#IfaceTarget"));
    // A virtual method that no subtype overrides, an override that
    // replaces its base method, and a sealed class.
    assert!(has(&got, "Proofs.cs#CallsBare", "Proofs.cs#VirtUnchanged"));
    assert!(has(
        &got,
        "Proofs.cs#CallsBare",
        "Proofs.cs#VirtOverriddenBelow~2"
    ));
    assert!(has(&got, "Proofs.cs#GadgetCaller", "Proofs.cs#OpenFree~2"));
    // A bare call is no extension call.
    assert!(has(&got, "Proofs.cs#BareShadowed", "Proofs.cs#Shadowed"));
    assert!(has(&got, "Proofs.cs#BareWhere", "Proofs.cs#Where"));
    // `this.M()`, and a primary constructor parameter, a parameter, a
    // typed local, a `var` local, a foreach variable, a lambda parameter and
    // a `using` local.
    assert!(has(&got, "Proofs.cs#ViaThis", "Proofs.cs#Local"));
    for src in [
        "ViaCtorParam",
        "ViaParam",
        "ViaLocal",
        "ViaVar",
        "ViaForeach",
        "ViaLambda",
        "ViaUsing",
    ] {
        assert!(has(&got, &format!("{p}{src}"), draw), "{src}");
    }
    assert!(has(&got, "Proofs.cs#ViaLambda", "Proofs.cs#Run"));
    // `new T()` reaches the class node.
    for src in ["ViaLocal", "ViaVar", "ViaUsing"] {
        assert!(has(&got, &format!("{p}{src}"), widget), "{src}");
    }
    // A `using`, an enclosing namespace before a `using`, and a
    // partial type that merges its parts.
    assert!(has(&got, "Cross.cs#ViaUsing", "Other.cs#Gizmo"));
    assert!(has(&got, "Cross.cs#ViaUsing", "Other.cs#Spin"));
    assert!(has(&got, "Cross.cs#ViaEnclosing", draw));
    assert!(has(&got, "Cross.cs#ViaOrder", "Other.cs#WhichCsx"));
    assert!(!has(&got, "Cross.cs#ViaOrder", "Other.cs#WhichOther"));
    assert!(has(&got, "PartA.cs#FromA", "PartB.cs#FromB"));
}

#[test]
fn test_csx_fixture_pins_the_member_stops() {
    let got = extracted_calls_of("csx");
    // 2 methods of one name, a field of the name, an optional
    // parameter, a `params` parameter, a wrong count, a partial method.
    for src in ["CallOv", "CallHid", "CallOpt", "CallPar", "CallArity"] {
        assert!(!from(&got, &format!("Stops.cs#{src}")), "{src}");
    }
    assert!(!from(&got, "PartA.cs#CallHook"));
    // A private base method is no candidate.
    assert!(!from(&got, "Stops.cs#CallPrivOnly"));
    // An abstract target, a subtype override, a deep override.
    for src in ["CallAbsent", "CallOver", "CallDeepOver"] {
        assert!(!from(&got, &format!("Stops.cs#{src}")), "{src}");
    }
    // An interface receiver.
    assert!(!from(&got, "Stops.cs#CallGo"));
    // A local function, a pattern variable, an out variable, a
    // deconstruction and a type parameter.
    let draw = "Proofs.cs#Draw";
    for src in [
        "LocalFn",
        "PatternVar",
        "OutVar",
        "Deconstructed",
        "TypeParam",
    ] {
        assert!(!has(&got, &format!("Stops.cs#{src}"), draw), "{src}");
        assert!(
            !has(&got, &format!("Stops.cs#{src}"), "Stops.cs#Shadow"),
            "{src}"
        );
    }
    // A qualified field type, a qualified `Type.M()`, a call result and
    // `?.` give no proof.
    for src in ["ViaField", "ViaStatic", "ViaNullCheck"] {
        assert!(!from(&got, &format!("Stops.cs#{src}")), "{src}");
    }
    assert!(!has(&got, "Stops.cs#ViaChain", draw));
    // An in-repo extension name, a frozen extension name, a `using` of
    // an unknown namespace. The bare call stays proven.
    assert!(!from(&got, "Stops.cs#CallShadowed"));
    assert!(!from(&got, "Stops.cs#CallWhere"));
    assert!(!from(&got, "Scopes.cs#CallHelperThis"));
    assert!(!from(&got, "Scopes.cs#CallDraw"));
    assert!(has(&got, "Scopes.cs#CallHelperBare", "Scopes.cs#Helper"));
    // A base class outside the repo.
    assert!(!from(&got, "Stops.cs#CallInside"));
    // A file that names `dynamic` proves no call.
    assert!(!from(&got, "Dyn.cs#CallDynHelper"));
    // A partial type without the name stops the walk.
    assert!(!from(&got, "PartA.cs#InnerF"));
}

#[test]
fn test_csx_fixture_pins_the_new_and_name_stops() {
    let got = extracted_calls_of("csx");
    // A struct, a partial class, an array, a qualified name, a
    // target-typed `new` and a delegate.
    for src in [
        "NewStruct",
        "NewPartial",
        "NewArray",
        "NewQualified",
        "NewTargetTyped",
        "NewDelegate",
    ] {
        assert!(!from(&got, &format!("Stops.cs#{src}")), "{src}");
    }
    // 2 usings with one name, a `using` in a namespace block, a
    // generic twin, a conditional twin, an alias, and a global `using`.
    for src in [
        "TwinUse.cs#NewTwin",
        "InnerUsing.cs#NewGizmo",
        "InnerUsing.cs#NewWidget",
        "ArityCase.cs#NewBoth",
        "CondCase.cs#NewCond",
        "AliasCase.cs#NewAlias",
        "GlobalUse.cs#NewGlobal",
    ] {
        assert!(!from(&got, src), "{src}");
    }
}

#[test]
fn test_csx_project_files_scope_the_member_proofs() {
    let got = extracted_calls_of("csx_proj");
    // A `<Using>` item makes the project risky, so its member calls
    // stop. A bare call stays proven.
    assert!(!from(&got, "b/Y.cs#ViaThis"));
    assert!(!from(&got, "b/Y.cs#Cross"));
    assert!(has(&got, "b/Y.cs#BareOwn", "b/Y.cs#Own"));
    // A plain project keeps its member proofs.
    assert!(has(&got, "c/Z.cs#ViaThis", "c/Z.cs#Own"));
    assert!(has(&got, "c/Z.cs#CrossPub", "a/Lib.cs#Pub"));
    // An internal member of another project stops.
    assert!(!from(&got, "c/Z.cs#CrossInternal"));
}

#[test]
fn test_csx_hidden_project_file_makes_member_proofs_stop() {
    // A `.csproj` under a skipped folder is invisible, so every file is
    // risky. A bare call stays proven.
    let got = extracted_calls_of("csx_hidden");
    assert!(!from(&got, "Y.cs#ViaThis"));
    assert!(has(&got, "Y.cs#BareOwn", "Y.cs#Own"));
}

#[test]
fn test_csx_broken_file_stops_only_what_its_text_can_hide() {
    let got = extracted_calls_of("csx_broken");
    // The text names no extension `Draw`, so the member call stays
    // proven. The text declares an extension `Shadowed`, so that call stops.
    assert!(has(&got, "Use.cs#ViaDraw", "Widget.cs#Draw"));
    assert!(!from(&got, "Use.cs#ViaShadowed"));
    // The text holds `override Open`, so the virtual call stops. A
    // non-virtual call stays proven.
    assert!(!from(&got, "Widget.cs#CallOpen"));
    assert!(has(&got, "Widget.cs#CallPlain", "Widget.cs#Plain"));
    assert!(has(&got, "Use.cs#ViaNew", "Widget.cs#Widget"));
    // A type of a broken file is unknown.
    assert!(!from(&got, "Use.cs#ChildCaller"));
}

#[test]
fn test_csx1b_fixture_pins_base_static_and_field_proofs() {
    let got = extracted_calls_of("csx");
    let m = "Members.cs#";
    // The override in the intermediate class, not the root.
    assert!(has(&got, "Members.cs#CallBase", "Members.cs#Hook~2"));
    assert!(has(&got, "Members.cs#CallBaseRoot", "Members.cs#Hook"));
    // A static method, and a static extension call.
    assert!(has(&got, "Members.cs#CallBoot", "Members.cs#Boot"));
    assert!(has(&got, "Members.cs#CallTwice", "Members.cs#Twice"));
    // A field receiver with and without `this.`.
    for src in ["CallField", "CallThisField"] {
        assert!(has(&got, &format!("{m}{src}"), "Members.cs#Start"), "{src}");
    }
}

#[test]
fn test_csx1b_fixture_pins_base_static_and_field_stops() {
    let got = extracted_calls_of("csx");
    let m = "Members.cs#";
    // An abstract target and an object member.
    // An instance method, an overload, a local of the name, Color Color.
    // A property, an interface field, a field of the outer class.
    for src in [
        "CallObj",
        "CallInstance",
        "CallOver",
        "CallLocal",
        "CallColor",
        "CallProp",
        "CallPart",
        "CallOuter",
    ] {
        let to_member = got.iter().any(|(a, b)| {
            a == &format!("{m}{src}") && (b.ends_with("#Start") || b.ends_with("#Boot"))
        });
        assert!(!to_member, "{src}");
    }
    assert!(!from(&got, "Members.cs#CallObj"));
    assert!(!from(&got, "Members.cs#Must~2"));
    // A parameter named like the field hides it: the call reaches the
    // parameter type, which is the same type here.
    assert!(has(&got, "Members.cs#CallHidden", "Members.cs#Start"));
}
