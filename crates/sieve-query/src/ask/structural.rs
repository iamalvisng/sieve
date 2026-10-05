//! Structural mode: "who calls X" and "what does X call" queries
//! (the `ask-ranking.md` note section 6).

use std::collections::HashSet;
use std::sync::OnceLock;

use regex::Regex;
use sieve_core::collate::collate;
use sieve_core::{Graph, Kind, Node, Relation};

use super::{path_under_prefix, snippet_of, AskHit};
use crate::blast::JS_WS;

/// The relations an incoming-edge (caller) query walks (section 6.3).
const INCOMING_RELS: [Relation; 4] = [
    Relation::Calls,
    Relation::References,
    Relation::Implements,
    Relation::Extends,
];

/// The relations an outgoing-edge (callee) query walks (section 6.3).
const OUTGOING_RELS: [Relation; 5] = [
    Relation::Calls,
    Relation::References,
    Relation::Imports,
    Relation::Implements,
    Relation::Extends,
];

/// Matches a "who calls / callers of / used by" query. Case-sensitive: no
/// `i` flag (section 6.1). The pattern has no `u`
/// flag, so its `\b` is ASCII: `éuses` matches. `(?-u:\b)` pins that.
fn incoming_re() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    RE.get_or_init(|| {
        let s = JS_WS;
        Regex::new(&format!(
            r"(?-u:\b)(caller|callers|calls?{s}+into|who{s}+calls|what{s}+calls|called{s}+by|used{s}+by|uses)(?-u:\b)"
        ))
        .ok()
    })
    .as_ref()
}

/// Matches a "what does X call / callees / imports / depends on" query.
/// Case-sensitive: no `i` flag (section 6.1). The pattern
/// has no `u` flag, so `\b` and `\w` are ASCII: `what does
/// résumé call` does not match. `(?-u:...)` pins both.
fn outgoing_re() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    RE.get_or_init(|| {
        let s = JS_WS;
        Regex::new(&format!(
            r"(?-u:\b)(callee|callees|what{s}+does{s}+(?-u:\w)+{s}+call|calls{s}+what|imports?|depends{s}+on)(?-u:\b)"
        ))
        .ok()
    })
    .as_ref()
}

/// Splits a prose query into word-like tokens, keeping dots so a qualified
/// name (`Cache.get`) survives as one token. Dedups, first-occurrence order
/// (section 6.2, `subjectWords`).
pub(crate) fn subject_words(query: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut words = Vec::new();
    for word in query.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.')) {
        if word.is_empty() {
            continue;
        }
        if seen.insert(word.to_string()) {
            words.push(word.to_string());
        }
    }
    words
}

/// Strips a mint-time disambiguation ordinal (`~2`, `~3`, …) from the end of
/// each dot-segment of an id tail. The `~\d+(?=\.|$)` lookahead only fires at a
/// segment boundary.
fn strip_ordinals(id_tail: &str) -> String {
    id_tail
        .split('.')
        .map(strip_trailing_ordinal)
        .collect::<Vec<_>>()
        .join(".")
}

fn strip_trailing_ordinal(segment: &str) -> String {
    if let Some(idx) = segment.rfind('~') {
        let digits = &segment[idx + 1..];
        if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
            return segment[..idx].to_string();
        }
    }
    segment.to_string()
}

/// Non-file nodes whose name equals `lower_query`, or whose id ends
/// `#<query>` or `.<query>` after ordinal-stripping, case-insensitively
/// (`resolveSymbol` step 1, `traverse.ts`).
fn symbol_matches<'a>(nodes: &'a [Node], lower_query: &str) -> Vec<&'a Node> {
    let suffix_hash = format!("#{lower_query}");
    let suffix_dot = format!(".{lower_query}");
    nodes
        .iter()
        .filter(|n| {
            if n.kind == Kind::File {
                return false;
            }
            if n.name.to_lowercase() == lower_query {
                return true;
            }
            let lower_id = n.id.to_lowercase();
            let candidate_id = match lower_id.find('#') {
                None => lower_id.clone(),
                Some(hash_idx) => {
                    let (head, tail) = lower_id.split_at(hash_idx + 1);
                    format!("{head}{}", strip_ordinals(tail))
                }
            };
            candidate_id.ends_with(&suffix_hash) || candidate_id.ends_with(&suffix_dot)
        })
        .collect()
}

/// Resolves a query string to every matching node:
/// exact/suffix name match, then the last dot-segment as a
/// bare name, then a filename match against file nodes, then an optional
/// `--in` path-prefix filter.
pub(crate) fn resolve_symbol<'a>(
    graph: &'a Graph,
    query: &str,
    in_prefix: Option<&str>,
) -> Vec<&'a Node> {
    let lower_query = query.to_lowercase();
    let looks_like_filename = query.contains('.') && !query.contains('#');

    let mut matches = symbol_matches(&graph.nodes, &lower_query);

    if matches.is_empty() && query.contains('.') {
        if let Some(idx) = query.rfind('.') {
            let last_segment = query[idx + 1..].to_lowercase();
            if !last_segment.is_empty() {
                matches = graph
                    .nodes
                    .iter()
                    .filter(|n| n.kind != Kind::File && n.name.to_lowercase() == last_segment)
                    .collect();
            }
        }
    }

    if matches.is_empty() && looks_like_filename {
        matches = graph
            .nodes
            .iter()
            .filter(|n| {
                n.kind == Kind::File
                    && (n.name.to_lowercase() == lower_query
                        || n.path.to_lowercase() == lower_query
                        || n.path.to_lowercase().ends_with(&format!("/{lower_query}")))
            })
            .collect();
    }

    if let Some(prefix) = in_prefix {
        matches.retain(|n| path_under_prefix(&n.path, prefix));
    }
    matches
}

/// Resolves the query's structural subject, trying each `subjectWords` word
/// longest-first until one resolves (section 6.2, `findSubjectNodes`).
/// Returns the empty vector, plus the word tried last, when nothing
/// resolved.
fn find_subject_nodes<'a>(
    query: &str,
    graph: &'a Graph,
    in_prefix: Option<&str>,
) -> (Vec<&'a Node>, String) {
    let mut words = subject_words(query);
    words.sort_by_key(|w| std::cmp::Reverse(w.len()));
    for word in &words {
        let nodes = resolve_symbol(graph, word, in_prefix);
        if !nodes.is_empty() {
            return (nodes, word.clone());
        }
    }
    let tried = words.first().cloned().unwrap_or_else(|| query.to_string());
    (Vec::new(), tried)
}

/// The loud, never-silent note a fallthrough to lexical mode carries
/// (section 6.2, `fallthroughNoteFor`).
fn fallthrough_note_for(subject: &str) -> String {
    let name = sieve_core::product().name;
    format!(
        "structural index: no entries for '{subject}' — showing lexical matches; \
        for precise edges try {name} callers '{subject}', or {name} grep '{subject}' for every \
        reference (loosen the pattern if it returns nothing)"
    )
}

/// The word `ask` prints for a structural hit's `kind` field.
fn edge_kind_word(outgoing: bool) -> &'static str {
    if outgoing {
        "callee"
    } else {
        "caller"
    }
}

/// What `structural()` found: a genuine structural result, a signal to fall
/// through to lexical mode with a loud note, or no structural intent at all
/// (section 6, `StructuralOutcome`).
pub(crate) enum StructuralOutcome {
    Result {
        /// The resolved subject's name (`subjects[0].name`).
        subject: String,
        hits: Vec<AskHit>,
        note: String,
    },
    Fallthrough(String),
    None,
}

/// Runs the structural mode: walks incoming or outgoing edges of the
/// query's resolved subject. Returns [`StructuralOutcome::None`] when the
/// query has no structural intent (section 6.1).
pub(crate) fn structural(
    query: &str,
    graph: &Graph,
    limit: usize,
    in_prefix: Option<&str>,
) -> StructuralOutcome {
    let wants_in = incoming_re().is_some_and(|re| re.is_match(query));
    let wants_out = outgoing_re().is_some_and(|re| re.is_match(query));
    if !wants_in && !wants_out {
        return StructuralOutcome::None;
    }

    let (subjects, tried) = find_subject_nodes(query, graph, in_prefix);
    if subjects.is_empty() {
        return StructuralOutcome::Fallthrough(fallthrough_note_for(&tried));
    }

    let ids: HashSet<&str> = subjects.iter().map(|n| n.id.as_str()).collect();
    let outgoing = wants_out && !wants_in;

    let mut hits: Vec<AskHit> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for edge in &graph.edges {
        let walked = if outgoing {
            OUTGOING_RELS.contains(&edge.relation)
        } else {
            INCOMING_RELS.contains(&edge.relation)
        };
        if !walked {
            continue;
        }
        let (anchor, other) = if outgoing {
            (edge.source.as_str(), edge.target.as_str())
        } else {
            (edge.target.as_str(), edge.source.as_str())
        };
        if !ids.contains(anchor) {
            continue;
        }
        let dedup_key = format!("{other}{}", edge.relation.as_str());
        if !seen.insert(dedup_key) {
            continue;
        }
        let node = graph.nodes.iter().find(|n| n.id == other);
        hits.push(match node {
            Some(node) => AskHit {
                id: node.id.clone(),
                kind: edge_kind_word(outgoing).to_string(),
                title: node.name.clone(),
                pointer: format!("{}:{}", node.path, node.span),
                snippet: snippet_of(node),
                related: Vec::new(),
                relation: Some(edge.relation.as_str().to_string()),
                score: 1.0,
                scope: None,
                code: None,
                scope_last: false,
                path: node.path.clone(),
                span: Some(node.span.clone()),
            },
            None => AskHit {
                id: other.to_string(),
                kind: edge_kind_word(outgoing).to_string(),
                title: other.to_string(),
                pointer: other.to_string(),
                snippet: String::new(),
                related: Vec::new(),
                relation: Some(edge.relation.as_str().to_string()),
                score: 1.0,
                scope: None,
                code: None,
                scope_last: false,
                path: String::new(),
                span: None,
            },
        });
    }

    if hits.is_empty() {
        return StructuralOutcome::Fallthrough(fallthrough_note_for(&subjects[0].name));
    }

    hits.sort_by(|a, b| collate(&a.pointer, &b.pointer));
    hits.truncate(limit);
    let subject = subjects[0].name.clone();
    let note = if outgoing {
        format!("outgoing edges from {subject}")
    } else {
        format!("callers / references of {subject}")
    };
    StructuralOutcome::Result {
        subject,
        hits,
        note,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// P1-04: the `JS_WS` class holds the same chars as `is_js_whitespace`,
    /// over the whole BMP plus the astral plane edge.
    #[test]
    fn test_p1_04_js_ws_class_matches_is_js_whitespace() {
        let re = Regex::new(&format!("^{JS_WS}$")).expect("JS_WS compiles");
        let mut buf = [0u8; 4];
        for c in ('\0'..='\u{FFFF}').chain(['\u{10000}', '\u{10FFFF}']) {
            assert_eq!(
                re.is_match(c.encode_utf8(&mut buf)),
                crate::callers::is_js_whitespace(c),
                "U+{:04X}",
                c as u32
            );
        }
    }
    use sieve_core::{Confidence, Edge, Meta, Node as WNode, Origin, SummaryState};

    fn node(id: &str, name: &str, kind: Kind, path: &str, span: &str) -> WNode {
        WNode {
            id: id.to_string(),
            name: name.to_string(),
            kind,
            owner: None,
            path: path.to_string(),
            span: span.to_string(),
            signature: None,
            exported: false,
            origin: Origin::Ast,
            body_hash: "0".repeat(64),
            chars: None,
            body_text: None,
            arity: None,
            variadic: None,
            summary_state: SummaryState::Pending,
            summary: None,
            crux: None,
        }
    }

    fn edge(source: &str, target: &str, relation: Relation) -> Edge {
        Edge {
            source: source.to_string(),
            target: target.to_string(),
            relation,
            confidence: Confidence::Extracted,
        }
    }

    fn graph(nodes: Vec<WNode>, edges: Vec<Edge>) -> Graph {
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

    /// P1-04 (DV2): the intent regexes have no `u` flag, so `\b` and
    /// `\w` are ASCII. A non-ASCII letter is a boundary, not a word char.
    #[test]
    fn test_p1_04_intent_regexes_use_ascii_word_boundaries() {
        let incoming = incoming_re().expect("regex compiles");
        assert!(incoming.is_match("éuses double"));
        let outgoing = outgoing_re().expect("regex compiles");
        assert!(outgoing.is_match("what does run call"));
        assert!(!outgoing.is_match("what does résumé call"));
    }

    #[test]
    fn incoming_regex_is_case_sensitive() {
        let re = incoming_re().expect("regex compiles");
        assert!(re.is_match("who calls run"));
        assert!(re.is_match("callers of run"));
        assert!(!re.is_match("Who Calls run"));
    }

    #[test]
    fn outgoing_regex_is_case_sensitive() {
        let re = outgoing_re().expect("regex compiles");
        assert!(re.is_match("what does run call"));
        assert!(re.is_match("run imports"));
        assert!(!re.is_match("What Does run Call"));
    }

    #[test]
    fn subject_words_dedups_and_keeps_first_occurrence_order() {
        let words = subject_words("who calls run.run and run");
        assert_eq!(words, vec!["who", "calls", "run.run", "and", "run"]);
    }

    #[test]
    fn subject_words_splits_on_non_identifier_characters_but_keeps_dots() {
        let words = subject_words("Cache.get(x)!");
        assert_eq!(words, vec!["Cache.get", "x"]);
    }

    #[test]
    fn structural_returns_none_with_no_structural_intent() {
        let g = graph(vec![], vec![]);
        assert!(matches!(
            structural("box value accessor", &g, 8, None),
            StructuralOutcome::None
        ));
    }

    #[test]
    fn structural_falls_through_when_the_subject_does_not_resolve() {
        let g = graph(vec![], vec![]);
        match structural("who calls missingSymbol", &g, 8, None) {
            StructuralOutcome::Fallthrough(note) => {
                assert!(note.starts_with("structural index: no entries for 'missingSymbol'"));
            }
            _ => panic!("expected a fallthrough"),
        }
    }

    #[test]
    fn structural_finds_incoming_callers_and_dedups_by_other_and_relation() {
        let run = node("a.ts#run", "run", Kind::Function, "a.ts", "L1-L2");
        let caller = node("a.ts#main", "main", Kind::Function, "a.ts", "L3-L4");
        let g = graph(
            vec![run, caller],
            vec![
                edge("a.ts#main", "a.ts#run", Relation::Calls),
                edge("a.ts#main", "a.ts#run", Relation::Calls),
            ],
        );
        match structural("who calls run", &g, 8, None) {
            StructuralOutcome::Result { hits, note, .. } => {
                assert_eq!(hits.len(), 1);
                assert_eq!(hits[0].title, "main");
                assert_eq!(hits[0].kind, "caller");
                assert_eq!(hits[0].relation.as_deref(), Some("calls"));
                assert_eq!(note, "callers / references of run");
            }
            _ => panic!("expected a structural result"),
        }
    }

    #[test]
    fn structural_sorts_hits_by_pointer_and_slices_to_the_limit() {
        let run = node("a.ts#run", "run", Kind::Function, "a.ts", "L1-L2");
        let z = node("z.ts#z", "z", Kind::Function, "z.ts", "L1-L2");
        let a = node("a.ts#a", "a", Kind::Function, "a.ts", "L5-L6");
        let g = graph(
            vec![run, z, a],
            vec![
                edge("a.ts#run", "z.ts#z", Relation::Calls),
                edge("a.ts#run", "a.ts#a", Relation::Calls),
            ],
        );
        match structural("what does run call", &g, 1, None) {
            StructuralOutcome::Result { hits, .. } => {
                assert_eq!(hits.len(), 1);
                assert_eq!(hits[0].pointer, "a.ts:L5-L6");
            }
            _ => panic!("expected a structural result"),
        }
    }
}
