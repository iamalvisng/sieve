//! Lean `ask --def`: the answer for a one-identifier question. It prints the
//! definition rows, the body of the top definition, and a callers line.

use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;
use sieve_core::collate::collate;
use sieve_core::{Graph, Kind, Node};

use super::format::row_of;
use super::structural::{incoming_counts, resolve_symbol, walked_edges};
use super::{is_test_path, parse_span_pointer, pointer_of, slice_span_capped, to_hit, wants_tests};
use crate::grep::{grep_graph, GrepOptions};

/// The most definition rows the answer prints.
const MAX_DEF_ROWS: usize = 5;
/// The most body lines the answer prints.
const MAX_BODY_LINES: usize = 60;
/// The most body UTF-16 units the answer prints.
const MAX_BODY_UNITS: usize = 2800;
/// The most grep rows the 0-definition fallback prints.
const MAX_GREP_ROWS: usize = 15;

/// A query that is one identifier, with at most one qualifier. A `::`
/// qualifier is stored as `.`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ident(String);

impl Ident {
    /// The identifier text, such as `checkScene` or `Cache.get`.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The last segment: the name that a definition node carries.
    fn name(&self) -> &str {
        self.0.rsplit('.').next().unwrap_or(&self.0)
    }
}

fn ident_re() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^[A-Za-z_$][A-Za-z0-9_$]*((\.|::)[A-Za-z_$][A-Za-z0-9_$]*)?$").ok()
    })
    .as_ref()
}

/// Returns the identifier when the trimmed query is one identifier. A query
/// that starts with `$` is not one: it takes the ranked `ask` path.
pub fn single_ident(query: &str) -> Option<Ident> {
    let q = query.trim();
    if q.starts_with('$') || !ident_re()?.is_match(q) {
        return None;
    }
    Some(Ident(q.replace("::", ".")))
}

/// The definitions of an identifier, top first, and the callers count.
#[derive(Debug)]
pub struct Def<'g> {
    /// The definition nodes, top first.
    pub nodes: Vec<&'g Node>,
    /// The count of distinct nodes with an incoming edge to a definition. The
    /// edges are the four incoming relations (calls, references, implements,
    /// extends), not `calls` only.
    pub callers: usize,
}

fn span_start(node: &Node) -> u32 {
    parse_span_pointer(&pointer_of(node)).map_or(0, |(_, from, _)| from)
}

/// Finds the definitions of `ident` (File nodes dropped) and sorts them by:
/// exact-case name, non-test path, exported, more incoming edges, then path
/// and span start.
pub fn define<'g>(graph: &'g Graph, ident: &Ident, in_prefix: Option<&str>) -> Def<'g> {
    let lower = ident.as_str().to_lowercase();
    let qualified = ident.as_str().contains('.');
    let nodes: Vec<&Node> = resolve_symbol(graph, ident.as_str(), in_prefix)
        .into_iter()
        .filter(|n| n.kind != Kind::File)
        // A qualified query drops the bare-name fallback matches.
        .filter(|n| !qualified || n.id.to_lowercase().contains(&lower))
        .collect();
    let tests_ok = wants_tests(ident.as_str());
    let counts = incoming_counts(graph);
    let incoming = |n: &Node| counts.get(n.id.as_str()).copied().unwrap_or(0);
    let mut keyed: Vec<_> = nodes
        .into_iter()
        .map(|n| {
            let key = (
                n.name != ident.name(),
                !tests_ok && is_test_path(&n.path),
                !n.exported,
                std::cmp::Reverse(incoming(n)),
            );
            (key, n)
        })
        .collect();
    keyed.sort_by(|(ka, a), (kb, b)| {
        ka.cmp(kb)
            .then_with(|| collate(&a.path, &b.path))
            .then_with(|| span_start(a).cmp(&span_start(b)))
    });
    let nodes: Vec<&Node> = keyed.into_iter().map(|(_, n)| n).collect();
    let ids: HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    let others: HashSet<&str> = walked_edges(graph, &ids, false)
        .into_iter()
        .map(|(_, other)| other)
        .collect();
    Def {
        nodes,
        callers: others.len(),
    }
}

/// A fence of backticks that no body line can close.
fn fence_for(body: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in body.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    "`".repeat((longest + 1).max(3))
}

/// Up to [`MAX_GREP_ROWS`] indexed rows for the fixed string, then a
/// `+N more` line when grep cut some. An empty vector means no row. The
/// second value counts the indexed files that grep could not read. For a
/// qualified identifier, grep searches the last segment.
fn grep_rows(
    graph: &Graph,
    repo_root: &Path,
    ident: &Ident,
    in_prefix: Option<&str>,
) -> (Vec<String>, usize) {
    let opts = GrepOptions {
        ignore_case: false,
        fixed: true,
        in_prefix: in_prefix.map(str::to_string),
        max_hits: MAX_GREP_ROWS,
    };
    // A prefix with nothing indexed is an error in grep: it gives no row here.
    let Ok(result) = grep_graph(graph, repo_root, ident.name(), &opts) else {
        return (Vec::new(), 0);
    };
    let mut rows: Vec<String> = result
        .groups
        .iter()
        .flat_map(|g| {
            g.hits
                .iter()
                .map(|h| format!("{}:{}: {}", g.path, h.line, h.text))
        })
        .collect();
    if result.truncated.hits > 0 {
        let name = sieve_core::product().name;
        rows.push(format!(
            "+{} more \u{b7} {name} find {} --fixed",
            result.truncated.hits,
            ident.name()
        ));
    }
    (rows, result.truncated.files)
}

/// Renders the lean answer. The CLI adds the savings line.
pub fn format_def(
    graph: &Graph,
    repo_root: &Path,
    ident: &Ident,
    def: &Def<'_>,
    in_prefix: Option<&str>,
) -> String {
    let name = sieve_core::product().name;
    let q = ident.as_str();
    let mut out = vec![format!("query  {q} \u{b7} def"), String::new()];
    let Some(top) = def.nodes.first() else {
        let (rows, unreadable) = grep_rows(graph, repo_root, ident, in_prefix);
        if rows.is_empty() {
            let mut line = format!("no definition and no occurrence of {q} in the index");
            if unreadable > 0 {
                line.push_str(&format!(" ({unreadable} indexed files unreadable)"));
            }
            out.push(line);
        } else {
            out.extend(rows);
        }
        return out.join("\n") + "\n";
    };
    for node in def.nodes.iter().take(MAX_DEF_ROWS) {
        out.push(row_of(&to_hit(node, 1.0, None)));
    }
    if def.nodes.len() > MAX_DEF_ROWS {
        out.push(format!(
            "+{} more \u{b7} {name} find {}",
            def.nodes.len() - MAX_DEF_ROWS,
            ident.name()
        ));
    }
    let body = parse_span_pointer(&pointer_of(top)).and_then(|(path, from, to)| {
        slice_span_capped(
            repo_root,
            path,
            from,
            to,
            MAX_BODY_LINES,
            Some(MAX_BODY_UNITS),
        )
    });
    if let Some(body) = body.filter(|b| !b.is_empty()) {
        let fence = fence_for(&body);
        out.push(fence.clone());
        out.push(body);
        out.push(fence);
    }
    out.push(if def.callers == 0 {
        "callers: 0".to_string()
    } else {
        format!("callers: {} \u{b7} {name} callers {q}", def.callers)
    });
    out.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;
    use sieve_core::{Confidence, Edge, Meta, Node as WNode, Origin, Relation, SummaryState};

    fn node(id: &str, name: &str, path: &str, span: &str, exported: bool) -> WNode {
        WNode {
            id: id.to_string(),
            name: name.to_string(),
            kind: Kind::Function,
            owner: None,
            path: path.to_string(),
            span: span.to_string(),
            signature: None,
            exported,
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

    fn file(path: &str) -> WNode {
        let mut n = node(path, path, path, "L1-L1", false);
        n.kind = Kind::File;
        n
    }

    fn calls(source: &str, target: &str) -> Edge {
        Edge {
            source: source.to_string(),
            target: target.to_string(),
            relation: Relation::Calls,
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

    fn id(q: &str) -> Ident {
        single_ident(q).expect("an identifier")
    }

    /// Writes `a.ts` with `n` lines `line1`..`lineN` and returns a graph with
    /// the function `run` over all of them.
    fn one_fn(dir: &TempDir, n: usize) -> Graph {
        let text = (1..=n).map(|i| format!("line{i}")).collect::<Vec<_>>();
        std::fs::write(dir.join("a.ts"), text.join("\n")).expect("write");
        graph(
            vec![
                file("a.ts"),
                node("a.ts#run", "run", "a.ts", &format!("L1-L{n}"), true),
            ],
            vec![],
        )
    }

    fn render(g: &Graph, dir: &TempDir, q: &str, prefix: Option<&str>) -> String {
        let ident = id(q);
        format_def(g, dir, &ident, &define(g, &ident, prefix), prefix)
    }

    #[test]
    fn test_def_single_ident_accepts_a_name_and_one_qualifier() {
        assert_eq!(id("checkScene").as_str(), "checkScene");
        assert_eq!(id("  run  ").as_str(), "run");
        assert_eq!(id("Cache.get").as_str(), "Cache.get");
        assert_eq!(id("Cache::get").as_str(), "Cache.get");
        assert_eq!(id("_a$1").as_str(), "_a$1");
    }

    #[test]
    fn test_def_single_ident_rejects_a_phrase_a_regex_and_a_path() {
        for q in [
            "who calls run",
            "run now",
            "run.*",
            "a|b",
            "src/check.ts",
            "a.b.c",
            "a::b::c",
            "\"run\"",
            "1run",
            "",
            "$S",
            "$S.x",
        ] {
            assert_eq!(single_ident(q), None, "{q}");
        }
    }

    #[test]
    fn test_def_prints_the_row_the_body_and_the_callers_line() {
        let dir = TempDir::new("def-basic");
        let mut g = one_fn(&dir, 20);
        g.nodes.push(node("b.ts#go", "go", "b.ts", "L1-L2", false));
        g.edges.push(calls("b.ts#go", "a.ts#run"));
        g.edges.push(calls("b.ts#go", "a.ts#run"));
        let out = render(&g, &dir, "run", None);
        let body = (1..=20)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let name = sieve_core::product().name;
        let want = format!(
            "query  run \u{b7} def\n\nrun  fn  a.ts:1-20\n```\n{body}\n```\ncallers: 1 \u{b7} {name} callers run\n"
        );
        assert_eq!(out, want);
        // Measured: a 20-line body.
        eprintln!("def-20-lines tokens={}", out.chars().count() / 4);
        let none = render(&one_fn(&dir, 3), &dir, "run", None);
        assert!(none.ends_with("```\ncallers: 0\n"));
    }

    #[test]
    fn test_def_lists_up_to_5_definitions_top_first() {
        let dir = TempDir::new("def-five");
        let nodes: Vec<WNode> = (1..=7)
            .map(|i| {
                let path = format!("f{i}.ts");
                node(&format!("{path}#run"), "run", &path, "L1-L2", true)
            })
            .collect();
        let g = graph(nodes, vec![]);
        let out = render(&g, &dir, "run", None);
        let name = sieve_core::product().name;
        assert!(out.contains("run  fn  f1.ts:1-2\nrun  fn  f2.ts:1-2\n"));
        assert!(out.contains("run  fn  f5.ts:1-2\n"));
        assert!(!out.contains("f6.ts:1-2"));
        assert!(out.contains(&format!("+2 more \u{b7} {name} find run\n")));
        // No file on disk: the answer prints no body, only what exists.
        assert!(!out.contains("```"));
        assert!(out.ends_with("callers: 0\n"));
    }

    #[test]
    fn test_def_top_order_prefers_exact_case_then_non_test_then_exported() {
        let g = graph(
            vec![
                node("z.ts#Run", "Run", "z.ts", "L1-L2", true),
                node("a.test.ts#run", "run", "a.test.ts", "L1-L2", true),
                node("b.ts#run", "run", "b.ts", "L1-L2", false),
                node("c.ts#run", "run", "c.ts", "L1-L2", true),
                node("d.ts#run", "run", "d.ts", "L1-L2", true),
                node("e.ts#caller", "caller", "e.ts", "L1-L2", false),
            ],
            vec![calls("e.ts#caller", "d.ts#run")],
        );
        let def = define(&g, &id("run"), None);
        let order: Vec<&str> = def.nodes.iter().map(|n| n.id.as_str()).collect();
        // d: most incoming. c: exported, no incoming. b: not exported.
        // a.test: test path. z: case-folded match.
        assert_eq!(
            order,
            [
                "d.ts#run",
                "c.ts#run",
                "b.ts#run",
                "a.test.ts#run",
                "z.ts#Run"
            ]
        );
        assert_eq!(def.callers, 1);
    }

    #[test]
    fn test_def_cuts_the_body_at_60_lines() {
        let dir = TempDir::new("def-60");
        let g = one_fn(&dir, 100);
        let out = render(&g, &dir, "run", None);
        assert!(out.contains("\nline60\n"));
        assert!(!out.contains("line61\n"));
        assert!(out.contains("\n… (+40 more lines; open a.ts:L61-L100)\n```\n"));
        // Measured: a 60-line body.
        let full = render(&one_fn(&dir, 60), &dir, "run", None);
        eprintln!("def-60-lines tokens={}", full.chars().count() / 4);
    }

    #[test]
    fn test_def_cuts_the_body_at_2800_utf16_units_on_a_line_end() {
        let dir = TempDir::new("def-units");
        // Each line holds 100 UTF-16 units (50 chars of 2 units each).
        let line = "\u{1F600}".repeat(50);
        let text = vec![line.as_str(); 40].join("\n");
        std::fs::write(dir.join("a.ts"), text).expect("write");
        let g = graph(
            vec![
                file("a.ts"),
                node("a.ts#run", "run", "a.ts", "L1-L40", true),
            ],
            vec![],
        );
        let out = render(&g, &dir, "run", None);
        // 100 + 26 * 101 = 2726 fits; the 28th line needs 2827.
        assert_eq!(out.matches(&line).count(), 27);
        assert!(out.contains("\n… (+13 more lines; open a.ts:L28-L40)\n"));
    }

    #[test]
    fn test_def_prints_at_least_one_body_line() {
        let dir = TempDir::new("def-one-line");
        let long = "x".repeat(3000);
        std::fs::write(dir.join("a.ts"), format!("{long}\nshort\nlast")).expect("write");
        let g = graph(
            vec![file("a.ts"), node("a.ts#run", "run", "a.ts", "L1-L3", true)],
            vec![],
        );
        let out = render(&g, &dir, "run", None);
        // The first line prints, cut to the 2800-unit cap.
        let kept = "x".repeat(2800);
        assert!(out.contains(&format!(
            "```\n{kept}\n… (+2 more lines; open a.ts:L2-L3) (line cut)\n```"
        )));
    }

    #[test]
    fn test_def_cuts_a_first_line_over_the_cap_at_a_char_boundary() {
        let dir = TempDir::new("def-cut-line");
        let long = "\u{1F600}".repeat(2500);
        std::fs::write(dir.join("a.ts"), format!("{long}\nshort")).expect("write");
        let g = graph(
            vec![file("a.ts"), node("a.ts#run", "run", "a.ts", "L1-L2", true)],
            vec![],
        );
        let out = render(&g, &dir, "run", None);
        let kept = "\u{1F600}".repeat(1400);
        assert!(out.contains(&format!(
            "```\n{kept}\n… (+1 more lines; open a.ts:L2-L2) (line cut)\n```"
        )));
    }

    #[test]
    fn test_def_qualified_ident_drops_bare_name_matches() {
        let g = graph(
            vec![
                node("a.ts#Cache.get", "get", "a.ts", "L1-L2", false),
                node("b.ts#Other.get", "get", "b.ts", "L1-L2", true),
            ],
            vec![],
        );
        let def = define(&g, &id("Cache.get"), None);
        let ids: Vec<&str> = def.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, ["a.ts#Cache.get"]);
    }

    #[test]
    fn test_def_with_no_definition_greps_the_last_segment_of_a_qualifier() {
        let dir = TempDir::new("def-qual-grep");
        std::fs::write(dir.join("a.rs"), "let x = A::b();\n").expect("write");
        let g = graph(vec![file("a.rs")], vec![]);
        let out = render(&g, &dir, "A::b", None);
        assert!(out.contains("a.rs:1: let x = A::b();\n"));
    }

    #[test]
    fn test_def_with_no_definition_and_no_row_counts_unreadable_files() {
        let dir = TempDir::new("def-unreadable");
        let g = graph(vec![file("gone.ts")], vec![]);
        let out = render(&g, &dir, "minFont", None);
        assert!(out.ends_with("in the index (1 indexed files unreadable)\n"));
    }

    #[test]
    fn test_def_with_no_definition_prints_at_most_15_grep_rows() {
        let dir = TempDir::new("def-grep");
        let text = (1..=20)
            .map(|i| format!("use(minFont, {i})"))
            .collect::<Vec<_>>();
        std::fs::write(dir.join("a.ts"), text.join("\n")).expect("write");
        let g = graph(vec![file("a.ts")], vec![]);
        let out = render(&g, &dir, "minFont", None);
        let name = sieve_core::product().name;
        assert!(out.starts_with("query  minFont \u{b7} def\n\n"));
        assert!(out.contains("a.ts:1: use(minFont, 1)\n"));
        assert!(out.contains("a.ts:15: use(minFont, 15)\n"));
        assert!(!out.contains("a.ts:16:"));
        assert!(out.ends_with(&format!("+5 more \u{b7} {name} find minFont --fixed\n")));
    }

    #[test]
    fn test_def_with_no_definition_and_no_row_prints_one_line() {
        let dir = TempDir::new("def-none");
        std::fs::write(dir.join("a.ts"), "nothing here\n").expect("write");
        let g = graph(vec![file("a.ts")], vec![]);
        let out = render(&g, &dir, "minFont", None);
        assert_eq!(
            out,
            "query  minFont \u{b7} def\n\nno definition and no occurrence of minFont in the index\n"
        );
    }

    #[test]
    fn test_def_in_prefix_narrows_to_one_file() {
        let dir = TempDir::new("def-in");
        std::fs::write(dir.join("a.ts"), "x\ny\n").expect("write");
        std::fs::write(dir.join("b.ts"), "p\nq\n").expect("write");
        let g = graph(
            vec![
                file("a.ts"),
                file("b.ts"),
                node("a.ts#run", "run", "a.ts", "L1-L2", true),
                node("b.ts#run", "run", "b.ts", "L1-L2", true),
            ],
            vec![],
        );
        let out = render(&g, &dir, "run", Some("b.ts"));
        assert!(out.contains("run  fn  b.ts:1-2\n```\np\nq\n```\n"));
        assert!(!out.contains("a.ts"));
    }
}
