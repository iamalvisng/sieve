//! Parity tests for `sieve skeleton`, `sieve callers`, `sieve map` and
//! `sieve check` (P1-17 to P1-36, P3-20 to P3-33): each command's report,
//! against the goldens under `tests/fixtures/<name>.expected/queries/`.

mod support;

use std::fs;
use std::path::Path;
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

/// Prints a minimal line diff between `expected` and `actual`, labeled
/// with `id` and `field`.
fn print_diff(id: &str, field: &str, expected: &str, actual: &str) {
    eprintln!("query {id}, field {field}, mismatch:");
    let exp_lines: Vec<&str> = expected.lines().collect();
    let act_lines: Vec<&str> = actual.lines().collect();
    let max = exp_lines.len().max(act_lines.len());
    for i in 0..max {
        let e = exp_lines.get(i).copied().unwrap_or("<missing>");
        let a = act_lines.get(i).copied().unwrap_or("<missing>");
        if e != a {
            eprintln!("  -{e}");
            eprintln!("  +{a}");
        }
    }
}

/// Builds one fixture into a fresh temp copy and returns the copy's root.
fn build_fixture(name: &str) -> TempDir {
    let fixture = support::manifest_dir().join(format!("../../tests/fixtures/{name}"));
    let temp = TempDir::new(name);
    copy_dir(&fixture, &temp.path);
    let output = run_sieve(&temp.path, &["build".to_string(), ".".to_string()]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "sieve build failed for {name}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    temp
}

/// True when `id` belongs to one of the four commands this test pins.
fn is_pinned_id(id: &str) -> bool {
    id.starts_with("skeleton-")
        || id.starts_with("callers-")
        || id.starts_with("map")
        || id.starts_with("check")
}

/// Masks every spelling of `root` in `text` with `<TMP>`, the way
/// the same mask the goldens use: the realpath form first, then the
/// literal form.
fn mask_root(text: &str, root: &Path) -> String {
    let canon = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    text.replace(&canon.display().to_string(), "<TMP>")
        .replace(&root.display().to_string(), "<TMP>")
}

/// Runs `argv` in `root` and asserts the run matches the golden `id`
/// under `expected_dir`: stdout, stderr (after dropping `⬆` lines and
/// masking `root`), and the exit code.
fn assert_run_matches_golden(root: &Path, expected_dir: &Path, id: &str, argv: &[String]) {
    let output = run_sieve(root, argv);

    support::golden::bless_triple(
        expected_dir,
        id,
        mask_root(&String::from_utf8_lossy(&output.stdout), root).as_bytes(),
        normalize_stderr(&mask_root(&String::from_utf8_lossy(&output.stderr), root)).as_bytes(),
        output.status.code().unwrap_or(-1),
    );
    let expected_stdout = fs::read_to_string(expected_dir.join(format!("{id}.stdout.txt")))
        .expect("read expected stdout");
    let expected_stderr = fs::read_to_string(expected_dir.join(format!("{id}.stderr.txt")))
        .expect("read expected stderr");
    let expected_exit = fs::read_to_string(expected_dir.join(format!("{id}.exit.txt")))
        .expect("read expected exit")
        .trim()
        .parse::<i32>()
        .expect("parse expected exit");

    let actual_stdout = mask_root(&String::from_utf8_lossy(&output.stdout), root);
    let actual_stderr = mask_root(&String::from_utf8_lossy(&output.stderr), root);
    let actual_exit = output.status.code().unwrap_or(-1);

    if actual_stdout != expected_stdout {
        print_diff(id, "stdout", &expected_stdout, &actual_stdout);
    }
    if normalize_stderr(&expected_stderr) != normalize_stderr(&actual_stderr) {
        print_diff(
            id,
            "stderr",
            &normalize_stderr(&expected_stderr),
            &normalize_stderr(&actual_stderr),
        );
    }
    assert_eq!(
        actual_stdout, expected_stdout,
        "query {id}: stdout mismatch"
    );
    assert_eq!(
        normalize_stderr(&actual_stderr),
        normalize_stderr(&expected_stderr),
        "query {id}: stderr mismatch"
    );
    assert_eq!(actual_exit, expected_exit, "query {id}: exit mismatch");
}

/// Reads `tests/inputs/queries/<name>.txt` into `(id, argv)` pairs,
/// skipping comments and blank lines.
fn read_query_list(name: &str) -> Vec<(String, Vec<String>)> {
    let query_list = support::manifest_dir().join(format!("../../tests/inputs/queries/{name}.txt"));
    let lines = fs::read_to_string(&query_list).expect("read query list");
    lines
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split_once('\t'))
        .map(|(id, args)| (id.to_string(), split_args(args)))
        .collect()
}

fn expected_dir(name: &str) -> std::path::PathBuf {
    support::manifest_dir().join(format!("../../tests/fixtures/{name}.expected/queries"))
}

/// Runs every pinned query id for one fixture and asserts it matches the
/// golden stdout, stderr (after dropping `⬆` lines), and exit code.
fn assert_fixture_queries_match_golden(name: &str) {
    let temp = build_fixture(name);
    let expected = expected_dir(name);
    for (id, argv) in read_query_list(name) {
        if is_pinned_id(&id) {
            assert_run_matches_golden(&temp.path, &expected, &id, &argv);
        }
    }
}

/// Builds one fixture and runs only the listed query ids from its query
/// list. Panics when an id is not in the list.
fn assert_fixture_ids_match_golden(name: &str, ids: &[&str]) {
    let temp = build_fixture(name);
    let expected = expected_dir(name);
    let list = read_query_list(name);
    for id in ids {
        let (_, argv) = list
            .iter()
            .find(|(list_id, _)| list_id == id)
            .unwrap_or_else(|| panic!("no query id {id} in tests/inputs/queries/{name}.txt"));
        assert_run_matches_golden(&temp.path, &expected, id, argv);
    }
}

/// Copies one fixture into a fresh temp copy with no build.
fn unbuilt_fixture(name: &str) -> TempDir {
    let fixture = support::manifest_dir().join(format!("../../tests/fixtures/{name}"));
    let temp = TempDir::new(&format!("{name}-unbuilt"));
    copy_dir(&fixture, &temp.path);
    temp
}

fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| s.to_string()).collect()
}

#[test]
fn test_p1_17_to_36_p3_20_to_33_query_cli_matches_golden() {
    for fixture in [
        "basic",
        "symbols",
        "edges",
        "multi",
        "workspace",
        "pnpm",
        "langs",
        "wide",
        "callx",
    ] {
        assert_fixture_queries_match_golden(fixture);
    }
}

/// P1-21: in `zed`'s span, `doubled(` comes before `double(`, so a
/// substring match quotes the wrong line. Sieve quotes the whole-word line.
#[test]
fn test_p1_21_callers_quotes_the_whole_word_line() {
    assert_fixture_ids_match_golden("callx", &["callers-p1-21-double"]);
}

/// P3-25: sieve lists the hits in edge order, sorted by (source, relation,
/// target). From `zed`, the `calls` hits come before the `references` hit
/// to `Widget`, so the hit order is not id order.
#[test]
fn test_p3_25_callers_keeps_edge_order_not_id_order() {
    assert_fixture_ids_match_golden("callx", &["callers-p3-25-zed-out"]);
}

/// P1-22: a zero-hit `--json` match carries the `note` string.
#[test]
fn test_p1_22_callers_json_zero_hit_carries_note() {
    assert_fixture_ids_match_golden("symbols", &["callers-p1-22-method-json"]);
}

/// P3-23: a file seed at depth 2 walks from the file node and from every
/// symbol in that file.
#[test]
fn test_p3_23_callers_file_seed_walks_every_symbol_in_the_file() {
    assert_fixture_ids_match_golden("basic", &["callers-p3-23-file-depth2"]);
}

/// P1-17: an exact repo-relative path wins, even when a second file
/// shares the basename; only the bare basename is ambiguous.
#[test]
fn test_p1_17_skeleton_exact_path_wins_over_a_basename_twin() {
    assert_fixture_ids_match_golden("multi", &["skeleton-exact", "skeleton-amb"]);
}

/// P1-19: `-d`, `--depth full`, `--depth max` and `--no-refresh` each run
/// the documented way. `full` and `max` query `double`, whose chain
/// `double <- quadruple <- run <- main` has depth 3. So a parse that
/// maps `full` or `max` to depth 2 drops `main` and fails.
#[test]
fn test_p1_19_callers_depth_grammar_and_no_refresh_match_golden() {
    assert_fixture_ids_match_golden(
        "basic",
        &[
            "callers-d2",
            "callers-full",
            "callers-max",
            "callers-norefresh",
        ],
    );
}

/// P1-21: `callers app.ts --direction out` lists two import hits. Sieve
/// marks `zod` as `(unresolved import)` and gives `util.ts` a file span.
/// Sieve quotes no call-site line for either hit. The golden pins both.
#[test]
fn test_p1_21_callers_quotes_no_line_for_an_unresolved_import() {
    assert_fixture_ids_match_golden("basic", &["callers-file-out"]);
}

/// P1-23: `--depth 0` exits 1 with the `✗` line.
#[test]
fn test_p1_23_callers_exits_1_on_a_bad_depth() {
    assert_fixture_ids_match_golden("basic", &["callers-depth0"]);
}

/// P1-23: `callers` with no graph exits 1 with the `✗` line, the
/// scratch path masked as `<TMP>`.
#[test]
fn test_p1_23_callers_exits_1_without_graph() {
    let temp = unbuilt_fixture("basic");
    assert_run_matches_golden(
        &temp.path,
        &expected_dir("basic"),
        "callers-nograph",
        &argv(&["callers", "add"]),
    );
}

/// P1-28: with 19 one-file directories and 13 called symbols, the
/// defaults show 16 directories, `+3 more directories`, and 12 hotspots.
#[test]
fn test_p1_28_map_defaults_drop_the_17th_directory_and_the_13th_hotspot() {
    assert_fixture_ids_match_golden("wide", &["map"]);
}

/// P1-31: two dropped directories read `directories`, one reads
/// `directory`.
#[test]
fn test_p1_31_map_dropped_note_uses_the_plural() {
    assert_fixture_ids_match_golden("basic", &["map-max1", "map-max2"]);
}

/// P1-36: `check` and `check --json` with no graph exit 1.
#[test]
fn test_p1_36_check_exits_1_without_graph() {
    let temp = unbuilt_fixture("basic");
    let expected = expected_dir("basic");
    assert_run_matches_golden(&temp.path, &expected, "check-nograph", &argv(&["check"]));
    assert_run_matches_golden(
        &temp.path,
        &expected,
        "check-nograph-json",
        &argv(&["check", "--json"]),
    );
}

/// P1-36: `check` after one source line changes, with no rebuild,
/// reports STALE and exits 1.
#[test]
fn test_p1_36_check_exits_1_on_drift() {
    let temp = build_fixture("basic");
    let util = temp.path.join("src/util.ts");
    let mut text = fs::read_to_string(&util).expect("read util.ts");
    text.push_str("\nexport const driftMarker = 1;\n");
    fs::write(&util, text).expect("write util.ts");
    assert_run_matches_golden(
        &temp.path,
        &expected_dir("basic"),
        "check-drift",
        &argv(&["check"]),
    );
}
