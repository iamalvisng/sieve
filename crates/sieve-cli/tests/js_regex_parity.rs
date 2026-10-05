//! Parity tests for JS regex rules against a recorded run (P1-04, P1-12),
//! against the goldens under `tests/fixtures/basic.expected/js-regex/`.
//! The query list lives in `tests/inputs/js-regex/basic.txt`.

mod support;

use std::fs;
use std::path::Path;

use support::TempDir;

/// Copies `src` into `dst`, recursively, including dot-directories.
fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("create dst dir");
    for entry in fs::read_dir(src).expect("read src dir") {
        let entry = entry.expect("read dir entry");
        let target = dst.join(entry.file_name());
        if entry.file_type().expect("read file type").is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy file");
        }
    }
}

/// Splits a query line's arguments: whitespace-separated, with single or
/// double quotes grouping one argument. Only ASCII space and tab split, so
/// U+0085 stays in its argument.
fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in s.chars() {
        match c {
            '"' | '\'' if quote.is_none() => quote = Some(c),
            c if Some(c) == quote => quote = None,
            ' ' | '\t' if quote.is_none() => {
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

/// Runs the query line `id` in a built copy of `basic` and compares
/// stdout, stderr (minus `⬆` lines) and the exit code with the golden.
fn assert_matches_golden(id: &str) {
    let root = support::manifest_dir().join("../..");
    let list = fs::read_to_string(root.join("tests/inputs/js-regex/basic.txt"))
        .expect("read js-regex list");
    let args = list
        .lines()
        .find_map(|l| l.split_once('\t').filter(|(lid, _)| *lid == id))
        .map(|(_, args)| args)
        .unwrap_or_else(|| panic!("id {id} missing from the js-regex list"));
    let temp = TempDir::new("js-regex");
    copy_dir(&root.join("tests/fixtures/basic"), &temp.path);
    let build = support::sieve_command()
        .args(["build", "."])
        .current_dir(&temp.path)
        .output()
        .expect("run sieve build");
    assert_eq!(build.status.code(), Some(0), "sieve build failed");
    let output = support::sieve_command()
        .args(split_args(args))
        .current_dir(&temp.path)
        .output()
        .expect("run sieve");
    let golden = root.join("tests/fixtures/basic.expected/js-regex");
    let read = |suffix: &str| {
        fs::read_to_string(golden.join(format!("{id}.{suffix}.txt")))
            .unwrap_or_else(|e| panic!("read golden {id}.{suffix}.txt: {e}"))
    };
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr: String = stderr
        .lines()
        .filter(|l| !l.starts_with('⬆'))
        .map(|l| format!("{l}\n"))
        .collect();
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        read("stdout"),
        "{id} stdout"
    );
    assert_eq!(stderr, read("stderr"), "{id} stderr");
    assert_eq!(
        output.status.code(),
        Some(read("exit").trim().parse::<i32>().expect("parse exit")),
        "{id} exit"
    );
}

/// P1-04: U+FEFF is JS `\s`, so `who<FEFF>calls` is a structural query.
#[test]
fn test_p1_04_feff_counts_as_js_whitespace() {
    assert_matches_golden("p1-04-feff");
}

/// P1-04: U+0085 is not JS `\s`, so `who<NEL>calls` is a lexical query.
#[test]
fn test_p1_04_nel_is_not_js_whitespace() {
    assert_matches_golden("p1-04-nel");
}

/// P1-12: a pattern that starts with `{2}` reports `Nothing to repeat`.
#[test]
fn test_p1_12_leading_brace_quantifier_reason() {
    assert_matches_golden("p1-12-brace-quant");
}
