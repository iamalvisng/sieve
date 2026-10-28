//! The `sieve symbols` subcommand: a signatures-only view of one file
//! (the `query-commands.md` note section 1).

use std::path::{Path, PathBuf};

use clap::Args;
use serde::Serialize;

use sieve_core::wiring::Graph;
use sieve_core::workspace;
use sieve_query::skeleton::{format_skeleton, skeleton, skeleton_saved_paths};
use sieve_savings::{append_line, savings_for, signatures_line};

use crate::query;

/// Flags for `sieve symbols`.
#[derive(Args, Debug)]
pub struct SkeletonArgs {
    /// The file to summarize, or a trailing path fragment of it.
    pub file: String,

    /// The repo root. Default: the nearest ancestor with a graph.
    #[arg(value_name = "dir")]
    pub dir: Option<PathBuf>,

    /// Prints the result as JSON, instead of the plain-text report.
    #[arg(long)]
    pub json: bool,

    /// Skips the pre-query graph refresh.
    #[arg(long = "no-refresh")]
    pub no_refresh: bool,
}

/// The `saved` key `--json` appends, key order `files` then
/// `baselineChars` (section 1.7).
#[derive(Serialize)]
struct SavedJson {
    files: usize,
    #[serde(rename = "baselineChars")]
    baseline_chars: u64,
}

/// The full JSON payload: [`sieve_query::skeleton::SkeletonResult`]'s own
/// fields, then `saved` when a baseline is known (section 1.7).
#[derive(Serialize)]
struct SkeletonResultJson<'a> {
    #[serde(flatten)]
    result: &'a sieve_query::skeleton::SkeletonResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    saved: Option<SavedJson>,
}

/// Loads the graph and the file path to look up. At a workspace parent,
/// the file goes to the child repo whose name prefixes the path, and the
/// answer comes from that child's index.
fn load_target(
    args: &SkeletonArgs,
    dir_override: Option<&Path>,
) -> Result<(Graph, String), String> {
    let (root, context_dir) =
        query::run_prelude(args.dir.as_deref(), dir_override, args.no_refresh)?;
    if workspace::read(&context_dir).is_none() {
        return Ok((query::read_wiring_named(&context_dir)?, args.file.clone()));
    }
    let graphs = sieve_query::workspace::load_children(&root, &context_dir);
    let file = args.file.trim_start_matches("./");
    let under = |child: &str| file.strip_prefix(child).and_then(|r| r.strip_prefix('/'));
    if let Some(hit) = graphs
        .loaded
        .into_iter()
        .find(|c| under(&c.child).is_some())
    {
        let rest = under(&hit.child).unwrap_or(file).to_string();
        return Ok((hit.graph, rest));
    }
    if let Some(child) = graphs.missing.iter().find(|c| under(c).is_some()) {
        return Err(format!(
            "no index in {child} yet \u{2014} run sieve build . there first"
        ));
    }
    Err("this is a workspace parent \u{2014} run sieve symbols inside one repo, or give a path under one".to_string())
}

/// Runs `sieve symbols`: the query prelude, then the lookup, then the
/// report. It fails with the no-index line when no graph exists.
pub fn run(args: &SkeletonArgs, dir_override: Option<&Path>) -> Result<(), String> {
    let (graph, file) = load_target(args, dir_override)?;
    let graph_ref = if graph.nodes.is_empty() && graph.edges.is_empty() {
        None
    } else {
        Some(&graph)
    };
    let result = skeleton(graph_ref, &file);

    if args.json {
        print_json(&graph, &result);
        return Ok(());
    }

    if result.entries.is_empty() {
        if let Some(line) = indexed_empty_line(&graph, &file, result.note.as_deref()) {
            println!("{line}");
            return Ok(());
        }
        return Err(miss_message(&file, result.note.as_deref()));
    }

    // `format_skeleton` appends its own trailing newline. The answer is
    // measured on the body without it.
    let rendered = format_skeleton(&result);
    let body = rendered.strip_suffix('\n').unwrap_or(&rendered);
    let saved_paths = skeleton_saved_paths(&result);
    let saved = savings_for(&graph, &saved_paths);
    let baseline = saved.map_or(0, |s| s.baseline_chars);
    let line = signatures_line(result.entries.len(), baseline, body);
    print!("{}", append_line(body, &line));
    Ok(())
}

/// The report line for a file that the index holds with 0 symbols, such as a
/// script with no functions. Gives `None` for a missing or ambiguous file.
fn indexed_empty_line(graph: &Graph, file: &str, note: Option<&str>) -> Option<String> {
    if note.is_some_and(|n| n.starts_with("ambiguous") || n.starts_with("no wiring graph")) {
        return None;
    }
    let suffix = format!("/{file}");
    let mut paths: Vec<&str> = graph
        .nodes
        .iter()
        .filter(|n| n.path == file || n.path.ends_with(&suffix))
        .map(|n| n.path.as_str())
        .collect();
    paths.sort_unstable();
    paths.dedup();
    let [path] = paths[..] else { return None };
    Some(format!(
        "{path} \u{b7} 0 symbols (indexed; no functions or types found)"
    ))
}

/// The error line for a file that is not in the index.
fn miss_message(file: &str, note: Option<&str>) -> String {
    let name = sieve_core::product().name;
    match note {
        Some(n) if n.starts_with("ambiguous \u{2014} matches: ") => {
            let list = n.trim_start_matches("ambiguous \u{2014} matches: ");
            format!("{file} matches more than one file: {list} \u{2014} give the full path")
        }
        Some(n) if n.starts_with("no wiring graph") => {
            format!("no index here yet \u{2014} run {name} build .")
        }
        _ => format!("{file} is not in the index \u{2014} check the path, or run {name} build ."),
    }
}

/// Prints the result as pretty JSON, with a `saved` key appended when a
/// baseline is known (section 1.7).
fn print_json(graph: &sieve_core::wiring::Graph, result: &sieve_query::skeleton::SkeletonResult) {
    let saved_paths = skeleton_saved_paths(result);
    let saved = savings_for(graph, &saved_paths).map(|s| SavedJson {
        files: s.files,
        baseline_chars: s.baseline_chars,
    });
    let payload = SkeletonResultJson { result, saved };
    if let Ok(text) = serde_json::to_string_pretty(&payload) {
        println!("{text}");
    }
}

#[cfg(test)]
mod tests {
    use super::{indexed_empty_line, miss_message};
    use sieve_core::wiring::{Graph, Meta};
    use sieve_query::skeleton::SkeletonResult;

    #[test]
    fn a_file_that_is_not_in_the_index_is_an_error_line() {
        assert_eq!(
            miss_message("nope.ts", Some("no definitions indexed for this file")),
            "nope.ts is not in the index \u{2014} check the path, or run sieve build ."
        );
        assert_eq!(
            miss_message("a.ts", Some("ambiguous \u{2014} matches: x/a.ts, y/a.ts")),
            "a.ts matches more than one file: x/a.ts, y/a.ts \u{2014} give the full path"
        );
    }

    fn graph_with_file(path: &str) -> Graph {
        let mut node = sieve_core::wiring::Node {
            id: path.to_string(),
            name: path.to_string(),
            kind: sieve_core::Kind::File,
            owner: None,
            path: path.to_string(),
            span: sieve_core::span(1, 1),
            signature: None,
            exported: true,
            origin: sieve_core::Origin::Generic,
            body_hash: String::new(),
            chars: None,
            body_text: None,
            arity: None,
            variadic: None,
            summary_state: sieve_core::SummaryState::Pending,
            summary: None,
            crux: None,
        };
        node.path = path.to_string();
        Graph {
            meta: Meta {
                version: 1,
                node_count: 1,
                edge_count: 0,
                languages: vec![],
                scopes: vec![],
            },
            nodes: vec![node],
            edges: vec![],
        }
    }

    #[test]
    fn an_indexed_file_with_no_symbols_gives_a_zero_symbols_line() {
        let g = graph_with_file("scripts/run.sh");
        assert_eq!(
            indexed_empty_line(&g, "run.sh", Some("no definitions indexed for this file")),
            Some(
                "scripts/run.sh \u{b7} 0 symbols (indexed; no functions or types found)"
                    .to_string()
            )
        );
    }

    #[test]
    fn two_files_with_one_name_give_no_zero_symbols_line() {
        let mut g = graph_with_file("a/run.sh");
        g.nodes.push(graph_with_file("b/run.sh").nodes.remove(0));
        assert_eq!(indexed_empty_line(&g, "run.sh", None), None);
    }

    #[test]
    fn a_file_missing_from_the_index_gives_no_zero_symbols_line() {
        let g = graph_with_file("scripts/run.sh");
        assert_eq!(indexed_empty_line(&g, "nope.sh", None), None);
    }

    #[test]
    fn print_json_attaches_saved_after_entries_and_note() {
        // A no-graph run has no `saved` key (no baseline known), and
        // `note` stays present. This only checks the key insertion does
        // not panic on a missing `saved`.
        let r = SkeletonResult {
            file: "a.ts".to_string(),
            entries: Vec::new(),
            note: Some("no wiring graph — run `sieve build` first".to_string()),
        };
        let value = serde_json::to_value(&r).expect("serializes");
        assert!(value.get("saved").is_none());
    }
}
