//! The `sieve map` subcommand: a deterministic, token-budgeted repo orientation
//! (the `query-commands.md` note section 4).

use std::path::{Path, PathBuf};

use clap::Args;
use serde::Serialize;

use sieve_core::workspace;
use sieve_query::map::{build_repo_map, format_repo_map, map_saved_paths, MapOptions};
use sieve_savings::{savings_for, with_savings};

use crate::query;

/// Flags for `sieve map`.
#[derive(Args, Debug)]
pub struct MapArgs {
    /// The repo root. Default: the nearest ancestor with a graph.
    #[arg(value_name = "dir")]
    pub dir: Option<PathBuf>,

    /// The most directories to report.
    #[arg(long = "max-dirs", value_name = "n", allow_hyphen_values = true)]
    pub max_dirs: Option<String>,

    /// Prints the result as JSON, instead of the plain-text report.
    #[arg(long)]
    pub json: bool,

    /// Skips the pre-query graph refresh.
    #[arg(long = "no-refresh")]
    pub no_refresh: bool,
}

/// The `saved` key `--json` appends, key order `files` then
/// `baselineChars` (section 4.7).
#[derive(Serialize)]
struct SavedJson {
    files: usize,
    #[serde(rename = "baselineChars")]
    baseline_chars: u64,
}

/// The full JSON payload: [`sieve_query::map::RepoMap`]'s own fields, then
/// `saved` when a baseline is known (section 4.7).
#[derive(Serialize)]
struct RepoMapJson<'a> {
    #[serde(flatten)]
    map: &'a sieve_query::map::RepoMap,
    #[serde(skip_serializing_if = "Option::is_none")]
    saved: Option<SavedJson>,
}

/// Parses a raw `--max-dirs` value with JavaScript `parseInt(raw, 10)`
/// semantics: leading whitespace, an optional sign, then the longest
/// leading run of decimal digits; no digits at all gives `NaN` (section
/// 4.1). Rejects non-finite or non-positive values.
fn parse_max_dirs(raw: &str) -> Result<usize, String> {
    let trimmed = raw.trim_start();
    let mut chars = trimmed.chars().peekable();
    let mut digits = String::new();
    if let Some(&sign) = chars.peek() {
        if sign == '+' || sign == '-' {
            digits.push(sign);
            chars.next();
        }
    }
    let mut saw_digit = false;
    for c in chars {
        if c.is_ascii_digit() {
            digits.push(c);
            saw_digit = true;
        } else {
            break;
        }
    }
    let value: Option<i64> = if saw_digit { digits.parse().ok() } else { None };
    match value {
        Some(n) if n > 0 => Ok(n as usize),
        _ => Err(format!(
            "--max-dirs must be a positive integer, got \"{raw}\""
        )),
    }
}

/// Runs `sieve map`: validates `--max-dirs` before the refresh runs, then
/// the query prelude, the grouping, then the report (section 4.1). Text
/// `map` at a workspace parent federates (P1-54);
/// `--json` falls through to the single path and its no-graph line.
pub fn run(args: &MapArgs, dir_override: Option<&Path>) -> Result<(), String> {
    let max_dirs = args.max_dirs.as_deref().map(parse_max_dirs).transpose()?;

    let (root, context_dir) =
        query::run_prelude(args.dir.as_deref(), dir_override, args.no_refresh)?;
    if !args.json && workspace::read(&context_dir).is_some() {
        return run_workspace(&root, &context_dir, max_dirs);
    }
    let graph = query::read_wiring_named(&context_dir)?;

    let opts = MapOptions {
        max_dirs: max_dirs.unwrap_or(MapOptions::default().max_dirs),
        ..MapOptions::default()
    };
    let map = build_repo_map(&graph, &opts);

    if args.json {
        print_json(&graph, &map);
        return Ok(());
    }

    // `format_repo_map` appends its own trailing newline, but
    // `with_savings(body, saved)` plus a newline measures the savings pack on
    // `body` with no trailing newline, then adds one newline after the
    // whole header-plus-body string (section 4.6). Stripping it here
    // before the pack measurement, then adding it back after, keeps the
    // printed bytes identical while fixing the token count at the
    // boundary where the extra `\n` would round differently.
    let rendered = format_repo_map(&map);
    let body = rendered.strip_suffix('\n').unwrap_or(&rendered);
    let saved_paths = map_saved_paths(&graph);
    let saved = savings_for(&graph, &saved_paths);
    println!(
        "{}",
        with_savings(sieve_core::product().name, body, saved.as_ref())
    );
    Ok(())
}

/// The workspace `map` (P1-54): one section per built child, each with
/// its own savings header, then the coverage note.
fn run_workspace(root: &Path, context_dir: &Path, max_dirs: Option<usize>) -> Result<(), String> {
    let graphs = sieve_query::workspace::load_children(root, context_dir);
    let text = sieve_query::workspace::federate_map(&graphs, max_dirs, |graph, body| {
        let saved = savings_for(graph, &map_saved_paths(graph));
        with_savings(sieve_core::product().name, body, saved.as_ref())
    });
    print!("{text}");
    Ok(())
}

/// Prints the result as pretty JSON, with a `saved` key appended when a
/// baseline is known (section 4.7).
fn print_json(graph: &sieve_core::Graph, map: &sieve_query::map::RepoMap) {
    let saved_paths = map_saved_paths(graph);
    let saved = savings_for(graph, &saved_paths).map(|s| SavedJson {
        files: s.files,
        baseline_chars: s.baseline_chars,
    });
    let payload = RepoMapJson { map, saved };
    if let Ok(text) = serde_json::to_string_pretty(&payload) {
        println!("{text}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_max_dirs_accepts_leading_digits_and_ignores_trailing_garbage() {
        assert_eq!(parse_max_dirs("2x").unwrap(), 2);
        assert_eq!(parse_max_dirs(" 16 ").unwrap(), 16);
    }

    #[test]
    fn parse_max_dirs_rejects_zero_and_non_numeric() {
        assert!(parse_max_dirs("0").is_err());
        assert!(parse_max_dirs("x").is_err());
        assert!(parse_max_dirs("-1").is_err());
    }
}
