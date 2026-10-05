//! Parity test for P1-45: a context dir given through `--dir`, outside the
//! repo, must never leak into the repo-root resolution. `main.rs`'s global
//! `--dir` flag and each subcommand's own `[dir]` positional both derived
//! their clap id from the field name `dir`, so the one `--dir` value filled
//! both fields, and the repo root silently became the context dir.

mod support;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use serde_json::Value;

use support::TempDir;

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

/// Runs `sieve --dir ctx_dir <args...>` with `cwd` as the current dir.
fn run_sieve(cwd: &Path, ctx_dir: &Path, args: &[&str]) -> Output {
    support::sieve_command()
        .arg("--dir")
        .arg(ctx_dir)
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("run sieve")
}

fn expected_dir() -> PathBuf {
    support::manifest_dir().join("../../tests/fixtures/basic.expected/queries")
}

fn read_golden(id: &str, kind: &str) -> String {
    fs::read_to_string(expected_dir().join(format!("{id}.{kind}.txt")))
        .unwrap_or_else(|e| panic!("read golden {id}.{kind}.txt: {e}"))
}

fn golden_exit(id: &str) -> i32 {
    read_golden(id, "exit")
        .trim()
        .parse::<i32>()
        .expect("parse golden exit")
}

/// Asserts `output`'s stdout and exit code match the `id` golden. `check`
/// has no query-list entry with `[dir]`, so this only compares stdout and
/// the exit code; the stderr refresh/build notes are the CLI's own, not
/// covered by this parity pin.
fn assert_matches_golden(id: &str, output: &Output) {
    let expected_stdout = read_golden(id, "stdout");
    let actual_stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(actual_stdout, expected_stdout, "{id}: stdout mismatch");
    assert_eq!(
        output.status.code(),
        Some(golden_exit(id)),
        "{id}: exit mismatch"
    );
}

/// Runs one `sieve mcp` session with `--dir ctx_dir`, piping `input` to
/// stdin, and returns `(stdout, exit)`.
fn run_mcp(cwd: &Path, ctx_dir: &Path, input: &str) -> (String, i32) {
    let mut child = support::sieve_command()
        .arg("--dir")
        .arg(ctx_dir)
        .arg("mcp")
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn sieve mcp");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(input.as_bytes())
        .expect("write request to stdin");
    let output = child.wait_with_output().expect("wait for sieve mcp");
    (
        String::from_utf8(output.stdout).expect("utf-8 stdout"),
        output.status.code().unwrap_or(-1),
    )
}

/// Strips the `serverInfo.version` field and the update-nudge prefix from
/// `instructions`, so a value compare ignores the two fields that always
/// differ between Sieve and a captured golden.
fn normalize_in_value(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(s)) = map.get_mut("instructions") {
                if let Some(rest) = s.strip_prefix('⬆') {
                    if let Some(idx) = rest.find("\n\n") {
                        *s = rest[idx + 2..].to_string();
                    }
                }
            }
            if let Some(Value::Object(server_info)) = map.get_mut("serverInfo") {
                server_info.insert(
                    "version".to_string(),
                    Value::String("<VERSION>".to_string()),
                );
            }
            for v in map.values_mut() {
                normalize_in_value(v);
            }
        }
        Value::Array(items) => {
            for v in items.iter_mut() {
                normalize_in_value(v);
            }
        }
        _ => {}
    }
}

/// Reads every NDJSON reply line into a map keyed by its `id`'s raw JSON
/// text, with every id-less line collected separately.
fn index_replies(stdout: &str) -> (std::collections::HashMap<String, Value>, Vec<Value>) {
    let mut by_id = std::collections::HashMap::new();
    let mut id_less = Vec::new();
    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let mut value: Value =
            serde_json::from_str(line).unwrap_or_else(|e| panic!("reply not JSON: {line}: {e}"));
        normalize_in_value(&mut value);
        match value.as_object().and_then(|o| o.get("id")).cloned() {
            Some(id) => {
                by_id.insert(id.to_string(), value);
            }
            None => id_less.push(value),
        }
    }
    (by_id, id_less)
}

/// Compares one NDJSON session's lines against the golden session, by `id`,
/// after normalizing the version and the nudge prefix. Id-less lines (the
/// token-savings notices) compare as a multiset, matching `mcp_cli.rs`.
fn assert_mcp_session_matches_golden(stdout: &str) {
    let golden_stdout = fs::read_to_string(
        support::manifest_dir().join("../../tests/fixtures/basic.expected/mcp/session.stdout.txt"),
    )
    .expect("read golden mcp session stdout");

    let (got_by_id, mut got_id_less) = index_replies(stdout);
    let mut golden_id_less = Vec::new();
    for line in golden_stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let mut golden_value: Value = serde_json::from_str(line).expect("golden line is JSON");
        normalize_in_value(&mut golden_value);
        match golden_value.as_object().and_then(|o| o.get("id")).cloned() {
            Some(id) => {
                let key = id.to_string();
                let got_value = got_by_id
                    .get(&key)
                    .unwrap_or_else(|| panic!("no reply for id {id}: golden line: {line}"));
                assert_eq!(&golden_value, got_value, "id {id} mismatch");
            }
            None => golden_id_less.push(golden_value),
        }
    }

    let to_sorted_strings = |values: &mut Vec<Value>| -> Vec<String> {
        let mut out: Vec<String> = values
            .iter()
            .map(|v| serde_json::to_string(v).expect("reserialize id-less line"))
            .collect();
        out.sort();
        out
    };
    assert_eq!(
        to_sorted_strings(&mut got_id_less),
        to_sorted_strings(&mut golden_id_less),
        "id-less lines (multiset)"
    );
}

/// P1-45: a `--dir` outside the repo must resolve every query's repo root
/// from `cwd` (or an explicit `[dir]`), never from the context dir.
#[test]
fn test_p1_45_context_dir_outside_the_repo() {
    let fixture = support::manifest_dir().join("../../tests/fixtures/basic");
    let repo = TempDir::new("repo");
    copy_dir(&fixture, &repo.path);
    let ctx = TempDir::new("ctx");
    let ctx_dir = ctx.path.join("ctx");

    let build_output = run_sieve(&repo.path, &ctx_dir, &["build", "."]);
    assert_eq!(
        build_output.status.code(),
        Some(0),
        "build failed: {}",
        String::from_utf8_lossy(&build_output.stderr)
    );

    // A `--dir` outside the repo disables both the `.gitignore` and the
    // `.ignore` edit (build note section 1): neither file exists.
    assert!(
        !repo.path.join(".gitignore").exists(),
        "a build with --dir outside the repo must not write .gitignore"
    );
    assert!(
        !repo.path.join(".ignore").exists(),
        "a build with --dir outside the repo must not write .ignore"
    );

    let grep = run_sieve(
        &repo.path,
        &ctx_dir,
        &["grep", "add", "--fixed", "--no-refresh"],
    );
    assert_matches_golden("grep-add", &grep);

    let check = run_sieve(&repo.path, &ctx_dir, &["check"]);
    assert_matches_golden("check", &check);

    let callers = run_sieve(&repo.path, &ctx_dir, &["callers", "double", "--no-refresh"]);
    assert_matches_golden("callers-double", &callers);

    let skeleton = run_sieve(
        &repo.path,
        &ctx_dir,
        &["skeleton", "src/app.ts", "--no-refresh"],
    );
    assert_matches_golden("skeleton-app", &skeleton);

    let map = run_sieve(&repo.path, &ctx_dir, &["map", "--no-refresh"]);
    assert_matches_golden("map", &map);

    // `ask "double"` has no golden at this exact arg shape (only `ask-n1`,
    // with `-n 1`, does); this only checks the query runs clean with the
    // repo root resolved outside the context dir.
    let ask = run_sieve(&repo.path, &ctx_dir, &["ask", "double", "--no-refresh"]);
    assert_eq!(
        ask.status.code(),
        Some(0),
        "ask failed: {}",
        String::from_utf8_lossy(&ask.stderr)
    );

    let request =
        fs::read_to_string(support::manifest_dir().join("../../tests/inputs/mcp/basic.ndjson"))
            .expect("read mcp request file");
    let (mcp_stdout, mcp_exit) = run_mcp(&repo.path, &ctx_dir, &request);
    assert_eq!(mcp_exit, 0, "mcp session exit");
    assert_mcp_session_matches_golden(&mcp_stdout);
}
