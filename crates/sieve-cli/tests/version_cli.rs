//! Tests for `sieve version` (P1-37, P1-38). Sieve prints no update nudge
//! (P1-47), and these tests pin the silence.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A temp dir that removes itself on drop.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("sieve-version-cli-{label}-{pid}-{n}"));
        fs::create_dir_all(&path).expect("create temp dir");
        TempDir { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

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

/// Runs `sieve <args>` in `root`, with `$HOME` pinned to `home` — never
/// the real home dir, so no real `~/.sieve/update-check.json` leaks in.
fn run_sieve_with_home(root: &Path, home: &Path, args: &[&str]) -> Output {
    support::sieve_command()
        .args(args)
        .current_dir(root)
        .env("HOME", home)
        .output()
        .expect("run sieve")
}

/// Writes `~/.sieve/update-check.json` with the given `latest` value
/// (`None` writes no file at all: the "no cache" branch).
fn seed_update_cache(home: &Path, latest: Option<&str>) {
    if let Some(latest) = latest {
        let dir = home.join(".sieve");
        fs::create_dir_all(&dir).expect("create .sieve dir");
        fs::write(
            dir.join("update-check.json"),
            format!(r#"{{"latest":"{latest}","checkedAt":0}}"#),
        )
        .expect("write update-check.json");
    }
}

#[test]
fn test_p1_38_version_never_prints_the_update_nudge() {
    let temp = TempDir::new("exempt");
    let home = TempDir::new("exempt-home");
    // A newer cached version would nudge every other command; `version`
    // stays silent on stderr regardless (P1-38).
    seed_update_cache(&home.path, Some("9.9.9"));

    let output = run_sieve_with_home(&temp.path, &home.path, &["version"]);

    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
}

/// P1-47: `sieve init` calls `build::run` under the hood, but `init` is
/// exempt from the nudge (Sieve's own exempt set adds `init`,
/// `uninstall`, `hook` and `statusline` to the `UPKEEP_SKIP`). A
/// newer cached version must not leak a nudge line into `init`'s stderr.
#[test]
fn test_p1_47_init_stays_silent_even_though_it_calls_build() {
    let fixture = support::manifest_dir().join("../../tests/fixtures/basic");
    let temp = TempDir::new("nudge-init");
    copy_dir(&fixture, &temp.path);
    let home = TempDir::new("nudge-init-home");
    seed_update_cache(&home.path, Some("9.9.9"));

    let output = support::sieve_command()
        .args(["init", "--yes"])
        .current_dir(&temp.path)
        .env("HOME", &home.path)
        .output()
        .expect("run sieve init");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.lines().any(|line| line.starts_with('\u{2b06}')),
        "init must never print the update nudge, got: {stderr:?}"
    );
}

/// P1-47/P4-57: `mcp` sits inside the CLI `UPKEEP_SKIP` set, so the
/// CLI preAction prints no nudge for it. The MCP server prints its own
/// banner at boot; `test_p4_57_*` in `mcp_cli.rs` pins that. Here the
/// cache is current, so `mcp` stays silent. `mcp` serves stdin until
/// EOF; an empty stdin ends the session immediately.
#[test]
fn test_p1_47_mcp_stays_silent() {
    let temp = TempDir::new("nudge-mcp");
    let home = TempDir::new("nudge-mcp-home");
    seed_update_cache(&home.path, Some("0.21.1"));

    let output = support::sieve_command()
        .args(["mcp"])
        .current_dir(&temp.path)
        .env("HOME", &home.path)
        .output()
        .expect("run sieve mcp");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.lines().any(|line| line.starts_with('\u{2b06}')),
        "mcp must print no nudge on a current cache, got: {stderr:?}"
    );
}

/// P1-47: `hook` and `statusline` read their own canned JSON from stdin
/// and never call the query prelude, so they stay silent on the nudge
/// too.
#[test]
fn test_p1_47_hook_and_statusline_stay_silent() {
    let temp = TempDir::new("nudge-hook");
    let home = TempDir::new("nudge-hook-home");
    seed_update_cache(&home.path, Some("9.9.9"));

    let hook_output = support::sieve_command()
        .args(["hook", "stop"])
        .current_dir(&temp.path)
        .env("HOME", &home.path)
        .env("SIEVE_TEST_STDIN", "{}")
        .output()
        .expect("run sieve hook stop");
    let hook_stderr = String::from_utf8_lossy(&hook_output.stderr);
    assert!(!hook_stderr.lines().any(|line| line.starts_with('\u{2b06}')));

    let statusline_output = support::sieve_command()
        .args(["statusline"])
        .current_dir(&temp.path)
        .env("HOME", &home.path)
        .output()
        .expect("run sieve statusline");
    let statusline_stderr = String::from_utf8_lossy(&statusline_output.stderr);
    assert!(!statusline_stderr
        .lines()
        .any(|line| line.starts_with('\u{2b06}')));
}

/// P4-41: `sieve hook session-start` never panics and prints no nudge
/// when the cache is missing (a fresh machine, or `$HOME` unset).
#[test]
fn test_p4_41_hook_session_start_stays_silent_without_a_cache() {
    let temp = TempDir::new("hook-no-cache");
    let home = TempDir::new("hook-no-cache-home");

    let output = support::sieve_command()
        .args(["hook", "session-start"])
        .current_dir(&temp.path)
        .env("HOME", &home.path)
        .env("SIEVE_TEST_STDIN", "{}")
        .output()
        .expect("run sieve hook session-start");

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains('\u{2b06}'), "got: {stdout:?}");
}

/// P4-41: `sieve statusline` never runs the update upkeep (only the
/// `session-start` hook does), so it prints no nudge even from a newer cached
/// version.
#[test]
fn test_p4_41_statusline_never_prints_the_nudge() {
    let temp = TempDir::new("statusline-nudge");
    let home = TempDir::new("statusline-nudge-home");
    seed_update_cache(&home.path, Some("9.9.9"));

    let output = support::sieve_command()
        .args(["statusline"])
        .current_dir(&temp.path)
        .env("HOME", &home.path)
        .output()
        .expect("run sieve statusline");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stdout.contains('\u{2b06}'), "got: {stdout:?}");
    assert!(!stderr.contains('\u{2b06}'), "got: {stderr:?}");
}

/// Runs `sieve <args>` with a seeded update cache in a scratch HOME.
fn run_under(args: &[&str]) -> Output {
    let temp = TempDir::new("product-cwd");
    let home = TempDir::new("product-home");
    seed_update_cache(&home.path, Some("9.9.9"));
    support::sieve_command()
        .args(args)
        .env("HOME", &home.path)
        .current_dir(&temp.path)
        .output()
        .expect("run sieve")
}

#[test]
fn test_version_cli_sieve_prints_the_crate_version() {
    let crate_version = env!("CARGO_PKG_VERSION");
    let flag = run_under(&["--version"]);
    assert_eq!(
        String::from_utf8_lossy(&flag.stdout),
        format!("{crate_version}\n")
    );
    let sub = run_under(&["version"]);
    assert_eq!(
        String::from_utf8_lossy(&sub.stdout),
        format!("sieve {crate_version}\n")
    );
}

#[test]
fn test_version_cli_sieve_prints_no_upgrade_nudge() {
    let sieve = run_under(&["check"]);
    assert!(!String::from_utf8_lossy(&sieve.stderr).contains("available"));
}
