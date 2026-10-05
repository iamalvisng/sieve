//! F6 slice 1: `sieve why`, `sieve why --check`, the sidecar and the
//! post-read hint, on a small fixture project.

mod support;

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use support::TempDir;

const SRC: &str = "pub fn build_repo() {\n    fail(\"a graph rebuild is already in flight\");\n}\n\npub fn plain() {}\n";

const LEDGER: &str = "# Ledger\n\n\
## 2026-09-01, the first rule\n\n- **Date:** 2026-09-01\n- **Code:** `src/lib.rs#plain`\n- **Decision:** The old rule.\n\n\
## 2026-10-01, build takes a lock\n\n- **Date:** 2026-10-01\n- **Code:** `src/lib.rs#build_repo`, `src/gone.rs#old`\n- **Decision:** The build waits.\n\n\
## 2026-10-02, the second rule\n\n- **Date:** 2026-10-02\n- **Decision:** This reverses the entry \"2026-09-01, the first rule\".\n";

/// A scratch project with one source file and one ledger.
fn project(label: &str) -> TempDir {
    let temp = TempDir::new(label);
    fs::create_dir_all(temp.path.join("src")).expect("mkdir src");
    fs::create_dir_all(temp.path.join("docs/decisions")).expect("mkdir docs");
    fs::write(temp.path.join("src/lib.rs"), SRC).expect("write src");
    fs::write(temp.path.join("docs/decisions/LEDGER.md"), LEDGER).expect("write ledger");
    temp
}

/// Runs the binary in `cwd` with a scratch `HOME`.
fn run(cwd: &Path, args: &[&str], stdin_json: Option<&str>) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sieve"));
    cmd.args(args)
        .current_dir(cwd)
        .env("HOME", cwd.join("home"))
        .env("CLAUDE_PROJECT_DIR", cwd);
    if let Some(json) = stdin_json {
        cmd.env("SIEVE_TEST_STDIN", json);
    }
    cmd.output().expect("run sieve")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn test_f6_build_writes_the_sidecar_and_why_answers() {
    let temp = project("f6-why");
    let built = run(&temp.path, &["build"], None);
    assert!(built.status.success(), "{}", text(&built.stderr));
    assert!(temp.path.join("sieve/.cache/why-refs.json").is_file());

    let out = run(&temp.path, &["why", "build_repo"], None);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let body = text(&out.stdout);
    assert!(body.contains("why build_repo  src/lib.rs:1"), "{body}");
    assert!(body.contains("docs/decisions/LEDGER.md:"), "{body}");
    assert!(body.contains("build takes a lock"), "{body}");
    assert!(body.contains("Code anchor"), "{body}");
    // 8 rows at about 40 tokens each: the answer stays small.
    assert!(body.chars().count() / 4 < 300, "{body}");

    let json = run(&temp.path, &["why", "build_repo", "--json"], None);
    let v: serde_json::Value = serde_json::from_slice(&json.stdout).expect("json");
    assert_eq!(v["symbols"][0]["decisions"][0]["level"], "anchor");
}

#[test]
fn test_f6_check_lists_the_superseded_entry_that_links_to_code() {
    let temp = project("f6-check");
    run(&temp.path, &["build"], None);
    let out = run(&temp.path, &["why", "--check"], None);
    let body = text(&out.stdout);
    assert!(body.contains("the first rule"), "{body}");
    assert!(body.contains("still links: plain"), "{body}");
    assert!(body.contains("STALE ANCHOR"), "{body}");
    assert!(body.contains("src/gone.rs#old"), "{body}");
}

#[test]
fn test_f6_post_read_hint_is_one_short_line_with_in_repo_text() {
    let temp = project("f6-hint");
    run(&temp.path, &["build"], None);
    let file = fs::canonicalize(temp.path.join("src/lib.rs")).expect("canon");
    let stdin = format!(
        r#"{{"session_id":"s1","tool_name":"Read","tool_input":{{"file_path":"{}"}}}}"#,
        file.display()
    );
    let out = run(&temp.path, &["hook", "post-read"], Some(&stdin));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("hook json");
    let note = v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("note");
    assert!(note.starts_with("sieve: "), "{note}");
    assert!(note.contains("sieve why "), "{note}");
    assert!(!note.contains('\n'));
    assert!(note.chars().count() / 4 <= 40, "{note}");
}

/// The stdin JSON of a Read of the fixture source file.
fn read_stdin(temp: &TempDir) -> String {
    let file = fs::canonicalize(temp.path.join("src/lib.rs")).expect("canon");
    format!(
        r#"{{"session_id":"s1","tool_name":"Read","tool_input":{{"file_path":"{}"}}}}"#,
        file.display()
    )
}

/// Runs the post-read hook with a fake `git` first on `PATH`. The fake
/// writes a marker file when it runs. Gives the hook output and whether the
/// marker exists.
fn hook_with_fake_git(temp: &TempDir) -> (Output, bool) {
    use std::os::unix::fs::PermissionsExt;
    let bin = temp.path.join("fakebin");
    fs::create_dir_all(&bin).expect("mkdir");
    let marker = temp.path.join("git-ran");
    let script = bin.join("git");
    fs::write(
        &script,
        format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display()),
    )
    .expect("write fake git");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod");
    let path = format!("{}:/usr/bin:/bin", bin.display());
    let out = Command::new(env!("CARGO_BIN_EXE_sieve"))
        .args(["hook", "post-read"])
        .current_dir(&temp.path)
        .env("HOME", temp.path.join("home"))
        .env("CLAUDE_PROJECT_DIR", &temp.path)
        .env("PATH", path)
        .env("SIEVE_TEST_STDIN", read_stdin(temp))
        .output()
        .expect("run hook");
    (out, marker.exists())
}

/// The ledger with one more entry that names a sha. A rebuild of the
/// sidecar would run git for it.
fn ledger_with_sha() -> String {
    format!("{LEDGER}\n## 2026-10-04, a sha\n\n- **Date:** 2026-10-04\n- In commit abc1234.\n")
}

#[test]
fn test_f6_hook_with_a_missing_sidecar_gives_no_hint_and_runs_no_git() {
    let temp = project("f6-hook-missing");
    fs::write(
        temp.path.join("docs/decisions/LEDGER.md"),
        ledger_with_sha(),
    )
    .expect("write");
    let (out, ran) = hook_with_fake_git(&temp);
    assert_eq!(text(&out.stdout), "");
    assert!(!ran, "the hook ran git");
    assert!(!temp.path.join("sieve/.cache/why-refs.json").exists());
}

#[test]
fn test_f6_hook_with_a_stale_sidecar_gives_no_hint_and_runs_no_git() {
    let temp = project("f6-hook-stale");
    run(&temp.path, &["build"], None);
    assert!(temp.path.join("sieve/.cache/why-refs.json").is_file());
    fs::write(
        temp.path.join("docs/decisions/LEDGER.md"),
        ledger_with_sha(),
    )
    .expect("write");
    let (out, ran) = hook_with_fake_git(&temp);
    assert_eq!(text(&out.stdout), "");
    assert!(!ran, "the hook ran git");
}

/// Runs git in `dir` with a fixed identity and no global config.
fn run_in_repo(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .output()
        .expect("run git");
    assert!(out.status.success(), "{}", text(&out.stderr));
}

#[test]
fn test_f6_suggest_prints_the_ids_of_the_changed_symbols() {
    let temp = project("f6-suggest");
    run_in_repo(&temp.path, &["init", "-q"]);
    run_in_repo(&temp.path, &["add", "src", "docs"]);
    run_in_repo(&temp.path, &["commit", "-q", "-m", "one"]);
    run(&temp.path, &["build"], None);
    fs::write(temp.path.join("src/lib.rs"), SRC.replace("fail(", "fail2(")).expect("write");

    let out = run(&temp.path, &["why", "--suggest", "--no-refresh"], None);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "src/lib.rs#build_repo\n");

    run_in_repo(&temp.path, &["add", "src"]);
    run_in_repo(&temp.path, &["commit", "-q", "-m", "two"]);
    let out = run(
        &temp.path,
        &["why", "--suggest", "HEAD", "--no-refresh"],
        None,
    );
    assert_eq!(text(&out.stdout), "src/lib.rs#build_repo\n");
}

// ---------------------------------------------------------------------------
// Slice 2: reason comments, pinning tests, history and the MCP tool
// ---------------------------------------------------------------------------

const ROWS_SRC: &str = "pub fn build_repo(x: i32) -> Result<i32, String> {\n    // SAFETY: the lock is held here.\n    if x < 0 {\n        return Err(\"negative\".to_string());\n    }\n    // NOTE: never log token=abc123\n    let y = x.checked_add(1).ok_or(\"overflow\")?;\n    Ok(y)\n}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n\n    fn test_build_repo_rejects_a_negative() {\n        let _ = build_repo(-1);\n    }\n}\n";

const ROWS_LEDGER: &str = "# Ledger\n\n## 2026-10-01, build takes a lock\n\n- **Date:** 2026-10-01\n- **Code:** `src/lib.rs#build_repo`\n- **Decision:** The build waits.\n";

/// Runs git in `dir` with a fixed identity and a fixed date.
fn git_at(dir: &Path, date: &str, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .output()
        .expect("run git");
    assert!(out.status.success(), "{}", text(&out.stderr));
}

/// A project whose `build_repo` has a decision, two reason comments, a
/// test and two commits.
fn rows_project(label: &str) -> TempDir {
    let temp = TempDir::new(label);
    fs::create_dir_all(temp.path.join("src")).expect("mkdir src");
    fs::create_dir_all(temp.path.join("docs/decisions")).expect("mkdir docs");
    fs::write(temp.path.join("docs/decisions/LEDGER.md"), ROWS_LEDGER).expect("write ledger");
    fs::write(
        temp.path.join("src/lib.rs"),
        ROWS_SRC.replace("x < 0", "x < 1"),
    )
    .expect("write src");
    let d1 = "2026-01-01T00:00:00Z";
    git_at(&temp.path, d1, &["init", "-q"]);
    git_at(&temp.path, d1, &["add", "src", "docs"]);
    git_at(&temp.path, d1, &["commit", "-q", "-m", "add build_repo"]);
    fs::write(temp.path.join("src/lib.rs"), ROWS_SRC).expect("write src");
    let d2 = "2026-02-03T00:00:00Z";
    git_at(&temp.path, d2, &["add", "src"]);
    let subject = "fix the bound, ghp_abcdefghijklmnop1234";
    git_at(&temp.path, d2, &["commit", "-q", "-m", subject]);
    temp
}

/// Replaces each 7-character commit id with `<sha>`, so the golden does not
/// pin a hash.
fn mask_shas(s: &str) -> String {
    s.split(' ')
        .map(|w| {
            let hex = w.len() == 7 && w.bytes().all(|b| b.is_ascii_hexdigit());
            if hex {
                "<sha>"
            } else {
                w
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

const ROWS_GOLDEN: &str = "why build_repo  src/lib.rs:1
decision  docs/decisions/LEDGER.md:3  2026-10-01, build takes a lock
          link: Code anchor
comment  src/lib.rs:2  SAFETY: the lock is held here.
comment  src/lib.rs:6  NOTE: never log token=[redacted]
test  src/lib.rs:15  test_build_repo_rejects_a_negative
history  introduced  <sha> 2026-01-01  commit subject (untrusted): add build_repo
history  last  <sha> 2026-02-03  commit subject (untrusted): fix the bound, [redacted]
";

#[test]
fn test_f6_golden_why_shows_every_row_type_in_order() {
    let temp = rows_project("f6-golden");
    let built = run(&temp.path, &["build"], None);
    assert!(built.status.success(), "{}", text(&built.stderr));
    let out = run(&temp.path, &["why", "build_repo"], None);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let body = mask_shas(&text(&out.stdout));
    assert_eq!(body, ROWS_GOLDEN);
    assert!(body.chars().count() / 4 <= 400, "{}", body.chars().count());
}

#[test]
fn test_f6_all_adds_the_file_level_row_that_the_default_hides() {
    let temp = rows_project("f6-all");
    let more = "\n## 2026-10-02, names a file\n\n- **Date:** 2026-10-02\n- The file src/lib.rs holds it.\n";
    let ledger = temp.path.join("docs/decisions/LEDGER.md");
    fs::write(&ledger, format!("{ROWS_LEDGER}{more}")).expect("write");
    run(&temp.path, &["build"], None);
    let plain = text(&run(&temp.path, &["why", "build_repo"], None).stdout);
    let all = text(&run(&temp.path, &["why", "build_repo", "--all"], None).stdout);
    assert!(!plain.contains("file-level"), "{plain}");
    assert!(
        all.contains("file-level  docs/decisions/LEDGER.md"),
        "{all}"
    );
    assert!(all.contains("link: names the file"), "{all}");
    assert!(all.contains("history  last"), "{all}");
}

/// Sends NDJSON `lines` to `sieve mcp` in `cwd`. Gives the stdout lines.
fn mcp(cwd: &Path, lines: &[&str]) -> Vec<String> {
    use std::io::Write;
    let path = std::env::var("PATH").unwrap_or_default();
    let mut child = Command::new(env!("CARGO_BIN_EXE_sieve"))
        .arg("mcp")
        .current_dir(cwd)
        .env("HOME", cwd.join("home"))
        .env("CLAUDE_PROJECT_DIR", cwd)
        .env("PATH", format!("{}:{path}", cwd.join("fakebin").display()))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn mcp");
    let mut stdin = child.stdin.take().expect("stdin");
    for l in lines {
        writeln!(stdin, "{l}").expect("write line");
    }
    drop(stdin);
    let out = child.wait_with_output().expect("wait");
    text(&out.stdout).lines().map(str::to_string).collect()
}

const LIST: &str = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#;

fn why_call(args: &str) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{{"name":"sieve_why","arguments":{args}}}}}"#
    )
}

fn tool_names(reply: &str) -> Vec<String> {
    let v: serde_json::Value = serde_json::from_str(reply).expect("json");
    v["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .map(|t| t["name"].as_str().expect("name").to_string())
        .collect()
}

#[test]
fn test_f6_mcp_sieve_why_gives_the_cli_text_and_runs_no_gh() {
    let temp = rows_project("f6-mcp");
    run(&temp.path, &["build"], None);
    // A fake `gh` first on PATH writes a marker if the tool runs it.
    let fake = temp.path.join("fakebin");
    fs::create_dir_all(&fake).expect("mkdir");
    let marker = temp.path.join("gh-ran");
    let script = format!("#!/bin/sh\ntouch '{}'\n", marker.display());
    fs::write(fake.join("gh"), script).expect("write gh");
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(fake.join("gh"), fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    let cli = text(&run(&temp.path, &["why", "build_repo"], None).stdout);
    let replies = mcp(&temp.path, &[LIST, &why_call(r#"{"symbol":"build_repo"}"#)]);
    let names = tool_names(&replies[0]);
    assert_eq!(names.len(), 7, "{names:?}");
    assert_eq!(names[6], "sieve_why");
    let v: serde_json::Value = serde_json::from_str(&replies[1]).expect("json");
    assert_eq!(v["result"]["isError"], false, "{}", replies[1]);
    assert_eq!(v["result"]["content"][0]["text"], cli.as_str());
    let all = mcp(
        &temp.path,
        &[&why_call(r#"{"symbol":"build_repo","all":true}"#)],
    );
    let v: serde_json::Value = serde_json::from_str(&all[0]).expect("json");
    let want = text(&run(&temp.path, &["why", "build_repo", "--all"], None).stdout);
    assert_eq!(v["result"]["content"][0]["text"], want.as_str());
    assert!(!marker.exists(), "the tool ran gh");
    let bad = mcp(&temp.path, &[&why_call(r#"{}"#)]);
    assert!(bad[0].contains(r#""isError":true"#), "{}", bad[0]);
}

#[test]
fn test_f6_check_outside_a_repo_prints_one_line_and_no_sha_line() {
    let temp = project("f6-check-norepo");
    fs::write(
        temp.path.join("docs/decisions/LEDGER.md"),
        ledger_with_sha(),
    )
    .expect("write");
    run(&temp.path, &["build"], None);
    let body = text(&run(&temp.path, &["why", "--check"], None).stdout);
    assert!(!body.contains("sha not found"), "{body}");
    assert_eq!(
        body.matches("not a git repo: commit checks skipped\n")
            .count(),
        1,
        "{body}"
    );
    // Inside a repo, the same unknown sha is reported.
    run_in_repo(&temp.path, &["init", "-q"]);
    let body = text(&run(&temp.path, &["why", "--check"], None).stdout);
    assert!(body.contains("sha not found: abc1234"), "{body}");
    assert!(!body.contains("not a git repo"), "{body}");
}
