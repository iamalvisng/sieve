//! Unit tests of the Kotlin call proof.

use super::trace_line;
use crate::extract::{Extractor, KtRec, RawEdge};
use crate::kt_index::KtIndex;
use sieve_core::{Confidence, Node, Relation};

/// Extracts the files. A path that ends in `.java` reads as Java.
fn extract(files: &[(&str, &str)]) -> (Vec<Node>, Vec<RawEdge>) {
    let mut x = Extractor::new().expect("build extractor");
    let (mut nodes, mut raw) = (Vec::new(), Vec::new());
    for (path, src) in files {
        let lang = if path.ends_with(".java") {
            "java"
        } else {
            "kotlin"
        };
        let (n, e) = x.extract_file(path, src, lang);
        nodes.extend(n);
        raw.extend(e);
    }
    (nodes, raw)
}

/// The proof of the first call of `name`: the target id or the stop word.
fn prove(files: &[(&str, &str)], name: &str) -> Result<String, &'static str> {
    let (_, raw) = extract(files);
    let ix = KtIndex::build(&raw);
    let (file, call) = raw
        .iter()
        .find_map(|e| match &e.kt {
            Some(KtRec::Cross(c)) if c.name == name => Some((e.file.clone(), c.clone())),
            _ => None,
        })
        .expect("call record");
    ix.prove(&file, &call, name)
}

fn stop(files: &[(&str, &str)], name: &str) -> &'static str {
    prove(files, name).err().unwrap_or("proved")
}

/// The confidence of the call edge between two node ids.
fn conf(files: &[(&str, &str)], from: &str, to: &str) -> Option<Confidence> {
    let (nodes, raw) = extract(files);
    crate::resolve::resolve_edges(&nodes, &raw)
        .into_iter()
        .find(|e| e.relation == Relation::Calls && e.source == from && e.target == to)
        .map(|e| e.confidence)
}

#[test]
fn a_bare_call_reaches_a_top_level_function_of_the_package_in_another_file() {
    let files = [
        ("b.kt", "package p\n\nfun helper() {}\n"),
        ("a.kt", "package p\n\nfun use() {\n    helper()\n}\n"),
    ];
    assert_eq!(prove(&files, "helper"), Ok("b.kt#helper".to_string()));
    assert_eq!(
        conf(&files, "a.kt#use", "b.kt#helper"),
        Some(Confidence::Extracted)
    );
}

#[test]
fn a_call_this_f_reaches_the_member_of_its_class() {
    let files = [(
        "a.kt",
        "class A {\n    fun m() {}\n    fun g() {\n        this.m()\n    }\n}\n",
    )];
    assert_eq!(prove(&files, "m"), Ok("a.kt#A.m".to_string()));
    assert_eq!(
        conf(&files, "a.kt#A.g", "a.kt#A.m"),
        Some(Confidence::Extracted)
    );
}

#[test]
fn a_constructor_call_reaches_the_class_node() {
    let files = [
        ("t.kt", "class T(val x: Int)\n"),
        ("a.kt", "fun mk() {\n    T(1)\n}\n"),
    ];
    assert_eq!(prove(&files, "T"), Ok("t.kt#T".to_string()));
    assert_eq!(
        conf(&files, "a.kt#mk", "t.kt#T"),
        Some(Confidence::Extracted)
    );
}

#[test]
fn a_bare_call_reaches_a_member_of_the_enclosing_class() {
    let files = [(
        "a.kt",
        "class A {\n    private fun m() {}\n    fun g() {\n        m()\n    }\n}\n",
    )];
    assert_eq!(prove(&files, "m"), Ok("a.kt#A.m".to_string()));
}

#[test]
fn an_extension_receiver_is_an_implicit_receiver() {
    let files = [
        ("r.kt", "class R {\n    fun m() {}\n}\n"),
        ("a.kt", "fun R.ext() {\n    m()\n}\n"),
    ];
    assert_eq!(prove(&files, "m"), Ok("r.kt#R.m".to_string()));
}

#[test]
fn an_unproven_kotlin_call_is_capped_at_inferred() {
    // A binder named like the function stops the proof. The old path still
    // finds the one function of that name in the file.
    let files = [("a.kt", "fun f() {}\nfun g(f: () -> Unit) {\n    f()\n}\n")];
    assert_eq!(stop(&files, "f"), "binder");
    assert_eq!(conf(&files, "a.kt#g", "a.kt#f"), Some(Confidence::Inferred));
}

#[test]
fn a_binder_of_the_called_name_stops() {
    let files = [(
        "a.kt",
        "fun f() {}\nfun g() {\n    val f = { }\n    f()\n}\n",
    )];
    assert_eq!(stop(&files, "f"), "binder");
}

#[test]
fn two_functions_of_the_name_stop() {
    let files = [
        ("b.kt", "package p\n\nfun f() {}\n"),
        ("c.kt", "package p\n\nfun f(x: Int) {}\n"),
        ("a.kt", "package p\n\nfun g() {\n    f()\n}\n"),
    ];
    assert_eq!(stop(&files, "f"), "two_candidates");
}

#[test]
fn a_name_that_no_declaration_has_stops() {
    let files = [("a.kt", "fun g() {\n    nothing()\n}\n")];
    assert_eq!(stop(&files, "nothing"), "no_candidate");
}

#[test]
fn an_import_of_the_name_from_outside_the_repo_stops() {
    let files = [
        ("b.kt", "package p\n\nfun f() {}\n"),
        (
            "a.kt",
            "package p\n\nimport org.lib.f\n\nfun g() {\n    f()\n}\n",
        ),
    ];
    assert_eq!(stop(&files, "f"), "out_of_repo_import");
}

#[test]
fn a_star_import_from_outside_the_repo_stops() {
    let files = [
        ("b.kt", "package p\n\nfun f() {}\n"),
        (
            "a.kt",
            "package p\n\nimport java.util.*\n\nfun g() {\n    f()\n}\n",
        ),
    ];
    assert_eq!(stop(&files, "f"), "star_outside");
}

#[test]
fn a_name_on_the_default_import_list_stops() {
    let files = [
        ("b.kt", "package p\n\nfun listOf() {}\n"),
        ("a.kt", "package p\n\nfun g() {\n    listOf()\n}\n"),
    ];
    assert_eq!(stop(&files, "listOf"), "top_list");
}

#[test]
fn a_this_call_on_an_extension_list_name_stops() {
    let files = [(
        "a.kt",
        "class A {\n    fun let() {}\n    fun g() {\n        this.let()\n    }\n}\n",
    )];
    assert_eq!(stop(&files, "let"), "ext_any");
}

#[test]
fn a_subtype_that_overrides_the_member_stops_even_with_no_open() {
    // A compiler plugin can open a class that has no `open` word.
    let files = [
        (
            "b.kt",
            "class B {\n    fun f() {}\n    fun g() {\n        f()\n    }\n}\n",
        ),
        ("c.kt", "class C : B() {\n    override fun f() {}\n}\n"),
    ];
    assert_eq!(stop(&files, "f"), "override_in_subtype");
}

#[test]
fn a_deep_subtype_that_overrides_the_member_stops() {
    let files = [
        (
            "b.kt",
            "open class B {\n    open fun f() {}\n    fun g() {\n        f()\n    }\n}\n",
        ),
        ("c.kt", "open class C : B()\n"),
        ("d.kt", "class D : C() {\n    override fun f() {}\n}\n"),
    ];
    assert_eq!(stop(&files, "f"), "override_in_subtype");
}

#[test]
fn an_anonymous_object_or_an_enum_entry_that_overrides_stops() {
    let anon = [
        (
            "b.kt",
            "open class B {\n    open fun f() {}\n    fun g() {\n        f()\n    }\n}\n",
        ),
        (
            "c.kt",
            "fun mk() {\n    val o = object : B() {\n        override fun f() {}\n    }\n}\n",
        ),
    ];
    assert_eq!(stop(&anon, "f"), "override_in_subtype");
    let entry = [(
        "e.kt",
        "enum class E {\n    X {\n        override fun f() {}\n    };\n    open fun f() {}\n    fun g() {\n        f()\n    }\n}\n",
    )];
    assert_eq!(stop(&entry, "f"), "override_in_subtype");
}

#[test]
fn a_subtype_that_does_not_override_the_name_keeps_the_proof() {
    let files = [
        (
            "b.kt",
            "open class B {\n    open fun f() {}\n    fun g() {\n        f()\n    }\n}\n",
        ),
        ("c.kt", "class C : B() {\n    override fun other() {}\n}\n"),
    ];
    assert_eq!(prove(&files, "f"), Ok("b.kt#B.f".to_string()));
}

#[test]
fn a_supertype_that_does_not_resolve_and_overrides_the_name_stops() {
    let files = [
        (
            "b.kt",
            "class B {\n    fun f() {}\n    fun g() {\n        f()\n    }\n}\n",
        ),
        ("a.kt", "typealias Alias = B\n"),
        ("c.kt", "class C : Alias() {\n    override fun f() {}\n}\n"),
    ];
    assert_eq!(stop(&files, "f"), "override_in_subtype");
}

#[test]
fn a_java_subtype_of_the_same_simple_name_stops() {
    let files = [
        (
            "b.kt",
            "open class B {\n    open fun f() {}\n    fun g() {\n        f()\n    }\n}\n",
        ),
        ("C.java", "public class C extends B {\n}\n"),
    ];
    assert_eq!(stop(&files, "f"), "java_subtype");
}

#[test]
fn an_object_member_needs_no_subtype_walk() {
    let files = [
        (
            "b.kt",
            "object O {\n    fun f() {}\n    fun g() {\n        f()\n    }\n}\n",
        ),
        ("c.kt", "class C : O() {\n    override fun f() {}\n}\n"),
    ];
    assert_eq!(prove(&files, "f"), Ok("b.kt#O.f".to_string()));
}

#[test]
fn an_interface_or_abstract_member_is_never_proven() {
    let iface = [(
        "a.kt",
        "interface I {\n    fun f() {}\n    fun g() {\n        f()\n    }\n}\n",
    )];
    assert_eq!(stop(&iface, "f"), "abstract");
    let abs = [(
        "a.kt",
        "abstract class A {\n    abstract fun f()\n    fun g() {\n        f()\n    }\n}\n",
    )];
    assert_eq!(stop(&abs, "f"), "abstract");
    let expect = [
        ("a.kt", "expect fun f()\n"),
        ("b.kt", "fun g() {\n    f()\n}\n"),
    ];
    assert_eq!(stop(&expect, "f"), "abstract");
}

#[test]
fn a_constructor_of_an_abstract_class_an_object_or_an_enum_stops() {
    let abs = [
        ("t.kt", "abstract class T\n"),
        ("a.kt", "fun mk() {\n    T()\n}\n"),
    ];
    assert_eq!(stop(&abs, "T"), "abstract");
    let obj = [("a.kt", "object T\nfun mk() {\n    T()\n}\n")];
    assert_eq!(stop(&obj, "T"), "not_ctor");
    let en = [("a.kt", "enum class T { A }\nfun mk() {\n    T()\n}\n")];
    assert_eq!(stop(&en, "T"), "not_ctor");
    let invoke = [(
        "a.kt",
        "class T {\n    companion object {\n        operator fun invoke() {}\n    }\n}\nfun mk() {\n    T()\n}\n",
    )];
    assert_eq!(stop(&invoke, "T"), "companion_invoke");
}

#[test]
fn a_lambda_on_the_no_receiver_list_keeps_the_proof() {
    let files = [
        ("b.kt", "package p\n\nfun h() {}\n"),
        (
            "a.kt",
            "package p\n\nfun w() {\n    repeat(2) {\n        h()\n    }\n}\n",
        ),
    ];
    assert_eq!(prove(&files, "h"), Ok("b.kt#h".to_string()));
}

#[test]
fn a_lambda_off_the_list_or_with_a_receiver_stops() {
    let with = [
        ("b.kt", "package p\n\nfun h() {}\n"),
        (
            "a.kt",
            "package p\n\nfun w() {\n    with(1) {\n        h()\n    }\n}\n",
        ),
    ];
    assert_eq!(stop(&with, "h"), "lambda_receiver");
    let recv = [
        ("b.kt", "package p\n\nfun h() {}\n"),
        (
            "a.kt",
            "package p\n\nfun w() {\n    1.run {\n        h()\n    }\n}\n",
        ),
    ];
    assert_eq!(stop(&recv, "h"), "lambda_receiver");
    let own = [
        (
            "b.kt",
            "package p\n\nfun h() {}\nfun repeat(n: Int, f: () -> Unit) {}\n",
        ),
        (
            "a.kt",
            "package p\n\nfun w() {\n    repeat(2) {\n        h()\n    }\n}\n",
        ),
    ];
    assert_eq!(stop(&own, "h"), "lambda_receiver");
    let gradle = [
        ("b.kt", "package p\n\nfun h() {}\n"),
        (
            "a.kt",
            "package p\n\nimport org.gradle.api.Project\n\nfun w() {\n    repeat(2) {\n        h()\n    }\n}\n",
        ),
    ];
    assert_eq!(stop(&gradle, "h"), "lambda_receiver");
}

#[test]
fn every_enclosing_lambda_must_pass() {
    let files = [
        ("b.kt", "package p\n\nfun h() {}\n"),
        (
            "a.kt",
            "package p\n\nfun w() {\n    repeat(2) {\n        with(1) {\n            h()\n        }\n    }\n}\n",
        ),
    ];
    assert_eq!(stop(&files, "h"), "lambda_receiver");
}

#[test]
fn a_call_in_an_anonymous_object_stops() {
    let files = [(
        "a.kt",
        "fun h() {}\nfun w() {\n    val o = object : Runnable {\n        override fun run() {\n            h()\n        }\n    }\n}\n",
    )];
    assert_eq!(stop(&files, "h"), "anon_body");
}

#[test]
fn a_chain_with_an_outside_supertype_stops() {
    let files = [(
        "a.kt",
        "class A : Outside() {\n    fun m() {}\n    fun g() {\n        m()\n    }\n}\n",
    )];
    assert_eq!(stop(&files, "m"), "unknown_chain");
}

#[test]
fn a_frozen_supertype_keeps_the_chain_and_an_implicit_member_stops() {
    let files = [(
        "a.kt",
        "class A : Comparable<A> {\n    fun m() {}\n    fun g() {\n        m()\n    }\n}\n",
    )];
    assert_eq!(prove(&files, "m"), Ok("a.kt#A.m".to_string()));
    let implicit = [(
        "a.kt",
        "@Serializable\nclass D {\n    fun g() {\n        serializer()\n    }\n}\n",
    )];
    assert_eq!(stop(&implicit, "serializer"), "implicit_member");
}

#[test]
fn an_arity_that_no_parameter_list_allows_stops() {
    let files = [
        ("b.kt", "package p\n\nfun f(a: Int, b: Int = 1) {}\n"),
        (
            "a.kt",
            "package p\n\nfun g() {\n    f(1)\n    f(1, 2, 3)\n}\n",
        ),
    ];
    assert_eq!(prove(&files, "f"), Ok("b.kt#f".to_string()));
    let (_, raw) = extract(&files);
    let ix = KtIndex::build(&raw);
    let over = raw
        .iter()
        .filter_map(|e| match &e.kt {
            Some(KtRec::Cross(c)) if c.name == "f" => Some(c.clone()),
            _ => None,
        })
        .find(|c| c.arity == 3)
        .expect("call with 3 arguments");
    assert_eq!(ix.prove("a.kt", &over, "f"), Err("arity"));
}

#[test]
fn a_named_argument_or_a_vararg_parameter_stops() {
    let named = [("a.kt", "fun f(a: Int) {}\nfun g() {\n    f(a = 1)\n}\n")];
    assert_eq!(stop(&named, "f"), "loose");
    let vararg = [("a.kt", "fun f(vararg a: Int) {}\nfun g() {\n    f(1)\n}\n")];
    assert_eq!(stop(&vararg, "f"), "vararg");
}

#[test]
fn a_property_of_the_name_stops() {
    let files = [("a.kt", "val f = 1\nfun g() {\n    f()\n}\n")];
    assert_eq!(stop(&files, "f"), "property");
}

#[test]
fn a_private_function_of_another_file_stops() {
    let files = [
        ("b.kt", "package p\n\nprivate fun f() {}\n"),
        ("a.kt", "package p\n\nfun g() {\n    f()\n}\n"),
    ];
    assert_eq!(stop(&files, "f"), "private_other_file");
}

#[test]
fn a_split_package_stops() {
    // `a.kt` imports a name of package `q` that the repo does not declare.
    let files = [
        ("q.kt", "package q\n\nfun other() {}\n"),
        ("b.kt", "package q\n\nfun f() {}\n"),
        (
            "a.kt",
            "package q\n\nimport q.Missing\n\nfun g() {\n    f()\n}\n",
        ),
    ];
    assert_eq!(stop(&files, "f"), "split_package");
}

#[test]
fn a_broken_file_gets_no_proof_and_its_names_stop_the_repo() {
    let files = [
        ("b.kt", "package p\n\ncontext(Ctx)\nfun f() {}\n"),
        ("c.kt", "package p\n\nfun f(x: Int) {}\nfun h() {}\n"),
        ("a.kt", "package p\n\nfun g() {\n    f(1)\n    h()\n}\n"),
    ];
    assert_eq!(stop(&files, "f"), "lost_name");
    assert_eq!(prove(&files, "h"), Ok("c.kt#h".to_string()));
    let own = [(
        "b.kt",
        "package p\n\ncontext(Ctx)\nfun f() {}\nfun g() {\n    g()\n}\n",
    )];
    assert_eq!(stop(&own, "g"), "broken_file");
}

#[test]
fn a_script_gets_no_proof() {
    let files = [
        ("b.kt", "fun h() {}\n"),
        ("a.kts", "fun w() {\n    h()\n}\n"),
    ];
    assert_eq!(stop(&files, "h"), "script");
}

#[test]
fn a_java_type_of_the_name_stops() {
    let files = [
        ("T.java", "public class T {\n}\n"),
        ("t.kt", "class T\n"),
        ("a.kt", "fun mk() {\n    T()\n}\n"),
    ];
    assert_eq!(stop(&files, "T"), "java_type");
}

#[test]
fn a_call_on_a_local_var_has_no_proof() {
    let files = [(
        "a.kt",
        "class A {\n    fun m() {}\n    fun g() {\n        var x: A = A()\n        x.m()\n    }\n}\n",
    )];
    assert_eq!(stop(&files, "m"), "receiver_binder");
}

#[test]
fn a_this_call_with_a_label_stops() {
    let files = [(
        "a.kt",
        "class A {\n    fun m() {}\n    fun g() {\n        this@A.m()\n    }\n}\n",
    )];
    assert_eq!(stop(&files, "m"), "label");
}

#[test]
fn an_alias_import_hides_the_original_name() {
    let files = [
        ("b.kt", "package q\n\nfun orig() {}\n"),
        (
            "a.kt",
            "package p\n\nimport q.orig as other\n\nfun g() {\n    other()\n    orig()\n}\n",
        ),
    ];
    assert_eq!(prove(&files, "other"), Ok("b.kt#orig".to_string()));
    assert_eq!(stop(&files, "orig"), "no_candidate");
}

#[test]
fn an_enum_entry_body_of_a_nested_enum_that_overrides_stops() {
    let files = [(
        "a.kt",
        "class Outer {\n    enum class E {\n        A {\n            override fun f() = 2\n        };\n        open fun f() = 1\n        fun g() = f()\n    }\n}\n",
    )];
    assert_eq!(stop(&files, "f"), "override_in_subtype");
}

#[test]
fn an_anonymous_object_of_a_nested_class_that_overrides_stops() {
    let files = [(
        "a.kt",
        "class Outer {\n    open class Base {\n        open fun f() = 1\n        fun g() = f()\n    }\n    fun mk() = object : Base() {\n        override fun f() = 2\n    }\n}\n",
    )];
    assert_eq!(stop(&files, "f"), "override_in_subtype");
}

#[test]
fn a_subtype_behind_an_import_alias_that_overrides_stops() {
    let files = [
        (
            "a.kt",
            "package p\n\nopen class A {\n    open fun f() = 1\n    fun g() = f()\n}\n",
        ),
        (
            "c.kt",
            "package q\n\nimport p.A as Z\n\nclass C : Z() {\n    override fun f() = 2\n}\n",
        ),
    ];
    assert_eq!(stop(&files, "f"), "override_in_subtype");
}

#[test]
fn a_lambda_call_that_is_a_local_function_a_lost_name_or_an_outside_import_stops() {
    let local = [
        (
            "b.kt",
            "package p\n\nfun open() {}\nclass Foo {\n    fun open() {}\n}\n",
        ),
        (
            "a.kt",
            "package p\n\nfun m() {\n    fun repeat(n: Int, b: Foo.() -> Unit) = Unit\n    repeat(2) {\n        open()\n    }\n}\n",
        ),
    ];
    assert_eq!(stop(&local, "open"), "lambda_receiver");
    let lost = [
        ("b.kt", "package p\n\nfun open() {}\n"),
        ("c.kt", "package p\n\ncontext(Foo)\nfun repeat() {}\n"),
        (
            "a.kt",
            "package p\n\nfun m() {\n    repeat(2) {\n        open()\n    }\n}\n",
        ),
    ];
    assert_eq!(stop(&lost, "open"), "lambda_receiver");
    let outside = [
        ("b.kt", "package p\n\nfun open() {}\n"),
        (
            "a.kt",
            "package p\n\nimport org.lib.repeat\n\nfun m() {\n    repeat(2) {\n        open()\n    }\n}\n",
        ),
    ];
    assert_eq!(stop(&outside, "open"), "lambda_receiver");
}

#[test]
fn a_context_receiver_marks_the_file_broken_in_every_form() {
    for head in [
        "context(Foo) fun g()",
        "context(foo: Foo) fun g()",
        "@A context(Foo) fun g()",
        "private context(Foo) fun g()",
        "context(Foo)\nfun g()",
    ] {
        let text = format!("{head} {{\n    log()\n}}\n");
        let files = [
            ("b.kt", "fun log() {}\nclass Foo {\n    fun log() {}\n}\n"),
            ("a.kt", text.as_str()),
        ];
        assert_eq!(stop(&files, "log"), "broken_file", "{head}");
    }
}

#[test]
fn an_invoke_extension_on_a_companion_stops_a_constructor_call() {
    let files = [
        ("b.kt", "class Box {\n    companion object\n}\n"),
        (
            "c.kt",
            "operator fun Box.Companion.invoke(x: Int) = Box()\n",
        ),
        ("a.kt", "fun mk() {\n    Box(1)\n}\n"),
    ];
    let (_, raw) = extract(&files);
    let ix = KtIndex::build(&raw);
    let (file, call) = raw
        .iter()
        .find_map(|e| match &e.kt {
            Some(KtRec::Cross(c)) if c.name == "Box" && e.file == "a.kt" => {
                Some((e.file.clone(), c.clone()))
            }
            _ => None,
        })
        .expect("call record");
    assert_eq!(ix.prove(&file, &call, "Box"), Err("companion_invoke"));
}

#[test]
fn the_trace_line_has_five_tab_separated_fields() {
    let line = trace_line("a.kt", 7, "f", "bare", "a.kt#f", "ok");
    assert_eq!(line, "a.kt:7\tf\tbare\ta.kt#f\tok\n");
}

#[test]
fn the_trace_file_gets_one_line_per_call() {
    let files = [("a.kt", "fun f() {}\nfun g() {\n    f()\n    nothing()\n}\n")];
    let (_, raw) = extract(&files);
    let mut ix = KtIndex::build(&raw);
    let path = std::env::temp_dir().join(format!("sieve-kt-trace-{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&path);
    ix.trace = Some(path.clone());
    for e in &raw {
        if let (Some(KtRec::Cross(c)), Some(name)) = (&e.kt, e.name.as_deref()) {
            ix.resolve(&e.file, c, name);
        }
    }
    let text = std::fs::read_to_string(&path).expect("trace file");
    let _ = std::fs::remove_file(&path);
    assert_eq!(
        text,
        "a.kt:3\tf\tbare\ta.kt#f\tok\na.kt:4\tnothing\tbare\t-\tno_candidate\n"
    );
}

#[test]
fn test_kt_recv_a_subtype_fun_without_override_stops_the_walk() {
    let files = [
        (
            "b.kt",
            "open class B {\n    open fun f() {}\n    fun g() {\n        f()\n    }\n}\n",
        ),
        ("c.kt", "class C : B() {\n    fun f(x: Int) {}\n}\n"),
    ];
    assert_eq!(stop(&files, "f"), "override_in_subtype");
    let anon = [(
        "a.kt",
        "open class B {\n    open fun f() {}\n    fun g() {\n        f()\n    }\n}\nfun mk() {\n    val o = object : B() {\n        fun f(x: Int) {}\n    }\n}\n",
    )];
    assert_eq!(stop(&anon, "f"), "override_in_subtype");
}

#[test]
fn test_kt_recv_a_local_fun_binder_stops_a_this_call() {
    let files = [(
        "a.kt",
        "class A {\n    fun m() {}\n    fun g() {\n        fun m() {}\n        this.m()\n    }\n}\n",
    )];
    assert_eq!(stop(&files, "m"), "binder");
}

const RECV_BASE: &str =
    "open class B {\n    open fun f() {}\n    fun g() {\n        f()\n    }\n}\n";

#[test]
fn test_kt_recv_an_enum_entry_body_with_a_plain_fun_stops() {
    let files = [
        ("b.kt", RECV_BASE),
        (
            "e.kt",
            "enum class E : B() {\n    X {\n        fun f(x: Int) {}\n    }\n}\n",
        ),
    ];
    assert_eq!(stop(&files, "f"), "override_in_subtype");
}

#[test]
fn test_kt_recv_a_local_class_with_a_plain_fun_stops() {
    let files = [
        ("b.kt", RECV_BASE),
        (
            "a.kt",
            "fun mk() {\n    class L : B() {\n        fun f(x: Int) {}\n    }\n}\n",
        ),
    ];
    assert_eq!(stop(&files, "f"), "override_in_subtype");
}

#[test]
fn test_kt_recv_a_plain_fun_in_a_subtype_of_a_subtype_stops() {
    let files = [
        ("b.kt", RECV_BASE),
        ("c.kt", "open class C : B()\n"),
        ("d.kt", "class D : C() {\n    fun f(x: Int) {}\n}\n"),
    ];
    assert_eq!(stop(&files, "f"), "override_in_subtype");
}

const CLASS_A: &str = "class A {\n    fun m() {}\n}\n";

/// The proof of `m` in the file `a.kt`: `CLASS_A` plus `rest`.
fn with_a(rest: &str) -> Result<String, &'static str> {
    let src = format!("{CLASS_A}{rest}");
    prove(&[("a.kt", src.as_str())], "m")
}

fn a_stop(rest: &str) -> &'static str {
    with_a(rest).err().unwrap_or("proved")
}

#[test]
fn test_kt_link_kt12_a_typed_param_reaches_the_member() {
    assert_eq!(
        with_a("fun use(a: A) {\n    a.m()\n}\n"),
        Ok("a.kt#A.m".to_string())
    );
}

#[test]
fn test_kt_link_kt12_a_typed_local_val_reaches_the_member() {
    let src = "fun use() {\n    val a: A = make()\n    a.m()\n}\n";
    assert_eq!(with_a(src), Ok("a.kt#A.m".to_string()));
}

#[test]
fn test_kt_link_kt12_a_new_local_reaches_the_member() {
    let src = "fun use() {\n    val a = A()\n    a.m()\n}\n";
    assert_eq!(with_a(src), Ok("a.kt#A.m".to_string()));
}

#[test]
fn test_kt_link_kt12_a_class_property_reaches_the_member() {
    let src = "class B(val a: A) {\n    fun g() {\n        a.m()\n    }\n}\n";
    assert_eq!(with_a(src), Ok("a.kt#A.m".to_string()));
    let body = "class C {\n    private val a: A = A()\n    fun g() {\n        a.m()\n    }\n}\n";
    assert_eq!(with_a(body), Ok("a.kt#A.m".to_string()));
}

#[test]
fn test_kt_link_kt12_a_nullable_receiver_with_safe_call_reaches_the_member() {
    let src = "fun use(a: A?) {\n    a?.m()\n}\n";
    assert_eq!(with_a(src), Ok("a.kt#A.m".to_string()));
}

#[test]
fn test_kt_link_kt12_an_extension_on_the_supertype_is_the_target() {
    let files = [(
        "a.kt",
        "open class Base\nclass A : Base()\nclass Other\nfun Base.m() {}\nfun Other.m() {}\nfun use(a: A) {\n    a.m()\n}\n",
    )];
    assert_eq!(prove(&files, "m"), Ok("a.kt#m".to_string()));
}

#[test]
fn test_kt_link_kt12_a_member_and_an_extension_make_two_candidates() {
    let src = "fun A.m() {}\nfun use(a: A) {\n    a.m()\n}\n";
    assert_eq!(a_stop(src), "two_candidates");
}

#[test]
fn test_kt_link_kt12_an_open_member_with_an_override_stops() {
    let files = [(
        "a.kt",
        "open class A {\n    open fun m() {}\n}\nclass B : A() {\n    override fun m() {}\n}\nfun use(a: A) {\n    a.m()\n}\n",
    )];
    assert_eq!(stop(&files, "m"), "override_in_subtype");
}

#[test]
fn test_kt_link_kt12_an_interface_member_stops() {
    let files = [(
        "a.kt",
        "interface I {\n    fun m()\n}\nfun use(i: I) {\n    i.m()\n}\n",
    )];
    assert_eq!(stop(&files, "m"), "abstract");
}

#[test]
fn test_kt_link_kt12_a_cast_in_the_function_stops() {
    let src = "class B\nfun use(a: A) {\n    if (a is B) {}\n    a.m()\n}\n";
    assert_eq!(a_stop(src), "cast");
}

#[test]
fn test_kt_link_kt12_a_local_var_and_an_untyped_val_stop() {
    let var = "fun use() {\n    var a: A = make()\n    a.m()\n}\n";
    assert_eq!(a_stop(var), "receiver_binder");
    let untyped = "fun use() {\n    val a = make()\n    a.m()\n}\n";
    assert_eq!(a_stop(untyped), "receiver_binder");
}

#[test]
fn test_kt_link_kt12_a_type_param_and_an_outside_type_stop() {
    let tp = "fun <A> use(a: A) {\n    a.m()\n}\n";
    assert_eq!(a_stop(tp), "type_param");
    let outside = "fun use(a: String) {\n    a.m()\n}\n";
    assert_eq!(a_stop(outside), "outside_type");
}

#[test]
fn test_kt_link_kt12_two_properties_named_x_stop() {
    let src = "val a: A = A()\nclass B(val a: A) {\n    fun g() {\n        a.m()\n    }\n}\n";
    assert_eq!(a_stop(src), "two_properties");
}

#[test]
fn test_kt_link_kt12_a_vararg_param_stops() {
    let src = "fun use(vararg a: A) {\n    a.m()\n}\n";
    assert_eq!(a_stop(src), "receiver_binder");
}

#[test]
fn test_kt_link_b3_a_local_callable_named_f_stops_every_shape() {
    let local = "fun use(a: A) {\n    fun m() {}\n    a.m()\n}\n";
    assert_eq!(a_stop(local), "binder");
    let lambda = "fun use(a: A, m: A.() -> Unit) {\n    a.m()\n}\n";
    assert_eq!(a_stop(lambda), "binder");
}

#[test]
fn test_kt_link_b4_a_property_type_resolves_where_the_property_is_declared() {
    let files = [(
        "a.kt",
        "open class Holder {\n    class Item {\n        fun m() {}\n    }\n    val item: Item = Item()\n}\nclass Sub : Holder() {\n    class Item {\n        fun m() {}\n    }\n    fun g() {\n        item.m()\n    }\n}\n",
    )];
    assert_eq!(prove(&files, "m"), Ok("a.kt#Holder.Item.m".to_string()));
}

#[test]
fn test_kt_link_b4_an_extension_receiver_resolves_where_the_extension_is_declared() {
    let files = [
        (
            "a.kt",
            "package p\n\nimport q.m\n\nclass Item {\n    fun m() {}\n}\nfun use(i: Item) {\n    i.m()\n}\n",
        ),
        ("b.kt", "package q\n\nclass Item\nfun Item.m() {}\n"),
    ];
    assert_eq!(prove(&files, "m"), Ok("a.kt#Item.m".to_string()));
}

#[test]
fn test_kt_link_b5_a_binder_after_the_call_stops() {
    let src = "fun use() {\n    a.m()\n    val a: A = make()\n}\n";
    assert_eq!(a_stop(src), "receiver_binder");
}

#[test]
fn test_kt_link_b6_a_type_named_like_the_property_stops() {
    let src = "class a\nval a: A = A()\nfun use() {\n    a.m()\n}\n";
    assert_eq!(a_stop(src), "type_named_x");
}

#[test]
fn test_kt_link_b6_a_library_name_or_a_lost_name_stops_the_property_path() {
    let top = "val println: A = A()\nfun use() {\n    println.m()\n}\n";
    assert_eq!(a_stop(top), "top_list");
    let files = [
        (
            "a.kt",
            "class A {\n    fun m() {}\n}\nval lostx: A = A()\nfun use() {\n    lostx.m()\n}\n",
        ),
        ("b.kt", "val lostx = 1\nfun ((( broken\n"),
    ];
    assert_eq!(stop(&files, "m"), "lost_name");
}

#[test]
fn test_kt_link_kt13_an_object_member_is_exact() {
    let files = [(
        "a.kt",
        "open class Base {\n    open fun m() {}\n}\nclass D : Base() {\n    override fun m() {}\n}\nobject O : Base()\nfun use() {\n    O.m()\n}\n",
    )];
    assert_eq!(prove(&files, "m"), Ok("a.kt#Base.m".to_string()));
}

#[test]
fn test_kt_link_kt13_a_companion_member_is_the_target() {
    let files = [(
        "a.kt",
        "class A {\n    companion object {\n        fun m() {}\n    }\n}\nfun use() {\n    A.m()\n}\n",
    )];
    let got = prove(&files, "m").expect("proved");
    assert!(got.starts_with("a.kt#A.") && got.ends_with(".m"), "{got}");
}

#[test]
fn test_kt_link_kt13_an_extension_on_the_companion_is_the_target() {
    let files = [(
        "a.kt",
        "class A {\n    companion object\n}\nfun A.Companion.m() {}\nfun use() {\n    A.m()\n}\n",
    )];
    assert_eq!(prove(&files, "m"), Ok("a.kt#m".to_string()));
}

#[test]
fn test_kt_link_b9_an_extension_with_a_qualified_receiver_still_counts() {
    let files = [(
        "a.kt",
        "class A {\n    companion object {\n        fun m() {}\n    }\n}\nfun A.Companion.m() {}\nfun use() {\n    A.m()\n}\n",
    )];
    assert_eq!(stop(&files, "m"), "two_candidates");
}

#[test]
fn test_kt_link_kt13_a_nested_class_constructor_gives_the_class_node() {
    let files = [(
        "a.kt",
        "class Outer {\n    class Inner\n}\nfun use() {\n    Outer.Inner()\n}\n",
    )];
    assert_eq!(prove(&files, "Inner"), Ok("a.kt#Outer.Inner".to_string()));
}

#[test]
fn test_kt_link_kt13_a_param_named_like_the_type_takes_the_value_path() {
    let src = "class B {\n    fun m() {}\n}\nfun use(A: B) {\n    A.m()\n}\n";
    assert_eq!(with_a(src), Ok("a.kt#B.m".to_string()));
}

#[test]
fn test_kt_link_kt13_a_property_named_like_the_type_stops() {
    let prop = "class C {\n    companion object {\n        fun m() {}\n    }\n}\nval C: A = A()\nfun use() {\n    C.m()\n}\n";
    assert_eq!(a_stop(prop), "type_named_x");
}

#[test]
fn test_kt_link_kt13_an_enum_implicit_member_and_an_entry_name_stop() {
    let files = [(
        "a.kt",
        "enum class E { X }\nfun use() {\n    E.values()\n}\n",
    )];
    assert_eq!(stop(&files, "values"), "implicit_member");
    let entry = [(
        "a.kt",
        "enum class E {\n    X;\n    fun g() {\n        X.m()\n    }\n}\n",
    )];
    assert_eq!(stop(&entry, "m"), "value_named_x");
}

#[test]
fn test_kt_link_kt13_a_class_without_a_companion_has_no_candidate() {
    assert_eq!(a_stop("fun use() {\n    A.m()\n}\n"), "no_candidate");
}

#[test]
fn test_kt_link_kt14_a_super_call_reaches_an_open_member() {
    let files = [(
        "a.kt",
        "open class B {\n    open fun m() {}\n}\nclass C : B() {\n    override fun m() {\n        super.m()\n    }\n}\n",
    )];
    assert_eq!(prove(&files, "m"), Ok("a.kt#B.m".to_string()));
}

#[test]
fn test_kt_link_kt14_a_super_call_does_not_dispatch() {
    let files = [(
        "a.kt",
        "open class B {\n    open fun m() {}\n}\nopen class C : B() {\n    override fun m() {\n        super.m()\n    }\n}\nclass D : C() {\n    override fun m() {}\n}\n",
    )];
    assert_eq!(prove(&files, "m"), Ok("a.kt#B.m".to_string()));
}

#[test]
fn test_kt_link_kt14_an_interface_member_and_a_typed_super_stop() {
    let iface = [(
        "a.kt",
        "interface I {\n    fun m() {}\n}\nclass C : I {\n    override fun m() {\n        super.m()\n    }\n}\n",
    )];
    assert_eq!(stop(&iface, "m"), "abstract");
    let typed = [(
        "a.kt",
        "interface I {\n    fun m() {}\n}\nclass C : I {\n    override fun m() {\n        super<I>.m()\n    }\n}\n",
    )];
    assert_eq!(stop(&typed, "m"), "label");
}

#[test]
fn test_kt_link_kt14_an_override_of_an_override_stops() {
    let files = [(
        "a.kt",
        "open class A {\n    open fun m() {}\n}\nopen class B : A() {\n    override fun m() {}\n}\nclass C : B() {\n    override fun m() {\n        super.m()\n    }\n}\n",
    )];
    assert_eq!(stop(&files, "m"), "two_candidates");
}

#[test]
fn test_kt_link_kt14_an_extension_function_around_the_call_stops() {
    let files = [(
        "a.kt",
        "open class B {\n    open fun m() {}\n}\nfun B.g() {\n    super.m()\n}\n",
    )];
    assert_eq!(stop(&files, "m"), "ext_super");
}

#[test]
fn test_kt_link_a_proved_name_call_is_extracted() {
    let files = [(
        "a.kt",
        "class A {\n    fun m() {}\n}\nfun use(a: A) {\n    a.m()\n}\n",
    )];
    assert_eq!(
        conf(&files, "a.kt#use", "a.kt#A.m"),
        Some(Confidence::Extracted)
    );
}

#[test]
fn test_kt_link_f1_a_generic_extension_with_a_type_param_receiver_stays_in_the_set() {
    let src = "class Item\nclass A {\n    fun f(n: Int) {}\n}\nfun <Item> Item.f(s: String) {}\nfun use(a: A) {\n    a.f(\"x\")\n}\n";
    let files = [("a.kt", src)];
    assert_eq!(stop(&files, "f"), "two_candidates");
}

#[test]
fn test_kt_link_f1_a_generic_extension_can_be_the_only_candidate() {
    let src = "class A\nfun <T> T.f() {}\nfun use(a: A) {\n    a.f()\n}\n";
    assert_eq!(prove(&[("a.kt", src)], "f"), Ok("a.kt#f".to_string()));
}

#[test]
fn test_kt_link_f2_a_local_fun_named_like_the_new_type_stops() {
    let src = "class T {\n    fun m() {}\n}\nclass U {\n    fun m() {}\n}\nfun use() {\n    fun T() = U()\n    val x = T()\n    x.m()\n}\n";
    assert_eq!(stop(&[("a.kt", src)], "m"), "receiver_binder");
}

#[test]
fn test_kt_link_f6_an_unrelated_extension_drops_out() {
    let src = "class Other\nfun Other.m() {}\nfun use(a: A) {\n    a.m()\n}\n";
    assert_eq!(with_a(src), Ok("a.kt#A.m".to_string()));
}

#[test]
fn test_kt_link_f6_a_typealias_receiver_stays_in_the_set() {
    let src = "typealias Al = A\nfun Al.m() {}\nfun use(a: A) {\n    a.m()\n}\n";
    assert_eq!(a_stop(src), "two_candidates");
}

#[test]
fn test_kt_link_f6_an_aliased_import_receiver_stays_in_the_set() {
    let files = [
        (
            "a.kt",
            "package p\n\nimport q.Item as I\nimport q.Item\n\nfun I.m() {}\nfun use(i: Item) {\n    i.m()\n}\n",
        ),
        ("b.kt", "package q\n\nclass Item {\n    fun m() {}\n}\n"),
    ];
    assert_eq!(stop(&files, "m"), "two_candidates");
}

#[test]
fn test_kt_link_f6_a_nullable_receiver_extension_applies() {
    let src = "fun A?.m() {}\nfun use(a: A) {\n    a.m()\n}\n";
    assert_eq!(a_stop(src), "two_candidates");
    let only = "class B\nfun B?.m() {}\nfun use(b: B?) {\n    b?.m()\n}\n";
    assert_eq!(prove(&[("a.kt", only)], "m"), Ok("a.kt#m".to_string()));
}

#[test]
fn test_kt_link_f6_an_extension_property_named_x_stops() {
    let src = "val A.x: A get() = this\nfun A.g() {\n    x.m()\n}\n";
    assert_eq!(a_stop(src), "ext_property");
}

#[test]
fn test_kt_link_f6_a_private_property_of_another_file_stops() {
    let files = [
        (
            "a.kt",
            "class A {\n    fun m() {}\n}\nfun use() {\n    pa.m()\n}\n",
        ),
        ("b.kt", "private val pa: A = A()\n"),
    ];
    assert_eq!(stop(&files, "m"), "private_other_file");
}

#[test]
fn test_kt_link_f6_a_property_without_a_written_type_stops() {
    let src = "val pa = A()\nfun use() {\n    pa.m()\n}\n";
    assert_eq!(a_stop(src), "untyped_property");
}

#[test]
fn test_kt_link_f6_a_type_param_named_like_the_type_stops_on_the_type_path() {
    let src = "fun <A> use() {\n    A.m()\n}\n";
    assert_eq!(a_stop(src), "type_param");
}

#[test]
fn test_kt_link_f6_an_object_that_overrides_the_member_gives_two_candidates() {
    let files = [(
        "a.kt",
        "open class Base {\n    open fun m() {}\n}\nobject O : Base() {\n    override fun m() {}\n}\nfun use() {\n    O.m()\n}\n",
    )];
    assert_eq!(stop(&files, "m"), "two_candidates");
}

#[test]
fn test_kt_link_f6_an_abstract_class_member_stops() {
    let files = [(
        "a.kt",
        "abstract class B {\n    abstract fun m()\n}\nfun use(b: B) {\n    b.m()\n}\n",
    )];
    assert_eq!(stop(&files, "m"), "abstract");
}

const OPEN_B: &str = "open class B {\n    open fun m() {}\n}\n";

#[test]
fn test_kt_link_f6_a_super_call_in_a_plain_lambda() {
    let lambda = format!(
        "{OPEN_B}class C : B() {{\n    override fun m() {{\n        check(true) {{ super.m() }}\n    }}\n}}\n"
    );
    assert_eq!(
        prove(&[("a.kt", lambda.as_str())], "m"),
        Ok("a.kt#B.m".to_string())
    );
}

#[test]
fn test_kt_link_f6_a_super_call_in_a_nested_class() {
    let src = format!(
        "{OPEN_B}class Outer : B() {{\n    class N : B() {{\n        fun g() {{\n            super.m()\n        }}\n    }}\n}}\n"
    );
    assert_eq!(
        prove(&[("a.kt", src.as_str())], "m"),
        Ok("a.kt#B.m".to_string())
    );
}

#[test]
fn test_kt_link_f6_a_super_call_in_an_inner_class() {
    let src = format!(
        "{OPEN_B}class Outer {{\n    inner class N : B() {{\n        fun g() {{\n            super.m()\n        }}\n    }}\n}}\n"
    );
    assert_eq!(
        prove(&[("a.kt", src.as_str())], "m"),
        Ok("a.kt#B.m".to_string())
    );
}

#[test]
fn test_kt_link_f6_a_super_call_in_an_object() {
    let src = format!("{OPEN_B}object O : B() {{\n    fun g() {{\n        super.m()\n    }}\n}}\n");
    assert_eq!(
        prove(&[("a.kt", src.as_str())], "m"),
        Ok("a.kt#B.m".to_string())
    );
}

#[test]
fn test_kt_link_f6_a_super_call_in_an_anonymous_object_stops() {
    let src = format!(
        "{OPEN_B}class C : B() {{\n    fun g() {{\n        val o = object : B() {{\n            fun h() {{\n                super.m()\n            }}\n        }}\n    }}\n}}\n"
    );
    assert_eq!(stop(&[("a.kt", src.as_str())], "m"), "anon_body");
}

#[test]
fn test_kt_link_f6_a_super_call_unions_the_chains_of_two_supertypes() {
    let files = [(
        "a.kt",
        "interface I {\n    fun m() {}\n}\nopen class B {\n    open fun m() {}\n}\nclass C : B(), I {\n    override fun m() {\n        super.m()\n    }\n}\n",
    )];
    assert_eq!(stop(&files, "m"), "two_candidates");
}
