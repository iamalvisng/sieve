//! Under the sieve product the model prints no savings line (test_savings_*).
//! Every test runs the binary with `SIEVE_PRODUCT=sieve` and a scratch HOME.

mod support;

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use support::TempDir;

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

fn run(cwd: &Path, home: &Path, args: &[&str], stdin: Option<&str>) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sieve"));
    cmd.args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("CLAUDE_PROJECT_DIR", cwd)
        .env("PATH", support::sieve_path());
    if let Some(stdin) = stdin {
        cmd.env("SIEVE_TEST_STDIN", stdin);
    }
    cmd.output().expect("run sieve")
}

/// A built copy of the `basic` fixture and a scratch HOME.
fn built() -> (TempDir, TempDir) {
    let home = TempDir::new("savings-sieve-home");
    let copy = TempDir::new("savings-sieve-copy");
    let fixture = support::manifest_dir().join("../../tests/fixtures/basic");
    copy_dir(&fixture, &copy.path);
    let out = run(&copy.path, &home.path, &["build", "."], None);
    assert!(out.status.success(), "build failed");
    (copy, home)
}

/// True when `text` holds the emoji or a tally instruction.
fn has_tally(text: &str) -> bool {
    text.contains('\u{1f331}')
        || text.contains("At the end of your reply")
        || text.contains("close your reply")
        || text.contains("tally")
}

#[test]
fn test_savings_sieve_skill_and_session_start_hold_no_tally() {
    let (copy, home) = built();
    let init = run(
        &copy.path,
        &home.path,
        &["init", "--agents", "agents", "claude", "--no-build"],
        None,
    );
    assert!(init.status.success(), "init failed");
    let skill =
        fs::read_to_string(copy.path.join(".claude/skills/sieve/SKILL.md")).expect("read SKILL.md");
    assert!(!has_tally(&skill), "skill holds a tally instruction");

    let stdin = format!(
        r#"{{"session_id":"s1","cwd":"{}","hook_event_name":"SessionStart"}}"#,
        copy.path.display()
    );
    let hook = run(
        &copy.path,
        &home.path,
        &["hook", "session-start"],
        Some(&stdin),
    );
    let out = String::from_utf8_lossy(&hook.stdout);
    assert!(out.contains("has a sieve/ folder"), "no directive: {out}");
    assert!(!has_tally(&out), "session-start holds a tally: {out}");
}

#[test]
fn test_savings_sieve_tool_header_is_short_and_recorded() {
    let (copy, home) = built();
    let grep = run(&copy.path, &home.path, &["grep", "import", "--fixed"], None);
    let stdout = String::from_utf8_lossy(&grep.stdout).into_owned();
    let header = stdout.lines().next().expect("a header line");
    assert!(
        header.starts_with("[sieve] saved \u{2248} ") && header.ends_with(" tokens"),
        "header: {header}"
    );
    assert!(!has_tally(&stdout), "{stdout}");

    // The hook still records the saving from the short header.
    let stdin = format!(
        r#"{{"session_id":"sess1","cwd":"{}","tool_name":"Bash","tool_response":{{"stdout":"[sieve] saved ≈ 100 tokens"}}}}"#,
        copy.path.display()
    );
    run(
        &copy.path,
        &home.path,
        &["hook", "tool-savings"],
        Some(&stdin),
    );
    let path = copy.path.join("sieve/.cache/session/sess1.json");
    let session: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(path).expect("read session")).expect("json");
    assert_eq!(session["savedTokens"], 100, "{session}");
    let by_day: u64 = session["savedByDay"]
        .as_object()
        .expect("savedByDay")
        .values()
        .filter_map(serde_json::Value::as_u64)
        .sum();
    assert_eq!(by_day, 100, "{session}");
}

#[test]
fn test_savings_stats_json_gives_session_today_and_seven_days() {
    let home = TempDir::new("savings-stats-home");
    let copy = TempDir::new("savings-stats-copy");
    let dir = copy.path.join("sieve/.cache/session");
    fs::create_dir_all(&dir).expect("mkdir");
    fs::write(
        dir.join("a.json"),
        r#"{"savedTokens":700,"savedByDay":{"2000-01-01":700}}"#,
    )
    .expect("write fixture");
    let out = run(&copy.path, &home.path, &["stats", "--json"], None);
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stats --json is JSON");
    assert_eq!(json["session"]["tokens"], 700, "{json}");
    assert!(
        json["today"]["date"].is_string() && json["today"]["tokens"] == 0,
        "{json}"
    );
    assert_eq!(
        json["last7days"].as_array().map(Vec::len),
        Some(7),
        "{json}"
    );
    assert!(json["basis"].is_string(), "{json}");
}
