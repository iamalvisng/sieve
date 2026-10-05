//! Parity tests for the prompt hook at a workspace parent (P4-46).
//!
//! The prompt hook runs `sieve ask <prompt> . --json -n 3`,
//! so at a parent it gets the federated result.
//! The expected stdout below is the recorded output for the same
//! `tests/fixtures/ws-ask/` copy, built with `sieve build`.

mod support;

use std::fs;
use std::path::Path;
use std::process::Output;

use support::ws::{copy_dir, git_init};
use support::TempDir;

const PACK_HEAD: &str = "[sieve] starting points for this task: pull the code inline with \
`sieve ask \\\"<what you need>\\\" --source`, trace impact with `sieve callers <symbol>`, or \
search with `sieve grep \\\"<literal>\\\"`:";

/// A built copy of the `ws-ask` fixture: a git repo per child, then one
/// `sieve build` at the parent.
fn built(label: &str) -> TempDir {
    let copy = TempDir::new(label);
    let fixture = support::manifest_dir().join("../../tests/fixtures/ws-ask");
    copy_dir(&fixture, &copy.path);
    for child in ["alpha", "beta", "gamma"] {
        git_init(&copy.path.join(child));
    }
    let build = support::ws::sieve(&copy.path, &["build"]);
    assert_eq!(build.status.code(), Some(0), "workspace build");
    copy
}

/// Runs `sieve hook prompt` in `dir` with `HOME` at a scratch dir.
fn prompt_hook(dir: &Path, home: &Path, prompt: &str) -> Output {
    fs::create_dir_all(home).expect("create home dir");
    let json = serde_json::json!({ "prompt": prompt, "session_id": "s1" }).to_string();
    support::sieve_command()
        .args(["hook", "prompt"])
        .current_dir(dir)
        .env("CLAUDE_PROJECT_DIR", dir)
        .env("SIEVE_TEST_STDIN", json)
        .env("HOME", home)
        .env_remove("SIEVE_DIR")
        .output()
        .expect("run sieve hook")
}

fn pack(blocks: &str) -> String {
    format!(
        "{{\"hookSpecificOutput\":{{\"hookEventName\":\"UserPromptSubmit\",\
\"additionalContext\":\"{PACK_HEAD}\\n{blocks}\"}}}}"
    )
}

/// P4-46: at the parent the hook injects the federated top three, each
/// pointer prefixed with its child.
#[test]
fn test_p4_46_prompt_hook_federates_at_a_workspace_parent() {
    let copy = built("p446-parent");
    let out = prompt_hook(
        &copy.path,
        &copy.path.join("home"),
        "parse config please explain",
    );
    let expected = pack(
        " 1. parseConfigFile \u{b7} function: alpha/src/a.ts:L9-L11\\n    function parseConfigFile(path: string): string\\n \
2. parseConfigRoot \u{b7} function: gamma/src/root.ts:L1-L3\\n    function parseConfigRoot(text: string): string\\n \
3. parseConfigX \u{b7} function: gamma/packages/x/src/parse-config.ts:L1-L3\\n    function parseConfigX(text: string): string",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), expected);
    assert_eq!(String::from_utf8_lossy(&out.stderr), "");
    assert_eq!(out.status.code(), Some(0));
}

/// P4-46: a structural prompt federates too, and only the gamma child
/// answers it.
#[test]
fn test_p4_46_prompt_hook_structural_prompt_at_a_parent() {
    let copy = built("p446-structural");
    let out = prompt_hook(
        &copy.path,
        &copy.path.join("home"),
        "who calls parseConfig in this repo",
    );
    let expected = pack(
        " 1. parseConfigRoot \u{b7} function: gamma/src/root.ts:L1-L3\\n    function parseConfigRoot(text: string): string\\n \
2. parseConfigX \u{b7} function: gamma/packages/x/src/parse-config.ts:L1-L3\\n    function parseConfigX(text: string): string\\n \
3. configY \u{b7} function: gamma/packages/y/src/parse/y.ts:L1-L3\\n    function configY(): number",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), expected);
}

/// P4-46: a prompt with no hit at the parent injects nothing, with exit 0.
#[test]
fn test_p4_46_prompt_hook_no_hit_at_a_parent_is_silent() {
    let copy = built("p446-nohit");
    let out = prompt_hook(
        &copy.path,
        &copy.path.join("home"),
        "zzzqqq nonexistent thing xyzzy",
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
    assert_eq!(String::from_utf8_lossy(&out.stderr), "");
    assert_eq!(out.status.code(), Some(0));
}
