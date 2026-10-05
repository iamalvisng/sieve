//! Parity test for `check_graph`/`check_context` (P1-33 to P1-36, P4-42),
//! pinned against the goldens recorded for the `basic` and
//! `multi` fixtures.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sieve_core::write_graph;
use sieve_parse::format_graph_check_report;
use sieve_parse::{build_graph_cached, check_context, check_graph, format_check_report};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn manifest_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
}

fn repo_fixture(name: &str) -> PathBuf {
    manifest_dir().join(format!("../../tests/fixtures/{name}"))
}

fn golden_queries(name: &str, file: &str) -> PathBuf {
    manifest_dir().join(format!(
        "../../tests/fixtures/{name}.expected/queries/{file}"
    ))
}

fn read_golden(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("read golden {}: {e}", path.display()))
}

/// A throwaway copy of a fixture repo, so a test can edit and rebuild
/// without touching the checked-in fixture under `tests/fixtures/`.
struct TempRepo {
    path: PathBuf,
}

impl TempRepo {
    fn from_fixture(name: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .expect("system clock before epoch")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("sieve-check-parity-{name}-{pid}-{n}-{nanos}"));
        let _ = fs::remove_dir_all(&path);
        copy_dir_all(&repo_fixture(name), &path).expect("copy fixture");
        TempRepo { path }
    }
}

impl Drop for TempRepo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &to)?;
        } else {
            fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

/// Builds and writes `wiring.json` for `repo`, the same way `sieve build`
/// does, so `check_graph` has a committed graph to diff against.
fn build_and_commit(repo: &Path, context_dir: &Path) {
    let report =
        build_graph_cached(repo, context_dir).expect("build_graph_cached should not error");
    write_graph(
        &report.graph,
        &context_dir.join(".graph").join("wiring.json"),
    )
    .expect("write_graph should not error");
}

/// Assembles `sieve check`'s stdout, the way CLI assembly (spec section
/// 5.5) does: the markdown-layer line, a blank line, the graph report,
/// one trailing newline.
fn assemble_stdout(c: &sieve_parse::ContextCheck, g: &sieve_parse::GraphCheck) -> String {
    let markdown_line = if c.missing {
        "deep layer: not built — wiring graph is the source of truth".to_string()
    } else {
        format_check_report(c)
    };
    let graph_line = format_graph_check_report(g);
    format!("{markdown_line}\n\n{graph_line}\n")
}

#[test]
fn test_p1_33_to_36_p4_42_check_matches_golden() {
    for name in ["basic", "multi"] {
        let repo = TempRepo::from_fixture(name);
        let context_dir = repo.path.join("sieve");
        build_and_commit(&repo.path, &context_dir);

        let g = check_graph(&repo.path, &context_dir).expect("check_graph should not error");
        let c = check_context(&repo.path, &context_dir);

        let golden_stdout = read_golden(&golden_queries(name, "check.stdout.txt"));
        assert_eq!(
            assemble_stdout(&c, &g),
            golden_stdout,
            "check stdout mismatch for fixture {name}"
        );
        assert!(c.missing, "a Tier-1 fixture never writes manifest.json");
        assert!(
            g.ok,
            "a freshly committed graph must report OK for fixture {name}"
        );

        if name == "basic" {
            // A struct, not `serde_json::json!`: the macro round-trips
            // through `Value`, whose map re-sorts keys alphabetically
            // without the (unavailable, dependency-adding) `preserve_order`
            // feature. A struct's derived `Serialize` writes fields in
            // declaration order directly, which is what the golden pins.
            #[derive(serde::Serialize)]
            struct CheckOutput<'a> {
                context: &'a sieve_parse::ContextCheck,
                graph: &'a sieve_parse::GraphCheck,
            }
            let json = serde_json::to_string_pretty(&CheckOutput {
                context: &c,
                graph: &g,
            })
            .expect("json serializes");
            let golden_json = read_golden(&golden_queries(name, "check-json.stdout.txt"));
            assert_eq!(
                format!("{json}\n"),
                golden_json,
                "check --json mismatch for fixture {name}"
            );

            let golden_exit = read_golden(&golden_queries(name, "check.exit.txt"));
            let bad = !g.missing && !g.ok;
            let exit = if bad { "1" } else { "0" };
            assert_eq!(
                exit,
                golden_exit.trim(),
                "exit code mismatch for fixture {name}"
            );
        }

        // Edit one source file without rebuilding: the graph must now
        // report STALE, with that file's node in `changed`, and the exit
        // rule must give 1.
        let edited = repo.path.join("src/util.ts");
        let before = if name == "basic" {
            edited.clone()
        } else {
            repo.path.join("packages/alpha/src/layout.ts")
        };
        let text = fs::read_to_string(&before).expect("read source file to edit");
        fs::write(&before, format!("{text}\n// edited by the parity test\n"))
            .expect("edit source file");

        let g2 = check_graph(&repo.path, &context_dir).expect("check_graph should not error");
        assert!(
            !g2.ok,
            "an edited file must turn the report STALE for fixture {name}"
        );
        let rel = before
            .strip_prefix(&repo.path)
            .expect("edited path is under the repo root")
            .to_str()
            .expect("path is utf8")
            .replace('\\', "/");
        assert!(
            g2.changed.contains(&rel),
            "changed must name the edited file's node for fixture {name}: {:?}",
            g2.changed
        );
        let bad2 = !g2.missing && !g2.ok;
        assert!(
            bad2,
            "a stale graph must trip the exit-1 rule for fixture {name}"
        );
        assert!(format_graph_check_report(&g2).starts_with("graph check: STALE"));
    }
}

#[test]
fn test_p4_42_mcp_check_freshness_text_matches_golden() {
    let repo = TempRepo::from_fixture("basic");
    let context_dir = repo.path.join("sieve");
    build_and_commit(&repo.path, &context_dir);

    let g = check_graph(&repo.path, &context_dir).expect("check_graph should not error");
    let c = check_context(&repo.path, &context_dir);
    let text = format!(
        "{}\n\n{}",
        format_check_report(&c),
        format_graph_check_report(&g)
    );

    let session_path =
        manifest_dir().join("../../tests/fixtures/basic.expected/mcp/session.stdout.txt");
    let session = read_golden(&session_path);
    let mut found = None;
    for line in session.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let msg: serde_json::Value = serde_json::from_str(line).expect("mcp line parses as json");
        if msg.get("id") == Some(&serde_json::json!(11)) {
            found = Some(msg);
            break;
        }
    }
    let msg = found.expect("mcp session golden has a line with id 11");
    let golden_text = msg["result"]["content"][0]["text"]
        .as_str()
        .expect("text field is a string");
    assert_eq!(text, golden_text);
}

/// P1-33: the check keys the committed nodes by id in a `Map`,
/// so a node id repeated in `wiring.json` counts once. A merge-conflict
/// repo writes and `c.ts#c` three times. The golden reported
/// `nodes: 5` and `pending: 5` for this graph (9 node records).
#[test]
fn test_p1_33_check_counts_a_repeated_node_id_once() {
    let _ = manifest_dir();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let repo = std::env::temp_dir().join(format!("sieve-check-dup-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&repo);
    fs::create_dir_all(&repo).expect("mkdir");
    fs::write(
        repo.join("a.ts"),
        "import {c} from \"./c\"; export function a(){ return c(); }\n",
    )
    .expect("write a.ts");
    fs::write(repo.join("b.ts"), "export const b = 1\n").expect("write b.ts");
    fs::write(repo.join("c.ts"), "export function c(){ return 1 }\n").expect("write c.ts");
    let context_dir = repo.join("sieve");
    build_and_commit(&repo, &context_dir);

    let wiring = context_dir.join(".graph").join("wiring.json");
    let mut doc: serde_json::Value =
        serde_json::from_slice(&fs::read(&wiring).expect("read wiring")).expect("parse wiring");
    let nodes = doc["nodes"].as_array_mut().expect("nodes array");
    let c_nodes: Vec<serde_json::Value> = nodes
        .iter()
        .filter(|n| n["path"] == "c.ts")
        .cloned()
        .collect();
    nodes.extend(c_nodes.iter().cloned());
    nodes.extend(c_nodes);
    assert_eq!(nodes.len(), 9);
    fs::write(&wiring, serde_json::to_vec_pretty(&doc).expect("encode")).expect("write wiring");

    let g = check_graph(&repo, &context_dir).expect("check_graph should not error");
    let _ = fs::remove_dir_all(&repo);
    assert!(g.ok);
    assert_eq!(g.nodes, 5);
    assert_eq!(g.pending, 5);
    assert_eq!(g.pending_ids, ["a.ts", "a.ts#a", "b.ts", "c.ts", "c.ts#c"]);
}
