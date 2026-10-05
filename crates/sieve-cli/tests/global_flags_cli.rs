//! Parity tests for P1-45: the global `--dir` flag and the `SIEVE_DIR`
//! env var are two separate overrides.
//!
//! The `--dir` flag sets the context dir override, read only from the CLI flag,
//! with no env fallback of its own (the `01-cli.md` note section 4).
//! `SIEVE_DIR` is a different override, read only by the context dir resolver
//! for the hook and statusline entry points. A query command — `ask`, `grep`,
//! `skeleton`, `callers`, `blast`, `map`, `check` — never reads `SIEVE_DIR`.
//! This file pins that a query command ignores `SIEVE_DIR` outright, and that
//! `--dir` alone decides the context dir even when `SIEVE_DIR` also names a
//! different path. `--no-refresh` and `SIEVE_NO_REFRESH` are the one flag/env
//! pair a query command does honor together; `grep_cli.rs`'s
//! `test_p1_46_no_refresh_skips_the_rebuild_and_p2_29_refresh_writes_no_cards`
//! already pins that rule.

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

/// Runs `sieve` in `root`, with `sieve_dir_env` set to `SIEVE_DIR` when
/// given, and `dir_flag` passed as `--dir` when given.
fn run_sieve(
    root: &Path,
    dir_flag: Option<&Path>,
    dir_env: Option<&Path>,
    args: &[&str],
) -> Output {
    let mut cmd = support::sieve_command();
    if let Some(d) = dir_flag {
        cmd.arg("--dir").arg(d);
    }
    cmd.args(args).current_dir(root);
    match dir_env {
        Some(v) => cmd.env("SIEVE_DIR", v),
        None => cmd.env_remove("SIEVE_DIR"),
    };
    cmd.output().expect("run sieve")
}

/// Copies the `basic` fixture into a fresh temp dir and runs `sieve build`
/// with no `--dir`, so the graph lands at `<repo>/sieve`.
fn build_fixture_default_dir() -> TempDir {
    let fixture = support::manifest_dir().join("../../tests/fixtures/basic");
    let temp = TempDir::new("global-flags");
    copy_dir(&fixture, &temp.path);
    let output = run_sieve(&temp.path, None, None, &["build", "."]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "build failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    temp
}

/// P1-45: a query command must ignore `SIEVE_DIR` outright. With the
/// graph at the default `<repo>/sieve` and `SIEVE_DIR` pointed at an
/// empty, unrelated dir, `grep` must still find the default graph — a
/// command that read `SIEVE_DIR` would report "no graph" instead.
#[test]
fn test_p1_45_dir_env_alone_does_not_relocate_a_query_context_dir() {
    let temp = build_fixture_default_dir();
    let empty_other = TempDir::new("global-flags-other");

    let grep = run_sieve(
        &temp.path,
        None,
        Some(&empty_other.path),
        &["grep", "add", "--fixed", "--no-refresh"],
    );
    assert_eq!(
        grep.status.code(),
        Some(0),
        "grep must still read <repo>/sieve, ignoring SIEVE_DIR: stderr={}",
        String::from_utf8_lossy(&grep.stderr)
    );
    assert!(
        !grep.stdout.is_empty(),
        "grep must report hits from the default context dir"
    );
}

/// P1-45: `--dir` decides the context dir outright when `SIEVE_DIR` also
/// names a different, unrelated path. This is the flag/env pair's real
/// precedence rule: not "the flag beats the env var" (they are not the
/// same override), but "the flag is the only one a query command reads".
#[test]
fn test_p1_45_dir_flag_decides_the_context_dir_even_with_sieve_dir_set() {
    let fixture = support::manifest_dir().join("../../tests/fixtures/basic");
    let repo = TempDir::new("global-flags-repo");
    copy_dir(&fixture, &repo.path);
    let ctx = TempDir::new("global-flags-ctx");
    let ctx_dir = ctx.path.join("ctx");
    let unrelated = TempDir::new("global-flags-unrelated");

    let build = run_sieve(&repo.path, Some(&ctx_dir), None, &["build", "."]);
    assert_eq!(
        build.status.code(),
        Some(0),
        "build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let grep = run_sieve(
        &repo.path,
        Some(&ctx_dir),
        Some(&unrelated.path),
        &["grep", "add", "--fixed", "--no-refresh"],
    );
    assert_eq!(
        grep.status.code(),
        Some(0),
        "grep must read the --dir context dir, ignoring SIEVE_DIR: stderr={}",
        String::from_utf8_lossy(&grep.stderr)
    );
    assert!(
        !grep.stdout.is_empty(),
        "grep must report hits from the --dir context dir"
    );
    assert!(
        !unrelated.path.join(".graph").exists(),
        "the graph must never land under the SIEVE_DIR path for a query command"
    );
}
