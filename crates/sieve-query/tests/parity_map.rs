//! Parity test: `build_repo_map` plus `format_repo_map` against the
//! `map*` goldens, for every fixture in `tests/inputs/queries/`.
//!
//! Cites P1-29 to P1-32 and P3-27 to P3-31.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use serde::Serialize;
use sieve_core::{Graph, Kind};
use sieve_query::map::{build_repo_map, format_repo_map, map_saved_paths, MapOptions, RepoMap};

fn manifest_dir() -> PathBuf {
    PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
}

fn fixtures_root() -> PathBuf {
    manifest_dir().join("../../tests/fixtures")
}

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

fn parse_map_query(args: &str) -> (MapOptions, bool) {
    let tokens = tokenize(args);
    let mut opts = MapOptions::default();
    let mut json = false;
    let mut i = 1; // tokens[0] is "map".
    while i < tokens.len() {
        match tokens[i].as_str() {
            "--json" => json = true,
            "--max-dirs" => {
                i += 1;
                opts.max_dirs = tokens[i].parse().expect("max-dirs is a positive integer");
            }
            _ => {}
        }
        i += 1;
    }
    (opts, json)
}

fn strip_savings_header(golden: &str) -> &str {
    // The saving is the last line of a golden. Keep the newline before it.
    let body = golden.strip_suffix('\n').unwrap_or(golden);
    match body.rfind('\n') {
        Some(i) if body[i + 1..].starts_with("[sieve] saved") => &golden[..=i],
        _ => golden,
    }
}

fn load_query_lines(fixture: &str) -> Vec<(String, String)> {
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
        .filter(|(id, _)| id == "map" || id.starts_with("map-"))
        .collect()
}

/// The savings baseline the CLI attaches to a `map --json` payload, for
/// the one golden (`map-json`) that needs the full byte-for-byte match.
/// `map.rs` itself never carries this field — the CLI adds it, per the
/// parity note — so this test builds it once, the same way
/// `sieve-savings::savings_for` would, without adding that crate as a
/// dependency of `sieve-query`.
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
    for node in &graph.nodes {
        if node.kind != Kind::File || !wanted.contains(node.path.as_str()) {
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

#[derive(Serialize)]
struct RepoMapWithSaved<'a> {
    totals: &'a sieve_query::map::Totals,
    dirs: &'a [sieve_query::map::DirEntry],
    #[serde(skip_serializing_if = "Option::is_none")]
    scopes: &'a Option<Vec<sieve_query::map::ScopeGroup>>,
    hotspots: &'a [sieve_query::map::Hub],
    dropped: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    saved: Option<Saved>,
}

fn render_json(map: &RepoMap, graph: &Graph) -> String {
    let saved = saved_for(graph, &map_saved_paths(graph));
    let with_saved = RepoMapWithSaved {
        totals: &map.totals,
        dirs: &map.dirs,
        scopes: &map.scopes,
        hotspots: &map.hotspots,
        dropped: map.dropped,
        saved,
    };
    format!(
        "{}\n",
        serde_json::to_string_pretty(&with_saved).expect("serialize RepoMap")
    )
}

#[test]
fn test_p1_29_to_32_p3_27_to_31_map_matches_golden() {
    let fixtures = [
        "basic",
        "edges",
        "scopes",
        "symbols",
        "multi",
        "workspace",
        "pnpm",
    ];
    let mut failures = Vec::new();

    for fixture in fixtures {
        let expected_dir = fixtures_root().join(format!("{fixture}.expected"));
        let graph_path = expected_dir.join("wiring.json");
        let graph_text = fs::read_to_string(&graph_path).expect("read wiring.json");
        let graph: Graph = serde_json::from_str(&graph_text).expect("parse wiring.json");

        for (id, args) in load_query_lines(fixture) {
            let stdout_golden_path = expected_dir
                .join("queries")
                .join(format!("{id}.stdout.txt"));
            let Ok(stdout_golden) = fs::read_to_string(&stdout_golden_path) else {
                failures.push(format!(
                    "{fixture}/{id}: missing golden {}",
                    stdout_golden_path.display()
                ));
                continue;
            };

            let (opts, json) = parse_map_query(&args);
            let map = build_repo_map(&graph, &opts);

            let got = if json {
                render_json(&map, &graph)
            } else {
                format_repo_map(&map)
            };
            let want = strip_savings_header(&stdout_golden);

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

/// A minimal unified-style diff: every differing line, want then got,
/// side by side — enough to see the byte the golden disagrees on without
/// a diff crate dependency.
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
