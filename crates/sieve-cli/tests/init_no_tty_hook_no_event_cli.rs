//! P4-58: bare `init` with no flag and no TTY prints the recorded help
//! and writes nothing. P4-59: `hook` with no event,
//! or an unknown event, exits 0 with no output (`main`).
//! The expected bytes come from `sieve init </dev/null` in scratch repos.

mod support;

use std::fs;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use support::TempDir;

fn run(cmd: &mut Command, home: &Path, repo: &Path) -> Output {
    cmd.current_dir(repo)
        .env("HOME", home)
        .env("SIEVE_NO_TELEMETRY", "1")
        .env_remove("SIEVE_DIR")
        .stdin(Stdio::null())
        .output()
        .expect("run sieve")
}

fn bare_init(markers_home: &[&str], markers_repo: &[&str]) -> (Output, TempDir) {
    let t = TempDir::new("p458");
    let home = t.join("home");
    let repo = t.join("repo");
    fs::create_dir_all(&home).expect("home");
    fs::create_dir_all(&repo).expect("repo");
    for m in markers_home {
        fs::create_dir_all(home.join(m)).expect("home marker");
    }
    for m in markers_repo {
        fs::create_dir_all(repo.join(m)).expect("repo marker");
    }
    let mut cmd = support::sieve_command();
    cmd.arg("init");
    let out = run(&mut cmd, &home, &repo);
    (out, t)
}

fn assert_help(out: &Output, t: &TempDir, expected: &str) {
    assert_eq!(String::from_utf8_lossy(&out.stderr), expected);
    assert!(out.stdout.is_empty());
    assert_eq!(out.status.code(), Some(0));
    let n = fs::read_dir(t.join("repo")).expect("read repo").count();
    let marker = ".github";
    let kept = usize::from(t.join("repo").join(marker).is_dir());
    assert_eq!(n, kept, "init wrote a file");
}

const HEAD: &str =
    "sieve init: no TTY to prompt on, and no --agents/--yes given \u{2014} nothing written.\n";

#[test]
fn test_p4_58_bare_init_no_tty_claude_only() {
    let (out, t) = bare_init(&[], &[]);
    let want = format!(
        "{HEAD}detected: claude\n\n  sieve init --agents claude   # wire these\n  sieve init --yes             # same, without spelling them out\n  sieve init --agents claude   # Claude Code only\n  sieve init --dry-run         # list every file first\n"
    );
    assert_help(&out, &t, &want);
}

#[test]
fn test_p4_58_bare_init_no_tty_several_hosts() {
    let (out, t) = bare_init(&[".cursor", ".gemini", ".codex"], &[".github"]);
    let want = format!(
        "{HEAD}detected: claude, agents, cursor, gemini, copilot\n\n  sieve init --agents claude agents cursor gemini copilot   # wire these\n  sieve init --yes                                          # same, without spelling them out\n  sieve init --agents claude                                # Claude Code only\n  sieve init --dry-run                                      # list every file first\n"
    );
    assert_help(&out, &t, &want);
}

#[test]
fn test_p4_58_bare_init_no_tty_sieve_brand() {
    let t = TempDir::new("p458b");
    let (home, repo) = (t.join("home"), t.join("repo"));
    fs::create_dir_all(&home).expect("home");
    fs::create_dir_all(&repo).expect("repo");
    let mut cmd = support::sieve_command();
    cmd.arg("init");
    let out = run(&mut cmd, &home, &repo);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.starts_with("sieve init: no TTY to prompt on"), "{err}");
    assert!(err.contains("  sieve init --dry-run "), "{err}");
}

fn hook(args: &[&str]) -> Output {
    let t = TempDir::new("p459");
    let (home, repo) = (t.join("home"), t.join("repo"));
    fs::create_dir_all(&home).expect("home");
    fs::create_dir_all(&repo).expect("repo");
    let mut cmd = support::sieve_command();
    cmd.arg("hook").args(args);
    run(&mut cmd, &home, &repo)
}

#[test]
fn test_p4_59_hook_no_event_exits_zero_silent() {
    let out = hook(&[]);
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty());
    assert!(out.stderr.is_empty());
}

#[test]
fn test_p4_59_hook_unknown_event_exits_zero_silent() {
    let out = hook(&["no-such-event"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty());
    assert!(out.stderr.is_empty());
}
