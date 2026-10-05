//! Multi-scope fusion: scope assignment, body-only suppression, file
//! collapse, and `combineComparableScopes` (`ask-ranking.md` section 8).
//!
//! The ranking pipeline collapses each scope to one row per file
//! (`collapseCandidates`), combines those file-level rows
//! across scopes (`combineComparableScopes`), then
//! materializes each file's queue with the LEADER at the combined,
//! normalized score and every other member at its own raw baseline
//! (`materializeFileQueue`), and finally round-robins
//! the queues in combined order.
//!
//! The top lock of the multi-scope path reads a SECOND combine, over every
//! pre-collapse candidate of the surviving scopes (`baselineFusion`). Its best
//! candidate, by score then `symbolTitle`, is the symbol the concept merge
//! compares against. `run_multi_scope` builds that candidate and its score map,
//! and `select::select_hits_with_concepts` applies the lock. This module
//! returns the combined file groups, already in the right order because
//! `combine_comparable_scopes` put them there.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};

use sieve_core::collate::collate;
use sieve_core::wiring::Scope;
use sieve_core::{Graph, Node};

use super::lexical::{has_term, Corpus, DocBag};
use super::pagerank;
use super::select::{build_candidates, rank_files_bounded, title_key, FileGroup, FusedTop};
use super::{AlsoMatched, ScopeMeta, PARTICIPATION_RATIO};

/// The nearest-prefix scope owner of `path` among `scopes`, with the
/// empty-prefix root as the fallback. Copied from `sieve-parse`'s
/// `scope_of`, so this crate adds no dependency on `sieve-parse`. Public
/// so the prompt hook's `lastFileScopeHint` resolves a scope the same way
/// `ask --in` does.
pub fn scope_of(path: &str, scopes: &[Scope]) -> String {
    let mut best: Option<&str> = None;
    for s in scopes {
        if s.prefix.is_empty() {
            continue;
        }
        let under_prefix = path == s.prefix || path.starts_with(&format!("{}/", s.prefix));
        if under_prefix && best.is_none_or(|b| s.prefix.len() > b.len()) {
            best = Some(s.prefix.as_str());
        }
    }
    best.map(str::to_string).unwrap_or_default()
}

/// Groups `active_nodes` by `scope_of`, in ascending byte order of the
/// scope prefix. `BTreeMap<String, _>` orders its keys by `Ord`, the same
/// plain byte order `[...byScope.keys].sort` uses at
/// (never `localeCompare`), so this iterates in the exact order that
/// feeds `rankScopesAndFuse`.
pub(crate) fn group_by_scope<'a>(
    active_nodes: &[&'a Node],
    scopes: &[Scope],
) -> BTreeMap<String, Vec<&'a Node>> {
    let mut out: BTreeMap<String, Vec<&'a Node>> = BTreeMap::new();
    for &node in active_nodes {
        out.entry(scope_of(&node.path, scopes))
            .or_default()
            .push(node);
    }
    out
}

fn has_identifier_match(nodes: &[&Node], docs: &HashMap<&str, DocBag>, terms: &[String]) -> bool {
    nodes.iter().any(|n| {
        docs.get(n.id.as_str()).is_some_and(|d| {
            terms
                .iter()
                .any(|t| has_term(&d.name, t) || has_term(&d.path, t))
        })
    })
}

/// Walks `nodes`' own subgraph only, seeded from `seeds` (a scope's own
/// lexical scores): the adjacency drops every edge that touches a node
/// outside `nodes`, so a cross-scope call edge cannot carry PageRank mass
/// between scopes (`preparePageRankPartitions`). Each
/// scope gets its own walk; there is no graph-wide `pr` in the multi-scope
/// path.
fn scope_pagerank(
    graph: &Graph,
    nodes: &[&Node],
    seeds: &HashMap<String, f64>,
    graph_rank: bool,
) -> HashMap<String, f64> {
    if !graph_rank || seeds.is_empty() {
        return HashMap::new();
    }
    let node_ids: HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    let adjacency = pagerank::build_adjacency(graph, &node_ids);
    pagerank::run(nodes, seeds, &adjacency)
}

/// One scored row at the granularity `combineComparableScopes` compares:
/// a file's representative node id, the scope it was ranked in, and its
/// (already shared-denominator-normalized) file score. The
/// `collapseCandidates` step hands the fusion the representative's real node id
/// and every `localeCompare` there reads that id.
struct ScopedRow {
    id: String,
    scope: String,
    score: f64,
}

/// Score-desc, id-collate ordering ('s `byScore`;
/// every `localeCompare` site here uses `collate`, per the parity rule).
fn by_score(a: &ScopedRow, b: &ScopedRow) -> Ordering {
    b.score
        .partial_cmp(&a.score)
        .unwrap_or(Ordering::Equal)
        .then_with(|| collate(&a.id, &b.id))
}

/// Ports `combineComparableScopes`: groups `rows` by
/// scope, drops non-scoring rows, passes a lone scope through unchanged,
/// else gates every scope on `PARTICIPATION_RATIO` of the best scope's top
/// score and combines the survivors, keeping each id's best-scoring scope
/// and normalizing by the combined max.
///
/// Returns `(ranked, federated, also_matched)`: `ranked` holds only the
/// federated rows, in the final order (score desc, scope collate, id collate);
/// `federated` is the surviving scope list, best score first;
/// `also_matched` is the gated-out scopes with their best row's id.
fn combine_comparable_scopes(
    rows: Vec<ScopedRow>,
) -> (Vec<ScopedRow>, Vec<String>, Vec<AlsoMatched>) {
    // Insertion-ordered scope grouping (rule book 9.1): a `Vec` plus a
    // linear scan, since a query fuses a handful of scopes at most.
    let mut by_scope: Vec<(String, Vec<ScopedRow>)> = Vec::new();
    for row in rows {
        if row.score <= 0.0 {
            continue;
        }
        match by_scope.iter_mut().find(|(s, _)| *s == row.scope) {
            Some((_, list)) => list.push(row),
            None => by_scope.push((row.scope.clone(), vec![row])),
        }
    }
    for (_, list) in &mut by_scope {
        list.sort_by(by_score);
    }

    if by_scope.is_empty() {
        return (Vec::new(), Vec::new(), Vec::new());
    }
    if by_scope.len() == 1 {
        let (scope, list) = by_scope.into_iter().next().expect("checked len == 1");
        let ranked: Vec<ScopedRow> = list
            .into_iter()
            .map(|r| ScopedRow {
                id: r.id,
                scope: scope.clone(),
                score: r.score,
            })
            .collect();
        return (ranked, vec![scope], Vec::new());
    }

    // Each scope orders by its own best row: score desc, then the scope
    // name (`localeCompare`).
    let mut scopes_by_best = by_scope;
    scopes_by_best.sort_by(|a, b| {
        b.1[0]
            .score
            .partial_cmp(&a.1[0].score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| collate(&a.0, &b.0))
    });
    let gate = PARTICIPATION_RATIO * scopes_by_best[0].1[0].score;
    let mut federated: Vec<String> = Vec::new();
    let mut also_matched: Vec<AlsoMatched> = Vec::new();
    for (scope, list) in &scopes_by_best {
        if list[0].score >= gate {
            federated.push(scope.clone());
        } else {
            also_matched.push(AlsoMatched {
                scope: scope.clone(),
                best_id: list[0].id.clone(),
            });
        }
    }

    // An id can only belong to one scope's file set (a file has one
    // scope), but keep the defensive max-pick so a duplicate cannot
    // double-count or flip its attribution.
    let mut acc: Vec<(String, String, f64)> = Vec::new();
    for (scope, list) in &scopes_by_best {
        if !federated.contains(scope) {
            continue;
        }
        for row in list {
            match acc.iter_mut().find(|(id, _, _)| *id == row.id) {
                Some((_, s, score)) if row.score > *score => {
                    *s = scope.clone();
                    *score = row.score;
                }
                Some(_) => {}
                None => acc.push((row.id.clone(), scope.clone(), row.score)),
            }
        }
    }

    let max = acc.iter().map(|(_, _, s)| *s).fold(0.0_f64, f64::max);
    let mut ranked: Vec<ScopedRow> = acc
        .into_iter()
        .map(|(id, scope, score)| ScopedRow {
            id,
            scope,
            score: if max > 0.0 { score / max } else { 0.0 },
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| collate(&a.scope, &b.scope))
            .then_with(|| collate(&a.id, &b.id))
    });
    (ranked, federated, also_matched)
}

/// The multi-scope branch's file groups, ready for the concept merge.
pub(crate) struct MultiScope<'a> {
    /// One group per file. When `fused` is set, the order and every score
    /// are the combined, normalized ones.
    pub groups: Vec<FileGroup<'a>>,
    /// Scope label per file path, for the `[scope/]` hit label.
    pub file_scope: HashMap<String, String>,
    /// The fused top candidate and the candidate score map, when `groups`
    /// carry fused scores. `None` when one scope ranked on its own.
    pub fused: Option<FusedTop<'a>>,
    pub scope_meta: Option<ScopeMeta>,
    /// Every baseline candidate with its baseline score, in the order
    /// `baselineFusion.ranked` lists them: the input to the
    /// ranking metadata's baseline list.
    pub baseline_rows: Vec<(&'a Node, f64)>,
}

/// Runs the multi-scope branch (section 8): scores every scope against
/// the shared, repo-wide `lex` map, walks each scope's own subgraph
/// separately, suppresses a body-only-match scope into `also_matched`,
/// collapses each surviving scope to one row per file
/// (`rank_files_bounded`, the same `collapseCandidates` the single-scope
/// path uses), and combines the file rows across scopes
/// (`combine_comparable_scopes`). The caller merges the concept hits
/// and round-robins the groups once.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_multi_scope<'a>(
    by_scope: &BTreeMap<String, Vec<&'a Node>>,
    docs: &HashMap<&str, DocBag>,
    corpus: &Corpus,
    terms: &[String],
    query_wants_tests: bool,
    lex: &HashMap<String, f64>,
    graph: &Graph,
    graph_rank: bool,
) -> MultiScope<'a> {
    if by_scope.len() <= 1 {
        // One real scope: `combineComparableScopes` passes scores through
        // untouched, so the single-scope pipeline is byte-identical.
        let nodes: Vec<&Node> = by_scope.values().flatten().copied().collect();
        let pr = scope_pagerank(graph, &nodes, lex, graph_rank);
        let candidates = build_candidates(&nodes, lex, &pr, query_wants_tests, Some((docs, terms)));
        // Sieve still labels each hit with its scope on this path.
        let file_scope: HashMap<String, String> = by_scope
            .iter()
            .flat_map(|(scope, ns)| ns.iter().map(move |n| (n.path.clone(), scope.clone())))
            .collect();
        // One scope passes through `combineComparableScopes` sorted by
        // score desc, then id (`byScore`).
        let mut baseline_rows: Vec<(&'a Node, f64)> =
            candidates.iter().map(|c| (c.node, c.baseline)).collect();
        baseline_rows.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(Ordering::Equal)
                .then_with(|| collate(&a.0.id, &b.0.id))
        });
        return MultiScope {
            groups: rank_files_bounded(candidates, terms, corpus),
            file_scope,
            fused: None,
            scope_meta: None,
            baseline_rows,
        };
    }

    // Per-scope raw best (`meta` in): the best raw
    // lexical score and its id, byte-compared on a tie (`id < bestId`
    // stays a byte comparison, never `collate`). A scope with no positive
    // lexical score never enters `meta` and never appears in the output.
    let mut meta: Vec<(String, f64, String)> = Vec::new();
    for (scope, nodes) in by_scope {
        let mut max_lex = 0.0_f64;
        let mut best_id: Option<String> = None;
        for &n in nodes {
            let Some(&v) = lex.get(n.id.as_str()) else {
                continue;
            };
            if v > max_lex {
                max_lex = v;
                best_id = Some(n.id.clone());
            } else if v == max_lex {
                if let Some(cur) = &best_id {
                    if n.id.as_str() < cur.as_str() {
                        best_id = Some(n.id.clone());
                    }
                }
            }
        }
        if let Some(best_id) = best_id.filter(|_| max_lex > 0.0) {
            meta.push((scope.clone(), max_lex, best_id));
        }
    }

    // Body-only suppression: a scope whose docs never
    // matched a query term in a name or path, while some other scope did,
    // is reported in `also_matched` instead of ranked.
    let any_identifier_match = meta.iter().any(|(scope, _, _)| {
        by_scope
            .get(scope)
            .is_some_and(|nodes| has_identifier_match(nodes, docs, terms))
    });
    let mut survivors: Vec<String> = Vec::new();
    let mut also_matched: Vec<AlsoMatched> = Vec::new();
    for (scope, _, best_id) in &meta {
        let named = by_scope
            .get(scope)
            .is_some_and(|nodes| has_identifier_match(nodes, docs, terms));
        if any_identifier_match && !named {
            also_matched.push(AlsoMatched {
                scope: scope.clone(),
                best_id: best_id.clone(),
            });
        } else {
            survivors.push(scope.clone());
        }
    }

    // The shared normalization denominator (`globalMaxLex`) must come
    // from the surviving scopes only, or a suppressed scope could still
    // set the scale for everyone else. Trimming `lex`
    // to the surviving node ids makes `build_candidates`'s internal max
    // recompute exactly that. The graph score stays scope-local (each
    // scope walks only its own subgraph, `scope_pagerank`), so it needs
    // no such trim.
    let surviving_nodes: Vec<&Node> = survivors
        .iter()
        .flat_map(|s| by_scope.get(s).into_iter().flatten().copied())
        .collect();
    let trimmed_lex: HashMap<String, f64> = surviving_nodes
        .iter()
        .filter_map(|n| lex.get(n.id.as_str()).map(|&v| (n.id.clone(), v)))
        .collect();

    // Collapse every surviving scope to one row per file
    // (`collapseCandidates`): rank each scope's candidates into file groups,
    // keep the full group (for the merged round robin below) alongside a
    // `ScopedRow` at the representative's score.
    let mut groups_by_id: HashMap<String, (String, FileGroup<'a>)> = HashMap::new();
    let mut rows: Vec<ScopedRow> = Vec::new();
    // Every raw candidate, scope-tagged, at its own (pre-collapse)
    // baseline — the input to the SEPARATE candidate-granularity combine
    // below (`baselineFusion`).
    let mut candidate_rows: Vec<ScopedRow> = Vec::new();
    let mut node_by_id: HashMap<String, &'a Node> = HashMap::new();
    for scope in &survivors {
        let Some(nodes) = by_scope.get(scope) else {
            continue;
        };
        let scope_lex: HashMap<String, f64> = nodes
            .iter()
            .filter_map(|n| lex.get(n.id.as_str()).map(|&v| (n.id.clone(), v)))
            .collect();
        let scope_pr = scope_pagerank(graph, nodes, &scope_lex, graph_rank);
        let candidates = build_candidates(
            nodes,
            &trimmed_lex,
            &scope_pr,
            query_wants_tests,
            Some((docs, terms)),
        );
        node_by_id.extend(candidates.iter().map(|c| (c.node.id.clone(), c.node)));
        candidate_rows.extend(candidates.iter().map(|c| ScopedRow {
            id: c.node.id.clone(),
            scope: scope.clone(),
            score: c.baseline,
        }));
        let file_groups = rank_files_bounded(candidates, terms, corpus);
        for group in file_groups {
            let rep_node = group.queue[0].0;
            let rep_id = rep_node.id.clone();
            rows.push(ScopedRow {
                id: rep_id.clone(),
                scope: scope.clone(),
                score: group.score,
            });
            groups_by_id.insert(rep_id, (scope.clone(), group));
        }
    }

    // Combine the file rows across scopes (`combineComparableScopes`),
    // then rebuild the file-group list in the combined order, with each
    // group's score overwritten by the combined score.
    let (ranked, federated, mut gate_also_matched) = combine_comparable_scopes(rows);
    also_matched.append(&mut gate_also_matched);

    // A SEPARATE combine at candidate granularity ('s
    // `baselineFusion`): every non-leader queue member's displayed score
    // is this normalized value, not its own raw baseline
    // (`materializeFileQueue`'s `baselineScoreById.get(member.id) ??
    // member.baselineScore`). The file-level combine
    // above and this one usually share the same global max (a
    // single-candidate file's own score sets both), but are not the same
    // computation, so they get their own pass.
    let (baseline_ranked, _, _) = combine_comparable_scopes(candidate_rows);
    // `baselineTopRanked`: the first row in fusion order
    // that a later row beats strictly on `baselineDisplayOrder`, which is
    // score desc, then `symbolTitle` asc.
    let mut best: Option<(&'a Node, f64)> = None;
    for r in &baseline_ranked {
        let Some(&node) = node_by_id.get(&r.id) else {
            continue;
        };
        let better = match best {
            None => true,
            Some((bn, bs)) => {
                r.score
                    .partial_cmp(&bs)
                    .unwrap_or(Ordering::Equal)
                    .reverse()
                    .then_with(|| collate(&title_key(node), &title_key(bn)))
                    == Ordering::Less
            }
        };
        if better {
            best = Some((node, r.score));
        }
    }
    let baseline_rows: Vec<(&'a Node, f64)> = baseline_ranked
        .iter()
        .filter_map(|r| node_by_id.get(&r.id).map(|&n| (n, r.score)))
        .collect();
    let baseline_by_id: HashMap<String, f64> = baseline_ranked
        .into_iter()
        .map(|r| (r.id, r.score))
        .collect();

    // Rebuild the file-group list in the combined order. Each group's
    // leader takes the combined, normalized score; every other queue
    // member takes the candidate-level combine score, falling back to its
    // own raw baseline (`materializeFileQueue`).
    let mut final_groups: Vec<FileGroup<'a>> = Vec::new();
    let mut file_scope: HashMap<String, String> = HashMap::new();
    for row in &ranked {
        if let Some((scope, mut group)) = groups_by_id.remove(&row.id) {
            group.score = row.score;
            for (i, member) in group.queue.iter_mut().enumerate() {
                if i == 0 {
                    member.1 = row.score;
                } else if let Some(&s) = baseline_by_id.get(&member.0.id) {
                    member.1 = s;
                }
            }
            file_scope.insert(group.file.clone(), scope);
            final_groups.push(group);
        }
    }

    let scope_meta = if federated.len() > 1 || !also_matched.is_empty() {
        Some(ScopeMeta {
            federated,
            also_matched,
        })
    } else {
        None
    };

    MultiScope {
        groups: final_groups,
        file_scope,
        fused: Some(FusedTop {
            best,
            scores: baseline_by_id,
        }),
        scope_meta,
        baseline_rows,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, scope: &str, score: f64) -> ScopedRow {
        ScopedRow {
            id: id.to_string(),
            scope: scope.to_string(),
            score,
        }
    }

    /// P3-12: on a tied score, the scopes order by scope name, then the rows by
    /// scope name, then by node id. The root-fallback scope `""` sorts first.
    #[test]
    fn test_p3_12_combine_comparable_scopes_breaks_a_tie_by_scope_then_id() {
        let rows = vec![
            row("b.ts#betaFive", "packages/beta", 1.5),
            row("a.ts#alphaTwo", "packages/alpha", 1.5),
            row("a.ts#alphaOne", "packages/alpha", 1.5),
            row("root.ts#zGamma", "", 1.5),
        ];
        let (ranked, federated, also_matched) = combine_comparable_scopes(rows);
        assert!(also_matched.is_empty());
        assert_eq!(federated, vec!["", "packages/alpha", "packages/beta"]);
        let ids: Vec<&str> = ranked.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "root.ts#zGamma",
                "a.ts#alphaOne",
                "a.ts#alphaTwo",
                "b.ts#betaFive"
            ]
        );
    }
}
