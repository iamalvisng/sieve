//! Lookup routing.
//!
//! Grep: a `PostToolUse` hook on the Grep tool re-prints the
//! matches grouped by file and by enclosing symbol. The new text keeps
//! every `path:line` pair of the original, in the original order. If it
//! cannot, the hook keeps the original output and records a pass reason.
//!
//! Bash: the hook routes the stdout. The lexer in `lex.rs` splits the
//! command into segments. Each search segment counts as one lookup.
//!
//! The hook splits the stdout into runs of lines. A run of `path:line:text`
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
    /// The first word that is not a flag. `None` when `-e` or `--regexp`
    /// gives the pattern.
    pattern: Option<String>,
    /// The tool is `rg`, not `grep`.
    rg: bool,
    /// `-i` is present.
    ignore_case: bool,
    /// `-F` or `--fixed-strings` is present.
    fixed: bool,
    /// A flag outside the small set that `find` can answer.
    other_flag: bool,
    /// A flag outside the wider set that the deny rule accepts.
    deny_off: bool,
    /// `-w` or `--word-regexp` is present.
    word: bool,
    /// `-l` or `--files-with-matches` is present.
    files_only: bool,
    /// The values of `-e` and `--regexp`.
    e_patterns: Vec<String>,
    /// The values of the glob flags that keep files (`-g`, `--glob`, `--include`).
    globs: Vec<String>,
    /// The values of `-t` and `--type`.
    types: Vec<String>,
}

impl SearchArgs {
    /// Records one flag that takes a value, for the deny rule. A flag that
    /// the rule does not know turns the rule off for this search.
    fn note_valued(&mut self, name: &str, value: Option<String>) {
        let value = value.unwrap_or_default();
        match (name, self.rg) {
            ("regexp", _) => self.e_patterns.push(value),
            ("glob", true) | ("include", _) => self.globs.push(value),
            ("type", true) => self.types.push(value),
            // An exclude never adds a file to the search.
            ("exclude" | "exclude-dir", _) => {}
            _ => self.deny_off = true,
        }
    }
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
        rg,
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
            } else {
                args.pattern = Some(text.to_string());
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
            match name {
                "fixed-strings" => args.fixed = true,
                "line-number" | "no-heading" => {}
                _ => args.other_flag = true,
            }
            let mut value = long.split_once('=').map(|(_, v)| v.to_string());
            args.recursive |= name == "recursive";
            args.blocked |= BLOCKED_LONG.iter().any(|f| text.starts_with(f));
            args.unsafe_flag |= UNSAFE_LONG.contains(&name);
            have_pattern |= name == "regexp";
            if !has_value && VALUED_LONG.contains(&name) {
                value = words.next().map(|w| w.text.clone());
            }
            match name {
                "fixed-strings" | "line-number" | "no-heading" => {}
                "word-regexp" => args.word = true,
                "files-with-matches" => args.files_only = true,
                "recursive" if !rg => {}
                "regexp" | "glob" | "type" | "include" | "exclude" | "exclude-dir" => {
                    args.note_valued(name, value);
                }
                _ => args.deny_off = true,
            }
            continue;
        }
        let mut cluster = text[1..].chars();
        while let Some(ch) = cluster.next() {
            args.line_numbers |= ch == 'n';
            // In `rg`, `-r` replaces text. In `grep`, it means recursive.
            match ch {
                'i' => args.ignore_case = true,
                'F' => args.fixed = true,
                'n' | 'H' => {}
                'r' | 'R' if !rg => {}
                _ => args.other_flag = true,
            }
            match ch {
                'i' | 'F' | 'n' | 'H' | 'e' => {}
                'r' | 'R' if !rg => {}
                'w' => args.word = true,
                'l' => args.files_only = true,
                'g' | 't' if rg => {}
                _ => args.deny_off = true,
            }
            args.recursive |= matches!(ch, 'r' | 'R');
            args.blocked |= BLOCKED_SHORT.contains(ch);
            args.unsafe_flag |= matches!(ch, 'z' | 'f');
            if VALUED_SHORT.contains(ch) || (rg && VALUED_SHORT_RG.contains(ch)) {
                have_pattern |= ch == 'e';
                let rest = cluster.as_str();
                let value = if rest.is_empty() {
                    words.next().map(|w| w.text.clone())
                } else {
                    Some(rest.to_string())
                };
                match ch {
                    'e' => args.note_valued("regexp", value),
                    'g' if rg => args.note_valued("glob", value),
                    't' if rg => args.note_valued("type", value),
                    _ => {}
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

/// Characters that make a pattern or a path unsafe to repeat inside a
/// quoted `find` command. The hook passes such a search.
const UNQUOTABLE: [char; 12] = [
    '"', '\\', '$', '`', '\n', '\'', ' ', '*', '?', '[', '<', '>',
];

/// Characters with a regex meaning. `grep`, `rg` and `find` read them in
/// different dialects, so the hook denies a pattern with one only for `-F`.
const REGEX_META: [char; 9] = ['.', '+', '(', ')', '{', '}', '^', '|', ']'];

/// True when an indexed file lies under `prefix` (a root-relative folder, or
/// empty for the root).
fn index_has_file_under(context_dir: &Path, prefix: &str) -> Option<bool> {
    let dir = format!("{prefix}/");
    let under = |path: &str| prefix.is_empty() || path.starts_with(&dir);
    Some(match hook::open_lookup(context_dir) {
        Some(lookup) => lookup.paths().any(under),
        None => hook::read_wiring(context_dir)?
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::File && under(&n.path)),
    })
}

/// `PreToolUse`: the deny text for one plain search of indexed code, or
/// `None`. The text names the equal `find` command. The caller lets the same
/// search run when the agent sends it again. Every doubt returns `None`: the
/// call is compound, piped or redirected, a flag changes the meaning, the
/// pattern is a regex, the target holds no indexed file, or the index is
/// stale or absent.
#[cfg(test)]
pub(crate) fn pre_search(input: &Value, project_dir: &Path) -> Option<String> {
    pre_search_plan(input, project_dir).map(|plan| plan.deny_text())
}

/// The `find` command that equals one plain search, with the `cd` the search
/// runs after.
#[derive(Debug, PartialEq)]
pub(crate) struct FindPlan {
    ignore_case: bool,
    pattern: String,
    /// The pattern is an identifier that `-w` matches as a whole word.
    word: bool,
    /// The root-relative folder or file, empty for the root.
    scope: String,
    /// The directory of a leading `cd <dir>`.
    cd: Option<String>,
}

impl FindPlan {
    /// The `find` command as the agent reads it.
    fn display(&self) -> String {
        let bin = crate::names::BIN;
        let case = if self.ignore_case { " -i" } else { "" };
        let pattern = &self.pattern;
        let quoted = if self.word {
            format!("\"\\b{pattern}\\b\"")
        } else {
            format!("--fixed \"{pattern}\"")
        };
        let scope = if self.scope.is_empty() {
            String::new()
        } else {
            format!(" --in {}", self.scope)
        };
        format!("{bin} find{case} {quoted}{scope}")
    }

    /// The deny text for a search the host cannot rewrite.
    pub(crate) fn deny_text(&self) -> String {
        let bin = crate::names::BIN;
        format!(
            "[{bin}] Use: {}. {bin} find searches indexed code files only, and the same command sent again runs the raw search.",
            self.display()
        )
    }

    /// A shell command that runs `find` with the binary at `exe`. If `find`
    /// prints nothing, or fails, the command runs `original` instead. If
    /// `find` prints, the first line names the `find` command and says that
    /// the same command sent again runs the raw search.
    pub(crate) fn rewrite(&self, exe: &str, original: &str) -> String {
        let mut find = format!("{} find", sh_quote(exe));
        if self.ignore_case {
            find.push_str(" -i");
        }
        if self.word {
            find.push_str(&format!(
                " {}",
                sh_quote(&format!("\\b{}\\b", self.pattern))
            ));
        } else {
            find.push_str(&format!(" --fixed {}", sh_quote(&self.pattern)));
        }
        if !self.scope.is_empty() {
            find.push_str(&format!(" --in {}", sh_quote(&self.scope)));
        }
        let cd = self
            .cd
            .as_ref()
            .map_or_else(String::new, |dir| format!("cd {} && ", sh_quote(dir)));
        let note = format!(
            "[{}] answered by: {} \u{b7} indexed code files only \u{b7} the same command sent again runs the raw search",
            crate::names::BIN,
            self.display()
        );
        format!(
            "sieve_out=$({cd}{find} 2>/dev/null); if [ -n \"$sieve_out\" ]; then printf '%s\\n%s\\n' {} \"$sieve_out\"; else\n{{\n{original}\n}}\nfi",
            sh_quote(&note)
        )
    }
}

/// Quotes one word for a POSIX shell: single quotes, and a quote inside the
/// word closes, escapes and reopens the quotes.
pub(crate) fn sh_quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}

/// The plan behind [`pre_search`], or `None` on every doubt.
pub(crate) fn pre_search_plan(input: &Value, project_dir: &Path) -> Option<FindPlan> {
    let tool_input = input.get("tool_input")?;
    let str_of = |key: &str| tool_input.get(key).and_then(Value::as_str);
    let mut cd = Cd::None;
    let (pattern, path, ignore_case, fixed, word) =
        if input.get("tool_name").and_then(Value::as_str) == Some("Grep") {
            // Any other field, such as context lines or multiline, changes the search.
            let plain = tool_input.as_object()?.keys().all(|k| {
                matches!(
                    k.as_str(),
                    "pattern" | "path" | "-i" | "-n" | "glob" | "type" | "output_mode"
                )
            });
            let mode_ok = matches!(
                str_of("output_mode"),
                None | Some("content" | "files_with_matches")
            );
            let globs: Vec<String> = str_of("glob").map(String::from).into_iter().collect();
            let types: Vec<String> = str_of("type").map(String::from).into_iter().collect();
            if !plain || !mode_ok || !filters_are_code(&globs, &types) {
                return None;
            }
            let ignore_case = tool_input.get("-i").and_then(Value::as_bool) == Some(true);
            (
                str_of("pattern")?.to_string(),
                str_of("path").map(String::from),
                ignore_case,
                false,
                false,
            )
        } else {
            let command = str_of("command")?;
            // A redirect or a substitution changes what the search reads or writes.
            if command.contains(['<', '>', '`', '$']) {
                return None;
            }
            let Analysis::Plan(plan) = analyze(command) else {
                return None;
            };
            let [search] = plan.searches.as_slice() else {
                return None;
            };
            let reads_stdin = !search.rg && !search.recursive && search.paths.is_empty();
            // Exactly one pattern: the first word, or one `-e`.
            let patterns = usize::from(search.pattern.is_some()) + search.e_patterns.len();
            if plan.sources != 1
                || plan.piped
                || plan.cd == Cd::Compound
                || search.deny_off
                || search.unsafe_flag
                || reads_stdin
                || search.paths.len() > 1
                || patterns != 1
                || !filters_are_code(&search.globs, &search.types)
            {
                return None;
            }
            cd = plan.cd;
            (
                search
                    .pattern
                    .clone()
                    .or_else(|| search.e_patterns.first().cloned())?,
                search.paths.first().cloned(),
                search.ignore_case,
                search.fixed,
                search.word,
            )
        };
    if pattern.is_empty() || pattern.starts_with('-') || pattern.contains(UNQUOTABLE) {
        return None;
    }
    // `-w` names the regex `\bname\b`, which `find` reads like the word
    // match of `rg` only for a plain identifier.
    if word {
        if !pattern
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return None;
        }
    } else if !fixed && pattern.contains(REGEX_META) {
        return None;
    }
    if path
        .as_deref()
        .is_some_and(|p| p.contains(UNQUOTABLE) || non_code(p))
    {
        return None;
    }
    let context_dir = hook::resolve_context_dir(project_dir);
    let wiring_time = std::fs::metadata(hook::wiring_path(&context_dir))
        .and_then(|m| m.modified())
        .ok()?;
    if hook::index_freshness(project_dir, &context_dir).is_some_and(|f| f.missing > 0) {
        return None;
    }
    let root = std::fs::canonicalize(project_dir).ok()?;
    let cwd = input
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
        .map_or_else(|| project_dir.to_path_buf(), PathBuf::from);
    // A `cd <dir>` moves the base of the path. A dir outside the project
    // gives no index path, so the search passes.
    let cwd = match &cd {
        Cd::Dir(dir) => cwd.join(dir),
        _ => cwd,
    };
    let rel = index_path(path.as_deref().unwrap_or("."), &cwd, &root)?;
    let full = root.join(&rel);
    if full.is_file() {
        // A file target must be indexed and no newer than the index.
        let fresh = std::fs::metadata(&full)
            .and_then(|m| m.modified())
            .is_ok_and(|t| t <= wiring_time);
        let wanted = HashSet::from([rel.as_str()]);
        let (indexed, _) = match hook::open_lookup(&context_dir) {
            Some(lookup) => symbols_from_lookup(lookup, &wanted),
            None => symbols_from_graph(&hook::read_wiring(&context_dir)?, &wanted),
        };
        if !fresh || indexed.is_empty() {
            return None;
        }
    } else if !index_has_file_under(&context_dir, &rel)? {
        // `find` would print no hit for a folder of documents or scripts.
        return None;
    }
    Some(FindPlan {
        ignore_case,
        pattern,
        word,
        scope: rel,
        cd: match cd {
            Cd::Dir(dir) => Some(dir),
            _ => None,
        },
    })
}

/// The `rg` type names whose whole file set the index holds. A type such as
/// `java` or `php` also selects files the index does not hold (`.jsp`,
/// `.phtml`), so `find` could give fewer matches. Those types pass.
const CODE_TYPES: [(&str, &[&str]); 2] = [("rust", &["rs"]), ("go", &["go"])];

/// True when every glob and type keeps only files of a language the index
/// holds, so `find` covers at least the files the filter keeps. An exclude
/// glob (`!...`) only removes files, so it is always fine.
fn filters_are_code(globs: &[String], types: &[String]) -> bool {
    let supported = crate::check::supported_extensions();
    let indexed = |ext: &str| supported.contains(&format!(".{}", ext.to_lowercase()).as_str());
    let glob_ok = |glob: &String| {
        if glob.starts_with('!') {
            return true;
        }
        // Only the file-name part (`*.ts`, `*.{ts,tsx}`) names extensions.
        let name = glob.rsplit('/').next().unwrap_or(glob);
        let Some(exts) = name.strip_prefix("*.") else {
            return false;
        };
        let exts = exts.strip_prefix('{').and_then(|e| e.strip_suffix('}'));
        let list = exts.unwrap_or(&name[2..]);
        !list.is_empty()
            && list
                .split(',')
                .all(|e| e.chars().all(|c| c.is_ascii_alphanumeric()) && indexed(e))
    };
    let type_ok = |name: &String| {
        CODE_TYPES
            .iter()
            .find(|(n, _)| n == name)
            .is_some_and(|(_, exts)| exts.iter().all(|e| indexed(e)))
    };
    globs.iter().all(glob_ok) && types.iter().all(type_ok)
}

/// `PostToolUse`: the `updatedToolOutput` JSON for a routed search, or
/// `None`. The function counts the lookup either way. It reads a Grep tool
/// call or a Bash call with one or more `grep` or `rg` segments.
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
/// keeps the later node (the rule of `sieve find`).
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
        // Other lines stay in place; the rows around them route.
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
        // A bare row in a call with two searches is `compound`.
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

    fn pre_input(root: &Path, tool: &str, tool_input: Value) -> Value {
        serde_json::json!({
            "session_id": "s1",
            "cwd": root.to_string_lossy(),
            "tool_name": tool,
            "tool_input": tool_input,
        })
    }

    fn pre_bash(root: &Path, command: &str) -> Option<String> {
        pre_search(
            &pre_input(root, "Bash", serde_json::json!({"command": command})),
            root,
        )
    }

    #[test]
    fn test_pre_search_names_the_equal_find_command() {
        let dir = fixture("pre-deny");
        let root = dir.0.as_path();
        let handlers = "crates/engine/src/handlers";
        assert_eq!(
            pre_bash(root, &format!("rg handle_1 {handlers}")).as_deref(),
            Some(&*format!(
                "[sieve] Use: sieve find --fixed \"handle_1\" --in {handlers}. sieve find searches indexed code files only, and the same command sent again runs the raw search."
            ))
        );
        let text = pre_bash(root, "grep -rn TODO .").expect("denies a folder search");
        assert!(text.contains("sieve find --fixed \"TODO\". "), "{text}");
        let text = pre_bash(root, "rg total").expect("denies a search with no path");
        assert!(text.contains("sieve find --fixed \"total\". "), "{text}");
        let text = pre_bash(root, "rg -in total crates").expect("carries -i");
        assert!(
            text.contains("find -i --fixed \"total\" --in crates."),
            "{text}"
        );
        let text = pre_bash(root, "rg -F total.x crates").expect("-F allows any pattern");
        assert!(
            text.contains("find --fixed \"total.x\" --in crates."),
            "{text}"
        );
        let tool = serde_json::json!({"pattern": "total", "path": handlers});
        let text = pre_search(&pre_input(root, "Grep", tool), root).expect("denies the Grep tool");
        assert!(
            text.contains(&format!("sieve find --fixed \"total\" --in {handlers}.")),
            "{text}"
        );
    }

    #[test]
    fn test_pre_search_passes_what_find_cannot_answer() {
        let dir = fixture("pre-pass");
        let root = dir.0.as_path();
        let file = "crates/engine/src/handlers/group_00_handlers.rs";
        std::fs::create_dir_all(root.join("scripts")).expect("mkdir scripts");
        std::fs::write(root.join("scripts/run.sh"), "echo total\n").expect("script");
        for command in [
            "rg -v total crates",
            "rg -o total crates",
            "rg -x total crates",
            "rg -E utf8 total crates",
            "rg -P total crates",
            "rg -r X total crates",
            "rg 'foo(' crates",
            "grep 'a+' -r crates",
            "rg '(?i)total' crates",
            "rg 'a.b' crates",
            "rg total scripts",
            "rg total | head",
            "rg total && ls",
            "rg total; ls",
            "rg total $(pwd)",
            "rg total > out.txt",
            "rg total < in.txt",
            "rg total README.md",
            "rg -c total crates",
            "rg --json total crates",
            "rg --pre cat total crates",
            "rg total a b",
            "rg \"two words\" crates",
            "rg 'a\"b' crates",
            "grep total",
            "rg total missing-folder",
            "rg total ../outside",
            "ls crates",
            "sieve find total",
        ] {
            assert_eq!(pre_bash(root, command), None, "{command}");
        }
        let glob = serde_json::json!({"pattern": "total", "path": "crates", "glob": "*.md"});
        assert_eq!(
            pre_search(&pre_input(root, "Grep", glob), root),
            None,
            "glob field"
        );
        let tool = serde_json::json!({"pattern": "total", "path": "README.md"});
        assert_eq!(
            pre_search(&pre_input(root, "Grep", tool), root),
            None,
            "document target"
        );
        let command = format!("rg total {file}");
        assert!(pre_bash(root, &command).is_some(), "a fresh indexed file");

        // A file newer than the index passes.
        let newer = std::time::SystemTime::now() + Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(root.join(file))
            .and_then(|f| f.set_modified(newer))
            .expect("touch");
        assert_eq!(pre_bash(root, &command), None, "stale file");
    }

    #[test]
    fn test_pre_search_shape_table() {
        let dir = fixture("pre-shapes");
        let root = dir.0.as_path();
        let r = root.to_string_lossy();
        // Each accepted shape names the `find` command that gives the same matches or more.
        let word = |name: &str| format!("find \"\\b{name}\\b\"");
        let fixed = |name: &str| format!("find --fixed \"{name}\"");
        let deny = [
            (
                "rg -lw --glob '!sieve/**' -g '*.{ts,tsx}' roleAllows",
                word("roleAllows") + ".",
            ),
            (
                &format!("cd {r} && rg -nw --glob '!sieve/**' -g '*.{{ts,tsx}}' writeSetting"),
                word("writeSetting") + ".",
            ),
            ("rg -w total", word("total") + "."),
            ("grep -rnw total crates", word("total") + " --in crates."),
            (
                "rg -wi total crates",
                "find -i \"\\btotal\\b\" --in crates.".to_string(),
            ),
            (
                "rg --word-regexp total crates",
                word("total") + " --in crates.",
            ),
            ("grep -rlw total crates", word("total") + " --in crates."),
            ("rg -l total crates", fixed("total") + " --in crates."),
            ("rg --files-with-matches total", fixed("total") + "."),
            (
                "rg -g '*.rs' total crates",
                fixed("total") + " --in crates.",
            ),
            ("rg --glob=*.rs total", fixed("total") + "."),
            ("rg -t rust total crates", fixed("total") + " --in crates."),
            ("rg --type=go total", fixed("total") + "."),
            (
                "grep -rn --include='*.rs' total crates",
                fixed("total") + " --in crates.",
            ),
            (
                "grep -rn --exclude-dir=sieve --exclude='*.md' total crates",
                fixed("total") + " --in crates.",
            ),
            ("rg -g '!*.md' total", fixed("total") + "."),
            ("rg -e total crates", fixed("total") + " --in crates."),
            ("rg -etotal crates", fixed("total") + " --in crates."),
            ("cd crates && rg total", fixed("total") + " --in crates."),
            (
                "cd crates; rg total engine",
                fixed("total") + " --in crates/engine.",
            ),
            (
                &format!("cd {r}/crates && rg -w total ."),
                word("total") + " --in crates.",
            ),
        ];
        for (command, want) in deny {
            let text = pre_bash(root, command).unwrap_or_else(|| panic!("denies: {command}"));
            assert!(text.contains(&want), "{command}\n{text}");
            // The same row gives a rewrite that falls back to the original.
            let input = pre_input(root, "Bash", serde_json::json!({"command": command}));
            let plan = pre_search_plan(&input, root).expect("a plan");
            let rewritten = plan.rewrite("/opt/sieve", command);
            assert!(rewritten.contains("'/opt/sieve' find"), "{rewritten}");
            assert!(
                rewritten.ends_with(&format!("else\n{{\n{command}\n}}\nfi")),
                "{rewritten}"
            );
            assert!(
                rewritten.contains("answered by: [sieve]")
                    || rewritten.contains("answered by: sieve find"),
                "{rewritten}"
            );
        }
        let pass = [
            "for p in a b; do rg -lw $p; done",
            "rg -c -w x",
            "rg -w x -g '*.md'",
            "rg -w total crates | head",
            "rg -v total crates",
            "rg -A 2 total crates",
            "rg -C2 -w total crates",
            "rg -e total -e handle crates",
            "rg total -e handle crates",
            "rg -w 'a.b' crates",
            "rg -w 'two words' crates",
            "rg total crates -g '*.{rs,md}'",
            "rg total -t md",
            "rg total -t nosuchtype",
            "rg -t php total crates",
            "rg -t java total crates",
            "rg -t ts total crates",
            "rg -t rs total crates",
            "rg --iglob '*.rs' total",
            "cd /tmp && rg -w total",
            "cd .. && rg -w total",
            "cd crates && cd engine && rg total",
            "rg -w total crates; ls",
            "grep -rc total crates",
            "rg 'a.b' crates",
        ];
        for command in pass {
            assert_eq!(pre_bash(root, command), None, "{command}");
        }
        let tool = |tool_input: Value| pre_search(&pre_input(root, "Grep", tool_input), root);
        let text = tool(serde_json::json!({
            "pattern": "total", "glob": "*.rs", "output_mode": "files_with_matches", "-n": true
        }))
        .expect("Grep tool with a code glob");
        assert!(text.contains(&(fixed("total") + ".")), "{text}");
        let typed = serde_json::json!({"pattern": "total", "type": "go", "output_mode": "content"});
        assert!(tool(typed).is_some());
        assert!(tool(serde_json::json!({"pattern": "total", "output_mode": "count"})).is_none());
        assert!(tool(serde_json::json!({"pattern": "total", "glob": "*.md"})).is_none());
        assert!(tool(serde_json::json!({"pattern": "total", "-A": 2})).is_none());
    }

    #[test]
    fn test_pre_search_rewrite_keeps_the_cd_and_quotes_every_word() {
        let dir = fixture("pre-rewrite");
        let root = dir.0.as_path();
        let plan_of = |command: &str| {
            let input = pre_input(root, "Bash", serde_json::json!({"command": command}));
            pre_search_plan(&input, root)
        };
        let plan = plan_of("cd crates && rg -w total engine").expect("a plan");
        let text = plan.rewrite("/opt/it's/sieve", "cd crates && rg -w total engine");
        assert!(
            text.starts_with(
                "sieve_out=$(cd 'crates' && '/opt/it'\\''s/sieve' find '\\btotal\\b' --in 'crates/engine' 2>/dev/null);"
            ),
            "{text}"
        );
        assert!(
            text.contains("printf '%s\\n%s\\n' '[sieve] answered by: sieve find "),
            "{text}"
        );
        assert!(
            text.contains(" \u{b7} indexed code files only \u{b7} the same command sent again runs the raw search'"),
            "{text}"
        );
        // A trailing comment or `;` in the original cannot break the wrapper.
        for original in ["grep -rn total crates # note", "grep -rn total crates;"] {
            let input = pre_input(root, "Bash", serde_json::json!({"command": original}));
            let plan = pre_search_plan(&input, root).expect("a plan");
            let exe = root.join("no-such-sieve");
            // A missing `sieve` binary makes `find` fail, so the original runs.
            let script = plan.rewrite(exe.to_str().expect("utf8"), original);
            let ran = std::process::Command::new("sh")
                .arg("-c")
                .arg(&script)
                .current_dir(root)
                .output()
                .expect("run sh");
            assert!(ran.status.success(), "{original}: {script}");
            let out = String::from_utf8_lossy(&ran.stdout);
            assert!(out.contains("group_00_handlers.rs:"), "{original}: {out}");
        }
        // A word that the shell could split or expand never reaches a command.
        for command in [
            "rg 'two words' crates",
            "rg \"a'b\" crates",
            "rg total 'my dir'",
        ] {
            assert!(plan_of(command).is_none(), "{command}");
        }
        assert_eq!(sh_quote("a'b c"), "'a'\\''b c'");
    }

    #[test]
    fn test_pre_search_passes_without_an_index() {
        let dir = Scratch::new("pre-no-index");
        std::fs::write(dir.0.join("a.rs"), "fn total() {}\n").expect("write");
        assert_eq!(pre_bash(&dir.0, "rg total a.rs"), None);
    }
}
