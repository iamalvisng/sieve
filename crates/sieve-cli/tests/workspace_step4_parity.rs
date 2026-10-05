//! Parity tests for workspace step 4: `skeleton`, `blast`, `stats` and
//! `version` do not federate at a parent (P1-62), against recorded
//! goldens under `tests/fixtures/ws.expected/workspace4/`. The helpers live in
//! `support/ws.rs`. The MCP half of step 4 (P4-44) is tested in
//! `crates/sieve-daemon/tests/mcp_workspace_parity.rs`.

mod support;

use std::fs;
use std::path::Path;
use std::process::Output;

use support::ws::{fresh_copy, git_init};
use support::TempDir;

const GOLDEN: &str = "workspace4";

const SHARED_CHAIN: &str = "\nexport function shared(): number {\n  return 1;\n}\n\n\
export function useShared(): number {\n  return shared();\n}\n\n\
export function useSharedTwice(): number {\n  return useShared() + useShared();\n}\n";

/// A built workspace with the `shared` chain in both children, as the
/// `workspace4` golden capture builds it.
fn built(label: &str) -> TempDir {
    let copy = fresh_copy(label);
    for rel in ["alpha/src/a.ts", "beta/src/b.ts"] {
        let path = copy.path.join(rel);
        let mut body = fs::read_to_string(&path).expect("read source");
        body.push_str(SHARED_CHAIN);
        fs::write(&path, body).expect("write source");
    }
    git_init(&copy.path.join("alpha"));
    git_init(&copy.path.join("beta"));
    let build = support::ws::sieve(&copy.path, &["build"]);
    assert_eq!(build.status.code(), Some(0), "workspace build");
    copy
}

/// Runs `sieve <args>` at the parent, with `HOME` pinned to an empty temp
/// dir: `stats` and `version` read files under `HOME`.
fn run(copy: &Path, home: &Path, args: &[&str]) -> Output {
    support::sieve_command()
        .args(args)
        .current_dir(copy)
        .env("HOME", home)
        .output()
        .expect("run sieve")
}

fn assert_case(id: &str, label: &str, args: &[&str]) {
    let copy = built(label);
    let home = TempDir::new(&format!("{label}-home"));
    let output = run(&copy.path, &home.path, args);
    support::ws::assert_matches(GOLDEN, id, &output, &copy.path);
}

/// P1-62: `skeleton` at the parent does not federate; the parent has no
/// wiring graph, so it prints the no-graph note and exits 0.
#[test]
fn test_p1_62_skeleton_at_a_workspace_parent_has_no_graph() {
    assert_case("skeleton", "ws4-skeleton", &["skeleton", "alpha/src/a.ts"]);
}

/// P1-62: `skeleton --json` at the parent prints the same note as JSON.
#[test]
fn test_p1_62_skeleton_json_at_a_workspace_parent_has_no_graph() {
    assert_case(
        "skeleton-json",
        "ws4-skeleton-json",
        &["skeleton", "alpha/src/a.ts", "--json"],
    );
}

/// P1-62: `blast` at the parent does not federate; it prints the
/// no-graph line and exits 1.
#[test]
fn test_p1_62_blast_at_a_workspace_parent_has_no_graph() {
    assert_case("blast", "ws4-blast", &["blast"]);
}

/// P1-62: `stats` at the parent reads the session, not the children.
#[test]
fn test_p1_62_stats_at_a_workspace_parent_is_not_federated() {
    assert_case("stats", "ws4-stats", &["stats"]);
}

/// P1-62: `stats --json` at the parent prints `null`.
#[test]
fn test_p1_62_stats_json_at_a_workspace_parent_is_not_federated() {
    assert_case("stats-json", "ws4-stats-json", &["stats", "--json"]);
}

/// P1-62: `version` at the parent prints the installed version first and
/// no workspace line. Sieve asks npm for the latest version, so only the
/// first line is in the golden.
#[test]
fn test_p1_62_version_at_a_workspace_parent_is_not_federated() {
    let copy = built("ws4-version");
    let home = TempDir::new("ws4-version-home");
    let output = run(&copy.path, &home.path, &["version"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let first = format!("{}\n", stdout.lines().next().unwrap_or_default());
    support::ws::bless(GOLDEN, "version.stdout.txt", &first);
    assert_eq!(first, support::ws::golden(GOLDEN, "version.stdout.txt"));
    assert!(!stdout.contains("workspace"), "no federated line: {stdout}");
    assert_eq!(output.status.code(), Some(0));
}
