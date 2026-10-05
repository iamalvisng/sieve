//! Per-file wiring cards and the `INDEX.md` roster (section 4 and 5 of the
//! build-cards-freshness note).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::collate::collate;
use crate::concept::read_nodes;
use crate::wiring::{Graph, Kind, Node};

/// Turns a source path into its card path, by swapping the final extension
/// for `.md`.
///
/// A path with no extension gains `.md` (`"Makefile"` becomes
/// `"Makefile.md"`). A root-level source whose stem equals a live concept
/// slug in `concept_slugs` routes to `_root/` instead, so the per-file
/// wiring card never collides with that concept node's own file on disk
/// (the `deep-tier.md` note section 2.2).
pub fn card_path_for(source_path: &str, concept_slugs: &HashSet<String>) -> String {
    let search_start = source_path.rfind('/').map(|i| i + 1).unwrap_or(0);
    let stem = match source_path[search_start..].rfind('.') {
        Some(dot) => &source_path[..search_start + dot],
        None => source_path,
    };
    let md = format!("{stem}.md");
    if search_start > 0 {
        // A nested source: the collision cannot happen below the top level.
        return md;
    }
    if concept_slugs.contains(stem) {
        format!("_root/{md}")
    } else {
        md
    }
}

/// Returns the serialized, lowercase name of a node kind, for a card
/// bullet.
pub(crate) fn kind_str(kind: Kind) -> String {
    serde_json::to_string(&kind)
        .unwrap_or_default()
        .trim_matches('"')
        .to_string()
}

/// Parses the leading line number out of a `L<start>-L<end>` span.
///
/// Returns 0 when the span does not start with `L<digits>`.
pub(crate) fn span_start(span: &str) -> u32 {
    span.strip_prefix('L')
        .and_then(|rest| rest.split('-').next())
        .and_then(|digits| digits.parse().ok())
        .unwrap_or(0)
}

/// Returns the one-line summary for a card: the trimmed summary's first
/// line when non-empty, else the trimmed signature, else an empty string.
fn one_liner(node: Option<&Node>) -> String {
    let Some(node) = node else {
        return String::new();
    };
    let summary_line = node
        .summary
        .as_deref()
        .map(str::trim)
        .and_then(|summary| summary.lines().next())
        .map(str::trim)
        .unwrap_or("");
    if !summary_line.is_empty() {
        return summary_line.to_string();
    }
    node.signature.as_deref().unwrap_or("").trim().to_string()
}

/// Renders one card's Markdown text: the heading, the optional file
/// one-liner, and a bullet per symbol.
///
/// `uplinks` is the slug of every concept whose `sources` names this file,
/// already sorted in byte order (not ICU); each becomes a `[[slug]]` link
/// on the heading line, joined by one space (note
/// section 2.1). An empty list leaves the heading as a bare path.
pub fn render_card(
    source_path: &str,
    file_node: Option<&Node>,
    symbols: &[&Node],
    uplinks: &[String],
) -> String {
    let heading = if uplinks.is_empty() {
        format!("# {source_path}")
    } else {
        let links: Vec<String> = uplinks.iter().map(|slug| format!("[[{slug}]]")).collect();
        format!("# {source_path} · {}", links.join(" "))
    };
    let mut lines = vec![heading, String::new()];

    let file_summary = one_liner(file_node);
    if !file_summary.is_empty() {
        lines.push(file_summary);
        lines.push(String::new());
    }

    if symbols.is_empty() {
        lines.push("_No extracted symbols in this file._".to_string());
    } else {
        let mut sorted = symbols.to_vec();
        sorted.sort_by(|a, b| {
            span_start(&a.span)
                .cmp(&span_start(&b.span))
                .then_with(|| collate(&a.name, &b.name))
        });
        for node in sorted {
            let desc = one_liner(Some(node));
            let tail = if desc.is_empty() {
                String::new()
            } else {
                format!(" — {desc}")
            };
            lines.push(format!(
                "- {} · {} · {}{tail}",
                node.name,
                kind_str(node.kind),
                node.span
            ));
        }
    }

    lines.join("\n") + "\n"
}

/// The card write counts a build reports. `written` is the count of
/// distinct card paths, which the build line prints. `files` is the count
/// of source paths and `with_symbols` the count of those with at least one
/// symbol node: `INDEX.md` prints both. The two
/// differ when two source paths share one card path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CardStats {
    pub written: usize,
    pub files: usize,
    pub with_symbols: usize,
}

/// Writes every card for `graph` under `out_dir`, then prunes stale cards.
///
/// Every path in `graph.nodes` gets a card, written unconditionally. A file
/// node supplies the heading one-liner; every other node on that path becomes a
/// bullet. The graph file sorts a copy of `graph.nodes`, but `write_cards`
/// walks the unsorted list in build order. This function writes cards in the
/// order each path first appears in `graph.nodes`. When two source paths share
/// one card path, the later one wins on disk, with the last-write-wins card
/// count quirk (see the `build-cards-freshness.md` note).
pub fn write_cards(graph: &Graph, out_dir: &Path) -> io::Result<CardStats> {
    // One read of the concept layer per build, straight off disk: the
    // concept pass runs before the wiring pass (note section 1.1), so a
    // deep build's concept files already sit under `out_dir` here. With no
    // concept layer this returns empty and every branch below is a no-op,
    // so a Tier-1 build stays byte-identical to today's output.
    let concepts = read_nodes(out_dir);
    let concept_slugs: HashSet<String> = concepts.iter().map(|c| c.slug.clone()).collect();
    let mut uplinks_by_path: HashMap<String, Vec<String>> = HashMap::new();
    for concept in &concepts {
        for source in &concept.sources {
            uplinks_by_path
                .entry(source.path.clone())
                .or_default()
                .push(concept.slug.clone());
        }
    }
    for slugs in uplinks_by_path.values_mut() {
        // Byte order, not ICU: sorts up-links with the plain
        // `.sort()`.
        slugs.sort();
    }

    let mut by_path: HashMap<&str, (Option<&Node>, Vec<&Node>)> = HashMap::new();
    let mut path_order: Vec<&str> = Vec::new();
    for node in &graph.nodes {
        let path = node.path.as_str();
        if !by_path.contains_key(path) {
            path_order.push(path);
        }
        let entry = by_path.entry(path).or_default();
        if node.kind == Kind::File {
            entry.0 = Some(node);
        } else {
            entry.1.push(node);
        }
    }

    let mut card_has_symbols: HashMap<PathBuf, bool> = HashMap::new();
    let mut files = 0;
    let mut with_symbols = 0;
    for path in path_order {
        let (file_node, symbols) = &by_path[path];
        let rel = card_path_for(path, &concept_slugs);
        let full_path = out_dir.join(&rel);
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let empty: Vec<String> = Vec::new();
        let uplinks = uplinks_by_path.get(path).unwrap_or(&empty);
        let content = render_card(path, *file_node, symbols, uplinks);
        fs::write(&full_path, content)?;
        card_has_symbols.insert(full_path, !symbols.is_empty());
        files += 1;
        if !symbols.is_empty() {
            with_symbols += 1;
        }
    }

    let written: HashSet<PathBuf> = card_has_symbols.keys().cloned().collect();
    prune_stale_cards(out_dir, &written)?;

    Ok(CardStats {
        written: card_has_symbols.len(),
        files,
        with_symbols,
    })
}

/// Removes every `.md` file under a subdirectory of `out_dir` that this
/// build did not write, skipping `.cache` and `.graph`, then removes any
/// subdirectory left empty. A top-level card is never pruned.
fn prune_stale_cards(out_dir: &Path, written: &HashSet<PathBuf>) -> io::Result<()> {
    let entries = match fs::read_dir(out_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name();
        if name == ".cache" || name == ".graph" {
            continue;
        }
        prune_dir(&path, written)?;
        remove_if_empty(&path)?;
    }
    Ok(())
}

/// Recursively removes stale `.md` files under `dir`, then removes any
/// nested directory left empty, bottom-up.
fn prune_dir(dir: &Path, written: &HashSet<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            prune_dir(&path, written)?;
            remove_if_empty(&path)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("md")
            && !written.contains(&path)
        {
            fs::remove_file(&path)?;
        }
    }
    Ok(())
}

/// Removes `dir` when it holds no entries.
fn remove_if_empty(dir: &Path) -> io::Result<()> {
    let mut entries = fs::read_dir(dir)?;
    if entries.next().is_none() {
        fs::remove_dir(dir)?;
    }
    Ok(())
}

/// Writes `sieve/INDEX.md`: the fixed preamble, then a `## Files` section
/// when the build wrote at least one card.
pub fn write_index(out_dir: &Path, stats: &CardStats) -> io::Result<()> {
    let product = crate::product();
    let name = product.name;
    let mut lines = vec![
        format!("# {name} — repo map"),
        String::new(),
        "Small markdown nodes summarising this repo. `grep` any term, symbol, or".to_string(),
        format!(
            "filename here, or run `{name} ask \"<task>\"`. Each node carries prose plus exact"
        ),
        "`file:line`; open a source file only to edit the named span.".to_string(),
        String::new(),
        format!(
            "The same graph is queryable as MCP tools (`{}`, `{}`,",
            product.tool("find_code"),
            product.tool("find_all"),
        ),
        format!(
            "`{}`, `{}`, `{}`) where a host exposes them, and",
            product.tool("trace_calls"),
            product.tool("file_api"),
            product.tool("repo_map"),
        ),
        format!("as the `{name}` CLI everywhere else. Edges — who calls what — live only in the"),
        format!(
            "graph, not in these files: `{name} callers <symbol>` is the only way to read them."
        ),
        String::new(),
    ];

    // One read of the concept layer per build. With no concept node on
    // disk this is empty, and the section below never appears, so a
    // Tier-1 build's `INDEX.md` stays byte-identical to today's output.
    let concepts = read_nodes(out_dir);
    if !concepts.is_empty() {
        lines.push("## Concepts".to_string());
        lines.push(String::new());
        for concept in &concepts {
            let name: &str = if concept.name.is_empty() {
                &concept.slug
            } else {
                &concept.name
            };
            let sources: Vec<&str> = concept.sources.iter().map(|s| s.path.as_str()).collect();
            let tail = if sources.is_empty() {
                String::new()
            } else {
                format!(" · {}", sources.join(", "))
            };
            lines.push(format!(
                "- [{}]({}.md) — {name}{tail}",
                concept.slug, concept.slug
            ));
        }
        lines.push(String::new());
    }

    if stats.files > 0 {
        lines.push("## Files".to_string());
        lines.push(String::new());
        let dir = product.context_dir_name();
        lines.push(format!(
            "{} per-file wiring cards mirror the source tree under `{dir}/` ({} carry extracted symbols). They are deliberately not enumerated here —",
            stats.files, stats.with_symbols
        ));
        lines.push(format!(
            "`grep` a symbol or `find`/`ls` a filename under `{dir}/` to land on the card for that file."
        ));
        lines.push(String::new());
    }

    let text = lines.join("\n");
    fs::write(out_dir.join("INDEX.md"), text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wiring::{Confidence, Edge, Kind, Meta, Origin, Relation, SummaryState};

    /// A temp dir unique per call, that removes itself on drop, even if
    /// the test panics before it reaches its own cleanup line.
    fn unique_temp_dir(label: &str) -> crate::test_support::TempDir {
        crate::test_support::TempDir::new(label)
    }

    fn node(id: &str, name: &str, kind: Kind, path: &str, span: &str) -> Node {
        Node {
            id: id.to_string(),
            name: name.to_string(),
            kind,
            owner: None,
            path: path.to_string(),
            span: span.to_string(),
            signature: None,
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

    #[test]
    fn card_path_for_swaps_the_final_extension() {
        let no_slugs = HashSet::new();
        assert_eq!(card_path_for("src/a.b.ts", &no_slugs), "src/a.b.md");
        assert_eq!(card_path_for("Makefile", &no_slugs), "Makefile.md");
    }

    /// P2-31: a nested source never hits the `_root/` branch, even when its
    /// stem happens to equal a live concept slug (note section 2.2, rule 2).
    #[test]
    fn card_path_for_p2_31_a_nested_file_never_collides() {
        let mut slugs = HashSet::new();
        slugs.insert("util".to_string());
        assert_eq!(card_path_for("src/util.ts", &slugs), "src/util.md");
    }

    /// P2-31: a root-level source whose stem equals a live concept slug
    /// routes to `_root/` instead of colliding with the concept node file.
    #[test]
    fn card_path_for_p2_31_a_root_level_collision_moves_under_root() {
        let mut slugs = HashSet::new();
        slugs.insert("util".to_string());
        assert_eq!(card_path_for("util.ts", &slugs), "_root/util.md");
    }

    /// P2-31: a root-level source whose stem matches no concept slug never
    /// fires the collision branch.
    #[test]
    fn card_path_for_p2_31_a_root_level_file_with_no_collision_stays_top_level() {
        let no_slugs = HashSet::new();
        assert_eq!(card_path_for("plain-root.ts", &no_slugs), "plain-root.md");
    }

    #[test]
    fn render_card_sorts_same_start_line_bullets_by_collated_name() {
        let b = node("f.ts#b", "b", Kind::Function, "f.ts", "L1-L2");
        let a = node("f.ts#a", "a", Kind::Function, "f.ts", "L1-L2");
        let text = render_card("f.ts", None, &[&b, &a], &[]);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[2].starts_with("- a"));
        assert!(lines[3].starts_with("- b"));
    }

    #[test]
    fn render_card_with_no_symbols_writes_the_empty_line() {
        let text = render_card("f.ts", None, &[], &[]);
        assert_eq!(text, "# f.ts\n\n_No extracted symbols in this file._\n");
    }

    /// The up-link list joins in byte order, not ICU.
    /// `[[slug]]` heading exactly (note section 2.1).
    #[test]
    fn render_card_joins_uplinks_in_byte_order_on_the_heading() {
        let uplinks = vec!["Zeta".to_string(), "alpha".to_string()];
        let text = render_card("f.ts", None, &[], &uplinks);
        // Byte order: 'Z' (0x5A) sorts before 'a' (0x61), so the caller's
        // own pre-sorted order is trusted verbatim, with no re-sort here.
        assert!(text.starts_with("# f.ts \u{b7} [[Zeta]] [[alpha]]\n"));
    }

    #[test]
    fn render_card_with_no_uplinks_leaves_a_bare_heading() {
        let text = render_card("f.ts", None, &[], &[]);
        assert!(text.starts_with("# f.ts\n"));
    }

    fn sample_graph() -> Graph {
        Graph {
            meta: Meta {
                version: 1,
                node_count: 0,
                edge_count: 0,
                languages: vec![],
                scopes: vec![],
            },
            nodes: vec![
                node("a.ts", "a.ts", Kind::File, "a.ts", "L1-L1"),
                node("a.ts#run", "run", Kind::Function, "a.ts", "L1-L2"),
                node("b.ts", "b.ts", Kind::File, "b.ts", "L1-L1"),
            ],
            edges: vec![Edge {
                source: "a.ts#run".to_string(),
                target: "a.ts#run".to_string(),
                relation: Relation::Contains,
                confidence: Confidence::Extracted,
            }],
        }
    }

    #[test]
    fn write_index_matches_the_golden_text_with_the_numbers_substituted() {
        let dir = unique_temp_dir("index");
        let stats = CardStats {
            written: 7,
            files: 7,
            with_symbols: 7,
        };

        write_index(&dir, &stats).expect("write index succeeds");
        let text = fs::read_to_string(dir.join("INDEX.md")).expect("read index");

        let golden = fs::read_to_string(
            crate::test_support::manifest_dir()
                .join("../../tests/fixtures/basic.expected/sieve/INDEX.md"),
        )
        .expect("read golden index");
        assert_eq!(text, golden);
    }

    #[test]
    fn write_cards_lets_the_later_node_path_win_a_card_path_collision() {
        let dir = unique_temp_dir("collision");

        // The node list holds a.py before a.ts, and `write_cards` keeps that
        // order. Both map to "a.md": the later write, a.ts, wins on disk.
        let graph = Graph {
            meta: Meta {
                version: 1,
                node_count: 0,
                edge_count: 0,
                languages: vec![],
                scopes: vec![],
            },
            nodes: vec![
                node("a.py", "a.py", Kind::File, "a.py", "L1-L1"),
                node("a.ts", "a.ts", Kind::File, "a.ts", "L1-L1"),
            ],
            edges: vec![],
        };

        let stats = write_cards(&graph, &dir).expect("write cards succeeds");
        assert_eq!(stats.written, 1);

        let text = fs::read_to_string(dir.join("a.md")).expect("read a.md");
        assert!(text.starts_with("# a.ts"));
    }

    /// P2-31 (DV10): cards are written in `graph.nodes` order, not
    /// id order. With `M.ts` before `m.js` in the list,
    /// the later write, `m.js`, wins the shared card path. The result is
    /// `# m.js` on a repo with `m.js` and `M.ts`.
    #[test]
    fn test_p2_31_dv10_card_collision_follows_node_order_not_id_order() {
        let dir = unique_temp_dir("p2-31-dv10");
        let graph = Graph {
            meta: Meta {
                version: 1,
                node_count: 0,
                edge_count: 0,
                languages: vec![],
                scopes: vec![],
            },
            nodes: vec![
                node("M.ts", "M.ts", Kind::File, "M.ts", "L1-L1"),
                node("m.js", "m.js", Kind::File, "m.js", "L1-L1"),
            ],
            edges: vec![],
        };

        write_cards(&graph, &dir).expect("write cards succeeds");

        let text = fs::read_to_string(dir.join("M.md")).expect("read the card");
        assert!(text.starts_with("# m.js"), "got: {text}");
    }

    /// Sieve prints the count of source files in `INDEX.md`, not the count of
    /// card paths. Expected line: a build of `a.py`, `a.ts`, `b.ts`, where the
    /// first two share `a.md`.
    #[test]
    fn test_p2_29_dv8_index_card_count_matches_golden() {
        let dir = unique_temp_dir("p2-29-dv8");
        let graph = Graph {
            meta: Meta {
                version: 1,
                node_count: 0,
                edge_count: 0,
                languages: vec![],
                scopes: vec![],
            },
            nodes: vec![
                node("a.py", "a.py", Kind::File, "a.py", "L1-L2"),
                node("a.py:a", "a", Kind::Function, "a.py", "L1-L2"),
                node("a.ts", "a.ts", Kind::File, "a.ts", "L1-L3"),
                node("a.ts:a", "a", Kind::Function, "a.ts", "L1-L3"),
                node("b.ts", "b.ts", Kind::File, "b.ts", "L1-L1"),
            ],
            edges: vec![],
        };

        let stats = write_cards(&graph, &dir).expect("write cards succeeds");
        write_index(&dir, &stats).expect("write index succeeds");
        let text = fs::read_to_string(dir.join("INDEX.md")).expect("read index");

        assert_eq!(stats.written, 2);
        assert!(text.contains("\n3 per-file wiring cards mirror the source tree under `sieve/` (2 carry extracted symbols)."));
    }

    /// With no card and no concept, the `INDEX.md` ends in one newline
    /// after the preamble. The golden preamble ends in
    /// a blank line before `## Files`; the empty form drops that blank line.
    #[test]
    fn test_p2_29_dv9_empty_index_ends_in_one_newline() {
        let dir = unique_temp_dir("p2-29-dv9");
        let stats = CardStats {
            written: 0,
            files: 0,
            with_symbols: 0,
        };
        write_index(&dir, &stats).expect("write index succeeds");
        let text = fs::read_to_string(dir.join("INDEX.md")).expect("read index");

        let golden = fs::read_to_string(
            crate::test_support::manifest_dir()
                .join("../../tests/fixtures/basic.expected/sieve/INDEX.md"),
        )
        .expect("read golden index");
        let preamble = golden.split("## Files").next().expect("golden preamble");
        assert_eq!(text, preamble.strip_suffix('\n').expect("blank line"));
    }

    #[test]
    fn write_cards_prunes_a_stale_nested_card_and_keeps_a_top_level_one() {
        let dir = unique_temp_dir("prune");
        let nested_dir = dir.join("src");
        fs::create_dir_all(&nested_dir).expect("create nested dir");
        fs::write(nested_dir.join("stale.md"), "stale").expect("write stale card");
        fs::write(dir.join("top.md"), "top").expect("write top-level card");

        let graph = sample_graph();
        write_cards(&graph, &dir).expect("write cards succeeds");

        assert!(!nested_dir.join("stale.md").exists());
        assert!(dir.join("top.md").exists());
        assert!(dir.join("a.md").exists());
        assert!(dir.join("b.md").exists());
    }

    /// P2-31, end to end: a root-level source whose stem equals a live
    /// concept slug on disk lands under `_root/`, not at the top level.
    #[test]
    fn write_cards_p2_31_moves_a_colliding_root_file_under_root() {
        let dir = unique_temp_dir("p2-31");
        fs::write(
            dir.join("util.md"),
            "---\nname: Util\nslug: util\ntype: concept\n---\nbody\n",
        )
        .expect("write concept node");

        let graph = Graph {
            meta: Meta {
                version: 1,
                node_count: 0,
                edge_count: 0,
                languages: vec![],
                scopes: vec![],
            },
            nodes: vec![node("util.ts", "util.ts", Kind::File, "util.ts", "L1-L1")],
            edges: vec![],
        };

        write_cards(&graph, &dir).expect("write cards succeeds");

        // The wiring card lands under `_root/`, and the concept node's own
        // top-level `util.md` survives untouched — no overwrite, no
        // collision.
        assert!(dir.join("_root/util.md").exists());
        let root_card = fs::read_to_string(dir.join("_root/util.md")).expect("read root card");
        assert!(root_card.starts_with("# util.ts"));
        let concept_file = fs::read_to_string(dir.join("util.md")).expect("read concept file");
        assert!(concept_file.starts_with("---\nname: Util\n"));
    }

    /// The up-link line for a file two concepts cite lists both slugs in
    /// byte order, not ICU (note section 2.1).
    #[test]
    fn write_cards_lists_two_concepts_citing_one_file_in_byte_order() {
        let dir = unique_temp_dir("uplinks");
        fs::write(
            dir.join("Zeta.md"),
            "---\nname: Zeta\nslug: Zeta\nsources:\n  - path: app.ts\n    hash: a\n---\nbody\n",
        )
        .expect("write concept node");
        fs::write(
            dir.join("alpha.md"),
            "---\nname: alpha\nslug: alpha\nsources:\n  - path: app.ts\n    hash: a\n---\nbody\n",
        )
        .expect("write concept node");

        let graph = Graph {
            meta: Meta {
                version: 1,
                node_count: 0,
                edge_count: 0,
                languages: vec![],
                scopes: vec![],
            },
            nodes: vec![node("app.ts", "app.ts", Kind::File, "app.ts", "L1-L1")],
            edges: vec![],
        };

        write_cards(&graph, &dir).expect("write cards succeeds");
        let text = fs::read_to_string(dir.join("app.md")).expect("read app.md");
        // Byte order: 'Z' (0x5A) sorts before 'a' (0x61).
        assert!(text.starts_with("# app.ts \u{b7} [[Zeta]] [[alpha]]\n"));
    }

    /// P2-37: `INDEX.md`'s `## Concepts` roster sorts by slug with ICU,
    /// not byte order.
    #[test]
    fn write_index_p2_37_sorts_the_concepts_roster_with_icu() {
        let dir = unique_temp_dir("p2-37");
        fs::write(
            dir.join("Bravo.md"),
            "---\nname: Bravo\nslug: Bravo\nsources:\n  - path: b.ts\n    hash: a\n---\nbody\n",
        )
        .expect("write concept node");
        fs::write(
            dir.join("alpha.md"),
            "---\nname: Alpha\nslug: alpha\n---\nbody\n",
        )
        .expect("write concept node");

        let stats = CardStats {
            written: 0,
            files: 0,
            with_symbols: 0,
        };
        write_index(&dir, &stats).expect("write index succeeds");
        let text = fs::read_to_string(dir.join("INDEX.md")).expect("read index");

        let concepts_line = text
            .lines()
            .position(|line| line == "## Concepts")
            .expect("concepts heading present");
        let lines: Vec<&str> = text.lines().collect();
        // ICU root collation puts "alpha" before "Bravo"; byte order would
        // reverse them.
        assert_eq!(lines[concepts_line + 2], "- [alpha](alpha.md) — Alpha");
        assert_eq!(
            lines[concepts_line + 3],
            "- [Bravo](Bravo.md) — Bravo \u{b7} b.ts"
        );
    }

    /// With zero concept nodes on disk, `INDEX.md` never gets a
    /// `## Concepts` section — the existing golden stays exact (section
    /// 2.3's spec correction and P2-37's "only when at least one concept
    /// exists" rule).
    #[test]
    fn write_index_with_no_concepts_omits_the_concepts_section() {
        let dir = unique_temp_dir("no-concepts");
        let stats = CardStats {
            written: 0,
            files: 0,
            with_symbols: 0,
        };
        write_index(&dir, &stats).expect("write index succeeds");
        let text = fs::read_to_string(dir.join("INDEX.md")).expect("read index");
        assert!(!text.contains("## Concepts"));
    }

    /// The one-liner never reads `summary_state`: a `stale` summary still
    /// prints its first line, exactly like a `ready` one (note section 2.3's
    /// spec correction).
    #[test]
    fn one_liner_prints_a_stale_summary_like_a_ready_one() {
        let mut file_node = node("f.ts", "f.ts", Kind::File, "f.ts", "L1-L1");
        file_node.summary = Some("first line\nsecond line".to_string());
        file_node.summary_state = SummaryState::Stale;
        let text = render_card("f.ts", Some(&file_node), &[], &[]);
        assert!(text.starts_with("# f.ts\n\nfirst line\n\n"));
    }

    /// F3: `oneLiner` trims the split line a second time
    /// `s.split("\n")[0].trim()`), so trailing spaces before the newline on
    /// the first line never survive into the card.
    #[test]
    fn one_liner_trims_the_first_line_a_second_time() {
        let mut file_node = node("f.ts", "f.ts", Kind::File, "f.ts", "L1-L1");
        file_node.summary = Some("first line   \nsecond line".to_string());
        let text = render_card("f.ts", Some(&file_node), &[], &[]);
        assert!(text.starts_with("# f.ts\n\nfirst line\n\n"));
    }
}
