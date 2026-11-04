//! `sieve why --link`: the user's coding agent links one decision record to
//! the code files it bears on. Sieve proposes candidate files and prints a
//! card for each. The agent keeps the files that match. Sieve records the
//! kept files in `.sieve-links.json` at the repo root, and `sieve why <file>`
//! shows them as an "agent link". Sieve calls no model and never writes into
//! a record doc.
//!
//! The candidates come from two lists: the files that the record names
//! (exact mentions), then the best 50 files by a BM25F score over the file
//! path, the symbol names, the comments and the body words.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sieve_core::askindex::{ask_index_path, read_ask_index, tokenize};
use sieve_core::fingerprint::hash_source;
use sieve_core::wiring::{Graph, Kind};

use crate::why::{find_docs, normalize, raw_entries, read_docs, RawEntry, WhyArgs};

/// The sidecar file name, at the repo root. The repo commits it.
pub(crate) const LINKS_FILE: &str = ".sieve-links.json";
/// The sidecar layout version. A reader drops a file with another value.
const LINKS_VERSION: u32 = 1;
/// The most exact-mention files on one page of the candidate list.
const EXACT_TOP: usize = 10;
/// The most BM25F files on one page of the candidate list.
const SCORE_TOP: usize = 50;
/// The agent kept at least this many files of a page, so Sieve shows the next page.
const PAGE_MORE_AT: usize = 3;
/// The last page of one record.
const PAGE_MAX: usize = 3;
/// A folder needs this many ranked files under it to be a card by density.
const FOLDER_MIN_HITS: usize = 3;
/// A folder needs this share of its files among the ranked files to be a card by density.
const FOLDER_MIN_SHARE: f64 = 0.2;
/// A folder that the record names needs this many ranked files under it.
const FOLDER_NAMED_MIN: usize = 2;
/// The most folder cards for one record.
pub(crate) const FOLDER_CARDS: usize = 10;
/// The most files under a folder that the agent may keep.
pub(crate) const FOLDER_MAX_FILES: usize = 20;
/// The exact-mention files that the folder step reads.
const FOLDER_EXACT: usize = 30;
/// The BM25F files that the folder step reads.
const FOLDER_SCORED: usize = 150;
/// The BM25F field weights: path, symbol names, comments, body words.
const FIELD_WEIGHTS: [f64; 4] = [3.0, 2.0, 1.0, 0.5];
/// The BM25 term-frequency saturation.
const BM25_K1: f64 = 1.2;
/// The BM25 length normalization.
const BM25_B: f64 = 0.75;
/// The most symbol names on one card.
const CARD_SYMBOLS: usize = 6;
/// The symbol names on a card when no name shares a word with the record.
const CARD_FALLBACK_SYMBOLS: usize = 4;
/// The most lines on one card.
const CARD_LINES: usize = 3;
/// The most characters of one card line.
const CARD_LINE_CHARS: usize = 180;
/// The most bytes of a file that a card reads.
const CARD_READ_BYTES: u64 = 400_000;
/// How long a writer waits for the lock.
const LOCK_WAIT: Duration = Duration::from_secs(5);

/// The directory names that say little about a topic. The exact-mention pass
/// skips them.
#[rustfmt::skip]
const GENERIC_DIRS: &[&str] = &[
    "src", "lib", "libs", "test", "tests", "spec", "specs", "app", "apps", "core", "common",
    "shared", "utils", "util", "components", "component", "pages", "page", "models", "model",
    "services", "service", "types", "type", "config", "configs", "scripts", "script", "docs",
    "doc", "fixtures", "fixture", "assets", "public", "static", "internal", "pkg", "cmd",
    "bin", "build", "dist", "server", "client", "api", "modules", "module", "packages",
    "package", "features", "feature", "helpers", "helper", "data", "views", "view", "hooks",
    "hook", "styles", "style", "mocks", "mock", "examples", "example", "tools", "tool",
    "crates", "main", "index", "base", "extensions", "extension", "integration", "unit",
    "browser", "studio", "react", "dotnet",
];

/// The words that the candidate step drops, sorted. They are common code
/// words and the words of the record template. The list is the one that the
/// measured setting used, without the two names of this project.
#[rustfmt::skip]
const SKIP_WORDS: &[&str] = &[
    "all", "also", "alternatives", "and", "any", "are", "async", "await", "bool", "but",
    "can", "class", "code", "const", "constraint", "could", "crate", "date", "decision",
    "default", "does", "each", "entry", "enum", "every", "export", "false", "file", "files",
    "for", "from", "function", "get", "had", "has", "have", "how", "impl", "import", "int",
    "interface", "into", "its", "less", "let", "line", "lines", "lost", "match", "may", "mod",
    "more", "most", "must", "mut", "new", "none", "not", "null", "old", "one", "only",
    "other", "out", "over", "per", "private", "pub", "public", "ref", "return", "same", "see",
    "self", "set", "should", "some", "static", "string", "struct", "super", "than", "that",
    "the", "then", "this", "true", "two", "type", "under", "use", "used", "uses", "var",
    "via", "void", "was", "were", "what", "when", "where", "which", "who", "will", "with",
    "would",
];

/// True when the candidate step keeps `token`: at least 3 letters, and not a
/// skip word.
fn keeps(token: &str) -> bool {
    token.len() > 2 && SKIP_WORDS.binary_search(&token).is_err()
}

/// The tokens of `text` that the candidate step keeps.
fn words(text: &str) -> Vec<String> {
    let mut out = tokenize(text);
    out.retain(|t| keeps(t));
    out
}

/// The rule text that the agent reads before the cards.
const INSTRUCTION: &str = concat!(
    "You link one design record (an ADR or a decision log entry) to the code files it bears on. ",
    "You judge each candidate file on its own with this rule only:\n",
    "A record bears on a file when it names the file or a symbol in it in a role that matters, ",
    "or states a behaviour, limit or rule that the file implements, so an edit to the file can ",
    "repeat or contradict it. Repo-wide process rules and crate-name mentions do not count. ",
    "A repo-wide rule, process note or audit state counts only when the file holds the exact ",
    "flag, symbol or behaviour that the record names.\n",
    "A tech-stack choice or a general convention does not make a file that only uses it a match.\n",
    "Three cases do not match:\n",
    "- an experiment, benchmark, prototype or one-off script that tries out the idea;\n",
    "- a file that only follows a pattern that the record sets for a whole class of files ",
    "(every page, every test, every handler), when the record does not name the file or a ",
    "symbol in it;\n",
    "- a shared config, registry or module-setup file, unless it holds the exact key, flag or ",
    "registration that the record names.\n",
    "A caller that handles the decided case, a test that asserts the decided behaviour, and a ",
    "type that carries the decided data do match. Most candidates do not match.\n",
    "Decide from the cards. Keep a file only when you can name the symbol, flag or behaviour in it."
);

// ---------------------------------------------------------------------------
// Record keys and hashes
// ---------------------------------------------------------------------------

/// The first 8 hex digits of the sha256 of `text`.
fn hash8(text: &str) -> String {
    hash_source(text.as_bytes())[..8].to_string()
}

/// The text hash of a record: it ignores case, commas, dashes and spacing.
pub(crate) fn text_hash(text: &str) -> String {
    hash8(&normalize(text))
}

/// The key of each record: `<doc>#<heading>`. A heading that repeats in one
/// doc gets ` #2`, ` #3` in doc order. The key holds no line number, so an
/// edit above a record never moves its key.
pub(crate) fn record_keys(raw: &[RawEntry]) -> Vec<String> {
    let mut seen: HashMap<String, usize> = HashMap::with_capacity(raw.len());
    raw.iter()
        .map(|e| {
            let base = format!("{}#{}", e.doc, e.heading);
            let n = seen.entry(base.clone()).or_insert(0);
            *n += 1;
            if *n == 1 {
                base
            } else {
                format!("{base} #{n}")
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The sidecar
// ---------------------------------------------------------------------------

/// One reviewed record in `.sieve-links.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LinkEntry {
    /// The record key.
    pub key: String,
    /// The text hash of the record when the agent reviewed it.
    pub text: String,
    /// The hash of the candidate paths of the proposal.
    pub candidates: String,
    /// The review date `YYYY-MM-DD`.
    pub date: String,
    /// The kept graph file paths. Empty means the agent kept nothing.
    pub kept: Vec<String>,
    /// The pages of candidates that the agent reviewed. An old entry has 1.
    #[serde(default = "first_page", skip_serializing_if = "is_first_page")]
    pub pages: u32,
    /// True when the agent kept enough files for the next page to be due.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub more: bool,
    /// The kept folders: every file under one has the record as a link.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub folders: Vec<KeptFolder>,
}

/// A folder that the agent kept, with its file count at that time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct KeptFolder {
    /// The folder path, without a trailing slash.
    pub path: String,
    /// The files under the folder when the agent kept it.
    pub files: usize,
}

/// The page count of an entry that has no `pages` field.
fn first_page() -> u32 {
    1
}

fn is_first_page(n: &u32) -> bool {
    *n == 1
}

/// The first line of the sidecar.
#[derive(Debug, Serialize, Deserialize)]
struct LinksHead {
    version: u32,
}

/// Parses the sidecar: a head line with the version, then one record per
/// line. It gives `None` for a file of another version and for a damaged line.
fn parse_links(bytes: &[u8]) -> Option<Vec<LinkEntry>> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let head: LinksHead = serde_json::from_str(lines.next()?).ok()?;
    if head.version != LINKS_VERSION {
        return None;
    }
    lines.map(|l| serde_json::from_str(l).ok()).collect()
}

/// Reads the sidecar. A missing file, a damaged file and a file of another
/// version all give no entry.
pub(crate) fn read_links(root: &Path) -> Vec<LinkEntry> {
    std::fs::read(root.join(LINKS_FILE))
        .ok()
        .and_then(|bytes| parse_links(&bytes))
        .unwrap_or_default()
}

/// The sidecar entry of each record, when the entry key matches and the text
/// hash equals the hash of the record now. A record whose text changed after
/// the review has no entry here.
pub(crate) fn confirmed<'a>(
    raw: &[RawEntry],
    keys: &[String],
    links: &'a [LinkEntry],
) -> Vec<Option<&'a LinkEntry>> {
    if links.is_empty() {
        return vec![None; raw.len()];
    }
    let by_key: HashMap<&str, &LinkEntry> = links.iter().map(|l| (l.key.as_str(), l)).collect();
    raw.iter()
        .zip(keys)
        .map(|(e, k)| {
            by_key
                .get(k.as_str())
                .copied()
                .filter(|l| l.text == text_hash(&e.text))
        })
        .collect()
}

/// Writes the sidecar whole: a head line with the version, then one record
/// per line, sorted by key. No line ends in a comma and no line depends on
/// its neighbours, so a new record adds a line and changes no other line. A
/// diff and a merge then touch only the lines of the records they change. The
/// write goes through a temp file and a rename.
fn write_links(root: &Path, records: &[LinkEntry]) -> Result<(), String> {
    let path = root.join(LINKS_FILE);
    // A file that Sieve cannot read must not be replaced.
    if let Ok(bytes) = std::fs::read(&path) {
        if parse_links(&bytes).is_none() {
            return Err(format!(
                "{LINKS_FILE} has another version or is damaged; Sieve left it as it was"
            ));
        }
    }
    let mut text = format!("{{\"version\":{LINKS_VERSION}}}\n");
    for r in records {
        text.push_str(&serde_json::to_string(r).map_err(|e| e.to_string())?);
        text.push('\n');
    }
    let tmp = root.join(format!("{LINKS_FILE}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

/// Replaces the entry of `entry.key`, or adds it, and writes the sidecar.
/// The caller holds the lock.
fn put_entry(root: &Path, entry: LinkEntry) -> Result<(), String> {
    let mut records = read_links(root);
    records.retain(|r| r.key != entry.key);
    records.push(entry);
    records.sort_by(|a, b| a.key.cmp(&b.key));
    write_links(root, &records)
}

/// A lock older than this many seconds is taken over.
const LOCK_STALE_SECS: u64 = 60;

/// The write lock: a file made with `create_new` that holds the pid and the
/// start time of its owner. It goes when the guard drops. A waiter takes over
/// a lock whose owner is gone or whose age is over `LOCK_STALE_SECS`.
struct Lock(PathBuf);

/// True when the process `pid` runs.
fn pid_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The lock file text, or `None` when the lock is not stale. A file that
/// holds no readable owner is as old as its modify time.
fn stale_lock(path: &Path, now: u64) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut parts = text.split_whitespace();
    let owner = parts
        .next()
        .and_then(|p| p.parse::<u32>().ok())
        .zip(parts.next().and_then(|t| t.parse::<u64>().ok()));
    let stale = match owner {
        Some((pid, started)) => now.saturating_sub(started) > LOCK_STALE_SECS || !pid_alive(pid),
        None => std::fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age.as_secs() > LOCK_STALE_SECS),
    };
    stale.then_some(text)
}

impl Lock {
    /// Takes the lock under `context_dir`. It waits up to `wait`.
    fn take(context_dir: &Path, wait: Duration) -> Result<Lock, String> {
        let path = context_dir.join(".cache").join("why-links.lock");
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let start = Instant::now();
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    let now = crate::telemetry::now_secs();
                    let _ = writeln!(file, "{} {now}", std::process::id());
                    return Ok(Lock(path));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // ponytail: two waiters can both see a stale lock; the
                    // loser of the create_new race simply waits again.
                    // Upgrade to an atomic rename if this ever shows.
                    if let Some(seen) = stale_lock(&path, crate::telemetry::now_secs()) {
                        if std::fs::read_to_string(&path).is_ok_and(|now| now == seen) {
                            let _ = std::fs::remove_file(&path);
                        }
                        continue;
                    }
                    if start.elapsed() >= wait {
                        return Err(format!(
                            "another sieve why --link write holds the lock: {}; delete it if no write runs",
                            path.display()
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => return Err(e.to_string()),
            }
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

// ---------------------------------------------------------------------------
// The candidate step
// ---------------------------------------------------------------------------

/// The files of the graph with the lookups of the candidate step.
struct Corpus<'g> {
    /// The file paths, sorted.
    paths: Vec<&'g str>,
    /// The symbol names of each file, in graph order.
    names: Vec<Vec<&'g str>>,
    /// The files that define a symbol name.
    defs: HashMap<&'g str, Vec<usize>>,
    /// The files with a base name.
    base: HashMap<&'g str, Vec<usize>>,
    /// The files with a base name up to the first dot.
    stem: HashMap<&'g str, Vec<usize>>,
    /// The files below a directory name. A file is in the list once for each
    /// time its path holds the name.
    dirs: HashMap<&'g str, Vec<usize>>,
    /// The id of each term.
    term: HashMap<String, u32>,
    /// The inverse document frequency of each term id.
    idf: Vec<f64>,
    /// For each term id, the files that hold it with the BM25F weight.
    post: Vec<Vec<(u32, f64)>>,
}

/// Gives the id of `token`, and adds it when new.
fn intern(map: &mut HashMap<String, u32>, token: &str) -> u32 {
    if let Some(&id) = map.get(token) {
        return id;
    }
    let id = map.len() as u32;
    map.insert(token.to_string(), id);
    id
}

impl<'g> Corpus<'g> {
    /// Builds the lookups and the BM25F postings from `graph`, the ask index
    /// and the comment bags under `context_dir`. A missing ask index or
    /// comment bag leaves that field empty.
    fn build(context_dir: &Path, graph: &'g Graph) -> Corpus<'g> {
        let mut paths: Vec<&str> = graph
            .nodes
            .iter()
            .filter(|n| n.kind == Kind::File)
            .map(|n| n.path.as_str())
            .collect();
        paths.sort_unstable();
        paths.dedup();
        let at: HashMap<&str, usize> = paths.iter().enumerate().map(|(i, p)| (*p, i)).collect();
        let n = paths.len();

        let mut names: Vec<Vec<&str>> = vec![Vec::new(); n];
        let mut defs: HashMap<&str, Vec<usize>> = HashMap::new();
        let mut node_file: HashMap<&str, usize> = HashMap::with_capacity(graph.nodes.len());
        for node in &graph.nodes {
            let Some(&i) = at.get(node.path.as_str()) else {
                continue;
            };
            node_file.insert(node.id.as_str(), i);
            if node.kind != Kind::File {
                names[i].push(node.name.as_str());
                defs.entry(node.name.as_str()).or_default().push(i);
            }
        }
        for files in defs.values_mut() {
            files.sort_unstable();
            files.dedup();
        }
        let mut base: HashMap<&str, Vec<usize>> = HashMap::new();
        let mut stem: HashMap<&str, Vec<usize>> = HashMap::new();
        let mut dirs: HashMap<&str, Vec<usize>> = HashMap::new();
        for (i, p) in paths.iter().enumerate() {
            let mut parts: Vec<&str> = p.split('/').collect();
            let name = parts.pop().unwrap_or("");
            base.entry(name).or_default().push(i);
            stem.entry(name.split('.').next().unwrap_or(""))
                .or_default()
                .push(i);
            for d in parts {
                dirs.entry(d).or_default().push(i);
            }
        }

        // The four fields of each file: term id and count pairs.
        let mut term: HashMap<String, u32> = HashMap::new();
        let mut fields: Vec<[Vec<(u32, u32)>; 4]> = vec![Default::default(); n];
        for (i, p) in paths.iter().enumerate() {
            // The path words count twice, as the measured setting did.
            for t in words(p) {
                let id = intern(&mut term, &t);
                fields[i][0].push((id, 2));
            }
            for name in &names[i] {
                for t in words(name) {
                    let id = intern(&mut term, &t);
                    fields[i][1].push((id, 1));
                }
            }
        }
        if let Some(bags) = sieve_parse::read_comment_bags(context_dir) {
            for (path, bag) in &bags {
                if let Some(&i) = at.get(path.as_str()) {
                    for (t, c) in bag.iter().filter(|(t, _)| keeps(t)) {
                        let id = intern(&mut term, t);
                        fields[i][2].push((id, *c));
                    }
                }
            }
        }
        // The file node holds no body of its own. The body words of a file
        // are those of its nodes.
        if let Some(index) = read_ask_index(&ask_index_path(context_dir)) {
            for doc in &index.docs {
                if let Some(&i) = node_file.get(doc.id.as_str()) {
                    for (t, c) in doc.body.iter().filter(|(t, _)| keeps(t)) {
                        let id = intern(&mut term, t);
                        fields[i][3].push((id, *c));
                    }
                }
            }
        }

        let mut avg = [0.0_f64; 4];
        for f in &fields {
            for (k, pairs) in f.iter().enumerate() {
                avg[k] += pairs.iter().map(|p| f64::from(p.1)).sum::<f64>();
            }
        }
        for a in &mut avg {
            *a = (*a / n.max(1) as f64).max(1e-9);
        }
        let mut df = vec![0_u32; term.len()];
        let mut post: Vec<Vec<(u32, f64)>> = vec![Vec::new(); term.len()];
        let mut tf: HashMap<u32, f64> = HashMap::new();
        for (i, f) in fields.iter().enumerate() {
            tf.clear();
            for (k, pairs) in f.iter().enumerate() {
                let len: f64 = pairs.iter().map(|p| f64::from(p.1)).sum();
                let norm = 1.0 - BM25_B + BM25_B * len / avg[k];
                for &(id, c) in pairs {
                    *tf.entry(id).or_insert(0.0) += FIELD_WEIGHTS[k] * f64::from(c) / norm;
                }
            }
            for (&id, &x) in &tf {
                df[id as usize] += 1;
                post[id as usize].push((i as u32, x));
            }
        }
        let idf: Vec<f64> = df
            .iter()
            .map(|&d| {
                let d = f64::from(d);
                (1.0 + (n as f64 - d + 0.5) / (d + 0.5)).ln()
            })
            .collect();
        for (id, list) in post.iter_mut().enumerate() {
            for entry in list.iter_mut() {
                entry.1 = idf[id] * entry.1 / (BM25_K1 + entry.1);
            }
        }
        Corpus {
            paths,
            names,
            defs,
            base,
            stem,
            dirs,
            term,
            idf,
            post,
        }
    }

    /// The idf of a token, or 0 for a token no file holds.
    fn idf_of(&self, token: &str) -> f64 {
        self.term
            .get(token)
            .map_or(0.0, |&id| self.idf[id as usize])
    }

    /// Sorts `(file, score)` by score, highest first. The path breaks a tie.
    fn sorted(&self, scores: impl Iterator<Item = (usize, f64)>) -> Vec<(usize, f64)> {
        let mut out: Vec<(usize, f64)> = scores.collect();
        out.sort_by(|a, b| {
            b.1.total_cmp(&a.1)
                .then_with(|| self.paths[a.0].cmp(self.paths[b.0]))
        });
        out
    }

    /// The BM25F score of every file that shares a word with the record. The
    /// heading words count two more times.
    fn bm25f(&self, text: &str, heading: &str) -> Vec<(usize, f64)> {
        let mut query: HashMap<String, u32> = HashMap::new();
        for t in words(text) {
            *query.entry(t).or_insert(0) += 1;
        }
        let title: HashSet<String> = words(heading).into_iter().collect();
        for t in title {
            *query.entry(t).or_insert(0) += 2;
        }
        let mut acc: HashMap<usize, f64> = HashMap::new();
        for (t, c) in &query {
            let Some(&id) = self.term.get(t) else {
                continue;
            };
            let weight = (1.0 + f64::from(*c).ln()) * self.idf[id as usize];
            for &(file, s) in &self.post[id as usize] {
                *acc.entry(file as usize).or_insert(0.0) += weight * s;
            }
        }
        self.sorted(acc.into_iter())
    }

    /// The files that the record text names, with a score.
    fn exact(&self, text: &str) -> Vec<(usize, f64)> {
        let mut out: HashMap<usize, f64> = HashMap::new();
        let low = text.to_lowercase();
        let words = idents(text);
        let backticked: HashSet<&str> = text
            .split('`')
            .skip(1)
            .step_by(2)
            .flat_map(idents)
            .collect();

        for (i, p) in self.paths.iter().enumerate() {
            if text.contains(p) {
                *out.entry(i).or_insert(0.0) += 100.0;
            }
        }
        let mut pathlike: HashSet<&str> = HashSet::new();
        pathlike.extend(slash_runs(text));
        pathlike.extend(dotted_names(text));
        for token in pathlike {
            let pl = token.trim_matches(|c| c == '.' || c == '/');
            if pl.contains('/') && pl.chars().count() > 6 {
                let inner = format!("/{pl}/");
                let hits: Vec<usize> = self
                    .paths
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| {
                        p.ends_with(pl) || p.starts_with(&inner[1..]) || p.contains(&inner)
                    })
                    .map(|(i, _)| i)
                    .collect();
                let share = 20.0 / (hits.len().max(1) as f64).sqrt();
                for &i in hits.iter().take(200) {
                    *out.entry(i).or_insert(0.0) += share;
                }
            } else if let Some(files) = self.base.get(pl) {
                for &i in files {
                    *out.entry(i).or_insert(0.0) += 10.0 / files.len() as f64;
                }
            }
        }
        for w in &words {
            if w.len() >= 6 && mixed_case(w) {
                if let Some(files) = self.stem.get(w).filter(|f| f.len() <= 5) {
                    for &i in files {
                        *out.entry(i).or_insert(0.0) += 8.0 / files.len() as f64;
                    }
                }
            }
            if let Some(files) = self.defs.get(w) {
                let named = w.contains('_') || lower_upper(w) || backticked.contains(w);
                if w.len() >= 4 && files.len() <= 5 && named {
                    for &i in files {
                        *out.entry(i).or_insert(0.0) += 5.0 / files.len() as f64;
                    }
                }
            }
        }
        for (dir, files) in &self.dirs {
            let name = dir.to_lowercase();
            if name.chars().count() < 5
                || GENERIC_DIRS.contains(&name.as_str())
                || files.len() > 400
            {
                continue;
            }
            if has_word(&low, &name) {
                let share = 3.0 / (files.len() as f64).sqrt();
                for &i in files {
                    *out.entry(i).or_insert(0.0) += share;
                }
            }
        }
        self.sorted(out.into_iter())
    }

    /// The candidate files of page `page` of a record. Page `j` holds exact
    /// ranks `(j-1)*10..j*10`, then BM25F ranks `(j-1)*50..j*50`. A file
    /// shows once, on its first page. The order is the proposal order.
    fn candidates(&self, rec: &RawEntry, page: usize) -> Vec<usize> {
        let exact = self.exact(&rec.text);
        let scored = self.bm25f(&rec.text, &rec.heading);
        pages_of(&exact, &scored, page).pop().unwrap_or_default()
    }

    /// The folders where the ranked files of a record cluster, or that the
    /// record names. A folder with over `FOLDER_MAX_FILES` files is never
    /// returned: the agent may not keep it.
    fn folders(
        &self,
        rec: &RawEntry,
        exact: &[(usize, f64)],
        scored: &[(usize, f64)],
    ) -> Vec<Folder> {
        let mut size: HashMap<&str, usize> = HashMap::new();
        for p in &self.paths {
            for d in dir_prefixes(p) {
                *size.entry(d).or_insert(0) += 1;
            }
        }
        let mut rank: HashMap<usize, f64> = HashMap::new();
        for (list, top) in [(exact, FOLDER_EXACT), (scored, FOLDER_SCORED)] {
            for (i, (f, _)) in list.iter().take(top).enumerate() {
                let r = i as f64 / top as f64;
                let slot = rank.entry(*f).or_insert(r);
                *slot = slot.min(r);
            }
        }
        let mut under: HashMap<&str, Vec<usize>> = HashMap::new();
        for &f in rank.keys() {
            for d in dir_prefixes(self.paths[f]) {
                under.entry(d).or_default().push(f);
            }
        }
        let low = rec.text.to_lowercase();
        // (hits, named) of each folder that qualifies.
        let mut cand: HashMap<&str, (usize, bool)> = HashMap::new();
        for (d, hits) in &under {
            let seg = d.rsplit('/').next().unwrap_or(d).to_lowercase();
            let named = (d.contains('/') && low.contains(&d.to_lowercase()))
                || (seg.chars().count() >= 5
                    && !GENERIC_DIRS.contains(&seg.as_str())
                    && has_word(&low, &seg));
            let dense = hits.len() >= FOLDER_MIN_HITS
                && hits.len() as f64 / size[d] as f64 >= FOLDER_MIN_SHARE;
            if (named && hits.len() >= FOLDER_NAMED_MIN) || dense {
                cand.insert(d, (hits.len(), named));
            }
        }
        // A folder that adds no ranked file over a deeper folder is dropped.
        let mut keep: Vec<&str> = cand
            .keys()
            .copied()
            .filter(|d| {
                !cand.iter().any(|(e, v)| {
                    e.len() > d.len()
                        && e.starts_with(*d)
                        && e[d.len()..].starts_with('/')
                        && v.0 == cand[d].0
                })
            })
            .collect();
        keep.sort_by(|a, b| {
            let (x, y) = (cand[a], cand[b]);
            y.1.cmp(&x.1).then(y.0.cmp(&x.0)).then(a.cmp(b))
        });
        keep.truncate(FOLDER_CARDS);
        keep.into_iter()
            .filter(|d| size[d] <= FOLDER_MAX_FILES)
            .map(|d| {
                let mut top = under[d].clone();
                top.sort_by(|a, b| rank[a].total_cmp(&rank[b]).then(a.cmp(b)));
                top.truncate(4);
                Folder {
                    path: d.to_string(),
                    files: size[d],
                    hits: cand[d].0,
                    top,
                }
            })
            .collect()
    }
}

/// The candidate lists of pages `1..=upto`. A file shows once, on its first page.
fn pages_of(exact: &[(usize, f64)], scored: &[(usize, f64)], upto: usize) -> Vec<Vec<usize>> {
    let mut seen = HashSet::new();
    (1..=upto)
        .map(|j| {
            let e = exact.iter().skip((j - 1) * EXACT_TOP).take(EXACT_TOP);
            let s = scored.iter().skip((j - 1) * SCORE_TOP).take(SCORE_TOP);
            e.chain(s)
                .map(|c| c.0)
                .filter(|i| seen.insert(*i))
                .collect()
        })
        .collect()
}

/// The directory prefixes of a file path: `a/b/c.rs` gives `a`, `a/b`.
fn dir_prefixes(path: &str) -> impl Iterator<Item = &str> {
    path.match_indices('/').map(move |(at, _)| &path[..at])
}

/// The ASCII identifiers of `text`.
fn idents(text: &str) -> HashSet<&str> {
    let b = text.as_bytes();
    let mut out = HashSet::new();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_alphabetic() || b[i] == b'_' {
            let start = i;
            i += 1;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            out.insert(&text[start..i]);
        } else {
            i += 1;
        }
    }
    out
}

/// True when `w` holds a lowercase letter followed by an uppercase letter.
fn lower_upper(w: &str) -> bool {
    w.as_bytes()
        .windows(2)
        .any(|p| p[0].is_ascii_lowercase() && p[1].is_ascii_uppercase())
}

/// True when `w` holds an underscore, a lower-to-upper step, or two capitals
/// and a lowercase letter.
fn mixed_case(w: &str) -> bool {
    w.contains('_')
        || lower_upper(w)
        || w.as_bytes().windows(3).any(|p| {
            p[0].is_ascii_uppercase() && p[1].is_ascii_uppercase() && p[2].is_ascii_lowercase()
        })
}

/// True for a letter, a digit or an underscore.
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The runs of the form `a/b/c`, where each part holds letters, digits and
/// `_ @ . -`. A run has at least one slash.
fn slash_runs(text: &str) -> Vec<&str> {
    let part = |c: char| is_word(c) || matches!(c, '@' | '.' | '-');
    let cs: Vec<(usize, char)> = text.char_indices().collect();
    let at = |k: usize| cs.get(k).map_or(text.len(), |c| c.0);
    let mut out = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        if !part(cs[i].1) {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < cs.len() && part(cs[j].1) {
            j += 1;
        }
        let mut end = j;
        let mut slashes = 0;
        while end + 1 < cs.len() && cs[end].1 == '/' && part(cs[end + 1].1) {
            end += 1;
            while end < cs.len() && part(cs[end].1) {
                end += 1;
            }
            slashes += 1;
        }
        if slashes > 0 {
            out.push(&text[at(i)..at(end)]);
            i = end;
        } else {
            i = j;
        }
    }
    out
}

/// The names of the form `name.ext`, where `ext` holds 1 to 5 letters,
/// digits or underscores.
fn dotted_names(text: &str) -> Vec<&str> {
    let cs: Vec<(usize, char)> = text.char_indices().collect();
    let at = |k: usize| cs.get(k).map_or(text.len(), |c| c.0);
    let mut out = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        if !(is_word(cs[i].1) || cs[i].1 == '-') {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < cs.len() && (is_word(cs[j].1) || cs[j].1 == '-') {
            j += 1;
        }
        let mut next = j;
        if j < cs.len() && cs[j].1 == '.' {
            let mut k = j + 1;
            while k < cs.len() && is_word(cs[k].1) {
                k += 1;
            }
            let first = (i..j).find(|&x| is_word(cs[x].1));
            if let (Some(s), 1..=5) = (first, k - (j + 1)) {
                out.push(&text[at(s)..at(k)]);
                next = k;
            }
        }
        i = next;
    }
    out
}

/// True when `name` is in `low` as a whole word. A letter, a digit, `_` or
/// `-` may not follow it, and a letter, a digit, `_`, `.` or `-` may not
/// precede it.
fn has_word(low: &str, name: &str) -> bool {
    low.match_indices(name).any(|(at, _)| {
        let before = low[..at].chars().next_back();
        let after = low[at + name.len()..].chars().next();
        !before.is_some_and(|c| is_word(c) || c == '.' || c == '-')
            && !after.is_some_and(|c| is_word(c) || c == '-')
    })
}

// ---------------------------------------------------------------------------
// The proposal
// ---------------------------------------------------------------------------

/// One card: a candidate file with its symbols and its best lines.
struct Card {
    n: usize,
    path: String,
    symbols: Vec<String>,
    lines: Vec<(usize, String)>,
}

/// One folder card: a folder where the candidates of a record cluster.
struct Folder {
    path: String,
    /// The files under the folder.
    files: usize,
    /// The ranked files under the folder.
    hits: usize,
    /// The best ranked files under the folder.
    top: Vec<usize>,
}

/// The candidate files of one record, with the hashes that bind a write to
/// the proposal that the agent read.
struct Proposal {
    page: usize,
    files: Vec<usize>,
    folders: Vec<Folder>,
    link_id: String,
    candidate_hash: String,
    text_hash: String,
}

impl Corpus<'_> {
    /// The proposal of page `page` of a record. Page 1 also holds the folder
    /// cards when `with_folders` is set. The candidate hash covers the files
    /// of pages `1..=page`, so an old entry stays fresh.
    fn proposal(&self, key: &str, rec: &RawEntry, page: usize, with_folders: bool) -> Proposal {
        let exact = self.exact(&rec.text);
        let scored = self.bm25f(&rec.text, &rec.heading);
        let mut lists = pages_of(&exact, &scored, page);
        let files = lists.pop().unwrap_or_default();
        let folders = if with_folders && page == 1 {
            self.folders(rec, &exact, &scored)
        } else {
            Vec::new()
        };
        let paths: Vec<&str> = files.iter().map(|&i| self.paths[i]).collect();
        let text_hash = text_hash(&rec.text);
        let mut link_id_text = format!("{key}\n{text_hash}\n{page}\n{}", paths.join("\n"));
        for f in &folders {
            link_id_text.push('\n');
            link_id_text.push_str(&f.path);
        }
        let mut sorted: Vec<&str> = lists
            .iter()
            .flatten()
            .chain(&files)
            .map(|&i| self.paths[i])
            .collect();
        sorted.sort_unstable();
        Proposal {
            page,
            link_id: hash8(&link_id_text),
            candidate_hash: hash8(&sorted.join("\n")),
            files,
            folders,
            text_hash,
        }
    }

    /// The card of file `i`: up to 6 symbol names that share a word with the
    /// record, else the first 4 names, and the 3 lines with the highest idf
    /// sum of record words. The lines print in file order.
    fn card(&self, root: &Path, n: usize, i: usize, record_words: &HashSet<String>) -> Card {
        let names = &self.names[i];
        let mut hit: Vec<&str> = Vec::new();
        for name in names {
            let shares = words(name).iter().any(|t| record_words.contains(t));
            if shares && !hit.contains(name) {
                hit.push(name);
            }
        }
        hit.truncate(CARD_SYMBOLS);
        if hit.is_empty() {
            hit = names.iter().copied().take(CARD_FALLBACK_SYMBOLS).collect();
        }
        let text = read_head(&root.join(self.paths[i]));
        let lines_of: Vec<&str> = text.split('\n').collect();
        let mut scored: Vec<(f64, usize)> = Vec::new();
        for (k, line) in lines_of.iter().enumerate() {
            let mut tokens = words(line);
            tokens.sort_unstable();
            tokens.dedup();
            let sum: f64 = tokens
                .iter()
                .filter(|t| record_words.contains(*t))
                .map(|t| self.idf_of(t))
                .sum();
            if tokens.iter().any(|t| record_words.contains(t)) {
                scored.push((sum, k));
            }
        }
        scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.truncate(CARD_LINES);
        scored.sort_by_key(|s| s.1);
        Card {
            n,
            path: self.paths[i].to_string(),
            symbols: hit.iter().map(|s| (*s).to_string()).collect(),
            lines: scored
                .iter()
                .map(|&(_, k)| {
                    let line: String = lines_of[k].trim().chars().take(CARD_LINE_CHARS).collect();
                    (k + 1, line)
                })
                .collect(),
        }
    }

    fn cards(&self, root: &Path, p: &Proposal, rec: &RawEntry) -> Vec<Card> {
        let record_words: HashSet<String> = words(&rec.text).into_iter().collect();
        p.files
            .iter()
            .enumerate()
            .map(|(k, &i)| self.card(root, k + 1, i, &record_words))
            .collect()
    }
}

/// The first bytes of a file as text. A missing file gives an empty text.
fn read_head(path: &Path) -> String {
    let Ok(file) = std::fs::File::open(path) else {
        return String::new();
    };
    let mut bytes = Vec::new();
    if file.take(CARD_READ_BYTES).read_to_end(&mut bytes).is_err() {
        return String::new();
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Quotes `key` for a POSIX shell.
fn shell_quote(key: &str) -> String {
    format!("'{}'", key.replace('\'', "'\\''"))
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// Everything that one proposal prints.
struct View<'a> {
    key: &'a str,
    rec: &'a RawEntry,
    proposal: &'a Proposal,
    cards: Vec<Card>,
    folder_cards: Vec<FolderCard>,
    unlinked: usize,
    auto: bool,
}

/// A folder card as it prints: path, files, ranked files, example files.
type FolderCard = (String, usize, usize, Vec<String>);

impl View<'_> {
    fn text(&self) -> String {
        let which = if self.auto {
            "next record"
        } else {
            "this record"
        };
        let page = if self.proposal.page > 1 {
            format!(" · page {} of up to {PAGE_MAX}", self.proposal.page)
        } else {
            String::new()
        };
        let mut out = format!(
            "sieve why --link · {which}{page} · {} · link_id {}\n\n{INSTRUCTION}\n\n",
            plural(self.unlinked, "record unlinked", "records unlinked"),
            self.proposal.link_id
        );
        out.push_str(&format!(
            "RECORD {} ({}):\n<<<\n{}\n>>>\n\n",
            self.key,
            self.rec.date,
            self.rec.text.trim()
        ));
        out.push_str(&format!(
            "CANDIDATE FILES ({}), each with its symbols and the lines that best match the record:\n",
            self.cards.len()
        ));
        for c in &self.cards {
            out.push_str(&format!("[{}] {}\n", c.n, c.path));
            if c.symbols.is_empty() {
                out.push_str("  symbols: -\n");
            } else {
                out.push_str(&format!("  symbols: {}\n", c.symbols.join(", ")));
            }
            for (line, text) in &c.lines {
                out.push_str(&format!("  L{line}: {text}\n"));
            }
        }
        if !self.folder_cards.is_empty() {
            out.push_str(&format!(
                "\nCANDIDATE FOLDERS. Keep a folder (F1, F2) only when the record bears on the files under it as a group. Sieve then links every file under it, at most {FOLDER_MAX_FILES}:\n"
            ));
            for (n, (path, files, hits, top)) in self.folder_cards.iter().enumerate() {
                out.push_str(&format!(
                    "[F{}] {path}/ ({files} files, {hits} ranked; e.g. {})\n",
                    n + 1,
                    top.join(", ")
                ));
            }
        }
        let page_flag = if self.proposal.page > 1 {
            format!(" --page {}", self.proposal.page)
        } else {
            String::new()
        };
        out.push_str(&format!(
            "\nThen run, with the numbers you keep, or none:\n  sieve why --link {}{page_flag} --keep 3,7,12 --expect {}\nSieve records the links and shows the next page or the next record.\n",
            shell_quote(self.key),
            self.proposal.link_id
        ));
        out
    }

    fn json(&self) -> serde_json::Value {
        let cards: Vec<serde_json::Value> = self
            .cards
            .iter()
            .map(|c| {
                serde_json::json!({
                    "n": c.n,
                    "path": c.path,
                    "symbols": c.symbols,
                    "lines": c.lines.iter()
                        .map(|(line, text)| serde_json::json!({"line": line, "text": text}))
                        .collect::<Vec<_>>(),
                })
            })
            .collect();
        serde_json::json!({
            "key": self.key,
            "doc": self.rec.doc,
            "heading": self.rec.heading,
            "date": self.rec.date,
            "link_id": self.proposal.link_id,
            "page": self.proposal.page,
            "folders": self.folder_cards.iter().enumerate()
                .map(|(n, (path, files, hits, _))| serde_json::json!({
                    "n": format!("F{}", n + 1), "path": path, "files": files, "ranked": hits,
                }))
                .collect::<Vec<_>>(),
            "unlinked": self.unlinked,
            "instruction": INSTRUCTION,
            "record_text": self.rec.text.trim(),
            "candidates": cards,
        })
    }
}

/// Reads the `--keep` list: 1-based file numbers of the proposal, folder
/// numbers such as `F1`, or `none`. It gives the file indexes and the folder
/// indexes.
fn parse_keep(
    keep: &str,
    count: usize,
    folder_count: usize,
) -> Result<(Vec<usize>, Vec<usize>), String> {
    if keep.trim() == "none" {
        return Ok((Vec::new(), Vec::new()));
    }
    let (mut files, mut folders) = (Vec::new(), Vec::new());
    for part in keep.split(',') {
        let part = part.trim();
        let (digits, is_folder) = match part.strip_prefix(['F', 'f']) {
            Some(rest) => (rest, true),
            None => (part, false),
        };
        let n: usize = digits
            .parse()
            .map_err(|_| format!("--keep takes numbers like 3,7,F1 or none, not \"{part}\""))?;
        let limit = if is_folder { folder_count } else { count };
        if n == 0 || n > limit {
            return Err(format!(
                "--keep {part} is not in the proposal (1 to {limit}); nothing was written"
            ));
        }
        if is_folder {
            folders.push(n - 1);
        } else {
            files.push(n - 1);
        }
    }
    files.sort_unstable();
    files.dedup();
    folders.sort_unstable();
    folders.dedup();
    Ok((files, folders))
}

// ---------------------------------------------------------------------------
// The command
// ---------------------------------------------------------------------------

/// Runs `sieve why --link` and prints the result.
pub(crate) fn run(
    args: &WhyArgs,
    root: &Path,
    context_dir: &Path,
    graph: &Graph,
) -> Result<(), String> {
    print!("{}", link_text(args, root, context_dir, graph, LOCK_WAIT)?);
    Ok(())
}

/// The `--keep` and `--expect` pair of a write, or `None` for a proposal.
/// The two go together and need the record key.
fn write_request(args: &WhyArgs) -> Result<Option<(&str, &str)>, String> {
    match (args.keep.as_deref(), args.expect.as_deref()) {
        (None, None) => Ok(None),
        (Some(_), Some(_)) if args.symbol.is_none() => Err(
            "--keep needs the record key: sieve why --link '<key>' --keep 1,2 --expect <link id>"
                .to_string(),
        ),
        (Some(keep), Some(expect)) => Ok(Some((keep, expect))),
        _ => Err("--keep and --expect go together".to_string()),
    }
}

/// Checks the `--link` flags. The call comes before any build or read.
pub(crate) fn check_flags(args: &WhyArgs) -> Result<(), String> {
    write_request(args)?;
    match args.page {
        Some(_) if args.symbol.is_none() => {
            Err("--page needs the record key: sieve why --link '<key>' --page 2".to_string())
        }
        Some(n) if !(1..=PAGE_MAX).contains(&n) => {
            Err(format!("--page takes a number from 1 to {PAGE_MAX}"))
        }
        _ => Ok(()),
    }
}

/// The output of `sieve why --link`. A write waits up to `wait` for the lock.
fn link_text(
    args: &WhyArgs,
    root: &Path,
    context_dir: &Path,
    graph: &Graph,
    wait: Duration,
) -> Result<String, String> {
    let wanted: Option<&str> = args.symbol.as_deref();
    let write = write_request(args)?;
    // The lock comes before the read of the docs, so a write sees the record
    // as it is when the sidecar changes.
    let _lock = write.map(|_| Lock::take(context_dir, wait)).transpose()?;

    let raw = raw_entries(&read_docs(root, &find_docs(root)));
    if raw.is_empty() {
        return Err("no decision record found".to_string());
    }
    let keys = record_keys(&raw);
    let links = read_links(root);
    let done = confirmed(&raw, &keys, &links);
    let unlinked = done.iter().filter(|d| d.is_none()).count();
    let (idx, auto, page) = match wanted {
        Some(k) => {
            let at = keys
                .iter()
                .position(|x| x == k)
                .ok_or_else(|| format!("no record has the key: {k}"))?;
            (at, false, args.page.unwrap_or(1))
        }
        // A record whose page was kept in part comes before a new record.
        None => match done.iter().position(|d| d.is_some_and(|e| e.more)) {
            Some(at) => (at, true, done[at].map_or(1, |e| e.pages as usize + 1)),
            None => match done.iter().position(Option::is_none) {
                Some(at) => (at, true, 1),
                None => return Ok(all_linked(args.json)),
            },
        },
    };
    // Page 2 and 3 run only after the agent kept enough files of the page
    // before, for the same record text.
    let prior = if page > 1 {
        let want = text_hash(&raw[idx].text);
        let found = links.iter().find(|l| {
            l.key == keys[idx] && l.text == want && l.more && l.pages as usize >= page - 1
        });
        Some(found.ok_or_else(|| {
            format!(
                "page {page} is due only after you keep {PAGE_MORE_AT} or more files of page {}: run sieve why --link {} --page {} first",
                page - 1,
                shell_quote(&keys[idx]),
                page - 1
            )
        })?)
    } else {
        None
    };
    let corpus = Corpus::build(context_dir, graph);
    let proposal = corpus.proposal(&keys[idx], &raw[idx], page, true);
    let view = |at: usize, p: &Proposal, unlinked: usize, auto: bool| {
        let cards = corpus.cards(root, p, &raw[at]);
        let folder_cards = p
            .folders
            .iter()
            .map(|f| {
                let top = f.top.iter().map(|&i| corpus.paths[i].to_string()).collect();
                (f.path.clone(), f.files, f.hits, top)
            })
            .collect();
        let v = View {
            key: &keys[at],
            rec: &raw[at],
            proposal: p,
            cards,
            folder_cards,
            unlinked,
            auto,
        };
        (v.text(), v.json())
    };
    let Some((keep, expect)) = write else {
        let (text, json) = view(idx, &proposal, unlinked, auto);
        return Ok(print_form(args.json, text, json));
    };

    if proposal.link_id != expect {
        return Err(format!(
            "the record or its candidates changed since the proposal; run sieve why --link {} again",
            shell_quote(&keys[idx])
        ));
    }
    let (picks, folder_picks) = parse_keep(keep, proposal.files.len(), proposal.folders.len())?;
    let mut kept: Vec<String> = Vec::new();
    let mut kept_folders: Vec<KeptFolder> = Vec::new();
    let mut pages = 1;
    if let Some(old) = prior {
        // A later page adds to the earlier picks.
        kept = old.kept.clone();
        kept_folders = old.folders.clone();
        pages = page;
    }
    for &k in &picks {
        let path = corpus.paths[proposal.files[k]].to_string();
        if !kept.contains(&path) {
            kept.push(path);
        }
    }
    for &k in &folder_picks {
        let f = &proposal.folders[k];
        kept_folders.push(KeptFolder {
            path: f.path.clone(),
            files: f.files,
        });
    }
    let more = picks.len() >= PAGE_MORE_AT
        && page < PAGE_MAX
        && !corpus.candidates(&raw[idx], page + 1).is_empty();
    put_entry(
        root,
        LinkEntry {
            key: keys[idx].clone(),
            text: proposal.text_hash.clone(),
            candidates: proposal.candidate_hash.clone(),
            date: crate::telemetry::local_date_at(crate::telemetry::now_secs()),
            kept: kept.clone(),
            pages: pages as u32,
            more,
            folders: kept_folders.clone(),
        },
    )?;
    // The refs cache holds the agent rows. The post-read note reads the
    // cache, so the cache must follow the sidecar now.
    let _ = crate::why::write_after_build(root, context_dir, graph);

    let links = read_links(root);
    let done = confirmed(&raw, &keys, &links);
    let left = done.iter().filter(|d| d.is_none()).count();
    let next = if more {
        let p = corpus.proposal(&keys[idx], &raw[idx], page + 1, false);
        Some(view(idx, &p, left, false))
    } else {
        done.iter().position(Option::is_none).map(|at| {
            let p = corpus.proposal(&keys[at], &raw[at], 1, true);
            view(at, &p, left, true)
        })
    };
    let report = format!(
        "linked {}: kept {} of {} files and {} folders, written to {LINKS_FILE}",
        keys[idx],
        picks.len(),
        proposal.files.len(),
        folder_picks.len()
    );
    if args.json {
        let value = serde_json::json!({
            "written": LINKS_FILE,
            "key": keys[idx],
            "page": page,
            "kept": kept,
            "folders": kept_folders.iter().map(|f| &f.path).collect::<Vec<_>>(),
            "next": next.map(|n| n.1),
        });
        return Ok(format!("{}\n", pretty(&value)));
    }
    Ok(match next {
        Some((text, _)) => format!("{report}\n\n{text}"),
        None => format!("{report}\nevery record is linked\n"),
    })
}

fn pretty(value: &serde_json::Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

fn print_form(json: bool, text: String, value: serde_json::Value) -> String {
    if json {
        format!("{}\n", pretty(&value))
    } else {
        text
    }
}

fn all_linked(json: bool) -> String {
    if json {
        format!(
            "{}\n",
            pretty(&serde_json::json!({"unlinked": 0, "next": null}))
        )
    } else {
        "every record is linked\n".to_string()
    }
}

// ---------------------------------------------------------------------------
// Staleness and the post-read note
// ---------------------------------------------------------------------------

/// The block that `sieve why --check` adds for linked records that are
/// stale, or an empty text. A repo with no sidecar pays nothing.
pub(crate) fn stale_block(root: &Path, context_dir: &Path, graph: &Graph) -> String {
    let links = read_links(root);
    if links.is_empty() {
        return String::new();
    }
    let raw = raw_entries(&read_docs(root, &find_docs(root)));
    let keys = record_keys(&raw);
    let at: HashMap<&str, usize> = keys
        .iter()
        .enumerate()
        .map(|(i, k)| (k.as_str(), i))
        .collect();
    let files: HashSet<&str> = graph
        .nodes
        .iter()
        .filter(|n| n.kind == Kind::File)
        .map(|n| n.path.as_str())
        .collect();
    let mut corpus: Option<Corpus> = None;
    let mut lines: Vec<String> = Vec::new();
    for l in &links {
        let reason = match at.get(l.key.as_str()) {
            None => "the record is gone".to_string(),
            Some(&i) if l.text != text_hash(&raw[i].text) => "the record text changed".to_string(),
            Some(&i) => match l.kept.iter().find(|p| !files.contains(p.as_str())) {
                Some(gone) => format!("a kept file is gone: {gone}"),
                None => {
                    if let Some(why) = folder_problem(&l.folders, &files) {
                        lines.push(format!("  {}: {why}\n", l.key));
                        continue;
                    }
                    let c = corpus.get_or_insert_with(|| Corpus::build(context_dir, graph));
                    let page = l.pages as usize;
                    if c.proposal(&l.key, &raw[i], page, false).candidate_hash == l.candidates {
                        continue;
                    }
                    "the candidate files changed".to_string()
                }
            },
        };
        lines.push(format!("  {}: {reason}\n", l.key));
    }
    if lines.is_empty() {
        return String::new();
    }
    let count = lines.len();
    let head = if count == 1 {
        "1 linked record is stale".to_string()
    } else {
        format!("{count} linked records are stale")
    };
    format!(
        "{head}; run sieve why --link '<key>' for each:\n{}",
        lines.concat()
    )
}

/// Why a kept folder no longer holds: it is gone, it grew past the cap, or
/// its file count changed since the agent kept it. `None` when all hold.
fn folder_problem(kept: &[KeptFolder], files: &HashSet<&str>) -> Option<String> {
    kept.iter().take(FOLDER_CARDS).find_map(|f| {
        let prefix = format!("{}/", f.path);
        let now = files.iter().filter(|p| p.starts_with(&prefix)).count();
        if now == 0 {
            Some(format!("a kept folder is gone: {}", f.path))
        } else if now > FOLDER_MAX_FILES {
            Some(format!(
                "a kept folder now holds {now} files, over {FOLDER_MAX_FILES}, so its rows stop: {}",
                f.path
            ))
        } else if now != f.files {
            Some(format!(
                "a kept folder changed from {} to {now} files: {}",
                f.files, f.path
            ))
        } else {
            None
        }
    })
}

/// The post-read note for a repo with `unlinked` records that have no
/// confirmed code link.
pub(crate) fn link_note(unlinked: usize) -> String {
    let (count, verb, noun) = if unlinked == 1 {
        ("1 decision record", "has", "link")
    } else {
        ("decision records", "have", "links")
    };
    let subject = if unlinked == 1 {
        count.to_string()
    } else {
        format!("{unlinked} {count}")
    };
    format!(
        "sieve: {subject} {verb} no confirmed code {noun}. When your current task is done, run sieve why --link to link the next one."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("sieve-why-link-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("make the scratch dir");
        dir
    }

    fn entry(key: &str, kept: &[&str]) -> LinkEntry {
        LinkEntry {
            key: key.to_string(),
            text: "3f2a9c1e".to_string(),
            candidates: "9b1d02aa".to_string(),
            date: "2026-11-02".to_string(),
            kept: kept.iter().map(|s| (*s).to_string()).collect(),
            pages: 1,
            more: false,
            folders: Vec::new(),
        }
    }

    #[test]
    fn the_sidecar_round_trips_sorted_with_one_record_per_line() {
        let dir = temp("round");
        put_entry(&dir, entry("docs/b.md#B", &["src/b.rs"])).expect("write b");
        put_entry(&dir, entry("docs/a.md#A", &[])).expect("write a");
        put_entry(&dir, entry("docs/b.md#B", &["src/b.rs", "src/c.rs"])).expect("replace b");
        let text = std::fs::read_to_string(dir.join(LINKS_FILE)).expect("read");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "{text}");
        assert_eq!(lines[0], "{\"version\":1}");
        assert!(lines[1].contains("docs/a.md#A"), "{text}");
        assert!(lines[2].contains("src/c.rs"), "{text}");
        let back = read_links(&dir);
        assert_eq!(back.len(), 2);
        assert_eq!(back[1].kept, ["src/b.rs", "src/c.rs"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_new_record_adds_a_line_and_changes_no_other_line() {
        let dir = temp("append");
        put_entry(&dir, entry("docs/a.md#A", &["src/a.rs"])).expect("write a");
        let one = std::fs::read_to_string(dir.join(LINKS_FILE)).expect("read");
        // B sorts last: the old text is a prefix of the new text.
        put_entry(&dir, entry("docs/z.md#Z", &[])).expect("write z");
        let two = std::fs::read_to_string(dir.join(LINKS_FILE)).expect("read");
        assert!(two.starts_with(&one), "{one}\n---\n{two}");
        assert_eq!(two.lines().count(), one.lines().count() + 1);
        // A record that sorts in the middle also leaves every old line as it was.
        put_entry(&dir, entry("docs/m.md#M", &[])).expect("write m");
        let three = std::fs::read_to_string(dir.join(LINKS_FILE)).expect("read");
        let old: Vec<&str> = two.lines().collect();
        let new: Vec<&str> = three.lines().filter(|l| !l.contains("docs/m.md")).collect();
        assert_eq!(old, new);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_sidecar_of_another_version_is_neither_read_nor_replaced() {
        let dir = temp("version");
        let text = "{\"version\":9}\n";
        std::fs::write(dir.join(LINKS_FILE), text).expect("write");
        assert!(read_links(&dir).is_empty());
        let err = put_entry(&dir, entry("docs/a.md#A", &[])).expect_err("must refuse");
        assert!(err.contains("another version"), "{err}");
        assert_eq!(
            std::fs::read_to_string(dir.join(LINKS_FILE)).expect("read"),
            text
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_held_lock_makes_the_second_writer_wait_and_then_fail() {
        let dir = temp("lock");
        let first = Lock::take(&dir, Duration::from_millis(100)).expect("first lock");
        let err = Lock::take(&dir, Duration::from_millis(120))
            .map(|_| ())
            .expect_err("second lock must fail");
        assert!(
            err.starts_with("another sieve why --link write holds the lock:"),
            "{err}"
        );
        assert!(err.ends_with("delete it if no write runs"), "{err}");
        drop(first);
        Lock::take(&dir, Duration::from_millis(100)).expect("lock after release");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Writes a lock file as if another process made it.
    fn plant_lock(dir: &Path, pid: u32, started: u64) -> PathBuf {
        let path = dir.join(".cache").join("why-links.lock");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, format!("{pid} {started}\n")).expect("plant");
        path
    }

    #[test]
    fn a_lock_older_than_60_seconds_or_with_a_dead_owner_is_taken_over() {
        let dir = temp("stale-lock");
        let now = crate::telemetry::now_secs();
        // The owner runs (this process) but the lock is 2 minutes old.
        plant_lock(&dir, std::process::id(), now - 120);
        Lock::take(&dir, Duration::from_millis(100)).expect("old lock is taken over");
        // The lock is new but the owner is gone.
        plant_lock(&dir, 4_000_000_000, now);
        Lock::take(&dir, Duration::from_millis(100)).expect("dead owner is taken over");
        // A new lock of a live owner holds.
        plant_lock(&dir, std::process::id(), now);
        let err = Lock::take(&dir, Duration::from_millis(100))
            .map(|_| ())
            .expect_err("a live lock holds");
        assert!(err.contains("holds the lock"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The arguments of `sieve why --link <key>`, with an optional write.
    fn link_args(key: &str, write: Option<(&str, &str)>, json: bool) -> WhyArgs {
        WhyArgs {
            symbol: Some(key.to_string()),
            dir: None,
            check: false,
            json,
            all: false,
            suggest: false,
            diff: false,
            no_refresh: true,
            link: true,
            keep: write.map(|w| w.0.to_string()),
            expect: write.map(|w| w.1.to_string()),
            page: None,
        }
    }

    #[test]
    fn a_shifted_duplicate_heading_makes_the_old_link_id_stale() {
        let dir = temp("dup-heading");
        std::fs::create_dir_all(dir.join("docs/adr")).expect("mkdir");
        std::fs::write(dir.join("a.rs"), "pub fn a() {}\n").expect("write code");
        let doc = |bodies: &[&str]| {
            let mut text = String::new();
            for b in bodies {
                text.push_str(&format!(
                    "## 2026-10-01, same\n\n- **Date:** 2026-10-01\n- {b}\n\n"
                ));
            }
            std::fs::write(dir.join("docs/adr/0001-same.md"), text).expect("write doc");
        };
        doc(&["the first record", "the second record"]);
        let context = dir.join("sieve");
        let graph = sieve_parse::build_graph(&dir, &context).expect("graph");
        // The second record has the key with the count.
        let key = "docs/adr/0001-same.md#2026-10-01, same #2";
        let shown = link_text(
            &link_args(key, None, true),
            &dir,
            &context,
            &graph,
            LOCK_WAIT,
        )
        .expect("proposal");
        let shown: serde_json::Value = serde_json::from_str(&shown).expect("json");
        assert!(shown["record_text"]
            .as_str()
            .is_some_and(|t| t.contains("the second record")));
        let id = shown["link_id"].as_str().expect("id").to_string();

        // A new record with the same heading goes first. The key now names
        // another record, so the old link id must not write.
        doc(&[
            "a new first record",
            "the first record",
            "the second record",
        ]);
        let graph = sieve_parse::build_graph(&dir, &context).expect("graph");
        let args = link_args(key, Some(("none", &id)), true);
        let err = link_text(&args, &dir, &context, &graph, LOCK_WAIT).expect_err("must refuse");
        assert!(err.contains("changed since the proposal"), "{err}");
        assert!(!dir.join(LINKS_FILE).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_write_with_the_lock_held_waits_then_fails_and_leaves_the_sidecar() {
        let dir = temp("write-lock");
        std::fs::create_dir_all(dir.join("docs/adr")).expect("mkdir");
        std::fs::write(dir.join("a.rs"), "pub fn a() {}\n").expect("write code");
        std::fs::write(
            dir.join("docs/adr/0001-a-rule.md"),
            "# A rule\n\n- **Date:** 2026-10-01\n- The a function.\n",
        )
        .expect("write ledger");
        let context = dir.join("sieve");
        let graph = sieve_parse::build_graph(&dir, &context).expect("graph");
        let args = link_args(
            "docs/adr/0001-a-rule.md#A rule",
            Some(("none", "00000000")),
            false,
        );
        let held = Lock::take(&context, Duration::from_millis(100)).expect("hold the lock");
        let start = Instant::now();
        let err = link_text(&args, &dir, &context, &graph, Duration::from_millis(150))
            .expect_err("a held lock must fail the write");
        assert!(err.contains("holds the lock"), "{err}");
        assert!(
            start.elapsed() >= Duration::from_millis(150),
            "it must wait first"
        );
        assert!(!dir.join(LINKS_FILE).exists());
        drop(held);
        // With the lock free, the same call reaches the link id check.
        let err = link_text(&args, &dir, &context, &graph, Duration::from_millis(150))
            .expect_err("the link id is wrong");
        assert!(err.contains("changed since the proposal"), "{err}");
        assert!(!dir.join(LINKS_FILE).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_repeated_heading_gets_a_count_in_doc_order() {
        let docs = vec![(
            "docs/adr/a.md".to_string(),
            "## 2026-01-01 Same\n\none\n\n## 2026-01-02 Other\n\ntwo\n\n## 2026-01-01 Same\n\nthree\n"
                .to_string(),
        )];
        let keys = record_keys(&raw_entries(&docs));
        assert_eq!(
            keys,
            [
                "docs/adr/a.md#2026-01-01 Same",
                "docs/adr/a.md#2026-01-02 Other",
                "docs/adr/a.md#2026-01-01 Same #2"
            ]
        );
    }

    #[test]
    fn the_text_hash_ignores_case_and_spacing_but_not_words() {
        assert_eq!(text_hash("The  Lock, waits"), text_hash("the lock waits"));
        assert_ne!(text_hash("the lock waits"), text_hash("the lock stops"));
        assert_eq!(text_hash("x").len(), 8);
    }

    #[test]
    fn the_path_scanners_read_slash_runs_and_dotted_names() {
        let text = "see crates/a/src/lib.rs and `-x.rs`, also a//b and foo.barbaz and README.md.";
        assert_eq!(slash_runs(text), ["crates/a/src/lib.rs"]);
        assert_eq!(dotted_names(text), ["lib.rs", "x.rs", "README.md"]);
    }

    #[test]
    fn a_directory_name_matches_only_as_a_whole_word() {
        assert!(has_word("the billing module", "billing"));
        assert!(!has_word("the rebilling module", "billing"));
        assert!(!has_word("the billing-v2 module", "billing"));
        assert!(!has_word("see a.billing", "billing"));
    }

    #[test]
    fn the_keep_list_takes_numbers_in_range_or_none() {
        assert_eq!(parse_keep("3, 1,3", 5, 0).expect("ok").0, [0, 2]);
        assert!(parse_keep("none", 5, 0).expect("ok").0.is_empty());
        assert!(parse_keep("6", 5, 0).is_err());
        assert!(parse_keep("0", 5, 0).is_err());
        assert!(parse_keep("a", 5, 0).is_err());
        assert_eq!(parse_keep("2,F1", 5, 1).expect("ok"), (vec![1], vec![0]));
        assert!(parse_keep("F2", 5, 1).is_err());
    }

    #[test]
    fn the_note_fits_the_hint_limit_and_names_the_count() {
        let note = link_note(47);
        assert!(note.starts_with("sieve: 47 decision records have no confirmed code links."));
        assert!(note.chars().count() <= 160, "{note}");
        assert!(link_note(99_999).chars().count() <= 160);
        assert!(link_note(1).starts_with("sieve: 1 decision record has no confirmed code link."));
        assert!(!note.contains("tokens"), "{note}");
    }

    #[test]
    fn the_skip_words_are_sorted_for_the_binary_search() {
        assert!(SKIP_WORDS.windows(2).all(|w| w[0] < w[1]));
        assert!(!keeps("the") && !keeps("id") && keeps("lock"));
    }

    /// A scratch project: the `files` hold one function each, and one record
    /// has `text`. It gives the dir, the context dir, the graph and the key.
    fn project(label: &str, files: &[String], text: &str) -> (PathBuf, PathBuf, Graph, String) {
        let dir = temp(label);
        std::fs::create_dir_all(dir.join("docs/adr")).expect("mkdir");
        for f in files {
            let path = dir.join(f);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(path, "pub fn widget() {}\n").expect("write code");
        }
        std::fs::write(
            dir.join("docs/adr/0001-x.md"),
            format!("# Rec\n\n- **Date:** 2026-10-01\n- {text}\n"),
        )
        .expect("write ledger");
        let context = dir.join("sieve");
        let graph = sieve_parse::build_graph(&dir, &context).expect("graph");
        (dir, context, graph, "docs/adr/0001-x.md#Rec".to_string())
    }

    fn json_of(args: &WhyArgs, dir: &Path, ctx: &Path, g: &Graph) -> serde_json::Value {
        let out = link_text(args, dir, ctx, g, LOCK_WAIT).expect("link text");
        serde_json::from_str(&out).expect("json")
    }

    #[test]
    fn page_two_runs_only_after_three_files_are_kept() {
        let files: Vec<String> = (0..70).map(|i| format!("src/widget_{i:02}.rs")).collect();
        let (dir, ctx, g, key) = project("pages", &files, "The widget rule.");
        let p1 = json_of(&link_args(&key, None, true), &dir, &ctx, &g);
        assert_eq!(p1["page"], 1);
        let id = p1["link_id"].as_str().expect("id").to_string();
        // Page 2 is not due before a write.
        let mut early = link_args(&key, None, true);
        early.page = Some(2);
        let err = link_text(&early, &dir, &ctx, &g, LOCK_WAIT).expect_err("not due");
        assert!(err.contains("page 2 is due only after"), "{err}");
        // Two kept files: no next page.
        let two = json_of(&link_args(&key, Some(("1,2", &id)), true), &dir, &ctx, &g);
        assert!(two["next"].is_null(), "{two}");
        assert!(link_text(&early, &dir, &ctx, &g, LOCK_WAIT).is_err());
        // Three kept files: page 2 of the same record comes next.
        let three = json_of(&link_args(&key, Some(("1,2,3", &id)), true), &dir, &ctx, &g);
        assert_eq!(three["next"]["page"], 2, "{three}");
        assert_eq!(three["next"]["key"], key.as_str());
        let id2 = three["next"]["link_id"].as_str().expect("id").to_string();
        let mut w = link_args(&key, Some(("1", &id2)), true);
        w.page = Some(2);
        let done = json_of(&w, &dir, &ctx, &g);
        assert_eq!(done["kept"].as_array().map(Vec::len), Some(4), "{done}");
        let entry = &read_links(&dir)[0];
        assert_eq!((entry.pages, entry.more), (2, false));
        // A page-1 write starts over.
        let again = json_of(&link_args(&key, Some(("1", &id)), true), &dir, &ctx, &g);
        assert_eq!(again["kept"].as_array().map(Vec::len), Some(1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_kept_folder_links_the_files_under_it_and_no_others() {
        let mut files: Vec<String> = (0..20).map(|i| format!("src/billing/f{i:02}.rs")).collect();
        files.push("src/other/o.rs".to_string());
        let (dir, ctx, g, key) = project(
            "folder",
            &files,
            "The files in src/billing/ follow one rule.",
        );
        let p = json_of(&link_args(&key, None, true), &dir, &ctx, &g);
        assert_eq!(p["folders"][0]["path"], "src/billing", "{p}");
        assert_eq!(p["folders"][0]["files"], 20);
        let id = p["link_id"].as_str().expect("id");
        json_of(&link_args(&key, Some(("F1", id)), true), &dir, &ctx, &g);
        assert_eq!(
            read_links(&dir)[0].folders,
            [KeptFolder {
                path: "src/billing".to_string(),
                files: 20
            }]
        );
        let docs = read_docs(&dir, &find_docs(&dir));
        let refs = crate::why::link_docs(&dir, &g, &docs, String::new());
        let on = |path: &str| refs.links.iter().filter(|l| l.path == path).count();
        assert_eq!(on("src/billing/f07.rs"), 1);
        assert!(refs
            .links
            .iter()
            .all(|l| l.via.contains("folder src/billing/")));
        assert_eq!(on("src/other/o.rs"), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_flags_a_kept_folder_that_grew_past_20_files_or_was_deleted() {
        let files: Vec<String> = (0..20).map(|i| format!("src/billing/f{i:02}.rs")).collect();
        let (dir, ctx, g, key) = project(
            "folder-check",
            &files,
            "The files in src/billing/ follow one rule.",
        );
        let p = json_of(&link_args(&key, None, true), &dir, &ctx, &g);
        let id = p["link_id"].as_str().expect("id");
        json_of(&link_args(&key, Some(("F1", id)), true), &dir, &ctx, &g);
        assert!(!stale_block(&dir, &ctx, &g).contains("folder"));
        // The folder grows to 21 files.
        std::fs::write(dir.join("src/billing/f20.rs"), "pub fn widget() {}\n").expect("write");
        let g = sieve_parse::build_graph(&dir, &ctx).expect("graph");
        let grown = stale_block(&dir, &ctx, &g);
        assert!(grown.contains("now holds 21 files, over 20"), "{grown}");
        // The folder is deleted.
        std::fs::remove_dir_all(dir.join("src/billing")).expect("remove");
        std::fs::write(dir.join("src/keep.rs"), "pub fn widget() {}\n").expect("write");
        let g = sieve_parse::build_graph(&dir, &ctx).expect("graph");
        let gone = stale_block(&dir, &ctx, &g);
        assert!(
            gone.contains("a kept folder is gone: src/billing"),
            "{gone}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_folder_over_20_files_is_refused() {
        let files: Vec<String> = (0..21).map(|i| format!("src/big/f{i:02}.rs")).collect();
        let (dir, ctx, g, key) = project(
            "big-folder",
            &files,
            "The files in src/big/ follow one rule.",
        );
        let p = json_of(&link_args(&key, None, true), &dir, &ctx, &g);
        assert!(p["folders"].as_array().is_some_and(Vec::is_empty), "{p}");
        let id = p["link_id"].as_str().expect("id");
        let err = link_text(
            &link_args(&key, Some(("F1", id)), true),
            &dir,
            &ctx,
            &g,
            LOCK_WAIT,
        )
        .expect_err("no folder card");
        assert!(err.contains("not in the proposal"), "{err}");
        // A hand-written sidecar that keeps the folder links nothing.
        let mut e = entry(&key, &[]);
        e.text = text_hash("The files in src/big/ follow one rule.");
        e.folders = vec![KeptFolder {
            path: "src/big".to_string(),
            files: 21,
        }];
        put_entry(&dir, e).expect("write");
        let docs = read_docs(&dir, &find_docs(&dir));
        let refs = crate::why::link_docs(&dir, &g, &docs, String::new());
        assert!(refs.links.is_empty(), "{:?}", refs.links);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_old_sidecar_still_reads_and_a_plain_entry_writes_the_old_line() {
        let dir = temp("old-sidecar");
        let old = "{\"version\":1}\n{\"key\":\"d.md#A\",\"text\":\"3f2a9c1e\",\"candidates\":\"9b1d02aa\",\"date\":\"2026-11-02\",\"kept\":[\"src/a.rs\"]}\n";
        std::fs::write(dir.join(LINKS_FILE), old).expect("write");
        let back = read_links(&dir);
        assert_eq!(back.len(), 1);
        assert_eq!(
            (back[0].pages, back[0].more, back[0].folders.len()),
            (1, false, 0)
        );
        put_entry(&dir, back[0].clone()).expect("rewrite");
        assert_eq!(
            std::fs::read_to_string(dir.join(LINKS_FILE)).expect("read"),
            old
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_key_is_quoted_for_a_posix_shell() {
        assert_eq!(shell_quote("a b#it's"), "'a b#it'\\''s'");
    }
}
