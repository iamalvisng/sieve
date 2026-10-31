//! The savings line: token estimates and the "saved" line every retrieval
//! command puts last in its output.

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

/// The tokens an `answer` saves against a baseline of `baseline_chars`.
///
/// Both sides count as chars / 4. The result is never below 0.
pub fn saved_tokens(baseline_chars: u64, answer: &str) -> u64 {
    to_tokens(baseline_chars).saturating_sub(to_tokens(utf16_len(answer)))
}

/// Formats a token count: under 1,000 as the number, else `k` with one
/// decimal (`1.9k`), and from 10,000 up `k` with no decimal (`84k`).
pub fn format_short(n: u64) -> String {
    if n < 1_000 {
        return n.to_string();
    }
    let tenths = (n + 50) / 100;
    if tenths < 100 {
        format!("{}.{}k", tenths / 10, tenths % 10)
    } else {
        format!("{}k", (n + 500) / 1_000)
    }
}

/// The answer line: `\u{1f3af} <label>`, then ` \u{b7} <saved> tokens saved`
/// when `saved` is above 0.
pub fn answer_line(label: &str, saved: u64) -> String {
    if saved == 0 {
        format!("{MARK} {label}")
    } else {
        format!(
            "{MARK} {label} \u{b7} ~{} tokens saved",
            format_short(saved)
        )
    }
}

/// The mark that opens every answer line.
const MARK: &str = "\u{1f3af}";

/// The answer line of `callers` and `trace_calls`: `n` exact callers, and
/// the saving against `baseline_chars` of whole-word grep output.
pub fn callers_line(n: usize, baseline_chars: u64, body: &str) -> String {
    answer_line(
        &format!("{n} exact callers"),
        saved_tokens(baseline_chars, body),
    )
}

/// The answer line of `skeleton` and `file_api`: `n` signatures, and the
/// saving against the whole file of `baseline_chars`.
pub fn signatures_line(n: usize, baseline_chars: u64, body: &str) -> String {
    answer_line(
        &format!("{n} signatures"),
        saved_tokens(baseline_chars, body),
    )
}

/// The answer line of `ask --def`: the exact body of `name`, and the saving
/// against the whole file of `baseline_chars`.
pub fn def_line(name: &str, baseline_chars: u64, body: &str) -> String {
    answer_line(
        &format!("exact body of {name}"),
        saved_tokens(baseline_chars, body),
    )
}

/// The answer line of `grep` and `find_all`: the saving against
/// `baseline_chars` of `grep -n` rows for the same hits. A `baseline_chars` of
/// 0 prints no saving.
pub fn hits_line(hits: usize, symbols: usize, baseline_chars: u64, body: &str) -> String {
    answer_line(
        &format!("{hits} hits in {symbols} symbols"),
        saved_tokens(baseline_chars, body),
    )
}

/// The answer line of a ranked `ask` and `find_code`: the saving against
/// `baseline_chars` of source the answer cut to a short excerpt. A
/// `baseline_chars` of 0 prints no saving.
pub fn ranked_line(hits: usize, baseline_chars: u64, body: &str) -> String {
    answer_line(
        &format!("{hits} ranked hits"),
        saved_tokens(baseline_chars, body),
    )
}

/// The answer line of a ready `rename` plan.
pub fn rename_line(files: usize) -> String {
    answer_line(&format!("{files} files \u{b7} 0 unproven edits"), 0)
}

/// Sums the saved tokens of every answer line in `text`.
///
/// A line counts when it holds the mark and ends in `tokens saved`. A
/// `k` number reads as thousands. `text` may be a JSON string, so a
/// line also ends at a quote or a backslash.
pub fn sum_answer_lines(text: &str) -> u64 {
    let mut total = 0u64;
    let mut rest = text;
    while let Some(idx) = rest.find(MARK) {
        let after = &rest[idx + MARK.len()..];
        let end = after.find(['\n', '"', '\\']).unwrap_or(after.len());
        total += parse_saved_tail(&after[..end]).unwrap_or(0);
        rest = &after[end..];
    }
    total
}

/// Reads `<n>[.d][k] tokens saved` from the end of one answer line.
fn parse_saved_tail(line: &str) -> Option<u64> {
    let head = line.trim_end().strip_suffix(" tokens saved")?;
    let num = head.rsplit(" \u{b7} ").next()?;
    let num = num.strip_prefix('~').unwrap_or(num);
    let (digits, scale) = match num.strip_suffix('k') {
        Some(d) => (d, 1_000.0),
        None => (num, 1.0),
    };
    let value: f64 = digits.parse().ok()?;
    if value < 0.0 || !value.is_finite() {
        return None;
    }
    Some((value * scale).round() as u64)
}

/// Adds the savings `line` as the last line of `text`.
///
/// `text` keeps its own final newline, if it has one. The result always
/// ends with one newline after `line`, so a reader finds the marker on the
/// last line.
pub fn append_line(text: &str, line: &str) -> String {
    if text.ends_with('\n') {
        format!("{text}{line}\n")
    } else {
        format!("{text}\n{line}\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sieve_core::wiring::{Meta, Node, Origin, SummaryState};

    #[test]
    fn append_line_puts_the_saving_on_the_last_line() {
        assert_eq!(
            append_line("a\nb\n", "[sieve] saved"),
            "a\nb\n[sieve] saved\n"
        );
        assert_eq!(
            append_line("a\nb", "[sieve] saved"),
            "a\nb\n[sieve] saved\n"
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
    fn saved_tokens_is_the_token_gap_and_never_below_zero() {
        assert_eq!(saved_tokens(229_476, &"x".repeat(6_536)), 55_735);
        assert_eq!(saved_tokens(230, &"x".repeat(297)), 0);
        assert_eq!(saved_tokens(0, "abc"), 0);
    }

    #[test]
    fn format_short_uses_k_with_one_decimal_below_10k() {
        let cases = [
            (0, "0"),
            (999, "999"),
            (1_000, "1.0k"),
            (1_949, "1.9k"),
            (9_949, "9.9k"),
            (9_950, "10k"),
            (10_000, "10k"),
            (84_000, "84k"),
            (84_499, "84k"),
        ];
        for (n, want) in cases {
            assert_eq!(format_short(n), want, "{n}");
        }
    }

    #[test]
    fn answer_line_leaves_out_the_saving_when_it_is_zero() {
        assert_eq!(
            answer_line("13 exact callers", 1_900),
            "\u{1f3af} 13 exact callers \u{b7} ~1.9k tokens saved"
        );
        assert_eq!(
            answer_line("13 exact callers", 0),
            "\u{1f3af} 13 exact callers"
        );
    }

    #[test]
    fn each_command_line_has_its_label() {
        let body = "x".repeat(40);
        assert_eq!(
            callers_line(13, 7_640, &body),
            "\u{1f3af} 13 exact callers \u{b7} ~1.9k tokens saved"
        );
        assert_eq!(
            signatures_line(4, 4_040, &body),
            "\u{1f3af} 4 signatures \u{b7} ~1.0k tokens saved"
        );
        assert_eq!(
            def_line("run", 440, &body),
            "\u{1f3af} exact body of run \u{b7} ~100 tokens saved"
        );
        assert_eq!(callers_line(2, 40, &body), "\u{1f3af} 2 exact callers");
        assert_eq!(hits_line(9, 4, 40, &body), "\u{1f3af} 9 hits in 4 symbols");
        assert_eq!(ranked_line(5, 40, &body), "\u{1f3af} 5 ranked hits");
        assert_eq!(
            hits_line(9, 4, 440, &body),
            "\u{1f3af} 9 hits in 4 symbols \u{b7} ~100 tokens saved"
        );
        assert_eq!(
            ranked_line(5, 440, &body),
            "\u{1f3af} 5 ranked hits \u{b7} ~100 tokens saved"
        );
        assert_eq!(rename_line(3), "\u{1f3af} 3 files \u{b7} 0 unproven edits");
    }

    #[test]
    fn sum_answer_lines_reads_k_numbers_and_json_strings() {
        let text = "x\n\u{1f3af} 3 exact callers \u{b7} ~1.9k tokens saved\n\
                    \u{1f3af} 2 signatures \u{b7} ~84k tokens saved\n\
                    \u{1f3af} 9 hits in 4 symbols\n\u{1f3af} 1 signatures \u{b7} 8 tokens saved";
        assert_eq!(sum_answer_lines(text), 1_900 + 84_000 + 8);
        let json = "{\"stdout\":\"\u{1f3af} 1 exact callers \u{b7} ~500 tokens saved\\n\"}";
        assert_eq!(sum_answer_lines(json), 500);
        assert_eq!(sum_answer_lines("no line"), 0);
    }

    #[test]
    fn format_thousands_groups_by_three_digits() {
        assert_eq!(format_thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn utf16_len_counts_a_surrogate_pair_as_two() {
        assert_eq!(utf16_len("🌱"), 2);
    }
}
