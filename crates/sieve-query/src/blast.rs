//! `blast`: what a diff's changed lines reach, walked over the graph
//! (the `query-commands.md` note section 3).
//!
//! `blast` reads the diff through `git`, seeds the walk at the innermost
//! changed symbol per file, then groups the result into areas (the diff's
//! own side) and modules (the dependents' side). The walk here is its own
//! multi-seed BFS, not [`crate::callers::edge_walk`]: `blast` merges several
//! seeds into ONE walk per changed file, which `edge_walk` does not do
//! (it only auto-adds same-path symbols for a single file seed). This
//! duplicates `callers.rs`'s private `bfs` helper; a later pass should pull
//! a shared multi-seed BFS out of both modules.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Command;

use regex::Regex;
use serde::Serialize;
use sieve_core::collate::collate;
use sieve_core::{Graph, Kind, Node, Relation};
use thiserror::Error;

use crate::callers::{is_js_whitespace, js_trim, Depth};
use crate::relations::WALK_RELATIONS;

mod owners;
mod render;
mod viz;

pub use owners::{Owner, Reviewer, MAX_REVIEWERS};
pub use render::{format_blast_markdown, mermaid_diagram};
pub use viz::{blast_viz_graph, repo_label, strip_url_credentials};

// ---------------------------------------------------------------------
// Options and errors
// ---------------------------------------------------------------------

/// What `blast` was asked to do: which base to diff against, how far to
/// walk, and whether to attach owners (section 3.1).
#[derive(Debug, Clone)]
pub struct BlastOptions {
    /// `--base <ref>`. `None` diffs the working tree against `HEAD`.
    pub base: Option<String>,
    /// `--depth`. `Depth(None)` is the unbounded full closure.
    pub depth: Depth,
    /// `--no-owners` turns this off. On by default.
    pub owners: bool,
    /// `--pr-author <who...>`: names, emails or handles dropped from the
    /// reviewer list, on top of the diff's own authors.
    pub pr_author: Vec<String>,
}

/// Every way `blast` can fail (section 3.1 to 3.2).
#[derive(Debug, Error)]
pub enum BlastError {
    #[error("could not read a diff in {0} \u{2014} run it inside a git repository")]
    NotGit(String),
    #[error(
        "base ref {0} is not in this checkout \u{2014} in CI, fetch the full history (actions/checkout with fetch-depth: 0)"
    )]
    BadBase(String),
    #[error(
        "--format must be text, markdown, mermaid or json, got {0} \u{2014} try --format text"
    )]
    BadFormat(String),
    #[error("--depth must be a positive number or all, got {0} \u{2014} try --depth 2")]
    BadDepth(String),
    /// Raised by the caller that loads the graph, not by [`blast`] itself —
    /// `blast` is handed an already-loaded [`Graph`].
    #[error("{}", no_graph_message(_0))]
    NoGraph(String),
}

/// Renders the `NoGraph` message, naming the active product's build
/// command.
fn no_graph_message(path: &str) -> String {
    format!(
        "no index at {path} \u{2014} run {} build",
        sieve_core::product().name
    )
}

/// The four `--format` names (section 3.1, 3.7 to 3.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlastFormat {
    Text,
    Markdown,
    Mermaid,
    Json,
}

/// Parses a raw `--format` value (section 3.1).
pub fn parse_format(raw: &str) -> Result<BlastFormat, BlastError> {
    match raw {
        "text" => Ok(BlastFormat::Text),
        "markdown" => Ok(BlastFormat::Markdown),
        "mermaid" => Ok(BlastFormat::Mermaid),
        "json" => Ok(BlastFormat::Json),
        other => Err(BlastError::BadFormat(other.to_string())),
    }
}

// ---------------------------------------------------------------------
// The diff reader (section 3.2)
// ---------------------------------------------------------------------

/// How a changed file was touched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
}

/// A contiguous run of changed lines, in post-image (new file) line
/// numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct LineRange {
    pub start: u32,
    pub end: u32,
}

/// One line git reported inside a hunk. `n` is the post-image number; a
/// deleted line has none.
#[derive(Debug, Clone, Serialize)]
pub struct DiffLine {
    pub n: Option<u32>,
    pub sign: char,
    pub text: String,
}

/// A hunk's range and the lines in it, capped at `MAX_HUNK_LINES`.
#[derive(Debug, Clone, Serialize)]
pub struct Hunk {
    pub start: u32,
    pub end: u32,
    pub lines: Vec<DiffLine>,
    pub dropped: u32,
}

/// One changed file: its status, its post-image ranges, and the hunks
/// behind them.
#[derive(Debug, Clone, Serialize)]
pub struct ChangedFile {
    pub path: String,
    pub status: ChangeStatus,
    #[serde(rename = "oldPath", skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    pub ranges: Vec<LineRange>,
    pub hunks: Vec<Hunk>,
}

/// Text kept per hunk and per file — a regenerated lockfile must not ride
/// along in `--format json` or in a diagram export (section 3.2).
const MAX_HUNK_LINES: usize = 24;
const MAX_FILE_LINES: usize = 200;

struct DiffResult {
    basis: String,
    files: Vec<ChangedFile>,
}

/// Runs `git` in `root`, returning its stdout, or `None` when git fails
/// (not a repo, unknown ref, non-zero exit).
fn run_git(root: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = Command::new("git");
    cmd.current_dir(root).arg("-c").arg("core.quotePath=false");
    cmd.args(args);
    let output = cmd.output().ok()?;
    if !output.status.success() {
        return None;
    }
    // Node's `spawnSync(..., { encoding: "utf8" })`
    // decodes an invalid byte to U+FFFD. A strict decode drops the whole
    // patch when one changed file holds latin1 (P3-26).
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// True when `reference` names something git can resolve (section 3.2).
pub fn ref_exists(root: &Path, reference: &str) -> bool {
    run_git(
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{reference}^{{commit}}"),
        ],
    )
    .is_some()
}

/// Changed files and their post-image line ranges, for the working tree
/// against `HEAD` (no base), or `<base>...HEAD` (section 3.2).
fn changed_files(root: &Path, base: Option<&str>) -> Option<DiffResult> {
    if let Some(base) = base {
        let basis = format!("{base}...HEAD");
        let files = diff_files(root, &[&basis])?;
        return Some(DiffResult { basis, files });
    }

    let working = diff_files(root, &["HEAD"])?;
    if !working.is_empty() {
        return Some(DiffResult {
            basis: "working tree vs HEAD".to_string(),
            files: working,
        });
    }
    match diff_files(root, &["HEAD~1...HEAD"]) {
        None => Some(DiffResult {
            basis: "working tree vs HEAD".to_string(),
            files: Vec::new(),
        }),
        Some(last) => Some(DiffResult {
            basis: "HEAD~1...HEAD".to_string(),
            files: last,
        }),
    }
}

/// Both git passes for one range: name-status (statuses, renames), then
/// hunks (section 3.2).
fn diff_files(root: &Path, range: &[&str]) -> Option<Vec<ChangedFile>> {
    let mut name_status_args = vec!["diff", "--name-status", "--find-renames", "-z"];
    name_status_args.extend_from_slice(range);
    name_status_args.push("--");
    let status_out = run_git(root, &name_status_args)?;
    let mut files = parse_name_status(&status_out);
    if files.is_empty() {
        return Some(files);
    }

    let mut patch_args = vec![
        "diff",
        "--unified=0",
        "--no-color",
        "--no-ext-diff",
        "--find-renames",
    ];
    patch_args.extend_from_slice(range);
    patch_args.push("--");
    if let Some(patch) = run_git(root, &patch_args) {
        apply_hunks(&mut files, &patch);
    }
    Some(files)
}

/// `--name-status -z` output: NUL-separated fields, where a rename or a
/// copy spends three fields (`R096`, old, new) and everything else spends
/// two (section 3.2).
fn parse_name_status(out: &str) -> Vec<ChangedFile> {
    let fields: Vec<&str> = out.split('\0').filter(|f| !f.is_empty()).collect();
    let mut files = Vec::new();
    let mut i = 0;
    while i < fields.len() {
        let code = fields[i];
        i += 1;
        if code.starts_with('R') || code.starts_with('C') {
            let Some(&old_path) = fields.get(i) else {
                break;
            };
            i += 1;
            let Some(&path) = fields.get(i) else { break };
            i += 1;
            files.push(ChangedFile {
                path: path.to_string(),
                status: ChangeStatus::Renamed,
                old_path: Some(old_path.to_string()),
                ranges: Vec::new(),
                hunks: Vec::new(),
            });
            continue;
        }
        let Some(&path) = fields.get(i) else { break };
        i += 1;
        let status = if code.starts_with('A') {
            ChangeStatus::Added
        } else if code.starts_with('D') {
            ChangeStatus::Deleted
        } else {
            ChangeStatus::Modified
        };
        files.push(ChangedFile {
            path: path.to_string(),
            status,
            old_path: None,
            ranges: Vec::new(),
            hunks: Vec::new(),
        });
    }
    files
}

fn hunk_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    // JS `\d` is ASCII only; Rust `\d` is not.
    RE.get_or_init(|| {
        Regex::new(r"^@@ -[0-9]+(?:,[0-9]+)? \+([0-9]+)(?:,([0-9]+))? @@").expect("valid regex")
    })
}

///  → `src/x.ts`, minus any trailing tab-separated timestamp.
fn strip_prefix(raw: &str) -> &str {
    let untabbed = raw.split('\t').next().unwrap_or(raw);
    untabbed.strip_prefix("b/").unwrap_or(untabbed)
}

/// Attaches each hunk's post-image range, and its text, to its file
/// (section 3.2).
fn apply_hunks(files: &mut [ChangedFile], patch: &str) {
    let by_path: HashMap<String, usize> = files
        .iter()
        .enumerate()
        .map(|(idx, f)| (f.path.clone(), idx))
        .collect();

    let mut current: Option<usize> = None;
    let mut hunk: Option<usize> = None;
    let mut next: u32 = 0;
    let mut kept: usize = 0;

    for line in patch.split('\n') {
        if line.starts_with("diff --git ") {
            current = None;
            hunk = None;
            continue;
        }
        if let Some(raw) = line.strip_prefix("+++ ") {
            current = if raw == "/dev/null" {
                None
            } else {
                by_path.get(strip_prefix(raw)).copied()
            };
            hunk = None;
            kept = 0;
            continue;
        }
        if line.starts_with("@@") {
            hunk = None;
            let Some(file_idx) = current else { continue };
            let Some(m) = hunk_re().captures(line) else {
                continue;
            };
            let start: u32 = m[1].parse().unwrap_or(0);
            let count: u32 = m
                .get(2)
                .map(|c| c.as_str().parse().unwrap_or(1))
                .unwrap_or(1);
            let range = if count == 0 {
                LineRange { start, end: start }
            } else {
                LineRange {
                    start,
                    end: start + count - 1,
                }
            };
            files[file_idx].ranges.push(range);
            files[file_idx].hunks.push(Hunk {
                start: range.start,
                end: range.end,
                lines: Vec::new(),
                dropped: 0,
            });
            hunk = Some(files[file_idx].hunks.len() - 1);
            next = start;
            continue;
        }
        let Some(file_idx) = current else { continue };
        let Some(hunk_idx) = hunk else { continue };
        if let Some(text) = line.strip_prefix('+') {
            let within_file = kept < MAX_FILE_LINES;
            kept += 1;
            push_line(
                &mut files[file_idx].hunks[hunk_idx],
                DiffLine {
                    n: Some(next),
                    sign: '+',
                    text: text.to_string(),
                },
                within_file,
            );
            next += 1;
        } else if let Some(text) = line.strip_prefix('-') {
            let within_file = kept < MAX_FILE_LINES;
            kept += 1;
            push_line(
                &mut files[file_idx].hunks[hunk_idx],
                DiffLine {
                    n: None,
                    sign: '-',
                    text: text.to_string(),
                },
                within_file,
            );
        }
    }
}

fn push_line(hunk: &mut Hunk, line: DiffLine, within_file: bool) {
    if !within_file || hunk.lines.len() >= MAX_HUNK_LINES {
        hunk.dropped += 1;
        return;
    }
    hunk.lines.push(line);
}

// ---------------------------------------------------------------------
// Seeds and the walk (section 3.3, 3.4)
// ---------------------------------------------------------------------

/// A changed symbol the walk started from.
#[derive(Debug, Clone, Serialize)]
pub struct Seed {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub path: String,
    pub span: String,
    #[serde(rename = "wholeFile")]
    pub whole_file: bool,
}

/// One dependent symbol, and how the walk reached it.
#[derive(Debug, Clone, Serialize)]
pub struct Impacted {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub path: String,
    pub span: String,
    pub relation: Relation,
    pub depth: u32,
    /// The id of the symbol the walk came through. The text report uses it
    /// to draw the tree. The JSON report does not hold it.
    #[serde(skip)]
    pub parent: Option<String>,
}

fn span_bounds(span: &str) -> Option<(u32, u32)> {
    let rest = span.strip_prefix('L')?;
    let (start, rest) = rest.split_once("-L")?;
    Some((start.parse().ok()?, rest.parse().ok()?))
}

/// The innermost symbols whose spans overlap any changed range in `path`:
/// a symbol that strictly contains another overlapping one is dropped, so
/// a changed line inside a method seeds the method, not its class
/// (section 3.3).
fn seeds_for_file<'g>(graph: &'g Graph, path: &str, ranges: &[LineRange]) -> Vec<&'g Node> {
    let symbols: Vec<(&Node, u32, u32)> = graph
        .nodes
        .iter()
        .filter(|n| n.kind != Kind::File && n.path == path)
        .filter_map(|n| span_bounds(&n.span).map(|(s, e)| (n, s, e)))
        .collect();
    if symbols.is_empty() {
        return Vec::new();
    }

    let mut order: Vec<&str> = Vec::new();
    let mut hit: HashMap<&str, &Node> = HashMap::new();
    for r in ranges {
        let overlapping: Vec<&(&Node, u32, u32)> = symbols
            .iter()
            .filter(|(_, s, e)| *s <= r.end && *e >= r.start)
            .collect();
        for s in &overlapping {
            let contains_another = overlapping
                .iter()
                .any(|o| !std::ptr::eq(*o, *s) && o.1 >= s.1 && o.2 <= s.2);
            if !contains_another && hit.insert(s.0.id.as_str(), s.0).is_none() {
                order.push(s.0.id.as_str());
            }
        }
    }
    order.into_iter().map(|id| hit[id]).collect()
}

/// One traversed edge, reached on the way in.
struct EdgeHit<'a> {
    id: String,
    node: Option<&'a Node>,
    relation: Relation,
    depth: u32,
    parent: String,
}

/// BFS over incoming walk-relation edges from every seed at once, each
/// node deduped by id and reported once at the depth it was first
/// reached from any seed (section 3.4, `impactOfMany`). `blast` always
/// walks `in`.
fn impact_of_many<'a>(graph: &'a Graph, seeds: &[&Node], max_depth: Depth) -> Vec<EdgeHit<'a>> {
    let by_id: HashMap<&str, &Node> = graph.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut adj: HashMap<&str, Vec<(&str, Relation)>> = HashMap::new();
    for edge in &graph.edges {
        if !WALK_RELATIONS.contains(&edge.relation) {
            continue;
        }
        adj.entry(edge.target.as_str())
            .or_default()
            .push((edge.source.as_str(), edge.relation));
    }

    let mut visited: HashSet<&str> = HashSet::new();
    let mut frontier: Vec<&str> = Vec::new();
    for seed in seeds {
        if visited.insert(seed.id.as_str()) {
            frontier.push(seed.id.as_str());
        }
    }

    let limit = match max_depth {
        Depth(Some(n)) => n,
        Depth(None) => usize::MAX,
    };
    let mut hits = Vec::new();
    let mut depth = 1usize;
    while !frontier.is_empty() && depth <= limit {
        let mut next = Vec::new();
        for current in &frontier {
            for (other, relation) in adj.get(*current).into_iter().flatten() {
                if !visited.insert(other) {
                    continue;
                }
                hits.push(EdgeHit {
                    id: other.to_string(),
                    node: by_id.get(*other).copied(),
                    relation: *relation,
                    depth: depth as u32,
                    parent: current.to_string(),
                });
                next.push(*other);
            }
        }
        frontier = next;
        depth += 1;
    }
    hits
}

// ---------------------------------------------------------------------
// Module labels (section 3.5)
// ---------------------------------------------------------------------

/// Path → module-label lookup. `conceptOf` names the deep tier's concept
/// claiming a file; Tier 1 writes no such file, so a Tier-1 build always
/// falls back to a directory label.
struct ModuleIndex {
    has_concepts: bool,
    /// `path` → `(concept name, that concept's own source count)`, the
    /// smallest claiming concept only (section 2.11).
    claims: HashMap<String, (String, usize)>,
}

/// Builds the module index from `context_dir`'s concept nodes (`sieve/*.md`,
/// read through [`sieve_core::concept::read_nodes`]). A file cited by
/// several concepts goes to the concept with the fewest `sources` overall;
/// a strict `<` keeps the earliest claim on a tie, over
/// `readNodes`' slug order.
fn module_index_for(context_dir: &Path) -> ModuleIndex {
    let nodes = sieve_core::concept::read_nodes(context_dir);
    let has_concepts = !nodes.is_empty();
    let mut claims: HashMap<String, (String, usize)> = HashMap::new();
    for node in &nodes {
        if node.name.is_empty() {
            continue;
        }
        let size = node.sources.len();
        for src in &node.sources {
            let wins = match claims.get(&src.path) {
                None => true,
                Some((_, prev_size)) => size < *prev_size,
            };
            if wins {
                claims.insert(src.path.clone(), (node.name.clone(), size));
            }
        }
    }
    ModuleIndex {
        has_concepts,
        claims,
    }
}

impl ModuleIndex {
    /// The name of the smallest concept that claims `path`, or `None` when
    /// no concept claims it.
    fn concept_of(&self, path: &str) -> Option<String> {
        self.claims.get(path).map(|(label, _)| label.clone())
    }
}

/// Longest label a circle can hold before the text overruns it.
const MAX_LABEL: usize = 30;

/// The JS `\s` class: ECMAScript WhiteSpace plus LineTerminator (spec
/// 22.2.2.9 `CharacterClassEscape :: s`). It keeps U+FEFF and drops U+0085,
/// unlike Rust `\s`. `callers::is_js_whitespace` names the same set; a
/// test in `ask::structural` pins the two together. The one copy in
/// `sieve-query`; `ask::structural` imports it.
pub(crate) const JS_WS: &str = r"[\t\n\x0B\x0C\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}]";

fn clause_break_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        // JS `/i` without `u` folds ASCII only, so the word list runs with
        // `(?i-u)`. `\s` is [`JS_WS`].
        Regex::new(&format!(
            r"{JS_WS}(?:\(|—|-{JS_WS})|:{JS_WS}|{JS_WS}(?i-u:via|and|for|with|using|in|across|over|from|of|by|to){JS_WS}"
        ))
        .expect("valid regex")
    })
}

fn concept_prefix_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(&format!(r"(?i-u)^concepts?:(?u:{JS_WS}*)")).expect("valid regex"))
}

/// A concept name trimmed to fit a node (section 3.5).
///
/// Every length and index is in UTF-16 units, as JS `length`, `slice` and
/// `lastIndexOf` are. JS `/i` without `u` folds ASCII only, so the prefix runs
/// with `(?i-u)`. Every trim is the JS one: `trim` strips U+FEFF and keeps
/// U+0085.
fn short_label(label: &str) -> String {
    let bare = js_trim(&concept_prefix_re().replace(label, "")).to_string();
    let units: Vec<u16> = bare.encode_utf16().collect();
    if units.len() <= MAX_LABEL {
        return bare;
    }
    if let Some(m) = clause_break_re().find(&bare) {
        let at = bare[..m.start()].encode_utf16().count();
        if (8..=MAX_LABEL).contains(&at) {
            return String::from_utf16_lossy(&units[..at])
                .trim_end_matches(is_js_whitespace)
                .to_string();
        }
    }
    // JS `lastIndexOf(" ", 30)` looks at index 30 too.
    let cut = units[..(MAX_LABEL + 1).min(units.len())]
        .iter()
        .rposition(|&u| u == u16::from(b' '));
    let cut = match cut {
        Some(at) if at > MAX_LABEL / 2 => at,
        _ => MAX_LABEL,
    };
    format!(
        "{}…",
        String::from_utf16_lossy(&units[..cut]).trim_end_matches(is_js_whitespace)
    )
}

/// The parent directory of a directory label, or `""` at the top level.
fn parent_dir(dir: &str) -> String {
    let trimmed = dir.strip_suffix('/').unwrap_or(dir);
    match trimmed.rfind('/') {
        Some(cut) => format!("{}/", &trimmed[..cut]),
        None => String::new(),
    }
}

/// Fallback label: the file's directory, or `(root)` for a top-level file.
fn dir_label(path: &str) -> String {
    match path.rfind('/') {
        Some(cut) => format!("{}/", &path[..cut]),
        None => "(root)".to_string(),
    }
}

/// The deterministic backstop label: the cluster's most significant
/// symbol, or `fallback` when the cluster has none.
fn hub_label(names: &[String], fallback: &str) -> String {
    match names.iter().find(|n| !n.is_empty()) {
        Some(head) => short_label(head),
        None => fallback.to_string(),
    }
}

/// The concept claiming EVERY file in a cluster, or `None` when they
/// disagree (or no concept claims any of them).
fn shared_concept(paths: &[String], index: &ModuleIndex) -> Option<String> {
    let concepts: HashSet<Option<String>> = paths.iter().map(|p| index.concept_of(p)).collect();
    if concepts.len() == 1 {
        concepts
            .into_iter()
            .next()
            .flatten()
            .map(|c| short_label(&c))
    } else {
        None
    }
}

/// Directory groups the diff is folded down to before anything is drawn
/// (section 3.5).
const MAX_AREAS: usize = 5;

/// Folds directory groups into their parents until at most `max` remain.
/// The deepest group goes first, and only into a parent that already
/// exists; never into the top level (section 3.5, `coarsen`).
fn coarsen(order: &mut Vec<String>, groups: &mut HashMap<String, Vec<String>>, max: usize) {
    let depth = |dir: &str| dir.split('/').filter(|s| !s.is_empty()).count();
    while groups.len() > max {
        let mut keys: Vec<String> = order.clone();
        keys.sort_by(|a, b| {
            depth(b)
                .cmp(&depth(a))
                .then(groups[a].len().cmp(&groups[b].len()))
        });
        let pick = keys
            .iter()
            .find(|k| {
                let parent = parent_dir(k);
                !parent.is_empty() && groups.contains_key(&parent)
            })
            .or_else(|| keys.iter().find(|k| !parent_dir(k).is_empty()));
        let Some(pick) = pick.cloned() else { return };
        let parent = parent_dir(&pick);
        let moved = groups.remove(&pick).unwrap_or_default();
        order.retain(|k| k != &pick);
        let entry = groups.entry(parent.clone()).or_insert_with(|| {
            order.push(parent.clone());
            Vec::new()
        });
        entry.extend(moved);
    }
}

fn test_path_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        // JS `/i` without `u` folds ASCII only.
        Regex::new(
            r"(?i-u)(^|/)(tests?|specs?|__tests__)/|\.(test|spec)\.[cm]?[jt]sx?$|_test\.(go|py|rb)$",
        )
        .expect("valid regex")
    })
}

/// True when `path` is a test file (section 3.6).
pub fn is_test_path(path: &str) -> bool {
    test_path_re().is_match(path)
}

// ---------------------------------------------------------------------
// The report (section 3.3 to 3.8)
// ---------------------------------------------------------------------

/// Where a cluster's label came from. `blast` never reaches `Named`
/// (`--name` is not ported): every label here is `Concept` or `Symbol`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LabelSource {
    Concept,
    Symbol,
}

/// Whether the diff brought its tests along (section 3.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TestSignal {
    Changed,
    Stale,
    None,
    Na,
}

impl TestSignal {
    fn glyph(self) -> char {
        match self {
            TestSignal::Changed => '✓',
            TestSignal::Stale => '⚠',
            TestSignal::None => '✗',
            TestSignal::Na => '–',
        }
    }
}

/// One area of the diff: the changed files a reviewer thinks of as one
/// thing.
#[derive(Debug, Clone, Serialize)]
pub struct ChangedArea {
    pub label: String,
    #[serde(rename = "labelSource")]
    pub label_source: LabelSource,
    pub key: String,
    pub files: Vec<String>,
    pub seeds: u32,
    pub tests: TestSignal,
    #[serde(rename = "testFiles")]
    pub test_files: Vec<String>,
    #[serde(rename = "changedTestFiles")]
    pub changed_test_files: Vec<String>,
    pub reached: u32,
    pub behavioural: u32,
    pub unreached: Vec<String>,
    #[serde(rename = "seedNames")]
    pub seed_names: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owners: Option<Vec<Owner>>,
}

/// Dependents grouped for the diagram: the unit a reviewer actually
/// thinks in.
#[derive(Debug, Clone, Serialize)]
pub struct ImpactedModule {
    pub label: String,
    #[serde(rename = "labelSource")]
    pub label_source: LabelSource,
    pub key: String,
    pub files: Vec<String>,
    pub symbols: Vec<Impacted>,
    pub from: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owners: Option<Vec<Owner>>,
}

/// The blast radius of one diff: the areas it changed, and the modules
/// that depend on them.
#[derive(Debug, Clone, Serialize)]
pub struct BlastReport {
    pub basis: String,
    pub depth: Option<u32>,
    pub changed: Vec<ChangedFile>,
    pub unindexed: Vec<String>,
    pub deleted: Vec<String>,
    pub seeds: Vec<Seed>,
    pub impacted: Vec<Impacted>,
    pub modules: Vec<ImpactedModule>,
    #[serde(rename = "testModules")]
    pub test_modules: Vec<ImpactedModule>,
    pub areas: Vec<ChangedArea>,
    /// Absent under `--no-owners`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reviewers: Option<Vec<Reviewer>>,
}

fn depth_as_u32(depth: Depth) -> Option<u32> {
    match depth {
        Depth(Some(n)) => Some(n as u32),
        Depth(None) => None,
    }
}

/// Groups dependents into modules by concept, else directory, skipping
/// any hit whose path the diff already changed — that is the diff
/// itself, not reach (section 3.5, `groupByModule`).
fn group_by_module(
    impacted: &[Impacted],
    changed_paths: &HashSet<&str>,
    origins: &HashMap<String, Vec<String>>,
    index: &ModuleIndex,
) -> (Vec<ImpactedModule>, Vec<ImpactedModule>) {
    let mut order: Vec<String> = Vec::new();
    let mut by_key: HashMap<String, ImpactedModule> = HashMap::new();

    for hit in impacted {
        if changed_paths.contains(hit.path.as_str()) {
            continue;
        }
        let key = index
            .concept_of(&hit.path)
            .unwrap_or_else(|| dir_label(&hit.path));
        let module = by_key.entry(key.clone()).or_insert_with(|| {
            order.push(key.clone());
            ImpactedModule {
                label: key.clone(),
                label_source: LabelSource::Symbol,
                key: key.clone(),
                files: Vec::new(),
                symbols: Vec::new(),
                from: Vec::new(),
                owners: None,
            }
        });
        if !module.files.contains(&hit.path) {
            module.files.push(hit.path.clone());
        }
        module.symbols.push(hit.clone());
    }

    for module in by_key.values_mut() {
        let mut from: HashSet<String> = HashSet::new();
        for s in &module.symbols {
            if let Some(paths) = origins.get(&s.id) {
                from.extend(paths.iter().cloned());
            }
        }
        module.from = from.into_iter().collect();
        module.from.sort();
        module.files.sort();
        module.symbols.sort_by(|a, b| {
            a.depth
                .cmp(&b.depth)
                .then_with(|| collate(&a.path, &b.path))
        });
        let concept = shared_concept(&module.files, index);
        let names: Vec<String> = module.symbols.iter().map(|s| s.name.clone()).collect();
        module.label = concept
            .clone()
            .unwrap_or_else(|| hub_label(&names, &module.key));
        module.label_source = if concept.is_some() {
            LabelSource::Concept
        } else {
            LabelSource::Symbol
        };
    }

    let is_test_only =
        |m: &ImpactedModule| !m.files.is_empty() && m.files.iter().all(|f| is_test_path(f));
    let mut all: Vec<ImpactedModule> = order
        .into_iter()
        .filter_map(|k| by_key.remove(&k))
        .collect();
    all.sort_by(|a, b| {
        b.symbols
            .len()
            .cmp(&a.symbols.len())
            .then_with(|| collate(&a.label, &b.label))
    });
    let (test_modules, modules): (Vec<_>, Vec<_>) = all.into_iter().partition(|m| is_test_only(m));
    (modules, test_modules)
}

/// Groups the changed files into areas, and works out whether each one's
/// tests moved (section 3.5, `changedAreas`).
#[allow(clippy::too_many_arguments)]
fn changed_areas(
    graph: &Graph,
    seed_order: &[String],
    seed_nodes: &HashMap<String, Vec<&Node>>,
    changed_test_paths: &HashSet<&str>,
    index: &ModuleIndex,
    modules: &[ImpactedModule],
) -> Vec<ChangedArea> {
    let node_by_id: HashMap<&str, &Node> = graph.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut in_degree: HashMap<&str, u32> = HashMap::new();
    for edge in &graph.edges {
        *in_degree.entry(edge.target.as_str()).or_insert(0) += 1;
    }
    let mut incoming: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in &graph.edges {
        let Some(path) = node_by_id
            .get(edge.source.as_str())
            .map(|n| n.path.as_str())
        else {
            continue;
        };
        if !is_test_path(path) {
            continue;
        }
        let at = incoming.entry(edge.target.as_str()).or_default();
        if !at.contains(&path) {
            at.push(path);
        }
    }

    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<String>> = HashMap::new();
    for path in seed_order {
        if is_test_path(path) {
            continue;
        }
        let dir = dir_label(path);
        let entry = groups.entry(dir.clone()).or_insert_with(|| {
            order.push(dir.clone());
            Vec::new()
        });
        entry.push(path.clone());
    }
    coarsen(&mut order, &mut groups, MAX_AREAS);

    struct Candidate {
        name: String,
        behavioural: bool,
        degree: u32,
    }

    let mut areas: Vec<ChangedArea> = Vec::new();
    let mut candidates: Vec<Vec<Candidate>> = Vec::new();
    for dir in &order {
        let Some(paths) = groups.get(dir) else {
            continue;
        };
        let mut area = ChangedArea {
            label: dir.clone(),
            label_source: LabelSource::Symbol,
            key: dir.clone(),
            files: Vec::new(),
            seeds: 0,
            tests: TestSignal::None,
            test_files: Vec::new(),
            changed_test_files: Vec::new(),
            reached: 0,
            behavioural: 0,
            unreached: Vec::new(),
            seed_names: Vec::new(),
            owners: None,
        };
        let mut area_candidates = Vec::new();

        for path in paths {
            let nodes = seed_nodes.get(path).cloned().unwrap_or_default();
            area.files.push(path.clone());
            area.seeds += nodes.len() as u32;

            for node in nodes {
                let behavioural = matches!(node.kind, Kind::Function | Kind::Method | Kind::Class);
                let from = incoming.get(node.id.as_str()).cloned().unwrap_or_default();
                for t in &from {
                    if !area.test_files.iter().any(|f| f == t) {
                        area.test_files.push(t.to_string());
                    }
                    if changed_test_paths.contains(t)
                        && !area.changed_test_files.iter().any(|f| f == t)
                    {
                        area.changed_test_files.push(t.to_string());
                    }
                }
                area_candidates.push(Candidate {
                    name: node.name.clone(),
                    behavioural,
                    degree: *in_degree.get(node.id.as_str()).unwrap_or(&0),
                });
                if !behavioural {
                    continue;
                }
                area.behavioural += 1;
                if !from.is_empty() {
                    area.reached += 1;
                } else {
                    area.unreached.push(node.name.clone());
                }
            }
        }

        areas.push(area);
        candidates.push(area_candidates);
    }

    for (area, area_candidates) in areas.iter_mut().zip(candidates.iter_mut()) {
        area_candidates.sort_by(|a, b| {
            b.behavioural
                .cmp(&a.behavioural)
                .then_with(|| b.degree.cmp(&a.degree))
                .then_with(|| collate(&a.name, &b.name))
        });
        area.seed_names = area_candidates.iter().map(|c| c.name.clone()).collect();
        let concept = shared_concept(&area.files, index);
        area.label = concept
            .clone()
            .unwrap_or_else(|| hub_label(&area.seed_names, &area.key));
        area.label_source = if concept.is_some() {
            LabelSource::Concept
        } else {
            LabelSource::Symbol
        };
        area.files.sort();
        area.test_files.sort();
        area.changed_test_files.sort();
        area.tests = if area.behavioural == 0 {
            TestSignal::Na
        } else if !area.changed_test_files.is_empty() {
            TestSignal::Changed
        } else if !area.test_files.is_empty() {
            TestSignal::Stale
        } else {
            TestSignal::None
        };
    }

    let reach = |a: &ChangedArea| -> usize {
        modules
            .iter()
            .filter(|m| m.from.iter().any(|f| a.files.contains(f)))
            .count()
    };
    areas.sort_by(|a, b| {
        reach(b)
            .cmp(&reach(a))
            .then_with(|| b.files.len().cmp(&a.files.len()))
            .then_with(|| collate(&a.label, &b.label))
    });
    areas
}

/// Computes the blast radius of `changed` against `graph` (section 3.3
/// to 3.5). `depth` is the BFS depth; `context_dir` feeds the (currently
/// concept-less) module index.
fn blast_radius(
    graph: &Graph,
    context_dir: &Path,
    changed: Vec<ChangedFile>,
    basis: String,
    depth: Depth,
) -> BlastReport {
    let index = module_index_for(context_dir);
    let _ = index.has_concepts;

    let file_nodes: HashMap<&str, &Node> = graph
        .nodes
        .iter()
        .filter(|n| n.kind == Kind::File)
        .map(|n| (n.path.as_str(), n))
        .collect();

    let mut seeds: Vec<Seed> = Vec::new();
    let mut unindexed: Vec<String> = Vec::new();
    let mut deleted: Vec<String> = Vec::new();
    let mut seed_order: Vec<String> = Vec::new();
    let mut seed_nodes: HashMap<String, Vec<&Node>> = HashMap::new();

    let mut merge_order: Vec<String> = Vec::new();
    let mut merged: HashMap<String, (Impacted, Vec<String>)> = HashMap::new();

    for file in &changed {
        if file.status == ChangeStatus::Deleted {
            deleted.push(file.path.clone());
            continue;
        }
        let Some(&file_node) = file_nodes.get(file.path.as_str()) else {
            unindexed.push(file.path.clone());
            continue;
        };

        let symbol_seeds: Vec<&Node> = if !file.ranges.is_empty() {
            seeds_for_file(graph, &file.path, &file.ranges)
        } else {
            Vec::new()
        };
        for node in &symbol_seeds {
            seeds.push(Seed {
                id: node.id.clone(),
                name: node.name.clone(),
                kind: node.kind,
                path: node.path.clone(),
                span: node.span.clone(),
                whole_file: false,
            });
        }
        if symbol_seeds.is_empty() {
            seeds.push(Seed {
                id: file_node.id.clone(),
                name: file_node.name.clone(),
                kind: file_node.kind,
                path: file_node.path.clone(),
                span: file_node.span.clone(),
                whole_file: true,
            });
        }

        let walk_seeds: Vec<&Node> = if symbol_seeds.is_empty() {
            vec![file_node]
        } else {
            symbol_seeds
        };
        seed_order.push(file.path.clone());
        seed_nodes.insert(file.path.clone(), walk_seeds.clone());

        let hits = impact_of_many(graph, &walk_seeds, depth);
        for h in hits {
            let Some(node) = h.node else { continue };
            let impacted = Impacted {
                id: node.id.clone(),
                name: node.name.clone(),
                kind: node.kind,
                path: node.path.clone(),
                span: node.span.clone(),
                relation: h.relation,
                depth: h.depth,
                parent: Some(h.parent.clone()),
            };
            match merged.get_mut(&h.id) {
                None => {
                    merge_order.push(h.id.clone());
                    merged.insert(h.id.clone(), (impacted, vec![file.path.clone()]));
                }
                Some((prev, from)) => {
                    if !from.contains(&file.path) {
                        from.push(file.path.clone());
                    }
                    if impacted.depth < prev.depth {
                        *prev = impacted;
                    }
                }
            }
        }
    }

    let impacted: Vec<Impacted> = merge_order.iter().map(|id| merged[id].0.clone()).collect();
    let origins: HashMap<String, Vec<String>> = merge_order
        .iter()
        .map(|id| (id.clone(), merged[id].1.clone()))
        .collect();
    let changed_paths: HashSet<&str> = changed.iter().map(|c| c.path.as_str()).collect();
    let (modules, test_modules) = group_by_module(&impacted, &changed_paths, &origins, &index);

    let changed_test_paths: HashSet<&str> = changed
        .iter()
        .filter(|c| is_test_path(&c.path))
        .map(|c| c.path.as_str())
        .collect();
    let areas = changed_areas(
        graph,
        &seed_order,
        &seed_nodes,
        &changed_test_paths,
        &index,
        &modules,
    );

    BlastReport {
        basis,
        depth: depth_as_u32(depth),
        changed,
        unindexed,
        deleted,
        seeds,
        impacted,
        modules,
        test_modules,
        areas,
        reviewers: None,
    }
}

/// Computes `graph`'s blast radius for the diff `repo_root` carries,
/// against `context_dir`'s module index (section 3.1 to 3.5).
pub fn blast(
    graph: &Graph,
    repo_root: &Path,
    context_dir: &Path,
    opts: &BlastOptions,
) -> Result<BlastReport, BlastError> {
    if let Some(base) = &opts.base {
        if !ref_exists(repo_root, base) {
            return Err(BlastError::BadBase(base.clone()));
        }
    }

    let diff = changed_files(repo_root, opts.base.as_deref())
        .ok_or_else(|| BlastError::NotGit(repo_root.display().to_string()))?;

    let mut report = blast_radius(graph, context_dir, diff.files, diff.basis, opts.depth);

    // The diff's own authors never review their own change: with no
    // `--base` the local identity stands in for the range.
    if opts.owners {
        let mut exclude = match &opts.base {
            None => owners::local_identity(repo_root),
            Some(base) => owners::diff_authors(repo_root, base),
        };
        exclude.extend(opts.pr_author.iter().cloned());
        owners::attach_owners(repo_root, &mut report, &exclude, owners::now_ms());
    }

    Ok(report)
}

// ---------------------------------------------------------------------
// Text rendering (section 3.7)
// ---------------------------------------------------------------------

fn plural(n: usize, one: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {one}s")
    }
}

fn depth_label(depth: Option<u32>) -> String {
    match depth {
        Some(n) => format!("depth {n}"),
        None => "full closure".to_string(),
    }
}

/// The most changed symbols the text report lists.
const MAX_SEEDS: usize = 12;

/// The most dependents the text report lists.
const MAX_LISTED: usize = 40;

/// A count and the first names of some files: `2 files: a.ts, b.ts`.
fn file_list(files: &[String]) -> String {
    let shown: Vec<&str> = files.iter().take(5).map(String::as_str).collect();
    let more = match files.len().saturating_sub(5) {
        0 => String::new(),
        n => format!(" +{n} more"),
    };
    format!(
        "{}: {}{more}",
        plural(files.len(), "file"),
        shown.join(", ")
    )
}

/// The row of one dependent, with its relation when it is not a call.
fn impacted_row(s: &Impacted) -> String {
    let row = sieve_core::voice::row(
        &s.name,
        crate::ask::kind_word(s.kind),
        &s.path,
        Some(&s.span),
    );
    match s.relation {
        Relation::Calls => row,
        other => format!("{row}  \u{b7} {}", other.as_str()),
    }
}

/// The plain-text report the terminal path prints, with no savings line
/// (section 3.7). It names the changed symbols, counts the dependents, and
/// draws them as a tree by hop.
pub fn format_blast_text(r: &BlastReport) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut names: Vec<&str> = Vec::new();
    for seed in &r.seeds {
        if !names.contains(&seed.name.as_str()) {
            names.push(seed.name.as_str());
        }
    }
    let shown = names.iter().take(2).copied().collect::<Vec<_>>().join(", ");
    let more = names.len().saturating_sub(2);
    let shown = match (shown.is_empty(), more) {
        (true, _) => "nothing in the index".to_string(),
        (false, 0) => shown,
        (false, n) => format!("{shown} and {n} more"),
    };
    let reached: u32 = r.areas.iter().map(|a| a.reached).sum();
    let behavioural: u32 = r.areas.iter().map(|a| a.behavioural).sum();
    let tested = if behavioural > 0 {
        format!(" \u{b7} {reached} of {behavioural} reached by a test")
    } else {
        String::new()
    };
    lines.push(format!(
        "your diff changes {shown} \u{b7} {} in {}{tested} \u{b7} {}",
        plural(r.seeds.len(), "symbol"),
        plural(r.changed.len(), "file"),
        r.basis
    ));

    // More than one changed symbol: one row for each, with its range.
    if r.seeds.len() > 1 {
        for seed in r.seeds.iter().take(MAX_SEEDS) {
            lines.push(format!(
                "  {}",
                sieve_core::voice::row(
                    &seed.name,
                    crate::ask::kind_word(seed.kind),
                    &seed.path,
                    Some(&seed.span)
                )
            ));
        }
        if r.seeds.len() > MAX_SEEDS {
            lines.push(format!(
                "  \u{2026} {} not shown",
                plural(r.seeds.len() - MAX_SEEDS, "more symbol")
            ));
        }
    }

    // The dependents, minus the test-only modules, in module order.
    let listed: Vec<&Impacted> = r.modules.iter().flat_map(|m| m.symbols.iter()).collect();
    let deepest = listed.iter().map(|s| s.depth).max().unwrap_or(1);
    let reach = if deepest > 1 {
        format!(" within {deepest} hops")
    } else {
        String::new()
    };
    if listed.is_empty() {
        lines.push("no callers affected outside the changed files".to_string());
    } else {
        lines.push(format!(
            "{} affected{reach}",
            plural(listed.len(), "caller")
        ));
        let items: Vec<sieve_core::voice::TreeItem> = listed
            .iter()
            .map(|s| sieve_core::voice::TreeItem {
                id: s.id.clone(),
                parent: s.parent.clone(),
                text: impacted_row(s),
                note: None,
            })
            .collect();
        let tree = sieve_core::voice::tree_lines(&items);
        let hidden = tree.len().saturating_sub(MAX_LISTED);
        lines.extend(tree.into_iter().take(MAX_LISTED));
        if hidden > 0 {
            lines.push(format!(
                "\u{2026} {} not shown",
                plural(hidden, "more caller")
            ));
        }
    }

    if !r.test_modules.is_empty() {
        let files: HashSet<&str> = r
            .test_modules
            .iter()
            .flat_map(|m| m.files.iter().map(|f| f.as_str()))
            .collect();
        let verb = if files.len() == 1 {
            "references"
        } else {
            "reference"
        };
        lines.push(format!(
            "{} also {verb} this code (not listed)",
            plural(files.len(), "test suite")
        ));
    }
    for a in &r.areas {
        match a.tests {
            TestSignal::None => lines.push(format!("no test reaches {}", a.label)),
            TestSignal::Stale => lines.push(format!("tests for {} were not updated", a.label)),
            TestSignal::Changed | TestSignal::Na => {}
        }
    }
    if let Some(reviewers) = &r.reviewers {
        if !reviewers.is_empty() {
            let now = owners::now_ms();
            let who: Vec<String> = reviewers
                .iter()
                .take(MAX_REVIEWERS)
                .map(|p| {
                    format!(
                        "{} ({}, {})",
                        owners::mention(&p.name, p.handle.as_deref()),
                        plural(p.commits as usize, "commit"),
                        owners::since_label(p.last, now)
                    )
                })
                .collect();
            lines.push(format!("ask for review  {}", who.join(" \u{b7} ")));
        }
    }
    if !r.deleted.is_empty() {
        lines.push(format!(
            "deleted  {} \u{b7} their callers cannot be found from this index",
            file_list(&r.deleted)
        ));
    }
    if !r.unindexed.is_empty() {
        lines.push(format!("not indexed  {}", file_list(&r.unindexed)));
    }
    format!("{}\n", lines.join("\n"))
}

fn caveat_lines(r: &BlastReport) -> Vec<String> {
    let mut out = Vec::new();
    if !r.deleted.is_empty() {
        out.push(format!(
            "⚠️ {} ({}) — their dependents cannot be computed from a graph built at this commit, since the files are gone from it.",
            plural(r.deleted.len(), "deleted file"),
            r.deleted.iter().take(5).cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    if !r.unindexed.is_empty() {
        out.push(format!(
            "⚠️ {} not in the graph ({}) — no parser claims the extension, or the index predates the file.",
            plural(r.unindexed.len(), "changed file"),
            r.unindexed.iter().take(5).cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use sieve_core::{Edge, Meta, Origin, SummaryState};
    use std::fs;

    fn node(id: &str, kind: Kind, path: &str, span: &str) -> Node {
        Node {
            id: id.to_string(),
            name: id.rsplit(['#', '.']).next().unwrap_or(id).to_string(),
            kind,
            path: path.to_string(),
            span: span.to_string(),
            signature: None,
            exported: true,
            origin: Origin::Ast,
            body_hash: "0".repeat(64),
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

    fn graph_with(nodes: Vec<Node>, edges: Vec<Edge>) -> Graph {
        Graph {
            meta: Meta {
                version: 1,
                node_count: nodes.len(),
                edge_count: edges.len(),
                languages: Vec::new(),
                scopes: Vec::new(),
            },
            nodes,
            edges,
        }
    }

    #[test]
    fn parse_name_status_reads_a_rename_record() {
        let out = "R100\0old/path.ts\0new/path.ts\0";
        let files = parse_name_status(out);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "new/path.ts");
        assert_eq!(files[0].old_path, Some("old/path.ts".to_string()));
        assert_eq!(files[0].status, ChangeStatus::Renamed);
    }

    #[test]
    fn hunk_header_with_zero_count_records_the_line_before_the_gap() {
        let m = hunk_re().captures("@@ -10,3 +5,0 @@").expect("matches");
        let start: u32 = m[1].parse().unwrap();
        let count: u32 = m.get(2).map(|c| c.as_str().parse().unwrap()).unwrap_or(1);
        assert_eq!(start, 5);
        assert_eq!(count, 0);
        let range = if count == 0 {
            LineRange { start, end: start }
        } else {
            LineRange {
                start,
                end: start + count - 1,
            }
        };
        assert_eq!(range, LineRange { start: 5, end: 5 });
    }

    #[test]
    fn seeds_for_file_picks_the_innermost_overlapping_symbol() {
        let graph = graph_with(
            vec![
                node("a.ts#C", Kind::Class, "a.ts", "L1-L10"),
                node("a.ts#C.m", Kind::Method, "a.ts", "L4-L6"),
            ],
            Vec::new(),
        );
        let seeds = seeds_for_file(&graph, "a.ts", &[LineRange { start: 5, end: 5 }]);
        assert_eq!(seeds.len(), 1);
        assert_eq!(seeds[0].id, "a.ts#C.m");
    }

    #[test]
    fn coarsen_never_folds_a_group_into_the_top_level() {
        let mut order: Vec<String> = (0..6).map(|i| format!("src/m{i}/")).collect();
        let mut groups: HashMap<String, Vec<String>> = order
            .iter()
            .map(|k| (k.clone(), vec![format!("{k}file.ts")]))
            .collect();
        coarsen(&mut order, &mut groups, MAX_AREAS);
        assert!(groups.len() <= MAX_AREAS || groups.len() == order.len());
        assert!(!groups.contains_key(""));
    }

    #[test]
    fn test_path_regex_matches_ts_and_go_conventions() {
        assert!(is_test_path("src/x.test.ts"));
        assert!(is_test_path("pkg/x_test.go"));
        assert!(!is_test_path("src/x.ts"));
    }

    #[test]
    fn format_blast_text_reports_zero_impact_with_no_dependents_line() {
        let report = BlastReport {
            basis: "working tree vs HEAD".to_string(),
            depth: Some(1),
            changed: Vec::new(),
            unindexed: Vec::new(),
            deleted: Vec::new(),
            seeds: Vec::new(),
            impacted: Vec::new(),
            modules: Vec::new(),
            test_modules: Vec::new(),
            areas: Vec::new(),
            reviewers: None,
        };
        let text = format_blast_text(&report);
        assert!(text.contains("no callers affected outside the changed files"));
        assert!(text.ends_with('\n') && !text.ends_with("\n\n"));
    }

    #[test]
    fn format_blast_text_lists_every_changed_symbol_with_its_range() {
        let seed = |n: &str, span: &str| Seed {
            id: n.to_string(),
            name: n.to_string(),
            kind: Kind::Function,
            path: "a.ts".to_string(),
            span: span.to_string(),
            whole_file: false,
        };
        let report = BlastReport {
            basis: "working tree vs HEAD".to_string(),
            depth: Some(2),
            changed: Vec::new(),
            unindexed: Vec::new(),
            deleted: Vec::new(),
            seeds: vec![seed("a", "L1-L4"), seed("b", "L6-L9"), seed("c", "L11-L12")],
            impacted: Vec::new(),
            modules: Vec::new(),
            test_modules: Vec::new(),
            areas: Vec::new(),
            reviewers: None,
        };
        let text = format_blast_text(&report);
        assert!(
            text.starts_with("your diff changes a, b and 1 more \u{b7} 3 symbols"),
            "{text}"
        );
        assert!(
            text.contains("\n  a  fn  a.ts:1-4\n  b  fn  a.ts:6-9\n  c  fn  a.ts:11-12\n"),
            "{text}"
        );
    }

    #[test]
    fn format_blast_text_draws_the_dependents_as_a_tree_by_hop() {
        let dep = |id: &str, depth: u32, parent: &str| Impacted {
            id: id.to_string(),
            name: id.to_string(),
            kind: Kind::Function,
            path: format!("{id}.ts"),
            span: "L1-L4".to_string(),
            relation: Relation::Calls,
            depth,
            parent: Some(parent.to_string()),
        };
        let symbols = vec![dep("a", 1, "seed"), dep("b", 2, "a")];
        let report = BlastReport {
            basis: "working tree vs HEAD".to_string(),
            depth: Some(2),
            changed: Vec::new(),
            unindexed: vec![".gitignore".to_string()],
            deleted: Vec::new(),
            seeds: Vec::new(),
            impacted: symbols.clone(),
            modules: vec![ImpactedModule {
                label: "m".to_string(),
                label_source: LabelSource::Symbol,
                key: "m".to_string(),
                files: Vec::new(),
                symbols,
                from: Vec::new(),
                owners: None,
            }],
            test_modules: Vec::new(),
            areas: Vec::new(),
            reviewers: None,
        };
        let text = format_blast_text(&report);
        assert_eq!(
            text,
            "your diff changes nothing in the index \u{b7} 0 symbols in 0 files \u{b7} working tree vs HEAD\n\
             2 callers affected within 2 hops\n\
             \u{2514}\u{2500} a  fn  a.ts:1-4\n   \u{2514}\u{2500} b  fn  b.ts:1-4\n\
             not indexed  1 file: .gitignore\n"
        );
    }

    #[test]
    fn depth_serializes_as_null_when_unbounded() {
        let report = BlastReport {
            basis: "HEAD".to_string(),
            depth: None,
            changed: Vec::new(),
            unindexed: Vec::new(),
            deleted: Vec::new(),
            seeds: Vec::new(),
            impacted: Vec::new(),
            modules: Vec::new(),
            test_modules: Vec::new(),
            areas: Vec::new(),
            reviewers: None,
        };
        let json = serde_json::to_string(&report).expect("serializes");
        assert!(json.contains("\"depth\":null"));
    }

    #[test]
    fn ref_exists_is_false_outside_any_git_repository() {
        let dir = std::env::temp_dir();
        assert!(!ref_exists(&dir, "definitely-not-a-ref-xyz"));
    }

    // -----------------------------------------------------------------
    // Concept-label tests (section 2.11, P1-24 to P1-26).
    // -----------------------------------------------------------------

    /// A unique temp dir per call, so concurrent test runs never collide.
    /// It removes itself on drop, even if the test panics first.
    fn concept_dir(label: &str) -> crate::test_support::TempDir {
        crate::test_support::TempDir::new(label)
    }

    fn write_concept(dir: &Path, slug: &str, name: &str, sources: &[&str]) {
        let list: String = sources
            .iter()
            .map(|p| format!("  - path: {p}\n    hash: aaaa\n"))
            .collect();
        let text =
            format!("---\nname: {name}\nslug: {slug}\ntype: concept\nsources:\n{list}---\nbody\n");
        fs::write(dir.join(format!("{slug}.md")), text).expect("write concept node");
    }

    #[test]
    fn module_index_gives_a_shared_file_to_the_smallest_claiming_concept() {
        let dir = concept_dir("smallest");
        // "big" claims two files, "small" claims one of the same two: the
        // smaller concept should win the shared file.
        write_concept(&dir, "big", "Big Concept", &["src/a.ts", "src/b.ts"]);
        write_concept(&dir, "small", "Small Concept", &["src/a.ts"]);

        let index = module_index_for(&dir);
        assert_eq!(
            index.concept_of("src/a.ts"),
            Some("Small Concept".to_string())
        );
        assert_eq!(
            index.concept_of("src/b.ts"),
            Some("Big Concept".to_string())
        );
    }

    #[test]
    fn module_index_keeps_the_earliest_claim_on_a_size_tie() {
        let dir = concept_dir("tie");
        // Both concepts claim one file each, so their sizes tie. Sieve's
        // `<` test never replaces an equal-size claim, and `readNodes`
        // visits nodes in slug order, so "alpha" (read first) keeps the
        // claim over "beta".
        write_concept(&dir, "alpha", "Alpha Concept", &["src/a.ts"]);
        write_concept(&dir, "beta", "Beta Concept", &["src/a.ts"]);

        let index = module_index_for(&dir);
        assert_eq!(
            index.concept_of("src/a.ts"),
            Some("Alpha Concept".to_string())
        );
    }

    #[test]
    fn shared_concept_is_none_when_one_file_maps_elsewhere() {
        let dir = concept_dir("split");
        write_concept(&dir, "one", "One Concept", &["src/a.ts"]);
        write_concept(&dir, "two", "Two Concept", &["src/b.ts"]);
        let index = module_index_for(&dir);

        let files = vec!["src/a.ts".to_string(), "src/b.ts".to_string()];
        assert_eq!(shared_concept(&files, &index), None);
    }

    #[test]
    fn shared_concept_labels_a_cluster_when_every_file_agrees() {
        let dir = concept_dir("agree");
        write_concept(&dir, "one", "One Concept", &["src/a.ts", "src/b.ts"]);
        let index = module_index_for(&dir);

        let files = vec!["src/a.ts".to_string(), "src/b.ts".to_string()];
        assert_eq!(
            shared_concept(&files, &index),
            Some("One Concept".to_string())
        );
    }

    #[test]
    fn short_label_strips_the_concepts_prefix_and_leaves_a_short_name_alone() {
        assert_eq!(short_label("Concept: Retry Helper"), "Retry Helper");
        assert_eq!(short_label("concepts: Retry Helper"), "Retry Helper");
        assert_eq!(short_label("Retry Helper"), "Retry Helper");
    }

    #[test]
    fn short_label_trims_a_long_name_to_thirty_characters() {
        let long = "Reciprocal Rank Fusion for Workspace Federation Across Every Shard";
        let label = short_label(long);
        assert!(label.chars().count() <= MAX_LABEL + 1); // +1 for a trailing "…"
        assert!(long.starts_with(label.trim_end_matches('…').trim_end()));
    }

    /// The `✗ <label> — 2 changed files` line's label from one
    /// capture under `tests/fixtures/edges.expected/blast-label/`.
    fn golden_blast_label(slug: &str) -> String {
        let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
        let path = Path::new(&manifest)
            .join("../../tests/fixtures/edges.expected/blast-label")
            .join(format!("{slug}.stdout.txt"));
        let text = fs::read_to_string(&path).unwrap_or_else(|_| panic!("read {}", path.display()));
        // An older golden has a `✗ <label> — ...` line. A golden from the
        // current text has a `no test reaches <label>` line.
        if let Some(line) = text.lines().find(|l| l.starts_with("✗ ")) {
            let (label, _) = line[4..].split_once(" — ").expect("label before the dash");
            return label.to_string();
        }
        text.lines()
            .find_map(|l| l.strip_prefix("no test reaches "))
            .expect("a label line in the golden")
            .to_string()
    }

    /// P1-25: JS `length` and `slice` count UTF-16 units. A byte slice at
    /// 30 lands inside `é` here and panics.
    #[test]
    fn test_p1_25_short_label_cuts_in_utf16_units_not_bytes() {
        let name = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxéyyyyy";
        assert_eq!(short_label(name), golden_blast_label("label-utf16"));
        assert_eq!(short_label(name), "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxé…");
    }

    /// P1-25: JS `lastIndexOf(" ", 30)` takes a space at index 30.
    #[test]
    fn test_p1_25_short_label_word_cut_takes_a_space_at_index_thirty() {
        let name = "Aaaaaaaaaaaaaaaaaaaa Bbbbbbbbb Cccc";
        assert_eq!(short_label(name), golden_blast_label("label-word"));
        assert_eq!(short_label(name), "Aaaaaaaaaaaaaaaaaaaa Bbbbbbbbb…");
    }

    /// P1-25: JS `/^concepts?:\s*/i` without `u` folds ASCII only, so a
    /// long s (U+017F) is not an `s` there.
    #[test]
    fn test_p1_25_short_label_keeps_a_long_s_concept_prefix() {
        assert_eq!(short_label("conceptſ: x"), "conceptſ: x");
        assert_eq!(short_label("CONCEPTS: x"), "x");
    }

    /// P1-25: JS `/i` without `u` folds ASCII only in the clause-break and
    /// test-path patterns too. `K` (U+212A) is not a `k` there.
    #[test]
    fn test_p1_25_ascii_case_fold_in_the_fixed_blast_patterns() {
        let kelvin_via = "Long Concept Name \u{212A}\u{212A}\u{212A} Reaches Further";
        assert!(clause_break_re()
            .find("Long Concept Name via Reaches")
            .is_some());
        assert!(clause_break_re().find(kelvin_via).is_none());
        assert!(test_path_re().is_match("src/TESTS/a.ts"));
        assert!(!test_path_re().is_match("src/te\u{017F}ts/a.ts"));
    }

    /// P3-26: JS `\d` in the hunk header pattern is ASCII only.
    #[test]
    fn test_p3_26_hunk_re_rejects_a_non_ascii_digit() {
        assert!(hunk_re().captures("@@ -1,2 +3,4 @@").is_some());
        assert!(hunk_re().captures("@@ -\u{661},2 +\u{663},4 @@").is_none());
    }

    #[test]
    fn label_source_is_concept_when_unanimous_and_symbol_when_it_disagrees() {
        let dir = concept_dir("label-source");
        write_concept(&dir, "one", "One Concept", &["src/a.ts", "src/b.ts"]);
        let index = module_index_for(&dir);
        assert!(index.has_concepts);

        let agreeing = vec!["src/a.ts".to_string(), "src/b.ts".to_string()];
        let agreeing_source = if shared_concept(&agreeing, &index).is_some() {
            LabelSource::Concept
        } else {
            LabelSource::Symbol
        };
        assert_eq!(agreeing_source, LabelSource::Concept);

        let disagreeing = vec!["src/a.ts".to_string(), "src/c.ts".to_string()];
        let disagreeing_source = if shared_concept(&disagreeing, &index).is_some() {
            LabelSource::Concept
        } else {
            LabelSource::Symbol
        };
        assert_eq!(disagreeing_source, LabelSource::Symbol);
    }

    #[test]
    fn module_index_with_no_concept_files_leaves_edges_untouched() {
        let dir = concept_dir("empty");
        let index = module_index_for(&dir);
        assert!(!index.has_concepts);
        assert_eq!(index.concept_of("src/a.ts"), None);
    }

    /// The `✗` label from a `blast-dv/<id>.stdout.txt` golden.
    fn golden_blast_dv_label(id: &str) -> String {
        let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
        let path = Path::new(&manifest)
            .join("../../tests/fixtures/edges.expected/blast-dv")
            .join(format!("{id}.stdout.txt"));
        let text = fs::read_to_string(&path).unwrap_or_else(|_| panic!("read {}", path.display()));
        // An older golden has a `✗ <label> — ...` line. A golden from the
        // current text has a `no test reaches <label>` line.
        if let Some(line) = text.lines().find(|l| l.starts_with("✗ ")) {
            let (label, _) = line[4..].split_once(" — ").expect("label before the dash");
            return label.to_string();
        }
        text.lines()
            .find_map(|l| l.strip_prefix("no test reaches "))
            .expect("a label line in the golden")
            .to_string()
    }

    /// DV12: JS `\s` and `trim` take U+FEFF.
    #[test]
    fn test_p1_25_dv12_short_label_strips_a_bom() {
        assert_eq!(
            short_label("concept:\u{FEFF}Foo Bar"),
            golden_blast_dv_label("label-bom")
        );
        assert_eq!(short_label("concept:\u{FEFF}Foo Bar"), "Foo Bar");
    }

    /// DV13: JS `\s` and `trim` leave U+0085.
    #[test]
    fn test_p1_25_dv13_short_label_keeps_a_nel() {
        assert_eq!(
            short_label("concept:\u{85}Foo Bar"),
            golden_blast_dv_label("label-nel")
        );
        assert_eq!(short_label("concept:\u{85}Foo Bar"), "\u{85}Foo Bar");
    }

    /// DV6 (P3-25): the label tie-break is `localeCompare`, not byte order.
    #[test]
    fn test_p3_25_dv6_module_order_is_locale_compare() {
        let seed = node("src/a.ts#f", Kind::Function, "src/a.ts", "L1-L1");
        let alpha = node("lib/a.ts#alpha", Kind::Function, "lib/a.ts", "L1-L1");
        let zulu = node("lib2/z.ts#Zulu", Kind::Function, "lib2/z.ts", "L1-L1");
        let mut hits: Vec<Impacted> = Vec::new();
        for n in [&zulu, &alpha] {
            hits.push(Impacted {
                id: n.id.clone(),
                name: n.name.clone(),
                kind: n.kind,
                path: n.path.clone(),
                span: n.span.clone(),
                relation: Relation::Calls,
                depth: 1,
                parent: None,
            });
        }
        let changed: HashSet<&str> = [seed.path.as_str()].into_iter().collect();
        let index = module_index_for(&concept_dir("order"));
        let (modules, _) = group_by_module(&hits, &changed, &HashMap::new(), &index);
        let labels: Vec<&str> = modules.iter().map(|m| m.label.as_str()).collect();
        assert_eq!(labels, vec!["alpha", "Zulu"]);
    }
}
