//! Parity tests for two P4 divergences outside the MCP server: the
//! billed-session statusline (P4-24) and the `check -e` warning shape
//! (P4-42), against the recorded captures under
//! `tests/fixtures/basic.expected/mcp-dv/`.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use support::TempDir;

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

fn expected_dir() -> PathBuf {
    support::manifest_dir().join("../../tests/fixtures/basic.expected/mcp-dv")
}

fn golden(name: &str) -> Vec<u8> {
    fs::read(expected_dir().join(name)).unwrap_or_else(|e| panic!("read golden {name}: {e}"))
}

/// A built copy of the basic fixture, with a scratch `HOME` beside it.
fn built_copy(label: &str) -> (TempDir, PathBuf, PathBuf) {
    let tmp = TempDir::new(label);
    let copy = tmp.path.join("copy");
    copy_dir(
        &support::manifest_dir().join("../../tests/fixtures/basic"),
        &copy,
    );
    let home = tmp.path.join("home");
    fs::create_dir_all(&home).expect("create scratch home");
    let real = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    assert!(
        !real.as_os_str().is_empty() && !home.starts_with(&real),
        "scratch HOME sits under the real HOME"
    );
    let output = support::sieve_command()
        .arg("build")
        .current_dir(&copy)
        .env("HOME", &home)
        .output()
        .expect("run sieve build");
    assert!(
        output.status.success(),
        "sieve build failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    (tmp, copy, home)
}

fn run_sieve(copy: &Path, home: &Path, args: &[&str], stdin_json: Option<&str>) -> Output {
    let mut cmd = support::sieve_command();
    cmd.args(args)
        .current_dir(copy)
        .env("HOME", home)
        .env("CLAUDE_PROJECT_DIR", copy)
        .env("NO_COLOR", "1")
        .env("DO_NOT_TRACK", "1")
        .env_remove("SIEVE_DIR");
    if let Some(json) = stdin_json {
        cmd.env("SIEVE_TEST_STDIN", json);
    }
    cmd.output().expect("run sieve")
}

/// P4-42, P1-36: `check -e .foo .bar` prints one `⚠` line per unknown
/// value, then one `supported:` line with every extension the three
/// parser tiers claim, byte for byte as Sieve prints it.
#[test]
fn test_p4_42_check_two_unknown_extensions_warn_then_list_once() {
    let (_tmp, copy, home) = built_copy("check-ext-two");
    let out = run_sieve(&copy, &home, &["check", "-e", ".foo", ".bar"], None);
    support::golden::bless_triple(
        &expected_dir(),
        "check-ext-two",
        &out.stdout,
        &out.stderr,
        out.status.code().unwrap_or(-1),
    );
    let want = golden("check-ext-two.stderr.txt");
    assert_eq!(
        out.stderr,
        want,
        "check -e stderr\n got: {}\nwant: {}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&want)
    );
    assert_eq!(
        out.stdout,
        golden("check-ext-two.stdout.txt"),
        "check -e stdout"
    );
    let exit: i32 = String::from_utf8_lossy(&golden("check-ext-two.exit.txt"))
        .trim()
        .parse()
        .expect("golden exit");
    assert_eq!(out.status.code(), Some(exit), "check -e exit");
}

/// P4-24: a session with `inputCostMicros` and `inputTokensBilled` keeps
/// both fields through the tool-savings rewrite. The statusline text is
/// not pinned: its mascot frame changes with the clock.
#[test]
fn test_p4_24_billed_session_keeps_the_billing_fields() {
    let (_tmp, copy, home) = built_copy("statusline-billed");
    let session_dir = copy.join("sieve").join(".cache").join("session");
    fs::create_dir_all(&session_dir).expect("create session dir");
    fs::copy(
        support::manifest_dir().join("../../tests/inputs/mcp-dv/billed-session.json"),
        session_dir.join("billed-session.json"),
    )
    .expect("seed session");

    let copy_str = copy.to_string_lossy().into_owned();
    let transcript = copy
        .join("no-transcript.jsonl")
        .to_string_lossy()
        .into_owned();
    let savings_json = format!(
        r#"{{"session_id":"billed-session","transcript_path":"{transcript}","cwd":"{copy_str}","hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{{"command":"sieve ask \"how does run work\" . --json -n 3"}},"tool_response":{{"stdout":"[sieve] tokens saved ≈ 12000 (72%); this pack ≈ 160 tok vs reading the 3 file(s) whole ≈ 580 tok (estimate).","stderr":""}}}}"#
    );
    run_sieve(&copy, &home, &["hook", "tool-savings"], Some(&savings_json));

    let rewritten: serde_json::Value = serde_json::from_slice(
        &fs::read(session_dir.join("billed-session.json")).expect("read rewritten session"),
    )
    .expect("rewritten session is JSON");
    assert_eq!(rewritten["savedTokens"], 12000, "{rewritten}");
    assert_eq!(rewritten["inputCostMicros"], 5000000, "{rewritten}");
    assert_eq!(rewritten["inputTokensBilled"], 1000000, "{rewritten}");
}
