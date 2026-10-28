//! `sieve rename` (design `docs/design/rename.md`, slice RN3b): the dry run,
//! the `--write` and `--expect` rule, the exit codes, the JSON plan and
//! `--recover`, on a small TypeScript repo built in a temp dir.

mod support;

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use support::TempDir;

const ARGS_TS: &str = "export function parseArgs(argv: string[]): string[] {\n  return argv;\n}\n\nexport function usage(): string {\n  return \"usage\";\n}\n";

const CLI_TS: &str = "import { parseArgs, usage } from \"./args\";\n\n// parseArgs reads argv\nexport function main(): void {\n  const args = parseArgs(process.argv.slice(2));\n  console.log(usage(), args);\n}\n";

/// A scratch repo with two TypeScript files, already built.
fn repo(label: &str) -> TempDir {
    let temp = TempDir::new(label);
    fs::create_dir_all(temp.path.join("src")).expect("mkdir src");
    fs::create_dir_all(temp.path.join("home")).expect("mkdir home");
    fs::write(temp.path.join("src/args.ts"), ARGS_TS).expect("write args");
    fs::write(temp.path.join("src/cli.ts"), CLI_TS).expect("write cli");
    let built = run(&temp.path, &["build", "."]);
    assert!(built.status.success(), "{}", text(&built.stderr));
    temp
}

/// Runs the binary in `cwd` with a scratch `HOME`.
fn run(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sieve"))
        .args(args)
        .current_dir(cwd)
        .env("HOME", cwd.join("home"))
        .env("PATH", support::sieve_path())
        .output()
        .expect("run sieve")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn read(root: &Path, rel: &str) -> String {
    fs::read_to_string(root.join(rel)).expect("read file")
}

/// The plan id of a dry run, from its JSON.
fn plan_id(root: &Path, symbol: &str, new: &str) -> String {
    let out = run(root, &["rename", symbol, new, "--json"]);
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("plan json");
    json["plan_id"].as_str().expect("plan_id").to_string()
}

#[test]
fn test_rn3_dry_run_changes_no_file() {
    let temp = repo("rn3-dry");
    let out = run(&temp.path, &["rename", "parseArgs", "parseCliArgs"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let body = text(&out.stdout);
    assert!(body.contains("Dry run: no file changed."), "{body}");
    assert!(body.contains("--write --expect "), "{body}");
    assert!(body.contains("-export function parseArgs"), "{body}");
    assert_eq!(read(&temp.path, "src/args.ts"), ARGS_TS);
    assert_eq!(read(&temp.path, "src/cli.ts"), CLI_TS);
}

#[test]
fn test_rn3_write_needs_matching_expect() {
    let temp = repo("rn3-expect");
    let wrong = run(
        &temp.path,
        &[
            "rename",
            "parseArgs",
            "parseCliArgs",
            "--write",
            "--expect",
            "00000000",
        ],
    );
    assert_eq!(wrong.status.code(), Some(2), "{}", text(&wrong.stdout));
    assert_eq!(read(&temp.path, "src/args.ts"), ARGS_TS);

    let id = plan_id(&temp.path, "parseArgs", "parseCliArgs");
    let right = run(
        &temp.path,
        &[
            "rename",
            "parseArgs",
            "parseCliArgs",
            "--write",
            "--expect",
            &id,
        ],
    );
    assert_eq!(right.status.code(), Some(0), "{}", text(&right.stdout));
    assert!(read(&temp.path, "src/args.ts").contains("function parseCliArgs("));
    assert!(read(&temp.path, "src/cli.ts").contains("parseCliArgs(process.argv"));
    assert!(read(&temp.path, "src/cli.ts").contains("// parseArgs reads argv"));
}

#[test]
fn test_rn3_changed_file_gives_new_plan_id() {
    let temp = repo("rn3-id");
    let before = plan_id(&temp.path, "parseArgs", "parseCliArgs");
    assert_eq!(before, plan_id(&temp.path, "parseArgs", "parseCliArgs"));
    let changed = format!("{CLI_TS}// a new line\n");
    fs::write(temp.path.join("src/cli.ts"), changed).expect("edit cli");
    let after = plan_id(&temp.path, "parseArgs", "parseCliArgs");
    assert_ne!(before, after);
}

#[test]
fn test_rn3_write_refreshes_graph() {
    let temp = repo("rn3-graph");
    let id = plan_id(&temp.path, "parseArgs", "parseCliArgs");
    let out = run(
        &temp.path,
        &[
            "rename",
            "parseArgs",
            "parseCliArgs",
            "--write",
            "--expect",
            &id,
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stdout));
    let body = text(&out.stdout);
    assert!(
        body.starts_with("renamed src/args.ts#parseArgs -> parseCliArgs: 2 files"),
        "{body}"
    );
    assert!(body.contains("Graph refreshed. Check passed."), "{body}");
    // The query below skips its own refresh, so the answer comes from the
    // graph that the rename wrote.
    let hit = run(&temp.path, &["callers", "parseCliArgs", "--no-refresh"]);
    let hit_body = text(&hit.stdout);
    assert!(hit_body.contains("src/cli.ts"), "{hit_body}");
}

#[test]
fn test_rn3_refusal_exits_2() {
    let temp = repo("rn3-refuse");
    // `usage` is already declared in the module.
    let out = run(&temp.path, &["rename", "parseArgs", "usage"]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    let body = text(&out.stdout);
    assert!(
        body.starts_with("rename refused: src/args.ts#parseArgs (function) -> usage\n"),
        "{body}"
    );
    assert!(body.contains("No file changed."), "{body}");
    assert!(body.contains("collision"), "{body}");
    assert_eq!(read(&temp.path, "src/args.ts"), ARGS_TS);

    let write = run(&temp.path, &["rename", "parseArgs", "usage", "--write"]);
    assert_eq!(write.status.code(), Some(2), "{}", text(&write.stdout));
    assert_eq!(read(&temp.path, "src/args.ts"), ARGS_TS);
}

#[test]
fn test_rn3_usage_error_exits_1() {
    let temp = repo("rn3-usage");
    let out = run(&temp.path, &["rename", "parseArgs"]);
    assert_eq!(out.status.code(), Some(1));
    let none = run(&temp.path, &["rename", "noSuchSymbol", "x"]);
    assert_eq!(none.status.code(), Some(1));
}

#[test]
fn test_rn3_json_plan_shape() {
    let temp = repo("rn3-json");
    let out = run(
        &temp.path,
        &["rename", "parseArgs", "parseCliArgs", "--json"],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("plan json");
    assert_eq!(json["version"], 1);
    assert_eq!(json["status"], "ready");
    assert_eq!(json["old"], "parseArgs");
    assert_eq!(json["new"], "parseCliArgs");
    assert_eq!(json["target"]["id"], "src/args.ts#parseArgs");
    assert_eq!(json["plan_id"].as_str().map(str::len), Some(8));
    let files = json["files"].as_array().expect("files");
    assert_eq!(files.len(), 2);
    assert!(files[0]["sha256"].is_string());
    assert!(files[0]["edits"][0]["start"].is_u64());
    assert!(files[0]["edits"][0]["site"].is_string());
    assert!(json["refused"].as_array().expect("refused").is_empty());
    assert!(json["report"].is_array());
}

/// Hex text of some bytes.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The sha256 of some bytes, in lowercase hex. It asks the system tool,
/// because the crate has no hash dependency.
fn sha256(bytes: &[u8]) -> String {
    use std::io::Write;
    use std::process::Stdio;
    for argv in [&["sha256sum"][..], &["shasum", "-a", "256"][..]] {
        let Ok(mut child) = Command::new(argv[0])
            .args(&argv[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
        else {
            continue;
        };
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(bytes)
            .expect("write stdin");
        let out = child.wait_with_output().expect("hash output");
        return text(&out.stdout)
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_string();
    }
    panic!("no sha256sum or shasum on PATH");
}

/// Writes a journal for the rename of `parseArgs` in `src/args.ts` and puts
/// `disk` in that file. Gives the journal path.
fn leave_journal(temp: &TempDir, disk: &str) -> std::path::PathBuf {
    let new_bytes = ARGS_TS.replace("parseArgs", "parseCliArgs");
    let old_sha = sha256(ARGS_TS.as_bytes());
    let new_sha = sha256(new_bytes.as_bytes());
    let old_hex = hex(ARGS_TS.as_bytes());
    let joined =
        format!("1\0parseArgs\0parseCliArgs\0src/args.ts\0{old_sha}\0{new_sha}\0{old_hex}\0");
    let journal = serde_json::json!({
        "version": 1,
        "old": "parseArgs",
        "new": "parseCliArgs",
        "files": [{
            "path": "src/args.ts",
            "sha256": old_sha,
            "new_sha256": new_sha,
            "bytes_hex": old_hex,
        }],
        "checksum": sha256(joined.as_bytes()),
    });
    let journal_path = temp.path.join("sieve/.cache/rename-journal.json");
    fs::create_dir_all(journal_path.parent().expect("parent")).expect("mkdir cache");
    fs::write(&journal_path, journal.to_string()).expect("write journal");
    fs::write(temp.path.join("src/args.ts"), disk).expect("write disk bytes");
    journal_path
}

#[test]
fn test_rn3_recover_restores_files() {
    let temp = repo("rn3-recover");
    // Act out a crash after the write: the file holds the new bytes and the
    // journal holds the old bytes.
    let new_bytes = ARGS_TS.replace("parseArgs", "parseCliArgs");
    let journal_path = leave_journal(&temp, &new_bytes);

    let refused = run(&temp.path, &["rename", "parseCliArgs", "other"]);
    assert_eq!(refused.status.code(), Some(2), "{}", text(&refused.stdout));
    assert!(text(&refused.stdout).contains("journal_present"));

    let rec = run(&temp.path, &["rename", "--recover"]);
    assert_eq!(rec.status.code(), Some(0), "{}", text(&rec.stderr));
    assert!(
        text(&rec.stdout).contains("recovered 1 file: src/args.ts"),
        "{}",
        text(&rec.stdout)
    );
    assert_eq!(read(&temp.path, "src/args.ts"), ARGS_TS);
    assert!(!journal_path.exists());

    let none = run(&temp.path, &["rename", "--recover"]);
    assert_eq!(none.status.code(), Some(0));
    assert!(text(&none.stdout).contains("no rename journal found"));
}

#[test]
fn test_rn3_recover_conflict_exits_2_and_discard_clears() {
    let temp = repo("rn3-conflict");
    // Someone else edited the file after the crash: its bytes match neither
    // the old nor the new bytes.
    let edited = format!("{ARGS_TS}// mine\n");
    let journal_path = leave_journal(&temp, &edited);

    let rec = run(&temp.path, &["rename", "--recover"]);
    assert_eq!(rec.status.code(), Some(2), "{}", text(&rec.stderr));
    let body = text(&rec.stdout);
    assert!(body.contains("src/args.ts"), "{body}");
    assert!(body.contains("--discard-journal"), "{body}");
    assert!(journal_path.exists());
    assert_eq!(read(&temp.path, "src/args.ts"), edited);

    let discard = run(&temp.path, &["rename", "--discard-journal"]);
    assert_eq!(discard.status.code(), Some(0), "{}", text(&discard.stderr));
    assert!(!journal_path.exists());
    assert_eq!(read(&temp.path, "src/args.ts"), edited);

    let plan = run(&temp.path, &["rename", "parseArgs", "parseCliArgs"]);
    assert_eq!(plan.status.code(), Some(0), "{}", text(&plan.stdout));
    assert!(text(&plan.stdout).contains("Dry run: no file changed."));
}

#[test]
fn test_rn3_bad_journal_recover_exits_1() {
    let temp = repo("rn3-badjournal");
    let journal_path = temp.path.join("sieve/.cache/rename-journal.json");
    fs::create_dir_all(journal_path.parent().expect("parent")).expect("mkdir cache");
    fs::write(&journal_path, "not json").expect("write journal");
    let rec = run(&temp.path, &["rename", "--recover"]);
    assert_eq!(rec.status.code(), Some(1));
    assert!(
        text(&rec.stderr).contains("recover refused"),
        "{}",
        text(&rec.stderr)
    );
    assert!(journal_path.exists());
}

#[test]
fn test_rn3_dry_run_text_golden() {
    let temp = repo("rn3-golden");
    let id = plan_id(&temp.path, "parseArgs", "parseCliArgs");
    let out = run(&temp.path, &["rename", "parseArgs", "parseCliArgs"]);
    let expected = format!(
        "rename plan {id}: src/args.ts#parseArgs (function) -> parseCliArgs\n\
         2 files, 3 edits. Dry run: no file changed.\n\
         Write it: sieve rename parseArgs parseCliArgs --write --expect {id}\n\
         \n\
         --- a/src/args.ts\n\
         +++ b/src/args.ts\n\
         @@ -1 +1 @@\n\
         -export function parseArgs(argv: string[]): string[] {{\n\
         +export function parseCliArgs(argv: string[]): string[] {{\n\
         --- a/src/cli.ts\n\
         +++ b/src/cli.ts\n\
         @@ -1 +1 @@\n\
         -import {{ parseArgs, usage }} from \"./args\";\n\
         +import {{ parseCliArgs, usage }} from \"./args\";\n\
         @@ -5 +5 @@\n\
         -  const args = parseArgs(process.argv.slice(2));\n\
         +  const args = parseCliArgs(process.argv.slice(2));\n\
         not edited  src/cli.ts:3  comment  // parseArgs reads argv\n\
         \u{1f3af} 2 files \u{b7} 0 unproven edits\n"
    );
    assert_eq!(text(&out.stdout), expected);
}

#[test]
fn test_rn3_write_without_expect_succeeds() {
    let temp = repo("rn3-noexpect");
    let out = run(
        &temp.path,
        &["rename", "parseArgs", "parseCliArgs", "--write"],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).starts_with("renamed "),
        "{}",
        text(&out.stdout)
    );
    assert!(read(&temp.path, "src/args.ts").contains("function parseCliArgs("));
}

#[test]
fn test_rn3_journal_flags_conflict() {
    let temp = repo("rn3-flags");
    for args in [
        &["rename", "--recover", "--discard-journal"][..],
        &["rename", "--discard-journal", "--write", "a", "b"][..],
        &["rename", "--recover", "--write", "a", "b"][..],
    ] {
        let out = run(&temp.path, args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
    }
    let none = run(&temp.path, &["rename", "--discard-journal"]);
    assert_eq!(none.status.code(), Some(0), "{}", text(&none.stderr));
    assert_eq!(text(&none.stdout), "no journal to discard.\n");
}

#[test]
fn test_rn3_json_write_shape() {
    let temp = repo("rn3-json-write");
    let out = run(
        &temp.path,
        &["rename", "parseArgs", "parseCliArgs", "--write", "--json"],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(json["status"], "written");
    assert_eq!(json["plan"]["plan_id"].as_str().map(str::len), Some(8));
    assert_eq!(json["plan"]["target"]["id"], "src/args.ts#parseArgs");
    assert!(json["files"].is_array());
    assert!(json.get("plan_id").is_none());
    assert!(json.get("target").is_none());
}

#[test]
fn test_rn3_dry_run_strips_terminal_controls() {
    let temp = repo("rn3-controls");
    let cli = CLI_TS.replace(
        "// parseArgs reads argv",
        "// parseArgs reads argv \x1b[31mRED\x1b[0m \x1b]0;title\x07end",
    );
    fs::write(temp.path.join("src/cli.ts"), cli).expect("write cli");
    let out = run(&temp.path, &["rename", "parseArgs", "parseCliArgs"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let body = text(&out.stdout);
    assert!(
        body.contains("not edited  src/cli.ts:3  comment  // parseArgs reads argv RED end"),
        "{body}"
    );
    assert!(!body.contains('\x1b'), "{body:?}");
    assert!(!body.contains('\x07'), "{body:?}");
}
