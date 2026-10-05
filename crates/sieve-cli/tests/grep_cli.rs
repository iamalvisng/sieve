//! Parity tests for `sieve grep` (P1-12 to P1-16, P1-46, P1-48, P1-49,
//! P2-29): the query prelude and the `grep` report, against Sieve's
//! goldens under `tests/fixtures/<name>.expected/queries/`.

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

/// Runs every `grep-*` query for one fixture and asserts it matches the
/// golden stdout, stderr (after dropping `⬆` lines), and exit code.
/// Returns the count of `grep-*` ids it ran, so the caller can catch a
/// fixture with zero ids (a vacuous pass) across the whole test.
fn assert_fixture_grep_matches_golden(name: &str) -> usize {
    let query_list = support::manifest_dir().join(format!("../../tests/inputs/queries/{name}.txt"));
    if !query_list.is_file() {
        return 0;
    }
    let expected_dir =
        support::manifest_dir().join(format!("../../tests/fixtures/{name}.expected/queries"));
    let temp = build_fixture(name);
    let lines = fs::read_to_string(&query_list).expect("read query list");
    let mut ran = 0usize;

    for line in lines.lines() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((id, args)) = line.split_once('\t') else {
            continue;
        };
        if !id.starts_with("grep-") {
            continue;
        }

        ran += 1;
        let mut argv = split_args(args);
        argv[0] = "grep".to_string();
        let output = run_sieve(&temp.path, &argv);

        support::golden::bless_triple(
            &expected_dir,
            id,
            &output.stdout,
            normalize_stderr(&String::from_utf8_lossy(&output.stderr)).as_bytes(),
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

        let actual_stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let actual_stderr = String::from_utf8_lossy(&output.stderr).into_owned();
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
            "query {id} in {name}: stdout mismatch"
        );
        assert_eq!(
            normalize_stderr(&actual_stderr),
            normalize_stderr(&expected_stderr),
            "query {id} in {name}: stderr mismatch"
        );
        assert_eq!(
            actual_exit, expected_exit,
            "query {id} in {name}: exit mismatch"
        );
    }
    ran
}

#[test]
fn test_p1_12_to_16_p1_49_grep_cli_matches_golden() {
    let mut total_ran = 0usize;
    for fixture in [
        "basic",
        "edges",
        "scopes",
        "symbols",
        "multi",
        "workspace",
        "pnpm",
    ] {
        total_ran += assert_fixture_grep_matches_golden(fixture);
    }
    // `workspace` and `pnpm` carry no `grep-*` queries. Assert at least one
    // id ran across every fixture, so an empty query list never lets the
    // test pass without checking anything.
    assert!(total_ran > 0, "no grep-* query ran in any fixture");
}

#[test]
fn test_p1_46_no_refresh_skips_the_rebuild_and_p2_29_refresh_writes_no_cards() {
    let temp = build_fixture("basic");
    let card_path = temp.path.join("sieve").join("py").join("helpers.md");
    let card_before = fs::read(&card_path).expect("read card before edit");
    let wiring_path = temp.path.join("sieve").join(".graph").join("wiring.json");
    let wiring_before = fs::read(&wiring_path).expect("read wiring before edit");

    // Edit a source file, then run a query with the refresh enabled: the
    // graph rebuilds, but the card and INDEX.md stay untouched.
    let helpers = temp.path.join("py").join("helpers.py");
    let mut source = fs::read_to_string(&helpers).expect("read helpers.py");
    source.push_str("\n\ndef extra(x):\n    return x\n");
    fs::write(&helpers, source).expect("edit helpers.py");

    let output = run_sieve(
        &temp.path,
        &["grep".to_string(), "add".to_string(), "--fixed".to_string()],
    );
    let stderr = normalize_stderr(&String::from_utf8_lossy(&output.stderr));
    assert_eq!(
        stderr,
        "[sieve] refreshed the graph (1 file changed) before answering\n"
    );

    let card_after = fs::read(&card_path).expect("read card after refresh");
    assert_eq!(
        card_before, card_after,
        "the graph-only refresh must not touch a card"
    );

    let wiring_after = fs::read(&wiring_path).expect("read wiring after refresh");
    assert_ne!(
        wiring_before, wiring_after,
        "the graph-only refresh must rewrite wiring.json"
    );

    // Edit again, then run with `--no-refresh`: no refresh note.
    let mut source = fs::read_to_string(&helpers).expect("read helpers.py");
    source.push_str("\ndef extra2(x):\n    return x\n");
    fs::write(&helpers, source).expect("edit helpers.py again");

    let output = run_sieve(
        &temp.path,
        &[
            "grep".to_string(),
            "add".to_string(),
            "--fixed".to_string(),
            "--no-refresh".to_string(),
        ],
    );
    let stderr = normalize_stderr(&String::from_utf8_lossy(&output.stderr));
    assert_eq!(stderr, "", "--no-refresh must print no refresh note");
}

#[test]
fn test_p1_48_ancestor_walk_note() {
    let temp = build_fixture("basic");
    let py_dir = temp.path.join("py");
    let output = run_sieve(
        &py_dir,
        &["grep".to_string(), "add".to_string(), "--fixed".to_string()],
    );
    let stderr = normalize_stderr(&String::from_utf8_lossy(&output.stderr));
    let expected_root = fs::canonicalize(&temp.path).expect("canonicalize temp root");
    assert_eq!(
        stderr,
        format!(
            "[sieve] no sieve/ here — answering from {}/sieve\n",
            expected_root.display()
        )
    );
}
