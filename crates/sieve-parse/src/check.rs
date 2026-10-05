//! `check_graph`/`check_context` — is the committed wiring graph still in
//! sync with the code? (P1-33 to P1-36, P4-42)
//!
//! This is a deterministic re-extraction, diffed against the committed graph by
//! `id` and `body_hash`, no LLM call.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::path::Path;

use serde::Serialize;

use sieve_core::concept::{read_nodes, Manifest};
use sieve_core::fingerprint::{decode_source, fingerprint_path, hash_source, read_fingerprint};
use sieve_core::walk::walk_repo;
use sieve_core::wiring::{Graph, SummaryState};

use crate::build::{build_graph_cached_with, BuildOptions};

/// The extensions the concept pass reads and `checkContext` re-hashes
/// (`CODE_EXTENSIONS`). This is a separate list from
/// `sieve_core::lang`'s parser table: it carries `.sql`, `.sh` and `.proto`,
/// which no Sieve parser reads, and it drops several extensions the breadth
/// tier claims (`.lua`, `.rb` sits in both lists, but `.zig`, `.clj`,
/// `.dart`, `.ex`, `.sol`, `.ml`, `.nix`, `.vue` do not appear here).
const CODE_EXTENSIONS: &[&str] = &[
    ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".py", ".go", ".rs", ".java", ".kt", ".scala",
    ".rb", ".php", ".c", ".h", ".cpp", ".hpp", ".cc", ".cs", ".swift", ".sql", ".sh", ".proto",
];

/// Cap on how many pending ids the OK-note lists (`PENDING_SAMPLE`).
pub const PENDING_SAMPLE: usize = 8;

/// Drift between the committed wiring graph and a fresh re-extraction.
///
/// Field order matches the `--json` output: `ok`, `missing`, `added`,
/// `removed`, `changed`, `stale`, `pending`, `pendingIds`, `nodes`.
#[derive(Debug, Clone, Serialize)]
pub struct GraphCheck {
    pub ok: bool,
    /// True when there is no `wiring.json` (a graph has never been built).
    pub missing: bool,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<String>,
    pub stale: Vec<String>,
    /// Nodes never summarized. Reported for context, not counted as drift.
    pub pending: usize,
    /// Ids of pending nodes, capped by [`PENDING_SAMPLE`] only when
    /// formatted as a report.
    #[serde(rename = "pendingIds")]
    pub pending_ids: Vec<String>,
    /// Committed nodes in total, the denominator behind the pending share.
    pub nodes: usize,
}

/// One source file whose recorded content hash no longer matches the file
/// on disk. The deep-layer type; Sieve never populates this today,
/// because Sieve writes no `manifest.json` (see [`check_context`]).
#[derive(Debug, Clone, Serialize)]
pub struct ContentDrift {
    pub path: String,
    pub from: String,
    pub to: String,
}

/// Drift between the committed deep-layer manifest and the code on disk.
///
/// Field order matches the `--json` output: `ok`, `missing`,
/// `contentDrift`, `removed`, `coverage`, `indexDrift`.
#[derive(Debug, Clone, Serialize)]
pub struct ContextCheck {
    pub ok: bool,
    /// True when there is no `manifest.json` (no meaning-tier build has run).
    pub missing: bool,
    #[serde(rename = "contentDrift")]
    pub content_drift: Vec<ContentDrift>,
    pub removed: Vec<String>,
    pub coverage: Vec<String>,
    #[serde(rename = "indexDrift")]
    pub index_drift: Vec<String>,
}

/// Checks whether the committed `wiring.json` still matches the code.
///
/// This loads `<context_dir>/.graph/wiring.json`; a missing file reports
/// `missing: true` with every array empty. Otherwise this re-extracts the
/// repo with [`build_graph_cached`] (read-only towards `wiring.json`
/// itself: this never writes that file, only its own extraction cache) and
/// diffs the fresh nodes against the committed ones by `id` and
/// `body_hash`, the same three-way split `checkGraph` runs.
pub fn check_graph(root: &Path, context_dir: &Path) -> io::Result<GraphCheck> {
    let wiring_path = context_dir.join(".graph").join("wiring.json");
    let Ok(bytes) = std::fs::read(&wiring_path) else {
        return Ok(GraphCheck {
            ok: false,
            missing: true,
            added: Vec::new(),
            removed: Vec::new(),
            changed: Vec::new(),
            stale: Vec::new(),
            pending: 0,
            pending_ids: Vec::new(),
            nodes: 0,
        });
    };
    let committed: Graph = serde_json::from_slice(&bytes).map_err(io::Error::other)?;

    // The last build's `--only-dir` whitelist rides in the fingerprint
    // (P1-70).
    let stamp = crate::extractor_stamp();
    let opts = BuildOptions {
        only_dirs: read_fingerprint(&fingerprint_path(context_dir, &stamp), &stamp)
            .and_then(|fp| fp.only_dirs)
            .unwrap_or_default(),
        no_reuse: false,
        // Nothing writes a file at all (P1-35).
        read_only: true,
    };
    let report = build_graph_cached_with(root, context_dir, &opts)?;
    let current: HashMap<&str, &str> = report
        .graph
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n.body_hash.as_str()))
        .collect();

    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();
    let mut stale = Vec::new();
    let mut pending_ids = Vec::new();

    // Sieve keys the committed nodes by id in a `Map`: a
    // repeated id counts once, and the last node with that id wins.
    let committed_by_id: HashMap<&str, &sieve_core::wiring::Node> =
        committed.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    for node in committed_by_id.values() {
        match current.get(node.id.as_str()) {
            None => removed.push(node.id.clone()),
            Some(hash) if *hash != node.body_hash => changed.push(node.id.clone()),
            Some(_) => {}
        }
        if node.summary_state == SummaryState::Stale {
            stale.push(node.id.clone());
        }
        if node.summary_state == SummaryState::Pending {
            pending_ids.push(node.id.clone());
        }
    }
    for id in current.keys() {
        if !committed_by_id.contains_key(id) {
            added.push(id.to_string());
        }
    }

    for arr in [
        &mut added,
        &mut removed,
        &mut changed,
        &mut stale,
        &mut pending_ids,
    ] {
        arr.sort();
    }

    let pending = pending_ids.len();
    let ok = added.is_empty() && removed.is_empty() && changed.is_empty() && stale.is_empty();

    Ok(GraphCheck {
        ok,
        missing: false,
        added,
        removed,
        changed,
        stale,
        pending,
        pending_ids,
        nodes: committed_by_id.len(),
    })
}

/// Checks whether the committed deep-layer manifest still matches the
/// code.
///
/// Sieve never writes `manifest.json` or a concept node body (Phase 1
/// scope, the `deep-tier.md` note rule R3); this
/// reads only what a meaning-tier build already left on disk. A missing
/// manifest reports `missing: true` with every array empty, matching
/// the report for a Tier-1-only build. With a manifest present, this
/// re-hashes the recorded source files and re-reads the concept nodes, and
/// diffs both against the manifest, mirroring.
pub fn check_context(root: &Path, context_dir: &Path) -> ContextCheck {
    let Some(manifest) = Manifest::read(context_dir) else {
        return ContextCheck {
            ok: false,
            missing: true,
            content_drift: Vec::new(),
            removed: Vec::new(),
            coverage: Vec::new(),
            index_drift: Vec::new(),
        };
    };

    // ponytail: no `--only-dir` whitelist applied here, matching
    // `probe_drift`'s own ceiling note in `fingerprint.rs`. Sieve stores
    // `onlyDirs` on `Fingerprint`, but `check` reads no fingerprint today.
    // Add the filter here (and to `probe_drift`) together, once a caller
    // needs `--only-dir` scoping.
    let context_rel = context_dir
        .strip_prefix(root)
        .ok()
        .map(|p| p.to_string_lossy().replace('\\', "/"));

    let mut current: BTreeMap<String, String> = BTreeMap::new();
    if let Ok(files) = walk_repo(root) {
        for rel in files {
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            let lower = rel_str.to_lowercase();
            if !CODE_EXTENSIONS.iter().any(|ext| lower.ends_with(ext)) {
                continue;
            }
            if let Some(ctx) = &context_rel {
                if rel_str.starts_with(ctx.as_str()) {
                    continue;
                }
            }
            let Ok(bytes) = std::fs::read(root.join(&rel_str)) else {
                continue;
            };
            let Some(text) = decode_source(&bytes) else {
                continue; // unsupported encoding (UTF-16BE) → treat as removed below
            };
            current.insert(rel_str, hash_source(text.as_bytes()));
        }
    }

    let mut recorded: HashSet<&str> = HashSet::new();
    let mut removed = Vec::new();
    let mut content_drift = Vec::new();
    for file_ref in &manifest.files {
        recorded.insert(file_ref.path.as_str());
        match current.get(&file_ref.path) {
            None => removed.push(file_ref.path.clone()),
            Some(now) if Some(now.as_str()) != file_ref.hash.as_deref() => {
                content_drift.push(ContentDrift {
                    path: file_ref.path.clone(),
                    from: short_hash(file_ref.hash.as_deref().unwrap_or_default()),
                    to: short_hash(now),
                })
            }
            Some(_) => {}
        }
    }

    let coverage: Vec<String> = current
        .keys()
        .filter(|path| !recorded.contains(path.as_str()))
        .cloned()
        .collect();

    let manifest_nodes: HashMap<&str, &sieve_core::concept::ManifestNode> = manifest
        .nodes
        .iter()
        .map(|n| (n.slug.as_str(), n))
        .collect();
    let on_disk = read_nodes(context_dir);
    let mut seen: HashSet<String> = HashSet::new();
    let mut index_drift = Vec::new();
    for node in &on_disk {
        seen.insert(node.slug.clone());
        match manifest_nodes.get(node.slug.as_str()) {
            None => index_drift.push(format!("{}: node file not in manifest", node.slug)),
            Some(m) if m.sources_digest != node.sources_digest => index_drift.push(format!(
                "{}: frontmatter digest \u{2260} manifest",
                node.slug
            )),
            Some(_) => {}
        }
    }
    for node in &manifest.nodes {
        if !seen.contains(&node.slug) {
            index_drift.push(format!("{}: in manifest but node file missing", node.slug));
        }
    }

    let ok = content_drift.is_empty()
        && removed.is_empty()
        && coverage.is_empty()
        && index_drift.is_empty();

    ContextCheck {
        ok,
        missing: false,
        content_drift,
        removed,
        coverage,
        index_drift,
    }
}

/// The first 8 hex characters of a hash.
fn short_hash(hash: &str) -> String {
    hash.chars().take(8).collect()
}

/// Renders a [`GraphCheck`] as a human-readable report.
/// `formatGraphCheckReport`.
pub fn format_graph_check_report(g: &GraphCheck) -> String {
    if g.missing {
        let name = sieve_core::product().name;
        return format!(
            "graph check: NO GRAPH\n\nNo {name}/.graph/wiring.json found. Run `{name} build` first."
        );
    }
    if g.ok {
        let note = if g.pending > 0 {
            format!(" ({})", pending_note(g))
        } else {
            String::new()
        };
        return format!("graph check: OK — the wiring graph is in sync with the code.{note}");
    }

    let mut lines = vec!["graph check: STALE".to_string(), String::new()];
    if !g.changed.is_empty() {
        lines.push(format!("changed ({}):", g.changed.len()));
        lines.extend(g.changed.iter().map(|id| format!("  ~ {id}")));
    }
    if !g.added.is_empty() {
        lines.push(format!("added ({}):", g.added.len()));
        lines.extend(g.added.iter().map(|id| format!("  + {id}")));
    }
    if !g.removed.is_empty() {
        lines.push(format!("removed ({}):", g.removed.len()));
        lines.extend(g.removed.iter().map(|id| format!("  - {id}")));
    }
    if !g.stale.is_empty() {
        lines.push(format!("stale summaries ({}):", g.stale.len()));
        lines.extend(g.stale.iter().map(|id| format!("  ! {id}")));
    }
    lines.push(String::new());
    let structural = !g.changed.is_empty() || !g.added.is_empty() || !g.removed.is_empty();
    let name = sieve_core::product().name;
    if structural {
        lines.push(format!(
            "Run `{name} build` to rebuild the structure, then commit {name}/."
        ));
    }
    if !g.stale.is_empty() {
        lines.push(format!("Run `{name} build` to refresh stale summaries."));
    }
    lines.join("\n")
}

/// The pending-coverage note the OK report appends in parens, matching
fn pending_note(g: &GraphCheck) -> String {
    let pct = if g.nodes > 0 {
        ((g.nodes - g.pending) as f64 / g.nodes as f64 * 100.0).round() as u64
    } else {
        0
    };
    let sample = &g.pending_ids[..g.pending_ids.len().min(PENDING_SAMPLE)];
    let more = if g.pending_ids.len() > PENDING_SAMPLE {
        format!(", … +{} more", g.pending_ids.len() - PENDING_SAMPLE)
    } else {
        String::new()
    };
    let named = if sample.is_empty() {
        String::new()
    } else {
        format!(": {}{more}", sample.join(", "))
    };
    format!(
        "meaning tier {pct}% complete — {} of {} node(s) pending{named}.",
        g.pending, g.nodes
    )
}

/// Renders a [`ContextCheck`] as a human-readable report.
/// `formatCheckReport`.
pub fn format_check_report(c: &ContextCheck) -> String {
    let name = sieve_core::product().name;
    if c.missing {
        return format!(
            "{name} check: NO GRAPH\n\nNo {name}/manifest.json found. Run `{name} build` first."
        );
    }
    if c.ok {
        return format!("{name} check: OK — the graph is in sync with the code.");
    }

    let mut lines = vec![format!("{name} check: STALE"), String::new()];
    if !c.content_drift.is_empty() {
        lines.push(format!("changed ({}):", c.content_drift.len()));
        lines.extend(
            c.content_drift
                .iter()
                .map(|d| format!("  ~ {}  ({} → {})", d.path, d.from, d.to)),
        );
    }
    if !c.removed.is_empty() {
        lines.push(format!("removed ({}):", c.removed.len()));
        lines.extend(c.removed.iter().map(|p| format!("  - {p}")));
    }
    if !c.coverage.is_empty() {
        lines.push(format!("not in graph ({}):", c.coverage.len()));
        lines.extend(c.coverage.iter().map(|p| format!("  + {p}")));
    }
    if !c.index_drift.is_empty() {
        lines.push(format!("index mismatch ({}):", c.index_drift.len()));
        lines.extend(c.index_drift.iter().map(|s| format!("  ! {s}")));
    }
    lines.push(String::new());
    lines.push(format!(
        "Run `{name} build` to regenerate, then commit {name}/."
    ));
    lines.join("\n")
}

/// The workspace `check` text (P1-59, P4-44): one line per child, then the
/// coverage note, ending in a newline. `loaded` holds each built child's dir
/// name and repo root. `missing` holds the unbuilt names. Returns the text and
/// whether every built child is in sync. The CLI and the MCP server both print
/// this text.
pub fn federated_check_text(
    loaded: &[(&str, &Path)],
    missing: &[String],
    coverage: &str,
) -> io::Result<(String, bool)> {
    let name = sieve_core::product().name;
    let context_name = sieve_core::product().context_dir_name();
    let mut lines = vec![
        format!("workspace check — {} repo(s)", loaded.len() + missing.len()),
        String::new(),
    ];
    let mut ok = true;
    for (child, root) in loaded {
        let g = check_graph(root, &root.join(context_name))?;
        if g.ok {
            lines.push(format!("{child}/: OK"));
            continue;
        }
        ok = false;
        let bits: Vec<String> = [
            (g.added.len(), "added"),
            (g.removed.len(), "removed"),
            (g.changed.len(), "changed"),
            (g.stale.len(), "stale"),
        ]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, label)| format!("{count} {label}"))
        .collect();
        lines.push(format!("{child}/: STALE ({})", bits.join(", ")));
    }
    for child in missing {
        lines.push(format!("{child}/: not built (run {name} build)"));
    }
    if !coverage.is_empty() {
        lines.push(String::new());
        lines.push(coverage.to_string());
    }
    Ok((format!("{}\n", lines.join("\n")), ok))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build::build_graph_cached;

    #[test]
    fn federated_check_text_lists_an_unbuilt_child_and_the_coverage_note() {
        let (text, ok) =
            federated_check_text(&[], &["beta".to_string()], "1 of 1 covered").expect("text");
        assert!(ok);
        assert!(text.starts_with("workspace check — 1 repo(s)\n\nbeta/: not built (run "));
        assert!(text.ends_with("\n\n1 of 1 covered\n"));
    }

    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempDir {
        path: std::path::PathBuf,
    }

    impl TempDir {
        fn new(label: &str) -> Self {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let pid = std::process::id();
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .expect("system clock before epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!("sieve-check-{label}-{pid}-{n}-{nanos}"));
            fs::create_dir_all(&path).expect("create temp dir");
            TempDir { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn check_graph_reports_missing_when_wiring_json_is_absent() {
        let dir = TempDir::new("missing");
        let g =
            check_graph(&dir.path, &dir.path.join("sieve")).expect("check_graph should not error");
        assert!(g.missing);
        assert!(!g.ok);
        assert!(g.added.is_empty());
        assert_eq!(g.nodes, 0);
    }

    #[test]
    fn check_context_reports_missing_when_manifest_json_is_absent() {
        let dir = TempDir::new("ctx-missing");
        let c = check_context(&dir.path, &dir.path.join("sieve"));
        assert!(c.missing);
        assert!(!c.ok);
        assert!(c.content_drift.is_empty());
    }

    /// Writes `<context_dir>/manifest.json` with one recorded file and one
    /// concept node roster entry.
    fn write_manifest(
        context_dir: &Path,
        file_path: &str,
        file_hash: &str,
        slug: &str,
        digest: &str,
    ) {
        fs::create_dir_all(context_dir).expect("create context dir");
        let manifest = format!(
            r#"{{
  "version": 1,
  "model": "openai:gpt-4o-mini",
  "repoDigest": "deadbeef",
  "files": [ {{ "path": "{file_path}", "hash": "{file_hash}" }} ],
  "nodes": [ {{ "slug": "{slug}", "name": "Widget", "type": "concept", "sources": ["{file_path}"], "sourcesDigest": "{digest}" }} ]
}}"#
        );
        fs::write(context_dir.join("manifest.json"), manifest).expect("write manifest");
    }

    /// Writes `<context_dir>/<slug>.md`, a minimal concept node with the
    /// given `sources_digest` in its frontmatter.
    fn write_concept_node(context_dir: &Path, slug: &str, file_path: &str, digest: &str) {
        let text = format!(
            "---\nname: Widget\nslug: {slug}\ntype: concept\nsources:\n  - path: {file_path}\n    hash: aaaa\nsources_digest: {digest}\n---\nbody\n"
        );
        fs::write(context_dir.join(format!("{slug}.md")), text).expect("write concept node");
    }

    #[test]
    fn check_context_reports_ok_when_the_manifest_matches_the_code() {
        let dir = TempDir::new("ctx-ok");
        fs::write(dir.path.join("a.ts"), b"const a = 1;").expect("write source");
        let hash = hash_source(b"const a = 1;");
        let context_dir = dir.path.join("sieve");
        write_manifest(&context_dir, "a.ts", &hash, "widget", "bbbb");
        write_concept_node(&context_dir, "widget", "a.ts", "bbbb");

        let c = check_context(&dir.path, &context_dir);
        assert!(c.ok, "{c:?}");
        assert!(!c.missing);
        assert!(c.content_drift.is_empty());
        assert!(c.removed.is_empty());
        assert!(c.coverage.is_empty());
        assert!(c.index_drift.is_empty());
    }

    #[test]
    fn check_context_reports_content_drift_when_a_recorded_file_changed() {
        let dir = TempDir::new("ctx-drift");
        fs::write(dir.path.join("a.ts"), b"const a = 2;").expect("write source");
        let old_hash = hash_source(b"const a = 1;");
        let context_dir = dir.path.join("sieve");
        write_manifest(&context_dir, "a.ts", &old_hash, "widget", "bbbb");
        write_concept_node(&context_dir, "widget", "a.ts", "bbbb");

        let c = check_context(&dir.path, &context_dir);
        assert!(!c.ok);
        assert_eq!(c.content_drift.len(), 1);
        assert_eq!(c.content_drift[0].path, "a.ts");
        assert_eq!(c.content_drift[0].from, &old_hash[..8]);
        assert_eq!(c.content_drift[0].to, &hash_source(b"const a = 2;")[..8]);
    }

    #[test]
    fn check_context_reports_removed_when_a_recorded_file_is_gone() {
        let dir = TempDir::new("ctx-removed");
        let context_dir = dir.path.join("sieve");
        write_manifest(&context_dir, "gone.ts", "deadbeef", "widget", "bbbb");
        write_concept_node(&context_dir, "widget", "gone.ts", "bbbb");

        let c = check_context(&dir.path, &context_dir);
        assert!(!c.ok);
        assert_eq!(c.removed, vec!["gone.ts".to_string()]);
    }

    #[test]
    fn check_context_reports_coverage_when_a_new_file_is_not_in_the_manifest() {
        let dir = TempDir::new("ctx-coverage");
        fs::write(dir.path.join("a.ts"), b"const a = 1;").expect("write recorded source");
        fs::write(dir.path.join("b.ts"), b"const b = 2;").expect("write new source");
        let hash = hash_source(b"const a = 1;");
        let context_dir = dir.path.join("sieve");
        write_manifest(&context_dir, "a.ts", &hash, "widget", "bbbb");
        write_concept_node(&context_dir, "widget", "a.ts", "bbbb");

        let c = check_context(&dir.path, &context_dir);
        assert!(!c.ok);
        assert_eq!(c.coverage, vec!["b.ts".to_string()]);
    }

    #[test]
    fn check_context_reports_index_drift_for_each_of_the_three_cases() {
        let dir = TempDir::new("ctx-index");
        fs::write(dir.path.join("a.ts"), b"const a = 1;").expect("write source a");
        fs::write(dir.path.join("b.ts"), b"const b = 2;").expect("write source b");
        let hash_a = hash_source(b"const a = 1;");
        let hash_b = hash_source(b"const b = 2;");
        let context_dir = dir.path.join("sieve");
        fs::create_dir_all(&context_dir).expect("create context dir");
        // The roster names both "widget" (digest will disagree with the
        // file on disk) and "orphan" (whose node file never gets written).
        let manifest = format!(
            r#"{{
  "version": 1,
  "model": "openai:gpt-4o-mini",
  "repoDigest": "deadbeef",
  "files": [ {{ "path": "a.ts", "hash": "{hash_a}" }}, {{ "path": "b.ts", "hash": "{hash_b}" }} ],
  "nodes": [
    {{ "slug": "widget", "name": "Widget", "type": "concept", "sources": ["a.ts"], "sourcesDigest": "committed-digest" }},
    {{ "slug": "orphan", "name": "Orphan", "type": "concept", "sources": ["b.ts"], "sourcesDigest": "cccc" }}
  ]
}}"#
        );
        fs::write(context_dir.join("manifest.json"), manifest).expect("write manifest");
        // "widget"'s node file exists but its frontmatter digest disagrees
        // with the manifest roster (a hand edit).
        write_concept_node(&context_dir, "widget", "a.ts", "hand-edited-digest");
        // "extra" has a node file with no roster entry at all.
        write_concept_node(&context_dir, "extra", "b.ts", "dddd");

        let c = check_context(&dir.path, &context_dir);
        assert!(!c.ok);
        assert!(
            c.index_drift
                .contains(&"widget: frontmatter digest \u{2260} manifest".to_string()),
            "{:?}",
            c.index_drift
        );
        assert!(
            c.index_drift
                .contains(&"extra: node file not in manifest".to_string()),
            "{:?}",
            c.index_drift
        );
        assert!(
            c.index_drift
                .contains(&"orphan: in manifest but node file missing".to_string()),
            "{:?}",
            c.index_drift
        );
    }

    #[test]
    fn check_graph_reports_ok_when_the_wiring_graph_matches_the_code() {
        let dir = TempDir::new("ok");
        fs::write(dir.path.join("a.ts"), "const x = 1;\n").expect("write file");
        let context_dir = dir.path.join("sieve");

        let report = build_graph_cached(&dir.path, &context_dir).expect("build succeeds");
        sieve_core::write_graph(
            &report.graph,
            &context_dir.join(".graph").join("wiring.json"),
        )
        .expect("write succeeds");

        let g = check_graph(&dir.path, &context_dir).expect("check_graph should not error");
        assert!(g.ok);
        assert!(!g.missing);
        assert!(g.added.is_empty());
        assert!(g.removed.is_empty());
        assert!(g.changed.is_empty());
        assert_eq!(g.nodes, 1);
    }

    #[test]
    fn check_graph_reports_stale_when_a_committed_node_changed() {
        let dir = TempDir::new("stale");
        fs::write(dir.path.join("a.ts"), "const x = 1;\n").expect("write file");
        let context_dir = dir.path.join("sieve");

        let report = build_graph_cached(&dir.path, &context_dir).expect("build succeeds");
        sieve_core::write_graph(
            &report.graph,
            &context_dir.join(".graph").join("wiring.json"),
        )
        .expect("write succeeds");

        fs::write(dir.path.join("a.ts"), "const x = 2;\n").expect("edit file");

        let g = check_graph(&dir.path, &context_dir).expect("check_graph should not error");
        assert!(!g.ok);
        assert!(g.changed.contains(&"a.ts".to_string()));
        let report_text = format_graph_check_report(&g);
        assert!(report_text.starts_with("graph check: STALE"));
        assert!(report_text.contains("  ~ a.ts"));
    }

    #[test]
    fn format_graph_check_report_ok_with_pending_caps_the_sample_at_eight() {
        let g = GraphCheck {
            ok: true,
            missing: false,
            added: Vec::new(),
            removed: Vec::new(),
            changed: Vec::new(),
            stale: Vec::new(),
            pending: 10,
            pending_ids: (0..10).map(|i| format!("f{i}.ts")).collect(),
            nodes: 10,
        };
        let text = format_graph_check_report(&g);
        assert!(text.contains("meaning tier 0% complete"));
        assert!(text.contains("f0.ts, f1.ts, f2.ts, f3.ts, f4.ts, f5.ts, f6.ts, f7.ts"));
        assert!(text.contains(", … +2 more"));
        assert!(!text.contains("f8.ts"));
    }

    #[test]
    fn check_context_hashes_a_utf16le_bom_file_like_its_utf8_text() {
        let dir = TempDir::new("ctx-utf16le");
        let text = "const a = 1;";
        let mut bytes = vec![0xff, 0xfe];
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        fs::write(dir.path.join("a.ts"), &bytes).expect("write utf16le source");
        let expected_hash = hash_source(text.as_bytes());
        let context_dir = dir.path.join("sieve");
        write_manifest(&context_dir, "a.ts", &expected_hash, "widget", "bbbb");
        write_concept_node(&context_dir, "widget", "a.ts", "bbbb");

        let c = check_context(&dir.path, &context_dir);
        assert!(c.ok, "{c:?}");
    }

    #[test]
    fn check_context_excludes_a_utf16be_bom_file_from_current() {
        let dir = TempDir::new("ctx-utf16be");
        let bytes = vec![0xfe, 0xff, 0x00, b'a'];
        fs::write(dir.path.join("a.ts"), &bytes).expect("write utf16be source");
        let context_dir = dir.path.join("sieve");
        // Nothing recorded for "a.ts": with the file excluded from `current`,
        // it must not show up as `coverage` (a new, un-ingested file) either.
        fs::create_dir_all(&context_dir).expect("create context dir");
        fs::write(
            context_dir.join("manifest.json"),
            r#"{"version":1,"model":"m","repoDigest":"d","files":[],"nodes":[]}"#,
        )
        .expect("write empty manifest");

        let c = check_context(&dir.path, &context_dir);
        assert!(c.ok, "{c:?}");
        assert!(c.coverage.is_empty());
    }

    #[test]
    fn format_check_report_missing_matches_golden_text() {
        let c = ContextCheck {
            ok: false,
            missing: true,
            content_drift: Vec::new(),
            removed: Vec::new(),
            coverage: Vec::new(),
            index_drift: Vec::new(),
        };
        assert_eq!(
            format_check_report(&c),
            "sieve check: NO GRAPH\n\nNo sieve/manifest.json found. Run `sieve build` first."
        );
    }
}
