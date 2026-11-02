//! `sieve init --yes` wires OpenCode when it finds a marker, and
//! `sieve uninstall` takes the plugin files out again.
//! Every test spawns the binary with a scratch `HOME`, never the real one.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use support::TempDir;

/// The owner's file shape: 2-space JSON with three keys beside `mcp`, and
/// another MCP server in it.
const USER_CONFIG: &str = r#"{
  "$schema": "https://opencode.ai/config.json",
  "theme": "tokyonight",
  "model": "anthropic/claude-sonnet",
  "mcp": {
    "other": {
      "type": "local",
      "command": [
        "other-server"
      ],
      "enabled": true
    }
  }
}
"#;

fn run(repo: &Path, home: &Path, cmd: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sieve"))
        .arg(cmd)
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("PATH", support::sieve_path())
        .output()
        .expect("run sieve")
}

fn scratch(label: &str) -> (TempDir, PathBuf, PathBuf) {
    let tmp = TempDir::new(label);
    let repo = tmp.join("repo");
    let home = tmp.join("home");
    fs::create_dir_all(&repo).expect("make repo");
    fs::create_dir_all(&home).expect("make home");
    fs::write(repo.join("a.rs"), "fn a() {}\n").expect("write source");
    (tmp, repo, home)
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn init_yes(repo: &Path, home: &Path) -> Output {
    let out = run(repo, home, "init", &["--yes", "--no-build"]);
    assert!(out.status.success(), "{}", stderr(&out));
    out
}

const PLUGIN: &str = ".opencode/plugins/sieve";

#[test]
fn test_init_yes_merges_into_a_marker_file_and_a_second_run_changes_nothing() {
    let (_tmp, repo, home) = scratch("oc-merge");
    fs::write(repo.join("opencode.json"), USER_CONFIG).expect("seed config");
    init_yes(&repo, &home);

    let merged = fs::read_to_string(repo.join("opencode.json")).expect("read config");
    let json: serde_json::Value = serde_json::from_str(&merged).expect("valid json");
    assert_eq!(json["theme"], "tokyonight");
    assert_eq!(json["model"], "anthropic/claude-sonnet");
    assert_eq!(json["$schema"], "https://opencode.ai/config.json");
    assert_eq!(json["mcp"]["other"]["command"][0], "other-server");
    assert_eq!(json["mcp"]["sieve"]["command"][0], "sieve");
    // The keys keep their order, and the 2-space form stays.
    let order = ["\"$schema\"", "\"theme\"", "\"model\"", "\"mcp\""];
    let at: Vec<usize> = order.iter().map(|k| merged.find(k).expect("key")).collect();
    assert!(at.windows(2).all(|w| w[0] < w[1]), "{merged}");
    assert!(merged.contains("\n  \"theme\""), "{merged}");

    for file in ["index.ts", "tui.tsx"] {
        assert!(repo.join(PLUGIN).join(file).is_file(), "no {file}");
    }
    let tui = fs::read_to_string(repo.join(PLUGIN).join("tui.tsx")).expect("read tui");
    assert!(tui.contains("[\"statusline\", \"--format\", \"plain\"]"));
    assert!(!tui.contains("__BIN__") && !tui.contains("__CONTEXT_DIR__"));
    let ignore = fs::read_to_string(repo.join(".gitignore")).expect("read gitignore");
    assert!(ignore.lines().any(|l| l == "/.opencode/plugins/sieve/"));

    let files = [
        repo.join("opencode.json"),
        repo.join(PLUGIN).join("index.ts"),
        repo.join(PLUGIN).join("tui.tsx"),
        repo.join(".gitignore"),
    ];
    let before: Vec<Vec<u8>> = files.iter().map(|f| fs::read(f).expect("read")).collect();
    init_yes(&repo, &home);
    for (file, bytes) in files.iter().zip(before) {
        assert_eq!(fs::read(file).expect("read"), bytes, "{}", file.display());
    }
}

#[test]
fn test_init_yes_writes_the_entry_when_only_the_home_folder_exists() {
    let (_tmp, repo, home) = scratch("oc-home");
    fs::create_dir_all(home.join(".config/opencode")).expect("home marker");
    init_yes(&repo, &home);
    let text = fs::read_to_string(repo.join("opencode.json")).expect("opencode.json");
    assert!(text.contains("\"sieve\""), "{text}");
    assert!(repo.join(PLUGIN).join("tui.tsx").is_file());
}

#[test]
fn test_init_yes_prints_a_hint_and_writes_nothing_for_a_commented_config() {
    for (name, text) in [
        ("opencode.json", "{\n  // my model\n  \"model\": \"x\"\n}\n"),
        ("opencode.jsonc", "{\n  \"model\": \"x\" // my model\n}\n"),
    ] {
        let (_tmp, repo, home) = scratch("oc-comment");
        fs::write(repo.join(name), text).expect("seed config");
        let out = init_yes(&repo, &home);
        assert_eq!(fs::read_to_string(repo.join(name)).expect("read"), text);
        if name == "opencode.jsonc" {
            assert!(!repo.join("opencode.json").exists(), "wrote a second file");
        }
        let hint = stderr(&out)
            .lines()
            .find(|l| l.contains("mcp opencode"))
            .expect("hint line")
            .to_string();
        assert!(
            hint.contains("\"sieve\": {\"type\": \"local\", \"command\": [\"sieve\", \"mcp\"]"),
            "{hint}"
        );
    }
}

#[test]
fn test_uninstall_removes_the_plugin_files_and_its_ignore_block() {
    let (_tmp, repo, home) = scratch("oc-uninstall");
    fs::create_dir_all(repo.join(".opencode")).expect("repo marker");
    fs::write(repo.join(".gitignore"), "target/\n").expect("seed ignore");
    init_yes(&repo, &home);
    assert!(repo.join(PLUGIN).join("index.ts").is_file());

    let out = run(&repo, &home, "uninstall", &["--yes"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!repo.join(PLUGIN).exists(), "plugin folder is still there");
    let ignore = fs::read_to_string(repo.join(".gitignore")).expect("read gitignore");
    assert_eq!(ignore, "target/\n");
}

#[test]
fn test_an_edited_plugin_file_survives_init_and_uninstall() {
    let (_tmp, repo, home) = scratch("oc-edited");
    fs::create_dir_all(repo.join(PLUGIN)).expect("make plugin dir");
    let mine = "// my own footer\nexport default {}\n";
    let tui = repo.join(PLUGIN).join("tui.tsx");
    fs::write(&tui, mine).expect("seed tui");

    let out = init_yes(&repo, &home);
    assert_eq!(fs::read_to_string(&tui).expect("read"), mine);
    assert!(
        stderr(&out).contains("tui.tsx left unchanged (edited)"),
        "{}",
        stderr(&out)
    );
    // The other file has the marker, so init wrote it.
    let index = fs::read_to_string(repo.join(PLUGIN).join("index.ts")).expect("index.ts");
    assert!(index.starts_with("// sieve: generated by sieve init; sieve owns this file\n"));

    let out = run(&repo, &home, "uninstall", &["--yes"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(fs::read_to_string(&tui).expect("still there"), mine);
    assert!(!repo.join(PLUGIN).join("index.ts").exists());
}
