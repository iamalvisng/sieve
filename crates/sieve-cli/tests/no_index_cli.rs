//! Every query command prints one clear line, and exits 1, when the repo
//! has no built index. No command builds the index on its own.

mod support;

use std::fs;

use support::TempDir;

const LINE: &str = "sieve: no index here yet \u{2014} run sieve build .";

/// Runs one query command in an empty dir and checks the line, the exit
/// code and that the query makes no folder.
fn check_no_index(args: &[&str]) {
    let temp = TempDir::new("no-index");
    let output = support::sieve_command()
        .args(args)
        .current_dir(&temp.path)
        .output()
        .expect("run sieve");
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.status.code(), Some(1), "{args:?}: {all}");
    assert!(all.contains(LINE), "{args:?}: {all}");
    assert!(!temp.path.join("sieve").exists(), "{args:?} made a folder");
}

#[test]
fn test_no_index_ask() {
    check_no_index(&["ask", "anything"]);
}

#[test]
fn test_no_index_grep() {
    check_no_index(&["grep", "anything"]);
}

#[test]
fn test_no_index_callers() {
    check_no_index(&["callers", "anything"]);
}

#[test]
fn test_no_index_skeleton() {
    check_no_index(&["skeleton", "a.rs"]);
}

#[test]
fn test_no_index_map() {
    check_no_index(&["map"]);
}

#[test]
fn test_no_index_blast() {
    check_no_index(&["blast"]);
}

#[test]
fn test_no_index_why() {
    check_no_index(&["why", "anything"]);
}

/// Runs `sieve` with `args` in `dir`; returns the exit code and all output.
fn run_in(dir: &std::path::Path, args: &[&str]) -> (Option<i32>, String) {
    let output = support::sieve_command()
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run sieve");
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.code(), all)
}

/// A workspace parent has no index of its own. The commands that do not
/// federate say so, and never print the no-index line.
#[test]
fn test_no_index_line_never_fires_at_a_workspace_parent() {
    let temp = TempDir::new("no-index-ws");
    fs::create_dir_all(temp.path.join("sieve")).expect("mkdir");
    fs::create_dir_all(temp.path.join("alpha")).expect("mkdir");
    fs::write(
        temp.path.join("sieve/workspace.json"),
        "{\n  \"version\": 1,\n  \"children\": [\"alpha\"]\n}\n",
    )
    .expect("write workspace.json");
    for args in [
        &["skeleton", "zzz/a.rs"][..],
        &["callers", "x", "--json"],
        &["map", "--json"],
        &["blast"],
    ] {
        let (code, all) = run_in(&temp.path, args);
        assert_eq!(code, Some(1), "{args:?}: {all}");
        assert!(all.contains("workspace parent"), "{args:?}: {all}");
        assert!(!all.contains(LINE), "{args:?}: {all}");
    }
}

/// A `--dir` that does not exist is a missing directory, not a missing index.
#[test]
fn test_no_index_missing_dir_says_directory_not_found() {
    let temp = TempDir::new("no-index-dir");
    for args in [
        &["ask", "q", "--dir", "nope"][..],
        &["callers", "a", "nope"],
    ] {
        let (code, all) = run_in(&temp.path, args);
        assert_eq!(code, Some(1), "{args:?}: {all}");
        assert!(all.contains("directory not found: nope"), "{args:?}: {all}");
        assert!(!all.contains(LINE), "{args:?}: {all}");
    }
}

/// A repo with an index never sees the line.
#[test]
fn test_no_index_line_does_not_fire_with_an_index() {
    let temp = TempDir::new("no-index-built");
    fs::write(temp.path.join("a.ts"), "export function alpha() {}\n").expect("write");
    let (code, all) = run_in(&temp.path, &["build", "."]);
    assert_eq!(code, Some(0), "{all}");
    for args in [
        &["ask", "alpha"][..],
        &["grep", "alpha"],
        &["skeleton", "a.ts"],
    ] {
        let (code, all) = run_in(&temp.path, args);
        assert_eq!(code, Some(0), "{args:?}: {all}");
        assert!(!all.contains(LINE), "{args:?}: {all}");
    }
}
