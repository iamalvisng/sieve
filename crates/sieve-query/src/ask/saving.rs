//! The token saving of a ranked answer: what an agent would read to get the
//! code that the answer cut to a short excerpt.

use sieve_core::wiring::{Graph, Kind};

use super::{parse_span_pointer, AskHit};
use crate::callers::GraphIndex;

/// The share of a span's size that the baseline counts. The baseline guesses
/// a span's chars from the average line of its file. Code lines run longer
/// than that average, so a share below 1 keeps the guess under the real size.
const K: f64 = 0.5;

/// The most chars one line counts toward a file's average. One minified or
/// data line can hold most of a file. Without this bound it would make every
/// other line of that file look long, and the baseline would claim far more
/// than an agent would read. Only file nodes hold a char count, so the
/// average comes from the file.
const MAX_AVG_LINE: f64 = 200.0;

/// The line count of a `L<from>-L<to>` span.
fn span_len(span: &str) -> Option<u64> {
    let (from, to) = span.strip_prefix('L')?.split_once("-L")?;
    let (from, to): (u64, u64) = (from.parse().ok()?, to.parse().ok()?);
    (to >= from).then(|| to - from + 1)
}

/// The estimated chars of the source spans behind the hits that show only an
/// excerpt of their span. A span counts as its lines times the average line
/// of its file, bounded by [`MAX_AVG_LINE`]. Lines shared by two spans count once. A hit shown in full, a file hit and
/// a concept hit add nothing. `index` is the lookup built for `graph`, if any.
pub fn crux_baseline_chars(graph: &Graph, index: Option<&GraphIndex>, hits: &[AskHit]) -> u64 {
    let mut spans: Vec<(&str, u32, u32)> = hits
        .iter()
        .filter(|hit| hit.cut)
        .filter_map(|hit| parse_span_pointer(&hit.pointer))
        .collect();
    spans.sort_unstable();
    let mut total = 0.0;
    let mut rest = spans.as_slice();
    while let Some(&(path, ..)) = rest.first() {
        let run = rest.partition_point(|s| s.0 == path);
        let (mine, after) = rest.split_at(run);
        rest = after;
        let mut lines = 0u64;
        let mut reached = 0u32;
        for &(_, from, to) in mine {
            let from = from.max(reached + 1);
            if to >= from {
                lines += u64::from(to - from + 1);
                reached = to;
            }
        }
        // A one-shot command has no index. It scans `graph.nodes` once per
        // file with a cut hit, at most one scan per hit, which costs less than
        // building the index.
        let file = match index {
            Some(index) => index.node(graph, path),
            None => graph.nodes.iter().find(|n| n.id == path),
        };
        let Some(file) = file.filter(|n| n.kind == Kind::File) else {
            continue;
        };
        let (Some(chars), Some(file_lines)) = (file.chars, span_len(&file.span)) else {
            continue;
        };
        let avg_line = (chars as f64 / file_lines as f64).min(MAX_AVG_LINE);
        total += lines as f64 * avg_line * K;
    }
    total as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use sieve_core::{Meta, Node, Origin, SummaryState};

    fn file(path: &str, chars: u64, span: &str) -> Node {
        Node {
            id: path.to_string(),
            name: path.to_string(),
            kind: Kind::File,
            owner: None,
            path: path.to_string(),
            span: span.to_string(),
            signature: None,
            exported: true,
            origin: Origin::Ast,
            body_hash: "0".repeat(64),
            chars: Some(chars),
            body_text: None,
            arity: None,
            variadic: None,
            summary_state: SummaryState::Pending,
            summary: None,
            crux: None,
        }
    }

    fn hit(pointer: &str, cut: bool) -> AskHit {
        AskHit {
            id: pointer.to_string(),
            kind: "symbol".to_string(),
            title: String::new(),
            pointer: pointer.to_string(),
            snippet: String::new(),
            related: Vec::new(),
            relation: None,
            score: 1.0,
            scope: None,
            code: Some("code".to_string()),
            scope_last: false,
            cut,
            path: String::new(),
            span: None,
        }
    }

    #[test]
    fn only_cut_hits_count_and_shared_lines_count_once() {
        let graph = Graph {
            meta: Meta {
                version: 1,
                node_count: 2,
                edge_count: 0,
                languages: Vec::new(),
                scopes: Vec::new(),
            },
            nodes: vec![file("a.ts", 1_000, "L1-L100"), file("b.ts", 400, "L1-L10")],
            edges: Vec::new(),
        };
        let hits = [
            hit("a.ts:L10-L29", true),
            hit("a.ts:L20-L39", true),
            hit("a.ts:L50-L60", false),
            hit("a.ts", true),
            hit("gone.ts:L1-L9", true),
            hit("b.ts:L1-L2", true),
        ];
        // a.ts: 30 distinct lines at 10 chars. b.ts: 2 lines at 40 chars.
        let want = ((30.0 * 10.0 + 2.0 * 40.0) * K) as u64;
        assert_eq!(crux_baseline_chars(&graph, None, &hits), want);
        let index = GraphIndex::build(&graph);
        assert_eq!(crux_baseline_chars(&graph, Some(&index), &hits), want);
        assert_eq!(crux_baseline_chars(&graph, None, &hits[2..4]), 0);
    }

    #[test]
    fn one_huge_line_cannot_inflate_the_figure() {
        // 100 lines, 1,000,000 chars: one minified line holds most of them.
        let graph = Graph {
            meta: Meta {
                version: 1,
                node_count: 1,
                edge_count: 0,
                languages: Vec::new(),
                scopes: Vec::new(),
            },
            nodes: vec![file("min.js", 1_000_000, "L1-L100")],
            edges: Vec::new(),
        };
        let hits = [hit("min.js:L1-L100", true)];
        assert_eq!(
            crux_baseline_chars(&graph, None, &hits),
            (100.0 * MAX_AVG_LINE * K) as u64
        );
    }
}
