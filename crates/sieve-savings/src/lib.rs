//! The savings header: token estimates and the "tokens saved" line every
//! retrieval command prepends to its output (P3-34 to P3-39).

use std::collections::HashSet;

use sieve_core::wiring::{Graph, Kind};

/// Converts a character count to an estimated token count.
///
/// About four characters per token, rounded half away from zero.
pub fn to_tokens(chars: u64) -> u64 {
    (chars + 2) / 4
}

/// Returns the number of UTF-16 code units in `s`, the same length
/// JavaScript's `String.length` reports.
pub fn utf16_len(s: &str) -> u64 {
    s.encode_utf16().count() as u64
}

/// The "read it whole" baseline for a set of file paths: total chars and
/// file count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Savings {
    pub baseline_chars: u64,
    pub files: usize,
}

/// Sums the `chars` of the file nodes whose path is one of `paths`.
///
/// Skips a path with no known size. Returns `None` when not one path had a
/// known size.
pub fn savings_for(graph: &Graph, paths: &[String]) -> Option<Savings> {
    let wanted: HashSet<&str> = paths.iter().map(String::as_str).collect();
    let mut baseline_chars = 0u64;
    let mut files = 0usize;
    let mut seen: HashSet<&str> = HashSet::new();
    for node in &graph.nodes {
        if node.kind != Kind::File {
            continue;
        }
        if !wanted.contains(node.path.as_str()) {
            continue;
        }
        if !seen.insert(node.path.as_str()) {
            continue;
        }
        let Some(chars) = node.chars else {
            continue;
        };
        baseline_chars += chars;
        files += 1;
    }
    if files > 0 {
        Some(Savings {
            baseline_chars,
            files,
        })
    } else {
        None
    }
}

/// Formats `n` with a comma every three digits, `en-US` style.
pub fn format_thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    let bytes = digits.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        let from_end = bytes.len() - i;
        if i > 0 && from_end.is_multiple_of(3) {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

/// The short savings header: no percent, no instruction text. The
/// statusline and the pane show the total. The hook reads the number after
/// the `\u{2248}` sign.
pub fn sieve_header(name: &str, saved_tokens: u64) -> String {
    format!(
        "[{name}] saved \u{2248} {} tokens",
        format_thousands(saved_tokens)
    )
}

/// The one-line savings estimate for a command's rendered `body`.
///
/// `name` is the product name (for example `sieve`) the line
/// opens with, in `[name]`.
///
/// Returns `""` when there is nothing honest to claim: no baseline, a zero
/// baseline, or an output that is not actually smaller than the baseline.
pub fn savings_line(name: &str, body: &str, saved: Option<&Savings>) -> String {
    let Some(saved) = saved else {
        return String::new();
    };
    if saved.baseline_chars == 0 {
        return String::new();
    }
    let pack = to_tokens(utf16_len(body));
    let base = to_tokens(saved.baseline_chars);
    if base <= pack {
        return String::new();
    }
    let delta = base - pack;
    sieve_header(name, delta)
}

/// Renders `body` with the savings line prepended as a header.
///
/// `name` is the product name the header opens with, in `[name]`.
///
/// Returns `body` unchanged when the savings line is empty.
pub fn with_savings(name: &str, body: &str, saved: Option<&Savings>) -> String {
    let line = savings_line(name, body, saved);
    if line.is_empty() {
        body.to_string()
    } else {
        format!("{line}\n\n{body}")
    }
}

/// The part of a rendered `ask` output that the savings line measures.
///
/// The savings line measures `body`: the joined lines,
/// header included, before it appends the escalation nudge and the final
/// `"\n"`. This cuts that nudge (`\n\n[name] ...`) and the final newline
/// from `rendered`, and keeps the header line. `sieve-cli`'s `ask` and
/// `sieve-daemon`'s `find_code` share this one cut.
pub fn ask_pack_region(name: &str, rendered: &str) -> String {
    let without_trailing_newline = rendered.strip_suffix('\n').unwrap_or(rendered);
    let marker = format!("\n\n[{name}] ");
    match without_trailing_newline.rfind(&marker) {
        Some(idx) => without_trailing_newline[..idx].to_string(),
        None => without_trailing_newline.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sieve_core::wiring::{Meta, Node, Origin, SummaryState};

    #[test]
    fn ask_pack_region_cuts_the_nudge_and_keeps_the_header() {
        let rendered =
            "sieve ask — \"x\"  (lexical)\n\n1. a\n   b\n\n[sieve] only 1 hit — switch tool\n";
        assert_eq!(
            ask_pack_region("sieve", rendered),
            "sieve ask — \"x\"  (lexical)\n\n1. a\n   b"
        );
        assert_eq!(
            ask_pack_region("sieve", "sieve ask — \"x\"  (lexical)\n\n1. a\n"),
            "sieve ask — \"x\"  (lexical)\n\n1. a"
        );
    }

    fn file_node(path: &str, chars: Option<u64>) -> Node {
        Node {
            id: path.to_string(),
            name: path.to_string(),
            kind: Kind::File,
            path: path.to_string(),
            span: "L1-L1".to_string(),
            signature: None,
            exported: false,
            origin: Origin::Generic,
            body_hash: "0".repeat(64),
            chars,
            body_text: None,
            summary_state: SummaryState::Pending,
            summary: None,
            crux: None,
            owner: None,
            arity: None,
            variadic: None,
        }
    }

    fn graph_of(nodes: Vec<Node>) -> Graph {
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

    #[test]
    fn to_tokens_matches_the_basic_fixture() {
        assert_eq!(to_tokens(230), 58);
        assert_eq!(to_tokens(297), 74);
    }

    #[test]
    fn to_tokens_rounds_half_away_from_zero() {
        assert_eq!(to_tokens(2), 1);
        assert_eq!(to_tokens(1), 0);
    }

    #[test]
    fn savings_for_returns_none_when_no_path_has_a_known_size() {
        let graph = graph_of(vec![file_node("a.rs", None)]);
        let paths = vec!["a.rs".to_string()];
        assert_eq!(savings_for(&graph, &paths), None);
    }

    #[test]
    fn savings_for_skips_a_path_with_no_known_size() {
        let graph = graph_of(vec![file_node("a.rs", Some(100)), file_node("b.rs", None)]);
        let paths = vec!["a.rs".to_string(), "b.rs".to_string()];
        let saved = savings_for(&graph, &paths).expect("one known path");
        assert_eq!(saved.baseline_chars, 100);
        assert_eq!(saved.files, 1);
    }

    #[test]
    fn savings_line_is_empty_when_the_pack_is_not_smaller() {
        let saved = Savings {
            baseline_chars: 230,
            files: 1,
        };
        let body = "x".repeat(297);
        assert_eq!(savings_line("sieve", &body, Some(&saved)), "");
    }

    #[test]
    fn savings_line_names_the_given_product() {
        let saved = Savings {
            baseline_chars: 229_476,
            files: 14,
        };
        let body = "x".repeat(6_536);
        let line = savings_line("sieve", &body, Some(&saved));
        assert_eq!(line, "[sieve] saved ≈ 55,735 tokens");
    }

    /// The header holds no emoji and no instruction text.
    #[test]
    fn test_savings_sieve_header_has_no_tally_instruction() {
        let saved = Savings {
            baseline_chars: 229_476,
            files: 14,
        };
        let line = savings_line("sieve", &"x".repeat(6_536), Some(&saved));
        assert!(!line.contains('🌱') && !line.contains("reply"), "{line}");
    }

    #[test]
    fn format_thousands_groups_by_three_digits() {
        assert_eq!(format_thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn with_savings_returns_the_body_unchanged_on_an_empty_line() {
        assert_eq!(with_savings("sieve", "hello", None), "hello");
    }

    #[test]
    fn utf16_len_counts_a_surrogate_pair_as_two() {
        assert_eq!(utf16_len("🌱"), 2);
    }
}
