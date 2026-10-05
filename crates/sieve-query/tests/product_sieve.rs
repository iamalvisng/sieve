//! Rename check: `sieve-query`'s runtime output holds the `sieve` name
//! and never the old name (Phase 2).
//!
//! `sieve_core::product` reads a process-wide `OnceLock`. Cargo runs every
//! test in one file's binary in parallel threads by default, so two tests
//! that each call `set_for_tests` race: whichever thread's code path reads
//! `product()` first wins for the whole process, and the other's pin is
//! silently ignored. One test function that pins the product first, then
//! runs every check, avoids the race.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sieve_core::askindex::read_ask_index;
use sieve_core::{Graph, Meta};
use sieve_query::ask::{ask, format_ask, AskOptions};
use sieve_query::skeleton::{format_skeleton, skeleton};

fn manifest_dir() -> PathBuf {
    PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
}

fn fixtures_root() -> PathBuf {
    manifest_dir().join("../../tests/fixtures")
}

static TEMP_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A unique temp dir under the system temp dir, mirroring `sieve-query`'s
/// own `test_support::TempDir` (`pub(crate)`, unreachable from this
/// integration test). It creates the dir on `new`, and removes it on
/// drop, even if the test panics first.
struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let n = TEMP_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("sieve-query-tests-{label}-{pid}-{n}"));
        fs::create_dir_all(&path).expect("create temp dir");
        TempDir(path)
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn load_basic_graph() -> Graph {
    let expected_dir = fixtures_root().join("basic.expected");
    let text = fs::read_to_string(expected_dir.join("wiring.json")).expect("read wiring.json");
    serde_json::from_str(&text).expect("parse wiring.json")
}

#[test]
fn rename_table_query_output_names_sieve() {
    let expected_dir = fixtures_root().join("basic.expected");
    let graph = load_basic_graph();

    // ask.
    let index = read_ask_index(&expected_dir.join("ask-index.json"));
    let opts = AskOptions::default();
    let ask_result = ask(&graph, index.as_ref(), "double", &opts, &expected_dir).expect("ask runs");
    let ask_rendered = format_ask(&ask_result);
    assert!(
        ask_rendered.contains("sieve ask —"),
        "rendered ask text: {ask_rendered}"
    );

    // skeleton, with a graph.
    let skeleton_result = skeleton(Some(&graph), "src/util.ts");
    let skeleton_rendered = format_skeleton(&skeleton_result);
    assert!(
        skeleton_rendered.contains("sieve skeleton —"),
        "rendered skeleton text: {skeleton_rendered}"
    );

    // skeleton, with no graph — the "run `<product> build` first" note.
    let no_graph_result = skeleton(None, "src/util.ts");
    let no_graph_rendered = format_skeleton(&no_graph_result);
    assert!(
        no_graph_rendered.contains("run `sieve build` first"),
        "rendered skeleton text: {no_graph_rendered}"
    );
}

#[test]
fn rename_table_query_empty_note_names_sieve_dir() {
    let expected_dir = fixtures_root().join("basic.expected");
    let graph = load_basic_graph();
    let index = read_ask_index(&expected_dir.join("ask-index.json"));
    let opts = AskOptions::default();
    let ask_result = ask(
        &graph,
        index.as_ref(),
        "zzz_no_such_term_matches_nothing",
        &opts,
        &expected_dir,
    )
    .expect("ask runs");

    assert_eq!(
        ask_result.note.as_deref(),
        Some("no matching nodes — try different words, or `sieve build` if sieve/ is empty")
    );
}

#[test]
fn rename_table_query_concept_corpus_reads_the_sieve_dir() {
    // The deep golden's top-level concept docs, `INDEX.md` excluded, the
    // same skip test `concept::load_corpus` runs.
    let concept_src = fixtures_root().join("deep.expected/sieve");
    let concept_files: Vec<PathBuf> = fs::read_dir(&concept_src)
        .expect("read deep.expected/sieve")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().map(|n| n.to_string_lossy().to_string());
            matches!(&name, Some(n) if n.ends_with(".md") && n != "INDEX.md")
        })
        .collect();
    let expected_doc_count = concept_files.len();

    let temp = TempDir::new("concept-corpus");
    let sieve_dir = temp.as_ref().join("sieve");
    fs::create_dir_all(&sieve_dir).expect("create sieve dir");
    for path in &concept_files {
        let file_name = path.file_name().expect("concept doc file name");
        fs::copy(path, sieve_dir.join(file_name)).expect("copy concept doc");
    }

    // An empty graph: only the concept corpus can score a hit.
    let graph = Graph {
        meta: Meta {
            version: 1,
            node_count: 0,
            edge_count: 0,
            languages: Vec::new(),
            scopes: Vec::new(),
        },
        nodes: Vec::new(),
        edges: Vec::new(),
    };
    let opts = AskOptions {
        limit: 20,
        ..AskOptions::default()
    };
    // One unique word from each deep-golden concept doc's name, so every
    // doc scores and the round robin keeps every one of them, up to
    // `limit` (`ask/select.rs`'s `select_hits_with_concepts`).
    let query = "module arithmetic calculator component legacy versioning functions";
    let ask_result = ask(&graph, None, query, &opts, temp.as_ref()).expect("ask runs");

    let concept_hit_count = ask_result
        .hits
        .iter()
        .filter(|h| h.kind == "concept")
        .count();
    assert_eq!(
        concept_hit_count, expected_doc_count,
        "hits: {:?}",
        ask_result.hits
    );
}
