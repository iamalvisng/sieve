//! Tests for the Rust crate table, the module tree and the lookup (RSX1).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sieve_core::{Confidence, Relation};

use super::{packages, parse_manifest, EditionSpec};
use crate::build::build_graph;

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("sieve-rsx1-{}-{n}", std::process::id()));
        fs::create_dir_all(&path).expect("create temp dir");
        TempDir(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const MANIFEST: &str = "[package]\nname = \"t\"\nedition = \"2021\"\n";

type Calls = Vec<(String, String, Confidence)>;

fn write(dir: &Path, rel: &str, text: &str) {
    let path = dir.join(rel);
    fs::create_dir_all(path.parent().expect("has parent")).expect("create dir");
    fs::write(path, text).expect("write file");
}

/// The `calls` edges of a built tree as `(source, target, confidence)`.
fn calls_in(dir: &Path) -> Calls {
    build_graph(dir, &dir.join("ctx"))
        .expect("build graph")
        .edges
        .into_iter()
        .filter(|e| e.relation == Relation::Calls)
        .map(|e| (e.source, e.target, e.confidence))
        .collect()
}

/// Builds a tree with the default manifest and gives its call edges.
fn calls(files: &[(&str, &str)]) -> Calls {
    let dir = TempDir::new();
    write(&dir.0, "Cargo.toml", MANIFEST);
    for (rel, text) in files {
        write(&dir.0, rel, text);
    }
    calls_in(&dir.0)
}

fn extracted(got: &Calls, src: &str, dst: &str) -> bool {
    got.iter()
        .any(|(s, t, c)| s == src && t == dst && *c == Confidence::Extracted)
}

fn any_extracted_to(got: &Calls, dst_file: &str) -> bool {
    got.iter()
        .any(|(_, t, c)| t.starts_with(dst_file) && *c == Confidence::Extracted)
}

#[test]
fn test_rsx1_use_simple() {
    let got = calls(&[
        ("src/lib.rs", "mod a;\nuse a::f;\nfn g() { f(); }\n"),
        ("src/a.rs", "pub fn f() {}\n"),
    ]);
    assert!(extracted(&got, "src/lib.rs#g", "src/a.rs#f"), "{got:?}");
}

#[test]
fn test_rsx1_use_group() {
    let got = calls(&[
        (
            "src/lib.rs",
            "mod a;\nuse a::{f, h};\nfn g() { f(); h(); }\n",
        ),
        ("src/a.rs", "pub fn f() {}\npub fn h() {}\n"),
    ]);
    assert!(extracted(&got, "src/lib.rs#g", "src/a.rs#f"), "{got:?}");
    assert!(extracted(&got, "src/lib.rs#g", "src/a.rs#h"), "{got:?}");
}

#[test]
fn test_rsx1_use_rename() {
    let got = calls(&[
        ("src/lib.rs", "mod a;\nuse a::f as k;\nfn g() { k(); }\n"),
        ("src/a.rs", "pub fn f() {}\n"),
    ]);
    assert!(extracted(&got, "src/lib.rs#g", "src/a.rs#f"), "{got:?}");
}

#[test]
fn test_rsx1_use_self_in_group() {
    let got = calls(&[
        (
            "src/lib.rs",
            "mod a;\nuse a::b::{self, h};\nfn g() { b::f(); h(); }\n",
        ),
        ("src/a.rs", "pub mod b;\n"),
        ("src/a/b.rs", "pub fn f() {}\npub fn h() {}\n"),
    ]);
    assert!(extracted(&got, "src/lib.rs#g", "src/a/b.rs#f"), "{got:?}");
    assert!(extracted(&got, "src/lib.rs#g", "src/a/b.rs#h"), "{got:?}");
}

#[test]
fn test_rsx1_super_crate_paths() {
    let got = calls(&[
        ("src/lib.rs", "mod a;\nmod b;\npub fn top() {}\n"),
        (
            "src/a.rs",
            "fn loc() {}\npub fn f() { crate::b::h(); super::top(); self::loc(); }\n",
        ),
        ("src/b.rs", "pub fn h() {}\n"),
    ]);
    assert!(extracted(&got, "src/a.rs#f", "src/b.rs#h"), "{got:?}");
    assert!(extracted(&got, "src/a.rs#f", "src/lib.rs#top"), "{got:?}");
    assert!(extracted(&got, "src/a.rs#f", "src/a.rs#loc"), "{got:?}");
}

#[test]
fn test_rsx1_mod_rs_vs_file_rs() {
    let got = calls(&[
        (
            "src/lib.rs",
            "mod a;\nmod b;\nfn g() { a::c::f(); b::d::f(); }\n",
        ),
        ("src/a/mod.rs", "pub mod c;\n"),
        ("src/a/c.rs", "pub fn f() {}\n"),
        ("src/b.rs", "pub mod d;\n"),
        ("src/b/d.rs", "pub fn f() {}\n"),
    ]);
    assert!(extracted(&got, "src/lib.rs#g", "src/a/c.rs#f"), "{got:?}");
    assert!(extracted(&got, "src/lib.rs#g", "src/b/d.rs#f"), "{got:?}");
}

#[test]
fn test_rsx1_path_attr() {
    let got = calls(&[
        (
            "src/lib.rs",
            "#[path = \"weird/x.rs\"]\nmod x;\nfn g() { x::f(); }\n",
        ),
        ("src/weird/x.rs", "pub fn f() {}\n"),
    ]);
    assert!(
        extracted(&got, "src/lib.rs#g", "src/weird/x.rs#f"),
        "{got:?}"
    );
}

#[test]
fn test_rsx1_two_mod_files_no_edge() {
    let got = calls(&[
        ("src/lib.rs", "mod a;\nfn g() { a::f(); }\n"),
        ("src/a.rs", "pub fn f() {}\n"),
        ("src/a/mod.rs", "pub fn f() {}\n"),
    ]);
    assert!(!any_extracted_to(&got, "src/a"), "{got:?}");
}

#[test]
fn test_rsx1_pub_use_chain() {
    let got = calls(&[
        (
            "src/lib.rs",
            "mod a;\nuse a::f;\nfn g() { f(); }\nfn h() { a::f(); }\n",
        ),
        ("src/a.rs", "mod b;\npub use b::f;\n"),
        ("src/a/b.rs", "pub fn f() {}\n"),
    ]);
    assert!(extracted(&got, "src/lib.rs#g", "src/a/b.rs#f"), "{got:?}");
    assert!(extracted(&got, "src/lib.rs#h", "src/a/b.rs#f"), "{got:?}");
}

#[test]
fn test_rsx1_glob_parent_tests_mod() {
    let got = calls(&[(
        "src/lib.rs",
        "fn helper() {}\n#[cfg(test)]\nmod tests {\n    use super::*;\n    fn t() { helper(); }\n}\n",
    )]);
    assert!(
        extracted(&got, "src/lib.rs#t", "src/lib.rs#helper"),
        "{got:?}"
    );
}

#[test]
fn test_rsx1_glob_conflict_no_edge() {
    let got = calls(&[
        (
            "src/lib.rs",
            "mod a;\nmod b;\nuse a::*;\nuse b::*;\nfn g() { f(); }\n",
        ),
        ("src/a.rs", "pub fn f() {}\n"),
        ("src/b.rs", "pub fn f() {}\n"),
    ]);
    assert!(!any_extracted_to(&got, "src/a.rs"), "{got:?}");
    assert!(!any_extracted_to(&got, "src/b.rs"), "{got:?}");
}

#[test]
fn test_rsx1_glob_item_macro_no_edge() {
    let tree = |a: &'static str| {
        calls(&[
            ("src/lib.rs", "mod a;\nuse a::*;\nfn g() { f(); }\n"),
            ("src/a.rs", a),
        ])
    };
    let control = tree("pub fn f() {}\n");
    assert!(
        extracted(&control, "src/lib.rs#g", "src/a.rs#f"),
        "{control:?}"
    );
    let got = tree("pub fn f() {}\nmake_items! { }\n");
    assert!(!any_extracted_to(&got, "src/a.rs"), "{got:?}");
}

#[test]
fn test_rsx1_external_glob_blocks() {
    let got = calls(&[
        (
            "src/lib.rs",
            "mod a;\nuse a::*;\nuse ext::*;\nfn g() { f(); }\n",
        ),
        ("src/a.rs", "pub fn f() {}\n"),
    ]);
    assert!(!any_extracted_to(&got, "src/a.rs"), "{got:?}");
}

#[test]
fn test_rsx1_fn_and_mod_same_name() {
    let got = calls(&[
        ("src/lib.rs", "mod f;\nfn f() {}\nfn g() { f(); f::h(); }\n"),
        ("src/f.rs", "pub fn h() {}\n"),
    ]);
    // The `mod f` node takes the id `f`, so the free fn gets the id `f~2`.
    assert!(extracted(&got, "src/lib.rs#g", "src/lib.rs#f~2"), "{got:?}");
    assert!(extracted(&got, "src/lib.rs#g", "src/f.rs#h"), "{got:?}");
}

#[test]
fn test_rsx1_rename_zero_drops() {
    let got = calls(&[
        (
            "src/lib.rs",
            "mod a;\nmod other;\nuse a::missing as g;\nfn run() { g(); }\n",
        ),
        ("src/a.rs", "pub fn f() {}\n"),
        ("src/other.rs", "pub fn g() {}\n"),
    ]);
    assert!(
        !got.iter().any(|(s, _, _)| s == "src/lib.rs#run"),
        "{got:?}"
    );
}

#[test]
fn test_rsx1_cfg_on_use_twin_no_edge() {
    let got = calls(&[
        (
            "src/lib.rs",
            "mod a;\nmod b;\n#[cfg(unix)]\nuse a::f;\n#[cfg(not(unix))]\nuse b::f;\nfn g() { f(); }\n",
        ),
        ("src/a.rs", "pub fn f() {}\n"),
        ("src/b.rs", "pub fn f() {}\n"),
    ]);
    assert!(!any_extracted_to(&got, "src/a.rs"), "{got:?}");
    assert!(!any_extracted_to(&got, "src/b.rs"), "{got:?}");
}

#[test]
fn test_rsx1_type_param_shadow_path_no_edge() {
    let tree = |body: &'static str| calls(&[("src/lib.rs", body), ("src/a.rs", "pub fn f() {}\n")]);
    let control = tree("mod a;\nfn g() { a::f(); }\n");
    assert!(
        extracted(&control, "src/lib.rs#g", "src/a.rs#f"),
        "{control:?}"
    );
    let got = tree("mod a;\nfn g<a: Tr>() { a::f(); }\n");
    assert!(!any_extracted_to(&got, "src/a.rs"), "{got:?}");
}

#[test]
fn test_rsx1_file_in_two_roots_no_edge() {
    let got = calls(&[
        ("src/lib.rs", "mod common;\nfn g() { common::f(); }\n"),
        ("src/main.rs", "mod common;\nfn main() {}\n"),
        ("src/common.rs", "pub fn f() {}\n"),
    ]);
    assert!(!any_extracted_to(&got, "src/common.rs"), "{got:?}");
}

#[test]
fn test_rsx1_unreadable_manifest_no_proof() {
    let files = [
        ("src/lib.rs", "mod a;\nuse a::f;\nfn g() { f(); }\n"),
        ("src/a.rs", "pub fn f() {}\n"),
    ];
    let good = calls(&files);
    assert!(extracted(&good, "src/lib.rs#g", "src/a.rs#f"), "{good:?}");
    let dir = TempDir::new();
    write(
        &dir.0,
        "Cargo.toml",
        &format!("{MANIFEST}this is not toml\n"),
    );
    for (rel, text) in files {
        write(&dir.0, rel, text);
    }
    let bad = calls_in(&dir.0);
    assert!(!any_extracted_to(&bad, "src/a.rs"), "{bad:?}");
}

#[test]
fn test_rsx1_manifest_scanner() {
    let m = parse_manifest(
        "[package]\nname = \"x-y\" # c\nedition.workspace = true\n\n[features]\nf = [\n  \"a\",\n]\n\n[[bin]]\nname = \"b\"\npath = \"tools/b.rs\"\n",
    )
    .expect("readable");
    assert_eq!(m.name.as_deref(), Some("x-y"));
    assert_eq!(m.edition, EditionSpec::Workspace);
    assert_eq!(m.paths, vec!["tools/b.rs".to_string()]);
    assert!(parse_manifest("[package]\nname = \"x\nz\"\n").is_none());
    assert!(parse_manifest("name = \"x\"\n").is_none());
}

#[test]
fn test_rsx1_package_roots_and_workspace_edition() {
    let manifests = vec![
        (
            String::new(),
            "[workspace.package]\nedition = \"2021\"\n".to_string(),
        ),
        (
            "k".to_string(),
            "[package]\nname = \"k\"\nedition.workspace = true\n".to_string(),
        ),
    ];
    let files: Vec<String> = [
        "k/src/lib.rs",
        "k/src/bin/t.rs",
        "k/src/bin/u/main.rs",
        "k/src/a.rs",
        "k/tests/it.rs",
        "k/build.rs",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let pkgs = packages(&manifests, &files);
    assert_eq!(pkgs.len(), 1);
    assert_eq!(pkgs[0].edition, 2021);
    assert_eq!(
        pkgs[0].roots,
        vec![
            "k/build.rs",
            "k/src/bin/t.rs",
            "k/src/bin/u/main.rs",
            "k/src/lib.rs",
            "k/tests/it.rs"
        ]
    );
}

#[test]
fn test_rsx1_fixture_workspace() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets the manifest dir");
    let root = Path::new(&manifest_dir).join("../../tests/fixtures/rsx");
    let got = calls_in(&root);
    assert!(
        extracted(&got, "alpha/src/lib.rs#run", "alpha/src/util.rs#clean"),
        "{got:?}"
    );
    assert!(
        extracted(
            &got,
            "alpha/src/lib.rs#run",
            "alpha/src/deep/inner.rs#deepest"
        ),
        "{got:?}"
    );
    assert!(
        extracted(&got, "beta/src/main.rs#main", "beta/src/m.rs#hello"),
        "{got:?}"
    );
    assert!(!any_extracted_to(&got, "beta/src/lib.rs"), "{got:?}");
    assert!(
        extracted(&got, "beta/src/main.rs#main", "alpha/src/lib.rs#exported"),
        "{got:?}"
    );
    assert!(
        extracted(
            &got,
            "beta/src/main.rs#main",
            "alpha/src/deep/inner.rs#deepest"
        ),
        "{got:?}"
    );
}

/// Like `calls`, with a given manifest text.
fn calls_with(manifest: &str, files: &[(&str, &str)]) -> Calls {
    let dir = TempDir::new();
    write(&dir.0, "Cargo.toml", manifest);
    for (rel, text) in files {
        write(&dir.0, rel, text);
    }
    calls_in(&dir.0)
}

#[test]
fn test_rsx1_failed_use_blocks_glob() {
    let got = calls(&[
        (
            "src/lib.rs",
            "mod a;\nmod b;\nuse a::f;\nuse b::*;\nfn g() { f(); }\n",
        ),
        ("src/a.rs", "pub fn other() {}\n"),
        ("src/b.rs", "pub fn f() {}\n"),
    ]);
    assert!(!any_extracted_to(&got, "src/b.rs"), "{got:?}");
}

#[test]
fn test_rsx1_use_and_mod_with_one_name() {
    // `mod escape` and `pub use crate::escape::escape` share a name in two
    // namespaces. The path walk through `escape` must not loop.
    let got = calls(&[
        (
            "src/lib.rs",
            "mod escape;\nmod user;\npub use crate::escape::escape;\n",
        ),
        ("src/escape.rs", "pub fn escape() {}\npub fn esc_os() {}\n"),
        (
            "src/user.rs",
            "use crate::escape::{escape, esc_os};\nfn g() { escape(); esc_os(); }\n",
        ),
    ]);
    assert!(
        extracted(&got, "src/user.rs#g", "src/escape.rs#escape"),
        "{got:?}"
    );
    assert!(
        extracted(&got, "src/user.rs#g", "src/escape.rs#esc_os"),
        "{got:?}"
    );
}

#[test]
fn test_rsx1_path_diamond_bounded() {
    // Each file loads the next one twice through `#[path]`.
    let mut files: Vec<(String, String)> = vec![(
        "src/lib.rs".to_string(),
        "#[path = \"d0.rs\"]\nmod p;\n#[path = \"d0.rs\"]\nmod q;\nfn g() { p::f(); }\n"
            .to_string(),
    )];
    for i in 0..14 {
        let body = format!(
            "pub fn f() {{}}\n#[path = \"d{n}.rs\"]\nmod p;\n#[path = \"d{n}.rs\"]\nmod q;\n",
            n = i + 1
        );
        files.push((format!("src/d{i}.rs"), body));
    }
    files.push(("src/d14.rs".to_string(), "pub fn f() {}\n".to_string()));
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let got = calls(&refs);
    assert!(!any_extracted_to(&got, "src/d0.rs"), "{got:?}");
}

#[test]
fn test_rsx1_edition_2015_stops() {
    let manifest = "[package]\nname = \"t\"\n";
    let got = calls_with(
        manifest,
        &[
            (
                "src/lib.rs",
                "mod a;\nuse a::f;\nfn g() { f(); }\nfn h() { crate::a::f(); }\n",
            ),
            ("src/a.rs", "pub fn f() {}\n"),
        ],
    );
    assert!(!extracted(&got, "src/lib.rs#g", "src/a.rs#f"), "{got:?}");
    assert!(extracted(&got, "src/lib.rs#h", "src/a.rs#f"), "{got:?}");
}

#[test]
fn test_rsx1_use_chain_limit_is_8_links() {
    let chain = |len: usize| {
        let mut files: Vec<(String, String)> = vec![(
            "src/lib.rs".to_string(),
            "use m0::f;\nfn g() { f(); }\n".to_string(),
        )];
        for i in 0..len {
            let body = if i + 1 == len {
                "pub fn f() {}\n".to_string()
            } else {
                format!("pub use crate::m{}::f;\n", i + 1)
            };
            files.push((format!("src/m{i}.rs"), body));
            files[0].1 = format!("mod m{i};\n{}", files[0].1);
        }
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        calls(&refs)
    };
    let short = chain(3);
    assert!(
        extracted(&short, "src/lib.rs#g", "src/m2.rs#f"),
        "{short:?}"
    );
    let long = chain(14);
    assert!(!any_extracted_to(&long, "src/m13.rs"), "{long:?}");
}

#[test]
fn test_rsx1_lookup_limit_is_256() {
    let tree = |globs: usize| {
        let mut lib = String::from("mod target;\nuse target::*;\n");
        let mut files: Vec<(String, String)> = Vec::new();
        for i in 0..globs {
            lib.push_str(&format!("mod e{i};\nuse e{i}::*;\n"));
            files.push((format!("src/e{i}.rs"), String::new()));
        }
        lib.push_str("fn g() { f(); }\n");
        files.push(("src/lib.rs".to_string(), lib));
        files.push(("src/target.rs".to_string(), "pub fn f() {}\n".to_string()));
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        calls(&refs)
    };
    let few = tree(3);
    assert!(
        extracted(&few, "src/lib.rs#g", "src/target.rs#f"),
        "{few:?}"
    );
    let many = tree(300);
    assert!(!any_extracted_to(&many, "src/target.rs"), "{many:?}");
}

#[test]
fn test_rsx1_pub_use_loop_terminates() {
    let got = calls(&[
        ("src/lib.rs", "mod a;\nmod b;\nuse a::f;\nfn g() { f(); }\n"),
        ("src/a.rs", "pub use crate::b::f;\n"),
        ("src/b.rs", "pub use crate::a::f;\n"),
    ]);
    assert!(!any_extracted_to(&got, "src/a.rs"), "{got:?}");
    assert!(!any_extracted_to(&got, "src/b.rs"), "{got:?}");
}

#[test]
fn test_rsx1_item_and_use_both_match_no_edge() {
    let got = calls(&[
        (
            "src/lib.rs",
            "mod a;\nuse a::f;\nfn f() {}\nfn g() { f(); }\n",
        ),
        ("src/a.rs", "pub fn f() {}\n"),
    ]);
    assert!(
        !got.iter()
            .any(|(s, _, c)| s == "src/lib.rs#g" && *c == Confidence::Extracted),
        "{got:?}"
    );
}

#[test]
fn test_rsx1_cfg_on_mod_no_edge() {
    let tree = |attr: &'static str| {
        calls(&[
            (
                "src/lib.rs",
                &format!("{attr}mod a;\nfn g() {{ a::f(); }}\n"),
            ),
            ("src/a.rs", "pub fn f() {}\n"),
        ])
    };
    let control = tree("");
    assert!(
        extracted(&control, "src/lib.rs#g", "src/a.rs#f"),
        "{control:?}"
    );
    let got = tree("#[cfg(unix)]\n");
    assert!(!any_extracted_to(&got, "src/a.rs"), "{got:?}");
}

#[test]
fn test_rsx1_lib_path() {
    let manifest = "[package]\nname = \"t\"\nedition = \"2021\"\n\n[lib]\npath = \"core/lib.rs\"\n";
    let got = calls_with(
        manifest,
        &[
            ("core/lib.rs", "mod a;\nuse a::f;\nfn g() { f(); }\n"),
            ("core/a.rs", "pub fn f() {}\n"),
        ],
    );
    assert!(extracted(&got, "core/lib.rs#g", "core/a.rs#f"), "{got:?}");
}

#[test]
fn test_rsx1_ripgrep_manifest_shape() {
    // A root package with `autotests = false`, a multi-line description,
    // `edition.workspace`, and a `[[bin]]` path outside `src/`.
    let manifest = "[package]\nname = \"rg\"\ndescription = \"\"\"\nline one\nline two\n\"\"\"\nbuild = \"build.rs\"\nautotests = false\nedition.workspace = true\n\n[[bin]]\nbench = false\npath = \"crates/core/main.rs\"\nname = \"rg\"\n\n[workspace]\nmembers = [\n  \"crates/x\",\n]\n\n[workspace.package]\nedition = \"2024\"\n\n[target.'cfg(windows)'.dependencies.w]\nversion = \"0.1\"\n";
    let got = calls_with(
        manifest,
        &[
            (
                "crates/core/main.rs",
                "mod flags;\nuse crate::flags::run;\nfn main() { run(); }\n",
            ),
            (
                "crates/core/flags/mod.rs",
                "mod parse;\npub use self::parse::run;\n",
            ),
            ("crates/core/flags/parse.rs", "pub fn run() {}\n"),
        ],
    );
    assert!(
        extracted(
            &got,
            "crates/core/main.rs#main",
            "crates/core/flags/parse.rs#run"
        ),
        "{got:?}"
    );
}

#[test]
fn test_rsx1_manifest_change_triggers_refresh() {
    use crate::refresh::{ensure_fresh_graph, rebuild_graph_only, RefreshOptions};
    let dir = TempDir::new();
    write(&dir.0, "Cargo.toml", MANIFEST);
    write(&dir.0, "src/lib.rs", "mod a;\nuse a::f;\nfn g() { f(); }\n");
    write(&dir.0, "src/a.rs", "pub fn f() {}\n");
    let ctx = dir.0.join("sieve");
    rebuild_graph_only(&dir.0, &ctx).expect("build graph");
    let clean = ensure_fresh_graph(&dir.0, &ctx, &RefreshOptions::default());
    assert!(!clean.refreshed, "{clean:?}");
    // The edition drops to 2015 and the size changes.
    write(&dir.0, "Cargo.toml", "[package]\nname = \"t\"\n");
    let after = ensure_fresh_graph(&dir.0, &ctx, &RefreshOptions::default());
    assert!(after.refreshed, "{after:?}");
}

/// Builds a multi-package tree (every manifest is a given file) and gives
/// its call edges.
fn ws(files: &[(&str, &str)]) -> Calls {
    let dir = TempDir::new();
    for (rel, text) in files {
        write(&dir.0, rel, text);
    }
    calls_in(&dir.0)
}

/// A manifest text for package `name` with an extra `tail`.
fn pkg(name: &str, tail: &str) -> String {
    format!("[package]\nname = \"{name}\"\nedition = \"2021\"\n{tail}")
}

const A_F: &str = "pub fn f() {}\n";
const B_G: &str = "pub fn g() { a::f(); }\n";
const DEP_A: &str = "\n[dependencies]\na = { path = \"../a\" }\n";

#[test]
fn test_rsx2_dep_by_name() {
    let got = ws(&[
        ("a/Cargo.toml", &pkg("a", "")),
        ("a/src/lib.rs", A_F),
        ("b/Cargo.toml", &pkg("b", DEP_A)),
        ("b/src/lib.rs", B_G),
        ("c/Cargo.toml", &pkg("c", DEP_A)),
        ("c/src/lib.rs", "use a::f;\npub fn g() { f(); }\n"),
    ]);
    assert!(
        extracted(&got, "b/src/lib.rs#g", "a/src/lib.rs#f"),
        "{got:?}"
    );
    assert!(
        extracted(&got, "c/src/lib.rs#g", "a/src/lib.rs#f"),
        "{got:?}"
    );
}

#[test]
fn test_rsx2_dash_normalized() {
    let got = ws(&[
        ("my/Cargo.toml", &pkg("my-lib", "")),
        ("my/src/lib.rs", A_F),
        (
            "b/Cargo.toml",
            &pkg("b", "\n[dependencies]\nmy-lib = { path = \"../my\" }\n"),
        ),
        ("b/src/lib.rs", "pub fn g() { my_lib::f(); }\n"),
    ]);
    assert!(
        extracted(&got, "b/src/lib.rs#g", "my/src/lib.rs#f"),
        "{got:?}"
    );
}

#[test]
fn test_rsx2_workspace_true_dep() {
    let root = "[workspace]\nmembers = [\"a\", \"b\", \"c\"]\n\n[workspace.dependencies]\na = { path = \"a\", version = \"0.1\" }\n";
    let got = ws(&[
        ("Cargo.toml", root),
        ("a/Cargo.toml", &pkg("a", "")),
        ("a/src/lib.rs", A_F),
        (
            "b/Cargo.toml",
            &pkg("b", "\n[dependencies]\na = { workspace = true }\n"),
        ),
        ("b/src/lib.rs", B_G),
        (
            "c/Cargo.toml",
            &pkg("c", "\n[dependencies]\na.workspace = true\n"),
        ),
        ("c/src/lib.rs", B_G),
    ]);
    assert!(
        extracted(&got, "b/src/lib.rs#g", "a/src/lib.rs#f"),
        "{got:?}"
    );
    assert!(
        extracted(&got, "c/src/lib.rs#g", "a/src/lib.rs#f"),
        "{got:?}"
    );
}

#[test]
fn test_rsx2_package_rename_no_edge() {
    let got = ws(&[
        ("a/Cargo.toml", &pkg("a", "")),
        ("a/src/lib.rs", A_F),
        (
            "b/Cargo.toml",
            &pkg(
                "b",
                "\n[dependencies]\nx = { path = \"../a\", package = \"a\" }\n",
            ),
        ),
        (
            "b/src/lib.rs",
            "pub fn g() { x::f(); }\npub fn h() { a::f(); }\n",
        ),
    ]);
    assert!(!any_extracted_to(&got, "a/src/lib.rs"), "{got:?}");
}

#[test]
fn test_rsx2_registry_same_name_no_edge() {
    let got = ws(&[
        ("a/Cargo.toml", &pkg("a", "")),
        ("a/src/lib.rs", A_F),
        ("b/Cargo.toml", &pkg("b", "\n[dependencies]\na = \"1.0\"\n")),
        ("b/src/lib.rs", B_G),
    ]);
    assert!(!any_extracted_to(&got, "a/src/lib.rs"), "{got:?}");
}

#[test]
fn test_rsx2_bin_sees_own_lib() {
    let got = ws(&[
        ("t/Cargo.toml", &pkg("t", "")),
        ("t/src/lib.rs", A_F),
        ("t/src/main.rs", "fn main() { t::f(); }\n"),
        ("t/src/bin/x.rs", "use t::f;\nfn main() { f(); }\n"),
        ("t/tests/it.rs", "#[test]\nfn it() { t::f(); }\n"),
    ]);
    assert!(
        extracted(&got, "t/src/main.rs#main", "t/src/lib.rs#f"),
        "{got:?}"
    );
    assert!(
        extracted(&got, "t/src/bin/x.rs#main", "t/src/lib.rs#f"),
        "{got:?}"
    );
    assert!(
        extracted(&got, "t/tests/it.rs#it", "t/src/lib.rs#f"),
        "{got:?}"
    );
}

#[test]
fn test_rsx2_dev_dep_only_in_tests() {
    let got = ws(&[
        ("a/Cargo.toml", &pkg("a", "")),
        ("a/src/lib.rs", A_F),
        (
            "b/Cargo.toml",
            &pkg("b", "\n[dev-dependencies]\na = { path = \"../a\" }\n"),
        ),
        ("b/src/lib.rs", B_G),
        ("b/tests/it.rs", "fn it() { a::f(); }\n"),
        ("b/build.rs", "fn main() { a::f(); }\n"),
    ]);
    assert!(
        extracted(&got, "b/tests/it.rs#it", "a/src/lib.rs#f"),
        "{got:?}"
    );
    assert!(
        !extracted(&got, "b/src/lib.rs#g", "a/src/lib.rs#f"),
        "{got:?}"
    );
    assert!(
        !extracted(&got, "b/build.rs#main", "a/src/lib.rs#f"),
        "{got:?}"
    );
}

#[test]
fn test_rsx2_pub_crate_not_visible() {
    let got = ws(&[
        ("a/Cargo.toml", &pkg("a", "")),
        (
            "a/src/lib.rs",
            "pub(crate) fn f() {}\npub fn open() {}\nmod hid { pub fn deep() {} }\n",
        ),
        ("b/Cargo.toml", &pkg("b", DEP_A)),
        (
            "b/src/lib.rs",
            "pub fn g() { a::f(); }\npub fn h() { a::open(); }\npub fn k() { a::hid::deep(); }\n",
        ),
    ]);
    assert!(
        !extracted(&got, "b/src/lib.rs#g", "a/src/lib.rs#f"),
        "{got:?}"
    );
    assert!(
        !extracted(&got, "b/src/lib.rs#k", "a/src/lib.rs#deep"),
        "{got:?}"
    );
    assert!(
        extracted(&got, "b/src/lib.rs#h", "a/src/lib.rs#open"),
        "{got:?}"
    );
}

#[test]
fn test_rsx2_facade_reexport_chain() {
    let dep = |k: &str| format!("\n[dependencies]\n{k} = {{ path = \"../{k}\" }}\n");
    let got = ws(&[
        ("base/Cargo.toml", &pkg("base", "")),
        ("base/src/lib.rs", A_F),
        ("facade/Cargo.toml", &pkg("facade", &dep("base"))),
        (
            "facade/src/lib.rs",
            "pub use base::f;\npub use base::f as renamed;\n",
        ),
        ("app/Cargo.toml", &pkg("app", &dep("facade"))),
        (
            "app/src/lib.rs",
            "pub fn g() { facade::f(); }\npub fn h() { facade::renamed(); }\n",
        ),
    ]);
    assert!(
        extracted(&got, "app/src/lib.rs#g", "base/src/lib.rs#f"),
        "{got:?}"
    );
    assert!(
        extracted(&got, "app/src/lib.rs#h", "base/src/lib.rs#f"),
        "{got:?}"
    );
}

#[test]
fn test_rsx2_two_packages_same_lib_name_no_edge() {
    let got = ws(&[
        ("a/Cargo.toml", &pkg("a", "")),
        ("a/src/lib.rs", A_F),
        ("old/a/Cargo.toml", &pkg("a", "")),
        ("old/a/src/lib.rs", A_F),
        ("b/Cargo.toml", &pkg("b", DEP_A)),
        ("b/src/lib.rs", B_G),
    ]);
    assert!(!any_extracted_to(&got, "a/src/lib.rs"), "{got:?}");
    assert!(!any_extracted_to(&got, "old/a/src/lib.rs"), "{got:?}");
}

#[test]
fn test_rsx2_edition_2015_extern_crate_path() {
    let got = ws(&[
        ("a/Cargo.toml", &pkg("a", "")),
        ("a/src/lib.rs", A_F),
        (
            "b/Cargo.toml",
            "[package]\nname = \"b\"\n\n[dependencies]\na = { path = \"../a\" }\n",
        ),
        ("b/src/lib.rs", "extern crate a;\npub fn g() { a::f(); }\n"),
    ]);
    assert!(!any_extracted_to(&got, "a/src/lib.rs"), "{got:?}");
}

#[test]
fn test_rsx2_target_dep_after_plain_dep_blocks() {
    let tail = "\n[dependencies]\na = { path = \"../a\" }\n\n[target.'cfg(unix)'.dependencies]\na = { path = \"../other\" }\n";
    let got = ws(&[
        ("a/Cargo.toml", &pkg("a", "")),
        ("a/src/lib.rs", A_F),
        ("b/Cargo.toml", &pkg("b", tail)),
        ("b/src/lib.rs", B_G),
    ]);
    assert!(!any_extracted_to(&got, "a/src/lib.rs"), "{got:?}");
}

#[test]
fn test_rsx2_item_macro_blocks_extern_fallback() {
    let got = ws(&[
        ("a/Cargo.toml", &pkg("a", "")),
        ("a/src/lib.rs", A_F),
        ("b/Cargo.toml", &pkg("b", DEP_A)),
        (
            "b/src/lib.rs",
            "macro_rules! m { () => {} }\nm!();\npub fn g() { a::f(); }\n",
        ),
    ]);
    assert!(!any_extracted_to(&got, "a/src/lib.rs"), "{got:?}");
}

#[test]
fn test_rsx2_workspace_dep_needs_workspace_table() {
    // The root manifest has the entry but no `[workspace]` table.
    let root = "[workspace.dependencies]\na = { path = \"a\" }\n";
    let got = ws(&[
        ("Cargo.toml", root),
        ("a/Cargo.toml", &pkg("a", "")),
        ("a/src/lib.rs", A_F),
        (
            "b/Cargo.toml",
            &pkg("b", "\n[dependencies]\na.workspace = true\n"),
        ),
        ("b/src/lib.rs", B_G),
    ]);
    assert!(!any_extracted_to(&got, "a/src/lib.rs"), "{got:?}");
}

/// The targets of the Extracted edges from `src`.
fn ext_from(got: &Calls, src: &str) -> Vec<String> {
    got.iter()
        .filter(|(s, _, c)| s == src && *c == Confidence::Extracted)
        .map(|(_, t, _)| t.clone())
        .collect()
}

fn one_ext(got: &Calls, src: &str, dst: &str) {
    assert_eq!(ext_from(got, src), vec![dst.to_string()], "{got:?}");
}

fn no_ext(got: &Calls, src: &str) {
    assert!(ext_from(got, src).is_empty(), "{got:?}");
}

#[test]
fn test_rsx3_inherent_assoc() {
    let got = calls(&[(
        "src/lib.rs",
        "struct T;\nimpl T { fn f() {} }\nfn g() { T::f(); }\n",
    )]);
    one_ext(&got, "src/lib.rs#g", "src/lib.rs#f");
}

#[test]
fn test_rsx3_self_assoc() {
    let got = calls(&[(
        "src/lib.rs",
        "struct T;\nimpl T { fn f() {} fn h() { Self::f(); } }\n",
    )]);
    one_ext(&got, "src/lib.rs#h", "src/lib.rs#f");
}

#[test]
fn test_rsx3_impl_in_other_file() {
    let got = calls(&[
        ("src/lib.rs", "mod a;\nstruct T;\nfn g() { T::f(); }\n"),
        ("src/a.rs", "use crate::T;\nimpl T { pub fn f() {} }\n"),
    ]);
    one_ext(&got, "src/lib.rs#g", "src/a.rs#f");
}

#[test]
fn test_rsx3_impl_in_other_module() {
    let got = calls(&[(
        "src/lib.rs",
        "struct T;\nmod m { use super::T; impl T { pub fn f() {} } }\nfn g() { T::f(); }\n",
    )]);
    one_ext(&got, "src/lib.rs#g", "src/lib.rs#f");
}

#[test]
fn test_rsx3_generic_impl() {
    let got = calls(&[(
        "src/lib.rs",
        "struct T<X>(X);\nimpl<X> T<X> { fn f() {} }\nfn g() { T::<u8>::f(); T::f(); }\n",
    )]);
    one_ext(&got, "src/lib.rs#g", "src/lib.rs#f");
}

#[test]
fn test_rsx3_trait_impl_same_name_blocks() {
    let got = calls(&[(
        "src/lib.rs",
        "struct T;\ntrait Tr { fn f(); }\nimpl T { fn f() {} }\nimpl Tr for T { fn f() {} }\nfn g() { T::f(); }\n",
    )]);
    no_ext(&got, "src/lib.rs#g");
}

#[test]
fn test_rsx3_cfg_impl_blocks() {
    let got = calls(&[(
        "src/lib.rs",
        "struct T;\n#[cfg(unix)]\nimpl T { fn f() {} }\nfn g() { T::f(); }\n",
    )]);
    no_ext(&got, "src/lib.rs#g");
    let got = calls(&[(
        "src/lib.rs",
        "struct T;\nimpl T {\n#[cfg(unix)]\nfn f() {} }\nfn g() { T::f(); }\n",
    )]);
    no_ext(&got, "src/lib.rs#g");
    let got = calls(&[(
        "src/lib.rs",
        "struct T;\nimpl T { fn f() {} }\nimpl T { fn f() {} }\nfn g() { T::f(); }\n",
    )]);
    no_ext(&got, "src/lib.rs#g");
}

#[test]
fn test_rsx3_type_alias_blocks() {
    let got = calls(&[(
        "src/lib.rs",
        "struct S;\ntype T = S;\nimpl S { fn f() {} }\nfn g() { T::f(); }\n",
    )]);
    no_ext(&got, "src/lib.rs#g");
    // An impl through an alias adds a second candidate for `S`.
    let got = calls(&[(
        "src/lib.rs",
        "struct S;\ntype A = S;\nimpl S { fn f() {} }\nimpl A { fn f() {} }\nfn g() { S::f(); }\n",
    )]);
    no_ext(&got, "src/lib.rs#g");
}

#[test]
fn test_rsx3_type_param_shadow_blocks() {
    let got = calls(&[(
        "src/lib.rs",
        "struct T;\nimpl T { fn f() {} }\nfn g<T: Tr>() { T::f(); }\n",
    )]);
    no_ext(&got, "src/lib.rs#g");
}

#[test]
fn test_rsx3_enum_variant_not_fn() {
    let got = calls(&[(
        "src/lib.rs",
        "fn f(x: i32) {}\nenum E { f(i32) }\nfn g() { E::f(1); }\n",
    )]);
    no_ext(&got, "src/lib.rs#g");
}

#[test]
fn test_rsx3_self_in_trait_body_no_edge() {
    let got = calls(&[(
        "src/lib.rs",
        "struct T;\nimpl T { fn f() {} }\ntrait Tr { fn h() { Self::f(); } }\nimpl Tr for T { fn k() { Self::f(); } }\n",
    )]);
    no_ext(&got, "src/lib.rs#h");
    no_ext(&got, "src/lib.rs#k");
}

#[test]
fn test_rsx3_nested_impl_self() {
    let got = calls(&[(
        "src/lib.rs",
        "struct A;\nstruct B;\nimpl A { fn f() {} }\nimpl B { fn f() {}\nfn h() { Self::f(); } }\n",
    )]);
    one_ext(&got, "src/lib.rs#h", "src/lib.rs#f~2");
}

#[test]
fn test_rsx3_turbofish_type() {
    let got = calls(&[(
        "src/lib.rs",
        "struct T<X>(X);\nimpl<X> T<X> { fn f<Y>() {} }\nfn g() { T::<u8>::f::<u16>(); }\n",
    )]);
    one_ext(&got, "src/lib.rs#g", "src/lib.rs#f");
}

#[test]
fn test_rsx3_cross_crate_type() {
    let got = ws(&[
        ("a/Cargo.toml", &pkg("a", "")),
        ("a/src/lib.rs", "pub struct T;\nimpl T { pub fn f() {} }\n"),
        ("b/Cargo.toml", &pkg("b", DEP_A)),
        ("b/src/lib.rs", "pub fn g() { a::T::f(); }\n"),
        ("c/Cargo.toml", &pkg("c", DEP_A)),
        ("c/src/lib.rs", "use a::T;\npub fn g() { T::f(); }\n"),
    ]);
    one_ext(&got, "b/src/lib.rs#g", "a/src/lib.rs#f");
    one_ext(&got, "c/src/lib.rs#g", "a/src/lib.rs#f");
}

#[test]
fn test_rsx3_private_fn_across_crate_no_edge() {
    let got = ws(&[
        ("a/Cargo.toml", &pkg("a", "")),
        ("a/src/lib.rs", "pub struct T;\nimpl T { fn f() {} }\n"),
        ("b/Cargo.toml", &pkg("b", DEP_A)),
        ("b/src/lib.rs", "pub fn g() { a::T::f(); }\n"),
    ]);
    no_ext(&got, "b/src/lib.rs#g");
}

#[test]
fn test_rsx3_private_fn_outside_impl_module_no_edge() {
    let got = calls(&[(
        "src/lib.rs",
        "struct T;\nmod m { impl super::T { fn f() {} } }\nfn g() { T::f(); }\n",
    )]);
    no_ext(&got, "src/lib.rs#g");
}

#[test]
fn test_rsx3_blanket_impl_with_private_inherent() {
    let got = calls(&[(
        "src/lib.rs",
        "struct T;\ntrait Tr { fn f(); }\nimpl<X> Tr for X { fn f() {} }\nmod m { impl super::T { fn f() {} } }\nfn g() { T::f(); }\n",
    )]);
    no_ext(&got, "src/lib.rs#g");
}

#[test]
fn test_rsx3_turbofish_with_specialized_impls_no_edge() {
    let got = calls(&[(
        "src/lib.rs",
        "struct T<X>(X);\nimpl T<u8> { fn f() {} }\nimpl T<u16> { fn h() {} }\nfn g() { T::<u8>::f(); }\n",
    )]);
    no_ext(&got, "src/lib.rs#g");
}

#[test]
fn test_rsx3_deref_target_ignored() {
    let got = calls(&[(
        "src/lib.rs",
        "struct W;\nstruct T;\nimpl std::ops::Deref for W { type Target = T; }\nimpl T { fn f() {} }\nfn g() { W::f(); }\n",
    )]);
    no_ext(&got, "src/lib.rs#g");
}

#[test]
fn test_rsx3_u8_u16_pair_blocks() {
    let got = calls(&[(
        "src/lib.rs",
        "struct T<X>(X);\nimpl T<u8> { fn f() {} }\nimpl T<u16> { fn f() {} }\nfn g() { T::f(); }\n",
    )]);
    no_ext(&got, "src/lib.rs#g");
}

#[test]
fn test_rsx3_impl_generic_shadows_type_name() {
    let got = calls(&[(
        "src/lib.rs",
        "struct T;\nimpl T { fn f() {} }\nstruct S<A>(A);\nimpl<T> S<T> { fn h() { T::f(); } }\n",
    )]);
    no_ext(&got, "src/lib.rs#h");
}
