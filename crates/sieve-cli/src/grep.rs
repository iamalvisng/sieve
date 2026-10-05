//! The `sieve grep` subcommand: a regex search over the indexed graph,
//! grouped by enclosing symbol.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use clap::Args;
use serde::Serialize;

use sieve_core::wiring::Graph;
use sieve_core::workspace;
use sieve_query::grep::{
    format_grep_result, grep_graph, zero_hit_note, GrepOptions, GrepResult, DEFAULT_MAX_HITS,
};
use sieve_savings::{savings_for, with_savings, Savings};

use crate::query;

/// Flags for `sieve grep`.
#[derive(Args, Debug)]
pub struct GrepArgs {
    /// The regex, or literal string with `--fixed`, to search for.
    pub pattern: String,

    /// The repo root. Default: the nearest ancestor with a graph.
    #[arg(value_name = "dir")]
    pub dir: Option<PathBuf>,

    /// Matches the pattern without regard to letter case.
    #[arg(short = 'i', long = "ignore-case")]
    pub ignore_case: bool,

    /// Treats the pattern as a literal string, not a regex.
    #[arg(long)]
    pub fixed: bool,

    /// Searches only indexed files under this path prefix.
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
/// `baselineChars`, matching the golden byte for byte.
#[derive(Serialize)]
struct SavedJson {
    files: usize,
    #[serde(rename = "baselineChars")]
    baseline_chars: u64,
}

/// The full JSON payload: [`GrepResult`]'s own fields, then `saved` when a
/// baseline is known.
#[derive(Serialize)]
struct GrepJson<'a> {
    #[serde(flatten)]
    result: &'a GrepResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    saved: Option<SavedJson>,
}

/// Runs `sieve grep`: the query prelude, then the search, then the report.
/// At a workspace parent, the search runs in each child and merges
/// (P1-57).
pub fn run(args: &GrepArgs, dir_override: Option<&Path>) -> Result<(), String> {
    let (root, context_dir) =
        query::run_prelude(args.dir.as_deref(), dir_override, args.no_refresh)?;

    let opts = GrepOptions {
        ignore_case: args.ignore_case,
        fixed: args.fixed,
        in_prefix: args.in_prefix.clone(),
        max_hits: DEFAULT_MAX_HITS,
    };
    if workspace::read(&context_dir).is_some() {
        return run_workspace(args, &root, &context_dir, &opts);
    }
    let graph = query::read_wiring_named(&context_dir)?;
    let result = grep_graph(&graph, &root, &args.pattern, &opts).map_err(|err| err.to_string())?;

    if args.json {
        print_json(&graph, &result)?;
        return Ok(());
    }

    if result.total_hits == 0 {
        eprintln!("{}", zero_hit_note(&result));
        return Ok(());
    }

    let paths = dedup_paths(result.groups.iter().map(|g| g.path.clone()));
    let saved = savings_for(&graph, &paths);
    let body = format_grep_result(&result);
    print!(
        "{}",
        with_savings(sieve_core::product().name, &body, saved.as_ref())
    );
    Ok(())
}

/// Prints `result` as pretty JSON, with a `saved` key appended when the
/// group paths' file sizes are known.
fn print_json(graph: &Graph, result: &GrepResult) -> Result<(), String> {
    let paths = dedup_paths(result.groups.iter().map(|g| g.path.clone()));
    let saved = savings_for(graph, &paths);
    print_json_with(result, saved.as_ref())
}

/// Prints `result` as pretty JSON, with `saved` appended when given.
fn print_json_with(result: &GrepResult, saved: Option<&Savings>) -> Result<(), String> {
    let saved = saved.map(|s| SavedJson {
        files: s.files,
        baseline_chars: s.baseline_chars,
    });
    let payload = GrepJson { result, saved };
    let text = serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?;
    println!("{text}");
    Ok(())
}

/// The workspace `grep` (P1-57, P1-60): one search per built child,
/// merged, with the coverage note after the body, or after the zero-hit
/// note on stderr. `--json` prints the merged result alone, and `--in`
/// is ignored.
fn run_workspace(
    args: &GrepArgs,
    root: &Path,
    context_dir: &Path,
    opts: &GrepOptions,
) -> Result<(), String> {
    let graphs = sieve_query::workspace::load_children(root, context_dir);
    let coverage = sieve_query::workspace::coverage_note(&graphs);
    let fed = sieve_query::workspace::federate_grep(&graphs, &args.pattern, opts, |g, paths| {
        savings_for(g, paths).map(|s| (s.files, s.baseline_chars))
    })
    .map_err(|err| err.to_string())?;
    // Sieve keeps a summed baseline only when it is above zero.
    let saved = (fed.saved_chars > 0).then_some(Savings {
        baseline_chars: fed.saved_chars,
        files: fed.saved_files,
    });

    if args.json {
        return print_json_with(&fed.result, saved.as_ref());
    }
    if fed.result.total_hits == 0 {
        let note = zero_hit_note(&fed.result);
        if coverage.is_empty() {
            eprintln!("{note}");
        } else {
            eprintln!("{note}\n{coverage}");
        }
        return Ok(());
    }
    let body = format_grep_result(&fed.result);
    print!(
        "{}",
        with_savings(sieve_core::product().name, &body, saved.as_ref())
    );
    if !coverage.is_empty() {
        println!("{coverage}");
    }
    Ok(())
}

/// Keeps only the first occurrence of each path, in encounter order.
fn dedup_paths<I: IntoIterator<Item = String>>(paths: I) -> Vec<String> {
    let mut seen = HashSet::new();
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

    #[test]
    fn dedup_paths_keeps_the_first_occurrence_order() {
        let paths = vec!["b.rs".to_string(), "a.rs".to_string(), "b.rs".to_string()];
        assert_eq!(
            dedup_paths(paths),
            vec!["b.rs".to_string(), "a.rs".to_string()]
        );
    }
}
