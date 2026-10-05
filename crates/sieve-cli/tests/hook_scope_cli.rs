//! Parity tests for the prompt hook's scope hint (P4-23, P4-17), the
//! UTF-16 prompt floor (P4-17) and the UTF-16 session-start cut (P4-13),
//! against the goldens under `tests/fixtures/workspace.expected/hooks/`.
//!
//! The workspace fixture has two scopes, so it is the one fixture where
//! `lastFileScopeHint` can fire. Every case runs in its own fixture copy
//! and its own session. The helpers mirror `hook_cli.rs`; that file is
//! a separate test binary, so its helpers are out of reach here.

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

fn fixture_src() -> PathBuf {
    support::manifest_dir().join("../../tests/fixtures/workspace")
}

fn hooks_expected_dir() -> PathBuf {
    support::manifest_dir().join("../../tests/fixtures/workspace.expected/hooks")
}

/// Runs `sieve build` in `copy`, the same prelude that runs before
/// it drives the hooks.
fn build(copy: &Path) {
    let output = support::sieve_command()
        .arg("build")
        .current_dir(copy)
        .output()
        .expect("run sieve build");
    assert!(
        output.status.success(),
        "sieve build failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Runs `sieve hook <event>` with `stdin_json` on `SIEVE_TEST_STDIN`,
/// `CLAUDE_PROJECT_DIR` and the cwd both at `copy`, and `HOME` at an empty
/// scratch dir, never the real home.
fn run_hook(copy: &Path, event: &str, stdin_json: &str) -> Output {
    let home = copy.parent().unwrap_or(copy).join("home-scratch");
    fs::create_dir_all(&home).expect("create home scratch dir");
    support::sieve_command()
        .arg("hook")
        .arg(event)
        .current_dir(copy)
        .env("CLAUDE_PROJECT_DIR", copy)
        .env("SIEVE_TEST_STDIN", stdin_json)
        .env("HOME", home)
        .env_remove("SIEVE_DIR")
        .output()
        .expect("run sieve hook")
}

fn read_golden_bytes(dir: &Path, name: &str) -> Vec<u8> {
    fs::read(dir.join(name)).unwrap_or_else(|e| panic!("read golden {name}: {e}"))
}

fn assert_hook_matches_golden(id: &str, output: &Output) {
    let dir = hooks_expected_dir();
    support::golden::bless_triple(
        &dir,
        id,
        &output.stdout,
        &output.stderr,
        output.status.code().unwrap_or(-1),
    );
    let expected_stdout = read_golden_bytes(&dir, &format!("{id}.stdout.txt"));
    let expected_stderr = read_golden_bytes(&dir, &format!("{id}.stderr.txt"));
    let expected_exit: i32 =
        String::from_utf8_lossy(&read_golden_bytes(&dir, &format!("{id}.exit.txt")))
            .trim()
            .parse()
            .unwrap_or_else(|e| panic!("parse {id}.exit.txt: {e}"));
    assert_eq!(
        output.stdout,
        expected_stdout,
        "{id}: stdout mismatch\n  got: {}\n  want: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&expected_stdout)
    );
    assert_eq!(
        output.stderr,
        expected_stderr,
        "{id}: stderr mismatch\n  got: {}\n  want: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&expected_stderr)
    );
    assert_eq!(
        output.status.code(),
        Some(expected_exit),
        "{id}: exit mismatch"
    );
}

/// One `scope_case`: a fresh copy, an optional extra source
/// file built in, an optional post-edit on one file, one prompt, then the
/// session and stats files, each compared byte for byte.
fn scope_case(case: &str, extra_file: Option<&str>, edit_file: Option<&str>, prompt: &str) {
    scope_case_with(case, extra_file.map(|f| (f, None)), edit_file, prompt);
}

/// `scope_case` with an optional source file for the extra file: the
/// name of a file under `tests/inputs/hooks` to copy in, else the default
/// `betaAlpha` body.
fn scope_case_with(
    case: &str,
    extra: Option<(&str, Option<&str>)>,
    edit_file: Option<&str>,
    prompt: &str,
) {
    let tmp = TempDir::new(case);
    let copy = tmp.path.join("copy");
    copy_dir(&fixture_src(), &copy);
    if let Some((extra, src)) = extra {
        let path = copy.join(extra);
        fs::create_dir_all(path.parent().expect("extra file parent")).expect("create extra dir");
        match src {
            Some(name) => {
                let from = support::manifest_dir()
                    .join("../../tests/inputs/hooks")
                    .join(name);
                fs::copy(from, &path).expect("copy extra file");
            }
            None => fs::write(
                &path,
                "export function betaAlpha(): number {\n  return 7;\n}\n",
            )
            .expect("write extra file"),
        }
    }
    build(&copy);

    let cp = copy.to_string_lossy().into_owned();
    let ntp = format!("{cp}/no-transcript.jsonl");
    if let Some(edit) = edit_file {
        let json = format!(
            r#"{{"session_id":"golden-session","transcript_path":"{ntp}","cwd":"{cp}","hook_event_name":"PostToolUse","tool_name":"Edit","tool_input":{{"file_path":"{cp}/{edit}","old_string":"a","new_string":"b"}},"tool_response":{{"filePath":"{cp}/{edit}"}}}}"#
        );
        let output = run_hook(&copy, "post-edit", &json);
        assert_hook_matches_golden(&format!("{case}-edit"), &output);
    }
    let json = format!(
        r#"{{"session_id":"golden-session","transcript_path":"{ntp}","cwd":"{cp}","hook_event_name":"UserPromptSubmit","prompt":"{prompt}"}}"#
    );
    let output = run_hook(&copy, "prompt", &json);
    assert_hook_matches_golden(&format!("{case}-prompt"), &output);

    let state_dir = hooks_expected_dir().join("state").join(case);
    for (actual, golden) in [
        (
            "sieve/.cache/session/golden-session.json",
            "golden-session.json",
        ),
        ("sieve/.cache/stats.json", "stats.json"),
    ] {
        let got = fs::read_to_string(copy.join(actual)).ok();
        support::golden::bless_optional(&state_dir.join(golden), got.as_deref());
        let expected = fs::read_to_string(state_dir.join(golden)).ok();
        assert_eq!(got, expected, "{case}: {golden} mismatch");
    }
}

#[test]
fn test_p4_23_scope_hint_narrows_the_prompt_ask_to_the_last_file_scope() {
    scope_case(
        "p4-23-scope",
        None,
        Some("packages/alpha/src/a.ts"),
        "how does betaOne compute its number",
    );
}

#[test]
fn test_p4_23_missing_last_file_skips_the_hint_with_a_note() {
    scope_case(
        "p4-23-missing",
        None,
        Some("zzz-missing.ts"),
        "how does betaOne compute its number",
    );
}

#[test]
fn test_p4_23_ambiguous_last_file_skips_the_hint_with_a_note() {
    scope_case(
        "p4-23-ambiguous",
        Some("packages/beta/src/a.ts"),
        Some("packages/alpha/src/a.ts"),
        "how does betaOne compute its number",
    );
}

#[test]
fn test_p4_23_root_last_file_gives_no_hint_and_no_note() {
    scope_case(
        "p4-23-root",
        None,
        Some("src/index.ts"),
        "how does betaOne compute its number",
    );
}

#[test]
fn test_p4_17_prompt_floor_counts_utf16_units() {
    // Six emoji: 6 chars, 12 UTF-16 units. The hook runs the ask and writes
    // `lastQuery`.
    scope_case("p4-17-emoji", None, None, "😀😀😀😀😀😀");
}

#[test]
fn test_p4_19_retrieval_snippet_cuts_at_140_utf16_units() {
    // The signature holds an emoji at units 138-139. A char cut keeps one
    // more char and a byte cut splits the emoji; both differ from the unit cut.
    scope_case_with(
        "p4-19-snippet-utf16",
        Some(("src/zeta.ts", Some("snippet-long-utf16.ts"))),
        None,
        "how does zetaPlanner compute its number",
    );
}

#[test]
fn test_p4_13_session_start_cuts_a_long_index_at_1500_utf16_units() {
    let tmp = TempDir::new("p4-13-utf16");
    let copy = tmp.path.join("copy");
    copy_dir(&fixture_src(), &copy);
    build(&copy);
    let long_index = support::manifest_dir().join("../../tests/inputs/hooks/index-long-utf16.md");
    fs::copy(long_index, copy.join("sieve/INDEX.md")).expect("copy the long index");
    let cp = copy.to_string_lossy().into_owned();
    let json = format!(
        r#"{{"session_id":"golden-session","transcript_path":"{cp}/no-transcript.jsonl","cwd":"{cp}","hook_event_name":"SessionStart","source":"startup"}}"#
    );
    let output = run_hook(&copy, "session-start", &json);
    assert_hook_matches_golden("p4-13-index-long-utf16", &output);
}
