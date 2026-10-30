//! RN2 gate: the rename plan against the frozen TypeScript oracle rows of
//! `tests/fixtures/rename1.expected/oracle.jsonl` (design section E).
//!
//! Each case runs on its own temp copy of `tests/fixtures/rename1`. The copy
//! drops every hazard file that the case does not need, so one unrelated
//! construct cannot decide the result. Oracle rows of a dropped file leave the
//! comparison too. A case with a `refuse` expectation runs on the whole
//! fixture. Then the result is checked against the case's `expect` field.
//!
//! Each case is Exact (a ready plan equals the oracle), Wrong (a ready plan
//! differs, or a plan is ready where the case expects a refusal) or Refused.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sieve_parse::rename::{plan_rename, Plan, SiteClass, Status};

/// Files that make the reach, clean-file or symlink rule refuse every case: a glob, a symlink and a
/// UTF-16 file.
const HAZARDS: [&str; 3] = ["src/glob.ts", "src/linked.ts", "src/utf16.ts"];

/// Importers of `src/def.ts#greet` that the rename plan cannot plan: namespace and
/// star barrels, a shorthand property, a nested shadow and a TS file with a
/// JSDoc type.
const GREET_EXTRA: [&str; 8] = [
    "src/nsuser.ts",
    "src/ns_user.ts",
    "src/barrel_ns.ts",
    "src/barrel_star.ts",
    "src/star_user.ts",
    "src/short.ts",
    "src/shadow.ts",
    "src/named.ts",
];

/// The workspace root, read at run time (never `env!`).
fn workspace_root() -> PathBuf {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by cargo");
    Path::new(&manifest)
        .parent()
        .and_then(Path::parent)
        .expect("manifest dir has two parents")
        .to_path_buf()
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let dir =
            std::env::temp_dir().join(format!("sieve-rn2-oracle-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("mkdir temp");
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Copies a tree and keeps each symlink as a symlink.
fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("mkdir copy");
    for entry in fs::read_dir(from).expect("read dir") {
        let entry = entry.expect("dir entry");
        let target = to.join(entry.file_name());
        let kind = entry.file_type().expect("file type");
        if kind.is_symlink() {
            let link = fs::read_link(entry.path()).expect("read link");
            std::os::unix::fs::symlink(link, &target).expect("symlink");
        } else if kind.is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy file");
        }
    }
}

/// One oracle row: path, byte span and tag.
type Row = (String, usize, usize, String);

fn read_jsonl(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .expect("read jsonl")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("json row"))
        .collect()
}

/// The node id (or `None`) that a case renames. `None` means the case renames
/// something that is not a top-level function node.
fn symbol_of(case: &str) -> Option<&'static str> {
    Some(match case {
        "def_decl"
        | "def_jsdoc_link"
        | "named_import"
        | "alias_import_name"
        | "shadow_import"
        | "short_import"
        | "short_prop"
        | "globalthis_rhs"
        | "ns_member"
        | "barrel_named_reexport"
        | "barrel_named_user"
        | "star_user"
        | "ns_barrel_member"
        | "js_type_site"
        | "js_link_site"
        | "utf16_importer"
        | "symlink_path"
        | "utf8_importer"
        | "utf8_importer_second"
        | "strings_comments" => "src/def.ts#greet",
        "decoy_decl" | "decoy_user" => "src/decoy.ts#greet",
        "jsx_decl" | "jsx_use" => "src/widget.tsx#Widget",
        "default_decl" | "default_import" | "default_keyword" => "src/dflt.ts#dflt",
        "local_export_decl" | "local_export_clause" | "local_export_importer" => "src/loc.ts#loc",
        "merged_decl" => "src/merged.ts#mrg",
        _ => return None,
    })
}

/// The files to drop from the copy for one case.
fn removals(case: &str, expect: &str, symbol: Option<&str>) -> Vec<&'static str> {
    if expect == "other" {
        return Vec::new();
    }
    if expect.starts_with("refuse") {
        // Keep only the files whose rule the case tests, so the refusal
        // comes from that rule and no other hazard adds a reason.
        let own: &[&str] = match case {
            "utf16_importer" => &["src/utf16.ts"],
            "named_import" => &["src/named.ts"],
            "symlink_path" => &["src/linked.ts"],
            "short_import" | "short_prop" => &["src/short.ts"],
            "ns_member" => &["src/nsuser.ts"],
            "star_user" => &["src/star_user.ts", "src/barrel_star.ts"],
            "ns_barrel_member" => &["src/ns_user.ts", "src/barrel_ns.ts"],
            "shadow_import" => &["src/shadow.ts"],
            _ => &[],
        };
        return HAZARDS
            .iter()
            .chain(GREET_EXTRA.iter())
            .copied()
            .filter(|f| !own.contains(f))
            .collect();
    }
    let mut out: Vec<&'static str> = HAZARDS.to_vec();
    match symbol {
        Some("src/def.ts#greet") => {
            for f in GREET_EXTRA {
                out.push(f);
            }
        }
        Some("src/decoy.ts#greet") => out.extend(["src/nsuser.ts", "src/named.ts"]),
        Some("src/loc.ts#loc") => out.push("src/loc_user.ts"),
        _ => {}
    }
    out
}

/// What one case gave.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Res {
    Exact,
    Wrong(String),
    Refused(Vec<String>),
    SymbolError(String),
}

fn reasons_of(plan: &Plan) -> Vec<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    for r in &plan.refused {
        let v = serde_json::to_value(r.reason).expect("reason json");
        out.insert(v.as_str().unwrap_or("?").to_string());
    }
    out.into_iter().collect()
}

/// Compares a ready plan with the oracle rows.
fn judge(plan: &Plan, rows: &[Row]) -> Result<(), String> {
    let mut code: BTreeSet<(String, usize, usize)> = BTreeSet::new();
    let mut doc: BTreeSet<(String, usize, usize)> = BTreeSet::new();
    let mut report_only: BTreeSet<(String, usize, usize)> = BTreeSet::new();
    for (path, start, end, tag) in rows {
        let key = (path.clone(), *start, *end);
        match tag.as_str() {
            "code" => code.insert(key),
            "jsdoc_link" | "jsdoc_type" => doc.insert(key),
            _ => report_only.insert(key),
        };
    }
    let mut got_code = BTreeSet::new();
    let mut got_doc = BTreeSet::new();
    for file in &plan.files {
        for edit in &file.edits {
            let key = (file.path.clone(), edit.start, edit.end);
            if edit.site == SiteClass::Jsdoc {
                got_doc.insert(key);
            } else {
                got_code.insert(key);
            }
        }
    }
    let touches_report = got_code
        .iter()
        .chain(&got_doc)
        .any(|k| report_only.contains(k));
    if got_code == code && got_doc == doc && !touches_report {
        return Ok(());
    }
    Err(format!(
        "missed code {:?}, extra code {:?}, missed doc {:?}, extra doc {:?}, edits a comment or string: {touches_report}",
        code.difference(&got_code).collect::<Vec<_>>(),
        got_code.difference(&code).collect::<Vec<_>>(),
        doc.difference(&got_doc).collect::<Vec<_>>(),
        got_doc.difference(&doc).collect::<Vec<_>>(),
    ))
}

/// Says whether a result meets the `expect` field.
/// Reasons that a refuse case may show besides the expected one, because the
/// same site breaks a second rule.
fn extra_reasons(case: &str) -> &'static [&'static str] {
    match case {
        // `d.greet` also has a relative namespace receiver (property rule).
        "ns_member" => &["unproven"],
        _ => &[],
    }
}

fn meets(case: &str, expect: &str, res: &Res) -> bool {
    match (expect, res) {
        ("plan" | "plan_if_no_importer" | "report_only", Res::Exact) => true,
        // `other`: the renamed name is not a function node, so no plan.
        ("other", Res::Refused(_) | Res::SymbolError(_)) => true,
        ("refuse", Res::Refused(_)) => true,
        ("refuse:symbol_error", Res::SymbolError(_)) => true,
        (e, Res::Refused(reasons)) if e.starts_with("refuse:") => {
            // The expected reason is present, and no other reason is.
            let want = e.trim_start_matches("refuse:");
            reasons.iter().any(|r| r == want)
                && reasons
                    .iter()
                    .all(|r| r == want || extra_reasons(case).contains(&r.as_str()))
        }
        _ => false,
    }
}

#[test]
fn test_rn2_matches_frozen_oracle() {
    let root = workspace_root();
    let fixture = root.join("tests/fixtures/rename1");
    let oracle = read_jsonl(&root.join("tests/fixtures/rename1.expected/oracle.jsonl"));
    let cases = read_jsonl(&fixture.join("cases.jsonl"));

    let mut rows: BTreeMap<String, Vec<Row>> = BTreeMap::new();
    let mut excluded: BTreeSet<String> = BTreeSet::new();
    for row in &oracle {
        let Some(case) = row.get("case").and_then(Value::as_str) else {
            continue;
        };
        if row.get("excluded").is_some() {
            excluded.insert(case.to_string());
            continue;
        }
        let get = |k: &str| row.get(k).and_then(Value::as_u64).unwrap_or(0) as usize;
        rows.entry(case.to_string()).or_default().push((
            row["path"].as_str().unwrap_or("").to_string(),
            get("start"),
            get("end"),
            row["tag"].as_str().unwrap_or("").to_string(),
        ));
    }

    let mut table: Vec<(String, String, String)> = Vec::new();
    let mut missing_expect: Vec<String> = Vec::new();
    let mut no_rows: Vec<String> = Vec::new();
    let mut wrong: Vec<String> = Vec::new();
    let mut mismatch: Vec<String> = Vec::new();
    let mut exact = 0usize;
    let mut refused: BTreeMap<String, usize> = BTreeMap::new();

    for case in &cases {
        let name = case["case"].as_str().expect("case name").to_string();
        if excluded.contains(&name) {
            table.push((name, "-".into(), "excluded".into()));
            continue;
        }
        let Some(expect) = case.get("expect").and_then(Value::as_str) else {
            missing_expect.push(name.clone());
            table.push((name, "(missing)".into(), "not run".into()));
            continue;
        };
        let Some(case_rows) = rows.get(&name) else {
            no_rows.push(name.clone());
            table.push((name, expect.into(), "no oracle rows".into()));
            continue;
        };
        let symbol = symbol_of(&name);
        let temp = TempDir::new(&name);
        let repo = temp.0.join("rename1");
        copy_tree(&fixture, &repo);
        // Git holds no symlink (the public export bans it), so make it here.
        std::os::unix::fs::symlink("named.ts", repo.join("src/linked.ts")).expect("symlink");
        for gone in removals(&name, expect, symbol) {
            fs::remove_file(repo.join(gone)).expect("remove hazard file");
        }
        let present: Vec<Row> = case_rows
            .iter()
            .filter(|(p, _, _, _)| fs::symlink_metadata(repo.join(p)).is_ok())
            .cloned()
            .collect();
        let new = case["new"].as_str().expect("new name");
        let bare = case["name"].as_str().expect("name");
        let result = plan_rename(
            &repo,
            &temp.0.join("ctx"),
            symbol.unwrap_or(bare),
            new,
            None,
        );
        let res = match result {
            Err(err) => Res::SymbolError(err.to_string()),
            Ok(plan) if plan.status == Status::Refused => {
                if expect.starts_with("plan") || expect == "report_only" {
                    eprintln!("{name}: refused: {:?}", plan.refused);
                }
                Res::Refused(reasons_of(&plan))
            }
            Ok(plan) => match judge(&plan, &present) {
                Ok(()) if symbol.is_some() => Res::Exact,
                Ok(()) => Res::Wrong("a plan for a name that is not a function".into()),
                Err(why) => Res::Wrong(why),
            },
        };
        match &res {
            Res::Exact => exact += 1,
            Res::Wrong(why) => {
                eprintln!("WRONG {name}: {why}");
                wrong.push(name.clone());
            }
            Res::Refused(reasons) => {
                for r in reasons {
                    *refused.entry(r.clone()).or_default() += 1;
                }
            }
            Res::SymbolError(_) => *refused.entry("symbol_error".into()).or_default() += 1,
        }
        if !meets(&name, expect, &res) {
            mismatch.push(name.clone());
        }
        table.push((name, expect.into(), format!("{res:?}")));
    }

    eprintln!("{:<24} {:<32} result", "case", "expect");
    for (case, expect, res) in &table {
        eprintln!("{case:<24} {expect:<32} {res}");
    }
    eprintln!(
        "Exact {exact}, Wrong {}, Refused per reason {refused:?}",
        wrong.len()
    );
    eprintln!("expect mismatches: {mismatch:?}");

    assert!(
        missing_expect.is_empty(),
        "cases with no expect: {missing_expect:?}"
    );
    assert!(no_rows.is_empty(), "cases with no oracle rows: {no_rows:?}");
    assert!(wrong.is_empty(), "wrong cases: {wrong:?}");
    assert!(
        mismatch.is_empty(),
        "cases that miss their expect field: {mismatch:?}"
    );
}
