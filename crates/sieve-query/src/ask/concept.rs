//! The concept corpus: `sieve/<slug>.md` read live at query time, scored, and
//! merged into `ask`'s hit list (the `deep-tier.md` note section 2.8).
//!
//! `loadCorpus` takes every top-level `.md` in the context dir except
//! `INDEX.md`, with **no** slug test — unlike `sieve_core::concept::
//! read_nodes`, which skips a root-level wiring card. Sieve reproduces
//! that quirk: a bare wiring card with no `slug` still becomes a phantom
//! concept here.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use sieve_core::askindex::{bag, tokenize};
use sieve_core::concept::ConceptNode;

use super::lexical::{matched_idf_share, score, Corpus};
use super::AskHit;

/// One concept document, read fresh on every `ask` call: Sieve never
/// caches this corpus.
#[derive(Debug, Clone)]
pub(crate) struct ConceptDoc {
    pub slug: String,
    pub name: String,
    /// `sources[].path`, frontmatter order.
    pub sources: Vec<String>,
    /// `links[].to`, frontmatter order.
    pub related: Vec<String>,
    pub snippet: String,
    /// `"${name} ${snippet} ${sources.join(' ')}"`: the concept's body
    /// bag before tokenizing.
    pub text: String,
}

/// The first trimmed body line that is not empty and does not open with
/// `#`, `<!--` or `-`. On a concept node this is the
/// first line of the LLM summary.
pub(crate) fn first_prose(body: &str) -> String {
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with('#')
            || trimmed.starts_with("<!--")
            || trimmed.starts_with('-')
        {
            continue;
        }
        return trimmed.to_string();
    }
    String::new()
}

/// Reads every top-level concept document under `context_dir`, `INDEX.md`
/// excluded, with no skip test (`loadCorpus`). An unusable
/// or missing directory gives an empty corpus, the same as a Tier-1 graph.
pub(crate) fn load_corpus(context_dir: &Path) -> Vec<ConceptDoc> {
    let Ok(entries) = fs::read_dir(context_dir) else {
        return Vec::new();
    };
    // Node's `readdirSync` lists a directory sorted by name;
    // `read_dir` order is the file system's own.
    let mut entries: Vec<fs::DirEntry> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    let mut out = Vec::new();
    for entry in entries {
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        if !name.ends_with(".md") || name == "INDEX.md" {
            continue;
        }
        let Ok(text) = fs::read_to_string(entry.path()) else {
            continue;
        };
        let stem = name.trim_end_matches(".md").to_string();
        let node = ConceptNode::parse(&stem, &text);
        let snippet = first_prose(&node.body);
        let sources: Vec<String> = node.sources.into_iter().map(|s| s.path).collect();
        let related: Vec<String> = node.links.into_iter().map(|l| l.to).collect();
        let text = format!("{} {} {}", node.name, snippet, sources.join(" "));
        out.push(ConceptDoc {
            slug: node.slug,
            name: node.name,
            sources,
            related,
            snippet,
            text,
        });
    }
    out
}

/// The token set one concept folds into the idf table as its own
/// document: the union of its name and text tokens, once each.
pub(crate) fn idf_fold_terms(doc: &ConceptDoc) -> HashSet<String> {
    tokenize(&doc.name)
        .into_iter()
        .chain(tokenize(&doc.text))
        .collect()
}

/// One concept's raw score, before max-normalization.
pub(crate) struct ConceptScore<'a> {
    pub doc: &'a ConceptDoc,
    pub raw: f64,
}

/// Scores every concept doc: `total = score(name)*3 + score(body)`, kept
/// only when positive. No BM25, no path field, no test factor.
pub(crate) fn score_concepts<'a>(
    docs: &'a [ConceptDoc],
    terms: &[String],
    corpus: &Corpus,
) -> Vec<ConceptScore<'a>> {
    docs.iter()
        .filter_map(|doc| {
            let name_bag = bag(&tokenize(&doc.name));
            let body_bag = bag(&tokenize(&doc.text));
            let raw = score(terms, &name_bag, corpus) * 3.0 + score(terms, &body_bag, corpus);
            (raw > 0.0).then_some(ConceptScore { doc, raw })
        })
        .collect()
}

/// The `coverage` and `coverageStrong` pair a concept top hit reports
///: the matched idf share over the name and body bags,
/// then over the name bag alone. A concept has no path field.
pub(crate) fn coverage_of(doc: &ConceptDoc, terms: &[String], corpus: &Corpus) -> (f64, f64) {
    let name_bag = bag(&tokenize(&doc.name));
    let body_bag = bag(&tokenize(&doc.text));
    (
        matched_idf_share(terms, &[&name_bag, &body_bag], corpus),
        matched_idf_share(terms, &[&name_bag], corpus),
    )
}

/// Divides every concept score by the best one, so the top concept scores
/// exactly `1`. An empty or all-zero input gives no
/// concepts.
pub(crate) fn normalize<'a>(scores: &[ConceptScore<'a>]) -> Vec<(&'a ConceptDoc, f64)> {
    let max = scores.iter().map(|s| s.raw).fold(0.0_f64, f64::max);
    if max <= 0.0 {
        return Vec::new();
    }
    scores.iter().map(|s| (s.doc, s.raw / max)).collect()
}

/// The title Sieve renders for a concept: `name`, or `slug` when `name` is
/// empty (the phantom-concept case in note section 2.8).
pub(crate) fn title_of(doc: &ConceptDoc) -> String {
    if doc.name.is_empty() {
        doc.slug.clone()
    } else {
        doc.name.clone()
    }
}

/// Renders one concept as an [`AskHit`]: `pointer` is the comma-joined
/// source list, or the slug when there are no sources.
pub(crate) fn to_hit(doc: &ConceptDoc, score: f64) -> AskHit {
    let pointer = if doc.sources.is_empty() {
        doc.slug.clone()
    } else {
        doc.sources.join(", ")
    };
    AskHit {
        id: format!("concept:{}", doc.slug),
        kind: "concept".to_string(),
        title: title_of(doc),
        pointer,
        snippet: doc.snippet.clone(),
        related: doc.related.clone(),
        relation: None,
        score,
        scope: None,
        code: None,
        scope_last: false,
        path: String::new(),
        span: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_temp_dir(label: &str) -> crate::test_support::TempDir {
        crate::test_support::TempDir::new(label)
    }

    /// DV7: a concept's coverage reads the name and body bags; the strong
    /// share reads the name bag alone.
    #[test]
    fn test_p1_06_dv7_coverage_of_reads_name_and_body_bags() {
        let corpus = Corpus {
            idf: [("notes".to_string(), 2.0), ("series".to_string(), 1.0)]
                .into_iter()
                .collect(),
            dflt_idf: 1.0,
            avg_body_len: 0.0,
        };
        let doc = ConceptDoc {
            slug: "widget-notes".to_string(),
            name: "Widget Notes".to_string(),
            sources: Vec::new(),
            related: Vec::new(),
            snippet: String::new(),
            text: "Widget Notes a fixed series".to_string(),
        };
        let terms = vec!["notes".to_string(), "series".to_string(), "zzz".to_string()];
        let (broad, strong) = coverage_of(&doc, &terms, &corpus);
        // By hand: total weight = 2.0 + 1.0 + 1.0 (dflt) = 4.0.
        // Broad: "notes" (name) and "series" (body) -> 3.0 / 4.0.
        // Strong: "notes" only -> 2.0 / 4.0.
        assert!((broad - 0.75).abs() < 1e-12, "coverage: {broad}");
        assert!((strong - 0.5).abs() < 1e-12, "coverageStrong: {strong}");
    }

    /// P3-01: `firstProse` skips a blank line, a heading, an HTML
    /// comment and a Markdown list item, and returns the first real prose
    /// line.
    #[test]
    fn test_p3_01_first_prose_skips_empty_heading_comment_and_list_lines() {
        let body = concat!(
            "\n",
            "## Summary\n",
            "\n",
            "<!-- context:generated:start -->\n",
            "- not a bullet\n",
            "  The retry helper backs off before it calls again.\n",
            "More prose never reached.\n",
        );
        // The indented line still starts with a space, not `-`, so it is
        // the first real prose line once trimmed.
        assert_eq!(
            first_prose(body),
            "The retry helper backs off before it calls again."
        );
    }

    #[test]
    fn test_p3_01_first_prose_gives_an_empty_string_with_no_prose_line() {
        assert_eq!(first_prose("# heading\n<!-- c -->\n- item\n"), "");
    }

    /// P3-02: the concept score is `name*3 + body`, with no BM25 term.
    #[test]
    fn test_p3_02_concept_score_is_name_times_three_plus_body() {
        let corpus = Corpus::build_with_concepts(&[], &std::collections::HashMap::new(), &[]);
        let doc = ConceptDoc {
            slug: "widget".to_string(),
            name: "widget".to_string(),
            sources: Vec::new(),
            related: Vec::new(),
            snippet: "widget helper".to_string(),
            text: "widget widget helper".to_string(),
        };
        let terms = vec!["widget".to_string()];
        let scored = score_concepts(std::slice::from_ref(&doc), &terms, &corpus);
        assert_eq!(scored.len(), 1);
        // idf(widget) = 1.0 (unseen by an empty corpus). name bag holds
        // one "widget" -> score(name) = 1.0, times 3. body bag holds two
        // "widget" -> score(body) = 2.0. Total = 3.0 + 2.0 = 5.0.
        assert!((scored[0].raw - 5.0).abs() < 1e-12);
    }

    /// P3-03: a concept's own term set folds into the shared idf table as
    /// one more document, changing a symbol hit's idf.
    #[test]
    fn test_p3_03_idf_fold_changes_a_symbol_hits_idf() {
        use super::super::lexical::DocBag;
        use sieve_core::{Kind, Node, Origin, SummaryState};

        let node = Node {
            id: "a.ts#run".to_string(),
            name: "run".to_string(),
            kind: Kind::Function,
            owner: None,
            path: "a.ts".to_string(),
            span: "L1-L2".to_string(),
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
        };
        let active_nodes = vec![&node];
        let mut docs = std::collections::HashMap::new();
        docs.insert(
            "a.ts#run",
            DocBag {
                name: bag(&tokenize("run")),
                path: bag(&tokenize("a.ts")),
                body: bag(&tokenize("")),
                body_len: 0,
            },
        );

        let without_concepts = Corpus::build_with_concepts(&active_nodes, &docs, &[]);
        let doc = ConceptDoc {
            slug: "run-concept".to_string(),
            name: "run".to_string(),
            sources: Vec::new(),
            related: Vec::new(),
            snippet: String::new(),
            text: "run".to_string(),
        };
        let with_concepts =
            Corpus::build_with_concepts(&active_nodes, &docs, &[idf_fold_terms(&doc)]);

        assert!(without_concepts.idf("run") != with_concepts.idf("run"));
    }

    /// P3-04: every concept score normalizes so the best concept is
    /// exactly `1`.
    #[test]
    fn test_p3_04_concept_scores_normalize_to_a_max_of_one() {
        let a = ConceptDoc {
            slug: "a".to_string(),
            name: String::new(),
            sources: Vec::new(),
            related: Vec::new(),
            snippet: String::new(),
            text: String::new(),
        };
        let b = ConceptDoc {
            slug: "b".to_string(),
            name: String::new(),
            sources: Vec::new(),
            related: Vec::new(),
            snippet: String::new(),
            text: String::new(),
        };
        let scores = vec![
            ConceptScore { doc: &a, raw: 4.0 },
            ConceptScore { doc: &b, raw: 1.0 },
        ];
        let normalized = normalize(&scores);
        assert_eq!(normalized[0].1, 1.0);
        assert_eq!(normalized[1].1, 0.25);
    }

    /// P3-05 (render): `related:` prints in frontmatter order, not
    /// sorted.
    #[test]
    fn test_p3_05_to_hit_carries_related_in_frontmatter_order() {
        let doc = ConceptDoc {
            slug: "widget".to_string(),
            name: "Widget".to_string(),
            sources: vec!["src/a.ts".to_string(), "src/b.ts".to_string()],
            related: vec!["zeta".to_string(), "alpha".to_string()],
            snippet: "the widget summary".to_string(),
            text: String::new(),
        };
        let hit = to_hit(&doc, 1.0);
        assert_eq!(hit.kind, "concept");
        assert_eq!(hit.title, "Widget");
        assert_eq!(hit.pointer, "src/a.ts, src/b.ts");
        assert_eq!(hit.related, vec!["zeta".to_string(), "alpha".to_string()]);
    }

    /// A concept with no sources falls back to its slug as its pointer,
    /// and to its slug as its title when its name is empty — the phantom
    /// concept case (note section 2.8).
    #[test]
    fn test_p3_05_a_phantom_concept_falls_back_to_its_slug() {
        let doc = ConceptDoc {
            slug: "plain-root".to_string(),
            name: String::new(),
            sources: Vec::new(),
            related: Vec::new(),
            snippet: String::new(),
            text: String::new(),
        };
        let hit = to_hit(&doc, 1.0);
        assert_eq!(hit.title, "plain-root");
        assert_eq!(hit.pointer, "plain-root");
    }

    #[test]
    fn load_corpus_skips_index_md_and_reads_every_other_top_level_md() {
        let dir = unique_temp_dir("load");
        fs::write(dir.join("INDEX.md"), "---\n---\nignored\n").expect("write");
        fs::write(
            dir.join("widget.md"),
            "---\nname: Widget\nslug: widget\n---\nThe widget summary line.\n",
        )
        .expect("write");

        let docs = load_corpus(&dir);
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].slug, "widget");
        assert_eq!(docs[0].snippet, "The widget summary line.");
    }

    /// P3-09: Node lists the context dir sorted by name, so
    /// the corpus order, and with it a concept score tie, follows the file
    /// name and not the file system's own order.
    #[test]
    fn test_p3_09_load_corpus_reads_the_dir_sorted_by_name() {
        let dir = unique_temp_dir("sorted");
        let names = ["c7", "c2", "c9", "c0", "c5", "c1", "c8", "c3", "c6", "c4"];
        for n in names {
            fs::write(
                dir.join(format!("{n}-list-page.md")),
                format!("---\nname: List Page\nslug: {n}-list-page\n---\nBody.\n"),
            )
            .expect("write");
        }

        let slugs: Vec<String> = load_corpus(&dir).into_iter().map(|d| d.slug).collect();
        let mut want = slugs.clone();
        want.sort();
        assert_eq!(slugs, want);
        assert_eq!(slugs.first().map(String::as_str), Some("c0-list-page"));
    }

    /// P3-07: the concept hit always carries `related`,
    /// an empty array when the concept has no links. A symbol hit never
    /// carries the key.
    #[test]
    fn test_p3_07_a_concept_hit_json_keeps_an_empty_related_array() {
        let doc = ConceptDoc {
            slug: "widget".to_string(),
            name: "Widget".to_string(),
            sources: Vec::new(),
            related: Vec::new(),
            snippet: String::new(),
            text: String::new(),
        };
        let json = serde_json::to_string(&to_hit(&doc, 1.0)).expect("serialize");
        assert_eq!(
            json,
            r#"{"kind":"concept","title":"Widget","pointer":"widget","snippet":"","related":[],"score":1.0}"#
        );
    }

    #[test]
    fn load_corpus_keeps_a_root_level_wiring_card_as_a_phantom_concept() {
        // `read_nodes` would skip this file (a wiring-card heading, no
        // slug). `loadCorpus` runs no skip test at all.
        let dir = unique_temp_dir("phantom");
        fs::write(
            dir.join("util.md"),
            "---\n{}\n---\n# util.ts\n\nA one-line summary of util.ts.\n",
        )
        .expect("write");

        let docs = load_corpus(&dir);
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].slug, "util");
        assert_eq!(docs[0].name, "");
        assert!(docs[0].sources.is_empty());
    }
}
