//! The inferred tier of `sieve why`. A decision record that no `Code:` line
//! and no path mention links to a file can still be about it. This module
//! scores each record against each file by the words they share, and keeps the
//! best record or two of a file when the score stands far above the rest.
//!
//! The scores are not recorded links. The build, the refresh and the `why`
//! rebuild write them to `.cache/why-topics.json`. A query and the post-read
//! hook read that file. The pass runs no git and no model.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use sieve_core::askindex::tokenize;
use sieve_core::wiring::{Graph, Kind};
use sieve_parse::{CommentBags, TopicInput};

use crate::why::{
    add_static_links, current_stamp, find_docs, raw_entries, read_docs, stat_part, LinkTargets,
    RawEntry,
};

/// The file terms that score the records. A file keeps its 50 heaviest
/// terms (the terms that few other files use). More terms add noise, and
/// fewer lose true records. The value comes from one repo with hand labels,
/// so another repo can differ.
const INFER_TERMS: usize = 50;

/// The least z-score of a shown record. The z-score is the distance of the
/// record score from the mean score of all scored records, in standard
/// deviations. A lower value gave too many wrong rows on the labelled files.
/// The value comes from one repo, so another repo can differ.
const INFER_Z: f64 = 4.5;

/// The most inferred rows of one file. The tier is a lead and not a full
/// answer, so it adds little to the text of a query.
pub(crate) const INFER_ROWS: usize = 2;

/// The BM25 term-frequency saturation. The common default value.
const BM25_K1: f64 = 1.2;

/// The BM25 length normalization. The common default value.
const BM25_B: f64 = 0.75;

/// The layout version of the topic sidecar. A reader drops another value.
const TOPIC_VERSION: u32 = 1;

/// One inferred record of a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TopicRow {
    /// The doc path, relative to the repo root.
    pub doc: String,
    /// The 1-based line of the record heading.
    pub line: usize,
    /// The heading text, with the date.
    pub heading: String,
    /// The record date `YYYY-MM-DD`.
    pub date: String,
}

/// The inferred records of one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TopicFile {
    /// The code path.
    pub path: String,
    /// True when a recorded link already gives the file an exact row.
    pub exact: bool,
    /// The records, newest date first.
    pub rows: Vec<TopicRow>,
}

/// The topic sidecar: the inferred rows of every file that has one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TopicRefs {
    /// The layout version.
    pub version: u32,
    /// The doc and graph stamp that the rows were made from.
    pub stamp: String,
    /// The files with at least one row, sorted by path.
    pub files: Vec<TopicFile>,
}

impl TopicRefs {
    /// The rows of `path`, or `None` when the file has none.
    pub(crate) fn file(&self, path: &str) -> Option<&TopicFile> {
        self.files
            .binary_search_by(|f| f.path.as_str().cmp(path))
            .ok()
            .map(|i| &self.files[i])
    }
}

/// The BM25 weights of the records, by term.
pub(crate) struct TopicIndex {
    /// For each term, the records that hold it with the BM25 weight of the
    /// term in the record, in record order.
    postings: HashMap<String, Vec<(usize, f64)>>,
    /// The record count.
    records: usize,
}

/// Builds the BM25 index of the record `texts`.
pub(crate) fn topic_index(texts: &[&str]) -> TopicIndex {
    let counts: Vec<HashMap<String, u32>> = texts
        .iter()
        .map(|t| {
            let mut c: HashMap<String, u32> = HashMap::new();
            for token in tokenize(t) {
                *c.entry(token).or_insert(0) += 1;
            }
            c
        })
        .collect();
    let n = counts.len();
    let lengths: Vec<f64> = counts
        .iter()
        .map(|c| f64::from(c.values().sum::<u32>()))
        .collect();
    let avg_len = lengths.iter().sum::<f64>() / n.max(1) as f64;
    let mut df: HashMap<&str, usize> = HashMap::new();
    for c in &counts {
        for t in c.keys() {
            *df.entry(t.as_str()).or_insert(0) += 1;
        }
    }
    let mut postings: HashMap<String, Vec<(usize, f64)>> = HashMap::with_capacity(df.len());
    for (i, c) in counts.iter().enumerate() {
        for (t, &f) in c {
            let docs = df[t.as_str()] as f64;
            let idf = (1.0 + (n as f64 - docs + 0.5) / (docs + 0.5)).ln();
            let f = f64::from(f);
            let weight = idf * f * (BM25_K1 + 1.0)
                / (f + BM25_K1 * (1.0 - BM25_B + BM25_B * lengths[i] / avg_len));
            postings.entry(t.clone()).or_default().push((i, weight));
        }
    }
    TopicIndex {
        postings,
        records: n,
    }
}

/// The z-score of each record that the file `terms` score at all. It gives
/// nothing when fewer than two records score. The score of a record is the
/// sum of its BM25 weights over the heaviest file terms. `code_df` gives the
/// count of files that hold a term, and `nf` the count of files.
pub(crate) fn file_z(
    idx: &TopicIndex,
    terms: &HashMap<String, u32>,
    code_df: &HashMap<&str, u32>,
    nf: usize,
) -> Vec<(usize, f64)> {
    let mut weighted: Vec<(&str, f64)> = terms
        .iter()
        .filter(|(t, _)| idx.postings.contains_key(t.as_str()))
        .filter_map(|(t, &f)| {
            let df = *code_df.get(t.as_str())?;
            let w = (1.0 + f64::from(f).ln()) * (nf as f64 / f64::from(df)).ln();
            Some((t.as_str(), w))
        })
        .collect();
    // The term order breaks a tie, so the choice does not depend on the map.
    weighted.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    weighted.truncate(INFER_TERMS);

    let mut score = vec![0.0_f64; idx.records];
    let mut touched: Vec<usize> = Vec::new();
    for (t, _) in &weighted {
        for &(i, w) in &idx.postings[*t] {
            // Every weight is positive, so a zero score means not scored yet.
            if score[i] == 0.0 {
                touched.push(i);
            }
            score[i] += w;
        }
    }
    if touched.len() < 2 {
        return Vec::new();
    }
    let n = touched.len() as f64;
    let mean = touched.iter().map(|&i| score[i]).sum::<f64>() / n;
    let var = touched
        .iter()
        .map(|&i| (score[i] - mean).powi(2))
        .sum::<f64>()
        / n;
    let sd = var.sqrt();
    let sd = if sd == 0.0 { 1.0 } else { sd };
    touched
        .into_iter()
        .map(|i| (i, (score[i] - mean) / sd))
        .collect()
}

/// The largest z-score of `n` scores is the square root of `n - 1`. So a
/// repo with too few records can never show an inferred row, and the pass
/// reads nothing for it.
fn too_few_records(n: usize) -> bool {
    (n.saturating_sub(1) as f64).sqrt() < INFER_Z
}

/// The entries that each file has as exact rows: a `Code:` anchor or a full
/// path mention. It runs the same git-free code as the sidecar build.
fn exact_by_path(graph: &Graph, raw: &[RawEntry]) -> HashMap<String, HashSet<usize>> {
    let targets = LinkTargets::new(graph);
    let mut out: HashMap<String, HashSet<usize>> = HashMap::new();
    for (i, e) in raw.iter().enumerate() {
        let mut links = BTreeMap::new();
        let mut stale = Vec::new();
        add_static_links(i, &e.text, &targets, &mut links, &mut stale);
        for l in links.values() {
            out.entry(l.path.clone()).or_default().insert(i);
        }
    }
    out
}

/// The terms of each graph file: the tokens of its node names, of its path,
/// and of its comments. Gives the files and the count of files that hold each
/// term. A file whose names and path give no token is not a file here.
fn file_terms<'a>(
    graph: &'a Graph,
    bags: &CommentBags<'_>,
) -> BTreeMap<&'a str, HashMap<String, u32>> {
    let mut files: BTreeMap<&str, HashMap<String, u32>> = BTreeMap::new();
    for n in &graph.nodes {
        let terms = files.entry(n.path.as_str()).or_default();
        for t in tokenize(&n.name) {
            *terms.entry(t).or_insert(0) += 1;
        }
        if n.kind == Kind::File {
            for t in tokenize(&n.path) {
                *terms.entry(t).or_insert(0) += 1;
            }
        }
    }
    files.retain(|_, terms| !terms.is_empty());
    for (path, terms) in &mut files {
        if let Some(bag) = bags.get(path) {
            for (t, c) in bag.iter() {
                *terms.entry(t.clone()).or_insert(0) += c;
            }
        }
    }
    files
}

/// Finds the inferred rows of every file: at most `INFER_ROWS` records whose
/// z-score is `INFER_Z` or more, and that the file does not already have as
/// exact rows. The rows of a file come newest date first.
pub(crate) fn topic_links(
    graph: &Graph,
    raw: &[RawEntry],
    bags: &CommentBags<'_>,
) -> Vec<TopicFile> {
    if too_few_records(raw.len()) {
        return Vec::new();
    }
    let texts: Vec<&str> = raw.iter().map(|e| e.text.as_str()).collect();
    let idx = topic_index(&texts);
    let files = file_terms(graph, bags);
    let mut code_df: HashMap<&str, u32> = HashMap::new();
    for terms in files.values() {
        for t in terms.keys() {
            *code_df.entry(t.as_str()).or_insert(0) += 1;
        }
    }
    let exact = exact_by_path(graph, raw);
    let ids: Vec<String> = raw
        .iter()
        .map(|e| format!("{}:{}", e.doc, e.line))
        .collect();

    let mut out = Vec::new();
    for (path, terms) in &files {
        let known = exact.get(*path);
        let mut top: Vec<(usize, f64)> = file_z(&idx, terms, &code_df, files.len())
            .into_iter()
            .filter(|&(i, z)| z >= INFER_Z && !known.is_some_and(|s| s.contains(&i)))
            .collect();
        // The best score wins; the record id breaks a tie.
        top.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| ids[a.0].cmp(&ids[b.0])));
        top.truncate(INFER_ROWS);
        if top.is_empty() {
            continue;
        }
        // Newest date first, then the sidecar order, as `sieve why` ranks rows.
        top.sort_by(|a, b| raw[b.0].date.cmp(&raw[a.0].date).then(a.0.cmp(&b.0)));
        out.push(TopicFile {
            path: (*path).to_string(),
            exact: known.is_some_and(|s| !s.is_empty()),
            rows: top
                .iter()
                .map(|&(i, _)| TopicRow {
                    doc: raw[i].doc.clone(),
                    line: raw[i].line,
                    heading: raw[i].heading.clone(),
                    date: raw[i].date.clone(),
                })
                .collect(),
        });
    }
    out
}

// ---------------------------------------------------------------------------
// The sidecar
// ---------------------------------------------------------------------------

fn topics_path(context_dir: &Path) -> PathBuf {
    context_dir.join(".cache").join("why-topics.json")
}

/// The count of sidecar writes in this process. It makes each temp file name
/// unique, so two threads never write one temp file.
static WRITES: AtomicU64 = AtomicU64::new(0);

/// Writes the sidecar atomically. The first line holds the stamp alone, so a
/// reader that needs only the stamp reads one line and parses no row. The
/// JSON follows on the next line.
fn write_file(context_dir: &Path, refs: &TopicRefs) -> std::io::Result<()> {
    let path = topics_path(context_dir);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let n = WRITES.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("{}.{n}.tmp", std::process::id()));
    let json = serde_json::to_string(refs).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, format!("{}\n{json}", refs.stamp))?;
    std::fs::rename(&tmp, &path)
}

/// Reads the topic sidecar when its stamp is `stamp`. It never builds. A
/// stale sidecar is dropped after its first line, with no JSON parse.
pub(crate) fn topics_with_stamp(context_dir: &Path, stamp: &str) -> Option<TopicRefs> {
    let bytes = std::fs::read(topics_path(context_dir)).ok()?;
    let body = bytes.strip_prefix(stamp.as_bytes())?.strip_prefix(b"\n")?;
    let refs: TopicRefs = serde_json::from_slice(body).ok()?;
    (refs.version == TOPIC_VERSION && refs.stamp == stamp).then_some(refs)
}

/// Makes the topic sidecar content. `bags` are the comment bags of a build
/// that is in memory. With `None`, the bags come from the extract cache. When
/// the cache is missing or of another version, the sidecar has no row. It
/// still holds the stamp, so the next call does not read the cache again. It
/// gives `None` only when there is no record doc.
fn make_topics(
    root: &Path,
    context_dir: &Path,
    graph: &Graph,
    bags: Option<&CommentBags<'_>>,
) -> Option<TopicRefs> {
    let docs = find_docs(root);
    // The stamp comes first: a doc edit after it makes the sidecar stale.
    let stamp = current_stamp(root, context_dir, &docs);
    let raw = raw_entries(&read_docs(root, &docs));
    if raw.is_empty() {
        return None;
    }
    let files = if too_few_records(raw.len()) {
        Vec::new()
    } else if let Some(bags) = bags {
        topic_links(graph, &raw, bags)
    } else {
        match sieve_parse::read_comment_bags(context_dir) {
            Some(owned) => {
                let view: CommentBags<'_> = owned.iter().map(|(id, b)| (id.as_str(), b)).collect();
                topic_links(graph, &raw, &view)
            }
            None => Vec::new(),
        }
    };
    Some(TopicRefs {
        version: TOPIC_VERSION,
        stamp,
        files,
    })
}

/// Builds the topic sidecar and writes it. The build calls this with the
/// bags of its result, and the `why` rebuild calls it with `None`. A repo with
/// no record doc gets no sidecar. A write failure is an error text, and the
/// caller never fails on it.
pub(crate) fn write_topics(
    root: &Path,
    context_dir: &Path,
    graph: &Graph,
    bags: Option<&CommentBags<'_>>,
) -> Result<(), String> {
    if !sieve_core::walk::read_why_inferred(root) {
        return Ok(());
    }
    write_topics_on(root, context_dir, graph, bags)
}

/// `write_topics` for a caller that already checked the config switch.
fn write_topics_on(
    root: &Path,
    context_dir: &Path,
    graph: &Graph,
    bags: Option<&CommentBags<'_>>,
) -> Result<(), String> {
    match make_topics(root, context_dir, graph, bags) {
        Some(refs) => write_file(context_dir, &refs).map_err(|e| e.to_string()),
        None => Ok(()),
    }
}

/// Gives the current topic sidecar. Else it builds the rows again from
/// `graph` and tries to write the sidecar. A write failure is not fatal.
pub(crate) fn load_topics(root: &Path, context_dir: &Path, graph: &Graph) -> Option<TopicRefs> {
    if !sieve_core::walk::read_why_inferred(root) {
        return None;
    }
    let docs = find_docs(root);
    let stamp = current_stamp(root, context_dir, &docs);
    if let Some(refs) = topics_with_stamp(context_dir, &stamp) {
        return Some(refs);
    }
    let refs = make_topics(root, context_dir, graph, None)?;
    let _ = write_file(context_dir, &refs);
    Some(refs)
}

/// The doc path inside one stamp part `<doc>:<size>:<nanos>` or
/// `<doc>:missing`.
fn doc_of_part(part: &str) -> Option<&str> {
    if let Some(doc) = part.strip_suffix(":missing") {
        return Some(doc);
    }
    part.rsplitn(3, ':').nth(2)
}

/// True when a record doc changed since the sidecar was written. The stamp of
/// the sidecar names the docs, so a read of its first line and a few stats
/// answer it. A new doc file is not seen here. The next build or `sieve why`
/// sees it.
fn docs_changed(root: &Path, context_dir: &Path) -> bool {
    let Ok(file) = std::fs::File::open(topics_path(context_dir)) else {
        return false;
    };
    let mut stamp = String::new();
    if BufReader::new(file).read_line(&mut stamp).is_err() {
        return false;
    }
    let stamp = stamp.trim_end_matches('\n');
    let mut parts: Vec<&str> = stamp.split('|').collect();
    // The last part is the wiring. The refresh that wrote the wiring wrote
    // the topic sidecar after it.
    parts.pop();
    parts.into_iter().any(|part| {
        doc_of_part(part).is_none_or(|doc| format!("{doc}:{}", stat_part(&root.join(doc))) != part)
    })
}

/// The topic writer that the refresh calls. With a graph and bags, the refresh
/// just wrote the wiring, so the sidecar is made again. With `None`, no code
/// file changed, and the sidecar is made again only if a record doc did.
pub(crate) fn write_topics_for(root: &Path, context_dir: &Path, input: TopicInput<'_, '_>) {
    if !sieve_core::walk::read_why_inferred(root) {
        return;
    }
    match input {
        Some((graph, bags)) => {
            let _ = write_topics_on(root, context_dir, graph, Some(bags));
        }
        None => {
            if !docs_changed(root, context_dir) {
                return;
            }
            let wiring = context_dir.join(".graph").join("wiring.json");
            let Some(graph) = std::fs::read(wiring)
                .ok()
                .and_then(|b| serde_json::from_slice::<Graph>(&b).ok())
            else {
                return;
            };
            let _ = write_topics_on(root, context_dir, &graph, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sieve_core::askindex::Bag;
    use sieve_core::wiring::{Meta, Node, Origin, SummaryState};

    const FILE: &str = "src/widget.rs";

    fn node(path: &str, name: &str, kind: Kind) -> Node {
        Node {
            id: if kind == Kind::File {
                path.to_string()
            } else {
                format!("{path}#{name}")
            },
            name: name.to_string(),
            kind,
            path: path.to_string(),
            span: "L1-L2".to_string(),
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

    fn graph() -> Graph {
        let nodes = vec![
            node(FILE, "widget.rs", Kind::File),
            node(FILE, "widget", Kind::Function),
            node("src/other.rs", "other.rs", Kind::File),
        ];
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

    /// A record list of `n` records that all hold "common". The records in
    /// `needles` hold "needle" three times as well. The records in `anchored`
    /// have a `Code:` line that names the file.
    fn ledger(n: usize, needles: &[usize], anchored: &[usize]) -> Vec<(String, String)> {
        let mut text = String::from("# Ledger\n\n");
        for i in 0..n {
            let date = format!("2026-{:02}-{:02}", 1 + i / 28, 1 + i % 28);
            let more = if needles.contains(&i) {
                "needle needle needle"
            } else {
                ""
            };
            let code = if anchored.contains(&i) {
                format!("- **Code:** `{FILE}#widget`\n")
            } else {
                String::new()
            };
            text.push_str(&format!(
                "## {date}, record {i}\n\n- **Date:** {date}\n{code}- common {more}\n\n"
            ));
        }
        vec![("records/log.md".to_string(), text)]
    }

    fn links(docs: &[(String, String)]) -> Vec<TopicFile> {
        let bag: Bag = vec![("needle".to_string(), 3), ("common".to_string(), 1)];
        let owned = BTreeMap::from([(FILE.to_string(), bag)]);
        let view: CommentBags<'_> = owned.iter().map(|(id, b)| (id.as_str(), b)).collect();
        topic_links(&graph(), &raw_entries(docs), &view)
    }

    #[test]
    fn a_record_that_stands_out_is_an_inferred_row_of_the_file() {
        let found = links(&ledger(25, &[24], &[]));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, FILE);
        assert!(!found[0].exact);
        assert_eq!(found[0].rows.len(), 1);
        assert!(found[0].rows[0].heading.ends_with("record 24"));
    }

    #[test]
    fn a_repo_with_too_few_records_gets_no_inferred_row() {
        assert!(too_few_records(21));
        assert!(!too_few_records(22));
        assert!(links(&ledger(21, &[20], &[])).is_empty());
    }

    #[test]
    fn a_file_gets_at_most_two_inferred_rows_newest_date_first() {
        let found = links(&ledger(70, &[10, 20, 30], &[]));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].rows.len(), INFER_ROWS);
        assert!(found[0].rows[0].date >= found[0].rows[1].date);
    }

    #[test]
    fn a_record_with_a_code_anchor_to_the_file_is_never_inferred() {
        // The needle record is the anchored record: it is exact, so no row.
        assert!(links(&ledger(200, &[190], &[190])).is_empty());
        // Two needle records, one of them anchored: the other stays inferred,
        // and the file has an exact row. The anchor line adds file words to
        // the anchored record, so the record list is long to keep the z-score up.
        let found = links(&ledger(200, &[190, 191], &[190]));
        assert_eq!(found.len(), 1);
        assert!(found[0].exact);
        assert_eq!(found[0].rows.len(), 1);
        assert!(found[0].rows[0].heading.ends_with("record 191"));
    }

    #[test]
    fn the_index_gives_a_rare_term_more_weight_than_a_common_term() {
        let idx = topic_index(&["alpha common", "beta common", "gamma common", "delta"]);
        let alpha = idx.postings["alpha"][0].1;
        let common = idx.postings["common"][0].1;
        assert!(alpha > common);
    }

    /// A scratch repo with a record list of 25 records and no extract cache.
    fn repo_without_cache(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "sieve-topics-{label}-{}-{}",
            std::process::id(),
            WRITES.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(root.join("adr")).expect("mkdir");
        std::fs::write(root.join("adr/records.md"), &ledger(25, &[24], &[])[0].1).expect("write");
        root
    }

    #[test]
    fn a_missing_extract_cache_leaves_a_fresh_empty_sidecar() {
        let root = repo_without_cache("nocache");
        let ctx = root.join("sieve");
        std::fs::create_dir_all(ctx.join(".cache")).expect("mkdir");
        std::fs::create_dir_all(root.join(".sieve")).expect("mkdir");
        std::fs::write(root.join(".sieve/config.json"), r#"{"whyInferred":true}"#).expect("write");
        let got = load_topics(&root, &ctx, &graph()).expect("topics");
        assert!(got.files.is_empty());
        // The sidecar exists and is fresh, so the next call reads no cache.
        let stamp = current_stamp(&root, &ctx, &find_docs(&root));
        let again = topics_with_stamp(&ctx, &stamp).expect("fresh sidecar");
        assert!(again.files.is_empty());
        // A doc edit makes it stale, and the refresh check sees the edit.
        assert!(!docs_changed(&root, &ctx));
        std::fs::write(root.join("adr/records.md"), "# none\n").expect("write");
        assert!(docs_changed(&root, &ctx));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_stamp_check_reads_the_first_line_and_a_stale_stamp_is_dropped() {
        let root = repo_without_cache("stamp");
        let ctx = root.join("sieve");
        write_file(
            &ctx,
            &TopicRefs {
                version: TOPIC_VERSION,
                stamp: "a|wiring:1:2".to_string(),
                files: Vec::new(),
            },
        )
        .expect("write");
        let text = std::fs::read_to_string(topics_path(&ctx)).expect("read");
        assert!(text.starts_with("a|wiring:1:2\n{"), "{text}");
        assert!(topics_with_stamp(&ctx, "a|wiring:1:2").is_some());
        assert!(topics_with_stamp(&ctx, "b|wiring:1:2").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_stamp_part_names_its_doc() {
        assert_eq!(doc_of_part("docs/a.md:120:99"), Some("docs/a.md"));
        assert_eq!(doc_of_part("docs/a.md:missing"), Some("docs/a.md"));
        assert_eq!(doc_of_part("bad"), None);
    }
}
