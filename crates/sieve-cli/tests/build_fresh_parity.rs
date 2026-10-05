//! Parity tests for the build's freshness rule (P2-35, P1-46) and for an
//! unreadable file or dir (P2-01), against recorded goldens under
//! `tests/fixtures/basic.expected/build-fresh/`.
//!
//! Sieve reads and hashes every file on every build and replays only the parse.
//! A `touch` replays, a same-size edit with the mtime put back re-parses, and
//! `SIEVE_REFRESH=hash` rebuilds once. An unreadable file prints Node's
//! `EACCES` `open` line and the build goes on. With no git, an unreadable dir
//! fails the walk with Node's `scandir` line.
//!
//! Each test copies the `basic` fixture into a scratch dir, builds with
//! `sieve`, applies one change, runs one command, then byte-compares
//! stdout, stderr and the exit code with the golden.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

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

/// Drops every `⬆` update-nudge line, then rejoins with one trailing
/// newline when anything remains.
fn normalize_stderr(s: &str) -> String {
    let kept: Vec<&str> = s.lines().filter(|l| !l.starts_with('⬆')).collect();
    if kept.is_empty() {
        String::new()
    } else {
        format!("{}\n", kept.join("\n"))
    }
}

/// Replaces every spelling of `root` with `<TMP>`.
fn mask_tmp(s: &str, root: &Path) -> String {
    let canon = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    s.replace(&canon.display().to_string(), "<TMP>")
        .replace(&root.display().to_string(), "<TMP>")
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

/// `git init`, then one commit of everything.
fn git_commit_all(root: &Path) {
    run_git(root, &["init", "-q"]);
    run_git(root, &["config", "user.name", "sieve-fixture"]);
    run_git(root, &["config", "user.email", "fixture@sieve.local"]);
    run_git(root, &["add", "-A"]);
    run_git(root, &["commit", "-q", "-m", "base"]);
}

fn golden_dir() -> PathBuf {
    support::manifest_dir().join("../../tests/fixtures/basic.expected/build-fresh")
}

/// Copies the `basic` fixture into a fresh scratch dir.
fn fresh_copy(label: &str) -> TempDir {
    let fixture = support::manifest_dir().join("../../tests/fixtures/basic");
    let temp = TempDir::new(&format!("build-fresh-{label}"));
    copy_dir(&fixture, &temp.path);
    temp
}

/// Runs one `sieve` command in `root`, with `envs` set, and returns its
/// masked stdout, normalized stderr and exit code.
fn run_sieve(root: &Path, args: &[&str], envs: &[(&str, &str)]) -> (String, String, i32) {
    let output = support::sieve_command()
        .args(args)
        .envs(envs.iter().copied())
        .current_dir(root)
        .output()
        .expect("run sieve");
    (
        mask_tmp(&String::from_utf8_lossy(&output.stdout), root),
        normalize_stderr(&mask_tmp(&String::from_utf8_lossy(&output.stderr), root)),
        output.status.code().unwrap_or(-1),
    )
}

/// Builds `root` with `sieve` and asserts that the build succeeded.
fn build_quiet(root: &Path) {
    let built = run_sieve(root, &["build"], &[]);
    assert_eq!(built.2, 0, "sieve build failed: {}", built.1);
}

/// Reads the golden triple for one id.
fn golden(id: &str) -> (String, String, i32) {
    let read = |suffix: &str| {
        fs::read_to_string(golden_dir().join(format!("{id}.{suffix}.txt")))
            .unwrap_or_else(|e| panic!("read golden build-fresh/{id}.{suffix}.txt: {e}"))
    };
    let exit = read("exit").trim().parse::<i32>().expect("parse exit");
    (read("stdout"), normalize_stderr(&read("stderr")), exit)
}

/// Asserts that one command's stdout, stderr and exit match the golden.
fn assert_matches_golden(id: &str, got: (String, String, i32)) {
    support::golden::bless_triple(&golden_dir(), id, got.0.as_bytes(), got.1.as_bytes(), got.2);
    let (want_out, want_err, want_exit) = golden(id);
    assert_eq!(got.0, want_out, "build-fresh {id}: stdout mismatch");
    assert_eq!(got.1, want_err, "build-fresh {id}: stderr mismatch");
    assert_eq!(got.2, want_exit, "build-fresh {id}: exit mismatch");
}

/// Asserts that the copy's `wiring.json` matches `<id>.wiring.json`.
fn assert_wiring_matches_golden(id: &str, root: &Path) {
    let got = fs::read(root.join("sieve/.graph/wiring.json")).expect("read wiring.json");
    if support::golden::blessing() {
        support::golden::bless(&golden_dir().join(format!("{id}.wiring.json")), &got);
    }
    let want =
        fs::read(golden_dir().join(format!("{id}.wiring.json"))).expect("read golden wiring");
    assert_eq!(
        String::from_utf8_lossy(&got),
        String::from_utf8_lossy(&want),
        "build-fresh {id}: wiring.json mismatch"
    );
}

/// `double` becomes `dubble` in `src/util.ts`, same byte count, with the
/// file's mtime put back.
fn same_size_edit(root: &Path) {
    let path = root.join("src/util.ts");
    let mtime = fs::metadata(&path)
        .expect("stat util.ts")
        .modified()
        .expect("read mtime");
    let text = fs::read_to_string(&path).expect("read util.ts");
    let edited = text.replace("double", "dubble");
    assert_eq!(
        text.len(),
        edited.len(),
        "the edit must keep the byte count"
    );
    fs::write(&path, edited).expect("write util.ts");
    fs::File::options()
        .write(true)
        .open(&path)
        .expect("open util.ts")
        .set_modified(mtime)
        .expect("restore mtime");
}

/// A `chmod 000` that puts the mode back on drop, so the scratch dir can
/// be removed on a pass or on a panic.
#[cfg(unix)]
struct Locked {
    path: PathBuf,
    restore: u32,
}

#[cfg(unix)]
impl Locked {
    fn new(path: &Path, restore: u32) -> Self {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o000)).expect("chmod 000");
        Locked {
            path: path.to_path_buf(),
            restore,
        }
    }
}

#[cfg(unix)]
impl Drop for Locked {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&self.path, fs::Permissions::from_mode(self.restore));
    }
}

/// P2-35: a `touch` on an unchanged file replays it from the cache,
/// because the build hashes every file and compares the hash, not the
/// stat. The `parsed:` line reads `0 of 7 files (7 replayed from cache)`.
#[test]
fn test_p2_35_touch_replays_every_file_like_golden() {
    let temp = fresh_copy("touch");
    git_commit_all(&temp.path);
    build_quiet(&temp.path);
    fs::File::options()
        .write(true)
        .open(temp.path.join("src/util.ts"))
        .expect("open util.ts")
        .set_modified(SystemTime::now())
        .expect("touch util.ts");
    let built = run_sieve(&temp.path, &["build"], &[]);
    assert_matches_golden("touch", built);
}

/// P2-35: a same-size edit with the mtime put back re-parses the file.
/// The `parsed:` line counts one parse, and `wiring.json` holds the new
/// `dubble` name, byte for byte as sieve writes it.
#[test]
fn test_p2_35_same_size_edit_with_mtime_restored_reparses_like_golden() {
    let temp = fresh_copy("same-size");
    git_commit_all(&temp.path);
    build_quiet(&temp.path);
    same_size_edit(&temp.path);
    let built = run_sieve(&temp.path, &["build"], &[]);
    assert_matches_golden("same-size", built);
    assert_wiring_matches_golden("same-size", &temp.path);
}

/// P1-46: `SIEVE_REFRESH=hash` on the same edit refreshes the graph once,
/// with the note on stderr, and the second run prints no note.
#[test]
fn test_p1_46_refresh_hash_rebuilds_once_like_golden() {
    let temp = fresh_copy("hash-refresh");
    git_commit_all(&temp.path);
    build_quiet(&temp.path);
    same_size_edit(&temp.path);
    let envs = [("SIEVE_REFRESH", "hash")];
    let first = run_sieve(&temp.path, &["grep", "dubble"], &envs);
    assert_matches_golden("hash-refresh-1", first);
    let second = run_sieve(&temp.path, &["grep", "dubble"], &envs);
    assert_matches_golden("hash-refresh-2", second);
}

/// P2-01: an unreadable file prints Node's `EACCES` `open` line on
/// stderr, the build exits 0, `wiring.json` has no node for the file, and
/// the fingerprint records the file with an empty hash.
#[cfg(unix)]
#[test]
fn test_p2_01_unreadable_file_is_reported_not_fatal_like_golden() {
    let temp = fresh_copy("unreadable-file");
    let x = temp.path.join("src/x.ts");
    fs::write(&x, "export function hidden(): number {\n  return 1;\n}\n").expect("write x.ts");
    git_commit_all(&temp.path);
    let built = {
        let _locked = Locked::new(&x, 0o644);
        run_sieve(&temp.path, &["build"], &[])
    };
    assert_matches_golden("unreadable-file", built);
    assert_wiring_matches_golden("unreadable-file", &temp.path);

    let cache_dir = temp.path.join("sieve/.cache");
    let fingerprint = fs::read_dir(&cache_dir)
        .expect("read cache dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("fingerprint."))
        })
        .expect("a fingerprint sidecar");
    let fp: serde_json::Value =
        serde_json::from_slice(&fs::read(&fingerprint).expect("read fingerprint"))
            .expect("parse fingerprint");
    assert_eq!(
        fp["files"]["src/x.ts"][2],
        serde_json::Value::String(String::new()),
        "the unreadable file keeps an empty hash in the fingerprint"
    );
}

/// P2-01: with no git, an unreadable dir fails the build with Node's
/// `EACCES` `scandir` line, no `✗` prefix, and exit 1.
#[cfg(unix)]
#[test]
fn test_p2_01_unreadable_dir_with_no_git_fails_like_golden() {
    let temp = fresh_copy("unreadable-dir");
    let locked = temp.path.join("src/locked");
    fs::create_dir_all(&locked).expect("create locked dir");
    fs::write(
        locked.join("l.ts"),
        "export function locked(): number {\n  return 1;\n}\n",
    )
    .expect("write l.ts");
    let built = {
        let _locked = Locked::new(&locked, 0o755);
        run_sieve(&temp.path, &["build"], &[])
    };
    assert_matches_golden("unreadable-dir", built);
}
