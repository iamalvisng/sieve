//! Tests for `build --exclude` and `build --no-exclude`: the excluded path
//! stays out of the index across a build, a refresh and a later edit.

mod support;

use std::fs;
use std::path::Path;
use std::process::Output;

use support::TempDir;

/// Runs `sieve <args>` in `repo` with a temp `HOME`.
fn sieve(base: &TempDir, repo: &Path, args: &[&str]) -> Output {
    support::sieve_command()
        .args(args)
        .current_dir(repo)
        .env("HOME", base.path.join("home"))
        .output()
        .expect("run sieve")
}

/// The sorted file-node paths of the repo's index.
fn indexed_files(repo: &Path) -> Vec<String> {
    let wiring: serde_json::Value =
        serde_json::from_slice(&fs::read(repo.join("sieve/.graph/wiring.json")).expect("wiring"))
            .expect("json");
    let mut files: Vec<String> = wiring["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .filter(|n| n["kind"] == "file")
        .map(|n| n["path"].as_str().expect("path").to_string())
        .collect();
    files.sort();
    files
}

fn fixture(base: &TempDir) -> std::path::PathBuf {
    let repo = base.path.join("repo");
    fs::create_dir_all(repo.join("src")).expect("mkdir");
    fs::create_dir_all(repo.join("playground")).expect("mkdir");
    fs::create_dir_all(base.path.join("home")).expect("mkdir");
    fs::write(repo.join("src/a.ts"), "export function a(){return 1}\n").expect("write");
    fs::write(
        repo.join("playground/p.ts"),
        "export function p(){return 1}\n",
    )
    .expect("write");
    repo
}

/// The exclusion holds through a refresh after an edit and after a new file
/// in the excluded folder. `--no-exclude` brings the folder back.
#[test]
fn exclusion_persists_across_a_refresh_and_undoes_with_no_exclude() {
    let base = TempDir::new("exclude");
    let repo = fixture(&base);

    let out = sieve(&base, &repo, &["build", "--exclude", "./playground/"]);
    assert!(out.status.success(), "build");
    assert_eq!(indexed_files(&repo), ["src/a.ts"]);

    fs::write(
        repo.join("playground/p.ts"),
        "export function p(){return 2}\nexport function q(){return 3}\n",
    )
    .expect("edit");
    fs::write(repo.join("playground/new.ts"), "export const n = 1;\n").expect("write");
    let out = sieve(&base, &repo, &["query", "a"]);
    assert!(out.status.success(), "query");
    assert_eq!(indexed_files(&repo), ["src/a.ts"]);

    let out = sieve(&base, &repo, &["build", "--no-exclude", "playground"]);
    assert!(out.status.success(), "build again");
    assert_eq!(
        indexed_files(&repo),
        ["playground/new.ts", "playground/p.ts", "src/a.ts"]
    );
}

/// An absolute path through a symlinked prefix names the same folder as the
/// real path.
#[cfg(unix)]
#[test]
fn an_absolute_path_through_a_symlink_is_inside_the_root() {
    let base = TempDir::new("exclude-link");
    let repo = fixture(&base);
    let link = base.path.join("link");
    std::os::unix::fs::symlink(&repo, &link).expect("symlink");

    let target = link.join("playground");
    let out = sieve(
        &base,
        &repo,
        &["build", "--exclude", target.to_str().expect("utf-8 path")],
    );
    assert!(out.status.success(), "build");
    assert_eq!(indexed_files(&repo), ["src/a.ts"]);
}

/// At a workspace root, `--exclude <child>` leaves that child repo out.
#[test]
fn exclude_at_a_workspace_root_skips_the_named_child() {
    let base = TempDir::new("exclude-workspace");
    let root = base.path.join("ws");
    fs::create_dir_all(base.path.join("home")).expect("mkdir");
    for child in ["a", "b", "c"] {
        let dir = root.join(child);
        fs::create_dir_all(&dir).expect("mkdir");
        fs::write(dir.join("x.ts"), "export function x(){return 1}\n").expect("write");
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .status()
            .expect("run git");
        assert!(status.success(), "git init");
    }

    let out = sieve(&base, &root, &["build", "--exclude", "c"]);
    assert!(out.status.success(), "build");
    let workspace: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("sieve/workspace.json")).expect("workspace"))
            .expect("json");
    assert_eq!(workspace["children"], serde_json::json!(["a", "b"]));
}

/// A path outside the repo root is an error, and the config stays unwritten.
#[test]
fn a_path_outside_the_root_is_an_error() {
    let base = TempDir::new("exclude-outside");
    let repo = fixture(&base);
    let out = sieve(&base, &repo, &["build", "--exclude", "../other"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("inside the repo root"), "{stderr}");
    assert!(!repo.join(".sieve/config.json").exists());
}
