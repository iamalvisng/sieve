//! Parity tests for the workspace build and the ancestor walk (P1-55,
//! P1-61, P2-38), against the recorded goldens under
//! `tests/fixtures/ws.expected/workspace/`. The helpers live in `support/ws.rs`.

mod support;

use std::fs;
use std::path::Path;
use std::process::Output;

use support::ws::{built_workspace, file_list, fresh_copy, git_init, sieve};

const GOLDEN: &str = "workspace";

fn golden(name: &str) -> String {
    support::ws::golden(GOLDEN, name)
}

fn assert_matches(id: &str, output: &Output, copy: &Path) {
    support::ws::assert_matches(GOLDEN, id, output, copy);
}

/// P1-55, P2-38: `build` at a parent with two git children builds each
/// child, prints the per-child lines and the federated line, and leaves
/// only `workspace.json` under the parent's context dir.
#[test]
fn test_p1_55_build_at_a_workspace_parent_matches_golden() {
    let (copy, output) = built_workspace("ws-build");
    assert_matches("build", &output, &copy);
    support::ws::bless(GOLDEN, "build.files.txt", &file_list(&copy));
    assert_eq!(file_list(&copy), golden("build.files.txt"), "file list");
    let index = fs::read_to_string(copy.path.join("sieve/workspace.json")).expect("read index");
    support::ws::bless(GOLDEN, "workspace.json", &index);
    assert_eq!(index, golden("workspace.json"));
    let gitignore = fs::read_to_string(copy.path.join(".gitignore")).expect("read gitignore");
    support::ws::bless(GOLDEN, "gitignore.txt", &gitignore);
    assert_eq!(gitignore, golden("gitignore.txt"));
}

/// P1-55: a mega graph built before the children were repos gets the
/// migration note, and the parent's old tree goes.
#[test]
fn test_p1_55_build_over_a_mega_graph_prints_the_migration_note() {
    let copy = fresh_copy("ws-migrate");
    let first = sieve(&copy.path, &["build"]);
    assert_eq!(first.status.code(), Some(0), "mega build");
    assert!(copy.path.join("sieve/.graph/wiring.json").is_file());
    git_init(&copy.path.join("alpha"));
    git_init(&copy.path.join("beta"));
    let output = sieve(&copy.path, &["build"]);
    assert_matches("migrate", &output, &copy);
    support::ws::bless(GOLDEN, "migrate.files.txt", &file_list(&copy));
    assert_eq!(file_list(&copy), golden("migrate.files.txt"), "file list");
}

/// P1-61: from a plain subdir of the parent, the ancestor walk stops at
/// the parent's `workspace.json`, not at the file system root.
#[test]
fn test_p1_61_walk_stops_at_the_workspace_parent() {
    let (copy, build) = built_workspace("ws-walk-parent");
    assert_eq!(build.status.code(), Some(0), "workspace build");
    let output = sieve(&copy.path.join("docs"), &["skeleton", "alpha/src/a.ts"]);
    assert_matches("skeleton-docs", &output, &copy);
}

/// P1-61: from a child's subdir, the walk stops at the child's own graph.
#[test]
fn test_p1_61_walk_stops_at_a_child_repo() {
    let (copy, build) = built_workspace("ws-walk-child");
    assert_eq!(build.status.code(), Some(0), "workspace build");
    let output = sieve(&copy.path.join("alpha/src"), &["grep", "alphaAdd"]);
    assert_matches("grep-alpha-src", &output, &copy);
}

/// P2-38: one git child is not a workspace; the parent builds as one
/// repo, with the single-repo report and tree.
#[test]
fn test_p2_38_one_git_child_is_not_a_workspace() {
    let copy = fresh_copy("ws-single");
    git_init(&copy.path.join("alpha"));
    let output = sieve(&copy.path, &["build"]);
    assert_matches("single", &output, &copy);
    support::ws::bless(GOLDEN, "single.files.txt", &file_list(&copy));
    assert_eq!(file_list(&copy), golden("single.files.txt"), "file list");
}
