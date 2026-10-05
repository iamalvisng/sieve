//! The `sieve check` subcommand: is the committed wiring graph (and the
//! deep-layer manifest) still in sync with the code?
//! (the `query-commands.md` note section 5)

use std::path::{Path, PathBuf};

use clap::Args;
use serde::Serialize;

use sieve_core::lang::native_extensions;
use sieve_core::workspace;
use sieve_parse::check::{
    check_context, check_graph, format_check_report, format_graph_check_report, ContextCheck,
    GraphCheck,
};
use sieve_parse::{CONTAINER_LANGS, GENERIC_LANGS};

use crate::build::resolve_abs;
use crate::query;
use crate::ui::Ui;

/// Every extension a build parses, sorted and de-duplicated: the native
/// tier, the breadth tier and the container tier.
fn supported_extensions() -> Vec<&'static str> {
    let mut all = native_extensions();
    all.extend(GENERIC_LANGS.iter().flat_map(|l| l.exts.iter().copied()));
    all.extend(CONTAINER_LANGS.iter().flat_map(|l| l.exts.iter().copied()));
    all.sort_unstable();
    all.dedup();
    all
}

/// Flags for `sieve check`.
#[derive(Args, Debug)]
pub struct CheckArgs {
    /// The repo root. Default: the nearest ancestor with a graph.
    #[arg(value_name = "dir")]
    pub dir: Option<PathBuf>,

    /// Accepted, unused: checked for an unknown extension, then ignored —
    /// `check` re-extracts with every extension `sieve` knows.
    #[arg(short = 'e', long = "extensions", value_name = "exts", num_args = 1..)]
    pub extensions: Vec<String>,

    /// Prints the result as JSON, instead of the plain-text report.
    #[arg(long)]
    pub json: bool,
}

/// The `--json` payload's key order: `context`, then `graph` (`null` when
/// missing is impossible for `context`, so only `graph` ever goes
/// `null`) (section 5.5).
#[derive(Serialize)]
struct CheckJson {
    context: ContextCheck,
    graph: Option<GraphCheck>,
}

/// Runs `sieve check`: warns on an unknown `-e` extension, then runs both
/// layers, with no refresh (section 5.1 to 5.5).
pub fn run(args: &CheckArgs, dir_override: Option<&Path>) -> Result<(), String> {
    warn_unknown_extensions(&args.extensions);

    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let root = match args.dir.as_deref() {
        Some(dir) => resolve_abs(&cwd, dir),
        None => query::query_root(None, dir_override, &cwd),
    };
    let context_dir = query::context_dir(&root, dir_override, &cwd);
    if workspace::read(&context_dir).is_some() {
        return run_workspace(&root, &context_dir);
    }

    let started = std::time::Instant::now();
    let context = check_context(&root, &context_dir);
    let graph = check_graph(&root, &context_dir).map_err(|e| e.to_string())?;

    let both_missing = context.missing && graph.missing;
    let exit_failure =
        both_missing || (!context.missing && !context.ok) || (!graph.missing && !graph.ok);

    if args.json {
        let payload = CheckJson {
            context,
            graph: if graph.missing { None } else { Some(graph) },
        };
        let text = serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?;
        println!("{text}");
    } else if both_missing {
        return Err(query::NO_INDEX.to_string());
    } else {
        let ui = Ui::stdout();
        if !context.missing {
            println!("{}", format_check_report(&context));
        }
        if !graph.missing {
            println!(
                "{}",
                graph_summary(&ui, &graph, &crate::build::elapsed_text(started.elapsed()))
            );
        }
    }

    if exit_failure {
        // `check` has already printed its own report; there is no extra
        // error line to add, so this exits directly instead of returning an
        // error `main` would print.
        std::process::exit(1);
    }
    Ok(())
}

/// The check result as the reader sees it: a green tick and the elapsed time
/// when the index is in sync, a red cross and the changed ids when it is
/// behind.
fn graph_summary(ui: &Ui, g: &GraphCheck, elapsed: &str) -> String {
    let report = format_graph_check_report(g);
    let (first, rest) = report.split_once('\n').unwrap_or((report.as_str(), ""));
    if g.ok {
        return format!(
            "{} {} {}",
            ui.green("\u{2713}"),
            ui.fg(first),
            ui.dim(&format!("\u{b7} checked in {elapsed}"))
        );
    }
    let mut out = format!("{} {}", ui.red("\u{2717}"), ui.fg(first));
    if !rest.is_empty() {
        out.push('\n');
        out.push_str(&ui.dim(rest));
    }
    out
}

/// The workspace `check` (P1-59, P1-60): one line per child, `OK`, `STALE (…)`
/// or `not built`, then the coverage note. `--json` is ignored here. Exits 1
/// when a built child is stale; an unbuilt child is coverage, not failure.
fn run_workspace(root: &Path, context_dir: &Path) -> Result<(), String> {
    let graphs = sieve_query::workspace::load_children(root, context_dir);
    let loaded: Vec<(&str, &Path)> = graphs
        .loaded
        .iter()
        .map(|c| (c.child.as_str(), c.root.as_path()))
        .collect();
    let coverage = sieve_query::workspace::coverage_note(&graphs);
    let (text, ok) = sieve_parse::federated_check_text(&loaded, &graphs.missing, &coverage)
        .map_err(|e| e.to_string())?;
    print!("{text}");
    if !ok {
        std::process::exit(1);
    }
    Ok(())
}

/// Warns on stderr for each `-e` value whose extension no parser claims
/// (section 5.1). The value keeps its leading `.` only when the caller
/// already wrote one; the warning text always shows one, so this adds it
/// when missing. `build` calls this too.
pub(crate) fn warn_unknown_extensions(extensions: &[String]) {
    for line in unknown_extension_lines(extensions) {
        eprintln!("{line}");
    }
}

/// The stderr lines for the unknown `-e` values
/// `warnUnsupportedExtensions`): one `⚠` line per value outside the
/// supported set (compared lower-cased, `normExt`), then one `supported:`
/// line, space-joined, only when at least one value was unknown.
fn unknown_extension_lines(extensions: &[String]) -> Vec<String> {
    let supported = supported_extensions();
    let mut lines = Vec::new();
    for raw in extensions {
        let trimmed = raw.trim();
        let ext = if trimmed.starts_with('.') {
            trimmed.to_string()
        } else {
            format!(".{trimmed}")
        };
        if !supported.contains(&ext.to_lowercase().as_str()) {
            lines.push(format!(
                "⚠ -e \"{ext}\": no parser registered for this extension — ignoring it."
            ));
        }
    }
    if !lines.is_empty() {
        lines.push(format!("  supported: {}", supported.join(" ")));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(ok: bool) -> GraphCheck {
        GraphCheck {
            ok,
            missing: false,
            added: Vec::new(),
            removed: Vec::new(),
            changed: if ok {
                Vec::new()
            } else {
                vec!["a.ts#f".to_string()]
            },
            stale: Vec::new(),
            pending: 0,
            pending_ids: Vec::new(),
            nodes: 3,
            files: 2,
        }
    }

    #[test]
    fn the_check_result_is_one_line_in_sync_and_names_the_ids_when_behind() {
        let ui = Ui::plain();
        assert_eq!(
            graph_summary(&ui, &graph(true), "0.2 s"),
            "\u{2713} sieve is in sync with the code \u{b7} 2 files \u{b7} checked in 0.2 s"
        );
        assert_eq!(
            graph_summary(&ui, &graph(false), "0.2 s"),
            "\u{2717} sieve is behind on 1 file \u{2014} run sieve build\n  ~ a.ts#f"
        );
    }

    #[test]
    fn test_p4_42_unknown_extensions_warn_each_then_list_supported_once() {
        let lines = unknown_extension_lines(&[".foo".to_string(), "bar".to_string()]);
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert_eq!(
            lines[0],
            "⚠ -e \".foo\": no parser registered for this extension — ignoring it."
        );
        assert_eq!(
            lines[1],
            "⚠ -e \".bar\": no parser registered for this extension — ignoring it."
        );
        assert_eq!(
            lines[2],
            "  supported: .astro .bb .c .cc .cjs .clj .cljc .cljs .cpp .cs .cts .cxx .dart .ex \
             .exs .go .h .hh .hpp .java .js .jsx .kt .kts .lua .mjs .ml .mli .mts .nix .php .py \
             .pyi .r .rb .rs .sc .scala .sol .svelte .swift .ts .tsx .vue .zig"
        );
        assert!(unknown_extension_lines(&[
            ".ts".to_string(),
            "rs".to_string(),
            ".VUE".to_string()
        ])
        .is_empty());
    }
}
