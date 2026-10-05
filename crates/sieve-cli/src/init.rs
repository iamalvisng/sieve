//! `sieve init [dir]`: wires every detected or selected agent host into
//! the repo, merges Claude Code's `settings.json` and `.mcp.json`, and
//! mirrors the hooks into `$HOME` for Claude and Codex
//! (the `hosts-hooks.md` note section 1).

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use clap::Args;
use sieve_core::product::product;
use sieve_core::workspace;

use crate::build::{resolve_abs, BuildArgs};
use crate::hosts::{
    self, epilogue, graph_counts, json_mcp_targets, merge_claude_settings, merge_mcp_json,
    merge_mcp_toml, merge_opencode_json, render_owned, upsert_section, write_cursor_hooks,
    HostSpec, Kind, Retract, WriteAction,
};
use crate::names;
use crate::pane_mod;

/// Flags for `sieve init`. `--dry-run` writes nothing, prints the plan on
/// stderr and exits 0.
#[derive(Args, Debug)]
pub struct InitArgs {
    /// The repo root. Default: the current dir.
    #[arg(value_name = "dir", default_value = ".")]
    pub dir: PathBuf,

    #[arg(long = "no-build")]
    pub no_build: bool,
    #[arg(long = "agents", value_name = "ids", num_args = 1..)]
    pub agents: Vec<String>,
    #[arg(long = "all-agents")]
    pub all_agents: bool,
    #[arg(long = "no-agents")]
    pub no_agents: bool,
    #[arg(long = "list-agents")]
    pub list_agents: bool,
    #[arg(long = "no-mcp")]
    pub no_mcp: bool,
    #[arg(long = "no-hooks")]
    pub no_hooks: bool,
    #[arg(long = "no-statusline")]
    pub no_statusline: bool,
    #[arg(long = "dry-run")]
    pub dry_run: bool,
    #[arg(short = 'y', long = "yes")]
    pub yes: bool,
    /// Writes or removes files under `HOME` too. Without this flag, `init`
    /// touches only the repo (ledger "sieve init writes under HOME only
    /// with --global", 2026-09-16).
    #[arg(long = "global", conflicts_with = "no_global")]
    pub global: bool,
    /// Skips writes outside this repo. Sieve already skips them without
    /// `--global`; this flag only picks the matching report line.
    // P4-48: the doc comment above reaches the derived `--help`
    // (`help_cli.rs` pins the text).
    #[arg(long = "no-global")]
    pub no_global: bool,
    /// Prints every file written, the build output and the closing banner.
    #[arg(long = "verbose")]
    pub verbose: bool,
    /// Installs the sieve-pane Claude Code mod into the project.
    #[arg(long = "mod", id = "mod")]
    pub mod_: bool,
}

/// What one `init` step reports. The compact default prints only warnings
/// (held for the end); `--verbose` prints every line at once (P4-47).
struct Out {
    verbose: bool,
    warnings: Vec<String>,
    /// Report lines that the compact report prints after the warnings.
    notes: Vec<String>,
}

impl Out {
    fn say(&mut self, line: String) {
        if self.verbose {
            eprintln!("{line}");
        } else if line.starts_with('\u{26a0}') {
            self.warnings.push(line);
        }
    }
}

macro_rules! say {
    ($out:expr, $($arg:tt)*) => {
        $out.say(format!($($arg)*))
    };
}

/// Runs `sieve init`.
pub fn run(args: &InitArgs, context_dir_override: Option<&Path>) -> Result<(), String> {
    if args.list_agents {
        for id in agent_ids() {
            println!("{id}");
        }
        return Ok(());
    }

    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let root = resolve_abs(&cwd, &args.dir);
    let context_dir = match context_dir_override {
        Some(dir) => resolve_abs(&cwd, dir),
        None => root.join(product().context_dir_name()),
    };

    let Some(selected) = select_hosts(args, &root)? else {
        return Ok(());
    };

    // P4-47: `--dry-run` writes nothing and prints the plan on stderr
    // (the parent plan, then one plan per workspace child).
    if args.dry_run {
        let home = home_dir()?;
        let plan = plan_writes(&root, &home, &selected, args, false);
        eprintln!("{}", format_plan(&plan, &root, &home));
        print_skipped(args, &selected);
        if workspace::is_build_root(&root, &context_dir) {
            for child in workspace::discover_children(&root) {
                let dir = root.join(&child);
                let plan = plan_writes(&dir, &home, &selected, args, false);
                eprintln!(
                    "\n\u{2014} {child}/ (workspace child)\n{}",
                    format_plan(&plan, &dir, &home)
                );
            }
        }
        return Ok(());
    }

    // A workspace parent wires itself FIRST, then each child.
    // The parent's build is the workspace build, which
    // builds every child graph, so a child's own build then finds one.
    let children = if workspace::is_build_root(&root, &context_dir) {
        workspace::discover_children(&root)
    } else {
        Vec::new()
    };
    if !children.is_empty() {
        eprintln!(
            "\u{b7} workspace: wiring {} and {} child repo(s) \u{2014} {}",
            root.display(),
            children.len(),
            children.join(", ")
        );
    }
    let mut out = Out {
        verbose: args.verbose,
        warnings: Vec::new(),
        notes: Vec::new(),
    };
    let mut retracted = wire_target(&root, &context_dir, &selected, args, &mut out)?;
    for child in &children {
        if args.verbose {
            eprintln!("\n\u{2014} {child}/");
        }
        let dir = root.join(child);
        let child_context = dir.join(product().context_dir_name());
        retracted.extend(wire_target(
            &dir,
            &child_context,
            &selected,
            args,
            &mut out,
        )?);
    }

    if !args.verbose {
        let ctx = CompactCtx {
            root: &root,
            context_dir: &context_dir,
            context_override: context_dir_override,
            has_children: !children.is_empty(),
        };
        let mut lines = out.warnings;
        lines.append(&mut out.notes);
        print_compact(args, &ctx, &selected, &retracted, lines)?;
        return Ok(());
    }

    // A workspace parent holds no nodes, so the totals sum the children.
    let counts = if children.is_empty() {
        graph_counts(&context_dir)
    } else {
        children
            .iter()
            .filter_map(|c| graph_counts(&root.join(c).join(product().context_dir_name())))
            .reduce(|a, b| (a.0 + b.0, a.1 + b.1))
    };
    eprint!("{}", epilogue(counts));

    Ok(())
}

/// One repo's worth of `init` writes: the parent, then each workspace child.
fn wire_target(
    root: &Path,
    context_dir: &Path,
    selected: &[&str],
    args: &InitArgs,
    out: &mut Out,
) -> Result<Vec<String>, String> {
    let no_statusline = args.no_statusline || env_truthy(&product().env_var("NO_STATUSLINE"));
    let mut warnings: Vec<String> = Vec::new();
    // Tracks whether any write under `HOME` was skipped for lack of
    // `--global`, so the caller prints one summary line, not one per site.
    let mut skipped_global = false;

    let retracted = retract_unselected(root, selected, args.global, args.verbose);

    // Claude branch: runs only when the selection holds `claude`. The
    // `want_claude` flag is set from the selection and
    // gates every Claude write on it. Section 1.2's
    // "prepended always" describes the picker plan, not this write gate.
    let want_claude = selected.contains(&"claude");
    if want_claude {
        // Every Claude write below runs BEFORE any of its own report lines
        // print (the `init` flow, which writes settings, the skill, and
        // `.mcp.json`, THEN runs `buildGraphIfMissing` in-process, and only
        // after that returns does its caller print the "✓ wrote ..." / "✓ built
        // the graph" lines). On a repo with no graph, the build subprocess's
        // own stdout/stderr (the `parsing N/M` progress, the build report) is
        // inherited and so prints first; only once the build finishes does
        // Sieve report what it wrote. On a repo with a graph already built,
        // there is no build output to print first, so the two orders look the
        // same.
        let settings_path = root.join(".claude").join("settings.json");
        let (settings_action, mut settings_warnings) =
            merge_claude_settings(&settings_path, no_statusline).map_err(|e| e.to_string())?;
        warnings.append(&mut settings_warnings);

        // No product writes a `.cjs` shim (ledger 2026-09-12): the hook and
        // statusline commands resolve the binary by `PATH`.

        let skill_path = root
            .join(".claude")
            .join("skills")
            .join(product().skill_dir())
            .join("SKILL.md");
        hosts::write_owned(&skill_path, &hosts::skill_template()).map_err(|e| e.to_string())?;

        // `--no-mcp` gates the other hosts only; `init` writes Claude's
        // `.mcp.json` with no such gate (P4-47).
        let mcp_path = root.join(".mcp.json");
        let mcp_action = merge_mcp_json(&mcp_path, "mcpServers").map_err(|e| e.to_string())?;

        // The mod merge runs after the hooks merge, on the same file.
        let mod_lines = if args.mod_ {
            pane_mod::install(root).map_err(|e| e.to_string())?
        } else {
            Vec::new()
        };

        let built = args.verbose && build_graph_if_missing(root, context_dir, args.no_build)?;

        if settings_action == WriteAction::SkippedUnparseable {
            say!(
                out,
                "⚠ .claude/settings.json: {} left unchanged (not valid JSON)",
                settings_path.display()
            );
        } else {
            say!(out, "✓ wrote {}", settings_path.display());
        }
        say!(out, "✓ wrote {}", skill_path.display());
        match mcp_action {
            WriteAction::Unchanged => {
                say!(
                    out,
                    "· mcp claude: {} (already registered)",
                    mcp_path.display()
                );
            }
            WriteAction::SkippedUnparseable => {
                say!(
                    out,
                    "⚠ .mcp.json: {} left unchanged (not valid JSON) — add the {} server manually",
                    mcp_path.display(),
                    product().name
                );
            }
            action => {
                say!(
                    out,
                    "✓ mcp claude: {} ({}) — restart Claude Code to load the {} MCP server",
                    mcp_path.display(),
                    action.word(),
                    product().name
                );
            }
        }
        say_build(out, built);
        for line in mod_lines {
            if out.verbose {
                out.say(line);
            } else {
                out.notes.push(line);
            }
        }

        if no_statusline {
            say!(out, "· skipped Claude Code statusLine (--no-statusline)");
        }
    }
    // A hook whose command is not on `PATH` fails silently: the host runs
    // it, gets nothing, and reports nothing (probe of 2026-09-30).
    warnings.extend(missing_bin_warning(
        names::BIN,
        std::env::var_os("PATH").as_deref(),
    ));
    for warning in &warnings {
        say!(out, "⚠ {warning}");
    }

    for host in hosts::hosts() {
        if !selected.contains(&host.id) {
            continue;
        }
        write_host(root, &host, out)?;
    }

    if !args.no_mcp {
        // The registry order (`json_mcp_targets`) interleaves `grok`'s
        // TOML target between `gemini` and `antigravity`; split the loop
        // so the report lines land in that same order. Codex and opencode
        // come first, as the `agents` host does in the registry (P4-51).
        let home = home_dir()?;
        if selected.contains(&"agents") {
            if let Some(path) = hosts::codex_mcp_path(&home) {
                if args.global {
                    let action = merge_mcp_toml(&path).map_err(|e| e.to_string())?;
                    report_mcp_toml(out, "codex", &path, action);
                } else {
                    skipped_global = true;
                }
            }
            if let Some(path) = hosts::opencode_mcp_path(root, &home) {
                let action = merge_opencode_json(&path).map_err(|e| e.to_string())?;
                say!(
                    out,
                    "✓ mcp opencode: {} ({})",
                    path.display(),
                    action.word()
                );
            }
        }
        let targets = json_mcp_targets();
        let mut write_json_target =
            |target: &hosts::McpTarget, out: &mut Out| -> Result<(), String> {
                if !selected.contains(&target.id) {
                    return Ok(());
                }
                if target.under_home && !args.global {
                    skipped_global = true;
                    return Ok(());
                }
                let base = if target.under_home {
                    home_dir()?
                } else {
                    root.to_path_buf()
                };
                let path = base.join(target.rel_path);
                let action = merge_mcp_json(&path, "mcpServers").map_err(|e| e.to_string())?;
                say!(
                    out,
                    "✓ mcp {}: {} ({})",
                    target.id,
                    path.display(),
                    action.word()
                );
                Ok(())
            };
        for target in targets
            .iter()
            .filter(|t| t.id == "cursor" || t.id == "gemini")
        {
            write_json_target(target, out)?;
        }
        if selected.contains(&"grok") {
            let path = root.join(".grok").join("config.toml");
            let action = merge_mcp_toml(&path).map_err(|e| e.to_string())?;
            report_mcp_toml(out, "grok", &path, action);
        }
        for target in targets
            .iter()
            .filter(|t| t.id == "antigravity" || t.id == "kiro")
        {
            write_json_target(target, out)?;
        }
    }

    // The hook lines run `codex-hooks`, `cursor-hooks`, then
    // `antigravity-skill` (P4-52).
    if args.global && !args.no_hooks && selected.contains(&"agents") {
        match install_codex_hooks() {
            Ok(Some((path, action))) => {
                say!(
                    out,
                    "✓ hook codex-hooks: {} ({})",
                    path.display(),
                    action.word()
                );
            }
            Ok(None) => {}
            Err(e) => say!(out, "⚠ could not mirror the global Codex hooks: {e}"),
        }
    }
    if !args.no_hooks && selected.contains(&"cursor") {
        let path = root.join(".cursor").join("hooks.json");
        let action = write_cursor_hooks(&path).map_err(|e| e.to_string())?;
        say!(
            out,
            "✓ hook cursor-hooks: {} ({})",
            path.display(),
            action.word()
        );
    }
    // Sieve writes this skill under `--no-hooks` too: the P4-47 golden
    // `init-no-hooks.written.txt` lists `home/.gemini/skills/sieve/SKILL.md`.
    if selected.contains(&"antigravity") {
        if args.global {
            let home = home_dir()?;
            let skill_path = home
                .join(".gemini")
                .join("skills")
                .join(product().skill_dir())
                .join("SKILL.md");
            let action = match hosts::write_owned(&skill_path, &hosts::skill_template())
                .map_err(|e| e.to_string())?
            {
                WriteAction::Replaced => WriteAction::Updated,
                other => other,
            };
            say!(
                out,
                "✓ hook antigravity-skill: {} ({})",
                skill_path.display(),
                action.word()
            );
        } else {
            skipped_global = true;
        }
    }

    if args.global {
        if want_claude {
            if let Err(e) = install_claude_global() {
                say!(out, "⚠ could not mirror the global Claude Code hooks: {e}");
            }
        }
    } else {
        skipped_global = true;
    }
    if args.no_global {
        // The line is printed only when another host is selected and the plan
        // holds an out-of-repo write. Claude and antigravity are the hosts with
        // such writes (P4-48).
        let others = selected.iter().any(|id| *id != "claude");
        if others && (want_claude || selected.contains(&"antigravity")) {
            say!(out, "· skipped out-of-repo writes (--no-global)");
        }
    } else if skipped_global {
        say!(
            out,
            "· skipped global hooks under ~ (pass --global to write them)"
        );
    }

    // Without Claude, `init` still builds the graph after the other hosts
    // and reports the build on its own line.
    if !want_claude && args.verbose {
        let built = build_graph_if_missing(root, context_dir, args.no_build)?;
        say_build(out, built);
    }
    Ok(retracted)
}

/// The verbose report line of the graph step.
fn say_build(out: &mut Out, built: bool) {
    if built {
        say!(out, "✓ built the graph ({} build)", product().name);
    } else {
        say!(out, "· skipped graph build");
    }
}

/// Removes the wiring of every agent that is not selected now (P4-49).
/// Order: instruction files, MCP entries, Claude files, then the
/// `HOME` files with `--global`. Each removed target prints
/// `- removed <path> (<what>) — agent not selected`. A file or entry that
/// holds no Sieve content is never touched. A path a selected agent also
/// writes (the shared `AGENTS.md`) is skipped.
// Sieve writes no `.cjs` shim, so it removes none.
fn retract_unselected(root: &Path, selected: &[&str], global: bool, verbose: bool) -> Vec<String> {
    let sel = |id: &str| selected.contains(&id);
    let mut skip: Vec<PathBuf> = Vec::new();
    for host in hosts::hosts() {
        if sel(host.id) {
            skip.push(root.join(&host.rel_path));
        }
    }
    let home = if global { home_dir().ok() } else { None };
    let mcp_path = |t: &hosts::McpTarget| match (&home, t.under_home) {
        (Some(h), true) => Some(h.join(t.rel_path)),
        (None, true) => None,
        (_, false) => Some(root.join(t.rel_path)),
    };
    for t in json_mcp_targets() {
        if sel(t.id) {
            skip.extend(mcp_path(&t));
        }
    }
    let mut seen: Vec<PathBuf> = Vec::new();
    // The ids of the agents that lost a file, once each, for the compact
    // report `- removed <id> — agent not selected` (P4-47).
    let mut removed: Vec<String> = Vec::new();
    let mut run = |id: &str,
                   path: PathBuf,
                   what: &str,
                   f: &dyn Fn(&Path, bool) -> std::io::Result<Retract>| {
        if skip.contains(&path) || seen.contains(&path) {
            return;
        }
        seen.push(path.clone());
        match f(&path, true) {
            Ok(r) if r.hit() => {
                if verbose {
                    eprintln!(
                        "- removed {} ({}) \u{2014} agent not selected",
                        path.display(),
                        what
                    );
                } else if !removed.iter().any(|r| r == id) {
                    removed.push(id.to_string());
                }
                let top = match &home {
                    Some(h) if path.starts_with(h) => h.as_path(),
                    _ => root,
                };
                if !path.exists() {
                    prune_empty_dirs(path.parent(), top);
                }
            }
            Ok(_) => {}
            Err(e) => eprintln!("\u{26a0} could not retract {}: {e}", path.display()),
        }
    };
    let mcp_json = |p: &Path, apply: bool| hosts::strip_mcp_json(p, "mcpServers", apply);
    for host in hosts::hosts().into_iter().filter(|h| !sel(h.id)) {
        let path = root.join(&host.rel_path);
        match host.kind {
            Kind::Owned => run(
                host.id,
                path,
                "sieve-owned instruction file",
                &hosts::remove_owned,
            ),
            Kind::Section => run(host.id, path, "fenced sieve section", &hosts::strip_section),
        }
    }
    let targets = json_mcp_targets();
    let json_target = |id: &str| targets.iter().find(|t| t.id == id).and_then(mcp_path);
    // Codex's config is a `HOME` file, so it needs `--global`; opencode's is
    // in the repo. Both need the host's own config dir (P4-51).
    if !sel("agents") {
        if let Some(p) = home.as_deref().and_then(hosts::codex_mcp_path) {
            run("agents", p, "[mcp_servers.sieve]", &hosts::strip_mcp_toml);
        }
        if let Some(p) = home_dir()
            .ok()
            .and_then(|h| hosts::opencode_mcp_path(root, &h))
        {
            run("agents", p, "mcp.sieve", &|p, apply| {
                hosts::strip_mcp_json(p, "mcp", apply)
            });
        }
    }
    for id in ["cursor", "gemini"] {
        if let (false, Some(p)) = (sel(id), json_target(id)) {
            run(id, p, "mcpServers.sieve", &mcp_json);
        }
    }
    if !sel("grok") {
        let p = root.join(".grok").join("config.toml");
        run("grok", p, "[mcp_servers.sieve]", &hosts::strip_mcp_toml);
    }
    for id in ["antigravity", "kiro"] {
        if let (false, Some(p)) = (sel(id), json_target(id)) {
            run(id, p, "mcpServers.sieve", &mcp_json);
        }
    }
    let hooks_what = "SessionStart / UserPromptSubmit / PostToolUse / Stop";
    if !sel("claude") {
        let claude = root.join(".claude");
        let what = "statusline + hooks + allowlist + footer regex";
        run(
            "claude",
            claude.join("settings.json"),
            what,
            &hosts::strip_claude_settings,
        );
        let skill = claude.join("skills").join(product().skill_dir());
        run(
            "claude",
            skill.join("SKILL.md"),
            "sieve skill",
            &hosts::remove_owned,
        );
        run(
            "claude",
            root.join(".mcp.json"),
            "mcpServers.sieve",
            &mcp_json,
        );
        if let Some(h) = &home {
            let settings = h.join(".claude").join("settings.json");
            run(
                "claude",
                settings,
                hooks_what,
                &hosts::strip_claude_global_hooks,
            );
            run(
                "claude",
                h.join(".claude.json"),
                "mcpServers.sieve",
                &mcp_json,
            );
        }
    }
    if let Some(h) = &home {
        if !sel("agents") {
            let path = h.join(".codex").join("hooks.json");
            run("agents", path, hooks_what, &hosts::strip_codex_hooks);
        }
        if !sel("antigravity") {
            let skill = h.join(".gemini").join("skills").join(product().skill_dir());
            run(
                "antigravity",
                skill.join("SKILL.md"),
                "sieve skill (shared)",
                &hosts::remove_owned,
            );
        }
    }
    removed
}

/// Removes the empty directories a removed file leaves behind: at most 6 levels
/// up, stopping at the first non-empty directory. It never removes `top` (the
/// repo root or `HOME`) or anything above it.
pub(crate) fn prune_empty_dirs(mut dir: Option<&Path>, top: &Path) {
    for _ in 0..6 {
        let Some(d) = dir else { return };
        if d == top || !d.starts_with(top) {
            return;
        }
        let empty = std::fs::read_dir(d).is_ok_and(|mut it| it.next().is_none());
        if !empty || std::fs::remove_dir(d).is_err() {
            return;
        }
        dir = d.parent();
    }
}

/// Writes one registry host's own instruction file and reports the
/// result line, matching `✓ <id>: <path> (<action>)`.
fn write_host(root: &Path, host: &HostSpec, out: &mut Out) -> Result<(), String> {
    let path = root.join(&host.rel_path);
    let action = match host.kind {
        Kind::Owned => {
            let content = render_owned(host.wrap);
            hosts::write_owned(&path, &content)
        }
        Kind::Section => upsert_section(&path, &hosts::instruction_body()),
    }
    .map_err(|e| e.to_string())?;
    say!(out, "✓ {}: {} ({})", host.id, path.display(), action.word());
    Ok(())
}

/// Runs `sieve build` in-process unless `--no-build` was given or a graph
/// already exists at `<context dir>/.graph/wiring.json` (section 1.6).
/// Returns `true` on a fresh build, `false` when the build was skipped OR
/// failed. The caller prints the build's report line, after every Claude
/// write (see the ordering note above the settings merge, in `run`'s
/// Claude branch): the build itself runs, and prints its own progress,
/// before that line.
///
/// A build failure never stops `init`: the error text of the failed build
/// reaches the terminal, then `init` finishes the rest of the report and the
/// epilogue, and still exits 0. A repo where `sieve` exists as a plain file
/// makes the build's own `mkdir` fail. Sieve builds in-process instead of
/// spawning a child, so this prints the same text a direct `sieve build`
/// failure would print (`main.rs`'s `✗ {message}` / plain `ENOENT` rule) here,
/// in place of the child's inherited stderr, then reports the build as skipped.
/// Returns the warning `init` prints when no directory in `path` holds an
/// executable file named `name`. Every hook, statusline, and MCP entry `init`
/// writes runs the bare `name`, so the host resolves it by `PATH`. Returns
/// `None` when the name resolves.
fn missing_bin_warning(name: &str, path: Option<&std::ffi::OsStr>) -> Option<String> {
    let found = path
        .map(|p| std::env::split_paths(p).any(|dir| is_executable_file(&dir.join(name))))
        .unwrap_or(false);
    if found {
        return None;
    }
    Some(format!(
        "{name} is not on PATH: the hooks run `{name} hook ...` and do nothing, silently, \
         until it resolves — install {name} or add its directory to PATH"
    ))
}

/// `true` when `path` is a regular file the current user may execute, the
/// same test a shell applies to each `PATH` entry.
fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn build_graph_if_missing(root: &Path, context_dir: &Path, no_build: bool) -> Result<bool, String> {
    let wiring_path = context_dir.join(".graph").join("wiring.json");
    // A workspace parent's graph is its `workspace.json`.
    if no_build || wiring_path.is_file() || workspace::workspace_path(context_dir).is_file() {
        return Ok(false);
    }
    let build_args = BuildArgs {
        root_dir: root.to_path_buf(),
        no_gitignore: false,
        no_ignore: false,
        lsp: false,
        extensions: Vec::new(),
        include_dir: Vec::new(),
        follow_submodules: false,
        no_follow_submodules: false,
        follow_nested_repos: false,
        no_follow_nested_repos: false,
        only_dir: Vec::new(),
        no_reuse: false,
    };
    match crate::build::run(&build_args, Some(context_dir)) {
        Ok(()) => Ok(true),
        Err(message) => {
            if message.starts_with("ENOENT") {
                eprintln!("{message}");
            } else {
                eprintln!("✗ {message}");
            }
            Ok(false)
        }
    }
}

/// Mirrors Claude's hooks and MCP entry into `$HOME`. `init` writes
/// both files even when `$HOME/.claude` does not exist yet, so Sieve does
/// the same. The golden under `tests/fixtures/basic.expected/init/home/`
/// pins this.
fn install_claude_global() -> Result<(), String> {
    let home = home_dir()?;
    let settings_path = home.join(".claude").join("settings.json");
    hosts::merge_claude_global_hooks(&settings_path).map_err(|e| e.to_string())?;
    // Write 2 runs only after write 1 succeeded (section 1.9): the `?`
    // above already returns on a write-1 error, so nothing more to gate.
    let mcp_path = home.join(".claude.json");
    hosts::merge_mcp_json(&mcp_path, "mcpServers").map_err(|e| e.to_string())?;
    Ok(())
}

/// One file `init` writes, for the `--dry-run` plan.
struct PlanWrite {
    /// The host id the row belongs to. `plan_writes` sets it.
    id: &'static str,
    path: PathBuf,
    global: bool,
    what: String,
}

/// The files `init` writes for the selected hosts, in the
/// order: Claude Code first, then the registry hosts. Only files
/// Sieve really writes are listed. No product writes a `.cjs` shim or a
/// wiring stamp (ledger 2026-09-12), and Sieve writes no Codex or opencode
/// MCP entry, so those rows are absent. Sieve writes under `HOME` only with
/// `--global` (ledger 2026-09-16), so the global rows need that flag.
/// `--no-mcp` and `--no-hooks` drop the rows the real run skips.
fn plan_writes(
    root: &Path,
    home: &Path,
    selected: &[&str],
    args: &InitArgs,
    all_rows: bool,
) -> Vec<PlanWrite> {
    // The compact report lists every file the agent can own,
    // `planInit` does, whatever `--no-mcp` and `--no-hooks` skip (P4-47).
    let no_mcp = args.no_mcp && !all_rows;
    let no_hooks = args.no_hooks && !all_rows;
    let w = |path: PathBuf, global: bool, what: &str| PlanWrite {
        id: "",
        path,
        global,
        what: what.to_string(),
    };
    let mut out = Vec::new();
    if selected.contains(&"claude") {
        let claude = root.join(".claude");
        out.push(w(
            claude.join("settings.json"),
            false,
            if args.mod_ {
                "sieve statusline + hook blocks + enabledPlugins.sieve-pane@skills-dir"
            } else {
                "sieve statusline + hook blocks"
            },
        ));
        out.push(w(
            claude
                .join("skills")
                .join(product().skill_dir())
                .join("SKILL.md"),
            false,
            "sieve skill",
        ));
        out.push(w(root.join(".mcp.json"), false, "mcpServers.sieve"));
        if args.mod_ {
            for path in pane_mod::owned_paths(root) {
                out.push(w(path, false, "sieve-pane mod file"));
            }
        }
        if args.global {
            out.push(w(
                home.join(".claude").join("settings.json"),
                true,
                "SessionStart / UserPromptSubmit / PostToolUse / Stop",
            ));
            out.push(w(home.join(".claude.json"), true, "mcpServers.sieve"));
        }
        for row in &mut out {
            row.id = "claude";
        }
    }
    for host in hosts::hosts() {
        if !selected.contains(&host.id) {
            continue;
        }
        let first = out.len();
        let what = if host.kind == Kind::Owned {
            "sieve-owned file"
        } else {
            "fenced sieve section"
        };
        out.push(w(root.join(&host.rel_path), false, what));
        // `--no-mcp` skips every host MCP file but Claude's `.mcp.json`, and
        // a file under `HOME` needs `--global` (ledger 2026-09-16).
        let mcp_targets = json_mcp_targets();
        let wanted = mcp_targets
            .iter()
            .filter(|t| t.id == host.id && !no_mcp && (args.global || !t.under_home));
        for target in wanted {
            let base = if target.under_home { home } else { root };
            out.push(w(
                base.join(target.rel_path),
                target.under_home,
                "mcpServers.sieve",
            ));
        }
        // Codex's config is global; opencode's is in the repo. Each needs
        // the host's own config dir (P4-51).
        if host.id == "agents" && !no_mcp {
            if args.global {
                if let Some(path) = hosts::codex_mcp_path(home) {
                    out.push(w(path, true, "[mcp_servers.sieve]"));
                }
            }
            if let Some(path) = hosts::opencode_mcp_path(root, home) {
                out.push(w(path, false, "mcp.sieve"));
            }
        }
        match host.id {
            "grok" if !no_mcp => out.push(w(
                root.join(".grok").join("config.toml"),
                false,
                "[mcp_servers.sieve]",
            )),
            "agents" if args.global && !no_hooks && home.join(".codex").is_dir() => {
                out.push(w(
                    home.join(".codex").join("hooks.json"),
                    true,
                    "SessionStart / UserPromptSubmit / PostToolUse / Stop",
                ));
            }
            "cursor" if !no_hooks => out.push(w(
                root.join(".cursor").join("hooks.json"),
                false,
                "postToolUse / afterMCPExecution / sessionEnd",
            )),
            "antigravity" if args.global => out.push(w(
                home.join(".gemini")
                    .join("skills")
                    .join(product().skill_dir())
                    .join("SKILL.md"),
                true,
                "sieve skill (shared)",
            )),
            _ => {}
        }
        for row in &mut out[first..] {
            row.id = host.id;
        }
    }
    out
}

/// What `print_compact` needs to know about the repo.
struct CompactCtx<'a> {
    root: &'a Path,
    context_dir: &'a Path,
    context_override: Option<&'a Path>,
    has_children: bool,
}

/// The totals a quiet build reports.
struct QuietBuild {
    built: bool,
    failed: bool,
    /// Nodes, edges and parsed files, when the build printed them.
    graph: Option<(usize, usize, usize)>,
    /// The warning and error lines the build printed.
    messages: Vec<String>,
}

/// Formats a count with a comma between each group of 3 digits.
fn fmt_count(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Splits the run of digits and commas that starts `text` from the rest.
/// The count has commas removed. A run with no digit counts as 0.
fn take_count(text: &str) -> Option<(usize, &str)> {
    let end = text
        .find(|c: char| !(c.is_ascii_digit() || c == ','))
        .unwrap_or(text.len());
    if end == 0 {
        return None;
    }
    Some((
        text[..end].replace(',', "").parse().unwrap_or(0),
        &text[end..],
    ))
}

/// The first `<marker><count>` in one line of `text`, and the rest after it.
fn find_count<'a>(text: &'a str, marker: &str) -> Option<(usize, &'a str)> {
    let mut from = 0;
    while let Some(i) = text[from..].find(marker) {
        let start = from + i + marker.len();
        if let Some(found) = take_count(&text[start..]) {
            return Some(found);
        }
        from = start;
    }
    None
}

/// Reads the totals out of a build's stdout. It uses two unanchored patterns:
/// `✓ wiring: N nodes.*?, E edges` on one line, and `parsed: X of Y files`
/// anywhere.
fn parse_build_summary(stdout: &str) -> Option<(usize, usize, usize)> {
    let (nodes, edges) = stdout.lines().find_map(|line| {
        let (nodes, rest) = find_count(line, "\u{2713} wiring: ")?;
        let mut rest = rest.strip_prefix(" nodes")?;
        // The lazy `.*?, ` of the pattern: the first `, N edges`.
        while let Some(i) = rest.find(", ") {
            rest = &rest[i + 2..];
            if let Some((edges, tail)) = take_count(rest) {
                if tail.starts_with(" edges") {
                    return Some((nodes, edges));
                }
            }
        }
        None
    })?;
    let files = stdout
        .lines()
        .find_map(|line| {
            let (_, rest) = find_count(line, "parsed: ")?;
            let (files, tail) = take_count(rest.strip_prefix(" of ")?)?;
            tail.starts_with(" files").then_some(files)
        })
        .unwrap_or(0);
    Some((nodes, edges, files))
}

/// How long the quiet build may run before `init` stops it.
const BUILD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Runs `cmd` with both outputs piped. It stops the child after `limit`.
/// Returns the stdout, the stderr and whether the child exited with 0.
/// The std lib has no wait with a timeout, so a loop polls the child and
/// two threads drain the pipes.
fn run_with_timeout(
    mut cmd: std::process::Command,
    limit: std::time::Duration,
) -> Option<(String, String, bool)> {
    use std::io::Read;
    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut bytes);
            }
            String::from_utf8_lossy(&bytes).into_owned()
        })
    };
    let out = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let err = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let start = std::time::Instant::now();
    let ok = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if start.elapsed() >= limit => {
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
            Err(_) => break false,
        }
    };
    Some((out.join().ok()?, err.join().ok()?, ok))
}

/// Builds the graph unless one exists, in a child process whose output is read,
/// not shown. Only the warning and error lines it printed come back.
fn build_quiet(ctx: &CompactCtx, no_build: bool) -> QuietBuild {
    let wiring = ctx.context_dir.join(".graph").join("wiring.json");
    let has_index = wiring.is_file() || workspace::workspace_path(ctx.context_dir).is_file();
    let none = |failed| QuietBuild {
        built: false,
        failed,
        graph: None,
        messages: Vec::new(),
    };
    if no_build || has_index {
        return none(false);
    }
    let Ok(exe) = std::env::current_exe() else {
        return none(true);
    };
    let mut cmd = std::process::Command::new(exe);
    if ctx.context_override.is_some() {
        cmd.arg("--dir").arg(ctx.context_dir);
    }
    cmd.args(["build", "."])
        .current_dir(ctx.root)
        .stdin(std::process::Stdio::null());
    let Some((stdout, stderr, ok)) = run_with_timeout(cmd, BUILD_TIMEOUT) else {
        return none(true);
    };
    // Delete the text after a `\r` (a redrawn progress line) in stderr.
    let stderr = strip_redrawn(&stderr);
    let messages = stdout
        .lines()
        .chain(stderr.lines())
        .map(|l| l.trim_end().to_string())
        .filter(|l| l.trim_start().starts_with(['\u{26a0}', '\u{2717}']))
        .collect();
    if !ok {
        return QuietBuild {
            built: false,
            failed: true,
            graph: None,
            messages,
        };
    }
    QuietBuild {
        built: true,
        failed: false,
        graph: parse_build_summary(&stdout),
        messages,
    }
}

/// Replaces each `\r` and the text after it, up to the line end, with a
/// line break.
fn strip_redrawn(text: &str) -> String {
    let mut out = String::new();
    let mut skipping = false;
    for c in text.chars() {
        match (skipping, c) {
            (_, '\r') if !skipping => {
                skipping = true;
                out.push('\n');
            }
            (true, '\n') => {
                skipping = false;
                out.push('\n');
            }
            (true, _) => {}
            (false, _) => out.push(c),
        }
    }
    out
}

/// The one-line summary of what an agent writes:
/// at most 2 repo entries, a folder written into several times shown once
/// as `folder/`, then the count of writes under `HOME`.
fn compact_writes(rows: &[&PlanWrite], root: &Path, home: &Path) -> String {
    let repo_paths: Vec<String> = rows
        .iter()
        .filter(|r| !r.global)
        .map(|r| {
            r.path
                .strip_prefix(root)
                .unwrap_or(&r.path)
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    let mut tops: Vec<(String, Vec<String>)> = Vec::new();
    for p in &repo_paths {
        let top = p.split('/').next().unwrap_or(p).to_string();
        match tops.iter_mut().find(|(t, _)| *t == top) {
            Some((_, list)) => list.push(p.clone()),
            None => tops.push((top, vec![p.clone()])),
        }
    }
    let entries: Vec<String> = if tops.len() >= 2 {
        tops.iter()
            .map(|(top, list)| {
                if list.len() >= 2 {
                    format!("{top}/")
                } else {
                    list[0].clone()
                }
            })
            .collect()
    } else {
        repo_paths
    };
    let mut parts: Vec<String> = Vec::new();
    if !entries.is_empty() {
        parts.push(if entries.len() <= 2 {
            entries.join(", ")
        } else {
            format!("{} +{} more", entries[..2].join(", "), entries.len() - 2)
        });
    }
    let globals: Vec<&&PlanWrite> = rows.iter().filter(|r| r.global).collect();
    if !globals.is_empty() {
        let dirs: Vec<Vec<_>> = globals
            .iter()
            .map(|r| r.path.parent().unwrap_or(home).components().collect())
            .collect();
        let mut common = 0;
        while dirs[0].len() > common && dirs.iter().all(|d| d.get(common) == dirs[0].get(common)) {
            common += 1;
        }
        let dir: PathBuf = dirs[0][..common].iter().collect();
        let shown = match dir.strip_prefix(home) {
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => dir.display().to_string(),
        };
        let shown = if shown.ends_with('/') {
            shown
        } else {
            format!("{shown}/")
        };
        parts.push(format!("+ {} in {shown}", globals.len()));
    }
    parts.join(" \u{b7} ")
}

/// Prints the compact `init` report (P4-47): the graph line, the removals, the
/// warnings, one line per agent, then two epilogue lines. `--verbose` prints
/// the old per-file lines instead.
fn print_compact(
    args: &InitArgs,
    ctx: &CompactCtx,
    selected: &[&str],
    retracted: &[String],
    warnings: Vec<String>,
) -> Result<(), String> {
    let name = product().name;
    let res = build_quiet(ctx, args.no_build);
    for m in &res.messages {
        eprintln!("{m}");
    }
    let wiring = ctx.context_dir.join(".graph").join("wiring.json");
    let has_index = wiring.is_file() || workspace::workspace_path(ctx.context_dir).is_file();
    let graph_line = if let Some((nodes, edges, files)) = res.graph {
        let from = match files {
            0 => String::new(),
            1 => " from 1 file".to_string(),
            n => format!(" from {} files", fmt_count(n)),
        };
        format!(
            "\u{2713} graph built \u{b7} {} nodes, {} edges{from}",
            fmt_count(nodes),
            fmt_count(edges)
        )
    } else if res.built {
        "\u{2713} graph built".to_string()
    } else if res.failed {
        format!("\u{26a0} the graph build failed \u{2014} run {name} build to see why")
    } else if let Some((nodes, edges)) = graph_counts(ctx.context_dir).filter(|_| !ctx.has_children)
    {
        format!(
            "\u{2713} graph ready \u{b7} {} nodes, {} edges",
            fmt_count(nodes),
            fmt_count(edges)
        )
    } else if has_index {
        "\u{2713} graph ready".to_string()
    } else {
        format!("\u{b7} skipped the graph build \u{2014} run {name} build")
    };
    eprintln!("{graph_line}");
    for id in retracted {
        eprintln!("- removed {id} \u{2014} agent not selected");
    }
    for w in &warnings {
        eprintln!("{w}");
    }

    let home = home_dir()?;
    let plan = plan_writes(ctx.root, &home, selected, args, true);
    // The order of `--agents` stays as given. `--all-agents` lists the plan
    // order, Claude Code first; `-y` and `--no-agents`
    // already give that order.
    let mut ids: Vec<&str> = selected.to_vec();
    if args.agents.is_empty() {
        ids.sort_by_key(|i| *i != "claude");
    }
    let width = ids.iter().map(|i| i.len()).max().unwrap_or(0);
    for id in &ids {
        let rows: Vec<&PlanWrite> = plan.iter().filter(|r| r.id == *id).collect();
        let summary = compact_writes(&rows, ctx.root, &home);
        eprintln!(
            "{}",
            format!("\u{2713} {id:<width$}   {summary}").trim_end()
        );
    }

    // What to commit: the top-level entries the selected agents wrote.
    let mut tops: Vec<String> = Vec::new();
    for row in plan.iter().filter(|r| !r.global && ids.contains(&r.id)) {
        let shown = row.path.strip_prefix(ctx.root).unwrap_or(&row.path);
        let parts: Vec<_> = shown.components().collect();
        let first = parts[0].as_os_str().to_string_lossy();
        let top = if parts.len() > 1 {
            format!("{first}/")
        } else {
            first.into_owned()
        };
        if !tops.contains(&top) {
            tops.push(top);
        }
    }
    eprintln!("\u{b7} restart your agents so a new session picks up sieve");
    if !tops.is_empty() {
        let more = if tops.len() > 4 {
            format!(" +{} more", tops.len() - 4)
        } else {
            String::new()
        };
        eprintln!(
            "\u{b7} commit {}{more} to share it \u{2014} {}/ stays local and git-ignored",
            tops[..tops.len().min(4)].join(" "),
            product().context_dir_name()
        );
    }
    print_skipped(args, selected);
    Ok(())
}

/// Names the hosts that `--yes` or `--dry-run` left out and how to add them.
fn print_skipped(args: &InitArgs, selected: &[&str]) {
    if (args.yes || args.dry_run) && args.agents.is_empty() && !args.all_agents && !args.no_agents {
        let skipped: Vec<&str> = agent_ids()
            .into_iter()
            .filter(|id| !selected.contains(id))
            .collect();
        if !skipped.is_empty() {
            eprintln!(
                "\u{b7} skipped {} (no marker in this repo) \u{2014} add with --agents <name>",
                skipped.join(", ")
            );
        }
    }
}

/// The `--dry-run` plan text:
/// repo writes first, then the out-of-repo writes in their own section.
/// ponytail: no ANSI dimming, which a TTY could show.
fn format_plan(writes: &[PlanWrite], root: &Path, home: &Path) -> String {
    if writes.is_empty() {
        return "would write \u{2014} nothing (no agents selected)".to_string();
    }
    let rows = |global: bool| -> Vec<(String, &str)> {
        writes
            .iter()
            .filter(|p| p.global == global)
            .map(|p| {
                let shown = if global {
                    match p.path.strip_prefix(home) {
                        Ok(rest) => format!("~/{}", rest.display()),
                        Err(_) => p.path.display().to_string(),
                    }
                } else {
                    p.path
                        .strip_prefix(root)
                        .unwrap_or(&p.path)
                        .display()
                        .to_string()
                };
                (shown, p.what.as_str())
            })
            .collect()
    };
    let pad = |rows: &[(String, &str)]| -> Vec<String> {
        let width = rows
            .iter()
            .map(|(p, _)| p.encode_utf16().count())
            .max()
            .unwrap_or(0);
        rows.iter()
            .map(|(p, what)| {
                let fill = width - p.encode_utf16().count();
                format!("  {p}{}  {what}", " ".repeat(fill))
            })
            .collect()
    };
    let mut lines: Vec<String> = Vec::new();
    let repo_rows = rows(false);
    if !repo_rows.is_empty() {
        lines.push("would write \u{2014} this repo:".to_string());
        lines.extend(pad(&repo_rows));
    }
    let global_rows = rows(true);
    if !global_rows.is_empty() {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push("would write \u{2014} your machine, affects ALL repos:".to_string());
        lines.extend(pad(&global_rows));
        lines.push(String::new());
        lines.push("suppress the out-of-repo writes by leaving out --global".to_string());
    }
    lines.push(String::new());
    lines.push("nothing was written (--dry-run)".to_string());
    lines.join("\n")
}

/// Prints the report line of a TOML MCP merge. A skipped file gets a
/// warning, as `.mcp.json` does.
fn report_mcp_toml(out: &mut Out, id: &str, path: &Path, action: WriteAction) {
    if action == WriteAction::SkippedUnparseable {
        say!(
            out,
            "\u{26a0} mcp {id}: {} left unchanged (not UTF-8 text, or mcp_servers.{} is set as an inline or dotted key) \u{2014} add the {} server manually",
            path.display(),
            product().mcp_key(),
            product().name
        );
    } else {
        say!(
            out,
            "\u{2713} mcp {id}: {} ({})",
            path.display(),
            action.word()
        );
    }
}

/// Writes the Codex hooks under `$HOME/.codex`, gated on that directory
/// already existing (section 1.10).
fn install_codex_hooks() -> Result<Option<(PathBuf, WriteAction)>, String> {
    let home = home_dir()?;
    if !home.join(".codex").is_dir() {
        return Ok(None);
    }
    let path = home.join(".codex").join("hooks.json");
    let action = hosts::merge_codex_hooks(&path).map_err(|e| e.to_string())?;
    Ok(Some((path, action)))
}

fn home_dir() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is not set".to_string())
}

/// Returns `true` when an env var is set to something other than unset,
/// `""`, `"0"`, or `"false"` (section 1.7).
fn env_truthy(name: &str) -> bool {
    match std::env::var(name) {
        Ok(v) => !matches!(v.as_str(), "" | "0" | "false"),
        Err(_) => false,
    }
}

/// Resolves which hosts `sieve init` writes to, in selection precedence
/// order (section 1.3). Returns `None` when nothing should be written
/// (an empty selection, a cancelled picker, or a non-interactive run
/// with no explicit flag) — the caller has already printed the reason.
fn select_hosts(args: &InitArgs, root: &Path) -> Result<Option<Vec<&'static str>>, String> {
    let known = agent_ids();
    let home = home_dir()?;

    let selected = if !args.agents.is_empty() {
        let mut unknown = Vec::new();
        for id in &args.agents {
            if !known.contains(&id.as_str()) {
                unknown.push(id.clone());
            }
        }
        if !unknown.is_empty() {
            return Err(format!(
                "unknown agent id(s): {} — valid: {}",
                unknown.join(", "),
                known.join(", ")
            ));
        }
        args.agents.clone()
    } else if args.all_agents {
        known.iter().map(|s| s.to_string()).collect()
    } else if args.no_agents {
        // `--no-agents` selects Claude Code only.
        vec!["claude".to_string()]
    } else if args.yes || args.dry_run {
        detect_hosts(&home, root, true)
    } else if std::io::stdin().is_terminal() && std::io::stderr().is_terminal() {
        // ponytail: no interactive picker yet (Phase 1 has no fixture
        // that drives a TTY session). Treat an interactive run like a
        // detected, non-interactive one until a picker is built.
        detect_hosts(&home, root, false)
    } else {
        eprintln!(
            "{}",
            non_interactive_help(&detect_hosts(&home, root, false))
        );
        return Ok(None);
    };

    if selected.is_empty() && args.agents.is_empty() {
        eprintln!("· no agents selected — nothing written");
        return Ok(None);
    }

    Ok(Some(
        selected
            .into_iter()
            .filter_map(|id| known.iter().find(|k| **k == id).copied())
            .collect(),
    ))
}

/// Every id `--agents` accepts and `--list-agents` prints: the registry
/// hosts, then `claude`. Both flags read this one list.
fn agent_ids() -> Vec<&'static str> {
    let mut ids: Vec<&'static str> = hosts::hosts().into_iter().map(|h| h.id).collect();
    ids.push("claude");
    ids
}

/// Returns the detected agent ids, in the plan order.
///
/// `claude` comes first and is always detected: the Claude plan row has
/// `detected: true`, and `-y` selects every detected row. The registry hosts
/// follow, in registry order, each probed the same way: the `$HOME` markers
/// first, then the in-repo marker.
///
/// With `repo_only` (the sieve product under `--yes`), a marker under
/// `$HOME` does not count, and only `.github/copilot-instructions.md`
/// marks Copilot, because most repos keep a `.github/` folder for CI.
fn detect_hosts(home: &Path, repo: &Path, repo_only: bool) -> Vec<String> {
    // Each marker is `(in_repo, path)`, so a repo that is `$HOME` or holds
    // `$HOME` still drops the `$HOME` markers under `repo_only`.
    let h = |parts: &[&str]| {
        (
            false,
            parts.iter().fold(home.to_path_buf(), |p, s| p.join(s)),
        )
    };
    let r = |name: &str| (true, repo.join(name));
    let markers = |id: &str| -> Vec<(bool, PathBuf)> {
        let all = match id {
            "agents" => vec![
                h(&[".codex"]),
                h(&[".config", "opencode"]),
                h(&[".config", "agents"]),
            ],
            "adal" => vec![h(&[".adal"]), r(".adal")],
            "cursor" => vec![h(&[".cursor"]), r(".cursor")],
            "gemini" => vec![h(&[".gemini"])],
            "grok" => vec![h(&[".grok"]), r(".grok")],
            "hermes" => vec![
                h(&[".hermes"]),
                h(&["AppData", "Local", "hermes"]),
                r(".hermes"),
            ],
            "antigravity" => vec![
                h(&[".gemini", "config"]),
                h(&[".gemini", "antigravity-cli"]),
                r(".agents"),
            ],
            "copilot" => vec![r(".github")],
            "kiro" => vec![h(&[".kiro"]), r(".kiro")],
            "windsurf" => vec![h(&[".codeium", "windsurf"]), r(".windsurf")],
            _ => Vec::new(),
        };
        if !repo_only {
            return all;
        }
        if id == "copilot" {
            return vec![(true, repo.join(".github").join("copilot-instructions.md"))];
        }
        all.into_iter().filter(|(in_repo, _)| *in_repo).collect()
    };
    let mut detected = vec!["claude".to_string()];
    detected.extend(
        hosts::hosts()
            .into_iter()
            .map(|h| h.id)
            .filter(|id| {
                markers(id)
                    .iter()
                    .any(|(_, p)| p.is_dir() || (*id == "copilot" && p.is_file()))
            })
            .map(str::to_string),
    );
    detected
}

/// The text `init` prints when no TTY exists and no selection flag was
/// given. It prints the non-interactive help.
fn non_interactive_help(detected: &[String]) -> String {
    let bin = product().name;
    let list = if detected.is_empty() {
        "none".to_string()
    } else {
        detected.join(", ")
    };
    let mut examples: Vec<(String, &str)> = Vec::new();
    if !detected.is_empty() {
        examples.push((
            format!("{bin} init --agents {}", detected.join(" ")),
            "wire these",
        ));
        examples.push((
            format!("{bin} init --yes"),
            "same, without spelling them out",
        ));
    }
    examples.push((format!("{bin} init --agents claude"), "Claude Code only"));
    examples.push((format!("{bin} init --dry-run"), "list every file first"));
    let width = examples
        .iter()
        .map(|(c, _)| c.chars().count())
        .max()
        .unwrap_or(0);
    let mut lines = vec![
        format!("{bin} init: no TTY to prompt on, and no --agents/--yes given \u{2014} nothing written."),
        format!("detected: {list}"),
        String::new(),
    ];
    lines.extend(
        examples
            .iter()
            .map(|(cmd, note)| format!("  {cmd:<width$}   # {note}")),
    );
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A scratch dir under the OS temp root that removes itself on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "sieve-init-{label}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            fs::create_dir_all(&path).expect("create scratch dir");
            Scratch(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// P4-01: detection probes `$HOME` and the repo, and always adds
    /// `claude` first.
    #[test]
    fn test_p4_01_detect_hosts_probes_home_and_repo_and_adds_claude_first() {
        let home = Scratch::new("detect-home");
        let repo = Scratch::new("detect-repo");

        assert_eq!(detect_hosts(&home.0, &repo.0, false), vec!["claude"]);

        fs::create_dir_all(home.0.join(".kiro")).expect("mkdir home .kiro");
        assert_eq!(
            detect_hosts(&home.0, &repo.0, false),
            vec!["claude", "kiro"]
        );

        fs::create_dir_all(repo.0.join(".cursor")).expect("mkdir repo .cursor");
        fs::create_dir_all(home.0.join(".config").join("opencode")).expect("mkdir opencode");
        // A plain `~/.gemini` selects gemini and not antigravity.
        fs::create_dir_all(home.0.join(".gemini")).expect("mkdir home .gemini");
        assert_eq!(
            detect_hosts(&home.0, &repo.0, false),
            vec!["claude", "agents", "cursor", "gemini", "kiro"]
        );
    }

    #[test]
    fn missing_bin_warning_names_the_command_when_path_lacks_it() {
        let empty = Scratch::new("warn-absent");
        let path = std::env::join_paths([&empty.0]).expect("join paths");

        let warning = missing_bin_warning("sieve", Some(&path)).expect("warns");

        assert!(
            warning.starts_with("sieve is not on PATH"),
            "got: {warning}"
        );
        assert!(warning.contains("`sieve hook ...`"), "got: {warning}");
        assert!(
            warning.contains("add its directory to PATH"),
            "got: {warning}"
        );
    }

    #[test]
    fn missing_bin_warning_warns_when_path_is_unset() {
        assert!(missing_bin_warning("sieve", None).is_some());
    }

    #[test]
    fn missing_bin_warning_is_none_when_an_executable_sieve_is_on_path() {
        let bin_dir = Scratch::new("warn-present");
        let bin = bin_dir.0.join("sieve");
        fs::write(&bin, "#!/bin/sh\n").expect("write shim");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        let path = std::env::join_paths([&bin_dir.0]).expect("join paths");

        assert_eq!(missing_bin_warning("sieve", Some(&path)), None);
    }

    #[cfg(unix)]
    #[test]
    fn missing_bin_warning_ignores_a_sieve_file_with_no_exec_bit() {
        use std::os::unix::fs::PermissionsExt;
        let bin_dir = Scratch::new("warn-noexec");
        let bin = bin_dir.0.join("sieve");
        fs::write(&bin, "").expect("write file");
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o644)).expect("chmod");
        let path = std::env::join_paths([&bin_dir.0]).expect("join paths");

        assert!(missing_bin_warning("sieve", Some(&path)).is_some());
    }

    #[test]
    fn test_p4_47_fmt_count_groups_digits_by_three() {
        assert_eq!(fmt_count(0), "0");
        assert_eq!(fmt_count(999), "999");
        assert_eq!(fmt_count(1204), "1,204");
        assert_eq!(fmt_count(1_942_000), "1,942,000");
    }

    #[test]
    fn test_p4_47_parse_build_summary_reads_the_totals() {
        let out = "\u{2713} wiring: 21 nodes (5 function, 3 file), 23 edges, 7 cards [ts]\n  parsed: 7 of 7 files (0 replayed from cache)\n";
        assert_eq!(parse_build_summary(out), Some((21, 23, 7)));
        assert_eq!(parse_build_summary("nothing here\n"), None);
    }

    #[test]
    fn test_p4_47_parse_build_summary_matches_golden_patterns() {
        // Unanchored: text before the marker is fine, and commas group digits.
        let out = "x \u{2713} wiring: 1,204 nodes (a, b), 2,000 edges\nparsed: 3 of 1,942 files\n";
        assert_eq!(parse_build_summary(out), Some((1204, 2000, 1942)));
        // No `parsed:` line gives 0 files.
        let out = "\u{2713} wiring: 4 nodes, 2 edges\n";
        assert_eq!(parse_build_summary(out), Some((4, 2, 0)));
        // A workspace build prints no `wiring:` line.
        assert_eq!(
            parse_build_summary("\u{2713} workspace: 2 repos federated\n"),
            None
        );
    }

    #[test]
    fn test_p4_47_strip_redrawn_drops_the_text_after_a_cr() {
        assert_eq!(strip_redrawn("a\rparsing 1/2\nb\n"), "a\n\nb\n");
    }

    #[test]
    fn test_p4_47_run_with_timeout_stops_a_slow_child() {
        let mut cmd = std::process::Command::new("sleep");
        cmd.arg("5");
        let start = std::time::Instant::now();
        let got = run_with_timeout(cmd, std::time::Duration::from_millis(100));
        assert!(start.elapsed() < std::time::Duration::from_secs(4));
        assert_eq!(got.map(|g| g.2), Some(false));
        let mut ok = std::process::Command::new("echo");
        ok.arg("hi");
        let got = run_with_timeout(ok, std::time::Duration::from_secs(10));
        assert_eq!(got, Some(("hi\n".to_string(), String::new(), true)));
    }

    #[test]
    fn test_p4_47_compact_writes_folds_folders_and_counts_home_writes() {
        let root = Path::new("/r");
        let home = Path::new("/h");
        let row = |path: &str, global: bool| PlanWrite {
            id: "x",
            path: PathBuf::from(path),
            global,
            what: String::new(),
        };
        let claude = [
            row("/r/.claude/settings.json", false),
            row("/r/.claude/skills/sieve/SKILL.md", false),
            row("/r/.mcp.json", false),
            row("/h/.claude/settings.json", true),
            row("/h/.claude.json", true),
        ];
        let rows: Vec<&PlanWrite> = claude.iter().collect();
        assert_eq!(
            compact_writes(&rows, root, home),
            ".claude/, .mcp.json \u{b7} + 2 in ~/"
        );
        let one = [row("/r/AGENTS.md", false)];
        let rows: Vec<&PlanWrite> = one.iter().collect();
        assert_eq!(compact_writes(&rows, root, home), "AGENTS.md");
        let many = [
            row("/r/.cursor/a", false),
            row("/r/.cursor/b", false),
            row("/r/.cursor/c", false),
        ];
        let rows: Vec<&PlanWrite> = many.iter().collect();
        assert_eq!(
            compact_writes(&rows, root, home),
            ".cursor/a, .cursor/b +1 more"
        );
    }
}
