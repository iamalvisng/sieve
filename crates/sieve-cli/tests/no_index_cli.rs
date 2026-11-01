//! A query in a folder that is not a repo root prints one clear line, and
//! exits 1, when there is no built index. A query at a repo root with no
//! index builds the index first, unless `SIEVE_NO_AUTO_BUILD` is set.

mod support;

use std::fs;

use support::TempDir;

const LINE: &str = "sieve: no index here yet \u{2014} run sieve build .";

/// Runs one query command in an empty dir and checks the line, the exit
/// code and that the query makes no folder.
fn check_no_index(args: &[&str]) {
    let temp = TempDir::new("no-index");
    let output = support::sieve_command()
        .args(args)
        .current_dir(&temp.path)
        .output()
        .expect("run sieve");
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.status.code(), Some(1), "{args:?}: {all}");
    assert!(all.contains(LINE), "{args:?}: {all}");
    assert!(!temp.path.join("sieve").exists(), "{args:?} made a folder");
}

#[test]
fn test_no_index_ask() {
    check_no_index(&["ask", "anything"]);
}

#[test]
fn test_no_index_grep() {
    check_no_index(&["grep", "anything"]);
}

#[test]
fn test_no_index_callers() {
    check_no_index(&["callers", "anything"]);
}

#[test]
fn test_no_index_skeleton() {
    check_no_index(&["skeleton", "a.rs"]);
}

#[test]
fn test_no_index_map() {
    check_no_index(&["map"]);
}

#[test]
fn test_no_index_blast() {
    check_no_index(&["blast"]);
}

#[test]
fn test_no_index_why() {
    check_no_index(&["why", "anything"]);
}

/// Runs `sieve` with `args` in `dir`; returns the exit code and all output.
fn run_in(dir: &std::path::Path, args: &[&str]) -> (Option<i32>, String) {
    let output = support::sieve_command()
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run sieve");
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.code(), all)
}

/// A workspace parent has no index of its own. The commands that do not
/// federate say so, and never print the no-index line.
#[test]
fn test_no_index_line_never_fires_at_a_workspace_parent() {
    let temp = TempDir::new("no-index-ws");
    fs::create_dir_all(temp.path.join("sieve")).expect("mkdir");
    fs::create_dir_all(temp.path.join("alpha")).expect("mkdir");
    fs::write(
        temp.path.join("sieve/workspace.json"),
        "{\n  \"version\": 1,\n  \"children\": [\"alpha\"]\n}\n",
    )
    .expect("write workspace.json");
    for args in [
        &["skeleton", "zzz/a.rs"][..],
        &["callers", "x", "--json"],
        &["map", "--json"],
        &["blast"],
    ] {
        let (code, all) = run_in(&temp.path, args);
        assert_eq!(code, Some(1), "{args:?}: {all}");
        assert!(all.contains("workspace parent"), "{args:?}: {all}");
        assert!(!all.contains(LINE), "{args:?}: {all}");
    }
}

/// A `--dir` that does not exist is a missing directory, not a missing index.
#[test]
fn test_no_index_missing_dir_says_directory_not_found() {
    let temp = TempDir::new("no-index-dir");
    for args in [
        &["ask", "q", "--dir", "nope"][..],
        &["callers", "a", "nope"],
    ] {
        let (code, all) = run_in(&temp.path, args);
        assert_eq!(code, Some(1), "{args:?}: {all}");
        assert!(all.contains("directory not found: nope"), "{args:?}: {all}");
        assert!(!all.contains(LINE), "{args:?}: {all}");
    }
}

/// A repo with an index never sees the line.
#[test]
fn test_no_index_line_does_not_fire_with_an_index() {
    let temp = TempDir::new("no-index-built");
    fs::write(temp.path.join("a.ts"), "export function alpha() {}\n").expect("write");
    let (code, all) = run_in(&temp.path, &["build", "."]);
    assert_eq!(code, Some(0), "{all}");
    for args in [
        &["ask", "alpha"][..],
        &["grep", "alpha"],
        &["skeleton", "a.ts"],
    ] {
        let (code, all) = run_in(&temp.path, args);
        assert_eq!(code, Some(0), "{args:?}: {all}");
        assert!(!all.contains(LINE), "{args:?}: {all}");
    }
}

/// Makes a git repo with one source file and no index.
fn unbuilt_repo(label: &str) -> TempDir {
    let temp = TempDir::new(label);
    fs::write(temp.path.join("a.ts"), "export function alpha() {}\n").expect("write");
    let status = std::process::Command::new("git")
        .arg("init")
        .arg("-q")
        .current_dir(&temp.path)
        .status()
        .expect("run git");
    assert!(status.success(), "git init failed");
    temp
}

/// A query at a repo root with no index builds it, notes the build on
/// stderr, answers, and leaves the index. The answer equals the answer
/// after an explicit build.
#[test]
fn a_query_at_a_repo_root_builds_the_index_then_answers() {
    let first = unbuilt_repo("first-build");
    let output = support::sieve_command()
        .args(["grep", "alpha"])
        .current_dir(&first.path)
        .output()
        .expect("run sieve");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("[sieve] built the index (1 file,"),
        "{stderr}"
    );
    assert!(first.path.join("sieve/.graph/wiring.json").is_file());

    let twin = unbuilt_repo("first-build-twin");
    let (code, all) = run_in(&twin.path, &["build", "."]);
    assert_eq!(code, Some(0), "{all}");
    let expected = support::sieve_command()
        .args(["grep", "alpha"])
        .current_dir(&twin.path)
        .output()
        .expect("run sieve");
    assert_eq!(output.stdout, expected.stdout);
}

/// One guard case. The folder is a git repo unless `git` is off.
struct Guard {
    label: &'static str,
    git: bool,
    /// The folder holds `workspace.json`.
    workspace: bool,
    env: Option<(&'static str, &'static str)>,
    /// `HOME` is the folder.
    home: bool,
    /// Extra args for `ask`.
    extra: &'static [&'static str],
    /// The text the stop prints.
    text: &'static str,
}

impl Guard {
    const fn new(label: &'static str, text: &'static str) -> Self {
        Self {
            label,
            git: true,
            workspace: false,
            env: None,
            home: false,
            extra: &[],
            text,
        }
    }
}

/// Every guard keeps the old stop and builds nothing.
#[test]
fn each_guard_keeps_the_stop_and_builds_nothing() {
    let cases = [
        Guard {
            git: false,
            ..Guard::new("no-git", LINE)
        },
        Guard {
            home: true,
            ..Guard::new("home", LINE)
        },
        Guard {
            extra: &["--no-refresh"],
            ..Guard::new("no-refresh", LINE)
        },
        Guard {
            env: Some(("SIEVE_NO_AUTO_BUILD", "1")),
            ..Guard::new("off-switch", LINE)
        },
        Guard {
            workspace: true,
            ..Guard::new("workspace", "workspace parent")
        },
    ];
    for case in cases {
        let label = case.label;
        let temp = if case.git {
            unbuilt_repo(&format!("guard-{label}"))
        } else {
            TempDir::new(&format!("guard-{label}"))
        };
        if case.workspace {
            fs::create_dir_all(temp.path.join("sieve")).expect("mkdir");
            fs::write(
                temp.path.join("sieve/workspace.json"),
                "{\n  \"version\": 1,\n  \"children\": []\n}\n",
            )
            .expect("write workspace.json");
        }
        let mut command = support::sieve_command();
        // `ask` and `blast` differ here: `ask` federates at a workspace
        // parent, and `blast` stops there.
        if case.workspace {
            command.arg("blast");
        } else {
            command.arg("ask").args(case.extra).arg("alpha");
        }
        command.current_dir(&temp.path);
        if let Some((key, value)) = case.env {
            command.env(key, value);
        }
        if case.home {
            command.env("HOME", &temp.path);
        }
        let output = command.output().expect("run sieve");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{label}: {stderr}");
        assert!(stderr.contains(case.text), "{label}: {stderr}");
        assert!(!temp.path.join("sieve/.graph").exists(), "{label} built");
    }
}

/// Two first queries at once both answer. One builds the index and the
/// other uses it, with no "already in flight" error.
#[test]
fn two_first_queries_at_once_both_answer() {
    let temp = unbuilt_repo("first-build-race");
    for n in 0..150 {
        fs::write(
            temp.path.join(format!("f{n}.ts")),
            format!("export function fn{n}() {{ return {n}; }}\n"),
        )
        .expect("write");
    }
    let spawn = || {
        support::sieve_command()
            .args(["grep", "alpha"])
            .current_dir(&temp.path)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("run sieve")
    };
    let (one, two) = (spawn(), spawn());
    let one = one.wait_with_output().expect("wait");
    let two = two.wait_with_output().expect("wait");
    for output in [&one, &two] {
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(0), "{stderr}");
        assert!(!stderr.contains("in flight"), "{stderr}");
    }
    assert_eq!(one.stdout, two.stdout);
}

/// A running `sieve mcp` with a reader thread, so a reply wait has a timeout.
struct McpSession {
    child: std::process::Child,
    replies: std::sync::mpsc::Receiver<String>,
}

impl McpSession {
    fn start(dir: &std::path::Path) -> Self {
        use std::io::BufRead;
        let mut child = support::sieve_command()
            .arg("mcp")
            .current_dir(dir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("run sieve mcp");
        let stdout = child.stdout.take().expect("stdout");
        let (sender, replies) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout)
                .lines()
                .map_while(Result::ok)
            {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        Self { child, replies }
    }

    /// Sends one request and returns the reply, or fails after `wait`.
    fn ask(&mut self, request: &str, wait: std::time::Duration) -> serde_json::Value {
        use std::io::Write;
        let stdin = self.child.stdin.as_mut().expect("stdin");
        writeln!(stdin, "{request}").expect("write request");
        let line = self
            .replies
            .recv_timeout(wait)
            .unwrap_or_else(|_| panic!("no reply to {request} within {wait:?}"));
        serde_json::from_str(&line).expect("reply is json")
    }
}

impl Drop for McpSession {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

const CALL_QUERY: &str = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"sieve_query","arguments":{"query":"alpha"}}}"#;

/// `sieve mcp` on a repo with no index answers `initialize` and lists the
/// tools at once, then answers the first tool call after the build.
#[test]
fn mcp_in_a_repo_with_no_index_builds_then_answers_the_first_call() {
    let temp = unbuilt_repo("mcp-first-build");
    let quick = std::time::Duration::from_secs(20);
    let slow = std::time::Duration::from_secs(60);
    let mut mcp = McpSession::start(&temp.path);
    let init = mcp.ask(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        quick,
    );
    assert!(init["result"]["serverInfo"].is_object(), "{init}");
    let list = mcp.ask(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#, quick);
    assert!(!list["result"]["tools"]
        .as_array()
        .expect("tools")
        .is_empty());
    let call = mcp.ask(CALL_QUERY, slow);
    assert_eq!(call["result"]["isError"], false, "{call}");
    let text = call["result"]["content"][0]["text"].as_str().expect("text");
    assert!(text.contains("alpha"), "{text}");
    assert!(temp.path.join("sieve/.graph/wiring.json").is_file());
}

/// A failed first build reaches the waiting tool call as a tool error.
/// A plain file named `sieve` makes the build fail.
#[test]
fn mcp_returns_a_failed_first_build_to_the_call() {
    let temp = unbuilt_repo("mcp-failed-build");
    fs::write(temp.path.join("sieve"), "not a folder").expect("write");
    let mut mcp = McpSession::start(&temp.path);
    let call = mcp.ask(CALL_QUERY, std::time::Duration::from_secs(60));
    assert_eq!(call["result"]["isError"], true, "{call}");
    let text = call["result"]["content"][0]["text"].as_str().expect("text");
    assert!(!text.is_empty() && !text.contains("alpha"), "{text}");
}
