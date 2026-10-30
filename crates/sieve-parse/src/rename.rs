//! The cross-file rename plan for TS and JS top-level functions (design
//! `docs/design/rename.md`, slice RN2). This is a dry run: it never writes a
//! source file. It may refresh the index under the build lock, as design
//! section D steps 1 and 2 say.
//!
//! The plan proves each edit site with the conservative scope scan
//! (`ts_scope`) and the existing barrel walk. Any doubt refuses the whole
//! rename. A report site never refuses.

mod apply;
#[cfg(test)]
mod apply_tests;
mod glob;
use glob::Reach;
mod policy;
#[cfg(test)]
mod tests;

pub use apply::{
    apply_plan, discard_journal, recover, render_diff, ApplyReport, ApplyStatus, RecoverReport,
};

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sieve_core::fingerprint::decode_source;
use sieve_core::lang::lang_for_path;
use sieve_core::{Confidence, Kind, Node, Relation};
use tree_sitter::{Node as TsNode, Parser, Tree};

use crate::container::container_lang_of;
use crate::extract::{RawEdge, ReExport};
use crate::refresh::{acquire_refresh_lock, rebuild_keeping, Kept};
use crate::resolve::{ExportIndex, Lookup};
use crate::ts_scope::{
    scope_file, word_matches, Binder, DynShape, FileScopes, ImportBinding, ImportKind,
    Site as ScanSite, SiteKind,
};

/// The path of the write journal under the context dir (design section D).
const JOURNAL_PATH: &str = ".cache/rename-journal.json";

/// Words that a strict-mode identifier cannot be, and names the rename
/// refuses (design S9).
const RESERVED: [&str; 45] = [
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "new",
    "null",
    "return",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
    "let",
    "static",
    "implements",
    "interface",
    "package",
    "private",
    "protected",
    "public",
];

/// More names that the rename refuses (design S9).
const RESERVED_EXTRA: [&str; 3] = ["arguments", "eval", "await"];

/// A failure that stops the plan before it can say "refused".
#[derive(Debug)]
pub enum RenameError {
    /// A file system or build failure.
    Io(io::Error),
    /// The symbol names no node, or more than one node. The text lists the
    /// candidate ids when there are two or more.
    Symbol(String),
    /// Another process holds the build lock.
    LockBusy,
    /// The apply or recover step cannot go on, for example a journal that
    /// does not parse.
    Apply(String),
    /// A restore failed. The journal at `journal` is kept, so `recover` can
    /// try again. `files` lists the repo paths that were not restored.
    RestoreFailed {
        /// Why the rename was rolled back.
        why: String,
        /// The repo-relative paths that were not restored.
        files: Vec<String>,
        /// The journal that was kept.
        journal: PathBuf,
    },
}

impl fmt::Display for RenameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RenameError::Io(err) => write!(f, "rename failed: {err}"),
            RenameError::Symbol(text) => write!(f, "{text}"),
            RenameError::LockBusy => write!(f, "another process holds the build lock"),
            RenameError::Apply(text) => write!(f, "{text}"),
            RenameError::RestoreFailed {
                why,
                files,
                journal,
            } => write!(
                f,
                "{why}; could not restore {}; the journal {} is kept, run recover",
                files.join(", "),
                journal.display()
            ),
        }
    }
}

impl std::error::Error for RenameError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RenameError::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for RenameError {
    fn from(err: io::Error) -> Self {
        RenameError::Io(err)
    }
}

/// The kind of one edit site (plan JSON v1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SiteClass {
    /// The declaration name.
    Declaration,
    /// A value reference that is not a call.
    Reference,
    /// A call.
    Call,
    /// A JSX tag name.
    Jsx,
    /// An `export { name }` clause.
    Export,
    /// An `import { name }` specifier.
    Import,
    /// An `export { name } from` specifier.
    Reexport,
    /// A JSDoc link or type site.
    Jsdoc,
}

/// Why a rename is refused (plan JSON v1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// A site that the scan cannot prove.
    Unproven,
    /// A dynamic module access that reaches the chain.
    DynamicImport,
    /// The new name is already in use where the rename gains it.
    Collision,
    /// An edit site would bind to an existing declaration of the new name.
    Capture,
    /// A construct that slice 1 does not support.
    UnsupportedSite,
    /// A file with a hit that does not parse cleanly.
    ParseError,
    /// A file with a hit that is not an ES module.
    NotModule,
    /// A file changed since the index build.
    StaleIndex,
    /// The target is not a plain top-level function.
    TargetKind,
    /// The specifier names a package that this repo also defines.
    PackageSelfImport,
    /// A write journal is left from a crash.
    JournalPresent,
}

/// One edit: replace the bytes of the old name with the new name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edit {
    /// The first byte, as an offset in the file on disk.
    pub start: usize,
    /// The byte after the last byte.
    pub end: usize,
    /// The line of the first byte, counted from 1.
    pub line: usize,
    /// The UTF-8 byte column of the first byte, counted from 1.
    pub col: usize,
    /// The kind of site.
    pub site: SiteClass,
}

/// The edits of one file, with the hash of its bytes on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanFile {
    /// The repo-relative posix path.
    pub path: String,
    /// The sha256 of the disk bytes, in lowercase hex.
    pub sha256: String,
    /// The edits, in byte order.
    pub edits: Vec<Edit>,
}

/// One cause that refuses the rename.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refusal {
    /// The repo-relative posix path.
    pub path: String,
    /// The line, counted from 1.
    pub line: usize,
    /// The UTF-8 byte column, counted from 1.
    pub col: usize,
    /// The reason class.
    pub reason: Reason,
    /// A short text for the reader.
    pub text: String,
}

/// One hit that the rename lists and never edits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    /// The repo-relative posix path.
    pub path: String,
    /// The line, counted from 1.
    pub line: usize,
    /// The UTF-8 byte column, counted from 1.
    pub col: usize,
    /// The class: `comment`, `string`, `doc`, `other_language` or `inferred`.
    pub class: String,
}

/// The target symbol of a plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    /// The node id.
    pub id: String,
    /// The node kind, such as `function`.
    pub kind: String,
    /// The defining file.
    pub path: String,
    /// The first line of the declaration.
    pub line: u32,
}

/// Whether the plan can be written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Every site is proven.
    Ready,
    /// At least one cause refuses the rename.
    Refused,
}

/// The rename plan, JSON version 1 (design section C).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    /// The plan format version, always 1.
    pub version: u32,
    /// The first 8 hex digits of a sha256 over the target, names, edits and
    /// file hashes.
    pub plan_id: String,
    /// Ready or refused.
    pub status: Status,
    /// The target symbol.
    pub target: Target,
    /// The old name.
    pub old: String,
    /// The new name.
    pub new: String,
    /// The edits per file. Empty when the plan is refused.
    pub files: Vec<PlanFile>,
    /// One row per cause. Empty when the plan is ready.
    pub refused: Vec<Refusal>,
    /// The hits that the rename lists and never edits.
    pub report: Vec<Report>,
}

/// Computes the plan id: the first 8 hex digits of a sha256 over the target
/// id, both names, every edit and the hash of every touched file.
pub fn plan_id(target_id: &str, old: &str, new: &str, files: &[PlanFile]) -> String {
    let mut hasher = Sha256::new();
    for part in [target_id, old, new] {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    for file in files {
        hasher.update(file.path.as_bytes());
        hasher.update([0]);
        hasher.update(file.sha256.as_bytes());
        hasher.update([0]);
        for edit in &file.edits {
            let row = format!("{}:{}:{:?};", edit.start, edit.end, edit.site);
            hasher.update(row.as_bytes());
        }
    }
    let digest = hasher.finalize();
    digest.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

/// Plans the rename of `symbol` to `new_name` in the repo at `root`.
///
/// It takes the build lock, runs a cached build and writes the index, as
/// design section D steps 1 and 2 say. It writes no source file. `in_prefix`
/// narrows the symbol search only, never the scan.
pub fn plan_rename(
    root: &Path,
    context_dir: &Path,
    symbol: &str,
    new_name: &str,
    in_prefix: Option<&str>,
) -> Result<Plan, RenameError> {
    fs::create_dir_all(context_dir)?;
    let lock = acquire_refresh_lock(context_dir)?.ok_or(RenameError::LockBusy)?;
    let kept = rebuild_keeping(root, context_dir)?;
    let plan = plan_from_build(root, context_dir, &kept, symbol, new_name, in_prefix, true);
    drop(lock);
    plan
}

fn plan_from_build(
    root: &Path,
    context_dir: &Path,
    kept: &Kept,
    symbol: &str,
    new_name: &str,
    in_prefix: Option<&str>,
    check_journal: bool,
) -> Result<Plan, RenameError> {
    plan_counted(
        root,
        context_dir,
        kept,
        symbol,
        new_name,
        in_prefix,
        check_journal,
    )
    .map(|(plan, _)| plan)
}

/// What the plan step counted for the apply post-check.
pub(super) struct Counts {
    /// The old-name hits in TS and JS files that the plan classed Other or
    /// Report.
    pub kept_hits: usize,
    /// The (file, line) of each hit that the plan classed Other for a
    /// structural reason: a member property, an object key, a label or a
    /// private name.
    pub structural: Vec<(String, usize)>,
}

/// Plans like [`plan_from_build`] and also returns the counts that the apply
/// post-check needs.
fn plan_counted(
    root: &Path,
    context_dir: &Path,
    kept: &Kept,
    symbol: &str,
    new_name: &str,
    in_prefix: Option<&str>,
    check_journal: bool,
) -> Result<(Plan, Counts), RenameError> {
    let target = resolve_target(&kept.graph.nodes, symbol, in_prefix)?;
    let mut run = Run::new(root, kept, target, new_name);
    // The apply post-check plans while its own journal exists, so it skips
    // this test.
    if check_journal && context_dir.join(JOURNAL_PATH).exists() {
        run.refuse_at_target(
            Reason::JournalPresent,
            "a write journal is left from a crash",
        );
    }
    if run.check_target() && run.check_new_name() {
        run.scan(context_dir)?;
    }
    let counts = Counts {
        kept_hits: run.kept_hits.len(),
        structural: std::mem::take(&mut run.structural),
    };
    Ok((run.finish(), counts))
}

// ---- symbol resolution ----

/// Resolves the symbol text to exactly one node.
fn resolve_target<'a>(
    nodes: &'a [Node],
    symbol: &str,
    in_prefix: Option<&str>,
) -> Result<&'a Node, RenameError> {
    let prefix = in_prefix.map(|p| p.trim_matches('/')).unwrap_or("");
    let under = |n: &Node| {
        prefix.is_empty() || n.path == prefix || n.path.starts_with(&format!("{prefix}/"))
    };
    let real = |n: &&Node| n.kind != Kind::File && under(n);
    let mut found: Vec<&Node> = if symbol.contains('#') {
        nodes
            .iter()
            .filter(real)
            .filter(|n| n.id == symbol)
            .collect()
    } else if let Some((path, line)) = symbol
        .rsplit_once(':')
        .and_then(|(p, l)| l.parse::<u32>().ok().map(|l| (p, l)))
    {
        let in_file: Vec<&Node> = nodes
            .iter()
            .filter(real)
            .filter(|n| n.path == path)
            .collect();
        let starts: Vec<&Node> = in_file
            .iter()
            .copied()
            .filter(|n| span_lines(&n.span).0 == line)
            .collect();
        if starts.is_empty() {
            // The innermost node that holds the line.
            let mut holds: Vec<&Node> = in_file
                .into_iter()
                .filter(|n| {
                    let (a, b) = span_lines(&n.span);
                    a <= line && line <= b
                })
                .collect();
            holds.sort_by_key(|n| {
                let (a, b) = span_lines(&n.span);
                b - a
            });
            holds.truncate(1);
            holds
        } else {
            starts
        }
    } else {
        nodes
            .iter()
            .filter(real)
            .filter(|n| n.name == symbol)
            .collect()
    };
    found.dedup_by_key(|n| n.id.clone());
    match found.as_slice() {
        [one] => Ok(one),
        [] => Err(RenameError::Symbol(format!(
            "no symbol matches \"{symbol}\""
        ))),
        many => {
            let ids: Vec<&str> = many.iter().map(|n| n.id.as_str()).collect();
            Err(RenameError::Symbol(format!(
                "\"{symbol}\" matches {} symbols: {}",
                many.len(),
                ids.join(", ")
            )))
        }
    }
}

/// The first and last line of a node span such as `L12-L20`.
fn span_lines(span: &str) -> (u32, u32) {
    let mut parts = span
        .split('-')
        .map(|p| p.trim_start_matches('L').parse::<u32>().unwrap_or_default());
    let first = parts.next().unwrap_or_default();
    (first, parts.next().unwrap_or(first))
}

// ---- text helpers ----

/// Design S7d: is the word at `at` a whole string in a data or doc file? That
/// is a quoted `"name"` or `'name'`, or a YAML or TOML value that is exactly
/// the name after `: ` or `= `.
fn string_form(text: &str, name: &str, at: usize) -> bool {
    let end = at + name.len();
    let before = text.get(..at).unwrap_or("");
    let after = text.get(end..).unwrap_or("");
    for q in ['"', '\''] {
        if before.ends_with(q) && after.starts_with(q) {
            return true;
        }
    }
    let line_before = before.rsplit('\n').next().unwrap_or("");
    let line_after = after.split('\n').next().unwrap_or("").trim();
    let value = [": ", "= ", ":\t", "=\t"]
        .iter()
        .any(|sep| line_before.ends_with(sep));
    value && (line_after.is_empty() || line_after.starts_with('#'))
}

fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let head = &text.as_bytes()[..offset.min(text.len())];
    let line = head.iter().filter(|&&b| b == b'\n').count() + 1;
    let col = head.iter().rev().take_while(|&&b| b != b'\n').count() + 1;
    (line, col)
}

fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn to_slash(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn unquote(text: &str) -> &str {
    let t = text.trim();
    if t.len() >= 2 && (t.starts_with(['"', '\'', '`'])) {
        &t[1..t.len() - 1]
    } else {
        t
    }
}

fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

fn join_posix(dir: &str, rel: &str) -> String {
    let mut parts: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// True when `name` is a valid identifier (JS ID_Start and ID_Part, as far
/// as the std char classes go).
fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    // Only letters, `$`, `_` and ASCII digits. A non-ASCII number such as
    // `²` is not an identifier part, and the scan cannot tell the Unicode
    // decimal digits from it, so it refuses both.
    let letter = |c: char| c == '$' || c == '_' || (c.is_alphabetic() && !c.is_numeric());
    letter(first) && chars.all(|c| letter(c) || c.is_ascii_digit())
}

fn grammar_of(path: &str) -> Option<&'static str> {
    lang_for_path(Path::new(path))
        .map(|l| l.grammar)
        .filter(|g| matches!(*g, "typescript" | "tsx"))
}

fn is_js_path(path: &str) -> bool {
    [".js", ".jsx", ".mjs", ".cjs"]
        .iter()
        .any(|e| path.ends_with(e))
}

fn is_dts(path: &str) -> bool {
    [".d.ts", ".d.mts", ".d.cts"]
        .iter()
        .any(|e| path.ends_with(e))
}

fn parse(parsers: &mut Parsers, grammar: &str, text: &str) -> Option<Tree> {
    let parser = if grammar == "tsx" {
        &mut parsers.tsx
    } else {
        &mut parsers.ts
    };
    parser.parse(text, None)
}

struct Parsers {
    ts: Parser,
    tsx: Parser,
}

impl Parsers {
    fn new() -> Result<Self, RenameError> {
        let mut ts = Parser::new();
        let mut tsx = Parser::new();
        ts.set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
            .map_err(|e| RenameError::Io(io::Error::other(e.to_string())))?;
        tsx.set_language(&tree_sitter_typescript::LANGUAGE_TSX.into())
            .map_err(|e| RenameError::Io(io::Error::other(e.to_string())))?;
        Ok(Parsers { ts, tsx })
    }
}

/// Every module name in a `declare module "name"` block of the tree.
fn ambient_modules(tree: &Tree, text: &str) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "module" {
            if let Some(name) = node.child_by_field_name("name") {
                if name.kind() == "string" {
                    let raw = text.get(name.byte_range()).unwrap_or("");
                    out.push((unquote(raw).to_string(), name.start_byte()));
                }
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    out
}

// ---- the run ----

/// How a file's module-scope binding of the old name relates to the target.
#[derive(Debug, Clone)]
enum Link {
    /// The defining file. The number is the start of the declaration name.
    Defining(usize),
    /// A plain `import { name }` of the target. The number indexes the import.
    Target(usize),
    /// The module-scope binding is another symbol, or there is none.
    Other,
    /// The scan cannot say.
    Doubt(Reason, String),
}

/// What an import statement resolves to.
enum ImpClass {
    Target,
    Other,
    Doubt(Reason, String),
}

/// The decision for one site.
enum Out {
    Edit(SiteClass, Note),
    Other,
    Report(&'static str),
    Bad(Reason, String),
}

/// A fact about an edit that the later checks use.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Note {
    None,
    /// A plain import: the file gains the new name as a binding.
    PlainImport,
    /// A re-export with no alias: the barrel changes its public name.
    Barrel,
}

/// One scanned file, with the data a site decision needs.
struct Fc<'a> {
    path: &'a str,
    text: &'a str,
    tree: &'a Tree,
    scopes: &'a FileScopes,
    is_js: bool,
}

/// The file bytes of a file that holds a hit.
struct Loaded {
    bytes: Vec<u8>,
    text: String,
    grammar: &'static str,
}

struct Run<'a> {
    root: &'a Path,
    kept: &'a Kept,
    idx: ExportIndex<'a>,
    target: &'a Node,
    old: &'a str,
    new: &'a str,
    chain: HashSet<String>,
    policy: policy::Policy,
    refused: Vec<Refusal>,
    report: Vec<Report>,
    edits: BTreeMap<String, Vec<Edit>>,
    gains_new: BTreeSet<String>,
    barrels: BTreeSet<String>,
    loaded: BTreeMap<String, Loaded>,
    /// The (file, offset) of each TS or JS hit classed Other or Report.
    kept_hits: BTreeSet<(String, usize)>,
    /// The (file, line) of each structural Other hit.
    structural: Vec<(String, usize)>,
    /// Design S13b: the repo paths of the symlinks that do not refuse by
    /// themselves. An import that goes through one is Unproven.
    links: Vec<String>,
    /// For each link in `links`, the repo path of its real target.
    link_real: BTreeMap<String, String>,
    /// Design S7d: the (file, line, column) of each dynamic module access
    /// that the scan cannot trace.
    untraceable: Vec<(String, usize, usize)>,
    /// Design S7d: the (file, line) of the first old-name form that a dynamic
    /// access could read: a property, a destructuring key or a whole string.
    old_form: Option<(String, usize)>,
}

impl<'a> Run<'a> {
    fn new(root: &'a Path, kept: &'a Kept, target: &'a Node, new: &'a str) -> Self {
        let idx = ExportIndex::new(&kept.graph.nodes, &kept.raw);
        let chain = chain_of(&idx, &kept.raw, &target.path);
        Run {
            root,
            kept,
            idx,
            target,
            old: &target.name,
            new,
            chain,
            policy: policy::Policy::default(),
            refused: Vec::new(),
            report: Vec::new(),
            edits: BTreeMap::new(),
            gains_new: BTreeSet::new(),
            barrels: BTreeSet::new(),
            loaded: BTreeMap::new(),
            kept_hits: BTreeSet::new(),
            structural: Vec::new(),
            links: Vec::new(),
            link_real: BTreeMap::new(),
            untraceable: Vec::new(),
            old_form: None,
        }
    }

    fn refuse(&mut self, path: &str, line: usize, col: usize, reason: Reason, text: &str) {
        self.refused.push(Refusal {
            path: path.to_string(),
            line,
            col,
            reason,
            text: text.to_string(),
        });
    }

    fn refuse_at_target(&mut self, reason: Reason, text: &str) {
        let line = span_lines(&self.target.span).0 as usize;
        let path = self.target.path.clone();
        self.refuse(&path, line.max(1), 1, reason, text);
    }

    fn refuse_at_offset(&mut self, path: &str, text: &str, at: usize, reason: Reason, why: &str) {
        let (line, col) = line_col(text, at);
        self.refuse(path, line, col, reason, why);
    }

    fn add_report(&mut self, path: &str, line: usize, col: usize, class: &str) {
        let row = Report {
            path: path.to_string(),
            line,
            col,
            class: class.to_string(),
        };
        if !self.report.contains(&row) {
            self.report.push(row);
        }
    }

    // ---- S1 and S9 name checks ----

    /// Design S1, the part that needs only the graph.
    fn check_target(&mut self) -> bool {
        let t = self.target;
        if t.kind != Kind::Function || t.owner.is_some() || grammar_of(&t.path).is_none() {
            self.refuse_at_target(
                Reason::TargetKind,
                "the target is not a top-level function in a TS or JS file",
            );
            return false;
        }
        let default = self
            .kept
            .raw
            .iter()
            .any(|e| e.default_export && e.target_id.as_deref() == Some(t.id.as_str()));
        if default {
            self.refuse_at_target(Reason::TargetKind, "the target is a default export");
            return false;
        }
        true
    }

    /// Design S9, the part that needs only the names.
    fn check_new_name(&mut self) -> bool {
        let new = self.new;
        let why = if new == self.old {
            Some("the new name equals the old name")
        } else if !is_identifier(new) {
            Some("the new name is not a valid identifier")
        } else if RESERVED.contains(&new) || RESERVED_EXTRA.contains(&new) {
            Some("the new name is a reserved word")
        } else {
            None
        };
        match why {
            Some(text) => {
                self.refuse_at_target(Reason::Collision, text);
                false
            }
            None => true,
        }
    }

    // ---- the scan ----

    fn scan(&mut self, context_dir: &Path) -> Result<(), RenameError> {
        let files: Vec<String> = crate::build::collect_files(self.root, context_dir, None)?
            .iter()
            .map(|p| to_slash(p))
            .collect();
        let mut parsers = Parsers::new()?;
        let mut ambient: HashSet<String> = HashSet::new();
        let mut ambient_rel: Vec<(String, String, usize, String)> = Vec::new();
        let mut hit_files: BTreeSet<String> = BTreeSet::new();
        let mut others: Vec<&String> = Vec::new();
        let mut unindexed: Vec<String> = Vec::new();
        self.symlink_walk(context_dir, &files);

        // Pass 1: read, check the print, probe S7 over every TS and JS file.
        for path in &files {
            let Some(grammar) = grammar_of(path) else {
                if is_markup_name(path) {
                    self.probe_markup(path);
                }
                others.push(path);
                continue;
            };
            let bytes = match fs::read(self.root.join(path)) {
                Ok(b) => b,
                Err(_) => {
                    // The scan cannot read the file, so it cannot rule out a hit.
                    self.refuse(
                        path,
                        1,
                        1,
                        Reason::StaleIndex,
                        "the file cannot be read, so a hit cannot be ruled out",
                    );
                    continue;
                }
            };
            let text = decode_any(&bytes);
            let has_text_hit = word_matches(&text, self.old).next().is_some();
            if bytes.starts_with(&[0xfe, 0xff]) {
                // UTF-16BE: the build skips the file and Sieve cannot parse it.
                if has_text_hit {
                    self.refuse(
                        path,
                        1,
                        1,
                        Reason::ParseError,
                        "a UTF-16BE file holds a hit and cannot be parsed",
                    );
                }
                // The S7 probe still reads the decoded text.
                if let Some(tree) = parse(&mut parsers, grammar, &text) {
                    let scopes = scope_file(&tree, &text, self.old);
                    self.probe_s7(path, &text, &tree, &scopes, has_text_hit);
                }
                continue;
            }
            let Some(print) = self.kept.prints.get(path) else {
                // Not in the index: an `--only-dir` build left it out.
                if has_text_hit {
                    unindexed.push(path.clone());
                }
                continue;
            };
            if print.hash != hex_sha256(text.as_bytes()) {
                self.refuse(
                    path,
                    1,
                    1,
                    Reason::StaleIndex,
                    "the file changed since the build",
                );
                continue;
            }
            let Some(tree) = parse(&mut parsers, grammar, &text) else {
                self.refuse(path, 1, 1, Reason::ParseError, "the file does not parse");
                continue;
            };
            let scopes = scope_file(&tree, &text, self.old);
            let has_hit = has_text_hit;
            self.probe_s7(path, &text, &tree, &scopes, has_hit);
            for (name, at) in ambient_modules(&tree, &text) {
                ambient.insert(name.clone());
                ambient_rel.push((path.clone(), text.clone(), at, name));
            }
            if has_hit || *path == self.target.path {
                hit_files.insert(path.clone());
                self.loaded.insert(
                    path.clone(),
                    Loaded {
                        bytes,
                        text,
                        grammar,
                    },
                );
            }
        }
        if let Some(first) = unindexed.first() {
            let text = format!(
                "{} file(s) with a hit lie outside the index (an --only-dir build)",
                unindexed.len()
            );
            self.refuse(first, 1, 1, Reason::StaleIndex, &text);
        }
        for (path, text, at, name) in &ambient_rel {
            let reaches = name.starts_with('.')
                && self
                    .idx
                    .resolve_import(path, name)
                    .is_some_and(|f| self.chain.contains(&f));
            if reaches {
                self.refuse_at_offset(
                    path,
                    text,
                    *at,
                    Reason::UnsupportedSite,
                    "a declare module block names a chain file",
                );
            }
        }
        self.policy = policy::Policy::load(self.root, &files, ambient);
        if self.policy.preserve_symlinks() {
            self.refuse_kept_links();
        }

        // Pass 2: classify every hit.
        for path in &hit_files {
            let Some(loaded) = self.loaded.remove(path) else {
                continue;
            };
            let Some(tree) = parse(&mut parsers, loaded.grammar, &loaded.text) else {
                continue;
            };
            let scopes = scope_file(&tree, &loaded.text, self.old);
            let fc = Fc {
                path,
                text: &loaded.text,
                tree: &tree,
                scopes: &scopes,
                is_js: is_js_path(path),
            };
            let utf8_ok = std::str::from_utf8(&loaded.bytes)
                .map(|s| s == loaded.text)
                .unwrap_or(false);
            self.classify_file(&fc, utf8_ok);
            self.loaded.insert(path.clone(), loaded);
        }
        if !hit_files.contains(&self.target.path) {
            self.refuse_at_target(Reason::TargetKind, "the defining file was not scanned");
        }
        self.scan_others(&others);
        self.refuse_untraceable();
        self.post_checks(&mut parsers);
        self.symlink_check();
        self.graph_checks();
        Ok(())
    }

    /// Design S7, for a markup file. The scan does not parse it, so any text
    /// of a glob or context call refuses.
    fn probe_markup(&mut self, path: &str) {
        let Ok(bytes) = fs::read(self.root.join(path)) else {
            return;
        };
        let text = decode_any(&bytes);
        // Design S7d: a markup file is not parsed, so any whole word is a hit.
        if self.old_form.is_none() {
            if let Some(at) = word_matches(&text, self.old).next() {
                self.old_form = Some((path.to_string(), line_col(&text, at).0));
            }
        }
        let found = ["import.meta.glob", "require.context", "webpackContext"]
            .iter()
            .find_map(|needle| text.find(needle));
        if let Some(at) = found {
            let (line, col) = line_col(&text, at);
            self.untraceable.push((path.to_string(), line, col));
        }
    }

    /// Design S7d: an untraceable dynamic access refuses only when the repo
    /// also holds an old-name form that the plan does not edit.
    fn refuse_untraceable(&mut self) {
        let Some((hit_path, hit_line)) = self.old_form.clone() else {
            return;
        };
        let accesses = std::mem::take(&mut self.untraceable);
        for (path, line, col) in accesses {
            let why = format!(
                "a dynamic module access at {path}:{line} can reach the chain, and the old name appears as a property, key or string at {hit_path}:{hit_line}"
            );
            self.refuse(&path, line, col, Reason::DynamicImport, &why);
        }
    }

    /// Design S7d: records the first old-name form of a TS or JS file.
    fn note_old_form(&mut self, path: &str, tree: &Tree, scopes: &FileScopes) {
        if self.old_form.is_some() {
            return;
        }
        let root = tree.root_node();
        let form = scopes.sites.iter().find(|s| match s.kind {
            SiteKind::MemberProperty => true,
            SiteKind::String | SiteKind::Template => s.whole,
            // A shorthand `{ f }` counts in an object literal too: the scan
            // keeps the key name there, and it cannot be told from a pattern
            // without the edit decision.
            SiteKind::ShorthandProperty => true,
            SiteKind::ObjectKey => root
                .descendant_for_byte_range(s.start, s.end)
                .is_some_and(|n| n.parent().is_some_and(|p| p.kind() == "pair_pattern")),
            _ => false,
        });
        if let Some(site) = form {
            self.old_form = Some((path.to_string(), site.line));
        }
    }

    /// Design S7, for one file.
    fn probe_s7(
        &mut self,
        path: &str,
        text: &str,
        tree: &Tree,
        scopes: &FileScopes,
        has_hit: bool,
    ) {
        self.note_old_form(path, tree, scopes);
        use crate::ts_scope::DynamicKind::{
            ImportCall, ImportRequire, MetaGlob, RequireCall, RequireContext,
        };
        let flags = &scopes.flags;
        let reaches_chain = |run: &Self, spec: &str| {
            spec.starts_with('.')
                && run
                    .idx
                    .resolve_import(path, spec)
                    .is_some_and(|f| run.chain.contains(&f))
        };
        let star_sources = flags
            .namespace_imports
            .iter()
            .chain(&flags.export_star)
            .chain(&flags.export_namespace);
        for spec in star_sources {
            if let Some(link) = self.link_for_dynamic(path, spec, has_hit) {
                let at = text.find(spec.as_str()).unwrap_or(0);
                let why = format!("the specifier \"{spec}\" resolves through the symlink {link}");
                self.refuse_at_offset(path, text, at, Reason::Unproven, &why);
            }
            if reaches_chain(self, spec) {
                let at = text.find(spec.as_str()).unwrap_or(0);
                self.refuse_at_offset(
                    path,
                    text,
                    at,
                    Reason::UnsupportedSite,
                    "a namespace import or a star export reaches the chain",
                );
            }
        }
        for d in &flags.dynamic {
            let at = d.start;
            if matches!(d.kind, ImportCall | RequireCall | ImportRequire) {
                if let Some(link) = d
                    .literal
                    .as_deref()
                    .and_then(|l| self.link_for_dynamic(path, l, has_hit))
                {
                    let why = format!("a dynamic import resolves through the symlink {link}");
                    self.refuse_at_offset(path, text, at, Reason::Unproven, &why);
                }
            }
            let reach = match d.kind {
                ImportCall | RequireCall | ImportRequire => match &d.literal {
                    Some(l) if reaches_chain(self, l) => Reach::Traced,
                    Some(_) => Reach::No,
                    None => Reach::Untraceable,
                },
                MetaGlob | RequireContext => match &d.shape {
                    DynShape::Glob {
                        patterns: Some(p),
                        plain_options: true,
                    } => glob::glob_reaches(path, p, &self.links, &self.chain),
                    DynShape::Context {
                        dir: Some(dir),
                        shallow,
                        regex_literal: true,
                    } => glob::context_reaches(path, dir, *shallow, &self.links, &self.chain),
                    _ => Reach::Untraceable,
                },
            };
            match reach {
                Reach::No => {}
                Reach::Traced => self.refuse_at_offset(
                    path,
                    text,
                    at,
                    Reason::DynamicImport,
                    "a dynamic module access can reach the chain",
                ),
                Reach::Untraceable => {
                    let (line, col) = line_col(text, at);
                    self.untraceable.push((path.to_string(), line, col));
                }
            }
        }
        let cjs =
            flags.has_module_exports || flags.has_exports_assign || flags.has_export_assignment;
        if cjs && self.chain.contains(path) {
            self.refuse(
                path,
                1,
                1,
                Reason::UnsupportedSite,
                "a chain file uses module.exports, exports or export =",
            );
        }
    }

    // ---- per-file classification ----

    fn classify_file(&mut self, fc: &Fc, utf8_ok: bool) {
        let flags = &fc.scopes.flags;
        debug_assert_eq!(fc.scopes.name, self.old);
        let defining = fc.path == self.target.path;
        let starts: HashSet<usize> = fc.scopes.sites.iter().map(|s| s.start).collect();
        let hits: Vec<usize> = word_matches(fc.text, self.old).collect();
        if defining && !self.check_defining(fc) {
            return;
        }
        // S3: a file that is not clean gives Unproven for every hit.
        let unclean = if !utf8_ok {
            Some((
                Reason::Unproven,
                "the decoded text differs from the disk bytes",
            ))
        } else if flags.has_error {
            Some((Reason::ParseError, "the file has a syntax error"))
        } else if flags.has_with {
            Some((Reason::Unproven, "the file has a with statement"))
        } else if flags.has_direct_eval {
            Some((Reason::Unproven, "the file calls eval"))
        } else if flags.has_identifier_escape {
            Some((Reason::Unproven, "an identifier has a \\u escape"))
        } else {
            None
        };
        if let Some((reason, why)) = unclean {
            for at in hits {
                self.refuse_at_offset(fc.path, fc.text, at, reason, why);
            }
            return;
        }
        for at in &hits {
            if !starts.contains(at) {
                self.refuse_at_offset(
                    fc.path,
                    fc.text,
                    *at,
                    Reason::Unproven,
                    "a word the scan does not model",
                );
            }
        }
        if is_dts(fc.path) {
            let declares = fc
                .scopes
                .module_bindings(self.old)
                .decls
                .iter()
                .any(|d| d.import.is_none())
                || fc
                    .scopes
                    .sites
                    .iter()
                    .any(|s| s.kind == SiteKind::Declaration);
            if declares {
                if let Some(site) = fc.scopes.sites.first() {
                    self.refuse(
                        fc.path,
                        site.line,
                        site.col,
                        Reason::UnsupportedSite,
                        "a .d.ts file declares the name",
                    );
                }
            }
        }
        let link = self.link_of(fc);
        let hit_set: HashSet<usize> = hits.iter().copied().collect();
        for site in &fc.scopes.sites {
            let out = self.decide(fc, &link, site);
            if matches!(out, Out::Other | Out::Report(_)) && hit_set.contains(&site.start) {
                self.kept_hits.insert((fc.path.to_string(), site.start));
                if matches!(out, Out::Other)
                    && matches!(
                        site.kind,
                        SiteKind::MemberProperty
                            | SiteKind::ObjectKey
                            | SiteKind::Label
                            | SiteKind::PrivateName
                    )
                {
                    self.structural.push((fc.path.to_string(), site.line));
                }
            }
            self.apply(fc, site, out);
        }
    }

    /// Design S1, the part that needs the defining file.
    fn check_defining(&mut self, fc: &Fc) -> bool {
        let mb = fc.scopes.module_bindings(self.old);
        if !fc.scopes.flags.is_module {
            self.refuse_at_target(Reason::NotModule, "the defining file is not an ES module");
            return false;
        }
        let (first, last) = span_lines(&self.target.span);
        let decl_ok = match mb.decls.as_slice() {
            [d] if d.import.is_none() => {
                let (line, _) = line_col(fc.text, d.span.start);
                (first as usize..=last as usize).contains(&line)
            }
            _ => false,
        };
        if !decl_ok {
            self.refuse_at_target(
                Reason::TargetKind,
                "the defining file must hold exactly one declaration of the name",
            );
            return false;
        }
        true
    }

    fn link_of(&self, fc: &Fc) -> Link {
        let mb = fc.scopes.module_bindings(self.old);
        if fc.path == self.target.path {
            return mb
                .decls
                .first()
                .map_or(Link::Other, |d| Link::Defining(d.span.start));
        }
        match mb.decls.as_slice() {
            [] => Link::Other,
            [d] if d.import.is_some() => {
                let idx = d.import.unwrap_or_default();
                match fc.scopes.imports.get(idx) {
                    Some(b) => match self.import_class(fc.path, b) {
                        ImpClass::Target => Link::Target(idx),
                        ImpClass::Other => Link::Other,
                        ImpClass::Doubt(r, t) => Link::Doubt(r, t),
                    },
                    None => Link::Doubt(Reason::Unproven, "the import is not modeled".into()),
                }
            }
            many if many.iter().any(|d| d.import.is_some()) => Link::Doubt(
                Reason::Unproven,
                "two module-scope bindings of the name".into(),
            ),
            _ => Link::Other,
        }
    }

    /// Design S5 for one import binding.
    fn import_class(&self, from: &str, b: &ImportBinding) -> ImpClass {
        if b.kind == ImportKind::Named {
            if b.imported.as_deref() == Some(self.old) {
                return self.source_class(from, &b.source);
            }
            return ImpClass::Other;
        }
        if b.source.starts_with('.') {
            if let Some(doubt) = self.link_doubt(from, &b.source) {
                return doubt;
            }
            return match self.idx.resolve_import(from, &b.source) {
                Some(f) if self.chain.contains(&f) => ImpClass::Doubt(
                    Reason::Unproven,
                    "a default or namespace import of a chain module".into(),
                ),
                Some(_) => ImpClass::Other,
                None => ImpClass::Doubt(Reason::Unproven, "no repo file for the specifier".into()),
            };
        }
        match self.policy.classify(&b.source) {
            Ok(()) => ImpClass::Other,
            Err((r, t)) => ImpClass::Doubt(r, t),
        }
    }

    /// Design S5: where does `import { old } from source` point?
    fn source_class(&self, from: &str, source: &str) -> ImpClass {
        if !source.starts_with('.') {
            return match self.policy.classify(source) {
                Ok(()) => ImpClass::Other,
                Err((r, t)) => ImpClass::Doubt(r, t),
            };
        }
        if let Some(doubt) = self.link_doubt(from, source) {
            return doubt;
        }
        let Some(file) = self.idx.resolve_import(from, source) else {
            return ImpClass::Doubt(Reason::Unproven, format!("no repo file for \"{source}\""));
        };
        match self.idx.lookup(&file, self.old) {
            Lookup::One(id) if id == self.target.id => ImpClass::Target,
            Lookup::One(_) => ImpClass::Other,
            other => ImpClass::Doubt(
                Reason::Unproven,
                format!("the export walk of \"{source}\" gives {other:?}"),
            ),
        }
    }

    /// Decides one site (design S4 to S12).
    fn decide(&self, fc: &Fc, link: &Link, site: &ScanSite) -> Out {
        use SiteKind as K;
        let unproven = |t: &str| Out::Bad(Reason::Unproven, t.to_string());
        let linked = !matches!(link, Link::Other);
        let ident_kind = !matches!(
            site.kind,
            K::String
                | K::Template
                | K::Comment
                | K::JsDoc
                | K::JsDocType
                | K::Label
                | K::PrivateName
        );
        if ident_kind && !fc.scopes.flags.is_module {
            return Out::Bad(Reason::NotModule, "the file is not an ES module".into());
        }
        match site.kind {
            K::Label | K::PrivateName | K::ObjectKey => return Out::Other,
            K::Comment => return Out::Report("comment"),
            K::String | K::Template => {
                return if site.whole {
                    unproven("a string holds only the name")
                } else {
                    Out::Report("string")
                };
            }
            K::MemberProperty => return self.member_out(fc, site),
            K::JsDoc | K::JsDocType => {
                // A nested new name must not capture an edited JSDoc site.
                if linked && fc.scopes.declares(self.new) {
                    return unproven("a nested scope declares the new name");
                }
                return self.jsdoc_out(fc, link, site);
            }
            K::ImportSpecifier => return self.import_out(fc, site),
            K::ReExportSpecifier => return self.reexport_out(fc, site),
            _ => {}
        }
        if let Link::Doubt(reason, text) = link {
            return Out::Bad(*reason, text.clone());
        }
        if linked && fc.scopes.declares(self.old) {
            return unproven("a nested scope declares the name");
        }
        if linked && fc.scopes.declares(self.new) {
            return unproven("a nested scope declares the new name");
        }
        let edit_binder = |binder: Binder| match (link, binder) {
            (Link::Defining(d), Binder::Module(span)) => span.start == *d,
            (Link::Target(i), Binder::Import(j)) => *i == j,
            _ => false,
        };
        match site.kind {
            K::Declaration => {
                if ambient_ancestor(fc.tree, site) {
                    return Out::Bad(
                        Reason::UnsupportedSite,
                        "a declare statement for the name".into(),
                    );
                }
                match link {
                    Link::Defining(d) if *d == site.start => {
                        Out::Edit(SiteClass::Declaration, Note::None)
                    }
                    Link::Other => Out::Other,
                    _ => unproven("a second declaration of the name"),
                }
            }
            K::Reference | K::Call | K::JsxOpen | K::JsxClose => {
                if matches!(link, Link::Other) {
                    return Out::Other;
                }
                if !edit_binder(site.binder) {
                    return unproven("the scan cannot bind this use");
                }
                let class = match site.kind {
                    K::Call => SiteClass::Call,
                    K::JsxOpen | K::JsxClose => SiteClass::Jsx,
                    _ => SiteClass::Reference,
                };
                Out::Edit(class, Note::None)
            }
            K::TypePosition => {
                if linked {
                    unproven("a type position names the function")
                } else {
                    Out::Other
                }
            }
            K::ExportClause => match (link, site.binder) {
                (_, Binder::NoBinding) => Out::Other,
                (Link::Other, _) => Out::Other,
                (Link::Defining(_), b) if edit_binder(b) => {
                    Out::Edit(SiteClass::Export, Note::None)
                }
                (Link::Target(_), b) if edit_binder(b) => Out::Bad(
                    Reason::UnsupportedSite,
                    "an export clause re-exports the target".into(),
                ),
                _ => unproven("the scan cannot bind this export clause"),
            },
            K::ExportDefault => match link {
                Link::Defining(_) => {
                    Out::Bad(Reason::TargetKind, "the target is a default export".into())
                }
                Link::Other => Out::Other,
                _ => unproven("a default export of the target"),
            },
            K::ExportAsNamespace => {
                if linked {
                    unproven("an export as namespace statement")
                } else {
                    Out::Other
                }
            }
            K::ShorthandProperty => match link {
                Link::Defining(_) | Link::Target(_) => Out::Bad(
                    Reason::UnsupportedSite,
                    "a shorthand property binds to the target".into(),
                ),
                _ => Out::Other,
            },
            _ => unproven("a site the scan does not model"),
        }
    }

    /// Design S5 and S10, for a token in an import statement.
    fn import_out(&self, fc: &Fc, site: &ScanSite) -> Out {
        // A nested declaration of the name turns the binder of a declaring
        // token into Unknown. The import is still the declaration at this token.
        let import_of = |s: &ScanSite| match s.binder {
            Binder::Import(i) => Some(i),
            _ => fc
                .scopes
                .module_bindings(self.old)
                .decls
                .iter()
                .find(|d| d.span.start == s.start)
                .and_then(|d| d.import),
        };
        let Some(idx) = import_of(site) else {
            return Out::Bad(Reason::Unproven, "an import the scan does not model".into());
        };
        let Some(b) = fc.scopes.imports.get(idx) else {
            return Out::Bad(Reason::Unproven, "an import the scan does not model".into());
        };
        let both_named = b.local == self.old && b.imported.as_deref() == Some(self.old);
        let tokens = fc
            .scopes
            .sites
            .iter()
            .filter(|s| s.kind == SiteKind::ImportSpecifier && import_of(s) == Some(idx))
            .count();
        if both_named && tokens > 1 {
            return Out::Bad(Reason::Unproven, "an import that renames to itself".into());
        }
        let alias_token = b.kind == ImportKind::Named
            && b.local == self.old
            && b.imported.as_deref() != Some(self.old);
        if alias_token {
            return Out::Other;
        }
        match self.import_class(fc.path, b) {
            ImpClass::Target => {
                let note = if both_named {
                    Note::PlainImport
                } else {
                    Note::None
                };
                Out::Edit(SiteClass::Import, note)
            }
            ImpClass::Other => Out::Other,
            ImpClass::Doubt(r, t) => Out::Bad(r, t),
        }
    }

    /// Design K6, for a token in `export { a as b } from`.
    fn reexport_out(&self, fc: &Fc, site: &ScanSite) -> Out {
        let bad = |t: &str| Out::Bad(Reason::Unproven, t.to_string());
        let Some(node) = fc
            .tree
            .root_node()
            .descendant_for_byte_range(site.start, site.end)
        else {
            return bad("a re-export the scan cannot read");
        };
        let Some(parent) = node.parent() else {
            return bad("a re-export the scan cannot read");
        };
        if parent.kind() == "namespace_export" {
            return Out::Other;
        }
        if parent.kind() != "export_specifier" {
            return bad("a re-export the scan cannot read");
        }
        let statement = parent.parent().and_then(|c| c.parent());
        let source = statement
            .and_then(|s| s.child_by_field_name("source"))
            .and_then(|n| fc.text.get(n.byte_range()))
            .map(|t| unquote(t).to_string());
        let Some(source) = source else {
            return bad("a re-export with no source");
        };
        let name = parent.child_by_field_name("name");
        let alias = parent.child_by_field_name("alias");
        let text_of = |n: Option<TsNode>| n.and_then(|n| fc.text.get(n.byte_range()));
        let is_name = name.is_some_and(|n| n.start_byte() == site.start);
        if is_name {
            if text_of(alias) == Some(self.old) {
                return bad("a re-export that renames to itself");
            }
            let class = self.source_class(fc.path, &source);
            return match class {
                ImpClass::Target => {
                    let note = if alias.is_none() {
                        Note::Barrel
                    } else {
                        Note::None
                    };
                    Out::Edit(SiteClass::Reexport, note)
                }
                ImpClass::Other => Out::Other,
                ImpClass::Doubt(r, t) => Out::Bad(r, t),
            };
        }
        // The token is the exported alias. It names a public export, not the target.
        let Some(imported) = text_of(name) else {
            return Out::Other;
        };
        if let Some(file) = self.idx.resolve_import(fc.path, &source) {
            if self.idx.lookup(&file, imported) == Lookup::One(self.target.id.clone()) {
                return bad("an alias that exports the target under the old name");
            }
        }
        Out::Other
    }

    /// Design S8, for `x.name` and qualified names.
    fn member_out(&self, fc: &Fc, site: &ScanSite) -> Out {
        let bad = |t: &str| Out::Bad(Reason::Unproven, t.to_string());
        let Some(node) = fc
            .tree
            .root_node()
            .descendant_for_byte_range(site.start, site.end)
        else {
            return bad("a member the scan cannot read");
        };
        let Some(parent) = node.parent() else {
            return bad("a member the scan cannot read");
        };
        let receiver = match parent.kind() {
            "member_expression" => parent.child_by_field_name("object"),
            _ => parent.named_child(0),
        };
        let Some(mut cur) = receiver else {
            return bad("a member the scan cannot read");
        };
        let root_name = loop {
            match cur.kind() {
                "identifier" => break fc.text.get(cur.byte_range()).map(str::to_string),
                "this" | "super" => return Out::Other,
                "member_expression" | "subscript_expression" | "nested_identifier" => match cur
                    .child_by_field_name("object")
                    .or_else(|| cur.named_child(0))
                {
                    Some(next) => cur = next,
                    None => return bad("a member the scan cannot read"),
                },
                "non_null_expression"
                | "parenthesized_expression"
                | "as_expression"
                | "satisfies_expression" => match cur.named_child(0) {
                    Some(next) => cur = next,
                    None => return bad("a member the scan cannot read"),
                },
                _ => return bad("the receiver of the member is not a name"),
            }
        };
        let Some(root_name) = root_name else {
            return bad("a member the scan cannot read");
        };
        let mb = fc.scopes.module_bindings(&root_name);
        for d in &mb.decls {
            let Some(b) = d.import.and_then(|i| fc.scopes.imports.get(i)) else {
                continue;
            };
            if matches!(b.kind, ImportKind::Namespace | ImportKind::Require) {
                if b.source.starts_with('.') {
                    return bad("the receiver is a namespace import of a repo module");
                }
                if let Err((r, t)) = self.policy.classify(&b.source) {
                    return Out::Bad(r, t);
                }
            }
        }
        Out::Other
    }

    /// Design S11, for a JSDoc site.
    fn jsdoc_out(&self, fc: &Fc, link: &Link, site: &ScanSite) -> Out {
        if site.kind == SiteKind::JsDocType && !fc.is_js {
            return Out::Bad(Reason::Unproven, "a JSDoc type in a TS file".into());
        }
        // The oracle pins only `{typeof name}`. A bare `{name}` is not pinned.
        if site.kind == SiteKind::JsDocType {
            let head = fc.text.get(..site.start).unwrap_or("").trim_end();
            let typeof_form = head
                .strip_suffix("typeof")
                .is_some_and(|h| h.trim_end().ends_with('{'));
            if !typeof_form {
                return Out::Bad(
                    Reason::Unproven,
                    "a bare JSDoc type that no oracle row pins".into(),
                );
            }
        }
        match (link, site.binder) {
            (Link::Defining(d), Binder::Module(span)) if span.start == *d => {
                Out::Edit(SiteClass::Jsdoc, Note::None)
            }
            (Link::Target(i), Binder::Import(j)) if *i == j => {
                Out::Edit(SiteClass::Jsdoc, Note::None)
            }
            (Link::Other, Binder::Module(_) | Binder::Import(_) | Binder::Global) => Out::Other,
            _ => Out::Bad(Reason::Unproven, "a JSDoc site the scan cannot bind".into()),
        }
    }

    fn apply(&mut self, fc: &Fc, site: &ScanSite, out: Out) {
        match out {
            Out::Other => {}
            Out::Report(class) => self.add_report(fc.path, site.line, site.col, class),
            Out::Bad(reason, text) => self.refuse(fc.path, site.line, site.col, reason, &text),
            Out::Edit(class, note) => {
                if fc.text.get(site.start..site.end) != Some(self.old) {
                    self.refuse(
                        fc.path,
                        site.line,
                        site.col,
                        Reason::Unproven,
                        "the bytes differ",
                    );
                    return;
                }
                let list = self.edits.entry(fc.path.to_string()).or_default();
                if list.iter().any(|e| e.start == site.start) {
                    return;
                }
                list.push(Edit {
                    start: site.start,
                    end: site.end,
                    line: site.line,
                    col: site.col,
                    site: class,
                });
                match note {
                    Note::PlainImport => {
                        self.gains_new.insert(fc.path.to_string());
                    }
                    Note::Barrel => {
                        self.barrels.insert(fc.path.to_string());
                    }
                    Note::None => {}
                }
                if class == SiteClass::Declaration {
                    self.gains_new.insert(fc.path.to_string());
                }
            }
        }
    }

    // ---- other files ----

    /// Design S11 for files that are not TS or JS.
    fn scan_others(&mut self, others: &[&String]) {
        for path in others {
            let Ok(bytes) = fs::read(self.root.join(path.as_str())) else {
                continue;
            };
            let Some(text) = decode_source(&bytes) else {
                continue;
            };
            let hits: Vec<usize> = word_matches(&text, self.old).collect();
            if hits.is_empty() {
                continue;
            }
            if self.old_form.is_none() {
                if let Some(at) = hits
                    .iter()
                    .copied()
                    .find(|&at| string_form(&text, self.old, at))
                {
                    self.old_form = Some((path.to_string(), line_col(&text, at).0));
                }
            }
            let lower = path.to_lowercase();
            let module_script = container_lang_of(path).is_some()
                || [".html", ".htm", ".mdx"].iter().any(|e| lower.ends_with(e));
            let source = lang_for_path(Path::new(path.as_str())).is_some()
                || crate::generic::generic_lang_of(path).is_some();
            for at in hits {
                let (line, col) = line_col(&text, at);
                if module_script {
                    self.refuse(
                        path,
                        line,
                        col,
                        Reason::Unproven,
                        "the file can hold a module script",
                    );
                } else if source {
                    self.add_report(path, line, col, "other_language");
                } else {
                    self.add_report(path, line, col, "doc");
                }
            }
        }
    }

    // ---- S9, S13, S6 ----

    fn post_checks(&mut self, parsers: &mut Parsers) {
        let touched: Vec<String> = self.edits.keys().cloned().collect();
        let has_jsx = self
            .edits
            .values()
            .flatten()
            .any(|e| e.site == SiteClass::Jsx);
        if has_jsx && !self.new.starts_with(char::is_uppercase) {
            self.refuse_at_target(
                Reason::Collision,
                "the new name must start with an uppercase letter when a JSX site exists",
            );
        }
        for path in &touched {
            let Some(loaded) = self.loaded.get(path) else {
                continue;
            };
            let Some(tree) = parse(parsers, loaded.grammar, &loaded.text) else {
                continue;
            };
            let text = loaded.text.clone();
            let scopes = scope_file(&tree, &text, self.new);
            let defining = *path == self.target.path;
            let bound = scopes.module_bindings(self.new);
            if !bound.decls.is_empty() {
                let at = bound.decls.first().map_or(0, |d| d.span.start);
                let reason = if defining {
                    Reason::Collision
                } else {
                    Reason::Capture
                };
                let why = if defining {
                    "the new name is already bound in the defining module"
                } else {
                    "the new name is already bound in a file with an edit"
                };
                self.refuse_at_offset(path, &text, at, reason, why);
            }
            if self.gains_new.contains(path) {
                let clash = scopes.sites.iter().find(|s| {
                    !matches!(
                        s.kind,
                        SiteKind::String
                            | SiteKind::Template
                            | SiteKind::Comment
                            | SiteKind::JsDoc
                            | SiteKind::JsDocType
                            | SiteKind::Label
                            | SiteKind::PrivateName
                            | SiteKind::ObjectKey
                            | SiteKind::MemberProperty
                    )
                });
                if let Some(site) = clash {
                    self.refuse(
                        path,
                        site.line,
                        site.col,
                        Reason::Collision,
                        "the new name already appears in a file that gains it",
                    );
                }
            }
        }
        let barrels: Vec<String> = self.barrels.iter().cloned().collect();
        for barrel in barrels {
            if self.idx.lookup(&barrel, self.new) != Lookup::Zero {
                self.refuse(
                    &barrel,
                    1,
                    1,
                    Reason::Collision,
                    "the barrel already exports the new name",
                );
            }
        }
    }

    /// Design S13b, the repo walk. The build walk drops symlinks. A link
    /// refuses only when files behind it escape the scan. A link whose
    /// target the scan reads at its real path is safe. An import that goes
    /// through such a link is Unproven (see `link_crossed`).
    fn symlink_walk(&mut self, context_dir: &Path, files: &[String]) {
        let mut found = Vec::new();
        let includes = sieve_core::walk::read_include_dirs(self.root);
        find_symlinks(self.root, context_dir, "", &includes, &mut found);
        let root = fs::canonicalize(self.root).unwrap_or_else(|_| self.root.to_path_buf());
        let ctx = fs::canonicalize(context_dir).unwrap_or_else(|_| context_dir.to_path_buf());
        let claimed: HashSet<&str> = files.iter().map(String::as_str).collect();
        for (path, text) in found {
            let link = self.root.join(&path);
            match judge_link(&root, &ctx, &includes, &claimed, &link, &text) {
                None => {
                    if let Some(rel) = fs::canonicalize(&link)
                        .ok()
                        .and_then(|l| l.strip_prefix(&root).ok().map(to_slash))
                    {
                        self.link_real.insert(path.clone(), rel);
                    }
                    self.links.push(path);
                }
                Some(why) => {
                    let text = format!("{path} is a symlink {why}; the scan cannot follow it");
                    self.refuse(&path, 1, 1, Reason::UnsupportedSite, &text);
                }
            }
        }
    }

    /// Design S13b: a config keeps link paths in resolution, so the in-repo
    /// exemption is off for every link to a directory or a source file.
    fn refuse_kept_links(&mut self) {
        for path in std::mem::take(&mut self.links) {
            let full = self.root.join(&path);
            let real = self.link_real.get(&path).cloned().unwrap_or_default();
            let dir = fs::metadata(&full).map_or(true, |m| m.is_dir());
            if dir || is_source_name(&path) || is_source_name(&real) {
                let text = format!(
                    "{path} is a symlink and a config sets preserveSymlinks or symlinks: false"
                );
                self.refuse(&path, 1, 1, Reason::UnsupportedSite, &text);
            }
        }
    }

    /// Design S13b rule 3: the in-repo link that the import specifier
    /// passes, if any. The walk checks each step, so `./link/../x` counts.
    fn link_crossed(&self, from: &str, spec: &str) -> Option<String> {
        if self.links.is_empty() || !spec.starts_with('.') {
            return None;
        }
        let mut parts: Vec<&str> = from.split('/').collect();
        parts.pop();
        for seg in spec.split('/') {
            match seg {
                "" | "." => {}
                ".." => {
                    let here = parts.join("/");
                    if self.links.contains(&here) {
                        return Some(here);
                    }
                    parts.pop();
                }
                name => {
                    parts.push(name);
                    let at = parts.join("/");
                    if let Some(l) = self.links.iter().find(|l| **l == at || strip_ext(l) == at) {
                        return Some(l.clone());
                    }
                }
            }
        }
        None
    }

    /// Design S13b rule 3 for a star export or a dynamic import: the link
    /// the specifier crosses, when the file has a hit or the link leads to
    /// a chain file.
    fn link_for_dynamic(&self, from: &str, spec: &str, has_hit: bool) -> Option<String> {
        let link = self.link_crossed(from, spec)?;
        let leads = match self.link_real.get(&link) {
            Some(real) => self
                .chain
                .iter()
                .any(|c| c == real || c.starts_with(&format!("{real}/")) || strip_ext(c) == real),
            None => true,
        };
        (has_hit || leads).then_some(link)
    }

    /// The Unproven doubt for an import that goes through a link.
    fn link_doubt(&self, from: &str, spec: &str) -> Option<ImpClass> {
        let link = self.link_crossed(from, spec)?;
        Some(ImpClass::Doubt(
            Reason::Unproven,
            format!("the specifier \"{spec}\" resolves through the symlink {link}"),
        ))
    }

    /// Design S13: no touched path or ancestor may be a symlink.
    fn symlink_check(&mut self) {
        let touched: Vec<(String, usize, usize)> = self
            .edits
            .iter()
            .filter_map(|(p, e)| e.first().map(|f| (p.clone(), f.line, f.col)))
            .collect();
        for (path, line, col) in touched {
            if path_has_symlink(self.root, &path) {
                self.refuse(
                    &path,
                    line,
                    col,
                    Reason::UnsupportedSite,
                    "the path or an ancestor is a symlink",
                );
            }
        }
    }

    /// Design S6: the graph and the scan must agree.
    fn graph_checks(&mut self) {
        let graph = &self.kept.graph;
        let by_id: HashMap<&str, &Node> = graph.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
        let mut rows: Vec<(String, usize)> = Vec::new();
        for edge in graph.edges.iter().filter(|e| {
            e.target == self.target.id
                && matches!(e.relation, Relation::Calls | Relation::References)
        }) {
            let Some(source) = by_id.get(edge.source.as_str()) else {
                continue;
            };
            match edge.confidence {
                Confidence::Inferred if grammar_of(&source.path).is_none() => {
                    rows.push((source.path.clone(), span_lines(&source.span).0 as usize));
                }
                Confidence::Extracted => {
                    let bound = self.kept.raw.iter().any(|r: &RawEdge| {
                        r.source == edge.source
                            && r.relation == edge.relation
                            && r.specifier.is_some()
                            && r.import_name.is_none()
                            && r.name.as_deref() == Some(self.old)
                    });
                    if !bound {
                        continue;
                    }
                    let (first, last) = span_lines(&source.span);
                    let seen = self.edits.get(&source.path).is_some_and(|list| {
                        list.iter()
                            .any(|e| (first as usize..=last as usize).contains(&e.line))
                    });
                    if !seen {
                        let path = source.path.clone();
                        self.refuse(
                            &path,
                            first as usize,
                            1,
                            Reason::Unproven,
                            "the graph links this node to the target but the scan finds no edit",
                        );
                    }
                }
                _ => {}
            }
        }
        for (path, line) in rows {
            self.add_report(&path, line.max(1), 1, "inferred");
        }
    }

    // ---- the result ----

    fn finish(mut self) -> Plan {
        self.refused
            .sort_by(|a, b| (&a.path, a.line, a.col).cmp(&(&b.path, b.line, b.col)));
        self.refused.dedup();
        self.report
            .sort_by(|a, b| (&a.path, a.line, a.col).cmp(&(&b.path, b.line, b.col)));
        let ready = self.refused.is_empty();
        let mut files = Vec::new();
        if ready {
            for (path, mut edits) in std::mem::take(&mut self.edits) {
                edits.sort_by_key(|e| e.start);
                let sha256 = self
                    .loaded
                    .get(&path)
                    .map(|l| hex_sha256(&l.bytes))
                    .unwrap_or_default();
                files.push(PlanFile {
                    path,
                    sha256,
                    edits,
                });
            }
        }
        let t = self.target;
        Plan {
            version: 1,
            plan_id: plan_id(&t.id, self.old, self.new, &files),
            status: if ready {
                Status::Ready
            } else {
                Status::Refused
            },
            target: Target {
                id: t.id.clone(),
                kind: "function".to_string(),
                path: t.path.clone(),
                line: span_lines(&t.span).0,
            },
            old: self.old.to_string(),
            new: self.new.to_string(),
            files,
            refused: self.refused,
            report: self.report,
        }
    }
}

/// Decodes bytes for the word pre-filter. It reads UTF-16LE and UTF-16BE
/// with a BOM, and UTF-8 with lossy bytes otherwise.
fn decode_any(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xfe, 0xff]) {
        let units: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| u16::from_be_bytes(*p))
            .collect();
        return char::decode_utf16(units)
            .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect();
    }
    decode_source(bytes).unwrap_or_default()
}

/// Walks `rel` under `root` with the build walk's skip rules and lists every
/// symlink as a repo path and the link text. It does not follow links.
fn find_symlinks(
    root: &Path,
    context_dir: &Path,
    rel: &str,
    includes: &[String],
    out: &mut Vec<(String, String)>,
) {
    let dir = if rel.is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel)
    };
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        let child = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        if meta.file_type().is_symlink() {
            let target = fs::read_link(&path)
                .map(|t| t.to_string_lossy().into_owned())
                .unwrap_or_default();
            out.push((child, target));
        } else if meta.is_dir() {
            let skipped =
                sieve_core::walk::SKIP_DIRS.contains(&name.as_str()) && !includes.contains(&name);
            if !skipped && !path.starts_with(context_dir) {
                find_symlinks(root, context_dir, &child, includes, out);
            }
        }
    }
}

/// True when the path names a markup file that the scan does not parse.
fn is_markup_name(path: &str) -> bool {
    let lower = path.to_lowercase();
    [".vue", ".svelte", ".astro", ".html", ".htm", ".mdx"]
        .iter()
        .any(|e| lower.ends_with(e))
}

/// True when the file name is a TS, JS or markup source.
fn is_source_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    [
        ".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs", ".vue", ".svelte", ".astro",
        ".mdx", ".html", ".htm",
    ]
    .iter()
    .any(|e| lower.ends_with(e))
}

/// The path without the extension of its last part.
fn strip_ext(p: &str) -> &str {
    match p.rfind('.') {
        Some(i) if i > 0 && !p[i..].contains('/') => &p[..i],
        _ => p,
    }
}

/// True when a path part is a directory or file that the build walk skips.
fn skipped_part(name: &str, includes: &[String]) -> bool {
    name.starts_with('.')
        || (sieve_core::walk::SKIP_DIRS.contains(&name) && !includes.iter().any(|i| i == name))
}

/// True when the directory holds a source file. It does not follow links,
/// and it skips the parts that the build walk skips.
fn dir_holds_source(dir: &Path, includes: &[String]) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(meta) = fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if meta.is_dir() {
            if !skipped_part(&name, includes) && dir_holds_source(&entry.path(), includes) {
                return true;
            }
        } else if is_source_name(&name) {
            return true;
        }
    }
    false
}

/// True when a source file under `dir` is not in the build file list. It
/// does not follow links, and it skips the parts that the build walk skips.
fn unclaimed_source(dir: &Path, root: &Path, includes: &[String], claimed: &HashSet<&str>) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            if !skipped_part(&name, includes) && unclaimed_source(&path, root, includes, claimed) {
                return true;
            }
        } else if is_source_name(&name) && !meta.file_type().is_symlink() {
            let rel = path.strip_prefix(root).map(to_slash).unwrap_or_default();
            if !claimed.contains(rel.as_str()) {
                return true;
            }
        }
    }
    false
}

/// Follows a chain of links by hand. It gives the text of the last link, or
/// `None` when the chain does not end (a cycle).
fn chain_end(link: &Path) -> Option<String> {
    let mut cur = link.to_path_buf();
    let mut last = String::new();
    for _ in 0..40 {
        let Ok(meta) = fs::symlink_metadata(&cur) else {
            return Some(last);
        };
        if !meta.file_type().is_symlink() {
            return Some(last);
        }
        let Ok(next) = fs::read_link(&cur) else {
            return Some(last);
        };
        last = next.to_string_lossy().into_owned();
        cur = cur.parent().map_or(next.clone(), |p| p.join(&next));
    }
    None
}

/// Design S13b rules 1 and 2 for one link. It gives `None` when the link is
/// safe, else the reason as a phrase. `root` and `ctx` are canonical.
fn judge_link(
    root: &Path,
    ctx: &Path,
    includes: &[String],
    claimed: &HashSet<&str>,
    link: &Path,
    text: &str,
) -> Option<String> {
    let Ok(last) = fs::canonicalize(link) else {
        // A broken link, a cycle, or a target that cannot be read.
        let unknown = fs::canonicalize(link)
            .err()
            .is_some_and(|e| e.kind() != io::ErrorKind::NotFound);
        return match chain_end(link) {
            None => Some("in a cycle".to_string()),
            Some(_) if unknown => Some("whose target cannot be read".to_string()),
            Some(end) => {
                let name = link.file_name().map(|n| n.to_string_lossy().into_owned());
                let named = name.as_deref().is_some_and(is_source_name);
                (named || is_source_name(text) || is_source_name(&end))
                    .then(|| "that is broken and names a TS, JS or markup file".to_string())
            }
        };
    };
    let skipped_inside = match last.strip_prefix(root) {
        Ok(rel) => {
            let hidden = last.starts_with(ctx)
                || rel
                    .components()
                    .any(|c| skipped_part(&c.as_os_str().to_string_lossy(), includes));
            if !hidden {
                // Design S13b rule 2, exact: the build file list must hold
                // every source file behind the link at its real path.
                let missing = if last.is_dir() {
                    unclaimed_source(&last, root, includes, claimed)
                } else {
                    last.file_name()
                        .is_some_and(|n| is_source_name(&n.to_string_lossy()))
                        && !claimed.contains(to_slash(rel).as_str())
                };
                return missing
                    .then(|| "to a TS, JS or markup file that the scan does not read".to_string());
            }
            true
        }
        Err(_) => false,
    };
    if !skipped_inside && root.starts_with(&last) {
        return Some("to a parent of the repo".to_string());
    }
    let holds = if last.is_dir() {
        // A skipped top directory is read whole. Only its children are skipped.
        dir_holds_source(&last, includes)
    } else {
        last.file_name()
            .is_some_and(|n| is_source_name(&n.to_string_lossy()))
    };
    if !holds {
        return None;
    }
    Some(if skipped_inside {
        "into a directory the scan skips, which holds a TS, JS or markup file".to_string()
    } else {
        "outside the repo, to a TS, JS or markup file".to_string()
    })
}

/// True when `rel` or an ancestor of it inside `root` is a symlink, or cannot
/// be read.
fn path_has_symlink(root: &Path, rel: &str) -> bool {
    let mut cur = PathBuf::from(root);
    for part in rel.split('/') {
        cur.push(part);
        let linked = fs::symlink_metadata(&cur).map_or(true, |m| m.file_type().is_symlink());
        if linked {
            return true;
        }
    }
    false
}

/// True when the site sits inside a `declare` statement.
fn ambient_ancestor(tree: &Tree, site: &ScanSite) -> bool {
    let mut cur = tree
        .root_node()
        .descendant_for_byte_range(site.start, site.end);
    while let Some(node) = cur {
        if node.kind() == "ambient_declaration" {
            return true;
        }
        cur = node.parent();
    }
    false
}

/// The files that carry the target to the outside: the defining file and
/// every module that re-exports from a chain file, by the `from` records.
fn chain_of(idx: &ExportIndex, raw: &[RawEdge], defining: &str) -> HashSet<String> {
    let links: Vec<(&str, String)> = raw
        .iter()
        .filter(|e| e.relation == Relation::Imports)
        .filter(|e| matches!(e.reexport, Some(ref r) if !matches!(r, ReExport::Local { .. })))
        .filter_map(|e| {
            let spec = e.specifier.as_deref()?;
            Some((e.file.as_str(), idx.resolve_import(&e.file, spec)?))
        })
        .collect();
    let mut chain: HashSet<String> = HashSet::from([defining.to_string()]);
    loop {
        let before = chain.len();
        for (file, target) in &links {
            if chain.contains(target) {
                chain.insert((*file).to_string());
            }
        }
        if chain.len() == before {
            return chain;
        }
    }
}
