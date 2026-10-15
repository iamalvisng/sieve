//! Unit tests of the Kotlin file facts and call records.

use super::{header_line, Marks};
use crate::extract::{
    Extractor, KtBodyKind, KtCall, KtFileRec, KtKind, KtRec, KtRty, KtScope, KtShape, KtType,
};

/// Extracts one Kotlin file. Returns its file facts and its call records.
fn read(path: &str, src: &str) -> (KtFileRec, Vec<KtCall>) {
    let mut extractor = Extractor::new().expect("build extractor");
    let (_nodes, edges) = extractor.extract_file(path, src, "kotlin");
    let rec = edges
        .iter()
        .find_map(|e| match &e.kt {
            Some(KtRec::File(r)) => Some((**r).clone()),
            _ => None,
        })
        .expect("file record");
    let calls = edges
        .iter()
        .filter_map(|e| match &e.kt {
            Some(KtRec::Cross(c)) => Some(c.clone()),
            _ => None,
        })
        .collect();
    (rec, calls)
}

fn facts(src: &str) -> KtFileRec {
    read("A.kt", src).0
}

fn call(src: &str, name: &str) -> KtCall {
    read("A.kt", src)
        .1
        .into_iter()
        .find(|c| c.name == name)
        .expect("call record")
}

fn ty<'a>(rec: &'a KtFileRec, name: &str) -> &'a KtType {
    rec.types.iter().find(|t| t.name == name).expect("type")
}

#[test]
fn kt_file_facts_read_the_package_and_the_imports() {
    let rec = facts("package a.b\n\nimport x.y.Z\nimport x.y.*\nimport x.y.W as V\n\nfun f() {}\n");
    assert_eq!(rec.pkg, "a.b");
    assert_eq!(rec.imports.len(), 3);
    assert_eq!(rec.imports[0].path, "x.y.Z");
    assert!(!rec.imports[0].star && rec.imports[0].alias.is_none());
    assert_eq!(rec.imports[1].path, "x.y");
    assert!(rec.imports[1].star);
    assert_eq!(rec.imports[2].path, "x.y.W");
    assert_eq!(rec.imports[2].alias.as_deref(), Some("V"));
    assert!(!rec.broken && !rec.script && !rec.gradle);
}

#[test]
fn kt_file_facts_flag_a_gradle_import_and_a_script() {
    let (rec, _) = read(
        "build.gradle.kts",
        "import org.gradle.api.Project\n\nfun f() {}\n",
    );
    assert!(rec.gradle && rec.script);
    let rec = facts("import org.gradle.*\n");
    assert!(rec.gradle);
    let rec = facts("import org.gradleish.Thing\n");
    assert!(!rec.gradle);
}

#[test]
fn kt_file_facts_read_top_level_functions() {
    let rec = facts(
        "private fun a(x: Int, y: String = \"\", vararg z: Int) {}\n\
         fun <T> Foo<T>.ext(a: Int) {}\n\
         expect fun e()\n",
    );
    let a = rec.funs.iter().find(|f| f.name == "a").expect("a");
    assert_eq!((a.n, a.d, a.vararg), (3, 1, true));
    assert!(a.mods.contains(&"private".to_string()));
    assert_eq!(a.ext_recv, None);
    assert_eq!(a.line, 1);
    let ext = rec.funs.iter().find(|f| f.name == "ext").expect("ext");
    assert_eq!(ext.ext_recv.as_deref(), Some("Foo"));
    assert_eq!((ext.n, ext.d, ext.vararg), (1, 0, false));
    let e = rec.funs.iter().find(|f| f.name == "e").expect("e");
    assert!(e.mods.contains(&"expect".to_string()));
}

#[test]
fn kt_file_facts_read_top_level_properties_with_a_receiver() {
    let rec = facts("val Foo.pp: Int get() = 1\nconst val K: Long = 2\nval plain = 3\n");
    let pp = rec.props.iter().find(|p| p.name == "pp").expect("pp");
    assert_eq!(pp.ext_recv.as_deref(), Some("Foo"));
    assert_eq!(pp.ty.as_deref(), Some("Int"));
    let k = rec.props.iter().find(|p| p.name == "K").expect("K");
    assert!(k.mods.contains(&"const".to_string()));
    assert_eq!(k.ext_recv, None);
    let plain = rec.props.iter().find(|p| p.name == "plain").expect("plain");
    assert_eq!(plain.ty, None);
}

#[test]
fn kt_file_facts_read_a_class_with_its_members() {
    let rec = facts(
        "@Serializable\nopen class A<T : Any>(val p: Int, q: String = \"\") : B(), C by d, E<T> {\n\
         \x20   companion object Named {\n        fun g() {}\n    }\n\
         \x20   val prop: Foo? = null\n\
         \x20   private fun m(a: Int) {}\n\
         \x20   override fun toString() = \"x\"\n\
         \x20   class Inner\n}\n",
    );
    let a = ty(&rec, "A");
    assert_eq!(a.kind, KtKind::Class);
    assert_eq!(a.line, 1);
    assert_eq!(a.mods, vec!["open".to_string()]);
    assert_eq!(a.annots, vec!["Serializable".to_string()]);
    assert_eq!(a.bases, vec!["B", "C", "E"]);
    assert_eq!(a.by_bases, vec!["C"]);
    assert_eq!(a.tparams, vec!["T"]);
    assert_eq!(a.companion.as_deref(), Some("Named"));
    let props: Vec<_> = a
        .props
        .iter()
        .map(|p| (p.name.as_str(), p.ty.as_deref()))
        .collect();
    assert_eq!(props, vec![("p", Some("Int")), ("prop", Some("Foo"))]);
    let methods: Vec<_> = a.methods.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(methods, vec!["m", "toString"]);
    assert!(a.methods[1].mods.contains(&"override".to_string()));
    let named = ty(&rec, "Named");
    assert_eq!(named.kind, KtKind::Companion);
    assert_eq!(named.outer.as_deref(), Some("A"));
    assert_eq!(named.methods[0].name, "g");
    assert_eq!(ty(&rec, "Inner").outer.as_deref(), Some("A"));
}

#[test]
fn kt_file_facts_read_interfaces_objects_and_enums() {
    let rec = facts(
        "sealed interface S : J\n\
         object O : Base()\n\
         data class D(val a: Int)\n\
         enum class Co(val x: Int) : I {\n    A(1),\n    B(2) {\n        override fun f() {}\n    };\n    fun z() {}\n}\n\
         companion object\n",
    );
    assert_eq!(ty(&rec, "S").kind, KtKind::Interface);
    assert_eq!(ty(&rec, "S").mods, vec!["sealed".to_string()]);
    assert_eq!(ty(&rec, "S").bases, vec!["J"]);
    assert_eq!(ty(&rec, "O").kind, KtKind::Object);
    assert_eq!(ty(&rec, "O").bases, vec!["Base"]);
    assert_eq!(ty(&rec, "D").mods, vec!["data".to_string()]);
    let co = ty(&rec, "Co");
    assert_eq!(co.kind, KtKind::Enum);
    assert!(co.mods.contains(&"enum".to_string()));
    assert_eq!(co.bases, vec!["I"]);
    assert_eq!(co.methods[0].name, "z");
}

#[test]
fn kt_file_facts_read_a_typealias() {
    let rec = facts("typealias TA = List<Int>\n");
    assert_eq!(rec.aliases.len(), 1);
    assert_eq!(rec.aliases[0].name, "TA");
    assert_eq!(rec.aliases[0].line, 1);
}

#[test]
fn kt_file_facts_read_bodies_with_supertypes_and_overrides() {
    let rec = facts(
        "enum class E {\n    X {\n        override fun a() {}\n    },\n    Y\n}\n\
         fun main() {\n\
         \x20   val o = object : Foo(), Bar {\n        override fun run() {}\n        override val z = 1\n        fun other() {}\n    }\n\
         \x20   class Loc : I {\n        override fun f() {}\n    }\n\
         }\n",
    );
    assert_eq!(rec.bodies.len(), 3);
    let entry = &rec.bodies[0];
    assert_eq!(entry.kind, KtBodyKind::Entry);
    assert_eq!(entry.name, "X");
    assert_eq!(entry.bases, vec!["E"]);
    assert_eq!(entry.overrides, vec!["a"]);
    let anon = &rec.bodies[1];
    assert_eq!(anon.kind, KtBodyKind::Anon);
    assert_eq!(anon.bases, vec!["Foo", "Bar"]);
    assert_eq!(anon.overrides, vec!["run", "z"]);
    let local = &rec.bodies[2];
    assert_eq!(local.kind, KtBodyKind::Local);
    assert_eq!(local.name, "Loc");
    assert_eq!(local.bases, vec!["I"]);
    assert_eq!(local.overrides, vec!["f"]);
}

#[test]
fn kt_file_facts_read_a_one_line_anonymous_object() {
    // The grammar reads the body of this object as a lambda.
    let rec = facts("fun main() {\n    val o = object : Runnable { override fun run() {} }\n}\n");
    assert_eq!(rec.bodies.len(), 1);
    assert_eq!(rec.bodies[0].kind, KtBodyKind::Anon);
    assert_eq!(rec.bodies[0].bases, vec!["Runnable"]);
    assert_eq!(rec.bodies[0].overrides, vec!["run"]);
}

#[test]
fn kt_file_ids_come_from_the_nodes() {
    let rec = facts("class A {\n    fun m() {}\n}\nfun top() {}\n");
    assert!(!ty(&rec, "A").id.is_empty());
    assert!(!ty(&rec, "A").methods[0].id.is_empty());
    assert!(!rec.funs[0].id.is_empty());
}

#[test]
fn kt_call_shapes_arity_and_loose() {
    let src = "fun main() {\n    f()\n    this.g(1)\n    super.h()\n    x.i(1, 2) { }\n    j { }\n    k(1, n = 2)\n    l(*a)\n}\n";
    let (_, calls) = read("A.kt", src);
    let by = |n: &str| calls.iter().find(|c| c.name == n).expect("call");
    assert_eq!(by("f").shape, KtShape::Bare);
    assert_eq!(by("f").arity, 0);
    assert_eq!(by("g").shape, KtShape::This);
    assert_eq!(by("g").arity, 1);
    assert_eq!(by("h").shape, KtShape::Super);
    let i = by("i");
    assert_eq!(
        (i.shape, i.arity, i.recv.as_deref()),
        (KtShape::Name, 3, Some("x"))
    );
    assert_eq!(by("j").arity, 1);
    assert!(by("k").loose && by("l").loose);
    assert!(!i.loose && !by("f").loose);
    assert_eq!(by("k").arity, 2);
}

#[test]
fn kt_call_context_lists_the_scopes_from_the_inside_out() {
    let src = "class A {\n    companion object {\n        fun c() { h { z() } }\n    }\n\
               \x20   fun Foo.m() { y() }\n}\n";
    let (_, calls) = read("A.kt", src);
    let z = calls.iter().find(|c| c.name == "z").expect("z");
    assert_eq!(
        z.ctx,
        vec![
            KtScope::Lambda {
                call: "h".into(),
                recv: false,
                bound: false
            },
            KtScope::Fun {
                name: "c".into(),
                ext: None,
                tparams: vec![]
            },
            KtScope::Companion {
                name: "Companion".into(),
                line: 2
            },
            KtScope::Class {
                name: "A".into(),
                line: 1
            },
        ]
    );
    let y = calls.iter().find(|c| c.name == "y").expect("y");
    assert_eq!(
        y.ctx[0],
        KtScope::Fun {
            name: "m".into(),
            ext: Some("Foo".into()),
            tparams: vec![]
        }
    );
}

#[test]
fn kt_call_context_marks_a_lambda_that_a_member_call_gets() {
    let c = call("fun main() {\n    xs.map { z() }\n}\n", "z");
    assert_eq!(
        c.ctx[0],
        KtScope::Lambda {
            call: "map".into(),
            recv: true,
            bound: false
        }
    );
}

#[test]
fn kt_call_context_points_into_the_bodies() {
    let src = "enum class E {\n    X {\n        override fun a() { q() }\n    }\n}\n\
               fun main() {\n    val o = object : T {\n        fun b() { r() }\n    }\n    class L {\n        fun c() { s() }\n    }\n}\n";
    let (rec, calls) = read("A.kt", src);
    let body_of = |n: &str| {
        let c = calls.iter().find(|c| c.name == n).expect("call");
        c.ctx
            .iter()
            .find_map(|s| match s {
                KtScope::Body { idx } => Some(rec.bodies[*idx as usize].kind),
                _ => None,
            })
            .expect("body scope")
    };
    assert_eq!(body_of("q"), KtBodyKind::Entry);
    assert_eq!(body_of("r"), KtBodyKind::Anon);
    assert_eq!(body_of("s"), KtBodyKind::Local);
}

fn stop(src: &str, name: &str) -> Option<String> {
    call(src, name).stop
}

#[test]
fn kt_binder_a_param_stops_a_bare_call() {
    assert_eq!(
        stop("fun g(f: () -> Unit) {\n    f()\n}\n", "f").as_deref(),
        Some("param")
    );
    assert_eq!(stop("fun g() {\n    f()\n}\n", "f"), None);
}

#[test]
fn kt_binder_a_local_val_stops_a_bare_call() {
    let src = "fun g() {\n    val f = make()\n    f()\n}\n";
    assert_eq!(stop(src, "f").as_deref(), Some("local"));
}

#[test]
fn kt_binder_a_destructuring_stops_a_bare_call() {
    let src = "fun g() {\n    val (a, f) = pair\n    f()\n}\n";
    assert_eq!(stop(src, "f").as_deref(), Some("destructure"));
}

#[test]
fn kt_binder_a_for_var_stops_a_bare_call() {
    let src = "fun g() {\n    for (f in fs) {\n        f()\n    }\n}\n";
    assert_eq!(stop(src, "f").as_deref(), Some("for"));
}

#[test]
fn kt_binder_a_catch_param_stops_a_bare_call() {
    let src = "fun g() {\n    try {\n        run()\n    } catch (f: Exception) {\n        f()\n    }\n}\n";
    assert_eq!(stop(src, "f").as_deref(), Some("catch"));
}

#[test]
fn kt_binder_a_when_subject_val_stops_a_bare_call() {
    let src = "fun g() {\n    when (val f = x()) {\n        else -> f()\n    }\n}\n";
    assert_eq!(stop(src, "f").as_deref(), Some("when"));
}

#[test]
fn kt_binder_a_lambda_param_stops_a_bare_call() {
    let src = "fun g() {\n    xs.map { f -> f() }\n}\n";
    assert_eq!(stop(src, "f").as_deref(), Some("lambda_param"));
}

#[test]
fn kt_binder_it_stops_a_call_to_it() {
    let src = "fun g() {\n    xs.map {\n        it(1)\n    }\n}\n";
    assert_eq!(stop(src, "it").as_deref(), Some("it"));
    let src = "fun g() {\n    xs.map { x ->\n        it(1)\n    }\n}\n";
    assert_eq!(stop(src, "it"), None);
}

#[test]
fn kt_binder_an_anonymous_function_param_stops_a_bare_call() {
    let src = "fun g() {\n    val h = fun(f: Int) {\n        f()\n    }\n}\n";
    assert_eq!(stop(src, "f").as_deref(), Some("anon_param"));
}

#[test]
fn kt_binder_a_local_function_stops_a_bare_call() {
    let src = "fun g() {\n    fun f() {}\n    f()\n}\n";
    assert_eq!(stop(src, "f").as_deref(), Some("local_fun"));
}

#[test]
fn kt_binder_a_binder_of_a_closed_scope_does_not_stop_a_call() {
    let src = "fun g() {\n    xs.map { f -> f }\n    f()\n}\n";
    assert_eq!(stop(src, "f"), None);
}

#[test]
fn kt_binder_stops_a_member_call_of_the_same_name() {
    let src = "fun g(f: Int) {\n    x.f()\n}\n";
    assert_eq!(stop(src, "f").as_deref(), Some("param"));
}

#[test]
fn kt_call_a_label_on_this_stops_the_call() {
    let src = "class K {\n    fun go() {\n        this@K.go()\n        this.go()\n    }\n}\n";
    let (_, calls) = read("A.kt", src);
    let stops: Vec<_> = calls.iter().map(|c| c.stop.as_deref()).collect();
    assert_eq!(stops, vec![Some("label"), None]);
}

#[test]
fn kt_lost_names_take_the_plain_fun_name() {
    assert_eq!(lost("fun f() {}\n"), vec!["f"]);
}

#[test]
fn kt_lost_names_skip_a_type_param_list() {
    assert_eq!(lost("fun <T> f(x: T) {}\n"), vec!["f"]);
}

#[test]
fn kt_lost_names_skip_a_receiver_type() {
    assert_eq!(lost("fun Foo.f() {}\n"), vec!["f"]);
    assert_eq!(lost("fun a.b.Foo.f() {}\n"), vec!["f"]);
}

#[test]
fn kt_lost_names_skip_a_generic_receiver_type() {
    assert_eq!(lost("fun <T> Foo<T>.f() {}\n"), vec!["f"]);
    assert_eq!(lost("fun Map<String, List<Int>>.f() {}\n"), vec!["f"]);
    assert_eq!(lost("fun Foo<(A) -> B>.f() {}\n"), vec!["f"]);
}

#[test]
fn kt_lost_names_skip_a_nullable_receiver_type() {
    assert_eq!(lost("fun Foo?.f() {}\n"), vec!["f"]);
}

#[test]
fn kt_lost_names_skip_backticks() {
    assert_eq!(lost("fun `my test`() {}\n"), vec!["my test"]);
    assert_eq!(lost("fun Foo.`is`() {}\n"), vec!["is"]);
}

#[test]
fn kt_lost_names_take_the_names_after_the_declaration_words() {
    let names =
        lost("class C\nobject O\ninterface I\nval v = 1\nvar w = 2\nval Foo.pp: Int get() = 1\n");
    assert_eq!(names, vec!["C", "I", "O", "pp", "v", "w"]);
}

#[test]
fn kt_lost_names_take_an_override_fun() {
    assert_eq!(lost("override fun run() {}\n"), vec!["run"]);
}

#[test]
fn kt_lost_names_leave_out_an_anonymous_function_and_an_anonymous_object() {
    assert_eq!(
        lost("val a = fun(x: Int) {}\nval b = object : T {}\n"),
        vec!["a", "b"]
    );
}

#[test]
fn kt_lost_names_take_the_name_of_a_fun_interface() {
    assert_eq!(lost("fun interface F {\n    fun a()\n}\n"), vec!["F", "a"]);
}

/// The lost names of a source text that the test marks broken.
fn lost(src: &str) -> Vec<String> {
    super::lost_names(src)
}

#[test]
fn kt_broken_an_error_node_marks_the_file() {
    let rec = facts("class A {\n    fun f( {\n}\n");
    assert!(rec.broken);
    assert!(rec.lost.contains(&"A".to_string()));
    assert!(rec.lost.contains(&"f".to_string()));
}

#[test]
fn kt_broken_a_context_line_marks_the_file() {
    let rec = facts("context(Ctx)\nfun f() {}\n");
    assert!(rec.broken);
    assert!(rec.lost.contains(&"f".to_string()));
    let rec = facts("    context(Ctx) fun g() {}\n");
    assert!(rec.broken);
}

#[test]
fn kt_file_facts_read_a_fun_interface() {
    let rec = facts("fun interface F {\n    fun a()\n}\n");
    assert!(!rec.broken);
    assert_eq!(ty(&rec, "F").kind, KtKind::Interface);
    assert_eq!(ty(&rec, "F").mods, vec!["fun".to_string()]);
}

#[test]
fn kt_broken_a_header_without_a_declaration_node_marks_the_file() {
    // The grammar reads this one-line object as an infix call and gives no
    // error node.
    let rec = facts("object O { fun f() {} }\n");
    assert!(rec.broken);
    assert!(rec.lost.contains(&"O".to_string()));
    assert!(rec.lost.contains(&"f".to_string()));
}

#[test]
fn kt_broken_a_header_in_a_comment_or_a_string_does_not_mark_the_file() {
    let src = "/*\nclass Fake {\n*/\nval s = \"\"\"\nobject Fake2\ncontext(x)\n\"\"\"\n// class Fake3\nfun f() {}\n";
    let rec = facts(src);
    assert!(!rec.broken, "{:?}", rec.lost);
    assert!(rec.lost.is_empty());
}

#[test]
fn kt_binder_a_setter_param_stops_a_bare_call() {
    let src = "class C {\n    var x: Int = 0\n        set(f) {\n            f()\n        }\n    init {\n        g()\n    }\n}\n";
    assert_eq!(stop(src, "f").as_deref(), Some("setter_param"));
    assert_eq!(stop(src, "g"), None);
}

#[test]
fn kt_file_facts_read_a_type_inside_an_anonymous_object() {
    for src in [
        "fun a() {\n    val o = object : T {\n        class Inner {\n            fun z() {}\n        }\n    }\n}\n",
        "fun a() { val o = object : T { class Inner { fun z() {} } } }\n",
    ] {
        let rec = facts(src);
        let inner = ty(&rec, "Inner");
        assert!(inner.local, "{src}");
        assert_eq!(inner.methods[0].name, "z", "{src}");
    }
}

#[test]
fn kt_file_facts_read_a_type_inside_a_local_class() {
    let rec = facts("fun a() {\n    class L {\n        class N {\n            fun z() {}\n        }\n    }\n}\n");
    let n = ty(&rec, "N");
    assert!(n.local);
    assert_eq!(n.outer.as_deref(), Some("L"));
    assert_eq!(n.methods[0].name, "z");
    assert!(rec.types.iter().all(|t| t.name != "L"));
}

#[test]
fn kt_file_facts_read_a_type_inside_an_enum_entry_body() {
    let src =
        "enum class E {\n    X {\n        class Q {\n            fun z() {}\n        }\n    }\n}\n";
    let rec = facts(src);
    assert!(ty(&rec, "Q").local);
    assert!(!ty(&rec, "E").local);
}

#[test]
fn kt_call_in_a_nested_local_type_names_a_matching_fact() {
    let src = "fun a() {\n    val o = object : T {\n        class Inner {\n            fun z() { w() }\n        }\n    }\n}\n";
    let (rec, calls) = read("A.kt", src);
    let w = calls.iter().find(|c| c.name == "w").expect("w");
    for s in &w.ctx {
        match s {
            KtScope::Class { name, line } => {
                assert!(rec.types.iter().any(|t| &t.name == name && t.line == *line));
            }
            KtScope::Body { idx } => assert!((*idx as usize) < rec.bodies.len()),
            _ => {}
        }
    }
}

#[test]
fn kt_lost_names_take_destructured_names() {
    assert_eq!(lost("val (f, g) = pair\n"), vec!["f", "g"]);
    assert_eq!(lost("var (a, _, c: Int) = t\n"), vec!["a", "c"]);
}

#[test]
fn kt_lost_names_take_typealias_names() {
    assert_eq!(lost("typealias f = List<Int>\n"), vec!["f"]);
}

#[test]
fn kt_guard_knows_every_modifier_before_a_declaration() {
    for line in [
        "inline class IC(val v: Int)",
        "value class V(val v: Int)",
        "@JvmInline value class V(val v: Int)",
        "public abstract class A",
        "sealed interface S",
        "enum class E",
        "annotation class An",
        "inner class In",
        "expect class X",
        "actual object O",
        "external fun f()",
        "const val K = 1",
        "lateinit var v: Int",
        "override fun f()",
        "operator fun plus(o: A)",
        "infix fun to(o: A)",
        "tailrec fun f()",
        "suspend fun f()",
        "internal data class D(val a: Int)",
        "final override val x = 1",
    ] {
        assert!(header_line(line).is_some(), "{line}");
    }
    assert!(header_line("value.foo()").is_none());
    assert!(header_line("suspend {").is_none());
}

#[test]
fn kt_guard_marks_a_file_when_a_fun_val_or_var_header_has_no_node() {
    let mut marks = Marks::default();
    for line in ["fun f() {}", "val x = 1", "  var y = 2", "inline class IC"] {
        assert!(marks.guard_broken(&format!("{line}\n")), "{line}");
    }
    marks.member_rows.insert(0);
    marks.keyword_rows.insert(0);
    for line in ["fun f() {}", "val x = 1", "var y = 2", "inline class IC"] {
        assert!(!marks.guard_broken(&format!("{line}\n")), "{line}");
    }
}

#[test]
fn kt_guard_leaves_a_valid_file_with_local_declarations_alone() {
    let src = "class A(\n    val x: Int,\n    var y: Int = 0,\n) {\n    @Suppress(\"x\")\n    fun f() {\n        val a = 1\n        var b = 2\n        xs.map {\n            fun g() = a\n            val c = g()\n            c\n        }\n        when (\n            val s = b\n        ) {\n            else -> s\n        }\n        val h =\n            fun(z: Int) = z\n    }\n}\n";
    let rec = facts(src);
    assert!(!rec.broken, "{:?}", rec.lost);
}

#[test]
fn kt_guard_skips_a_block_comment_that_closes_on_the_line() {
    let mut marks = Marks::default();
    assert!(marks.guard_broken("/* c */ class A\n"));
    assert!(marks.guard_broken("/* a */ /* b */ fun f() {}\n"));
    marks.keyword_rows.insert(0);
    assert!(!marks.guard_broken("/* c */ class A\n"));
    assert!(!facts("/* c */ class A\n").broken);
    assert!(!facts("/* open\nclass A\n*/\nfun f() {}\n").broken);
}

#[test]
fn kt_file_facts_strip_backticks_from_names() {
    let src = "class T {\n    fun `should wrap`() {}\n    fun `in`() {}\n    val `is`: Int = 1\n    class `Odd Name`\n}\nfun `top fun`() {}\nval `top val` = 2\n";
    let rec = facts(src);
    assert!(!rec.broken);
    let t = ty(&rec, "T");
    let methods: Vec<_> = t.methods.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(methods, vec!["should wrap", "in"]);
    assert_eq!(t.props[0].name, "is");
    assert_eq!(ty(&rec, "Odd Name").outer.as_deref(), Some("T"));
    assert_eq!(rec.funs[0].name, "top fun");
    assert_eq!(rec.props[0].name, "top val");
    assert!(!ty(&rec, "T").methods[0].id.is_empty());
}

#[test]
fn kt_call_strips_backticks_from_the_name_and_a_binder() {
    let c = call("fun g() {\n    `is`()\n}\n", "is");
    assert_eq!(c.name, "is");
    let src = "fun g(`in`: () -> Unit) {\n    `in`()\n}\n";
    assert_eq!(stop(src, "in").as_deref(), Some("param"));
}

#[test]
fn kt_broken_a_valid_file_is_not_broken() {
    let src = "package a\n\nimport x.Y\n\n@Serializable\ndata class D(\n    val a: Int,\n) : I {\n    companion object {\n        fun g() {}\n    }\n}\n\n\
               enum class E {\n    A,\n    B {\n        override fun f() {}\n    },\n}\n\n\
               val o = object : T {\n    override fun f() {}\n}\n\n\
               fun main() {\n    val x =\n        object : Runnable {\n            override fun run() {}\n        }\n    class L\n}\n";
    let rec = facts(src);
    assert!(!rec.broken);
    assert!(rec.lost.is_empty());
}

/// The `rty` of the call `x.f()` in a function body with the given lines.
fn rty(body: &str) -> Option<KtRty> {
    call(&format!("fun main() {{\n{body}\n}}\n"), "f").rty
}

fn stops(r: Option<KtRty>) -> String {
    match r {
        Some(KtRty::Stop(why)) => why,
        other => format!("not a stop: {other:?}"),
    }
}

#[test]
fn test_kt_recv_rty_reads_a_typed_param() {
    let c = call("fun g(x: Foo?, y: Bar) {\n    x.f()\n}\n", "f");
    assert_eq!(
        c.rty,
        Some(KtRty::Param {
            ty: "Foo".into(),
            nullable: true
        })
    );
}

#[test]
fn test_kt_recv_rty_is_none_without_a_binder() {
    assert_eq!(rty("    x.f()"), None);
}

#[test]
fn test_kt_recv_rty_vararg_param_stops() {
    let c = call("fun g(vararg x: Foo) {\n    x.f()\n}\n", "f");
    assert_eq!(stops(c.rty), "vararg");
}

#[test]
fn test_kt_recv_rty_reads_a_typed_val_and_a_new() {
    assert_eq!(
        rty("    val x: Foo = make()\n    x.f()"),
        Some(KtRty::Val {
            ty: "Foo".into(),
            nullable: false
        })
    );
    assert_eq!(
        rty("    val x = Foo<Int>(1)\n    x.f()"),
        Some(KtRty::New { ty: "Foo".into() })
    );
}

#[test]
fn test_kt_recv_rty_new_needs_one_call_expression() {
    for init in [
        "Foo().also { }",
        "Foo() ?: other",
        "Foo()!!",
        "foo()",
        "a.Foo()",
    ] {
        let r = rty(&format!("    val x = {init}\n    x.f()"));
        assert_eq!(stops(r), "untyped", "{init}");
    }
}

#[test]
fn test_kt_recv_rty_unusable_types_stop() {
    assert_eq!(
        stops(rty("    val x: (Int) -> Int = g\n    x.f()")),
        "local_type"
    );
    assert_eq!(stops(rty("    val x: a.Foo = g\n    x.f()")), "local_type");
    let c = call("fun g(x: a.Foo) {\n    x.f()\n}\n", "f");
    assert_eq!(stops(c.rty), "param_type");
    let c = call("fun g(x: () -> Unit) {\n    x.f()\n}\n", "f");
    assert_eq!(stops(c.rty), "param_type");
}

#[test]
fn test_kt_recv_rty_other_binders_stop() {
    assert_eq!(stops(rty("    var x: Foo = g()\n    x.f()")), "var");
    assert_eq!(
        stops(rty("    for (x in xs) {\n        x.f()\n    }")),
        "for"
    );
    assert_eq!(stops(rty("    val (x, y) = p()\n    x.f()")), "destructure");
    assert_eq!(
        stops(rty(
            "    try { g() } catch (x: Exception) {\n        x.f()\n    }"
        )),
        "catch"
    );
    assert_eq!(
        stops(rty("    xs.map { x ->\n        x.f()\n    }")),
        "lambda_param"
    );
    assert_eq!(stops(rty("    xs.map {\n        it.f()\n    }")), "it");
    assert_eq!(
        stops(rty(
            "    when (val x = g()) {\n        else -> x.f()\n    }"
        )),
        "when"
    );
    assert_eq!(stops(rty("    fun x() = 1\n    x.f()")), "local_fun");
    assert_eq!(
        stops(rty("    val x by lazy { Foo() }\n    x.f()")),
        "untyped"
    );
}

#[test]
fn test_kt_recv_rty_binder_after_the_call_stops() {
    let r = rty("    x.f()\n    val x: Foo = g()");
    assert_eq!(stops(r), "binder_after_call");
}

#[test]
fn test_kt_recv_rty_two_binders_stop() {
    let r = rty("    val x: Foo = g()\n    xs.map { x ->\n        x.f()\n    }");
    assert_eq!(stops(r), "two_binders");
    let r = call("fun g(x: Foo) {\n    val x: Foo = h()\n    x.f()\n}\n", "f").rty;
    assert_eq!(stops(r), "two_binders");
}

#[test]
fn test_kt_recv_cast_scans_the_outermost_declaration() {
    let f = |body: &str| call(&format!("fun g(x: Foo) {{\n    x.f()\n{body}\n}}\n"), "f").cast;
    assert!(f("    if (x is Bar) {}"));
    assert!(f("    if (x !is Bar) {}"));
    assert!(f("    val y = x as Bar"));
    assert!(f("    val y = x as? Bar"));
    assert!(f(
        "    when (x) {\n        is Bar -> 1\n        else -> 2\n    }"
    ));
    assert!(!f("    when (x) {\n        else -> 2\n    }"));
    assert!(!f("    if (y is Bar) {}"));
    // The scan covers the outer declaration, not only the inner function.
    let src = "class K {\n    fun a(x: Foo) {\n        x.f()\n    }\n    fun b(x: Any) {\n        x as Bar\n    }\n}\n";
    assert!(call(src, "f").cast);
    // A bare call never sets the flag.
    assert!(!call("fun g(x: Foo) {\n    f()\n    x as Bar\n}\n", "f").cast);
}

#[test]
fn test_kt_recv_fun_scope_lists_its_type_params() {
    let c = call("fun <T, U> g() {\n    f()\n}\n", "f");
    assert_eq!(
        c.ctx[0],
        KtScope::Fun {
            name: "g".into(),
            ext: None,
            tparams: vec!["T".into(), "U".into()]
        }
    );
}

#[test]
fn test_kt_recv_enum_entries_are_listed() {
    let rec =
        facts("enum class E {\n    A, B { override fun m() {} };\n    fun m() {}\n}\nclass C\n");
    assert_eq!(
        ty(&rec, "E").entries,
        vec!["A".to_string(), "B".to_string()]
    );
    assert!(ty(&rec, "C").entries.is_empty());
}

#[test]
fn test_kt_recv_super_with_a_type_or_a_label_stops() {
    let src = "class A : B() {\n    override fun f() {\n        super<B>.f()\n    }\n}\n";
    let c = call(src, "f");
    assert!(c.stop.is_some() && c.shape == KtShape::Super);
    let src = "class A : B() {\n    override fun f() {\n        super@A.f()\n    }\n}\n";
    let c = call(src, "f");
    assert!(c.stop.is_some() && c.shape == KtShape::Super);
}

#[test]
fn test_kt_recv_binder_stop_runs_for_a_member_call() {
    let c = call("fun g(x: Foo) {\n    fun f() {}\n    x.f()\n}\n", "f");
    assert!(c.stop.is_some());
    assert!(call("fun g(x: Foo) {\n    x.f()\n}\n", "f").stop.is_none());
}

#[test]
fn test_kt_recv_body_records_all_its_function_names() {
    let src = "fun mk() {\n    val o = object : B() {\n        override fun a() {}\n        fun b() {}\n        val c = 1\n    }\n}\n";
    let rec = facts(src);
    assert_eq!(rec.bodies[0].overrides, vec!["a".to_string()]);
    assert_eq!(rec.bodies[0].funs, vec!["a".to_string(), "b".to_string()]);
}

#[test]
fn test_kt_recv_rty_ctor_param_seen_from_a_method_is_no_binder() {
    let src = "open class Base { val x: Bar = Bar() }\nclass K(x: Foo) : Base() {\n    fun g() {\n        x.f()\n    }\n}\n";
    assert_eq!(call(src, "f").rty, None);
}

#[test]
fn test_kt_recv_rty_ctor_param_outside_a_nested_class_is_no_binder() {
    let src = "class Outer(val x: Foo) {\n    class Nested {\n        fun g() {\n            x.f()\n        }\n    }\n}\n";
    assert_eq!(call(src, "f").rty, None);
}

#[test]
fn test_kt_recv_rty_binder_outside_an_anonymous_object_stops() {
    let src = "fun m() {\n    val x: A = a()\n    object : Base() {\n        fun g() {\n            x.f()\n        }\n    }\n}\n";
    assert_eq!(stops(call(src, "f").rty), "class_boundary");
    let src = "fun m(x: A) {\n    class L : Base() {\n        fun g() {\n            x.f()\n        }\n    }\n}\n";
    assert_eq!(stops(call(src, "f").rty), "class_boundary");
}

#[test]
fn test_kt_recv_rty_ctor_param_in_a_property_initializer_is_a_binder() {
    let src = "class K(x: Foo) {\n    val a = x.f()\n}\n";
    assert_eq!(
        call(src, "f").rty,
        Some(KtRty::Param {
            ty: "Foo".into(),
            nullable: false
        })
    );
    let src = "class K(x: Foo) {\n    init {\n        x.f()\n    }\n}\n";
    assert!(matches!(call(src, "f").rty, Some(KtRty::Param { .. })));
}

#[test]
fn test_kt_link_the_line_of_a_name_call_is_the_line_of_the_name() {
    let src = "fun g(x: Foo) {\n    x\n        .f()\n}\n";
    assert_eq!(call(src, "f").line, 3);
}

#[test]
fn test_kt_link_the_line_of_this_and_super_calls_is_the_line_of_the_name() {
    let src = "class K {\n    fun g() {\n        this\n            .f()\n    }\n}\n";
    assert_eq!(call(src, "f").line, 4);
    let src = "class K : B() {\n    fun g() {\n        super\n            .f()\n    }\n}\n";
    assert_eq!(call(src, "f").line, 4);
}

#[test]
fn test_kt_link_f2_a_binder_named_like_the_new_type_gives_stop() {
    let src = "fun g() {\n    fun T() = U()\n    val x = T()\n    x.f()\n}\n";
    assert_eq!(stops(call(src, "f").rty), "new_shadowed");
}

#[test]
fn test_kt_recv_rty_ctor_val_is_a_property_and_stops() {
    let src = "class K(val x: Foo) {\n    val a = x.f()\n}\n";
    assert_eq!(stops(call(src, "f").rty), "ctor_prop");
}

#[test]
fn test_kt_recv_cast_covers_a_ctor_default_and_a_property_initializer() {
    let src = "class K(x: Any, val a: Int = if (x is D) x.f() else 0)\n";
    assert!(call(src, "f").cast);
    let src = "val p = if (x is D) x.f() else 0\n";
    assert!(call(src, "f").cast);
}

#[test]
fn test_kt_recv_cast_scan_skips_quote_marks_in_strings_and_comments() {
    let src = "fun g(x: Foo) {\n    x.f()\n    val s = \"`\"\n    val c = '`'\n    // a ` here\n    if (x is Bar) {}\n}\n";
    assert!(call(src, "f").cast);
    // A cast word in a string or a comment is no test.
    let src = "fun g(x: Foo) {\n    x.f()\n    val s = \"x is Bar\"\n    // x as Bar\n}\n";
    assert!(!call(src, "f").cast);
    // The code in a string template stays.
    let src = "fun g(x: Foo) {\n    x.f()\n    val s = \"${x as Bar}\"\n}\n";
    assert!(call(src, "f").cast);
}

#[test]
fn test_kt_recv_cast_marks_x_before_the_keyword_only() {
    let src = "fun g(x: Foo, y: Any) {\n    x.f()\n    if (y is x) {}\n}\n";
    assert!(!call(src, "f").cast);
    let src = "fun g(x: Foo) {\n    x.f()\n    val y = x as? Bar ?: return\n}\n";
    assert!(call(src, "f").cast);
    let src = "fun g(x: Foo) {\n    x.f()\n    if (x !is Bar) return\n}\n";
    assert!(call(src, "f").cast);
}

#[test]
fn test_kt_recv_cast_known_gap_parenthesized_and_bang_bang() {
    // Known gap: the token scan misses `(x) as T` and `x!! is T`. The
    // subtype walk of the dispatch proof covers the risk.
    let src = "fun g(x: Foo) {\n    x.f()\n    val y = (x) as Bar\n}\n";
    assert!(!call(src, "f").cast);
    let src = "fun g(x: Foo?) {\n    x.f()\n    if (x!! is Bar) {}\n}\n";
    assert!(!call(src, "f").cast);
}

#[test]
fn test_kt_recv_rty_setter_param() {
    let src =
        "class K {\n    var p: Int = 0\n        set(x: Int) {\n            x.f()\n        }\n}\n";
    assert!(matches!(call(src, "f").rty, Some(KtRty::Param { .. })));
}

#[test]
fn test_kt_recv_rty_new_with_a_trailing_lambda_and_a_nullable_type_arg() {
    assert_eq!(
        rty("    val x = Foo { }\n    x.f()"),
        Some(KtRty::New { ty: "Foo".into() })
    );
    assert_eq!(
        rty("    val x: Foo<Int>? = g()\n    x.f()"),
        Some(KtRty::Val {
            ty: "Foo".into(),
            nullable: true
        })
    );
}

#[test]
fn test_kt_recv_rty_val_in_a_try_block_and_an_if_branch() {
    assert!(matches!(
        rty("    try {\n        val x: Foo = g()\n        x.f()\n    } catch (e: Exception) {}"),
        Some(KtRty::Val { .. })
    ));
    assert!(matches!(
        rty("    if (c) {\n        val x: Foo = g()\n        x.f()\n    }"),
        Some(KtRty::Val { .. })
    ));
}

#[test]
fn test_kt_recv_rty_for_destructuring_stops() {
    assert_eq!(
        stops(rty("    for ((x, b) in xs) {\n        x.f()\n    }")),
        "destructure"
    );
}

#[test]
fn test_kt_scope_two_calls_in_one_function_read_the_same_cached_scopes() {
    let src = "fun <T> g(x: Foo) {\n    x.f()\n    x.h()\n}\n";
    let (f, h) = (call(src, "f"), call(src, "h"));
    assert_eq!(f.ctx, h.ctx);
    assert_eq!(f.ctx.len(), 1);
    assert!(f.cast == h.cast && !f.cast);
}
