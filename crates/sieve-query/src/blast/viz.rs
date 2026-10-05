//! `blast --export-viz` (P1-24, P1-65): the context graph of one blast report,
//! plus the `repoLabel`. The page itself comes from
//! `crate::viz::export_page_with`.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Command;

use sieve_core::Kind;

use super::owners::iso_day;
use super::render::{reach_terms, reference_line, span_range, FileReader, ReachTerms};
use super::{
    plural, BlastReport, ChangeStatus, ChangedArea, ChangedFile, Impacted, ImpactedModule, Owner,
    Seed, TestSignal,
};
use crate::viz::{ContextEdge, ContextGraph, ContextNode, Evidence, EvidenceLine, NodeOwner};

/// Symbols given their own snippet per node.
const MAX_EVIDENCE: usize = 4;
/// Lines shown per snippet before it folds.
const MAX_LINES: usize = 6;

/// Owners as the page carries them. Empty becomes
/// `None`, so the key is absent.
fn node_owners(owners: Option<&[Owner]>) -> Option<Vec<NodeOwner>> {
    match owners {
        None | Some([]) => None,
        Some(list) => Some(
            list.iter()
                .map(|o| NodeOwner {
                    name: o.name.clone(),
                    handle: o.handle.clone(),
                    commits: o.commits,
                    last: iso_day(o.last),
                })
                .collect(),
        ),
    }
}

/// Kinds that carry behaviour. Sieve has no `constructor`
/// kind.
fn is_behavioural(kind: Kind) -> bool {
    matches!(kind, Kind::Function | Kind::Method | Kind::Class)
}

/// One sentence about the tests.
fn test_note(a: &ChangedArea) -> String {
    match a.tests {
        TestSignal::Changed => format!(
            "A test that reaches it changed too — {} of {} covered.",
            a.reached,
            plural(a.behavioural as usize, "function")
        ),
        TestSignal::Stale => format!(
            "Tests reach it, and this diff left all {} of them alone.",
            a.test_files.len()
        ),
        TestSignal::None => "No test reaches this code.".to_string(),
        TestSignal::Na => {
            "No function or class changed here — types, config or comments only.".to_string()
        }
    }
}

/// `a`, `a and b`, `a, b and c`, `a, b, c and 3 more`.
fn list(names: &[&str]) -> String {
    let shown = &names[..names.len().min(3)];
    let rest = names.len() - shown.len();
    if rest > 0 {
        return format!("{} and {rest} more", shown.join(", "));
    }
    match shown.split_last() {
        Some((last, [])) => (*last).to_string(),
        Some((last, head)) => format!("{} and {last}", head.join(", ")),
        None => String::new(),
    }
}

/// What this PR did to an area. `mine` is the area's
/// own seeds.
fn changed_summary(a: &ChangedArea, mine: &[&Seed]) -> String {
    let named: Vec<&Seed> = mine.iter().copied().filter(|s| !s.whole_file).collect();
    let behaviour: Vec<&str> = named
        .iter()
        .filter(|s| is_behavioural(s.kind))
        .map(|s| s.name.as_str())
        .collect();
    let rest: Vec<&str> = named
        .iter()
        .filter(|s| !is_behavioural(s.kind))
        .map(|s| s.name.as_str())
        .collect();
    let what = if !behaviour.is_empty() {
        let mut what = format!("Edits {}", list(&behaviour));
        if !rest.is_empty() {
            what.push_str(&format!(", plus {}", list(&rest)));
        }
        what
    } else if !rest.is_empty() {
        format!("Edits {}", list(&rest))
    } else {
        // Whole-file seeds only: an added file, or edits outside every
        // symbol.
        format!(
            "Changes {} outside any symbol",
            plural(a.seeds as usize, "region")
        )
    };
    format!(
        "{what} in {}. {}",
        plural(a.files.len(), "file"),
        test_note(a)
    )
}

/// `name · path:start-end`, or `name · path` when the span does not
/// parse.
fn label_for(name: &str, path: &str, span: &str) -> String {
    match span_range(span) {
        Some((start, end)) => format!("{name} · {path}:{start}-{end}"),
        None => format!("{name} · {path}"),
    }
}

/// Evidence for one changed symbol: the hunks inside its span, folded
/// at `MAX_LINES`.
fn seed_evidence(seed: &Seed, file: &ChangedFile) -> Option<Evidence> {
    let span = if seed.whole_file {
        None
    } else {
        span_range(&seed.span)
    };
    let mut lines: Vec<EvidenceLine> = Vec::new();
    let mut dropped: usize = 0;
    for h in &file.hunks {
        if let Some((start, end)) = span {
            if (h.end as usize) < start || (h.start as usize) > end {
                continue;
            }
        }
        lines.extend(h.lines.iter().map(|l| EvidenceLine {
            n: l.n,
            sign: l.sign,
            text: l.text.clone(),
        }));
        dropped += h.dropped as usize;
    }
    if lines.is_empty() && dropped == 0 {
        return None;
    }
    let folded = lines.len().saturating_sub(MAX_LINES);
    lines.truncate(MAX_LINES);
    let more = folded + dropped;
    Some(Evidence {
        label: if seed.whole_file {
            seed.path.clone()
        } else {
            label_for(&seed.name, &seed.path, &seed.span)
        },
        note: if file.status == ChangeStatus::Added {
            Some("new file".to_string())
        } else if seed.whole_file {
            Some("outside any symbol".to_string())
        } else {
            None
        },
        lines,
        more: (more > 0).then_some(more),
    })
}

/// Evidence for one dependent symbol: the line that names a changed
/// symbol, else the line that names a changed module, else nothing.
fn impacted_evidence(
    sym: &Impacted,
    reach: &ReachTerms,
    read: &mut FileReader,
) -> Option<Evidence> {
    let lines = read.read(&sym.path)?;
    let label = label_for(&sym.name, &sym.path, &sym.span);
    let quote = |n: usize, text: String| EvidenceLine {
        n: u32::try_from(n).ok(),
        sign: ' ',
        text,
    };
    if let Some((n, text)) = reference_line(lines, &sym.span, &reach.names) {
        let hit = reach
            .name_text
            .iter()
            .zip(&reach.names)
            .find(|(_, re)| re.is_match(&text))
            .map_or("", |(name, _)| name.as_str());
        return Some(Evidence {
            label,
            note: Some(format!("{} {hit}", sym.relation.as_str())),
            lines: vec![quote(n, text)],
            more: None,
        });
    }
    let (n, text) = reference_line(lines, &sym.span, &reach.modules)?;
    Some(Evidence {
        label,
        note: Some(sym.relation.as_str().to_string()),
        lines: vec![quote(n, text)],
        more: None,
    })
}

fn changed_node(
    a: &ChangedArea,
    seeds: &[Seed],
    by_path: &HashMap<&str, &ChangedFile>,
) -> ContextNode {
    let files: HashSet<&str> = a.files.iter().map(String::as_str).collect();
    let mine: Vec<&Seed> = seeds
        .iter()
        .filter(|s| files.contains(s.path.as_str()))
        .collect();
    let evidence: Vec<Evidence> = mine
        .iter()
        .take(MAX_EVIDENCE)
        .filter_map(|s| {
            by_path
                .get(s.path.as_str())
                .and_then(|f| seed_evidence(s, f))
        })
        .collect();
    ContextNode {
        id: format!("changed:{}", a.key),
        name: a.label.clone(),
        node_type: "changed".to_string(),
        summary: changed_summary(a, &mine),
        sources: a.files.clone(),
        evidence: (!evidence.is_empty()).then_some(evidence),
        owners: node_owners(a.owners.as_deref()),
    }
}

fn affected_node(m: &ImpactedModule, reach: &ReachTerms, read: &mut FileReader) -> ContextNode {
    let how = match m.symbols.first() {
        Some(n) => format!(
            " — nearest is {}, {} away.",
            n.name,
            plural(n.depth as usize, "hop")
        ),
        None => ".".to_string(),
    };
    let evidence: Vec<Evidence> = m
        .symbols
        .iter()
        .take(MAX_EVIDENCE)
        .filter_map(|s| impacted_evidence(s, reach, read))
        .collect();
    let verb = if m.symbols.len() == 1 {
        "reaches"
    } else {
        "reach"
    };
    ContextNode {
        id: format!("affected:{}", m.key),
        name: m.label.clone(),
        node_type: "affected".to_string(),
        summary: format!(
            "Not edited by this PR. {} here {verb} code it changed{how}",
            plural(m.symbols.len(), "symbol")
        ),
        sources: m
            .symbols
            .iter()
            .map(|s| format!("{} · {}", s.path, s.span))
            .collect(),
        evidence: (!evidence.is_empty()).then_some(evidence),
        owners: node_owners(m.owners.as_deref()),
    }
}

/// What an empty canvas means.
fn empty_note(r: &BlastReport) -> String {
    if r.changed.is_empty() {
        return "This pull request changes no files.".to_string();
    }
    if r.unindexed.len() == r.changed.len() {
        let shown: Vec<&str> = r.unindexed.iter().take(3).map(String::as_str).collect();
        let rest = if r.unindexed.len() > 3 {
            format!(", +{} more", r.unindexed.len() - 3)
        } else {
            String::new()
        };
        return format!(
            "Nothing to draw: no parser claims {} ({}{rest}), so this change has no symbols to trace.",
            plural(r.unindexed.len(), "changed file"),
            shown.join(", ")
        );
    }
    "Nothing to draw: the changed symbols have no resolved dependents at this depth.".to_string()
}

/// The viewer graph for a report: one `changed:` node
/// per area, one `affected:` node per module, and one `depends_on` edge
/// from each affected node to each changed node it reaches. `root` lets
/// the affected side quote the line that reaches the diff; without it
/// those nodes lose only the snippet.
pub fn blast_viz_graph(r: &BlastReport, root: Option<&Path>) -> ContextGraph {
    let by_path: HashMap<&str, &ChangedFile> =
        r.changed.iter().map(|f| (f.path.as_str(), f)).collect();
    let reach = reach_terms(&r.seeds, &r.changed);
    let mut read = FileReader::new(root);
    let mut nodes: Vec<ContextNode> = r
        .areas
        .iter()
        .map(|a| changed_node(a, &r.seeds, &by_path))
        .collect();
    nodes.extend(
        r.modules
            .iter()
            .map(|m| affected_node(m, &reach, &mut read)),
    );
    // Changed path → the id of the area node standing for it.
    let mut area_of: HashMap<&str, String> = HashMap::new();
    for a in &r.areas {
        for f in &a.files {
            area_of.insert(f.as_str(), format!("changed:{}", a.key));
        }
    }
    let mut edges: Vec<ContextEdge> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for m in &r.modules {
        for from in &m.from {
            // A changed test file is the signal, not an area.
            let Some(source) = area_of.get(from.as_str()) else {
                continue;
            };
            let affected = format!("affected:{}", m.key);
            if !seen.insert(format!("{affected}→{source}")) {
                continue;
            }
            // The edge is the dependency, affected → changed, not the
            // impact: the viewer words its panels from the edge.
            edges.push(ContextEdge {
                source: affected,
                target: source.clone(),
                relation: "depends_on".to_string(),
                description: Some("this area calls or imports code the PR changed".to_string()),
            });
        }
    }
    ContextGraph {
        node_count: nodes.len(),
        edge_count: edges.len(),
        empty_note: nodes.is_empty().then(|| empty_note(r)),
        skipped_files: r.unindexed.len(),
        dropped_edges: 0,
        nodes,
        edges,
    }
}

/// Drops the secret from a remote URL (P1-65 security rule). Removes the
/// userinfo of a `scheme://` URL. An `ssh` URL keeps its user name and loses
/// only a `:password`. An scp-style remote and a URL with no userinfo stay as
/// is.
pub fn strip_url_credentials(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    let Some((userinfo, host)) = authority.rsplit_once('@') else {
        return url.to_string();
    };
    let ssh = matches!(scheme.to_ascii_lowercase().as_str(), "ssh" | "git+ssh");
    if !ssh {
        return format!("{scheme}://{host}{tail}");
    }
    match userinfo.split_once(':') {
        Some((user, _)) => format!("{scheme}://{user}@{host}{tail}"),
        None => url.to_string(),
    }
}

/// What to call the repository in the appbar: the last segment of the `origin`
/// remote URL without `.git`, else the basename of `root`.
pub fn repo_label(root: &Path) -> String {
    let url = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| strip_url_credentials(String::from_utf8_lossy(&o.stdout).trim()));
    if let Some(url) = url {
        let stem = url.strip_suffix(".git").unwrap_or(&url);
        let name = stem.rsplit(['/', ':']).next().unwrap_or("");
        if !name.is_empty() {
            return name.to_string();
        }
    }
    root.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::super::{Hunk, LineRange};
    use super::*;
    use crate::test_support::TempDir;

    /// P1-65: `isoDay` is the UTC day of the epoch ms.
    #[test]
    fn test_p1_65_iso_day_is_the_utc_day() {
        assert_eq!(iso_day(0), "1970-01-01");
        // 2026-09-01T00:00:00+08:00 is 2026-08-31T16:00:00Z.
        assert_eq!(iso_day(1_788_192_000_000), "2026-08-31");
        // 2026-09-01T08:00:00+08:00, the pinned fixture date, is midnight UTC.
        assert_eq!(iso_day(1_788_220_800_000), "2026-09-01");
        assert_eq!(iso_day(951_782_400_000), "2000-02-29");
        assert_eq!(iso_day(-1), "1969-12-31");
    }

    /// P1-65: `list` names up to three, then counts the rest.
    #[test]
    fn test_p1_65_list_names_three_then_counts() {
        assert_eq!(list(&["a"]), "a");
        assert_eq!(list(&["a", "b"]), "a and b");
        assert_eq!(list(&["a", "b", "c"]), "a, b and c");
        assert_eq!(list(&["a", "b", "c", "d", "e"]), "a, b, c and 2 more");
    }

    fn seed(name: &str, path: &str, span: &str, kind: Kind, whole_file: bool) -> Seed {
        Seed {
            id: format!("{path}:{name}"),
            name: name.to_string(),
            kind,
            path: path.to_string(),
            span: span.to_string(),
            whole_file,
        }
    }

    fn area(key: &str, label: &str, files: &[&str], tests: TestSignal) -> ChangedArea {
        ChangedArea {
            label: label.to_string(),
            label_source: super::super::LabelSource::Symbol,
            key: key.to_string(),
            files: files.iter().map(|f| f.to_string()).collect(),
            seeds: 1,
            tests,
            test_files: vec!["t.ts".to_string()],
            changed_test_files: Vec::new(),
            reached: 1,
            behavioural: 2,
            unreached: Vec::new(),
            seed_names: Vec::new(),
            owners: Some(vec![Owner {
                name: "Other Person".to_string(),
                handle: None,
                commits: 3,
                score: 0.5,
                last: 1_788_220_800_000,
            }]),
        }
    }

    fn report(dir: &Path) -> BlastReport {
        std::fs::create_dir_all(dir.join("lib")).expect("mkdir");
        std::fs::write(
            dir.join("lib/a.ts"),
            "import { x } from \"../src/core\";\nexport function alpha() { return x(); }\n",
        )
        .expect("write a.ts");
        BlastReport {
            basis: "working tree vs HEAD".to_string(),
            depth: Some(1),
            changed: vec![ChangedFile {
                path: "src/core.ts".to_string(),
                status: ChangeStatus::Modified,
                old_path: None,
                ranges: vec![LineRange { start: 2, end: 2 }],
                hunks: vec![Hunk {
                    start: 2,
                    end: 2,
                    lines: (0..8)
                        .map(|i| super::super::DiffLine {
                            n: Some(i),
                            sign: '+',
                            text: format!("line {i}"),
                        })
                        .collect(),
                    dropped: 1,
                }],
            }],
            unindexed: Vec::new(),
            deleted: Vec::new(),
            seeds: vec![
                seed("x", "src/core.ts", "L1-L3", Kind::Function, false),
                seed("T", "src/core.ts", "L5-L5", Kind::Type, false),
            ],
            impacted: Vec::new(),
            modules: vec![ImpactedModule {
                label: "alpha".to_string(),
                label_source: super::super::LabelSource::Symbol,
                key: "lib/".to_string(),
                files: vec!["lib/a.ts".to_string()],
                symbols: vec![Impacted {
                    id: "lib/a.ts:alpha".to_string(),
                    name: "alpha".to_string(),
                    kind: Kind::Function,
                    path: "lib/a.ts".to_string(),
                    span: "L2-L2".to_string(),
                    relation: sieve_core::Relation::Calls,
                    depth: 1,
                }],
                from: vec![
                    "src/core.ts".to_string(),
                    "src/core.ts".to_string(),
                    "tests/t.ts".to_string(),
                ],
                owners: Some(Vec::new()),
            }],
            test_modules: Vec::new(),
            areas: vec![area("src/", "core", &["src/core.ts"], TestSignal::Stale)],
            reviewers: None,
        }
    }

    /// P1-24, P1-65: the graph carries one node per area and module, the
    /// summaries, the folded hunk evidence, the reference line with its
    /// `calls x` note, the owners without scores, and one deduplicated
    /// `depends_on` edge per module and area.
    #[test]
    fn test_p1_24_p1_65_blast_viz_graph_shape() {
        let dir = TempDir::new("blast-viz-graph");
        let r = report(&dir);
        let g = blast_viz_graph(&r, Some(&dir));
        assert_eq!((g.node_count, g.edge_count, g.skipped_files), (2, 1, 0));
        assert_eq!(g.empty_note, None);
        let changed = &g.nodes[0];
        assert_eq!(changed.id, "changed:src/");
        assert_eq!(
            changed.summary,
            "Edits x, plus T in 1 file. Tests reach it, and this diff left all 1 of them alone."
        );
        // `T` at L5 lies outside the one hunk at L2, so it has no block.
        let ev = changed.evidence.as_ref().expect("evidence");
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].label, "x · src/core.ts:1-3");
        assert_eq!((ev[0].lines.len(), ev[0].more), (6, Some(3)));
        let owners = changed.owners.as_ref().expect("owners");
        assert_eq!(
            (owners[0].commits, owners[0].last.as_str()),
            (3, "2026-09-01")
        );
        let affected = &g.nodes[1];
        assert_eq!(affected.id, "affected:lib/");
        assert_eq!(
            affected.summary,
            "Not edited by this PR. 1 symbol here reaches code it changed — nearest is alpha, 1 hop away."
        );
        assert_eq!(affected.sources, ["lib/a.ts · L2-L2"]);
        let ev = affected.evidence.as_ref().expect("evidence");
        assert_eq!(ev[0].note.as_deref(), Some("calls x"));
        assert_eq!(ev[0].lines[0].n, Some(2));
        assert!(affected.owners.is_none(), "empty owners drop the key");
        assert_eq!(
            (g.edges[0].source.as_str(), g.edges[0].target.as_str()),
            ("affected:lib/", "changed:src/")
        );

        // No root: the affected side loses only its snippet.
        let g = blast_viz_graph(&r, None);
        assert!(g.nodes[1].evidence.is_none());
    }

    /// P1-65: the empty notes name the reason.
    #[test]
    fn test_p1_65_empty_note_names_the_reason() {
        let dir = TempDir::new("blast-viz-empty");
        let mut r = report(&dir);
        r.areas.clear();
        r.modules.clear();
        assert_eq!(
            blast_viz_graph(&r, None).empty_note.as_deref(),
            Some("Nothing to draw: the changed symbols have no resolved dependents at this depth.")
        );
        r.unindexed = vec!["notes.md".to_string()];
        assert_eq!(
            blast_viz_graph(&r, None).empty_note.as_deref(),
            Some("Nothing to draw: no parser claims 1 changed file (notes.md), so this change has no symbols to trace.")
        );
        r.changed.clear();
        r.unindexed.clear();
        assert_eq!(
            blast_viz_graph(&r, None).empty_note.as_deref(),
            Some("This pull request changes no files.")
        );
    }

    /// P1-65: `repoLabel` reads the origin remote, else the basename.
    #[test]
    fn test_p1_65_repo_label_prefers_origin() {
        let dir = TempDir::new("blast-viz-label");
        let root = dir.join("edges");
        std::fs::create_dir_all(&root).expect("mkdir");
        assert_eq!(repo_label(&root), "edges");
        let git = |args: &[&str]| {
            assert!(Command::new("git")
                .args(args)
                .current_dir(&root)
                .status()
                .expect("git")
                .success());
        };
        git(&["init", "-q"]);
        git(&[
            "remote",
            "add",
            "origin",
            "git@github.com:acme/sieve-demo.git",
        ]);
        assert_eq!(repo_label(&root), "sieve-demo");
    }

    /// P1-65: the rule table for `strip_url_credentials`.
    #[test]
    fn test_p1_65_origin_url_drops_credentials_table() {
        let cases = [
            (
                "https://ghp_FAKE0000@github.com/o/r.git",
                "https://github.com/o/r.git",
            ),
            ("https://user:pass@host/r", "https://host/r"),
            ("https://user:pass@host:8443/r", "https://host:8443/r"),
            ("git@github.com:o/r.git", "git@github.com:o/r.git"),
            ("ssh://git@host/r", "ssh://git@host/r"),
            ("ssh://git:secret@host/r", "ssh://git@host/r"),
            ("https://github.com/o/r.git", "https://github.com/o/r.git"),
            ("https://host/a@b/r", "https://host/a@b/r"),
            ("https://tok@host", "https://host"),
            ("", ""),
            ("not a url", "not a url"),
        ];
        for (input, want) in cases {
            assert_eq!(strip_url_credentials(input), want, "input {input:?}");
        }
    }

    /// P1-65: a token in the origin never reaches the repo label, even
    /// when the URL has no path and the label is the authority.
    #[test]
    fn test_p1_65_origin_url_drops_credentials_in_repo_label() {
        let dir = TempDir::new("blast-viz-cred");
        let root = dir.join("proj");
        std::fs::create_dir_all(&root).expect("mkdir");
        for args in [
            &["init", "-q"][..],
            &["remote", "add", "origin", "https://ghp_FAKE0000@github.com"][..],
        ] {
            assert!(Command::new("git")
                .args(args)
                .current_dir(&root)
                .status()
                .expect("git")
                .success());
        }
        let label = repo_label(&root);
        assert!(!label.contains("ghp_FAKE0000"), "label {label:?}");
        assert_eq!(label, "github.com");
    }
}
