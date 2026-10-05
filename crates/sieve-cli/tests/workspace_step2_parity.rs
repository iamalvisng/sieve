//! Parity tests for workspace step 2: the federated `grep` and `check`,
//! the coverage note, the child refresh, and the symlinked non-child
//! (P1-57, P1-59, P1-60, P2-39), against the recorded goldens under
//! `tests/fixtures/ws.expected/workspace2/`. The helpers live in `support/ws.rs`.

mod support;

use std::fs;
use std::path::Path;
use std::process::Output;

use support::ws::{built_workspace, fresh_copy, git_init, sieve};
use support::TempDir;

const GOLDEN: &str = "workspace2";

fn golden(name: &str) -> String {
    support::ws::golden(GOLDEN, name)
}

fn assert_matches(id: &str, output: &Output, copy: &Path) {
    support::ws::assert_matches(GOLDEN, id, output, copy);
}

/// A built workspace, with the build asserted clean.
fn built(label: &str) -> TempDir {
    let (copy, build) = built_workspace(label);
    assert_eq!(build.status.code(), Some(0), "workspace build");
    copy
}

/// Appends `alphaExtra` to.
fn edit_alpha(copy: &Path) {
    let path = copy.join("alpha/src/a.ts");
    let mut body = fs::read_to_string(&path).expect("read a.ts");
    body.push_str("\nexport function alphaExtra(): number {\n  return 3;\n}\n");
    fs::write(&path, body).expect("write a.ts");
}

/// P1-57: `grep` at the parent searches both children, prefixes each
/// path with the child dir, and sums the counts.
#[test]
fn test_p1_57_grep_at_a_workspace_parent_hits_both_children() {
    let copy = built("ws2-grep-both");
    let output = sieve(&copy.path, &["grep", "return"]);
    assert_matches("grep-both", &output, &copy);
}

/// P1-57: a zero hit at the parent prints the merged zero-hit note, with
/// the summed indexed-file count.
#[test]
fn test_p1_57_grep_zero_hit_at_a_workspace_parent() {
    let copy = built("ws2-grep-zero");
    let output = sieve(&copy.path, &["grep", "nothingHere"]);
    assert_matches("grep-zero", &output, &copy);
}

/// P1-57: `grep --json` at the parent prints the merged result, with the
/// summed `saved` baseline.
#[test]
fn test_p1_57_grep_json_at_a_workspace_parent() {
    let copy = built("ws2-grep-json");
    let output = sieve(&copy.path, &["grep", "--json", "return"]);
    assert_matches("grep-json", &output, &copy);
}

/// P1-57: sieve drops `--in` at a workspace; the search covers every
/// child.
#[test]
fn test_p1_57_grep_in_is_ignored_at_a_workspace_parent() {
    let copy = built("ws2-grep-in");
    let output = sieve(&copy.path, &["grep", "--in", "alpha", "return"]);
    assert_matches("grep-in", &output, &copy);
}

/// P1-59: `check` at the parent prints one `OK` line per built child,
/// and `--json` is ignored.
#[test]
fn test_p1_59_check_at_a_workspace_parent_with_both_children_built() {
    let copy = built("ws2-check-ok");
    let output = sieve(&copy.path, &["check"]);
    assert_matches("check-ok", &output, &copy);
    let output = sieve(&copy.path, &["check", "--json"]);
    assert_matches("check-json", &output, &copy);
}

/// P1-59, P1-60: an unbuilt child is `not built`, then the coverage
/// note; the exit stays 0. `grep` adds the same note after its body,
/// or after the zero-hit note on stderr.
#[test]
fn test_p1_60_check_and_grep_with_one_child_unbuilt() {
    let copy = built("ws2-unbuilt");
    fs::remove_dir_all(copy.path.join("beta/sieve")).expect("remove beta graph");
    let output = sieve(&copy.path, &["check"]);
    assert_matches("check-unbuilt", &output, &copy);
    let output = sieve(&copy.path, &["grep", "return"]);
    assert_matches("grep-coverage", &output, &copy);
    let output = sieve(&copy.path, &["grep", "nothingHere"]);
    assert_matches("grep-coverage-zero", &output, &copy);
}

/// P1-59: a stale child is `STALE (…)`, and the exit is 1. `check` runs
/// no refresh, so the edit stays visible.
#[test]
fn test_p1_59_check_with_one_child_stale() {
    let copy = built("ws2-check-stale");
    edit_alpha(&copy.path);
    let output = sieve(&copy.path, &["check"]);
    assert_matches("check-stale", &output, &copy);
}

/// P2-39: a query at the parent refreshes the edited child first, with
/// The two-part note, and answers from the fresh graph.
#[test]
fn test_p2_39_grep_refreshes_an_edited_child_before_answering() {
    let copy = built("ws2-refresh");
    edit_alpha(&copy.path);
    let output = sieve(&copy.path, &["grep", "alphaExtra"]);
    assert_matches("grep-refresh", &output, &copy);
}

/// P2-38: a symlinked dir beside the children is not a workspace member:
/// the build names two repos, the index lists two, and `grep` searches
/// two.
#[cfg(unix)]
#[test]
fn test_p2_38_a_symlinked_dir_is_not_a_workspace_child() {
    let copy = fresh_copy("ws2-symlink");
    git_init(&copy.path.join("alpha"));
    git_init(&copy.path.join("beta"));
    std::os::unix::fs::symlink("alpha", copy.path.join("gamma")).expect("symlink");
    let output = sieve(&copy.path, &["build"]);
    assert_matches("symlink-build", &output, &copy);
    let index = fs::read_to_string(copy.path.join("sieve/workspace.json")).expect("read index");
    support::ws::bless(GOLDEN, "symlink-workspace.json", &index);
    assert_eq!(index, golden("symlink-workspace.json"));
    let output = sieve(&copy.path, &["grep", "return"]);
    assert_matches("symlink-grep", &output, &copy);
}

/// A deliberate safety rule (approved 2026-10-01): `build --dir
/// <root>` at a workspace parent would delete the root, and every child
/// repo with it. Sieve refuses with one `✗` line and exit 1, and touches
/// nothing. There is no golden here, because a plain run deletes the folder.
#[test]
fn test_build_dir_at_the_workspace_root_refuses_and_keeps_the_children() {
    let copy = fresh_copy("ws2-dir-root");
    git_init(&copy.path.join("alpha"));
    git_init(&copy.path.join("beta"));
    let root = copy.path.to_string_lossy().into_owned();
    let output = sieve(&copy.path, &["--dir", &root, "build"]);
    assert_eq!(output.status.code(), Some(1), "exit");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let canon = fs::canonicalize(&copy.path).expect("canonicalize");
    assert!(
        stderr.contains(&format!(
            "✗ refusing to build: the context dir {} contains the workspace root or a child repo",
            canon.display()
        )),
        "stderr: {stderr}"
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "", "stdout");
    for file in ["alpha/src/a.ts", "beta/src/b.ts", "alpha/.git", "beta/.git"] {
        assert!(copy.path.join(file).exists(), "{file} still exists");
    }
    assert!(
        !copy.path.join("alpha/sieve").exists(),
        "no child build ran"
    );
}
