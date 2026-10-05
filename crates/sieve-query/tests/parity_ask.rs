//! Parity test: `ask` against the `ask-*` goldens, for every parity
//! fixture (the `ask-ranking.md` note).
//!
//! Cites P3-01 to P3-13.

use std::fs;
use std::path::PathBuf;

use sieve_core::askindex::{read_ask_index, AskIndex};
use sieve_core::Graph;
use sieve_query::ask::{ask, format_ask, format_ask_json, AskMode, AskOptions, DEFAULT_LIMIT};

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

struct ParsedQuery {
    query: String,
    opts: AskOptions,
    json: bool,
}

fn parse_ask_query(args: &str) -> ParsedQuery {
    let tokens = tokenize(args);
    // tokens[0] is "ask".
    let query = tokens[1].clone();
    let mut opts = AskOptions {
        limit: DEFAULT_LIMIT,
        in_prefix: None,
        graph_rank: true,
        source: false,
        full: false,
    };
    let mut json = false;
    let mut i = 2;
    while i < tokens.len() {
        match tokens[i].as_str() {
            "-n" | "--limit" => {
                i += 1;
                opts.limit = tokens[i].parse().unwrap_or(DEFAULT_LIMIT);
            }
            "--in" => {
                i += 1;
                opts.in_prefix = Some(tokens[i].clone());
            }
            "--source" => opts.source = true,
            "--full" => opts.full = true,
            "--no-graph-rank" => opts.graph_rank = false,
            "--json" => json = true,
            _ => {}
        }
        i += 1;
    }
    ParsedQuery { query, opts, json }
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
        .filter(|(id, _)| id.starts_with("ask-"))
        .collect()
}

/// Parses the golden `ask` text output into `(pointer, scope_label)`
/// pairs, one per numbered hit, plus the footer's `matched in:` and
/// `also matched:` lines.
struct GoldenBody {
    pointers: Vec<(String, Option<String>)>,
    footer: Vec<String>,
}

fn parse_golden_body(stdout: &str) -> GoldenBody {
    let mut pointers = Vec::new();
    let mut footer = Vec::new();
    for line in stdout.lines() {
        // A hit row is `<n>  [scope/] name  kind  path:start-end`.
        if let Some(rest) = numbered_hit_rest(line) {
            let (scope, row) = match rest.strip_prefix('[') {
                Some(after) => {
                    let (label, tail) = after.split_once("] ").unwrap_or((after, ""));
                    (Some(label.trim_end_matches('/').to_string()), tail)
                }
                None => (None, rest),
            };
            let pointer = row.rsplit("  ").next().unwrap_or("").trim().to_string();
            pointers.push((pointer, scope));
        }
        if line.starts_with("matched in:") || line.starts_with("also matched:") {
            footer.push(line.to_string());
        }
    }
    GoldenBody { pointers, footer }
}

/// Strips the leading `<n>  ` numbering from a hit row, when present. A
/// snippet line starts with spaces, so it never matches.
fn numbered_hit_rest(line: &str) -> Option<&str> {
    let (num, rest) = line.split_once("  ")?;
    if !num.chars().all(|c| c.is_ascii_digit()) || num.is_empty() {
        return None;
    }
    Some(rest)
}

#[test]
fn test_p3_01_to_13_ask_hit_order_matches_golden() {
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

        let index_path = expected_dir.join("ask-index.json");
        let index: Option<AskIndex> = if index_path.exists() {
            read_ask_index(&index_path)
        } else {
            Some(sieve_core::askindex::build_ask_index(&graph))
        };

        for (id, args) in load_query_lines(fixture) {
            if id == "ask-source" || id == "ask-json" {
                // `--source` rendering and `--json` rendering are not in
                // this task; `test_p3_ask_json_scores_and_coverage_match_sieve`
                // covers the `ask-json` scores directly.
                continue;
            }

            let exit_path = expected_dir.join("queries").join(format!("{id}.exit.txt"));
            let stdout_path = expected_dir
                .join("queries")
                .join(format!("{id}.stdout.txt"));
            let exit_golden = fs::read_to_string(&exit_path)
                .unwrap_or_else(|_| panic!("read golden {}", exit_path.display()));
            let stdout_golden = fs::read_to_string(&stdout_path)
                .unwrap_or_else(|_| panic!("read golden {}", stdout_path.display()));

            let repo_root = fixtures_root().join(fixture);
            let parsed = parse_ask_query(&args);
            let result = ask(
                &graph,
                index.as_ref(),
                &parsed.query,
                &parsed.opts,
                &repo_root,
            );

            if exit_golden.trim() == "1" {
                if result.is_ok() {
                    failures.push(format!("{fixture}/{id}: expected an error"));
                }
                continue;
            }

            let result = match result {
                Ok(r) => r,
                Err(err) => {
                    failures.push(format!("{fixture}/{id}: unexpected error: {err}"));
                    continue;
                }
            };

            if id == "ask-zero" || id == "ask-in" {
                if result.mode != AskMode::Empty {
                    failures.push(format!(
                        "{fixture}/{id}: expected AskMode::Empty, got {:?}",
                        result.mode
                    ));
                }
                continue;
            }

            // A structural golden (`ask-calls`) lists `- title  pointer`
            // lines, not numbered hits, so `parse_golden_body` finds no
            // pointer. The byte-for-byte test below pins that body; here
            // only the mode is checked.
            if stdout_golden
                .lines()
                .next()
                .is_some_and(|l| l.ends_with("\u{b7} structural"))
            {
                if result.mode != AskMode::Structural {
                    failures.push(format!(
                        "{fixture}/{id}: expected AskMode::Structural, got {:?}",
                        result.mode
                    ));
                }
                continue;
            }

            let golden = parse_golden_body(&stdout_golden);
            let got_pointers: Vec<(String, Option<String>)> = result
                .hits
                .iter()
                .map(|h| (sieve_core::voice::pointer(&h.pointer), h.scope.clone()))
                .collect();
            let want_pointers: Vec<String> =
                golden.pointers.iter().map(|(p, _)| p.clone()).collect();
            let got_only_pointers: Vec<String> =
                got_pointers.iter().map(|(p, _)| p.clone()).collect();

            if got_only_pointers != want_pointers {
                failures.push(format!(
                    "{fixture}/{id}: pointer order mismatch\n  want: {want_pointers:?}\n  got:  {got_only_pointers:?}"
                ));
                continue;
            }

            // The `[scope/]` label, when the golden shows one.
            for (i, (_, want_scope)) in golden.pointers.iter().enumerate() {
                let got_scope = result
                    .scopes
                    .is_some()
                    .then(|| got_pointers[i].1.clone())
                    .flatten();
                let got_scope_matches = match want_scope {
                    Some(w) => got_scope.as_deref() == Some(w.as_str()),
                    // The text render never brackets the root scope
                    // (`!scope.is_empty()`, `format.rs`), so parsing the
                    // golden's text cannot tell a root hit's `Some("")`
                    // apart from a genuinely unscoped hit's `None` — both
                    // render bracket-free. Accept either.
                    None => got_scope.is_none() || got_scope.as_deref() == Some(""),
                };
                if !got_scope_matches {
                    failures.push(format!(
                        "{fixture}/{id}: hit {i} scope label mismatch: want {want_scope:?}, got {got_scope:?}"
                    ));
                }
            }

            if !golden.footer.is_empty() {
                if result.scopes.is_none() {
                    failures.push(format!(
                        "{fixture}/{id}: expected a scope footer, got none. Golden footer: {:?}",
                        golden.footer
                    ));
                }
            } else if let Some(scopes) = &result.scopes {
                if !scopes.also_matched.is_empty() {
                    failures.push(format!(
                        "{fixture}/{id}: unexpected alsoMatched footer: {:?}",
                        scopes.also_matched
                    ));
                }
            }
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn test_p3_ask_json_scores_and_coverage_match_golden() {
    let expected_dir = fixtures_root().join("basic.expected");
    let graph: Graph =
        serde_json::from_str(&fs::read_to_string(expected_dir.join("wiring.json")).expect("read"))
            .expect("parse");
    let index = read_ask_index(&expected_dir.join("ask-index.json"));

    let opts = AskOptions::default();
    let result = ask(&graph, index.as_ref(), "double", &opts, &expected_dir).expect("ask succeeds");

    let want_scores = [
        1.441006913009861,
        0.2606308316299293,
        0.7808410916295618,
        0.09787383366036148,
    ];
    let want_pointers = [
        "src/util.ts:L1-L3",
        "src/app.ts:L11-L13",
        "src/util.ts:L5-L7",
        "src/app.ts:L16-L20",
    ];

    for (pointer, want_score) in want_pointers.iter().zip(want_scores.iter()) {
        let hit = result
            .hits
            .iter()
            .find(|h| &h.pointer == pointer)
            .unwrap_or_else(|| panic!("missing hit for {pointer}"));
        let diff = (hit.score - want_score).abs();
        assert!(
            diff < 1e-9,
            "score mismatch for {pointer}: want {want_score}, got {}",
            hit.score
        );
    }

    let coverage = result
        .coverage
        .expect("coverage is set on a lexical result");
    let coverage_strong = result
        .coverage_strong
        .expect("coverageStrong is set on a lexical result");
    assert!((coverage - 1.0).abs() < 1e-12, "coverage: {coverage}");
    assert!(
        (coverage_strong - 1.0).abs() < 1e-12,
        "coverageStrong: {coverage_strong}"
    );
}

#[test]
fn test_p3_05_pagerank_matches_the_sieve_golden_on_basic() {
    let expected_dir = fixtures_root().join("basic.expected");
    let graph: Graph =
        serde_json::from_str(&fs::read_to_string(expected_dir.join("wiring.json")).expect("read"))
            .expect("parse");
    let index = read_ask_index(&expected_dir.join("ask-index.json"));

    let opts = AskOptions::default();
    let result = ask(&graph, index.as_ref(), "double", &opts, &expected_dir).expect("ask succeeds");
    let pr = result.pagerank_for_test.expect("pagerank ran");

    let cases = [
        ("src/util.ts#double", 0.882013826019722),
        ("src/app.ts#App.run", 0.5212616632598586),
        ("src/app.ts#main", 0.19574766732072296),
    ];
    for (id, want) in cases {
        let got = pr.get(id).copied().unwrap_or(0.0);
        assert!(
            (got - want).abs() < 1e-9,
            "pr({id}): want {want}, got {got}"
        );
    }
}

/// Strips a leading `[sieve] tokens saved` header plus its blank line,
/// when the golden carries one (section 7.9). None of the `ask-*` goldens
/// this crate ports carry one — the `--source` savings sentence is empty
/// on every parity fixture (section 11, `basic/ask-source`) — but the
/// stripping runs anyway, so a future fixture with one still passes.
fn strip_savings_header(golden: &str) -> &str {
    // The saving is the last line of a golden. Keep the newline before it.
    let body = golden.strip_suffix('\n').unwrap_or(golden);
    match body.rfind('\n') {
        Some(i) if body[i + 1..].starts_with("[sieve] saved") => &golden[..=i],
        _ => golden,
    }
}

/// A short unified-style diff: the first line where `got` and `want`
/// disagree, with a few lines of context either side.
fn line_diff(want: &str, got: &str) -> String {
    let want_lines: Vec<&str> = want.lines().collect();
    let got_lines: Vec<&str> = got.lines().collect();
    let mismatch = want_lines
        .iter()
        .zip(got_lines.iter())
        .position(|(w, g)| w != g)
        .unwrap_or_else(|| want_lines.len().min(got_lines.len()));

    let mut out = String::new();
    let start = mismatch.saturating_sub(2);
    let end_want = (mismatch + 3).min(want_lines.len());
    let end_got = (mismatch + 3).min(got_lines.len());
    out.push_str("--- want\n");
    for line in &want_lines[start..end_want] {
        out.push_str("  ");
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("+++ got\n");
    for line in &got_lines[start..end_got] {
        out.push_str("  ");
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Parity test: every `ask-*` query, rendered text or `--json`, matches
/// the golden byte for byte (P1-02 to P1-11, P3-04, P3-15). The
/// `askx` fixture pins the `related:` line (P1-07), the 0.35 test-path
/// factor in a `--json` score (P3-04) and the 80-line `--source --full`
/// cap (P3-15).
#[test]
fn test_p1_02_to_11_p3_15_ask_output_matches_golden() {
    let fixtures = [
        "basic",
        "edges",
        "scopes",
        "symbols",
        "multi",
        "workspace",
        "pnpm",
        "askx",
    ];
    let mut failures = Vec::new();

    for fixture in fixtures {
        let expected_dir = fixtures_root().join(format!("{fixture}.expected"));
        let repo_root = fixtures_root().join(fixture);
        let graph: Graph = serde_json::from_str(
            &fs::read_to_string(expected_dir.join("wiring.json")).expect("read wiring.json"),
        )
        .expect("parse wiring.json");

        let index_path = expected_dir.join("ask-index.json");
        let index: Option<AskIndex> = if index_path.exists() {
            read_ask_index(&index_path)
        } else {
            Some(sieve_core::askindex::build_ask_index(&graph))
        };

        for (id, args) in load_query_lines(fixture) {
            let exit_path = expected_dir.join("queries").join(format!("{id}.exit.txt"));
            let exit_golden = fs::read_to_string(&exit_path)
                .unwrap_or_else(|_| panic!("read golden {}", exit_path.display()));
            if exit_golden.trim() != "0" {
                continue;
            }

            let stdout_path = expected_dir
                .join("queries")
                .join(format!("{id}.stdout.txt"));
            let stdout_golden = fs::read_to_string(&stdout_path)
                .unwrap_or_else(|_| panic!("read golden {}", stdout_path.display()));
            let want = strip_savings_header(&stdout_golden);

            let parsed = parse_ask_query(&args);
            let result = match ask(
                &graph,
                index.as_ref(),
                &parsed.query,
                &parsed.opts,
                &repo_root,
            ) {
                Ok(r) => r,
                Err(err) => {
                    failures.push(format!("{fixture}/{id}: unexpected error: {err}"));
                    continue;
                }
            };

            let got = if parsed.json {
                match format_ask_json(&result) {
                    Ok(json) => json,
                    Err(err) => {
                        failures.push(format!("{fixture}/{id}: JSON render failed: {err}"));
                        continue;
                    }
                }
            } else {
                format_ask(&result)
            };

            if got != want {
                failures.push(format!(
                    "{fixture}/{id}: mismatch\n{}",
                    line_diff(want, &got)
                ));
            }
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
