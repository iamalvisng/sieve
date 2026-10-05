//! The `ask` sidecar index: the tokenizer, the per-node bags, and the
//! `.cache/ask-index.json` file (the `ask-ranking.md` note
//! sections 1 and 2).

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process;

use serde::{Deserialize, Serialize};

use crate::collate::collate;
use crate::wiring::Graph;

/// The stop word list, verbatim and in source order
/// (`ask-ranking.md` section 2.2). The note names this list "27 words" but
/// lists 32; Sieve keeps the full listed word set, since the note gives the
/// word list as the source of truth over the count.
pub const STOP_WORDS: [&str; 32] = [
    "the", "a", "an", "of", "to", "in", "is", "are", "how", "does", "do", "what", "where", "which",
    "that", "this", "it", "for", "on", "and", "or", "with", "i", "we", "get", "set", "use", "used",
    "using", "when", "why", "can",
];

/// Splits `text` into lowercase alphanumeric tokens, and drops stop words.
///
/// The steps run in this order: insert a space between an ASCII
/// `[a-z0-9]` character and an ASCII `[A-Z]` character, lowercase the
/// text, split on every run of characters outside `[a-z0-9]`, then keep a
/// token when its length is greater than 1 and it is not a stop word.
///
/// Rust's `to_lowercase` and JavaScript's `toLowerCase` agree on ASCII and
/// may differ on a few Unicode letters. The split step keeps only
/// `[a-z0-9]` runs, so a token can hold only ASCII letters and digits. A
/// Unicode lowercasing difference cannot reach a returned token.
pub fn tokenize(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut spaced = String::with_capacity(text.len() + 8);
    for (i, &c) in chars.iter().enumerate() {
        if i > 0 {
            let prev = chars[i - 1];
            if (prev.is_ascii_lowercase() || prev.is_ascii_digit()) && c.is_ascii_uppercase() {
                spaced.push(' ');
            }
        }
        spaced.push(c);
    }
    let lower = spaced.to_lowercase();
    lower
        .split(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit()))
        .filter(|t| t.len() > 1 && !STOP_WORDS.contains(t))
        .map(|t| t.to_string())
        .collect()
}

/// A `[token, count]` list in first-occurrence order of the token.
pub type Bag = Vec<(String, u32)>;

/// Counts `tokens` into a `Bag`, in first-occurrence order.
pub fn bag(tokens: &[String]) -> Bag {
    let mut out: Bag = Vec::new();
    for token in tokens {
        match out.iter_mut().find(|(t, _)| t == token) {
            Some((_, count)) => *count += 1,
            None => out.push((token.clone(), 1)),
        }
    }
    out
}

/// Sums the counts in `bag`.
fn bag_len(bag: &Bag) -> u32 {
    bag.iter().map(|(_, count)| count).sum()
}

/// One indexed node: its id, and its three token bags.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskDoc {
    pub id: String,
    pub name: Bag,
    pub path: Bag,
    pub body: Bag,
}

/// Sums the `body` bag counts of `doc`.
pub fn body_len(doc: &AskDoc) -> u32 {
    bag_len(&doc.body)
}

/// The `.cache/ask-index.json` sidecar: the document frequency table and
/// the per-node token bags, used to score `ask` queries without the full
/// graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AskIndex {
    pub version: u32,
    #[serde(rename = "avgBodyLen")]
    pub avg_body_len: f64,
    pub df: Vec<(String, u32)>,
    #[serde(rename = "docCount")]
    pub doc_count: usize,
    pub docs: Vec<AskDoc>,
}

/// Builds the `ask` sidecar index from `graph` (`ask-ranking.md` sections
/// 1.4 to 1.7 and 2.3).
///
/// The `body` bag on the `basic` parity fixture must give `avgBodyLen ==
/// 8.857142857142858` once `sieve-parse` fills in `body_text`; that number
/// pins the body extraction (section 10.10). `sieve-parse`
/// owns that test, because this crate builds no `body_text` of its own.
pub fn build_ask_index(graph: &Graph) -> AskIndex {
    let mut nodes: Vec<_> = graph.nodes.iter().collect();
    nodes.sort_by(|a, b| collate(&a.id, &b.id));

    let mut df: Vec<(String, u32)> = Vec::new();
    let mut docs: Vec<AskDoc> = Vec::with_capacity(nodes.len());

    for node in &nodes {
        let name = bag(&tokenize(&node.name));
        let path = bag(&tokenize(&node.path));
        let body_source = format!(
            "{} {} {}",
            node.signature.as_deref().unwrap_or(""),
            node.summary.as_deref().unwrap_or(""),
            node.body_text.as_deref().unwrap_or("")
        );
        let body = bag(&tokenize(&body_source));

        let mut seen: Vec<&str> = Vec::new();
        for (token, _) in name.iter().chain(path.iter()).chain(body.iter()) {
            if seen.contains(&token.as_str()) {
                continue;
            }
            seen.push(token.as_str());
            match df.iter_mut().find(|(t, _)| t == token) {
                Some((_, count)) => *count += 1,
                None => df.push((token.clone(), 1)),
            }
        }

        docs.push(AskDoc {
            id: node.id.clone(),
            name,
            path,
            body,
        });
    }

    let avg_body_len = if docs.is_empty() {
        0.0
    } else {
        let total: u64 = docs.iter().map(|doc| u64::from(body_len(doc))).sum();
        total as f64 / docs.len() as f64
    };

    AskIndex {
        version: 1,
        avg_body_len,
        df,
        doc_count: docs.len(),
        docs,
    }
}

/// Builds the `.cache/ask-index.json` path under `context_dir`.
pub fn ask_index_path(context_dir: &Path) -> PathBuf {
    context_dir.join(".cache").join("ask-index.json")
}

/// Writes `index` to `path` as compact JSON, atomically.
///
/// The write goes through a `.<pid>.tmp` sibling file and a rename, so a
/// reader never sees a partial file. `avgBodyLen` prints without a
/// fraction when its value is a whole number, matching `JSON.stringify`;
/// `serde_json`'s own float formatting already gives the shortest
/// round-trip form for every other value.
pub fn write_ask_index(path: &Path, index: &AskIndex) -> io::Result<()> {
    let mut json = serde_json::to_string(index).map_err(io::Error::other)?;
    let default_avg = serde_json::to_string(&index.avg_body_len).map_err(io::Error::other)?;
    if let Some(whole) = default_avg.strip_suffix(".0") {
        let needle = format!("\"avgBodyLen\":{default_avg}");
        let replacement = format!("\"avgBodyLen\":{whole}");
        json = json.replacen(&needle, &replacement, 1);
    }
    json.push('\n');

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }

    let tmp_path = tmp_path_for(path);
    let result = fs::write(&tmp_path, json.as_bytes()).and_then(|()| fs::rename(&tmp_path, path));
    if result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    result
}

/// Builds the `<path>.<pid>.tmp` sibling path used for the atomic write.
fn tmp_path_for(path: &Path) -> PathBuf {
    let pid = process::id();
    let mut file_name: OsString = path.file_name().unwrap_or_default().to_os_string();
    file_name.push(format!(".{pid}.tmp"));
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(file_name),
        _ => PathBuf::from(file_name),
    }
}

/// Reads and validates `.cache/ask-index.json` at `path`.
///
/// Returns `None` when the file is missing, fails to parse, holds a
/// `version` other than `1`, or holds a `docCount` that does not match
/// `docs.len()` (`ask-ranking.md` section 1.8).
pub fn read_ask_index(path: &Path) -> Option<AskIndex> {
    let text = fs::read_to_string(path).ok()?;
    let index: AskIndex = serde_json::from_str(&text).ok()?;
    if index.version != 1 || index.doc_count != index.docs.len() {
        return None;
    }
    Some(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wiring::{Kind, Meta, Node, Origin, SummaryState};

    /// A temp dir unique per call, that removes itself on drop, even if
    /// the test panics before it reaches its own cleanup line.
    fn unique_temp_dir(label: &str) -> crate::test_support::TempDir {
        crate::test_support::TempDir::new(label)
    }

    #[test]
    fn tokenize_splits_camel_case_and_drops_a_stop_word() {
        assert_eq!(tokenize("getUserByID"), vec!["user", "by", "id"]);
    }

    #[test]
    fn tokenize_drops_stop_words_from_a_query() {
        assert_eq!(
            tokenize("how does run compute the result"),
            vec!["run", "compute", "result"]
        );
    }

    #[test]
    fn tokenize_splits_on_a_digit_to_uppercase_boundary_and_drops_short_tokens() {
        assert_eq!(tokenize("a1B c"), vec!["a1"]);
    }

    #[test]
    fn bag_keeps_first_occurrence_order_and_counts() {
        let tokens = tokenize("run run compute run");
        assert_eq!(
            bag(&tokens),
            vec![("run".to_string(), 3), ("compute".to_string(), 1)]
        );
    }

    fn sample_node(id: &str, name: &str, path: &str, body_text: Option<&str>) -> Node {
        Node {
            id: id.to_string(),
            name: name.to_string(),
            kind: Kind::Function,
            owner: None,
            path: path.to_string(),
            span: "L1-L2".to_string(),
            signature: None,
            exported: true,
            origin: Origin::Ast,
            body_hash: "0".repeat(64),
            chars: None,
            body_text: body_text.map(|s| s.to_string()),
            arity: None,
            variadic: None,
            summary_state: SummaryState::Pending,
            summary: None,
            crux: None,
        }
    }

    fn sample_graph() -> Graph {
        Graph {
            meta: Meta {
                version: 1,
                node_count: 0,
                edge_count: 0,
                languages: vec!["rust".to_string()],
                scopes: vec![],
            },
            nodes: vec![
                sample_node("a.rs#run", "run", "a.rs", Some("run the loop")),
                sample_node("b.rs#run", "run", "b.rs", Some("run once")),
            ],
            edges: vec![],
        }
    }

    #[test]
    fn build_ask_index_counts_df_and_avg_body_len_over_sorted_docs() {
        let index = build_ask_index(&sample_graph());
        assert_eq!(index.doc_count, 2);
        assert_eq!(index.docs[0].id, "a.rs#run");
        assert_eq!(index.docs[1].id, "b.rs#run");

        let df: std::collections::HashMap<_, _> = index.df.into_iter().collect();
        assert_eq!(df.get("run").copied(), Some(2));
        assert_eq!(df.get("loop").copied(), Some(1));
        assert_eq!(df.get("once").copied(), Some(1));

        // a.rs#run body = "run the loop" -> tokens "run","loop" (len 2)
        // b.rs#run body = "run once" -> tokens "run","once" (len 2)
        assert_eq!(index.avg_body_len, 2.0);
    }

    #[test]
    fn build_ask_index_on_an_empty_graph_gives_zero_avg_body_len() {
        let graph = Graph {
            meta: Meta {
                version: 1,
                node_count: 0,
                edge_count: 0,
                languages: vec![],
                scopes: vec![],
            },
            nodes: vec![],
            edges: vec![],
        };
        let index = build_ask_index(&graph);
        assert_eq!(index.avg_body_len, 0.0);
        assert_eq!(index.doc_count, 0);
    }

    #[test]
    fn write_ask_index_prints_the_exact_key_order_and_a_bare_zero_avg_body_len() {
        let graph = Graph {
            meta: Meta {
                version: 1,
                node_count: 0,
                edge_count: 0,
                languages: vec![],
                scopes: vec![],
            },
            nodes: vec![],
            edges: vec![],
        };
        let index = build_ask_index(&graph);
        let dir = unique_temp_dir("write");
        let path = dir.join("ask-index.json");

        write_ask_index(&path, &index).expect("write succeeds");
        let text = fs::read_to_string(&path).expect("read written file");

        assert_eq!(
            text,
            "{\"version\":1,\"avgBodyLen\":0,\"df\":[],\"docCount\":0,\"docs\":[]}\n"
        );
    }

    #[test]
    fn read_ask_index_rejects_a_doc_count_mismatch() {
        let dir = unique_temp_dir("mismatch");
        let path = dir.join("ask-index.json");
        fs::write(
            &path,
            "{\"version\":1,\"avgBodyLen\":0,\"df\":[],\"docCount\":1,\"docs\":[]}\n",
        )
        .expect("write file");

        assert!(read_ask_index(&path).is_none());
    }

    #[test]
    fn read_ask_index_returns_none_for_a_missing_file() {
        let path = Path::new("/nonexistent/sieve-ask-index.json");
        assert!(read_ask_index(path).is_none());
    }

    #[test]
    fn ask_index_path_joins_cache_and_the_file_name() {
        let path = ask_index_path(Path::new("/repo/.sieve"));
        assert_eq!(path, PathBuf::from("/repo/.sieve/.cache/ask-index.json"));
    }

    #[test]
    fn body_len_sums_the_body_bag_counts() {
        let doc = AskDoc {
            id: "x".to_string(),
            name: vec![],
            path: vec![],
            body: vec![("run".to_string(), 2), ("loop".to_string(), 1)],
        };
        assert_eq!(body_len(&doc), 3);
    }
}
