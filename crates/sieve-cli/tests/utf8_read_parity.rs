//! Parity tests for a source file that is not valid UTF-8 (P3-26), and
//! for the files `check` writes (P1-35), against recorded goldens
//! under `tests/fixtures/edges.expected/utf8-read/`.
//!
//! `tests/inputs/utf8-read/setup.sh seed` writes two TypeScript files with
//! a Latin-1 `0xE9` in a comment, a string, a call-site line and a
//! function name, plus a lead byte `0xC3` cut off by a line end. Sieve
//! decodes each file with `readFileSync(path, "utf8")`, so every invalid
//! byte becomes U+FFFD and the file keeps its symbols. Each test copies the
//! `edges` fixture, seeds the files, commits, builds with `sieve`, runs
//! one command, then byte-compares stdout, stderr and the exit code with
//! the golden.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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

fn setup_script() -> PathBuf {
    support::manifest_dir().join("../../tests/inputs/utf8-read/setup.sh")
}

fn golden_dir() -> PathBuf {
    support::manifest_dir().join("../../tests/fixtures/edges.expected/utf8-read")
}

/// Runs `setup.sh <mode>` in `root`.
fn run_setup(root: &Path, mode: &str) {
    let status = Command::new("bash")
        .arg(setup_script())
        .arg(mode)
        .current_dir(root)
        .status()
        .expect("run setup.sh");
    assert!(status.success(), "setup.sh {mode} failed");
}

/// Runs one `sieve` command in `root` and returns its masked stdout,
/// normalized stderr and exit code.
fn run_sieve(root: &Path, args: &[&str]) -> (String, String, i32) {
    let output = support::sieve_command()
        .args(args)
        .current_dir(root)
        .output()
        .expect("run sieve");
    (
        mask_tmp(&String::from_utf8_lossy(&output.stdout), root),
        normalize_stderr(&mask_tmp(&String::from_utf8_lossy(&output.stderr), root)),
        output.status.code().unwrap_or(-1),
    )
}

/// Reads the golden triple for one id.
fn golden(id: &str) -> (String, String, i32) {
    let read = |suffix: &str| {
        fs::read_to_string(golden_dir().join(format!("{id}.{suffix}.txt")))
            .unwrap_or_else(|e| panic!("read golden utf8-read/{id}.{suffix}.txt: {e}"))
    };
    let exit = read("exit").trim().parse::<i32>().expect("parse exit");
    (read("stdout"), normalize_stderr(&read("stderr")), exit)
}

/// Asserts that one command's stdout, stderr and exit match the golden.
fn assert_matches_golden(id: &str, got: (String, String, i32)) {
    support::golden::bless_triple(&golden_dir(), id, got.0.as_bytes(), got.1.as_bytes(), got.2);
    let (want_out, want_err, want_exit) = golden(id);
    assert_eq!(got.0, want_out, "utf8-read {id}: stdout mismatch");
    assert_eq!(got.1, want_err, "utf8-read {id}: stderr mismatch");
    assert_eq!(got.2, want_exit, "utf8-read {id}: exit mismatch");
}

/// Copies the `edges` fixture, seeds the non-UTF-8 files, commits, then
/// builds. Returns the copy and the build's masked output triple.
fn build_seeded(label: &str) -> (TempDir, (String, String, i32)) {
    let fixture = support::manifest_dir().join("../../tests/fixtures/edges");
    let temp = TempDir::new(&format!("utf8-read-{label}"));
    copy_dir(&fixture, &temp.path);
    run_setup(&temp.path, "seed");
    run_git(&temp.path, &["init", "-q"]);
    run_git(&temp.path, &["config", "user.name", "sieve-fixture"]);
    run_git(&temp.path, &["config", "user.email", "fixture@sieve.local"]);
    run_git(&temp.path, &["add", "-A"]);
    run_git(&temp.path, &["commit", "-q", "-m", "base"]);
    let built = run_sieve(&temp.path, &["build"]);
    assert_eq!(built.2, 0, "sieve build failed: {}", built.1);
    (temp, built)
}

/// P3-26: the build keeps a file that is not valid UTF-8. The node count
/// line and `wiring.json` match the golden byte for byte, so U+FFFD leaves
/// every span, `chars` count and body hash where the golden puts them.
#[test]
fn test_p3_26_utf8_build_keeps_a_latin1_file_like_golden() {
    let (temp, built) = build_seeded("build");
    assert_matches_golden("build", built);
    let got = fs::read(temp.path.join("sieve/.graph/wiring.json")).expect("read wiring.json");
    let want = fs::read(golden_dir().join("wiring.json")).expect("read golden wiring.json");
    assert_eq!(
        String::from_utf8_lossy(&got),
        String::from_utf8_lossy(&want),
        "utf8-read: wiring.json mismatch"
    );
}

/// P3-26: `skeleton` lists the symbols of a Latin-1 file, and prints a
/// function name that holds U+FFFD.
#[test]
fn test_p3_26_utf8_skeleton_matches_golden() {
    let (temp, _) = build_seeded("skeleton");
    assert_matches_golden(
        "skeleton",
        run_sieve(&temp.path, &["skeleton", "ts/latin.ts"]),
    );
    assert_matches_golden(
        "skeleton-name",
        run_sieve(&temp.path, &["skeleton", "ts/latin-name.ts"]),
    );
}

/// P3-26: `grep` searches the decoded text and quotes a line with U+FFFD.
#[test]
fn test_p3_26_utf8_grep_matches_golden() {
    let (temp, _) = build_seeded("grep");
    assert_matches_golden("grep", run_sieve(&temp.path, &["grep", "latinString"]));
}

/// P3-26: `callers` quotes a call-site line that holds an invalid byte.
#[test]
fn test_p3_26_utf8_callers_quotes_a_latin1_line_like_golden() {
    let (temp, _) = build_seeded("callers");
    assert_matches_golden(
        "callers",
        run_sieve(&temp.path, &["callers", "latinString"]),
    );
}

/// P3-26: `ask --source` prints the source slice with U+FFFD, and finds a
/// symbol whose name holds U+FFFD.
#[test]
fn test_p3_26_utf8_ask_source_matches_golden() {
    let (temp, _) = build_seeded("ask");
    assert_matches_golden(
        "ask-source",
        run_sieve(&temp.path, &["ask", "latinString", "--source"]),
    );
    assert_matches_golden(
        "ask-name",
        run_sieve(&temp.path, &["ask", "caf", "--source"]),
    );
}

/// P3-26: after a `touch`, the freshness probe re-hashes the decoded text
/// and finds no drift, so the query prints no refresh note.
#[test]
fn test_p3_26_utf8_touched_latin1_file_is_not_drift() {
    let (temp, _) = build_seeded("touched");
    let path = temp.path.join("ts/latin.ts");
    let bytes = fs::read(&path).expect("read latin.ts");
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(&path, bytes).expect("rewrite latin.ts");
    assert_matches_golden(
        "skeleton-touched",
        run_sieve(&temp.path, &["skeleton", "ts/latin.ts"]),
    );
}

/// P3-26, DV7: `blast` seeds the function a Latin-1 edit adds.
#[test]
fn test_p3_26_dv7_blast_seeds_a_latin1_edit_like_golden() {
    let (temp, _) = build_seeded("blast");
    run_setup(&temp.path, "change");
    run_git(&temp.path, &["add", "ts/latin.ts"]);
    let mut cmd = support::sieve_command();
    let output = cmd
        .args(["blast", "--format", "text", "-d", "1"])
        .current_dir(&temp.path)
        .output()
        .expect("run sieve blast");
    assert_matches_golden(
        "blast",
        (
            mask_tmp(&String::from_utf8_lossy(&output.stdout), &temp.path),
            normalize_stderr(&mask_tmp(
                &String::from_utf8_lossy(&output.stderr),
                &temp.path,
            )),
            output.status.code().unwrap_or(-1),
        ),
    );
}

/// The sorted file names under `dir`, one level deep.
fn file_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .expect("read dir")
        .map(|e| {
            e.expect("dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

/// P1-35: `check` writes no file. It never calls
/// `writeExtractCache`, so a `check` after the extract sidecar is gone
/// leaves the cache dir as it found it.
#[test]
fn test_p1_35_check_writes_no_extract_sidecar() {
    let (temp, _) = build_seeded("check");
    let cache = temp.path.join("sieve/.cache");
    for name in file_names(&cache) {
        if name.starts_with("extract.") {
            fs::remove_file(cache.join(name)).expect("remove extract sidecar");
        }
    }
    let before = file_names(&cache);
    let (_, _, exit) = run_sieve(&temp.path, &["check"]);
    assert_eq!(exit, 0, "check reports a fresh graph");
    assert_eq!(
        file_names(&cache),
        before,
        "check wrote a file into the cache dir"
    );
}
