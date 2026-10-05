//! Parity test: `skeleton` and `callers` against the `skeleton-*` and
//! `callers-*` goldens, for every fixture in `tests/inputs/queries/`.
//!
//! Cites P1-17, P1-18, P1-19 to P1-23, P3-20 to P3-25, P3-32 and P3-33.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use sieve_core::{Graph, Kind, Node};
use sieve_query::callers::{
    callers_saved_paths, edge_walk, format_callers, parse_depth, parse_direction, resolve_symbol,
    to_callers_json, CallersMatchJson, Depth, Direction, FileReader, Hit,
};
use sieve_query::skeleton::{format_skeleton, skeleton, skeleton_saved_paths, SkeletonEntry};

fn manifest_dir() -> PathBuf {
    PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
}

fn fixtures_root() -> PathBuf {
    manifest_dir().join("../../tests/fixtures")
}

/// A minimal shell-style tokenizer: splits on whitespace outside a matching
/// pair of double quotes. The fixture query lines carry no escaped quotes.
fn tokenize(args: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for c in args.chars() {
        match c {
            '"' => in_quotes = !in_quotes,
            c if c.is_whitespace() && !in_quotes => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn strip_savings_header(golden: &str) -> &str {
    // The saving is the last line of a golden. Keep the newline before it.
    let body = golden.strip_suffix('\n').unwrap_or(golden);
    match body.rfind('\n') {
        Some(i) if body[i + 1..].starts_with("[sieve] saved") => &golden[..=i],
        _ => golden,
    }
}

fn load_query_lines(fixture: &str, prefix: &str) -> Vec<(String, String)> {
    let path = fixtures_root()
        .join("../inputs/queries")
        .join(format!("{fixture}.txt"));
    let text = fs::read_to_string(&path).expect("read query file");
    text.lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .filter_map(|line| {
            let (id, args) = line.split_once('\t')?;
            Some((id.to_string(), args.to_string()))
        })
        .filter(|(id, _)| id.starts_with(prefix))
        .collect()
}

fn read_exit(expected_dir: &Path, id: &str) -> i32 {
    let path = expected_dir.join("queries").join(format!("{id}.exit.txt"));
    fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("read {}", path.display()))
        .trim()
        .parse()
        .unwrap_or_else(|_| panic!("parse exit code in {}", path.display()))
}

fn last_stderr_line(expected_dir: &Path, id: &str) -> String {
    let path = expected_dir
        .join("queries")
        .join(format!("{id}.stderr.txt"));
    let text = fs::read_to_string(&path).unwrap_or_else(|_| panic!("read {}", path.display()));
    text.lines().last().unwrap_or("").to_string()
}

/// A minimal unified-style diff: every differing line, want then got, side
/// by side — enough to see the byte the golden disagrees on without a diff
/// crate dependency.
fn unified_diff(want: &str, got: &str) -> String {
    let want_lines: Vec<&str> = want.lines().collect();
    let got_lines: Vec<&str> = got.lines().collect();
    let max = want_lines.len().max(got_lines.len());
    let mut out = String::new();
    for i in 0..max {
        let w = want_lines.get(i).copied().unwrap_or("<missing>");
        let g = got_lines.get(i).copied().unwrap_or("<missing>");
        if w != g {
            out.push_str(&format!("  line {i}:\n  - want: {w}\n  + got:  {g}\n"));
        }
    }
    out
}

/// The savings baseline the CLI attaches to a `--json` payload. Neither
/// `skeleton.rs` nor `callers.rs` carries this field — the CLI adds it —
/// so this test builds it once, the same way `sieve-savings::savings_for`
/// would, without adding that crate as a dev-dependency of `sieve-query`.
#[derive(Serialize)]
struct Saved {
    files: usize,
    #[serde(rename = "baselineChars")]
    baseline_chars: u64,
}

fn saved_for(graph: &Graph, paths: &[String]) -> Option<Saved> {
    let wanted: HashSet<&str> = paths.iter().map(String::as_str).collect();
    let mut baseline_chars = 0u64;
    let mut files = 0usize;
    let mut seen: HashSet<&str> = HashSet::new();
    for node in &graph.nodes {
        if node.kind != Kind::File || !wanted.contains(node.path.as_str()) {
            continue;
        }
        if !seen.insert(node.path.as_str()) {
            continue;
        }
        let Some(chars) = node.chars else {
            continue;
        };
        baseline_chars += chars;
        files += 1;
    }
    (files > 0).then_some(Saved {
        files,
        baseline_chars,
    })
}

// ---- skeleton --------------------------------------------------------

fn parse_skeleton_query(args: &str) -> (String, bool) {
    let tokens = tokenize(args);
    let file = tokens[1].clone();
    let json = tokens[2..].iter().any(|t| t == "--json");
    (file, json)
}

#[derive(Serialize)]
struct SkeletonJson {
    file: String,
    entries: Vec<SkeletonEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    saved: Option<Saved>,
}

fn run_skeleton_case(graph: &Graph, args: &str, json: &mut bool) -> String {
    let (file, as_json) = parse_skeleton_query(args);
    *json = as_json;
    let result = skeleton(Some(graph), &file);
    if as_json {
        let saved = saved_for(graph, &skeleton_saved_paths(&result));
        let payload = SkeletonJson {
            file: result.file.clone(),
            entries: result.entries.clone(),
            note: result.note.clone(),
            saved,
        };
        format!(
            "{}\n",
            serde_json::to_string_pretty(&payload).expect("serialize skeleton")
        )
    } else {
        format_skeleton(&result)
    }
}

#[test]
fn test_p1_17_18_p3_32_33_skeleton_matches_golden() {
    let fixtures = ["basic", "edges", "scopes", "symbols", "multi"];
    let mut failures = Vec::new();

    for fixture in fixtures {
        let expected_dir = fixtures_root().join(format!("{fixture}.expected"));
        let graph_path = expected_dir.join("wiring.json");
        let graph_text = fs::read_to_string(&graph_path).expect("read wiring.json");
        let graph: Graph = serde_json::from_str(&graph_text).expect("parse wiring.json");

        for (id, args) in load_query_lines(fixture, "skeleton-") {
            let stdout_golden_path = expected_dir
                .join("queries")
                .join(format!("{id}.stdout.txt"));
            let stdout_golden = fs::read_to_string(&stdout_golden_path)
                .unwrap_or_else(|_| panic!("read golden {}", stdout_golden_path.display()));

            let mut json = false;
            let got = run_skeleton_case(&graph, &args, &mut json);
            // A file that is not in the index exits 1 in the CLI, with an
            // error line on stderr and nothing on stdout. The library gives
            // the note instead, which has no entries.
            let exit_path = expected_dir.join("queries").join(format!("{id}.exit.txt"));
            let exit_golden = fs::read_to_string(&exit_path).unwrap_or_default();
            if exit_golden.trim() == "1" && !json {
                if !stdout_golden.is_empty() || got.trim().is_empty() {
                    failures.push(format!("{fixture}/{id}: want an error with no stdout"));
                }
                continue;
            }
            let want = strip_savings_header(stdout_golden.trim_end_matches('\n'));
            let got = got.trim_end_matches('\n');
            let want = want.trim_end_matches('\n');
            if got != want {
                failures.push(format!(
                    "{fixture}/{id}: mismatch\n{}",
                    unified_diff(want, got)
                ));
            }
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

// ---- callers ----------------------------------------------------------

struct ParsedCallers {
    symbol: String,
    direction_raw: Option<String>,
    depth_raw: Option<String>,
    in_prefix: Option<String>,
    json: bool,
}

fn parse_callers_query(args: &str) -> ParsedCallers {
    let tokens = tokenize(args);
    let symbol = tokens[1].clone();
    let mut parsed = ParsedCallers {
        symbol,
        direction_raw: None,
        depth_raw: None,
        in_prefix: None,
        json: false,
    };
    let mut i = 2;
    while i < tokens.len() {
        match tokens[i].as_str() {
            "--direction" => {
                i += 1;
                parsed.direction_raw = Some(tokens[i].clone());
            }
            "-d" | "--depth" => {
                i += 1;
                parsed.depth_raw = Some(tokens[i].clone());
            }
            "--in" => {
                i += 1;
                parsed.in_prefix = Some(tokens[i].clone());
            }
            "--json" => parsed.json = true,
            _ => {}
        }
        i += 1;
    }
    parsed
}

/// Everything a `callers` run needs to either render or error: the parsed
/// direction/depth, and the matched symbols with their walked hits.
enum CallersRun<'a> {
    Error(String),
    Ok {
        direction: Direction,
        show_depth: bool,
        matches: Vec<(&'a Node, Vec<Hit<'a>>)>,
    },
}

fn run_callers<'a>(graph: &'a Graph, parsed: &ParsedCallers) -> CallersRun<'a> {
    let direction = match &parsed.direction_raw {
        Some(raw) => match parse_direction(raw) {
            Ok(d) => d,
            Err(err) => return CallersRun::Error(err.to_string()),
        },
        None => Direction::In,
    };
    let depth = match &parsed.depth_raw {
        Some(raw) => match parse_depth(raw) {
            Ok(d) => d,
            Err(err) => return CallersRun::Error(err.to_string()),
        },
        None => Depth(Some(1)),
    };
    let show_depth = !matches!(depth, Depth(Some(n)) if n <= 1);

    let symbols = match resolve_symbol(graph, &parsed.symbol, parsed.in_prefix.as_deref()) {
        Ok(s) => s,
        Err(err) => return CallersRun::Error(err.to_string()),
    };

    let matches = symbols
        .into_iter()
        .map(|symbol| {
            let hits = edge_walk(graph, symbol, direction, depth);
            (symbol, hits)
        })
        .collect();

    CallersRun::Ok {
        direction,
        show_depth,
        matches,
    }
}

#[derive(Serialize)]
struct CallersJsonWithSaved {
    query: String,
    matches: Vec<CallersMatchJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    saved: Option<Saved>,
}

#[test]
fn test_p1_19_to_23_p3_20_to_25_callers_matches_golden() {
    // callx pins P1-21 (whole-word quote) and P3-25 (discovery order).
    let fixtures = ["basic", "edges", "scopes", "symbols", "multi", "callx"];
    let mut failures = Vec::new();

    for fixture in fixtures {
        let repo_root = fixtures_root().join(fixture);
        let expected_dir = fixtures_root().join(format!("{fixture}.expected"));
        let graph_path = expected_dir.join("wiring.json");
        let graph_text = fs::read_to_string(&graph_path).expect("read wiring.json");
        let graph: Graph = serde_json::from_str(&graph_text).expect("parse wiring.json");

        for (id, args) in load_query_lines(fixture, "callers-") {
            let parsed = parse_callers_query(&args);
            let exit = read_exit(&expected_dir, &id);
            let run = run_callers(&graph, &parsed);

            match (exit, run) {
                (1, CallersRun::Error(got)) => {
                    let want_line = last_stderr_line(&expected_dir, &id);
                    let want = want_line.strip_prefix("sieve: ").unwrap_or(&want_line);
                    if got != want {
                        failures.push(format!(
                            "{fixture}/{id}: error mismatch\n  want: {want}\n  got:  {got}"
                        ));
                    }
                }
                (1, CallersRun::Ok { .. }) => {
                    failures.push(format!("{fixture}/{id}: expected an error, got a result"));
                }
                (0, CallersRun::Error(got)) => {
                    failures.push(format!("{fixture}/{id}: unexpected error: {got}"));
                }
                (
                    0,
                    CallersRun::Ok {
                        direction,
                        show_depth,
                        matches,
                    },
                ) => {
                    let stdout_golden_path = expected_dir
                        .join("queries")
                        .join(format!("{id}.stdout.txt"));
                    let stdout_golden = fs::read_to_string(&stdout_golden_path)
                        .unwrap_or_else(|_| panic!("read golden {}", stdout_golden_path.display()));

                    let got = if parsed.json {
                        let saved = saved_for(&graph, &callers_saved_paths(&matches));
                        let payload = to_callers_json(&parsed.symbol, &matches, direction);
                        let with_saved = CallersJsonWithSaved {
                            query: payload.query,
                            matches: payload.matches,
                            saved,
                        };
                        format!(
                            "{}\n",
                            serde_json::to_string_pretty(&with_saved).expect("serialize callers")
                        )
                    } else {
                        let mut reader = FileReader::new(&repo_root);
                        format_callers(&parsed.symbol, &matches, direction, show_depth, &mut reader)
                    };

                    let want = strip_savings_header(stdout_golden.trim_end_matches('\n'));
                    let got = got.trim_end_matches('\n');
                    let want = want.trim_end_matches('\n');
                    if got != want {
                        failures.push(format!(
                            "{fixture}/{id}: mismatch\n{}",
                            unified_diff(want, got)
                        ));
                    }
                }
                (other, _) => {
                    failures.push(format!("{fixture}/{id}: unhandled exit code {other}"));
                }
            }
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
