//! Parity tests for `sieve blast` (P1-24 to P1-28, P3-26): the diff and impact
//! report, against the goldens under `tests/fixtures/edges.expected/queries/`.
//! Builds the `edges` fixture, commits it, edits two files, then runs every
//! `edges.blast.txt` id (the two edits live in `build_and_edit_edges_fixture`).

mod support;

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

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

/// Prints a minimal line diff between `expected` and `actual`, labeled
/// with `id` and `field`.
fn print_diff(id: &str, field: &str, expected: &str, actual: &str) {
    eprintln!("blast query {id}, field {field}, mismatch:");
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

/// Builds the `edges` fixture into a fresh temp copy, commits the built tree,
/// then applies two edits: one to an existing file and one new file.
fn build_and_edit_edges_fixture() -> TempDir {
    let fixture = support::manifest_dir().join("../../tests/fixtures/edges");
    let temp = TempDir::new("edges");
    copy_dir(&fixture, &temp.path);

    let output = run_sieve(&temp.path, &["build".to_string(), ".".to_string()]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "sieve build failed for edges: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    run_git(&temp.path, &["init", "-q"]);
    // The repo-local identity: `blast` drops
    // the local identity from its reviewers, so the base commit's author
    // must be that identity, not the machine's global one.
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

    temp
}

/// Replaces every occurrence of `root`'s canonical path with `<TMP>`, the
/// same mask the golden files carry.
fn mask_tmp(s: &str, root: &Path) -> String {
    let canon = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    s.replace(&canon.display().to_string(), "<TMP>")
        .replace(&root.display().to_string(), "<TMP>")
}

fn assert_query_matches_golden(root: &Path, expected_dir: &Path, id: &str, args: &str) {
    let argv = split_args(args);
    let output = run_sieve(root, &argv);

    support::golden::bless_triple(
        expected_dir,
        id,
        mask_tmp(&String::from_utf8_lossy(&output.stdout), root).as_bytes(),
        normalize_stderr(&mask_tmp(&String::from_utf8_lossy(&output.stderr), root)).as_bytes(),
        output.status.code().unwrap_or(-1),
    );
    let expected_stdout =
        fs::read_to_string(expected_dir.join(format!("{id}.stdout.txt"))).expect("read stdout");
    let expected_stderr =
        fs::read_to_string(expected_dir.join(format!("{id}.stderr.txt"))).expect("read stderr");
    let expected_exit = fs::read_to_string(expected_dir.join(format!("{id}.exit.txt")))
        .expect("read exit")
        .trim()
        .parse::<i32>()
        .expect("parse exit");

    let actual_stdout = mask_tmp(&String::from_utf8_lossy(&output.stdout), root);
    let actual_stderr = mask_tmp(&String::from_utf8_lossy(&output.stderr), root);
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
        "blast query {id}: stdout mismatch"
    );
    assert_eq!(
        normalize_stderr(&actual_stderr),
        normalize_stderr(&expected_stderr),
        "blast query {id}: stderr mismatch"
    );
    assert_eq!(
        actual_exit, expected_exit,
        "blast query {id}: exit mismatch"
    );
}

#[test]
fn test_p1_24_to_28_p3_26_blast_cli_matches_golden() {
    let query_list = support::manifest_dir().join("../../tests/inputs/queries/edges.blast.txt");
    let expected_dir = support::manifest_dir().join("../../tests/fixtures/edges.expected/queries");
    let lines = fs::read_to_string(&query_list).expect("read edges.blast.txt");

    let git_temp = build_and_edit_edges_fixture();

    // `blast-notgit` runs in a second copy with `.git` removed, keeping
    // the already-built `sieve/` tree.
    let notgit_fixture = support::manifest_dir().join("../../tests/fixtures/edges");
    let notgit_temp = TempDir::new("edges-notgit");
    copy_dir(&notgit_fixture, &notgit_temp.path);
    let output = run_sieve(&notgit_temp.path, &["build".to_string(), ".".to_string()]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "sieve build failed for edges-notgit"
    );
    let derived = notgit_temp.path.join("ts").join("derived.ts");
    let mut source = fs::read_to_string(&derived).expect("read derived.ts");
    source.push_str("\nexport function extra(): number { return greet().length; }\n");
    fs::write(&derived, source).expect("edit derived.ts");
    fs::write(
        notgit_temp.path.join("ts").join("newfile.ts"),
        "export function newFn(): number {\n  return 1;\n}\n",
    )
    .expect("write newfile.ts");

    for line in lines.lines() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((id, args)) = line.split_once('\t') else {
            continue;
        };

        let root = if id == "blast-notgit" {
            &notgit_temp.path
        } else {
            &git_temp.path
        };
        assert_query_matches_golden(root, &expected_dir, id, args);
    }
}
