//! Parity tests for the `sieve blast` divergences from a recorded run
//! (DV2, DV4, DV6, DV7 and DV12; P1-24, P1-27, P3-25, P3-26), against
//! the goldens under `tests/fixtures/edges.expected/blast-dv/`. Each case
//! is one `tests/inputs/blast-dv/<id>.sh`: its first line names the
//! arguments, the rest is a setup script.
//!
//! Each test builds the `edges` fixture into a fresh copy, commits it, applies
//! two edits to `ts/derived.ts` and `ts/newfile.ts`, runs the case setup, then
//! byte-compares stdout, stderr and the exit code with the golden.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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

/// Replaces every spelling of `root` with `<TMP>`.
fn mask_tmp(s: &str, root: &Path) -> String {
    let canon = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    s.replace(&canon.display().to_string(), "<TMP>")
        .replace(&root.display().to_string(), "<TMP>")
}

fn run_git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "sieve-fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@sieve.local")
        .env("GIT_COMMITTER_NAME", "sieve-fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@sieve.local")
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

fn case_dir() -> PathBuf {
    support::manifest_dir().join("../../tests/inputs/blast-dv")
}

fn golden_dir() -> PathBuf {
    support::manifest_dir().join("../../tests/fixtures/edges.expected/blast-dv")
}

/// Builds the `edges` fixture into a fresh copy, commits the built tree,
/// applies the two standard edits, then runs the case setup script in
/// the copy. Returns the copy and the case's arguments.
fn prepare_case(id: &str) -> (TempDir, Vec<String>) {
    let fixture = support::manifest_dir().join("../../tests/fixtures/edges");
    let temp = TempDir::new(&format!("blast-dv-{id}"));
    copy_dir(&fixture, &temp.path);

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

    run_git(&temp.path, &["init", "-q"]);
    // The repo-local identity: `blast` drops
    // the local identity from its reviewers, so the base commit's author
    // must be that identity, not the machine's global one.
    run_git(&temp.path, &["config", "user.name", "sieve-fixture"]);
    run_git(&temp.path, &["config", "user.email", "fixture@sieve.local"]);
    run_git(&temp.path, &["add", "-A"]);
    run_git(&temp.path, &["commit", "-q", "-m", "base"]);

    let derived = temp.path.join("ts").join("derived.ts");
    let mut source = fs::read_to_string(&derived).expect("read derived.ts");
    source.push_str("\nexport function extra(): number { return greet().length; }\n");
    fs::write(&derived, source).expect("edit derived.ts");
    fs::write(
        temp.path.join("ts").join("newfile.ts"),
        "export function newFn(): number {\n  return 1;\n}\n",
    )
    .expect("write newfile.ts");
    run_git(&temp.path, &["add", "-A"]);

    // The case list runs `edges.blast.txt` in the same copy before it
    // captures the cases, so the graph is fresh over the two edits when
    // a case setup starts. One discarded query mirrors that.
    let warm = support::sieve_command()
        .args(["blast", "-d", "1"])
        .current_dir(&temp.path)
        .output()
        .expect("run warm-up blast");
    assert_eq!(warm.status.code(), Some(0), "warm-up blast failed");

    let script = case_dir().join(format!("{id}.sh"));
    let text = fs::read_to_string(&script).unwrap_or_else(|e| panic!("read {id}.sh: {e}"));
    let args = text
        .lines()
        .find_map(|l| l.strip_prefix("# args: "))
        .unwrap_or_else(|| panic!("{id}.sh has no `# args:` line"));
    let argv: Vec<String> = args.split_whitespace().map(str::to_string).collect();

    let status = Command::new("bash")
        .arg(&script)
        .current_dir(&temp.path)
        .status()
        .expect("run case setup");
    assert!(status.success(), "setup {id}.sh failed");
    (temp, argv)
}

/// Runs one case and returns its masked stdout, normalized stderr and
/// exit code.
fn run_case(id: &str) -> (String, String, i32) {
    let (temp, argv) = prepare_case(id);
    let mut cmd = support::sieve_command();
    let output = cmd
        .args(&argv)
        .current_dir(&temp.path)
        .output()
        .expect("run sieve blast");
    let got = (
        mask_tmp(&String::from_utf8_lossy(&output.stdout), &temp.path),
        normalize_stderr(&mask_tmp(
            &String::from_utf8_lossy(&output.stderr),
            &temp.path,
        )),
        output.status.code().unwrap_or(-1),
    );
    support::golden::bless_triple(&golden_dir(), id, got.0.as_bytes(), got.1.as_bytes(), got.2);
    got
}

/// Reads the golden triple for one id.
fn golden(id: &str) -> (String, String, i32) {
    let read = |suffix: &str| {
        fs::read_to_string(golden_dir().join(format!("{id}.{suffix}.txt")))
            .unwrap_or_else(|e| panic!("read golden blast-dv/{id}.{suffix}.txt: {e}"))
    };
    let exit = read("exit").trim().parse::<i32>().expect("parse exit");
    (read("stdout"), normalize_stderr(&read("stderr")), exit)
}

/// Asserts that one case's stdout, stderr and exit match its golden.
fn assert_case_matches_golden(id: &str) {
    let (got_out, got_err, got_exit) = run_case(id);
    let (want_out, want_err, want_exit) = golden(id);
    assert_eq!(got_out, want_out, "blast-dv {id}: stdout mismatch");
    assert_eq!(got_err, want_err, "blast-dv {id}: stderr mismatch");
    assert_eq!(got_exit, want_exit, "blast-dv {id}: exit mismatch");
}

/// DV4 (P1-24, P1-27): on a dirty tree the refresh note comes before the
/// `--format` or `--depth` error.
#[test]
fn test_p1_24_p1_27_dv4_refresh_note_precedes_a_flag_error() {
    assert_case_matches_golden("format-bogus");
    assert_case_matches_golden("depth-zero");
}

/// DV6 (P3-25): module labels sort with `localeCompare`, so `alpha` comes
/// before `Zulu`.
#[test]
fn test_p3_25_dv6_modules_sort_with_locale_compare() {
    assert_case_matches_golden("order-modules");
    assert_case_matches_golden("order-modules-json");
}

/// DV6 (P3-25): area labels sort with `localeCompare` too (dist).
#[test]
fn test_p3_25_dv6_areas_sort_with_locale_compare() {
    assert_case_matches_golden("order-areas");
    assert_case_matches_golden("order-areas-json");
}

/// DV7 (P3-26): a diff with a latin1 byte keeps its ranges and hunks. Node
/// decodes the patch as UTF-8 with U+FFFD.
///
/// Only the `changed` array is compared. The build (sieve-parse) still
/// drops a file that is not UTF-8, so `seeds` and `unindexed` differ
/// from the golden until that crate reads the file the way Node does.
#[test]
fn test_p3_26_dv7_latin1_diff_keeps_its_ranges() {
    let (got_out, _, got_exit) = run_case("latin1-json");
    let (want_out, _, want_exit) = golden("latin1-json");
    assert_eq!(got_exit, want_exit, "blast-dv latin1-json: exit mismatch");
    let got: serde_json::Value = serde_json::from_str(&got_out).expect("parse sieve json");
    let want: serde_json::Value = serde_json::from_str(&want_out).expect("parse golden json");
    assert_eq!(
        got["changed"], want["changed"],
        "blast-dv latin1-json: `changed` mismatch"
    );
}

/// DV12: JS `trim()` strips U+FEFF from a concept label (dist).
#[test]
fn test_p1_25_dv12_label_trim_strips_a_bom() {
    assert_case_matches_golden("label-bom");
}

/// P1-27: a base ref that does not exist makes blast exit 1 with sieve's
/// message.
#[test]
fn test_p1_27_blast_bad_base_exits_1() {
    assert_case_matches_golden("base-nope");
    assert_eq!(golden("base-nope").2, 1, "the golden must be exit 1");
}

/// P1-27: with no graph, blast exits 1 and names the build command.
#[test]
fn test_p1_27_blast_no_graph_exits_1() {
    assert_case_matches_golden("no-graph");
    assert_eq!(golden("no-graph").2, 1, "the golden must be exit 1");
}

/// P3-20: blast with no `--depth` uses depth 2. The chain reaches l2 and
/// stops before l3, so a default of 1 or 3 changes the output.
#[test]
fn test_p3_20_blast_default_depth_is_2() {
    assert_case_matches_golden("depth-default");
    let (out, _, _) = golden("depth-default");
    assert!(out.contains("within 2 hops") && out.contains("l2  fn") && !out.contains("l3  fn"));
}

/// P3-22: blast follows in edges only. The seed `newFn` calls `leaf`, and
/// the report never lists `leaf`. It lists the three callers of `extra`.
#[test]
fn test_p3_22_blast_follows_in_edges_only() {
    assert_case_matches_golden("direction-in-only");
    let (out, _, _) = golden("direction-in-only");
    assert!(out.contains("l3 ") && !out.contains("leaf"));
}
