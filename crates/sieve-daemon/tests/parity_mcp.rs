//! Parity test: `serve` against the captured `mcp/` sessions for the
//! `basic`, `multi`, and `edges` fixtures (P4-26 to P4-33).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;
use sieve_core::askindex::{ask_index_path, build_ask_index, write_ask_index};
use sieve_core::fingerprint::{fingerprint_path, write_fingerprint, Fingerprint};
use sieve_core::write_graph;
use sieve_daemon::serve;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A temp dir that removes itself on drop.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("sieve-daemon-{label}-{pid}-{n}"));
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

fn manifest_dir() -> PathBuf {
    PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
}

fn fixtures_root() -> PathBuf {
    manifest_dir().join("../../tests/fixtures")
}

fn parity_root() -> PathBuf {
    manifest_dir().join("../../tests/inputs")
}

/// Builds the graph tree a fresh `sieve build` would write: `wiring.json`,
/// the ask sidecar, and the fingerprint. Mirrors `sieve-cli/src/build.rs`
/// (this crate cannot depend on the CLI binary).
fn build_graph_tree(root: &Path, context_dir: &Path) {
    let report = sieve_parse::build_graph_cached(root, context_dir).expect("build graph");
    let wiring_path = context_dir.join(".graph").join("wiring.json");
    write_graph(&report.graph, &wiring_path).expect("write wiring.json");

    let ask_index = build_ask_index(&report.graph);
    write_ask_index(&ask_index_path(context_dir), &ask_index).expect("write ask index");

    let stamp = sieve_parse::extractor_stamp();
    let fingerprint = Fingerprint {
        version: 1,
        extractor: stamp.clone(),
        files: report.prints,
        only_dirs: None,
    };
    write_fingerprint(&fingerprint_path(context_dir, &stamp), &fingerprint)
        .expect("write fingerprint");
}

/// Reads every NDJSON reply line into a map keyed by its `id`'s raw JSON
/// text (`"1"`, `"null"`, ...), with every id-less line collected
/// separately as a multiset.
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
        let value: Value = serde_json::from_str(line).unwrap_or_else(|e| {
            panic!("reply line is not JSON: {line}: {e}");
        });
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

/// Drops every `⬆` stderr line: Sieve's MCP server prints no update
/// nudge, the one thing the boot upkeep prints on every run of the
/// captured goldens.
fn strip_nudge_lines(stderr: &str) -> String {
    stderr
        .lines()
        .filter(|line| !line.starts_with('⬆'))
        .map(|line| format!("{line}\n"))
        .collect()
}

/// Strips the `"⬆ ... \n\n"` update-nudge prefix from a golden
/// `instructions` field, so the rest of the value can compare against
/// Sieve's own nudge-free `instructions` text.
fn strip_nudge_prefix(text: &str) -> &str {
    match text.strip_prefix('⬆') {
        Some(rest) => match rest.find("\n\n") {
            Some(idx) => &rest[idx + 2..],
            None => text,
        },
        None => text,
    }
}

/// Recursively drops the update-nudge prefix from every `instructions`
/// string field in a JSON value, in place.
fn strip_nudge_in_value(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(s)) = map.get_mut("instructions") {
                *s = strip_nudge_prefix(s).to_string();
            }
            for v in map.values_mut() {
                strip_nudge_in_value(v);
            }
        }
        Value::Array(items) => {
            for v in items.iter_mut() {
                strip_nudge_in_value(v);
            }
        }
        _ => {}
    }
}

/// Compares one golden reply line against every reply Sieve produced for
/// the same id: first by JSON value equality (with the nudge stripped
/// from both sides), then by the compact re-serialization bytes, to catch
/// a key-order mismatch. Panics with a readable diff on a mismatch; a
/// `text` field mismatch prints both texts line by line.
fn assert_reply_matches(golden_line: &str, got: &[(Value, String)]) {
    let mut golden_value: Value = serde_json::from_str(golden_line).expect("golden line is JSON");
    strip_nudge_in_value(&mut golden_value);

    let id = golden_value.get("id").cloned();
    assert!(
        !got.is_empty(),
        "no reply for id {id:?}; golden line: {golden_line}"
    );

    // `tools/call` replies may legitimately reorder relative to other
    // calls; take the (only) reply matching this id.
    let (mut got_value, got_line) = got[0].clone();
    strip_nudge_in_value(&mut got_value);

    if golden_value != got_value {
        if let (Some(gt), Some(ot)) = (text_field(&golden_value), text_field(&got_value)) {
            if gt != ot {
                eprintln!("id {id:?}: text field differs:");
                for (i, (g, o)) in gt.lines().zip(ot.lines()).enumerate() {
                    if g != o {
                        eprintln!("  line {i}: golden = {g:?}");
                        eprintln!("  line {i}:    got = {o:?}");
                    }
                }
            }
        }
        panic!("id {id:?} mismatch:\n  golden: {golden_line}\n  got:    {got_line}");
    }

    let golden_compact = serde_json::to_string(&golden_value).unwrap();
    let got_compact = serde_json::to_string(&got_value).unwrap();
    assert_eq!(
        golden_compact, got_compact,
        "id {id:?}: same value, different key order"
    );
}

fn text_field(value: &Value) -> Option<&str> {
    value
        .get("result")?
        .get("content")?
        .get(0)?
        .get("text")?
        .as_str()
}

/// Runs `serve` over `request_file`'s lines, against `root`/`context_dir`,
/// and returns `(stdout, stderr, exit_code)`.
fn run_session(root: &Path, context_dir: &Path, request_file: &Path) -> (String, String, i32) {
    let input = fs::read_to_string(request_file).expect("read request file");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = serve(
        root,
        context_dir,
        false,
        env!("CARGO_PKG_VERSION"),
        std::io::Cursor::new(input.into_bytes()),
        &mut stdout,
        &mut stderr,
        None,
    );
    (
        String::from_utf8(stdout).expect("utf-8 stdout"),
        String::from_utf8(stderr).expect("utf-8 stderr"),
        exit,
    )
}

/// Asserts one session's stdout, stderr and exit code match the golden
/// files at `expected_dir/<name>.{stdout,stderr,exit}.txt`.
fn assert_session_matches(stdout: &str, stderr: &str, exit: i32, expected_dir: &Path, name: &str) {
    let golden_stdout = fs::read_to_string(expected_dir.join(format!("{name}.stdout.txt")))
        .expect("read golden stdout");
    let golden_stderr = fs::read_to_string(expected_dir.join(format!("{name}.stderr.txt")))
        .expect("read golden stderr");
    let golden_exit = fs::read_to_string(expected_dir.join(format!("{name}.exit.txt")))
        .expect("read golden exit")
        .trim()
        .parse::<i32>()
        .expect("golden exit is an int");

    assert_eq!(exit, golden_exit, "{name}: exit code");
    assert_eq!(
        strip_nudge_lines(stderr),
        strip_nudge_lines(&golden_stderr),
        "{name}: stderr"
    );

    let got = index_replies(stdout);
    let mut golden_id_less = Vec::new();
    for line in golden_stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("{name}: golden line not JSON: {line}: {e}"));
        match value.as_object().and_then(|o| o.get("id")) {
            Some(id) => {
                let key = id.to_string();
                let Some(replies) = got.by_id.get(&key) else {
                    panic!("{name}: no reply for id {id}: golden line: {line}");
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
                strip_nudge_in_value(&mut value);
                serde_json::to_string(&value).expect("reserialize id-less line")
            })
            .collect();
        out.sort();
        out
    };
    assert_eq!(
        normalize(&got.id_less),
        normalize(&golden_id_less),
        "{name}: id-less lines (multiset)"
    );
}

fn run_fixture_session(fixture: &str, session: &str) {
    let fixture_dir = fixtures_root().join(fixture);
    let expected_dir = fixtures_root()
        .join(format!("{fixture}.expected"))
        .join("mcp");
    let request_file = parity_root().join("mcp").join(format!("{fixture}.ndjson"));

    let temp = TempDir::new(&format!("mcp-{fixture}-{session}"));
    copy_dir(&fixture_dir, &temp.path);
    let context_dir = temp.path.join("sieve");
    build_graph_tree(&temp.path, &context_dir);

    let (stdout, stderr, exit) = run_session(&temp.path, &context_dir, &request_file);
    assert_session_matches(&stdout, &stderr, exit, &expected_dir, session);
}

#[test]
fn test_p4_26_to_33_mcp_session_matches_sieve_basic() {
    run_fixture_session("basic", "session");
}

#[test]
fn test_p4_26_to_33_mcp_session_matches_sieve_multi() {
    run_fixture_session("multi", "session");
}

#[test]
fn test_p4_26_to_33_mcp_session_matches_sieve_edges() {
    run_fixture_session("edges", "session");
}
