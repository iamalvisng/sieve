//! Shared helpers for the workspace parity tests (`workspace_parity.rs`,
//! `workspace_step2_parity.rs`): a fresh copy of `tests/fixtures/ws/`,
//! the run-time git init, the path mask, and the golden compare, all
//! matching the golden captures.

#![allow(dead_code)]

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use super::TempDir;

/// Copies `src` into `dst`, recursively, including dot-directories.
pub fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("create dst dir");
    for entry in fs::read_dir(src).expect("read src dir") {
        let entry = entry.expect("read dir entry");
        let target = dst.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy file");
        }
    }
}

/// A fresh copy of `tests/fixtures/ws/`, with no git anywhere.
pub fn fresh_copy(label: &str) -> TempDir {
    let fixture = super::manifest_dir().join("../../tests/fixtures/ws");
    let temp = TempDir::new(label);
    copy_dir(&fixture, &temp.path);
    temp
}

/// Makes `dir` a git repo with every file staged.
pub fn git_init(dir: &Path) {
    for args in [
        vec!["-c", "init.defaultBranch=main", "init", "-q"],
        vec!["add", "-A"],
    ] {
        let status = Command::new("git")
            .args(&args)
            .current_dir(dir)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed in {}", dir.display());
    }
}

/// Runs `sieve <args>` from `run_dir`.
pub fn sieve(run_dir: &Path, args: &[&str]) -> Output {
    super::sieve_command()
        .args(args)
        .current_dir(run_dir)
        .output()
        .expect("run sieve")
}

/// Replaces every run of `"../"` (zero or more), an optional leading `/`,
/// then the literal `body`, with `<TMP>`
/// does.
fn mask_body(text: &str, body: &str) -> String {
    let bytes = text.as_bytes();
    let body_bytes = body.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(body_bytes) {
            let mut cut = out.len();
            while out[..cut].ends_with("../") {
                cut -= 3;
            }
            if out[..cut].ends_with('/') {
                cut -= 1;
            }
            out.truncate(cut);
            out.push_str("<TMP>");
            i += body_bytes.len();
        } else {
            let ch = text[i..].chars().next().expect("utf-8 boundary");
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// Masks the copy's path, the realpath form first, then the literal form.
pub fn mask(text: &str, copy: &Path) -> String {
    let canon = fs::canonicalize(copy).unwrap_or_else(|_| copy.to_path_buf());
    let mut out = text.to_string();
    for form in [canon, copy.to_path_buf()] {
        let body = form.display().to_string();
        out = mask_body(&out, body.trim_start_matches('/'));
    }
    out
}

/// Drops the update-nudge line, which the harness cannot silence.
pub fn drop_update_nudge(text: &str) -> String {
    text.lines()
        .filter(|line| !line.starts_with('⬆'))
        .map(|line| format!("{line}\n"))
        .collect()
}

/// The golden dir `tests/fixtures/ws.expected/<sub>/`.
pub fn golden_dir(sub: &str) -> std::path::PathBuf {
    super::manifest_dir()
        .join("../../tests/fixtures/ws.expected")
        .join(sub)
}

/// With `SIEVE_BLESS=1`, writes `actual` as the golden `name` under `sub`.
pub fn bless(sub: &str, name: &str, actual: &str) {
    if super::golden::blessing() {
        super::golden::bless(&golden_dir(sub).join(name), actual.as_bytes());
    }
}

/// Reads the golden `name` under `tests/fixtures/ws.expected/<sub>/`.
pub fn golden(sub: &str, name: &str) -> String {
    let path = golden_dir(sub).join(name);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Every file under `copy`, sorted, less `.git/` and `sieve/.cache/`, one
/// per line.
pub fn file_list(copy: &Path) -> String {
    fn walk(dir: &Path, rel: &Path, out: &mut Vec<String>) {
        for entry in fs::read_dir(dir).expect("read dir") {
            let entry = entry.expect("dir entry");
            let name = entry.file_name();
            let rel = rel.join(&name);
            if entry.file_type().expect("file type").is_dir() {
                let under_sieve = dir.file_name().is_some_and(|n| n == "sieve");
                if name == ".git" || (name == ".cache" && under_sieve) {
                    continue;
                }
                walk(&entry.path(), &rel, out);
            } else {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let mut out = Vec::new();
    walk(copy, Path::new(""), &mut out);
    out.sort();
    out.iter().map(|p| format!("{p}\n")).collect()
}

/// Asserts `output` matches the golden `id` under `sub`: stdout, stderr
/// and the exit code, with the copy's path masked.
pub fn assert_matches(sub: &str, id: &str, output: &Output, copy: &Path) {
    let stdout = mask(&String::from_utf8_lossy(&output.stdout), copy);
    let stderr = drop_update_nudge(&mask(&String::from_utf8_lossy(&output.stderr), copy));
    let exit = format!("{}\n", output.status.code().unwrap_or(-1));
    bless(sub, &format!("{id}.stdout.txt"), &stdout);
    bless(sub, &format!("{id}.stderr.txt"), &stderr);
    bless(sub, &format!("{id}.exit.txt"), &exit);
    assert_eq!(
        stdout,
        golden(sub, &format!("{id}.stdout.txt")),
        "{id} stdout"
    );
    assert_eq!(
        stderr,
        golden(sub, &format!("{id}.stderr.txt")),
        "{id} stderr"
    );
    assert_eq!(exit, golden(sub, &format!("{id}.exit.txt")), "{id} exit");
}

/// A built workspace: the fixture copy with both children as repos, after
/// one `sieve build` at the parent.
pub fn built_workspace(label: &str) -> (TempDir, Output) {
    let copy = fresh_copy(label);
    git_init(&copy.path.join("alpha"));
    git_init(&copy.path.join("beta"));
    let output = sieve(&copy.path, &["build"]);
    (copy, output)
}
