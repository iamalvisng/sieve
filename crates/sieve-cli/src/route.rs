//! Lookup routing (`docs/design/lookup-routing.md`).
//!
//! Slice 1 is R1. A `PostToolUse` hook on the Grep tool re-prints the
//! matches grouped by file and by enclosing symbol. The new text keeps
//! every `path:line` pair of the original. If it cannot, the hook keeps
//! the original output and records a pass reason.
//!
//! Slice 2 is R2. It wires `pre-search` and `post-search` on the Bash
//! matcher. `pre-search` adds `-n` to a simple `grep` or `rg` command.
//! `post-search` groups the output of a command that has `-n`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::hook::{self, Lookup};
use crate::intercept::is_unsafe_command;
use sieve_core::wiring::{Kind, Node};

/// The hook gives up after this long and keeps the original output. The
/// host timeout in `hosts.rs` is 3,000 ms.
const BUDGET: Duration = Duration::from_millis(2500);
/// An output of this many bytes or more passes.
const OVER_CAP_BYTES: usize = 30_000;

/// Why a lookup passed. `as_str` gives the reason the counters record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pass {
    /// The project has no index.
    NoIndex,
    /// A hit file is outside the project or the index.
    NotIndexedPath,
    /// A hit file changed after the index was built.
    Stale,
    /// The output is not `path:line:text` rows, or the response is not exact.
    Regex,
    /// The host cut the output ("Output too large").
    Truncated,
    /// The output is at or over the byte cap.
    OverCap,
    /// The hook ran past its time budget.
    Timeout,
    /// The new text is not at most 90% of the original.
    NoGain,
}

impl Pass {
    /// The reason name in the session record.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Pass::NoIndex => "no-index",
            Pass::NotIndexedPath => "not-indexed-path",
            Pass::Stale => "stale",
            Pass::Regex => "regex",
            Pass::Truncated => "truncated",
            Pass::OverCap => "over-cap",
            Pass::Timeout => "timeout",
            Pass::NoGain => "no-gain",
        }
    }
}

/// One simple `grep` or `rg` command.
struct Search<'a> {
    tool: &'a str,
    /// `-n` or `--line-number` is present.
    line_numbers: bool,
    /// A flag changes the output shape, so the hook must not touch it.
    blocked: bool,
}

/// Short flags that change the output shape. `N` is `--no-line-number` in
/// `rg`. A digit is the `-NUM` context form of `grep`.
/// `z` and `f` run a decompressor or read a pattern file.
const BLOCKED_SHORT: &str = "lLcoZqABCNzf0123456789";
/// `--pre`, `--pre-glob`, `--hostname-bin`, `--search-zip`, `--null-data`
/// and `--file` run a program or read a pattern file.
const BLOCKED_LONG: [&str; 20] = [
    "--count",
    "--files",
    "--json",
    "--context",
    "--after-context",
    "--before-context",
    "--null",
    "--only-matching",
    "--quiet",
    "--silent",
    "--no-line-number",
    "--vimgrep",
    "--column",
    "--heading",
    "--pre",
    "--pre-glob",
    "--hostname-bin",
    "--search-zip",
    "--null-data",
    "--file",
];

/// Reads one command. Returns `None` unless it is one simple `grep` or
/// `rg` call with an argument.
fn parse_search(command: &str) -> Option<Search<'_>> {
    let command = command.trim();
    if is_unsafe_command(command) {
        return None;
    }
    let mut tokens = command.split_whitespace();
    let tool = tokens.next().filter(|t| matches!(*t, "grep" | "rg"))?;
    let mut search = Search {
        tool,
        line_numbers: false,
        blocked: false,
    };
    let mut args = 0;
    for tok in tokens {
        args += 1;
        if tok == "--" {
            break;
        }
        // A quoted token is part of a pattern, never a flag.
        if tok.contains('\'') || !tok.starts_with('-') || tok == "-" {
            continue;
        }
        if tok == "--line-number" {
            search.line_numbers = true;
        } else if tok.starts_with("--") {
            search.blocked |= BLOCKED_LONG.iter().any(|f| tok.starts_with(f));
        } else {
            let cluster = &tok[1..];
            search.line_numbers |= cluster.contains('n');
            search.blocked |= cluster.chars().any(|c| BLOCKED_SHORT.contains(c));
        }
    }
    (args > 0).then_some(search)
}

/// `PreToolUse`: the `updatedInput` JSON that adds `-n`, or `None`.
pub(crate) fn pre_search(input: &Value) -> Option<String> {
    let tool_input = input.get("tool_input")?;
    let command = tool_input.get("command").and_then(Value::as_str)?;
    let search = parse_search(command)?;
    if search.line_numbers || search.blocked {
        return None;
    }
    let command = command.trim();
    let mut updated = tool_input.clone();
    updated["command"] = Value::from(format!(
        "{} -n{}",
        search.tool,
        &command[search.tool.len()..]
    ));
    // `permissionDecision: "allow"` follows the F1 convention in `hook.rs`,
    // where a rewrite is a real tool call. The blocked flags keep the command
    // read-only. The hook does not widen the host Bash permission flow
    // beyond what F1 does.
    Some(
        serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "allow",
                "updatedInput": updated,
            },
        })
        .to_string(),
    )
}

/// `PostToolUse`: the `updatedToolOutput` JSON for a routed search, or
/// `None`. The function counts the lookup either way. It reads a Grep tool
/// call (R1) or a Bash `grep` or `rg` call (R2).
pub(crate) fn post_search(input: &Value, project_dir: &Path) -> Option<String> {
    let tool_input = input.get("tool_input")?;
    let response = input.get("tool_response")?;
    if input.get("tool_name").and_then(Value::as_str) == Some("Grep") {
        // Only the `content` mode holds `path:line:text` rows.
        let mode = tool_input.get("output_mode").and_then(Value::as_str);
        let Some(text) = (mode == Some("content"))
            .then(|| response_text(response, "content"))
            .flatten()
        else {
            hook::record_lookup(project_dir, input, Lookup::Passed(Pass::Regex.as_str()));
            return None;
        };
        return finish(input, project_dir, response, "content", text);
    }
    // The count depends on the PostToolUse `tool_input.command` holding the
    // rewritten command with `-n`. The F2 live probe must confirm it.
    let command = tool_input.get("command").and_then(Value::as_str)?;
    let search = parse_search(command)?;
    if search.blocked || !search.line_numbers {
        return None;
    }
    // The Bash `tool_response` holds `stdout` and `stderr` in every test of
    // this crate. A plain string is also read.
    let text = response_text(response, "stdout")?;
    // R2: the Bash output may be cut by the host, so a large one passes.
    if text.len() >= OVER_CAP_BYTES {
        hook::record_lookup(project_dir, input, Lookup::Passed(Pass::Truncated.as_str()));
        return None;
    }
    finish(input, project_dir, response, "stdout", text)
}

/// The output text of a `tool_response`: the string itself, or its `key`.
fn response_text<'a>(response: &'a Value, key: &str) -> Option<&'a str> {
    match response {
        Value::String(s) => Some(s.as_str()),
        other => other.get(key).and_then(Value::as_str),
    }
}

/// Routes `text`, counts the lookup and builds the hook JSON.
fn finish(
    input: &Value,
    project_dir: &Path,
    response: &Value,
    key: &str,
    text: &str,
) -> Option<String> {
    match route_output(input, project_dir, text, Instant::now()) {
        Ok(new_text) => {
            hook::record_lookup(project_dir, input, Lookup::Routed);
            // A real Grep result in content mode is an object with `mode`,
            // `numFiles`, `filenames`, `content`, `numLines` and `totalLines`.
            // The hook keeps every field and replaces only the text.
            let mut updated = response.clone();
            match &mut updated {
                Value::String(s) => *s = new_text,
                other => other[key] = Value::from(new_text),
            }
            Some(
                serde_json::json!({
                    "hookSpecificOutput": {
                        "hookEventName": "PostToolUse",
                        "updatedToolOutput": updated,
                    },
                })
                .to_string(),
            )
        }
        Err(pass) => {
            hook::record_lookup(project_dir, input, Lookup::Passed(pass.as_str()));
            None
        }
    }
}

/// One parsed `path:line:text` line.
struct Hit<'a> {
    path: &'a str,
    line: u32,
    text: &'a str,
}

/// Parses every line of `out`. Returns `None` if one line does not parse.
fn parse_hits(out: &str) -> Option<Vec<Hit<'_>>> {
    let body = out.strip_suffix('\n').unwrap_or(out);
    body.split('\n')
        .map(|row| {
            let (path, rest) = row.split_once(':')?;
            // A one-file search prints `line:text` with no path.
            if path.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let (line, text) = rest.split_once(':')?;
            Some(Hit {
                path,
                line: line.parse().ok().filter(|n| *n > 0)?,
                text,
            })
        })
        .collect()
}

/// Splits a span such as `L3-L9`.
fn parse_span(span: &str) -> Option<(u32, u32)> {
    let (start, end) = span.strip_prefix('L')?.split_once("-L")?;
    Some((start.parse().ok()?, end.parse().ok()?))
}

/// The index path of `raw`, a path as `rg` printed it, or `None` if the
/// file is outside the project.
fn index_path(raw: &str, cwd: &Path, root: &Path) -> Option<String> {
    let joined = cwd.join(raw);
    let full = std::fs::canonicalize(joined).ok()?;
    let rel = full.strip_prefix(root).ok()?;
    Some(rel.to_string_lossy().into_owned())
}

/// Builds the grouped text, or the reason to pass.
fn route_output(
    input: &Value,
    project_dir: &Path,
    out: &str,
    started: Instant,
) -> Result<String, Pass> {
    if out.len() >= OVER_CAP_BYTES {
        return Err(Pass::OverCap);
    }
    if out.contains("Output too large") {
        return Err(Pass::Truncated);
    }
    let hits = parse_hits(out).ok_or(Pass::Regex)?;
    // A row holds no line number when `-n` is false, so a `12:x` text would
    // invent a line. A count or a limit in the response means the rows may
    // not be the full set.
    if input.pointer("/tool_input/-n") == Some(&Value::Bool(false)) {
        return Err(Pass::Regex);
    }
    if let Some(response) = input.get("tool_response") {
        let counted = response
            .get("numLines")
            .and_then(Value::as_u64)
            .is_some_and(|n| n != hits.len() as u64);
        if counted
            || response.get("appliedLimit").is_some()
            || response.get("appliedOffset").is_some()
        {
            return Err(Pass::Regex);
        }
    }
    let context_dir = hook::resolve_context_dir(project_dir);
    let root = std::fs::canonicalize(project_dir).map_err(|_| Pass::NotIndexedPath)?;
    let cwd = input
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
        .map_or_else(|| root.clone(), PathBuf::from);
    let wiring_time = std::fs::metadata(hook::wiring_path(&context_dir))
        .and_then(|m| m.modified())
        .map_err(|_| Pass::NoIndex)?;

    // Map each printed path to its index path. A file outside the project,
    // or a file newer than the index, passes the whole output.
    let mut rel_of: HashMap<&str, String> = HashMap::new();
    for hit in &hits {
        if rel_of.contains_key(hit.path) {
            continue;
        }
        let rel = index_path(hit.path, &cwd, &root).ok_or(Pass::NotIndexedPath)?;
        let fresh = std::fs::metadata(root.join(&rel))
            .and_then(|m| m.modified())
            .is_ok_and(|t| t <= wiring_time);
        if !fresh {
            return Err(Pass::Stale);
        }
        rel_of.insert(hit.path, rel);
    }
    // The wiring read and the freshness check cost the most, so they run last.
    let graph = hook::read_wiring(&context_dir).ok_or(Pass::NoIndex)?;
    if hook::index_freshness(project_dir, &context_dir).is_some_and(|f| f.missing > 0) {
        return Err(Pass::Stale);
    }
    let wanted: HashSet<&str> = rel_of.values().map(String::as_str).collect();
    let mut indexed: HashSet<&str> = HashSet::new();
    let mut symbols: HashMap<&str, Vec<(&Node, u32, u32)>> = HashMap::new();
    for node in &graph.nodes {
        if !wanted.contains(node.path.as_str()) {
            continue;
        }
        if node.kind == Kind::File {
            indexed.insert(node.path.as_str());
        } else if let Some((start, end)) = parse_span(&node.span) {
            symbols
                .entry(node.path.as_str())
                .or_default()
                .push((node, start, end));
        }
    }
    if rel_of.values().any(|rel| !indexed.contains(rel.as_str())) {
        return Err(Pass::NotIndexedPath);
    }

    // Group by printed path, in order of first appearance.
    let mut order: Vec<&str> = Vec::new();
    let mut by_file: HashMap<&str, Vec<&Hit>> = HashMap::new();
    for hit in &hits {
        by_file
            .entry(hit.path)
            .or_insert_with(|| {
                order.push(hit.path);
                Vec::new()
            })
            .push(hit);
    }
    let mut text = String::from("[sieve] routed:replaced\n");
    let mut printed: HashSet<(&str, u32)> = HashSet::new();
    for path in &order {
        text.push_str(path);
        text.push('\n');
        let syms = symbols.get(rel_of[path].as_str());
        for hit in &by_file[path] {
            printed.insert((hit.path, hit.line));
            match syms.and_then(|s| enclosing(s, hit.line)) {
                Some((node, _, _)) => {
                    let name = node
                        .id
                        .split_once('#')
                        .map_or(node.name.as_str(), |(_, n)| n);
                    let kind = match node.kind {
                        Kind::Function => "fn",
                        _ => "sym",
                    };
                    text.push_str(&format!("  L{} in {kind} {name}: {}\n", hit.line, hit.text));
                }
                None => text.push_str(&format!("  L{}: {}\n", hit.line, hit.text)),
            }
        }
    }
    // The set of path:line pairs must equal the original, line for line.
    let original: HashSet<(&str, u32)> = hits.iter().map(|h| (h.path, h.line)).collect();
    if printed != original || original.len() != hits.len() {
        return Err(Pass::Regex);
    }
    if !out.ends_with('\n') {
        text.pop();
    }
    if started.elapsed() > BUDGET {
        return Err(Pass::Timeout);
    }
    if text.len() * 10 > out.len() * 9 {
        return Err(Pass::NoGain);
    }
    Ok(text)
}

/// The innermost symbol around `line`: the greatest start wins, and a tie
/// keeps the later node (the rule of `sieve grep`).
fn enclosing<'a>(symbols: &[(&'a Node, u32, u32)], line: u32) -> Option<(&'a Node, u32, u32)> {
    let mut best: Option<(&Node, u32, u32)> = None;
    for &(node, start, end) in symbols {
        if start <= line && line <= end && best.is_none_or(|(_, s, _)| start >= s) {
            best = Some((node, start, end));
        }
    }
    best
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    /// A scratch dir that removes itself on drop.
    pub(crate) struct Scratch(pub(crate) PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            let dir = std::env::temp_dir().join(format!(
                "sieve-route-{label}-{}-{nanos}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).expect("create scratch dir");
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// 12 Rust files, each with 4 functions, plus a README the index lacks.
    /// The graph is built and written under the context dir.
    pub(crate) fn fixture(label: &str) -> Scratch {
        let dir = Scratch::new(label);
        let src = dir
            .0
            .join("crates")
            .join("engine")
            .join("src")
            .join("handlers");
        std::fs::create_dir_all(&src).expect("mkdir");
        for i in 0..12 {
            let mut body = format!("//! Handlers of group {i}.\n\n");
            for f in 0..4 {
                body.push_str(&format!(
                    "pub fn handle_{i}_{f}(input: u64) -> u64 {{\n    \
                     let total = input + {f};\n    let total = total * {i};\n    \
                     // TODO tidy group {i}\n    total\n}}\n\n"
                ));
            }
            std::fs::write(src.join(format!("group_{i:02}_handlers.rs")), body).expect("write");
        }
        std::fs::write(dir.0.join("README.md"), "total handlers TODO\n").expect("readme");
        let ctx = hook::resolve_context_dir(&dir.0);
        let graph = sieve_parse::build_graph(&dir.0, &ctx).expect("build graph");
        sieve_core::write_graph(&graph, &ctx.join(".graph").join("wiring.json")).expect("write");
        dir
    }

    fn post_input(root: &Path, command: &str, stdout: &str, agent: Option<&str>) -> Value {
        let mut input = serde_json::json!({
            "session_id": "s1",
            "cwd": root.to_string_lossy(),
            "tool_name": "Bash",
            "tool_input": {"command": command},
            "tool_response": {"stdout": stdout, "stderr": ""},
        });
        if let Some(a) = agent {
            input["agent_id"] = Value::from(a);
        }
        input
    }

    pub(crate) fn new_stdout(json: &str) -> String {
        let v: Value = serde_json::from_str(json).expect("json");
        v["hookSpecificOutput"]["updatedToolOutput"]["stdout"]
            .as_str()
            .expect("stdout")
            .to_string()
    }

    /// The path:line pairs of routed text: a path header, then `  L<n>` rows.
    pub(crate) fn routed_pairs(text: &str) -> HashSet<(String, u32)> {
        let mut pairs = HashSet::new();
        let mut path = String::new();
        for row in text.lines().skip(1) {
            match row.strip_prefix("  L") {
                Some(rest) => {
                    let n: String = rest.chars().take_while(char::is_ascii_digit).collect();
                    pairs.insert((path.clone(), n.parse().expect("line")));
                }
                None => path = row.to_string(),
            }
        }
        pairs
    }

    pub(crate) fn original_pairs(out: &str) -> HashSet<(String, u32)> {
        let hits = parse_hits(out).expect("parse");
        hits.iter().map(|h| (h.path.to_string(), h.line)).collect()
    }

    pub(crate) fn run_rg(root: &Path, pattern: &str) -> String {
        let out = Command::new("rg")
            .args(["-n", "--no-config", "-g", "!sieve", "--", pattern, "."])
            .current_dir(root)
            .stdin(Stdio::null())
            .output()
            .expect("run rg");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[test]
    fn test_route_bash_keeps_every_match() {
        if Command::new("rg").arg("--version").output().is_err() {
            println!("skip: rg is not installed");
            return;
        }
        let dir = fixture("keeps");
        let mut patterns: Vec<String> = [
            "total", "TODO", "input", "handle", "pub", "fn", "u64", "group",
        ]
        .map(String::from)
        .to_vec();
        for i in 0..12 {
            patterns.push(format!("handle_{i}_"));
        }
        for f in 0..4 {
            patterns.push(format!("input_{f}"));
            patterns.push(format!("input + {f}"));
            patterns.push(format!("handle_[0-9]+_{f}"));
        }
        for i in [2, 5, 9] {
            patterns.push(format!("group {i}"));
            patterns.push(format!("total * {i}"));
            patterns.push(format!("Handlers of group {i}"));
        }
        patterns.extend(
            [
                "nomatchanywhere",
                "handlers",
                "README",
                "tidy",
                "return",
                "let total",
            ]
            .map(String::from),
        );
        assert!(patterns.len() >= 40, "{}", patterns.len());
        let (mut routed, mut passed) = (0, 0);
        for pattern in &patterns {
            let out = run_rg(&dir.0, pattern);
            let command = format!("rg -n {}", pattern.replace(' ', "_"));
            let input = post_input(&dir.0, &command, &out, None);
            match post_search(&input, &dir.0) {
                Some(json) => {
                    routed += 1;
                    let text = new_stdout(&json);
                    assert!(text.starts_with("[sieve] routed:replaced"), "{text}");
                    assert_eq!(routed_pairs(&text), original_pairs(&out), "{pattern}");
                    assert!(text.len() * 10 <= out.len() * 9, "{pattern}");
                }
                None => passed += 1,
            }
        }
        println!("routed {routed}, passed {passed} of {}", patterns.len());
        assert!(routed > 0 && passed > 0, "routed {routed}, passed {passed}");
        // A README hit is outside the index, so the original bytes stay.
        let readme = run_rg(&dir.0, "README|handlers TODO");
        let input = post_input(&dir.0, "rg -n README", &readme, None);
        assert!(post_search(&input, &dir.0).is_none());
    }

    #[test]
    fn test_route_bash_passes_on_parse_loss() {
        let dir = fixture("loss");
        let good = "crates/engine/src/handlers/group_00_handlers.rs";
        let mut out = String::new();
        for line in [3, 9, 15, 21, 27] {
            out.push_str(&format!(
                "{good}:{line}:    let total = input + 1; // long padding\n"
            ));
        }
        let ok = post_input(&dir.0, "rg -n total", &out, None);
        assert!(
            post_search(&ok, &dir.0).is_some(),
            "the clean output must route"
        );
        out.push_str("Binary file crates/engine/src/x.bin matches\n");
        let bad = post_input(&dir.0, "rg -n total", &out, None);
        assert!(post_search(&bad, &dir.0).is_none());
        let session = hook::read_session(&dir.0, "s1");
        assert_eq!(session.lookup_pass_reasons.get("regex"), Some(&1));
    }

    #[test]
    fn test_route_bash_passes_on_stale() {
        let dir = fixture("stale");
        let file = "crates/engine/src/handlers/group_00_handlers.rs";
        let mut out = String::new();
        for line in [3, 9, 15, 21, 27] {
            out.push_str(&format!(
                "{file}:{line}:    let total = input + 1; // long padding\n"
            ));
        }
        let input = post_input(&dir.0, "rg -n total", &out, None);
        let later = std::time::SystemTime::now() + Duration::from_secs(3600);
        let handle = std::fs::File::options()
            .write(true)
            .open(dir.0.join(file))
            .expect("open");
        handle.set_modified(later).expect("set mtime");
        assert!(post_search(&input, &dir.0).is_none());
        let session = hook::read_session(&dir.0, "s1");
        assert_eq!(session.lookup_pass_reasons.get("stale"), Some(&1));
    }

    #[test]
    fn test_route_adds_n_only_to_simple_search() {
        // (command, the command after the hook, None when the hook passes)
        let table: [(&str, Option<&str>); 23] = [
            ("grep foo src", Some("grep -n foo src")),
            ("rg foo", Some("rg -n foo")),
            ("grep -r foo src", Some("grep -n -r foo src")),
            ("rg -i foo src", Some("rg -n -i foo src")),
            ("grep -rn foo src", None),
            ("rg --line-number foo", None),
            ("rg -N foo", None),
            ("rg --no-line-number foo", None),
            ("grep -rl foo src", None),
            ("grep -c foo file.rs", None),
            ("rg -C 2 foo", None),
            ("rg --json foo", None),
            ("rg --pre ./run foo", None),
            ("rg --pre-glob '*.gz' foo", None),
            ("rg --hostname-bin ./h foo", None),
            ("rg -z foo", None),
            ("rg --search-zip foo", None),
            ("rg --null-data foo", None),
            ("grep -f pats src", None),
            ("rg --file pats", None),
            ("grep foo src | head", None),
            ("grep foo a && grep foo b", None),
            ("cat src/lib.rs", None),
        ];
        for (command, want) in table {
            let input = serde_json::json!({"tool_input": {"command": command, "description": "d"}});
            let got = pre_search(&input).map(|json| {
                let v: Value = serde_json::from_str(&json).expect("json");
                let updated = &v["hookSpecificOutput"]["updatedInput"];
                assert_eq!(updated["description"], "d", "{command}");
                updated["command"].as_str().expect("command").to_string()
            });
            assert_eq!(got.as_deref(), want, "{command}");
        }
    }

    #[test]
    fn test_route_bash_over_30k_passes_truncated() {
        let dir = fixture("over30k");
        let good = "crates/engine/src/handlers/group_00_handlers.rs";
        let row = format!("{good}:3:    let total = input + 1;\n");
        let out = row.repeat(OVER_CAP_BYTES / row.len() + 1);
        assert!(out.len() >= OVER_CAP_BYTES);
        let input = post_input(&dir.0, "rg -n total", &out, None);
        assert!(post_search(&input, &dir.0).is_none());
        let session = hook::read_session(&dir.0, "s1");
        assert_eq!(session.lookup_pass_reasons.get("truncated"), Some(&1));
    }

    #[test]
    fn test_route_bash_counts_per_agent() {
        let dir = fixture("agents");
        let file = "crates/engine/src/handlers/group_00_handlers.rs";
        let mut out = String::new();
        for line in [3, 9, 15, 21, 27] {
            out.push_str(&format!(
                "{file}:{line}:    let total = input + 1; // long padding\n"
            ));
        }
        let routed = |agent: Option<&str>| post_input(&dir.0, "rg -n total", &out, agent);
        assert!(post_search(&routed(Some("a1")), &dir.0).is_some());
        assert!(post_search(&routed(Some("a1")), &dir.0).is_some());
        assert!(post_search(&routed(Some("a2")), &dir.0).is_some());
        // A search with a missing file passes for the main agent.
        let miss = post_input(&dir.0, "rg -n total", "nowhere.rs:1:total x\n", None);
        assert!(post_search(&miss, &dir.0).is_none());
        // A Sieve call is a pick.
        let pick = serde_json::json!({
            "session_id": "s1", "agent_id": "a2", "cwd": dir.0.to_string_lossy(),
            "tool_name": "Bash", "tool_input": {"command": "sieve grep total"},
            "tool_response": {"stdout": ""},
        });
        hook::record_lookup(&dir.0, &pick, Lookup::Picked);
        let a1 = hook::read_session(&dir.0, "s1.a1");
        let a2 = hook::read_session(&dir.0, "s1.a2");
        let main = hook::read_session(&dir.0, "s1");
        assert_eq!(
            (a1.lookups_routed, a1.lookups_passed, a1.lookups_picked),
            (2, 0, 0)
        );
        assert_eq!(
            (a2.lookups_routed, a2.lookups_passed, a2.lookups_picked),
            (1, 0, 1)
        );
        assert_eq!((main.lookups_routed, main.lookups_passed), (0, 1));
        assert_eq!(main.lookup_pass_reasons.get("not-indexed-path"), Some(&1));
        assert_eq!(a1.lookups_by_day.values().map(|d| d.routed).sum::<u64>(), 2);
        // A session with no lookups writes none of the new keys.
        let bare = serde_json::to_string(&hook::SessionState::default()).expect("json");
        assert!(!bare.contains("lookup"), "{bare}");
    }

    #[test]
    fn test_route_grep_passes_without_line_numbers() {
        let dir = fixture("grep-nonum");
        let mut input = post_input(&dir.0, "x", &"a.rs:5:text\n".repeat(3), None);
        input["tool_name"] = Value::from("Grep");
        input["tool_input"] = serde_json::json!({"output_mode": "content", "-n": false});
        input["tool_response"] =
            Value::from("crates/engine/src/handlers/group_00_handlers.rs:12:x\n");
        assert!(post_search(&input, &dir.0).is_none());
        let session = hook::read_session(&dir.0, "s1");
        assert_eq!(session.lookup_pass_reasons.get("regex"), Some(&1));
    }

    #[test]
    fn test_route_grep_passes_on_numlines_mismatch() {
        let dir = fixture("grep-numlines");
        let file = "crates/engine/src/handlers/group_00_handlers.rs";
        let mut out = String::new();
        for line in [3, 9, 15, 21, 27, 33, 39, 45] {
            out.push_str(&format!(
                "{file}:{line}:    let total = input + 1; // long padding\n"
            ));
        }
        let mut input = post_input(&dir.0, "x", "", None);
        input["tool_name"] = Value::from("Grep");
        input["tool_input"] = serde_json::json!({"output_mode": "content"});
        input["tool_response"] = serde_json::json!({"content": out, "numLines": 8});
        assert!(
            post_search(&input, &dir.0).is_some(),
            "a matching count routes"
        );
        input["tool_response"]["numLines"] = Value::from(9);
        assert!(post_search(&input, &dir.0).is_none());
        input["tool_response"]["numLines"] = Value::from(8);
        input["tool_response"]["appliedLimit"] = Value::from(8);
        assert!(post_search(&input, &dir.0).is_none());
        let session = hook::read_session(&dir.0, "s1");
        assert_eq!(
            (
                session.lookups_routed,
                session.lookup_pass_reasons.get("regex")
            ),
            (1, Some(&2))
        );
    }

    #[test]
    fn test_route_grep_passes_on_single_file_rows() {
        let dir = fixture("grep-single");
        let mut input = post_input(&dir.0, "x", "", None);
        input["tool_name"] = Value::from("Grep");
        input["tool_input"] = serde_json::json!({"output_mode": "content"});
        for content in [
            "313:export function checkApiKeyRateLimit(",
            "313:12:export function a(): void",
        ] {
            input["tool_response"] = serde_json::json!({
                "mode": "content", "numFiles": 0, "filenames": [],
                "content": content, "numLines": 1, "totalLines": 1,
            });
            assert!(post_search(&input, &dir.0).is_none(), "{content}");
        }
        let session = hook::read_session(&dir.0, "s1");
        assert_eq!(session.lookup_pass_reasons.get("regex"), Some(&2));
    }
}
