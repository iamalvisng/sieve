//! Parity tests for the deep tier (P1-33 to P1-36, P2-31, P2-37, P3-01 to
//! P3-09, P3-14, P3-15): a stored `sieve build --deep` layer, replayed by
//! a plain `sieve build` with no LLM call.
//!
//! Setup for every test: copy `tests/fixtures/deep/` into a fresh temp dir,
//! restore the stored deep layer into it the way a user's checkout would hold
//! it (the `deep-tier.md` note section 4.1), then run a plain `sieve build`.
//! Sieve never calls an LLM; every Tier-2 field it prints comes from the
//! restored `wiring.json`, `manifest.json` and concept `.md` files.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use support::TempDir;

/// Copies `src` into `dst`, recursively, including dot-directories.
fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("create dst dir");
    for entry in fs::read_dir(src).expect("read src dir") {
        let entry = entry.expect("read dir entry");
        let file_type = entry.file_type().expect("read file type");
        let target = dst.join(entry.file_name());
        if file_type.is_dir() {
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

/// Drops every line starting with `⬆` (the update-nudge line, undetermined
/// by the fixture), then rejoins with one trailing newline when anything
/// remains.
fn normalize_stderr(s: &str) -> String {
    let kept: Vec<&str> = s.lines().filter(|l| !l.starts_with('⬆')).collect();
    if kept.is_empty() {
        String::new()
    } else {
        format!("{}\n", kept.join("\n"))
    }
}

fn run_sieve(root: &Path, args: &[String]) -> Output {
    support::sieve_command()
        .args(args)
        .current_dir(root)
        .output()
        .expect("run sieve")
}

/// Prints the query id, the field, and the first differing line. Returns
/// whether `expected` and `actual` matched.
fn print_diff(id: &str, field: &str, expected: &str, actual: &str) -> bool {
    if expected == actual {
        return true;
    }
    let exp_lines: Vec<&str> = expected.lines().collect();
    let act_lines: Vec<&str> = actual.lines().collect();
    let max = exp_lines.len().max(act_lines.len());
    for i in 0..max {
        let e = exp_lines.get(i).copied().unwrap_or("<missing>");
        let a = act_lines.get(i).copied().unwrap_or("<missing>");
        if e != a {
            eprintln!("mismatch: query {id}, field {field}, first differing line {i}:");
            eprintln!("  -{e}");
            eprintln!("  +{a}");
            return false;
        }
    }
    eprintln!("mismatch: query {id}, field {field}: length mismatch");
    false
}

fn fixture_dir(name: &str) -> PathBuf {
    support::manifest_dir().join(format!("../../tests/fixtures/{name}"))
}

fn golden_dir() -> PathBuf {
    fixture_dir("deep.expected")
}

/// Restores the stored deep layer into `temp_root`, the way a user's
/// checkout would hold it (`notes/deep-tier.md` section 4.1): every
/// concept `.md` file, `manifest.json`, and `wiring.json`. Sieve
/// regenerates every card and `INDEX.md` itself, so those never restore.
fn restore_deep_layer(temp_root: &Path) {
    let golden = golden_dir();
    let sieve_dir = temp_root.join("sieve");
    fs::create_dir_all(&sieve_dir).expect("create sieve dir");

    for entry in fs::read_dir(golden.join("sieve")).expect("read golden sieve dir") {
        let entry = entry.expect("read golden sieve dir entry");
        let path = entry.path();
        let name = entry.file_name();
        let is_md = path.extension().and_then(|e| e.to_str()) == Some("md");
        if path.is_file() && is_md && name != "INDEX.md" {
            fs::copy(&path, sieve_dir.join(&name)).expect("restore concept md");
        }
    }

    fs::copy(
        golden.join("manifest.json"),
        sieve_dir.join("manifest.json"),
    )
    .expect("restore manifest.json");

    let graph_dir = sieve_dir.join(".graph");
    fs::create_dir_all(&graph_dir).expect("create .graph dir");
    fs::copy(golden.join("wiring.json"), graph_dir.join("wiring.json"))
        .expect("restore wiring.json");
}

/// Copies `tests/fixtures/deep/` into a fresh temp dir, restores the
/// stored deep layer, and runs one plain `sieve build`. Returns the temp
/// dir on success.
fn setup_deep_temp(label: &str) -> TempDir {
    let temp = TempDir::new(label);
    copy_dir(&fixture_dir("deep"), &temp.path);
    restore_deep_layer(&temp.path);

    let output = run_sieve(&temp.path, &["build".to_string(), ".".to_string()]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "sieve build failed over the restored deep layer: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    temp
}

/// Reads one query id's argument line, either from
/// `tests/inputs/queries/deep.txt`, or from the fixed `check`/`check
/// --json` pair the stale-state capture runs directly
/// (not a `deep.txt` line).
fn deep_query_args(id: &str) -> Vec<String> {
    if id == "check-stale" {
        return vec!["check".to_string()];
    }
    if id == "check-stale-json" {
        return vec!["check".to_string(), "--json".to_string()];
    }
    let query_list = support::manifest_dir().join("../../tests/inputs/queries/deep.txt");
    let text = fs::read_to_string(&query_list).expect("read deep.txt query list");
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((line_id, args)) = line.split_once('\t') else {
            continue;
        };
        if line_id == id {
            return split_args(args);
        }
    }
    panic!("no query id {id} in tests/inputs/queries/deep.txt");
}

/// Runs one query id from `deep.txt` against `root`, and asserts its
/// stdout, `⬆`-normalized stderr and exit code equal the golden files
/// under `tests/fixtures/deep.expected/queries/`.
fn assert_query_matches_golden(root: &Path, id: &str) {
    let golden = golden_dir().join("queries");
    let argv = deep_query_args(id);
    let output = run_sieve(root, &argv);
    support::golden::bless_triple(
        &golden,
        id,
        &output.stdout,
        normalize_stderr(&String::from_utf8_lossy(&output.stderr)).as_bytes(),
        output.status.code().unwrap_or(-1),
    );

    let expected_stdout =
        fs::read_to_string(golden.join(format!("{id}.stdout.txt"))).expect("read expected stdout");
    let expected_stderr =
        fs::read_to_string(golden.join(format!("{id}.stderr.txt"))).expect("read expected stderr");
    let expected_exit = fs::read_to_string(golden.join(format!("{id}.exit.txt")))
        .expect("read expected exit")
        .trim()
        .parse::<i32>()
        .expect("parse expected exit");

    let actual_stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let actual_stderr = normalize_stderr(&String::from_utf8_lossy(&output.stderr));
    let expected_stderr = normalize_stderr(&expected_stderr);
    let actual_exit = output.status.code().unwrap_or(-1);

    let stdout_ok = print_diff(id, "stdout", &expected_stdout, &actual_stdout);
    let stderr_ok = print_diff(id, "stderr", &expected_stderr, &actual_stderr);
    assert!(stdout_ok, "query {id}: stdout mismatch");
    assert!(stderr_ok, "query {id}: stderr mismatch");
    assert_eq!(actual_exit, expected_exit, "query {id}: exit mismatch");
}

/// Recursively collects every `.md` path, relative to `dir`, under `dir`.
fn collect_md(dir: &Path, prefix: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        fs::read_dir(dir).unwrap_or_else(|err| panic!("read dir {}: {err}", dir.display()));
    for entry in entries {
        let entry = entry.expect("read dir entry");
        let path = entry.path();
        let rel = prefix.join(entry.file_name());
        if path.is_dir() {
            collect_md(&path, &rel, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
            out.push(rel);
        }
    }
}

/// P1-34: the wiring section of the replayed graph is byte-for-byte the
/// same as the golden — 23 ready nodes, 2 pending, the
/// cruxes it stored survive a plain Tier-1 build with no LLM call.
#[test]
fn test_p1_34_build_carries_the_stored_meaning_tier() {
    let temp = setup_deep_temp("wiring");

    let actual = fs::read(temp.path.join("sieve/.graph/wiring.json")).expect("read wiring.json");
    support::golden::bless_if_blessing(&golden_dir().join("wiring.json"), &actual);
    let expected = fs::read(golden_dir().join("wiring.json")).expect("read golden wiring.json");
    if actual != expected {
        print_diff(
            "wiring.json",
            "bytes",
            &String::from_utf8_lossy(&expected),
            &String::from_utf8_lossy(&actual),
        );
    }
    assert_eq!(
        actual, expected,
        "wiring.json did not carry the meaning tier forward"
    );
}

/// P2-31, P2-37: every card under `sieve/`, including the `_root/`
/// collision card and the seven concept files with their `covers:`
/// blocks, matches the golden byte for byte, and Sieve writes no
/// extra file.
#[test]
fn test_p2_31_p2_37_cards_index_and_covers_match_golden() {
    let temp = setup_deep_temp("cards");
    let golden = golden_dir();
    if support::golden::blessing() {
        let mut cards = Vec::new();
        collect_md(&temp.path.join("sieve"), Path::new(""), &mut cards);
        for card in &cards {
            support::golden::bless(
                &golden.join("sieve").join(card),
                &fs::read(temp.path.join("sieve").join(card)).expect("read actual card"),
            );
        }
    }

    let mut expected_cards = Vec::new();
    collect_md(&golden.join("sieve"), Path::new(""), &mut expected_cards);
    let mut actual_cards = Vec::new();
    collect_md(&temp.path.join("sieve"), Path::new(""), &mut actual_cards);
    expected_cards.sort();
    actual_cards.sort();
    assert!(!expected_cards.is_empty(), "golden card list is empty");
    assert_eq!(actual_cards, expected_cards, "card file set mismatch");

    for card in &expected_cards {
        let expected_text =
            fs::read_to_string(golden.join("sieve").join(card)).expect("read expected card");
        let actual_text =
            fs::read_to_string(temp.path.join("sieve").join(card)).expect("read actual card");
        if actual_text != expected_text {
            print_diff(
                &card.display().to_string(),
                "bytes",
                &expected_text,
                &actual_text,
            );
        }
        assert_eq!(
            actual_text,
            expected_text,
            "card mismatch: {}",
            card.display()
        );
    }

    // No extra top-level entry under `sieve/`: only `.graph`, `.cache`,
    // `manifest.json`, and whatever the golden's own top level holds.
    let mut expected_top: std::collections::BTreeSet<std::ffi::OsString> =
        fs::read_dir(golden.join("sieve"))
            .expect("read golden sieve dir")
            .map(|e| e.expect("read entry").file_name())
            .collect();
    expected_top.insert(".graph".into());
    expected_top.insert(".cache".into());
    expected_top.insert("manifest.json".into());

    let actual_top: std::collections::BTreeSet<std::ffi::OsString> =
        fs::read_dir(temp.path.join("sieve"))
            .expect("read actual sieve dir")
            .map(|e| e.expect("read entry").file_name())
            .collect();
    assert_eq!(actual_top, expected_top, "unexpected entry under sieve/");
}

/// P1-33, P1-35, P1-36: `sieve check` over a present, complete deep layer
/// matches the `check-deep` golden — stdout, `⬆`-normalized stderr
/// and exit code.
#[test]
fn test_p1_33_p1_35_p1_36_check_matches_golden_on_a_present_deep_layer() {
    let temp = setup_deep_temp("check-deep");
    assert_query_matches_golden(&temp.path, "check-deep");
}

/// P1-34: a Tier-1 rebuild over one changed source file moves that
/// node's `summary_state` to `stale`, and both `check` and `check --json`
/// report it the way the `check-stale` goldens do.
#[test]
fn test_p1_34_check_reports_stale_summaries_like_golden() {
    let temp = setup_deep_temp("check-stale");

    let util_path = temp.path.join("src/util.ts");
    let mut src = fs::read_to_string(&util_path).expect("read util.ts");
    src.push_str("\nexport const staleMarker = 1;\n");
    fs::write(&util_path, src).expect("write util.ts");

    let output = run_sieve(&temp.path, &["build".to_string(), ".".to_string()]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "second build over the touched file failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert_query_matches_golden(&temp.path, "check-stale");
    assert_query_matches_golden(&temp.path, "check-stale-json");
}

/// P3-01 to P3-09: `ask` ranks concept documents against a symbol hit the
/// same way the `ask-concept` golden does.
#[test]
fn test_p3_01_to_p3_09_ask_ranks_concepts_like_golden() {
    let temp = setup_deep_temp("ask-concept");
    assert_query_matches_golden(&temp.path, "ask-concept");
}

/// P3-14, P3-15: `ask --source` replays the stored crux instead of
/// re-slicing the file, and `--full` still bypasses the crux map. P1-02:
/// the two goldens differ, so `--full` has an effect this test can
/// break; `src/app.ts:L4-L14` is longer than its crux and under 80 lines.
#[test]
fn test_p1_02_p3_14_p3_15_ask_source_replays_the_crux_and_full_bypasses_it() {
    let crux = fs::read_to_string(golden_dir().join("queries/ask-source-crux.stdout.txt"))
        .expect("read the crux golden");
    let full = fs::read_to_string(golden_dir().join("queries/ask-source-full.stdout.txt"))
        .expect("read the full golden");
    assert_ne!(crux, full, "the crux and full goldens must differ");
    assert!(
        full.contains("export function quadruple"),
        "the full golden inlines the span"
    );
    let temp = setup_deep_temp("ask-source");
    assert_query_matches_golden(&temp.path, "ask-source-crux");
    assert_query_matches_golden(&temp.path, "ask-source-full");
}

/// P2-10: `sieve skeleton` prints the stored summary line, matching
/// The `skeleton-deep` golden.
#[test]
fn test_p1_18_p2_10_skeleton_prints_the_summary_line() {
    let temp = setup_deep_temp("skeleton");
    assert_query_matches_golden(&temp.path, "skeleton-deep");
}

/// `map` and `grep` read no Tier-2 field, so their output over a deep
/// graph is unchanged from a Tier-1 graph
/// (`notes/deep-tier.md` section 2.11).
#[test]
fn test_deep_map_and_grep_are_unchanged() {
    let temp = setup_deep_temp("map-grep");
    assert_query_matches_golden(&temp.path, "map-deep");
    assert_query_matches_golden(&temp.path, "grep-deep");
}
