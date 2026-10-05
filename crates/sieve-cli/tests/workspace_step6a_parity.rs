//! Parity tests for workspace step 6a: `init` at a workspace parent wires the
//! parent, then each child repo (P4-45). The expected text comes from a
//! recorded run in a scratch HOME, less the recorded deviations (no `.cjs`
//! file, no wiring stamp, no update nudge).

mod support;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use support::ws::{drop_update_nudge, mask};
use support::TempDir;

/// A parent with two child repos, `a` and `b`. Neither has a graph.
fn parent(label: &str) -> TempDir {
    let temp = TempDir::new(label);
    for (child, file, body) in [
        ("a", "x.ts", "export function foo() { return 1; }\n"),
        ("b", "y.ts", "export function bar() { return 2; }\n"),
    ] {
        fs::create_dir_all(temp.path.join(child).join(".git")).expect("make child repo");
        fs::write(temp.path.join(child).join(file), body).expect("write source");
    }
    temp
}

fn init(dir: &Path, home: &Path, args: &[&str]) -> Output {
    support::sieve_command()
        .arg("init")
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .output()
        .expect("run sieve init")
}

fn files(base: &Path) -> BTreeSet<PathBuf> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeSet<PathBuf>) {
        for entry in fs::read_dir(dir).expect("read dir").flatten() {
            let path = entry.path();
            if entry.file_type().expect("file type").is_dir() {
                walk(base, &path, out);
            } else {
                out.insert(path.strip_prefix(base).expect("under base").to_path_buf());
            }
        }
    }
    let mut out = BTreeSet::new();
    walk(base, base, &mut out);
    out
}

const WIRED: &str = "\
\u{b7} workspace: wiring <TMP> and 2 child repo(s) \u{2014} a, b
building 2 workspace repos: a, b
\u{2713} wrote <TMP>/.claude/settings.json
\u{2713} wrote <TMP>/.claude/skills/sieve/SKILL.md
\u{2713} mcp claude: <TMP>/.mcp.json (created) \u{2014} restart Claude Code to load the sieve MCP server
\u{2713} built the graph (sieve build)

\u{2014} a/
\u{2713} wrote <TMP>/a/.claude/settings.json
\u{2713} wrote <TMP>/a/.claude/skills/sieve/SKILL.md
\u{2713} mcp claude: <TMP>/a/.mcp.json (created) \u{2014} restart Claude Code to load the sieve MCP server
\u{b7} skipped graph build

\u{2014} b/
\u{2713} wrote <TMP>/b/.claude/settings.json
\u{2713} wrote <TMP>/b/.claude/skills/sieve/SKILL.md
\u{2713} mcp claude: <TMP>/b/.mcp.json (created) \u{2014} restart Claude Code to load the sieve MCP server
\u{b7} skipped graph build
";

/// P4-45: `--verbose` prints the per-file report. Sieve wires the parent first, then each child. The parent build
/// builds both child graphs, so each child then skips its own build. The
/// epilogue sums the child graphs (4 nodes, 2 edges).
#[test]
fn test_p4_45_init_verbose_at_a_parent_wires_the_parent_then_each_child() {
    let copy = parent("p4-45-wire");
    let home = TempDir::new("p4-45-wire-home");
    let out = init(&copy.path, &home.path, &["--agents", "claude", "--verbose"]);
    assert_eq!(out.status.code(), Some(0));
    let stderr = drop_update_nudge(&mask(&String::from_utf8_lossy(&out.stderr), &copy.path));
    // The skipped-global line is a recorded deviation (ledger 2026-09-16).
    let stderr = stderr.replace(
        "\u{b7} skipped global hooks under ~ (pass --global to write them)\n",
        "",
    );
    assert!(stderr.starts_with(WIRED), "{stderr}");
    assert!(stderr.contains("  |___/_|\\___| \\_/ \\___|  4 nodes \u{b7} 2 edges\n"));
}

/// P4-45, P4-47: the compact default at a parent. One graph line (the
/// parent build is the workspace build, so it has no node count), one line
/// for the agent and two epilogue lines. The recorded run prints the same text,
/// less the `+ 3 in ~/` tail (ledger 2026-09-16).
#[test]
fn test_p4_45_init_compact_at_a_parent_wires_the_parent_then_each_child() {
    let copy = parent("p4-45-compact");
    let home = TempDir::new("p4-45-compact-home");
    let out = init(&copy.path, &home.path, &["--agents", "claude"]);
    assert_eq!(out.status.code(), Some(0));
    let stderr = drop_update_nudge(&mask(&String::from_utf8_lossy(&out.stderr), &copy.path));
    let want = "\
\u{b7} workspace: wiring <TMP> and 2 child repo(s) \u{2014} a, b
\u{2713} graph built
\u{2713} claude   .claude/, .mcp.json
\u{b7} restart your agents so a new session picks up sieve
\u{b7} commit .claude/ .mcp.json to share it \u{2014} sieve/ stays local and git-ignored
";
    assert_eq!(stderr, want);
    for child in ["a", "b"] {
        assert!(copy
            .path
            .join(child)
            .join(".claude/settings.json")
            .is_file());
    }
}

/// P4-45: the files sieve writes with `init --agents claude --no-global`
/// at the parent: the parent's wiring, each child's wiring and graph, and
/// `sieve/workspace.json` at the parent only.
#[test]
fn test_p4_45_init_at_a_parent_writes_sieve_file_list() {
    let copy = parent("p4-45-files");
    let home = TempDir::new("p4-45-files-home");
    let out = init(&copy.path, &home.path, &["--agents", "claude"]);
    assert_eq!(out.status.code(), Some(0));
    let got: Vec<String> = files(&copy.path)
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .filter(|p| !p.contains(".cache/"))
        .collect();
    let want = [
        ".claude/settings.json",
        ".claude/skills/sieve/SKILL.md",
        ".gitignore",
        ".mcp.json",
        "a/.claude/settings.json",
        "a/.claude/skills/sieve/SKILL.md",
        "a/.gitignore",
        "a/.ignore",
        "a/.mcp.json",
        "a/sieve/.graph/wiring.json",
        "a/sieve/INDEX.md",
        "a/sieve/x.md",
        "a/x.ts",
        "b/.claude/settings.json",
        "b/.claude/skills/sieve/SKILL.md",
        "b/.gitignore",
        "b/.ignore",
        "b/.mcp.json",
        "b/sieve/.graph/wiring.json",
        "b/sieve/INDEX.md",
        "b/sieve/y.md",
        "b/y.ts",
        "sieve/workspace.json",
    ];
    assert_eq!(got, want);
    assert!(
        home.path.join(".claude").metadata().is_err(),
        "no HOME write"
    );
}

/// P4-45: the real run writes the files the `--dry-run` plan lists, at the
/// parent and under each child (`.gitignore`, `.ignore` and the index dir
/// are not in the plan).
#[test]
fn test_p4_45_init_dry_run_plan_equals_the_files_a_real_run_writes_at_a_parent() {
    let copy = parent("p4-45-plan");
    let home = TempDir::new("p4-45-plan-home");
    let args = ["--all-agents", "--no-build"];
    let plan = init(
        &copy.path,
        &home.path,
        &[&args[..], &["--dry-run"]].concat(),
    );
    assert_eq!(plan.status.code(), Some(0));
    let text = mask(&String::from_utf8_lossy(&plan.stderr), &copy.path);
    let mut planned = BTreeSet::new();
    let mut prefix = String::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("\u{2014} ") {
            prefix = rest.split('/').next().expect("child name").to_string() + "/";
        } else if let Some(row) = line.strip_prefix("  ") {
            let path = row.split("  ").next().expect("row path").trim();
            planned.insert(PathBuf::from(format!("{prefix}{path}")));
        }
    }
    assert!(planned.contains(Path::new("a/.mcp.json")), "{text}");
    let before = files(&copy.path);
    let real = init(&copy.path, &home.path, &args);
    assert_eq!(real.status.code(), Some(0));
    let written: BTreeSet<PathBuf> = files(&copy.path)
        .difference(&before)
        .filter(|p| {
            let s = p.to_string_lossy();
            !(s.ends_with(".gitignore")
                || s.ends_with(".ignore")
                || ["sieve/", "a/sieve/", "b/sieve/"]
                    .iter()
                    .any(|d| s.starts_with(d)))
        })
        .cloned()
        .collect();
    assert_eq!(written, planned);
}

/// P4-45: a second `init` at the parent builds nothing, because the parent
/// holds `sieve/workspace.json`.
#[test]
fn test_p4_45_second_init_at_a_parent_skips_the_build() {
    let copy = parent("p4-45-twice");
    let home = TempDir::new("p4-45-twice-home");
    assert_eq!(
        init(&copy.path, &home.path, &["--agents", "claude"])
            .status
            .code(),
        Some(0)
    );
    let again = init(&copy.path, &home.path, &["--agents", "claude"]);
    let stderr = String::from_utf8_lossy(&again.stderr);
    assert!(!stderr.contains("building 2 workspace repos"), "{stderr}");
    assert!(!stderr.contains("\u{2713} built the graph"), "{stderr}");
}
