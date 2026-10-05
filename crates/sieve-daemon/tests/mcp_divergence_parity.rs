//! Parity tests for the P4 MCP divergences (P4-26, P4-28, P4-29, P4-31,
//! P4-33) against the recorded `mcp-dv/` sessions
//! (`tests/fixtures/basic.expected/mcp-dv/`).
//!
//! The compare is by reply id, then by compact bytes, the same way
//! `parity_mcp.rs` compares the three main sessions: Sieve answers the
//! three id-less-method lines before the tool calls, so line order is
//! not part of the oracle.

#[path = "support/golden.rs"]
mod golden;

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
        let path = std::env::temp_dir().join(format!("sieve-daemon-dv-{label}-{pid}-{n}"));
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

fn fixture_dir() -> PathBuf {
    manifest_dir().join("../../tests/fixtures/basic")
}

fn expected_dir() -> PathBuf {
    manifest_dir().join("../../tests/fixtures/basic.expected/mcp-dv")
}

fn request_file(id: &str) -> PathBuf {
    manifest_dir().join(format!("../../tests/inputs/mcp-dv/{id}.ndjson"))
}

/// Builds the graph tree a fresh `sieve build` would write (mirrors
/// `parity_mcp.rs`).
fn build_graph_tree(root: &Path, context_dir: &Path) {
    let report = sieve_parse::build_graph_cached(root, context_dir).expect("build graph");
    write_graph(
        &report.graph,
        &context_dir.join(".graph").join("wiring.json"),
    )
    .expect("write wiring.json");
    write_ask_index(
        &ask_index_path(context_dir),
        &build_ask_index(&report.graph),
    )
    .expect("write ask index");
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

/// Runs `serve` over one request file and returns `(stdout, stderr, exit)`.
fn run_session(root: &Path, context_dir: &Path, id: &str) -> (String, String, i32) {
    let input = fs::read_to_string(request_file(id)).expect("read request file");
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

/// Every reply line keyed by its `id` JSON text; a reply with no id
/// fails the test, because no `mcp-dv` request expects one.
fn index_replies(stdout: &str) -> HashMap<String, (Value, String)> {
    let mut by_id = HashMap::new();
    for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
        let value: Value =
            serde_json::from_str(line).unwrap_or_else(|e| panic!("reply is not JSON: {line}: {e}"));
        let id = value
            .get("id")
            .unwrap_or_else(|| panic!("reply with no id: {line}"))
            .to_string();
        let old = by_id.insert(id.clone(), (value, line.to_string()));
        assert!(old.is_none(), "two replies for id {id}");
    }
    by_id
}

fn text_field(value: &Value) -> Option<&str> {
    value
        .get("result")?
        .get("content")?
        .get(0)?
        .get("text")?
        .as_str()
}

/// Asserts the session's replies, stderr and exit match the golden files
/// `<id>.{stdout,stderr,exit}.txt`: the same id set, each reply equal by
/// value and by compact bytes.
fn assert_session_matches(id: &str, stdout: &str, stderr: &str, exit: i32) {
    let dir = expected_dir();
    golden::bless(
        &dir.join(format!("{id}.exit.txt")),
        format!("{exit}\n").as_bytes(),
    );
    golden::bless(&dir.join(format!("{id}.stderr.txt")), stderr.as_bytes());
    golden::bless(&dir.join(format!("{id}.stdout.txt")), stdout.as_bytes());
    let golden_stdout =
        fs::read_to_string(dir.join(format!("{id}.stdout.txt"))).expect("read golden stdout");
    let golden_stderr =
        fs::read_to_string(dir.join(format!("{id}.stderr.txt"))).expect("read golden stderr");
    let golden_exit: i32 = fs::read_to_string(dir.join(format!("{id}.exit.txt")))
        .expect("read golden exit")
        .trim()
        .parse()
        .expect("golden exit is an int");

    assert_eq!(exit, golden_exit, "{id}: exit code");
    assert_eq!(stderr, golden_stderr, "{id}: stderr");

    let got = index_replies(stdout);
    let want = index_replies(&golden_stdout);
    let mut got_ids: Vec<&String> = got.keys().collect();
    let mut want_ids: Vec<&String> = want.keys().collect();
    got_ids.sort();
    want_ids.sort();
    assert_eq!(got_ids, want_ids, "{id}: reply id set");

    let mut failures = Vec::new();
    for (reply_id, (golden_value, golden_line)) in &want {
        let (got_value, got_line) = &got[reply_id];
        if golden_value != got_value {
            if let (Some(gt), Some(ot)) = (text_field(golden_value), text_field(got_value)) {
                if gt != ot {
                    eprintln!("{id}: id {reply_id}: text field differs:");
                    eprintln!("  golden: {gt:?}");
                    eprintln!("  got:    {ot:?}");
                }
            }
            failures.push(format!(
                "id {reply_id} mismatch:\n  golden: {golden_line}\n  got:    {got_line}"
            ));
            continue;
        }
        let golden_compact = serde_json::to_string(golden_value).unwrap();
        let got_compact = serde_json::to_string(got_value).unwrap();
        if golden_compact != got_compact {
            failures.push(format!("id {reply_id}: same value, different key order"));
        }
    }
    assert!(failures.is_empty(), "{id}:\n{}", failures.join("\n"));
}

/// P4-26, P4-28, P4-31, P4-33: the tool-argument coercions, the
/// `protocolVersion` echo, the `method not found` text, the silent
/// non-object lines, the ask savings measure, the backticked unknown
/// symbol text, the bare regex error and the fractional `max_dirs` note.
#[test]
fn test_p4_26_28_31_33_mcp_divergence_session_matches_golden() {
    let temp = TempDir::new("basic");
    copy_dir(&fixture_dir(), &temp.path);
    let context_dir = temp.path.join("sieve");
    build_graph_tree(&temp.path, &context_dir);
    let (stdout, stderr, exit) = run_session(&temp.path, &context_dir, "basic");
    assert_session_matches("basic", &stdout, &stderr, exit);
}

/// P4-29: `tools/list` in a tree with no graph lists no tool.
#[test]
fn test_p4_29_tools_list_is_empty_with_no_graph() {
    let temp = TempDir::new("nograph");
    copy_dir(&fixture_dir(), &temp.path);
    let context_dir = temp.path.join("sieve");
    let (stdout, stderr, exit) = run_session(&temp.path, &context_dir, "nograph");
    assert_session_matches("nograph", &stdout, &stderr, exit);
}

/// P4-29: a dir that holds only `sieve/workspace.json` still lists the
/// six tools.
#[test]
fn test_p4_29_tools_list_is_full_with_only_a_workspace_index() {
    let temp = TempDir::new("workspace");
    let context_dir = temp.path.join("sieve");
    fs::create_dir_all(&context_dir).expect("create sieve dir");
    fs::write(
        context_dir.join("workspace.json"),
        "{\"version\":1,\"children\":[]}",
    )
    .expect("write workspace.json");
    let (stdout, stderr, exit) = run_session(&temp.path, &context_dir, "workspace");
    assert_session_matches("workspace", &stdout, &stderr, exit);
}
