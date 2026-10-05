//! Parity tests for workspace step 5: the federated `ask` at a workspace parent
//! (P1-56) and the federated MCP `find_code` (P4-44), against the goldens under
//! `tests/fixtures/ws.expected/workspace5/`. The copies come from
//! `tests/fixtures/ws-ask/`; `tests/inputs/ws-ask/` holds the variant table and
//! the case list. The helpers live in `support/ws.rs`.

mod support;

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::{Output, Stdio};

use serde_json::Value;
use support::ws::{assert_matches, copy_dir, git_init, golden};
use support::TempDir;

const GOLDEN: &str = "workspace5";

/// The tab-separated rows of `tests/inputs/ws-ask/<name>`, less comments.
fn table(name: &str) -> Vec<Vec<String>> {
    let path = support::manifest_dir()
        .join("../../tests/inputs/ws-ask")
        .join(name);
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.split('\t').map(str::to_string).collect())
        .collect()
}

/// A comma list from a table field; `-` is the empty list.
fn list(field: &str) -> Vec<String> {
    field
        .split(',')
        .filter(|s| !s.is_empty() && *s != "-")
        .map(str::to_string)
        .collect()
}

/// A built copy of the `ws-ask` fixture
/// builds it: the overlays, a git repo per child, one `sieve build` at
/// the parent, the post-build files, then the unbuilt child's graph
/// removed.
fn built(label: &str, variant: &str) -> TempDir {
    let rows = table("variants.txt");
    let row = rows
        .iter()
        .find(|r| r[0] == variant)
        .unwrap_or_else(|| panic!("no variant {variant}"));
    let fixtures = support::manifest_dir().join("../../tests/fixtures");
    let copy = TempDir::new(label);
    copy_dir(&fixtures.join("ws-ask"), &copy.path);
    for name in list(&row[1]) {
        copy_dir(&fixtures.join("ws-ask.overlays").join(name), &copy.path);
    }
    for child in ["alpha", "beta", "gamma"] {
        if copy.path.join(child).is_dir() {
            git_init(&copy.path.join(child));
        }
    }
    let build = support::ws::sieve(&copy.path, &["build"]);
    assert_eq!(build.status.code(), Some(0), "workspace build");
    for name in list(&row[2]) {
        copy_dir(&fixtures.join("ws-ask.post").join(name), &copy.path);
    }
    if row[3] != "-" {
        fs::remove_dir_all(copy.path.join(&row[3]).join("sieve")).expect("remove graph");
    }
    copy
}

/// Splits a case's argument string on spaces; a double-quoted run is one
/// argument, and `""` is the empty argument.
fn split_args(args: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut started = false;
    for c in args.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            ' ' if !quoted => {
                if started {
                    out.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            _ => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        out.push(current);
    }
    out
}

/// Runs the case `id` from `cases.txt` in a fresh copy and returns both.
fn run_case(id: &str) -> (TempDir, Output) {
    let rows = table("cases.txt");
    let row = rows
        .iter()
        .find(|r| r[0] == id)
        .unwrap_or_else(|| panic!("no case {id}"));
    let copy = built(&format!("ws5-{id}"), &row[1]);
    let args = split_args(&row[2]);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = support::ws::sieve(&copy.path, &args);
    (copy, output)
}

/// Asserts the case `id` matches its golden: stdout, stderr and the
/// exit code. Returns the stdout.
fn assert_case(id: &str) -> String {
    let (copy, output) = run_case(id);
    assert_matches(GOLDEN, id, &output, &copy.path);
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// P1-56: `ask` at the parent runs every child and fuses by reciprocal
/// rank. The root scope of gamma labels as `[gamma/]`, a package scope as
/// `[gamma/packages/x/]`, and the body-only child beta is only mentioned.
#[test]
fn test_p1_56_default_text_federates_the_children() {
    let stdout = assert_case("default");
    assert!(stdout.contains("[gamma/] parseConfigRoot"));
    assert!(stdout.contains("[gamma/packages/x/] parseConfigX"));
    assert!(stdout.contains("matched in: alpha/ (3) · gamma/packages/x/ (2)"));
}

/// P1-56: `-n 3` cuts the round robin at three hits, and the nudge shows.
#[test]
fn test_p1_56_limit_three_cuts_the_merged_list() {
    let stdout = assert_case("n3");
    assert!(stdout.contains("only 3 hits"));
}

/// P1-56: `-n 0` gives mode `empty` and the no-match note across the
/// built children.
#[test]
fn test_p1_56_limit_zero_gives_empty_mode_and_the_note() {
    let stdout = assert_case("n0");
    assert!(stdout.contains("(empty)"));
    assert!(stdout.contains("no matching nodes across 3 workspace repo(s)"));
}

/// P1-56: the `--json` result keeps the key order, the fused scores
/// and the `scopes` footer; a hit with no child scope gets `scope` last.
#[test]
fn test_p1_56_json_key_order_and_scores() {
    assert_case("json");
}

/// P1-56: with `--source` the key `scope` of a hit with no child scope
/// comes after `code`; a hit with a child scope keeps it before `code`.
#[test]
fn test_p1_56_json_source_scope_after_code_for_a_child_hit() {
    let stdout = assert_case("json-source");
    // Alpha has one scope, so its hit has no child scope: `scope` is last.
    assert!(stdout.contains("\",\n      \"scope\": \"alpha\"\n"));
    // Gamma's package scope came from the child, so it stays before `code`.
    assert!(stdout.contains("\"scope\": \"gamma/packages/x\",\n      \"code\": "));
}

/// P1-56: `--source` inlines the code per hit, with no savings line.
#[test]
fn test_p1_56_source_text_inlines_code_per_child() {
    let stdout = assert_case("source");
    assert!(stdout.contains("```"));
    assert!(!stdout.contains("tokens saved"));
}

/// P1-56: `--source --full` inlines the whole definition span. The fixture
/// has no crux, so `--full` changes no byte; the case pins the bytes only.
#[test]
fn test_p1_56_source_full_inlines_whole_spans() {
    assert_case("source-full");
}

/// P1-56: `--no-graph-rank` never reaches a child, so the bytes equal the
/// plain run.
#[test]
fn test_p1_56_no_graph_rank_gives_the_same_bytes() {
    let stdout = assert_case("no-graph-rank");
    assert_eq!(stdout, golden(GOLDEN, "default.stdout.txt"));
}

/// P1-56: `--in beta` skips the strength gate, so the body-only child
/// answers.
#[test]
fn test_p1_56_in_child_skips_the_gate() {
    let stdout = assert_case("in-beta");
    assert!(stdout.contains("[beta/] render"));
}

/// P1-56: `--in gamma` keeps the root-scope hit under the bare child
/// scope, and the JSON lists the scope `gamma` with no slash.
#[test]
fn test_p1_56_root_scope_hit_uses_the_bare_child_scope() {
    let stdout = assert_case("in-gamma");
    assert!(stdout.contains("[gamma/] parseConfigRoot"));
    let json = assert_case("in-gamma-json");
    assert!(json.contains("\"gamma\","));
}

/// P1-56: `--in gamma/packages/x` hands the rest of the path to the child
/// as its own `--in`.
#[test]
fn test_p1_56_in_child_sub_scope() {
    assert_case("in-gamma-x");
}

/// P1-56: a sub-prefix the child does not index throws in the child, and
/// the child is skipped: the result is empty.
#[test]
fn test_p1_56_in_bad_sub_prefix_skips_the_child() {
    let stdout = assert_case("in-alpha-nosuch");
    assert!(stdout.contains("(empty)"));
}

/// P1-56: an unknown first `--in` segment prints the plain message on
/// stderr and exits 1.
#[test]
fn test_p1_56_in_unknown_child_exits_one() {
    let (copy, output) = run_case("in-zzz");
    assert_matches(GOLDEN, "in-zzz", &output, &copy.path);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no workspace repo \"zzz\" - repos: alpha, beta, gamma"));
    assert!(!stderr.contains('✗'));
}

/// P1-56: `--in ""` means no scope: the bytes equal the plain run.
#[test]
fn test_p1_56_in_empty_string_means_no_scope() {
    let stdout = assert_case("in-empty");
    assert_eq!(stdout, golden(GOLDEN, "default.stdout.txt"));
}

/// P1-56: a body-only child is gated out of the fusion and shows as
/// `alsoMatched` with its best pointer prefixed.
#[test]
fn test_p1_56_body_only_child_is_also_matched() {
    let stdout = assert_case("alsomatched-json");
    assert!(stdout.contains("\"bestId\": \"beta/src/b.ts:L1-L4\""));
}

/// P1-56: a structural child has no ranking, so its coverage reads 0 and
/// the gate drops it unless `--in` names it.
#[test]
fn test_p1_56_structural_child_is_gated_out_without_in() {
    let stdout = assert_case("structural");
    assert!(stdout.contains("also matched: alpha/"));
    assert!(!stdout.contains("[caller]"));
}

/// P1-56: `--in alpha` keeps the structural hits: mode lexical, no
/// subject, labeled `[alpha/]`.
#[test]
fn test_p1_56_structural_in_child_keeps_the_hits() {
    let stdout = assert_case("structural-in-alpha");
    assert!(stdout.contains("[alpha/] loadConfig  [caller]"));
    let json = assert_case("structural-in-alpha-json");
    assert!(!json.contains("subject"));
}

/// P1-56: a concept pointer gets the child prefix on each path.
#[test]
fn test_p1_56_concept_pointer_gets_the_child_prefix() {
    let stdout = assert_case("concept");
    assert!(stdout.contains("   alpha/src/a.ts, alpha/src/b.ts\n"));
    assert_case("concept-json");
}

/// P1-56: a concept that tops a child's baseline takes the lock, and its
/// group stays a singleton.
#[test]
fn test_p1_56_concept_at_a_child_top_locks_first() {
    let stdout = assert_case("concept-gamma");
    assert!(stdout.contains("1. [gamma/] Config pipeline  [concept]"));
    assert_case("concept-gamma-json");
}

/// P1-56: an unbuilt child adds the coverage line after the header, and
/// the hits come from the built children.
#[test]
fn test_p1_56_unbuilt_child_adds_the_coverage_note() {
    let stdout = assert_case("unbuilt");
    assert!(stdout.contains("2 of 3 workspace repos have graphs; run sieve build to cover beta"));
}

/// P1-56: `--in <unbuilt child>` passes the name check, then gives the
/// empty note and the coverage line.
#[test]
fn test_p1_56_in_missing_child_gives_note_and_coverage() {
    let stdout = assert_case("unbuilt-in-beta");
    assert!(stdout.contains("(empty)"));
    assert!(stdout.contains("2 of 3 workspace repos have graphs"));
}

/// P1-56: eleven baseline entries tie in score. The baseline stream keeps
/// the unpadded index in its doc id, so `gamma 10` sorts before `gamma 9`
/// and the lock takes `parseConfigRb`, not `parseConfigRa`.
#[test]
fn test_p1_56_baseline_ties_order_by_unpadded_id() {
    let stdout = assert_case("ties");
    assert!(stdout.contains("1. [gamma/] parseConfigRb · function"));
}

/// P1-56: a package scope below a quarter of the best leader across the
/// children is gated out of the fusion. Its `bestId` is the file doc id,
/// `gamma file 00000002`, padded to eight digits.
#[test]
fn test_p1_56_weak_sub_scope_is_also_matched() {
    let stdout = assert_case("weaky");
    assert!(stdout.contains("also matched: gamma/packages/y/ — narrow with --in gamma/packages/y/"));
    let json = assert_case("weaky-json");
    assert!(json.contains("\"bestId\": \"gamma file 00000002\""));
}

/// P1-56: a child with 24 file groups under `-n 1` keeps the first 20
/// (`max(4n, 20)`). The package scope `y` leads group 7 and stays in the
/// footer. The package scope `z` leads group 24 and drops out.
#[test]
fn test_p1_56_file_slice_is_max_of_four_n_and_twenty() {
    let stdout = assert_case("many-json");
    assert!(stdout.contains("\"gamma/packages/y\""));
    assert!(!stdout.contains("\"gamma/packages/z\""));
}

/// P1-56: the baseline top of beta is a file node, whose strong share is 0,
/// and its coverage is under one half, so the baseline gate fails. The file
/// gate reads the union of the donor symbols' name terms
/// (`unionStrongCoverage`), so beta still joins the fusion. With no baseline
/// survivor there is no lock.
#[test]
fn test_p1_56_file_gate_reads_the_union_strong_coverage() {
    let stdout = assert_case("filegate");
    assert!(stdout.contains("1. [beta/] zeta-omega-sigma-rho.ts · file"));
    let json = assert_case("filegate-json");
    assert!(json.contains("\"federated\": [\n      \"beta\"\n    ]"));
}

/// P1-56: the lock tail takes the file-stream score of its group leader.
/// Rb sits in the second root file, so its group ranks second in the file
/// stream (61/62), while the lock itself carries the baseline score 1. The
/// tail hit `parseTail` shows the 61/62 score in the JSON.
#[test]
fn test_p1_56_lock_tail_takes_the_group_leader_score() {
    let json = assert_case("ties-json");
    assert!(json.contains("\"title\": \"parseTail · function\""));
    assert!(json.contains("\"score\": 0.9838709677419354"));
}

/// P1-56: the strong terms of a file group fold plurals, so `zetas` and
/// `omegas` match the name tokens `zeta` and `omega`, and beta joins.
#[test]
fn test_p1_56_file_gate_strong_terms_fold_plurals() {
    let stdout = assert_case("filegate-plural");
    assert!(stdout.contains("[beta/] zeta-omega-sigma-rho.ts"));
}

/// P1-56: with a crux in the graph, `--source` inlines the crux excerpt and
/// `--source --full` inlines the whole span, so `--full` reaches each child.
#[test]
fn test_p1_56_full_reaches_the_children() {
    let short = assert_case("crux-source");
    assert!(short.contains("rerun with --full"));
    let full = assert_case("crux-full");
    assert!(!full.contains("rerun with --full"));
}

/// P4-44: a federated `find_code` that throws drops the refresh note,
/// because the call path catches the throw before it adds the note.
#[test]
fn test_p4_44_find_code_error_drops_the_refresh_note() {
    let copy = built("ws5-mcp-throw", "edited");
    assert_session_matches("findcode-throw", &copy.path);
}

/// P4-44: a federated `find_code` that answers keeps the refresh note.
#[test]
fn test_p4_44_find_code_keeps_the_refresh_note() {
    let copy = built("ws5-mcp-edited", "edited");
    assert_session_matches("findcode-edited", &copy.path);
}

/// Every reply of an MCP session keyed by its `id`.
fn index_replies(stdout: &str) -> HashMap<String, Value> {
    let mut by_id = HashMap::new();
    for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
        let value: Value =
            serde_json::from_str(line).unwrap_or_else(|e| panic!("reply is not JSON: {line}: {e}"));
        let id = value
            .get("id")
            .unwrap_or_else(|| panic!("reply with no id: {line}"))
            .to_string();
        assert!(
            by_id.insert(id.clone(), value).is_none(),
            "two replies for id {id}"
        );
    }
    by_id
}

/// Runs `sieve mcp` at `root` over `tests/inputs/mcp-ws/<id>.ndjson`, and
/// asserts it matches the golden `mcp-<id>.*`: the same reply ids, and
/// every reply equal to the golden. Sieve answers in completion order, so the
/// line order is not part of the oracle.
fn assert_session_matches(id: &str, root: &Path) {
    let request = support::manifest_dir().join(format!("../../tests/inputs/mcp-ws/{id}.ndjson"));
    let output = support::sieve_command()
        .arg("mcp")
        .current_dir(root)
        .stdin(Stdio::from(fs::File::open(request).expect("open requests")))
        .output()
        .expect("run sieve mcp");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let exit = format!("{}\n", output.status.code().unwrap_or(-1));
    let stderr = support::ws::drop_update_nudge(&support::ws::mask(&stderr, root));
    support::ws::bless(GOLDEN, &format!("mcp-{id}.exit.txt"), &exit);
    support::ws::bless(GOLDEN, &format!("mcp-{id}.stderr.txt"), &stderr);
    support::ws::bless(GOLDEN, &format!("mcp-{id}.stdout.txt"), &stdout);
    assert_eq!(
        exit,
        golden(GOLDEN, &format!("mcp-{id}.exit.txt")),
        "{id}: exit code"
    );
    assert_eq!(
        stderr,
        golden(GOLDEN, &format!("mcp-{id}.stderr.txt")),
        "{id}: stderr"
    );
    let got = index_replies(&stdout);
    let want = index_replies(&golden(GOLDEN, &format!("mcp-{id}.stdout.txt")));
    let mut got_ids: Vec<&String> = got.keys().collect();
    let mut want_ids: Vec<&String> = want.keys().collect();
    got_ids.sort();
    want_ids.sort();
    assert_eq!(got_ids, want_ids, "{id}: reply id set");
    for (reply_id, want_reply) in &want {
        assert_eq!(&got[reply_id], want_reply, "{id}: reply {reply_id}");
    }
}

/// P4-44: `find_code` at the parent runs the federated `ask` with source
/// inlined and no savings line, and an unknown `in` child is an error
/// reply with the plain message.
#[test]
fn test_p4_44_find_code_federates_at_a_workspace_parent() {
    let copy = built("ws5-mcp", "base");
    assert_session_matches("findcode-federated", &copy.path);
}

/// P4-44: with a child unbuilt, `find_code` adds the coverage note.
#[test]
fn test_p4_44_find_code_adds_the_coverage_note_with_a_child_unbuilt() {
    let copy = built("ws5-mcp-unbuilt", "unbuilt");
    assert_session_matches("findcode-unbuilt", &copy.path);
}
