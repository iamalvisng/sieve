//! NDJSON framing and method routing for `sieve mcp`
//! (the `mcp-server.md` note sections 1, 2, 6, 7).

use std::io::{BufRead, Write};
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use crate::names::{self, server_name, tool_specs};
use crate::tools;

/// Runs one MCP session: reads NDJSON requests from `stdin`, writes NDJSON
/// replies to `stdout`, writes every diagnostic to `stderr`, and returns
/// the process exit code.
///
/// `dir_override` is true when the caller named the context dir with
/// `--dir`; `tools/list` then advertises every tool with no probe
/// (`advertised`).
///
/// `ancestor_note`, when given, is the one line the CLI's root walk may
/// have printed (`query-prelude-grep.md`). This form has no boot upkeep
/// lines; see [`serve_with_upkeep`].
// One flag over the seven-argument cap: every caller passes the same
// five session facts plus three streams, and a struct would only move
// the list.
#[allow(clippy::too_many_arguments)]
pub fn serve<R: BufRead, W1: Write, W2: Write>(
    root: &Path,
    context_dir: &Path,
    dir_override: bool,
    version: &str,
    stdin: R,
    stdout: W1,
    stderr: W2,
    ancestor_note: Option<&str>,
) -> i32 {
    serve_with_upkeep(
        root,
        context_dir,
        dir_override,
        version,
        stdin,
        stdout,
        stderr,
        ancestor_note,
        &[],
    )
}

/// Like [`serve`], plus the boot upkeep lines (P4-57). The server
/// prints each line on stderr after the ancestor note, then puts the
/// lines at the start of `instructions`, joined by a newline and
/// followed by a blank line.
#[allow(clippy::too_many_arguments)]
pub fn serve_with_upkeep<R: BufRead, W1: Write, W2: Write>(
    root: &Path,
    context_dir: &Path,
    dir_override: bool,
    version: &str,
    stdin: R,
    mut stdout: W1,
    mut stderr: W2,
    ancestor_note: Option<&str>,
    upkeep: &[String],
) -> i32 {
    if let Some(note) = ancestor_note {
        let _ = writeln!(stderr, "{note}");
    }
    for line in upkeep {
        let _ = writeln!(stderr, "{line}");
    }

    let session = Session {
        root,
        context_dir,
        dir_override,
        version,
        upkeep,
    };
    for line in stdin.lines() {
        let Ok(line) = line else { break };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        handle_line(trimmed, &session, &mut stdout);
    }
    0
}

/// The per-session facts every request handler reads.
struct Session<'a> {
    root: &'a Path,
    context_dir: &'a Path,
    dir_override: bool,
    version: &'a str,
    upkeep: &'a [String],
}

/// JavaScript's `String(method)` for the `method not found` text: a
/// missing key reads `undefined`, every other value its string form.
fn method_text(method: Option<&Value>) -> String {
    match method {
        None => "undefined".to_string(),
        Some(v) => tools::js_string(v),
    }
}

/// Parses and routes one NDJSON line, writing at most one reply line.
fn handle_line<W: Write>(line: &str, session: &Session, out: &mut W) {
    let parsed: Result<Value, _> = serde_json::from_str(line);
    let Ok(value) = parsed else {
        write_error_line(out, None, -32700, "parse error");
        return;
    };
    let obj = match &value {
        Value::Object(obj) => obj,
        // `const { id, method } = msg` on a number, string, array or bool reads
        // `undefined` for both: a notification with an unknown method, which
        // gets no reply. A `null` line would crash a plain JS server; Sieve
        // answers a parse error instead (DV-7, open).
        Value::Null => {
            write_error_line(out, None, -32700, "parse error");
            return;
        }
        _ => return,
    };

    let has_id = obj.contains_key("id");
    let id = obj.get("id").cloned();
    let method = obj.get("method").and_then(Value::as_str).unwrap_or("");
    let params = obj.get("params").cloned().unwrap_or(Value::Null);

    match method {
        "initialize" => {
            // `params?.protocolVersion ?? '2024-11-05'`: the client's
            // value echoes back as sent, whatever its JSON type.
            let protocol_version = match params.get("protocolVersion") {
                None | Some(Value::Null) => json_str("2024-11-05"),
                Some(Value::Number(n)) => tools::js_number_text(n.as_f64().unwrap_or(f64::NAN)),
                Some(v) => v.to_string(),
            };
            let result = format!(
                "{{\"protocolVersion\":{},\"capabilities\":{{\"tools\":{{}}}},\"serverInfo\":{{\"name\":{},\"version\":{}}},\"instructions\":{}}}",
                protocol_version,
                json_str(server_name()),
                json_str(session.version),
                json_str(&names::instructions_with_upkeep(session.upkeep)),
            );
            write_result_line(out, has_id, id.as_ref(), &result);
        }
        "notifications/initialized" | "notifications/cancelled" => {}
        "ping" => {
            if has_id {
                write_result_line(out, has_id, id.as_ref(), "{}");
            }
        }
        "tools/list" => {
            if !has_id {
                return;
            }
            let specs = if advertised(session.root, session.context_dir, session.dir_override) {
                tool_specs()
            } else {
                Vec::new()
            };
            let entries: Vec<String> = specs
                .iter()
                .map(|s| {
                    format!(
                        "{{\"name\":{},\"description\":{},\"inputSchema\":{}}}",
                        json_str(&s.name),
                        json_str(&s.description),
                        s.input_schema
                    )
                })
                .collect();
            let result = format!("{{\"tools\":[{}]}}", entries.join(","));
            write_result_line(out, has_id, id.as_ref(), &result);
        }
        "tools/call" => {
            if !has_id {
                return;
            }
            // `String(params?.name ?? '')` and `params?.arguments ?? {}`.
            let name = match params.get("name") {
                None | Some(Value::Null) => String::new(),
                Some(v) => tools::js_string(v),
            };
            let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
            let (text, is_error) =
                tools::call(&name, &arguments, session.root, session.context_dir);
            let result = format!(
                "{{\"content\":[{{\"type\":\"text\",\"text\":{}}}],\"isError\":{}}}",
                json_str(&text),
                is_error
            );
            write_result_line(out, has_id, id.as_ref(), &result);
        }
        _ => {
            if has_id {
                write_error_line(
                    out,
                    id.as_ref(),
                    -32601,
                    &format!("method not found: {}", method_text(obj.get("method"))),
                );
            }
        }
    }
}

/// Whether `tools/list` lists the tools (`advertised`):
/// always under a `--dir` override, else when this tree holds an index,
/// else when this tree is a linked worktree whose main checkout holds
/// one. A tree with no index gets an empty list, so a host that starts
/// this server in every project pays no tool-schema context there.
fn advertised(root: &Path, context_dir: &Path, dir_override: bool) -> bool {
    if dir_override || has_index(context_dir) {
        return true;
    }
    match main_worktree_root(root) {
        Some(main) => has_index(&main.join(sieve_core::product().context_dir_name())),
        None => false,
    }
}

/// The context dir holds an index: a built repo (`wiring.json`) or
/// a workspace parent (`workspace.json`), both under the context dir.
fn has_index(context_dir: &Path) -> bool {
    context_dir.join(".graph").join("wiring.json").exists()
        || sieve_core::workspace::workspace_path(context_dir).exists()
}

/// Resolves `.` and `..` segments the way Node's `path.resolve` does,
/// with no filesystem read.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// The main checkout of a linked `git worktree`, or `None` when `root`
/// is not one (`mainWorktreeRoot`): `.git` is a file
/// whose `gitdir:` line points under `<main>/.git/worktrees/`, and that
/// dir's `commondir` names `<main>/.git`.
fn main_worktree_root(root: &Path) -> Option<PathBuf> {
    let dot = root.join(".git");
    if std::fs::metadata(&dot).ok()?.is_dir() {
        return None;
    }
    let text = std::fs::read_to_string(&dot).ok()?;
    let line = text.split('\n').find(|l| l.starts_with("gitdir:"))?;
    let target = line["gitdir:".len()..].trim();
    if target.is_empty() {
        return None;
    }
    let gitdir = normalize(&root.join(target));
    if gitdir.parent()?.file_name()? != "worktrees" {
        return None;
    }
    let rel = std::fs::read_to_string(gitdir.join("commondir")).ok()?;
    let rel = rel.trim();
    if rel.is_empty() {
        return None;
    }
    let common = normalize(&gitdir.join(rel));
    if common.file_name()? != ".git" || !common.is_dir() {
        return None;
    }
    let main = common.parent()?.to_path_buf();
    if normalize(&main) == normalize(root) {
        return None;
    }
    Some(main)
}

/// JSON-encodes a string, matching `JSON.stringify`: only the characters
/// JSON requires escaped are escaped; non-ASCII stays literal.
fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

/// Renders `id` as its own JSON text, or `null` when there is none.
fn id_json(id: Option<&Value>) -> String {
    match id {
        Some(v) => v.to_string(),
        None => "null".to_string(),
    }
}

/// Writes one `{"jsonrpc":"2.0",["id":...,]"result":...}` line. `has_id`
/// tells whether the reply carries an `id` key at all: `initialize`
/// answers even a request with no `id`, and that reply then has no `id`
/// key (section 2).
fn write_result_line<W: Write>(out: &mut W, has_id: bool, id: Option<&Value>, result_json: &str) {
    let line = if has_id {
        format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{result_json}}}",
            id_json(id)
        )
    } else {
        format!("{{\"jsonrpc\":\"2.0\",\"result\":{result_json}}}")
    };
    let _ = writeln!(out, "{line}");
}

/// Writes one `{"jsonrpc":"2.0","id":...,"error":{"code":...,"message":...}}`
/// line. `id: None` writes `"id":null` (the parse-error shape); every
/// other error path always carries a real `id`, because only a request
/// with an `id` reaches an error branch.
fn write_error_line<W: Write>(out: &mut W, id: Option<&Value>, code: i64, message: &str) {
    let line = format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{},\"error\":{{\"code\":{code},\"message\":{}}}}}",
        id_json(id),
        json_str(message)
    );
    let _ = writeln!(out, "{line}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn run(input: &str) -> (String, String) {
        // ponytail: skips the real refresh so these unit tests touch no
        // filesystem lock; the refresh itself is covered by the parity
        // test, which builds a real graph in a temp dir.
        std::env::set_var(sieve_core::product().env_var("NO_REFRESH"), "1");
        let root = Path::new(".");
        let stdin = Cursor::new(input.as_bytes().to_vec());
        let mut stdout = Vec::new();
        let mut stderr: Vec<u8> = Vec::new();
        serve(
            root,
            root,
            false,
            "0.1.0",
            stdin,
            &mut stdout,
            &mut stderr,
            None,
        );
        (
            String::from_utf8(stdout).unwrap(),
            String::from_utf8(stderr).unwrap(),
        )
    }

    #[test]
    fn a_malformed_line_gives_a_parse_error_with_a_null_id() {
        let (out, _) = run("not json\n");
        assert_eq!(
            out,
            "{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{\"code\":-32700,\"message\":\"parse error\"}}\n"
        );
    }

    #[test]
    fn test_p4_26_a_non_object_json_line_gets_no_reply() {
        let (out, _) = run("42\n\"str\"\n[]\ntrue\n");
        assert_eq!(out, "");
    }

    #[test]
    fn test_p4_28_method_not_found_prints_the_javascript_string_form() {
        let (out, _) =
            run("{\"jsonrpc\":\"2.0\",\"id\":22}\n{\"jsonrpc\":\"2.0\",\"id\":23,\"method\":7}\n");
        assert_eq!(
            out,
            "{\"jsonrpc\":\"2.0\",\"id\":22,\"error\":{\"code\":-32601,\"message\":\"method not found: undefined\"}}\n\
             {\"jsonrpc\":\"2.0\",\"id\":23,\"error\":{\"code\":-32601,\"message\":\"method not found: 7\"}}\n"
        );
    }

    #[test]
    fn test_p4_26_initialize_echoes_the_protocol_version_as_sent() {
        let (out, _) =
            run("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":5}}\n");
        let value: Value = serde_json::from_str(out.trim_end()).unwrap();
        assert_eq!(value["result"]["protocolVersion"], 5);
    }

    #[test]
    fn test_p4_29_tools_list_is_empty_with_no_index_and_full_under_an_override() {
        let scratch = Scratch::new("adv");
        let dir = scratch.0.clone();
        assert!(!advertised(&dir, &dir.join("sieve"), false));
        assert!(advertised(&dir, &dir.join("sieve"), true));
        std::fs::create_dir_all(dir.join("sieve")).unwrap();
        std::fs::write(dir.join("sieve").join("workspace.json"), "{}").unwrap();
        assert!(advertised(&dir, &dir.join("sieve"), false));
    }

    /// A temp dir that removes itself on drop, also after a failed
    /// assert.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("sieve-rpc-{label}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn test_p4_29_a_linked_worktree_advertises_its_main_checkout_index() {
        let scratch = Scratch::new("wt");
        let base = scratch.0.clone();
        let main = base.join("main");
        let wt = base.join("wt");
        std::fs::create_dir_all(main.join(".git").join("worktrees").join("wt")).unwrap();
        std::fs::create_dir_all(main.join("sieve").join(".graph")).unwrap();
        std::fs::write(main.join("sieve").join(".graph").join("wiring.json"), "{}").unwrap();
        std::fs::write(
            main.join(".git")
                .join("worktrees")
                .join("wt")
                .join("commondir"),
            "../..\n",
        )
        .unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", main.join(".git/worktrees/wt").display()),
        )
        .unwrap();
        assert_eq!(main_worktree_root(&wt), Some(main.clone()));
        assert_eq!(main_worktree_root(&main), None);
        assert!(advertised(&wt, &wt.join("sieve"), false));
    }

    #[test]
    fn ping_with_no_id_is_silent() {
        let (out, _) = run("{\"jsonrpc\":\"2.0\",\"method\":\"ping\"}\n");
        assert_eq!(out, "");
    }

    #[test]
    fn initialize_with_no_id_has_no_id_key() {
        let (out, _) = run("{\"jsonrpc\":\"2.0\",\"method\":\"initialize\",\"params\":{}}\n");
        let line = out.trim_end();
        let value: Value = serde_json::from_str(line).unwrap();
        assert!(value.as_object().unwrap().contains_key("result"));
        assert!(!value.as_object().unwrap().contains_key("id"));
    }

    #[test]
    fn an_alias_dispatches_to_its_canonical_tool() {
        let (out, _) = run(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"sieve_ask\",\"arguments\":{}}}\n",
        );
        let value: Value = serde_json::from_str(out.trim_end()).unwrap();
        let text = value["result"]["content"][0]["text"].as_str().unwrap();
        assert_eq!(text, "sieve_find_code requires a query");
    }

    #[test]
    fn an_unknown_tool_reports_its_own_text() {
        let (out, _) = run(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"sieve_nope\",\"arguments\":{}}}\n",
        );
        let value: Value = serde_json::from_str(out.trim_end()).unwrap();
        let text = value["result"]["content"][0]["text"].as_str().unwrap();
        assert_eq!(text, "unknown tool: sieve_nope");
        assert_eq!(value["result"]["isError"], true);
    }
}
