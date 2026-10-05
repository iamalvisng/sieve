//! Parity tests for `sieve blast --format markdown`, `--format mermaid`
//! and the owner block (DV1 and DV5; P1-24, P1-25, P1-26, P3-25), against
//! the goldens under `tests/fixtures/edges.expected/blast-fmt/`. Each
//! case is one `tests/inputs/blast-fmt/<id>.sh` in the `blast-dv` shape.
//!
//! The owner cases commit as a second author at run time, so the `last
//! today` text holds on every run. The JSON owner case pins its author
//! date, so `last` is byte-stable; `score` decays with the clock, so the
//! test masks it after a range check.

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

/// Runs git in `root`'s fixture identity. `blast` drops
/// the repo-local identity from its reviewers, so the base commit must
/// carry that identity.
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
    support::manifest_dir().join("../../tests/inputs/blast-fmt")
}

fn golden_dir() -> PathBuf {
    support::manifest_dir().join("../../tests/fixtures/edges.expected/blast-fmt")
}

/// Builds the `edges` fixture into a fresh copy, commits the built tree,
/// applies the two standard edits, then runs the case setup script in
/// the copy. Returns the copy and the case's arguments.
fn prepare_case(id: &str) -> (TempDir, Vec<String>) {
    let fixture = support::manifest_dir().join("../../tests/fixtures/edges");
    let temp = TempDir::new(&format!("blast-fmt-{id}"));
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
    // A fixed clock keeps the owner scores of the golden stable.
    cmd.env("SIEVE_TEST_NOW_MS", "1790000000000");
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
            .unwrap_or_else(|e| panic!("read golden blast-fmt/{id}.{suffix}.txt: {e}"))
    };
    let exit = read("exit").trim().parse::<i32>().expect("parse exit");
    (read("stdout"), normalize_stderr(&read("stderr")), exit)
}

/// Asserts that one case's stdout, stderr and exit match its golden.
fn assert_case_matches_golden(id: &str) {
    let (got_out, got_err, got_exit) = run_case(id);
    let (want_out, want_err, want_exit) = golden(id);
    assert_eq!(got_out, want_out, "blast-fmt {id}: stdout mismatch");
    assert_eq!(got_err, want_err, "blast-fmt {id}: stderr mismatch");
    assert_eq!(got_exit, want_exit, "blast-fmt {id}: exit mismatch");
}

/// DV1 (P1-24, P1-26): markdown with no dependents prints the "Nothing
/// outside this diff depends on it" headline, no diagram and no table.
#[test]
fn test_p1_24_p1_26_dv1_markdown_with_no_dependents() {
    assert_case_matches_golden("md-empty");
}

/// DV1, DV5 (P1-24, P1-25, P1-26): markdown with two areas and two
/// modules: the tag line, the diagram, the table, the owner table and the
/// quoted evidence lines, in the section order.
#[test]
fn test_p1_26_dv1_markdown_with_two_areas_and_two_modules() {
    assert_case_matches_golden("md-modules");
}

/// DV1 (P1-26): the markdown caps: 5 diagram boxes plus the tail circle,
/// 6 table rows plus the "smaller areas" row, 60 listed symbols, and 8
/// owner rows plus the "further area" row.
#[test]
fn test_p1_26_dv1_markdown_caps() {
    assert_case_matches_golden("md-caps");
}

/// DV1 (P1-24): mermaid with no dependents prints `%% no dependents to
/// draw` and exits 0.
#[test]
fn test_p1_24_dv1_mermaid_with_no_dependents() {
    assert_case_matches_golden("mermaid-empty");
}

/// DV1 (P1-24): mermaid with two dependent modules draws two circles.
#[test]
fn test_p1_24_dv1_mermaid_with_dependents() {
    assert_case_matches_golden("mermaid-modules");
}

/// DV5 (P1-25, P3-25): a second author ends the text report with a
/// `who to tag` block that reads `last today`.
#[test]
fn test_p1_25_p3_25_dv5_second_author_in_text() {
    assert_case_matches_golden("owners-text");
}

/// Replaces every `score` number under `key` with a placeholder, after a
/// range check: a score is a sum of half-life weights, so it is in (0, 1]
/// for one commit.
fn mask_scores(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(score) = map.get_mut("score") {
                let n = score.as_f64().expect("score is a number");
                assert!(n > 0.0 && n <= 1.0, "score {n} is outside (0, 1]");
                *score = serde_json::Value::String("<score>".to_string());
            }
            for v in map.values_mut() {
                mask_scores(v);
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(mask_scores),
        _ => {}
    }
}

/// DV5 (P1-25, P3-25): the JSON report carries `owners` with `name`,
/// `commits`, `score` and `last`, and `reviewers` with `areas`. `last`
/// is the pinned author date in ms; `score` is masked.
#[test]
fn test_p1_25_p3_25_dv5_second_author_in_json() {
    let (got_out, got_err, got_exit) = run_case("owners-json");
    let (want_out, want_err, want_exit) = golden("owners-json");
    assert_eq!(got_err, want_err, "blast-fmt owners-json: stderr mismatch");
    assert_eq!(got_exit, want_exit, "blast-fmt owners-json: exit mismatch");
    let mut got: serde_json::Value = serde_json::from_str(&got_out).expect("parse sieve json");
    let mut want: serde_json::Value = serde_json::from_str(&want_out).expect("parse golden json");
    mask_scores(&mut got);
    mask_scores(&mut want);
    assert_eq!(got, want, "blast-fmt owners-json: report mismatch");
    // The key order is the golden's: a byte compare of the masked texts.
    let got_text = serde_json::to_string_pretty(&got).expect("serialize");
    let want_text = serde_json::to_string_pretty(&want).expect("serialize");
    assert_eq!(
        got_text, want_text,
        "blast-fmt owners-json: key order mismatch"
    );
}

/// DV5 (P1-24, P1-25): `--no-owners` leaves `owners` off every area and
/// module, and `reviewers` absent.
#[test]
fn test_p1_24_dv5_no_owners_json() {
    assert_case_matches_golden("no-owners");
}

/// DV5 (P1-24, P1-25): `--pr-author <email>` drops that author, so the
/// other second author is the one to tag.
#[test]
fn test_p1_24_dv5_pr_author_drops_the_named_author() {
    assert_case_matches_golden("pr-author");
}
