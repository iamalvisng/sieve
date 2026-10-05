//! The `sieve ask` subcommand: a ranked-hit query over the indexed graph
//! (the `ask-ranking.md` note).

use std::path::{Path, PathBuf};

use clap::Args;

use sieve_core::askindex::{ask_index_path, read_ask_index};
use sieve_core::workspace;
use sieve_query::ask::{ask, format_ask, format_ask_json, AskOptions, AskSaved};
use sieve_query::workspace::{federate_ask, FederateAskOptions};
use sieve_savings::{ask_pack_region, savings_for, sieve_header, to_tokens, utf16_len};

use crate::query;

/// Flags for `sieve ask`.
#[derive(Args, Debug)]
pub struct AskArgs {
    /// The natural-language question to search the graph for.
    pub query: String,

    /// The repo root. Default: the nearest ancestor with a graph.
    #[arg(value_name = "dir")]
    pub dir: Option<PathBuf>,

    /// The most hits to return.
    #[arg(short = 'n', long = "limit", value_name = "n", default_value_t = sieve_query::ask::DEFAULT_LIMIT)]
    pub limit: usize,

    /// Inlines each hit's source lines.
    #[arg(long)]
    pub source: bool,

    /// Reads a hit's full span, even when a shorter crux excerpt exists.
    #[arg(long)]
    pub full: bool,

    /// Searches only indexed nodes under this path prefix.
    #[arg(long = "in", value_name = "path", allow_hyphen_values = true)]
    pub in_prefix: Option<String>,

    /// Prints the result as JSON, instead of the plain-text report.
    #[arg(long)]
    pub json: bool,

    /// Skips the personalized PageRank pass.
    #[arg(long = "no-graph-rank")]
    pub no_graph_rank: bool,

    /// Skips the pre-query graph refresh.
    #[arg(long = "no-refresh")]
    pub no_refresh: bool,
}

/// Runs `sieve ask`: the query prelude, the sidecar load, the scoring
/// engine, then the report.
pub fn run(args: &AskArgs, dir_override: Option<&Path>) -> Result<(), String> {
    let (root, context_dir) =
        query::run_prelude(args.dir.as_deref(), dir_override, args.no_refresh)?;
    if workspace::read(&context_dir).is_some() {
        return run_workspace(args, &root, &context_dir);
    }
    let graph = query::read_wiring_named(&context_dir)?;
    let index = read_ask_index(&ask_index_path(&context_dir));

    let opts = AskOptions {
        limit: args.limit,
        in_prefix: args.in_prefix.clone(),
        graph_rank: !args.no_graph_rank,
        source: args.source,
        full: args.full,
    };

    let mut result =
        ask(&graph, index.as_ref(), &args.query, &opts, &root).map_err(|err| err.to_string())?;
    // `saved` exists only under `--source` (`baselineFor`),
    // counted once here for both the JSON field and the text sentence.
    if args.source {
        result.saved = source_saved(&graph, &result.hits);
    }

    if args.json {
        let text = format_ask_json(&result).map_err(|e| e.to_string())?;
        print!("{text}");
        return Ok(());
    }

    print!("{}", render_text(&result));
    Ok(())
}

/// The workspace `ask` (P1-56): each child answers, and the merged result
/// prints with no savings line. `--no-graph-rank` never reaches a child. An
/// unknown `--in` child prints its message with no prefix and exits 1.
fn run_workspace(args: &AskArgs, root: &Path, context_dir: &Path) -> Result<(), String> {
    let graphs = sieve_query::workspace::load_children(root, context_dir);
    let opts = FederateAskOptions {
        limit: args.limit,
        source: args.source,
        full: args.full,
        in_prefix: args.in_prefix.clone(),
    };
    let result = match federate_ask(&graphs, &args.query, &opts) {
        Ok(result) => result,
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    };
    if args.json {
        print!("{}", format_ask_json(&result).map_err(|e| e.to_string())?);
    } else {
        print!("{}", format_ask(&result));
    }
    Ok(())
}

/// The `--source` baseline over the hit files, or `None` when no hit names a
/// file with a known size.
fn source_saved(
    graph: &sieve_core::wiring::Graph,
    hits: &[sieve_query::ask::AskHit],
) -> Option<AskSaved> {
    let saved = savings_for(graph, &dedup_paths(hit_files(hits)))?;
    Some(AskSaved {
        files: saved.files,
        baseline_chars: saved.baseline_chars,
    })
}

/// Renders the text report, with the `ask --source` savings sentence
/// prepended when the rule in `ask-ranking.md` section 7.9 keeps it.
fn render_text(result: &sieve_query::ask::AskResult) -> String {
    let body = format_ask(result);
    match source_savings_line(result, &body) {
        Some(line) => format!("{line}\n\n{body}"),
        None => body,
    }
}

/// The `ask --source` savings sentence (section 7.9), or `None` when the
/// rule omits it: no `saved` (no `--source`, or no baseline), a zero
/// baseline, or a pack that is not smaller than the baseline.
fn source_savings_line(
    result: &sieve_query::ask::AskResult,
    rendered_body: &str,
) -> Option<String> {
    let saved = result.saved?;
    if saved.baseline_chars == 0 {
        return None;
    }
    let pack = to_tokens(utf16_len(&pack_region(rendered_body)));
    let base = to_tokens(saved.baseline_chars);
    if base <= pack {
        return None;
    }
    let delta = base - pack;
    Some(sieve_header(sieve_core::product().name, delta))
}

/// The rendered body with the escalation nudge and the final newline cut
/// away, for the `pack` measurement in section 7.9 (`ask_pack_region`,
/// shared with `sieve-daemon`'s `find_code`).
fn pack_region(rendered: &str) -> String {
    ask_pack_region(sieve_core::product().name, rendered)
}

/// Every repo-relative file path a set of hits points into
/// (`ask.ts`'s `hitFiles`). A symbol hit's path is `h.path`; a concept
/// hit's pointer is a comma-joined path list, or a bare slug when the
/// concept has no source (`concept.rs`'s `to_hit`). A slug part carries
/// no space, so it passes the same filter the `hitFiles` applies, and
/// `savings_for` then skips it: no graph node has that path.
fn hit_files(hits: &[sieve_query::ask::AskHit]) -> Vec<String> {
    let mut files = Vec::new();
    for hit in hits {
        if hit.span.is_some() {
            files.push(hit.path.clone());
            continue;
        }
        for part in hit.pointer.split(',') {
            let part = part.trim();
            if !part.is_empty() && !part.contains(' ') {
                files.push(part.to_string());
            }
        }
    }
    files
}

/// Keeps only the first occurrence of each path, in encounter order.
fn dedup_paths<I: IntoIterator<Item = String>>(paths: I) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for path in paths {
        if seen.insert(path.clone()) {
            out.push(path);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A concept hit's comma-joined pointer contributes every source file
    /// (`ask.ts`'s `hitFiles`). A sourceless concept's pointer is a bare
    /// slug, which carries no space and so still passes the filter — it
    /// then falls out of the savings baseline in `savings_for`, since no
    /// graph node names that path.
    #[test]
    fn hit_files_splits_a_concept_pointer_and_keeps_a_symbol_path() {
        use sieve_query::ask::AskHit;

        fn hit(kind: &str, pointer: &str, path: &str, span: Option<&str>) -> AskHit {
            AskHit {
                id: "id".to_string(),
                kind: kind.to_string(),
                title: "t".to_string(),
                pointer: pointer.to_string(),
                snippet: String::new(),
                related: Vec::new(),
                relation: None,
                score: 1.0,
                scope: None,
                code: None,
                scope_last: false,
                path: path.to_string(),
                span: span.map(|s| s.to_string()),
            }
        }

        let hits = vec![
            hit("symbol", "a.ts:L1-L3", "a.ts", Some("L1-L3")),
            hit("concept", "b.ts, c.ts", "", None),
            hit("concept", "sourceless-slug", "", None),
        ];

        assert_eq!(
            hit_files(&hits),
            vec![
                "a.ts".to_string(),
                "b.ts".to_string(),
                "c.ts".to_string(),
                "sourceless-slug".to_string(),
            ]
        );
    }

    #[test]
    fn pack_region_strips_the_escalation_nudge_but_keeps_the_header() {
        let rendered = "sieve ask — \"x\"  (lexical)\n\n1. a\n   b\n\n[sieve] only 1 hit — switch tool: `sieve grep \"x\"`\n";
        assert_eq!(
            pack_region(rendered),
            "sieve ask — \"x\"  (lexical)\n\n1. a\n   b"
        );
    }

    #[test]
    fn pack_region_keeps_the_body_when_there_is_no_nudge() {
        let rendered = "sieve ask — \"x\"  (lexical)\n\n1. a\n   b\n";
        assert_eq!(
            pack_region(rendered),
            "sieve ask — \"x\"  (lexical)\n\n1. a\n   b"
        );
    }

    /// Pins the pack measurement to the `formatAsk` rule:
    /// `body = lines.join("\n").trimEnd()`, header included, nudge and the
    /// final newline excluded. Built by hand from two hits — one with a
    /// three-line `code` block — so the expected string is the exact region
    /// the savings line measures.
    #[test]
    fn pack_region_matches_golden_body_length_with_a_code_hit() {
        use sieve_query::ask::{AskHit, AskMode, AskResult};

        let hits = vec![
            AskHit {
                id: "one".to_string(),
                kind: "symbol".to_string(),
                title: "one".to_string(),
                pointer: "a.ts:L1-L3".to_string(),
                snippet: String::new(),
                related: Vec::new(),
                relation: None,
                score: 1.0,
                scope: None,
                code: Some("line1\nline2\nline3".to_string()),
                scope_last: false,
                path: "a.ts".to_string(),
                span: Some("L1-L3".to_string()),
            },
            AskHit {
                id: "two".to_string(),
                kind: "symbol".to_string(),
                title: "two".to_string(),
                pointer: "b.ts:L1-L1".to_string(),
                snippet: String::new(),
                related: Vec::new(),
                relation: None,
                score: 1.0,
                scope: None,
                code: None,
                scope_last: false,
                path: "b.ts".to_string(),
                span: Some("L1-L1".to_string()),
            },
        ];
        let result = AskResult {
            query: "q".to_string(),
            mode: AskMode::Lexical,
            subject: None,
            hits,
            scopes: None,
            coverage: Some(0.0),
            coverage_strong: Some(0.0),
            ranking: None,
            note: None,
            saved: None,
            pagerank_for_test: None,
        };

        let rendered = sieve_query::ask::format_ask(&result);

        // Built by hand from `formatAsk`'s rules: the header, a blank line,
        // then each hit's `"N. title  [kind]"`, its pointer, its fenced
        // `code` block when present, and a blank line after each hit. No
        // scope footer (no `scopes`), no rules. `trimEnd()` then drops the
        // last hit's trailing blank line. This is the exact `body` the
        // `askSavingsLine` measures: header included, nudge excluded (4
        // hits or fewer triggers the nudge, but it is appended after the
        // measurement, so it never counts toward `pack`).
        let expected = "sieve ask — \"q\"  (lexical)\n\n\
1. one  [symbol]\n   a.ts:L1-L3\n\n```\nline1\nline2\nline3\n```\n\n\
2. two  [symbol]\n   b.ts:L1-L1";

        assert_eq!(pack_region(&rendered), expected);
    }
}
