//! `sieve init` warns when the bare `sieve` command its hook entries run
//! is not on `PATH` (probe of 2026-09-30: a mis-installed hook fails
//! silently). Every test here spawns the binary with its own `PATH` and a
//! scratch `HOME`, never the process's real ones.

mod support;

use std::path::Path;
use std::process::{Command, Output};

use support::TempDir;

const WARNING_START: &str = "⚠ sieve is not on PATH";

/// Runs `sieve init --no-agents --no-build` in `cwd` with the given
/// `PATH` and a scratch `HOME`.
fn run_init(cwd: &Path, home: &Path, path: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sieve"))
        .args(["init", "--no-agents", "--no-build"])
        .current_dir(cwd)
        .env("HOME", home)
        .env("PATH", path)
        .output()
        .expect("run sieve init")
}

/// Builds a dir whose only entry is a `sieve` symlink to the test binary.
fn dir_with_sieve(label: &str) -> TempDir {
    let dir = TempDir::new(label);
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_sieve"), dir.path.join("sieve"))
        .expect("symlink sieve");
    dir
}

#[test]
fn init_warns_writes_files_and_exits_zero_when_sieve_is_absent_from_path() {
    let home = TempDir::new("bin-warn-absent-home");
    let cwd = TempDir::new("bin-warn-absent-cwd");
    let empty_path = TempDir::new("bin-warn-absent-path");

    let output = run_init(&cwd, &home, &empty_path);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "init must exit 0, stderr={stderr}");
    assert!(
        stderr.lines().any(|l| l.starts_with(WARNING_START)),
        "init must warn, stderr={stderr}"
    );
    assert!(
        cwd.path.join(".claude").join("settings.json").is_file(),
        "init must still write settings.json"
    );
}

#[test]
fn init_prints_no_warning_when_sieve_is_on_path() {
    let home = TempDir::new("bin-warn-present-home");
    let cwd = TempDir::new("bin-warn-present-cwd");
    let bin_dir = dir_with_sieve("bin-warn-present-path");

    let output = run_init(&cwd, &home, &bin_dir);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "init must exit 0, stderr={stderr}");
    assert!(
        !stderr.contains("not on PATH"),
        "no warning expected, stderr={stderr}"
    );
}
