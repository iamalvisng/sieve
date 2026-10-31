//! The lexical corpus, the tokenizer wrapper, and BM25/name/path scoring
//!.

use std::collections::{HashMap, HashSet};

use sieve_core::askindex::{bag, body_len, tokenize, AskDoc, AskIndex, Bag};
use sieve_core::{Graph, Kind, Node};

use super::{
    is_test_class_name, is_test_module_name, is_test_path, test_factor, TEST_RANK_PENALTY,
};
use crate::blast::span_bounds;

/// One node's three token bags, built either from the sidecar or live.
#[derive(Debug, Clone)]
pub(crate) struct DocBag {
    pub name: Bag,
    pub path: Bag,
    pub body: Bag,
    pub body_len: u32,
    /// Set when the node sits inside a Rust test module of its own file.
    pub inline_test: bool,
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
        inline_test: false,
    }
}

fn doc_bag_from_ask_doc(doc: &AskDoc) -> DocBag {
    DocBag {
        name: doc.name.clone(),
        path: doc.path.clone(),
        body: doc.body.clone(),
        body_len: body_len(doc),
        inline_test: false,
    }
}

/// Every node's token bags by node id. The keys are owned, so a warm caller
/// can keep the map beyond one query.
pub(crate) type Docs = HashMap<String, DocBag>;

/// Builds every graph node's token bags, using the sidecar when it is
/// usable, else tokenizing live. The flag is true when the sidecar was used.
pub(crate) fn build_docs(graph: &Graph, index: Option<&AskIndex>) -> (Docs, bool) {
    if let Some(idx) = index {
        let by_id: HashMap<&str, &AskDoc> = idx.docs.iter().map(|d| (d.id.as_str(), d)).collect();
        let usable = idx.docs.len() == graph.nodes.len()
            && graph
                .nodes
                .iter()
                .all(|n| by_id.contains_key(n.id.as_str()));
        if usable {
            let docs = graph
                .nodes
                .iter()
                .map(|n| (n.id.clone(), doc_bag_from_ask_doc(by_id[n.id.as_str()])))
                .collect();
            return (docs, true);
        }
    }
    let docs = graph
        .nodes
        .iter()
        .map(|n| (n.id.clone(), doc_bag_from_node(n)))
        .collect();
    (docs, false)
}

/// Swaps the bags of the files in `changed`. It drops the bags of the nodes
/// these files held in `old`, adds their fresh bags, and decides the inline
/// tests again for these files only. A test scope and the nodes in it share
/// one file, so no other file changes.
pub(crate) fn patch_docs(
    docs: &mut Docs,
    old: &Graph,
    new: &Graph,
    changed: &HashMap<&str, &[AskDoc]>,
) {
    for node in &old.nodes {
        if changed.contains_key(node.path.as_str()) {
            docs.remove(&node.id);
        }
    }
    for doc in changed.values().flat_map(|docs| docs.iter()) {
        docs.insert(doc.id.clone(), doc_bag_from_ask_doc(doc));
    }
    mark_inline_tests(
        new.nodes
            .iter()
            .filter(|n| changed.contains_key(n.path.as_str())),
        docs,
    );
}

/// The query's unique tokens, in first-occurrence order.
pub fn query_terms(query: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for t in tokenize(query) {
        if seen.insert(t.clone()) {
            out.push(t);
        }
    }
    if is_how_does_work(query) {
        out.retain(|t| t != "work" && t != "works");
    }
    out
}

/// Whether the query has the shape "how does X work". The last word is a
/// verb that frames the question, not a name to find, so it is no term.
fn is_how_does_work(query: &str) -> bool {
    let words: Vec<String> = query
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    let [first, second, .., last] = words.as_slice() else {
        return false;
    };
    first == "how" && (second == "does" || second == "do") && (last == "work" || last == "works")
}

/// The document-frequency table and average body length over one active
/// node set.
#[derive(Debug, Clone)]
pub(crate) struct Corpus {
    pub(crate) idf: HashMap<String, f64>,
    /// `ln(1 + docCount)`, the coverage-function fallback.
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
    /// docCount + conceptBags.length`.
    /// `avgBodyLen` never counts a concept: BM25 never scores one.
    pub(crate) fn build_with_concepts(
        active_nodes: &[&Node],
        docs: &Docs,
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
    /// (the coverage-function site).
    pub(crate) fn idf_or_default(&self, term: &str) -> f64 {
        *self.idf.get(term).unwrap_or(&self.dflt_idf)
    }
}

fn bag_count(bag: &Bag, term: &str) -> Option<u32> {
    bag.iter().find(|(t, _)| t == term).map(|(_, c)| *c)
}

/// `score(query, doc, idf)`: exact term equality, no plural
/// folding. Also the concept score's building block:
/// `total = score(name)*3 + score(body)`, with no BM25 for a concept.
pub(crate) fn score(terms: &[String], doc: &Bag, corpus: &Corpus) -> f64 {
    terms
        .iter()
        .filter_map(|t| bag_count(doc, t).map(|c| f64::from(c) * corpus.idf(t)))
        .sum()
}

/// `bm25(query, doc, idf)`.
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

/// Whether `n` opens a test scope: a Rust test module or a Python
/// `Test*` class.
fn is_test_scope(n: &Node) -> bool {
    (n.kind == Kind::Module && n.path.ends_with(".rs") && is_test_module_name(&n.name))
        || (n.kind == Kind::Class && n.path.ends_with(".py") && is_test_class_name(&n.name))
}

/// Whether `n` is a Python `test_*` function in a test file. Elsewhere the
/// function is code unless it lies inside a test class.
fn is_py_test_fn(n: &Node) -> bool {
    matches!(n.kind, Kind::Function | Kind::Method)
        && n.path.ends_with(".py")
        && n.name.starts_with("test_")
        && is_test_path(&n.path)
}

/// Sets `inline_test` on every doc whose node lies inside a test scope of
/// the same file: a Rust test module (`tests`, `test` or `*_tests`) or a
/// Python `Test*` class. A Python `test_*` function is a test too. The id
/// has no scope, so the scope node's line span is the only signal.
/// Runs once per ask run.
pub(crate) fn mark_inline_tests<'a>(
    nodes: impl IntoIterator<Item = &'a Node> + Clone,
    docs: &mut Docs,
) {
    let mut scopes: HashMap<&str, Vec<(u32, u32)>> = HashMap::new();
    for n in nodes.clone() {
        if is_test_scope(n) {
            if let Some(b) = span_bounds(&n.span) {
                scopes.entry(n.path.as_str()).or_default().push(b);
            }
        }
    }
    let mut test_ids: HashSet<&str> = HashSet::new();
    let mut code_ids: HashSet<&str> = HashSet::new();
    for n in nodes {
        if is_test_scope(n) {
            continue;
        }
        if is_py_test_fn(n) {
            test_ids.insert(n.id.as_str());
            continue;
        }
        let inside = match (scopes.get(n.path.as_str()), span_bounds(&n.span)) {
            (Some(spans), Some(b)) => spans.iter().any(|&(s, e)| b.0 >= s && b.1 <= e),
            _ => false,
        };
        if inside {
            test_ids.insert(n.id.as_str());
        } else {
            code_ids.insert(n.id.as_str());
        }
    }
    // The id has no scope: a code node that shares an id with a test node
    // keeps its rank.
    for id in test_ids.difference(&code_ids) {
        if let Some(d) = docs.get_mut(*id) {
            d.inline_test = true;
        }
    }
}

/// The rank factor of a test in a Rust source file. A test whose name
/// holds every query term keeps its rank, so an exact-name lookup works.
pub(crate) fn inline_test_factor(query_wants_tests: bool, doc: &DocBag, terms: &[String]) -> f64 {
    if !query_wants_tests && doc.inline_test && !terms.iter().all(|t| has_exact(&doc.name, t)) {
        TEST_RANK_PENALTY
    } else {
        1.0
    }
}

/// One node's lexical total.
pub(crate) fn node_total(
    terms: &[String],
    doc: &DocBag,
    corpus: &Corpus,
    query_wants_tests: bool,
    query_words: &[String],
    path: &str,
) -> f64 {
    let name_score = score(terms, &doc.name, corpus) * 3.0;
    let path_score = score(terms, &doc.path, corpus) * 2.0;
    let body_score = bm25(terms, &doc.body, corpus, doc.body_len);
    // A script, like a test path, gets this factor twice: here and in the
    // candidate baseline.
    let factor = test_factor(query_wants_tests, query_words, path).min(inline_test_factor(
        query_wants_tests,
        doc,
        terms,
    ));
    (name_score + path_score + body_score) * factor
}

/// Scores every active node, keeping only a positive total.
pub(crate) fn score_all(
    active_nodes: &[&Node],
    docs: &Docs,
    corpus: &Corpus,
    terms: &[String],
    query_wants_tests: bool,
    query_words: &[String],
) -> HashMap<String, f64> {
    let mut lex = HashMap::new();
    for node in active_nodes {
        let Some(doc) = docs.get(node.id.as_str()) else {
            continue;
        };
        let total = node_total(
            terms,
            doc,
            corpus,
            query_wants_tests,
            query_words,
            &node.path,
        );
        if total > 0.0 {
            lex.insert(node.id.clone(), total);
        }
    }
    lex
}

/// Whether `bag` holds `term`, folding one trailing `s` either way
///. Ports `hasTerm`: `t`, `t + "s"`, and
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
            inline_test: false,
        }
    }

    fn mod_node(id: &str, name: &str, kind: Kind, path: &str, span: &str) -> Node {
        Node {
            id: id.to_string(),
            name: name.to_string(),
            kind,
            owner: None,
            path: path.to_string(),
            span: span.to_string(),
            signature: None,
            exported: false,
            origin: sieve_core::Origin::Ast,
            body_hash: "0".repeat(64),
            chars: None,
            body_text: None,
            arity: None,
            variadic: None,
            summary_state: sieve_core::SummaryState::Pending,
            summary: None,
            crux: None,
        }
    }

    /// Nodes shaped like the extractor output for a file with `fn code`
    /// and `mod tests { fn code_name_words_test }`: the Rust ids carry no
    /// module scope.
    fn sample_nodes(path: &str) -> Vec<Node> {
        let id = |n: &str| format!("{path}#{n}");
        vec![
            mod_node(&id("code"), "code", Kind::Function, path, "L1-L1"),
            mod_node(&id("tests"), "tests", Kind::Module, path, "L3-L6"),
            mod_node(
                &id("code_name_words_test"),
                "code_name_words_test",
                Kind::Function,
                path,
                "L5-L5",
            ),
        ]
    }

    fn marked(nodes: &[Node]) -> Docs {
        let mut docs: Docs = nodes
            .iter()
            .map(|n| (n.id.clone(), doc_bag_from_node(n)))
            .collect();
        mark_inline_tests(nodes, &mut docs);
        docs
    }

    #[test]
    fn mark_inline_tests_marks_the_fn_inside_a_test_module_only() {
        let nodes = sample_nodes("src/a.rs");
        let docs = marked(&nodes);
        assert!(!docs["src/a.rs#code"].inline_test);
        assert!(!docs["src/a.rs#tests"].inline_test);
        assert!(docs["src/a.rs#code_name_words_test"].inline_test);
    }

    #[test]
    fn mark_inline_tests_skips_a_non_rust_file_and_a_plain_module() {
        let nodes = sample_nodes("src/a.ts");
        assert!(!marked(&nodes)["src/a.ts#code_name_words_test"].inline_test);
        let mut nodes = sample_nodes("src/a.rs");
        nodes[1].name = "helpers".to_string();
        assert!(!marked(&nodes)["src/a.rs#code_name_words_test"].inline_test);
    }

    #[test]
    fn mark_inline_tests_skips_a_module_named_contests() {
        let mut nodes = sample_nodes("src/a.rs");
        nodes[1].name = "contests".to_string();
        assert!(!marked(&nodes)["src/a.rs#code_name_words_test"].inline_test);
    }

    #[test]
    fn mark_inline_tests_marks_a_fn_in_a_one_line_test_module() {
        let mut nodes = sample_nodes("src/a.rs");
        nodes[1].span = "L3-L3".to_string();
        nodes[2].span = "L3-L3".to_string();
        let docs = marked(&nodes);
        assert!(docs["src/a.rs#code_name_words_test"].inline_test);
        assert!(!docs["src/a.rs#tests"].inline_test);
    }

    #[test]
    fn mark_inline_tests_keeps_a_code_fn_that_shares_an_id_with_a_test_fn() {
        let mut nodes = sample_nodes("src/a.rs");
        nodes.push(mod_node(
            "src/a.rs#code_name_words_test",
            "code_name_words_test",
            Kind::Function,
            "src/a.rs",
            "L8-L9",
        ));
        assert!(!marked(&nodes)["src/a.rs#code_name_words_test"].inline_test);
    }

    #[test]
    fn mark_inline_tests_marks_a_python_test_class_and_its_methods() {
        let nodes = vec![
            mod_node("a.py#code", "code", Kind::Function, "a.py", "L1-L2"),
            mod_node("a.py#TestA", "TestA", Kind::Class, "a.py", "L4-L9"),
            mod_node("a.py#TestA.check", "check", Kind::Method, "a.py", "L5-L6"),
            mod_node("a.py#Latest", "Latest", Kind::Class, "a.py", "L11-L12"),
        ];
        let docs = marked(&nodes);
        assert!(!docs["a.py#code"].inline_test);
        assert!(!docs["a.py#TestA"].inline_test);
        assert!(docs["a.py#TestA.check"].inline_test);
        assert!(!docs["a.py#Latest"].inline_test);
    }

    #[test]
    fn mark_inline_tests_marks_a_python_test_fn_only_in_py() {
        let nodes = vec![
            mod_node(
                "tests/a.py#test_x",
                "test_x",
                Kind::Function,
                "tests/test_a.py",
                "L1-L2",
            ),
            mod_node("a.py#run", "run", Kind::Function, "a.py", "L4-L5"),
            mod_node("a.ts#test_x", "test_x", Kind::Function, "a.ts", "L1-L2"),
        ];
        let docs = marked(&nodes);
        assert!(docs["tests/a.py#test_x"].inline_test);
        assert!(!docs["a.py#run"].inline_test);
        assert!(!docs["a.ts#test_x"].inline_test);
    }

    #[test]
    fn mark_inline_tests_keeps_a_test_named_fn_in_a_code_module() {
        let nodes = vec![mod_node(
            "db.py#test_connection",
            "test_connection",
            Kind::Function,
            "pkg/db.py",
            "L1-L2",
        )];
        assert!(!marked(&nodes)["db.py#test_connection"].inline_test);
    }

    #[test]
    fn mark_inline_tests_keeps_a_python_code_fn_that_shares_an_id() {
        let nodes = vec![
            mod_node("a.py#TestA", "TestA", Kind::Class, "a.py", "L1-L4"),
            mod_node("a.py#helper", "helper", Kind::Method, "a.py", "L2-L3"),
            mod_node("a.py#helper", "helper", Kind::Function, "a.py", "L6-L7"),
        ];
        assert!(!marked(&nodes)["a.py#helper"].inline_test);
    }

    #[test]
    fn exact_name_query_keeps_a_python_test_rank() {
        let nodes = vec![mod_node(
            "a.py#test_x",
            "test_parse_name",
            Kind::Function,
            "tests/test_a.py",
            "L1-L2",
        )];
        let docs = marked(&nodes);
        let d = &docs["a.py#test_x"];
        let other = vec!["stop".to_string()];
        assert_eq!(inline_test_factor(false, d, &other), TEST_RANK_PENALTY);
        let exact = vec!["parse".to_string(), "name".to_string()];
        assert_eq!(inline_test_factor(false, d, &exact), 1.0);
    }

    #[test]
    fn inline_test_with_more_name_terms_ranks_below_a_code_symbol() {
        let corpus = Corpus {
            idf: [("code".to_string(), 1.0), ("query".to_string(), 1.0)]
                .into_iter()
                .collect(),
            dflt_idf: 1.0,
            avg_body_len: 0.0,
        };
        let terms = vec!["code".to_string(), "query".to_string(), "stop".to_string()];
        let nodes = sample_nodes("src/a.rs");
        let mut docs = marked(&nodes);
        docs.get_mut("src/a.rs#code").unwrap().name = bag(&["query".to_string()]);
        docs.get_mut("src/a.rs#code_name_words_test").unwrap().name =
            bag(&["code".to_string(), "query".to_string()]);
        let total = |id: &str, path: &str| node_total(&terms, &docs[id], &corpus, false, &[], path);
        let code = total("src/a.rs#code", "src/a.rs");
        let test = total("src/a.rs#code_name_words_test", "src/a.rs");
        assert!(code > test, "{code} {test}");
    }

    #[test]
    fn query_that_names_tests_keeps_an_inline_test_rank() {
        let terms = vec!["stop".to_string()];
        let nodes = sample_nodes("src/a.rs");
        let docs = marked(&nodes);
        let d = &docs["src/a.rs#code_name_words_test"];
        assert_eq!(inline_test_factor(true, d, &terms), 1.0);
        assert_eq!(inline_test_factor(false, d, &terms), TEST_RANK_PENALTY);
    }

    #[test]
    fn exact_name_query_keeps_an_inline_test_rank() {
        let terms = vec!["code".to_string(), "words".to_string()];
        let nodes = sample_nodes("src/a.rs");
        let docs = marked(&nodes);
        let d = &docs["src/a.rs#code_name_words_test"];
        assert_eq!(inline_test_factor(false, d, &terms), 1.0);
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

    /// The idf is `Math.log(1 + n / (1 + df))`.
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

    /// Inputs in `[1, sqrt(2))` take the `k == 0` branches. The
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
        // Pins the `idf * ((tf * (k1 + 1)) / (tf + norm))`
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
            test_factor(false, &[], "src/tests/app.rs"),
            super::super::TEST_RANK_PENALTY
        );
        assert_eq!(test_factor(true, &[], "src/tests/app.rs"), 1.0);
        assert_eq!(test_factor(false, &[], "src/app.rs"), 1.0);
    }

    #[test]
    fn test_factor_penalizes_a_script_path_unless_the_query_wants_or_names_it() {
        use super::super::split_words as w;
        let p = super::super::TEST_RANK_PENALTY;
        assert_eq!(test_factor(false, &[], "scripts/oracle/Run.kt"), p);
        assert_eq!(
            test_factor(false, &w("all scripts"), "scripts/oracle/Run.kt"),
            1.0
        );
        assert_eq!(test_factor(false, &[], "crates/x/src/scripts.rs"), 1.0);
        let q = w("where does the label gate check plan labels");
        assert_eq!(test_factor(false, &q, "scripts/label-gate.sh"), 1.0);
        assert_eq!(test_factor(false, &q, "scripts/label-lint.sh"), p);
    }

    #[test]
    fn script_path_matches_whole_segments() {
        use super::super::is_script_path;
        assert!(is_script_path("scripts/a.sh"));
        assert!(is_script_path("tools/scripts/a.sh"));
        assert!(!is_script_path("myscripts/a.sh"));
        assert!(!is_script_path("tools/a.sh"));
    }

    #[test]
    fn query_names_file_needs_every_long_stem_word() {
        use super::super::{query_names_file as names, split_words as w};
        assert!(names(&w("the label gate"), "scripts/label-gate.sh"));
        assert!(names(&w("LABEL Gate"), "scripts/label_gate.sh"));
        assert!(!names(&w("the label"), "scripts/label-gate.sh"));
        assert!(!names(&w("labels gate"), "scripts/label-gate.sh"));
        // Words under 3 letters are ignored; a stem of only those is never named.
        assert!(names(&w("the run"), "scripts/ab-run.sh"));
        assert!(!names(&w("ab cd"), "scripts/ab-cd.sh"));
        assert!(!names(&w("sh"), "scripts/.sh"));
        // Digits do not count as letters.
        assert!(names(&w("run2"), "scripts/run2-1.sh"));
        assert!(!names(&w("run"), "scripts/run2-1.sh"));
        assert!(!names(&w("123"), "scripts/123.sh"));
        // The dot splits the stem; the folder never counts.
        assert!(names(&w("build all"), "scripts/build.all.sh"));
        assert!(!names(&w("scripts"), "scripts/label-gate.sh"));
        // A joined name is one word; a dotfile has no stem.
        assert!(!names(&w("label gate"), "scripts/LabelGate.sh"));
        assert!(!names(&w("bashrc"), "scripts/.bashrc"));
    }

    #[test]
    fn wants_tests_matches_a_whole_word() {
        assert!(super::super::wants_tests("run the tests"));
        assert!(!super::super::wants_tests("run the greatest"));
    }

    #[test]
    fn how_does_x_work_drops_the_framing_verb_only_in_that_shape() {
        assert_eq!(query_terms("how does the build work"), ["build"]);
        assert_eq!(query_terms("how do hooks work"), ["hooks"]);
        assert_eq!(query_terms("git work tree"), ["git", "work", "tree"]);
    }

    #[test]
    fn has_term_folds_one_trailing_s() {
        let b: Bag = vec![("helper".to_string(), 1)];
        assert!(has_term(&b, "helper"));
        assert!(has_term(&b, "helpers"));
        assert!(!has_term(&b, "help"));
    }

    /// A term that ends in `s` also finds itself plus `s`, so `pass`
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
