//! `sieve why --link`: the proposal, the sidecar write, the agent link row,
//! the stale lines of `why --check` and the once-per-session note.

mod support;

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use support::TempDir;

const LIB: &str = "pub fn build_repo() {\n    fail(\"a graph rebuild is already in flight\");\n}\n";
const OTHER: &str = "pub fn plain() {}\n";
const ADR1: &str = "# Build takes a lock\n\n- **Date:** 2026-10-01\n- **Decision:** The build waits for the lock in src/lib.rs.\n";
const ADR2: &str = "# Plain stays small\n\n- **Date:** 2026-10-02\n- **Decision:** The plain function in src/other.rs stays empty.\n";
const DOC1: &str = "docs/adr/0001-build-lock.md";
const DOC2: &str = "docs/adr/0002-plain.md";
const KEY1: &str = "docs/adr/0001-build-lock.md#Build takes a lock";
const KEY2: &str = "docs/adr/0002-plain.md#Plain stays small";

/// A built scratch project with two source files and a ledger of two records.
fn project(label: &str) -> TempDir {
    let temp = TempDir::new(label);
    fs::create_dir_all(temp.path.join("src")).expect("mkdir src");
    fs::create_dir_all(temp.path.join("docs/adr")).expect("mkdir docs");
    fs::write(temp.path.join("src/lib.rs"), LIB).expect("write lib");
    fs::write(temp.path.join("src/other.rs"), OTHER).expect("write other");
    fs::write(temp.path.join(DOC1), ADR1).expect("write adr 1");
    fs::write(temp.path.join(DOC2), ADR2).expect("write adr 2");
    let built = run(&temp.path, &["build"], None);
    assert!(built.status.success(), "{}", text(&built.stderr));
    temp
}

fn run(cwd: &Path, args: &[&str], stdin_json: Option<&str>) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sieve"));
    cmd.args(args)
        .current_dir(cwd)
        .env("HOME", cwd.join("home"))
        .env("CLAUDE_PROJECT_DIR", cwd)
        .env("PATH", support::sieve_path());
    if let Some(json) = stdin_json {
        cmd.env("SIEVE_TEST_STDIN", json);
    }
    cmd.output().expect("run sieve")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// The JSON proposal of `key`.
fn proposal(temp: &TempDir, key: &str) -> serde_json::Value {
    let out = run(&temp.path, &["why", "--link", key, "--json"], None);
    assert!(out.status.success(), "{}", text(&out.stderr));
    serde_json::from_slice(&out.stdout).expect("proposal json")
}

/// The candidate number of `path` in a proposal.
fn number_of(p: &serde_json::Value, path: &str) -> u64 {
    p["candidates"]
        .as_array()
        .expect("candidates")
        .iter()
        .find(|c| c["path"] == path)
        .unwrap_or_else(|| panic!("{path} is no candidate: {p}"))["n"]
        .as_u64()
        .expect("number")
}

/// Keeps the candidates `keep` of the proposal for `key`.
fn write_link(temp: &TempDir, key: &str, keep: &str, expect: &str) -> Output {
    run(
        &temp.path,
        &[
            "why", "--link", key, "--keep", keep, "--expect", expect, "--json",
        ],
        None,
    )
}

/// Links `src/lib.rs` to the first record.
fn link_lib(temp: &TempDir) {
    let p = proposal(temp, KEY1);
    let n = number_of(&p, "src/lib.rs").to_string();
    let out = write_link(temp, KEY1, &n, p["link_id"].as_str().expect("id"));
    assert!(out.status.success(), "{}", text(&out.stderr));
}

/// The `why` output of the fixture project before any link file exists. The
/// main binary printed these texts before the link feature; the feature must
/// not change one byte of them.
const BASELINE_TEXT: &str = "lib.rs  file  src/lib.rs:1-4\nno decision found for lib.rs\n";
const BASELINE_JSON: &str = "{\n  \"query\": \"src/lib.rs\",\n  \"symbols\": [\n    {\n      \"decisions\": [],\n      \"evidence\": [],\n      \"id\": \"src/lib.rs\",\n      \"line\": 1,\n      \"name\": \"lib.rs\",\n      \"path\": \"src/lib.rs\",\n      \"total\": 0\n    }\n  ]\n}\n";
const BASELINE_SYMBOL: &str =
    "build_repo  fn  src/lib.rs:1-3\nno recorded link for build_repo\nhistory skipped: git failed\n";

/// One project build serves the whole flow, because the first build of a
/// project is the slow step of these tests.
#[test]
fn test_link_flow_writes_shows_refuses_and_goes_stale() {
    let temp = project("link-flow");

    // A repo with no link file answers as it did before the feature.
    assert_eq!(
        text(&run(&temp.path, &["why", "src/lib.rs"], None).stdout),
        BASELINE_TEXT
    );
    assert_eq!(
        text(&run(&temp.path, &["why", "src/lib.rs", "--json"], None).stdout),
        BASELINE_JSON
    );
    assert_eq!(
        text(&run(&temp.path, &["why", "build_repo", "--all"], None).stdout),
        BASELINE_SYMBOL
    );

    // The proposal, then the write.
    let p = proposal(&temp, KEY1);
    assert_eq!(p["key"], KEY1);
    assert_eq!(p["unlinked"], 2);
    let n = number_of(&p, "src/lib.rs");
    let docs_before = (
        fs::read(temp.path.join(DOC1)).expect("doc 1"),
        fs::read(temp.path.join(DOC2)).expect("doc 2"),
    );
    let out = write_link(
        &temp,
        KEY1,
        &n.to_string(),
        p["link_id"].as_str().expect("id"),
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    let written: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(written["written"], ".sieve-links.json");
    assert_eq!(written["kept"][0], "src/lib.rs");
    // The next proposal is for the record that is still unlinked.
    assert_eq!(written["next"]["key"], KEY2);
    assert_eq!(written["next"]["unlinked"], 1);
    let sidecar = fs::read_to_string(temp.path.join(".sieve-links.json")).expect("sidecar");
    assert!(
        sidecar.contains(KEY1) && sidecar.contains("src/lib.rs"),
        "{sidecar}"
    );
    // Sieve never writes into a record doc.
    assert_eq!(
        docs_before,
        (
            fs::read(temp.path.join(DOC1)).expect("doc 1"),
            fs::read(temp.path.join(DOC2)).expect("doc 2")
        )
    );

    // The file shows an agent link, never an anchor.
    let body = text(&run(&temp.path, &["why", "src/lib.rs"], None).stdout);
    assert!(
        body.contains("agent link  docs/adr/0001-build-lock.md:"),
        "{body}"
    );
    assert!(body.contains("link: agent link, confirmed "), "{body}");
    assert!(!body.contains("Code anchor"), "{body}");
    let json = run(&temp.path, &["why", "src/lib.rs", "--json"], None);
    let v: serde_json::Value = serde_json::from_slice(&json.stdout).expect("json");
    assert_eq!(v["symbols"][0]["decisions"][0]["level"], "agent");
    // A symbol shows a file-level agent row only with --all.
    let sym = text(&run(&temp.path, &["why", "build_repo"], None).stdout);
    assert!(!sym.contains("agent link"), "{sym}");
    let all = text(&run(&temp.path, &["why", "build_repo", "--all"], None).stdout);
    assert!(all.contains("agent link"), "{all}");

    // The link file is never a graph file.
    let rebuilt = run(&temp.path, &["build"], None);
    assert!(rebuilt.status.success(), "{}", text(&rebuilt.stderr));
    let wiring = fs::read_to_string(temp.path.join("sieve/.graph/wiring.json")).expect("wiring");
    assert!(
        !wiring.contains(".sieve-links"),
        "the link file is in the graph"
    );

    // --check: clean, then a gone file, a changed text and a gone record.
    let clean = text(&run(&temp.path, &["why", "--check"], None).stdout);
    assert!(!clean.contains("stale; run"), "{clean}");
    fs::remove_file(temp.path.join("src/lib.rs")).expect("remove");
    let gone = text(&run(&temp.path, &["why", "--check"], None).stdout);
    assert!(
        gone.contains("1 linked record is stale; run sieve why --link '<key>' for each:"),
        "{gone}"
    );
    assert!(
        gone.contains(&format!("  {KEY1}: a kept file is gone: src/lib.rs")),
        "{gone}"
    );
    fs::write(temp.path.join("src/lib.rs"), LIB).expect("restore");
    let edited = ADR1.replace("waits for the lock", "never waits for the lock");
    fs::write(temp.path.join(DOC1), edited).expect("edit");
    let changed = text(&run(&temp.path, &["why", "--check"], None).stdout);
    assert!(
        changed.contains(&format!("  {KEY1}: the record text changed")),
        "{changed}"
    );
    let body = text(&run(&temp.path, &["why", "src/lib.rs"], None).stdout);
    assert!(!body.contains("agent link"), "{body}");
    let renamed = ADR1.replace("Build takes a lock", "Build takes a mutex");
    fs::write(temp.path.join(DOC1), renamed).expect("rename");
    let missing = text(&run(&temp.path, &["why", "--check"], None).stdout);
    assert!(
        missing.contains(&format!("  {KEY1}: the record is gone")),
        "{missing}"
    );
    fs::write(temp.path.join(DOC1), ADR1).expect("restore doc");

    // --expect refuses after the record changed, and writes nothing.
    let p = proposal(&temp, KEY2);
    let id = p["link_id"].as_str().expect("id").to_string();
    let before = fs::read(temp.path.join(".sieve-links.json")).expect("sidecar");
    fs::write(
        temp.path.join(DOC2),
        ADR2.replace("stays empty", "stays tiny"),
    )
    .expect("edit");
    let out = write_link(&temp, KEY2, "1", &id);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("changed since the proposal"),
        "{}",
        text(&out.stderr)
    );
    fs::write(temp.path.join(DOC2), ADR2).expect("restore doc");
    // A number outside the proposal also writes nothing.
    let out = write_link(&temp, KEY2, "999", &id);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        before,
        fs::read(temp.path.join(".sieve-links.json")).expect("sidecar")
    );

    // The last record is linked with nothing kept: no record is left.
    let none = write_link(&temp, KEY2, "none", &id);
    assert!(none.status.success(), "{}", text(&none.stderr));
    let v: serde_json::Value = serde_json::from_slice(&none.stdout).expect("json");
    assert_eq!(v["kept"].as_array().map(Vec::len), Some(0));
    assert!(v["next"].is_null(), "{v}");
    let done = text(&run(&temp.path, &["why", "--link"], None).stdout);
    assert_eq!(done, "every record is linked\n");
}

/// The stdin of a Read hook call.
fn read_stdin(temp: &TempDir, session: &str, agent: Option<&str>) -> String {
    let file = fs::canonicalize(temp.path.join("src/other.rs")).expect("canon");
    let agent = agent.map_or(String::new(), |a| format!(r#""agent_id":"{a}","#));
    format!(
        r#"{{{agent}"session_id":"{session}","tool_name":"Read","tool_input":{{"file_path":"{}"}}}}"#,
        file.display()
    )
}

fn hook_text(temp: &TempDir, stdin: &str) -> String {
    let out = run(&temp.path, &["hook", "post-read"], Some(stdin));
    text(&out.stdout)
}

#[test]
fn test_link_note_fires_once_per_main_session_and_never_in_a_subagent() {
    use std::os::unix::fs::PermissionsExt;
    let temp = project("link-note");
    // A subagent never gets the note and never uses up the main session's.
    let sub = hook_text(&temp, &read_stdin(&temp, "s1", Some("a1")));
    assert!(!sub.contains("no confirmed code link"), "{sub}");
    let first = hook_text(&temp, &read_stdin(&temp, "s1", None));
    assert!(
        first.contains("sieve: 2 decision records have no confirmed code links."),
        "{first}"
    );
    assert!(first.contains("run sieve why --link"), "{first}");
    assert!(!first.contains("tokens"), "{first}");
    let again = hook_text(&temp, &read_stdin(&temp, "s1", None));
    assert!(!again.contains("no confirmed code link"), "{again}");
    // A new session gets the note again.
    let other = hook_text(&temp, &read_stdin(&temp, "s2", None));
    assert!(other.contains("no confirmed code links"), "{other}");

    // A session write that fails gives no note now.
    let sessions = temp.path.join("sieve/.cache/session");
    fs::create_dir_all(&sessions).expect("sessions dir");
    fs::set_permissions(&sessions, fs::Permissions::from_mode(0o555)).expect("lock dir");
    let blocked = hook_text(&temp, &read_stdin(&temp, "s4", None));
    fs::set_permissions(&sessions, fs::Permissions::from_mode(0o755)).expect("unlock dir");
    assert!(!blocked.contains("no confirmed code link"), "{blocked}");

    // The note stops when every record is linked.
    link_lib(&temp);
    let p = proposal(&temp, KEY2);
    let out = write_link(&temp, KEY2, "none", p["link_id"].as_str().expect("id"));
    assert!(out.status.success(), "{}", text(&out.stderr));
    let body = hook_text(&temp, &read_stdin(&temp, "s3", None));
    assert!(!body.contains("no confirmed code link"), "{body}");
}

#[test]
fn test_link_flags_that_do_not_combine_are_refused() {
    // The flag check runs before any build, so the folder holds no project.
    let temp = TempDir::new("link-flags");
    for args in [
        &["why", "--link", "--check"][..],
        &["why", "--link", "--diff"][..],
        &["why", "--keep", "1", "--expect", "abcd1234"][..],
        &["why", "--link", "--keep", "1"][..],
    ] {
        let out = run(&temp.path, args, None);
        assert_eq!(
            out.status.code(),
            Some(1),
            "{args:?}: {}",
            text(&out.stdout)
        );
        assert!(text(&out.stderr).contains("--"), "{args:?}");
    }
}
