//! The `sieve blast` subcommand: what a diff's changed lines reach
//! (the `query-commands.md` note section 3).
//!
//! `blast` never prepends a savings header (section 0). All four
//! `--format` names render: `text`, `json`, `markdown` and `mermaid`.

use std::path::{Path, PathBuf};

use clap::Args;

use sieve_query::blast::{
    blast, blast_viz_graph, format_blast_markdown, format_blast_text, mermaid_diagram,
    parse_format, repo_label, BlastFormat, BlastOptions, BlastReport,
};
use sieve_query::callers::parse_depth;
use sieve_query::viz::{export_page_with, ExportOptions};

use crate::build::resolve_abs;
use crate::query;
use crate::viz::VIEWER;

/// Flags for `sieve blast`.
#[derive(Args, Debug)]
pub struct BlastArgs {
    /// The repo root. Default: the nearest ancestor with a graph.
    #[arg(value_name = "dir")]
    pub dir: Option<PathBuf>,

    /// Diffs against this ref instead of the working tree vs `HEAD`.
    #[arg(long, allow_hyphen_values = true)]
    pub base: Option<String>,

    /// How far the walk runs: a positive number, or `all`.
    #[arg(short = 'd', long, default_value = "2", allow_hyphen_values = true)]
    pub depth: String,

    /// `text`, `markdown`, `mermaid`, or `json`.
    #[arg(long, default_value = "text", allow_hyphen_values = true)]
    pub format: String,

    /// Writes the radius as one self-contained index.html under this dir.
    #[arg(long = "export-viz", value_name = "dir", allow_hyphen_values = true)]
    pub export_viz: Option<PathBuf>,
    /// The subtitle beside the repo name in the exported page.
    #[arg(long, allow_hyphen_values = true)]
    pub title: Option<String>,
    /// Skips owner attribution.
    #[arg(long = "no-owners")]
    pub no_owners: bool,
    /// Names, emails or handles dropped from the reviewer list.
    #[arg(long = "pr-author", value_name = "who", num_args = 1..)]
    pub pr_author: Vec<String>,

    /// Skips the pre-query graph refresh.
    #[arg(long = "no-refresh")]
    pub no_refresh: bool,
}

/// Runs `sieve blast`: refreshes the graph, validates `--format` then
/// `--depth`, loads the graph, then runs the diff and the walk (section
/// 3.1).
///
/// The refresh runs before the flag checks: on a dirty tree the refresh note
/// comes before a flag error (P1-24, P1-27).
pub fn run(args: &BlastArgs, dir_override: Option<&Path>) -> Result<(), String> {
    let loaded = query::load_graph_named(args.dir.as_deref(), dir_override, args.no_refresh);
    let format = parse_format(&args.format).map_err(|e| e.to_string())?;
    let depth = parse_depth(&args.depth).map_err(|e| e.to_string())?;
    let (root, context_dir, graph) = loaded?;

    let opts = BlastOptions {
        base: args.base.clone(),
        depth,
        owners: !args.no_owners,
        pr_author: args.pr_author.clone(),
    };
    let report = blast(&graph, &root, &context_dir, &opts).map_err(|e| e.to_string())?;

    if let Some(out_dir) = args.export_viz.as_deref() {
        export_radius(&report, &root, &context_dir, out_dir, args.title.as_deref())?;
    }

    match format {
        BlastFormat::Json => {
            if let Ok(text) = serde_json::to_string_pretty(&report) {
                println!("{text}");
            }
        }
        BlastFormat::Text => print!("{}", format_blast_text(&report)),
        BlastFormat::Markdown => print!("{}", format_blast_markdown(&report, Some(&root))),
        // Exit 0 with a comment, not an error: "nothing depends on this
        // diff" is a legitimate answer.
        BlastFormat::Mermaid => println!(
            "{}",
            mermaid_diagram(&report).unwrap_or_else(|| "%% no dependents to draw".to_string())
        ),
    }
    Ok(())
}

/// Writes the interactive page for this radius: the same viewer `viz --export`
/// writes, with the blast graph as its only tab. The note goes to stderr,
/// before the report.
fn export_radius(
    report: &BlastReport,
    root: &Path,
    context_dir: &Path,
    out_dir: &Path,
    subtitle: Option<&str>,
) -> Result<(), String> {
    let tabs = ["context".to_string()];
    let repo_name = repo_label(root);
    let out = export_page_with(
        VIEWER,
        &ExportOptions {
            context_dir,
            repo_name: &repo_name,
            subtitle,
            tabs: Some(&tabs),
        },
        &blast_viz_graph(report, Some(root)),
    )
    .map_err(|e| e.to_string())?;
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let out_dir = resolve_abs(&cwd, out_dir);
    std::fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
    let file = out_dir.join("index.html");
    std::fs::write(&file, &out.page).map_err(|e| e.to_string())?;
    let kb = (out.page.len() as f64 / 1024.0).round();
    eprintln!(
        "• --export-viz: {} ({kb} kB, {} areas, {} code nodes)",
        file.display(),
        out.context_nodes,
        out.code_nodes
    );
    Ok(())
}
