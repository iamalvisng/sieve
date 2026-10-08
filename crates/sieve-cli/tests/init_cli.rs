//! Tests for `sieve init` and `sieve uninstall` (P4-01 to P4-12, P4-34):
//! every file `sieve init --all-agents` writes in the `basic` fixture,
//! against the golden tree under `tests/fixtures/basic.expected/init/`.
//! `SIEVE_BLESS=1` writes the goldens.
//!
//! Sieve writes no `.cjs` shim (ledger 2026-09-12: "Sieve writes no .cjs
//! shim"). Its hook commands read `sieve hook <sub>`.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use support::TempDir;

/// Restores a path's permissions on drop, even if the caller panics
/// before it can restore them itself.
struct PermGuard {
    path: PathBuf,
    mode: std::fs::Permissions,
}

impl Drop for PermGuard {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.path, self.mode.clone());
    }
}

/// `true` when the current process runs as root. Root ignores file
/// permission bits, so a test that relies on a chmod'd file to fail a
/// read cannot run under root.
fn is_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
        .unwrap_or(false)
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

/// Lists every file under `dir`, as paths relative to `dir`.
fn list_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    // `read_dir` order differs by file system, so sort for a stable result.
    out.sort();
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

/// Masks one absolute temp path in `text`, matching the golden capture's
/// own `<TMP>` mask.
fn mask(text: &str, path: &Path) -> String {
    text.replace(&path.display().to_string(), "<TMP>")
}

/// Compares one golden subtree (`golden_root`, already filtered to the
/// files this test expects) against `produced_root`. Panics with every
/// mismatch, not just the first, so a failing run shows the whole
/// picture at once.
fn compare_tree(golden_root: &Path, produced_root: &Path, rel_files: &[PathBuf]) {
    let mut failures = Vec::new();
    for rel in rel_files {
        let produced_path = produced_root.join(rel);
        let expected = fs::read(golden_root.join(rel)).expect("read golden file");
        match fs::read(&produced_path) {
            Ok(actual) if actual == expected => {}
            Ok(actual) => {
                failures.push(format!(
                    "{}: byte mismatch\n  first diff: {}",
                    rel.display(),
                    first_diff(&expected, &actual)
                ));
            }
            Err(e) => {
                failures.push(format!("{}: missing ({e})", rel.display()));
            }
        }
    }
    if !failures.is_empty() {
        panic!("init tree mismatch:\n{}", failures.join("\n"));
    }
}

/// Describes the first byte offset at which `a` and `b` differ, with a
/// short context window on each side.
fn first_diff(a: &[u8], b: &[u8]) -> String {
    let n = a.len().min(b.len());
    for i in 0..n {
        if a[i] != b[i] {
            let start = i.saturating_sub(20);
            let a_ctx = String::from_utf8_lossy(&a[start..(i + 20).min(a.len())]);
            let b_ctx = String::from_utf8_lossy(&b[start..(i + 20).min(b.len())]);
            return format!("at byte {i}: expected ...{a_ctx:?}... got ...{b_ctx:?}...");
        }
    }
    format!(
        "lengths differ: expected {} bytes, got {} bytes",
        a.len(),
        b.len()
    )
}

#[test]
fn test_p4_01_to_12_p4_34_init_matches_golden() {
    let fixture = support::manifest_dir().join("../../tests/fixtures/basic");
    let golden_root = support::manifest_dir().join("../../tests/fixtures/basic.expected/init");

    let copy = TempDir::new("copy");
    let home = TempDir::new("home");
    copy_dir(&fixture, &copy.path);
    // `sieve` resolves `std::env::current_dir()`, which canonicalizes a
    // macOS `/var/...` temp path to `/private/var/...`; canonicalize
    // here too, so the mask below matches what `sieve` actually reports.
    let copy_path = fs::canonicalize(&copy.path).expect("canonicalize copy path");
    let home_path = fs::canonicalize(&home.path).expect("canonicalize home path");

    let build_output = support::sieve_command()
        .args(["build", copy.path.to_str().expect("utf-8 path")])
        .output()
        .expect("run sieve build");
    assert_eq!(
        build_output.status.code(),
        Some(0),
        "sieve build failed to set up the fixture"
    );

    let tree_before = snapshot_tree(&copy.path);
    let init_output = support::sieve_command()
        .args(["init", "--all-agents", "--global"])
        .current_dir(&copy.path)
        .env("HOME", &home_path)
        .env("CLAUDE_PROJECT_DIR", &copy_path)
        .env("NO_COLOR", "1")
        .env("DO_NOT_TRACK", "1")
        .output()
        .expect("run sieve init");

    assert!(init_output.stdout.is_empty(), "sieve init wrote to stdout");
    assert_eq!(
        init_output.status.code(),
        Some(0),
        "sieve init exited non-zero"
    );

    let mut stderr = String::from_utf8(init_output.stderr).expect("utf-8 stderr");
    stderr = mask(&stderr, &copy_path);
    stderr = mask(&stderr, &home_path);
    let produced_lines: Vec<&str> = stderr.lines().filter(|l| !l.contains('⬆')).collect();

    if support::golden::blessing() {
        bless_init_tree(&golden_root, &copy.path, &tree_before, &home.path, &stderr);
    }
    let golden_stderr = fs::read_to_string(golden_root.join("init.stderr.txt"))
        .expect("read golden init.stderr.txt");
    let golden_lines: Vec<&str> = golden_stderr.lines().filter(|l| !l.contains('⬆')).collect();

    assert_eq!(produced_lines, golden_lines, "init stderr mismatch");

    let rel_files: Vec<PathBuf> = list_files(&golden_root)
        .into_iter()
        .filter(|p| {
            if p.components().count() == 1 {
                let name = p.to_str().unwrap_or("");
                if name.ends_with(".txt") {
                    return false;
                }
            }
            !p.starts_with("home")
        })
        .collect();
    compare_tree(&golden_root, &copy.path, &rel_files);

    let home_golden_root = golden_root.join("home");
    let home_rel_files = list_files(&home_golden_root);
    compare_tree(&home_golden_root, &home.path, &home_rel_files);

    // A second run must leave every file it already wrote untouched
    // (`init/second.stdout.txt` is empty by design), except the
    // regenerable `sieve/.cache/` stamp, excluded from the golden
    // capture for the same reason.
    let before = snapshot_tree(&copy.path);
    let second_output = support::sieve_command()
        .args(["init", "--all-agents", "--global"])
        .current_dir(&copy.path)
        .env("HOME", &home_path)
        .env("CLAUDE_PROJECT_DIR", &copy_path)
        .env("NO_COLOR", "1")
        .env("DO_NOT_TRACK", "1")
        .output()
        .expect("run sieve init a second time");
    assert!(
        second_output.stdout.is_empty(),
        "second sieve init wrote to stdout"
    );
    let second_golden = fs::read(golden_root.join("second.stdout.txt")).expect("read golden");
    assert!(
        second_golden.is_empty(),
        "golden second.stdout.txt is not empty"
    );
    let after = snapshot_tree(&copy.path);
    assert_eq!(
        before, after,
        "a second `sieve init --all-agents` changed the tree"
    );

    // Uninstall: compare stdout (empty) and the removed-file list,
    // computed the same way as the golden's
    // `uninstall.removed.txt` — a before/after `find -type f` diff —
    // rather than by reading a report string (uninstall's own stderr
    // report is not a golden-pinned file; the capture throws it away
    // too).
    let before_project = list_files(&copy.path);
    let before_home = list_files(&home.path);
    let uninstall_output = support::sieve_command()
        .args(["uninstall", "-y", "--global"])
        .current_dir(&copy.path)
        .env("HOME", &home_path)
        .env("CLAUDE_PROJECT_DIR", &copy_path)
        .env("NO_COLOR", "1")
        .env("DO_NOT_TRACK", "1")
        .output()
        .expect("run sieve uninstall");
    assert!(
        uninstall_output.stdout.is_empty(),
        "sieve uninstall wrote to stdout"
    );
    let uninstall_golden_stdout =
        fs::read(golden_root.join("uninstall.stdout.txt")).expect("read golden");
    assert!(
        uninstall_golden_stdout.is_empty(),
        "golden uninstall.stdout.txt is not empty"
    );

    let after_project = list_files(&copy.path);
    let after_home = list_files(&home.path);
    let mut removed: Vec<String> = before_project
        .iter()
        .filter(|p| !after_project.contains(p))
        .map(|p| format!("./{}", p.display()))
        .collect();
    removed.extend(
        before_home
            .iter()
            .filter(|p| !after_home.contains(p))
            .map(|p| format!("home/{}", p.display())),
    );
    // `sieve/.cache/*` file names carry the extractor stamp hash
    // (`sieve_parse::extractor_stamp()`), which is implementation- and
    // version-specific — not a parity-pinned value — and the golden's own
    // hook/statusline runs between `init` and `uninstall` (see the hook and
    // statusline goldens) add `sieve/.cache/session/*`, `stats.json`, and
    // `wiring-stamp.json` that this test never creates, since it never calls
    // `sieve hook` or `sieve statusline` in between. `sieve/` itself is
    // asserted gone as a whole below instead of file by file.
    removed.retain(|l| !l.starts_with("./sieve/.cache/"));
    removed.sort();

    let removed_text: String = removed.iter().map(|l| format!("{l}\n")).collect();
    support::golden::bless_text_if_blessing(
        &golden_root.join("uninstall.removed.txt"),
        &removed_text,
    );
    let golden_removed_text =
        fs::read_to_string(golden_root.join("uninstall.removed.txt")).expect("read golden");
    let mut golden_removed: Vec<String> = golden_removed_text
        .lines()
        .filter(|l| !l.starts_with("./sieve/.cache/"))
        .map(|l| l.to_string())
        .collect();
    golden_removed.sort();

    assert_eq!(
        removed, golden_removed,
        "uninstall removed-file list mismatch"
    );
    assert!(
        !copy.path.join("sieve").exists(),
        "sieve uninstall left the sieve/ dir behind"
    );
}

/// With `SIEVE_BLESS=1`, rewrites the golden init tree from the first
/// `init` run: the report, the files that run wrote in the repo, and the
/// files under `home`. It leaves the `init-fresh.*` goldens alone.
fn bless_init_tree(
    golden_root: &Path,
    repo: &Path,
    before: &[(PathBuf, Vec<u8>)],
    home: &Path,
    stderr: &str,
) {
    use support::golden::bless;
    for entry in fs::read_dir(golden_root).expect("read golden dir") {
        let entry = entry.expect("golden entry");
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with("init-fresh.")
        {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            fs::remove_dir_all(&path).expect("remove golden dir");
        } else {
            fs::remove_file(&path).expect("remove golden file");
        }
    }
    for (rel, bytes) in snapshot_tree(repo) {
        let unchanged = before.iter().any(|(r, b)| *r == rel && *b == bytes);
        if !unchanged {
            bless(&golden_root.join(&rel), &bytes);
        }
    }
    for rel in list_files(home) {
        let bytes = fs::read(home.join(&rel)).expect("read home file");
        bless(&golden_root.join("home").join(rel), &bytes);
    }
    bless(&golden_root.join("init.stderr.txt"), stderr.as_bytes());
    bless(&golden_root.join("init.stdout.txt"), b"");
    bless(&golden_root.join("init.exit.txt"), b"0\n");
    bless(&golden_root.join("second.stdout.txt"), b"");
    bless(&golden_root.join("uninstall.stdout.txt"), b"");
}

/// A deterministic snapshot of every file under `dir` and its content,
/// excluding the regenerable `sieve/.cache/` tree (its `wiring-stamp.json`
/// carries an `at` timestamp that changes on every run, same exclusion
/// the golden capture itself uses).
fn snapshot_tree(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files: Vec<PathBuf> = list_files(dir)
        .into_iter()
        .filter(|p| !p.starts_with(Path::new("sieve").join(".cache")))
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|p| {
            let bytes = fs::read(dir.join(&p)).expect("read snapshot file");
            (p, bytes)
        })
        .collect()
}

/// Reads `path` as a parsed `serde_json::Value`, panicking on a missing
/// or unparseable file (the caller already knows the file should exist).
fn read_json(path: &Path) -> serde_json::Value {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

/// `sieve uninstall -y` must strip Sieve's own hooks and MCP entry from
/// the three `$HOME`-scoped mirrors `install_claude_global` and
/// `install_codex_hooks` write (P4-12, "Global `~/.claude` mirror: 3
/// files, hooks-only, user-MCP-scope"), leaving a foreign hook or a
/// foreign MCP server behind exactly as it was before `init`, and
/// deleting `~/.claude.json` outright when `init` was the one that
/// created it.
#[test]
fn test_p4_12_uninstall_strips_the_global_mirrors() {
    let project = TempDir::new("global-uninstall-project");
    let home = TempDir::new("global-uninstall-home");

    // A foreign Claude Code hook, unrelated to Sieve, must survive both
    // `init` and `uninstall`.
    let claude_settings_path = home.path.join(".claude").join("settings.json");
    let foreign_claude_settings = serde_json::json!({
        "hooks": {
            "PostToolUse": [{
                "matcher": "Foo",
                "hooks": [{"type": "command", "command": "foreign-tool hook", "timeout": 5000}]
            }]
        }
    });
    fs::create_dir_all(claude_settings_path.parent().unwrap()).expect("mkdir ~/.claude");
    fs::write(
        &claude_settings_path,
        serde_json::to_string_pretty(&foreign_claude_settings).unwrap(),
    )
    .expect("write ~/.claude/settings.json");

    // `~/.claude.json` does not exist before `init`: it must not exist
    // after `uninstall` either.
    let claude_json_path = home.path.join(".claude.json");
    assert!(!claude_json_path.exists());

    // A foreign Codex hook, same rule.
    let codex_hooks_path = home.path.join(".codex").join("hooks.json");
    let foreign_codex_hooks = serde_json::json!({
        "hooks": {
            "PostToolUse": [{
                "matcher": "Foo",
                "hooks": [{"type": "command", "command": "foreign-tool hook", "timeout": 5000}]
            }]
        }
    });
    fs::create_dir_all(codex_hooks_path.parent().unwrap()).expect("mkdir ~/.codex");
    fs::write(
        &codex_hooks_path,
        serde_json::to_string_pretty(&foreign_codex_hooks).unwrap(),
    )
    .expect("write ~/.codex/hooks.json");

    let init_output = support::sieve_command()
        .args([
            "init",
            "--agents",
            "agents",
            "claude",
            "--no-build",
            "--global",
        ])
        .current_dir(&project.path)
        .env("HOME", &home.path)
        .env("NO_COLOR", "1")
        .env("DO_NOT_TRACK", "1")
        .output()
        .expect("run sieve init");
    assert_eq!(
        init_output.status.code(),
        Some(0),
        "sieve init exited non-zero: {}",
        String::from_utf8_lossy(&init_output.stderr)
    );

    // Sanity: `init` actually wired the three global mirrors before we
    // assert `uninstall` unwinds them.
    assert!(read_json(&claude_settings_path)["hooks"]["Stop"].is_array());
    assert!(
        claude_json_path.is_file(),
        "init did not create ~/.claude.json"
    );
    assert!(read_json(&codex_hooks_path)["hooks"]["SessionStart"].is_array());

    let uninstall_output = support::sieve_command()
        .args(["uninstall", "-y", "--global"])
        .current_dir(&project.path)
        .env("HOME", &home.path)
        .env("NO_COLOR", "1")
        .env("DO_NOT_TRACK", "1")
        .output()
        .expect("run sieve uninstall");
    assert_eq!(
        uninstall_output.status.code(),
        Some(0),
        "sieve uninstall exited non-zero: {}",
        String::from_utf8_lossy(&uninstall_output.stderr)
    );

    assert_eq!(
        read_json(&claude_settings_path),
        foreign_claude_settings,
        "~/.claude/settings.json did not return to its pre-init content"
    );
    assert!(
        !claude_json_path.exists(),
        "~/.claude.json should be gone: init was the one that created it"
    );
    assert_eq!(
        read_json(&codex_hooks_path),
        foreign_codex_hooks,
        "~/.codex/hooks.json did not return to its pre-init content"
    );
}

/// An unreadable `~/.claude/settings.json` makes its own strip fail, but
/// `sieve uninstall -y` must still exit 0, warn once, and go on to strip
/// the other two global mirrors (F6: the three global strips are each
/// best-effort, so one failure never stops the strips after it).
#[test]
fn test_uninstall_warns_and_continues_past_an_unreadable_global_settings_file() {
    if is_root() {
        eprintln!("skip: running as root, chmod 0o000 grants no protection");
        return;
    }
    let project = TempDir::new("unreadable-settings-project");
    let home = TempDir::new("unreadable-settings-home");
    // `install_claude_global` and `install_codex_hooks` (`init.rs`) only
    // mirror into a host directory that already exists.
    fs::create_dir_all(home.path.join(".claude")).expect("mkdir ~/.claude");
    fs::create_dir_all(home.path.join(".codex")).expect("mkdir ~/.codex");

    let init_output = support::sieve_command()
        .args([
            "init",
            "--agents",
            "agents",
            "claude",
            "--no-build",
            "--global",
        ])
        .current_dir(&project.path)
        .env("HOME", &home.path)
        .env("NO_COLOR", "1")
        .env("DO_NOT_TRACK", "1")
        .output()
        .expect("run sieve init");
    assert_eq!(
        init_output.status.code(),
        Some(0),
        "sieve init exited non-zero: {}",
        String::from_utf8_lossy(&init_output.stderr)
    );

    let claude_settings_path = home.path.join(".claude").join("settings.json");
    let codex_hooks_path = home.path.join(".codex").join("hooks.json");
    assert!(read_json(&claude_settings_path)["hooks"]["Stop"].is_array());
    assert!(read_json(&codex_hooks_path)["hooks"]["SessionStart"].is_array());

    use std::os::unix::fs::PermissionsExt;
    let original_mode = fs::metadata(&claude_settings_path)
        .expect("stat ~/.claude/settings.json")
        .permissions();
    fs::set_permissions(
        &claude_settings_path,
        std::fs::Permissions::from_mode(0o000),
    )
    .expect("chmod ~/.claude/settings.json unreadable");
    // The guard restores read access on drop, even on a panic below.
    let _restore = PermGuard {
        path: claude_settings_path.clone(),
        mode: original_mode,
    };

    let uninstall_output = support::sieve_command()
        .args(["uninstall", "-y", "--global"])
        .current_dir(&project.path)
        .env("HOME", &home.path)
        .env("NO_COLOR", "1")
        .env("DO_NOT_TRACK", "1")
        .output()
        .expect("run sieve uninstall");
    assert_eq!(
        uninstall_output.status.code(),
        Some(0),
        "sieve uninstall exited non-zero: {}",
        String::from_utf8_lossy(&uninstall_output.stderr)
    );

    let stderr = String::from_utf8(uninstall_output.stderr).expect("utf-8 stderr");
    let warning_lines: Vec<&str> = stderr.lines().filter(|l| l.starts_with('⚠')).collect();
    assert_eq!(
        warning_lines.len(),
        1,
        "expected exactly one warning line, got:\n{stderr}"
    );

    assert!(
        !codex_hooks_path.exists(),
        "sieve uninstall should still strip ~/.codex/hooks.json past the earlier failure"
    );
}

/// A read-only `~/.claude` makes the global mirror write fail, but
/// `sieve init` must still exit 0 and print the epilogue: the global
/// mirror is a best-effort extra, not a reason to fail every project-
/// local write that already succeeded.
#[test]
fn test_init_warns_instead_of_failing_on_a_read_only_home() {
    if is_root() {
        eprintln!("skip: running as root, chmod 0o500 grants no protection");
        return;
    }
    let project = TempDir::new("readonly-home-project");
    let home = TempDir::new("readonly-home-home");
    let claude_dir = home.path.join(".claude");
    fs::create_dir_all(&claude_dir).expect("mkdir ~/.claude");

    use std::os::unix::fs::PermissionsExt;
    let original_mode = fs::metadata(&claude_dir)
        .expect("stat ~/.claude")
        .permissions();
    fs::set_permissions(&claude_dir, std::fs::Permissions::from_mode(0o500))
        .expect("chmod ~/.claude read-only");
    // The guard restores write access on drop, even on a panic below, so
    // the temp dir can still be removed on drop too.
    let _restore = PermGuard {
        path: claude_dir.clone(),
        mode: original_mode,
    };

    let init_output = support::sieve_command()
        .args([
            "init",
            "--agents",
            "agents",
            "claude",
            "--no-build",
            "--global",
        ])
        .current_dir(&project.path)
        .env("HOME", &home.path)
        .env("NO_COLOR", "1")
        .env("DO_NOT_TRACK", "1")
        .output()
        .expect("run sieve init");

    assert_eq!(
        init_output.status.code(),
        Some(0),
        "a read-only $HOME/.claude must not fail sieve init: {}",
        String::from_utf8_lossy(&init_output.stderr)
    );
    let stderr = String::from_utf8(init_output.stderr).expect("utf-8 stderr");
    let warning_lines: Vec<&str> = stderr.lines().filter(|l| l.starts_with('⚠')).collect();
    assert_eq!(
        warning_lines.len(),
        1,
        "expected exactly one warning line, got:\n{stderr}"
    );
    assert!(
        stderr.contains("restart your agent"),
        "sieve init skipped its epilogue:\n{stderr}"
    );
}

/// A build failure never stops `init` (P4-01): `init` runs
/// `buildGraphIfMissing` runs `sieve build .` as a child process, catches a
/// non-zero exit, and returns `false`; its caller then finishes the whole
/// report and the epilogue, and `sieve init` still exits 0. Pre-creating
/// `sieve` as a plain file forces the build's own `mkdir` to fail. The error
/// text can differ by runtime, so only the shape of the report — not that one
/// line — is pinned here, matching the `⚠`-count/epilogue style of
/// `test_init_warns_instead_of_failing_on_a_read_only_home` above).
#[test]
fn test_p4_01_init_continues_when_the_build_fails() {
    let project = TempDir::new("build-fails-project");
    let home = TempDir::new("build-fails-home");
    fs::write(project.path.join("sieve"), b"").expect("write a plain `sieve` file");

    let init_output = support::sieve_command()
        .args(["init", "--agents", "agents", "claude", "--verbose"])
        .current_dir(&project.path)
        .env("HOME", &home.path)
        .env("NO_COLOR", "1")
        .env("DO_NOT_TRACK", "1")
        .output()
        .expect("run sieve init");

    assert_eq!(
        init_output.status.code(),
        Some(0),
        "a build failure must not fail sieve init: {}",
        String::from_utf8_lossy(&init_output.stderr)
    );

    let stderr = String::from_utf8(init_output.stderr).expect("utf-8 stderr");
    assert!(
        stderr.contains("sieve: "),
        "missing the build's own error line:\n{stderr}"
    );
    assert!(
        stderr.contains("· skipped graph build"),
        "a failed build must report itself as skipped, not built:\n{stderr}"
    );
    assert!(
        stderr.contains("✓ wrote"),
        "the Claude writes must still be reported after a build failure:\n{stderr}"
    );
    assert!(
        stderr.contains("✓ agents:"),
        "the host loop must still run after a build failure:\n{stderr}"
    );
    // No graph exists after the failed build, so the epilogue takes
    // the no-graph branch: a fourth, renumbered step ("1. build the
    // graph") and no node/edge stats in the wordmark.
    assert!(
        stderr.contains("1. build the graph     sieve build"),
        "sieve init did not fall back to the no-graph epilogue:\n{stderr}"
    );
    assert!(
        stderr.contains("2. restart your agent"),
        "the no-graph epilogue's steps were not renumbered:\n{stderr}"
    );
    assert!(
        project.path.join("AGENTS.md").is_file(),
        "the agents host file must still be written after a build failure"
    );
}

/// An unparseable `.claude/settings.json` must be left untouched, with a
/// warning printed instead of `✓ wrote`, matching the `.mcp.json` block's
/// own `SkippedUnparseable` handling.
#[test]
fn test_init_warns_when_claude_settings_is_unparseable() {
    let project = TempDir::new("unparseable-settings-project");
    let home = TempDir::new("unparseable-settings-home");
    let settings_path = project.path.join(".claude").join("settings.json");
    fs::create_dir_all(settings_path.parent().unwrap()).expect("mkdir .claude");
    let original = "{ not valid json";
    fs::write(&settings_path, original).expect("write .claude/settings.json");

    let init_output = support::sieve_command()
        .args(["init", "--agents", "agents", "claude", "--no-build"])
        .current_dir(&project.path)
        .env("HOME", &home.path)
        .env("NO_COLOR", "1")
        .env("DO_NOT_TRACK", "1")
        .output()
        .expect("run sieve init");
    assert_eq!(
        init_output.status.code(),
        Some(0),
        "sieve init exited non-zero: {}",
        String::from_utf8_lossy(&init_output.stderr)
    );

    let after = fs::read_to_string(&settings_path).expect("read .claude/settings.json");
    assert_eq!(
        after, original,
        "an unparseable settings.json was rewritten"
    );

    let stderr = String::from_utf8(init_output.stderr).expect("utf-8 stderr");
    assert!(
        stderr.contains("left unchanged (not valid JSON)"),
        "missing the unparseable-settings warning:\n{stderr}"
    );
}

/// `sieve init --all-agents` on a repo with no graph builds the graph
/// in-process before it reports any of its own writes (P4-01): the build
/// subprocess's own progress and report print first, then every `✓ wrote` / `✓
/// mcp` / `✓ built the graph` line, matching the init flow, which runs
/// `buildGraphIfMissing` inside itself and returns before its caller prints a
/// single report line. The `basic.expected/init/ init-fresh.*` golden pins this
/// branch; the pre-built-graph test above only pins the "graph already exists,
/// build is skipped" branch.
#[test]
fn test_p4_01_init_on_a_fresh_repo_builds_first_and_matches_golden() {
    let fixture = support::manifest_dir().join("../../tests/fixtures/basic");
    let golden_root = support::manifest_dir().join("../../tests/fixtures/basic.expected/init");

    let copy = TempDir::new("fresh-copy");
    let home = TempDir::new("fresh-home");
    copy_dir(&fixture, &copy.path);
    let copy_path = fs::canonicalize(&copy.path).expect("canonicalize copy path");
    let home_path = fs::canonicalize(&home.path).expect("canonicalize home path");

    let init_output = support::sieve_command()
        .args(["init", "--all-agents", "--global"])
        .current_dir(&copy.path)
        .env("HOME", &home_path)
        .env("CLAUDE_PROJECT_DIR", &copy_path)
        .env("NO_COLOR", "1")
        .env("DO_NOT_TRACK", "1")
        .output()
        .expect("run sieve init");

    let golden_exit = {
        let code = init_output.status.code().unwrap_or(-1);
        support::golden::bless_text_if_blessing(
            &golden_root.join("init-fresh.exit.txt"),
            &format!("{code}\n"),
        );
        fs::read_to_string(golden_root.join("init-fresh.exit.txt"))
            .expect("read golden init-fresh.exit.txt")
    };
    assert_eq!(
        init_output.status.code(),
        Some(
            golden_exit
                .trim()
                .parse::<i32>()
                .expect("parse golden exit code")
        ),
        "sieve init exit code mismatch: stderr:\n{}",
        String::from_utf8_lossy(&init_output.stderr)
    );

    let mut stdout = String::from_utf8(init_output.stdout).expect("utf-8 stdout");
    stdout = mask(&stdout, &copy_path);
    stdout = mask(&stdout, &home_path);
    support::golden::bless_text_if_blessing(&golden_root.join("init-fresh.stdout.txt"), &stdout);
    let golden_stdout =
        fs::read_to_string(golden_root.join("init-fresh.stdout.txt")).expect("read golden stdout");
    assert_eq!(stdout, golden_stdout, "init-fresh stdout mismatch");

    let mut stderr = String::from_utf8(init_output.stderr).expect("utf-8 stderr");
    stderr = mask(&stderr, &copy_path);
    stderr = mask(&stderr, &home_path);
    // Drop the update-nudge line, same as the pre-built-graph test above —
    // its content depends on the machine and the day, not on this fixture.
    let produced_lines: Vec<&str> = stderr.lines().filter(|l| !l.contains('⬆')).collect();

    support::golden::bless_text_if_blessing(&golden_root.join("init-fresh.stderr.txt"), &stderr);
    let golden_stderr = fs::read_to_string(golden_root.join("init-fresh.stderr.txt"))
        .expect("read golden init-fresh.stderr.txt");
    let golden_lines: Vec<&str> = golden_stderr.lines().filter(|l| !l.contains('⬆')).collect();

    assert_eq!(produced_lines, golden_lines, "init-fresh stderr mismatch");
}

/// Builds a scratch `$HOME` with an empty `.claude/` and `.codex/` dir,
/// and a `.claude/settings.json` holding one foreign key. Returns the
/// `TempDir` and the settings file's starting bytes.
fn seed_home_with_settings(label: &str) -> (TempDir, Vec<u8>) {
    let home = TempDir::new(label);
    assert!(
        !home.path.starts_with("/Users/iamalvisng"),
        "the scratch HOME must never be the real HOME"
    );
    fs::create_dir_all(home.path.join(".claude")).expect("mkdir ~/.claude");
    fs::create_dir_all(home.path.join(".codex")).expect("mkdir ~/.codex");
    let settings_path = home.path.join(".claude").join("settings.json");
    let starting = b"{\"theme\":\"dark\"}".to_vec();
    fs::write(&settings_path, &starting).expect("write ~/.claude/settings.json");
    (home, starting)
}

/// `sieve init` without `--global` (P: "sieve init writes under HOME only
/// with --global", ledger 2026-09-16) must leave every file under `$HOME`
/// untouched, print the skip line once, and still write the repo files.
#[test]
fn init_without_global_writes_nothing_under_home() {
    let fixture = support::manifest_dir().join("../../tests/fixtures/basic");
    let (home, settings_before) = seed_home_with_settings("no-global-init-home");
    assert!(
        home.path != Path::new("/Users/iamalvisng"),
        "the scratch HOME must never be the real HOME"
    );
    let copy = TempDir::new("no-global-init-copy");
    copy_dir(&fixture, &copy.path);

    let home_files_before = list_files(&home.path);

    let output = support::sieve_command()
        .args([
            "init",
            "-y",
            "--verbose",
            "--agents",
            "agents",
            "claude",
            "cursor",
            "antigravity",
        ])
        .current_dir(&copy.path)
        .env("HOME", &home.path)
        .env("NO_COLOR", "1")
        .env("DO_NOT_TRACK", "1")
        .output()
        .expect("run sieve init");
    assert_eq!(
        output.status.code(),
        Some(0),
        "sieve init exited non-zero: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let home_files_after = list_files(&home.path);
    assert_eq!(
        home_files_before, home_files_after,
        "init without --global changed the set of files under HOME"
    );

    let settings_path = home.path.join(".claude").join("settings.json");
    let settings_after = fs::read(&settings_path).expect("read ~/.claude/settings.json");
    assert_eq!(
        settings_before, settings_after,
        "init without --global rewrote ~/.claude/settings.json"
    );

    assert!(
        !home.path.join(".codex").join("hooks.json").exists(),
        "init without --global must not write ~/.codex/hooks.json"
    );
    assert!(
        !home.path.join(".claude.json").exists(),
        "init without --global must not write ~/.claude.json"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("skipped global hooks under ~ \u{b7} pass --global to write them"),
        "missing the global-skip line:\n{stderr}"
    );

    assert!(
        copy.path.join(".cursor").join("rules").is_dir(),
        "init must still write the repo-local .cursor/rules/ dir"
    );
}

/// `sieve init --global` writes the global Claude Code and Codex hooks
/// under `$HOME`, keeping a foreign key already in `settings.json`.
#[test]
fn init_with_global_writes_the_global_hooks() {
    let fixture = support::manifest_dir().join("../../tests/fixtures/basic");
    let (home, _settings_before) = seed_home_with_settings("with-global-init-home");
    let copy = TempDir::new("with-global-init-copy");
    copy_dir(&fixture, &copy.path);

    let output = support::sieve_command()
        .args([
            "init",
            "-y",
            "--agents",
            "agents",
            "claude",
            "cursor",
            "antigravity",
            "--global",
        ])
        .current_dir(&copy.path)
        .env("HOME", &home.path)
        .env("NO_COLOR", "1")
        .env("DO_NOT_TRACK", "1")
        .output()
        .expect("run sieve init");
    assert_eq!(
        output.status.code(),
        Some(0),
        "sieve init exited non-zero: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let settings_path = home.path.join(".claude").join("settings.json");
    let settings_after = read_json(&settings_path);
    assert_eq!(
        settings_after["theme"], "dark",
        "init --global must keep the foreign theme key"
    );
    assert!(
        settings_after["hooks"]["Stop"].is_array(),
        "init --global must add the Claude Code hook entries"
    );

    assert!(
        home.path.join(".codex").join("hooks.json").is_file(),
        "init --global must write ~/.codex/hooks.json"
    );
}

/// `sieve uninstall` without `--global` must leave every file under
/// `$HOME` exactly as `init --global` left it.
#[test]
fn uninstall_without_global_leaves_home_untouched() {
    let fixture = support::manifest_dir().join("../../tests/fixtures/basic");
    let (home, _settings_before) = seed_home_with_settings("no-global-uninstall-home");
    let copy = TempDir::new("no-global-uninstall-copy");
    copy_dir(&fixture, &copy.path);

    let init_output = support::sieve_command()
        .args([
            "init",
            "-y",
            "--agents",
            "agents",
            "claude",
            "cursor",
            "antigravity",
            "--global",
        ])
        .current_dir(&copy.path)
        .env("HOME", &home.path)
        .env("NO_COLOR", "1")
        .env("DO_NOT_TRACK", "1")
        .output()
        .expect("run sieve init");
    assert_eq!(
        init_output.status.code(),
        Some(0),
        "sieve init exited non-zero: {}",
        String::from_utf8_lossy(&init_output.stderr)
    );

    let home_snapshot_before = snapshot_tree(&home.path);

    let uninstall_output = support::sieve_command()
        .args(["uninstall", "-y"])
        .current_dir(&copy.path)
        .env("HOME", &home.path)
        .env("NO_COLOR", "1")
        .env("DO_NOT_TRACK", "1")
        .output()
        .expect("run sieve uninstall");
    assert_eq!(
        uninstall_output.status.code(),
        Some(0),
        "sieve uninstall exited non-zero: {}",
        String::from_utf8_lossy(&uninstall_output.stderr)
    );

    let home_snapshot_after = snapshot_tree(&home.path);
    assert_eq!(
        home_snapshot_before, home_snapshot_after,
        "uninstall without --global changed a file under HOME"
    );

    let stderr = String::from_utf8_lossy(&uninstall_output.stderr);
    assert!(
        stderr.contains("skipped global hooks under ~ \u{b7} pass --global to remove them"),
        "missing the uninstall global-skip line:\n{stderr}"
    );
}

/// `sieve uninstall`'s dry run (no `-y`) without `--global` must still name
/// the global entries it would leave, and must touch no file under `$HOME`.
#[test]
fn uninstall_dry_run_without_global_says_global_hooks_stay() {
    let (home, _settings_before) = seed_home_with_settings("dry-run-no-global-home");
    let copy = TempDir::new("dry-run-no-global-copy");

    let home_snapshot_before = snapshot_tree(&home.path);

    let output = support::sieve_command()
        .args(["uninstall"])
        .current_dir(&copy.path)
        .env("HOME", &home.path)
        .env("NO_COLOR", "1")
        .env("DO_NOT_TRACK", "1")
        .output()
        .expect("run sieve uninstall");
    assert_eq!(
        output.status.code(),
        Some(0),
        "sieve uninstall dry run exited non-zero: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("global hooks under ~ stay \u{b7} pass --global to remove them"),
        "missing the dry-run global-stay line:\n{stderr}"
    );

    let home_snapshot_after = snapshot_tree(&home.path);
    assert_eq!(
        home_snapshot_before, home_snapshot_after,
        "the dry run touched a file under HOME"
    );
}

/// Defect 1 (probe 2026-09-30): `sieve init --no-agents` wrote nothing
/// and printed "no agents selected". The flag maps
/// `--no-agents` to `ids = ["claude"]`, so the run writes the Claude
/// Code files and no other host's files.
#[test]
fn test_defect1_init_no_agents_writes_claude_only() {
    let project = TempDir::new("no-agents-project");
    let home = TempDir::new("no-agents-home");
    assert!(
        !home.path.starts_with("/Users/iamalvisng"),
        "the scratch HOME must never be the real HOME"
    );
    fs::create_dir_all(project.path.join(".cursor")).expect("mkdir .cursor");

    let output = support::sieve_command()
        .args(["init", "--no-agents", "--no-build"])
        .current_dir(&project.path)
        .env("HOME", &home.path)
        .env("NO_COLOR", "1")
        .env("DO_NOT_TRACK", "1")
        .output()
        .expect("run sieve init");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "exit: {stderr}");
    assert!(
        !stderr.contains("no agents selected"),
        "--no-agents must select Claude Code:\n{stderr}"
    );
    assert!(
        project.path.join(".claude").join("settings.json").is_file(),
        "missing .claude/settings.json:\n{stderr}"
    );
    assert!(
        !project.path.join(".cursor").join("hooks.json").exists(),
        "--no-agents must not write the detected cursor host:\n{stderr}"
    );
}

/// Defect 2 (probe 2026-09-30): `sieve init --agents claude` exited 1
/// with "unknown agent id(s): claude", while `--list-agents` printed
/// `claude`. Both flags read one id list, which
/// validates against `[...hostIds(), "claude"]`.
#[test]
fn test_defect2_init_agents_accepts_every_listed_id() {
    let project = TempDir::new("agents-claude-project");
    let home = TempDir::new("agents-claude-home");
    assert!(
        !home.path.starts_with("/Users/iamalvisng"),
        "the scratch HOME must never be the real HOME"
    );

    let listed = support::sieve_command()
        .args(["init", "--list-agents"])
        .current_dir(&project.path)
        .env("HOME", &home.path)
        .output()
        .expect("run sieve init --list-agents");
    let ids: Vec<String> = String::from_utf8_lossy(&listed.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    assert!(ids.iter().any(|id| id == "claude"), "ids: {ids:?}");

    for id in &ids {
        let output = support::sieve_command()
            .args(["init", "--no-build", "--agents", id])
            .current_dir(&project.path)
            .env("HOME", &home.path)
            .env("NO_COLOR", "1")
            .env("DO_NOT_TRACK", "1")
            .output()
            .expect("run sieve init --agents");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.code(),
            Some(0),
            "--agents {id} was listed but rejected:\n{stderr}"
        );
    }
}

/// P4-02, defect 3 (probe 2026-09-30): `sieve init --agents copilot` wrote the
/// Claude Code files. The init sets `wantClaude = ids.includes("claude")` and
/// writes the Claude files only when it is true. Without `claude`, sieve writes
/// the selected host, then reports the build on its own line after the host
/// lines.
#[test]
fn test_p4_02_init_agents_without_claude_writes_no_claude_files() {
    let project = TempDir::new("agents-copilot-project");
    let home = TempDir::new("agents-copilot-home");
    assert!(
        !home.path.starts_with("/Users/iamalvisng"),
        "the scratch HOME must never be the real HOME"
    );

    let output = support::sieve_command()
        .args([
            "init",
            "--agents",
            "copilot",
            "--no-build",
            "--global",
            "--verbose",
        ])
        .current_dir(&project.path)
        .env("HOME", &home.path)
        .env("NO_COLOR", "1")
        .env("DO_NOT_TRACK", "1")
        .output()
        .expect("run sieve init");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "exit: {stderr}");

    assert!(
        project
            .path
            .join(".github")
            .join("copilot-instructions.md")
            .is_file(),
        "missing the copilot host file:\n{stderr}"
    );
    assert!(
        !project.path.join(".claude").exists(),
        "--agents copilot must not write .claude/:\n{stderr}"
    );
    assert!(
        !project.path.join(".mcp.json").exists(),
        "--agents copilot must not write .mcp.json:\n{stderr}"
    );
    assert!(
        !home.path.join(".claude").exists() && !home.path.join(".claude.json").exists(),
        "--agents copilot --global must not mirror the Claude hooks under HOME:\n{stderr}"
    );
    assert!(
        !stderr.contains("✓ wrote") && !stderr.contains("mcp claude"),
        "no Claude report line may print:\n{stderr}"
    );
    let host_line = stderr.find("✓ copilot:").expect("copilot report line");
    let build_line = stderr
        .find("· skipped graph build")
        .expect("build report line");
    assert!(
        host_line < build_line,
        "the build line must follow the host lines, as sieve prints it:\n{stderr}"
    );
}

/// The four Codex hook events the spec row for P4-05 lists
/// (`04-hosts-mcp-hooks.md` section 1, "Codex hooks"), with every matcher and
/// every timeout. They nest under a top-level `hooks` key. The command form is
/// `sieve hook <sub>`, not `node "<shim>" <sub>`, per the ledger entry of
/// 2026-09-12 ("Sieve writes no .cjs shim").
fn expected_codex_hooks() -> serde_json::Value {
    let block = |matcher: Option<&str>, sub: &str, timeout: u64| {
        let handler = serde_json::json!({
            "type": "command",
            "command": format!("sieve hook {sub}"),
            "timeout": timeout,
        });
        match matcher {
            Some(m) => serde_json::json!([{ "matcher": m, "hooks": [handler] }]),
            None => serde_json::json!([{ "hooks": [handler] }]),
        }
    };
    serde_json::json!({ "hooks": {
        "SessionStart": block(Some("startup|resume|compact"), "session-start", 10000),
        "UserPromptSubmit": block(None, "prompt", 15000),
        "PostToolUse": block(Some("apply_patch|Write|Edit|MultiEdit"), "post-edit", 10000),
        "Stop": block(None, "stop", 10000),
    } })
}

/// Runs `sieve init -y --agents agents --global --no-build` in a fresh
/// fixture copy, with `home` as `$HOME`. Returns the stderr text.
fn run_init_agents_global(home: &Path) -> String {
    assert!(
        !home.starts_with("/Users/iamalvisng"),
        "the scratch HOME must never be the real HOME"
    );
    let fixture = support::manifest_dir().join("../../tests/fixtures/basic");
    let copy = TempDir::new("p4-05-copy");
    copy_dir(&fixture, &copy.path);
    let output = support::sieve_command()
        .args(["init", "-y", "--agents", "agents", "--global", "--no-build"])
        .current_dir(&copy.path)
        .env("HOME", home)
        .env("NO_COLOR", "1")
        .env("DO_NOT_TRACK", "1")
        .output()
        .expect("run sieve init");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(
        output.status.code(),
        Some(0),
        "sieve init exited non-zero: {stderr}"
    );
    stderr
}

/// P4-05: `init --global` with `~/.codex/` present writes `~/.codex/hooks.json`
/// with exactly the four events, their matchers, their timeouts and the
/// command form. Without `~/.codex/`, the gate holds and no file appears.
#[test]
fn test_p4_05_init_global_writes_the_codex_hooks_exactly() {
    let home = TempDir::new("p4-05-home");
    fs::create_dir_all(home.path.join(".codex")).expect("mkdir ~/.codex");
    run_init_agents_global(&home.path);

    let hooks_path = home.path.join(".codex").join("hooks.json");
    assert_eq!(
        read_json(&hooks_path),
        expected_codex_hooks(),
        "~/.codex/hooks.json differs from the P4-05 spec row"
    );

    // The gate: no `~/.codex/` directory, no write.
    let bare_home = TempDir::new("p4-05-bare-home");
    run_init_agents_global(&bare_home.path);
    assert!(
        !bare_home.path.join(".codex").exists(),
        "init --global must not create ~/.codex when the dir is absent"
    );
}

/// Runs `sieve init -y --agents claude --no-build` in a fresh fixture copy
/// under a scratch `$HOME`, with `env` as extra environment variables and `extra_args` after the base arguments.
/// Returns the parsed repo-local `.claude/settings.json`.
fn init_claude_settings(env: &[(&str, &str)], extra_args: &[&str]) -> serde_json::Value {
    let fixture = support::manifest_dir().join("../../tests/fixtures/basic");
    let home = TempDir::new("p4-11-home");
    assert!(
        !home.path.starts_with("/Users/iamalvisng"),
        "the scratch HOME must never be the real HOME"
    );
    let copy = TempDir::new("p4-11-copy");
    copy_dir(&fixture, &copy.path);
    let mut cmd = support::sieve_command();
    cmd.args(["init", "-y", "--agents", "claude", "--no-build"])
        .args(extra_args)
        .current_dir(&copy.path)
        .env("HOME", &home.path)
        .env("NO_COLOR", "1")
        .env("DO_NOT_TRACK", "1");
    for (key, value) in env {
        cmd.env(key, value);
    }
    let output = cmd.output().expect("run sieve init");
    assert_eq!(
        output.status.code(),
        Some(0),
        "sieve init exited non-zero: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    read_json(&copy.path.join(".claude").join("settings.json"))
}

/// Asserts that `settings` holds no statusline key at all.
fn assert_no_statusline(settings: &serde_json::Value, label: &str) {
    for key in ["statusLine", "subagentStatusLine"] {
        assert!(
            settings.get(key).is_none(),
            "{label}: settings.json must hold no {key} key, got: {settings}"
        );
    }
}

/// Asserts that `settings` holds Sieve's own statusline under both keys.
fn assert_has_statusline(settings: &serde_json::Value, label: &str) {
    for key in ["statusLine", "subagentStatusLine"] {
        assert_eq!(
            settings[key]["command"], "sieve statusline",
            "{label}: settings.json must hold Sieve's {key}, got: {settings}"
        );
    }
}

/// P4-11: the `--no-statusline` flag writes no statusline key. The control
/// run without the flag writes both keys, so the assertion cannot pass on
/// an empty file.
#[test]
fn test_p4_11_no_statusline_flag_writes_no_statusline_key() {
    assert_has_statusline(&init_claude_settings(&[], &[]), "control");
    assert_no_statusline(
        &init_claude_settings(&[], &["--no-statusline"]),
        "--no-statusline",
    );
}

/// P4-11: under the sieve name, `SIEVE_NO_STATUSLINE=1` removes the key.
#[test]
fn test_p4_11_sieve_env_removes_the_statusline_under_the_sieve_name() {
    assert_no_statusline(
        &init_claude_settings(&[("SIEVE_NO_STATUSLINE", "1")], &[]),
        "SIEVE_NO_STATUSLINE=1 under sieve",
    );
}

/// P4-11: the variable name follows the product. Under the sieve name,
/// `OTHER_NO_STATUSLINE=1` is a foreign variable and removes nothing.
#[test]
fn test_p4_11_a_foreign_env_does_not_remove_the_statusline() {
    assert_has_statusline(
        &init_claude_settings(&[("OTHER_NO_STATUSLINE", "1")], &[]),
        "OTHER_NO_STATUSLINE=1 under sieve",
    );
}

/// Runs `sieve init --no-build` in `project` with `home` as `$HOME` and
/// `args` after the base arguments. Asserts a zero exit. Returns the
/// stderr text.
fn run_init_in(project: &Path, home: &Path, args: &[&str]) -> String {
    assert!(
        !home.starts_with("/Users/iamalvisng"),
        "the scratch HOME must never be the real HOME"
    );
    let output = support::sieve_command()
        .args(["init", "--no-build", "--verbose"])
        .args(args)
        .current_dir(project)
        .env("HOME", home)
        .env("NO_COLOR", "1")
        .env("DO_NOT_TRACK", "1")
        .output()
        .expect("run sieve init");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(
        output.status.code(),
        Some(0),
        "sieve init exited non-zero: {stderr}"
    );
    stderr
}

/// P4-02: `--agents` outranks `--all-agents`. With both flags, `init`
/// writes the two kiro files and nothing else. `--dry-run`
/// with the same flags lists only those two files.
#[test]
fn test_p4_02_agents_flag_outranks_all_agents() {
    let project = TempDir::new("p4-02-precedence-project");
    let home = TempDir::new("p4-02-precedence-home");
    let stderr = run_init_in(
        &project.path,
        &home.path,
        &["--agents", "kiro", "--all-agents"],
    );

    let written = list_files(&project.path);
    let expected = vec![
        PathBuf::from(".kiro/settings/mcp.json"),
        PathBuf::from(".kiro/steering/sieve.md"),
    ];
    assert_eq!(
        written, expected,
        "--agents kiro --all-agents must write the kiro files only:\n{stderr}"
    );
    assert!(
        !stderr.contains("✓ wrote"),
        "no Claude report line may print when --agents omits claude:\n{stderr}"
    );
}

/// P4-03: an `owned` file with other content is overwritten and reported
/// `(replaced)`. A `section` file with user text keeps that text and gets
/// the fenced block appended. A repeat run reports both `(unchanged)`.
/// The expected bytes come from a fresh run in a second project, so the
/// test does not restate the instruction body.
#[test]
fn test_p4_03_owned_replaces_on_diff_and_section_keeps_user_text() {
    let home = TempDir::new("p4-03-home");
    let fresh = TempDir::new("p4-03-fresh-project");
    run_init_in(&fresh.path, &home.path, &["--agents", "kiro", "agents"]);
    let kiro_rel = Path::new(".kiro/steering/sieve.md");
    let fresh_kiro = fs::read(fresh.path.join(kiro_rel)).expect("read the fresh kiro file");
    let fresh_agents = fs::read_to_string(fresh.path.join("AGENTS.md")).expect("read AGENTS.md");
    assert!(
        fresh_agents.starts_with("<!-- sieve:start -->"),
        "a fresh AGENTS.md must be the block alone, got:\n{fresh_agents}"
    );

    let seeded = TempDir::new("p4-03-seeded-project");
    // `init` prints the canonical cwd, so the report lines match only a
    // canonical path (`/private/var/...`, not the `/var/...` symlink).
    let seeded_root = fs::canonicalize(&seeded.path).expect("canonicalize the project");
    let kiro_path = seeded_root.join(kiro_rel);
    fs::create_dir_all(kiro_path.parent().expect("parent")).expect("mkdir steering");
    fs::write(&kiro_path, "stale steering text\n").expect("seed the kiro file");
    let user_text = "# My notes\n";
    fs::write(seeded_root.join("AGENTS.md"), user_text).expect("seed AGENTS.md");

    let stderr = run_init_in(&seeded.path, &home.path, &["--agents", "kiro", "agents"]);
    assert!(
        stderr.contains(&format!("✓ kiro: {} (replaced)", kiro_path.display())),
        "the kiro line must say (replaced):\n{stderr}"
    );
    assert_eq!(
        fs::read(&kiro_path).expect("read the kiro file"),
        fresh_kiro,
        "the owned file must hold the fresh content after the overwrite"
    );
    assert_eq!(
        fs::read_to_string(seeded_root.join("AGENTS.md")).expect("read AGENTS.md"),
        format!("{user_text}\n{fresh_agents}"),
        "AGENTS.md must keep the user text and append the block"
    );

    let stderr = run_init_in(&seeded.path, &home.path, &["--agents", "kiro", "agents"]);
    assert!(
        stderr.contains(&format!("✓ kiro: {} (unchanged)", kiro_path.display())),
        "the repeat run must report the kiro file unchanged:\n{stderr}"
    );
    let agents_path = seeded_root.join("AGENTS.md");
    assert!(
        stderr.contains(&format!("✓ agents: {} (unchanged)", agents_path.display())),
        "the repeat run must report AGENTS.md unchanged:\n{stderr}"
    );
}

/// Seeds `.claude/settings.json` in a fresh project with `seed`, runs
/// `init --agents claude --no-build`, and returns the parsed file, the
/// stderr text and the project dir (kept alive for a repeat run).
fn init_claude_over_seed(seed: &str) -> (serde_json::Value, String, TempDir, TempDir) {
    let project = TempDir::new("seeded-settings-project");
    let home = TempDir::new("seeded-settings-home");
    let settings_path = project.path.join(".claude").join("settings.json");
    fs::create_dir_all(project.path.join(".claude")).expect("mkdir .claude");
    fs::write(&settings_path, seed).expect("seed settings.json");
    let stderr = run_init_in(&project.path, &home.path, &["--agents", "claude"]);
    (read_json(&settings_path), stderr, project, home)
}

/// P4-06: a foreign `statusLine.command` is kept as-is and a warning
/// prints. The absent `subagentStatusLine` is still set to Sieve's own,
/// because each key is merged on its own.
#[test]
fn test_p4_06_foreign_statusline_is_kept_with_a_warning() {
    let (settings, stderr, _project, _home) =
        init_claude_over_seed(r#"{"statusLine":{"type":"command","command":"foo"}}"#);
    assert_eq!(
        settings["statusLine"]["command"], "foo",
        "the foreign statusLine must survive, got: {settings}"
    );
    assert_eq!(
        settings["subagentStatusLine"]["command"], "sieve statusline",
        "the absent subagentStatusLine must be set, got: {settings}"
    );
    assert!(
        stderr.contains("⚠ Existing statusLine left untouched"),
        "the foreign statusLine must print a warning:\n{stderr}"
    );
    assert!(
        !stderr.contains("subagentStatusLine left untouched"),
        "no subagentStatusLine warning may print when the key was absent:\n{stderr}"
    );
}

/// One of Sieve's own Claude hook blocks, in the ledgered `sieve hook <sub>`
/// command form (ledger 2026-09-12, "Sieve writes no .cjs shim").
fn claude_hook_json(matcher: &str, sub: &str, timeout: u64) -> serde_json::Value {
    serde_json::json!({
        "matcher": matcher,
        "hooks": [{ "type": "command", "command": format!("sieve hook {sub}"), "timeout": timeout }],
    })
}

/// P4-07: the hooks merge keeps a foreign entry in place, drops a stale
/// entry of Sieve's own, and appends the exact current blocks after the
/// foreign entry. An event Sieve does not manage survives untouched.
#[test]
fn test_p4_07_hooks_merge_keeps_foreign_and_drops_stale_own_entries() {
    let foreign = serde_json::json!({
        "matcher": "Bash",
        "hooks": [{ "type": "command", "command": "echo foreign", "timeout": 1 }],
    });
    let stale_own = claude_hook_json("Write", "post-edit", 1);
    let unmanaged = serde_json::json!([{ "hooks": [{ "type": "command", "command": "say hi" }] }]);
    let seed = serde_json::json!({
        "hooks": { "PostToolUse": [stale_own, foreign], "Notification": unmanaged }
    });
    let (settings, _stderr, _project, _home) = init_claude_over_seed(&seed.to_string());

    let expected_post_tool_use = serde_json::json!([
        foreign,
        claude_hook_json("Write|Edit|MultiEdit", "post-edit", 10000),
        claude_hook_json("Bash|mcp__sieve__|Read|Grep|Glob", "tool-savings", 8000),
        claude_hook_json("Read", "post-read", 5000),
        claude_hook_json("Bash", "post-read", 5000),
        claude_hook_json("Bash", "post-search", 3000),
        claude_hook_json("Grep", "post-search", 3000),
    ]);
    assert_eq!(
        settings["hooks"]["PostToolUse"], expected_post_tool_use,
        "PostToolUse must be [foreign, post-edit, tool-savings, post-read x2, post-search x2], got: {settings}"
    );
    assert_eq!(
        settings["hooks"]["Notification"], unmanaged,
        "an unmanaged event must survive untouched, got: {settings}"
    );
}

/// P4-09: the allow merge keeps a foreign entry, drops every prior entry
/// of Sieve's own, then appends the four entries once, in order. A repeat
/// run leaves the list as it is.
///
/// This test seeds the four exact entries. It does not seed the bare
/// `Bash` form the prefix regex also drops; Sieve keeps that
/// form today, and the main thread owns that decision.
#[test]
fn test_p4_09_allow_merge_appends_the_four_entries_once() {
    let seed = r#"{"permissions":{"allow":["Bash(npx sieve:*)","Bash(ls:*)","Bash(sieve:*)"],"deny":["Bash(rm:*)"]}}"#;
    let (settings, _stderr, project, home) = init_claude_over_seed(seed);
    let expected = serde_json::json!([
        "Bash(ls:*)",
        "Bash(sieve:*)",
        "Bash(npx sieve:*)",
        "Bash(sieve-dev:*)",
        "Bash(node dist/cli.js:*)",
    ]);
    assert_eq!(
        settings["permissions"]["allow"], expected,
        "allow must be [foreign, then the four own entries], got: {settings}"
    );
    assert_eq!(
        settings["permissions"]["deny"],
        serde_json::json!(["Bash(rm:*)"]),
        "permissions.deny must survive, got: {settings}"
    );

    run_init_in(&project.path, &home.path, &["--agents", "claude"]);
    let again = read_json(&project.path.join(".claude").join("settings.json"));
    assert_eq!(
        again["permissions"]["allow"], expected,
        "a repeat run must not stack the entries, got: {again}"
    );
}

/// P4-49: `init --agents claude` after `init --agents claude cursor` removes
/// the Cursor wiring and prints the two removal lines before any `✓ wrote`
/// line. The Claude files stay.
#[test]
fn test_p4_49_init_retracts_the_unselected_agent() {
    let project = TempDir::new("p4-49-seq-project");
    let home = TempDir::new("p4-49-seq-home");
    let root = fs::canonicalize(&project.path).expect("canonicalize the project");
    run_init_in(&project.path, &home.path, &["--agents", "claude", "cursor"]);
    assert!(root.join(".cursor/rules/sieve.mdc").is_file());
    assert!(root.join(".cursor/mcp.json").is_file());

    let stderr = run_init_in(&project.path, &home.path, &["--agents", "claude"]);
    let expected = format!(
        "- removed {r}/.cursor/rules/sieve.mdc (sieve-owned instruction file) \u{2014} agent not selected\n\
         - removed {r}/.cursor/mcp.json (mcpServers.sieve) \u{2014} agent not selected\n\
         \u{2713} wrote {r}/.claude/settings.json\n",
        r = root.display()
    );
    assert!(
        stderr.contains(&expected),
        "the retract lines must come first, in sieve's order:\n{stderr}"
    );
    assert!(!root.join(".cursor/rules/sieve.mdc").exists());
    assert!(!root.join(".cursor/mcp.json").exists());
    assert!(root.join(".claude/settings.json").is_file());
    assert!(root.join(".claude/skills/sieve/SKILL.md").is_file());
    assert!(root.join(".mcp.json").is_file());
}

/// P4-49: retract never removes what Sieve did not write. A user's prose in
/// `AGENTS.md`, a foreign server in `.cursor/mcp.json`, and a user's own
/// `.claude/settings.json` and `.mcp.json` all survive. The last two hold no
/// Sieve key, so they stay byte for byte and print no `removed` line.
#[test]
fn test_p4_49_retract_keeps_user_owned_content() {
    let project = TempDir::new("p4-49-user-project");
    let home = TempDir::new("p4-49-user-home");
    let root = fs::canonicalize(&project.path).expect("canonicalize the project");
    fs::write(root.join("AGENTS.md"), "# Notes\n").expect("seed AGENTS.md");
    run_init_in(
        &project.path,
        &home.path,
        &["--agents", "agents", "cursor", "kiro"],
    );
    let cursor_mcp = root.join(".cursor/mcp.json");
    let mut mcp = read_json(&cursor_mcp);
    mcp["mcpServers"]["theirs"] = serde_json::json!({"command": "theirs"});
    fs::write(&cursor_mcp, mcp.to_string()).expect("add a foreign server");
    let settings = "{\"model\":\"x\"}";
    fs::create_dir_all(root.join(".claude")).expect("mkdir .claude");
    fs::write(root.join(".claude/settings.json"), settings).expect("seed settings");
    let user_mcp = "{ \"mcpServers\": { \"theirs\": { \"command\": \"t\" } } }";
    fs::write(root.join(".mcp.json"), user_mcp).expect("seed .mcp.json");

    let stderr = run_init_in(&project.path, &home.path, &["--agents", "kiro"]);
    assert_eq!(
        fs::read_to_string(root.join("AGENTS.md")).expect("read AGENTS.md"),
        "# Notes\n",
        "only the fenced block may leave AGENTS.md:\n{stderr}"
    );
    let after = read_json(&cursor_mcp);
    assert!(after["mcpServers"]["theirs"].is_object());
    assert!(after["mcpServers"].get("sieve").is_none());
    assert!(!root.join(".cursor/rules/sieve.mdc").exists());
    assert_eq!(
        fs::read_to_string(root.join(".claude/settings.json")).expect("read settings"),
        settings
    );
    assert_eq!(
        fs::read_to_string(root.join(".mcp.json")).expect("read .mcp.json"),
        user_mcp
    );
    for line in stderr.lines().filter(|l| l.starts_with("- removed")) {
        assert!(
            !line.contains("settings.json") && !line.contains("/.mcp.json"),
            "retract must not report a user file: {line}"
        );
    }
}

/// P4-49: the `--dry-run` plan lists writes only and never retractions, and
/// removes nothing. `--dry-run` with a smaller selection leaves the Cursor
/// files in place.
#[test]
fn test_p4_49_dry_run_removes_nothing_and_plans_no_removal() {
    let project = TempDir::new("p4-49-dry-project");
    let home = TempDir::new("p4-49-dry-home");
    run_init_in(&project.path, &home.path, &["--agents", "claude", "cursor"]);
    let before = snapshot_tree(&project.path);

    let stderr = run_init_in(
        &project.path,
        &home.path,
        &["--agents", "claude", "--dry-run"],
    );
    assert!(
        stderr.contains("would set up"),
        "plan text missing:\n{stderr}"
    );
    assert!(
        !stderr.contains("removed") && !stderr.contains("not selected"),
        "the plan must list no removal:\n{stderr}"
    );
    assert_eq!(
        snapshot_tree(&project.path),
        before,
        "dry run changed files"
    );
}

/// Runs `sieve <args>` under the default `sieve` product in `project`,
/// with a temp `home`. Never passes `--global`. Asserts a zero exit.
fn run_sieve_product(project: &Path, home: &Path, args: &[&str]) -> String {
    assert!(
        !home.starts_with("/Users/iamalvisng"),
        "the scratch HOME must never be the real HOME"
    );
    let output = support::sieve_command()
        .args(args)
        .current_dir(project)
        .env("HOME", home)
        .env("NO_COLOR", "1")
        .env("DO_NOT_TRACK", "1")
        .output()
        .expect("run sieve");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(output.status.code(), Some(0), "sieve failed: {stderr}");
    stderr
}

/// Seeds `files` (relative path, bytes) in a fresh project, then runs the
/// retract step (`init --agents agents`) and `uninstall -y`. Asserts every
/// seed stays byte for byte after each command.
fn assert_user_files_survive(label: &str, files: &[(&str, &str)], uninstall_too: bool) {
    let project = TempDir::new(&format!("p4-49-{label}-project"));
    let home = TempDir::new(&format!("p4-49-{label}-home"));
    for (rel, text) in files {
        let path = project.path.join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(&path, text).expect("seed");
    }
    let check = |step: &str, stderr: &str| {
        for (rel, text) in files {
            assert_eq!(
                fs::read_to_string(project.path.join(rel)).expect("seed must survive"),
                *text,
                "{rel} changed after {step}:\n{stderr}"
            );
        }
    };
    let stderr = run_sieve_product(
        &project.path,
        &home.path,
        &["init", "--no-build", "--agents", "agents"],
    );
    check("init", &stderr);
    if uninstall_too {
        let stderr = run_sieve_product(&project.path, &home.path, &["uninstall", "-y"]);
        check("uninstall -y", &stderr);
    }
}

/// P4-49 F1: an `mcpServers` bucket with no Sieve key stays, even when it
/// is empty.
#[test]
fn test_p4_49_empty_mcp_bucket_without_our_key_survives() {
    assert_user_files_survive(
        "f1",
        &[
            (".mcp.json", "{\"mcpServers\":{}}"),
            (".cursor/mcp.json", "{\"mcpServers\":{}}"),
        ],
        true,
    );
}

/// P4-49 F2: a user hook `my-sieve hook x` and an empty `permissions`
/// object stay.
#[test]
fn test_p4_49_user_claude_settings_survive() {
    let seed = "{\"permissions\":{},\"hooks\":{\"Stop\":[{\"hooks\":[{\"type\":\"command\",\"command\":\"my-sieve hook stop\"}]}]}}";
    assert_user_files_survive("f2", &[(".claude/settings.json", seed)], true);
}

/// P4-49 F4: a commented TOML header and a marker quoted inside a line are
/// not Sieve's wiring (the strip anchors on whole trimmed lines).
#[test]
fn test_p4_49_quoted_headers_and_markers_survive() {
    assert_user_files_survive(
        "f4",
        &[
            (
                ".grok/config.toml",
                "# [mcp_servers.sieve]\nx=1\n[other]\ny=2",
            ),
            (
                "GEMINI.md",
                "see <!-- sieve:start --> and\nthen\n<!-- sieve:end -->\n",
            ),
        ],
        true,
    );
}

/// P4-49 F7: a removed file prunes the empty directories it leaves. A directory
/// that holds another file stays, and the repo root stays.
#[test]
fn test_p4_49_retract_prunes_empty_dirs_only() {
    let project = TempDir::new("p4-49-f7-project");
    let home = TempDir::new("p4-49-f7-home");
    run_sieve_product(
        &project.path,
        &home.path,
        &["init", "--no-build", "--agents", "adal", "kiro"],
    );
    fs::write(project.path.join(".kiro/steering/mine.md"), "mine\n").expect("seed");
    run_sieve_product(
        &project.path,
        &home.path,
        &["init", "--no-build", "--agents", "windsurf"],
    );
    assert!(
        !project.path.join(".cursor").exists(),
        ".cursor left behind"
    );
    assert!(!project.path.join(".kiro/settings").exists());
    assert!(project.path.join(".kiro/steering/mine.md").is_file());
    assert!(project.path.is_dir());
}

/// Seeds `rel` with `seed`, runs the retract step and `uninstall -y` (each
/// in a fresh project), and asserts the file equals `expected`. The
/// expected text is what a recorded run wrote for the same seed in the
/// scratch compare: key order kept, `null` kept, `1.50` printed `1.5`.
fn assert_strip_keeps_order(label: &str, rel: &str, seed: &str, expected: &str) {
    for (mode, args) in [
        (
            "init",
            ["init", "--no-build", "--agents", "agents"].as_slice(),
        ),
        ("uninstall", ["uninstall", "-y"].as_slice()),
    ] {
        let project = TempDir::new(&format!("p4-49-{label}-{mode}-project"));
        let home = TempDir::new(&format!("p4-49-{label}-{mode}-home"));
        let path = project.path.join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(&path, seed).expect("seed");
        run_sieve_product(&project.path, &home.path, args);
        assert_eq!(
            fs::read_to_string(&path).expect("read the stripped file"),
            expected,
            "{rel} after {mode}"
        );
    }
}

/// P4-49: a stripped `.mcp.json` keeps the user's key order, a `null`, and a
/// float, and loses only the Sieve server.
#[test]
fn test_p4_49_strip_keeps_mcp_json_key_order() {
    assert_strip_keeps_order(
        "order-mcp",
        ".mcp.json",
        r#"{"zeta":1.50,"mcpServers":{"zed":{"command":"z","n":null},"sieve":{"command":"sieve","args":["mcp"]}},"alpha":[1,2]}"#,
        "{\n  \"zeta\": 1.5,\n  \"mcpServers\": {\n    \"zed\": {\n      \"command\": \"z\",\n      \"n\": null\n    }\n  },\n  \"alpha\": [\n    1,\n    2\n  ]\n}\n",
    );
}

/// P4-49: a stripped `.claude/settings.json` keeps the user's key order,
/// a `null`, an empty array and an exponent number, and loses only the
/// Sieve hook entry.
#[test]
fn test_p4_49_strip_keeps_settings_json_key_order() {
    assert_strip_keeps_order(
        "order-settings",
        ".claude/settings.json",
        r#"{"zeta":null,"statusLine":{"type":"command","command":"mine"},"hooks":{"Stop":[{"hooks":[{"type":"command","command":"sieve hook stop"}]},{"hooks":[{"type":"command","command":"keep"}]}],"Alpha":[]},"alpha":1.5e3}"#,
        "{\n  \"zeta\": null,\n  \"statusLine\": {\n    \"type\": \"command\",\n    \"command\": \"mine\"\n  },\n  \"hooks\": {\n    \"Stop\": [\n      {\n        \"hooks\": [\n          {\n            \"type\": \"command\",\n            \"command\": \"keep\"\n          }\n        ]\n      }\n    ]\n  },\n  \"alpha\": 1500\n}\n",
    );
}

/// The top-level keys of pretty JSON text, in file order.
fn top_keys(text: &str) -> Vec<String> {
    text.lines()
        .filter(|l| l.starts_with("  \"") && l.contains("\":"))
        .filter_map(|l| l.trim_start().split('"').nth(1).map(str::to_string))
        .collect()
}

/// Seeds `seed` at `rel` (under the project or the home), runs `init` with
/// `args` twice under the sieve product with `--global`, and asserts the
/// file keeps the user's key order, `null`, `1.50` as `1.5`, `1.5e3` as
/// `1500`, an empty array and the foreign hook, then adds the managed keys
/// (the settings and hooks merges).
fn assert_merge_keeps_user_values(
    label: &str,
    in_home: bool,
    rel: &str,
    seed: &str,
    args: &[&str],
    keys: &[&str],
) {
    let project = TempDir::new(&format!("p4-49-{label}-project"));
    let home = TempDir::new(&format!("p4-49-{label}-home"));
    let path = if in_home { &home.path } else { &project.path }.join(rel);
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(&path, seed).expect("seed");
    let mut full = vec!["init", "--no-build", "--global"];
    full.extend_from_slice(args);
    run_sieve_product(&project.path, &home.path, &full);
    let first = fs::read_to_string(&path).expect("read the merged file");
    run_sieve_product(&project.path, &home.path, &full);
    let second = fs::read_to_string(&path).expect("read the merged file again");
    assert_eq!(first, second, "a repeat run must not change {rel}");
    assert_eq!(top_keys(&first), keys, "key order in {rel}:\n{first}");
    for text in [
        "\"zeta\": null",
        "\"n\": 1.5",
        "\"alpha\": 1500",
        "\"command\": \"keep\"",
    ] {
        if seed.contains(text.split(':').next().unwrap_or("")) || text.contains("keep") {
            assert!(first.contains(text), "{rel} lost {text}:\n{first}");
        }
    }
    assert!(
        !first.contains("1.50") && !first.contains("1.5e3"),
        "{first}"
    );
}

const USER_SETTINGS: &str = r#"{"zeta":null,"model":"x","permissions":{"deny":["a"],"n":1.50},"hooks":{"Stop":[{"hooks":[{"type":"command","command":"keep"}]}],"Alpha":[]},"alpha":1.5e3,"empty":[]}"#;

/// P4-49: `init --agents claude` merges into a user's `.claude/settings.json`
/// without sorting keys or turning `null` or a float into another value.
#[test]
fn test_p4_49_settings_merge_keeps_user_keys_and_values() {
    assert_merge_keeps_user_values(
        "merge-settings",
        false,
        ".claude/settings.json",
        USER_SETTINGS,
        &["--agents", "claude"],
        &[
            "zeta",
            "model",
            "permissions",
            "hooks",
            "alpha",
            "empty",
            "statusLine",
            "subagentStatusLine",
            "footerLinksRegexes",
        ],
    );
}

/// P4-49: the global hooks merge into `~/.claude/settings.json` keeps the
/// user's keys and values the same way.
#[test]
fn test_p4_49_global_hooks_merge_keeps_user_keys_and_values() {
    assert_merge_keeps_user_values(
        "merge-global",
        true,
        ".claude/settings.json",
        USER_SETTINGS,
        &["--agents", "claude"],
        &["zeta", "model", "permissions", "hooks", "alpha", "empty"],
    );
}

/// P4-49: the Codex hooks merge into `~/.codex/hooks.json` keeps the user's
/// keys and values. The four events sit under the `hooks` key, where `init`
/// writes them.
#[test]
fn test_p4_49_codex_hooks_merge_keeps_user_keys_and_values() {
    assert_merge_keeps_user_values(
        "merge-codex",
        true,
        ".codex/hooks.json",
        r#"{"zeta":null,"hooks":{"Stop":[{"hooks":[{"type":"command","command":"keep"}]}],"Alpha":[]},"alpha":1.5e3}"#,
        &["--agents", "agents"],
        &["zeta", "hooks", "alpha"],
    );
}

/// P4-49: an `mcpServers` value that is not an object stays, and `init`
/// prints the skip line, and leaves the file as it is for both seeds).
#[test]
fn test_p4_49_mcp_merge_skips_a_non_object_bucket() {
    for (label, seed) in [
        ("arr", "{\"mcpServers\":[1,2]}"),
        ("str", "{\"mcpServers\":\"keep me\"}"),
    ] {
        let project = TempDir::new(&format!("p4-49-bucket-{label}-project"));
        let home = TempDir::new(&format!("p4-49-bucket-{label}-home"));
        fs::write(project.path.join(".mcp.json"), seed).expect("seed");
        let stderr = run_sieve_product(
            &project.path,
            &home.path,
            &["init", "--no-build", "--agents", "claude"],
        );
        assert_eq!(
            fs::read_to_string(project.path.join(".mcp.json")).expect("read"),
            seed
        );
        assert!(
            stderr
                .contains("left unchanged (not valid JSON) \u{2014} add the sieve server manually"),
            "{label}: skip line missing:\n{stderr}"
        );
    }
}

/// P4-49: a wrong-typed `hooks`, `permissions`, `statusLine` or hook event
/// is the user's value. The three merges skip them (the codex merge
/// does the same; a plain Claude merge would spread
/// or replace the value, which loses it).
#[test]
fn test_p4_49_settings_merges_skip_wrong_shapes() {
    let project = TempDir::new("p4-49-shape-project");
    let home = TempDir::new("p4-49-shape-home");
    let files = [
        (
            &project.path,
            ".claude/settings.json",
            "{\"hooks\":\"keep me\"}",
        ),
        (&home.path, ".claude/settings.json", "{\"hooks\":[1]}"),
        (
            &home.path,
            ".codex/hooks.json",
            "{\"hooks\":{\"Stop\":\"x\"}}",
        ),
    ];
    for (base, rel, seed) in files {
        let path = base.join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(path, seed).expect("seed");
    }
    run_sieve_product(
        &project.path,
        &home.path,
        &[
            "init",
            "--no-build",
            "--global",
            "--agents",
            "claude",
            "agents",
        ],
    );
    for (base, rel, seed) in files {
        assert_eq!(
            fs::read_to_string(base.join(rel)).expect("read"),
            seed,
            "{rel}"
        );
    }
}

/// P4-50: a foreign tool's hook entries in `~/.codex/hooks.json` are not
/// Sieve's. A sieve merge and a sieve uninstall keep them.
#[test]
fn test_p4_50_sieve_codex_merge_and_strip_keep_foreign_entries() {
    let project = TempDir::new("p4-50-foreign-project");
    let home = TempDir::new("p4-50-foreign-home");
    let seed = serde_json::to_string_pretty(&serde_json::json!({
        "hooks": { "Stop": [{ "hooks": [{
            "type": "command",
            "command": "node \"/h/.codex/hooks/other/other-hooks.cjs\" stop",
            "timeout": 10000
        }] }] }
    }))
    .expect("json")
        + "\n";
    let path = home.path.join(".codex").join("hooks.json");
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(&path, &seed).expect("seed");

    run_sieve_product(
        &project.path,
        &home.path,
        &["init", "--no-build", "--global", "--agents", "agents"],
    );
    let merged = read_json(&path);
    let stop = merged["hooks"]["Stop"].as_array().expect("Stop array");
    assert_eq!(stop.len(), 2, "foreign entry plus sieve entry");
    assert!(stop[0]["hooks"][0]["command"]
        .as_str()
        .expect("command")
        .contains("other-hooks.cjs"));

    run_sieve_product(&project.path, &home.path, &["uninstall", "-y", "--global"]);
    assert_eq!(fs::read_to_string(&path).expect("read"), seed);
}

/// P4-49: a statusLine that is any truthy foreign value stays, here a
/// bare string.
#[test]
fn test_p4_49_foreign_string_statusline_stays() {
    let project = TempDir::new("p4-49-sl-project");
    let home = TempDir::new("p4-49-sl-home");
    let path = project.path.join(".claude/settings.json");
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(&path, "{\"statusLine\":\"mine\"}").expect("seed");
    run_sieve_product(
        &project.path,
        &home.path,
        &["init", "--no-build", "--agents", "claude"],
    );
    let text = fs::read_to_string(&path).expect("read");
    assert!(text.contains("\"statusLine\": \"mine\""), "{text}");
}

/// P4-49: after `init` retract and `uninstall -y --global`, the empty
/// `~/.claude` and `~/.codex` are pruned and HOME stays. A directory with
/// another file stays.
#[test]
fn test_p4_49_global_removal_prunes_empty_home_dirs() {
    let project = TempDir::new("p4-49-prune-project");
    let home = TempDir::new("p4-49-prune-home");
    fs::create_dir_all(home.path.join(".codex")).expect("mkdir .codex");
    let init = ["init", "--no-build", "--global", "--agents"];
    let mut both = init.to_vec();
    both.extend(["claude", "agents"]);
    run_sieve_product(&project.path, &home.path, &both);
    assert!(home.path.join(".claude/settings.json").is_file());

    // The retract step: `claude` leaves the selection.
    let mut only_agents = init.to_vec();
    only_agents.push("agents");
    run_sieve_product(&project.path, &home.path, &only_agents);
    assert!(!home.path.join(".claude").exists(), "~/.claude left behind");

    run_sieve_product(&project.path, &home.path, &["uninstall", "-y", "--global"]);
    assert!(!home.path.join(".codex").exists(), "~/.codex left behind");
    assert!(home.path.is_dir());

    fs::create_dir_all(home.path.join(".claude")).expect("mkdir");
    fs::write(home.path.join(".claude/mine.txt"), "x").expect("seed");
    run_sieve_product(&project.path, &home.path, &both);
    run_sieve_product(&project.path, &home.path, &only_agents);
    assert!(home.path.join(".claude/mine.txt").is_file());
}

/// A scratch HOME with the Codex and opencode config dirs, which `init`
/// probes before it writes their MCP entries.
fn home_with_codex_and_opencode(label: &str) -> TempDir {
    let home = TempDir::new(label);
    fs::create_dir_all(home.path.join(".codex")).expect("mkdir ~/.codex");
    fs::create_dir_all(home.path.join(".config/opencode")).expect("mkdir ~/.config/opencode");
    home
}

/// P4-51: `--agents agents --global` writes `~/.codex/config.toml` and
/// `<repo>/opencode.json`. `init` prints `mcp codex`, `mcp opencode`, then
/// `hook codex-hooks`, in that order. Without
/// `--global` the `HOME` file is skipped. Without the opencode dir, no
/// `opencode.json`. Leaving `agents` out of a later `init` retracts both.
#[test]
fn test_p4_51_agents_write_the_codex_config_and_opencode_json() {
    let project = TempDir::new("p4-51-project");
    let home = home_with_codex_and_opencode("p4-51-home");
    let global = ["--agents", "agents", "--global"];
    let stderr = run_init_in(&project.path, &home.path, &global);

    let root = fs::canonicalize(&project.path).expect("canonical project");
    let toml = home.path.join(".codex/config.toml");
    let json = root.join("opencode.json");
    let hooks = home.path.join(".codex/hooks.json");
    let lines = [
        format!("✓ mcp codex: {} (created)", toml.display()),
        format!("✓ mcp opencode: {} (created)", json.display()),
        format!("✓ hook codex-hooks: {} (created)", hooks.display()),
    ];
    let at: Vec<usize> = lines
        .iter()
        .map(|l| {
            stderr
                .find(l.as_str())
                .unwrap_or_else(|| panic!("no `{l}`:\n{stderr}"))
        })
        .collect();
    assert!(
        at.windows(2).all(|w| w[0] < w[1]),
        "report order:\n{stderr}"
    );
    assert_eq!(
        fs::read_to_string(&toml).expect("read config.toml"),
        "[mcp_servers.sieve]\ncommand = \"sieve\"\nargs = [\"mcp\"]\n"
    );
    assert_eq!(
        fs::read_to_string(&json).expect("read opencode.json"),
        "{\n  \"mcp\": {\n    \"sieve\": {\n      \"type\": \"local\",\n      \"command\": [\n        \"sieve\",\n        \"mcp\"\n      ],\n      \"enabled\": true\n    }\n  }\n}\n"
    );

    // A repeat run reports `unchanged`.
    let again = run_init_in(&project.path, &home.path, &global);
    assert!(again.contains(&format!("✓ mcp codex: {} (unchanged)", toml.display())));
    assert!(again.contains(&format!(
        "✓ hook codex-hooks: {} (unchanged)",
        hooks.display()
    )));

    // The retract step: `agents` leaves the selection.
    let stderr = run_init_in(&project.path, &home.path, &["--agents", "kiro", "--global"]);
    for line in [
        format!(
            "- removed {} ([mcp_servers.sieve]) — agent not selected",
            toml.display()
        ),
        format!(
            "- removed {} (mcp.sieve) — agent not selected",
            json.display()
        ),
    ] {
        assert!(stderr.contains(&line), "no `{line}`:\n{stderr}");
    }
    assert!(!toml.exists() && !json.exists());

    // No `--global`: opencode.json only, and the hint line.
    let local = TempDir::new("p4-51-local-project");
    let stderr = run_init_in(&local.path, &home.path, &["--agents", "agents"]);
    assert!(!toml.exists(), "HOME file written without --global");
    assert!(local.path.join("opencode.json").is_file());
    assert!(stderr.contains("skipped global hooks under ~ \u{b7} pass --global to write them"));

    // No opencode dir: no opencode.json.
    let bare = TempDir::new("p4-51-bare-home");
    let bare_project = TempDir::new("p4-51-bare-project");
    run_init_in(&bare_project.path, &bare.path, &global);
    assert!(!bare_project.path.join("opencode.json").exists());
}

/// P4-51: a user's own Codex tables and opencode servers stay, in their
/// order. A later `init` that drops `agents` gives the user's bytes back.
#[test]
fn test_p4_51_codex_and_opencode_merges_keep_user_content() {
    let project = TempDir::new("p4-51-keep-project");
    let home = home_with_codex_and_opencode("p4-51-keep-home");
    let user_toml = "[mcp_servers.mine]\ncommand = \"foo\"\n\n[other]\nx = 1\n";
    let user_json = r#"{"theme":"dark","mcp":{"mine":{"type":"local","command":["foo"]}}}"#;
    fs::write(home.path.join(".codex/config.toml"), user_toml).expect("seed toml");
    fs::write(project.path.join("opencode.json"), user_json).expect("seed json");

    let stderr = run_init_in(
        &project.path,
        &home.path,
        &["--agents", "agents", "--global"],
    );
    assert!(stderr.contains("/config.toml (updated)"), "{stderr}");
    assert!(stderr.contains("/opencode.json (updated)"), "{stderr}");
    let toml = fs::read_to_string(home.path.join(".codex/config.toml")).expect("read toml");
    assert_eq!(
        toml,
        format!("{user_toml}\n[mcp_servers.sieve]\ncommand = \"sieve\"\nargs = [\"mcp\"]\n")
    );

    run_init_in(&project.path, &home.path, &["--agents", "kiro", "--global"]);
    let back = fs::read_to_string(home.path.join(".codex/config.toml")).expect("read toml");
    assert_eq!(back, user_toml);
    let want: serde_json::Value = serde_json::from_str(user_json).expect("parse");
    assert_eq!(read_json(&project.path.join("opencode.json")), want);
}

/// P4-52: the report names the action per writer. A section appended to
/// a user file says `appended`. A merge into a user JSON file says
/// `updated`. The Codex hooks file prints a `hook codex-hooks` line.
#[test]
fn test_p4_52_report_words_follow_golden() {
    let project = TempDir::new("p4-52-project");
    let home = home_with_codex_and_opencode("p4-52-home");
    fs::write(project.path.join("AGENTS.md"), "# My rules\n\nkeep me\n").expect("seed");
    let mine = r#"{"mcpServers":{"mine":{"command":"foo"}}}"#;
    fs::write(project.path.join(".mcp.json"), mine).expect("seed");
    let stop = r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"mine"}]}]}}"#;
    fs::write(home.path.join(".codex/hooks.json"), stop).expect("seed");

    let args = ["--agents", "claude", "agents", "--global"];
    let stderr = run_init_in(&project.path, &home.path, &args);
    let root = fs::canonicalize(&project.path).expect("canonical project");
    for line in [
        format!("✓ agents: {} (appended)", root.join("AGENTS.md").display()),
        format!(
            "✓ mcp claude: {} (updated) — restart Claude Code to load the sieve MCP server",
            root.join(".mcp.json").display()
        ),
        format!(
            "✓ hook codex-hooks: {} (updated)",
            home.path.join(".codex/hooks.json").display()
        ),
    ] {
        assert!(stderr.contains(&line), "no `{line}`:\n{stderr}");
    }
}

/// Runs `sieve uninstall` in `project` with `home` as `$HOME`. Returns
/// the stderr text without the update nudge.
fn run_uninstall_in(project: &Path, home: &Path, args: &[&str]) -> String {
    assert!(
        !home.starts_with("/Users/iamalvisng"),
        "the scratch HOME must never be the real HOME"
    );
    let output = support::sieve_command()
        .arg("uninstall")
        .args(args)
        .current_dir(project)
        .env("HOME", home)
        .env("NO_COLOR", "1")
        .env("DO_NOT_TRACK", "1")
        .output()
        .expect("run sieve uninstall");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(
        output.status.code(),
        Some(0),
        "sieve uninstall failed: {stderr}"
    );
    let kept: Vec<&str> = stderr.lines().filter(|l| !l.contains('⬆')).collect();
    kept.join("\n")
}

/// The expected `uninstall` report for a repo wired with every agent,
/// with a user's `AGENTS.md`, `.mcp.json`, `opencode.json` and Codex
/// `config.toml`. `{R}` is the repo and `{H}` is `HOME`.
/// The shims are left out: Sieve writes no shim (ledger
/// 2026-09-12).
const GOLDEN_UNINSTALL_REPORT: &str = "agents:
  ~ would remove: {R}/AGENTS.md (fenced sieve section)
  ~ would remove: {H}/.codex/config.toml [machine-wide] ([mcp_servers.sieve])
  ~ would remove: {R}/opencode.json (mcp.sieve)
  - would remove: {H}/.codex/hooks.json [machine-wide] (SessionStart / UserPromptSubmit / PostToolUse / Stop — deleted)

adal:
  - would remove: {R}/.adal/skills/sieve/SKILL.md (sieve-owned instruction file — deleted)

cursor:
  - would remove: {R}/.cursor/rules/sieve.mdc (sieve-owned instruction file — deleted)
  - would remove: {R}/.cursor/mcp.json (mcpServers.sieve — deleted)

gemini:
  - would remove: {R}/GEMINI.md (fenced sieve section — deleted)
  - would remove: {R}/.gemini/settings.json (mcpServers.sieve — deleted)

grok:
  - would remove: {R}/.grok/skills/sieve/SKILL.md (sieve-owned instruction file — deleted)
  - would remove: {R}/.grok/config.toml ([mcp_servers.sieve] — deleted)

copilot:
  - would remove: {R}/.github/copilot-instructions.md (fenced sieve section — deleted)

kiro:
  - would remove: {R}/.kiro/steering/sieve.md (sieve-owned instruction file — deleted)
  - would remove: {R}/.kiro/settings/mcp.json (mcpServers.sieve — deleted)

windsurf:
  - would remove: {R}/.windsurf/rules/sieve.md (sieve-owned instruction file — deleted)

antigravity:
  - would remove: {H}/.gemini/config/mcp_config.json [machine-wide] (mcpServers.sieve — deleted)
  - would remove: {H}/.gemini/skills/sieve/SKILL.md [machine-wide] (sieve skill (shared) — deleted)

claude:
  - would remove: {R}/.claude/settings.json (statusline + hooks + allowlist + footer regex — deleted)
  - would remove: {R}/.claude/skills/sieve/SKILL.md (sieve skill — deleted)
  ~ would remove: {R}/.mcp.json (mcpServers.sieve)
  - would remove: {H}/.claude/settings.json [machine-wide] (SessionStart / UserPromptSubmit / PostToolUse / Stop — deleted)
  - would remove: {H}/.claude.json [machine-wide] (mcpServers.sieve — deleted)";

/// P1-72: the dry run groups lines by host and says `would remove`, then
/// the two closing lines. `-y` says `removed` and ends with the product
/// line. Both equal the expected text. The dry run removes nothing.
#[test]
fn test_p1_72_uninstall_report_matches_golden() {
    let project = TempDir::new("p1-72-project");
    let home = home_with_codex_and_opencode("p1-72-home");
    fs::create_dir_all(home.path.join(".claude")).expect("mkdir ~/.claude");
    fs::write(project.path.join("AGENTS.md"), "# My rules\n\nkeep me\n").expect("seed");
    let mine = r#"{"mcpServers":{"mine":{"command":"foo"}}}"#;
    fs::write(project.path.join(".mcp.json"), mine).expect("seed");
    let oc = r#"{"mcp":{"mine":{"type":"local","command":["foo"]}}}"#;
    fs::write(project.path.join("opencode.json"), oc).expect("seed");
    let toml = "[mcp_servers.mine]\ncommand = \"foo\"\n";
    fs::write(home.path.join(".codex/config.toml"), toml).expect("seed");
    run_init_in(&project.path, &home.path, &["--all-agents", "--global"]);

    let root = fs::canonicalize(&project.path).expect("canonical project");
    let fill = |text: &str| {
        text.replace("{R}", &root.display().to_string())
            .replace("{H}", &home.path.display().to_string())
    };
    let before = snapshot_tree(&root);
    let dry = run_uninstall_in(&project.path, &home.path, &["--global"]);
    let closing = "dry run \u{b7} nothing removed \u{b7} run sieve uninstall -y to remove\nentries marked [machine-wide] affect every project \u{b7} --no-global skips them";
    assert_eq!(
        dry,
        format!("{}\n\n{closing}", fill(GOLDEN_UNINSTALL_REPORT))
    );
    assert_eq!(before, snapshot_tree(&root), "the dry run changed the repo");

    let done = run_uninstall_in(&project.path, &home.path, &["-y", "--global"]);
    let removed = GOLDEN_UNINSTALL_REPORT.replace("would remove", "removed");
    let last = "✓ sieve removed \u{b7} run sieve init to set it up again";
    assert_eq!(done, format!("{}\n\n{last}", fill(&removed)));
    // The user's own content stays.
    let agents = fs::read_to_string(project.path.join("AGENTS.md")).expect("read");
    assert_eq!(agents, "# My rules\n\nkeep me\n");
    let kept = fs::read_to_string(home.path.join(".codex/config.toml")).expect("read");
    assert_eq!(kept, toml);

    // A repo with nothing of ours: the one line.
    let empty = TempDir::new("p1-72-empty-project");
    let quiet = TempDir::new("p1-72-empty-home");
    let dry = run_uninstall_in(&empty.path, &quiet.path, &["--no-global"]);
    let want = "nothing to remove \u{2014} no sieve files found here\n\ndry run \u{b7} nothing removed \u{b7} run sieve uninstall -y to remove";
    assert_eq!(dry, want);
}

/// P1-72: under the sieve product, `build` writes a `sieve/` block into
/// `.gitignore` and `.ignore`, and `uninstall -y` removes both blocks and
/// keeps every user line. The dry run lists both files.
#[test]
fn test_p1_72_uninstall_strips_the_sieve_ignore_blocks_and_keeps_user_lines() {
    let project = TempDir::new("p1-72-ignore-project");
    let home = TempDir::new("p1-72-ignore-home");
    let fixture = support::manifest_dir().join("../../tests/fixtures/basic");
    copy_dir(&fixture, &project.path);
    let user = "node_modules/\nx# sieve's local graph cache\n";
    let gi = project.path.join(".gitignore");
    let ig = project.path.join(".ignore");
    fs::write(&gi, user).expect("seed");
    fs::write(&ig, "mine\n").expect("seed");
    run_sieve_product(&project.path, &home.path, &["build"]);
    let built = fs::read_to_string(&gi).expect("read");
    assert!(built.contains("\n/sieve/\n"), "no sieve block:\n{built}");
    assert!(fs::read_to_string(&ig).expect("read").contains("!sieve/"));

    let dry = run_sieve_product(&project.path, &home.path, &["uninstall"]);
    assert!(dry.contains(".gitignore (sieve/ ignore entry)"), "{dry}");
    assert!(
        dry.contains(".ignore (sieve/ search re-admit entries)"),
        "{dry}"
    );
    run_sieve_product(&project.path, &home.path, &["uninstall", "-y"]);
    assert_eq!(fs::read_to_string(&gi).expect("read"), user);
    assert_eq!(fs::read_to_string(&ig).expect("read"), "mine\n");
    assert!(!project.path.join("sieve").exists());
}

/// Runs `init` with no `--verbose` in a fresh project and returns stderr.
fn run_init_compact(project: &Path, home: &Path, args: &[&str]) -> String {
    assert!(
        !home.starts_with("/Users/iamalvisng"),
        "the scratch HOME must never be the real HOME"
    );
    let output = support::sieve_command()
        .arg("init")
        .args(args)
        .current_dir(project)
        .env("HOME", home)
        .env("NO_COLOR", "1")
        .env("DO_NOT_TRACK", "1")
        .output()
        .expect("run sieve init");
    assert!(output.stdout.is_empty(), "init wrote to stdout");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(output.status.code(), Some(0), "init failed: {stderr}");
    stderr
}

/// P4-47, P4-10: `init` prints the compact report by default. One graph
/// line, one line per agent (Claude Code first), then two epilogue lines.
/// No per-file line and no banner appear.
#[test]
fn test_p4_47_init_compact_output() {
    let project = TempDir::new("p4-47-compact-project");
    let home = TempDir::new("p4-47-compact-home");
    let stderr = run_init_compact(
        &project.path,
        &home.path,
        &["--no-build", "--agents", "cursor", "claude"],
    );
    let want = "\
sieve set up Cursor, Claude Code in this repo
graph build skipped \u{2014} run sieve build
Cursor       .cursor/rules/sieve.mdc, .cursor/mcp.json +1 more
Claude Code  .claude/, .mcp.json
restart your agents so a new session picks up sieve
commit .claude/ .mcp.json .cursor/ to share it \u{b7} sieve/ stays local and git-ignored
done \u{b7} wrote 6 files in this repo \u{b7} nothing outside this repo was written
";
    let lines: String = stderr
        .lines()
        .filter(|l| !l.contains('\u{2b06}'))
        .map(|l| format!("{l}\n"))
        .collect();
    assert_eq!(lines, want);
}

/// P4-47: `--agents` keeps the order the user gave
/// (`--agents agents claude` prints agents, then claude). The
/// commit line follows the plan order, Claude Code first.
#[test]
fn test_p4_47_init_compact_keeps_the_agents_order() {
    let project = TempDir::new("p4-47-order-project");
    let home = TempDir::new("p4-47-order-home");
    let stderr = run_init_compact(
        &project.path,
        &home.path,
        &["--no-build", "--agents", "agents", "claude"],
    );
    let want = "\
sieve set up AGENTS.md, Claude Code in this repo
graph build skipped \u{2014} run sieve build
AGENTS.md    AGENTS.md
Claude Code  .claude/, .mcp.json
restart your agents so a new session picks up sieve
commit .claude/ .mcp.json AGENTS.md to share it \u{b7} sieve/ stays local and git-ignored
done \u{b7} wrote 4 files in this repo \u{b7} nothing outside this repo was written
";
    let lines: String = stderr
        .lines()
        .filter(|l| !l.contains('\u{2b06}'))
        .map(|l| format!("{l}\n"))
        .collect();
    assert_eq!(lines, want);
}

/// P4-47: `--all-agents` lists Claude Code first, then the registry order
/// (claude, agents, adal, cursor, ...).
#[test]
fn test_p4_47_init_compact_all_agents_lists_claude_first() {
    let project = TempDir::new("p4-47-all-project");
    let home = TempDir::new("p4-47-all-home");
    let stderr = run_init_compact(&project.path, &home.path, &["--no-build", "--all-agents"]);
    let hosts: Vec<&str> = stderr
        .lines()
        .skip(2)
        .take_while(|l| !l.starts_with("restart your agents"))
        .filter_map(|l| l.split("  ").next())
        .collect();
    assert_eq!(&hosts[..4], ["Claude Code", "AGENTS.md", "AdaL", "Cursor"]);
    assert!(
        stderr.starts_with("sieve set up Claude Code, AGENTS.md, AdaL and 8 more in this repo\n"),
        "{stderr}"
    );
}

/// P4-47: `--verbose` brings back the per-file lines and the banner. The
/// compact lines do not appear.
#[test]
fn test_p4_47_init_verbose_prints_every_file() {
    let project = TempDir::new("p4-47-verbose-project");
    let home = TempDir::new("p4-47-verbose-home");
    let stderr = run_init_in(&project.path, &home.path, &["--agents", "claude"]);
    assert!(stderr.contains("\u{2713} wrote "), "{stderr}");
    assert!(stderr.contains("\u{b7} skipped graph build"), "{stderr}");
    assert!(stderr.contains("share it: git add .claude"), "{stderr}");
    assert!(!stderr.contains("restart your agents"), "{stderr}");
}

/// P4-10: a build that fails shows as one warning in the compact report.
/// The agent lines and the epilogue still print.
#[test]
fn test_p4_10_init_compact_reports_a_failed_build() {
    let project = TempDir::new("p4-10-compact-project");
    let home = TempDir::new("p4-10-compact-home");
    fs::write(project.path.join("sieve"), b"").expect("write a plain `sieve` file");
    let stderr = run_init_compact(&project.path, &home.path, &["--agents", "claude"]);
    assert!(
        stderr.contains("\u{26a0} the graph build failed \u{2014} run sieve build to see why"),
        "{stderr}"
    );
    assert!(
        stderr.contains("Claude Code  .claude/, .mcp.json"),
        "{stderr}"
    );
    assert!(stderr.contains("restart your agents"), "{stderr}");
    assert!(!stderr.contains("\u{2713} wrote "), "{stderr}");
}

/// P4-47: the compact report names the product in Sieve mode.
#[test]
fn test_p4_47_init_compact_output_in_sieve_mode() {
    let project = TempDir::new("p4-47-sieve-project");
    let home = TempDir::new("p4-47-sieve-home");
    let stderr = run_sieve_product(
        &project.path,
        &home.path,
        &["init", "--no-build", "--agents", "claude"],
    );
    assert!(
        stderr.contains("graph build skipped \u{2014} run sieve build"),
        "{stderr}"
    );
    assert!(
        stderr.contains("Claude Code  .claude/, .mcp.json"),
        "{stderr}"
    );
    assert!(
        stderr.contains("to share it \u{b7} sieve/ stays local and git-ignored"),
        "{stderr}"
    );
    assert!(
        stderr.ends_with(
            "done \u{b7} wrote 3 files in this repo \u{b7} nothing outside this repo was written\n"
        ),
        "{stderr}"
    );
}

/// Data safety: `uninstall -y` removes only Sieve's own block from `.ignore`
/// and `.gitignore`. A line of the user stays, and so does the file. The
/// `sieve/` dir, with its cache files, goes.
#[test]
fn test_p1_72_uninstall_keeps_the_user_lines_of_the_ignore_files() {
    let project = TempDir::new("p1-72-ignore-project");
    let home = TempDir::new("p1-72-ignore-home");
    let src = support::manifest_dir().join("../../tests/fixtures/basic");
    copy_dir(&src, &project.path);
    fs::write(project.path.join(".ignore"), "my-own-entry\n").expect("seed .ignore");
    fs::write(project.path.join(".gitignore"), "my-git-entry\n").expect("seed .gitignore");
    run_sieve_product(&project.path, &home.path, &["build"]);
    assert!(
        project.path.join("sieve/.cache").is_dir(),
        "build writes the cache"
    );
    let built = fs::read_to_string(project.path.join(".ignore")).expect("read .ignore");
    assert!(
        built.contains("my-own-entry") && built.contains("sieve/"),
        "{built}"
    );
    run_sieve_product(&project.path, &home.path, &["uninstall", "-y"]);
    assert_eq!(
        fs::read_to_string(project.path.join(".ignore")).expect(".ignore stays"),
        "my-own-entry\n"
    );
    assert_eq!(
        fs::read_to_string(project.path.join(".gitignore")).expect(".gitignore stays"),
        "my-git-entry\n"
    );
    assert!(!project.path.join("sieve").exists(), "sieve/ stays");
}
