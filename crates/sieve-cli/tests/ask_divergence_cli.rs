//! Parity tests for the `sieve ask` divergences from a recorded run
//! (DV1 to DV8, P1-02 to P1-07), against the goldens under
//! `tests/fixtures/<name>.expected/ask-dv/`. The query lists live in
//! `tests/inputs/ask-dv/<name>.txt`.
//!
//! Each test byte-compares stdout, stderr and the exit code of the ids it
//! names, after one `sieve build` of a fresh fixture copy.

mod support;

use std::fs;
use std::path::Path;

use support::TempDir;

/// Copies `src` into `dst`, recursively, including dot-directories.
fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("create dst dir");
    for entry in fs::read_dir(src).expect("read src dir") {
        let entry = entry.expect("read dir entry");
        let target = dst.join(entry.file_name());
        if entry.file_type().expect("read file type").is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy file");
        }
    }
}

/// Splits a query line's arguments the way the golden was produced:
/// whitespace-separated, with double quotes grouping one argument.
fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    for c in s.chars() {
        match c {
            '"' => in_quotes = !in_quotes,
            c if c.is_whitespace() && !in_quotes => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Drops every `⬆` update-nudge line, then rejoins with one trailing
/// newline when anything remains.
fn normalize_stderr(s: &str) -> String {
    let kept: Vec<&str> = s.lines().filter(|l| !l.starts_with('⬆')).collect();
    if kept.is_empty() {
        String::new()
    } else {
        format!("{}\n", kept.join("\n"))
    }
}

/// Builds one fixture into a fresh temp copy and returns the copy.
fn build_fixture(name: &str) -> TempDir {
    let fixture = support::manifest_dir().join(format!("../../tests/fixtures/{name}"));
    let temp = TempDir::new(&format!("ask-dv-{name}"));
    copy_dir(&fixture, &temp.path);
    let output = support::sieve_command()
        .args(["build", "."])
        .current_dir(&temp.path)
        .output()
        .expect("run sieve build");
    assert_eq!(
        output.status.code(),
        Some(0),
        "sieve build failed for {name}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    temp
}

/// Reads the golden triple for one id.
fn golden(name: &str, id: &str) -> (String, String, i32) {
    let dir = support::manifest_dir().join(format!("../../tests/fixtures/{name}.expected/ask-dv"));
    let read = |suffix: &str| {
        fs::read_to_string(dir.join(format!("{id}.{suffix}.txt")))
            .unwrap_or_else(|e| panic!("read golden {name}/ask-dv/{id}.{suffix}.txt: {e}"))
    };
    let exit = read("exit").trim().parse::<i32>().expect("parse exit");
    (read("stdout"), normalize_stderr(&read("stderr")), exit)
}

/// Runs every listed id of one fixture against its golden and returns
/// the ids that mismatched, with the first differing line printed.
fn mismatches(name: &str, ids: &[&str]) -> Vec<String> {
    let list = support::manifest_dir().join(format!("../../tests/inputs/ask-dv/{name}.txt"));
    let lines = fs::read_to_string(&list).expect("read ask-dv query list");
    let temp = build_fixture(name);
    let mut failed = Vec::new();
    for id in ids {
        let args = lines
            .lines()
            .find_map(|l| l.split_once('\t').filter(|(lid, _)| lid == id))
            .map(|(_, args)| args)
            .unwrap_or_else(|| panic!("id {id} missing from {}", list.display()));
        let mut argv = split_args(args);
        argv[0] = "ask".to_string();
        if !run_matches(name, id, &temp.path, &argv) {
            failed.push(id.to_string());
        }
    }
    failed
}

/// Runs one `sieve` command in `dir` against the golden `id` of one
/// fixture. Prints the first differing line and returns `false` on a
/// mismatch.
fn run_matches(name: &str, id: &str, dir: &Path, argv: &[String]) -> bool {
    let output = support::sieve_command()
        .args(argv)
        .current_dir(dir)
        .output()
        .expect("run sieve ask");
    let got_out = String::from_utf8_lossy(&output.stdout).into_owned();
    let got_err = normalize_stderr(&String::from_utf8_lossy(&output.stderr));
    let got_exit = output.status.code().unwrap_or(-1);
    let dir = support::manifest_dir().join(format!("../../tests/fixtures/{name}.expected/ask-dv"));
    support::golden::bless_triple(&dir, id, got_out.as_bytes(), got_err.as_bytes(), got_exit);
    let (want_out, want_err, want_exit) = golden(name, id);
    let mut ok = true;
    for (field, want, got) in [
        ("stdout", &want_out, &got_out),
        ("stderr", &want_err, &got_err),
    ] {
        if want != got {
            ok = false;
            let first = want
                .lines()
                .zip(got.lines())
                .position(|(w, g)| w != g)
                .unwrap_or(want.lines().count().min(got.lines().count()));
            eprintln!("FAIL {name}/{id} {field}, first differing line {first}:");
            eprintln!("  -{}", want.lines().nth(first).unwrap_or("<missing>"));
            eprintln!("  +{}", got.lines().nth(first).unwrap_or("<missing>"));
        }
    }
    if want_exit != got_exit {
        ok = false;
        eprintln!("FAIL {name}/{id} exit: expected {want_exit}, got {got_exit}");
    }
    if ok {
        eprintln!("PASS {name}/{id}");
    }
    ok
}

/// The ids of one fixture that mismatched when run in `dir`, a copy the
/// caller already built and edited.
fn mismatches_in(name: &str, dir: &Path, runs: &[(&str, &[&str])]) -> Vec<String> {
    let mut failed = Vec::new();
    for (id, args) in runs {
        let argv: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        if !run_matches(name, id, dir, &argv) {
            failed.push(id.to_string());
        }
    }
    failed
}

/// DV1 (P1-06): a structural `--json` result carries `subject` and no
/// `coverage`; an empty result carries neither.
#[test]
fn test_p1_06_dv1_structural_json_has_subject_and_no_coverage() {
    let failed = mismatches("basic", &["dv1-calls-json", "dv1-zero-json"]);
    assert!(failed.is_empty(), "DV1 mismatched sieve: {failed:?}");
}

/// DV2 (P1-04): the structural intent regex uses ASCII word boundaries, so
/// `éuses` still matches `\buses\b` and `résumé` never matches `\w+`.
#[test]
fn test_p1_04_dv2_structural_regex_is_ascii() {
    let failed = mismatches("basic", &["dv2-eaccent-uses", "dv2-resume-call"]);
    assert!(failed.is_empty(), "DV2 mismatched sieve: {failed:?}");
}

/// DV3 (P1-02): `-n 0` keeps the `lexical` mode and prints `no matches.`;
/// the mode reads the scored set, not the cut hit list.
#[test]
fn test_p1_02_dv3_limit_zero_keeps_the_lexical_mode() {
    let failed = mismatches("basic", &["dv3-n0"]);
    assert!(failed.is_empty(), "DV3 mismatched sieve: {failed:?}");
}

/// DV5 (P1-02): `--source --json` appends `saved` with the file count and
/// the baseline chars, in lexical and structural mode, and omits it when
/// no hit names a file.
#[test]
fn test_p1_02_dv5_source_json_carries_saved() {
    let mut failed = mismatches("basic", &["dv5-source-json", "dv5-calls-source-json"]);
    failed.extend(mismatches("scopes", &["dv5-source-json"]));
    assert!(failed.is_empty(), "DV5 mismatched sieve: {failed:?}");
}

/// DV6 (P1-05): a zero-hit note on a multi-scope graph ends with the
/// `scopes here:` clause, in graph scope order, root last as `(root)`.
#[test]
fn test_p1_05_dv6_multi_scope_zero_hit_note_lists_scopes() {
    let failed = mismatches("multi", &["dv6-zero", "dv6-zero-json", "dv6-calls-in"]);
    assert!(failed.is_empty(), "DV6 mismatched sieve: {failed:?}");
}

/// DV7 (P1-06): a concept top hit carries `coverage` and `coverageStrong`
/// from the concept's name and body bags.
#[test]
fn test_p1_06_dv7_concept_top_json_carries_coverage() {
    let failed = mismatches("askx", &["dv7-concept-top-json", "dv7-concept-weak-json"]);
    assert!(failed.is_empty(), "DV7 mismatched sieve: {failed:?}");
}

/// DV3 with `--in` (P1-02): the one-scope branch of the multi-scope path
/// keeps the `lexical` mode with `-n 0`.
#[test]
fn test_p1_02_dv3_limit_zero_with_in_keeps_the_lexical_mode() {
    let failed = mismatches("multi", &["dv3-n0-in"]);
    assert!(failed.is_empty(), "DV3 --in mismatched sieve: {failed:?}");
}

/// P1-02: the default limit is 8. `render` has 12 hits at `-n 20`, and
/// the run with no `-n` prints 8.
#[test]
fn test_p1_02_default_limit_is_eight() {
    let (twenty, _, _) = golden("multi", "dv-limit-20");
    let (default, _, _) = golden("multi", "dv-limit-default");
    assert!(
        twenty.matches("\"pointer\"").count() > 8,
        "the -n 20 golden has 8 hits or fewer"
    );
    assert_eq!(
        default.matches("\"pointer\"").count(),
        8,
        "the default golden has not 8 hits"
    );
    let failed = mismatches("multi", &["dv-limit-default", "dv-limit-20"]);
    assert!(
        failed.is_empty(),
        "default limit mismatched sieve: {failed:?}"
    );
}

/// P1-02: `--no-refresh` answers from the stale graph. The copy gains
/// `src/extra.ts` after the build; the run with the flag finds nothing,
/// and the run without the flag refreshes and finds `septuple`.
#[test]
fn test_p1_02_no_refresh_answers_from_the_stale_graph() {
    let (stale, _, _) = golden("basic", "dv-norefresh-stale");
    let (fresh, _, _) = golden("basic", "dv-norefresh-fresh");
    assert_ne!(stale, fresh, "the two goldens must differ");
    let temp = build_fixture("basic");
    let extra = support::manifest_dir().join("../../tests/inputs/ask-dv/basic-extra.ts");
    fs::copy(&extra, temp.path.join("src/extra.ts")).expect("copy extra.ts");
    let failed = mismatches_in(
        "basic",
        &temp.path,
        &[
            ("dv-norefresh-stale", &["ask", "septuple", "--no-refresh"]),
            ("dv-norefresh-fresh", &["ask", "septuple"]),
        ],
    );
    assert!(
        failed.is_empty(),
        "--no-refresh mismatched sieve: {failed:?}"
    );
}

/// DV8 (P1-07): a concept source with no `hash` still parses. The title
/// and the pointer come from the frontmatter, not from the file stem.
#[test]
fn test_p1_07_dv8_concept_source_without_hash_still_parses() {
    let temp = build_fixture("askx");
    let doc = temp.path.join("sieve/widget-notes.md");
    let text = fs::read_to_string(&doc).expect("read the concept doc");
    let stripped: String = text
        .lines()
        .filter(|l| !l.starts_with("    hash: "))
        .map(|l| format!("{l}\n"))
        .collect();
    assert_ne!(text, stripped, "the hash line was not found");
    fs::write(&doc, stripped).expect("write the concept doc");
    let failed = mismatches_in(
        "askx",
        &temp.path,
        &[
            ("dv8-nohash", &["ask", "notes"]),
            ("dv8-nohash-json", &["ask", "notes", "--json"]),
        ],
    );
    assert!(failed.is_empty(), "DV8 mismatched sieve: {failed:?}");
}

/// DV9 (P1-07): a `links:` entry with no `relation` still parses. Sieve
/// keeps the concept, its title, and its `related` roster.
#[test]
fn test_p1_07_dv9_concept_link_without_relation_still_parses() {
    let temp = build_fixture("askx");
    let doc = temp.path.join("sieve/widget-notes.md");
    let text = fs::read_to_string(&doc).expect("read the concept doc");
    let stripped: String = text
        .lines()
        .filter(|l| !l.starts_with("    relation: "))
        .map(|l| format!("{l}\n"))
        .collect();
    assert_ne!(text, stripped, "the relation lines were not found");
    fs::write(&doc, stripped).expect("write the concept doc");
    let failed = mismatches_in(
        "askx",
        &temp.path,
        &[
            ("dv9-norelation", &["ask", "notes"]),
            ("dv9-norelation-json", &["ask", "notes", "--json"]),
        ],
    );
    assert!(failed.is_empty(), "DV9 mismatched sieve: {failed:?}");
}

/// DV10 (P1-07): with `--source`, a span that ends past the end of the
/// file gives no code. Sieve sets the code only when it is not empty, so
/// there is no empty code block (text) and no `"code": ""` (JSON). The
/// source file is cut short after the build and read with `--no-refresh`.
#[test]
fn test_p1_07_dv10_source_past_end_of_file_gives_no_code() {
    let temp = build_fixture("askx");
    fs::write(
        temp.path.join("src/widget.ts"),
        "export function widget(n: number): number {\n  return n + 1;\n}\n",
    )
    .expect("cut the source short");
    let failed = mismatches_in(
        "askx",
        &temp.path,
        &[
            (
                "dv10-eof-text",
                &["ask", "widget", "--source", "--no-refresh"],
            ),
            (
                "dv10-eof-json",
                &["ask", "bigwidget", "--source", "--no-refresh", "--json"],
            ),
        ],
    );
    assert!(failed.is_empty(), "DV10 mismatched sieve: {failed:?}");
}
