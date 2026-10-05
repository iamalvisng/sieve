//! Integration tests for the optional LSP enrichment layer (P2-23 to
//! P2-25), against a fake language server this test writes to a temp
//! dir. The fake server is a Python 3 script; a machine with no
//! `python3` on `PATH` skips these tests with a clear message, per the
//! task note that allows this fallback.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use sieve_core::{Confidence, Kind, Relation};
use sieve_parse::build_graph;
use sieve_parse::lsp::{
    enrich_with_lsp_and_extra_path, enrich_with_lsp_report_and_extra_path,
    pick_server_with_extra_path, repo_languages,
};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A guard around one temp-dir path. It derefs to [`Path`], so a caller
/// treats it like a path. On drop, it removes the directory, pass or
/// fail, so a test never leaves a temp dir behind.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// The guard's path, as `&Path`. `PathBuf` has this method; `Path`
    /// itself does not, so a caller needs it here too.
    fn as_path(&self) -> &Path {
        &self.path
    }
}

impl std::ops::Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Builds a temp-dir guard, unique per process, per call, and per
/// nanosecond. The guard removes the directory on drop.
fn unique_temp_dir(label: &str) -> TempDir {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .expect("system clock before epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("sieve-lsp-{label}-{pid}-{n}-{nanos}"));
    TempDir { path }
}

/// `true` when `python3` resolves on `PATH`. The fake server needs it.
fn has_python3() -> bool {
    Command::new("python3")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// A three-function Python fixture: `foo` calls `bar` directly (the AST
/// resolver places this edge already, `confidence: extracted`); `baz`
/// has no caller the AST sees. This lets the LSP layer both re-discover
/// an edge that already exists (P2-25 dedupe) and add a new one (`foo`
/// calling `baz` through the fake server's canned answer).
const FIXTURE_SOURCE: &str =
    "def foo():\n    bar()\n\n\ndef bar():\n    pass\n\n\ndef baz():\n    pass\n";

/// Writes the fake `pyright-langserver` script to `bin_dir`, made
/// executable, that answers `initialize`, `textDocument/prepareCallHierarchy`
/// and `callHierarchy/outgoingCalls` with canned positions for `foo`,
/// `bar` and `baz` at their real 0-indexed LSP lines in `file_uri`.
/// `foo`'s outgoing calls always name `bar` and `baz`; `bar` and `baz`
/// report no outgoing calls.
fn write_fake_server(bin_dir: &Path, file_uri: &str, foo_line: u32, bar_line: u32, baz_line: u32) {
    let script = FAKE_SERVER_TEMPLATE
        .replace("__URI__", file_uri)
        .replace("__FOO_LINE__", &foo_line.to_string())
        .replace("__BAR_LINE__", &bar_line.to_string())
        .replace("__BAZ_LINE__", &baz_line.to_string());
    let path = bin_dir.join("pyright-langserver");
    let mut file = fs::File::create(&path).expect("writes the fake server script");
    file.write_all(script.as_bytes())
        .expect("writes the fake server body");
    drop(file);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&path)
            .expect("reads the script's metadata")
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).expect("makes the script executable");
    }
}

const FAKE_SERVER_TEMPLATE: &str = r#"#!/usr/bin/env python3
import sys, json

def read_message():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        line = line.decode("utf-8", "replace").rstrip("\r\n")
        if line == "":
            break
        if ":" in line:
            k, v = line.split(":", 1)
            headers[k.strip()] = v.strip()
    length = int(headers.get("Content-Length", "0"))
    body = sys.stdin.buffer.read(length)
    return json.loads(body.decode("utf-8")) if body else None

def write_message(obj):
    body = json.dumps(obj).encode("utf-8")
    sys.stdout.buffer.write(("Content-Length: %d\r\n\r\n" % len(body)).encode("ascii"))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()

URI = "__URI__"
FOO_LINE = __FOO_LINE__
BAR_LINE = __BAR_LINE__
BAZ_LINE = __BAZ_LINE__

def item(name, line):
    return {
        "name": name,
        "kind": 12,
        "uri": URI,
        "range": {"start": {"line": line, "character": 0}, "end": {"line": line, "character": 10}},
        "selectionRange": {"start": {"line": line, "character": 4}, "end": {"line": line, "character": 7}},
    }

while True:
    msg = read_message()
    if msg is None:
        break
    method = msg.get("method")
    if method == "initialize":
        write_message({"jsonrpc": "2.0", "id": msg["id"], "result": {"capabilities": {}}})
    elif method == "textDocument/prepareCallHierarchy":
        line = msg["params"]["position"]["line"]
        if line == FOO_LINE:
            result = [item("foo", FOO_LINE)]
        elif line == BAR_LINE:
            result = [item("bar", BAR_LINE)]
        elif line == BAZ_LINE:
            result = [item("baz", BAZ_LINE)]
        else:
            result = []
        write_message({"jsonrpc": "2.0", "id": msg["id"], "result": result})
    elif method == "callHierarchy/outgoingCalls":
        name = msg["params"]["item"]["name"]
        if name == "foo":
            calls = [
                {"to": item("bar", BAR_LINE), "fromRanges": []},
                {"to": item("baz", BAZ_LINE), "fromRanges": []},
            ]
        else:
            calls = []
        write_message({"jsonrpc": "2.0", "id": msg["id"], "result": calls})
    elif "id" in msg:
        write_message({"jsonrpc": "2.0", "id": msg["id"], "result": None})
    # every other message is a notification: no response.
"#;

/// Writes [`FIXTURE_SOURCE`] under a fresh temp repo root and returns
/// `(root, a.py's absolute path)`.
fn write_fixture_repo(label: &str) -> (TempDir, PathBuf) {
    let root = unique_temp_dir(label);
    fs::create_dir_all(&root).expect("creates the fixture repo root");
    let file_path = root.join("a.py");
    fs::write(&file_path, FIXTURE_SOURCE).expect("writes a.py");
    (root, file_path)
}

/// A test-only script dir with a fake `pyright-langserver`, plus the
/// three functions' spans (1-indexed start lines) read off a real,
/// LSP-free build, so the script's canned lines always match whatever
/// line convention the Python extractor actually uses.
struct FakeServerSetup {
    root: TempDir,
    bin_dir: PathBuf,
}

fn setup_fake_server(label: &str) -> FakeServerSetup {
    let (root, file_path) = write_fixture_repo(label);
    let context_dir = root.join("sieve");
    let graph = build_graph(&root, &context_dir).expect("builds the baseline graph");

    let line_of = |name: &str| -> u32 {
        graph
            .nodes
            .iter()
            .find(|n| n.path == "a.py" && n.kind == Kind::Function && n.name == name)
            .and_then(|n| n.span.strip_prefix('L'))
            .and_then(|s| s.split("-L").next())
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or_else(|| panic!("the fixture defines {name}"))
    };
    let foo_line = line_of("foo") - 1; // LSP lines are 0-indexed.
    let bar_line = line_of("bar") - 1;
    let baz_line = line_of("baz") - 1;

    // P2-25's own precondition: the AST resolver already placed
    // `foo` calling `bar`, `confidence: extracted`. The LSP layer must
    // not double it.
    let foo_id = &graph
        .nodes
        .iter()
        .find(|n| n.path == "a.py" && n.name == "foo")
        .expect("foo exists")
        .id;
    let bar_id = &graph
        .nodes
        .iter()
        .find(|n| n.path == "a.py" && n.name == "bar")
        .expect("bar exists")
        .id;
    let has_extracted_foo_calls_bar = graph.edges.iter().any(|e| {
        &e.source == foo_id
            && &e.target == bar_id
            && e.relation == Relation::Calls
            && e.confidence == Confidence::Extracted
    });
    assert!(
        has_extracted_foo_calls_bar,
        "the AST resolver must place foo calling bar before the LSP layer runs"
    );

    let bin_dir = root.join("bin");
    fs::create_dir_all(&bin_dir).expect("creates the fake server's bin dir");
    // A real language server reports the real (canonicalized) path. The fixture
    // matches that so the F1 fix (which canonicalizes `root` before comparing)
    // also matches on an unremarkable temp dir with no extra symlink of its
    // own.
    let canonical_file_path = fs::canonicalize(&file_path).expect("canonicalizes a.py's path");
    let uri = format!("file://{}", canonical_file_path.display());
    write_fake_server(&bin_dir, &uri, foo_line, bar_line, baz_line);

    FakeServerSetup { root, bin_dir }
}

#[test]
fn test_p2_25_lsp_edges_are_calls_lsp_resolved_and_dedupe_by_exact_key() {
    if !has_python3() {
        eprintln!("skip: no python3 on PATH, cannot run the fake LSP server");
        return;
    }
    let setup = setup_fake_server("p2-25");
    let context_dir = setup.root.join("sieve");
    let mut graph = build_graph(&setup.root, &context_dir).expect("builds the graph");
    let edges_before = graph.edges.len();

    let added =
        enrich_with_lsp_and_extra_path(&setup.root, &mut graph, Some(setup.bin_dir.as_path()));

    // `foo` calling `bar` was already an edge (dedupe skips it); `foo`
    // calling `baz` is new. Exactly one edge is added.
    assert_eq!(added, 1, "only the foo-to-baz edge is new");
    assert_eq!(graph.edges.len(), edges_before + 1);

    let foo_id = &graph
        .nodes
        .iter()
        .find(|n| n.path == "a.py" && n.name == "foo")
        .expect("foo exists")
        .id;
    let baz_id = &graph
        .nodes
        .iter()
        .find(|n| n.path == "a.py" && n.name == "baz")
        .expect("baz exists")
        .id;
    let new_edge = graph
        .edges
        .iter()
        .find(|e| &e.source == foo_id && &e.target == baz_id)
        .expect("the foo-to-baz edge exists");
    assert_eq!(new_edge.relation, Relation::Calls);
    assert_eq!(new_edge.confidence, Confidence::LspResolved);

    // Exactly one foo-to-bar edge remains: the LSP layer's duplicate
    // answer for that same call did not add a second one.
    let bar_id = &graph
        .nodes
        .iter()
        .find(|n| n.path == "a.py" && n.name == "bar")
        .expect("bar exists")
        .id;
    let foo_to_bar_count = graph
        .edges
        .iter()
        .filter(|e| &e.source == foo_id && &e.target == bar_id && e.relation == Relation::Calls)
        .count();
    assert_eq!(foo_to_bar_count, 1);
}

#[test]
fn test_p2_23_f1_a_symlinked_root_still_gets_lsp_edges() {
    if !has_python3() {
        eprintln!("skip: no python3 on PATH, cannot run the fake LSP server");
        return;
    }
    let setup = setup_fake_server("p2-23-f1-symlink");
    let context_dir = setup.root.join("sieve");
    let mut graph = build_graph(&setup.root, &context_dir).expect("builds the graph");
    let edges_before = graph.edges.len();

    // A symlink to the real fixture root, elsewhere in the temp parent.
    // The fake server's canned callee URIs always name the real
    // (`setup.root`) path. Passing the symlink as `root` pins the F1 fix:
    // without `canonicalize`, `uri_to_rel` would compare those real-path
    // URIs against the un-resolved symlink path, textually mismatch, and
    // drop every in-repo callee as external.
    let symlink_root = unique_temp_dir("p2-23-f1-symlink-link");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&setup.root, &symlink_root)
            .expect("creates a symlink to the fixture root");
    }
    #[cfg(not(unix))]
    {
        eprintln!("skip: the symlink root test needs a unix symlink");
        return;
    }

    let added =
        enrich_with_lsp_and_extra_path(&symlink_root, &mut graph, Some(setup.bin_dir.as_path()));
    assert_eq!(
        added, 1,
        "the foo-to-baz edge still arrives through the symlinked root"
    );
    assert_eq!(graph.edges.len(), edges_before + 1);
}

#[test]
fn test_p2_23_a_missing_server_leaves_the_graph_unchanged() {
    let (root, _file_path) = write_fixture_repo("p2-23-missing");
    let context_dir = root.join("sieve");
    let mut graph = build_graph(&root, &context_dir).expect("builds the graph");
    let before = graph.clone();

    // An empty, otherwise-real temp dir: no `pyright-langserver` on
    // this `PATH`, so `pick_server` finds nothing.
    let empty_bin_dir = unique_temp_dir("p2-23-empty-bin");
    fs::create_dir_all(&empty_bin_dir).expect("creates an empty bin dir");

    let added = enrich_with_lsp_and_extra_path(&root, &mut graph, Some(empty_bin_dir.as_path()));
    assert_eq!(added, 0);
    assert_eq!(graph, before);
}

#[test]
fn test_p2_23_a_server_that_exits_early_leaves_the_graph_unchanged() {
    if !has_python3() {
        eprintln!("skip: no python3 on PATH, cannot run the fake exiting server");
        return;
    }
    let (root, _file_path) = write_fixture_repo("p2-23-exits");
    let context_dir = root.join("sieve");
    let mut graph = build_graph(&root, &context_dir).expect("builds the graph");
    let before = graph.clone();

    let bin_dir = root.join("bin");
    fs::create_dir_all(&bin_dir).expect("creates the bin dir");
    let path = bin_dir.join("pyright-langserver");
    fs::write(&path, "#!/usr/bin/env python3\nimport sys\nsys.exit(1)\n")
        .expect("writes an early-exit script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&path).expect("reads metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).expect("makes it executable");
    }

    let added = enrich_with_lsp_and_extra_path(&root, &mut graph, Some(bin_dir.as_path()));
    assert_eq!(added, 0);
    assert_eq!(graph, before);
}

#[test]
fn test_p2_24_pick_server_takes_the_first_matching_row_by_priority() {
    if !has_python3() {
        eprintln!("skip: no python3 on PATH, cannot fake either server");
        return;
    }
    // Both `gopls` (go) and `pyright-langserver` (python) sit on the
    // fake `PATH`; the repo speaks both languages. `gopls` sits before
    // `pyright-langserver` in the registry, so it must win.
    let bin_dir = unique_temp_dir("p2-24-bin");
    fs::create_dir_all(&bin_dir).expect("creates the bin dir");
    for name in ["gopls", "pyright-langserver"] {
        let path = bin_dir.join(name);
        fs::write(
            &path,
            "#!/usr/bin/env python3\nimport time\ntime.sleep(60)\n",
        )
        .expect("writes a stub server");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&path).expect("reads metadata").permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&path, perms).expect("makes it executable");
        }
    }

    let mut languages = BTreeSet::new();
    languages.insert("go".to_string());
    languages.insert("python".to_string());

    let resolved = pick_server_with_extra_path(&languages, Some(bin_dir.as_path()))
        .expect("one of the two fake servers resolves");
    assert_eq!(resolved.spec.name, "gopls");
}

/// Writes a stub server script at `bin_dir/<name>` that never answers a
/// request (a sleeping stand-in, same shape as the priority test above):
/// `pick_server_with_extra_path` only needs the command to resolve on
/// `PATH`, it never spawns it.
fn write_stub_server(bin_dir: &Path, name: &str) {
    let path = bin_dir.join(name);
    fs::write(
        &path,
        "#!/usr/bin/env python3\nimport time\ntime.sleep(60)\n",
    )
    .expect("writes a stub server");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&path).expect("reads metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).expect("makes it executable");
    }
}

#[test]
fn test_p2_24_a_mixed_rust_and_typescript_repo_picks_rust_analyzer() {
    if !has_python3() {
        eprintln!("skip: no python3 on PATH, cannot fake either server");
        return;
    }
    // A repo with a breadth-tier file (`.rs`) and a depth-tier file
    // (`.ts`). Before the P2-24 fix, `repo_languages` only saw the
    // depth-tier label, so it dropped `rust` and `pick_server` fell
    // through to the TypeScript row. `rust-analyzer` sits before
    // `typescript-language-server` in the registry, so a correct
    // language set must pick it.
    let root = unique_temp_dir("p2-24-mixed-rust-ts");
    fs::create_dir_all(&root).expect("creates the fixture repo root");
    fs::write(root.join("a.rs"), "fn foo() {}\n").expect("writes a.rs");
    fs::write(root.join("b.ts"), "function bar() {}\n").expect("writes b.ts");
    let context_dir = root.join("sieve");
    let graph = build_graph(&root, &context_dir).expect("builds the graph");

    let bin_dir = unique_temp_dir("p2-24-mixed-rust-ts-bin");
    fs::create_dir_all(&bin_dir).expect("creates the bin dir");
    write_stub_server(&bin_dir, "rust-analyzer");
    write_stub_server(&bin_dir, "typescript-language-server");

    let languages = repo_languages(&graph);
    let resolved = pick_server_with_extra_path(&languages, Some(bin_dir.as_path()))
        .expect("one of the two fake servers resolves");
    assert_eq!(resolved.spec.name, "rust-analyzer");
}

#[test]
fn test_p2_24_a_mixed_c_and_typescript_repo_picks_clangd() {
    if !has_python3() {
        eprintln!("skip: no python3 on PATH, cannot fake either server");
        return;
    }
    // Same defect, a second breadth-tier language: `.c` must count too,
    // and `clangd` sits before the TypeScript row in the registry.
    let root = unique_temp_dir("p2-24-mixed-c-ts");
    fs::create_dir_all(&root).expect("creates the fixture repo root");
    fs::write(root.join("a.c"), "int foo() { return 0; }\n").expect("writes a.c");
    fs::write(root.join("b.ts"), "function bar() {}\n").expect("writes b.ts");
    let context_dir = root.join("sieve");
    let graph = build_graph(&root, &context_dir).expect("builds the graph");

    let bin_dir = unique_temp_dir("p2-24-mixed-c-ts-bin");
    fs::create_dir_all(&bin_dir).expect("creates the bin dir");
    write_stub_server(&bin_dir, "clangd");
    write_stub_server(&bin_dir, "typescript-language-server");

    let languages = repo_languages(&graph);
    let resolved = pick_server_with_extra_path(&languages, Some(bin_dir.as_path()))
        .expect("one of the two fake servers resolves");
    assert_eq!(resolved.spec.name, "clangd");
}

/// Pins the leak fix: the `TempDir` guard removes its directory when it
/// drops, even though nothing in the test calls `remove_dir_all` by hand.
#[test]
fn test_temp_dir_guard_removes_its_directory_on_drop() {
    let dir = unique_temp_dir("guard-drop");
    fs::create_dir_all(&dir).expect("creates the guarded dir");
    let path = dir.path.clone();
    assert!(path.is_dir(), "the guarded dir exists before drop");

    drop(dir);

    assert!(!path.exists(), "drop must remove the guarded dir");
}

#[test]
fn test_p2_23_dv21_report_counts_queried_and_names_the_server() {
    if !has_python3() {
        eprintln!("skip: no python3 on PATH, cannot run the fake LSP server");
        return;
    }
    let setup = setup_fake_server("p2-23-dv21");
    let context_dir = setup.root.join("sieve");
    let mut graph = build_graph(&setup.root, &context_dir).expect("builds the graph");

    let report = enrich_with_lsp_report_and_extra_path(
        &setup.root,
        &mut graph,
        Some(setup.bin_dir.as_path()),
    );

    assert_eq!(report.added, 1);
    assert!(report.queried >= 1, "the server answered a call hierarchy");
    let server = report.server.expect("a server was found");
    assert!(server.ends_with("pyright-langserver"), "got {server}");
}
