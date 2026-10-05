//! `sieve uninstall [dir]`: removes every file `sieve init` wrote, never
//! touching a foreign key it merged alongside its own (the `hosts-hooks.md`
//! note section 1.12). The report: lines grouped by host, "would remove" in the
//! dry run (P1-72).
//!
//! `sieve uninstall -y` leaves `.cursor/hooks.json` behind. The golden
//! fixture pins this: Sieve never writes a `.cjs` shim file (the ledger's
//! "no `.cjs` shim" rule), and never removes the hooks file either.

use std::path::{Path, PathBuf};

use clap::Args;
use sieve_core::product::product;

use crate::build::resolve_abs;
use crate::hosts::{self, json_mcp_targets, Kind, Retract};
use crate::init::prune_empty_dirs;
use crate::ui::Ui;

/// Flags for `sieve uninstall`.
#[derive(Args, Debug)]
pub struct UninstallArgs {
    /// The repo root. Default: the current dir.
    #[arg(value_name = "dir", default_value = ".")]
    pub dir: PathBuf,

    /// Removes without a confirmation prompt.
    #[arg(short = 'y', long = "yes")]
    pub yes: bool,

    /// Removes files under `HOME` too. Without this flag, `uninstall`
    /// touches only the repo (ledger "sieve init writes under HOME only
    /// with --global", 2026-09-16).
    #[arg(long = "global", conflicts_with = "no_global")]
    pub global: bool,

    /// Leaves out-of-repo files alone. Sieve already leaves them without
    /// `--global`; this flag only drops the `--global` hint line.
    // P1-72: the doc comments here reach the derived `--help`
    // (`help_cli.rs` pins the text).
    #[arg(long = "no-global")]
    pub no_global: bool,

    /// Keeps the context dir and the `.gitignore` entry; removes the
    /// wiring only.
    #[arg(long = "keep-cache")]
    pub keep_cache: bool,
}

/// How one target runs: `apply` false only reports what would happen.
type Run = Box<dyn Fn(bool) -> std::io::Result<Retract>>;

/// One thing a retraction can remove.
struct Target {
    host: &'static str,
    path: PathBuf,
    what: String,
    global: bool,
    run: Run,
}

/// A target and what its run did, or would do.
struct Outcome {
    host: &'static str,
    path: PathBuf,
    what: String,
    global: bool,
    action: Retract,
}

/// Runs `sieve uninstall`. Prints nothing to stdout; every report line
/// goes to stderr (matching the golden's empty `uninstall.stdout.txt`).
pub fn run(args: &UninstallArgs, _context_dir_override: Option<&Path>) -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let root = resolve_abs(&cwd, &args.dir);
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is not set".to_string())?;
    let targets = targets(&root, &home, args.global, !args.keep_cache);

    let ui = Ui::stderr();
    if !args.yes {
        let plan = retract(targets, &root, &home, false)?;
        eprintln!("{}", format_retractions(&ui, &plan, false));
        eprintln!(
            "\n{}",
            ui.dim(&format!(
                "dry run \u{b7} nothing removed \u{b7} run {} uninstall -y to remove",
                product().name
            ))
        );
        if args.global {
            eprintln!(
                "{}",
                ui.dim("entries marked [machine-wide] affect every project \u{b7} --no-global skips them")
            );
        } else if !args.no_global {
            eprintln!(
                "{}",
                ui.dim("global hooks under ~ stay \u{b7} pass --global to remove them")
            );
        }
        return Ok(());
    }

    let done = retract(targets, &root, &home, true)?;
    eprintln!("{}", format_retractions(&ui, &done, true));
    let bad = done
        .iter()
        .filter(|o| o.action == Retract::Unparseable)
        .count();
    if bad > 0 {
        let files = if bad == 1 { "file" } else { "files" };
        eprintln!(
            "\n{} {bad} {files} could not be parsed and left as they are \u{2014} see above",
            ui.orange("\u{26a0}")
        );
    } else {
        eprintln!(
            "\n{} {}",
            ui.green("\u{2713}"),
            ui.fg(&format!(
                "{} removed \u{b7} run {} init to set it up again",
                product().name,
                product().name
            ))
        );
    }
    if !args.global && !args.no_global {
        eprintln!(
            "{}",
            ui.dim("skipped global hooks under ~ \u{b7} pass --global to remove them")
        );
    }
    Ok(())
}

/// Runs every target, in order. With `apply` false nothing changes on
/// disk. An error on a repo file stops the run. An error on a `HOME` file
/// prints a warning and the run goes on: a global strip is a best-effort
/// extra. Absent targets are dropped.
fn retract(
    targets: Vec<Target>,
    root: &Path,
    home: &Path,
    apply: bool,
) -> Result<Vec<Outcome>, String> {
    let mut out = Vec::new();
    for t in targets {
        let action = match (t.run)(apply) {
            Ok(action) => action,
            Err(e) if t.global => {
                eprintln!("\u{26a0} could not remove {}: {e}", t.path.display());
                continue;
            }
            Err(e) => return Err(e.to_string()),
        };
        if apply && action.hit() && !t.path.exists() {
            prune_empty_dirs(t.path.parent(), if t.global { home } else { root });
        }
        if action != Retract::Absent {
            out.push(Outcome {
                host: t.host,
                path: t.path,
                what: t.what,
                global: t.global,
                action,
            });
        }
    }
    Ok(out)
}

/// The report: lines grouped by host, in order of first appearance.
fn format_retractions(ui: &Ui, outcomes: &[Outcome], apply: bool) -> String {
    if outcomes.is_empty() {
        return "nothing to remove \u{2014} no sieve files found here".to_string();
    }
    let verb = if apply { "removed" } else { "would remove" };
    let mut hosts_seen: Vec<&str> = Vec::new();
    for o in outcomes {
        if !hosts_seen.contains(&o.host) {
            hosts_seen.push(o.host);
        }
    }
    let mut lines: Vec<String> = Vec::new();
    for host in hosts_seen {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push(ui.purple(&format!("{host}:")));
        for o in outcomes.iter().filter(|o| o.host == host) {
            let (mark, note) = match o.action {
                Retract::Unparseable => (
                    "\u{26a0}",
                    " \u{2014} not valid JSON, left untouched (remove the sieve entry by hand)"
                        .to_string(),
                ),
                Retract::Deleted => ("-", format!(" ({} \u{2014} deleted)", o.what)),
                _ => ("~", format!(" ({})", o.what)),
            };
            let scope = if o.global { " [machine-wide]" } else { "" };
            lines.push(format!(
                "  {mark} {}: {}{scope}{}",
                ui.orange(verb),
                ui.cyan(&o.path.display().to_string()),
                ui.dim(&note)
            ));
        }
    }
    lines.join("\n")
}

/// Every target `sieve init` could have written, in the removal order.
/// A file under `HOME` needs `global`. The
/// context dir and the `.gitignore` entry need `cache`. A path listed
/// twice (the shared `AGENTS.md`) runs once.
fn targets(root: &Path, home: &Path, global: bool, cache: bool) -> Vec<Target> {
    let mut out: Vec<Target> = Vec::new();
    let mut add = |host: &'static str, path: PathBuf, what: &str, is_global: bool, run: Run| {
        if out.iter().any(|t| t.path == path) {
            return;
        }
        out.push(Target {
            host,
            path,
            what: what.to_string(),
            global: is_global,
            run,
        });
    };
    let with = |path: &Path, f: fn(&Path, bool) -> std::io::Result<Retract>| -> Run {
        let path = path.to_path_buf();
        Box::new(move |apply| f(&path, apply))
    };
    let mcp_json = |path: &Path, key: &'static str| -> Run {
        let path = path.to_path_buf();
        Box::new(move |apply| hosts::strip_mcp_json(&path, key, apply))
    };

    for host in hosts::hosts() {
        let path = root.join(&host.rel_path);
        match host.kind {
            Kind::Owned => add(
                host.id,
                path.clone(),
                "sieve-owned instruction file",
                false,
                with(&path, hosts::remove_owned),
            ),
            Kind::Section => add(
                host.id,
                path.clone(),
                "fenced sieve section",
                false,
                with(&path, hosts::strip_section),
            ),
        }
    }

    // Codex and opencode belong to the `agents` host; each needs the
    // host's own config dir (P4-51).
    if global {
        if let Some(path) = hosts::codex_mcp_path(home) {
            let run = with(&path, hosts::strip_mcp_toml);
            add("agents", path, "[mcp_servers.sieve]", true, run);
        }
    }
    if let Some(path) = hosts::opencode_mcp_path(root, home) {
        let run = mcp_json(&path, "mcp");
        add("agents", path, "mcp.sieve", false, run);
    }
    let json_targets = json_mcp_targets();
    let json_target = |id: &str| json_targets.iter().find(|t| t.id == id);
    for id in ["cursor", "gemini", "grok", "antigravity", "kiro"] {
        if id == "grok" {
            let path = root.join(".grok").join("config.toml");
            let run = with(&path, hosts::strip_mcp_toml);
            add("grok", path, "[mcp_servers.sieve]", false, run);
        } else if let Some(t) = json_target(id).filter(|t| global || !t.under_home) {
            let path = if t.under_home { home } else { root }.join(t.rel_path);
            let run = mcp_json(&path, "mcpServers");
            add(t.id, path, "mcpServers.sieve", t.under_home, run);
        }
    }

    let claude = root.join(".claude");
    let settings = claude.join("settings.json");
    add(
        "claude",
        settings.clone(),
        "statusline + hooks + allowlist + footer regex",
        false,
        with(&settings, hosts::strip_claude_settings),
    );
    let skill = claude
        .join("skills")
        .join(product().skill_dir())
        .join("SKILL.md");
    add(
        "claude",
        skill.clone(),
        "sieve skill",
        false,
        with(&skill, hosts::remove_owned),
    );
    // Only the files `init --mod` wrote. A file the user added stays.
    for path in crate::pane_mod::owned_paths(root) {
        let run = with(&path, hosts::remove_owned);
        add("claude", path, "sieve-pane mod file", false, run);
    }
    let mcp = root.join(".mcp.json");
    add(
        "claude",
        mcp.clone(),
        "mcpServers.sieve",
        false,
        mcp_json(&mcp, "mcpServers"),
    );

    if global {
        let hooks_what = "SessionStart / UserPromptSubmit / PostToolUse / Stop";
        let settings = home.join(".claude").join("settings.json");
        let run = with(&settings, hosts::strip_claude_global_hooks);
        add("claude", settings, hooks_what, true, run);
        let json = home.join(".claude.json");
        let run = mcp_json(&json, "mcpServers");
        add("claude", json, "mcpServers.sieve", true, run);
        if home.join(".codex").is_dir() {
            let path = home.join(".codex").join("hooks.json");
            let run = with(&path, hosts::strip_codex_hooks);
            add("agents", path, hooks_what, true, run);
        }
        let skill = home
            .join(".gemini")
            .join("skills")
            .join(product().skill_dir())
            .join("SKILL.md");
        let run = with(&skill, hosts::remove_owned);
        add("antigravity", skill, "sieve skill (shared)", true, run);
    }

    if cache {
        let name = product().context_dir_name();
        let dir = root.join(name);
        let run_dir = dir.clone();
        add(
            "graph",
            dir,
            "local graph cache",
            false,
            Box::new(move |apply| remove_dir(&run_dir, apply)),
        );
        let ignore = root.join(".gitignore");
        add(
            "graph",
            ignore.clone(),
            &format!("{name}/ ignore entry"),
            false,
            with(&ignore, hosts::strip_gitignore),
        );
        let search = root.join(".ignore");
        add(
            "graph",
            search.clone(),
            &format!("{name}/ search re-admit entries"),
            false,
            with(&search, hosts::strip_ignore),
        );
    }
    out
}

/// Removes the context dir.
fn remove_dir(path: &Path, apply: bool) -> std::io::Result<Retract> {
    if !path.is_dir() {
        return Ok(Retract::Absent);
    }
    if apply {
        std::fs::remove_dir_all(path)?;
    }
    Ok(Retract::Deleted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(
        host: &'static str,
        path: &str,
        what: &str,
        global: bool,
        action: Retract,
    ) -> Outcome {
        Outcome {
            host,
            path: PathBuf::from(path),
            what: what.to_string(),
            global,
            action,
        }
    }

    /// The expected report for the same outcomes (captured
    /// from `sieve uninstall` and `sieve uninstall -y` in a scratch repo).
    #[test]
    fn test_p1_72_report_groups_lines_by_host_as_golden() {
        let rs = [
            outcome(
                "agents",
                "/r/AGENTS.md",
                "fenced sieve section",
                false,
                Retract::Removed,
            ),
            outcome(
                "agents",
                "/h/.codex/config.toml",
                "[mcp_servers.sieve]",
                true,
                Retract::Removed,
            ),
            outcome(
                "adal",
                "/r/.adal/skills/x/SKILL.md",
                "sieve-owned instruction file",
                false,
                Retract::Deleted,
            ),
            outcome(
                "agents",
                "/h/.codex/hooks.json",
                "SessionStart",
                true,
                Retract::Deleted,
            ),
            outcome(
                "claude",
                "/r/.mcp.json",
                "mcpServers.sieve",
                false,
                Retract::Unparseable,
            ),
        ];
        let want = "agents:\n  ~ would remove: /r/AGENTS.md (fenced sieve section)\n  ~ would remove: /h/.codex/config.toml [machine-wide] ([mcp_servers.sieve])\n  - would remove: /h/.codex/hooks.json [machine-wide] (SessionStart \u{2014} deleted)\n\nadal:\n  - would remove: /r/.adal/skills/x/SKILL.md (sieve-owned instruction file \u{2014} deleted)\n\nclaude:\n  \u{26a0} would remove: /r/.mcp.json \u{2014} not valid JSON, left untouched (remove the sieve entry by hand)";
        assert_eq!(format_retractions(&Ui::plain(), &rs, false), want);
        assert_eq!(
            format_retractions(&Ui::plain(), &rs, true),
            want.replace("would remove", "removed")
        );
    }

    #[test]
    fn test_p1_72_report_with_nothing_to_remove() {
        assert_eq!(
            format_retractions(&Ui::plain(), &[], false),
            "nothing to remove \u{2014} no sieve files found here"
        );
    }
}
