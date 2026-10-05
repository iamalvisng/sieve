//! The `sieve callers` subcommand: who calls, references, implements or
//! extends a symbol, or (with `--direction out`) what it calls
//! (the `query-commands.md` note section 2).

use std::path::{Path, PathBuf};

use clap::Args;
use serde::Serialize;

use sieve_core::workspace;
use sieve_query::callers::{
    edge_walk, format_callers, parse_depth, parse_direction, resolve_symbol, to_callers_json,
    CallersJson, Depth, Direction, FileReader, Hit,
};
use sieve_savings::{savings_for, with_savings};

use crate::query;

/// Flags for `sieve callers`.
#[derive(Args, Debug)]
pub struct CallersArgs {
    /// The symbol to resolve: a bare name, or `Type.method`.
    pub symbol: String,

    /// The repo root. Default: the nearest ancestor with a graph.
    #[arg(value_name = "dir")]
    pub dir: Option<PathBuf>,

    /// Walks `in` (who points at the symbol) or `out` (what it points at).
    #[arg(long, default_value = "in", allow_hyphen_values = true)]
    pub direction: String,

    /// How far the walk runs: a positive number, or `all`.
    #[arg(short = 'd', long, default_value = "1", allow_hyphen_values = true)]
    pub depth: String,

    /// Narrows the resolved symbols to this path prefix.
    #[arg(long = "in", value_name = "path", allow_hyphen_values = true)]
    pub in_prefix: Option<String>,

    /// Prints the result as JSON, instead of the plain-text report.
    #[arg(long)]
    pub json: bool,

    /// Skips the pre-query graph refresh.
    #[arg(long = "no-refresh")]
    pub no_refresh: bool,
}

/// The `saved` key `--json` appends, key order `files` then
/// `baselineChars` (section 2.9).
#[derive(Serialize)]
struct SavedJson {
    files: usize,
    #[serde(rename = "baselineChars")]
    baseline_chars: u64,
}

/// The full JSON payload: [`CallersJson`]'s own fields, then `saved` when a
/// baseline is known (section 2.9).
#[derive(Serialize)]
struct CallersResultJson<'a> {
    #[serde(flatten)]
    result: &'a CallersJson,
    #[serde(skip_serializing_if = "Option::is_none")]
    saved: Option<SavedJson>,
}

/// True when hit lines carry a `[depth N]` tag (section 2.3).
fn show_depth(depth: Depth) -> bool {
    match depth {
        Depth(Some(n)) => n > 1,
        Depth(None) => true,
    }
}

/// Runs `sieve callers` in the check order: loads the graph, resolves `--in`
/// and the symbol, validates `--direction` then `--depth`, walks the edges,
/// then renders the report.
pub fn run(args: &CallersArgs, dir_override: Option<&Path>) -> Result<(), String> {
    let (root, context_dir) =
        query::run_prelude(args.dir.as_deref(), dir_override, args.no_refresh)?;
    // Text `callers` at a workspace parent federates (P1-58);
    // `--json` falls through to the named no-graph line.
    if !args.json && workspace::read(&context_dir).is_some() {
        return run_workspace(args, &root, &context_dir);
    }
    let graph = query::read_wiring_named(&context_dir)?;

    let symbols = resolve_symbol(&graph, &args.symbol, args.in_prefix.as_deref())
        .map_err(|e| e.to_string())?;

    let direction = parse_direction(&args.direction).map_err(|e| e.to_string())?;
    let depth = parse_depth(&args.depth).map_err(|e| e.to_string())?;

    let matches: Vec<(&sieve_core::Node, Vec<Hit>)> = symbols
        .iter()
        .map(|symbol| (*symbol, edge_walk(&graph, symbol, direction, depth)))
        .collect();

    if args.json {
        print_json(&graph, &args.symbol, &matches, direction);
        return Ok(());
    }

    let mut reader = FileReader::new(&root);
    let body = format_callers(
        &args.symbol,
        &matches,
        direction,
        show_depth(depth),
        &mut reader,
    );
    let saved_paths = sieve_query::callers::callers_saved_paths(&matches);
    let saved = savings_for(&graph, &saved_paths);
    print!(
        "{}",
        with_savings(sieve_core::product().name, &body, saved.as_ref())
    );
    Ok(())
}

/// The workspace `callers` (P1-58): one block per child with a hit, each
/// with its own savings header, then the coverage note; a miss in every
/// child is the `✗` line and exit 1. Sieve reads `--direction` and
/// `--depth` leniently here: any other direction is `in`, and a bad depth
/// is 1.
fn run_workspace(args: &CallersArgs, root: &Path, context_dir: &Path) -> Result<(), String> {
    let graphs = sieve_query::workspace::load_children(root, context_dir);
    let direction = parse_direction(&args.direction).unwrap_or(Direction::In);
    let depth = parse_depth(&args.depth).unwrap_or(Depth(Some(1)));
    let fed = sieve_query::workspace::federate_callers(
        &graphs,
        &args.symbol,
        args.in_prefix.as_deref(),
        direction,
        depth,
        |graph, body, paths| {
            let saved = savings_for(graph, paths);
            with_savings(sieve_core::product().name, body, saved.as_ref())
        },
    )
    .map_err(|e| e.to_string())?;
    if !fed.found {
        return Err(fed.text);
    }
    println!("{}", fed.text);
    Ok(())
}

/// Prints the `--json` payload, with a `saved` key appended when a
/// baseline is known (section 2.9, no savings header in JSON).
fn print_json(
    graph: &sieve_core::Graph,
    query: &str,
    matches: &[(&sieve_core::Node, Vec<Hit>)],
    direction: Direction,
) {
    let result = to_callers_json(query, matches, direction);
    let saved_paths = sieve_query::callers::callers_saved_paths(matches);
    let saved = savings_for(graph, &saved_paths).map(|s| SavedJson {
        files: s.files,
        baseline_chars: s.baseline_chars,
    });
    let payload = CallersResultJson {
        result: &result,
        saved,
    };
    if let Ok(text) = serde_json::to_string_pretty(&payload) {
        println!("{text}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn show_depth_is_false_only_at_exactly_one() {
        assert!(!show_depth(Depth(Some(1))));
        assert!(show_depth(Depth(Some(2))));
        assert!(show_depth(Depth(None)));
    }
}
