//! Parity tests for workspace step 3: the federated `map` and `callers` (P1-54,
//! P1-58), against the recorded goldens under
//! `tests/fixtures/ws.expected/workspace3/`. The helpers live in
//! `support/ws.rs`. The fixture has no symbol in both children, so each test
//! appends the same `shared` chain to both children before the build.

mod support;

use std::fs;
use std::path::Path;
use std::process::Output;

use support::ws::{fresh_copy, git_init, sieve};
use support::TempDir;

const GOLDEN: &str = "workspace3";

const SHARED_CHAIN: &str = "\nexport function shared(): number {\n  return 1;\n}\n\n\
export function useShared(): number {\n  return shared();\n}\n\n\
export function useSharedTwice(): number {\n  return useShared() + useShared();\n}\n";

fn assert_matches(id: &str, output: &Output, copy: &Path) {
    support::ws::assert_matches(GOLDEN, id, output, copy);
}

/// Appends the `shared` chain to `path`.
fn append_shared(path: &Path) {
    let mut body = fs::read_to_string(path).expect("read source");
    body.push_str(SHARED_CHAIN);
    fs::write(path, body).expect("write source");
}

/// A built workspace with the `shared` chain in both children.
fn built(label: &str) -> TempDir {
    let copy = fresh_copy(label);
    append_shared(&copy.path.join("alpha/src/a.ts"));
    append_shared(&copy.path.join("beta/src/b.ts"));
    git_init(&copy.path.join("alpha"));
    git_init(&copy.path.join("beta"));
    let output = sieve(&copy.path, &["build"]);
    assert_eq!(output.status.code(), Some(0), "workspace build");
    copy
}

/// P1-54: text `map` at the parent prints the workspace head, then one
/// `## <child>/` section per child, each with its own savings header.
#[test]
fn test_p1_54_map_at_a_workspace_parent_prints_one_section_per_child() {
    let copy = built("ws3-map");
    let output = sieve(&copy.path, &["map"]);
    assert_matches("map", &output, &copy);
}

/// P1-54: `--max-dirs` splits evenly across the children.
#[test]
fn test_p1_54_map_max_dirs_splits_across_the_children() {
    let copy = built("ws3-map-max-dirs");
    let output = sieve(&copy.path, &["map", "--max-dirs", "2"]);
    assert_matches("map-max-dirs", &output, &copy);
}

/// P1-58: `map --json` at the parent does not federate; it prints the
/// plain no-graph line and exits 1.
#[test]
fn test_p1_58_map_json_at_a_workspace_parent_has_no_graph() {
    let copy = built("ws3-map-json");
    let output = sieve(&copy.path, &["map", "--json"]);
    assert_matches("map-json", &output, &copy);
}

/// P1-54: an unbuilt child drops out of the map, and the coverage note
/// closes the output.
#[test]
fn test_p1_54_map_with_one_child_unbuilt_adds_the_coverage_note() {
    let copy = built("ws3-map-unbuilt");
    fs::remove_dir_all(copy.path.join("beta/sieve")).expect("remove beta graph");
    let output = sieve(&copy.path, &["map"]);
    assert_matches("map-unbuilt", &output, &copy);
}

/// P1-58: `callers` at the parent prints one `## <child>/` block per
/// child with a hit, child-relative paths, no call-site quote, and one
/// savings line per block.
#[test]
fn test_p1_58_callers_with_hits_in_both_children() {
    let copy = built("ws3-callers-both");
    let output = sieve(&copy.path, &["callers", "shared"]);
    assert_matches("callers-both", &output, &copy);
}

/// P1-58: a child with no match prints no block.
#[test]
fn test_p1_58_callers_with_a_hit_in_one_child_only() {
    let copy = built("ws3-callers-one");
    let output = sieve(&copy.path, &["callers", "alphaAdd"]);
    assert_matches("callers-one", &output, &copy);
}

/// P1-58: a miss in every child prints the workspace miss line and exits
/// 1.
#[test]
fn test_p1_58_callers_miss_in_every_child_exits_1() {
    let copy = built("ws3-callers-miss");
    let output = sieve(&copy.path, &["callers", "zzz"]);
    assert_matches("callers-miss", &output, &copy);
}

/// P1-58: `callers --json` at the parent does not federate; it prints the
/// named no-graph line and exits 1.
#[test]
fn test_p1_58_callers_json_at_a_workspace_parent_has_no_graph() {
    let copy = built("ws3-callers-json");
    let output = sieve(&copy.path, &["callers", "shared", "--json"]);
    assert_matches("callers-json", &output, &copy);
}

/// P1-58: `--depth 2` tags each hit with its depth, in every block.
#[test]
fn test_p1_58_callers_depth_2_tags_each_hit_in_every_child() {
    let copy = built("ws3-callers-depth");
    let output = sieve(&copy.path, &["callers", "shared", "--depth", "2"]);
    assert_matches("callers-depth", &output, &copy);
}
