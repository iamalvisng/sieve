//! Lookup routing (`docs/design/lookup-routing.md` and
//! `docs/design/lookup-routing-slice3.md`).
//!
//! Slice 1 is R1. A `PostToolUse` hook on the Grep tool re-prints the
//! matches grouped by file and by enclosing symbol. The new text keeps
//! every `path:line` pair of the original, in the original order. If it
//! cannot, the hook keeps the original output and records a pass reason.
//!
//! Slice 3 routes Bash on the stdout. The lexer in `lex.rs` splits the
//! command into segments. Each search segment counts as one lookup.
//!
//! Slice 3b splits the stdout into runs of lines. A run of `path:line:text`
//! rows with current indexed files becomes one regrouped block. Every other
//! line stays in place, byte for byte. Every search segment of the call gets
//! the same outcome. The hook adds no `-n` and never changes a command.
//!
//! The reason order, first match wins: `unsafe`, `no-index`, `truncated`,
//! `compound`, `pipe`, `no-n`, `regex`, `non-code`, `not-indexed-path`,
//! `stale`, `timeout`, `no-gain`. `over-cap` stays for the Grep tool.

mod lex;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::hook::{self, Lookup};
use lex::{classify, lex, mentions_search, Kind, Lexed, Segment, Sep};
use sieve_core::wiring::Kind as NodeKind;

/// The hook gives up after this long and keeps the original output. The
/// host timeout in `hosts.rs` is 3,000 ms.
const BUDGET: Duration = Duration::from_millis(2500);
/// An output of this many bytes or more passes.
const OVER_CAP_BYTES: usize = 30_000;
/// The hook reads a one-file search result against its file up to this size.
const MAX_VERIFY_BYTES: usize = 8 * 1024 * 1024;

/// Why a lookup passed. `as_str` gives the reason the counters record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pass {
    /// The command holds text the lexer does not read, or a flag that runs
    /// a program or reads a pattern file.
    Unsafe,
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
    /// A `cd` the hook cannot follow, or a bare `line:text` row in a call
    /// with two or more searches.
    Compound,
    /// A pipe changed the output so that the rows do not parse.
    Pipe,
    /// A search segment has no `-n`.
    NoN,
    /// Every hit file is a document, not code.
    NonCode,
}

impl Pass {
    /// The reason name in the session record.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Pass::Unsafe => "unsafe",
            Pass::NoIndex => "no-index",
            Pass::NotIndexedPath => "not-indexed-path",
            Pass::Stale => "stale",
            Pass::Regex => "regex",
            Pass::Truncated => "truncated",
            Pass::OverCap => "over-cap",
            Pass::Timeout => "timeout",
            Pass::NoGain => "no-gain",
            Pass::Compound => "compound",
            Pass::Pipe => "pipe",
            Pass::NoN => "no-n",
            Pass::NonCode => hook::NON_CODE,
        }
    }
}

/// What the hook reads from one search segment.
#[derive(Debug, Default)]
struct SearchArgs {
    /// `-n` or `--line-number` is present.
    line_numbers: bool,
    /// `-r`, `-R` or `--recursive` is present: the path is a directory.
    recursive: bool,
    /// A flag changes the output shape, so the hook must not touch it.
    blocked: bool,
    /// A flag runs a program or reads a pattern file.
    unsafe_flag: bool,
    /// The path arguments, after the pattern.
    paths: Vec<String>,
    /// The file of a `<file` redirect.
    stdin: Option<String>,
}

/// The directory a leading `cd` sets.
#[derive(Debug, PartialEq)]
enum Cd {
    /// The call has no `cd`.
    None,
    /// The first segment is `cd <dir>` and no other `cd` follows.
    Dir(String),
    /// A later `cd`, or a directory the hook cannot resolve.
    Compound,
}

/// One Bash call, read by the lexer.
#[derive(Debug)]
struct Plan {
    /// The search segments that are not right of a pipe.
    searches: Vec<SearchArgs>,
    /// The segments that write to the call's stdout: the searches and every
    /// other command that no pipe feeds.
    sources: usize,
    /// The call holds a pipe.
    piped: bool,
    cd: Cd,
}

/// The lexer's verdict on one Bash call.
#[derive(Debug)]
enum Analysis {
    /// The call holds no search segment, so it gets no record.
    NoSearch,
    /// The call is `unsafe`, with one record.
    Unsafe,
    /// The call holds one or more search segments.
    Plan(Plan),
}

/// Short flags that change the output shape. `N` is `--no-line-number` in
/// `rg`. A digit is the `-NUM` context form of `grep`.
/// `z` and `f` run a decompressor or read a pattern file.
const BLOCKED_SHORT: &str = "lLcZqABCNzf0123456789";
/// `--pre`, `--pre-glob`, `--hostname-bin`, `--search-zip`, `--null-data`
/// and `--file` run a program or read a pattern file.
const BLOCKED_LONG: [&str; 19] = [
    "--count",
    "--files",
    "--json",
    "--context",
    "--after-context",
    "--before-context",
    "--null",
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
/// The long flags that pass with `unsafe`, by exact name.
const UNSAFE_LONG: [&str; 5] = ["pre", "pre-glob", "hostname-bin", "search-zip", "file"];
/// Short flags that take a value: the next token, or the rest of the cluster.
const VALUED_SHORT: &str = "efmgtABCdD";
/// More short flags of `rg` that take a value.
const VALUED_SHORT_RG: &str = "EMjrT";
/// Long flags that take a value in the next token when no `=` follows.
const VALUED_LONG: [&str; 18] = [
    "regexp",
    "file",
    "max-count",
    "glob",
    "iglob",
    "type",
    "type-not",
    "after-context",
    "before-context",
    "context",
    "include",
    "exclude",
    "exclude-dir",
    "max-depth",
    "threads",
    "sort",
    "sortr",
    "replace",
];

/// Reads the flags and the path arguments of one search segment.
fn parse_args(seg: &Segment, rg: bool, start: usize) -> SearchArgs {
    let mut args = SearchArgs {
        stdin: seg.stdin.clone(),
        ..SearchArgs::default()
    };
    let mut have_pattern = false;
    let mut flags_done = false;
    let mut words = seg.words[start..].iter();
    while let Some(word) = words.next() {
        let text = word.text.as_str();
        if flags_done || !text.starts_with('-') || text == "-" {
            if have_pattern {
                args.paths.push(text.to_string());
            }
            have_pattern = true;
            continue;
        }
        if text == "--" {
            flags_done = true;
            continue;
        }
        if let Some(long) = text.strip_prefix("--") {
            let (name, has_value) = match long.split_once('=') {
                Some((name, _)) => (name, true),
                None => (long, false),
            };
            args.line_numbers |= name == "line-number";
            args.recursive |= name == "recursive";
            args.blocked |= BLOCKED_LONG.iter().any(|f| text.starts_with(f));
            args.unsafe_flag |= UNSAFE_LONG.contains(&name);
            have_pattern |= name == "regexp";
            if !has_value && VALUED_LONG.contains(&name) {
                words.next();
            }
            continue;
        }
        let mut cluster = text[1..].chars();
        while let Some(ch) = cluster.next() {
            args.line_numbers |= ch == 'n';
            args.recursive |= matches!(ch, 'r' | 'R');
            args.blocked |= BLOCKED_SHORT.contains(ch);
            args.unsafe_flag |= matches!(ch, 'z' | 'f');
            if VALUED_SHORT.contains(ch) || (rg && VALUED_SHORT_RG.contains(ch)) {
                have_pattern |= ch == 'e';
                if cluster.as_str().is_empty() {
                    words.next();
                }
                break;
            }
        }
    }
    args
}

/// Reads a first `cd <dir>` segment that ends in `&&` or `;`. Any other `cd`
/// form is `compound`: a `&` runs the `cd` in a background subshell.
fn cd_target(index: usize, seg: &Segment) -> Cd {
    match seg.words.as_slice() {
        [cd, dir]
            if index == 0
                && cd.text == "cd"
                && matches!(seg.sep, Sep::And | Sep::Semi)
                && !seg.piped_out
                && !dir.text.contains(['~', '$', '*', '?', '[', ' '])
                && !dir.text.starts_with('-') =>
        {
            Cd::Dir(dir.text.clone())
        }
        _ => Cd::Compound,
    }
}

/// Reads one Bash command.
fn analyze(command: &str) -> Analysis {
    let unreadable = || {
        if hook::command_invokes_sieve(command) {
            Analysis::NoSearch
        } else if mentions_search(command) {
            Analysis::Unsafe
        } else {
            Analysis::NoSearch
        }
    };
    let Lexed::Segments(segments) = lex(command) else {
        return unreadable();
    };
    let mut plan = Plan {
        searches: Vec::new(),
        sources: 0,
        piped: false,
        cd: Cd::None,
    };
    let mut cds = 0;
    for (index, seg) in segments.iter().enumerate() {
        plan.piped |= seg.piped_in || seg.piped_out;
        match classify(seg) {
            Kind::Keyword | Kind::FanOut => return unreadable(),
            Kind::Cd => {
                cds += 1;
                plan.cd = cd_target(index, seg);
            }
            // A search right of a pipe reads stdin: it is a filter.
            Kind::Search { .. } if seg.piped_in => {}
            Kind::Search { rg, start } => {
                plan.searches.push(parse_args(seg, rg, start));
                plan.sources += 1;
            }
            Kind::Other => plan.sources += usize::from(!seg.piped_in),
        }
    }
    if cds > 1 {
        plan.cd = Cd::Compound;
    }
    if plan.searches.is_empty() {
        Analysis::NoSearch
    } else {
        Analysis::Plan(plan)
    }
}

impl Plan {
    /// The one file a `line:text` row can belong to: the call has one search
    /// segment and no other command writes, and that segment names one path
    /// or reads one `<file`.
    fn single_arg(&self) -> Option<&str> {
        if self.searches.len() != 1 || self.sources != 1 {
            return None;
        }
        let search = &self.searches[0];
        match (search.paths.as_slice(), &search.stdin) {
            ([path], _) => Some(path.as_str()),
            ([], Some(file)) => Some(file.as_str()),
            _ => None,
        }
    }

    /// A flag that runs a program or reads a pattern file.
    fn has_unsafe_flag(&self) -> bool {
        self.searches.iter().any(|s| s.unsafe_flag)
    }
}

/// The reasons that follow from the call shape and the stdout bytes, in
/// order: `truncated`, `compound`, `pipe`, `no-n`. `compound` means a `cd`
/// the hook cannot follow, or a bare `line:text` row in a call with two or
/// more searches. `single_file` is true when the one path argument is a
/// regular file. `exists` tells whether a printed path names a file.
fn shape_reason(
    plan: &Plan,
    out: &str,
    single_file: bool,
    exists: &dyn Fn(&str) -> bool,
) -> Option<Pass> {
    if out.len() >= OVER_CAP_BYTES || out.contains("Output too large") {
        return Some(Pass::Truncated);
    }
    if plan.cd == Cd::Compound {
        return Some(Pass::Compound);
    }
    let single = plan.single_arg().filter(|_| single_file);
    let pieces = parse_pieces(out, single);
    // A bare row names no file, so two searches cannot own it.
    if plan.searches.len() > 1 && pieces.iter().any(|p| bare_row(p.raw)) {
        return Some(Pass::Compound);
    }
    // A pipe stage that edits the path column, such as `uniq -c` or `sed`,
    // leaves a path that names no file.
    let rows_exist = pieces
        .iter()
        .filter_map(|p| p.hit.as_ref())
        .any(|h| exists(h.path));
    if plan.piped && !pieces.is_empty() && !rows_exist {
        return Some(Pass::Pipe);
    }
    plan.searches
        .iter()
        .any(|s| !s.line_numbers)
        .then_some(Pass::NoN)
}

/// `PostToolUse`: the `updatedToolOutput` JSON for a routed search, or
/// `None`. The function counts the lookup either way. It reads a Grep tool
/// call (R1) or a Bash call with one or more `grep` or `rg` segments (R2).
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
            let pass = Lookup::Passed(Pass::Regex.as_str());
            hook::record_lookup(project_dir, input, pass, 1);
            return None;
        };
        let result = route_output(input, project_dir, text, Instant::now());
        return finish(
            input,
            project_dir,
            response,
            "content",
            result,
            (Lookup::Routed, 1),
        );
    }
    let command = tool_input.get("command").and_then(Value::as_str)?;
    let plan = match analyze(command) {
        Analysis::NoSearch => return None,
        Analysis::Unsafe => {
            let pass = Lookup::Passed(Pass::Unsafe.as_str());
            hook::record_lookup(project_dir, input, pass, 1);
            return None;
        }
        Analysis::Plan(plan) => plan,
    };
    // The Bash `tool_response` holds `stdout` and `stderr` in every test of
    // this crate. A plain string is also read.
    let text = response_text(response, "stdout")?;
    let result = route_bash(input, project_dir, &plan, text);
    finish(
        input,
        project_dir,
        response,
        "stdout",
        result,
        (Lookup::Routed, plan.searches.len() as u64),
    )
}

/// The output text of a `tool_response`: the string itself, or its `key`.
fn response_text<'a>(response: &'a Value, key: &str) -> Option<&'a str> {
    match response {
        Value::String(s) => Some(s.as_str()),
        other => other.get(key).and_then(Value::as_str),
    }
}

/// Counts the lookup, `n` segments with one outcome, and builds the hook JSON.
fn finish(
    input: &Value,
    project_dir: &Path,
    response: &Value,
    key: &str,
    result: Result<String, Pass>,
    (routed, n): (Lookup, u64),
) -> Option<String> {
    match result {
        Ok(new_text) => {
            hook::record_lookup(project_dir, input, routed, n);
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
            hook::record_lookup(project_dir, input, Lookup::Passed(pass.as_str()), n);
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

/// One stdout line: the raw bytes, and the row when the line parses.
struct Piece<'a> {
    /// The line with its newline, if it has one.
    raw: &'a str,
    hit: Option<Hit<'a>>,
}

/// Parses one line without its newline. A `line:text` row has no path: it
/// parses only when `single` names the one file of the search.
fn parse_row<'a>(row: &'a str, single: Option<&'a str>) -> Option<Hit<'a>> {
    let (head, rest) = row.split_once(':')?;
    let (path, line, text) = if head.bytes().all(|b| b.is_ascii_digit()) {
        if head.is_empty() {
            return None;
        }
        (single?, head, rest)
    } else {
        let (line, text) = rest.split_once(':')?;
        (head, line, text)
    };
    Some(Hit {
        path,
        line: line.parse().ok().filter(|n| *n > 0)?,
        text,
    })
}

/// True if the line is a `line:text` row with no path.
fn bare_row(raw: &str) -> bool {
    parse_row(raw.strip_suffix('\n').unwrap_or(raw), Some("")).is_some_and(|h| h.path.is_empty())
}

/// Splits `out` into lines, each with its parsed row if it has one.
fn parse_pieces<'a>(out: &'a str, single: Option<&'a str>) -> Vec<Piece<'a>> {
    out.split_inclusive('\n')
        .map(|raw| Piece {
            raw,
            hit: parse_row(raw.strip_suffix('\n').unwrap_or(raw), single),
        })
        .collect()
}

/// Parses every line of `out`. Returns `None` if one line does not parse.
fn parse_rows<'a>(out: &'a str, single: Option<&'a str>) -> Option<Vec<Hit<'a>>> {
    parse_pieces(out, single)
        .into_iter()
        .map(|p| p.hit)
        .collect()
}

/// True if every row of the one-file search holds text of its file line.
/// This stops a pipe stage that numbers the rows again from inventing lines.
fn rows_match_file(hits: &[&Hit], arg: &str, file: &Path) -> bool {
    let Ok(data) = std::fs::read(file) else {
        return false;
    };
    if data.len() > MAX_VERIFY_BYTES {
        return false;
    }
    let text = String::from_utf8_lossy(&data);
    let lines: Vec<&str> = text.split('\n').collect();
    hits.iter().filter(|h| h.path == arg).all(|h| {
        lines
            .get(h.line as usize - 1)
            .is_some_and(|l| l.contains(h.text))
    })
}

/// True if the file is a document, not code.
fn non_code(path: &str) -> bool {
    let ext = Path::new(path).extension().and_then(|e| e.to_str());
    matches!(
        ext,
        Some("md" | "json" | "yaml" | "yml" | "lock" | "svg" | "txt")
    )
}

/// The project root and the directory the paths of the rows start from. A
/// leading `cd <dir>` moves the second. When the absolute `dir` has its own
/// index, that dir is the root.
fn locate(input: &Value, project_dir: &Path, cd: &Cd) -> (PathBuf, PathBuf) {
    let base = input
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
        .map_or_else(|| project_dir.to_path_buf(), PathBuf::from);
    let Cd::Dir(dir) = cd else {
        return (project_dir.to_path_buf(), base);
    };
    let dir = Path::new(dir);
    // An absolute `dir` replaces the base.
    let full = base.join(dir);
    let context = hook::resolve_context_dir(&full);
    let own = dir.is_absolute()
        && context != hook::resolve_context_dir(project_dir)
        && hook::wiring_path(&context).is_file();
    (
        if own {
            full.clone()
        } else {
            project_dir.to_path_buf()
        },
        full,
    )
}

/// Routes the stdout of a Bash call, or gives the reason to pass.
fn route_bash(input: &Value, project_dir: &Path, plan: &Plan, out: &str) -> Result<String, Pass> {
    let started = Instant::now();
    if plan.has_unsafe_flag() {
        return Err(Pass::Unsafe);
    }
    // `line:text` rows of one file never pass the 90% rule: no path to drop.
    let recursive = plan.searches.iter().any(|s| s.recursive);
    if let Some(arg) = plan.single_arg().filter(|_| !recursive) {
        let bare =
            |hits: &[Hit]| !hits.is_empty() && hits.iter().all(|h| std::ptr::eq(h.path, arg));
        if parse_rows(out, Some(arg)).is_some_and(|hits| bare(&hits)) {
            return Err(Pass::NoGain);
        }
    }
    let (root, cwd) = locate(input, project_dir, &plan.cd);
    let wiring = hook::wiring_path(&hook::resolve_context_dir(&root));
    if !wiring.is_file() {
        return Err(Pass::NoIndex);
    }
    let single = plan.single_arg().filter(|arg| cwd.join(arg).is_file());
    let exists = |path: &str| cwd.join(path).exists();
    if let Some(pass) = shape_reason(plan, out, single.is_some(), &exists) {
        return Err(pass);
    }
    if plan.searches.iter().any(|s| s.blocked) || out.contains("\u{1b}[") {
        return Err(Pass::Regex);
    }
    if out.is_empty() {
        return Err(Pass::NoGain);
    }
    let pieces = parse_pieces(out, single);
    let hits: Vec<&Hit> = pieces.iter().filter_map(|p| p.hit.as_ref()).collect();
    if hits.is_empty() {
        return Err(Pass::Regex);
    }
    if hits.iter().all(|h| non_code(h.path)) {
        return Err(Pass::NonCode);
    }
    if let Some(arg) = single {
        if !rows_match_file(&hits, arg, &cwd.join(arg)) {
            return Err(Pass::Regex);
        }
    }
    render(&root, &cwd, pieces, out, started, Mode::Bash)
}

/// Builds the routed text of a Grep tool result, or the reason to pass.
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
    let pieces = parse_pieces(out, None);
    if pieces.is_empty() || pieces.iter().any(|p| p.hit.is_none()) {
        return Err(Pass::Regex);
    }
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
            .is_some_and(|n| n != pieces.len() as u64);
        if counted
            || response.get("appliedLimit").is_some()
            || response.get("appliedOffset").is_some()
        {
            return Err(Pass::Regex);
        }
    }
    let all_docs = pieces
        .iter()
        .all(|p| p.hit.as_ref().is_some_and(|h| non_code(h.path)));
    if all_docs {
        return Err(Pass::NonCode);
    }
    let cwd = input
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
        .map_or_else(|| project_dir.to_path_buf(), PathBuf::from);
    render(project_dir, &cwd, pieces, out, started, Mode::Grep)
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

/// Which route calls `render`.
#[derive(Clone, Copy)]
enum Mode {
    /// The Grep tool: a row that has no current file passes the output.
    Grep,
    /// The Bash route.
    Bash,
}

/// Builds the routed text from the parsed lines, or the reason to pass.
/// `project_dir` owns the index. `cwd` is where the printed paths start.
/// A row whose file is outside the project, stale or not indexed becomes a
/// verbatim line. In `Mode::Grep`, such a row passes the whole output.
/// Every other line stays in place. Each run of rows becomes one block.
fn render(
    project_dir: &Path,
    cwd: &Path,
    mut pieces: Vec<Piece>,
    out: &str,
    started: Instant,
    mode: Mode,
) -> Result<String, Pass> {
    let strict = matches!(mode, Mode::Grep);
    let context_dir = hook::resolve_context_dir(project_dir);
    let root = std::fs::canonicalize(project_dir).map_err(|_| Pass::NotIndexedPath)?;
    let wiring_time = std::fs::metadata(hook::wiring_path(&context_dir))
        .and_then(|m| m.modified())
        .map_err(|_| Pass::NoIndex)?;

    // Map each printed path to its index path, or to the reason it has none.
    let mut rel_of: HashMap<&str, Result<String, Pass>> = HashMap::new();
    let mut first_miss: Option<Pass> = None;
    for hit in pieces.iter().filter_map(|p| p.hit.as_ref()) {
        if rel_of.contains_key(hit.path) {
            continue;
        }
        let rel = index_path(hit.path, cwd, &root).ok_or(Pass::NotIndexedPath);
        let rel = rel.and_then(|rel| {
            let fresh = std::fs::metadata(root.join(&rel))
                .and_then(|m| m.modified())
                .is_ok_and(|t| t <= wiring_time);
            if fresh {
                Ok(rel)
            } else {
                Err(Pass::Stale)
            }
        });
        if let Err(pass) = &rel {
            if strict {
                return Err(*pass);
            }
            first_miss.get_or_insert(*pass);
        }
        rel_of.insert(hit.path, rel);
    }
    // No row has a current file: the call passes before the wiring read.
    if rel_of.values().all(Result::is_err) {
        return Err(first_miss.unwrap_or(Pass::Regex));
    }
    // The wiring read and the freshness check cost the most, so they run last.
    let wanted: HashSet<&str> = rel_of
        .values()
        .filter_map(|r| r.as_ref().ok())
        .map(String::as_str)
        .collect();
    let (indexed, symbols) = match hook::open_lookup(&context_dir) {
        Some(lookup) => symbols_from_lookup(lookup, &wanted),
        None => {
            let graph = hook::read_wiring(&context_dir).ok_or(Pass::NoIndex)?;
            symbols_from_graph(&graph, &wanted)
        }
    };
    if hook::index_freshness(project_dir, &context_dir).is_some_and(|f| f.missing > 0) {
        return Err(Pass::Stale);
    }
    // Demote each row that has no indexed file. The first reason stands in
    // for the call if no row is left.
    for piece in &mut pieces {
        let Some(hit) = &piece.hit else { continue };
        let miss = match rel_of.get(hit.path) {
            Some(Ok(rel)) if indexed.contains(rel.as_str()) => None,
            Some(Err(pass)) => Some(*pass),
            _ => Some(Pass::NotIndexedPath),
        };
        if let Some(pass) = miss {
            if strict {
                return Err(pass);
            }
            first_miss.get_or_insert(pass);
            piece.hit = None;
        }
    }
    if pieces.iter().all(|p| p.hit.is_none()) {
        return Err(first_miss.unwrap_or(Pass::Regex));
    }

    // Keep the line order. Consecutive rows of one file share one header.
    // Every other line stays in place, byte for byte.
    let mut text = String::from("[sieve] routed:replaced\n");
    let mut printed: Vec<(&str, u32)> = Vec::with_capacity(pieces.len());
    let mut verbatim: Vec<&str> = Vec::new();
    let mut header: Option<&str> = None;
    for piece in &pieces {
        let Some(hit) = &piece.hit else {
            text.push_str(piece.raw);
            verbatim.push(piece.raw);
            header = None;
            continue;
        };
        if header != Some(hit.path) {
            text.push_str(hit.path);
            text.push('\n');
            header = Some(hit.path);
        }
        printed.push((hit.path, hit.line));
        let syms = match rel_of.get(hit.path) {
            Some(Ok(rel)) => symbols.get(rel.as_str()).map(Vec::as_slice),
            _ => None,
        };
        match syms.and_then(|s| enclosing(s, hit.line)) {
            Some(node) => {
                let name = node
                    .id
                    .split_once('#')
                    .map_or(node.name.as_str(), |(_, n)| n);
                let kind = match node.kind {
                    NodeKind::Function => "fn",
                    _ => "sym",
                };
                text.push_str(&format!("  L{} in {kind} {name}: {}\n", hit.line, hit.text));
            }
            None => text.push_str(&format!("  L{}: {}\n", hit.line, hit.text)),
        }
    }
    // The printed `(path, line)` sequence must equal the parsed sequence,
    // and the other lines must equal the original other lines.
    let rows = pieces.iter().filter_map(|p| p.hit.as_ref());
    let kept = pieces.iter().filter(|p| p.hit.is_none()).map(|p| p.raw);
    let same_rows = printed.iter().copied().eq(rows.map(|h| (h.path, h.line)));
    if !same_rows || !verbatim.iter().copied().eq(kept) {
        return Err(Pass::Regex);
    }
    // The last row has no newline in the original: drop the one we added.
    if !out.ends_with('\n') && pieces.last().is_some_and(|p| p.hit.is_some()) {
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
fn enclosing(symbols: &[Sym], line: u32) -> Option<&Sym> {
    let mut best: Option<&Sym> = None;
    for sym in symbols {
        if sym.start <= line && line <= sym.end && best.is_none_or(|b| sym.start >= b.start) {
            best = Some(sym);
        }
    }
    best
}

/// A symbol the router can name: the fields of a node it prints, and its
/// line range.
struct Sym {
    id: String,
    name: String,
    kind: NodeKind,
    start: u32,
    end: u32,
}

/// The indexed paths and the symbols of each wanted path.
type PathSymbols = (HashSet<String>, HashMap<String, Vec<Sym>>);

/// Reads the wanted paths from the full graph.
fn symbols_from_graph(graph: &sieve_core::wiring::Graph, wanted: &HashSet<&str>) -> PathSymbols {
    let mut indexed = HashSet::new();
    let mut symbols: HashMap<String, Vec<Sym>> = HashMap::new();
    for node in &graph.nodes {
        if !wanted.contains(node.path.as_str()) {
            continue;
        }
        if node.kind == NodeKind::File {
            indexed.insert(node.path.clone());
        } else if let Some((start, end)) = parse_span(&node.span) {
            symbols.entry(node.path.clone()).or_default().push(Sym {
                id: node.id.clone(),
                name: node.name.clone(),
                kind: node.kind,
                start,
                end,
            });
        }
    }
    (indexed, symbols)
}

/// Reads the wanted paths from the lookup, one record each. A path with no
/// record is not indexed.
fn symbols_from_lookup(
    mut lookup: sieve_core::lookup::Lookup,
    wanted: &HashSet<&str>,
) -> PathSymbols {
    let mut indexed = HashSet::new();
    let mut symbols: HashMap<String, Vec<Sym>> = HashMap::new();
    for path in wanted {
        let Some(record) = lookup.record(path) else {
            continue;
        };
        for node in record.nodes {
            if node.kind == NodeKind::File {
                indexed.insert((*path).to_string());
            } else if let Some((start, end)) = parse_span(&node.span) {
                symbols.entry((*path).to_string()).or_default().push(Sym {
                    id: node.id,
                    name: node.name,
                    kind: node.kind,
                    start,
                    end,
                });
            }
        }
    }
    (indexed, symbols)
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
        let hits = parse_rows(out, None).expect("parse");
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
                    // A README row is outside the index: its line stays as it was.
                    let mut want = original_pairs(&out);
                    want.retain(|(path, _)| !path.ends_with("README.md"));
                    assert_eq!(routed_pairs(&text), want, "{pattern}");
                    for line in out.lines().filter(|l| l.contains("README.md")) {
                        assert!(text.lines().any(|t| t == line), "{pattern}: {line}");
                    }
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
        let mixed = post_input(&dir.0, "rg -n total", &out, None);
        let json = post_search(&mixed, &dir.0).expect("the rows route");
        assert!(new_stdout(&json).ends_with("Binary file crates/engine/src/x.bin matches\n"));
        // Only lines that are not rows: the original stays.
        let bad = post_input(&dir.0, "rg -n total", "Binary file x.bin matches\n", None);
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
        hook::record_lookup(&dir.0, &pick, Lookup::Picked, 1);
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

    const G0: &str = "crates/engine/src/handlers/group_00_handlers.rs";
    const G1: &str = "crates/engine/src/handlers/group_01_handlers.rs";
    /// The lines with `total`: two lines in each of the first four functions.
    const TOTAL: [u32; 8] = [4, 5, 11, 12, 18, 19, 25, 26];

    fn file_line(root: &Path, file: &str, n: u32) -> String {
        let text = std::fs::read_to_string(root.join(file)).expect("read");
        text.lines().nth(n as usize - 1).expect("line").to_string()
    }

    /// `path:line:text` rows of real lines. `shown` is the path as printed.
    fn path_rows(root: &Path, file: &str, shown: &str, lines: &[u32]) -> String {
        lines.iter().fold(String::new(), |mut out, n| {
            out.push_str(&format!("{shown}:{n}:{}\n", file_line(root, file, *n)));
            out
        })
    }

    /// `line:text` rows, as `grep -n` prints for one file.
    fn bare_rows(root: &Path, file: &str, lines: &[u32]) -> String {
        lines.iter().fold(String::new(), |mut out, n| {
            out.push_str(&format!("{n}:{}\n", file_line(root, file, *n)));
            out
        })
    }

    fn routed_seq(text: &str) -> Vec<(String, u32)> {
        let mut seq = Vec::new();
        let mut path = String::new();
        for row in text.lines().skip(1) {
            match row.strip_prefix("  L") {
                Some(rest) => {
                    let n: String = rest.chars().take_while(char::is_ascii_digit).collect();
                    seq.push((path.clone(), n.parse().expect("line")));
                }
                None => path = row.to_string(),
            }
        }
        seq
    }

    /// Runs one Bash call under its own session id.
    fn run_bash(
        dir: &Scratch,
        session: &str,
        command: &str,
        stdout: &str,
    ) -> (Option<String>, hook::SessionState) {
        let mut input = post_input(&dir.0, command, stdout, None);
        input["session_id"] = Value::from(session);
        let routed = post_search(&input, &dir.0).map(|json| new_stdout(&json));
        (routed, hook::read_session(&dir.0, session))
    }

    /// What one Bash call must record.
    enum Want {
        /// Routed, with this segment count.
        Routed(u64),
        /// Passed, with this reason and segment count.
        Passed(&'static str, u64),
        /// No record at all.
        Unrecorded,
    }

    fn check(dir: &Scratch, session: &str, command: &str, stdout: &str, want: Want) -> String {
        let (routed, s) = run_bash(dir, session, command, stdout);
        match want {
            Want::Routed(n) => {
                assert!(routed.is_some(), "{command}: {:?}", s.lookup_pass_reasons);
                assert_eq!((s.lookups_routed, s.lookups_passed), (n, 0), "{command}");
            }
            Want::Passed(reason, n) => {
                assert!(routed.is_none(), "{command}");
                assert_eq!((s.lookups_routed, s.lookups_passed), (0, n), "{command}");
                assert_eq!(s.lookup_pass_reasons.get(reason), Some(&n), "{command}");
            }
            Want::Unrecorded => {
                assert!(routed.is_none(), "{command}");
                assert_eq!((s.lookups_routed, s.lookups_passed), (0, 0), "{command}");
            }
        }
        routed.unwrap_or_default()
    }

    const GOLDEN_TWO_FILES: &str = "\
[sieve] routed:replaced
crates/engine/src/handlers/group_00_handlers.rs
  L4 in fn handle_0_0:     let total = input + 0;
  L5 in fn handle_0_0:     let total = total * 0;
  L11 in fn handle_0_1:     let total = input + 1;
  L12 in fn handle_0_1:     let total = total * 0;
  L18 in fn handle_0_2:     let total = input + 2;
  L19 in fn handle_0_2:     let total = total * 0;
crates/engine/src/handlers/group_01_handlers.rs
  L4 in fn handle_1_0:     let total = input + 0;
  L5 in fn handle_1_0:     let total = total * 1;
  L11 in fn handle_1_1:     let total = input + 1;
  L12 in fn handle_1_1:     let total = total * 1;
  L18 in fn handle_1_2:     let total = input + 2;
  L19 in fn handle_1_2:     let total = total * 1;
";

    #[test]
    fn test_route_bash_shapes() {
        let dir = fixture("shapes");
        let root = dir.0.as_path();
        let rows = |file: &str, shown: &str, lines: &[u32]| path_rows(root, file, shown, lines);
        let two = [rows(G0, G0, &TOTAL), rows(G1, G1, &TOTAL)].concat();
        let six = [4, 5, 11, 12, 18, 19];
        let mut n = 0;
        let mut run = |command: &str, stdout: &str, want: Want| {
            n += 1;
            check(&dir, &format!("case{n}"), command, stdout, want)
        };
        // A one-file `line:text` row parses, but the original has no path, so
        // the routed text is never at most 90% of it.
        let bare = bare_rows(root, G0, &TOTAL);
        run(
            &format!("grep -n total {G0}"),
            &bare,
            Want::Passed("no-gain", 1),
        );
        run(
            &format!("grep -n total < {G0}"),
            &bare,
            Want::Passed("no-gain", 1),
        );
        // The check reads no file: a path that does not exist still gives no-gain.
        run(
            "grep -n total no/such.rs",
            "4:let total\n",
            Want::Passed("no-gain", 1),
        );
        // Routed: one search, with and without a path, in the order printed.
        run("grep -rn total crates", &two, Want::Routed(1));
        run("rg -n total", &rows(G0, G0, &TOTAL), Want::Routed(1));
        let under_rows = [
            rows(G0, "src/handlers/group_00_handlers.rs", &TOTAL),
            rows(G1, "src/handlers/group_01_handlers.rs", &TOTAL),
        ]
        .concat();
        run(
            "cd crates/engine && grep -rn \"total\" src",
            &under_rows,
            Want::Routed(1),
        );
        let abs = format!("cd {} && grep -rn total crates", root.display());
        run(&abs, &two, Want::Routed(1));
        run("git grep -n total", &two, Want::Routed(1));
        let only: String = [G0, G1]
            .iter()
            .flat_map(|f| TOTAL.iter().map(move |n| format!("{f}:{n}:total\n")))
            .collect();
        run("grep -rno total crates", &only, Want::Routed(1));
        // Two segments route when every row carries a path.
        let both = [
            rows(G0, "engine/src/handlers/group_00_handlers.rs", &TOTAL),
            rows(G1, "engine/src/handlers/group_01_handlers.rs", &TOTAL),
        ]
        .concat();
        run(
            "cd crates && grep -rn a engine && grep -rn b engine",
            &both,
            Want::Routed(2),
        );
        let chained = [rows(G0, G0, &six), rows(G1, G1, &six)].concat();
        let text = run(
            "grep -rn total crates/engine/src/handlers/group_00_handlers.rs; grep -rn total \
             crates/engine/src/handlers/group_01_handlers.rs",
            &chained,
            Want::Routed(2),
        );
        assert_eq!(text, GOLDEN_TWO_FILES);
        // A `line:text` row in a call with two searches passes.
        let split = [bare_rows(root, G0, &[4, 5]), bare_rows(root, G1, &[4, 5])].concat();
        let two_greps = format!("grep -n a {G0}; grep -n b {G1}");
        run(&two_greps, &split, Want::Passed("compound", 2));
        // The second file starts above the last hit of the first file.
        let head = [
            bare_rows(root, G0, &six[..5]),
            bare_rows(root, G1, &six[..5]),
        ]
        .concat();
        let piped = format!("grep -n total {G0} | head -5; grep -n total {G1} | head -5");
        run(&piped, &head, Want::Passed("compound", 2));
        // The pipes that keep the row shape route; the others do not.
        let twelve = [rows(G0, G0, &TOTAL), rows(G1, G1, &[4, 5, 11, 12])].concat();
        run("grep -rn total crates | head -12", &twelve, Want::Routed(1));
        run("grep -rn total crates | tail -12", &twelve, Want::Routed(1));
        run(
            "grep -rn total crates | grep -v TODO",
            &two,
            Want::Routed(1),
        );
        let cut: String = two.lines().map(|l| format!("{}\n", &l[..60])).collect();
        run("grep -rn total crates | cut -c1-60", &cut, Want::Routed(1));
        let sorted = [rows(G1, G1, &TOTAL), rows(G0, G0, &TOTAL)].concat();
        run("grep -rn total crates | sort", &sorted, Want::Routed(1));
        let one = bare_rows(root, G0, &[4]);
        run(
            &format!("grep -n total {G0} | head -1"),
            &one,
            Want::Passed("no-gain", 1),
        );
        run(
            "grep -rn total crates | wc -l",
            "      16\n",
            Want::Passed("pipe", 1),
        );
        let names = format!("{G0}\n{G1}\n");
        let cut_sort = "grep -rn total crates | cut -d: -f1 | sort -u";
        run(cut_sort, &names, Want::Passed("pipe", 1));
        let shifted: String = two.lines().map(|l| format!("> {l}\n")).collect();
        let sed = "grep -rn total crates | sed 's/^/> /'";
        run(sed, &shifted, Want::Passed("pipe", 1));
        let counted: String = two.lines().map(|l| format!("      2 {l}\n")).collect();
        run(
            "grep -rn total crates | uniq -c",
            &counted,
            Want::Passed("pipe", 1),
        );
        // Flags and output that the hook does not route.
        run("grep -rnl total crates", &names, Want::Passed("regex", 1));
        let counts = format!("{G0}:8\n{G1}:8\n");
        run("grep -rnc total crates", &counts, Want::Passed("regex", 1));
        let plain = format!("{G0}:    let total = input + 0;\n");
        run("grep -r total crates", &plain, Want::Passed("no-n", 1));
        let context = format!("{G0}:4:a\n{G0}-5-b\n--\n{G1}:4:a\n");
        run(
            "grep -rn -A2 total crates",
            &context,
            Want::Passed("regex", 1),
        );
        let ansi = format!("\u{1b}[35m{G0}\u{1b}[0m:4:x\n");
        run(
            "rg -n --color=always total",
            &ansi,
            Want::Passed("regex", 1),
        );
        // A line that is no row stays in place; the rows around it route.
        let text = run(
            "grep -rn total crates",
            &(two.clone() + "Binary file x.bin matches\n"),
            Want::Routed(1),
        );
        assert!(text.ends_with(
            "  L26 in fn handle_1_3:     let total = total * 1;\nBinary file x.bin matches\n"
        ));
        run(
            "grep -rn total crates",
            "Binary file x.bin matches\n",
            Want::Passed("regex", 1),
        );
        run(
            "grep -rn total crates",
            &bare_rows(root, G0, &TOTAL),
            Want::Passed("regex", 1),
        );
        run("grep -rn nomatch crates", "", Want::Passed("no-gain", 1));
        // Documents stay out of the rate; a code file outside the index stays in.
        let readme = "./README.md:1:total handlers TODO\n";
        run("grep -rn total .", readme, Want::Passed("non-code", 1));
        let mixed = format!("{readme}{}", rows(G0, &format!("./{G0}"), &TOTAL));
        let text = run("grep -rn total .", &mixed, Want::Routed(1));
        let want = format!("[sieve] routed:replaced\n{readme}./{G0}\n  L4 in fn handle_0_0:");
        assert!(text.starts_with(&want), "{text}");
        // Slice 3b: other lines stay in place; the rows around them route.
        let block = GOLDEN_TWO_FILES.strip_prefix("[sieve] routed:replaced\n");
        let block = block.expect("golden header");
        let sed_lines: String = (1..=5).map(|n| file_line(root, G0, n) + "\n").collect();
        let two_files = format!("grep -n total {G0} {G1}");
        let command = format!("sed -n 1,5p {G0}; {two_files}");
        let text = run(&command, &(sed_lines.clone() + &chained), Want::Routed(1));
        assert_eq!(text, format!("[sieve] routed:replaced\n{sed_lines}{block}"));
        let ls = "a.rs\nb.rs\n";
        let command = format!("{two_files}; ls");
        let text = run(&command, &(chained.clone() + ls), Want::Routed(1));
        assert_eq!(text, format!("{GOLDEN_TWO_FILES}{ls}"));
        let head: String = sed_lines
            .lines()
            .take(3)
            .map(|l| format!("{l}\n"))
            .collect();
        let dotted = [
            rows(G0, &format!("./{G0}"), &six),
            rows(G1, &format!("./{G1}"), &six),
        ]
        .concat();
        let text = run(
            "cat f0 | head -3; grep -rn total .",
            &(head.clone() + &dotted),
            Want::Routed(1),
        );
        assert!(text.starts_with(&format!("[sieve] routed:replaced\n{head}./{G0}\n")));
        // A row-like sed line routes only when its file resolves.
        let fake = "src/x.ts:12:text\n";
        let command = format!("sed -n 12p src/x.ts; {two_files}");
        let text = run(&command, &(fake.to_string() + &chained), Want::Routed(1));
        assert_eq!(text, format!("[sieve] routed:replaced\n{fake}{block}"));
        let real = rows(G0, G0, &[4]);
        let text = run(&command, &(real + &chained), Want::Routed(1));
        assert_eq!(text.matches("L4 in fn handle_0_0").count(), 2, "{text}");
        // E1: a bare row in a call with two searches is `compound`.
        let command = format!("ls; grep -n a {G0}; grep -n b {G1}");
        run(&command, &split, Want::Passed("compound", 2));
        // A mixed output that does not shrink to 90% passes.
        let one_row = sed_lines + &rows(G0, G0, &[4]);
        let command = format!("sed -n 1,5p {G0}; {two_files}");
        run(&command, &one_row, Want::Passed("no-gain", 1));
        // The call shape decides.
        run(
            "cd crates && cd engine && grep -rn total src",
            &under_rows,
            Want::Passed("compound", 1),
        );
        run(
            "grep -rn \"$(cat pats)\" crates",
            &two,
            Want::Passed("unsafe", 1),
        );
        run(
            "grep -rn total crates -f pats",
            &two,
            Want::Passed("unsafe", 1),
        );
        run(
            "npm test 2>&1 | grep -E \"pass|fail\"",
            "pass 3\n",
            Want::Unrecorded,
        );
        run("sieve ask total | grep -v x", "a\n", Want::Unrecorded);
    }

    #[test]
    fn test_route_segments_count_every_search() {
        let count = |command: &str| match analyze(command) {
            Analysis::NoSearch => 0,
            Analysis::Unsafe => 1,
            Analysis::Plan(plan) => plan.searches.len(),
        };
        assert_eq!(count("grep -n \"a\" f; grep -n \"b\" g"), 2);
        assert_eq!(
            count("cd crates && grep -rn a engine && grep -rn b engine"),
            2
        );
        assert_eq!(count("grep -rn total crates | grep -v TODO"), 1);
        assert_eq!(count("npm test 2>&1 | grep -E \"pass|fail\""), 0);
        assert_eq!(count("A=1 time sudo env B=2 rg -n x && git grep -n y"), 2);
        assert_eq!(count("sed -n 1,5p f | head; grep -n x f; echo ---"), 1);
        assert_eq!(count("sieve grep x | head"), 0);
        // One record for a call the hook cannot read, and for a fan-out.
        assert_eq!(count("for f in a b; do grep -n x $f; done"), 1);
        assert_eq!(count("grep -n x f | xargs grep -n y"), 1);
        assert_eq!(count("find . -name x | xargs grep -n y"), 1);
        assert_eq!(count("echo $(date); echo hi"), 0);
    }

    #[test]
    fn test_route_cd_prefix_resolves_paths() {
        let dir = fixture("cd-prefix");
        let root = dir.0.as_path();
        let rows = [
            path_rows(root, G0, "src/handlers/group_00_handlers.rs", &TOTAL),
            path_rows(root, G1, "src/handlers/group_01_handlers.rs", &TOTAL),
        ]
        .concat();
        let go = |n: u32, command: &str, stdout: &str, want: Want| {
            check(&dir, &format!("cd{n}"), command, stdout, want)
        };
        go(
            1,
            "cd crates/engine && grep -rn total src",
            &rows,
            Want::Routed(1),
        );
        let abs = format!("cd {} && grep -rn total crates", root.display());
        let all = path_rows(root, G0, G0, &TOTAL);
        go(2, &abs, &all, Want::Routed(1));
        go(
            3,
            "cd crates && cd engine && grep -rn total src",
            &rows,
            Want::Passed("compound", 1),
        );
        go(
            4,
            "grep -rn total src; cd crates",
            &rows,
            Want::Passed("compound", 1),
        );
        for (i, bad) in ["cd ~/x", "cd $HOME", "cd -", "cd"].iter().enumerate() {
            let command = format!("{bad} && grep -rn total src");
            go(10 + i as u32, &command, &rows, Want::Passed("compound", 1));
        }
        // A `cd` out of the project passes with `not-indexed-path`.
        let out = Scratch::new("cd-outside");
        std::fs::create_dir_all(out.0.join("src")).expect("mkdir");
        std::fs::write(out.0.join("src/a.rs"), "total x\n").expect("write");
        let command = format!("cd {} && grep -rn total src", out.0.display());
        go(
            6,
            &command,
            "src/a.rs:1:total x\n",
            Want::Passed("not-indexed-path", 1),
        );
        // An absolute dir with its own index routes against that index.
        let own = fixture("cd-own");
        let command = format!("cd {} && grep -rn total crates", own.0.display());
        let own_rows = path_rows(&own.0, G0, G0, &TOTAL);
        go(7, &command, &own_rows, Want::Routed(1));
    }

    #[test]
    fn test_route_head_tail_keeps_seen_lines() {
        let dir = fixture("head-tail");
        let root = dir.0.as_path();
        let six = path_rows(root, G0, G0, &[4, 5, 11, 12, 18, 19]);
        let text = check(
            &dir,
            "h1",
            "grep -rn total crates | head -6",
            &six,
            Want::Routed(1),
        );
        assert_eq!(routed_seq(&text), original_seq(&six));
        assert_eq!(routed_seq(&text).len(), 6);
        // The agent sees the last rows of the second file only.
        let tail = path_rows(root, G1, G1, &[11, 12, 18, 19, 25, 26]);
        let text = check(
            &dir,
            "t1",
            "grep -rn total crates | tail -6",
            &tail,
            Want::Routed(1),
        );
        assert_eq!(routed_seq(&text), original_seq(&tail));
        // Two rows are too small for a gain: the original stays.
        let two = path_rows(root, G0, G0, &[4, 5]);
        check(
            &dir,
            "h2",
            "grep -rn total crates | head -2",
            &two,
            Want::Passed("no-gain", 1),
        );
    }

    fn original_seq(out: &str) -> Vec<(String, u32)> {
        let hits = parse_rows(out, None).expect("parse");
        hits.iter().map(|h| (h.path.to_string(), h.line)).collect()
    }

    #[test]
    fn test_route_mixed_output_passes() {
        let dir = fixture("mixed");
        let root = dir.0.as_path();
        let two = [
            path_rows(root, G0, G0, &TOTAL),
            path_rows(root, G1, G1, &TOTAL),
        ]
        .concat();
        let binary = two.clone() + "Binary file crates/engine/src/x.bin matches\n";
        check(
            &dir,
            "m1",
            "grep -rn total crates",
            &binary,
            Want::Routed(1),
        );
        let sed = (1..=5)
            .map(|n| file_line(root, G0, n) + "\n")
            .collect::<String>();
        let out = sed + &bare_rows(root, G0, &TOTAL);
        let command = format!("sed -n 1,5p {G0}; grep -n total {G0}");
        check(&dir, "m2", &command, &out, Want::Passed("regex", 1));
        // A pipe stage that numbers the rows again must not invent lines.
        let renumbered: String = bare_rows(root, G0, &TOTAL)
            .lines()
            .enumerate()
            .map(|(i, l)| format!("{}:{l}\n", i + 1))
            .collect();
        let command = format!("grep -n total {G0} | grep -n x");
        check(
            &dir,
            "m3",
            &command,
            &renumbered,
            Want::Passed("no-gain", 1),
        );
    }

    #[test]
    fn test_route_mixed_output_keeps_every_byte() {
        let dir = fixture("every-byte");
        let root = dir.0.as_path();
        let rows: Vec<String> = [(G0, &TOTAL), (G1, &TOTAL)]
            .iter()
            .flat_map(|(f, lines)| {
                path_rows(root, f, f, *lines)
                    .lines()
                    .map(String::from)
                    .collect::<Vec<_>>()
            })
            .collect();
        let mut state = 7u64;
        let mut next = |bound: u64| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) % bound
        };
        for seed in 0..10 {
            // One or two breaks. Each break holds one to three other lines.
            let mut breaks: Vec<usize> = (0..=rows.len()).collect();
            let mut at = Vec::new();
            for _ in 0..=next(2) {
                at.push(breaks.remove(next(breaks.len() as u64) as usize));
            }
            let (mut input, mut others) = (String::new(), Vec::new());
            for (i, row) in rows
                .iter()
                .enumerate()
                .map(|(i, r)| (i, Some(r)))
                .chain([(rows.len(), None)])
            {
                if at.contains(&i) {
                    for k in 0..=next(3) {
                        let line = if next(2) == 0 {
                            format!("other-{seed}-{i}-{k}")
                        } else {
                            format!("src/x.ts:{}:other-{seed}-{k}", i + 1)
                        };
                        input.push_str(&line);
                        input.push('\n');
                        others.push(line);
                    }
                }
                if let Some(row) = row {
                    input.push_str(row);
                    input.push('\n');
                }
            }
            let name = format!("seed{seed}");
            let text = check(
                &dir,
                &name,
                "sed -n 1,5p f; grep -rn total crates",
                &input,
                Want::Routed(1),
            );
            // Every other line is in the output, in order.
            let mut want = others.iter().peekable();
            for line in text.lines() {
                if want.peek().is_some_and(|w| w.as_str() == line) {
                    want.next();
                }
            }
            assert_eq!(want.next(), None, "{seed}: {text}");
            // Every row text is in the output once per input row.
            for row in &rows {
                let body = row.splitn(3, ':').nth(2).expect("row text");
                let count = |t: &str| {
                    t.lines()
                        .filter(|l| {
                            l.starts_with(if t == input { "crates" } else { "  L" })
                                && l.ends_with(body)
                        })
                        .count()
                };
                assert_eq!(count(&text), count(&input), "{seed}: {body}");
            }
        }
    }

    #[test]
    fn test_route_mixed_output_edge_bytes() {
        let dir = fixture("edge-bytes");
        let root = dir.0.as_path();
        let cmd = "sed -n 1,5p f; grep -rn total crates";
        let rows = path_rows(root, G0, G0, &TOTAL);
        // (1) Every `\r` stays, in verbatim lines and in row text.
        let crlf = format!("plain\r\n{}", rows.replace('\n', "\r\n"));
        let text = check(&dir, "e1", cmd, &crlf, Want::Routed(1));
        assert_eq!(text.matches('\r').count(), crlf.matches('\r').count());
        assert!(text.contains("plain\r\n") && text.contains("input + 0;\r\n"));
        // (2) A `--` line splits two row runs into two blocks.
        let (a, b) = rows.split_at(rows.match_indices('\n').nth(3).map_or(0, |(i, _)| i + 1));
        let text = check(&dir, "e2", cmd, &format!("{a}--\n{b}"), Want::Routed(1));
        let parts: Vec<&str> = text.split("--\n").collect();
        assert_eq!(parts.len(), 2, "{text}");
        assert!(parts[0].contains("L12 in"), "{text}");
        assert!(parts[1].starts_with(G0), "{text}");
        assert_eq!(text.matches(&format!("{G0}\n")).count(), 2);
        // (3) A last line with no newline stays as it is.
        let text = check(&dir, "e3", cmd, &format!("{rows}tail"), Want::Routed(1));
        assert!(text.ends_with("\ntail"), "{text}");
    }

    #[test]
    fn test_route_rate_counts_segments() {
        let dir = fixture("rate");
        let input = post_input(&dir.0, "x", "", None);
        hook::record_lookup(&dir.0, &input, Lookup::Picked, 2);
        hook::record_lookup(&dir.0, &input, Lookup::Routed, 5);
        hook::record_lookup(&dir.0, &input, Lookup::Passed("no-n"), 2);
        hook::record_lookup(&dir.0, &input, Lookup::Passed(Pass::NonCode.as_str()), 3);
        let session = hook::read_session(&dir.0, "s1");
        let (rate, excluded) = session.lookup_rate().expect("rate");
        // (2 + 5) / (2 + 5 + 2) = 77.8%, with 3 documents left out.
        assert!((rate - 700.0 / 9.0).abs() < 1e-9, "{rate}");
        assert_eq!(excluded, 3);
        assert_eq!(hook::SessionState::default().lookup_rate(), None);
    }

    #[test]
    fn test_route_grep_tool_output_keeps_its_bytes() {
        let dir = fixture("grep-golden");
        let root = dir.0.as_path();
        let lines = [4, 5, 11, 12];
        let content = [
            path_rows(root, G0, G0, &lines),
            path_rows(root, G1, G1, &lines),
        ]
        .concat();
        let mut input = post_input(&dir.0, "x", "", None);
        input["tool_name"] = Value::from("Grep");
        input["tool_input"] = serde_json::json!({"output_mode": "content"});
        input["tool_response"] = serde_json::json!({
            "mode": "content", "numFiles": 2, "filenames": [],
            "content": content, "numLines": 8, "totalLines": 8,
        });
        let json = post_search(&input, &dir.0).expect("routed");
        let v: Value = serde_json::from_str(&json).expect("json");
        let text = v["hookSpecificOutput"]["updatedToolOutput"]["content"]
            .as_str()
            .expect("content");
        let want = "\
[sieve] routed:replaced
crates/engine/src/handlers/group_00_handlers.rs
  L4 in fn handle_0_0:     let total = input + 0;
  L5 in fn handle_0_0:     let total = total * 0;
  L11 in fn handle_0_1:     let total = input + 1;
  L12 in fn handle_0_1:     let total = total * 0;
crates/engine/src/handlers/group_01_handlers.rs
  L4 in fn handle_1_0:     let total = input + 0;
  L5 in fn handle_1_0:     let total = total * 1;
  L11 in fn handle_1_1:     let total = input + 1;
  L12 in fn handle_1_1:     let total = total * 1;
";
        assert_eq!(text, want);
    }

    #[test]
    fn test_route_cd_needs_and_or_semicolon() {
        let cd = |c: &str| match analyze(c) {
            Analysis::Plan(p) => p.cd,
            _ => Cd::None,
        };
        assert_eq!(cd("cd d && grep -n x f"), Cd::Dir("d".into()));
        assert_eq!(cd("cd d; grep -n x f"), Cd::Dir("d".into()));
        assert_eq!(cd("cd d & grep -n x f"), Cd::Compound);
        assert_eq!(cd("cd d || grep -n x f"), Cd::Compound);
    }

    #[test]
    fn test_route_sieve_call_with_unreadable_text_is_no_search() {
        for c in [
            "sieve grep \"$(x)\" | head",
            "cd r && sieve grep \"$(x)\" | head",
        ] {
            assert!(matches!(analyze(c), Analysis::NoSearch), "{c}");
        }
        assert!(matches!(
            analyze("grep \"$(x)\" f | head"),
            Analysis::Unsafe
        ));
    }

    #[test]
    fn test_route_lexer_real_commands() {
        // The path is read at run time, so a worktree build never bakes it.
        let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir");
        let path = Path::new(&manifest).join("tests/fixtures/route-real.json");
        let text = std::fs::read_to_string(path).expect("fixture");
        let entries: Vec<Value> = serde_json::from_str(&text).expect("json");
        assert!(entries.len() >= 40, "{}", entries.len());
        let mut seen = HashSet::new();
        for e in &entries {
            let command = e["command"].as_str().expect("command");
            let stdout = e["stdout"].as_str().expect("stdout");
            let (segments, reason) = match analyze(command) {
                Analysis::NoSearch => (0, "none"),
                Analysis::Unsafe => (1, "unsafe"),
                Analysis::Plan(plan) => {
                    let reason = if plan.has_unsafe_flag() {
                        "unsafe"
                    } else {
                        let single = plan.single_arg().is_some();
                        shape_reason(&plan, stdout, single, &|_| true)
                            .map_or("beyond", Pass::as_str)
                    };
                    (plan.searches.len() as u64, reason)
                }
            };
            let at = format!("{}:{}: {command}", e["file"], e["line"]);
            assert_eq!(segments, e["segments"].as_u64().expect("segments"), "{at}");
            assert_eq!(reason, e["reason"].as_str().expect("reason"), "{at}");
            seen.insert(reason);
        }
        for reason in ["unsafe", "compound", "pipe", "no-n", "beyond", "none"] {
            assert!(seen.contains(reason), "{reason}");
        }
    }

    #[test]
    fn test_s3_route_lookup_equals_full() {
        let dir = fixture("s3-route");
        let ctx = hook::resolve_context_dir(&dir.0);
        // The fixture writes the wiring alone. A real refresh writes both.
        let graph = hook::read_wiring(&ctx).expect("graph");
        sieve_parse::write_wiring_and_lookup(&graph, &ctx).expect("write graph");
        // A synthetic rg row at the start and the end line of each definition.
        let pad = "x".repeat(60);
        let mut out = String::new();
        for node in &graph.nodes {
            if node.kind == NodeKind::File {
                continue;
            }
            if let Some((start, end)) = parse_span(&node.span) {
                for line in [start, end] {
                    out.push_str(&format!("{}:{line}:{pad}\n", node.path));
                }
            }
        }
        out.push_str(&format!("README.md:1:{pad}\n"));
        let input = post_input(&dir.0, "rg -n x", &out, None);

        hook::s3_support::drop_lookup(&ctx);
        let full = post_search(&input, &dir.0).expect("the full path routes");
        sieve_parse::write_wiring_and_lookup(&graph, &ctx).expect("write graph");
        let with = post_search(&input, &dir.0).expect("the lookup path routes");
        assert_eq!(with, full);
        // With the wiring garbled, only the lookup can answer.
        hook::s3_support::garble_wiring(&ctx);
        assert_eq!(post_search(&input, &dir.0), Some(full));
    }

    #[test]
    fn test_s3_over_cap_with_lookup_routes() {
        let dir = fixture("s3-route-cap");
        let ctx = hook::resolve_context_dir(&dir.0);
        let graph = hook::read_wiring(&ctx).expect("graph");
        sieve_parse::write_wiring_and_lookup(&graph, &ctx).expect("write graph");
        let pad = "x".repeat(60);
        let path = "crates/engine/src/handlers/group_00_handlers.rs";
        let out: String = (3..9)
            .map(|line| format!("{path}:{line}:{pad}\n"))
            .collect();
        let input = post_input(&dir.0, "rg -n x", &out, None);
        hook::set_test_wiring_cap(Some(1));
        let with = post_search(&input, &dir.0);
        hook::s3_support::drop_lookup(&ctx);
        let without = post_search(&input, &dir.0);
        hook::set_test_wiring_cap(None);
        assert!(with.is_some(), "a valid lookup routes over the cap");
        assert!(without.is_none(), "no lookup passes over the cap");
    }
}
