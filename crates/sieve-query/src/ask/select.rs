//! Candidate scoring, file grouping, the baseline top lock, and the
//! round-robin selection (`ask-ranking.md` section 5).

use std::cmp::Ordering;
use std::collections::HashMap;

use sieve_core::collate::collate;
use sieve_core::{Kind, Node};

use super::concept::ConceptDoc;
use super::lexical::{has_exact, has_term, Corpus, DocBag};
use super::{kind_word, test_factor, AskHit, RankGroup, Ranking};

fn clamp01(x: f64) -> f64 {
    x.clamp(0.0, 1.0)
}

fn parse_span_start(span: &str) -> u32 {
    span.strip_prefix('L')
        .and_then(|rest| rest.split_once('-'))
        .and_then(|(start, _)| start.parse().ok())
        .unwrap_or(0)
}

pub(crate) fn title_key(node: &Node) -> String {
    format!("{} \u{b7} {}", node.name, kind_word(node.kind))
}

/// One scored candidate: a node with its lexical and graph scores
/// (section 5.2).
#[derive(Debug, Clone)]
pub(crate) struct Cand<'a> {
    pub node: &'a Node,
    pub raw_lexical: f64,
    pub lexical: f64,
    pub graph: f64,
    pub rank_factor: f64,
    pub baseline: f64,
    pub span_start: u32,
    pub matched_terms: Vec<String>,
    /// The query terms the node NAME holds, with plural folding. Empty on a
    /// file node (`matchedStrongTerms`).
    pub matched_strong_terms: Vec<String>,
}

/// Orders two candidates by `baselineOrder`: baseline desc, then title asc. It
/// always sets `baselineTieKey`, and then returns the title compare even when
/// it is 0, so a same-score, same-title pair keeps its candidate order (the
/// sort is stable).
fn baseline_cmp(a: &Cand, b: &Cand) -> Ordering {
    b.baseline
        .partial_cmp(&a.baseline)
        .unwrap_or(Ordering::Equal)
        .then_with(|| collate(&title_key(a.node), &title_key(b.node)))
}

/// The idf share of the query terms `c` matched: the sum in matched-term
/// order, then clamped to `0..=1` (`coverage`).
fn coverage(c: &Cand, weights: &HashMap<String, f64>) -> f64 {
    let mut share = 0.0;
    for t in &c.matched_terms {
        let w = weights.get(t).copied().unwrap_or(0.0);
        if w.is_finite() && w > 0.0 {
            share += w;
        }
    }
    clamp01(share)
}

/// Orders two donor candidates for the anchor pick, as `lexicalAnchorOrder`:
/// raw lexical desc, idf coverage desc, span start asc, id asc.
///
/// A token-cost key is not part of this order: it would read `emittedTokens`, which no
/// caller sets, so that key is NaN and `||` skips it. Sieve has
/// no such key.
fn anchor_cmp(a: &Cand, b: &Cand, weights: &HashMap<String, f64>) -> Ordering {
    b.raw_lexical
        .partial_cmp(&a.raw_lexical)
        .unwrap_or(Ordering::Equal)
        .then_with(|| {
            coverage(b, weights)
                .partial_cmp(&coverage(a, weights))
                .unwrap_or(Ordering::Equal)
        })
        .then_with(|| a.span_start.cmp(&b.span_start))
        .then_with(|| collate(&a.node.id, &b.node.id))
}

/// Builds the candidate set: every node in `lex`, plus every rescued node
/// whose graph score clears `RESCUE_FLOOR` (section 5.2). Walks
/// `active_nodes` itself, so the candidate order is the node order the
/// caller already holds, not a `HashMap`'s iteration order (rule book
/// 9.1).
pub(crate) fn build_candidates<'a>(
    active_nodes: &[&'a Node],
    lex: &HashMap<String, f64>,
    pr: &HashMap<String, f64>,
    query_wants_tests: bool,
    docs: Option<(&HashMap<&str, DocBag>, &[String])>,
) -> Vec<Cand<'a>> {
    let lex_max = lex.values().cloned().fold(0.0_f64, f64::max);

    let mut out = Vec::new();
    for &node in active_nodes {
        let id = node.id.as_str();
        let in_lex = lex.contains_key(id);
        let rescued = !in_lex && pr.get(id).is_some_and(|&v| v >= super::RESCUE_FLOOR);
        if !in_lex && !rescued {
            continue;
        }
        let raw_lexical = lex.get(id).copied().unwrap_or(0.0);
        let lexical = if lex_max > 0.0 {
            raw_lexical / lex_max
        } else {
            0.0
        };
        let graph = pr.get(id).copied().unwrap_or(0.0);
        let rank_factor = test_factor(query_wants_tests, &node.path);
        let baseline = (lexical + super::GRAPH_WEIGHT * graph) * rank_factor;
        if baseline <= 0.0 {
            continue;
        }
        let matched_terms: Vec<String> = match docs {
            Some((docs, terms)) => match docs.get(id) {
                Some(d) => terms
                    .iter()
                    // Exact `.has`, no plural folding.
                    .filter(|t| {
                        has_exact(&d.name, t) || has_exact(&d.path, t) || has_exact(&d.body, t)
                    })
                    .cloned()
                    .collect(),
                None => Vec::new(),
            },
            None => Vec::new(),
        };
        let matched_strong_terms: Vec<String> = match docs {
            Some((docs, terms)) if node.kind != Kind::File => match docs.get(id) {
                Some(d) => terms
                    .iter()
                    .filter(|t| has_term(&d.name, t))
                    .cloned()
                    .collect(),
                None => Vec::new(),
            },
            _ => Vec::new(),
        };
        out.push(Cand {
            node,
            raw_lexical,
            lexical,
            graph,
            rank_factor,
            baseline,
            span_start: parse_span_start(&node.span),
            matched_terms,
            matched_strong_terms,
        });
    }
    out
}

fn query_weights(terms: &[String], corpus: &Corpus) -> HashMap<String, f64> {
    let raw: Vec<f64> = terms.iter().map(|t| corpus.idf_or_default(t)).collect();
    let sum: f64 = raw.iter().sum();
    if sum <= 0.0 {
        return terms.iter().map(|t| (t.clone(), 0.0)).collect();
    }
    terms
        .iter()
        .cloned()
        .zip(raw)
        .map(|(t, v)| (t, v / sum))
        .collect()
}

/// One file's ranked candidates, and the score its representative carries
/// (section 5.4).
pub(crate) struct FileGroup<'a> {
    pub file: String,
    pub score: f64,
    pub baseline_order: Vec<Cand<'a>>,
    pub queue: Vec<(&'a Node, f64)>,
    /// The idf share of the union of the donors' matched terms
    /// (`unionCoverage`). Zero with no donor.
    pub union_coverage: f64,
    /// The same share over the donors' NAME-only terms
    /// (`unionStrongCoverage`).
    pub union_strong_coverage: f64,
}

/// The group's representative candidate: the queue leader, which is the pooled
/// anchor when pooling wins `a.representative`).
fn representative<'g, 'a>(g: &'g FileGroup<'a>) -> Option<&'g Cand<'a>> {
    let lead = g.queue.first()?.0;
    g.baseline_order.iter().find(|c| c.node.id == lead.id)
}

/// Groups candidates by file, picks each file's representative (a
/// pooled anchor when it strictly beats the baseline, else the top
/// baseline candidate), and orders the files (section 5.4).
pub(crate) fn rank_files_bounded<'a>(
    candidates: Vec<Cand<'a>>,
    terms: &[String],
    corpus: &Corpus,
) -> Vec<FileGroup<'a>> {
    let weights = query_weights(terms, corpus);
    let mut by_file: Vec<(String, Vec<Cand<'a>>)> = Vec::new();
    for cand in candidates {
        let file = cand.node.path.clone();
        match by_file.iter_mut().find(|(f, _)| *f == file) {
            Some((_, members)) => members.push(cand),
            None => by_file.push((file, vec![cand])),
        }
    }

    let mut groups: Vec<FileGroup<'a>> = Vec::new();
    for (file, mut members) in by_file {
        members.sort_by(baseline_cmp);
        let default_rep_baseline = members.first().map_or(0.0, |c| c.baseline);

        // Donors in `lexicalAnchorOrder`. The anchor is the first donor, and
        // the union sums in this order.
        let mut donors: Vec<usize> = members
            .iter()
            .enumerate()
            .filter(|(_, c)| c.node.kind != Kind::File && c.raw_lexical > 0.0 && c.lexical > 0.0)
            .map(|(i, _)| i)
            .collect();
        donors.sort_by(|&a, &b| anchor_cmp(&members[a], &members[b], &weights));

        let mut pooled: Option<(usize, f64)> = None; // (anchor index, P_F)
        let mut union_coverage = 0.0;
        let mut union_strong_coverage = 0.0;
        if let Some(&anchor_idx) = donors.first() {
            let anchor = &members[anchor_idx];
            let mut union_terms: Vec<&str> = Vec::new();
            for &i in &donors {
                for t in &members[i].matched_terms {
                    if !union_terms.contains(&t.as_str()) {
                        union_terms.push(t.as_str());
                    }
                }
            }
            let mut u_sum = 0.0;
            for t in &union_terms {
                let w = weights.get(*t).copied().unwrap_or(0.0);
                if w.is_finite() && w > 0.0 {
                    u_sum += w;
                }
            }
            let u_f = clamp01(u_sum);
            union_coverage = u_f;
            let mut strong_terms: Vec<&str> = Vec::new();
            for &i in &donors {
                for t in &members[i].matched_strong_terms {
                    if !strong_terms.contains(&t.as_str()) {
                        strong_terms.push(t.as_str());
                    }
                }
            }
            let mut strong_sum = 0.0;
            for t in &strong_terms {
                let w = weights.get(*t).copied().unwrap_or(0.0);
                if w.is_finite() && w > 0.0 {
                    strong_sum += w;
                }
            }
            union_strong_coverage = clamp01(strong_sum);
            let a_f = coverage(anchor, &weights);
            let r_f = if u_f > 0.0 {
                clamp01(a_f * (u_f - a_f).max(0.0) / u_f)
            } else {
                0.0
            };
            let pooled_lexical = clamp01(anchor.lexical + (1.0 - clamp01(anchor.lexical)) * r_f);
            let p_f = clamp01(anchor.rank_factor) * (pooled_lexical + 0.5 * clamp01(anchor.graph));
            pooled = Some((anchor_idx, p_f));
        }

        let (rep_idx, score) = match pooled {
            Some((idx, p_f)) if p_f > default_rep_baseline => (idx, p_f),
            _ => (0, default_rep_baseline),
        };

        let rep_id = members[rep_idx].node.id.clone();
        let mut queue: Vec<(&Node, f64)> = vec![(members[rep_idx].node, score)];
        for c in &members {
            if c.node.id != rep_id {
                queue.push((c.node, c.baseline));
            }
        }

        groups.push(FileGroup {
            file,
            score,
            baseline_order: members,
            queue,
            union_coverage,
            union_strong_coverage,
        });
    }

    groups.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| match (representative(a), representative(b)) {
                (Some(ca), Some(cb)) => baseline_cmp(ca, cb),
                _ => Ordering::Equal,
            })
            .then_with(|| collate(&a.file, &b.file))
    });
    groups
}

/// Emits every queue's leader, then every queue's second member, and so
/// on, stopping the instant the result reaches `limit` (section 5.7).
pub(crate) fn round_robin_queues<T: Clone>(queues: &[Vec<T>], limit: usize) -> Vec<T> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    loop {
        let mut progressed = false;
        for q in queues {
            if depth < q.len() {
                out.push(q[depth].clone());
                progressed = true;
                if out.len() >= limit {
                    return out;
                }
            }
        }
        if !progressed {
            break;
        }
        depth += 1;
    }
    out
}

/// The ranking metadata a workspace parent reads from one child
/// (`ranking`). `groups` is the concept and file groups
/// sorted by leader score then title, cut to `limit`; each group's hits and
/// baseline hits are cut to `limit` too, as is `baseline`. `baseline_syms`
/// lists every baseline candidate in the order the hits are built; the
/// stable sort by score then title does the rest. `coverage_of` gives the
/// `(coverage, coverageStrong)` of one item (`matchedOf`, `matchedStrongOf`).
/// `scope_of_path` labels a symbol hit with its scope on the multi-scope
/// path.
pub(crate) fn build_ranking<'a>(
    file_groups: &[FileGroup<'a>],
    concepts: &[(&'a ConceptDoc, f64)],
    baseline_syms: &[(&'a Node, f64)],
    limit: usize,
    scope_of_path: &dyn Fn(&str) -> Option<String>,
    coverage_of: &dyn Fn(&QueueItem<'a>) -> (f64, f64),
) -> Ranking {
    let concept_key = |index: usize, doc: &ConceptDoc| format!("concept:{}:{index}", doc.slug);
    let hit_of = |item: &QueueItem<'a>| match item {
        QueueItem::Symbol(n, s) => super::to_hit(n, *s, scope_of_path(&n.path)),
        QueueItem::Concept(d, s) => super::concept::to_hit(d, *s),
    };
    let title_of = |item: &QueueItem<'a>| match item {
        QueueItem::Symbol(n, _) => title_key(n),
        QueueItem::Concept(d, _) => super::concept::title_of(d),
    };
    let score_of = |item: &QueueItem<'a>| match item {
        QueueItem::Symbol(_, s) | QueueItem::Concept(_, s) => *s,
    };

    // `baselineScored`: concepts first, then the baseline hits, one stable sort
    // by score desc then title.
    let mut baseline_scored: Vec<(String, QueueItem<'a>)> = Vec::new();
    for (i, &(doc, score)) in concepts.iter().enumerate() {
        baseline_scored.push((concept_key(i, doc), QueueItem::Concept(doc, score)));
    }
    for &(node, score) in baseline_syms {
        baseline_scored.push((
            format!("file:{}", node.path),
            QueueItem::Symbol(node, score),
        ));
    }
    baseline_scored.sort_by(|a, b| {
        score_of(&b.1)
            .partial_cmp(&score_of(&a.1))
            .unwrap_or(Ordering::Equal)
            .then_with(|| collate(&title_of(&a.1), &title_of(&b.1)))
    });

    // `unlockedGroups`: concept groups, then file groups, one stable sort by
    // leader score then leader title.
    let mut unlocked: Vec<(String, Vec<QueueItem<'a>>, f64, f64)> = Vec::new();
    for (i, &(doc, score)) in concepts.iter().enumerate() {
        let item = QueueItem::Concept(doc, score);
        let (coverage, strong) = coverage_of(&item);
        unlocked.push((concept_key(i, doc), vec![item], coverage, strong));
    }
    for g in file_groups {
        let items = g.queue.iter().map(|&(n, s)| QueueItem::Symbol(n, s));
        unlocked.push((
            format!("file:{}", g.file),
            items.collect(),
            g.union_coverage,
            g.union_strong_coverage,
        ));
    }
    unlocked.sort_by(|a, b| {
        score_of(&b.1[0])
            .partial_cmp(&score_of(&a.1[0]))
            .unwrap_or(Ordering::Equal)
            .then_with(|| collate(&title_of(&a.1[0]), &title_of(&b.1[0])))
    });

    let groups = unlocked
        .into_iter()
        .take(limit)
        .map(|(key, items, coverage, coverage_strong)| {
            let baseline_hits = baseline_scored
                .iter()
                .filter(|(k, _)| *k == key)
                .take(limit)
                .map(|(_, item)| hit_of(item))
                .collect();
            RankGroup {
                hits: items.iter().take(limit).map(&hit_of).collect(),
                baseline_hits,
                key,
                coverage,
                coverage_strong,
            }
        })
        .collect();
    let (baseline_coverage, baseline_coverage_strong) = baseline_scored
        .first()
        .map_or((0.0, 0.0), |(_, item)| coverage_of(item));
    let baseline = baseline_scored
        .iter()
        .take(limit)
        .map(|(key, item)| (key.clone(), hit_of(item)))
        .collect();
    Ranking {
        groups,
        baseline,
        baseline_coverage,
        baseline_coverage_strong,
    }
}

/// The multi-scope top lock's inputs: the best candidate of the
/// candidate-level combine and its normalized score, and every candidate's
/// normalized score (`baselineFusion`).
pub(crate) struct FusedTop<'a> {
    pub best: Option<(&'a Node, f64)>,
    pub scores: HashMap<String, f64>,
}

/// One round-robin queue's item: a symbol candidate, or the single item a
/// concept's own singleton queue ever holds (the `deep-tier.md` note section
/// 2.8: "one singleton group per concept, never a file group").
#[derive(Clone)]
pub(crate) enum QueueItem<'a> {
    Symbol(&'a Node, f64),
    Concept(&'a ConceptDoc, f64),
}

/// One emission group ready for the round robin: a file's queue, or one
/// concept's singleton queue.
struct EmitGroup<'a> {
    score: f64,
    title: String,
    queue: Vec<QueueItem<'a>>,
}

/// Applies the baseline top lock across symbol candidates and concept
/// scores together, then runs the round robin over the merged group list
/// (note section 2.8; section 5.5 to 5.7 for the symbol-only half).
///
/// With `fused` set (the multi-scope path), `file_groups` are already
/// ordered and scored on the combined scale. The best symbol is then
/// `fused.best`, the top of the candidate-level combine,
/// and the locked group scores every member from `fused.scores`.
///
/// A concept group never competes for the pooled-anchor or file-grouping
/// machinery: it is always a singleton, so only its own normalized score
/// enters the top-lock comparison and the final sort.
pub(crate) fn select_hits_with_concepts<'a>(
    mut file_groups: Vec<FileGroup<'a>>,
    concepts: &[(&'a ConceptDoc, f64)],
    limit: usize,
    fused: Option<&FusedTop<'a>>,
) -> (Vec<AskHit>, Option<super::Top<'a>>, bool) {
    let best_symbol: Option<(&'a Node, f64)> = if let Some(f) = fused {
        f.best
    } else {
        file_groups
            .iter()
            .flat_map(|g| g.baseline_order.iter())
            .min_by(|a, b| baseline_cmp(a, b))
            .map(|c| (c.node, c.baseline))
    };
    // `baselineScored[0]` among the concepts: score desc, then title asc,
    // and the first one on a full tie (a stable sort).
    let best_concept: Option<(&'a ConceptDoc, f64)> = concepts.iter().copied().min_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(Ordering::Equal)
            .then_with(|| {
                collate(
                    &super::concept::title_of(a.0),
                    &super::concept::title_of(b.0),
                )
            })
    });

    // A concept wins the top lock when its score beats the best symbol
    // baseline, or ties it and its title collates before the symbol's
    // title. An absent symbol always favors the concept. Sieve sorts
    // `[...conceptHits, ...baselineSymbolHits]` by score desc, then title
    // asc through `localeCompare`.
    let concept_wins = match (&best_symbol, &best_concept) {
        (Some((s_node, s_score)), Some((doc, cs))) => {
            match cs.partial_cmp(s_score).unwrap_or(Ordering::Equal) {
                Ordering::Greater => true,
                Ordering::Less => false,
                Ordering::Equal => {
                    collate(&super::concept::title_of(doc), &title_key(s_node)) == Ordering::Less
                }
            }
        }
        (None, Some(_)) => true,
        _ => false,
    };

    // `scored[0]`: the best unlocked leader or concept,
    // by score desc, then title asc, concepts first on a full tie. At
    // `-n 0` it is the top, whatever the lock chose.
    let mut scored_first: Option<(f64, String, QueueItem<'a>)> = None;
    let cands = concepts
        .iter()
        .map(|&(d, s)| (s, super::concept::title_of(d), QueueItem::Concept(d, s)))
        .chain(file_groups.iter().filter_map(|g| {
            g.queue
                .first()
                .map(|&(n, s)| (s, title_key(n), QueueItem::Symbol(n, s)))
        }));
    for (s, t, item) in cands {
        let better = match &scored_first {
            None => true,
            Some((bs, bt, _)) => {
                s.partial_cmp(bs)
                    .unwrap_or(Ordering::Equal)
                    .reverse()
                    .then_with(|| collate(&t, bt))
                    == Ordering::Less
            }
        };
        if better {
            scored_first = Some((s, t, item));
        }
    }

    let mut locked_slug: Option<&str> = None;
    let mut front: Option<EmitGroup<'a>> = None;
    if concept_wins {
        if let Some((doc, score)) = best_concept {
            locked_slug = Some(doc.slug.as_str());
            front = Some(EmitGroup {
                score,
                title: super::concept::title_of(doc),
                queue: vec![QueueItem::Concept(doc, score)],
            });
        }
    } else if let Some((best_node, best_score)) = best_symbol {
        if let Some(idx) = file_groups.iter().position(|g| g.file == best_node.path) {
            let group = file_groups.remove(idx);
            // The locked queue is `[top, ...file.queue minus top]`, every
            // member at its own baseline score (`baselineQueueHitsByGroup`).
            // The queue order puts the pooled anchor right after the top.
            let mut queue = vec![QueueItem::Symbol(best_node, best_score)];
            for &(n, stored) in &group.queue {
                // `sameHit`: kind, pointer and title.
                if super::pointer_of(n) == super::pointer_of(best_node)
                    && title_key(n) == title_key(best_node)
                {
                    continue;
                }
                let score = match fused {
                    Some(f) => f.scores.get(&n.id).copied().unwrap_or(stored),
                    None => group
                        .baseline_order
                        .iter()
                        .find(|c| c.node.id == n.id)
                        .map_or(stored, |c| c.baseline),
                };
                queue.push(QueueItem::Symbol(n, score));
            }
            front = Some(EmitGroup {
                score: best_score,
                title: title_key(best_node),
                queue,
            });
        }
    }

    // The remaining groups: concept groups before file groups in the
    // pre-sort order, so a stable sort keeps a concept ahead of a file on
    // an exact score-and-title tie.
    let mut rest: Vec<EmitGroup<'a>> = Vec::new();
    for &(doc, score) in concepts {
        if Some(doc.slug.as_str()) == locked_slug {
            continue;
        }
        rest.push(EmitGroup {
            score,
            title: super::concept::title_of(doc),
            queue: vec![QueueItem::Concept(doc, score)],
        });
    }
    for group in file_groups {
        // The sort title is the leader's (`a.hits[0].title`).
        let title = group
            .queue
            .first()
            .map(|(n, _)| title_key(n))
            .unwrap_or_default();
        let queue: Vec<QueueItem<'a>> = group
            .queue
            .into_iter()
            .map(|(n, s)| QueueItem::Symbol(n, s))
            .collect();
        rest.push(EmitGroup {
            score: group.score,
            title,
            queue,
        });
    }
    rest.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| collate(&a.title, &b.title))
    });

    let mut ordered: Vec<EmitGroup<'a>> = Vec::new();
    if let Some(front) = front {
        ordered.push(front);
    }
    ordered.extend(rest);

    let queues: Vec<Vec<QueueItem<'a>>> = ordered.into_iter().map(|g| g.queue).collect();
    let mut selected = round_robin_queues(&queues, limit);
    selected.truncate(limit);

    // `top = selected[0] ?? scored[0]`: with `-n 0` the
    // `scored[0]` item stands as the top, so `coverage` still reads from
    // it. `scored` is `scored.length > 0`, the mode's source;
    // a concept top scores too.
    let top_item = selected.first().or(scored_first.as_ref().map(|x| &x.2));
    let scored = top_item.is_some();
    let top = match top_item {
        Some(QueueItem::Symbol(n, _)) => Some(super::Top::Symbol(n)),
        Some(QueueItem::Concept(doc, _)) => Some(super::Top::Concept(doc)),
        None => None,
    };
    let hits = selected
        .into_iter()
        .map(|item| match item {
            QueueItem::Symbol(n, s) => super::to_hit(n, s, None),
            QueueItem::Concept(doc, s) => super::concept::to_hit(doc, s),
        })
        .collect();

    (hits, top, scored)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The symbol id, or `concept:<slug>`, of the top item.
    fn top_key<'a>(top: &'a Option<super::super::Top<'a>>) -> Option<String> {
        match top {
            Some(super::super::Top::Symbol(n)) => Some(n.id.clone()),
            Some(super::super::Top::Concept(doc)) => Some(format!("concept:{}", doc.slug)),
            None => None,
        }
    }
    use sieve_core::{Kind, Origin, SummaryState};

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

    fn cand<'a>(node: &'a Node, baseline: f64, raw_lexical: f64, lexical: f64) -> Cand<'a> {
        Cand {
            node,
            raw_lexical,
            lexical,
            graph: 0.0,
            rank_factor: 1.0,
            baseline,
            span_start: parse_span_start(&node.span),
            matched_terms: Vec::new(),
            matched_strong_terms: Vec::new(),
        }
    }

    #[test]
    fn round_robin_queues_caps_at_the_limit_across_three_queues() {
        let queues = vec![vec![1, 2], vec![3], vec![4, 5, 6]];
        assert_eq!(round_robin_queues(&queues, 4), vec![1, 3, 4, 2]);
    }

    #[test]
    fn round_robin_queues_never_exceeds_available_items() {
        let queues: Vec<Vec<i32>> = vec![vec![1], vec![2]];
        assert_eq!(round_robin_queues(&queues, 10), vec![1, 2]);
    }

    fn concept_doc(slug: &str, name: &str) -> ConceptDoc {
        ConceptDoc {
            slug: slug.to_string(),
            name: name.to_string(),
            sources: Vec::new(),
            related: Vec::new(),
            snippet: String::new(),
            text: String::new(),
        }
    }

    /// A concept group is a singleton, never a file group, and on an
    /// exact score tie for the top lock, the concept wins when its title
    /// collates before the symbol's title.
    #[test]
    fn a_concept_group_is_a_singleton_and_sorts_before_a_file_group_on_a_tie() {
        // File A's baseline (10.0) strictly beats the concept, so it wins
        // the top lock and fronts the list, leaving file B's group and
        // the concept group to tie for second place.
        let a = node("a.rs#run", "run", Kind::Function, "a.rs", "L1-L2");
        let group_a = FileGroup {
            file: "a.rs".to_string(),
            score: 10.0,
            baseline_order: vec![cand(&a, 10.0, 10.0, 10.0)],
            queue: vec![(&a, 10.0)],
            union_coverage: 0.0,
            union_strong_coverage: 0.0,
        };
        let b = node("b.rs#walk", "walk", Kind::Function, "b.rs", "L1-L2");
        let group_b = FileGroup {
            file: "b.rs".to_string(),
            score: 0.5,
            baseline_order: vec![cand(&b, 0.5, 0.5, 0.5)],
            queue: vec![(&b, 0.5)],
            union_coverage: 0.0,
            union_strong_coverage: 0.0,
        };
        // "Aardvark" collates before "walk \u{b7} function", so the tied
        // concept wins the second-place slot on the title compare.
        let doc = concept_doc("aardvark-concept", "Aardvark");

        let (hits, top, _) =
            select_hits_with_concepts(vec![group_a, group_b], &[(&doc, 0.5)], 3, None);

        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].kind, "symbol");
        assert_eq!(hits[0].title, "run \u{b7} function");
        // Second place is a tie: the concept title "Aardvark" collates
        // before the symbol title, so the concept wins the second slot.
        assert_eq!(hits[1].kind, "concept");
        assert_eq!(hits[1].title, "Aardvark");
        assert_eq!(hits[2].kind, "symbol");
        assert_eq!(hits[2].title, "walk \u{b7} function");
        assert_eq!(top_key(&top).as_deref(), Some("a.rs#run"));
    }

    /// A concept that strictly beats the best symbol baseline wins the
    /// top lock: it fronts the group list, and `top` (the coverage
    /// anchor) reports no graph node, since a concept carries none (note
    /// section 2.8).
    #[test]
    fn a_concept_that_strictly_beats_the_baseline_wins_the_top_lock() {
        let n = node("a.rs#run", "run", Kind::Function, "a.rs", "L1-L2");
        let file_group = FileGroup {
            file: "a.rs".to_string(),
            score: 0.2,
            baseline_order: vec![cand(&n, 0.2, 0.2, 0.2)],
            queue: vec![(&n, 0.2)],
            union_coverage: 0.0,
            union_strong_coverage: 0.0,
        };
        let doc = concept_doc("run-concept", "Run Concept");

        let (hits, top, _) = select_hits_with_concepts(vec![file_group], &[(&doc, 1.0)], 2, None);

        assert_eq!(hits[0].kind, "concept");
        assert_eq!(hits[0].title, "Run Concept");
        assert_eq!(top_key(&top).as_deref(), Some("concept:run-concept"));
    }

    /// On an exact score tie for the top lock, a concept titled "Zebra"
    /// loses to a symbol titled "Aardvark", since "Aardvark" collates
    /// before "Zebra".
    #[test]
    fn a_concept_that_ties_the_baseline_loses_the_top_lock_on_title() {
        let n = node("a.rs#run", "Aardvark", Kind::Function, "a.rs", "L1-L2");
        let file_group = FileGroup {
            file: "a.rs".to_string(),
            score: 0.5,
            baseline_order: vec![cand(&n, 0.5, 0.5, 0.5)],
            queue: vec![(&n, 0.5)],
            union_coverage: 0.0,
            union_strong_coverage: 0.0,
        };
        let doc = concept_doc("zebra-concept", "Zebra");

        let (hits, top, _) = select_hits_with_concepts(vec![file_group], &[(&doc, 0.5)], 2, None);

        assert_eq!(hits[0].kind, "symbol");
        assert_eq!(hits[0].title, "Aardvark \u{b7} function");
        assert_eq!(top_key(&top).as_deref(), Some("a.rs#run"));
    }

    /// P3-09: with `baselineTieKey` set, a same-score,
    /// same-title pair compares Equal, so the file keeps its candidate order
    /// and the later span does not move ahead.
    #[test]
    fn test_p3_09_a_same_score_same_title_pair_keeps_candidate_order() {
        let late = node("a.rs#Alpha.run", "run", Kind::Method, "a.rs", "L8-L10");
        let early = node("a.rs#Beta.run", "run", Kind::Method, "a.rs", "L2-L4");
        let first = cand(&late, 0.5, 1.0, 0.5);
        let second = cand(&early, 0.5, 1.0, 0.5);
        assert_eq!(baseline_cmp(&first, &second), Ordering::Equal);
        let corpus = Corpus::build_with_concepts(&[], &HashMap::new(), &[]);
        let groups = rank_files_bounded(vec![first, second], &["run".to_string()], &corpus);
        let ids: Vec<&str> = groups[0].queue.iter().map(|(n, _)| n.id.as_str()).collect();
        assert_eq!(ids, vec!["a.rs#Alpha.run", "a.rs#Beta.run"]);
    }

    /// P3-09: `matchedLexicalTerms` uses exact `.has`.
    /// The query term `helpers` does not match the name token `helper`.
    #[test]
    fn test_p3_09_matched_terms_use_exact_match_with_no_plural_folding() {
        let n = node(
            "a.rs#renderHelper",
            "renderHelper",
            Kind::Function,
            "a.rs",
            "L1-L2",
        );
        let bag = |words: &[&str]| -> Vec<(String, u32)> {
            words.iter().map(|w| (w.to_string(), 1)).collect()
        };
        let mut docs: HashMap<&str, DocBag> = HashMap::new();
        docs.insert(
            n.id.as_str(),
            DocBag {
                name: bag(&["render", "helper"]),
                path: bag(&["a", "rs"]),
                body: Vec::new(),
                body_len: 0,
            },
        );
        let lex: HashMap<String, f64> = [(n.id.clone(), 1.0)].into_iter().collect();
        let terms = vec!["helpers".to_string(), "render".to_string()];
        let nodes = [&n];
        let cands = build_candidates(&nodes, &lex, &HashMap::new(), false, Some((&docs, &terms)));
        assert_eq!(cands[0].matched_terms, vec!["render".to_string()]);
    }

    /// P3-08: two concepts with an equal score tie on title, so the one whose
    /// title collates first takes the top lock, whatever the load order.
    #[test]
    fn test_p3_08_a_concept_score_tie_takes_the_first_title() {
        let alpha = concept_doc("alpha-widget", "Alpha Widget");
        let zeta = concept_doc("zeta-widget", "Zeta Widget");
        for concepts in [[(&alpha, 0.5), (&zeta, 0.5)], [(&zeta, 0.5), (&alpha, 0.5)]] {
            let (hits, top, _) = select_hits_with_concepts(Vec::new(), &concepts, 2, None);
            assert_eq!(hits[0].title, "Alpha Widget");
            assert_eq!(hits[1].title, "Zeta Widget");
            assert_eq!(top_key(&top).as_deref(), Some("concept:alpha-widget"));
        }
    }

    fn weighted_corpus(terms: &[(&str, f64)]) -> Corpus {
        Corpus {
            idf: terms.iter().map(|(t, w)| (t.to_string(), *w)).collect(),
            dflt_idf: 1.0,
            avg_body_len: 0.0,
        }
    }

    /// P3-09: on a raw-lexical tie the anchor is the donor with the larger idf
    /// coverage, not the one with more matched terms. `x` matches one rare
    /// term. `y` matches two common terms, so it has the larger count and the
    /// smaller coverage.
    #[test]
    fn test_p3_09_anchor_tie_goes_to_the_larger_idf_coverage_not_the_count() {
        let x = node("a.rs#x", "xfn", Kind::Function, "a.rs", "L9-L10");
        let y = node("a.rs#y", "yfn", Kind::Function, "a.rs", "L1-L2");
        let mut cx = cand(&x, 0.5, 1.0, 0.5);
        cx.matched_terms = vec!["rare".to_string()];
        let mut cy = cand(&y, 0.5, 1.0, 0.5);
        cy.matched_terms = vec!["c1".to_string(), "c2".to_string()];
        let corpus = weighted_corpus(&[("rare", 3.0), ("c1", 0.5), ("c2", 0.5)]);
        let terms: Vec<String> = ["rare", "c1", "c2"].iter().map(|t| t.to_string()).collect();

        let groups = rank_files_bounded(vec![cy, cx], &terms, &corpus);

        assert_eq!(groups[0].queue[0].0.id, "a.rs#x");
    }

    /// P3-09: two files with the same score tie on
    /// `baselineOrder(a.representative, b.representative)`. The representative
    /// is the pooled anchor, not the file's first baseline candidate.
    #[test]
    fn test_p3_09_group_tie_compares_the_representative_not_the_baseline_first() {
        let terms: Vec<String> = ["t1", "t2"].iter().map(|t| t.to_string()).collect();
        let corpus = weighted_corpus(&[("t1", 1.0), ("t2", 1.0)]);
        // Each file: the baseline-first candidate `m` (0.6) and the anchor
        // `a` (raw 1.0, baseline 0.5). Pooling lifts `a` to 0.625, above 0.6.
        let nodes = [
            node("a.rs#m", "zzz", Kind::Function, "a.rs", "L1-L2"),
            node("a.rs#a", "aaa", Kind::Function, "a.rs", "L3-L4"),
            node("b.rs#m", "bbb", Kind::Function, "b.rs", "L1-L2"),
            node("b.rs#a", "ccc", Kind::Function, "b.rs", "L3-L4"),
        ];
        let mut cands = Vec::new();
        for pair in nodes.chunks(2) {
            let mut m = cand(&pair[0], 0.6, 0.6, 0.6);
            m.matched_terms = vec!["t2".to_string()];
            let mut a = cand(&pair[1], 0.5, 1.0, 0.5);
            a.matched_terms = vec!["t1".to_string()];
            cands.push(m);
            cands.push(a);
        }

        let groups = rank_files_bounded(cands, &terms, &corpus);

        assert_eq!(groups[0].queue[0].1, groups[1].queue[0].1);
        // Representatives `aaa` and `ccc` order a.rs first. The baseline-first
        // titles `zzz` and `bbb` would order b.rs first.
        assert_eq!(groups[0].file, "a.rs");
    }

    /// P3-08 and P3-09: on the multi-scope path the top
    /// lock reads `fused.best`, the best candidate of the candidate-level
    /// combine, and not the first group's leader. The locked queue scores
    /// every member from `fused.scores`. A concept compares against the
    /// best candidate's fused score.
    #[test]
    fn test_p3_08_fused_top_lock_uses_the_candidate_combine_not_the_first_group() {
        let lead = node("a.rs#lead", "lead", Kind::Function, "a.rs", "L1-L2");
        let top = node("b.rs#top", "top", Kind::Function, "b.rs", "L1-L2");
        let other = node("b.rs#other", "other", Kind::Function, "b.rs", "L3-L4");
        let groups = || {
            vec![
                FileGroup {
                    file: "a.rs".to_string(),
                    score: 1.0,
                    baseline_order: vec![cand(&lead, 1.0, 1.0, 1.0)],
                    queue: vec![(&lead, 1.0)],
                    union_coverage: 0.0,
                    union_strong_coverage: 0.0,
                },
                FileGroup {
                    file: "b.rs".to_string(),
                    score: 0.9,
                    baseline_order: vec![cand(&other, 0.4, 0.4, 0.4), cand(&top, 0.9, 0.9, 0.9)],
                    queue: vec![(&other, 0.9), (&top, 0.4)],
                    union_coverage: 0.0,
                    union_strong_coverage: 0.0,
                },
            ]
        };
        let scores: HashMap<String, f64> = [
            ("a.rs#lead".to_string(), 0.8),
            ("b.rs#top".to_string(), 0.95),
            ("b.rs#other".to_string(), 0.3),
        ]
        .into_iter()
        .collect();
        let fused = FusedTop {
            best: Some((&top, 0.95)),
            scores,
        };

        let (hits, top_item, _) = select_hits_with_concepts(groups(), &[], 3, Some(&fused));

        let titles: Vec<&str> = hits.iter().map(|h| h.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "top \u{b7} function",
                "lead \u{b7} function",
                "other \u{b7} function"
            ]
        );
        assert_eq!(hits[0].score, 0.95);
        assert_eq!(hits[2].score, 0.3);
        assert_eq!(top_key(&top_item).as_deref(), Some("b.rs#top"));

        // A concept at 0.97 beats the fused best (0.95). It would lose to
        // the first group's leader (1.0), so the first hit shows which one
        // the lock compares against.
        let doc = concept_doc("c", "C");
        let (hits, _, _) = select_hits_with_concepts(groups(), &[(&doc, 0.97)], 1, Some(&fused));
        assert_eq!(hits[0].kind, "concept");
    }

    /// P3-09: at `-n 0` the top is `scored[0]`, the best
    /// unlocked leader, and not the locked group's first item. Here the
    /// lock picks `b1` (baseline 0.6), but `a1` leads its file at 0.9.
    #[test]
    fn test_p3_09_limit_zero_top_is_the_best_unlocked_leader() {
        let a1 = node("a.rs#a1", "a1", Kind::Function, "a.rs", "L1-L2");
        let b1 = node("b.rs#b1", "b1", Kind::Function, "b.rs", "L1-L2");
        let groups = vec![
            FileGroup {
                file: "a.rs".to_string(),
                score: 0.9,
                baseline_order: vec![cand(&a1, 0.5, 0.5, 0.5)],
                queue: vec![(&a1, 0.9)],
                union_coverage: 0.0,
                union_strong_coverage: 0.0,
            },
            FileGroup {
                file: "b.rs".to_string(),
                score: 0.6,
                baseline_order: vec![cand(&b1, 0.6, 0.6, 0.6)],
                queue: vec![(&b1, 0.6)],
                union_coverage: 0.0,
                union_strong_coverage: 0.0,
            },
        ];

        let (hits, top, scored) = select_hits_with_concepts(groups, &[], 0, None);

        assert!(hits.is_empty() && scored);
        assert_eq!(top_key(&top).as_deref(), Some("a.rs#a1"));
    }

    /// P3-08: the locked queue drops a member that is
    /// the same hit as the top by kind, pointer and title, not by node id.
    #[test]
    fn test_p3_08_locked_queue_filters_with_same_hit_not_node_id() {
        let t = node("a.rs#t", "t", Kind::Function, "a.rs", "L1-L2");
        let twin = node("a.rs#t2", "t", Kind::Function, "a.rs", "L1-L2");
        let groups = vec![FileGroup {
            file: "a.rs".to_string(),
            score: 0.8,
            baseline_order: vec![cand(&t, 0.8, 0.8, 0.8), cand(&twin, 0.5, 0.5, 0.5)],
            queue: vec![(&t, 0.8), (&twin, 0.5)],
            union_coverage: 0.0,
            union_strong_coverage: 0.0,
        }];

        let (hits, _, _) = select_hits_with_concepts(groups, &[], 5, None);

        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn rank_files_bounded_picks_the_baseline_representative_when_p_f_equals_it() {
        // A single donor in a file: the anchor is the only member, so the
        // pooled score P_F collapses to the same value as its own
        // baseline. The baseline representative must win the tie.
        let n = node("a.rs#run", "run", Kind::Function, "a.rs", "L1-L2");
        let mut c = cand(&n, 0.5, 1.0, 0.5);
        c.matched_terms = vec!["run".to_string()];
        let corpus = Corpus::build_with_concepts(&[], &HashMap::new(), &[]);
        let groups = rank_files_bounded(vec![c], &["run".to_string()], &corpus);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].queue[0].0.id, "a.rs#run");
    }
}
