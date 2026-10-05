//! Parity tests for CLI argument handling against a recorded run (P1-15, P1-21,
//! P1-23), against the goldens under `tests/fixtures/basic.expected/cli-args/`.
//! The query list lives in `tests/inputs/cli-args/basic.txt`; the four extra
//! copies live in `tests/inputs/cli-args/`.
//!
//! Each test byte-compares stdout, stderr and the exit code of the ids it
//! names, after one `sieve build` of a fresh fixture copy.

mod support;

use std::fs;
use std::path::{Path, PathBuf};

use support::TempDir;

const FIXTURE: &str = "basic";

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

fn fixture_dir() -> PathBuf {
    support::manifest_dir().join(format!("../../tests/fixtures/{FIXTURE}"))
}

fn golden_dir() -> PathBuf {
    support::manifest_dir().join(format!("../../tests/fixtures/{FIXTURE}.expected/cli-args"))
}

/// Copies the fixture into a fresh temp dir, unbuilt.
fn fresh_copy(label: &str) -> TempDir {
    let temp = TempDir::new(&format!("cli-args-{label}"));
    copy_dir(&fixture_dir(), &temp.path);
    temp
}

/// Builds the fixture into a fresh temp copy and returns the copy.
fn build_fixture(label: &str) -> TempDir {
    let temp = fresh_copy(label);
    let output = support::sieve_command()
        .args(["build", "."])
        .current_dir(&temp.path)
        .output()
        .expect("run sieve build");
    assert_eq!(
        output.status.code(),
        Some(0),
        "sieve build failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    temp
}

/// Reads the golden triple for one id.
fn golden(id: &str) -> (String, String, i32) {
    let dir = golden_dir();
    let read = |suffix: &str| {
        fs::read_to_string(dir.join(format!("{id}.{suffix}.txt")))
            .unwrap_or_else(|e| panic!("read golden cli-args/{id}.{suffix}.txt: {e}"))
    };
    let exit = read("exit").trim().parse::<i32>().expect("parse exit");
    (read("stdout"), normalize_stderr(&read("stderr")), exit)
}

/// Masks `dir`'s realpath and literal path with `<TMP>`, the way
/// the same mask the goldens use.
fn mask(dir: &Path, text: &str) -> String {
    let mut out = text.to_string();
    let real = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    for p in [real.as_path(), dir] {
        out = out.replace(&p.display().to_string(), "<TMP>");
    }
    out
}

/// The arguments of one id in the query list.
fn listed_args(id: &str) -> Vec<String> {
    let list = support::manifest_dir().join(format!("../../tests/inputs/cli-args/{FIXTURE}.txt"));
    let lines = fs::read_to_string(&list).expect("read cli-args query list");
    let args = lines
        .lines()
        .find_map(|l| l.split_once('\t').filter(|(lid, _)| lid == &id))
        .map(|(_, args)| args)
        .unwrap_or_else(|| panic!("id {id} missing from {}", list.display()));
    split_args(args)
}

/// Runs one `sieve` command in `dir` against the golden `id`. Prints the
/// first differing line and returns `false` on a mismatch.
fn run_matches(id: &str, dir: &Path, argv: &[String]) -> bool {
    let output = support::sieve_command()
        .args(argv)
        .current_dir(dir)
        .output()
        .expect("run sieve");
    let got_out = mask(dir, &String::from_utf8_lossy(&output.stdout));
    let got_err = mask(
        dir,
        &normalize_stderr(&String::from_utf8_lossy(&output.stderr)),
    );
    let got_exit = output.status.code().unwrap_or(-1);
    support::golden::bless_triple(
        &golden_dir(),
        id,
        got_out.as_bytes(),
        got_err.as_bytes(),
        got_exit,
    );
    let (want_out, want_err, want_exit) = golden(id);
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
            eprintln!("FAIL {id} {field}, first differing line {first}:");
            eprintln!("  -{}", want.lines().nth(first).unwrap_or("<missing>"));
            eprintln!("  +{}", got.lines().nth(first).unwrap_or("<missing>"));
        }
    }
    if want_exit != got_exit {
        ok = false;
        eprintln!("FAIL {id} exit: expected {want_exit}, got {got_exit}");
    }
    if ok {
        eprintln!("PASS {id}");
    }
    ok
}

/// The listed ids that mismatched when run in `dir`.
fn mismatches_in(dir: &Path, ids: &[&str]) -> Vec<String> {
    ids.iter()
        .filter(|id| !run_matches(id, dir, &listed_args(id)))
        .map(|id| id.to_string())
        .collect()
}

/// The `(id, argv)` pairs that mismatched when run in `dir`.
fn mismatches_argv(dir: &Path, runs: &[(&str, &[&str])]) -> Vec<String> {
    runs.iter()
        .filter(|(id, args)| {
            let argv: Vec<String> = args.iter().map(|a| a.to_string()).collect();
            !run_matches(id, dir, &argv)
        })
        .map(|(id, _)| id.to_string())
        .collect()
}

/// DV2 (P1-23): with a graph, the command reports the unknown `--in` prefix or
/// the unknown symbol before a bad `--direction` or `--depth`.
#[test]
fn test_p1_23_dv2_callers_checks_symbol_before_flags() {
    let temp = build_fixture("dv2");
    let failed = mismatches_in(
        &temp.path,
        &[
            "callers-dv2-direction",
            "callers-dv2-depth",
            "callers-dv2-in-direction",
            "callers-dv2-in-depth",
        ],
    );
    assert!(failed.is_empty(), "DV2 mismatched sieve: {failed:?}");
}

/// DV2 (P1-23): with no graph, the command reports the missing graph before
/// any flag check.
#[test]
fn test_p1_23_dv2_callers_checks_graph_before_flags() {
    let temp = fresh_copy("dv2-nograph");
    let failed = mismatches_argv(
        &temp.path,
        &[
            (
                "callers-dv2-nograph-direction",
                &["callers", "zzz_nope", "--direction", "sideways"],
            ),
            (
                "callers-dv2-nograph-depth",
                &["callers", "zzz_nope", "--depth", "0"],
            ),
            (
                "callers-dv2-nograph-in-direction",
                &[
                    "callers",
                    "zzz_nope",
                    "--direction",
                    "sideways",
                    "--in",
                    "nope",
                ],
            ),
            (
                "callers-dv2-nograph-in-depth",
                &["callers", "zzz_nope", "--depth", "0", "--in", "nope"],
            ),
        ],
    );
    assert!(
        failed.is_empty(),
        "DV2 no-graph mismatched sieve: {failed:?}"
    );
}

/// The built tree plus the unicode and BOM extras; the query refreshes.
fn build_with_extras(label: &str) -> TempDir {
    let temp = build_fixture(label);
    let extras = support::manifest_dir().join("../../tests/inputs/cli-args");
    fs::copy(extras.join("basic-uni.ts"), temp.path.join("src/uni.ts")).expect("copy uni.ts");
    fs::copy(extras.join("basic-bom.py"), temp.path.join("py/bom.py")).expect("copy bom.py");
    temp
}

/// DV1 (P1-21): the quote regex uses ASCII `\b`, so `café(2)` and
/// `𝒻oo(3)` never quote; the hit line stands alone. DV5 (P1-21): JS
/// `trim()` strips the BOM from a quoted line 1. The three runs share one
/// copy in capture order: only the first prints the refresh note.
#[test]
fn test_p1_21_dv1_dv5_quote_regex_is_ascii_and_trim_strips_the_bom() {
    let temp = build_with_extras("dv1-dv5");
    let failed = mismatches_argv(
        &temp.path,
        &[
            ("callers-dv1-cafe", &["callers", "café"]),
            ("callers-dv1-astral", &["callers", "𝒻oo"]),
            ("callers-dv5-bom", &["callers", "add"]),
        ],
    );
    assert!(failed.is_empty(), "DV1/DV5 mismatched sieve: {failed:?}");
}

/// grep DV1 (P1-15): `1 indexed file` and `2 indexed files` in the
/// zero-hit note.
#[test]
fn test_p1_15_grep_dv1_unreadable_note_is_singular_or_plural() {
    let one = build_fixture("grep-one");
    fs::remove_file(one.path.join("py/helpers.py")).expect("delete one file");
    let two = build_fixture("grep-two");
    fs::remove_file(two.path.join("py/helpers.py")).expect("delete one file");
    fs::remove_file(two.path.join("src/util.ts")).expect("delete two files");
    let argv: &[&str] = &["grep", "zzz_nohit", "--no-refresh"];
    let mut failed = mismatches_argv(&one.path, &[("grep-dv1-one", argv)]);
    failed.extend(mismatches_argv(&two.path, &[("grep-dv1-two", argv)]));
    assert!(failed.is_empty(), "grep DV1 mismatched sieve: {failed:?}");
}

/// P1-23: a negative value reaches Sieve's own check and prints its
/// `✗` line; a repeated flag keeps its last value.
#[test]
fn test_p1_23_clap_hyphen_values_and_repeated_flags_match_golden() {
    let temp = build_fixture("clap");
    let failed = mismatches_in(
        &temp.path,
        &[
            "callers-neg-depth",
            "callers-repeat-depth",
            "callers-repeat-json",
            "map-neg-max-dirs",
            "map-repeat-max-dirs",
            "ask-repeat-limit",
            "grep-repeat-in",
            "blast-neg-depth",
            "skeleton-repeat-json",
            "dir-repeat",
        ],
    );
    assert!(failed.is_empty(), "clap sweep mismatched sieve: {failed:?}");
}
