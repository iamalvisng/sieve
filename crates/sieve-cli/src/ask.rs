//! The `sieve query` subcommand: a ranked-hit query over the indexed graph
//!.

use std::path::{Path, PathBuf};

use clap::Args;

use sieve_core::askindex::{ask_index_path, read_ask_index};
use sieve_core::workspace;
use sieve_query::ask::{
    ask, define, format_ask, format_ask_json, format_def, single_ident, AskOptions, AskSaved, Ident,
};
use sieve_query::workspace::{federate_ask, FederateAskOptions};
use sieve_savings::{append_line, def_line, ranked_line, savings_for};

use crate::query;

/// Flags for `sieve query`.
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

    /// With one identifier: prints the defining body and the callers count.
    #[arg(long)]
    pub def: bool,

    /// Skips the personalized PageRank pass.
    #[arg(long = "no-graph-rank")]
    pub no_graph_rank: bool,

    /// Skips the pre-query graph refresh.
    #[arg(long = "no-refresh")]
    pub no_refresh: bool,
}

/// Runs `sieve query`: the query prelude, the sidecar load, the scoring
/// engine, then the report.
pub fn run(args: &AskArgs, dir_override: Option<&Path>) -> Result<(), String> {
    let (root, context_dir) =
        query::run_prelude(args.dir.as_deref(), dir_override, args.no_refresh)?;
    if workspace::read(&context_dir).is_some() {
        return run_workspace(args, &root, &context_dir);
    }
    let graph = query::read_wiring_named(&context_dir)?;
    let opts = AskOptions {
        limit: args.limit,
        in_prefix: args.in_prefix.clone(),
        graph_rank: !args.no_graph_rank,
        source: args.source,
        full: args.full,
    };
    // ponytail: `--def` at a workspace parent prints the federated bytes
    // (above). A lean workspace answer needs a federated `define`.
    if args.def && !args.full && !args.json {
        if let Some(ident) = single_ident(&args.query) {
            // A bad `--in` prefix fails as without `--def`: `ask` runs the
            // same prefix check first. ponytail: this scores the query once
            // more; a shared prefix check would avoid that.
            if args.in_prefix.is_some() {
                ask(&graph, None, &args.query, &opts, &root).map_err(|err| err.to_string())?;
            }
            print!(
                "{}",
                render_def(&graph, &root, &ident, args.in_prefix.as_deref())
            );
            return Ok(());
        }
    }
    let index = read_ask_index(&ask_index_path(&context_dir));

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

/// The lean `--def` answer: the definition, then the answer line. The
/// baseline is the top definition's whole file.
fn render_def(
    graph: &sieve_core::wiring::Graph,
    root: &Path,
    ident: &Ident,
    in_prefix: Option<&str>,
) -> String {
    let def = define(graph, ident, in_prefix);
    let body = format_def(graph, root, ident, &def, in_prefix);
    let Some(top) = def.nodes.first() else {
        return body;
    };
    let baseline =
        savings_for(graph, std::slice::from_ref(&top.path)).map_or(0, |s| s.baseline_chars);
    append_line(&body, &def_line(&top.name, baseline, &body))
}

/// The workspace `ask`: each child answers, and the merged result
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

/// Renders the text report, with the ranked answer line as the last line
/// when the result holds a hit.
fn render_text(result: &sieve_query::ask::AskResult) -> String {
    let body = format_ask(result);
    if result.hits.is_empty() {
        body
    } else {
        append_line(&body, &ranked_line(result.hits.len()))
    }
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
}
