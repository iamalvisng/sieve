//! Parity test: `blast` against the `blast*` goldens under the `edges`
//! fixture (`tests/inputs/queries/edges.blast.txt`).
//!
//! Cites P1-24 to P1-28, P3-26.
//!
//! The harness commits the built `edges` fixture, edits and adds, then runs the
//! `blast*` queries against the working tree. This test reproduces that git
//! state in a temp copy.
//!
//! Known gap: `sieve blast` refreshes (re-parses) the graph before it
//! diffs, so its seeds see the EDITED file content. `sieve-query` has no
//! parser (extraction lives in `sieve-parse`, a different crate this task
//! must not touch), so this test hands `blast()` the golden `wiring.json`
//! as captured before the edit. That graph is stale for any query whose
//! seeds depend on the edit, so this test covers only `blast-base` (an
//! empty `HEAD...HEAD` range) and `blast-notgit` (an error path), which do
//! not depend on the edited content and match exactly.
//!
//! The `blast`, `blast-d2` and `blast-json` ids need the refresh, so
//! `crates/sieve-cli/tests/blast_cli.rs`'s
//! `test_p1_24_to_28_p3_26_blast_cli_matches_sieve` pins them through the real
//! binary, which runs the refresh before it seeds.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use sieve_core::Graph;
use sieve_query::blast::{blast, format_blast_text, BlastOptions};
use sieve_query::callers::Depth;

fn manifest_dir() -> PathBuf {
    PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
}

fn fixtures_root() -> PathBuf {
    manifest_dir().join("../../tests/fixtures")
}

fn repo_root() -> PathBuf {
    manifest_dir()
        .join("../..")
        .canonicalize()
        .expect("canonicalize repo root")
}

/// A temp directory that removes itself on drop, even if a test panics
/// before it reaches its own cleanup line.
struct TempDir(PathBuf);

impl std::ops::Deref for TempDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn mktemp_dir() -> TempDir {
    let out = Command::new("mktemp")
        .arg("-d")
        .output()
        .expect("run mktemp");
    assert!(out.status.success(), "mktemp failed");
    let path = PathBuf::from(String::from_utf8(out.stdout).expect("utf8").trim())
        .canonicalize()
        .expect("canonicalize temp dir");
    TempDir(path)
}

fn run(dir: &Path, args: &[&str]) {
    let out = Command::new(args[0])
        .args(&args[1..])
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("run {:?} in {}: {e}", args, dir.display()));
    assert!(
        out.status.success(),
        "command {:?} failed in {}: {}",
        args,
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn copy_dir(src: &Path, dst: &Path) {
    run(
        Path::new("/"),
        &[
            "cp",
            "-R",
            &format!("{}/.", src.display()),
            &dst.to_string_lossy(),
        ],
    );
}

/// Builds the temp git state of the blast queries:
/// the fixture, committed, then edited.
fn setup_edges_repo() -> TempDir {
    let dir = mktemp_dir();
    copy_dir(&fixtures_root().join("edges"), &dir);
    run(
        &dir,
        &["git", "-c", "init.defaultBranch=main", "init", "-q"],
    );
    run(&dir, &["git", "config", "user.name", "sieve-fixture"]);
    run(
        &dir,
        &["git", "config", "user.email", "fixture@sieve.local"],
    );
    run(&dir, &["git", "add", "-A"]);
    run(&dir, &["git", "commit", "-q", "-m", "base"]);

    let derived = dir.join("ts/derived.ts");
    let mut text = fs::read_to_string(&derived).expect("read derived.ts");
    text.push_str("\nexport function extra(): number { return greet().length; }\n");
    fs::write(&derived, text).expect("write derived.ts");
    fs::write(
        dir.join("ts/newfile.ts"),
        "export function newFn(): number {\n  return 1;\n}\n",
    )
    .expect("write newfile.ts");
    run(&dir, &["git", "add", "-A"]);
    dir
}

fn load_golden_graph() -> Graph {
    let text = fs::read_to_string(fixtures_root().join("edges.expected/wiring.json"))
        .expect("read wiring.json");
    serde_json::from_str(&text).expect("parse wiring.json")
}

fn read_golden(id: &str, ext: &str) -> String {
    fs::read_to_string(
        fixtures_root()
            .join("edges.expected/queries")
            .join(format!("{id}.{ext}")),
    )
    .unwrap_or_else(|_| panic!("read golden {id}.{ext}"))
}

/// Masks one temp dir's path out of an error message, the way
/// the golden captures mask it, so the two
/// sides compare without the directory mktemp handed them this run.
fn mask_tmp(msg: &str, dir: &Path) -> String {
    msg.replace(&dir.to_string_lossy().to_string(), "<TMP>")
}

fn last_stderr_line_without_prefix(golden: &str) -> Option<String> {
    let line = golden.lines().last()?;
    Some(line.strip_prefix("✗ ").unwrap_or(line).to_string())
}

/// One unified-style diff: every differing line, want then got.
fn unified_diff(want: &str, got: &str) -> String {
    let want_lines: Vec<&str> = want.lines().collect();
    let got_lines: Vec<&str> = got.lines().collect();
    let max = want_lines.len().max(got_lines.len());
    let mut out = String::new();
    for i in 0..max {
        let w = want_lines.get(i).copied().unwrap_or("<missing>");
        let g = got_lines.get(i).copied().unwrap_or("<missing>");
        if w != g {
            out.push_str(&format!("  line {i}:\n  - want: {w}\n  + got:  {g}\n"));
        }
    }
    out
}

struct Query {
    id: &'static str,
    base: Option<&'static str>,
    depth: Depth,
}

const QUERIES: &[Query] = &[Query {
    id: "blast-base",
    base: Some("HEAD"),
    depth: Depth(Some(1)),
}];

/// Covers `blast-base` and `blast-notgit` only. `crates/sieve-cli/tests/
/// blast_cli.rs`'s `test_p1_24_to_28_p3_26_blast_cli_matches_sieve` pins
/// `blast`, `blast-d2` and `blast-json`, because those three need the
/// refresh (re-parse) that only the real CLI binary runs before it seeds.
#[test]
fn test_p1_24_to_28_p3_26_blast_matches_golden() {
    let root = repo_root();
    let context_dir = root.join("tests/fixtures/edges.expected/sieve");
    let graph = load_golden_graph();
    let dir = setup_edges_repo();

    let mut failures = Vec::new();

    for q in QUERIES {
        let opts = BlastOptions {
            base: q.base.map(str::to_string),
            depth: q.depth,
            owners: true,
            pr_author: Vec::new(),
        };
        let want_exit: i32 = read_golden(q.id, "exit.txt")
            .trim()
            .parse()
            .expect("exit code");
        let result = blast(&graph, &dir, &context_dir, &opts);

        match result {
            Ok(report) => {
                if want_exit != 0 {
                    failures.push(format!(
                        "{}: want exit {want_exit}, blast() returned Ok",
                        q.id
                    ));
                    continue;
                }
                let got = if q.id == "blast-json" {
                    format!(
                        "{}\n",
                        serde_json::to_string_pretty(&report).expect("serialize report")
                    )
                } else {
                    format_blast_text(&report)
                };
                let want = read_golden(q.id, "stdout.txt");
                if got.trim_end_matches('\n') != want.trim_end_matches('\n') {
                    failures.push(format!(
                        "{}: stdout mismatch\n{}",
                        q.id,
                        unified_diff(want.trim_end_matches('\n'), got.trim_end_matches('\n'))
                    ));
                }
            }
            Err(e) => {
                if want_exit == 0 {
                    failures.push(format!("{}: want exit 0, blast() returned Err({e})", q.id));
                    continue;
                }
                let want_stderr = read_golden(q.id, "stderr.txt");
                let want_last = last_stderr_line_without_prefix(&want_stderr).unwrap_or_default();
                let got = mask_tmp(&e.to_string(), &dir);
                if got != want_last {
                    failures.push(format!(
                        "{}: stderr mismatch\n{}",
                        q.id,
                        unified_diff(&want_last, &got)
                    ));
                }
            }
        }
    }

    // blast-notgit: a second copy with `.git` removed. The graph still
    // loads (the caller already has it); the diff read must fail.
    let notgit_dir = mktemp_dir();
    copy_dir(&dir, &notgit_dir);
    fs::remove_dir_all(notgit_dir.join(".git")).expect("remove .git");
    let opts = BlastOptions {
        base: None,
        depth: Depth(Some(1)),
        owners: true,
        pr_author: Vec::new(),
    };
    let want_exit: i32 = read_golden("blast-notgit", "exit.txt")
        .trim()
        .parse()
        .expect("exit code");
    match blast(&graph, &notgit_dir, &context_dir, &opts) {
        Ok(_) => {
            if want_exit != 0 {
                failures.push("blast-notgit: want an error, blast() returned Ok".to_string());
            }
        }
        Err(e) => {
            let want_stderr = read_golden("blast-notgit", "stderr.txt");
            let want_last = last_stderr_line_without_prefix(&want_stderr).unwrap_or_default();
            let got = mask_tmp(&e.to_string(), &notgit_dir);
            if got != want_last {
                failures.push(format!(
                    "blast-notgit: stderr mismatch\n{}",
                    unified_diff(&want_last, &got)
                ));
            }
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
