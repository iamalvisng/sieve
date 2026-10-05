//! The deep-layer reader: a concept node's frontmatter (`sieve/<slug>.md`)
//! and `sieve/manifest.json` (the `deep-tier.md` note
//! sections 1.2, 1.3, 2.6 and 4.4). Sieve never writes a concept node body,
//! a summary, or the manifest — this module only reads what a
//! `--deep` pass already wrote.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::collate::collate;

/// One entry of a concept node's `sources:` list, or one entry of
/// `manifest.json`'s `files` list. Both carry the same two keys.
#[derive(Debug, Clone, Deserialize)]
pub struct SourceRef {
    pub path: String,
    /// The content hash at generation time. `None` stands for an absent key:
    /// the readers take the frontmatter as is, so a hand-edited entry with no
    /// `hash` still parses.
    #[serde(default)]
    pub hash: Option<String>,
}

/// One entry of a concept node's `links:` list: an edge to another concept.
#[derive(Debug, Clone, Deserialize)]
pub struct NodeLink {
    pub to: String,
    /// A link with no `relation` still parses: it reads as unset.
    #[serde(default)]
    pub relation: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// One entry of a concept node's `covers:` list: a symbol the concept
/// claims, and where it lives.
#[derive(Debug, Clone, Deserialize)]
pub struct CoverRef {
    pub symbol: String,
    pub kind: String,
    pub at: String,
}

/// A parsed `sieve/<slug>.md`: the frontmatter fields Sieve reads, plus the
/// body verbatim. Sieve never rewrites the body; only `covers.rs` (a later
/// row) splices a new `covers:` block into the frontmatter text.
#[derive(Debug, Clone)]
pub struct ConceptNode {
    pub slug: String,
    pub name: String,
    pub kind_type: String,
    pub sources: Vec<SourceRef>,
    pub sources_digest: String,
    pub links: Vec<NodeLink>,
    pub covers: Vec<CoverRef>,
    /// Every byte after the closing frontmatter fence, verbatim.
    pub body: String,
}

/// The frontmatter fields Sieve reads out of a concept node, before the
/// filename-stem fallback for `slug` applies. Deserialized straight off
/// the YAML block; every field is optional because Sieve must tolerate a
/// hand-edited or partial node file.
#[derive(Debug, Clone, Default, Deserialize)]
struct RawFrontmatter {
    name: Option<String>,
    slug: Option<String>,
    #[serde(rename = "type", default)]
    kind_type: Option<String>,
    #[serde(default)]
    sources: Vec<SourceRef>,
    #[serde(default, rename = "sources_digest")]
    sources_digest: String,
    #[serde(default)]
    links: Vec<NodeLink>,
    #[serde(default)]
    covers: Vec<CoverRef>,
}

/// `sieve/manifest.json`'s roster entry for one concept node.
#[derive(Debug, Clone, Deserialize)]
pub struct ManifestNode {
    pub slug: String,
    pub name: String,
    #[serde(rename = "type")]
    pub node_type: String,
    pub sources: Vec<String>,
    #[serde(rename = "sourcesDigest")]
    pub sources_digest: String,
}

/// `sieve/manifest.json`. Sieve only reads this file; the `--deep` pass
/// is the only writer.
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub model: String,
    #[serde(rename = "repoDigest")]
    pub repo_digest: String,
    pub files: Vec<SourceRef>,
    pub nodes: Vec<ManifestNode>,
}

/// Splits a gray-matter document into its YAML frontmatter and its body.
///
/// Returns `None` when the text does not open with a `---` fence, so a
/// caller can fall back to treating the whole text as body with no
/// frontmatter, the same tolerance gray-matter itself gives.
pub fn split_frontmatter(text: &str) -> Option<(&str, &str)> {
    const CLOSE: &str = "\n---\n";
    let rest = text.strip_prefix("---\n")?;
    let idx = rest.find(CLOSE)?;
    let yaml = &rest[..idx];
    let body = &rest[idx + CLOSE.len()..];
    Some((yaml, body))
}

impl ConceptNode {
    /// Parses one concept node file. `slug_from_filename` is the `.md`
    /// stem, used only when the frontmatter carries no `slug` key (an
    /// absent key, not an empty string — a `??` fallback).
    ///
    /// Malformed or missing frontmatter parses as every field empty, the
    /// same tolerance `readNodes` gives a hand-dropped file.
    pub fn parse(slug_from_filename: &str, text: &str) -> ConceptNode {
        let (yaml, body) = split_frontmatter(text).unwrap_or(("", text));
        let raw: RawFrontmatter = serde_yaml_ng::from_str(yaml).unwrap_or_default();
        ConceptNode {
            slug: raw.slug.unwrap_or_else(|| slug_from_filename.to_string()),
            name: raw.name.unwrap_or_default(),
            kind_type: raw.kind_type.unwrap_or_default(),
            sources: raw.sources,
            sources_digest: raw.sources_digest,
            links: raw.links,
            covers: raw.covers,
            body: body.to_string(),
        }
    }
}

impl Manifest {
    /// Reads `<context_dir>/manifest.json`. Returns `None` when the file
    /// is absent or fails to parse — the same "no deep layer yet" signal
    /// `check`'s context layer uses.
    pub fn read(context_dir: &Path) -> Option<Manifest> {
        let text = fs::read_to_string(context_dir.join("manifest.json")).ok()?;
        serde_json::from_str(&text).ok()
    }
}

// ponytail: `digestSources`'s sha256-over-sorted-`path:hash`-lines helper
// (note section 1.2) is not built here. No row up to
// this one needs to verify a digest; add it beside the row that does
// (`check.rs`, row 5) rather than carry dead code now.

/// Stems of top-level per-file wiring cards recorded in the manifest, i.e.
/// root-level source files the wiring build also cards.
fn root_file_card_stems(manifest: Option<&Manifest>) -> HashSet<String> {
    let Some(manifest) = manifest else {
        return HashSet::new();
    };
    manifest
        .files
        .iter()
        .filter(|f| !f.path.contains('/'))
        .map(|f| match f.path.rfind('.') {
            Some(dot) => f.path[..dot].to_string(),
            None => f.path.clone(),
        })
        .collect()
}

/// The first skip test: a non-null `slug` in frontmatter whose string form is
/// non-empty always keeps the file `fm.slug != null && String(fm.slug).length >
/// 0`). A number or a bool still counts, because its string form is never
/// empty; only `null` or `""` fail this test.
fn has_concept_slug(fm: &serde_yaml_ng::Value) -> bool {
    match fm.get("slug") {
        None | Some(serde_yaml_ng::Value::Null) => false,
        Some(serde_yaml_ng::Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// The third skip test: a `covers`-only frontmatter, with no `name`,
/// `type` or `sources` key.
fn is_covers_only_file_card(fm: &serde_yaml_ng::Value) -> bool {
    let Some(map) = fm.as_mapping() else {
        return false;
    };
    if !map.contains_key("covers") {
        return false;
    }
    let absent_or_null = |key: &str| match map.get(key) {
        None => true,
        Some(v) => v.is_null(),
    };
    absent_or_null("name") && absent_or_null("type") && absent_or_null("sources")
}

/// The fourth skip test: the body's first line looks like a wiring
/// card heading, `# <path>.<ext>` with a 1-12 character extension.
fn is_wiring_card_heading(content: &str) -> bool {
    let first_line = content.trim_start().lines().next().unwrap_or("");
    let Some(rest) = first_line.strip_prefix("# ") else {
        return false;
    };
    let token_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let token = &rest[..token_end];
    let Some(dot) = token.rfind('.') else {
        return false;
    };
    if dot == 0 || token[..dot].contains('#') {
        return false;
    }
    let ext = &token[dot + 1..];
    (1..=12).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric())
}

/// A top-level `.md` that is a per-file wiring card, not a concept node.
fn is_root_file_card(
    stem: &str,
    yaml: &str,
    content: &str,
    file_card_stems: &HashSet<String>,
) -> bool {
    let fm: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(yaml).unwrap_or(serde_yaml_ng::Value::Null);
    if has_concept_slug(&fm) {
        return false;
    }
    if file_card_stems.contains(stem) {
        return true;
    }
    if is_covers_only_file_card(&fm) {
        return true;
    }
    is_wiring_card_heading(content)
}

/// Reads and parses every concept-node `.md` in a context dir, skipping
/// `INDEX.md` and every root-level per-file card, sorted by slug with ICU
/// collation (note section 2.6).
pub fn read_nodes(context_dir: &Path) -> Vec<ConceptNode> {
    let manifest = Manifest::read(context_dir);
    let file_card_stems = root_file_card_stems(manifest.as_ref());

    let entries = match fs::read_dir(context_dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };

    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.ends_with(".md") || name == "INDEX.md" {
            continue;
        }
        let Ok(text) = fs::read_to_string(entry.path()) else {
            continue;
        };
        let stem = name.trim_end_matches(".md").to_string();
        let (yaml, body) = split_frontmatter(&text).unwrap_or(("", text.as_str()));
        if is_root_file_card(&stem, yaml, body, &file_card_stems) {
            continue;
        }
        out.push(ConceptNode::parse(&stem, &text));
    }
    out.sort_by(|a, b| collate(&a.slug, &b.slug));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp dir unique per call, that removes itself on drop, even if
    /// the test panics before it reaches its own cleanup line.
    fn unique_temp_dir(label: &str) -> crate::test_support::TempDir {
        crate::test_support::TempDir::new(label)
    }

    #[test]
    fn parse_joins_a_folded_plain_scalar_description() {
        // js-yaml's `lineWidth: 80` folds a long plain scalar onto a
        // continuation line, indented deeper than the key. YAML folds a
        // single line break inside a plain scalar into one space.
        let text = concat!(
            "---\n",
            "name: Retry helper\n",
            "slug: retry-helper\n",
            "type: concept\n",
            "sources:\n",
            "  - path: src/app.ts\n",
            "    hash: aaaa\n",
            "sources_digest: bbbb\n",
            "links:\n",
            "  - to: backoff\n",
            "    relation: uses\n",
            "    description: this description is written on purpose so it runs well past\n",
            "      the eighty column fold point js-yaml uses when it dumps frontmatter\n",
            "generator:\n",
            "  version: 1\n",
            "---\n",
            "<!-- context:generated:start -->\n",
            "## Summary\n",
            "\n",
            "_(no summary)_\n",
            "<!-- context:generated:end -->\n"
        );
        let node = ConceptNode::parse("retry-helper", text);
        assert_eq!(node.links.len(), 1);
        assert_eq!(
            node.links[0].description.as_deref(),
            Some(
                "this description is written on purpose so it runs well past the eighty column fold point js-yaml uses when it dumps frontmatter"
            )
        );
    }

    /// DV8 (P1-07): a `sources` entry with no `hash` still parses, as
    /// the reader takes the frontmatter as is.
    #[test]
    fn test_p1_07_dv8_parse_reads_a_source_with_no_hash() {
        let node = ConceptNode::parse(
            "stem",
            "---\nname: Widget Notes\nslug: widget-notes\nsources:\n  - path: src/widget.ts\n---\nbody\n",
        );
        assert_eq!(node.name, "Widget Notes");
        assert_eq!(node.slug, "widget-notes");
        assert_eq!(node.sources.len(), 1);
        assert_eq!(node.sources[0].path, "src/widget.ts");
        assert_eq!(node.sources[0].hash, None);
    }

    #[test]
    fn parse_with_no_covers_gives_an_empty_vector() {
        let text = "---\nname: Widget\nslug: widget\ntype: concept\n---\nbody\n";
        let node = ConceptNode::parse("widget", text);
        assert_eq!(node.name, "Widget");
        assert!(node.covers.is_empty());
        assert_eq!(node.body, "body\n");
    }

    #[test]
    fn skip_test_one_root_level_manifest_file_stem_drops_the_card() {
        let dir = unique_temp_dir("skip1");
        fs::write(
            dir.join("manifest.json"),
            r#"{"version":1,"model":"m","repoDigest":"d","files":[{"path":"app.ts","hash":"h"}],"nodes":[]}"#,
        )
        .expect("write manifest");
        // No `slug` in frontmatter, and the stem "app" is a root-level
        // manifest file, so this is a wiring card for app.ts, not a
        // concept node. The body's first line is deliberately not a
        // wiring-card heading, so only the manifest-stem test can fire.
        fs::write(dir.join("app.md"), "---\n{}\n---\nnot a heading\n").expect("write card");

        let nodes = read_nodes(&dir);
        assert!(nodes.is_empty());
    }

    #[test]
    fn skip_test_two_covers_only_frontmatter_drops_the_card() {
        let dir = unique_temp_dir("skip2");
        // No manifest, so the first skip test never fires; the file has
        // no `name`, `type` or `sources`, only `covers`, so the third
        // skip test fires.
        fs::write(
            dir.join("notes.md"),
            "---\ncovers:\n  - symbol: run\n    kind: method\n    at: src/a.ts:L1-L2\n---\nbody\n",
        )
        .expect("write card");

        let nodes = read_nodes(&dir);
        assert!(nodes.is_empty());
    }

    #[test]
    fn skip_test_three_wiring_card_heading_drops_the_card() {
        let dir = unique_temp_dir("skip3");
        // No slug, no manifest match, no covers-only frontmatter, but the
        // body opens with a wiring card heading.
        fs::write(
            dir.join("util.md"),
            "---\n{}\n---\n# util.ts\n\nsome body\n",
        )
        .expect("write card");

        let nodes = read_nodes(&dir);
        assert!(nodes.is_empty());
    }

    #[test]
    fn a_real_concept_node_survives_every_skip_test() {
        let dir = unique_temp_dir("keep");
        fs::write(
            dir.join("widget.md"),
            "---\nname: Widget\nslug: widget\ntype: concept\n---\n# not a card heading\n",
        )
        .expect("write card");

        let nodes = read_nodes(&dir);
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].slug, "widget");
    }

    #[test]
    fn read_nodes_sorts_by_slug_with_icu_not_byte_order() {
        let dir = unique_temp_dir("sort");
        // Byte order puts "Bravo" (0x42) before "alpha" (0x61). ICU root
        // collation treats case as a tie-break under the base letter, so
        // "alpha" sorts first.
        fs::write(
            dir.join("Bravo.md"),
            "---\nname: Bravo\nslug: Bravo\n---\nb\n",
        )
        .expect("write");
        fs::write(
            dir.join("alpha.md"),
            "---\nname: alpha\nslug: alpha\n---\na\n",
        )
        .expect("write");

        let nodes = read_nodes(&dir);
        let slugs: Vec<&str> = nodes.iter().map(|n| n.slug.as_str()).collect();
        assert_eq!(slugs, vec!["alpha", "Bravo"]);
    }

    /// F4: a non-string `slug` still counts as present, because the reader
    /// coerces it with `String(fm.slug)`, and `String(0)` is `"0"`, a
    /// non-empty string.
    #[test]
    fn skip_test_one_a_numeric_slug_keeps_the_card() {
        let dir = unique_temp_dir("numeric-slug");
        // The body is deliberately not a wiring-card heading and there is
        // no manifest, so only the slug test can decide this file's fate.
        fs::write(dir.join("zero.md"), "---\nslug: 0\n---\nnot a heading\n").expect("write card");

        let nodes = read_nodes(&dir);
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].slug, "0");
    }

    /// F5: an explicit folded block scalar (`>-`) parses without error and
    /// leaves the fields Sieve reads untouched.
    #[test]
    fn parse_reads_an_explicit_folded_block_description() {
        let text = concat!(
            "---\n",
            "name: Widget\n",
            "slug: widget\n",
            "description: >-\n",
            "  first line\n",
            "  second line\n",
            "---\n",
            "body\n"
        );
        let node = ConceptNode::parse("widget", text);
        assert_eq!(node.name, "Widget");
        assert_eq!(node.body, "body\n");
    }

    /// F5: a single-quoted YAML scalar escapes an embedded quote by
    /// doubling it (`'it''s'` decodes to `it's`).
    #[test]
    fn parse_decodes_a_single_quoted_escaped_apostrophe() {
        let text = "---\nname: 'it''s widget'\nslug: widget\n---\nbody\n";
        let node = ConceptNode::parse("widget", text);
        assert_eq!(node.name, "it's widget");
    }

    /// F5: an empty `links: []` sequence parses as zero links, not as a
    /// missing key.
    #[test]
    fn parse_reads_an_explicit_empty_links_list() {
        let text = "---\nname: Widget\nslug: widget\nlinks: []\n---\nbody\n";
        let node = ConceptNode::parse("widget", text);
        assert!(node.links.is_empty());
    }

    /// F5: the frontmatter split takes the first closing `---` fence; a
    /// later `---` line inside the body stays in the body untouched.
    #[test]
    fn split_frontmatter_keeps_a_later_dashes_line_in_the_body() {
        let text = "---\nname: Widget\nslug: widget\n---\nbefore\n---\nafter\n";
        let (yaml, body) = split_frontmatter(text).expect("frontmatter present");
        assert_eq!(yaml, "name: Widget\nslug: widget");
        assert_eq!(body, "before\n---\nafter\n");
    }

    #[test]
    fn manifest_reads_the_golden_key_names() {
        let dir = unique_temp_dir("manifest");
        fs::write(
            dir.join("manifest.json"),
            r#"{
  "version": 1,
  "model": "openai:gpt-4o-mini",
  "repoDigest": "deadbeef",
  "files": [ { "path": "src/app.ts", "hash": "aaaa" } ],
  "nodes": [ { "slug": "widget", "name": "Widget", "type": "concept", "sources": ["src/app.ts"], "sourcesDigest": "bbbb" } ]
}"#,
        )
        .expect("write manifest");

        let manifest = Manifest::read(&dir).expect("manifest parses");
        assert_eq!(manifest.version, 1);
        assert_eq!(manifest.repo_digest, "deadbeef");
        assert_eq!(manifest.files[0].path, "src/app.ts");
        assert_eq!(manifest.nodes[0].node_type, "concept");
        assert_eq!(manifest.nodes[0].sources_digest, "bbbb");
    }
}
