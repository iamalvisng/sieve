//! Parity tests for `sieve hook <event>` and `sieve statusline` (P4-13 to
//! P4-23, P4-43, P4-24, P4-25): the five Claude Code hook events and the
//! statusline, against the goldens under
//! `tests/fixtures/basic.expected/{hooks,statusline}/` (the `hosts-hooks.md`
//! note).

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

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
    support::manifest_dir().join("../../tests/fixtures/basic")
}

fn statusline_expected_dir() -> PathBuf {
    support::manifest_dir().join("../../tests/fixtures/basic.expected/statusline")
}

fn hooks_expected_dir() -> PathBuf {
    support::manifest_dir().join("../../tests/fixtures/basic.expected/hooks")
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
/// `CLAUDE_PROJECT_DIR` and the cwd both at `copy`. `HOME` pins to an
/// empty scratch dir, never the real home — session-start reads
/// `~/.sieve/update-check.json` (no fetch) and a real, stale cache there
/// would otherwise leak an update nudge into the golden comparison.
fn run_hook(copy: &Path, event: &str, stdin_json: &str) -> Output {
    support::sieve_command()
        .arg("hook")
        .arg(event)
        .current_dir(copy)
        .env("CLAUDE_PROJECT_DIR", copy)
        .env("SIEVE_TEST_STDIN", stdin_json)
        .env("HOME", home_scratch(copy))
        .env_remove("SIEVE_DIR")
        .output()
        .expect("run sieve hook")
}

/// An empty dir beside `copy`, used as `HOME` so `~/.sieve/update-check.json`
/// never resolves to the real home dir.
fn home_scratch(copy: &Path) -> PathBuf {
    let home = copy.parent().unwrap_or(copy).join("home-scratch");
    fs::create_dir_all(&home).expect("create home scratch dir");
    home
}

fn read_golden_bytes(dir: &Path, name: &str) -> Vec<u8> {
    fs::read(dir.join(name)).unwrap_or_else(|e| panic!("read golden {name}: {e}"))
}

fn golden_exit(dir: &Path, id: &str) -> i32 {
    String::from_utf8_lossy(&read_golden_bytes(dir, &format!("{id}.exit.txt")))
        .trim()
        .parse()
        .unwrap_or_else(|e| panic!("parse {id}.exit.txt: {e}"))
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
    let expected_exit = golden_exit(&dir, id);
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

/// The Stop hook's fixed transcript path — the file never exists, so
/// `sample_turn_cost`/`count_tally_turn` both fail soft, matching the
/// golden capture (the capture never wrote a real transcript either).
fn transcript_path(copy: &Path) -> String {
    copy.join("no-transcript.jsonl")
        .to_string_lossy()
        .into_owned()
}

#[test]
fn test_p4_13_to_23_p4_43_hooks_match_golden() {
    let tmp = TempDir::new("hooks");
    let copy = tmp.path.join("copy");
    copy_dir(&fixture_src(), &copy);
    build(&copy);

    let copy_str = copy.to_string_lossy().into_owned();
    let transcript = transcript_path(&copy);

    let session_start = format!(
        r#"{{"session_id":"golden-session","transcript_path":"{transcript}","cwd":"{copy_str}","hook_event_name":"SessionStart","source":"startup"}}"#
    );
    let prompt_hit = format!(
        r#"{{"session_id":"golden-session","transcript_path":"{transcript}","cwd":"{copy_str}","hook_event_name":"UserPromptSubmit","prompt":"how does run compute the result"}}"#
    );
    let prompt_short = format!(
        r#"{{"session_id":"golden-session","transcript_path":"{transcript}","cwd":"{copy_str}","hook_event_name":"UserPromptSubmit","prompt":"hi"}}"#
    );
    let prompt_miss = format!(
        r#"{{"session_id":"golden-session","transcript_path":"{transcript}","cwd":"{copy_str}","hook_event_name":"UserPromptSubmit","prompt":"add some totally unrelated padding words here"}}"#
    );
    let prompt_miss2 = format!(
        r#"{{"session_id":"golden-session","transcript_path":"{transcript}","cwd":"{copy_str}","hook_event_name":"UserPromptSubmit","prompt":"quadruple totally unrelated padding filler text here"}}"#
    );
    let prompt_miss3 = format!(
        r#"{{"session_id":"golden-session","transcript_path":"{transcript}","cwd":"{copy_str}","hook_event_name":"UserPromptSubmit","prompt":"total totally unrelated padding filler text please"}}"#
    );
    let post_edit = format!(
        r#"{{"session_id":"golden-session","transcript_path":"{transcript}","cwd":"{copy_str}","hook_event_name":"PostToolUse","tool_name":"Edit","tool_input":{{"file_path":"{copy_str}/src/util.ts","old_string":"a","new_string":"b"}},"tool_response":{{"filePath":"{copy_str}/src/util.ts"}}}}"#
    );
    let post_edit_under_sieve = format!(
        r#"{{"session_id":"golden-session","transcript_path":"{transcript}","cwd":"{copy_str}","hook_event_name":"PostToolUse","tool_name":"Edit","tool_input":{{"file_path":"{copy_str}/sieve/INDEX.md","old_string":"a","new_string":"b"}},"tool_response":{{"filePath":"{copy_str}/sieve/INDEX.md"}}}}"#
    );
    let savings_read = format!(
        r#"{{"session_id":"golden-session","transcript_path":"{transcript}","cwd":"{copy_str}","hook_event_name":"PostToolUse","tool_name":"Read","tool_input":{{"file_path":"{copy_str}/src/app.ts"}},"tool_response":{{"type":"text","file":{{"filePath":"{copy_str}/src/app.ts","content":"see src/app.ts"}}}}}}"#
    );
    let savings_bash = format!(
        r#"{{"session_id":"golden-session","transcript_path":"{transcript}","cwd":"{copy_str}","hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{{"command":"sieve ask \"how does run work\" . --json -n 3"}},"tool_response":{{"stdout":"[sieve] tokens saved ≈ 420 (72%); this pack ≈ 160 tok vs reading the 3 file(s) whole ≈ 580 tok (estimate).","stderr":""}}}}"#
    );
    let stop = format!(
        r#"{{"session_id":"golden-session","transcript_path":"{transcript}","cwd":"{copy_str}","hook_event_name":"Stop"}}"#
    );

    let calls: &[(&str, &str, &str)] = &[
        ("hook-session-start", "session-start", &session_start),
        ("hook-prompt-hit", "prompt", &prompt_hit),
        ("hook-prompt-short", "prompt", &prompt_short),
        ("hook-prompt-miss", "prompt", &prompt_miss),
        ("hook-prompt-miss2", "prompt", &prompt_miss2),
        ("hook-prompt-miss3", "prompt", &prompt_miss3),
        ("hook-post-edit", "post-edit", &post_edit),
        (
            "hook-post-edit-under-sieve",
            "post-edit",
            &post_edit_under_sieve,
        ),
        ("hook-savings-read", "tool-savings", &savings_read),
        ("hook-savings-bash", "tool-savings", &savings_bash),
        ("hook-stop", "stop", &stop),
    ];

    let mut failures = Vec::new();
    for (id, event, stdin_json) in calls {
        let output = run_hook(&copy, event, stdin_json);
        let result = std::panic::catch_unwind(|| assert_hook_matches_golden(id, &output));
        if let Err(e) = result {
            failures.push(format!("{id}: {e:?}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));

    // The two state files, once, after every hook ran.
    let state_dir = hooks_expected_dir().join("state");
    let actual_session = fs::read_to_string(copy.join("sieve/.cache/session/golden-session.json"))
        .expect("read the session state file");
    support::golden::bless_text_if_blessing(
        &state_dir.join("golden-session.json"),
        &actual_session,
    );
    let expected_session = fs::read_to_string(state_dir.join("golden-session.json"))
        .expect("read the golden session file");
    assert_eq!(actual_session, expected_session, "session state mismatch");

    let actual_stats = fs::read_to_string(copy.join("sieve/.cache/stats.json"))
        .expect("read the stats state file");
    support::golden::bless_text_if_blessing(&state_dir.join("stats.json"), &actual_stats);
    let expected_stats =
        fs::read_to_string(state_dir.join("stats.json")).expect("read the golden stats file");
    assert_eq!(actual_stats, expected_stats, "stats state mismatch");
}

// ---------------------------------------------------------------------------
// Per-item hook tests (P4-13 to P4-22). Each test runs in its own built copy
// and its own session id, so no test touches the state the 11-event test pins.
// The hand-made inputs (a long INDEX.md, a tally transcript) live under
// `tests/inputs/hooks/`.
// ---------------------------------------------------------------------------

fn hook_inputs_dir() -> PathBuf {
    support::manifest_dir().join("../../tests/inputs/hooks")
}

/// A fresh built copy of `src` at `<tmp>/<name>`.
fn built_copy(tmp: &Path, name: &str, src: &Path) -> PathBuf {
    let copy = tmp.join(name);
    copy_dir(src, &copy);
    build(&copy);
    copy
}

/// One hook event's stdin JSON: the common envelope, then `rest`.
fn envelope(session: &str, transcript: &str, copy: &Path, rest: &str) -> String {
    let cwd = copy.to_string_lossy();
    format!(r#"{{"session_id":"{session}","transcript_path":"{transcript}","cwd":"{cwd}",{rest}}}"#)
}

/// A `Read` of `src/app.ts`: a source read, which stamps `host` first.
fn read_event_json(copy: &Path, session: &str, transcript: &str) -> String {
    let cwd = copy.to_string_lossy();
    envelope(
        session,
        transcript,
        copy,
        &format!(
            r#""hook_event_name":"PostToolUse","tool_name":"Read","tool_input":{{"file_path":"{cwd}/src/app.ts"}},"tool_response":{{"type":"text","file":{{"filePath":"{cwd}/src/app.ts","content":"see src/app.ts"}}}}"#
        ),
    )
}

fn edit_event_json(copy: &Path, session: &str, rel_file: &str) -> String {
    let cwd = copy.to_string_lossy();
    envelope(
        session,
        &transcript_path(copy),
        copy,
        &format!(
            r#""hook_event_name":"PostToolUse","tool_name":"Edit","tool_input":{{"file_path":"{cwd}/{rel_file}","old_string":"a","new_string":"b"}},"tool_response":{{"filePath":"{cwd}/{rel_file}"}}"#
        ),
    )
}

fn prompt_event_json(copy: &Path, session: &str, extra: &str) -> String {
    envelope(
        session,
        &transcript_path(copy),
        copy,
        &format!(
            r#""hook_event_name":"UserPromptSubmit","prompt":"how does run compute the result"{extra}"#
        ),
    )
}

/// Compares one state file in `copy` with `hooks/state/<golden>`.
fn assert_state_matches_golden(copy: &Path, rel_state: &str, golden: &str) {
    let actual = fs::read_to_string(copy.join(rel_state))
        .unwrap_or_else(|e| panic!("read the state file {rel_state}: {e}"));
    support::golden::bless_text_if_blessing(
        &hooks_expected_dir().join("state").join(golden),
        &actual,
    );
    let expected = fs::read_to_string(hooks_expected_dir().join("state").join(golden))
        .unwrap_or_else(|e| panic!("read the golden state file {golden}: {e}"));
    assert_eq!(actual, expected, "{golden}: state mismatch");
}

// Divergence: sieve cuts the repo map at 1500 UTF-16 units
// (`slice(0, 1500)`); Sieve cuts at 1500 bytes (`hook.rs`, `truncate_bytes`).
// The INDEX.md header holds an em dash (3 bytes, 1 unit), so Sieve's cut
// lands 2 bytes early. The golden is the unit cut; the test runs once the cut
// counts units.
#[test]
fn test_p4_13_session_start_cuts_a_long_index_at_the_budget() {
    let tmp = TempDir::new("hooks-p4-13");
    let copy = built_copy(&tmp, "copy", &fixture_src());
    fs::copy(
        hook_inputs_dir().join("index-long.md"),
        copy.join("sieve/INDEX.md"),
    )
    .expect("copy the long INDEX.md");
    let json = envelope(
        "p4-13-index-long",
        &transcript_path(&copy),
        &copy,
        r#""hook_event_name":"SessionStart","source":"startup""#,
    );
    let output = run_hook(&copy, "session-start", &json);
    assert_hook_matches_golden("p4-13-index-long", &output);
}

#[test]
fn test_p4_14_post_edit_reads_the_apply_patch_header() {
    let tmp = TempDir::new("hooks-p4-14a");
    let copy = built_copy(&tmp, "copy", &fixture_src());
    let json = envelope(
        "p4-14-apply-patch",
        &transcript_path(&copy),
        &copy,
        r#""hook_event_name":"PostToolUse","tool_name":"apply_patch","tool_input":{"command":"*** Begin Patch\n*** Update File: src/util.ts\n*** End Patch"},"tool_response":{}"#,
    );
    let output = run_hook(&copy, "post-edit", &json);
    assert_hook_matches_golden("p4-14-apply-patch", &output);
    assert_state_matches_golden(
        &copy,
        "sieve/.cache/stats.json",
        "p4-14-apply-patch.stats.json",
    );
}

#[test]
fn test_p4_14_post_edit_counts_the_stale_file() {
    let tmp = TempDir::new("hooks-p4-14b");
    let copy = built_copy(&tmp, "copy", &fixture_src());
    let util = copy.join("src/util.ts");
    let mut source = fs::read_to_string(&util).expect("read src/util.ts");
    source.push_str("\nexport const driftMarker = 1;\n");
    fs::write(&util, source).expect("write src/util.ts");
    let json = edit_event_json(&copy, "p4-14-stale", "src/util.ts");
    let output = run_hook(&copy, "post-edit", &json);
    assert_hook_matches_golden("p4-14-stale", &output);
    assert_state_matches_golden(&copy, "sieve/.cache/stats.json", "p4-14-stale.stats.json");
}

#[test]
fn test_p4_15_tool_savings_reclassifies_a_footer_bash_as_golden() {
    let tmp = TempDir::new("hooks-p4-15");
    let copy = built_copy(&tmp, "copy", &fixture_src());
    let transcript = transcript_path(&copy);
    let read = read_event_json(&copy, "p4-15-session", &transcript);
    let output = run_hook(&copy, "tool-savings", &read);
    assert_hook_matches_golden("p4-15-read", &output);
    let bash = envelope(
        "p4-15-session",
        &transcript,
        &copy,
        r#""hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{"command":"echo hi"},"tool_response":{"stdout":"[sieve] tokens saved ≈ 100","stderr":""}"#,
    );
    let output = run_hook(&copy, "tool-savings", &bash);
    assert_hook_matches_golden("p4-15-echo-footer", &output);
    assert_state_matches_golden(
        &copy,
        "sieve/.cache/session/p4-15-session.json",
        "p4-15-session.json",
    );
}

#[test]
fn test_p4_16_stop_counts_the_tally_turn() {
    let tmp = TempDir::new("hooks-p4-16");
    let copy = built_copy(&tmp, "copy", &fixture_src());
    let transcript = copy.join("tally.jsonl");
    fs::copy(hook_inputs_dir().join("tally.jsonl"), &transcript).expect("copy tally.jsonl");
    let transcript = transcript.to_string_lossy().into_owned();
    let read = read_event_json(&copy, "p4-16-session", &transcript);
    let output = run_hook(&copy, "tool-savings", &read);
    assert_hook_matches_golden("p4-16-read", &output);
    let bash = envelope(
        "p4-16-session",
        &transcript,
        &copy,
        r#""hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{"command":"sieve ask \"how does run work\" . --json -n 3"},"tool_response":{"stdout":"[sieve] tokens saved ≈ 420 (72%); this pack ≈ 160 tok vs reading the 3 file(s) whole ≈ 580 tok (estimate).","stderr":""}"#,
    );
    let output = run_hook(&copy, "tool-savings", &bash);
    assert_hook_matches_golden("p4-16-bash", &output);
    let stop = envelope(
        "p4-16-session",
        &transcript,
        &copy,
        r#""hook_event_name":"Stop""#,
    );
    let output = run_hook(&copy, "stop", &stop);
    assert_hook_matches_golden("p4-16-stop", &output);
    assert_state_matches_golden(
        &copy,
        "sieve/.cache/session/p4-16-session.json",
        "p4-16-session.json",
    );
}

#[test]
fn test_p4_17_prompt_records_the_agent_query() {
    let tmp = TempDir::new("hooks-p4-17");
    let copy = built_copy(&tmp, "copy", &fixture_src());
    let json = prompt_event_json(&copy, "p4-17-session", r#","agent":{"name":"scout"}"#);
    let output = run_hook(&copy, "prompt", &json);
    assert_hook_matches_golden("p4-17-agent", &output);
    assert_state_matches_golden(
        &copy,
        "sieve/.cache/session/p4-17-session.json",
        "p4-17-session.json",
    );
}

#[test]
fn test_p4_20_blast_radius_caps_at_8_and_counts_the_rest() {
    let tmp = TempDir::new("hooks-p4-20");
    let wide = support::manifest_dir().join("../../tests/fixtures/wide");
    let copy = built_copy(&tmp, "wide", &wide);
    let json = edit_event_json(&copy, "p4-20-session", "d01/a.ts");
    let output = run_hook(&copy, "post-edit", &json);
    assert_hook_matches_golden("p4-20-wide", &output);
}

#[test]
fn test_p4_22_novelty_gate_drops_a_repeated_prompt() {
    let tmp = TempDir::new("hooks-p4-22");
    let copy = built_copy(&tmp, "copy", &fixture_src());
    let json = prompt_event_json(&copy, "p4-22-session", "");
    let output = run_hook(&copy, "prompt", &json);
    assert_hook_matches_golden("p4-22-first", &output);
    let output = run_hook(&copy, "prompt", &json);
    assert_hook_matches_golden("p4-22-again", &output);
    // Other words, same hits: the gate keys on the pointers, not the text.
    let reworded = envelope(
        "p4-22-session",
        &transcript_path(&copy),
        &copy,
        r#""hook_event_name":"UserPromptSubmit","prompt":"explain the result that run computes""#,
    );
    let output = run_hook(&copy, "prompt", &reworded);
    assert_hook_matches_golden("p4-22-reworded", &output);
}

/// P4-22 (c): the top hit is a concept, and one prompt word in twelve
/// matches it. The strength gate reads the concept's coverage, so the
/// hook prints the weak-match nudge, not the pack.
#[test]
fn test_p4_22_strength_gate_reads_a_concept_top_coverage() {
    let tmp = TempDir::new("hooks-p4-22c");
    let copy = built_copy(&tmp, "copy", &fixture_src());
    fs::copy(
        hook_inputs_dir().join("notes-concept.md"),
        copy.join("sieve/notes-concept.md"),
    )
    .expect("copy the concept doc");
    let json = envelope(
        "p4-22-concept",
        &transcript_path(&copy),
        &copy,
        r#""hook_event_name":"UserPromptSubmit","prompt":"notes alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo""#,
    );
    let output = run_hook(&copy, "prompt", &json);
    assert_hook_matches_golden("p4-22-concept-weak", &output);
}

// ---------------------------------------------------------------------------
// P4-53 to P4-56: the hook events the 11-event golden test does not drive.
// Each expected value is what the recorded run printed or wrote for the same
// stdin, read with the audit's `hk.sh` compare.
// ---------------------------------------------------------------------------

/// A built copy, the Stop tally transcript, and a `Read` and a `Bash sieve`
/// tool use already recorded for `session`: the `p4-16` setup.
fn tally_session(tmp: &Path, session: &str) -> (PathBuf, String) {
    let copy = built_copy(tmp, "copy", &fixture_src());
    let transcript = copy.join("tally.jsonl");
    fs::copy(hook_inputs_dir().join("tally.jsonl"), &transcript).expect("copy tally.jsonl");
    let transcript = transcript.to_string_lossy().into_owned();
    let read = read_event_json(&copy, session, &transcript);
    run_hook(&copy, "tool-savings", &read);
    let bash = envelope(
        session,
        &transcript,
        &copy,
        r#""hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{"command":"sieve ask \"how does run work\" . --json -n 3"},"tool_response":{"stdout":"[sieve] tokens saved ≈ 420 (72%); this pack ≈ 160 tok vs reading the 3 file(s) whole ≈ 580 tok (estimate).","stderr":""}"#,
    );
    run_hook(&copy, "tool-savings", &bash);
    (copy, transcript)
}

#[test]
fn test_p4_53_post_edit_sync_prints_the_blast_radius_and_runs_the_stop_tally() {
    let tmp = TempDir::new("hooks-p4-53");
    let (copy, transcript) = tally_session(&tmp, "p4-53-session");
    let cwd = copy.to_string_lossy();
    let edit = envelope(
        "p4-53-session",
        &transcript,
        &copy,
        &format!(
            r#""hook_event_name":"PostToolUse","tool_name":"Edit","tool_input":{{"file_path":"{cwd}/src/util.ts"}}"#
        ),
    );
    let output = run_hook(&copy, "post-edit-sync", &edit);
    // The edit half: the blast radius, as for `post-edit`.
    assert_hook_matches_golden("hook-post-edit", &output);
    let stats: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(copy.join("sieve/.cache/stats.json")).expect("read stats.json"),
    )
    .expect("parse stats.json");
    assert_eq!(stats["lastFile"], "util.ts");
    assert_eq!(stats["staleCount"], 0);
    // The Stop half: the tally count, as for `stop` (the p4-16 golden).
    assert_state_matches_golden(
        &copy,
        "sieve/.cache/session/p4-53-session.json",
        "p4-16-session.json",
    );
}

/// A bare scratch project: no build, only the dir the hooks write under.
fn bare_copy(tmp: &Path) -> PathBuf {
    let copy = tmp.join("copy");
    fs::create_dir_all(&copy).expect("create the project dir");
    copy
}

/// The session file the context dir holds for `id`, parsed, or `None`.
/// The `savedByDay` key holds today's date, so this drops it.
fn session_json(copy: &Path, id: &str) -> Option<serde_json::Value> {
    let raw = fs::read_to_string(copy.join(format!("sieve/.cache/session/{id}.json"))).ok()?;
    let mut value: serde_json::Value = serde_json::from_str(&raw).expect("parse the session file");
    if let Some(map) = value.as_object_mut() {
        map.remove("savedByDay");
    }
    Some(value)
}

/// The session file after one tool use. Key order differs from the golden
/// (`host` before `turnUsedSieve`, the recorded D3 deviation), so the test
/// compares parsed values.
fn cursor_session(sieve: u64, source: u64, saved: u64, used_sieve: bool) -> serde_json::Value {
    let mut state = serde_json::json!({
        "lastQuery": null,
        "perAgentQuery": {},
        "toolReads": sieve,
        "sourceReads": source,
        "savedTokens": saved,
        "injectedPointers": [],
        "nudges": 0,
        "host": "cursor",
    });
    if used_sieve {
        state["turnUsedSieve"] = serde_json::json!(true);
    }
    state
}

#[test]
fn test_p4_54_cursor_post_tool_scores_each_tool_use() {
    let tmp = TempDir::new("hooks-p4-54a");
    let copy = bare_copy(&tmp);
    let cases = [
        // A Shell command that runs sieve, with a footer in `tool_output`.
        (
            "c1",
            r#"{"conversation_id":"c1","tool_name":"Shell","tool_input":{"command":"sieve ask x"},"tool_output":"{\"stdout\":\"[sieve] tokens saved ≈ 420 (72%)\"}"}"#,
            Some(cursor_session(1, 0, 420, true)),
        ),
        // A source read.
        (
            "c2",
            r#"{"conversation_id":"c2","tool_name":"Read","tool_input":{"file_path":"x"},"tool_output":"text"}"#,
            Some(cursor_session(0, 1, 0, false)),
        ),
        // `session_id` stands in for a missing `conversation_id`.
        (
            "sx",
            r#"{"session_id":"sx","tool_name":"Grep","tool_input":{"pattern":"x"},"tool_output":"text"}"#,
            Some(cursor_session(0, 1, 0, false)),
        ),
        // No id at all: the `default` session.
        (
            "default",
            r#"{"tool_name":"Read","tool_input":{"file_path":"x"}}"#,
            Some(cursor_session(0, 1, 0, false)),
        ),
        // `cmd` stands in for `command`.
        (
            "c6",
            r#"{"conversation_id":"c6","tool_name":"Shell","tool_input":{"cmd":"npx -y sieve map"},"tool_output":"nothing"}"#,
            Some(cursor_session(1, 0, 0, true)),
        ),
        // `tool_response` stands in for `tool_output`, and a footer makes
        // an `echo` a use of sieve.
        (
            "c7",
            r#"{"conversation_id":"c7","tool_name":"Shell","tool_input":{"command":"echo hi"},"tool_response":{"stdout":"[sieve] tokens saved ≈ 1,100"}}"#,
            Some(cursor_session(1, 0, 1100, true)),
        ),
        // A Write tool is neither: no file.
        (
            "c8",
            r#"{"conversation_id":"c8","tool_name":"Write","tool_input":{"file_path":"x"},"tool_output":"ok"}"#,
            None,
        ),
        // An MCP tool name belongs to `cursor-mcp`, in both name shapes.
        (
            "c4",
            r#"{"conversation_id":"c4","tool_name":"MCP:sieve_find_code","tool_input":{},"tool_output":"[sieve] tokens saved ≈ 99"}"#,
            None,
        ),
        (
            "c5",
            r#"{"conversation_id":"c5","tool_name":"sieve_find_code","tool_input":{},"tool_output":"[sieve] tokens saved ≈ 99"}"#,
            None,
        ),
    ];
    for (id, stdin, want) in cases {
        let output = run_hook(&copy, "cursor-post-tool", stdin);
        assert!(output.status.success(), "{id}: exit");
        assert!(output.stdout.is_empty(), "{id}: stdout must be empty");
        assert_eq!(session_json(&copy, id), want, "{id}: session file");
    }
}

#[test]
fn test_p4_54_cursor_mcp_counts_a_sieve_tool_call() {
    let tmp = TempDir::new("hooks-p4-54b");
    let copy = bare_copy(&tmp);
    let cases = [
        // The savings sit in `result_json`.
        (
            "c3",
            r#"{"conversation_id":"c3","tool_name":"sieve_find_code","tool_input":{},"result_json":"{\"content\":[{\"text\":\"[sieve] tokens saved ≈ 1,500\"}]}"}"#,
            Some(cursor_session(1, 0, 1500, true)),
        ),
        // `result` stands in for `result_json`; the name carries a prefix.
        (
            "c9",
            r#"{"conversation_id":"c9","tool_name":"MCP:sieve_repo_map","result":{"t":"[sieve] tokens saved ≈ 7"}}"#,
            Some(cursor_session(1, 0, 7, true)),
        ),
        // A call to a sieve tool with no footer still counts as a read.
        (
            "c11",
            r#"{"conversation_id":"c11","tool_name":"mcp__sieve__sieve_trace_calls","result_json":"nothing"}"#,
            Some(cursor_session(1, 0, 0, true)),
        ),
        // A tool that is not a sieve tool is ignored, footer or not.
        (
            "c10",
            r#"{"conversation_id":"c10","tool_name":"other_tool","result_json":"[sieve] tokens saved ≈ 7"}"#,
            None,
        ),
    ];
    for (id, stdin, want) in cases {
        let output = run_hook(&copy, "cursor-mcp", stdin);
        assert!(output.status.success(), "{id}: exit");
        assert!(output.stdout.is_empty(), "{id}: stdout must be empty");
        assert_eq!(session_json(&copy, id), want, "{id}: session file");
    }
}

#[test]
fn test_p4_54_cursor_session_end_prints_and_writes_nothing() {
    let tmp = TempDir::new("hooks-p4-54c");
    let copy = bare_copy(&tmp);
    let output = run_hook(&copy, "cursor-session-end", r#"{"conversation_id":"c1"}"#);
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    assert!(!copy.join("sieve").exists(), "no state under sieve/");
}

/// The stdin of a `Read` of a small file, for `pre-read` and `post-read`.
fn read_hook_stdin(copy: &Path) -> String {
    let file = copy.join("a.txt");
    fs::write(&file, "one\n").expect("write a.txt");
    format!(
        r#"{{"session_id":"p4-56","cwd":"{cwd}","tool_name":"Read","tool_input":{{"file_path":"{file}"}}}}"#,
        cwd = copy.display(),
        file = file.display()
    )
}

#[test]
fn test_p4_56_sieve_name_pre_read_still_remembers_the_read() {
    let tmp = TempDir::new("hooks-p4-56b");
    let copy = bare_copy(&tmp);
    let stdin = read_hook_stdin(&copy);
    let output = support::sieve_command()
        .arg("hook")
        .arg("pre-read")
        .current_dir(&copy)
        .env("CLAUDE_PROJECT_DIR", &copy)
        .env("SIEVE_TEST_STDIN", stdin)
        .env("HOME", home_scratch(&copy))
        .env_remove("SIEVE_DIR")
        .output()
        .expect("run sieve hook");
    assert!(output.status.success());
    let raw = fs::read_to_string(copy.join("sieve/.cache/session/p4-56.json"))
        .expect("the sieve name set records the read (F3)");
    let session: serde_json::Value = serde_json::from_str(&raw).expect("parse the session file");
    assert_eq!(session["reads"].as_object().map(|r| r.len()), Some(1));
}

#[test]
fn test_p4_54_session_id_never_leaves_the_session_dir_through_a_hook() {
    let tmp = TempDir::new("hooks-p4-54d");
    let copy = bare_copy(&tmp);
    let escape =
        r#"{"conversation_id":"../../../../ESC","tool_name":"sieve_find_code","result_json":"x"}"#;
    run_hook(&copy, "cursor-mcp", escape);
    let claude =
        r#"{"session_id":"../../../../ESC4","tool_name":"Read","tool_input":{"file_path":"x"}}"#;
    run_hook(&copy, "tool-savings", claude);
    let session_dir = copy.join("sieve/.cache/session");
    let mut names: Vec<String> = fs::read_dir(&session_dir)
        .expect("read the session dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".json"))
        .collect();
    names.sort();
    assert_eq!(names, ["_.._.._.._.._ESC.json", "_.._.._.._.._ESC4.json"]);
    let outside: Vec<String> = fs::read_dir(&tmp.path)
        .expect("read the temp dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        outside.iter().all(|n| n == "copy" || n == "home-scratch"),
        "{outside:?}"
    );
}

#[test]
fn test_p4_54_cursor_keys_pick_in_golden_order() {
    let tmp = TempDir::new("hooks-p4-54e");
    let copy = bare_copy(&tmp);
    // conversation_id wins over session_id; tool_output wins over tool_response.
    let both = r#"{"conversation_id":"A","session_id":"B","tool_name":"Shell","tool_input":{"command":"echo"},"tool_output":"[sieve] tokens saved ≈ 10","tool_response":"[sieve] tokens saved ≈ 20"}"#;
    run_hook(&copy, "cursor-post-tool", both);
    assert_eq!(
        session_json(&copy, "A"),
        Some(cursor_session(1, 0, 10, true))
    );
    assert_eq!(session_json(&copy, "B"), None);
    // result_json wins over result.
    let mcp = r#"{"conversation_id":"M","tool_name":"sieve_find_code","result_json":"[sieve] tokens saved ≈ 5","result":"[sieve] tokens saved ≈ 9"}"#;
    run_hook(&copy, "cursor-mcp", mcp);
    assert_eq!(
        session_json(&copy, "M"),
        Some(cursor_session(1, 0, 5, true))
    );
    // A number names the file; an array falls to session_id.
    let number = r#"{"conversation_id":123,"tool_name":"Read","tool_input":{}}"#;
    run_hook(&copy, "cursor-post-tool", number);
    assert_eq!(
        session_json(&copy, "123"),
        Some(cursor_session(0, 1, 0, false))
    );
    let array = r#"{"conversation_id":[1],"session_id":"S","tool_name":"Read","tool_input":{}}"#;
    run_hook(&copy, "cursor-post-tool", array);
    assert_eq!(
        session_json(&copy, "S"),
        Some(cursor_session(0, 1, 0, false))
    );
}

/// Runs `sieve statusline` with `stdin_json` and gives its stdout. The
/// clock is fixed, so the mascot frame is the same on every run.
fn run_statusline(copy: &Path, stdin_json: &str) -> Vec<u8> {
    let mut child = support::sieve_command()
        .arg("statusline")
        .current_dir(copy)
        .env("CLAUDE_PROJECT_DIR", copy)
        // The goldens hold 24-bit color. CI sets neither variable.
        .env("COLORTERM", "truecolor")
        .env_remove("NO_COLOR")
        .env_remove("SIEVE_DIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn sieve statusline");
    use std::io::Write as _;
    child
        .stdin
        .as_mut()
        .expect("child stdin")
        .write_all(stdin_json.as_bytes())
        .expect("write stdin");
    let output = child.wait_with_output().expect("wait for sieve statusline");
    output.stdout
}

/// The main, subagent and no-graph statusline text, byte for byte.
#[test]
fn test_p4_24_25_statusline_matches_its_golden() {
    let tmp = TempDir::new("statusline");
    let copy = tmp.path.join("copy");
    copy_dir(&fixture_src(), &copy);
    build(&copy);

    // Reuse the same session accumulation the hooks test drives, so
    // `main`'s `~420 tok saved` reflects a real session, matching the
    // golden capture's sequence (hooks ran, then the statusline).
    let copy_str = copy.to_string_lossy().into_owned();
    let transcript = transcript_path(&copy);
    let savings_bash = format!(
        r#"{{"session_id":"golden-session","transcript_path":"{transcript}","cwd":"{copy_str}","hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{{"command":"sieve ask \"how does run work\" . --json -n 3"}},"tool_response":{{"stdout":"[sieve] tokens saved ≈ 420 (72%); this pack ≈ 160 tok vs reading the 3 file(s) whole ≈ 580 tok (estimate).","stderr":""}}}}"#
    );
    run_hook(&copy, "tool-savings", &savings_bash);

    let main_json = r#"{"session_id":"golden-session","cwd":"__CWD__","context_window":{"used_percentage":42},"model":{"id":"claude"},"cost":{"total_cost_usd":0}}"#
        .replace("__CWD__", &copy_str);
    let subagent_json =
        r#"{"session_id":"golden-session","cwd":"__CWD__","agent":{"name":"scout"}}"#
            .replace("__CWD__", &copy_str);

    let cases: &[(&str, &str, PathBuf)] = &[
        ("main", &main_json, copy.clone()),
        ("subagent", &subagent_json, copy.clone()),
    ];

    let mut failures = Vec::new();
    for (id, json, dir) in cases {
        let actual = run_statusline(dir, json);
        if support::golden::blessing() {
            support::golden::bless(
                &statusline_expected_dir().join(format!("{id}.stdout.txt")),
                &actual,
            );
        }
        let expected = read_golden_bytes(&statusline_expected_dir(), &format!("{id}.stdout.txt"));
        if *actual != expected {
            failures.push(format!(
                "{id}: mismatch\n  got: {}\n  want: {}",
                String::from_utf8_lossy(&actual),
                String::from_utf8_lossy(&expected)
            ));
        }
    }

    // `nograph` reuses `main`'s JSON against a copy with `sieve/` deleted.
    let nograph_copy = tmp.path.join("nograph");
    copy_dir(&fixture_src(), &nograph_copy);
    build(&nograph_copy);
    fs::remove_dir_all(nograph_copy.join("sieve")).expect("remove sieve/");
    let nograph_json = main_json.replace(&copy_str, &nograph_copy.to_string_lossy());
    let actual = run_statusline(&nograph_copy, &nograph_json);
    if support::golden::blessing() {
        support::golden::bless(
            &statusline_expected_dir().join("nograph.stdout.txt"),
            &actual,
        );
    }
    let expected = read_golden_bytes(&statusline_expected_dir(), "nograph.stdout.txt");
    if actual != expected {
        failures.push(format!(
            "nograph: mismatch\n  got: {}\n  want: {}",
            String::from_utf8_lossy(&actual),
            String::from_utf8_lossy(&expected)
        ));
    }

    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
