//! Parity tests for `stats` (P1-64), `telemetry` (P1-67) and the hidden
//! `_update-check`, `_telemetry-flush` and `_brain-refresh` commands (P1-68).
//! Goldens from recorded runs under
//! `tests/fixtures/basic.expected/stats-telemetry/`. Each test replays the same
//! command sequence, in the same scratch-home state, and compares stdout,
//! stderr and the exit code.
//!
//! Every `HOME` here is a scratch `TempDir`. No test reads or writes the
//! real home.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use support::TempDir;

const ONE: &str = r#"{"lastQuery":"ask bigwidget","perAgentQuery":{},"toolReads":3,"sourceReads":1,"savedTokens":12345,"injectedPointers":[],"nudges":0,"inputCostMicros":5000000,"inputTokensBilled":1000000}"#;
const ZERO: &str = r#"{"lastQuery":null,"perAgentQuery":{},"toolReads":0,"sourceReads":0,"savedTokens":0,"injectedPointers":[],"nudges":0}"#;
const SUBCENT: &str = r#"{"toolReads":1,"sourceReads":0,"savedTokens":100,"inputCostMicros":5000000,"inputTokensBilled":1000000,"lastQuery":"grep x"}"#;

fn golden_dir() -> PathBuf {
    support::manifest_dir().join("../../tests/fixtures/basic.expected/stats-telemetry")
}

fn read_golden(id: &str, stream: &str) -> String {
    let path = golden_dir().join(format!("{id}.{stream}.txt"));
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read golden {}: {e}", path.display()))
}

/// Copies `src` into `dst`, recursively, including dot-directories.
fn copy_dir(src: &Path, dst: &Path) {
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

/// A scratch home with the seeded update cache
/// one, so no run prints the update nudge.
fn scratch_home(label: &str) -> TempDir {
    let home = TempDir::new(&format!("st-home-{label}"));
    fs::create_dir_all(home.join(".sieve")).expect("create .sieve");
    fs::write(
        home.join(".sieve/update-check.json"),
        r#"{"latest":"0.21.1","checkedAt":9999999999999}"#,
    )
    .expect("seed update cache");
    home
}

/// A fresh copy of the built basic fixture, with one `x.json` session
/// file when `session` is not empty.
fn session_copy(built: &Path, label: &str, session: &str) -> TempDir {
    let copy = TempDir::new(&format!("st-copy-{label}"));
    copy_dir(built, &copy.path);
    if !session.is_empty() {
        let dir = copy.join("sieve/.cache/session");
        fs::create_dir_all(&dir).expect("create session dir");
        fs::write(dir.join("x.json"), session).expect("write session");
    }
    copy
}

/// Builds the basic fixture once, in a temp copy.
fn build_basic() -> TempDir {
    let fixture = support::manifest_dir().join("../../tests/fixtures/basic");
    let built = TempDir::new("st-built");
    copy_dir(&fixture, &built.path);
    let output = support::sieve_command()
        .args(["build", "."])
        .current_dir(&built.path)
        .output()
        .expect("run sieve build");
    assert_eq!(output.status.code(), Some(0), "sieve build failed");
    built
}

fn run(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    let mut cmd = support::sieve_command();
    cmd.args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("NO_COLOR", "1")
        .env_remove("SIEVE_DIR");
    cmd.env("DO_NOT_TRACK", "1");
    cmd.output().expect("run sieve")
}

/// Drops every `⬆` update-nudge line.
fn drop_nudge(s: &str) -> String {
    s.lines()
        .filter(|l| !l.starts_with('\u{2b06}'))
        .map(|l| format!("{l}\n"))
        .collect()
}

/// Runs one case and compares it with its golden.
fn check(id: &str, home: &Path, cwd: &Path, args: &[&str]) {
    let output = run(home, cwd, args);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = drop_nudge(&String::from_utf8_lossy(&output.stderr));
    support::golden::bless_triple(
        &golden_dir(),
        id,
        stdout.as_bytes(),
        stderr.as_bytes(),
        output.status.code().unwrap_or(-1),
    );
    assert_eq!(stdout, read_golden(id, "stdout"), "{id}: stdout");
    assert_eq!(stderr, read_golden(id, "stderr"), "{id}: stderr");
    assert_eq!(
        output
            .status
            .code()
            .map_or("none".to_string(), |c| c.to_string()),
        read_golden(id, "exit").trim(),
        "{id}: exit"
    );
}

#[test]
fn test_p1_64_stats_matches_golden() {
    let built = build_basic();
    let home = scratch_home("stats");
    let none = session_copy(&built, "none", "");
    check("stats-none", &home, &none, &["stats"]);
    check("stats-none-json", &home, &none, &["stats", "--json"]);
    let one = session_copy(&built, "one", ONE);
    check("stats-one", &home, &one, &["stats"]);
    check("stats-one-json", &home, &one, &["stats", "--json"]);
    let one_path = one.path.to_str().expect("utf-8 path");
    check("stats-dirarg", &home, &home, &["stats", one_path]);
    let zero = session_copy(&built, "zero", ZERO);
    check("stats-zero", &home, &zero, &["stats"]);
    check("stats-zero-json", &home, &zero, &["stats", "--json"]);
    let bad = session_copy(&built, "bad", "{not json");
    check("stats-bad", &home, &bad, &["stats"]);
    check("stats-bad-json", &home, &bad, &["stats", "--json"]);
    let subcent = session_copy(&built, "subcent", SUBCENT);
    check("stats-subcent", &home, &subcent, &["stats"]);
    assert!(
        !home.join(".sieve/telemetry.json").exists(),
        "stats must not write the telemetry state"
    );
}

#[test]
fn test_p1_68_hidden_commands_match_sieve_and_touch_no_file() {
    let home = scratch_home("hidden");
    let cwd = TempDir::new("hidden-cwd");
    let cache = home.join(".sieve/update-check.json");
    let before = fs::read_to_string(&cache).expect("read seeded cache");
    check("hidden-telemetry-flush", &home, &cwd, &["_telemetry-flush"]);
    check("hidden-brain-refresh", &home, &cwd, &["_brain-refresh"]);
    check("hidden-update-check", &home, &cwd, &["_update-check"]);
    assert_eq!(
        fs::read_to_string(&cache).expect("read cache"),
        before,
        "_update-check must not write the update cache without the registry"
    );
    let names: Vec<String> = fs::read_dir(home.join(".sieve"))
        .expect("read .sieve")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        ["update-check.json"],
        "no hidden command writes under HOME"
    );
}

/// A product text could claim a running telemetry pipeline. Sieve records and
/// sends nothing, so every action prints one true line, exits 0, and writes
/// nothing under `HOME`.
#[test]
fn test_p1_67_telemetry_under_the_sieve_name_prints_one_true_line() {
    let home = TempDir::new("tel-sieve-home");
    let cwd = TempDir::new("tel-sieve-cwd");
    let actions: [&[&str]; 5] = [
        &["telemetry"],
        &["telemetry", "status"],
        &["telemetry", "enable"],
        &["telemetry", "disable"],
        &["telemetry", "debug"],
    ];
    for args in actions {
        let output = support::sieve_command()
            .args(args)
            .current_dir(&cwd.path)
            .env("HOME", &home.path)
            .env_remove("DO_NOT_TRACK")
            .output()
            .expect("run sieve");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "telemetry: off — sieve records and sends nothing\n",
            "{args:?}: stdout"
        );
        assert_eq!(output.status.code(), Some(0), "{args:?}: exit");
        let entries: Vec<_> = fs::read_dir(&home.path)
            .expect("read home")
            .map(|e| e.expect("entry").file_name())
            .collect();
        assert!(entries.is_empty(), "{args:?} wrote under HOME: {entries:?}");
    }
    let output = support::sieve_command()
        .args(["telemetry", "bogus"])
        .current_dir(&cwd.path)
        .env("HOME", &home.path)
        .output()
        .expect("run sieve");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "sieve: unknown action bogus \u{2014} use status, enable, disable, or debug\n"
    );
    assert_eq!(output.status.code(), Some(1));
}

/// The clock seam: with `SIEVE_TEST_NOW` at 2026-11-30, `stats --json` holds
/// the seven days that end on 2026-11-30, and no October date. The goldens
/// above use the fixed clock of `support::sieve_command`, so they pass on any
/// day.
#[test]
fn test_stats_json_dates_follow_the_test_clock() {
    let built = build_basic();
    let home = scratch_home("clock");
    let copy = session_copy(&built, "clock", ONE);
    let out = support::sieve_command()
        .args(["stats", "--json"])
        .current_dir(&copy.path)
        .env("HOME", &home.path)
        .env("SIEVE_TEST_NOW", "1796040000")
        .output()
        .expect("run sieve stats");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("2026-11-30") && text.contains("2026-11-24"),
        "{text}"
    );
    assert!(!text.contains("2026-10-"), "{text}");
}
