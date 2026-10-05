//! The `sieve viz [dir]` subcommand (P1-65), step 1: `--export <dir>`
//! writes one self-contained `index.html`. Without `--export`
//! the live server (`-p`, `--no-open`) runs; see `viz_serve.rs`.
//!
//! The viewer files are original Sieve code (`assets/viewer/`). The build
//! embeds them at compile time.

use std::path::{Path, PathBuf};

use clap::Args;
use sieve_core::product::product;
use sieve_query::viz::{export_page, ExportOptions, Viewer, TABS};

use crate::build::resolve_abs;
use crate::query;
use crate::viz_serve;

/// The three viewer files, embedded as they are.
pub(crate) const VIEWER: Viewer<'static> = Viewer {
    html: include_str!("../assets/viewer/index.html"),
    css: include_str!("../assets/viewer/style.css"),
    js: include_str!("../assets/viewer/app.js"),
};

/// Flags for `sieve viz`.
#[derive(Args, Debug)]
pub struct VizArgs {
    /// The repo root. Default: the nearest ancestor with a graph.
    #[arg(value_name = "dir")]
    pub dir: Option<PathBuf>,

    /// The port to serve on. A busy port moves up, nine times at most.
    #[arg(
        short = 'p',
        long,
        value_name = "port",
        default_value = "4400",
        allow_hyphen_values = true
    )]
    pub port: String,

    /// Does not open the browser.
    #[arg(long = "no-open")]
    pub no_open: bool,

    /// Writes one self-contained index.html instead of serving.
    #[arg(long, value_name = "dir", allow_hyphen_values = true)]
    pub export: Option<PathBuf>,

    /// The subtitle shown beside the repo name in an exported page.
    #[arg(long, value_name = "text", allow_hyphen_values = true)]
    pub title: Option<String>,

    /// The tabs the exported page offers, comma separated. Default: all.
    #[arg(long, value_name = "list", allow_hyphen_values = true)]
    pub tabs: Option<String>,
}

/// `parseTabs`: splits on commas, trims, drops empty
/// items. An unknown name, or an empty list, is the `--tabs` error.
/// `None` means all three tabs.
pub fn parse_tabs(raw: Option<&str>) -> Result<Option<Vec<String>>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let want: Vec<String> = raw
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect();
    let bad: Vec<&str> = want
        .iter()
        .map(String::as_str)
        .filter(|t| !TABS.contains(t))
        .collect();
    if !bad.is_empty() || want.is_empty() {
        let got = if bad.is_empty() {
            String::new()
        } else {
            format!(" — got \"{}\"", bad.join("\", \""))
        };
        return Err(format!(
            "--tabs takes a comma-separated subset of {}{got}",
            TABS.join(", ")
        ));
    }
    Ok(Some(want))
}

/// The live server: binds, prints the line, opens the browser unless
/// `--no-open`, then serves until the process is stopped.
/// An unparsable port is an error line, not a stack trace.
fn serve_live(args: &VizArgs, context_dir: PathBuf, repo_name: String) -> Result<(), String> {
    let port: u16 = args
        .port
        .trim()
        .parse()
        .map_err(|_| format!("invalid port \"{}\"", args.port))?;
    let (listener, port) = viz_serve::bind(port)?;
    let url = format!("http://127.0.0.1:{port}");
    println!("{} viz → {url}  (ctrl-c to stop)", product().name);
    if !args.no_open {
        viz_serve::open_browser(&url);
    }
    let site = viz_serve::Site {
        viewer: VIEWER,
        context_dir,
        repo_name,
    };
    viz_serve::serve(&listener, &site);
    Ok(())
}

/// Runs `sieve viz`. The flags are checked before the repo is, and no
/// refresh runs: `viz` reads the context dir as it stands.
pub fn run(args: &VizArgs, dir_override: Option<&Path>) -> Result<(), String> {
    let tabs = parse_tabs(args.tabs.as_deref())?;
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let root = query::query_root(args.dir.as_deref(), dir_override, &cwd);
    let name = product().name;
    // `contextDirFor(root, override)`: the override verbatim, not resolved.
    let context_dir = match dir_override {
        Some(dir) => dir.to_path_buf(),
        None => root.join(product().context_dir_name()),
    };
    if !context_dir.exists() {
        return Err(format!(
            "no context graph at {} — run `{name} build` first",
            context_dir.display()
        ));
    }
    let repo_name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let Some(export) = args.export.as_deref() else {
        return serve_live(args, context_dir, repo_name);
    };
    let out = export_page(
        VIEWER,
        &ExportOptions {
            context_dir: &context_dir,
            repo_name: &repo_name,
            subtitle: args.title.as_deref(),
            tabs: tabs.as_deref(),
        },
    )
    .map_err(|e| e.to_string())?;
    let out_dir = resolve_abs(&cwd, export);
    std::fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
    let file = out_dir.join("index.html");
    std::fs::write(&file, &out.page).map_err(|e| e.to_string())?;
    let kb = (out.page.len() as f64 / 1024.0).round();
    println!(
        "{name} viz → {} ({kb} kB, {} concept nodes, {} code nodes)",
        file.display(),
        out.context_nodes,
        out.code_nodes
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_p1_65_parse_tabs_matches_golden() {
        assert_eq!(parse_tabs(None).expect("none"), None);
        assert_eq!(
            parse_tabs(Some(" code , context,,")).expect("ok"),
            Some(vec!["code".to_string(), "context".to_string()])
        );
        assert_eq!(
            parse_tabs(Some("bogus")).expect_err("bad"),
            "--tabs takes a comma-separated subset of context, code, outline — got \"bogus\""
        );
        assert_eq!(
            parse_tabs(Some("x,code,y")).expect_err("bad"),
            "--tabs takes a comma-separated subset of context, code, outline — got \"x\", \"y\""
        );
        assert_eq!(
            parse_tabs(Some(" , ")).expect_err("empty"),
            "--tabs takes a comma-separated subset of context, code, outline"
        );
    }
}
