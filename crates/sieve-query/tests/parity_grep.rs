//! Parity test: `grep_graph` plus `format_grep_result` against the
//! `grep-*` goldens, for every fixture in `tests/inputs/queries/`.
//!
//! Cites P1-12 to P1-16 and P3-16 to P3-19.

use std::fs;
use std::path::PathBuf;

use sieve_core::Graph;
use sieve_query::grep::{
    format_grep_result, grep_graph, zero_hit_note, GrepOptions, DEFAULT_MAX_HITS,
};

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

struct ParsedQuery {
    pattern: String,
    opts: GrepOptions,
}

fn parse_grep_query(args: &str) -> ParsedQuery {
    let tokens = tokenize(args);
    // tokens[0] is the command name, "grep".
    let pattern = tokens[1].clone();
    let mut opts = GrepOptions {
        ignore_case: false,
        fixed: false,
        in_prefix: None,
        max_hits: DEFAULT_MAX_HITS,
    };
    let mut i = 2;
    while i < tokens.len() {
        match tokens[i].as_str() {
            "--fixed" => opts.fixed = true,
            "-i" => opts.ignore_case = true,
            "--in" => {
                i += 1;
                opts.in_prefix = Some(tokens[i].clone());
            }
            "--json" => {}
            _ => {}
        }
        i += 1;
    }
    ParsedQuery { pattern, opts }
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
        .filter(|(id, _)| id.starts_with("grep-"))
        .collect()
}

#[test]
fn test_p1_12_to_16_p3_16_to_19_grep_matches_golden() {
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
    let mut total_ran = 0usize;

    for fixture in fixtures {
        let repo_root = fixtures_root().join(fixture);
        let expected_dir = fixtures_root().join(format!("{fixture}.expected"));
        let graph_path = expected_dir.join("wiring.json");
        let graph_text = fs::read_to_string(&graph_path).expect("read wiring.json");
        let graph: Graph = serde_json::from_str(&graph_text).expect("parse wiring.json");

        for (id, args) in load_query_lines(fixture) {
            if id == "grep-json" {
                continue; // The CLI crate covers --json.
            }
            total_ran += 1;

            let stdout_golden_path = expected_dir
                .join("queries")
                .join(format!("{id}.stdout.txt"));
            let stderr_golden_path = expected_dir
                .join("queries")
                .join(format!("{id}.stderr.txt"));
            let stdout_golden = fs::read_to_string(&stdout_golden_path)
                .unwrap_or_else(|_| panic!("read golden {}", stdout_golden_path.display()));
            let stderr_golden = fs::read_to_string(&stderr_golden_path)
                .unwrap_or_else(|_| panic!("read golden {}", stderr_golden_path.display()));

            let parsed = parse_grep_query(&args);
            let result = grep_graph(&graph, &repo_root, &parsed.pattern, &parsed.opts);

            if stdout_golden.trim().is_empty() {
                let last_stderr_line = stderr_golden.lines().last().unwrap_or("");
                if let Some(want) = last_stderr_line.strip_prefix("sieve: ") {
                    // An error case.
                    match result {
                        Ok(_) => failures.push(format!("{fixture}/{id}: expected an error")),
                        Err(err) => {
                            let got = err.to_string();
                            if got != want {
                                failures.push(format!(
                                    "{fixture}/{id}: error mismatch\n  want: {want}\n  got:  {got}"
                                ));
                            }
                        }
                    }
                } else {
                    // A zero-hit case.
                    match result {
                        Err(err) => {
                            failures.push(format!("{fixture}/{id}: unexpected error: {err}"))
                        }
                        Ok(result) => {
                            let got = zero_hit_note(&result);
                            if got != last_stderr_line {
                                failures.push(format!(
                                    "{fixture}/{id}: zero-hit note mismatch\n  want: {last_stderr_line}\n  got:  {got}"
                                ));
                            }
                        }
                    }
                }
                continue;
            }

            match result {
                Err(err) => failures.push(format!("{fixture}/{id}: unexpected error: {err}")),
                Ok(result) => {
                    let got = format_grep_result(&result);
                    let want = strip_savings_header(stdout_golden.trim_end_matches('\n'));
                    let got = got.trim_end_matches('\n');
                    let want = want.trim_end_matches('\n');
                    if got != want {
                        failures.push(format!(
                            "{fixture}/{id}: stdout mismatch\n--- want ---\n{want}\n--- got ---\n{got}"
                        ));
                    }
                }
            }
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    // `workspace` and `pnpm` carry no `grep-*` queries. Assert at least one
    // id ran across every fixture, so an empty query list never lets the
    // test pass without checking anything.
    assert!(total_ran > 0, "no grep-* query ran in any fixture");
}
