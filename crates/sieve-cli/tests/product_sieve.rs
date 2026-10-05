//! Pins the `sieve` branding path (rename-cli task): every runtime-visible
//! product string in `sieve-cli` must route through
//! `sieve_core::product()`, so a run never prints, or
//! writes, the old product name.
//!
//! Every test here spawns the binary with a scratch `HOME` (never the process's
//! real one), over a fresh copy of `tests/fixtures/basic` with no `sieve/`
//! directory — the fixture ships with none, so no cleanup step is needed before
//! the copy.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use support::TempDir;

fn sieve_bin() -> &'static str {
    env!("CARGO_BIN_EXE_sieve")
}

/// Copies `src` into `dst`, recursively, including dot-directories.
fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("create dst dir");
    for entry in fs::read_dir(src).expect("read src dir") {
        let entry = entry.expect("read dir entry");
        let file_type = entry.file_type().expect("read file type");
        let target = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy file");
        }
    }
}

/// Runs `sieve <args>` in `cwd`, branded `SIEVE_PRODUCT=sieve`, with
/// `home` as `$HOME` — never the real one.
fn run_sieve(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(sieve_bin())
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("CLAUDE_PROJECT_DIR", cwd)
        .output()
        .expect("run sieve")
}

/// Lists every file under `dir`, as paths relative to `dir`.
fn list_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out
}

fn walk(base: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let entry = entry.expect("read dir entry");
        let path = entry.path();
        if entry.file_type().expect("file type").is_dir() {
            walk(base, &path, out);
        } else {
            out.push(path.strip_prefix(base).expect("strip prefix").to_path_buf());
        }
    }
}

#[test]
fn rename_table_cli_help_shows_sieve_and_no_old_name() {
    let home = TempDir::new("product-sieve-help-home");
    let cwd = TempDir::new("product-sieve-help-cwd");

    let output = run_sieve(&cwd.path, &home.path, &["help"]);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.starts_with("Usage: sieve"),
        "stdout must open with the sieve usage line, got: {stdout}"
    );
}

#[test]
fn rename_table_cli_build_writes_sieve_dir() {
    let fixture = support::manifest_dir().join("../../tests/fixtures/basic");
    let home = TempDir::new("product-sieve-build-home");
    let copy = TempDir::new("product-sieve-build-copy");
    copy_dir(&fixture, &copy.path);

    run_sieve(&copy.path, &home.path, &["build", "."]);

    assert!(
        copy.path
            .join("sieve")
            .join(".graph")
            .join("wiring.json")
            .is_file(),
        "sieve build must write sieve/.graph/wiring.json under a sieve-branded run"
    );
}

#[test]
fn rename_table_cli_grep_savings_line_names_sieve() {
    let fixture = support::manifest_dir().join("../../tests/fixtures/basic");
    let home = TempDir::new("product-sieve-grep-home");
    let copy = TempDir::new("product-sieve-grep-copy");
    copy_dir(&fixture, &copy.path);

    let build_output = run_sieve(&copy.path, &home.path, &["build", "."]);
    assert!(
        build_output.status.success(),
        "sieve build must exit 0, stderr={}",
        String::from_utf8_lossy(&build_output.stderr)
    );

    let output = run_sieve(&copy.path, &home.path, &["grep", "run"]);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[sieve] saved \u{2248} "),
        "grep stdout must open its savings line with [sieve], got: {stdout}"
    );
}

#[test]
fn rename_table_cli_ask_source_savings_line_names_sieve() {
    let fixture = support::manifest_dir().join("../../tests/fixtures/basic");
    let home = TempDir::new("product-sieve-ask-home");
    let copy = TempDir::new("product-sieve-ask-copy");
    copy_dir(&fixture, &copy.path);

    let build_output = run_sieve(&copy.path, &home.path, &["build", "."]);
    assert!(
        build_output.status.success(),
        "sieve build must exit 0, stderr={}",
        String::from_utf8_lossy(&build_output.stderr)
    );

    let output = run_sieve(&copy.path, &home.path, &["ask", "import", "--source"]);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[sieve] saved \u{2248} "),
        "ask --source stdout must open its savings line with [sieve], got: {stdout}"
    );
    assert!(
        !stdout.contains("At the end of your reply"),
        "ask --source stdout must hold no turn nudge, got: {stdout}"
    );
}

#[test]
fn rename_table_cli_version_shows_sieve_and_no_old_name() {
    let home = TempDir::new("product-sieve-version-home");
    let cwd = TempDir::new("product-sieve-version-cwd");

    let output = run_sieve(&cwd.path, &home.path, &["version"]);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("sieve"),
        "version stdout must name sieve, got: {stdout}"
    );
}

#[test]
fn rename_table_cli_check_no_graph_reports_sieve_and_no_old_name() {
    let home = TempDir::new("product-sieve-check-home");
    let copy = TempDir::new("product-sieve-check-copy");
    let fixture = support::manifest_dir().join("../../tests/fixtures/basic");
    copy_dir(&fixture, &copy.path);

    let output = run_sieve(&copy.path, &home.path, &["check", "."]);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("sieve: no index here yet \u{2014} run sieve build ."),
        "check must name the sieve build command, got: {stderr}"
    );
}

#[test]
fn rename_table_cli_init_cursor_and_kiro_write_sieve_paths_and_no_old_name() {
    let fixture = support::manifest_dir().join("../../tests/fixtures/basic");
    let home = TempDir::new("product-sieve-init-hosts-home");
    let copy = TempDir::new("product-sieve-init-hosts-copy");
    copy_dir(&fixture, &copy.path);

    let output = run_sieve(
        &copy.path,
        &home.path,
        &["init", "--agents", "cursor", "kiro", "--no-build"],
    );
    assert!(
        output.status.success(),
        "sieve init must exit 0, stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        copy.path
            .join(".cursor")
            .join("rules")
            .join("sieve.mdc")
            .is_file(),
        "init must write .cursor/rules/sieve.mdc for a sieve-branded run"
    );
    assert!(
        copy.path
            .join(".kiro")
            .join("steering")
            .join("sieve.md")
            .is_file(),
        "init must write .kiro/steering/sieve.md for a sieve-branded run"
    );
}

#[test]
fn rename_table_cli_hook_tool_savings_sums_sieve_marker() {
    let fixture = support::manifest_dir().join("../../tests/fixtures/basic");
    let home = TempDir::new("product-sieve-hook-home");
    let copy = TempDir::new("product-sieve-hook-copy");
    copy_dir(&fixture, &copy.path);

    let stdin = format!(
        "{{\"session_id\":\"sess1\",\"cwd\":\"{}\",\"tool_name\":\"Bash\",\
         \"tool_response\":{{\"stdout\":\"[sieve] saved \u{2248} 100 tokens\"}}}}",
        copy.path.display()
    );
    Command::new(sieve_bin())
        .args(["hook", "tool-savings"])
        .current_dir(&copy.path)
        .env("HOME", &home.path)
        .env("CLAUDE_PROJECT_DIR", &copy.path)
        .env("SIEVE_TEST_STDIN", &stdin)
        .output()
        .expect("run sieve hook tool-savings");

    let session_path = copy
        .path
        .join("sieve")
        .join(".cache")
        .join("session")
        .join("sess1.json");
    let session: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&session_path).expect("read session file"))
            .expect("parse session file");
    assert_eq!(
        session["savedTokens"], 100,
        "the sieve marker must sum into savedTokens, got: {session}"
    );
}

#[test]
fn rename_table_cli_init_writes_sieve_branded_claude_files_and_no_old_name() {
    let fixture = support::manifest_dir().join("../../tests/fixtures/basic");
    let home = TempDir::new("product-sieve-init-home");
    let copy = TempDir::new("product-sieve-init-copy");
    copy_dir(&fixture, &copy.path);

    // The Claude branch runs only when the selection holds `claude`.
    let output = run_sieve(
        &copy.path,
        &home.path,
        &["init", "--agents", "agents", "claude", "--no-build"],
    );
    assert!(
        output.status.success(),
        "sieve init must exit 0, stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let claude_dir = copy.path.join(".claude");
    assert!(
        !claude_dir.join("helpers").exists(),
        "init must write nothing under .claude/helpers/ (ledger 2026-09-12)"
    );
    let settings: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(claude_dir.join("settings.json")).expect("read settings.json"),
    )
    .expect("parse settings.json");
    for (event, sub) in [
        ("SessionStart", "session-start"),
        ("UserPromptSubmit", "prompt"),
        ("PostToolUse", "post-edit"),
        ("Stop", "stop"),
    ] {
        let hooks = settings["hooks"][event].to_string();
        assert!(
            hooks.contains(&format!("\"command\":\"sieve hook {sub}\"")),
            "{event} must run `sieve hook {sub}`, got: {hooks}"
        );
    }
    assert_eq!(
        settings["statusLine"]["command"], "sieve statusline",
        "the statusline command must be `sieve statusline`"
    );
    assert!(
        claude_dir
            .join("skills")
            .join("sieve")
            .join("SKILL.md")
            .is_file(),
        "init must write .claude/skills/sieve/SKILL.md for a sieve-branded run"
    );

    let mcp_json: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(copy.path.join(".mcp.json")).expect("read .mcp.json"),
    )
    .expect("parse .mcp.json");
    assert!(
        mcp_json["mcpServers"]["sieve"].is_object(),
        ".mcp.json must key the server entry \"sieve\", got: {mcp_json}"
    );
}

#[test]
fn rename_table_cli_default_is_sieve_without_the_switch() {
    let home = TempDir::new("product-sieve-default-home");
    let cwd = TempDir::new("product-sieve-default-cwd");

    // The binary has one name set. This pins that `help` names `sieve`.
    let output = Command::new(sieve_bin())
        .args(["help"])
        .current_dir(&cwd.path)
        .env("HOME", &home.path)
        .env("CLAUDE_PROJECT_DIR", &cwd.path)
        .output()
        .expect("run sieve");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.starts_with("Usage: sieve"),
        "stdout must open with the sieve usage line with no switch set, got: {stdout}"
    );
}

/// P4-47: the `--dry-run` plan of a sieve-branded run lists the files the
/// run writes, lists no shim row, names no old name and writes nothing.
#[test]
fn test_p4_47_init_dry_run_plan_names_sieve_and_lists_no_shims() {
    let fixture = support::manifest_dir().join("../../tests/fixtures/basic");
    let home = TempDir::new("product-sieve-plan-home");
    let copy = TempDir::new("product-sieve-plan-copy");
    copy_dir(&fixture, &copy.path);
    let before = list_files(&copy.path);

    let output = run_sieve(
        &copy.path,
        &home.path,
        &["init", "--dry-run", "--agents", "claude", "cursor"],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr={stderr}");
    for row in [
        ".claude/skills/sieve/SKILL.md  tells the agent when to use sieve",
        ".cursor/rules/sieve.mdc  tells the agent when to use sieve",
    ] {
        assert!(stderr.contains(row), "missing row {row:?} in: {stderr}");
    }
    assert!(!stderr.contains(".cjs"), "plan lists a shim: {stderr}");
    assert!(
        !stderr.contains("on your machine"),
        "plan has HOME rows: {stderr}"
    );
    assert!(stderr.ends_with("dry run \u{b7} nothing written \u{b7} nothing outside this repo\n"));
    assert_eq!(list_files(&copy.path), before, "dry run wrote a file");
    assert!(
        list_files(&home.path).is_empty(),
        "dry run wrote under HOME"
    );

    let output = run_sieve(
        &copy.path,
        &home.path,
        &["init", "--dry-run", "--global", "--agents", "claude"],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("~/.claude.json           the sieve MCP server"),
        "missing global row in: {stderr}"
    );
}

/// The paths a plan lists, as absolute paths: repo rows under `repo`, and
/// `~/` rows under `home`.
fn plan_paths(stderr: &str, repo: &Path, home: &Path) -> std::collections::BTreeSet<PathBuf> {
    let mut paths = std::collections::BTreeSet::new();
    let mut global = false;
    for line in stderr.lines() {
        if line.starts_with("sieve would ") {
            global = line.contains("on your machine");
        } else if let Some(row) = line.strip_prefix("  ") {
            let path = row.split("  ").next().expect("row path").trim();
            paths.insert(match path.strip_prefix("~/") {
                Some(rest) if global => home.join(rest),
                _ => repo.join(path),
            });
        }
    }
    paths
}

/// P4-47: for each product, with and without `--global`, `--no-mcp` and
/// `--no-hooks`, the files the real `init` writes equal the plan's rows.
/// The plan omits `.gitignore`, `.ignore` and the index dir. The
/// scratch HOME holds `.codex` and `.config/opencode`, so the Codex hook,
/// Codex config and opencode rows show (P4-51).
#[test]
fn test_p4_47_init_dry_run_plan_equals_the_files_a_real_run_writes() {
    let fixture = support::manifest_dir().join("../../tests/fixtures/basic");
    let agents = [
        "--agents",
        "claude",
        "agents",
        "adal",
        "cursor",
        "gemini",
        "grok",
        "hermes",
        "antigravity",
        "copilot",
        "kiro",
        "windsurf",
        "--no-build",
    ];
    let flag_sets: [&[&str]; 6] = [
        &[],
        &["--global"],
        &["--no-mcp"],
        &["--no-hooks"],
        &["--global", "--no-mcp"],
        &["--global", "--no-hooks"],
    ];
    for flags in flag_sets {
        let home = TempDir::new("plan-real-home");
        fs::create_dir_all(home.path.join(".codex")).expect("make .codex");
        fs::create_dir_all(home.path.join(".config/opencode")).expect("make opencode");
        let copy = TempDir::new("plan-real-copy");
        copy_dir(&fixture, &copy.path);
        let run = |dry: bool| {
            let mut cmd = Command::new(sieve_bin());
            cmd.arg("init")
                .args(flags)
                .args(agents)
                .current_dir(&copy.path)
                .env("HOME", &home.path)
                .env("CLAUDE_PROJECT_DIR", &copy.path);
            if dry {
                cmd.arg("--dry-run");
            }
            cmd.output().expect("run sieve init")
        };
        let plan = run(true);
        let stderr = String::from_utf8_lossy(&plan.stderr).to_string();
        assert!(plan.status.success(), "{flags:?}: {stderr}");
        let planned = plan_paths(&stderr, &copy.path, &home.path);
        let repo_before = list_files(&copy.path);
        let real = run(false);
        assert!(real.status.success(), "{flags:?}: {real:?}");
        let mut written = std::collections::BTreeSet::new();
        let skip = |p: &Path| {
            let s = p.to_string_lossy();
            s == ".gitignore" || s == ".ignore" || s.starts_with("sieve/")
        };
        for rel in list_files(&copy.path) {
            if !repo_before.contains(&rel) && !skip(&rel) {
                written.insert(copy.path.join(rel));
            }
        }
        for rel in list_files(&home.path) {
            written.insert(home.path.join(rel));
        }
        assert_eq!(written, planned, "{flags:?}: written vs plan");
    }
}
