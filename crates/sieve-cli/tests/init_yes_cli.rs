//! `sieve init --yes` sets up only the detected agent hosts.
//! Every test spawns the binary with a scratch `HOME`, never the real one.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use support::TempDir;

/// Runs `sieve init <args> --no-build` in `repo` with a scratch `HOME`.
fn run_init(repo: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sieve"))
        .arg("init")
        .args(args)
        .arg("--no-build")
        .current_dir(repo)
        .env("HOME", home)
        .output()
        .expect("run sieve init")
}

/// Makes a repo and a scratch `HOME` side by side.
fn scratch(label: &str) -> (TempDir, PathBuf, PathBuf) {
    let tmp = TempDir::new(label);
    let repo = tmp.join("repo");
    let home = tmp.join("home");
    fs::create_dir_all(&repo).expect("make repo");
    fs::create_dir_all(&home).expect("make home");
    fs::write(repo.join("a.rs"), "fn a() {}\n").expect("write source");
    (tmp, repo, home)
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn test_init_yes_sieve_writes_only_claude_files_with_no_host_marker() {
    let (_tmp, repo, home) = scratch("yes-only-claude");
    let out = run_init(&repo, &home, &["--yes"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(repo.join(".claude").exists());
    for path in [
        "AGENTS.md",
        "opencode.json",
        "GEMINI.md",
        ".gemini/settings.json",
        ".github/copilot-instructions.md",
        ".windsurf/rules/sieve.md",
    ] {
        assert!(!repo.join(path).exists(), "wrote {path}");
    }
    let err = stderr(&out);
    assert!(
        err.contains("skipped")
            && err.contains("(no marker in this repo) \u{2014} add with --agents <name>"),
        "no skipped line: {err}"
    );
}

#[test]
fn test_init_yes_sieve_sets_up_a_detected_host() {
    let (_tmp, repo, home) = scratch("yes-detected");
    fs::create_dir_all(repo.join(".kiro")).expect("repo marker");
    fs::create_dir_all(repo.join(".windsurf")).expect("repo marker");
    let out = run_init(&repo, &home, &["--yes"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(repo.join(".claude").exists());
    assert!(repo.join(".windsurf/rules/sieve.md").exists());
    assert!(!repo.join("GEMINI.md").exists());
    assert!(!repo.join("AGENTS.md").exists());
    let err = stderr(&out);
    let skipped = err
        .lines()
        .find(|l| l.contains("(no marker in this repo)"))
        .expect("skipped line");
    assert!(!skipped.contains("kiro") && !skipped.contains("windsurf"));
    assert!(skipped.contains("gemini"));
}

#[test]
fn test_init_yes_all_agents_still_sets_up_every_host_under_sieve() {
    let (_tmp, repo, home) = scratch("yes-all-agents");
    let out = run_init(&repo, &home, &["--yes", "--all-agents"]);
    assert!(out.status.success(), "{}", stderr(&out));
    for path in ["AGENTS.md", "GEMINI.md", ".github/copilot-instructions.md"] {
        assert!(repo.join(path).exists(), "missing {path}");
    }
    assert!(!stderr(&out).contains("(no marker in this repo)"));
}

/// Makes the HOME markers that `init` detects and the ones it ignores.
fn mark_home(home: &Path) {
    for d in [".gemini", ".codex", ".codeium/windsurf"] {
        fs::create_dir_all(home.join(d)).expect("home marker");
    }
}

#[test]
fn test_init_yes_sieve_ignores_home_markers_and_a_bare_github_dir() {
    let (_tmp, repo, home) = scratch("yes-repo-only");
    mark_home(&home);
    fs::create_dir_all(repo.join(".github/workflows")).expect("ci dir");
    let out = run_init(&repo, &home, &["--yes"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(repo.join(".claude").exists());
    for path in [
        "GEMINI.md",
        "AGENTS.md",
        ".windsurf",
        ".github/copilot-instructions.md",
    ] {
        assert!(!repo.join(path).exists(), "wrote {path}");
    }
}

#[test]
fn test_init_yes_sieve_sets_up_cursor_from_a_repo_marker() {
    let (_tmp, repo, home) = scratch("yes-cursor");
    fs::create_dir_all(repo.join(".cursor")).expect("repo marker");
    let out = run_init(&repo, &home, &["--yes"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(repo.join(".claude").exists());
    let err = stderr(&out);
    let skipped = err
        .lines()
        .find(|l| l.contains("(no marker in this repo)"))
        .expect("skipped line");
    assert!(!skipped.contains("cursor"), "{skipped}");
    assert!(skipped.contains("gemini"), "{skipped}");
}

#[test]
fn test_init_yes_sieve_sets_up_copilot_from_its_instructions_file() {
    let (_tmp, repo, home) = scratch("yes-copilot");
    fs::create_dir_all(repo.join(".github")).expect("github dir");
    fs::write(repo.join(".github/copilot-instructions.md"), "x\n").expect("marker");
    let out = run_init(&repo, &home, &["--yes"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let err = stderr(&out);
    let skipped = err
        .lines()
        .find(|l| l.contains("(no marker in this repo)"))
        .expect("skipped line");
    assert!(!skipped.contains("copilot"), "{skipped}");
}

#[test]
fn test_init_yes_sieve_dry_run_plans_claude_only_and_prints_the_skipped_line() {
    let (_tmp, repo, home) = scratch("yes-dry-run");
    mark_home(&home);
    let out = run_init(&repo, &home, &["--dry-run"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains(".claude"), "{err}");
    assert!(
        !err.contains("GEMINI.md") && !err.contains("AGENTS.md"),
        "{err}"
    );
    assert!(err.contains("(no marker in this repo)"), "{err}");
}

#[test]
fn test_init_yes_sieve_ignores_home_markers_when_the_repo_is_home() {
    let tmp = TempDir::new("yes-repo-is-home");
    let home = tmp.join("home");
    fs::create_dir_all(&home).expect("make home");
    fs::write(home.join("a.rs"), "fn a() {}\n").expect("write source");
    mark_home(&home);
    let out = run_init(&home, &home, &["--yes"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!home.join("GEMINI.md").exists());
    assert!(!home.join("AGENTS.md").exists());
}

#[test]
fn test_init_yes_sieve_with_agents_sets_up_the_named_host_without_a_marker() {
    let (_tmp, repo, home) = scratch("yes-agents");
    let out = run_init(&repo, &home, &["--yes", "--agents", "gemini"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(repo.join("GEMINI.md").exists());
    assert!(!repo.join("AGENTS.md").exists());
    assert!(!stderr(&out).contains("(no marker in this repo)"));
}

#[test]
fn test_init_yes_sieve_with_no_agents_sets_up_claude_only() {
    let (_tmp, repo, home) = scratch("yes-no-agents");
    fs::create_dir_all(repo.join(".cursor")).expect("repo marker");
    let out = run_init(&repo, &home, &["--yes", "--no-agents"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(repo.join(".claude").exists());
    assert!(!repo.join(".cursor/hooks.json").exists());
    assert!(!stderr(&out).contains("(no marker in this repo)"));
}
