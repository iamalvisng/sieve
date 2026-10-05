//! The `viz --export` data (P1-65): the context graph assembled from the
//! top-level `<context_dir>/*.md` files and the one-file page that inlines the
//! viewer, both graphs and the stylesheet.
//!
//! The viewer files are original Sieve code (`crates/sieve-cli/assets/
//! viewer/`). In an exported page, `app.js` reads `window.__SIEVE_DATA__`
//! and its two keys, `contextGraph` and `codeGraph`. The names stay the
//! same under every product name.

use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;
use serde::Serialize;
use sieve_core::concept::ConceptNode;
use sieve_core::wiring::Graph;
use thiserror::Error;

/// The tab names an exported page can offer, in order
/// (`VIZ_TABS`). `--tabs` takes a comma-separated subset.
pub const TABS: [&str; 3] = ["context", "code", "outline"];

const GEN_START: &str = "<!-- context:generated:start -->";
const GEN_END: &str = "<!-- context:generated:end -->";
const SUMMARY_MAX: usize = 500;

/// The two tags the exporter rewrites. `viewer/index.html` must hold both.
const LINK_TAG: &str = r#"<link rel="stylesheet" href="/style.css">"#;
const SCRIPT_TAG: &str = r#"<script type="module" src="/app.js"></script>"#;

/// Why an export did not produce a page.
#[derive(Debug, Error)]
pub enum VizError {
    /// `index.html` no longer carries the two tags the exporter rewrites.
    #[error(
        "the viewer page does not match the tags the exporter rewrites \u{2014} rebuild sieve from source"
    )]
    TagsMissing,
    /// A graph did not serialize.
    #[error("the viz export failed: {0} \u{2014} run sieve viz again")]
    Json(#[from] serde_json::Error),
}

/// One quoted line in a node's evidence block. `n` is `null` for a
/// deleted line.
#[derive(Debug, Clone, Serialize)]
pub struct EvidenceLine {
    pub n: Option<u32>,
    pub sign: char,
    pub text: String,
}

/// One evidence block on a node: a label, an optional note, the quoted
/// lines, and how many more lines were left out.
#[derive(Debug, Clone, Serialize)]
pub struct Evidence {
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub lines: Vec<EvidenceLine>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub more: Option<usize>,
}

/// One owner as the page carries it: no score, since a weight nobody can
/// check is not evidence (`nodeOwners`).
#[derive(Debug, Clone, Serialize)]
pub struct NodeOwner {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    pub commits: u32,
    /// The newest commit's day, `YYYY-MM-DD` in UTC.
    pub last: String,
}

/// One concept node as the viewer renders it. `evidence` and `owners`
/// are only set on a blast node.
#[derive(Debug, Clone, Serialize)]
pub struct ContextNode {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub node_type: String,
    pub summary: String,
    pub sources: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Vec<Evidence>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owners: Option<Vec<NodeOwner>>,
}

/// One concept edge as the viewer renders it.
#[derive(Debug, Clone, Serialize)]
pub struct ContextEdge {
    pub source: String,
    pub target: String,
    pub relation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// The assembled context graph: counts, nodes and the edges that survived
/// the dangling-target filter.
#[derive(Debug, Clone)]
pub struct ContextGraph {
    pub node_count: usize,
    pub edge_count: usize,
    /// What an empty canvas means. Only a blast graph sets it.
    pub empty_note: Option<String>,
    pub skipped_files: usize,
    pub dropped_edges: usize,
    pub nodes: Vec<ContextNode>,
    pub edges: Vec<ContextEdge>,
}

/// Maps the four legacy vague verbs to the concrete verb each usually
/// means (`NORMALIZE_RELATION`). Every other verb passes through.
pub fn normalize_relation(rel: &str) -> &str {
    match rel {
        "influences" => "configures",
        "supports" | "defines" | "measures" => "validates",
        other => other,
    }
}

/// `^##\s+Summary\s*$` with the JS `m` flag: the heading line the summary
/// drops, first match only.
fn summary_heading() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?m)^##\s+Summary\s*$").expect("valid regex"))
}

/// The prose summary out of a node body's generated block, cut at
/// `## Related`, trimmed, and capped at 500 UTF-16 units plus `…`
/// (`extractSummary`). JS slices strings by UTF-16 unit, so the cap counts
/// units, not chars.
fn extract_summary(body: &str) -> String {
    let mut text = body;
    if let (Some(start), Some(end)) = (text.find(GEN_START), text.find(GEN_END)) {
        if end > start {
            text = &text[start + GEN_START.len()..end];
        }
    }
    if let Some(related) = text.find("## Related") {
        text = &text[..related];
    }
    let text = summary_heading().replacen(text, 1, "");
    let text = text.trim();
    let units: Vec<u16> = text.encode_utf16().collect();
    if units.len() > SUMMARY_MAX {
        let head = String::from_utf16_lossy(&units[..SUMMARY_MAX]);
        format!("{}…", head.trim_end())
    } else {
        text.to_string()
    }
}

/// Reads every top-level `*.md` in `context_dir`, sorted by name, into the
/// render-ready graph (`assembleContextGraph`). A missing dir assembles to
/// an empty graph. An edge whose target is not a node drops, and the drop
/// count lands in `dropped_edges`.
pub fn assemble_context_graph(context_dir: &Path) -> ContextGraph {
    let mut names: Vec<String> = std::fs::read_dir(context_dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| n.ends_with(".md"))
                .collect()
        })
        .unwrap_or_default();
    names.sort();

    let mut nodes = Vec::new();
    let mut raw_edges = Vec::new();
    for name in &names {
        let Ok(text) = std::fs::read_to_string(context_dir.join(name)) else {
            continue;
        };
        // ponytail: gray-matter throws on bad YAML and a throw counts the
        // file in `skippedFiles`; `ConceptNode::parse` reads bad YAML as
        // empty fields instead, so Sieve counts no skip. Add a checked
        // parse in `sieve_core::concept` if a golden ever needs the count.
        let stem = name.trim_end_matches(".md");
        let node = ConceptNode::parse(stem, &text);
        let id = node.slug.clone();
        for link in &node.links {
            raw_edges.push(ContextEdge {
                source: id.clone(),
                target: link.to.clone(),
                relation: normalize_relation(&link.relation).to_string(),
                description: link.description.clone().filter(|d| !d.is_empty()),
            });
        }
        nodes.push(ContextNode {
            name: if node.name.is_empty() {
                id.clone()
            } else {
                node.name
            },
            node_type: if node.kind_type.is_empty() {
                "concept".to_string()
            } else {
                node.kind_type
            },
            summary: extract_summary(&node.body),
            sources: node.sources.into_iter().map(|s| s.path).collect(),
            evidence: None,
            owners: None,
            id,
        });
    }
    let known: std::collections::HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    let raw_count = raw_edges.len();
    let edges: Vec<ContextEdge> = raw_edges
        .into_iter()
        .filter(|e| known.contains(e.source.as_str()) && known.contains(e.target.as_str()))
        .collect();
    ContextGraph {
        node_count: nodes.len(),
        edge_count: edges.len(),
        empty_note: None,
        skipped_files: 0,
        dropped_edges: raw_count - edges.len(),
        nodes,
        edges,
    }
}

/// The `meta` object of the exported context graph: the assembly counts,
/// then the page fields `exportViz` adds, in the key order.
#[derive(Serialize)]
struct ExportMeta<'a> {
    #[serde(rename = "nodeCount")]
    node_count: usize,
    #[serde(rename = "edgeCount")]
    edge_count: usize,
    #[serde(rename = "emptyNote", skip_serializing_if = "Option::is_none")]
    empty_note: Option<&'a str>,
    #[serde(rename = "skippedFiles")]
    skipped_files: usize,
    #[serde(rename = "droppedEdges")]
    dropped_edges: usize,
    #[serde(rename = "repoName")]
    repo_name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    subtitle: Option<&'a str>,
    #[serde(rename = "defaultTab")]
    default_tab: &'a str,
    tabs: &'a [String],
}

/// The exported context graph document.
#[derive(Serialize)]
struct ExportGraph<'a> {
    meta: ExportMeta<'a>,
    nodes: &'a [ContextNode],
    edges: &'a [ContextEdge],
}

/// What the exporter needs beside the viewer files.
#[derive(Debug, Clone)]
pub struct ExportOptions<'a> {
    /// The context dir: `<root>/sieve` or the `--dir` override.
    pub context_dir: &'a Path,
    /// The repo root's basename, shown beside the brand.
    pub repo_name: &'a str,
    /// `--title`: the subtitle beside the repo name, when given.
    pub subtitle: Option<&'a str>,
    /// `--tabs`, already validated. `None` means all three.
    pub tabs: Option<&'a [String]>,
}

/// The assembled page and the counts the CLI line prints.
#[derive(Debug, Clone)]
pub struct Export {
    /// The full `index.html` text.
    pub page: String,
    /// The context graph's node count.
    pub context_nodes: usize,
    /// The code graph's node count, 0 when the page carries none.
    pub code_nodes: usize,
}

/// The three viewer files: the page, the stylesheet and the script.
#[derive(Debug, Clone, Copy)]
pub struct Viewer<'a> {
    pub html: &'a str,
    pub css: &'a str,
    pub js: &'a str,
}

/// `<context_dir>/.graph/wiring.json` when it parses and
/// `meta.version === 1`, else `None` (`codeGraph`). The typed round trip
/// keeps the key order; a `serde_json::Value` would sort the keys.
fn code_graph(context_dir: &Path) -> Option<Graph> {
    let bytes = std::fs::read(context_dir.join(".graph").join("wiring.json")).ok()?;
    let graph: Graph = serde_json::from_slice(&bytes).ok()?;
    (graph.meta.version == 1).then_some(graph)
}

/// JSON safe inside a `<script>` element (`inlineJson`): `<` becomes
/// `\u003c`, and U+2028 and U+2029 become their escapes, because both are
/// legal in JSON and illegal in a JavaScript source line.
fn inline_json<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    Ok(serde_json::to_string(value)?
        .replace('<', "\\u003c")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029"))
}

/// Builds the one-file page (`exportViz`) from the context dir's own
/// concept cards: the stylesheet replaces the `<link>` tag, and the data
/// block plus the bundled script replace the `<script src>` tag. Both
/// tags are checked on the source first.
pub fn export_page(viewer: Viewer<'_>, opts: &ExportOptions<'_>) -> Result<Export, VizError> {
    if !viewer.html.contains(LINK_TAG) || !viewer.html.contains(SCRIPT_TAG) {
        return Err(VizError::TagsMissing);
    }
    export_page_with(viewer, opts, &assemble_context_graph(opts.context_dir))
}

/// Builds the one-file page around a supplied context graph
/// (`exportViz` with `contextGraph`), as `blast --export-viz` does.
pub fn export_page_with(
    viewer: Viewer<'_>,
    opts: &ExportOptions<'_>,
    context: &ContextGraph,
) -> Result<Export, VizError> {
    if !viewer.html.contains(LINK_TAG) || !viewer.html.contains(SCRIPT_TAG) {
        return Err(VizError::TagsMissing);
    }
    let all_tabs: Vec<String> = TABS.iter().map(|t| t.to_string()).collect();
    let tabs: &[String] = opts.tabs.unwrap_or(&all_tabs);
    // Both remaining tabs read the wiring graph, so dropping them drops
    // the payload as well.
    let wanted = tabs.iter().any(|t| t == "code" || t == "outline");
    let code = if wanted {
        code_graph(opts.context_dir)
    } else {
        None
    };
    let code_nodes = code.as_ref().map_or(0, |g| g.nodes.len());
    // The viewer starts on Context, which only a deep build fills. A
    // structural build assembles to INDEX alone, so the page opens on Code
    // when a code graph exists.
    let default_tab = if context.nodes.len() > 1 || code_nodes == 0 {
        "context"
    } else {
        "code"
    };
    let graph = ExportGraph {
        meta: ExportMeta {
            node_count: context.node_count,
            edge_count: context.edge_count,
            empty_note: context.empty_note.as_deref(),
            skipped_files: context.skipped_files,
            dropped_edges: context.dropped_edges,
            repo_name: opts.repo_name,
            subtitle: opts.subtitle,
            default_tab,
            tabs,
        },
        nodes: &context.nodes,
        edges: &context.edges,
    };
    let data = format!(
        "<script>window.__SIEVE_DATA__ = {{\n  contextGraph: {},\n  codeGraph: {}\n}};</script>",
        inline_json(&graph)?,
        inline_json(&code)?
    );
    let page = viewer
        .html
        .replacen(LINK_TAG, &format!("<style>\n{}\n</style>", viewer.css), 1)
        .replacen(
            SCRIPT_TAG,
            &format!("{data}\n<script type=\"module\">\n{}\n</script>", viewer.js),
            1,
        );
    Ok(Export {
        page,
        context_nodes: context.nodes.len(),
        code_nodes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;
    use std::fs;

    const HTML: &str = "<head>\n<link rel=\"stylesheet\" href=\"/style.css\">\n</head>\n<script type=\"module\" src=\"/app.js\"></script>\n";

    fn write(dir: &Path, name: &str, text: &str) {
        fs::write(dir.join(name), text).expect("write node");
    }

    #[test]
    fn test_p1_65_summary_takes_the_generated_block_and_caps_at_500_units() {
        let body = "intro\n<!-- context:generated:start -->\n## Summary\n\nA short summary.\n\n## Related\n- x\n<!-- context:generated:end -->\n";
        assert_eq!(extract_summary(body), "A short summary.");
        // No block: the whole body, trimmed.
        assert_eq!(
            extract_summary("\n# card\n\nbody text\n"),
            "# card\n\nbody text"
        );
        // 600 units of a non-ASCII BMP char: cut at 500, then `…`.
        let long = "é".repeat(600);
        let cut = extract_summary(&long);
        assert_eq!(cut.encode_utf16().count(), 501);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn test_p1_65_assemble_sorts_normalizes_and_drops_dangling_edges() {
        let dir = TempDir::new("viz-assemble");
        write(
            &dir,
            "b.md",
            "---\nname: Bee\nslug: b\ntype: system\nsources:\n  - path: src/b.ts\n    hash: h\nlinks:\n  - to: a\n    relation: supports\n  - to: nope\n    relation: uses\n---\n<!-- context:generated:start -->\n## Summary\n\nB.\n<!-- context:generated:end -->\n",
        );
        write(&dir, "a.md", "# a.md\n\nplain card\n");
        write(&dir, "notes.txt", "ignored");
        let g = assemble_context_graph(&dir);
        assert_eq!(
            g.nodes.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!(g.nodes[0].name, "a");
        assert_eq!(g.nodes[0].node_type, "concept");
        assert_eq!(g.nodes[0].summary, "# a.md\n\nplain card");
        assert_eq!(g.nodes[1].sources, ["src/b.ts"]);
        assert_eq!(g.edges.len(), 1);
        assert_eq!(g.edges[0].relation, "validates");
        assert_eq!((g.node_count, g.edge_count, g.dropped_edges), (2, 1, 1));
        assert_eq!(assemble_context_graph(&dir.join("missing")).node_count, 0);
    }

    #[test]
    fn test_p1_65_export_inlines_both_graphs_and_escapes_script_breakers() {
        let dir = TempDir::new("viz-export");
        write(&dir, "INDEX.md", "# a </script> b\u{2028}c\n");
        let viewer = Viewer {
            html: HTML,
            css: "body{}",
            js: "run()",
        };
        let opts = ExportOptions {
            context_dir: &dir,
            repo_name: "repo",
            subtitle: None,
            tabs: None,
        };
        let out = export_page(viewer, &opts).expect("export");
        assert!(out.page.contains("<style>\nbody{}\n</style>"));
        assert!(out.page.contains("\\u003c/script>"));
        assert!(out.page.contains("\\u2028"));
        assert!(out.page.contains("\"repoName\":\"repo\",\"defaultTab\":\"context\",\"tabs\":[\"context\",\"code\",\"outline\"]}"));
        assert!(out.page.contains(
            "  codeGraph: null\n};</script>\n<script type=\"module\">\nrun()\n</script>"
        ));
        assert_eq!((out.context_nodes, out.code_nodes), (1, 0));

        let tabs = ["context".to_string()];
        let opts = ExportOptions {
            subtitle: Some("PR #1"),
            tabs: Some(&tabs),
            ..opts
        };
        let out = export_page(viewer, &opts).expect("export");
        assert!(out
            .page
            .contains("\"subtitle\":\"PR #1\",\"defaultTab\":\"context\",\"tabs\":[\"context\"]}"));

        let bad = Viewer {
            html: "<html></html>",
            ..viewer
        };
        assert!(matches!(
            export_page(bad, &opts),
            Err(VizError::TagsMissing)
        ));
    }
}
