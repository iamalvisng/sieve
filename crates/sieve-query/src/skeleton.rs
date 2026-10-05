//! `skeleton`: a signatures-only view of one file, straight from the graph
//! (the `query-commands.md` note section 1).

use std::cmp::Ordering;

use serde::Serialize;
use sieve_core::{Graph, Kind, Node};

/// One definition in a file's skeleton view.
#[derive(Debug, Clone, Serialize)]
pub struct SkeletonEntry {
    pub name: String,
    pub kind: Kind,
    pub span: String,
    /// The node's signature, or `null` when it has none (section 1.7).
    pub signature: Option<String>,
    /// The first line of the node's summary, trimmed, when it has one
    /// (section 1.7). Omitted, not `null`, when there is none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// The full result of one `skeleton` run. `saved` is not part of this
/// struct: the CLI computes and attaches it, the same way `ask` does
/// (section 1.7).
#[derive(Debug, Clone, Serialize)]
pub struct SkeletonResult {
    pub file: String,
    pub entries: Vec<SkeletonEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Compares two strings by UTF-16 code unit, matching JavaScript's default
/// `Array.sort()` (no `localeCompare`), for the skeleton ambiguity note
/// (section 6, "Spec corrections").
fn code_unit_cmp(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

/// Parses a node's `L<start>-...` span into the leading line number. A
/// failed match gives `0` (section 1.3).
fn start_line(span: &str) -> u32 {
    span.strip_prefix('L')
        .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|digits| digits.parse().ok())
        .unwrap_or(0)
}

/// The first line of `summary`, trimmed, or `None` when it is empty
/// (section 1.7, `summary.split("\n")[0].trim() || undefined`).
fn first_summary_line(summary: &str) -> Option<String> {
    let first = summary.lines().next().unwrap_or("").trim();
    if first.is_empty() {
        None
    } else {
        Some(first.to_string())
    }
}

/// Builds the skeleton view of `file`: every non-file definition at that
/// path, sorted by start line, or a note when none resolve (section 1.2).
/// `graph: None` gives the no-graph note.
pub fn skeleton(graph: Option<&Graph>, file: &str) -> SkeletonResult {
    let Some(graph) = graph else {
        return SkeletonResult {
            file: file.to_string(),
            entries: Vec::new(),
            note: Some(format!(
                "no wiring graph — run `{} build` first",
                sieve_core::product().name
            )),
        };
    };

    let mut defs: Vec<&Node> = graph
        .nodes
        .iter()
        .filter(|n| n.kind != Kind::File && n.path == file)
        .collect();

    if defs.is_empty() {
        let suffix = format!("/{file}");
        let mut seen = std::collections::HashSet::new();
        let mut matched_paths: Vec<&str> = Vec::new();
        for n in &graph.nodes {
            if (n.path == file || n.path.ends_with(&suffix)) && seen.insert(n.path.as_str()) {
                matched_paths.push(n.path.as_str());
            }
        }
        if matched_paths.len() > 1 {
            matched_paths.sort_by(|a, b| code_unit_cmp(a, b));
            return SkeletonResult {
                file: file.to_string(),
                entries: Vec::new(),
                note: Some(format!("ambiguous — matches: {}", matched_paths.join(", "))),
            };
        }
        if let Some(path) = matched_paths.first() {
            defs = graph
                .nodes
                .iter()
                .filter(|n| n.kind != Kind::File && n.path == *path)
                .collect();
        }
    }

    if defs.is_empty() {
        return SkeletonResult {
            file: file.to_string(),
            entries: Vec::new(),
            note: Some("no definitions indexed for this file".to_string()),
        };
    }

    defs.sort_by_key(|n| start_line(&n.span));

    let entries = defs
        .iter()
        .map(|n| SkeletonEntry {
            name: n.name.clone(),
            kind: n.kind,
            span: n.span.clone(),
            signature: n.signature.clone(),
            summary: n.summary.as_deref().and_then(first_summary_line),
        })
        .collect();

    SkeletonResult {
        file: defs[0].path.clone(),
        entries,
        note: None,
    }
}

/// Renders a [`SkeletonResult`] as the plain-text `skeleton` report,
/// without the savings header — the CLI prepends that (section 1.4 to 1.6).
pub fn format_skeleton(r: &SkeletonResult) -> String {
    let head = format!("{} skeleton — {}", sieve_core::product().name, r.file);
    if r.entries.is_empty() {
        let note = r.note.as_deref().unwrap_or("no definitions.");
        return format!("{head}\n\n{note}\n");
    }
    let lines: Vec<String> = r
        .entries
        .iter()
        .map(|e| {
            let sig = e
                .signature
                .as_deref()
                .map(|s| format!("  {s}"))
                .unwrap_or_default();
            let sum = e
                .summary
                .as_deref()
                .map(|s| format!(" — {s}"))
                .unwrap_or_default();
            format!(
                "- {}  {} {}{sig}{sum}",
                e.span,
                crate::ask::kind_word(e.kind),
                e.name
            )
        })
        .collect();
    format!("{head}\n{}\n", lines.join("\n"))
}

/// The paths `skeleton`'s tokens-saved baseline reads: the resolved file
/// alone, or none when the result holds no entries (section 0).
pub fn skeleton_saved_paths(r: &SkeletonResult) -> Vec<String> {
    if r.entries.is_empty() {
        Vec::new()
    } else {
        vec![r.file.clone()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sieve_core::{Meta, Origin, SummaryState};

    fn node(path: &str, name: &str, kind: Kind, span: &str, signature: Option<&str>) -> Node {
        Node {
            id: format!("{path}#{name}"),
            name: name.to_string(),
            kind,
            owner: None,
            path: path.to_string(),
            span: span.to_string(),
            signature: signature.map(str::to_string),
            exported: true,
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

    fn file_node(path: &str) -> Node {
        node(path, path, Kind::File, "L1-L1", None)
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

    #[test]
    fn no_graph_gives_the_no_graph_note() {
        let r = skeleton(None, "a.ts");
        assert_eq!(r.file, "a.ts");
        assert_eq!(
            r.note.as_deref(),
            Some("no wiring graph — run `sieve build` first")
        );
    }

    #[test]
    fn entries_sort_by_start_line_and_report_the_resolved_path() {
        let g = graph(vec![
            file_node("a.ts"),
            node("a.ts", "b", Kind::Function, "L10-L11", Some("function b()")),
            node("a.ts", "a", Kind::Function, "L1-L2", Some("function a()")),
        ]);
        let r = skeleton(Some(&g), "a.ts");
        assert_eq!(r.file, "a.ts");
        assert_eq!(r.entries.len(), 2);
        assert_eq!(r.entries[0].name, "a");
        assert_eq!(r.entries[1].name, "b");
    }

    #[test]
    fn ambiguous_basename_reports_a_code_unit_sorted_note() {
        let g = graph(vec![
            file_node("packages/beta/src/render.ts"),
            file_node("packages/alpha/src/render.ts"),
        ]);
        let r = skeleton(Some(&g), "render.ts");
        assert!(r.entries.is_empty());
        assert_eq!(
            r.note.as_deref(),
            Some("ambiguous — matches: packages/alpha/src/render.ts, packages/beta/src/render.ts")
        );
    }

    #[test]
    fn no_match_gives_the_no_definitions_note() {
        let g = graph(vec![file_node("a.ts")]);
        let r = skeleton(Some(&g), "nope.ts");
        assert_eq!(
            r.note.as_deref(),
            Some("no definitions indexed for this file")
        );
    }

    #[test]
    fn format_skeleton_renders_the_entry_line_shape() {
        let g = graph(vec![node(
            "a.ts",
            "run",
            Kind::Function,
            "L1-L2",
            Some("function run(): void"),
        )]);
        let r = skeleton(Some(&g), "a.ts");
        assert_eq!(
            format_skeleton(&r),
            "sieve skeleton — a.ts\n- L1-L2  function run  function run(): void\n"
        );
    }

    /// `skeleton` appends the summary's first line, trimmed, after the
    /// signature; a whitespace-only summary drops the key entirely
    /// (the `deep-tier.md` note section 2.10).
    #[test]
    fn test_p2_10_skeleton_appends_the_summarys_first_line() {
        let mut n = node(
            "a.ts",
            "run",
            Kind::Function,
            "L1-L2",
            Some("function run(): void"),
        );
        n.summary = Some("Runs the main loop.\nMore detail never reaches skeleton.".to_string());
        let r = skeleton(Some(&graph(vec![n])), "a.ts");
        assert_eq!(r.entries[0].summary.as_deref(), Some("Runs the main loop."));
        assert_eq!(
            format_skeleton(&r),
            "sieve skeleton — a.ts\n- L1-L2  function run  function run(): void — Runs the main loop.\n"
        );
    }

    #[test]
    fn test_p2_10_a_whitespace_only_summary_drops_the_key() {
        let mut n = node(
            "a.ts",
            "run",
            Kind::Function,
            "L1-L2",
            Some("function run(): void"),
        );
        n.summary = Some("   \n".to_string());
        let r = skeleton(Some(&graph(vec![n])), "a.ts");
        assert!(r.entries[0].summary.is_none());
    }

    #[test]
    fn format_skeleton_prints_a_blank_line_then_the_note_on_zero_entries() {
        let r = skeleton(None, "a.ts");
        assert_eq!(
            format_skeleton(&r),
            "sieve skeleton — a.ts\n\nno wiring graph — run `sieve build` first\n"
        );
    }

    #[test]
    fn skeleton_saved_paths_is_empty_on_zero_entries() {
        let r = skeleton(None, "a.ts");
        assert!(skeleton_saved_paths(&r).is_empty());
    }

    #[test]
    fn skeleton_saved_paths_holds_the_resolved_file_alone() {
        let g = graph(vec![node(
            "a.ts",
            "run",
            Kind::Function,
            "L1-L2",
            Some("function run(): void"),
        )]);
        let r = skeleton(Some(&g), "a.ts");
        assert_eq!(skeleton_saved_paths(&r), vec!["a.ts".to_string()]);
    }
}
