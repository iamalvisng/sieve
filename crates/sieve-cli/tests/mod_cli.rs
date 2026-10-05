//! `sieve init --mod` installs the sieve-pane Claude Code mod into the
//! project, and `sieve uninstall` removes exactly what it wrote.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use support::TempDir;

const MOD_FILES: [&str; 5] = [
    ".claude/skills/sieve-pane/.claude-plugin/plugin.json",
    ".claude/skills/sieve-pane/hooks/hooks.json",
    ".claude/skills/sieve-pane/hooks/register.tsx",
    ".claude/skills/sieve-pane/hooks/mascots.ts",
    ".claude/skills/sieve-pane/types/index.d.ts",
];

/// Runs the binary with a scratch HOME.
fn run(repo: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sieve"))
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("PATH", support::sieve_path())
        .env("CLAUDE_PROJECT_DIR", repo)
        .output()
        .expect("run sieve")
}

fn init(repo: &Path, home: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["init", "--no-build", "-y", "--agents", "claude"];
    args.extend_from_slice(extra);
    run(repo, home, &args)
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for entry in fs::read_dir(&d).expect("read dir").flatten() {
            let p = entry.path();
            if p.is_dir() {
                todo.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// The scratch dirs: a project and a HOME that is not the real one.
fn scratch(label: &str) -> (TempDir, PathBuf) {
    let temp = TempDir::new(label);
    let repo = temp.path.join("repo");
    let home = temp.path.join("home");
    fs::create_dir_all(&repo).expect("repo");
    fs::create_dir_all(&home).expect("home");
    (temp, home)
}

fn repo_of(temp: &TempDir) -> PathBuf {
    temp.path.join("repo")
}

fn settings(repo: &Path) -> serde_json::Value {
    let text = fs::read_to_string(repo.join(".claude/settings.json")).expect("settings");
    serde_json::from_str(&text).expect("settings json")
}

#[test]
fn test_mod_init_writes_the_five_files_and_the_settings_key() {
    let (temp, home) = scratch("mod-writes");
    let repo = repo_of(&temp);
    let out = init(&repo, &home, &["--mod"]);
    assert!(out.status.success(), "{}", stderr(&out));
    for rel in MOD_FILES {
        let written = fs::read_to_string(repo.join(rel)).expect(rel);
        // The crate dir comes from a run-time read, so the cwd does not matter.
        let source = support::manifest_dir()
            .join("assets/claude-mod/sieve-pane")
            .join(rel.trim_start_matches(".claude/skills/sieve-pane/"));
        assert_eq!(
            written,
            fs::read_to_string(&source).expect("source"),
            "{rel}"
        );
    }
    assert!(!repo.join(".claude/skills/sieve-pane/README.md").exists());
    assert!(!repo.join(".claude/skills/sieve-pane/tests").exists());
    assert_eq!(
        settings(&repo)["enabledPlugins"]["sieve-pane@skills-dir"],
        serde_json::Value::Bool(true)
    );
    assert!(stderr(&out).contains("mod sieve-pane"), "{}", stderr(&out));
}

#[test]
fn test_mod_settings_merge_keeps_other_keys() {
    let (temp, home) = scratch("mod-merge");
    let repo = repo_of(&temp);
    fs::create_dir_all(repo.join(".claude")).expect("claude dir");
    fs::write(
        repo.join(".claude/settings.json"),
        r#"{"model":"opus","enabledPlugins":{"other@skills-dir":false}}"#,
    )
    .expect("seed settings");
    let out = init(&repo, &home, &["--mod"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let value = settings(&repo);
    assert_eq!(value["model"], "opus");
    assert_eq!(value["enabledPlugins"]["other@skills-dir"], false);
    assert_eq!(value["enabledPlugins"]["sieve-pane@skills-dir"], true);
    assert!(value["hooks"].is_object());
}

#[test]
fn test_mod_second_init_reports_unchanged_and_a_changed_file_updated() {
    let (temp, home) = scratch("mod-twice");
    let repo = repo_of(&temp);
    init(&repo, &home, &["--mod"]);
    let again = stderr(&init(&repo, &home, &["--mod"]));
    let lines: Vec<&str> = again
        .lines()
        .filter(|l| l.contains("mod sieve-pane"))
        .collect();
    assert_eq!(lines.len(), 6, "{again}");
    assert!(lines.iter().all(|l| l.ends_with("(unchanged)")), "{again}");

    let plugin = repo.join(MOD_FILES[0]);
    fs::write(&plugin, "changed").expect("edit");
    let third = stderr(&init(&repo, &home, &["--mod"]));
    let updated: Vec<&str> = third.lines().filter(|l| l.ends_with("(updated)")).collect();
    assert_eq!(updated.len(), 1, "{third}");
    assert!(updated[0].contains("plugin.json"), "{third}");
    assert_ne!(fs::read_to_string(&plugin).expect("plugin"), "changed");
}

#[test]
fn test_mod_uninstall_removes_exactly_these() {
    let (temp, home) = scratch("mod-uninstall");
    let repo = repo_of(&temp);
    fs::create_dir_all(repo.join(".claude")).expect("claude dir");
    fs::write(
        repo.join(".claude/settings.json"),
        r#"{"model":"opus","enabledPlugins":{"other@skills-dir":true}}"#,
    )
    .expect("seed settings");
    // A file the user added inside the mod dir must stay.
    fs::create_dir_all(repo.join(".claude/skills/sieve-pane")).expect("mod dir");
    fs::write(repo.join(".claude/skills/sieve-pane/mine.txt"), "mine").expect("user file");
    init(&repo, &home, &["--mod"]);
    let out = run(&repo, &home, &["uninstall", "-y", "--keep-cache"]);
    assert!(out.status.success(), "{}", stderr(&out));
    for rel in MOD_FILES {
        assert!(!repo.join(rel).exists(), "{rel} stays");
    }
    assert!(repo.join(".claude/skills/sieve-pane/mine.txt").is_file());
    let value = settings(&repo);
    assert_eq!(value["model"], "opus");
    assert_eq!(value["enabledPlugins"]["other@skills-dir"], true);
    assert!(value["enabledPlugins"]
        .get("sieve-pane@skills-dir")
        .is_none());
}

#[test]
fn test_mod_uninstall_drops_the_empty_plugin_bucket_and_the_dir() {
    let (temp, home) = scratch("mod-uninstall-clean");
    let repo = repo_of(&temp);
    init(&repo, &home, &["--mod"]);
    run(&repo, &home, &["uninstall", "-y", "--keep-cache"]);
    assert!(!repo.join(".claude/skills/sieve-pane").exists());
    assert!(!repo.join(".claude/settings.json").exists());
}

#[test]
fn test_mod_without_the_flag_writes_none_of_them() {
    let (temp, home) = scratch("mod-off");
    let repo = repo_of(&temp);
    let out = init(&repo, &home, &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!repo.join(".claude/skills/sieve-pane").exists());
    assert!(settings(&repo).get("enabledPlugins").is_none());
}

#[test]
fn test_mod_help_lists_the_flag() {
    let (temp, home) = scratch("mod-help");
    let repo = repo_of(&temp);
    let sieve = run(&repo, &home, &["init", "--help"]);
    assert!(String::from_utf8_lossy(&sieve.stdout).contains("  --mod  "));
}

#[test]
fn test_mod_init_writes_nothing_under_home() {
    let (temp, home) = scratch("mod-home");
    let repo = repo_of(&temp);
    let out = init(&repo, &home, &["--mod"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(files_under(&home).is_empty(), "{:?}", files_under(&home));
}

/// Seeds the project settings file with `text`.
fn seed_settings(repo: &Path, text: &str) {
    fs::create_dir_all(repo.join(".claude")).expect("claude dir");
    fs::write(repo.join(".claude/settings.json"), text).expect("seed settings");
}

#[test]
fn test_mod_uninstall_leaves_a_false_key() {
    let (temp, home) = scratch("mod-uninstall-false");
    let repo = repo_of(&temp);
    seed_settings(
        &repo,
        r#"{"enabledPlugins":{"sieve-pane@skills-dir":false}}"#,
    );
    run(&repo, &home, &["uninstall", "-y", "--keep-cache"]);
    assert_eq!(
        settings(&repo)["enabledPlugins"]["sieve-pane@skills-dir"],
        false
    );
}

#[test]
fn test_mod_init_leaves_a_false_key_writes_files_and_says_disabled() {
    let (temp, home) = scratch("mod-init-false");
    let repo = repo_of(&temp);
    seed_settings(
        &repo,
        r#"{"enabledPlugins":{"sieve-pane@skills-dir":false}}"#,
    );
    let out = init(&repo, &home, &["--mod"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        settings(&repo)["enabledPlugins"]["sieve-pane@skills-dir"],
        false
    );
    for rel in MOD_FILES {
        assert!(repo.join(rel).is_file(), "{rel}");
    }
    let lines: Vec<String> = stderr(&out)
        .lines()
        .filter(|l| l.contains("disabled in the project settings"))
        .map(String::from)
        .collect();
    assert_eq!(lines.len(), 1, "{}", stderr(&out));
}

#[test]
fn test_mod_init_skips_a_non_object_plugins_value_with_a_warning() {
    let (temp, home) = scratch("mod-init-bad-bucket");
    let repo = repo_of(&temp);
    seed_settings(&repo, r#"{"enabledPlugins":"nope"}"#);
    let out = init(&repo, &home, &["--mod"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(settings(&repo)["enabledPlugins"], "nope");
    assert!(
        stderr(&out).contains("\u{26a0} mod sieve-pane"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn test_mod_dry_run_lists_the_plugin_key() {
    let (temp, home) = scratch("mod-dry-run");
    let repo = repo_of(&temp);
    let out = init(&repo, &home, &["--mod", "--dry-run"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("hooks, the status line and the pane mod"),
        "{}",
        stderr(&out)
    );
    assert!(files_under(&repo).is_empty());
}

/// Every relative import of `register.tsx` must be an installed file.
#[test]
fn test_mod_every_relative_import_of_register_is_installed() {
    let mod_src = support::manifest_dir().join("assets/claude-mod/sieve-pane");
    let text = fs::read_to_string(mod_src.join("hooks/register.tsx")).expect("register.tsx");
    let prefix = ".claude/skills/sieve-pane/";
    let mut count = 0;
    for line in text.lines() {
        let line = line.trim();
        if !line.starts_with("import ") && !line.starts_with("export ") {
            continue;
        }
        let Some(spec) = line.split(&['\'', '"'][..]).nth(1) else {
            continue;
        };
        if !spec.starts_with("./") && !spec.starts_with("../") {
            continue;
        }
        count += 1;
        let base = mod_src.join("hooks").join(spec);
        let found = ["ts", "tsx", "d.ts"]
            .iter()
            .map(|ext| PathBuf::from(format!("{}.{ext}", base.display())))
            .chain([base.join("index.d.ts")])
            .find(|c| c.is_file())
            .unwrap_or_else(|| panic!("import {spec} resolves to no file"));
        let canon = found.canonicalize().expect("canonical");
        let rel = canon
            .strip_prefix(mod_src.canonicalize().expect("mod dir"))
            .expect("inside the mod dir");
        let installed = format!("{prefix}{}", rel.display());
        assert!(
            MOD_FILES.contains(&installed.as_str()),
            "import {spec} -> {installed} is not installed"
        );
    }
    assert!(count > 0, "no relative import found");
}

#[test]
fn test_mod_cli_status_line_and_pane_sprites_are_equal() {
    let root = support::manifest_dir();
    let read = |rel: &str| -> serde_json::Value {
        let text = fs::read_to_string(root.join(rel)).expect(rel);
        serde_json::from_str(&text).expect(rel)
    };
    assert_eq!(
        read("assets/mascots.json"),
        read("assets/claude-mod/sieve-pane/assets/mascots.json"),
        "the status line and the pane must share one sprite file"
    );
}
