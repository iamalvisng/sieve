//! `blast --format markdown` and `--format mermaid` (the report text, plus the
//! pieces the markdown symbol list quotes; P1-24, P1-26).

use std::collections::{HashMap, HashSet};
use std::path::Path;

use regex::Regex;

use super::owners::{mention, now_ms, since_label};
use super::{
    caveat_lines, depth_label, plural, BlastReport, ChangedFile, Impacted, Owner, Seed, TestSignal,
};
use crate::callers::js_trim;

/// Diagram cap: everything past it folds into one tail circle.
const MAX_MODULE_BOXES: usize = 5;
/// Rows in the table under the diagram.
const MAX_TABLE_ROWS: usize = 6;
/// Symbols listed in the collapsed list.
const MAX_SYMBOLS_LISTED: usize = 60;
/// Rows in the collapsed ownership table.
const MAX_OWNER_ROWS: usize = 8;

/// `plural` with an irregular plural form.
fn plural_as(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// Quotes end a Mermaid label, and angle brackets inject markup.
fn escape_label(text: &str) -> String {
    text.replace('"', "#quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// A quoted Mermaid label: each line escaped, then joined with `<br/>`.
fn label(lines: &[&str]) -> String {
    let body: Vec<String> = lines.iter().map(|l| escape_label(l)).collect();
    format!("\"{}\"", body.join("<br/>"))
}

/// The diagram: one circle per area that can be affected.
/// `None` when there is nothing to draw.
pub fn mermaid_diagram(r: &BlastReport) -> Option<String> {
    let shown = &r.modules[..r.modules.len().min(MAX_MODULE_BOXES)];
    if shown.is_empty() {
        return None;
    }
    let hidden = &r.modules[shown.len()..];
    let mut lines = vec!["flowchart TB".to_string()];
    for (i, m) in shown.iter().enumerate() {
        let count = plural(m.symbols.len(), "symbol");
        lines.push(format!("  A{i}(({}))", label(&[&m.label, &count])));
    }
    if !hidden.is_empty() {
        let symbols: usize = hidden.iter().map(|m| m.symbols.len()).sum();
        lines.push(format!(
            "  AX(({}))",
            label(&[
                &plural(hidden.len(), "smaller area"),
                &plural(symbols, "symbol")
            ])
        ));
    }
    lines.push(
        "  classDef reached fill:#D9EDF3,stroke:#3AA7C9,stroke-width:1.5px,color:#0E313C;"
            .to_string(),
    );
    let ids: Vec<String> = (0..shown.len()).map(|i| format!("A{i}")).collect();
    lines.push(format!("  class {} reached;", ids.join(",")));
    if !hidden.is_empty() {
        lines.push(
            "  classDef tail fill:#EEF2F3,stroke:#9AA4A9,stroke-width:1px,color:#3A4247;"
                .to_string(),
        );
        lines.push("  class AX tail;".to_string());
    }
    Some(lines.join("\n"))
}

// ---------------------------------------------------------------------
// Evidence
// ---------------------------------------------------------------------

/// `L12-L34` or `L12` → `(12, 34)`.
pub(super) fn span_range(span: &str) -> Option<(usize, usize)> {
    let rest = span.strip_prefix('L')?;
    let (start, end) = match rest.split_once("-L") {
        Some((s, e)) => (s, e),
        None => (rest, rest),
    };
    if !start.bytes().all(|b| b.is_ascii_digit()) || !end.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((start.parse().ok()?, end.parse().ok()?))
}

/// What a dependent's line can name: the changed symbols, then the
/// changed modules. `name_text` holds the symbol
/// name behind each entry of `names`, in the same order.
pub(super) struct ReachTerms {
    pub(super) name_text: Vec<String>,
    pub(super) names: Vec<Regex>,
    pub(super) modules: Vec<Regex>,
}

/// A symbol name as a whole ASCII word: `export` must not hit
/// `exportViz` (a non-`u` regex).
fn word_re(name: &str) -> Option<Regex> {
    Regex::new(&format!(r"(?-u:\b){}(?-u:\b)", regex::escape(name))).ok()
}

pub(super) fn reach_terms(seeds: &[Seed], changed: &[ChangedFile]) -> ReachTerms {
    let mut name_text: Vec<String> = Vec::new();
    for s in seeds.iter().filter(|s| !s.whole_file) {
        if !name_text.contains(&s.name) {
            name_text.push(s.name.clone());
        }
    }
    // JS `length` counts UTF-16 units; the sort is stable.
    name_text.sort_by_key(|n| std::cmp::Reverse(n.encode_utf16().count()));
    let mut stems: Vec<String> = Vec::new();
    for f in changed {
        let no_ext = match f.path.rfind('.') {
            Some(cut) if !f.path[cut + 1..].contains('/') && cut + 1 < f.path.len() => {
                &f.path[..cut]
            }
            _ => f.path.as_str(),
        };
        let stem = no_ext.rsplit('/').next().unwrap_or("");
        if !stem.is_empty() && !stems.iter().any(|s| s == stem) {
            stems.push(stem.to_string());
        }
    }
    // `word_re` escapes the name, so every name compiles; a name that did
    // not would leave `name_text` one longer than `names`.
    let names: Vec<Regex> = name_text.iter().filter_map(|n| word_re(n)).collect();
    ReachTerms {
        name_text,
        names,
        modules: stems
            .iter()
            .filter_map(|stem| {
                Regex::new(&format!(
                    "[\"'`][^\"'`]*(?-u:\\b){}(\\.[a-z]+)?[\"'`]",
                    regex::escape(stem)
                ))
                .ok()
            })
            .collect(),
    }
}

/// Reads each file once, and never fails.
pub(super) struct FileReader<'a> {
    root: Option<&'a Path>,
    cache: HashMap<String, Option<Vec<String>>>,
}

impl<'a> FileReader<'a> {
    /// A reader over `root`. With no root every read is `None`.
    pub(super) fn new(root: Option<&'a Path>) -> Self {
        FileReader {
            root,
            cache: HashMap::new(),
        }
    }

    /// The file's lines, split on `\n`, or `None` when it cannot be read.
    pub(super) fn read(&mut self, path: &str) -> Option<&Vec<String>> {
        let root = self.root?;
        if !self.cache.contains_key(path) {
            let lines = std::fs::read(root.join(path)).ok().map(|b| {
                String::from_utf8_lossy(&b)
                    .split('\n')
                    .map(str::to_string)
                    .collect()
            });
            self.cache.insert(path.to_string(), lines);
        }
        self.cache.get(path).and_then(Option::as_ref)
    }
}

/// The first line inside `span` that one of `needles` matches.
pub(super) fn reference_line(
    lines: &[String],
    span: &str,
    needles: &[Regex],
) -> Option<(usize, String)> {
    let (start, end) = span_range(span)?;
    let from = start.saturating_sub(1).min(lines.len());
    let to = end.min(lines.len()).max(from);
    lines[from..to]
        .iter()
        .enumerate()
        .find(|(_, text)| needles.iter().any(|re| re.is_match(text)))
        .map(|(i, text)| (start + i, text.clone()))
}

/// The line that reaches the diff, for one dependent.
fn evidence_line(
    sym: &Impacted,
    reach: &ReachTerms,
    read: &mut FileReader,
) -> Option<(usize, String)> {
    let lines = read.read(&sym.path)?;
    reference_line(lines, &sym.span, &reach.names)
        .or_else(|| reference_line(lines, &sym.span, &reach.modules))
}

// --------------------------------------------------------------------- The
// markdown report
// ---------------------------------------------------------------------

/// The markdown title: the product name and no emoji.
fn blast_title(product: &str) -> String {
    format!("### {product} blast radius")
}

/// The PR-comment body: diagram, one table, then everything else
/// collapsed. `root` lets the symbol list quote the line that reaches the
/// diff; without it the list loses only the snippet.
pub fn format_blast_markdown(r: &BlastReport, root: Option<&Path>) -> String {
    let product = sieve_core::product().name;
    let symbols: usize = r.modules.iter().map(|m| m.symbols.len()).sum();
    let mut out: Vec<String> = vec![blast_title(product), String::new()];
    out.push(headline(r, symbols));
    if let Some(line) = test_headline(r) {
        out.push(line);
    }
    if let Some(tag) = tag_line(r) {
        out.push(tag);
    }
    if symbols > 0 {
        if let Some(diagram) = mermaid_diagram(r) {
            out.push(String::new());
            out.push("```mermaid".to_string());
            out.push(diagram);
            out.push("```".to_string());
        }
        out.push(String::new());
        out.extend(impact_table(r));
    }
    let owners = owner_section(r);
    if !owners.is_empty() {
        out.push(String::new());
        out.extend(owners);
    }
    out.push(String::new());
    let reach = reach_terms(&r.seeds, &r.changed);
    let mut read = FileReader::new(root);
    out.extend(detail_sections(r, symbols, &reach, &mut read));
    let caveats = caveat_lines(r);
    if !caveats.is_empty() {
        out.push(String::new());
        out.extend(caveats);
    }
    out.push(String::new());
    out.push(format!(
        "<sub>`{product} blast` · {} · {} · {}</sub>",
        r.basis,
        depth_label(r.depth),
        plural(r.changed.len(), "changed file")
    ));
    format!("{}\n", out.join("\n"))
}

fn headline(r: &BlastReport, symbols: usize) -> String {
    let areas = plural(r.areas.len(), "area");
    if symbols == 0 {
        return format!(
            "**Nothing outside this diff depends on it.** {areas} changed; no indexed dependents at {}.",
            depth_label(r.depth)
        );
    }
    format!(
        "**{areas} changed → {} can be affected.** {}, {}.",
        plural(r.modules.len(), "area"),
        plural(symbols, "dependent symbol"),
        depth_label(r.depth)
    )
}

/// One sentence on whether the diff brought its tests.
fn test_headline(r: &BlastReport) -> Option<String> {
    if r.areas.is_empty() {
        return None;
    }
    let labels = |state: TestSignal| -> Vec<&str> {
        r.areas
            .iter()
            .filter(|a| a.tests == state)
            .map(|a| a.label.as_str())
            .collect()
    };
    let none = labels(TestSignal::None);
    let stale = labels(TestSignal::Stale);
    let changed = labels(TestSignal::Changed);
    let mut parts: Vec<String> = Vec::new();
    if !none.is_empty() {
        parts.push(format!("**no test reaches {}**", none.join(", ")));
    }
    if !stale.is_empty() {
        let verb = if stale.len() == 1 {
            "has tests"
        } else {
            "have tests"
        };
        parts.push(format!(
            "{} {verb} the diff did not touch",
            stale.join(", ")
        ));
    }
    if !changed.is_empty() {
        let whose = if changed.len() == 1 { "its" } else { "their" };
        parts.push(format!(
            "{} updated {whose} tests",
            plural(changed.len(), "area")
        ));
    }
    if parts.is_empty() {
        return None;
    }
    Some(format!("Tests: {}.", parts.join("; ")))
}

/// The table: one row per affected area, with the nearest symbol.
fn impact_table(r: &BlastReport) -> Vec<String> {
    let shown = &r.modules[..r.modules.len().min(MAX_TABLE_ROWS)];
    let hidden = &r.modules[shown.len()..];
    let mut area_of: HashMap<&str, &str> = HashMap::new();
    for a in &r.areas {
        for f in &a.files {
            area_of.insert(f.as_str(), a.label.as_str());
        }
    }
    let mut rows = vec![
        "| Can be affected | Symbols | Nearest hop | Reached from |".to_string(),
        "| --- | --: | --- | --- |".to_string(),
    ];
    for m in shown {
        let hop = match m.symbols.first() {
            Some(n) => format!(
                "`{}:{}` {} — {}, depth {}",
                n.path,
                n.span,
                n.name,
                n.relation.as_str(),
                n.depth
            ),
            None => "—".to_string(),
        };
        let mut names: Vec<&str> = Vec::new();
        for f in &m.from {
            let name = area_of.get(f.as_str()).copied().unwrap_or(f.as_str());
            if !names.contains(&name) {
                names.push(name);
            }
        }
        let from = match names.len() {
            0 => "—".to_string(),
            1 | 2 => names.join(", "),
            n => format!("{} +{}", names[..2].join(", "), n - 2),
        };
        rows.push(format!(
            "| {} | {} | {hop} | {from} |",
            m.label,
            m.symbols.len()
        ));
    }
    if !hidden.is_empty() {
        let symbols: usize = hidden.iter().map(|m| m.symbols.len()).sum();
        let names: Vec<&str> = hidden.iter().take(3).map(|m| m.label.as_str()).collect();
        let more = if hidden.len() > 3 { ", …" } else { "" };
        rows.push(format!(
            "| _{}_ | {symbols} | {}{more} | see below |",
            plural(hidden.len(), "smaller area"),
            names.join(", ")
        ));
    }
    rows
}

/// The tag line: who to ask, and why them.
fn tag_line(r: &BlastReport) -> Option<String> {
    let people = r.reviewers.as_deref().unwrap_or(&[]);
    if people.is_empty() {
        return None;
    }
    let total = r.areas.len() + r.modules.len();
    let bits: Vec<String> = people
        .iter()
        .map(|p| {
            let who = match &p.handle {
                Some(h) => format!("@{h}"),
                None => format!("**{}**", p.name),
            };
            let why = if p.areas.len() >= 3 {
                format!("{} of {total} areas", p.areas.len())
            } else {
                p.areas.join(", ")
            };
            format!("{who} — {why}")
        })
        .collect();
    Some(format!("Tag: {}", bits.join(" · ")))
}

/// `Name — 57 commits, last 9d ago`, for one table cell.
fn owner_cell(owners: Option<&[Owner]>, now: i64) -> String {
    match owners {
        None | Some([]) => "_only you — nobody else has touched these files_".to_string(),
        Some(list) => list
            .iter()
            .map(|o| {
                format!(
                    "{} — {}, last {}",
                    mention(&o.name, o.handle.as_deref()),
                    plural(o.commits as usize, "commit"),
                    since_label(o.last, now)
                )
            })
            .collect::<Vec<_>>()
            .join(" · "),
    }
}

struct OwnerRow<'a> {
    label: &'a str,
    side: &'static str,
    owners: Option<&'a [Owner]>,
}

/// The collapsed evidence behind the tag line.
fn owner_section(r: &BlastReport) -> Vec<String> {
    if r.reviewers.is_none() {
        return Vec::new();
    }
    let rows: Vec<OwnerRow> = r
        .areas
        .iter()
        .map(|a| OwnerRow {
            label: &a.label,
            side: "changed",
            owners: a.owners.as_deref(),
        })
        .chain(r.modules.iter().map(|m| OwnerRow {
            label: &m.label,
            side: "affected",
            owners: m.owners.as_deref(),
        }))
        .collect();
    let has_names = |row: &OwnerRow| row.owners.is_some_and(|o| !o.is_empty());
    if !rows.iter().any(has_names) {
        return Vec::new();
    }
    let now = now_ms();
    let mut people: HashSet<&str> = HashSet::new();
    for row in &rows {
        for o in row.owners.unwrap_or(&[]) {
            people.insert(o.handle.as_deref().unwrap_or(&o.name));
        }
    }
    let mut out = vec![
        "<details>".to_string(),
        format!(
            "<summary><strong>Who knows this code</strong> — {} across {}</summary>",
            plural_as(people.len(), "person", "people"),
            plural(rows.len(), "area")
        ),
        String::new(),
        "| Area | Who knows it |".to_string(),
        "| --- | --- |".to_string(),
    ];
    // Areas with names first: a row saying "only you" is context, not an
    // answer.
    let ordered: Vec<&OwnerRow> = rows
        .iter()
        .filter(|row| has_names(row))
        .chain(rows.iter().filter(|row| !has_names(row)))
        .collect();
    for row in ordered.iter().take(MAX_OWNER_ROWS) {
        out.push(format!(
            "| **{}** · {} | {} |",
            row.label,
            row.side,
            owner_cell(row.owners, now)
        ));
    }
    if ordered.len() > MAX_OWNER_ROWS {
        out.push(format!(
            "| _…{}_ | |",
            plural(ordered.len() - MAX_OWNER_ROWS, "further area")
        ));
    }
    out.push(String::new());
    out.push(
        "_Ownership is git history over each area's own files, weighted towards recent work \
         (120-day half-life). Merge commits and bots are dropped, and you are dropped from your own PR. \
         A name with no `@` has no GitHub handle in its commit email — tag them by hand, or add a \
         `.mailmap` entry. A suggestion from history, not a CODEOWNERS rule._"
            .to_string(),
    );
    out.push(String::new());
    out.push("</details>".to_string());
    out
}

/// The three collapsed sections nobody reads by default.
fn detail_sections(
    r: &BlastReport,
    symbols: usize,
    reach: &ReachTerms,
    read: &mut FileReader,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if symbols > 0 {
        out.push("<details>".to_string());
        out.push(format!(
            "<summary><strong>All {}</strong>, grouped by area</summary>",
            plural(symbols, "dependent symbol")
        ));
        out.push(String::new());
        let mut listed = 0usize;
        for m in &r.modules {
            out.push(format!(
                "**{}** — {} in {}",
                m.label,
                plural(m.symbols.len(), "symbol"),
                plural(m.files.len(), "file")
            ));
            out.push(String::new());
            for s in &m.symbols {
                // JS `listed++ >= MAX`: the count steps before the test.
                let was = listed;
                listed += 1;
                if was >= MAX_SYMBOLS_LISTED {
                    break;
                }
                out.push(format!(
                    "- `{}:{}` — {} ({}, depth {})",
                    s.path,
                    s.span,
                    s.name,
                    s.relation.as_str(),
                    s.depth
                ));
                if let Some((n, text)) = evidence_line(s, reach, read) {
                    out.push(format!("  ```{n}: {}```", js_trim(&text)));
                }
            }
            out.push(String::new());
            if listed >= MAX_SYMBOLS_LISTED {
                out.push(format!(
                    "…{} not listed.",
                    plural(symbols.saturating_sub(listed), "further symbol")
                ));
                out.push(String::new());
                break;
            }
        }
        out.push("</details>".to_string());
    }
    if !r.areas.is_empty() {
        let by = |state: TestSignal| r.areas.iter().filter(|a| a.tests == state).count();
        let states: Vec<String> = [
            (by(TestSignal::Changed), "✓"),
            (by(TestSignal::Stale), "⚠"),
            (by(TestSignal::None), "✗"),
            (by(TestSignal::Na), "–"),
        ]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, glyph)| format!("{n} {glyph}"))
        .collect();
        out.push("<details>".to_string());
        out.push(format!(
            "<summary><strong>Test signal</strong> per changed area — {}</summary>",
            states.join(" · ")
        ));
        out.push(String::new());
        out.push("_Reached = a node under a test path has a resolved edge into the changed symbol. It undercounts anything called indirectly — through a CLI, a spawned process or a dynamic import — so read a low ratio as “look here”, never as a coverage gate._".to_string());
        out.push(String::new());
        for a in &r.areas {
            let mut bits: Vec<String> = Vec::new();
            if a.behavioural > 0 {
                bits.push(format!("{} of {} reached", a.reached, a.behavioural));
            }
            if !a.changed_test_files.is_empty() {
                let files: Vec<String> = a
                    .changed_test_files
                    .iter()
                    .map(|f| format!("`{f}`"))
                    .collect();
                bits.push(format!(
                    "{} changed here: {}",
                    plural(a.changed_test_files.len(), "test file"),
                    files.join(", ")
                ));
            } else if !a.test_files.is_empty() {
                let verb = if a.test_files.len() == 1 {
                    "reaches"
                } else {
                    "reach"
                };
                bits.push(format!(
                    "{} {verb} it, none changed here",
                    plural(a.test_files.len(), "test file")
                ));
            } else if a.behavioural > 0 {
                bits.push("no test file reaches it".to_string());
            } else {
                bits.push("no function, method or class changed here".to_string());
            }
            out.push(format!(
                "- {} **{}** — {}",
                a.tests.glyph(),
                a.label,
                bits.join(" · ")
            ));
            if !a.unreached.is_empty() {
                let names: Vec<String> = a
                    .unreached
                    .iter()
                    .take(8)
                    .map(|n| format!("`{n}`"))
                    .collect();
                let more = if a.unreached.len() > 8 {
                    format!(", …{} more", a.unreached.len() - 8)
                } else {
                    String::new()
                };
                out.push(format!("  - not reached: {}{more}", names.join(", ")));
            }
        }
        out.push(String::new());
        out.push("</details>".to_string());
    }
    if !r.test_modules.is_empty() {
        let symbol_count: usize = r.test_modules.iter().map(|m| m.symbols.len()).sum();
        let set: HashSet<&str> = r
            .test_modules
            .iter()
            .flat_map(|m| m.files.iter().map(String::as_str))
            .collect();
        let mut files: Vec<&str> = set.into_iter().collect();
        files.sort_unstable();
        let verb = if files.len() == 1 {
            "references"
        } else {
            "reference"
        };
        out.push("<details>".to_string());
        out.push(format!(
            "<summary>{} also {verb} this code</summary>",
            plural(files.len(), "test suite")
        ));
        out.push(String::new());
        out.push(format!(
            "{}, kept out of the diagram and the table so they cannot crowd out the areas a reviewer has to look at.",
            plural(symbol_count, "symbol")
        ));
        out.push(String::new());
        for f in files.iter().take(20) {
            out.push(format!("- `{f}`"));
        }
        if files.len() > 20 {
            out.push(format!("- …{} more", files.len() - 20));
        }
        out.push(String::new());
        out.push("</details>".to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_savings_blast_title_has_no_leaf_under_sieve() {
        assert_eq!(blast_title("sieve"), "### sieve blast radius");
    }

    /// P1-26: a label escapes quotes and angle brackets per line, then
    /// joins with `<br/>`.
    #[test]
    fn test_p1_26_mermaid_label_escapes_each_line() {
        assert_eq!(label(&["a\"b", "<c>"]), "\"a#quot;b<br/>&lt;c&gt;\"");
    }

    /// P1-26: `wordRe` is an ASCII word match; `export` misses `exportViz`.
    #[test]
    fn test_p1_26_word_re_is_a_whole_ascii_word() {
        let re = word_re("export").expect("compiles");
        assert!(re.is_match("return export();"));
        assert!(!re.is_match("exportViz()"));
        assert!(re.is_match("éexport"));
    }

    /// P1-26: the module-stem regex needs a quote, the stem as a word, and
    /// an optional ASCII extension before the closing quote.
    #[test]
    fn test_p1_26_module_stem_regex_matches_an_import_specifier() {
        let changed = vec![ChangedFile {
            path: "ts/derived.ts".to_string(),
            status: super::super::ChangeStatus::Modified,
            old_path: None,
            ranges: Vec::new(),
            hunks: Vec::new(),
        }];
        let reach = reach_terms(&[], &changed);
        assert_eq!(reach.modules.len(), 1);
        let re = &reach.modules[0];
        assert!(re.is_match("import { x } from \"../ts/derived\";"));
        assert!(re.is_match("require('./derived.js')"));
        assert!(!re.is_match("import { x } from \"../ts/underived\";"));
        assert!(!re.is_match("derived"));
    }

    /// P1-26: `spanRange` reads both `L12-L34` and a bare `L12`.
    #[test]
    fn test_p1_26_span_range_accepts_a_single_line_span() {
        assert_eq!(span_range("L12-L34"), Some((12, 34)));
        assert_eq!(span_range("L7"), Some((7, 7)));
        assert_eq!(span_range("x"), None);
    }
}
