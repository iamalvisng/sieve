//! The lexical corpus, the tokenizer wrapper, and BM25/name/path scoring
//! (`ask-ranking.md` sections 1 to 3).

use std::collections::{HashMap, HashSet};

use sieve_core::askindex::{bag, body_len, tokenize, AskDoc, AskIndex, Bag};
use sieve_core::{Graph, Kind, Node};

use super::test_factor;

/// One node's three token bags, built either from the sidecar or live.
#[derive(Debug, Clone)]
pub(crate) struct DocBag {
    pub name: Bag,
    pub path: Bag,
    pub body: Bag,
    pub body_len: u32,
}

fn doc_bag_from_node(node: &Node) -> DocBag {
    let name = bag(&tokenize(&node.name));
    let path = bag(&tokenize(&node.path));
    let body_source = format!(
        "{} {}",
        node.signature.as_deref().unwrap_or(""),
        node.summary.as_deref().unwrap_or("")
    );
    let body = bag(&tokenize(&body_source));
    let body_len_val = body.iter().map(|(_, c)| *c).sum();
    DocBag {
        name,
        path,
        body,
        body_len: body_len_val,
    }
}

fn doc_bag_from_ask_doc(doc: &AskDoc) -> DocBag {
    DocBag {
        name: doc.name.clone(),
        path: doc.path.clone(),
        body: doc.body.clone(),
        body_len: body_len(doc),
    }
}

/// Builds every graph node's token bags, using the sidecar when it is
/// usable (section 1.9), else tokenizing live (section 1.10).
pub(crate) fn build_docs<'a>(
    graph: &'a Graph,
    index: Option<&AskIndex>,
) -> HashMap<&'a str, DocBag> {
    if let Some(idx) = index {
        let by_id: HashMap<&str, &AskDoc> = idx.docs.iter().map(|d| (d.id.as_str(), d)).collect();
        let usable = idx.docs.len() == graph.nodes.len()
            && graph
                .nodes
                .iter()
                .all(|n| by_id.contains_key(n.id.as_str()));
        if usable {
            return graph
                .nodes
                .iter()
                .map(|n| (n.id.as_str(), doc_bag_from_ask_doc(by_id[n.id.as_str()])))
                .collect();
        }
    }
    graph
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), doc_bag_from_node(n)))
        .collect()
}

/// The query's unique tokens, in first-occurrence order (section 2.5).
pub fn query_terms(query: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for t in tokenize(query) {
        if seen.insert(t.clone()) {
            out.push(t);
        }
    }
    out
}

/// The document-frequency table and average body length over one active
/// node set (section 1.11).
#[derive(Debug, Clone)]
pub(crate) struct Corpus {
    pub(crate) idf: HashMap<String, f64>,
    /// `ln(1 + docCount)`, the coverage-function fallback (section 3.1).
    pub dflt_idf: f64,
    pub avg_body_len: f64,
}

/// JavaScript's `Math.log` (V8's fdlibm port, `ieee754::log`). The system
/// `ln` differs from it in the last bit for about 9 percent of the idf
/// inputs, and that bit shows in `coverage`. The `mul_add` calls match the
/// fused multiply-add that V8's arm64 build emits. The port matches node on
/// arm64 (the host that wrote the expected values). An x64 node does not
/// fuse, so the last bit can differ there. For an input that is not a
/// positive normal number the port is not needed, and the system `ln`
/// answers.
#[allow(clippy::excessive_precision)] // the fdlibm constants, verbatim
pub(crate) fn js_ln(x: f64) -> f64 {
    const LN2_HI: f64 = 6.931_471_803_691_238_164_90e-01;
    const LN2_LO: f64 = 1.908_214_929_270_587_700_02e-10;
    const LG1: f64 = 6.666_666_666_666_735_130e-01;
    const LG2: f64 = 3.999_999_999_940_941_908e-01;
    const LG3: f64 = 2.857_142_874_366_239_149e-01;
    const LG4: f64 = 2.222_219_843_214_978_396e-01;
    const LG5: f64 = 1.818_357_216_161_805_012e-01;
    const LG6: f64 = 1.531_383_769_920_937_332e-01;
    const LG7: f64 = 1.479_819_860_511_658_591e-01;
    if !x.is_normal() || x < 0.0 {
        return x.ln();
    }
    let bits = x.to_bits();
    let mut hx = (bits >> 32) as i32;
    let mut k = (hx >> 20) - 1023;
    hx &= 0x000f_ffff;
    let i = (hx + 0x95f64) & 0x10_0000;
    let x =
        f64::from_bits((u64::from((hx | (i ^ 0x3ff0_0000)) as u32) << 32) | (bits & 0xffff_ffff));
    k += i >> 20;
    let f = x - 1.0;
    let dk = f64::from(k);
    if (0x000f_ffff & (2 + hx)) < 3 {
        if f == 0.0 {
            return if k == 0 {
                0.0
            } else {
                dk * LN2_HI + dk * LN2_LO
            };
        }
        let r = f * f * (0.5 - 0.333_333_333_333_333_33 * f);
        return if k == 0 {
            f - r
        } else {
            dk * LN2_HI - ((r - dk * LN2_LO) - f)
        };
    }
    let s = f / (2.0 + f);
    let z = s * s;
    let mut i = hx - 0x6147a;
    let w = z * z;
    let j = 0x6b851 - hx;
    let t1 = w * (LG2 + w * (LG4 + w * LG6));
    let t2 = z * (LG1 + w * (LG3 + w * (LG5 + w * LG7)));
    i |= j;
    let r = t2 + t1;
    if i > 0 {
        let hfsq = 0.5 * f * f;
        if k == 0 {
            f - (-s).mul_add(hfsq + r, hfsq)
        } else {
            dk * LN2_HI - ((hfsq - s.mul_add(hfsq + r, dk * LN2_LO)) - f)
        }
    } else if k == 0 {
        (-s).mul_add(f - r, f)
    } else {
        dk * LN2_HI - (s.mul_add(f - r, -(dk * LN2_LO)) - f)
    }
}

impl Corpus {
    /// Builds the corpus over `active_nodes`, then folds every concept
    /// document's own term set into `df` as one more document, with `n =
    /// docCount + conceptBags.length` (the `deep-tier.md` note section 2.8).
    /// `avgBodyLen` never counts a concept: BM25 never scores one.
    pub(crate) fn build_with_concepts(
        active_nodes: &[&Node],
        docs: &HashMap<&str, DocBag>,
        concept_bags: &[HashSet<String>],
    ) -> Corpus {
        let mut df: HashMap<String, u32> = HashMap::new();
        let mut total_body: u64 = 0;
        for node in active_nodes {
            let Some(d) = docs.get(node.id.as_str()) else {
                continue;
            };
            let mut seen: HashSet<&str> = HashSet::new();
            for (t, _) in d.name.iter().chain(d.path.iter()).chain(d.body.iter()) {
                if seen.insert(t.as_str()) {
                    *df.entry(t.clone()).or_insert(0) += 1;
                }
            }
            total_body += u64::from(d.body_len);
        }
        for bag in concept_bags {
            for t in bag {
                *df.entry(t.clone()).or_insert(0) += 1;
            }
        }
        let doc_count = active_nodes.len();
        let n = doc_count + concept_bags.len();
        let avg_body_len = if doc_count == 0 {
            0.0
        } else {
            total_body as f64 / doc_count as f64
        };
        let idf: HashMap<String, f64> = df
            .into_iter()
            .map(|(t, c)| (t, js_ln(1.0 + n as f64 / (1.0 + c as f64))))
            .collect();
        let dflt_idf = js_ln(1.0 + n as f64);
        Corpus {
            idf,
            dflt_idf,
            avg_body_len,
        }
    }

    /// A token's `idf`, or `1.0` when the corpus never saw it (section
    /// 3.1, `score` and `bm25` sites).
    pub(crate) fn idf(&self, term: &str) -> f64 {
        *self.idf.get(term).unwrap_or(&1.0)
    }

    /// A token's `idf`, or `dfltIdf` when the corpus never saw it
    /// (section 3.1, the coverage-function site).
    pub(crate) fn idf_or_default(&self, term: &str) -> f64 {
        *self.idf.get(term).unwrap_or(&self.dflt_idf)
    }
}

fn bag_count(bag: &Bag, term: &str) -> Option<u32> {
    bag.iter().find(|(t, _)| t == term).map(|(_, c)| *c)
}

/// `score(query, doc, idf)` (section 3.2): exact term equality, no plural
/// folding. Also the concept score's building block (note section 2.8:
/// `total = score(name)*3 + score(body)`, no BM25 for a concept).
pub(crate) fn score(terms: &[String], doc: &Bag, corpus: &Corpus) -> f64 {
    terms
        .iter()
        .filter_map(|t| bag_count(doc, t).map(|c| f64::from(c) * corpus.idf(t)))
        .sum()
}

/// `bm25(query, doc, idf)` (section 3.3).
pub(crate) fn bm25(terms: &[String], body: &Bag, corpus: &Corpus, dl: u32) -> f64 {
    let avg = if corpus.avg_body_len != 0.0 {
        corpus.avg_body_len
    } else {
        1.0
    };
    let norm = super::K1 * (1.0 - super::B + super::B * f64::from(dl) / avg);
    terms
        .iter()
        .filter_map(|t| {
            bag_count(body, t).map(|c| {
                let tf = f64::from(c);
                // Op order pins the `bm25`: idf times the whole `tf * (k1 + 1)
                // / (tf + norm)` term, not `idf * tf` first. The two float
                // orders can differ by one ULP.
                corpus.idf(t) * (tf * (super::K1 + 1.0) / (tf + norm))
            })
        })
        .sum()
}

/// One node's lexical total (section 3.4).
pub(crate) fn node_total(
    terms: &[String],
    doc: &DocBag,
    corpus: &Corpus,
    query_wants_tests: bool,
    path: &str,
) -> f64 {
    let name_score = score(terms, &doc.name, corpus) * 3.0;
    let path_score = score(terms, &doc.path, corpus) * 2.0;
    let body_score = bm25(terms, &doc.body, corpus, doc.body_len);
    let factor = test_factor(query_wants_tests, path);
    (name_score + path_score + body_score) * factor
}

/// Scores every active node, keeping only a positive total (section 3.4).
pub(crate) fn score_all(
    active_nodes: &[&Node],
    docs: &HashMap<&str, DocBag>,
    corpus: &Corpus,
    terms: &[String],
    query_wants_tests: bool,
) -> HashMap<String, f64> {
    let mut lex = HashMap::new();
    for node in active_nodes {
        let Some(doc) = docs.get(node.id.as_str()) else {
            continue;
        };
        let total = node_total(terms, doc, corpus, query_wants_tests, &node.path);
        if total > 0.0 {
            lex.insert(node.id.clone(), total);
        }
    }
    lex
}

/// Whether `bag` holds `term`, folding one trailing `s` either way
/// (section 8.4). Ports `hasTerm`: `t`, `t + "s"`, and
/// `t` without its last `s`. A term that ends in `s` checks all three, so
/// `pass` also finds `passs`.
pub(crate) fn has_term(bag: &Bag, term: &str) -> bool {
    bag_count(bag, term).is_some()
        || bag_count(bag, &format!("{term}s")).is_some()
        || term
            .strip_suffix('s')
            .is_some_and(|stripped| bag_count(bag, stripped).is_some())
}

/// Whether `bag` holds `term` exactly, with no plural folding
/// (`matchedLexicalTerms`: a plain `.has` lookup).
pub(crate) fn has_exact(bag: &Bag, term: &str) -> bool {
    bag_count(bag, term).is_some()
}

/// `matchedIdfShare(query, fields, idf, dfltIdf)` (`ask.ts` lines 455 to
/// 469): the idf-weighted share of the query terms found in any of
/// `fields`. Each term counts by `idf_or_default`, so a rare term that
/// matches outweighs many common terms that miss. Terms the corpus never
/// saw take `dflt_idf`, the same fallback `idf_or_default` returns.
pub(crate) fn matched_idf_share(terms: &[String], fields: &[&Bag], corpus: &Corpus) -> f64 {
    let mut matched = 0.0;
    let mut total = 0.0;
    for term in terms {
        let w = corpus.idf_or_default(term);
        total += w;
        if fields.iter().any(|f| has_term(f, term)) {
            matched += w;
        }
    }
    if total > 0.0 {
        matched / total
    } else {
        0.0
    }
}

/// `strongShare(q, node, doc, idf, dfltIdf)` (`ask.ts` lines 444 to 453):
/// the match-strength share behind `coverageStrong`, over the NAME field
/// only. A `file` node's `name` is a path basename, not a symbol name, so
/// a file node contributes zero strength (`ask.ts` line 451).
pub(crate) fn strong_share(kind: Kind, name: &Bag, terms: &[String], corpus: &Corpus) -> f64 {
    if kind == Kind::File {
        return 0.0;
    }
    matched_idf_share(terms, &[name], corpus)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(name: &[(&str, u32)], path: &[(&str, u32)], body: &[(&str, u32)]) -> DocBag {
        let to_bag =
            |s: &[(&str, u32)]| -> Bag { s.iter().map(|(t, c)| (t.to_string(), *c)).collect() };
        let body_bag = to_bag(body);
        let body_len_val = body_bag.iter().map(|(_, c)| *c).sum();
        DocBag {
            name: to_bag(name),
            path: to_bag(path),
            body: body_bag,
            body_len: body_len_val,
        }
    }

    #[test]
    fn matched_idf_share_and_strong_share_diverge_on_a_body_only_term() {
        // "run" matches the name field; "helper" matches only the body
        // field. `matched_idf_share` over [name, path, body] counts both
        // terms as found; `strong_share` (name only) counts only "run".
        let corpus = Corpus {
            idf: [("run".to_string(), 2.0), ("helper".to_string(), 0.5)]
                .into_iter()
                .collect(),
            dflt_idf: 1.0,
            avg_body_len: 0.0,
        };
        let terms = vec!["run".to_string(), "helper".to_string()];
        let d = doc(&[("run", 1)], &[], &[("helper", 1)]);

        let coverage = matched_idf_share(&terms, &[&d.name, &d.path, &d.body], &corpus);
        let coverage_strong = strong_share(Kind::Function, &d.name, &terms, &corpus);

        // By hand: total weight = 2.0 + 0.5 = 2.5.
        // Plain: "run" (2.0) and "helper" (0.5) both found -> 2.5 / 2.5 = 1.0.
        // Strong: only "run" (2.0) found in name -> 2.0 / 2.5 = 0.8.
        assert!((coverage - 1.0).abs() < 1e-12, "coverage: {coverage}");
        assert!(
            (coverage_strong - 0.8).abs() < 1e-12,
            "coverageStrong: {coverage_strong}"
        );
        assert!(coverage > coverage_strong);
    }

    #[test]
    fn strong_share_is_zero_for_a_file_node_even_when_the_name_matches() {
        let corpus = Corpus {
            idf: [("run".to_string(), 2.0)].into_iter().collect(),
            dflt_idf: 1.0,
            avg_body_len: 0.0,
        };
        let terms = vec!["run".to_string()];
        let d = doc(&[("run", 1)], &[], &[]);
        assert_eq!(strong_share(Kind::File, &d.name, &terms, &corpus), 0.0);
    }

    /// P3-03: the idf is `Math.log(1 + n / (1 + df))`.
    /// These inputs make the system `ln` differ from V8's `Math.log` in
    /// the last bit. The expected values come from node 24.
    #[test]
    fn test_p3_03_js_ln_matches_v8_math_log_where_system_ln_differs() {
        for (n, df, want) in [
            (13.0_f64, 0.0_f64, 2.639057329615259_f64),
            (24.0, 16.0, 0.8803587226480918),
            (16.0, 6.0, 1.1895840668738364),
            (34.0, 14.0, 1.1837700970084166),
        ] {
            assert_eq!(js_ln(1.0 + n / (1.0 + df)), want, "n={n} df={df}");
        }
    }

    /// P3-03: inputs in `[1, sqrt(2))` take the `k == 0` branches. The
    /// expected values come from node 24 on arm64. The unfused form of
    /// those branches differs from them in the last bit.
    #[test]
    fn test_p3_03_js_ln_fuses_the_k_zero_branches_like_v8() {
        for (x, want) in [
            (1.222946831427598_f64, 0.20126338186598544_f64),
            (1.254042791309991, 0.226372565480191),
            (1.2524861465747739, 0.22513049329677068),
            (1.2650218754482099, 0.23508941487439874),
            (1.2787669018689298, 0.24589625568915816),
            (1.0362257523725509, 0.03558502779437033),
        ] {
            assert_eq!(js_ln(x), want, "x={x}");
        }
    }

    #[test]
    fn idf_matches_the_formula_with_four_docs_two_hits() {
        let expected = (1.0 + 4.0 / (1.0 + 2.0_f64)).ln();
        let corpus = Corpus {
            idf: [("run".to_string(), expected)].into_iter().collect(),
            dflt_idf: (1.0 + 4.0_f64).ln(),
            avg_body_len: 0.0,
        };
        assert!((corpus.idf("run") - expected).abs() < 1e-12);
        assert_eq!(corpus.idf("unseen"), 1.0);
        assert!((corpus.idf_or_default("unseen") - corpus.dflt_idf).abs() < 1e-12);
    }

    #[test]
    fn bm25_matches_golden_op_order_on_the_multi_fixture_render_footer_values() {
        // Pins P3-15: 's `idf * ((tf * (k1 + 1)) / (tf + norm))`
        // op order. `idf * tf * (k1 + 1) / (tf + norm)` rounds one ULP high
        // on these inputs, the `multi` fixture's `render.ts#renderFooter`
        // row (`ask-json.stdout.txt`, score `0.8255402111579943`).
        let corpus = Corpus {
            idf: [("render".to_string(), 1.029_619_417_181_158_1)]
                .into_iter()
                .collect(),
            dflt_idf: 1.0,
            avg_body_len: 10.037_037_037_037_036,
        };
        let terms = vec!["render".to_string()];
        let body = doc(&[], &[], &[("render", 2)]).body;
        let got = bm25(&terms, &body, &corpus, 10);
        assert_eq!(got, 1.417_197_498_611_119_5);
    }

    #[test]
    fn bm25_favors_a_shorter_document_with_equal_term_frequency() {
        let corpus = Corpus {
            idf: [("run".to_string(), 1.0)].into_iter().collect(),
            dflt_idf: 1.0,
            avg_body_len: 4.0,
        };
        let terms = vec!["run".to_string()];
        let short = doc(&[], &[], &[("run", 1)]);
        let long = doc(&[], &[], &[("run", 1), ("padding", 7)]);
        let short_score = bm25(&terms, &short.body, &corpus, short.body_len);
        let long_score = bm25(&terms, &long.body, &corpus, long.body_len);
        assert!(short_score > long_score);
    }

    #[test]
    fn test_factor_penalizes_a_test_path_only_when_the_query_does_not_want_tests() {
        assert_eq!(
            test_factor(false, "src/tests/app.rs"),
            super::super::TEST_RANK_PENALTY
        );
        assert_eq!(test_factor(true, "src/tests/app.rs"), 1.0);
        assert_eq!(test_factor(false, "src/app.rs"), 1.0);
    }

    #[test]
    fn wants_tests_matches_a_whole_word() {
        assert!(super::super::wants_tests("run the tests"));
        assert!(!super::super::wants_tests("run the greatest"));
    }

    #[test]
    fn has_term_folds_one_trailing_s() {
        let b: Bag = vec![("helper".to_string(), 1)];
        assert!(has_term(&b, "helper"));
        assert!(has_term(&b, "helpers"));
        assert!(!has_term(&b, "help"));
    }

    /// P1-06: a term that ends in `s` also finds itself plus `s`, so `pass`
    /// finds `passs`.
    #[test]
    fn test_p1_06_has_term_checks_the_term_plus_s_when_it_ends_in_s() {
        let b: Bag = vec![("passs".to_string(), 1)];
        assert!(has_term(&b, "pass"));
        assert!(!has_term(&b, "pas"));
        assert!(!has_exact(&b, "pass"));
        assert!(has_exact(&b, "passs"));
    }
}
