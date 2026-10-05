//! The `sieve skeleton` subcommand: a signatures-only view of one file
//! (the `query-commands.md` note section 1).

use std::path::{Path, PathBuf};

use clap::Args;
use serde::Serialize;

use sieve_core::wiring::Graph;
use sieve_core::workspace;
use sieve_query::skeleton::{format_skeleton, skeleton, skeleton_saved_paths};
use sieve_savings::{savings_for, with_savings};

use crate::query;

/// Flags for `sieve skeleton`.
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
            "No index in {child} yet. Run `sieve build .` there first."
        ));
    }
    Err("This is a workspace parent. Run `sieve skeleton` inside one repo, or give a path under one.".to_string())
}

/// Runs `sieve skeleton`: the query prelude, then the lookup, then the
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

    // `format_skeleton` appends its own trailing newline, but
    // `with_savings(body, saved)` plus a newline measures the savings pack on
    // `body` with no trailing newline, then adds one newline after the
    // whole header-plus-body string (section 1.5). Stripping it here
    // before the pack measurement, then adding it back after, keeps the
    // printed bytes identical while fixing the token count at the
    // boundary where the extra `\n` would round differently.
    let rendered = format_skeleton(&result);
    let body = rendered.strip_suffix('\n').unwrap_or(&rendered);
    let saved_paths = skeleton_saved_paths(&result);
    let saved = savings_for(&graph, &saved_paths);
    println!(
        "{}",
        with_savings(sieve_core::product().name, body, saved.as_ref())
    );
    Ok(())
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
    use sieve_query::skeleton::SkeletonResult;

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
