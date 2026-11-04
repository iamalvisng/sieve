//! The `sieve why` subcommand: which decision doc entry
//! explains a symbol. The links are recorded, not guessed from prose. An
//! entry holds a `**Code:**` line of node ids (the anchor). A commit sha in
//! an entry links to the nodes that the commit changed (L0). The build
//! writes the links to `.cache/why-refs.json`. A query and the post-read
//! hook read it. No git at query time, no
//! network and no MCP tool.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use clap::Args;
use serde::{Deserialize, Serialize};
use sieve_core::wiring::{Graph, Kind, Node};
use sieve_query::callers::resolve_symbol;

use crate::query;
use crate::why_infer::{load_topics, topics_with_stamp, TopicRefs, TopicRow};
use crate::why_rows::{span_rows, GitFn, TYPE_CAP};

/// The sidecar layout version. A reader drops a sidecar with another value.
const SIDECAR_VERSION: u32 = 4;
/// Decision docs above this total size are not read.
const DOC_CAP_BYTES: u64 = 5 * 1024 * 1024;
/// The time limit for the one L0 git call per sha.
const GIT_TIMEOUT: Duration = Duration::from_secs(2);
/// The most L0 git calls in one build.
const MAX_GIT_CALLS: usize = 50;
/// One commit that changes more nodes than this gets file-level links only.
const MAX_COMMIT_NODES: usize = 20;
/// The most git output that the linker reads.
const GIT_MAX_BYTES: u64 = 8 * 1024 * 1024;
/// The default row count of one `sieve why` answer.
const DEFAULT_ROWS: usize = 10;
/// The default line cap of `sieve why --diff`.
const DIFF_ROWS: usize = 40;
/// The hint limit in characters. 40 tokens at 4 characters each.
const HINT_MAX_CHARS: usize = 160;
/// The link level names: anchor, commit, path, agent, folder.
const LEVELS: [&str; 5] = ["anchor", "commit", "path", "agent", "folder"];
/// The level of a link that an agent confirmed with `sieve why --link`.
const AGENT_LEVEL: u8 = 3;
/// The level of a link that an agent confirmed for a whole folder. It ranks
/// after every file link.
const FOLDER_LEVEL: u8 = 4;
/// What an inferred row says about its link. It is not a recorded link.
const INFERRED_VIA: &str = "inferred from shared terms (not a recorded link)";

/// Flags for `sieve why`.
#[derive(Args, Debug)]
pub struct WhyArgs {
    /// The symbol, or `path:line`. With `--suggest`, a rev.
    pub symbol: Option<String>,

    /// The repo root. Default: the nearest ancestor with a graph.
    #[arg(value_name = "dir")]
    pub dir: Option<PathBuf>,

    /// Lists the SUPERSEDED entries that still link to code.
    #[arg(long)]
    pub check: bool,

    /// Prints the result as JSON.
    #[arg(long)]
    pub json: bool,

    /// Shows every row, not the first 10. Adds the file-level rows and the
    /// commit-mention rows.
    #[arg(long)]
    pub all: bool,

    /// Prints the ids of the changed non-test symbols, one per line, for a
    /// `Code:` line. The positional argument is then a rev. With no rev, it
    /// reads the working tree diff. With a rev, it reads that commit.
    #[arg(long)]
    pub suggest: bool,

    /// Lists the recorded decisions and the stale anchors for a diff. The
    /// positional argument is then a rev or `<base>...<head>`. With no
    /// argument, it reads the working tree diff.
    #[arg(long)]
    pub diff: bool,

    /// Skips the pre-query graph refresh.
    #[arg(long = "no-refresh")]
    pub no_refresh: bool,

    /// Proposes candidate code files for one decision record, for your agent
    /// to confirm. With no record key, it takes the next unlinked record.
    /// The positional argument is then the record key, `<doc>#<heading>`.
    #[arg(long)]
    pub link: bool,

    /// With `--link`: the numbers of the candidates to keep, such as `3,7,12`,
    /// or `none`. Needs `--expect`.
    #[arg(long, value_name = "numbers")]
    pub keep: Option<String>,

    /// With `--link`: the link id of the proposal that you read.
    #[arg(long, value_name = "link id")]
    pub expect: Option<String>,

    /// With `--link` and a record key: the page of candidates, 1 to 3. The
    /// next page is due when you keep 3 or more files of a page.
    #[arg(long, value_name = "page", requires = "link")]
    pub page: Option<usize>,
}

/// One decision doc entry, as the sidecar stores it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryRec {
    /// The doc path, relative to the repo root.
    pub doc: String,
    /// The 1-based line of the entry heading.
    pub line: usize,
    /// The heading text, with the date.
    pub heading: String,
    /// The entry date `YYYY-MM-DD`, or an empty string.
    pub date: String,
    /// True when the entry is SUPERSEDED.
    pub superseded: bool,
    /// True when an agent confirmed the entry with `sieve why --link` and the
    /// entry text has not changed since.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub linked: bool,
    /// The heading of the later entry that supersedes this one, if known.
    pub successor: Option<String>,
}

/// One link from an entry to a node, or to a file when `node` is `None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkRec {
    /// The index into `entries`.
    pub entry: usize,
    /// The link level: 0 anchor, 1 commit, 2 path, 3 agent, 4 folder.
    pub level: u8,
    /// The node id. `None` is a file-level link.
    pub node: Option<String>,
    /// The code path.
    pub path: String,
    /// The 1-based start line of the node. 0 for a file-level link. A folder
    /// link holds the depth of its folder here, so a deeper folder ranks first.
    pub line: usize,
    /// The node name. Empty for a file-level link.
    pub name: String,
    /// What made the link, for the human.
    pub via: String,
}

/// The sidecar: every entry and every link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhyRefs {
    /// The layout version.
    pub version: u32,
    /// The doc and graph stamp the links were made from.
    pub stamp: String,
    /// Every decision entry.
    pub entries: Vec<EntryRec>,
    /// Every link.
    pub links: Vec<LinkRec>,
    /// Every `Code:` item that no longer resolves.
    pub stale: Vec<StaleRec>,
    /// Every sha that an entry names and git could not read. The `id` is
    /// the sha.
    pub missing_shas: Vec<StaleRec>,
}

/// One `Code:` item that names a node or file that is gone, or one sha that
/// git could not read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaleRec {
    /// The index into `entries`.
    pub entry: usize,
    /// The id as the entry wrote it.
    pub id: String,
}

/// One entry with its text. The sidecar does not store the text.
pub(crate) struct RawEntry {
    pub(crate) doc: String,
    pub(crate) line: usize,
    pub(crate) heading: String,
    pub(crate) date: String,
    pub(crate) text: String,
    status_superseded: bool,
}

// ---------------------------------------------------------------------------
// Doc discovery and entry parsing
// ---------------------------------------------------------------------------

fn collect_md(root: &Path, rel: &str, depth: u32, out: &mut Vec<String>) {
    let Ok(read) = std::fs::read_dir(root.join(rel)) else {
        return;
    };
    for item in read.flatten() {
        let name = item.file_name().to_string_lossy().into_owned();
        let child = format!("{rel}/{name}");
        let Ok(kind) = item.file_type() else { continue };
        if kind.is_dir() {
            if depth > 0 {
                collect_md(root, &child, depth - 1, out);
            }
        } else if name.ends_with(".md") {
            out.push(child);
        }
    }
}

/// The folder that `.adr-dir` names, if it is a plain path inside the repo.
fn adr_dir_setting(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(root.join(".adr-dir")).ok()?;
    let dir = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    let dir = dir.trim_end_matches('/');
    let plain = Path::new(dir)
        .components()
        .all(|c| matches!(c, std::path::Component::Normal(_)));
    // A link inside the repo can point outside it: compare real paths.
    let real = std::fs::canonicalize(root.join(dir)).ok()?;
    let inside = real.starts_with(std::fs::canonicalize(root).ok()?);
    (plain && inside).then(|| dir.to_string())
}

/// Finds the record docs: `LEDGER.md`, `ERRORS.md`, `docs/decisions`,
/// `docs/adr`, `doc/adr`, `adr`, `docs/errors`, the folder in `.adr-dir`,
/// and `ADR-*.md` in the root or in `docs`. Paths are relative and sorted.
pub(crate) fn find_docs(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let setting = adr_dir_setting(root);
    let dirs = [
        "docs/decisions",
        "docs/adr",
        "doc/adr",
        "adr",
        "docs/errors",
    ];
    for dir in dirs.into_iter().chain(setting.as_deref()) {
        collect_md(root, dir, 3, &mut out);
    }
    for dir in ["", "docs"] {
        let Ok(read) = std::fs::read_dir(root.join(dir)) else {
            continue;
        };
        for item in read.flatten() {
            let name = item.file_name().to_string_lossy().into_owned();
            let lower = name.to_lowercase();
            let is_adr = lower.starts_with("adr-") && lower.ends_with(".md");
            let is_log = name == "ERRORS.md" || (dir.is_empty() && name == "LEDGER.md");
            if (is_adr || is_log) && item.path().is_file() {
                out.push(if dir.is_empty() {
                    name
                } else {
                    format!("{dir}/{name}")
                });
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

pub(crate) fn stat_part(path: &Path) -> String {
    let Ok(meta) = std::fs::metadata(path) else {
        return "missing".to_string();
    };
    let nanos = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    format!("{}:{nanos}", meta.len())
}

/// The stamp of the doc files and `wiring.json`. A change in any of them
/// makes the sidecar stale.
pub(crate) fn current_stamp(root: &Path, context_dir: &Path, docs: &[String]) -> String {
    let mut parts: Vec<String> = docs
        .iter()
        .map(|d| format!("{d}:{}", stat_part(&root.join(d))))
        .collect();
    // The link file counts as a doc, but only when it exists, so a repo with
    // no link file keeps its stamp.
    let links = root.join(crate::why_link::LINKS_FILE);
    if links.is_file() {
        parts.push(format!(
            "{}:{}",
            crate::why_link::LINKS_FILE,
            stat_part(&links)
        ));
    }
    parts.push(format!(
        "wiring:{}",
        stat_part(&context_dir.join(".graph").join("wiring.json"))
    ));
    parts.join("|")
}

/// Finds the first `YYYY-MM-DD` in `text`.
fn find_date(text: &str) -> Option<String> {
    let b = text.as_bytes();
    (0..b.len().saturating_sub(9)).find_map(|i| {
        let w = &b[i..i + 10];
        let ok = w.iter().enumerate().all(|(k, c)| match k {
            4 | 7 => *c == b'-',
            _ => c.is_ascii_digit(),
        });
        ok.then(|| text[i..i + 10].to_string())
    })
}

/// True when a `Status:` line of `text` says Superseded, Deprecated or
/// Rejected.
fn has_superseded_status(text: &str) -> bool {
    text.lines().any(|l| {
        let t = l.trim_start_matches(['*', '-', ' ']).to_lowercase();
        t.strip_prefix("status:")
            .map(|v| v.trim_start_matches(['*', ' ']))
            .is_some_and(|v| {
                ["superseded", "deprecated", "rejected"]
                    .iter()
                    .any(|w| v.starts_with(w))
            })
    })
}

/// True when the 10 bytes of `s` read `YYYY-MM-DD`.
fn is_date(s: &str) -> bool {
    s.len() == 10
        && s.bytes().enumerate().all(|(k, c)| match k {
            4 | 7 => c == b'-',
            _ => c.is_ascii_digit(),
        })
}

/// The date of an entry heading: a leading `YYYY-MM-DD`, or a trailing one
/// as in `[title, YYYY-MM-DD]`. A date inside the title text does not count.
fn heading_date(heading: &str) -> Option<&str> {
    let lead = heading.get(..10).filter(|d| is_date(d));
    let tail = heading.trim_end_matches([']', ')', ' ']);
    let tail = tail
        .get(tail.len().checked_sub(10)?..)
        .filter(|d| is_date(d));
    lead.or(tail)
}

/// The heading level of a line (`## x` is 2) and its text.
fn heading_of(line: &str) -> Option<(usize, &str)> {
    let level = line.bytes().take_while(|&c| c == b'#').count();
    let text = line[level..].strip_prefix(' ')?;
    (1..=6).contains(&level).then_some((level, text.trim()))
}

/// The entries that start at the headings of `level`, each with its date.
/// An entry runs to the next heading of the same or a higher level. An
/// entry with no date is skipped, as is the undated template.
fn entries_at(lines: &[&str], heads: &[(usize, usize, &str)], level: usize) -> Vec<RawEntry> {
    let date_field = |body: &str| {
        body.lines()
            .find(|l| l.contains("Date:"))
            .and_then(find_date)
    };
    let mut out = Vec::new();
    for (k, &(start, lv, heading)) in heads.iter().enumerate() {
        if lv != level {
            continue;
        }
        let end = heads[k + 1..]
            .iter()
            .find(|h| h.1 <= level)
            .map_or(lines.len(), |h| h.0);
        let body = lines[start..end].join("\n");
        let Some(date) = heading_date(heading)
            .map(str::to_string)
            .or_else(|| date_field(&body))
        else {
            continue;
        };
        out.push(RawEntry {
            doc: String::new(),
            line: start + 1,
            heading: heading.to_string(),
            date,
            status_superseded: has_superseded_status(&body),
            text: body,
        });
    }
    out
}

/// Splits one doc into entries. A log has one dated heading per entry. The
/// entry level is the heading level with the most dated entries. A doc with
/// no dated heading is one entry, if it has a `Date:` line. Any other doc
/// gives no entry.
fn parse_entries(doc: &str, text: &str) -> Vec<RawEntry> {
    let lines: Vec<&str> = text.lines().collect();
    let mut heads: Vec<(usize, usize, &str)> = Vec::new();
    let mut fenced = false;
    for (i, l) in lines.iter().enumerate() {
        if l.starts_with("```") {
            fenced = !fenced;
        }
        if let Some((level, h)) = heading_of(l).filter(|_| !fenced) {
            heads.push((i, level, h));
        }
    }
    // The deepest level wins a tie: a `# title` holds the whole doc, so its
    // `Date:` field is that of the first entry below it.
    let best = (1..=6)
        .map(|lv| entries_at(&lines, &heads, lv))
        .max_by_key(Vec::len)
        .unwrap_or_default();
    let mut out = if best.is_empty() {
        let date = lines
            .iter()
            .find(|l| l.contains("Date:"))
            .and_then(|l| find_date(l));
        let heading = heads
            .iter()
            .find(|h| h.1 == 1)
            .map_or_else(|| doc.to_string(), |h| h.2.to_string());
        date.map(|date| RawEntry {
            doc: String::new(),
            line: 1,
            date,
            status_superseded: has_superseded_status(text),
            heading,
            text: text.to_string(),
        })
        .into_iter()
        .collect()
    } else {
        best
    };
    for e in &mut out {
        e.doc = doc.to_string();
    }
    out
}

// ---------------------------------------------------------------------------
// Supersede rule
// ---------------------------------------------------------------------------

/// Lowercases, turns an em dash, an en dash and a comma into a space, and
/// collapses spaces. The compare then treats " — " and ", " as one
/// separator.
pub(crate) fn normalize(s: &str) -> String {
    let mapped: String = s
        .to_lowercase()
        .chars()
        .map(|c| {
            if matches!(c, '—' | '–' | ',') {
                ' '
            } else {
                c
            }
        })
        .collect();
    mapped.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Finds the later entry that supersedes entry `i`: a sentence with one of
/// the verbs reverses, replaces, supersedes or overrides that quotes the
/// heading of `i`, and has no "keeps" or "stands".
fn find_successor(entries: &[RawEntry], norm_text: &[String], i: usize) -> Option<usize> {
    let head = normalize(&entries[i].heading);
    if head.is_empty() {
        return None;
    }
    // An explicit "Supersedes" line wins over the sentence rule. It names the
    // old heading, with or without the leading date.
    let undated = {
        let (first, rest) = head.split_once(' ').unwrap_or((&head, ""));
        if is_date(first) {
            rest
        } else {
            head.as_str()
        }
    };
    for (j, e) in entries.iter().enumerate() {
        if j == i || e.date < entries[i].date {
            continue;
        }
        // A line inside a fenced code block is an example, not a record.
        let mut fenced = false;
        let named = e.text.lines().any(|line| {
            if line.starts_with("```") {
                fenced = !fenced;
            }
            !fenced
                && line.split_once("**Supersedes:**").is_some_and(|(_, name)| {
                    let name = normalize(name);
                    name == head || name == undated
                })
        });
        if named {
            return Some(j);
        }
    }
    for (j, text) in norm_text.iter().enumerate() {
        if j == i || entries[j].date < entries[i].date {
            continue;
        }
        let mut from = 0;
        while let Some(pos) = text[from..].find(&head) {
            let at = from + pos;
            let left = text[..at].rfind(". ").map_or(0, |p| p + 2);
            let right = text[at + head.len()..]
                .find(". ")
                .map_or(text.len(), |p| at + head.len() + p);
            let sentence = &text[left..right];
            let verb = ["reverses", "replaces", "supersedes", "overrides"]
                .iter()
                .any(|v| sentence.contains(v));
            let negative = sentence.contains("keeps") || sentence.contains("stands");
            if verb && !negative {
                return Some(j);
            }
            from = at + head.len();
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Source index: node spans and string literals
// ---------------------------------------------------------------------------

/// Parses a span such as `L376-L378`.
fn span_lines(span: &str) -> Option<(usize, usize)> {
    let (a, b) = span.split_once('-')?;
    Some((
        a.trim_start_matches('L').parse().ok()?,
        b.trim_start_matches('L').parse().ok()?,
    ))
}

/// True for a path of test code or a doc.
fn is_test_path(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    path.starts_with("tests/")
        || path.contains("/tests/")
        || path.contains("/test/")
        || name.starts_with("test_")
        || name.contains("_test.")
        || name.contains(".test.")
        || name.contains(".spec.")
        || path.ends_with(".md")
}

/// The nodes of each path, as `(start, end, node index)`, with no file and
/// no module node.
fn spans_by_path(graph: &Graph) -> HashMap<&str, Vec<(usize, usize, usize)>> {
    let mut by_path: HashMap<&str, Vec<(usize, usize, usize)>> = HashMap::new();
    for (idx, n) in graph.nodes.iter().enumerate() {
        if matches!(n.kind, Kind::File | Kind::Module) {
            continue;
        }
        if let Some((a, b)) = span_lines(&n.span) {
            by_path
                .entry(n.path.as_str())
                .or_default()
                .push((a, b, idx));
        }
    }
    by_path
}

/// Like `spans_by_path`, but with no test file and no node inside a
/// `tests` module.
fn code_spans_by_path(graph: &Graph) -> HashMap<&str, Vec<(usize, usize, usize)>> {
    let mut by_path = spans_by_path(graph);
    by_path.retain(|p, _| !is_test_path(p));
    for m in graph
        .nodes
        .iter()
        .filter(|n| n.kind == Kind::Module && n.name == "tests")
    {
        if let (Some(v), Some((a, b))) = (by_path.get_mut(m.path.as_str()), span_lines(&m.span)) {
            v.retain(|s| !(a <= s.0 && s.1 <= b));
        }
    }
    by_path
}

/// The nodes of `spans` that hold `line`, the innermost first.
fn enclosing(spans: &[(usize, usize, usize)], line: usize) -> Vec<usize> {
    let mut held: Vec<_> = spans
        .iter()
        .filter(|(a, b, _)| *a <= line && line <= *b)
        .collect();
    held.sort_by_key(|(a, b, _)| b - a);
    held.into_iter().map(|s| s.2).collect()
}

/// The candidate commit shas of `text`: 7 to 40 hex characters with a digit
/// and a letter.
fn candidate_shas(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tok in text.split(|c: char| !c.is_ascii_alphanumeric()) {
        let hex = (7..=40).contains(&tok.len()) && tok.bytes().all(|b| b.is_ascii_hexdigit());
        let mixed =
            tok.bytes().any(|b| b.is_ascii_digit()) && tok.bytes().any(|b| b.is_ascii_lowercase());
        let lower = tok.bytes().all(|b| !b.is_ascii_uppercase());
        if hex && mixed && lower && !out.iter().any(|o| o == tok) {
            out.push(tok.to_string());
        }
    }
    out.truncate(5);
    out
}

/// Runs git under a 2 s limit. Gives the stdout, or `None` on any failure
/// or when the output is over 8 MB. A thread drains the pipe, so a large
/// diff does not block git.
fn run_git(root: &Path, args: &[&str]) -> Option<String> {
    run_prog("git", root, args).ok()
}

/// Reads one C-style quoted path, as git writes a path with a tab, a quote
/// or a backslash even when `core.quotePath` is false. Gives the path with
/// no quotes. A text with no quotes stays as it is.
fn unquote_path(s: &str) -> String {
    let Some(inner) = s.strip_prefix('"').and_then(|r| r.strip_suffix('"')) else {
        return s.to_string();
    };
    let b = inner.as_bytes();
    let octal = |k: usize| b.get(k).is_some_and(|d| (b'0'..=b'7').contains(d));
    let mut out: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'\\' || i + 1 >= b.len() {
            out.push(b[i]);
            i += 1;
        } else if octal(i + 1) && octal(i + 2) && octal(i + 3) {
            let v = (u32::from(b[i + 1] - b'0') << 6)
                | (u32::from(b[i + 2] - b'0') << 3)
                | u32::from(b[i + 3] - b'0');
            out.push((v & 0xff) as u8);
            i += 4;
        } else {
            out.push(match b[i + 1] {
                b'a' => 7,
                b'b' => 8,
                b't' => b'\t',
                b'n' => b'\n',
                b'v' => 11,
                b'f' => 12,
                b'r' => b'\r',
                other => other,
            });
            i += 2;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Runs `prog` like `run_git`. Gives the stdout, or the reason for a
/// failure: `timeout`, `output over 8 MB`, `git failed` or `git not found`.
fn run_prog(prog: &str, root: &Path, args: &[&str]) -> Result<String, &'static str> {
    let mut child = Command::new(prog)
        .current_dir(root)
        .args(["-c", "core.quotePath=false"])
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "git not found")?;
    let pipe = child.stdout.take().ok_or("git failed")?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        // One byte over the cap shows an oversize output. Dropping the pipe
        // then ends git with a broken pipe.
        let _ =
            std::io::Read::read_to_end(&mut std::io::Read::take(pipe, GIT_MAX_BYTES + 1), &mut buf);
        buf
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() < GIT_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(5));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("timeout");
            }
        }
    };
    let bytes = reader.join().map_err(|_| "git failed")?;
    if bytes.len() as u64 > GIT_MAX_BYTES {
        return Err("output over 8 MB");
    }
    if !status.success() {
        return Err("git failed");
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// The changed new-side line ranges of each file in a `-U0` diff. A file
/// header counts only between a `diff --git` line and the first `@@` line
/// of that block, so an added line that starts with `++ ` is not a header.
/// A merge commit (`@@@`) gives no range, on purpose. A pure deletion
/// (`+N,0`) gives no range: it has no new-side line.
fn parse_hunks(diff: &str) -> Vec<(String, Vec<(usize, usize)>)> {
    let mut out: Vec<(String, Vec<(usize, usize)>)> = Vec::new();
    let mut in_header = false;
    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            in_header = true;
        } else if in_header && line.starts_with("+++ ") {
            // A deleted file shows `/dev/null`: it has no new-side lines.
            let path = unquote_path(line[4..].trim_end_matches('\t'));
            let path = path.strip_prefix("b/").unwrap_or("");
            out.push((path.to_string(), Vec::new()));
        } else if line.starts_with("@@") {
            in_header = false;
            let Some(rest) = line.strip_prefix("@@ ") else {
                continue;
            };
            let Some(new_side) = rest.split(" +").nth(1).and_then(|r| r.split(' ').next()) else {
                continue;
            };
            let (start, count) = new_side.split_once(',').unwrap_or((new_side, "1"));
            let (Ok(start), Ok(count)) = (start.parse::<usize>(), count.parse::<usize>()) else {
                continue;
            };
            if let (Some(last), true) = (out.last_mut(), count > 0) {
                last.1.push((start, start + count - 1));
            }
        }
    }
    out
}

/// The code nodes that the hunks of `diff` overlap, and the touched graph
/// files. No test node and no doc. A line number is the line of the
/// commit, so a later edit to the file moves it.
fn hunk_targets(graph: &Graph, diff: &str) -> (Vec<usize>, Vec<String>) {
    let spans = code_spans_by_path(graph);
    let mut nodes: Vec<usize> = Vec::new();
    let mut files: Vec<String> = Vec::new();
    for (path, ranges) in parse_hunks(diff) {
        let Some(on_path) = spans.get(path.as_str()) else {
            continue;
        };
        files.push(path);
        for &(a, b, idx) in on_path {
            if ranges.iter().any(|r| r.0 <= b && a <= r.1) && !nodes.contains(&idx) {
                nodes.push(idx);
            }
        }
    }
    (nodes, files)
}

/// The `-U0` diff text for the working tree, a commit `rev`, or a range
/// `<base>...<head>`. This is the one git call of `--suggest` and `--diff`.
fn diff_text(root: &Path, rev: Option<&str>) -> Result<String, String> {
    match rev {
        Some(r) if r.starts_with('-') => return Err(format!("bad rev: {r}")),
        Some(r) if r.contains("...") => run_git(root, &["diff", "-U0", "-M", "--no-color", r]),
        Some(r) => run_git(root, &["show", "-U0", "-M", "--format=", "--no-color", r]),
        None => run_git(root, &["diff", "-U0", "-M", "--no-color", "HEAD"]),
    }
    .ok_or_else(|| "git could not give the diff".to_string())
}

/// The ids for `sieve why --suggest`: the changed non-test symbols of the
/// working tree diff, or of the commit `rev`.
fn suggest_ids(root: &Path, graph: &Graph, rev: Option<&str>) -> Result<Vec<String>, String> {
    let diff = diff_text(root, rev)?;
    let mut ids: Vec<String> = hunk_targets(graph, &diff)
        .0
        .into_iter()
        .map(|i| graph.nodes[i].id.clone())
        .collect();
    ids.sort();
    Ok(ids)
}

/// Every path that the diff changes, deletes or renames from. It reads the
/// `diff --git` header, so a binary or mode-only change counts, and so does a
/// quoted path. A rename line gives both names. A function removed inside a
/// file leaves the file in this set.
fn diff_paths(diff: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    for line in diff.lines() {
        let names: Vec<&str> = if let Some(h) = line.strip_prefix("diff --git ") {
            // `a/X b/X`: the two halves are equal when the path has no quote.
            if let Some(i) = h.find(" \"b/") {
                vec![&h[..i], &h[i + 1..]]
            } else if let (true, Some((x, y))) = (h.starts_with('"'), h.split_once("\" ")) {
                vec![&h[..x.len() + 1], y]
            } else if h.len() % 2 == 1 {
                vec![&h[..h.len() / 2], &h[h.len() / 2 + 1..]]
            } else {
                Vec::new()
            }
        } else {
            (line.strip_prefix("rename from ").into_iter())
                .chain(line.strip_prefix("rename to "))
                .collect()
        };
        for n in names {
            let n = unquote_path(n);
            let p = n.strip_prefix("a/").or_else(|| n.strip_prefix("b/"));
            out.insert(p.unwrap_or(&n).to_string());
        }
    }
    out
}

/// The text or JSON answer of `sieve why --diff`. It runs no git call.
fn diff_report(
    refs: &WhyRefs,
    graph: &Graph,
    diff: &str,
    basis: &str,
    all: bool,
    json: bool,
) -> String {
    let (idx, files) = hunk_targets(graph, diff);
    let mut nodes: Vec<&Node> = idx.iter().map(|&i| &graph.nodes[i]).collect();
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    let paths = diff_paths(diff);
    let stale: Vec<&StaleRec> = (refs.stale.iter())
        .filter(|s| paths.contains(s.id.split('#').next().unwrap_or("")))
        .collect();
    // Anchors that name one node. An anchor on a whole file counts per file.
    let by_node: Vec<(&Node, Vec<&LinkRec>)> = nodes
        .iter()
        .map(|&n| {
            (
                n,
                links_for(refs, n, false)
                    .into_iter()
                    .filter(|l| l.node.is_some())
                    .collect(),
            )
        })
        .filter(|(_, v): &(&Node, Vec<&LinkRec>)| !v.is_empty())
        .collect();
    let mut file_counts: BTreeMap<&str, usize> = BTreeMap::new();
    for l in refs
        .links
        .iter()
        .filter(|l| l.node.is_none() && l.level == 0)
    {
        if files.contains(&l.path) {
            *file_counts.entry(l.path.as_str()).or_default() += 1;
        }
    }
    let plural = if stale.len() == 1 { "" } else { "s" };
    let counts = if nodes.is_empty() {
        "no code symbol changed".to_string()
    } else {
        format!(
            "{} changed symbols  {} with decisions",
            nodes.len(),
            by_node.len()
        )
    };
    let head = format!(
        "why --diff {basis}  {counts}  {} stale anchor{plural}",
        stale.len()
    );
    let stale_row = |s: &StaleRec| {
        let e = &refs.entries[s.entry];
        format!("stale anchor  {}  {}:{}", s.id, e.doc, e.line)
    };
    if json {
        let v = serde_json::json!({
            "basis": basis,
            "changed": nodes.len(),
            "symbols": by_node.iter().map(|(n, ls)| serde_json::json!({
                "id": n.id,
                "decisions": ls.iter().map(|l| link_json(refs, l)).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "fileAnchors": file_counts,
            "stale": stale.iter().map(|s| stale_row(s)).collect::<Vec<_>>(),
        });
        return format!("{}\n", serde_json::to_string_pretty(&v).unwrap_or_default());
    }
    let mut rows: Vec<String> = Vec::new();
    for (n, ls) in &by_node {
        rows.push(node_head(n).trim_end().to_string());
        rows.extend(ls.iter().flat_map(|l| {
            row_text(refs, l, false)
                .lines()
                .map(String::from)
                .collect::<Vec<_>>()
        }));
    }
    for (p, c) in &file_counts {
        rows.push(format!(
            "{p}: {c} file-level decisions (sieve why {p} --all)"
        ));
    }
    rows.extend(stale.iter().map(|s| stale_row(s)));
    let cap = if all { rows.len() } else { DIFF_ROWS };
    let more = rows.len().saturating_sub(cap);
    let mut out = format!("{head}\n{}\n", rows[..rows.len() - more].join("\n"));
    if more > 0 {
        out.push_str(&format!("more: {more}, --all\n"));
    }
    out.replace("\n\n", "\n")
}

// ---------------------------------------------------------------------------
// The linker
// ---------------------------------------------------------------------------

/// Reads the decision docs under `root`. Gives `(path, text)` pairs. Stops
/// at the size cap.
pub(crate) fn read_docs(root: &Path, docs: &[String]) -> Vec<(String, String)> {
    let mut total = 0u64;
    let mut out = Vec::new();
    for d in docs {
        let Ok(text) = std::fs::read_to_string(root.join(d)) else {
            continue;
        };
        total += text.len() as u64;
        if total > DOC_CAP_BYTES {
            break;
        }
        out.push((d.clone(), text));
    }
    out
}

/// A link key: one link per entry and target. The lowest level wins.
pub(crate) type LinkKey = (usize, String);

fn add_link(map: &mut BTreeMap<LinkKey, LinkRec>, rec: LinkRec) {
    let key = (
        rec.entry,
        rec.node
            .clone()
            .unwrap_or_else(|| format!("file:{}", rec.path)),
    );
    match map.get(&key) {
        Some(old) if old.level <= rec.level => {}
        _ => {
            map.insert(key, rec);
        }
    }
}

fn node_link(entry: usize, level: u8, n: &Node, via: String) -> LinkRec {
    LinkRec {
        entry,
        level,
        node: Some(n.id.clone()),
        path: n.path.clone(),
        line: span_lines(&n.span).map_or(0, |s| s.0),
        name: n.name.clone(),
        via,
    }
}

fn file_link(entry: usize, level: u8, path: &str, via: String) -> LinkRec {
    LinkRec {
        entry,
        level,
        node: None,
        path: path.to_string(),
        line: 0,
        name: String::new(),
        via,
    }
}

/// The node ids and paths in the backticks of the anchor line of an entry.
/// An anchor line starts with `- **Code:**` or `* **Code:**`.
fn code_items(text: &str) -> Vec<String> {
    let Some(line) = text.lines().map(str::trim).find(|l| {
        l.strip_prefix("- ")
            .or_else(|| l.strip_prefix("* "))
            .is_some_and(|r| r.starts_with("**Code:**"))
    }) else {
        return Vec::new();
    };
    let after = line.split_once("**Code:**").map_or("", |p| p.1);
    after
        .split('`')
        .skip(1)
        .step_by(2)
        .map(str::trim)
        .filter(|s| {
            !s.is_empty()
                && !s.contains(char::is_whitespace)
                && (s.contains('#') || Path::new(s).extension().is_some())
        })
        .map(str::to_string)
        .collect()
}

/// The graph lookups that the anchor and path rules share.
pub(crate) struct LinkTargets<'a> {
    by_id: HashMap<&'a str, &'a Node>,
    file_paths: HashSet<&'a str>,
}

impl<'a> LinkTargets<'a> {
    /// Indexes the nodes of `graph` by id, and the file paths.
    pub(crate) fn new(graph: &'a Graph) -> Self {
        LinkTargets {
            by_id: graph.nodes.iter().map(|n| (n.id.as_str(), n)).collect(),
            file_paths: graph
                .nodes
                .iter()
                .filter(|n| n.kind == Kind::File)
                .map(|n| n.path.as_str())
                .collect(),
        }
    }
}

/// Adds the git-free links of entry `i`: the anchor links (level 0) and the
/// full-path links (level 2). The sidecar build and the topic pass both use
/// it, so both agree on which entries a file has as exact rows.
pub(crate) fn add_static_links(
    i: usize,
    text: &str,
    targets: &LinkTargets,
    links: &mut BTreeMap<LinkKey, LinkRec>,
    stale: &mut Vec<StaleRec>,
) {
    // Anchor: an id that the entry records. It links to that node.
    for item in code_items(text) {
        match targets.by_id.get(item.as_str()) {
            Some(n) if n.kind == Kind::File => {
                add_link(links, file_link(i, 0, &n.path, "Code anchor".to_string()));
            }
            Some(n) => add_link(links, node_link(i, 0, n, "Code anchor".to_string())),
            None => stale.push(StaleRec { entry: i, id: item }),
        }
    }

    // Path: the entry names a whole file in its text. Shown with --all.
    for tok in text.split(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '.' | '/' | '-'))) {
        if tok.contains('/') && targets.file_paths.contains(tok) {
            add_link(links, file_link(i, 2, tok, "names the file".to_string()));
        }
    }
}

/// Parses the entries of the docs, sorted by doc and line. This is the entry
/// order of the sidecar.
pub(crate) fn raw_entries(docs: &[(String, String)]) -> Vec<RawEntry> {
    let mut raw: Vec<RawEntry> = docs
        .iter()
        .flat_map(|(path, text)| parse_entries(path, text))
        .collect();
    raw.sort_by(|a, b| (&a.doc, a.line).cmp(&(&b.doc, b.line)));
    raw
}

/// Makes the sidecar content from the docs and the graph. L0 runs `git show`
/// for a sha that an entry names, under `root`. A `root` that is no repo
/// gives no L0 link.
pub fn link_docs(root: &Path, graph: &Graph, docs: &[(String, String)], stamp: String) -> WhyRefs {
    let raw = raw_entries(docs);
    let norm_text: Vec<String> = raw.iter().map(|e| normalize(&e.text)).collect();
    let successors: Vec<Option<usize>> = (0..raw.len())
        .map(|i| find_successor(&raw, &norm_text, i))
        .collect();
    let mut entries: Vec<EntryRec> = raw
        .iter()
        .enumerate()
        .map(|(i, e)| EntryRec {
            doc: e.doc.clone(),
            line: e.line,
            heading: e.heading.clone(),
            date: e.date.clone(),
            superseded: e.status_superseded || successors[i].is_some(),
            linked: false,
            successor: successors[i].map(|j| raw[j].heading.clone()),
        })
        .collect();

    let mut links: BTreeMap<LinkKey, LinkRec> = BTreeMap::new();
    let mut stale: Vec<StaleRec> = Vec::new();
    let mut missing_shas: Vec<StaleRec> = Vec::new();
    let targets = LinkTargets::new(graph);
    let mut git_calls = 0;
    let mut sha_cache: HashMap<String, Option<String>> = HashMap::new();
    for (i, e) in raw.iter().enumerate() {
        add_static_links(i, &e.text, &targets, &mut links, &mut stale);

        // L0: a commit sha in the entry. The hunks of the commit give the
        // nodes. A commit with more than 20 nodes gives file links only.
        for sha in candidate_shas(&e.text) {
            if !sha_cache.contains_key(&sha) && git_calls < MAX_GIT_CALLS {
                git_calls += 1;
                let diff = run_git(root, &["show", "-U0", "--format=", "--no-color", &sha]);
                sha_cache.insert(sha.clone(), diff);
            }
            let Some(cached) = sha_cache.get(&sha) else {
                continue;
            };
            let Some(diff) = cached else {
                missing_shas.push(StaleRec { entry: i, id: sha });
                continue;
            };
            let via = format!("commit mention (inferred) {}", &sha[..7]);
            let (nodes, files) = hunk_targets(graph, diff);
            if nodes.len() > MAX_COMMIT_NODES {
                for path in files {
                    add_link(&mut links, file_link(i, 1, &path, via.clone()));
                }
            } else {
                for idx in nodes {
                    add_link(&mut links, node_link(i, 1, &graph.nodes[idx], via.clone()));
                }
            }
        }
    }
    // Agent links: the kept files of a record that an agent confirmed. A
    // record whose text changed since then has no agent link.
    let confirmations = crate::why_link::read_links(root);
    let keys = crate::why_link::record_keys(&raw);
    for (i, done) in crate::why_link::confirmed(&raw, &keys, &confirmations)
        .into_iter()
        .enumerate()
    {
        let Some(done) = done else { continue };
        entries[i].linked = true;
        for path in done
            .kept
            .iter()
            .filter(|p| targets.file_paths.contains(p.as_str()))
        {
            let via = format!("agent link, confirmed {}", done.date);
            add_link(&mut links, file_link(i, AGENT_LEVEL, path, via));
        }
        // A hand-edited sidecar cannot grow the work: read at most the
        // number of folders that Sieve proposes. A folder with over the cap
        // of files is ignored, as if never kept; `why --check` says why.
        let mut folders: Vec<&str> = done
            .folders
            .iter()
            .take(crate::why_link::FOLDER_CARDS)
            .map(|f| f.path.as_str())
            .collect();
        folders.sort_by_key(|f| std::cmp::Reverse(f.matches('/').count()));
        for dir in folders {
            let prefix = format!("{dir}/");
            let under: Vec<&str> = targets
                .file_paths
                .iter()
                .copied()
                .filter(|p| p.starts_with(&prefix))
                .collect();
            if under.len() > crate::why_link::FOLDER_MAX_FILES {
                continue;
            }
            let via = format!("agent link, folder {dir}/, confirmed {}", done.date);
            for path in under {
                let mut rec = file_link(i, FOLDER_LEVEL, path, via.clone());
                rec.line = dir.matches('/').count() + 1;
                add_link(&mut links, rec);
            }
        }
    }
    WhyRefs {
        version: SIDECAR_VERSION,
        stamp,
        entries,
        links: links.into_values().collect(),
        stale,
        missing_shas,
    }
}

// ---------------------------------------------------------------------------
// The sidecar
// ---------------------------------------------------------------------------

fn sidecar_path(context_dir: &Path) -> PathBuf {
    context_dir.join(".cache").join("why-refs.json")
}

fn write_sidecar(context_dir: &Path, refs: &WhyRefs) -> std::io::Result<()> {
    let path = sidecar_path(context_dir);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    let text = serde_json::to_string(refs).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)
}

/// Builds the links and writes the sidecar. The build calls this after the
/// graph is written. Gives an error text on a write failure. The caller
/// never fails the build on it.
pub fn write_after_build(root: &Path, context_dir: &Path, graph: &Graph) -> Result<(), String> {
    let docs = find_docs(root);
    let stamp = current_stamp(root, context_dir, &docs);
    let refs = link_docs(root, graph, &read_docs(root, &docs), stamp);
    write_sidecar(context_dir, &refs).map_err(|e| e.to_string())
}

/// Reads the sidecar when its stamp is `stamp`. It never builds and never
/// runs git.
fn sidecar_with_stamp(context_dir: &Path, stamp: &str) -> Option<WhyRefs> {
    let refs: WhyRefs =
        serde_json::from_slice(&std::fs::read(sidecar_path(context_dir)).ok()?).ok()?;
    (refs.version == SIDECAR_VERSION && refs.stamp == stamp).then_some(refs)
}

/// Reads the sidecar when its stamp is current. It never builds and never
/// runs git.
fn fresh_sidecar(root: &Path, context_dir: &Path) -> Option<WhyRefs> {
    let docs = find_docs(root);
    let stamp = current_stamp(root, context_dir, &docs);
    sidecar_with_stamp(context_dir, &stamp)
}

/// Gives the current sidecar. Else it builds the links again from
/// `load_graph` and tries to write the sidecar. A write failure is not
/// fatal. The CLI uses it.
fn load_refs(
    root: &Path,
    context_dir: &Path,
    load_graph: impl FnOnce() -> Option<Graph>,
) -> Option<WhyRefs> {
    if let Some(refs) = fresh_sidecar(root, context_dir) {
        return Some(refs);
    }
    let docs = find_docs(root);
    let stamp = current_stamp(root, context_dir, &docs);
    let graph = load_graph()?;
    let refs = link_docs(root, &graph, &read_docs(root, &docs), stamp);
    let _ = write_sidecar(context_dir, &refs);
    let _ = crate::why_infer::write_topics(root, context_dir, &graph, None);
    Some(refs)
}

// ---------------------------------------------------------------------------
// Query
// ---------------------------------------------------------------------------

/// Orders rows: level first, then node-level before file-level, then the
/// newest date.
fn ranked<'a>(refs: &'a WhyRefs, mut links: Vec<&'a LinkRec>) -> Vec<&'a LinkRec> {
    links.sort_by(|a, b| {
        let da = &refs.entries[a.entry].date;
        let db = &refs.entries[b.entry].date;
        // A deeper folder first: a folder link holds its depth in `line`.
        let depth = |l: &LinkRec| if l.level == FOLDER_LEVEL { l.line } else { 0 };
        (a.level, a.node.is_none())
            .cmp(&(b.level, b.node.is_none()))
            .then(depth(b).cmp(&depth(a)))
            .then(db.cmp(da))
            .then(a.entry.cmp(&b.entry))
    });
    links
}

/// The links that answer for node `n`: its own links, then the file-level
/// links of its file that the node does not already have. Only the anchor
/// (level 0) shows by default. A commit link (level 1) shows only when `all`
/// is set.
///
/// A file query also answers for the file's nodes and for the entries that
/// name the file by its full path. So a file shows the anchors inside it and
/// the path mentions by default. Each entry shows once, with its strongest
/// link.
fn links_for<'a>(refs: &'a WhyRefs, n: &Node, all: bool) -> Vec<&'a LinkRec> {
    if n.kind == Kind::File {
        return file_links(refs, &n.path, all);
    }
    let own: Vec<&LinkRec> = refs
        .links
        .iter()
        .filter(|l| l.node.as_deref() == Some(n.id.as_str()) && (all || l.level == 0))
        .collect();
    let seen: HashSet<usize> = own.iter().map(|l| l.entry).collect();
    let file_level: Vec<&LinkRec> = refs
        .links
        .iter()
        .filter(|l| {
            l.node.is_none()
                && l.path == n.path
                && !seen.contains(&l.entry)
                && (all || l.level == 0)
        })
        .collect();
    ranked(refs, own.into_iter().chain(file_level).collect())
}

/// Picks the links to show for a node.
type Pick = for<'a> fn(&'a WhyRefs, &Node, bool) -> Vec<&'a LinkRec>;

/// The most file-level anchors a `path:line` answer shows when it falls
/// back to the file.
const LINE_FILE_ANCHORS: usize = 3;

/// The links for a `path:line` query. A node answers with its own links
/// only. A file answers with its file-level anchors and its agent links, at
/// most `LINE_FILE_ANCHORS` even with `--all`, never a path mention.
fn line_links<'a>(refs: &'a WhyRefs, n: &Node, all: bool) -> Vec<&'a LinkRec> {
    let file = n.kind == Kind::File;
    let own = refs.links.iter().filter(|l| {
        if file {
            l.node.is_none()
                && l.path == n.path
                && (l.level == 0 || l.level == AGENT_LEVEL || l.level == FOLDER_LEVEL)
        } else {
            l.node.as_deref() == Some(n.id.as_str()) && (all || l.level == 0)
        }
    });
    let mut links = ranked(refs, own.collect());
    if file {
        links.truncate(LINE_FILE_ANCHORS);
    }
    links
}

/// The links of every node in `path`, and of the file itself, one per entry.
/// The default keeps the anchors and the full-path mentions.
fn file_links<'a>(refs: &'a WhyRefs, path: &str, all: bool) -> Vec<&'a LinkRec> {
    let here = refs
        .links
        .iter()
        .filter(|l| l.path == path && (all || l.level != 1));
    let mut seen = HashSet::new();
    ranked(refs, here.collect())
        .into_iter()
        .filter(|l| seen.insert(l.entry))
        .collect()
}

/// One row of text for a link. `show_name` adds the node name to the link
/// line, for a file answer where the rows come from several nodes.
fn row_text(refs: &WhyRefs, l: &LinkRec, show_name: bool) -> String {
    let e = &refs.entries[l.entry];
    let kind = if l.level == AGENT_LEVEL || l.level == FOLDER_LEVEL {
        "agent link"
    } else if l.node.is_some() {
        "decision"
    } else {
        "file-level"
    };
    let name = if show_name && l.node.is_some() {
        format!(" ({})", l.name)
    } else {
        String::new()
    };
    let date = if e.date.is_empty() {
        String::new()
    } else {
        format!("  [{}]", e.date)
    };
    let mut s = format!(
        "{kind}  {}:{}  {}\n          link: {}{name}{date}\n",
        e.doc, e.line, e.heading, l.via
    );
    if e.superseded {
        let by = e
            .successor
            .as_deref()
            .map_or("its Status line".to_string(), |h| format!("\"{h}\""));
        s.push_str(&format!("          SUPERSEDED by {by}\n"));
    }
    s
}

fn node_head(n: &Node) -> String {
    let kind = format!("{:?}", n.kind).to_lowercase();
    format!(
        "{}\n",
        sieve_core::voice::row(&n.name, &kind, &n.path, Some(&n.span))
    )
}

/// The text answer for a list of nodes.
fn render_text(
    refs: &WhyRefs,
    query: &str,
    nodes: &[&Node],
    all: bool,
    ctx: Option<&Ctx>,
    pick: Pick,
) -> String {
    let mut out = String::new();
    let mut done = HashSet::new();
    for n in nodes {
        out.push_str(&node_head(n));
        let links = pick(refs, n, all);
        // Decisions first, then comments, tests and history.
        let mut rows: Vec<String> = links
            .iter()
            .map(|l| row_text(refs, l, n.kind == Kind::File))
            .collect();
        rows.extend(
            inferred_rows(ctx, refs, n, &links, &mut done)
                .into_iter()
                .map(inferred_text),
        );
        let (more, hidden) = evidence(ctx, n, all);
        rows.extend(more);
        if links.is_empty() {
            // Other rows follow the line, so it must not say nothing was found.
            let line = if rows.is_empty() {
                "no decision found for"
            } else {
                "no recorded link for"
            };
            out.push_str(&format!("{line} {}\n", n.name));
        }
        let cap = if all { rows.len() } else { DEFAULT_ROWS };
        let cut = rows.len().saturating_sub(cap) + hidden;
        for row in rows.iter().take(cap) {
            out.push_str(row.trim_end_matches('\n'));
            out.push('\n');
        }
        if cut > 0 {
            out.push_str(&format!("more: {cut} rows. sieve why {query} --all\n"));
        }
    }
    out
}

/// What a query needs besides the links: the repo root, the graph, and
/// whether to run git for the history. The hook has no `Ctx`.
struct Ctx<'a> {
    root: &'a Path,
    graph: &'a Graph,
    history: bool,
    /// The inferred rows, when the repo has any.
    topics: Option<&'a TopicRefs>,
    /// The file of a `path:line` query. Its inferred rows follow the rows of
    /// the line, whatever node answers.
    line_path: Option<&'a str>,
}

/// The inferred rows to print after the exact rows of `n`: the rows of the
/// file of a `path:line` query, or of a file node. A row that the exact rows
/// already show is left out. `done` holds the files whose rows an earlier
/// node of the same answer printed, so a file prints its rows once.
fn inferred_rows<'a>(
    ctx: Option<&'a Ctx>,
    refs: &WhyRefs,
    n: &Node,
    exact: &[&LinkRec],
    done: &mut HashSet<String>,
) -> Vec<&'a TopicRow> {
    let Some(c) = ctx else {
        return Vec::new();
    };
    let path = c
        .line_path
        .or_else(|| (n.kind == Kind::File).then_some(n.path.as_str()));
    let Some(file) = path.and_then(|p| c.topics?.file(p)) else {
        return Vec::new();
    };
    if !done.insert(file.path.clone()) {
        return Vec::new();
    }
    let shown: HashSet<(&str, usize)> = exact
        .iter()
        .map(|l| {
            let e = &refs.entries[l.entry];
            (e.doc.as_str(), e.line)
        })
        .collect();
    file.rows
        .iter()
        .filter(|r| !shown.contains(&(r.doc.as_str(), r.line)))
        .collect()
}

/// One text row for an inferred record.
fn inferred_text(r: &TopicRow) -> String {
    format!(
        "inferred  {}:{}  {}\n          link: {INFERRED_VIA}\n",
        r.doc, r.line, r.heading
    )
}

/// The rows from the span of `n`, in output order, with the count of rows
/// that a per-type limit hid. `--all` hides none.
fn evidence(ctx: Option<&Ctx>, n: &Node, all: bool) -> (Vec<String>, usize) {
    let Some(c) = ctx else {
        return (Vec::new(), 0);
    };
    let git = |a: &[&str]| run_prog("git", c.root, a);
    let git_dyn: &GitFn = &git;
    let r = span_rows(
        c.root,
        c.graph,
        n,
        c.history.then_some(git_dyn),
        sieve_query::blast::is_test_path,
    );
    let (mut out, mut hidden) = (Vec::new(), 0);
    for mut rows in [r.comments, r.tests] {
        if !all && rows.len() > TYPE_CAP {
            hidden += rows.len() - TYPE_CAP;
            rows.truncate(TYPE_CAP);
        }
        out.extend(rows);
    }
    out.extend(r.history);
    (out, hidden)
}

/// The JSON of one inferred record.
fn inferred_json(r: &TopicRow) -> serde_json::Value {
    serde_json::json!({
        "doc": r.doc,
        "line": r.line,
        "date": r.date,
        "heading": r.heading,
        "level": "inferred",
        "via": INFERRED_VIA,
        "fileLevel": true,
        "superseded": false,
        "supersededBy": serde_json::Value::Null,
    })
}

fn link_json(refs: &WhyRefs, l: &LinkRec) -> serde_json::Value {
    let e = &refs.entries[l.entry];
    serde_json::json!({
        "doc": e.doc,
        "line": e.line,
        "date": e.date,
        "heading": e.heading,
        "level": LEVELS[usize::from(l.level)],
        "via": l.via,
        "fileLevel": l.node.is_none(),
        "superseded": e.superseded,
        "supersededBy": e.successor,
    })
}

fn render_json(
    refs: &WhyRefs,
    query: &str,
    nodes: &[&Node],
    all: bool,
    ctx: Option<&Ctx>,
    pick: Pick,
) -> serde_json::Value {
    let mut done = HashSet::new();
    let symbols: Vec<serde_json::Value> = nodes
        .iter()
        .map(|n| {
            let links = pick(refs, n, all);
            let inferred = inferred_rows(ctx, refs, n, &links, &mut done);
            let total = links.len() + inferred.len();
            let cap = if all { total } else { DEFAULT_ROWS };
            let rows: Vec<_> = links
                .iter()
                .map(|l| link_json(refs, l))
                .chain(inferred.into_iter().map(inferred_json))
                .take(cap)
                .collect();
            serde_json::json!({
                "id": n.id,
                "name": n.name,
                "path": n.path,
                "line": span_lines(&n.span).map_or(0, |s| s.0),
                "decisions": rows,
                "evidence": evidence(ctx, n, all).0,
                "total": total,
            })
        })
        .collect();
    serde_json::json!({ "query": query, "symbols": symbols })
}

/// Splits `path:line`. Gives `None` for a plain symbol.
fn split_path_line(q: &str) -> Option<(&str, usize)> {
    let (p, l) = q.rsplit_once(':')?;
    let line = l.parse().ok()?;
    (p.contains('.') || p.contains('/')).then_some((p, line))
}

/// The answer for a line of a decision doc: the code that the entry
/// explains.
fn render_reverse(refs: &WhyRefs, doc: &str, line: usize) -> Option<String> {
    let idx = refs
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.doc == doc && e.line <= line)
        .max_by_key(|(_, e)| e.line)?
        .0;
    let e = &refs.entries[idx];
    let mut out = format!("why {}:{}  {}\n", e.doc, e.line, e.heading);
    let mut links: Vec<&LinkRec> = refs.links.iter().filter(|l| l.entry == idx).collect();
    links.sort_by_key(|l| (l.level, l.node.is_none(), l.path.clone(), l.line));
    if links.is_empty() {
        out.push_str("this entry links to no code\n");
    }
    for l in links {
        let target = if l.node.is_some() {
            format!("{}  {}:{}", l.name, l.path, l.line)
        } else {
            format!("(file)  {}", l.path)
        };
        out.push_str(&format!("code  {target}  {}\n", l.via));
    }
    Some(out)
}

/// The `--check` report: SUPERSEDED entries that still link to code, and
/// the `Code:` items that no longer resolve. `in_repo` is false outside a
/// repo that git knows: the report then has one line about it and no
/// per-sha line.
fn render_check(refs: &WhyRefs, in_repo: bool) -> String {
    let mut out = String::new();
    let mut count = 0;
    for (i, e) in refs
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.superseded)
    {
        let linked: Vec<&LinkRec> = ranked(
            refs,
            refs.links
                .iter()
                .filter(|l| l.entry == i && l.level == 0)
                .collect(),
        );
        if linked.is_empty() {
            continue;
        }
        count += 1;
        let by = e
            .successor
            .as_deref()
            .map_or("its Status line".to_string(), |h| format!("\"{h}\""));
        out.push_str(&format!(
            "SUPERSEDED  {}:{}  {}\n  by {by}\n",
            e.doc, e.line, e.heading
        ));
        for l in linked.iter().take(5) {
            let target = if l.node.is_some() {
                format!("{} {}:{}", l.name, l.path, l.line)
            } else {
                format!("(file) {}", l.path)
            };
            out.push_str(&format!("  still links: {target}\n"));
        }
    }
    for st in &refs.stale {
        let e = &refs.entries[st.entry];
        out.push_str(&format!(
            "STALE ANCHOR  {}:{}  {}\n  {}\n",
            e.doc, e.line, e.heading, st.id
        ));
    }
    let missing: &[StaleRec] = if in_repo { &refs.missing_shas } else { &[] };
    for m in missing {
        let e = &refs.entries[m.entry];
        out.push_str(&format!(
            "sha not found: {}\n  {}:{}  {}\n",
            m.id, e.doc, e.line, e.heading
        ));
    }
    if count == 0 && refs.stale.is_empty() && missing.is_empty() {
        out.push_str("no superseded entry links to code\nno stale anchor\n");
    } else {
        out.push_str(&format!(
            "{count} superseded entries still link to code, {} stale anchors\n",
            refs.stale.len()
        ));
    }
    if !in_repo {
        out.push_str("not a git repo: commit checks skipped\n");
    }
    out
}

/// Runs `sieve why`.
pub fn run(args: &WhyArgs, dir_override: Option<&Path>) -> Result<(), String> {
    if args.suggest && args.diff {
        return Err("--suggest and --diff do not combine: use one".to_string());
    }
    if args.link && (args.suggest || args.diff || args.check) {
        return Err("--link does not combine with --suggest, --diff or --check".to_string());
    }
    if !args.link && (args.keep.is_some() || args.expect.is_some()) {
        return Err("--keep and --expect need --link".to_string());
    }
    if args.link {
        crate::why_link::check_flags(args)?;
    }
    let (root, context_dir) =
        query::run_prelude(args.dir.as_deref(), dir_override, args.no_refresh)?;
    let graph = query::read_wiring_named(&context_dir)?;
    if args.link {
        return crate::why_link::run(args, &root, &context_dir, &graph);
    }
    if args.suggest {
        for id in suggest_ids(&root, &graph, args.symbol.as_deref())? {
            println!("{id}");
        }
        return Ok(());
    }
    let refs = load_refs(&root, &context_dir, || Some(graph.clone()))
        .ok_or_else(|| "no decision index".to_string())?;
    if args.diff {
        let rev = args.symbol.as_deref();
        let diff = diff_text(&root, rev)?;
        print!(
            "{}",
            diff_report(
                &refs,
                &graph,
                &diff,
                rev.unwrap_or("working tree"),
                args.all,
                args.json
            )
        );
        return Ok(());
    }
    if args.check {
        let in_repo = run_git(&root, &["rev-parse", "--git-dir"]).is_some();
        print!("{}", render_check(&refs, in_repo));
        print!(
            "{}",
            crate::why_link::stale_block(&root, &context_dir, &graph)
        );
        return Ok(());
    }
    let query = args
        .symbol
        .as_deref()
        .ok_or_else(|| "missing symbol: give a symbol, a path:line, or --check".to_string())?;
    let topics = load_topics(&root, &context_dir, &graph);
    print!(
        "{}",
        query_text(
            &refs,
            &graph,
            &root,
            query,
            args.all,
            args.json,
            topics.as_ref()
        )?
    );
    Ok(())
}

/// The answer text for one symbol query. The CLI and the MCP tool both use
/// it, so both give the same text. It runs one `git log -L` call for the
/// history rows.
fn query_text(
    refs: &WhyRefs,
    graph: &Graph,
    root: &Path,
    query: &str,
    all: bool,
    json: bool,
    topics: Option<&TopicRefs>,
) -> Result<String, String> {
    let mut note = String::new();
    let mut pick: Pick = links_for;
    let line_path = split_path_line(query).map(|(path, _)| path);
    let nodes: Vec<&Node> = match split_path_line(query) {
        Some((path, line)) => {
            if let Some(text) = render_reverse(refs, path, line) {
                return Ok(text);
            }
            let (chosen, said) = walk_up(refs, graph, path, line, all);
            note = said;
            pick = line_links;
            chosen.into_iter().collect()
        }
        None => resolve_symbol(graph, query, None).map_err(|e| e.to_string())?,
    };
    if nodes.is_empty() {
        return Err(format!("no symbol at {query}"));
    }
    let ctx = Ctx {
        root,
        graph,
        history: true,
        topics,
        line_path,
    };
    if json {
        let v = render_json(refs, query, &nodes, all, Some(&ctx), pick);
        let text = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
        return Ok(format!("{text}\n"));
    }
    Ok(note + &render_text(refs, query, &nodes, all, Some(&ctx), pick))
}

/// The node that answers for `path:line`: the innermost node that has a
/// record, else the nearest enclosing node that has one, else the file. When
/// no level has a record, the innermost node. The text says at which level
/// the record links, or is empty when the innermost node answers.
fn walk_up<'a>(
    refs: &WhyRefs,
    graph: &'a Graph,
    path: &str,
    line: usize,
    all: bool,
) -> (Option<&'a Node>, String) {
    let spans = spans_by_path(graph);
    let inner = spans
        .get(path)
        .map(|s| enclosing(s, line))
        .unwrap_or_default();
    let file = graph
        .nodes
        .iter()
        .find(|n| n.kind == Kind::File && n.path == path);
    let chain: Vec<&Node> = inner
        .into_iter()
        .map(|i| &graph.nodes[i])
        .chain(file)
        .collect();
    let first = chain.first().copied();
    let Some(hit) = chain
        .iter()
        .copied()
        .find(|n| !line_links(refs, n, all).is_empty())
    else {
        return (first, String::new());
    };
    if first.is_some_and(|f| std::ptr::eq(f, hit)) {
        return (first, String::new());
    }
    let level = if hit.kind == Kind::File {
        format!("file (file-level anchors only, at most {LINE_FILE_ANCHORS})")
    } else {
        format!(
            "enclosing {} {}",
            format!("{:?}", hit.kind).to_lowercase(),
            hit.name
        )
    };
    let at = first.map_or(String::new(), |f| format!(" at {}", f.name));
    (
        Some(hit),
        format!("no record{at}: the record links at the {level}\n"),
    )
}

/// The MCP tool `sieve_why`. It gives the text that `sieve why <symbol>`
/// prints, and whether the text is an error. It never runs `gh`. The
/// daemon calls it through a function pointer, because the daemon crate
/// cannot depend on this crate.
pub fn mcp_why(symbol: &str, all: bool, root: &Path, context_dir: &Path) -> (String, bool) {
    let result = query::read_wiring_named(context_dir).and_then(|graph| {
        let refs = load_refs(root, context_dir, || Some(graph.clone()))
            .ok_or_else(|| "no decision index".to_string())?;
        let topics = load_topics(root, context_dir, &graph);
        query_text(&refs, &graph, root, symbol, all, false, topics.as_ref())
    });
    match result {
        Ok(text) => (text, false),
        Err(e) => (e, true),
    }
}

// ---------------------------------------------------------------------------
// The post-read hint
// ---------------------------------------------------------------------------

/// What a Read can add: the decision hint for the file, and the note that
/// asks the agent to link the decision records.
pub struct ReadNotes {
    /// The decision hint for the read range, if any.
    pub hint: Option<String>,
    /// The link note, when it was asked for and a record has no confirmed link.
    pub link_note: Option<String>,
}

/// The one-line hint for a Read of `file_path`, and, when `want_link_note` is
/// set, the link note. `range` is the read range as 1-based lines, or `None`
/// for the whole file. The hint quotes only the in-repo decision doc. It
/// needs a `Code:` anchor to a node of the read range. The link note counts
/// the sidecar entries that have no confirmed link. Both read the current
/// sidecar once. Neither builds the links, reads a record doc or runs git.
pub fn read_notes(
    project_dir: &Path,
    context_dir: &Path,
    file_path: &str,
    range: Option<(usize, usize)>,
    want_link_note: bool,
) -> ReadNotes {
    let rel = std::fs::canonicalize(file_path)
        .ok()
        .zip(std::fs::canonicalize(project_dir).ok())
        .and_then(|(abs, base)| {
            let rel = abs.strip_prefix(&base).ok()?;
            Some(rel.to_string_lossy().into_owned())
        });
    if rel.is_none() && !want_link_note {
        return ReadNotes {
            hint: None,
            link_note: None,
        };
    }
    let docs = find_docs(project_dir);
    let stamp = current_stamp(project_dir, context_dir, &docs);
    let refs = sidecar_with_stamp(context_dir, &stamp);
    let link_note = refs.as_ref().filter(|_| want_link_note).and_then(|r| {
        let unlinked = r.entries.iter().filter(|e| !e.linked).count();
        (unlinked > 0).then(|| crate::why_link::link_note(unlinked))
    });
    let hint = rel.and_then(|rel| {
        let anchored = refs.as_ref().and_then(|r| hint_from(r, &rel, range));
        // A file with no recorded link gets the first inferred record, from
        // the topic sidecar. A stale topic sidecar gives nothing. An anchored
        // hit pays no config read.
        anchored.or_else(|| {
            if !sieve_core::walk::read_why_inferred(project_dir) {
                return None;
            }
            inferred_hint(&topics_with_stamp(context_dir, &stamp)?, &rel)
        })
    });
    ReadNotes { hint, link_note }
}

/// The one-line hint for the first inferred record of `rel`, or `None` when
/// the file has a recorded link, no inferred record, or the line is too long.
fn inferred_hint(topics: &TopicRefs, rel: &str) -> Option<String> {
    let file = topics.file(rel).filter(|f| !f.exact)?;
    let row = file.rows.first()?;
    let name = Path::new(rel).file_name()?.to_string_lossy();
    let text = format!(
        "sieve: {name} has an inferred record ({}:{}, {}). Run sieve why {rel}.",
        row.doc, row.line, row.date
    );
    (text.chars().count() <= HINT_MAX_CHARS).then_some(text)
}

fn hint_from(refs: &WhyRefs, rel: &str, range: Option<(usize, usize)>) -> Option<String> {
    let (lo, hi) = range.unwrap_or((0, usize::MAX));
    let strong: Vec<&LinkRec> = refs
        .links
        .iter()
        .filter(|l| l.node.is_some() && l.level == 0 && l.path == rel)
        .filter(|l| l.line >= lo && l.line <= hi)
        .collect();
    let best = ranked(refs, strong).into_iter().next()?;
    let e = &refs.entries[best.entry];
    let tail = if e.superseded { ", superseded" } else { "" };
    let text = format!(
        "sieve: {} has a decision ({}:{}, {}{tail}). Run sieve why {}.",
        best.name, e.doc, e.line, e.date, best.name
    );
    (text.chars().count() <= HINT_MAX_CHARS).then_some(text)
}

// ---------------------------------------------------------------------------
// Scorer for a label file
// ---------------------------------------------------------------------------

#[cfg(test)]
/// One hand label: a symbol and the entry headings that explain it.
#[derive(Debug, Deserialize)]
pub struct Label {
    /// The code path.
    pub file: String,
    /// The symbol name.
    pub symbol: String,
    /// The 1-based line of the symbol.
    pub line: usize,
    /// The headings that explain the symbol.
    #[serde(default)]
    pub explained_by: Vec<String>,
}

/// The scores of the linker against a label file.
#[cfg(test)]
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Score {
    /// Node-level links that name a labelled heading.
    pub correct: usize,
    /// Node-level links (anchor and commit) emitted for the labelled symbols.
    pub emitted: usize,
    /// The count of labelled headings.
    pub labelled: usize,
    /// Each false link, as text.
    pub false_links: Vec<String>,
    /// Labels with no node in the graph.
    pub unmatched: Vec<String>,
}

#[cfg(test)]
impl Score {
    /// Correct links over emitted links. 1.0 when none was emitted.
    pub fn precision(&self) -> f64 {
        if self.emitted == 0 {
            1.0
        } else {
            self.correct as f64 / self.emitted as f64
        }
    }

    /// Correct links over labelled links. 1.0 when none is labelled.
    pub fn recall(&self) -> f64 {
        if self.labelled == 0 {
            1.0
        } else {
            self.correct as f64 / self.labelled as f64
        }
    }
}

#[cfg(test)]
/// Scores the links of the given `levels` against the labels in
/// `labels_json`.
pub fn score_labels(
    labels_json: &str,
    graph: &Graph,
    refs: &WhyRefs,
    levels: &[u8],
) -> Result<Score, String> {
    let labels: Vec<Label> = serde_json::from_str(labels_json).map_err(|e| e.to_string())?;
    let mut score = Score::default();
    for label in &labels {
        score.labelled += label.explained_by.len();
        let found: Vec<&Node> = graph
            .nodes
            .iter()
            .filter(|n| n.kind != Kind::File && n.path == label.file && n.name == label.symbol)
            .collect();
        let node = found
            .iter()
            .find(|n| span_lines(&n.span).is_some_and(|s| s.0 == label.line))
            .or_else(|| found.first());
        let Some(node) = node else {
            score
                .unmatched
                .push(format!("{}:{}", label.file, label.symbol));
            continue;
        };
        let wanted: Vec<String> = label.explained_by.iter().map(|h| normalize(h)).collect();
        for l in links_for(refs, node, true)
            .into_iter()
            .filter(|l| l.node.is_some() && levels.contains(&l.level))
        {
            score.emitted += 1;
            if wanted.contains(&normalize(&refs.entries[l.entry].heading)) {
                score.correct += 1;
            } else {
                score.false_links.push(format!(
                    "{} -> {} ({})",
                    label.symbol, refs.entries[l.entry].heading, l.via
                ));
            }
        }
    }
    Ok(score)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use sieve_core::wiring::{Meta, Origin, SummaryState};

    fn node(path: &str, name: &str, kind: Kind, span: &str) -> Node {
        Node {
            id: if kind == Kind::File {
                path.to_string()
            } else {
                format!("{path}#{name}")
            },
            name: name.to_string(),
            kind,
            path: path.to_string(),
            span: span.to_string(),
            signature: None,
            exported: false,
            origin: Origin::Ast,
            body_hash: String::new(),
            chars: None,
            body_text: None,
            summary_state: SummaryState::Pending,
            summary: None,
            crux: None,
            owner: None,
            arity: None,
            variadic: None,
        }
    }

    fn graph(nodes: Vec<Node>) -> Graph {
        Graph {
            meta: Meta {
                version: 1,
                node_count: nodes.len(),
                edge_count: 0,
                languages: Vec::new(),
                scopes: Vec::new(),
            },
            nodes,
            edges: Vec::new(),
        }
    }

    /// A scratch dir that removes itself on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            let dir = std::env::temp_dir()
                .join(format!("sieve-why-{label}-{}-{nanos}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("mkdir");
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn test_the_stamp_has_a_link_file_part_only_when_the_link_file_exists() {
        let s = Scratch::new("stamp-link");
        let ctx = s.0.join("sieve");
        std::fs::write(s.0.join("a.md"), "x").expect("write");
        let docs = vec!["a.md".to_string()];
        // The stamp of a repo with no link file is the one from before the
        // link feature: the docs, then the wiring.
        let before = format!(
            "a.md:{}|wiring:{}",
            stat_part(&s.0.join("a.md")),
            stat_part(&ctx.join(".graph").join("wiring.json"))
        );
        assert_eq!(current_stamp(&s.0, &ctx, &docs), before);
        std::fs::write(s.0.join(".sieve-links.json"), "{\"version\":1}\n").expect("write");
        let with = current_stamp(&s.0, &ctx, &docs);
        assert_ne!(with, before);
        assert!(
            with.contains("|.sieve-links.json:") && with.contains("|wiring:"),
            "{with}"
        );
        std::fs::remove_file(s.0.join(".sieve-links.json")).expect("remove");
        assert_eq!(current_stamp(&s.0, &ctx, &docs), before);
    }

    const LIB: &str = "crates/a/src/lib.rs";

    const LEDGER: &str = "# Ledger\n\n## Template\n\nnothing\n\n\
        ## 2026-09-01, first rule\n\n- **Date:** 2026-09-01\n- **Code:** `crates/a/src/lib.rs#walk`\n- **Decision:** The old way.\n\n\
        ## 2026-10-01, build takes a lock\n\n- **Date:** 2026-10-01\n- **Code:** `crates/a/src/lib.rs#build_repo`, `crates/a/src/main.rs`, `crates/a/src/gone.rs#old`\n- **Decision:** The build waits.\n\n\
        ## 2026-10-02, second rule\n\n- **Date:** 2026-10-02\n- **Decision:** This reverses the entry \"2026-09-01 \u{2014} first rule\".\n\n\
        ## 2026-10-03, prose rule\n\n- **Date:** 2026-10-03\n- **Decision:** The `plain` function, in crates/a/src/lib.rs, says \"a graph rebuild is already in flight\" to the user.\n";

    fn fixture_graph() -> Graph {
        graph(vec![
            node(LIB, "lib.rs", Kind::File, "L1-L5"),
            node(LIB, "build_repo", Kind::Function, "L1-L2"),
            node(LIB, "plain", Kind::Function, "L3-L3"),
            node(LIB, "walk", Kind::Function, "L4-L5"),
            node("crates/a/src/main.rs", "main.rs", Kind::File, "L1-L2"),
        ])
    }

    fn fixture() -> (Graph, WhyRefs) {
        let g = fixture_graph();
        let docs = vec![("docs/decisions/LEDGER.md".to_string(), LEDGER.to_string())];
        let refs = link_docs(Path::new("/nonexistent"), &g, &docs, "s".into());
        (g, refs)
    }

    fn linked(refs: &WhyRefs, name: &str, heading: &str) -> bool {
        refs.links
            .iter()
            .any(|l| l.name == name && refs.entries[l.entry].heading.contains(heading))
    }

    #[test]
    fn test_f6_a_decision_that_quotes_the_anchor_form_is_no_anchor() {
        let text = "## 2026-10-05, quote\n\n- **Date:** 2026-10-05\n- **Decision:** F6 links. An entry holds one line, `- **Code:** `path#scope.name`, ...`. Each item is a graph node id.\n";
        assert!(code_items(text).is_empty());
        let docs = vec![("docs/decisions/LEDGER.md".to_string(), text.to_string())];
        let refs = link_docs(
            Path::new("/nonexistent"),
            &fixture_graph(),
            &docs,
            "s".into(),
        );
        assert!(refs.stale.is_empty());
    }

    #[test]
    fn test_f6_an_anchor_line_with_two_ids_links_both() {
        let text = "## 2026-10-05, two\n\n- **Date:** 2026-10-05\n* **Code:** `crates/a/src/lib.rs#walk`, `crates/a/src/lib.rs#plain`\n";
        assert_eq!(code_items(text).len(), 2);
        let docs = vec![("docs/decisions/LEDGER.md".to_string(), text.to_string())];
        let refs = link_docs(
            Path::new("/nonexistent"),
            &fixture_graph(),
            &docs,
            "s".into(),
        );
        assert!(linked(&refs, "walk", "two"));
        assert!(linked(&refs, "plain", "two"));
    }

    #[test]
    fn test_f6_a_span_with_spaces_on_an_anchor_line_is_skipped() {
        let text = "- **Code:** `crates/a/src/lib.rs#walk`, `not an id`, `word`\n";
        assert_eq!(code_items(text), vec!["crates/a/src/lib.rs#walk"]);
    }

    #[test]
    fn test_f6_entries_skip_the_undated_template() {
        let (_g, refs) = fixture();
        assert_eq!(refs.entries.len(), 4);
    }

    #[test]
    fn test_f6_anchor_links_the_exact_node() {
        let (_g, refs) = fixture();
        assert!(linked(&refs, "build_repo", "build takes a lock"));
        assert!(linked(&refs, "walk", "first rule"));
        let a = refs.links.iter().find(|l| l.name == "walk").expect("link");
        assert_eq!(
            (a.level, a.node.as_deref()),
            (0, Some("crates/a/src/lib.rs#walk"))
        );
    }

    #[test]
    fn test_f6_anchor_to_a_file_links_the_file_node() {
        let (_g, refs) = fixture();
        assert!(refs
            .links
            .iter()
            .any(|l| l.node.is_none() && l.level == 0 && l.path == "crates/a/src/main.rs"));
    }

    #[test]
    fn test_f6_check_reports_a_stale_anchor() {
        let (_g, refs) = fixture();
        assert_eq!(refs.stale.len(), 1);
        assert_eq!(refs.stale[0].id, "crates/a/src/gone.rs#old");
        let out = render_check(&refs, true);
        assert!(out.contains("STALE ANCHOR"), "{out}");
        assert!(out.contains("crates/a/src/gone.rs#old"), "{out}");
    }

    #[test]
    fn test_f6_prose_names_give_no_default_link() {
        let (g, refs) = fixture();
        // The prose entry names `plain`, the file and a literal of the code.
        assert!(!linked(&refs, "plain", "prose rule"));
        let plain = g.nodes.iter().find(|n| n.name == "plain").expect("node");
        let shown = links_for(&refs, plain, false);
        assert!(shown
            .iter()
            .all(|l| !refs.entries[l.entry].heading.contains("prose")));
        // The path link exists and shows only with --all.
        let all = links_for(&refs, plain, true);
        assert!(all
            .iter()
            .any(|l| refs.entries[l.entry].heading.contains("prose") && l.level == 2));
    }

    #[test]
    fn test_why_file_lists_inner_anchors_and_path_mentions_once_each() {
        let (g, refs) = fixture();
        let file = g.nodes.iter().find(|n| n.name == "lib.rs").expect("file");
        let shown = links_for(&refs, file, false);
        let heads: Vec<&str> = shown
            .iter()
            .map(|l| refs.entries[l.entry].heading.as_str())
            .collect();
        // Anchors first (newest first), then the path mention. No repeat.
        assert_eq!(
            heads,
            [
                "2026-10-01, build takes a lock",
                "2026-09-01, first rule",
                "2026-10-03, prose rule"
            ]
        );
        let text = render_text(
            &refs,
            "crates/a/src/lib.rs",
            &[file],
            false,
            None,
            links_for,
        );
        assert!(text.contains("link: Code anchor (build_repo)"), "{text}");
        assert!(
            text.contains("file-level  docs/decisions/LEDGER.md"),
            "{text}"
        );
    }

    #[test]
    fn test_why_line_walks_up_to_the_enclosing_node_then_the_file() {
        let (g, refs) = fixture();
        // Line 1 is in `build_repo`, which has a record: no note.
        let (n, note) = walk_up(&refs, &g, LIB, 1, false);
        assert_eq!(
            (n.map(|n| n.name.as_str()), note.as_str()),
            (Some("build_repo"), "")
        );
        // `plain` has no record and no file anchor: the path mention of the
        // prose entry stays out, so the innermost node answers with no row.
        let (n, note) = walk_up(&refs, &g, LIB, 3, false);
        assert_eq!(
            (n.map(|n| n.name.as_str()), note.as_str()),
            (Some("plain"), "")
        );
        // Five file anchors: the file answers with the newest 3 and says so.
        let mut doc = String::new();
        for d in 1..=5 {
            doc.push_str(&format!(
                "## 2026-10-0{d}, rule {d}\n\n- **Date:** 2026-10-0{d}\n- **Code:** `{LIB}`\n\n"
            ));
        }
        let docs = vec![("docs/decisions/LEDGER.md".to_string(), doc)];
        let refs = link_docs(Path::new("/nonexistent"), &g, &docs, "s".into());
        let (n, note) = walk_up(&refs, &g, LIB, 3, false);
        let file = n.expect("file");
        assert_eq!(file.name, "lib.rs");
        assert_eq!(
            note,
            "no record at plain: the record links at the file (file-level anchors only, at most 3)\n"
        );
        let text = render_text(&refs, LIB, &[file], false, None, line_links);
        assert!(text.contains("rule 5") && text.contains("rule 3"), "{text}");
        assert!(!text.contains("rule 2"), "{text}");
        assert!(text.contains("link: Code anchor  [2026-10-05]"), "{text}");
    }

    #[test]
    fn test_why_entries_split_an_error_log_at_its_own_heading_level() {
        let errors = "# What did not work\n\n## Template\n\n## [Task]\n\n\
            ## [Audit of 2026-01-01 data, 2026-09-12]\n**What did not work:** a\n\n\
            ## [Second, 2026-09-13]\nbody\n";
        let heads = |text| {
            parse_entries("E.md", text)
                .into_iter()
                .map(|e| (e.heading, e.date))
                .collect::<Vec<_>>()
        };
        // The date in the middle of a title is no date.
        assert_eq!(
            heads(errors),
            [
                (
                    "[Audit of 2026-01-01 data, 2026-09-12]".into(),
                    "2026-09-12".into()
                ),
                ("[Second, 2026-09-13]".into(), "2026-09-13".into()),
            ]
        );
        let deep = "# Log\n\n### First thing\n- **Date:** 2026-03-01\nx\n\n### Second thing\n- **Date:** 2026-03-02\ny\n";
        let got = heads(deep);
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[1], ("Second thing".into(), "2026-03-02".into()));
        // A log with no date gives no record, not one record for the file.
        assert!(heads("# Log\n\n## one\n\n## two\n").is_empty());
    }

    #[test]
    fn test_why_a_file_shows_an_entry_with_an_anchor_and_a_path_link_once() {
        let g = fixture_graph();
        let text = format!(
            "## 2026-10-05, both\n\n- **Date:** 2026-10-05\n- **Code:** `{LIB}#walk`\n- The file {LIB} holds it.\n"
        );
        let docs = vec![("docs/decisions/LEDGER.md".to_string(), text)];
        let refs = link_docs(Path::new("/nonexistent"), &g, &docs, "s".into());
        assert_eq!(refs.links.len(), 2, "{:?}", refs.links);
        let shown = file_links(&refs, LIB, false);
        assert_eq!(shown.len(), 1);
        assert_eq!((shown[0].level, shown[0].name.as_str()), (0, "walk"));
    }

    #[cfg(unix)]
    #[test]
    fn test_why_adr_dir_refuses_a_link_that_leaves_the_repo() {
        let entry = "# Rec\n\n## 2026-10-01, a\n\nbody\n";
        let outside = repo("adrout", &[("0001.md", entry)]);
        let s = repo(
            "adrlink",
            &[(".adr-dir", "\n  records\n"), ("a/b.md", entry)],
        );
        std::os::unix::fs::symlink(&outside.0, s.0.join("records")).expect("link");
        assert!(find_docs(&s.0).is_empty());
        std::fs::remove_file(s.0.join("records")).expect("rm");
        std::fs::create_dir(s.0.join("records")).expect("mkdir");
        assert_eq!(adr_dir_setting(&s.0).as_deref(), Some("records"));
    }

    #[test]
    fn test_why_find_docs_reads_error_logs_and_the_adr_dir_setting() {
        let entry = "# Log\n\n## 2026-10-01, a\n\nbody\n";
        let s = repo(
            "finddocs",
            &[
                ("docs/errors/ERRORS.md", entry),
                ("ERRORS.md", entry),
                ("doc/adr/0001-x.md", entry),
                ("records/0002-y.md", entry),
                (".adr-dir", "records\n"),
                ("src/other.md", entry),
            ],
        );
        assert_eq!(
            find_docs(&s.0),
            [
                "ERRORS.md",
                "doc/adr/0001-x.md",
                "docs/errors/ERRORS.md",
                "records/0002-y.md"
            ]
        );
        std::fs::write(s.0.join(".adr-dir"), "../outside").expect("write");
        assert!(!find_docs(&s.0).contains(&"records/0002-y.md".to_string()));
    }

    #[test]
    fn test_f6_supersede_catches_the_reverse_with_a_dash_heading() {
        let (_g, refs) = fixture();
        assert!(refs.entries[0].superseded);
        assert_eq!(
            refs.entries[0].successor.as_deref(),
            Some("2026-10-02, second rule")
        );
        assert!(!refs.entries[1].superseded);
    }

    #[test]
    fn test_f6_supersede_ignores_a_keeps_sentence() {
        let keeps = "## 2026-10-04, third\n\n- **Date:** 2026-10-04\n- This keeps the entry \"2026-10-01, build takes a lock\" as it is.\n";
        let docs = vec![
            ("d.md".to_string(), LEDGER.to_string()),
            ("e.md".to_string(), keeps.to_string()),
        ];
        let refs = link_docs(
            Path::new("/nonexistent"),
            &fixture_graph(),
            &docs,
            "s".into(),
        );
        assert!(!refs
            .entries
            .iter()
            .any(|e| e.heading.contains("build takes") && e.superseded));
    }

    #[test]
    fn test_f6_supersede_reads_a_supersedes_line() {
        let a = "## 2026-10-01, old rule\n\n- **Date:** 2026-10-01\n- Text.\n";
        let b = "## 2026-10-05, new rule\n\n- **Date:** 2026-10-05\n- **Supersedes:** 2026-10-01, old rule\n";
        let c = "## 2026-10-06, other rule\n\n- **Date:** 2026-10-06\n- **Supersedes:** no such heading\n```\n- **Supersedes:** new rule\n```\n";
        let docs = vec![("d.md".to_string(), format!("{a}\n{b}\n{c}"))];
        let refs = link_docs(
            Path::new("/nonexistent"),
            &fixture_graph(),
            &docs,
            "s".into(),
        );
        let old = &refs.entries[0];
        assert!(old.superseded);
        assert_eq!(old.successor.as_deref(), Some("2026-10-05, new rule"));
        assert!(!refs.entries[1].superseded);
        assert!(!refs.entries[2].superseded);
    }

    #[test]
    fn test_f6_supersede_reads_an_adr_status_line() {
        let adr = "# Use X\n\nStatus: Superseded by ADR-9\n\nDate: 2026-01-02\n";
        let raw = parse_entries("adr/ADR-1.md", adr);
        assert_eq!(raw.len(), 1);
        assert!(raw[0].status_superseded);
        assert_eq!(raw[0].date, "2026-01-02");
    }

    #[test]
    fn test_f6_check_lists_a_superseded_entry_that_still_links() {
        let (_g, refs) = fixture();
        let out = render_check(&refs, true);
        assert!(out.contains("first rule"), "{out}");
        assert!(
            out.contains("1 superseded entries still link to code"),
            "{out}"
        );
    }

    #[test]
    fn test_f6_why_text_shows_the_decision_and_the_flag() {
        let (g, refs) = fixture();
        let n = g
            .nodes
            .iter()
            .find(|n| n.name == "build_repo")
            .expect("node");
        let out = render_text(&refs, "build_repo", &[n], false, None, links_for);
        assert!(out.starts_with("build_repo  fn  crates/a/src/lib.rs:1"));
        assert!(out.contains("build takes a lock"), "{out}");
        let walk = g.nodes.iter().find(|n| n.name == "walk").expect("node");
        assert!(
            render_text(&refs, "walk", &[walk], false, None, links_for).contains("SUPERSEDED by")
        );
    }

    #[test]
    fn test_f6_hint_is_short_and_uses_a_recorded_link_only() {
        let (_g, refs) = fixture();
        let hint = hint_from(&refs, LIB, Some((1, 2))).expect("hint");
        assert!(hint.starts_with("sieve: build_repo has a decision"));
        assert!(hint.chars().count() <= HINT_MAX_CHARS);
        // `plain` has only a path link.
        assert_eq!(hint_from(&refs, LIB, Some((3, 3))), None);
        assert_eq!(hint_from(&refs, "crates/a/src/main.rs", None), None);
    }

    /// The topic row of the entry whose heading holds `heading`.
    fn topic_row(refs: &WhyRefs, heading: &str) -> TopicRow {
        let e = refs
            .entries
            .iter()
            .find(|e| e.heading.contains(heading))
            .expect("entry");
        TopicRow {
            doc: e.doc.clone(),
            line: e.line,
            heading: e.heading.clone(),
            date: e.date.clone(),
        }
    }

    fn topics_of(path: &str, exact: bool, rows: Vec<TopicRow>) -> TopicRefs {
        TopicRefs {
            version: 1,
            stamp: "s".to_string(),
            files: vec![crate::why_infer::TopicFile {
                path: path.to_string(),
                exact,
                rows,
            }],
        }
    }

    fn ctx_with<'a>(g: &'a Graph, topics: &'a TopicRefs, line_path: Option<&'a str>) -> Ctx<'a> {
        Ctx {
            root: Path::new("/nonexistent"),
            graph: g,
            history: false,
            topics: Some(topics),
            line_path,
        }
    }

    #[test]
    fn test_inferred_rows_follow_the_exact_rows_of_a_file_and_never_repeat_one() {
        let (g, refs) = fixture();
        let second = topic_row(&refs, "second rule");
        let lock = topic_row(&refs, "build takes a lock");
        // `lock` is an exact row of the file, so it must not print twice.
        let topics = topics_of(LIB, true, vec![second.clone(), lock]);
        let ctx = ctx_with(&g, &topics, None);
        let file = g
            .nodes
            .iter()
            .find(|n| n.kind == Kind::File && n.path == LIB)
            .expect("file");
        let plain = render_text(&refs, LIB, &[file], false, None, links_for);
        let with = render_text(&refs, LIB, &[file], false, Some(&ctx), links_for);
        assert_eq!(with.matches("inferred  ").count(), 1, "{with}");
        assert!(
            with.contains("inferred from shared terms (not a recorded link)\n"),
            "{with}"
        );
        // The exact rows keep their text and their order.
        assert_eq!(with.replace(&inferred_text(&second), ""), plain);
        assert!(with.starts_with(&plain), "{with}");
    }

    #[test]
    fn test_inferred_rows_show_for_a_path_line_query_and_json_names_the_level() {
        let (g, refs) = fixture();
        let topics = topics_of(LIB, false, vec![topic_row(&refs, "second rule")]);
        let ctx = ctx_with(&g, &topics, Some(LIB));
        let plain = g.nodes.iter().find(|n| n.name == "plain").expect("node");
        // The line answers with `plain`, a symbol with no recorded decision.
        let text = render_text(&refs, "q", &[plain], false, Some(&ctx), line_links);
        assert!(text.contains("no recorded link for plain"), "{text}");
        assert!(!text.contains("no decision found"), "{text}");
        assert!(
            text.contains("inferred  docs/decisions/LEDGER.md:"),
            "{text}"
        );
        // Two answering nodes print the file rows once.
        let twice = render_text(&refs, "q", &[plain, plain], false, Some(&ctx), line_links);
        assert_eq!(twice.matches("inferred  ").count(), 1, "{twice}");
        let v2 = render_json(&refs, "q", &[plain, plain], false, Some(&ctx), line_links);
        assert_eq!(v2["symbols"][1]["total"], 0);
        // A symbol query for a symbol has no file, so it gets no inferred row.
        let sym_ctx = ctx_with(&g, &topics, None);
        let text = render_text(&refs, "q", &[plain], false, Some(&sym_ctx), links_for);
        assert!(!text.contains("inferred"), "{text}");
        let v = render_json(&refs, "q", &[plain], false, Some(&ctx), line_links);
        let rows = &v["symbols"][0]["decisions"];
        assert_eq!(rows[0]["level"], "inferred");
        assert_eq!(v["symbols"][0]["total"], 1);
    }

    #[test]
    fn test_inferred_hint_is_one_short_line_and_only_for_a_file_with_no_exact_row() {
        let (_g, refs) = fixture();
        let row = topic_row(&refs, "second rule");
        let none = topics_of(LIB, false, vec![row.clone()]);
        let hint = inferred_hint(&none, LIB).expect("hint");
        assert!(
            hint.starts_with("sieve: lib.rs has an inferred record (docs/decisions/LEDGER.md:"),
            "{hint}"
        );
        assert!(hint.ends_with(&format!(". Run sieve why {LIB}.")), "{hint}");
        assert!(!hint.contains('\n') && hint.chars().count() <= HINT_MAX_CHARS);
        // A file with an exact row keeps its own hint rules.
        assert_eq!(
            inferred_hint(&topics_of(LIB, true, vec![row.clone()]), LIB),
            None
        );
        assert_eq!(inferred_hint(&none, "crates/a/src/main.rs"), None);
        // A line over the limit gives no hint.
        let long = TopicRow {
            doc: "d".repeat(200),
            ..row
        };
        assert_eq!(inferred_hint(&topics_of(LIB, false, vec![long]), LIB), None);
    }

    #[test]
    fn test_f6_reverse_query_lists_the_code_of_an_entry() {
        let (_g, refs) = fixture();
        let line = refs.entries[1].line + 1;
        let out = render_reverse(&refs, "docs/decisions/LEDGER.md", line).expect("entry");
        assert!(out.contains("code  build_repo"), "{out}");
    }

    #[test]
    fn test_f6_score_counts_correct_and_false_links() {
        let (g, refs) = fixture();
        let labels = r#"[
          {"file":"crates/a/src/lib.rs","symbol":"build_repo","line":1,
           "explained_by":["2026-10-01 — build takes a lock"]},
          {"file":"crates/a/src/lib.rs","symbol":"walk","line":4,"explained_by":[]},
          {"file":"crates/a/src/lib.rs","symbol":"plain","line":3,"explained_by":[]}]"#;
        let s = score_labels(labels, &g, &refs, &[0]).expect("score");
        assert_eq!((s.correct, s.emitted, s.labelled), (1, 2, 1));
        assert!(s.false_links[0].starts_with("walk ->"));
    }

    #[test]
    fn test_f6_parse_hunks_reads_new_side_ranges() {
        let diff = "diff --git a/x b/x\n--- a/x\n+++ b/src/x.rs\n@@ -1,2 +3,4 @@\n@@ -9 +20 @@\n@@ -5,3 +7,0 @@\n";
        assert_eq!(
            parse_hunks(diff),
            vec![("src/x.rs".to_string(), vec![(3, 6), (20, 20)])]
        );
    }

    #[test]
    fn test_f6_parse_hunks_ignores_an_added_line_that_starts_with_plus_plus() {
        let diff =
            "diff --git a/x b/x\n--- a/x\n+++ b/src/x.rs\t\n@@ -1 +1 @@\n+++ x\n@@ -9 +20 @@\n+y\n\
            diff --cc m\n@@@ -1 -1 +1 @@@\n+++ z\n";
        assert_eq!(
            parse_hunks(diff),
            vec![("src/x.rs".to_string(), vec![(1, 1), (20, 20)])]
        );
    }

    #[test]
    fn test_f6_suggest_reads_a_path_with_a_space_and_a_non_ascii_name() {
        let names = ["src/my file.rs", "src/\u{e9}.rs"];
        let s = repo(
            "quote",
            &[(names[0], "fn a() {}\n"), (names[1], "fn a() {}\n")],
        );
        commit(&s.0, "one");
        for n in names {
            std::fs::write(s.0.join(n), "fn a() { 1; }\n").expect("write");
        }
        let mut nodes = Vec::new();
        for n in names {
            nodes.push(node(n, n, Kind::File, "L1-L1"));
            nodes.push(node(n, "a", Kind::Function, "L1-L1"));
        }
        let ids = suggest_ids(&s.0, &graph(nodes), None).expect("ids");
        assert_eq!(
            ids,
            vec![
                "src/my file.rs#a".to_string(),
                "src/\u{e9}.rs#a".to_string()
            ]
        );
    }

    #[test]
    fn test_f6_check_reports_a_sha_that_git_cannot_read() {
        let doc = "## 2026-10-01, a rule\n\n- **Date:** 2026-10-01\n- Done in commit abc1234.\n";
        let docs = vec![("d.md".to_string(), doc.to_string())];
        let refs = link_docs(
            Path::new("/nonexistent"),
            &fixture_graph(),
            &docs,
            "s".into(),
        );
        assert_eq!(refs.missing_shas.len(), 1);
        assert!(render_check(&refs, true).contains("sha not found: abc1234"));
    }

    /// A temp repo with fixed dates. Gives the dir. `files` are `(path, text)`.
    fn repo(label: &str, files: &[(&str, &str)]) -> Scratch {
        let s = Scratch::new(label);
        for (p, t) in files {
            let f = s.0.join(p);
            std::fs::create_dir_all(f.parent().expect("parent")).expect("mkdir");
            std::fs::write(f, t).expect("write");
        }
        run_in(&s.0, &["init", "-q"]);
        s
    }

    fn run_in(dir: &Path, args: &[&str]) -> String {
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
            .expect("run");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Commits and gives the short sha. The linker reads a sha with a digit
    /// and a letter, so the helper amends the message until the sha has both.
    fn commit(dir: &Path, msg: &str) -> String {
        run_in(dir, &["add", "."]);
        run_in(dir, &["commit", "-q", "-m", msg]);
        for n in 0..40 {
            let sha = run_in(dir, &["rev-parse", "--short=7", "HEAD"]);
            if sha.bytes().any(|b| b.is_ascii_digit())
                && sha.bytes().any(|b| b.is_ascii_lowercase())
            {
                return sha;
            }
            run_in(
                dir,
                &["commit", "-q", "--amend", "-m", &format!("{msg} {n}")],
            );
        }
        String::new()
    }

    fn lines_graph(n: usize) -> Graph {
        let mut nodes = vec![node(
            "src/lib.rs",
            "lib.rs",
            Kind::File,
            &format!("L1-L{n}"),
        )];
        for k in 1..=n {
            nodes.push(node(
                "src/lib.rs",
                &format!("f{k}"),
                Kind::Function,
                &format!("L{k}-L{k}"),
            ));
        }
        nodes.push(node("tests/t.rs", "t", Kind::Function, "L1-L1"));
        graph(nodes)
    }

    fn lib_text(n: usize, tag: &str) -> String {
        (1..=n)
            .map(|k| format!("fn f{k}() {{ /*{tag}*/ }}\n"))
            .collect()
    }

    fn doc_for(sha: &str) -> Vec<(String, String)> {
        let text =
            format!("## 2026-10-01, a rule\n\n- **Date:** 2026-10-01\n- Done in commit {sha}.\n");
        vec![("docs/decisions/LEDGER.md".to_string(), text)]
    }

    #[test]
    fn test_f6_l0_links_only_the_nodes_that_the_commit_hunks_touch() {
        let s = repo(
            "l0",
            &[
                ("src/lib.rs", &lib_text(5, "a")),
                ("tests/t.rs", "fn t() {}\n"),
            ],
        );
        commit(&s.0, "one");
        let mut text = lib_text(5, "a");
        text = text.replace("f2() { /*a*/ }", "f2() { /*b*/ }");
        std::fs::write(s.0.join("src/lib.rs"), text).expect("write");
        std::fs::write(s.0.join("tests/t.rs"), "fn t() { 1; }\n").expect("write");
        let sha = commit(&s.0, "two");
        let refs = link_docs(&s.0, &lines_graph(5), &doc_for(&sha), "s".into());
        let names: Vec<&str> = refs.links.iter().map(|l| l.name.as_str()).collect();
        // The hunk touches f2 only. The test file is not a target.
        assert_eq!(names, vec!["f2"]);
        assert_eq!(refs.links[0].level, 1);
        // A commit link is inferred: it shows only with --all, never in the
        // default output, the hint or the superseded report.
        let g = lines_graph(5);
        let f2 = g.nodes.iter().find(|n| n.name == "f2").expect("node");
        assert!(links_for(&refs, f2, false).is_empty());
        let all = links_for(&refs, f2, true);
        assert_eq!(all.len(), 1);
        assert!(all[0].via.starts_with("commit mention (inferred)"));
        assert_eq!(hint_from(&refs, "src/lib.rs", None), None);
    }

    #[test]
    fn test_f6_l0_keeps_file_links_only_for_a_commit_with_over_20_nodes() {
        let s = repo("cap", &[("src/lib.rs", &lib_text(25, "a"))]);
        commit(&s.0, "one");
        std::fs::write(s.0.join("src/lib.rs"), lib_text(25, "b")).expect("write");
        let sha = commit(&s.0, "two");
        let refs = link_docs(&s.0, &lines_graph(25), &doc_for(&sha), "s".into());
        assert_eq!(refs.links.len(), 1);
        assert!(refs.links[0].node.is_none());
        assert_eq!(refs.links[0].path, "src/lib.rs");
    }

    #[test]
    fn test_f6_suggest_lists_changed_non_test_ids_for_the_diff_and_a_rev() {
        let s = repo(
            "sug",
            &[
                ("src/lib.rs", &lib_text(3, "a")),
                ("tests/t.rs", "fn t() {}\n"),
            ],
        );
        commit(&s.0, "one");
        std::fs::write(
            s.0.join("src/lib.rs"),
            lib_text(3, "a").replace("f3() { /*a*/ }", "f3() { /*c*/ }"),
        )
        .expect("write");
        std::fs::write(s.0.join("tests/t.rs"), "fn t() { 2; }\n").expect("write");
        let g = lines_graph(3);
        let ids = suggest_ids(&s.0, &g, None).expect("ids");
        assert_eq!(ids, vec!["src/lib.rs#f3".to_string()]);
        commit(&s.0, "two");
        let ids = suggest_ids(&s.0, &g, Some("HEAD")).expect("ids");
        assert_eq!(ids, vec!["src/lib.rs#f3".to_string()]);
        assert!(suggest_ids(&s.0, &g, Some("--output=x")).is_err());
    }

    #[test]
    fn test_f6_sidecar_round_trips_and_a_stale_stamp_is_dropped() {
        let (g, refs) = fixture();
        let s = Scratch::new("side");
        let ctx = s.0.join("ctx");
        write_sidecar(&ctx, &refs).expect("write");
        let back: WhyRefs =
            serde_json::from_slice(&std::fs::read(sidecar_path(&ctx)).expect("read"))
                .expect("json");
        assert_eq!(back, refs);
        // No docs on disk: the stamp differs, so the loader links again.
        let fresh = load_refs(&s.0, &ctx, || Some(g.clone())).expect("refs");
        assert!(fresh.entries.is_empty());
    }

    /// Scores the linker on this repo against the label file named by
    /// `SIEVE_WHY_LABELS`. `SIEVE_WHY_ROOT` names the repo root. The root
    /// defaults to the nearest ancestor of the cwd with `docs/decisions`.
    /// Run: `SIEVE_WHY_LABELS=<file> cargo test -p sieve-cli
    /// test_f6_score_repo_labels -- --nocapture`. With no variable set, the
    /// test does nothing.
    #[test]
    fn test_f6_score_repo_labels() {
        let Ok(labels) = std::env::var("SIEVE_WHY_LABELS") else {
            return;
        };
        let root = std::env::var("SIEVE_WHY_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let mut dir = std::env::current_dir().expect("cwd");
                while !dir.join("docs/decisions").is_dir() && dir.pop() {}
                dir
            });
        let scratch = Scratch::new("score");
        let graph = sieve_parse::build_graph(&root, &scratch.0).expect("graph");
        let docs = find_docs(&root);
        let refs = link_docs(&root, &graph, &read_docs(&root, &docs), String::new());
        let text = std::fs::read_to_string(&labels).expect("labels");
        // The default output shows anchors only: the "F6 SCORE" line is
        // that score. The "F6 SOURCE" lines give each link source.
        let sources: [(&str, &[u8]); 3] = [("anchor", &[0]), ("l0", &[1]), ("both", &[0, 1])];
        let mut first = true;
        for (name, levels) in sources {
            let s = score_labels(&text, &graph, &refs, levels).expect("score");
            let line = format!(
                "precision={:.3} ({}/{}) recall={:.3} ({}/{})",
                s.precision(),
                s.correct,
                s.emitted,
                s.recall(),
                s.correct,
                s.labelled
            );
            if first {
                println!("F6 SCORE {line}");
                first = false;
            }
            println!("F6 SOURCE {name} {line}");
            for f in &s.false_links {
                println!("F6 FALSE LINK [{name}] {f}");
            }
        }
        let s = score_labels(&text, &graph, &refs, &[0]).expect("score");
        for u in &s.unmatched {
            println!("F6 UNMATCHED LABEL {u}");
        }
    }

    /// Prints the `sieve why` answer for the symbol named by
    /// `SIEVE_WHY_SYMBOL`, on this repo. The graph goes to a scratch dir. With
    /// no variable set, the test does nothing.
    #[test]
    fn test_f6_print_repo_answer() {
        let Ok(symbol) = std::env::var("SIEVE_WHY_SYMBOL") else {
            return;
        };
        let mut root = std::env::current_dir().expect("cwd");
        while !root.join("docs/decisions").is_dir() && root.pop() {}
        let scratch = Scratch::new("answer");
        let graph = sieve_parse::build_graph(&root, &scratch.0).expect("graph");
        let docs = find_docs(&root);
        let refs = link_docs(&root, &graph, &read_docs(&root, &docs), String::new());
        let out = query_text(&refs, &graph, &root, &symbol, false, false, None).expect("answer");
        println!(
            "F6 ANSWER BEGIN\n{out}F6 ANSWER END chars={}",
            out.chars().count()
        );
    }

    #[test]
    fn test_f6_git_output_over_8_mb_is_refused() {
        // `run_prog` puts `-c core.quotePath=false` before its arguments.
        // GNU `yes` rejects `-c`, so a script that ignores its arguments
        // runs `yes`. The output has no end and passes 8 MB.
        let dir = std::env::temp_dir().join(format!("sieve-yes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("make dir");
        let script = dir.join("endless");
        std::fs::write(&script, "#!/bin/sh\nexec yes\n").expect("write script");
        std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .expect("chmod");
        let got = run_prog(script.to_str().expect("utf8 path"), &dir, &[]);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(got, Err("output over 8 MB"));
        let dir = std::env::temp_dir();
        // The control: a small output is read.
        let ok = run_prog("echo", &dir, &["x"]).expect("echo");
        assert!(ok.ends_with(" x\n"), "{ok}");
        assert_eq!(
            run_prog("sieve-no-such-program", &dir, &[]),
            Err("git not found")
        );
    }

    #[test]
    fn test_f6_unquote_path_reads_a_tab_a_quote_and_octal() {
        assert_eq!(unquote_path("\"b/a\\tb.rs\""), "b/a\tb.rs");
        assert_eq!(unquote_path("\"b/q\\\"x\\\\y\""), "b/q\"x\\y");
        assert_eq!(unquote_path("\"b/\\303\\251.rs\""), "b/\u{e9}.rs");
        assert_eq!(unquote_path("b/plain.rs"), "b/plain.rs");
        let diff = "diff --git \"a/a\\tb.rs\" \"b/a\\tb.rs\"\n--- \"a/a\\tb.rs\"\n+++ \"b/a\\tb.rs\"\n@@ -1 +1 @@\n";
        assert_eq!(
            parse_hunks(diff),
            vec![("a\tb.rs".to_string(), vec![(1, 1)])]
        );
    }

    #[test]
    fn test_f6_suggest_reads_a_path_with_a_tab_and_a_quote() {
        let names = ["src/a\tb.rs", "src/q\"x.rs"];
        let s = repo(
            "tabquote",
            &[(names[0], "fn a() {}\n"), (names[1], "fn a() {}\n")],
        );
        commit(&s.0, "one");
        let mut nodes = Vec::new();
        for n in names {
            std::fs::write(s.0.join(n), "fn a() { 1; }\n").expect("write");
            nodes.push(node(n, n, Kind::File, "L1-L1"));
            nodes.push(node(n, "a", Kind::Function, "L1-L1"));
        }
        let ids = suggest_ids(&s.0, &graph(nodes), None).expect("ids");
        assert_eq!(
            ids,
            vec!["src/a\tb.rs#a".to_string(), "src/q\"x.rs#a".to_string()]
        );
    }

    #[test]
    fn test_f6_a_sha_candidate_has_a_digit_and_a_letter() {
        let text = "deadbeef abcdefabcdef 1234567 20261001 abc1234 ABC1234 v1.0.0 \
            1234567890123456789012345678901234567890a";
        assert_eq!(candidate_shas(text), vec!["abc1234".to_string()]);
    }

    #[test]
    fn test_f6_check_outside_a_repo_prints_one_line_and_no_sha_line() {
        let doc = "## 2026-10-01, a rule\n\n- **Date:** 2026-10-01\n- Done in commit abc1234 and 1234abc.\n";
        let docs = vec![("d.md".to_string(), doc.to_string())];
        let refs = link_docs(
            Path::new("/nonexistent"),
            &fixture_graph(),
            &docs,
            "s".into(),
        );
        assert_eq!(refs.missing_shas.len(), 2);
        let out = render_check(&refs, false);
        assert!(!out.contains("sha not found"), "{out}");
        assert_eq!(
            out.matches("not a git repo: commit checks skipped\n")
                .count(),
            1,
            "{out}"
        );
        assert!(render_check(&refs, true).contains("sha not found: abc1234"));
        assert!(!render_check(&refs, true).contains("not a git repo"));
    }

    /// A node with a `L<a>-L<b>` span.
    fn span_node(path: &str, name: &str, a: usize, b: usize) -> Node {
        node(path, name, Kind::Function, &format!("L{a}-L{b}"))
    }

    fn call_edge(from: &str, to: &str) -> sieve_core::wiring::Edge {
        sieve_core::wiring::Edge {
            source: from.to_string(),
            target: to.to_string(),
            relation: sieve_core::wiring::Relation::Calls,
            confidence: sieve_core::wiring::Confidence::Extracted,
        }
    }

    fn call_edge_with(
        from: &str,
        to: &str,
        c: sieve_core::wiring::Confidence,
    ) -> sieve_core::wiring::Edge {
        let mut e = call_edge(from, to);
        e.confidence = c;
        e
    }

    #[test]
    fn test_f6_pinning_tests_come_from_resolved_edges_to_test_code_only() {
        let src = "fn target() {}\n\
            fn use_it() { target(); }\n\
            fn test_by_name() { target(); }\n\
            #[cfg(test)]\n\
            mod checks {\n\
            fn helper() { target(); }\n\
            }\n\
            #[test]\n\
            fn marked() { target(); }\n";
        let s = repo(
            "pin",
            &[
                ("src/lib.rs", src),
                ("tests/t.rs", "fn in_test_file() {}\nfn guessed() {}\n"),
            ],
        );
        let mut m = node("src/lib.rs", "checks", Kind::Module, "L5-L7");
        m.id = "src/lib.rs#checks".to_string();
        let mut g = graph(vec![
            span_node("src/lib.rs", "target", 1, 1),
            span_node("src/lib.rs", "use_it", 2, 2),
            span_node("src/lib.rs", "test_by_name", 3, 3),
            m,
            span_node("src/lib.rs", "helper", 6, 6),
            span_node("src/lib.rs", "marked", 9, 9),
            span_node("tests/t.rs", "in_test_file", 1, 1),
            span_node("tests/t.rs", "guessed", 2, 2),
        ]);
        let inferred = sieve_core::wiring::Confidence::Inferred;
        g.edges = vec![
            call_edge("src/lib.rs#use_it", "src/lib.rs#target"),
            call_edge("src/lib.rs#test_by_name", "src/lib.rs#target"),
            call_edge("src/lib.rs#helper", "src/lib.rs#target"),
            call_edge("src/lib.rs#marked", "src/lib.rs#target"),
            call_edge("tests/t.rs#in_test_file", "src/lib.rs#target"),
            call_edge_with("tests/t.rs#guessed", "src/lib.rs#target", inferred),
        ];
        let target = g.nodes[0].clone();
        let is_test = sieve_query::blast::is_test_path;
        let rows = crate::why_rows::span_rows(&s.0, &g, &target, None, is_test).tests;
        // A name that starts with `test` in a source file is no test. An
        // inferred edge from a test file gives no row.
        assert_eq!(
            rows,
            vec![
                "test  src/lib.rs:6  helper".to_string(),
                "test  src/lib.rs:9  marked".to_string(),
                "test  tests/t.rs:1  in_test_file".to_string(),
            ]
        );
    }

    #[test]
    fn test_f6_default_output_caps_each_row_type_and_all_shows_every_row() {
        let mut src = String::from("fn busy() {\n");
        for k in 0..5 {
            src.push_str(&format!("    // because reason {k}\n    let a{k} = {k};\n"));
        }
        src.push_str("}\n");
        let s = repo("cap-rows", &[("src/lib.rs", &src)]);
        let g = graph(vec![span_node("src/lib.rs", "busy", 1, 12)]);
        let refs = WhyRefs {
            version: SIDECAR_VERSION,
            stamp: String::new(),
            entries: Vec::new(),
            links: Vec::new(),
            stale: Vec::new(),
            missing_shas: Vec::new(),
        };
        let ctx = Ctx {
            root: &s.0,
            graph: &g,
            history: false,
            topics: None,
            line_path: None,
        };
        let n = &g.nodes[0];
        let out = render_text(&refs, "busy", &[n], false, Some(&ctx), links_for);
        assert_eq!(out.matches("comment  ").count(), 3, "{out}");
        assert!(
            out.contains("more: 2 rows. sieve why busy --all\n"),
            "{out}"
        );
        let all = render_text(&refs, "busy", &[n], true, Some(&ctx), links_for);
        assert_eq!(all.matches("comment  ").count(), 5, "{all}");
        assert!(!all.contains("more:"), "{all}");
    }

    #[test]
    fn test_f6_history_rows_come_from_one_log_call_with_the_span() {
        let s = repo("hist", &[("src/lib.rs", "fn a() {\n}\n")]);
        commit(&s.0, "first subject");
        std::fs::write(s.0.join("src/lib.rs"), "fn a() {\n    1;\n}\n").expect("write");
        commit(&s.0, "second subject");
        let n = span_node("src/lib.rs", "a", 1, 3);
        let g = graph(vec![n.clone()]);
        let calls = std::cell::RefCell::new(Vec::<String>::new());
        let git = |a: &[&str]| {
            calls.borrow_mut().push(a[0].to_string());
            run_prog("git", &s.0, a)
        };
        let no_test = |_: &str| false;
        let rows = crate::why_rows::span_rows(&s.0, &g, &n, Some(&git), no_test).history;
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert!(rows[0].starts_with("history  introduced  "), "{rows:?}");
        assert!(rows[0].contains("first subject"), "{rows:?}");
        assert!(rows[0].contains("2026-01-01"), "{rows:?}");
        assert!(rows[1].starts_with("history  last  "), "{rows:?}");
        assert!(rows[1].contains("second subject"), "{rows:?}");
        assert_eq!(calls.borrow().iter().filter(|c| *c == "log").count(), 1);
        // A git failure shows the reason and no row.
        let failing = |_: &[&str]| Err("timeout");
        let rows = crate::why_rows::span_rows(&s.0, &g, &n, Some(&failing), no_test).history;
        assert_eq!(rows, vec!["history skipped: timeout".to_string()]);
    }

    #[test]
    fn test_f6_history_skips_a_file_with_uncommitted_changes_and_runs_no_log() {
        let s = repo("hist-dirty", &[("src/lib.rs", "fn a() {\n}\n")]);
        commit(&s.0, "first subject");
        std::fs::write(s.0.join("src/lib.rs"), "fn a() {\n    1;\n}\n").expect("write");
        let n = span_node("src/lib.rs", "a", 1, 2);
        let g = graph(vec![n.clone()]);
        let calls = std::cell::RefCell::new(Vec::<String>::new());
        let git = |a: &[&str]| {
            calls.borrow_mut().push(a[0].to_string());
            run_prog("git", &s.0, a)
        };
        let rows = crate::why_rows::span_rows(&s.0, &g, &n, Some(&git), |_: &str| false).history;
        assert_eq!(
            rows,
            vec!["history skipped: uncommitted changes".to_string()]
        );
        assert!(!calls.borrow().iter().any(|c| c == "log"), "{calls:?}");
    }

    #[test]
    fn test_f6_history_skips_a_span_past_the_end_of_the_committed_file() {
        let s = repo("hist-stale", &[("src/lib.rs", "fn a() {\n}\n")]);
        commit(&s.0, "first subject");
        let n = span_node("src/lib.rs", "a", 1, 9);
        let g = graph(vec![n.clone()]);
        let calls = std::cell::RefCell::new(Vec::<String>::new());
        let git = |a: &[&str]| {
            calls.borrow_mut().push(a[0].to_string());
            run_prog("git", &s.0, a)
        };
        let rows = crate::why_rows::span_rows(&s.0, &g, &n, Some(&git), |_: &str| false).history;
        assert_eq!(rows, vec!["history skipped: the span is stale".to_string()]);
        assert!(!calls.borrow().iter().any(|c| c == "log"), "{calls:?}");
    }

    fn code_doc(items: &str) -> Vec<(String, String)> {
        let text = format!(
            "## 2026-10-01, a rule\n\n- **Date:** 2026-10-01\n- **Code:** {items}\n- **Decision:** x.\n"
        );
        vec![("docs/decisions/LEDGER.md".to_string(), text)]
    }

    /// A repo with `n` functions, committed, then `f<k>` edited for each `k`.
    fn edited_repo(label: &str, n: usize, edit: &[usize]) -> Scratch {
        let s = repo(label, &[("src/lib.rs", &lib_text(n, "a"))]);
        commit(&s.0, "one");
        let mut text = lib_text(n, "a");
        for k in edit {
            text = text.replace(
                &format!("f{k}() {{ /*a*/ }}"),
                &format!("f{k}() {{ /*b*/ }}"),
            );
        }
        std::fs::write(s.0.join("src/lib.rs"), text).expect("write");
        s
    }

    fn report(s: &Scratch, g: &Graph, items: &str, rev: Option<&str>) -> String {
        let refs = link_docs(&s.0, g, &code_doc(items), "s".into());
        let diff = diff_text(&s.0, rev).expect("diff");
        diff_report(&refs, g, &diff, rev.unwrap_or("working tree"), false, false)
    }

    #[test]
    fn test_f6_diff_prints_the_decision_of_a_changed_anchored_symbol_only() {
        let s = edited_repo("diff-a", 3, &[1, 2]);
        let out = report(&s, &lines_graph(3), "`src/lib.rs#f1`", None);
        assert!(
            out.starts_with(
                "why --diff working tree  2 changed symbols  1 with decisions  0 stale anchors\n"
            ),
            "{out}"
        );
        assert!(out.contains("f1  fn  src/lib.rs:1"), "{out}");
        assert!(out.contains("decision  docs/decisions/LEDGER.md"), "{out}");
        assert!(!out.contains("f2  fn  "), "{out}");
        assert!(!out.contains("history"), "{out}");
    }

    #[test]
    fn test_f6_diff_counts_a_file_anchor_in_one_line_not_one_row_per_node() {
        let s = edited_repo("diff-file", 3, &[1, 2, 3]);
        let out = report(&s, &lines_graph(3), "`src/lib.rs`", None);
        assert!(out.contains("3 changed symbols  0 with decisions"), "{out}");
        assert!(
            out.contains("src/lib.rs: 1 file-level decisions (sieve why src/lib.rs --all)"),
            "{out}"
        );
        assert!(!out.contains("file-level  "), "{out}");
        assert!(!out.contains("  fn  src/lib.rs"), "{out}");
    }

    #[test]
    fn test_f6_diff_lists_the_anchor_of_a_function_removed_inside_a_file_as_stale() {
        let s = repo("diff-stale", &[("src/lib.rs", &lib_text(3, "a"))]);
        commit(&s.0, "one");
        std::fs::write(s.0.join("src/lib.rs"), lib_text(2, "a")).expect("write");
        let out = report(
            &s,
            &lines_graph(2),
            "`src/lib.rs#f3`, `src/other.rs#x`",
            None,
        );
        assert!(
            out.starts_with("why --diff working tree  no code symbol changed  1 stale anchor\n"),
            "{out}"
        );
        assert!(
            out.contains("stale anchor  src/lib.rs#f3  docs/decisions/LEDGER.md:"),
            "{out}"
        );
        assert!(!out.contains("other.rs"), "{out}");
    }

    #[test]
    fn test_f6_diff_reads_a_range_and_a_rev() {
        let s = edited_repo("diff-range", 3, &[2]);
        commit(&s.0, "two");
        std::fs::write(
            s.0.join("src/lib.rs"),
            lib_text(3, "a")
                .replace("f2() { /*a*/ }", "f2() { /*b*/ }")
                .replace("f3() { /*a*/ }", "f3() { /*b*/ }"),
        )
        .expect("write");
        commit(&s.0, "three");
        let g = lines_graph(3);
        let range = report(
            &s,
            &g,
            "`src/lib.rs#f2`, `src/lib.rs#f3`",
            Some("HEAD~2...HEAD~1"),
        );
        assert!(
            range.contains("1 changed symbols  1 with decisions"),
            "{range}"
        );
        assert!(
            range.contains("f2  fn  ") && !range.contains("f3  fn  "),
            "{range}"
        );
        let rev = report(&s, &g, "`src/lib.rs#f2`, `src/lib.rs#f3`", Some("HEAD"));
        assert!(
            rev.contains("f3  fn  ") && !rev.contains("f2  fn  "),
            "{rev}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_f6_diff_runs_one_diff_call_and_no_log() {
        // The child run has a fake git first on its own PATH. The parent
        // PATH stays as it is, so no other test sees the fake git.
        if let Ok(dir) = std::env::var("SIEVE_FAKE_GIT_DIR") {
            let sc = Scratch(PathBuf::from(dir));
            let out = report(&sc, &lines_graph(2), "`src/lib.rs#f1`", None);
            std::mem::forget(sc);
            assert!(out.contains("f1  fn  "), "{out}");
            return;
        }
        let s = edited_repo("diff-fake", 2, &[1]);
        let real = String::from_utf8_lossy(
            &Command::new("sh")
                .args(["-c", "command -v git"])
                .output()
                .expect("which")
                .stdout,
        )
        .trim()
        .to_string();
        let bin = Scratch::new("diff-bin");
        let log = bin.0.join("calls.log");
        let fake = bin.0.join("git");
        let script = format!(
            "#!/bin/sh\necho \"$*\" >> {}\nexec '{real}' \"$@\"\n",
            log.display()
        );
        std::fs::write(&fake, script).expect("write");
        let mut perm = std::fs::metadata(&fake).expect("meta").permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
        std::fs::set_permissions(&fake, perm).expect("chmod");
        let path = format!(
            "{}:{}",
            bin.0.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        // A first run of a new script can be slow. Warm it up once.
        let _ = Command::new(&fake).arg("--version").output();
        let exe = std::env::current_exe().expect("exe");
        let status = Command::new(exe)
            .args([
                "--exact",
                "why::tests::test_f6_diff_runs_one_diff_call_and_no_log",
            ])
            .env("PATH", path)
            .env("SIEVE_FAKE_GIT_DIR", &s.0)
            .status()
            .expect("child");
        assert!(status.success());
        let calls = std::fs::read_to_string(&log).unwrap_or_default();
        let calls: Vec<&str> = calls.lines().filter(|l| *l != "--version").collect();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert!(
            calls[0].contains(" diff ") && !calls[0].contains(" log "),
            "{calls:?}"
        );
    }

    #[test]
    fn test_f6_diff_paths_read_the_header_with_a_quote_a_binary_and_a_rename() {
        let diff = "diff --git \"a/q\\\"x.rs\" \"b/q\\\"x.rs\"\n--- \"a/q\\\"x.rs\"\n\
            diff --git a/img.png b/img.png\nBinary files a/img.png and b/img.png differ\n\
            diff --git a/m n.rs b/m n.rs\nold mode 100644\nnew mode 100755\n\
            diff --git a/old.rs b/new.rs\nrename from old.rs\nrename to new.rs\n";
        let mut got: Vec<String> = diff_paths(diff).into_iter().collect();
        got.sort();
        assert_eq!(got, ["img.png", "m n.rs", "new.rs", "old.rs", "q\"x.rs"]);
    }

    #[test]
    fn test_f6_diff_lists_the_anchor_of_a_binary_changed_file_as_stale() {
        let g = lines_graph(2);
        let refs = link_docs(
            Path::new("/nonexistent"),
            &g,
            &code_doc("`img.png`"),
            "s".into(),
        );
        let diff = "diff --git a/img.png b/img.png\nBinary files a/img.png and b/img.png differ\n";
        let out = diff_report(&refs, &g, diff, "HEAD", false, false);
        assert!(out.contains("1 stale anchor\n"), "{out}");
        assert!(out.contains("stale anchor  img.png"), "{out}");
    }

    #[test]
    fn test_f6_diff_rejects_a_rev_and_a_range_that_start_with_a_dash() {
        let none = Path::new("/nonexistent");
        for r in ["--output=x", "-x...HEAD"] {
            let e = diff_text(none, Some(r)).expect_err("rejected");
            assert!(e.starts_with("bad rev"), "{e}");
        }
    }

    #[test]
    fn test_f6_suggest_gives_no_id_for_a_pure_rename() {
        let s = repo("diff-mv", &[("src/old.rs", &lib_text(3, "a"))]);
        commit(&s.0, "one");
        std::fs::rename(s.0.join("src/old.rs"), s.0.join("src/lib.rs")).expect("mv");
        commit(&s.0, "two");
        let ids = suggest_ids(&s.0, &lines_graph(3), Some("HEAD")).expect("ids");
        assert!(ids.is_empty(), "{ids:?}");
    }

    #[test]
    fn test_f6_diff_json_and_all_give_the_full_lists() {
        let s = edited_repo("diff-json", 25, &(1..=25).collect::<Vec<_>>());
        let g = lines_graph(25);
        let items: Vec<String> = (1..=25).map(|k| format!("`src/lib.rs#f{k}`")).collect();
        let refs = link_docs(&s.0, &g, &code_doc(&items.join(", ")), "s".into());
        let diff = diff_text(&s.0, None).expect("diff");
        let text = diff_report(&refs, &g, &diff, "w", false, false);
        assert!(text.contains("more: "), "{text}");
        assert_eq!(text.lines().count(), 1 + DIFF_ROWS + 1, "{text}");
        let all = diff_report(&refs, &g, &diff, "w", true, false);
        assert!(
            !all.contains("more: ") && all.contains("f25  fn  "),
            "{all}"
        );
        let json = diff_report(&refs, &g, &diff, "w", false, true);
        let v: serde_json::Value = serde_json::from_str(&json).expect("json");
        assert_eq!(v["symbols"].as_array().map(Vec::len), Some(25), "{json}");
    }

    #[test]
    fn test_f6_suggest_and_diff_do_not_combine() {
        let args = WhyArgs {
            symbol: None,
            dir: None,
            check: false,
            json: false,
            all: false,
            suggest: true,
            diff: true,
            no_refresh: true,
            link: false,
            keep: None,
            expect: None,
            page: None,
        };
        let e = run(&args, None).expect_err("rejected");
        assert!(e.contains("do not combine"), "{e}");
    }
}
