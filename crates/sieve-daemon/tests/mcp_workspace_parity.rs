//! Parity tests for workspace step 4: the MCP tools at a workspace
//! parent (P4-44), against the recorded sessions under
//! `tests/fixtures/ws.expected/workspace4/mcp-*` (the requests are
//! `tests/inputs/mcp-ws/*.ndjson`).
//!
//! The compare is by reply id: sieve answers the calls of one session in
//! completion order, so line order is not part of the oracle.
//! `sieve_find_code` is not in these sessions. It federates with `ask`;
//! its tests are in `crates/sieve-cli/tests/workspace_step5_parity.rs`.

#[path = "support/golden.rs"]
mod golden;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;
use sieve_core::askindex::{ask_index_path, build_ask_index, write_ask_index};
use sieve_core::fingerprint::{fingerprint_path, write_fingerprint, Fingerprint};
use sieve_core::{workspace, write_graph};
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
        let path = std::env::temp_dir().join(format!("sieve-daemon-ws-{label}-{pid}-{n}"));
        fs::create_dir_all(&path).expect("create temp dir");
        TempDir { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
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

const SHARED_CHAIN: &str = "\nexport function shared(): number {\n  return 1;\n}\n\n\
export function useShared(): number {\n  return shared();\n}\n\n\
export function useSharedTwice(): number {\n  return useShared() + useShared();\n}\n";

/// Appends `text` to the file `rel` under `root`.
fn append(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    let mut body = fs::read_to_string(&path).expect("read source");
    body.push_str(text);
    fs::write(&path, body).expect("write source");
}

/// Builds the graph tree a fresh `sieve build` would write for `root`.
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

/// A workspace parent: the `ws` fixture with the shared chain in both
/// children, each child a git repo with every file staged, each child
/// built, and the parent index `sieve/workspace.json`.
fn built_workspace(label: &str) -> TempDir {
    let temp = TempDir::new(label);
    copy_dir(&manifest_dir().join("../../tests/fixtures/ws"), &temp.path);
    append(&temp.path, "alpha/src/a.ts", SHARED_CHAIN);
    append(&temp.path, "beta/src/b.ts", SHARED_CHAIN);
    for child in ["alpha", "beta"] {
        let root = temp.path.join(child);
        for args in [
            vec!["-c", "init.defaultBranch=main", "init", "-q"],
            vec!["add", "-A"],
        ] {
            let status = Command::new("git")
                .args(&args)
                .current_dir(&root)
                .status()
                .expect("run git");
            assert!(
                status.success(),
                "git {args:?} failed in {}",
                root.display()
            );
        }
        build_graph_tree(&root, &root.join("sieve"));
    }
    let children = vec!["alpha".to_string(), "beta".to_string()];
    workspace::write(&temp.path.join("sieve"), &children).expect("write workspace.json");
    temp
}

/// Runs `serve` at the parent over the request file `<id>.ndjson`, and
/// returns `(stdout, stderr, exit)`.
fn run_session(root: &Path, id: &str) -> (String, String, i32) {
    let request = manifest_dir().join(format!("../../tests/inputs/mcp-ws/{id}.ndjson"));
    let input = fs::read_to_string(request).expect("read request file");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = serve(
        root,
        &root.join("sieve"),
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

/// Every reply keyed by its `id`.
fn index_replies(stdout: &str) -> HashMap<String, Value> {
    let mut by_id = HashMap::new();
    for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
        let value: Value =
            serde_json::from_str(line).unwrap_or_else(|e| panic!("reply is not JSON: {line}: {e}"));
        let id = value
            .get("id")
            .unwrap_or_else(|| panic!("reply with no id: {line}"))
            .to_string();
        assert!(
            by_id.insert(id.clone(), value).is_none(),
            "two replies for id {id}"
        );
    }
    by_id
}

/// Asserts the session matches the golden `mcp-<id>.*`: the same ids, and
/// every reply equal to the golden.
fn assert_session_matches(id: &str, root: &Path) {
    let (stdout, stderr, exit) = run_session(root, id);
    let dir = manifest_dir().join("../../tests/fixtures/ws.expected/workspace4");
    golden::bless(
        &dir.join(format!("mcp-{id}.exit.txt")),
        format!("{exit}\n").as_bytes(),
    );
    golden::bless(&dir.join(format!("mcp-{id}.stderr.txt")), stderr.as_bytes());
    golden::bless(&dir.join(format!("mcp-{id}.stdout.txt")), stdout.as_bytes());
    let read = |ext: &str| {
        fs::read_to_string(dir.join(format!("mcp-{id}.{ext}.txt")))
            .unwrap_or_else(|e| panic!("read golden mcp-{id}.{ext}: {e}"))
    };
    assert_eq!(exit.to_string(), read("exit").trim(), "{id}: exit code");
    assert_eq!(stderr, read("stderr"), "{id}: stderr");
    let got = index_replies(&stdout);
    let want = index_replies(&read("stdout"));
    let mut got_ids: Vec<&String> = got.keys().collect();
    let mut want_ids: Vec<&String> = want.keys().collect();
    got_ids.sort();
    want_ids.sort();
    assert_eq!(got_ids, want_ids, "{id}: reply id set");
    for (reply_id, want_reply) in &want {
        assert_eq!(&got[reply_id], want_reply, "{id}: reply {reply_id}");
    }
}

/// P4-44: at a parent with built children, `tools/list` lists the tools;
/// `trace_calls`, `find_all`, `repo_map` and `check_freshness` federate,
/// each result labeled `<child>/`; `file_api` and an empty `trace_calls`
/// answer as in a single repo.
#[test]
fn test_p4_44_mcp_tools_federate_at_a_workspace_parent() {
    let temp = built_workspace("built");
    assert_session_matches("workspace4", &temp.path);
}

/// P4-44: an unbuilt child drops out of each result, and the coverage
/// note closes it.
#[test]
fn test_p4_44_mcp_tools_add_the_coverage_note_with_a_child_unbuilt() {
    let temp = built_workspace("unbuilt");
    fs::remove_dir_all(temp.path.join("beta/sieve")).expect("remove beta graph");
    assert_session_matches("workspace4-unbuilt", &temp.path);
}

/// P4-44: `check_freshness` at a parent names the stale child, with no
/// refresh first.
#[test]
fn test_p4_44_mcp_check_freshness_names_the_stale_child() {
    let temp = built_workspace("stale");
    append(
        &temp.path,
        "alpha/src/a.ts",
        "\nexport function alphaExtra(): number {\n  return 3;\n}\n",
    );
    assert_session_matches("workspace4-stale", &temp.path);
}

/// P4-44: a federated tool refreshes each child first and prefixes the
/// per-child refresh note.
#[test]
fn test_p4_44_mcp_find_all_refreshes_the_children_first() {
    let temp = built_workspace("refresh");
    append(
        &temp.path,
        "alpha/src/a.ts",
        "\nexport function alphaExtra(): number {\n  return 3;\n}\n",
    );
    assert_session_matches("workspace4-refresh", &temp.path);
}

/// A plain repo builds copy D: `src/a.ts` holds 300
/// functions with a padded body, `src/b.ts` holds one, and the graph is
/// built.
fn built_single_repo(label: &str) -> TempDir {
    let temp = TempDir::new(label);
    fs::create_dir_all(temp.path.join("src")).expect("create src");
    let mut a = String::new();
    for i in 1..=300 {
        a.push_str(&format!(
            "export function alpha{i}(): number {{\n  // padding line one of the body, so the file is long\n  // padding line two of the body, so the file is long\n  // padding line three of the body, so the file is long\n  return {i};\n}}\n\n"
        ));
    }
    fs::write(temp.path.join("src/a.ts"), a).expect("write a.ts");
    fs::write(
        temp.path.join("src/b.ts"),
        "export function beta(): number {\n  return 1;\n}\n",
    )
    .expect("write b.ts");
    build_graph_tree(&temp.path, &temp.path.join("sieve"));
    temp
}

/// P4-31: in a single repo, `find_all` starts with the savings header,
/// as the grep result does.
#[test]
fn test_p4_31_find_all_adds_the_savings_header_in_a_single_repo() {
    let temp = built_single_repo("findall-single");
    assert_session_matches("findall-single", &temp.path);
}
