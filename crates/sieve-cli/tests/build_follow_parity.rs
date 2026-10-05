//! Parity tests for `build --follow-submodules`, `--no-follow-submodules`,
//! `--follow-nested-repos` and `--no-follow-nested-repos` (P1-69).
//!
//! The expected values come from recorded builds of the same fixture:
//! one repo with `a.ts`, a submodule at `libsub` (`s.ts`) and an
//! untracked nested clone at `nested` (`n.ts`). Each run uses a temp
//! `HOME`, so no test touches the real one.

mod support;

use std::fs;
use std::path::Path;
use std::process::Command;

use support::TempDir;

const CFG_SUB: &str = "{\n  \"followSubmodules\": true\n}";
const CFG_NESTED: &str = "{\n  \"followNestedRepos\": true\n}";
const CFG_BOTH: &str = "{\n  \"followSubmodules\": true,\n  \"followNestedRepos\": true\n}";

fn git(dir: &Path, home: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args([
            "-c",
            "user.name=sieve-fixture",
            "-c",
            "user.email=fixture@sieve.local",
            "-c",
            "protocol.file.allow=always",
        ])
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?}");
}

/// A repo `par` with a submodule `libsub` and an untracked clone `nested`.
fn fixture(base: &TempDir) -> std::path::PathBuf {
    let home = base.path.join("home");
    let sub = base.path.join("sub");
    let par = base.path.join("par");
    for d in [&home, &sub, &par] {
        fs::create_dir_all(d).expect("mkdir");
    }
    git(&sub, &home, &["init", "-q"]);
    fs::write(sub.join("s.ts"), "export function subfn(){return 1}\n").expect("write");
    git(&sub, &home, &["add", "."]);
    git(&sub, &home, &["commit", "-qm", "s"]);
    git(&par, &home, &["init", "-q"]);
    fs::write(par.join("a.ts"), "export function a(){return 1}\n").expect("write");
    git(&par, &home, &["add", "."]);
    git(&par, &home, &["commit", "-qm", "a"]);
    git(&par, &home, &["submodule", "add", "-q", "../sub", "libsub"]);
    git(&par, &home, &["commit", "-qm", "sm"]);
    let nested = par.join("nested");
    fs::create_dir_all(&nested).expect("mkdir");
    git(&nested, &home, &["init", "-q"]);
    fs::write(nested.join("n.ts"), "export function n(){return 1}\n").expect("write");
    git(&nested, &home, &["add", "."]);
    git(&nested, &home, &["commit", "-qm", "n"]);
    par
}

/// Builds `par` with `flags` and returns the stdout `wiring` line, the
/// sorted file-node paths and the config text, if any.
fn build(base: &TempDir, par: &Path, flags: &[&str]) -> (String, Vec<String>, Option<String>) {
    let out = support::sieve_command()
        .arg("build")
        .args(flags)
        .current_dir(par)
        .env("HOME", base.path.join("home"))
        .output()
        .expect("run sieve");
    assert!(out.status.success(), "build {flags:?}");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let line = stdout
        .lines()
        .find(|l| l.starts_with("✓ wiring"))
        .expect("wiring line")
        .to_string();
    let wiring: serde_json::Value =
        serde_json::from_slice(&fs::read(par.join("sieve/.graph/wiring.json")).expect("wiring"))
            .expect("json");
    let mut files: Vec<String> = wiring["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .filter(|n| n["kind"] == "file")
        .map(|n| n["path"].as_str().expect("path").to_string())
        .collect();
    files.sort();
    (
        line,
        files,
        fs::read_to_string(par.join(".sieve/config.json")).ok(),
    )
}

fn case(flags: &[&str], line: &str, files: &[&str], config: Option<&str>) {
    let base = TempDir::new("follow");
    let par = fixture(&base);
    let (got_line, got_files, got_config) = build(&base, &par, flags);
    assert_eq!(got_line, line, "{flags:?} stdout");
    assert_eq!(got_files, files, "{flags:?} files");
    assert_eq!(got_config.as_deref(), config, "{flags:?} config");
}

const ONE: &str = "✓ wiring: 2 nodes (1 file, 1 function), 1 edges, 1 cards [typescript]";
const TWO: &str = "✓ wiring: 4 nodes (2 file, 2 function), 2 edges, 2 cards [typescript]";
const THREE: &str = "✓ wiring: 6 nodes (3 file, 3 function), 3 edges, 3 cards [typescript]";

/// P1-69: with no flag, a submodule and a nested clone stay out.
#[test]
fn test_p1_69_default_excludes_submodule_and_nested() {
    case(&[], ONE, &["a.ts"], None);
}

/// P1-69: each follow flag adds its own child and persists its choice.
#[test]
fn test_p1_69_follow_flags_add_children_and_persist() {
    case(
        &["--follow-submodules"],
        TWO,
        &["a.ts", "libsub/s.ts"],
        Some(CFG_SUB),
    );
    case(
        &["--follow-nested-repos"],
        TWO,
        &["a.ts", "nested/n.ts"],
        Some(CFG_NESTED),
    );
    case(
        &["--follow-nested-repos", "--follow-submodules"],
        THREE,
        &["a.ts", "libsub/s.ts", "nested/n.ts"],
        Some(CFG_BOTH),
    );
}

/// P1-69: a `--no-` flag excludes the child and persists `false`; the
/// last flag of a pair wins.
#[test]
fn test_p1_69_no_follow_flags_exclude_and_persist_false() {
    case(
        &["--no-follow-submodules"],
        ONE,
        &["a.ts"],
        Some("{\n  \"followSubmodules\": false\n}"),
    );
    case(
        &["--no-follow-nested-repos"],
        ONE,
        &["a.ts"],
        Some("{\n  \"followNestedRepos\": false\n}"),
    );
    case(
        &["--follow-submodules", "--no-follow-submodules"],
        ONE,
        &["a.ts"],
        Some("{\n  \"followSubmodules\": false\n}"),
    );
    case(
        &["--no-follow-nested-repos", "--follow-nested-repos"],
        TWO,
        &["a.ts", "nested/n.ts"],
        Some(CFG_NESTED),
    );
}

/// P1-69: a later plain build reads the persisted choice.
#[test]
fn test_p1_69_plain_build_reads_the_persisted_choice() {
    let base = TempDir::new("follow-persist");
    let par = fixture(&base);
    build(&base, &par, &["--follow-submodules"]);
    let (line, files, _) = build(&base, &par, &[]);
    assert_eq!(line, TWO);
    assert_eq!(files, ["a.ts", "libsub/s.ts"]);
}
