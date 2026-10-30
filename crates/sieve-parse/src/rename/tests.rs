//! Unit tests for the rename plan, one group per design rule.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A temp repo with its own context dir, removed on drop.
pub(super) struct Repo {
    pub(super) dir: PathBuf,
}

impl Repo {
    pub(super) fn new(files: &[(&str, &str)]) -> Repo {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("sieve-rn2-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("repo")).expect("mkdir repo");
        let repo = Repo { dir };
        for (path, body) in files {
            repo.write(path, body.as_bytes());
        }
        repo
    }

    pub(super) fn root(&self) -> PathBuf {
        self.dir.join("repo")
    }

    pub(super) fn ctx(&self) -> PathBuf {
        self.dir.join("ctx")
    }

    pub(super) fn write(&self, path: &str, body: &[u8]) {
        let full = self.root().join(path);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).expect("mkdir parent");
        }
        fs::write(full, body).expect("write file");
    }

    pub(super) fn plan(&self, symbol: &str, new: &str) -> Plan {
        plan_rename(&self.root(), &self.ctx(), symbol, new, None).expect("plan")
    }

    fn git_init(&self) {
        let status = Command::new("git")
            .arg("init")
            .arg("-q")
            .current_dir(self.root())
            .status();
        assert!(status.is_ok_and(|s| s.success()), "git init");
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn edits_of<'p>(plan: &'p Plan, path: &str) -> Vec<&'p Edit> {
    plan.files
        .iter()
        .filter(|f| f.path == path)
        .flat_map(|f| f.edits.iter())
        .collect()
}

fn count_edits(plan: &Plan) -> usize {
    plan.files.iter().map(|f| f.edits.len()).sum()
}

fn assert_ready(plan: &Plan) {
    assert_eq!(plan.status, Status::Ready, "refused: {:?}", plan.refused);
}

fn assert_refused(plan: &Plan, reason: Reason) {
    assert_eq!(plan.status, Status::Refused);
    assert!(plan.files.is_empty());
    assert!(
        plan.refused.iter().any(|r| r.reason == reason),
        "want {reason:?}, got {:?}",
        plan.refused
    );
}

pub(super) const DEF: &str = "export function f(x: number): number {\n  return x + 1;\n}\n";
pub(super) const USE: &str = "import { f } from \"./def\";\nexport const y = f(1);\n";

// ---- the pass path ----

#[test]
fn test_rn2_plan_covers_declaration_import_and_call() {
    let repo = Repo::new(&[("def.ts", DEF), ("use.ts", USE)]);
    let plan = repo.plan("f", "g");
    assert_ready(&plan);
    assert_eq!(plan.version, 1);
    assert_eq!(plan.plan_id.len(), 8);
    assert_eq!(plan.target.id, "def.ts#f");
    assert_eq!(count_edits(&plan), 3);
    let sites: Vec<SiteClass> = edits_of(&plan, "use.ts").iter().map(|e| e.site).collect();
    assert_eq!(sites, vec![SiteClass::Import, SiteClass::Call]);
    let def = &plan.files[0];
    assert_eq!(def.path, "def.ts");
    assert_eq!(def.sha256, hex_sha256(DEF.as_bytes()));
    assert_eq!((def.edits[0].start, def.edits[0].end), (16, 17));
    assert_eq!(def.edits[0].site, SiteClass::Declaration);
    // A dry run never writes a source file.
    assert_eq!(
        fs::read_to_string(repo.root().join("def.ts")).expect("read"),
        DEF
    );
}

#[test]
fn test_rn2_plan_id_changes_with_the_new_name() {
    let repo = Repo::new(&[("def.ts", DEF)]);
    let a = repo.plan("f", "g");
    let b = repo.plan("f", "h");
    assert_ne!(a.plan_id, b.plan_id);
    assert_eq!(a.plan_id, repo.plan("f", "g").plan_id);
}

#[test]
fn test_rn2_symbol_forms_and_ambiguity() {
    let repo = Repo::new(&[("a/def.ts", DEF), ("b/def.ts", DEF)]);
    let both = plan_rename(&repo.root(), &repo.ctx(), "f", "g", None);
    assert!(matches!(both, Err(RenameError::Symbol(ref t)) if t.contains("a/def.ts#f")));
    let one = plan_rename(&repo.root(), &repo.ctx(), "f", "g", Some("a"));
    assert!(one.is_ok());
    assert_eq!(repo.plan("b/def.ts#f", "g").target.path, "b/def.ts");
    assert_eq!(repo.plan("b/def.ts:1", "g").target.path, "b/def.ts");
    let none = plan_rename(&repo.root(), &repo.ctx(), "nope", "g", None);
    assert!(matches!(none, Err(RenameError::Symbol(_))));
}

// ---- Target rule ----

#[test]
fn test_rn2_s1_merged_declaration_refuses() {
    let repo = Repo::new(&[(
        "def.ts",
        "export function f(): number {\n  return 1;\n}\nexport namespace f {\n  export const x = 2;\n}\n",
    )]);
    assert_refused(&repo.plan("f", "g"), Reason::TargetKind);
}

#[test]
fn test_rn2_s1_default_export_refuses() {
    let repo = Repo::new(&[(
        "def.ts",
        "export default function f(): number {\n  return 1;\n}\n",
    )]);
    assert_refused(&repo.plan("f", "g"), Reason::TargetKind);
}

#[test]
fn test_rn2_s1_export_default_statement_refuses() {
    let repo = Repo::new(&[(
        "def.ts",
        "function f(): number {\n  return 1;\n}\nexport default f;\n",
    )]);
    assert_refused(&repo.plan("f", "g"), Reason::TargetKind);
}

#[test]
fn test_rn2_s1_method_and_class_targets_refuse() {
    let repo = Repo::new(&[(
        "def.ts",
        "export class K {\n  m(): number {\n    return 1;\n  }\n}\n",
    )]);
    assert_refused(&repo.plan("m", "n"), Reason::TargetKind);
    assert_refused(&repo.plan("K", "L"), Reason::TargetKind);
}

#[test]
fn test_rn2_s1_script_file_refuses_as_not_module() {
    let repo = Repo::new(&[("def.ts", "function f(): number {\n  return 1;\n}\n")]);
    assert_refused(&repo.plan("f", "g"), Reason::NotModule);
}

#[test]
fn test_rn2_s1_arrow_const_target_passes() {
    let repo = Repo::new(&[
        ("def.ts", "export const f = (x: number) => x + 1;\n"),
        ("use.ts", USE),
    ]);
    let plan = repo.plan("f", "g");
    assert_ready(&plan);
    assert_eq!(count_edits(&plan), 3);
}

// ---- Stale-file rule ----

#[test]
fn test_rn2_s2_changed_file_is_stale() {
    let repo = Repo::new(&[("def.ts", DEF)]);
    fs::create_dir_all(repo.ctx()).expect("ctx");
    let kept = rebuild_keeping(&repo.root(), &repo.ctx()).expect("build");
    repo.write(
        "def.ts",
        b"export function f(x: number): number {\n  return x + 2;\n}\n",
    );
    let plan =
        plan_from_build(&repo.root(), &repo.ctx(), &kept, "f", "g", None, true).expect("plan");
    assert_refused(&plan, Reason::StaleIndex);
}

// ---- Clean-file rule ----

#[test]
fn test_rn2_s3_utf16_importer_is_unproven() {
    let repo = Repo::new(&[("def.ts", DEF)]);
    let mut bytes = vec![0xff, 0xfe];
    for unit in USE.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    repo.write("use.ts", &bytes);
    assert_refused(&repo.plan("f", "g"), Reason::Unproven);
}

#[test]
fn test_rn2_s3_syntax_error_in_importer_refuses() {
    let repo = Repo::new(&[
        ("def.ts", DEF),
        (
            "use.ts",
            "import { f } from \"./def\";\nexport const y = f(1;\n",
        ),
    ]);
    assert_refused(&repo.plan("f", "g"), Reason::ParseError);
}

#[test]
fn test_rn2_s3_with_and_eval_refuse() {
    let with = Repo::new(&[
        ("def.ts", DEF),
        (
            "use.js",
            "import { f } from \"./def\";\nwith (Math) { f(1); }\n",
        ),
    ]);
    assert_refused(&with.plan("f", "g"), Reason::Unproven);
    let eval = Repo::new(&[
        ("def.ts", DEF),
        (
            "use.ts",
            "import { f } from \"./def\";\neval(\"1\");\nf(1);\n",
        ),
    ]);
    assert_refused(&eval.plan("f", "g"), Reason::Unproven);
}

#[test]
fn test_rn2_s3_file_without_a_hit_may_be_broken() {
    let repo = Repo::new(&[("def.ts", DEF), ("other.ts", "export const z = (;\n")]);
    assert_ready(&repo.plan("f", "g"));
}

// ---- Nested-name rule ----

#[test]
fn test_rn2_s4_nested_declaration_in_importer_is_unproven() {
    let repo = Repo::new(&[
        ("def.ts", DEF),
        (
            "use.ts",
            "import { f } from \"./def\";\nexport function h(f: number) {\n  return f;\n}\nexport const y = f(1);\n",
        ),
    ]);
    assert_refused(&repo.plan("f", "g"), Reason::Unproven);
}

#[test]
fn test_rn2_s4_nested_declaration_in_defining_module_is_unproven() {
    let repo = Repo::new(&[(
        "def.ts",
        "export function f(): number {\n  return 1;\n}\nexport function h() {\n  const f = 2;\n  return f;\n}\n",
    )]);
    assert_refused(&repo.plan("f", "g"), Reason::Unproven);
}

#[test]
fn test_rn2_s4_nested_name_in_unrelated_file_does_not_refuse() {
    let repo = Repo::new(&[
        ("def.ts", DEF),
        (
            "other.ts",
            "export function h(f: number) {\n  return f;\n}\n",
        ),
    ]);
    let plan = repo.plan("f", "g");
    assert_ready(&plan);
    assert_eq!(count_edits(&plan), 1);
}

#[test]
fn test_rn2_s4_nested_new_name_in_importer_is_unproven() {
    let repo = Repo::new(&[
        ("def.ts", DEF),
        (
            "use.ts",
            "import { f } from \"./def\";\nexport function h(g: number) {\n  return g + f(1);\n}\n",
        ),
    ]);
    assert_refused(&repo.plan("f", "g"), Reason::Unproven);
}

// ---- Import rule ----

#[test]
fn test_rn2_s5_alias_import_edits_only_the_specifier() {
    let repo = Repo::new(&[
        ("def.ts", DEF),
        (
            "use.ts",
            "import { f as hello } from \"./def\";\nexport const y = hello(1);\n",
        ),
    ]);
    let plan = repo.plan("f", "g");
    assert_ready(&plan);
    let use_edits = edits_of(&plan, "use.ts");
    assert_eq!(use_edits.len(), 1);
    assert_eq!(use_edits[0].site, SiteClass::Import);
    assert_eq!(use_edits[0].line, 1);
}

#[test]
fn test_rn2_s5_decoy_module_import_is_other() {
    let repo = Repo::new(&[
        ("def.ts", DEF),
        (
            "decoy.ts",
            "export function f(): number {\n  return 0;\n}\n",
        ),
        (
            "use.ts",
            "import { f } from \"./decoy\";\nexport const y = f();\n",
        ),
    ]);
    let plan = repo.plan("def.ts#f", "g");
    assert_ready(&plan);
    assert_eq!(count_edits(&plan), 1);
}

#[test]
fn test_rn2_s5_unresolved_relative_import_is_unproven() {
    let repo = Repo::new(&[
        ("def.ts", DEF),
        (
            "use.ts",
            "import { f } from \"./missing\";\nexport const y = f();\n",
        ),
    ]);
    assert_refused(&repo.plan("f", "g"), Reason::Unproven);
}

#[test]
fn test_rn2_s5_package_import_is_other() {
    let repo = Repo::new(&[
        ("def.ts", DEF),
        (
            "use.ts",
            "import { f } from \"lodash\";\nexport const y = f();\n",
        ),
    ]);
    assert_ready(&repo.plan("def.ts#f", "g"));
}

#[test]
fn test_rn2_s5_bundler_alias_is_unproven() {
    let repo = Repo::new(&[
        ("def.ts", DEF),
        (
            "vite.config.js",
            "export default { resolve: { alias: { lib: \"./def\" } } };\n",
        ),
        (
            "use.ts",
            "import { f } from \"lib\";\nexport const y = f();\n",
        ),
    ]);
    assert_refused(&repo.plan("def.ts#f", "g"), Reason::Unproven);
}

#[test]
fn test_rn2_s5_tsconfig_paths_is_unproven() {
    let lib_use = "import { f } from \"lib\";\nexport const y = f();\n";
    let repo = Repo::new(&[
        ("def.ts", DEF),
        (
            "tsconfig.json",
            "{\n  // comment\n  \"compilerOptions\": { \"paths\": { \"lib\": [\"./def\"], }, },\n}\n",
        ),
        ("use.ts", lib_use),
    ]);
    assert_refused(&repo.plan("def.ts#f", "g"), Reason::Unproven);
    let plain = Repo::new(&[
        ("def.ts", DEF),
        (
            "tsconfig.json",
            "{ \"compilerOptions\": { \"strict\": true } }\n",
        ),
        ("use.ts", lib_use),
    ]);
    assert_ready(&plain.plan("def.ts#f", "g"));
    let extends = Repo::new(&[
        ("def.ts", DEF),
        (
            "tsconfig.json",
            "{ \"extends\": \"@tsconfig/node/tsconfig.json\" }\n",
        ),
        ("use.ts", lib_use),
    ]);
    assert_refused(&extends.plan("def.ts#f", "g"), Reason::Unproven);
}

#[test]
fn test_rn2_s5_package_self_import_has_its_own_reason() {
    let repo = Repo::new(&[
        ("def.ts", DEF),
        ("package.json", "{ \"name\": \"mylib\" }\n"),
        (
            "use.ts",
            "import { f } from \"mylib\";\nexport const y = f();\n",
        ),
    ]);
    assert_refused(&repo.plan("def.ts#f", "g"), Reason::PackageSelfImport);
}

#[test]
fn test_rn2_s5_alias_prefix_specifier_is_unproven() {
    for spec in ["@/def", "~/def", "#def", "$lib/def", "virtual:def"] {
        let body = format!("import {{ f }} from \"{spec}\";\nexport const y = f();\n");
        let repo = Repo::new(&[("def.ts", DEF), ("use.ts", &body)]);
        assert_refused(&repo.plan("def.ts#f", "g"), Reason::Unproven);
    }
}

#[test]
fn test_rn2_s5_barrel_import_is_an_edit_and_the_reexport_too() {
    let repo = Repo::new(&[
        ("def.ts", DEF),
        ("barrel.ts", "export { f } from \"./def\";\n"),
        (
            "use.ts",
            "import { f } from \"./barrel\";\nexport const y = f();\n",
        ),
    ]);
    let plan = repo.plan("f", "g");
    assert_ready(&plan);
    assert_eq!(edits_of(&plan, "barrel.ts")[0].site, SiteClass::Reexport);
    assert_eq!(count_edits(&plan), 4);
}

#[test]
fn test_rn2_s5_default_import_of_chain_module_is_unproven() {
    let repo = Repo::new(&[
        ("def.ts", DEF),
        (
            "use.ts",
            "import f from \"./def\";\nexport const y = f(1);\n",
        ),
    ]);
    assert_refused(&repo.plan("def.ts#f", "g"), Reason::Unproven);
}

// ---- Graph-scan rule ----

#[test]
fn test_rn2_s6_shadowed_call_does_not_refuse() {
    let repo = Repo::new(&[(
        "def.ts",
        "export function f(): number {\n  return 1;\n}\nconst cfg = { f: () => 2 };\nexport function h() {\n  return cfg.f();\n}\n",
    )]);
    let plan = repo.plan("f", "g");
    assert_ready(&plan);
    assert_eq!(count_edits(&plan), 1);
}

#[test]
fn test_rn2_s6_inferred_link_from_another_language_is_a_report() {
    let repo = Repo::new(&[
        ("def.ts", DEF),
        ("tool.py", "def run():\n    return f(1)\n"),
    ]);
    let plan = repo.plan("f", "g");
    assert_ready(&plan);
    assert!(plan.report.iter().any(|r| r.path == "tool.py"));
}

// ---- Reach rule ----

#[test]
fn test_rn2_s7_namespace_import_refuses() {
    let repo = Repo::new(&[
        ("def.ts", DEF),
        (
            "use.ts",
            "import * as d from \"./def\";\nexport const y = d.f(1);\n",
        ),
    ]);
    assert_refused(&repo.plan("f", "g"), Reason::UnsupportedSite);
}

#[test]
fn test_rn2_s7_star_export_and_export_namespace_refuse() {
    for line in [
        "export * from \"./def\";\n",
        "export * as d from \"./def\";\n",
    ] {
        let repo = Repo::new(&[("def.ts", DEF), ("barrel.ts", line)]);
        assert_refused(&repo.plan("f", "g"), Reason::UnsupportedSite);
    }
}

#[test]
fn test_rn2_s7_import_meta_glob_refuses() {
    let repo = Repo::new(&[
        ("src/def.ts", DEF),
        (
            "src/glob.ts",
            "export const m = import.meta.glob(\"./*.ts\");\n",
        ),
    ]);
    assert_refused(&repo.plan("f", "g"), Reason::DynamicImport);
    let far = Repo::new(&[
        ("src/def.ts", DEF),
        (
            "other/glob.ts",
            "export const m = import.meta.glob(\"./*.ts\");\n",
        ),
    ]);
    assert_ready(&far.plan("f", "g"));
    let opaque = Repo::new(&[
        ("src/def.ts", DEF),
        (
            "other/glob.ts",
            "const p = \"./x\";\nexport const m = import.meta.glob(p);\n",
        ),
        PROP_HIT,
    ]);
    assert_refused(&opaque.plan("f", "g"), Reason::DynamicImport);
}

/// A file with an old-name property hit `o.f` (the reach rule).
const PROP_HIT: (&str, &str) = ("hit/prop.ts", "export const k = (o: any) => o.f;\n");

/// A repo with `src/def.ts` and one more file; plans the rename of `f`.
/// The repo holds no old-name property, key or string.
fn glob_plan_clean(path: &str, body: &str, extra: &[(&str, &str)]) -> Plan {
    let mut files = vec![("src/def.ts", DEF), (path, body)];
    files.extend_from_slice(extra);
    Repo::new(&files).plan("f", "g")
}

/// Like `glob_plan_clean`, with a property hit `o.f` in another file.
fn glob_plan(path: &str, body: &str, extra: &[(&str, &str)]) -> Plan {
    let mut files = vec![("src/def.ts", DEF), (path, body), PROP_HIT];
    files.extend_from_slice(extra);
    Repo::new(&files).plan("f", "g")
}

#[test]
fn test_rn2_s7_glob_matching_forms() {
    let refusing = [
        "import.meta.glob(\"./**/*.ts\")",
        "import.meta.glob(\"./{def,x}.ts\")",
        "import.meta.glob(\"./de?.ts\")",
        "import.meta.glob(\"./def.ts?raw\")",
        "import.meta.glob(\"/src/*.ts\")",
        "import.meta.webpackContext(\"./\", { recursive: true })",
        "import.meta.webpackContext(\"./\")",
        "import.meta.webpackContext(d)",
        "import.meta.webpackContext(\"./other\", { recursive: r })",
        "import.meta.webpackContext(\"./other\", { regExp: re })",
        "import.meta.glob(\"./[a-e]ef.ts\")",
        "import.meta.glob([\"./nope/*.ts\", \"./def.ts\"])",
        "import.meta.glob([\"./*.ts\", \"!./def.ts\"])",
        "import.meta.glob(`./def.ts`)",
        "import.meta.globEager(\"./def.ts\")",
        "import.meta.glob(\"./*.ts\", { eager: true })",
        "import.meta.glob(\"./def.ts\", { base: \"./x\" })",
        "import.meta.glob(\"@/mods/*.ts\")",
        "import.meta.glob(`./${x}.ts`)",
        "import.meta.glob([p])",
    ];
    for call in refusing {
        let body = format!("export const m = {call};\n");
        assert_refused(&glob_plan("src/m.ts", &body, &[]), Reason::DynamicImport);
    }
    let passing = [
        "import.meta.glob(\"./modules/*.ts\")",
        "import.meta.glob(\"./sub/**/*.ts\")",
        "import.meta.webpackContext(\"./other\")",
        "import.meta.webpackContext(\"./other\", { recursive: false, regExp: /x/ })",
        "import.meta.glob(\"./[!d]ef.ts\")",
        "import.meta.glob([\"./a.ts\", \"./b/*.ts\"], { eager: true })",
        "import.meta.glob(\"!./def.ts\")",
        "import.meta.globEager(\"./other/*.ts\")",
    ];
    for call in passing {
        let body = format!("export const m = {call};\n");
        assert_ready(&glob_plan("src/m.ts", &body, &[]));
    }
}

#[test]
fn test_rn2_s7_glob_root_pattern_always_refuses() {
    // The Vite root may differ from the repo root, so a `/` pattern is opaque.
    let body = "export const m = import.meta.glob(\"/lib/*.ts\");\n";
    assert_refused(&glob_plan("app/m.ts", body, &[]), Reason::DynamicImport);
    let rooted = [("vite.config.ts", "export default { root: \"app\" };\n")];
    assert_refused(&glob_plan("app/m.ts", body, &rooted), Reason::DynamicImport);
    let plain = [("vite.config.ts", "export default {};\n")];
    assert_refused(&glob_plan("app/m.ts", body, &plain), Reason::DynamicImport);
}

#[test]
fn test_rn2_s7_markup_file_with_a_glob_refuses() {
    for (name, body) in [
        (
            "src/a.vue",
            "<script>const m = import.meta.glob('./x/*.ts');</script>\n",
        ),
        ("src/a.svelte", "<script>require.context('./x');</script>\n"),
        (
            "src/a.html",
            "<script>import.meta.webpackContext('./x');</script>\n",
        ),
    ] {
        let plan = glob_plan(name, body, &[]);
        assert_refused(&plan, Reason::DynamicImport);
        assert!(plan.refused.iter().any(|r| r.path == name));
    }
    let clean = "<script>const a = 1;</script>\n";
    assert_ready(&glob_plan("src/a.vue", clean, &[]));
}

#[test]
fn test_rn2_s7_glob_through_a_symlink_refuses() {
    let repo = Repo::new(&[
        ("lib/def.ts", DEF),
        (
            "src/m.ts",
            "export const m = import.meta.glob(\"./link/*.ts\");\n",
        ),
    ]);
    std::os::unix::fs::symlink("../lib", repo.root().join("src/link")).expect("symlink");
    assert_refused(&repo.plan("lib/def.ts#f", "g"), Reason::DynamicImport);
}

#[test]
fn test_rn2_s7_require_context_checks_the_directory() {
    let under = "export const m = require.context(\"./\", true, /\\.ts$/);\n";
    assert_refused(&glob_plan("src/m.ts", under, &[]), Reason::DynamicImport);
    let away = "export const m = require.context(\"./other\", true, /\\.ts$/);\n";
    assert_ready(&glob_plan("src/m.ts", away, &[]));
    let shallow = "export const m = require.context(\"../src\", false, /\\.ts$/);\n";
    assert_refused(&glob_plan("lib/m.ts", shallow, &[]), Reason::DynamicImport);
    let deep_off = "export const m = require.context(\".\", false, /\\.ts$/);\n";
    assert_refused(&glob_plan("src/m.ts", deep_off, &[]), Reason::DynamicImport);
    let up = "export const m = require.context(\"..\", false, /\\.ts$/);\n";
    assert_ready(&glob_plan("src/m.ts", up, &[]));
    let opaque = "export const m = require.context(d, true, /x/);\n";
    assert_refused(&glob_plan("other/m.ts", opaque, &[]), Reason::DynamicImport);
    let rx = "export const m = require.context(\"./other\", true, re);\n";
    assert_refused(&glob_plan("src/m.ts", rx, &[]), Reason::DynamicImport);
}

const DYN_IMPORT: (&str, &str) = ("dyn.ts", "export const load = (x: string) => import(x);\n");

fn s7d_plan(extra: &[(&str, &str)]) -> Plan {
    let mut files = vec![("def.ts", DEF), ("use.ts", USE), DYN_IMPORT];
    files.extend_from_slice(extra);
    Repo::new(&files).plan("f", "g")
}

#[test]
fn test_rn2_s7d_non_literal_import_without_an_old_name_form_is_ready() {
    let plan = s7d_plan(&[]);
    assert_ready(&plan);
    assert_eq!(edits_of(&plan, "def.ts").len(), 1);
    assert_eq!(edits_of(&plan, "use.ts").len(), 2);
}

#[test]
fn test_rn2_s7d_non_literal_import_refuses_with_an_old_name_form() {
    let forms = [
        "export const a = (m: any) => m.f;\n",
        "export const a = (m: any) => m?.f;\n",
        "export const a = (m: any) => m[\"f\"];\n",
        "export const a = (m: any) => m['f'];\n",
        "export const a = async (x: string) => { const { f } = await import(x); return f; };\n",
        "export const a = (m: any) => { const { f: y } = m; return y; };\n",
        "export const a = (m: any) => { const { f = 1 } = m; return f; };\n",
        "export const a = ({ f }: any) => f;\n",
        "export const a = ({ q: { f } }: any) => f;\n",
        "export const a = `f`;\n",
    ];
    for body in forms {
        let plan = s7d_plan(&[("form.ts", body)]);
        assert_refused(&plan, Reason::DynamicImport);
        let r = plan
            .refused
            .iter()
            .find(|r| r.reason == Reason::DynamicImport)
            .expect("refusal");
        assert_eq!(r.path, "dyn.ts");
        assert!(r.text.contains("dyn.ts:1"), "{}", r.text);
        assert!(r.text.contains("form.ts:1"), "{}", r.text);
    }
}

#[test]
fn test_rn2_s7d_a_word_inside_a_longer_string_is_not_a_form() {
    let plan = s7d_plan(&[("text.ts", "export const a = \"f is a letter\";\n")]);
    assert_ready(&plan);
}

#[test]
fn test_rn2_s7d_alias_glob_without_a_form_is_ready() {
    let body = "export const m = import.meta.glob(\"#src/*.ts\");\n";
    assert_ready(&glob_plan_clean("src/m.ts", body, &[]));
    assert_refused(&glob_plan("src/m.ts", body, &[]), Reason::DynamicImport);
}

#[test]
fn test_rn2_s7d_root_glob_refuses_on_a_this_property_in_an_unrelated_class() {
    let body = "export const m = import.meta.glob(\"/src/*.ts\");\n";
    let class = (
        "other/c.ts",
        "export class C {\n  g() {\n    return this.f;\n  }\n}\n",
    );
    assert_ready(&glob_plan_clean("app/m.ts", body, &[]));
    let plan = glob_plan_clean("app/m.ts", body, &[class]);
    assert_refused(&plan, Reason::DynamicImport);
    let sup = (
        "other/s.ts",
        "export class S extends B {\n  g = super.f;\n}\n",
    );
    assert_refused(
        &glob_plan_clean("app/m.ts", body, &[sup]),
        Reason::DynamicImport,
    );
}

#[test]
fn test_rn2_s7d_other_untraceable_globs_wait_for_a_form() {
    for call in [
        "import.meta.glob(\"./x?.ts\")",
        "import.meta.glob(p)",
        "import.meta.glob(\"./*.ts\", { base: \"/o\" })",
        "require.context(\"./\", true, /\\.ts$/)",
        "import.meta.webpackContext(d)",
    ] {
        let body = format!("export const m = {call};\n");
        assert_ready(&glob_plan_clean("src/m.ts", &body, &[]));
        assert_refused(&glob_plan("src/m.ts", &body, &[]), Reason::DynamicImport);
    }
}

#[test]
fn test_rn2_s7d_markup_word_is_a_form() {
    let plan = s7d_plan(&[("src/a.vue", "<template>{{ f }}</template>\n")]);
    assert_refused(&plan, Reason::DynamicImport);
    assert!(plan.refused.iter().any(|r| r.text.contains("src/a.vue:1")));
    let other = s7d_plan(&[("src/a.vue", "<template>{{ h }}</template>\n")]);
    assert_ready(&other);
}

#[test]
fn test_rn2_s7d_markup_glob_waits_for_a_form() {
    let body = "<script>const m = import.meta.glob('./x/*.ts');</script>\n";
    assert_ready(&glob_plan_clean("src/a.vue", body, &[]));
}

#[test]
fn test_rn2_s7d_literal_glob_of_the_defining_file_still_refuses() {
    let body = "export const m = import.meta.glob(\"./def.ts\");\n";
    assert_refused(
        &glob_plan_clean("src/m.ts", body, &[]),
        Reason::DynamicImport,
    );
    let ctx = "export const m = require.context(\"./\", true, /\\.ts$/);\n";
    assert_ready(&glob_plan_clean("src/m.ts", ctx, &[]));
}

#[test]
fn test_rn2_s7d_refusal_names_the_real_line_of_the_access() {
    let body = "// one\n// two\n// three\n// four\n// five\n// six\n// seven\n// eight\nexport const load = (x: string) => import(x);\n";
    let plan = s7d_plan(&[("form.ts", "export const a = (m: any) => m.f;\n")]);
    assert_ready(&s7d_plan_with(body, &[]));
    let plan9 = s7d_plan_with(body, &[("form.ts", "export const a = (m: any) => m.f;\n")]);
    assert_refused(&plan9, Reason::DynamicImport);
    let r = plan9
        .refused
        .iter()
        .find(|r| r.reason == Reason::DynamicImport)
        .expect("refusal");
    assert_eq!(r.line, 9);
    assert!(r.text.contains("dyn.ts:9"), "{}", r.text);
    assert_refused(&plan, Reason::DynamicImport);
}

fn s7d_plan_with(dyn_body: &str, extra: &[(&str, &str)]) -> Plan {
    let mut files = vec![("def.ts", DEF), ("use.ts", USE), ("dyn.ts", dyn_body)];
    files.extend_from_slice(extra);
    Repo::new(&files).plan("f", "g")
}

#[test]
fn test_rn2_s7d_string_in_a_data_file_is_a_form() {
    let body = "export const m = import.meta.glob(\"#src/*.ts\");\n";
    let json = ("routes.json", "{ \"handler\": \"f\" }\n");
    assert_ready(&glob_plan_clean("src/router.ts", body, &[]));
    let plan = glob_plan_clean("src/router.ts", body, &[json]);
    assert_refused(&plan, Reason::DynamicImport);
    assert!(plan
        .refused
        .iter()
        .any(|r| r.text.contains("routes.json:1")));
    for (name, text) in [
        ("a.yaml", "handler: f\n"),
        ("a.yml", "handler: f # note\n"),
        ("a.toml", "handler = f\n"),
        ("a.toml", "handler = 'f'\n"),
        ("a.md", "see 'f' here\n"),
    ] {
        let plan = glob_plan_clean("src/router.ts", body, &[(name, text)]);
        assert_refused(&plan, Reason::DynamicImport);
    }
    // A word in prose is not a whole-string form.
    let prose = ("notes.md", "the f function is nice\n");
    assert_ready(&glob_plan_clean("src/router.ts", body, &[prose]));
}

#[test]
fn test_rn2_s7d_property_hit_in_js_mjs_and_tsx_files() {
    for (name, text) in [
        ("p.js", "export const a = (m) => m.f;\n"),
        ("p.mjs", "export const a = (m) => m?.f;\n"),
        ("p.tsx", "export const a = (m: any) => <div>{m.f}</div>;\n"),
        (
            "p.js",
            "export const a = (m) => { const { f } = m; return f; };\n",
        ),
        ("p.mjs", "export const a = (m) => m[\"f\"];\n"),
    ] {
        let plan = s7d_plan(&[(name, text)]);
        assert_refused(&plan, Reason::DynamicImport);
    }
}

#[test]
fn test_rn2_s7d_object_literal_shorthand_counts_as_a_form() {
    // The scan counts a shorthand `{ f }` in an object literal too.
    let plan = s7d_plan(&[(
        "call.ts",
        "const f = 1;\nexport const a = (g: (o: object) => void) => g({ f });\n",
    )]);
    assert_refused(&plan, Reason::DynamicImport);
}

#[test]
fn test_rn2_s7_dynamic_import_literal_refuses() {
    let repo = Repo::new(&[
        ("def.ts", DEF),
        ("use.ts", "export const m = () => import(\"./def\");\n"),
    ]);
    assert_refused(&repo.plan("f", "g"), Reason::DynamicImport);
    let req = Repo::new(&[
        ("def.ts", DEF),
        ("use.cjs", "const m = require(\"./def\");\n"),
    ]);
    assert_refused(&req.plan("f", "g"), Reason::DynamicImport);
    let unrelated = Repo::new(&[
        ("def.ts", DEF),
        ("use.ts", "export const m = () => import(\"./other\");\n"),
    ]);
    assert_ready(&unrelated.plan("f", "g"));
}

#[test]
fn test_rn2_s7_non_literal_import_refuses_only_with_an_old_name_form() {
    let hit = Repo::new(&[
        ("def.ts", DEF),
        (
            "use.ts",
            "import { f } from \"./def\";\nexport const m = (p: string) => import(p);\nf(1);\nexport const k = (o: any) => o.f;\n",
        ),
    ]);
    assert_refused(&hit.plan("f", "g"), Reason::DynamicImport);
    let no_form = Repo::new(&[
        ("def.ts", DEF),
        (
            "use.ts",
            "import { f } from \"./def\";\nexport const m = (p: string) => import(p);\nf(1);\n",
        ),
    ]);
    assert_ready(&no_form.plan("f", "g"));
    let clean = Repo::new(&[
        ("def.ts", DEF),
        ("use.ts", "export const m = (p: string) => import(p);\n"),
    ]);
    assert_ready(&clean.plan("f", "g"));
}

#[test]
fn test_rn2_s7_commonjs_exports_in_a_chain_file_refuse() {
    let repo = Repo::new(&[(
        "def.ts",
        "export function f(): number {\n  return 1;\n}\nmodule.exports = {};\n",
    )]);
    assert_refused(&repo.plan("f", "g"), Reason::UnsupportedSite);
}

// ---- Property rule ----

#[test]
fn test_rn2_s8_property_of_a_local_is_other() {
    let repo = Repo::new(&[(
        "def.ts",
        "export function f(): number {\n  return 1;\n}\nconst o = { f: 1 };\nexport const v = o.f;\n",
    )]);
    let plan = repo.plan("f", "g");
    assert_ready(&plan);
    assert_eq!(count_edits(&plan), 1);
}

#[test]
fn test_rn2_s8_property_of_a_relative_namespace_is_unproven() {
    let repo = Repo::new(&[
        ("def.ts", DEF),
        ("util.ts", "export const u = 1;\n"),
        (
            "use.ts",
            "import * as ns from \"./util\";\nexport const v = ns.f;\n",
        ),
    ]);
    assert_refused(&repo.plan("def.ts#f", "g"), Reason::Unproven);
}

#[test]
fn test_rn2_s8_globalthis_key_stays_and_the_value_is_an_edit() {
    let repo = Repo::new(&[
        ("def.ts", DEF),
        (
            "gt.ts",
            "import { f } from \"./def\";\n(globalThis as any).f = f;\n",
        ),
    ]);
    let plan = repo.plan("f", "g");
    assert_ready(&plan);
    let gt = edits_of(&plan, "gt.ts");
    assert_eq!(gt.len(), 2);
    // The key `.f` is at column 21 of line 2 and stays. The value is an edit.
    assert!(gt.iter().all(|e| e.line == 1 || e.col > 21));
}

#[test]
fn test_rn2_s8_object_key_and_class_member_are_other() {
    let repo = Repo::new(&[(
        "def.ts",
        "export function f(): number {\n  return 1;\n}\nexport class K {\n  f() {\n    return this.f;\n  }\n}\nexport const o = { f: 1 };\n",
    )]);
    let plan = repo.plan("def.ts#f", "g");
    assert_ready(&plan);
    assert_eq!(count_edits(&plan), 1);
}

// ---- New-name rule ----

#[test]
fn test_rn2_s9_bad_names_refuse() {
    let repo = Repo::new(&[("def.ts", DEF)]);
    for bad in [
        "f",
        "1x",
        "a-b",
        "class",
        "default",
        "arguments",
        "await",
        "static",
        "",
    ] {
        assert_refused(&repo.plan("f", bad), Reason::Collision);
    }
}

#[test]
fn test_rn2_s9_defining_module_binding_refuses() {
    let repo = Repo::new(&[(
        "def.ts",
        "export function f(): number {\n  return 1;\n}\nexport const g = 2;\n",
    )]);
    assert_refused(&repo.plan("f", "g"), Reason::Collision);
}

#[test]
fn test_rn2_s9_importer_with_a_module_binding_of_the_new_name_is_capture() {
    let repo = Repo::new(&[
        ("def.ts", DEF),
        (
            "use.ts",
            "import { f } from \"./def\";\nconst g = 1;\nexport const y = f(1) + g;\n",
        ),
    ]);
    assert_refused(&repo.plan("f", "g"), Reason::Capture);
}

#[test]
fn test_rn2_s9_reverse_capture_refuses() {
    let clash = Repo::new(&[
        ("def.ts", DEF),
        (
            "use.ts",
            "import { f } from \"./def\";\nexport const y = f(1) + Math.PI;\n",
        ),
    ]);
    assert_refused(&clash.plan("f", "Math"), Reason::Collision);
    let free = Repo::new(&[("def.ts", DEF), ("use.ts", USE)]);
    assert_ready(&free.plan("f", "Math"));
}

#[test]
fn test_rn2_s9_alias_importer_does_not_gain_the_new_name() {
    let repo = Repo::new(&[
        ("def.ts", DEF),
        (
            "use.ts",
            "import { f as h } from \"./def\";\nexport const y = h(1) + Math.PI;\n",
        ),
    ]);
    assert_ready(&repo.plan("f", "Math"));
}

#[test]
fn test_rn2_s9_jsx_needs_an_uppercase_name() {
    let repo = Repo::new(&[
        ("w.tsx", "export function W() {\n  return <span />;\n}\n"),
        (
            "app.tsx",
            "import { W } from \"./w\";\nexport const a = <W />;\n",
        ),
    ]);
    assert_refused(&repo.plan("W", "w2"), Reason::Collision);
    let plan = repo.plan("W", "W2");
    assert_ready(&plan);
    assert!(edits_of(&plan, "app.tsx")
        .iter()
        .any(|e| e.site == SiteClass::Jsx));
}

#[test]
fn test_rn2_s9_barrel_that_exports_the_new_name_refuses() {
    let repo = Repo::new(&[
        ("def.ts", DEF),
        (
            "other.ts",
            "export function g(): number {\n  return 1;\n}\n",
        ),
        (
            "barrel.ts",
            "export { f } from \"./def\";\nexport { g } from \"./other\";\n",
        ),
    ]);
    assert_refused(&repo.plan("def.ts#f", "g"), Reason::Collision);
}

// ---- Export-form rule ----

#[test]
fn test_rn2_s10_shorthand_property_refuses() {
    let repo = Repo::new(&[
        ("def.ts", DEF),
        (
            "use.ts",
            "import { f } from \"./def\";\nexport const t = { f };\n",
        ),
    ]);
    assert_refused(&repo.plan("f", "g"), Reason::UnsupportedSite);
}

#[test]
fn test_rn2_s10_foreign_export_clause_refuses() {
    let repo = Repo::new(&[
        ("def.ts", DEF),
        ("use.ts", "import { f } from \"./def\";\nexport { f };\n"),
    ]);
    assert_refused(&repo.plan("f", "g"), Reason::UnsupportedSite);
}

#[test]
fn test_rn2_s10_overloads_refuse() {
    let repo = Repo::new(&[(
        "def.ts",
        "export function f(x: number): number;\nexport function f(x: string): string;\nexport function f(x: any): any {\n  return x;\n}\n",
    )]);
    assert_refused(&repo.plan("f", "g"), Reason::TargetKind);
}

#[test]
fn test_rn2_s10_declare_statement_and_dts_refuse() {
    let declare = Repo::new(&[
        ("def.ts", DEF),
        (
            "globals.ts",
            "export {};\ndeclare global {\n  function f(): void;\n}\n",
        ),
    ]);
    assert_refused(&declare.plan("f", "g"), Reason::UnsupportedSite);
    let dts = Repo::new(&[
        ("def.ts", DEF),
        ("types.d.ts", "export declare function f(): void;\n"),
    ]);
    assert_refused(&dts.plan("def.ts#f", "g"), Reason::UnsupportedSite);
}

#[test]
fn test_rn2_s10_declare_module_of_a_chain_file_refuses() {
    let repo = Repo::new(&[
        ("def.ts", DEF),
        (
            "aug.ts",
            "export {};\ndeclare module \"./def\" {\n  export const extra: number;\n}\n",
        ),
    ]);
    assert_refused(&repo.plan("f", "g"), Reason::UnsupportedSite);
}

// ---- JSDoc rule ----

#[test]
fn test_rn2_s11_jsdoc_link_is_edit() {
    let repo = Repo::new(&[(
        "def.ts",
        "/** See {@link f}. */\nexport function f(): number {\n  return 1;\n}\n",
    )]);
    let plan = repo.plan("f", "g");
    assert_ready(&plan);
    let sites: Vec<SiteClass> = plan.files[0].edits.iter().map(|e| e.site).collect();
    assert_eq!(sites, vec![SiteClass::Jsdoc, SiteClass::Declaration]);
}

#[test]
fn test_rn2_s11_jsdoc_type_in_js_is_edit_and_in_ts_is_unproven() {
    let js = Repo::new(&[
        ("def.js", "export function f() {\n  return 1;\n}\n"),
        (
            "use.js",
            "import { f } from \"./def\";\n/** @type {typeof f} */\nexport const v = f;\n",
        ),
    ]);
    let plan = js.plan("f", "g");
    assert_ready(&plan);
    assert!(edits_of(&plan, "use.js")
        .iter()
        .any(|e| e.site == SiteClass::Jsdoc));
    let ts = Repo::new(&[
        ("def.ts", DEF),
        (
            "use.ts",
            "import { f } from \"./def\";\n/** @param {f} x */\nexport const v = f;\n",
        ),
    ]);
    assert_refused(&ts.plan("f", "g"), Reason::Unproven);
}

#[test]
fn test_rn2_s11_whole_string_hit_refuses() {
    let repo = Repo::new(&[("def.ts", DEF), ("use.ts", "export const k = \"f\";\n")]);
    assert_refused(&repo.plan("f", "g"), Reason::Unproven);
}

#[test]
fn test_rn2_s11_comment_and_longer_string_are_reports() {
    let repo = Repo::new(&[
        ("def.ts", DEF),
        (
            "use.ts",
            "// f is called below\nexport const k = \"say f now\";\n",
        ),
        ("README.md", "Call `f()` to start.\n"),
    ]);
    let plan = repo.plan("f", "g");
    assert_ready(&plan);
    let classes: Vec<(&str, &str)> = plan
        .report
        .iter()
        .map(|r| (r.path.as_str(), r.class.as_str()))
        .collect();
    assert!(classes.contains(&("use.ts", "comment")));
    assert!(classes.contains(&("use.ts", "string")));
    assert!(classes.contains(&("README.md", "doc")));
    assert_eq!(count_edits(&plan), 1);
}

#[test]
fn test_rn2_s11_container_file_hit_is_unproven() {
    let repo = Repo::new(&[
        ("def.ts", DEF),
        (
            "App.vue",
            "<script>\nimport { f } from \"./def\";\nf(1);\n</script>\n",
        ),
    ]);
    assert_refused(&repo.plan("f", "g"), Reason::Unproven);
}

// ---- Script-file rule ----

#[test]
fn test_rn2_s12_script_file_with_a_hit_is_not_a_module() {
    let repo = Repo::new(&[("def.ts", DEF), ("script.js", "var y = f(1);\n")]);
    assert_refused(&repo.plan("f", "g"), Reason::NotModule);
}

// ---- Symlink rule ----

#[test]
fn test_rn2_s13_symlinked_file_or_ancestor_is_found() {
    let repo = Repo::new(&[("real/def.ts", DEF)]);
    let root = repo.root();
    std::os::unix::fs::symlink("real", root.join("alias")).expect("symlink dir");
    std::os::unix::fs::symlink("def.ts", root.join("real/linked.ts")).expect("symlink file");
    assert!(!path_has_symlink(&root, "real/def.ts"));
    assert!(path_has_symlink(&root, "real/linked.ts"));
    assert!(path_has_symlink(&root, "alias/def.ts"));
    assert!(path_has_symlink(&root, "real/missing.ts"));
}

/// A directory beside the repo, with the files, for links that leave it.
fn outside_dir(repo: &Repo, name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = repo.dir.join(name);
    fs::create_dir_all(&dir).expect("mkdir outside");
    for (file, body) in files {
        fs::write(dir.join(file), body).expect("write outside");
    }
    dir
}

fn link(target: impl AsRef<std::path::Path>, at: PathBuf) {
    std::os::unix::fs::symlink(target, at).expect("symlink");
}

#[test]
fn test_rn2_s13b_link_to_a_scanned_dir_or_file_does_not_refuse() {
    let repo = Repo::new(&[("lib/def.ts", DEF), ("lib/use.ts", USE)]);
    link("lib", repo.root().join("alias"));
    link("use.ts", repo.root().join("lib/use_link.ts"));
    let plan = repo.plan("lib/def.ts#f", "g");
    assert_ready(&plan);
    assert_eq!(edits_of(&plan, "lib/def.ts").len(), 1);
    assert_eq!(edits_of(&plan, "lib/use.ts").len(), 2);
    assert_eq!(count_edits(&plan), 3);
    // The same with a git work tree, where the walk lists the files.
    repo.git_init();
    assert_ready(&repo.plan("lib/def.ts#f", "g"));
}

#[test]
fn test_rn2_s13b_link_outside_the_repo_to_source_refuses() {
    // `src/shared` points outside the repo. Its importer is never scanned.
    let repo = Repo::new(&[("src/def.ts", DEF), ("src/use.ts", USE)]);
    let outside = outside_dir(
        &repo,
        "shared",
        &[(
            "far.ts",
            "import { f } from \"../def\";\nexport const z = f(2);\n",
        )],
    );
    link(&outside, repo.root().join("src/shared"));
    let plan = repo.plan("src/def.ts#f", "g");
    assert_refused(&plan, Reason::UnsupportedSite);
    assert!(plan.refused.iter().any(|r| r.path == "src/shared"));
    // A link to one outside file refuses too.
    let file = Repo::new(&[("def.ts", DEF)]);
    let far = outside_dir(&file, "far", &[("one.ts", USE)]);
    link(far.join("one.ts"), file.root().join("one_link.ts"));
    assert_refused(&file.plan("f", "g"), Reason::UnsupportedSite);
}

#[test]
fn test_rn2_s13b_broken_link_refuses() {
    let repo = Repo::new(&[("def.ts", DEF)]);
    link("nowhere.ts", repo.root().join("gone.ts"));
    let plan = repo.plan("f", "g");
    assert_refused(&plan, Reason::UnsupportedSite);
    assert!(plan.refused.iter().any(|r| r.path == "gone.ts"));
}

#[test]
fn test_rn2_s13b_link_into_a_skipped_dir_with_source_refuses() {
    let repo = Repo::new(&[
        ("def.ts", DEF),
        ("node_modules/pkg/a.ts", USE),
        (".hidden/b.ts", USE),
    ]);
    link("node_modules/pkg", repo.root().join("vendor"));
    let plan = repo.plan("f", "g");
    assert_refused(&plan, Reason::UnsupportedSite);
    assert!(plan.refused.iter().any(|r| r.path == "vendor"));
    let hidden = Repo::new(&[("def.ts", DEF), (".hidden/b.ts", USE)]);
    link(".hidden", hidden.root().join("shown"));
    assert_refused(&hidden.plan("f", "g"), Reason::UnsupportedSite);
}

#[test]
fn test_rn2_s13b_import_through_an_in_repo_link_is_unproven() {
    let repo = Repo::new(&[
        ("lib/def.ts", DEF),
        (
            "src/use.ts",
            "import { f } from \"./alias/def\";\nexport const y = f(1);\n",
        ),
    ]);
    link("../lib", repo.root().join("src/alias"));
    let plan = repo.plan("lib/def.ts#f", "g");
    assert_refused(&plan, Reason::Unproven);
    let row = plan.refused.iter().find(|r| r.path == "src/use.ts");
    assert!(
        row.is_some_and(|r| r.text.contains("src/alias")),
        "{:?}",
        plan.refused
    );
    // A walk that crosses the link with `..` counts too.
    let up = Repo::new(&[
        ("lib/def.ts", DEF),
        (
            "src/use.ts",
            "import { f } from \"./alias/../def\";\nexport const y = f(1);\n",
        ),
    ]);
    link("../lib", up.root().join("src/alias"));
    assert_refused(&up.plan("lib/def.ts#f", "g"), Reason::Unproven);
}

#[test]
fn test_rn2_s13b_star_export_and_dynamic_import_through_a_link_are_unproven() {
    let cases = [
        "export * from \"./alias/def\";\n",
        "export * as ns from \"./alias/def\";\n",
        "export const m = () => import(\"./alias/def\");\n",
    ];
    for body in cases {
        let repo = Repo::new(&[("lib/def.ts", DEF), ("src/other.ts", body)]);
        link("../lib", repo.root().join("src/alias"));
        let plan = repo.plan("lib/def.ts#f", "g");
        assert_refused(&plan, Reason::Unproven);
        let row = plan.refused.iter().find(|r| r.path == "src/other.ts");
        assert!(
            row.is_some_and(|r| r.reason == Reason::Unproven && r.text.contains("src/alias")),
            "{body}: {:?}",
            plan.refused
        );
    }
}

#[test]
fn test_rn2_s13b_link_to_a_gitignored_source_refuses() {
    let repo = Repo::new(&[
        ("def.ts", DEF),
        (".gitignore", "ignored/\n"),
        ("ignored/x.ts", USE),
    ]);
    repo.git_init();
    link("ignored", repo.root().join("shown"));
    let plan = repo.plan("f", "g");
    assert_refused(&plan, Reason::UnsupportedSite);
    assert!(plan.refused.iter().any(|r| r.path == "shown"));
    // A link to one ignored file refuses too.
    link("ignored/x.ts", repo.root().join("one.ts"));
    assert_refused(&repo.plan("f", "g"), Reason::UnsupportedSite);
}

#[test]
fn test_rn2_s13b_preserve_symlinks_turns_the_exemption_off() {
    let plain = [("lib/def.ts", DEF), ("lib/use.ts", USE)];
    let repo = Repo::new(&plain);
    link("lib", repo.root().join("alias"));
    assert_ready(&repo.plan("lib/def.ts#f", "g"));
    let configs = [
        (
            "tsconfig.json",
            "{ \"compilerOptions\": { \"preserveSymlinks\": true } }\n",
        ),
        ("tsconfig.json", "{ \"extends\": \"./base.json\" }\n"),
        (
            "webpack.config.js",
            "module.exports = { resolve: { symlinks:false } };\n",
        ),
    ];
    for (name, body) in configs {
        let repo = Repo::new(&plain);
        repo.write(name, body.as_bytes());
        repo.write(
            "base.json",
            b"{ \"compilerOptions\": { \"preserveSymlinks\": true } }\n",
        );
        link("lib", repo.root().join("alias"));
        let plan = repo.plan("lib/def.ts#f", "g");
        assert_refused(&plan, Reason::UnsupportedSite);
        assert!(plan.refused.iter().any(|r| r.path == "alias"), "{name}");
    }
}

#[test]
fn test_rn2_s13b_tracked_file_link_with_an_importer_is_not_a_missed_edit() {
    let repo = Repo::new(&[
        ("src/bar.ts", DEF),
        (
            "src/use.ts",
            "import { f } from \"./foo\";\nexport const y = f(1);\n",
        ),
    ]);
    link("bar.ts", repo.root().join("src/foo.ts"));
    repo.git_init();
    let added = Command::new("git")
        .args(["add", "-A"])
        .current_dir(repo.root())
        .status();
    assert!(added.is_ok_and(|s| s.success()), "git add");
    let plan = repo.plan("src/bar.ts#f", "g");
    let exact = plan.status == Status::Ready && !edits_of(&plan, "src/use.ts").is_empty();
    assert!(plan.status == Status::Refused || exact, "{plan:?}");
}

#[test]
fn test_rn2_s13b_importer_inside_a_link_target_resolves_by_real_path() {
    let repo = Repo::new(&[
        ("x.ts", DEF),
        (
            "lib/use.ts",
            "import { f } from \"../x\";\nexport const y = f(1);\n",
        ),
    ]);
    link("../lib", repo.root().join("src_alias"));
    let plan = repo.plan("x.ts#f", "g");
    assert_ready(&plan);
    assert_eq!(edits_of(&plan, "x.ts").len(), 1);
    assert_eq!(edits_of(&plan, "lib/use.ts").len(), 2);
    assert_eq!(count_edits(&plan), 3);
}

#[test]
fn test_rn2_s13b_two_links_to_one_target_edit_each_file_once() {
    let repo = Repo::new(&[("lib/def.ts", DEF), ("lib/use.ts", USE)]);
    link("lib", repo.root().join("one"));
    link("lib", repo.root().join("two"));
    let plan = repo.plan("lib/def.ts#f", "g");
    assert_ready(&plan);
    assert_eq!(plan.files.len(), 2);
    assert_eq!(count_edits(&plan), 3);
}

#[test]
fn test_rn2_s13b_link_chain_ending_outside_refuses() {
    let repo = Repo::new(&[("def.ts", DEF)]);
    let far = outside_dir(&repo, "far", &[("one.ts", USE)]);
    link(far.join("one.ts"), repo.root().join("hop1"));
    link("hop1", repo.root().join("hop2"));
    let plan = repo.plan("f", "g");
    assert_refused(&plan, Reason::UnsupportedSite);
    assert!(plan.refused.iter().any(|r| r.path == "hop2"));
}

#[test]
fn test_rn2_s13b_link_cycle_refuses() {
    let repo = Repo::new(&[("def.ts", DEF)]);
    link("loop2", repo.root().join("loop1"));
    link("loop1", repo.root().join("loop2"));
    assert_refused(&repo.plan("f", "g"), Reason::UnsupportedSite);
}

#[test]
fn test_rn2_s13b_link_to_a_parent_of_the_repo_refuses() {
    let repo = Repo::new(&[("def.ts", DEF)]);
    link("..", repo.root().join("up"));
    assert_refused(&repo.plan("f", "g"), Reason::UnsupportedSite);
}

#[test]
fn test_rn2_s13b_link_to_a_dir_without_source_does_not_refuse() {
    let repo = Repo::new(&[("def.ts", DEF), ("use.ts", USE)]);
    let outside = outside_dir(&repo, "data", &[("a.json", "{}\n")]);
    link(&outside, repo.root().join("data"));
    assert_ready(&repo.plan("f", "g"));
}

#[test]
fn test_rn2_s13_symlink_to_a_data_file_is_ignored() {
    let repo = Repo::new(&[("def.ts", DEF), ("data.txt", "x\n")]);
    std::os::unix::fs::symlink("data.txt", repo.root().join("copy.txt")).expect("symlink");
    assert_ready(&repo.plan("f", "g"));
}

// ---- review fixes ----

#[test]
fn test_rn2_s5_webpack_and_jest_aliases_are_unproven() {
    let lib_use = "import { f } from \"lib\";\nexport const y = f();\n";
    let cases: [(&str, &str); 6] = [
        (
            "webpack.config.js",
            "module.exports = { resolve: { alias: { lib: \"./def\" } } };\n",
        ),
        (
            "jest.config.js",
            "module.exports = { moduleNameMapper: { \"^lib$\": \"<rootDir>/def\" } };\n",
        ),
        ("babel.config.js", "plugins: [\"module-resolver\"]\n"),
        (
            "esbuild.config.mjs",
            "export default { alias: { lib: \"./def\" } };\n",
        ),
        (
            "package.json",
            "{ \"jest\": { \"moduleNameMapper\": { \"^lib$\": \"<rootDir>/def\" } } }\n",
        ),
        (
            "package.json",
            "{ \"imports\": { \"#x\": \"./def.js\" } }\n",
        ),
    ];
    for (name, body) in cases {
        let repo = Repo::new(&[("def.ts", DEF), (name, body), ("use.ts", lib_use)]);
        assert_refused(&repo.plan("def.ts#f", "g"), Reason::Unproven);
    }
    let browser = Repo::new(&[
        ("def.ts", DEF),
        (
            "package.json",
            "{ \"browser\": { \"lib\": \"./def.js\" } }\n",
        ),
        ("use.ts", lib_use),
    ]);
    assert_refused(&browser.plan("def.ts#f", "g"), Reason::Unproven);
    let plain = Repo::new(&[
        ("def.ts", DEF),
        ("eslint.config.js", "export default [];\n"),
        ("package.json", "{ \"version\": \"1.0.0\" }\n"),
        ("use.ts", lib_use),
    ]);
    assert_ready(&plain.plan("def.ts#f", "g"));
}

#[test]
fn test_rn2_s9_nested_new_name_cannot_capture_a_jsdoc_link() {
    let repo = Repo::new(&[
        ("def.ts", DEF),
        (
            "use.js",
            "import { f } from \"./def\";\n/** See {@link f}. */\nexport function h(g) {\n  return g;\n}\n",
        ),
    ]);
    assert_refused(&repo.plan("f", "g"), Reason::Unproven);
    let free = Repo::new(&[
        ("def.ts", DEF),
        (
            "use.js",
            "import { f } from \"./def\";\n/** See {@link f}. */\nexport function h(x) {\n  return x;\n}\n",
        ),
    ]);
    assert_ready(&free.plan("f", "g"));
}

#[test]
fn test_rn2_s9_identifier_check_rejects_numeric_marks() {
    let repo = Repo::new(&[("def.ts", DEF)]);
    assert_refused(&repo.plan("f", "foo\u{b2}"), Reason::Collision);
    assert!(is_identifier("foo9"));
    assert!(is_identifier("$_a1"));
    assert!(!is_identifier("9a"));
}

#[test]
fn test_rn2_s7_typeof_import_type_refuses() {
    let repo = Repo::new(&[
        ("def.ts", DEF),
        ("use.ts", "export type T = typeof import(\"./def\");\n"),
    ]);
    assert_refused(&repo.plan("f", "g"), Reason::DynamicImport);
    let member = Repo::new(&[
        ("def.ts", DEF),
        ("use.ts", "export let x: import(\"./def\").Foo;\n"),
    ]);
    assert_refused(&member.plan("f", "g"), Reason::DynamicImport);
}

#[test]
fn test_rn2_s3_utf16_files_refuse_only_with_a_hit() {
    let utf16 = |src: &str, big: bool| {
        let mut bytes = if big {
            vec![0xfe, 0xff]
        } else {
            vec![0xff, 0xfe]
        };
        for unit in src.encode_utf16() {
            let pair = if big {
                unit.to_be_bytes()
            } else {
                unit.to_le_bytes()
            };
            bytes.extend_from_slice(&pair);
        }
        bytes
    };
    let big_hit = Repo::new(&[("def.ts", DEF)]);
    big_hit.write("use.ts", &utf16(USE, true));
    assert_refused(&big_hit.plan("f", "g"), Reason::ParseError);
    let big_clean = Repo::new(&[("def.ts", DEF)]);
    big_clean.write("use.ts", &utf16("export const z = 1;\n", true));
    assert_ready(&big_clean.plan("f", "g"));
    let little_clean = Repo::new(&[("def.ts", DEF)]);
    little_clean.write("use.ts", &utf16("export const z = 1;\n", false));
    assert_ready(&little_clean.plan("f", "g"));
}

#[test]
fn test_rn2_only_dir_build_refuses_once_for_unindexed_hits() {
    let repo = Repo::new(&[
        ("a/def.ts", DEF),
        ("b/one.ts", USE),
        ("b/two.ts", USE),
        ("b/ok.ts", "export const z = 1;\n"),
    ]);
    let stamp = crate::extractor_stamp();
    let print = sieve_core::fingerprint::fingerprint_path(&repo.ctx(), &stamp);
    fs::create_dir_all(print.parent().expect("parent")).expect("mkdir");
    let fp = sieve_core::fingerprint::Fingerprint {
        version: 1,
        extractor: stamp,
        files: std::collections::BTreeMap::new(),
        only_dirs: Some(vec!["a".to_string()]),
        walked_at_ms: None,
    };
    sieve_core::fingerprint::write_fingerprint(&print, &fp).expect("fingerprint");
    let plan = repo.plan("f", "g");
    assert_refused(&plan, Reason::StaleIndex);
    let rows: Vec<_> = plan
        .refused
        .iter()
        .filter(|r| r.reason == Reason::StaleIndex)
        .collect();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].text.contains("--only-dir"));
}

// ---- second review fixes ----

#[test]
fn test_rn2_s5_more_alias_sources_are_unproven() {
    let lib_use = "import { f } from \"lib\";\nexport const y = f();\n";
    let cases: [(&str, &str); 11] = [
        ("deno.json", "{ \"imports\": { \"lib\": \"./def.ts\" } }\n"),
        ("sub/deno.jsonc", "{ \"importMap\": \"./m.json\" }\n"),
        (
            "import_map.json",
            "{ \"imports\": { \"lib\": \"./def.ts\" } }\n",
        ),
        (
            "jest.config.js",
            "module.exports = { moduleDirectories: [\"src\"] };\n",
        ),
        (
            "jest.config.js",
            "module.exports = { roots: [\"<rootDir>/src\"] };\n",
        ),
        (
            "webpack.config.js",
            "module.exports = { resolve: { modules: [\"src\"] } };\n",
        ),
        (".env", "NODE_PATH=src\n"),
        ("pkg/.env.local", "NODE_PATH=src\n"),
        (
            "package.json",
            "{ \"scripts\": { \"start\": \"NODE_PATH=src node x\" } }\n",
        ),
        (
            "pkg/.babelrc.json",
            "{ \"plugins\": [[\"module-resolver\"]] }\n",
        ),
        (
            ".swcrc",
            "{ \"jsc\": { \"baseUrl\": \".\", \"paths\": {} }, \"alias\": 1 }\n",
        ),
    ];
    for (name, body) in cases {
        let repo = Repo::new(&[("def.ts", DEF), (name, body), ("use.ts", lib_use)]);
        assert_refused(&repo.plan("def.ts#f", "g"), Reason::Unproven);
    }
    let storybook = Repo::new(&[
        ("def.ts", DEF),
        (
            ".storybook/main.js",
            "export default { webpackFinal: (c) => { c.resolve.alias = {}; return c; } };\n",
        ),
        ("use.ts", lib_use),
    ]);
    assert_refused(&storybook.plan("def.ts#f", "g"), Reason::Unproven);
    let plain = Repo::new(&[
        ("def.ts", DEF),
        (".env", "API_URL=http://x\n"),
        ("deno.json", "{ \"tasks\": {} }\n"),
        ("use.ts", lib_use),
    ]);
    assert_ready(&plain.plan("def.ts#f", "g"));
}

#[test]
fn test_rn2_s11_bare_jsdoc_type_in_js_is_unproven() {
    let repo = Repo::new(&[
        ("def.js", "export function f() {\n  return 1;\n}\n"),
        (
            "use.js",
            "import { f } from \"./def\";\n/** @type {f} */\nexport const v = f;\n",
        ),
    ]);
    assert_refused(&repo.plan("f", "g"), Reason::Unproven);
}

#[test]
fn test_rn2_s13b_markup_target_outside_refuses() {
    for name in [
        "page.vue",
        "page.svelte",
        "page.astro",
        "doc.mdx",
        "index.html",
    ] {
        let repo = Repo::new(&[("def.ts", DEF)]);
        let far = outside_dir(&repo, "far", &[(name, "x\n")]);
        link(far.join(name), repo.root().join("alias_link"));
        assert_refused(&repo.plan("f", "g"), Reason::UnsupportedSite);
    }
}

#[test]
fn test_rn2_s7_utf16be_file_without_a_hit_is_still_probed() {
    let src = "export const m = () => import(\"./def\");\n";
    let mut bytes = vec![0xfe, 0xff];
    for unit in src.encode_utf16() {
        bytes.extend_from_slice(&unit.to_be_bytes());
    }
    let repo = Repo::new(&[("def.ts", DEF)]);
    repo.write("use.ts", &bytes);
    assert_refused(&repo.plan("f", "g"), Reason::DynamicImport);
}

// ---- the journal ----

#[test]
fn test_rn2_leftover_journal_refuses() {
    let repo = Repo::new(&[("def.ts", DEF)]);
    let journal = repo.ctx().join(JOURNAL_PATH);
    fs::create_dir_all(journal.parent().expect("parent")).expect("mkdir");
    fs::write(&journal, "{}").expect("journal");
    assert_refused(&repo.plan("f", "g"), Reason::JournalPresent);
}
