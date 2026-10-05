//! Parity tests for `sieve mcp` (P1-48, P4-26 to P4-33): the CLI wiring
//! around `sieve_daemon::serve` — root resolution, context dir, the
//! ancestor note, and stdin/stdout/stderr piping — against the
//! captured `mcp/` sessions (the `mcp-server.md` note).

mod support;

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde_json::Value;

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

fn fixtures_root() -> PathBuf {
    support::manifest_dir().join("../../tests/fixtures")
}

fn parity_root() -> PathBuf {
    support::manifest_dir().join("../../tests/inputs")
}

/// Builds one fixture into a fresh temp copy and returns the copy's root.
fn build_fixture(name: &str) -> TempDir {
    let fixture = fixtures_root().join(name);
    let temp = TempDir::new(name);
    copy_dir(&fixture, &temp.path);
    let output = support::sieve_command()
        .args(["build", "."])
        .current_dir(&temp.path)
        .output()
        .expect("run sieve build");
    assert_eq!(
        output.status.code(),
        Some(0),
        "sieve build failed for {name}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    temp
}

/// Runs `sieve mcp` with `cwd` as the current directory and no `[dir]`
/// argument, piping `input` to stdin, and returns `(stdout, stderr, exit)`.
/// The HOME is a fresh empty dir, so no update cache exists.
fn run_mcp(cwd: &Path, input: &str) -> (String, String, i32) {
    let home = TempDir::new("mcp-empty-home");
    run_mcp_home(cwd, &home.path, input)
}

/// Like `run_mcp`, with `home` as HOME.
fn run_mcp_home(cwd: &Path, home: &Path, input: &str) -> (String, String, i32) {
    let mut child = support::sieve_command()
        .arg("mcp")
        .env("HOME", home)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn sieve mcp");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(input.as_bytes())
        .expect("write request file to stdin");
    let output = child.wait_with_output().expect("wait for sieve mcp");
    (
        String::from_utf8(output.stdout).expect("utf-8 stdout"),
        String::from_utf8(output.stderr).expect("utf-8 stderr"),
        output.status.code().unwrap_or(-1),
    )
}

/// Replaces every `serverInfo.version` field with a fixed
/// placeholder, in place. Sieve reports its own crate version, not the
/// recorded version, so the two are never equal and both sides normalize to the
/// same placeholder before the value comparison.
fn normalize_in_value(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if let Some(Value::Object(server_info)) = map.get_mut("serverInfo") {
                server_info.insert(
                    "version".to_string(),
                    Value::String("<VERSION>".to_string()),
                );
            }
            for v in map.values_mut() {
                normalize_in_value(v);
            }
        }
        Value::Array(items) => {
            for v in items.iter_mut() {
                normalize_in_value(v);
            }
        }
        _ => {}
    }
}

/// Reads every NDJSON reply line into a map keyed by its `id`'s raw JSON
/// text, with every id-less line collected separately.
struct Replies {
    by_id: HashMap<String, Vec<(Value, String)>>,
    id_less: Vec<String>,
}

fn index_replies(stdout: &str) -> Replies {
    let mut by_id: HashMap<String, Vec<(Value, String)>> = HashMap::new();
    let mut id_less = Vec::new();
    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("reply line is not JSON: {line}: {e}"));
        match value.as_object().and_then(|o| o.get("id")) {
            Some(id) => {
                let key = id.to_string();
                by_id
                    .entry(key)
                    .or_default()
                    .push((value, line.to_string()));
            }
            None => id_less.push(line.to_string()),
        }
    }
    Replies { by_id, id_less }
}

/// Compares one golden reply line against the reply Sieve produced for the
/// same id: by parsed JSON value (after normalization), then by the
/// compact re-serialization bytes, to catch a key-order mismatch.
fn assert_reply_matches(golden_line: &str, got: &[(Value, String)]) {
    let mut golden_value: Value = serde_json::from_str(golden_line).expect("golden line is JSON");
    normalize_in_value(&mut golden_value);

    let id = golden_value.get("id").cloned();
    assert!(
        !got.is_empty(),
        "no reply for id {id:?}; golden line: {golden_line}"
    );

    let (mut got_value, got_line) = got[0].clone();
    normalize_in_value(&mut got_value);

    assert_eq!(
        golden_value, got_value,
        "id {id:?} mismatch:\n  golden: {golden_line}\n  got:    {got_line}"
    );

    let golden_compact = serde_json::to_string(&golden_value).unwrap();
    let got_compact = serde_json::to_string(&got_value).unwrap();
    assert_eq!(
        golden_compact, got_compact,
        "id {id:?}: same value, different key order"
    );
}

/// Asserts one session's stdout, stderr and exit code match the golden
/// files at `expected_dir/session.{stdout,stderr,exit}.txt`.
fn assert_session_matches(stdout: &str, stderr: &str, exit: i32, expected_dir: &Path) {
    let golden_stdout =
        fs::read_to_string(expected_dir.join("session.stdout.txt")).expect("read golden stdout");
    let golden_stderr =
        fs::read_to_string(expected_dir.join("session.stderr.txt")).expect("read golden stderr");
    let golden_exit = fs::read_to_string(expected_dir.join("session.exit.txt"))
        .expect("read golden exit")
        .trim()
        .parse::<i32>()
        .expect("golden exit is an int");

    assert_eq!(exit, golden_exit, "exit code");
    assert_eq!(stderr, golden_stderr, "stderr");

    let got = index_replies(stdout);
    let mut golden_id_less = Vec::new();
    for line in golden_stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("golden line not JSON: {line}: {e}"));
        match value.as_object().and_then(|o| o.get("id")) {
            Some(id) => {
                let key = id.to_string();
                let Some(replies) = got.by_id.get(&key) else {
                    panic!("no reply for id {id}: golden line: {line}");
                };
                assert_reply_matches(line, replies);
            }
            None => golden_id_less.push(line.to_string()),
        }
    }

    let normalize = |lines: &[String]| -> Vec<String> {
        let mut out: Vec<String> = lines
            .iter()
            .map(|line| {
                let mut value: Value = serde_json::from_str(line).expect("id-less line is JSON");
                normalize_in_value(&mut value);
                serde_json::to_string(&value).expect("reserialize id-less line")
            })
            .collect();
        out.sort();
        out
    };
    assert_eq!(
        normalize(&got.id_less),
        normalize(&golden_id_less),
        "id-less lines (multiset)"
    );
}

fn run_fixture_session(fixture: &str) {
    let expected_dir = fixtures_root()
        .join(format!("{fixture}.expected"))
        .join("mcp");
    let request_file = parity_root().join("mcp").join(format!("{fixture}.ndjson"));
    let input = fs::read_to_string(&request_file).expect("read request file");

    let temp = build_fixture(fixture);
    let (stdout, stderr, exit) = run_mcp(&temp.path, &input);
    support::golden::bless_triple(
        &expected_dir,
        "session",
        stdout.as_bytes(),
        stderr.as_bytes(),
        exit,
    );
    assert_session_matches(&stdout, &stderr, exit, &expected_dir);
}

#[test]
fn test_p4_26_to_33_mcp_cli_matches_golden() {
    for fixture in ["basic", "multi", "edges"] {
        run_fixture_session(fixture);
    }
}

#[test]
fn test_p1_48_mcp_prints_the_ancestor_note_first() {
    let temp = build_fixture("basic");
    let sub = temp.path.join("py");
    assert!(sub.is_dir(), "basic fixture should have a py/ subdir");

    let request_file = parity_root().join("mcp").join("basic.ndjson");
    let input = fs::read_to_string(&request_file).expect("read request file");

    let (_stdout, stderr, exit) = run_mcp(&sub, &input);
    assert_eq!(exit, 0, "exit code");

    let expected_root = temp.path.canonicalize().expect("canonicalize temp root");
    let expected_note = format!(
        "[sieve] no sieve/ here — answering from {}/sieve",
        expected_root.display()
    );
    let first_line = stderr.lines().next().unwrap_or("");
    assert_eq!(first_line, expected_note, "first stderr line");
}

/// The shipped `sieve mcp` reports the same version as `sieve --version`.
/// Both read `CURRENT_VERSION`, so the two cannot drift apart.
#[test]
fn test_p1_48_mcp_server_info_version_equals_the_cli_version() {
    let temp = TempDir::new("mcp-version");
    let request = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#;
    let (stdout, _stderr, exit) = run_mcp(&temp.path, &format!("{request}\n"));
    assert_eq!(exit, 0, "exit code");
    let reply: Value = serde_json::from_str(stdout.lines().next().unwrap_or("")).expect("json");
    let mcp_version = reply["result"]["serverInfo"]["version"].as_str();

    let output = support::sieve_command()
        .arg("--version")
        .current_dir(&temp.path)
        .env("HOME", &temp.path)
        .output()
        .expect("run sieve --version");
    let cli_version = String::from_utf8_lossy(&output.stdout);
    assert_eq!(mcp_version, Some(cli_version.trim()));
}

/// Runs `sieve mcp initialize` and returns `serverInfo.version`.
fn server_info_version() -> String {
    let temp = TempDir::new("mcp-version-product");
    let request = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}"#;
    let mut child = support::sieve_command()
        .arg("mcp")
        .env("HOME", &temp.path)
        .current_dir(&temp.path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn sieve mcp");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(format!("{request}\n").as_bytes())
        .expect("write request");
    let output = child.wait_with_output().expect("wait for sieve mcp");
    let out = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let reply: Value = serde_json::from_str(out.lines().next().unwrap_or("")).expect("json");
    reply["result"]["serverInfo"]["version"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

#[test]
fn test_version_mcp_server_info_under_sieve_is_the_crate_version() {
    assert_eq!(server_info_version(), env!("CARGO_PKG_VERSION"));
}

const INIT_REQUEST: &str =
    r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}"#;

fn seed_cache(home: &Path, body: &str) {
    fs::create_dir_all(home.join(".sieve")).expect("create .sieve");
    fs::write(home.join(".sieve").join("update-check.json"), body).expect("write cache");
}

/// P4-57: no cache, or a cache that says the version is current, gives
/// no banner.
#[test]
fn test_p4_57_mcp_is_silent_without_a_newer_cache() {
    let cwd = TempDir::new("mcp-p457-silent-cwd");
    let input = format!("{INIT_REQUEST}\n");
    let cases = [
        None,
        Some(r#"{"latest":"0.21.1","checkedAt":4102444800000}"#),
        Some(r#"{"latest":null,"checkedAt":4102444800000}"#),
    ];
    for cache in cases {
        let home = TempDir::new("mcp-p457-silent-home");
        if let Some(body) = cache {
            seed_cache(&home.path, body);
        }
        let (out, err, _) = run_mcp_home(&cwd.path, &home.path, &input);
        assert_eq!(err, "", "stderr for cache {cache:?}");
        assert!(
            !out.contains('\u{2b06}'),
            "instructions for cache {cache:?}: {out}"
        );
    }
}
